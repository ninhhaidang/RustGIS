//! Diagrams drawn on each feature ([`Layer::diagram`]): a pie or bar chart of several fields'
//! values at the feature's anchor, over the renderer's own symbol, as ArcGIS Pro's Chart
//! symbology and QGIS's Diagrams draw them. Sizes are in points, scaled by `Ctx::ppp` as a
//! marker's are; arcs are cubic Béziers, at most a quarter turn each, as tiny-skia has no arc.

use rustgis_data::Geometry;
use rustgis_geom::Coord;
#[cfg(doc)]
use rustgis_map::Layer;
use rustgis_map::{DiagramLabel, DiagramSettings, Rgba};
use tiny_skia::{FillRule, LineJoin, PathBuilder, Stroke, Transform};

use crate::Canvas;

use crate::paint;

/// Draws the diagram `d` for one feature at device pixel (`x`, `y`), whose category values
/// `values` sum to `total` ([`DiagramSettings::values`]): a pie's slices clockwise from its
/// start angle, or bars rising from a baseline, each category's share of the total. With
/// `opacity`, the layer's transparency; `ppp`, pixels per point; `paused`, that the label
/// display is paused, which draws no value labels.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw(
    pm: &mut Canvas,
    x: f32,
    y: f32,
    d: &DiagramSettings,
    values: &[(f64, Rgba)],
    total: f64,
    opacity: f32,
    ppp: f32,
    paused: bool,
) {
    if !(x.is_finite() && y.is_finite()) || values.is_empty() || total <= 0.0 {
        return;
    }
    let size = d.size_for(total) * ppp;
    if size < 0.5 {
        return;
    }
    match d.kind {
        rustgis_map::DiagramKind::Pie => pie(pm, x, y, d, values, total, size, opacity, ppp, paused),
        rustgis_map::DiagramKind::Bar => bars(pm, x, y, d, values, total, size, opacity, ppp, paused),
    }
}

/// How far from its anchor any feature's diagram reaches, in points (for culling): the largest
/// size its settings draw (a bar chart's half width, its background's margin included, for many
/// bars), half its outline, plus what a value label takes when labels are on. A label is centred 7
/// points past the crust; one beside a pie runs outward from there, about 54 points for a long one
/// (`1,234K (12.3%)` at 7 points), so 60 reaches its last letter.
pub(crate) fn reach(d: &DiagramSettings) -> f32 {
    let size = d.max_size();
    let chart = match d.kind {
        rustgis_map::DiagramKind::Pie => size,
        rustgis_map::DiagramKind::Bar => {
            // `bars`: n bars of size/n with n - 1 gaps of size/16, and a gap of margin each side.
            let n = d.categories.len().max(1) as f32;
            (size / 2.0 * (1.0 + (n - 1.0) / 16.0) + size / 16.0).max(size)
        }
    };
    chart + d.stroke_width / 2.0 + if d.labels.is_none() { 0.0 } else { 60.0 }
}

/// Where a feature's diagram sits: the point itself, a line's middle, or a polygon's interior
/// point ([`interior`]).
pub(crate) fn anchor(g: &Geometry) -> Option<Coord> {
    match g {
        Geometry::Point(c) => Some(*c),
        Geometry::MultiPoint(v) => v.iter().copied().find(|c| c.is_finite()),
        Geometry::Line(parts) => {
            let part = parts.iter().max_by_key(|p| p.len())?;
            let n = part.len();
            if n == 0 {
                return None;
            }
            // An odd count takes the middle vertex; an even one, the middle of the middle edge.
            Some(if n % 2 == 1 {
                part[n / 2]
            } else {
                let (a, b) = (part[n / 2 - 1], part[n / 2]);
                Coord::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0)
            })
        }
        Geometry::Polygon(polys) => {
            // The polygon with the largest exterior ring, and the ring that makes it.
            let rings = polys
                .iter()
                .filter_map(|rings| rings.first().map(|ring| (rings, ring)))
                .max_by(|a, b| area(a.1).abs().total_cmp(&area(b.1).abs()))
                .map(|(rings, _)| rings)?;
            interior(rings)
        }
    }
}

/// A point inside a polygon, given its rings (the exterior first, then any holes): the centroid
/// of the exterior when that lies inside, else the middle of the widest run of the polygon across
/// the centroid's height. The runs are found by an even-odd scan, so a hole splits them.
fn interior(rings: &[Vec<Coord>]) -> Option<Coord> {
    let c = centroid(rings.first()?)?;
    let mut xs: Vec<f64> = Vec::new();
    for ring in rings {
        let n = ring.len();
        for i in 0..n {
            let (p, q) = (ring[i], ring[(i + 1) % n]);
            if (p.y <= c.y) != (q.y <= c.y) {
                xs.push(p.x + (c.y - p.y) * (q.x - p.x) / (q.y - p.y));
            }
        }
    }
    xs.sort_by(f64::total_cmp);
    let runs: Vec<(f64, f64)> = xs.as_chunks::<2>().0.iter().map(|&[a, b]| (a, b)).collect();
    if runs.iter().any(|(a, b)| *a <= c.x && c.x <= *b) {
        return Some(c);
    }
    runs.into_iter().max_by(|a, b| (a.1 - a.0).total_cmp(&(b.1 - b.0))).map(|(a, b)| Coord::new((a + b) / 2.0, c.y)).or(Some(c))
}

/// A pie at (`x`, `y`) of radius `size` pixels: its background disc, then a slice per category,
/// each clockwise from `start_angle` (degrees from north) by its share of `total`, each outlined.
#[allow(clippy::too_many_arguments)]
fn pie(
    pm: &mut Canvas,
    x: f32,
    y: f32,
    d: &DiagramSettings,
    values: &[(f64, Rgba)],
    total: f64,
    size: f32,
    opacity: f32,
    ppp: f32,
    paused: bool,
) {
    if let Some(bg) = d.background.filter(|c| c.a() > 0)
        && let Some(p) = disc(x, y, size)
    {
        pm.fill_path(&p, &paint(bg, opacity), FillRule::Winding, Transform::identity(), None);
    }
    let mut from = d.start_angle;
    for (v, colour) in values {
        let sweep = (v / total * 360.0) as f32;
        if sweep <= 0.0 {
            continue;
        }
        let to = from + sweep;
        // A slice taking the whole turn is a disc: a full-turn wedge would stroke a spoke.
        let slice = if sweep >= 360.0 { disc(x, y, size) } else { wedge(x, y, size, from, to) };
        if let Some(p) = slice {
            if colour.a() > 0 {
                pm.fill_path(&p, &paint(*colour, opacity), FillRule::Winding, Transform::identity(), None);
            }
            stroke(pm, &p, d.stroke, d.stroke_width, opacity, ppp);
        }
        // The label sits on the slice's mid-angle, just outside the crust: its near edge is there,
        // so a label beside the pie (at 3 or 9 o'clock) runs away from it rather than onto it.
        if !paused && d.labels != DiagramLabel::None {
            let mid = (from + to) / 2.0 * std::f32::consts::PI / 180.0;
            let (dx, dy) = (mid.sin(), -mid.cos());
            let (lx, ly) = (x + dx * (size + 7.0 * ppp), y + dy * (size + 7.0 * ppp));
            let align = if dx > 0.5 {
                crate::text::Align::Left
            } else if dx < -0.5 {
                crate::text::Align::Right
            } else {
                crate::text::Align::Center
            };
            label_at(pm, lx, ly, d.labels, *v, total, ppp, align);
        }
        from = to;
    }
}

/// Bars at (`x`, `y`), `size` pixels high (a category's full share): a bar per category rising
/// from a baseline under the anchor, the chart as wide as a bar each plus a gap, outlined as one.
#[allow(clippy::too_many_arguments)]
fn bars(
    pm: &mut Canvas,
    x: f32,
    y: f32,
    d: &DiagramSettings,
    values: &[(f64, Rgba)],
    total: f64,
    size: f32,
    opacity: f32,
    ppp: f32,
    paused: bool,
) {
    let n = values.len();
    let (bar, gap) = (size / n as f32, size / 16.0);
    let (w, baseline) = (bar * n as f32 + gap * (n as f32 - 1.0), y + size / 2.0);
    let left = x - w / 2.0;
    if let Some(bg) = d.background.filter(|c| c.a() > 0) {
        let mut pb = PathBuilder::new();
        rect(&mut pb, left - gap, baseline - size - gap, w + 2.0 * gap, size + 2.0 * gap);
        if let Some(p) = pb.finish() {
            pm.fill_path(&p, &paint(bg, opacity), FillRule::Winding, Transform::identity(), None);
            stroke(pm, &p, d.stroke, d.stroke_width, opacity, ppp);
        }
    }
    let mut at = left;
    for (v, colour) in values {
        let h = (v / total) as f32 * size;
        if h > 0.0 {
            let mut pb = PathBuilder::new();
            rect(&mut pb, at, baseline - h, bar, h);
            if let Some(p) = pb.finish() {
                if colour.a() > 0 {
                    pm.fill_path(&p, &paint(*colour, opacity), FillRule::Winding, Transform::identity(), None);
                }
                stroke(pm, &p, d.stroke, d.stroke_width, opacity, ppp);
            }
        }
        // The label sits above the bar's top, on the bar's centre line.
        if !paused && d.labels != DiagramLabel::None {
            label_at(pm, at + bar / 2.0, baseline - h - 4.0 * ppp, d.labels, *v, total, ppp, crate::text::Align::Center);
        }
        at += bar + gap;
    }
}

/// Says a category's value, its percent of the total, or both, at (`x`, `y`) aligned as `align`
/// says, in a small haloed dark text (7 pt, as the map's finest labels are).
#[allow(clippy::too_many_arguments)]
fn label_at(pm: &mut Canvas, x: f32, y: f32, mode: DiagramLabel, v: f64, total: f64, ppp: f32, align: crate::text::Align) {
    if !(x.is_finite() && y.is_finite()) {
        return;
    }
    let dark = Rgba::rgb(0x22, 0x22, 0x22);
    let text = match mode {
        DiagramLabel::None => return,
        DiagramLabel::Value => rustgis_map::symbology::fmt_num(v),
        DiagramLabel::Percent => percent(v, total),
        DiagramLabel::Both => format!("{} ({})", rustgis_map::symbology::fmt_num(v), percent(v, total)),
    };
    crate::text::text_at(pm, &text, x, y, 7.0 * 1.33 * ppp, dark, false, align, true);
}

/// A share of `total` in percent: a whole number, or one decimal below ten percent.
fn percent(v: f64, total: f64) -> String {
    let p = v / total * 100.0;
    if p >= 10.0 { format!("{p:.0}%") } else { format!("{p:.1}%") }
}

/// Outlines `p` in `colour`, `width` points wide (at least half a pixel), when it shows.
fn stroke(pm: &mut Canvas, p: &tiny_skia::Path, colour: Rgba, width: f32, opacity: f32, ppp: f32) {
    if colour.a() == 0 || width <= 0.0 {
        return;
    }
    let st = Stroke { width: (width * ppp).max(0.5), line_join: LineJoin::Round, ..Stroke::default() };
    pm.stroke_path(p, &paint(colour, opacity), &st, Transform::identity(), None);
}

/// A full disc at (`x`, `y`) of radius `r`.
fn disc(x: f32, y: f32, r: f32) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    let start = (x + r, y);
    pb.move_to(start.0, start.1);
    arc(&mut pb, x, y, r, 0.0, std::f32::consts::TAU);
    pb.close();
    pb.finish()
}

/// A pie slice at (`x`, `y`) of radius `r`, from `from` to `to` degrees clockwise from north.
fn wedge(x: f32, y: f32, r: f32, from: f32, to: f32) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    pb.move_to(x, y);
    // Screen space runs y down, so clockwise from north runs angle `-90°` onwards.
    arc(&mut pb, x, y, r, (from - 90.0).to_radians(), (to - 90.0).to_radians());
    pb.close();
    pb.finish()
}

/// A rectangle, as a path (tiny-skia's `PathBuilder` builds one only from a `Rect`).
fn rect(pb: &mut PathBuilder, x: f32, y: f32, w: f32, h: f32) {
    pb.move_to(x, y);
    pb.line_to(x + w, y);
    pb.line_to(x + w, y + h);
    pb.line_to(x, y + h);
    pb.close();
}

/// Appends the arc of the circle at (`cx`, `cy`) of `r` from angle `a0` to `a1` in radians
/// (screen space, y down), starting with a line to the arc's start, as cubic Béziers a quarter
/// turn at most: past that the control points overshoot too far to pass as a circle.
fn arc(pb: &mut PathBuilder, cx: f32, cy: f32, r: f32, a0: f32, a1: f32) {
    let at = |a: f32| (cx + r * a.cos(), cy + r * a.sin());
    let (mut x, mut y) = at(a0);
    pb.line_to(x, y);
    let turns = (((a1 - a0).abs() / std::f32::consts::FRAC_PI_2).ceil() as usize).max(1);
    let step = (a1 - a0) / turns as f32;
    let mut a = a0;
    for _ in 0..turns {
        // The control points sit k along the tangents at each end of the segment.
        let k = 4.0 / 3.0 * (step / 4.0).tan();
        let (c0x, c0y) = (x - k * r * a.sin(), y + k * r * a.cos());
        let b = a + step;
        let (x1, y1) = at(b);
        let (c1x, c1y) = (x1 + k * r * b.sin(), y1 - k * r * b.cos());
        pb.cubic_to(c0x, c0y, c1x, c1y, x1, y1);
        x = x1;
        y = y1;
        a = b;
    }
}

/// A ring's signed area (the shoelace formula).
fn area(ring: &[Coord]) -> f64 {
    let n = ring.len();
    if n < 3 {
        return 0.0;
    }
    let mut a = 0.0;
    for i in 0..n {
        let (p, q) = (ring[i], ring[(i + 1) % n]);
        a += p.x * q.y - q.x * p.y;
    }
    a / 2.0
}

/// A ring's centroid (its area-weighted centre; the vertices' average when it has none).
fn centroid(ring: &[Coord]) -> Option<Coord> {
    let n = ring.len();
    if n == 0 {
        return None;
    }
    if n < 3 {
        return ring.first().copied();
    }
    let a = area(ring);
    if a.abs() < 1e-12 {
        let (mut x, mut y) = (0.0, 0.0);
        for c in ring {
            x += c.x;
            y += c.y;
        }
        return Some(Coord::new(x / n as f64, y / n as f64));
    }
    let (mut cx, mut cy) = (0.0, 0.0);
    for i in 0..n {
        let (p, q) = (ring[i], ring[(i + 1) % n]);
        let cross = p.x * q.y - q.x * p.y;
        cx += (p.x + q.x) * cross;
        cy += (p.y + q.y) * cross;
    }
    Some(Coord::new(cx / (6.0 * a), cy / (6.0 * a)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_map::{DiagramCategory, DiagramKind, DiagramSize, diagram::CATEGORY_COLORS};

    fn settings(kind: DiagramKind) -> DiagramSettings {
        DiagramSettings {
            kind,
            categories: vec![
                DiagramCategory { field: "male".into(), colour: CATEGORY_COLORS[0] },
                DiagramCategory { field: "female".into(), colour: CATEGORY_COLORS[1] },
            ],
            size: DiagramSize::Fixed(20.0),
            start_angle: 0.0,
            background: None,
            stroke: DiagramSettings::default_stroke(),
            stroke_width: DiagramSettings::default_stroke_width(),
            labels: DiagramLabel::None,
        }
    }

    fn painted(pm: &Canvas, x: u32, y: u32) -> bool {
        pm.pixmap().pixel(x, y).is_some_and(|p| p.alpha() > 0)
    }

    #[test]
    fn a_pie_paints_its_centre_and_leaves_the_corners() {
        let mut pm = Canvas::new(64, 64).unwrap();
        let d = settings(DiagramKind::Pie);
        draw(&mut pm, 32.0, 32.0, &d, &[(52_000.0, CATEGORY_COLORS[0]), (48_000.0, CATEGORY_COLORS[1])], 100_000.0, 1.0, 1.0, false);
        assert!(painted(&pm, 32, 32));
        assert!(!painted(&pm, 0, 0));
    }

    #[test]
    fn a_bar_rises_from_the_anchor_baseline() {
        let mut pm = Canvas::new(64, 64).unwrap();
        let d = settings(DiagramKind::Bar);
        // The baseline sits half a size below the anchor; the first bar covers x 21..31 of it.
        draw(&mut pm, 32.0, 32.0, &d, &[(52_000.0, CATEGORY_COLORS[0]), (48_000.0, CATEGORY_COLORS[1])], 100_000.0, 1.0, 1.0, false);
        assert!(painted(&pm, 28, 40));
        assert!(!painted(&pm, 28, 20));
        assert!(!painted(&pm, 0, 0));
    }

    #[test]
    fn nothing_is_drawn_without_a_positive_total() {
        let mut pm = Canvas::new(64, 64).unwrap();
        let d = settings(DiagramKind::Pie);
        draw(&mut pm, 32.0, 32.0, &d, &[(0.0, CATEGORY_COLORS[0])], 0.0, 1.0, 1.0, false);
        assert!(pm.pixmap().pixels().iter().all(|p| p.alpha() == 0));
    }

    #[test]
    fn a_percent_label_draws_just_outside_the_pie() {
        let mut pm = Canvas::new(96, 96).unwrap();
        let d = DiagramSettings { labels: DiagramLabel::Percent, ..settings(DiagramKind::Pie) };
        draw(&mut pm, 48.0, 48.0, &d, &[(52_000.0, CATEGORY_COLORS[0]), (48_000.0, CATEGORY_COLORS[1])], 100_000.0, 1.0, 1.0, false);
        // The pie reaches 20 px; each label sits just past its crust, so pixels outside that
        // radius are painted where without labels nothing is.
        let outside = pm.pixmap().pixels().iter().enumerate().filter(|(_, p)| p.alpha() > 0).any(|(i, _)| {
            let (dx, dy) = ((i % 96) as f32 - 47.5, (i / 96) as f32 - 47.5);
            (dx * dx + dy * dy) > 21.0 * 21.0
        });
        assert!(outside, "a label should draw just past the crust");
    }

    #[test]
    fn a_label_beside_the_pie_clears_its_side() {
        // The slices' mid-angles sit at 3 and 9 o'clock: their labels start past the right side and
        // end before the left, so the band 1 to 4 points past the crust, level with the centre,
        // is clear of text.
        let mut pm = Canvas::new(96, 96).unwrap();
        let d = DiagramSettings { labels: DiagramLabel::Percent, ..settings(DiagramKind::Pie) };
        draw(&mut pm, 48.0, 48.0, &d, &[(52_000.0, CATEGORY_COLORS[0]), (48_000.0, CATEGORY_COLORS[1])], 100_000.0, 1.0, 1.0, false);
        let on_the_band = pm.pixmap().pixels().iter().enumerate().filter(|(_, p)| p.alpha() > 0).any(|(i, _)| {
            let (dx, dy) = ((i % 96) as f32 + 0.5 - 48.0, (i / 96) as f32 + 0.5 - 48.0);
            dy.abs() < 4.0 && (21.0..24.0).contains(&dx.abs())
        });
        assert!(!on_the_band, "a label beside the pie runs away from its crust, not onto it");
    }

    #[test]
    fn a_paused_label_display_draws_no_value_labels() {
        let values = [(52_000.0, CATEGORY_COLORS[0]), (48_000.0, CATEGORY_COLORS[1])];
        let labelled = DiagramSettings { labels: DiagramLabel::Percent, ..settings(DiagramKind::Pie) };
        let mut paused = Canvas::new(96, 96).unwrap();
        draw(&mut paused, 48.0, 48.0, &labelled, &values, 100_000.0, 1.0, 1.0, true);
        let mut bare = Canvas::new(96, 96).unwrap();
        draw(&mut bare, 48.0, 48.0, &settings(DiagramKind::Pie), &values, 100_000.0, 1.0, 1.0, false);
        assert!(paused.pixmap().data() == bare.pixmap().data(), "the chart draws as it would with no labels");
    }

    #[test]
    fn a_pie_of_one_slice_is_a_disc_with_no_spoke() {
        // A clear slice shows only its outline: a full-turn wedge would stroke a spoke from the
        // centre north to the crust, through (32, 22).
        let mut pm = Canvas::new(64, 64).unwrap();
        let d = settings(DiagramKind::Pie);
        draw(&mut pm, 32.0, 32.0, &d, &[(10.0, rustgis_map::Rgba::new(0, 0, 0, 0))], 10.0, 1.0, 1.0, false);
        assert!(painted(&pm, 32, 12) || painted(&pm, 32, 13), "the crust is outlined");
        assert!(!painted(&pm, 32, 22), "no spoke runs from the centre");
    }

    #[test]
    fn reach_covers_a_wide_bar_chart_with_its_background() {
        let categories = (0..16).map(|i| DiagramCategory { field: format!("f{i}"), colour: CATEGORY_COLORS[0] }).collect();
        let d = DiagramSettings { categories, background: Some(rustgis_map::Rgba::new(255, 255, 255, 255)), ..settings(DiagramKind::Bar) };
        // `bars` at size 20: 16 bars of 1.25 and 15 gaps of 1.25, so 38.75 wide, and a 1.25 margin
        // each side: the box's edge is 20.625 from the anchor, past the size.
        assert!(reach(&d) >= 20.625 + d.stroke_width / 2.0, "reach {} misses the box's edge", reach(&d));
        assert!(reach(&settings(DiagramKind::Pie)) >= 20.0 + DiagramSettings::default_stroke_width() / 2.0);
    }

    #[test]
    fn a_multipoint_anchors_at_its_first_finite_point() {
        let m = Geometry::MultiPoint(vec![Coord::new(f64::NAN, 0.0), Coord::new(3.0, 4.0)]);
        assert!(anchor(&m).is_some_and(|c| c.x == 3.0 && c.y == 4.0), "a point that projected to nothing is passed over");
        assert!(anchor(&Geometry::MultiPoint(vec![Coord::new(f64::NAN, f64::NAN)])).is_none());
    }

    #[test]
    fn a_polygon_anchors_at_its_centroid() {
        let square = Geometry::Polygon(vec![vec![vec![
            Coord::new(0.0, 0.0),
            Coord::new(1.0, 0.0),
            Coord::new(1.0, 1.0),
            Coord::new(0.0, 1.0),
            Coord::new(0.0, 0.0),
        ]]]);
        assert!(anchor(&square).is_some_and(|c| (c.x - 0.5).abs() < 1e-9 && (c.y - 0.5).abs() < 1e-9));
        assert!(anchor(&Geometry::Point(Coord::new(3.0, 4.0))).is_some_and(|c| (c.x - 3.0).abs() < 1e-9));
        assert!(
            anchor(&Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(2.0, 0.0), Coord::new(4.0, 0.0)]]))
                .is_some_and(|c| (c.x - 2.0).abs() < 1e-9)
        );
    }

    #[test]
    fn a_concave_polygon_anchors_inside_itself() {
        // A U: its centroid (1.5, 1.36) lies in the gap between the arms, outside the shape.
        let u = Geometry::Polygon(vec![vec![vec![
            Coord::new(0.0, 0.0),
            Coord::new(3.0, 0.0),
            Coord::new(3.0, 3.0),
            Coord::new(2.0, 3.0),
            Coord::new(2.0, 1.0),
            Coord::new(1.0, 1.0),
            Coord::new(1.0, 3.0),
            Coord::new(0.0, 3.0),
            Coord::new(0.0, 0.0),
        ]]]);
        assert!(anchor(&u).is_some_and(|c| c.y > 1.0 && (c.x < 1.0 || c.x > 2.0)), "the point sits in one of the arms, not the gap");
    }
}
