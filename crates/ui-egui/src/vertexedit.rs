//! The Edit Vertices map tool: the selected features' vertices drawn as handles, with a midpoint
//! handle on each edge. Drag a vertex to move it, drag a midpoint (or click an edge) to insert
//! one, click a vertex and press Delete to remove it. Shared Vertices opts polygon moves into
//! `edit.moveSharedVertex`; other edits use `edit.vertex`. Each is one command for undo and rules.

use egui::{Color32, Painter, Pos2, Rect, Stroke, vec2};
use rustgis_engine::data::{Feature, Geometry, VertexId};
use rustgis_engine::geom::Coord;
use serde_json::json;

use crate::RustGisApp;
use crate::mapview::{lonlat_to_screen, screen_to_lonlat};

/// How near (points) the pointer must be to a vertex, a midpoint or an edge to take it.
const VERTEX_PX: f32 = 7.0;
const MID_PX: f32 = 6.0;
const EDGE_PX: f32 = 5.0;
/// Most features edited and handles drawn at once (a large selection still draws quickly).
const MAX_FEATURES: usize = 50;
const MAX_HANDLES: usize = 4000;
/// Edges shorter than this (points) get no midpoint handle.
const MID_MIN_PX: f32 = 24.0;

/// A vertex being dragged: the feature, the vertex (for an insertion, the vertex the new one
/// follows) and where the pointer has it now.
#[derive(Clone, Copy, Debug)]
pub struct VertexDrag {
    pub layer: u32,
    pub row: usize,
    pub vertex: VertexId,
    pub insert: bool,
    pub at: Coord,
    /// Whether `at` snapped to a feature (the overlay marks it).
    pub snapped: bool,
}

/// The source chosen by a vertex click, without retaining rows or invalidating ordinary edits.
#[derive(Clone, Copy)]
pub(crate) struct VertexOwner {
    project: u64,
    map: usize,
    layer: u64,
    replacement: u64,
}

impl VertexOwner {
    fn capture(app: &RustGisApp, layer: u32) -> Option<Self> {
        let layer = app.session.map().layer(layer)?;
        Some(Self {
            project: app.session.opened,
            map: app.session.project.active_map,
            layer: layer.instance_id(),
            replacement: layer.replacement_revision(),
        })
    }

    fn matches(self, app: &RustGisApp, layer: u32) -> bool {
        self.project == app.session.opened
            && self.map == app.session.project.active_map
            && app
                .session
                .map()
                .layer(layer)
                .is_some_and(|layer| self.layer == layer.instance_id() && self.replacement == layer.replacement_revision())
    }
}

/// Chooses a vertex together with the live source identity its row belongs to.
pub(crate) fn choose(app: &mut RustGisApp, vertex: Option<(u32, usize, VertexId)>) {
    app.map.vertex_owner = vertex.and_then(|(layer, _, _)| VertexOwner::capture(app, layer));
    app.map.vertex = vertex.filter(|_| app.map.vertex_owner.is_some());
}

/// The source of a held gesture, kept separately from its copyable preview. A weak row identity
/// detects any mutable row access, including edits without `touch_geometry`, without retaining
/// the rows or copying the geometry. A different project, map slot or map name also invalidates
/// it: a duplicated map can share rows and later occupy the original map's slot.
pub(crate) struct DragOwner {
    project: u64,
    map: usize,
    map_name: String,
    rows: std::sync::Weak<Vec<Feature>>,
    shared: bool,
    /// Bounded geographic edges captured once, discarded with this gesture's row identity.
    preview: Option<SharedPreview>,
}

impl DragOwner {
    /// Captures the selected source's allocation, not a global revision: styling or moving the
    /// camera can change the session revision without changing the gesture's source.
    fn capture(app: &RustGisApp, d: VertexDrag) -> Option<Self> {
        let map = app.session.map();
        let layer = map.layer(d.layer)?;
        Some(Self {
            project: app.session.opened,
            map: app.session.project.active_map,
            map_name: map.name.clone(),
            rows: layer.dataset.features.allocation_key(),
            shared: app.map.move_shared_vertices,
            preview: shared_origin(app, d)
                .map(|original| SharedPreview { original, edges: shared_edges(layer.dataset.features.iter(), original) }),
        })
    }

    /// A gesture is usable only in its original context while its source is still an editable
    /// target. Row edits or a changed shared mode cancel; styles and camera changes do not.
    fn matches(&self, app: &RustGisApp, d: VertexDrag) -> bool {
        let map = app.session.map();
        self.project == app.session.opened
            && self.map == app.session.project.active_map
            && self.map_name == map.name
            && self.shared == app.map.move_shared_vertices
            && app.session.tool == rustgis_engine::Tool::EditVertices
            && map.layer(d.layer).is_some_and(|l| self.rows.ptr_eq(&l.dataset.features.allocation_key()))
            && targets(app).iter().any(|(l, r, g)| *l == d.layer && *r == d.row && g.vertex(d.vertex).is_some())
    }
}

/// Drops stale chosen vertices, previews and pending pointer releases before they can act on
/// replacement rows. Also runs each frame and before Delete, including after direct session calls.
pub(crate) fn cancel_stale_drag(app: &mut RustGisApp) {
    if let Some((layer, _, _)) = app.map.vertex
        && !app.map.vertex_owner.is_some_and(|owner| owner.matches(app, layer))
    {
        app.map.cancel_vertex_drag();
        return;
    }
    if app.map.vertex.is_none() {
        app.map.vertex_owner = None;
    }
    let Some(d) = app.map.vertex_drag else {
        app.map.vertex_drag_owner = None;
        return;
    };
    if !app.map.vertex_drag_owner.as_ref().is_some_and(|owner| owner.matches(app, d)) {
        app.map.cancel_vertex_drag();
    }
}

/// What is under the pointer.
#[derive(Clone, Copy, Debug)]
enum Handle {
    Vertex(u32, usize, VertexId, Coord),
    /// An edge's midpoint handle or a point on the edge: the edge's start and the place.
    Edge(u32, usize, VertexId, Coord),
}

/// The features whose vertices the tool edits: the selected features of visible feature layers
/// held in memory (a layer read in place from its file can't be edited), when editing is on.
fn targets(app: &RustGisApp) -> Vec<(u32, usize, &Geometry)> {
    let m = app.session.map();
    if !m.editing.settings.enabled {
        return Vec::new();
    }
    let mut out = Vec::new();
    for l in m.layers.iter().filter(|l| m.shows(l) && !l.selection.is_empty() && !l.is_raster() && !l.graphics && !l.is_streamed()) {
        for r in &l.selection {
            if out.len() >= MAX_FEATURES {
                return out;
            }
            if let Some(g) = l.dataset.features.get(*r).and_then(|f| f.geometry.as_ref()) {
                out.push((l.id, *r, g));
            }
        }
    }
    out
}

/// The screen points of an edge's ends, when both are in view.
fn edge_on_screen(app: &RustGisApp, rect: Rect, a: Coord, b: Coord) -> Option<(Pos2, Pos2)> {
    Some((lonlat_to_screen(app, rect, a)?, lonlat_to_screen(app, rect, b)?))
}

/// The nearest point to `p` on the segment `a`–`b`.
fn nearest_on(a: Pos2, b: Pos2, p: Pos2) -> Pos2 {
    let d = b - a;
    let len2 = d.length_sq();
    let t = if len2 > 0.0 { ((p - a).dot(d) / len2).clamp(0.0, 1.0) } else { 0.0 };
    a + d * t
}

/// The handle under a screen point: a vertex first, then a midpoint, then an edge.
fn handle_at(app: &RustGisApp, rect: Rect, p: Pos2) -> Option<Handle> {
    // Features nowhere near the pointer aren't searched: their extents (in degrees) padded by
    // the reach of a handle around it.
    let near = screen_to_lonlat(app, rect, p).map(|c| {
        let reach = [vec2(1.0, 0.0), vec2(0.0, 1.0), vec2(-1.0, 0.0), vec2(0.0, -1.0)]
            .iter()
            .filter_map(|d| screen_to_lonlat(app, rect, p + *d * (VERTEX_PX + MID_PX)))
            .map(|q| (q.x - c.x).abs().max((q.y - c.y).abs()))
            .fold(0.0_f64, f64::max);
        (c, reach * 2.0)
    });
    let ts: Vec<_> =
        targets(app).into_iter().filter(|(_, _, g)| near.is_none_or(|(c, reach)| g.extent().padded(0.0, reach).contains(c))).collect();
    let mut best: Option<(f32, Handle)> = None;
    for &(layer, row, g) in &ts {
        g.for_each_vertex(|v, c| {
            let Some(s) = lonlat_to_screen(app, rect, c) else { return };
            let d = (s - p).length();
            if d <= VERTEX_PX && best.is_none_or(|b| d < b.0) {
                best = Some((d, Handle::Vertex(layer, row, v, c)));
            }
        });
    }
    if let Some((_, h)) = best {
        return Some(h);
    }
    for &(layer, row, g) in &ts {
        g.for_each_edge(|v, a, b| {
            let Some((sa, sb)) = edge_on_screen(app, rect, a, b) else { return };
            let mid = sa + (sb - sa) / 2.0;
            let (d, at) = if (sb - sa).length() >= MID_MIN_PX && (mid - p).length() <= MID_PX {
                ((mid - p).length() - MID_PX, mid)
            } else {
                let q = nearest_on(sa, sb, p);
                ((q - p).length(), q)
            };
            if d <= EDGE_PX
                && best.is_none_or(|b| d < b.0)
                && let Some(c) = screen_to_lonlat(app, rect, at)
            {
                best = Some((d, Handle::Edge(layer, row, v, c)));
            }
        });
    }
    best.map(|(_, h)| h)
}

/// A press at `origin` starts dragging the vertex or midpoint there, capturing the source rows
/// and map context before previewing. A later source change cancels rather than retargets it.
pub fn drag_start(app: &mut RustGisApp, rect: Rect, origin: Pos2) -> bool {
    let drag = match handle_at(app, rect, origin) {
        Some(Handle::Vertex(layer, row, vertex, at)) => VertexDrag { layer, row, vertex, insert: false, at, snapped: false },
        Some(Handle::Edge(layer, row, vertex, at)) => VertexDrag { layer, row, vertex, insert: true, at, snapped: false },
        None => return false,
    };
    let Some(owner) = DragOwner::capture(app, drag) else { return false };
    app.map.vertex_drag_owner = Some(owner);
    choose(app, (!drag.insert).then_some((drag.layer, drag.row, drag.vertex)));
    app.map.vertex_drag = Some(drag);
    true
}

/// A dragged line/polygon vertex follows the pointer, excluding itself and its incident edges.
/// Point/multipoint moves keep whole-feature exclusion. An insertion also excludes its whole
/// feature, so its source edge cannot become a target. Stale source ownership cancels first,
/// even when this pointer position cannot be projected.
pub fn drag_to(app: &mut RustGisApp, rect: Rect, p: Pos2) {
    cancel_stale_drag(app);
    let Some(c) = screen_to_lonlat(app, rect, p) else { return };
    let Some(d) = app.map.vertex_drag else { return };
    // Asked directly, not as a command: commands mark the map changed, redrawing it every frame.
    let snap = if d.insert {
        rustgis_engine::cmd::design::snap_except(&mut app.session, c, Some((d.layer, d.row)))
    } else {
        rustgis_engine::cmd::design::snap_vertex_except(&mut app.session, c, Some((d.layer, d.row, d.vertex)))
    };
    if let Some(d) = &mut app.map.vertex_drag {
        d.at = snap.map_or(c, |(at, _)| at);
        d.snapped = snap.is_some();
    }
}

/// Letting go commits a still-owned move or insertion as one edit. A queued release after a
/// source/context change is discarded, preserving the current geometry and undo/redo history.
pub fn drag_stop(app: &mut RustGisApp) {
    // Cancellation also drops the pending Vertex pointer action, swallowing a stale release.
    cancel_stale_drag(app);
    let Some(d) = app.map.vertex_drag.take() else { return };
    app.map.vertex_drag_owner = None;
    // A vertex let go where it was is no edit (and no undo step).
    let at_start = targets(app).iter().any(|(l, r, g)| {
        *l == d.layer && *r == d.row && g.vertex(d.vertex).is_some_and(|c| (c.x - d.at.x).abs() < 1e-9 && (c.y - d.at.y).abs() < 1e-9)
    });
    if d.insert || !at_start {
        commit(app, d);
    }
}

/// Sends a polygon move through the shared command only when opted in; insertion and other
/// geometry kinds retain `edit.vertex`. Held drags check ownership; edge clicks commit directly.
fn commit(app: &mut RustGisApp, d: VertexDrag) {
    let action = if d.insert { "insert" } else { "move" };
    let mut p = json!({
        "layer": d.layer, "row": d.row, "action": action,
        "part": d.vertex.part, "ring": d.vertex.ring, "index": d.vertex.index, "lon": d.at.x, "lat": d.at.y,
    });
    let shared = app.map.move_shared_vertices
        && !d.insert
        && app
            .session
            .map()
            .layer(d.layer)
            .and_then(|l| l.dataset.features.get(d.row))
            .and_then(|f| f.geometry.as_ref())
            .is_some_and(|g| matches!(g, Geometry::Polygon(_)));
    let command = if shared {
        if let Some(p) = p.as_object_mut() {
            p.remove("action");
        }
        "edit.moveSharedVertex"
    } else {
        "edit.vertex"
    };
    if let Ok(v) = app.run(command, p) {
        let num = |k: &str| v.get(k).and_then(serde_json::Value::as_u64).map(|n| n as usize);
        if let (Some(part), Some(ring), Some(index)) = (num("part"), num("ring"), num("index")) {
            choose(app, Some((d.layer, d.row, VertexId::new(part, ring, index))));
        }
    }
}

/// A click chooses the vertex under it, inserts a vertex on the edge under it, or else selects
/// the feature there (Shift adds it to the selection).
pub fn click(app: &mut RustGisApp, rect: Rect, p: Pos2, c: Coord, shift: bool) {
    match handle_at(app, rect, p) {
        Some(Handle::Vertex(layer, row, v, _)) => choose(app, Some((layer, row, v))),
        Some(Handle::Edge(layer, row, vertex, at)) => commit(app, VertexDrag { layer, row, vertex, insert: true, at, snapped: false }),
        None => {
            choose(app, None);
            let mode = if shift { "add" } else { "new" };
            let _ = app.run("select.atPoint", json!({"lon": c.x, "lat": c.y, "tolerancePx": 5, "mode": mode}));
        }
    }
}

/// Delete removes the chosen vertex; `false` when none is chosen.
pub fn delete_chosen(app: &mut RustGisApp) -> bool {
    cancel_stale_drag(app);
    let Some((layer, row, v)) = app.map.vertex else { return false };
    // A vertex chosen before an undo, or on a feature no longer selected, is gone: Delete then
    // deletes the selected features as usual.
    if !targets(app).iter().any(|(l, r, g)| *l == layer && *r == row && g.vertex(v).is_some()) {
        choose(app, None);
        return false;
    }
    let p = json!({"layer": layer, "row": row, "action": "delete", "part": v.part, "ring": v.ring, "index": v.index});
    if app.run("edit.vertex", p).is_ok() {
        // The vertex before it is chosen next, so Delete can go on removing.
        choose(app, v.index.checked_sub(1).map(|i| (layer, row, VertexId::new(v.part, v.ring, i))));
    }
    true
}

/// The vertices either side of `v` on its path (a ring's first and last are neighbours).
fn neighbours(g: &Geometry, v: VertexId) -> Vec<Coord> {
    let edges: Vec<(VertexId, Coord, Coord)> = g.edges().into_iter().filter(|e| e.0.part == v.part && e.0.ring == v.ring).collect();
    let n = g.vertices().iter().filter(|(w, _)| w.part == v.part && w.ring == v.ring).count();
    // A ring has as many edges as vertices, its last edge ending at the first vertex.
    let prev = match v.index.checked_sub(1) {
        Some(i) => Some(i),
        None => (edges.len() == n).then(|| n.checked_sub(1)).flatten(),
    };
    let next = edges.iter().find(|e| e.0.index == v.index).map(|e| e.2);
    let prev = prev.and_then(|i| edges.iter().find(|e| e.0.index == i)).map(|e| e.1);
    prev.into_iter().chain(next).collect()
}

/// The original coordinate shared by a polygon move, before any projected preview is drawn.
fn shared_origin(app: &RustGisApp, d: VertexDrag) -> Option<Coord> {
    if !app.map.move_shared_vertices || d.insert {
        return None;
    }
    let g = app.session.map().layer(d.layer)?.dataset.features.get(d.row)?.geometry.as_ref()?;
    matches!(g, Geometry::Polygon(_)).then(|| g.vertex(d.vertex)).flatten()
}

/// Gesture-local original edges, never screen points or retained source rows. Camera changes
/// reproject this bounded overlay; row/context changes discard its owning `DragOwner`.
struct SharedPreview {
    original: Coord,
    edges: Vec<(Coord, Coord)>,
}

/// Collects incident polygon edges once at press, stopping the lazy traversal at the overlay
/// cap even within a large ring. Ring closure and short paths follow `Geometry::for_each_edge`.
/// This cap never supplies or restricts the command's independently collected propagation set.
fn shared_edges<'a>(features: impl Iterator<Item = &'a Feature>, original: Coord) -> Vec<(Coord, Coord)> {
    features
        .filter_map(|f| match &f.geometry {
            Some(Geometry::Polygon(polys)) => Some(polys),
            _ => None,
        })
        .flatten()
        .flatten()
        .flat_map(|ring| {
            let repeated = ring.len() > 1 && ring.first() == ring.last();
            let n = ring.len().saturating_sub(usize::from(repeated));
            let count = if n > 2 { n } else { n.saturating_sub(1) };
            ring.iter().take(n).copied().zip(ring.iter().take(n).copied().cycle().skip(1)).take(count)
        })
        .inspect(|_| {
            #[cfg(test)]
            tests::SCANNED_EDGES.set(tests::SCANNED_EDGES.get() + 1);
        })
        .filter(|(a, b)| *a == original || *b == original)
        .take(MAX_HANDLES)
        .collect()
}

/// Reprojects only the cached incident edges, including unselected neighbours. Each frame's
/// shared overlay work is bounded independently of layer size and never mutates source rows.
fn draw_shared(app: &RustGisApp, p: &Painter, rect: Rect, d: VertexDrag, preview: &SharedPreview, accent: Color32) {
    for &(a, b) in &preview.edges {
        let a = if a == preview.original { d.at } else { a };
        let b = if b == preview.original { d.at } else { b };
        if let Some((a, b)) = edge_on_screen(app, rect, a, b) {
            p.line_segment([a, b], Stroke::new(2.0, accent));
        }
    }
}

/// The pointer's cursor over a handle.
pub fn cursor(app: &RustGisApp, rect: Rect, p: Pos2) -> Option<egui::CursorIcon> {
    if app.map.vertex_drag.is_some() {
        return Some(egui::CursorIcon::Grabbing);
    }
    match handle_at(app, rect, p)? {
        Handle::Vertex(..) => Some(egui::CursorIcon::Grab),
        Handle::Edge(..) => Some(egui::CursorIcon::Copy),
    }
}

/// Draws the handles, the chosen vertex and, mid-drag, the edges the vertex pulls.
pub fn draw(app: &RustGisApp, p: &Painter, rect: Rect, accent: Color32) {
    let green = Color32::from_rgb(0x16, 0x9A, 0x5A);
    let drag = app.map.vertex_drag;
    let shared = drag.and_then(|d| app.map.vertex_drag_owner.as_ref()?.preview.as_ref().map(|preview| (d, preview)));
    if let Some((d, preview)) = shared {
        draw_shared(app, p, rect, d, preview, accent);
    }
    // Handles off the map aren't drawn and don't count towards the cap.
    let shown = rect.expand(8.0);
    let mut handles = 0;
    for (layer, row, g) in targets(app) {
        // The edges a dragged vertex pulls: its neighbours' (or the split edge's ends) to it.
        if let Some(d) = drag.filter(|d| d.layer == layer && d.row == row) {
            let ends = if d.insert {
                g.edges().into_iter().find(|e| e.0 == d.vertex).map(|e| vec![e.1, e.2]).unwrap_or_default()
            } else {
                neighbours(g, d.vertex)
            };
            if let Some(to) = lonlat_to_screen(app, rect, d.at) {
                for e in ends.into_iter().filter_map(|c| lonlat_to_screen(app, rect, c)) {
                    p.line_segment([e, to], Stroke::new(2.0, accent));
                }
                p.circle_filled(to, 5.0, accent);
                p.circle_stroke(to, 5.0, Stroke::new(1.5, Color32::WHITE));
            }
        }
        g.for_each_edge(|v, a, b| {
            if handles >= MAX_HANDLES {
                return;
            }
            let Some((sa, sb)) = edge_on_screen(app, rect, a, b) else { return };
            let mid = sa + (sb - sa) / 2.0;
            if shown.contains(mid)
                && (sb - sa).length() >= MID_MIN_PX
                && !drag.is_some_and(|d| d.insert && d.layer == layer && d.row == row && d.vertex == v)
            {
                p.circle_filled(mid, 3.5, Color32::WHITE.gamma_multiply(0.85));
                p.circle_stroke(mid, 3.5, Stroke::new(1.2, green));
                handles += 1;
            }
        });
        g.for_each_vertex(|v, c| {
            if handles >= MAX_HANDLES {
                return;
            }
            // The dragged vertex is drawn where the pointer has it.
            if drag.is_some_and(|d| !d.insert && d.layer == layer && d.row == row && d.vertex == v) {
                return;
            }
            if shared.is_some_and(|(d, preview)| d.layer == layer && preview.original == c) {
                return;
            }
            let Some(s) = lonlat_to_screen(app, rect, c).filter(|s| shown.contains(*s)) else { return };
            let chosen = app.map.vertex == Some((layer, row, v));
            let size = if chosen { 9.0 } else { 7.0 };
            let r = Rect::from_center_size(s, vec2(size, size));
            p.rect_filled(r, 0.0, if chosen { accent } else { Color32::WHITE });
            p.rect_stroke(r, 0.0, Stroke::new(1.3, if chosen { Color32::WHITE } else { green }), egui::StrokeKind::Middle);
            handles += 1;
        });
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use rustgis_engine::{Project, Session, data::Dataset};

    std::thread_local! {
        /// Deterministic traversal evidence, confined to the current test thread.
        pub(super) static SCANNED_EDGES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    /// A closed square with two edges incident to its first vertex.
    fn square(x: f64) -> Geometry {
        Geometry::Polygon(vec![vec![vec![
            Coord::new(x, 0.),
            Coord::new(x + 1., 0.),
            Coord::new(x + 1., 1.),
            Coord::new(x, 1.),
            Coord::new(x, 0.),
        ]]])
    }

    /// Only the first row is selected; remaining rows must still participate in shared moves.
    fn app(geometries: impl Iterator<Item = Geometry>) -> RustGisApp {
        let mut session = Session::new(Project::new("Shared preview cache"));
        let mut data = Dataset::new("Parcels", rustgis_engine::data::GeometryType::Polygon, vec![]);
        data.features =
            geometries.enumerate().map(|(id, g)| Feature { id: id as u64, geometry: Some(g), attrs: vec![] }).collect::<Vec<_>>().into();
        let id = session.map_mut().add_dataset(data);
        session.active_layer = Some(id);
        session.map_mut().layer_mut(id).unwrap().selection = vec![0];
        session.map_mut().camera.res = 1000.;
        session.map_mut().editing.snapping.enabled = false;
        let mut app = RustGisApp::new(session);
        app.run("tool.set", json!({"tool":"editVertices"})).unwrap();
        app.run("ui.moveSharedVertices", json!({"on":true})).unwrap();
        app.session.clear_history();
        app
    }

    /// A held gesture scans once, retains no rows, reprojects after camera changes, and drops
    /// its cache when any row changes. Restarting captures fresh edges under the new identity.
    #[test]
    fn shared_preview_reuses_edges_and_invalidates_with_owner() {
        let mut app = app(std::iter::repeat_with(|| square(0.)).take(2).chain(std::iter::repeat_with(|| square(20.)).take(500)));
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(1000., 800.));
        SCANNED_EDGES.set(0);
        let from = lonlat_to_screen(&app, rect, Coord::new(0., 0.)).unwrap();
        assert!(drag_start(&mut app, rect, from));
        assert_eq!(SCANNED_EDGES.get(), 2008);
        let edges = &app.map.vertex_drag_owner.as_ref().unwrap().preview.as_ref().unwrap().edges;
        assert_eq!(edges.len(), 4);
        let allocation = edges.as_ptr();
        let ctx = egui::Context::default();
        for _ in 0..8 {
            app.session.map_mut().camera.center.x += 10.;
            cancel_stale_drag(&mut app);
            let to = lonlat_to_screen(&app, rect, Coord::new(0.2, 0.3)).unwrap();
            drag_to(&mut app, rect, to);
            let mut output = ctx.run_ui(Default::default(), |ui| {
                draw(&app, &ui.ctx().layer_painter(egui::LayerId::background()), rect, Color32::RED);
            });
            output.textures_delta.clear();
            assert_eq!(SCANNED_EDGES.get(), 2008, "no layer scan on movement or redraw");
            assert_eq!(app.map.vertex_drag_owner.as_ref().unwrap().preview.as_ref().unwrap().edges.as_ptr(), allocation);
        }
        app.session.map_mut().layers[0].dataset.features[1].id = 999;
        cancel_stale_drag(&mut app);
        assert!(app.map.vertex_drag_owner.is_none());
        assert!(app.map.vertex_drag.is_none());
        let from = lonlat_to_screen(&app, rect, Coord::new(0., 0.)).unwrap();
        assert!(drag_start(&mut app, rect, from));
        assert_eq!(SCANNED_EDGES.get(), 4016, "new gesture scans under the new row identity");
        assert_eq!(app.session.undo_depth(), 0);
    }

    /// The overlay stops at its cap without visiting the remaining layer, whereas release
    /// still moves every coincident row, including rows omitted from that overlay, in one undo.
    #[test]
    fn shared_preview_cap_does_not_limit_command_propagation() {
        let mut app = app(std::iter::repeat_with(|| square(0.)).take(2101));
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(1000., 800.));
        SCANNED_EDGES.set(0);
        let from = lonlat_to_screen(&app, rect, Coord::new(0., 0.)).unwrap();
        assert!(drag_start(&mut app, rect, from));
        assert_eq!(app.map.vertex_drag_owner.as_ref().unwrap().preview.as_ref().unwrap().edges.len(), MAX_HANDLES);
        assert_eq!(SCANNED_EDGES.get(), 8000, "stop before the remaining 101 rows");
        let to = lonlat_to_screen(&app, rect, Coord::new(0.2, 0.3)).unwrap();
        drag_to(&mut app, rect, to);
        let at = app.map.vertex_drag.unwrap().at;
        drag_stop(&mut app);
        assert!(app.map.vertex_drag_owner.is_none());
        assert_eq!(app.session.undo_depth(), 1);
        for f in app.session.map().layers[0].dataset.features.iter() {
            assert_eq!(f.geometry.as_ref().unwrap(), &square(0.).move_vertex(VertexId::new(0, 0, 0), at).unwrap());
        }
        app.run("edit.undo", json!(null)).unwrap();
        assert!(app.session.map().layers[0].dataset.features.iter().all(|f| f.geometry.as_ref() == Some(&square(0.))));
    }

    /// Lazy polygon traversal matches the engine's closure convention, including short and
    /// unclosed rings, holes, multiple parts, repeated nodes and exact rather than near matches.
    #[test]
    fn shared_preview_edges_match_geometry_contract() {
        let o = Coord::new(0., 0.);
        let a = Coord::new(1., 0.);
        let b = Coord::new(0., 1.);
        let g = Geometry::Polygon(vec![
            vec![vec![], vec![o], vec![o, a], vec![o, a, o], vec![o, a, b], vec![o, a, b, o]],
            vec![vec![o, a, o, b, o], vec![Coord::new(f64::EPSILON, 0.), a, b]],
        ]);
        let expected: Vec<_> = g.edges().into_iter().filter(|(_, a, b)| *a == o || *b == o).map(|(_, a, b)| (a, b)).collect();
        let features = [
            Feature { id: 0, geometry: Some(g), attrs: vec![] },
            Feature { id: 1, geometry: Some(Geometry::Line(vec![vec![o, a]])), attrs: vec![] },
        ];
        assert_eq!(shared_edges(features.iter(), o), expected);
        let dense = Feature { id: 2, geometry: Some(Geometry::Polygon(vec![vec![vec![o; MAX_HANDLES * 2]]])), attrs: vec![] };
        SCANNED_EDGES.set(0);
        assert_eq!(shared_edges(std::iter::once(&dense), o).len(), MAX_HANDLES);
        assert_eq!(SCANNED_EDGES.get(), MAX_HANDLES, "also stop inside one large ring");
    }
}
