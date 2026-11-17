//! A layer's symbol set per feature by expressions ([`crate::Layer::symbol_by`]): its size,
//! fill and outline colour, outline width and marker angle, each from an expression in the
//! language of [`rustgis_data::query`] (`"pop" / 1e6 + 4`, `CASE WHEN … END`,
//! `scale_linear(…)`). The text is stored; it is compiled against the layer's fields once per
//! drawing ([`Layer::styler`]), so a project never holds a parsed tree.

use crate::fills::FillLayer;
use crate::symbology::{LineStroke, Symbol};
use crate::{Layer, Rgba};
use rustgis_data::query::{self, Env, Expr};
use rustgis_data::{Dataset, Value};
use serde::{Deserialize, Serialize};

/// The largest marker size an expression gives, in points.
pub const MAX_SIZE: f32 = 400.0;
/// The widest outline an expression gives, in points.
pub const MAX_WIDTH: f32 = 100.0;

/// Expressions setting a layer's symbol per feature; none set: the renderer's symbol alone.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SymbolBy {
    /// The marker's size in points.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "de_text")]
    pub size: Option<String>,
    /// The fill colour (a point's marker, a polygon's inside): `'#rrggbb'`, a CSS colour or
    /// `'r,g,b[,a]'` as `color_rgb` gives it.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "de_text")]
    pub fill: Option<String>,
    /// The outline colour, a line's colour on a line layer.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "de_text")]
    pub stroke: Option<String>,
    /// The outline's width in points, a line's width on a line layer.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "de_text")]
    pub stroke_width: Option<String>,
    /// A point's marker turned by this angle in degrees clockwise, in place of
    /// [`Layer::marker_rotation`]'s (where it gives none, that one's).
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "de_text")]
    pub angle: Option<String>,
}

impl SymbolBy {
    pub fn is_empty(&self) -> bool {
        self.size.is_none() && self.fill.is_none() && self.stroke.is_none() && self.stroke_width.is_none() && self.angle.is_none()
    }
}

/// How [`SizeBy`] grows a marker with its value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SizeScaling {
    /// The marker's area grows with the value (its size with the square root), as Proportional
    /// Symbols size theirs: a value twice another looks about twice as big.
    #[default]
    Area,
    /// The marker's size (its radius) grows in a straight line with the value, as MapLibre's
    /// `interpolate` sizes a circle.
    Radius,
}

impl SizeScaling {
    /// Its name in a command's parameters and result: `"area"` or `"radius"`.
    pub fn code(self) -> &'static str {
        match self {
            SizeScaling::Area => "area",
            SizeScaling::Radius => "radius",
        }
    }
    /// The scaling a command names, any case; `"linear"` is `Radius`.
    pub fn from_code(s: &str) -> Option<SizeScaling> {
        match s.trim().to_ascii_lowercase().as_str() {
            "area" => Some(SizeScaling::Area),
            "radius" | "linear" => Some(SizeScaling::Radius),
            _ => None,
        }
    }
}

/// A point layer's marker size from a numeric field, alongside the colours of its Unique Values or
/// Graduated Colors classes ([`crate::Layer::size_by`]): each feature keeps its class's symbol,
/// sized by its value. Values from `min` to `max` take sizes from `min_size` to `max_size`
/// ([`SizeBy::size`]); the field's values are only read, never changed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SizeBy {
    pub field: String,
    /// The value drawn at `min_size`; a smaller one is drawn at `min_size` too.
    pub min: f64,
    /// The value drawn at `max_size`; a larger one is drawn at `max_size` too.
    pub max: f64,
    /// The smallest marker size, in points.
    pub min_size: f32,
    /// The largest marker size, in points.
    pub max_size: f32,
    #[serde(default)]
    pub scaling: SizeScaling,
}

impl SizeBy {
    /// The marker size for a value: clamped to the range's ends, and with no range (`max` not
    /// above `min`) the size halfway, as Proportional Symbols draw it.
    pub fn size(&self, v: f64) -> f32 {
        scaled_size(v, self.min, self.max, self.min_size, self.max_size, self.scaling).clamp(0.0, MAX_SIZE)
    }

    /// Whether it applies to `l`: a point layer whose colours come from Unique Values or Graduated
    /// Colors classes. Otherwise it is kept, unused, for when it does again.
    pub fn applies_to(l: &Layer) -> bool {
        !l.graphics
            && l.geometry_type() == rustgis_data::GeometryType::Point
            && matches!(l.renderer, crate::Renderer::Unique { .. } | crate::Renderer::Graduated { .. })
    }

    /// Legend rows for the sizes: the smallest, middle and largest value, each drawn with `like`
    /// and its mark stack at the mapped size and labelled with the field. Marks of a `like` sized 0
    /// have nothing to scale against and stay as set.
    pub fn legend(&self, like: Symbol, strokes: &[LineStroke]) -> Vec<crate::symbology::LegendEntry> {
        let like = Symbol { fill: crate::symbology::OTHER_FILL, ..like };
        let (lo, hi) = if self.max > self.min { (self.min, self.max) } else { (self.min, self.min) };
        let mut values = vec![lo, lo / 2.0 + hi / 2.0, hi];
        values.dedup();
        values
            .into_iter()
            .map(|v| {
                let size = self.size(v);
                let k = if size == 0.0 {
                    0.0
                } else if like.size.is_finite() && like.size > 0.0 {
                    size / like.size
                } else {
                    1.0
                };
                crate::symbology::LegendEntry {
                    label: format!("{} {}", self.field, crate::symbology::fmt_num(v)),
                    symbol: hide_zero(Symbol { size, ..like }),
                    strokes: scaled_marks(strokes, k),
                    fallback: false,
                }
            })
            .collect()
    }
}

/// A point sized zero has no ink, even in render paths that clamp its geometry to one pixel.
fn hide_zero(sym: Symbol) -> Symbol {
    if sym.size != 0.0 {
        return sym;
    }
    // An image ignores the colours, so it gives way to a circle they hide.
    let marker = if matches!(sym.marker, crate::Marker::Picture(_)) { crate::Marker::Circle } else { sym.marker };
    Symbol { fill: Rgba([0, 0, 0, 0]), stroke: Rgba([0, 0, 0, 0]), stroke_width: 0.0, marker, ..sym }
}

/// Scales point marks and their offsets together, for both map drawing and size legends.
fn scaled_marks(strokes: &[LineStroke], k: f32) -> Vec<LineStroke> {
    strokes
        .iter()
        .map(|st| {
            let mut st = *st;
            st.offset *= k;
            if let Some(m) = &mut st.markers {
                m.size *= k;
                m.offset_along *= k;
            }
            if k == 0.0 {
                st.color = Some(Rgba([0, 0, 0, 0]));
                st.width = Some(0.0);
            }
            st.clamped()
        })
        .collect()
}

/// A size for `v` between `min_size` and `max_size` as it falls between `min` and `max`, by
/// `scaling`; values past an end take that end's size, and with no range (`max` not above `min`,
/// or one too wide to divide) the size halfway.
pub fn scaled_size(v: f64, min: f64, max: f64, min_size: f32, max_size: f32, scaling: SizeScaling) -> f32 {
    let width = max - min;
    let t = if max > min && width.is_finite() { (v - min) / width } else { 0.5 };
    let t = if t.is_nan() { 0.5 } else { t.clamp(0.0, 1.0) };
    let k = match scaling {
        SizeScaling::Area => t.sqrt(),
        SizeScaling::Radius => t,
    };
    min_size + (max_size - min_size) * k as f32
}

/// Reads an optional size mapping, refusing malformed settings instead of discarding them:
/// an unknown scaling mode may come from a newer writer and must not be lost on save.
pub fn de_size_by<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<SizeBy>, D::Error> {
    let by = Option::<SizeBy>::deserialize(d)?;
    if by.as_ref().is_some_and(|s| {
        s.field.is_empty()
            || !s.min.is_finite()
            || !s.max.is_finite()
            || !(s.max > s.min && (s.max - s.min).is_finite())
            || !(0.0..=MAX_SIZE).contains(&s.min_size)
            || !(0.0..=MAX_SIZE).contains(&s.max_size)
    }) {
        return Err(serde::de::Error::custom("invalid size_by field, value bounds or marker sizes"));
    }
    Ok(by)
}

/// Reads one expression leniently: text up to [`query::MAX_LEN`], blank or anything else none.
fn de_text<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(match serde_json::Value::deserialize(d)? {
        serde_json::Value::String(s) if !s.trim().is_empty() && s.chars().count() <= query::MAX_LEN => Some(s),
        _ => None,
    })
}

/// Reads a layer's expressions leniently: a crafted file's that aren't are none, and the rest of
/// the layer still reads.
pub fn de_symbol_by<'de, D: serde::Deserializer<'de>>(d: D) -> Result<SymbolBy, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(v).unwrap_or_default())
}

/// A layer's expressions compiled against its fields for one drawing.
#[derive(Clone, Debug)]
struct Compiled {
    size: Option<Expr>,
    fill: Option<Expr>,
    stroke: Option<Expr>,
    stroke_width: Option<Expr>,
    angle: Option<Expr>,
    env: Env,
    /// A point layer's: a marker sized 0 or less draws nothing.
    points: bool,
    /// The last text each colour expression gave and the colour it read as: most layers colour
    /// many features alike, and the text needn't be read again for each.
    seen: [std::cell::RefCell<Option<(String, Option<Rgba>)>>; 2],
}

impl Compiled {
    /// None where nothing parses against the dataset (an expression naming a field since
    /// removed draws as if it weren't set).
    fn new(by: &SymbolBy, ds: &Dataset, env: Env, points: bool) -> Option<Compiled> {
        let one = |t: &Option<String>| t.as_deref().and_then(|t| query::parse_fields(t, ds).ok());
        let c = Compiled {
            size: one(&by.size),
            fill: one(&by.fill),
            stroke: one(&by.stroke),
            stroke_width: one(&by.stroke_width),
            angle: one(&by.angle),
            env,
            points,
            seen: Default::default(),
        };
        let any = c.size.is_some() || c.fill.is_some() || c.stroke.is_some() || c.stroke_width.is_some() || c.angle.is_some();
        any.then_some(c)
    }

    fn number(&self, e: &Option<Expr>, ds: &Dataset, row: usize) -> Option<f64> {
        query::value(e.as_ref()?, ds, row, &self.env).as_f64().filter(|v| v.is_finite())
    }

    fn color(&self, e: &Option<Expr>, slot: usize, ds: &Dataset, row: usize) -> Option<Rgba> {
        let Value::Text(t) = query::value(e.as_ref()?, ds, row, &self.env) else { return None };
        let Some(seen) = self.seen.get(slot) else { return rustgis_data::color::parse_value(&t).map(Rgba) };
        let Ok(mut seen) = seen.try_borrow_mut() else { return rustgis_data::color::parse_value(&t).map(Rgba) };
        match &*seen {
            Some((had, c)) if *had == t => *c,
            _ => {
                let c = rustgis_data::color::parse_value(&t).map(Rgba);
                *seen = Some((t, c));
                c
            }
        }
    }

    /// The symbol with what the expressions give for the row; a value that is null or isn't a
    /// number (a colour) keeps the symbol's own.
    fn apply(&self, ds: &Dataset, row: usize, mut sym: Symbol) -> Symbol {
        if let Some(v) = self.number(&self.size, ds, row) {
            sym.size = (v as f32).clamp(0.0, MAX_SIZE);
            // As QGIS draws a marker of no size: not even its outline.
            if self.points && sym.size == 0.0 {
                let none = Rgba([0, 0, 0, 0]);
                return Symbol { fill: none, stroke: none, stroke_width: 0.0, ..sym };
            }
        }
        if let Some(v) = self.number(&self.stroke_width, ds, row) {
            sym.stroke_width = (v as f32).clamp(0.0, MAX_WIDTH);
        }
        if let Some(c) = self.color(&self.fill, 0, ds, row) {
            sym.fill = c;
        }
        if let Some(c) = self.color(&self.stroke, 1, ds, row) {
            sym.stroke = c;
        }
        sym
    }

    /// The row's angle in degrees clockwise, none where the expression gives no number.
    fn turn(&self, ds: &Dataset, row: usize) -> Option<f32> {
        self.number(&self.angle, ds, row).map(|v| v.rem_euclid(360.0) as f32)
    }
}

/// A layer's per-row style for one drawing: [`Layer::symbol`], [`Layer::style`],
/// [`Layer::style_and_fills`] and [`Layer::marker_turn`] with its [`Layer::symbol_by`]
/// expressions applied (compiled once, at [`Layer::styler`]).
#[derive(Clone, Debug)]
pub struct Styler<'a> {
    layer: &'a Layer,
    by: Option<Compiled>,
    /// Its renderer's class expression compiled (Unique Values by expression).
    classer: Option<crate::Classer>,
    /// Its size mapping where it applies ([`SizeBy::applies_to`]), with its field's index.
    sized: Option<(usize, &'a SizeBy)>,
}

impl<'a> Styler<'a> {
    pub(crate) fn new(layer: &'a Layer, map_scale: Option<f64>) -> Styler<'a> {
        let by = if layer.graphics || layer.symbol_by.is_empty() {
            None
        } else {
            let env = Env { map_scale: map_scale.filter(|s| s.is_finite() && *s > 0.0) };
            Compiled::new(&layer.symbol_by, &layer.dataset, env, layer.geometry_type() == rustgis_data::GeometryType::Point)
        };
        let classer = (!layer.graphics && layer.renderer.expression().is_some()).then(|| layer.renderer.classer(&layer.dataset, map_scale));
        let sized =
            layer.size_by.as_ref().filter(|_| SizeBy::applies_to(layer)).and_then(|s| Some((layer.dataset.field_index(&s.field)?, s)));
        Styler { layer, by, classer, sized }
    }

    /// Whether no expression or size mapping applies: every row draws as the layer's own methods
    /// give it.
    pub fn is_plain(&self) -> bool {
        self.by.is_none() && self.classer.is_none() && self.sized.is_none()
    }

    /// The row's size from the size mapping: none where it doesn't apply, or where the row's
    /// value is null or not a finite number (the class's own size is drawn then).
    fn mapped_size(&self, row: usize) -> Option<f32> {
        let (fi, s) = self.sized?;
        self.layer.dataset.value(row, fi).as_f64().filter(|v| v.is_finite()).map(|v| s.size(v))
    }

    /// The row's symbol, strokes and fills from its renderer, classed by its expression when it
    /// has one.
    fn base(&self, row: usize) -> (Symbol, &'a [LineStroke], &'a [FillLayer]) {
        match &self.classer {
            Some(c) => self.layer.style_and_fills_with(row, c),
            None => self.layer.style_and_fills(row),
        }
    }

    fn base_symbol(&self, row: usize) -> Symbol {
        match &self.classer {
            Some(c) => self.layer.style_and_fills_with(row, c).0,
            None => self.layer.symbol(row),
        }
    }

    /// The class's symbol sized by the size mapping, then with the expressions' values.
    fn apply(&self, row: usize, mut sym: Symbol) -> Symbol {
        if let Some(size) = self.mapped_size(row) {
            sym.size = size;
        }
        let sym = match &self.by {
            Some(b) => b.apply(&self.layer.dataset, row, sym),
            None => sym,
        };
        if self.sized.is_some() { hide_zero(sym) } else { sym }
    }

    pub fn symbol(&self, row: usize) -> Symbol {
        self.apply(row, self.base_symbol(row))
    }

    pub fn style(&self, row: usize) -> (Symbol, &'a [LineStroke]) {
        let (sym, strokes) = match &self.classer {
            Some(_) => {
                let (sym, strokes, _) = self.base(row);
                (sym, strokes)
            }
            None => self.layer.style(row),
        };
        (self.apply(row, sym), strokes)
    }

    pub fn style_and_fills(&self, row: usize) -> (Symbol, &'a [LineStroke], &'a [FillLayer]) {
        let (sym, strokes, fills) = self.base(row);
        (self.apply(row, sym), strokes, fills)
    }

    /// A point's marks (`strokes`, as [`Styler::style`] gives them) scaled with the size its
    /// expression (else its size mapping) gives, against the symbol's own, as QGIS scales a marker
    /// of several layers: their sizes and offsets. None at size 0. The marks as they are without
    /// either, or when the symbol's own size is 0 and there is nothing to scale against.
    pub fn marks(&self, row: usize, strokes: &'a [LineStroke]) -> std::borrow::Cow<'a, [LineStroke]> {
        use std::borrow::Cow;
        if strokes.is_empty() {
            return Cow::Borrowed(strokes);
        }
        // The expression's size, else the size mapping's.
        let by = self.by.as_ref().filter(|b| b.points).and_then(|b| b.number(&b.size, &self.layer.dataset, row));
        let Some(size) = by.map(|v| (v as f32).clamp(0.0, MAX_SIZE)).or_else(|| self.mapped_size(row)) else {
            // A row the mapping leaves at its class's size: a class of size 0 is hidden, marks too.
            if self.sized.is_some() && self.base_symbol(row).size == 0.0 {
                return Cow::Owned(Vec::new());
            }
            return Cow::Borrowed(strokes);
        };
        if size == 0.0 {
            return Cow::Owned(Vec::new());
        }
        // The size the marks are drawn for: a Proportional renderer's base symbol's (it sizes the
        // symbol per feature, not the marks), else the feature's class's.
        let base = match &self.layer.renderer {
            crate::Renderer::Proportional { .. } => self.layer.renderer.base_symbol().size,
            _ => self.base_symbol(row).size,
        };
        if base.is_nan() || base <= 0.0 {
            return Cow::Borrowed(strokes);
        }
        Cow::Owned(scaled_marks(strokes, size / base))
    }

    pub fn marker_turn(&self, row: usize) -> f32 {
        match self.by.as_ref().and_then(|b| b.turn(&self.layer.dataset, row)) {
            Some(a) => a,
            None => self.layer.marker_turn(row),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symbology::MarkerRotation;
    use rustgis_data::{Field, FieldType, Geometry, GeometryType};
    use rustgis_geom::Coord;

    fn points() -> Layer {
        let fields = vec![Field::new("pop", FieldType::Double), Field::new("kind", FieldType::Text), Field::new("deg", FieldType::Double)];
        let mut l = Layer::new(1, Dataset::new("p", GeometryType::Point, fields));
        for (pop, kind, deg) in [(Value::Float(2e6), "a", 90.0), (Value::Null, "b", 450.0), (Value::Float(5e5), "c", -30.0)] {
            l.dataset.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![pop, Value::Text(kind.into()), Value::Float(deg)]);
        }
        l
    }

    #[test]
    fn a_drawing_classes_by_its_renderers_expression_at_its_scale() {
        let mut l = points();
        let base = l.symbol(0);
        let e = "CASE WHEN @map_scale > 10000 THEN 'far' WHEN kind = 'a' THEN 'a' END";
        l.dataset.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![Value::Null, Value::Text("a".into()), Value::Null]);
        l.renderer = crate::Renderer::unique_by(&l.dataset, &[], "kind || 'x'", crate::Ramp::Harbour, base).unwrap();
        let crate::Renderer::Unique { expression, classes, other, class_strokes, .. } = &mut l.renderer else { panic!() };
        *expression = Some(e.into());
        *classes = vec![("a".into(), Symbol { size: 11.0, ..base }), ("far".into(), Symbol { size: 22.0, ..base })];
        *other = Symbol { size: 3.0, ..base };
        class_strokes.insert("a".into(), vec![LineStroke::default()]);
        let st = l.styler(Some(4000.0));
        assert!(!st.is_plain());
        let sizes = |st: &Styler| (0..4).map(|r| st.symbol(r).size).collect::<Vec<_>>();
        assert_eq!(sizes(&st), [11.0, 3.0, 3.0, 11.0]);
        assert_eq!(sizes(&l.styler(Some(40_000.0))), [22.0; 4]);
        // Without a map, @map_scale is null; one row at a time, too.
        assert_eq!(sizes(&l.styler(None)), [11.0, 3.0, 3.0, 11.0]);
        assert_eq!(l.symbol(0).size, 11.0);
        // The class's own strokes come with it.
        assert_eq!((st.style(0).1.len(), st.style(1).1.len()), (1, 0));
        assert_eq!(st.style_and_fills(0).0.size, 11.0);
    }

    #[test]
    fn expressions_set_the_symbol_per_feature() {
        let mut l = points();
        let base = l.symbol(0);
        assert!(l.styler(None).is_plain());
        l.symbol_by = SymbolBy {
            size: Some(r#""pop" / 1e5"#.into()),
            fill: Some("CASE WHEN kind = 'a' THEN '#ff0000' WHEN kind = 'b' THEN color_rgba(0, 0, 255, 128) ELSE 'nocolour' END".into()),
            stroke: Some("'green'".into()),
            stroke_width: Some("@map_scale / 1000".into()),
            angle: Some("deg".into()),
        };
        let st = l.styler(Some(4000.0));
        assert!(!st.is_plain());
        let (a, b, c) = (st.symbol(0), st.symbol(1), st.symbol(2));
        assert_eq!((a.size, a.fill, a.stroke, a.stroke_width), (20.0, Rgba([255, 0, 0, 255]), Rgba([0, 128, 0, 255]), 4.0));
        // Null (no population) and what isn't a colour keep the symbol's own.
        assert_eq!((b.size, b.fill), (base.size, Rgba([0, 0, 255, 128])));
        assert_eq!((c.size, c.fill), (5.0, base.fill));
        assert_eq!((st.marker_turn(0), st.marker_turn(1), st.marker_turn(2)), (90.0, 90.0, 330.0));
        // The styled symbol comes with the strokes the layer's own style gives.
        assert_eq!(st.style(0).0, a);
        // Without a map, @map_scale is null: the width is the symbol's.
        assert_eq!(l.styler(None).symbol(0).stroke_width, base.stroke_width);
    }

    #[test]
    fn values_are_kept_in_range() {
        let mut l = points();
        l.symbol_by =
            SymbolBy { size: Some("pop".into()), stroke_width: Some("-5".into()), angle: Some("1 / 0".into()), ..Default::default() };
        let st = l.styler(None);
        assert_eq!((st.symbol(0).size, st.symbol(0).stroke_width), (MAX_SIZE, 0.0));
        // A point sized 0 or less draws nothing, its outline neither.
        let mut zero = points();
        zero.symbol_by.size = Some("pop - 1e6".into());
        let gone = zero.styler(None).symbol(2);
        assert_eq!((gone.size, gone.fill.a(), gone.stroke.a(), gone.stroke_width), (0.0, 0, 0, 0.0));
        // No number from the angle: the field rotation's, or none.
        assert_eq!(st.marker_turn(0), 0.0);
        l.marker_rotation = Some(MarkerRotation { field: "deg".into(), counterclockwise: false });
        assert_eq!(l.styler(None).marker_turn(1), 90.0);
    }

    #[test]
    fn a_points_marks_scale_with_its_size() {
        use crate::symbology::{LineMark, LineMarkers, Marker};
        let mut l = points();
        let base = l.symbol(0).size;
        let mark = |size: f32, along: f32| LineStroke {
            markers: Some(LineMarkers { mark: LineMark::Marker(Marker::Circle), size, offset_along: along, ..LineMarkers::default() }),
            offset: 1.0,
            ..LineStroke::default()
        };
        l.line_strokes = vec![mark(10.0, 0.0), mark(2.0, 3.0)];
        // Without a size expression, the marks as they are.
        assert!(matches!(l.styler(None).marks(0, &l.line_strokes), std::borrow::Cow::Borrowed(_)));
        l.symbol_by.size = Some(format!("{} * 2", base));
        let st = l.styler(None);
        let (_, strokes) = st.style(0);
        let m = st.marks(0, strokes);
        let sizes: Vec<_> = m.iter().map(|s| (s.markers.unwrap().size, s.markers.unwrap().offset_along, s.offset)).collect();
        assert_eq!(sizes, vec![(20.0, 0.0, 2.0), (4.0, 6.0, 2.0)]);
        // Sized 0: none.
        l.symbol_by.size = Some("0".into());
        assert!(l.styler(None).marks(0, &l.line_strokes).is_empty());
        // Proportional symbols size the symbol per feature, not the marks: they scale against the
        // renderer's base size.
        let base_sym = l.symbol(0);
        l.renderer = crate::Renderer::Proportional {
            field: "pop".into(),
            base: Symbol { size: 10.0, ..base_sym },
            min_size: 5.0,
            max_size: 40.0,
            min: 0.0,
            max: 2e6,
        };
        l.symbol_by.size = Some("20".into());
        let st = l.styler(None);
        assert_ne!(l.symbol(0).size, 10.0, "sized per feature");
        assert_eq!(st.marks(0, &l.line_strokes).first().and_then(|m| m.markers).map(|m| m.size), Some(20.0));
    }

    #[test]
    fn what_no_longer_parses_draws_as_unset() {
        let mut l = points();
        let base = l.symbol(0);
        l.symbol_by = SymbolBy { size: Some("gone * 2".into()), fill: Some("'red' ||".into()), ..Default::default() };
        let st = l.styler(Some(1e4));
        assert!(st.is_plain());
        assert_eq!(st.symbol(0), base);
        // A graphics layer's features carry their own style.
        l.symbol_by.size = Some("pop".into());
        l.graphics = true;
        assert!(l.styler(None).is_plain());
    }

    #[test]
    fn kept_in_a_project_and_read_leniently() {
        let mut l = points();
        // None isn't written: an older RustGIS reads the layer as before.
        assert!(serde_json::to_value(&l).unwrap().get("symbol_by").is_none());
        l.symbol_by = SymbolBy { fill: Some("'#00ff00'".into()), angle: Some("deg + 5".into()), ..Default::default() };
        let v = serde_json::to_value(&l).unwrap();
        assert_eq!(v["symbol_by"], serde_json::json!({"fill": "'#00ff00'", "angle": "deg + 5"}));
        let back: Layer = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(back.symbol_by, l.symbol_by);
        for junk in [
            serde_json::json!(5),
            serde_json::json!("size"),
            serde_json::json!({"fill": 3, "angle": ["deg"]}),
            serde_json::json!({"size": "", "fill": "x".repeat(query::MAX_LEN + 1)}),
        ] {
            let mut v = v.clone();
            v["symbol_by"] = junk.clone();
            let read: Layer = serde_json::from_value(v).unwrap();
            assert!(read.symbol_by.is_empty(), "{junk}");
        }
        // One bad key leaves the others.
        let mut v = v.clone();
        v["symbol_by"] = serde_json::json!({"size": 7, "stroke": "'red'"});
        let read: Layer = serde_json::from_value(v).unwrap();
        assert_eq!(read.symbol_by, SymbolBy { stroke: Some("'red'".into()), ..Default::default() });
    }

    /// A Graduated Colors layer sized by another field: each point keeps its class's colour, at
    /// the size its value gives; a value past an end takes the end's size, a null or text one its
    /// class's own; under another renderer the mapping draws nothing.
    #[test]
    fn a_size_mapping_sizes_classed_points_and_keeps_their_colours() {
        let mut l = points();
        l.dataset.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![Value::Float(9e6), Value::Text("d".into()), Value::Float(0.0)]);
        l.dataset
            .push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![Value::Text("n/a".into()), Value::Text("e".into()), Value::Float(0.0)]);
        let base = l.symbol(0);
        l.renderer = crate::Renderer::Unique {
            field: "kind".into(),
            classes: ["a", "b", "c", "d", "e"]
                .iter()
                .map(|k| (k.to_string(), Symbol { fill: Rgba::rgb(1, 2, 3), size: 7.0, ..base }))
                .collect(),
            other: base,
            ramp: Default::default(),
            class_labels: Default::default(),
            other_label: None,
            class_strokes: Default::default(),
            other_strokes: Vec::new(),
            class_fills: Default::default(),
            other_fills: Vec::new(),
            expression: None,
        };
        let by = SizeBy { field: "pop".into(), min: 0.0, max: 4e6, min_size: 2.0, max_size: 22.0, scaling: SizeScaling::Radius };
        l.size_by = Some(by.clone());
        let st = l.styler(None);
        assert!(!st.is_plain());
        let sizes: Vec<f32> = (0..5).map(|r| st.symbol(r).size).collect();
        // 2e6 halfway; null and text keep the class's 7; 9e6 past the top takes 22.
        assert_eq!(sizes, [12.0, 7.0, 4.5, 22.0, 7.0]);
        assert!((0..5).all(|r| st.symbol(r).fill == Rgba::rgb(1, 2, 3)), "colours kept");
        assert_eq!(l.dataset.value(0, 0), &Value::Float(2e6), "attributes untouched");
        // By area, halfway in value is the square root of a half of the way in size.
        l.size_by = Some(SizeBy { scaling: SizeScaling::Area, ..by.clone() });
        assert_eq!(l.styler(None).symbol(0).size, 2.0 + 20.0 * 0.5f32.sqrt());
        // No range: every value takes the size halfway, as Proportional Symbols do.
        assert_eq!(SizeBy { max: 0.0, ..by.clone() }.size(5.0), 12.0);
        assert_eq!(SizeBy { min: 1.0, max: 0.0, ..by.clone() }.size(-3.0), 12.0);
        assert_eq!(SizeBy { max: 0.0, scaling: SizeScaling::Area, ..by.clone() }.size(5.0), 2.0 + 20.0 * 0.5f32.sqrt());
        // The legend lists the classes, then three sizes.
        l.size_by = Some(by.clone());
        let rows = l.with_size_legend(l.renderer.legend());
        let sized: Vec<_> = rows.iter().skip(6).map(|e| (e.label.as_str(), e.symbol.size)).collect();
        assert_eq!(sized, [("pop 0", 2.0), ("pop 2.0M", 12.0), ("pop 4.0M", 22.0)]);
        // A field the layer doesn't have sizes nothing, on the map or in the legend.
        l.size_by = Some(SizeBy { field: "gone".into(), ..by.clone() });
        assert_eq!(l.with_size_legend(l.renderer.legend()).len(), 6);
        l.size_by = Some(by.clone());
        // Nor do bins, drawn instead of the points while they're aggregated.
        l.aggregation = Some(crate::effects::Aggregation { hexagons: true, size: 24.0 });
        assert_eq!(l.with_size_legend(l.renderer.legend()).len(), 6);
        l.aggregation = None;
        // Size samples use the first class's own marks, scaled just as on the map.
        let mark = LineStroke {
            markers: Some(crate::symbology::LineMarkers { size: 14.0, offset_along: 3.5, ..Default::default() }),
            offset: 3.5,
            ..Default::default()
        };
        l.line_strokes = vec![LineStroke { markers: Some(crate::symbology::LineMarkers::default()), ..Default::default() }];
        if let crate::Renderer::Unique { class_strokes, .. } = &mut l.renderer {
            class_strokes.insert("a".into(), vec![mark]);
        }
        let rows = l.with_size_legend(l.renderer.legend());
        for (entry, size) in rows.iter().skip(6).zip([2.0, 12.0, 22.0]) {
            let stroke = entry.strokes.first().unwrap();
            assert!((stroke.markers.unwrap().size - 2.0 * size).abs() < 1e-4);
            assert!((stroke.offset - size / 2.0).abs() < 1e-4);
        }
        // A final mapped zero leaves no ink; a positive expression still wins over that zero.
        l.size_by = Some(SizeBy { min: 2e6, min_size: 0.0, ..by.clone() });
        let gone = l.styler(None).symbol(0);
        assert_eq!((gone.size, gone.fill.a(), gone.stroke.a(), gone.stroke_width), (0.0, 0, 0, 0.0));
        let zero_sample = l.size_by.as_ref().unwrap().legend(Symbol { size: 0.0, ..base }, &[mark]);
        assert_eq!(zero_sample[0].strokes[0].color.unwrap().a(), 0, "a zero legend mark has no ink even with a zero class base");
        // An image marker gives way too: it ignores the colours that hide a zero.
        let picture = Symbol { marker: crate::Marker::Picture(crate::Picture::new("AA==")), size: 0.0, ..base };
        assert!(!matches!(hide_zero(picture).marker, crate::Marker::Picture(_)));
        l.symbol_by.size = Some("6".into());
        let back = l.styler(None).symbol(0);
        assert_eq!((back.size, back.fill), (6.0, Rgba::rgb(1, 2, 3)));
        // Under a Size expression the legend shows no mapped sizes, as the map draws none.
        assert_eq!(l.with_size_legend(l.renderer.legend()).len(), l.renderer.legend().len());
        l.symbol_by.size = None;
        // A null value keeps its class's size; a class of size 0 then draws nothing, marks included.
        if let crate::Renderer::Unique { classes, .. } = &mut l.renderer {
            classes.iter_mut().filter(|(k, _)| k == "b").for_each(|(_, s)| s.size = 0.0);
        }
        let hidden = l.styler(None).symbol(1);
        assert_eq!((hidden.size, hidden.fill.a(), hidden.stroke.a()), (0.0, 0, 0));
        assert!(l.styler(None).marks(1, std::slice::from_ref(&mark)).is_empty());
        // A single symbol doesn't take it: kept, unused.
        l.renderer = crate::Renderer::Single(base);
        assert!(l.styler(None).is_plain());
        assert_eq!(l.with_size_legend(Vec::new()), Vec::new());
    }

    /// Saved only when set, so an older project reads and writes as before; malformed mappings
    /// are refused instead of silently losing their settings on the next save.
    #[test]
    fn a_size_mapping_is_saved_only_when_set_and_refused_when_malformed() {
        let mut l = points();
        assert!(serde_json::to_value(&l).unwrap().get("size_by").is_none());
        l.size_by = Some(SizeBy { field: "pop".into(), min: 1.0, max: 9.0, min_size: 4.0, max_size: 30.0, scaling: SizeScaling::Radius });
        let v = serde_json::to_value(&l).unwrap();
        assert_eq!(
            v["size_by"],
            serde_json::json!({"field": "pop", "min": 1.0, "max": 9.0, "min_size": 4.0, "max_size": 30.0, "scaling": "radius"})
        );
        let back: Layer = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(back.size_by, l.size_by);
        let mut empty = v.clone();
        empty["size_by"] = serde_json::Value::Null;
        assert!(serde_json::from_value::<Layer>(empty).unwrap().size_by.is_none());
        for junk in [
            serde_json::json!("pop"),
            serde_json::json!({"field": "", "min": 1, "max": 2, "min_size": 1, "max_size": 2}),
            serde_json::json!({"field": "pop", "min": 1, "max": 2, "min_size": -1, "max_size": 2}),
            serde_json::json!({"field": "pop", "min": 1, "max": 2, "min_size": 1, "max_size": 2, "scaling": "cubic"}),
            // Ranges `layer.sizeBy` refuses: none, and one too wide to scale over.
            serde_json::json!({"field": "pop", "min": 5, "max": 5, "min_size": 1, "max_size": 2}),
            serde_json::json!({"field": "pop", "min": -1e308, "max": 1e308, "min_size": 1, "max_size": 2}),
        ] {
            let mut v = v.clone();
            v["size_by"] = junk.clone();
            assert!(serde_json::from_value::<Layer>(v).is_err(), "{junk}");
        }
        // Field names come from the dataset; a valid long name must not make our own saved
        // project unreadable. The project reader bounds the document as a whole.
        let field = "x".repeat(2 * crate::symbology::MAX_FONT_NAME + 1);
        l.dataset.fields[0].name = field.clone();
        l.size_by.as_mut().unwrap().field = field;
        let back: Layer = serde_json::from_value(serde_json::to_value(&l).unwrap()).unwrap();
        assert_eq!(back.size_by, l.size_by);
    }

    /// A finite range whose subtraction overflows uses the documented midpoint fallback,
    /// consistently for every value rather than jumping backwards at the high end.
    #[test]
    fn a_size_range_too_wide_to_divide_uses_the_midpoint() {
        for value in [-1e308, 0.0, 1e308] {
            assert_eq!(scaled_size(value, -1e308, 1e308, 2.0, 22.0, SizeScaling::Radius), 12.0);
        }
    }
}
