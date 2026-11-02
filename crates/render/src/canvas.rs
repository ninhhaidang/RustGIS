//! What the renderer draws on. Maps, layouts and reports draw through a [`Canvas`] rather than
//! straight onto a `tiny_skia::Pixmap`: its drawing calls are tiny-skia's own, and [`Canvas::pixels`]
//! hands out the pixmap for work that is pixels by nature (rasters, tiles, glyph coverage, effects).
//! Today a canvas draws onto its pixmap; it is the place a drawing kept as vectors (a vector PDF)
//! will be recorded.

use tiny_skia::{Color, FillRule, Mask, Paint, Path, Pixmap, PixmapPaint, PixmapRef, Rect, Stroke, Transform};

/// See the module.
pub struct Canvas {
    pm: Pixmap,
}

impl Canvas {
    /// A transparent canvas `width` × `height` pixels; `None` for a size tiny-skia can't hold.
    pub fn new(width: u32, height: u32) -> Option<Canvas> {
        Pixmap::new(width, height).map(Canvas::from_pixmap)
    }

    /// A canvas drawing onto `pm`, keeping what it has.
    pub fn from_pixmap(pm: Pixmap) -> Canvas {
        Canvas { pm }
    }

    /// The pixels drawn.
    pub fn into_pixmap(self) -> Pixmap {
        self.pm
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
        &mut self.pm
    }

    /// Fills `path` with `paint`, as `Pixmap::fill_path` does.
    pub fn fill_path(&mut self, path: &Path, paint: &Paint, rule: FillRule, transform: Transform, mask: Option<&Mask>) {
        self.pm.fill_path(path, paint, rule, transform, mask);
    }

    /// Strokes `path` with `paint` and `stroke`, as `Pixmap::stroke_path` does.
    pub fn stroke_path(&mut self, path: &Path, paint: &Paint, stroke: &Stroke, transform: Transform, mask: Option<&Mask>) {
        self.pm.stroke_path(path, paint, stroke, transform, mask);
    }

    /// Fills `rect` with `paint`, as `Pixmap::fill_rect` does.
    pub fn fill_rect(&mut self, rect: Rect, paint: &Paint, transform: Transform, mask: Option<&Mask>) {
        self.pm.fill_rect(rect, paint, transform, mask);
    }

    /// Draws `pixmap` with its top left at (`x`, `y`) moved by `transform`, as
    /// `Pixmap::draw_pixmap` does.
    pub fn draw_pixmap(&mut self, x: i32, y: i32, pixmap: PixmapRef, paint: &PixmapPaint, transform: Transform, mask: Option<&Mask>) {
        self.pm.draw_pixmap(x, y, pixmap, paint, transform, mask);
    }

    /// Covers the whole canvas with `color`.
    pub fn fill(&mut self, color: Color) {
        self.pm.fill(color);
    }
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
