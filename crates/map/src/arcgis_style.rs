//! An ArcGIS layer's own symbology (its description's `drawingInfo`, as ArcGIS REST's JSON has
//! it) translated onto a feature layer just added from its service (#2080).
use serde_json::Value;

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::symbology::{ClassMethod, Dash, LineStroke, MAX_PICTURE_BYTES, MAX_STROKE_WIDTH, Marker, OTHER_FILL, Picture, normalize_label};
use crate::{Layer, Ramp, Renderer, Rgba, Symbol};
use rustgis_data::{FieldType, GeometryType};

/// The largest marker, in points, as Style › Marker sizes one.
const MAX_MARKER_SIZE: f32 = 80.0;

/// The most classes a service's renderer brings, as many as a QGIS style's.
const MAX_CLASSES: usize = 4096;

/// The colour of a circle drawn for a picture marker whose image RustGIS can't draw.
const PICTURE_FILL: Rgba = Rgba::rgb(0x80, 0x80, 0x80);

/// What of a service's symbology was drawn differently or left out, each with how many symbols it
/// came up for, in the order first met.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Notes(Vec<(Cow<'static, str>, usize)>);

impl Notes {
    fn add(&mut self, text: impl Into<Cow<'static, str>>) {
        let text = text.into();
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
        self.0.iter().map(|(t, n)| if *n > 1 { format!("{t} ({n} symbols)") } else { t.to_string() }).collect::<Vec<_>>().join("; ")
    }
}

/// Picture markers' images (`imageData`) as a service gives them, each checked and kept once,
/// however many classes show it.
struct Pictures<'r> {
    /// Whether an image is one a picture marker draws, or why not after "whose image"
    /// (`render::MarkerRefusal::whose`): the check `layer.marker` makes, which is the renderer's,
    /// so that what draws here draws there.
    readable: &'r dyn Fn(&str) -> Result<(), &'static str>,
    /// Each image met, with its picture or why it isn't drawn.
    met: HashMap<String, Result<Picture, Cow<'static, str>>>,
}

impl Pictures<'_> {
    /// A picture marker's image as a picture, or the note saying why it is drawn as a circle.
    fn of(&mut self, s: &Value) -> Result<Picture, Cow<'static, str>> {
        let Some(b64) = s.get("imageData").and_then(Value::as_str).map(str::trim).filter(|b| !b.is_empty()) else {
            // RustGIS doesn't fetch a marker's image from its `url`.
            return Err(if s.get("url").and_then(Value::as_str).is_some_and(|u| !u.trim().is_empty()) {
                "picture markers whose image is only at a URL, drawn as circles".into()
            } else {
                "picture markers with no image, drawn as circles".into()
            });
        };
        // Looked up first, so that classes sharing an image don't each copy it.
        if let Some(met) = self.met.get(b64) {
            return met.clone();
        }
        self.met
            .entry(b64.to_owned())
            .or_insert_with(|| {
                // Too long to keep is said before decoding it. What is decoded is bounded by the
                // description's own cap (`DESCRIPTION_MAX`, 4 MiB), as `layer.marker` decodes one.
                if b64.len() > MAX_PICTURE_BYTES {
                    return Err("picture markers past 2 MiB of base64, drawn as circles".into());
                }
                (self.readable)(b64).map_err(|why| format!("picture markers {why}, drawn as circles"))?;
                // Past the images a run keeps (from a crafted service), it isn't kept.
                Some(Picture::new(b64))
                    .filter(|p| !p.is_default())
                    .ok_or("picture markers past the images RustGIS keeps, drawn as circles".into())
            })
            .clone()
    }
}

/// Gives `layer`, a feature layer just added from an ArcGIS service, the symbology its
/// description's `drawingInfo` sets: its renderer, when RustGIS draws its kind (a simple one, by
/// unique values of a field or by class breaks), and its transparency. A picture marker draws its
/// image when `readable` (the renderer's check) takes it, else says why after "whose image".
/// What can't be drawn as the service draws it is noted, its labels too; a renderer RustGIS
/// doesn't read leaves the layer's own. Anything but an object changes nothing.
pub fn apply(layer: &mut Layer, drawing_info: &Value, readable: &dyn Fn(&str) -> Result<(), &'static str>, notes: &mut Notes) {
    if !drawing_info.is_object() || layer.is_raster() {
        return;
    }
    match drawing_info.get("transparency").filter(|t| !t.is_null()).map(|t| t.as_f64().filter(|t| t.is_finite())) {
        Some(Some(t)) => {
            if !(0.0..=100.0).contains(&t) {
                notes.add("transparency past 0 to 100, brought into it");
            }
            layer.transparency = t.clamp(0.0, 100.0) as f32;
        }
        Some(None) => notes.add("a transparency RustGIS can't read, left out"),
        None => {}
    }
    // The service's labels aren't carried over (#2080), unless it shows none.
    let labelled = drawing_info.get("labelingInfo").and_then(Value::as_array).is_some_and(|l| !l.is_empty());
    if labelled && drawing_info.get("showLabels").and_then(Value::as_bool) != Some(false) {
        notes.add("its labels, left out");
    }
    let renderer = match drawing_info.get("renderer") {
        None | Some(Value::Null) => return,
        Some(r) if r.is_object() => r,
        Some(_) => {
            notes.add("its renderer, which RustGIS can't read, drawn with RustGIS's single symbol");
            return;
        }
    };
    let g = layer.geometry_type();
    if g == GeometryType::None {
        return;
    }
    let pictures = &mut Pictures { readable, met: HashMap::new() };
    match renderer.get("type").and_then(Value::as_str) {
        Some("simple") if renderer.get("symbol").is_none_or(Value::is_null) => {
            notes.add("its renderer names no symbol, drawn with RustGIS's single symbol");
        }
        Some("simple") => {
            if let Some((symbol, dash)) = symbol(renderer.get("symbol").unwrap_or(&Value::Null), g, None, pictures, notes) {
                layer.renderer = Renderer::Single(symbol);
                layer.line_strokes = dash.map(strokes).unwrap_or_default();
            }
        }
        Some("uniqueValue") => unique(layer, renderer, g, pictures, notes),
        Some("classBreaks") => class_breaks(layer, renderer, g, pictures, notes),
        _ => notes.add("its renderer, of a kind RustGIS doesn't read, drawn with RustGIS's single symbol"),
    }
    if renderer.get("visualVariables").and_then(Value::as_array).is_some_and(|v| !v.is_empty()) {
        notes.add("sizes, colours or rotations by value (visual variables), left out");
    }
    if renderer.get("rotationExpression").and_then(Value::as_str).is_some_and(|e| !e.trim().is_empty()) {
        notes.add("markers rotated by a field, drawn upright");
    }
}

/// A renderer by unique values (`uniqueValue`) of one field as a Unique Values renderer: a class
/// for each value it lists, with its label, and its default symbol (`defaultSymbol`, with
/// `defaultLabel`) for the rest, which draw nothing without one, as ArcGIS draws them. One by
/// several fields or by an expression leaves the layer's own renderer, noted.
fn unique(layer: &mut Layer, r: &Value, g: GeometryType, pictures: &mut Pictures, notes: &mut Notes) {
    let Some((fi, field)) = class_field(layer, r, "field1", notes) else { return };
    if ["field2", "field3"].iter().any(|k| r.get(k).and_then(Value::as_str).is_some_and(|f| !f.trim().is_empty())) {
        notes.add("its renderer by the values of several fields, drawn with RustGIS's single symbol");
        return;
    }
    let ty = layer.dataset.fields.get(fi).map_or(FieldType::Text, |f| f.ty);
    let infos = r.get("uniqueValueInfos").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    let (ramp, n) = (Ramp::default(), infos.len().min(MAX_CLASSES));
    let mut classes: Vec<(String, Symbol)> = Vec::new();
    let mut seen = BTreeSet::new();
    let (mut class_labels, mut class_strokes) = (BTreeMap::new(), BTreeMap::new());
    for info in infos {
        let Some(value) = info.get("value").and_then(|v| class_value(v, ty)) else {
            notes.add("classes with no value RustGIS can read, left out");
            continue;
        };
        // A value listed again is drawn as its first class, as ArcGIS draws it.
        if seen.contains(&value) {
            notes.add("values listed twice, drawn as their first class");
            continue;
        }
        if classes.len() >= MAX_CLASSES {
            notes.add("classes past the first 4096, drawn as all other values");
            break;
        }
        seen.insert(value.clone());
        let color = ramp.pick(classes.len(), n);
        let (symbol, dash) = class_symbol(info.get("symbol"), g, Some(color), color, pictures, notes);
        if let Some(label) = label(info, "label", notes).filter(|l| *l != value) {
            class_labels.insert(value.clone(), label);
        }
        if let Some(d) = dash {
            class_strokes.insert(value.clone(), strokes(d));
        }
        classes.push((value, symbol));
    }
    if classes.is_empty() {
        notes.add("its renderer by unique values lists none, drawn with RustGIS's single symbol");
        return;
    }
    // Its label only with a symbol to show it by.
    let (other, other_strokes, other_label) = match r.get("defaultSymbol").filter(|s| !s.is_null()) {
        // A picture marker of its own that can't be drawn is grey, as a simple renderer's: there
        // is no ramp to take.
        Some(s) => {
            let (symbol, dash) = class_symbol(Some(s), g, None, OTHER_FILL, pictures, notes);
            (symbol, dash.map(strokes).unwrap_or_default(), label(r, "defaultLabel", notes))
        }
        None => {
            // They draw nothing, so a layer whose values it doesn't list would show nothing
            // unsaid (its legend leaves a hidden <all other values> out, #207).
            let ds = &layer.dataset;
            let printed = |v: &rustgis_data::Value| match v {
                rustgis_data::Value::Text(t) => t.clone(),
                v => v.to_string(),
            };
            if (0..ds.features.len()).any(|row| !seen.contains(&printed(ds.value(row, fi)))) {
                notes.add("features of values it doesn't list, not drawn, as ArcGIS draws them");
            }
            (
                Symbol { fill: Rgba::TRANSPARENT, stroke: Rgba::TRANSPARENT, stroke_width: 0.0, ..Symbol::default_for(g, 0) },
                Vec::new(),
                None,
            )
        }
    };
    layer.renderer = Renderer::Unique {
        field,
        classes,
        other,
        ramp,
        class_labels,
        other_label,
        class_strokes,
        other_strokes,
        class_fills: BTreeMap::new(),
        other_fills: Vec::new(),
        expression: None,
    };
    layer.line_strokes = Vec::new();
}

/// A renderer by class breaks (`classBreaks`) as a Graduated Colors renderer with its bounds, kept
/// as they are (Manual), each class with its label. What RustGIS draws otherwise is noted: values
/// outside every class or none (ArcGIS draws them with its default symbol, or not at all), gaps
/// between classes, a background fill. One by a normalised value or an expression leaves the
/// layer's own renderer, noted.
fn class_breaks(layer: &mut Layer, r: &Value, g: GeometryType, pictures: &mut Pictures, notes: &mut Notes) {
    if r.get("normalizationType").and_then(Value::as_str).is_some_and(|t| t != "esriNormalizeByNone") {
        notes.add("its renderer by class breaks of a normalised value, drawn with RustGIS's single symbol");
        return;
    }
    let Some((fi, field)) = class_field(layer, r, "field", notes) else { return };
    let finite = |v: Option<&Value>| v.and_then(Value::as_f64).filter(|v| v.is_finite());
    let mut infos: Vec<(f64, &Value)> = Vec::new();
    for info in r.get("classBreakInfos").and_then(Value::as_array).into_iter().flatten() {
        let Some(max) = finite(info.get("classMaxValue")) else {
            notes.add("classes with no upper bound RustGIS can read, left out");
            continue;
        };
        if infos.len() >= MAX_CLASSES {
            notes.add("classes past the first 4096, left out");
            break;
        }
        infos.push((max, info));
    }
    let Some(last) = infos.iter().map(|i| i.0).reduce(f64::max) else {
        notes.add("its renderer by class breaks lists none, drawn with RustGIS's single symbol");
        return;
    };
    infos.sort_by(|a, b| a.0.total_cmp(&b.0));
    // A class whose upper bound one before has is never drawn: what it holds is in that one.
    let listed = infos.len();
    infos.dedup_by(|b, a| b.0 == a.0);
    if infos.len() < listed {
        notes.add("classes with the upper bound of one before, left out");
    }
    // A lower bound that is there but isn't a number is read as if it weren't.
    let unread = |v: Option<&Value>| v.is_some_and(|v| !v.is_null()) && finite(v).is_none();
    if unread(r.get("minValue")) || infos.iter().any(|i| unread(i.1.get("classMinValue"))) {
        notes.add("lower bounds RustGIS can't read, left out");
    }
    let ds = &layer.dataset;
    let values = || (0..ds.features.len()).map(|row| ds.value(row, fi).as_f64());
    let lowest = |info: &Value| finite(info.get("classMinValue"));
    let min = finite(r.get("minValue"))
        .or_else(|| infos.first().and_then(|i| lowest(i.1)))
        .or_else(|| values().flatten().filter(|v| v.is_finite()).reduce(f64::min))
        .unwrap_or(last)
        .min(infos.first().map_or(last, |i| i.0));
    // A value between one class's top and the next one's lowest is in none for ArcGIS; here, in
    // the next. Where classes overlap, both draw a value in the first that holds it.
    let tops: Vec<f64> = infos.iter().map(|i| i.0).collect();
    let in_gap = |v: f64| {
        let i = tops.partition_point(|t| *t < v);
        i > 0 && infos.get(i).and_then(|b| lowest(b.1)).is_some_and(|lo| v < lo)
    };
    if values().flatten().any(in_gap) {
        notes.add("values between its classes, drawn in the class above");
    }
    // Below the higher of the renderer's lowest (`minValue`, below which ArcGIS begins no class)
    // and its first class's own: in none for ArcGIS, in the first here.
    let first = infos.first();
    let floor = first.and_then(|i| lowest(i.1)).map_or(min, |lo| lo.max(min).min(first.map_or(last, |i| i.0)));
    if values().flatten().any(|v| v < floor || v > last) {
        notes.add("values below its first class or above its last, drawn in the first or last class");
    }
    if values().any(|v| v.is_none()) {
        notes.add("features with no value, drawn grey");
    }
    if r.get("backgroundFillSymbol").is_some_and(Value::is_object) {
        notes.add("its background fill under the class symbols, left out");
    }
    let (ramp, n) = (Ramp::default(), infos.len());
    let (mut symbols, mut class_labels, mut class_strokes) = (Vec::new(), BTreeMap::new(), BTreeMap::new());
    for (i, (_, info)) in infos.iter().enumerate() {
        let color = ramp.pick(i, n);
        let (symbol, dash) = class_symbol(info.get("symbol"), g, Some(color), color, pictures, notes);
        if let Some(label) = label(info, "label", notes) {
            class_labels.insert(i, label);
        }
        if let Some(d) = dash {
            class_strokes.insert(i, strokes(d));
        }
        symbols.push(symbol);
    }
    layer.renderer = Renderer::Graduated {
        field,
        breaks: infos.iter().map(|i| i.0).collect(),
        symbols,
        ramp,
        method: ClassMethod::Manual,
        min,
        classes: n,
        class_labels,
        class_strokes,
        class_fills: BTreeMap::new(),
    };
    layer.line_strokes = Vec::new();
}

/// The layer's field a renderer classes by (its `key`), its place and its name as the layer has
/// it; `None`, noted, when it classes by an expression (`valueExpression`, in Arcade), names none
/// or one the layer hasn't.
fn class_field(layer: &Layer, r: &Value, key: &str, notes: &mut Notes) -> Option<(usize, String)> {
    if r.get("valueExpression").and_then(Value::as_str).is_some_and(|e| !e.trim().is_empty()) {
        notes.add("its renderer by an expression (Arcade), drawn with RustGIS's single symbol");
        return None;
    }
    let Some(name) = r.get(key).and_then(Value::as_str).filter(|f| !f.trim().is_empty()) else {
        notes.add("its renderer names no field, drawn with RustGIS's single symbol");
        return None;
    };
    let Some(fi) = layer.dataset.field_index(name.trim()) else {
        notes.add("its renderer's field, which the layer hasn't, drawn with RustGIS's single symbol");
        return None;
    };
    Some((fi, layer.dataset.fields.get(fi).map_or_else(|| name.to_string(), |f| f.name.clone())))
}

/// A class's value as the layer's values print where they are matched to it (Unique Values
/// compares them as text): a number in a numeric field as RustGIS prints it (`"1.0"` as `1`);
/// `None` for a value that is neither text nor a number.
fn class_value(v: &Value, ty: FieldType) -> Option<String> {
    let text = match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    if !matches!(ty, FieldType::Integer | FieldType::Double) {
        return Some(text);
    }
    let t = text.trim();
    let printed = t
        .parse::<i64>()
        .map(rustgis_data::Value::Int)
        .ok()
        .or_else(|| t.parse::<f64>().ok().filter(|v| v.is_finite()).map(rustgis_data::Value::Float))
        .map(|v| v.to_string());
    Some(printed.unwrap_or(text))
}

/// A legend label (`key`), when there is one, as one typed by hand is kept (one line, at most
/// [`MAX_LABEL`](crate::symbology::MAX_LABEL) characters); one that changes is noted.
fn label(v: &Value, key: &str, notes: &mut Notes) -> Option<String> {
    let text = v.get(key).and_then(Value::as_str)?;
    let label = normalize_label(text);
    if label != text.trim() {
        notes.add("legend labels of several lines or over 200 characters, joined into one line and cut");
    }
    Some(label).filter(|l| !l.is_empty())
}

/// A class's symbol for a `g` layer and its line's dash pattern: its own; a picture marker whose
/// image can't be drawn as a circle in `picture`, a ramp's colour so that its classes tell apart
/// (grey without one); one RustGIS can't draw, or none, as its own symbol in `color` (noted).
fn class_symbol(
    s: Option<&Value>,
    g: GeometryType,
    picture: Option<Rgba>,
    color: Rgba,
    pictures: &mut Pictures,
    notes: &mut Notes,
) -> (Symbol, Option<Dash>) {
    let own = || {
        let base = Symbol::default_for(g, 0);
        let symbol = match g {
            GeometryType::Polyline => Symbol { fill: color, stroke: color, ..base },
            GeometryType::Polygon => Symbol { fill: color.with_alpha(base.fill.a()), ..base },
            _ => Symbol { fill: color, ..base },
        };
        (symbol, None)
    };
    let Some(s) = s.filter(|s| !s.is_null()) else {
        notes.add("classes with no symbol, drawn in a ramp's colours");
        return own();
    };
    symbol(s, g, picture, pictures, notes).unwrap_or_else(own)
}

/// A dashed line's stacked stroke, which takes its colour and width from the renderer.
fn strokes(dash: Dash) -> Vec<LineStroke> {
    vec![LineStroke { dash: Some(dash), ..LineStroke::default() }]
}

/// An ArcGIS symbol as a RustGIS symbol for a `g` layer, with a line's dash pattern; `None` (noted)
/// for a symbol that isn't for that geometry or that RustGIS can't draw. A picture marker draws
/// its image at its width; one whose image can't be drawn is a circle in `picture`, a ramp's
/// colour, or grey without one.
fn symbol(s: &Value, g: GeometryType, picture: Option<Rgba>, pictures: &mut Pictures, notes: &mut Notes) -> Option<(Symbol, Option<Dash>)> {
    let kind = s.get("type").and_then(Value::as_str).unwrap_or_default();
    let base = Symbol::default_for(g, 0);
    match (kind, g) {
        ("esriSFS", GeometryType::Polygon) => {
            let fill = match style(s, notes).unwrap_or("esriSFSSolid") {
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
            let marker = match style(s, notes) {
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
            if offset_or_angle(s) {
                notes.add("markers' angles and offsets, left out");
            }
            // The fill is the circle drawn should the image not decode after all.
            let fill = picture.unwrap_or(PICTURE_FILL);
            match pictures.of(s) {
                // Sized by its width, as a picture marker is: its height follows the image's own
                // proportion, which a service draws its symbol at. Without a width above 0, its
                // height stands in as its width, said, since an image that isn't square is then
                // drawn that wide, not that tall; with neither, RustGIS's size does.
                Ok(p) => {
                    let wide = s.get("width").and_then(Value::as_f64).is_some_and(|w| w.is_finite() && w > 0.0);
                    if !wide {
                        notes.add(if s.get("height").is_some_and(|h| !h.is_null()) {
                            "picture markers with no width, drawn with their height as their width"
                        } else {
                            "picture markers with no size, drawn at RustGIS's"
                        });
                    }
                    let size = size(s.get(if wide { "width" } else { "height" }), base.size, notes);
                    return Some((Symbol { fill, size, marker: Marker::Picture(p), ..base }, None));
                }
                Err(why) => notes.add(why),
            }
            // As a circle, the larger of its width and height, so that one past the largest is
            // said once.
            let points = |k: &str| s.get(k).and_then(Value::as_f64).filter(|v| v.is_finite()).unwrap_or(f64::NEG_INFINITY);
            let larger = if points("height") > points("width") { s.get("height") } else { s.get("width") };
            let size = size(larger, base.size, notes);
            Some((Symbol { fill, size, ..base }, None))
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
    let style = style(s, notes).unwrap_or("esriSLSSolid");
    if style == "esriSLSNull" {
        return (Rgba::TRANSPARENT, 0.0, None);
    }
    let w = number(s.get("width"), "line widths RustGIS can't read, drawn at RustGIS's", notes).map_or(width, |w| {
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
    number(v, "marker sizes RustGIS can't read, drawn at RustGIS's", notes).map_or(default, |s| {
        if s > f64::from(MAX_MARKER_SIZE) {
            notes.add("marker sizes past RustGIS's largest (80 points), drawn at it");
        } else if s < 0.0 {
            notes.add("marker sizes below 0, drawn at 0");
        }
        (s as f32).clamp(0.0, MAX_MARKER_SIZE)
    })
}

/// A symbol's style (`style`), `None` when it names none; one that isn't text is noted and taken as
/// none (ArcGIS's default style).
fn style<'a>(s: &'a Value, notes: &mut Notes) -> Option<&'a str> {
    let v = s.get("style").filter(|v| !v.is_null())?;
    let style = v.as_str();
    if style.is_none() {
        notes.add("styles RustGIS can't read, drawn as ArcGIS's default");
    }
    style
}

/// A number of a symbol (`v`), `None` when it names none; one that isn't a number is noted with
/// `unread` and taken as none.
fn number(v: Option<&Value>, unread: &'static str, notes: &mut Notes) -> Option<f64> {
    let v = v.filter(|v| !v.is_null())?;
    let n = v.as_f64().filter(|n| n.is_finite());
    if n.is_none() {
        notes.add(unread);
    }
    n
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

    /// The renderer's check stood in for (`map` doesn't decode images): a PNG's base64 begins so.
    fn png(b64: &str) -> Result<(), &'static str> {
        if b64.starts_with("iVBORw0KGgo") { Ok(()) } else { Err("whose image isn't a PNG, JPEG or SVG RustGIS reads") }
    }

    fn styled(g: GeometryType, info: Value) -> (Layer, Notes) {
        let mut l = layer(g);
        let mut notes = Notes::default();
        apply(&mut l, &info, &png, &mut notes);
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
    fn a_picture_marker_draws_its_image_at_its_width() {
        let image = "iVBORw0KGgoSIMPLE=";
        let info = json!({"renderer": {"type": "simple", "symbol": {"type": "esriPMS", "imageData": image, "contentType": "image/png",
            "width": 15, "height": 18}}});
        let (l, notes) = styled(GeometryType::Point, info);
        let Renderer::Single(s) = l.renderer else { panic!("a single symbol") };
        assert_eq!((s.marker, s.fill, s.size), (Marker::Picture(Picture::new(image)), PICTURE_FILL, 15.0));
        assert!(notes.is_empty(), "{}", notes.text());
        // Without a width above 0, its height sizes it.
        for width in [json!(null), json!(0)] {
            let info =
                json!({"renderer": {"type": "simple", "symbol": {"type": "esriPMS", "imageData": image, "width": width, "height": 18}}});
            let (l, notes) = styled(GeometryType::Point, info);
            let Renderer::Single(s) = l.renderer else { panic!("a single symbol") };
            assert_eq!(s.size, 18.0, "width {width}");
            assert_eq!(notes.text(), "picture markers with no width, drawn with their height as their width");
        }
        // With neither, RustGIS's size.
        let info = json!({"renderer": {"type": "simple", "symbol": {"type": "esriPMS", "imageData": image}}});
        let (l, notes) = styled(GeometryType::Point, info);
        let Renderer::Single(s) = l.renderer else { panic!("a single symbol") };
        let Renderer::Single(base) = layer(GeometryType::Point).renderer else { panic!("a single symbol") };
        assert_eq!(s.size, base.size);
        assert_eq!(notes.text(), "picture markers with no size, drawn at RustGIS's");
        // Turned or shifted, it is drawn upright on its point, said.
        let info = json!({"renderer": {"type": "simple", "symbol": {"type": "esriPMS", "imageData": image, "width": 15, "angle": 30}}});
        let (l, notes) = styled(GeometryType::Point, info);
        assert!(matches!(l.renderer, Renderer::Single(Symbol { marker: Marker::Picture(_), .. })));
        assert_eq!(notes.text(), "markers' angles and offsets, left out");
    }

    #[test]
    fn a_picture_marker_rustgis_cant_draw_is_a_grey_circle_of_its_size_and_said_why() {
        let circle = |symbol: Value| {
            let (l, notes) = styled(GeometryType::Point, json!({"renderer": {"type": "simple", "symbol": symbol}}));
            let Renderer::Single(s) = l.renderer else { panic!("a single symbol") };
            assert_eq!((s.marker, s.fill, s.size), (Marker::Circle, PICTURE_FILL, 18.0));
            notes.text()
        };
        let gif = "R0lGODlhAQABAIAAAP///wAAACw=";
        let long = format!("iVBORw0KGgo{}", "A".repeat(MAX_PICTURE_BYTES));
        for (image, why) in [
            (json!(null), "picture markers with no image, drawn as circles"),
            (json!(""), "picture markers with no image, drawn as circles"),
            (json!(12), "picture markers with no image, drawn as circles"),
            (json!(gif), "picture markers whose image isn't a PNG, JPEG or SVG RustGIS reads, drawn as circles"),
            (json!(long), "picture markers past 2 MiB of base64, drawn as circles"),
        ] {
            assert_eq!(circle(json!({"type": "esriPMS", "imageData": image, "width": 15, "height": 18})), why);
        }
        // Its image only at its `url`, which RustGIS doesn't fetch.
        let url = json!({"type": "esriPMS", "url": "4d1f6b3c", "width": 15, "height": 18});
        assert_eq!(circle(url), "picture markers whose image is only at a URL, drawn as circles");
        assert_eq!(Picture::kept(gif), None, "an image that isn't drawn isn't kept");
    }

    #[test]
    fn what_rustgis_doesnt_draw_leaves_the_layers_own_symbol_and_is_said() {
        let own = layer(GeometryType::Polygon).renderer;
        for (info, said) in [
            (
                json!({"renderer": {"type": "uniqueValue", "field1": "NAME", "uniqueValueInfos": []}}),
                "its renderer by unique values lists none, drawn with RustGIS's single symbol",
            ),
            (
                json!({"renderer": {"type": "classBreaks", "field": "NAME", "classBreakInfos": []}}),
                "its renderer by class breaks lists none, drawn with RustGIS's single symbol",
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
        // A picture marker larger than the largest both ways is said once, drawn as its image or not.
        for (image, why) in [(json!("iVBORw0KGgoLARGE="), ""), (json!(null), "picture markers with no image, drawn as circles; ")] {
            let info =
                json!({"renderer": {"type": "simple", "symbol": {"type": "esriPMS", "imageData": image, "width": 100, "height": 120}}});
            let (l, notes) = styled(GeometryType::Point, info);
            let Renderer::Single(s) = l.renderer else { panic!("a single symbol") };
            assert_eq!(s.size, MAX_MARKER_SIZE);
            assert_eq!(notes.text(), format!("{why}marker sizes past RustGIS's largest (80 points), drawn at it"));
        }
        // Not an object: nothing changes.
        for info in [json!(null), json!([]), json!("simple"), json!({"renderer": null})] {
            let (l, notes) = styled(GeometryType::Point, info);
            assert_eq!(l.renderer, layer(GeometryType::Point).renderer);
            assert!(notes.is_empty());
        }
        // A renderer that isn't an object is said.
        for renderer in [json!("simple"), json!([]), json!(3)] {
            let (l, notes) = styled(GeometryType::Point, json!({ "renderer": renderer }));
            assert_eq!(l.renderer, layer(GeometryType::Point).renderer);
            assert_eq!(notes.text(), "its renderer, which RustGIS can't read, drawn with RustGIS's single symbol");
        }
    }

    /// A `g` layer with a text field `NAME` and a numeric `CODE` of type `code`, a feature for each
    /// of `rows`.
    fn with_rows(g: GeometryType, code: FieldType, rows: &[(&str, rustgis_data::Value)]) -> Layer {
        let mut ds = Dataset::new("l", g, vec![Field::new("NAME", FieldType::Text), Field::new("CODE", code)]);
        for (name, code) in rows {
            ds.push(None, vec![rustgis_data::Value::Text((*name).to_string()), code.clone()]);
        }
        Layer::new(1, ds)
    }

    fn styled_rows(mut l: Layer, info: Value) -> (Layer, Notes) {
        let mut notes = Notes::default();
        apply(&mut l, &info, &png, &mut notes);
        (l, notes)
    }

    #[test]
    fn unique_values_of_a_field_take_the_services_classes_and_labels() {
        use rustgis_data::Value as V;
        let fill = |c: [u8; 4]| json!({"type": "esriSFS", "style": "esriSFSSolid", "color": c});
        let info = json!({"renderer": {"type": "uniqueValue", "field1": "name", "uniqueValueInfos": [
            {"value": "BOSA", "label": "Bosa", "symbol": fill([255, 0, 0, 255])},
            {"value": "SUBA", "label": "SUBA", "symbol": fill([0, 0, 255, 255])},
            {"value": "BOSA", "label": "again", "symbol": fill([0, 255, 0, 255])}]}});
        let l = with_rows(GeometryType::Polygon, FieldType::Integer, &[("BOSA", V::Null), ("SUBA", V::Null), ("USME", V::Null)]);
        let (l, notes) = styled_rows(l, info);
        // USME, which it doesn't list, isn't drawn, and that is said.
        assert_eq!(
            notes.text(),
            "values listed twice, drawn as their first class; features of values it doesn't list, not drawn, as ArcGIS draws them"
        );
        let Renderer::Unique { field, classes, class_labels, other, other_label, .. } = &l.renderer else { panic!("{:?}", l.renderer) };
        // The field as the layer names it; a value listed again is drawn as its first class.
        assert_eq!(field, "NAME");
        assert_eq!(classes.iter().map(|c| c.0.as_str()).collect::<Vec<_>>(), ["BOSA", "SUBA"]);
        // A label that is its value isn't kept.
        assert_eq!(class_labels.iter().collect::<Vec<_>>(), [(&"BOSA".to_string(), &"Bosa".to_string())]);
        assert_eq!(l.renderer.symbol_for(&l.dataset, 0).fill, Rgba::rgb(255, 0, 0));
        assert_eq!(l.renderer.symbol_for(&l.dataset, 1).fill, Rgba::rgb(0, 0, 255));
        // Without a default symbol, the rest draw nothing, as ArcGIS draws them.
        assert_eq!((other.fill.a(), other.stroke.a(), other_label.as_deref()), (0, 0, None));
        assert_eq!(l.renderer.symbol_for(&l.dataset, 2), *other);
    }

    #[test]
    fn a_numeric_fields_values_match_as_rustgis_prints_them() {
        use rustgis_data::Value as V;
        let marker = |c: [u8; 4]| json!({"type": "esriSMS", "color": c, "size": 6});
        for code in [FieldType::Integer, FieldType::Double] {
            let value = |v: i64| if code == FieldType::Integer { V::Int(v) } else { V::Float(v as f64) };
            let info = json!({"renderer": {"type": "uniqueValue", "field1": "CODE", "uniqueValueInfos": [
                {"value": "1.0", "symbol": marker([255, 0, 0, 255])}, {"value": 2, "symbol": marker([0, 0, 255, 255])},
                {"value": null, "symbol": marker([0, 255, 0, 255])}]}});
            let (l, notes) = styled_rows(with_rows(GeometryType::Point, code, &[("a", value(1)), ("b", value(2))]), info);
            assert_eq!(l.renderer.symbol_for(&l.dataset, 0).fill, Rgba::rgb(255, 0, 0));
            assert_eq!(l.renderer.symbol_for(&l.dataset, 1).fill, Rgba::rgb(0, 0, 255));
            assert_eq!(notes.text(), "classes with no value RustGIS can read, left out");
        }
        // In a text field, a value stays as it is.
        assert_eq!(class_value(&json!("1.0"), FieldType::Text).as_deref(), Some("1.0"));
    }

    /// Each class draws its own image, each image checked once however many classes show it.
    #[test]
    fn picture_marker_classes_draw_their_own_images() {
        let (school, hospital) = ("iVBORw0KGgoSCHOOL=", "iVBORw0KGgoHOSPITAL=");
        let picture = |image: &str| json!({"type": "esriPMS", "imageData": image, "width": 11, "height": 12});
        let info = json!({"renderer": {"type": "uniqueValue", "field1": "NAME", "uniqueValueInfos": [
            {"value": "1", "label": "Escuela", "symbol": picture(school)}, {"value": "2", "label": "Hospital", "symbol": picture(hospital)},
            {"value": "3", "label": "Otra escuela", "symbol": picture(school)}]}});
        let checked = std::cell::Cell::new(0);
        let counted = |b64: &str| {
            checked.set(checked.get() + 1);
            png(b64)
        };
        let (mut l, mut notes) = (layer(GeometryType::Point), Notes::default());
        apply(&mut l, &info, &counted, &mut notes);
        assert_eq!(checked.get(), 2, "the school's image once");
        let Renderer::Unique { classes, .. } = &l.renderer else { panic!("{:?}", l.renderer) };
        let markers: Vec<Marker> = classes.iter().map(|c| c.1.marker).collect();
        let (school, hospital) = (Marker::Picture(Picture::new(school)), Marker::Picture(Picture::new(hospital)));
        assert_eq!(markers, [school, hospital, school]);
        assert!(classes.iter().all(|c| c.1.size == 11.0));
        assert!(notes.is_empty(), "{}", notes.text());
    }

    #[test]
    fn picture_marker_classes_rustgis_cant_draw_are_circles_in_a_ramps_colours() {
        let picture = json!({"type": "esriPMS", "url": "4d1f6b3c", "width": 11, "height": 12});
        let info = json!({"renderer": {"type": "classBreaks", "field": "CODE", "classBreakInfos": [
            {"classMaxValue": 1, "symbol": picture.clone()}, {"classMaxValue": 2, "symbol": picture}]}});
        let (l, notes) = styled_rows(with_rows(GeometryType::Point, FieldType::Integer, &[]), info);
        let Renderer::Graduated { symbols, .. } = &l.renderer else { panic!("{:?}", l.renderer) };
        let fills: Vec<Rgba> = symbols.iter().map(|s| s.fill).collect();
        assert_eq!(fills, [Ramp::default().pick(0, 2), Ramp::default().pick(1, 2)]);
        assert_ne!(fills.first(), fills.get(1), "the classes tell apart");
        assert!(symbols.iter().all(|s| s.marker == Marker::Circle && s.size == 12.0));
        assert_eq!(notes.text(), "picture markers whose image is only at a URL, drawn as circles (2 symbols)");
    }

    #[test]
    fn line_classes_keep_their_dashes_and_the_default_symbol_draws_the_rest() {
        let line = |style: &str, c: [u8; 4]| json!({"type": "esriSLS", "style": style, "color": c, "width": 2});
        let info = json!({"renderer": {"type": "uniqueValue", "field1": "NAME",
            "uniqueValueInfos": [{"value": "a", "symbol": line("esriSLSDash", [255, 0, 0, 255])}, {"value": "b", "symbol": line("esriSLSSolid", [0, 0, 255, 255])}],
            "defaultSymbol": line("esriSLSDot", [9, 9, 9, 255]), "defaultLabel": "Others"}});
        let (l, notes) = styled(GeometryType::Polyline, info);
        assert!(notes.is_empty(), "{}", notes.text());
        assert!(l.line_strokes.is_empty());
        let Renderer::Unique { class_strokes, other, other_label, other_strokes, .. } = &l.renderer else { panic!("{:?}", l.renderer) };
        assert_eq!(class_strokes.keys().collect::<Vec<_>>(), ["a"]);
        assert_eq!(class_strokes.get("a").and_then(|s| s.first()).and_then(|s| s.dash).map(|d| d.steps().to_vec()), Some(vec![8.0, 6.0]));
        assert_eq!((other.stroke, other_label.as_deref()), (Rgba::rgb(9, 9, 9), Some("Others")));
        assert_eq!(other_strokes.first().and_then(|s| s.dash).map(|d| d.steps().to_vec()), Some(vec![2.0, 6.0]));
    }

    #[test]
    fn a_class_rustgis_cant_draw_takes_a_ramps_colour() {
        let info = json!({"renderer": {"type": "uniqueValue", "field1": "NAME", "uniqueValueInfos": [
            {"value": "a", "symbol": {"type": "CIMSymbolReference"}}, {"value": "b", "symbol": {"type": "esriSMS"}}]}});
        let (l, notes) = styled(GeometryType::Polygon, info);
        let Renderer::Unique { classes, .. } = &l.renderer else { panic!("{:?}", l.renderer) };
        let base = Symbol::default_for(GeometryType::Polygon, 0);
        assert_eq!(classes.first().map(|c| c.1.fill), Some(Ramp::default().pick(0, 2).with_alpha(base.fill.a())));
        assert_eq!(
            notes.text(),
            "a symbol of a kind RustGIS doesn't read, drawn with RustGIS's own; a symbol for another kind of geometry, drawn with RustGIS's own"
        );
    }

    #[test]
    fn unique_values_rustgis_doesnt_class_by_leave_the_layers_own_and_are_said() {
        let own = layer(GeometryType::Polygon).renderer;
        let infos = json!([{"value": "a", "symbol": {"type": "esriSFS", "color": [1, 2, 3, 255]}}]);
        for (renderer, said) in [
            (
                json!({"type": "uniqueValue", "field1": "NAME", "field2": "KIND", "fieldDelimiter": ",", "uniqueValueInfos": infos.clone()}),
                "its renderer by the values of several fields, drawn with RustGIS's single symbol",
            ),
            (
                json!({"type": "uniqueValue", "field1": "NAME", "field2": "", "field3": "KIND", "uniqueValueInfos": infos.clone()}),
                "its renderer by the values of several fields, drawn with RustGIS's single symbol",
            ),
            (
                json!({"type": "uniqueValue", "field1": "NAME", "valueExpression": "$feature.NAME", "uniqueValueInfos": infos.clone()}),
                "its renderer by an expression (Arcade), drawn with RustGIS's single symbol",
            ),
            (
                json!({"type": "uniqueValue", "uniqueValueInfos": infos.clone()}),
                "its renderer names no field, drawn with RustGIS's single symbol",
            ),
            (
                json!({"type": "uniqueValue", "field1": "KIND", "uniqueValueInfos": infos.clone()}),
                "its renderer's field, which the layer hasn't, drawn with RustGIS's single symbol",
            ),
            (
                json!({"type": "classBreaks", "field": "NAME", "normalizationType": "esriNormalizeByField", "normalizationField": "AREA"}),
                "its renderer by class breaks of a normalised value, drawn with RustGIS's single symbol",
            ),
        ] {
            let (l, notes) = styled(GeometryType::Polygon, json!({ "renderer": renderer }));
            assert_eq!(l.renderer, own);
            assert_eq!(notes.text(), said);
        }
    }

    #[test]
    fn class_breaks_take_the_services_bounds_labels_and_symbols() {
        use rustgis_data::Value as V;
        let fill = |c: [u8; 4]| json!({"type": "esriSFS", "style": "esriSFSSolid", "color": c});
        let info = json!({"renderer": {"type": "classBreaks", "field": "CODE", "minValue": 5000, "classBreakInfos": [
            {"classMaxValue": 1_500_000, "label": "800,000 – 1,500,000", "symbol": fill([0, 0, 255, 255])},
            {"classMaxValue": 800_000, "symbol": fill([255, 0, 0, 255])}]}});
        let rows = [("a", V::Float(6000.0)), ("b", V::Float(900_000.0))];
        let (l, notes) = styled_rows(with_rows(GeometryType::Polygon, FieldType::Double, &rows), info);
        assert!(notes.is_empty(), "{}", notes.text());
        let Renderer::Graduated { field, breaks, min, method, classes, class_labels, .. } = &l.renderer else { panic!("{:?}", l.renderer) };
        // Sorted by their upper bounds, kept as they are.
        assert_eq!(
            (field.as_str(), breaks.as_slice(), *min, *method, *classes),
            ("CODE", [800_000.0, 1_500_000.0].as_slice(), 5000.0, ClassMethod::Manual, 2)
        );
        assert_eq!(class_labels.iter().collect::<Vec<_>>(), [(&1, &"800,000 – 1,500,000".to_string())]);
        assert_eq!(l.renderer.symbol_for(&l.dataset, 0).fill, Rgba::rgb(255, 0, 0));
        assert_eq!(l.renderer.symbol_for(&l.dataset, 1).fill, Rgba::rgb(0, 0, 255));
    }

    #[test]
    fn what_class_breaks_draw_otherwise_is_said_only_when_it_happens() {
        use rustgis_data::Value as V;
        let fill = json!({"type": "esriSFS", "color": [1, 2, 3, 255]});
        let info = json!({"renderer": {"type": "classBreaks", "field": "CODE", "minValue": 10,
            "classBreakInfos": [{"classMinValue": 10, "classMaxValue": 20, "symbol": fill}, {"classMinValue": 25, "classMaxValue": 30, "symbol": fill},
                {"classMaxValue": "x", "symbol": fill}, {"classMaxValue": 20, "symbol": fill}],
            "backgroundFillSymbol": {"type": "esriSFS", "color": [0, 0, 0, 255]}}});
        let rows = [("a", V::Int(5)), ("b", V::Null), ("c", V::Int(40)), ("d", V::Int(22))];
        let (l, notes) = styled_rows(with_rows(GeometryType::Polygon, FieldType::Integer, &rows), info.clone());
        let Renderer::Graduated { breaks, .. } = &l.renderer else { panic!("{:?}", l.renderer) };
        assert_eq!(breaks.as_slice(), [20.0, 30.0], "each bound once");
        assert_eq!(
            notes.text(),
            "classes with no upper bound RustGIS can read, left out; classes with the upper bound of one before, left out; \
             values between its classes, drawn in the class above; \
             values below its first class or above its last, drawn in the first or last class; features with no value, drawn grey; \
             its background fill under the class symbols, left out"
        );
        // Every value within the classes, none between them: nothing is said.
        let info = json!({"renderer": {"type": "classBreaks", "field": "CODE",
            "classBreakInfos": [{"classMinValue": 10, "classMaxValue": 20, "symbol": fill}, {"classMinValue": 25, "classMaxValue": 30, "symbol": fill}]}});
        let (l, notes) = styled_rows(with_rows(GeometryType::Polygon, FieldType::Integer, &[("a", V::Int(10)), ("b", V::Int(30))]), info);
        assert!(notes.is_empty(), "{}", notes.text());
        let Renderer::Graduated { min, .. } = l.renderer else { panic!() };
        assert_eq!(min, 10.0, "from the first class without a minValue");
    }

    #[test]
    fn values_of_the_wrong_type_are_said_and_taken_as_none() {
        let info = json!({"renderer": {"type": "simple", "symbol": {"type": "esriSMS", "style": 3, "color": [0, 0, 0, 255], "size": "big",
            "outline": {"color": [0, 0, 0, 255], "width": "w"}}}, "transparency": "half"});
        let (l, notes) = styled(GeometryType::Point, info);
        let Renderer::Single(s) = l.renderer else { panic!("a single symbol") };
        let base = Symbol::default_for(GeometryType::Point, 0);
        assert_eq!((s.marker, s.size, s.stroke_width, l.transparency), (Marker::Circle, base.size, 0.0, 0.0));
        assert_eq!(
            notes.text(),
            "a transparency RustGIS can't read, left out; styles RustGIS can't read, drawn as ArcGIS's default; \
             line widths RustGIS can't read, drawn at RustGIS's; marker sizes RustGIS can't read, drawn at RustGIS's"
        );
    }

    #[test]
    fn the_services_labels_are_kept_as_one_typed_by_hand() {
        use crate::symbology::MAX_LABEL;
        let fill = json!({"type": "esriSFS", "color": [1, 2, 3, 255]});
        let info = json!({"renderer": {"type": "uniqueValue", "field1": "NAME", "uniqueValueInfos": [
            {"value": "a", "label": "two\nlines ", "symbol": fill}, {"value": "b", "label": "x".repeat(MAX_LABEL + 50), "symbol": fill}],
            "defaultSymbol": fill, "defaultLabel": "  \t "}});
        let (l, notes) = styled(GeometryType::Polygon, info);
        let Renderer::Unique { class_labels, other_label, .. } = &l.renderer else { panic!("{:?}", l.renderer) };
        // Said once for both; a blank one isn't.
        assert_eq!(notes.text(), "legend labels of several lines or over 200 characters, joined into one line and cut (2 symbols)");
        assert_eq!(class_labels.get("a").map(String::as_str), Some("two lines"));
        assert_eq!(class_labels.get("b").map(|l| l.chars().count()), Some(MAX_LABEL));
        assert_eq!(other_label, &None, "a blank label is none");
    }

    /// What a first class starting above the renderer's `minValue`, a lower bound that isn't a
    /// number and a class with no symbol leave out is said; a default label with no default symbol
    /// to show it by isn't kept (#2152).
    #[test]
    fn lower_bounds_and_missing_symbols_are_said() {
        use rustgis_data::Value as V;
        let fill = json!({"type": "esriSFS", "color": [1, 2, 3, 255]});
        let breaks = |min: Value, first: Value| {
            json!({"renderer": {"type": "classBreaks", "field": "CODE", "minValue": min, "classBreakInfos": [
                {"classMinValue": first, "classMaxValue": 20, "symbol": fill},
                {"classMinValue": 20, "classMaxValue": 30, "symbol": fill}]}})
        };
        let rows = |v: i64| with_rows(GeometryType::Polygon, FieldType::Integer, &[("a", V::Int(v)), ("b", V::Int(30))]);
        // Below the first class's own lower bound, though above the renderer's: said.
        let (_, notes) = styled_rows(rows(5), breaks(json!(0), json!(10)));
        assert_eq!(notes.text(), "values below its first class or above its last, drawn in the first or last class");
        let (_, notes) = styled_rows(rows(10), breaks(json!(0), json!(10)));
        assert!(notes.is_empty(), "{}", notes.text());
        // A lower bound that isn't a number is said; one absent or null isn't.
        for (min, first) in [(json!("low"), json!(10)), (json!(0), json!({"v": 1}))] {
            let (_, notes) = styled_rows(rows(10), breaks(min, first));
            assert_eq!(notes.text(), "lower bounds RustGIS can't read, left out");
        }
        let (_, notes) = styled_rows(rows(10), breaks(json!(null), json!(10)));
        assert!(notes.is_empty(), "{}", notes.text());
        // A class with no symbol takes a ramp's colour, said as such.
        let info = json!({"renderer": {"type": "uniqueValue", "field1": "NAME",
            "uniqueValueInfos": [{"value": "a"}, {"value": "b", "symbol": null}], "defaultLabel": "x".repeat(300)}});
        let (l, notes) = styled(GeometryType::Polygon, info);
        let Renderer::Unique { classes, other_label, .. } = &l.renderer else { panic!("{:?}", l.renderer) };
        let base = Symbol::default_for(GeometryType::Polygon, 0);
        assert_eq!(classes.first().map(|c| c.1.fill), Some(Ramp::default().pick(0, 2).with_alpha(base.fill.a())));
        // The default label, with no default symbol to show it by, is neither kept nor said cut.
        assert_eq!(other_label, &None);
        assert_eq!(notes.text(), "classes with no symbol, drawn in a ramp's colours (2 symbols)");
    }

    #[test]
    fn a_default_picture_marker_is_grey_as_a_simple_ones() {
        let picture = json!({"type": "esriPMS", "width": 10, "height": 10});
        let info = json!({"renderer": {"type": "uniqueValue", "field1": "NAME",
            "uniqueValueInfos": [{"value": "a", "symbol": {"type": "esriSMS", "color": [1, 2, 3, 255]}}], "defaultSymbol": picture}});
        let (l, notes) = styled(GeometryType::Point, info);
        let Renderer::Unique { other, .. } = &l.renderer else { panic!("{:?}", l.renderer) };
        assert_eq!((other.marker, other.fill), (Marker::Circle, PICTURE_FILL));
        assert_eq!(notes.text(), "picture markers with no image, drawn as circles");
        // With its image, it draws it.
        let picture = json!({"type": "esriPMS", "imageData": "iVBORw0KGgoOTHER=", "width": 10});
        let info = json!({"renderer": {"type": "uniqueValue", "field1": "NAME",
            "uniqueValueInfos": [{"value": "a", "symbol": {"type": "esriSMS", "color": [1, 2, 3, 255]}}], "defaultSymbol": picture}});
        let (l, notes) = styled(GeometryType::Point, info);
        let Renderer::Unique { other, .. } = &l.renderer else { panic!("{:?}", l.renderer) };
        assert_eq!(other.marker, Marker::Picture(Picture::new("iVBORw0KGgoOTHER=")));
        assert!(notes.is_empty(), "{}", notes.text());
    }

    #[test]
    fn classes_past_the_most_are_left_out_and_said() {
        let fill = json!({"type": "esriSFS", "color": [1, 2, 3, 255]});
        let infos: Vec<Value> = (0..=MAX_CLASSES).map(|i| json!({"value": format!("v{i}"), "symbol": fill})).collect();
        let (l, notes) =
            styled(GeometryType::Polygon, json!({"renderer": {"type": "uniqueValue", "field1": "NAME", "uniqueValueInfos": infos}}));
        let Renderer::Unique { classes, .. } = &l.renderer else { panic!("{:?}", l.renderer) };
        assert_eq!((classes.len(), notes.text().as_str()), (MAX_CLASSES, "classes past the first 4096, drawn as all other values"));
        let infos: Vec<Value> = (0..=MAX_CLASSES).map(|i| json!({"classMaxValue": i, "symbol": fill})).collect();
        let (l, notes) =
            styled(GeometryType::Polygon, json!({"renderer": {"type": "classBreaks", "field": "NAME", "classBreakInfos": infos}}));
        let Renderer::Graduated { breaks, .. } = &l.renderer else { panic!("{:?}", l.renderer) };
        assert_eq!((breaks.len(), notes.text().as_str()), (MAX_CLASSES, "classes past the first 4096, left out"));
    }
}
