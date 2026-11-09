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
        Styler { layer, by, classer }
    }

    /// Whether no expression applies: every row draws as the layer's own methods give it.
    pub fn is_plain(&self) -> bool {
        self.by.is_none() && self.classer.is_none()
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

    fn apply(&self, row: usize, sym: Symbol) -> Symbol {
        match &self.by {
            Some(b) => b.apply(&self.layer.dataset, row, sym),
            None => sym,
        }
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
    /// expression gives, against the symbol's own, as QGIS scales a marker of several layers: their
    /// sizes and offsets. None at size 0. The marks as they are without a size expression.
    pub fn marks(&self, row: usize, strokes: &'a [LineStroke]) -> std::borrow::Cow<'a, [LineStroke]> {
        use std::borrow::Cow;
        let Some(b) = self.by.as_ref().filter(|b| b.points && !strokes.is_empty()) else { return Cow::Borrowed(strokes) };
        let Some(size) = b.number(&b.size, &self.layer.dataset, row) else { return Cow::Borrowed(strokes) };
        let size = (size as f32).clamp(0.0, MAX_SIZE);
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
        let k = size / base;
        Cow::Owned(
            strokes
                .iter()
                .map(|st| {
                    let mut st = *st;
                    st.offset *= k;
                    if let Some(m) = &mut st.markers {
                        m.size *= k;
                        m.offset_along *= k;
                    }
                    st.clamped()
                })
                .collect(),
        )
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
}
