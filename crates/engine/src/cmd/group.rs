//! Layer groups (#413): related layers under one name, shown, hidden, made see-through and
//! limited to a scale range together. Groups nest (#836): a group in another goes with it, and
//! every group above a layer applies to it.

use rustgis_map::{LayerGroup, MAX_GROUP_DEPTH, MAX_GROUPS, Map, Outline};
use serde_json::{Value, json};

use super::{f, ps, uint};
use crate::{CmdResult, CommandSpec as C, Session};

pub fn commands() -> Vec<C> {
    vec![
        C::new("group.add", "New Group", "Layers", add).inputs(add_inputs).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r#"{"name"?: text (default New Group), "parent"?: name|id (the group it goes in), "layers"?: [name|id] (moved into it, out of any other group; together in the drawing order at the topmost)}"#,
        ),
        C::new("group.set", "Group Properties", "Layers", set).inputs(group_inputs).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r#"{"group": name|id, "name"?: text, "visible"?: bool (toggles when no other property is given), "transparency"?: 0-100 (on top of each layer's own)}"#,
        ),
        C::new("group.expand", "Expand Group", "Layers", expand).inputs(group_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
            .params(r#"{"group": name|id, "on"?: bool (toggles when omitted)}"#)
            .view(),
        C::new("group.scaleRange", "Group Visibility Range", "Layers", scale_range).inputs(group_inputs).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r#"{"group": name|id, "min"?: scale (out beyond), "max"?: scale (in beyond); both set on each call, one left out is cleared; its layers draw only within it and their own}"#,
        ),
        C::new("group.move", "Move Group", "Layers", move_group).inputs(group_inputs).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r#"{"group": name|id, "to": position among the other layers, counted in layers (0 = top; past the end, the bottom; never inside another group: above it; a group in another stays in it, at most at its top or bottom)}"#,
        ),
        C::new("group.setParent", "Move Group To Group", "Layers", set_parent).inputs(parent_inputs).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r#"{"group": name|id, "parent": name|id|null (the group it goes in; null: the top level), "before"?: name|id|null (a layer or group: places the group just above it; null puts it at the end of the parent, or the bottom at the top level; omitted: next to the parent's layers, or where it is at the top level)}"#,
        ),
        C::new("group.indent", "Indent Group", "Layers", indent_group).inputs(indent_group_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
            .params(r#"{"group": name|id} (into the group just above it in the Layers pane, as its last item)"#),
        C::new("group.outdent", "Outdent Group", "Layers", outdent_group).inputs(outdent_group_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
            .params(r#"{"group": name|id} (out of the group it is in, up one level, just below that group)"#),
        C::new("group.ungroup", "Ungroup", "Layers", ungroup).inputs(group_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
            .params(r#"{"group": name|id} (its layers and subgroups stay, one level up)"#),
        C::new("group.remove", "Remove Group", "Layers", remove).inputs(group_inputs).roles(&[]).effects(crate::policy::Effects::NONE).params(r#"{"group": name|id} (and its layers and subgroups)"#),
        C::new("layer.group", "Move To Group", "Layers", move_to_group).inputs(layer_group_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
            .when(super::need_layer)
            .params(r#"{"layer"?: name|id | [name|id] (default: the layer selection), "group": name|id|null (null takes it out of its group), "before"?: name|id|null (places a single layer just above this layer, one of the group's or its subgroups'; null puts it at the group's end, or the bottom out of a group; omitted: next to the group's layers, or where it is when taken out)}"#),
        C::new("layer.indent", "Indent Layer", "Layers", indent_layer).inputs(indent_layer_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
            .when(super::need_layer)
            .params(r#"{"layer"?: name|id (default: the active layer)} (into the group just above it in the Layers pane, as its last item)"#),
        C::new("layer.outdent", "Outdent Layer", "Layers", outdent_layer).inputs(outdent_layer_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
            .when(super::need_layer)
            .params(r#"{"layer"?: name|id (default: the active layer)} (out of its group, up one level, just below that group)"#),
    ]
}

/// A `group` parameter as the id of one of the map's groups: by id, or by name (ignoring case),
/// else a name that is an id.
pub(crate) fn group_id(m: &Map, p: &Value) -> Result<u32, String> {
    group_ref(m, p.get("group")).ok_or_else(|| match p.get("group") {
        Some(Value::Number(n)) => format!("no group with id {n}"),
        Some(Value::String(name)) => format!("no group named '{name}'"),
        _ => "give the group's name or id (\"group\")".into(),
    })
}

/// A group named by id, or by name (ignoring case), else a name that is an id.
fn group_ref(m: &Map, v: Option<&Value>) -> Option<u32> {
    match v? {
        Value::Number(n) => n.as_u64().and_then(|n| u32::try_from(n).ok()).filter(|id| m.group(*id).is_some()),
        Value::String(name) => m
            .groups
            .iter()
            .find(|g| g.name.eq_ignore_ascii_case(name.trim()))
            .map(|g| g.id)
            .or_else(|| name.trim().parse::<u32>().ok().filter(|id| m.group(*id).is_some())),
        _ => None,
    }
}

/// A `parent` parameter: a group, or null for the top level.
fn parent_of(m: &Map, p: &Value) -> Result<Option<u32>, String> {
    match p.get("parent") {
        None | Some(Value::Null) => Ok(None),
        v => group_ref(m, v).map(Some).ok_or_else(|| "no such group to go in (\"parent\")".into()),
    }
}

/// A layer that can join a group: not one of a service's collections (its service group keeps it).
fn groupable(input: &impl super::InputSource, id: u32) -> Result<u32, String> {
    let context = input.input_context();
    let s = &context;
    if in_service(s.map(), id) {
        let name = s.map().layer(id).map(|l| l.name.clone()).unwrap_or_default();
        return Err(format!("'{name}' belongs to a service group"));
    }
    Ok(id)
}

fn in_service(m: &Map, id: u32) -> bool {
    m.service_groups.iter().any(|g| g.children.iter().any(|c| c.layer == id))
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
    let parent = parent_of(s.map(), p)?;
    if parent.is_some_and(|g| s.map().group_depth(g) >= MAX_GROUP_DEPTH) {
        return Err(format!("groups nest at most {MAX_GROUP_DEPTH} levels deep"));
    }
    let mut children = added_layers(s, p)?;
    // In drawing order, so gathering keeps them as they were.
    let m = s.map_mut();
    children.sort_by_key(|id| m.layer_index(*id));
    let mut group = LayerGroup::new(name);
    group.children = children;
    group.parent = parent;
    let id = m.add_group(group);
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

/// A `before` that names a layer or a group, as the layer at the top of it.
fn before_of(input: &impl super::InputSource, v: &Value) -> Result<u32, String> {
    let context = input.input_context();
    let s = &context;
    let m = s.map();
    if let Ok(id) = s.layer_id(&json!({"layer": v})) {
        return Ok(id);
    }
    let g = group_ref(m, Some(v)).ok_or("no layer or group to go before")?;
    m.group_layers(g).first().copied().ok_or_else(|| "the group to go before has no layers".into())
}

fn set_parent(s: &mut Session, p: &Value) -> CmdResult {
    let id = group_id(s.map(), p)?;
    if p.get("parent").is_none() {
        return Err("give the group to go in (\"parent\"), or null for the top level".into());
    }
    let parent = parent_of(s.map(), p)?;
    match p.get("before") {
        None => s.map_mut().set_group_parent(id, parent)?,
        Some(Value::Null) => s.map_mut().place_group(id, parent, None)?,
        Some(v) => {
            let before = before_of(s, v)?;
            s.map_mut().place_group(id, parent, Some(before))?;
        }
    }
    Ok(json!({"group": id, "parent": parent}))
}

/// The group just above an item at its level of the outline, which indenting puts it in.
fn group_above(m: &Map, parent: Option<u32>, item: Outline) -> Result<u32, String> {
    let level = m.outline(parent);
    let at = level.iter().position(|x| *x == item).ok_or("it isn't listed in the Layers pane")?;
    match at.checked_sub(1).and_then(|k| level.get(k)) {
        None => Err("nothing above it to go into: it is first at its level".into()),
        Some(Outline::Group(g)) => Ok(*g),
        Some(Outline::Layer(l)) => {
            let name = m.layer(*l).map(|l| l.name.clone()).unwrap_or_default();
            if in_service(m, *l) {
                Err(format!("'{name}' above it is in a service group, which keeps its own layers"))
            } else {
                Err(format!("'{name}' above it is a layer, not a group"))
            }
        }
    }
}

/// The layer just below a group's layers (leaving `moving` out) if it is in `parent` (`None`:
/// any): what goes just below the group goes before it. `None`: the end of `parent`, or the
/// bottom.
fn below(m: &Map, group: u32, parent: Option<u32>, moving: &[u32]) -> Option<u32> {
    let members = m.group_layers(group);
    let last = m.layers.iter().rposition(|l| members.contains(&l.id))?;
    m.layers.iter().skip(last + 1).map(|l| l.id).find(|id| !moving.contains(id)).filter(|next| parent.is_none_or(|p| m.layer_in(*next, p)))
}

fn indent_layer(s: &mut Session, p: &Value) -> CmdResult {
    let id = groupable(s, s.layer_id(p)?)?;
    let m = s.map();
    let into = indent_destination(m, Outline::Layer(id))?;
    s.map_mut().place_layer(id, Some(into), None)?;
    Ok(json!({"layer": id, "group": into}))
}

fn outdent_layer(s: &mut Session, p: &Value) -> CmdResult {
    let id = s.layer_id(p)?;
    let m = s.map();
    let (parent, before) = outdent_layer_destination(m, id)?;
    s.map_mut().place_layer(id, parent, before)?;
    Ok(json!({"layer": id, "group": parent}))
}

fn indent_group(s: &mut Session, p: &Value) -> CmdResult {
    let id = group_id(s.map(), p)?;
    let m = s.map();
    let into = indent_destination(m, Outline::Group(id))?;
    s.map_mut().place_group(id, Some(into), None)?;
    Ok(json!({"group": id, "parent": into}))
}

fn outdent_group(s: &mut Session, p: &Value) -> CmdResult {
    let id = group_id(s.map(), p)?;
    let m = s.map();
    let (up, before) = outdent_group_destination(m, id)?;
    s.map_mut().place_group(id, up, before)?;
    Ok(json!({"group": id, "parent": up}))
}

fn ungroup(s: &mut Session, p: &Value) -> CmdResult {
    let id = group_id(s.map(), p)?;
    s.map_mut().ungroup(id)?;
    Ok(Value::Null)
}

fn remove(s: &mut Session, p: &Value) -> CmdResult {
    let id = group_id(s.map(), p)?;
    let layers = s.map().group_layers(id);
    let mut gone = s.map().subgroups(id);
    gone.push(id);
    crate::cmd::layer::drop_layers(s, &layers);
    s.map_mut().groups.retain(|g| !gone.contains(&g.id));
    if s.active_layer.is_some_and(|a| layers.contains(&a)) {
        s.active_layer = s.map().layers.first().map(|l| l.id);
    }
    Ok(Value::Null)
}

fn move_to_group(s: &mut Session, p: &Value) -> CmdResult {
    let group = destination_group(s.map(), p)?;
    // Several (a list, or the layer selection): each in or out of the group, one undo step.
    let ids = super::layer::layer_ids(s, p)?;
    if ids.len() > 1 {
        if p.get("before").is_some() {
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
        Some(Value::Null) => s.map_mut().place_layer(layer, group, None)?,
        Some(v) => {
            let before = s.layer_id(&json!({"layer": v}))?;
            s.map_mut().place_layer(layer, group, Some(before))?;
        }
        None => s.map_mut().set_group(layer, group),
    }
    Ok(json!({"layer": layer, "group": group}))
}

/// Resolves and deduplicates groupable layer names/ids in the actual insertion order.
fn added_layers(input: &impl super::InputSource, p: &Value) -> Result<Vec<u32>, String> {
    let context = input.input_context();
    let s = &context;
    let mut children = Vec::new();
    if let Some(layers) = p.get("layers") {
        let layers = layers.as_array().ok_or("layers must be a list of layer names or ids")?;
        for v in layers {
            let id = groupable(input, s.layer_id(&json!({"layer": v}))?)?;
            if !children.contains(&id) {
                children.push(id);
            }
        }
    }
    Ok(children)
}

/// Binds the chosen group's expanded layer subtree, including an empty group's identity.
fn group_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let s = input;
    let id = group_id(s.map(), p)?;
    let mut inputs = super::Inputs::layers(s.map().group_layers(id).iter().filter_map(|id| s.map().layer(*id)).map(|l| (s.map(), l)));
    inputs.state = json!({"group":id});
    Ok(inputs)
}
/// Uses the actual insertion selector and parent resolver without editing group membership.
fn add_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let s = input;
    let ids = added_layers(input, p)?;
    let mut inputs = super::Inputs::layers(ids.iter().filter_map(|id| s.map().layer(*id)).map(|l| (s.map(), l)));
    inputs.state = json!({"parent":parent_of(s.map(),p)?});
    Ok(inputs)
}
/// Binds moved members, parent and the layer-first before selector.
fn parent_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let s = input;
    let mut inputs = group_inputs(input, p)?;
    let before = p.get("before").filter(|v| !v.is_null()).map(|v| before_of(input, v)).transpose()?;
    if let Some(id) = before {
        inputs.layers.push((s.map().instance_id(), id));
    }
    inputs.state["parent"] = json!(parent_of(s.map(), p)?);
    Ok(inputs)
}
/// Uses the actual selected-layer list and layer-only before semantics.
fn layer_group_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let s = input;
    let ids = super::layer::layer_ids(input, p)?;
    let mut inputs = super::Inputs::layers(ids.iter().filter_map(|id| s.map().layer(*id)).map(|l| (s.map(), l)));
    let group = destination_group(s.map(), p)?;
    let before = p.get("before").filter(|v| !v.is_null()).map(|v| s.layer_id(&json!({"layer":v}))).transpose()?;
    if let Some(id) = before {
        inputs.layers.push((s.map().instance_id(), id));
    }
    inputs.state = json!({"group":group});
    Ok(inputs)
}

/// Distinguishes missing group from explicit null, which removes a layer from its group.
fn destination_group(map: &Map, p: &Value) -> Result<Option<u32>, String> {
    match p.get("group") {
        None => Err("give the group's name or id (\"group\"), or null to take the layer out of its group".into()),
        Some(Value::Null) => Ok(None),
        Some(_) => group_id(map, p).map(Some),
    }
}
/// Finds the existing group immediately above an outline item at its current level.
fn indent_destination(map: &Map, item: Outline) -> Result<u32, String> {
    let parent = match item {
        Outline::Layer(id) => map.group_of(id).map(|g| g.id),
        Outline::Group(id) => map.group(id).and_then(|g| g.parent),
    };
    group_above(map, parent, item)
}
/// Computes the parent and following layer used by the real layer outdent.
fn outdent_layer_destination(map: &Map, id: u32) -> Result<(Option<u32>, Option<u32>), String> {
    let group = map.group_of(id).ok_or("the layer isn't in a group")?;
    Ok((group.parent, below(map, group.id, group.parent, &[id])))
}
/// Computes the parent and following layer used by the real group outdent.
fn outdent_group_destination(map: &Map, id: u32) -> Result<(Option<u32>, Option<u32>), String> {
    let parent = map.group(id).and_then(|g| g.parent).ok_or("the group isn't in another group")?;
    let up = map.group(parent).and_then(|g| g.parent);
    Ok((up, below(map, parent, up, &map.group_layers(id))))
}
/// Binds the moving subtree and the actual group selected by indentation.
fn indent_group_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let s = input;
    let mut inputs = group_inputs(input, p)?;
    inputs.state["into"] = json!(indent_destination(s.map(), Outline::Group(group_id(s.map(), p)?))?);
    Ok(inputs)
}
/// Binds the moving subtree and the actual outdent placement.
fn outdent_group_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let s = input;
    let mut inputs = group_inputs(input, p)?;
    inputs.state["destination"] = json!(outdent_group_destination(s.map(), group_id(s.map(), p)?)?);
    Ok(inputs)
}
/// Binds the moving layer and its current outline destination.
fn indent_layer_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let s = input;
    let mut inputs = super::Inputs::primary(s, p)?;
    inputs.state = json!({"into":indent_destination(s.map(),Outline::Layer(s.layer_id(p)?))?});
    Ok(inputs)
}
/// Binds the moving layer and its current outdent placement.
fn outdent_layer_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let s = input;
    let mut inputs = super::Inputs::primary(s, p)?;
    inputs.state = json!({"destination":outdent_layer_destination(s.map(),s.layer_id(p)?)?});
    Ok(inputs)
}
