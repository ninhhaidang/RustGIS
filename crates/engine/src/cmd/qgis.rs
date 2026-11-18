//! QGIS projects and layer styles: `project.importQgis` opens a `.qgs` (or `.qgz`) as a new map,
//! with its layers, their order, visibility and styles, its coordinate system and its view;
//! `style.importQgis` gives a layer a `.qml` style. Both say what they couldn't carry over
//! (`rustgis_map::qgis::Report`).

use rustgis_geom::{Coord, Extent};
use rustgis_map::qgis::{self, NoteKind, Report, Source};
use serde_json::{Value, json};

use super::{need_layer, ps};
use crate::{CmdResult, CommandSpec as C, Session};

pub fn commands() -> Vec<C> {
    vec![
        C::new("project.importQgis", "Import QGIS Project", "Project › Import", import_project).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Reserved).with(crate::policy::Effect::Files)).params(
            r#"{"path"?: "*.qgs|*.qgz", "text"?: "a .qgs file's text", "data"?: "a .qgs or .qgz file, base64", "dir"?: "the folder its relative paths start from (default: the file's)"} → the layers added and notes on what wasn't carried over"#,
        ),
        C::new("style.importQgis", "Import QGIS Style", "Style › Styles", import_style).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files)).when(need_layer).params(
            r#"{"layer"?, "path"?: "*.qml", "text"?: "a .qml file's text", "data"?: "a .qml file, base64"} → notes on what wasn't carried over"#,
        ),
    ]
}

/// A QGIS file's text: a `.qgs` or `.qml` as it is, a `.qgz` (a zip archive) its `.qgs`.
fn qgis_text(p: &Value) -> Result<String, String> {
    let bytes = match (ps(p, "data"), ps(p, "text"), ps(p, "path")) {
        (Some(d), ..) => {
            // Base64 is 4 characters for 3 bytes: a payload past the file limit isn't decoded.
            if d.len() as u64 > MAX_FILE / 3 * 4 + 4 {
                return Err("data is too large for a QGIS file".into());
            }
            use base64::Engine;
            base64::engine::general_purpose::STANDARD.decode(d.trim()).map_err(|_| "data must be base64".to_string())?
        }
        (None, Some(t), _) if t.len() > MAX_XML => return Err("text is too large for a QGIS project or style".into()),
        (None, Some(t), _) => return Ok(t.to_string()),
        (None, None, Some(path)) => read_file(path)?,
        _ => return Err("path, text or data is required".into()),
    };
    if bytes.starts_with(b"PK") {
        let entries = rustgis_data::zip::read_matching_within(&bytes, |n| n.to_ascii_lowercase().ends_with(".qgs"), MAX_XML)?;
        let (_, qgs) = entries.into_iter().next().ok_or("the .qgz holds no .qgs project")?;
        return String::from_utf8(qgs).map_err(|_| "the .qgz's project isn't text".to_string());
    }
    if bytes.len() > MAX_XML {
        return Err("too large for a QGIS project or style".into());
    }
    String::from_utf8(bytes).map_err(|_| "not a QGIS file (it isn't text)".to_string())
}

/// The most bytes of a QGIS file read (a `.qgz` can carry data files beside its project).
const MAX_FILE: u64 = 256 << 20;
/// The most bytes of a project's or style's XML parsed: its tree is several times larger than
/// the text, and real ones are a few megabytes.
const MAX_XML: usize = 64 << 20;

/// Reads at most [`MAX_FILE`] bytes of `path` (natively: the web app passes `text` or `data`).
fn read_file(path: &str) -> Result<Vec<u8>, String> {
    crate::project::read_file(std::path::Path::new(path), MAX_FILE)
}

/// Imports a QGIS project as a new map. On the thread that runs it, as opening a project is: it
/// adds layers to the session one after another, each through the command that adds its kind
/// (a tile archive through that command's add itself, `tiles::add_with_metadata_wait`, with no
/// wait for its metadata, as Add Data adds one), and no part of it runs on its own yet. Its work is bounded by the project: at most
/// [`qgis::MAX_LAYERS`] layers, each read once by the command that adds it (a file's layers, and
/// a virtual layer on one, each read their file), archives added without waiting for their
/// metadata, annotation pictures within [`MAX_PICTURE_BYTES`]. A local archive's file is looked
/// for on this thread (`is_file`), which a slow network share can hold up. Running the whole
/// import off the window, with progress and Cancel, is for the import as a whole.
fn import_project(s: &mut Session, p: &Value) -> CmdResult {
    let text = qgis_text(p)?;
    // The folder relative paths start from: `dir` (a blank one is none, so they don't start
    // from the working directory), else the file's own (`.` for a file named without one).
    let dir = ps(p, "dir").filter(|d| !d.trim().is_empty()).map(str::to_string).or_else(|| {
        ps(p, "path").and_then(|path| std::path::Path::new(path).parent()).map(|d| match d.to_string_lossy() {
            d if d.is_empty() => ".".to_string(),
            d => d.into_owned(),
        })
    });
    let mut report = Report::default();
    let project = qgis::read_project(&text, dir.as_deref().unwrap_or_default(), &mut report)?;
    // A map of its own, named as the project.
    let name = if project.title.is_empty() { "QGIS project" } else { project.title.as_str() };
    s.run_part("map.new", &json!({"name": name}))?;
    // QGIS draws a project on its canvas alone: no basemap, no graticule.
    let m = s.map_mut();
    m.basemap = rustgis_map::Basemap::Blank;
    m.graticule = false;
    let degrees = project.crs.as_deref().and_then(Geographic::of);
    if let Some(crs) = &project.crs {
        let code = match crs.as_str() {
            "EPSG:3857" | "EPSG:900913" => "mercator",
            // QGIS draws a project in degrees as plate carrée: longitude and latitude as x and y.
            _ if degrees.is_some() => "equirectangular",
            other => other,
        };
        match s.run_part("map.projection", &json!({"type": code})) {
            Err(e) => report.approximated("", format!("the project's coordinate system {crs} ({e}), drawn in Web Mercator")),
            Ok(_) if degrees.is_some_and(|g| !g.wgs84 || !g.greenwich) => {
                let meridian = if degrees.is_some_and(|g| !g.greenwich) { ", its longitudes taken as from Greenwich" } else { "" };
                report.approximated("", format!("the project's coordinate system {crs}, drawn in Equirectangular on WGS 84{meridian}"));
            }
            Ok(_) => {}
        }
    }
    // Bottom first, so each is added on top of the ones below it; the main annotation layer
    // over them all, as QGIS draws it.
    let mut added = Vec::new();
    // The project layer each of `added` came from (none for the main annotations).
    let mut added_from: Vec<Option<usize>> = Vec::new();
    let crs = project.crs.as_deref();
    let mut pictures = Pictures::default();
    // Saved connections made for the project's database layers.
    let mut connections = 0usize;
    // The coordinates the map draws in, where QGIS fits a picture whose aspect is locked (on its
    // canvas): the map's projection as set above (Web Mercator where none was taken).
    let canvas_def = match &s.map().projection {
        rustgis_map::Projection::Custom(code) => rustgis_map::projection::lookup(code).ok().map(|(def, _)| def),
        _ => Some("EPSG:3857".to_string()),
    };
    let canvas = canvas_def.and_then(|d| rustgis_data::raster::crs::Crs::parse(&d).and_then(|c| c.transformer()).ok());
    // Each layer added, bottom first: whether it is drawn, and, for an annotation layer, its
    // texts over all its other graphics and whether it has any such.
    let mut drawn: Vec<(String, bool, Option<Drawn>)> = Vec::new();
    // What each of the project's layers came in as, by its index in `project.layers`.
    let mut came_in: Vec<CameIn> = Vec::new();
    let tree_layers = project.layers.iter().enumerate().rev().map(|(i, l)| (Some(i), l));
    for (index, layer) in tree_layers.chain(project.main_annotations.iter().map(|l| (None, l))) {
        let before: std::collections::HashSet<u32> = s.map().layers.iter().map(|l| l.id).collect();
        let tiles = s.map().tiles.len();
        let result = if layer.kind == "annotation" {
            add_annotations(s, layer, crs, canvas.as_ref(), dir.is_some(), &mut pictures, &mut report)
                .map(|(name, d)| Some((name, Some(d))))
        } else {
            add_layer(s, layer, dir.is_some(), &mut connections, &mut report).map(|name| name.map(|name| (name, None)))
        };
        match result {
            Ok(Some((name, d))) => {
                drawn.push((layer.name.clone(), layer.visible, d));
                added.push(name);
                added_from.push(index);
                if let Some(index) = index {
                    let ids = s.map().layers.iter().map(|l| l.id).filter(|id| !before.contains(id)).collect();
                    // A web tile layer on the same tiles as one added before takes its place (the
                    // map keeps one tile layer of the same tiles): that one isn't on the map, nor
                    // among those added, said under its name.
                    let key = tile_key(&layer.source);
                    if let Some(k) = &key {
                        for c in came_in.iter_mut().filter(|c| c.key.as_ref() == Some(k)) {
                            c.tile = false;
                            c.key = None;
                            let Some(gone) = project.layers.get(c.index) else { continue };
                            // By the project layer it came from: names can repeat.
                            if let Some(at) = added_from.iter().position(|i| *i == Some(c.index)) {
                                added.remove(at);
                                added_from.remove(at);
                            }
                            report.skipped(
                                &gone.name,
                                format!("the layer: the same tiles as {}, which takes its place (RustGIS keeps one tile layer of the same tiles)", layer.name),
                            );
                        }
                    }
                    let tile = key.is_some() || s.map().tiles.len() > tiles;
                    came_in.push(CameIn { index, ids, tile, key });
                }
            }
            Ok(None) => {}
            Err(e) => report.skipped(&layer.name, format!("the layer: {e}")),
        }
    }
    note_texts_under_layers_above(&drawn, &mut report);
    add_groups(s, &project, &came_in, &mut report);
    add_relations(s, &project, &came_in, &mut report);
    go_to_view(s, &project, degrees);
    add_bookmarks(s, &project, &mut report);
    let notes: Vec<Value> = report.notes.iter().map(note_json).collect();
    if !report.notes.is_empty() {
        let left = report.notes.iter().filter(|n| n.kind == NoteKind::Skipped).count();
        s.notify(
            format!(
                "Imported {} of {} QGIS layers; {} things drawn differently, {left} left out (see Messages)",
                added.len(),
                project.layers.len() + usize::from(project.main_annotations.is_some()),
                report.notes.len() - left
            ),
            false,
        );
    }
    notify_report(s, &report);
    Ok(json!({"map": name, "layers": added, "notes": notes}))
}

/// What one of a project's layers came in as.
struct CameIn {
    /// Its index in [`qgis::Project::layers`].
    index: usize,
    /// The map layers added for it (a file of several layers adds them all).
    ids: Vec<u32>,
    /// A web tile layer was added for it.
    tile: bool,
    /// The tiles it was added as (see [`tile_key`]), while its tile layer is on the map.
    key: Option<(String, &'static str)>,
}

/// The tiles a web tile layer is added as: its address and kind, as the map tells tile layers of
/// the same tiles apart. (`map.addTiles` replaces an overlay of the same address and kind with the
/// same look or name, `tiles.rs`'s `add_with_metadata_wait`; imported ones are overlays with no
/// look of their own, so two on the same tiles always merge. A change there changes this.)
fn tile_key(source: &Source) -> Option<(String, &'static str)> {
    match source {
        // Trimmed, as the map keeps the address.
        Source::Xyz { url, vector, .. } => Some((url.trim().to_string(), if *vector { "vector" } else { "raster" })),
        Source::Archive { path, kind, .. } => Some((path.clone(), kind)),
        _ => None,
    }
}

/// Makes the project's layer tree groups as layer groups, nested as in QGIS, and puts each layer
/// added in its own (`came_in`), its box checked as its own is in QGIS (the groups' apply on
/// top). A group is made even when none of its layers came in, as QGIS keeps empty groups. A
/// web tile layer can't be in a group: one that was in a group is noted, added outside it (shown
/// as the groups holding it showed it).
fn add_groups(s: &mut Session, project: &qgis::Project, came_in: &[CameIn], report: &mut Report) {
    if project.groups.is_empty() {
        return;
    }
    let m = s.map_mut();
    // Every group made first, empty (parents before the groups in them, so each finds its
    // parent), then given its layers and gathered once: the layers came in in the tree's order,
    // so gathering moves none of them. Giving a group its layers as it is made would gather them
    // before its subgroups exist, moving a layer under a subgroup above that subgroup's layers.
    let mut made: Vec<u32> = Vec::with_capacity(project.groups.len());
    for g in &project.groups {
        let mut group = rustgis_map::LayerGroup::new(g.name.as_str());
        group.visible = g.checked;
        group.expanded = g.expanded;
        group.transparency = g.transparency.unwrap_or(0.0).clamp(0.0, 100.0);
        group.min_scale = g.min_scale;
        group.max_scale = g.max_scale;
        group.parent = g.parent.and_then(|p| made.get(p).copied());
        made.push(m.add_group(group));
    }
    for c in came_in {
        let Some(id) = project.layers.get(c.index).and_then(|l| l.group).and_then(|g| made.get(g).copied()) else { continue };
        if let Some(group) = m.groups.iter_mut().find(|g| g.id == id) {
            group.children.extend(c.ids.iter().copied());
        }
    }
    let mut outside = Vec::new();
    for c in came_in {
        let Some(layer) = project.layers.get(c.index) else { continue };
        let Some(group) = layer.group.and_then(|g| project.groups.get(g)) else { continue };
        if c.tile && c.ids.is_empty() {
            // The opacity of a group drawn as one holding it, which the tile goes without.
            let mut chain = layer.group;
            let mut faded = false;
            for _ in 0..project.groups.len() {
                let Some(g) = chain.and_then(|g| project.groups.get(g)) else { break };
                faded |= g.transparency.is_some();
                chain = g.parent;
            }
            outside.push((layer.name.as_str(), group.name.as_str(), faded));
        }
        for id in &c.ids {
            if let Some(l) = m.layer_mut(*id) {
                l.visible = layer.checked;
            }
        }
    }
    // Parents come before the groups in them, so no link should be dropped; said if one is.
    let dropped = m.tidy_groups();
    if dropped > 0 {
        report.approximated("", format!("groups whose place in another couldn't be kept ({dropped}), put at the top level"));
    }
    for (name, group, faded) in outside {
        let opacity = if faded { " and the opacity of a group drawn as one holding it" } else { "" };
        report.approximated(name, format!("its place in the group {group}{opacity}: a web tile layer, added outside the group"));
    }
}

/// What an annotation layer draws, as placed (bottom first): the texts over all its other
/// graphics, and whether it has any other graphics.
struct Drawn {
    texts_on_top: usize,
    others: bool,
}

/// Notes the texts of each annotation layer (`drawn`: the layers added, bottom first, each with
/// whether it is visible) that a visible layer above draws over in QGIS: RustGIS draws
/// graphics layers' texts after all layers, so over it. Those already under a shape or picture
/// of their own layer are noted with it, not again.
fn note_texts_under_layers_above(drawn: &[(String, bool, Option<Drawn>)], report: &mut Report) {
    let mut above = false;
    for (name, visible, d) in drawn.iter().rev() {
        // The layer noted hidden or not (shown later, it draws so); only one drawn covers it.
        if let Some(d) = d
            && above
            && d.texts_on_top > 0
        {
            let what = if d.texts_on_top == 1 {
                "an annotation text a layer above draws over in QGIS is drawn over that layer".to_string()
            } else {
                format!("{} annotation texts layers above draw over in QGIS are drawn over those layers", d.texts_on_top)
            };
            report.approximated(name, format!("{what}: RustGIS draws graphics layers' texts after all layers"));
        }
        above |= *visible && d.as_ref().is_none_or(|d| d.others);
    }
}

/// Adds one QGIS layer to the active map with its style: `Ok(Some(name))` when added, `Ok(None)`
/// when its source is one RustGIS doesn't read (noted). Without the project's folder (`has_dir`:
/// a project given as text or data with no `dir`), a relative path has nothing to start from and
/// is noted rather than looked for in the working directory.
fn add_layer(
    s: &mut Session,
    layer: &qgis::ProjectLayer,
    has_dir: bool,
    connections: &mut usize,
    report: &mut Report,
) -> Result<Option<String>, String> {
    let name = layer.name.as_str();
    if let Source::File { path, .. } | Source::Csv { path, .. } | Source::Archive { path, .. } = &layer.source {
        if !has_dir && qgis::is_relative(path) {
            report.skipped(name, format!("the file {path}, relative to the project's folder, which the import wasn't given (pass dir)"));
            return Ok(None);
        }
        // `C:roads.gpkg`: relative to the working directory on drive C, which no folder fixes.
        if qgis::is_drive_relative(path) {
            report.skipped(name, format!("the file {path}, relative to a drive's working directory (give a full path in QGIS)"));
            return Ok(None);
        }
    }
    match &layer.source {
        Source::File { path, layer: table, ignored, .. } => {
            if !ignored.is_empty() {
                // `layerid=2`: RustGIS opens a layer by name, so without one every layer of a file
                // that has several comes in; with one, the options are only not read.
                let why = if table.is_some() { "not read" } else { "not read: a file with several layers adds them all" };
                report.approximated(name, format!("its source options {} ({why})", ignored.join(", ")));
            }
            let mut q = json!({"path": path, "name": name});
            if let Some(t) = table {
                q["layer"] = json!(t);
            }
            s.run_part("layer.addData", &q)?;
        }
        Source::Csv { path, x, y, wkt, crs } => {
            let mut q = json!({"path": path, "name": name});
            for (k, v) in [("x", x), ("y", y), ("wkt", wkt), ("crs", crs)] {
                if let Some(v) = v {
                    q[k] = json!(v);
                }
            }
            s.run_part("layer.addData", &q)?;
        }
        Source::Xyz { url, zmin, zmax, vector, style } => {
            // An overlay, as a WMS layer is, so web layers keep the tree's order between them.
            let mut q = json!({"url": url, "name": name, "kind": if *vector { "vector" } else { "raster" }, "overlay": true});
            if let Some(z) = zmin {
                q["minZoom"] = json!(z);
            }
            if let Some(z) = zmax {
                q["maxZoom"] = json!(z);
            }
            let before = s.active_layer;
            let added = s.run_part("map.addTiles", &q)?;
            web_settings(s, &added, before, layer, report);
            if *vector {
                note_vector_tile_style(name, style.is_some(), report);
            }
            return Ok(Some(name.to_string()));
        }
        Source::Archive { path, kind, vector, zoom: (zmin, zmax) } => {
            // RustGIS opens an MBTiles file on this computer, and an archive on a network share
            // by no address.
            // (A share's path, `\\server\share\…` or `//server/share/…`, is refused before the
            // file is looked for: looking reaches the server.)
            let share = path.starts_with("file://") || path.starts_with("\\\\") || path.starts_with("//");
            if share || (*kind == "mbtiles" && path.contains("://")) {
                let at = if share { "on a network share" } else { "at a web address" };
                report.skipped(
                    name,
                    format!(
                        "the tile archive {at}: RustGIS reads MBTiles files on this computer, and PMTiles archives there or on the web"
                    ),
                );
                return Ok(None);
            }
            // A file that isn't there is noted, not added to fail as it draws; the browser reads
            // no file by its path.
            #[cfg(not(target_arch = "wasm32"))]
            if !path.contains("://") && !std::path::Path::new(path).is_file() {
                report.skipped(name, format!("the tile archive {path}: no file there"));
                return Ok(None);
            }
            #[cfg(target_arch = "wasm32")]
            if !path.contains("://") {
                report
                    .skipped(name, format!("the tile archive {path}, a file the browser doesn't read by its path (add it with Add Data)"));
                return Ok(None);
            }
            let before = s.active_layer;
            // Without waiting for its metadata (it lands as the map draws), as Add Data adds a
            // local archive: an import of many archives doesn't hold the window a second for each.
            // (The import runs only in the user's own dispatch, which may read files and the web.)
            let q = json!({"url": path, "name": name, "kind": kind, "overlay": true});
            // The zoom levels QGIS limits it to: an archive shows its own levels in RustGIS.
            if zmin.is_some() || zmax.is_some() {
                let level = |z: &Option<u8>| z.map_or("…".to_string(), |z| z.to_string());
                report.approximated(
                    name,
                    format!("its zoom levels {}–{}: RustGIS shows a tile archive at the levels it has", level(zmin), level(zmax)),
                );
            }
            // (As run_part would, a refused add leaves the map's tile layers as they were: it may
            // drop the same archive's layer before it finds the map full.)
            let tiles = s.map().tiles.clone();
            let added = match super::tiles::add_with_metadata_wait(s, &q, web_time::Duration::ZERO) {
                Ok(added) => added,
                Err(e) => {
                    s.map_mut().tiles = tiles;
                    return Err(e);
                }
            };
            web_settings(s, &added, before, layer, report);
            if *vector {
                note_vector_tile_style(name, false, report);
            }
            return Ok(Some(name.to_string()));
        }
        Source::Memory { geometry, crs, fields, as_text: (as_text, texts), cut, unnamed, taken, zm, curved, unknown_geometry } => {
            // QGIS keeps none of a memory layer's features: it opens empty, with its fields.
            let mut ds = rustgis_data::Dataset::new(name, *geometry, fields.clone());
            if let Some(c) = layer.crs.as_deref().or(crs.as_deref()) {
                match rustgis_data::raster::crs::Crs::parse(c) {
                    Ok(c) => ds.crs = Some(c),
                    Err(e) => report.skipped(name, format!("its coordinate system {c}, which RustGIS can't read ({e})")),
                }
            }
            super::layer::add_dataset(s, ds);
            if *texts > 0 {
                let more = if *texts > as_text.len() { ", …" } else { "" };
                report.approximated(name, format!("its fields of kinds RustGIS hasn't, as text ({texts}: {}{more})", as_text.join(", ")));
            }
            if let Some(g) = unknown_geometry {
                report.skipped(name, format!("its geometry ({g}), of a kind RustGIS hasn't: a table without geometry"));
            }
            if *unnamed > 0 {
                report.skipped(name, format!("its fields with no name ({unnamed})"));
            }
            if *taken > 0 {
                report.skipped(name, format!("its fields of a name an earlier field has ({taken})"));
            }
            if let Some(kind) = curved {
                report.approximated(name, format!("its geometry kind ({kind}): curves read as lines and polygons of their points"));
            }
            if *zm {
                report.approximated(name, "its geometry's Z and M values: RustGIS keeps x and y");
            }
            if *cut > 0 {
                report.skipped(name, format!("its fields past the first {} ({cut})", qgis::MAX_MEMORY_FIELDS));
            }
        }
        Source::Virtual { query, from, .. } => {
            let shown: String = query.chars().take(200).collect();
            let more = if query.chars().count() > 200 { "…" } else { "" };
            let why = match from {
                Some(qgis::VirtualFrom::Missing(why)) => why.clone(),
                Some(_) => {
                    "on a layer RustGIS doesn't read as a file's layer (a CSV, memory, database or web layer, or another virtual layer)"
                        .into()
                }
                None => "RustGIS reads one that selects every column of one layer, with or without a WHERE clause, as that layer".into(),
            };
            report.skipped(name, format!("the virtual layer `{shown}{more}`: {why}"));
            return Ok(None);
        }
        Source::Wms { url, layer: wms, .. } => {
            let before = s.active_layer;
            let added = s.run_part("service.add", &json!({"url": url, "type": "wms", "layer": wms, "name": name}))?;
            web_settings(s, &added, before, layer, report);
            return Ok(Some(name.to_string()));
        }
        Source::Wfs { url, layer: typename } => {
            report.skipped(name, format!("the WFS layer {typename} at {url} (add it with Home › Add › Web Data)"));
            return Ok(None);
        }
        Source::Postgres { connection, table, dropped } => {
            database_note(s, name, "PostGIS", table, connection, dropped, connections, report);
            return Ok(None);
        }
        Source::SqlServer { connection, table, dropped } => {
            database_note(s, name, "SQL Server", table, connection, dropped, connections, report);
            return Ok(None);
        }
        Source::Other { provider } => {
            // A layer of no provider (an annotation layer, a group layer) is told by its kind.
            let what = if provider.is_empty() { format!("{} layers", layer.kind) } else { format!("{provider} sources") };
            report.skipped(name, format!("the layer: RustGIS doesn't read QGIS's {what}"));
            return Ok(None);
        }
    }
    let id = s.active_layer.ok_or("the layer wasn't added")?;
    let Some(l) = s.map_mut().layer_mut(id) else { return Err("the layer wasn't added".into()) };
    let added_name = l.name.clone();
    l.visible = layer.visible;
    if l.raster.is_some() {
        // A raster takes its opacity, blending, scale range and colouring (when its bands are
        // there).
        if let Some(why) = l.raster.as_ref().and_then(|r| layer.style.raster_misfit(r)) {
            report.skipped(name, why);
        }
        layer.style.apply(l);
        return Ok(Some(added_name));
    }
    layer.style.apply(l);
    // What reads only against the layer's fields: its definition query, and what the style names.
    if let Some(q) = &layer.definition_query {
        match parse_imported(q, &l.dataset) {
            Ok(_) => l.definition_query = Some(q.clone()),
            Err(e) => report.skipped(name, format!("its filter `{q}` ({e})")),
        }
    }
    check_fields(l, name, &layer.style.rules, layer.style.categories_by.as_ref(), &layer.style.bool_classes, report);
    if layer.style.diagram.is_some() {
        layer.style.check_diagram_fields(l, name, report);
    }
    bind_time(l, name, layer.style.time.as_deref(), report);
    Ok(Some(added_name))
}

/// Notes that a vector tile layer's own style (QGIS's, made in QGIS or from a MapLibre style,
/// `styled`) isn't read: RustGIS draws its tiles in its own colours. (The style's address isn't
/// shown: it may carry a key.)
fn note_vector_tile_style(name: &str, styled: bool, report: &mut Report) {
    let from = if styled { ", and the MapLibre style QGIS made it from" } else { "" };
    report.approximated(name, format!("its vector tile style{from}: drawn in RustGIS's colours, one for each of its tile layers"));
}

/// The most saved connections one import makes.
const MAX_IMPORTED_CONNECTIONS: usize = 16;

/// Notes a database table that isn't added (`what`: PostGIS, SQL Server): on the desktop, with
/// the saved connection to its database to log in to, made from the project's connection text
/// (its password left out, which the project text never keeps here) when no connection to it is
/// there yet. No connection is made to the database itself.
#[expect(clippy::too_many_arguments, reason = "the parts of one note, each from the layer's source")]
fn database_note(
    s: &mut Session,
    name: &str,
    what: &str,
    table: &str,
    connection: &str,
    dropped: &[String],
    made: &mut usize,
    report: &mut Report,
) {
    let saved = saved_database_connection(s, what, connection, made);
    // Why none could be saved, when making one was tried and refused.
    let (saved, refused) = match saved {
        Ok(c) => (Some(c), String::new()),
        Err(Some(e)) => (None, format!(" (no connection could be saved: {e})")),
        Err(None) => (None, String::new()),
    };
    // Where a login would go, so a project can't hide it behind a connection's name.
    // (A saved connection's text has no quoted values: the first `host=` setting is its host.)
    let host = connection.split(' ').find_map(|p| p.strip_prefix("host=")).unwrap_or_default();
    // How it is reached, when the project sets it (`sslmode=disable`): the login goes that way.
    let tls = connection.split(' ').find_map(|p| p.strip_prefix("sslmode=")).map(|m| format!(", sslmode {m}")).unwrap_or_default();
    // Its settings the saved connection, or the connection text, leaves out, by name
    // (`sslrootcert`, `authcfg`, …).
    let left = |what: &str| {
        if dropped.is_empty() {
            String::new()
        } else {
            format!("; {what} leaves out these settings of the project's: {}", dropped.join(", "))
        }
    };
    let text = match saved {
        Some(c) => format!(
            "the {what} table {table}: log in to the saved connection '{c}' (to {host}{tls}; Data Sources › Connections), then add the table from it{}",
            left("the saved connection")
        ),
        None => {
            format!("the {what} table {table} ({connection}): connect to the database to add it{refused}{}", left("the connection text"))
        }
    };
    report.skipped(name, text);
}

/// The name of the saved connection to the database `connection` names (QGIS's `key=value`
/// text): one already saved, else one made now (at most [`MAX_IMPORTED_CONNECTIONS`] an import).
/// `Err(None)` in the browser (which reads no databases), for a connection RustGIS can't address
/// (a `service=` one, a value it would have to escape), or past the import's limit; `Err(Some)`
/// with why, when making one was refused.
fn saved_database_connection(s: &mut Session, what: &str, connection: &str, made: &mut usize) -> Result<String, Option<String>> {
    let room = *made < MAX_IMPORTED_CONNECTIONS;
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (s, what, connection, made, room);
        Err(None)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        // A value QGIS quoted (it holds a space or a quote) isn't put into an address.
        if connection.contains('\'') {
            return Err(None);
        }
        let kv: Vec<(&str, &str)> = connection.split(' ').filter_map(|p| p.split_once('=')).collect();
        let get = |k: &str| kv.iter().find(|(n, _)| *n == k).map(|(_, v)| *v).filter(|v| !v.is_empty());
        let host = get("host").ok_or(None)?;
        // A service's settings (pg_service.conf) aren't in the address: no connection is made.
        if get("service").is_some() {
            return Err(None);
        }
        // Values an address would have to escape (a backslash, a control character) aren't
        // guessed at.
        if kv.iter().any(|(_, v)| v.chars().any(|c| c.is_control())) {
            return Err(None);
        }
        let (kind, address) = if what == "PostGIS" {
            if kv.iter().any(|(_, v)| v.contains('\\')) {
                return Err(None);
            }
            let parts: Vec<String> =
                ["host", "port", "dbname", "user", "sslmode"].iter().filter_map(|k| get(k).map(|v| format!("{k}={v}"))).collect();
            ("postgres", format!("postgres:{}", parts.join(" ")))
        } else {
            // QGIS writes a SQL Server's host as host, host,port or host\instance.
            let (host, instance) = host.split_once('\\').map_or((host, None), |(h, i)| (h, Some(i)));
            let (host, port) = host.split_once(',').map_or((host, get("port")), |(h, p)| (h, Some(p)));
            if host.is_empty() {
                return Err(None);
            }
            let (user, db) = (get("user"), get("dbname").unwrap_or_default());
            // Parts that would need escaping in an address (a DOMAIN\user, say) aren't guessed at.
            let plain = |p: &str| !p.contains(['@', '/', '?', '#', ':', '%', '\\', '&', '=']);
            if ![user.unwrap_or_default(), host, port.unwrap_or_default(), db, instance.unwrap_or_default()].into_iter().all(plain) {
                return Err(None);
            }
            let user = user.map(|u| format!("{u}@")).unwrap_or_default();
            let port = port.map(|p| format!(":{p}")).unwrap_or_default();
            let instance = instance.map(|i| format!("?instance={i}")).unwrap_or_default();
            ("sqlserver", format!("mssql://{user}{host}{port}/{db}{instance}"))
        };
        if let Some(c) = s.connections.matching_database(kind, &address).first() {
            return Ok(c.name.clone());
        }
        if !room {
            return Err(Some(format!("an import makes at most {MAX_IMPORTED_CONNECTIONS}")));
        }
        let new = s.run_part("connection.new", &json!({"kind": kind, "address": address})).map_err(Some)?;
        *made += 1;
        new.get("name").and_then(Value::as_str).map(str::to_string).ok_or(None)
    }
}

/// Adds a QGIS annotation layer as a graphics layer: its annotations taken from its coordinate
/// system (else the project's, `project_crs`) to longitude/latitude, its pictures read from their
/// files (a relative one only with the project's folder, `has_dir`), its opacity, blending and
/// scale range from its style. What can't be placed or read is noted.
///
/// On the thread that runs the import, as the import's other layers are: like opening a project,
/// it adds layers to the session as it goes, and no part of it runs on its own yet. Its work is
/// bounded: at most [`MAX_PICTURE_BYTES`] of pictures an import (each file at most what
/// `graphics::picture` reads), each read once, and at most [`qgis::MAX_ANNOTATIONS`] items a layer
/// placed. Running the whole import off the window, with Cancel and progress, is for the import
/// as a whole, not this layer kind.
fn add_annotations(
    s: &mut Session,
    layer: &qgis::ProjectLayer,
    project_crs: Option<&str>,
    canvas: Option<&rustgis_data::raster::crs::Transformer>,
    has_dir: bool,
    pictures: &mut Pictures,
    report: &mut Report,
) -> Result<(String, Drawn), String> {
    let name = layer.name.as_str();
    let crs = layer.crs.as_deref().or(project_crs);
    let transformer = match crs.map(|c| rustgis_data::raster::crs::Crs::parse(c).and_then(|c| c.transformer())) {
        Some(Ok(t)) => Some(t),
        Some(Err(_)) if layer.annotations.is_empty() => None,
        Some(Err(e)) => {
            let what = if layer.annotations.len() == 1 {
                "its annotation".to_string()
            } else {
                format!("its {} annotations", layer.annotations.len())
            };
            report.skipped(name, format!("{what}, in a coordinate system RustGIS can't read ({e})"));
            None
        }
        None if layer.annotations.is_empty() => None,
        None => {
            let what = if layer.annotations.len() == 1 {
                "its annotation".to_string()
            } else {
                format!("its {} annotations", layer.annotations.len())
            };
            report.skipped(name, format!("{what}, in a coordinate system neither the layer nor the project names"));
            None
        }
    };
    let added = s.run_part("graphics.newLayer", &json!({"name": name}))?;
    let id = added.get("layer").and_then(Value::as_u64).and_then(|id| u32::try_from(id).ok()).ok_or("the layer wasn't added")?;
    let (mut unplaced, mut unread, mut first_error, mut over) = (0usize, 0usize, None::<String>, 0usize);
    // Pictures whose locked aspect couldn't be kept on the map (its coordinates don't take the
    // box), fitted in the layer's coordinates instead.
    let mut fitted_in_layer = 0usize;
    let mut graphics = Vec::new();
    // A geometry taken to longitude/latitude; `None` where a point doesn't take.
    let place = |geometry: &rustgis_data::Geometry| {
        let t = transformer.as_ref()?;
        let failed = std::cell::Cell::new(false);
        let out = geometry.map_coords(&|c| {
            t.to_lonlat(c).filter(|c| c.x.is_finite() && c.y.is_finite() && c.y.abs() <= 90.0).unwrap_or_else(|| {
                failed.set(true);
                c
            })
        });
        (!failed.get()).then_some(out)
    };
    for a in layer.annotations.iter().filter(|_| transformer.is_some()) {
        let mut g = a.graphic.clone();
        // Placed first: a picture is read only for a graphic that is.
        let Some(placed) = place(&g.geometry) else {
            unplaced += 1;
            continue;
        };
        g.geometry = placed;
        if let Some(picture) = &a.picture {
            let left = MAX_PICTURE_BYTES.saturating_sub(pictures.bytes);
            let mut fresh = false;
            // Each file read once while the budget has room, and kept only when it fits, so what
            // is kept stays within the budget; every row keeps its own copy, counted against it.
            let read = match pictures.read.get(picture) {
                _ if pictures.over.contains(picture) => None,
                Some(Ok(b64)) if b64.len() > left => None,
                Some(read) => Some(read.clone()),
                None => {
                    // Its size first, from its text or its file's length (base64 is 4 bytes for 3):
                    // one that can't fit isn't read.
                    let size = match picture.strip_prefix("base64:") {
                        Some(b64) => Some(b64.trim().len()),
                        None => std::fs::metadata(picture)
                            .ok()
                            .map(|m| usize::try_from(m.len()).unwrap_or(usize::MAX).div_ceil(3).saturating_mul(4)),
                    };
                    let read = if !has_dir && picture.strip_prefix("base64:").is_none() && qgis::is_relative(picture) {
                        Err(format!("{picture}, relative to the project's folder, which the import wasn't given (pass dir)"))
                    } else if size.is_some_and(|size| size > left) {
                        pictures.over.insert(picture.clone());
                        over += 1;
                        continue;
                    } else if let Some(b64) = picture.strip_prefix("base64:") {
                        super::graphics::picture(&json!({"image": b64}))
                    } else {
                        super::graphics::picture(&json!({"path": picture}))
                    };
                    if read.as_ref().is_ok_and(|b64| b64.len() > left) {
                        pictures.over.insert(picture.clone());
                        None
                    } else {
                        pictures.read.insert(picture.clone(), read.clone());
                        fresh = true;
                        Some(read)
                    }
                }
            };
            match read {
                None => {
                    over += 1;
                    continue;
                }
                Some(Ok(b64)) => {
                    if a.keep_aspect
                        && let Some(size) = rustgis_render::picture_size(&b64)
                    {
                        // Fitted where QGIS fits it, on the map (`canvas`), then back to
                        // longitude/latitude; else in the layer's coordinates (noted). Inside the
                        // box placed above, so it takes too unless the transform is odd there
                        // (then a picture just read isn't kept, as it is charged to nothing).
                        let on_canvas = canvas.and_then(|c| {
                            // The placed box's own corners on the canvas (a box across ±180° is
                            // fitted in the layer's coordinates instead).
                            let e = g.geometry.extent();
                            if e.max_x - e.min_x > 180.0 {
                                return None;
                            }
                            let failed = std::cell::Cell::new(false);
                            let on = g.geometry.map_coords(&|p| {
                                c.from_lonlat(p).filter(|p| p.x.is_finite() && p.y.is_finite()).unwrap_or_else(|| {
                                    failed.set(true);
                                    p
                                })
                            });
                            if failed.get() {
                                return None;
                            }
                            let fit = fitted(&on, size);
                            failed.set(false);
                            let back = fit.map_coords(&|p| {
                                c.to_lonlat(p).filter(|p| p.x.is_finite() && p.y.is_finite() && p.y.abs() <= 90.0).unwrap_or_else(|| {
                                    failed.set(true);
                                    p
                                })
                            });
                            (!failed.get()).then_some(back)
                        });
                        let fit = match on_canvas {
                            Some(fit) => Some(fit),
                            None => {
                                fitted_in_layer += 1;
                                place(&fitted(&a.graphic.geometry, size))
                            }
                        };
                        let Some(fit) = fit else {
                            if fresh {
                                pictures.read.remove(picture);
                            }
                            unplaced += 1;
                            continue;
                        };
                        g.geometry = fit;
                    }
                    pictures.bytes += b64.len();
                    g.image = Some(b64);
                }
                Some(Err(e)) => {
                    unread += 1;
                    first_error.get_or_insert(e);
                    continue;
                }
            }
        }
        graphics.push(g);
    }
    let n = |count: usize, one: &str, many: &str| if count == 1 { one.to_string() } else { format!("{count} {many}") };
    if unplaced > 0 {
        report.skipped(
            name,
            n(
                unplaced,
                "an annotation that doesn't take to longitude and latitude",
                "annotations that don't take to longitude and latitude",
            ),
        );
    }
    if fitted_in_layer > 0 {
        let what = n(fitted_in_layer, "a picture whose aspect is locked keeps it", "pictures whose aspect is locked keep it");
        report
            .approximated(name, format!("{what} in the layer's coordinates, not on the map as QGIS keeps it (the map's coordinates don't take its box, or it lies across ±180°)"));
    }
    // Texts over shapes or pictures drawn after them (RustGIS draws a layer's texts last), and
    // those over all other graphics, which a layer above may cover: from what is placed.
    let last_other = graphics.iter().rposition(|g| g.kind != "text");
    let under = last_other.map_or(0, |i| graphics.iter().take(i).filter(|g| g.kind == "text").count());
    let drawn = Drawn {
        texts_on_top: graphics.iter().skip(last_other.map_or(0, |i| i + 1)).filter(|g| g.kind == "text").count(),
        others: last_other.is_some(),
    };
    // Hidden or not: shown later, it draws so.
    if under > 0 {
        let what = n(
            under,
            "an annotation text QGIS draws under other annotations is drawn over them",
            "annotation texts QGIS draws under other annotations are drawn over them",
        );
        report.approximated(name, format!("{what}: RustGIS draws a layer's texts after its shapes and pictures"));
    }
    if let Some(e) = first_error {
        // Its background and frame are still drawn.
        let what = n(unread, "a picture annotation's picture, which doesn't read", "picture annotations' pictures, which don't read");
        report.skipped(name, format!("{what} (the first: {e})"));
    }
    if over > 0 {
        let what = n(over, "a picture annotation's picture", "picture annotations' pictures");
        report.skipped(name, format!("{what} past the {} of pictures an import takes", size_text(MAX_PICTURE_BYTES)));
    }
    let Some(l) = s.map_mut().layer_mut(id) else { return Err("the layer wasn't added".into()) };
    for g in graphics {
        rustgis_map::graphics::push(&mut l.dataset, g);
    }
    l.touch_geometry();
    l.visible = layer.visible;
    // Only what an annotation layer has: its opacity, blending and scale range (a renderer or
    // labels in an odd file would restyle the graphics).
    let st = &layer.style;
    qgis::Style { transparency: st.transparency, blend: st.blend, min_scale: st.min_scale, max_scale: st.max_scale, ..Default::default() }
        .apply(l);
    Ok((l.name.clone(), drawn))
}

/// The most bytes of pictures (base64) one import adds to graphics layers (in tests, a few
/// small pictures').
const MAX_PICTURE_BYTES: usize = if cfg!(test) { 1 << 10 } else { 64 << 20 };

/// `bytes` in MiB, or KiB below one.
fn size_text(bytes: usize) -> String {
    if bytes >= 1 << 20 { format!("{} MiB", bytes >> 20) } else { format!("{} KiB", bytes >> 10) }
}

/// The pictures an import has read, by file (or `base64:` text), and the bytes it has added to
/// placed graphics.
#[derive(Default)]
struct Pictures {
    /// Those read that fit the budget when read, and those that don't read.
    read: std::collections::HashMap<String, Result<String, String>>,
    /// Those read that didn't fit what was left of the budget, not kept.
    over: std::collections::HashSet<String>,
    bytes: usize,
}

/// The box `rect` (a rectangle's polygon) shrunk to a picture's aspect (`size`: its width and
/// height in pixels), centred in it, as QGIS fits a picture whose aspect is locked.
fn fitted(rect: &rustgis_data::Geometry, (w, h): (u32, u32)) -> rustgis_data::Geometry {
    let e = rect.extent();
    let (bw, bh) = (e.max_x - e.min_x, e.max_y - e.min_y);
    if w == 0 || h == 0 || !(bw > 0.0 && bh > 0.0) {
        return rect.clone();
    }
    let aspect = f64::from(w) / f64::from(h);
    let (fw, fh) = if bw / bh > aspect { (bh * aspect, bh) } else { (bw, bw / aspect) };
    let (cx, cy) = ((e.min_x + e.max_x) / 2.0, (e.min_y + e.max_y) / 2.0);
    let (x0, x1, y0, y1) = (cx - fw / 2.0, cx + fw / 2.0, cy - fh / 2.0, cy + fh / 2.0);
    rustgis_data::Geometry::Polygon(vec![vec![vec![
        Coord::new(x0, y0),
        Coord::new(x1, y0),
        Coord::new(x1, y1),
        Coord::new(x0, y1),
        Coord::new(x0, y0),
    ]]])
}

/// An imported QGIS expression read on `ds`. QGIS measures `$area`, `$perimeter` and `$length`
/// in the project's units, which aren't read yet, so an expression using them doesn't read.
fn parse_imported(text: &str, ds: &rustgis_data::Dataset) -> Result<rustgis_data::query::Expr, String> {
    if qgis::reads_measure(text) {
        return Err("it measures in the QGIS project's units, which RustGIS doesn't read yet".into());
    }
    rustgis_data::query::parse(text, ds)
}

/// Checks what a style names against the layer's fields: its label classes' clauses and fields
/// (a class that can't label is switched off), the field a Unique Values or Graduated Colors
/// renderer classes by, and the expression its classes (rules or categories, `by`) are picked by.
fn check_fields(
    l: &mut rustgis_map::Layer,
    name: &str,
    rules: &[qgis::RuleFilter],
    by: Option<&qgis::CategoriesBy>,
    bool_classes: &[String],
    report: &mut Report,
) {
    for c in std::iter::once(&mut l.labels.default).chain(l.labels.classes.iter_mut()) {
        if let Some(w) = c.where_.clone()
            && let Err(e) = parse_imported(&w, &l.dataset)
        {
            report.skipped(name, format!("the label class \"{}\", whose clause `{w}` doesn't fit the layer ({e})", c.name));
            c.visible = false;
        }
        // Its data-defined text, size, colour and angle: an expression that doesn't read here is
        // left out, as the symbol's are below.
        for (what, slot) in [("text", &mut c.by.text), ("size", &mut c.by.size), ("colour", &mut c.by.color), ("angle", &mut c.by.angle)] {
            if let Some(text) = slot.as_deref()
                && let Err(e) = parse_imported(text, &l.dataset)
            {
                report.skipped(name, format!("the label {what} `{text}` of the class \"{}\" ({e})", c.name));
                *slot = None;
            }
        }
        // (A class with no field set labels nothing, and isn't noted; one whose text is an
        // expression needs none.)
        if c.visible && c.by.text.is_none() && !c.field.is_empty() && l.dataset.field_index(&c.field).is_none() {
            report.skipped(name, format!("labels from the field {}, which the layer hasn't", c.field));
            c.visible = false;
        }
    }
    // Classes by expression (the style's rule-based renderer's): a rule whose filter doesn't read
    // here is left out, the rest kept.
    // (Whether the categories were set aside for one symbol, or nothing, below.)
    let mut fell_back = false;
    if l.renderer.expression().is_some() && !rules.is_empty() {
        let kept = fit_rules(l, name, rules, report);
        note_overlapping_rules(l, name, &kept, report);
    } else if let Some(by) = by
        && l.renderer.expression().is_some()
        && let Some(fi) = l.dataset.field_index(&by.qgis)
    {
        // QGIS classes by a field when its attribute names one, as it writes a name with spaces
        // or other characters (unquoted): classed by that field.
        if let Some(field) = l.dataset.fields.get(fi)
            && let rustgis_map::Renderer::Unique { field: f, expression, .. } = &mut l.renderer
        {
            *f = field.name.clone();
            *expression = None;
        }
    } else if let Some(by) = by
        && let Some(e) = l.renderer.expression().map(str::to_string)
        && let Err(why) = parse_imported(&e, &l.dataset)
    {
        // The style's categorized renderer by an expression that doesn't read here: every
        // feature drawn with its first drawn category's symbol, as before such renderers were
        // classed (none: as the rest).
        fell_back = true;
        let mut drawn = "as the rest, which draws nothing: no category is drawn";
        if let Some((symbol, strokes, fills)) = &by.first {
            l.renderer = rustgis_map::Renderer::Single(*symbol);
            l.line_strokes.clone_from(strokes);
            // (Only a polygon layer's fills draw, as `Style::apply` sets them.)
            if l.geometry_type() == rustgis_data::GeometryType::Polygon {
                l.fill_layers.clone_from(fills);
            }
            drawn = "with its first category's symbol (of those switched on)";
        }
        report.approximated(
            name,
            format!("its classes by the expression `{}`, which doesn't fit the layer ({why}): every feature drawn {drawn}", by.qgis),
        );
    }
    // The style's boolean categories, matching values as they print: by an expression, a
    // boolean's (`True`); by a field, its own (a boolean field's `True`, a text field's `true`).
    // A value another category already has, or two categories share, stays as written.
    if let rustgis_map::Renderer::Unique { field, expression, .. } = &l.renderer
        && !fell_back
        && !bool_classes.is_empty()
        && let Some(printed) = match expression {
            Some(_) => Some(true),
            None => l.dataset.field_index(field).and_then(|i| l.dataset.fields.get(i)).map(|f| f.ty == rustgis_data::FieldType::Boolean),
        }
    {
        let (mut taken, mut shared): (Vec<&str>, Vec<&str>) = (Vec::new(), Vec::new());
        for k in bool_classes {
            let Some(to) = qgis::bool_key(k, printed) else { continue };
            if to == k {
                continue;
            }
            let twice =
                matches!(&l.renderer, rustgis_map::Renderer::Unique { classes, .. } if classes.iter().filter(|c| c.0 == *k).count() > 1);
            if twice {
                shared.push(k);
            } else if !qgis::rename_class(&mut l.renderer, k, to) {
                taken.push(k);
            }
        }
        let count = |n: usize| if n == 1 { "1 boolean category".to_string() } else { format!("{n} boolean categories") };
        if let Some(first) = taken.first() {
            report.approximated(
                name,
                format!(
                    "{} kept as written (the first: {first}), as another category has the value it would match: its features take that category's symbol",
                    count(taken.len())
                ),
            );
        }
        if let Some(first) = shared.first() {
            report.approximated(
                name,
                format!(
                    "{} kept as written (the first: {first}), its value listed by more than one category: its features drawn as the rest",
                    count(shared.len())
                ),
            );
        }
    }
    let field = match &l.renderer {
        rustgis_map::Renderer::Unique { expression: Some(_), .. } => None,
        rustgis_map::Renderer::Unique { field, .. } | rustgis_map::Renderer::Graduated { field, .. } => Some(field.clone()),
        _ => None,
    };
    if let Some(f) = field.filter(|f| l.dataset.field_index(f).is_none()) {
        report.approximated(name, format!("classes by the field {f}, which the layer hasn't: every feature draws as the rest"));
    }
    // The symbol's data-defined settings: an expression that doesn't read here (a QGIS function
    // or variable RustGIS lacks, a field the layer hasn't) is left out.
    let by = &mut l.symbol_by;
    for (what, slot) in [
        ("size", &mut by.size),
        ("fill colour", &mut by.fill),
        ("outline colour", &mut by.stroke),
        ("outline width", &mut by.stroke_width),
        ("angle", &mut by.angle),
    ] {
        if let Some(text) = slot.as_deref()
            && let Err(e) = parse_imported(text, &l.dataset)
        {
            report.skipped(name, format!("its data-defined symbol {what} `{text}` ({e})"));
            *slot = None;
        }
    }
}

/// Leaves the rules of a rule-based renderer (classed by expression) that don't read on `l`'s
/// fields out of its classes, each noted; none left: the first rule's symbol for every feature,
/// noted. The rules kept.
fn fit_rules(l: &mut rustgis_map::Layer, name: &str, rules: &[qgis::RuleFilter], report: &mut Report) -> Vec<qgis::RuleFilter> {
    let mut kept = Vec::new();
    // Those that don't fit, said once: how many, and the first.
    let mut unfit: Option<(String, usize)> = None;
    for r in rules {
        match parse_imported(&r.when, &l.dataset) {
            Ok(_) => kept.push(r.clone()),
            Err(e) => match &mut unfit {
                Some((_, n)) => *n += 1,
                None => unfit = Some((format!("\"{}\", whose filter `{}` doesn't fit the layer ({e})", r.key, r.condition), 1)),
            },
        }
    }
    if let Some((first, n)) = unfit {
        let what =
            if n == 1 { format!("the rule {first}") } else { format!("{n} rules whose filters don't fit the layer, the first {first}") };
        report.skipped(name, what);
    }
    if kept.len() == rules.len() {
        return kept;
    }
    let rustgis_map::Renderer::Unique { classes, class_strokes, class_fills, expression, .. } = &mut l.renderer else { return kept };
    if kept.is_empty() {
        // As before rules were classed: the first rule's symbol, strokes and fills.
        let first = rules.first().and_then(|r| classes.iter().find(|c| c.0 == r.key)).cloned();
        if let Some((key, sym)) = first {
            let strokes = class_strokes.get(&key).cloned().unwrap_or_default();
            let fills = class_fills.get(&key).cloned().unwrap_or_default();
            l.renderer = rustgis_map::Renderer::Single(sym);
            l.line_strokes = strokes;
            l.fill_layers = fills;
        }
        report.approximated(name, "its rules, none of whose filters fit the layer, drawn with the first rule's symbol");
        return kept;
    }
    let keys: std::collections::HashSet<&str> = kept.iter().map(|r| r.key.as_str()).collect();
    classes.retain(|c| keys.contains(c.0.as_str()));
    class_strokes.retain(|k, _| keys.contains(k.as_str()));
    class_fills.retain(|k, _| keys.contains(k.as_str()));
    *expression = Some(qgis::rules_expression(&kept));
    kept
}

/// The most rule checks counted (rules × features): past it, the features that several rules
/// pick are noted as a possibility.
const MAX_RULE_CHECKS: usize = 20_000_000;

/// Notes the features of `l` that more than one of a rule-based renderer's `rules` picks at some
/// scale: QGIS draws such a feature with each rule's symbol, RustGIS with the first's.
fn note_overlapping_rules(l: &rustgis_map::Layer, name: &str, rules: &[qgis::RuleFilter], report: &mut Report) {
    let ds = &l.dataset;
    let mut compiled: Vec<_> = rules.iter().filter_map(|r| Some((r, parse_imported(&r.condition, ds).ok()?))).collect();
    // Only rules some scale draws together can pick a feature together: in the order of where
    // they start zoomed in, a rule meets one before it when it starts within the farthest out
    // any of those reaches (a sweep over their scale ranges).
    compiled.sort_by(|a, b| a.0.inside.unwrap_or(0.0).total_cmp(&b.0.inside.unwrap_or(0.0)));
    if !together(compiled.iter().map(|c| c.0)) {
        return;
    }
    let what = "each with the first rule that picks it, where QGIS draws it with every one";
    // Rules read at no map scale can't be counted for one that reads it (an else rule beside a
    // rule with a scale range does too).
    let scaled = compiled.iter().any(|(r, _)| qgis::reads_scale(&r.condition));
    if scaled || compiled.len().saturating_mul(ds.features.len()) > MAX_RULE_CHECKS || l.source.is_some() {
        report.approximated(name, format!("features that more than one of its rules picks (if any), drawn {what}"));
        return;
    }
    let env = rustgis_data::query::Env::default();
    // (Only the features its definition query keeps are drawn.)
    let shown = l.visible_rows();
    let n = (0..ds.features.len())
        .filter(|row| shown.get(*row).copied().unwrap_or(false) && ds.features.get(*row).is_some_and(|f| f.geometry.is_some()))
        .filter(|row| together(compiled.iter().filter(|(_, e)| rustgis_data::query::matches(e, ds, *row, &env)).map(|c| c.0)))
        .count();
    if n > 0 {
        report.approximated(name, format!("{n} features that more than one of its rules picks, drawn {what}"));
    }
}

/// Whether two of `rules` (in the order of where they start zoomed in) are drawn at some scale
/// together: one starts within the farthest out any rule before it reaches.
fn together<'a>(rules: impl Iterator<Item = &'a qgis::RuleFilter>) -> bool {
    let mut reach: Option<f64> = None;
    // (A range that starts in beyond where it ends out is never drawn.)
    for r in rules.filter(|r| r.inside.unwrap_or(0.0) <= r.out.unwrap_or(f64::INFINITY)) {
        if reach.is_some_and(|out| r.inside.unwrap_or(0.0) <= out) {
            return true;
        }
        reach = Some(reach.unwrap_or(f64::NEG_INFINITY).max(r.out.unwrap_or(f64::INFINITY)));
    }
    false
}

/// Gives what a QGIS web layer was added as the layer's visibility, opacity and blending mode.
/// `added` is what `map.addTiles` or `service.add` answered: a tile layer's `index`, or no index
/// when the service offered the layer as features or a coverage, which come in as a map layer
/// (the active one, if it isn't `before` any more), which takes the scale range too. A tile layer
/// has no scale range, so one is noted.
pub(crate) fn web_settings(s: &mut Session, added: &Value, before: Option<u32>, layer: &qgis::ProjectLayer, report: &mut Report) {
    let name = layer.name.as_str();
    let st = &layer.style;
    let transparency = st.transparency.filter(|t| t.is_finite() && *t > 0.0);
    let index = added.get("index").and_then(Value::as_u64).and_then(|i| usize::try_from(i).ok());
    if index.is_none()
        && let Some(id) = s.active_layer.filter(|id| Some(*id) != before)
        && let Some(l) = s.map_mut().layer_mut(id)
    {
        l.visible = layer.visible;
        if let Some(tr) = transparency {
            l.transparency = tr.clamp(0.0, 100.0);
        }
        if let Some(blend) = st.blend {
            l.blend = blend;
        }
        if let Some(m) = st.min_scale {
            l.min_scale = m;
        }
        if let Some(m) = st.max_scale {
            l.max_scale = m;
        }
        return;
    }
    match index.and_then(|i| s.map_mut().tiles.get_mut(i)) {
        Some(t) => {
            t.visible = layer.visible;
            if let Some(blend) = st.blend {
                t.blend = blend;
            }
            if let Some(tr) = transparency {
                t.opacity = (1.0 - tr / 100.0).clamp(0.0, 1.0);
            }
        }
        None => {
            if !layer.visible {
                report.approximated(name, "a hidden web layer, added shown");
            }
            if transparency.is_some() {
                report.approximated(name, "its transparency: added opaque");
            }
            if st.blend.is_some_and(|b| b != rustgis_map::effects::Blend::default()) {
                report.approximated(name, "its blending mode: added as normal");
            }
        }
    }
    if st.min_scale.flatten().is_some() || st.max_scale.flatten().is_some() {
        report.approximated(name, "its scale range: a web layer draws at every scale");
    }
}

/// Goes to the project's saved view, or to its layers when that view shows none of them.
fn go_to_view(s: &mut Session, project: &qgis::Project, degrees: Option<Geographic>) {
    let data = s.map().full_extent();
    let saved = project.extent.and_then(|[x0, y0, x1, y1]| match degrees {
        // In degrees already (from Greenwich: the view of a project on another prime meridian
        // is left to its data).
        Some(g) if g.greenwich => Some(Extent::new(x0.max(-180.0), y0.max(-90.0), x1.min(180.0), y1.min(90.0))).filter(|e| !e.is_empty()),
        Some(_) => None,
        None => {
            let m = s.map();
            let corners = [(x0, y0), (x1, y0), (x0, y1), (x1, y1)].map(|(x, y)| m.to_lonlat(Coord::new(x, y)));
            corners
                .iter()
                .flatten()
                .fold(None, |e: Option<Extent>, c| Some(e.unwrap_or(Extent::EMPTY).union(&Extent::new(c.x, c.y, c.x, c.y))))
        }
    });
    let target = match saved {
        Some(e) if !e.is_empty() && (data.is_empty() || e.intersects(&data)) => e,
        _ => data,
    };
    if !target.is_empty() {
        s.zoom_to(&target);
    }
    // QGIS turns the map clockwise; a bearing faces the top of the view east of north.
    if project.rotation != 0.0 {
        s.map_mut().camera.set_bearing(-project.rotation);
    }
}

/// Binds `l` to the time slider on `field` (from a QGIS layer's time settings), when it has the
/// field and its values read as dates; noted when not. A streamed layer's rows aren't read yet,
/// so it is bound on the field alone, its dates read as its features are. Whether it was bound.
fn bind_time(l: &mut rustgis_map::Layer, name: &str, field: Option<&str>, report: &mut Report) -> bool {
    let Some(field) = field else { return false };
    match l.dataset.field_index(field).and_then(|i| l.dataset.fields.get(i)) {
        Some(f) if l.is_streamed() || rustgis_map::time::field_span(&l.dataset, &f.name).is_some() => {
            if l.is_streamed() {
                report.approximated(
                    name,
                    format!("its time settings, by the field {field}, bound before its features are read: one whose value isn't a date is hidden while the slider is on, or every one shows when none reads as one"),
                );
            }
            l.time = Some(rustgis_map::time::TimeBinding::new(f.name.clone()));
            true
        }
        Some(_) => {
            report.skipped(name, format!("its time settings, by the field {field}, none of whose values read as dates"));
            false
        }
        None => {
            report.skipped(name, format!("its time settings, by the field {field}, which the layer hasn't"));
            false
        }
    }
}

/// Adds the project's relations between layers that came in as RustGIS relationships (origin:
/// the referenced, parent layer; destination: the referencing, child layer). A relation whose
/// layers didn't come in, or haven't its fields, is noted; one whose name is taken is numbered.
/// One to a layer that came in as several tables is related to the one with the field, noted, as
/// QGIS's own table (by `layerid`) isn't known; a relation of such a layer to itself whose two
/// fields are in two of its tables isn't made, and is noted.
fn add_relations(s: &mut Session, project: &qgis::Project, came_in: &[CameIn], report: &mut Report) {
    // The map layer a project layer came in as, with the field. A file of several layers (one
    // QGIS names by `layerid`, which isn't read) adds them all: when more than one has the field,
    // which the relation means isn't known (`Err`), rather than taking the first; when one has
    // it, it is taken, said to be a guess (`true`).
    let find = |s: &Session, id: &str, field: &str| -> Option<Result<(String, String, bool), ()>> {
        let index = project.layers.iter().position(|l| l.id == id)?;
        let c = came_in.iter().find(|c| c.index == index)?;
        let mut with: Vec<(String, String)> = c
            .ids
            .iter()
            .filter_map(|lid| {
                let l = s.map().layer(*lid).filter(|l| !l.is_raster())?;
                let f = l.dataset.field_index(field).and_then(|i| l.dataset.fields.get(i))?;
                Some((l.name.clone(), f.name.clone()))
            })
            .collect();
        // Several vector tables came in for it (a raster beside one can't be QGIS's vector layer).
        let guessed = c.ids.iter().filter(|lid| s.map().layer(**lid).is_some_and(|l| !l.is_raster())).count() > 1;
        match with.len() {
            0 => None,
            1 => with.pop().map(|(layer, field)| Ok((layer, field, guessed))),
            _ => Some(Err(())),
        }
    };
    let mut missing = Vec::new();
    // Those between layers that came in as several, either of which could be meant.
    let mut ambiguous = Vec::new();
    // Those related to the one table of several that has the field.
    let mut guesses = Vec::new();
    // Those of a layer to itself whose fields are each in another of the tables it came in as.
    let mut split = Vec::new();
    let mut names = Names::new(s.map().editing.relationships.iter().map(|r| r.name.as_str()), MAX_RELATION_NAME);
    for r in &project.relations {
        let (origin, destination) = (find(s, &r.referenced, &r.referenced_field), find(s, &r.referencing, &r.referencing_field));
        let (Some(Ok((origin, origin_key, origin_guessed))), Some(Ok((destination, destination_key, destination_guessed)))) =
            (&origin, &destination)
        else {
            // (A layer that didn't come in, or hasn't the field, is the reason first.)
            if origin.is_none() || destination.is_none() {
                missing.push(r.name.as_str());
            } else {
                ambiguous.push(r.name.as_str());
            }
            continue;
        };
        // A relation of a layer to itself relates one table: two tables of it, each with one of
        // the fields, can't both be QGIS's layer.
        if r.referenced == r.referencing && origin != destination {
            split.push(r.name.as_str());
            continue;
        }
        if *origin_guessed || *destination_guessed {
            guesses.push(r.name.as_str());
        }
        let (origin, origin_key, destination, destination_key) =
            (origin.clone(), origin_key.clone(), destination.clone(), destination_key.clone());
        // A relationship names its layers: their names are unique (a layer added under a taken
        // name, ignoring case, is numbered), so it relates the ones that came in.
        let name = names.take(&r.name);
        s.map_mut().editing.relationships.push(rustgis_map::design::Relationship {
            name,
            origin,
            origin_key,
            destination,
            destination_key,
        });
    }
    let listed = |names: &[&str]| {
        let mut text = names.iter().take(8).copied().collect::<Vec<_>>().join(", ");
        if names.len() > 8 {
            text.push_str(", …");
        }
        format!("{}: {text}", names.len())
    };
    if !missing.is_empty() {
        report.skipped("", format!("its relations between layers that didn't come in or haven't the fields ({})", listed(&missing)));
    }
    if !ambiguous.is_empty() {
        report.skipped(
            "",
            format!(
                "its relations to a layer that came in as several tables with the field, any of which it could mean ({})",
                listed(&ambiguous)
            ),
        );
    }
    if !split.is_empty() {
        report.skipped(
            "",
            format!(
                "its relations of a layer to itself whose two fields are in two of the tables it came in as, neither holding both ({})",
                listed(&split)
            ),
        );
    }
    if !guesses.is_empty() {
        report.approximated(
            "",
            format!(
                "its relations to a layer that came in as several tables, related to the one table with the field, QGIS's own not being known ({})",
                listed(&guesses)
            ),
        );
    }
    if names.cut > 0 {
        report.approximated("", format!("relation names cut to {MAX_RELATION_NAME} characters ({})", names.cut));
    }
}

/// The longest relationship name an import keeps.
const MAX_RELATION_NAME: usize = 200;

/// Names made unique as they are taken, ignoring case: a taken one numbered (`Name 2`, `Name 3`,
/// …), kept to `max` characters. Each name's next number is remembered, so many alike don't
/// count up from 2 every time.
struct Names {
    taken: std::collections::HashSet<String>,
    next: std::collections::HashMap<String, u64>,
    max: usize,
    /// Names taken that were cut to `max`.
    cut: usize,
}

impl Names {
    fn new<'a>(taken: impl Iterator<Item = &'a str>, max: usize) -> Names {
        Names { taken: taken.map(str::to_lowercase).collect(), next: std::collections::HashMap::new(), max, cut: 0 }
    }

    fn take(&mut self, base: &str) -> String {
        let mut cut = base.chars().nth(self.max).is_some();
        let base: String = base.chars().take(self.max).collect();
        let key = base.to_lowercase();
        let mut k = self.next.get(&key).copied().unwrap_or(1);
        let name = loop {
            let name = if k <= 1 {
                base.clone()
            } else {
                let suffix = format!(" {k}");
                let room = self.max.saturating_sub(suffix.len());
                // (A name shortened to make room for its number is cut too.)
                cut |= base.chars().nth(room).is_some();
                base.chars().take(room).chain(suffix.chars()).collect()
            };
            if self.taken.insert(name.to_lowercase()) {
                if cut {
                    self.cut += 1;
                }
                break name;
            }
            k = k.saturating_add(1);
        };
        self.next.insert(key, k.saturating_add(1));
        name
    }
}

/// The most coordinate systems an import's bookmarks are read in: each is parsed, and a PROJ
/// string stays in the run's table of systems.
const MAX_BOOKMARK_CRS: usize = 100;

/// Adds the project's bookmarks, each the place its extent shows (in its own coordinate system,
/// else the project's), turned as QGIS turns it. One whose coordinate system doesn't read, or
/// past the bookmarks a map keeps, is noted.
///
/// On the UI thread, with the rest of `project.importQgis`, which replaces the session's map: the
/// work is bounded (at most 10,000 bookmarks, in at most [`MAX_BOOKMARK_CRS`] coordinate systems,
/// each parsed once), well under a second.
fn add_bookmarks(s: &mut Session, project: &qgis::Project, report: &mut Report) {
    if project.bookmarks.is_empty() {
        return;
    }
    let (w, h) = s.view_size;
    let m = s.map_mut();
    let view = m.camera;
    // Names compare ignoring case, as a map's bookmarks do; a taken one is numbered.
    let mut names = Names::new(m.bookmarks.iter().map(|b| b.name.as_str()), super::navigate::MAX_BOOKMARK_NAME);
    // Those with no coordinate system, in one RustGIS can't read, and those whose place
    // doesn't all lie in theirs.
    let (mut none, mut unread, mut outside) = (Vec::new(), Vec::new(), Vec::new());
    // Those the map's projection doesn't reach.
    let mut beyond = Vec::new();
    // Those in coordinate systems past the first `MAX_BOOKMARK_CRS`.
    let mut many = Vec::new();
    // Those read in the project's coordinate system, having none of their own.
    let mut assumed = Vec::new();
    // Those reaching past the latitudes a Web Mercator map shows (about 85°), fitted to the part
    // it does.
    let mut polar = Vec::new();
    let mercator = matches!(m.flat(), rustgis_map::projection::Flat::Mercator);
    let mut over = 0usize;
    // One transformer per coordinate system.
    let mut transformers: std::collections::HashMap<&str, Option<rustgis_data::raster::crs::Transformer>> =
        std::collections::HashMap::new();
    for b in &project.bookmarks {
        let Some(crs) = b.crs.as_deref().or(project.crs.as_deref()) else {
            none.push(b.name.as_str());
            continue;
        };
        let own_crs = b.crs.is_some();
        if !transformers.contains_key(crs) && transformers.len() >= MAX_BOOKMARK_CRS {
            many.push(b.name.as_str());
            continue;
        }
        let t = transformers.entry(crs).or_insert_with(|| rustgis_data::raster::crs::Crs::parse(crs).and_then(|c| c.transformer()).ok());
        let Some(t) = t.as_ref() else {
            unread.push(b.name.as_str());
            continue;
        };
        let [x0, y0, x1, y1] = b.extent;
        let corners: Vec<Coord> = [(x0, y0), (x1, y0), (x0, y1), (x1, y1)]
            .into_iter()
            .filter_map(|(x, y)| t.to_lonlat(Coord::new(x, y)))
            .filter(|c| (-180.0..=180.0).contains(&c.x) && (-90.0..=90.0).contains(&c.y))
            .collect();
        let e = corners.iter().fold(Extent::EMPTY, |e, c| e.union(&Extent::new(c.x, c.y, c.x, c.y)));
        if corners.len() < 4 || e.is_empty() {
            outside.push(b.name.as_str());
            continue;
        }
        if m.bookmarks.len() >= super::navigate::MAX_BOOKMARKS {
            over += 1;
            continue;
        }
        // QGIS's extent is the view's own rectangle, turned with it (measured: turning the map
        // keeps its scale): fitted facing north, then turned.
        m.camera.set_bearing(0.0);
        // (Not one wholly past a Web Mercator map's 85°, nor one its projection doesn't reach.)
        if !m.fit_lonlat(&e, w, h) {
            beyond.push(b.name.as_str());
            continue;
        }
        m.camera.set_bearing(-b.rotation);
        if !own_crs {
            assumed.push(b.name.as_str());
        }
        if mercator && (e.max_y > rustgis_geom::MAX_LAT || e.min_y < -rustgis_geom::MAX_LAT) {
            polar.push(b.name.as_str());
        }
        let name = names.take(&b.name);
        m.bookmarks.push(rustgis_map::Bookmark::new(name, m.camera));
    }
    m.camera = view;
    let listed = |names: &[&str]| {
        let mut text = names.iter().take(8).copied().collect::<Vec<_>>().join(", ");
        if names.len() > 8 {
            text.push_str(", …");
        }
        format!("{}: {text}", names.len())
    };
    if !none.is_empty() {
        report.skipped("", format!("its bookmarks with no coordinate system, in a project without one ({})", listed(&none)));
    }
    if !unread.is_empty() {
        report.skipped("", format!("its bookmarks in a coordinate system RustGIS can't read ({})", listed(&unread)));
    }
    if !assumed.is_empty() {
        report.approximated(
            "",
            format!("its bookmarks with no coordinate system of their own, read in the project's ({})", listed(&assumed)),
        );
    }
    if !polar.is_empty() {
        report.approximated(
            "",
            format!("its bookmarks reaching past the 85° a Web Mercator map shows, fitted to the part it does ({})", listed(&polar)),
        );
    }
    if !many.is_empty() {
        report.skipped("", format!("its bookmarks in coordinate systems past the first {MAX_BOOKMARK_CRS} ({})", listed(&many)));
    }
    if !outside.is_empty() {
        report.skipped("", format!("its bookmarks whose place lies outside their coordinate system's area ({})", listed(&outside)));
    }
    if !beyond.is_empty() {
        report.skipped("", format!("its bookmarks whose place the map's projection doesn't reach ({})", listed(&beyond)));
    }
    if names.cut > 0 {
        report.approximated("", format!("bookmark names cut to {} characters ({})", super::navigate::MAX_BOOKMARK_NAME, names.cut));
    }
    if over > 0 {
        report.skipped("", format!("its bookmarks past those a map keeps ({over})"));
    }
}

/// A coordinate system in degrees (EPSG:4326, ETRS89, NAD83, …), which QGIS draws as plate carrée.
#[derive(Clone, Copy, Debug)]
struct Geographic {
    /// On WGS 84, or a datum PROJ takes as the same (no shift to WGS 84).
    wgs84: bool,
    /// Longitudes from Greenwich (not from another prime meridian, such as Paris's).
    greenwich: bool,
}

impl Geographic {
    /// `None` for a coordinate system that isn't in degrees, or isn't known: an `EPSG:` code,
    /// `OGC:CRS84`, or a PROJ string (a project QGIS saved without an authority code).
    fn of(crs: &str) -> Option<Geographic> {
        if crs.eq_ignore_ascii_case("OGC:CRS84") {
            return Some(Geographic { wgs84: true, greenwich: true });
        }
        let def = if crs.trim_start().starts_with('+') {
            crs
        } else {
            let code = crs.strip_prefix("EPSG:")?.trim().parse::<u16>().ok()?;
            rustgis_data::raster::crs::epsg_proj4(code)?
        };
        let has = |p: &str| def.split_whitespace().any(|t| t == p);
        ["+proj=longlat", "+proj=latlong", "+proj=lonlat", "+proj=latlon"].iter().any(|p| has(p)).then(|| Geographic {
            wgs84: has("+datum=WGS84") || has("+datum=NAD83") || has("+towgs84=0,0,0,0,0,0,0") || has("+towgs84=0,0,0"),
            // `+pm=` names a meridian or gives its offset in degrees (`0`, `0.0`: Greenwich).
            greenwich: def
                .split_whitespace()
                .filter_map(|t| t.strip_prefix("+pm="))
                .all(|pm| pm.eq_ignore_ascii_case("greenwich") || pm.parse::<f64>().is_ok_and(|d| d == 0.0)),
        })
    }
}

fn note_json(n: &qgis::Note) -> Value {
    json!({"layer": n.layer, "kind": n.kind, "text": n.text, "count": n.count})
}

/// The returned report serves CLI and Python callers; the app also needs every coalesced
/// approximation and omission in its Messages pane. Report bounds and groups these notes.
fn notify_report(s: &mut Session, report: &Report) {
    let bounded = |text: &str, limit| {
        let mut chars = text.chars();
        let mut text: String = chars.by_ref().take(limit).collect();
        if chars.next().is_some() {
            text.push_str("… [truncated]");
        }
        text
    };
    for kind in [NoteKind::Approximated, NoteKind::Skipped] {
        let notes: Vec<_> = report.notes.iter().filter(|note| note.kind == kind).collect();
        let Some(first) = notes.first() else { continue };
        if notes.len() == 1 {
            let template = match kind {
                NoteKind::Approximated => "QGIS import, {layer}: {detail} (drawn differently; count {count})",
                NoteKind::Skipped => "QGIS import, {layer}: {detail} (left out; count {count})",
            };
            s.notify_text(
                crate::notice_text::NoticeText::format(
                    template,
                    [
                        ("layer", bounded(if first.layer.is_empty() { "project" } else { &first.layer }, 80).into()),
                        ("detail", bounded(&first.text, 320).into()),
                        ("count", first.count.to_string().into()),
                    ],
                ),
                false,
            );
            continue;
        }
        // One notification per kind across layers. The command result retains the complete
        // report; the app gets a bounded sample and the total, including notes beyond the sample.
        let count = notes.iter().fold(0usize, |count, note| count.saturating_add(note.count));
        let layers = notes.iter().map(|note| &note.layer).collect::<std::collections::HashSet<_>>().len();
        let mut samples = notes
            .iter()
            .take(8)
            .map(|note| {
                format!(
                    "{}: {} (count {})",
                    bounded(if note.layer.is_empty() { "project" } else { &note.layer }, 80),
                    bounded(&note.text, 320),
                    note.count
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        if notes.len() > 8 {
            samples.push_str(&format!("; {} additional notes not listed here", notes.len() - 8));
        }
        let template = match kind {
            NoteKind::Approximated => "QGIS import: {count} approximations; affected layers: {layers}. {samples}",
            NoteKind::Skipped => "QGIS import: {count} omissions; affected layers: {layers}. {samples}",
        };
        s.notify_text(
            crate::notice_text::NoticeText::format(
                template,
                [("count", count.to_string().into()), ("layers", layers.to_string().into()), ("samples", samples.into())],
            ),
            false,
        );
    }
}

fn import_style(s: &mut Session, p: &Value) -> CmdResult {
    let text = qgis_text(p)?;
    let id = s.layer_id(p)?;
    let name = s.map().layer(id).map(|l| l.name.clone()).ok_or("no such layer")?;
    let mut report = Report::default();
    let style = qgis::read_style(&text, &name, &mut report)?;
    let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
    if l.raster.is_some() {
        // A style that doesn't fit leaves the raster as it was, its opacity and scale range too.
        if let Some(why) = l.raster.as_ref().and_then(|r| style.raster_misfit(r)) {
            report.skipped(&name, why);
        } else {
            // Pixels still on their way (a remote raster) can't be checked: the style is applied,
            // and a band it reads that the file hasn't is drawn from the file's last band.
            if style.raster.is_some() && l.raster.as_ref().is_some_and(|r| r.data.is_none()) {
                report.approximated(
                    &name,
                    "its raster colouring, applied before the raster's bands could be checked (a band the file hasn't is drawn from its last)",
                );
            }
            if style.raster.is_none() && style.renderer.is_some() {
                report.skipped(
                    &name,
                    "a feature layer's symbols and labels, which a raster hasn't (only its opacity and scale range are taken)",
                );
            }
            style.apply(l);
        }
    } else {
        if style.raster.is_some() && style.renderer.is_none() {
            report.skipped(&name, "a raster's colouring, which a feature layer hasn't (only its opacity and scale range are taken)");
        }
        // Its symbols' own fills are drawn, as with Apply Style; a style without symbols leaves
        // No Fill on.
        let no_fill_dropped = style.renderer.as_ref().and_then(|_| super::layer::end_no_fill_for_new_renderer(l));
        style.apply(l);
        check_fields(l, &name, &style.rules, style.categories_by.as_ref(), &style.bool_classes, &mut report);
        if style.diagram.is_some() {
            style.check_diagram_fields(l, &name, &mut report);
        }
        // Bound: the layer shows the slider's window at once (nothing changes while it is off).
        if bind_time(l, &name, style.time.as_deref(), &mut report) {
            s.map_mut().apply_time();
        }
        // Last: `l` borrows `s`, so the notice waits until the layer's last use.
        if let Some(notice) = no_fill_dropped {
            s.notify(notice, false);
        }
    }
    notify_report(s, &report);
    Ok(json!({"notes": report.notes.iter().map(note_json).collect::<Vec<_>>()}))
}
