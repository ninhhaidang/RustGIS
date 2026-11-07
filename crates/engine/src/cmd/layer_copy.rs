//! Layers › Copy and Paste (#491): whole layers, with their groups, from one map to another (or
//! the same one, as a duplicate).
//!
//! A copy keeps everything that belongs to the layer (source, symbology, labels, transparency,
//! blend, scale range, definition query, display filters, mask) but not its selection. The
//! features aren't copied in memory until one side edits them (they are shared copy-on-write).

use std::collections::HashMap;

use rustgis_data::GeometryType;
use rustgis_map::{Layer, LayerGroup, MAX_GROUP_DEPTH, MAX_GROUPS, Map};
use serde_json::{Value, json};

use super::{copy_name, uint};
use crate::{CmdResult, CommandSpec as C, Session};

/// Most layer names or ids one copy takes (parameters are hostile).
const MAX_COPIED: usize = 10_000;

/// Layers on the clipboard: copies in drawing order (top first), with the groups that were copied
/// whole, their subgroups included (their children are ids of `layers`; a subgroup's `parent` is
/// one of `groups`, a copied group's own is left as it was).
#[derive(Clone, Debug)]
pub struct LayerClip {
    pub layers: Vec<Layer>,
    pub groups: Vec<LayerGroup>,
}

pub fn commands() -> Vec<C> {
    vec![
        C::new("layer.copy", "Copy Layers", "Layers", copy).inputs(copy_inputs).roles(&[]).effects(crate::policy::Effects::NONE).view().params(
            r#"{"layer"?: name|id | [name|id, …] (default: the active layer, unless a group is given), "group"?: name|id | [name|id, …] (the group with all its layers and subgroups)}"#,
        ),
        C::new("layer.paste", "Paste Layers", "Layers", paste).inputs(paste_inputs).roles(&[]).effects(crate::policy::Effects::NONE).when(need_copied).params(
            r#"{"map"?: name (default: the map in view), "group"?: name|id (into that group, at its top; copied groups go in it too), "to"?: position (0 = top, the default; never inside a group: above it)}"#,
        ),
    ]
}

fn need_copied(s: &Session) -> Option<&'static str> {
    if s.layer_clipboard.is_some() { None } else { Some("copy a layer first (Layers › Copy)") }
}

/// One name or id, or a list of them.
fn list(v: &Value) -> Result<Vec<&Value>, String> {
    match v {
        Value::Array(a) if a.len() > MAX_COPIED => Err(format!("at most {MAX_COPIED} at once")),
        Value::Array(a) => Ok(a.iter().collect()),
        other => Ok(vec![other]),
    }
}

fn copy(s: &mut Session, p: &Value) -> CmdResult {
    let (ids, groups) = copy_selection(s, p)?;
    let m = s.map();
    let layers: Vec<Layer> = ids
        .iter()
        .filter_map(|id| m.layer(*id))
        .map(|l| {
            let mut l = l.clone();
            l.selection.clear();
            l
        })
        .collect();
    // Nothing named (an empty list) leaves the clipboard as it was.
    if layers.is_empty() && groups.is_empty() {
        return Err("nothing to copy".into());
    }
    let names: Vec<String> = layers.iter().map(|l| l.name.clone()).collect();
    s.layer_clipboard = Some(LayerClip { layers, groups });
    let n = names.len();
    s.notify(if n == 1 { format!("Copied {}", names.join("")) } else { format!("Copied {n} layers") }, false);
    Ok(json!({"copied": n, "layers": names}))
}

/// A name no layer of the map has (case aside): the name itself, else `<name> copy`, numbered.
fn unique_layer_name(m: &Map, name: &str) -> String {
    let taken = |n: &str| m.layers.iter().any(|l| l.name.eq_ignore_ascii_case(n));
    if taken(name) { copy_name(None, name, taken) } else { name.to_string() }
}

fn unique_group_name(m: &Map, name: &str) -> String {
    let taken = |n: &str| m.groups.iter().any(|g| g.name.eq_ignore_ascii_case(n));
    if taken(name) { copy_name(None, name, taken) } else { name.to_string() }
}

/// What a paste added, as its notice says it: "2 layers", "1 group and 3 layers", "1 group" (an
/// empty group copied).
fn pasted_what(layers: usize, groups: usize) -> String {
    let count = |n: usize, one: &str, many: &str| if n == 1 { format!("1 {one}") } else { format!("{n} {many}") };
    match (groups, layers) {
        (0, n) => count(n, "layer", "layers"),
        (g, 0) => count(g, "group", "groups"),
        (g, n) => format!("{} and {}", count(g, "group", "groups"), count(n, "layer", "layers")),
    }
}

fn paste(s: &mut Session, p: &Value) -> CmdResult {
    let clip = s.layer_clipboard.clone().ok_or("no layers have been copied")?;
    let mi = super::navigate::map_at(s, p)?;
    let m = s.project.maps.get(mi).ok_or("no such map")?;
    let into = match p.get("group") {
        None | Some(Value::Null) => None,
        Some(_) => Some(super::group::group_id(m, p)?),
    };
    let to = match p.get("to") {
        None | Some(Value::Null) => None,
        Some(_) => Some(uint(p, "to").ok_or("to must be a position: 0 at the top")?),
    };
    if into.is_some() && to.is_some() {
        return Err("give a group or a position, not both".into());
    }
    if into.is_some() && clip.layers.is_empty() && clip.groups.is_empty() {
        return Err("nothing copied to paste into a group".into());
    }
    let groups = clip.groups;
    if m.groups.len().saturating_add(groups.len()) > MAX_GROUPS {
        return Err(format!("a map holds at most {MAX_GROUPS} groups"));
    }
    // Each copied group's level among the copied ones (1: copied without the group holding it).
    let copied = |id: u32| groups.iter().any(|g| g.id == id);
    let depths: HashMap<u32, usize> = groups
        .iter()
        .map(|g| {
            let mut depth = 1;
            let mut up = g.parent.filter(|p| copied(*p));
            while let Some(p) = up.filter(|_| depth <= MAX_GROUP_DEPTH) {
                depth += 1;
                up = groups.iter().find(|x| x.id == p).and_then(|x| x.parent).filter(|p| copied(*p));
            }
            (g.id, depth)
        })
        .collect();
    let deepest = depths.values().copied().max().unwrap_or(0);
    if into.is_some_and(|g| m.group_depth(g).saturating_add(deepest) > MAX_GROUP_DEPTH) {
        return Err(format!("groups nest at most {MAX_GROUP_DEPTH} levels deep"));
    }
    // Where the copies go: above this layer (`None`: the bottom, as on an empty map).
    let before = match into {
        Some(g) => m.group_layers(g).first().copied(),
        None => m.layers.get(usize::try_from(to.unwrap_or(0)).unwrap_or(usize::MAX)).map(|l| l.id),
    };
    let m = s.project.maps.get_mut(mi).ok_or("no such map")?;
    let mut new_ids: HashMap<u32, u32> = HashMap::new();
    // Each pasted layer's name as copied, in drawing order, to find masks by.
    let mut copied: Vec<(String, u32)> = Vec::new();
    let mut pasted: Vec<u32> = Vec::new();
    for mut l in clip.layers {
        let old = l.id;
        let name = unique_layer_name(m, &l.name);
        let was = std::mem::replace(&mut l.name, name.clone());
        l.dataset.name = name.clone();
        let id = m.add_layer(l);
        m.place_layer(id, into, before)?;
        new_ids.insert(old, id);
        copied.push((was, id));
        pasted.push(id);
    }
    // A mask follows its layer when that was pasted too; one the map lacks is cleared. Masks are
    // found as the renderer finds them: the first polygon layer of that name, case aside (ASCII).
    let mut unmasked: Vec<String> = Vec::new();
    for id in &pasted {
        let Some(mask) = m.layer(*id).and_then(|l| l.mask.clone()) else { continue };
        let found = mask_target(&mask, copied.iter().filter_map(|(name, id)| m.layer(*id).map(|l| (name.as_str(), l))), m).map(|l| l.id);
        let target = found.and_then(|c| m.layer(c)).map(|l| l.name.clone());
        if let Some(l) = m.layer_mut(*id) {
            if target.is_none() {
                unmasked.push(l.name.clone());
            }
            l.mask = target;
        }
    }
    let mut group_ids = Vec::new();
    // Outermost first, so each subgroup's parent is pasted before it.
    let mut groups = groups;
    groups.sort_by_key(|g| depths.get(&g.id).copied().unwrap_or(0));
    let mut new_groups: HashMap<u32, u32> = HashMap::new();
    for mut g in groups {
        let old = g.id;
        g.children = g.children.iter().filter_map(|c| new_ids.get(c).copied()).collect();
        g.name = unique_group_name(m, &g.name);
        g.parent = match g.parent.and_then(|p| new_groups.get(&p)) {
            Some(p) => Some(*p),
            None => into,
        };
        let gid = m.add_group(g);
        new_groups.insert(old, gid);
        group_ids.push(gid);
    }
    let map = m.name.clone();
    if mi == s.project.active_map
        && let Some(first) = pasted.first()
    {
        s.active_layer = Some(*first);
    }
    // Said, as a paste from the Browser can go to a map out of view.
    s.notify(format!("Pasted {} into {map}", pasted_what(pasted.len(), group_ids.len())), false);
    match unmasked.as_slice() {
        [] => {}
        [one] => s.notify(format!("{one} lost its mask: {map} has no polygon layer by that name"), false),
        many => s.notify(format!("{} lost their masks: {map} has no polygon layers by those names", many.join(", ")), false),
    }
    Ok(json!({"map": map, "layers": pasted, "groups": group_ids, "unmasked": unmasked}))
}

/// Resolves explicit layer lists and expanded groups with copy's precise active-layer fallback.
fn copy_selection(input: &impl super::InputSource, p: &Value) -> Result<(Vec<u32>, Vec<LayerGroup>), String> {
    let context = input.input_context();
    let s = &context;
    let mut ids: Vec<u32> = Vec::new();
    let mut groups: Vec<LayerGroup> = Vec::new();
    if let Some(v) = p.get("group").filter(|v| !v.is_null()) {
        for g in list(v)? {
            let id = super::group::group_id(s.map(), &json!({"group": g}))?;
            let m = s.map();
            // The group and its subgroups, outermost first, each once.
            let mut tree = vec![id];
            tree.extend(m.subgroups(id));
            tree.sort_by_key(|g| m.group_depth(*g));
            for gid in tree {
                let Some(group) = m.group(gid) else { continue };
                if groups.iter().all(|x| x.id != gid) {
                    ids.extend(&group.children);
                    groups.push(group.clone());
                }
            }
        }
    }
    match p.get("layer") {
        Some(v @ Value::Array(_)) => {
            for v in list(v)? {
                ids.push(s.layer_id(&json!({"layer": v}))?);
            }
        }
        Some(v) if !v.is_null() => ids.push(s.layer_id(p)?),
        // The active layer by default, unless only groups are copied.
        _ if groups.is_empty() => ids.push(s.layer_id(p)?),
        _ => {}
    }
    let m = s.map();
    ids.sort_by_key(|id| m.layer_index(*id));
    ids.dedup();
    Ok((ids, groups))
}

/// Binds the copied layer subtree and copied group membership without changing the clipboard.
fn copy_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let s = input;
    let (ids, groups) = copy_selection(input, p)?;
    let mut inputs = super::Inputs::layers(ids.iter().filter_map(|id| s.map().layer(*id)).map(|l| (s.map(), l)));
    inputs.state = json!(groups.iter().map(|g| (g.id, g.parent, &g.children)).collect::<Vec<_>>());
    Ok(inputs)
}
/// Binds detached clipboard incarnations, destination and existing mask dependencies without rebinding ids.
fn paste_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let s = input;
    let clip = s.layer_clipboard.as_ref().ok_or("no layers have been copied")?;
    let map = s.project.maps.get(super::navigate::map_at(input, p)?).ok_or("no map")?;
    let group = p.get("group").filter(|v| !v.is_null()).map(|_| super::group::group_id(map, p)).transpose()?;
    let masks = clip.layers.iter().filter_map(|l| l.mask.as_ref()).filter_map(|name| {
        let found = mask_target(name, clip.layers.iter().map(|l| (l.name.as_str(), l)), map)?;
        map.layers.iter().find(|l| std::ptr::eq(*l, found))
    });
    let mut inputs = super::Inputs::layers(masks.map(|l| (map, l)));
    inputs.maps.push(map.instance_id());
    inputs.state = json!({"group":group,"clipboard":clip.layers.iter().map(|l|(l.instance_id(),&l.name,&l.mask,crate::policy::input_source(l))).collect::<Vec<_>>(),"groups":clip.groups.iter().map(|g|(g.id,g.parent,&g.children)).collect::<Vec<_>>()});
    Ok(inputs)
}

/// Copied polygons win over the destination's first matching polygon, exactly as paste remaps masks.
fn mask_target<'a>(name: &str, copied: impl IntoIterator<Item = (&'a str, &'a Layer)>, destination: &'a Map) -> Option<&'a Layer> {
    copied
        .into_iter()
        .find(|(was, l)| was.eq_ignore_ascii_case(name) && l.geometry_type() == GeometryType::Polygon)
        .map(|(_, l)| l)
        .or_else(|| destination.layers.iter().find(|l| l.name.eq_ignore_ascii_case(name) && l.geometry_type() == GeometryType::Polygon))
}
