//! Count label-setting and diagram losses, and reference scales left out, while skipping feature
//! data and retaining no moved-label arrays or diagrams.

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
    /// The class's name as written (a missing one is "Default", as the reader takes it).
    #[serde(default)]
    name: Option<serde_json::Value>,
    #[serde(default)]
    moved: MoveCount,
    #[serde(default)]
    callout: Option<serde_json::Value>,
    #[serde(default)]
    square_to: Option<serde_json::Value>,
    #[serde(default)]
    guide: Option<serde_json::Value>,
    #[serde(default)]
    line_offset: Option<serde_json::Value>,
    #[serde(default)]
    show_all: Option<serde_json::Value>,
    #[serde(default)]
    weight: Option<serde_json::Value>,
    #[serde(default)]
    stretch: Option<serde_json::Value>,
}

#[derive(Default, Deserialize)]
struct Labels {
    /// The Default class's name as written.
    #[serde(default)]
    name: Option<serde_json::Value>,
    #[serde(default)]
    moved: MoveCount,
    #[serde(default)]
    callout: Option<serde_json::Value>,
    #[serde(default)]
    line_offset: Option<serde_json::Value>,
    #[serde(default)]
    show_all: Option<serde_json::Value>,
    #[serde(default)]
    weight: Option<serde_json::Value>,
    #[serde(default)]
    square_to: Option<serde_json::Value>,
    #[serde(default)]
    guide: Option<serde_json::Value>,
    #[serde(default)]
    stretch: Option<serde_json::Value>,
    #[serde(default)]
    classes: Vec<Class>,
}

#[derive(Deserialize)]
struct Layer {
    #[serde(default)]
    name: String,
    #[serde(default)]
    labels: Labels,
    #[serde(default)]
    reference_scale: Option<serde_json::Value>,
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

/// A distance from the line (#1187) that isn't a number (read as on the line) or is out of its
/// range (held to it); null is on the line.
fn adjusted_line_offset(value: Option<&serde_json::Value>) -> bool {
    value.is_some_and(|v| !v.is_null() && v.as_f64().is_none_or(|n| !rustgis_map::symbology::LABEL_OFFSET.contains(&n)))
}

/// A label weight (#2803) that isn't a whole number from 0 to 10 (held to it and rounded, or 5
/// when it isn't a number); null is 5.
fn adjusted_weight(value: Option<&serde_json::Value>) -> bool {
    value.is_some_and(|v| !v.is_null() && v.as_f64().is_none_or(|n| n.fract() != 0.0 || !rustgis_map::symbology::LABEL_WEIGHT.contains(&n)))
}

/// How much of its line a fitted label is spread over (#2265) that isn't a number (read as the
/// default) or is out of its range (held to it); null is the default.
fn adjusted_stretch(value: Option<&serde_json::Value>) -> bool {
    value.is_some_and(|v| {
        !v.is_null() && v.as_f64().is_none_or(|n| !n.is_finite() || !rustgis_map::symbology::LABEL_STRETCH.contains(&(n as f32)))
    })
}

/// A perpendicular label's line (#2263) that doesn't read or names no layer (dropped: labelled as
/// auto), or whose distance or side isn't one (held to its range, or away); null is none.
fn adjusted_square_to(value: Option<&serde_json::Value>) -> bool {
    value.is_some_and(|v| {
        !v.is_null()
            && (v.get("layer").and_then(serde_json::Value::as_str).is_none_or(|n| n.trim().is_empty())
                || v.get("within").is_some_and(|w| w.as_f64().is_none_or(|n| !rustgis_map::symbology::SQUARE_WITHIN.contains(&n)))
                || v.get("side").is_some_and(|s| serde_json::from_value::<rustgis_map::SquareSide>(s.clone()).is_err()))
    })
}

/// A label guide (#2262) that doesn't read, names no layer or gives a field that isn't a name:
/// dropped, its labels placed as without one; null is none.
fn dropped_guide(value: Option<&serde_json::Value>) -> bool {
    value.is_some_and(|v| {
        !v.is_null()
            && (v.get("layer").and_then(serde_json::Value::as_str).is_none_or(|n| n.trim().is_empty())
                || v.get("field").is_some_and(|f| !f.is_null() && !f.is_string()))
    })
}

/// Whether a "Show all labels" setting is something other than true or false (null is none).
fn damaged_flag(value: Option<&serde_json::Value>) -> bool {
    value.is_some_and(|v| !v.is_boolean() && !v.is_null())
}

/// What reading a layer's label classes drops or cuts (see `From<LabelingFile> for Labeling`).
#[derive(Default)]
struct ClassLosses {
    /// Class names longer than [`rustgis_map::symbology::MAX_LABEL_CLASS_NAME`], cut to it.
    cut: usize,
    /// Classes past [`rustgis_map::symbology::MAX_LABEL_CLASSES`], dropped.
    excess: usize,
    /// Classes with no name, or the name of an earlier class or of Default, dropped.
    unnamed_or_repeated: usize,
}

impl ClassLosses {
    /// Replays the reader's class rules on one layer's raw labels and adds what they drop or cut.
    fn add(&mut self, labels: &Labels) {
        use rustgis_map::symbology::{DEFAULT_LABEL_CLASS, MAX_LABEL_CLASS_NAME, MAX_LABEL_CLASSES, label_class_name};
        let raw_name = |n: &Option<serde_json::Value>| match n {
            None => DEFAULT_LABEL_CLASS.to_string(),
            Some(v) => v.as_str().unwrap_or_default().to_string(),
        };
        let mut cut = |raw: &str| {
            if raw.trim().chars().count() > MAX_LABEL_CLASS_NAME {
                self.cut = self.cut.saturating_add(1);
            }
        };
        let raw_default = raw_name(&labels.name);
        cut(&raw_default);
        let mut default = label_class_name(&raw_default);
        if default.is_empty() {
            default = DEFAULT_LABEL_CLASS.into();
        }
        let mut kept: Vec<String> = Vec::new();
        for class in &labels.classes {
            if kept.len() >= MAX_LABEL_CLASSES {
                self.excess = self.excess.saturating_add(1);
                continue;
            }
            let raw = raw_name(&class.name);
            cut(&raw);
            let name = label_class_name(&raw);
            let same = |n: &str| n.eq_ignore_ascii_case(&name);
            if name.is_empty() || same(&default) || kept.iter().any(|o| same(o)) {
                self.unnamed_or_repeated = self.unnamed_or_repeated.saturating_add(1);
            } else {
                kept.push(name);
            }
        }
    }
}

/// Count what the raw layers hold against what the read labels kept; one notice per kind, each
/// naming the item read (`name`) and what it was (`what`: "the project", "the map file", ...).
fn count<'a>(
    raw: impl Iterator<Item = &'a Layer>,
    retained: impl Iterator<Item = &'a rustgis_map::symbology::Labeling>,
    name: &str,
    what: &str,
) -> Vec<(ReadPart, String)> {
    let name = crate::project::short_name(name);
    let mut classes = ClassLosses::default();
    let (mut moves, mut callouts, mut offsets, mut flags, mut squares, mut stretches) = (0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
    let (mut guides, mut weights) = (0usize, 0usize);
    let mut diagrams = rustgis_map::diagram::DiagramAudit::default();
    for layer in raw {
        diagrams = diagrams.plus(layer.diagram);
        let labels = &layer.labels;
        classes.add(labels);
        moves = moves.saturating_add(labels.moved.0);
        callouts = callouts.saturating_add(usize::from(adjusted_callout(labels.callout.as_ref())));
        offsets = offsets.saturating_add(usize::from(adjusted_line_offset(labels.line_offset.as_ref())));
        flags = flags.saturating_add(usize::from(damaged_flag(labels.show_all.as_ref())));
        weights = weights.saturating_add(usize::from(adjusted_weight(labels.weight.as_ref())));
        squares = squares.saturating_add(usize::from(adjusted_square_to(labels.square_to.as_ref())));
        guides = guides.saturating_add(usize::from(dropped_guide(labels.guide.as_ref())));
        stretches = stretches.saturating_add(usize::from(adjusted_stretch(labels.stretch.as_ref())));
        for class in &labels.classes {
            moves = moves.saturating_add(class.moved.0);
            callouts = callouts.saturating_add(usize::from(adjusted_callout(class.callout.as_ref())));
            offsets = offsets.saturating_add(usize::from(adjusted_line_offset(class.line_offset.as_ref())));
            flags = flags.saturating_add(usize::from(damaged_flag(class.show_all.as_ref())));
            weights = weights.saturating_add(usize::from(adjusted_weight(class.weight.as_ref())));
            squares = squares.saturating_add(usize::from(adjusted_square_to(class.square_to.as_ref())));
            guides = guides.saturating_add(usize::from(dropped_guide(class.guide.as_ref())));
            stretches = stretches.saturating_add(usize::from(adjusted_stretch(class.stretch.as_ref())));
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
    if offsets > 0 {
        messages
            .push(format!("{name}: {offsets} label distance(s) from the line out of range or not a number adjusted while opening {what}"));
    }
    if flags > 0 {
        messages
            .push(format!("{name}: {flags} \"show all labels\" setting(s) that were not true or false read as off while opening {what}"));
    }
    if weights > 0 {
        messages.push(format!("{name}: {weights} label weight(s) not a whole number from 0 to 10 adjusted while opening {what}"));
    }
    if squares > 0 {
        messages.push(format!(
            "{name}: {squares} perpendicular label line setting(s) dropped (labelled as auto) or adjusted while opening {what}"
        ));
    }
    if guides > 0 {
        messages.push(format!("{name}: {guides} label guide setting(s) that didn't read dropped while opening {what}"));
    }
    if stretches > 0 {
        messages.push(format!("{name}: {stretches} label stretch setting(s) out of range or not a number adjusted while opening {what}"));
    }
    if classes.cut > 0 {
        messages.push(format!(
            "{name}: {} label class name(s) longer than {} characters cut to that length while opening {what}",
            classes.cut,
            rustgis_map::symbology::MAX_LABEL_CLASS_NAME
        ));
    }
    if classes.excess > 0 {
        messages.push(format!(
            "{name}: {} label class(es) past the {} a layer holds dropped while opening {what}",
            classes.excess,
            rustgis_map::symbology::MAX_LABEL_CLASSES
        ));
    }
    if classes.unnamed_or_repeated > 0 {
        messages.push(format!(
            "{name}: {} label class(es) with no name, or the name of an earlier class or of Default, dropped while opening {what}",
            classes.unnamed_or_repeated
        ));
    }
    let mut messages: Vec<(ReadPart, String)> = messages.into_iter().map(|m| (ReadPart::Labels, m)).collect();
    let mut found = Vec::new();
    diagram_messages(diagrams, &name, what, &mut found);
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

/// The layers a notice names; the rest are counted.
const MAX_NAMED: usize = 3;

/// The first few layer names, quoted, and how many more.
fn names(names: &[&String]) -> String {
    let shown: Vec<String> = names.iter().take(MAX_NAMED).map(|n| format!("'{}'", crate::project::short_name(n))).collect();
    match names.len().saturating_sub(MAX_NAMED) {
        0 => shown.join(", "),
        more => format!("{} and {more} more", shown.join(", ")),
    }
}

/// One notice with a count and the first few names, for the raw layers whose saved reference
/// scale is out of [`rustgis_map::REFERENCE_SCALES`], which reading the layer leaves out.
fn refused_reference_scales<'a>(raw: impl Iterator<Item = &'a Layer>) -> Vec<(ReadPart, String)> {
    let (least, most) = rustgis_map::REFERENCE_SCALES;
    let refused: Vec<&String> = raw
        .filter(|l| l.reference_scale.as_ref().and_then(serde_json::Value::as_f64).is_some_and(|r| !(least..=most).contains(&r)))
        .map(|l| &l.name)
        .collect();
    if refused.is_empty() {
        return Vec::new();
    }
    vec![(
        ReadPart::Maps,
        format!(
            "{} layer(s) with a reference scale out of range (1:{least} to 1:{most}) opened without one ({})",
            refused.len(),
            names(&refused)
        ),
    )]
}

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
        notices.extend(refused_reference_scales(file.maps.iter().take(project.maps.len()).flat_map(|m| &m.layers)));
        notices.extend(hidden(project.maps.iter().flat_map(|m| &m.layers)));
        notices.extend(unfollowed_guides(project.maps.iter()));
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
    notices.extend(refused_reference_scales(file.maps.iter().chain(file.offline_areas.iter().map(|a| &a.data)).flat_map(|m| &m.layers)));
    // Offline snapshots say so when `offline.open` opens them as maps.
    notices.extend(hidden(project.maps.iter().flat_map(|m| &m.layers)));
    notices.extend(unfollowed_guides(project.maps.iter()));
    Ok(notices)
}

/// The same audit for a map file read on its own; unreadable label settings give no notice
/// (the map itself was read, so its labels were too). Diagram and reference scale notices only
/// with `diagrams` (an import that takes the layers whole, not only their styles).
pub(crate) fn map_notices(text: &str, map: &rustgis_map::Map, diagrams: bool) -> Vec<String> {
    // The layers' own state still says whether a diagram is hidden when the audit can't read them.
    let Ok(raw) = serde_json::from_str::<Map>(text) else { return only(hidden(map.layers.iter()), diagrams) };
    let mut notices = count(raw.layers.iter(), map.layers.iter().map(|l| &l.labels), &map.name, "the map file");
    if diagrams {
        notices.extend(refused_reference_scales(raw.layers.iter()));
    }
    notices.extend(hidden(map.layers.iter()));
    notices.extend(unfollowed_guides(std::iter::once(map)));
    only(notices, diagrams)
}

/// One notice a kind, with a count and the first few names, for the label guides (#2262) of a file
/// that its labels can't follow as written, which `labels.placement` refuses when they are set:
/// along a layer read in place from its file (only a view's window of it at hand), or one that
/// isn't there or has no lines (placed as without it), by a field one of the two layers lacks
/// (matched by place, not by value), or on a perpendicular label (square to its line where one is
/// near).
fn unfollowed_guides<'a>(maps: impl Iterator<Item = &'a rustgis_map::Map>) -> Vec<(ReadPart, String)> {
    use rustgis_data::GeometryType;
    let (mut streamed, mut missing, mut fields, mut square) = (Vec::new(), Vec::new(), Vec::new(), 0usize);
    for map in maps {
        for labelled in &map.layers {
            for (c, g) in labelled.labels.in_order().filter_map(|c| Some((c, c.guide.as_ref()?))) {
                match map.layer_by_name(&g.layer) {
                    Some(l) if l.source.is_some() => streamed.push(&g.layer),
                    Some(l) if l.has_geometry(GeometryType::Polyline) => {
                        if let Some(f) = g.field.as_ref().filter(|f| [l, labelled].iter().any(|l| l.dataset.field_index(f).is_none())) {
                            fields.push(f);
                        }
                    }
                    _ => missing.push(&g.layer),
                }
                square += usize::from(c.placement == rustgis_map::LabelPlacement::Perpendicular);
            }
        }
    }
    let named = |mut v: Vec<&String>| {
        let n = v.len();
        v.sort_unstable();
        v.dedup();
        (n, names(&v))
    };
    let mut notices = Vec::new();
    if !streamed.is_empty() {
        let (n, names) = named(streamed);
        notices.push(format!(
            "{n} label guide(s) along {names}, read in place from its file, are placed as without it: Load All Features (layer.loadAll) to guide them"
        ));
    }
    if !missing.is_empty() {
        let (n, names) = named(missing);
        notices.push(format!("{n} label guide(s) along {names}, not a line layer of the map, are placed as without it"));
    }
    if !fields.is_empty() {
        let (n, names) = named(fields);
        notices.push(format!(
            "{n} label guide field(s) {names} that aren't a field of both layers: matched to guide lines by place, not by value"
        ));
    }
    if square > 0 {
        notices.push(format!(
            "{square} perpendicular label class(es) with a guide: square to their line where one is near, guided only where none is"
        ));
    }
    notices.into_iter().map(|m| (ReadPart::Labels, m)).collect()
}

/// The same audit for a layer file read on its own.
pub(crate) fn layer_notices(text: &str, layer: &rustgis_map::Layer, diagrams: bool) -> Vec<String> {
    let Ok(raw) = serde_json::from_str::<Layer>(text) else { return only(hidden(std::iter::once(layer)), diagrams) };
    let mut notices = count(std::iter::once(&raw), std::iter::once(&layer.labels), &layer.name, "the layer file");
    if diagrams {
        notices.extend(refused_reference_scales(std::iter::once(&raw)));
    }
    notices.extend(hidden(std::iter::once(layer)));
    only(notices, diagrams)
}
