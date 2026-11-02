//! Vector tiles as meshes: a tile's shapes (see `rustgis_tiles::shapes`) tessellated once and
//! drawn at every zoom of their level, as MapLibre draws its tiles, rather than drawn into a
//! picture again for every zoom and resolution.
//!
//! Strokes are tessellated at width 1 and their vertices moved by the vertex shader along lyon's
//! normals, so one mesh serves every width; circles are fans whose rims the shader moves out the
//! same way. A mesh holds a run of indices per style layer (merged with the next when it shows at
//! the same zooms), for the renderer to leave out the layers hidden at its zoom.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};

use lyon::path::Path;
use lyon::tessellation::{
    BuffersBuilder, FillOptions, FillRule, FillTessellator, FillVertex, LineCap, LineJoin, StrokeOptions, StrokeTessellator, StrokeVertex,
    VertexBuffers,
};
use rustgis_tiles::cache::Payload;
use rustgis_tiles::mvt::Tile;
use rustgis_tiles::shapes::{DataTile, Paint, Run, shapes};
use rustgis_tiles::source::Resolved;
use rustgis_tiles::style;
use rustgis_tiles::template::TileId;

use crate::gpu_scene::TileMeshVertex;

/// Round caps and joins, in the tessellation's units (a half width of 0.5): about 16 segments
/// a circle, smooth at the widths roads are drawn at.
const TOLERANCE: f32 = 0.02;

/// Segments of a circle's rim.
const RIM: usize = 16;

/// The most vertices a tile's mesh holds, about (Liberty's densest tiles take a tenth of it
/// natively, 100 MB; a fifth in the browser, within its budget): the shapes past are left out, so
/// a hostile tile can't take the memory, and its indices stay far within `u32`.
const MAX_VERTICES: usize = if cfg!(target_arch = "wasm32") { 512 << 10 } else { 2 << 20 };

/// The most mesh memory kept, bytes.
const BUDGET: usize = if cfg!(target_arch = "wasm32") { 128 << 20 } else { 512 << 20 };

/// A tile's shapes as one mesh.
#[derive(Debug, Default)]
pub struct TileMesh {
    pub vertices: Vec<TileMeshVertex>,
    pub indices: Vec<u32>,
    /// Bottom first.
    pub runs: Vec<MeshRun>,
    /// Made while the sprite was on its way, without the pattern fills: made again once it has
    /// arrived.
    pub waiting: bool,
    /// The ids of the style's sprites its pattern fills were made from, if it has any.
    pub sheets: Option<Vec<String>>,
}

/// The indices of one or more style layers showing at the same zooms.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeshRun {
    pub first: u32,
    pub count: u32,
    /// It shows from `min_zoom` up to `max_zoom` (excluded).
    pub min_zoom: f32,
    pub max_zoom: f32,
}

impl TileMesh {
    /// Its size, bytes.
    pub fn bytes(&self) -> usize {
        std::mem::size_of_val(self.vertices.as_slice()) + std::mem::size_of_val(self.indices.as_slice())
    }

    /// The index ranges showing at `zoom`, bottom first.
    pub fn shown(&self, zoom: f32) -> impl Iterator<Item = std::ops::Range<u32>> + '_ {
        self.runs.iter().filter(move |r| zoom >= r.min_zoom && zoom < r.max_zoom).map(|r| r.first..r.first + r.count)
    }
}

/// Straight RGBA, 0..1, premultiplied in bytes.
fn premul(c: [f32; 4]) -> [u8; 4] {
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    [byte(c[0] * c[3]), byte(c[1] * c[3]), byte(c[2] * c[3]), byte(c[3])]
}

/// A dash pattern as the vertex shader takes it: an odd count repeated once over (as in SVG,
/// and as tiny-skia draws it), at most four lengths; `None` for a longer one (see
/// `rustgis_tiles::shapes::MAX_DASHES`, past which styles are drawn otherwise).
pub fn dash_pattern(d: Option<&[f32]>) -> Option<[f32; 4]> {
    let Some(d) = d.filter(|d| !d.is_empty()) else { return Some([0.0; 4]) };
    let mut v: Vec<f32> = d.to_vec();
    if v.len() % 2 == 1 {
        v.extend_from_slice(d);
    }
    if v.len() > 4 {
        return None;
    }
    let mut out = [0.0; 4];
    for (o, x) in out.iter_mut().zip(v) {
        *o = x.max(0.0);
    }
    Some(if out.iter().sum::<f32>() <= 0.0 { [0.0; 4] } else { out })
}

/// Tessellates `runs` of `tile` (a tile at zoom `z`) into one mesh, in map units from the
/// tile's north-west corner. Shapes lyon refuses are left out.
pub fn tessellate(tile: &Tile, runs: &[Run], z: u8) -> TileMesh {
    let span = TileId::span(z) as f32;
    let mut out: VertexBuffers<TileMeshVertex, u32> = VertexBuffers::new();
    let mut mesh_runs: Vec<MeshRun> = Vec::new();
    let mut fill = FillTessellator::new();
    let mut stroke = StrokeTessellator::new();
    for run in runs {
        let k = span / run.extent.max(1) as f32;
        // Tessellated in tile units (small numbers, where a width of 1 keeps its precision; map
        // units at a low zoom run to millions), then put in map units, y north.
        let at = |p: &[f32; 2]| lyon::math::point(p[0], p[1]);
        let map = |x: f32, y: f32| [x * k, -y * k];
        // Indices are u32: a tile past them (none is, but tiles are hostile) stops there.
        let Ok(first) = u32::try_from(out.indices.len()) else { break };
        for shape in &run.shapes {
            if out.vertices.len() >= MAX_VERTICES {
                break;
            }
            let geometry: Option<&[Vec<[f32; 2]>]> =
                shape.feature.and_then(|(l, f)| tile.layers.get(l).and_then(|l| l.features.get(f))).map(|f| f.geometry.as_slice());
            match &shape.paint {
                Paint::Fill { color, .. } => {
                    let (c0, c1) = (premul(color[0]), premul(color[1]));
                    let vertex = |v: FillVertex| {
                        let p = v.position();
                        TileMeshVertex { position: map(p.x, p.y), color: c0, color_next: c1, ..TileMeshVertex::default() }
                    };
                    // One convex ring (most buildings): a fan, without lyon's sweep. Either winding
                    // fills alike: the pipeline culls no faces.
                    if let Some([ring]) = geometry
                        && let Some(pts) = convex(ring)
                    {
                        let Ok(base) = u32::try_from(out.vertices.len()) else { break };
                        out.vertices.extend(pts.iter().map(|p| TileMeshVertex {
                            position: map(p[0], p[1]),
                            color: c0,
                            color_next: c1,
                            ..TileMeshVertex::default()
                        }));
                        for i in 1..pts.len() as u32 - 1 {
                            out.indices.extend_from_slice(&[base, base + i, base + i + 1]);
                        }
                        continue;
                    }
                    let mut b = Path::builder();
                    match geometry {
                        Some(rings) => {
                            for ring in rings.iter().filter(|r| r.len() >= 3) {
                                let Some((head, rest)) = ring.split_first() else { continue };
                                b.begin(at(head));
                                for p in rest {
                                    b.line_to(at(p));
                                }
                                b.close();
                            }
                        }
                        // The whole square.
                        None => {
                            let e = run.extent as f32;
                            b.begin(at(&[0.0, 0.0]));
                            b.line_to(at(&[e, 0.0]));
                            b.line_to(at(&[e, e]));
                            b.line_to(at(&[0.0, e]));
                            b.close();
                        }
                    }
                    let options = FillOptions::default().with_fill_rule(FillRule::EvenOdd);
                    let mark = (out.vertices.len(), out.indices.len());
                    if fill.tessellate_path(&b.build(), &options, &mut BuffersBuilder::new(&mut out, vertex)).is_err() {
                        undo(&mut out, mark);
                    }
                }
                Paint::Line { color, width, cap, join, dashes, rings } => {
                    let Some(lines) = geometry else { continue };
                    let base = TileMeshVertex {
                        color: premul(color[0]),
                        color_next: premul(color[1]),
                        width: *width,
                        // Declined styles aside (`shapes::vector_tiles`), a feature's own longer
                        // pattern is drawn solid rather than with the wrong dashes.
                        dashes: dash_pattern(dashes.as_deref()).unwrap_or_default(),
                        ..TileMeshVertex::default()
                    };
                    let options = StrokeOptions::default()
                        .with_line_width(1.0)
                        .with_tolerance(TOLERANCE)
                        .with_line_cap(match cap {
                            style::LineCap::Butt => LineCap::Butt,
                            style::LineCap::Round => LineCap::Round,
                            style::LineCap::Square => LineCap::Square,
                        })
                        .with_line_join(match join {
                            style::LineJoin::Bevel => LineJoin::Bevel,
                            style::LineJoin::Round => LineJoin::Round,
                            style::LineJoin::Miter => LineJoin::Miter,
                        });
                    for line in lines.iter().filter(|l| l.len() >= 2) {
                        // Within a shape too: one feature can hold any number of parts.
                        if out.vertices.len() >= MAX_VERTICES {
                            break;
                        }
                        let Some((head, rest)) = line.split_first() else { continue };
                        let mut b = Path::builder();
                        b.begin(at(head));
                        for p in rest {
                            b.line_to(at(p));
                        }
                        b.end(*rings);
                        // On the line: lyon's position less half the width it was tessellated at.
                        let vertex = |v: StrokeVertex| {
                            let (p, n) = (v.position(), v.normal());
                            TileMeshVertex {
                                position: map(p.x - n.x * 0.5, p.y - n.y * 0.5),
                                normal: [n.x, -n.y],
                                along: v.advancement() * k,
                                ..base
                            }
                        };
                        let mark = (out.vertices.len(), out.indices.len());
                        if stroke.tessellate_path(&b.build(), &options, &mut BuffersBuilder::new(&mut out, vertex)).is_err() {
                            undo(&mut out, mark);
                        }
                    }
                }
                Paint::Circle { color, radius, stroke: ring } => {
                    let Some(points) = geometry else { continue };
                    let disc = [radius[0] * 2.0, radius[1] * 2.0];
                    for p in points.iter().flatten() {
                        if out.vertices.len() >= MAX_VERTICES {
                            break;
                        }
                        let c = map(p[0], p[1]);
                        circle(&mut out, c, color, disc, disc);
                        if let Some((sc, sw)) = ring {
                            circle(&mut out, c, sc, [disc[0] - sw[0], disc[1] - sw[1]], [disc[0] + sw[0], disc[1] + sw[1]]);
                        }
                    }
                }
            }
        }
        let count = u32::try_from(out.indices.len()).map_or(0, |n| n.saturating_sub(first));
        if count == 0 {
            continue;
        }
        match mesh_runs.last_mut() {
            Some(last)
                if last.first.saturating_add(last.count) == first && last.min_zoom == run.min_zoom && last.max_zoom == run.max_zoom =>
            {
                last.count += count;
            }
            _ => mesh_runs.push(MeshRun { first, count, min_zoom: run.min_zoom, max_zoom: run.max_zoom }),
        }
    }
    TileMesh { vertices: out.vertices, indices: out.indices, runs: mesh_runs, waiting: false, sheets: None }
}

/// The most points of a ring fanned rather than given to lyon: the walk round it stays cheap,
/// and rings past it are rarely convex.
const FAN_POINTS: usize = 64;

/// How far (radians) a ring's turns may add up away from once round and still be convex: f32
/// rounding, far less than a turn's worth.
const TURN_TOLERANCE: f32 = 0.1;

/// A ring's points without its closing one, when they make a convex polygon of some area
/// (turning one way all round, once): a fan of them fills it. A spike (going back on itself)
/// turns half round more, so it fails the count and goes to lyon.
fn convex(ring: &[[f32; 2]]) -> Option<&[[f32; 2]]> {
    let pts = match ring {
        [first, .., last] if first == last => ring.get(..ring.len() - 1)?,
        _ => ring,
    };
    let n = pts.len();
    if !(3..=FAN_POINTS).contains(&n) {
        return None;
    }
    let (mut sign, mut turned) = (0.0f32, 0.0f32);
    for i in 0..n {
        let (a, b, c) = (pts[i], pts[(i + 1) % n], pts[(i + 2) % n]);
        let cross = (b[0] - a[0]) * (c[1] - b[1]) - (b[1] - a[1]) * (c[0] - b[0]);
        if !cross.is_finite() {
            return None;
        }
        if cross != 0.0 {
            if sign != 0.0 && cross.signum() != sign {
                return None;
            }
            sign = cross.signum();
        }
        // The angle turned at b: all round once for a simple convex ring, more for a star.
        let (u, v) = ([b[0] - a[0], b[1] - a[1]], [c[0] - b[0], c[1] - b[1]]);
        turned += cross.atan2(u[0] * v[0] + u[1] * v[1]);
    }
    (sign != 0.0 && (turned.abs() - std::f32::consts::TAU).abs() < TURN_TOLERANCE).then_some(pts)
}

/// Takes back what a shape lyon refused left in `out`: shapes lyon refuses are left out, not
/// drawn in part.
fn undo(out: &mut VertexBuffers<TileMeshVertex, u32>, (vertices, indices): (usize, usize)) {
    out.vertices.truncate(vertices);
    out.indices.truncate(indices);
}

/// A disc (`inner == outer`, a fan about its centre) or a ring between two widths, centred at
/// `c`: its rim vertices sit at the centre, moved out by the shader.
fn circle(out: &mut VertexBuffers<TileMeshVertex, u32>, c: [f32; 2], color: &[[f32; 4]; 2], inner: [f32; 2], outer: [f32; 2]) {
    let base = TileMeshVertex { position: c, color: premul(color[0]), color_next: premul(color[1]), ..TileMeshVertex::default() };
    let at = |i: usize, width: [f32; 2]| {
        let a = i as f32 / RIM as f32 * std::f32::consts::TAU;
        TileMeshVertex { normal: [a.cos(), a.sin()], width: width.map(|w| w.max(0.0)), ..base }
    };
    let Ok(first) = u32::try_from(out.vertices.len()) else { return };
    let ring = inner != outer;
    if ring {
        for i in 0..RIM {
            out.vertices.push(at(i, inner));
            out.vertices.push(at(i, outer));
        }
        for i in 0..RIM as u32 {
            let (a, b) = (first + 2 * i, first + 2 * ((i + 1) % RIM as u32));
            out.indices.extend_from_slice(&[a, a + 1, b + 1, a, b + 1, b]);
        }
    } else {
        out.vertices.push(base);
        for i in 0..RIM {
            out.vertices.push(at(i, outer));
        }
        for i in 0..RIM as u32 {
            out.indices.extend_from_slice(&[first, first + 1 + i, first + 1 + (i + 1) % RIM as u32]);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    /// The tile's payload and the resolved style, by address ([`Entry`] tells whether they are
    /// still the same).
    payload: usize,
    style: usize,
    level: u8,
}

struct Entry {
    payload: Weak<Payload>,
    style: Weak<Resolved>,
    mesh: Arc<TileMesh>,
    used: u64,
    /// Made while the sprite was on its way: made again once it has arrived (or failed), not
    /// before.
    waiting: bool,
    /// The ids of the style's sprites its pattern fills were made from, if it has any.
    sheets: Option<Vec<String>>,
}

/// Whether a mesh made from the sprites `sheets`, `waiting` for them or not, still serves while
/// `pending` of them are on their way and `read` have been read. Other sprites read (one that
/// failed read after all, or another failing instead) make those with pattern fills again, once;
/// until then they stand, however long one keeps failing.
fn still_serves(waiting: bool, sheets: Option<&[String]>, pending: bool, read: &[String]) -> bool {
    pending || (!waiting && sheets.is_none_or(|ids| ids == read))
}

#[derive(Default)]
struct Store {
    entries: HashMap<Key, Entry>,
    bytes: usize,
    tick: u64,
}

fn lock() -> MutexGuard<'static, Store> {
    static S: OnceLock<Mutex<Store>> = OnceLock::new();
    S.get_or_init(Mutex::default).lock().unwrap_or_else(|e| e.into_inner())
}

/// The meshes of `tiles` (bottom first) for `style`'s `source` at `level`, each kept once made
/// (while its tile and the style are, and within a memory budget; one made while the sprite was
/// on its way until it has arrived); `None` for one that is no vector tile, or not made
/// yet: with `until`, those missing are made nearest `centre` first until then (a round of them
/// at least). Natively, they are made on several threads. Also whether the sprites of its pattern
/// fills are on their way, as a mesh kept meanwhile (one made when a sprite failed, which is now
/// tried again) doesn't say so itself.
pub fn meshes(
    style: &Arc<Resolved>,
    source: &str,
    tiles: &[DataTile],
    level: u8,
    until: Option<(web_time::Instant, [f64; 2])>,
) -> (Vec<Option<Arc<TileMesh>>>, bool) {
    // Whether the style's sprite is still on its way, asked before the store is locked (a
    // lookup in the sprites the tile cache keeps).
    let (sprite_pending, sprites_read) = if rustgis_tiles::shapes::draws_patterns(style, level) {
        match rustgis_tiles::sprite::load(&style.sprites, false) {
            rustgis_tiles::sprite::Load::Pending => (true, Vec::new()),
            rustgis_tiles::sprite::Load::Ready(s) => (false, s.sheet_ids()),
            rustgis_tiles::sprite::Load::Failed => (false, Vec::new()),
        }
    } else {
        // No pattern fills here: nothing to load the sprites for.
        (false, Vec::new())
    };
    let key = |t: &DataTile| Key { payload: Arc::as_ptr(&t.payload) as usize, style: Arc::as_ptr(style) as usize, level };
    let mut out: Vec<Option<Arc<TileMesh>>> = Vec::with_capacity(tiles.len());
    let mut missing = Vec::new();
    let (mut seen, mut repeats) = (HashMap::new(), Vec::new());
    // This call's tick: what it uses and makes is spared by its eviction, whatever other calls
    // (another thread's) do meanwhile.
    let tick = {
        let mut s = lock();
        s.tick += 1;
        let tick = s.tick;
        for (i, t) in tiles.iter().enumerate() {
            let found = s.entries.get_mut(&key(t)).filter(|e| {
                e.payload.upgrade().is_some_and(|p| Arc::ptr_eq(&p, &t.payload))
                    && e.style.upgrade().is_some_and(|x| Arc::ptr_eq(&x, style))
                    && still_serves(e.waiting, e.sheets.as_deref(), sprite_pending, &sprites_read)
            });
            match found {
                Some(e) => {
                    e.used = tick;
                    out.push(Some(e.mesh.clone()));
                }
                None => {
                    // A tile standing in for several (the same payload) is made once.
                    match seen.get(&key(t)) {
                        Some(&first) => repeats.push((i, first)),
                        None => {
                            seen.insert(key(t), i);
                            missing.push(i);
                        }
                    }
                    out.push(None);
                }
            }
        }
        tick
    };
    let make = |i: &usize| -> Option<Arc<TileMesh>> {
        let t = tiles.get(*i)?;
        let Payload::Vector(tile) = &*t.payload else { return None };
        let s = shapes(style, source, tile, level);
        Some(Arc::new(TileMesh { waiting: s.waiting, sheets: s.sheets, ..tessellate(tile, &s.runs, t.z) }))
    };
    // A tile that panicked (hostile data) is done: an empty mesh, kept, so it isn't made again
    // every frame, and the map settles.
    let done = |m: Option<Option<Arc<TileMesh>>>| m.unwrap_or_else(|| Some(Arc::new(TileMesh::default())));
    let made: Vec<Option<Arc<TileMesh>>> = match until {
        None => parallel(&missing, make).into_iter().map(done).collect(),
        Some((until, centre)) => {
            let far = |i: &usize| {
                tiles.get(*i).map_or(f64::MAX, |t| {
                    let c = &t.clip;
                    ((c.min_x + c.max_x) / 2.0 - centre[0]).hypot((c.min_y + c.max_y) / 2.0 - centre[1])
                })
            };
            let mut order: Vec<usize> = (0..missing.len()).collect();
            order.sort_by(|a, b| far(&missing[*a]).total_cmp(&far(&missing[*b])));
            let round = if cfg!(target_arch = "wasm32") { 1 } else { std::thread::available_parallelism().map_or(1, |n| n.get()).min(16) };
            let mut made: Vec<Option<Arc<TileMesh>>> = vec![None; missing.len()];
            for (n, some) in order.chunks(round).enumerate() {
                if n > 0 && web_time::Instant::now() >= until {
                    break;
                }
                let picked: Vec<usize> = some.iter().filter_map(|&j| missing.get(j).copied()).collect();
                for (&j, m) in some.iter().zip(parallel(&picked, make)) {
                    if let Some(slot) = made.get_mut(j) {
                        *slot = done(m);
                    }
                }
            }
            made
        }
    };
    let mut s = lock();
    for (&i, mesh) in missing.iter().zip(made) {
        let (Some(mesh), Some(t)) = (mesh, tiles.get(i)) else { continue };
        let entry = Entry {
            payload: Arc::downgrade(&t.payload),
            style: Arc::downgrade(style),
            mesh: mesh.clone(),
            used: tick,
            waiting: mesh.waiting,
            sheets: mesh.sheets.clone(),
        };
        // The bytes are counted again by `evict`, below.
        s.entries.insert(key(t), entry);
        if let Some(slot) = out.get_mut(i) {
            *slot = Some(mesh);
        }
    }
    for (i, first) in repeats {
        let mesh = out.get(first).cloned().flatten();
        if let Some(slot) = out.get_mut(i) {
            *slot = mesh;
        }
    }
    // Every call, so meshes of tiles the cache let go go as the map moves over others.
    evict(&mut s, tick);
    (out, sprite_pending)
}

/// `f` of every item (`None` for one that panicked), natively shared among threads. In the
/// browser a panic aborts (`wasm32-unknown-unknown` unwinds nothing): there a panic is caught
/// nowhere, as everywhere else in the web app.
fn parallel<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<Option<R>> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(items.len()).min(16);
        if threads > 1 {
            let next = std::sync::atomic::AtomicUsize::new(0);
            let out: Mutex<Vec<Option<R>>> = Mutex::new((0..items.len()).map(|_| None).collect());
            std::thread::scope(|scope| {
                for _ in 0..threads {
                    scope.spawn(|| {
                        loop {
                            let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            let Some(item) = items.get(i) else { break };
                            // A tile that panics (hostile data) gives no mesh, rather than the frame.
                            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(item))).ok();
                            if let Some(slot) = out.lock().unwrap_or_else(|e| e.into_inner()).get_mut(i) {
                                *slot = r;
                            }
                        }
                    });
                }
            });
            return out.into_inner().unwrap_or_else(|e| e.into_inner());
        }
    }
    items.iter().map(|i| std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(i))).ok()).collect()
}

/// Drops the meshes of tiles and styles gone, and past the budget those unused longest (but
/// never those of the call at `tick`: a view's own meshes may go past the budget, bounded by
/// the size of its tiles).
fn evict(s: &mut Store, tick: u64) {
    // Those of tiles the tile cache let go, and styles gone, go first, budget or not.
    s.entries.retain(|_, e| e.payload.strong_count() > 0 && e.style.strong_count() > 0);
    s.bytes = s.entries.values().map(|e| e.mesh.bytes()).sum();
    if s.bytes <= BUDGET {
        return;
    }
    let mut old: Vec<(u64, Key, usize)> =
        s.entries.iter().filter(|(_, e)| e.used != tick).map(|(k, e)| (e.used, *k, e.mesh.bytes())).collect();
    old.sort_unstable_by_key(|o| o.0);
    for (_, k, bytes) in old {
        if s.bytes <= BUDGET {
            break;
        }
        s.entries.remove(&k);
        s.bytes = s.bytes.saturating_sub(bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_geom::Extent;
    use rustgis_tiles::mvt::{Feature, GeomKind, Layer};
    use rustgis_tiles::shapes::Shape;
    use rustgis_tiles::style::{LineCap as Cap, LineJoin as Join};

    fn tile() -> Tile {
        let f = |kind, geometry: Vec<Vec<[f32; 2]>>| Feature { id: None, kind, geometry, props: Vec::new() };
        Tile {
            layers: vec![Layer {
                name: "l".into(),
                extent: 4096,
                features: vec![
                    f(GeomKind::Polygon, vec![vec![[0.0, 0.0], [2048.0, 0.0], [2048.0, 2048.0], [0.0, 2048.0], [0.0, 0.0]]]),
                    f(GeomKind::LineString, vec![vec![[0.0, 1024.0], [1024.0, 1024.0], [1024.0, 3072.0]]]),
                    f(GeomKind::Point, vec![vec![[2048.0, 2048.0]]]),
                ],
            }],
        }
    }

    const RED: [[f32; 4]; 2] = [[1.0, 0.0, 0.0, 0.5], [1.0, 0.0, 0.0, 1.0]];

    fn run(layer: usize, min_zoom: f32, feature: Option<usize>, paint: Paint) -> Run {
        Run { layer, min_zoom, max_zoom: 24.0, extent: 4096, shapes: vec![Shape { feature: feature.map(|f| (0, f)), paint }] }
    }

    #[test]
    fn tessellates_fills_strokes_and_circles() {
        let span = TileId::span(1) as f32;
        let line = Paint::Line { color: RED, width: [2.0, 4.0], cap: Cap::Butt, join: Join::Miter, dashes: Some(vec![3.0]), rings: false };
        let runs = [
            run(0, 0.0, None, Paint::Fill { color: RED, pattern: None }),
            run(1, 0.0, Some(0), Paint::Fill { color: RED, pattern: None }),
            run(2, 5.0, Some(1), line),
            run(3, 5.0, Some(2), Paint::Circle { color: RED, radius: [3.0, 3.0], stroke: Some((RED, [1.0, 1.0])) }),
        ];
        let m = tessellate(&tile(), &runs, 1);
        // A pattern fills with its colour (its image's average, from `shapes`).
        let patterned = [run(1, 0.0, Some(0), Paint::Fill { color: RED, pattern: Some("dots".into()) })];
        assert!(!tessellate(&tile(), &patterned, 1).indices.is_empty());
        // The background and the fill show at the same zooms: one run; the line and circle another.
        assert_eq!(m.runs.len(), 2);
        assert_eq!(m.shown(4.0).count(), 1);
        assert_eq!(m.shown(6.0).map(|r| r.len()).sum::<usize>(), m.indices.len());
        assert!(m.indices.iter().all(|&i| (i as usize) < m.vertices.len()));
        let fills: Vec<&TileMeshVertex> = m.vertices.iter().filter(|v| v.width == [0.0, 0.0]).collect();
        // Premultiplied, at the level and the next; in map units, y north from the corner.
        assert!(fills.iter().all(|v| v.color == [128, 0, 0, 128] && v.color_next == [255, 0, 0, 255]));
        assert!(fills.iter().all(|v| (0.0..=span).contains(&v.position[0]) && (-span..=0.0).contains(&v.position[1])));
        assert!(fills.iter().any(|v| v.position == [span, -span]), "the background fills the square");
        // The line's vertices lie on it, each side of it, dashed (an odd pattern repeated),
        // further along at its end.
        let strokes: Vec<&TileMeshVertex> = m.vertices.iter().filter(|v| v.width == [2.0, 4.0]).collect();
        // Unit normals, longer at the mitred corner.
        let reach = |v: &&TileMeshVertex| v.normal[0].hypot(v.normal[1]);
        assert!(strokes.iter().all(|v| (1.0 - 1e-3..=4.0).contains(&reach(v)) && v.dashes == [3.0, 3.0, 0.0, 0.0]));
        assert!(strokes.iter().any(|v| (reach(v) - std::f32::consts::SQRT_2).abs() < 1e-3));
        let quarter = span / 4.0;
        assert!(strokes.iter().any(|v| (v.along - 3.0 * quarter).abs() < 1.0));
        // The circle: a disc of its diameter and a ring across its edge.
        let widths: Vec<[f32; 2]> = m.vertices.iter().filter(|v| v.dashes == [0.0; 4] && v.width != [0.0, 0.0]).map(|v| v.width).collect();
        assert!(widths.contains(&[6.0, 6.0]) && widths.contains(&[5.0, 5.0]) && widths.contains(&[7.0, 7.0]));
    }

    #[test]
    fn a_tile_holds_so_many_vertices() {
        let points = vec![(0..10_000).map(|i| [i as f32 % 4096.0, (i / 4096) as f32]).collect::<Vec<_>>()];
        let mut t = tile();
        t.layers[0].features[2].geometry = points;
        let dots = Paint::Circle { color: RED, radius: [1.0, 1.0], stroke: None };
        let runs: Vec<Run> = (0..20).map(|l| run(l, 0.0, Some(2), dots.clone())).collect();
        // 200,000 circles of 17 vertices: stopped at the cap, give or take a circle.
        let m = tessellate(&t, &runs, 1);
        assert!(m.vertices.len() >= MAX_VERTICES && m.vertices.len() <= MAX_VERTICES + RIM + 1);
        assert!(m.indices.iter().all(|&i| (i as usize) < m.vertices.len()));
    }

    #[test]
    fn a_tile_that_panics_gives_no_mesh() {
        let out = parallel(&[1, 2, 3, 4], |&i: &i32| if i == 2 { panic!("hostile") } else { i * 10 });
        assert_eq!(out, [Some(10), None, Some(30), Some(40)]);
    }

    #[test]
    fn dash_patterns_fit_four_lengths_or_none() {
        assert_eq!(dash_pattern(None), Some([0.0; 4]));
        assert_eq!(dash_pattern(Some(&[3.0])), Some([3.0, 3.0, 0.0, 0.0]));
        assert_eq!(dash_pattern(Some(&[3.0, 1.0, 2.0, 1.0])), Some([3.0, 1.0, 2.0, 1.0]));
        // Three repeat to six, and five to ten: too long, not cut short.
        assert_eq!(dash_pattern(Some(&[3.0, 2.0, 1.0])), None);
        assert_eq!(dash_pattern(Some(&[1.0, 2.0, 3.0, 4.0, 5.0])), None);
    }

    #[test]
    fn a_mesh_is_made_again_when_other_sprites_are_read() {
        let ids = |list: &[&str]| list.iter().map(|id| id.to_string()).collect::<Vec<_>>();
        let (none, a, b, both) = (ids(&[]), ids(&["a"]), ids(&["b"]), ids(&["a", "b"]));
        // Without pattern fills it stands, whatever the sprites do.
        assert!(still_serves(false, None, false, &none) && still_serves(false, None, false, &both));
        // Made from all that could be read: it stands while the other keeps failing...
        assert!(still_serves(false, Some(&a), false, &a));
        // ...and while that one is asked for again...
        assert!(still_serves(false, Some(&a), true, &none));
        // ...and is made again, once, when it is read.
        assert!(!still_serves(false, Some(&a), false, &both));
        assert!(still_serves(false, Some(&both), false, &both));
        // As many sprites, but another one: made again.
        assert!(!still_serves(false, Some(&a), false, &b));
        // Made while they were on their way: until they've come.
        assert!(still_serves(true, Some(&none), true, &none));
        assert!(!still_serves(true, Some(&none), false, &both) && !still_serves(true, Some(&none), false, &none));
        // None could be read: it stands, not made again on every frame.
        assert!(still_serves(false, Some(&none), false, &none));
    }

    #[test]
    fn meshes_say_a_sprite_is_on_its_way() {
        use rustgis_tiles::style::{Source, Style, StyleLayer};
        // Left unanswered, so the sprite stays on its way.
        crate::tiles::tests::serve();
        let address = "https://stuck.test/mesh-sprite".to_owned();
        let resolved = |paint: serde_json::Value| {
            let layer = serde_json::json!({"id": "p", "type": "fill", "source": "s", "source-layer": "l", "paint": paint});
            let sprites = vec![("default".to_owned(), address.clone())];
            Arc::new(Resolved {
                style: Style {
                    name: String::new(),
                    sources: vec![("s".into(), Source::parse(&serde_json::json!({"type": "vector", "tiles": ["x/{z}/{x}/{y}"]})))],
                    layers: StyleLayer::parse(&layer).into_iter().collect(),
                    glyphs: None,
                    sprites: sprites.clone(),
                    sprites_left_out: Vec::new(),
                    sprites_listed: 1,
                    view: Default::default(),
                },
                sprites,
                attribution: Vec::new(),
                left_out: Vec::new(),
                made: std::sync::Mutex::new(web_time::Instant::now()),
                waiting: false,
            })
        };
        assert!(meshes(&resolved(serde_json::json!({"fill-pattern": "dots"})), "s", &[], 3, None).1);
        // Without pattern fills, a mesh doesn't wait for sprites.
        assert!(!meshes(&resolved(serde_json::json!({"fill-color": "#f00"})), "s", &[], 3, None).1);
    }

    #[test]
    fn meshes_are_made_once() {
        let mut t = tile();
        t.layers[0].name = "parks".into();
        let payload = Arc::new(Payload::Vector(t));
        use rustgis_tiles::style::{Source, Style, StyleLayer};
        let layer = serde_json::json!({"id": "p", "type": "fill", "source": "s", "source-layer": "parks", "paint": {"fill-color": "#f00"}});
        let style = Arc::new(Resolved {
            style: Style {
                name: String::new(),
                sources: vec![("s".into(), Source::parse(&serde_json::json!({"type": "vector", "tiles": ["x/{z}/{x}/{y}"]})))],
                layers: StyleLayer::parse(&layer).into_iter().collect(),
                glyphs: None,
                sprites: Vec::new(),
                sprites_left_out: Vec::new(),
                sprites_listed: 0,
                view: Default::default(),
            },
            sprites: Vec::new(),
            attribution: Vec::new(),
            left_out: Vec::new(),
            made: std::sync::Mutex::new(web_time::Instant::now()),
            waiting: false,
        });
        let at = Extent::new(0.0, 0.0, 1.0, 1.0);
        let tiles =
            [DataTile { payload: payload.clone(), at, z: 1, clip: at }, DataTile { payload: Arc::new(Payload::Empty), at, z: 1, clip: at }];
        let (a, _) = meshes(&style, "s", &tiles, 3, None);
        assert!(a[0].as_ref().is_some_and(|m| !m.indices.is_empty()) && a[1].is_none());
        let (b, _) = meshes(&style, "s", &tiles, 3, None);
        // A payload standing in twice is made once, for both.
        let twice = [tiles[0].clone(), tiles[0].clone()];
        let (t, _) = meshes(&style, "s", &twice, 6, None);
        assert!(Arc::ptr_eq(t[0].as_ref().unwrap(), t[1].as_ref().unwrap()));
        assert!(Arc::ptr_eq(a[0].as_ref().unwrap(), b[0].as_ref().unwrap()), "kept");
        // Another level is another mesh.
        let (c, _) = meshes(&style, "s", &tiles, 4, None);
        assert!(!Arc::ptr_eq(a[0].as_ref().unwrap(), c[0].as_ref().unwrap()));
        // Out of time before it starts: a round made, nearest the centre first; the rest later.
        let many: Vec<DataTile> = (0..64)
            .map(|i| {
                let at = Extent::new(f64::from(i), 0.0, f64::from(i) + 1.0, 1.0);
                DataTile { payload: Arc::new(Payload::Vector(tile_named("parks"))), at, z: 1, clip: at }
            })
            .collect();
        let (some, _) = meshes(&style, "s", &many, 5, Some((web_time::Instant::now(), [63.5, 0.5])));
        let made = some.iter().filter(|m| m.is_some()).count();
        assert!(made >= 1 && made < many.len() && some[63].is_some() && some[0].is_none(), "{made} made");
        let (all, _) = meshes(&style, "s", &many, 5, Some((web_time::Instant::now() + std::time::Duration::from_secs(60), [0.0, 0.0])));
        assert!(all.iter().all(Option::is_some));
    }

    #[test]
    fn convex_rings_are_fanned() {
        let square = [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0], [0.0, 0.0]];
        assert_eq!(convex(&square).map(<[_]>::len), Some(4));
        // Not convex, a star (turning twice round), a line, nothing at all: lyon's.
        assert!(convex(&[[0.0, 0.0], [10.0, 0.0], [5.0, 2.0], [10.0, 10.0], [0.0, 10.0]]).is_none());
        let star: Vec<[f32; 2]> = (0..5)
            .map(|i| {
                let a = i as f32 * 4.0 * std::f32::consts::PI / 5.0;
                [a.cos(), a.sin()]
            })
            .collect();
        assert!(convex(&star).is_none());
        assert!(convex(&[[0.0, 0.0], [1.0, 1.0], [2.0, 2.0]]).is_none());
        assert!(convex(&[[0.0, 0.0], [f32::NAN, 1.0], [2.0, 0.0]]).is_none());
        // Points along an edge: still convex, the fan covers it. A point twice over (an edge of
        // no length, no turn to read) is left to lyon.
        let edgy = [[0.0, 0.0], [5.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0], [0.0, 0.0]];
        assert_eq!(convex(&edgy).map(<[_]>::len), Some(5));
        assert!(convex(&[[0.0, 0.0], [10.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]]).is_none());
        // A sliver, nearly flat but turning one way: convex, and the fan covers it.
        assert_eq!(convex(&[[0.0, 0.0], [4096.0, 0.0], [4096.0, 1.0]]).map(<[_]>::len), Some(3));
        // Nearly flat, turning both ways: lyon's.
        assert!(convex(&[[0.0, 0.0], [2048.0, 1.0], [4096.0, 0.0], [4096.0, 1.0]]).is_none());
        // A convex building fills as lyon fills it: the same area.
        let area = |m: &TileMesh| {
            m.indices
                .chunks(3)
                .map(|t| {
                    let [a, b, c] = [t[0], t[1], t[2]].map(|i| m.vertices[i as usize].position);
                    ((b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])).abs() / 2.0
                })
                .sum::<f32>()
        };
        let mut t = tile();
        t.layers[0].features[0].geometry = vec![vec![[0.0, 0.0], [2048.0, 0.0], [2048.0, 2048.0], [0.0, 2048.0], [0.0, 0.0]]];
        let m = tessellate(&t, &[run(0, 0.0, Some(0), Paint::Fill { color: RED, pattern: None })], 1);
        let quarter = TileId::span(1) as f32 / 2.0;
        assert_eq!(m.vertices.len(), 4);
        assert!((area(&m) - quarter * quarter).abs() / (quarter * quarter) < 1e-4);
    }

    fn tile_named(name: &str) -> Tile {
        let mut t = tile();
        t.layers[0].name = name.into();
        t
    }
}
