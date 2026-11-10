//! Data design: rules that keep a layer's attributes valid (domains, subtypes, attribute rules,
//! contingent values), an edit archive, feature templates, and the map's relationships, snapping,
//! topology rules and edit settings. The engine enforces them on every edit.

use rustgis_data::{Feature, Value};
use serde::{Deserialize, Serialize};

/// The values a field may take.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum DomainKind {
    /// One of a list of codes, each with a description.
    Coded(Vec<(Value, String)>),
    /// A number between two limits (inclusive).
    Range { min: f64, max: f64 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Domain {
    pub field: String,
    pub kind: DomainKind,
}

impl Domain {
    /// Whether a value is allowed (empty values always are).
    pub fn allows(&self, v: &Value) -> bool {
        if v.is_null() {
            return true;
        }
        match &self.kind {
            DomainKind::Coded(codes) => codes.iter().any(|(c, _)| c == v || c.to_string() == v.to_string()),
            DomainKind::Range { min, max } => v.as_f64().is_some_and(|x| x >= *min && x <= *max),
        }
    }
    /// The description of a coded value.
    pub fn describe(&self, v: &Value) -> Option<&str> {
        match &self.kind {
            DomainKind::Coded(codes) => codes.iter().find(|(c, _)| c.to_string() == v.to_string()).map(|(_, d)| d.as_str()),
            DomainKind::Range { .. } => None,
        }
    }
}

/// One subtype: a code of the subtype field, its name and its default values.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Subtype {
    pub code: i64,
    pub name: String,
    #[serde(default)]
    pub defaults: Vec<(String, Value)>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Subtypes {
    /// The integer field holding each feature's subtype code.
    pub field: String,
    pub subtypes: Vec<Subtype>,
}

/// What an attribute rule does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleKind {
    /// Sets `field` from `expression` (the Calculate Field calculator) on every insert and update.
    Calculation,
    /// Rejects edits whose features don't satisfy `expression` (a SQL where clause).
    Constraint,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AttributeRule {
    pub name: String,
    pub kind: RuleKind,
    #[serde(default)]
    pub field: String,
    pub expression: String,
}

/// Valid combinations of values across a group of fields.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContingentSet {
    pub name: String,
    pub fields: Vec<String>,
    /// Each allowed combination, one value per field (empty values match anything).
    pub combos: Vec<Vec<Value>>,
}

/// One archived edit: the feature before and after (by feature id), at a time (seconds since the
/// Unix epoch).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ArchiveEntry {
    pub time: f64,
    pub id: u64,
    pub before: Option<Feature>,
    pub after: Option<Feature>,
}

/// A feature template: a named set of default values for new features.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FeatureTemplate {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub defaults: Vec<(String, Value)>,
}

/// A layer's data design.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Design {
    #[serde(default)]
    pub domains: Vec<Domain>,
    #[serde(default)]
    pub subtypes: Option<Subtypes>,
    #[serde(default)]
    pub rules: Vec<AttributeRule>,
    #[serde(default)]
    pub contingent: Vec<ContingentSet>,
    /// Set when archiving is on: every edit, oldest first.
    #[serde(default)]
    pub archive: Option<Vec<ArchiveEntry>>,
    #[serde(default)]
    pub templates: Vec<FeatureTemplate>,
}

impl Design {
    /// Counts typed values written as Null in the saved design, including archived feature
    /// snapshots, template/subtype defaults, domain codes and contingent combinations.
    pub fn unsaved_numbers(&self) -> usize {
        let mut count = 0;
        for domain in &self.domains {
            if let DomainKind::Coded(codes) = &domain.kind {
                count += codes.iter().filter(|(value, _)| value.is_non_finite()).count();
            }
        }
        if let Some(subtypes) = &self.subtypes {
            count += subtypes.subtypes.iter().flat_map(|subtype| &subtype.defaults).filter(|(_, value)| value.is_non_finite()).count();
        }
        count += self.contingent.iter().flat_map(|set| &set.combos).flatten().filter(|value| value.is_non_finite()).count();
        count += self.templates.iter().flat_map(|template| &template.defaults).filter(|(_, value)| value.is_non_finite()).count();
        count += self
            .archive
            .iter()
            .flatten()
            .flat_map(|entry| [&entry.before, &entry.after])
            .flatten()
            .flat_map(|feature| &feature.attrs)
            .filter(|value| value.is_non_finite())
            .count();
        count
    }

    pub fn is_empty(&self) -> bool {
        *self == Design::default()
    }
    pub fn domain(&self, field: &str) -> Option<&Domain> {
        self.domains.iter().find(|d| d.field.eq_ignore_ascii_case(field))
    }
}

/// A relationship: rows of the origin layer relate to rows of the destination layer whose key
/// field holds the same value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Relationship {
    pub name: String,
    pub origin: String,
    pub origin_key: String,
    pub destination: String,
    pub destination_key: String,
}

/// Where sketches snap to.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Snapping {
    pub enabled: bool,
    pub tolerance_px: f64,
    pub vertex: bool,
    pub edge: bool,
    pub end: bool,
}

impl Default for Snapping {
    fn default() -> Self {
        Snapping { enabled: false, tolerance_px: 10.0, vertex: true, edge: true, end: true }
    }
}

/// A topology rule between layers (by name).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum TopologyRule {
    /// Polygons of a layer must not overlap each other.
    MustNotOverlap { layer: String },
    /// Polygons of a layer must not leave gaps between them.
    MustNotHaveGaps { layer: String },
    /// Lines and polygon outlines must not cross themselves.
    MustNotSelfIntersect { layer: String },
    /// Line ends must touch another line.
    MustNotHaveDangles { layer: String },
    /// Points must fall inside a polygon of another layer.
    MustBeCoveredBy { layer: String, by: String },
}

impl TopologyRule {
    pub fn label(&self) -> &'static str {
        match self {
            TopologyRule::MustNotOverlap { .. } => "Must Not Overlap",
            TopologyRule::MustNotHaveGaps { .. } => "Must Not Have Gaps",
            TopologyRule::MustNotSelfIntersect { .. } => "Must Not Self-Intersect",
            TopologyRule::MustNotHaveDangles { .. } => "Must Not Have Dangles",
            TopologyRule::MustBeCoveredBy { .. } => "Must Be Covered By",
        }
    }
    pub fn layer(&self) -> &str {
        match self {
            TopologyRule::MustNotOverlap { layer }
            | TopologyRule::MustNotHaveGaps { layer }
            | TopologyRule::MustNotSelfIntersect { layer }
            | TopologyRule::MustNotHaveDangles { layer }
            | TopologyRule::MustBeCoveredBy { layer, .. } => layer,
        }
    }
}

/// Where new features get their elevation (stored in a `z` field).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum Elevation {
    #[default]
    None,
    Constant(f64),
    /// Sampled from a raster layer (a DEM) at each new feature.
    Surface(String),
}

/// Map-wide editing settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EditSettings {
    /// Editing is on (off makes the map read-only).
    pub enabled: bool,
    /// Show the vertices of selected features.
    pub show_vertices: bool,
    pub elevation: Elevation,
    /// Ground to grid correction: (scale factor, rotation in degrees), when on.
    pub ground_to_grid: Option<(f64, f64)>,
}

impl Default for EditSettings {
    fn default() -> Self {
        EditSettings { enabled: true, show_vertices: true, elevation: Elevation::None, ground_to_grid: None }
    }
}

/// The map's editing setup: relationships, snapping, topology and settings.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MapEditing {
    #[serde(default)]
    pub relationships: Vec<Relationship>,
    #[serde(default)]
    pub snapping: Snapping,
    #[serde(default)]
    pub topology: Vec<TopologyRule>,
    #[serde(default)]
    pub settings: EditSettings,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domains_allow_and_describe() {
        let coded =
            Domain { field: "kind".into(), kind: DomainKind::Coded(vec![(Value::Int(1), "Road".into()), (Value::Int(2), "Rail".into())]) };
        assert!(coded.allows(&Value::Int(2)) && coded.allows(&Value::Null) && !coded.allows(&Value::Int(3)));
        assert_eq!(coded.describe(&Value::Int(1)), Some("Road"));
        let range = Domain { field: "speed".into(), kind: DomainKind::Range { min: 0.0, max: 130.0 } };
        assert!(range.allows(&Value::Float(90.0)) && !range.allows(&Value::Float(200.0)) && !range.allows(&Value::Text("fast".into())));
    }
}
