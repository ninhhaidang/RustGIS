//! A layer kept in the project and the file it was read from: whether its edits and the file are
//! in step, and writing its edits back to the file (#1385).

use crate::{CmdResult, CommandSpec as C, Session};
use serde_json::Value;

/// File inspection, and publishing with an explicit edit effect but no automatic undo step:
/// undo cannot reverse the external write, so its committed sync record follows the file.
pub fn commands() -> Vec<C> {
    vec![
        C::new("source.syncState", "Source File State", "Data › Data Source", sync_state).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files)).view().params(
            r#"{"layer"?: "name|id (default: the active layer): one read whole from a Shapefile or .geojson file"} — whether it has edits its file lacks ("edited"), the file changed since it was read ("sourceChanged"), both ("both") or neither ("inSync"), and which of the file's parts changed"#,
        ),
        C::new("source.publish", "Publish to Source", "Data › Data Source", |s, p| super::exports::write_here(s, p, "source.publish")).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files).with(crate::policy::Effect::Edits)).view().params(
            r#"{"layer"?: "name|id (default: the active layer): one read whole from a Shapefile or .geojson file"} — writes the layer's edits back over its source; refused, with nothing written, when the source changed or a rewrite would lose data, metadata or number text. Shapefiles keep their code page, column layout and .prj; GeoJSON keeps its document kind, collection name and feature ids"#,
        ),
    ]
}

/// The layer's record of the file it was read from and that file's path, or why it has none
/// (it wasn't read whole from a Shapefile or `.geojson` file, or its record was lost), or why
/// the file isn't opened (it isn't on this computer).
fn record(l: &rustgis_map::Layer) -> Result<(&rustgis_data::sync::Synced, &str), String> {
    let (Some(synced), Some(path)) = (&l.synced, &l.dataset.source) else {
        if l.dataset.source.as_deref().is_some_and(rustgis_data::sync::is_syncable) && !l.is_streamed() {
            return Err(format!(
                "{} has no record of its file as it was read (it was added before RustGIS kept one, a part of the file couldn't be read, or an older RustGIS saved the project): add the file again to compare it",
                l.name
            ));
        }
        return Err(format!("{} wasn't read whole from a Shapefile or .geojson file, so there's no file to compare it with", l.name));
    };
    // A project from elsewhere names the path: only a local file of a kind a record is made for
    // is opened, never a network share (opening one can send the user's login to its host).
    if !local_file(path) {
        return Err(format!("{}: {path} isn't a Shapefile or .geojson file on this computer, so it isn't checked", l.name));
    }
    Ok((synced, path.as_str()))
}

/// `source.publish`'s work, its inputs taken from the session ([`super::exports`]): writes layer
/// `p` names back over its source ([`rustgis_data::publish::shapefile`] or
/// [`rustgis_data::publish::geojson`], which describe the fidelity checks), when it has edits
/// the file lacks. Its work grows with the
/// file (hashed, read, written twice in memory and read back): the desktop app runs it on a
/// thread of its own with progress and Cancel. Once written, the layer holds its rows as the file
/// reads them (the same, but for what a Shapefile can't tell apart: an empty text and no value),
/// and its record is of the files written and those rows, so it reads `inSync`; the project then
/// has a change to save.
pub(crate) fn publish_work(s: &Session, p: &Value) -> Result<super::exports::Work, String> {
    let id = s.layer_id(p)?;
    let l = s.map().layer(id).ok_or("the layer no longer exists")?;
    let (synced, path) = record(l)?;
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (synced, path);
        Err("the web app can't write files on this computer".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use super::exports::{Then, Wrote};
        let (synced, path, dataset, instance, name) =
            (synced.clone(), path.to_string(), l.dataset.clone(), l.instance_id(), l.name.clone());
        Ok(Box::new(move || {
            let rows = rustgis_data::sync::rows_digest(&dataset).map_err(|e| e.to_string())?;
            let features = dataset.features.len();
            if rows == synced.rows {
                let changed = synced.files.changed(std::path::Path::new(&path)).map_err(|e| e.to_string())?;
                if !changed.is_empty() {
                    return Err(format!(
                        "{path}: {} changed since {name} was read, and {name} has no edits to write: add the file again to read it as it is",
                        changed.join(", ")
                    ));
                }
                return Ok(Wrote {
                    answer: serde_json::json!({"layer": id, "path": path, "published": false, "features": features}),
                    notes: vec![format!("{name} has no edits its file lacks, so {path} wasn't written")],
                    written_path: None,
                    then: None,
                    owner: None,
                    crs_used: Vec::new(),
                });
            }
            let publish =
                if path.to_ascii_lowercase().ends_with(".shp") { rustgis_data::publish::shapefile } else { rustgis_data::publish::geojson };
            let published = publish(&dataset, std::path::Path::new(&path), &synced).map_err(|e| e.to_string())?;
            // The layer as the file now reads it, its features keeping their ids: what the layer
            // holds once published, and its record (the file reads so next time too).
            let mut held = dataset.clone();
            let ids: Vec<u64> = dataset.features.iter().map(|f| f.id).collect();
            held.features = published
                .read
                .features
                .iter()
                .zip(&ids)
                .map(|(f, id)| rustgis_data::Feature { id: *id, ..f.clone() })
                .collect::<Vec<_>>()
                .into();
            held.fields = published.read.fields.clone();
            for field in &mut held.fields {
                if let Some(was) = dataset.fields.iter().find(|was| was.name == field.name) {
                    field.alias = was.alias.clone();
                }
            }
            held.crs = published.read.crs;
            // Already hashed before the files were placed: Cancel must not leave a successful
            // write without its sync record. Feature ids do not contribute to the digest.
            let read = published.rows;
            // Unchanged when the file reads as the layer is.
            let held = (read != rows).then_some(held);
            let new = rustgis_data::sync::Synced { files: published.files, rows: read };
            let (said, written_path) = (path.clone(), Some(path.clone()));
            let then: Then = Box::new(move |s: &mut Session, _: &mut Value| {
                s.remember_source_write(instance, said.clone(), new.clone());
                // The record follows the file: the layer read from it takes the record of what
                // was written, unless it was read again or its record changed meanwhile.
                let live = s.project.maps.iter_mut().flat_map(|m| m.layers.iter_mut()).find(|l| l.instance_id() == instance);
                match live {
                    Some(l) if l.synced.as_ref() == Some(&synced) && l.dataset.source.as_deref() == Some(said.as_str()) => {
                        l.synced = Some(new);
                        // Not edited meanwhile: it takes what the file holds (an empty text as no
                        // value, whole numbers as integers). Edited, it keeps its edits, which
                        // the file then lacks.
                        if let Some(held) = held
                            && l.dataset.features.shares(&dataset.features)
                            && l.dataset.fields == dataset.fields
                            && l.dataset.crs == dataset.crs
                        {
                            l.dataset.features = held.features;
                            l.dataset.fields = held.fields;
                            l.dataset.crs = held.crs;
                            l.touch_geometry();
                            s.revision = s.revision.wrapping_add(1);
                        }
                        s.mark_unsaved();
                    }
                    _ => s.warn(format!("{said} was written, but {name} changed while it was: add the file again to compare them")),
                }
                s.notify(format!("Published {name} to {said}"), false);
                Ok(())
            });
            Ok(Wrote {
                answer: serde_json::json!({"layer": id, "path": path, "published": true, "features": published.features}),
                notes: published.notes,
                written_path,
                then: Some(then),
                owner: None,
                crs_used: Vec::new(),
            })
        }))
    }
}

/// The key a check of layer `id`'s files made ahead is taken by (see [`super::reads`]).
#[cfg(not(target_arch = "wasm32"))]
fn ahead_key(id: u32) -> String {
    format!("syncState:{id}")
}

/// A check of a layer made ahead: the record, path and rows checked (the rows shared with the
/// layer, not copied), the rows' digest and what changed in the files.
#[cfg(not(target_arch = "wasm32"))]
struct Checked {
    synced: rustgis_data::sync::Synced,
    path: String,
    dataset: rustgis_data::Dataset,
    rows: Result<u64, String>,
    changed: Result<Vec<String>, String>,
}

#[cfg(not(target_arch = "wasm32"))]
impl Checked {
    /// Whether it checked layer `l` as it is: the same record and path, and the same rows (the
    /// same allocation: an edit copies them), fields and coordinate system.
    fn of(&self, l: &rustgis_map::Layer) -> bool {
        l.synced.as_ref() == Some(&self.synced)
            && l.dataset.source.as_deref() == Some(self.path.as_str())
            && l.dataset.features.shares(&self.dataset.features)
            && l.dataset.fields == self.dataset.fields
            && l.dataset.crs == self.dataset.crs
    }
}

/// The check `source.syncState` makes before it runs in the desktop app: every part of the
/// file read and hashed, and the layer's rows hashed, on a thread of its own with progress and
/// Cancel.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn sync_ahead(s: &Session, p: &Value) -> Option<super::reads::ReadWork> {
    let id = s.layer_id(p).ok()?;
    let l = s.map().layer(id)?;
    let (synced, path, dataset) = (l.synced.clone()?, l.dataset.source.clone().filter(|p| local_file(p))?, l.dataset.clone());
    Some(super::reads::ahead("Source File State", ahead_key(id), move || {
        let changed = synced.files.changed(std::path::Path::new(&path)).map_err(|e| e.to_string());
        let rows = rustgis_data::sync::rows_digest(&dataset).map_err(|e| e.to_string());
        Checked { synced, path, dataset, rows, changed }
    }))
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn sync_ahead(_: &Session, _: &Value) -> Option<super::reads::ReadWork> {
    None
}

/// Compares the layer and the file it was read from, each with the record of them made when it
/// was read: whether the layer's rows differ from the rows read (edits the file lacks), and which
/// parts of the file differ from the parts read. It doesn't compare the layer with the file as
/// it is now, so the same edit made to both shows as `both`. Each part of the size recorded is
/// read and hashed, and the rows are hashed, so the work grows with the file. The desktop app
/// does it on a thread of its own first ([`sync_ahead`]), with progress and **Cancel**, and the
/// command takes that check as it lands; when the layer changed meanwhile it says so rather than
/// check again here, which would hold the window. The assistant, scripts and the command line
/// check where they ask.
fn sync_state(s: &mut Session, p: &Value) -> CmdResult {
    let id = s.layer_id(p)?;
    let l = s.map().layer(id).ok_or("the layer no longer exists")?;
    let (synced, path) = record(l)?;
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (synced, path);
        Err("the web app can't read files on this computer".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let (was, files, path) = (synced.rows, synced.files.clone(), path.to_string());
        let ahead = super::reads::take::<Checked>(s, &ahead_key(id));
        let l = s.map().layer(id).ok_or("the layer no longer exists")?;
        let (rows, changed) = match ahead {
            Some(checked) if checked.of(l) => (checked.rows?, checked.changed?),
            // Checking again here would hold the window: the check made on its thread is dropped.
            Some(_) => return Err("the layer or its file's record changed while it was checked: check again".into()),
            None => (
                rustgis_data::sync::rows_digest(&l.dataset).map_err(|e| e.to_string())?,
                files.changed(std::path::Path::new(&path)).map_err(|e| e.to_string())?,
            ),
        };
        let edited = rows != was;
        let state = match (edited, !changed.is_empty()) {
            (false, false) => "inSync",
            (true, false) => "edited",
            (false, true) => "sourceChanged",
            (true, true) => "both",
        };
        Ok(serde_json::json!({"layer": id, "path": path, "state": state, "edited": edited, "changed": changed}))
    }
}

/// Whether `path` names a file on this computer a record is made for: a Shapefile or `.geojson`
/// file, not an address, a database or a network share (`\\host\share`, `//host/share`,
/// `/\host\share`, `\\?\UNC\host\share`). Windows' long form of a local path (`\\?\C:\…`) is one
/// on this computer. The desktop's Layers pane marks the same layers this publishes.
pub fn local_file(path: &str) -> bool {
    let b = rustgis_map::paths::local_path(path).as_bytes();
    let sep = |i: usize| b.get(i).is_some_and(|c| matches!(c, b'/' | b'\\'));
    // Windows takes any two leading separators, of either kind, for a network share, unless they
    // begin the long form of a path on a drive.
    let drive = b.get(2) == Some(&b'?') && sep(3) && b.get(4).is_some_and(u8::is_ascii_alphabetic) && b.get(5) == Some(&b':') && sep(6);
    let share = sep(0) && sep(1) && !drive;
    rustgis_map::paths::is_local(path) && rustgis_data::sync::is_syncable(path) && !share
}
