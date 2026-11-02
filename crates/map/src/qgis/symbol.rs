//! QGIS symbols (`<symbol type="fill|line|marker">`) as a RustGIS [`Symbol`], and a line's
//! symbol layers as stacked [`LineStroke`]s. A symbol layer RustGIS has no match for is drawn
//! as near as it can (a pattern fill as a tint of its colour, an SVG marker as a circle in its
//! colour) or left out, and noted.

use roxmltree::Node;

use super::{Report, color, data_defined_on, has_effect, points, prop, props, signed_points};
use crate::Rgba;
use crate::fills::{FillLayer, FillPattern, GradientPoint, GradientShape, Spread, Stop, Stops};
use crate::symbol_by::SymbolBy;
use crate::symbology::{
    Dash, Glyph, LineCap, LineJoin, LineMark, LineMarkers, LineStroke, MAX_DASH_STEPS, MAX_STROKES, Marker, MarkerRotation, Placements,
    Symbol,
};

/// A symbol layer's settings by name.
type Props = Vec<(String, String)>;

/// The setting that stands for QGIS 1's symbol-wide unit (`<symbol outputUnit>`), for sizes a
/// layer gives no unit of its own. No symbol layer setting is named so.
const SYMBOL_UNIT: &str = "(symbol unit)";

/// A QGIS symbol in RustGIS's terms: the symbol, and a line's strokes bottom first (each simple
/// line of it; empty for fills and markers, and for a line with none).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Converted {
    pub symbol: Symbol,
    pub strokes: Vec<LineStroke>,
    /// A marker symbol turned by a field (its data-defined angle).
    pub rotation: Option<MarkerRotation>,
    /// Its data-defined size, colours, outline width and angle, as expressions.
    pub by: SymbolBy,
    /// A fill symbol's stacked fills, bottom first: empty when its one fill is a colour (the
    /// symbol's own), as before patterns were read.
    pub fills: Vec<FillLayer>,
}

impl Converted {
    /// Whether the strokes say more than the symbol's single line does: more than one, or one
    /// shifted sideways, dashed or of markers.
    pub fn stacked(&self) -> bool {
        self.strokes.len() > 1 || self.strokes.iter().any(|s| s.offset != 0.0 || s.dash.is_some() || s.markers.is_some())
    }
}

/// Nothing drawn: QGIS's hidden categories and empty symbols.
pub(super) fn hidden() -> Symbol {
    let none = Rgba::new(0, 0, 0, 0);
    Symbol { fill: none, stroke: none, stroke_width: 0.0, size: 0.0, marker: Marker::Circle }
}

/// Whether the marker symbol `sym` turns by a field (its data-defined angle, or a layer's).
pub(super) fn turns_by_field(sym: Node) -> bool {
    sym.attribute("type") == Some("marker")
        && std::iter::once(sym)
            .chain(sym.children().filter(|c| c.has_tag_name("layer")).take(64))
            .any(|n| active_properties(n).into_iter().any(|(name, p)| name == "angle" && field_angle(p).is_some()))
}

/// The symbol `sym` (a `<symbol>` element), its sizes at the reference scale `reference` where
/// they are in map units. `turned`: the layer's markers turn by a field, so a marker symbol that
/// doesn't is drawn as a mark that keeps still.
pub(super) fn read(sym: Node, reference: Option<f64>, turned: bool, layer: &str, report: &mut Report) -> Converted {
    let alpha = sym.attribute("alpha").and_then(|a| a.trim().parse::<f32>().ok()).filter(|a| a.is_finite()).unwrap_or(1.0).clamp(0.0, 1.0);
    let fade = |c: Rgba| c.with_alpha((f32::from(c.a()) * alpha).round() as u8);
    let unit = sym.attribute("outputUnit");
    let layers: Vec<(Node, Props)> = sym
        .children()
        .filter(|c| c.has_tag_name("layer") && c.attribute("enabled") != Some("0"))
        .take(64)
        .map(|l| (l, settings(l, unit)))
        .collect();
    // A marker symbol's angle from a field turns the layer's markers; its size, colours, outline
    // width and other angles become expressions; other data-defined settings are noted.
    let is_marker = !matches!(sym.attribute("type"), Some("fill") | Some("line"));
    let mut rotation = None;
    let mut by = SymbolBy::default();
    // The size (in points) of the layer whose size expression was taken.
    let mut sized_layer: Option<f32> = None;
    // How many of its layers are sized by expression.
    let mut sized_layers = 0;
    // A marker of several layers draws as marks scaled with its size: the top layer's is read first.
    let marks = is_marker && layers.len() > 1;
    let order: Vec<&(Node, Props)> = if marks { layers.iter().rev().collect() } else { layers.iter().collect() };
    for (node, p) in std::iter::once((sym, None)).chain(order.into_iter().map(|(l, p)| (*l, Some(p.as_slice())))) {
        let active = active_properties(node);
        // A layout `active_properties` doesn't read (QGIS 2's `dd_properties`) is still noted.
        if active.is_empty() && data_defined_on(node) {
            report.skipped(layer, "data-defined symbol settings (from fields or expressions)");
        }
        for (name, property) in active {
            match field_angle(property).filter(|_| is_marker && name == "angle") {
                Some(r) if rotation.as_ref().is_none_or(|had| *had == r) => rotation = Some(r),
                Some(_) => report.approximated(layer, "marker layers turned by different fields, all turned by the first"),
                // A lower layer's size: the marks scale with the top one's.
                None if marks && name == "size" && by.size.is_some() => sized_layers += usize::from(p.is_some()),
                None if by_expression(sym.attribute("type"), name, property, p, reference, &mut by, layer, report) => {
                    // (A size on the symbol itself scales the whole symbol: nothing to turn.)
                    if name == "size" && p.is_some() {
                        sized_layers += 1;
                        sized_layer = p.and_then(|p| size(p, "size", reference, layer, report)).or(Some(2.0 * 96.0 / 25.4));
                    }
                    if !(marks && name == "size") && layers.len() > 1 {
                        report.approximated(layer, "data-defined settings of one symbol layer, applied to the whole symbol");
                    }
                }
                None => report.skipped(layer, "data-defined symbol settings (from fields or expressions)"),
            }
        }
    }
    // Each layer sized by expression scales as QGIS scales them together; a layer of a fixed size
    // scales with the rest.
    if marks && by.size.is_some() && sized_layers < layers.len() {
        report.approximated(layer, "the layers of a marker sized by expression, all scaled with its size");
    }
    for (l, _) in &layers {
        if has_effect(*l) {
            report.skipped(layer, "draw effects (shadows, glows, blurs and the like)");
        }
    }
    let mut out = match sym.attribute("type") {
        Some("fill") => fill(&layers, reference, layer, report),
        Some("line") => line(&layers, reference, layer, report),
        _ => marker(&layers, reference, turned && rotation.is_none(), layer, report),
    };
    out.rotation = rotation;
    let drawn_as_marks = is_marker && !out.strokes.is_empty();
    // Marks keep their own colours and outlines.
    if drawn_as_marks && (by.fill.is_some() || by.stroke.is_some() || by.stroke_width.is_some()) {
        report.skipped(layer, "data-defined colours and outline widths of a marker drawn as marks");
        (by.fill, by.stroke, by.stroke_width) = (None, None, None);
    }
    // A line of stacked strokes draws each with its own colour and width.
    if sym.attribute("type") == Some("line") && out.stacked() && (by.stroke.is_some() || by.stroke_width.is_some()) {
        report.skipped(layer, "data-defined colours and widths of a line of several strokes");
        (by.stroke, by.stroke_width) = (None, None);
    }
    // Marks scale with the symbol's size, its largest layer's: the expression, taken from one
    // layer, is turned into the size it gives that one, so every mark scales as that layer does.
    if let (true, Some(text), Some(own)) = (drawn_as_marks, by.size.as_ref(), sized_layer.map(|s| s.clamp(0.0, 200.0)))
        && own > 0.0
        && out.symbol.size > 0.0
        && (out.symbol.size / own - 1.0).abs() > 1e-6
    {
        let k = (f64::from(out.symbol.size / own)).clamp(1e-3, 1e3);
        let scaled = format!("({text}) * {}", (k * 1e6).round() / 1e6);
        if scaled.len() <= rustgis_data::query::MAX_LEN {
            by.size = Some(scaled);
        } else {
            report.skipped(layer, "a data-defined marker size too long to read");
            by.size = None;
        }
    }
    // (An expression's colour is drawn as it is: the symbol's opacity doesn't fade it.)
    if alpha < 1.0 && (by.fill.is_some() || by.stroke.is_some()) {
        report.approximated(layer, "data-defined colours of a see-through symbol, drawn opaque");
    }
    out.by = by;
    out.symbol.fill = fade(out.symbol.fill);
    out.symbol.stroke = fade(out.symbol.stroke);
    for s in &mut out.strokes {
        s.color = s.color.map(fade);
        if let Some(m) = &mut s.markers {
            m.outline = m.outline.map(fade);
        }
    }
    for f in &mut out.fills {
        f.color = f.color.map(fade);
        match &mut f.pattern {
            FillPattern::Dots { outline, .. } => *outline = outline.map(fade),
            FillPattern::Gradient { color2, stops, .. } | FillPattern::Shapeburst { color2, stops, .. } => {
                *color2 = fade(*color2);
                *stops = Stops::new(stops.as_slice().iter().map(|s| Stop { at: s.at, color: fade(s.color) }));
            }
            _ => {}
        }
    }
    out
}

/// A symbol layer's settings, with those QGIS 2 named otherwise also under today's names (where
/// the layer has only the old), and the symbol's own unit (QGIS 1) for sizes without one.
fn settings(l: Node, unit: Option<&str>) -> Props {
    let mut p = props(l);
    let renamed: &[(&str, &str)] = match l.attribute("class") {
        Some("SimpleLine") => {
            &[("color", "line_color"), ("width", "line_width"), ("width_unit", "line_width_unit"), ("penstyle", "line_style")]
        }
        Some("SimpleFill") => &[
            ("color_border", "outline_color"),
            ("width_border", "outline_width"),
            ("border_width_unit", "outline_width_unit"),
            ("style_border", "outline_style"),
        ],
        Some("SimpleMarker") => &[("color_border", "outline_color")],
        _ => &[],
    };
    for (old, new) in renamed {
        if prop(&p, new).is_none()
            && let Some(v) = prop(&p, old).map(str::to_string)
        {
            p.push((new.to_string(), v));
        }
    }
    if let Some(u) = unit {
        p.push((SYMBOL_UNIT.to_string(), u.to_string()));
    }
    p
}

/// A size setting `k` (with its `k_unit`) of a symbol layer, in points.
fn size(p: &[(String, String)], k: &str, reference: Option<f64>, layer: &str, report: &mut Report) -> Option<f32> {
    let v = prop(p, k)?.trim().parse::<f64>().ok().filter(|v| v.is_finite())?;
    let unit = prop(p, &format!("{k}_unit")).or(prop(p, SYMBOL_UNIT));
    Some(points(v, unit, reference, layer, report))
}

/// An offset setting `k` (with its `k_unit`) of a symbol layer, in points: negative to the left.
fn offset(p: &[(String, String)], k: &str, reference: Option<f64>, layer: &str, report: &mut Report) -> Option<f32> {
    let v = prop(p, k)?.trim().parse::<f64>().ok().filter(|v| v.is_finite())?;
    let unit = prop(p, &format!("{k}_unit")).or(prop(p, SYMBOL_UNIT));
    Some(signed_points(v, unit, reference, layer, report))
}

/// Whether a line style (`line_style`, `outline_style`) is dashed or dotted.
fn dashed(style: Option<&str>) -> bool {
    !matches!(style, None | Some("solid" | "no"))
}

/// A simple line's dash pattern in points: its own (`use_custom_dash`, in its unit), or Qt's for
/// a named pen style (in line widths, as Qt sizes them). `None` for a solid line, and for a
/// pattern RustGIS can't draw (noted).
fn dash(p: &[(String, String)], width: f32, reference: Option<f64>, layer: &str, report: &mut Report) -> Option<Dash> {
    let style = prop(p, "line_style").unwrap_or("solid");
    let steps: Vec<f32> = if prop(p, "use_custom_dash") == Some("1") {
        let unit = prop(p, "customdash_unit").or(prop(p, SYMBOL_UNIT));
        let parsed: Option<Vec<f64>> = prop(p, "customdash")
            .unwrap_or_default()
            .split(';')
            .filter(|s| !s.trim().is_empty())
            .take(MAX_DASH_STEPS + 2)
            .map(|s| s.trim().parse::<f64>().ok().filter(|v| v.is_finite()))
            .collect();
        let mut v = parsed.unwrap_or_default();
        // An odd pattern ending in a step of no length: that step adds nothing.
        if v.len() % 2 == 1 && v.last() == Some(&0.0) {
            v.pop();
        }
        v.into_iter().map(|s| points(s, unit, reference, layer, report)).collect()
    } else {
        let qt: &[f32] = match style {
            "solid" | "no" => return None,
            "dash" => &[4.0, 2.0],
            "dot" => &[1.0, 2.0],
            "dash dot" => &[4.0, 2.0, 1.0, 2.0],
            "dash dot dot" => &[4.0, 2.0, 1.0, 2.0, 1.0, 2.0],
            _ => &[],
        };
        // Qt takes a line thinner than a pixel as a pixel wide for its pattern.
        qt.iter().map(|s| s * width.max(1.0)).collect()
    };
    let d = Dash::new(&steps);
    if d.is_none() {
        report.approximated(layer, "dashed lines whose pattern RustGIS can't draw, drawn solid");
    }
    if d.is_some() && prop(p, "dash_pattern_offset").and_then(|o| o.trim().parse::<f64>().ok()).is_some_and(|o| o != 0.0) {
        report.approximated(layer, "dash patterns' offsets along the line, left out");
    }
    d
}

/// A Qt brush's hatch lines (QGIS's simple fill styles), as (angle anticlockwise from east, points
/// across between lines): 1 px lines 8 px apart, the diagonals 8 px apart along a row (measured on
/// QGIS 4.2.3 at 96 dpi: `b_diagonal` rises to the right, `f_diagonal` falls).
fn brush_hatches(style: &str) -> Option<&'static [(f32, f32)]> {
    const DIAG: f32 = 8.0 * std::f32::consts::FRAC_1_SQRT_2;
    Some(match style {
        "horizontal" => &[(0.0, 8.0)],
        "vertical" => &[(90.0, 8.0)],
        "b_diagonal" => &[(45.0, DIAG)],
        "f_diagonal" => &[(135.0, DIAG)],
        "cross" => &[(0.0, 8.0), (90.0, 8.0)],
        "diagonal_x" => &[(45.0, DIAG), (135.0, DIAG)],
        _ => return None,
    })
}

fn fill(layers: &[(Node, Props)], reference: Option<f64>, layer: &str, report: &mut Report) -> Converted {
    let none = Rgba::new(0, 0, 0, 0);
    let (mut fill, mut stroke, mut width) = (None::<Rgba>, None::<Rgba>, 0.0f32);
    // Its fills, bottom first, as QGIS lists its layers; the symbol's fill stays the colour a
    // legend shows (a pattern's as a tint of it).
    let mut stack: Vec<FillLayer> = Vec::new();
    let num = |p: &Props, k: &str| prop(p, k).and_then(|v| v.trim().parse::<f32>().ok()).filter(|v| v.is_finite());
    for (l, p) in layers {
        match l.attribute("class").unwrap_or_default() {
            "SimpleFill" => {
                let c = prop(p, "color").and_then(color).unwrap_or(none);
                let style = prop(p, "style").unwrap_or("solid");
                fill = Some(match style {
                    "solid" => {
                        stack.push(FillLayer { color: Some(c), pattern: FillPattern::Solid });
                        c
                    }
                    "no" => fill.unwrap_or(none),
                    hatched => {
                        match brush_hatches(hatched) {
                            Some(lines) => {
                                for &(angle, spacing) in lines {
                                    stack.push(FillLayer { color: Some(c), ..FillLayer::hatch(angle, spacing, 1.0) });
                                }
                            }
                            // Qt's dense dot brushes.
                            None => {
                                report.approximated(layer, "fills of dense dots, drawn as a light tint of their colour");
                                stack.push(FillLayer { color: Some(tint(c)), pattern: FillPattern::Solid });
                            }
                        }
                        tint(c)
                    }
                });
                if prop(p, "outline_style") != Some("no") {
                    if dashed(prop(p, "outline_style")) {
                        report.approximated(layer, "dashed polygon outlines, drawn solid");
                    }
                    stroke = prop(p, "outline_color").and_then(color).or(stroke);
                    width = size(p, "outline_width", reference, layer, report).unwrap_or(0.26 * 96.0 / 25.4);
                }
            }
            "SimpleLine" => {
                if prop(p, "line_style") != Some("no") {
                    if dashed(prop(p, "line_style")) || prop(p, "use_custom_dash") == Some("1") {
                        report.approximated(layer, "dashed polygon outlines, drawn solid");
                    }
                    stroke = prop(p, "line_color").and_then(color).or(stroke);
                    width = size(p, "line_width", reference, layer, report).unwrap_or(width);
                }
            }
            class @ ("GradientFill" | "ShapeburstFill") => {
                let (first, pattern) = gradient(class == "ShapeburstFill", *l, p, reference, layer, report);
                stack.push(FillLayer { color: Some(first), pattern });
                fill = Some(first);
            }
            // Hatch lines: its line's colour and width, its angle and spacing.
            "LinePatternFill" => {
                let sub = l.children().find(|c| c.has_tag_name("symbol")).map(|s| read(s, reference, false, layer, report));
                // QGIS 3 saves it as `angle`, QGIS 2 as `lineangle`.
                let angle = num(p, "angle").or(num(p, "lineangle")).unwrap_or(45.0);
                // QGIS's default: 5 mm apart.
                let spacing = size(p, "distance", reference, layer, report).unwrap_or(5.0 * 96.0 / 25.4);
                let shift = offset(p, "offset", reference, layer, report).unwrap_or(0.0);
                // A hatch for each of the line's strokes, bottom first (a cased line hatches with
                // its casing under its core); a line of one stroke is its symbol's line.
                let base = sub.as_ref().map(|s| s.symbol);
                let lines: Vec<(Option<Rgba>, Option<f32>)> = match sub.as_ref().map(|s| s.strokes.as_slice()) {
                    Some(strokes) if !strokes.is_empty() => {
                        if strokes.iter().any(|st| st.dash.is_some() || st.markers.is_some()) {
                            report.approximated(layer, "dashed and marker lines in line pattern fills, drawn solid");
                        }
                        strokes.iter().map(|st| (st.color, st.width)).collect()
                    }
                    _ => vec![(None, None)],
                };
                for (c, w) in lines {
                    let c = c.or_else(|| base.map(|s| s.stroke)).or_else(|| prop(p, "color").and_then(color)).unwrap_or(Rgba::rgb(0, 0, 0));
                    let w = w.or_else(|| base.map(|s| s.stroke_width)).unwrap_or(0.26 * 96.0 / 25.4);
                    let pattern = FillPattern::Hatch { angle, spacing, width: w, offset: shift };
                    stack.push(FillLayer { color: Some(c), pattern }.clamped());
                    fill = Some(tint(c));
                }
            }
            // Markers on a grid: its marker, its spacing and shifted rows and columns.
            "PointPatternFill" => {
                let sub = l.children().find(|c| c.has_tag_name("symbol")).map(|s| read(s, reference, false, layer, report));
                let m = sub.as_ref().map_or_else(
                    || Symbol {
                        fill: Rgba::rgb(0, 0, 0),
                        stroke: Rgba::new(0, 0, 0, 0),
                        stroke_width: 0.0,
                        size: 2.0 * 96.0 / 25.4,
                        marker: Marker::Circle,
                    },
                    |s| s.symbol,
                );
                // QGIS's default: 15 mm apart each way.
                let apart = |k: &str, report: &mut Report| size(p, k, reference, layer, report).unwrap_or(15.0 * 96.0 / 25.4);
                let shift = |k: &str, report: &mut Report| offset(p, k, reference, layer, report).unwrap_or(0.0);
                let spacing = [apart("distance_x", report), apart("distance_y", report)];
                let displacement = [shift("displacement_x", report), shift("displacement_y", report)];
                let moved = [shift("offset_x", report), shift("offset_y", report)];
                let outline = (m.stroke.a() > 0 && m.stroke_width > 0.0).then_some(m.stroke);
                stack.push(
                    FillLayer {
                        color: Some(m.fill),
                        pattern: FillPattern::Dots {
                            marker: m.marker,
                            size: m.size,
                            outline,
                            outline_width: m.stroke_width,
                            spacing,
                            displacement,
                            offset: moved,
                        },
                    }
                    .clamped(),
                );
                fill = Some(tint(if m.fill.a() > 0 { m.fill } else { m.stroke }));
            }
            class @ ("SVGFill" | "RasterFill" | "RandomMarkerFill") => {
                // The pattern's own symbol's colour, as a tint over what's below.
                let sub = l.children().find(|c| c.has_tag_name("symbol")).map(|s| read(s, reference, false, layer, &mut Report::default()));
                let c = sub.map(|s| if s.symbol.stroke.a() > 0 && s.symbol.fill.a() == 0 { s.symbol.stroke } else { s.symbol.fill });
                if let Some(c) = c.or_else(|| prop(p, "color").and_then(color)) {
                    fill = Some(tint(c));
                    stack.push(FillLayer { color: Some(tint(c)), pattern: FillPattern::Solid });
                }
                report.approximated(layer, format!("{} fills, drawn as a light tint of their colour", pattern_name(class)));
            }
            "CentroidFill" => report.skipped(layer, "markers at polygon centroids (centroid fills)"),
            other => report.skipped(layer, format!("{other} symbol layers")),
        }
    }
    // One colour and nothing over it is the symbol's fill alone, as before fills were stacked.
    if stack.len() <= 1 && stack.iter().all(|f| f.pattern == FillPattern::Solid) {
        stack.clear();
    }
    if stack.len() > crate::fills::MAX_FILLS {
        report.skipped(layer, format!("fills past the first {}", crate::fills::MAX_FILLS));
        stack.truncate(crate::fills::MAX_FILLS);
    }
    let symbol =
        Symbol { fill: fill.unwrap_or(none), stroke: stroke.unwrap_or(none), stroke_width: width, size: 0.0, marker: Marker::Circle };
    Converted { symbol, strokes: Vec::new(), rotation: None, by: SymbolBy::default(), fills: stack }
}

/// A gradient fill's (or a shapeburst fill's, `burst`) first colour and the rest of it: its two
/// colours, or its colour ramp (a gradient's stops; another kind of ramp by its two ends), how
/// it runs, and for a shapeburst how deep it shades.
fn gradient(burst: bool, l: Node, p: &Props, reference: Option<f64>, layer: &str, report: &mut Report) -> (Rgba, FillPattern) {
    let num = |k: &str| prop(p, k).and_then(|v| v.trim().parse::<f32>().ok()).filter(|v| v.is_finite());
    let white = Rgba::rgb(255, 255, 255);
    // QGIS's defaults: blue to white.
    let two =
        (prop(p, "color").and_then(color).unwrap_or(Rgba::rgb(0, 0, 255)), prop(p, "gradient_color2").and_then(color).unwrap_or(white));
    let (first, color2, stops, discrete) = if prop(p, "color_type") == Some("1") {
        // The ramp's settings: in a `colorramp` of the layer's own where it has one, else among
        // the layer's.
        let ramp = l.children().find(|c| c.has_tag_name("colorramp"));
        let own = ramp.map(props);
        let r = own.as_ref().map_or(p, |o| o);
        let kind = ramp.and_then(|c| c.attribute("type"));
        // Another kind of ramp is drawn by its two ends alone, its stops left out.
        let gradient = matches!(prop(r, "rampType").or(kind), None | Some("gradient"));
        if !gradient {
            report.approximated(layer, "gradient fills' colour ramps other than gradients, drawn from their first colour to their last");
        }
        let stops = if gradient { ramp_stops(prop(r, "stops").unwrap_or_default()) } else { Stops::default() };
        match (prop(r, "color1").and_then(color), prop(r, "color2").and_then(color)) {
            (Some(a), Some(b)) => (a, b, stops, gradient && prop(r, "discrete") == Some("1")),
            _ => (two.0, two.1, Stops::default(), false),
        }
    } else {
        (two.0, two.1, Stops::default(), false)
    };
    if prop(p, "offset").is_some_and(|o| o.split(',').any(|v| v.trim().parse::<f64>().is_ok_and(|v| v != 0.0))) {
        report.approximated(layer, "gradient and shapeburst fills' offsets, left out");
    }
    if burst {
        // QGIS's default: over the whole shape.
        let distance = (prop(p, "use_whole_shape") == Some("0")).then(|| {
            let v = prop(p, "max_distance").and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite()).unwrap_or(5.0);
            points(v, prop(p, "distance_unit").or(prop(p, SYMBOL_UNIT)), reference, layer, report)
        });
        let blur = num("blur_radius").map_or(0, |b| b.clamp(0.0, f32::from(crate::fills::MAX_BURST_BLUR)).round() as u8);
        let ignore_rings = prop(p, "ignore_rings") == Some("1");
        let pattern = FillPattern::Shapeburst { color2, stops, discrete, distance, blur, ignore_rings };
        return (first, FillLayer { color: None, pattern }.clamped().pattern);
    }
    let point = |k: &str, default: [f32; 2]| {
        if prop(p, &format!("{k}_iscentroid")) == Some("1") {
            return GradientPoint::CENTROID;
        }
        let xy: Option<Vec<f32>> = prop(p, k).map(|v| v.split(',').filter_map(|c| c.trim().parse::<f32>().ok()).collect());
        match xy.as_deref() {
            Some([x, y]) => GradientPoint::At([*x, *y]),
            _ => GradientPoint::At(default),
        }
    };
    let shape = match prop(p, "type") {
        Some("1") => GradientShape::Radial,
        Some("2") => GradientShape::Conical,
        _ => GradientShape::Linear,
    };
    let spread = match prop(p, "spread") {
        Some("1") => Spread::Reflect,
        Some("2") => Spread::Repeat,
        _ => Spread::Pad,
    };
    let pattern = FillPattern::Gradient {
        shape,
        color2,
        stops,
        discrete,
        from: point("reference_point1", [0.5, 0.0]),
        to: point("reference_point2", [0.5, 1.0]),
        spread,
        angle: num("angle").unwrap_or(0.0),
        in_view: prop(p, "coordinate_mode") == Some("1"),
    };
    (first, FillLayer { color: None, pattern }.clamped().pattern)
}

/// A QGIS gradient ramp's `stops`: `position;colour` pairs split by colons (a colour may carry
/// its own colon, in `…,rgb:0.2,0.4,…`, and a stop its colour spec and direction after it).
fn ramp_stops(stops: &str) -> Stops {
    // Each stop read as it ends, so only valid ones count towards what a fill keeps (and one
    // more, to tell it has more), however many entries before them aren't.
    let parse = |s: &str| {
        let mut f = s.split(';');
        let at = f.next()?.trim().parse::<f32>().ok().filter(|a| a.is_finite())?;
        Some(Stop { at, color: color(f.next()?)? })
    };
    let mut valid: Vec<Stop> = Vec::new();
    let mut current: Option<String> = None;
    for part in stops.split(':') {
        // A new stop starts with its position and a semicolon; anything else goes on the last.
        let starts = part.split_once(';').is_some_and(|(at, _)| at.trim().parse::<f32>().is_ok());
        match current.as_mut() {
            Some(last) if !starts => {
                last.push(':');
                last.push_str(part);
            }
            _ => {
                valid.extend(current.take().as_deref().and_then(parse));
                if valid.len() > crate::fills::MAX_STOPS {
                    break;
                }
                current = Some(part.to_string());
            }
        }
    }
    valid.extend(current.as_deref().and_then(parse));
    Stops::new(valid)
}

fn line(layers: &[(Node, Props)], reference: Option<f64>, layer: &str, report: &mut Report) -> Converted {
    let mut strokes = Vec::new();
    for (l, p) in layers {
        match l.attribute("class").unwrap_or_default() {
            "SimpleLine" => {
                let style = prop(p, "line_style").unwrap_or("solid");
                let Some(c) = prop(p, "line_color").and_then(color) else { continue };
                if style == "no" || c.a() == 0 {
                    continue;
                }
                let pass = l.attribute("pass").and_then(|v| v.trim().parse::<u8>().ok());
                let width = size(p, "line_width", reference, layer, report).unwrap_or(0.26 * 96.0 / 25.4);
                strokes.push(
                    LineStroke {
                        color: Some(c),
                        width: Some(width),
                        // Positive to the right of the line's direction on the screen, in both.
                        offset: offset(p, "offset", reference, layer, report).unwrap_or(0.0),
                        cap: match prop(p, "capstyle") {
                            Some("flat") => LineCap::Butt,
                            Some("square") => LineCap::Square,
                            _ => LineCap::Round,
                        },
                        join: match prop(p, "joinstyle") {
                            Some("miter") => LineJoin::Miter,
                            Some("bevel") => LineJoin::Bevel,
                            _ => LineJoin::Round,
                        },
                        level: pass,
                        dash: dash(p, width, reference, layer, report),
                        markers: None,
                    }
                    .clamped(),
                );
            }
            class @ ("MarkerLine" | "HashLine") => strokes.extend(along(*l, p, class == "HashLine", reference, layer, report)),
            "ArrowLine" => report.skipped(layer, "arrow lines"),
            other => report.skipped(layer, format!("{other} line symbol layers")),
        }
    }
    if strokes.len() > MAX_STROKES {
        report.skipped(layer, format!("line strokes past the first {MAX_STROKES}"));
        strokes.truncate(MAX_STROKES);
    }
    // The symbol's line: the top stroke.
    let top = strokes.last();
    let symbol = match top {
        Some(s) => {
            let c = s.color.unwrap_or(Rgba::new(0, 0, 0, 0));
            Symbol { fill: c, stroke: c, stroke_width: s.width.unwrap_or(1.0), size: 0.0, marker: Marker::Circle }
        }
        None => hidden(),
    };
    Converted { symbol, strokes, rotation: None, by: SymbolBy::default(), fills: Vec::new() }
}

/// A marker line (`hash` false) or hashed line as strokes of markers or hash marks along the line:
/// one for each layer of the symbol it places, bottom first, all at its places.
fn along(l: Node, p: &[(String, String)], hash: bool, reference: Option<f64>, layer: &str, report: &mut Report) -> Vec<LineStroke> {
    let num = |k: &str| prop(p, k).and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite());
    let at = placements(p, layer, report);
    if at.is_empty() {
        return Vec::new();
    }
    if prop(p, "place_on_every_part") == Some("false") {
        report.approximated(layer, "markers along only the first part of a line, drawn along every part");
    }
    if num("trim_distance_start").is_some_and(|v| v != 0.0) || num("trim_distance_end").is_some_and(|v| v != 0.0) {
        report.approximated(layer, "markers along lines trimmed at their ends, drawn along the whole line");
    }
    let base = LineMarkers {
        at,
        interval: size(p, "interval", reference, layer, report).unwrap_or(3.0 * 96.0 / 25.4),
        offset_along: offset(p, "offset_along_line", reference, layer, report).unwrap_or(0.0),
        rotate: prop(p, "rotate") != Some("0"),
        // QGIS's default stretch is 4 mm.
        // (Its unit is `average_angle_unit`, not the `_length_unit` `size` would look for.)
        average_angle: prop(p, "average_angle_length")
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|v| v.is_finite())
            .map(|v| points(v, prop(p, "average_angle_unit").or(prop(p, SYMBOL_UNIT)), reference, layer, report))
            .unwrap_or(4.0 * 96.0 / 25.4),
        ..LineMarkers::default()
    };
    let across = offset(p, "offset", reference, layer, report).unwrap_or(0.0);
    let pass = l.attribute("pass").and_then(|v| v.trim().parse::<u8>().ok());
    let Some(sym) = l.children().find(|c| c.has_tag_name("symbol")) else { return Vec::new() };
    let alpha = sym.attribute("alpha").and_then(|a| a.trim().parse::<f32>().ok()).filter(|a| a.is_finite()).unwrap_or(1.0).clamp(0.0, 1.0);
    let fade = |c: Rgba| c.with_alpha((f32::from(c.a()) * alpha).round() as u8);
    let unit = sym.attribute("outputUnit");
    let mut out = Vec::new();
    for sub in sym.children().filter(|c| c.has_tag_name("layer") && c.attribute("enabled") != Some("0")).take(MAX_STROKES) {
        if data_defined_on(sub) {
            report.skipped(layer, "data-defined symbol settings (from fields or expressions)");
        }
        if has_effect(sub) {
            report.skipped(layer, "draw effects (shadows, glows, blurs and the like)");
        }
        let sp = settings(sub, unit);
        let class = sub.attribute("class").unwrap_or_default();
        let stroke = if hash {
            hash_mark(class, &sp, p, base, reference, layer, report)
        } else {
            marker_mark(class, &sp, base, reference, layer, report)
        };
        if let Some(mut st) = stroke {
            st.offset += across;
            st.level = pass;
            st.color = st.color.map(fade);
            if let Some(m) = &mut st.markers {
                m.outline = m.outline.map(fade);
            }
            out.push(st.clamped());
        }
    }
    out
}

/// Where a marker or hashed line puts its marks: `placements` (QGIS 3.24 on, several at once) or
/// `placement` (before); none for a placement RustGIS has no match for (noted).
fn placements(p: &[(String, String)], layer: &str, report: &mut Report) -> Placements {
    let text = prop(p, "placements").or(prop(p, "placement")).unwrap_or("Interval");
    let mut at = Placements::NONE;
    for code in text.split('|').map(str::trim).filter(|c| !c.is_empty()) {
        at = at.with(match code.to_ascii_lowercase().as_str() {
            "interval" => Placements::INTERVAL,
            "vertex" => Placements::VERTICES,
            "innervertices" => Placements::INNER_VERTICES,
            "firstvertex" => Placements::FIRST,
            "lastvertex" => Placements::LAST,
            "centralpoint" => Placements::MIDDLE,
            "segmentcenter" => Placements::SEGMENT_MIDDLES,
            "curvepoint" => {
                report.approximated(layer, "markers at curve points, drawn at the vertices");
                Placements::VERTICES
            }
            other => {
                report.skipped(layer, format!("markers along lines placed at \"{other}\""));
                Placements::NONE
            }
        });
    }
    at
}

/// A marker line's marker layer `class` (its settings `p`) as a stroke of markers at `base`'s
/// places, or `None` for one RustGIS has no match for (noted).
fn marker_mark(
    class: &str,
    p: &[(String, String)],
    base: LineMarkers,
    reference: Option<f64>,
    layer: &str,
    report: &mut Report,
) -> Option<LineStroke> {
    let shape = match class {
        "SimpleMarker" => {
            let name = prop(p, "name").unwrap_or("circle");
            shape_of(name).unwrap_or_else(|| {
                report.approximated(layer, format!("{name} markers, drawn as circles"));
                Marker::Circle
            })
        }
        "FontMarker" => font_char(p, layer, report)?,
        "SvgMarker" | "RasterMarker" | "EllipseMarker" | "FilledMarker" => {
            report.approximated(layer, format!("{} markers, drawn as circles in their colour", marker_name(class)));
            Marker::Circle
        }
        other => {
            report.skipped(layer, format!("{other} marker symbol layers"));
            return None;
        }
    };
    if class == "FontMarker" && anchored(p) {
        report.approximated(layer, "font markers anchored off their centre, drawn centred");
    }
    let fill = prop(p, "color").and_then(color).unwrap_or(Rgba::new(0, 0, 0, 0));
    let outline = if prop(p, "outline_style") == Some("no") { None } else { prop(p, "outline_color").and_then(color) };
    let width = if outline.is_some() { size(p, "outline_width", reference, layer, report).unwrap_or(0.0) } else { 0.0 };
    let (along_x, across) = marker_offset(p, base.rotate, reference, layer, report);
    let markers = LineMarkers {
        mark: LineMark::Marker(shape),
        size: size(p, "size", reference, layer, report).unwrap_or(2.0 * 96.0 / 25.4),
        offset_along: base.offset_along + along_x,
        angle: prop(p, "angle").and_then(|a| a.trim().parse::<f32>().ok()).unwrap_or(0.0),
        outline,
        ..base
    };
    Some(LineStroke { color: Some(fill), width: Some(width), offset: across, markers: Some(markers), ..LineStroke::default() })
}

/// A hashed line's line layer `class` (its settings `p`; the hashed line's `hp`) as a stroke of
/// hash marks at `base`'s places, or `None` for one RustGIS has no match for (noted).
fn hash_mark(
    class: &str,
    p: &[(String, String)],
    hp: &[(String, String)],
    base: LineMarkers,
    reference: Option<f64>,
    layer: &str,
    report: &mut Report,
) -> Option<LineStroke> {
    if class != "SimpleLine" {
        report.skipped(layer, format!("{class} line symbol layers in hash marks"));
        return None;
    }
    if prop(p, "line_style") == Some("no") {
        return None;
    }
    if dashed(prop(p, "line_style")) || prop(p, "use_custom_dash") == Some("1") {
        report.approximated(layer, "dashed hash marks, drawn solid");
    }
    let c = prop(p, "line_color").and_then(color)?;
    let markers = LineMarkers {
        mark: LineMark::Hash,
        size: size(hp, "hash_length", reference, layer, report).unwrap_or(3.0 * 96.0 / 25.4),
        angle: prop(hp, "hash_angle").and_then(|a| a.trim().parse::<f32>().ok()).unwrap_or(0.0),
        ..base
    };
    let width = size(p, "line_width", reference, layer, report).unwrap_or(0.26 * 96.0 / 25.4);
    let across = offset(p, "offset", reference, layer, report).unwrap_or(0.0);
    Some(LineStroke { color: Some(c), width: Some(width), offset: across, markers: Some(markers), ..LineStroke::default() })
}

/// A marker's offset (`offset`, "x,y" in its unit) along and across the line, in points: when it
/// turns with the line its x runs along it and its y to the right of it; when it doesn't, taken
/// the same way (noted).
fn marker_offset(p: &[(String, String)], rotate: bool, reference: Option<f64>, layer: &str, report: &mut Report) -> (f32, f32) {
    let Some((x, y)) = prop(p, "offset").and_then(|o| o.split_once(',')) else { return (0.0, 0.0) };
    let parse = |v: &str| v.trim().parse::<f64>().ok().filter(|v| v.is_finite()).unwrap_or(0.0);
    let (x, y) = (parse(x), parse(y));
    if x == 0.0 && y == 0.0 {
        return (0.0, 0.0);
    }
    if !rotate {
        report.approximated(layer, "offsets of upright markers along lines, taken along and across the line");
    }
    let unit = prop(p, "offset_unit").or(prop(p, SYMBOL_UNIT));
    (signed_points(x, unit, reference, layer, report), signed_points(y, unit, reference, layer, report))
}

/// `still`: the layer's markers turn by a field, but this symbol's don't.
fn marker(layers: &[(Node, Props)], reference: Option<f64>, still: bool, layer: &str, report: &mut Report) -> Converted {
    let mut symbol = hidden();
    // Each layer as a mark, bottom first: drawn as marks when one alone (the symbol) can't say it.
    let mut marks: Vec<LineStroke> = Vec::new();
    for (l, p) in layers {
        let class = l.attribute("class").unwrap_or_default();
        let fill = prop(p, "color").and_then(color);
        let outline = prop(p, "outline_color").and_then(color);
        let sz = size(p, "size", reference, layer, report).unwrap_or(2.0 * 96.0 / 25.4);
        let shape = match class {
            "SimpleMarker" => {
                let name = prop(p, "name").unwrap_or("circle");
                shape_of(name).unwrap_or_else(|| {
                    report.approximated(layer, format!("{name} markers, drawn as circles"));
                    Marker::Circle
                })
            }
            "FontMarker" => match font_char(p, layer, report) {
                Some(m) => m,
                None => continue,
            },
            "SvgMarker" | "RasterMarker" | "EllipseMarker" | "FilledMarker" => {
                report.approximated(layer, format!("{} markers, drawn as circles in their colour", marker_name(class)));
                Marker::Circle
            }
            other => {
                report.skipped(layer, format!("{other} marker symbol layers"));
                continue;
            }
        };
        let fill = fill.unwrap_or(Rgba::new(0, 0, 0, 0));
        let width = size(p, "outline_width", reference, layer, report).unwrap_or(0.0);
        let angle = prop(p, "angle").and_then(|a| a.trim().parse::<f32>().ok()).filter(|a| a.is_finite()).unwrap_or(0.0);
        let (x, y) = point_offset(p, sz, reference, layer, report);
        let drawn_outline = outline.filter(|_| prop(p, "outline_style") != Some("no") && width > 0.0);
        marks.push(LineStroke {
            color: Some(fill),
            width: Some(width),
            offset: y,
            markers: Some(LineMarkers {
                mark: LineMark::Marker(shape),
                size: sz,
                offset_along: x,
                angle,
                outline: drawn_outline,
                rotate: !still,
                at: Placements::FIRST,
                ..LineMarkers::default()
            }),
            ..LineStroke::default()
        });
        // Each layer drawn over the one before: the top one's look, the biggest size (the legend's
        // and the symbol's when it is drawn alone).
        symbol = Symbol {
            fill,
            stroke: outline.unwrap_or(fill),
            stroke_width: width,
            size: sz.max(if symbol.size > 0.0 { symbol.size } else { 0.0 }).clamp(0.0, 200.0),
            marker: shape,
        };
    }
    // One layer, centred and upright, is the symbol itself (drawn the quickest); more, or one shifted
    // or turned, are marks, as is one that keeps still where the layer's markers turn.
    let alone = !still
        && marks.len() <= 1
        && marks.iter().all(|m| m.offset == 0.0 && m.markers.is_some_and(|k| k.offset_along == 0.0 && k.angle == 0.0));
    let strokes = if alone { Vec::new() } else { marks.into_iter().map(LineStroke::clamped).collect() };
    Converted { symbol, strokes, rotation: None, by: SymbolBy::default(), fills: Vec::new() }
}

/// A point marker layer's offset (`offset`, "x,y" in its unit: right and down) in points, its
/// anchor point added: anchored at its left or top edge, a marker of size `size` sits half its
/// size right of or below the point, as QGIS draws it. A font marker's baseline anchor is drawn
/// centred (noted).
fn point_offset(p: &[(String, String)], size: f32, reference: Option<f64>, layer: &str, report: &mut Report) -> (f32, f32) {
    let parse = |v: &str| v.trim().parse::<f64>().ok().filter(|v| v.is_finite()).unwrap_or(0.0);
    let (x, y) = prop(p, "offset").and_then(|o| o.split_once(',')).map_or((0.0, 0.0), |(x, y)| (parse(x), parse(y)));
    let unit = prop(p, "offset_unit").or(prop(p, SYMBOL_UNIT));
    let (mut x, mut y) = if x == 0.0 && y == 0.0 {
        (0.0, 0.0)
    } else {
        (signed_points(x, unit, reference, layer, report), signed_points(y, unit, reference, layer, report))
    };
    let half = size / 2.0;
    match prop(p, "horizontal_anchor_point").map(str::trim) {
        Some("0") => x += half,
        Some("2") => x -= half,
        _ => {}
    }
    match prop(p, "vertical_anchor_point").map(str::trim) {
        Some("0") => y += half,
        Some("2") => y -= half,
        Some("3") => report.approximated(layer, "markers anchored at their baseline, drawn centred"),
        _ => {}
    }
    (x, y)
}

/// The data-defined properties switched on in `node`'s own `data_defined_properties`, by name.
fn active_properties<'a, 'i>(node: Node<'a, 'i>) -> Vec<(&'a str, Node<'a, 'i>)> {
    active_properties_in(node, "data_defined_properties")
}

/// The data-defined properties switched on in `node`'s own `tag` (`data_defined_properties` of a
/// symbol, `dd_properties` of label settings), by name.
pub(super) fn active_properties_in<'a, 'i>(node: Node<'a, 'i>, tag: &str) -> Vec<(&'a str, Node<'a, 'i>)> {
    let Some(dd) = node.children().find(|c| c.has_tag_name(tag)) else { return Vec::new() };
    let mut out = Vec::new();
    for props in dd.descendants().filter(|o| o.has_tag_name("Option") && o.attribute("name") == Some("properties")) {
        for p in props.children().filter(|c| c.has_tag_name("Option")).take(64) {
            let on = p.children().any(|a| a.attribute("name") == Some("active") && a.attribute("value") == Some("true"));
            if let (true, Some(name)) = (on, p.attribute("name")) {
                out.push((name, p));
            }
        }
    }
    out
}

/// A data-defined property's expression: a field's (`type` 2) quoted, or the expression's own
/// (`type` 3); `None` for a fixed value or text too long to read.
pub(super) fn property_text(property: Node) -> Option<String> {
    let named = |name: &str| {
        property.children().find(|c| c.has_tag_name("Option") && c.attribute("name") == Some(name)).and_then(|o| o.attribute("value"))
    };
    let text = match named("type").map(str::trim) {
        Some("2") => format!("\"{}\"", named("field")?.trim().replace('"', "\"\"")),
        Some("3") => super::expr::rustgis_scales(named("expression")?.trim()),
        _ => return None,
    };
    Some(text).filter(|t| !t.is_empty() && t != "\"\"" && t.chars().count() <= rustgis_data::query::MAX_LEN - 32)
}

/// Takes the data-defined property `name` of a symbol layer (its settings `p`) of a `kind`
/// symbol into `by` when RustGIS draws it by expression: a marker's size and angle, the fill and
/// outline colours and the outline's (a line's) width, sizes turned from the layer's unit into
/// points. False for one it doesn't. A second symbol layer's for the same keeps the first's (noted).
#[allow(clippy::too_many_arguments)]
fn by_expression(
    kind: Option<&str>,
    name: &str,
    property: Node,
    p: Option<&[(String, String)]>,
    reference: Option<f64>,
    by: &mut SymbolBy,
    layer: &str,
    report: &mut Report,
) -> bool {
    let (marker, fill, line) = (!matches!(kind, Some("fill" | "line")), kind == Some("fill"), kind == Some("line"));
    let sized = |key: &str, text: String, report: &mut Report| {
        let unit = p.and_then(|p| prop(p, &format!("{key}_unit")).or(prop(p, SYMBOL_UNIT)));
        let k = super::in_points(1.0, unit, reference, layer, report);
        if (k - 1.0).abs() < 1e-9 || !k.is_finite() { text } else { format!("({text}) * {}", (k * 1e6).round() / 1e6) }
    };
    let Some(text) = property_text(property) else { return false };
    let (slot, text) = match name {
        "size" if marker => (&mut by.size, sized("size", text, report)),
        "fillColor" if marker || fill => (&mut by.fill, text),
        "outlineColor" => (&mut by.stroke, text),
        "outlineWidth" if line => (&mut by.stroke_width, sized("line_width", text, report)),
        "outlineWidth" => (&mut by.stroke_width, sized("outline_width", text, report)),
        "angle" if marker => (&mut by.angle, text),
        _ => return false,
    };
    match slot {
        Some(had) if *had != text => {
            report.approximated(layer, "symbol layers with different data-defined settings, drawn with the first's")
        }
        Some(_) => {}
        None => *slot = Some(text),
    }
    true
}

/// A data-defined angle that is a field's value (a field property, or the expression `"field"`,
/// `- "field"`): the rotation it gives. `None` for any other expression.
fn field_angle(property: Node) -> Option<MarkerRotation> {
    let named = |name: &str| {
        property.children().find(|c| c.has_tag_name("Option") && c.attribute("name") == Some(name)).and_then(|o| o.attribute("value"))
    };
    let field = |f: &str| Some(f.to_string()).filter(|f| !f.is_empty() && f.len() <= 256);
    match named("type").map(str::trim) {
        Some("2") => Some(MarkerRotation { field: field(named("field")?.trim())?, counterclockwise: false }),
        Some("3") => {
            let e = named("expression")?.trim();
            let (minus, e) = match e.strip_prefix('-') {
                Some(rest) => (true, rest.trim()),
                None => (false, e),
            };
            let name = match e.strip_prefix('"').and_then(|e| e.strip_suffix('"')) {
                Some(quoted) if !quoted.contains('"') => quoted,
                Some(_) => return None,
                None if e.chars().all(|c| c.is_alphanumeric() || c == '_') => e,
                None => return None,
            };
            Some(MarkerRotation { field: field(name)?, counterclockwise: minus })
        }
        _ => None,
    }
}

/// A font marker's character (`chr`, of the family `font`) as a character marker: QGIS's `A` when
/// it has none, and the characters it saves escaped ([`unescape_chr`]) decoded. A
/// string of several draws its first (noted).
pub(super) fn font_char(p: &[(String, String)], layer: &str, report: &mut Report) -> Option<Marker> {
    let text = unescape_chr(prop(p, "chr").filter(|c| !c.is_empty()).unwrap_or("A"));
    let mut chars = text.chars();
    let ch = chars.next()?;
    if chars.next().is_some() {
        report.approximated(layer, "font markers of several characters, drawn as their first");
    }
    if prop(p, "font_style").is_some_and(|s| !s.trim().is_empty() && !s.trim().eq_ignore_ascii_case("regular")) {
        report.approximated(layer, "font marker styles (bold, italic and the like), drawn regular");
    }
    Some(Marker::Char(Glyph::new(ch, prop(p, "font").unwrap_or_default())))
}

/// `chr` as QGIS reads it: it saves some characters as their code between two `~!_#!#_!~`.
fn unescape_chr(chr: &str) -> String {
    const FIX: &str = "~!_#!#_!~";
    let (mut out, mut rest) = (String::new(), chr);
    while let Some(i) = rest.find(FIX) {
        let after = &rest[i + FIX.len()..];
        let digits = after.len() - after.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        let code = after[..digits].parse::<u16>().ok().and_then(|n| char::from_u32(n.into()));
        match code {
            Some(c) if after[digits..].starts_with(FIX) => {
                out.push_str(&rest[..i]);
                out.push(c);
                rest = &after[digits + FIX.len()..];
            }
            _ => {
                out.push_str(&rest[..i + FIX.len()]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Whether a marker is anchored off its centre (`horizontal_anchor_point`,
/// `vertical_anchor_point`: 1 is the centre).
fn anchored(p: &[(String, String)]) -> bool {
    ["horizontal_anchor_point", "vertical_anchor_point"].iter().any(|k| prop(p, k).is_some_and(|v| v.trim() != "1"))
}

/// QGIS's simple marker shapes RustGIS has.
fn shape_of(name: &str) -> Option<Marker> {
    Some(match name {
        "circle" => Marker::Circle,
        "square" | "rectangle" => Marker::Square,
        "triangle" | "equilateral_triangle" => Marker::Triangle,
        "diamond" => Marker::Diamond,
        "star" | "regular_star" => Marker::Star,
        _ => return None,
    })
}

/// A light tint of `c`, standing in for a pattern of it.
fn tint(c: Rgba) -> Rgba {
    c.with_alpha((f32::from(c.a()) * 0.35).round() as u8)
}

fn pattern_name(class: &str) -> &'static str {
    match class {
        "LinePatternFill" => "line pattern",
        "PointPatternFill" => "point pattern",
        "SVGFill" => "SVG",
        "RasterFill" => "raster image",
        _ => "random marker",
    }
}

fn marker_name(class: &str) -> &'static str {
    match class {
        "SvgMarker" => "SVG",
        "RasterMarker" => "raster image",
        "EllipseMarker" => "ellipse",
        _ => "filled",
    }
}
