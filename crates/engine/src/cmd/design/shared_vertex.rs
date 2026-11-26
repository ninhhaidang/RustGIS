//! Moves every polygon vertex stored at the source coordinate, within one layer.
//! Exact coordinate equality includes point-only contacts; no edge remapping or tolerance.

use rustgis_data::{Geometry, VertexId};
use rustgis_geom::Coord;
use serde_json::{Value, json};

use super::{Change, apply};
use crate::{CmdResult, Session};

/// Moves coincident polygon nodes through one design-rule batch and the caller's transaction.
/// Source addresses match `edit.vertex`; closed rings follow `Geometry::move_vertex`.
/// Same-position calls still run design rules and retain ordinary command history behaviour.
pub(super) fn run(s: &mut Session, p: &Value) -> CmdResult {
    let id = s.layer_id(p)?;
    let row = index(p, "row", None)?;
    let vertex = VertexId::new(index(p, "part", Some(0))?, index(p, "ring", Some(0))?, index(p, "index", None)?);
    let own = super::own_place(p)?;
    let layer = s.map().layer(id).ok_or("no such layer")?;
    // The place in the layer's own coordinates, where its vertices are matched exactly: as given
    // there (`x`, `y`), or from longitude/latitude, a stored place it was snapped to as stored.
    let at = match own {
        Some(at) => at,
        None => {
            let place = Geometry::Point(super::lonlat_place(p)?);
            let held = super::held_places(s, id, &[], &[&place]);
            let Geometry::Point(at) = layer.dataset.edit_from_lonlat(place, &held)? else {
                return Err("the place can't be given in the layer's coordinates".into());
            };
            at
        }
    };
    let source = layer.dataset.features.get(row).ok_or("no such row")?.geometry.as_ref().ok_or("the feature has no shape")?;
    if !matches!(source, Geometry::Polygon(_)) {
        return Err("shared vertices can only be moved in polygons".into());
    }
    let original = source.vertex(vertex).ok_or("no such vertex")?;
    // This clone retains the Arc, not a second row table. The first write can still copy the
    // full table, which the undo snapshot also shares; keep these rows for post-rule counts.
    let before = layer.dataset.features.clone();
    let mut replacements = Vec::new();
    for (row, feature) in before.iter().enumerate() {
        let Some(geometry) = &feature.geometry else { continue };
        let Some(moved) = move_matching(geometry, original, at) else { continue };
        super::super::edit::validate(&moved)?;
        replacements.push((row, moved));
    }
    let layer = s.map_mut().layer_mut(id).ok_or("no such layer")?;
    let mut changes = Vec::new();
    for (row, geometry) in replacements {
        let feature = layer.dataset.features.get_mut(row).ok_or("no such row")?;
        changes.push(Change { before: Some(feature.clone()), row: Some(row) });
        feature.geometry = Some(geometry);
    }
    layer.touch_geometry();
    let rows: Vec<usize> = changes.iter().filter_map(|c| c.row).collect();
    apply(s, id, changes)?;
    let layer = s.map().layer(id).ok_or("no such layer")?;
    let modified = rows.iter().filter(|&&row| before.get(row) != layer.dataset.features.get(row)).count();
    Ok(json!({"part": vertex.part, "ring": vertex.ring, "index": vertex.index, "modified": modified}))
}

/// Copies a matching polygon once and rewrites its original-coordinate matches in linear time.
/// Stored closing duplicates match their first node and move with it, as with `move_vertex`;
/// open rings stay unnormalized. Non-polygons and polygons without a match need no clone.
fn move_matching(geometry: &Geometry, original: Coord, at: Coord) -> Option<Geometry> {
    let Geometry::Polygon(parts) = geometry else { return None };
    if !parts.iter().flatten().flatten().any(|c| *c == original) {
        return None;
    }
    let mut moved = parts.clone();
    for c in moved.iter_mut().flatten().flatten() {
        if *c == original {
            *c = at;
        }
    }
    Some(Geometry::Polygon(moved))
}

/// Reads a platform-sized address; optional null parts/rings use the same defaults as `edit.vertex`.
fn index(p: &Value, key: &str, default: Option<usize>) -> Result<usize, String> {
    match p.get(key) {
        None | Some(Value::Null) if default.is_some() => default.ok_or_else(|| format!("{key} is required")),
        Some(v) => v.as_u64().and_then(|n| usize::try_from(n).ok()).ok_or_else(|| format!("{key} must be a whole number")),
        None => Err(format!("{key} is required")),
    }
}
