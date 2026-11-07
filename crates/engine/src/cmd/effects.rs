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
            with(s, p, |l| l.aggregation = agg)
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
                r#"{"layer"?, "type": "none"|"height"|"field", "field"?: numeric field (metres), "height"?: metres, "factor"?: number}"#,
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
    ]
}

fn display_filters(s: &mut Session, p: &Value) -> CmdResult {
    let id = s.layer_id(p)?;
    let ds = &s.map().layer(id).ok_or("no such layer")?.dataset;
    let list = p.get("filters").and_then(Value::as_array).ok_or("filters is required (an empty list clears them)")?;
    let mut filters = Vec::new();
    for v in list.iter().take(50) {
        let w = v.get("where").and_then(Value::as_str).map(str::trim).filter(|w| !w.is_empty()).ok_or("each filter has a where clause")?;
        rustgis_data::query::parse(w, ds)?;
        let scale = |k: &str| v.get(k).and_then(Value::as_f64).filter(|x| x.is_finite() && *x > 0.0);
        filters.push(DisplayFilter {
            name: v.get("name").and_then(Value::as_str).unwrap_or("Filter").chars().take(100).collect(),
            min_scale: scale("minScale"),
            max_scale: scale("maxScale"),
            where_: w.to_string(),
        });
    }
    let n = filters.len();
    with(s, p, |l| l.display_filters = filters)?;
    Ok(json!({"filters": n}))
}

fn extrusion(s: &mut Session, p: &Value) -> CmdResult {
    let id = s.layer_id(p)?;
    let l = s.map().layer(id).ok_or("no such layer")?;
    let prev = l.extrusion.clone();
    let x = match ps(p, "type").ok_or("type is required")? {
        "none" => None,
        t @ ("height" | "field") => {
            let field = if t == "field" {
                let n = ps(p, "field").ok_or("field is required")?;
                Some(
                    l.dataset
                        .field_index(n)
                        .and_then(|i| l.dataset.fields.get(i))
                        .filter(|f| f.ty.is_numeric())
                        .map(|f| f.name.clone())
                        .ok_or_else(|| format!("no numeric field '{n}'"))?,
                )
            } else {
                None
            };
            let keep =
                prev.unwrap_or(Extrusion { field: None, height: 0.0, factor: 1.0, face_culling: true, lighting: true, shadows: false });
            Some(Extrusion {
                field,
                height: f(p, "height").unwrap_or(keep.height.max(1000.0)).clamp(0.0, 1e7),
                factor: f(p, "factor").unwrap_or(keep.factor).clamp(0.0, 1e6),
                ..keep
            })
        }
        t => return Err(format!("unknown extrusion type '{t}'")),
    };
    let globe = s.map().projection.is_globe();
    let mut r = with(s, p, |l| l.extrusion = x)?;
    // The globe draws polygons flat; extrusion shows on flat maps.
    if globe && let Some(o) = r.as_object_mut() {
        o.insert("note".into(), json!("extrusion shows on flat maps; the globe draws the polygons flat"));
    }
    Ok(r)
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
