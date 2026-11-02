//! A polygon's stacked fills (`Layer::fill_layers`, #882 piece 5), drawn bottom first inside it
//! in place of its symbol's fill, its outline over them: a fill in a colour, hatch lines, or
//! markers on a grid. A pattern is a small tile repeated by tiny-skia's pattern shader, its
//! lattice anchored to the map's origin (worked out in f64 and reduced to within a tile, so it
//! keeps its place on the map at every zoom and neighbouring polygons share one), turned for a
//! hatch's angle.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use crate::Canvas;
use rustgis_geom::Coord;
use rustgis_map::fills::{FillLayer, FillPattern};
use rustgis_map::{Marker, Rgba, Symbol};
use tiny_skia::{FillRule, Paint, Pixmap, Transform};

use crate::{Ctx, paint};

/// The most pattern tiles kept between frames, and the most bytes they take: past either, the
/// oldest go first (a map with many hatched classes, each colour a tile, keeps most of them
/// rather than drawing them again each frame).
const MAX_TILES: usize = 1024;
const MAX_TILE_BYTES: usize = 64 << 20;
/// The longest side of a tile in pixels: a coarser pattern is drawn at a lower resolution and
/// scaled up, so a tile never takes more than a megabyte.
const MAX_TILE_PX: f32 = 512.0;

/// The pattern tiles drawn so far, by what they draw at what size, the order they were made in,
/// and the bytes they take.
#[derive(Default)]
struct Tiles {
    by_key: HashMap<Key, Option<Arc<Pixmap>>>,
    order: VecDeque<Key>,
    bytes: usize,
}

/// The most provisional tiles kept: those of character markers drawn while the installed
/// families are still being indexed.
const MAX_PROVISIONAL: usize = 64;

thread_local! {
    static TILES: RefCell<Tiles> = RefCell::new(Tiles::default());
    /// Provisional tiles (in the label font, for now), kept apart and never used once their
    /// marker's family is settled, so a layer of them isn't drawn again for each polygon meanwhile.
    static PROVISIONAL: RefCell<HashMap<Key, Option<Arc<Pixmap>>>> = RefCell::new(HashMap::new());
}

impl Ctx<'_> {
    /// Draws `polys` with its stacked `fills`, bottom first (a fill without a colour of its own
    /// takes the symbol's), then the symbol's outline over them.
    pub(crate) fn filled(&self, pm: &mut Canvas, polys: &[Vec<Vec<Coord>>], s: &Symbol, fills: &[FillLayer]) {
        let Some(path) = self.polygon_path(polys) else { return };
        // The map's origin in device pixels, in f64: the patterns' lattice point.
        let origin = match self.globe {
            Some(_) => (0.0, 0.0),
            None => {
                let (x, y) = self.view.to_screen(Coord::new(0.0, 0.0));
                // (Placed as the polygons are: by the view's pixels a point, not the layer's
                // reference-scaled one, so a pattern keeps its place on the map as it pans.)
                let ppp = self.view.pixels_per_point;
                (x * ppp, y * ppp)
            }
        };
        for f in fills.iter().filter(|f| f.draws(s.fill)) {
            let color = f.color.unwrap_or(s.fill);
            match f.pattern {
                FillPattern::Solid => pm.fill_path(&path, &paint(color, 1.0), FillRule::EvenOdd, Transform::identity(), None),
                FillPattern::Gradient { .. } | FillPattern::Shapeburst { .. } => {
                    // Each part shaded on its own, in its own box, as QGIS shades a multi-part
                    // feature (France's gradient in France, not across to French Guiana).
                    let clip = self.clip_box();
                    let clip = (self.globe.is_none())
                        .then(|| tiny_skia::Rect::from_ltrb(clip.min_x as f32, clip.min_y as f32, clip.max_x as f32, clip.max_y as f32))
                        .flatten();
                    for part in polys {
                        let Some(path) = self.polygon_path(std::slice::from_ref(part)) else { continue };
                        // Its outer ring alone, for a shapeburst that leaves its holes' edges out.
                        let exterior = || part.first().and_then(|r| self.polygon_path(&[vec![r.clone()]]));
                        let whole = part.first().and_then(|r| self.device_box(r));
                        let centroid = part.first().and_then(|r| self.device_centroid(r));
                        let whole_depth = |outer_only: bool| {
                            let rings: Vec<Vec<(f64, f64)>> =
                                part.iter().take(if outer_only { 1 } else { usize::MAX }).filter_map(|r| self.device_ring(r)).collect();
                            crate::gradient_fill::whole_depth(&rings)
                        };
                        let part = crate::gradient_fill::Part {
                            path: &path,
                            whole,
                            centroid,
                            clip,
                            whole_depth: &whole_depth,
                            budget: &self.bursts,
                        };
                        crate::gradient_fill::draw(pm, &part, exterior, &f.pattern, color, self.ppp);
                    }
                }
                pattern => {
                    let Some((tile, t)) = pattern_tile(pattern, color, self.ppp, origin) else { continue };
                    let shader = tiny_skia::Pattern::new(
                        tile.as_ref().as_ref(),
                        tiny_skia::SpreadMode::Repeat,
                        tiny_skia::FilterQuality::Bilinear,
                        1.0,
                        t,
                    );
                    let fill = Paint { shader, anti_alias: crate::antialiased(), ..Paint::default() };
                    pm.fill_path(&path, &fill, FillRule::EvenOdd, Transform::identity(), None);
                }
            }
        }
        if s.stroke.a() > 0 {
            self.outline(pm, &path, &paint(s.stroke, 1.0), s.stroke_width);
        }
    }
}

impl Ctx<'_> {
    /// `ring`'s box in device pixels before any clipping, held within ±10⁷ (a gradient's box when
    /// zoomed far into its polygon); `None` on the globe (its rings are drawn as seen) or for a
    /// ring of no finite point.
    fn device_box(&self, ring: &[Coord]) -> Option<tiny_skia::Rect> {
        if self.globe.is_some() {
            return None;
        }
        let ppp = self.view.pixels_per_point;
        let mut b = rustgis_geom::Extent::EMPTY;
        for c in ring {
            let (x, y) = self.view.to_screen(*c);
            let (x, y) = (x * ppp, y * ppp);
            if x.is_finite() && y.is_finite() {
                b.expand(Coord::new(x.clamp(-1e7, 1e7), y.clamp(-1e7, 1e7)));
            }
        }
        if b.is_empty() {
            return None;
        }
        tiny_skia::Rect::from_ltrb(b.min_x as f32, b.min_y as f32, b.max_x as f32, b.max_y as f32)
    }

    /// `ring` in device pixels before any clipping, in f64 (its points of no finite place left
    /// out); `None` on the globe or for a ring of under three points.
    fn device_ring(&self, ring: &[Coord]) -> Option<Vec<(f64, f64)>> {
        if self.globe.is_some() {
            return None;
        }
        let ppp = self.view.pixels_per_point;
        let pts: Vec<(f64, f64)> = ring
            .iter()
            .map(|c| self.view.to_screen(*c))
            .map(|(x, y)| (x * ppp, y * ppp))
            .filter(|(x, y)| x.is_finite() && y.is_finite())
            .collect();
        (pts.len() >= 3).then_some(pts)
    }

    /// The centroid of the area `ring` (an outer ring) encloses, in device pixels before any
    /// clipping, held within ±10⁷; `None` on the globe or for no area.
    fn device_centroid(&self, ring: &[Coord]) -> Option<tiny_skia::Point> {
        let pts = self.device_ring(ring)?;
        // Taken about its first point, so far-off rings keep their precision.
        let (ox, oy) = *pts.first()?;
        let (mut a, mut cx, mut cy) = (0.0f64, 0.0f64, 0.0f64);
        for (p, q) in pts.iter().zip(pts.iter().cycle().skip(1)) {
            let (px, py, qx, qy) = (p.0 - ox, p.1 - oy, q.0 - ox, q.1 - oy);
            let cross = px * qy - qx * py;
            a += cross;
            cx += (px + qx) * cross;
            cy += (py + qy) * cross;
        }
        // (Each sum is three times the area times the centroid, the area twice over.)
        if a.abs() <= 1e-9 {
            return None;
        }
        let (x, y) = (ox + cx / (3.0 * a), oy + cy / (3.0 * a));
        (x.is_finite() && y.is_finite()).then(|| tiny_skia::Point::from_xy(x.clamp(-1e7, 1e7) as f32, y.clamp(-1e7, 1e7) as f32))
    }
}

/// `v` modulo `m` (positive), as f32: a lattice offset reduced to within one period.
fn reduce(v: f64, m: f64) -> f32 {
    if m > 0.0 && v.is_finite() { v.rem_euclid(m) as f32 } else { 0.0 }
}

/// `pattern`'s tile in `color` at `ppp` pixels a point, and the transform that repeats it with
/// its lattice through `origin` (device pixels); `None` for a pattern of no size.
fn pattern_tile(pattern: FillPattern, color: Rgba, ppp: f32, origin: (f64, f64)) -> Option<(Arc<Pixmap>, Transform)> {
    match pattern {
        FillPattern::Solid | FillPattern::Gradient { .. } | FillPattern::Shapeburst { .. } => None,
        FillPattern::Hatch { angle, spacing, width, offset } => {
            // A tile a few pixels across and a period high, its line along the middle, turned to
            // the angle (anticlockwise from east; the screen's y runs down).
            let period = spacing * ppp;
            let k = (MAX_TILE_PX / period).min(1.0);
            let h = (period * k).ceil().max(1.0);
            let tile = tile(
                &key(&pattern, color, ppp),
                || true,
                4.0,
                h,
                |pm| {
                    let w = (width * ppp * k).max(0.0);
                    let mid = h / 2.0;
                    if let Some(r) = tiny_skia::Rect::from_ltrb(-1.0, mid - w / 2.0, 5.0, mid + w / 2.0) {
                        pm.fill_rect(r, &paint(color, 1.0), Transform::identity(), None);
                    }
                },
            )?;
            let rad = -angle.to_radians();
            // The lattice across the lines (their normal) through the origin, shifted by the offset.
            let (nx, ny) = (-f64::from(rad.sin()), f64::from(rad.cos()));
            let across = reduce(origin.0 * nx + origin.1 * ny + f64::from(offset * ppp), f64::from(period));
            let t = Transform::from_scale(1.0, period / h).post_translate(0.0, across - period / 2.0).post_rotate(rad.to_degrees());
            Some((tile, t))
        }
        FillPattern::Dots { marker, size, outline, outline_width, spacing, displacement, offset } => {
            // Two cells each way, for the odd rows' and columns' displacement.
            let [dx, dy] = spacing.map(|s| s * ppp);
            let [sx, sy] = displacement.map(|d| d * ppp);
            let (tw, th) = (2.0 * dx, 2.0 * dy);
            let k = (MAX_TILE_PX / tw.max(th)).min(1.0);
            let symbol =
                Symbol { fill: color, stroke: outline.unwrap_or(Rgba::new(0, 0, 0, 0)), stroke_width: outline_width, size, marker };
            // A character marker drawn while the installed families are still being indexed is
            // in the label font for now (as `glyphs::shape` keeps none then): not kept among the
            // tiles. (Asked only when the tile isn't kept yet.)
            let settled = || match marker {
                Marker::Char(g) => crate::text::marker_font(g.font.as_str()).1,
                _ => true,
            };
            let tile = tile(&key(&pattern, color, ppp), settled, (tw * k).ceil().max(1.0), (th * k).ceil().max(1.0), |pm| {
                for (col, row) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    // A marker in each cell's middle, odd rows shifted across, odd columns down;
                    // drawn again a tile away each way where it crosses the tile's edges.
                    let x = (col as f32 + 0.5) * dx + if row == 1 { sx } else { 0.0 };
                    let y = (row as f32 + 0.5) * dy + if col == 1 { sy } else { 0.0 };
                    // Copies two tiles out each way: a marker up to about three times its spacing
                    // still reaches across the tile's edge whole.
                    for (ox, oy) in (-2..=2).flat_map(|i| (-2..=2).map(move |j| (i as f32 * tw, j as f32 * th))) {
                        crate::draw_marker(pm, (x + ox) * k, (y + oy) * k, &symbol, 1.0, None, ppp * k);
                    }
                }
            })?;
            let (ax, ay) = (
                reduce(origin.0 + f64::from(offset[0] * ppp), f64::from(tw)),
                reduce(origin.1 + f64::from(offset[1] * ppp), f64::from(th)),
            );
            Some((tile, Transform::from_scale(tw / (tw * k).ceil().max(1.0), th / (th * k).ceil().max(1.0)).post_translate(ax, ay)))
        }
    }
}

/// The colour a polygon's stacked `fills` show as together when it is smaller than a pixel (a
/// fill without a colour of its own taking the feature's `feature`): each over those under it, a
/// pattern by the share of the area its lines or markers cover (a marker's outline too), a solid
/// fill by all of it.
pub(crate) fn tint(fills: &[FillLayer], feature: Rgba) -> Rgba {
    // Premultiplied, 0 to 1.
    let mut out = [0.0f32; 4];
    let mut over = |c: Rgba, share: f32| {
        let a = f32::from(c.a()) / 255.0 * if share.is_finite() { share.clamp(0.0, 1.0) } else { 1.0 };
        for (o, v) in out.iter_mut().zip([c.r(), c.g(), c.b()]) {
            *o = f32::from(v) / 255.0 * a + *o * (1.0 - a);
        }
        out[3] = a + out[3] * (1.0 - a);
    };
    let per = |ink: f32, cell: f32| if cell > 0.0 { ink / cell } else { 1.0 };
    for f in fills.iter().filter(|f| f.draws(feature)) {
        let c = f.color.unwrap_or(feature);
        match f.pattern {
            FillPattern::Solid => over(c, 1.0),
            FillPattern::Gradient { color2, ref stops, discrete, .. } | FillPattern::Shapeburst { color2, ref stops, discrete, .. } => {
                over(crate::gradient_fill::average(c, color2, stops, discrete), 1.0)
            }
            FillPattern::Hatch { spacing, width, .. } => over(c, per(width, spacing)),
            FillPattern::Dots { marker, size, outline, outline_width, spacing, .. } => {
                // As drawn: at least a point across; a square covers its whole box, the other
                // shapes about a circle's share of it.
                let d = size.max(1.0);
                let (area, edge) = if marker == Marker::Square {
                    (d * d, 4.0 * d)
                } else {
                    (std::f32::consts::FRAC_PI_4 * d * d, std::f32::consts::PI * d)
                };
                let cell = spacing[0] * spacing[1];
                over(c, per(area, cell));
                if let Some(o) = outline.filter(|_| outline_width > 0.0) {
                    over(o, per(edge * outline_width, cell));
                }
            }
        }
    }
    if out[3] <= 0.0 {
        return Rgba::new(0, 0, 0, 0);
    }
    let byte = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    Rgba::new(byte(out[0] / out[3]), byte(out[1] / out[3]), byte(out[2] / out[3]), byte(out[3]))
}

/// What a tile draws at what size (and whether antialiased): its colour, size and kind, and the
/// pattern's numbers by their bits; a dot's marker.
#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    bits: [u32; 11],
    marker: Option<Marker>,
}

/// The cache key of a tile (made for each polygon drawn: no allocation).
fn key(pattern: &FillPattern, color: Rgba, ppp: f32) -> Key {
    let mut bits = [0; 11];
    bits[..3].copy_from_slice(&[u32::from_le_bytes(color.0), ppp.to_bits(), u32::from(crate::antialiased())]);
    let marker = match *pattern {
        FillPattern::Solid | FillPattern::Gradient { .. } | FillPattern::Shapeburst { .. } => None,
        FillPattern::Hatch { spacing, width, .. } => {
            bits[3..6].copy_from_slice(&[1, spacing.to_bits(), width.to_bits()]);
            None
        }
        FillPattern::Dots { marker, size, outline, outline_width, spacing, displacement, .. } => {
            bits[3..7].copy_from_slice(&[2, size.to_bits(), outline_width.to_bits(), outline.map_or(0, |o| u32::from_le_bytes(o.0))]);
            bits[7..11].copy_from_slice(&[
                spacing[0].to_bits(),
                spacing[1].to_bits(),
                displacement[0].to_bits(),
                displacement[1].to_bits(),
            ]);
            Some(marker)
        }
    };
    Key { bits, marker }
}

/// The tile `key` names, drawn by `draw` on a `w` × `h` pixel picture the first time; `None` for
/// one that can't be made. Not kept yet, `settled` says whether it may be kept among the tiles:
/// a character marker drawn while the installed families are being indexed (in the label font
/// for now) is kept among the provisional ones instead, which all go once one of a named family
/// is settled (the index is done, for every family; the label font's, unnamed, is settled from
/// the start and says nothing about it).
fn tile(key: &Key, settled: impl FnOnce() -> bool, w: f32, h: f32, draw: impl FnOnce(&mut Canvas)) -> Option<Arc<Pixmap>> {
    if let Some(t) = TILES.with(|t| t.borrow().by_key.get(key).cloned()) {
        return t;
    }
    let keep = settled();
    if keep {
        if key.marker.is_some_and(|m| matches!(m, Marker::Char(g) if !g.font.as_str().trim().is_empty())) {
            PROVISIONAL.with(|p| p.borrow_mut().clear());
        }
    } else if let Some(t) = PROVISIONAL.with(|p| p.borrow().get(key).cloned()) {
        return t;
    }
    let made = (w.is_finite() && h.is_finite() && w <= MAX_TILE_PX + 1.0 && h <= MAX_TILE_PX + 1.0)
        .then(|| Canvas::new(w.max(1.0) as u32, h.max(1.0) as u32))
        .flatten()
        .map(|mut pm| {
            draw(&mut pm);
            Arc::new(pm.into_pixmap())
        });
    if !keep {
        PROVISIONAL.with(|p| {
            let mut p = p.borrow_mut();
            if p.len() >= MAX_PROVISIONAL {
                p.clear();
            }
            p.insert(key.clone(), made.clone());
        });
        return made;
    }
    let size = made.as_ref().map_or(0, |pm| pm.data().len());
    TILES.with(|t| {
        let mut t = t.borrow_mut();
        // The oldest make room, one at a time, so the rest stay.
        while !t.order.is_empty() && (t.order.len() >= MAX_TILES || t.bytes + size > MAX_TILE_BYTES) {
            if let Some(old) = t.order.pop_front()
                && let Some(Some(pm)) = t.by_key.remove(&old)
            {
                t.bytes = t.bytes.saturating_sub(pm.data().len());
            }
        }
        t.bytes += size;
        t.order.push_back(key.clone());
        t.by_key.insert(key.clone(), made.clone());
    });
    made
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_tile_cache_lets_its_oldest_go_and_keeps_the_rest() {
        let hatch = |i: u32| key(&FillPattern::Hatch { angle: 0.0, spacing: 6.0, width: 1.5, offset: 0.0 }, Rgba(i.to_le_bytes()), 1.0);
        let kept = |i: u32| TILES.with(|t| t.borrow().by_key.contains_key(&hatch(i)));
        for i in 0..=MAX_TILES as u32 {
            assert!(tile(&hatch(i), || true, 4.0, 4.0, |_| {}).is_some());
        }
        assert!(!kept(0), "the oldest made room");
        assert!(kept(1) && kept(MAX_TILES as u32), "the rest stay");
        TILES.with(|t| {
            let t = t.borrow();
            assert_eq!((t.by_key.len(), t.order.len(), t.bytes), (MAX_TILES, MAX_TILES, MAX_TILES * 4 * 4 * 4));
        });
        // The key of a dot pattern tells its markers apart.
        let dots = |marker| FillPattern::Dots {
            marker,
            size: 3.0,
            outline: None,
            outline_width: 0.0,
            spacing: [10.0, 10.0],
            displacement: [5.0, 0.0],
            offset: [0.0, 0.0],
        };
        let red = Rgba::rgb(255, 0, 0);
        assert!(key(&dots(Marker::Circle), red, 1.0) != key(&dots(Marker::Square), red, 1.0));
        // One not to keep (a character marker drawn before the fonts are indexed) is drawn once
        // and kept apart, not among the tiles; settled, it is drawn again and kept.
        let glyph = |c| key(&dots(Marker::Char(rustgis_map::Glyph::new(c, "Noto Sans"))), red, 1.0);
        let mut drawn = 0;
        for _ in 0..3 {
            assert!(tile(&glyph('x'), || false, 4.0, 4.0, |_| drawn += 1).is_some());
        }
        assert!(tile(&glyph('y'), || false, 4.0, 4.0, |_| {}).is_some());
        assert_eq!(drawn, 1, "drawn once while the fonts are indexed");
        assert!(TILES.with(|t| !t.borrow().by_key.contains_key(&glyph('x'))));
        assert!(tile(&glyph('x'), || true, 4.0, 4.0, |_| drawn += 1).is_some());
        assert_eq!(drawn, 2, "drawn again once settled");
        assert!(PROVISIONAL.with(|p| p.borrow().is_empty()), "every provisional tile left behind");
        // Kept, it isn't asked again whether it may be.
        let mut asked = false;
        assert!(
            tile(
                &glyph('x'),
                || {
                    asked = true;
                    true
                },
                4.0,
                4.0,
                |_| drawn += 1
            )
            .is_some()
        );
        assert!(!asked && drawn == 2);
    }

    #[test]
    fn stacked_fills_under_a_pixel_tint_as_their_ink_covers() {
        let (red, white) = (Rgba::rgb(255, 0, 0), Rgba::rgb(255, 255, 255));
        let solid = |c| FillLayer { color: Some(c), pattern: FillPattern::Solid };
        // A solid fill is its colour; one over another hides it.
        assert_eq!(tint(&[solid(white), solid(red)], white), red);
        // Hatch lines 1.5 wide every 6 cover a quarter: a quarter of red over white.
        let hatch = FillLayer { color: Some(red), pattern: FillPattern::Hatch { angle: 0.0, spacing: 6.0, width: 1.5, offset: 0.0 } };
        let t = tint(&[solid(white), hatch], white);
        assert_eq!((t.r(), t.a()), (255, 255));
        assert!((i32::from(t.g()) - 191).abs() <= 1, "{t:?}");
        // Alone, a quarter as opaque; nothing drawn, nothing.
        assert!((i32::from(tint(&[hatch], white).a()) - 64).abs() <= 1);
        assert_eq!(tint(&[], white).a(), 0);
        assert_eq!(tint(&[FillLayer { color: None, pattern: FillPattern::Solid }], Rgba::new(0, 0, 0, 0)).a(), 0);
        // Hollow dots show their outlines' ink.
        let hollow = FillPattern::Dots {
            marker: Marker::Circle,
            size: 3.0,
            outline: Some(red),
            outline_width: 0.5,
            spacing: [10.0, 10.0],
            displacement: [5.0, 0.0],
            offset: [0.0, 0.0],
        };
        let t = tint(&[FillLayer { color: Some(Rgba::new(0, 0, 0, 0)), pattern: hollow }], white);
        assert!(t.r() == 255 && t.g() == 0 && t.a() > 0, "{t:?}");
    }
}
