//! The picture of what a publication layout's map covers.

use super::*;

/// The coverage grid of a publication's frame (`frame`, page mm) as a PNG, three pixels a cell
/// (#1379): a cell the map draws dark, a free one white, both tinted blue outside `area` (the
/// strips the grid's labels keep, which the search leaves out), and each surround placed over the
/// map (`placed`, page mm) outlined in red.
pub(in crate::cmd::layout) fn coverage_png(
    (gw, gh, cells): &(u32, u32, Vec<bool>),
    frame: Rect,
    area: (f64, f64, f64, f64),
    placed: &[Rect],
) -> Result<Vec<u8>, String> {
    const CELL_PX: u32 = 3;
    let (fx, fy, fw, fh) = frame;
    let mut pm = tiny_skia::Pixmap::new(gw * CELL_PX, gh * CELL_PX).ok_or("the coverage grid is empty")?;
    let paint = |r: u8, g: u8, b: u8| {
        let mut paint = tiny_skia::Paint::default();
        paint.set_color_rgba8(r, g, b, 255);
        paint
    };
    let (dark, free, dark_band, free_band) =
        (paint(0x33, 0x33, 0x33), paint(0xFF, 0xFF, 0xFF), paint(0x2E, 0x4A, 0x7A), paint(0xC9, 0xDD, 0xF2));
    for (i, drawn) in cells.iter().enumerate() {
        let (col, row) = ((i % *gw as usize) as f64, (i / *gw as usize) as f64);
        let (x, y) = (fx + (col + 0.5) / f64::from(*gw) * fw, fy + (row + 0.5) / f64::from(*gh) * fh);
        let band = x < area.0 || x > area.2 || y < area.1 || y > area.3;
        let colour = match (*drawn, band) {
            (true, false) => &dark,
            (false, false) => &free,
            (true, true) => &dark_band,
            (false, true) => &free_band,
        };
        let px = CELL_PX as f32;
        if let Some(cell) = tiny_skia::Rect::from_xywh(col as f32 * px, row as f32 * px, px, px) {
            pm.fill_rect(cell, colour, tiny_skia::Transform::identity(), None);
        }
    }
    let (sx, sy) = (f64::from(pm.width()) / fw, f64::from(pm.height()) / fh);
    let line = tiny_skia::Stroke { width: 2.0, ..tiny_skia::Stroke::default() };
    for (x, y, w, h) in placed {
        let r = tiny_skia::Rect::from_xywh(((x - fx) * sx) as f32, ((y - fy) * sy) as f32, (w * sx) as f32, (h * sy) as f32);
        if let Some(r) = r {
            pm.stroke_path(&tiny_skia::PathBuilder::from_rect(r), &paint(0xD6, 0x27, 0x28), &line, tiny_skia::Transform::identity(), None);
        }
    }
    pm.encode_png().map_err(|e| e.to_string())
}
