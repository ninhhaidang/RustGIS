//! Style tab effects: blending, masking, swipe, raster rotation, display
//! filters, point aggregation, and extrusion with face culling, lighting and shadows.

use rustgis_data::GeometryType;
use rustgis_map::effects::{Aggregation, Blend, DisplayFilter, Extrusion};
use serde_json::{Value, json};

use super::layer::with;
use super::{b, f, need_feature_layer, need_layer, ps};
use crate::{CmdResult, CommandSpec as C, Session};

/// A raster layer is active.
fn need_raster(s: &Session) -> Option<&'static str> {
    match s.active_layer.and_then(|id| s.map().layer(id)) {
        Some(l) if l.is_raster() => None,
        Some(_) => Some("this works on raster layers"),
        None => Some("select a layer in the Layers pane first"),
    }
}

fn need_polygons(s: &Session) -> Option<&'static str> {
    match s.active_layer.and_then(|id| s.map().layer(id)) {
        Some(l) if l.geometry_type() == GeometryType::Polygon => None,
        Some(_) => Some("this works on polygon layers"),
        None => Some("select a layer in the Layers pane first"),
    }
}

pub fn commands() -> Vec<C> {
    vec![
        C::new("layer.blend", "Layer Blend", "Style › Effects", |s, p| {
            let m = ps(p, "mode").ok_or("mode is required")?;
            let mode = Blend::from_code(m).ok_or_else(|| format!("unknown blend mode '{m}'"))?;
            with(s, p, |l| l.blend = mode)
        })
        .roles(&[])
        .effects(crate::policy::Effects::NONE)
        .when(need_layer)
        .params(r#"{"layer"?, "mode": "normal"|"multiply"|"screen"|"overlay"|"darken"|"lighten"|"difference"}"#),
        C::new("layer.mask", "Masking", "Style › Drawing", |s, p| {
            let id = s.layer_id(p)?;
            let mask = mask_of(s, p, id)?;
            with(s, p, |l| l.mask = mask.clone())
        })
        .inputs(mask_inputs)
        .roles(&[])
        .effects(crate::policy::Effects::NONE)
        .when(need_layer)
        .params(r#"{"layer"?, "mask": polygon layer|null}"#),
        C::new("layer.swipe", "Swipe", "Style › Compare", |s, p| {
            let id = s.layer_id(p)?;
            let pos = match p.get("position") {
                None | Some(Value::Null) => None,
                Some(v) => Some(v.as_f64().filter(|x| x.is_finite()).ok_or("position is 0 to 1 across the view")?),
            };
            s.map_mut().swipe = match (pos, b(p, "off")) {
                (_, Some(true)) => None,
                (Some(x), _) => Some((id, x.clamp(0.0, 1.0))),
                (None, _) => Some((id, 0.5)),
            };
            Ok(json!({"swipe": s.map().swipe.map(|(l, x)| json!({"layer": l, "position": x}))}))
        })
        .roles(&[])
        .effects(crate::policy::Effects::NONE)
        .when(need_layer)
        .params(r#"{"layer"?, "position"?: 0..1, "off"?: bool}"#),
        C::new("raster.rotation", "Rotation Type", "Style › Rotation", |s, p| {
            let a = f(p, "angle").ok_or("angle is required (degrees clockwise)")?;
            let id = s.layer_id(p)?;
            let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
            let rl = l.raster.as_mut().ok_or("this works on raster layers")?;
            rl.style.rotation = a.rem_euclid(360.0);
            Ok(json!({"angle": rl.style.rotation}))
        })
        .roles(&[])
        .effects(crate::policy::Effects::NONE)
        .when(need_raster)
        .params(r#"{"layer"?, "angle": degrees clockwise}"#),
        C::new("layer.displayFilters", "Display Filters", "Style › Drawing", display_filters)
            .roles(&[])
            .effects(crate::policy::Effects::NONE)
            .when(need_feature_layer)
            .params(r#"{"layer"?, "filters": [{"name", "minScale"?, "maxScale"?, "where"}] (empty to clear)}"#),
        C::new("layer.aggregation", "Aggregation", "Style › Drawing", |s, p| {
            let id = s.layer_id(p)?;
            if s.map().layer(id).map(|l| l.geometry_type()) != Some(GeometryType::Point) {
                return Err("aggregation bins point layers".into());
            }
            let agg = match ps(p, "shape") {
                None | Some("none") => None,
                Some(sh @ ("hexagon" | "square")) => {
                    Some(Aggregation { hexagons: sh == "hexagon", size: f(p, "size").unwrap_or(24.0).clamp(4.0, 400.0) })
                }
                Some(o) => return Err(format!("unknown bin shape '{o}'")),
            };
            let r = with(s, p, |l| l.aggregation = agg)?;
            super::diagram::hidden_notice(s, id);
            Ok(r)
        })
        .roles(&[])
        .effects(crate::policy::Effects::NONE)
        .when(need_feature_layer)
        .params(r#"{"layer"?, "shape": "hexagon"|"square"|"none", "size"?: points}"#),
        C::new("layer.extrusion", "Layer Extrusion", "Style › Extrusion", extrusion)
            .roles(&[])
            .effects(crate::policy::Effects::NONE)
            .when(need_polygons)
            .params(
                r#"{"layer"?: name|id | [name|id] (default: the active layer; a list is set alike in one step, or refused whole), "type": "none"|"height"|"field", "field"?: numeric field (metres), "height"?: metres, "factor"?: number}"#,
            ),
        C::new("layer.faces", "Faces", "Style › Faces", |s, p| {
            let (cull, light) = (b(p, "culling"), b(p, "lighting"));
            change_extrusion(s, p, |x| {
                if let Some(v) = cull {
                    x.face_culling = v;
                }
                if let Some(v) = light {
                    x.lighting = v;
                }
            })
        })
        .roles(&[])
        .effects(crate::policy::Effects::NONE)
        .when(need_polygons)
        .params(r#"{"layer"?, "culling"?: bool, "lighting"?: bool}"#),
        C::new("layer.lightShadow", "Light and Shadow", "Style › Light and Shadow", |s, p| {
            let (light, shadow) = (b(p, "lighting"), b(p, "shadows"));
            change_extrusion(s, p, |x| {
                if let Some(v) = light {
                    x.lighting = v;
                }
                if let Some(v) = shadow {
                    x.shadows = v;
                }
            })
        })
        .roles(&[])
        .effects(crate::policy::Effects::NONE)
        .when(need_polygons)
        .params(r#"{"layer"?, "lighting"?: bool, "shadows"?: bool}"#),
        C::new("layer.extrusionBase", "Extrusion Base", "Style › Extrusion", extrusion_base)
            .roles(&[])
            .effects(crate::policy::Effects::NONE)
            .when(need_polygons)
            .params(r#"{"layer"?: name|id | [name|id] (as layer.extrusion), "field"?: numeric field (metres; null or "" clears it), "base"?: metres (0 to 10,000,000)}"#),
    ]
}

fn display_filters(s: &mut Session, p: &Value) -> CmdResult {
    let id = s.layer_id(p)?;
    let ds = &s.map().layer(id).ok_or("no such layer")?.dataset;
    let list = p.get("filters").and_then(Value::as_array).ok_or("filters is required (an empty list clears them)")?;
    let mut filters = Vec::new();
    super::at_most("filters", "a layer", list, 50)?;
    for v in list {
        let w = v.get("where").and_then(Value::as_str).map(str::trim).filter(|w| !w.is_empty()).ok_or("each filter has a where clause")?;
        rustgis_data::query::parse(w, ds)?;
        let scale = |k: &str| v.get(k).and_then(Value::as_f64).filter(|x| x.is_finite() && *x > 0.0);
        filters.push(DisplayFilter {
            name: super::within(
                "a filter's name",
                "a display filter's name",
                v.get("name").and_then(Value::as_str).unwrap_or("Filter"),
                100,
            )?,
            min_scale: scale("minScale"),
            max_scale: scale("maxScale"),
            where_: w.to_string(),
        });
    }
    let n = filters.len();
    with(s, p, |l| l.display_filters = filters)?;
    Ok(json!({"filters": n}))
}

/// The layers an extrusion command changes, and whether they were given as a list: `"layer"` as a
/// list of names or ids (each once), else the one layer it names, or the active layer.
fn targets(s: &Session, p: &Value) -> Result<(Vec<u32>, bool), String> {
    if p.get("layer").is_some_and(Value::is_array) { Ok((super::layer::layer_ids(s, p)?, true)) } else { Ok((vec![s.layer_id(p)?], false)) }
}

/// The names of the layers among `ids` that `bad` picks, quoted and joined ("'A', 'B'"; past five,
/// the first five and "and 3 more"), or `None` when it picks none.
fn named_where(s: &Session, ids: &[u32], bad: impl Fn(&rustgis_map::Layer) -> bool) -> Option<String> {
    let names: Vec<String> = ids
        .iter()
        .filter_map(|id| s.map().layer(*id))
        .filter(|l| bad(l))
        .map(|l| format!("'{}'", crate::project::short_name(&l.name)))
        .collect();
    match names.len() {
        0 => None,
        n if n > 5 => Some(format!("{} and {} more", names[..5].join(", "), n - 5)),
        _ => Some(names.join(", ")),
    }
}

/// A list of layers is checked whole before any changes, so a refusal leaves every one as it was:
/// each must be a polygon layer.
fn all_polygons(s: &Session, ids: &[u32]) -> Result<(), String> {
    match named_where(s, ids, |l| l.geometry_type() != GeometryType::Polygon) {
        Some(names) => Err(format!("extrusion raises polygon layers, and these aren't: {names}; no layer changed")),
        None => Ok(()),
    }
}

/// A list of layers must all have the numeric field `n`, or none changes.
fn all_have(s: &Session, ids: &[u32], n: &str) -> Result<(), String> {
    match named_where(s, ids, |l| numeric_field(&l.dataset, n).is_err()) {
        Some(names) => Err(format!("no numeric field '{n}' in {names}; no layer changed")),
        None => Ok(()),
    }
}

/// `layer.extrusion`: one layer, or a list of them set alike in one undo step. Each keeps its own
/// height and factor when they are left out, as one layer does.
fn extrusion(s: &mut Session, p: &Value) -> CmdResult {
    let (ids, listed) = targets(s, p)?;
    let t = ps(p, "type").ok_or("type is required")?;
    let field = match t {
        "none" | "height" => None,
        "field" => Some(ps(p, "field").ok_or("field is required")?),
        t => return Err(format!("unknown extrusion type '{t}'")),
    };
    if listed {
        all_polygons(s, &ids)?;
        if let Some(n) = field {
            all_have(s, &ids, n)?;
        }
    }
    let mut next = Vec::with_capacity(ids.len());
    for &id in &ids {
        let l = s.map().layer(id).ok_or("no such layer")?;
        let x = if t == "none" {
            None
        } else {
            let own = field.map(|n| numeric_field(&l.dataset, n)).transpose()?;
            // A new extrusion starts at 1,000 m, so it shows; an extruded layer keeps its own height
            // (the fallback where the field holds no number), as it keeps its factor. A constant
            // height of 0 shows nothing, so one kept at 0 (a city model's fallback) starts at
            // 1,000 m too.
            let height = l.extrusion.as_ref().map(|x| x.height).filter(|h| h.is_finite() && (t != "height" || *h > 0.0)).unwrap_or(1000.0);
            let keep = l.extrusion.clone().unwrap_or_default();
            Some(Extrusion {
                field: own,
                height: f(p, "height").unwrap_or(height).clamp(0.0, 1e7),
                factor: f(p, "factor").unwrap_or(keep.factor).clamp(0.0, 1e6),
                ..keep
            })
        };
        next.push((id, x));
    }
    for (id, x) in &mut next {
        if let Some(l) = s.map_mut().layer_mut(*id) {
            l.extrusion = x.take();
        }
    }
    // Diagrams the extrusion hides: one notice, as for one layer, with how many more it holds for.
    let hidden: Vec<String> = next.iter().filter_map(|(id, _)| s.map().layer(*id).and_then(super::diagram::hidden_text)).collect();
    match hidden.as_slice() {
        [] => {}
        [one] => s.notify(one.clone(), false),
        [first, rest @ ..] => {
            let more = if rest.len() == 1 { "1 more layer".to_string() } else { format!("{} more layers", rest.len()) };
            s.notify(format!("{first} (and {more} like it)"), false)
        }
    }
    // The globe draws polygons flat; extrusion shows on flat maps.
    Ok(if s.map().projection.is_globe() {
        json!({"note": "extrusion shows on flat maps; the globe draws the polygons flat"})
    } else {
        Value::Null
    })
}

/// The name of the numeric field `n` (matched as `field_index` matches), or why there is none.
pub(crate) fn numeric_field(ds: &rustgis_data::Dataset, n: &str) -> Result<String, String> {
    ds.field_index(n)
        .and_then(|i| ds.fields.get(i))
        .filter(|f| f.ty.is_numeric())
        .map(|f| f.name.clone())
        .ok_or_else(|| format!("no numeric field '{n}'"))
}

/// `layer.extrusionBase`: what an extruded layer's walls stand on, a numeric field (`field`) else
/// a constant (`base`), in metres times the extrusion's factor. Each left out stays as it is, so
/// the height, its field and the factor never change here; null or an empty name clears the
/// field, and a base that is no number, or is outside 0 to 1e7 metres (the height's limit), is
/// refused rather than changed. A list of layers is set alike in one undo step, and only when every
/// one is an extruded polygon layer with the field given.
fn extrusion_base(s: &mut Session, p: &Value) -> CmdResult {
    let (ids, listed) = targets(s, p)?;
    if listed {
        all_polygons(s, &ids)?;
        if let Some(names) = named_where(s, &ids, |l| l.extrusion.is_none()) {
            return Err(format!("turn extrusion on first (Layer Extrusion) in {names}; no layer changed"));
        }
    } else if ids.first().and_then(|id| s.map().layer(*id)).ok_or("no such layer")?.extrusion.is_none() {
        return Err("turn extrusion on first (Layer Extrusion)".into());
    }
    let name = match p.get("field") {
        None => None,
        Some(Value::Null) => Some(None),
        Some(v) => match v.as_str().ok_or("field is a field name, or null to clear it")? {
            "" => Some(None),
            n => Some(Some(n)),
        },
    };
    if listed && let Some(Some(n)) = name {
        all_have(s, &ids, n)?;
    }
    // Each layer's own spelling of the field, as `field_index` matches it.
    let mut fields = Vec::with_capacity(ids.len());
    for &id in &ids {
        let l = s.map().layer(id).ok_or("no such layer")?;
        fields.push(match name {
            Some(Some(n)) => Some(Some(numeric_field(&l.dataset, n)?)),
            Some(None) => Some(None),
            None => None,
        });
    }
    let base = match p.get("base") {
        None => None,
        Some(_) => {
            let b = f(p, "base").ok_or("base is a number of metres")?;
            if !(0.0..=1e7).contains(&b) {
                return Err("base is between 0 and 10,000,000 metres".into());
            }
            Some(b)
        }
    };
    for (id, field) in ids.into_iter().zip(fields) {
        if let Some(x) = s.map_mut().layer_mut(id).and_then(|l| l.extrusion.as_mut()) {
            if let Some(v) = field {
                x.base_field = v;
            }
            if let Some(v) = base {
                x.base = v;
            }
        }
    }
    Ok(Value::Null)
}

fn change_extrusion(s: &mut Session, p: &Value, f: impl FnOnce(&mut Extrusion)) -> CmdResult {
    let id = s.layer_id(p)?;
    let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
    let x = l.extrusion.as_mut().ok_or("turn extrusion on first (Layer Extrusion)")?;
    f(x);
    Ok(Value::Null)
}

/// Binds the primary target and literal mask, retaining names resolved by earlier planned steps.
fn mask_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let mut inputs = super::Inputs::single_layer(input, p)?;
    if let Some(name) = mask_name(p)? {
        // Preserve known live validation; future names are bound by the planner and checked
        // against the actual polygon and target when the handler runs.
        if input.map().layer_by_name(name).is_some() {
            mask_layer(input.map(), name, input.layer_id(p).ok())?;
        }
        inputs.extend(super::Inputs::named(input.map(), [name])?);
    }
    Ok(inputs)
}

/// Parses the optional literal name exactly as the mask handler consumes it.
fn mask_name(p: &Value) -> Result<Option<&str>, String> {
    match p.get("mask") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(n)) if n.trim().is_empty() => Ok(None),
        Some(Value::String(n)) => Ok(Some(n)),
        Some(_) => Err("mask is a polygon layer's name, or null".into()),
    }
}

/// Validates a live polygon mask, including the existing self-mask refusal.
fn mask_layer<'a>(map: &'a rustgis_map::Map, name: &str, id: Option<u32>) -> Result<&'a rustgis_map::Layer, String> {
    let mask = map
        .layer_by_name(name)
        .filter(|l| l.geometry_type() == GeometryType::Polygon)
        .ok_or_else(|| format!("no polygon layer '{name}'"))?;
    if Some(mask.id) == id {
        return Err("a layer can't mask itself".into());
    }
    Ok(mask)
}

/// Resolves and validates the literal mask against the actual execution map.
fn mask_of(input: &impl super::InputSource, p: &Value, id: u32) -> Result<Option<String>, String> {
    let context = input.input_context();
    mask_name(p)?.map(|name| mask_layer(context.map(), name, Some(id)).map(|mask| mask.name.clone())).transpose()
}
