//! A drawing kept as what was drawn, for writers that keep vectors ([`crate::pdf`]): filled and
//! stroked paths, runs of glyphs, pictures and groups, bottom first, in the drawing's pixels
//! (y down, from its top left). A group composites its items as one layer: with an opacity, a
//! blend mode and a clip, as the renderer composites a layer drawn offscreen.

use ab_glyph::{Font, FontRef, GlyphId};
use rustgis_map::effects::Blend;
#[cfg(test)]
use tiny_skia::Pixmap;
use tiny_skia::{Color, FillRule, LineCap, LineJoin, Path, PixmapRef, Transform};

/// The most items one drawing keeps, groups' items included: a writer's content grows with it.
pub const MAX_ITEMS: usize = 20_000_000;

/// The most a drawing's content may come to ([`Drawing::cost`]), in bytes: past it, what is drawn
/// is refused and the drawing says so ([`Drawing::overflowed`]), for its caller to draw it another
/// way (as a picture) and say so.
pub const MAX_COST: usize = if cfg!(target_arch = "wasm32") { 64 << 20 } else { 256 << 20 };

#[cfg(test)]
thread_local! {
    /// A smaller [`MAX_COST`] for a test to reach.
    pub(crate) static TEST_MAX_COST: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// [`MAX_COST`] (in tests, any smaller limit set for them).
pub(crate) fn max_cost() -> usize {
    #[cfg(test)]
    if let Some(limit) = TEST_MAX_COST.with(std::cell::Cell::get) {
        return limit;
    }
    MAX_COST
}

/// A drawing `width` × `height` pixels; see the module.
pub struct Drawing {
    pub width: f32,
    pub height: f32,
    /// The root's items; `open` holds the groups begun and not yet ended, innermost last.
    items: Vec<Item>,
    open: Vec<Group>,
    count: usize,
    cost: usize,
    /// Something drawn was refused: the drawing passed [`MAX_ITEMS`] or [`MAX_COST`].
    overflowed: bool,
    /// The glyphs (by face and id) whose outlines are counted in `cost`, and what each counted.
    outlines: std::collections::HashMap<(usize, u16), usize>,
}

/// One thing drawn.
pub enum Item {
    Fill { path: Path, brush: Brush, rule: FillRule, transform: Transform },
    Stroke { path: Path, color: Color, stroke: Stroke, transform: Transform },
    Glyphs(GlyphRun),
    Image(Image),
    Group(Group),
}

/// What a path is filled with. Gradients pad past their ends.
#[derive(Clone, Debug, PartialEq)]
pub enum Brush {
    Solid(Color),
    /// Colour stops (offsets 0..=1, ascending) along the line from `start` to `end`.
    Linear {
        start: (f32, f32),
        end: (f32, f32),
        stops: Vec<(f32, Color)>,
        transform: Transform,
    },
    /// Colour stops from the circle (`start`, `r0`) to the circle (`end`, `r1`).
    Radial {
        start: (f32, f32),
        r0: f32,
        end: (f32, f32),
        r1: f32,
        stops: Vec<(f32, Color)>,
        transform: Transform,
    },
}

/// A stroke's style, as `tiny_skia::Stroke` has it (solid: a dashed line is kept as its dashes).
#[derive(Clone, Debug, PartialEq)]
pub struct Stroke {
    pub width: f32,
    pub cap: LineCap,
    pub join: LineJoin,
    pub miter_limit: f32,
}

impl Default for Stroke {
    /// One pixel wide, butt-capped, mitred: tiny-skia's default.
    fn default() -> Stroke {
        Stroke { width: 1.0, cap: LineCap::Butt, join: LineJoin::Miter, miter_limit: 4.0 }
    }
}

/// Glyphs of one face along a baseline: `transform` takes the run's space (the baseline along
/// y = 0, x from the run's origin, y down) to the drawing.
pub struct GlyphRun {
    pub font: &'static FontRef<'static>,
    /// The em, in the run's pixels.
    pub size: f32,
    pub color: Color,
    pub transform: Transform,
    pub glyphs: Vec<Glyph>,
}

/// A glyph at its pen position along the run, and the character it stands for (what copying
/// the text gives).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Glyph {
    pub id: GlyphId,
    pub x: f32,
    pub ch: char,
}

/// A picture, kept compressed: its pixels as zlib RGB and, where any isn't opaque, zlib alpha.
/// `transform` takes its pixel space (0..width, 0..height, y down) to the drawing.
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub(crate) rgb_z: Vec<u8>,
    pub(crate) alpha_z: Option<Vec<u8>>,
    pub transform: Transform,
    pub opacity: f32,
    /// Smoothed when scaled up (pictures and rasters drawn smooth), rather than blocky.
    pub smooth: bool,
}

/// Items composited together: see the module. `transform` takes the items' space to the
/// drawing's; the clip is in the drawing's.
pub struct Group {
    pub items: Vec<Item>,
    pub opacity: f32,
    pub blend: Blend,
    pub clip: Option<Clip>,
    pub transform: Transform,
}

/// A clip path: what is outside it isn't drawn.
#[derive(Clone, Debug)]
pub struct Clip {
    pub path: Path,
    pub rule: FillRule,
    pub transform: Transform,
}

impl Drawing {
    /// An empty drawing `width` × `height` pixels (a size that isn't positive is one pixel).
    pub fn new(width: f32, height: f32) -> Drawing {
        let size = |v: f32| if v.is_finite() && v > 0.0 { v } else { 1.0 };
        Drawing {
            width: size(width),
            height: size(height),
            items: Vec::new(),
            open: Vec::new(),
            count: 0,
            cost: 0,
            overflowed: false,
            outlines: Default::default(),
        }
    }

    /// Roughly how many bytes a writer gives it: what callers budget a document by.
    pub fn cost(&self) -> usize {
        self.cost
    }

    /// Whether nothing is drawn yet.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Whether something drawn was refused, the drawing having passed [`MAX_ITEMS`] or
    /// [`MAX_COST`]: it is then incomplete.
    pub fn overflowed(&self) -> bool {
        self.overflowed
    }

    /// Fills `path`, moved by `transform`, with `brush`; nothing for a transform or brush that
    /// isn't finite.
    pub fn fill(&mut self, path: Path, brush: Brush, rule: FillRule, transform: Transform) {
        if transform.is_finite() && brush.is_valid() {
            self.cost += path_cost(&path) + if matches!(brush, Brush::Solid(_)) { 0 } else { 400 };
            self.push(Item::Fill { path, brush, rule, transform });
        }
    }

    /// Strokes `path`, moved by `transform`, in `color`; nothing for a width that isn't finite.
    pub fn stroke(&mut self, path: Path, color: Color, stroke: Stroke, transform: Transform) {
        if transform.is_finite() && stroke.width.is_finite() && stroke.width >= 0.0 {
            self.cost += path_cost(&path) + 8;
            self.push(Item::Stroke { path, color, stroke, transform });
        }
    }

    /// Sets a run of glyphs; nothing for an empty run or a size or transform that isn't finite.
    pub fn glyphs(&mut self, run: GlyphRun) {
        if !run.glyphs.is_empty() && run.size.is_finite() && run.size > 0.0 && run.transform.is_finite() {
            self.cost += 60 + run.glyphs.len() * 8;
            // Each glyph's outline, written once in the document's fonts, counts the first time.
            let face = std::ptr::from_ref(run.font) as usize;
            for g in &run.glyphs {
                if let std::collections::hash_map::Entry::Vacant(e) = self.outlines.entry((face, g.id.0)) {
                    let c = 64 + run.font.outline(g.id).map_or(0, |o| o.curves.len() * 40);
                    e.insert(c);
                    self.cost += c;
                }
            }
            self.push(Item::Glyphs(run));
        }
    }

    /// Places `image` with `transform` (see [`Image`]); a picture with nothing in it is left out.
    pub fn image(&mut self, image: PixmapRef, transform: Transform, opacity: f32, smooth: bool) {
        if !transform.is_finite() || opacity.is_nan() || opacity <= 0.0 {
            return;
        }
        let Some(image) = Image::new(image, transform, opacity.min(1.0), smooth) else { return };
        self.cost += image.rgb_z.len() + image.alpha_z.as_ref().map_or(0, Vec::len) + 300;
        self.push(Item::Image(image));
    }

    /// Starts a group: what is drawn until [`Drawing::end_group`] is composited as one.
    pub fn begin_group(&mut self, opacity: f32, blend: Blend, clip: Option<Clip>) {
        let opacity = if opacity.is_finite() { opacity.clamp(0.0, 1.0) } else { 1.0 };
        let clip = clip.filter(|c| c.transform.is_finite());
        self.cost += clip.as_ref().map_or(0, |c| path_cost(&c.path)) + 120;
        self.open.push(Group { items: Vec::new(), opacity, blend, clip, transform: Transform::identity() });
    }

    /// Places `child` as a group: its items moved by `transform` (from its space to this
    /// drawing's), composited with `opacity` and `blend`, inside `clip` (in this drawing's
    /// space). Nothing for an empty drawing or a transform that isn't finite.
    pub fn place(&mut self, mut child: Drawing, transform: Transform, opacity: f32, blend: Blend, clip: Option<Clip>) {
        self.overflowed |= child.overflowed;
        if !transform.is_finite() || child.is_empty() {
            return;
        }
        // Outlines this drawing already counts are written once: not counted again.
        let held: usize = child.outlines.iter().filter(|(k, _)| self.outlines.contains_key(k)).map(|(_, c)| *c).sum();
        let (count, cost) = (child.count, child.cost.saturating_sub(held));
        let outlines = std::mem::take(&mut child.outlines);
        let (_, _, items) = child.finish();
        // (A group's own entry counts too, and its clip's path.)
        let clip = clip.filter(|c| c.transform.is_finite());
        let cost = cost + 120 + clip.as_ref().map_or(0, |c| path_cost(&c.path));
        if self.count.saturating_add(count) > MAX_ITEMS || self.cost.saturating_add(cost) > max_cost() {
            self.overflowed = true;
            return;
        }
        self.outlines.extend(outlines);
        self.count += count;
        self.cost += cost;
        let opacity = if opacity.is_finite() { opacity.clamp(0.0, 1.0) } else { 1.0 };
        self.add(Item::Group(Group { items, opacity, blend, clip, transform }));
    }

    /// Ends the innermost group begun; nothing when none is open. An empty group is left out.
    pub fn end_group(&mut self) {
        if let Some(g) = self.open.pop()
            && !g.items.is_empty()
        {
            self.add(Item::Group(g));
        }
    }

    /// The items, every group ended.
    pub fn finish(mut self) -> (f32, f32, Vec<Item>) {
        while !self.open.is_empty() {
            self.end_group();
        }
        (self.width, self.height, self.items)
    }

    /// Adds a drawn item, unless the drawing is past its budget (it then says it overflowed).
    fn push(&mut self, item: Item) {
        if self.count >= MAX_ITEMS || self.cost > max_cost() {
            self.overflowed = true;
            return;
        }
        self.count += 1;
        self.add(item);
    }

    /// Adds `item` to the innermost open group, or the drawing itself.
    fn add(&mut self, item: Item) {
        match self.open.last_mut() {
            Some(g) => g.items.push(item),
            None => self.items.push(item),
        }
    }
}

/// Roughly what a path's operators take: a dozen bytes a point.
fn path_cost(path: &Path) -> usize {
    16 + path.points().len() * 12
}

impl Brush {
    /// Whether it can be drawn: finite geometry and, for a gradient, a stop at least.
    fn is_valid(&self) -> bool {
        let finite = |p: &(f32, f32)| p.0.is_finite() && p.1.is_finite();
        match self {
            Brush::Solid(_) => true,
            Brush::Linear { start, end, stops, transform } => finite(start) && finite(end) && !stops.is_empty() && transform.is_finite(),
            Brush::Radial { start, r0, end, r1, stops, transform } => {
                finite(start) && finite(end) && r0.is_finite() && r1.is_finite() && !stops.is_empty() && transform.is_finite()
            }
        }
    }
}

impl Image {
    /// `pm` compressed (see [`compress_rgba`]); `None` for one with nothing showing.
    fn new(pm: PixmapRef, transform: Transform, opacity: f32, smooth: bool) -> Option<Image> {
        let (rgb_z, alpha_z) = compress_rgba(pm)?;
        Some(Image { width: pm.width(), height: pm.height(), rgb_z, alpha_z, transform, opacity, smooth })
    }
}

/// `pm`'s colours (unpremultiplied) as zlib RGB, a row at a time so a large picture never holds
/// a second full-size copy, and its alpha as zlib grey where any pixel isn't opaque; `None` if a
/// picture with no pixel showing, or if compressing fails.
pub(crate) fn compress_rgba(pm: PixmapRef) -> Option<(Vec<u8>, Option<Vec<u8>>)> {
    let px = pm.pixels();
    if px.iter().all(|p| p.alpha() == 0) {
        return None;
    }
    let w = pm.width().max(1) as usize;
    let rgb = deflate_rows(px.chunks(w), |line, row| {
        row.extend(line.iter().flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue()]
        }))
    })?;
    let alpha = if px.iter().any(|p| p.alpha() < 255) {
        Some(deflate_rows(px.chunks(w), |line, row| row.extend(line.iter().map(|p| p.alpha())))?)
    } else {
        None
    };
    Some((rgb, alpha))
}

/// Deflates (zlib) the rows `fill` writes, one row at a time; `None` if compressing fails.
pub(crate) fn deflate_rows<'a, T: 'a>(rows: impl Iterator<Item = &'a [T]>, mut fill: impl FnMut(&[T], &mut Vec<u8>)) -> Option<Vec<u8>> {
    use miniz_oxide::deflate::core::{CompressorOxide, TDEFLFlush, TDEFLStatus, compress_to_output, create_comp_flags_from_zip_params};
    let mut z = CompressorOxide::new(create_comp_flags_from_zip_params(6, 15, 0));
    let mut out = Vec::new();
    let mut row = Vec::new();
    for line in rows {
        row.clear();
        fill(line, &mut row);
        let (status, used) = compress_to_output(&mut z, &row, TDEFLFlush::None, |b| {
            out.extend_from_slice(b);
            true
        });
        if used != row.len() || matches!(status, TDEFLStatus::BadParam | TDEFLStatus::PutBufFailed) {
            return None;
        }
    }
    let (status, _) = compress_to_output(&mut z, &[], TDEFLFlush::Finish, |b| {
        out.extend_from_slice(b);
        true
    });
    (status == TDEFLStatus::Done).then_some(out)
}

/// Draws `items` (moved by `base`) back onto pixels with tiny-skia: for tests comparing a
/// recording with what the renderer draws.
#[cfg(test)]
pub(crate) fn replay(items: &[Item], pm: &mut Pixmap, base: Transform) {
    use ab_glyph::Font;
    use tiny_skia::{GradientStop, Paint, PathBuilder, PixmapPaint, Point, SpreadMode};
    let paint = |c: Color| {
        let mut p = Paint::default();
        p.set_color(c);
        p
    };
    for item in items {
        match item {
            Item::Fill { path, brush, rule, transform } => {
                let stops = |s: &[(f32, Color)]| s.iter().map(|(t, c)| GradientStop::new(*t, *c)).collect::<Vec<_>>();
                let shader = match brush {
                    Brush::Solid(c) => Some(tiny_skia::Shader::SolidColor(*c)),
                    Brush::Linear { start, end, stops: s, transform: g } => tiny_skia::LinearGradient::new(
                        Point::from_xy(start.0, start.1),
                        Point::from_xy(end.0, end.1),
                        stops(s),
                        SpreadMode::Pad,
                        *g,
                    ),
                    Brush::Radial { start, r0, end, r1, stops: s, transform: g } => tiny_skia::RadialGradient::new(
                        Point::from_xy(start.0, start.1),
                        *r0,
                        Point::from_xy(end.0, end.1),
                        *r1,
                        stops(s),
                        SpreadMode::Pad,
                        *g,
                    ),
                };
                if let Some(shader) = shader {
                    pm.fill_path(path, &Paint { shader, ..Paint::default() }, *rule, base.pre_concat(*transform), None);
                }
            }
            Item::Stroke { path, color, stroke, transform } => {
                let st = tiny_skia::Stroke {
                    width: stroke.width,
                    line_cap: stroke.cap,
                    line_join: stroke.join,
                    miter_limit: stroke.miter_limit,
                    dash: None,
                };
                pm.stroke_path(path, &paint(*color), &st, base.pre_concat(*transform), None);
            }
            Item::Glyphs(run) => {
                let upm = run.font.units_per_em().unwrap_or(1000.0);
                let k = run.size / upm;
                let mut pb = PathBuilder::new();
                for g in &run.glyphs {
                    let Some(o) = run.font.outline(g.id) else { continue };
                    let at = |p: ab_glyph::Point| (g.x + p.x * k, -p.y * k);
                    let mut last = None;
                    for c in &o.curves {
                        let (a, e) = match c {
                            ab_glyph::OutlineCurve::Line(a, b)
                            | ab_glyph::OutlineCurve::Quad(a, _, b)
                            | ab_glyph::OutlineCurve::Cubic(a, _, _, b) => (*a, *b),
                        };
                        if last != Some(a) {
                            if last.is_some() {
                                pb.close();
                            }
                            let (x, y) = at(a);
                            pb.move_to(x, y);
                        }
                        match c {
                            ab_glyph::OutlineCurve::Line(_, b) => {
                                let (x, y) = at(*b);
                                pb.line_to(x, y);
                            }
                            ab_glyph::OutlineCurve::Quad(_, q, b) => {
                                let ((qx, qy), (x, y)) = (at(*q), at(*b));
                                pb.quad_to(qx, qy, x, y);
                            }
                            ab_glyph::OutlineCurve::Cubic(_, q1, q2, b) => {
                                let ((ax, ay), (bx, by), (x, y)) = (at(*q1), at(*q2), at(*b));
                                pb.cubic_to(ax, ay, bx, by, x, y);
                            }
                        }
                        last = Some(e);
                    }
                    if last.is_some() {
                        pb.close();
                    }
                }
                if let Some(path) = pb.finish() {
                    pm.fill_path(&path, &paint(run.color), tiny_skia::FillRule::Winding, base.pre_concat(run.transform), None);
                }
            }
            Item::Image(image) => {
                let rgb = miniz_oxide::inflate::decompress_to_vec_zlib(&image.rgb_z).unwrap_or_default();
                let alpha = image.alpha_z.as_ref().map(|a| miniz_oxide::inflate::decompress_to_vec_zlib(a).unwrap_or_default());
                let Some(mut pic) = Pixmap::new(image.width, image.height) else { continue };
                for (i, px) in pic.pixels_mut().iter_mut().enumerate() {
                    let a = alpha.as_ref().and_then(|a| a.get(i).copied()).unwrap_or(255);
                    let c = |k: usize| rgb.get(i * 3 + k).copied().unwrap_or(0);
                    *px = tiny_skia::ColorU8::from_rgba(c(0), c(1), c(2), a).premultiply();
                }
                let pp = PixmapPaint { opacity: image.opacity, quality: tiny_skia::FilterQuality::Bilinear, ..PixmapPaint::default() };
                pm.draw_pixmap(0, 0, pic.as_ref(), &pp, base.pre_concat(image.transform), None);
            }
            Item::Group(g) => {
                let Some(mut own) = Pixmap::new(pm.width(), pm.height()) else { continue };
                replay(&g.items, &mut own, base.pre_concat(g.transform));
                let mask = g.clip.as_ref().and_then(|c| {
                    let mut m = tiny_skia::Mask::new(pm.width(), pm.height())?;
                    m.fill_path(&c.path, c.rule, true, base.pre_concat(c.transform));
                    Some(m)
                });
                let pp = PixmapPaint { opacity: g.opacity, blend_mode: crate::blend_mode(g.blend), ..PixmapPaint::default() };
                pm.draw_pixmap(0, 0, own.as_ref(), &pp, Transform::identity(), mask.as_ref());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiny_skia::PathBuilder;

    fn square() -> Path {
        PathBuilder::from_rect(tiny_skia::Rect::from_xywh(1.0, 1.0, 4.0, 4.0).unwrap())
    }

    #[test]
    fn groups_nest_and_empty_ones_are_left_out() {
        let mut d = Drawing::new(10.0, 10.0);
        d.fill(square(), Brush::Solid(Color::BLACK), FillRule::Winding, Transform::identity());
        d.begin_group(0.5, Blend::Multiply, None);
        d.begin_group(1.0, Blend::Normal, None);
        d.end_group();
        d.stroke(square(), Color::WHITE, Stroke::default(), Transform::identity());
        // Left open: finishing ends it.
        let (_, _, items) = d.finish();
        assert_eq!(items.len(), 2);
        let Item::Group(g) = &items[1] else { panic!("a group") };
        assert_eq!((g.items.len(), g.opacity, g.blend), (1, 0.5, Blend::Multiply));
        assert!(matches!(g.items[0], Item::Stroke { .. }));
    }

    #[test]
    fn nothing_unusable_is_kept() {
        let mut d = Drawing::new(f32::NAN, -1.0);
        assert_eq!((d.width, d.height), (1.0, 1.0));
        let bad = Transform::from_row(f32::NAN, 0.0, 0.0, 1.0, 0.0, 0.0);
        d.fill(square(), Brush::Solid(Color::BLACK), FillRule::Winding, bad);
        let no_stops = Brush::Linear { start: (0.0, 0.0), end: (1.0, 0.0), stops: vec![], transform: Transform::identity() };
        d.fill(square(), no_stops, FillRule::Winding, Transform::identity());
        d.stroke(square(), Color::BLACK, Stroke { width: f32::INFINITY, ..Stroke::default() }, Transform::identity());
        let clear = Pixmap::new(3, 3).unwrap();
        d.image(clear.as_ref(), Transform::identity(), 1.0, false);
        let mut red = Pixmap::new(3, 3).unwrap();
        red.fill(Color::from_rgba8(255, 0, 0, 255));
        d.image(red.as_ref(), Transform::identity(), 0.0, false);
        assert!(d.is_empty() && d.cost() == 0);
    }

    #[test]
    fn a_drawing_past_its_budget_refuses_more_and_says_so() {
        TEST_MAX_COST.with(|l| l.set(Some(200)));
        let mut d = Drawing::new(10.0, 10.0);
        for _ in 0..10 {
            d.fill(square(), Brush::Solid(Color::BLACK), FillRule::Winding, Transform::identity());
        }
        assert!(d.overflowed());
        let kept = d.count;
        assert!(kept > 0 && kept < 10, "kept {kept}");
        // A group holding an overflowed drawing is incomplete too.
        let mut parent = Drawing::new(10.0, 10.0);
        parent.place(d, Transform::identity(), 1.0, Blend::Normal, None);
        assert!(parent.overflowed());
        TEST_MAX_COST.with(|l| l.set(None));
        let mut fine = Drawing::new(10.0, 10.0);
        fine.fill(square(), Brush::Solid(Color::BLACK), FillRule::Winding, Transform::identity());
        assert!(!fine.overflowed());
    }

    #[test]
    fn a_glyph_outline_counts_once_however_many_drawings_set_it() {
        let font = crate::text::label_font().unwrap();
        let run = || GlyphRun {
            font,
            size: 12.0,
            color: Color::BLACK,
            transform: Transform::identity(),
            glyphs: vec![Glyph { id: ab_glyph::Font::glyph_id(font, 'W'), x: 0.0, ch: 'W' }],
        };
        let mut page = Drawing::new(10.0, 10.0);
        page.glyphs(run());
        let once = page.cost();
        let mut child = Drawing::new(10.0, 10.0);
        child.glyphs(run());
        let child_cost = child.cost();
        page.place(child, Transform::identity(), 1.0, Blend::Normal, None);
        // The child's own run counts (60 + 8), its outline doesn't again.
        assert_eq!(page.cost(), once + 68 + 120, "child alone came to {child_cost}");
        // A placed group's clip counts, as a begun group's does.
        let before = page.cost();
        let mut clipped = Drawing::new(10.0, 10.0);
        clipped.fill(square(), Brush::Solid(Color::BLACK), FillRule::Winding, Transform::identity());
        let inner = clipped.cost();
        let clip = Clip { path: square(), rule: FillRule::Winding, transform: Transform::identity() };
        page.place(clipped, Transform::identity(), 1.0, Blend::Normal, Some(clip));
        assert_eq!(page.cost(), before + inner + 120 + path_cost(&square()));
    }

    #[test]
    fn pictures_keep_alpha_only_when_they_need_it() {
        let mut d = Drawing::new(10.0, 10.0);
        let mut opaque = Pixmap::new(4, 2).unwrap();
        opaque.fill(Color::from_rgba8(10, 20, 30, 255));
        let mut half = Pixmap::new(4, 2).unwrap();
        half.fill(Color::from_rgba8(200, 100, 0, 128));
        d.image(opaque.as_ref(), Transform::identity(), 1.0, true);
        d.image(half.as_ref(), Transform::identity(), 1.0, true);
        let (_, _, items) = d.finish();
        let [Item::Image(a), Item::Image(b)] = items.as_slice() else { panic!("two pictures") };
        assert!(a.alpha_z.is_none());
        let rgb = miniz_oxide::inflate::decompress_to_vec_zlib(&b.rgb_z).unwrap();
        let alpha = miniz_oxide::inflate::decompress_to_vec_zlib(b.alpha_z.as_ref().unwrap()).unwrap();
        assert_eq!((rgb.len(), alpha.len()), (4 * 2 * 3, 4 * 2));
        // Unpremultiplied: the colour is the colour, the alpha apart.
        assert!(rgb[0].abs_diff(200) <= 1 && rgb[1].abs_diff(100) <= 1 && alpha[0] == 128);
    }
}
