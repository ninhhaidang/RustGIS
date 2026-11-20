//! A layer's features drawn in horizontal bands of the frame, on several threads (#1067).
//!
//! While a [`Canvas`](crate::canvas::Canvas) keeps its drawing for bands, the fills, strokes and
//! pixmaps it is given, and the pixel shortcuts the renderer takes for small shapes and markers
//! ([`PixelOp`]), wait in a list ([`Bands`]) instead of being drawn. The list is then drawn band by
//! band, the bands shared among threads: each band draws, in order, everything that reaches its
//! rows, moved up by the band's top. Strokes wider than a hairline are stroked once for all bands,
//! as tiny-skia strokes them (`Path::stroke`, then a fill), on several threads too.
//!
//! The bands are [`BAND_ROWS`] rows each, whatever the number of threads, so a frame comes out the
//! same, byte for byte, on one thread or many. It isn't byte for byte what drawing the whole frame
//! at once gives: tiny-skia starts an edge again where a band cuts it, as it does at the frame's own
//! edges, which moves an anti-aliased edge's coverage there (a hairline's most, a fill's least). The
//! pixel shortcuts are drawn exactly as on the whole frame.
//!
//! Only frames of at least [`MIN_PIXELS`] are drawn in bands, never in the browser (one thread).
//! What waits is at most about [`MAX_KEPT_BYTES`]: past it, it is drawn and the list starts again,
//! and a thing bigger than that alone is drawn straight away.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use rustgis_map::Rgba;
use tiny_skia::{FillRule, Paint, Path, PathStroker, Pixmap, PixmapMut, PixmapPaint, Point, Rect, Shader, Stroke, Transform};

/// Rows of the frame in a band.
pub(crate) const BAND_ROWS: u32 = 128;

/// The fewest pixels a frame drawn in bands has (a 1024 × 1024 one).
pub(crate) const MIN_PIXELS: u64 = 1 << 20;

/// About the most bytes of drawing kept before it is drawn: each kept thing's own size, its path's
/// points (a dashed stroke's dashes), a pixmap's pixels (a shared one's once), and the outline a
/// wide stroke becomes ([`OUTLINE_GROWTH`]).
const MAX_KEPT_BYTES: usize = 32 << 20;

/// How many times a path's size its outline is counted at, for a stroke wider than a hairline:
/// both sides, and round joins and caps, take several times the points of the path.
const OUTLINE_GROWTH: usize = 8;

/// What a kept thing takes itself, before its path or pixels.
const ENTRY_BYTES: usize = std::mem::size_of::<Kept>();

/// Drawing that reaches fewer pixels than this, summed over what is kept, is drawn on the calling
/// thread alone (still in bands): starting threads would cost more than they save. A kept thing
/// counts the rows and columns it reaches, and a [`PER_OP_PIXELS`] more for its own setup.
const THREADS_FROM_PIXELS: u64 = 1 << 20;

/// What a kept thing's setup (its edges, its paint's pipeline) counts in pixels.
const PER_OP_PIXELS: u64 = 256;

/// What a band drawing a path takes beside it, as a multiple of the path's size ([`path_bytes`],
/// about 9 bytes a point): tiny-skia moves a copy of it to the band (any transform but none at all;
/// a second one on a band wider than 8191 pixels), and builds its edge list, about 80 bytes a
/// segment, up to twice that while the list grows.
const SCRATCH_GROWTH: usize = 20;

/// How many kept things a band draws between looks at Cancel ([`crate::cancel`]).
const LOOK_EVERY: usize = 256;

/// Whether a frame `width` × `height` pixels is drawn in bands: a large one, not in the browser.
pub(crate) fn applies(width: u32, height: u32) -> bool {
    !cfg!(target_arch = "wasm32") && height >= 2 * BAND_ROWS && u64::from(width) * u64::from(height) >= MIN_PIXELS
}

/// Drawing done straight on the pixels, for shapes and markers too small or too many for paths.
pub(crate) enum PixelOp {
    /// [`crate::splat_color`]: `color` at `coverage` around (`cx`, `cy`).
    Splat { cx: f32, cy: f32, color: Rgba, coverage: f32 },
    /// [`crate::small_polygon_pixels`]: a tiny box `b` and its outline.
    SmallPolygon { b: [f32; 4], width: f32, fill: Rgba, stroke: Rgba, share: f32 },
    /// [`crate::blend_over`]: a marker sprite with its top left at (`x`, `y`).
    Sprite { pixmap: Arc<Pixmap>, x: i64, y: i64 },
}

impl PixelOp {
    /// Draws it on `pm`, whose first row is the frame's row `top`. The positions are moved up by
    /// whole rows, exactly, so a band's pixels are those of the whole frame.
    pub(crate) fn apply(&self, pm: &mut PixmapMut, top: u32) {
        match self {
            PixelOp::Splat { cx, cy, color, coverage } => crate::splat_color(pm, *cx, cy - top as f32, *color, *coverage),
            PixelOp::SmallPolygon { b, width, fill, stroke, share } => {
                crate::small_polygon_pixels(pm, top, *b, *width, *fill, *stroke, *share)
            }
            PixelOp::Sprite { pixmap, x, y } => crate::blend_over(pm, pixmap, *x, y - i64::from(top)),
        }
    }

    /// The frame rows it can touch, a little wide, and how many columns.
    fn reach(&self) -> ((f32, f32), f32) {
        match self {
            PixelOp::Splat { cy, .. } => ((cy - 2.0, cy + 2.0), 4.0),
            PixelOp::SmallPolygon { b, width, .. } => ((b[1] - width - 2.0, b[3] + width + 2.0), b[2] - b[0] + 2.0 * width + 4.0),
            PixelOp::Sprite { pixmap, y, .. } => {
                ((*y as f32 - 1.0, (*y + i64::from(pixmap.height())) as f32 + 1.0), pixmap.width() as f32 + 2.0)
            }
        }
    }
}

/// Something kept to be drawn.
enum Op {
    Fill {
        path: Path,
        paint: Paint<'static>,
        rule: FillRule,
        ts: Transform,
    },
    Stroke {
        path: Path,
        paint: Paint<'static>,
        stroke: Stroke,
        ts: Transform,
    },
    Rect {
        rect: Rect,
        paint: Paint<'static>,
        ts: Transform,
    },
    Pixmap {
        x: i32,
        y: i32,
        /// Shared by every point of a picture marker layer.
        pixmap: Arc<Pixmap>,
        paint: PixmapPaint,
        ts: Transform,
    },
    Pixels(PixelOp),
    /// A stroke that draws nothing (one tiny-skia refuses to stroke or dash).
    Nothing,
}

/// One thing kept, the frame rows it can reach (a band past them skips it; a reach that isn't a
/// number reaches every band), and how many columns (for the work it is: not a number, the frame's
/// width).
struct Kept {
    op: Op,
    rows: (f32, f32),
    cols: f32,
}

/// The drawing a canvas keeps for its bands (see the module).
#[derive(Default)]
pub(crate) struct Bands {
    kept: Vec<Kept>,
    bytes: usize,
    /// The shared pixmaps kept (by address, each alive in `kept`), counted once each: a cache may
    /// let one go while it waits here.
    shared: HashSet<usize>,
}

impl Bands {
    /// The bytes kept.
    #[cfg(test)]
    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }

    /// Whether what is kept has grown past [`MAX_KEPT_BYTES`]: time to draw it.
    pub(crate) fn full(&self) -> bool {
        self.bytes >= MAX_KEPT_BYTES
    }

    /// Keeps `path` filled with `paint`; `false` (nothing kept: draw it straight away) for a paint
    /// that can't be kept, a pattern (it borrows its pixmap), or a path bigger than all that may
    /// wait ([`MAX_KEPT_BYTES`]).
    pub(crate) fn fill(&mut self, path: &Path, paint: &Paint, rule: FillRule, ts: Transform) -> bool {
        let bytes = ENTRY_BYTES + path_bytes(path);
        if bytes > MAX_KEPT_BYTES {
            return false;
        }
        let Some(paint) = owned(paint) else { return false };
        let (rows, cols) = rows_of(path.bounds(), ts, 0.0);
        self.bytes += bytes;
        self.kept.push(Kept { op: Op::Fill { path: path.clone(), paint, rule, ts }, rows, cols });
        true
    }

    /// Keeps `path` stroked; `false` as for [`Bands::fill`]. A dashed stroke is kept dashed (as
    /// tiny-skia dashes it, before stroking), so its dashes count in full.
    pub(crate) fn stroke(&mut self, path: &Path, paint: &Paint, stroke: &Stroke, ts: Transform) -> bool {
        let growth = if hairline(paint, stroke, ts) { 1 } else { OUTLINE_GROWTH };
        if ENTRY_BYTES + path_bytes(path) * growth > MAX_KEPT_BYTES {
            return false;
        }
        let Some(paint) = owned(paint) else { return false };
        let (path, stroke) = match &stroke.dash {
            // tiny-skia draws nothing for a negative width, or for a path it can't dash.
            _ if stroke.width < 0.0 => return true,
            Some(dash) => {
                let Some(dashed) = path.dash(dash, PathStroker::compute_resolution_scale(&ts)) else { return true };
                (dashed, Stroke { dash: None, ..stroke.clone() })
            }
            None => (path.clone(), stroke.clone()),
        };
        let bytes = ENTRY_BYTES + path_bytes(&path) * growth;
        if bytes > MAX_KEPT_BYTES {
            return false;
        }
        // Half the width past the path, a mitre further.
        let (sx, sy) = ts.get_scale();
        let reach = stroke.width.max(1.0) * stroke.miter_limit.max(1.0) * sx.max(sy).max(1.0);
        let (rows, cols) = rows_of(path.bounds(), ts, reach);
        self.bytes += bytes;
        self.kept.push(Kept { op: Op::Stroke { path, paint, stroke, ts }, rows, cols });
        true
    }

    /// Keeps `rect` filled; `false` as for [`Bands::fill`].
    pub(crate) fn rect(&mut self, rect: Rect, paint: &Paint, ts: Transform) -> bool {
        let Some(paint) = owned(paint) else { return false };
        let (rows, cols) = rows_of(rect, ts, 0.0);
        self.bytes += ENTRY_BYTES;
        self.kept.push(Kept { op: Op::Rect { rect, paint, ts }, rows, cols });
        true
    }

    /// Keeps a copy of `pixmap` drawn with its top left at (`x`, `y`) moved by `ts`; `false` (draw
    /// it straight away) for one bigger than all that may wait.
    pub(crate) fn pixmap(&mut self, x: i32, y: i32, pixmap: tiny_skia::PixmapRef, paint: &PixmapPaint, ts: Transform) -> bool {
        if ENTRY_BYTES + pixmap.data().len() > MAX_KEPT_BYTES {
            return false;
        }
        self.bytes += pixmap.data().len();
        self.keep_pixmap(x, y, Arc::new(pixmap.to_owned()), paint, ts);
        true
    }

    /// Keeps `pixmap`, drawn in many places (a picture at every point), moved by `ts`: shared, not
    /// copied, its pixels counted once.
    pub(crate) fn shared_pixmap(&mut self, pixmap: &Arc<Pixmap>, paint: &PixmapPaint, ts: Transform) {
        self.count_shared(pixmap);
        self.keep_pixmap(0, 0, pixmap.clone(), paint, ts);
    }

    /// Counts a shared pixmap's pixels the first time it is kept.
    fn count_shared(&mut self, pixmap: &Arc<Pixmap>) {
        if self.shared.insert(Arc::as_ptr(pixmap) as usize) {
            self.bytes += pixmap.data().len();
        }
    }

    fn keep_pixmap(&mut self, x: i32, y: i32, pixmap: Arc<Pixmap>, paint: &PixmapPaint, ts: Transform) {
        let at = ts.pre_translate(x as f32, y as f32);
        let (rows, cols) = Rect::from_xywh(0.0, 0.0, pixmap.width() as f32, pixmap.height() as f32)
            .map_or(((f32::NAN, f32::NAN), f32::NAN), |r| rows_of(r, at, 0.0));
        self.bytes += ENTRY_BYTES;
        self.kept.push(Kept { op: Op::Pixmap { x, y, pixmap, paint: *paint, ts }, rows, cols });
    }

    /// Keeps a pixel shortcut.
    pub(crate) fn pixels(&mut self, op: PixelOp) {
        if let PixelOp::Sprite { pixmap, .. } = &op {
            self.count_shared(pixmap);
        }
        let (rows, cols) = op.reach();
        self.bytes += ENTRY_BYTES;
        self.kept.push(Kept { op: Op::Pixels(op), rows, cols });
    }

    /// Draws everything kept on `pm`, in bands, on up to the user's thread count
    /// ([`rustgis_data::threads::max`]), and forgets it. A frame asked to stop ([`crate::cancel`])
    /// draws none of it, or stops part-way: it is no frame.
    pub(crate) fn draw(&mut self, pm: &mut Pixmap) {
        self.draw_on(pm, rustgis_data::threads::max());
    }

    /// [`Bands::draw`] on at most `most` threads.
    fn draw_on(&mut self, pm: &mut Pixmap, most: usize) {
        if self.kept.is_empty() {
            return;
        }
        let mut kept = std::mem::take(&mut self.kept);
        self.bytes = 0;
        self.shared.clear();
        let stop = crate::cancel::current();
        let stopped = || stop.as_ref().is_some_and(|s| s.load(Ordering::Relaxed));
        if stopped() {
            return;
        }
        let (w, h) = (pm.width(), pm.height());
        let bands = h.div_ceil(BAND_ROWS) as usize;
        let scratch = rustgis_data::memory::thread_scratch();
        let threads = threads_for(&kept, w, h, most, scratch);
        stroke_wide(&mut kept, threads, &stopped);
        if stopped() {
            return;
        }
        // Again with the wide strokes' outlines as made, for the bands.
        let threads = threads.min(threads_for(&kept, w, h, most, scratch));
        // Each band's kept things, in order, so a band visits only what reaches it.
        let mut lists: Vec<Vec<u32>> = vec![Vec::new(); bands];
        for (n, k) in kept.iter().enumerate() {
            let Ok(n) = u32::try_from(n) else { break };
            let last = bands.saturating_sub(1);
            // A reach that isn't a number reaches every band.
            let (first, end) = if k.rows.0.is_nan() || k.rows.1.is_nan() {
                (0, last)
            } else {
                let band_of = |row: f32| ((row.max(0.0) / BAND_ROWS as f32) as usize).min(last);
                if k.rows.1 < 0.0 || k.rows.0 > h as f32 {
                    continue;
                }
                (band_of(k.rows.0), band_of(k.rows.1))
            };
            for list in lists.iter_mut().take(end + 1).skip(first) {
                list.push(n);
            }
        }
        let stride = w as usize * 4;
        let slots: Vec<std::sync::Mutex<Option<&mut [u8]>>> =
            pm.data_mut().chunks_mut(BAND_ROWS as usize * stride).map(|rows| std::sync::Mutex::new(Some(rows))).collect();
        let (kept, lists) = (&kept, &lists);
        on_threads(slots.len(), threads, |i| {
            let Some(data) = slots.get(i).and_then(|s| s.lock().ok().and_then(|mut s| s.take())) else { return };
            let rows = data.len() / stride.max(1);
            let Some(mut band) = PixmapMut::from_bytes(data, w, rows as u32) else { return };
            let top = i as u32 * BAND_ROWS;
            for (n, k) in lists.get(i).into_iter().flatten().enumerate() {
                if n % LOOK_EVERY == LOOK_EVERY - 1 && stopped() {
                    return;
                }
                if let Some(k) = kept.get(*k as usize) {
                    draw_op(&mut band, &k.op, top);
                }
            }
        });
    }
}

/// How many threads draw `kept` on a `w` × `h` frame, at most `most`: one below
/// [`THREADS_FROM_PIXELS`] of work, counted as the pixels each kept thing reaches; and no more than
/// `scratch` bytes ([`rustgis_data::memory::thread_scratch`]) hold, beyond one thread's, when each
/// band draws the biggest kept path at once ([`SCRATCH_GROWTH`]; a wide stroke not yet outlined as
/// [`OUTLINE_GROWTH`] times its path).
fn threads_for(kept: &[Kept], w: u32, h: u32, most: usize, scratch: usize) -> usize {
    let reach = |k: &Kept| -> u64 {
        let rows = (k.rows.1.min(h as f32) - k.rows.0.max(0.0)).max(0.0);
        let rows = if rows.is_finite() { rows as u64 } else { u64::from(h) };
        let cols = if k.cols.is_finite() { k.cols.clamp(0.0, w as f32) as u64 } else { u64::from(w) };
        rows * cols + PER_OP_PIXELS
    };
    let work: u64 = kept.iter().map(reach).fold(0, u64::saturating_add);
    if work < THREADS_FROM_PIXELS {
        return 1;
    }
    let biggest = kept
        .iter()
        .map(|k| match &k.op {
            Op::Fill { path, .. } => path_bytes(path),
            Op::Stroke { path, paint, stroke, ts } => path_bytes(path) * if hairline(paint, stroke, *ts) { 1 } else { OUTLINE_GROWTH },
            _ => 0,
        })
        .max()
        .unwrap_or(0);
    let affordable = (scratch / biggest.saturating_mul(SCRATCH_GROWTH).max(1)).saturating_add(1);
    most.min(affordable).clamp(1, h.div_ceil(BAND_ROWS).max(1) as usize)
}

/// Draws `op` on `band`, whose first row is the frame's row `top`.
fn draw_op(band: &mut PixmapMut, op: &Op, top: u32) {
    let up = -(top as f32);
    match op {
        Op::Fill { path, paint, rule, ts } => band.fill_path(path, paint, *rule, ts.post_translate(0.0, up), None),
        Op::Stroke { path, paint, stroke, ts } => band.stroke_path(path, paint, stroke, ts.post_translate(0.0, up), None),
        // Without a transform, tiny-skia fills a rectangle as one (not as a path): moved itself. Only
        // in one colour: a gradient would stay where it was.
        Op::Rect { rect, paint, ts } if ts.is_identity() && matches!(paint.shader, Shader::SolidColor(_)) => {
            if let Some(r) = Rect::from_xywh(rect.x(), rect.y() + up, rect.width(), rect.height()) {
                band.fill_rect(r, paint, Transform::identity(), None);
            }
        }
        Op::Rect { rect, paint, ts } => band.fill_rect(*rect, paint, ts.post_translate(0.0, up), None),
        Op::Pixmap { x, y, pixmap, paint, ts } if ts.is_identity() => {
            band.draw_pixmap(*x, y.saturating_sub_unsigned(top), Pixmap::as_ref(pixmap), paint, Transform::identity(), None)
        }
        Op::Pixmap { x, y, pixmap, paint, ts } => band.draw_pixmap(*x, *y, Pixmap::as_ref(pixmap), paint, ts.post_translate(0.0, up), None),
        Op::Pixels(p) => p.apply(band, top),
        Op::Nothing => {}
    }
}

/// Strokes wider than a hairline turned into the fills tiny-skia would draw for them
/// (`PixmapMut::stroke_path`: dashed, as [`Bands::stroke`] keeps them, then `Path::stroke`, then
/// filled nonzero), once for every band; on `threads` threads, looking at `stopped` every
/// [`LOOK_EVERY`] strokes.
fn stroke_wide(kept: &mut [Kept], threads: usize, stopped: &(dyn Fn() -> bool + Sync)) {
    let chunk = kept.len().div_ceil(threads.max(1) * 4).max(1);
    let slots: Vec<std::sync::Mutex<Option<&mut [Kept]>>> = kept.chunks_mut(chunk).map(|c| std::sync::Mutex::new(Some(c))).collect();
    on_threads(slots.len(), threads, |i| {
        let Some(part) = slots.get(i).and_then(|s| s.lock().ok().and_then(|mut s| s.take())) else { return };
        for (n, k) in part.iter_mut().enumerate() {
            if n % LOOK_EVERY == LOOK_EVERY - 1 && stopped() {
                return;
            }
            let Op::Stroke { path, paint, stroke, ts } = &k.op else { continue };
            if hairline(paint, stroke, *ts) {
                continue;
            }
            let Some(outline) = path.stroke(stroke, PathStroker::compute_resolution_scale(ts)) else {
                k.op = Op::Nothing;
                continue;
            };
            (k.rows, k.cols) = rows_of(outline.bounds(), *ts, 0.0);
            k.op = Op::Fill { path: outline, paint: paint.clone(), rule: FillRule::Winding, ts: *ts };
        }
    });
}

/// Whether tiny-skia draws a stroke as a hairline (its `treat_as_hairline`): a zero width, or an
/// anti-aliased one at most a pixel wide once transformed.
fn hairline(paint: &Paint, stroke: &Stroke, ts: Transform) -> bool {
    if stroke.width == 0.0 {
        return true;
    }
    if !paint.anti_alias {
        return false;
    }
    let turn = Transform::from_row(ts.sx, ts.ky, ts.kx, ts.sy, 0.0, 0.0);
    let mut points = [Point::from_xy(stroke.width, 0.0), Point::from_xy(0.0, stroke.width)];
    turn.map_points(&mut points);
    let fast_len = |p: Point| {
        let (x, y) = (p.x.abs(), p.y.abs());
        let (long, short) = if x < y { (y, x) } else { (x, y) };
        long + short * 0.5
    };
    fast_len(points[0]) <= 1.0 && fast_len(points[1]) <= 1.0
}

/// The frame rows `rect` reaches once moved by `ts`, `reach` further each way and a pixel more
/// for anti-aliasing, and how many columns, as far.
fn rows_of(rect: Rect, ts: Transform, reach: f32) -> ((f32, f32), f32) {
    let mut corners = [
        Point::from_xy(rect.left(), rect.top()),
        Point::from_xy(rect.right(), rect.top()),
        Point::from_xy(rect.left(), rect.bottom()),
        Point::from_xy(rect.right(), rect.bottom()),
    ];
    ts.map_points(&mut corners);
    let span =
        |at: fn(&Point) -> f32| corners.iter().map(at).fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), v| (lo.min(v), hi.max(v)));
    let ((lo, hi), (left, right)) = (span(|p| p.y), span(|p| p.x));
    if !(lo.is_finite() && hi.is_finite()) {
        return ((f32::NAN, f32::NAN), f32::NAN);
    }
    ((lo - reach - 1.0, hi + reach + 1.0), right - left + 2.0 * (reach + 1.0))
}

fn path_bytes(path: &Path) -> usize {
    path.points().len() * 8 + path.verbs().len() + 64
}

/// `paint` with a shader it owns; `None` for a pattern, which borrows its pixmap.
fn owned(paint: &Paint) -> Option<Paint<'static>> {
    let shader = match &paint.shader {
        Shader::SolidColor(c) => Shader::SolidColor(*c),
        Shader::LinearGradient(g) => Shader::LinearGradient(g.clone()),
        Shader::RadialGradient(g) => Shader::RadialGradient(g.clone()),
        Shader::SweepGradient(g) => Shader::SweepGradient(g.clone()),
        Shader::Pattern(_) => return None,
    };
    Some(Paint {
        shader,
        blend_mode: paint.blend_mode,
        anti_alias: paint.anti_alias,
        colorspace: paint.colorspace,
        force_hq_pipeline: paint.force_hq_pipeline,
    })
}

/// Runs `f` on each of `0..count`, shared among `threads` threads (this one among them). A thread
/// the system refuses costs speed: the others, and this one, take its share.
fn on_threads(count: usize, threads: usize, f: impl Fn(usize) + Sync) {
    let next = AtomicUsize::new(0);
    let work = || {
        loop {
            let i = next.fetch_add(1, Ordering::Relaxed);
            if i >= count {
                break;
            }
            f(i);
        }
    };
    if threads <= 1 || count <= 1 {
        work();
        return;
    }
    std::thread::scope(|scope| {
        for _ in 1..threads.min(count) {
            let _ = std::thread::Builder::new().name("rustgis-bands".into()).spawn_scoped(scope, work);
        }
        work();
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::Canvas;
    use tiny_skia::{Color, LineCap, LineJoin, PathBuilder, StrokeDash};

    /// A small deterministic generator, 0..1.
    struct Rng(u64);

    impl Rng {
        fn f(&mut self) -> f32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 40) as f32 / (1u64 << 24) as f32
        }
    }

    fn solid(r: u8, g: u8, b: u8, a: u8) -> Paint<'static> {
        let mut p = Paint::default();
        p.set_color_rgba8(r, g, b, a);
        p.anti_alias = true;
        p
    }

    /// Everything a layer keeps, of every kind, over a `w` × `h` frame: small and frame-sized
    /// fills, hairlines, wide, mitred and dashed strokes, a gradient, rectangles, pixmaps (one
    /// turned), sprites, splats and tiny boxes, many crossing bands.
    fn a_layers_drawing(w: f32, h: f32, hairlines: bool) -> Bands {
        let mut r = Rng(0x9e37_79b9_7f4a_7c15);
        let mut bands = Bands::default();
        let sprite = {
            let mut sp = Pixmap::new(9, 9).unwrap();
            sp.fill_path(
                &PathBuilder::from_circle(4.5, 4.5, 3.6).unwrap(),
                &solid(200, 40, 40, 255),
                FillRule::Winding,
                Transform::identity(),
                None,
            );
            Arc::new(sp)
        };
        for i in 0..600 {
            let (cx, cy) = (r.f() * w * 1.2 - 0.1 * w, r.f() * h * 1.2 - 0.1 * h);
            let size = if i % 25 == 0 { r.f() * h * 2.0 } else { 2.0 + r.f() * 60.0 };
            let mut pb = PathBuilder::new();
            pb.move_to(cx, cy);
            for _ in 0..(3 + i % 7) {
                pb.line_to(cx + (r.f() - 0.5) * size, cy + (r.f() - 0.5) * size);
            }
            if i % 2 == 0 {
                pb.close();
            }
            let Some(path) = pb.finish() else { continue };
            let paint = solid((r.f() * 255.0) as u8, (r.f() * 255.0) as u8, (r.f() * 255.0) as u8, (60.0 + r.f() * 195.0) as u8);
            match i % 9 {
                0..=2 => {
                    assert!(bands.fill(
                        &path,
                        &paint,
                        if i % 2 == 0 { FillRule::EvenOdd } else { FillRule::Winding },
                        Transform::identity()
                    ));
                }
                3 if hairlines => {
                    let stroke = Stroke { width: 0.3 + r.f() * 0.6, ..Stroke::default() };
                    assert!(bands.stroke(&path, &paint, &stroke, Transform::identity()));
                }
                3 | 4 => {
                    let join = [LineJoin::Round, LineJoin::Miter, LineJoin::Bevel][i % 3];
                    let cap = [LineCap::Round, LineCap::Square, LineCap::Butt][i % 3];
                    let stroke = Stroke { width: 1.5 + r.f() * 9.0, line_join: join, line_cap: cap, miter_limit: 6.0, ..Stroke::default() };
                    assert!(bands.stroke(&path, &paint, &stroke, Transform::identity()));
                }
                5 => {
                    let stroke = Stroke { width: 2.5, dash: StrokeDash::new(vec![6.0, 3.0], 1.0), ..Stroke::default() };
                    assert!(bands.stroke(&path, &paint, &stroke, Transform::from_row(1.1, 0.2, -0.1, 0.9, 5.0, -7.0)));
                }
                6 => {
                    let g = tiny_skia::LinearGradient::new(
                        Point::from_xy(cx, cy - 40.0),
                        Point::from_xy(cx, cy + 40.0),
                        vec![
                            tiny_skia::GradientStop::new(0.0, Color::from_rgba8(0, 90, 200, 255)),
                            tiny_skia::GradientStop::new(1.0, Color::from_rgba8(250, 200, 0, 120)),
                        ],
                        tiny_skia::SpreadMode::Pad,
                        Transform::identity(),
                    )
                    .unwrap();
                    let paint = Paint { shader: g, anti_alias: true, ..Paint::default() };
                    assert!(bands.fill(&path, &paint, FillRule::Winding, Transform::identity()));
                    assert!(bands.rect(Rect::from_xywh(cx, cy, 30.0, 70.0).unwrap(), &paint, Transform::identity()));
                }
                7 => {
                    let at = if i % 2 == 0 { Transform::identity() } else { Transform::from_rotate_at(17.0, cx, cy) };
                    assert!(bands.pixmap(
                        cx as i32,
                        cy as i32,
                        sprite.as_ref().as_ref(),
                        &PixmapPaint { opacity: 0.8, ..PixmapPaint::default() },
                        at,
                    ));
                }
                _ => {
                    let color = Rgba::new(10, (r.f() * 255.0) as u8, 90, 230);
                    bands.pixels(PixelOp::Splat { cx, cy, color, coverage: r.f() * 2.0 });
                    let b = [cx, cy, cx + 0.5 + r.f() * 1.4, cy + 0.5 + r.f() * 1.4];
                    bands.pixels(PixelOp::SmallPolygon { b, width: 1.4, fill: color, stroke: Rgba::new(0, 0, 0, 255), share: 0.9 });
                    bands.pixels(PixelOp::Sprite { pixmap: sprite.clone(), x: cx as i64 - 4, y: cy as i64 - 4 });
                }
            }
        }
        bands
    }

    fn drawn(threads: usize) -> Pixmap {
        let (w, h) = (1031, 1093);
        let mut pm = Pixmap::new(w, h).unwrap();
        pm.fill(Color::from_rgba8(235, 232, 220, 255));
        a_layers_drawing(w as f32, h as f32, true).draw_on(&mut pm, threads);
        pm
    }

    /// Drawn in bands, a layer differs from the whole frame drawn at once only where tiny-skia
    /// starts an edge again at a band's top: little, and nothing left out. Without hairlines, whose
    /// one-pixel steps move along the band, no pixel differs by more than an anti-aliased edge's
    /// rounding.
    #[test]
    fn bands_differ_from_the_whole_frame_only_at_edges() {
        let (w, h) = (1031u32, 1093u32);
        let mut whole = Pixmap::new(w, h).unwrap();
        whole.fill(Color::from_rgba8(235, 232, 220, 255));
        let mut banded = whole.clone();
        let mut all = a_layers_drawing(w as f32, h as f32, false);
        stroke_wide(&mut all.kept, 1, &|| false);
        for k in &all.kept {
            draw_op(&mut whole.as_mut(), &k.op, 0);
        }
        a_layers_drawing(w as f32, h as f32, false).draw_on(&mut banded, 4);
        let (mut differ, mut most) = (0usize, 0u8);
        for (a, b) in whole.pixels().iter().zip(banded.pixels()) {
            let d = [a.red().abs_diff(b.red()), a.green().abs_diff(b.green()), a.blue().abs_diff(b.blue())].into_iter().max().unwrap_or(0);
            differ += usize::from(d > 0);
            most = most.max(d);
        }
        // About 0.8% here, by at most about 20 levels: every shape is at least 60/255 opaque, so
        // one left out of a band would show by far more.
        assert!(differ * 50 < (w * h) as usize, "{differ} pixels differ");
        assert!(most <= 48, "a pixel differs by {most}");
    }

    /// The bands are the frame's whatever the threads: one thread and many draw the same bytes.
    #[test]
    fn a_layer_comes_out_the_same_on_one_thread_or_many() {
        let one = drawn(1);
        assert!(one.pixels().iter().any(|p| p.red() == 200), "the sprites are drawn");
        for threads in [2, 6] {
            assert!(one.data() == drawn(threads).data(), "{threads} threads drew other pixels than one");
        }
    }

    /// Splats, tiny boxes and sprites come out in bands exactly as on the whole frame, also where
    /// they straddle a band's edge, and a tiny box whose quarter-pixel snapping falls on a tie
    /// (half a quarter), on a band's edge or with half its outline's width added.
    #[test]
    fn the_pixel_shortcuts_are_the_whole_frames_pixels() {
        let (w, h) = (300, 3 * BAND_ROWS);
        let sprite = Arc::new({
            let mut sp = Pixmap::new(7, 7).unwrap();
            sp.fill(Color::from_rgba8(0, 0, 200, 160));
            sp
        });
        let ops = || {
            let mut ops = Vec::new();
            for (k, cy) in [5.3f32, 127.4, 127.9, 128.0, 128.6, 255.5, 256.25, 300.7].into_iter().enumerate() {
                let cx = 10.0 + k as f32 * 31.7;
                let color = Rgba::new(200, 30, 30, 255);
                ops.push(PixelOp::Splat { cx, cy, color, coverage: 0.7 });
                ops.push(PixelOp::SmallPolygon {
                    b: [cx + 9.1, cy - 0.6, cx + 10.4, cy + 0.9],
                    width: 1.5,
                    fill: color,
                    stroke: Rgba::new(0, 0, 0, 255),
                    share: 1.0,
                });
                ops.push(PixelOp::Sprite { pixmap: sprite.clone(), x: i64::from(cx as i32) + 15, y: i64::from(cy as i32) - 3 });
            }
            for (k, (top, bottom, width)) in
                [(127.875, 129.125, 1.0), (126.875, 128.125, 0.0), (129.0, 129.825, 0.1), (255.0, 255.7, 2.35), (254.9, 256.1, 0.7)]
                    .into_iter()
                    .enumerate()
            {
                let x = 20.0 + k as f32 * 50.0;
                let fill = Rgba::new(30, 160, 30, 255);
                ops.push(PixelOp::SmallPolygon { b: [x, top, x + 1.5, bottom], width, fill, stroke: Rgba::new(0, 0, 0, 255), share: 1.0 });
            }
            ops
        };
        let mut whole = Pixmap::new(w, h).unwrap();
        whole.fill(Color::WHITE);
        let mut banded = whole.clone();
        for op in ops() {
            op.apply(&mut whole.as_mut(), 0);
        }
        let mut bands = Bands::default();
        ops().into_iter().for_each(|op| bands.pixels(op));
        bands.draw_on(&mut banded, 3);
        assert!(whole.data() == banded.data());
    }

    /// A stroke wider than a hairline, stroked once for every band, draws what tiny-skia's own
    /// `stroke_path` draws; a hairline is left to it.
    #[test]
    fn a_wide_stroke_is_stroked_as_tiny_skia_strokes_it() {
        let mut pb = PathBuilder::new();
        pb.move_to(10.0, 20.0);
        pb.line_to(280.0, 90.0);
        pb.line_to(40.0, 110.0);
        let path = pb.finish().unwrap();
        let paint = solid(20, 60, 160, 200);
        for stroke in [
            Stroke { width: 4.5, line_join: LineJoin::Miter, ..Stroke::default() },
            Stroke { width: 3.0, dash: StrokeDash::new(vec![5.0, 4.0], 2.0), line_cap: LineCap::Round, ..Stroke::default() },
            Stroke { width: 0.7, ..Stroke::default() },
            Stroke { width: 0.0, ..Stroke::default() },
        ] {
            // One band: no edge cut, so only the stroking could differ.
            let mut own = Pixmap::new(300, BAND_ROWS).unwrap();
            own.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
            let mut banded = Pixmap::new(300, BAND_ROWS).unwrap();
            let mut bands = Bands::default();
            assert!(bands.stroke(&path, &paint, &stroke, Transform::identity()));
            bands.draw_on(&mut banded, 1);
            assert!(own.data() == banded.data(), "width {}", stroke.width);
        }
        // Scaled and skewed: the stroke's resolution and the hairline test follow the transform.
        for (ts, stroke) in [
            (Transform::from_row(2.5, 0.0, 0.0, 2.5, -3.0, 1.0), Stroke { width: 1.2, ..Stroke::default() }),
            (Transform::from_row(0.9, 0.3, -0.4, 0.8, 30.0, 0.0), Stroke { width: 3.0, line_join: LineJoin::Round, ..Stroke::default() }),
            (Transform::from_row(0.4, 0.0, 0.0, 0.4, 0.0, 0.0), Stroke { width: 2.0, ..Stroke::default() }),
        ] {
            let mut own = Pixmap::new(300, BAND_ROWS).unwrap();
            own.stroke_path(&path, &paint, &stroke, ts, None);
            let mut banded = Pixmap::new(300, BAND_ROWS).unwrap();
            let mut bands = Bands::default();
            assert!(bands.stroke(&path, &paint, &stroke, ts));
            bands.draw_on(&mut banded, 1);
            assert!(own.pixels().iter().any(|p| p.alpha() > 0));
            assert!(own.data() == banded.data(), "width {} under {ts:?}", stroke.width);
        }
        let turned = Transform::from_row(0.3, 0.0, 0.0, 0.3, 0.0, 0.0);
        assert!(hairline(&paint, &Stroke { width: 3.0, ..Stroke::default() }, turned));
        assert!(!hairline(&paint, &Stroke { width: 3.0, ..Stroke::default() }, Transform::identity()));
    }

    /// While a canvas keeps its drawing for bands, anything that touches its pixels (a pattern, a
    /// mask, glyph coverage) draws what was kept first: the order is that of the calls. Rectangles
    /// come out as on a canvas drawing straight away.
    #[test]
    fn touching_the_pixels_draws_what_was_kept_first() {
        let draw = |in_bands: bool| {
            let mut c = Canvas::new(1024, 1024).unwrap();
            if in_bands {
                c.begin_bands();
            }
            let rect = |x: f32, y: f32| Rect::from_xywh(x, y, 400.0, 100.0).unwrap();
            c.fill_rect(rect(0.0, 0.0), &solid(200, 0, 0, 255), Transform::identity(), None);
            // Straight on the pixels, over the red.
            c.pixels().fill_rect(rect(100.0, 20.0), &solid(0, 0, 200, 255), Transform::identity(), None);
            c.fill_rect(rect(200.0, 40.0), &solid(0, 160, 0, 128), Transform::identity(), None);
            let mut pattern = Pixmap::new(4, 4).unwrap();
            pattern.fill(Color::from_rgba8(250, 250, 0, 255));
            let shader = tiny_skia::Pattern::new(
                pattern.as_ref(),
                tiny_skia::SpreadMode::Repeat,
                tiny_skia::FilterQuality::Nearest,
                1.0,
                Transform::identity(),
            );
            let paint = Paint { shader, ..Paint::default() };
            c.fill_path(&PathBuilder::from_rect(rect(300.0, 60.0)), &paint, FillRule::Winding, Transform::identity(), None);
            c.fill_rect(rect(350.0, 80.0), &solid(0, 0, 0, 255), Transform::identity(), None);
            c.end_bands();
            c.into_pixmap()
        };
        assert!(draw(true).data() == draw(false).data());
    }

    /// What waits is bounded: past [`MAX_KEPT_BYTES`] it is drawn and the list starts again.
    #[test]
    fn what_waits_for_the_bands_is_bounded() {
        let mut c = Canvas::new(1024, 1024).unwrap();
        c.begin_bands();
        let mut pb = PathBuilder::new();
        pb.move_to(0.0, 0.0);
        for i in 0..100_000 {
            pb.line_to((i % 1000) as f32, (i / 100) as f32);
        }
        let path = pb.finish().unwrap();
        let paint = solid(0, 0, 0, 255);
        for _ in 0..60 {
            c.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
            assert!(c.kept_bytes() < MAX_KEPT_BYTES + path_bytes(&path), "{} bytes wait", c.kept_bytes());
        }
        assert!(c.kept_bytes() > 0);
        c.end_bands();
        assert_eq!(c.kept_bytes(), 0);
        assert!(c.pixmap().pixels().iter().any(|p| p.alpha() == 255));
    }

    /// A dashed stroke counts its dashes; one whose dashes alone are more than may wait is drawn
    /// straight away, as is a path that big.
    #[test]
    fn dashes_count_and_what_is_too_big_is_drawn_straight_away() {
        let line = |len: f32| {
            let mut pb = PathBuilder::new();
            pb.move_to(0.0, 0.0);
            pb.line_to(len, 0.0);
            pb.finish().unwrap()
        };
        let paint = solid(0, 0, 0, 255);
        let mut bands = Bands::default();
        let dashed = Stroke { width: 3.0, dash: StrokeDash::new(vec![1.0, 1.0], 0.0), ..Stroke::default() };
        assert!(bands.stroke(&line(1000.0), &paint, &dashed, Transform::from_translate(0.0, 10.0)));
        assert!(bands.bytes() > OUTLINE_GROWTH * 500 * 16, "{} bytes for 500 dashes", bands.bytes());
        // 400,000 dashes, a frame wide: kept, their outlines would be far past the bound. Drawn
        // straight away over what was kept before it, and under what comes after.
        let many = Stroke { width: 1000.0, dash: StrokeDash::new(vec![0.5, 0.5], 0.0), ..Stroke::default() };
        let ts = Transform::from_row(0.0025, 0.0, 0.0, 0.0025, 0.0, 500.0);
        let draw = |in_bands: bool| {
            let mut c = Canvas::new(1024, 1024).unwrap();
            if in_bands {
                c.begin_bands();
            }
            let rect = |x: f32| PathBuilder::from_rect(Rect::from_xywh(x, 400.0, 300.0, 200.0).unwrap());
            c.fill_path(&rect(100.0), &solid(200, 0, 0, 255), FillRule::Winding, Transform::identity(), None);
            c.stroke_path(&line(400_000.0), &solid(0, 0, 200, 160), &many, ts, None);
            if in_bands {
                assert_eq!(c.kept_bytes(), 0, "drawn straight away, after what was kept");
            }
            c.fill_path(&rect(600.0), &solid(0, 160, 0, 255), FillRule::Winding, Transform::identity(), None);
            c.end_bands();
            c.into_pixmap()
        };
        let banded = draw(true);
        assert!(banded.pixels().iter().any(|p| p.blue() > 0 && p.red() > 0), "the stroke is over the first fill");
        assert!(banded.data() == draw(false).data());
    }

    /// A picture shared by every point (a picture marker, a sprite) counts its pixels once, however
    /// many times it is kept, and again once drawn and kept anew.
    #[test]
    fn a_shared_picture_counts_once() {
        let picture = Arc::new(Pixmap::new(100, 100).unwrap());
        let mut bands = Bands::default();
        for _ in 0..3 {
            bands.shared_pixmap(&picture, &PixmapPaint::default(), Transform::from_translate(10.0, 10.0));
            bands.pixels(PixelOp::Sprite { pixmap: picture.clone(), x: 5, y: 5 });
        }
        assert_eq!(bands.bytes(), 100 * 100 * 4 + 6 * ENTRY_BYTES);
        bands.draw_on(&mut Pixmap::new(300, 300).unwrap(), 1);
        assert_eq!(bands.bytes(), 0);
        bands.shared_pixmap(&picture, &PixmapPaint::default(), Transform::identity());
        assert_eq!(bands.bytes(), 100 * 100 * 4 + ENTRY_BYTES);
    }

    /// Threads start for drawing that covers a megapixel, counted by the rows and columns each kept
    /// thing reaches: a narrow, tall shape alone starts none.
    #[test]
    fn a_narrow_shape_starts_no_threads() {
        let paint = solid(0, 0, 0, 255);
        let (w, h) = (2000, 2000);
        let mut bands = Bands::default();
        assert!(bands.fill(
            &PathBuilder::from_rect(Rect::from_xywh(100.0, 0.0, 3.0, 2000.0).unwrap()),
            &paint,
            FillRule::Winding,
            Transform::identity()
        ));
        assert_eq!(threads_for(&bands.kept, w, h, 8, 1 << 30), 1);
        assert!(bands.fill(
            &PathBuilder::from_rect(Rect::from_xywh(0.0, 0.0, 2000.0, 2000.0).unwrap()),
            &paint,
            FillRule::Winding,
            Transform::identity()
        ));
        assert_eq!(threads_for(&bands.kept, w, h, 8, 1 << 30), 8);
    }

    /// Every band moves a copy of a path it draws: a big path starts only the threads the scratch
    /// memory holds, beyond the calling thread's.
    #[test]
    fn a_big_path_starts_only_the_threads_scratch_holds() {
        let mut pb = PathBuilder::new();
        pb.move_to(0.0, 0.0);
        for i in 0..1_000_000 {
            pb.line_to((i % 2000) as f32, (i / 500) as f32);
        }
        let path = pb.finish().unwrap();
        let mut bands = Bands::default();
        assert!(bands.fill(&path, &solid(0, 0, 0, 255), FillRule::Winding, Transform::identity()));
        let each = path_bytes(&path) * SCRATCH_GROWTH;
        assert_eq!(threads_for(&bands.kept, 2000, 2000, 16, 2 * each), 3);
        assert_eq!(threads_for(&bands.kept, 2000, 2000, 16, each / 2), 1);
        assert_eq!(threads_for(&bands.kept, 2000, 2000, 16, 100 * each), 16);
        // Stroked wider than a hairline, a path becomes an outline several times its size; as a
        // hairline, it is drawn as it is.
        let mut pb = PathBuilder::new();
        pb.move_to(0.0, 0.0);
        for i in 0..200_000 {
            pb.line_to((i % 2000) as f32, (i / 100) as f32);
        }
        let path = pb.finish().unwrap();
        let each = path_bytes(&path) * SCRATCH_GROWTH;
        let mut wide = Bands::default();
        assert!(wide.stroke(&path, &solid(0, 0, 0, 255), &Stroke { width: 3.0, ..Stroke::default() }, Transform::identity()));
        assert_eq!(threads_for(&wide.kept, 2000, 2000, 16, OUTLINE_GROWTH * each), 2);
        let mut thin = Bands::default();
        assert!(thin.stroke(&path, &solid(0, 0, 0, 255), &Stroke { width: 0.5, ..Stroke::default() }, Transform::identity()));
        assert_eq!(threads_for(&thin.kept, 2000, 2000, 16, OUTLINE_GROWTH * each), 9);
        // No path at all (frame-wide pixmaps): every thread, whatever the scratch.
        let mut pictures = Bands::default();
        let picture = Arc::new(Pixmap::new(2000, 100).unwrap());
        for y in 0..20 {
            pictures.shared_pixmap(&picture, &PixmapPaint::default(), Transform::from_translate(0.0, y as f32 * 100.0));
        }
        assert_eq!(threads_for(&pictures.kept, 2000, 2000, 16, usize::MAX), 16);
    }

    /// Too small a frame, or a recording canvas, keeps nothing: drawn straight away as before.
    #[test]
    fn small_frames_and_recordings_draw_straight_away() {
        assert!(!applies(1023, 1024) && !applies(8000, 255) && applies(1024, 1024));
        let paint = solid(0, 0, 0, 255);
        let path = PathBuilder::from_rect(Rect::from_xywh(0.0, 0.0, 10.0, 10.0).unwrap());
        for mut c in [Canvas::new(800, 600).unwrap(), Canvas::recording(2000, 2000).unwrap()] {
            c.begin_bands();
            c.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
            assert_eq!(c.kept_bytes(), 0);
        }
    }

    /// A frame asked to stop draws none of what waits: it is no frame.
    #[test]
    fn a_stopped_frame_draws_nothing_kept() {
        let flag = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let _stop = crate::cancel::install(flag);
        let mut pm = Pixmap::new(300, 300).unwrap();
        let mut bands = Bands::default();
        assert!(bands.fill(
            &PathBuilder::from_rect(Rect::from_xywh(0.0, 0.0, 300.0, 300.0).unwrap()),
            &solid(0, 0, 0, 255),
            FillRule::Winding,
            Transform::identity()
        ));
        bands.draw_on(&mut pm, 4);
        assert!(pm.pixels().iter().all(|p| p.alpha() == 0));
    }
}
