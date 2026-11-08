//! The map view: the rendered map as a texture (re-rendered when the map or camera changes;
//! during pan/zoom the last frame is shifted and scaled for smoothness), the interactive tools,
//! sketches, labels, the identify pop-up, scale bar and north arrow, and the map view tabs.

use egui::{Align2, Color32, CornerRadius, Pos2, Rect, Sense, Stroke, Ui, pos2, vec2};
use rustgis_engine::Tool;
use rustgis_engine::analysis::{
    network::{radius_boundary_point, radius_distance_m},
    selection::MAX_POINTS,
};
use rustgis_engine::geom::{
    Coord, Extent, geodesic_length_m, geodesic_ring_area_m2, geodesic_ring_perimeter_m, wgs84_length_m, wgs84_ring_perimeter_area_m2,
};
use rustgis_engine::map::layout::TextAlign;
use rustgis_engine::map::projection::Flat;
use rustgis_engine::map::{Camera, Globe, Projection};
use rustgis_engine::render::rustgis_tiles::Unfinished;
use rustgis_engine::render::{PlacedLabel, Rendered, View};
use serde_json::{Value, json};

use crate::theme::{Tokens, c32, regular, semibold};
use crate::{RustGisApp, icons};

#[derive(Clone, Copy, PartialEq, Debug)]
struct Key {
    rev: u64,
    cam: (i64, i64, i64, i64, i64),
    size: [usize; 2],
    /// A temporary preview: an exact frame follows once motion settles.
    draft: bool,
    /// The tile cache's generation (0 for a map without tile layers).
    tiles: u64,
    /// Streamed layers' reads finished so far: a new one draws the map again.
    sources: u64,
    /// Which map, and which of its layers in what order, are shown ([`shown_layers`]): a frame
    /// is kept through a change to the contents only while this holds.
    shown: u64,
}

#[derive(Clone, Copy, PartialEq)]
enum DragKind {
    Pan,
    /// Turning and tilting the flat map (Ctrl+drag with the Pan tool, as in MapLibre).
    Turn,
    Box,
    /// Drawing a graphic by dragging (rectangle, circle, ellipse, freehand, lasso, picture).
    Sketch,
    SelectLasso,
    SelectRadius,
    /// Moving or inserting a vertex with the Edit Vertices tool.
    Vertex,
    /// Moving the selected text graphic, grabbed by its text (#516): its layer id and row, and
    /// where the press was. Held by the drag itself, so it ends with it, however it ends.
    Text {
        layer: u32,
        row: usize,
        origin: Pos2,
    },
    /// Moving a layer's label with the Move Label tool: where the press was. The label, and what
    /// it labels, are in `MapView::moving_label`.
    Label {
        origin: Pos2,
    },
}

/// Graphic shapes drawn by dragging rather than clicking.
fn drag_shape(tool: Tool) -> Option<&'static str> {
    match tool {
        Tool::Graphic(s) if matches!(s, "rectangle" | "circle" | "ellipse" | "freehand" | "lasso" | "picture") => Some(s),
        _ => None,
    }
}

/// Graphic shapes drawn vertex by vertex, finished with a double-click.
fn vertex_shape(tool: Tool) -> bool {
    matches!(tool, Tool::Graphic("line" | "curve" | "polygon"))
}

pub struct Popup {
    pub info: Value,
    pub at: Coord,
    /// Which of the identified features is shown (Visible Layers identifies several).
    pub index: usize,
    /// The map it was opened on (the project's `active_map` then): layer and feature ids belong to
    /// a map, and a duplicated map has the same ones.
    pub map: usize,
    /// The project it was opened on (`Session::opened`): answers still on their way when another
    /// project opens are dropped.
    pub opened: u64,
    /// The WMS layers being asked what is at the point, whose features join the list.
    pub asks: Vec<Ask>,
    /// When it opened: it waits for the WMS layers' answers up to [`ASK_WAIT`] from then.
    since: web_time::Instant,
}

/// How long a pop-up waits for the WMS layers asked before it shows what was found: then their
/// answers join it as they come.
const ASK_WAIT: std::time::Duration = std::time::Duration::from_secs(4);

/// A WMS layer asked what is at a popup's point (GetFeatureInfo), its answer on the way: the
/// request's, or first its service's capabilities (a layer from a project doesn't know its request).
pub struct Ask {
    layer: String,
    /// The other layers waiting for the same capabilities: fetched and read once for all.
    also: Vec<String>,
    capabilities: bool,
    /// The GetFeatureInfo request asked (none for capabilities): its answer format tells what to
    /// ask next after the service's error.
    request: Option<String>,
    slot: crate::Slot<Result<Vec<u8>, String>>,
}

impl Popup {
    /// A popup for what identify found at `at`, asking the WMS layers it lists.
    pub fn new(info: Value, at: Coord, map: usize, opened: u64) -> Popup {
        let mut asks: Vec<Ask> = Vec::new();
        // The capabilities each fetch is for: layers of one service share it.
        let mut fetched: Vec<(String, usize)> = Vec::new();
        for a in info["featureInfo"].as_array().into_iter().flatten() {
            let Some(layer) = a["tileLayer"].as_str().map(str::to_string) else { continue };
            let (url, capabilities) = match (a["url"].as_str(), a["capabilities"].as_str()) {
                (Some(u), _) => (u, false),
                (None, Some(c)) => (c, true),
                _ => continue,
            };
            if capabilities && let Some(&(_, i)) = fetched.iter().find(|(c, _)| c == url) {
                if let Some(ask) = asks.get_mut(i) {
                    ask.also.push(layer);
                }
                continue;
            }
            if capabilities {
                fetched.push((url.to_string(), asks.len()));
            }
            let slot = crate::fetch_into_limited(url, rustgis_engine::data::ogc::MAX_INFO_BYTES);
            let request = (!capabilities).then(|| url.to_string());
            asks.push(Ask { layer, also: Vec::new(), capabilities, request, slot });
        }
        Popup { info, at, index: 0, map, opened, asks, since: web_time::Instant::now() }
    }

    /// Whether it waits for the WMS layers asked, to show their answers and what was found
    /// together, in drawing order: a layer drawn over the feature found may answer, and take its
    /// place.
    fn asking(&self) -> bool {
        !self.asks.is_empty() && self.since.elapsed() < ASK_WAIT
    }
}

/// Whether a popup's value is a feature or pixel found, not only WMS layers still being asked.
fn is_found(v: &Value) -> bool {
    v.get("layer").is_some() || v.get("attributes").is_some()
}

/// A popup's results: its list, or the one feature or pixel found; none while it only waits for
/// WMS layers' answers.
fn results_of(info: &Value) -> Vec<Value> {
    match info["results"].as_array() {
        Some(r) => r.clone(),
        None if is_found(info) => vec![info.clone()],
        None => Vec::new(),
    }
}

/// Where the result at `index` of `before` is in `after`, the pop-up's results once some were
/// added or left out: the same one of identical results (a service may answer a feature twice,
/// and both lists keep their order), or the first when it's gone.
fn index_after(before: &[Value], index: usize, after: &[Value]) -> usize {
    let Some(shown) = before.get(index) else { return 0 };
    let nth = before.iter().take(index).filter(|v| *v == shown).count();
    after.iter().enumerate().filter(|(_, v)| *v == shown).nth(nth).map_or(0, |(i, _)| i)
}

/// A popup's results with the WMS layers' answers `found` added, in the map's drawing order, top
/// first, as identify ranks what it finds itself: a WMS layer drawn over the feature found comes
/// before it, and with topmost, the map's pop-up mode by default, takes its place once it answers
/// with features. `None` with nothing to show.
fn with_answers(map: &rustgis_engine::map::Map, info: &Value, found: Vec<Value>) -> Option<Value> {
    use rustgis_engine::map::Drawn;
    let mut all = results_of(info);
    all.extend(found);
    // A tile layer's results show while it draws: one hidden since must not keep a later answer
    // out. Of two layers of one name, the answers are placed by the one drawn (though
    // identify.featureInfo tells its answer to the first of them, drawn or not).
    let drawn = |name: &str| map.tiles.iter().position(|t| t.name == name && t.draws());
    all.retain(|v| v["tileLayer"].as_str().is_none_or(|name| drawn(name).is_some()));
    let stack = map.stack();
    // A tile layer's feature by its layer's name (two of one name drawn take the higher one's
    // place, as their answers are told apart by name too), a map layer's (or raster's pixel) by
    // its id; anything else after them all.
    let place = |v: &Value| {
        let drawn = match v["tileLayer"].as_str() {
            Some(name) => drawn(name).map(Drawn::Tile),
            None => v["layerId"].as_u64().and_then(|id| u32::try_from(id).ok()).and_then(|id| map.layer_index(id)).map(Drawn::Layer),
        };
        drawn.and_then(|d| stack.iter().position(|o| *o == d)).unwrap_or(usize::MAX)
    };
    // Stable: each layer's results keep the order identify or the service gave them.
    all.sort_by_cached_key(place);
    if map.popup_mode == rustgis_engine::map::PopupMode::Topmost {
        let top = all.first().map(place);
        all.retain(|v| Some(place(v)) == top);
    }
    let mut first = all.first().cloned()?;
    if let Value::Object(o) = &mut first {
        o.remove("featureInfo");
        o.insert("results".into(), Value::Array(all));
    }
    Some(first)
}

/// A notice shown as a toast.
pub struct Toast {
    pub text: String,
    pub error: bool,
    /// When it came, or was last hovered (it stays while the pointer is on it).
    pub at: f64,
    /// Its number, for a steady id.
    pub id: u64,
}

/// A selection outline belongs to one unchanged view, not just to a tool code.
struct SelectionGesture {
    tool: Tool,
    map: usize,
    camera: Camera,
    projection: Projection,
    rect: Rect,
}

pub struct MapView {
    texture: Option<egui::TextureHandle>,
    key: Option<Key>,
    /// A preview is attempted once for each frame key, including when drawing fails.
    preview_attempt: Option<Key>,
    /// The frame on screen kept through a change to the map's contents, and since when (s).
    kept: Option<(Key, f64)>,
    tex_camera: Camera,
    /// The size (points) of the view the texture was drawn for: larger than the map while it
    /// moves, so zooming out and panning have more of it to show.
    tex_size: (f64, f64),
    /// Where the texture sits on screen this frame.
    tex_rect: Rect,
    /// Where the map view was drawn on screen (points), and on which frame: what a screen
    /// snapshot for the MCP relay crops to.
    pub view_rect: Option<(Rect, u64)>,
    /// The map's controls, scale bar, sketches and pop-up are left out (a screen snapshot for the
    /// MCP relay that asked for the map alone).
    pub hide_overlays: bool,
    /// How long a frame of the view at full resolution takes to draw (ms), as the last one
    /// measured it: frames drawn mid-motion are spaced out by it, and drawn coarser only when
    /// full resolution would be too slow (see `motion_resolution`).
    motion_cost: f64,
    labels: Vec<PlacedLabel>,
    /// On the globe, where each label stands on the Earth, so it follows the globe as it turns
    /// and zooms until the next frame places them again.
    label_lonlat: Vec<Option<Coord>>,
    /// Where on a tilted map's ground each label is (the camera's coordinates), so it stays there
    /// while the map moves.
    label_map: Vec<Option<Coord>>,
    /// Where each label's callout starts (its feature), as `label_lonlat` and `label_map` keep the
    /// labels: its line stays on its feature while the map moves.
    callout_lonlat: Vec<Option<Coord>>,
    callout_map: Vec<Option<Coord>>,
    /// The camera the labels were placed for.
    label_camera: Camera,
    /// The map revision whose GPU scene had only live tile layers (#635): tiles arriving draw
    /// no new scene of it.
    live_rev: Option<u64>,
    last_interaction: f64,
    /// The view's size (physical pixels) on the last frame, and when it last changed: while a
    /// side pane is dragged the last frame is shown as it is, drawn again once the size holds.
    view_px: [usize; 2],
    resized_at: f64,
    drag: Option<(Pos2, DragKind)>,
    /// The label being moved with the Move Label tool.
    moving_label: Option<MovingLabel>,
    sketch: Vec<Coord>,
    selection_gesture: Option<SelectionGesture>,
    radius_preview: Option<(Coord, f64, Vec<Option<Pos2>>)>,
    /// A finished measurement stays on screen until the next click.
    sketch_done: bool,
    /// The next click splits the selected lines there (Editing pane › Split).
    pub split_mode: bool,
    /// The vertex chosen with the Edit Vertices tool (layer, row, vertex): Delete removes it.
    pub vertex: Option<(u32, usize, rustgis_engine::data::VertexId)>,
    /// The vertex being dragged with the Edit Vertices tool.
    pub vertex_drag: Option<crate::vertexedit::VertexDrag>,
    /// Non-owning source identity for the held vertex gesture; never persisted with the project.
    pub(crate) vertex_drag_owner: Option<crate::vertexedit::DragOwner>,
    /// Session-only opt-in for coincident polygon moves; never saved with a project or preference.
    pub move_shared_vertices: bool,
    /// A sketch vertex clicked on an edge follows that edge from the previous vertex (Trace);
    /// session only.
    pub trace_edges: bool,
    /// A notice said the sketch reached the drawn-line limit (a trace refused, or a full sketch):
    /// once per sketch.
    sketch_full_said: bool,
    /// The magnifying lens follows the pointer.
    pub magnifier: bool,
    pub popup: Option<Popup>,
    /// The session revision the popup was last checked against the map at ([`follow_popup`]).
    popup_rev: u64,
    /// A map or layout waiting for the user to confirm its deletion.
    pub deleting: Option<Deletion>,
    pub pointer_lonlat: Option<Coord>,
    /// The last pointer position seen over a map, kept for the status bar readout after the
    /// pointer leaves the map; with the map's instance and the session's `opened` it belongs to.
    kept_lonlat: Option<(Coord, u64, u64)>,
    /// Where the map's context menu was opened: the pointer leaves the map for the menu.
    pub menu_lonlat: Option<Coord>,
    /// The text graphic the context menu was opened on (layer id, row), for Edit Text.
    pub menu_text: Option<(u32, usize)>,
    /// How the pointer's coordinates read (status bar, right-click menu); a preference.
    pub coord_format: crate::CoordReadout,
    pub render_ms: u64,
    pub notices_seen: usize,
    pub toasts: Vec<Toast>,
    /// Toasts made so far, numbering each for a steady id.
    pub toasts_made: u64,
    camera_moved: bool,
    /// The last snap: its inputs (pointer, camera, undo depth) and where it snapped, if anywhere,
    /// so snapping runs only when one of them changes rather than every frame.
    snap: Option<([f64; 5], Option<Coord>)>,
    /// The side-by-side map's last render (texture and labels) and what it was drawn for, so it
    /// redraws only when that map, the project or the view changes.
    split: Option<([f64; 10], egui::TextureHandle, Vec<PlacedLabel>)>,
    /// The picture an identified feature carries (a photo's thumbnail, a `data:image/…` value):
    /// a hash of the value, and its texture (or why it couldn't be read).
    picture: Option<(u64, Result<egui::TextureHandle, String>)>,
    /// The tile layers' credits, from the last render.
    attribution: Vec<String>,
    /// Tile layer errors already reported.
    tile_errors: std::collections::HashSet<String>,
    /// When the map last rendered (seconds).
    last_render: f64,
    /// The map drawn flat and laid over the globe while it moves.
    drape: crate::globe_drape::GlobeDrape,
    /// The moving tilted map's drape.
    tilt_drape: crate::tilt_drape::TiltDrape,
    /// Exact globe, tilted and tiled flat frames, composed off the UI thread.
    tilt_settle: crate::tilt_settle::TiltSettle<Key>,
    /// The frame shown has tile pictures still to draw: drawn again.
    filling: bool,
    /// Shooting stars around the globe.
    comets: crate::comets::Comets,
    /// GPU map backend: pipelines (built lazily from the render state), the uploaded scene,
    /// and egui's wgpu render state handed over by the host app.
    pub(crate) gpu_renderer: Option<std::sync::Arc<crate::mapgpu::GpuMapRenderer>>,
    pub(crate) gpu_frame: Option<std::sync::Arc<crate::mapgpu::SceneBuffers>>,
    /// The map revision and zoom level a turned or tilted GPU scene was declined at (it couldn't
    /// be drawn on the GPU: labels with icons, or over the device's limits): until either
    /// changes such views stay on the CPU, with the tilted map's drape and its settled frame
    /// drawn off the UI thread.
    pub(crate) gpu_turn_declined: Option<(u64, i32)>,
    /// The GPU scene's offscreen MSAA target, shared with the paint callback across frames.
    pub(crate) gpu_target: std::sync::Arc<std::sync::Mutex<Option<crate::mapgpu::SceneTarget>>>,
    /// The last side-by-side map's GPU scene (its key, buffers and labels), when the second
    /// map rendered on the GPU.
    pub(crate) split_gpu: Option<([f64; 10], std::sync::Arc<crate::mapgpu::SceneBuffers>, Vec<PlacedLabel>)>,
    /// The split map's offscreen MSAA target (its own slot — main and split differ in size).
    pub(crate) gpu_target_split: std::sync::Arc<std::sync::Mutex<Option<crate::mapgpu::SceneTarget>>>,
    pub(crate) render_state: Option<egui_wgpu::RenderState>,
    /// The GPU map backend, for now gated on the RUSTGIS_GPU environment variable.
    pub gpu_enabled: bool,
    /// Rolling map-frame timestamps (for the status-bar FPS readout).
    frame_times: std::collections::VecDeque<f64>,
}

impl Default for MapView {
    /// Starts with no chosen node, preview or retained gesture owner, alongside empty map caches.
    fn default() -> Self {
        MapView {
            texture: None,
            key: None,
            preview_attempt: None,
            kept: None,
            tex_camera: Camera::world(),
            tex_size: (0.0, 0.0),
            tex_rect: Rect::NOTHING,
            view_rect: None,
            hide_overlays: false,
            motion_cost: 0.0,
            labels: Vec::new(),
            label_lonlat: Vec::new(),
            callout_lonlat: Vec::new(),
            callout_map: Vec::new(),
            label_camera: Camera::world(),
            live_rev: None,
            last_interaction: -10.0,
            view_px: [0, 0],
            resized_at: -10.0,
            drag: None,
            moving_label: None,
            sketch: Vec::new(),
            selection_gesture: None,
            radius_preview: None,
            sketch_done: false,
            split_mode: false,
            vertex: None,
            vertex_drag: None,
            vertex_drag_owner: None,
            move_shared_vertices: false,
            trace_edges: false,
            sketch_full_said: false,
            magnifier: false,
            popup: None,
            popup_rev: 0,
            deleting: None,
            pointer_lonlat: None,
            kept_lonlat: None,
            menu_lonlat: None,
            menu_text: None,
            coord_format: crate::CoordReadout::Decimal,
            render_ms: 0,
            notices_seen: 0,
            toasts: Vec::new(),
            toasts_made: 0,
            camera_moved: false,
            snap: None,
            split: None,
            picture: None,
            attribution: Vec::new(),
            tile_errors: std::collections::HashSet::new(),
            last_render: -10.0,
            drape: Default::default(),
            tilt_drape: Default::default(),
            tilt_settle: Default::default(),
            filling: false,
            label_map: Vec::new(),
            comets: Default::default(),
            gpu_renderer: None,
            gpu_frame: None,
            gpu_target: std::sync::Arc::new(std::sync::Mutex::new(None)),
            split_gpu: None,
            gpu_turn_declined: None,
            gpu_target_split: std::sync::Arc::new(std::sync::Mutex::new(None)),
            render_state: None,
            gpu_enabled: std::env::var_os("RUSTGIS_GPU").is_some(),
            frame_times: std::collections::VecDeque::new(),
        }
    }
}

impl MapView {
    /// The position the status bar shows: the last one over this map, kept while the pointer is
    /// elsewhere. A position from another map or project no longer applies, so it is dropped.
    pub fn kept_pointer_lonlat(&mut self, map_instance: u64, opened: u64) -> Option<Coord> {
        match self.kept_lonlat {
            Some((c, m, o)) if m == map_instance && o == opened => Some(c),
            _ => {
                self.kept_lonlat = None;
                None
            }
        }
    }

    /// Whether an exact frame is being composed off the UI thread, or the frame shown waits for
    /// tile pictures still to be drawn.
    pub fn drawing(&self) -> bool {
        self.tilt_settle.busy() || self.filling
    }

    /// egui's wgpu render state from the host (`frame.wgpu_render_state()`), for the GPU map
    /// backend. Cheap to call every frame: the state is Arc-backed and pipelines build lazily.
    pub fn set_render_state(&mut self, rs: Option<egui_wgpu::RenderState>) {
        self.render_state = rs;
    }

    /// Records one drawn map frame (the status bar's FPS readout reads the rolling window).
    fn record_frame(&mut self, now: f64) {
        self.frame_times.push_back(now);
        while self.frame_times.len() > 240 {
            self.frame_times.pop_front();
        }
        while self.frame_times.front().is_some_and(|t| *t < now - 2.0) {
            self.frame_times.pop_front();
        }
    }

    /// Frames drawn in the last second; `None` when the map is idle (egui repaints on demand,
    /// so no frames means nothing is moving — distinct from "0 fps" when moving).
    pub(crate) fn fps(&self, now: f64) -> Option<usize> {
        let live = self.frame_times.back().is_some_and(|t| *t > now - 0.5);
        live.then(|| self.frame_times.iter().filter(|t| **t > now - 1.0).count())
    }
    /// Forces a re-render on the next frame; also drops the GPU frame (the backend toggle
    /// relies on it).
    pub fn invalidate(&mut self) {
        self.key = None;
        self.preview_attempt = None;
        self.kept = None;
        self.gpu_frame = None;
        self.tilt_settle = Default::default();
    }
    /// The camera the last full render was drawn with (diagnostics and tests).
    pub fn full_frame_camera(&self) -> Camera {
        self.tex_camera
    }

    /// Whether the map shown is a GPU scene that moves with the camera (vector meshes and raster
    /// tiles) rather than a picture shifted and scaled until it is drawn again (diagnostics and
    /// tests).
    pub fn moves_on_gpu(&self) -> bool {
        self.gpu_frame.as_ref().is_some_and(|f| !f.has_images)
    }

    /// Whether the map shown places its tile layers for every frame (#635): its tiles are for the
    /// camera shown, whatever camera its scene was drawn for (diagnostics and tests).
    pub fn shows_live_tiles(&self) -> bool {
        self.gpu_frame.as_ref().is_some_and(|f| f.tiles_live)
    }

    /// How long the last map draw took, in milliseconds.
    pub fn render_ms(&self) -> u64 {
        self.render_ms
    }

    /// Drops unfinished map sketches, selection shapes and node previews without committing them.
    pub fn cancel_sketch(&mut self) {
        self.sketch.clear();
        self.sketch_done = false;
        self.sketch_full_said = false;
        if self.drag.is_some_and(|(_, kind)| matches!(kind, DragKind::SelectLasso | DragKind::SelectRadius)) {
            self.drag = None;
        }
        self.selection_gesture = None;
        self.radius_preview = None;
        self.cancel_vertex_drag();
        self.cancel_label_drag();
    }

    /// Drops a label being moved with the Move Label tool, leaving it where it was.
    pub(crate) fn cancel_label_drag(&mut self) {
        self.moving_label = None;
        if self.drag.is_some_and(|(_, kind)| matches!(kind, DragKind::Label { .. })) {
            self.drag = None;
        }
    }

    /// Cancels a vertex preview and its pending release together, leaving other map gestures
    /// alone. The chosen node is cleared because its row/address may now belong to another shape.
    pub(crate) fn cancel_vertex_drag(&mut self) {
        self.vertex = None;
        self.vertex_drag = None;
        self.vertex_drag_owner = None;
        if self.drag.is_some_and(|(_, kind)| kind == DragKind::Vertex) {
            self.drag = None;
        }
    }
    pub fn sketch_len(&self) -> usize {
        self.sketch.len()
    }
}

fn now(ui: &Ui) -> f64 {
    ui.input(|i| i.time)
}

/// Shows the active map workspace, discarding stale node gestures after map-tab changes and
/// before rendering their overlays or consuming pointer releases.
pub fn show(app: &mut RustGisApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    crate::modelbuilder::follow_view(app);
    view_tabs(app, ui, &t);
    crate::vertexedit::cancel_stale_drag(app);
    if app.mb.shown() {
        crate::modelbuilder::show(app, ui);
        return;
    }
    if app.session.active_layout.is_some() {
        crate::layoutview::show(app, ui);
        return;
    }
    if app.ws.catalog_view {
        egui::Frame::NONE.inner_margin(egui::Margin::same(16)).show(ui, |ui| crate::workspacepanes::catalog_view(app, ui, &t));
        return;
    }
    // A text selected on the map opens the Format pane (#516).
    crate::graphicformat::follow(app, ui.ctx());
    animate(app, ui);
    // Side by side: the other map on the right, following this one when views are linked.
    if let Some(other) = split_map(app).filter(|i| *i != app.session.project.active_map) {
        let full = ui.available_rect_before_wrap();
        let right = Rect::from_min_max(pos2(full.center().x + 1.0, full.min.y), full.max);
        split_view(app, ui, other, right, &t);
        let mut left = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_max(full.min, pos2(full.center().x - 1.0, full.max.y))));
        main_view(app, &mut left, &t);
        ui.allocate_rect(full, Sense::hover());
        return;
    }
    main_view(app, ui, &t);
}

/// Plays the map's animation: the camera moves through the keyframes in real time.
fn animate(app: &mut RustGisApp, ui: &mut Ui) {
    let Some((t, k)) = app.ws.playing else { return };
    let keys = app.session.map().animation.clone();
    let (Some(a), Some(b)) = (keys.get(k), keys.get(k + 1)) else {
        app.ws.playing = None;
        return;
    };
    let dt = ui.input(|i| i.stable_dt).min(0.1) as f64;
    let t = t + dt / b.seconds.max(0.1);
    let e = t.min(1.0);
    let s = e * e * (3.0 - 2.0 * e);
    let c = Coord::new(a.center.x + (b.center.x - a.center.x) * s, a.center.y + (b.center.y - a.center.y) * s);
    let sc = rustgis_engine::map::camera::scale_between(a.scale, b.scale, s);
    let m = app.session.map_mut();
    if let Some(mc) = m.to_map(c) {
        m.camera.center = mc;
    }
    m.set_scale(sc);
    app.ws.playing = if t >= 1.0 { Some((0.0, k + 1)) } else { Some((t, k)) };
    ui.ctx().request_repaint();
}

/// The second map of a side-by-side view: drawn read-only at its own (or the linked) camera.
fn split_view(app: &mut RustGisApp, ui: &mut Ui, other: usize, rect: Rect, t: &Tokens) {
    let (w, h) = (rect.width() as f64, rect.height() as f64);
    if w < 4.0 || h < 4.0 {
        return;
    }
    let ppp = ui.ctx().pixels_per_point();
    let revision = app.session.revision;
    let Some(m) = app.session.project.maps.get_mut(other) else { return };
    let view = View { width: w, height: h, pixels_per_point: ppp as f64, camera: m.camera, sky: true };
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, c32(m.palette().0));
    let key = [
        other as f64,
        revision as f64,
        m.camera.center.x,
        m.camera.center.y,
        m.camera.res,
        m.camera.bearing,
        m.camera.pitch,
        w,
        h,
        f64::from(ppp),
    ];
    let gpu_ok = app.map.gpu_enabled
        && !m.projection.is_globe()
        && m.camera.bearing == 0.0
        && m.camera.pitch == 0.0
        && m.view.effects.is_none()
        && m.view.color_vision == rustgis_engine::map::view::ColorVision::Normal;
    // Fresh GPU scene? Keep it. Stale? Rebuild. Either way `rendered_gpu` tracks whether the
    // split map draws from `split_gpu` this frame.
    let mut rendered_gpu = gpu_ok && app.map.split_gpu.as_ref().is_some_and(|(k, _, _)| *k == key);
    if gpu_ok && !rendered_gpu {
        if app.map.gpu_renderer.is_none()
            && let Some(rs) = &app.map.render_state
        {
            app.map.gpu_renderer = Some(std::sync::Arc::new(crate::mapgpu::GpuMapRenderer::new(rs)));
        }
        if let (Some(r), Some(rs)) = (&app.map.gpu_renderer, &app.map.render_state)
            && let Some(rustgis_engine::render::RenderOutput::Gpu(scene)) =
                rustgis_engine::render::render_backend(m, &view, rustgis_engine::render::Backend::Gpu)
        {
            // None = the scene exceeds the device's limits: the CPU path below renders it.
            if let Some(bufs) = crate::mapgpu::SceneBuffers::from_scene(&rs.device, &rs.queue, r, &scene, m.camera, [w, h], ppp) {
                app.map.split_gpu = Some((key, std::sync::Arc::new(bufs), scene.labels));
                rendered_gpu = true;
            }
        }
    }
    if !rendered_gpu {
        app.map.split_gpu = None;
        if app.map.split.as_ref().is_none_or(|(k, _, _)| *k != key)
            && let Some(r) = rustgis_engine::render::render(m, &view)
        {
            let img = egui::ColorImage::from_rgba_premultiplied([r.pixmap.width() as usize, r.pixmap.height() as usize], r.pixmap.data());
            match app.map.split.take() {
                Some((_, mut tex, _)) => {
                    tex.set(img, egui::TextureOptions::LINEAR);
                    app.map.split = Some((key, tex, r.labels));
                }
                None => app.map.split = Some((key, ui.ctx().load_texture("split_map", img, egui::TextureOptions::LINEAR), r.labels)),
            }
        }
    }
    if let (Some((_, bufs, labels)), Some(gr)) = (&app.map.split_gpu, &app.map.gpu_renderer) {
        painter.add(egui_wgpu::Callback::new_paint_callback(
            rect,
            crate::mapgpu::SceneCallback {
                renderer: gr.clone(),
                buffers: bufs.clone(),
                target: app.map.gpu_target_split.clone(),
                uv: Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                live: None,
                repaint: None,
            },
        ));
        for l in labels {
            draw_label(&painter, l, pos2(rect.min.x + l.x, rect.min.y + l.y));
        }
    } else if let Some((_, tex, labels)) = &app.map.split {
        painter.image(tex.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        for l in labels {
            draw_label(&painter, l, pos2(rect.min.x + l.x, rect.min.y + l.y));
        }
    }
    painter.line_segment([rect.left_top(), rect.left_bottom()], Stroke::new(2.0, t.border_strong));
    let name = app.session.project.maps.get(other).map(|m| m.name.clone()).unwrap_or_default();
    painter.text(rect.left_top() + vec2(10.0, 10.0), Align2::LEFT_TOP, name, semibold(12.0), t.text);
    // Linked cursors: the pointer's place on the main map, shown on this one.
    if app.ws.link_cursors
        && let Some(c) = app.map.pointer_lonlat
        && let Some(m) = app.session.project.maps.get(other)
        && let Some(mc) = m.to_map(c)
    {
        let (x, y) = m.camera.to_screen(mc, w, h);
        let p = pos2(rect.min.x + x as f32, rect.min.y + y as f32);
        if rect.contains(p) {
            let red = Color32::from_rgb(0xE0, 0x30, 0x30);
            painter.line_segment([p - vec2(10.0, 0.0), p + vec2(10.0, 0.0)], Stroke::new(1.5, red));
            painter.line_segment([p - vec2(0.0, 10.0), p + vec2(0.0, 10.0)], Stroke::new(1.5, red));
        }
    }
}

fn main_view(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    let t = *t;
    let rect = ui.available_rect_before_wrap();
    let resp = ui.allocate_rect(rect, Sense::click_and_drag());
    let (w, h) = (rect.width() as f64, rect.height() as f64);
    if w < 4.0 || h < 4.0 {
        return;
    }
    app.session.view_size = (w, h);
    app.map.view_rect = Some((rect, ui.ctx().cumulative_frame_nr()));
    // The start extent, fitted to the view's real size.
    if let Some(e) = app.ui.start_view.take() {
        let _ = app.run("map.zoomToExtent", json!({"extent": e}));
    }
    let ppp = ui.ctx().pixels_per_point();
    let time = now(ui);
    app.map.record_frame(time);

    interact(app, ui, &resp, rect);

    // Render when the map changed and we're not in the middle of a pan/zoom: meanwhile the flat
    // map's last frame is shifted and scaled. The globe can't be faked that way (turning and
    // zooming in perspective aren't shifts and scales): while it moves, the map drawn flat is
    // draped over the sphere (see `globe_drape`), or failing that it redraws every frame at
    // reduced resolution; it is drawn exactly once it settles.
    let cam = app.session.map().camera;
    let globe_view = app.session.map().projection == Projection::Globe;
    let dragging = app.map.drag.is_some_and(|(_, k)| k == DragKind::Pan || k == DragKind::Turn);
    // A tilted map waits longer before its exact (slower) frame: its drape already shows it in
    // perspective, so a short pause in a gesture doesn't stop to draw it.
    let settle = if !globe_view && cam.pitch != 0.0 { 0.4 } else { 0.18 };
    let moving = dragging || time - app.map.last_interaction < settle;
    // The GPU backend draws effect-free flat maps, turned and tilted ones too unless they need
    // pictures drawn on the CPU (tiles, rasters…). Vector scenes re-project in the vertex
    // shader; image scenes shift and scale as cached pictures while navigating.
    // A decline holds at its revision and zoom level (what it shows depends on both).
    let rev = (app.session.revision, (cam.res.max(1e-9).log2().round()) as i32);
    let turned_view = View { width: w, height: h, pixels_per_point: ppp as f64, camera: cam, sky: true };
    let gpu_ok = app.map.gpu_enabled
        && app.map.render_state.is_some()
        && !globe_view
        && ((cam.bearing == 0.0 && cam.pitch == 0.0)
            || (app.map.gpu_turn_declined != Some(rev) && rustgis_engine::render::gpu_may_turn(app.session.map(), &turned_view)))
        && app.session.map().view.effects.is_none()
        && app.session.map().view.color_vision == rustgis_engine::map::view::ColorVision::Normal;
    let mut draped = if globe_view && moving {
        let (key, tiles) = drape_state(app);
        app.map.drape.prepare(ui.ctx(), app.session.map_mut(), key, tiles, w, h, ppp, time)
    } else {
        if !globe_view {
            app.map.drape.clear();
        }
        false
    };
    // A tilted map moving on the CPU: its bands drawn flat once are laid over the tilted ground
    // (see `tilt_drape`), or failing that it redraws every frame. On the GPU it re-projects.
    // So does a flat map turning (to a bearing other than its frame's): drawn again for every
    // step of a turn, on the UI thread, a busy map (a tiled overlay) took over a hundred
    // milliseconds a frame. Its drape is one band round the view; panning it shifts its frame.
    let turned = !globe_view && cam.pitch == 0.0 && cam.bearing != 0.0 && !gpu_ok;
    let tilted = !globe_view && (cam.pitch != 0.0 || (turned && app.map.tex_camera.bearing != cam.bearing)) && !gpu_ok;
    let tilt_draped = if tilted && moving {
        let (key, tiles) = drape_state(app);
        app.map.tilt_drape.prepare(ui.ctx(), app.session.map_mut(), key, tiles, w, h, ppp, time)
    } else {
        if !tilted && !turned {
            app.map.tilt_drape.clear();
        }
        false
    };
    let draft = globe_view && moving && !draped;
    let render_ppp = if draft { (ppp * 0.6).max(0.5) } else { ppp };
    let key = Key {
        rev: app.session.revision,
        cam: (
            (cam.center.x * 100.0) as i64,
            (cam.center.y * 100.0) as i64,
            (cam.res * 1000.0) as i64,
            (cam.bearing * 1000.0) as i64,
            (cam.pitch * 1000.0) as i64,
        ),
        size: [(w as f32 * ppp).round() as usize, (h as f32 * ppp).round() as usize],
        draft,
        tiles: 0,
        sources: rustgis_engine::map::source::generation(),
        // Read only for tiled maps (see `keeps_frame`).
        shown: if app.session.map().tiles.is_empty() { 0 } else { shown_layers(&app.session) },
    };
    // Tiles arriving draw the map again, at most a few times a second.
    static WAKER: std::sync::Once = std::sync::Once::new();
    WAKER.call_once(|| {
        let ctx = ui.ctx().clone();
        rustgis_engine::render::rustgis_tiles::cache::set_waker(move || ctx.request_repaint());
        // A read that failed is tried again when its wait is over, though nothing else moves.
        let ctx = ui.ctx().clone();
        rustgis_engine::render::rustgis_tiles::cache::set_alarm(move |after| ctx.request_repaint_after(after));
        let ctx = ui.ctx().clone();
        rustgis_engine::map::source::set_waker(move || ctx.request_repaint());
    });
    let mut key = key;
    // A scene whose tile layers are all live places them for every frame: tiles arriving need no
    // new scene (#635).
    if gpu_ok
        && cam.bearing == 0.0
        && cam.pitch == 0.0
        && app.map.live_rev == Some(app.session.revision)
        && !reads_in_place(app.session.map())
    {
        key.tiles = 0;
    } else if fetches_tiles(app.session.map()) {
        key.tiles = rustgis_engine::render::rustgis_tiles::cache::generation();
        if let Some(held) = held_tiles(key, app.map.tilt_settle.key(), app.map.key, time - app.map.last_render) {
            key.tiles = held;
            ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(TILE_REDRAW_GAP));
        }
    }
    if app.map.view_px != key.size {
        // Not the first size: the view has a picture to hold only after that.
        if app.map.view_px != [0, 0] {
            app.map.resized_at = time;
        }
        app.map.view_px = key.size;
    }
    // A side pane being dragged (or the window resized) resizes the view every frame. Drawing the
    // map each time made the drag stutter: the last frame stays centred at its own scale (the uncovered edge shows the
    // map's background) until the size holds for a moment, as a pan shows its last frame.
    let resizing = time - app.map.resized_at < RESIZE_SETTLE;
    let hold = resizing
        && !moving
        && !globe_view
        && cam.bearing == 0.0
        && cam.pitch == 0.0
        && (app.map.texture.is_some() || app.map.gpu_frame.is_some())
        && app.map.key.is_some_and(|k| (Key { size: k.size, draft: k.draft, ..key }) == k);
    if hold {
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(RESIZE_SETTLE));
    }
    // Exact globe, tilted and tiled flat frames are composed off the UI thread. A drape or
    // cached flat picture keeps navigation and layer changes responsive while they finish.
    let tiled_flat = !globe_view && cam.bearing == 0.0 && cam.pitch == 0.0 && !app.session.map().tiles.is_empty();
    // A change to a tiled flat map's contents that leaves the same layers shown (a layer's colour
    // or transparency, its labels, an edit) keeps the frame on screen, labels and all, until the
    // exact frame arrives (#1051).
    let restyled =
        tiled_flat && (app.map.texture.is_some() || app.map.gpu_frame.is_some()) && keeps_frame(app.map.key, key, app.map.render_ms);
    // At most `KEEP_FRAME_S` from when the frame went out of date: the exact frame of a change
    // can take longer than the last one did. A frame shown meanwhile starts the wait again.
    if !restyled {
        app.map.kept = None;
    } else if app.map.kept.is_none_or(|(k, _)| Some(k) != app.map.key) {
        app.map.kept = app.map.key.map(|k| (k, time));
    }
    let waited = app.map.kept.map_or(0.0, |(_, at)| time - at);
    let keep_frame = restyled && waited <= KEEP_FRAME_S;
    if keep_frame {
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64((KEEP_FRAME_S - waited).max(0.0) + 0.01));
    }
    // Labels belong to the rendered contents. When visibility or data change, old labels
    // must disappear with their layers while a preview stands in for the exact frame.
    if app.map.key.is_none_or(|k| k.rev != key.rev) && !keep_frame {
        app.map.labels.clear();
        app.map.label_lonlat.clear();
        app.map.label_map.clear();
        app.map.callout_lonlat.clear();
        app.map.callout_map.clear();
    }
    let settling = (tilted || globe_view || tiled_flat) && !moving && app.ws.playing.is_none() && !hold;
    let mut tilt_draped = tilt_draped;
    // A frame finishing during a new gesture is obsolete. Drain it now so it neither holds
    // the worker slot nor keeps screenshot/idle waiters waiting for a frame we will not show.
    if !settling && let Some((_, done)) = app.map.tilt_settle.take() {
        app.session.map_mut().adopt_caches(&done.map);
    }
    // A GPU scene that re-projects (no pictures) of this map, at this size: a camera move needs
    // no picture drawn on the CPU meanwhile. One whose tile layers are all live, without labels
    // to place again, that still holds the view at about its zoom needs no new scene at all:
    // its tiles are placed for every frame (#635).
    let reprojects = gpu_ok
        && app.map.gpu_frame.as_ref().is_some_and(|f| !f.has_images && f.fits(&cam))
        && app.map.key.is_some_and(|k| k.rev == key.rev && k.size == key.size && k.sources == key.sources && !k.draft);
    let live_holds = reprojects
        && app.map.gpu_frame.as_ref().is_some_and(|f| f.tiles_live)
        && app.map.labels.is_empty()
        && scene_covers(app.map.tex_camera, cam, w, h)
        && (0.5..=2.0).contains(&(cam.res / app.map.tex_camera.res.max(1e-12)));
    if settling && live_holds && app.map.key != Some(key) {
        if let (Some(gf), Some(rs)) = (&app.map.gpu_frame, &app.map.render_state) {
            gf.set_camera(&rs.queue, &cam);
        }
        app.map.key = Some(key);
        if app.map.camera_moved {
            app.session.record_extent();
            app.map.camera_moved = false;
        }
    }
    if settling {
        let same_view = |a: Key, b: Key| Key { tiles: b.tiles, sources: b.sources, ..a } == b;
        if let Some((k, done)) = app.map.tilt_settle.take() {
            app.session.map_mut().adopt_caches(&done.map);
            // A frame of this view that couldn't be drawn isn't tried again: nothing is to fill.
            if same_view(k, key) && done.rendered.is_none() {
                app.map.filling = false;
            }
            // One drawn before tiles or data arrived still shows; the next follows. So does one
            // of a change made since, for a view whose frame on screen is older still.
            if (same_view(k, key) || (tiled_flat && shows_finished(k, key, app.map.key)))
                && let Some(r) = done.rendered
            {
                // With tile pictures still to draw, a draft: the next frame draws more.
                app.map.filling = done.incomplete;
                let k = Key { draft: k.draft || done.incomplete, ..k };
                app.map.render_ms = done.ms;
                app.map.motion_cost = full_cost(done.ms as f64, &done.view, (w, h), ppp);
                match r {
                    rustgis_engine::render::RenderOutput::Cpu(r) => {
                        app.map.gpu_frame = None;
                        show_frame(app, ui.ctx(), r, &done.view, (w, h), k, time, globe_view);
                    }
                    rustgis_engine::render::RenderOutput::Gpu(scene) => {
                        if !show_gpu_frame(app, scene, &done.view, k, time) {
                            // Device limits can decline a scene: compose the CPU fallback on
                            // the worker too, keeping oversized images off the UI thread.
                            app.map.tilt_settle.start(ui.ctx(), k, done.map, done.view);
                        }
                    }
                }
                if app.map.camera_moved {
                    app.session.record_extent();
                    app.map.camera_moved = false;
                }
            }
        }
        if app.map.key != Some(key) && !app.map.tilt_settle.busy() {
            let view = View { width: w, height: h, pixels_per_point: ppp as f64, camera: cam, sky: true };
            if tilted {
                rustgis_engine::render::sync_tilted_view(app.session.map_mut(), &view);
            }
            let backend = if gpu_ok { rustgis_engine::render::Backend::GpuLive } else { rustgis_engine::render::Backend::Cpu };
            app.map.tilt_settle.start_backend(ui.ctx(), key, app.session.map().clone(), view, backend);
        }
        let shown = (app.map.texture.is_some() || app.map.gpu_frame.is_some())
            && app.map.key.is_some_and(|k| same_view(Key { draft: false, ..k }, key));
        if !shown {
            let (drape_key, tiles) = drape_state(app);
            if globe_view {
                app.map.gpu_frame = None;
                draped = app.map.drape.prepare(ui.ctx(), app.session.map_mut(), drape_key, tiles, w, h, ppp, time);
            } else if tilted {
                tilt_draped = app.map.tilt_drape.prepare(ui.ctx(), app.session.map_mut(), drape_key, tiles, w, h, ppp, time);
            } else if app.map.preview_attempt != Some(key) && !reprojects && !(keep_frame && app.map.tilt_settle.busy()) {
                // A kept frame waits for the exact one; if that couldn't be drawn, a preview.
                // A small preview shows a visibility change immediately. The exact frame is
                // already on its way. Even a failed preview is attempted only once per key.
                app.map.preview_attempt = Some(key);
                let preview_ppp = f64::from(ppp.min(0.75)).min((524_288.0 / (w * h)).sqrt());
                let view = View { width: w, height: h, pixels_per_point: preview_ppp, camera: cam, sky: true };
                if let Some(r) = rustgis_engine::render::render(app.session.map_mut(), &view) {
                    app.map.gpu_frame = None;
                    show_frame(app, ui.ctx(), r, &view, (w, h), Key { draft: true, ..key }, time, false);
                }
            }
        }
    }
    // At rest, a turned map's drape is kept up to date off the UI thread: the next turn starts
    // on it.
    // (Not while an animation plays: the view moves every frame, past any drape drawn.)
    if turned && !moving && app.ws.playing.is_none() {
        let (drape_key, tiles) = drape_state(app);
        app.map.tilt_drape.warm(ui.ctx(), app.session.map_mut(), drape_key, tiles, w, h, ppp, time);
    }
    let interacting = moving && (!globe_view || draped) && (!tilted || tilt_draped);
    if (globe_view || tilted) && moving {
        // Wake up once the motion has settled, for the full-resolution frame.
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
    }
    if globe_view && !moving && app.map.key.is_some_and(|k| k.draft) {
        ui.ctx().request_repaint();
    }
    // GPU fast path: during pan/zoom (or a camera animation) with an all-GPU scene (no CPU
    // image runs — rasters and tiles don't re-project), only the camera changes, so rewrite
    // the per-draw uniforms instead of re-tessellating. `key` and `tex_camera` stay at the
    // last full render: the scene's contents (extent culling, the 0.35 px decimation, label
    // placement) are baked for that camera, so once the motion settles the still-stale key
    // makes the branch below do one full re-render. The meshes reach half a view past each
    // side (`GPU_CULL_MARGIN`); zooming out or panning further would show gaps where features
    // were left out, so then the branch below re-renders mid-motion instead.
    let stale = app.map.key != Some(key);
    // Image runs (tiles, rasters, CPU effects) move as one cached picture on a north-up
    // flat map. Re-uploading them every frame is much slower than shifting the GPU callback,
    // just as the CPU texture is shifted below. Turned/tilted scenes still re-project.
    let gpu_picture = gpu_ok && cam.bearing == 0.0 && cam.pitch == 0.0 && app.map.gpu_frame.as_ref().is_some_and(|f| f.has_images);
    // An asynchronous tiled preview is a CPU texture until its GPU scene arrives.
    let texture_motion = !gpu_ok || gpu_picture || (tiled_flat && app.map.gpu_frame.is_none());
    let has_picture = app.map.texture.is_some() || gpu_picture;
    let cam_only = app.map.key.is_some_and(|k| (Key { cam: k.cam, ..key }) == k);
    let mut fast_engaged = false;
    if stale
        && gpu_ok
        && (interacting || app.ws.playing.is_some())
        && cam_only
        && scene_covers(app.map.tex_camera, cam, w, h)
        && let (Some(gf), Some(rs)) = (&app.map.gpu_frame, &app.map.render_state)
        && !gf.has_images
        && gf.fits(&cam)
        // Tiles zoomed in past their own zoom blur, and zoomed out (no mipmaps) shimmer: the
        // tiles for the new zoom are placed, as often as a stretched CPU frame is sharpened below.
        && !(gf.has_tiles
            && !(app.map.tex_camera.res / MOTION_STRETCH..=app.map.tex_camera.res * TILE_SHRINK).contains(&cam.res)
            && time - app.map.last_render >= motion_gap(app.map.motion_cost))
    {
        gf.set_camera(&rs.queue, &cam);
        fast_engaged = true;
    }
    let stale = app.map.key != Some(key);
    // While the flat map moves on the CPU its last frame is shifted and scaled, until that no
    // longer covers the view (zooming out, panning past its edge) or stretches it noticeably
    // (zooming in). Then a frame is drawn mid-motion: for a gap, over 1.5 times the width and
    // height, so the next steps are covered too (as MapLibre keeps showing the tiles around the
    // view); when stretched, at the view. They are drawn at full resolution, as QGIS draws its
    // previews, unless that would take too long (see `motion_resolution`); sharpening ones are
    // spaced out by how long they take, so the motion between them stays smooth.
    // Turning the map isn't a shift and a scale: each step is drawn.
    // Nor is moving a tilted map (its perspective changes).
    // The tilted map's drape stands in for both.
    let turning =
        !globe_view && !tilt_draped && (app.map.tex_camera.bearing != cam.bearing || app.map.tex_camera.pitch != cam.pitch || tilted);
    let motion = (stale && interacting && texture_motion && !globe_view && !tilted && !turning && has_picture)
        .then(|| {
            let r = tex_rect(app, cam, rect, false);
            let stretch = r.width() as f64 / app.map.tex_size.0.max(1.0);
            let full = app.map.motion_cost;
            let (vw, vh, k) = if !r.expand(0.5).contains_rect(rect) {
                // A gap is filled at once.
                (w * GAP_SPAN, h * GAP_SPAN, motion_resolution(full, GAP_SPAN * GAP_SPAN))
            } else {
                let k = motion_resolution(full, 1.0);
                // Tile pictures still to draw are drawn as a stretched frame is sharpened.
                if (stretch <= MOTION_STRETCH && !app.map.filling) || time - app.map.last_render < motion_gap(full * k * k) {
                    return None;
                }
                (w, h, k)
            };
            Some((vw, vh, ppp * k as f32))
        })
        .flatten();
    let (vw, vh, vppp) = motion.unwrap_or((w, h, render_ppp));
    if stale
        && !fast_engaged
        && !settling
        && !hold
        && !draped
        && !tilt_draped
        && (motion.is_some()
            || (gpu_ok && !texture_motion)
            || turning
            || !interacting
            || !has_picture
            || app.map.key.is_some_and(|k| k.rev != key.rev || k.size != key.size))
    {
        let t0 = web_time::Instant::now();
        let view = View { width: vw, height: vh, pixels_per_point: vppp as f64, camera: cam, sky: true };
        let mut rendered_gpu = false;
        // Tile pictures already drawn stand in for missing ones mid-motion, and pictures are
        // drawn a frame's worth at a time: then the frame is a draft, drawn again.
        let mut lacks = None;
        if gpu_ok {
            if app.map.gpu_renderer.is_none()
                && let Some(rs) = &app.map.render_state
            {
                app.map.gpu_renderer = Some(std::sync::Arc::new(crate::mapgpu::GpuMapRenderer::new(rs)));
            }
            // A scene over the device's limits (huge buffers, oversized images) is declined
            // here too: the CPU block below renders the frame instead.
            let out = match (&app.map.gpu_renderer, &app.map.render_state) {
                (Some(_), Some(_)) => {
                    let (out, s) = drawing(interacting, || {
                        rustgis_engine::render::render_backend(app.session.map_mut(), &view, rustgis_engine::render::Backend::GpuLive)
                    });
                    lacks = s;
                    out
                }
                _ => None,
            };
            // A turned or tilted view the GPU declined comes back drawn on the CPU: shown, and
            // such views stay there until the map or its data change.
            let out = match out {
                Some(rustgis_engine::render::RenderOutput::Cpu(r)) if cam.bearing != 0.0 || cam.pitch != 0.0 => {
                    app.map.gpu_turn_declined = Some(rev);
                    app.map.gpu_frame = None;
                    show_frame(app, ui.ctx(), r, &view, (w, h), key, time, false);
                    rendered_gpu = true;
                    None
                }
                other => other,
            };
            if let Some(rustgis_engine::render::RenderOutput::Gpu(scene)) = out {
                let key = Key { draft: key.draft || motion.is_some() || lacks.is_some(), ..key };
                rendered_gpu = show_gpu_frame(app, scene, &view, key, time);
            }
        }
        // The CPU path, unchanged — also the fallback when the GPU backend declines a frame
        // (render_backend's own compatibility guard, or a scene over the device's limits), or
        // no render state arrived yet.
        if !rendered_gpu {
            // A turned or tilted scene the GPU couldn't take (over the device's limits) isn't
            // tried again until the map changes.
            if gpu_ok && (cam.bearing != 0.0 || cam.pitch != 0.0) {
                app.map.gpu_turn_declined = Some(rev);
            }
            app.map.gpu_frame = None;
            let view = View { width: vw, height: vh, pixels_per_point: vppp as f64, camera: cam, sky: true };
            let (r, s) = drawing(interacting, || rustgis_engine::render::render(app.session.map_mut(), &view));
            lacks = s;
            if let Some(r) = r {
                // Mid-motion the frame stands in for the real one, which follows once it settles.
                let key = Key { draft: key.draft || motion.is_some() || lacks.is_some(), ..key };
                show_frame(app, ui.ctx(), r, &view, (w, h), key, time, globe_view);
            }
        }
        app.map.filling = lacks == Some(Unfinished::Next);
        app.map.render_ms = t0.elapsed().as_millis() as u64;
        let view = View { width: vw, height: vh, pixels_per_point: vppp as f64, camera: cam, sky: true };
        app.map.motion_cost = full_cost(t0.elapsed().as_secs_f64() * 1000.0, &view, (w, h), ppp);
        if motion.is_some() {
            ui.ctx().request_repaint();
        } else if lacks == Some(Unfinished::Next) {
            // The next pictures.
            ui.ctx().request_repaint();
        } else if lacks.is_some() {
            // Once the motion has settled, for the exact frame.
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
        } else if app.map.camera_moved {
            app.session.record_extent();
            app.map.camera_moved = false;
        }
    } else if stale && !settling && !hold {
        ui.ctx().request_repaint();
    }

    // Draw the (possibly shifted) map texture.
    let painter = ui.painter_at(rect);
    // Behind the (possibly shifted or shrunk) last frame: the map's own background, so zooming
    // out never shows a band of window colour around it.
    let background = app.session.map().palette().0;
    if draped {
        app.map.drape.paint(ui.ctx(), &painter, rect, app.session.map(), ppp);
    } else if globe_view {
        app.map.drape.paint_space(ui.ctx(), &painter, rect, app.session.map(), ppp);
    } else if tilt_draped {
        app.map.tilt_drape.paint(&painter, rect, app.session.map());
    } else {
        painter.rect_filled(rect, 0.0, c32(background));
    }
    let old = app.map.tex_camera;
    let tex_rect = tex_rect(app, cam, rect, globe_view);
    app.map.tex_rect = tex_rect;
    // A GPU scene with image runs shifts/scales as a picture during north-up navigation;
    // vector-only scenes re-project in their shaders. Both draw inside egui's wgpu pass. The GPU
    // backend only serves flat maps, so the globe's drape never meets it.
    // Not under the tilted map's drape: a flat GPU frame from before the tilt would cover it.
    if let (Some(gf), Some(gr)) = (app.map.gpu_frame.as_ref().filter(|_| !tilt_draped), &app.map.gpu_renderer) {
        // A scene with pictures (or drawn for another size, a pane being dragged) is shown as a
        // picture of itself, shifted and scaled until it is drawn again.
        let picture = gpu_ok && cam.bearing == 0.0 && cam.pitch == 0.0 && (gf.has_images || gf.size != [w, h]);
        // Live tile layers, placed for this frame's camera; drawn again while tiles are uploaded.
        // In a picture, for the camera it was drawn for: the picture moves them with the rest (a
        // basemap zoomed in past what f32 holds is such a picture, under the tiles).
        let (live_frame, live_pending) = match gf.live_frame(if picture { app.map.tex_camera } else { cam }) {
            Some((f, pending, declined)) => {
                // A layer no longer drawn live (its style turned out to draw more than raster
                // tiles): the map is drawn again, as tiles arriving would draw it.
                if declined && app.map.live_rev.take().is_some() {
                    ui.ctx().request_repaint();
                }
                (Some(f), pending)
            }
            None => (None, false),
        };
        let backlog = live_frame.is_some() && gr.live_backlog.load(std::sync::atomic::Ordering::Relaxed);
        if backlog {
            ui.ctx().request_repaint();
        }
        // A scene whose tiles are all live is never drawn again for tiles arriving: whether the
        // map is still filling in is this frame's (tiles on their way, pictures to upload).
        if gf.tiles_live {
            app.map.filling = (live_pending || backlog) && !moving && app.ws.playing.is_none();
        }
        painter.add(egui_wgpu::Callback::new_paint_callback(
            rect,
            crate::mapgpu::SceneCallback {
                renderer: gr.clone(),
                buffers: gf.clone(),
                target: app.map.gpu_target.clone(),
                uv: if picture {
                    Rect::from_min_max(
                        ((rect.min - tex_rect.min) / tex_rect.size()).to_pos2(),
                        ((rect.max - tex_rect.min) / tex_rect.size()).to_pos2(),
                    )
                } else {
                    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0))
                },
                live: live_frame,
                repaint: Some(ui.ctx().clone()),
            },
        ));
    } else if let Some(tex) = app.map.texture.as_ref().filter(|_| !draped && !tilt_draped) {
        painter.image(tex.id(), tex_rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    }
    // Shooting stars, while space shows around the globe.
    let g = Globe::new(&cam, w, h);
    let (gx, gy) = g.center();
    // Outside the disc (an ellipse when the globe is tilted).
    let k = g.disc_height() / g.disc_radius().max(1e-9);
    let space = [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h)].iter().any(|&(x, y)| (x - gx).hypot((y - gy) / k) > g.disc_radius());
    if globe_view && space && app.session.map().view.effects.stars {
        let wait = app.map.comets.paint(&painter, rect, &g, time);
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(wait.clamp(0.0, 60.0)));
    } else {
        app.map.comets.clear();
    }
    // Labels stay on screen while the map moves, at their own size, as MapLibre keeps them
    // (rather than vanishing until it settles): on the flat map they move with the texture,
    // spreading apart or closing up as it zooms; on the globe each follows its place on the
    // Earth, and those turned behind the horizon drop out.
    let placed = app.map.label_camera;
    if globe_view && (placed.center != cam.center || placed.res != cam.res) {
        let g = Globe::new(&cam, w, h);
        let screen = |c: &Option<Coord>| c.and_then(|c| g.forward(c)).map(|(x, y)| rect.min + vec2(x as f32, y as f32));
        for (i, (l, c)) in app.map.labels.iter().zip(&app.map.label_lonlat).enumerate() {
            if let Some(at) = screen(c) {
                let feature = app.map.callout_lonlat.get(i).and_then(screen);
                draw_label(&painter, &callout_from(l, at, feature), at);
            }
        }
    } else if (tilt_draped || (app.map.gpu_frame.is_some() && !app.map.label_map.is_empty())) && placed != cam {
        // On a moving tilted (or, on the GPU, turned) map each label stays on its ground, turned
        // with it; under the sky (a tilted map's) it drops out.
        let sky = cam.sky_y(h);
        let (fw, fh) = app.map.tex_size;
        let at = |x: f32, y: f32| {
            let m = placed.try_to_map(f64::from(x), f64::from(y), fw, fh)?;
            cam.try_to_screen(m, w, h).map(|(x, y)| (x as f32, y as f32))
        };
        let turns = placed.bearing != cam.bearing || placed.pitch != cam.pitch;
        let screen = |m: &Option<Coord>| m.and_then(|m| cam.try_to_screen(m, w, h)).map(|(x, y)| rect.min + vec2(x as f32, y as f32));
        for (i, (l, m)) in app.map.labels.iter().zip(&app.map.label_map).enumerate() {
            if let Some(pos) = screen(m).filter(|p| sky.is_none_or(|sky| f64::from(p.y - rect.min.y) > sky)) {
                let moved = turns.then(|| {
                    let mut l = l.clone();
                    rustgis_engine::render::turn_label(&mut l, &at);
                    l
                });
                let feature = app.map.callout_map.get(i).and_then(screen);
                draw_label(&painter, &callout_from(moved.as_ref().unwrap_or(l), pos, feature), pos);
            }
        }
    } else {
        let s = if globe_view { 1.0 } else { (old.res / cam.res) as f32 };
        for l in &app.map.labels {
            let at = tex_rect.min + vec2(l.x, l.y) * s;
            // A callout's feature moves with the map, its text's end with the text.
            let feature = l.collide.callout.filter(|_| s != 1.0).map(|c| tex_rect.min + vec2(l.x + c[0], l.y + c[1]) * s);
            draw_label(&painter, &callout_from(l, at, feature), at);
        }
    }
    if app.map.hide_overlays {
        return;
    }
    overlays(app, ui, &painter, rect, &t);
    popup(app, ui, rect);
}

/// What a drape of the map shows besides the view (see `DrapeKey`), and the tiles come so far (0
/// for a map that fetches none): a change of either draws it again.
fn drape_state(app: &RustGisApp) -> (crate::globe_drape::DrapeKey, u64) {
    let key = crate::globe_drape::DrapeKey { rev: app.session.revision, sources: rustgis_engine::map::source::generation() };
    let tiles = if fetches_tiles(app.session.map()) { rustgis_engine::render::rustgis_tiles::cache::generation() } else { 0 };
    (key, tiles)
}

/// Whether the map draws from web tiles (its tile layers, or elevation tiles for its terrain):
/// tiles arriving draw it again.
fn fetches_tiles(m: &rustgis_engine::map::Map) -> bool {
    !m.tiles.is_empty()
        || m.view.terrain.as_ref().is_some_and(|t| matches!(t.source, rustgis_engine::map::view::TerrainSource::Tiles { .. }))
        || reads_in_place(m)
}

/// Whether the map has raster layers read in place: the part in view arrives a tile at a time,
/// through the tile cache, and the map is drawn again as it does.
fn reads_in_place(m: &rustgis_engine::map::Map) -> bool {
    m.layers.iter().any(|l| l.raster.as_ref().is_some_and(|r| r.in_place))
}

/// Uploads a composed GPU scene, whether built in place or by the frame worker.
fn show_gpu_frame(app: &mut RustGisApp, scene: rustgis_engine::render::gpu_scene::GpuScene, view: &View, key: Key, time: f64) -> bool {
    let Some(rs) = &app.map.render_state else { return false };
    let r = app.map.gpu_renderer.get_or_insert_with(|| std::sync::Arc::new(crate::mapgpu::GpuMapRenderer::new(rs)));
    let cam = view.camera;
    let Some(bufs) = crate::mapgpu::SceneBuffers::from_scene(
        &rs.device,
        &rs.queue,
        r,
        &scene,
        cam,
        [view.width, view.height],
        view.pixels_per_point as f32,
    ) else {
        return false;
    };
    app.map.live_rev = bufs.tiles_live.then_some(key.rev);
    app.map.gpu_frame = Some(std::sync::Arc::new(bufs));
    app.map.labels = scene.labels;
    app.map.labels.splice(0..0, scene.unplaced);
    app.map.label_lonlat.clear();
    // Where its labels stand: a turn starting from a north-up frame is draped too (on the CPU,
    // when the GPU doesn't turn the map), and they stay on their ground through it.
    app.map.callout_lonlat.clear();
    let to_map = |x: f32, y: f32| cam.try_to_map(x as f64, y as f64, view.width, view.height);
    app.map.label_map = app.map.labels.iter().map(|l| to_map(l.x, l.y)).collect();
    app.map.callout_map = app.map.labels.iter().map(|l| l.collide.callout.and_then(|c| to_map(l.x + c[0], l.y + c[1]))).collect();
    app.map.label_camera = cam;
    app.map.attribution = scene.attribution;
    app.map.last_render = time;
    for e in scene.tile_errors {
        if app.map.tile_errors.insert(e.clone()) {
            app.session.notify(e, true);
        }
    }
    app.map.tex_camera = cam;
    app.map.tex_size = (view.width, view.height);
    app.map.key = Some(key);
    true
}

/// Shows a frame the CPU drew for `view` (of a `w × h` map view) as the map's texture, with its
/// labels, credits and tile errors.
#[allow(clippy::too_many_arguments)]
fn show_frame(
    app: &mut RustGisApp,
    ctx: &egui::Context,
    r: Rendered,
    view: &View,
    (w, h): (f64, f64),
    key: Key,
    time: f64,
    globe_view: bool,
) {
    let cam = view.camera;
    let size = [r.pixmap.width() as usize, r.pixmap.height() as usize];
    let img = egui::ColorImage::from_rgba_premultiplied(size, r.pixmap.data());
    match &mut app.map.texture {
        Some(tex) => tex.set(img, egui::TextureOptions::LINEAR),
        None => app.map.texture = Some(ctx.load_texture("map", img, egui::TextureOptions::LINEAR)),
    }
    // A globe drawn roughly mid-motion keeps the last labels, which it would re-place
    // differently every frame.
    if !(globe_view && key.draft) {
        app.map.labels = r.labels;
        app.map.labels.splice(0..0, r.unplaced);
        (app.map.label_lonlat, app.map.callout_lonlat) = match globe_view {
            true => {
                let g = Globe::new(&cam, w, h);
                let at = |x: f32, y: f32| g.inverse(x as f64, y as f64);
                (
                    app.map.labels.iter().map(|l| at(l.x, l.y)).collect(),
                    app.map.labels.iter().map(|l| l.collide.callout.and_then(|c| at(l.x + c[0], l.y + c[1]))).collect(),
                )
            }
            false => (Vec::new(), Vec::new()),
        };
        // Where its labels stand while its drape moves (a north-up frame's too: a turn starting
        // from it is draped).
        (app.map.label_map, app.map.callout_map) = match !globe_view {
            true => {
                let to_map = |x: f32, y: f32| cam.try_to_map(x as f64, y as f64, view.width, view.height);
                (
                    app.map.labels.iter().map(|l| to_map(l.x, l.y)).collect(),
                    app.map.labels.iter().map(|l| l.collide.callout.and_then(|c| to_map(l.x + c[0], l.y + c[1]))).collect(),
                )
            }
            false => (Vec::new(), Vec::new()),
        };
        app.map.label_camera = cam;
    }
    app.map.attribution = r.attribution;
    app.map.last_render = time;
    for e in r.tile_errors {
        if app.map.tile_errors.insert(e.clone()) {
            app.session.notify(e, true);
        }
    }
    app.map.tex_camera = cam;
    app.map.tex_size = (view.width, view.height);
    app.map.key = Some(key);
}

/// How much wider and taller than the view a frame drawn mid-motion to fill a gap is, so the
/// next steps of zooming out or panning are covered too.
const GAP_SPAN: f64 = 1.5;

/// How long (s) the view's size holds before the map is drawn at it again (see `hold`).
const RESIZE_SETTLE: f64 = 0.15;

/// How long (ms) a frame drawn mid-motion may take before it is drawn coarser: about two
/// 60 Hz frames, so a pan or zoom that redraws stays smooth.
const MOTION_BUDGET_MS: f64 = 32.0;

/// The share of full resolution for a frame drawn mid-motion over `area` times the view, when
/// one at full resolution over the view takes `full_ms` to draw: full resolution while that
/// fits `MOTION_BUDGET_MS`, coarser (down to half) only for maps that draw slowly.
fn motion_resolution(full_ms: f64, area: f64) -> f64 {
    let cost = full_ms * area;
    if cost.is_nan() || cost <= MOTION_BUDGET_MS {
        return 1.0;
    }
    (MOTION_BUDGET_MS / cost).sqrt().clamp(0.5, 1.0)
}

/// What a frame drawn for `view` in `ms` would have cost at full resolution over the `w × h`
/// map view: drawing grows with its pixels.
fn full_cost(ms: f64, view: &View, (w, h): (f64, f64), ppp: f32) -> f64 {
    let pixels = (view.width * view.height) / (w * h).max(1.0) * (view.pixels_per_point / f64::from(ppp).max(0.01)).powi(2);
    ms / pixels.max(0.01)
}

/// The least time (s) between frames drawn mid-motion, for frames taking `ms` to draw: quick
/// ones as often as 30 a second, slower ones further apart so the motion between them stays
/// smooth.
fn motion_gap(ms: f64) -> f64 {
    let s = ms.min(300.0) / 1000.0;
    (if ms <= 60.0 { s * 1.5 } else { s * 3.0 }).max(1.0 / 30.0)
}

/// Draws with `f`, the tile pictures the map lacks drawn at the pace of a map `moving` or not
/// (see `rustgis_tiles::Pace`); and what the frame still lacks.
fn drawing<R>(moving: bool, f: impl FnOnce() -> R) -> (R, Option<Unfinished>) {
    use rustgis_engine::render::rustgis_tiles::{Pace, paced};
    paced(if moving { Pace::Moving } else { Pace::Settling }, f)
}

/// Where the map's last frame (drawn for `tex_camera`) lies on screen for the camera `cam`.
fn tex_rect(app: &RustGisApp, cam: Camera, rect: Rect, globe_view: bool) -> Rect {
    let (w, h) = (rect.width() as f64, rect.height() as f64);
    let old = app.map.tex_camera;
    if globe_view {
        // Where the old frame's centre now is, at the new zoom.
        let (old_g, new_g) = (Globe::new(&old, w, h), Globe::new(&cam, w, h));
        let s = (new_g.radius() / old_g.radius()) as f32;
        return match new_g.forward(old.center_lonlat()) {
            Some((x, y)) if s.is_finite() => Rect::from_center_size(pos2(rect.min.x + x as f32, rect.min.y + y as f32), rect.size() * s),
            _ => rect,
        };
    }
    // A tilted frame isn't shifted (its perspective moves with the camera; the drape stands in
    // while it moves): it fills the view.
    if old.pitch != 0.0 || cam.pitch != 0.0 {
        return rect;
    }
    let (tw, th) = if app.map.tex_size.0 > 0.0 { app.map.tex_size } else { (w, h) };
    // The frame's corners where they are now (at the same bearing the frame stays upright).
    let to_screen = |x: f64, y: f64| {
        let (sx, sy) = cam.to_screen(old.to_map(x, y, tw, th), w, h);
        pos2(rect.min.x + sx as f32, rect.min.y + sy as f32)
    };
    Rect::from_min_max(to_screen(0.0, 0.0), to_screen(tw, th))
}

/// Map text laid out as the map view draws it: its font (a label class's installed `family`, as
/// [`crate::labelfonts::family`] gives it, and whether egui slants it), and a text graphic's
/// style (#535), its lines aligned in their block, slanted (egui's own slant, as the interface
/// fonts have no italic face) and letter-spaced. The one layout for drawing a label and for
/// picking or dragging a text graphic, so they agree.
fn label_job(
    text: &str,
    size: f32,
    bold: bool,
    italic: bool,
    spacing: f32,
    align: TextAlign,
    family: Option<(egui::FontFamily, bool)>,
) -> egui::text::LayoutJob {
    let (font_id, italics) = match family {
        Some((family, slanted)) => (egui::FontId::new(size * 1.33, family), slanted),
        None => (if bold { semibold(size * 1.33) } else { regular(size * 1.33) }, italic),
    };
    // Letter spacing in points, scaled as the size is.
    let format = egui::TextFormat { font_id, color: Color32::WHITE, italics, extra_letter_spacing: spacing * 1.33, ..Default::default() };
    let mut job = egui::text::LayoutJob::single_section(text.to_owned(), format);
    job.halign = match align {
        TextAlign::Left => egui::Align::LEFT,
        TextAlign::Center => egui::Align::Center,
        TextAlign::Right => egui::Align::RIGHT,
    };
    job
}

/// Where a laid-out text is drawn from so that its block sits at `at` as `anchor` says, and the
/// block's rectangle: centred or right-aligned lines start left of the galley's origin.
fn place_galley(anchor: Align2, at: Pos2, g: &egui::Galley) -> (Pos2, Rect) {
    let r = anchor.anchor_size(at, g.size());
    (r.min - g.rect.min.to_vec2(), r)
}

/// A label being moved with the Move Label tool: as it was drawn when grabbed (in the view's
/// points), what it labels, and the map and label class it is of, named then (so it lands in
/// those, whatever is reordered or shown while it moves).
struct MovingLabel {
    label: PlacedLabel,
    layer: u32,
    feature: u64,
    class: String,
    project: u64,
    map: usize,
}

/// The layer's label drawn at a point of the map view (relative to it as `rect` is), topmost first:
/// what the Move Label tool grabs, as it is drawn now. Labels not of a layer's features aren't;
/// nor any while a moving globe or tilted map draws them from where they were placed.
fn label_at(app: &RustGisApp, rect: Rect, p: Pos2) -> Option<PlacedLabel> {
    label_at_where(app, rect, p, |_| true)
}

/// The topmost moved label under the pointer, even beneath one never moved (Show all labels).
fn pinned_label_at(app: &RustGisApp, rect: Rect, p: Pos2) -> Option<PlacedLabel> {
    label_at_where(app, rect, p, |l| l.collide.pinned)
}

/// The topmost label under the pointer that `keep` accepts.
fn label_at_where(app: &RustGisApp, rect: Rect, p: Pos2, keep: impl Fn(&PlacedLabel) -> bool) -> Option<PlacedLabel> {
    if app.map.key.is_none_or(|key| key.rev != app.session.revision || key.sources != rustgis_engine::map::source::generation()) {
        return None;
    }
    let cam = app.session.map().camera;
    let placed = app.map.label_camera;
    // A flat map moving draws the labels placed last with its frame: shifted, and spread apart
    // or closed up as it zooms (as `main_view` does).
    let (shift, s) = if placed == cam {
        (app.map.tex_rect.min - rect.min, 1.0)
    } else if app.session.map().projection != Projection::Globe && placed.pitch == 0.0 && cam.pitch == 0.0 && placed.bearing == cam.bearing
    {
        (app.map.tex_rect.min - rect.min, (app.map.tex_camera.res / cam.res) as f32)
    } else {
        return None;
    };
    if !(s.is_finite() && s > 0.0) {
        return None;
    }
    let at = p - rect.min;
    app.map.labels.iter().rev().filter(|l| l.collide.source.is_some() && !l.text.is_empty() && keep(l)).find_map(|l| {
        let mut l = l.clone();
        // Where it is drawn now: its text at its own size, its callout's feature where it is.
        (l.x, l.y) = (shift.x + l.x * s, shift.y + l.y * s);
        l.collide.callout_anchor = l.collide.callout_anchor.map(|[ax, ay]| [ax * s, ay * s]);
        l.collide.callout = l.collide.callout.map(|[x0, y0, x1, y1]| [x0 * s, y0 * s, x1, y1]);
        let e = rustgis_engine::render::label_extent(&l)?;
        let r = Rect::from_min_max(pos2(e[0], e[1]), pos2(e[2], e[3])).expand(3.0);
        r.contains(pos2(at.x, at.y)).then_some(l)
    })
}

/// A label drawn at `at` while the map moves, its callout's line starting at its feature where
/// that now is (none: as it was placed).
fn callout_from(l: &PlacedLabel, at: Pos2, feature: Option<Pos2>) -> std::borrow::Cow<'_, PlacedLabel> {
    match (l.collide.callout, feature) {
        (Some([_, _, x1, y1]), Some(f)) => {
            let mut l = l.clone();
            l.collide.callout = Some([f.x - at.x, f.y - at.y, x1, y1]);
            std::borrow::Cow::Owned(l)
        }
        _ => std::borrow::Cow::Borrowed(l),
    }
}

/// A label moved by `d` (points): its text there, its feature where it was, and its callout's line
/// from the feature to the text when its class has callouts.
fn moved_label(l: &PlacedLabel, d: egui::Vec2) -> PlacedLabel {
    let mut m = l.clone();
    m.x += d.x;
    m.y += d.y;
    m.collide.callout_anchor = l.collide.callout_anchor.map(|[ax, ay]| [ax - d.x, ay - d.y]);
    m.collide.callout = rustgis_engine::render::callout_to(&m);
    m
}

/// The name of a layer's label class by its place in the labeling order.
fn label_class_name(app: &RustGisApp, layer: u32, class: u32) -> Option<String> {
    app.session.map().layer(layer)?.labels.in_order().nth(class as usize).map(|c| c.name.clone())
}

/// Paints a label (on the map, or a moved one's preview), keeping callouts below text and curved
/// glyph halos below ink.
fn draw_label(p: &egui::Painter, l: &PlacedLabel, at: Pos2) {
    // A callout's line, from its feature to the edge of the text, under the text (its halo
    // under the line, so it shows over the map).
    if let Some([x0, y0, x1, y1]) = l.collide.callout {
        let ends = [at + vec2(x0, y0), at + vec2(x1, y1)];
        let width = rustgis_engine::render::CALLOUT_WIDTH;
        if let Some(h) = l.halo {
            p.line_segment(ends, Stroke::new(width + 2.0, crate::theme::c32(h)));
        }
        p.line_segment(ends, Stroke::new(width, crate::theme::c32(l.color)));
    }
    // Curved along a line: each character turned into its place, every character's halo first,
    // so none covers the letter before it.
    if !l.glyphs.is_empty() {
        let inks: &[(bool, bool)] = if l.halo.is_some() { &[(true, false), (false, true)] } else { &[(false, true)] };
        // (The label without its characters, copied for each: not the characters each time.)
        let base = PlacedLabel { text: String::new(), align: 0.0, glyphs: Vec::new(), ..l.clone() };
        for &(halo, text) in inks {
            for g in &l.glyphs {
                let one = PlacedLabel { text: g.text.clone(), angle: Some(g.angle), ..base.clone() };
                draw_label_ink(p, &one, at + vec2(g.dx, g.dy), halo, text);
            }
        }
        return;
    }
    draw_label_ink(p, l, at, true, true);
}

/// Draws a label (not a curved one) at `at`: its halo when `halo`, its text when `text`.
fn draw_label_ink(p: &egui::Painter, l: &PlacedLabel, at: Pos2, halo: bool, text: bool) {
    let family = crate::labelfonts::family(p.ctx(), l.font, l.bold, l.italic);
    let galley = p.layout_job(label_job(&l.text, l.size, l.bold, l.italic, l.spacing, l.line_align, family));
    // Text set beside an icon is lined up by its edge, with egui's own measure of its width.
    let anchor = if l.align < -0.25 {
        Align2::LEFT_CENTER
    } else if l.align > 0.25 {
        Align2::RIGHT_CENTER
    } else {
        Align2::CENTER_CENTER
    };
    // A label turned with the map (along a street): turned about its centre, at `at`.
    if let Some(angle) = l.angle.filter(|a| a.is_finite() && a.abs() > 1e-3) {
        let (pos, r) = place_galley(Align2::CENTER_CENTER, at, &galley);
        if halo {
            draw_background(p, l, r, angle);
        }
        let shape = |offset: egui::Vec2, color: Color32| {
            egui::epaint::TextShape::new(pos + offset, galley.clone(), color)
                .with_override_text_color(color)
                .with_angle_and_anchor(angle, Align2::CENTER_CENTER)
        };
        if let Some(h) = l.halo.filter(|h| halo && h.a() > 0) {
            let (offsets, hc) = halo_coats(l, h);
            for offset in offsets {
                p.add(shape(offset, hc));
            }
        }
        if text {
            p.add(shape(egui::Vec2::ZERO, c32(l.color)));
        }
        return;
    }
    let (pos, r) = place_galley(anchor, at, &galley);
    if halo {
        draw_background(p, l, r, 0.0);
    }
    // A selected text graphic: framed as laid out, under the text.
    if l.framed {
        let cyan = c32(rustgis_engine::render::SELECTION);
        p.rect(r.expand(3.0), 0.0, cyan.gamma_multiply(40.0 / 255.0), Stroke::new(2.0, cyan), egui::StrokeKind::Outside);
    }
    if let Some(h) = l.halo.filter(|h| halo && h.a() > 0) {
        let (offsets, hc) = halo_coats(l, h);
        for offset in offsets {
            p.galley_with_override_text_color(pos + offset, galley.clone(), hc);
        }
    }
    if text {
        p.galley_with_override_text_color(pos, galley, c32(l.color));
    }
}

/// Draws a label's background behind its text laid out in `text` (points), turned by `angle`
/// (radians, clockwise) about the text's centre: the rectangle grown by its padding, its corners
/// rounded, filled and outlined.
fn draw_background(p: &egui::Painter, l: &PlacedLabel, text: Rect, angle: f32) {
    if !l.glyphs.is_empty() {
        return;
    }
    let Some(b) = l.background.as_ref().filter(|b| b.draws()) else { return };
    let r = text.expand2(vec2(b.padding[0], b.padding[1]) * 1.33);
    let points = background_outline(r, b.radius * 1.33, angle);
    if points.len() < 3 {
        return;
    }
    let fill = b.fill.map_or(Color32::TRANSPARENT, c32);
    let stroke = match b.stroke {
        Some(c) if b.stroke_width > 0.0 => Stroke::new(b.stroke_width * 1.33, c32(c)),
        _ => Stroke::NONE,
    };
    p.add(egui::Shape::convex_polygon(points, fill, stroke));
}

/// A rectangle's outline with its corners rounded by `radius` (at most half its shorter side),
/// turned by `angle` about its centre: the corners as a few points each.
fn background_outline(r: Rect, radius: f32, angle: f32) -> Vec<Pos2> {
    if !(r.is_finite() && r.width() > 0.0 && r.height() > 0.0) {
        return Vec::new();
    }
    let radius = if radius.is_finite() { radius.clamp(0.0, r.width().min(r.height()) / 2.0) } else { 0.0 };
    let steps = if radius > 0.0 { 4 } else { 0 };
    // Each corner's centre and the angle its arc starts at, clockwise from the top left.
    let corners = [
        (pos2(r.min.x + radius, r.min.y + radius), std::f32::consts::PI),
        (pos2(r.max.x - radius, r.min.y + radius), -std::f32::consts::FRAC_PI_2),
        (pos2(r.max.x - radius, r.max.y - radius), 0.0),
        (pos2(r.min.x + radius, r.max.y - radius), std::f32::consts::FRAC_PI_2),
    ];
    let (sin, cos) = angle.sin_cos();
    let c = r.center();
    let turn = |q: Pos2| {
        let d = q - c;
        pos2(c.x + d.x * cos - d.y * sin, c.y + d.x * sin + d.y * cos)
    };
    let mut out = Vec::with_capacity(4 * (steps + 1));
    for (centre, start) in corners {
        for i in 0..=steps {
            let a = start + std::f32::consts::FRAC_PI_2 * i as f32 / steps.max(1) as f32;
            out.push(turn(centre + vec2(a.cos(), a.sin()) * radius));
        }
    }
    out
}

/// The most copies of a label's text a halo of its own width is drawn with.
const MAX_HALO_COATS: usize = 64;

/// Where a label's halo of colour `h` is drawn round its text (points), and in what colour each
/// copy: a thin ring, or for a halo of its own width rings about 1.5 points apart out to that
/// width (1.33 × in screen points, as the text's size), copies about a point apart round each
/// (at most [`MAX_HALO_COATS`] in all, shared among the rings by their length), in a see-through
/// colour shared among the rings. A halo of a few points (QGIS's usual buffer) is solid with few
/// copies; a very wide one is approximated, and a see-through one is about one coat (copies
/// overlap near the letters, so it is a little darker there): an export draws both exactly.
fn halo_coats(l: &PlacedLabel, h: rustgis_engine::map::Rgba) -> (Vec<egui::Vec2>, Color32) {
    let Some(width) = l.halo_width else {
        let ring = [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0), (-0.8, -0.8), (0.8, 0.8), (-0.8, 0.8), (0.8, -0.8)];
        return (ring.into_iter().map(|(x, y)| vec2(x, y)).collect(), c32(h));
    };
    let reach = rustgis_engine::render::halo_reach(width) * 1.33;
    // No width, or one that isn't a number: no halo.
    if reach.is_nan() || reach <= 0.0 {
        return (Vec::new(), c32(h));
    }
    let rings = ((reach / 1.5).ceil() as usize).clamp(1, 6);
    let step = reach / rings as f32;
    // Ring i (outermost first) has radius reach - i·step; copies go to each by its length.
    let radii: Vec<f32> = (0..rings).map(|i| reach - i as f32 * step).collect();
    let total: f32 = radii.iter().sum();
    let mut out = Vec::new();
    for r in radii {
        // A copy about a point apart round the ring, within its share of the budget (rounded down,
        // so the copies stay within it; the innermost of at most six rings still gets three).
        let share = ((MAX_HALO_COATS as f32 * r / total).floor() as usize).max(3);
        let n = ((std::f32::consts::TAU * r).ceil() as usize).clamp(3, share);
        out.extend((0..n).map(|k| {
            let (sin, cos) = (std::f32::consts::TAU * k as f32 / n as f32).sin_cos();
            vec2(r * cos, r * sin)
        }));
    }
    // About one copy of each ring covers a pixel of the halo.
    let a = f32::from(h.a()) / 255.0;
    let each = if a < 1.0 { 1.0 - (1.0 - a).powf(1.0 / rings as f32) } else { 1.0 };
    (out, c32(h.with_alpha((each * 255.0).round() as u8)))
}

/// The globe as seen in this view, when the map is a globe.
fn globe(app: &RustGisApp, rect: Rect) -> Option<Globe> {
    let m = app.session.map();
    (m.projection == Projection::Globe).then(|| Globe::new(&m.camera, rect.width() as f64, rect.height() as f64))
}

/// The longitude/latitude under a screen point (`None` off the globe).
pub(crate) fn screen_to_lonlat(app: &RustGisApp, rect: Rect, p: Pos2) -> Option<Coord> {
    let (w, h) = (rect.width() as f64, rect.height() as f64);
    let (x, y) = ((p.x - rect.min.x) as f64, (p.y - rect.min.y) as f64);
    match globe(app, rect) {
        Some(g) => g.inverse(x, y),
        // Nothing under the sky of a tilted map.
        None => app.session.map().to_lonlat(app.session.map().camera.try_to_map(x, y, w, h)?),
    }
}

/// The screen point of a longitude/latitude (`None` behind the globe).
pub(crate) fn lonlat_to_screen(app: &RustGisApp, rect: Rect, c: Coord) -> Option<Pos2> {
    let (w, h) = (rect.width() as f64, rect.height() as f64);
    let (x, y) = match globe(app, rect) {
        Some(g) => g.forward(c)?,
        None => app.session.map().camera.try_to_screen(app.session.map().to_map(c)?, w, h)?,
    };
    Some(pos2(rect.min.x + x as f32, rect.min.y + y as f32))
}

/// The text graphic drawn on top under the screen point `p`, as (layer id, row): one whose text,
/// with its dynamic values filled in as drawn, covers the point. A layer that isn't selectable
/// (`layer.selectable`) is passed over: its texts can't be picked, so can't be edited either.
pub(crate) fn text_graphic_at(app: &RustGisApp, ctx: &egui::Context, rect: Rect, p: Pos2) -> Option<(u32, usize)> {
    use rustgis_engine::map::graphics;
    let (map, scale) = (app.session.map(), app.session.scale());
    let tokens = graphics::TokenValues::of(map, scale);
    // Only what is drawn, as render_frame draws it: the first layer on top, and in a layer the
    // last row. Fully transparent layers and rows beyond the view clipping distance are not drawn.
    let clip = map.view.clip_distance.map(|d| (map.camera.center_lonlat(), d));
    map.layers.iter().filter(|l| l.graphics && l.selectable && map.draws_at(l, scale) && map.layer_opacity(l) > 0.0).find_map(|l| {
        let drawn = l.drawn_rows(scale);
        (0..l.dataset.features.len()).rev().filter(|r| drawn.get(*r) == Some(&true)).find_map(|row| {
            let g = l.dataset.features.get(row)?.geometry.as_ref()?;
            if let Some((centre, d)) = clip {
                let e = g.extent();
                let near = Coord::new(centre.x.clamp(e.min_x, e.max_x), centre.y.clamp(e.min_y, e.max_y));
                if e.is_empty() || rustgis_engine::geom::haversine_m(centre, near) > d {
                    return None;
                }
            }
            let (at, job) = text_shown(app, rect, &tokens, l, row)?;
            let size = ctx.fonts_mut(|f| f.layout_job(job).size());
            Rect::from_center_size(at, size).expand(3.0).contains(p).then_some((l.id, row))
        })
    })
}

/// A text graphic as the map shows it: the screen point its block is centred on, and its text
/// (the dynamic values filled in) laid out as [`draw_label`] lays it out, style and all.
fn text_shown(
    app: &RustGisApp,
    rect: Rect,
    tokens: &rustgis_engine::map::graphics::TokenValues,
    l: &rustgis_engine::map::Layer,
    row: usize,
) -> Option<(Pos2, egui::text::LayoutJob)> {
    use rustgis_engine::map::graphics;
    let ds = &l.dataset;
    let text = graphics::expand(graphics::label(ds, row)?, tokens);
    let at = lonlat_to_screen(app, rect, ds.features.get(row)?.geometry.as_ref()?.label_point()?)?;
    let job = label_job(
        &text,
        graphics::font_size(ds, row),
        graphics::bold(ds, row),
        graphics::italic(ds, row),
        graphics::letter_spacing(ds, row),
        graphics::align(ds, row),
        None,
    );
    Some((at, job))
}

/// Centres the map on a place as it moves along (a simulated drive), keeping the zoom; the map
/// treats it like a pan, shifting its last frame between redraws.
pub(crate) fn follow(app: &mut RustGisApp, c: Coord, time: f64) {
    let m = app.session.map_mut();
    if let Some(at) = m.to_map(c) {
        m.camera.center = at;
        app.map.last_interaction = time;
    }
}

/// Pans the flat map, or turns the globe, by a screen offset.
fn pan(app: &mut RustGisApp, dx: f64, dy: f64, w: f64, h: f64) {
    let m = app.session.map_mut();
    if m.projection == Projection::Globe {
        Globe::drag(&mut m.camera, dx, dy, w, h);
    } else {
        m.camera.pan_px(dx, dy);
    }
}

/// Turns the map or the globe by `degrees` about the view's centre.
fn turn_by(app: &mut RustGisApp, degrees: f64) {
    let cam = &mut app.session.map_mut().camera;
    let bearing = cam.bearing + degrees;
    cam.set_bearing(bearing);
}

/// Tilts the map or the globe by `degrees`.
fn tilt_by(app: &mut RustGisApp, degrees: f64) {
    if degrees != 0.0 {
        let cam = &mut app.session.map_mut().camera;
        let pitch = cam.pitch + degrees;
        cam.set_pitch(pitch);
    }
}

/// Zooms by `factor` about a screen point.
fn zoom(app: &mut RustGisApp, factor: f64, x: f64, y: f64, w: f64, h: f64) {
    let m = app.session.map_mut();
    if m.projection == Projection::Globe {
        Globe::zoom_at(&mut m.camera, factor, x, y, w, h);
    } else {
        m.camera.zoom_at(factor, x, y, w, h);
    }
}

fn selection_tool(tool: Tool) -> bool {
    matches!(tool, Tool::SelectPolygon | Tool::SelectLasso | Tool::SelectRadius)
}

fn cancel_changed_selection(app: &mut RustGisApp, rect: Rect) {
    if app.map.selection_gesture.as_ref().is_some_and(|g| {
        let map = app.session.map();
        // Polygon vertices are lon/lat and remain valid when the view moves between clicks.
        let view_bound = g.tool != Tool::SelectPolygon;
        g.tool != app.session.tool
            || g.map != app.session.project.active_map
            || g.projection != map.projection
            || (view_bound && (g.camera != map.camera || g.rect != rect))
    }) {
        app.map.cancel_sketch();
    }
}

fn begin_selection(app: &mut RustGisApp, rect: Rect) {
    if app.map.selection_gesture.is_none() {
        let map = app.session.map();
        app.map.selection_gesture = Some(SelectionGesture {
            tool: app.session.tool,
            map: app.session.project.active_map,
            camera: map.camera,
            projection: map.projection.clone(),
            rect,
        });
    }
}

fn selection_error(app: &mut RustGisApp, error: String) {
    app.map.cancel_sketch();
    app.session.notify(error, true);
}

fn invalid_selection() -> String {
    tl!("Selection outline is outside the supported map surface; draw a smaller area.").to_string()
}

fn selection_limit() -> String {
    tl!("Selection outline is too complex; draw a simpler area.").to_string()
}

fn selection_coord(app: &RustGisApp, rect: Rect, point: Pos2, flat: &Flat) -> Result<Coord, String> {
    if !rect.contains(point) {
        return Err(invalid_selection());
    }
    let coord = screen_to_lonlat(app, rect, point).ok_or_else(invalid_selection)?;
    flat.validate_selection_path(&[coord]).map_err(|_| invalid_selection())?;
    Ok(coord)
}

/// A globe's screen edges must be sampled before converting to the command's planar space.
fn append_selection_point(app: &RustGisApp, rect: Rect, points: &mut Vec<Coord>, end: Pos2) -> Result<(), String> {
    let flat = Flat::of(&app.session.map().projection);
    let start = points.last().and_then(|c| lonlat_to_screen(app, rect, *c));
    let samples = if app.session.map().projection == Projection::Globe {
        start.map_or(1, |p| ((end - p).length() / 3.0).ceil().max(1.0) as usize)
    } else {
        1
    };
    if samples > MAX_POINTS.saturating_sub(points.len()) {
        return Err(selection_limit());
    }
    for i in 1..=samples {
        let at = start.map_or(end, |p| p.lerp(end, i as f32 / samples as f32));
        let coord = selection_coord(app, rect, at, &flat)?;
        if let Some(last) = points.last() {
            flat.validate_selection_path(&[*last, coord]).map_err(|_| invalid_selection())?;
        }
        if let Some(first) = points.first() {
            flat.validate_selection_path(&[coord, *first]).map_err(|_| invalid_selection())?;
        }
        if points.last() != Some(&coord) {
            points.push(coord);
        }
    }
    Ok(())
}

fn add_selection_point(app: &mut RustGisApp, rect: Rect, point: Pos2, force: bool) -> Result<(), String> {
    let near = app.map.sketch.last().and_then(|c| lonlat_to_screen(app, rect, *c)).is_some_and(|p| (p - point).length() < 3.0);
    if near && !force {
        return selection_coord(app, rect, point, &Flat::of(&app.session.map().projection)).map(|_| ());
    }
    let mut points = std::mem::take(&mut app.map.sketch);
    let result = append_selection_point(app, rect, &mut points, point);
    app.map.sketch = points;
    result
}

fn selection_mode(ui: &Ui) -> &'static str {
    ui.input(|i| {
        if i.modifiers.shift {
            "add"
        } else if i.modifiers.ctrl || i.modifiers.command {
            "remove"
        } else {
            "new"
        }
    })
}

fn finish_area_selection(app: &mut RustGisApp, ui: &Ui, rect: Rect) {
    let mut points = std::mem::take(&mut app.map.sketch);
    let result = (|| {
        if let Some(first) = points.first().and_then(|c| lonlat_to_screen(app, rect, *c)) {
            append_selection_point(app, rect, &mut points, first)?;
        }
        Flat::of(&app.session.map().projection).validate_selection_path(&points).map_err(|_| invalid_selection())?;
        let coords: Vec<[f64; 2]> = points.iter().map(|c| [c.x, c.y]).collect();
        app.run("select.byPolygon", json!({"points": coords, "mode": selection_mode(ui)})).map(|_| ())
    })();
    app.map.cancel_sketch();
    if let Err(error) = result {
        app.session.notify(error, true);
    }
}

fn selection_interact(app: &mut RustGisApp, ui: &Ui, resp: &egui::Response, rect: Rect) {
    let tool = app.session.tool;
    // While a shortcut is being captured, Escape cancels the capture, not the sketch.
    if !app.ui.shortcut_editor.capturing() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        app.map.cancel_sketch();
        return;
    }
    if tool == Tool::SelectPolygon {
        if (resp.clicked() || resp.double_clicked())
            && let Some(point) = resp.interact_pointer_pos()
        {
            begin_selection(app, rect);
            let closes = app.map.sketch.len() >= 3
                && app.map.sketch.first().and_then(|c| lonlat_to_screen(app, rect, *c)).is_some_and(|first| (first - point).length() < 3.0);
            if closes {
                finish_area_selection(app, ui, rect);
            } else if let Err(error) = add_selection_point(app, rect, point, false) {
                selection_error(app, error);
            } else if resp.double_clicked() {
                finish_area_selection(app, ui, rect);
            }
        }
        return;
    }
    // Both drag tools consume ordinary clicks and double-clicks without identifying or zooming.
    if resp.double_clicked() {
        app.map.cancel_sketch();
        return;
    }
    if resp.drag_started_by(egui::PointerButton::Primary) {
        let origin = ui.input(|i| i.pointer.press_origin());
        app.map.cancel_sketch();
        if let Some(origin) = origin {
            begin_selection(app, rect);
            if let Err(error) = add_selection_point(app, rect, origin, true) {
                selection_error(app, error);
                return;
            }
            let kind = if tool == Tool::SelectLasso { DragKind::SelectLasso } else { DragKind::SelectRadius };
            app.map.drag = Some((origin, kind));
        }
    }
    if !app.map.drag.is_some_and(|(_, k)| matches!(k, DragKind::SelectLasso | DragKind::SelectRadius)) {
        return;
    }
    let end = resp.interact_pointer_pos().or_else(|| ui.input(|i| i.pointer.latest_pos()));
    if let Some(end) = end {
        let result = if tool == Tool::SelectLasso {
            add_selection_point(app, rect, end, resp.drag_stopped())
        } else {
            selection_coord(app, rect, end, &Flat::of(&app.session.map().projection)).map(|coord| {
                app.map.sketch.truncate(1);
                app.map.sketch.push(coord);
            })
        };
        if let Err(error) = result {
            selection_error(app, error);
            return;
        }
    }
    if resp.drag_stopped() {
        if tool == Tool::SelectLasso {
            finish_area_selection(app, ui, rect);
        } else {
            let ends = app.map.sketch.first().copied().zip(app.map.sketch.get(1).copied());
            if let Some((center, end)) = ends
                && let Some(radius) = radius_distance_m(center, end).filter(|r| *r > 0.0)
            {
                let valid = match &app.map.radius_preview {
                    Some((at, r, outline)) if *at == center && *r == radius => !outline.is_empty(),
                    _ => radius_outline(app, rect, center, radius).is_ok(),
                };
                if valid {
                    let _ =
                        app.run("select.byRadius", json!({"lon": center.x, "lat": center.y, "radius": radius, "mode": selection_mode(ui)}));
                } else {
                    app.session.notify(invalid_selection(), true);
                }
            }
            if let Some((center, end)) = ends
                && center != end
                && radius_distance_m(center, end).is_none()
            {
                app.session.notify(invalid_selection(), true);
            }
            app.map.cancel_sketch();
        }
    }
}

fn validate_radius(app: &RustGisApp, center: Coord, radius: f64) -> Result<(), String> {
    if !radius.is_finite() || radius <= 0.0 || radius > 5_000_000.0 {
        return Err(invalid_selection());
    }
    let mut extrema = [center; 4];
    for (i, point) in extrema.iter_mut().enumerate() {
        *point = radius_boundary_point(center, radius, i as f64 * std::f64::consts::FRAC_PI_2).ok_or_else(invalid_selection)?;
    }
    Flat::of(&app.session.map().projection).validate_selection_path(&extrema).map_err(|_| invalid_selection())
}

/// Subdivision is bounded to 64 * 2^8 intervals, independently of zoom or hostile input.
fn radius_outline(app: &RustGisApp, rect: Rect, center: Coord, radius: f64) -> Result<Vec<Option<Pos2>>, String> {
    validate_radius(app, center, radius)?;
    let flat = Flat::of(&app.session.map().projection);
    let point = |angle| -> Result<(Coord, Option<Pos2>), String> {
        let coord = radius_boundary_point(center, radius, angle).ok_or_else(invalid_selection)?;
        flat.validate_selection_path(&[coord]).map_err(|_| invalid_selection())?;
        let screen = lonlat_to_screen(app, rect, coord).filter(|p| p.x.is_finite() && p.y.is_finite() && rect.contains(*p));
        Ok((coord, screen))
    };
    fn interval(
        point: &impl Fn(f64) -> Result<(Coord, Option<Pos2>), String>,
        flat: &Flat,
        a: f64,
        b: f64,
        ends: [(Coord, Option<Pos2>); 2],
        depth: u8,
        out: &mut Vec<Option<Pos2>>,
    ) -> Result<(), String> {
        let mid = point((a + b) / 2.0)?;
        let refine = match (ends[0].1, mid.1, ends[1].1) {
            (Some(p), Some(m), Some(q)) => (m - p.lerp(q, 0.5)).length() > 0.75,
            (None, None, None) => false,
            _ => true,
        };
        if refine && depth < 8 {
            interval(point, flat, a, (a + b) / 2.0, [ends[0], mid], depth + 1, out)?;
            interval(point, flat, (a + b) / 2.0, b, [mid, ends[1]], depth + 1, out)?;
        } else {
            flat.validate_selection_path(&[ends[0].0, mid.0, ends[1].0]).map_err(|_| invalid_selection())?;
            if out.len() >= MAX_POINTS {
                return Err(selection_limit());
            }
            // Never bridge an unresolved horizon/viewport break at the subdivision limit.
            out.push(if mid.1.is_some() { ends[1].1 } else { None });
        }
        Ok(())
    }
    let mut out = Vec::with_capacity(65);
    let mut start = point(0.0)?;
    out.push(start.1);
    for i in 0..64 {
        let a = i as f64 * std::f64::consts::TAU / 64.0;
        let b = (i + 1) as f64 * std::f64::consts::TAU / 64.0;
        let end = point(b)?;
        interval(&point, &flat, a, b, [start, end], 0, &mut out)?;
        start = end;
    }
    Ok(out)
}

fn selection_readout(p: &egui::Painter, rect: Rect, t: &Tokens, text: String) {
    let galley = p.layout_no_wrap(text, semibold(12.5), t.text);
    let bg = Rect::from_center_size(pos2(rect.center().x, rect.min.y + 22.0), galley.size() + vec2(24.0, 12.0));
    p.rect_filled(bg, CornerRadius::same(14), t.menu);
    p.rect_stroke(bg, CornerRadius::same(14), Stroke::new(1.0, t.border_strong), egui::StrokeKind::Inside);
    p.galley(bg.center() - galley.size() / 2.0, galley, t.text);
}

fn selection_preview(app: &mut RustGisApp, p: &egui::Painter, rect: Rect, t: &Tokens) {
    let stroke = Stroke::new(2.0, t.accent);
    if app.session.tool == Tool::SelectRadius {
        if let Some((center, end)) = app.map.sketch.first().copied().zip(app.map.sketch.get(1).copied()) {
            if let Some(radius) = radius_distance_m(center, end).filter(|r| *r > 0.0) {
                if app.map.radius_preview.as_ref().is_none_or(|(at, r, _)| *at != center || *r != radius) {
                    let outline = radius_outline(app, rect, center, radius).unwrap_or_default();
                    app.map.radius_preview = Some((center, radius, outline));
                }
                if let Some((_, _, outline)) = &app.map.radius_preview {
                    for run in outline.split(|p| p.is_none()) {
                        let run: Vec<Pos2> = run.iter().flatten().copied().collect();
                        if run.len() >= 2 {
                            p.add(egui::Shape::line(run, stroke));
                        }
                    }
                    let text = if outline.is_empty() {
                        tl!("Invalid selection outline · draw a smaller radius").to_string()
                    } else {
                        crate::i18n::fmt(
                            "Radius {distance} (approximate)",
                            &[("distance", &crate::measure_units::distance(radius, app.ui.measurement_imperial))],
                        )
                    };
                    selection_readout(p, rect, t, text);
                }
            } else if center != end {
                selection_readout(p, rect, t, tl!("Invalid selection outline · draw a smaller radius").to_string());
            }
            if let (Some(a), Some(b)) = (lonlat_to_screen(app, rect, center), lonlat_to_screen(app, rect, end)) {
                p.line_segment([a, b], stroke);
                p.circle_filled(a, 3.5, t.accent);
            }
        }
        return;
    }
    // Only polygon has a live endpoint. The lasso already retained the pointer this frame,
    // so adding it again would create a duplicate release-to-first segment.
    let live =
        (app.session.tool == Tool::SelectPolygon).then_some(app.map.pointer_lonlat).flatten().filter(|c| app.map.sketch.last() != Some(c));
    let flat = Flat::of(&app.session.map().projection);
    let last = live.or_else(|| app.map.sketch.last().copied());
    let mut valid = true;
    if let (Some(first), Some(last)) = (app.map.sketch.first(), last) {
        valid = flat.validate_selection_path(&[last, *first]).is_ok();
        if let Some(live) = live {
            valid &= app.map.sketch.last().is_none_or(|last| flat.validate_selection_path(&[*last, live]).is_ok());
        }
        // The closing screen edge (and polygon's live edge) obey the same globe guard as commit.
        if app.session.map().projection == Projection::Globe {
            for (a, b) in app.map.sketch.last().copied().zip(live).into_iter().chain(Some((last, *first))) {
                let mut edge = vec![a];
                valid &= lonlat_to_screen(app, rect, b).is_some_and(|end| {
                    append_selection_point(app, rect, &mut edge, end).is_ok()
                        && edge.len().saturating_sub(1) <= MAX_POINTS.saturating_sub(app.map.sketch.len())
                });
            }
        }
    }
    let placed = app.map.sketch.iter().copied().chain(live).map(|c| lonlat_to_screen(app, rect, c).filter(|p| rect.contains(*p)));
    let mut first = None;
    let mut previous = None;
    for at in placed {
        if let Some(at) = at {
            if first.is_none() {
                first = Some(at);
            }
            if let Some(previous) = previous {
                p.line_segment([previous, at], stroke);
            }
            if app.session.tool == Tool::SelectPolygon {
                p.circle_filled(at, 3.5, Color32::WHITE);
                p.circle_stroke(at, 3.5, stroke);
            }
        }
        previous = at;
    }
    if valid {
        if let (Some(first), Some(last)) = (first, previous) {
            p.line_segment([last, first], stroke);
        }
    } else {
        selection_readout(p, rect, t, tl!("Invalid selection outline · draw a smaller area").to_string());
    }
}

fn interact(app: &mut RustGisApp, ui: &mut Ui, resp: &egui::Response, rect: Rect) {
    let (w, h) = (rect.width() as f64, rect.height() as f64);
    let time = now(ui);
    let hover = resp.hover_pos();
    app.map.pointer_lonlat = hover.and_then(|p| screen_to_lonlat(app, rect, p));
    if let Some(c) = app.map.pointer_lonlat.filter(|c| c.is_finite()) {
        app.map.kept_lonlat = Some((c, app.session.map().instance_id(), app.session.opened));
    }
    let tool = app.session.tool;
    cancel_changed_selection(app, rect);

    // A map tool aims with a crosshair, only over the map: the ribbon and panes keep their cursor.
    // A finished measurement keeps its points on show but waits for a new one.
    if hover.is_some() && tool != Tool::Explore && (app.map.sketch_len() == 0 || app.map.sketch_done) {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    // A place for Directions waits for a click.
    if hover.is_some() && app.ui.nav.picking.is_some() && app.ui.right == Some(crate::RightPane::Directions) {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    // So does a point for the NLDI.
    if hover.is_some() && crate::nldiui::wants_click(app) {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    // Wheel zoom about the pointer.
    if let Some(p) = hover {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y + i.zoom_delta().ln() * 300.0);
        if scroll.abs() > 0.1 {
            let f = (scroll as f64 * 0.0028).exp();
            zoom(app, f, (p.x - rect.min.x) as f64, (p.y - rect.min.y) as f64, w, h);
            app.map.last_interaction = time;
            app.map.camera_moved = true;
            app.map.popup = None;
        }
    }
    // First-person navigation: W A S D walk, Q and E fly up and down.
    if app.session.map().view.first_person && !app.ui.shortcut_editor.capturing() && !ui.ctx().egui_wants_keyboard_input() {
        let (mut fw, mut rt, mut up) = (0.0, 0.0, 0.0);
        ui.input(|i| {
            // Shortcuts (Ctrl+S, Ctrl+E…) aren't steps.
            if !i.modifiers.is_none() {
                return;
            }
            // Nine steps a second, whatever the frame rate.
            let step = (f64::from(i.stable_dt) * 9.0).clamp(0.0, 1.0);
            if i.key_down(egui::Key::W) {
                fw += step;
            }
            if i.key_down(egui::Key::S) {
                fw -= step;
            }
            if i.key_down(egui::Key::D) {
                rt += step;
            }
            if i.key_down(egui::Key::A) {
                rt -= step;
            }
            if i.key_down(egui::Key::E) {
                up += step;
            }
            if i.key_down(egui::Key::Q) {
                up -= step;
            }
        });
        if fw != 0.0 || rt != 0.0 || up != 0.0 {
            let _ = app.session.run("map.walk", &json!({"forward": fw, "right": rt, "up": up}));
            app.map.last_interaction = time;
            app.map.camera_moved = true;
            ui.ctx().request_repaint();
        }
    }
    // Arrow keys pan; Shift+Left and Shift+Right turn the map, Shift+Up and Shift+Down tilt it
    // (as in MapLibre). With Alt or Mod held the arrows are shortcuts (Alt+Shift+arrows move the
    // active layer in the Layers pane's outline, #836), not map moves.
    if !ui.ctx().egui_wants_keyboard_input() {
        let (mut dx, mut dy, mut turn, mut tilt) = (0.0, 0.0, 0.0, 0.0);
        ui.input(|i| {
            if i.modifiers.alt || i.modifiers.command {
                return;
            }
            // Ninety degrees a second, whatever the frame rate.
            let step = (f64::from(i.stable_dt) * 90.0).clamp(0.0, 15.0);
            if i.modifiers.shift {
                if i.key_down(egui::Key::ArrowLeft) {
                    turn -= step;
                }
                if i.key_down(egui::Key::ArrowRight) {
                    turn += step;
                }
                if i.key_down(egui::Key::ArrowUp) {
                    tilt += step / 2.0;
                }
                if i.key_down(egui::Key::ArrowDown) {
                    tilt -= step / 2.0;
                }
                return;
            }
            if i.key_down(egui::Key::ArrowLeft) {
                dx += 12.0;
            }
            if i.key_down(egui::Key::ArrowRight) {
                dx -= 12.0;
            }
            if i.key_down(egui::Key::ArrowUp) {
                dy += 12.0;
            }
            if i.key_down(egui::Key::ArrowDown) {
                dy -= 12.0;
            }
        });
        if dx != 0.0 || dy != 0.0 {
            pan(app, dx, dy, w, h);
            app.map.last_interaction = time;
            app.map.camera_moved = true;
        }
        if turn != 0.0 || tilt != 0.0 {
            turn_by(app, turn);
            tilt_by(app, tilt);
            app.map.last_interaction = time;
            app.map.camera_moved = true;
            ui.ctx().request_repaint();
        }
    }

    let shift = ui.input(|i| i.modifiers.shift);
    let middle = ui.input(|i| i.pointer.middle_down());
    let ctrl = ui.input(|i| i.modifiers.ctrl || i.modifiers.command);
    // Dragging with the right button turns the map or the globe (to the right turns it
    // clockwise) and tilts it (up tilts it further), as in MapLibre.
    if resp.dragged_by(egui::PointerButton::Secondary) {
        let d = resp.drag_delta();
        if d != egui::Vec2::ZERO {
            turn_by(app, d.x as f64 * 0.8);
            tilt_by(app, -d.y as f64 * 0.5);
            app.map.camera_moved = true;
            app.map.last_interaction = time;
        }
        cancel_changed_selection(app, rect);
        return;
    }
    cancel_changed_selection(app, rect);
    if selection_tool(tool) && !middle && !app.map.drag.is_some_and(|(_, k)| matches!(k, DragKind::Pan | DragKind::Turn)) {
        selection_interact(app, ui, resp, rect);
        return;
    }
    // Drag start.
    if resp.drag_started()
        && !ui.input(|i| i.pointer.secondary_down())
        && let Some(p) = resp.interact_pointer_pos()
    {
        let origin = ui.input(|i| i.pointer.press_origin()).unwrap_or(p);
        // The selected text, grabbed by its text, moves (#516); any other drag is as before.
        let grabbed = (matches!(tool, Tool::Explore | Tool::Select) && !middle && !shift && !ctrl)
            .then(|| text_graphic_at(app, ui.ctx(), rect, origin))
            .flatten()
            .filter(|hit| crate::graphicformat::selected(app) == Some(*hit));
        // With the Move Label tool, a layer's label grabbed by its text moves; elsewhere the map pans.
        let label = (tool == Tool::MoveLabel && !middle).then(|| label_at(app, rect, origin)).flatten();
        let kind = if let Some((layer, row)) = grabbed {
            DragKind::Text { layer, row, origin }
        } else if let Some(l) = label
            && let Some((layer, feature, class)) = l.collide.source
            && let Some(class) = label_class_name(app, layer, class)
        {
            let (project, map) = (app.session.opened, app.session.project.active_map);
            app.map.moving_label = Some(MovingLabel { label: l, layer, feature, class, project, map });
            DragKind::Label { origin }
        } else if middle {
            DragKind::Pan
        } else if tool == Tool::EditVertices && crate::vertexedit::drag_start(app, rect, origin) {
            DragKind::Vertex
        } else {
            match tool {
                Tool::Explore if shift => DragKind::Box,
                // Ctrl+drag turns and tilts the map, as the right button does.
                Tool::Explore if ctrl => DragKind::Turn,
                Tool::Explore => DragKind::Pan,
                Tool::Select | Tool::ZoomIn => DragKind::Box,
                t if drag_shape(t).is_some() => DragKind::Sketch,
                _ => DragKind::Pan,
            }
        };
        app.map.drag = Some((p, kind));
        app.map.popup = None;
        if kind == DragKind::Sketch {
            app.map.cancel_sketch();
            if let Some(c) = screen_to_lonlat(app, rect, p) {
                app.map.sketch.push(c);
            }
        }
    }
    // A dragged graphic follows the pointer: freehand shapes keep every point, the others their
    // two ends.
    if let (Some((_, DragKind::Sketch)), Some(p)) = (app.map.drag, resp.interact_pointer_pos())
        && let Some(c) = screen_to_lonlat(app, rect, p)
    {
        let freehand = matches!(tool, Tool::Graphic("freehand" | "lasso"));
        let far = app.map.sketch.last().and_then(|l| lonlat_to_screen(app, rect, *l)).is_none_or(|s| (s - p).length() > 3.0);
        if freehand {
            // Capped well below what a graphic may hold, so a very long drag still draws.
            if far && app.map.sketch.len() < 20_000 {
                app.map.sketch.push(c);
            }
        } else {
            app.map.sketch.truncate(1);
            app.map.sketch.push(c);
        }
    }
    // A label being moved shows where it will land, its callout's line following it; it is kept
    // there, as one undo step, on release.
    // Esc, another tool, or another map or project shown drops it where it was.
    if let Some((_, DragKind::Label { origin })) = app.map.drag {
        let same = app.map.moving_label.as_ref().is_some_and(|m| {
            m.project == app.session.opened && m.map == app.session.project.active_map && app.session.tool == Tool::MoveLabel
        });
        if !same || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            app.map.cancel_label_drag();
            return;
        }
        let end = resp.interact_pointer_pos().or_else(|| ui.input(|i| i.pointer.latest_pos()));
        let moved = app.map.moving_label.as_ref().zip(end).map(|(m, end)| moved_label(&m.label, end - origin));
        if resp.drag_stopped() {
            app.map.drag = None;
            let to = moved.as_ref().and_then(|l| screen_to_lonlat(app, rect, rect.min + vec2(l.x, l.y)));
            // Over a tilted map's sky or past the globe's edge there is no place to keep it at.
            if moved.is_some() && to.is_none() {
                app.session.notify(tl!("No place there to move the label to; it stays where it was."), true);
            }
            if let (Some(c), Some(m)) = (to, app.map.moving_label.take())
                && let Err(e) = app.run("labels.move", json!({"layer": m.layer, "class": m.class, "feature": m.feature, "to": [c.x, c.y]}))
            {
                app.session.notify(e, true);
            }
            app.map.moving_label = None;
            return;
        }
        if let Some(l) = moved {
            let t = Tokens::get(ui.ctx());
            let painter =
                ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, ui.scope_id().with("label_drag"))).with_clip_rect(rect);
            draw_label(&painter, &l, rect.min + vec2(l.x, l.y));
            if let Some(e) = rustgis_engine::render::label_extent(&l) {
                let r = Rect::from_min_max(rect.min + vec2(e[0], e[1]), rect.min + vec2(e[2], e[3]));
                painter.rect_stroke(r, 2.0, egui::Stroke::new(1.0, t.accent), egui::StrokeKind::Outside);
            }
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            ui.ctx().request_repaint();
        }
    }
    // A dragged text shows where it will land; it is placed there, as one undo step, on release.
    if let Some((_, DragKind::Text { layer, row, origin })) = app.map.drag {
        let end = resp.interact_pointer_pos().or_else(|| ui.input(|i| i.pointer.latest_pos()));
        let map = app.session.map();
        let tokens = rustgis_engine::map::graphics::TokenValues::of(map, app.session.scale());
        let shown = map.layer(layer).and_then(|l| text_shown(app, rect, &tokens, l, row));
        if resp.drag_stopped() {
            app.map.drag = None;
            let to = shown.zip(end).and_then(|((at, ..), end)| screen_to_lonlat(app, rect, at + (end - origin)));
            if let Some(c) = to {
                let _ = app.run("graphics.edit", json!({"layer": layer, "row": row, "points": [[c.x, c.y]]}));
            }
            return;
        }
        if let (Some((at, job)), Some(end)) = (shown, end) {
            let t = Tokens::get(ui.ctx());
            let painter =
                ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, ui.scope_id().with("text_drag"))).with_clip_rect(rect);
            // Laid out as the map draws it (its style too, #535), in the accent colour.
            let galley = painter.layout_job(job);
            let (pos, r) = place_galley(Align2::CENTER_CENTER, at + (end - origin), &galley);
            painter.galley_with_override_text_color(pos, galley, t.accent);
            painter.rect_stroke(r.expand(3.0), 2.0, egui::Stroke::new(1.0, t.accent), egui::StrokeKind::Outside);
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            ui.ctx().request_repaint();
        }
    }
    if let (Some((_, DragKind::Vertex)), Some(p)) = (app.map.drag, resp.interact_pointer_pos()) {
        crate::vertexedit::drag_to(app, rect, p);
        ui.ctx().request_repaint();
    }
    if resp.drag_stopped()
        && let Some((_, DragKind::Vertex)) = app.map.drag
    {
        app.map.drag = None;
        crate::vertexedit::drag_stop(app);
        return;
    }
    if let Some((_, DragKind::Turn)) = app.map.drag {
        let d = resp.drag_delta();
        if d != egui::Vec2::ZERO {
            turn_by(app, d.x as f64 * 0.8);
            tilt_by(app, -d.y as f64 * 0.5);
            app.map.camera_moved = true;
            app.map.last_interaction = time;
        }
    }
    if let Some((_, DragKind::Pan)) = app.map.drag {
        let d = resp.drag_delta();
        if d != egui::Vec2::ZERO {
            match (globe(app, rect), resp.interact_pointer_pos()) {
                // The globe turns so the grabbed point stays under the pointer.
                (Some(_), Some(p)) => {
                    let (x, y) = ((p.x - rect.min.x) as f64, (p.y - rect.min.y) as f64);
                    let m = app.session.map_mut();
                    Globe::drag_point(&mut m.camera, (x - d.x as f64, y - d.y as f64), (x, y), w, h);
                }
                // A tilted map moves so the grabbed ground stays under the pointer.
                (None, Some(p)) => {
                    let (x, y) = ((p.x - rect.min.x) as f64, (p.y - rect.min.y) as f64);
                    app.session.map_mut().camera.drag_px((x - d.x as f64, y - d.y as f64), (x, y), w, h);
                }
                _ => pan(app, d.x as f64, d.y as f64, w, h),
            }
            app.map.camera_moved = true;
            app.map.last_interaction = time;
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        }
    }
    cancel_changed_selection(app, rect);
    if resp.drag_stopped()
        && let Some((_, DragKind::Sketch)) = app.map.drag
    {
        app.map.drag = None;
        if let Some(shape) = drag_shape(tool) {
            let pts = std::mem::take(&mut app.map.sketch);
            if shape == "picture" {
                app.ui.pending_picture = Some(pts);
                let _ = app.run("ui.addPicture", Value::Null);
            } else {
                finish_graphic(app, shape, pts);
            }
        }
        return;
    }
    if resp.drag_stopped() {
        if let (Some((start, DragKind::Box)), Some(end)) = (app.map.drag, resp.interact_pointer_pos()) {
            // The box's extent from a grid of points inside it, so a box reaching off the globe (even
            // one whose corners all miss it) uses the part that is on it.
            let r = Rect::from_two_pos(start, end);
            let mut e = Extent::EMPTY;
            let n = 12;
            for i in 0..=n {
                for j in 0..=n {
                    let q = pos2(r.min.x + r.width() * i as f32 / n as f32, r.min.y + r.height() * j as f32 / n as f32);
                    if let Some(c) = screen_to_lonlat(app, rect, q) {
                        e.expand(c);
                    }
                }
            }
            if e.is_empty() {
                // Nothing on the globe.
            } else if tool == Tool::Select {
                let mode = ui.input(|i| {
                    if i.modifiers.shift {
                        "add"
                    } else if i.modifiers.command {
                        "remove"
                    } else {
                        "new"
                    }
                });
                let _ = app.run(
                    "select.byRect",
                    json!({"minLon": e.min_x, "minLat": e.min_y, "maxLon": e.max_x, "maxLat": e.max_y, "mode": mode}),
                );
            } else if (start - end).length() > 6.0 {
                app.session.zoom_to(&e);
                app.map.camera_moved = true;
            }
        }
        app.map.drag = None;
        app.map.last_interaction = time - 1.0;
    }

    // Clicks.
    if resp.double_clicked() {
        if matches!(tool, Tool::Measure | Tool::MeasureArea) || tool.draws_edit() || vertex_shape(tool) {
            finish_sketch(app);
        } else if tool == Tool::EditVertices {
            // Its two clicks chose a vertex or a feature; zooming would lose the place.
        } else if let Some(p) = resp.interact_pointer_pos() {
            // A double-click on a text graphic edits it in the Format pane; elsewhere it zooms in.
            if matches!(tool, Tool::Explore | Tool::Select)
                && let Some((layer, row)) = text_graphic_at(app, ui.ctx(), rect, p)
            {
                app.map.popup = None;
                let _ = crate::graphicformat::edit(app, layer, row);
                return;
            }
            zoom(app, 2.0, (p.x - rect.min.x) as f64, (p.y - rect.min.y) as f64, w, h);
            app.map.last_interaction = time;
            app.map.camera_moved = true;
            app.map.popup = None;
        }
        return;
    }
    if resp.clicked()
        && let Some(p) = resp.interact_pointer_pos()
    {
        let Some(c) = screen_to_lonlat(app, rect, p) else {
            // A click off the globe.
            app.map.popup = None;
            return;
        };
        // Directions waiting for a place, or a click on another route, takes the click.
        let nav = &app.ui.nav;
        let other = (nav.routes.len() > 1 && app.ui.right == Some(crate::RightPane::Directions))
            .then(|| {
                (0..nav.routes.len())
                    .filter(|&k| k != nav.chosen)
                    .find(|&k| nav.routes.get(k).is_some_and(|r| crate::navui::near(r, p, |c| lonlat_to_screen(app, rect, c))))
            })
            .flatten();
        if crate::navui::click(app, c, other) {
            return;
        }
        // The NLDI pane waiting for a point traces its flowline.
        if crate::nldiui::click(app, c) {
            return;
        }
        // A workflow waiting for map points takes the click.
        if let Some(pick) = &mut app.workflow.pick
            && pick.points.len() < pick.needed
        {
            pick.points.push(c);
            return;
        }
        if app.map.split_mode {
            app.map.split_mode = false;
            let _ = app.run("edit.split", json!({"lon": c.x, "lat": c.y}));
            return;
        }
        // A click anywhere on a text graphic's text selects it, not only on its anchor; on the
        // one already selected, it brings back the Format pane closed over it (#516). Shift and
        // Ctrl (Cmd) clicks add to or take from the selection as usual.
        if matches!(tool, Tool::Explore | Tool::Select)
            && !ui.input(|i| i.modifiers.shift || i.modifiers.command)
            && let Some((layer, row)) = text_graphic_at(app, ui.ctx(), rect, p)
        {
            app.map.popup = None;
            if crate::graphicformat::selected(app) == Some((layer, row)) {
                app.ui.open_right(crate::RightPane::Format);
            } else {
                let _ = app.run("select.rows", json!({"layer": layer, "rows": [row]}));
            }
            return;
        }
        // Sketch vertices snap to features when snapping is on.
        let c = if tool.creates() || tool.draws_edit() || matches!(tool, Tool::Graphic(_) | Tool::Measure | Tool::MeasureArea) {
            snapped(app, c)
        } else {
            c
        };
        match tool {
            // Labels move by dragging them; Shift-clicking a moved one puts it back.
            Tool::MoveLabel => {
                if shift
                    && let Some((layer, feature, class)) = pinned_label_at(app, rect, p).and_then(|l| l.collide.source)
                    && let Some(name) = label_class_name(app, layer, class)
                    && let Err(error) = app.run("labels.move", json!({"layer": layer, "class": name, "feature": feature, "to": null}))
                {
                    app.session.notify(error, true);
                }
            }
            Tool::Explore => {
                let r = app.session.run("identify", &json!({"lon": c.x, "lat": c.y, "tolerancePx": 5}));
                app.map.popup = match r {
                    Ok(v) if !v.is_null() => {
                        if let (Some(id), Some(row)) = (v["layerId"].as_u64(), v["row"].as_u64()) {
                            let _ = app.session.run("select.rows", &json!({"layer": id, "rows": [row]}));
                        } else if !is_found(&v) {
                            // Only WMS layers being asked: the click found nothing yet, so it clears
                            // the selection as an empty click does.
                            let _ = app.session.run("select.clear", &Value::Null);
                        }
                        Some(Popup::new(v, c, app.session.project.active_map, app.session.opened))
                    }
                    _ => {
                        let _ = app.session.run("select.clear", &Value::Null);
                        None
                    }
                };
            }
            Tool::Select => {
                let mode = ui.input(|i| {
                    if i.modifiers.shift {
                        "add"
                    } else if i.modifiers.command {
                        "remove"
                    } else {
                        "new"
                    }
                });
                let _ = app.run("select.atPoint", json!({"lon": c.x, "lat": c.y, "tolerancePx": 5, "mode": mode}));
            }
            Tool::SelectPolygon | Tool::SelectLasso | Tool::SelectRadius => {}
            Tool::ZoomIn => {
                zoom(app, 2.0, (p.x - rect.min.x) as f64, (p.y - rect.min.y) as f64, w, h);
                app.map.last_interaction = time;
                app.map.camera_moved = true;
            }
            Tool::EditVertices => crate::vertexedit::click(app, rect, p, c, shift),
            Tool::CreatePoint => {
                let _ = app.run("edit.addFeature", with_template(app, json!({"geometry": {"type": "Point", "coordinates": [c.x, c.y]}})));
            }
            Tool::Graphic("point") => finish_graphic(app, "point", vec![c]),
            // Placed, selected and open in the Format pane, as a layout's text (#516).
            Tool::Graphic("text") => {
                let text = app.ui.dynamic_text;
                if let Err(e) = crate::graphicformat::place(app, c.x, c.y, text) {
                    app.session.notify(e, true);
                }
            }
            // Dragged shapes ignore plain clicks.
            Tool::Graphic(s) if !vertex_shape(Tool::Graphic(s)) => {}
            Tool::Measure
            | Tool::MeasureArea
            | Tool::CreateLine
            | Tool::CreatePolygon
            | Tool::CutPolygon
            | Tool::Reshape
            | Tool::Graphic(_) => {
                if app.map.sketch_done {
                    app.map.cancel_sketch();
                }
                // A double-click arrives as two clicks first; ignore the duplicate vertex.
                let dup = app.map.sketch.last().and_then(|l| lonlat_to_screen(app, rect, *l)).is_some_and(|s| (s - p).length() < 3.0);
                if !dup {
                    add_vertex(app, tool, c);
                }
            }
        }
    }
    if tool == Tool::EditVertices
        && let Some(icon) = hover.and_then(|p| crate::vertexedit::cursor(app, rect, p))
    {
        ui.ctx().set_cursor_icon(icon);
    }
    if resp.secondary_clicked() {
        app.map.menu_lonlat = app.map.pointer_lonlat;
        app.map.menu_text = resp.interact_pointer_pos().and_then(|p| text_graphic_at(app, ui.ctx(), rect, p));
    }
    // No menu off the Earth (a globe's corners): there is no place for it to act on.
    if let Some(c) = app.map.menu_lonlat {
        resp.context_menu(|ui| crate::mapmenu::show(app, ui, c));
    }
    if hover.is_some() && (matches!(tool, Tool::Measure | Tool::MeasureArea) || tool.draws_edit()) && !app.map.sketch.is_empty() {
        ui.ctx().request_repaint();
    }
}

/// Finishes the sketch (a double-click): ends a measurement, adds the drawn feature, or runs Cut
/// Polygon or Reshape on the drawn line, then clears the sketch.
fn finish_sketch(app: &mut RustGisApp) {
    let tool = app.session.tool;
    let pts = app.map.sketch.clone();
    match tool {
        Tool::Measure | Tool::MeasureArea => app.map.sketch_done = true,
        Tool::CreateLine if pts.len() >= 2 => {
            let coords: Vec<[f64; 2]> = pts.iter().map(|c| [c.x, c.y]).collect();
            let _ = app.run("edit.addFeature", with_template(app, json!({"geometry": {"type": "LineString", "coordinates": coords}})));
            app.map.cancel_sketch();
        }
        Tool::CreatePolygon if pts.len() >= 3 => {
            let mut coords: Vec<[f64; 2]> = pts.iter().map(|c| [c.x, c.y]).collect();
            if let Some(f) = coords.first().copied() {
                coords.push(f);
            }
            let _ = app.run("edit.addFeature", with_template(app, json!({"geometry": {"type": "Polygon", "coordinates": [coords]}})));
            app.map.cancel_sketch();
        }
        Tool::CutPolygon | Tool::Reshape if pts.len() >= 2 => {
            let coords: Vec<[f64; 2]> = pts.iter().map(|c| [c.x, c.y]).collect();
            let id = if tool == Tool::CutPolygon { "edit.cutPolygon" } else { "edit.reshape" };
            let _ = app.run(id, json!({"line": {"type": "LineString", "coordinates": coords}}));
            app.map.cancel_sketch();
        }
        Tool::Graphic(shape) => {
            let min = if shape == "polygon" { 3 } else { 2 };
            if pts.len() >= min {
                finish_graphic(app, shape, pts);
            }
        }
        _ => {}
    }
}

/// Adds a clicked vertex to the sketch: along the edge from the last vertex when Trace finds one,
/// and never past the drawn-line limit of Cut Polygon and Reshape (a notice says so).
fn add_vertex(app: &mut RustGisApp, tool: Tool, c: Coord) {
    let limit = rustgis_engine::cmd::design::MAX_LINE;
    let capped = matches!(tool, Tool::CutPolygon | Tool::Reshape);
    if capped && app.map.sketch.len() >= limit {
        // Said once per sketch: further clicks on a full sketch add nothing, quietly.
        if !app.map.sketch_full_said {
            app.map.sketch_full_said = true;
            app.session.notify(tl!("Not added: a drawn line holds at most 10,000 points"), false);
        }
        return;
    }
    let last = app.map.sketch.last().copied().filter(|_| app.map.trace_edges && tool.draws_edit());
    match last.and_then(|from| traced(app, from, c)) {
        // The path starts where the last vertex meets the edge: it replaces it.
        Some(path) if !capped || app.map.sketch.len() - 1 + path.len() <= limit => {
            app.map.sketch.pop();
            app.map.sketch.extend(path);
        }
        Some(_) => {
            // Said once per sketch, like a full sketch.
            if !app.map.sketch_full_said {
                app.map.sketch_full_said = true;
                app.session.notify(tl!("Not traced: a drawn line holds at most 10,000 points"), false);
            }
            app.map.sketch.push(c);
        }
        None => app.map.sketch.push(c),
    }
}

/// The path along the edge under both the last sketch vertex and a new click (Trace), when there
/// is one: from where the last vertex meets the edge to the click. A click with no edge under
/// both points is an ordinary vertex: Trace only follows edges.
fn traced(app: &mut RustGisApp, from: Coord, to: Coord) -> Option<Vec<Coord>> {
    // Asked directly, not as a command: a click away from an edge isn't a failed command.
    let path = rustgis_engine::cmd::design::trace_between(&mut app.session, from, to)?;
    // Both clicks on the same point of the edge trace nothing: the click is an ordinary vertex.
    let apart = path.first().zip(path.last()).is_some_and(|(a, b)| a != b);
    (path.len() >= 2 && apart).then_some(path)
}

/// A point snapped to nearby features (when snapping is on).
fn snapped(app: &mut RustGisApp, c: Coord) -> Coord {
    if !app.session.map().editing.snapping.enabled {
        return c;
    }
    match app.session.run("edit.snap", &json!({"lon": c.x, "lat": c.y})) {
        Ok(v) => Coord::new(v["lon"].as_f64().unwrap_or(c.x), v["lat"].as_f64().unwrap_or(c.y)),
        Err(_) => c,
    }
}

/// Feature creation parameters with the Editing pane's chosen template, if it belongs to the layer.
fn with_template(app: &RustGisApp, mut p: Value) -> Value {
    let t = &app.forms.template;
    let has = app
        .session
        .active_layer
        .and_then(|id| app.session.map().layer(id))
        .is_some_and(|l| l.design.templates.iter().any(|x| x.name == *t));
    if has {
        p["template"] = json!(t);
    }
    p
}

/// Adds a graphic drawn with a tool, then returns to Explore unless the tool is kept.
pub fn finish_graphic(app: &mut RustGisApp, shape: &str, pts: Vec<Coord>) {
    let coords: Vec<[f64; 2]> = pts.iter().map(|c| [c.x, c.y]).collect();
    let ok = app.run("graphics.add", json!({"shape": shape, "points": coords})).is_ok();
    app.map.cancel_sketch();
    // A failed graphic (a click without a drag) keeps the tool; the error is in the notices.
    if ok && !app.session.keep_tool {
        app.session.tool = Tool::Explore;
    }
}

/// The credit in the map's corner: Natural Earth's where the built-in world outline is drawn, then
/// the sources drawn.
fn attribution(map: &rustgis_engine::map::Map, sources: &[String]) -> String {
    map.draws_world_outline()
        .then_some("Natural Earth")
        .into_iter()
        .chain(sources.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" | ")
}

/// Paints map decorations and interactive readouts, including measurements on the map's body.
fn overlays(app: &mut RustGisApp, ui: &mut Ui, p: &egui::Painter, rect: Rect, t: &Tokens) {
    let accent = t.accent;
    // Box drag.
    if let (Some((start, DragKind::Box)), Some(cur)) = (app.map.drag, ui.input(|i| i.pointer.latest_pos())) {
        let r = Rect::from_two_pos(start, cur);
        p.rect_filled(r, 0.0, accent.gamma_multiply(0.12));
        p.rect_stroke(r, 0.0, Stroke::new(1.5, accent), egui::StrokeKind::Middle);
    }
    // Vertices of the selected features while editing.
    let editing = &app.session.map().editing;
    if editing.settings.enabled
        && editing.settings.show_vertices
        && app.session.tool != Tool::EditVertices
        && (app.session.tool.creates() || app.session.tool.draws_edit() || app.ui.right == Some(crate::RightPane::Editing))
    {
        let mut verts: Vec<Coord> = Vec::new();
        for l in app.session.map().layers.iter().filter(|l| !l.selection.is_empty() && !l.is_raster() && !l.graphics) {
            for r in &l.selection {
                if let Some(g) = l.dataset.features.get(*r).and_then(|f| f.geometry.as_ref()) {
                    g.for_each_coord(|c| {
                        if verts.len() < 4000 {
                            verts.push(c);
                        }
                    });
                }
            }
        }
        for v in verts {
            if let Some(sp) = lonlat_to_screen(app, rect, v) {
                let r = Rect::from_center_size(sp, vec2(6.0, 6.0));
                p.rect_filled(r, 0.0, Color32::WHITE);
                p.rect_stroke(r, 0.0, Stroke::new(1.2, Color32::from_rgb(0x16, 0x9A, 0x5A)), egui::StrokeKind::Middle);
            }
        }
    }
    // The Edit Vertices tool's handles.
    if app.session.tool == Tool::EditVertices {
        crate::vertexedit::draw(app, p, rect, accent);
    }
    // The navigator: a compass, zoom buttons and a pan pad in the top-left corner.
    if app.session.map().view.navigator {
        navigator(app, ui, p, rect, t);
    }
    // First-person crosshair.
    if app.session.map().view.first_person {
        let c = rect.center();
        for (a, b) in [
            (vec2(-12.0, 0.0), vec2(-4.0, 0.0)),
            (vec2(4.0, 0.0), vec2(12.0, 0.0)),
            (vec2(0.0, -12.0), vec2(0.0, -4.0)),
            (vec2(0.0, 4.0), vec2(0.0, 12.0)),
        ] {
            p.line_segment([c + a, c + b], Stroke::new(2.0, Color32::from_black_alpha(160)));
        }
    }
    // The magnifier: twice the map under the pointer, in a lens beside it. It samples the CPU
    // texture, which GPU frames don't update — hide the lens while the GPU backend is drawing.
    if app.map.magnifier
        && app.map.gpu_frame.is_none()
        && let (Some(tex), Some(ptr)) = (&app.map.texture, ui.input(|i| i.pointer.hover_pos()).filter(|p| rect.contains(*p)))
    {
        let r = 70.0;
        let centre = pos2((ptr.x + r + 24.0).min(rect.max.x - r - 4.0), (ptr.y - r - 24.0).max(rect.min.y + r + 4.0));
        let tr = if app.map.tex_rect.is_positive() { app.map.tex_rect } else { rect };
        let uv_c = vec2((ptr.x - tr.min.x) / tr.width(), (ptr.y - tr.min.y) / tr.height());
        let half = vec2(r / 2.0 / tr.width(), r / 2.0 / tr.height());
        let mut mesh = egui::Mesh::with_texture(tex.id());
        let n = 48;
        let centre_uv = pos2(uv_c.x, uv_c.y);
        mesh.vertices.push(egui::epaint::Vertex { pos: centre, uv: centre_uv, color: Color32::WHITE });
        for k in 0..=n {
            let a = k as f32 / n as f32 * std::f32::consts::TAU;
            let (cs, sn) = (a.cos(), a.sin());
            mesh.vertices.push(egui::epaint::Vertex {
                pos: centre + vec2(cs, sn) * r,
                uv: pos2(uv_c.x + cs * half.x, uv_c.y + sn * half.y),
                color: Color32::WHITE,
            });
        }
        for k in 1..=n as u32 {
            mesh.indices.extend_from_slice(&[0, k, k + 1]);
        }
        p.add(egui::Shape::mesh(mesh));
        p.circle_stroke(centre, r, Stroke::new(2.5, t.accent));
        p.circle_stroke(ptr, 6.0, Stroke::new(1.5, t.accent));
    }
    // Directions: the routes, places and a simulated drive, while its pane is open (Save as
    // Layer keeps a route).
    if app.ui.right == Some(crate::RightPane::Directions) {
        crate::navui::paint(&app.ui.nav, p, rect, t, |c| lonlat_to_screen(app, rect, c));
    }
    // The NLDI's point, while its pane is open.
    crate::nldiui::paint(app, p, rect, t, |c| lonlat_to_screen(app, rect, c));
    // Points picked for a workflow.
    if let Some(pick) = &app.workflow.pick {
        let pts = pick.points.clone();
        for (i, c) in pts.iter().enumerate() {
            if let Some(sp) = lonlat_to_screen(app, rect, *c) {
                p.circle_filled(sp, 6.0, Color32::from_rgb(0xE5, 0x6A, 0x1A));
                p.circle_stroke(sp, 6.0, Stroke::new(1.5, Color32::WHITE));
                p.text(sp + vec2(9.0, -9.0), Align2::LEFT_BOTTOM, (i + 1).to_string(), semibold(11.0), Color32::from_rgb(0xE5, 0x6A, 0x1A));
            }
        }
    }
    // Where a click would snap to.
    if app.session.map().editing.snapping.enabled
        && (app.session.tool.creates()
            || app.session.tool.draws_edit()
            || matches!(app.session.tool, Tool::Graphic(_) | Tool::Measure | Tool::MeasureArea)
            || app.map.vertex_drag.is_some())
        && let Some(c) = app.map.pointer_lonlat
        && let Some(at) = {
            let cam = app.session.map().camera;
            let key = [c.x, c.y, cam.center.x + cam.center.y * 1e-3, cam.res, app.session.revision as f64];
            match (&app.map.vertex_drag, &app.map.snap) {
                // A dragged vertex has snapped already (leaving its own feature out).
                (Some(d), _) => d.snapped.then_some(d.at),
                (None, Some((k, at))) if *k == key => *at,
                _ => {
                    // Asked directly, not as a command: commands mark the map changed.
                    let at = rustgis_engine::cmd::design::snap_at(&mut app.session, c).map(|(at, _)| at);
                    app.map.snap = Some((key, at));
                    at
                }
            }
        }
        && let Some(sp) = lonlat_to_screen(app, rect, at)
    {
        let magenta = Color32::from_rgb(0xD0, 0x2A, 0xB0);
        p.circle_stroke(sp, 7.0, Stroke::new(2.0, magenta));
        p.circle_filled(sp, 2.5, magenta);
    }
    // A dragged rectangle, circle or ellipse previews as its outline.
    let tool = app.session.tool;
    let mut previewed = false;
    if selection_tool(tool) && !app.map.sketch.is_empty() {
        selection_preview(app, p, rect, t);
        previewed = true;
    }
    if !app.map.sketch.is_empty()
        && let Some(shape) = drag_shape(tool).filter(|s| !matches!(*s, "freehand" | "lasso"))
    {
        // Rectangles, circles and ellipses preview as their outline between the two points.
        if let (Some(a), Some(b)) = (
            app.map.sketch.first().and_then(|c| lonlat_to_screen(app, rect, *c)),
            app.map.sketch.last().and_then(|c| lonlat_to_screen(app, rect, *c)),
        ) {
            let green = Color32::from_rgb(0x16, 0x9A, 0x5A);
            let stroke = Stroke::new(2.0, green);
            match shape {
                "circle" => {
                    p.circle_stroke(a, (b - a).length(), stroke);
                }
                "ellipse" => {
                    let r = Rect::from_two_pos(a, b);
                    let pts: Vec<Pos2> = (0..72)
                        .map(|i| {
                            let t = i as f32 / 72.0 * std::f32::consts::TAU;
                            pos2(r.center().x + r.width() / 2.0 * t.cos(), r.center().y + r.height() / 2.0 * t.sin())
                        })
                        .collect();
                    p.add(egui::Shape::closed_line(pts, stroke));
                }
                _ => {
                    p.rect_stroke(Rect::from_two_pos(a, b), 0.0, stroke, egui::StrokeKind::Middle);
                }
            }
        }
        previewed = true;
    }
    // Sketch.
    if !app.map.sketch.is_empty() && !previewed {
        let mut pts: Vec<Coord> = app.map.sketch.clone();
        if !app.map.sketch_done
            && let Some(c) = app.map.pointer_lonlat
        {
            pts.push(c);
        }
        let closed = matches!(tool, Tool::MeasureArea | Tool::CreatePolygon | Tool::Graphic("polygon" | "lasso"));
        let placed: Vec<Option<Pos2>> = pts.iter().map(|c| lonlat_to_screen(app, rect, *c)).collect();
        let screen: Vec<Pos2> = placed.iter().flatten().copied().collect();
        let color =
            if tool.creates() || tool.draws_edit() { Color32::from_rgb(0x16, 0x9A, 0x5A) } else { Color32::from_rgb(0xE5, 0x6A, 0x1A) };
        let all_visible = screen.len() == placed.len();
        if closed && all_visible && screen.len() >= 3 {
            p.add(egui::Shape::closed_line(screen.clone(), Stroke::new(2.0, color)));
        } else {
            // On the globe a vertex behind the horizon breaks the outline rather than joining its
            // neighbours across the disc.
            for run in placed.split(|s| s.is_none()) {
                let run: Vec<Pos2> = run.iter().flatten().copied().collect();
                if run.len() >= 2 {
                    p.add(egui::Shape::line(run, Stroke::new(2.0, color)));
                }
            }
        }
        for s in &screen {
            p.circle_filled(*s, 3.5, Color32::WHITE);
            p.circle_stroke(*s, 3.5, Stroke::new(1.5, color));
        }
        if matches!(tool, Tool::Measure | Tool::MeasureArea) && pts.len() >= 2 {
            // On the map's body: the Earth measures on the WGS 84 ellipsoid, so the panel agrees
            // with the geoprocessing tools (#939); another body measures on its sphere and scales.
            let body = app.session.map().body();
            let measure_length = |pts: &[Coord]| {
                if body.is_earth() { wgs84_length_m(pts) } else { body.length(geodesic_length_m(pts)) }
            };
            // Perimeter and area in the map body's measurement model.
            let measure_ring = |ring: &[Coord]| {
                if body.is_earth() {
                    wgs84_ring_perimeter_area_m2(ring)
                } else {
                    (body.length(geodesic_ring_perimeter_m(ring)), body.area(geodesic_ring_area_m2(ring)))
                }
            };
            let text = if tool == Tool::Measure {
                let seg = pts.len().checked_sub(2).and_then(|i| pts.get(i..)).map(measure_length).unwrap_or(0.0);
                crate::i18n::fmt(
                    "Total {total}   ·   Segment {segment}",
                    &[
                        ("total", &crate::measure_units::distance(measure_length(&pts), app.ui.measurement_imperial)),
                        ("segment", &crate::measure_units::distance(seg, app.ui.measurement_imperial)),
                    ],
                )
            } else {
                let mut ring = pts.clone();
                if let Some(f) = ring.first().copied() {
                    ring.push(f);
                }
                let (perimeter, area) = measure_ring(&ring);
                crate::i18n::fmt(
                    "Area {area}   ·   Perimeter {perimeter}",
                    &[
                        ("area", &crate::measure_units::area(area, app.ui.measurement_imperial)),
                        ("perimeter", &crate::measure_units::distance(perimeter, app.ui.measurement_imperial)),
                    ],
                )
            };
            let anchor = pos2(rect.center().x, rect.min.y + 22.0);
            let galley = p.layout_no_wrap(text, semibold(12.5), t.text);
            let bg = Rect::from_center_size(anchor, galley.size() + vec2(24.0, 12.0));
            p.rect_filled(bg, CornerRadius::same(14), t.menu);
            p.rect_stroke(bg, CornerRadius::same(14), Stroke::new(1.0, t.border_strong), egui::StrokeKind::Inside);
            p.galley(bg.center() - galley.size() / 2.0, galley, t.text);
        }
    }
    // Hint for the active tool.
    let hint = match app.session.tool {
        Tool::Measure => Some("Click to add points · double-click to finish · Esc to stop"),
        Tool::MeasureArea => Some("Click the corners of the area · double-click to finish"),
        Tool::CreatePoint => Some("Click the map to add a point"),
        Tool::CreateLine => Some("Click to add vertices · double-click to finish the line"),
        Tool::CreatePolygon => Some("Click to add vertices · double-click to close the polygon"),
        Tool::CutPolygon => Some("Draw a line across the selected polygons · double-click to cut them"),
        Tool::Reshape => Some("Draw a line across the selected feature's edge · double-click to reshape it"),
        Tool::Select => Some("Click or drag a box · Shift adds · Ctrl removes"),
        Tool::SelectPolygon => {
            Some("Click vertices · click the start or double-click to select · Shift adds · Ctrl/Cmd removes · Esc cancels")
        }
        Tool::SelectLasso => Some("Drag an area · release to select · Shift adds · Ctrl/Cmd removes · Esc cancels"),
        Tool::SelectRadius => Some("Drag from the centre · release to select · Shift adds · Ctrl/Cmd removes · Esc cancels"),
        Tool::EditVertices if app.map.vertex.is_some() => {
            Some("Drag a vertex to move it · click an edge to add one · Delete removes the chosen vertex")
        }
        Tool::EditVertices => Some("Click a feature, then drag its vertices · click an edge to add one"),
        Tool::ZoomIn => Some("Drag a box or click to zoom in"),
        Tool::MoveLabel => Some("Drag a label to where it should be · drag the map to pan · Esc to stop"),
        Tool::Graphic("point") => Some("Click the map to place a point"),
        Tool::Graphic("text") => Some("Click the map to place text"),
        Tool::Graphic("line" | "curve") => Some("Click to add vertices · double-click to finish"),
        Tool::Graphic("polygon") => Some("Click to add vertices · double-click to close"),
        Tool::Graphic("freehand" | "lasso") => Some("Drag to draw"),
        Tool::Graphic("circle") => Some("Drag from the centre out"),
        Tool::Graphic(_) => Some("Drag a box"),
        Tool::Explore => None,
    };
    if let Some(h) = hint
        && (app.map.sketch.is_empty() || !matches!(app.session.tool, Tool::Measure | Tool::MeasureArea))
    {
        let g = p.layout_no_wrap(tl!(h).to_string(), regular(12.0), Color32::WHITE);
        let bg = Rect::from_center_size(pos2(rect.center().x, rect.max.y - 26.0), g.size() + vec2(22.0, 10.0));
        p.rect_filled(bg, CornerRadius::same(12), Color32::from_black_alpha(160));
        p.galley(bg.center() - g.size() / 2.0, g, Color32::WHITE);
    }
    // The place or coordinate the search box went to.
    if let Some((c, name)) = &app.ui.search.marker
        && let Some(sp) = lonlat_to_screen(app, rect, *c)
        && rect.contains(sp)
    {
        crate::searchbox::paint_marker(p, sp, name, t);
    }
    scale_bar(app, p, rect, t);
    // The arrow points north, leaning back with the map's tilt; on a turned or tilted map clicking
    // it faces north and looks straight down again.
    let cam = app.session.map().camera;
    let (bearing, pitch) = (cam.bearing, cam.pitch);
    // Facing north the corner stays the map's (identify, select and sketch clicks land there).
    if bearing != 0.0 || pitch != 0.0 {
        let hit = ui
            .interact(north_arrow_rect(rect), egui::Id::unique("map_north_arrow"), Sense::click())
            .on_hover_text(tl!("Reset north"))
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if hit.clicked() {
            let _ = app.run("map.resetNorth", json!({}));
        }
    }
    north_arrow(p, rect, t, bearing, pitch);
    // Attribution.
    let att = attribution(app.session.map(), &app.map.attribution);
    if !att.is_empty() {
        let g = p.layout(att, regular(10.5), t.text_dim, (rect.width() - 240.0).max(120.0));
        let r = Rect::from_min_size(pos2(rect.max.x - g.size().x - 14.0, rect.max.y - g.size().y - 6.0), g.size() + vec2(10.0, 4.0));
        p.rect_filled(r, CornerRadius::same(3), t.ribbon.gamma_multiply(0.75));
        p.galley(r.min + vec2(5.0, 2.0), g, t.text_dim);
    }
    // Navigation buttons (zoom in/out/full) in the top-right corner.
    let mut y = rect.min.y + 12.0;
    for (icon, id, tip) in
        [("zoom_in", "map.zoomIn", "Zoom in"), ("zoom_out", "map.zoomOut", "Zoom out"), ("zoom_full", "map.zoomFull", "Full extent")]
    {
        let r = Rect::from_min_size(pos2(rect.max.x - 44.0, y), vec2(32.0, 32.0));
        let resp = ui.interact(r, egui::Id::unique(("nav", id)), Sense::click());
        p.rect_filled(r, CornerRadius::same(8), if resp.hovered() { t.hover } else { t.ribbon });
        p.rect_stroke(r, CornerRadius::same(8), Stroke::new(1.0, t.border_strong), egui::StrokeKind::Inside);
        icons::paint(p, r.shrink(7.0), icon, t.icon, t.accent);
        if resp.on_hover_text(tl!(tip)).clicked() {
            let _ = app.run(id, Value::Null);
            app.map.popup = None;
        }
        y += 38.0;
    }
}

fn scale_bar(app: &RustGisApp, p: &egui::Painter, rect: Rect, t: &Tokens) {
    let m_per_pt = app.session.map().ground_res();
    let d = crate::measure_units::scale_distance(m_per_pt * 130.0, app.ui.measurement_imperial);
    if d <= 0.0 {
        return;
    }
    let len = (d / m_per_pt) as f32;
    let x0 = rect.min.x + 18.0;
    let y0 = rect.max.y - 22.0;
    let bg = Rect::from_min_max(pos2(x0 - 8.0, y0 - 20.0), pos2(x0 + len + 10.0, y0 + 10.0));
    p.rect_filled(bg, CornerRadius::same(5), t.ribbon.gamma_multiply(0.85));
    for i in 0..4 {
        let seg = Rect::from_min_size(pos2(x0 + len * i as f32 / 4.0, y0), vec2(len / 4.0, 5.0));
        p.rect_filled(seg, 0.0, if i % 2 == 0 { t.text } else { t.ribbon });
    }
    p.rect_stroke(Rect::from_min_size(pos2(x0, y0), vec2(len, 5.0)), 0.0, Stroke::new(1.0, t.text), egui::StrokeKind::Middle);
    p.text(pos2(x0, y0 - 3.0), Align2::LEFT_BOTTOM, "0", regular(10.5), t.text);
    p.text(
        pos2(x0 + len, y0 - 3.0),
        Align2::RIGHT_BOTTOM,
        crate::measure_units::distance(d, app.ui.measurement_imperial),
        regular(10.5),
        t.text,
    );
}

fn north_arrow_rect(rect: Rect) -> Rect {
    Rect::from_center_size(pos2(rect.min.x + 30.0, rect.min.y + 34.0), vec2(34.0, 34.0))
}

/// The north arrow, turned to point north on a map facing `bearing` and foreshortened by its
/// `pitch`.
fn north_arrow(p: &egui::Painter, rect: Rect, t: &Tokens, bearing: f64, pitch: f64) {
    let c = north_arrow_rect(rect).center();
    let (sn, cs) = (-bearing.to_radians() as f32).sin_cos();
    let k = (pitch.to_radians().cos() as f32).max(0.3);
    let at = |x: f32, y: f32| {
        let (x, y) = (x * cs - y * sn, x * sn + y * cs);
        c + vec2(x, y * k)
    };
    p.circle_filled(c, 17.0, t.ribbon.gamma_multiply(0.85));
    p.circle_stroke(c, 17.0, Stroke::new(1.0, t.border_strong));
    p.add(egui::Shape::convex_polygon(vec![at(0.0, -12.0), at(6.0, 6.0), at(0.0, 2.5)], t.text, Stroke::NONE));
    p.add(egui::Shape::convex_polygon(vec![at(0.0, -12.0), at(0.0, 2.5), at(-6.0, 6.0)], t.text.gamma_multiply(0.4), Stroke::NONE));
    p.text(at(0.0, 11.5), Align2::CENTER_CENTER, "N", semibold(8.5), t.text);
}

/// Where a popup result's feature lives.
enum Home<'a> {
    /// Nothing to check it against (no layer id).
    NoLayer,
    /// Its layer has been removed.
    LayerGone,
    Layer(&'a rustgis_engine::map::Layer),
}

/// Keeps the popup to features still on the map (#465): the ones deleted, undone away or in a
/// removed layer leave its list, and it closes when the one it shows is gone or another map is in
/// view. Checked once per change of the session.
fn follow_popup(app: &mut RustGisApp) {
    if app.map.popup_rev == app.session.revision {
        return;
    }
    app.map.popup_rev = app.session.revision;
    let active = app.session.project.active_map;
    let map = app.session.map();
    let Some(pp) = &mut app.map.popup else { return };
    // Another map in view, however it came (a tab, the Browser, a command).
    if pp.map != active {
        app.map.popup = None;
        return;
    }
    let results = pp.info["results"].as_array().cloned().unwrap_or_else(|| vec![pp.info.clone()]);
    let home = |r: &Value| match r.get("layerId").and_then(Value::as_u64) {
        None => Home::NoLayer,
        Some(id) => u32::try_from(id).ok().and_then(|id| map.layer(id)).map_or(Home::LayerGone, Home::Layer),
    };
    // The feature ids sought in each layer, found in one pass over it.
    let mut sought: std::collections::HashMap<u32, std::collections::HashSet<u64>> = Default::default();
    for r in &results {
        if let (Home::Layer(l), Some(fid)) = (home(r), r["fid"].as_u64()) {
            sought.entry(l.id).or_default().insert(fid);
        }
    }
    let mut found = std::collections::HashSet::new();
    for (id, fids) in &sought {
        let features = map.layer(*id).map_or(&[][..], |l| l.dataset.features.as_slice());
        found.extend(features.iter().filter(|f| fids.contains(&f.id)).map(|f| (*id, f.id)));
    }
    let here: Vec<bool> = results
        .iter()
        .map(|r| match home(r) {
            // A tile layer's answer, while the layer draws (as `with_answers` keeps them).
            Home::NoLayer => r["tileLayer"].as_str().is_none_or(|name| map.tiles.iter().any(|t| t.name == name && t.draws())),
            Home::LayerGone => false,
            // A streamed layer holds a window of its features: one outside it isn't gone.
            Home::Layer(l) => r["fid"].as_u64().is_none_or(|fid| l.dataset.windowed || found.contains(&(l.id, fid))),
        })
        .collect();
    let index = pp.index.min(results.len().saturating_sub(1));
    if here.get(index) != Some(&true) {
        app.map.popup = None;
        return;
    }
    if here.iter().all(|h| *h) {
        return;
    }
    let kept: Vec<Value> = results.iter().zip(here).filter(|(_, h)| *h).map(|(r, _)| r.clone()).collect();
    let Some(mut info) = kept.first().cloned() else { return };
    pp.index = index_after(&results, index, &kept);
    if let Value::Object(o) = &mut info {
        o.insert("results".into(), Value::Array(kept));
    }
    pp.info = info;
}

/// Adds what the WMS layers asked answer to the popup, asking a layer from a project once its
/// service's capabilities say how; failures are told. The popup closes when nothing was found.
fn follow_asks(app: &mut RustGisApp, ctx: &egui::Context) {
    let (active, opened) = (app.session.project.active_map, app.session.opened);
    let Some(pp) = &mut app.map.popup else { return };
    if pp.asks.is_empty() {
        return;
    }
    // Another map in view, or another project: the answers belong to layers no longer shown, and
    // a layer of the same name there must not take them.
    if pp.map != active || pp.opened != opened {
        app.map.popup = None;
        return;
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(100));
    let (lon, lat) = (pp.at.x, pp.at.y);
    let mut landed = Vec::new();
    pp.asks.retain_mut(|a| match a.slot.lock().unwrap_or_else(|e| e.into_inner()).take() {
        Some(r) => {
            landed.push((a.layer.clone(), std::mem::take(&mut a.also), a.capabilities, a.request.take(), r));
            false
        }
        None => true,
    });
    if landed.is_empty() {
        return;
    }
    let mut found = Vec::new();
    let mut next = Vec::new();
    // Only answers from layers drawn are shown: one hidden while its answer came would otherwise
    // take the pop-up from a feature in view.
    let exists = |app: &RustGisApp, layer: &str| app.session.map().tiles.iter().any(|t| t.name == layer && t.draws());
    for (layer, also, capabilities, request, r) in landed {
        // Layers removed, renamed or hidden while the answer came: nothing to show, nothing to
        // tell. The first of a service's layers still there takes the capabilities for the rest.
        let mut group = std::iter::once(layer).chain(also).filter(|l| exists(app, l));
        let Some(layer) = group.next() else { continue };
        let also: Vec<String> = group.collect();
        // The fetch's failure, or an answer that isn't text, is told here; the command tells its
        // own errors (naming the layer).
        let text = match r.and_then(|bytes| rustgis_engine::data::ogc::answer_text(&bytes).map(std::borrow::Cow::into_owned)) {
            Ok(text) => text,
            Err(e) => {
                app.session.notify(crate::i18n::fmt("{layer}: {e}", &[("layer", &layer), ("e", &e)]), true);
                continue;
            }
        };
        let mut replies = Vec::new();
        if capabilities {
            let p = json!({"layer": layer, "lon": lon, "lat": lat, "capabilities": text, "request": true});
            replies.push((layer, app.session.run("identify.featureInfo", &p)));
            // The service's other layers learned their requests from the same capabilities.
            if replies.iter().all(|(_, r)| r.is_ok()) {
                for other in also {
                    let p = json!({"layer": other, "lon": lon, "lat": lat, "request": true});
                    let r = app.session.run("identify.featureInfo", &p);
                    replies.push((other, r));
                }
            }
        } else {
            // With the request and the point: the service's error in one answer format asks again
            // in the next.
            let p = json!({"layer": layer, "text": text, "url": request, "lon": lon, "lat": lat});
            replies.push((layer.clone(), app.session.run("identify.featureInfo", &p)));
        }
        for (layer, r) in replies {
            let Ok(v) = r else { continue };
            if let Some(url) = v["url"].as_str() {
                let slot = crate::fetch_into_limited(url, rustgis_engine::data::ogc::MAX_INFO_BYTES);
                next.push(Ask { layer, also: Vec::new(), capabilities: false, request: Some(url.to_string()), slot });
            }
            found.extend(v["results"].as_array().cloned().unwrap_or_default());
        }
    }
    let Some(pp) = &mut app.map.popup else { return };
    pp.asks.extend(next);
    // Answers that land while the pop-up waits for them: it shows nothing yet, so it opens on the
    // top result, and selects its feature.
    let waited = pp.since.elapsed() < ASK_WAIT;
    if !found.is_empty()
        && let Some(info) = with_answers(app.session.map(), &pp.info, found)
    {
        if waited {
            pp.index = 0;
        } else {
            // The result shown stays shown, wherever the answers went; the first when it was left
            // out (a layer drawn higher answered, with topmost).
            let before = results_of(&pp.info);
            // As the pop-up shows it: an index past the end is the last result.
            let index = pp.index.min(before.len().saturating_sub(1));
            pp.index = index_after(&before, index, info["results"].as_array().map(Vec::as_slice).unwrap_or_default());
        }
        pp.info = info;
        if waited {
            select_shown(app);
        }
    }
    let Some(pp) = &mut app.map.popup else { return };
    if pp.asks.is_empty() && !is_found(&pp.info) {
        app.map.popup = None;
    }
}

fn popup(app: &mut RustGisApp, ui: &mut Ui, rect: Rect) {
    follow_asks(app, ui.ctx());
    follow_popup(app);
    let Some(pp) = &app.map.popup else { return };
    let t = Tokens::get(ui.ctx());
    let Some(anchor) = lonlat_to_screen(app, rect, pp.at) else { return };
    if !rect.contains(anchor) {
        return;
    }
    // Waiting for the WMS layers' answers, it shows none of the results yet.
    let asking = pp.asking();
    let results = if asking { 0 } else { pp.info["results"].as_array().map_or(0, Vec::len) };
    let index = pp.index.min(results.saturating_sub(1));
    let info = if asking {
        Value::Null
    } else if results > 1 {
        pp.info["results"][index].clone()
    } else {
        pp.info.clone()
    };
    let mut step: Option<usize> = None;
    let mut zoom = false;
    // The feature shown, as its layer id and feature id (a raster pixel has neither).
    let shown = info["layerId"].as_u64().and_then(|id| u32::try_from(id).ok()).zip(info["fid"].as_u64());
    // Only WMS layers being asked so far, or still waited for: their names.
    let waiting = !is_found(&info);
    let layer = if waiting {
        pp.asks.iter().map(|a| a.layer.as_str()).collect::<Vec<_>>().join(", ")
    } else {
        info["layer"].as_str().unwrap_or("").to_string()
    };
    let mut attrs = info["attributes"].as_object().cloned().unwrap_or_default();
    // A picture value (a photo's thumbnail) shows as the picture, not as its text.
    let picture_key = attrs.iter().find(|(_, v)| v.as_str().is_some_and(is_picture)).map(|(k, _)| k.clone());
    let picture = picture_key.and_then(|k| attrs.remove(&k)).and_then(|v| v.as_str().map(|s| picture_texture(app, ui.ctx(), s)));
    let title = if waiting {
        tl!("Asking the map service…").to_string()
    } else if info["raster"].as_bool() == Some(true) {
        tl!("Pixel").to_string()
    } else {
        {
            // A field called "name" first, then any name-like one (adm0name, NAME_EN…).
            let named = |exact: bool| {
                attrs.iter().find(|(k, _)| if exact { k.eq_ignore_ascii_case("name") } else { k.to_lowercase().contains("name") })
            };
            named(true).or_else(|| named(false)).and_then(|(_, v)| v.as_str()).unwrap_or(tl!("Feature")).to_string()
        }
    };
    let p = ui.painter();
    p.circle_filled(anchor, 6.0, Color32::from_rgb(0x00, 0xE5, 0xFF).gamma_multiply(0.5));
    p.circle_stroke(anchor, 6.0, Stroke::new(2.0, Color32::from_rgb(0x00, 0xA8, 0xC0)));
    // The popup is as tall as its rows, up to a limit; the corner grip sets the width and that
    // limit, and both are remembered for the next popup.
    let limit_id = egui::Id::unique("identify_popup_limit");
    let shown_id = egui::Id::unique("identify_popup_shown");
    let space = vec2((rect.width() - 24.0).max(240.0), (rect.height() - 24.0).max(140.0));
    let limit = ui.ctx().data(|d| d.get_temp::<egui::Vec2>(limit_id)).unwrap_or(vec2(310.0, 360.0)).min(space);
    let last_h = ui.ctx().data(|d| d.get_temp::<f32>(shown_id)).unwrap_or(limit.y);
    let pos = pos2(
        (anchor.x + 14.0).min(rect.max.x - limit.x - 12.0).max(rect.min.x + 8.0),
        (anchor.y - 40.0).clamp(rect.min.y + 8.0, (rect.max.y - last_h - 12.0).max(rect.min.y + 8.0)),
    );
    let mut close = false;
    let mut new_limit = None;
    // Keyed by the feature: egui remembers an area's (and a scroll area's and grid's) size, and
    // the next popup mustn't start from the last one's.
    let key = (&layer, &title, attrs.len());
    egui::Area::new(egui::Id::unique(("identify_popup", key))).fixed_pos(pos).order(egui::Order::Foreground).show(ui.ctx(), |ui| {
        let frame = egui::Frame::NONE
            .fill(t.menu)
            .stroke(Stroke::new(1.0, t.border_strong))
            .corner_radius(CornerRadius::same(10))
            .shadow(egui::Shadow { offset: [0, 6], blur: 20, spread: 0, color: t.shadow })
            .show(ui, |ui| {
                ui.set_width(limit.x);
                let header = egui::Frame::NONE
                    .fill(t.pane_header)
                    .corner_radius(CornerRadius { nw: 10, ne: 10, sw: 0, se: 0 })
                    .inner_margin(egui::Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        // Lay out controls first; the title and layer share only the width left.
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                            if ui.add(egui::Button::new(tl!("×")).frame(false)).clicked() {
                                close = true;
                            }
                            // A feature (not a raster pixel): zoom to it.
                            if shown.is_some() {
                                let (r, resp) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
                                let ink = if resp.hovered() { t.text } else { t.icon };
                                crate::icons::paint(ui.painter(), r, "zoom_sel", ink, t.accent);
                                if resp.on_hover_text(tl!("Zoom To")).clicked() {
                                    zoom = true;
                                }
                            }
                            // Several features identified: step through them.
                            if results > 1 {
                                if ui.add_enabled(index + 1 < results, egui::Button::new("›").frame(false)).clicked() {
                                    step = Some(index + 1);
                                }
                                let n = (index + 1).to_string();
                                let total = results.to_string();
                                ui.label(
                                    egui::RichText::new(crate::i18n::fmt("{n} of {total}", &[("n", &n), ("total", &total)]))
                                        .size(11.0)
                                        .color(t.text_dim),
                                );
                                if ui.add_enabled(index > 0, egui::Button::new("‹").frame(false)).clicked() {
                                    step = Some(index.saturating_sub(1));
                                }
                            }
                            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                                ui.add(egui::Label::new(egui::RichText::new(&title).font(semibold(14.0))).truncate()).on_hover_text(&title);
                                ui.add(egui::Label::new(egui::RichText::new(&layer).size(11.0).color(t.text_dim)).truncate())
                                    .on_hover_text(&layer);
                            });
                        });
                    });
                if let Some(picture) = &picture {
                    egui::Frame::NONE.inner_margin(egui::Margin { left: 12, right: 12, top: 8, bottom: 0 }).show(ui, |ui| match picture {
                        Ok(tex) => {
                            let size = tex.size_vec2();
                            let scale = (ui.available_width() / size.x.max(1.0)).min(1.5);
                            ui.add(egui::Image::new((tex.id(), size * scale)).corner_radius(CornerRadius::same(6)));
                        }
                        Err(e) => {
                            ui.label(egui::RichText::new(e.as_str()).size(11.0).color(t.text_dim));
                        }
                    });
                }
                // A solid scroll bar in its own gutter, shown when the rows are taller than the
                // limit (a floating one is invisible until hovered).
                ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
                // Full width, so the scrollbar sits on the right edge; as tall as the rows, up to
                // the limit.
                egui::ScrollArea::vertical()
                    .id_salt(("popup_rows", key))
                    .auto_shrink([false, true])
                    .max_height((limit.y - header.response.rect.height()).max(40.0))
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded)
                    .show(ui, |ui| {
                        egui::Frame::NONE.inner_margin(egui::Margin { left: 12, right: 14, top: 6, bottom: 12 }).show(ui, |ui| {
                            // The values take the width the names leave, so the stripes reach the scrollbar.
                            let name_w = attrs
                                .keys()
                                .map(|k| crate::widgets::text_w(ui, k, regular(11.5)))
                                .fold(0.0_f32, f32::max)
                                .min(ui.available_width() * 0.5);
                            let value_w = (ui.available_width() - name_w - 12.0).max(80.0);
                            egui::Grid::new(("popup_grid", key)).num_columns(2).striped(true).spacing(vec2(12.0, 4.0)).show(ui, |ui| {
                                for (k, v) in &attrs {
                                    ui.label(egui::RichText::new(k).size(11.5).color(t.text_dim));
                                    ui.scope(|ui| {
                                        // A fixed column: long values wrap, and the stripes span it.
                                        ui.set_min_width(value_w);
                                        ui.set_max_width(value_w);
                                        ui.add(egui::Label::new(egui::RichText::new(v.as_str().unwrap_or("")).size(11.5)).wrap());
                                    });
                                    ui.end_row();
                                }
                            });
                        });
                    });
            });
        // The corner grip: dragging it sets the width and the height limit.
        let r = frame.response.rect;
        let grip = Rect::from_min_max(r.max - vec2(16.0, 16.0), r.max);
        let resp = ui.interact(grip, egui::Id::unique("identify_popup_grip"), Sense::drag()).on_hover_cursor(egui::CursorIcon::ResizeNwSe);
        let color = if resp.hovered() || resp.dragged() { t.text } else { t.text_dim };
        for k in [4.0, 8.0, 12.0] {
            ui.painter()
                .line_segment([pos2(r.max.x - 3.0, r.max.y - 3.0 - k), pos2(r.max.x - 3.0 - k, r.max.y - 3.0)], Stroke::new(1.0, color));
        }
        if resp.dragged() {
            let d = resp.drag_delta();
            new_limit = Some(vec2(limit.x + d.x, r.height() + d.y).clamp(vec2(240.0, 120.0), space));
        }
        ui.ctx().data_mut(|d| d.insert_temp(shown_id, r.height()));
    });
    if let Some(l) = new_limit {
        ui.ctx().data_mut(|d| d.insert_temp(limit_id, l));
    }
    // Zooms to the feature shown, found by its id, as Zoom To Selection does: the selection and
    // the undo history are left as they are.
    if let (true, Some((id, fid))) = (zoom, shown) {
        let extent = app
            .session
            .map()
            .layer(id)
            .and_then(|l| l.dataset.features.iter().find(|f| f.id == fid))
            .and_then(|f| f.geometry.as_ref())
            .map(|g| g.extent());
        if let Some(e) = extent.filter(|e| !e.is_empty()) {
            app.session.zoom_to(&e);
        }
    }
    if close {
        app.map.popup = None;
    } else if let (Some(i), Some(pp)) = (step, &mut app.map.popup) {
        pp.index = i;
        select_shown(app);
    }
}

/// Selects the feature the pop-up shows, as after the click. Not through `select.rows`: browsing
/// a pop-up mustn't fill the undo history; the new revision redraws the map. Found again by its
/// feature id: a streamed layer may have read another window since the click, moving the feature
/// to another row (or out of the layer: then nothing is selected).
fn select_shown(app: &mut RustGisApp) {
    let Some(pp) = &app.map.popup else { return };
    let shown = pp.info["results"].get(pp.index).and_then(|r| Some((u32::try_from(r["layerId"].as_u64()?).ok()?, r["fid"].as_u64()?)));
    let found = shown.and_then(|(id, fid)| {
        let row = app.session.map().layer(id)?.dataset.features.iter().position(|f| f.id == fid)?;
        Some((id, row))
    });
    if let Some((id, row)) = found {
        rustgis_engine::cmd::select::set(&mut app.session, id, vec![row], rustgis_engine::cmd::select::Mode::New);
    } else {
        app.session.map_mut().clear_selection();
    }
    app.session.revision = app.session.revision.wrapping_add(1);
}

/// Whether an attribute value is a picture (a `data:image/png` or `jpeg` address, as a photo's
/// thumbnail is kept).
pub(crate) fn is_picture(v: &str) -> bool {
    v.starts_with("data:image/jpeg;base64,") || v.starts_with("data:image/png;base64,")
}

/// The texture of a picture value, decoded once and kept while the same value is shown.
fn picture_texture(app: &mut RustGisApp, ctx: &egui::Context, value: &str) -> Result<egui::TextureHandle, String> {
    use base64::Engine;
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut h);
    let key = h.finish();
    if let Some((k, t)) = &app.map.picture
        && *k == key
    {
        return t.clone();
    }
    // Pictures kept with features are small (thumbnails); anything large isn't decoded here.
    let decoded = (|| {
        let data = value.split_once(",").map(|(_, d)| d).ok_or("not a picture")?;
        if data.len() > 4 << 20 {
            return Err("the picture is too large to show".to_string());
        }
        let bytes = base64::engine::general_purpose::STANDARD.decode(data).map_err(|e| e.to_string())?;
        let img = decode_picture(&bytes)?;
        let (w, h) = (img.width() as usize, img.height() as usize);
        let color = egui::ColorImage::from_rgba_unmultiplied([w, h], img.as_raw());
        Ok(ctx.load_texture("identify_picture", color, egui::TextureOptions::LINEAR))
    })();
    app.map.picture = Some((key, decoded.clone()));
    decoded
}

/// A picture kept with a feature, decoded for the identify popup. A project's value is hostile:
/// its size is checked before anything is decoded, so a small compressed picture can't unpack
/// into a huge one.
pub(crate) fn decode_picture(bytes: &[u8]) -> Result<image::RgbaImage, String> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().map_err(|e| e.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(2048);
    limits.max_image_height = Some(2048);
    limits.max_alloc = Some(64 << 20);
    reader.limits(limits);
    Ok(reader.decode().map_err(|_| "the picture is too large to show, or isn't one".to_string())?.to_rgba8())
}

/// The on-screen navigator: a north compass, zoom in and out, and four pan arrows.
fn navigator(app: &mut RustGisApp, ui: &mut Ui, p: &egui::Painter, rect: Rect, t: &Tokens) {
    let c = pos2(rect.min.x + 52.0, rect.min.y + 112.0);
    p.circle_filled(c, 34.0, t.ribbon.gamma_multiply(0.92));
    p.circle_stroke(c, 34.0, Stroke::new(1.0, t.border_strong));
    let (w, h) = (rect.width() as f64, rect.height() as f64);
    // Each button is an egui widget above the map, so its clicks are its own (not the map's
    // identify, select or sketch) and popups over it get theirs first.
    let button = |ui: &mut Ui, r: Rect, id: &str| ui.interact(r, ui.scope_id().with(("navigator", id)), Sense::click()).clicked();
    for (dir, (dx, dy)) in [("n", (0.0, -1.0)), ("s", (0.0, 1.0)), ("e", (1.0, 0.0)), ("w", (-1.0, 0.0))] {
        let tip = c + vec2(dx, dy) * 28.0;
        let base = c + vec2(dx, dy) * 18.0;
        let side = vec2(-dy, dx) * 6.0;
        let col = if dir == "n" { Color32::from_rgb(0xD1, 0x3C, 0x3C) } else { t.text_dim };
        p.add(egui::Shape::convex_polygon(vec![tip, base + side, base - side], col, Stroke::NONE));
        if button(ui, Rect::from_center_size(c + vec2(dx, dy) * 23.0, vec2(18.0, 18.0)), dir) {
            pan(app, -dx as f64 * w * 0.25, -dy as f64 * h * 0.25, w, h);
            app.session.record_extent();
        }
    }
    p.text(c, Align2::CENTER_CENTER, "N", semibold(11.0), t.text);
    for (k, (label, f)) in [("+", 2.0), ("−", 0.5)].into_iter().enumerate() {
        let r = Rect::from_center_size(pos2(c.x, c.y + 52.0 + k as f32 * 26.0), vec2(24.0, 22.0));
        p.rect_filled(r, CornerRadius::same(5), t.ribbon.gamma_multiply(0.92));
        p.rect_stroke(r, CornerRadius::same(5), Stroke::new(1.0, t.border_strong), egui::StrokeKind::Inside);
        p.text(r.center(), Align2::CENTER_CENTER, label, semibold(14.0), t.text);
        if button(ui, r, label) {
            zoom(app, f, w / 2.0, h / 2.0, w, h);
            app.session.record_extent();
        }
    }
}

/// View tabs above the map, one per map in the project, like document tabs.
fn view_tabs(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
    ui.painter().rect_filled(r, 0.0, t.tab_strip);
    ui.painter().line_segment([pos2(r.min.x, r.max.y - 0.5), pos2(r.max.x, r.max.y - 0.5)], Stroke::new(1.0, t.border));
    let mut x = r.min.x + 6.0;
    let names: Vec<String> = app.session.project.maps.iter().map(|m| m.name.clone()).collect();
    let active = if app.session.active_layout.is_some() || app.mb.shown() { usize::MAX } else { app.session.project.active_map };
    // A tab's close button asks to delete its map or layout.
    let mut close: Option<Deletion> = None;
    for (i, n) in names.iter().enumerate() {
        let w = crate::widgets::text_w(ui, n, regular(12.0)) + 46.0 + TAB_CLOSE_W;
        let tr = Rect::from_min_size(pos2(x, r.min.y + 4.0), vec2(w, r.height() - 4.0));
        let resp = ui.interact(tr, egui::Id::unique(("maptab", i)), Sense::click());
        // Painted, so named for screen readers (and tests).
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, n));
        let is = i == active;
        // A project keeps at least one map (`map.remove` refuses the last).
        let last = names.len() < 2;
        let tip = if last { tl!("A project keeps at least one map") } else { tl!("Delete Map") };
        if is {
            ui.painter().rect_filled(tr, CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 }, t.ribbon);
            ui.painter().rect_filled(
                Rect::from_min_max(tr.min, pos2(tr.max.x, tr.min.y + 2.0)),
                CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 },
                t.accent,
            );
        } else if resp.contains_pointer() {
            ui.painter().rect_filled(tr, CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 }, t.hover);
        }
        icons::paint(ui.painter(), Rect::from_center_size(pos2(tr.min.x + 15.0, tr.center().y), vec2(15.0, 15.0)), "map", t.icon, t.accent);
        ui.painter().text(
            pos2(tr.min.x + 28.0, tr.center().y),
            Align2::LEFT_CENTER,
            n,
            if is { semibold(12.0) } else { regular(12.0) },
            if is { t.text } else { t.text_dim },
        );
        // Over the tab's fill, so painted after it.
        if tab_close(ui, t, tr, egui::Id::unique(("maptab_close", i)), is, !last, tip) {
            close = Some(Deletion::Map(n.clone()));
        }
        if resp.clicked() && !is {
            app.mb.active = false;
            let _ = app.run("layout.close", Value::Null);
            let _ = app.run("map.activate", json!({"name": n}));
            app.map.popup = None;
        }
        // Right-click for the map's menu, as in the Browser; double-click renames.
        if resp.double_clicked() {
            let _ = app.run("ui.dialog", json!({"name": "renameMap", "target": n}));
        }
        resp.context_menu(|ui| crate::panes::map_menu(app, ui, n));
        x += w + 2.0;
    }
    // Layouts, after the maps.
    let layouts: Vec<String> = app.session.project.layouts.iter().map(|l| l.name.clone()).collect();
    for (i, n) in layouts.iter().enumerate() {
        let w = crate::widgets::text_w(ui, n, regular(12.0)) + 46.0 + TAB_CLOSE_W;
        let tr = Rect::from_min_size(pos2(x, r.min.y + 4.0), vec2(w, r.height() - 4.0));
        let resp = ui.interact(tr, egui::Id::unique(("layouttab", i)), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, n));
        let is = app.session.active_layout == Some(i) && !app.mb.shown();
        if is {
            ui.painter().rect_filled(tr, CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 }, t.ribbon);
            ui.painter().rect_filled(
                Rect::from_min_max(tr.min, pos2(tr.max.x, tr.min.y + 2.0)),
                CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 },
                t.accent,
            );
        } else if resp.contains_pointer() {
            ui.painter().rect_filled(tr, CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 }, t.hover);
        }
        icons::paint(
            ui.painter(),
            Rect::from_center_size(pos2(tr.min.x + 15.0, tr.center().y), vec2(15.0, 15.0)),
            "new_layout",
            t.icon,
            t.accent,
        );
        ui.painter().text(
            pos2(tr.min.x + 28.0, tr.center().y),
            Align2::LEFT_CENTER,
            n,
            if is { semibold(12.0) } else { regular(12.0) },
            if is { t.text } else { t.text_dim },
        );
        // Over the tab's fill, so painted after it.
        if tab_close(ui, t, tr, egui::Id::unique(("layouttab_close", i)), is, true, tl!("Delete Layout")) {
            close = Some(Deletion::Layout(n.clone()));
        }
        if resp.clicked() && !is {
            app.mb.active = false;
            let _ = app.run("layout.activate", json!({"name": n}));
            app.ui.tab = "Publish".into();
        }
        if resp.double_clicked() {
            let _ = app.run("ui.dialog", json!({"name": "renameLayout", "target": n}));
        }
        resp.context_menu(|ui| crate::panes::layout_menu(app, ui, n));
        x += w + 2.0;
    }
    // ModelBuilder, after the layouts.
    if app.mb.open {
        let n = if app.mb.name.trim().is_empty() { tl!("Untitled model").to_string() } else { app.mb.name.trim().to_string() };
        let w = crate::widgets::text_w(ui, &n, regular(12.0)) + 46.0 + TAB_CLOSE_W;
        let tr = Rect::from_min_size(pos2(x, r.min.y + 4.0), vec2(w, r.height() - 4.0));
        let resp = ui.interact(tr, egui::Id::unique("modeltab"), Sense::click());
        let is = app.mb.shown();
        if is {
            ui.painter().rect_filled(tr, CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 }, t.ribbon);
            ui.painter().rect_filled(
                Rect::from_min_max(tr.min, pos2(tr.max.x, tr.min.y + 2.0)),
                CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 },
                t.accent,
            );
        } else if resp.contains_pointer() {
            ui.painter().rect_filled(tr, CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 }, t.hover);
        }
        icons::paint(
            ui.painter(),
            Rect::from_center_size(pos2(tr.min.x + 15.0, tr.center().y), vec2(15.0, 15.0)),
            "modelbuilder",
            t.icon,
            t.accent,
        );
        ui.painter().text(
            pos2(tr.min.x + 28.0, tr.center().y),
            Align2::LEFT_CENTER,
            &n,
            if is { semibold(12.0) } else { regular(12.0) },
            if is { t.text } else { t.text_dim },
        );
        if tab_close(ui, t, tr, egui::Id::unique("modeltab_close"), is, true, tl!("Close ModelBuilder")) {
            let _ = app.run("ui.modelBuilder", json!({"close": true}));
        }
        if resp.clicked() && !is {
            let _ = app.run("ui.modelBuilder", Value::Null);
        }
        x += w + 2.0;
    }
    let pr = Rect::from_min_size(pos2(x + 2.0, r.min.y + 6.0), vec2(22.0, 20.0));
    let resp = ui.interact(pr, egui::Id::unique("maptab_new"), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(pr, CornerRadius::same(4), t.hover);
    }
    ui.painter().text(pr.center(), Align2::CENTER_CENTER, "+", regular(15.0), t.text_dim);
    if resp.on_hover_text(tl!("New Map")).clicked() {
        let _ = app.run("map.new", Value::Null);
    }
    if close.is_some() {
        app.map.deleting = close;
    }
}

/// A map or a layout, by name, to delete once the user confirms ([`confirm_delete`]).
#[derive(Clone)]
pub enum Deletion {
    Map(String),
    Layout(String),
}

/// Asks before deleting the map or layout in `app.map.deleting` (tab close buttons, the
/// Browser): a whole map or layout goes in one click. The commands themselves never ask.
pub fn confirm_delete(app: &mut RustGisApp, ctx: &egui::Context) {
    let Some(d) = app.map.deleting.clone() else { return };
    // Gone meanwhile (a command, the assistant), or the last map now: nothing left to ask.
    let p = &app.session.project;
    let possible = match &d {
        Deletion::Map(n) => p.maps.len() > 1 && p.maps.iter().any(|m| m.name.eq_ignore_ascii_case(n)),
        Deletion::Layout(n) => p.layouts.iter().any(|l| l.name.eq_ignore_ascii_case(n)),
    };
    if !possible {
        app.map.deleting = None;
        return;
    }
    let t = Tokens::get(ctx);
    let title = match &d {
        Deletion::Map(n) => crate::i18n::fmt("Delete the map {name}?", &[("name", n)]),
        Deletion::Layout(n) => crate::i18n::fmt("Delete the layout {name}?", &[("name", n)]),
    };
    let undo = crate::widgets::shortcut_text(ctx, app, "edit.undo");
    let mut decision = None;
    egui::Modal::new(egui::Id::unique("confirm_delete")).show(ctx, |ui| {
        ui.set_width(360.0);
        ui.label(egui::RichText::new(title).font(semibold(15.0)));
        if !undo.is_empty() {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(crate::i18n::fmt("You can undo this with {shortcut}.", &[("shortcut", &undo)])).color(t.text_dim));
        }
        ui.add_space(10.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
            if crate::widgets::dialog_button(ui, tl!("Delete"), Some(t.red), vec2(84.0, 28.0)).clicked() {
                decision = Some(true);
            }
            if crate::widgets::dialog_button(ui, tl!("Cancel"), None, vec2(84.0, 28.0)).clicked()
                || ui.input(|i| i.key_pressed(egui::Key::Escape))
            {
                decision = Some(false);
            }
        });
    });
    match decision {
        Some(true) => {
            app.map.deleting = None;
            match d {
                Deletion::Map(n) => delete_map(app, &n),
                Deletion::Layout(n) => {
                    let _ = app.run("layout.remove", json!({"name": n}));
                }
            }
        }
        Some(false) => app.map.deleting = None,
        None => {}
    }
}

/// The map shown side by side, while its name finds one (a deleted or renamed map's doesn't).
pub(crate) fn split_map(app: &RustGisApp) -> Option<usize> {
    let n = app.ws.split.as_deref()?;
    app.session.project.maps.iter().position(|m| m.name.eq_ignore_ascii_case(n))
}

/// Deletes a map (`map.remove`); the popup goes with the active map, even behind an open layout.
pub(crate) fn delete_map(app: &mut RustGisApp, name: &str) {
    let p = &app.session.project;
    let was_active = p.maps.get(p.active_map).is_some_and(|m| m.name.eq_ignore_ascii_case(name));
    if app.run("map.remove", json!({"name": name})).is_ok() && was_active {
        app.map.popup = None;
    }
}

/// Room a view tab keeps at its right for the close button.
const TAB_CLOSE_W: f32 = 18.0;

/// A view tab's close button (×), shown on the tab in view and on the one under the pointer;
/// `enabled: false` dims it and only explains why (`tip`). True when clicked.
fn tab_close(ui: &mut Ui, t: &Tokens, tab: Rect, id: egui::Id, in_view: bool, enabled: bool, tip: &str) -> bool {
    // Hidden, it takes no clicks either: they open the tab (a touch screen has no hover).
    if !(in_view || ui.rect_contains_pointer(tab)) {
        return false;
    }
    let cr = Rect::from_center_size(pos2(tab.max.x - 13.0, tab.center().y), vec2(16.0, 16.0));
    // Added after the tab, so it sits on top: its click (even dimmed) doesn't also open the tab.
    let resp = ui.interact(cr, id, Sense::click());
    if enabled && resp.hovered() {
        ui.painter().rect_filled(cr, CornerRadius::same(3), t.pressed);
    }
    let ink = if !enabled {
        t.text_disabled
    } else if resp.hovered() {
        t.text
    } else {
        t.text_dim
    };
    icons::paint(ui.painter(), cr.shrink(3.0), "close", ink, t.accent);
    let resp = resp.on_hover_text(tip);
    enabled && resp.clicked()
}

/// How far the map's last frame is zoomed in, while the map moves, before it is sharpened: a
/// CPU frame drawn again, or a GPU scene's raster tiles placed for the new zoom.
const MOTION_STRETCH: f64 = 1.1;

/// How far a GPU scene's raster tiles are zoomed out, while the map moves, before the tiles for
/// the new zoom are placed: their textures have no mipmaps, so shrunk much further they shimmer.
const TILE_SHRINK: f64 = 1.5;

/// Whether a flat GPU frame drawn with `scene` holds every feature `cam` shows in a `w`×`h`
/// view: the view lies inside the extent its meshes were tessellated for, so moving them leaves
/// no gaps. Only the view's camera and size matter to that extent.
fn scene_covers(scene: Camera, cam: Camera, w: f64, h: f64) -> bool {
    use rustgis_engine::render::{GPU_CULL_MARGIN, cull_extent};
    let held = cull_extent(&View { width: w, height: h, pixels_per_point: 1.0, camera: scene, sky: true }, GPU_CULL_MARGIN);
    let e = cam.extent(w, h);
    !e.is_empty() && e.min_x >= held.min_x && e.min_y >= held.min_y && e.max_x <= held.max_x && e.max_y <= held.max_y
}

/// How long (ms) the last exact frame may have taken for a change to the map's contents to keep
/// it on screen until the next one arrives, rather than showing a coarse preview (#1051); and how
/// long it is kept at most, should the next one take longer.
const KEEP_FRAME_MS: u64 = 300;
const KEEP_FRAME_S: f64 = KEEP_FRAME_MS as f64 / 1000.0;

/// Which project and map are open, the map's backdrop, and which of its layers, groups and tile
/// layers are drawn, in order: a frame of other layers (one hidden, added, removed or moved since,
/// another basemap, map or project) shows a preview of the change instead of staying on screen.
fn shown_layers(session: &rustgis_engine::Session) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::hash::DefaultHasher::new();
    let project = &session.project;
    let map = project.map();
    (&session.path, &project.name, project.active_map, &map.name, map.basemap, map.graticule).hash(&mut h);
    for l in &map.layers {
        (l.id, l.visible).hash(&mut h);
    }
    for g in &map.groups {
        (g.id, g.visible, &g.children, g.parent).hash(&mut h);
    }
    for t in &map.tiles {
        (&t.name, &t.url, t.draws(), t.over).hash(&mut h);
    }
    // What is drawn, not how it looks: view effects and colour vision, like a layer's style,
    // keep the frame until the exact one (at most `KEEP_FRAME_S`).
    h.finish()
}

/// Whether revision `a` is later than `b` (revisions wrap).
fn later(a: u64, b: u64) -> bool {
    a.wrapping_sub(b).cast_signed() > 0
}

/// Whether a change to the map's contents keeps the frame on screen (`shown`) until the exact
/// frame for `key` arrives, instead of a coarse preview of the whole map: the view and the layers
/// shown are the same (a layer's colour, transparency, symbology or labels changed, or its
/// features were edited) and its frames are quick to draw (`render_ms`). A preview of a tiled
/// map drew the basemap coarsely too, so dragging a layer's colour or transparency slider made
/// the whole map flicker between blurry and sharp.
fn keeps_frame(shown: Option<Key>, key: Key, render_ms: u64) -> bool {
    render_ms <= KEEP_FRAME_MS && shown.is_some_and(|s| later(key.rev, s.rev) && (s.cam, s.size, s.shown) == (key.cam, key.size, key.shown))
}

/// Whether a frame finished for `done` is shown although the map's contents changed since (to
/// `key`): it is of the same view and layers, and later than the frame on screen (`shown`), which
/// is of an older change too. While a slider is dragged every exact frame is out of date when it
/// finishes; showing none kept the frame from before the drag up until it ended.
fn shows_finished(done: Key, key: Key, shown: Option<Key>) -> bool {
    let view = |k: Key| (k.cam, k.size, k.shown);
    later(key.rev, done.rev) && view(done) == view(key) && shown.is_some_and(|s| later(done.rev, s.rev) && view(s) == view(key))
}

/// How long (s) after a frame is shown tiles arriving wait before the map is drawn again.
const TILE_REDRAW_GAP: f64 = 0.25;

/// The tile cache generation `key` holds at instead of its own, when tiles arrived within
/// [`TILE_REDRAW_GAP`] of the last frame shown: the newest frame of this view, of the one being
/// drawn (`drawing`) and the one on screen (`shown`), so the map is drawn at most a few times a
/// second while they arrive. Generations only grow: a live GPU scene can show newer tiles than
/// the frame still being drawn, and isn't held back to it.
///
/// Holding at the frame on screen while a newer one was being drawn made the two alternate for
/// good: each, once shown, differed from the key held at meanwhile, which started the other again
/// (a tiled map in a projection other than Web Mercator, whose frames take longer, was drawn over
/// and over, keeping a core busy and the app slow).
fn held_tiles(key: Key, drawing: Option<Key>, shown: Option<Key>, since_render: f64) -> Option<u64> {
    let same_view = |k: &Key| Key { tiles: k.tiles, ..key } == *k;
    let newest = [drawing, shown].into_iter().flatten().filter(same_view).max_by_key(|k| k.tiles)?;
    (newest.tiles != key.tiles && since_render < TILE_REDRAW_GAP).then_some(newest.tiles)
}

#[cfg(test)]
mod tile_redraw_tests {
    use super::{KEEP_FRAME_MS, Key, held_tiles, keeps_frame, shows_finished};

    fn key(tiles: u64) -> Key {
        Key { rev: 4, cam: (1, 2, 3, 0, 0), size: [800, 600], draft: false, tiles, sources: 0, shown: 0 }
    }

    #[test]
    fn tiles_arriving_are_held_briefly_after_a_frame() {
        // Shown a moment ago: the new tiles wait; a while ago, they are drawn.
        assert_eq!(held_tiles(key(28), None, Some(key(0)), 0.1), Some(0));
        assert_eq!(held_tiles(key(28), None, Some(key(0)), 0.3), None);
        // Another view isn't held.
        let moved = Key { cam: (9, 9, 9, 0, 0), ..key(0) };
        assert_eq!(held_tiles(key(28), None, Some(moved), 0.1), None);
    }

    #[test]
    fn a_frame_being_drawn_with_newer_tiles_is_not_drawn_again_with_older_ones() {
        // The frame drawn with the tiles that arrived is on its way while an older one was just
        // shown: the key is that frame's, not the older one's.
        assert_eq!(held_tiles(key(28), Some(key(28)), Some(key(0)), 0.1), None);
        // A live scene shown with newer tiles than the frame being drawn isn't held back to it.
        assert_eq!(held_tiles(key(28), Some(key(5)), Some(key(28)), 0.1), None);
        assert_eq!(held_tiles(key(30), Some(key(5)), Some(key(28)), 0.1), Some(28));
        // Simulated: the view's frames alternated between generations 0 and 28 for good.
        let (mut shown, mut drawing, mut since) = (Some(key(0)), Some(key(28)), 0.0);
        let mut started = 0;
        for _ in 0..50 {
            let mut k = key(28);
            if let Some(t) = held_tiles(k, drawing, shown, since) {
                k.tiles = t;
            }
            // The frame being drawn finishes and is shown; another starts if the key differs.
            if let Some(d) = drawing.take() {
                shown = Some(d);
                since = 0.0;
            }
            if shown != Some(k) {
                drawing = Some(k);
                started += 1;
            }
            since += 0.016;
        }
        assert_eq!(started, 0);
        assert_eq!(shown, Some(key(28)));
    }

    #[test]
    fn a_change_to_the_contents_keeps_a_quick_frame_instead_of_a_preview() {
        // #1051: a layer's colour changed, the view didn't: the frame on screen stays.
        let changed = Key { rev: 5, ..key(3) };
        assert!(keeps_frame(Some(key(3)), changed, 40));
        assert!(keeps_frame(Some(Key { draft: true, ..key(3) }), changed, 40), "a draft of the view stays too");
        // Slow frames show a preview: the old picture would stay wrong too long.
        assert!(!keeps_frame(Some(key(3)), changed, KEEP_FRAME_MS + 1));
        // Another view, or nothing on screen: a preview.
        assert!(!keeps_frame(Some(Key { cam: (9, 9, 9, 0, 0), ..key(3) }), changed, 40));
        assert!(!keeps_frame(Some(Key { size: [400, 300], ..key(3) }), changed, 40));
        // A layer hidden, added, removed or moved, or another map: a preview.
        assert!(!keeps_frame(Some(Key { shown: 1, ..key(3) }), changed, 40));
        assert!(!keeps_frame(None, changed, 40));
        // Nothing changed: nothing to keep.
        assert!(!keeps_frame(Some(key(3)), key(3), 40));
    }

    #[test]
    fn a_frame_of_an_earlier_change_shows_while_a_slider_is_dragged() {
        let (on_screen, done, now) = (Key { rev: 4, ..key(0) }, Key { rev: 6, ..key(0) }, Key { rev: 9, ..key(0) });
        assert!(shows_finished(done, now, Some(on_screen)));
        // The frame on screen is already of the latest change, or is the finished one.
        assert!(!shows_finished(done, now, Some(now)));
        assert!(!shows_finished(done, now, Some(done)));
        // A frame of another view, or one of the latest change (shown the usual way).
        assert!(!shows_finished(Key { cam: (9, 9, 9, 0, 0), ..done }, now, Some(on_screen)));
        assert!(!shows_finished(now, now, Some(on_screen)));
        assert!(!shows_finished(done, now, None));
        // Older than the frame on screen, or of other layers: not shown.
        assert!(!shows_finished(on_screen, now, Some(done)));
        assert!(!shows_finished(Key { shown: 1, ..done }, now, Some(on_screen)));
        // Revisions wrap.
        let (old, newer, latest) = (Key { rev: u64::MAX, ..key(0) }, Key { rev: 1, ..key(0) }, Key { rev: 3, ..key(0) });
        assert!(shows_finished(newer, latest, Some(old)));
        assert!(!shows_finished(old, latest, Some(newer)));
        // Simulated drag: the contents change every frame and each exact frame takes three.
        // Frames of the drag are shown as they finish, not only the last one after it.
        let (mut shown, mut drawing, mut shown_during) = (Some(on_screen), None::<(Key, u32)>, 0);
        for step in 0..30u64 {
            let k = Key { rev: 10 + step, ..key(0) };
            if let Some((d, 0)) = drawing {
                if shows_finished(d, k, shown) {
                    shown = Some(d);
                    shown_during += 1;
                }
                drawing = None;
            }
            drawing = match drawing {
                Some((d, n)) => Some((d, n - 1)),
                None => Some((k, 2)),
            };
        }
        assert!(shown_during >= 5, "frames shown during the drag: {shown_during}");
    }
}

#[cfg(test)]
mod halo_tests {
    use super::*;
    use rustgis_engine::map::Rgba;

    #[test]
    fn a_backgrounds_outline_is_its_rectangle_rounded_and_turned() {
        let r = Rect::from_min_max(pos2(10.0, 20.0), pos2(50.0, 30.0));
        // Square: its four corners, clockwise from the top left.
        assert_eq!(background_outline(r, 0.0, 0.0), vec![r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()]);
        // Rounded: five points a corner, all within it, the corners themselves cut off.
        let round = background_outline(r, 3.0, 0.0);
        assert_eq!(round.len(), 20);
        assert!(round.iter().all(|p| r.expand(1e-3).contains(*p)));
        assert!(!round.contains(&r.left_top()));
        // A radius past half the shorter side is held to it.
        let most = background_outline(r, 99.0, 0.0);
        assert!(most.iter().all(|p| r.expand(1e-3).contains(*p)));
        // Turned a quarter about its centre: as tall as it was wide.
        let turned = background_outline(r, 0.0, std::f32::consts::FRAC_PI_2);
        let b = Rect::from_points(&turned);
        assert!((b.width() - 10.0).abs() < 1e-3 && (b.height() - 40.0).abs() < 1e-3 && (b.center() - r.center()).length() < 1e-3, "{b:?}");
        // Nothing for a rectangle of no size or no number.
        assert!(background_outline(Rect::from_min_max(pos2(0.0, 0.0), pos2(0.0, 5.0)), 0.0, 0.0).is_empty());
        assert!(background_outline(Rect::from_min_max(pos2(0.0, 0.0), pos2(f32::NAN, 5.0)), 0.0, 0.0).is_empty());
        assert_eq!(background_outline(r, f32::NAN, 0.0).len(), 4);
    }

    fn label(halo_width: Option<f32>) -> PlacedLabel {
        PlacedLabel {
            text: "A".into(),
            x: 0.0,
            y: 0.0,
            size: 10.0,
            color: Rgba::rgb(0, 0, 0),
            halo: Some(Rgba::rgb(255, 255, 255)),
            halo_width,
            bold: false,
            icon: None,
            align: 0.0,
            framed: false,
            italic: false,
            spacing: 0.0,
            font: None,
            line_align: TextAlign::Left,
            angle: None,
            collide: Default::default(),
            glyphs: Vec::new(),
            background: None,
        }
    }

    #[test]
    fn a_halos_copies_reach_its_width_and_stay_few_and_one_coat_see_through() {
        // The thin halo as it always was: eight copies a point round, in its own colour.
        let (thin, c) = halo_coats(&label(None), Rgba::new(255, 255, 255, 225));
        assert_eq!((thin.len(), c), (8, Color32::from_rgba_unmultiplied(255, 255, 255, 225)));
        for w in [0.5, 1.42, 3.0, 10.0, 1e9] {
            let (offsets, _) = halo_coats(&label(Some(w)), Rgba::rgb(255, 255, 255));
            let far = offsets.iter().map(|o| o.length()).fold(0.0, f32::max);
            let reach = rustgis_engine::render::halo_reach(w) * 1.33;
            assert!((far - reach).abs() < 1e-3 && offsets.len() <= MAX_HALO_COATS, "{w}: {far} of {reach}, {}", offsets.len());
        }
        assert!(halo_coats(&label(Some(0.0)), Rgba::rgb(255, 255, 255)).0.is_empty(), "no width, no halo");
        // QGIS's usual buffer (0.5 mm, 1.42 pt) is solid: neighbouring copies on a ring are well
        // under a pixel apart. On every ring, at any width, the copies stand evenly round it.
        // ... with few copies: about as many as the thin halo's two or three times over.
        assert!(halo_coats(&label(Some(1.42)), Rgba::rgb(255, 255, 255)).0.len() <= 24);
        for w in [1.42, 3.0, 10.0] {
            let (offsets, _) = halo_coats(&label(Some(w)), Rgba::rgb(255, 255, 255));
            let mut rings: Vec<(f32, Vec<egui::Vec2>)> = Vec::new();
            for o in offsets {
                match rings.iter_mut().find(|(r, _)| (r - o.length()).abs() < 1e-3) {
                    Some((_, ring)) => ring.push(o),
                    None => rings.push((o.length(), vec![o])),
                }
            }
            for (r, ring) in &rings {
                let gap = std::f32::consts::TAU * r / ring.len() as f32;
                let next = (ring[1] - ring[0]).length();
                assert!((next - 2.0 * r * (gap / (2.0 * r)).sin()).abs() < 1e-3, "{w}: uneven ring {r}");
                if w < 2.0 {
                    assert!(gap <= 1.0, "{w}: ring {r} has gaps of {gap}");
                }
            }
        }
        // Half see-through, shared among its rings: each copy fainter, about one coat in all.
        let (offsets, c) = halo_coats(&label(Some(3.0)), Rgba::new(255, 0, 0, 128));
        assert!(offsets.len() > 8 && c.a() < 128 && c.a() > 0, "{c:?}");
    }
}

#[cfg(test)]
mod picture_tests {
    #[test]
    fn large_pictures_are_refused_before_decoding() {
        let png = |w: u32, h: u32| {
            let mut out = Vec::new();
            image::RgbaImage::new(w, h).write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
            out
        };
        assert!(super::decode_picture(&png(160, 120)).is_ok());
        // A blank picture compresses to a few KB, but would unpack into far more than shown.
        let bomb = png(4000, 4000);
        assert!(bomb.len() < 4 << 20, "{}", bomb.len());
        assert!(super::decode_picture(&bomb).is_err());
        assert!(super::decode_picture(b"not a picture").is_err());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Natural Earth is credited only where its world outline is drawn: not on a blank basemap or
    /// another body, whose sources are still credited.
    #[test]
    fn natural_earth_is_credited_only_where_drawn() {
        use rustgis_engine::map::Basemap;
        let mut map = RustGisApp::with_sample().session.map().clone();
        let tiles = ["OpenStreetMap".to_string()];
        for basemap in [Basemap::Paper, Basemap::Midnight, Basemap::Ocean, Basemap::Atlas] {
            map.basemap = basemap;
            assert_eq!(attribution(&map, &[]), "Natural Earth");
            assert_eq!(attribution(&map, &tiles), "Natural Earth | OpenStreetMap");
        }
        map.basemap = Basemap::Blank;
        assert_eq!(attribution(&map, &[]), "");
        assert_eq!(attribution(&map, &tiles), "OpenStreetMap");
        map.basemap = Basemap::Paper;
        map.body = "mars".into();
        assert_eq!(attribution(&map, &[]), "");
        assert_eq!(attribution(&map, &tiles), "OpenStreetMap");
    }

    /// The pop-up stays on the result shown as answers come and results go, the second of two
    /// identical ones too, and goes to the first when that one is left out.
    #[test]
    fn the_result_shown_stays_shown() {
        let (a, b, c) = (json!({"layer": "A"}), json!({"layer": "B"}), json!({"layer": "C"}));
        let before = [a.clone(), b.clone(), b.clone()];
        // An answer before them: each keeps its place among the identical.
        let after = [c.clone(), a.clone(), b.clone(), b.clone()];
        assert_eq!((0..3).map(|i| index_after(&before, i, &after)).collect::<Vec<_>>(), [1, 2, 3]);
        // The one shown left out; a list shorter than the index.
        assert_eq!(index_after(&before, 0, &[c.clone(), b.clone()]), 0);
        assert_eq!(index_after(&before, 5, &after), 0);
    }

    /// A pop-up asking a WMS layer waits for its answer, up to `ASK_WAIT`, before it shows what
    /// was found, so a layer drawn over the feature found doesn't take its place once shown; with
    /// nothing asked, it shows at once.
    #[test]
    fn a_popup_waits_for_the_wms_answers_a_while() {
        let found = json!({"layer": "Countries", "attributes": {"name": "Switzerland"}});
        let mut pp = Popup::new(found, Coord::new(8.6, 46.65), 0, 0);
        assert!(!pp.asking(), "nothing asked");
        pp.asks.push(Ask { layer: "Over".into(), also: Vec::new(), capabilities: false, request: None, slot: Default::default() });
        assert!(pp.asking(), "asked");
        if let Some(earlier) = web_time::Instant::now().checked_sub(ASK_WAIT) {
            pp.since = earlier;
            assert!(!pp.asking(), "waited long enough");
        }
    }

    /// Answers that land while the pop-up waits open it on the top result, with its feature
    /// selected: drawn over Countries, Over's feature comes first, and Switzerland, selected by the
    /// click, no longer is.
    #[test]
    fn a_popup_opens_on_the_top_result_after_its_wait() {
        let mut s = rustgis_engine::Session::new(rustgis_engine::sample::world_project());
        let url = "https://wms.example.org/?SERVICE=WMS&VERSION=1.3.0&REQUEST=GetMap&LAYERS=over&STYLES=&CRS=EPSG:3857&BBOX={bbox-epsg-3857}&WIDTH=256&HEIGHT=256&FORMAT=image%2Fpng";
        s.run("map.addTiles", &json!({"url": url, "name": "Over", "overlay": true})).unwrap();
        s.run("map.popupMode", &json!({"mode": "visible"})).unwrap();
        let mut info = s.run("identify", &json!({"lon": 8.6, "lat": 46.65, "tolerancePx": 0})).unwrap();
        assert_eq!(info["layer"], "Countries");
        assert_eq!(info["featureInfo"][0]["tileLayer"], "Over", "{info}");
        s.run("select.rows", &json!({"layer": info["layerId"], "rows": [info["row"]]})).unwrap();
        // Its answer already in, not fetched.
        info.as_object_mut().unwrap().remove("featureInfo");
        let (map, opened) = (s.project.active_map, s.opened);
        let mut app = RustGisApp::new(s);
        let mut pp = Popup::new(info, Coord::new(8.6, 46.65), map, opened);
        let answer = br#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{"name":"Bern"},"geometry":null}]}"#;
        let slot = std::sync::Arc::new(std::sync::Mutex::new(Some(Ok(answer.to_vec()))));
        pp.asks.push(Ask { layer: "Over".into(), also: Vec::new(), capabilities: false, request: None, slot });
        app.map.popup = Some(pp);
        follow_asks(&mut app, &egui::Context::default());
        let pp = app.map.popup.as_ref().unwrap();
        let shown: Vec<_> =
            pp.info["results"].as_array().unwrap().iter().map(|r| r["tileLayer"].as_str().or(r["layer"].as_str()).unwrap()).collect();
        assert_eq!((shown, pp.index), (vec!["Over", "Countries"], 0));
        assert!(app.session.map().layer_by_name("Countries").unwrap().selection.is_empty());
    }

    /// A WMS layer's answer comes in the drawing order (#808): drawn over Countries, its feature
    /// comes first, and with topmost replaces Switzerland; drawn under the map's layers, it comes
    /// after them, and topmost keeps Switzerland.
    #[test]
    fn a_wms_answer_takes_its_place_in_the_drawing_order() {
        let mut s = rustgis_engine::Session::new(rustgis_engine::sample::world_project());
        s.run("map.addTiles", &json!({"url": "https://wms.example.org/{z}/{x}/{y}.png", "name": "Over", "overlay": true})).unwrap();
        s.run("map.addTiles", &json!({"url": "https://base.example.org/{z}/{x}/{y}.png", "name": "Under"})).unwrap();
        let answer = |layer: &str| json!({"layer": layer, "tileLayer": layer, "sourceLayer": null, "attributes": {"name": "Bern"}});
        let shown = |v: &Value| -> Vec<String> {
            v["results"].as_array().unwrap().iter().map(|r| r["tileLayer"].as_str().or(r["layer"].as_str()).unwrap().to_string()).collect()
        };
        for (mode, over, under) in
            [("topmost", vec!["Over"], vec!["Countries"]), ("visible", vec!["Over", "Countries"], vec!["Countries", "Under"])]
        {
            s.run("map.popupMode", &json!({"mode": mode})).unwrap();
            let info = s.run("identify", &json!({"lon": 8.6, "lat": 46.65, "tolerancePx": 0, "mode": mode})).unwrap();
            assert_eq!(info["layer"], "Countries", "{mode}");
            let v = with_answers(s.map(), &info, vec![answer("Over")]).unwrap();
            assert_eq!(shown(&v), over, "{mode}: drawn over");
            assert_eq!(v["layer"], over[0], "{mode}");
            let v = with_answers(s.map(), &info, vec![answer("Under")]).unwrap();
            assert_eq!(shown(&v), under, "{mode}: drawn under");
        }
        // Hidden since it answered (and took Switzerland's place): its result goes, and doesn't
        // keep out a later answer from a layer drawn under it.
        s.run("map.popupMode", &json!({"mode": "topmost"})).unwrap();
        let info = s.run("identify", &json!({"lon": 8.6, "lat": 46.65, "tolerancePx": 0})).unwrap();
        let over = with_answers(s.map(), &info, vec![answer("Over")]).unwrap();
        s.map_mut().tiles.iter_mut().filter(|t| t.name == "Over").for_each(|t| t.visible = false);
        let v = with_answers(s.map(), &over, vec![answer("Under")]).unwrap();
        assert_eq!(shown(&v), ["Under"], "hidden");
        // And with visible, which keeps everything, it isn't shown.
        s.map_mut().tiles.iter_mut().filter(|t| t.name == "Over").for_each(|t| t.visible = true);
        s.run("map.popupMode", &json!({"mode": "visible"})).unwrap();
        let info = s.run("identify", &json!({"lon": 8.6, "lat": 46.65, "tolerancePx": 0})).unwrap();
        let over = with_answers(s.map(), &info, vec![answer("Over")]).unwrap();
        s.map_mut().tiles.iter_mut().filter(|t| t.name == "Over").for_each(|t| t.visible = false);
        let v = with_answers(s.map(), &over, vec![answer("Under")]).unwrap();
        assert_eq!(shown(&v), ["Countries", "Under"], "hidden, visible");
        // Hidden with its result in the pop-up and no answer to come: the result goes at once.
        s.map_mut().tiles.iter_mut().filter(|t| t.name == "Over").for_each(|t| t.visible = true);
        let (map, opened) = (s.project.active_map, s.opened);
        let mut app = RustGisApp::new(s);
        let mut popup = Popup::new(over, Coord::new(8.6, 46.65), map, opened);
        popup.index = 1;
        app.map.popup = Some(popup);
        app.map.popup_rev = app.session.revision.wrapping_add(1);
        follow_popup(&mut app);
        assert_eq!(shown(&app.map.popup.as_ref().unwrap().info), ["Over", "Countries"], "drawn, in the pop-up");
        app.session.map_mut().tiles.iter_mut().filter(|t| t.name == "Over").for_each(|t| t.visible = false);
        app.map.popup_rev = app.session.revision.wrapping_add(1);
        follow_popup(&mut app);
        let popup = app.map.popup.as_ref().unwrap();
        assert_eq!((shown(&popup.info), popup.index), (vec!["Countries".to_string()], 0), "hidden, in the pop-up");
    }

    fn selection_harness() -> egui_kittest::Harness<'static, RustGisApp> {
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        app.run("layer.template", json!({"template": "points", "name": "Points"})).unwrap();
        for point in [[0.5, 0.5], [2.0, 2.0], [2.5, 0.5], [4.0, 4.0]] {
            app.run("edit.addFeature", json!({"layer": "Points", "geometry": {"type": "Point", "coordinates": point}})).unwrap();
        }
        app.run("select.clear", Value::Null).unwrap();
        let mut h =
            egui_kittest::Harness::builder().with_size(vec2(1400.0, 860.0)).with_step_dt(0.016).with_max_steps(10_000).build_ui_state(
                |ui, app: &mut RustGisApp| {
                    let ctx = ui.ctx().clone();
                    app.logic(&ctx);
                    app.ui(ui);
                },
                app,
            );
        h.step();
        h.state_mut().run("map.zoomToExtent", json!({"extent": [-1, -1, 5, 5]})).unwrap();
        for _ in 0..5 {
            h.step();
        }
        assert!(h.state().map.view_rect.is_some());
        h
    }

    fn selection_screen(h: &egui_kittest::Harness<'_, RustGisApp>, point: [f64; 2]) -> Pos2 {
        lonlat_to_screen(h.state(), h.state().map.view_rect.unwrap().0, Coord::new(point[0], point[1])).unwrap()
    }

    fn selection_pointer(h: &mut egui_kittest::Harness<'_, RustGisApp>, at: Pos2, pressed: Option<bool>, modifiers: egui::Modifiers) {
        h.event(egui::Event::ModifiersChanged(modifiers));
        h.event(egui::Event::PointerMoved(at));
        if let Some(pressed) = pressed {
            h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers });
        }
        h.step();
    }

    fn selection_click(h: &mut egui_kittest::Harness<'_, RustGisApp>, point: [f64; 2]) {
        let at = selection_screen(h, point);
        selection_pointer(h, at, Some(true), egui::Modifiers::NONE);
        selection_pointer(h, at, Some(false), egui::Modifiers::NONE);
    }

    fn selection_rows(h: &egui_kittest::Harness<'_, RustGisApp>) -> Vec<usize> {
        h.state().session.map().layer_by_name("Points").unwrap().selection.clone()
    }

    /// The Move Label tool: a label dragged and let go is kept where it was put, as one undo
    /// step; Esc while dragging, or another tool, drops it where it was; Shift-click puts a moved
    /// label back and leaves one never moved (no undo step).
    #[test]
    fn move_label_drags_cancels_and_puts_back() {
        let mut h = selection_harness();
        for (i, point) in [[0.5, 0.5], [3.5, 2.0], [2.5, 0.5], [1.0, 4.0]].into_iter().enumerate() {
            let p = json!({"layer": "Labelled", "geometry": {"type": "Point", "coordinates": point}, "attributes": {"name": format!("Place {i}")}});
            if i == 0 {
                h.state_mut().run("layer.template", json!({"template": "points", "name": "Labelled"})).unwrap();
            }
            h.state_mut().run("edit.addFeature", p).unwrap();
        }
        let id = h.state().session.map().layer_by_name("Labelled").unwrap().id;
        if let Some(l) = h.state_mut().session.map_mut().layer_mut(id) {
            l.labels.enabled = true;
            l.labels.default.field = "name".into();
        }
        h.state_mut().run("tool.set", json!({"tool": "moveLabel"})).unwrap();
        let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !h.state().map.labels.iter().any(|l| l.text == "Place 3") || h.state().map.drawing() {
            assert!(std::time::Instant::now() < end, "labels never drawn");
            std::thread::sleep(std::time::Duration::from_millis(2));
            h.step();
        }
        let rect = h.state().map.view_rect.unwrap().0;
        let grab = |h: &egui_kittest::Harness<'_, RustGisApp>| {
            let l = h.state().map.labels.iter().find(|l| l.text == "Place 3").unwrap();
            rect.min + vec2(l.x, l.y)
        };
        // Hit testing follows a retained frame's origin and refuses its old source window.
        let at = grab(&h);
        let retained = h.state().map.tex_rect;
        let shift = vec2(80.0, 80.0);
        h.state_mut().map.tex_rect = retained.translate(shift);
        assert_eq!(label_at(h.state(), rect, at + shift).unwrap().text, "Place 3");
        h.state_mut().map.tex_rect = retained;
        let key = h.state().map.key;
        h.state_mut().map.key.as_mut().unwrap().sources = rustgis_engine::map::source::generation().wrapping_add(1);
        assert!(label_at(h.state(), rect, at).is_none());
        h.state_mut().map.key = key;
        let moved = |h: &egui_kittest::Harness<'_, RustGisApp>| h.state().session.map().layer(id).unwrap().labels.default.moved.clone();
        let none = egui::Modifiers::NONE;
        let drag = |h: &mut egui_kittest::Harness<'_, RustGisApp>, escape: bool| {
            let at = grab(h);
            selection_pointer(h, at, None, none);
            selection_pointer(h, at, Some(true), none);
            for i in 1..=6 {
                selection_pointer(h, at + vec2(0.0, -8.0 * i as f32), None, none);
            }
            if escape {
                h.event(egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: none });
                h.step();
            }
            selection_pointer(h, at + vec2(0.0, -48.0), Some(false), none);
        };
        // Esc mid-drag: nothing saved.
        drag(&mut h, true);
        assert!(moved(&h).is_empty(), "Esc cancels the move");
        assert!(h.state().map.moving_label.is_none());
        // Dragged and let go: kept.
        h.state_mut().run("tool.set", json!({"tool": "moveLabel"})).unwrap();
        h.step();
        drag(&mut h, false);
        assert_eq!(moved(&h).len(), 1, "the move is kept");
        // Shift-click on the moved one: put back, and moved again by undo.
        let pinned = h.state().map.labels.iter().find(|l| l.collide.pinned).map(|l| rect.min + vec2(l.x, l.y));
        let shift = egui::Modifiers { shift: true, ..none };
        let at = pinned.expect("the moved label drawn");
        // A label never moved drawn over the moved one doesn't hide it from Shift-click.
        let mut over = h.state().map.labels.iter().find(|l| l.collide.pinned).cloned().expect("the moved label");
        (over.collide.pinned, over.text) = (false, "Over".into());
        h.state_mut().map.labels.push(over);
        assert_eq!(label_at(h.state(), rect, at).map(|l| l.text), Some("Over".into()), "the unmoved label is on top");
        assert!(pinned_label_at(h.state(), rect, at).is_some_and(|l| l.collide.pinned), "the moved one is still found");
        // Clicked with the unmoved label still on top: the moved one is put back.
        selection_pointer(&mut h, at, None, shift);
        selection_pointer(&mut h, at, Some(true), shift);
        selection_pointer(&mut h, at, Some(false), shift);
        assert!(moved(&h).is_empty(), "put back");
        h.state_mut().run("edit.undo", json!({})).unwrap();
        assert_eq!(moved(&h).len(), 1, "undone");
        let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !h.state().map.labels.iter().any(|l| l.collide.pinned) || h.state().map.drawing() {
            assert!(std::time::Instant::now() < end, "labels never drawn again");
            std::thread::sleep(std::time::Duration::from_millis(2));
            h.step();
        }
        // Shift-click on a label never moved: no undo step, so undo takes back the move.
        let other = h.state().map.labels.iter().find(|l| l.text == "Place 1" && !l.collide.pinned).map(|l| rect.min + vec2(l.x, l.y));
        let at = other.expect("another label drawn");
        selection_pointer(&mut h, at, Some(true), shift);
        selection_pointer(&mut h, at, Some(false), shift);
        h.state_mut().run("edit.undo", json!({})).unwrap();
        assert!(moved(&h).is_empty(), "the Shift-click left no step of its own");
    }

    /// #1321: the double-click delay applies to the map too, and its 1.2 s ceiling holds however
    /// slow the frames: two clicks 1.5 s apart at 0.75 s a frame stay two, and don't zoom.
    #[test]
    fn map_clicks_past_the_double_click_ceiling_do_not_zoom() {
        let app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1400.0, 860.0)).with_step_dt(0.75).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app,
        );
        h.run_steps(6);
        let at = h.state().map.view_rect.unwrap().0.center();
        let res = h.state().session.map().camera.res;
        crate::click_in_one_frame(&mut h, at, 0.75);
        crate::click_in_one_frame(&mut h, at, 1.5);
        assert_eq!(h.state().session.map().camera.res, res, "two separate clicks");
        crate::click_in_one_frame(&mut h, at, 1.0);
        assert_eq!(h.state().session.map().camera.res, res / 2.0, "a double-click inside the ceiling zooms in");
    }

    #[test]
    fn selection_tools_polygon_menu_repeat_and_fixed_default() {
        use egui_kittest::kittest::Queryable;
        let mut h = selection_harness();
        h.get_by_label("Select options").click();
        h.run();
        h.get_by_label("Select by Polygon").click();
        h.run();
        assert_eq!(h.state().session.tool, Tool::SelectPolygon);
        let camera = h.state().session.map().camera;
        let layers = h.state().session.map().layers.len();
        for _ in 0..2 {
            for point in [[0., 0.], [3., 0.], [3., 1.], [1., 1.], [1., 3.], [0., 3.]] {
                selection_click(&mut h, point);
            }
            selection_click(&mut h, [0., 0.]);
            assert_eq!(selection_rows(&h), vec![0, 2]);
            assert_eq!(h.state().session.tool, Tool::SelectPolygon);
            assert!(h.state().map.sketch.is_empty());
            assert!(h.state().map.selection_gesture.is_none());
            assert_eq!(h.state().session.map().camera, camera);
            assert_eq!(h.state().session.map().layers.len(), layers);
            assert_eq!(h.state().session.map().layer_by_name("Points").unwrap().dataset.features.len(), 4);
        }
        h.get_by_label("Select").click();
        h.run();
        assert_eq!(h.state().session.tool, Tool::Select);
        selection_click(&mut h, [2., 2.]);
        assert_eq!(selection_rows(&h), vec![1]);
    }

    #[test]
    fn selection_tools_polygon_survives_camera_and_viewport_changes() {
        let mut h = selection_harness();
        h.state_mut().run("tool.set", json!({"tool": "selectPolygon"})).unwrap();
        for point in [[0., 0.], [3., 0.], [3., 1.]] {
            selection_click(&mut h, point);
        }
        let sketch = h.state().map.sketch.clone();
        h.state_mut().session.map_mut().camera.center.x += 1000.0;
        h.step();
        assert_eq!(h.state().map.sketch, sketch, "camera movement preserves polygon vertices");
        assert!(h.state().map.selection_gesture.is_some());

        let rect = h.state().map.view_rect.unwrap().0;
        cancel_changed_selection(h.state_mut(), rect.translate(vec2(8.0, 0.0)));
        assert_eq!(h.state().map.sketch, sketch, "viewport movement preserves polygon vertices");
        assert!(h.state().map.selection_gesture.is_some());

        for point in [[1., 1.], [1., 3.], [0., 3.], [0., 0.]] {
            selection_click(&mut h, point);
        }
        assert_eq!(selection_rows(&h), vec![0, 2]);
        assert!(h.state().map.sketch.is_empty());
        assert!(h.state().map.selection_gesture.is_none());
    }

    #[test]
    fn selection_tools_lasso_release_and_modifiers() {
        let mut h = selection_harness();
        h.state_mut().run("tool.set", json!({"tool": "selectLasso"})).unwrap();
        let points = [[0., 0.], [3., 0.], [3., 1.], [1., 1.], [1., 3.], [0., 3.]];
        for modifiers in [
            egui::Modifiers::NONE,
            egui::Modifiers { shift: true, ..egui::Modifiers::NONE },
            egui::Modifiers { ctrl: true, command: true, ..egui::Modifiers::NONE },
        ] {
            let at = selection_screen(&h, points[0]);
            selection_pointer(&mut h, at, Some(true), modifiers);
            for point in &points[1..5] {
                let at = selection_screen(&h, *point);
                selection_pointer(&mut h, at, None, modifiers);
            }
            // The final corner appears only in the release frame.
            let at = selection_screen(&h, points[5]);
            selection_pointer(&mut h, at, Some(false), modifiers);
            assert_eq!(selection_rows(&h), if modifiers.ctrl { vec![] } else { vec![0, 2] });
            assert!(h.state().map.sketch.is_empty());
            assert!(h.state().map.drag.is_none());
            assert_eq!(h.state().session.tool, Tool::SelectLasso);
        }
    }

    #[test]
    fn selection_tools_radius_release_metric_and_plain_clicks() {
        let mut h = selection_harness();
        h.state_mut().run("tool.set", json!({"tool": "selectRadius"})).unwrap();
        let camera = h.state().session.map().camera;
        for _ in 0..2 {
            let center = selection_screen(&h, [0.5, 0.5]);
            selection_pointer(&mut h, center, Some(true), egui::Modifiers::NONE);
            let intermediate = selection_screen(&h, [3., 0.5]);
            selection_pointer(&mut h, intermediate, None, egui::Modifiers::NONE);
            let preview = h.state().map.radius_preview.as_ref().expect("live radius outline");
            assert!(!preview.2.is_empty());
            let sketch = &h.state().map.sketch;
            let expected = radius_distance_m(sketch[0], sketch[1]).unwrap();
            assert!((preview.1 - expected).abs() < 0.01);
            let end = selection_screen(&h, [1.5, 0.5]);
            selection_pointer(&mut h, end, Some(false), egui::Modifiers::NONE);
            assert_eq!(selection_rows(&h), vec![0]);
            assert_eq!(h.state().session.tool, Tool::SelectRadius);
            assert!(h.state().map.radius_preview.is_none());
        }
        for tool in ["selectLasso", "selectRadius"] {
            h.state_mut().run("tool.set", json!({"tool": tool})).unwrap();
            selection_click(&mut h, [2., 2.]);
            selection_click(&mut h, [2., 2.]);
            assert_eq!(selection_rows(&h), vec![0]);
            assert_eq!(h.state().session.map().camera, camera);
        }
    }

    #[test]
    fn selection_tools_cancelled_drag_cannot_commit_on_release() {
        let mut h = selection_harness();
        h.state_mut().run("select.rows", json!({"layer": "Points", "rows": [3]})).unwrap();
        for tool in ["selectLasso", "selectRadius"] {
            for cancel in ["same", "direct", "camera", "projection", "escape"] {
                h.state_mut().run("tool.set", json!({"tool": tool})).unwrap();
                let start = selection_screen(&h, [0., 0.]);
                let end = selection_screen(&h, [3., 3.]);
                selection_pointer(&mut h, start, Some(true), egui::Modifiers::NONE);
                selection_pointer(&mut h, end, None, egui::Modifiers::NONE);
                assert!(h.state().map.selection_gesture.is_some());
                assert!(h.state_mut().run("tool.set", json!({"tool": "not-a-tool"})).is_err());
                assert!(h.state().map.selection_gesture.is_some());
                match cancel {
                    "same" => {
                        h.state_mut().run("tool.set", json!({"tool": tool})).unwrap();
                    }
                    "direct" => h.state_mut().session.tool = Tool::Select,
                    "camera" => h.state_mut().session.map_mut().camera.center.x += 1.0,
                    "projection" => {
                        h.state_mut().run("map.projection", json!({"type": "robinson"})).unwrap();
                    }
                    _ => h.event(egui::Event::Key {
                        key: egui::Key::Escape,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    }),
                }
                h.step();
                selection_pointer(&mut h, end, Some(false), egui::Modifiers::NONE);
                assert_eq!(selection_rows(&h), vec![3], "{tool}: {cancel}");
                assert!(h.state().map.selection_gesture.is_none());
                assert!(h.state().map.drag.is_none());
                h.state_mut().run("map.projection", json!({"type": "mercator"})).unwrap();
                h.step();
            }
        }
    }

    #[test]
    fn a_globe_visibility_preview_drops_the_old_labels() {
        let mut app = RustGisApp::with_sample();
        app.run("map.body", json!({"name": "moon", "basemap": false})).expect("Moon");
        app.run("map.projection", json!({"type": "globe"})).expect("globe");
        let mut h = egui_kittest::Harness::builder().with_size(vec2(900.0, 550.0)).with_max_steps(10_000).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app,
        );
        let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while h.state().map.key.is_none() || h.state().map.drawing() {
            assert!(std::time::Instant::now() < end, "exact globe frame did not arrive");
            std::thread::sleep(std::time::Duration::from_millis(2));
            h.step();
        }
        assert!(!h.state().map.labels.is_empty(), "the sample has labels");
        h.state_mut().run("layer.visibility", json!({"layer": "Countries", "visible": false})).expect("hide countries");
        h.step();
        assert!(h.state().map.labels.is_empty(), "the preview kept labels from the old visibility state");
    }

    /// A frame is kept only through changes that leave the same layers shown in the same order.
    #[test]
    fn the_shown_layers_change_with_visibility_order_and_tile_placement_not_style() {
        let mut app = RustGisApp::with_sample();
        app.run("map.addTiles", json!({"url": "http://127.0.0.1:9/{z}/{x}/{y}.png", "name": "Offline"})).expect("tile layer");
        let before = shown_layers(&app.session);
        app.run("layer.transparency", json!({"layer": "Countries", "value": 50})).expect("transparency");
        assert_eq!(shown_layers(&app.session), before, "a style change");
        let top = app.session.map().layers.first().map(|l| l.id);
        app.session.map_mut().tiles[0].over = top;
        assert_ne!(shown_layers(&app.session), before, "a tile layer moved over a map layer");
        app.session.map_mut().tiles[0].over = None;
        app.session.map_mut().tiles[0].time_hidden = true;
        assert_ne!(shown_layers(&app.session), before, "a tile layer hidden by the time slider");
        app.session.map_mut().tiles[0].time_hidden = false;
        app.session.map_mut().graticule = !app.session.map().graticule;
        assert_ne!(shown_layers(&app.session), before, "the graticule turned on or off");
        app.session.map_mut().graticule = !app.session.map().graticule;
        app.session.map_mut().layers.swap(0, 1);
        assert_ne!(shown_layers(&app.session), before, "layers reordered");
        app.session.map_mut().layers.swap(0, 1);
        assert_eq!(shown_layers(&app.session), before, "back as they were");
        app.run("layer.visibility", json!({"layer": "Countries", "visible": false})).expect("hide");
        assert_ne!(shown_layers(&app.session), before, "a layer hidden");
    }

    /// #1051: changing a layer's transparency over a tiled basemap kept the exact frame on
    /// screen until the next one arrived; it showed a coarse preview of the whole map, basemap
    /// included, so every step of a slider drag made the map flicker.
    #[test]
    fn a_style_change_over_tiles_keeps_the_frame_instead_of_a_preview() {
        let _turn = crate::fetching::turn();
        let mut app = RustGisApp::with_sample();
        // Nothing listens there: the tiles fail, the map still draws.
        app.run("map.addTiles", json!({"url": "http://127.0.0.1:9/{z}/{x}/{y}.png", "name": "Offline"})).expect("tile layer");
        // Short steps: a kept frame gives way to a preview after `KEEP_FRAME_S` of the harness's time,
        // 6000 steps here (at least 12 s with the waits below), far longer than a debug frame takes.
        let mut h =
            egui_kittest::Harness::builder().with_size(vec2(900.0, 550.0)).with_step_dt(0.000_05).with_max_steps(10_000).build_ui_state(
                |ui, app: &mut RustGisApp| {
                    let ctx = ui.ctx().clone();
                    app.logic(&ctx);
                    app.ui(ui);
                },
                app,
            );
        let exact = |h: &mut egui_kittest::Harness<'_, RustGisApp>, what: &str| {
            let end = std::time::Instant::now() + std::time::Duration::from_secs(20);
            loop {
                h.step();
                let st = h.state();
                if st.map.key.is_some_and(|k| k.rev == st.session.revision) && !st.map.tilt_settle.busy() {
                    break;
                }
                assert!(std::time::Instant::now() < end, "{what}");
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        };
        exact(&mut h, "the first exact frame did not arrive");
        // A quick map (a debug build may draw slower than a release one).
        h.state_mut().map.render_ms = 20;
        let before = h.state().map.key.expect("a frame");
        h.state_mut().run("layer.transparency", json!({"layer": "Countries", "value": 50})).expect("transparency");
        h.step();
        let kept = h.state().map.key.expect("a frame");
        assert_eq!((kept.rev, kept.draft), (before.rev, before.draft), "a coarse preview replaced the frame on screen");
        exact(&mut h, "the exact frame of the change did not arrive");
        // Changed again while the exact frame of a change is drawn: that frame, of an earlier
        // change, is shown when it finishes, before the frame of the latest.
        h.state_mut().map.render_ms = 20;
        let shown = h.state().map.key.expect("a frame").rev;
        h.state_mut().run("layer.transparency", json!({"layer": "Countries", "value": 20})).expect("transparency");
        h.step();
        let first = h.state().session.revision;
        assert!(h.state().map.tilt_settle.busy(), "the exact frame of the change is being drawn");
        h.state_mut().run("layer.transparency", json!({"layer": "Countries", "value": 30})).expect("transparency");
        let end = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while h.state().map.key.is_some_and(|k| k.rev == shown) {
            assert!(std::time::Instant::now() < end, "no frame of the changes arrived");
            std::thread::sleep(std::time::Duration::from_millis(2));
            h.step();
        }
        assert_eq!(h.state().map.key.map(|k| k.rev), Some(first), "the earlier change's frame was shown first");
        exact(&mut h, "the exact frame of the latest change did not arrive");
        // Hiding a layer isn't kept: its preview shows at once.
        h.state_mut().map.render_ms = 20;
        h.state_mut().run("layer.visibility", json!({"layer": "Countries", "visible": false})).expect("hide");
        h.step();
        let st = h.state();
        assert!(st.map.key.is_some_and(|k| k.rev == st.session.revision), "the hidden layer stayed on screen");
    }

    /// Dragging a side pane resizes the map view every frame. The map is not drawn again for
    /// each size, which made the drag stutter: the last frame stays until the size holds, then
    /// the map is drawn once at it.
    #[test]
    fn a_pane_drag_draws_the_map_once_it_stops() {
        let mut h =
            egui_kittest::Harness::builder().with_size(vec2(1200.0, 700.0)).with_step_dt(1.0 / 60.0).with_max_steps(10_000).build_ui_state(
                |ui, app: &mut RustGisApp| {
                    let ctx = ui.ctx().clone();
                    app.logic(&ctx);
                    app.ui(ui);
                },
                RustGisApp::with_sample(),
            );
        let settled = |h: &mut egui_kittest::Harness<'_, RustGisApp>, what: &str| {
            let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while h.state().map.key.is_none_or(|k| k.draft || k.size != h.state().map.view_px) || h.state().map.drawing() {
                assert!(std::time::Instant::now() < end, "{what}");
                std::thread::sleep(std::time::Duration::from_millis(2));
                h.step();
            }
        };
        settled(&mut h, "the first frame did not arrive");
        let before = h.state().map.key.map(|k| k.size);
        // The window narrowing a little each frame, as the map does while a pane is dragged:
        // 5 frames at 1/60 s each, well inside `RESIZE_SETTLE`.
        for i in 1..=5 {
            h.set_size(vec2(1200.0 - 10.0 * i as f32, 700.0));
            h.step();
            assert_eq!(h.state().map.key.map(|k| k.size), before, "the map was drawn again mid-drag (frame {i})");
        }
        // Held still: drawn at the new size.
        settled(&mut h, "the map was not drawn once the drag stopped");
        assert_ne!(h.state().map.key.map(|k| k.size), before);
    }

    /// Frames drawn mid-motion stay at full resolution unless the map draws slowly, and even
    /// then are never drawn below half resolution (a blurry map while panning and zooming).
    #[test]
    fn motion_frames_stay_sharp_unless_slow() {
        assert_eq!(motion_resolution(0.0, 1.0), 1.0, "unmeasured cost");
        assert_eq!(motion_resolution(12.0, 1.0), 1.0, "a quick map");
        assert_eq!(motion_resolution(12.0, GAP_SPAN * GAP_SPAN), 1.0, "a quick map's gap frame");
        let slow = motion_resolution(64.0, 1.0);
        assert!((0.5..1.0).contains(&slow), "a slow map draws coarser: {slow}");
        assert_eq!(motion_resolution(10_000.0, 4.0), 0.5, "never below half");
        assert_eq!(motion_resolution(f64::NAN, 1.0), 1.0);
        assert_eq!(motion_resolution(f64::INFINITY, 1.0), 0.5);
        // A gap frame over 1.5 times the view at full resolution costs 2.25 views.
        let gap = View { width: 1200.0, height: 900.0, pixels_per_point: 2.0, camera: Camera::world(), sky: true };
        assert!((full_cost(45.0, &gap, (800.0, 600.0), 2.0) - 20.0).abs() < 1e-9);
        let coarse = View { pixels_per_point: 1.0, ..gap };
        assert!((full_cost(45.0, &coarse, (800.0, 600.0), 2.0) - 80.0).abs() < 1e-9);
    }

    /// The GPU fast path moves the last frame's meshes only while they cover the view: zooming
    /// out or panning past the cull margin re-renders, or features culled from the last frame
    /// would vanish until the motion stopped.
    #[test]
    fn gpu_fast_path_needs_the_scene_to_cover_the_view() {
        let scene = RustGisApp::with_sample().session.map().camera;
        let (w, h) = (800.0, 600.0);
        assert!(scene_covers(scene, scene, w, h));
        assert!(scene_covers(scene, Camera { res: scene.res * 0.5, ..scene }, w, h), "zooming in");
        assert!(scene_covers(scene, Camera { res: scene.res * 1.9, ..scene }, w, h), "zooming out within the margin");
        assert!(!scene_covers(scene, Camera { res: scene.res * 2.5, ..scene }, w, h), "zooming out past it");
        let panned = |dx: f64| Camera { center: Coord::new(scene.center.x + dx * scene.res, scene.center.y), ..scene };
        assert!(scene_covers(scene, panned(350.0), w, h), "panning within the margin");
        assert!(!scene_covers(scene, panned(450.0), w, h), "panning past it");
    }

    #[test]
    fn fps_counts_the_last_second_and_idle_is_none() {
        let mut mv = MapView::default();
        assert_eq!(mv.fps(0.0), None, "no frames yet");
        for i in 0..10 {
            mv.record_frame(i as f64 * 0.05);
        }
        assert_eq!(mv.fps(0.46), Some(10), "all ten frames inside the window");
        // A second burst later: the first burst has aged out of the 1 s window.
        for i in 0..10 {
            mv.record_frame(1.2 + i as f64 * 0.05);
        }
        assert_eq!(mv.fps(1.7), Some(10), "only the new burst counts");
        assert_eq!(mv.fps(2.3), None, "idle (no frame in 500 ms)");
    }

    /// A text graphic's style is measured as it is drawn (#535): letter-spaced, it is hit across
    /// the width its spacing gives it, as the map view lays it out.
    #[test]
    fn a_styled_text_graphic_is_hit_as_drawn() {
        let mut app = RustGisApp::with_sample();
        let c = app.session.map().center_lonlat();
        let r = app.run("graphics.add", json!({"shape": "text", "points": [[c.x, c.y]], "text": "Wide", "fontSize": 20})).unwrap();
        let (layer, row) = (r["layer"].clone(), r["row"].clone());
        let hit = r["layer"].as_u64().zip(r["row"].as_u64()).map(|(l, r)| (l as u32, r as usize));
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0));
        // Past the plain text's right end, within the spaced text's.
        let probe = |app: &RustGisApp| {
            let mut found = None;
            let mut out = egui::Context::default().run_ui(Default::default(), |ui| {
                let at = lonlat_to_screen(app, rect, c).unwrap();
                found = text_graphic_at(app, ui.ctx(), rect, at + vec2(50.0, 0.0));
            });
            out.textures_delta.clear();
            found
        };
        assert_eq!(probe(&app), None, "plain, the text ends before");
        app.run("graphics.edit", json!({"layer": layer, "row": row, "letterSpacing": 20})).unwrap();
        assert_eq!(probe(&app), hit, "spaced, it reaches there");
    }

    /// A text graphic is found anywhere on its text, not only at its anchor (#420).
    #[test]
    fn text_graphics_are_hit_on_their_text() {
        let mut app = RustGisApp::with_sample();
        let c = app.session.map().center_lonlat();
        let r = app
            .run("graphics.add", json!({"shape": "text", "points": [[c.x, c.y]], "text": "A long piece of text", "fontSize": 20}))
            .unwrap();
        let hit = r["layer"].as_u64().zip(r["row"].as_u64()).map(|(l, r)| (l as u32, r as usize));
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0));
        let mut out = egui::Context::default().run_ui(Default::default(), |ui| {
            let at = lonlat_to_screen(&app, rect, c).unwrap();
            assert_eq!(text_graphic_at(&app, ui.ctx(), rect, at), hit);
            assert_eq!(text_graphic_at(&app, ui.ctx(), rect, at + vec2(60.0, 0.0)), hit, "on the text, off its anchor");
            assert_eq!(text_graphic_at(&app, ui.ctx(), rect, at + vec2(0.0, 40.0)), None);
        });
        out.textures_delta.clear();
        // Dynamic text is measured as drawn ("1:20,000,000"), wider than its token ("{scale}").
        let r =
            app.run("graphics.add", json!({"shape": "text", "points": [[c.x, c.y - 30.0]], "text": "{scale}", "fontSize": 20})).unwrap();
        let hit = r["layer"].as_u64().zip(r["row"].as_u64()).map(|(l, r)| (l as u32, r as usize));
        let mut out = egui::Context::default().run_ui(Default::default(), |ui| {
            let at = lonlat_to_screen(&app, rect, Coord::new(c.x, c.y - 30.0)).unwrap();
            assert_eq!(text_graphic_at(&app, ui.ctx(), rect, at + vec2(50.0, 0.0)), hit, "on the scale it shows");
        });
        out.textures_delta.clear();
        // Text that isn't drawn (its layer's group is hidden) can't be hit.
        let layer = r["layer"].clone();
        app.run("group.add", json!({"name": "Notes", "layers": [layer]})).unwrap();
        app.run("group.set", json!({"group": "Notes", "visible": false})).unwrap();
        let mut out = egui::Context::default().run_ui(Default::default(), |ui| {
            let at = lonlat_to_screen(&app, rect, Coord::new(c.x, c.y - 30.0)).unwrap();
            assert_eq!(text_graphic_at(&app, ui.ctx(), rect, at), None);
        });
        out.textures_delta.clear();
        // Nor text clipped away by the view's clipping distance (it sits ~3,300 km from the
        // centre), or on a fully transparent layer.
        app.run("group.set", json!({"group": "Notes", "visible": true})).unwrap();
        let probe = |app: &RustGisApp| {
            let mut found = None;
            let mut out = egui::Context::default().run_ui(Default::default(), |ui| {
                let at = lonlat_to_screen(app, rect, Coord::new(c.x, c.y - 30.0)).unwrap();
                found = text_graphic_at(app, ui.ctx(), rect, at + vec2(50.0, 0.0));
            });
            out.textures_delta.clear();
            found
        };
        assert_eq!(probe(&app), hit);
        app.run("map.clipping", json!({"distance": 1_000_000.0})).unwrap();
        assert_eq!(probe(&app), None, "beyond the clipping distance");
        app.run("map.clipping", json!({"mode": "none"})).unwrap();
        assert_eq!(probe(&app), hit);
        // Nor text on a layer that isn't selectable.
        app.run("layer.selectable", json!({"layer": layer, "on": false})).unwrap();
        assert_eq!(probe(&app), None, "not selectable");
        app.run("layer.selectable", json!({"layer": layer, "on": true})).unwrap();
        assert_eq!(probe(&app), hit);
        app.run("group.set", json!({"group": "Notes", "transparency": 100})).unwrap();
        assert_eq!(probe(&app), None, "on a fully transparent layer");
    }

    /// Trace follows the edge under two clicks; finishing a Reshape or Cut Polygon sketch (what a
    /// double-click does) runs its command on the drawn line and clears the sketch.
    #[test]
    fn trace_and_finishing_reshape_and_cut_sketches_edit_the_selection() {
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        app.run("layer.template", json!({"template": "polygons", "name": "Parcels"})).unwrap();
        let square = json!({"type": "Polygon", "coordinates": [[[0, 0], [1, 0], [1, 1], [0, 1], [0, 0]]]});
        app.run("edit.addFeature", json!({"layer": "Parcels", "geometry": square})).unwrap();
        app.session.zoom_to(&rustgis_engine::geom::Extent::new(-1.0, -1.0, 2.0, 2.0));
        assert_eq!(
            traced(&mut app, Coord::new(0.5, 0.0), Coord::new(1.0, 0.5)),
            Some(vec![Coord::new(0.5, 0.0), Coord::new(1.0, 0.0), Coord::new(1.0, 0.5)])
        );
        // From just off the edge, the path starts on it (and replaces the last vertex).
        let off = traced(&mut app, Coord::new(0.5, -0.001), Coord::new(1.0, 0.5)).unwrap();
        assert!(off[0].y.abs() < 1e-9 && (off[0].x - 0.5).abs() < 1e-9, "{off:?}");
        let count = |app: &RustGisApp| app.session.map().layer_by_name("Parcels").unwrap().dataset.features.len();
        app.run("tool.set", json!({"tool": "reshape"})).unwrap();
        app.map.sketch = vec![Coord::new(0.2, 0.2), Coord::new(-0.5, 0.2), Coord::new(-0.5, 0.8), Coord::new(0.2, 0.8)];
        finish_sketch(&mut app);
        assert!(app.map.sketch.is_empty());
        let e = app.session.map().layer_by_name("Parcels").unwrap().dataset.features[0].geometry.as_ref().unwrap().extent();
        assert!((e.min_x + 0.5).abs() < 1e-9, "{e:?}");
        app.run("tool.set", json!({"tool": "cutPolygon"})).unwrap();
        app.map.sketch = vec![Coord::new(0.5, -1.0), Coord::new(0.5, 2.0)];
        finish_sketch(&mut app);
        assert!(app.map.sketch.is_empty());
        assert_eq!(count(&app), 2);
    }

    /// A unit-square parcel on the map, with Trace on and the Create Polygon tool.
    fn trace_harness() -> egui_kittest::Harness<'static, RustGisApp> {
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        app.run("layer.template", json!({"template": "polygons", "name": "Parcels"})).unwrap();
        let square = json!({"type": "Polygon", "coordinates": [[[0, 0], [1, 0], [1, 1], [0, 1], [0, 0]]]});
        app.run("edit.addFeature", json!({"layer": "Parcels", "geometry": square})).unwrap();
        let mut h =
            egui_kittest::Harness::builder().with_size(vec2(1400.0, 860.0)).with_step_dt(0.016).with_max_steps(10_000).build_ui_state(
                |ui, app: &mut RustGisApp| {
                    let ctx = ui.ctx().clone();
                    app.logic(&ctx);
                    app.ui(ui);
                },
                app,
            );
        h.step();
        h.state_mut().run("map.zoomToExtent", json!({"extent": [-1, -1, 2, 2]})).unwrap();
        for _ in 0..5 {
            h.step();
        }
        h.state_mut().run("ui.traceEdges", json!({"on": true})).unwrap();
        h.state_mut().run("tool.set", json!({"tool": "createPolygon"})).unwrap();
        h
    }

    /// With Trace on, two clicks on a polygon's edges draw the edge between them: the corner
    /// between the clicks joins the sketch.
    #[test]
    fn clicks_with_trace_on_follow_the_edge_round_a_corner() {
        let mut h = trace_harness();
        selection_click(&mut h, [0.5, 0.0]);
        for _ in 0..30 {
            h.step();
        }
        selection_click(&mut h, [1.0, 0.5]);
        let sketch = h.state().map.sketch.clone();
        assert_eq!(sketch.len(), 3, "{sketch:?}");
        assert_eq!(sketch[1], Coord::new(1.0, 0.0), "the corner, exactly");
        assert!(sketch[0].y.abs() < 1e-9 && (sketch[2].x - 1.0).abs() < 1e-9, "both ends on the edge: {sketch:?}");
    }

    /// A first click just off the edge, then one on it with Trace on: the sketch starts on the edge.
    #[test]
    fn a_trace_from_a_click_just_off_the_edge_starts_on_it() {
        let mut h = trace_harness();
        selection_click(&mut h, [0.5, -0.005]);
        for _ in 0..30 {
            h.step();
        }
        selection_click(&mut h, [1.0, 0.5]);
        let sketch = h.state().map.sketch.clone();
        assert_eq!(sketch.len(), 3, "{sketch:?}");
        assert!(sketch[0].y.abs() < 1e-9, "the first vertex moved onto the edge: {sketch:?}");
    }

    /// A click with Trace on but no edge near is a plain vertex, with no notice. A Cut Polygon sketch
    /// reaches the 10,000-point limit through a trace, but not past it: a
    /// trace that would pass it adds the click alone, and a full sketch takes no more points.
    #[test]
    fn a_cut_sketch_stops_at_the_line_limit() {
        let limit = rustgis_engine::cmd::design::MAX_LINE;
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        app.run("layer.template", json!({"template": "polygons", "name": "Parcels"})).unwrap();
        let square = json!({"type": "Polygon", "coordinates": [[[0, 0], [1, 0], [1, 1], [0, 1], [0, 0]]]});
        app.run("edit.addFeature", json!({"layer": "Parcels", "geometry": square})).unwrap();
        app.session.zoom_to(&rustgis_engine::geom::Extent::new(-1.0, -1.0, 2.0, 2.0));
        app.map.trace_edges = true;
        // A click away from any edge is an ordinary vertex: no trace, and no notice.
        app.map.sketch = vec![Coord::new(5.0, 5.0)];
        let mark = app.session.notice_mark();
        add_vertex(&mut app, Tool::CutPolygon, Coord::new(6.0, 6.0));
        assert_eq!((app.map.sketch.len(), app.session.notices_since(mark).len()), (2, 0));
        let start = |n: usize| {
            let mut v = vec![Coord::new(0.5, -0.5); n - 1];
            v.push(Coord::new(0.5, 0.0));
            v
        };
        // The trace (start, corner, click) replaces the last vertex: exactly the limit.
        app.map.sketch = start(limit - 2);
        add_vertex(&mut app, Tool::CutPolygon, Coord::new(1.0, 0.5));
        assert_eq!(app.map.sketch.len(), limit);
        assert_eq!(app.map.sketch[limit - 2], Coord::new(1.0, 0.0), "traced");
        // One more would pass it: the click alone, and a notice.
        app.map.sketch = start(limit - 1);
        let mark = app.session.notice_mark();
        add_vertex(&mut app, Tool::CutPolygon, Coord::new(1.0, 0.5));
        assert_eq!(app.map.sketch.len(), limit);
        assert_eq!(app.session.notices_since(mark).len(), 1);
        // Full: nothing more is added, and the limit was already said for this sketch.
        add_vertex(&mut app, Tool::CutPolygon, Coord::new(0.2, 0.2));
        assert_eq!((app.map.sketch.len(), app.session.notices_since(mark).len()), (limit, 1));
        // A new sketch that fills up says so once, for Reshape too.
        app.map.cancel_sketch();
        app.map.sketch = start(limit);
        let mark = app.session.notice_mark();
        add_vertex(&mut app, Tool::Reshape, Coord::new(0.2, 0.2));
        add_vertex(&mut app, Tool::Reshape, Coord::new(0.3, 0.3));
        assert_eq!((app.map.sketch.len(), app.session.notices_since(mark).len()), (limit, 1));
        // Drawing a new feature doesn't have the limit.
        add_vertex(&mut app, Tool::CreateLine, Coord::new(0.2, 0.2));
        assert_eq!(app.map.sketch.len(), limit + 1);
    }
}
