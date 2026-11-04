//! Desktop editing of one existing local GeoPackage feature table, saved atomically.

#[cfg(not(target_arch = "wasm32"))]
use super::{f, ps};
use crate::{CmdResult, CommandSpec as C, Session};
#[cfg(not(target_arch = "wasm32"))]
use rustgis_data::source::gpkg::edit::{EditTable, MAX_EDITABLE};
#[cfg(not(target_arch = "wasm32"))]
use rustgis_map::source::versions_of;
use serde_json::{Value, json};

pub fn commands() -> Vec<C> {
    vec![
        C::new("gpkg.addLayer", "Edit GeoPackage Table", "Home › Add › Add Data", add_layer).params(
            r#"{"path": "a GeoPackage file on this computer", "table"?: "its feature table (needed when it has several)", "limit"?: "the most rows (default and most: 100000)", "name"?: string}"#,
        ),
        C::new("gpkg.pendingEdits", "GeoPackage Edits", "Edit › Manage Edits", pending_edits).view()
            .params(r#"{"layer"?: "a layer added with gpkg.addLayer (default: the active one)"}"#),
        C::new("gpkg.saveEdits", "Save to GeoPackage", "Edit › Manage Edits", save_edits)
            .params(r#"{"layer"?: "a layer added with gpkg.addLayer (default: the active one)"}"#),
        C::new("gpkg.discardEdits", "Discard GeoPackage Edits", "Edit › Manage Edits", discard_edits)
            .params(r#"{"layer"?: "a layer added with gpkg.addLayer (default: the active one)"}"#),
    ]
}

fn gpkg_layer(s: &Session, p: &Value) -> Result<(u32, String, rustgis_map::source::GpkgEdit), String> {
    let id = s.layer_id(p)?;
    let l = s.map().layer(id).ok_or("no such layer")?;
    let edit = l
        .gpkg
        .clone()
        .ok_or_else(|| format!("{}: not a GeoPackage layer loaded for editing (Home › Add Data › Edit GeoPackage Table…)", l.name))?;
    Ok((id, l.name.clone(), edit))
}

fn pending_edits(s: &mut Session, p: &Value) -> CmdResult {
    let (id, name, edit) = gpkg_layer(s, p)?;
    let l = s.map().layer(id).ok_or("no such layer")?;
    let c = edit.versions.changes(&l.dataset.features);
    Ok(json!({"layer": name, "file": edit.path, "table": edit.table,
        "inserts": c.inserts.len(), "updates": c.updates.len(), "deletes": c.deletes.len(), "writable": edit.writable}))
}

/// Adds one feature table of a GeoPackage as an editable layer, framed on a map that shows nothing
/// yet.
fn add_layer(s: &mut Session, p: &Value) -> CmdResult {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (s, p);
        Err("GeoPackage tables are edited in the desktop app".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use std::path::Path;
        let path = ps(p, "path").map(str::trim).filter(|v| !v.is_empty()).ok_or("path is required")?;
        let path = Path::new(path);
        if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("gpkg")) {
            return Err(format!("{}: not a GeoPackage (.gpkg)", path.display()));
        }
        if !path.is_file() {
            return Err(format!("{}: no such file", path.display()));
        }
        let path = std::path::absolute(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let file = path.file_name().unwrap_or_default().to_string_lossy();
        let err = |e: rustgis_data::DataError| format!("{file}: {e}");
        let table = match ps(p, "table").map(str::trim).filter(|t| !t.is_empty()) {
            Some(t) => t.to_string(),
            None => {
                let tables = rustgis_data::source::layers(&path).map_err(err)?;
                if tables.len() != 1 {
                    return Err(format!("{file}: {} feature tables ({}); choose one with table", tables.len(), tables.join(", ")));
                }
                tables.first().cloned().ok_or_else(|| format!("{file}: no feature tables"))?
            }
        };
        let src = EditTable::open(&path, &table).map_err(err)?;
        let limit = f(p, "limit").map_or(MAX_EDITABLE, |n| n.clamp(1.0, MAX_EDITABLE as f64) as usize);
        let editable = src.read_editable(limit).map_err(err)?;
        let name = ps(p, "name").map(str::trim).filter(|v| !v.is_empty()).unwrap_or(src.table()).to_string();
        let info = src.info();
        let mut ds = rustgis_data::Dataset::new(name.clone(), info.geometry_type, info.fields.clone());
        ds.crs = src.crs();
        ds.zm_dropped = info.zm_dropped;
        let path = path.to_string_lossy().into_owned();
        ds.source = Some(path.clone());
        let n = editable.features.len();
        let read = editable.versions.iter().copied().collect();
        let versions = versions_of(&editable.features, &Default::default(), &read);
        ds.features = editable.features.into();
        for note in &info.notes {
            s.notify(format!("{name}: {note}"), false);
        }
        if (n as u64) < editable.matched {
            s.notify(
                format!("{name}: loaded {n} of {} rows; the others aren't edited, and saving leaves them as they are", editable.matched),
                false,
            );
        }
        if editable.skipped > 0 {
            s.notify(
                format!(
                    "{name}: {} rows with a geometry RustGIS can't read weren't loaded; saving leaves them as they are",
                    editable.skipped
                ),
                false,
            );
        }
        if !editable.writable {
            s.notify(format!("{name}: the file is read-only, so edits can't be saved to it"), false);
        }
        let empty_before = super::map_is_empty(s);
        let id = super::layer::add_dataset(s, ds);
        if let Some(l) = s.map_mut().layer_mut(id) {
            let (geometry_type, z, m) = src.declaration();
            l.gpkg = Some(rustgis_map::source::GpkgEdit {
                path,
                table: src.table().to_string(),
                key: src.key().to_string(),
                geometry: src.geometry_column().to_string(),
                srs_id: src.srs_id(),
                geometry_type: geometry_type.to_string(),
                z,
                m,
                revision: editable.revision,
                writable: editable.writable,
                limit,
                versions,
            });
        }
        if empty_before && let Some(e) = s.map().layer(id).map(|l| l.extent()).filter(|e| !e.is_empty()) {
            s.zoom_to(&e);
        }
        Ok(json!({"layer": id, "features": n, "matched": editable.matched, "skipped": editable.skipped, "writable": editable.writable}))
    }
}

fn save_edits(s: &mut Session, p: &Value) -> CmdResult {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (s, p);
        Err("GeoPackage tables are edited in the desktop app".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use std::path::Path;
        let (id, name, edit) = gpkg_layer(s, p)?;
        let l = s.map().layer(id).ok_or("no such layer")?;
        let changes = edit.versions.changes(&l.dataset.features);
        let fields = l.dataset.fields.clone();
        if changes.is_empty() {
            s.notify(format!("{name}: no edits to save"), false);
            return Ok(json!({"saved": false, "inserted": 0, "updated": 0, "deleted": 0}));
        }
        let table = &edit.table;
        if s.saved_over.iter().any(|v| v.shares(&edit.versions)) {
            return Err(format!(
                "{name}: the layer is back before an edit saved to {table} (undone), so nothing was saved. Discard Edits loads the rows as they are now"
            ));
        }
        let path = Path::new(&edit.path);
        if !path.is_file() {
            return Err(format!("{name}: {} isn't there any more, so nothing was saved", edit.path));
        }
        if std::fs::metadata(path).is_ok_and(|m| m.permissions().readonly()) {
            return Err(format!("{name}: {} is read-only, so nothing was saved", edit.path));
        }
        let err = |e: rustgis_data::DataError| format!("{name}: nothing was saved to {table}: {e}");
        let src = EditTable::open(path, table).map_err(err)?;
        // `src` is opened just now, so its declaration is compared with the one loaded; the save
        // transaction then compares `src` with the table again before writing.
        let (geometry_type, z, m) = src.declaration();
        if !src.key().eq_ignore_ascii_case(&edit.key)
            || !src.geometry_column().eq_ignore_ascii_case(&edit.geometry)
            || src.srs_id() != edit.srs_id
            || !geometry_type.eq_ignore_ascii_case(&edit.geometry_type)
            || z != edit.z
            || m != edit.m
        {
            return Err(format!("{name}: {table} changed shape since it was loaded, so nothing was saved. Discard Edits loads it again"));
        }
        let rows = changes.insert_rows;
        let edits = rustgis_data::source::edits::Edits { updates: changes.updates, deletes: changes.deletes, inserts: changes.inserts };
        let saved_rows = match src.save(&edits, &fields, edit.versions.iter().map(|(key, _)| key), &edit.revision).map_err(err)? {
            Ok(saved) => saved,
            Err(conflicts) => {
                let mut said: Vec<String> = conflicts.iter().take(20).map(ToString::to_string).collect();
                if conflicts.len() > 20 {
                    said.push(format!("{} more", conflicts.len() - 20));
                }
                return Err(format!(
                    "{name}: nothing was saved to {table} in {}, {} conflicts: {}. Discard Edits loads the rows as they are now",
                    edit.path,
                    conflicts.len(),
                    said.join("; ")
                ));
            }
        };
        let saved = saved_rows.changes;
        let mut stored: std::collections::HashMap<_, _> = saved_rows.features.into_iter().map(|f| (f.id, f)).collect();
        let versions = saved.versions.iter().copied().collect();
        if let Some(l) = s.map_mut().layer_mut(id) {
            for (row, (_, key)) in rows.iter().zip(&saved.inserted) {
                if let Some(f) = l.dataset.features.get_mut(*row) {
                    f.id = *key;
                }
            }
            let mut geometry_changed = false;
            if !saved_rows.removed.is_empty() {
                let selected: std::collections::HashSet<_> =
                    l.selection.iter().filter_map(|&row| l.dataset.features.get(row).map(|f| f.id)).collect();
                let before = l.dataset.features.len();
                l.dataset.features.retain(|f| saved_rows.removed.binary_search(&f.id).is_err());
                if l.dataset.features.len() != before {
                    geometry_changed = true;
                    l.selection =
                        l.dataset.features.iter().enumerate().filter_map(|(row, f)| selected.contains(&f.id).then_some(row)).collect();
                }
            }
            for feature in l.dataset.features.iter_mut() {
                if let Some(mut row) = stored.remove(&feature.id) {
                    for &i in &saved_rows.unstored_fields {
                        if let (Some(stored), Some(local)) = (row.attrs.get_mut(i), feature.attrs.get_mut(i)) {
                            std::mem::swap(stored, local);
                        }
                    }
                    geometry_changed |= row.geometry != feature.geometry;
                    *feature = row;
                }
            }
            for (key, _) in &edits.deletes {
                if let Some(row) = stored.remove(key) {
                    l.dataset.features.push(row);
                    geometry_changed = true;
                }
            }
            if geometry_changed {
                l.touch_geometry();
            }
            let baseline = versions_of(&l.dataset.features, &edit.versions, &versions);
            if let Some(g) = &mut l.gpkg {
                g.versions = baseline;
                g.revision = saved_rows.revision;
            }
        }
        s.saved_over.retain(rustgis_map::source::Versions::held_elsewhere);
        s.saved_over.push(edit.versions);
        let canonical = std::fs::canonicalize(path).ok();
        for map in &mut s.project.maps {
            for l in map.layers.iter_mut() {
                if let Some(source) = &mut l.source
                    && source.table.as_deref().is_none_or(|t| t.eq_ignore_ascii_case(table))
                    && source.path.as_deref().is_some_and(|p| match (canonical.as_ref(), std::fs::canonicalize(p).ok()) {
                        (Some(a), Some(b)) => *a == b,
                        _ => p == edit.path,
                    })
                {
                    source.reset();
                }
            }
        }
        s.notify(
            format!(
                "{name}: saved to {table} in {}: {} new, {} changed, {} deleted",
                edit.path,
                saved.inserted.len(),
                saved.updated,
                saved.deleted
            ),
            false,
        );
        if !saved.skipped_fields.is_empty() {
            s.notify(format!("{name}: {table} has no column for {}, so it wasn't saved", saved.skipped_fields.join(", ")), false);
        }
        Ok(json!({"saved": true, "inserted": saved.inserted.len(), "updated": saved.updated, "deleted": saved.deleted,
            "unchanged": saved.unchanged, "keys": saved.inserted.iter().map(|(_, k)| k.cast_signed()).collect::<Vec<_>>(), "skippedFields": saved.skipped_fields}))
    }
}

fn discard_edits(s: &mut Session, p: &Value) -> CmdResult {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (s, p);
        Err("GeoPackage tables are edited in the desktop app".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let (id, name, edit) = gpkg_layer(s, p)?;
        let err = |e: rustgis_data::DataError| format!("{name}: {e}");
        let src = EditTable::open(std::path::Path::new(&edit.path), &edit.table).map_err(err)?;
        let editable = src.read_editable(if edit.limit == 0 { MAX_EDITABLE } else { edit.limit }).map_err(err)?;
        let read = editable.versions.iter().copied().collect();
        let n = editable.features.len();
        let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
        let geometry_type = src.info().geometry_type;
        if l.dataset.geometry_type != geometry_type {
            l.renderer = rustgis_map::Renderer::Single(rustgis_map::Symbol::default_for(geometry_type, id));
        }
        l.dataset.geometry_type = geometry_type;
        l.dataset.crs = src.crs();
        l.dataset.fields = src.info().fields.clone();
        l.dataset.features = editable.features.into();
        l.selection.clear();
        l.touch_geometry();
        let baseline = versions_of(&l.dataset.features, &Default::default(), &read);
        if let Some(g) = &mut l.gpkg {
            g.versions = baseline;
            g.writable = editable.writable;
            g.key = src.key().to_string();
            g.geometry = src.geometry_column().to_string();
            g.srs_id = src.srs_id();
            let (geometry_type, z, m) = src.declaration();
            g.geometry_type = geometry_type.to_string();
            g.z = z;
            g.m = m;
            g.revision = editable.revision;
        }
        s.notify(format!("{name}: edits discarded; {n} rows loaded again from {}", edit.table), false);
        Ok(json!({"features": n, "matched": editable.matched}))
    }
}
