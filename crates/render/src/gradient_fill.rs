//! A polygon's gradient and shapeburst fills (`FillPattern::Gradient`, `FillPattern::Shapeburst`,
//! #882 piece 5b), drawn as QGIS draws its gradient and shapeburst fills. A gradient is one of
//! tiny-skia's shaders: linear, radial, or a sweep round its first point (QGIS's conical), its
//! points placed in the feature's box on screen (or the map view's). A shapeburst shades by how
//! far each pixel is from the polygon's edge: the polygon is drawn as a mask over its box, the
//! distance of each pixel inside to the nearest outside worked out exactly (Felzenszwalb and
//! Huttenlocher's two passes), then coloured along the ramp and drawn through the polygon. A large
//! polygon is worked out on a coarser grid, which a smooth shading scales up from well; one too
//! thin for that grid is worked out at full resolution in tiles.

use std::cell::Cell;

use crate::Canvas;
use rustgis_map::Rgba;
use rustgis_map::fills::{self, FillPattern, GradientPoint, GradientShape, Spread, Stop, Stops};
use tiny_skia::{
    Color, FillRule, GradientStop, LinearGradient, Mask, Paint, Path, Pixmap, Point, RadialGradient, Rect, Shader, SpreadMode,
    SweepGradient, Transform,
};

/// The most pixels a shapeburst works out its distances on: past that, a larger polygon is shaded
/// at a coarser resolution and scaled up (1 megapixel: about 13 MB of working memory; the shading
/// is smooth, so it scales up well).
const MAX_BURST_PX: f32 = 1.0e6;
/// The pixels all of a frame's shapebursts work on before each further one is shaded coarsely
/// (at [`MIN_BURST_PX`]), so a layer of many large polygons still draws in time: a threshold, not
/// a cap (the one that crosses it finishes). A tilted view's bands, each drawn as a frame of its
/// own, count their own.
pub(crate) const FRAME_BURST_PX: f32 = 16.0e6;
const MIN_BURST_PX: f32 = 65_536.0;

/// The side of a tile a thin shapeburst is worked out in at full resolution (device pixels), and
/// the most tiles one part takes (past that, it is shaded on the coarse grid).
const TILE_PX: f32 = 512.0;
const MAX_TILES: usize = 1024;
/// How many cells deep a shading must be on a coarse grid to be shaded from it: a thinner polygon
/// is worked out again at full resolution.
const MIN_DEPTH_CELLS: f32 = 4.0;

/// One polygon part to shade: its path (cut to the clip box), and, worked out from the part
/// before that cut (device pixels, so they stay put as the map pans), its box (a gradient's
/// points and turn are placed in it) and its outer ring's centroid (QGIS's, a gradient's
/// `"centroid"`); the clip box (whose sides cut into the path are no edges of it); and its
/// deepest point found coarsely (given whether to leave its holes out), for a shapeburst over the
/// whole shape that reaches past the clip box.
pub(crate) struct Part<'a> {
    pub path: &'a Path,
    pub whole: Option<Rect>,
    pub centroid: Option<Point>,
    pub clip: Option<Rect>,
    pub whole_depth: &'a dyn Fn(bool) -> Option<f32>,
    /// The pixels the frame's shapebursts have worked on so far.
    pub budget: &'a Cell<f32>,
}

/// Draws `pattern` (a gradient or shapeburst) in `part`, from `color` (its fill's own, or the
/// feature's) to its second colour. `exterior` is the part's outer ring alone, for a shapeburst
/// that leaves its holes' edges out; `ppp` is pixels a point. False for a pattern of another kind.
/// (A gradient in the view's box takes the picture's: in a tilted view, drawn in bands, each
/// band's.)
pub(crate) fn draw(
    pm: &mut Canvas,
    part: &Part,
    exterior: impl FnOnce() -> Option<Path>,
    pattern: &FillPattern,
    color: Rgba,
    ppp: f32,
) -> bool {
    let path = part.path;
    // A part under about a pixel: its colours' average, as a feature that small is drawn.
    let b = path.bounds();
    if b.width() < 1.5
        && b.height() < 1.5
        && let FillPattern::Gradient { color2, ref stops, discrete, .. } | FillPattern::Shapeburst { color2, ref stops, discrete, .. } =
            *pattern
    {
        let paint = Paint {
            shader: Shader::SolidColor(color_of(average(color, color2, stops, discrete))),
            anti_alias: crate::antialiased(),
            ..Paint::default()
        };
        pm.fill_path(path, &paint, FillRule::EvenOdd, Transform::identity(), None);
        return true;
    }
    match *pattern {
        FillPattern::Gradient { shape, color2, ref stops, discrete, from, to, spread, angle, in_view } => {
            let view = Rect::from_xywh(0.0, 0.0, pm.width() as f32, pm.height() as f32);
            let bounds = if in_view { view } else { Some(part.whole.unwrap_or(b)) };
            let shader = bounds.and_then(|b| {
                let centroid = part.centroid.unwrap_or_else(|| centre(b));
                gradient(Gradient { shape, ramp: fills::ramp(color, color2, stops, discrete), spread, angle }, b, centroid, from, to)
            });
            let shader = shader.unwrap_or(Shader::SolidColor(color_of(color)));
            let paint = Paint { shader, anti_alias: crate::antialiased(), ..Paint::default() };
            pm.fill_path(path, &paint, FillRule::EvenOdd, Transform::identity(), None);
            true
        }
        FillPattern::Shapeburst { color2, ref stops, discrete, distance, blur, ignore_rings } => {
            let edges = if ignore_rings { exterior() } else { None };
            let ramp = fills::ramp(color, color2, stops, discrete);
            let burst =
                Burst { ramp: &ramp, distance: distance.map(|d| d * ppp), blur: f32::from(blur) * ppp, outer_only: edges.is_some() };
            shapeburst(pm.pixels(), part, edges.as_ref().unwrap_or(path), &burst);
            true
        }
        _ => false,
    }
}

/// A gradient's look apart from its points.
struct Gradient {
    shape: GradientShape,
    ramp: Vec<Stop>,
    spread: Spread,
    angle: f32,
}

fn color_of(c: Rgba) -> Color {
    Color::from_rgba8(c.r(), c.g(), c.b(), c.a())
}

/// The shader of `g` from `from` to `to` in `bounds` (device pixels, as `centroid`); `None` for
/// one of no length.
fn gradient(g: Gradient, bounds: Rect, centroid: Point, from: GradientPoint, to: GradientPoint) -> Option<Shader<'static>> {
    let place = |p: GradientPoint| match p {
        GradientPoint::At([u, v]) => Point::from_xy(bounds.left() + u * bounds.width(), bounds.top() + v * bounds.height()),
        GradientPoint::Centroid(_) => centroid,
    };
    let (a, b) = (place(from), place(to));
    let stops: Vec<GradientStop> = g.ramp.iter().map(|s| GradientStop::new(s.at, color_of(s.color))).collect();
    let mode = match g.spread {
        Spread::Pad => SpreadMode::Pad,
        Spread::Reflect => SpreadMode::Reflect,
        Spread::Repeat => SpreadMode::Repeat,
    };
    // Turned anticlockwise on screen round the box's middle (tiny-skia turns clockwise: y runs down).
    let c = centre(bounds);
    let turn = Transform::from_rotate_at(-g.angle, c.x, c.y);
    match g.shape {
        GradientShape::Linear => LinearGradient::new(a, b, stops, mode, turn),
        GradientShape::Radial => {
            let r = a.distance(b);
            (r > 0.0).then(|| RadialGradient::new(a, 0.0, a, r, stops, mode, turn)).flatten()
        }
        GradientShape::Conical => {
            // A sweep runs clockwise on screen from east: mirrored to run anticlockwise, and turned
            // to start towards `to`.
            let towards = (b.y - a.y).atan2(b.x - a.x).to_degrees();
            if !towards.is_finite() || a.distance(b) <= 0.0 {
                return None;
            }
            let t = Transform::from_translate(-a.x, -a.y).post_scale(1.0, -1.0).post_rotate(towards).post_translate(a.x, a.y);
            // (`t` is a reflection through the point, its own inverse.)
            let t = t.post_concat(turn);
            SweepGradient::new(a, 0.0, 360.0, stops, mode, t)
        }
    }
}

fn centre(r: Rect) -> Point {
    Point::from_xy(r.left() + r.width() / 2.0, r.top() + r.height() / 2.0)
}

/// A shapeburst's look: its colours from the edge inwards, how deep it shades (device pixels;
/// `None`: to the point deepest inside), how wide its blur is, and whether its holes' edges are
/// left out.
struct Burst<'a> {
    ramp: &'a [Stop],
    distance: Option<f32>,
    blur: f32,
    outer_only: bool,
}

/// A squared distance standing for "no cell outside on the grid".
const FAR: f32 = 1e20;

/// Distances on a grid over device pixels `rect` (left, top, right, bottom) at `k` cells a pixel:
/// each cell's squared, in cells, to the nearest cell outside the polygon ([`FAR`] where none lies
/// on the grid; 0 outside).
struct Grid {
    dist: Vec<f32>,
    w: usize,
    h: usize,
    rect: [f32; 4],
    k: f32,
}

impl Grid {
    /// The grid of `edges` over `rect`, counted against the frame's `budget`; `None` for one that
    /// can't be made.
    fn new(edges: &Path, rect: [f32; 4], k: f32, budget: &Cell<f32>) -> Option<Grid> {
        let [l, t, r, b] = rect;
        let (w, h) = (((r - l) * k).ceil().max(1.0) as usize, ((b - t) * k).ceil().max(1.0) as usize);
        let size = tiny_skia::IntSize::from_wh(u32::try_from(w).ok()?, u32::try_from(h).ok()?)?;
        budget.set(budget.get() + (w * h) as f32);
        let mut mask = Mask::new(size.width(), size.height())?;
        mask.fill_path(edges, FillRule::EvenOdd, false, Grid::to_grid(rect, k));
        let mut dist: Vec<f32> = mask.data().iter().map(|&m| if m > 127 { FAR } else { 0.0 }).collect();
        if dist.iter().any(|d| *d > 0.0) {
            edt(&mut dist, w, h);
        }
        Some(Grid { dist, w, h, rect, k })
    }

    fn to_grid(rect: [f32; 4], k: f32) -> Transform {
        Transform::from_translate(-rect[0], -rect[1]).post_scale(k, k)
    }

    /// Whether any cell is inside the polygon.
    fn inside(&self) -> bool {
        self.dist.iter().any(|d| *d > 0.0)
    }

    /// The deepest cell's distance from the edge (device pixels) among those whose middles lie in
    /// `within` (device pixels; all, for `None`) and have a cell outside on the grid.
    fn deepest(&self, within: Option<[f32; 4]>) -> f32 {
        let [l, t, ..] = self.rect;
        let mut most = 0.0f32;
        for (row, line) in self.dist.chunks_exact(self.w.max(1)).enumerate() {
            let y = t + (row as f32 + 0.5) / self.k;
            if within.is_some_and(|[_, wt, _, wb]| y < wt || y >= wb) {
                continue;
            }
            for (col, d) in line.iter().enumerate() {
                let x = l + (col as f32 + 0.5) / self.k;
                if *d < FAR && within.is_none_or(|[wl, _, wr, _]| x >= wl && x < wr) {
                    most = most.max(*d);
                }
            }
        }
        most.sqrt() / self.k
    }

    /// Shades the grid along `b`'s colours, `depth` device pixels deep (none: every cell inside at
    /// its far colour), and draws it through `path`, or for a tile, in `tile` (device pixels)
    /// through the polygon's mask over the picture.
    fn paint(&self, pm: &mut Pixmap, path: &Path, depth: f32, b: &Burst, tile: Option<(Rect, &Mask)>) {
        let mut shade: Vec<f32> = self
            .dist
            .iter()
            .map(|d| match *d {
                d if d >= FAR => 1.0,
                d if depth > 0.0 => (d.sqrt() / self.k / depth).min(1.0),
                // No depth: the far colour straight in from the edge.
                d if d > 0.0 => 1.0,
                _ => 0.0,
            })
            .collect();
        let radius = (b.blur * self.k).round() as usize;
        if radius > 0 {
            blur(&mut shade, self.w, self.h, radius);
        }
        let Some(mut pic) = Pixmap::new(self.w as u32, self.h as u32) else { return };
        for (px, s) in pic.pixels_mut().iter_mut().zip(&shade) {
            let c = fills::color_at(b.ramp, *s);
            *px = tiny_skia::ColorU8::from_rgba(c.r(), c.g(), c.b(), c.a()).premultiply();
        }
        let Some(back) = Grid::to_grid(self.rect, self.k).invert() else { return };
        let shader = tiny_skia::Pattern::new(pic.as_ref(), SpreadMode::Pad, tiny_skia::FilterQuality::Bilinear, 1.0, back);
        let paint = Paint { shader, anti_alias: crate::antialiased(), ..Paint::default() };
        match tile {
            None => pm.fill_path(path, &paint, FillRule::EvenOdd, Transform::identity(), None),
            Some((r, mask)) => pm.fill_rect(r, &paint, Transform::identity(), Some(mask)),
        }
    }
}

/// `r` grown by `m` each way and cut to `to`; `None` where nothing is left.
fn grown(r: [f32; 4], m: f32, to: [f32; 4]) -> Option<[f32; 4]> {
    let g = [(r[0] - m).max(to[0]), (r[1] - m).max(to[1]), (r[2] + m).min(to[2]), (r[3] + m).min(to[3])];
    (g[2] > g[0] && g[3] > g[1]).then_some(g)
}

/// Draws a shapeburst in `part`, shaded by each pixel's distance from the edges of `edges` (the
/// polygon, or its outer ring alone). Over the whole shape, its depth is the deepest point of the
/// part: worked out within the clip box, or where the part reaches past it, from the whole part
/// coarsely (failing that, within the clip box; with no edge within it at all, every pixel takes
/// the far colour).
fn shapeburst(pm: &mut Pixmap, part: &Part, edges: &Path, b: &Burst) {
    let path = part.path;
    // Where the distances are worked out: the polygon's box, cut to the picture and as far round
    // it as the shading reaches (the whole box for a shading over the whole shape, whose deepest
    // point may lie off screen), and inside the clip box (whose cuts are no edges). A side of the
    // box gets a cell beyond it, so the polygon's edges there are outside; a side cut has nothing
    // beyond it: no edge there, as if the polygon went on.
    let bx = path.bounds();
    let reach = b.distance.map_or(f32::INFINITY, |d| d + b.blur + 2.0);
    let (mut lo_x, mut lo_y) = (-reach, -reach);
    let (mut hi_x, mut hi_y) = (pm.width() as f32 + reach, pm.height() as f32 + reach);
    if let Some(c) = part.clip {
        (lo_x, lo_y, hi_x, hi_y) =
            (lo_x.max(c.left() + 1.0), lo_y.max(c.top() + 1.0), hi_x.min(c.right() - 1.0), hi_y.min(c.bottom() - 1.0));
    }
    let cut = [bx.left() < lo_x, bx.top() < lo_y, bx.right() > hi_x, bx.bottom() > hi_y];
    let (l, t, r, btm) = (bx.left().max(lo_x), bx.top().max(lo_y), bx.right().min(hi_x), bx.bottom().min(hi_y));
    if !(r > l && btm > t && l.is_finite() && t.is_finite() && r.is_finite() && btm.is_finite()) {
        return;
    }
    // A cell and a pixel beyond each uncut side: a cell's middle past the edge, at any scale.
    let padded = |k: f32| {
        let pad = (1.0 / k).ceil() + 1.0;
        let side = |is_cut: bool| if is_cut { 0.0 } else { pad };
        [(l - side(cut[0])).floor(), (t - side(cut[1])).floor(), (r + side(cut[2])).ceil(), (btm + side(cut[3])).ceil()]
    };
    // How deep the shading runs, given the deepest point measured within the clip box.
    let depth = |measured: f32| match b.distance {
        Some(d) => d,
        None if cut.iter().any(|c| *c) => (part.whole_depth)(b.outer_only).unwrap_or(measured),
        None => measured,
    };
    // First on one grid: coarser for a large polygon (at most MAX_BURST_PX cells), and once the
    // frame's budget is spent, coarser still.
    let fresh = part.budget.get() < FRAME_BURST_PX;
    let cap = if fresh { MAX_BURST_PX } else { MIN_BURST_PX };
    let k = (cap / ((r - l) * (btm - t))).sqrt().min(1.0);
    let Some(grid) = Grid::new(edges, padded(k), k, part.budget) else { return };
    let deep = grid.deepest(None);
    // Deep enough in cells to shade from (or at full resolution, or out of budget).
    if k >= 1.0 || !fresh || deep * k >= MIN_DEPTH_CELLS {
        grid.paint(pm, path, depth(deep), b, None);
        return;
    }
    // Too thin for that grid: at full resolution, in tiles over the box, each with a margin as
    // deep as the polygon can be (a couple of coarse cells past its deepest found) and its blur.
    let region = padded(1.0);
    let margin = (deep + 2.0 / k).min(reach) + b.blur + 2.0;
    let (cols, rows) = (((r - l) / TILE_PX).ceil().max(1.0) as usize, ((btm - t) / TILE_PX).ceil().max(1.0) as usize);
    if cols.saturating_mul(rows) > MAX_TILES {
        grid.paint(pm, path, depth(deep), b, None);
        return;
    }
    let tiles: Vec<[f32; 4]> = (0..rows)
        .flat_map(|row| (0..cols).map(move |col| (col, row)))
        .map(|(col, row)| {
            let (x, y) = (l + col as f32 * TILE_PX, t + row as f32 * TILE_PX);
            [x, y, (x + TILE_PX).min(r), (y + TILE_PX).min(btm)]
        })
        .collect();
    let grid_of = |inner: [f32; 4]| grown(inner, margin, region).and_then(|g| Grid::new(edges, g, 1.0, part.budget)).filter(Grid::inside);
    // The tiles stop at the frame's budget too: those past it are shaded from the coarse grid.
    let affordable = || part.budget.get() < FRAME_BURST_PX;
    let picture = [0.0, 0.0, pm.width() as f32, pm.height() as f32];
    let on_screen = |inner: &[f32; 4]| grown(*inner, 0.0, picture).is_some();
    // Over the whole shape: its deepest point first, over every tile of the clip box, off screen
    // too, so the depth stays put as the map pans. The tiles the budget leaves out bring in the
    // coarse grid's depth instead.
    let depth = match b.distance {
        Some(d) => d,
        None => {
            let mut skipped = false;
            let measured = tiles
                .iter()
                .filter(|_| {
                    skipped |= !affordable();
                    !skipped
                })
                .filter_map(|&inner| grid_of(inner).map(|g| g.deepest(Some(inner))))
                .fold(0.0, f32::max);
            depth(if skipped { measured.max(deep) } else { measured })
        }
    };
    // Drawn through the polygon's mask over the picture, tile by tile.
    // (Counted against the frame's budget too: a frame of many such parts goes coarse instead.)
    let Some(mut clip) = Mask::new(pm.width(), pm.height()) else { return };
    part.budget.set(part.budget.get() + pm.width() as f32 * pm.height() as f32);
    clip.fill_path(path, FillRule::EvenOdd, crate::antialiased(), Transform::identity());
    let mut fill = |area: [f32; 4], g: &Grid| {
        if let Some(rect) = grown(area, 0.0, picture).and_then(|a| Rect::from_ltrb(a[0], a[1], a[2], a[3])) {
            g.paint(pm, path, depth, b, Some((rect, &clip)));
        }
    };
    for inner in tiles.iter().copied().filter(on_screen) {
        // Past the budget, what is left is shaded from the coarse grid at once (each painting of
        // it shades its whole picture): the rest of this row of tiles, and the rows below it.
        if !affordable() {
            fill([inner[0], inner[1], r, inner[3]], &grid);
            fill([l, inner[3], r, btm], &grid);
            break;
        }
        if let Some(g) = grid_of(inner) {
            fill(inner, &g);
        }
    }
}

/// The deepest point of a polygon part's `rings` (device pixels, before any clipping; its outer
/// ring first) from its edges, worked out on a grid at most 256 cells across; `None` for one too
/// thin to tell at that scale.
pub(crate) fn whole_depth(rings: &[Vec<(f64, f64)>]) -> Option<f32> {
    let (mut x0, mut y0, mut x1, mut y1) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    for &(x, y) in rings.first()? {
        (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
    }
    let side = (x1 - x0).max(y1 - y0);
    if !(side.is_finite() && side > 0.0) {
        return None;
    }
    let s = 256.0 / side;
    let mut pb = tiny_skia::PathBuilder::new();
    for ring in rings {
        let mut pts = ring.iter().map(|&(x, y)| (((x - x0) * s + 1.0) as f32, ((y - y0) * s + 1.0) as f32));
        let Some((x, y)) = pts.next() else { continue };
        pb.move_to(x, y);
        pts.for_each(|(x, y)| pb.line_to(x, y));
        pb.close();
    }
    let path = pb.finish()?;
    let rect = [0.0, 0.0, ((x1 - x0) * s + 2.0).ceil() as f32, ((y1 - y0) * s + 2.0).ceil() as f32];
    let deep = Grid::new(&path, rect, 1.0, &Cell::new(0.0))?.deepest(None);
    (deep >= MIN_DEPTH_CELLS).then(|| (f64::from(deep) / s) as f32)
}

/// Squared Euclidean distances, in place on a `w` × `h` grid: each cell's to the nearest cell of
/// 0 (Felzenszwalb and Huttenlocher's distance transform, by columns then rows).
fn edt(d: &mut [f32], w: usize, h: usize) {
    if d.len() != w * h {
        return;
    }
    let n = w.max(h);
    let (mut f, mut out, mut v, mut z) = (vec![0.0f32; n], vec![0.0f32; n], vec![0usize; n], vec![0.0f32; n + 1]);
    for x in 0..w {
        for (y, fy) in f.iter_mut().enumerate().take(h) {
            *fy = d.get(y * w + x).copied().unwrap_or(0.0);
        }
        edt_1d(f.get(..h).unwrap_or_default(), &mut out, &mut v, &mut z);
        for (y, o) in out.iter().enumerate().take(h) {
            if let Some(c) = d.get_mut(y * w + x) {
                *c = *o;
            }
        }
    }
    for row in d.chunks_exact_mut(w) {
        f.iter_mut().zip(row.iter()).for_each(|(a, b)| *a = *b);
        edt_1d(f.get(..w).unwrap_or_default(), &mut out, &mut v, &mut z);
        row.iter_mut().zip(&out).for_each(|(a, b)| *a = *b);
    }
}

/// One line of [`edt`]: `out[q]` = min over p of (q - p)² + `f[p]`, by the lower envelope of the
/// parabolas rooted at each p. `out`, `v` and `z` are scratch at least `f.len()` (+ 1) long.
fn edt_1d(f: &[f32], out: &mut [f32], v: &mut [usize], z: &mut [f32]) {
    let n = f.len();
    if n == 0 || out.len() < n || v.len() < n || z.len() <= n {
        return;
    }
    let at = |s: &[f32], i: usize| s.get(i).copied().unwrap_or(0.0);
    // The parabola rooted at p, where it crosses the one rooted at q.
    let cross = |q: usize, p: usize| {
        let (fq, fp) = (at(f, q), at(f, p));
        ((fq + (q * q) as f32) - (fp + (p * p) as f32)) / (2.0 * (q as f32 - p as f32))
    };
    let mut k = 0usize;
    if let (Some(v0), [z0, z1, ..]) = (v.first_mut(), &mut *z) {
        *v0 = 0;
        (*z0, *z1) = (f32::NEG_INFINITY, f32::INFINITY);
    }
    for q in 1..n {
        let mut s = cross(q, v.get(k).copied().unwrap_or(0));
        while k > 0 && s <= at(z, k) {
            k -= 1;
            s = cross(q, v.get(k).copied().unwrap_or(0));
        }
        k += 1;
        if let (Some(vk), Some(zk)) = (v.get_mut(k), z.get_mut(k)) {
            *vk = q;
            *zk = s;
        }
        if let Some(z1) = z.get_mut(k + 1) {
            *z1 = f32::INFINITY;
        }
    }
    k = 0;
    for (q, o) in out.iter_mut().enumerate().take(n) {
        while at(z, k + 1) < q as f32 {
            k += 1;
        }
        let p = v.get(k).copied().unwrap_or(0);
        let dq = q as f32 - p as f32;
        *o = dq * dq + at(f, p);
    }
}

/// Blurs `s` (a `w` × `h` grid) about `radius` cells wide: three box blurs each way, close to a
/// Gaussian.
fn blur(s: &mut [f32], w: usize, h: usize, radius: usize) {
    if s.len() != w * h {
        return;
    }
    let r = (radius / 3).max(1);
    let mut line = Vec::new();
    for _ in 0..3 {
        for row in s.chunks_exact_mut(w) {
            line.clear();
            line.extend_from_slice(row);
            box_1d(&line, row, r);
        }
        let mut col = vec![0.0f32; h];
        let mut out = vec![0.0f32; h];
        for x in 0..w {
            for (y, c) in col.iter_mut().enumerate() {
                *c = s.get(y * w + x).copied().unwrap_or(0.0);
            }
            box_1d(&col, &mut out, r);
            for (y, o) in out.iter().enumerate() {
                if let Some(c) = s.get_mut(y * w + x) {
                    *c = *o;
                }
            }
        }
    }
}

/// `out[i]` = the mean of `src` within `r` of i (its ends repeated past them).
fn box_1d(src: &[f32], out: &mut [f32], r: usize) {
    let n = src.len();
    if n == 0 {
        return;
    }
    let at = |i: isize| src.get(i.clamp(0, n as isize - 1) as usize).copied().unwrap_or(0.0);
    let span = (2 * r + 1) as f32;
    let mut sum: f32 = (-(r as isize)..=r as isize).map(at).sum();
    for (i, o) in out.iter_mut().enumerate().take(n) {
        *o = sum / span;
        let i = i as isize;
        sum += at(i + r as isize + 1) - at(i - r as isize);
    }
}

/// What a gradient or shapeburst shows as when smaller than a pixel: its colours' average along
/// it (each stretch between stops by its length, blended as drawn, by premultiplied colour).
pub(crate) fn average(first: Rgba, last: Rgba, stops: &Stops, discrete: bool) -> Rgba {
    let pre = |c: Rgba| {
        let a = f32::from(c.a()) / 255.0;
        [f32::from(c.r()) * a, f32::from(c.g()) * a, f32::from(c.b()) * a, a]
    };
    // Along the ramp as it is drawn: a discrete one holds each colour up to the next stop.
    let mut sum = [0.0f32; 4];
    for w in fills::ramp(first, last, stops, discrete).windows(2) {
        let [p, s] = w else { continue };
        let span = (s.at - p.at).max(0.0);
        let (a, b) = (pre(p.color), pre(s.color));
        for ((o, x), y) in sum.iter_mut().zip(a).zip(b) {
            *o += span * (x + y) / 2.0;
        }
    }
    if sum[3] <= 0.0 {
        return Rgba([0; 4]);
    }
    let byte = |v: f32| v.round().clamp(0.0, 255.0) as u8;
    Rgba::new(byte(sum[0] / sum[3]), byte(sum[1] / sum[3]), byte(sum[2] / sum[3]), byte(sum[3] * 255.0))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use rustgis_map::fills::Stop;

    /// The distance transform gives each cell its exact distance to the nearest zero.
    #[test]
    fn distances_are_exact() {
        let (w, h) = (7, 5);
        let mut d = vec![1e20f32; w * h];
        d[0] = 0.0;
        d[2 * w + 6] = 0.0;
        edt(&mut d, w, h);
        for y in 0..h {
            for x in 0..w {
                let a = (x * x + y * y) as f32;
                let b = ((6 - x as isize).pow(2) + (2 - y as isize).pow(2)) as f32;
                assert_eq!(d[y * w + x], a.min(b), "({x}, {y})");
            }
        }
    }

    /// With the frame's budget spent, a shapeburst is still shaded (on a coarser grid): its
    /// colours held where discrete, red to a quarter in, green past it, blue in the middle.
    #[test]
    fn a_shapeburst_past_the_frames_budget_still_shades() {
        let mut pm = Pixmap::new(400, 400).unwrap();
        let path = tiny_skia::PathBuilder::from_rect(Rect::from_ltrb(20.0, 20.0, 380.0, 380.0).unwrap());
        let budget = Cell::new(FRAME_BURST_PX + 1.0);
        let none = |_: bool| None;
        let part = Part { path: &path, whole: None, centroid: None, clip: None, whole_depth: &none, budget: &budget };
        let (red, green, blue) = (Rgba::rgb(255, 0, 0), Rgba::rgb(0, 255, 0), Rgba::rgb(0, 0, 255));
        let pattern = FillPattern::Shapeburst {
            color2: blue,
            stops: Stops::new([Stop { at: 0.5, color: green }]),
            discrete: true,
            distance: None,
            blur: 0,
            ignore_rings: false,
        };
        assert!(crate::canvas::with_canvas(&mut pm, |c| draw(c, &part, || None, &pattern, red, 1.0)));
        let at = |x: u32| pm.pixel(x, 200).map(|p| (p.red(), p.green(), p.blue())).unwrap();
        assert_eq!(at(30), (255, 0, 0), "near the edge");
        assert_eq!(at(150), (0, 255, 0), "past halfway in");
        assert_eq!(at(200), (0, 0, 255), "the middle");
        assert!(budget.get() > FRAME_BURST_PX + 1.0 && budget.get() < FRAME_BURST_PX + 1.0 + 2.0 * MIN_BURST_PX, "{}", budget.get());
    }

    /// A thin polygon over a large box, drawn in tiles at full resolution, stops building tiles
    /// at the frame's budget: past it, its tiles are shaded from the coarse grid, and still drawn.
    #[test]
    fn a_thin_shapeburst_stops_at_the_frames_budget() {
        let mut pm = Pixmap::new(2000, 2000).unwrap();
        let mut pb = tiny_skia::PathBuilder::new();
        pb.move_to(0.0, 6.0);
        pb.line_to(6.0, 0.0);
        pb.line_to(2000.0, 1994.0);
        pb.line_to(1994.0, 2000.0);
        pb.close();
        let path = pb.finish().unwrap();
        let budget = Cell::new(FRAME_BURST_PX - 1.0);
        let none = |_: bool| None;
        let part = Part { path: &path, whole: None, centroid: None, clip: None, whole_depth: &none, budget: &budget };
        let (red, blue) = (Rgba::rgb(255, 0, 0), Rgba::rgb(0, 0, 255));
        let pattern = FillPattern::Shapeburst {
            color2: blue,
            stops: Stops::default(),
            discrete: false,
            distance: None,
            blur: 0,
            ignore_rings: false,
        };
        assert!(crate::canvas::with_canvas(&mut pm, |c| draw(c, &part, || None, &pattern, red, 1.0)));
        // One coarse grid (padded a little) and the mask, no full-resolution tile (each about
        // 300,000 cells, sixteen of them).
        let picture = 2000.0 * 2000.0;
        assert!(budget.get() < FRAME_BURST_PX + 1.1 * MAX_BURST_PX + picture, "{}", budget.get() - FRAME_BURST_PX);
        assert!(pm.pixel(1000, 1000).is_some_and(|p| p.alpha() > 0), "drawn");
        // Running out part way through the painting: what each pass costs, measured on a fresh
        // budget (with a depth of its own, no depth pass over the tiles), then a budget that runs
        // out halfway through the painted tiles.
        let cost = |start: f32, pattern: &FillPattern| {
            let mut pm = Pixmap::new(2000, 2000).unwrap();
            let budget = Cell::new(start);
            let part = Part { path: &path, whole: None, centroid: None, clip: None, whole_depth: &none, budget: &budget };
            assert!(crate::canvas::with_canvas(&mut pm, |c| draw(c, &part, || None, pattern, red, 1.0)));
            (budget.get() - start, pm)
        };
        let deep = FillPattern::Shapeburst {
            color2: blue,
            stops: Stops::default(),
            discrete: false,
            distance: Some(5.0),
            blur: 0,
            ignore_rings: false,
        };
        let (all, _) = cost(0.0, &pattern);
        let tiles = all - cost(0.0, &deep).0;
        let before_painting = all - tiles;
        let (spent, pm) = cost(FRAME_BURST_PX - before_painting - tiles / 2.0, &pattern);
        assert!(spent > before_painting && spent < all, "{spent} of {all}: some tiles painted at full resolution, not all");
        for at in [20, 500, 1000, 1500, 1980] {
            assert!(pm.pixel(at, at).is_some_and(|p| p.alpha() > 0), "drawn at {at}");
        }
    }

    /// A ramp's average: by the length of each stretch, premultiplied.
    #[test]
    fn a_ramps_average_weighs_each_stretch() {
        let (red, blue) = (Rgba::rgb(255, 0, 0), Rgba::rgb(0, 0, 255));
        assert_eq!(average(red, blue, &Stops::default(), false), Rgba::rgb(128, 0, 128));
        let mostly_red = Stops::new([Stop { at: 0.9, color: red }]);
        assert_eq!(average(red, blue, &mostly_red, false), Rgba::rgb(242, 0, 13));
        assert_eq!(average(Rgba::new(0, 0, 0, 0), Rgba::new(0, 0, 0, 0), &Stops::default(), false), Rgba::new(0, 0, 0, 0));
        // A discrete one as it is drawn: red, then green from halfway, and no blue at all.
        let green = Stops::new([Stop { at: 0.5, color: Rgba::rgb(0, 255, 0) }]);
        assert_eq!(average(red, blue, &green, true), Rgba::rgb(128, 128, 0));
    }

    /// A box blur keeps a flat field flat and spreads a step.
    #[test]
    fn blur_keeps_flat_and_softens_steps() {
        let (w, h) = (9, 3);
        let mut s = vec![0.5f32; w * h];
        blur(&mut s, w, h, 3);
        assert!(s.iter().all(|v| (v - 0.5).abs() < 1e-6));
        let mut s: Vec<f32> = (0..w * h).map(|i| if i % w < 4 { 0.0 } else { 1.0 }).collect();
        blur(&mut s, w, h, 3);
        assert!(s[3] > 0.0 && s[4] < 1.0, "{s:?}");
    }
}
