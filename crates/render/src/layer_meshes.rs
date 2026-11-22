//! Each layer's GPU mesh kept from one scene to the next (#1057): restyling, editing or
//! filtering one layer of a map drawn on the GPU tessellates that layer again, not every layer.
//!
//! A layer's mesh ([`crate::mesh::collect_layer`]) is made from the layer, the geometry copy it
//! draws, the rows it draws, and the view: which features reach it (the GPU's cull, wider than
//! the view so the camera can move the scene), the 0.35 px decimation, the stroke widths in
//! pixels and where f32 holds its vertices. A kept mesh is taken again when all of those are
//! as they were: the layer by its look ([`LayerLook`]), the rest by [`Made`] and the rows drawn.
//! The mesh taken is then the one tessellating the layer again would make, vertex for vertex.
//!
//! A pan or a zoom makes the layers again. The GPU map moves a scene with the camera over the
//! ground it was drawn for, so a scene must reach past the view as far as a new one would; a
//! kept mesh panned to doesn't. Zoomed in, its decimation would be coarser than a new one's, and
//! zoomed out it reaches less far.
//!
//! Only screen scenes keep meshes, not a turned or tilted view's bands (their own views, sized
//! again after) nor exports, and only layers with a look: a layer read from its source draws
//! what each read gives. The meshes are shared with the scenes that draw them (`Arc`), so a
//! scene shown costs no more; what this keeps beside it is the last scene's meshes, within
//! [`MAX_BYTES`].

use std::sync::Arc;

use rustgis_geom::Extent;
use rustgis_map::Projection;
use rustgis_map::look::{Change, LayerLook};

use crate::gpu_scene::{DrawCmd, MeshVertex, PointInstance};

/// The most bytes of meshes kept, with what each was made from ([`ENTRY_BYTES`] and its rows):
/// about 3.3 million vertices on the desktop and 0.8 million in the browser. Past it, the meshes
/// used longest ago go first, but never one a scene being drawn has used ([`Scene`]): when the
/// newest doesn't fit beside those, it isn't kept. A layer's mesh is at most
/// [`crate::mesh::MAX_LAYER_VERTICES`] vertices, 80 MB.
pub(crate) const MAX_BYTES: usize = if cfg!(target_arch = "wasm32") { 32 << 20 } else { 128 << 20 };

/// What a kept mesh is counted as holding beside its data and rows: its layer's look (the
/// layer's settings, its renderer's classes among them) and the rest of the entry. A layer's
/// selection, which the look copies, is counted on top. A look can hold more (its fields' value
/// lists, say), but a layer keeps one, shared by its views, and drops the meshes of a look it no
/// longer has: what this holds of a layer's settings is one copy of them at most.
const ENTRY_BYTES: usize = 16 << 10;

/// The views a layer keeps meshes for: the one on screen and the one before it (a map and its
/// split view, say).
const VIEWS: usize = 2;

/// Which copy of a layer's geometry its mesh is made from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Drawn {
    /// Its rows projected, as they are.
    Projected,
    /// Thinned for the view's level of detail ([`rustgis_map::lod`]).
    Thinned(Option<i32>),
    /// The display copy of that level, from longitude/latitude rows.
    Display(Option<i32>),
}

/// What a layer's mesh is made from besides the layer and its rows.
#[derive(Clone, Debug)]
pub(crate) struct Made {
    /// The map's projection, which the geometry copies are in.
    pub projection: Projection,
    pub drawn: Drawn,
    /// The features kept: those reaching this extent (`GPU_CULL_MARGIN` around the view).
    pub cull: Extent,
    /// Map units per physical pixel: the decimation, the stroke widths and the f32 reach.
    pub map_per_px: f64,
    /// Physical pixels per point, a reference scale's growth included: marker and stroke sizes.
    pub ppp: f32,
    pub wireframe: bool,
}

impl Made {
    /// Whether a mesh made from `self` is the one `other` makes.
    fn same(&self, other: &Made) -> bool {
        let extent = |e: &Extent| [e.min_x, e.min_y, e.max_x, e.max_y].map(f64::to_bits);
        self.projection == other.projection
            && self.drawn == other.drawn
            && extent(&self.cull) == extent(&other.cull)
            && self.map_per_px.to_bits() == other.map_per_px.to_bits()
            && self.ppp.to_bits() == other.ppp.to_bits()
            && self.wireframe == other.wireframe
    }
}

/// One layer's mesh and what it was made from.
struct Kept {
    /// The live layer it was made for ([`rustgis_map::Layer::instance_id`]: a layer's id repeats
    /// from map to map).
    layer: u64,
    /// Shared by the layer's views: a layer keeps one look, the one it was last drawn with.
    look: Arc<LayerLook>,
    made: Made,
    /// The rows drawn: the definition query, the display filters at the view's scale and the
    /// time shown, as they applied.
    rows: Vec<bool>,
    /// The layer's Mesh and Points commands, sharing their data with the scenes that drew them.
    cmds: Vec<DrawCmd>,
    /// The mesh's vertices, which the layer's vertex limit counts.
    vertices: usize,
    bytes: usize,
    /// When it was last taken or kept ([`Meshes::clock`]).
    used: u64,
}

#[derive(Default)]
struct Meshes {
    kept: Vec<Kept>,
    clock: u64,
    /// The scenes being drawn ([`Scene`]), by number, with the clock when each began.
    drawing: Vec<(u64, u64)>,
    scenes: u64,
}

impl Meshes {
    /// Whether a scene being drawn may have used `k`: taken or kept since the first of them began.
    fn in_use(&self, k: &Kept) -> bool {
        self.drawing.iter().map(|d| d.1).min().is_some_and(|began| k.used > began)
    }
}

/// A command sharing `cmd`'s data, when it is a mesh or points.
fn share(cmd: &DrawCmd) -> Option<DrawCmd> {
    match cmd {
        DrawCmd::Mesh { origin, vertices, indices } => {
            Some(DrawCmd::Mesh { origin: *origin, vertices: Arc::clone(vertices), indices: Arc::clone(indices) })
        }
        DrawCmd::Points { origin, instances } => Some(DrawCmd::Points { origin: *origin, instances: Arc::clone(instances) }),
        _ => None,
    }
}

/// The bytes `cmds` hold, as allocated.
pub(crate) fn bytes_of(cmds: &[DrawCmd]) -> usize {
    cmds.iter()
        .map(|c| match c {
            DrawCmd::Mesh { vertices, indices, .. } => {
                vertices.capacity() * std::mem::size_of::<MeshVertex>() + indices.capacity() * std::mem::size_of::<u32>()
            }
            DrawCmd::Points { instances, .. } => instances.capacity() * std::mem::size_of::<PointInstance>(),
            _ => 0,
        })
        .sum()
}

/// The kept meshes, shared by the threads that draw scenes (the UI's and the one that draws a
/// view once it settles).
#[cfg(not(test))]
static MESHES: std::sync::Mutex<Meshes> = std::sync::Mutex::new(Meshes { kept: Vec::new(), clock: 0, drawing: Vec::new(), scenes: 0 });

/// Runs `f` on the kept meshes.
#[cfg(not(test))]
fn with_meshes<R>(f: impl FnOnce(&mut Meshes) -> R) -> R {
    // A thread that panicked while holding them left nothing half done: each is replaced whole.
    f(&mut MESHES.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
}

// Each test thread keeps its own, so tests running side by side don't take each other's.
#[cfg(test)]
thread_local! {
    static MESHES: std::cell::RefCell<Meshes> = std::cell::RefCell::new(Meshes::default());
    /// The layers whose meshes scenes took, by id.
    static TAKEN: std::cell::RefCell<Vec<u32>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
fn with_meshes<R>(f: impl FnOnce(&mut Meshes) -> R) -> R {
    MESHES.with_borrow_mut(f)
}

/// A scene being drawn, from [`Scene::begin`] until it is dropped (when the scene is done,
/// stopped or failed): while it lasts, no mesh a scene being drawn has used goes, though another
/// scene drawn at the same time (on the UI's thread and a worker's) keeps meshes too.
pub(crate) struct Scene(u64);

impl Scene {
    pub(crate) fn begin() -> Scene {
        with_meshes(|m| {
            m.scenes += 1;
            let n = m.scenes;
            m.drawing.push((n, m.clock));
            Scene(n)
        })
    }
}

/// Lets go of the meshes of layers gone (removed, their map or project closed, their rows edited
/// since: what they were made of no longer exists), which no scene can take again. Every frame
/// calls it, on the GPU or not, so a closed map's meshes go with the next frame drawn.
pub(crate) fn let_go() {
    with_meshes(|m| {
        if m.kept.iter().any(|k| !k.look.rows_held()) {
            m.kept.retain(|k| k.look.rows_held());
        }
    });
}

impl Drop for Scene {
    fn drop(&mut self) {
        let n = self.0;
        with_meshes(|m| m.drawing.retain(|d| d.0 != n));
    }
}

/// The commands of the mesh kept for the layer that looks as `look`, made as `made` from `rows`,
/// pushed onto `cmds`, and its vertices; `None`, pushing nothing, when none is kept.
pub(crate) fn take(cmds: &mut Vec<DrawCmd>, look: &LayerLook, made: &Made, rows: &[bool]) -> Option<usize> {
    let vertices = with_meshes(|m| {
        m.clock += 1;
        let clock = m.clock;
        let k = m
            .kept
            .iter_mut()
            .find(|k| k.look.id() == look.id() && k.made.same(made) && k.look.change(look) == Change::Same && k.rows == rows)?;
        k.used = clock;
        cmds.extend(k.cmds.iter().filter_map(share));
        Some(k.vertices)
    })?;
    #[cfg(test)]
    TAKEN.with_borrow_mut(|t| t.push(look.id()));
    Some(vertices)
}

/// Keeps `cmds` (a layer's Mesh and Points commands, of `vertices` vertices) for the next scene,
/// in place of what the live layer `layer` had kept for the same view, and of its oldest view past
/// [`VIEWS`]; `selected` is how many rows the layer has selected, which its look holds. See
/// [`MAX_BYTES`] for what goes when they don't all fit: nothing goes unless this is kept. A layer
/// with nothing in view keeps nothing: it is quick to find so again. Called while a [`Scene`] is
/// drawn.
pub(crate) fn keep(layer: u64, look: LayerLook, made: Made, rows: &[bool], cmds: &[DrawCmd], vertices: usize, selected: usize) {
    keep_within(MAX_BYTES, layer, look, made, rows, cmds, vertices, selected);
}

/// [`keep`], within `max` bytes.
#[expect(clippy::too_many_arguments, reason = "a mesh, what it was made from, and the budget it is kept within")]
fn keep_within(max: usize, layer: u64, look: LayerLook, made: Made, rows: &[bool], cmds: &[DrawCmd], vertices: usize, selected: usize) {
    let cmds: Vec<DrawCmd> = cmds.iter().filter_map(share).collect();
    let bytes = bytes_of(&cmds)
        .saturating_add(rows.len())
        .saturating_add(ENTRY_BYTES)
        .saturating_add(selected.saturating_mul(std::mem::size_of::<usize>()));
    if cmds.is_empty() || bytes > max {
        return;
    }
    with_meshes(|m| {
        // What goes for this one, worked out before anything goes: the layer's meshes of another
        // look (they no longer match), the one of this view, and past `VIEWS` its oldest; none
        // that a scene being drawn uses.
        let mut out: Vec<usize> = Vec::new();
        let mut views: Vec<(u64, usize)> = Vec::new();
        let mut shared: Option<Arc<LayerLook>> = None;
        for (i, k) in m.kept.iter().enumerate().filter(|(_, k)| k.layer == layer) {
            let same_look = k.look.change(&look) == Change::Same;
            if same_look && shared.is_none() {
                shared = Some(Arc::clone(&k.look));
            }
            if !same_look || k.made.same(&made) {
                if m.in_use(k) {
                    return;
                }
                out.push(i);
            } else {
                views.push((k.used, i));
            }
        }
        if views.len() >= VIEWS {
            views.sort_unstable();
            for (_, i) in views.iter().take(views.len() + 1 - VIEWS) {
                if m.kept.get(*i).is_some_and(|k| m.in_use(k)) {
                    return;
                }
                out.push(*i);
            }
        }
        // Kept only beside what the scenes being drawn use, which stays; then the meshes used
        // longest ago go first.
        let in_use: usize = m.kept.iter().filter(|k| m.in_use(k)).map(|k| k.bytes).sum();
        if in_use.saturating_add(bytes) > max {
            return;
        }
        let mut total: usize = m.kept.iter().enumerate().filter(|(i, _)| !out.contains(i)).map(|(_, k)| k.bytes).sum::<usize>();
        total = total.saturating_add(bytes);
        let mut old: Vec<(u64, usize)> =
            m.kept.iter().enumerate().filter(|(i, k)| !out.contains(i) && !m.in_use(k)).map(|(i, k)| (k.used, i)).collect();
        old.sort_unstable();
        for (_, i) in old {
            if total <= max {
                break;
            }
            total = total.saturating_sub(m.kept.get(i).map_or(0, |k| k.bytes));
            out.push(i);
        }
        out.sort_unstable_by(|a, b| b.cmp(a));
        out.dedup();
        for i in out {
            m.kept.swap_remove(i);
        }
        m.clock += 1;
        let used = m.clock;
        let look = shared.unwrap_or_else(|| Arc::new(look));
        m.kept.push(Kept { layer, look, made, rows: rows.to_vec(), cmds, vertices, bytes, used });
    });
}

/// The layers whose meshes scenes took since the last call, by id.
#[cfg(test)]
pub(crate) fn taken() -> Vec<u32> {
    TAKEN.take()
}

/// Forgets the meshes kept on this test thread.
#[cfg(test)]
pub(crate) fn clear() {
    MESHES.with_borrow_mut(|m| m.kept.clear());
    TAKEN.take();
}

/// The bytes the meshes kept on this test thread hold.
#[cfg(test)]
pub(crate) fn kept_bytes() -> usize {
    MESHES.with_borrow(|m| m.kept.iter().map(|k| k.bytes).sum())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu_scene::GpuScene;
    use crate::{Backend, RenderOutput, View, render_backend};
    use rustgis_data::{Dataset, Geometry, GeometryType};
    use rustgis_geom::{Coord, Extent};
    use rustgis_map::{Camera, Map, Renderer, Rgba, Symbol};

    fn square(x: f64, s: f64) -> Geometry {
        Geometry::Polygon(vec![vec![vec![Coord::new(x, 0.0), Coord::new(x + s, 0.0), Coord::new(x + s, s), Coord::new(x, s)]]])
    }

    /// A map of two polygon layers drawn on the GPU, `a` under `b`, and a view of them.
    fn two_layers() -> (Map, u32, u32, View) {
        let mut m = Map::new("Map");
        // Only the layers' meshes: no basemap or grid.
        m.basemap = rustgis_map::Basemap::Blank;
        m.graticule = false;
        let mut layer = |name: &str, x: f64| {
            let mut d = Dataset::new(name, GeometryType::Polygon, vec![]);
            d.push(Some(square(x, 1.0)), vec![]);
            m.add_dataset(d)
        };
        let (a, b) = (layer("a", 0.0), layer("b", 2.0));
        let camera = Camera { center: rustgis_geom::lonlat_to_merc(Coord::new(1.5, 0.5)), res: 2000.0, bearing: 0.0, pitch: 0.0 };
        (m, a, b, View { width: 400.0, height: 300.0, pixels_per_point: 1.0, camera, sky: false })
    }

    /// The GPU scene of `view`, drawn whole: a scene started from a kept picture
    /// (`frame_cache`) doesn't draw the layers under it. A frame drawn on the CPU instead fails
    /// the test.
    fn scene(m: &mut Map, view: &View) -> GpuScene {
        crate::frame_cache::clear();
        match render_backend(m, view, Backend::Gpu) {
            Some(RenderOutput::Gpu(s)) => s,
            _ => panic!("no GPU scene"),
        }
    }

    /// Each Mesh command's vertices and indices, by address and by value.
    fn meshes(s: &GpuScene) -> Vec<(*const Vec<MeshVertex>, Vec<[u32; 4]>, Vec<u32>)> {
        s.cmds
            .iter()
            .filter_map(|c| match c {
                DrawCmd::Mesh { vertices, indices, .. } => Some((
                    Arc::as_ptr(vertices),
                    vertices
                        .iter()
                        .map(|v| [v.position[0].to_bits(), v.position[1].to_bits(), v.color[0].to_bits(), v.stroke_px.to_bits()])
                        .collect(),
                    indices.to_vec(),
                )),
                _ => None,
            })
            .collect()
    }

    /// Gives layer `id` one symbol filled with `fill`.
    fn restyle(m: &mut Map, id: u32, fill: Rgba) {
        if let Some(l) = m.layers.iter_mut().find(|l| l.id == id) {
            l.renderer = Renderer::Single(Symbol { fill, ..l.symbol(0) });
        }
    }

    /// A scene of the same view after one layer was restyled takes the other's mesh, shared
    /// with the last scene, and tessellates the restyled one again: the same commands, vertex for
    /// vertex, as a scene drawn with nothing kept.
    #[test]
    fn a_layer_that_looks_the_same_keeps_its_mesh() {
        clear();
        let (mut m, a, b, view) = two_layers();
        let first = meshes(&scene(&mut m, &view));
        assert_eq!(first.len(), 2, "one mesh a layer");
        assert!(taken().is_empty(), "the first scene keeps, takes nothing");
        assert!(kept_bytes() > 0);
        restyle(&mut m, b, Rgba::rgb(200, 0, 0));
        let second = meshes(&scene(&mut m, &view));
        assert_eq!(taken(), vec![a], "only the layer that looks the same");
        assert_eq!(second[0].0, first[0].0, "a's mesh is shared with the last scene");
        assert_ne!(second[1].0, first[1].0, "b's is made again");
        assert_ne!(second[1].1, first[1].1, "in its new colour");
        clear();
        let fresh = meshes(&scene(&mut m, &view));
        let values = |ms: &[(*const Vec<MeshVertex>, Vec<[u32; 4]>, Vec<u32>)]| {
            ms.iter().map(|(_, v, i)| (v.clone(), i.clone())).collect::<Vec<_>>()
        };
        assert_eq!(values(&second), values(&fresh), "the same scene as one made from nothing");
        // Unchanged, the next scene takes both.
        let _ = scene(&mut m, &view);
        assert_eq!(taken(), vec![a, b]);
    }

    /// A pan, a zoom, an edit to a layer's rows or its definition query each make the mesh
    /// again; so does an export, which keeps nothing.
    #[test]
    fn a_moved_view_or_a_changed_layer_makes_its_mesh_again() {
        clear();
        let (mut m, a, b, view) = two_layers();
        let _ = scene(&mut m, &view);
        let mut panned = view;
        panned.camera.center.x += 50.0 * view.camera.res;
        let _ = scene(&mut m, &panned);
        assert!(taken().is_empty(), "a pan");
        let _ = scene(&mut m, &view);
        assert_eq!(taken(), vec![a, b], "back where it was kept");
        let mut zoomed = view;
        zoomed.camera.res *= 0.9;
        let _ = scene(&mut m, &zoomed);
        assert!(taken().is_empty(), "a zoom");
        // A layer keeps two views: the zoomed one put out the panned one, not this one.
        let _ = scene(&mut m, &panned);
        assert!(taken().is_empty(), "the panned view's went");
        let _ = scene(&mut m, &zoomed);
        assert_eq!(taken(), vec![a, b]);
        let _ = scene(&mut m, &view);
        assert!(taken().is_empty(), "made again for the view");
        if let Some(l) = m.layers.iter_mut().find(|l| l.id == a) {
            l.dataset.push(Some(square(0.2, 0.5)), vec![]);
        }
        let _ = scene(&mut m, &view);
        assert_eq!(taken(), vec![b], "a's rows were edited");
        if let Some(l) = m.layers.iter_mut().find(|l| l.id == b) {
            l.definition_query = Some("1 = 0".into());
        }
        let _ = scene(&mut m, &view);
        assert_eq!(taken(), vec![a], "b's definition query changed");
        let bytes = kept_bytes();
        let _ = crate::render(&mut m, &view);
        assert_eq!(kept_bytes(), bytes, "the CPU renderer keeps no meshes");
    }

    /// Meshes that don't all fit: a scene keeps what fits and never puts out a mesh it uses
    /// itself, so the next scenes take those same meshes rather than each pushing out the next one
    /// the scene needs. One with nothing in view keeps nothing.
    #[test]
    fn a_scene_keeps_the_meshes_it_uses_when_they_dont_all_fit() {
        clear();
        let (mut m, a, b, view) = two_layers();
        let mut d = Dataset::new("c", GeometryType::Polygon, vec![]);
        d.push(Some(square(4.0, 1.0)), vec![]);
        let c = m.add_dataset(d);
        let made = Made {
            projection: m.projection.clone(),
            drawn: Drawn::Projected,
            cull: view.camera.extent(view.width, view.height),
            map_per_px: view.camera.res,
            ppp: 1.0,
            wireframe: false,
        };
        let mesh = |n: usize| DrawCmd::Mesh {
            origin: [0.0; 2],
            vertices: Arc::new(vec![bytemuck::Zeroable::zeroed(); n]),
            indices: Arc::new(Vec::new()),
        };
        let one = bytes_of(&[mesh(4096)]) + ENTRY_BYTES + 1;
        // Room for two of the three.
        let max = one * 5 / 2;
        let look = |m: &Map, id: u32| m.layers.iter().find(|l| l.id == id).and_then(LayerLook::of).expect("look");
        let live = |m: &Map, id: u32| m.layers.iter().find(|l| l.id == id).map_or(0, |l| l.instance_id());
        let draw = |m: &Map, sizes: [usize; 3]| -> Vec<u32> {
            let _scene = Scene::begin();
            [a, b, c]
                .into_iter()
                .zip(sizes)
                .filter(|(id, n)| {
                    let taken = take(&mut Vec::new(), &look(m, *id), &made, &[true]).is_some();
                    if !taken {
                        keep_within(max, live(m, *id), look(m, *id), made.clone(), &[true], &[mesh(*n)], *n, 0);
                    }
                    taken
                })
                .map(|(id, _)| id)
                .collect()
        };
        let even = [4096; 3];
        assert!(draw(&m, even).is_empty());
        assert_eq!(draw(&m, even), vec![a, b], "the first two, kept by the first scene");
        restyle(&mut m, c, Rgba::rgb(200, 0, 0));
        assert_eq!(draw(&m, even), vec![a, b], "the third, restyled, doesn't put them out");
        assert!(kept_bytes() <= max);
        // One that doesn't fit beside those the scene uses puts none out first: here the second,
        // too big beside the first, leaves the third kept.
        clear();
        let uneven = [6000, 5000, 3000];
        assert!(draw(&m, uneven).is_empty());
        assert_eq!(draw(&m, uneven), vec![a, c]);
        assert_eq!(draw(&m, uneven), vec![a, c]);
        // With another scene still drawing (begun first, on another thread), the same.
        clear();
        let first = Scene::begin();
        assert!(draw(&m, even).is_empty());
        assert_eq!(draw(&m, even), vec![a, b]);
        drop(first);
        // Nothing in view: nothing kept, nothing put out.
        let _scene = Scene::begin();
        let bytes = kept_bytes();
        keep_within(max, live(&m, a), look(&m, a), made.clone(), &[true], &[], 0, 0);
        assert_eq!(kept_bytes(), bytes);
    }

    /// A mesh is kept only when nothing it would put out is still in use, and it puts nothing out
    /// unless it is kept: a third view too big beside what a scene uses leaves the layer's two
    /// views; a mesh kept by one scene isn't replaced by another drawing the same view at the same
    /// time; and two maps' layers of the same id keep their meshes apart.
    #[test]
    fn a_mesh_kept_puts_out_only_what_no_scene_uses() {
        clear();
        let (m, a, b, view) = two_layers();
        let at = |dx: f64| Made {
            projection: m.projection.clone(),
            drawn: Drawn::Projected,
            cull: {
                let e = view.camera.extent(view.width, view.height);
                Extent::new(e.min_x + dx, e.min_y, e.max_x + dx, e.max_y)
            },
            map_per_px: view.camera.res,
            ppp: 1.0,
            wireframe: false,
        };
        let mesh = |n: usize| DrawCmd::Mesh {
            origin: [0.0; 2],
            vertices: Arc::new(vec![bytemuck::Zeroable::zeroed(); n]),
            indices: Arc::new(Vec::new()),
        };
        let size = |n: usize| bytes_of(&[mesh(n)]) + ENTRY_BYTES + 1;
        let max = size(6000) + size(3000) + size(500);
        let layer = |m: &Map, id: u32| m.layers.iter().find(|l| l.id == id).expect("layer").clone();
        let (la, lb) = (layer(&m, a), layer(&m, b));
        let look = |l: &rustgis_map::Layer| LayerLook::of(l).expect("look");
        let keep_at =
            |l: &rustgis_map::Layer, dx: f64, n: usize| keep_within(max, l.instance_id(), look(l), at(dx), &[true], &[mesh(n)], n, 0);
        let has = |l: &rustgis_map::Layer, dx: f64| take(&mut Vec::new(), &look(l), &at(dx), &[true]);
        {
            let _old = Scene::begin();
            keep_at(&la, 0.0, 500);
            keep_at(&la, 1.0, 3000);
        }
        let _scene = Scene::begin();
        keep_at(&lb, 0.0, 6000);
        // A third view of `a` would put out its oldest, but doesn't fit beside `b`'s, in use.
        keep_at(&la, 2.0, 4000);
        assert_eq!((has(&la, 0.0), has(&la, 1.0), has(&la, 2.0)), (Some(500), Some(3000), None));
        // Kept again for the same view while in use: the first stays.
        keep_at(&la, 0.0, 400);
        assert_eq!(has(&la, 0.0), Some(500));
        drop(_scene);
        // Another map's layer of the same id keeps its own.
        clear();
        let (m2, a2, ..) = two_layers();
        let other = layer(&m2, a2);
        assert_eq!(other.id, la.id);
        let _scene = Scene::begin();
        keep_at(&la, 0.0, 500);
        keep_at(&other, 0.0, 400);
        keep_at(&other, 1.0, 400);
        assert_eq!((has(&la, 0.0), has(&other, 0.0)), (Some(500), Some(400)));
    }

    /// A map closed lets go of its layers' meshes with the next frame drawn, of any map and on
    /// any renderer; a layer edited lets go of the mesh of its rows before the edit.
    #[test]
    fn a_closed_map_lets_go_of_its_meshes() {
        clear();
        let (mut m, a, _, view) = two_layers();
        let _ = scene(&mut m, &view);
        let both = kept_bytes();
        assert!(both > 0);
        if let Some(l) = m.layers.iter_mut().find(|l| l.id == a) {
            l.dataset.push(Some(square(0.2, 0.5)), vec![]);
        }
        let _ = crate::render(&mut Map::new("Other"), &view);
        assert!(kept_bytes() < both, "the edited layer's old mesh went");
        drop(m);
        let _ = crate::render(&mut Map::new("Other"), &view);
        assert_eq!(kept_bytes(), 0);
    }
}
