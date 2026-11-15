//! An ArcGIS layer's own symbology (its description's `drawingInfo`, as ArcGIS REST's JSON has
//! it) translated onto a feature layer just added from its service (#2080).
use serde_json::Value;

use crate::symbology::{Dash, LineStroke, MAX_STROKE_WIDTH, Marker};
use crate::{Layer, Renderer, Rgba, Symbol};
use rustgis_data::GeometryType;

/// The largest marker, in points, as Style › Marker sizes one.
const MAX_MARKER_SIZE: f32 = 80.0;

/// The colour a picture marker is drawn in, which RustGIS can't draw yet.
const PICTURE_FILL: Rgba = Rgba::rgb(0x80, 0x80, 0x80);

/// What of a service's symbology was drawn differently or left out, each with how many symbols it
/// came up for, in the order first met.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Notes(Vec<(&'static str, usize)>);

impl Notes {
    fn add(&mut self, text: &'static str) {
        match self.0.iter_mut().find(|(t, _)| *t == text) {
            Some((_, n)) => *n = n.saturating_add(1),
            None => self.0.push((text, 1)),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The notes as one sentence's list: `"hatched fills, drawn solid (3 symbols); …"`.
    pub fn text(&self) -> String {
        self.0.iter().map(|(t, n)| if *n > 1 { format!("{t} ({n} symbols)") } else { (*t).to_string() }).collect::<Vec<_>>().join("; ")
    }
}

/// Gives `layer`, a feature layer just added from an ArcGIS service, the symbology its
/// description's `drawingInfo` sets: its renderer, when RustGIS draws its kind, and its
/// transparency. What can't be drawn as the service draws it is noted, its labels too; a renderer
/// of a kind RustGIS doesn't read leaves the layer's own. Anything but an object changes nothing.
pub fn apply(layer: &mut Layer, drawing_info: &Value, notes: &mut Notes) {
    if !drawing_info.is_object() || layer.is_raster() {
        return;
    }
    if let Some(t) = drawing_info.get("transparency").and_then(Value::as_f64).filter(|t| t.is_finite()) {
        if !(0.0..=100.0).contains(&t) {
            notes.add("transparency past 0 to 100, brought into it");
        }
        layer.transparency = t.clamp(0.0, 100.0) as f32;
    }
    // The service's labels aren't carried over (#2080), unless it shows none.
    let labelled = drawing_info.get("labelingInfo").and_then(Value::as_array).is_some_and(|l| !l.is_empty());
    if labelled && drawing_info.get("showLabels").and_then(Value::as_bool) != Some(false) {
        notes.add("its labels, left out");
    }
    let Some(renderer) = drawing_info.get("renderer").filter(|r| r.is_object()) else { return };
    let g = layer.geometry_type();
    if g == GeometryType::None {
        return;
    }
    match renderer.get("type").and_then(Value::as_str) {
        Some("simple") if renderer.get("symbol").is_none_or(Value::is_null) => {
            notes.add("its renderer names no symbol, drawn with RustGIS's single symbol");
        }
        Some("simple") => {
            if let Some((symbol, dash)) = symbol(renderer.get("symbol").unwrap_or(&Value::Null), g, notes) {
                layer.renderer = Renderer::Single(symbol);
                layer.line_strokes = dash.map(|d| vec![LineStroke { dash: Some(d), ..LineStroke::default() }]).unwrap_or_default();
            }
        }
        Some("uniqueValue") => notes.add("its renderer by unique values, drawn with RustGIS's single symbol"),
        Some("classBreaks") => notes.add("its renderer by class breaks, drawn with RustGIS's single symbol"),
        _ => notes.add("its renderer, of a kind RustGIS doesn't read, drawn with RustGIS's single symbol"),
    }
    if renderer.get("visualVariables").and_then(Value::as_array).is_some_and(|v| !v.is_empty()) {
        notes.add("sizes, colours or rotations by value (visual variables), left out");
    }
    if renderer.get("rotationExpression").and_then(Value::as_str).is_some_and(|e| !e.trim().is_empty()) {
        notes.add("markers rotated by a field, drawn upright");
    }
}

/// An ArcGIS symbol as a RustGIS symbol for a `g` layer, with a line's dash pattern; `None` (noted)
/// for a symbol that isn't for that geometry or that RustGIS can't draw.
fn symbol(s: &Value, g: GeometryType, notes: &mut Notes) -> Option<(Symbol, Option<Dash>)> {
    let kind = s.get("type").and_then(Value::as_str).unwrap_or_default();
    let base = Symbol::default_for(g, 0);
    match (kind, g) {
        ("esriSFS", GeometryType::Polygon) => {
            let style = s.get("style").and_then(Value::as_str).unwrap_or("esriSFSSolid");
            let fill = match style {
                "esriSFSNull" | "esriSFSHollow" => Rgba::TRANSPARENT,
                "esriSFSSolid" => color(s.get("color"), notes),
                "esriSFSBackwardDiagonal"
                | "esriSFSCross"
                | "esriSFSDiagonalCross"
                | "esriSFSForwardDiagonal"
                | "esriSFSHorizontal"
                | "esriSFSVertical" => {
                    notes.add("hatched fills, drawn solid");
                    color(s.get("color"), notes)
                }
                _ => {
                    notes.add("fill styles RustGIS doesn't draw, drawn solid");
                    color(s.get("color"), notes)
                }
            };
            let (stroke, stroke_width) = outline(s, "dashed polygon outlines, drawn solid", notes);
            Some((Symbol { fill, stroke, stroke_width, ..base }, None))
        }
        ("esriSLS", GeometryType::Polyline) => {
            let (c, width, dash) = line(Some(s), base.stroke_width, notes);
            Some((Symbol { fill: c, stroke: c, stroke_width: width, ..base }, dash))
        }
        ("esriSMS", GeometryType::Point) => {
            let marker = match s.get("style").and_then(Value::as_str) {
                Some("esriSMSSquare") => Marker::Square,
                Some("esriSMSDiamond") => Marker::Diamond,
                Some("esriSMSTriangle") => Marker::Triangle,
                Some("esriSMSCross") => {
                    notes.add("cross and x markers, drawn as + and × characters in the outline's colour");
                    Marker::Char(crate::symbology::Glyph::new('+', ""))
                }
                Some("esriSMSX") => {
                    notes.add("cross and x markers, drawn as + and × characters in the outline's colour");
                    Marker::Char(crate::symbology::Glyph::new('×', ""))
                }
                None | Some("esriSMSCircle") => Marker::Circle,
                Some(_) => {
                    notes.add("marker shapes RustGIS doesn't draw, drawn as circles");
                    Marker::Circle
                }
            };
            let (stroke, stroke_width) = outline(s, "dashed marker outlines, drawn solid", notes);
            if offset_or_angle(s) {
                notes.add("markers' angles and offsets, left out");
            }
            // A cross or an x is all outline in ArcGIS: its character is drawn in the outline's
            // colour, and nothing with none.
            if matches!(marker, Marker::Char(_)) {
                let size = size(s.get("size"), base.size, notes);
                return Some((Symbol { fill: stroke, stroke: Rgba::TRANSPARENT, stroke_width: 0.0, size, marker }, None));
            }
            let fill = color(s.get("color"), notes);
            Some((Symbol { fill, stroke, stroke_width, size: size(s.get("size"), base.size, notes), marker }, None))
        }
        ("esriPMS", GeometryType::Point) => {
            notes.add("picture markers, drawn as grey circles");
            // The larger of its width and height, so that one past the largest is said once.
            let points = |k: &str| s.get(k).and_then(Value::as_f64).filter(|v| v.is_finite()).unwrap_or(f64::NEG_INFINITY);
            let larger = if points("height") > points("width") { s.get("height") } else { s.get("width") };
            let size = size(larger, base.size, notes);
            Some((Symbol { fill: PICTURE_FILL, size, ..base }, None))
        }
        ("esriPFS", GeometryType::Polygon) => {
            notes.add("picture fills, drawn with their outline only");
            let (stroke, stroke_width) = outline(s, "dashed polygon outlines, drawn solid", notes);
            Some((Symbol { fill: Rgba::TRANSPARENT, stroke, stroke_width, ..base }, None))
        }
        ("esriSFS" | "esriSLS" | "esriSMS" | "esriPMS" | "esriPFS", _) => {
            notes.add("a symbol for another kind of geometry, drawn with RustGIS's own");
            None
        }
        _ => {
            notes.add("a symbol of a kind RustGIS doesn't read, drawn with RustGIS's own");
            None
        }
    }
}

/// A fill's or a marker's outline (`outline`, a simple line symbol): its colour and width, drawn
/// solid; a dashed one is said with `dashed`.
fn outline(s: &Value, dashed: &'static str, notes: &mut Notes) -> (Rgba, f32) {
    let (stroke, width, dash) = line(s.get("outline"), 0.0, notes);
    if dash.is_some() {
        notes.add(dashed);
    }
    (stroke, width)
}

/// A simple line symbol's (`esriSLS`) colour, width in points and dash pattern: none at all
/// (transparent, `0`) when absent or of style `esriSLSNull`, `width` when it names none. A style
/// RustGIS doesn't know is drawn solid, noted.
fn line(s: Option<&Value>, width: f32, notes: &mut Notes) -> (Rgba, f32, Option<Dash>) {
    let Some(s) = s.filter(|s| s.is_object()) else { return (Rgba::TRANSPARENT, 0.0, None) };
    let style = s.get("style").and_then(Value::as_str).unwrap_or("esriSLSSolid");
    if style == "esriSLSNull" {
        return (Rgba::TRANSPARENT, 0.0, None);
    }
    let w = s.get("width").and_then(Value::as_f64).filter(|w| w.is_finite()).map_or(width, |w| {
        if w > f64::from(MAX_STROKE_WIDTH) {
            notes.add("line widths past RustGIS's widest (50 points), drawn at it");
        } else if w < 0.0 {
            notes.add("line widths below 0, drawn at 0");
        }
        w.clamp(0.0, f64::from(MAX_STROKE_WIDTH)) as f32
    });
    let dash = dash(style, w).unwrap_or_else(|| {
        notes.add("line styles RustGIS doesn't draw, drawn solid");
        None
    });
    (color(s.get("color"), notes), w, dash)
}

/// The dash pattern of a simple line style, in points: ArcGIS's, in line widths (as the ArcGIS
/// Maps SDK for JavaScript draws them), at least a point wide; `Some(None)` for a solid line, and
/// `None` for a style RustGIS doesn't know.
fn dash(style: &str, width: f32) -> Option<Option<Dash>> {
    let steps: &[f32] = match style {
        "esriSLSSolid" | "esriSLSInsideFrame" => return Some(None),
        "esriSLSDash" => &[4.0, 3.0],
        "esriSLSDot" => &[1.0, 3.0],
        "esriSLSDashDot" => &[4.0, 3.0, 1.0, 3.0],
        "esriSLSDashDotDot" | "esriSLSLongDashDotDot" => &[8.0, 3.0, 1.0, 3.0, 1.0, 3.0],
        "esriSLSLongDash" => &[8.0, 3.0],
        "esriSLSLongDashDot" => &[8.0, 3.0, 1.0, 3.0],
        "esriSLSShortDash" => &[4.0, 1.0],
        "esriSLSShortDot" => &[1.0, 1.0],
        "esriSLSShortDashDot" => &[4.0, 1.0, 1.0, 1.0],
        "esriSLSShortDashDotDot" => &[4.0, 1.0, 1.0, 1.0, 1.0, 1.0],
        _ => return None,
    };
    let unit = width.max(1.0);
    Some(Dash::new(&steps.iter().map(|s| s * unit).collect::<Vec<_>>()))
}

/// An ArcGIS colour, `[r, g, b, a]` from 0 to 255 (`[r, g, b]` opaque); none or `null` is no
/// colour (transparent), as ArcGIS draws it. Anything else is transparent too, and said; a part
/// past 0 to 255 is brought into it, said.
fn color(v: Option<&Value>, notes: &mut Notes) -> Rgba {
    let Some(v) = v.filter(|v| !v.is_null()) else { return Rgba::TRANSPARENT };
    let parts = v.as_array().filter(|a| a.len() == 3 || a.len() == 4);
    let read: Option<Vec<f64>> = parts.and_then(|p| p.iter().map(|n| n.as_f64().filter(|n| n.is_finite())).collect());
    let Some(read) = read else {
        notes.add("colours RustGIS can't read, drawn transparent");
        return Rgba::TRANSPARENT;
    };
    let mut c = [0u8, 0, 0, 255];
    let mut brought_in = false;
    for (slot, n) in c.iter_mut().zip(read) {
        brought_in |= !(0.0..=255.0).contains(&n.round());
        *slot = n.round().clamp(0.0, 255.0) as u8;
    }
    if brought_in {
        notes.add("colour parts past 0 to 255, brought into it");
    }
    Rgba(c)
}

/// A marker's size in points, `default` when it names none, within Style › Marker's range; one
/// past it, or below 0, is said.
fn size(v: Option<&Value>, default: f32, notes: &mut Notes) -> f32 {
    v.and_then(Value::as_f64).filter(|s| s.is_finite()).map_or(default, |s| {
        if s > f64::from(MAX_MARKER_SIZE) {
            notes.add("marker sizes past RustGIS's largest (80 points), drawn at it");
        } else if s < 0.0 {
            notes.add("marker sizes below 0, drawn at 0");
        }
        (s as f32).clamp(0.0, MAX_MARKER_SIZE)
    })
}

/// Whether a marker is turned or shifted from its point.
fn offset_or_angle(s: &Value) -> bool {
    ["angle", "xoffset", "yoffset"].iter().any(|k| s.get(*k).and_then(Value::as_f64).is_some_and(|v| v.is_finite() && v != 0.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_data::{Dataset, Field, FieldType};
    use serde_json::json;

    fn layer(g: GeometryType) -> Layer {
        Layer::new(1, Dataset::new("l", g, vec![Field::new("NAME", FieldType::Text)]))
    }

    fn styled(g: GeometryType, info: Value) -> (Layer, Notes) {
        let mut l = layer(g);
        let mut notes = Notes::default();
        apply(&mut l, &info, &mut notes);
        (l, notes)
    }

    #[test]
    fn a_simple_fill_takes_the_services_colours_outline_and_transparency() {
        let info = json!({"renderer": {"type": "simple", "symbol": {"type": "esriSFS", "style": "esriSFSSolid", "color": [255, 211, 127, 128],
            "outline": {"type": "esriSLS", "style": "esriSLSSolid", "color": [110, 110, 110, 255], "width": 0.4}}}, "transparency": 30});
        let (l, notes) = styled(GeometryType::Polygon, info);
        let Renderer::Single(s) = l.renderer else { panic!("a single symbol") };
        assert_eq!((s.fill, s.stroke, s.stroke_width), (Rgba::new(255, 211, 127, 128), Rgba::rgb(110, 110, 110), 0.4));
        assert_eq!(l.transparency, 30.0);
        assert!(notes.is_empty());
    }

    #[test]
    fn a_hollow_fill_or_a_null_outline_draws_nothing_there() {
        let info = json!({"renderer": {"type": "simple", "symbol": {"type": "esriSFS", "style": "esriSFSNull", "color": [1, 2, 3, 255],
            "outline": {"type": "esriSLS", "style": "esriSLSNull", "color": [0, 0, 0, 255], "width": 1}}}});
        let (l, _) = styled(GeometryType::Polygon, info);
        let Renderer::Single(s) = l.renderer else { panic!("a single symbol") };
        assert_eq!((s.fill.a(), s.stroke.a(), s.stroke_width), (0, 0, 0.0));
        // A null colour is no colour, as ArcGIS draws it.
        let info = json!({"renderer": {"type": "simple", "symbol": {"type": "esriSFS", "color": null}}});
        let (l, _) = styled(GeometryType::Polygon, info);
        let Renderer::Single(s) = l.renderer else { panic!("a single symbol") };
        assert_eq!((s.fill.a(), s.stroke.a()), (0, 0));
    }

    #[test]
    fn a_hatched_fill_is_drawn_solid_and_said() {
        let info = json!({"renderer": {"type": "simple", "symbol": {"type": "esriSFS", "style": "esriSFSForwardDiagonal", "color": [0, 0, 255, 255],
            "outline": {"type": "esriSLS", "style": "esriSLSDash", "color": [0, 0, 0, 255], "width": 1}}}});
        let (l, notes) = styled(GeometryType::Polygon, info);
        let Renderer::Single(s) = l.renderer else { panic!("a single symbol") };
        assert_eq!(s.fill, Rgba::rgb(0, 0, 255));
        assert_eq!(notes.text(), "hatched fills, drawn solid; dashed polygon outlines, drawn solid");
    }

    #[test]
    fn a_dashed_line_keeps_its_width_and_pattern_in_line_widths() {
        let info = json!({"renderer": {"type": "simple", "symbol": {"type": "esriSLS", "style": "esriSLSDashDot", "color": [230, 0, 0, 255], "width": 2}}});
        let (l, notes) = styled(GeometryType::Polyline, info);
        let Renderer::Single(s) = l.renderer else { panic!("a single symbol") };
        assert_eq!((s.stroke, s.fill, s.stroke_width), (Rgba::rgb(230, 0, 0), Rgba::rgb(230, 0, 0), 2.0));
        assert_eq!(l.line_strokes.len(), 1);
        assert_eq!(l.line_strokes[0].dash.map(|d| d.steps().to_vec()), Some(vec![8.0, 6.0, 2.0, 6.0]));
        assert_eq!((l.line_strokes[0].color, l.line_strokes[0].width), (None, None), "the stroke follows the symbol");
        assert!(notes.is_empty());
        // A solid line has no stacked strokes.
        let info = json!({"renderer": {"type": "simple", "symbol": {"type": "esriSLS", "style": "esriSLSSolid", "color": [0, 0, 0, 255], "width": 1}}});
        assert!(styled(GeometryType::Polyline, info).0.line_strokes.is_empty());
    }

    #[test]
    fn a_simple_marker_takes_its_shape_size_and_outline() {
        let info = json!({"renderer": {"type": "simple", "symbol": {"type": "esriSMS", "style": "esriSMSDiamond", "color": [56, 168, 0, 255], "size": 9,
            "outline": {"color": [255, 255, 255, 255], "width": 1.5}}}});
        let (l, notes) = styled(GeometryType::Point, info);
        let Renderer::Single(s) = l.renderer else { panic!("a single symbol") };
        assert_eq!(
            (s.marker, s.fill, s.stroke, s.stroke_width, s.size),
            (Marker::Diamond, Rgba::rgb(56, 168, 0), Rgba::rgb(255, 255, 255), 1.5, 9.0)
        );
        assert!(notes.is_empty());
        for (style, ch) in [("esriSMSCross", '+'), ("esriSMSX", '×')] {
            let info = json!({"renderer": {"type": "simple", "symbol": {"type": "esriSMS", "style": style, "color": [0, 0, 255, 255], "size": 8, "angle": 45,
                "outline": {"type": "esriSLS", "style": "esriSLSSolid", "color": [230, 0, 0, 255], "width": 1}}}});
            let (l, notes) = styled(GeometryType::Point, info);
            let Renderer::Single(s) = l.renderer else { panic!("a single symbol") };
            assert_eq!(s.marker, Marker::Char(crate::symbology::Glyph::new(ch, "")));
            // All outline: the character takes the outline's colour, with no outline of its own.
            assert_eq!((s.fill, s.stroke, s.stroke_width, s.size), (Rgba::rgb(230, 0, 0), Rgba::TRANSPARENT, 0.0, 8.0));
            assert_eq!(
                notes.text(),
                "cross and x markers, drawn as + and × characters in the outline's colour; markers' angles and offsets, left out"
            );
            // Without an outline, ArcGIS draws nothing, whatever its colour.
            let info = json!({"renderer": {"type": "simple", "symbol": {"type": "esriSMS", "style": style, "color": [0, 0, 255, 255], "outline": null}}});
            let Renderer::Single(s) = styled(GeometryType::Point, info).0.renderer else { panic!("a single symbol") };
            assert_eq!((s.fill, s.stroke), (Rgba::TRANSPARENT, Rgba::TRANSPARENT));
        }
    }

    #[test]
    fn a_picture_marker_is_a_grey_circle_of_its_size_and_said() {
        let info =
            json!({"renderer": {"type": "simple", "symbol": {"type": "esriPMS", "imageData": "iVBORw0KGgo=", "width": 15, "height": 18}}});
        let (l, notes) = styled(GeometryType::Point, info);
        let Renderer::Single(s) = l.renderer else { panic!("a single symbol") };
        assert_eq!((s.marker, s.fill, s.size), (Marker::Circle, PICTURE_FILL, 18.0));
        assert_eq!(notes.text(), "picture markers, drawn as grey circles");
    }

    #[test]
    fn what_rustgis_doesnt_draw_leaves_the_layers_own_symbol_and_is_said() {
        let own = layer(GeometryType::Polygon).renderer;
        for (info, said) in [
            (
                json!({"renderer": {"type": "uniqueValue", "field1": "NAME", "uniqueValueInfos": []}}),
                "its renderer by unique values, drawn with RustGIS's single symbol",
            ),
            (
                json!({"renderer": {"type": "classBreaks", "field": "NAME", "classBreakInfos": []}}),
                "its renderer by class breaks, drawn with RustGIS's single symbol",
            ),
            (json!({"renderer": {"type": "heatmap"}}), "its renderer, of a kind RustGIS doesn't read, drawn with RustGIS's single symbol"),
            (
                json!({"renderer": {"type": "simple", "symbol": {"type": "esriSMS"}}}),
                "a symbol for another kind of geometry, drawn with RustGIS's own",
            ),
            (
                json!({"renderer": {"type": "simple", "symbol": {"type": "CIMSymbolReference"}}}),
                "a symbol of a kind RustGIS doesn't read, drawn with RustGIS's own",
            ),
        ] {
            let (l, notes) = styled(GeometryType::Polygon, info);
            assert_eq!(l.renderer, own);
            assert_eq!(notes.text(), said);
        }
    }

    #[test]
    fn visual_variables_and_rotation_are_said_left_out() {
        let info = json!({"renderer": {"type": "simple", "symbol": {"type": "esriSMS", "color": [0, 0, 0, 255], "size": 6},
            "visualVariables": [{"type": "sizeInfo", "field": "POP"}], "rotationExpression": "[ANGLE]"}});
        let (_, notes) = styled(GeometryType::Point, info);
        assert_eq!(
            notes.text(),
            "sizes, colours or rotations by value (visual variables), left out; markers rotated by a field, drawn upright"
        );
    }

    #[test]
    fn the_services_labels_are_said_left_out_unless_it_shows_none() {
        let symbol = json!({"type": "simple", "symbol": {"type": "esriSFS", "color": [1, 2, 3, 255]}});
        let labels = json!([{"labelExpression": "[NAME]", "symbol": {"type": "esriTS"}}]);
        for (info, said) in [
            (json!({"renderer": symbol.clone(), "labelingInfo": labels.clone()}), "its labels, left out"),
            (json!({"renderer": symbol.clone(), "labelingInfo": labels, "showLabels": false}), ""),
            (json!({"renderer": symbol.clone(), "labelingInfo": []}), ""),
            (json!({"renderer": symbol, "labelingInfo": null}), ""),
        ] {
            assert_eq!(styled(GeometryType::Polygon, info).1.text(), said);
        }
    }

    #[test]
    fn styles_rustgis_doesnt_know_are_drawn_near_and_said() {
        let info = json!({"renderer": {"type": "simple", "symbol": {"type": "esriSMS", "style": "esriSMSPath", "color": [0, 0, 0, 255]}}});
        let (l, notes) = styled(GeometryType::Point, info);
        let Renderer::Single(s) = l.renderer else { panic!("a single symbol") };
        assert_eq!((s.marker, notes.text().as_str()), (Marker::Circle, "marker shapes RustGIS doesn't draw, drawn as circles"));
        // No style is a circle, as ArcGIS draws it.
        let info = json!({"renderer": {"type": "simple", "symbol": {"type": "esriSMS", "color": [0, 0, 0, 255]}}});
        assert!(styled(GeometryType::Point, info).1.is_empty());
        let info = json!({"renderer": {"type": "simple", "symbol": {"type": "esriSLS", "style": "esriSLSWavy", "color": [0, 0, 0, 255], "width": 1}}});
        let (l, notes) = styled(GeometryType::Polyline, info);
        assert!(l.line_strokes.is_empty(), "solid");
        assert_eq!(notes.text(), "line styles RustGIS doesn't draw, drawn solid");
        let info =
            json!({"renderer": {"type": "simple", "symbol": {"type": "esriSLS", "style": "esriSLSInsideFrame", "color": [0, 0, 0, 255]}}});
        assert!(styled(GeometryType::Polyline, info).1.is_empty(), "drawn solid, as ArcGIS draws it");
        let info =
            json!({"renderer": {"type": "simple", "symbol": {"type": "esriSFS", "style": "esriSFSDotted", "color": [9, 9, 9, 255]}}});
        let (l, notes) = styled(GeometryType::Polygon, info);
        let Renderer::Single(s) = l.renderer else { panic!("a single symbol") };
        assert_eq!((s.fill, notes.text().as_str()), (Rgba::rgb(9, 9, 9), "fill styles RustGIS doesn't draw, drawn solid"));
    }

    #[test]
    fn a_dashed_marker_or_picture_fill_outline_is_drawn_solid_and_said() {
        let dashed = json!({"type": "esriSLS", "style": "esriSLSDash", "color": [0, 0, 0, 255], "width": 1});
        let info = json!({"renderer": {"type": "simple", "symbol": {"type": "esriSMS", "color": [1, 2, 3, 255], "size": 6, "outline": dashed.clone()}}});
        assert_eq!(styled(GeometryType::Point, info).1.text(), "dashed marker outlines, drawn solid");
        let info = json!({"renderer": {"type": "simple", "symbol": {"type": "esriPFS", "outline": dashed}}});
        assert_eq!(
            styled(GeometryType::Polygon, info).1.text(),
            "picture fills, drawn with their outline only; dashed polygon outlines, drawn solid"
        );
    }

    #[test]
    fn values_past_rustgis_ranges_or_unreadable_are_said() {
        let info = json!({"renderer": {"type": "simple", "symbol": {"type": "esriSMS", "color": [1, 2, "x", 255], "size": 120,
            "outline": {"color": [0, 0, 0, 255], "width": 60}}}});
        assert_eq!(
            styled(GeometryType::Point, info).1.text(),
            "line widths past RustGIS's widest (50 points), drawn at it; colours RustGIS can't read, drawn transparent; marker sizes past RustGIS's largest (80 points), drawn at it"
        );
        // A colour of none, or null, is transparent as ArcGIS draws it, and in range says nothing.
        let info =
            json!({"renderer": {"type": "simple", "symbol": {"type": "esriSMS", "color": null, "size": 80, "outline": {"width": 50}}}});
        assert!(styled(GeometryType::Point, info).1.is_empty());
        // A simple renderer without a symbol says so.
        let info = json!({"renderer": {"type": "simple"}});
        assert_eq!(styled(GeometryType::Point, info).1.text(), "its renderer names no symbol, drawn with RustGIS's single symbol");
    }

    #[test]
    fn notes_count_what_comes_up_again() {
        let mut notes = Notes::default();
        notes.add("hatched fills, drawn solid");
        notes.add("hatched fills, drawn solid");
        assert_eq!(notes.text(), "hatched fills, drawn solid (2 symbols)");
    }

    #[test]
    fn hostile_values_are_brought_into_range_or_ignored() {
        let info = json!({"renderer": {"type": "simple", "symbol": {"type": "esriSMS", "color": [999, -5, "x", 1], "size": 1e9,
            "outline": {"color": [0, 0, 0], "width": -3}}}, "transparency": 1e9});
        let (l, notes) = styled(GeometryType::Point, info);
        let Renderer::Single(s) = l.renderer else { panic!("a single symbol") };
        assert_eq!((s.fill, s.stroke, s.stroke_width, s.size), (Rgba::TRANSPARENT, Rgba::rgb(0, 0, 0), 0.0, MAX_MARKER_SIZE));
        assert_eq!(l.transparency, 100.0);
        assert_eq!(
            notes.text(),
            "transparency past 0 to 100, brought into it; line widths below 0, drawn at 0; \
             colours RustGIS can't read, drawn transparent; marker sizes past RustGIS's largest (80 points), drawn at it"
        );
        // Parts past 0 to 255, a negative size and a transparency below 0 are brought in, said.
        let info =
            json!({"renderer": {"type": "simple", "symbol": {"type": "esriSMS", "color": [999, -5, 0], "size": -4}}, "transparency": -1});
        let (l, notes) = styled(GeometryType::Point, info);
        let Renderer::Single(s) = l.renderer else { panic!("a single symbol") };
        assert_eq!((s.fill, s.size, l.transparency), (Rgba::rgb(255, 0, 0), 0.0, 0.0));
        assert_eq!(
            notes.text(),
            "transparency past 0 to 100, brought into it; colour parts past 0 to 255, brought into it; marker sizes below 0, drawn at 0"
        );
        // At the ends of each range, nothing is said.
        let info = json!({"renderer": {"type": "simple", "symbol": {"type": "esriSMS", "color": [255, 0, 0, 0], "size": 0,
            "outline": {"color": [0, 0, 0], "width": 0}}}, "transparency": 100});
        assert!(styled(GeometryType::Point, info).1.is_empty());
        // A picture marker larger than the largest both ways is said once.
        let info = json!({"renderer": {"type": "simple", "symbol": {"type": "esriPMS", "width": 100, "height": 120}}});
        let (l, notes) = styled(GeometryType::Point, info);
        let Renderer::Single(s) = l.renderer else { panic!("a single symbol") };
        assert_eq!(s.size, MAX_MARKER_SIZE);
        assert_eq!(notes.text(), "picture markers, drawn as grey circles; marker sizes past RustGIS's largest (80 points), drawn at it");
        // Not an object: nothing changes.
        for info in [json!(null), json!([]), json!("simple"), json!({"renderer": "simple"})] {
            let (l, notes) = styled(GeometryType::Point, info);
            assert_eq!(l.renderer, layer(GeometryType::Point).renderer);
            assert!(notes.is_empty());
        }
    }
}
