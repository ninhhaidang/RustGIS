//! Diagrams: a small chart drawn on each feature from several numeric fields, as ArcGIS Pro's
//! Chart symbology and QGIS's Diagrams draw them (pie and bar). A layer's renderer still draws
//! the feature itself; the diagram goes on top of it, so a polygon keeps its fill (say, a
//! Graduated renderer underneath) while the chart shows the make-up of each feature across the
//! category fields.
//!
//! Sizes are in points, so they keep their size on screen at every scale, as a marker's do. A
//! [`DiagramSettings`] is built only through [`DiagramSettings::clamped`], so its numbers are
//! always in range, and read from a project through [`de_diagram`], which takes a malformed one
//! (a hostile file) as `None` and leaves the rest of the layer reading.

use rustgis_data::Dataset;
use serde::{Deserialize, Serialize};

use crate::color::Rgba;
use crate::symbology::{LegendEntry, LineMark, LineMarkers, LineStroke, Marker, Symbol};

/// The most categories one diagram lists (each is a slice or a bar).
pub const MAX_CATEGORIES: usize = 16;

/// The smallest and largest diagram size in points (a pie's radius, a bar chart's height).
pub const MIN_SIZE: f32 = 1.0;
pub const MAX_SIZE: f32 = 200.0;

/// The widest a diagram's outline is, in points.
pub const MAX_STROKE_WIDTH: f32 = 8.0;

/// The colours new categories take in turn: the palette new layers take (`LAYER_COLORS`), so a
/// diagram's categories and a new layer's symbol agree.
pub const CATEGORY_COLORS: [Rgba; 8] = [
    Rgba::rgb(0x4A, 0x90, 0xD9),
    Rgba::rgb(0xE0, 0x7B, 0x39),
    Rgba::rgb(0x5B, 0xB0, 0x6E),
    Rgba::rgb(0xC2, 0x55, 0x9B),
    Rgba::rgb(0xE6, 0xB8, 0x3A),
    Rgba::rgb(0x7D, 0x6B, 0xD1),
    Rgba::rgb(0x3C, 0xB1, 0xB8),
    Rgba::rgb(0xB9, 0x5F, 0x4A),
];

/// What a diagram draws. Written to a project as `"pie"` or `"bar"`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiagramKind {
    /// A slice per category, its angle the category's share of the total.
    #[default]
    Pie,
    /// A bar per category, its height the category's share of the total, rising from a baseline.
    Bar,
}

impl DiagramKind {
    pub const ALL: [DiagramKind; 2] = [DiagramKind::Pie, DiagramKind::Bar];
    pub fn label(self) -> &'static str {
        match self {
            DiagramKind::Pie => "Pie",
            DiagramKind::Bar => "Bar",
        }
    }
    /// The code a command names it by ("pie", "bar"), whatever its case.
    pub fn from_code(code: &str) -> Option<DiagramKind> {
        DiagramKind::ALL.into_iter().find(|k| k.label().eq_ignore_ascii_case(code.trim()))
    }
}

/// What each slice or bar says beside it: nothing, its value, its share of the total in percent,
/// or both. The value takes the number format the legend's counts do (`symbology::fmt_num`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiagramLabel {
    #[default]
    None,
    Value,
    Percent,
    Both,
}

impl DiagramLabel {
    pub const ALL: [DiagramLabel; 4] = [DiagramLabel::None, DiagramLabel::Value, DiagramLabel::Percent, DiagramLabel::Both];
    pub fn label(self) -> &'static str {
        match self {
            DiagramLabel::None => "None",
            DiagramLabel::Value => "Value",
            DiagramLabel::Percent => "Percent",
            DiagramLabel::Both => "Value and Percent",
        }
    }
    /// The code a command names it by ("none", "value", "percent", "both"), whatever its case.
    pub fn from_code(code: &str) -> Option<DiagramLabel> {
        let code = code.trim();
        DiagramLabel::ALL.into_iter().find(|l| l.label().eq_ignore_ascii_case(code) || l.code().eq_ignore_ascii_case(code))
    }
    /// The code commands and the project file carry.
    pub fn code(self) -> &'static str {
        match self {
            DiagramLabel::None => "none",
            DiagramLabel::Value => "value",
            DiagramLabel::Percent => "percent",
            DiagramLabel::Both => "both",
        }
    }
}

/// One category: the field its value is read from, and the colour drawn for it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DiagramCategory {
    pub field: String,
    pub colour: Rgba,
}

/// A category's mark in a point layer's legend: its own, or the row would take the layer's stacked
/// marks and every category would look the same.
fn category_mark(colour: Rgba) -> LineStroke {
    let markers = LineMarkers {
        mark: LineMark::Marker(Marker::Circle),
        size: 7.0,
        outline: Some(Rgba::rgb(0xFF, 0xFF, 0xFF)),
        ..LineMarkers::default()
    };
    LineStroke { color: Some(colour), width: Some(1.0), markers: Some(markers), ..LineStroke::default() }
}

/// How big a diagram is: one size for every feature, or one grown with the categories' sum, as
/// [`crate::Renderer::Proportional`] grows a marker (area-proportional square root scaling).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiagramSize {
    /// A pie's radius, or a bar chart's height, in points: the same for every feature.
    Fixed(f32),
    /// Grown with the categories' sum: the size runs `min_size` to `max_size` in points as the
    /// sum runs `min` to `max` (the range over the layer's features, kept as Proportional keeps
    /// its range), by the square root of its share, so a pie's area tracks the total.
    Scaled { min_size: f32, max_size: f32, min: f64, max: f64 },
}

/// A layer's diagram: what it draws, from which fields, and how big ([`Layer::diagram`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DiagramSettings {
    pub kind: DiagramKind,
    /// The categories in draw order (pie clockwise from [`Self::start_angle`]; bars left to
    /// right). A field the dataset lacks, or whose value for a feature isn't a number, is
    /// skipped for that feature.
    pub categories: Vec<DiagramCategory>,
    pub size: DiagramSize,
    /// Where a pie's first slice starts, in degrees clockwise from north (12 o'clock).
    #[serde(default)]
    pub start_angle: f32,
    /// The disc (pie) or box (bars) behind the chart; `None` draws none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<Rgba>,
    /// The outline of the whole chart, and each slice's or bar's; its width in points.
    #[serde(default = "DiagramSettings::default_stroke")]
    pub stroke: Rgba,
    #[serde(default = "DiagramSettings::default_stroke_width")]
    pub stroke_width: f32,
    /// What each slice or bar says beside it: nothing, its value, its share in percent, or both.
    #[serde(default, skip_serializing_if = "DiagramLabel::is_none")]
    pub labels: DiagramLabel,
}

impl DiagramSettings {
    /// The outline a diagram without one set draws.
    pub fn default_stroke() -> Rgba {
        Rgba::rgb(0x4D, 0x4D, 0x4D)
    }
    /// The outline width a diagram without one set draws.
    pub fn default_stroke_width() -> f32 {
        0.75
    }

    /// Brought into range: at most [`MAX_CATEGORIES`] categories, each with a name (one without
    /// is dropped), a size from [`MIN_SIZE`] to [`MAX_SIZE`] with `min_size` no more than
    /// `max_size`, a start angle within a turn, and a stroke width up to [`MAX_STROKE_WIDTH`].
    /// Too large for an f32 (infinite once read) is the nearest bound, as a large value is.
    pub fn clamped(mut self) -> DiagramSettings {
        let fit = |v: f32, lo: f32, hi: f32| if v.is_nan() { lo } else { v.clamp(lo, hi) };
        self.categories.retain(|c| !c.field.trim().is_empty());
        self.categories.truncate(MAX_CATEGORIES);
        self.size = match self.size {
            DiagramSize::Fixed(s) => DiagramSize::Fixed(fit(s, MIN_SIZE, MAX_SIZE)),
            DiagramSize::Scaled { min_size, max_size, min, max } => DiagramSize::Scaled {
                min_size: fit(min_size, MIN_SIZE, MAX_SIZE),
                max_size: fit(max_size, MIN_SIZE, MAX_SIZE),
                min: if min.is_finite() { min } else { 0.0 },
                max: if max.is_finite() { max } else { 1.0 },
            },
        };
        if let DiagramSize::Scaled { min_size, max_size, min, max } = &mut self.size {
            if *min_size > *max_size {
                std::mem::swap(min_size, max_size);
            }
            // A data range stored upside down would size every feature at the middle.
            if *min > *max {
                std::mem::swap(min, max);
            }
        }
        // A range too wide for an f64 (its span infinite) would make the sizes not numbers.
        if let DiagramSize::Scaled { min, max, .. } = &mut self.size
            && !(*max - *min).is_finite()
        {
            (*min, *max) = (0.0, 1.0);
        }
        self.start_angle = if self.start_angle.is_finite() { self.start_angle.rem_euclid(360.0) } else { 0.0 };
        self.stroke_width = fit(self.stroke_width, 0.0, MAX_STROKE_WIDTH);
        self
    }

    /// The largest size the diagram reaches, in points (for culling): a fixed size, or the top
    /// of a scaled one.
    pub fn max_size(&self) -> f32 {
        match self.size {
            DiagramSize::Fixed(s) => s,
            DiagramSize::Scaled { max_size, .. } => max_size,
        }
    }

    /// The size of the diagram for a feature whose categories sum to `total`, in points: the
    /// fixed size, or one grown by the square root of the total's share of the range, as
    /// Proportional grows a marker.
    pub fn size_for(&self, total: f64) -> f32 {
        match self.size {
            DiagramSize::Fixed(s) => s,
            DiagramSize::Scaled { min_size, max_size, min, max } => {
                let t = if max > min { ((total - min) / (max - min)).clamp(0.0, 1.0) } else { 0.5 };
                min_size + (max_size - min_size) * t.sqrt() as f32
            }
        }
    }

    /// Each category's field index in `ds`, in draw order (`None` for a field the dataset lacks):
    /// resolved once for a layer's features, for [`Self::values_at`].
    pub fn field_indices(&self, ds: &Dataset) -> Vec<Option<usize>> {
        self.categories.iter().map(|c| ds.field_index(&c.field)).collect()
    }

    /// Row `row`'s category values with their colours, and their total, as `(values, total)`:
    /// finite positive values only (a missing field, a null or non-number, a zero or negative
    /// one, is skipped), and `None` when nothing is left to draw or the total overflows.
    pub fn values(&self, ds: &Dataset, row: usize) -> Option<(Vec<(f64, Rgba)>, f64)> {
        self.values_at(ds, row, &self.field_indices(ds))
    }

    /// [`Self::values`] with the field indices given ([`Self::field_indices`]), so a layer's
    /// features resolve them once rather than per feature.
    pub fn values_at(&self, ds: &Dataset, row: usize, fields: &[Option<usize>]) -> Option<(Vec<(f64, Rgba)>, f64)> {
        let mut out: Vec<(f64, Rgba)> = Vec::with_capacity(self.categories.len());
        let mut total = 0.0;
        for (c, fi) in self.categories.iter().zip(fields) {
            let Some(v) = fi.and_then(|fi| ds.value(row, fi).as_f64()).filter(|v| v.is_finite() && *v > 0.0) else {
                continue;
            };
            total += v;
            out.push((v, c.colour));
        }
        (total > 0.0 && total.is_finite()).then_some((out, total))
    }

    /// The legend rows of its categories: one per category, in draw order, its field the label.
    /// On a line layer the colour goes in the stroke, which is what a line's swatch draws; else it
    /// is the fill, with a white outline.
    pub fn legend(&self, line: bool) -> Vec<LegendEntry> {
        self.categories
            .iter()
            .map(|c| LegendEntry {
                label: c.field.clone(),
                symbol: Symbol {
                    fill: c.colour,
                    stroke: if line { c.colour } else { Rgba::rgb(0xFF, 0xFF, 0xFF) },
                    stroke_width: 1.0,
                    size: 7.0,
                    marker: Marker::Circle,
                },
                strokes: vec![category_mark(c.colour)],
                fallback: false,
            })
            .collect()
    }
}

impl DiagramLabel {
    /// Whether nothing is said — the setting an older project file carries without saying so.
    pub fn is_none(&self) -> bool {
        matches!(self, DiagramLabel::None)
    }
}

/// Reads a layer's diagram leniently: one that isn't (a crafted file's) is `None`, and the rest
/// of the layer still reads. A diagram without categories, or a size that isn't a number, is
/// not a diagram, as an empty one draws nothing. [`DiagramAudit`] counts what this changes, for
/// the project reader's notices.
pub fn de_diagram<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<DiagramSettings>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Read {
        Settings(DiagramSettings),
        Other(serde::de::IgnoredAny),
    }
    Ok(match Option::<Read>::deserialize(d)? {
        Some(Read::Settings(s)) => {
            let s = s.clamped();
            (!s.categories.is_empty()).then_some(s)
        }
        _ => None,
    })
}

/// What reading a layer's saved diagram through [`de_diagram`] changed, counted without keeping
/// the diagram: the project reader adds these up across its layers and says so once per kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiagramAudit {
    /// Diagrams that didn't read, or had no named category, and so were dropped (0 or 1).
    pub dropped: usize,
    /// Categories removed from a diagram that was kept: blank field names, or beyond
    /// [`MAX_CATEGORIES`].
    pub categories_removed: usize,
    /// Categories kept that repeat an earlier one's field (ignoring case), each drawn again as
    /// its own slice or bar.
    pub repeated_fields: usize,
    /// Diagrams whose size, data range or outline width was brought into range or order (0 or
    /// 1). A start angle is turned into 0 to 360, the same direction, so isn't counted.
    pub adjusted: usize,
}

impl DiagramAudit {
    /// The counts of `self` and `other` together.
    pub fn plus(self, other: DiagramAudit) -> DiagramAudit {
        DiagramAudit {
            dropped: self.dropped.saturating_add(other.dropped),
            categories_removed: self.categories_removed.saturating_add(other.categories_removed),
            repeated_fields: self.repeated_fields.saturating_add(other.repeated_fields),
            adjusted: self.adjusted.saturating_add(other.adjusted),
        }
    }
}

impl<'de> Deserialize<'de> for DiagramAudit {
    /// Reads a saved diagram as [`de_diagram`] does and counts what that changes; a missing or
    /// null diagram changes nothing.
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Read {
            Settings(DiagramSettings),
            Other(serde::de::IgnoredAny),
        }
        let s = match Option::<Read>::deserialize(d)? {
            None => return Ok(DiagramAudit::default()),
            Some(Read::Other(_)) => return Ok(DiagramAudit { dropped: 1, ..DiagramAudit::default() }),
            Some(Read::Settings(s)) => s,
        };
        let kept = s.clone().clamped();
        if kept.categories.is_empty() {
            return Ok(DiagramAudit { dropped: 1, ..DiagramAudit::default() });
        }
        let mut seen: Vec<&str> = Vec::with_capacity(kept.categories.len());
        let mut repeated_fields = 0usize;
        for c in &kept.categories {
            if seen.iter().any(|f| f.eq_ignore_ascii_case(&c.field)) {
                repeated_fields = repeated_fields.saturating_add(1);
            } else {
                seen.push(&c.field);
            }
        }
        // Any finite angle names the same direction after a whole number of turns: not counted.
        let angle_adjusted = !s.start_angle.is_finite();
        let adjusted = kept.size != s.size || angle_adjusted || kept.stroke_width != s.stroke_width;
        Ok(DiagramAudit {
            dropped: 0,
            categories_removed: s.categories.len().saturating_sub(kept.categories.len()),
            repeated_fields,
            adjusted: usize::from(adjusted),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_data::{Field, FieldType, GeometryType, Value};

    fn settings() -> DiagramSettings {
        DiagramSettings {
            kind: DiagramKind::Pie,
            categories: vec![
                DiagramCategory { field: "pop_male".into(), colour: CATEGORY_COLORS[0] },
                DiagramCategory { field: "pop_female".into(), colour: CATEGORY_COLORS[1] },
            ],
            size: DiagramSize::Fixed(12.0),
            start_angle: 0.0,
            background: None,
            stroke: DiagramSettings::default_stroke(),
            stroke_width: 0.75,
            labels: DiagramLabel::None,
        }
    }

    fn dataset() -> Dataset {
        let mut ds = Dataset::new(
            "regions",
            GeometryType::Polygon,
            vec![Field::new("pop_male", FieldType::Integer), Field::new("pop_female", FieldType::Integer)],
        );
        ds.push(None, vec![Value::Int(52_000), Value::Int(48_000)]);
        ds.push(None, vec![Value::Int(30_000), Value::Int(45_000)]);
        ds.push(None, vec![Value::Null, Value::Int(45_000)]);
        ds.push(None, vec![Value::Null, Value::Null]);
        ds
    }

    #[test]
    fn clamping_brings_every_number_into_range() {
        let d = DiagramSettings {
            kind: DiagramKind::Bar,
            categories: (0..MAX_CATEGORIES + 4)
                .map(|i| DiagramCategory { field: format!("f{i}"), colour: CATEGORY_COLORS[0] })
                .chain(std::iter::once(DiagramCategory { field: "  ".into(), colour: CATEGORY_COLORS[1] }))
                .collect(),
            size: DiagramSize::Scaled { min_size: 500.0, max_size: -5.0, min: f64::NAN, max: f64::INFINITY },
            start_angle: 725.0,
            background: None,
            stroke: DiagramSettings::default_stroke(),
            stroke_width: 99.0,
            labels: DiagramLabel::Both,
        }
        .clamped();
        assert_eq!(d.categories.len(), MAX_CATEGORIES, "the extra categories are dropped");
        let DiagramSize::Scaled { min_size, max_size, min, max } = d.size else { panic!("still scaled") };
        assert_eq!((min_size, max_size), (MIN_SIZE, MAX_SIZE), "the bounds are brought in and swapped into order");
        assert_eq!((min, max), (0.0, 1.0), "not numbers become the neutral range");
        assert_eq!(d.start_angle, 5.0);
        assert_eq!(d.stroke_width, MAX_STROKE_WIDTH);
    }

    #[test]
    fn a_scaled_range_too_wide_to_measure_is_reset() {
        let d = DiagramSettings { size: DiagramSize::Scaled { min_size: 4.0, max_size: 36.0, min: -1e308, max: 1e308 }, ..settings() }
            .clamped();
        let DiagramSize::Scaled { min, max, .. } = d.size else { panic!("still scaled") };
        assert_eq!((min, max), (0.0, 1.0), "its span would be infinite, so the neutral range");
        assert!(d.size_for(1e308).is_finite(), "no size is not a number");
    }

    #[test]
    fn a_data_range_stored_upside_down_is_put_in_order() {
        let d =
            DiagramSettings { size: DiagramSize::Scaled { min_size: 4.0, max_size: 36.0, min: 100.0, max: 0.0 }, ..settings() }.clamped();
        let DiagramSize::Scaled { min, max, .. } = d.size else { panic!("still scaled") };
        assert_eq!((min, max), (0.0, 100.0));
        assert_eq!(d.size_for(100.0), 36.0, "the sizes still scale, not all the middle");
    }

    #[test]
    fn a_category_without_a_name_does_not_cost_one_of_the_cap() {
        let mut categories: Vec<DiagramCategory> =
            (0..MAX_CATEGORIES).map(|i| DiagramCategory { field: format!("f{i}"), colour: CATEGORY_COLORS[0] }).collect();
        categories.insert(0, DiagramCategory { field: " ".into(), colour: CATEGORY_COLORS[1] });
        let d = DiagramSettings { categories, ..settings() }.clamped();
        assert_eq!(d.categories.len(), MAX_CATEGORIES);
        assert_eq!(d.categories.last().map(|c| c.field.as_str()), Some("f15"), "the last named category is kept");
    }

    #[test]
    fn fixed_size_is_the_same_for_every_feature() {
        let d = settings();
        assert_eq!(d.size_for(0.0), 12.0);
        assert_eq!(d.size_for(1e9), 12.0);
        assert_eq!(d.max_size(), 12.0);
    }

    #[test]
    fn scaled_size_grows_by_the_square_root_of_the_share() {
        let d = DiagramSettings { size: DiagramSize::Scaled { min_size: 4.0, max_size: 36.0, min: 0.0, max: 100.0 }, ..settings() };
        assert_eq!(d.size_for(0.0), 4.0);
        assert_eq!(d.size_for(100.0), 36.0);
        assert_eq!(d.size_for(25.0), 4.0 + 32.0 * 0.5, "half the range by its square root");
        assert_eq!(d.size_for(1e9), 36.0, "past the top, the top");
        // A range of nothing is the middle, as Proportional takes it.
        let flat = DiagramSettings { size: DiagramSize::Scaled { min_size: 4.0, max_size: 36.0, min: 10.0, max: 10.0 }, ..settings() };
        assert_eq!(flat.size_for(10.0), 4.0 + 32.0 * 0.5f32.sqrt());
    }

    #[test]
    fn values_skip_missing_and_total_the_rest() {
        let ds = dataset();
        let d = settings();
        let (v, total) = d.values(&ds, 0).expect("both fields are numbers");
        assert_eq!(v.iter().map(|(x, _)| *x).collect::<Vec<_>>(), vec![52_000.0, 48_000.0]);
        assert_eq!(total, 100_000.0);
        let (v, total) = d.values(&ds, 2).expect("one field is enough");
        assert_eq!(v.len(), 1);
        assert_eq!(total, 45_000.0);
        assert!(d.values(&ds, 3).is_none(), "nothing to draw");
        assert!(d.values(&ds, 999).is_none(), "a row past the end has nothing either");
        // A field the dataset no longer has is skipped, not an error.
        let gone = DiagramSettings { categories: vec![DiagramCategory { field: "gone".into(), colour: CATEGORY_COLORS[2] }], ..settings() };
        assert!(gone.values(&ds, 0).is_none());
    }

    #[test]
    fn a_total_that_overflows_draws_nothing() {
        // Two finite values whose sum is infinite: no slice would be drawn, so none is returned.
        let mut ds = Dataset::new(
            "big",
            GeometryType::Polygon,
            vec![Field::new("pop_male", FieldType::Double), Field::new("pop_female", FieldType::Double)],
        );
        ds.push(None, vec![Value::Float(1e308), Value::Float(1e308)]);
        assert!(settings().values(&ds, 0).is_none());
    }

    #[test]
    fn legend_lists_one_row_per_category() {
        let l = settings().legend(false);
        assert_eq!(l.iter().map(|e| e.label.as_str()).collect::<Vec<_>>(), ["pop_male", "pop_female"]);
        assert_eq!(l[0].symbol.fill, CATEGORY_COLORS[0]);
        assert_eq!(l[0].symbol.stroke, Rgba::rgb(0xFF, 0xFF, 0xFF), "a white outline on a polygon or point layer");
        assert!(l.iter().all(|e| !e.fallback));
        assert_eq!(l[0].strokes[0].color, Some(CATEGORY_COLORS[0]), "its own mark, not the layer's stacked ones");
        let line = settings().legend(true);
        assert_eq!(line[0].symbol.stroke, CATEGORY_COLORS[0], "a line's swatch draws its stroke, so the colour is there");
    }

    #[test]
    fn a_diagram_round_trips_through_a_project() {
        let d = settings();
        let j = serde_json::to_value(&d).unwrap();
        let back: DiagramSettings = serde_json::from_value(j).unwrap();
        assert_eq!(back, d);
        // Kinds and sizes read by their codes; a colour as the four bytes it is elsewhere.
        let j = serde_json::json!({"kind": "bar", "categories": [{"field": "a", "colour": [74, 144, 217, 255]}], "size": {"scaled": {"min_size": 6.0, "max_size": 40.0, "min": 0.0, "max": 100.0}}, "start_angle": 90.0});
        let back: DiagramSettings = serde_json::from_value(j).unwrap();
        assert_eq!(back.kind, DiagramKind::Bar);
        assert_eq!(back.categories[0].colour, CATEGORY_COLORS[0]);
        assert!(matches!(back.size, DiagramSize::Scaled { min_size: 6.0, .. }));
        assert_eq!(back.start_angle, 90.0);
    }

    #[test]
    fn a_crafted_diagram_reads_as_none_and_the_rest_survives() {
        #[derive(Deserialize)]
        struct Read {
            #[serde(default, deserialize_with = "de_diagram")]
            diagram: Option<DiagramSettings>,
        }
        for junk in [
            serde_json::json!("nonsense"),
            serde_json::json!({"kind": "hexagon"}),
            serde_json::json!({"categories": []}),
            serde_json::json!({"size": "wide"}),
        ] {
            let v = serde_json::json!({ "diagram": junk });
            let read: Read = serde_json::from_value(v).expect("the layer still reads");
            assert!(read.diagram.is_none(), "not a diagram: {junk}");
        }
    }

    #[test]
    fn the_audit_counts_what_reading_a_diagram_changes() {
        let audit = |v: serde_json::Value| serde_json::from_value::<DiagramAudit>(v).expect("an audit always reads");
        assert_eq!(audit(serde_json::Value::Null), DiagramAudit::default(), "no diagram, nothing changed");
        assert_eq!(audit(serde_json::to_value(settings()).unwrap()), DiagramAudit::default(), "a clean diagram reads as it is");
        for angle in [360.0, 720.0, -360.0, -10.0] {
            let turned = DiagramSettings { start_angle: angle, ..settings() };
            assert_eq!(
                audit(serde_json::to_value(turned).unwrap()),
                DiagramAudit::default(),
                "{angle} names a direction, not out of range"
            );
        }
        for junk in [serde_json::json!("nonsense"), serde_json::json!({"kind": "hexagon"}), serde_json::json!({"categories": []})] {
            assert_eq!(audit(junk.clone()).dropped, 1, "dropped: {junk}");
        }
        let mut lossy = settings();
        lossy.categories = (0..MAX_CATEGORIES + 2)
            .map(|i| DiagramCategory { field: format!("f{i}"), colour: CATEGORY_COLORS[0] })
            .chain(std::iter::once(DiagramCategory { field: " ".into(), colour: CATEGORY_COLORS[0] }))
            .collect();
        lossy.categories[1].field = "F0".into();
        lossy.stroke_width = 99.0;
        let a = audit(serde_json::to_value(lossy).unwrap());
        assert_eq!(a, DiagramAudit { dropped: 0, categories_removed: 3, repeated_fields: 1, adjusted: 1 });
    }

    #[test]
    fn labels_round_trip_and_a_project_without_any_reads_as_none() {
        for mode in DiagramLabel::ALL {
            let d = DiagramSettings { labels: mode, ..settings() };
            let v = serde_json::to_value(&d).unwrap();
            // "none" isn't written: an older RustGIS reads the layer as before.
            assert_eq!(v.get("labels").is_none(), mode == DiagramLabel::None);
            let back: DiagramSettings = serde_json::from_value(v).unwrap();
            assert_eq!(back.labels, mode);
        }
        let without: DiagramSettings = serde_json::from_value(serde_json::to_value(settings()).unwrap()).unwrap();
        assert_eq!(without.labels, DiagramLabel::None);
        // Commands name a mode by its code, whatever its case.
        assert_eq!(DiagramLabel::from_code("PERCENT"), Some(DiagramLabel::Percent));
        assert_eq!(DiagramLabel::from_code("Both"), Some(DiagramLabel::Both));
        assert_eq!(DiagramLabel::from_code("nonsense"), None);
    }
}
