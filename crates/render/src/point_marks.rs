//! A point's marks (a point layer's stacked strokes of markers, `Layer::line_strokes`) and markers
//! turned by a field (`Layer::marker_rotation`), as QGIS draws a marker symbol of several layers:
//! each mark at the point, bottom first, shifted by its offset (right and down), and turned
//! clockwise by its own angle and, where it turns ([`LineMarkers::rotate`]), the point's: its
//! offset turned with it.

use crate::Canvas;
#[cfg(doc)]
use rustgis_map::LineMarkers;
use rustgis_map::{LineMark, LineStroke, Rgba, Symbol};
use tiny_skia::{FillRule, LineCap, LineJoin, PathBuilder, Stroke, Transform};

use crate::{marker_path, paint};

/// One mark: its shape, size (points), fill, outline (colour and width in points), offset (points,
/// right and down) and angle (degrees clockwise).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Mark {
    shape: LineMark,
    size: f32,
    fill: Rgba,
    outline: Option<(Rgba, f32)>,
    offset: (f32, f32),
    angle: f32,
    /// Turned by the point's angle too.
    turns: bool,
}

/// The marks a point of symbol `sym` draws: `strokes`' marks (a stroke's colour, else the symbol's
/// fill; its width, else the symbol's outline width), else the symbol's own marker.
fn marks(sym: &Symbol, strokes: &[LineStroke]) -> Vec<Mark> {
    if strokes.is_empty() {
        let outline = (sym.stroke_width > 0.0 && sym.stroke.a() > 0).then_some((sym.stroke, sym.stroke_width));
        return vec![Mark {
            shape: LineMark::Marker(sym.marker),
            size: sym.size.max(1.0),
            fill: sym.fill,
            outline,
            offset: (0.0, 0.0),
            angle: 0.0,
            turns: true,
        }];
    }
    strokes
        .iter()
        .filter_map(|st| {
            let m = st.markers?;
            let width = st.width.unwrap_or(sym.stroke_width).max(0.0);
            let fill = st.color.unwrap_or(sym.fill);
            Some(Mark {
                shape: m.mark,
                size: m.size.max(0.0),
                // A hash mark is a line: its colour, as wide as the stroke.
                fill,
                outline: match m.mark {
                    LineMark::Hash => Some((fill, width)),
                    LineMark::Marker(_) => m.outline.filter(|c| c.a() > 0 && width > 0.0).map(|c| (c, width)),
                },
                offset: (m.offset_along, st.offset),
                angle: m.angle,
                turns: m.rotate,
            })
        })
        .collect()
}

/// Draws a point of symbol `sym` at (`x`, `y`) device pixels: `strokes`' marks, else its marker,
/// turned `turn` degrees clockwise about the point; with `halo`, a selection halo round each.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw(pm: &mut Canvas, x: f32, y: f32, sym: &Symbol, strokes: &[LineStroke], turn: f32, halo: Option<Rgba>, ppp: f32) {
    if !(x.is_finite() && y.is_finite()) {
        return;
    }
    let marks = marks(sym, strokes);
    // Every mark's halo under all the marks, so none covers the one below it.
    for (m, ink) in marks.iter().map(|m| (m, false)).chain(marks.iter().map(|m| (m, true))) {
        if !ink && halo.is_none() {
            continue;
        }
        let angle = if m.turns { turn } else { 0.0 } + m.angle;
        let (sin, cos) = angle.to_radians().sin_cos();
        let (ox, oy) = (m.offset.0 * ppp, m.offset.1 * ppp);
        // Turned clockwise on the screen (y down), as the mark is.
        let (dx, dy) = (ox * cos - oy * sin, ox * sin + oy * cos);
        let t = Transform::from_rotate(angle).post_translate(x + dx, y + dy);
        let size = m.size * ppp;
        let hash = matches!(m.shape, LineMark::Hash);
        let path = match m.shape {
            LineMark::Marker(shape) => marker_path(0.0, 0.0, size.max(ppp) / 2.0, shape),
            // Across a line running east: up and down.
            LineMark::Hash => {
                let mut pb = PathBuilder::new();
                pb.move_to(0.0, -size / 2.0);
                pb.line_to(0.0, size / 2.0);
                pb.finish()
            }
        };
        let Some(path) = path else { continue };
        if !ink {
            let h = halo.unwrap_or(Rgba::new(0, 0, 0, 0));
            let width = 3.5 * ppp + if hash { m.outline.map_or(0.0, |o| o.1 * ppp) } else { 0.0 };
            let st = Stroke { width, line_join: LineJoin::Round, line_cap: LineCap::Round, ..Stroke::default() };
            pm.stroke_path(&path, &paint(h, 1.0), &st, t, None);
            continue;
        }
        if !hash && m.fill.a() > 0 {
            pm.fill_path(&path, &paint(m.fill, 1.0), FillRule::Winding, t, None);
        }
        if let Some((c, w)) = m.outline {
            let cap = if hash { LineCap::Butt } else { LineCap::Round };
            let st = Stroke { width: (w * ppp).max(0.5), line_join: LineJoin::Round, line_cap: cap, ..Stroke::default() };
            pm.stroke_path(&path, &paint(c, 1.0), &st, t, None);
        }
    }
}

/// How far from the point (points) any of the marks reaches, for culling: a mark's offset and its
/// size (a character as far as its outline).
pub(crate) fn reach(sym: &Symbol, strokes: &[LineStroke]) -> f32 {
    marks(sym, strokes)
        .iter()
        .map(|m| {
            let extent = match m.shape {
                LineMark::Marker(shape) => crate::marker_reach(shape).max(0.6),
                LineMark::Hash => 0.5,
            };
            m.offset.0.hypot(m.offset.1) + m.size * extent + m.outline.map_or(0.0, |o| o.1)
        })
        .fold(0.0, f32::max)
}

/// Whether a symbol's own marker would draw the same as [`draw`] does without strokes and turn (the
/// fast paths, sprites and GPU instances, draw it then).
pub(crate) fn plain(strokes: &[LineStroke], turn: f32) -> bool {
    strokes.is_empty() && turn == 0.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_data::GeometryType;
    use rustgis_map::{Glyph, LineMarkers, Marker};
    use tiny_skia::Pixmap;

    fn ink(pm: &Pixmap, x: u32, y: u32) -> bool {
        pm.pixel(x, y).is_some_and(|p| p.alpha() > 200)
    }

    fn mark(shape: Marker, color: Rgba, size: f32, offset: (f32, f32), angle: f32) -> LineStroke {
        let m = LineMarkers { mark: LineMark::Marker(shape), size, offset_along: offset.0, angle, ..LineMarkers::default() };
        LineStroke { color: Some(color), width: Some(0.0), offset: offset.1, markers: Some(m), ..LineStroke::default() }
    }

    #[test]
    fn marks_stack_at_the_point_shifted_and_turned_as_qgis_turns_them() {
        let sym = Symbol { size: 6.0, ..Symbol::default_for(GeometryType::Point, 0) };
        let (red, black) = (Rgba::rgb(255, 0, 0), Rgba::rgb(0, 0, 0));
        // A big red square under a small black one, and a black one 20 pt to the right.
        let strokes = [mark(Marker::Square, red, 20.0, (0.0, 0.0), 0.0), mark(Marker::Square, black, 6.0, (20.0, 0.0), 0.0)];
        let mut pm = Pixmap::new(100, 100).unwrap();
        crate::canvas::with_canvas(&mut pm, |c| draw(c, 50.0, 50.0, &sym, &strokes, 0.0, None, 1.0));
        let at = |pm: &Pixmap, x, y| pm.pixel(x, y).map(|p| (p.red(), p.alpha()));
        assert_eq!(at(&pm, 50, 50), Some((255, 255)), "the red one at the point");
        assert_eq!(at(&pm, 70, 50), Some((0, 255)), "the black one 20 pt right");
        // Turned 90° clockwise: the offset turns with it, to 20 pt below.
        let mut pm = Pixmap::new(100, 100).unwrap();
        crate::canvas::with_canvas(&mut pm, |c| draw(c, 50.0, 50.0, &sym, &strokes, 90.0, None, 1.0));
        assert!(ink(&pm, 50, 70) && !ink(&pm, 70, 50));
        // A mark's own angle turns its offset too, and adds to the point's.
        let own = [mark(Marker::Square, black, 6.0, (20.0, 0.0), 90.0)];
        let mut pm = Pixmap::new(100, 100).unwrap();
        crate::canvas::with_canvas(&mut pm, |c| draw(c, 50.0, 50.0, &sym, &own, -90.0, None, 1.0));
        assert!(ink(&pm, 70, 50), "-90 + 90: not turned");
        // A mark that doesn't turn keeps its own angle alone.
        let mut fixed = own;
        if let Some(m) = &mut fixed[0].markers {
            m.rotate = false;
        }
        let mut pm = Pixmap::new(100, 100).unwrap();
        crate::canvas::with_canvas(&mut pm, |c| draw(c, 50.0, 50.0, &sym, &fixed, -90.0, None, 1.0));
        assert!(ink(&pm, 50, 70) && !ink(&pm, 70, 50), "its own 90° only");
        // No strokes: the symbol's marker, turned (a triangle's tip goes from up to right).
        let tri = Symbol { marker: Marker::Triangle, size: 30.0, stroke_width: 0.0, ..Symbol::default_for(GeometryType::Point, 0) };
        let mut pm = Pixmap::new(100, 100).unwrap();
        crate::canvas::with_canvas(&mut pm, |c| draw(c, 50.0, 50.0, &tri, &[], 90.0, None, 1.0));
        assert!(ink(&pm, 64, 50) && !ink(&pm, 50, 35));
        // A stroke of no markers draws nothing at a point; a character draws.
        let line = LineStroke::default();
        let mut pm = Pixmap::new(100, 100).unwrap();
        crate::canvas::with_canvas(&mut pm, |c| draw(c, 50.0, 50.0, &sym, &[line], 0.0, None, 1.0));
        assert!(pm.pixels().iter().all(|p| p.alpha() == 0));
        let chr = [mark(Marker::Char(Glyph::new('I', "")), black, 40.0, (0.0, 0.0), 0.0)];
        crate::canvas::with_canvas(&mut pm, |c| draw(c, 50.0, 50.0, &sym, &chr, 0.0, None, 1.0));
        assert!(pm.pixels().iter().any(|p| p.alpha() > 200));
        assert!(reach(&sym, &strokes) >= 23.0 && plain(&[], 0.0) && !plain(&strokes, 0.0) && !plain(&[], 1.0));
    }
}
