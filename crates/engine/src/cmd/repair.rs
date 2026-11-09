//! Undoable replacement of linked local data without replacing the layer.

use crate::{CmdResult, CommandSpec as C, Session};
use serde_json::Value;
#[cfg(not(target_arch = "wasm32"))]
use serde_json::json;

pub fn commands() -> Vec<C> {
    vec![
        C::new("layer.repairSource", "Repair Data Source", "Data › Data Source", repair_source).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files)).params(
            r#"{"layer"?: name|id (default: the active layer), "path": "the file or folder (a Zarr store, a File Geodatabase) the layer's data moved to", "table"?: "the dataset in a GeoPackage, GDAL or DuckDB file, or a raster's subdataset (default: the one the layer reads now)"}"#),
        C::new("layer.reloadData", "Reload Data", "Data › Data Source", reload_data).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files)).params(
            r#"{"layer"?: "name|id (default: the active layer). Manually rereads linked local data, retaining presentation. Folder sources are not monitored; snapshots must be added again.", "discardEdits"?: "bool (default: false): explicitly discard pending GeoPackage edits. Undo restores in-memory edits and configuration, not overwritten files or historical streamed contents."}"#),
        C::new("map.repairTiles", "Repair Tile Archive", "Layers", repair_tiles).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files)).params(
            r#"{"name"?: text | "index"?: number (0 = top) | "id"?: catalog id, "path": "the .pmtiles or .mbtiles file the archive moved to"}"#),
    ]
}

#[cfg(not(target_arch = "wasm32"))]
fn replacement(p: &Value) -> Result<String, String> {
    let path = super::layer::s_req(p, "path")?.trim();
    if !rustgis_map::paths::is_local(path) {
        return Err(format!("{path}: give a file or folder on this computer (web addresses and databases aren't repaired here)"));
    }
    let path = rustgis_map::paths::local_path(path);
    let full = std::path::absolute(path).map_err(|e| format!("{path}: {e}"))?.display().to_string();
    if !std::path::Path::new(&full).exists() {
        return Err(format!("{full}: there's no such file or folder"));
    }
    Ok(full)
}

/// Reopens linked data after compatibility checks; undo restores the source and any reset label moves.
fn repair_source(s: &mut Session, p: &Value) -> CmdResult {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (s, p);
        Err("the web app can't link to files on this computer: add the file again with Add Data".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let id = s.layer_id(p)?;
        let full = replacement(p)?;
        let table = super::ps(p, "table").map(str::trim).filter(|t| !t.is_empty());
        let l = s.map().layer(id).ok_or("the layer no longer exists")?;
        if l.linked_file().is_none() {
            return Err(format!(
                "{}'s data is kept in the project or read from a database or web address: there's no file to repair",
                l.name
            ));
        }
        replace_linked(s, id, &full, table)?;
        Ok(json!({"layer": id, "path": full, "table": s.map().layer(id).and_then(|l| l.linked_table())}))
    }
}

/// Manual reload deliberately does not infer a link from a snapshot's historical dataset.source.
fn reload_data(s: &mut Session, p: &Value) -> CmdResult {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (s, p);
        Err("the web app can't reload local files on this computer: add the file again with Add Data".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        // Consume the private payload even if the caller changed targets: a stale read must
        // never silently become a second, synchronous read.
        let prepared = super::reads::take::<Result<Prepared, String>>(s, "layer.reloadData");
        let id = s.layer_id(p)?;
        let prepared = match prepared {
            Some(result) => result?,
            None => prepare(reload_input(s, p)?)?,
        };
        let path = prepared.input.path.clone();
        let table = prepared.input.table.clone();
        let result = if prepared.input.id != id || prepared.input.discard != (super::b(p, "discardEdits") == Some(true)) {
            Err("the reload target or discard confirmation changed while reading; retry Reload Data".into())
        } else {
            install(s, prepared)
        };
        result.map_err(|why: String| {
            let layer = s.map().layer(id);
            let hint = if layer.is_some_and(|l| l.gpkg.is_some()) {
                "Correct the file and retry Reload Data, or add the table again with Add Data."
            } else {
                "Retry Reload Data or use Repair Data Source."
            };
            format!(
                "{} ({path}): reload failed; existing data and configuration retained. {hint} {why}",
                layer.map_or("Layer", |l| l.name.as_str())
            )
        })?;
        s.notify(format!("{}: data reloaded from {path}", s.map().layer(id).map_or("Layer", |l| l.name.as_str())), false);
        Ok(json!({"layer": id, "path": path, "table": table}))
    }
}

#[cfg(not(target_arch = "wasm32"))]
struct Input {
    id: u32,
    instance: u64,
    linked_file: Option<String>,
    linked_table: Option<String>,
    kind: (bool, bool, bool),
    source: Snapshot,
    slider: Option<rustgis_map::time::TimeSlider>,
    path: String,
    table: Option<String>,
    discard: bool,
}

#[cfg(not(target_arch = "wasm32"))]
enum Snapshot {
    Vector(Box<rustgis_map::source::SourceLayer>),
    Raster(Box<rustgis_map::RasterLayer>),
    Editable {
        name: String,
        edit: Box<rustgis_map::source::GpkgEdit>,
        features: rustgis_data::Features,
        fields: Vec<rustgis_data::Field>,
        geometry: rustgis_data::GeometryType,
    },
}

#[cfg(not(target_arch = "wasm32"))]
enum Candidate {
    Vector(Box<rustgis_map::source::SourceLayer>),
    Raster(Box<rustgis_map::RasterLayer>),
    Editable(Box<super::gpkg::Candidate>),
}

#[cfg(not(target_arch = "wasm32"))]
struct Prepared {
    input: Input,
    candidate: Candidate,
}

#[cfg(not(target_arch = "wasm32"))]
fn reload_input(s: &Session, p: &Value) -> Result<Input, String> {
    let id = s.layer_id(p)?;
    let l = s.map().layer(id).ok_or("the layer no longer exists")?;
    let path = l.gpkg.as_ref().map(|g| g.path.as_str()).or_else(|| l.linked_file())
        .filter(|p| rustgis_map::paths::is_local(p))
        .ok_or_else(|| format!("{}: no linked local source to reload. Add snapshots again with Add Data; use database discard or remote refresh for server data", l.name))?
        .to_string();
    let discard = super::b(p, "discardEdits") == Some(true);
    if let Some(edit) = &l.gpkg {
        let changes = edit.versions.changes(&l.dataset.features);
        if !changes.is_empty() && !discard {
            return Err(format!(
                "{}: {} has {} inserts, {} updates and {} deletes pending; nothing was reloaded or saved. Confirm Discard Edits and Reload, or pass discardEdits:true",
                l.name,
                path,
                changes.inserts.len(),
                changes.updates.len(),
                changes.deletes.len()
            ));
        }
    }
    let table = l.gpkg.as_ref().map(|g| g.table.clone()).or_else(|| l.linked_table().map(String::from));
    capture(s, id, path, table, discard)
}

/// Capture only what reading and stale validation consume, never vector presentation.
#[cfg(not(target_arch = "wasm32"))]
fn capture(s: &Session, id: u32, path: String, table: Option<String>, discard: bool) -> Result<Input, String> {
    let l = s.map().layer(id).ok_or("the layer no longer exists")?;
    let source = if let Some(edit) = &l.gpkg {
        Snapshot::Editable {
            name: l.dataset.name.clone(),
            edit: Box::new(edit.clone()),
            features: l.dataset.features.clone(),
            fields: l.dataset.fields.clone(),
            geometry: l.dataset.geometry_type,
        }
    } else if let Some(raster) = &l.raster {
        Snapshot::Raster(Box::new(raster.clone()))
    } else {
        Snapshot::Vector(Box::new(l.source.as_ref().ok_or("the layer has no linked source")?.clone()))
    };
    Ok(Input {
        id,
        instance: l.instance_id(),
        linked_file: l.linked_file().map(String::from),
        linked_table: l.linked_table().map(String::from),
        kind: (l.raster.is_some(), l.source.is_some(), l.gpkg.is_some()),
        source,
        slider: if l.raster.is_some() { s.map().time.clone() } else { None },
        path,
        table,
        discard,
    })
}

/// Only source reads/conversion run on the worker. Installation uses the live presentation.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn reload_ahead(s: &Session, p: &Value) -> Option<super::reads::ReadWork> {
    let input = reload_input(s, p);
    Some(super::reads::ahead("Reload Data", "layer.reloadData".into(), move || input.and_then(prepare)))
}

/// Repair and both reload callers share candidate construction and installation policy.
#[cfg(not(target_arch = "wasm32"))]
fn prepare(input: Input) -> Result<Prepared, String> {
    let candidate = match &input.source {
        Snapshot::Editable { name, edit, .. } => Candidate::Editable(Box::new(super::gpkg::read_candidate(name, edit)?)),
        Snapshot::Raster(rl) => {
            let new = if rl.time_zarr.is_some() {
                let mut new = super::zarr::relink_following(input.slider.as_ref(), rl, &input.path)?;
                new.reset_replacement_read();
                new
            } else {
                rl.relinked(&input.path, input.table.as_deref())?
            };
            Candidate::Raster(Box::new(new))
        }
        Snapshot::Vector(source) => Candidate::Vector(Box::new(source.relinked(&input.path, input.table.as_deref())?)),
    };
    Ok(Prepared { input, candidate })
}

#[cfg(not(target_arch = "wasm32"))]
fn replace_linked(s: &mut Session, id: u32, path: &str, table: Option<&str>) -> Result<(), String> {
    let input = capture(s, id, path.into(), table.map(String::from), false)?;
    let prepared = prepare(input)?;
    // Reload keeps a selected branch that has become empty. Repair instead chooses a new
    // source: rows exclusively of another family are not a compatible replacement.
    if let Candidate::Vector(new) = &prepared.candidate
        && let Some(origin) = &new.duckdb
        && let Some(info) = new.info()
        && info.count == Some(0)
        && let Some(note) =
            info.notes.iter().find(|note| note.starts_with("DuckDB: ") && note.ends_with(" rows of another geometry type were left out"))
    {
        return Err(format!("{path}: the replacement table contains no {}; {note}. The layer was not changed", origin.family.label()));
    }
    install(s, prepared)
}

#[cfg(not(target_arch = "wasm32"))]
fn install(s: &mut Session, prepared: Prepared) -> Result<(), String> {
    let Prepared { input, candidate } = prepared;
    let id = input.id;
    let l = s.map().layer(id).ok_or("the layer no longer exists")?;
    let stale = || "the source, configuration or pending edits changed while reading; nothing was discarded. Retry Reload Data".to_string();
    if l.instance_id() != input.instance
        || l.linked_file() != input.linked_file.as_deref()
        || l.linked_table() != input.linked_table.as_deref()
        || (l.raster.is_some(), l.source.is_some(), l.gpkg.is_some()) != input.kind
    {
        return Err(stale());
    }
    match candidate {
        Candidate::Raster(mut new) => {
            let current = l.raster.as_ref().ok_or_else(stale)?;
            let Snapshot::Raster(prior) = &input.source else { return Err(stale()) };
            let same_pixels = match (&current.data, &prior.data) {
                (Some(a), Some(b)) => std::sync::Arc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            };
            if current.path != prior.path
                || current.in_place != prior.in_place
                || !current.same_opening(prior)
                || current.subdataset != prior.subdataset
                || current.crs != prior.crs
                || current.placement != prior.placement
                || current.style != prior.style
                || current.embedded != prior.embedded
                || current.file_name != prior.file_name
                || current.time_zarr != prior.time_zarr
                || !same_pixels
                || s.map().time != input.slider
            {
                return Err(stale());
            }
            let name = l.name.clone();
            super::raster::read_notices(s, &name, &mut new);
            // A reloaded Zarr store: reads begun before it are not joined by its new requests.
            if let Some(zt) = &new.time_zarr {
                s.zarr_reads.forget_shared(Some(&zt.store));
            }
            let l = s.map_mut().layer_mut(id).ok_or("the layer no longer exists")?;
            l.raster = Some(*new);
            l.selection.clear();
            l.touch_geometry();
        }
        Candidate::Vector(new) => {
            let (Some(current), Snapshot::Vector(prior)) = (&l.source, &input.source) else { return Err(stale()) };
            if l.gpkg.is_some()
                || !current.same_opening(prior)
                || current.path != prior.path
                || current.table != prior.table
                || current.shapefile_crs != prior.shapefile_crs
                || current.embedded != prior.embedded
                || current.file_name != prior.file_name
                || current.gdal != prior.gdal
                || current.duckdb != prior.duckdb
                || current.postgis != prior.postgis
                || current.mysql != prior.mysql
                || current.sqlserver != prior.sqlserver
            {
                return Err(stale());
            }
            let info = new.info().ok_or_else(|| format!("{}: the source did not open", input.path))?;
            compatible(l, info.geometry_type, &info.fields, &input.path)?;
            let notes = info.notes.clone();
            let moved = s.map_mut().layer_mut(id).ok_or("the layer no longer exists")?.replace_source(*new);
            reset_labels(s, id, moved);
            for note in notes {
                s.notify(format!("{}: {note}", s.map().layer(id).map_or("Layer", |l| l.name.as_str())), false);
            }
        }
        Candidate::Editable(new) => {
            let super::gpkg::Candidate { mut dataset, edit, notes, .. } = *new;
            let (Some(current), Snapshot::Editable { edit: prior, features, fields, geometry, .. }) = (&l.gpkg, &input.source) else {
                return Err(stale());
            };
            if current != prior.as_ref()
                || !current.versions.shares(&prior.versions)
                || !l.dataset.features.shares(features)
                || l.dataset.fields != *fields
                || l.dataset.geometry_type != *geometry
            {
                return Err(stale());
            }
            compatible(l, dataset.geometry_type, &dataset.fields, &input.path)?;
            let l = s.map_mut().layer_mut(id).ok_or("the layer no longer exists")?;
            let moved = l.labels.in_order().map(|class| class.moved.len()).sum();
            l.labels.default.moved.clear();
            l.labels.classes.iter_mut().for_each(|class| class.moved.clear());
            if l.dataset.geometry_type == rustgis_data::GeometryType::None
                && dataset.geometry_type != rustgis_data::GeometryType::None
                && let rustgis_map::Renderer::Single(sym) = &l.renderer
                && let Some(seed) = sym.default_seed(rustgis_data::GeometryType::None)
            {
                l.renderer = rustgis_map::Renderer::Single(rustgis_map::Symbol::default_for(dataset.geometry_type, seed));
            }
            dataset.name.clone_from(&l.dataset.name);
            l.dataset = dataset;
            l.touch_replacement();
            l.gpkg = Some(edit);
            l.selection.clear();
            l.touch_geometry();
            reset_labels(s, id, moved);
            for note in notes {
                s.notify(format!("{}: {note}", s.map().layer(id).map_or("Layer", |l| l.name.as_str())), false);
            }
        }
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn reset_labels(s: &mut Session, id: u32, moved: usize) {
    // A partial lock hides all labels not named in it, so reset the entire lock, as repair does.
    let locked = s.map().label_lock.as_ref().map_or(0, Vec::len);
    let anchors = s.map().label_lock_line_anchors.len();
    s.map_mut().label_lock = None;
    s.map_mut().label_lock_line_anchors.clear();
    if moved > 0 {
        let name = s.map().layer(id).map_or("Layer", |l| l.name.as_str());
        s.notify(format!("{moved} saved label move(s) reset in {name}: replacement data may reuse feature ids; undo restores them"), false);
    }
    if locked > 0 || anchors > 0 {
        let map = s.map();
        let layer = map.layer(id).map_or("Layer", |l| l.name.as_str());
        s.notify(format!(
            "{}: {locked} locked label placement(s) and {anchors} line anchor(s) reset across the map after replacing {layer}'s data; undo restores them",
            map.name
        ), false);
    }
}

/// Check configured fields and expressions against declarations, not rows: valid empty data works.
#[cfg(not(target_arch = "wasm32"))]
fn compatible(
    l: &rustgis_map::Layer,
    geometry: rustgis_data::GeometryType,
    fields: &[rustgis_data::Field],
    path: &str,
) -> Result<(), String> {
    use rustgis_data::{Dataset, GeometryType, query};
    if l.dataset.geometry_type != GeometryType::None && geometry != l.dataset.geometry_type {
        return Err(format!(
            "{path} holds {} features; {} draws {} features: choose a file of {} features",
            geometry.label(),
            l.name,
            l.dataset.geometry_type.label(),
            l.dataset.geometry_type.label()
        ));
    }
    let mut required: Vec<&str> = l.renderer.field().into_iter().collect();
    required.extend(l.marker_rotation.as_ref().map(|m| m.field.as_str()));
    required.extend(l.extrusion.as_ref().and_then(|e| e.field.as_deref()));
    if let Some(d) = &l.diagram {
        required.extend(d.categories.iter().map(|c| c.field.as_str()));
    }
    required.extend(l.labels.in_order().filter(|c| c.by.text.is_none()).map(|c| c.field.as_str()));
    required.extend(l.time.as_ref().map(|t| t.field.as_str()));
    required.retain(|f| l.dataset.fields.iter().any(|old| old.name.eq_ignore_ascii_case(f)));
    required.sort_unstable();
    required.dedup();
    let missing: Vec<_> = required.into_iter().filter(|f| !fields.iter().any(|new| new.name.eq_ignore_ascii_case(f))).collect();
    if !missing.is_empty() {
        return Err(format!(
            "{path} has no field {}, which {}'s symbology, labels or time slider use: choose a file that has them",
            missing.join(", "),
            l.name
        ));
    }
    let mut numeric: Vec<&str> = Vec::new();
    if matches!(l.renderer, rustgis_map::symbology::Renderer::Graduated { .. } | rustgis_map::symbology::Renderer::Proportional { .. }) {
        numeric.extend(l.renderer.field());
    }
    numeric.extend(l.marker_rotation.as_ref().map(|m| m.field.as_str()));
    numeric.extend(l.extrusion.as_ref().and_then(|e| e.field.as_deref()));
    if let Some(d) = &l.diagram {
        numeric.extend(d.categories.iter().map(|c| c.field.as_str()));
    }
    numeric.retain(|name| l.dataset.fields.iter().any(|f| f.name.eq_ignore_ascii_case(name) && f.ty.is_numeric()));
    numeric.sort_unstable();
    numeric.dedup();
    let changed: Vec<_> =
        numeric.into_iter().filter(|name| fields.iter().any(|f| f.name.eq_ignore_ascii_case(name) && !f.ty.is_numeric())).collect();
    if !changed.is_empty() {
        return Err(format!(
            "{path}: field {} must remain numeric for {}'s symbology, rotation, extrusion or diagrams; choose a source with numeric declarations",
            changed.join(", "),
            l.name
        ));
    }
    let candidate = Dataset::new("", geometry, fields.to_vec());
    let check = |expression: &str| -> Result<(), String> {
        // Existing broken expressions are not newly invalidated by replacing the source.
        if query::parse_fields(expression, &l.dataset).is_ok()
            && let Err(why) = query::parse_fields(expression, &candidate)
        {
            return Err(format!(
                "{path} doesn't fit {}'s configured expression `{expression}` ({why}): choose a file that has its fields",
                l.name
            ));
        }
        Ok(())
    };
    let by = &l.symbol_by;
    for e in [
        l.renderer.expression(),
        l.definition_query.as_deref(),
        by.size.as_deref(),
        by.fill.as_deref(),
        by.stroke.as_deref(),
        by.stroke_width.as_deref(),
        by.angle.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        check(e)?;
    }
    for filter in &l.display_filters {
        check(&filter.where_)?;
    }
    for class in l.labels.in_order() {
        let by = &class.by;
        for e in [class.where_.as_deref(), by.text.as_deref(), by.size.as_deref(), by.color.as_deref(), by.angle.as_deref()]
            .into_iter()
            .flatten()
        {
            check(e)?;
        }
    }
    Ok(())
}

fn repair_tiles(s: &mut Session, p: &Value) -> CmdResult {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (s, p);
        Err("the web app can't link to files on this computer: add the file again with Add Data".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use rustgis_render::rustgis_tiles::{cache, mbtiles, pmtiles};
        let i = super::tiles::find(s, p)?.ok_or("name, index or id is required")?;
        let t = s.map().tiles.get(i).ok_or("the tile layer no longer exists")?;
        if t.archive_file().is_none() {
            return Err(format!("{}: only a tile archive read from a file (PMTiles or MBTiles) can be repaired", t.name));
        }
        let full = replacement(p)?;
        let mut new = t.clone();
        new.url = full.clone();
        new.error = None;
        new.repair()?;
        let vector = match new.kind {
            rustgis_map::TileKind::MbTiles => mbtiles::info(&full)?.format.is_vector(),
            _ => pmtiles::info_blocking(&full, web_time::Duration::from_secs(10))?.tile_type.is_vector(),
        };
        if (!t.layers.is_empty() || !t.style_layers.is_empty()) && !vector {
            return Err(format!(
                "{full} holds raster tiles; {} draws vector tiles with its own layers or style: choose a vector tile archive",
                t.name
            ));
        }
        cache::forget(&t.url);
        let slot = s.map_mut().tiles.get_mut(i).ok_or("the tile layer no longer exists")?;
        *slot = new;
        Ok(json!({"index": i, "path": full}))
    }
}
