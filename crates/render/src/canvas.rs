//! What the renderer draws on. Maps, layouts and reports draw through a [`Canvas`] rather than
//! straight onto a `tiny_skia::Pixmap`: its drawing calls are tiny-skia's own, and [`Canvas::pixels`]
//! hands out the pixmap for work that is pixels by nature (rasters, tiles, glyph coverage, effects).
//!
//! A canvas draws onto its pixmap, or records ([`Canvas::recording`]): what it is given then
//! becomes a [`Drawing`] kept as vectors (a vector PDF). A path filled or stroked in one colour
//! is kept as the path; one filled with a shader (a hatch, a gradient) is kept as a picture of it
//! alone, cut to the path; a pixmap drawn is placed as a picture. What is drawn on the pixels
//! gathers on a transparent pixmap of the canvas's size, placed as one picture (cropped to what
//! was drawn) before the next thing kept, so the drawing order stays.

use rustgis_map::effects::Blend;
use tiny_skia::{
    BlendMode, Color, FillRule, IntRect, Mask, Paint, Path, PathBuilder, Pixmap, PixmapPaint, PixmapRef, Rect, Shader, Stroke, Transform,
};

use crate::vector::{self, Brush, Clip, Drawing, GlyphRun};

/// See the module.
pub struct Canvas {
    pm: Pixmap,
    rec: Option<Box<Recording>>,
}

/// A recording canvas's drawing, and what its pixels hold.
struct Recording {
    drawing: Drawing,
    /// The pixmap holds what was drawn on the pixels since it was last placed.
    dirty: bool,
    /// Applied to what is kept ([`Canvas::with_transform`]).
    ts: Transform,
}

impl Canvas {
    /// A transparent canvas `width` × `height` pixels; `None` for a size tiny-skia can't hold.
    pub fn new(width: u32, height: u32) -> Option<Canvas> {
        Pixmap::new(width, height).map(Canvas::from_pixmap)
    }

    /// A canvas drawing onto `pm`, keeping what it has.
    pub fn from_pixmap(pm: Pixmap) -> Canvas {
        Canvas { pm, rec: None }
    }

    /// A canvas `width` × `height` pixels that records (see the module).
    pub fn recording(width: u32, height: u32) -> Option<Canvas> {
        let pm = Pixmap::new(width, height)?;
        let drawing = Drawing::new(width as f32, height as f32);
        Some(Canvas { pm, rec: Some(Box::new(Recording { drawing, dirty: false, ts: Transform::identity() })) })
    }

    /// Whether it records.
    pub fn is_recording(&self) -> bool {
        self.rec.is_some()
    }

    /// What a recording canvas's drawing would come to so far ([`Drawing::cost`]); 0 if it
    /// doesn't record.
    pub fn recorded_cost(&self) -> usize {
        self.rec.as_ref().map_or(0, |r| r.drawing.cost())
    }

    /// Whether a recording canvas refused something drawn, its drawing having grown too large
    /// ([`Drawing::overflowed`]).
    pub fn overflowed(&self) -> bool {
        self.rec.as_ref().is_some_and(|r| r.drawing.overflowed())
    }

    /// A canvas for a picture `width` × `height` pixels to be drawn and placed on this one: one
    /// that records if this one does.
    pub fn sub(&self, width: u32, height: u32) -> Option<Canvas> {
        if self.is_recording() { Canvas::recording(width, height) } else { Canvas::new(width, height) }
    }

    /// The pixels drawn (on a recording canvas, those not yet placed).
    pub fn into_pixmap(self) -> Pixmap {
        self.pm
    }

    /// What a recording canvas kept, everything drawn placed; `None` if it doesn't record.
    pub fn into_drawing(mut self) -> Option<Drawing> {
        self.flush();
        self.rec.map(|r| r.drawing)
    }

    /// Its width in pixels.
    pub fn width(&self) -> u32 {
        self.pm.width()
    }

    /// Its height in pixels.
    pub fn height(&self) -> u32 {
        self.pm.height()
    }

    /// The pixels, to read.
    pub fn pixmap(&self) -> &Pixmap {
        &self.pm
    }

    /// The pixels, for drawing that is pixels by nature (see the module).
    pub fn pixels(&mut self) -> &mut Pixmap {
        if let Some(r) = &mut self.rec {
            r.dirty = true;
        }
        &mut self.pm
    }

    /// Fills `path` with `paint`, as `Pixmap::fill_path` does.
    pub fn fill_path(&mut self, path: &Path, paint: &Paint, rule: FillRule, transform: Transform, mask: Option<&Mask>) {
        if self.rec.is_none() || mask.is_some() || paint.blend_mode != BlendMode::SourceOver {
            self.pixels().fill_path(path, paint, rule, transform, mask);
            return;
        }
        if let Shader::SolidColor(c) = paint.shader {
            self.keep(|r| r.drawing.fill(path.clone(), Brush::Solid(c), rule, r.ts.pre_concat(transform)));
            return;
        }
        // A shader: a picture of the fill, cut to the path.
        let Some(bounds) = path.clone().transform(transform).map(|p| p.bounds()) else { return };
        let clip = Clip { path: path.clone(), rule, transform };
        self.keep_picture(bounds, Some(clip), |pm, at| pm.fill_path(path, paint, rule, at.pre_concat(transform), None));
    }

    /// Keeps `path` filled with `brush` (a gradient a PDF shades); nothing unless it records.
    pub fn fill_brush(&mut self, path: &Path, brush: Brush, rule: FillRule) {
        self.keep(|r| r.drawing.fill(path.clone(), brush, rule, r.ts));
    }

    /// Strokes `path` with `paint` and `stroke`, as `Pixmap::stroke_path` does.
    pub fn stroke_path(&mut self, path: &Path, paint: &Paint, stroke: &Stroke, transform: Transform, mask: Option<&Mask>) {
        if self.rec.is_none() || mask.is_some() || paint.blend_mode != BlendMode::SourceOver {
            self.pixels().stroke_path(path, paint, stroke, transform, mask);
            return;
        }
        if let Shader::SolidColor(c) = paint.shader {
            // Dashes are cut from the path as tiny-skia cuts them, and kept as the pieces.
            let dashed = match &stroke.dash {
                Some(d) => path.dash(d, 1.0),
                None => Some(path.clone()),
            };
            let Some(kept) = dashed else { return };
            let style =
                vector::Stroke { width: stroke.width, cap: stroke.line_cap, join: stroke.line_join, miter_limit: stroke.miter_limit };
            self.keep(|r| r.drawing.stroke(kept, c, style, r.ts.pre_concat(transform)));
            return;
        }
        // The stroke reaches half its width past the path (a mitre further), scaled as the path.
        let (sx, sy) = transform.get_scale();
        let reach = stroke.width.max(1.0) * stroke.miter_limit.max(1.0) * sx.max(sy).max(1.0);
        let Some(bounds) = path.clone().transform(transform).map(|p| p.bounds()) else { return };
        let Some(bounds) = Rect::from_ltrb(bounds.left() - reach, bounds.top() - reach, bounds.right() + reach, bounds.bottom() + reach)
        else {
            return;
        };
        self.keep_picture(bounds, None, |pm, at| pm.stroke_path(path, paint, stroke, at.pre_concat(transform), None));
    }

    /// Fills `rect` with `paint`, as `Pixmap::fill_rect` does.
    pub fn fill_rect(&mut self, rect: Rect, paint: &Paint, transform: Transform, mask: Option<&Mask>) {
        if self.rec.is_none() {
            self.pm.fill_rect(rect, paint, transform, mask);
            return;
        }
        self.fill_path(&PathBuilder::from_rect(rect), paint, FillRule::Winding, transform, mask);
    }

    /// Draws `pixmap` with its top left at (`x`, `y`) moved by `transform`, as
    /// `Pixmap::draw_pixmap` does.
    pub fn draw_pixmap(&mut self, x: i32, y: i32, pixmap: PixmapRef, paint: &PixmapPaint, transform: Transform, mask: Option<&Mask>) {
        let blend = blend_of(paint.blend_mode);
        if self.rec.is_none() || mask.is_some() || blend.is_none() {
            self.pixels().draw_pixmap(x, y, pixmap, paint, transform, mask);
            return;
        }
        let at = transform.pre_translate(x as f32, y as f32);
        let smooth = paint.quality != tiny_skia::FilterQuality::Nearest;
        let (opacity, blend) = (paint.opacity, blend.unwrap_or_default());
        self.keep(|r| {
            let at = r.ts.pre_concat(at);
            if blend == Blend::Normal {
                r.drawing.image(pixmap, at, opacity, smooth);
            } else {
                let mut one = Drawing::new(r.drawing.width, r.drawing.height);
                one.image(pixmap, at, opacity, smooth);
                r.drawing.place(one, Transform::identity(), 1.0, blend, None);
            }
        });
    }

    /// Covers the whole canvas with `color`.
    pub fn fill(&mut self, color: Color) {
        if self.rec.is_none() {
            self.pm.fill(color);
            return;
        }
        let Some(r) = Rect::from_xywh(0.0, 0.0, self.width() as f32, self.height() as f32) else { return };
        self.keep(|rec| rec.drawing.fill(PathBuilder::from_rect(r), Brush::Solid(color), FillRule::Winding, rec.ts));
    }

    /// Keeps a run of glyphs, its transform taken from this canvas's space; nothing unless it
    /// records.
    pub fn glyphs(&mut self, mut run: GlyphRun) {
        self.keep(|r| {
            run.transform = r.ts.pre_concat(run.transform);
            r.drawing.glyphs(run);
        });
    }

    /// Draws with `f`, what is kept moved by `t` (from `f`'s space to this canvas's); for drawing
    /// placed with a transform where a canvas that draws pixels would draw a picture and place it.
    /// What `f` draws on the pixels is not moved.
    pub fn with_transform(&mut self, t: Transform, f: impl FnOnce(&mut Canvas)) {
        let old = self.rec.as_ref().map(|r| r.ts);
        if let Some(r) = &mut self.rec {
            r.ts = r.ts.pre_concat(t);
        }
        f(self);
        if let (Some(r), Some(old)) = (&mut self.rec, old) {
            r.ts = old;
        }
    }

    /// Draws with `f` as one group composited at `opacity` (so what `f` draws overlapping shows
    /// once); a canvas that draws pixels runs `f` on itself, for callers that only group what
    /// they record.
    pub fn group(&mut self, opacity: f32, f: impl FnOnce(&mut Canvas)) {
        if self.rec.is_none() {
            f(self);
            return;
        }
        self.keep(|r| r.drawing.begin_group(opacity, Blend::Normal, None));
        f(self);
        self.keep(|r| r.drawing.end_group());
    }

    /// Places `child` on this one as a group: moved by `transform`, composited with `opacity` and
    /// `blend`, inside `clip` (in this canvas's space). `child` comes from [`Canvas::sub`] (or
    /// draws pixels), so it records only when this canvas does: its recording is placed on this
    /// one's, its pixels on these pixels.
    pub fn place(&mut self, child: Canvas, transform: Transform, opacity: f32, blend: Blend, clip: Option<Clip>) {
        if self.rec.is_none() || !child.is_recording() {
            let paint = PixmapPaint { opacity, blend_mode: crate::blend_mode(blend), quality: tiny_skia::FilterQuality::Bilinear };
            // Cut to `clip` as the recording is: a mask the size of these pixels.
            let mask = match &clip {
                Some(c) => {
                    let Some(mut m) = Mask::new(self.pm.width(), self.pm.height()) else { return };
                    m.fill_path(&c.path, c.rule, true, c.transform);
                    Some(m)
                }
                None => None,
            };
            self.pixels().draw_pixmap(0, 0, child.pm.as_ref(), &paint, transform, mask.as_ref());
            return;
        }
        let Some(drawing) = child.into_drawing() else { return };
        self.keep(|r| {
            let clip = clip.map(|c| Clip { transform: r.ts.pre_concat(c.transform), ..c });
            r.drawing.place(drawing, r.ts.pre_concat(transform), opacity, blend, clip);
        });
    }

    /// Places what the pixels hold, if anything, then runs `f` on the recording.
    fn keep(&mut self, f: impl FnOnce(&mut Recording)) {
        self.flush();
        if let Some(r) = &mut self.rec {
            f(r);
        }
    }

    /// Keeps a picture of one thing drawn by `draw` (given where the picture's pixels lie), over
    /// `bounds` on the canvas, inside `clip`.
    fn keep_picture(&mut self, bounds: Rect, clip: Option<Clip>, draw: impl FnOnce(&mut Pixmap, Transform)) {
        let (w, h) = (self.width() as f32, self.height() as f32);
        // Cut to the canvas, unless drawn under a transform (its space isn't the canvas's), where
        // it is held to the canvas's size.
        let moved = self.rec.as_ref().is_some_and(|r| !r.ts.is_identity());
        let (lo_x, lo_y, hi_x, hi_y) =
            if moved { (bounds.left(), bounds.top(), bounds.left() + w, bounds.top() + h) } else { (0.0, 0.0, w, h) };
        let (x0, y0) = (bounds.left().max(lo_x).floor(), bounds.top().max(lo_y).floor());
        let (x1, y1) = (bounds.right().min(hi_x).ceil(), bounds.bottom().min(hi_y).ceil());
        if !(x0.is_finite() && y0.is_finite() && x1 > x0 && y1 > y0) {
            return;
        }
        let Some(mut pic) = Pixmap::new((x1 - x0) as u32, (y1 - y0) as u32) else { return };
        draw(&mut pic, Transform::from_translate(-x0, -y0));
        self.keep(|r| match clip {
            Some(c) => {
                let mut one = Drawing::new(r.drawing.width, r.drawing.height);
                one.image(pic.as_ref(), Transform::from_translate(x0, y0), 1.0, true);
                let c = Clip { transform: r.ts.pre_concat(c.transform), ..c };
                r.drawing.place(one, r.ts, 1.0, Blend::Normal, Some(c));
            }
            None => r.drawing.image(pic.as_ref(), r.ts.pre_concat(Transform::from_translate(x0, y0)), 1.0, true),
        });
    }

    /// Places what the pixels hold since they were last placed, cropped to it, and clears them.
    fn flush(&mut self) {
        let Some(r) = &mut self.rec else { return };
        if !r.dirty {
            return;
        }
        r.dirty = false;
        let w = self.pm.width() as usize;
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
        for (i, p) in self.pm.pixels().iter().enumerate() {
            if p.alpha() > 0 {
                let (x, y) = (i % w, i / w);
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            }
        }
        if x0 <= x1
            && let Some(rect) = IntRect::from_ltrb(x0 as i32, y0 as i32, x1 as i32 + 1, y1 as i32 + 1)
            && let Some(pic) = self.pm.clone_rect(rect)
        {
            // In the canvas's own pixels: drawing on them isn't moved by a transform.
            r.drawing.image(pic.as_ref(), Transform::from_translate(x0 as f32, y0 as f32), 1.0, true);
            // Cleared where it was drawn: everything else is still transparent.
            for row in self.pm.pixels_mut().chunks_mut(w).skip(y0).take(y1 + 1 - y0) {
                if let Some(cells) = row.get_mut(x0..=x1) {
                    cells.fill(tiny_skia::PremultipliedColorU8::TRANSPARENT);
                }
            }
        }
    }
}

/// The blend a recording keeps for tiny-skia's `mode`; `None` for one it can't.
fn blend_of(mode: BlendMode) -> Option<Blend> {
    Some(match mode {
        BlendMode::SourceOver => Blend::Normal,
        BlendMode::Multiply => Blend::Multiply,
        BlendMode::Screen => Blend::Screen,
        BlendMode::Overlay => Blend::Overlay,
        BlendMode::Darken => Blend::Darken,
        BlendMode::Lighten => Blend::Lighten,
        BlendMode::Difference => Blend::Difference,
        _ => return None,
    })
}

/// Lends `pm` to a canvas for `f` (tests that draw onto a pixmap and then read it).
#[cfg(test)]
pub(crate) fn with_canvas<R>(pm: &mut Pixmap, f: impl FnOnce(&mut Canvas) -> R) -> R {
    let empty = Pixmap::new(1, 1).expect("a 1 × 1 pixmap");
    let mut c = Canvas::from_pixmap(std::mem::replace(pm, empty));
    let r = f(&mut c);
    *pm = c.into_pixmap();
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector::Item;

    fn square(x: f32) -> Path {
        PathBuilder::from_rect(Rect::from_xywh(x, 10.0, 20.0, 20.0).unwrap())
    }

    fn solid(r: u8) -> Paint<'static> {
        let mut p = Paint::default();
        p.set_color_rgba8(r, 0, 0, 255);
        p
    }

    #[test]
    fn a_recording_keeps_paths_as_vectors_and_pixels_as_pictures_in_order() {
        let mut c = Canvas::recording(100, 50).unwrap();
        c.fill_path(&square(0.0), &solid(200), FillRule::Winding, Transform::identity(), None);
        // Drawn on the pixels (a raster, say): placed as a picture before what comes next.
        c.pixels().fill_rect(Rect::from_xywh(40.0, 10.0, 5.0, 5.0).unwrap(), &solid(10), Transform::identity(), None);
        let dashed = Stroke { width: 2.0, dash: tiny_skia::StrokeDash::new(vec![4.0, 4.0], 0.0), ..Stroke::default() };
        c.stroke_path(&square(50.0), &solid(30), &dashed, Transform::identity(), None);
        // A hatch or gradient: a picture of the fill alone, cut to the path.
        let shader = tiny_skia::LinearGradient::new(
            tiny_skia::Point::from_xy(0.0, 0.0),
            tiny_skia::Point::from_xy(100.0, 0.0),
            vec![tiny_skia::GradientStop::new(0.0, Color::BLACK), tiny_skia::GradientStop::new(1.0, Color::WHITE)],
            tiny_skia::SpreadMode::Repeat,
            Transform::identity(),
        )
        .unwrap();
        c.fill_path(&square(75.0), &Paint { shader, ..Paint::default() }, FillRule::Winding, Transform::identity(), None);
        assert!(!c.overflowed());
        let (_, _, items) = c.into_drawing().unwrap().finish();
        let kinds: Vec<&str> = items
            .iter()
            .map(|i| match i {
                Item::Fill { .. } => "fill",
                Item::Stroke { .. } => "stroke",
                Item::Image(_) => "picture",
                Item::Group(g) if g.clip.is_some() => "clipped",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, ["fill", "picture", "stroke", "clipped"]);
        let Item::Image(pic) = &items[1] else { panic!("the item expected") };
        assert_eq!((pic.width, pic.height, pic.transform.tx, pic.transform.ty), (5, 5, 40.0, 10.0), "cropped to what was drawn");
        let Item::Stroke { path, stroke, .. } = &items[2] else { panic!("the item expected") };
        // The dashes cut from the square's 80 px outline, kept solid.
        assert!(stroke.width == 2.0 && path.segments().filter(|s| matches!(s, tiny_skia::PathSegment::MoveTo(_))).count() == 10);
    }

    #[test]
    fn placing_pixels_keeps_inside_the_clip() {
        let mut c = Canvas::new(40, 20).unwrap();
        let mut child = c.sub(40, 20).unwrap();
        child.pixels().fill(Color::from_rgba8(200, 0, 0, 255));
        let clip = Clip {
            path: PathBuilder::from_rect(Rect::from_xywh(0.0, 0.0, 20.0, 20.0).unwrap()),
            rule: FillRule::Winding,
            transform: Transform::identity(),
        };
        c.place(child, Transform::identity(), 1.0, Blend::Normal, Some(clip));
        let pm = c.into_pixmap();
        assert_eq!(pm.pixel(5, 10).map(|p| p.alpha()), Some(255), "inside the clip");
        assert_eq!(pm.pixel(30, 10).map(|p| p.alpha()), Some(0), "outside the clip");
    }

    #[test]
    fn placing_pixels_cuts_to_the_clip_where_its_own_transform_puts_it() {
        let mut c = Canvas::new(40, 20).unwrap();
        let mut child = c.sub(10, 20).unwrap();
        child.pixels().fill(Color::from_rgba8(200, 0, 0, 255));
        // The clip covers x 20 to 30, the child x 15 to 25: only 20 to 25 is drawn.
        let clip = Clip {
            path: PathBuilder::from_rect(Rect::from_xywh(0.0, 0.0, 10.0, 20.0).unwrap()),
            rule: FillRule::Winding,
            transform: Transform::from_translate(20.0, 0.0),
        };
        c.place(child, Transform::from_translate(15.0, 0.0), 1.0, Blend::Normal, Some(clip));
        let pm = c.into_pixmap();
        let alpha = |x| pm.pixel(x, 10).map(|p| p.alpha());
        assert_eq!(alpha(22), Some(255), "the child inside the clip");
        assert_eq!(alpha(17), Some(0), "the child outside the clip");
        assert_eq!(alpha(27), Some(0), "the clip past the child");
    }

    #[test]
    fn a_label_keeps_its_halo_as_a_shape_and_its_text_once() {
        let label = crate::PlacedLabel {
            text: "Memphis".into(),
            x: 50.0,
            y: 25.0,
            size: 12.0,
            color: rustgis_map::Rgba::rgb(20, 20, 20),
            halo: Some(rustgis_map::Rgba::rgb(255, 255, 255)),
            halo_width: None,
            bold: false,
            icon: None,
            align: 0.0,
            framed: false,
            italic: false,
            spacing: 0.0,
            font: None,
            line_align: rustgis_map::layout::TextAlign::Left,
            angle: Some(0.5),
            collide: Default::default(),
            glyphs: Vec::new(),
            background: None,
        };
        let mut c = Canvas::recording(100, 50).unwrap();
        crate::text::draw(&mut c, &label, 1.0);
        let (_, _, items) = c.into_drawing().unwrap().finish();
        let [Item::Group(halo), Item::Glyphs(run)] = items.as_slice() else { panic!("a halo and the text") };
        // The halo: the letters and their outline spread round them, opaque, in one coat.
        assert!(matches!(halo.items.as_slice(), [Item::Fill { .. }, Item::Stroke { .. }]));
        assert_eq!(run.glyphs.iter().map(|g| g.ch).collect::<String>(), "Memphis");
        // Turned with the label, not drawn as a turned picture.
        assert!(run.transform.ky.abs() > 0.1);
        // A halo of no width draws nothing, as on the pixels.
        let mut c = Canvas::recording(100, 50).unwrap();
        crate::text::draw(&mut c, &crate::PlacedLabel { halo_width: Some(0.0), ..label }, 1.0);
        let (_, _, items) = c.into_drawing().unwrap().finish();
        assert!(matches!(items.as_slice(), [Item::Glyphs(_)]));
    }
}
