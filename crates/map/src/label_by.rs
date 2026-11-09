//! A label class's text, size, colour and angle per feature by expressions
//! ([`crate::LabelClass::by`]), in the language of [`rustgis_data::query`]: a text such as
//! `"name" || ' (' || round("pop" / 1e6, 1) || ' M)'`, a size by `scale_linear(…)`, a colour by
//! `CASE WHEN … END`. Stored as text and compiled against the layer's fields once per drawing
//! ([`LabelExprs::new`]), as [`crate::symbol_by`] is for symbols.

use crate::Rgba;
use crate::symbology::LABEL_SIZE;
use rustgis_data::query::{self, Env, Expr};
use rustgis_data::{Dataset, Value};
use serde::{Deserialize, Serialize};

/// The most characters of a label's text by expression kept.
pub const MAX_LABEL_TEXT: usize = 1000;

/// Expressions setting a label class's labels per feature; none set: its field, size, colour and
/// placement's angle alone.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LabelBy {
    /// The label's text, in place of the class's field: a feature it gives null for isn't labelled.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "de_text")]
    pub text: Option<String>,
    /// The text's size in points (within [`LABEL_SIZE`]).
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "de_text")]
    pub size: Option<String>,
    /// The text's colour: `'#rrggbb'`, a CSS colour or `'r,g,b[,a]'`.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "de_text")]
    pub color: Option<String>,
    /// A point's or polygon's label turned by this angle in degrees clockwise about its centre,
    /// kept upright: one past 90° reads from its other end (labels placed along lines keep the
    /// line's).
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "de_text")]
    pub angle: Option<String>,
}

impl LabelBy {
    pub fn is_empty(&self) -> bool {
        self.text.is_none() && self.size.is_none() && self.color.is_none() && self.angle.is_none()
    }
}

/// Reads one expression leniently: text up to [`query::MAX_LEN`], blank or anything else none.
fn de_text<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(match serde_json::Value::deserialize(d)? {
        serde_json::Value::String(s) if !s.trim().is_empty() && s.len() <= query::MAX_LEN => Some(s),
        _ => None,
    })
}

/// Reads a class's expressions leniently: a crafted file's that aren't are none, and the rest of
/// the class still reads.
pub fn de_label_by<'de, D: serde::Deserializer<'de>>(d: D) -> Result<LabelBy, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(v).unwrap_or_default())
}

/// A class's expressions compiled against a layer's fields for one drawing.
#[derive(Clone, Debug)]
pub struct LabelExprs {
    text: Option<Expr>,
    size: Option<Expr>,
    color: Option<Expr>,
    angle: Option<Expr>,
    env: Env,
}

impl LabelExprs {
    /// None where nothing parses against the dataset (an expression naming a field since removed
    /// labels as if it weren't set). `map_scale`: the scale (1:N) the map is drawn at, if any.
    pub fn new(by: &LabelBy, ds: &Dataset, map_scale: Option<f64>) -> Option<LabelExprs> {
        if by.is_empty() {
            return None;
        }
        let one = |t: &Option<String>| t.as_deref().and_then(|t| query::parse_fields(t, ds).ok());
        let e = LabelExprs {
            text: one(&by.text),
            size: one(&by.size),
            color: one(&by.color),
            angle: one(&by.angle),
            env: Env { map_scale: map_scale.filter(|s| s.is_finite() && *s > 0.0) },
        };
        (e.text.is_some() || e.size.is_some() || e.color.is_some() || e.angle.is_some()).then_some(e)
    }

    /// Whether the text comes from the expression, not the class's field.
    pub fn has_text(&self) -> bool {
        self.text.is_some()
    }

    fn number(&self, e: &Option<Expr>, ds: &Dataset, row: usize) -> Option<f64> {
        query::value(e.as_ref()?, ds, row, &self.env).as_f64().filter(|v| v.is_finite())
    }

    /// The row's text: `None` without a text expression, `Some(None)` where it gives null or
    /// nothing but blanks (no label).
    pub fn text(&self, ds: &Dataset, row: usize) -> Option<Option<String>> {
        self.text_cut(ds, row).map(|t| t.map(|(t, _)| t))
    }

    /// [`LabelExprs::text`], and whether it was cut to [`MAX_LABEL_TEXT`] characters.
    pub fn text_cut(&self, ds: &Dataset, row: usize) -> Option<Option<(String, bool)>> {
        let e = self.text.as_ref()?;
        let v = query::value(e, ds, row, &self.env);
        if v.is_null() {
            return Some(None);
        }
        let mut t = v.to_string();
        let cut = t.char_indices().nth(MAX_LABEL_TEXT).map(|(at, _)| at);
        if let Some(at) = cut {
            t.truncate(at);
        }
        Some(Some((t, cut.is_some())).filter(|(t, _)| !t.trim().is_empty()))
    }

    /// The row's size in points, none where the expression gives no number.
    pub fn size(&self, ds: &Dataset, row: usize) -> Option<f32> {
        self.number(&self.size, ds, row).map(|v| v.clamp(*LABEL_SIZE.start(), *LABEL_SIZE.end()) as f32)
    }

    /// The row's colour, none where the expression gives no colour.
    pub fn color(&self, ds: &Dataset, row: usize) -> Option<Rgba> {
        match query::value(self.color.as_ref()?, ds, row, &self.env) {
            Value::Text(t) => rustgis_data::color::parse_value(&t).map(Rgba),
            _ => None,
        }
    }

    /// The row's angle in degrees clockwise (0 to 360), none where the expression gives no number.
    pub fn angle(&self, ds: &Dataset, row: usize) -> Option<f32> {
        self.number(&self.angle, ds, row).map(|v| v.rem_euclid(360.0) as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_data::{Field, FieldType, Geometry, GeometryType};
    use rustgis_geom::Coord;

    fn places() -> Dataset {
        let fields = vec![Field::new("name", FieldType::Text), Field::new("pop", FieldType::Double), Field::new("deg", FieldType::Double)];
        let mut ds = Dataset::new("p", GeometryType::Point, fields);
        for (name, pop, deg) in [(Value::Text("Bern".into()), 1.4e5, 30.0), (Value::Null, 2e7, -90.0)] {
            ds.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![name, Value::Float(pop), Value::Float(deg)]);
        }
        ds
    }

    #[test]
    fn expressions_give_each_labels_text_size_colour_and_angle() {
        let ds = places();
        let by = LabelBy {
            text: Some(r#""name" || ' (' || round("pop" / 1e6, 1) || ' M)'"#.into()),
            size: Some("scale_linear(pop, 0, 2e7, 8, 20)".into()),
            color: Some("if(pop > 1e6, '#ff0000', 'nocolour')".into()),
            angle: Some("deg".into()),
        };
        let e = LabelExprs::new(&by, &ds, None).unwrap();
        assert!(e.has_text());
        assert_eq!(e.text(&ds, 0), Some(Some("Bern (0.1 M)".into())));
        // A null name joins to null: no label.
        assert_eq!(e.text(&ds, 1), Some(None));
        assert_eq!(e.size(&ds, 1), Some(20.0));
        assert_eq!((e.color(&ds, 0), e.color(&ds, 1)), (None, Some(Rgba([255, 0, 0, 255]))));
        assert_eq!((e.angle(&ds, 0), e.angle(&ds, 1)), (Some(30.0), Some(270.0)));
        // Sizes are kept to the label sizes; a long text is cut.
        let big = LabelBy { size: Some("1e9".into()), text: Some("concat(name, name)".into()), ..Default::default() };
        let e = LabelExprs::new(&big, &ds, None).unwrap();
        assert_eq!(e.size(&ds, 0), Some(*LABEL_SIZE.end() as f32));
        let long = LabelBy { text: Some(format!("'{}'", "x".repeat(5000))), ..Default::default() };
        let e = LabelExprs::new(&long, &ds, None).unwrap();
        assert_eq!(e.text(&ds, 0).flatten().map(|t| t.chars().count()), Some(MAX_LABEL_TEXT));
    }

    #[test]
    fn what_no_longer_parses_labels_as_unset() {
        let ds = places();
        assert!(LabelExprs::new(&LabelBy::default(), &ds, None).is_none());
        let gone = LabelBy { text: Some("gone || 'x'".into()), size: Some("(".into()), ..Default::default() };
        assert!(LabelExprs::new(&gone, &ds, None).is_none());
        // A size alone leaves the text to the field.
        let e = LabelExprs::new(&LabelBy { size: Some("@map_scale / 1000".into()), ..Default::default() }, &ds, Some(12_000.0)).unwrap();
        assert!(!e.has_text() && e.text(&ds, 0).is_none());
        assert_eq!(e.size(&ds, 0), Some(12.0));
    }

    #[test]
    fn kept_in_a_project_and_read_leniently() {
        let by = LabelBy { text: Some("upper(name)".into()), angle: Some("45".into()), ..Default::default() };
        let v = serde_json::to_value(&by).unwrap();
        assert_eq!(v, serde_json::json!({"text": "upper(name)", "angle": "45"}));
        let back: LabelBy = serde_json::from_value(v).unwrap();
        assert_eq!(back, by);
        #[derive(Deserialize)]
        struct Holder {
            #[serde(default, deserialize_with = "de_label_by")]
            by: LabelBy,
        }
        for junk in [serde_json::json!(5), serde_json::json!("text"), serde_json::json!({"text": 3, "size": ["x"]})] {
            let h: Holder = serde_json::from_value(serde_json::json!({ "by": junk })).unwrap();
            assert!(h.by.is_empty(), "{junk}");
        }
        let h: Holder = serde_json::from_value(serde_json::json!({"by": {"text": "", "color": "'red'", "angle": 4}})).unwrap();
        assert_eq!(h.by, LabelBy { color: Some("'red'".into()), ..Default::default() });
    }
}
