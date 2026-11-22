//! Layer display effects: blending, masking, display filters, point aggregation and 2.5D
//! extrusion with lighting and shadows.

use serde::{Deserialize, Serialize};

/// How a layer's pixels combine with what's beneath.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Blend {
    #[default]
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    Difference,
}

impl Blend {
    pub const ALL: [Blend; 7] =
        [Blend::Normal, Blend::Multiply, Blend::Screen, Blend::Overlay, Blend::Darken, Blend::Lighten, Blend::Difference];
    pub fn code(self) -> &'static str {
        match self {
            Blend::Normal => "normal",
            Blend::Multiply => "multiply",
            Blend::Screen => "screen",
            Blend::Overlay => "overlay",
            Blend::Darken => "darken",
            Blend::Lighten => "lighten",
            Blend::Difference => "difference",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Blend::Normal => "Normal",
            Blend::Multiply => "Multiply",
            Blend::Screen => "Screen",
            Blend::Overlay => "Overlay",
            Blend::Darken => "Darken",
            Blend::Lighten => "Lighten",
            Blend::Difference => "Difference",
        }
    }
    pub fn from_code(s: &str) -> Option<Blend> {
        Blend::ALL.into_iter().find(|b| b.code().eq_ignore_ascii_case(s.trim()))
    }
}

/// A scale-dependent filter: between two scales only features matching `where_` draw. When
/// several apply at a scale, a feature must match all of them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DisplayFilter {
    pub name: String,
    /// Applies when zoomed in beyond 1:`min_scale` (out-beyond limit), if set.
    #[serde(default)]
    pub min_scale: Option<f64>,
    /// Applies when zoomed out beyond 1:`max_scale` (in-beyond limit), if set.
    #[serde(default)]
    pub max_scale: Option<f64>,
    #[serde(rename = "where")]
    pub where_: String,
}

impl DisplayFilter {
    pub fn applies(&self, scale: f64) -> bool {
        self.min_scale.is_none_or(|m| scale <= m) && self.max_scale.is_none_or(|m| scale >= m)
    }
}

/// Points drawn as bins coloured by how many fall in each.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Aggregation {
    pub hexagons: bool,
    /// Bin size in points.
    pub size: f64,
}

/// How polygons rise above the map, drawn as an oblique 2.5D view.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Extrusion {
    /// Height in metres from this field, else `height`.
    #[serde(default)]
    pub field: Option<String>,
    #[serde(default)]
    pub height: f64,
    /// Heights are multiplied by this.
    #[serde(default = "one")]
    pub factor: f64,
    /// Walls facing away from the viewer aren't drawn.
    #[serde(default = "yes")]
    pub face_culling: bool,
    /// Walls are shaded by the way they face the light.
    #[serde(default = "yes")]
    pub lighting: bool,
    /// Extruded shapes cast a shadow on the map.
    #[serde(default)]
    pub shadows: bool,
    /// Base in metres from this field, else `base`: walls rise from it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_field: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub base: f64,
}

fn is_zero(v: &f64) -> bool {
    *v == 0.0
}

/// The columns of a dataset an extrusion reads, found once per layer.
#[derive(Clone, Copy, Debug, Default)]
pub struct ExtrusionColumns {
    pub height: Option<usize>,
    pub base: Option<usize>,
}

impl Default for Extrusion {
    fn default() -> Self {
        Self { field: None, height: 0.0, factor: 1.0, face_culling: true, lighting: true, shadows: false, base_field: None, base: 0.0 }
    }
}

impl Extrusion {
    /// The columns of `ds` this extrusion reads; find them once, then use `metres_at`.
    pub fn columns(&self, ds: &rustgis_data::Dataset) -> ExtrusionColumns {
        ExtrusionColumns {
            height: self.field.as_deref().and_then(|f| ds.field_index(f)),
            base: self.base_field.as_deref().and_then(|f| ds.field_index(f)),
        }
    }

    /// How high (metres) feature `row` of `ds` is extruded, with its columns `c` found already:
    /// its height field, else `height`, times `factor`.
    pub fn metres_at(&self, ds: &rustgis_data::Dataset, row: usize, c: ExtrusionColumns) -> f64 {
        c.height.and_then(|i| ds.value(row, i).as_f64()).filter(|v| v.is_finite()).unwrap_or(self.height) * self.factor
    }

    /// How high (metres) the walls of feature `row` of `ds` start: its base field, else `base`
    /// when the value is no number (empty, or text that isn't one), like the height; times
    /// `factor`, with its columns `c` found already. The result is 0 when it is no finite number
    /// or negative.
    pub fn base_metres_at(&self, ds: &rustgis_data::Dataset, row: usize, c: ExtrusionColumns) -> f64 {
        let b = c.base.and_then(|i| ds.value(row, i).as_f64()).filter(|v| v.is_finite()).unwrap_or(self.base) * self.factor;
        if b.is_finite() && b > 0.0 { b } else { 0.0 }
    }
}

fn one() -> f64 {
    1.0
}

fn yes() -> bool {
    true
}

#[cfg(test)]
mod extrusion_tests {
    use super::*;
    use rustgis_data::{Dataset, Field, FieldType, GeometryType, Value};

    fn rows(values: Vec<Value>) -> Dataset {
        let mut ds = Dataset::new("b", GeometryType::Polygon, vec![Field::new("b", FieldType::Double)]);
        for v in values {
            ds.push(None, vec![v]);
        }
        ds
    }

    #[test]
    fn a_base_comes_from_its_field_else_the_constant_times_the_factor() {
        let ds = rows(vec![Value::Float(10.0), Value::Null, Value::Text("4".into())]);
        let x = Extrusion { base_field: Some("b".into()), base: 3.0, factor: 2.0, ..Extrusion::default() };
        assert_eq!(x.base_metres_at(&ds, 0, x.columns(&ds)), 20.0, "from the field, with the factor");
        assert_eq!(x.base_metres_at(&ds, 1, x.columns(&ds)), 6.0, "no value: the constant, with the factor");
        assert_eq!(x.base_metres_at(&ds, 2, x.columns(&ds)), 8.0, "a number in text");
        let constant = Extrusion { base: 5.0, ..Extrusion::default() };
        assert_eq!(constant.base_metres_at(&ds, 0, constant.columns(&ds)), 5.0, "no field: the constant");
        let unknown = Extrusion { base_field: Some("nope".into()), base: 5.0, ..Extrusion::default() };
        assert_eq!(unknown.base_metres_at(&ds, 0, unknown.columns(&ds)), 5.0, "a field the layer lacks: the constant");
        assert_eq!(
            {
                let none = Extrusion::default();
                none.base_metres_at(&ds, 0, none.columns(&ds))
            },
            0.0,
            "no base: the ground"
        );
    }

    #[test]
    fn a_base_that_is_no_number_or_is_negative_is_the_ground() {
        let ds = rows(vec![Value::Text("tall".into()), Value::Float(f64::NAN), Value::Null, Value::Float(-4.0)]);
        let x = Extrusion { base_field: Some("b".into()), ..Extrusion::default() };
        for row in 0..4 {
            assert_eq!(x.base_metres_at(&ds, row, x.columns(&ds)), 0.0, "row {row}");
        }
        for (base, factor) in [(-1.0, 1.0), (f64::NAN, 1.0), (f64::INFINITY, 1.0), (5.0, -1.0), (5.0, f64::NAN)] {
            let x = Extrusion { base, factor, ..Extrusion::default() };
            assert_eq!(x.base_metres_at(&ds, 0, x.columns(&ds)), 0.0, "base {base}, factor {factor}");
        }
    }

    #[test]
    fn a_saved_extrusion_keeps_its_base_and_an_older_one_has_none() {
        let x = Extrusion { height: 30.0, base_field: Some("floor".into()), base: 12.5, ..Extrusion::default() };
        let back: Extrusion = serde_json::from_str(&serde_json::to_string(&x).unwrap()).unwrap();
        assert_eq!(back, x);
        let plain = serde_json::to_string(&Extrusion::default()).unwrap();
        assert!(!plain.contains("base"), "no base, no keys: {plain}");
        let old: Extrusion =
            serde_json::from_str(r#"{"field":"h","height":5.0,"factor":2.0,"face_culling":true,"lighting":false}"#).unwrap();
        assert_eq!((old.base_field, old.base), (None, 0.0));
        assert_eq!((old.field.as_deref(), old.height, old.factor, old.lighting), (Some("h"), 5.0, 2.0, false));
    }
}
