//! A polygon's centre line on the screen, for its label curved along it (#2262): the longest path
//! through its medial axis, found on a grid over the polygon in view. Everything here is in screen
//! points, y down.
//!
//! The polygon is set on a grid of at most [`MAX_SIDE`] cells a side, each inside cell given its
//! distance to the outline (a chamfer distance transform, Borgefors 1986). The ends are the
//! polygon's geodesic diameter (the inside cell farthest from its deepest one, and the cell
//! farthest from that), and the line between them is the cheapest path where a step costs its
//! length over the square of its distance to the outline, so it keeps to the middle. Its ends,
//! where it runs out from the middle into a tip, are trimmed.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use rustgis_geom::{Coord, Extent};
use unicode_segmentation::UnicodeSegmentation;

use crate::line_labels::{self, Line};

/// The most cells the grid has along its longer side: enough to find a label's course, few enough
/// to work out per frame for the polygons labelled in view.
pub(super) const MAX_SIDE: usize = 160;

/// About the most outline vertices projected: more are dropped evenly, as finer detail than a cell
/// is lost on the grid anyway. Small rings kept whole may add half as many again, and a longer ring
/// keeps at least three points and its shape, so a polygon of many rings can keep a few more.
const MAX_VERTICES: usize = 20_000;

/// A ring with fewer points than this is not thinned, while such rings add up to at most half of
/// [`MAX_VERTICES`]: a small hole or island keeps every corner.
const KEEP_WHOLE: usize = 64;

/// How a polygon's rings are thinned before they are projected: one point in `every` kept, and
/// rings of fewer than `whole_below` points kept whole.
#[derive(Clone, Copy)]
struct Thinning {
    every: usize,
    whole_below: usize,
}

impl Thinning {
    /// For rings of `lens` points: one point in as many as keeps about [`MAX_VERTICES`] of them all,
    /// and the small rings whole unless there are so many of them (thousands of small islands) that
    /// they would take more than half of that.
    fn new(lens: impl Iterator<Item = usize>) -> Self {
        let (mut all, mut small) = (0, 0);
        for n in lens {
            all += n;
            if n < KEEP_WHOLE {
                small += n;
            }
        }
        Thinning { every: all.div_ceil(MAX_VERTICES).max(1), whole_below: if small <= MAX_VERTICES / 2 { KEEP_WHOLE } else { 0 } }
    }
}

/// Every `every`th point of `ring` (at `xy`), but all of a small ring (see [`Thinning`]) and at least
/// three of a longer one, so a small hole or island next to a long outline keeps its place and its
/// corners, and fewer dropped while that would lose half its area, so a ring dense along one side is
/// not left a line with no inside (#2295).
fn thin<T: Copy>(ring: &[T], how: Thinning, xy: impl Fn(&T) -> (f64, f64)) -> Vec<T> {
    let Thinning { every, whole_below } = how;
    // The area within every `step`th point (the shoelace sum, back to the first point).
    let area = |step: usize| {
        let mut points = ring.iter().step_by(step).map(&xy);
        let Some(first) = points.next() else { return 0.0 };
        let (mut prev, mut twice) = (first, 0.0);
        for p in points.chain(std::iter::once(first)) {
            twice += prev.0 * p.1 - p.0 * prev.1;
            prev = p;
        }
        f64::abs(twice) / 2.0
    };
    let mut step = if ring.len() < whole_below { 1 } else { every.min(ring.len() / 3).max(1) };
    if step > 1 {
        let whole = area(1);
        while step > 1 && area(step) < whole / 2.0 {
            step /= 2;
        }
    }
    ring.iter().step_by(step).copied().collect()
}

/// How far the centre line is trimmed at each end, as a share of its distance to the outline there.
const TIP: f32 = 2.0;

/// A cell and its cost so far, cheapest first in a [`BinaryHeap`].
#[derive(PartialEq)]
struct Next(f32, usize);

impl Eq for Next {}

impl PartialOrd for Next {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Next {
    fn cmp(&self, other: &Self) -> Ordering {
        other.0.total_cmp(&self.0).then(other.1.cmp(&self.1))
    }
}

/// The polygon on a grid: each cell's distance to the outline in cells (0 outside).
struct Grid {
    nx: usize,
    ny: usize,
    depth: Vec<f32>,
}

impl Grid {
    /// The cells round `i`, with the length of the step to each.
    fn around(&self, i: usize) -> impl Iterator<Item = (usize, f32)> + '_ {
        let (x, y) = ((i % self.nx) as isize, (i / self.nx) as isize);
        [(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)].into_iter().filter_map(move |(dx, dy)| {
            let (x, y) = (x + dx, y + dy);
            (x >= 0 && y >= 0 && (x as usize) < self.nx && (y as usize) < self.ny)
                .then(|| (y as usize * self.nx + x as usize, if dx != 0 && dy != 0 { std::f32::consts::SQRT_2 } else { 1.0 }))
        })
    }

    /// The cheapest way from `from` to every inside cell, a step costing its length times
    /// `weight` of the cell it reaches: each cell's cost and the cell it was reached from.
    fn paths(&self, from: usize, weight: impl Fn(f32) -> f32) -> (Vec<f32>, Vec<usize>) {
        let n = self.depth.len();
        let (mut cost, mut prev) = (vec![f32::INFINITY; n], vec![usize::MAX; n]);
        let mut heap = BinaryHeap::new();
        if let Some(c) = cost.get_mut(from) {
            *c = 0.0;
            heap.push(Next(0.0, from));
        }
        while let Some(Next(c, i)) = heap.pop() {
            if cost.get(i).is_some_and(|best| c > *best) {
                continue;
            }
            for (j, step) in self.around(i) {
                let d = self.depth.get(j).copied().unwrap_or(0.0);
                if d <= 0.0 {
                    continue;
                }
                let next = c + step * weight(d);
                if let (Some(best), Some(p)) = (cost.get_mut(j), prev.get_mut(j))
                    && next < *best
                {
                    (*best, *p) = (next, i);
                    heap.push(Next(next, j));
                }
            }
        }
        (cost, prev)
    }
}

/// The inside cell farthest from `from` by cost (`from` itself when none is reachable).
fn farthest(cost: &[f32], from: usize) -> usize {
    cost.iter().enumerate().filter(|(_, c)| c.is_finite()).max_by(|a, b| a.1.total_cmp(b.1)).map_or(from, |(i, _)| i)
}

/// The centre line of the polygon with outlines `rings` (holes among them, even–odd) in a view
/// `w` × `h` points, as points from one end to the other: the part of it in view, trimmed where it
/// runs into the polygon's tips and where the polygon is narrower than `half_width` either side of
/// it. `None` for a polygon with no inside in view, or a centre line of fewer than two points.
pub(super) fn centre_line(rings: &[Vec<(f32, f32)>], w: f32, h: f32, half_width: f32) -> Option<Vec<(f32, f32)>> {
    let how = Thinning::new(rings.iter().map(Vec::len));
    let rings: Vec<Vec<(f32, f32)>> = rings
        .iter()
        .map(|r| {
            thin(r, how, |p| (f64::from(p.0), f64::from(p.1)))
                .into_iter()
                .filter(|p| p.0.is_finite() && p.1.is_finite())
                .collect::<Vec<_>>()
        })
        .filter(|r| r.len() >= 3)
        .collect();
    // The polygon's extent in view.
    let (mut x0, mut y0, mut x1, mut y1) = (f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY);
    for p in rings.iter().flatten() {
        (x0, y0, x1, y1) = (x0.min(p.0), y0.min(p.1), x1.max(p.0), y1.max(p.1));
    }
    let (x0, y0, x1, y1) = (x0.max(0.0), y0.max(0.0), x1.min(w), y1.min(h));
    if !(x1 > x0 && y1 > y0) {
        return None;
    }
    let cell = ((x1 - x0).max(y1 - y0) / MAX_SIDE as f32).max(0.25);
    // A cell of outside all round, so the line keeps off the view's edges as off the outline.
    let (nx, ny) = (((x1 - x0) / cell).ceil() as usize + 2, ((y1 - y0) / cell).ceil() as usize + 2);
    let (ox, oy) = (x0 - cell, y0 - cell);
    let mut depth = vec![0.0f32; nx * ny];
    // Where the outline crosses each row's middle: each edge put in the rows it spans, so the work
    // grows with the edges and the cells, not their product.
    let mut rows: Vec<Vec<f32>> = vec![Vec::new(); ny];
    let row_of = |y: f32| ((y - oy) / cell - 0.5).ceil().clamp(1.0, (ny - 1) as f32) as usize;
    for r in &rings {
        for (k, a) in r.iter().enumerate() {
            let Some(b) = r.get(k + 1).or_else(|| r.first()) else { continue };
            for j in row_of(a.1.min(b.1)).saturating_sub(1).max(1)..(row_of(a.1.max(b.1)) + 1).min(ny - 1) {
                let yc = oy + (j as f32 + 0.5) * cell;
                if let Some(row) = rows.get_mut(j)
                    && (a.1 <= yc) != (b.1 <= yc)
                {
                    row.push(a.0 + (yc - a.1) * (b.0 - a.0) / (b.1 - a.1));
                }
            }
        }
    }
    // Inside cells, row by row between pairs of crossings (even–odd).
    for (j, cross) in rows.iter_mut().enumerate() {
        cross.sort_by(f32::total_cmp);
        for [a, b] in cross.as_chunks::<2>().0 {
            // The cells whose middles are between the two crossings.
            let first = ((a - ox) / cell - 0.5).ceil().max(1.0);
            let last = ((b - ox) / cell - 0.5).floor().min((nx - 2) as f32);
            if last >= first {
                depth.get_mut(j * nx + first as usize..=j * nx + last as usize).into_iter().flatten().for_each(|d| *d = f32::INFINITY);
            }
        }
    }
    // Distance to the outline, two passes (a chamfer with steps 1 and √2, in cells).
    let grid_steps = |i: usize, forward: bool| -> [(Option<usize>, f32); 4] {
        let (x, y) = (i % nx, i / nx);
        let at = |dx: isize, dy: isize| {
            let (x, y) = (x as isize + dx, y as isize + dy);
            (x >= 0 && y >= 0 && (x as usize) < nx && (y as usize) < ny).then(|| y as usize * nx + x as usize)
        };
        let s = if forward { -1 } else { 1 };
        [(at(s, 0), 1.0), (at(0, s), 1.0), (at(s, s), std::f32::consts::SQRT_2), (at(-s, s), std::f32::consts::SQRT_2)]
    };
    for forward in [true, false] {
        let order: Box<dyn Iterator<Item = usize>> = if forward { Box::new(0..nx * ny) } else { Box::new((0..nx * ny).rev()) };
        for i in order {
            if depth.get(i).is_none_or(|d| *d <= 0.0) {
                continue;
            }
            let best = grid_steps(i, forward)
                .iter()
                .map(|(j, step)| j.and_then(|j| depth.get(j)).map_or(*step, |d| d + step))
                .fold(f32::INFINITY, f32::min);
            if let Some(d) = depth.get_mut(i) {
                *d = d.min(best);
            }
        }
    }
    let grid = Grid { nx, ny, depth };
    let deepest = grid.depth.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).filter(|(_, d)| **d > 0.0)?.0;
    let a = farthest(&grid.paths(deepest, |_| 1.0).0, deepest);
    let b = farthest(&grid.paths(a, |_| 1.0).0, a);
    let (_, prev) = grid.paths(a, |d| 1.0 / (d * d));
    let mut cells = vec![b];
    while let Some(&p) = cells.last().and_then(|i| prev.get(*i)).filter(|p| **p != usize::MAX) {
        if cells.len() > grid.depth.len() {
            return None;
        }
        cells.push(p);
    }
    // Trimmed at each end while the line is nearer that end than twice its distance to the
    // outline: the line runs from the middle out into a tip or a corner there (in a right-angled
    // corner its distance to the outline grows by 0.7 of the way it goes).
    let deep = |i: &usize| grid.depth.get(*i).copied().unwrap_or(0.0);
    let keep = |cells: &[usize]| {
        let mut run = 0.0;
        cells
            .windows(2)
            .position(|w| {
                let (Some(p), Some(q)) = (w.first(), w.get(1)) else { return true };
                run += if p % nx != q % nx && p / nx != q / nx { std::f32::consts::SQRT_2 } else { 1.0 };
                run >= TIP * deep(q)
            })
            .map_or(cells.len(), |k| k + 1)
    };
    let start = keep(&cells);
    cells.reverse();
    let end = cells.len().checked_sub(keep(&cells) + 1)?;
    cells.reverse();
    // Of what is left, the longest stretch where the polygon is wide enough all along: a narrow
    // neck between two wide parts (a dumbbell) is never under the text (#2295). A cell at depth `d`
    // is `d` cells from the middle of the nearest outside cell, and the outline runs somewhere
    // between the two: only `d - 1` cells of it are sure to be inside.
    let wide = |i: &usize| (deep(i) - 1.0) * cell >= half_width;
    let cells = cells.get(start..=end)?.split(|i| !wide(i)).max_by_key(|run| run.len()).filter(|run| run.len() >= 2)?;
    // Cell centres, softened over two cells either side so the grid's steps don't turn the text.
    let pts: Vec<(f32, f32)> = cells.iter().map(|i| (ox + ((i % nx) as f32 + 0.5) * cell, oy + ((i / nx) as f32 + 0.5) * cell)).collect();
    let soft: Vec<(f32, f32)> = (0..pts.len())
        .map(|k| {
            let s = pts.get(k.saturating_sub(2)..(k + 3).min(pts.len())).unwrap_or_default();
            let n = s.len().max(1) as f32;
            (s.iter().map(|p| p.0).sum::<f32>() / n, s.iter().map(|p| p.1).sum::<f32>() / n)
        })
        .collect();
    (soft.len() >= 2).then_some(soft)
}

/// One line of `text`, each character `measure` wide and `gap` apart, curved along the centre line
/// of the polygon `polys` as drawn: the label's centre and its characters. Tried at the line's
/// middle, then at places either side of it ([`line_labels::positions`]); the text must fit on the
/// line, with the polygon at least half its height either side of it. `None` when it fits nowhere,
/// or a part of the polygon near the view doesn't project (behind the globe): the label is then
/// placed as `Auto`.
///
/// It works per frame, for each polygon labelled this way with its label point in view, and looks at
/// no Cancel: a step bounded by the outline's points near the view (each read once or a few times
/// to thin it, about [`MAX_VERTICES`] projected) and a grid of at most [`MAX_SIDE`] + 2 cells a side, walked three times.
pub(super) fn glyphs(
    ctx: &super::Ctx<'_>,
    polys: &[Vec<Vec<Coord>>],
    text: &str,
    measure: impl Fn(&str) -> f32,
    gap: f32,
    size: f32,
) -> Option<((f32, f32), Vec<super::LabelGlyph>)> {
    let chars: Vec<String> = text.graphemes(true).map(String::from).collect();
    let advances: Vec<f32> = chars.iter().map(|c| measure(c)).collect();
    let (centre, turns) = curved(ctx, polys, &advances, gap, size, super::text::block_height(text, size))?;
    let glyphs = chars.into_iter().zip(turns).map(|(text, (dx, dy, angle))| super::LabelGlyph { text, dx, dy, angle }).collect();
    Some((centre, glyphs))
}

/// The rings of the polygon `polys` near the view on the screen, thinned before they are projected
/// (about [`MAX_VERTICES`] in all); `None` when a place of one doesn't project (behind the globe).
pub(super) fn screen_rings(ctx: &super::Ctx<'_>, polys: &[Vec<Vec<Coord>>]) -> Option<Vec<Vec<(f32, f32)>>> {
    let near = |ring: &Vec<Coord>| {
        let mut e = Extent::EMPTY;
        ring.iter().for_each(|c| e.expand(*c));
        ctx.globe.is_some() || ctx.cull.intersects(&e)
    };
    let near: Vec<&Vec<Coord>> = polys.iter().filter(|poly| poly.first().is_some_and(near)).flatten().collect();
    let how = Thinning::new(near.iter().map(|r| r.len()));
    near.into_iter()
        .map(|ring| {
            thin(ring, how, |c| (c.x, c.y))
                .into_iter()
                .map(|c| ctx.screen(c).map(|(x, y)| (x as f32, y as f32)))
                .collect::<Option<Vec<_>>>()
        })
        .collect()
}

/// [`glyphs`]' characters `advances` wide and `height` tall, as [`Line::curved`] takes them.
fn curved(
    ctx: &super::Ctx<'_>,
    polys: &[Vec<Vec<Coord>>],
    advances: &[f32],
    gap: f32,
    size: f32,
    height: f32,
) -> Option<((f32, f32), Vec<(f32, f32, f32)>)> {
    let rings = screen_rings(ctx, polys)?;
    let len = line_labels::curved_len(advances, gap);
    // Too small on the screen for the text: no grid worked out.
    let (mut lo, mut hi) = ((f32::INFINITY, f32::INFINITY), (f32::NEG_INFINITY, f32::NEG_INFINITY));
    for p in rings.iter().flatten() {
        (lo, hi) = ((lo.0.min(p.0), lo.1.min(p.1)), (hi.0.max(p.0), hi.1.max(p.1)));
    }
    if (hi.0 - lo.0).hypot(hi.1 - lo.1).partial_cmp(&len) != Some(Ordering::Greater) {
        return None;
    }
    let (w, h) = (ctx.view.width as f32, ctx.view.height as f32);
    let line = Line::new(&centre_line(&rings, w, h, height / 2.0)?)?;
    line_labels::positions(line.length(), len, 0.0).into_iter().find_map(|at| line.curved(advances, gap, size, 0.0, Some(at)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A band `width` points wide round a quarter of a circle of radius 300 about (0, 400).
    fn band(width: f32) -> Vec<(f32, f32)> {
        let arc =
            |r: f32| (0..=60).map(move |i| i as f32 / 60.0 * std::f32::consts::FRAC_PI_2).map(move |t| (r * t.sin(), 400.0 - r * t.cos()));
        let mut ring: Vec<(f32, f32)> = arc(300.0 + width / 2.0).collect();
        ring.extend(arc(300.0 - width / 2.0).collect::<Vec<_>>().into_iter().rev());
        ring
    }

    /// Distance from (0, 400): 300 on the band's middle.
    fn radius(p: (f32, f32)) -> f32 {
        p.0.hypot(p.1 - 400.0)
    }

    #[test]
    fn a_curved_band_has_its_centre_line_down_its_middle() {
        let line = centre_line(&[band(40.0)], 1000.0, 1000.0, 6.0).expect("a centre line");
        assert!(line.iter().all(|p| (radius(*p) - 300.0).abs() < 6.0), "{line:?}");
        // Nearly end to end, trimmed at each tip by about twice the band's half-width.
        let length: f32 = line.windows(2).map(|w| (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1)).sum();
        let arc = 300.0 * std::f32::consts::FRAC_PI_2;
        assert!(length > arc - 4.0 * TIP * 20.0 && length < arc, "{length} of {arc}");
    }

    #[test]
    fn a_hole_and_the_view_keep_the_line_off_them() {
        // A wide rectangle with a hole across its upper half: the line runs below the hole.
        let outer = vec![(0.0, 0.0), (600.0, 0.0), (600.0, 100.0), (0.0, 100.0)];
        let hole = vec![(50.0, 10.0), (550.0, 10.0), (550.0, 50.0), (50.0, 50.0)];
        let line = centre_line(&[outer.clone(), hole], 1000.0, 1000.0, 4.0).expect("below the hole");
        assert!(line.iter().filter(|p| (100.0..500.0).contains(&p.0)).all(|p| p.1 > 55.0), "{line:?}");
        // Half of it out of view: the line is in the part in view.
        let line = centre_line(&[outer], 300.0, 1000.0, 4.0).expect("in view");
        assert!(line.iter().all(|p| p.0 < 300.0 && (p.1 - 50.0).abs() < 8.0), "{line:?}");
    }

    /// Drawn: a lake curving round a quarter circle gets its name along it, every character inside
    /// it; a small one keeps its label level, unchanged; set to `Auto`, the long lake's label is too.
    #[test]
    fn a_curved_lakes_name_follows_it_and_a_small_ones_stays_level() {
        use rustgis_data::{Dataset, Field, FieldType, Geometry, GeometryType, Value};
        use rustgis_geom::lonlat_to_merc;
        use rustgis_map::{Camera, LabelPlacement, Map};
        let (centre, r) = (Coord::new(-1.0, -1.5), 2.2);
        let arc = |r: f64| {
            (0..=40)
                .map(move |i| f64::from(i) / 40.0 * std::f64::consts::FRAC_PI_2)
                .map(move |t| Coord::new(centre.x + r * t.sin(), centre.y + r * t.cos()))
        };
        let mut ring: Vec<Coord> = arc(r + 0.2).collect();
        ring.extend(arc(r - 0.2).collect::<Vec<_>>().into_iter().rev());
        ring.push(ring[0]);
        let square =
            vec![Coord::new(-1.6, -1.6), Coord::new(-1.3, -1.6), Coord::new(-1.3, -1.3), Coord::new(-1.6, -1.3), Coord::new(-1.6, -1.6)];
        let mut m = Map::new("Map");
        m.basemap = rustgis_map::Basemap::Blank;
        let mut d = Dataset::new("lakes", GeometryType::Polygon, vec![Field::new("name", FieldType::Text)]);
        d.push(Some(Geometry::Polygon(vec![vec![ring]])), vec![Value::Text("Lago Comprido".into())]);
        d.push(Some(Geometry::Polygon(vec![vec![square]])), vec![Value::Text("Lagoa Redonda".into())]);
        let id = m.add_dataset(d);
        let camera = Camera { center: lonlat_to_merc(Coord::new(0.0, 0.0)), res: 1500.0, bearing: 0.0, pitch: 0.0 };
        let view = crate::View { width: 400.0, height: 400.0, pixels_per_point: 1.0, camera, sky: false };
        let l = m.layer_mut(id).unwrap();
        l.labels.enabled = true;
        l.labels.default.field = "name".into();
        l.labels.default.size = 12.0;
        let level = crate::render(&mut m, &view).unwrap().labels;
        m.layer_mut(id).unwrap().labels.default.placement = LabelPlacement::Curved;
        let r_ = crate::render(&mut m, &view).unwrap().labels;
        let long = r_.iter().find(|l| l.text == "Lago Comprido").expect("placed");
        assert_eq!(long.glyphs.iter().map(|g| g.text.as_str()).collect::<String>(), "Lago Comprido");
        // Inside the lake: within its half-width (0.2°) of the middle of the band, on the screen.
        let (cx, cy) = view.to_screen(lonlat_to_merc(centre));
        let per_degree =
            (view.to_screen(lonlat_to_merc(Coord::new(1.0, 0.0))).0 - view.to_screen(lonlat_to_merc(Coord::new(0.0, 0.0))).0) as f32;
        for g in &long.glyphs {
            let off = ((long.x + g.dx) - cx as f32).hypot((long.y + g.dy) - cy as f32) / per_degree - r as f32;
            assert!(off.abs() < 0.2 - 6.0 / per_degree, "{} off the middle by {off}°", g.text);
        }
        assert!(long.glyphs.first().zip(long.glyphs.last()).is_some_and(|(a, b)| (a.angle - b.angle).abs() > 0.3), "turning with the lake");
        let small = r_.iter().find(|l| l.text == "Lagoa Redonda").expect("placed");
        let before = level.iter().find(|l| l.text == "Lagoa Redonda").expect("placed");
        assert!(small.glyphs.is_empty() && (small.x, small.y, small.angle) == (before.x, before.y, before.angle));
        assert!(level.iter().all(|l| l.glyphs.is_empty()), "Auto stays level");
    }

    /// An outline thinned to its [`MAX_VERTICES`] keeps a small hole: the line still goes round it.
    #[test]
    fn a_long_outline_keeps_its_small_holes() {
        // 40 000 points round a 600 × 100 rectangle, and a five-point hole across its upper half.
        let n = 10_000;
        let mut outer: Vec<(f32, f32)> = (0..n).map(|i| (i as f32 * 600.0 / n as f32, 0.0)).collect();
        outer.extend((0..n).map(|i| (600.0, i as f32 * 100.0 / n as f32)));
        outer.extend((0..n).map(|i| (600.0 - i as f32 * 600.0 / n as f32, 100.0)));
        outer.extend((0..n).map(|i| (0.0, 100.0 - i as f32 * 100.0 / n as f32)));
        let hole = vec![(50.0, 10.0), (550.0, 10.0), (550.0, 50.0), (50.0, 50.0), (50.0, 10.0)];
        let line = centre_line(&[outer, hole], 1000.0, 1000.0, 4.0).expect("below the hole");
        assert!(line.iter().filter(|p| (100.0..500.0).contains(&p.0)).all(|p| p.1 > 55.0), "{line:?}");
    }

    /// A hole dense along one side keeps its shape when thinned: points taken from that side alone
    /// would leave it a line with no inside, and the line through it.
    #[test]
    fn a_hole_dense_along_one_side_keeps_its_shape() {
        // 100 000 points round a 600 × 100 rectangle (one point in six read), and a hole across
        // its upper half, too long to be kept whole, with 61 points along its top side (its bottom
        // bent up to y = 40 in the middle, so one point in three keeps clearly over half its area).
        let n = 25_000;
        let mut outer: Vec<(f32, f32)> = (0..n).map(|i| (i as f32 * 600.0 / n as f32, 0.0)).collect();
        outer.extend((0..n).map(|i| (600.0, i as f32 * 100.0 / n as f32)));
        outer.extend((0..n).map(|i| (600.0 - i as f32 * 600.0 / n as f32, 100.0)));
        outer.extend((0..n).map(|i| (0.0, 100.0 - i as f32 * 100.0 / n as f32)));
        let mut hole: Vec<(f32, f32)> = (0..=60).map(|i| (50.0 + i as f32 * 500.0 / 60.0, 10.0)).collect();
        hole.extend([(550.0, 50.0), (300.0, 40.0), (50.0, 50.0), (50.0, 10.0)]);
        assert!(hole.len() >= KEEP_WHOLE);
        // Thinned, but not to its top side alone.
        let how = Thinning::new([outer.len(), hole.len()].into_iter());
        let kept = thin(&hole, how, |p| (f64::from(p.0), f64::from(p.1)));
        assert!(kept.len() < hole.len() && kept.iter().any(|p| p.1 == 50.0), "{kept:?}");
        let line = centre_line(&[outer, hole], 1000.0, 1000.0, 4.0).expect("below the hole");
        assert!(line.iter().filter(|p| (100.0..500.0).contains(&p.0)).all(|p| p.1 > 55.0), "{line:?}");
    }

    /// Two wide ends joined by a narrow neck: the line is on one end only, never across the neck.
    #[test]
    fn a_narrow_neck_is_never_under_the_text() {
        let dumbbell = vec![
            (0.0, 0.0),
            (250.0, 0.0),
            (250.0, 45.0),
            (350.0, 45.0),
            (350.0, 0.0),
            (600.0, 0.0),
            (600.0, 100.0),
            (350.0, 100.0),
            (350.0, 55.0),
            (250.0, 55.0),
            (250.0, 100.0),
            (0.0, 100.0),
        ];
        let line = centre_line(std::slice::from_ref(&dumbbell), 1000.0, 1000.0, 10.0).expect("on one end");
        assert!(line.iter().all(|p| p.0 < 250.0) || line.iter().all(|p| p.0 > 350.0), "{line:?}");
        // With text thin enough for the neck, the line runs from end to end.
        let through = centre_line(&[dumbbell], 1000.0, 1000.0, 2.0).expect("end to end");
        assert!(through.iter().any(|p| p.0 < 250.0) && through.iter().any(|p| p.0 > 350.0), "{through:?}");
    }

    /// A small hole is kept whole when its outer ring is thinned: one point in five of it would be
    /// a triangle of half its area, without its lower corners.
    #[test]
    fn a_small_hole_keeps_its_corners() {
        // 100 000 points round a 600 × 100 rectangle, and a 15-point hole across its upper half,
        // with a point at each corner.
        let n = 25_000;
        let mut outer: Vec<(f32, f32)> = (0..n).map(|i| (i as f32 * 600.0 / n as f32, 0.0)).collect();
        outer.extend((0..n).map(|i| (600.0, i as f32 * 100.0 / n as f32)));
        outer.extend((0..n).map(|i| (600.0 - i as f32 * 600.0 / n as f32, 100.0)));
        outer.extend((0..n).map(|i| (0.0, 100.0 - i as f32 * 100.0 / n as f32)));
        let mut hole: Vec<(f32, f32)> = (0..=5).map(|i| (50.0 + i as f32 * 100.0, 10.0)).collect();
        hole.extend((0..9).map(|i| (550.0 - i as f32 * 62.5, 50.0)));
        assert_eq!(hole.len(), 15);
        let how = Thinning::new([outer.len(), hole.len()].into_iter());
        let kept = thin(&hole, how, |p| (f64::from(p.0), f64::from(p.1)));
        assert!(kept.contains(&(50.0, 50.0)) && kept.contains(&(550.0, 50.0)), "{kept:?}");
        let line = centre_line(&[outer, hole], 1000.0, 1000.0, 4.0).expect("below the hole");
        assert!(line.iter().filter(|p| (100.0..500.0).contains(&p.0)).all(|p| p.1 > 55.0), "{line:?}");
    }

    /// A few small islands are kept whole; ten thousand of them are thinned, as a long ring is.
    #[test]
    fn many_small_islands_are_thinned() {
        let island: Vec<(f32, f32)> = (0..60).map(|i| i as f32 / 60.0 * std::f32::consts::TAU).map(|t| (t.cos(), t.sin())).collect();
        let xy = |p: &(f32, f32)| (f64::from(p.0), f64::from(p.1));
        assert_eq!(thin(&island, Thinning::new(std::iter::repeat_n(60, 100)), xy).len(), 60);
        let kept = thin(&island, Thinning::new(std::iter::repeat_n(60, 10_000)), xy).len();
        assert!((3..60).contains(&kept), "{kept}");
    }

    /// A rectangle 10 points tall is too narrow for text 12 points tall, though its middle row of
    /// cells is two cells (7.5 points) deep on the grid: the outline may be nearly a cell nearer.
    #[test]
    fn a_band_just_too_narrow_gives_no_line() {
        let thin_band = vec![(0.0, 0.0), (600.0, 0.0), (600.0, 10.0), (0.0, 10.0)];
        let line = centre_line(std::slice::from_ref(&thin_band), 1000.0, 1000.0, 6.0);
        assert!(line.is_none(), "{line:?}");
        // Text thin enough for it gets a line down its middle.
        let line = centre_line(&[thin_band], 1000.0, 1000.0, 3.0).expect("down its middle");
        assert!(line.iter().all(|p| (p.1 - 5.0).abs() < 2.0), "{line:?}");
    }

    #[test]
    fn nothing_inside_or_too_narrow_gives_no_line() {
        assert!(centre_line(&[], 100.0, 100.0, 1.0).is_none());
        assert!(centre_line(&[vec![(200.0, 200.0), (300.0, 200.0), (300.0, 300.0)]], 100.0, 100.0, 1.0).is_none(), "out of view");
        assert!(centre_line(&[vec![(0.0, 0.0), (f32::NAN, 1.0), (5.0, 5.0)]], 100.0, 100.0, 1.0).is_none());
        // A band 10 points wide has no stretch 20 either side.
        assert!(centre_line(&[band(10.0)], 1000.0, 1000.0, 20.0).is_none());
    }
}
