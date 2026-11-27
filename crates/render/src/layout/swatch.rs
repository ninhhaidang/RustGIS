//! The symbol a legend row shows.

use super::*;

/// A legend entry's symbol `s` of the layer `l` in the box (`x`, `y`, `w`, `h`), with its stacked
/// `strokes` and `fills` (#2490): a point as [`point_swatch`] draws it, or a line or a filled box a
/// swatch wide in the middle of the box (`across` pixels: [`LEGEND_SWATCH`] in the legend's size),
/// by the paths the map draws its features with, its widths at `ppp`. A line or polygon swatch's
/// ink ([`swatch_ink`]) is held to its row's top and bottom and, at its sides, to the `room` (left,
/// right, in pixels) its row leaves empty: left, its indent and what lies left of its column, none
/// at the legend's edge; right, the gap before its text. Stacked, see-through (`opacity`), grown
/// with the layer's reference scale at 1:`scale` or reaching past that, it is composited as one and
/// cut there.
pub(super) fn swatch(
    pm: &mut Canvas,
    (x, y, w, h): (f32, f32, f32, f32),
    l: &rustgis_map::Layer,
    (s, strokes, fills): (&rustgis_map::Symbol, &[rustgis_map::LineStroke], &[rustgis_map::fills::FillLayer]),
    opacity: f32,
    scale: f64,
    (ppp, across, (lead, gap)): (f32, f32, (f32, f32)),
) {
    let gt = l.geometry_type();
    let k = l.size_factor(scale);
    if gt == GeometryType::Point {
        point_swatch(pm, (x, y, w, h), (s, strokes), opacity, ppp * k);
        return;
    }
    // Every line or polygon swatch is cut to its row's top and bottom, so a line wider than its row,
    // or shifted off it, doesn't paint over the rows beside it (#2518), and at its sides to the room
    // its row leaves empty, so it doesn't paint over its label, the column before or past the
    // legend's edge; within those it keeps its ink, as wide as the map draws it (#2581).
    let plain = if gt == GeometryType::Polyline { strokes.is_empty() } else { fills.is_empty() };
    let ink = swatch_ink((x, y, w, h), gt, (s, strokes, fills), (across, ppp * k));
    let (lo, hi) = (x - lead, x + w + gap);
    let within = ink.is_none_or(|b| b.top() >= y - 0.01 && b.bottom() <= y + h + 0.01 && b.left() >= lo - 0.01 && b.right() <= hi + 0.01);
    if plain && opacity >= 0.999 && k == 1.0 && within {
        // A layer with no stack, within its row: the swatch it always had.
        flat_swatch(pm, (x, y, w, h), gt, s, (across, ppp));
        return;
    }
    let (left, right) = ink.map_or((x, x + w), |b| (x.min(b.left()).max(lo), (x + w).max(b.right()).min(hi)));
    composited(pm, (left, y, right - left, h), opacity, |c, (cl, ct)| {
        let cell = (x - cl, y - ct, w, h);
        if plain {
            flat_swatch(c, cell, gt, s, (across, ppp * k));
        } else {
            stacked_swatch(c, cell, l, (s, strokes, fills), (ppp, across), k);
        }
    });
}

/// Where a line or polygon swatch drawn in `cell`, `across` pixels wide, its widths at `wppp`,
/// puts ink: a plain one's line stroked as [`flat_swatch`] strokes it (its joins too); a stacked
/// line's `strokes` the map draws, as it strokes them ([`crate::line_stroke`]), an offset one as
/// wide as its width and twice its offset, mitred, as its shifted line's corners are
/// (`offset_polyline`), a marked one as far as [`crate::line_markers::reach`]; a stacked polygon's
/// box and its outline. `None` for no shape. [`swatch`] keeps it within its row and the room its
/// row leaves empty.
pub(super) fn swatch_ink(
    cell: (f32, f32, f32, f32),
    gt: GeometryType,
    (s, strokes, fills): (&rustgis_map::Symbol, &[rustgis_map::LineStroke], &[rustgis_map::fills::FillLayer]),
    (across, wppp): (f32, f32),
) -> Option<Rect> {
    let line = gt == GeometryType::Polyline;
    if (line && strokes.is_empty()) || (!line && fills.is_empty()) {
        let (p, stroke) = flat_swatch_outline(cell, gt, s, (across, wppp))?;
        return p.stroke(&stroke, 1.0).map(|p| p.bounds());
    }
    if !line {
        let b = swatch_box(cell, across)?;
        let o = if s.stroke.a() > 0 { s.stroke_width.max(0.0) * wppp / 2.0 } else { 0.0 };
        return Rect::from_ltrb(b.left() - o, b.top() - o, b.right() + o, b.bottom() + o);
    }
    let path = swatch_line_path(cell, across)?;
    let b = path.bounds();
    strokes
        .iter()
        .filter_map(|st| {
            let (c, w) = st.resolve(s);
            let w = if w.is_finite() { w.max(0.0) } else { 0.0 };
            if st.markers.is_some() {
                let r = crate::line_markers::reach(std::slice::from_ref(st), w) * wppp;
                return Rect::from_ltrb(b.left() - r, b.top() - r, b.right() + r, b.bottom() + r);
            }
            // One the map skips (no width or no colour) puts no ink.
            if w <= 0.0 || c.a() == 0 {
                return None;
            }
            let mut stroke = crate::line_stroke(st, (w * wppp).max(0.5) + 2.0 * st.offset.abs() * wppp);
            if st.offset != 0.0 {
                stroke.line_join = tiny_skia::LineJoin::Miter;
            }
            path.stroke(&stroke, 1.0).map(|p| p.bounds())
        })
        .reduce(|a, c| {
            Rect::from_ltrb(a.left().min(c.left()), a.top().min(c.top()), a.right().max(c.right()), a.bottom().max(c.bottom())).unwrap_or(a)
        })
}

/// A line swatch's zigzag ([`swatch_line`]) as a path.
pub(super) fn swatch_line_path(cell: (f32, f32, f32, f32), across: f32) -> Option<tiny_skia::Path> {
    let [a, b, c] = swatch_line(cell, across);
    let mut pb = PathBuilder::new();
    pb.move_to(a.0, a.1);
    pb.line_to(b.0, b.1);
    pb.line_to(c.0, c.1);
    pb.finish()
}

/// A plain line or polygon swatch's line, `across` pixels wide in the box: a line's zigzag or a
/// polygon's box, and the stroke it is drawn with, its width at `wppp`.
pub(super) fn flat_swatch_outline(
    cell: (f32, f32, f32, f32),
    gt: GeometryType,
    s: &rustgis_map::Symbol,
    (across, wppp): (f32, f32),
) -> Option<(tiny_skia::Path, Stroke)> {
    if gt == GeometryType::Polyline {
        Some((swatch_line_path(cell, across)?, Stroke { width: s.stroke_width.max(1.0) * wppp, ..Stroke::default() }))
    } else {
        Some((PathBuilder::from_rect(swatch_box(cell, across)?), Stroke { width: s.stroke_width.max(0.5) * wppp, ..Stroke::default() }))
    }
}

/// The part of the box (`x`, `w`) a line or polygon swatch `across` pixels wide takes, in its
/// middle.
pub(super) fn swatch_across(x: f32, w: f32, across: f32) -> (f32, f32) {
    (x + (w - across) / 2.0, across)
}

/// The three points of a line swatch `across` pixels wide in the box, in pixels.
pub(super) fn swatch_line((x, y, w, h): (f32, f32, f32, f32), across: f32) -> [(f32, f32); 3] {
    let (x, w) = swatch_across(x, w, across);
    let cy = y + h / 2.0;
    [(x, cy + h * 0.2), (x + w * 0.4, cy - h * 0.2), (x + w, cy + h * 0.1)]
}

/// A polygon swatch's box, `across` pixels wide, in the box, in pixels.
pub(super) fn swatch_box((x, y, w, h): (f32, f32, f32, f32), across: f32) -> Option<Rect> {
    let (x, w) = swatch_across(x, w, across);
    Rect::from_xywh(x, y + h * 0.2, w, h * 0.6)
}

/// A line or polygon swatch of the symbol `s` alone, `across` pixels wide, its widths at `wppp`.
pub(super) fn flat_swatch(pm: &mut Canvas, cell: (f32, f32, f32, f32), gt: GeometryType, s: &rustgis_map::Symbol, at: (f32, f32)) {
    let Some((p, stroke)) = flat_swatch_outline(cell, gt, s, at) else { return };
    if gt != GeometryType::Polyline {
        pm.fill_rect(p.bounds(), &paint(s.fill, 1.0), Transform::identity(), None);
    }
    pm.stroke_path(&p, &paint(s.stroke, 1.0), &stroke, Transform::identity(), None);
}

/// A line or polygon swatch with its stacked `strokes` or `fills`, drawn by the map's own paths
/// for a feature of that shape: [`crate::stroke_paths::Paths`] and [`crate::Ctx::filled`], `across`
/// pixels wide, at `ppp` and the layer's reference scale `k`. The canvas stands for a view whose
/// map units are its points, y up, so the shape's pixels are its coordinates.
pub(super) fn stacked_swatch(
    pm: &mut Canvas,
    cell: (f32, f32, f32, f32),
    l: &rustgis_map::Layer,
    (s, strokes, fills): (&rustgis_map::Symbol, &[rustgis_map::LineStroke], &[rustgis_map::fills::FillLayer]),
    (ppp, across): (f32, f32),
    k: f32,
) {
    let (w, h) = (f64::from(pm.width()) / f64::from(ppp), f64::from(pm.height()) / f64::from(ppp));
    let camera = rustgis_map::Camera::new(Coord::new(w / 2.0, -h / 2.0), 1.0);
    let view = View { width: w, height: h, pixels_per_point: f64::from(ppp), camera, sky: false };
    let ctx = crate::Ctx {
        view: &view,
        ppp: ppp * k,
        wireframe: false,
        // (The swatch's own view: the stroke and fill paths don't cull today, and one that does
        // still keeps the swatch, where an empty extent would drop it.)
        cull: crate::cull_extent(&view, crate::CULL_MARGIN),
        globe: None,
        sprites: Default::default(),
        bursts: Default::default(),
    };
    let at = |(x, y): (f32, f32)| Coord::new(f64::from(x / ppp), -f64::from(y / ppp));
    if l.geometry_type() == GeometryType::Polyline {
        // Drawn level by level, as the map draws a feature's strokes with symbol levels on.
        let mut ordered: Vec<(usize, rustgis_map::LineStroke)> = strokes.iter().copied().enumerate().collect();
        if l.stroke_levels {
            ordered.sort_by_key(|(i, st)| st.level_at(*i));
        }
        let ordered: Vec<rustgis_map::LineStroke> = ordered.into_iter().map(|(_, st)| st).collect();
        let part: Vec<Coord> = swatch_line(cell, across).into_iter().map(at).collect();
        crate::stroke_paths::Paths::default().draw(&ctx, pm, &[part], s, &ordered);
    } else if let Some(b) = swatch_box(cell, across) {
        let ring: Vec<Coord> =
            [(b.left(), b.top()), (b.right(), b.top()), (b.right(), b.bottom()), (b.left(), b.bottom()), (b.left(), b.top())]
                .into_iter()
                .map(at)
                .collect();
        ctx.filled(pm, &[vec![ring]], s, fills);
    }
}

/// A point's symbol at (`x`, `y`), unturned, by the path the map takes for it: its marker, or
/// `strokes`' stacked marks.
pub(super) fn draw_point(pm: &mut Canvas, (x, y): (f32, f32), s: &rustgis_map::Symbol, strokes: &[rustgis_map::LineStroke], ppp: f32) {
    if point_marks::plain(strokes, 0.0) {
        crate::draw_marker(pm, x, y, s, 1.0, None, ppp);
    } else {
        point_marks::draw(pm, x, y, s, strokes, 0.0, None, ppp);
    }
}

/// A point layer's symbol in the middle of the box, as the map draws its points: `strokes`' stacked
/// marks or its marker, at `ppp` (with the layer's reference scale), unturned. See-through, the
/// marks are composited as one, as the map composites the layer, in a picture no larger than the
/// page. So are those too large for the box (past [`LegendRow::symbol_side`]'s limit), which the
/// picture cuts to it.
pub(super) fn point_swatch(
    pm: &mut Canvas,
    (x, y, w, h): (f32, f32, f32, f32),
    (s, strokes): (&rustgis_map::Symbol, &[rustgis_map::LineStroke]),
    opacity: f32,
    ppp: f32,
) {
    let (cx, cy) = (x + w / 2.0, y + h / 2.0);
    // A thousandth of slack: the box's size went through a rounding of its own.
    let fits = 2.0 * point_marks::reach(s, strokes) * ppp <= w.min(h) * 1.001;
    if opacity >= 0.999 && fits {
        draw_point(pm, (cx, cy), s, strokes, ppp);
        return;
    }
    composited(pm, (x, y, w, h), opacity, |c, (left, top)| draw_point(c, (cx - left, cy - top), s, strokes, ppp));
}

/// Draws by `draw` into a picture of the box (`x`, `y`, `w`, `h`), no larger than the page, and
/// composites it as one at `opacity`, cut to the box. `draw` gets the picture and where its corner
/// sits on the canvas, in pixels.
pub(super) fn composited(pm: &mut Canvas, (x, y, w, h): (f32, f32, f32, f32), opacity: f32, draw: impl FnOnce(&mut Canvas, (f32, f32))) {
    // Kept on the canvas: a picture as large as the canvas then covers all of it that shows.
    let (left, top) = (x.floor().max(0.0), y.floor().max(0.0));
    let (cw, ch) = (((x + w).ceil() - left) as u32, ((y + h).ceil() - top) as u32);
    let (cw, ch) = (cw.min(pm.width()), ch.min(pm.height()));
    if let Some(mut picture) = pm.sub(cw, ch) {
        draw(&mut picture, (left, top));
        let Some(r) = Rect::from_xywh(x, y, w, h) else { return };
        let clip = if picture.is_recording() {
            // A recording keeps what lies past its picture's edge, so it is cut here, at the box.
            Some(crate::vector::Clip { path: PathBuilder::from_rect(r), rule: FillRule::Winding, transform: Transform::identity() })
        } else {
            // Pixels are cut in the picture, by a mask its size rather than the page's.
            if let Some(mut m) = Mask::new(cw, ch) {
                m.fill_path(&PathBuilder::from_rect(r), FillRule::Winding, true, Transform::from_translate(-left, -top));
                picture.pixels().apply_mask(&m);
            }
            None
        };
        pm.place(picture, Transform::from_translate(left, top), opacity, rustgis_map::effects::Blend::Normal, clip);
    }
}
