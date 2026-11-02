//! Reuse a single feature path without changing feature or stroke compositing order.
use super::{Canvas, Ctx, LineStroke, PathBuilder, Symbol};
use rustgis_geom::Coord;
// Retain paths up to this point count per draw chunk; this is not a byte/capacity budget.
const CACHE_POINTS: usize = 16_384;

#[derive(Default)]
pub(super) struct Paths {
    cached: Option<tiny_skia::Path>,
}
impl Paths {
    /// Draws a line's parts as its stacked `strokes`, reusing the last path's storage.
    pub fn draw(&mut self, ctx: &Ctx<'_>, pm: &mut Canvas, parts: &[Vec<Coord>], symbol: &Symbol, strokes: &[LineStroke]) {
        let mut builder = PathBuilder::new();
        if let Some(path) = self.cached.take() {
            builder = path.clear();
        }
        let mut starts = super::DashStarts::default();
        for part in parts {
            ctx.ring_path_at(&mut builder, part, false, &mut starts);
        }
        let Some(path) = builder.finish() else { return };
        ctx.stacked_path(pm, &path, symbol, strokes, &starts, parts);
        self.remember(path);
    }
    fn remember(&mut self, path: tiny_skia::Path) {
        self.cached = if path.points().len() <= CACHE_POINTS { Some(path) } else { None };
    }
}
#[cfg(test)]
mod tests;
