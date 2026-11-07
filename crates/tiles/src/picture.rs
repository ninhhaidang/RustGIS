//! A style's vector shapes drawn once into tile pictures.
//!
//! Filling and stroking the thousands of paths in a vector tile is nearly all the cost of
//! drawing a style, and a map moving across its tiles would draw them all again every frame. So
//! a style's shapes (every layer but its labels) are drawn square by square into pictures, kept
//! here, and put in place like raster tiles: copied into the map on the CPU, a texture each moved
//! with the camera on the GPU. Labels aren't in them: they are collected every time, for the
//! renderer to place with the map's own.
//!
//! A picture is drawn at the map's zoom rounded to a [`STEPS`]th of a level and its side to whole
//! pixels, so it is put in place stretched by a fraction of a percent at most (and copied pixel
//! for pixel). Its square is a tile a level or more below the vector tiles drawn there: 256 to
//! 512 pixels a side at one or two pixels a point, about as much at others (192 to 384 at 1.5).
//!
//! A map drawn for the screen draws the pictures it lacks at a [`Pace`], as MapLibre shows the
//! tiles it has while it loads others: while the map moves, a missing picture isn't drawn when
//! others already drawn cover its square (the same square at a nearby zoom or another resolution,
//! as frames drawn mid-motion are drawn coarser, the square above it or the four below,
//! stretched into place), and those none covers are drawn a frame's worth at a time; once it
//! stops, the missing pictures are drawn a frame's worth at a time, nearest the middle first.

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};

use rustgis_geom::Extent;
use tiny_skia::Pixmap;
use web_time::{Duration, Instant};

use crate::cache::{self, Payload};
use crate::draw::{Drawn, Frame, Item, Pass, draw_layers, overhang};
use crate::source::Resolved;
use crate::sprite::{self, Load};
use crate::style::{LayerKind, Source};
use crate::template::{self, Placed, TileId};

/// The zooms a level a picture is drawn at.
const STEPS: f32 = 256.0;

/// The most picture memory kept, bytes.
const BUDGET: usize = if cfg!(target_arch = "wasm32") { 96 << 20 } else { 384 << 20 };

/// The largest side of a picture, pixels.
const MAX_SIDE: f32 = 2048.0;

/// How many levels of zoom from its own a picture may stand in for another, stretched or shrunk.
const STAND_IN_LEVELS: u8 = 2;

/// How long a map drawn at a [`Pace`] draws pictures for in one frame.
const FRAME: Duration = Duration::from_millis(30);

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    /// The resolved style (its address; [`Entry::style`] tells whether it is still the same).
    style: usize,
    tile: TileId,
    /// The zoom, in [`STEPS`].
    step: i32,
    /// Pixels per point.
    ppp: u32,
}

struct Entry {
    style: Weak<Resolved>,
    picture: Arc<Payload>,
    bytes: usize,
    used: u64,
    /// Drawn while some of it was on its way, at this cache generation: drawn again once the
    /// cache has more.
    waiting: Option<u64>,
    /// For a style with pattern fills, the ids of the sprites it was drawn from.
    sheets: Option<Vec<String>>,
}

/// Whether any of a style's sprites are on their way and, if it shows pattern fills at `zoom` and
/// none are, the ids of those read: what its pictures' fills are drawn from now.
fn sprites_read(r: &Resolved, zoom: f32, ppp: f32) -> (bool, Option<Vec<String>>) {
    if !r.style.layers.iter().any(|l| l.visible_at(zoom) && l.draws_patterns()) {
        return (false, None);
    }
    match sprite::load(&r.sprites, ppp >= 1.5) {
        Load::Pending => (true, None),
        Load::Ready(s) => (false, Some(s.sheet_ids())),
        Load::Failed => (false, Some(Vec::new())),
    }
}

/// Whether a picture drawn from the sprites `sheets` still serves while `pending` of them are on
/// their way and `read` have been read. Other sprites read (one that failed read after all, or
/// another failing instead) draw it again, once; until then it stands.
fn sprites_serve(sheets: Option<&[String]>, pending: bool, read: Option<&[String]>) -> bool {
    pending || sheets == read
}

#[derive(Default)]
struct Store {
    entries: HashMap<Key, Entry>,
    bytes: usize,
    /// Counts the calls to [`cover`], for the least recently used.
    tick: u64,
}

/// How a map drawn for the screen draws the pictures it lacks (see [`paced`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pace {
    /// The map moves: pictures already drawn stand in, stretched, for missing ones wherever
    /// they cover their squares; only the squares none covers are drawn, for about a frame
    /// ([`FRAME`]), nearest the middle first, the others left out meanwhile.
    Moving,
    /// The map has stopped: the missing pictures are drawn for about a frame ([`FRAME`]), those
    /// nothing covers and then those nearest the middle first; pictures already drawn stand in
    /// for the rest meanwhile.
    Settling,
}

/// What a map drawn at a [`Pace`] still lacks: when to draw it again.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Unfinished {
    /// Pictures stand in for others while the map moves: once it stops.
    Later,
    /// Pictures are still to be drawn: the next frame.
    Next,
}

/// A map being drawn at a pace (inside [`paced`]).
#[derive(Clone, Copy)]
struct Paced {
    pace: Pace,
    /// What it still lacks.
    lacks: Option<Unfinished>,
    /// Until when it draws what it lacks, in this frame: one budget for all its layers.
    until: Instant,
}

thread_local! {
    static PACE: Cell<Option<Paced>> = const { Cell::new(None) };
}

/// Runs `f`, drawing a map for the screen, with the pictures it lacks drawn at `pace` (for about
/// a frame, [`FRAME`], from now, shared by all its layers); and what the map drawn still lacks,
/// if anything.
pub fn paced<R>(pace: Pace, f: impl FnOnce() -> R) -> (R, Option<Unfinished>) {
    /// Puts back what was there before, however `f` ends.
    struct Restore(Option<Paced>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let inner = PACE.with(|p| p.replace(self.0)).and_then(|p| p.lacks);
            // Inside another call, it learns of these too.
            if let Some(outer) = self.0 {
                PACE.with(|p| p.set(Some(Paced { lacks: outer.lacks.max(inner), ..outer })));
            }
        }
    }
    let now = Instant::now() + FRAME;
    // Inside another call, within its frame.
    let until = PACE.with(|p| p.get()).map_or(now, |p| p.until.min(now));
    let restore = Restore(PACE.with(|p| p.replace(Some(Paced { pace, lacks: None, until }))));
    let r = f();
    let lacks = PACE.with(|p| p.get()).and_then(|p| p.lacks);
    drop(restore);
    (r, lacks)
}

/// The pace the map being drawn on this thread draws what it lacks at (inside [`paced`]), and
/// until when it may draw in this frame (shared by all its layers): for other renderers that
/// draw a frame's worth at a time (the GPU map's tile meshes).
pub fn pace() -> Option<(Pace, Instant)> {
    PACE.with(|p| p.get()).map(|p| (p.pace, p.until))
}

/// Notes, for [`paced`], what the map being drawn on this thread still lacks: a renderer drawing
/// at its pace calls it when it left something out or stood something in, so the map is drawn
/// again (`Next`: the next frame; `Later`: once it stops moving). Outside `paced`, nothing.
pub fn unfinished(u: Unfinished) {
    PACE.with(|p| {
        if let Some(paced) = p.get() {
            p.set(Some(Paced { lacks: paced.lacks.max(Some(u)), ..paced }));
        }
    });
}

fn lock() -> MutexGuard<'static, Store> {
    static S: OnceLock<Mutex<Store>> = OnceLock::new();
    S.get_or_init(Mutex::default).lock().unwrap_or_else(|e| e.into_inner())
}

/// Forgets every picture (with [`cache::clear`]).
pub(crate) fn forget() {
    let mut s = lock();
    s.entries.clear();
    s.bytes = 0;
}

/// Whether the style draws shapes from vector tiles, or relief or hillshading from elevation
/// tiles (as costly, a pixel at a time), at `zoom`.
fn draws_shapes(r: &Resolved, zoom: f32) -> bool {
    r.style.layers.iter().any(|l| {
        let source = || l.source.as_deref().and_then(|id| r.style.source(id));
        l.visible_at(zoom)
            && match l.kind {
                LayerKind::Fill | LayerKind::FillExtrusion | LayerKind::Line | LayerKind::Circle => {
                    matches!(source(), Some(Source::Vector { .. }))
                }
                LayerKind::Hillshade => matches!(source(), Some(Source::RasterDem { .. })),
                // A colour ramp it can't draw is no reason for a picture.
                LayerKind::ColorRelief => matches!(source(), Some(Source::RasterDem { .. })) && l.color_relief_ramp(zoom).is_some(),
                _ => false,
            }
    })
}

/// The pictures of `r`'s shapes over `view`, drawing those missing, and out to `reach` (only
/// those already drawn), bottom first; `None` when the style draws no vector shapes here, or the
/// view is too wide for pictures (the whole world on the globe). `frame` gives the scale.
pub(crate) fn cover(r: &Arc<Resolved>, frame: &Frame, view: &Extent, reach: Option<&Extent>, pending: &mut bool) -> Option<Vec<Item>> {
    let ppp = frame.ppp;
    if !(ppp.is_finite() && ppp > 0.0 && frame.m_per_px.is_finite() && frame.m_per_px > 0.0) || view.width() > template::HALF_WORLD {
        return None;
    }
    let zoom = template::style_zoom(frame.m_per_px * f64::from(ppp));
    if !draws_shapes(r, zoom) {
        return None;
    }
    let step = (zoom * STEPS).round();
    let level = (step / STEPS).floor();
    // Squares of 256 to 512 pixels: those the size of a 256-point tile at one pixel a point, of
    // a 128-point tile at two, and so on. Smaller ones would each look through more tiles'
    // features; larger ones reach further past the view.
    let down = 1.0 + ppp.log2().round().clamp(0.0, 3.0);
    let z = (level + down) as u8;
    let side = (512.0 * ppp * (step / STEPS - level - down).exp2()).round().clamp(16.0, MAX_SIDE) as u32;
    let m_per_px = TileId::span(z) / f64::from(side);
    let key = |tile| Key { style: Arc::as_ptr(r) as usize, tile, step: step as i32, ppp: ppp.to_bits() };
    let generation = cache::generation();
    let (sprites_pending, read) = sprites_read(r, zoom, ppp);
    // A view of more squares than a covering holds (a huge window) is drawn straight instead;
    // a reach of more keeps to the view.
    let span = TileId::span(z);
    if ((view.width() / span).ceil() + 1.0) * ((view.height() / span).ceil() + 1.0) > template::MAX_TILES as f64 {
        return None;
    }
    let squares = reach
        .map(|e| template::covering(e, z))
        .filter(|s| !s.is_empty() && s.len() < template::MAX_TILES)
        .unwrap_or_else(|| template::covering(view, z));
    let mut found: Vec<(Placed, Option<Arc<Payload>>)> = Vec::with_capacity(squares.len());
    let mut missing = Vec::new();
    {
        let mut s = lock();
        s.tick += 1;
        let tick = s.tick;
        for p in squares {
            let in_view = p.extent().intersects(view);
            let e = s.entries.get_mut(&key(p.id)).filter(|e| e.style.upgrade().is_some_and(|x| Arc::ptr_eq(&x, r)));
            match e {
                Some(e)
                    if !in_view
                        || (e.waiting.is_none_or(|g| g == generation)
                            && sprites_serve(e.sheets.as_deref(), sprites_pending, read.as_deref())) =>
                {
                    e.used = tick;
                    // (A sprite on its way may add to it: an export waits.)
                    *pending |= in_view && (e.waiting.is_some() || sprites_pending);
                    found.push((p, Some(e.picture.clone())));
                }
                _ if in_view => {
                    missing.push(found.len());
                    found.push((p, None));
                }
                _ => {}
            }
        }
    }
    // Drawn for the screen, what pictures already drawn can stand in for those missing, and
    // which of those to draw now (see `Pace`).
    let paced = PACE.with(|p| p.get());
    let pace = paced.map(|p| p.pace);
    let mut covers: HashMap<usize, Vec<(Key, Extent, u8, Extent)>> = HashMap::new();
    if pace.is_some() && !missing.is_empty() {
        let (cx, cy) = ((view.min_x + view.max_x) / 2.0, (view.min_y + view.max_y) / 2.0);
        let far = |i: &usize| {
            found.get(*i).map_or(f64::MAX, |f| {
                let e = f.0.extent();
                ((e.min_x + e.max_x) / 2.0 - cx).hypot((e.min_y + e.max_y) / 2.0 - cy)
            })
        };
        missing.sort_by(|a, b| far(a).total_cmp(&far(b)));
        let s = lock();
        let index = stand_ins(&s, r, step as i32);
        for &i in &missing {
            if let Some(c) = found.get(i).and_then(|f| stand_in(f.0, &index, view)) {
                covers.insert(i, c);
            }
        }
        // Squares nothing covers first (the sort keeps them nearest the middle first).
        missing.sort_by_key(|i| covers.contains_key(i));
    }
    let todo: Vec<usize> = match pace {
        Some(Pace::Moving) => missing.iter().copied().filter(|i| !covers.contains_key(i)).collect(),
        _ => missing.clone(),
    };
    let squares: Vec<Placed> = todo.iter().filter_map(|&i| found.get(i).map(|f| f.0)).collect();
    // How far past its square a picture looks for shapes, in points: a line or circle drawn from
    // a neighbouring tile reaches into it by up to half its width.
    let reach = r.style.layers.iter().filter(|l| l.visible_at(zoom)).map(|l| overhang(l, zoom)).fold(0.0f32, f32::max);
    let until = paced.map(|p| p.until);
    let drawn = draw_all(r, &squares, side, m_per_px, ppp, f64::from(reach), until);
    let mut stood_in = Vec::new();
    {
        let mut s = lock();
        let tick = s.tick;
        let mut added = false;
        for (&i, d) in todo.iter().zip(drawn) {
            let (Some(slot), Some((pm, waiting))) = (found.get_mut(i), d) else { continue };
            *pending |= waiting;
            let bytes = pm.data().len();
            let picture = Arc::new(Payload::Image(pm));
            let entry = Entry {
                style: Arc::downgrade(r),
                picture: picture.clone(),
                bytes,
                used: tick,
                waiting: waiting.then_some(generation),
                sheets: read.clone(),
            };
            s.bytes += bytes;
            if let Some(old) = s.entries.insert(key(slot.0.id), entry) {
                s.bytes = s.bytes.saturating_sub(old.bytes);
            }
            slot.1 = Some(picture);
            added = true;
        }
        // Those still missing: stood in for, or left out until the next frame.
        for &i in &missing {
            if found.get(i).is_none_or(|f| f.1.is_some()) {
                continue;
            }
            // Only standing in while the map moves waits for it to stop.
            unfinished(if pace == Some(Pace::Moving) && covers.contains_key(&i) { Unfinished::Later } else { Unfinished::Next });
            for &(k, at, z, clip) in covers.get(&i).into_iter().flatten() {
                if let Some(e) = s.entries.get_mut(&k) {
                    e.used = tick;
                    stood_in.push(Item { payload: e.picture.clone(), at, z, clip });
                }
            }
        }
        // Only when pictures were added: a frame of pictures already drawn costs nothing here.
        if added {
            evict(&mut s);
        }
    }
    let mut items: Vec<Item> =
        found.into_iter().filter_map(|(p, picture)| Some(Item { payload: picture?, at: p.extent(), z, clip: p.extent() })).collect();
    items.extend(stood_in);
    Some(items)
}

/// The pictures of `r` drawn [`STAND_IN_LEVELS`] of zoom or less from `step`, at any
/// resolution, that can stand in for others: by tile, the one at the nearest zoom, then the sharpest.
fn stand_ins(s: &Store, r: &Arc<Resolved>, step: i32) -> HashMap<TileId, Key> {
    let style = Arc::as_ptr(r) as usize;
    let mut best: HashMap<TileId, Key> = HashMap::new();
    // Pixels per point are positive: their bits order as they do.
    let rank = |k: &Key| ((k.step - step).abs(), std::cmp::Reverse(k.ppp));
    for (k, e) in &s.entries {
        if k.style != style
            || (k.step - step).abs() > STEPS as i32 * i32::from(STAND_IN_LEVELS)
            || e.style.upgrade().is_none_or(|x| !Arc::ptr_eq(&x, r))
        {
            continue;
        }
        match best.get(&k.tile) {
            Some(b) if rank(b) <= rank(k) => {}
            _ => {
                best.insert(k.tile, *k);
            }
        }
    }
    best
}

/// What covers square `p` from `index` (see [`stand_ins`]) over `view`: its own picture, one of
/// a square above it, or those of the four below it in view; each with where it is placed, its
/// tile's zoom and the part of it shown.
fn stand_in(p: Placed, index: &HashMap<TileId, Key>, view: &Extent) -> Option<Vec<(Key, Extent, u8, Extent)>> {
    if let Some(k) = index.get(&p.id) {
        return Some(vec![(*k, p.extent(), p.id.z, p.extent())]);
    }
    for up in 1..=STAND_IN_LEVELS + 1 {
        if let Some(a) = p.id.ancestor(up)
            && let Some(k) = index.get(&a)
        {
            return Some(vec![(*k, Placed { id: a, wx: p.wx.div_euclid(1 << up) }.extent(), a.z, p.extent())]);
        }
    }
    let mut below = Vec::new();
    for c in p.id.children() {
        let wx = p.wx * 2 + i64::from(c.x) - 2 * i64::from(p.id.x);
        let at = Placed { id: c, wx }.extent();
        match index.get(&c) {
            Some(k) => below.push((*k, at, c.z, at)),
            None if at.intersects(view) => return None,
            None => {}
        }
    }
    (!below.is_empty()).then_some(below)
}

/// Drops the least recently used pictures past the budget, and those of styles gone. Those
/// used by the latest call stay, whatever they weigh.
fn evict(s: &mut Store) {
    s.entries.retain(|_, e| e.style.strong_count() > 0);
    s.bytes = s.entries.values().map(|e| e.bytes).sum();
    if s.bytes <= BUDGET {
        return;
    }
    let tick = s.tick;
    let mut old: Vec<(u64, Key, usize)> = s.entries.iter().filter(|(_, e)| e.used != tick).map(|(k, e)| (e.used, *k, e.bytes)).collect();
    old.sort_unstable_by_key(|o| o.0);
    for (_, k, bytes) in old {
        if s.bytes <= BUDGET {
            break;
        }
        s.entries.remove(&k);
        s.bytes = s.bytes.saturating_sub(bytes);
    }
}

/// Draws the pictures of `squares`: each a `side`-pixel pixmap and whether some of it is still
/// on its way, `None` for those not drawn. Natively the squares are shared among threads. With
/// `until`, drawing stops once it has passed, after a picture (a round of them natively).
fn draw_all(
    r: &Resolved,
    squares: &[Placed],
    side: u32,
    m_per_px: f64,
    ppp: f32,
    reach: f64,
    until: Option<Instant>,
) -> Vec<Option<(Pixmap, bool)>> {
    let Some(until) = until else { return draw_some(r, squares, side, m_per_px, ppp, reach) };
    let round = if cfg!(target_arch = "wasm32") { 1 } else { std::thread::available_parallelism().map_or(1, |n| n.get()).min(16) };
    let mut out = Vec::with_capacity(squares.len());
    for some in squares.chunks(round) {
        if !out.is_empty() && Instant::now() >= until {
            out.extend(some.iter().map(|_| None));
        } else {
            out.extend(draw_some(r, some, side, m_per_px, ppp, reach));
        }
    }
    out
}

/// Draws the pictures of all of `squares` (see [`draw_all`]).
fn draw_some(r: &Resolved, squares: &[Placed], side: u32, m_per_px: f64, ppp: f32, reach: f64) -> Vec<Option<(Pixmap, bool)>> {
    let one = |p: &Placed| draw_one(r, p, side, m_per_px, ppp, reach);
    #[cfg(not(target_arch = "wasm32"))]
    {
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(squares.len()).min(16);
        if threads > 1 {
            let next = std::sync::atomic::AtomicUsize::new(0);
            let out: Mutex<Vec<Option<(Pixmap, bool)>>> = Mutex::new((0..squares.len()).map(|_| None).collect());
            std::thread::scope(|scope| {
                for _ in 0..threads {
                    scope.spawn(|| {
                        loop {
                            let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            let Some(p) = squares.get(i) else { break };
                            let d = one(p);
                            if let Some(slot) = out.lock().unwrap_or_else(|e| e.into_inner()).get_mut(i) {
                                *slot = d;
                            }
                        }
                    });
                }
            });
            return out.into_inner().unwrap_or_else(|e| e.into_inner());
        }
    }
    squares.iter().map(one).collect()
}

/// Draws the shapes of one square, with those of the tiles around it out to `reach` points.
fn draw_one(r: &Resolved, p: &Placed, side: u32, m_per_px: f64, ppp: f32, reach: f64) -> Option<(Pixmap, bool)> {
    let mut pm = Pixmap::new(side, side)?;
    let e = p.extent();
    let frame = Frame { left: e.min_x, top: e.max_y, m_per_px, ppp };
    // Shapes from the tiles next to it too, where they reach into it.
    let pad = reach * f64::from(ppp) * m_per_px;
    let view = Extent::new(e.min_x - pad, e.min_y - pad, e.max_x + pad, e.max_y + pad);
    let mut d = Drawn::default();
    draw_layers(Some(&mut pm), (side, side), &view, r, &frame, Pass::Shapes, &mut d);
    Some((pm, d.pending))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::draw::{draw, place};
    use crate::mvt::{Feature, GeomKind, Layer, Tile};
    use crate::source::{Kind, Resolve, Spec, resolve};

    /// A one-tile archive (zoom 0) of a park, two roads (one dashed) and a point, with a style
    /// drawing them all, so a view at any zoom draws the tile.
    fn spec(name: &str) -> (Spec, std::path::PathBuf) {
        let feature = |kind, geometry: Vec<Vec<[f32; 2]>>, class: &str| Feature {
            id: None,
            kind,
            geometry,
            props: vec![("class".into(), crate::mvt::Value::String(class.into()))],
        };
        let tile = Tile {
            layers: vec![
                Layer {
                    name: "parks".into(),
                    extent: 4096,
                    features: vec![feature(
                        GeomKind::Polygon,
                        vec![vec![[1000.0, 1000.0], [3000.0, 1100.0], [2900.0, 3000.0], [1100.0, 2900.0], [1000.0, 1000.0]]],
                        "park",
                    )],
                },
                Layer {
                    name: "roads".into(),
                    extent: 4096,
                    features: vec![
                        feature(GeomKind::LineString, vec![vec![[0.0, 300.0], [4096.0, 3900.0]]], "major"),
                        feature(GeomKind::LineString, vec![vec![[200.0, 3800.0], [2000.0, 2100.0], [3900.0, 200.0]]], "path"),
                    ],
                },
                Layer { name: "pois".into(), extent: 4096, features: vec![feature(GeomKind::Point, vec![vec![[2048.0, 2048.0]]], "cafe")] },
            ],
        };
        let dir = std::env::temp_dir().join(format!("rustgis-picture-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.pmtiles");
        let new = crate::pmtiles::NewArchive {
            tile_type: crate::pmtiles::TileType::Mvt,
            gzipped_tiles: false,
            bounds: [-180.0, -85.0, 180.0, 85.0],
            metadata: serde_json::json!({"vector_layers": [{"id": "parks"}, {"id": "roads"}, {"id": "pois"}]}),
        };
        std::fs::write(&path, crate::pmtiles::write(vec![(0, 0, 0, crate::mvt::encode(&tile))], &new).unwrap()).unwrap();
        let style = r##"[
            {"id": "bg", "type": "background", "paint": {"background-color": "#f0eee8"}},
            {"id": "park", "type": "fill", "source": "x", "source-layer": "parks", "paint": {"fill-color": "#9c9", "fill-outline-color": "#363"}},
            {"id": "major", "type": "line", "source": "x", "source-layer": "roads", "filter": ["==", "class", "major"],
                "layout": {"line-join": "round"}, "paint": {"line-color": "#e92", "line-width": 9}},
            {"id": "path", "type": "line", "source": "x", "source-layer": "roads", "filter": ["==", "class", "path"],
                "paint": {"line-color": "#333", "line-width": 2, "line-dasharray": [3, 2]}},
            {"id": "poi", "type": "circle", "source": "x", "source-layer": "pois", "paint": {"circle-color": "#c00", "circle-radius": 7}},
            {"id": "label", "type": "symbol", "source": "x", "source-layer": "pois", "layout": {"text-field": ["get", "class"]}}
        ]"##;
        let spec = Spec {
            kind: Kind::PmTiles,
            url: path.display().to_string(),
            subdomains: Vec::new(),
            min_zoom: 0,
            max_zoom: 14,
            tile_size: 512,
            tms: false,
            attribution: String::new(),
            layers: Vec::new(),
            style_layers: style.into(),
            hidden_layers: Vec::new(),
        };
        (spec, dir)
    }

    /// The style resolved, once its archive is read.
    fn resolved(spec: &Spec) -> Arc<Resolved> {
        for _ in 0..500 {
            if let Resolve::Ready(r) = resolve(spec) {
                return r;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("the archive wasn't read");
    }

    /// A 300 × 200 point view at `zoom`, `ppp` pixels a point, with the middle of the world in
    /// it, on the pixels of the pictures (elsewhere they are put in place to the nearest pixel).
    fn frame(zoom: f64, ppp: f32) -> Frame {
        let half = template::HALF_WORLD;
        let m_per_px = 2.0 * half / (512.0 * zoom.exp2()) / f64::from(ppp);
        let on_pixels = |px: f64| ((half - px * m_per_px) / m_per_px).round() * m_per_px;
        Frame { left: on_pixels(195.0 * f64::from(ppp)) - half, top: half - on_pixels(70.0 * f64::from(ppp)), m_per_px, ppp }
    }

    /// Draws `spec` until all of it has arrived.
    fn drawn(spec: &Spec, f: &Frame, ppp: f32) -> (Pixmap, crate::draw::Drawn) {
        let (w, h) = ((300.0 * ppp) as u32, (200.0 * ppp) as u32);
        for _ in 0..500 {
            let mut pm = Pixmap::new(w, h).unwrap();
            let d = draw(&mut pm, spec, 1.0, f, true);
            if !d.pending {
                return (pm, d);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("still pending");
    }

    #[test]
    fn a_picture_is_drawn_again_when_other_sprites_are_read() {
        let ids = |list: &[&str]| list.iter().map(|id| id.to_string()).collect::<Vec<_>>();
        let (none, a, b, both) = (ids(&[]), ids(&["a"]), ids(&["a2"]), ids(&["a", "a2"]));
        // No pattern fills: it stands.
        assert!(sprites_serve(None, false, None));
        // Drawn from all that could be read: it stands while the other keeps failing...
        assert!(sprites_serve(Some(&a), false, Some(&a)) && sprites_serve(Some(&none), false, Some(&none)));
        // ...and while that one is asked for again...
        assert!(sprites_serve(Some(&a), true, None));
        // ...and is drawn again when it is read, or another is read in its place.
        assert!(!sprites_serve(Some(&a), false, Some(&both)) && !sprites_serve(Some(&a), false, Some(&b)));
        // Drawn while they were on their way: once they've come.
        assert!(!sprites_serve(None, false, Some(&both)));
    }

    #[test]
    fn pictures_draw_what_the_style_draws() {
        let _turn = crate::cache::turn();
        let (spec, dir) = spec("same");
        let r = resolved(&spec);
        // Zooms whose pictures are a whole number of pixels across, so they are at the view's
        // own scale (in between, a picture is a hair larger or smaller: a long dashed line's
        // dashes then fall a pixel or two along).
        for (zoom, ppp) in [(1.0, 1.0), (2.0, 2.0), (4.0, 1.5), (3.0, 1.0)] {
            let f = frame(zoom, ppp);
            let (pm, d) = drawn(&spec, &f, ppp);
            // As the style drawn straight into the view, but for anti-aliasing where the pictures meet.
            let mut direct = Pixmap::new(pm.width(), pm.height()).unwrap();
            let mut nothing = crate::draw::Drawn::default();
            draw_layers(
                Some(&mut direct),
                (pm.width(), pm.height()),
                &f.extent(pm.width(), pm.height()),
                &r,
                &f,
                Pass::Shapes,
                &mut nothing,
            );
            let diff: Vec<u8> = pm.data().iter().zip(direct.data()).map(|(a, b)| a.abs_diff(*b)).collect();
            let off = diff.chunks(4).filter(|p| p.iter().any(|d| *d > 40)).count();
            assert!(off * 400 < diff.len() / 4, "zoom {zoom}: {off} pixels differ");
            // Each colour is there: the park, both roads, the point.
            for c in [[0x99, 0xcc, 0x99], [0xee, 0x99, 0x22], [0x33, 0x33, 0x33], [0xcc, 0x00, 0x00]] {
                assert!(pm.pixels().iter().any(|p| [p.red(), p.green(), p.blue()] == c), "zoom {zoom}: no {c:?}");
            }
            // The labels still come out, on their own.
            assert_eq!(d.labels.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), ["cafe"], "zoom {zoom}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn pictures_drawn_stand_in_while_the_map_moves() {
        let _turn = crate::cache::turn();
        let (spec, dir) = spec("moving");
        let r = resolved(&spec);
        let f = frame(2.2, 1.0);
        drawn(&spec, &f, 1.0);
        let view = f.extent(300, 200);
        let mut pending = false;
        let held: Vec<Arc<Payload>> = cover(&r, &f, &view, None, &mut pending).unwrap().into_iter().map(|i| i.payload).collect();
        let theirs = |items: &[Item]| items.iter().all(|i| held.iter().any(|h| Arc::ptr_eq(h, &i.payload)));
        // A little further in, or coarser (as frames drawn mid-motion are): those drawn stand in.
        // Over the same ground: the view's squares are those drawn, or above or below them.
        for g in [frame(2.3, 1.0), frame(2.9, 1.0), frame(2.2, 0.6), frame(1.6, 1.0)] {
            let (items, lacks) = paced(Pace::Moving, || cover(&r, &g, &view, None, &mut pending).unwrap());
            assert!(lacks == Some(Unfinished::Later) && !items.is_empty() && theirs(&items), "stood in at {}", g.m_per_px);
            // Each covering the view's squares, where it is.
            assert!(items.iter().all(|i| i.at.intersects(&i.clip) && i.clip.width() > 0.0));
        }
        // Still, or over two levels away: drawn.
        let g = frame(2.3, 1.0);
        let still = cover(&r, &g, &view, None, &mut pending).unwrap();
        assert!(!theirs(&still));
        let far = frame(5.5, 1.0);
        let (items, lacks) = paced(Pace::Moving, || cover(&r, &far, &view, None, &mut pending).unwrap());
        // Nothing stands in that far away (a slow machine may leave some for the next frame).
        assert!(lacks != Some(Unfinished::Later) && !theirs(&items));
        // Settling, the exact pictures are drawn while there's time (a round at least).
        let g = frame(2.35, 1.0);
        let (items, lacks) = paced(Pace::Settling, || cover(&r, &g, &view, None, &mut pending).unwrap());
        // A round at least is new; complete, nothing held stands in.
        let held_in = |i: &Item| held.iter().any(|h| Arc::ptr_eq(h, &i.payload));
        assert!(lacks != Some(Unfinished::Later) && items.iter().any(|i| !held_in(i)));
        assert!(lacks.is_some() || !items.iter().any(held_in));
        // Inside nested calls, the outer one hears of pictures standing in.
        let ((_, inner), outer) = paced(Pace::Settling, || paced(Pace::Moving, || cover(&r, &frame(2.25, 1.0), &view, None, &mut pending)));
        assert!(inner == Some(Unfinished::Later) && outer == inner);
        assert!(PACE.with(|p| p.get()).is_none());
        // One deadline for the frame, whoever asks; an inner call keeps within it.
        let ((a, b, inner), _) = paced(Pace::Settling, || {
            let a = pace().map(|p| p.1);
            std::thread::sleep(Duration::from_millis(2));
            let (inner, _) = paced(Pace::Moving, || pace().map(|p| p.1));
            (a, pace().map(|p| p.1), inner)
        });
        assert!(a.is_some() && a == b && inner <= a);
        assert!(pace().is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn pictures_are_drawn_a_round_at_least_in_a_frame() {
        let _turn = crate::cache::turn();
        let (spec, dir) = spec("round");
        let r = resolved(&spec);
        let squares: Vec<Placed> = template::covering(&frame(3.0, 1.0).extent(300, 200), 4);
        assert!(squares.len() > 1);
        let m_per_px = TileId::span(4) / 256.0;
        // Out of time before it starts: still a round drawn, so a frame always draws more.
        let some = draw_all(&r, &squares, 256, m_per_px, 1.0, 16.0, Some(Instant::now()));
        assert_eq!(some.len(), squares.len());
        assert!(some.first().is_some_and(Option::is_some));
        // With time, all of them.
        let all = draw_all(&r, &squares, 256, m_per_px, 1.0, 16.0, Some(Instant::now() + Duration::from_secs(60)));
        assert!(all.iter().all(Option::is_some));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn pictures_are_drawn_once() {
        let _turn = crate::cache::turn();
        let (spec, dir) = spec("once");
        resolved(&spec);
        let f = frame(2.2, 1.0);
        drawn(&spec, &f, 1.0);
        let view = f.extent(300, 200);
        let placed = || place(&spec, 1.0, &f, &view, &view, None).unwrap().tiles;
        let (a, b) = (placed(), placed());
        assert!(!a.is_empty());
        assert!(a.iter().zip(&b).all(|(a, b)| Arc::ptr_eq(&a.picture, &b.picture)), "the same pictures");
        // A step along keeps those still in view, and the labels come with them.
        let moved = Frame { left: f.left + 90.0 * f.m_per_px, ..f };
        let p = place(&spec, 1.0, &moved, &moved.extent(300, 200), &moved.extent(300, 200), Some((300, 200))).unwrap();
        assert!(p.tiles.iter().any(|t| a.iter().any(|o| Arc::ptr_eq(&o.picture, &t.picture))));
        assert_eq!(p.labels.len(), 1);
        // Wider than the world (the globe's mosaic) or a broken scale: drawn straight instead.
        let r = resolved(&spec);
        let mut pending = false;
        let world = Extent::new(-template::HALF_WORLD * 1.5, -1e6, template::HALF_WORLD * 1.5, 1e6);
        assert!(cover(&r, &f, &world, None, &mut pending).is_none());
        // More squares than a covering holds: drawn straight too, not left blank.
        let near = frame(12.0, 1.0);
        let huge = near.extent(20_000, 10_000);
        assert!(huge.width() < template::HALF_WORLD);
        assert!(cover(&r, &near, &huge, None, &mut pending).is_none());
        for m_per_px in [0.0, f64::NAN, f64::INFINITY] {
            assert!(cover(&r, &Frame { m_per_px, ..f }, &view, None, &mut pending).is_none());
        }
        assert!(cover(&r, &Frame { ppp: f32::NAN, ..f }, &view, None, &mut pending).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }
}
