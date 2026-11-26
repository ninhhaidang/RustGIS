//! The Edit Vertices map tool: the selected features' vertices drawn as handles, with a midpoint
//! handle on each edge. Drag a vertex to move it, drag a midpoint (or click an edge) to insert
//! one, click a vertex and press Delete to remove it. Shared Vertices opts polygon moves into
//! `edit.moveSharedVertex`; other edits use `edit.vertex`. Each is one command for undo and rules.

use std::borrow::Cow;

use egui::{Color32, Painter, Pos2, Rect, Stroke, vec2};
use rustgis_engine::data::{Dataset, Feature, Geometry, VertexId};
use rustgis_engine::geom::Coord;
use serde_json::json;

use crate::RustGisApp;
use crate::mapview::{lonlat_to_screen, screen_to_lonlat};
use rustgis_engine::map::Projection;
use rustgis_engine::map::projection::Flat;

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
/// follows) and where the pointer has it now, in [view coordinates](view_to_screen).
#[derive(Clone, Copy, Debug)]
pub struct VertexDrag {
    pub layer: u32,
    pub row: usize,
    pub vertex: VertexId,
    pub insert: bool,
    pub at: Coord,
    /// Whether `at` snapped to a feature (the overlay marks it).
    pub snapped: bool,
    /// The longitude/latitude it snapped to, as snapping gave it: committed as it is, so the
    /// place snapped to is kept exactly, not after a round trip through the map's coordinates.
    pub snapped_lonlat: Option<Coord>,
}

impl VertexDrag {
    /// The vertex's place in longitude/latitude: where it snapped, as snapping gave it (what
    /// letting go writes), else the pointer's place ([`VertexDrag::at`], in view coordinates) in
    /// longitude/latitude. On a map in the layer's own system an unsnapped release writes `at` in
    /// the layer's coordinates instead. `None` where the map's coordinates have none.
    pub fn lonlat(&self, app: &RustGisApp) -> Option<Coord> {
        self.snapped_lonlat.or_else(|| view_to_lonlat(app, self.at))
    }
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
    /// The map's projection: a drag's place is in its view coordinates ([`VertexDrag::at`]).
    projection: Projection,
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
            projection: map.projection.clone(),
            rows: layer.dataset.features.allocation_key(),
            shared: app.map.move_shared_vertices,
            preview: shared_origin(app, d).and_then(|original| {
                // Matched in the layer's own coordinates, where shared vertices are equal, and
                // drawn in view coordinates, as the handles are.
                let flat = map.flat();
                let placing = Placing::of(&flat, map.projection == Projection::Globe, layer)?;
                let view = |c: Coord| placing.point(c);
                let edges = shared_edges(layer.dataset.features.iter(), original);
                Some(SharedPreview {
                    original: view(original)?,
                    edges: edges.into_iter().filter_map(|(a, b)| Some((view(a)?, view(b)?))).collect(),
                })
            }),
        })
    }

    /// A gesture is usable only in its original context while its source is still an editable
    /// target. Row edits, a changed projection or shared mode cancel; styles and camera changes do not.
    fn matches(&self, app: &RustGisApp, d: VertexDrag) -> bool {
        let map = app.session.map();
        self.project == app.session.opened
            && self.map == app.session.project.active_map
            && self.map_name == map.name
            && self.projection == map.projection
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

/// How a layer's coordinates are put where its handles are drawn and dragged, in view
/// coordinates ([`view_to_screen`]): into the map's coordinates as the map draws the layer
/// ([`Flat::held`]: as they are on a map in the layer's own system, so the handles are on the
/// drawn line exactly), or into longitude/latitude on the globe.
enum Placing<'a> {
    Map(rustgis_engine::map::projection::HeldDrawing<'a>),
    Globe(rustgis_engine::data::raster::Reprojection),
}

impl<'a> Placing<'a> {
    /// `None` for a layer whose system can't be converted at all.
    fn of(flat: &'a Flat, globe: bool, l: &rustgis_engine::map::Layer) -> Option<Self> {
        if globe {
            return l.dataset.shown_conversion().ok().map(Placing::Globe);
        }
        flat.held(l.dataset.coord_crs()).ok().map(Placing::Map)
    }

    /// Whether the layer's own coordinates are the view's (a map in its own system).
    fn is_identity(&self) -> bool {
        match self {
            Placing::Map(h) => h.is_identity(),
            Placing::Globe(to) => to.is_identity(),
        }
    }

    /// `g` in view coordinates (borrowed as it is when they are the layer's own); `None` when a
    /// place of it can't be drawn there ([`unplaced_targets`]).
    fn geometry<'g>(&self, g: &'g Geometry) -> Option<Cow<'g, Geometry>> {
        if self.is_identity() {
            return Some(Cow::Borrowed(g));
        }
        match self {
            Placing::Map(h) => match h.geometry(g) {
                (_, true) => None,
                (g, false) => Some(Cow::Owned(g)),
            },
            Placing::Globe(to) => Dataset::converted_to_lonlat(to, g).map(|g| Cow::Owned(g.into_owned())),
        }
    }

    /// One place in view coordinates.
    fn point(&self, c: Coord) -> Option<Coord> {
        let point = Geometry::Point(c);
        let g = self.geometry(&point)?;
        match g.as_ref() {
            Geometry::Point(p) if p.is_finite() => Some(*p),
            _ => None,
        }
    }
}

/// Whether handles are in longitude/latitude (the globe) rather than map coordinates.
fn on_globe(app: &RustGisApp) -> bool {
    app.session.map().projection == Projection::Globe
}

/// The screen point of a place in view coordinates: the map's coordinates, or longitude/latitude
/// on the globe (`None` off the view or behind the globe).
pub(crate) fn view_to_screen(app: &RustGisApp, rect: Rect, c: Coord) -> Option<Pos2> {
    if on_globe(app) {
        return lonlat_to_screen(app, rect, c);
    }
    if !c.is_finite() {
        return None;
    }
    let (x, y) = app.session.map().camera.try_to_screen(c, rect.width() as f64, rect.height() as f64)?;
    Some(egui::pos2(rect.min.x + x as f32, rect.min.y + y as f32))
}

/// The place in view coordinates under a screen point ([`view_to_screen`]).
fn screen_to_view(app: &RustGisApp, rect: Rect, p: Pos2) -> Option<Coord> {
    if on_globe(app) {
        return screen_to_lonlat(app, rect, p);
    }
    let (x, y) = ((p.x - rect.min.x) as f64, (p.y - rect.min.y) as f64);
    app.session.map().camera.try_to_map(x, y, rect.width() as f64, rect.height() as f64)
}

/// A longitude/latitude (a snapped place) in view coordinates.
fn lonlat_to_view(app: &RustGisApp, c: Coord) -> Option<Coord> {
    if on_globe(app) { Some(c) } else { app.session.map().to_map(c) }
}

/// A place in view coordinates in longitude/latitude.
pub(crate) fn view_to_lonlat(app: &RustGisApp, c: Coord) -> Option<Coord> {
    if on_globe(app) { Some(c) } else { app.session.map().to_lonlat(c) }
}

/// Whether layer `id`'s handles are in its own coordinates ([`Placing::is_identity`]): a map in
/// its own system, where an edit is written as placed.
fn own_placing(app: &RustGisApp, id: u32) -> bool {
    let m = app.session.map();
    let flat = m.flat();
    !on_globe(app) && m.layer(id).and_then(|l| Placing::of(&flat, false, l)).is_some_and(|p| p.is_identity())
}

/// The features whose vertices the tool edits: the selected features of visible feature layers
/// held in memory (a layer read in place from its file can't be edited), when editing is on, in
/// view coordinates ([`Placing`]).
fn targets(app: &RustGisApp) -> Vec<(u32, usize, Cow<'_, Geometry>)> {
    let m = app.session.map();
    if !m.editing.settings.enabled {
        return Vec::new();
    }
    let (flat, globe) = (m.flat(), on_globe(app));
    let mut out = Vec::new();
    for l in m.layers.iter().filter(|l| m.shows(l) && !l.selection.is_empty() && !l.is_raster() && !l.graphics && !l.is_streamed()) {
        let Some(placing) = Placing::of(&flat, globe, l) else { continue };
        for r in &l.selection {
            if out.len() >= MAX_FEATURES {
                return out;
            }
            if let Some(g) = l.dataset.features.get(*r).and_then(|f| f.geometry.as_ref())
                && let Some(g) = placing.geometry(g)
            {
                out.push((l.id, *r, g));
            }
        }
    }
    out
}

/// The most selected features [`say_unplaced`] looks for with no place in longitude/latitude: past
/// it, the notice says "at least" so many, and the scan stops.
const MAX_UNPLACED: usize = 1000;

/// The selected features [`targets`] leaves out, by layer and row, while Edit Vertices is on:
/// those with a place that can't be given in longitude/latitude (#2032 C2f-4b), which the map
/// draws them through. On a map in the layer's own system such a feature is drawn as it is, so it
/// gets its handles and isn't counted (#2032 C2f-7).
/// Up to [`MAX_UNPLACED`] of them.
struct Unplaced {
    rows: Vec<(u32, usize)>,
    /// There are more than [`MAX_UNPLACED`].
    more: bool,
    /// How many of each layer's selected rows the scan went through, by live layer: past them,
    /// nothing it says depends on the selection ([`unplaced_signature`]).
    visited: Vec<(u64, usize)>,
}

fn unplaced_targets(app: &RustGisApp) -> Unplaced {
    let m = app.session.map();
    // The features `targets` looks at, in its order, until it has its cap of placed ones: a
    // feature past that gets no handles either way, so isn't counted. Each layer's conversion is
    // made once. A row without geometry counts towards neither bound.
    let mut found = Unplaced { rows: Vec::new(), more: false, visited: Vec::new() };
    let mut placed = 0usize;
    let (flat, globe) = (m.flat(), on_globe(app));
    for l in m.layers.iter().filter(|l| m.shows(l) && !l.selection.is_empty() && !l.is_raster() && !l.graphics && !l.is_streamed()) {
        let placing = Placing::of(&flat, globe, l);
        let mut visited = 0;
        for r in &l.selection {
            if placed >= MAX_FEATURES {
                break;
            }
            visited += 1;
            if let Some(g) = l.dataset.features.geometry(*r) {
                match placing.as_ref().and_then(|p| p.geometry(g)) {
                    Some(_) => placed += 1,
                    None if found.rows.len() >= MAX_UNPLACED => {
                        found.more = true;
                        break;
                    }
                    None => found.rows.push((l.id, *r)),
                }
            }
        }
        found.visited.push((l.instance_id(), visited));
        if placed >= MAX_FEATURES || found.more {
            break;
        }
    }
    found
}

/// What [`unplaced_targets`] depends on: the map and each layer it looks at (the live layer, its
/// replacement and geometry revisions and rows' allocation, which any mutable row access renews,
/// its system, the map system it is shown through, and its selection: its length and the rows the
/// last scan went through, `visited`, so a large selection isn't hashed whole each frame). While
/// those are unchanged, the scan would go through the same rows and stop where it did, so the
/// selection isn't converted again.
fn unplaced_signature(app: &RustGisApp) -> u64 {
    use std::hash::{Hash, Hasher};
    let m = app.session.map();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    app.session.project.active_map.hash(&mut h);
    m.projection.hash(&mut h);
    for l in m.layers.iter().filter(|l| m.shows(l) && !l.selection.is_empty() && !l.is_raster() && !l.graphics && !l.is_streamed()) {
        let rows = l.dataset.features.allocation_key().as_ptr() as usize;
        let revisions = (l.instance_id(), l.replacement_revision(), l.geometry_rev, rows);
        let visited = app.map.vertex_unplaced_visited.iter().find(|(id, _)| *id == l.instance_id()).map_or(0, |(_, n)| *n);
        let reached = l.selection.get(..l.selection.len().min(visited)).unwrap_or_default();
        (revisions, l.dataset.coord_crs(), l.dataset.shown_in, l.selection.len(), reached).hash(&mut h);
    }
    h.finish()
}

/// The notice for a feature whose vertices can't be edited, as `layer` holds it in `system`: one of
/// its places can't be given in longitude/latitude (#2032 C2f-4b). The desktop's Edit Vertices and
/// the phone's shape editing say it alike.
pub fn unplaced_feature_notice(layer: &str, id: u64, system: &str) -> String {
    crate::i18n::fmt(
        "{layer}: feature {id} has a place in {system} that can't be given in longitude/latitude, so its vertices can't be edited there",
        &[("layer", layer), ("id", &id.to_string()), ("system", system)],
    )
}

/// Says once per selection which selected features Edit Vertices gives no handles to, in one
/// notice: one is named, several counted ([`unplaced_targets`]). Nothing is looked at while the
/// tool or editing is off.
pub(crate) fn say_unplaced(app: &mut RustGisApp) {
    if app.session.tool != rustgis_engine::Tool::EditVertices || !app.session.map().editing.settings.enabled {
        // Coming back to the tool says the selection again.
        app.map.vertex_unplaced_said.clear();
        app.map.vertex_unplaced_more = false;
        app.map.vertex_unplaced_seen = None;
        app.map.vertex_unplaced_visited.clear();
        return;
    }
    let signature = unplaced_signature(app);
    if app.map.vertex_unplaced_seen == Some(signature) {
        return;
    }
    app.map.vertex_unplaced_seen = Some(signature);
    // The map's index and the live layer go with the features: another map's layers, or a layer
    // added in a removed one's place, may have the same ids.
    let map = app.session.project.active_map;
    let Unplaced { rows: found, more, visited } = unplaced_targets(app);
    app.map.vertex_unplaced_visited = visited;
    let m = app.session.map();
    let live = |id: u32| m.layer(id).map_or(0, |l| l.instance_id());
    let left: Vec<(usize, u64, usize)> = found.iter().map(|(l, r)| (map, live(*l), *r)).collect();
    // Whether there were more goes with them: past the bound, the count turns to "at least".
    if (&left, more) == (&app.map.vertex_unplaced_said, app.map.vertex_unplaced_more) {
        return;
    }
    let text = match (found.as_slice(), more) {
        ([], _) => None,
        ([(layer, r)], false) => m
            .layer(*layer)
            .and_then(|l| Some(unplaced_feature_notice(&l.name, l.dataset.features.row(*r)?.id(), &l.dataset.coord_crs().label()))),
        (several, more) => {
            let mut layers: Vec<&str> = Vec::new();
            for (id, _) in several {
                if let Some(l) = m.layer(*id).filter(|l| !layers.contains(&l.name.as_str())) {
                    layers.push(&l.name);
                }
            }
            let count = several.len().to_string();
            let args = [("layers", layers.join(", ")), ("count", count)];
            let args: Vec<(&str, &str)> = args.iter().map(|(k, v)| (*k, v.as_str())).collect();
            Some(if more {
                crate::i18n::fmt(
                    "{layers}: at least {count} of the selected features have a place that can't be given in longitude/latitude, so their vertices can't be edited there",
                    &args,
                )
            } else {
                crate::i18n::fmt(
                    "{layers}: {count} of the selected features have a place that can't be given in longitude/latitude, so their vertices can't be edited there",
                    &args,
                )
            })
        }
    };
    app.map.vertex_unplaced_said = left;
    app.map.vertex_unplaced_more = more;
    if let Some(text) = text {
        app.session.notify(text, true);
    }
}

/// The screen points of an edge's ends, when both are in view.
fn edge_on_screen(app: &RustGisApp, rect: Rect, a: Coord, b: Coord) -> Option<(Pos2, Pos2)> {
    Some((view_to_screen(app, rect, a)?, view_to_screen(app, rect, b)?))
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
    // Features nowhere near the pointer aren't searched: their extents (in view coordinates)
    // padded by the reach of a handle around it.
    let near = screen_to_view(app, rect, p).map(|c| {
        let reach = [vec2(1.0, 0.0), vec2(0.0, 1.0), vec2(-1.0, 0.0), vec2(0.0, -1.0)]
            .iter()
            .filter_map(|d| screen_to_view(app, rect, p + *d * (VERTEX_PX + MID_PX)))
            .map(|q| (q.x - c.x).abs().max((q.y - c.y).abs()))
            .fold(0.0_f64, f64::max);
        (c, reach * 2.0)
    });
    let ts: Vec<_> =
        targets(app).into_iter().filter(|(_, _, g)| near.is_none_or(|(c, reach)| g.extent().padded(0.0, reach).contains(c))).collect();
    let mut best: Option<(f32, Handle)> = None;
    for (layer, row, g) in &ts {
        let (layer, row) = (*layer, *row);
        g.for_each_vertex(|v, c| {
            let Some(s) = view_to_screen(app, rect, c) else { return };
            let d = (s - p).length();
            if d <= VERTEX_PX && best.is_none_or(|b| d < b.0) {
                best = Some((d, Handle::Vertex(layer, row, v, c)));
            }
        });
    }
    if let Some((_, h)) = best {
        return Some(h);
    }
    for (layer, row, g) in &ts {
        let (layer, row) = (*layer, *row);
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
                && let Some(c) = screen_to_view(app, rect, at)
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
        Some(Handle::Vertex(layer, row, vertex, at)) => {
            VertexDrag { layer, row, vertex, insert: false, at, snapped: false, snapped_lonlat: None }
        }
        Some(Handle::Edge(layer, row, vertex, at)) => {
            VertexDrag { layer, row, vertex, insert: true, at, snapped: false, snapped_lonlat: None }
        }
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
    let Some(c) = screen_to_view(app, rect, p) else { return };
    let Some(d) = app.map.vertex_drag else { return };
    // Snapping looks in longitude/latitude. Asked directly, not as a command: commands mark the
    // map changed, redrawing it every frame.
    let snap = view_to_lonlat(app, c).and_then(|lonlat| {
        let at = if d.insert {
            rustgis_engine::cmd::design::snap_except(&mut app.session, lonlat, Some((d.layer, d.row)))
        } else {
            rustgis_engine::cmd::design::snap_vertex_except(&mut app.session, lonlat, Some((d.layer, d.row, d.vertex)))
        }?
        .0;
        Some((lonlat_to_view(app, at)?, at))
    });
    if let Some(d) = &mut app.map.vertex_drag {
        d.at = snap.map_or(c, |(view, _)| view);
        d.snapped = snap.is_some();
        d.snapped_lonlat = snap.map(|(_, lonlat)| lonlat);
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
        "part": d.vertex.part, "ring": d.vertex.ring, "index": d.vertex.index,
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
    // On a map in the layer's own system, a vertex placed by the pointer is written where it is,
    // in the layer's coordinates. A snapped one goes by longitude/latitude, so the place snapped
    // to is kept as stored (`held_places`).
    let own = d.snapped_lonlat.is_none() && own_placing(app, d.layer);
    let place = if own { Some(("x", "y", d.at)) } else { d.lonlat(app).map(|c| ("lon", "lat", c)) };
    let Some((kx, ky, at)) = place else {
        // Let go where the map's coordinates have no longitude/latitude: said, not dropped.
        app.session.notify(crate::tl!("The vertex can't be placed there: that place has no longitude/latitude"), true);
        return;
    };
    if let Some(o) = p.as_object_mut() {
        o.insert(kx.into(), json!(at.x));
        o.insert(ky.into(), json!(at.y));
    }
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
        Some(Handle::Edge(layer, row, vertex, at)) => {
            commit(app, VertexDrag { layer, row, vertex, insert: true, at, snapped: false, snapped_lonlat: None })
        }
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
                neighbours(&g, d.vertex)
            };
            if let Some(to) = view_to_screen(app, rect, d.at) {
                for e in ends.into_iter().filter_map(|c| view_to_screen(app, rect, c)) {
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
            let Some(s) = view_to_screen(app, rect, c).filter(|s| shown.contains(*s)) else { return };
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

    /// A selected feature with a place that can't be given in longitude/latitude gets no handles,
    /// and that is said once for the selection, naming it (#2032 C2f-4b); several are counted.
    #[test]
    fn a_selected_feature_with_no_place_in_longitude_latitude_is_said_once() {
        let mut app = app([square(0.), square(2.), square(4.)].into_iter());
        let id = app.session.active_layer.unwrap();
        let utm = rustgis_engine::data::raster::Crs::from_epsg(32631).unwrap();
        app.session.map_mut().editing.settings.enabled = true;
        app.session.map_mut().layer_mut(id).unwrap().dataset.hold_in(utm).unwrap();
        // Placed features: nothing to say.
        let mark = app.session.notice_mark();
        say_unplaced(&mut app);
        assert!(app.session.notices_since(mark).is_empty());
        assert_eq!(targets(&app).len(), 1);
        let layer = app.session.map_mut().layer_mut(id).unwrap();
        let far =
            |x: f64| Geometry::Polygon(vec![vec![vec![Coord::new(x, 0.), Coord::new(1e9, 0.), Coord::new(x, 1.), Coord::new(x, 0.)]]]);
        layer.dataset.features[0].geometry = Some(far(500_000.));
        layer.touch_geometry();
        let mark = app.session.notice_mark();
        say_unplaced(&mut app);
        say_unplaced(&mut app);
        let said: Vec<String> = app.session.notices_since(mark).iter().map(|n| n.text.clone()).collect();
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(said[0].contains("feature 0 has a place") && said[0].contains("can't be edited there"), "{said:?}");
        assert!(targets(&app).is_empty());
        // Two of them, selected: counted, once.
        let layer = app.session.map_mut().layer_mut(id).unwrap();
        layer.dataset.features[1].geometry = Some(far(500_100.));
        layer.touch_geometry();
        layer.selection = vec![0, 1, 2];
        let mark = app.session.notice_mark();
        say_unplaced(&mut app);
        say_unplaced(&mut app);
        let said: Vec<String> = app.session.notices_since(mark).iter().map(|n| n.text.clone()).collect();
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(said[0].contains("2 of the selected features"), "{said:?}");
        assert_eq!(targets(&app).len(), 1);
    }

    /// On a map in the layer's own system, the handles are its own coordinates, as the map draws
    /// them: a feature with a place that can't be given in longitude/latitude gets its handles and
    /// isn't said, and a dragged vertex is written where the pointer put it, with every other
    /// place as stored (#2032 C2f-7).
    #[test]
    fn on_a_map_in_the_layers_own_system_handles_are_its_own_coordinates() {
        let mut app = app([square(0.)].into_iter());
        let id = app.session.active_layer.unwrap();
        let utm = rustgis_engine::data::raster::Crs::from_epsg(32631).unwrap();
        app.session.map_mut().editing.settings.enabled = true;
        app.session.map_mut().editing.snapping.enabled = false;
        let far = Geometry::Polygon(vec![vec![vec![
            Coord::new(500_000.25, 10.5),
            Coord::new(500_040.75, 10.5),
            Coord::new(1e9, 30.0),
            Coord::new(500_000.25, 30.0),
            Coord::new(500_000.25, 10.5),
        ]]]);
        let layer = app.session.map_mut().layer_mut(id).unwrap();
        layer.dataset.hold_in(utm).unwrap();
        layer.dataset.features[0].geometry = Some(far.clone());
        layer.touch_geometry();
        app.run("map.projection", json!({"type": "EPSG:32631"})).unwrap();
        let cam = &mut app.session.map_mut().camera;
        cam.center = Coord::new(500_020.0, 20.0);
        cam.res = 0.1;
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(1000., 800.));
        // Its handles, as it is stored; nothing said.
        let ts = targets(&app);
        assert_eq!(ts.len(), 1);
        assert!(matches!(ts[0].2, Cow::Borrowed(_)));
        assert_eq!(ts[0].2.as_ref(), &far);
        drop(ts);
        let mark = app.session.notice_mark();
        say_unplaced(&mut app);
        assert!(app.session.notices_since(mark).is_empty());
        // A drag of the second vertex.
        let from = view_to_screen(&app, rect, Coord::new(500_040.75, 10.5)).unwrap();
        assert!(drag_start(&mut app, rect, from));
        drag_to(&mut app, rect, from + egui::vec2(37.0, -21.0));
        let at = app.map.vertex_drag.unwrap().at;
        assert!((at.x - 500_044.45).abs() < 0.01 && (at.y - 12.6).abs() < 0.01, "{at:?}");
        drag_stop(&mut app);
        let g = app.session.map().layer(id).unwrap().dataset.features.geometry(0).unwrap().clone();
        assert_eq!(g, far.move_vertex(VertexId::new(0, 0, 1), at).unwrap());
        // Deleted, the others stay as stored.
        choose(&mut app, Some((id, 0, VertexId::new(0, 0, 1))));
        assert!(delete_chosen(&mut app));
        let g = app.session.map().layer(id).unwrap().dataset.features.geometry(0).unwrap().clone();
        assert_eq!(g, far.delete_vertex(VertexId::new(0, 0, 1)).unwrap());
    }

    /// A vertex snapped on a Web Mercator map is committed at the longitude/latitude it snapped
    /// to, exactly, not after a round trip through the map's coordinates.
    #[test]
    fn a_snapped_vertex_is_placed_exactly_where_it_snapped() {
        // The second feature's corner, at a place whose Mercator round trip isn't exact.
        let target = Coord::new(0.123_456_789_012_3, 0.987_654_321_098_7);
        let back = |app: &RustGisApp| view_to_lonlat(app, lonlat_to_view(app, target).unwrap()).unwrap();
        let other = Geometry::Polygon(vec![vec![vec![target, Coord::new(5., 5.), Coord::new(5., 6.), target]]]);
        let mut app = app([square(0.), other].into_iter());
        let id = app.session.active_layer.unwrap();
        app.session.map_mut().editing.settings.enabled = true;
        app.session.map_mut().editing.snapping.enabled = true;
        app.map.move_shared_vertices = false;
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(1000., 800.));
        assert_ne!(back(&app), target, "the place must not round-trip exactly, or the test proves nothing");
        let from = lonlat_to_screen(&app, rect, Coord::new(1., 1.)).unwrap();
        assert!(drag_start(&mut app, rect, from));
        let near = lonlat_to_screen(&app, rect, target).unwrap() + egui::vec2(1.0, 1.0);
        drag_to(&mut app, rect, near);
        assert_eq!(app.map.vertex_drag.unwrap().snapped_lonlat, Some(target));
        drag_stop(&mut app);
        let g = app.session.map().layer(id).unwrap().dataset.features.geometry(0).unwrap().clone();
        assert_eq!(g.vertex(VertexId::new(0, 0, 2)), Some(target), "{g:?}");
    }

    /// A vertex let go where the map's coordinates have no longitude/latitude is said once and
    /// not edited; one let go at a place that has one is moved, with nothing said.
    #[test]
    fn a_vertex_let_go_at_no_longitude_latitude_is_said_not_dropped() {
        let mut app = app([square(0.)].into_iter());
        let id = app.session.active_layer.unwrap();
        app.session.map_mut().editing.settings.enabled = true;
        app.map.move_shared_vertices = false;
        app.run("map.projection", json!({"type": "EPSG:32631"})).unwrap();
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(1000., 800.));
        let nowhere = Coord::new(1e12, 1e12);
        assert!(view_to_lonlat(&app, nowhere).is_none(), "the place must have no longitude/latitude, or the test proves nothing");
        let said = |app: &RustGisApp, mark| {
            app.session.notices_since(mark).iter().filter(|n| n.text.contains("has no longitude/latitude")).count()
        };
        let before = app.session.map().layer(id).unwrap().dataset.features.geometry(0).unwrap().clone();
        let from = lonlat_to_screen(&app, rect, Coord::new(1., 1.)).unwrap();
        assert!(drag_start(&mut app, rect, from));
        drag_to(&mut app, rect, from + egui::vec2(5.0, 5.0));
        app.map.vertex_drag.as_mut().unwrap().at = nowhere;
        let mark = app.session.notice_mark();
        drag_stop(&mut app);
        assert_eq!(said(&app, mark), 1);
        assert_eq!(app.session.map().layer(id).unwrap().dataset.features.geometry(0), Some(&before));
        // A clean release moves the vertex and says nothing.
        assert!(drag_start(&mut app, rect, from));
        drag_to(&mut app, rect, from + egui::vec2(5.0, 5.0));
        let mark = app.session.notice_mark();
        drag_stop(&mut app);
        assert_eq!(said(&app, mark), 0);
        assert_ne!(app.session.map().layer(id).unwrap().dataset.features.geometry(0), Some(&before));
    }

    /// A projection change during a drag cancels it: its place is in the old view's coordinates.
    #[test]
    fn a_projection_change_cancels_a_held_drag() {
        let mut app = app([square(0.)].into_iter());
        let id = app.session.active_layer.unwrap();
        app.session.map_mut().editing.settings.enabled = true;
        app.map.move_shared_vertices = false;
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(1000., 800.));
        let before = app.session.map().layer(id).unwrap().dataset.features.geometry(0).unwrap().clone();
        let from = lonlat_to_screen(&app, rect, Coord::new(1., 1.)).unwrap();
        assert!(drag_start(&mut app, rect, from));
        drag_to(&mut app, rect, from + egui::vec2(5.0, 5.0));
        app.run("map.projection", json!({"type": "EPSG:32631"})).unwrap();
        drag_stop(&mut app);
        assert!(app.map.vertex_drag.is_none());
        assert_eq!(app.session.map().layer(id).unwrap().dataset.features.geometry(0), Some(&before));
    }

    /// The count stops where the handles do: at their cap of placed features. With 49 placed
    /// ones selected first, the two unplaced after them are both counted; past 50 placed, one
    /// isn't, as it gets no handles either way.
    #[test]
    fn the_unplaced_count_follows_the_handles_cap() {
        let squares = (0..53).map(|i| square(f64::from(i) * 0.05));
        let mut app = app(squares);
        let id = app.session.active_layer.unwrap();
        let utm = rustgis_engine::data::raster::Crs::from_epsg(32631).unwrap();
        app.session.map_mut().editing.settings.enabled = true;
        let layer = app.session.map_mut().layer_mut(id).unwrap();
        layer.dataset.hold_in(utm).unwrap();
        let far = Geometry::Polygon(vec![vec![vec![
            Coord::new(500_000., 0.),
            Coord::new(1e9, 0.),
            Coord::new(500_000., 1.),
            Coord::new(500_000., 0.),
        ]]]);
        for r in [49, 50, 52] {
            layer.dataset.features.set_geometry(r, Some(far.clone()));
        }
        layer.selection = (0..53).collect();
        // 49 placed, two unplaced (49, 50), the 50th placed (51): the cap; 52 is past it.
        let found = unplaced_targets(&app);
        assert_eq!((found.rows, found.more), (vec![(id, 49), (id, 50)], false));
        assert_eq!(targets(&app).len(), MAX_FEATURES);
        let mark = app.session.notice_mark();
        say_unplaced(&mut app);
        let said: Vec<String> = app.session.notices_since(mark).iter().map(|n| n.text.clone()).collect();
        assert!(said.len() == 1 && said[0].contains("2 of the selected features"), "{said:?}");
        // What was said is kept with its map, so another map's layer of the same id and rows is
        // said for itself.
        let (map, live) = (app.session.project.active_map, app.session.map().layer(id).unwrap().instance_id());
        assert_eq!(app.map.vertex_unplaced_said, [(map, live, 49), (map, live, 50)]);
        // Another tool forgets it, so coming back says it again.
        app.run("tool.set", json!({"tool": "explore"})).unwrap();
        say_unplaced(&mut app);
        assert!(app.map.vertex_unplaced_said.is_empty());
        app.run("tool.set", json!({"tool": "editVertices"})).unwrap();
        let mark = app.session.notice_mark();
        say_unplaced(&mut app);
        assert_eq!(app.session.notices_since(mark).len(), 1);
    }

    /// The scan stops at [`MAX_UNPLACED`] features with no place, and the notice says "at least".
    #[test]
    fn a_selection_with_no_place_at_all_is_counted_up_to_a_bound() {
        let far = Geometry::Polygon(vec![vec![vec![
            Coord::new(500_000., 0.),
            Coord::new(1e9, 0.),
            Coord::new(500_000., 1.),
            Coord::new(500_000., 0.),
        ]]]);
        let mut app = app(std::iter::repeat_n(far, MAX_UNPLACED + 5));
        let id = app.session.active_layer.unwrap();
        app.session.map_mut().editing.settings.enabled = true;
        let layer = app.session.map_mut().layer_mut(id).unwrap();
        layer.dataset.held_in = rustgis_engine::data::raster::Crs::from_epsg(32631).unwrap();
        layer.touch_geometry();
        layer.selection = (0..MAX_UNPLACED + 5).collect();
        // Exactly the bound: counted as it is.
        app.session.map_mut().layer_mut(id).unwrap().selection = (0..MAX_UNPLACED).collect();
        let mark = app.session.notice_mark();
        say_unplaced(&mut app);
        let said: Vec<String> = app.session.notices_since(mark).iter().map(|n| n.text.clone()).collect();
        assert!(said.len() == 1 && said[0].contains(&format!(": {MAX_UNPLACED} of the selected")), "{said:?}");
        // One more: the same features found, and more, which is said.
        app.session.map_mut().layer_mut(id).unwrap().selection = (0..MAX_UNPLACED + 5).collect();
        let found = unplaced_targets(&app);
        assert!(found.rows.len() == MAX_UNPLACED && found.more);
        let mark = app.session.notice_mark();
        say_unplaced(&mut app);
        let said: Vec<String> = app.session.notices_since(mark).iter().map(|n| n.text.clone()).collect();
        assert!(said.len() == 1 && said[0].contains(&format!("at least {MAX_UNPLACED} of the selected")), "{said:?}");
    }

    /// Rows without geometry count towards neither bound, so the scan goes past them, and a change
    /// in the selection beyond them is looked at again: a different unplaced feature there is said.
    #[test]
    fn an_unplaced_feature_past_rows_without_geometry_is_said_when_it_changes() {
        let far = Geometry::Polygon(vec![vec![vec![
            Coord::new(500_000., 0.),
            Coord::new(1e9, 0.),
            Coord::new(500_000., 1.),
            Coord::new(500_000., 0.),
        ]]]);
        let n = MAX_FEATURES + MAX_UNPLACED + 10;
        let mut app = app(std::iter::repeat_n(far, n + 2));
        let id = app.session.active_layer.unwrap();
        app.session.map_mut().editing.settings.enabled = true;
        let layer = app.session.map_mut().layer_mut(id).unwrap();
        layer.dataset.held_in = rustgis_engine::data::raster::Crs::from_epsg(32631).unwrap();
        for r in 0..n {
            layer.dataset.features.set_geometry(r, None);
        }
        layer.touch_geometry();
        let said = |app: &mut RustGisApp, rows: Vec<usize>| {
            app.session.map_mut().layer_mut(id).unwrap().selection = rows;
            let mark = app.session.notice_mark();
            say_unplaced(app);
            app.session.notices_since(mark).iter().map(|n| n.text.clone()).collect::<Vec<_>>()
        };
        let first = said(&mut app, (0..n).chain([n]).collect());
        assert!(first.len() == 1 && first[0].contains(&format!("feature {n} has a place")), "{first:?}");
        let second = said(&mut app, (0..n).chain([n + 1]).collect());
        assert!(second.len() == 1 && second[0].contains(&format!("feature {} has a place", n + 1)), "{second:?}");
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
        // The pointer's place is in the map's coordinates; the layer is in longitude/latitude.
        let at = view_to_lonlat(&app, app.map.vertex_drag.unwrap().at).unwrap();
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
