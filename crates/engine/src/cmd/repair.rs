//! Undoable replacement of linked local data without replacing the layer.

use crate::{CmdResult, CommandSpec as C, Session};
use serde_json::Value;
#[cfg(not(target_arch = "wasm32"))]
use serde_json::json;

pub fn commands() -> Vec<C> {
    vec![
        C::new("layer.repairSource", "Repair Data Source", "Data › Data Source", repair_source).params(
            r#"{"layer"?: name|id (default: the active layer), "path": "the file or folder (a Zarr store, a File Geodatabase) the layer's data moved to", "table"?: "the dataset in a GeoPackage, GDAL or DuckDB file, or a raster's subdataset (default: the one the layer reads now)"}"#),
        C::new("map.repairTiles", "Repair Tile Archive", "Layers", repair_tiles).params(
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
        if let Some(rl) = &l.raster {
            let new = if rl.time_zarr.is_some() { super::zarr::relink_following(s, rl, &full)? } else { rl.relinked(&full, table)? };
            let l = s.map_mut().layer_mut(id).ok_or("the layer no longer exists")?;
            l.raster = Some(new);
            l.selection.clear();
            l.touch_geometry();
        } else {
            let src = l.source.as_ref().ok_or("the layer has no linked source")?;
            let new = src.relinked(&full, table)?;
            if let Some(info) = new.info() {
                if l.dataset.geometry_type != rustgis_data::GeometryType::None && info.geometry_type != l.dataset.geometry_type {
                    return Err(format!(
                        "{full} holds {} features; {} draws {} features: choose a file of {} features",
                        info.geometry_type.label(),
                        l.name,
                        l.dataset.geometry_type.label(),
                        l.dataset.geometry_type.label()
                    ));
                }
                let mut required: Vec<&str> = l.renderer.field().into_iter().collect();
                if l.labels.enabled {
                    required.push(&l.labels.default.field);
                    required.extend(l.labels.classes.iter().map(|c| c.field.as_str()));
                }
                if let Some(time) = &l.time {
                    required.push(&time.field);
                }
                required.retain(|f| l.dataset.fields.iter().any(|old| old.name.eq_ignore_ascii_case(f)));
                required.sort_unstable();
                required.dedup();
                let missing: Vec<&str> =
                    required.into_iter().filter(|f| !info.fields.iter().any(|new| new.name.eq_ignore_ascii_case(f))).collect();
                if !missing.is_empty() {
                    return Err(format!(
                        "{full} has no field {}, which {}'s symbology, labels or time slider use: choose a file that has them",
                        missing.join(", "),
                        l.name
                    ));
                }
                // Classes by an expression that reads on the old fields must read on the new ones.
                if let Some(e) = l.renderer.expression().filter(|e| rustgis_data::query::parse(e, &l.dataset).is_ok())
                    && let Err(why) =
                        rustgis_data::query::parse(e, &rustgis_data::Dataset::new("", info.geometry_type, info.fields.clone()))
                {
                    return Err(format!(
                        "{full} doesn't fit {}'s symbology, which classes by `{e}` ({why}): choose a file that has its fields",
                        l.name
                    ));
                }
            }
            s.map_mut().layer_mut(id).ok_or("the layer no longer exists")?.replace_source(new);
        }
        Ok(json!({"layer": id, "path": full, "table": s.map().layer(id).and_then(|l| l.linked_table())}))
    }
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
