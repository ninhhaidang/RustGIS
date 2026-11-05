//! The Diagram commands (#976): a small chart drawn on each feature from several numeric fields,
//! as ArcGIS Pro's Chart symbology and QGIS's Diagrams draw. Part of Style › Drawing, alongside
//! the renderer commands in `layer` (a diagram goes on top of the renderer's own symbol).

use rustgis_data::GeometryType;
use rustgis_map::diagram::{CATEGORY_COLORS, MAX_CATEGORIES, MAX_SIZE, MAX_STROKE_WIDTH, MIN_SIZE};
use rustgis_map::{DiagramCategory, DiagramKind, DiagramLabel, DiagramSettings, DiagramSize, Rgba};
use serde_json::{Value, json};

use super::layer::with;
use super::{f, need_feature_layer, ps};
use crate::{CmdResult, CommandSpec as C, Session};

/// A size asked for by hand: one in points, or two it runs between as the categories' sum grows.
enum Asked {
    Fixed(f32),
    Scaled(f32, f32),
}

pub fn commands() -> Vec<C> {
    vec![
        C::new("layer.diagram", "Set Diagram", "Style › Drawing", set)
            .when(need_feature_layer)
            .params(
                r##"{"layer"?, "kind": "pie"|"bar", "fields": [numeric field names, or {"field": name, "color"?: "#RRGGBB"|"#RRGGBBAA"}] (a slice or bar each, in order; colours default to the palette new layers take), "size"?: points (fixed: a pie's radius or a bar chart's height, the same for every feature) | {"from"?: points (default 6), "to"?: points (default 40)} (grown with the categories' sum, square-root scaled as Proportional symbols are; the default), "startAngle"?: degrees clockwise from north (a pie's first slice; default 0), "labels"?: "none"|"value"|"percent"|"both" (what each slice or bar says beside it: its value in the legend's number format, its share of the total in percent, or both; default "none"), "background"?: "#RRGGBB"|"#RRGGBBAA"|null (the disc or box behind the chart; default none), "outline"?: {"color"?: "#RRGGBB"|"#RRGGBBAA" (default #4D4D4D), "width"?: points (default 0.75)}} (drawn on each feature over its symbol)"##,
            ),
        C::new("layer.clearDiagram", "Clear Diagram", "Style › Drawing", |s, p| with(s, p, |l| l.diagram = None))
            .when(need_feature_layer)
            .params(r#"{"layer"?: name|id (removes the layer's diagram; the renderer is untouched)}"#),
        C::new("layer.diagramInfo", "Diagram Settings", "Style › Drawing", info)
            .view()
            .when(need_feature_layer)
            .params(r#"{"layer"?: name|id} → {"kind", "fields": [{"field", "color"}], "size": points (fixed) | {"from", "to", "min", "max"} (scaled; min and max are the range of sums it was measured over), "startAngle"?, "labels", "background"?, "outline"} or null without one (layer.diagram takes it back as it is)"#),
    ]
}

/// Sets the layer's diagram (`layer.diagram`).
fn set(s: &mut Session, p: &Value) -> CmdResult {
    let kind = ps(p, "kind").and_then(DiagramKind::from_code).ok_or("kind is \"pie\" or \"bar\"")?;
    let list = match p.get("fields") {
        Some(Value::Array(v)) => v,
        _ => return Err("fields is required: a list of numeric field names, or of {\"field\", \"color\"?}".into()),
    };
    if list.is_empty() || list.len() > MAX_CATEGORIES {
        return Err(format!("fields lists 1 to {MAX_CATEGORIES} fields"));
    }
    // The categories asked for, their colours optional (the palette's turn otherwise).
    let mut asked: Vec<(String, Option<Rgba>)> = Vec::with_capacity(list.len());
    for item in list {
        let (field, colour) = match item {
            Value::String(f) if !f.trim().is_empty() => (f.trim().to_string(), None),
            Value::Object(o) => {
                let field = o
                    .get("field")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|f| !f.is_empty())
                    .ok_or("each field is a name, or {\"field\": name, \"color\"?: \"#RRGGBB\"}")?
                    .to_string();
                let colour = match o.get("color") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(c)) => Some(Rgba::from_hex(c).ok_or("color must be #RRGGBB or #RRGGBBAA")?),
                    Some(_) => return Err("color must be #RRGGBB or #RRGGBBAA".into()),
                };
                (field, colour)
            }
            _ => return Err("each field is a name, or {\"field\": name, \"color\"?: \"#RRGGBB\"}".into()),
        };
        // A field listed twice would draw two equal slices, halving each share.
        if asked.iter().any(|(f, _)| f.eq_ignore_ascii_case(&field)) {
            return Err(format!("'{field}' is listed twice: each field is one slice or bar"));
        }
        asked.push((field, colour));
    }
    let size = match p.get("size") {
        None | Some(Value::Null) => Asked::Scaled(6.0, 40.0),
        Some(v @ Value::Object(o)) => {
            // `min` and `max` are the data range layer.diagramInfo reads back: recomputed here, so
            // its output can be sent back as it is. Any other key is a mistake, not the default.
            if let Some(key) = o.keys().find(|k| !matches!(k.as_str(), "from" | "to" | "min" | "max")) {
                return Err(format!("size takes from and to in points, not '{key}'"));
            }
            // A bound given must be a number (or a numeric string, as `f` reads them) from 1 to 200
            // points: a bad one is an error, not the default or the nearest bound.
            let bound = |key: &str, default: f32| match v.get(key) {
                None | Some(Value::Null) => Ok(default),
                Some(_) => f(v, key)
                    .filter(|x| (f64::from(MIN_SIZE)..=f64::from(MAX_SIZE)).contains(x))
                    .map(|x| x as f32)
                    .ok_or("size's from and to are points from 1 to 200"),
            };
            let (from, to) = (bound("from", 6.0)?, bound("to", 40.0)?);
            if from > to {
                return Err("size's from is no more than its to".into());
            }
            Asked::Scaled(from, to)
        }
        Some(_) => Asked::Fixed(
            f(p, "size")
                .filter(|v| *v >= f64::from(MIN_SIZE) && *v <= f64::from(MAX_SIZE))
                .ok_or("size is points from 1 to 200 (a fixed size), or {\"from\": points, \"to\": points} (grown with the sum)")?
                as f32,
        ),
    };
    let start_angle = match p.get("startAngle") {
        None | Some(Value::Null) => 0.0,
        Some(_) => f(p, "startAngle").ok_or("startAngle is degrees")?.rem_euclid(360.0) as f32,
    };
    let background = match p.get("background") {
        None | Some(Value::Null) => None,
        Some(Value::String(c)) => Some(Rgba::from_hex(c).ok_or("background must be #RRGGBB or #RRGGBBAA")?),
        Some(_) => return Err("background is #RRGGBB or #RRGGBBAA, or null for none".into()),
    };
    let (stroke, stroke_width) = match p.get("outline") {
        None | Some(Value::Null) => (DiagramSettings::default_stroke(), DiagramSettings::default_stroke_width()),
        Some(v @ Value::Object(_)) => (
            match v.get("color") {
                None | Some(Value::Null) => DiagramSettings::default_stroke(),
                Some(Value::String(c)) => Rgba::from_hex(c).ok_or("outline's color must be #RRGGBB or #RRGGBBAA")?,
                Some(_) => return Err("outline's color must be #RRGGBB or #RRGGBBAA".into()),
            },
            match v.get("width") {
                None | Some(Value::Null) => DiagramSettings::default_stroke_width(),
                Some(_) => f(v, "width")
                    .filter(|w| (0.0..=f64::from(MAX_STROKE_WIDTH)).contains(w))
                    .ok_or("outline's width is points from 0 to 8")? as f32,
            },
        ),
        Some(_) => return Err("outline is {\"color\"?, \"width\"?: points}".into()),
    };
    let id = s.layer_id(p)?;
    let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
    if l.graphics {
        return Err("this works on feature layers, not graphics layers".into());
    }
    if matches!(l.dataset.geometry_type, GeometryType::None) {
        return Err("this works on layers with geometry".into());
    }
    // A streamed layer holds the features of the view only, so a range of sums taken from them
    // would be the view's, not the layer's: it needs the whole layer loaded, or a fixed size.
    if l.is_streamed() && matches!(size, Asked::Scaled(..)) {
        return Err("a size scaled by the sum needs all of a streamed layer: layer.loadAll first, or a fixed size".into());
    }
    // Every field must be there, and hold numbers.
    for (field, _) in &asked {
        let fi = l.dataset.field_index(field).ok_or_else(|| format!("no field named '{field}'"))?;
        if !l.dataset.fields.get(fi).is_some_and(|fd| fd.ty.is_numeric()) {
            return Err(format!("'{field}' is not a numeric field"));
        }
    }
    let categories: Vec<DiagramCategory> = asked
        .into_iter()
        .enumerate()
        .map(|(i, (field, colour))| DiagramCategory { field, colour: colour.unwrap_or(CATEGORY_COLORS[i % CATEGORY_COLORS.len()]) })
        .collect();
    // A value a chart cannot draw (zero, negative, or not a number; a null is just missing) is left
    // out of its feature's chart: say how many, and in which fields, in one notice.
    let streamed = l.is_streamed();
    let mut skipped_total = 0;
    let mut skipped_fields: Vec<String> = Vec::new();
    for c in &categories {
        let Some(fi) = l.dataset.field_index(&c.field) else { continue };
        let skipped = (0..l.dataset.features.len())
            .filter(|&row| {
                let v = l.dataset.value(row, fi);
                // As `values_at` draws them: a positive, finite number (an infinite one is dropped).
                !v.is_null() && !v.as_f64().is_some_and(|x| x.is_finite() && x > 0.0)
            })
            .count();
        if skipped > 0 {
            skipped_total += skipped;
            skipped_fields.push(format!("'{}'", c.field));
        }
    }
    // What each slice or bar says beside it: "none" (the default), "value", "percent" or "both".
    let labels = match p.get("labels") {
        None | Some(Value::Null) => DiagramLabel::None,
        Some(Value::String(code)) => DiagramLabel::from_code(code).ok_or("labels is \"none\", \"value\", \"percent\" or \"both\"")?,
        Some(_) => return Err("labels is \"none\", \"value\", \"percent\" or \"both\"".into()),
    };
    // A scaled size ranges over the sums of the features that have any (kept in the settings, as
    // Proportional keeps its range); without any, the neutral range, which draws mid-size.
    let size = match size {
        Asked::Fixed(s) => DiagramSize::Fixed(s),
        Asked::Scaled(min_size, max_size) => {
            let probe = DiagramSettings {
                kind,
                categories: categories.clone(),
                size: DiagramSize::Fixed(1.0),
                start_angle,
                background,
                stroke,
                stroke_width,
                labels,
            };
            let (min, max) = sum_range(&probe, &l.dataset);
            DiagramSize::Scaled { min_size, max_size, min, max }
        }
    };
    l.diagram = Some(DiagramSettings { kind, categories, size, start_angle, background, stroke, stroke_width, labels }.clamped());
    let out = json!({
        "kind": l.diagram.as_ref().map(|d| d.kind.label().to_lowercase()),
        "fields": l.diagram.iter().flat_map(|d| d.categories.iter().map(|c| json!({"field": c.field, "color": c.colour.hex()}))).collect::<Vec<_>>(),
    });
    // A range of sums given by hand is measured from the data instead: say so when they differ.
    let given = |key: &str| p.get("size").filter(|v| v.is_object()).and_then(|v| v.get(key)).and_then(Value::as_f64);
    if let Some(DiagramSize::Scaled { min, max, .. }) = l.diagram.as_ref().map(|d| d.size)
        && (given("min").is_some_and(|g| g != min) || given("max").is_some_and(|g| g != max))
    {
        s.notify(format!("size's min and max are measured from the data ({min} to {max}), not set: the ones given are not used"), false);
    }
    if skipped_total > 0 {
        // A streamed layer's features are the view's, so the count covers only those read so far.
        let scope = if streamed { " (only the features read so far)" } else { "" };
        s.notify(
            format!(
                "{skipped_total} value(s) in {} field(s) ({}) are zero, negative or not numbers, and are not drawn{scope}",
                skipped_fields.len(),
                skipped_fields.join(", "),
            ),
            false,
        );
    }
    Ok(out)
}

/// The range of the categories' sums over `ds`'s features, which a scaled size grows over: (0, 1)
/// when no feature has a chart to draw.
pub(crate) fn sum_range(d: &DiagramSettings, ds: &rustgis_data::Dataset) -> (f64, f64) {
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    let fields = d.field_indices(ds);
    for row in 0..ds.features.len() {
        if let Some((_, total)) = d.values_at(ds, row, &fields) {
            min = min.min(total);
            max = max.max(total);
        }
    }
    if max >= min && min.is_finite() && max.is_finite() { (min, max) } else { (0.0, 1.0) }
}

/// The layer's diagram settings, or null without one.
fn info(s: &mut Session, p: &Value) -> CmdResult {
    let id = s.layer_id(p)?;
    let l = s.map().layer(id).ok_or("no such layer")?;
    let Some(d) = &l.diagram else { return Ok(Value::Null) };
    Ok(json!({
        "kind": d.kind.label().to_lowercase(),
        "fields": d.categories.iter().map(|c| json!({"field": c.field, "color": c.colour.hex()})).collect::<Vec<_>>(),
        "size": match d.size {
            // The shapes layer.diagram takes, so this can be sent back as it is.
            DiagramSize::Fixed(s) => json!(s),
            DiagramSize::Scaled { min_size, max_size, min, max } => json!({"from": min_size, "to": max_size, "min": min, "max": max}),
        },
        "startAngle": d.start_angle,
        "labels": d.labels.code(),
        "background": d.background.map_or(Value::Null, |c| Value::String(c.hex())),
        "outline": {"color": d.stroke.hex(), "width": d.stroke_width},
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Project, Session};
    use rustgis_data::{Dataset, Field, FieldType, Geometry, GeometryType, Value};
    use rustgis_geom::Coord;

    /// A session with a two-feature Regions layer: (52k, 48k) and (30k, 45k).
    fn regions() -> (Session, u32) {
        let mut s = Session::new(Project::default());
        let square = |x: f64| {
            Geometry::Polygon(vec![vec![vec![
                Coord::new(x, 0.0),
                Coord::new(x + 1.0, 0.0),
                Coord::new(x + 1.0, 1.0),
                Coord::new(x, 1.0),
                Coord::new(x, 0.0),
            ]]])
        };
        let mut ds = Dataset::new(
            "Regions",
            GeometryType::Polygon,
            vec![Field::new("pop_male", FieldType::Integer), Field::new("pop_female", FieldType::Integer)],
        );
        ds.push(Some(square(0.0)), vec![Value::Int(52_000), Value::Int(48_000)]);
        ds.push(Some(square(2.0)), vec![Value::Int(30_000), Value::Int(45_000)]);
        let id = s.map_mut().add_dataset(ds);
        s.active_layer = Some(id);
        (s, id)
    }

    #[test]
    fn a_diagram_is_set_read_and_cleared() {
        let (mut s, id) = regions();
        s.run("layer.diagram", &json!({"kind": "pie", "fields": ["pop_male", "pop_female"]})).unwrap();
        let d = s.map().layer(id).unwrap().diagram.clone().expect("set");
        assert_eq!(d.kind, DiagramKind::Pie);
        assert_eq!(d.categories.iter().map(|c| c.field.as_str()).collect::<Vec<_>>(), ["pop_male", "pop_female"]);
        // Colours default to the palette's turn.
        assert_eq!(d.categories[0].colour, CATEGORY_COLORS[0]);
        assert_eq!(d.categories[1].colour, CATEGORY_COLORS[1]);
        // The scaled size ranges over the features' sums: 75,000 to 100,000.
        assert!(matches!(d.size, DiagramSize::Scaled { min: 75_000.0, max: 100_000.0, .. }));
        // Read back.
        let info = s.run("layer.diagramInfo", &json!({})).unwrap();
        assert_eq!(info["kind"], "pie");
        assert_eq!(info["fields"].as_array().unwrap().len(), 2);
        assert_eq!(info["fields"][0]["field"], "pop_male");
        // A fixed size and hand colours.
        s.run("layer.diagram", &json!({"kind": "bar", "fields": [{"field": "pop_male", "color": "#4A90D9"}], "size": 18})).unwrap();
        let d = s.map().layer(id).unwrap().diagram.clone().expect("replaced");
        assert_eq!(d.kind, DiagramKind::Bar);
        assert!(matches!(d.size, DiagramSize::Fixed(18.0)));
        // Value and percent labels ride along, and read back.
        s.run("layer.diagram", &json!({"kind": "pie", "fields": ["pop_male", "pop_female"], "labels": "both"})).unwrap();
        let d = s.map().layer(id).unwrap().diagram.clone().expect("labelled");
        assert_eq!(d.labels, DiagramLabel::Both);
        assert_eq!(s.run("layer.diagramInfo", &json!({})).unwrap()["labels"], "both");
        assert!(s.run("layer.diagram", &json!({"kind": "pie", "fields": ["pop_male"], "labels": "shout"})).is_err());
        // Errors: an unknown field, a non-numeric one, a bad kind.
        assert!(s.run("layer.diagram", &json!({"kind": "pie", "fields": ["nope"]})).is_err());
        let (mut s, id) = regions();
        assert!(s.run("layer.diagram", &json!({"kind": "hexagon", "fields": ["pop_male"]})).is_err());
        assert!(s.map().layer(id).unwrap().diagram.is_none(), "refused, so unset");
        // Cleared, and undo brings it back.
        let (mut s, id) = regions();
        s.run("layer.diagram", &json!({"kind": "pie", "fields": ["pop_male", "pop_female"]})).unwrap();
        s.run("layer.clearDiagram", &json!({})).unwrap();
        assert!(s.map().layer(id).unwrap().diagram.is_none());
        assert!(s.undo());
        assert!(s.map().layer(id).unwrap().diagram.is_some(), "the clear is undone");
    }

    #[test]
    fn a_malformed_number_is_refused_and_a_numeric_string_is_read() {
        let (mut s, id) = regions();
        let base = json!({"kind": "pie", "fields": ["pop_male"]});
        for (key, value) in [("size", json!({"from": "north"})), ("startAngle", json!("north")), ("outline", json!({"width": "thick"}))] {
            let mut p = base.clone();
            p[key] = value;
            assert!(s.run("layer.diagram", &p).is_err(), "{key} is refused, not replaced by its default");
        }
        assert!(s.map().layer(id).unwrap().diagram.is_none(), "refused, so unset");
        // A numeric string is a number, as the other numbers of a command are read.
        s.run("layer.diagram", &json!({"kind": "pie", "fields": ["pop_male"], "size": {"from": "18", "to": 40}})).unwrap();
        let d = s.map().layer(id).unwrap().diagram.clone().expect("set");
        assert!(matches!(d.size, DiagramSize::Scaled { min_size, .. } if min_size == 18.0));
    }

    #[test]
    fn a_size_or_outline_out_of_range_is_refused_not_clamped() {
        let (mut s, id) = regions();
        let base = json!({"kind": "pie", "fields": ["pop_male"]});
        for (key, value) in [
            ("size", json!({"from": 0.2})),
            ("size", json!({"to": 500})),
            ("size", json!({"from": 30, "to": 10})),
            ("outline", json!({"width": 20})),
            ("outline", json!({"width": -1})),
        ] {
            let mut p = base.clone();
            p[key] = value.clone();
            assert!(s.run("layer.diagram", &p).is_err(), "{key} {value} is refused, not moved into range");
        }
        assert!(s.map().layer(id).unwrap().diagram.is_none(), "refused, so unset");
        // The bounds themselves are drawn.
        s.run("layer.diagram", &json!({"kind": "pie", "fields": ["pop_male"], "size": {"from": 1, "to": 200}, "outline": {"width": 8}}))
            .unwrap();
        let d = s.map().layer(id).unwrap().diagram.clone().expect("set");
        assert!(matches!(d.size, DiagramSize::Scaled { min_size: 1.0, max_size: 200.0, .. }));
        assert_eq!(d.stroke_width, 8.0);
    }

    #[test]
    fn a_repeated_field_or_an_unknown_size_key_is_refused() {
        let (mut s, id) = regions();
        assert!(s.run("layer.diagram", &json!({"kind": "pie", "fields": ["pop_male", "POP_MALE"]})).is_err(), "a field twice");
        for size in [json!({"form": 18}), json!({"fixed": 12}), json!({"scaled": {"from": 6}})] {
            assert!(s.run("layer.diagram", &json!({"kind": "pie", "fields": ["pop_male"], "size": size})).is_err(), "{size} is refused");
        }
        assert!(s.map().layer(id).unwrap().diagram.is_none(), "refused, so unset");
    }

    #[test]
    fn deleting_a_charted_field_drops_its_slice_and_says_so() {
        let (mut s, id) = regions();
        s.run("layer.diagram", &json!({"kind": "pie", "fields": ["pop_male", "pop_female"]})).unwrap();
        let mark = s.notice_mark();
        s.run("edit.deleteField", &json!({"field": "POP_MALE"})).unwrap();
        let d = s.map().layer(id).unwrap().diagram.clone().expect("one field is left");
        assert_eq!(d.categories.iter().map(|c| c.field.as_str()).collect::<Vec<_>>(), ["pop_female"]);
        // The range of sums is measured again, over pop_female alone.
        let DiagramSize::Scaled { max, .. } = d.size else { panic!("still scaled") };
        assert!(max < 75_000.0, "measured without the deleted field: {max}");
        assert_eq!(s.notices_since(mark).len(), 1, "said once");
        // The last field goes, and the diagram with it.
        let mark = s.notice_mark();
        s.run("edit.deleteField", &json!({"field": "pop_female"})).unwrap();
        assert!(s.map().layer(id).unwrap().diagram.is_none());
        let said: Vec<&str> = s.notices_since(mark).iter().map(|n| n.text.as_str()).collect();
        assert!(said.iter().any(|t| t.contains("diagram drew only the deleted field")), "{said:?}");
        // Deleting a field no diagram draws says nothing about a diagram.
        let (mut s, _) = regions();
        s.run("layer.diagram", &json!({"kind": "pie", "fields": ["pop_male"]})).unwrap();
        let mark = s.notice_mark();
        s.run("edit.deleteField", &json!({"field": "pop_female"})).unwrap();
        assert!(s.notices_since(mark).iter().all(|n| !n.text.contains("diagram")));
    }

    #[test]
    fn diagram_info_can_be_sent_back_as_it_is() {
        let (mut s, id) = regions();
        for size in [json!(18), json!({"from": 8, "to": 30})] {
            s.run("layer.diagram", &json!({"kind": "bar", "fields": ["pop_male", "pop_female"], "size": size})).unwrap();
            let before = s.map().layer(id).unwrap().diagram.clone();
            let info = s.run("layer.diagramInfo", &json!({})).unwrap();
            let mark = s.notice_mark();
            s.run("layer.diagram", &info).unwrap();
            assert_eq!(s.map().layer(id).unwrap().diagram, before, "{size} reads back to the same diagram");
            assert!(s.notices_since(mark).is_empty(), "its own range is the one measured, so nothing to say");
        }
        // A range set by hand is not used, and that is said.
        let mark = s.notice_mark();
        s.run("layer.diagram", &json!({"kind": "pie", "fields": ["pop_male", "pop_female"], "size": {"min": 0, "max": 500}})).unwrap();
        let said: Vec<&str> = s.notices_since(mark).iter().map(|n| n.text.as_str()).collect();
        assert!(said.iter().any(|t| t.starts_with("size's min and max are measured from the data")), "{said:?}");
    }

    #[test]
    fn a_value_a_chart_cannot_draw_is_said_by_count() {
        let (mut s, _) = regions();
        let mark = s.notice_mark();
        s.run("layer.diagram", &json!({"kind": "pie", "fields": ["pop_male", "pop_female"]})).unwrap();
        assert!(s.notices_since(mark).is_empty(), "clean input says nothing");
        // A negative and a zero are not drawn, and say so once for their field; a null is missing, not skipped.
        let mut ds = Dataset::new(
            "Change",
            GeometryType::Polygon,
            vec![Field::new("net", FieldType::Integer), Field::new("gain", FieldType::Integer)],
        );
        ds.push(None, vec![Value::Int(-3_000), Value::Int(4_000)]);
        ds.push(None, vec![Value::Int(0), Value::Int(2_000)]);
        ds.push(None, vec![Value::Null, Value::Int(1_000)]);
        let id = s.map_mut().add_dataset(ds);
        s.active_layer = Some(id);
        let mark = s.notice_mark();
        s.run("layer.diagram", &json!({"kind": "bar", "fields": ["net", "gain"]})).unwrap();
        let said: Vec<&str> = s.notices_since(mark).iter().map(|n| n.text.as_str()).collect();
        assert_eq!(said, ["2 value(s) in 1 field(s) ('net') are zero, negative or not numbers, and are not drawn"]);
        // An infinite value is not drawn either, so it is counted.
        let mut ds = Dataset::new("Spikes", GeometryType::Polygon, vec![Field::new("v", FieldType::Double)]);
        ds.push(None, vec![Value::Float(f64::INFINITY)]);
        ds.push(None, vec![Value::Float(5.0)]);
        let id = s.map_mut().add_dataset(ds);
        s.active_layer = Some(id);
        let mark = s.notice_mark();
        s.run("layer.diagram", &json!({"kind": "pie", "fields": ["v"], "size": 12})).unwrap();
        let said: Vec<&str> = s.notices_since(mark).iter().map(|n| n.text.as_str()).collect();
        assert_eq!(said, ["1 value(s) in 1 field(s) ('v') are zero, negative or not numbers, and are not drawn"]);
    }
}
