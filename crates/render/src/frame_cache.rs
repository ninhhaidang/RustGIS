//! A screen frame's picture kept up to a layer, so the next frame of the same view draws only
//! from the first layer that changed (#1057): restyling a layer over a basemap, its tile layers
//! and the layers below it draws that layer and those above it, not the whole map again.
//!
//! A kept picture is a [`Checkpoint`]: the pixels of a frame as they stood before one of its
//! layers was drawn, with the label candidates, graphics text and tile output gathered so far.
//! What it was drawn from is kept beside it as looks ([`rustgis_map::look`]): the map's, each
//! layer's, and the frame's own (view, expression environment, tile and source generations).
//! A frame starts from the checkpoint when everything it holds still looks the same, which makes
//! it the same frame, pixel for pixel, as drawing it all again: the layers below draw first in
//! either case, from the same state.
//!
//! The layer the checkpoint stands under is the one being changed, as a slider drags: when it is
//! drawn on its own and composited (see-through or blended), its picture alone is kept too, as an
//! [`Alone`], with what the frame had gathered once it was drawn. A frame in which that layer
//! looks the same or only its transparency changed ([`Change::Transparency`]) composites that
//! picture at the layer's opacity instead of drawing the layer: the same steps a frame drawn
//! whole takes, so again the same pixels. A layer that shows a selection or graphics text isn't
//! kept alone: both are drawn after the composite, the text in the layer's opacity.
//!
//! Only flat, north-up frames for the screen keep pictures: not exports, recordings, the GPU,
//! the globe, turned or tilted views, nor maps whose layers read others while drawing (a mask,
//! labels turned square to another layer's lines, terrain water). A layer read in place from its
//! source or a raster has no look ([`LayerLook::of`]): it counts as changed in every frame, so a
//! picture is kept only from below it.
//!
//! Each layer's label candidates are kept beside the picture too ([`LayerLabels`]): a layer drawn
//! again over a restored picture, or in a frame of the same view drawn whole, that looks the same
//! or only its transparency changed takes the candidates it gave last time instead of working them
//! out again. Labels are still placed over all layers, since one layer's labels can hide another's.

use rustgis_data::query::Env;
use rustgis_map::look::{Change, LayerLook, MapLook};
use tiny_skia::Pixmap;

use crate::tiles::TileOutput;
use crate::{Candidate, PlacedLabel, Sprites, View};

/// The most pixels the kept pictures hold together, checkpoints and layers alone, about 66 MB:
/// two views of 3840 × 2160, or one with its layer alone; two views with their layers alone at
/// 1920 × 1080.
const MAX_PIXELS: u64 = 2 * 3840 * 2160;

/// The most pixels one frame's marker sprites can hold ([`crate::MAX_SPRITES`] of at most
/// [`crate::MAX_SPRITE_REACH`] around their centre): about 2.5 million, 9.8 MB. Each kept view
/// may hold that beside its pictures, so the sprites never cost a view its layer alone: with them
/// the kept pictures stay within about 86 MB.
const SPRITE_PIXELS: u64 = crate::MAX_SPRITES as u64 * (2 * crate::MAX_SPRITE_REACH as u64 + 1).pow(2);

/// The most bytes of label candidates a view keeps for its layers together, by
/// [`Candidate::bytes`], what they hold beside them included (text, glyphs curved along a line,
/// places to try): about 20,000 point labels, each of 464 bytes and the places around it. A
/// layer whose candidates would go past it works them out in every frame. A frame copies no more
/// than this either, its layers bottom first.
pub(crate) const MAX_LABEL_BYTES: usize = 16 << 20;

/// Pictures kept, one per view drawn (a map and its draft, two maps on screen).
const KEPT: usize = 2;

/// What a frame is drawn from besides its layers.
#[derive(Clone, Debug)]
pub(crate) struct Frame {
    pub map: MapLook,
    pub view: View,
    pub style_env: Env,
    pub label_env: Env,
    /// [`rustgis_tiles::cache::generation`]: a tile arrived, on a map with tile layers or terrain
    /// (a tile for another map's view leaves this one's pictures).
    pub tiles: Option<u64>,
    /// [`rustgis_map::source::generation`]: a streamed read finished, on a map with a layer read
    /// from its source.
    pub sources: Option<u64>,
    /// The world outline's worldview, and how many shapes it had at hand, when it is drawn.
    pub worldview: Option<(crate::sample_data::View, usize)>,
    /// Whether the installed font families were indexed ([`crate::families::installed_now`]).
    pub fonts: bool,
    /// [`crate::fonts::font_generation`]: a downloaded font now draws some text.
    pub font_generation: u32,
    /// How many layers the map has, which labels take their priority from.
    pub layers: usize,
    /// What graphics text shows for `{date}` and `{time}` (the minute) and for `{layers}` (the
    /// layers not of graphics), on a map with graphics ([`rustgis_map::graphics::TokenValues`]).
    pub tokens: Option<(u64, usize)>,
    /// The layer each tile layer is drawn over ([`rustgis_map::Map::tile_slots`]), which follows
    /// the layers present as well as the tile layers' settings.
    pub tile_slots: Vec<Option<u32>>,
}

impl Frame {
    /// Whether `other` draws everything but its layers as this frame did.
    fn same(&self, other: &Frame) -> bool {
        let (a, b) = (&self.view, &other.view);
        a.width.to_bits() == b.width.to_bits()
            && a.height.to_bits() == b.height.to_bits()
            && a.pixels_per_point.to_bits() == b.pixels_per_point.to_bits()
            && a.camera == b.camera
            && a.sky == b.sky
            && self.style_env == other.style_env
            && self.label_env == other.label_env
            && self.tiles == other.tiles
            && self.sources == other.sources
            && self.worldview == other.worldview
            && self.fonts == other.fonts
            && self.font_generation == other.font_generation
            && self.layers == other.layers
            && self.tokens == other.tokens
            && self.tile_slots == other.tile_slots
            && self.map.same(&other.map)
    }
}

/// A frame as it stood before the layer at one place in the drawing order was drawn.
#[derive(Clone)]
pub(crate) struct Checkpoint {
    pub pixels: Pixmap,
    /// The marker sprites drawn so far: a layer above draws a marker from one, or straight once
    /// there are as many as a frame keeps, as it would in a frame drawn whole.
    pub sprites: Sprites,
    /// The shapeburst budget spent so far (`gradient_fill::FRAME_BURST_PX`).
    pub bursts: f32,
    /// When it was drawn: one of a map with tile layers is drawn again once a failed tile may be
    /// asked for again ([`rustgis_tiles::cache::RETRY`]), as the next frame drawn whole would.
    pub taken: web_time::Instant,
    pub candidates: Vec<Candidate>,
    pub fixed: Vec<PlacedLabel>,
    pub tiles: TileOutput,
}

/// A layer's picture alone, drawn opaque, and what the frame had gathered once the layer was drawn
/// and composited, before the tile layers over it.
#[derive(Clone)]
pub(crate) struct Alone {
    pub pixels: Pixmap,
    pub sprites: Sprites,
    pub bursts: f32,
    pub candidates: Vec<Candidate>,
    pub fixed: Vec<PlacedLabel>,
    pub tiles: TileOutput,
}

/// The looks of a frame's layers, bottom first in drawing order, by id and place in the map's list
/// (which labels take their priority from); `None` for one with no look.
pub(crate) type Looks = Vec<((u32, usize), Option<LayerLook>)>;

/// The label candidates each layer gave a frame, by the layer's id and place in the map's list,
/// before any were placed.
pub(crate) type LayerLabels = Vec<((u32, usize), Vec<Candidate>)>;

/// A kept picture and what it was drawn from.
struct Kept {
    frame: Frame,
    looks: Looks,
    /// The candidates of the layers that look as in `looks`.
    labels: LayerLabels,
    /// The place in the drawing order the checkpoint stands before.
    at: usize,
    checkpoint: Checkpoint,
    /// The layer at `at` alone.
    alone: Option<Alone>,
}

impl Kept {
    /// The pixels it holds: its pictures and their marker sprites, each sprite counted once
    /// though the checkpoint and the layer alone share it.
    fn pixels(&self) -> u64 {
        let size = |p: &Pixmap| u64::from(p.width()) * u64::from(p.height());
        let mut seen = std::collections::HashSet::new();
        let alone = self.alone.as_ref();
        let sprites = std::iter::once(&self.checkpoint.sprites)
            .chain(alone.map(|a| &a.sprites))
            .flat_map(|s| s.values().flatten())
            .filter(|p| seen.insert(std::sync::Arc::as_ptr(p)))
            .map(|p| size(p))
            .sum::<u64>();
        size(&self.checkpoint.pixels) + alone.map_or(0, |a| size(&a.pixels)) + sprites
    }
}

/// The kept pictures, shared by the threads that draw frames (the UI's and the one that draws
/// a view once it settles).
#[cfg(not(test))]
static PICTURES: std::sync::Mutex<Vec<Kept>> = std::sync::Mutex::new(Vec::new());

/// Runs `f` on the kept pictures.
#[cfg(not(test))]
fn with_pictures<R>(f: impl FnOnce(&mut Vec<Kept>) -> R) -> R {
    // A thread that panicked while holding them left nothing half done: each is replaced whole.
    f(&mut PICTURES.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
}

// Each test thread keeps its own, so tests running side by side don't take each other's.
#[cfg(test)]
thread_local! {
    static PICTURES: std::cell::RefCell<Vec<Kept>> = const { std::cell::RefCell::new(Vec::new()) };
    /// Where the last frame planned started drawing its layers.
    static STARTED: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    /// Whether the last frame planned had a layer alone to composite.
    static RECOMBINED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The layers whose label candidates frames took from a kept frame.
    static REUSED: std::cell::RefCell<Vec<u32>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
fn with_pictures<R>(f: impl FnOnce(&mut Vec<Kept>) -> R) -> R {
    PICTURES.with_borrow_mut(f)
}

/// How a frame draws, from [`plan`].
pub(crate) struct Plan {
    /// The place in the drawing order the frame starts drawing layers from: 0 draws everything,
    /// basemap included.
    pub start: usize,
    /// What it starts from when `start` is past 0.
    pub restored: Option<Checkpoint>,
    /// The place to keep the frame at, if not the one it was restored at.
    pub capture_at: Option<usize>,
    /// The layer at `start` alone, to composite instead of drawing it: it looks the same or only
    /// its transparency changed. Kept again when the layer isn't drawn alone this time.
    pub recombine: Option<Alone>,
    /// The place in the drawing order of the layer to keep alone, when it is drawn so.
    pub alone_at: Option<usize>,
    /// Whether a frame of the same view was kept: only then does it keep its label candidates,
    /// which a frame of another view, as while panning, has no use for.
    pub same_view: bool,
    /// The candidates of the layers that look the same or differ only in transparency, which
    /// labels don't take: each is the same as working them out again in this frame.
    labels: LayerLabels,
    frame: Frame,
    looks: Looks,
}

impl Plan {
    /// The label candidates the layer at place `place` in the map's list gave the frame kept, when
    /// they still stand. [`keep`] keeps them again for the frames after this one.
    pub(crate) fn take_labels(&mut self, id: u32, place: usize) -> Option<Vec<Candidate>> {
        let i = self.labels.iter().position(|(k, _)| *k == (id, place))?;
        #[cfg(test)]
        REUSED.with_borrow_mut(|r| r.push(id));
        Some(self.labels.swap_remove(i).1)
    }
}

/// The candidates in `labels` of the layers that look in `now` as in `then`, or differ only in
/// their transparency.
fn standing_labels(labels: LayerLabels, then: &Looks, now: &Looks) -> LayerLabels {
    fn look(looks: &Looks, key: (u32, usize)) -> Option<&LayerLook> {
        looks.iter().find(|(k, _)| *k == key).and_then(|(_, l)| l.as_ref())
    }
    labels
        .into_iter()
        .filter(|(key, _)| match (look(then, *key), look(now, *key)) {
            (Some(a), Some(b)) => matches!(a.change(b), Change::Same | Change::Transparency),
            _ => false,
        })
        .collect()
}

/// The first place in the drawing order where `now` doesn't look as `then` did.
fn first_change(then: &Looks, now: &Looks) -> usize {
    then.iter()
        .zip(now)
        .position(|((a, la), (b, lb))| {
            a != b
                || match (la, lb) {
                    (Some(la), Some(lb)) => la.change(lb) != Change::Same,
                    _ => true,
                }
        })
        .unwrap_or(then.len().min(now.len()))
}

/// Whether a frame of `w` × `h` pixels can keep pictures: [`plan`] refuses one past the budget,
/// so a frame checks first rather than take the looks of its layers for nothing.
pub(crate) fn fits(w: u32, h: u32) -> bool {
    (u64::from(w) * u64::from(h)).saturating_mul(KEPT as u64) <= MAX_PIXELS
}

/// How the layer at place `at` in the drawing order differs between `then` and `now`.
fn change_at(then: &Looks, now: &Looks, at: usize) -> Change {
    match (then.get(at), now.get(at)) {
        (Some((a, Some(la))), Some((b, Some(lb)))) if a == b => la.change(lb),
        _ => Change::Drawn,
    }
}

/// The place at which a frame keeping its picture at `at` keeps a layer alone: one with a look,
/// when a view's checkpoint and layer fit within [`MAX_PIXELS`] together.
fn alone_at(looks: &Looks, at: usize, pixels: u64) -> Option<usize> {
    (pixels.saturating_mul(2) <= MAX_PIXELS && looks.get(at).is_some_and(|(_, l)| l.is_some())).then_some(at)
}

/// How to draw `frame` with layers `looks` of `pixels` pixels: from a kept picture when one still
/// shows its bottom layers, and where to keep this one. The kept picture is taken out meanwhile;
/// [`keep`] puts the frame's back.
pub(crate) fn plan(frame: Frame, looks: Looks, pixels: u64) -> Plan {
    let plan = plan_with(frame, looks, pixels);
    #[cfg(test)]
    STARTED.set(Some(plan.start));
    #[cfg(test)]
    RECOMBINED.set(plan.recombine.is_some());
    #[cfg(test)]
    REUSED.take();
    plan
}

/// [`plan`].
fn plan_with(frame: Frame, looks: Looks, pixels: u64) -> Plan {
    if pixels.saturating_mul(KEPT as u64) > MAX_PIXELS {
        return Plan {
            start: 0,
            restored: None,
            capture_at: None,
            recombine: None,
            alone_at: None,
            same_view: false,
            labels: Vec::new(),
            frame,
            looks,
        };
    }
    // A picture over tile layers lasts until a tile that failed in it may be asked for again: a
    // server that gave no time to come back is asked by the next draw after a minute.
    let fresh = |k: &Kept| k.frame.tiles.is_none() || k.checkpoint.taken.elapsed() < rustgis_tiles::cache::RETRY;
    let found = with_pictures(|kept| {
        kept.retain(fresh);
        kept.iter().position(|k| k.frame.same(&frame)).map(|i| kept.remove(i))
    });
    let Some(k) = found else {
        // Nothing to start from: keep what lies under the first layer (the basemap, the tile
        // layers below every layer), or the frame under its first changed layer next time.
        let alone_at = alone_at(&looks, 0, pixels);
        return Plan {
            start: 0,
            restored: None,
            capture_at: Some(0),
            recombine: None,
            alone_at,
            same_view: false,
            labels: Vec::new(),
            frame,
            looks,
        };
    };
    let changed = first_change(&k.looks, &looks);
    let labels = standing_labels(k.labels, &k.looks, &looks);
    if k.at <= changed {
        let capture_at = (changed > k.at && changed < looks.len()).then_some(changed);
        let recombine = k.alone.filter(|_| matches!(change_at(&k.looks, &looks, k.at), Change::Same | Change::Transparency));
        let alone_at = alone_at(&looks, capture_at.unwrap_or(k.at), pixels);
        Plan { start: k.at, restored: Some(k.checkpoint), capture_at, recombine, alone_at, same_view: true, labels, frame, looks }
    } else {
        let alone_at = alone_at(&looks, changed, pixels);
        Plan { start: 0, restored: None, capture_at: Some(changed), recombine: None, alone_at, same_view: true, labels, frame, looks }
    }
}

/// Keeps the picture a frame drawn by `plan` ends with: `captured` when it took one, else the one
/// it started from, with the layer over it alone: `alone` when the frame drew it so, else the one
/// it was composited from. `drawn` holds the label candidates of the layers the frame worked them
/// out for or took ([`Plan::take_labels`]), the others' are kept as they were. Called only for a
/// frame drawn to the end.
pub(crate) fn keep(plan: Plan, captured: Option<(usize, Checkpoint)>, alone: Option<(usize, Alone)>, drawn: LayerLabels) {
    let Plan { start, restored, recombine, labels: standing, frame, looks, .. } = plan;
    let Some((at, checkpoint)) = captured.or(restored.map(|c| (start, c))) else { return };
    let alone = match alone {
        Some((a, alone)) if a == at => Some(alone),
        _ => recombine.filter(|_| at == start),
    };
    // Bottom first in drawing order, as many as fit.
    let mut given: LayerLabels = drawn.into_iter().chain(standing).collect();
    let mut labels: LayerLabels = Vec::new();
    let mut bytes = 0;
    for (key, _) in &looks {
        if let Some((i, b)) = given.iter().position(|(k, _)| k == key).and_then(|i| {
            let b = Candidate::bytes(&given.get(i)?.1);
            (bytes + b <= MAX_LABEL_BYTES).then_some((i, b))
        }) {
            bytes += b;
            labels.push(given.swap_remove(i));
        }
    }
    with_pictures(|kept| {
        kept.retain(|k| !k.frame.same(&frame));
        if kept.len() >= KEPT {
            kept.remove(0);
        }
        kept.push(Kept { frame, looks, labels, at, checkpoint, alone });
        // Within the budget: the older views' layers alone go first, then the older views, then
        // this view's layer alone.
        while kept.iter().map(Kept::pixels).sum::<u64>() > MAX_PIXELS + KEPT as u64 * SPRITE_PIXELS {
            let older = kept.len().saturating_sub(1);
            if let Some(k) = kept.iter_mut().take(older).find(|k| k.alone.is_some()) {
                k.alone = None;
            } else if kept.len() > 1 {
                kept.remove(0);
            } else if let Some(k) = kept.first_mut().filter(|k| k.alone.is_some()) {
                k.alone = None;
            } else {
                // A view whose picture and sprites alone are past the budget keeps nothing.
                kept.clear();
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use rustgis_data::{Coord, Dataset, Field, FieldType, Geometry, GeometryType, Value};
    use rustgis_map::{Basemap, Camera, Map, Renderer, Rgba};

    use super::*;
    use crate::render;

    /// A map of a grid of labelled squares under a layer of points, and a view of it.
    fn map() -> (Map, View, u32, u32) {
        // The installed fonts indexed now: another test indexing them partway would change every
        // frame's key (`Frame::fonts`), and the next frame would draw whole.
        crate::families::installed();
        let mut squares = Dataset::new("squares", GeometryType::Polygon, vec![Field::new("name", FieldType::Text)]);
        for i in 0..40 {
            let (x, y) = (f64::from(i % 8) * 0.01, f64::from(i / 8) * 0.01);
            let ring = vec![
                Coord::new(x, y),
                Coord::new(x + 0.008, y),
                Coord::new(x + 0.008, y + 0.008),
                Coord::new(x, y + 0.008),
                Coord::new(x, y),
            ];
            let pos = rustgis_geom::lonlat_to_merc;
            squares.push(Some(Geometry::Polygon(vec![vec![ring.into_iter().map(pos).collect()]])), vec![Value::Text(format!("S{i}"))]);
        }
        let mut points = Dataset::new("points", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
        for i in 0..30 {
            let c = rustgis_geom::lonlat_to_merc(Coord::new(f64::from(i % 6) * 0.013, f64::from(i / 6) * 0.009));
            points.push(Some(Geometry::Point(c)), vec![Value::Text(format!("P{i}"))]);
        }
        let mut map = Map::new("Map");
        map.basemap = Basemap::Blank;
        map.graticule = false;
        let below = map.add_dataset(squares);
        let above = map.add_dataset(points);
        if let Some(l) = map.layer_mut(below) {
            l.labels.enabled = true;
            l.labels.default.field = "name".into();
        }
        let camera = Camera { center: rustgis_geom::lonlat_to_merc(Coord::new(0.04, 0.025)), res: 12.0, bearing: 0.0, pitch: 0.0 };
        (map, View { width: 200.0, height: 160.0, pixels_per_point: 1.0, camera, sky: false }, below, above)
    }

    /// Draws `map` twice: as it comes after the frames before it, and again from nothing. They
    /// must be the same frame; gives where the first started drawing its layers.
    fn draw(map: &mut Map, view: &View) -> Option<usize> {
        let kept = render(map, view).expect("drawn");
        let (started, recombined, reused) = (STARTED.get(), RECOMBINED.get(), REUSED.take());
        let pictures = PICTURES.take();
        let fresh = render(map, view).expect("drawn");
        PICTURES.set(pictures);
        RECOMBINED.set(recombined);
        REUSED.set(reused);
        assert!(kept.pixmap.data() == fresh.pixmap.data(), "a frame from a kept picture differs from one drawn whole");
        let texts = |r: &crate::Rendered| r.labels.iter().map(|l| (l.text.clone(), l.x.to_bits(), l.y.to_bits())).collect::<Vec<_>>();
        assert_eq!(texts(&kept), texts(&fresh), "its labels differ");
        started
    }

    #[test]
    fn a_frame_draws_from_its_first_changed_layer() {
        let (mut map, view, below, above) = map();
        assert_eq!(draw(&mut map, &view), Some(0), "the first frame draws everything");
        // Restyling the layer on top draws only it, the labels of the one below still placed.
        let restyle = |map: &mut Map, id: u32, rgb: (u8, u8, u8)| {
            if let Some(l) = map.layer_mut(id) {
                l.renderer = Renderer::Single(rustgis_map::Symbol { fill: Rgba::rgb(rgb.0, rgb.1, rgb.2), ..l.symbol(0) });
            }
        };
        // The first change after a new view starts from the picture under every layer, and keeps
        // one under the changed layer for the next.
        restyle(&mut map, above, (200, 30, 30));
        assert_eq!(draw(&mut map, &view), Some(0));
        restyle(&mut map, above, (30, 200, 30));
        assert_eq!(draw(&mut map, &view), Some(1), "each step of a drag draws from the picture under the layer");
        restyle(&mut map, above, (30, 30, 200));
        assert_eq!(draw(&mut map, &view), Some(1));
        // Nothing changed: still from the kept picture.
        assert_eq!(draw(&mut map, &view), Some(1));
        // The layer below changed: everything again.
        restyle(&mut map, below, (10, 10, 120));
        assert_eq!(draw(&mut map, &view), Some(0));
        if let Some(l) = map.layer_mut(below) {
            l.dataset.features.truncate(20);
        }
        assert_eq!(draw(&mut map, &view), Some(0), "an edit to the rows draws the layer again");
        // Moving the view draws everything.
        let mut moved = view;
        moved.camera.center.x += 50.0;
        assert_eq!(draw(&mut map, &moved), Some(0));
    }

    #[test]
    fn a_layer_drawn_again_takes_its_label_candidates_while_it_looks_the_same() {
        let (mut map, view, below, above) = map();
        if let Some(l) = map.layer_mut(above) {
            l.labels.enabled = true;
            l.labels.default.field = "name".into();
        }
        let restyle = |map: &mut Map, id: u32, rgb: (u8, u8, u8)| {
            if let Some(l) = map.layer_mut(id) {
                l.renderer = Renderer::Single(rustgis_map::Symbol { fill: Rgba::rgb(rgb.0, rgb.1, rgb.2), ..l.symbol(0) });
            }
        };
        let reused = |map: &mut Map| {
            draw(map, &view);
            REUSED.take()
        };
        assert_eq!(reused(&mut map), Vec::<u32>::new(), "the first frame works them all out");
        // A view's first frame keeps none (another view's, as while panning, would have no use for
        // them); its second keeps them.
        assert!(PICTURES.with_borrow(|k| k.iter().all(|k| k.labels.is_empty())));
        assert_eq!(reused(&mut map), Vec::<u32>::new());
        assert!(PICTURES.with_borrow(|k| k.iter().any(|k| !k.labels.is_empty())));
        // The layer below restyled: the one above is drawn again over it, with its candidates.
        restyle(&mut map, below, (10, 10, 120));
        assert_eq!(reused(&mut map), vec![above]);
        // The layer above restyled: drawn from the picture under it, its candidates worked out.
        restyle(&mut map, above, (200, 30, 30));
        assert_eq!(reused(&mut map), vec![below], "the frame was drawn whole, the layer below again");
        restyle(&mut map, above, (30, 200, 30));
        assert_eq!(reused(&mut map), Vec::<u32>::new());
        // Those of the layer below, not drawn in that frame, were kept from the one before.
        restyle(&mut map, below, (120, 10, 10));
        assert_eq!(reused(&mut map), vec![above]);
        // Only its transparency changed: still its candidates, which take no transparency.
        if let Some(l) = map.layer_mut(above) {
            l.transparency = 40.0;
        }
        restyle(&mut map, below, (10, 120, 10));
        assert_eq!(reused(&mut map), vec![above]);
        // Labelled by another field, or a moved view: worked out again.
        if let Some(l) = map.layer_mut(above) {
            l.labels.default.field = "missing".into();
        }
        restyle(&mut map, below, (10, 10, 10));
        assert_eq!(reused(&mut map), Vec::<u32>::new());
        restyle(&mut map, below, (20, 20, 20));
        assert_eq!(reused(&mut map), vec![above]);
        let mut moved = view;
        moved.camera.center.x += 50.0;
        draw(&mut map, &moved);
        assert_eq!(REUSED.take(), Vec::<u32>::new(), "another view's");
    }

    #[test]
    fn a_see_through_layer_is_composited_again_at_its_new_transparency() {
        let (mut map, view, _, above) = map();
        let set = |map: &mut Map, t: f32| map.layer_mut(above).map(|l| l.transparency = t);
        set(&mut map, 20.0);
        assert_eq!(draw(&mut map, &view), Some(0));
        // The first change keeps the frame under the layer, and the layer alone.
        set(&mut map, 30.0);
        assert_eq!(draw(&mut map, &view), Some(0));
        assert!(!RECOMBINED.get());
        // Each further step of the drag composites that picture: the same frame as one drawn whole.
        for t in [40.0, 75.0, 5.0] {
            set(&mut map, t);
            assert_eq!(draw(&mut map, &view), Some(1));
            assert!(RECOMBINED.get(), "{t}");
        }
        // Nothing changed: composited again.
        assert_eq!(draw(&mut map, &view), Some(1));
        assert!(RECOMBINED.get());
        // Opaque, it is drawn straight onto the map, which composites nothing.
        set(&mut map, 0.0);
        draw(&mut map, &view);
        set(&mut map, 50.0);
        draw(&mut map, &view);
        // A colour drawn differently: the layer is drawn again, and kept alone again.
        if let Some(l) = map.layer_mut(above) {
            l.renderer = Renderer::Single(rustgis_map::Symbol { fill: Rgba::rgb(10, 200, 90), ..l.symbol(0) });
        }
        assert_eq!(draw(&mut map, &view), Some(1));
        assert!(!RECOMBINED.get());
        set(&mut map, 60.0);
        assert_eq!(draw(&mut map, &view), Some(1));
        assert!(RECOMBINED.get());
        // A selection is drawn over the composite: the layer isn't kept alone while it shows one.
        if let Some(l) = map.layer_mut(above) {
            l.selection = vec![0, 3];
        }
        draw(&mut map, &view);
        set(&mut map, 70.0);
        draw(&mut map, &view);
        assert!(!RECOMBINED.get(), "a layer showing a selection is drawn");
    }

    #[test]
    fn a_layer_moved_in_the_list_draws_whole_though_its_drawing_order_holds() {
        let (mut map, view, below, above) = map();
        // Labels on both, which take their priority from the layers' places in the list; the
        // drawing order is held by the layers' depth priorities whatever the list says.
        for (id, priority) in [(below, 0), (above, 1)] {
            if let Some(l) = map.layer_mut(id) {
                l.labels.enabled = true;
                l.labels.default.field = "name".into();
                l.priority = priority;
            }
        }
        draw(&mut map, &view);
        // Changes to the layer on top keep the picture under it.
        for size in [9.0, 11.0] {
            if let Some(l) = map.layer_mut(above) {
                l.renderer = Renderer::Single(rustgis_map::Symbol { size, ..l.symbol(0) });
            }
            draw(&mut map, &view);
        }
        assert_eq!(draw(&mut map, &view), Some(1));
        map.layers.reverse();
        assert_eq!(draw(&mut map, &view), Some(0), "the labels below place differently");
        // A layer added to the end of the list, drawn on top by its depth priority, leaves the
        // others at their places, but the label priorities of all of them count the layers.
        for size in [9.0, 11.0] {
            if let Some(l) = map.layer_mut(above) {
                l.renderer = Renderer::Single(rustgis_map::Symbol { size, ..l.symbol(0) });
            }
            draw(&mut map, &view);
        }
        assert_eq!(draw(&mut map, &view), Some(1));
        let Some(mut added) = map.layer(above).cloned() else { panic!("the layer above") };
        added.id = 99;
        added.priority = 2;
        map.layers.push(added);
        assert_eq!(draw(&mut map, &view), Some(0), "the labels below rank differently");
    }

    #[test]
    fn a_view_of_3840_by_2160_keeps_its_layer_alone_beside_the_marker_sprites() {
        let (mut map, mut view, _, above) = map();
        (view.width, view.height) = (3840.0, 2160.0);
        // The layer dragged is of point markers, drawn from sprites.
        for t in [20.0, 30.0, 40.0] {
            if let Some(l) = map.layer_mut(above) {
                l.transparency = t;
            }
            draw(&mut map, &view);
        }
        assert!(RECOMBINED.get(), "the layer alone was kept");
    }

    #[test]
    fn a_layer_with_no_look_draws_in_every_frame() {
        let (mut map, view, below, _) = map();
        if let Some(l) = map.layer_mut(below) {
            l.source = Some(Default::default());
        }
        draw(&mut map, &view);
        assert_eq!(draw(&mut map, &view), Some(0));
    }
}
