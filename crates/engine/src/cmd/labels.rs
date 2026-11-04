//! The Labeling tab (#897): Map Labeling (lock, pause, unplaced labels, convert to annotation), a
//! layer's label visibility range and placement, and its label classes (#618).

use rustgis_data::{Dataset, Field, FieldType, Geometry, GeometryType, Value as Attr};
use rustgis_map::symbology::{MAX_LABEL_CLASSES, label_class_name};
use rustgis_map::{LabelBackground, LabelClass, LabelMatcher, LabelPlacement, Labeling, Layer, Renderer, Rgba, Symbol};
use serde_json::{Value, json};

use super::layer::s_req;
use super::{b, f, need_feature_layer, ps};
use crate::{CmdResult, CommandSpec as C, Session};

/// A number, or a number written as text (as `f` reads a parameter).
fn number(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())).filter(|x: &f64| x.is_finite())
}

/// A label offset given as `[right, down]` (points), each kept within the range a class keeps.
fn label_offset(v: &Value) -> Option<[f32; 2]> {
    let range = rustgis_map::symbology::LABEL_OFFSET;
    let [x, y] = v.as_array()?.as_slice() else { return None };
    let within = |v: &Value| number(v).map(|v| v.clamp(*range.start(), *range.end()) as f32);
    Some([within(x)?, within(y)?])
}

/// What `labels.style`'s `background` does to a class's background.
pub(super) type BackgroundChange = Box<dyn Fn(Option<LabelBackground>) -> Option<LabelBackground>>;

/// Reads `labels.style`'s `background`: false or null takes it away, true puts one on (the default,
/// or the one it has), and an object sets the parts it gives on the one it has (else on the
/// default). Anything else, or a part that isn't what it should be, is an error.
pub(super) fn background_change(v: &Value) -> Result<BackgroundChange, String> {
    let o = match v {
        Value::Null | Value::Bool(false) => return Ok(Box::new(|_| None)),
        Value::Bool(true) => return Ok(Box::new(|b| Some(b.unwrap_or_default()))),
        Value::Object(o) => o,
        _ => return Err("background must be true, false, null or {fill, stroke, strokeWidth, padding, radius}".into()),
    };
    // A colour, or "" or null for none.
    let color = |k: &str| -> Result<Option<Option<Rgba>>, String> {
        match o.get(k) {
            None => Ok(None),
            Some(Value::Null) => Ok(Some(None)),
            Some(Value::String(c)) if c.trim().is_empty() => Ok(Some(None)),
            Some(Value::String(c)) => Rgba::from_hex(c.trim())
                .map(|c| Some(Some(c)))
                .ok_or_else(|| format!("background.{k} must be #RRGGBB or #RRGGBBAA, or \"\"")),
            Some(_) => Err(format!("background.{k} must be #RRGGBB or #RRGGBBAA, or \"\"")),
        }
    };
    let number = |v: &Value| number(v).map(|v| v as f32);
    let points = |k: &str| -> Result<Option<f32>, String> {
        o.get(k).map(|v| number(v).ok_or_else(|| format!("background.{k} must be a number of points"))).transpose()
    };
    let (fill, stroke) = (color("fill")?, color("stroke")?);
    let (stroke_width, radius) = (points("strokeWidth")?, points("radius")?);
    let padding = match o.get("padding") {
        None => None,
        Some(v) => match v.as_array().map(Vec::as_slice) {
            Some([x, y]) => Some([number(x), number(y)]),
            _ => None,
        }
        .and_then(|[x, y]| Some([x?, y?]))
        .map(Some)
        .ok_or("background.padding must be [across, up and down] in points")?,
    };
    Ok(Box::new(move |b| {
        let mut b = b.unwrap_or_default();
        if let Some(c) = fill {
            b.fill = c;
        }
        if let Some(c) = stroke {
            b.stroke = c;
        }
        if let Some(w) = stroke_width {
            b.stroke_width = w;
        }
        if let Some(p) = padding {
            b.padding = p;
        }
        if let Some(r) = radius {
            b.radius = r;
        }
        Some(b.checked())
    }))
}

pub fn commands() -> Vec<C> {
    vec![
        C::new("labels.lock", "Lock", "Labeling › Map Labeling", |s, p| {
            let d = &mut s.map_mut().label_display;
            d.locked = b(p, "on").unwrap_or(!d.locked);
            Ok(json!({"locked": d.locked}))
        })
        .view()
        .params(r#"{"on"?: bool}"#),
        C::new("labels.pause", "Pause", "Labeling › Map Labeling", |s, p| {
            let d = &mut s.map_mut().label_display;
            d.paused = b(p, "on").unwrap_or(!d.paused);
            Ok(json!({"paused": d.paused}))
        })
        .view()
        .params(r#"{"on"?: bool}"#),
        C::new("labels.viewUnplaced", "View Unplaced", "Labeling › Map Labeling", |s, p| {
            let d = &mut s.map_mut().label_display;
            d.show_unplaced = b(p, "on").unwrap_or(!d.show_unplaced);
            Ok(json!({"showUnplaced": d.show_unplaced}))
        })
        .view()
        .params(r#"{"on"?: bool}"#),
        C::new("labels.fonts", "Label Fonts", "View › Windows › Labeling", |_, p| {
            // The families a font picker lists, those containing `search` (in any case) if given.
            // While the app is still indexing them (commands run on its UI thread), none yet,
            // and `indexing` says so: asking again later lists them.
            let search = ps(p, "search").map(str::to_lowercase).unwrap_or_default();
            let Some(all) = crate::render::families::available() else { return Ok(json!({"fonts": [], "indexing": true})) };
            let fonts: Vec<&str> =
                all.iter().filter(|f| f.listed && f.name.to_lowercase().contains(&search)).map(|f| f.name.as_str()).collect();
            Ok(json!({"fonts": fonts}))
        })
        .view()
        .params(r#"{"search"?: text in the family name} → {"fonts": [the installed font families, by name], "indexing"?: true (still being indexed: none listed yet)}"#),
        C::new("labels.convert", "Convert To Annotation", "Labeling › Map Labeling", convert)
            .when(need_feature_layer)
            .params(r#"{"layer"?, "name"?: string}"#),
        C::new("labels.scaleRange", "Label Visibility Range", "Labeling › Label Range", scale_range)
            .when(need_feature_layer)
            .params(r#"{"layer"?, "class"?: name (none: the Default class, by whatever name it has), "min"?: scale|null (out beyond), "max"?: scale|null (in beyond), "clear"?: bool (clears the scale and distance limits, as Clear Limits does)}"#),
        C::new("labels.distanceRange", "Label Distance Range", "Labeling › Label Range", distance_range)
            .when(need_feature_layer)
            .params(r#"{"layer"?, "class"?: name (none: the Default class, by whatever name it has), "closest"?: metres|null, "farthest"?: metres|null, "clear"?: bool}"#),
        C::new("labels.placement", "Label Placement", "Labeling › Label Placement", |s, p| {
            let pos = match ps(p, "position") {
                Some(code) => Some(LabelPlacement::from_code(code).ok_or_else(|| format!("unknown placement '{code}'"))?),
                // A position that isn't a placement name is refused, not dropped for the others (null:
                // none, as for the other keys).
                None if p.get("position").is_some_and(|v| !v.is_null()) => return Err("position must be a placement name".into()),
                None if ["offset", "repeat", "otherPositions"].iter().any(|k| p.get(k).is_some()) => None,
                None => return Err("position is required (or an offset, a repeat or otherPositions)".into()),
            };
            let offset = match p.get("offset") {
                None => None,
                Some(o) => Some(label_offset(o).ok_or("offset must be [right, down] in points")?),
            };
            // Along a line, a label every so many points (null or 0: the one at its middle).
            let repeat = match p.get("repeat") {
                None => None,
                Some(Value::Null) => Some(None),
                Some(v) => match number(v) {
                    Some(0.0) => Some(None),
                    Some(r) if r > 0.0 => Some(rustgis_map::symbology::label_repeat(r)),
                    _ => return Err("repeat must be a number of points, or 0 or null for none".into()),
                },
            };
            // A label tries other places when its own is taken: round its point (#1011), or along
            // its line (#1187).
            let others = match p.get("otherPositions") {
                None => None,
                Some(Value::Bool(on)) => Some(*on),
                Some(_) => return Err("otherPositions must be true or false".into()),
            };
            let id = s.layer_id(p)?;
            let (points, lines) = points_and_lines(s.map().layer(id).ok_or("no such layer")?);
            with_labels(s, p, |labels| {
                let c = class_in(labels, p)?;
                if let Some(pos) = pos {
                    c.placement = pos;
                }
                if let Some(on) = others {
                    c.other_positions = on;
                }
                if let Some(o) = offset {
                    c.offset = o;
                }
                if let Some(r) = repeat {
                    c.repeat = r;
                }
                // Asked of the class as this call leaves it, so a position or a repeat given with
                // it counts. On where it would do nothing is refused; off is taken anywhere.
                if others == Some(true) && !tries_other_places(points, lines, c) {
                    return Err("otherPositions is for points, and for lines placed parallel or curved without a repeat".into());
                }
                Ok(())
            })
        })
        .when(need_feature_layer)
        .params(
            r#"{"layer"?, "class"?: name (none: the Default class, by whatever name it has), "position"?: "auto"|"center"|"above"|"below"|"left"|"right"|"aboveRight"|"aboveLeft"|"belowRight"|"belowLeft"|"parallel" (along a line, straight)|"curved" (along a line, following its bends) (required without offset, repeat or otherPositions), "offset"?: [right, down] points (-200 to 200 each) the label moves from where its position puts it ([0, 0]: none; a label along a line isn't moved), "repeat"?: points (10 to 10000) between a line's labels, spaced out from the one at its middle, at most 32 a line (0 or null: the one at its middle; parallel and curved only), "otherPositions"?: bool (a label whose place is taken tries others: a point's round the point, corners first; a line's along the line, nearest its middle first, placed parallel or curved without a repeat; true is refused for any other line label and on polygons)}"#,
        ),
        // Label classes: each labels every feature its clause takes, as in ArcGIS Pro; the classes
        // listed first, Default last, are placed first (`Labeling`, #732).
        C::new("labels.class.add", "Add Label Class", "View › Windows › Labeling", add_class)
            .when(need_feature_layer)
            .params(r#"{"layer"?, "name": string, "where"?: sql (a definition query's language; none takes every feature), "from"?: class (whose field and text symbol it starts with; none: the Default class)}"#),
        C::new("labels.class.remove", "Remove Label Class", "View › Windows › Labeling", |s, p| {
            let name = s_req(p, "class")?.trim().to_string();
            with_labels(s, p, |labels| {
                let i = extra_class(labels, &name)?;
                labels.classes.remove(i);
                Ok(())
            })
        })
        .when(need_feature_layer)
        .params(r#"{"layer"?, "class": name (not Default)}"#),
        // Every class but Default; none to remove is no error, so a script can start with it.
        C::new("labels.class.clear", "Remove All Label Classes", "View › Windows › Labeling", |s, p| {
            let mut removed = 0;
            with_labels(s, p, |labels| {
                removed = labels.classes.len();
                labels.classes.clear();
                Ok(())
            })?;
            Ok(json!({"removed": removed}))
        })
        .when(need_feature_layer)
        .params(r#"{"layer"?}"#),
        C::new("labels.class.rename", "Rename Label Class", "View › Windows › Labeling", |s, p| {
            let class = s_req(p, "class")?.trim().to_string();
            let name = s_req(p, "name")?.to_string();
            with_labels(s, p, |labels| {
                let name = class_name(labels, &name, Some(&class))?;
                labels.class_mut(&class).ok_or_else(|| format!("no label class named '{class}'"))?.name = name;
                Ok(())
            })
        })
        .when(need_feature_layer)
        .params(r#"{"layer"?, "class": name, "name": string}"#),
        C::new("labels.class.update", "Update Label Class", "View › Windows › Labeling", |s, p| {
            let id = s.layer_id(p)?;
            let clause = s.map().layer(id).map(|l| clause(p, &l.dataset)).transpose()?.flatten();
            let visible = b(p, "visible");
            with_class(s, p, |c| {
                if let Some(w) = clause {
                    c.where_ = w;
                }
                if let Some(v) = visible {
                    c.visible = v;
                }
            })
        })
        .when(need_feature_layer)
        .params(r#"{"layer"?, "class"?: name (none: the Default class, by whatever name it has), "where"?: sql|null, "visible"?: bool}"#),
        C::new("labels.class.move", "Move Label Class", "View › Windows › Labeling", |s, p| {
            let name = s_req(p, "class")?.trim().to_string();
            let to = f(p, "to").filter(|v| v.is_finite()).ok_or("to is required: the class's place in the list, 0 first")?;
            with_labels(s, p, |labels| {
                let i = extra_class(labels, &name)?;
                let c = labels.classes.remove(i);
                let to = (to.max(0.0) as usize).min(labels.classes.len());
                labels.classes.insert(to, c);
                Ok(())
            })
        })
        .when(need_feature_layer)
        .params(r#"{"layer"?, "class": name (not Default), "to": number (its place among the classes listed before Default, 0 first: placed first when labels compete for room)}"#),
        C::new("labels.by", "Labels by Expression", "View › Windows › Labeling", labels_by)
            .when(need_feature_layer)
            .params(r#"{"layer"?, "class"?: name (none: the Default class, by whatever name it has), "text"?: expression|null (each label's text, in place of the class's field; a feature it gives null or blanks for isn't labelled), "size"?: expression|null (the text's size in points, 4 to 72), "color"?: expression|null (the text's colour: '#RRGGBB', a CSS colour name or 'r,g,b[,a]' as color_rgb gives it), "angle"?: expression|null (a point's or polygon's label turned by it, in degrees clockwise; labels along lines keep the line's)} (each expression in the language of queries, per feature, @map_scale the map's scale; a key left out keeps its expression, null or "" removes it; where a size, color or angle expression gives null the class's own setting is used)"#),
    ]
}

/// Whether the layer holds points and lines: declared so, or among its features, as the renderer
/// goes by each feature's geometry.
fn points_and_lines(l: &Layer) -> (bool, bool) {
    (l.has_geometry(GeometryType::Point), l.has_geometry(GeometryType::Polyline))
}

/// Whether a class on a layer holding these has other places to try when its label's own is taken:
/// round a point at any position (#1011), or along a line for a label at its middle (#1187). Each
/// repeat is already a place of its own, so repeats get none.
fn tries_other_places(points: bool, lines: bool, c: &LabelClass) -> bool {
    points || (lines && c.placement.along_line() && c.repeat.is_none())
}

/// `labels.by`: sets a label class's expressions, each checked against the layer's fields.
fn labels_by(s: &mut Session, p: &Value) -> CmdResult {
    const KEYS: [&str; 4] = ["text", "size", "color", "angle"];
    let id = s.layer_id(p)?;
    let l = s.map().layer(id).ok_or("no such layer")?;
    let mut changes: Vec<(&str, Option<String>)> = Vec::new();
    for key in KEYS {
        let text = match p.get(key) {
            None => continue,
            Some(Value::Null) => None,
            Some(Value::String(t)) if t.trim().is_empty() => None,
            Some(Value::String(t)) => Some(t.clone()),
            Some(_) => return Err(format!("{key} is an expression's text, or null for none")),
        };
        if let Some(t) = &text {
            rustgis_data::query::parse(t, &l.dataset).map_err(|e| format!("{key}: {e}"))?;
        }
        changes.push((key, text));
    }
    if changes.is_empty() {
        return Err("give text, size, color or angle: an expression, or null for none".into());
    }
    let mut by = None;
    with_labels(s, p, |labels| {
        let c = class_in(labels, p)?;
        for (key, text) in changes {
            let slot = match key {
                "text" => &mut c.by.text,
                "size" => &mut c.by.size,
                "color" => &mut c.by.color,
                _ => &mut c.by.angle,
            };
            *slot = text;
        }
        by = Some(c.by.clone());
        // Labels by a text expression need no field, but labels must be on to show.
        if c.by.text.is_some() {
            labels.enabled = true;
        }
        Ok(())
    })?;
    let by = by.unwrap_or_default();
    Ok(json!({"text": by.text, "size": by.size, "color": by.color, "angle": by.angle}))
}

/// Changes the label class `p` names (`"class"`; Default when none) of the layer it names.
pub(crate) fn with_class(s: &mut Session, p: &Value, f: impl FnOnce(&mut LabelClass)) -> CmdResult {
    with_labels(s, p, |labels| {
        f(class_in(labels, p)?);
        Ok(())
    })
}

/// The label class `p` names (`"class"`; Default when none).
pub(crate) fn class_in<'a>(labels: &'a mut Labeling, p: &Value) -> Result<&'a mut LabelClass, String> {
    match class_param(p, "class")? {
        None => Ok(&mut labels.default),
        Some(n) => labels.class_mut(n).ok_or_else(|| format!("no label class named '{n}'")),
    }
}

/// The class name `p` gives as `key`: none when it's missing or null (the Default class), and a
/// refusal when it isn't a name, rather than changing Default by mistake.
fn class_param<'a>(p: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            v.as_str().map(str::trim).filter(|n| !n.is_empty()).map(Some).ok_or_else(|| format!("{key} must be a label class's name"))
        }
    }
}

/// Changes the labels of the layer `p` names; nothing changes when `f` refuses.
pub(crate) fn with_labels(s: &mut Session, p: &Value, f: impl FnOnce(&mut Labeling) -> Result<(), String>) -> CmdResult {
    let id = s.layer_id(p)?;
    let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
    let mut labels = l.labels.clone();
    f(&mut labels)?;
    let relock = !lock_holds(l, &labels);
    l.labels = labels;
    if relock {
        s.map_mut().label_lock = None;
        s.map_mut().label_lock_line_anchors.clear();
    }
    Ok(Value::Null)
}

/// Whether `a` and `b` list the same classes in the same order. A lock names each label by its
/// class's place in the list (`LabelKey`), so when the list changes the labels are locked again
/// where they are placed next (`Map::label_lock` emptied, #732).
pub(crate) fn same_class_list(a: &Labeling, b: &Labeling) -> bool {
    a.classes.iter().map(|c| &c.name).eq(b.classes.iter().map(|c| &c.name))
}

/// Whether a lock taken with the layer's labels still names those of `b`: the same classes, listed
/// alike and putting their labels in the same places.
pub(crate) fn lock_holds(l: &Layer, b: &Labeling) -> bool {
    same_class_list(&l.labels, b) && same_places(l, b)
}

/// Whether the layer's classes and those of `b` (listed alike) put their labels in the same places.
/// A lock names the place round its point or along its line a label took by its number among its
/// class's (#1011, #1187), so another position, or trying the others turned on or off, locks the
/// labels again; with no class trying them a new position keeps the lock, as before. The flag only
/// counts where it has an effect: kept on a line's class at a position off the line, or with a
/// repeat, it does nothing ([`tries_other_places`]).
fn same_places(l: &Layer, b: &Labeling) -> bool {
    let (points, lines) = points_and_lines(l);
    let classes = |labels: &Labeling| {
        let tries = |c: &LabelClass| c.other_positions && tries_other_places(points, lines, c);
        labels.classes.iter().chain([&labels.default]).map(|c| (c.placement, tries(c), c.repeat.is_some())).collect::<Vec<_>>()
    };
    classes(&l.labels).into_iter().zip(classes(b)).all(|(x, y)| x == y || !(x.1 || y.1))
}

/// The place of the class named `name` among those checked before Default (of the
/// `MAX_LABEL_CLASSES` that count, as `Labeling::class` finds them).
fn extra_class(labels: &Labeling, name: &str) -> Result<usize, String> {
    if labels.default.name.eq_ignore_ascii_case(name) {
        return Err(format!("{name} is the Default class: it stays last in the list"));
    }
    let mut counted = labels.classes.iter().take(MAX_LABEL_CLASSES);
    counted.position(|c| c.name.eq_ignore_ascii_case(name)).ok_or_else(|| format!("no label class named '{name}'"))
}

/// A class name: trimmed, kept short, and not another class's (`renaming` is the class being
/// renamed, which may keep its own).
fn class_name(labels: &Labeling, name: &str, renaming: Option<&str>) -> Result<String, String> {
    let name = label_class_name(name);
    if name.is_empty() {
        return Err("a label class needs a name".into());
    }
    let own = |c: &LabelClass| renaming.is_some_and(|r| c.name.eq_ignore_ascii_case(r.trim()));
    if labels.in_order().any(|c| c.name.eq_ignore_ascii_case(&name) && !own(c)) {
        return Err(format!("there is already a label class named '{name}'"));
    }
    Ok(name)
}

/// A class clause parameter (`"where"`): `None` leaves it, `Some(None)` clears it, and a clause is
/// kept only if it reads against `ds`.
fn clause(p: &Value, ds: &Dataset) -> Result<Option<Option<String>>, String> {
    match p.get("where") {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(Value::String(w)) if w.trim().is_empty() => Ok(Some(None)),
        Some(Value::String(w)) => {
            rustgis_data::query::parse(w, ds).map_err(|e| format!("the class's clause doesn't read: {e}"))?;
            Ok(Some(Some(w.trim().to_string())))
        }
        Some(_) => Err("where must be a SQL clause".into()),
    }
}

fn add_class(s: &mut Session, p: &Value) -> CmdResult {
    let name = s_req(p, "name")?.to_string();
    let from = class_param(p, "from")?.map(String::from);
    let id = s.layer_id(p)?;
    let clause = s.map().layer(id).map(|l| clause(p, &l.dataset)).transpose()?.flatten().flatten();
    let mut added = String::new();
    with_labels(s, p, |labels| {
        if labels.classes.len() >= MAX_LABEL_CLASSES {
            return Err(format!("a layer keeps at most {MAX_LABEL_CLASSES} label classes besides Default"));
        }
        let name = class_name(labels, &name, None)?;
        let base = match &from {
            None => &labels.default,
            Some(n) => labels.class(n).ok_or_else(|| format!("no label class named '{n}'"))?,
        };
        let class = LabelClass { name: name.clone(), visible: true, where_: clause, ..base.clone() };
        labels.classes.push(class);
        added = name;
        Ok(())
    })?;
    Ok(json!({"class": added}))
}

/// A limit parameter: `Some(None)` clears it, `Some(Some(v))` sets it, `None` leaves it.
fn limit(p: &Value, k: &str) -> Result<Option<Option<f64>>, String> {
    match p.get(k) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(Value::String(t)) if t.trim().is_empty() => Ok(Some(None)),
        Some(_) => match f(p, k) {
            Some(v) if v > 0.0 => Ok(Some(Some(v.min(1e12)))),
            Some(v) if v.abs() < f64::EPSILON => Ok(Some(None)),
            _ => Err(format!("{k} must be a positive number")),
        },
    }
}

fn scale_range(s: &mut Session, p: &Value) -> CmdResult {
    let clear = b(p, "clear").unwrap_or(false);
    let (min, max) = (limit(p, "min")?, limit(p, "max")?);
    with_class(s, p, |c| {
        if clear {
            c.min_scale = None;
            c.max_scale = None;
            c.closest = None;
            c.farthest = None;
        }
        if let Some(v) = min {
            c.min_scale = v;
        }
        if let Some(v) = max {
            c.max_scale = v;
        }
    })
}

fn distance_range(s: &mut Session, p: &Value) -> CmdResult {
    let clear = b(p, "clear").unwrap_or(false);
    let (closest, farthest) = (limit(p, "closest")?, limit(p, "farthest")?);
    with_class(s, p, |c| {
        if clear {
            c.closest = None;
            c.farthest = None;
        }
        if let Some(v) = closest {
            c.closest = v;
        }
        if let Some(v) = farthest {
            c.farthest = v;
        }
    })
}

/// Converts a layer's labels into an annotation layer: one point per label, at the label point,
/// carrying the label text and drawn with the same text symbol. Each class labelling a feature
/// gives it a point (a hidden class none); with classes besides Default, a CLASS field keeps
/// which, and the annotation has the same classes, each picking its points by it. The layer's
/// own labels turn off.
fn convert(s: &mut Session, p: &Value) -> CmdResult {
    let id = s.layer_id(p)?;
    let scale = s.map().scale();
    let l = s.map().layer(id).ok_or("no such layer")?;
    let fields: Vec<Option<usize>> = l.labels.in_order().map(|c| l.dataset.field_index(&c.field)).collect();
    // Its classes' expressions, as the map draws them now (at its scale).
    let exprs: Vec<Option<rustgis_map::label_by::LabelExprs>> =
        l.labels.in_order().map(|c| rustgis_map::label_by::LabelExprs::new(&c.by, &l.dataset, Some(scale))).collect();
    // A line's labels along it keep the line's angle, not the expression's, as the map draws
    // them: decided for each feature by its geometry.
    let along = |c: &LabelClass, g: Option<&Geometry>| c.placement.along_line() && matches!(g, Some(Geometry::Line(_)));
    let only_lines = !l.has_geometry(GeometryType::Point) && !l.has_geometry(GeometryType::Polygon);
    let by_text = |i: usize| exprs.get(i).and_then(Option::as_ref).filter(|e| e.has_text());
    if fields.iter().all(Option::is_none) && (0..fields.len()).all(|i| by_text(i).is_none()) {
        return Err("the layer has no label field".into());
    }
    let classes = !l.labels.classes.is_empty();
    let rows = l.visible_rows();
    let matcher = LabelMatcher::new(&l.labels, &l.dataset, Some(scale));
    let mut out_fields = vec![Field::new("TEXT", FieldType::Text)];
    if classes {
        out_fields.push(Field::new("CLASS", FieldType::Text));
    }
    // Each label's size, colour and angle by expression kept as fields the annotation's classes
    // draw by, when a class has them.
    let kept = |pick: fn(&rustgis_map::label_by::LabelBy) -> bool| l.labels.in_order().any(|c| c.visible && pick(&c.by));
    let (by_size, by_color) = (kept(|b| b.size.is_some()), kept(|b| b.color.is_some()));
    let by_angle = l.labels.in_order().any(|c| c.visible && c.by.angle.is_some() && !(only_lines && c.placement.along_line()));
    // Texts cut to their most characters, said once.
    let mut cut = 0usize;
    for (on, name, kind) in
        [(by_size, "SIZE", FieldType::Double), (by_color, "COLOR", FieldType::Text), (by_angle, "ANGLE", FieldType::Double)]
    {
        if on {
            out_fields.push(Field::new(name, kind));
        }
    }
    let mut ds = Dataset::new(
        ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).map(String::from).unwrap_or_else(|| format!("{} Annotation", l.name)),
        GeometryType::Point,
        out_fields,
    );
    for (row, ft) in l.dataset.features.iter().enumerate() {
        if rows.get(row) != Some(&true) {
            continue;
        }
        let Some(at) = ft.geometry.as_ref().and_then(Geometry::label_point) else { continue };
        for (i, class) in matcher.classes_of(&l.dataset, row).filter(|(_, c)| c.visible) {
            let text = match by_text(i).and_then(|e| e.text_cut(&l.dataset, row)) {
                Some(Some((t, was_cut))) => {
                    cut += usize::from(was_cut);
                    t
                }
                Some(None) => continue,
                None => {
                    let Some(fi) = fields.get(i).copied().flatten() else { continue };
                    let v = l.dataset.value(row, fi);
                    if v.is_null() {
                        continue;
                    }
                    v.to_string()
                }
            };
            let mut attrs = vec![Attr::Text(text)];
            if classes {
                attrs.push(Attr::Text(class.name.clone()));
            }
            let e = exprs.get(i).and_then(Option::as_ref);
            let number = |v: Option<f32>| v.map_or(Attr::Null, |v| Attr::Float(f64::from(v)));
            if by_size {
                attrs.push(number(e.and_then(|e| e.size(&l.dataset, row))));
            }
            if by_color {
                let c = e.and_then(|e| e.color(&l.dataset, row));
                attrs.push(c.map_or(Attr::Null, |c| Attr::Text(format!("{},{},{},{}", c.r(), c.g(), c.b(), c.a()))));
            }
            if by_angle {
                let turns = class.by.angle.is_some() && !along(class, ft.geometry.as_ref());
                attrs.push(number(e.filter(|_| turns).and_then(|e| e.angle(&l.dataset, row))));
            }
            ds.push(Some(Geometry::Point(at)), attrs);
        }
    }
    if ds.features.is_empty() {
        return Err("the layer has no labels to convert".into());
    }
    let n = ds.features.len();
    let cut_note = (cut > 0).then(|| {
        format!(
            "{}: {cut} label text{} cut to {} characters",
            ds.name,
            if cut == 1 { "" } else { "s" },
            rustgis_map::label_by::MAX_LABEL_TEXT
        )
    });
    let mut labels = l.labels.clone();
    labels.enabled = true;
    // The classes that count, as for the layer's own labels.
    labels.classes.truncate(MAX_LABEL_CLASSES);
    for c in std::iter::once(&mut labels.default).chain(labels.classes.iter_mut()) {
        c.field = "TEXT".into();
        // On its point, never round it: an annotation stays where it is (#1011).
        c.placement = LabelPlacement::Center;
        c.other_positions = false;
        // Each class picks the points it labelled, Default too once there are others (#732):
        // with no clause it would label theirs as well.
        c.where_ = classes.then(|| format!("\"CLASS\" = '{}'", c.name.replace('\'', "''")));
        // Its text is in TEXT; its size, colour and angle by expression in their fields.
        let field = |on: bool, had: &Option<String>, name: &str| (on && had.is_some()).then(|| format!("\"{name}\""));
        c.by = rustgis_map::label_by::LabelBy {
            text: None,
            size: field(by_size, &c.by.size, "SIZE"),
            color: field(by_color, &c.by.color, "COLOR"),
            angle: field(by_angle, &c.by.angle, "ANGLE"),
        };
    }
    let (min_scale, max_scale) = (l.min_scale, l.max_scale);
    if let Some(src) = s.map_mut().layer_mut(id) {
        src.labels.enabled = false;
    }
    let new_id = super::layer::add_dataset(s, ds);
    if let Some(a) = s.map_mut().layer_mut(new_id) {
        // The annotation is all text: the points themselves are invisible.
        let clear = Rgba::new(0, 0, 0, 0);
        a.renderer = Renderer::Single(Symbol { fill: clear, stroke: clear, stroke_width: 0.0, size: 0.0, marker: Default::default() });
        a.labels = labels;
        a.min_scale = min_scale;
        a.max_scale = max_scale;
    }
    if let Some(note) = cut_note {
        s.notify(note, false);
    }
    Ok(json!({"layer": new_id, "annotations": n}))
}
