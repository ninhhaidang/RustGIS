//! Layer groups (#413): related layers under one name, shown, hidden, made see-through and
//! limited to a scale range together.

use rustgis_map::{LayerGroup, MAX_GROUPS, Map};
use serde_json::{Value, json};

use super::{f, ps, uint};
use crate::{CmdResult, CommandSpec as C, Session};

pub fn commands() -> Vec<C> {
    vec![
        C::new("group.add", "New Group", "Layers", add).params(
            r#"{"name"?: text (default New Group), "layers"?: [name|id] (moved into it, out of any other group; together in the drawing order at the topmost)}"#,
        ),
        C::new("group.set", "Group Properties", "Layers", set).params(
            r#"{"group": name|id, "name"?: text, "visible"?: bool (toggles when no other property is given), "transparency"?: 0-100 (on top of each layer's own)}"#,
        ),
        C::new("group.expand", "Expand Group", "Layers", expand)
            .params(r#"{"group": name|id, "on"?: bool (toggles when omitted)}"#)
            .view(),
        C::new("group.scaleRange", "Group Visibility Range", "Layers", scale_range).params(
            r#"{"group": name|id, "min"?: scale (out beyond), "max"?: scale (in beyond); both set on each call, one left out is cleared; its layers draw only within it and their own}"#,
        ),
        C::new("group.move", "Move Group", "Layers", move_group).params(
            r#"{"group": name|id, "to": position among the other layers, counted in layers (0 = top; past the end, the bottom; never inside another group: above it)}"#,
        ),
        C::new("group.ungroup", "Ungroup", "Layers", ungroup).params(r#"{"group": name|id} (its layers stay, out of any group)"#),
        C::new("group.remove", "Remove Group", "Layers", remove).params(r#"{"group": name|id} (and its layers)"#),
        C::new("layer.group", "Move To Group", "Layers", move_to_group)
            .when(super::need_layer)
            .params(r#"{"layer"?: name|id | [name|id] (default: the layer selection), "group": name|id|null (null takes it out of its group), "before"?: name|id (the layer it goes just above: one of the group's, or out of a group any other; without it, next to the group's layers, or where it is when taken out)}"#),
    ]
}

/// A `group` parameter as the id of one of the map's groups: by id, or by name (ignoring case),
/// else a name that is an id.
pub(crate) fn group_id(m: &Map, p: &Value) -> Result<u32, String> {
    match p.get("group") {
        Some(Value::Number(n)) => n
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .filter(|id| m.group(*id).is_some())
            .ok_or_else(|| format!("no group with id {n}")),
        Some(Value::String(name)) => m
            .groups
            .iter()
            .find(|g| g.name.eq_ignore_ascii_case(name.trim()))
            .map(|g| g.id)
            .or_else(|| name.trim().parse::<u32>().ok().filter(|id| m.group(*id).is_some()))
            .ok_or_else(|| format!("no group named '{name}'")),
        _ => Err("give the group's name or id (\"group\")".into()),
    }
}

/// A layer that can join a group: not one of a service's collections (its service group keeps it).
fn groupable(s: &Session, id: u32) -> Result<u32, String> {
    if s.map().service_groups.iter().any(|g| g.children.iter().any(|c| c.layer == id)) {
        let name = s.map().layer(id).map(|l| l.name.clone()).unwrap_or_default();
        return Err(format!("'{name}' belongs to a service group"));
    }
    Ok(id)
}

fn name_of(p: &Value) -> Result<Option<String>, String> {
    if p.get("name").is_some_and(|v| !v.is_null() && !v.is_string()) {
        return Err("name must be text".into());
    }
    match ps(p, "name") {
        Some(n) => {
            let n: String = n.trim().chars().take(200).collect();
            if n.is_empty() { Err("name is required".into()) } else { Ok(Some(n)) }
        }
        None => Ok(None),
    }
}

fn add(s: &mut Session, p: &Value) -> CmdResult {
    if s.map().groups.len() >= MAX_GROUPS {
        return Err(format!("a map holds at most {MAX_GROUPS} groups"));
    }
    let name = name_of(p)?.unwrap_or_else(|| "New Group".into());
    let mut children = Vec::new();
    if let Some(layers) = p.get("layers") {
        let layers = layers.as_array().ok_or("layers must be a list of layer names or ids")?;
        for v in layers {
            let id = groupable(s, s.layer_id(&json!({"layer": v}))?)?;
            if !children.contains(&id) {
                children.push(id);
            }
        }
    }
    // In drawing order, so gathering keeps them as they were.
    let m = s.map_mut();
    children.sort_by_key(|id| m.layer_index(*id));
    let mut group = LayerGroup::new(name);
    group.children = children;
    let id = m.add_group(group);
    m.gather_group(id);
    Ok(json!({"group": id}))
}

fn set(s: &mut Session, p: &Value) -> CmdResult {
    let id = group_id(s.map(), p)?;
    let name = name_of(p)?;
    let transparency = match p.get("transparency") {
        Some(v) => {
            Some(v.as_f64().filter(|t| t.is_finite()).ok_or("transparency must be a number from 0 to 100")?.clamp(0.0, 100.0) as f32)
        }
        None => None,
    };
    let visible = bool_of(p, "visible")?;
    let toggle = visible.is_none() && name.is_none() && transparency.is_none();
    let g = s.map_mut().groups.iter_mut().find(|g| g.id == id).ok_or("no such group")?;
    if let Some(n) = name {
        g.name = n;
    }
    if let Some(t) = transparency {
        g.transparency = t;
    }
    if toggle {
        g.visible = !g.visible;
    } else if let Some(v) = visible {
        g.visible = v;
    }
    Ok(json!({"group": id, "visible": g.visible}))
}

fn scale_range(s: &mut Session, p: &Value) -> CmdResult {
    let id = group_id(s.map(), p)?;
    // Like layer.scaleRange: each call sets both bounds (one left out is cleared), but a bound
    // given must be a positive scale.
    let bound = |k: &str| match p.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(_) => f(p, k).filter(|v| v.is_finite() && *v > 0.0).map(Some).ok_or_else(|| format!("{k} must be a scale greater than 0")),
    };
    let (min, max) = (bound("min")?, bound("max")?);
    let g = s.map_mut().groups.iter_mut().find(|g| g.id == id).ok_or("no such group")?;
    g.min_scale = min;
    g.max_scale = max;
    Ok(Value::Null)
}

/// A boolean parameter: absent, or a bool (anything else is an error rather than a toggle).
fn bool_of(p: &Value, k: &str) -> Result<Option<bool>, String> {
    match p.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_bool().map(Some).ok_or_else(|| format!("{k} must be true or false")),
    }
}

/// Folds or unfolds a group in the Layers pane (a view change: no undo step).
fn expand(s: &mut Session, p: &Value) -> CmdResult {
    let id = group_id(s.map(), p)?;
    let on = bool_of(p, "on")?;
    let g = s.map_mut().groups.iter_mut().find(|g| g.id == id).ok_or("no such group")?;
    g.expanded = on.unwrap_or(!g.expanded);
    Ok(json!({"group": id, "expanded": g.expanded}))
}

/// Moves a group's layers, together, to a position in the drawing order of the other layers.
fn move_group(s: &mut Session, p: &Value) -> CmdResult {
    let id = group_id(s.map(), p)?;
    let to = uint(p, "to").ok_or("to is required: the position among the other layers, 0 at the top")?;
    s.map_mut().move_group(id, usize::try_from(to).unwrap_or(usize::MAX))?;
    Ok(Value::Null)
}

fn ungroup(s: &mut Session, p: &Value) -> CmdResult {
    let id = group_id(s.map(), p)?;
    s.map_mut().groups.retain(|g| g.id != id);
    Ok(Value::Null)
}

fn remove(s: &mut Session, p: &Value) -> CmdResult {
    let id = group_id(s.map(), p)?;
    let children = s.map().group(id).map(|g| g.children.clone()).unwrap_or_default();
    crate::cmd::layer::drop_layers(s, &children);
    s.map_mut().groups.retain(|g| g.id != id);
    if s.active_layer.is_some_and(|a| children.contains(&a)) {
        s.active_layer = s.map().layers.first().map(|l| l.id);
    }
    Ok(Value::Null)
}

fn move_to_group(s: &mut Session, p: &Value) -> CmdResult {
    let group = match p.get("group") {
        None => return Err("give the group's name or id (\"group\"), or null to take the layer out of its group".into()),
        Some(Value::Null) => None,
        Some(_) => Some(group_id(s.map(), p)?),
    };
    // Several (a list, or the layer selection): each in or out of the group, one undo step.
    let ids = super::layer::layer_ids(s, p)?;
    if ids.len() > 1 {
        if p.get("before").is_some_and(|v| !v.is_null()) {
            return Err("\"before\" places one layer: give a single layer with it".into());
        }
        if group.is_some() {
            for id in &ids {
                groupable(s, *id)?;
            }
        }
        let m = s.map_mut();
        let mut ids = ids;
        ids.sort_by_key(|id| m.layer_index(*id));
        for id in &ids {
            m.set_group(*id, group);
        }
        return Ok(json!({"layers": ids, "group": group}));
    }
    let layer = ids.first().copied().ok_or("no such layer")?;
    if group.is_some() {
        groupable(s, layer)?;
    }
    match p.get("before") {
        Some(v) if !v.is_null() => {
            let before = s.layer_id(&json!({"layer": v}))?;
            s.map_mut().place_layer(layer, group, Some(before))?;
        }
        _ => s.map_mut().set_group(layer, group),
    }
    Ok(json!({"layer": layer, "group": group}))
}
