//! Editing a shape already on the map by touch, and snapping: the Capture sheet's Edit mode.
//!
//! As for capturing, the map is moved under the cross, which a finger does more exactly than a
//! tap: the feature under the cross is picked, then a vertex near it, and that vertex is moved to
//! where the cross is next, deleted, or followed by a new one there. With snapping on, the cross's
//! place is snapped to the map's features first (the map's own snapping settings), for capturing
//! and editing alike.

use egui::{Color32, Stroke};
use rustgis_ui_egui::rustgis_engine::data::{Coord, Geometry, GeometryType, VertexId};
use rustgis_ui_egui::theme::Tokens;
use rustgis_ui_egui::{RustGisApp, i18n, tl};
use serde_json::json;

/// How far from the cross, in points, a vertex or a feature is still under it: half a finger.
pub(crate) const REACH: f32 = super::TARGET / 2.0;

/// The feature being edited: by its id, which stays the same while rows come and go, and the
/// vertex picked on it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Editing {
    pub feature: u64,
    pub vertex: Option<VertexId>,
    /// Where the picked vertex was when it was picked or last changed here: one found elsewhere
    /// (an Undo moved or took it away, or its index now names another) is let go.
    pub at: Option<(f64, f64)>,
}

impl Editing {
    /// The feature `feature`, with no vertex picked.
    pub fn feature(feature: u64) -> Editing {
        Editing { feature, vertex: None, at: None }
    }

    /// The same feature with vertex `v` of `shape` picked (none when `shape` has no such vertex).
    pub fn picking(self, v: Option<VertexId>, shape: Option<&Geometry>) -> Editing {
        let at = v.and_then(|v| shape?.vertex(v)).map(|c| (c.x, c.y));
        Editing { vertex: v.filter(|_| at.is_some()), at, ..self }
    }

    /// Lets the picked vertex go when `shape` has it elsewhere than it was, or not at all.
    pub fn checked(self, shape: Option<&Geometry>) -> Editing {
        match self.vertex {
            Some(v) if shape.and_then(|g| g.vertex(v)).map(|c| (c.x, c.y)) == self.at => self,
            Some(_) => Editing { vertex: None, at: None, ..self },
            None => self,
        }
    }
}

/// The place the cross snapped to, as longitude and latitude, and what it snapped to (`vertex`,
/// `edge`, `end`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Snapped {
    pub lon: f64,
    pub lat: f64,
    pub kind: &'static str,
}

/// The row of layer `id` holding the feature `feature`, if it is still there.
pub(crate) fn row_of(app: &RustGisApp, id: u32, feature: u64) -> Option<usize> {
    app.session.map().layer(id)?.dataset.features.iter().position(|f| f.id == feature)
}

/// The shape of the feature in row `row` of layer `id`, in longitude/latitude, as edits give it.
pub(crate) fn shape(app: &RustGisApp, id: u32, row: usize) -> Option<Geometry> {
    shape_ref(app, id, row).map(std::borrow::Cow::into_owned)
}

/// [`shape`], borrowed while the layer's coordinates are longitude/latitude (as every layer's are
/// today): what is drawn and checked each frame isn't copied for it. Converted from the layer's
/// own coordinates otherwise; `None` when it can't be.
pub(crate) fn shape_ref(app: &RustGisApp, id: u32, row: usize) -> Option<std::borrow::Cow<'_, Geometry>> {
    let l = app.session.map().layer(id)?;
    l.dataset.geometry_to_lonlat(l.dataset.features.get(row)?.geometry.as_ref()?).ok()
}

/// Whether putting vertex `v` of `shape` at `at` (`move`), or a new one there after it
/// (`insert`), would leave two vertices in one place in a row: on the screen, within a point of
/// each other. The edge between them would have no length, which the capture draft refuses too.
pub(crate) fn doubles(app: &RustGisApp, shape: &Geometry, v: VertexId, action: &str, at: (f64, f64)) -> bool {
    let edges = shape.edges();
    let ring = matches!(shape, Geometry::Polygon(_));
    let mine = || edges.iter().filter(|e| e.0.part == v.part && e.0.ring == v.ring);
    let count = mine().count();
    let after = mine().find(|e| e.0 == v).map(|e| e.2);
    let before = mine().find(|e| e.0.index + 1 == v.index || (ring && v.index == 0 && e.0.index + 1 == count)).map(|e| e.1);
    let near = match action {
        "move" => [before, after],
        "insert" => [shape.vertex(v), after],
        _ => return false,
    };
    let Some(cross) = app.lonlat_to_screen(at.0, at.1) else { return false };
    near.into_iter().flatten().filter_map(|c| app.lonlat_to_screen(c.x, c.y)).any(|p| p.distance(cross) < 1.0)
}

/// The feature of layer `id` drawn on top under the cross (within [`REACH`]), by its id.
pub(crate) fn feature_at_cross(app: &mut RustGisApp, id: u32) -> Option<u64> {
    let map = app.session.map();
    let (at, tolerance) = (map.camera.center, f64::from(REACH) * map.camera.res);
    let scale = app.session.scale();
    let (layer, row) = app.session.map_mut().hit(at, tolerance, scale, |l| l.id == id)?;
    app.session.map().layer(layer)?.dataset.features.get(row).map(|f| f.id)
}

/// The vertex of `shape` nearest the screen point `cross`, within [`REACH`].
pub(crate) fn vertex_near(app: &RustGisApp, shape: &Geometry, cross: egui::Pos2) -> Option<VertexId> {
    shape
        .vertices()
        .into_iter()
        .filter_map(|(v, c)| app.lonlat_to_screen(c.x, c.y).map(|p| (v, p.distance(cross))))
        .filter(|(_, d)| *d <= REACH)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(v, _)| v)
}

/// Where the cross's place snaps to with the map's snapping on: `None` with it off, or with
/// nothing in reach. A vertex being moved (`moving`: layer, row and vertex) doesn't snap to itself
/// or the edges it ends, but does to the rest of its shape.
pub(crate) fn snap(app: &mut RustGisApp, at: (f64, f64), moving: Option<(u32, usize, VertexId)>) -> Option<Snapped> {
    use rustgis_ui_egui::rustgis_engine::cmd::design::{snap_at, snap_vertex_except};
    if !app.session.map().editing.snapping.enabled {
        return None;
    }
    let c = Coord::new(at.0, at.1);
    let (to, kind) = match moving {
        Some(skip) => snap_vertex_except(&mut app.session, c, Some(skip)),
        None => snap_at(&mut app.session, c),
    }?;
    Some(Snapped { lon: to.x, lat: to.y, kind })
}

/// What the cross snapped to, said for the sheet.
pub(crate) fn snapped_to(kind: &str) -> &'static str {
    match kind {
        "vertex" => tl!("snapped to a vertex"),
        "end" => tl!("snapped to a line's end"),
        _ => tl!("snapped to an edge"),
    }
}

/// Turns editing on for the map when it is off, and says so. Says whether it is on.
pub(crate) fn ensure_editing(app: &mut RustGisApp) -> bool {
    if app.session.map().editing.settings.enabled {
        return true;
    }
    if app.run_by_user("edit.enable", json!({"on": true})).is_err() {
        return false;
    }
    app.session.notify(tl!("Editing was turned on for this map."), false);
    true
}

/// Runs `edit.vertex` on the vertex `v` of row `row` of layer `id`: `move` it to `at`, `insert`
/// one at `at` after it, or `delete` it. The vertex picked afterwards: the one moved, the one
/// inserted, or none once deleted. `None` when it was refused (the front end says why).
pub(crate) fn vertex(
    app: &mut RustGisApp,
    id: u32,
    row: usize,
    v: VertexId,
    action: &str,
    at: Option<(f64, f64)>,
) -> Option<Option<VertexId>> {
    if !ensure_editing(app) {
        return None;
    }
    let mut params = json!({"layer": id, "row": row, "part": v.part, "ring": v.ring, "index": v.index, "action": action});
    if let Some((lon, lat)) = at {
        params["lon"] = json!(lon);
        params["lat"] = json!(lat);
    }
    let done = app.run_by_user("edit.vertex", params).ok()?;
    if action == "delete" {
        return Some(None);
    }
    let read = |k: &str| done.get(k).and_then(serde_json::Value::as_u64).and_then(|n| usize::try_from(n).ok());
    Some(Some(VertexId::new(read("part")?, read("ring")?, read("index")?)))
}

/// Moves the point in row `row` of layer `id` to `at`. A multipoint isn't moved, and that is said:
/// its points can't be moved one at a time by touch, and moving its first would leave the rest.
/// Says whether it moved.
pub(crate) fn move_point(app: &mut RustGisApp, id: u32, row: usize, at: (f64, f64)) -> bool {
    match shape(app, id, row) {
        Some(Geometry::Point(_)) => vertex(app, id, row, VertexId::new(0, 0, 0), "move", Some(at)).is_some(),
        _ => {
            app.session.notify(tl!("Nothing was moved: only a single point moves here, not a multipoint's points."), true);
            false
        }
    }
}

/// Deletes the feature in row `row` of layer `id`, as one edit that undoes. Says whether it was.
/// The map's selections are set aside while it is (Delete takes every layer's selection) and
/// put back after, the rows of layer `id` past it moved up one.
pub(crate) fn delete_feature(app: &mut RustGisApp, id: u32, row: usize) -> bool {
    if !ensure_editing(app) {
        return false;
    }
    let kept: Vec<(u32, Vec<usize>)> = app.session.map_mut().layers.iter_mut().map(|l| (l.id, std::mem::take(&mut l.selection))).collect();
    let deleted = app.run_by_user("select.rows", json!({"layer": id, "rows": [row]})).is_ok()
        && app.run_by_user("edit.delete", serde_json::Value::Null).is_ok();
    for (layer, mut rows) in kept {
        if layer == id && deleted {
            rows.retain(|r| *r != row);
            rows.iter_mut().filter(|r| **r > row).for_each(|r| *r -= 1);
        }
        if let Some(l) = app.session.map_mut().layer_mut(layer) {
            l.selection = rows;
        }
    }
    deleted
}

/// Draws the feature being edited over the map: every vertex, the picked one larger and filled.
pub(crate) fn draw(app: &RustGisApp, painter: &egui::Painter, shape: &Geometry, picked: Option<VertexId>, t: &Tokens) {
    if matches!(shape.kind(), GeometryType::Polyline | GeometryType::Polygon) {
        for (_, a, b) in shape.edges() {
            if let (Some(a), Some(b)) = (app.lonlat_to_screen(a.x, a.y), app.lonlat_to_screen(b.x, b.y)) {
                painter.line_segment([a, b], Stroke::new(2.0, t.accent));
            }
        }
    }
    for (v, c) in shape.vertices() {
        let Some(at) = app.lonlat_to_screen(c.x, c.y) else { continue };
        if Some(v) == picked {
            painter.circle(at, 9.0, t.accent, Stroke::new(2.0, Color32::WHITE));
        } else {
            painter.circle(at, 5.5, Color32::WHITE, Stroke::new(2.0, t.accent));
        }
    }
}

/// Draws where the cross snapped to: a ring there, joined to the cross.
pub(crate) fn draw_snap(app: &RustGisApp, painter: &egui::Painter, snapped: Snapped, cross: egui::Pos2, t: &Tokens) {
    let Some(at) = app.lonlat_to_screen(snapped.lon, snapped.lat) else { return };
    painter.line_segment([cross, at], Stroke::new(1.0, t.accent));
    painter.circle_stroke(at, 10.0, Stroke::new(3.0, Color32::WHITE));
    painter.circle_stroke(at, 10.0, Stroke::new(2.0, t.green));
}

/// The text the sheet shows for the feature being edited.
pub(crate) fn status(kind: GeometryType, shape: &Geometry, picked: Option<VertexId>) -> String {
    if kind == GeometryType::Point {
        return tl!("Move the map to where the point goes, then Move Point Here.").to_string();
    }
    let vertices = shape.vertices();
    match picked.and_then(|v| vertices.iter().position(|(id, _)| *id == v)) {
        Some(n) => i18n::fmt(
            "Vertex {n} of {all} picked: move the map to where it goes, then Move Here. Add After puts a new vertex at the cross after it.",
            &[("n", &(n + 1).to_string()), ("all", &vertices.len().to_string())],
        ),
        None => tl!("Put the cross on a vertex and pick it.").to_string(),
    }
}
