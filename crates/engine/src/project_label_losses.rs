//! Count label-setting and diagram losses while skipping feature data and retaining no
//! moved-label arrays or diagrams.

use serde::Deserialize;

use crate::Project;
use crate::project::ReadPart;

#[derive(Default)]
struct MoveCount(usize);

impl<'de> Deserialize<'de> for MoveCount {
    /// Counts raw records without retaining their values, including damaged non-array settings.
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Count;
        impl<'de> serde::de::Visitor<'de> for Count {
            type Value = MoveCount;
            /// Describes the optional project setting consumed by this visitor.
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("moved-label settings")
            }
            /// Counts entries using IgnoredAny so even an oversized list has bounded memory.
            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut count = 0usize;
                while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {
                    count = count.saturating_add(1);
                }
                Ok(MoveCount(count))
            }
            /// Skips a damaged object and counts its discarded setting once.
            fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                while map.next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?.is_some() {}
                Ok(MoveCount(1))
            }
            /// A boolean is one discarded move setting.
            fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(MoveCount(1))
            }
            /// A signed number is one discarded move setting.
            fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(MoveCount(1))
            }
            /// An unsigned number is one discarded move setting.
            fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(MoveCount(1))
            }
            /// A fractional number is one discarded move setting.
            fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(MoveCount(1))
            }
            /// Text is one discarded move setting.
            fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(MoveCount(1))
            }
            /// Null expresses no moved-label records and causes no loss notice.
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(MoveCount(0))
            }
        }
        d.deserialize_any(Count)
    }
}

#[derive(Default, Deserialize)]
struct Class {
    #[serde(default)]
    moved: MoveCount,
    #[serde(default)]
    callout: Option<serde_json::Value>,
    #[serde(default)]
    show_all: Option<serde_json::Value>,
}

#[derive(Default, Deserialize)]
struct Labels {
    #[serde(default)]
    moved: MoveCount,
    #[serde(default)]
    callout: Option<serde_json::Value>,
    #[serde(default)]
    show_all: Option<serde_json::Value>,
    #[serde(default)]
    classes: Vec<Class>,
}

#[derive(Deserialize)]
struct Layer {
    #[serde(default)]
    labels: Labels,
    #[serde(default)]
    diagram: rustgis_map::diagram::DiagramAudit,
}

#[derive(Default, Deserialize)]
struct Map {
    #[serde(default)]
    layers: Vec<Layer>,
}

#[derive(Deserialize)]
struct Offline {
    data: Map,
}

#[derive(Deserialize)]
struct File {
    maps: Vec<Map>,
    #[serde(default)]
    offline_areas: Vec<Offline>,
    #[serde(default)]
    styles: Vec<Layer>,
}

/// Zero, false and null explicitly disable callouts; other lengths must fit the supported range.
fn adjusted_callout(value: Option<&serde_json::Value>) -> bool {
    value.is_some_and(|v| {
        !v.is_null()
            && v.as_bool() != Some(false)
            && v.as_f64().is_none_or(|n| n != 0.0 && !rustgis_map::symbology::CALLOUT_REACH.contains(&n))
    })
}

/// Whether a "Show all labels" setting is something other than true or false (null is none).
fn damaged_flag(value: Option<&serde_json::Value>) -> bool {
    value.is_some_and(|v| !v.is_boolean() && !v.is_null())
}

/// Count what the raw layers hold against what the read labels kept; one notice per kind, each
/// naming the item read (`name`) and what it was (`what`: "the project", "the map file", ...).
fn count<'a>(
    raw: impl Iterator<Item = &'a Layer>,
    retained: impl Iterator<Item = &'a rustgis_map::symbology::Labeling>,
    name: &str,
    what: &str,
) -> Vec<(ReadPart, String)> {
    let (mut moves, mut callouts, mut flags) = (0usize, 0usize, 0usize);
    let mut diagrams = rustgis_map::diagram::DiagramAudit::default();
    for layer in raw {
        diagrams = diagrams.plus(layer.diagram);
        let labels = &layer.labels;
        moves = moves.saturating_add(labels.moved.0);
        callouts = callouts.saturating_add(usize::from(adjusted_callout(labels.callout.as_ref())));
        flags = flags.saturating_add(usize::from(damaged_flag(labels.show_all.as_ref())));
        for class in &labels.classes {
            moves = moves.saturating_add(class.moved.0);
            callouts = callouts.saturating_add(usize::from(adjusted_callout(class.callout.as_ref())));
            flags = flags.saturating_add(usize::from(damaged_flag(class.show_all.as_ref())));
        }
    }
    let retained = retained.flat_map(|labels| labels.in_order()).map(|c| c.moved.len()).fold(0usize, usize::saturating_add);
    let dropped = moves.saturating_sub(retained);
    let mut messages = Vec::new();
    if dropped > 0 {
        messages.push(format!("{name}: {dropped} invalid, duplicate or excess moved-label entry/setting(s) ignored while opening {what}"));
    }
    if callouts > 0 {
        messages.push(format!("{name}: {callouts} callout length setting(s) adjusted or disabled while opening {what}"));
    }
    if flags > 0 {
        messages
            .push(format!("{name}: {flags} \"show all labels\" setting(s) that were not true or false read as off while opening {what}"));
    }
    let mut messages: Vec<(ReadPart, String)> = messages.into_iter().map(|m| (ReadPart::Labels, m)).collect();
    let mut found = Vec::new();
    diagram_messages(diagrams, name, what, &mut found);
    messages.extend(found.into_iter().map(|m| (ReadPart::Diagrams, m)));
    messages
}

/// One notice per kind (aggregated points, extruded polygons) with a count and the first few layer
/// names, for the layers read whose diagram won't show (#1596), as `layer.diagram` says when it
/// is set.
pub(crate) fn hidden<'a>(layers: impl Iterator<Item = &'a rustgis_map::Layer>) -> Vec<(ReadPart, String)> {
    let (mut binned, mut extruded) = (Vec::new(), Vec::new());
    for l in layers.filter(|l| crate::cmd::diagram::hidden_text(l).is_some()) {
        if l.geometry_type() == rustgis_data::GeometryType::Point { binned.push(&l.name) } else { extruded.push(&l.name) }
    }
    let names = |names: &[&String]| {
        let shown: Vec<String> = names.iter().take(MAX_NAMED).map(|n| format!("'{}'", crate::project::short_name(n))).collect();
        match names.len().saturating_sub(MAX_NAMED) {
            0 => shown.join(", "),
            more => format!("{} and {more} more", shown.join(", ")),
        }
    };
    let mut notices = Vec::new();
    if !binned.is_empty() {
        notices.push(format!(
            "{} layer(s) ({}): the diagram doesn't show while the points are aggregated into bins; turn Aggregation off to see it",
            binned.len(),
            names(&binned)
        ));
    }
    if !extruded.is_empty() {
        notices.push(format!(
            "{} layer(s) ({}): while the polygons are extruded, the diagram doesn't show when a flat map is tilted (it shows untilted, and on the globe)",
            extruded.len(),
            names(&extruded)
        ));
    }
    notices.into_iter().map(|m| (ReadPart::HiddenDiagrams, m)).collect()
}

/// The layers a hidden-diagram notice names; the rest are counted.
const MAX_NAMED: usize = 3;

/// The notices of the parts asked for.
fn only(notices: Vec<(ReadPart, String)>, diagrams: bool) -> Vec<String> {
    notices
        .into_iter()
        .filter(|(part, _)| diagrams || !matches!(part, ReadPart::Diagrams | ReadPart::HiddenDiagrams))
        .map(|(_, m)| m)
        .collect()
}

/// One notice per kind of change reading the layers' diagrams made.
fn diagram_messages(d: rustgis_map::diagram::DiagramAudit, name: &str, what: &str, messages: &mut Vec<String>) {
    let name = crate::project::short_name(name);
    if d.dropped > 0 {
        messages.push(format!("{name}: {} diagram(s) that could not be read or named no field removed while opening {what}", d.dropped));
    }
    if d.categories_removed > 0 {
        messages.push(format!(
            "{name}: {} diagram category(ies) with a blank field name or beyond the {} a diagram holds removed while opening {what}",
            d.categories_removed,
            rustgis_map::diagram::MAX_CATEGORIES
        ));
    }
    if d.repeated_fields > 0 {
        messages.push(format!(
            "{name}: {} diagram category(ies) repeating an earlier field kept while opening {what}; each draws its own slice or bar",
            d.repeated_fields
        ));
    }
    if d.adjusted > 0 {
        messages.push(format!(
            "{name}: {} diagram(s) with a size, data range or outline width out of range or out of order adjusted while opening {what}",
            d.adjusted
        ));
    }
}

/// Compare raw record counts with retained state, including offline snapshots; one notice per kind.
pub(super) fn notices_for_maps(text: &str, project: &Project, maps_only: bool) -> Result<Vec<(ReadPart, String)>, String> {
    let file: File = serde_json::from_str(text).map_err(|e| format!("could not read project label settings: {e}"))?;
    if maps_only {
        let raw = file.maps.iter().take(project.maps.len()).flat_map(|m| &m.layers);
        let retained = project.maps.iter().flat_map(|m| &m.layers).map(|l| &l.labels);
        let mut notices = count(raw, retained, &project.name, "the imported maps");
        notices.extend(hidden(project.maps.iter().flat_map(|m| &m.layers)));
        return Ok(notices);
    }
    let raw = file.maps.iter().chain(file.offline_areas.iter().map(|a| &a.data)).flat_map(|m| &m.layers).chain(file.styles.iter());
    let retained = project
        .maps
        .iter()
        .chain(project.offline_areas.iter().map(|a| &a.data))
        .flat_map(|m| &m.layers)
        .map(|l| &l.labels)
        .chain(project.styles.iter().map(|s| &s.labels));
    let mut notices = count(raw, retained, &project.name, "the project");
    // Offline snapshots say so when `offline.open` opens them as maps.
    notices.extend(hidden(project.maps.iter().flat_map(|m| &m.layers)));
    Ok(notices)
}

/// The same audit for a map file read on its own; unreadable label settings give no notice
/// (the map itself was read, so its labels were too). Diagram notices only with `diagrams`
/// (an import that takes the layers' diagrams).
pub(crate) fn map_notices(text: &str, map: &rustgis_map::Map, diagrams: bool) -> Vec<String> {
    // The layers' own state still says whether a diagram is hidden when the audit can't read them.
    let Ok(raw) = serde_json::from_str::<Map>(text) else { return only(hidden(map.layers.iter()), diagrams) };
    let mut notices = count(raw.layers.iter(), map.layers.iter().map(|l| &l.labels), &map.name, "the map file");
    notices.extend(hidden(map.layers.iter()));
    only(notices, diagrams)
}

/// The same audit for a layer file read on its own.
pub(crate) fn layer_notices(text: &str, layer: &rustgis_map::Layer, diagrams: bool) -> Vec<String> {
    let Ok(raw) = serde_json::from_str::<Layer>(text) else { return only(hidden(std::iter::once(layer)), diagrams) };
    let mut notices = count(std::iter::once(&raw), std::iter::once(&layer.labels), &layer.name, "the layer file");
    notices.extend(hidden(std::iter::once(layer)));
    only(notices, diagrams)
}
