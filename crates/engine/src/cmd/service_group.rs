//! Atomic import and refresh of a service's feature collections. Both native callers and the
//! asynchronous GUI use the same discovery, response validation and layer update code.

use rustgis_data::{Dataset, ogc::Offer};
use rustgis_map::{ServiceChild, ServiceGroup};
use serde_json::{Value, json};

use super::{C, CmdResult, MAX_GROUP_BYTES, Session, capabilities, ps, service_json};

pub fn commands() -> Vec<C> {
    vec![
        C::new("service.import", "Import Service", "Home › Add", import).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network)).params(
            r#"{"url": "an OGC API landing page or collections address", "type"?, "name"?: "group name (default My OGC Layer)", "text"?, "documents"?: "as service.layers", "responses"?: [{"url": "features URL", "text": "downloaded GeoJSON"}], "connection"?: "as service.add", "anonymous"?: bool (internal: preserve plugin-selected service provenance)} ("group" and "layer" are rejected: use service.refresh)"#,
        ),
        C::new("service.refresh", "Refresh", "Layers", refresh).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network)).params(
            r#"{"group"?: id, "layer"?: "one child layer id or name", "text"?, "documents"?, "responses"?: "as service.import", "connection"?: "as service.add (default: the one its layers record)", "anonymous"?: bool (internal: do not borrow a saved login)}"#,
        ),
        C::new("service.groupPlan", "Service Import Requests", "Layers", plan).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network)).view().params(
            r#"{"url"?, "type"?, "name"?, "group"?: id, "layer"?: "one child layer", "sourceOnly"?: "return the connection without fetching", "text"?, "documents"?, "connection"?: "as service.add", "anonymous"?: bool (internal: preserve plugin-selected service provenance)} → request, layers, and next while discovery is incomplete"#,
        ),
        C::new("service.group", "Service Group", "Layers", set_group).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r#"{"group"?: id, "layer"?: "a child identifying its group", "name"?: text, "expanded"?: bool, "visible"?: "set all children", "remove"?: "remove the group and its children"}"#,
        ),
    ]
}

fn target(s: &Session, p: &Value) -> Result<Option<(ServiceGroup, Option<u32>)>, String> {
    let layer = p.get("layer").map(|_| s.layer_id(p)).transpose()?;
    if p.get("group").is_none() && layer.is_none() {
        return Ok(None);
    }
    let id = p.get("group").map(|v| v.as_u64().and_then(|n| u32::try_from(n).ok()).ok_or("invalid group id")).transpose()?;
    let group = s
        .map()
        .service_groups
        .iter()
        .find(|g| id.is_none_or(|id| g.id == id) && layer.is_none_or(|id| g.children.iter().any(|c| c.layer == id)))
        .ok_or("no such service group or child")?;
    Ok(Some((group.clone(), layer)))
}

fn request(s: &mut Session, p: &Value) -> CmdResult {
    let mut q = p.clone();
    let mut anonymous = s.web_logins.is_anonymous() || p.get("anonymous").and_then(Value::as_bool) == Some(true);
    if let Some((group, child)) = target(s, p)? {
        anonymous |= group.anonymous || child.and_then(|id| s.map().layer(id)).is_some_and(|l| l.anonymous);
        // A plugin-selected source never borrows a login, including a later user refresh.
        if !anonymous && q.get("connection").is_none_or(Value::is_null) {
            let mut ids = group
                .children
                .iter()
                .filter(|c| child.is_none_or(|id| c.layer == id))
                .filter_map(|c| s.map().layer(c.layer))
                .map(|l| l.connection.as_str());
            if let Some(id) = ids.next().filter(|id| !id.is_empty())
                && ids.all(|other| other == id)
                && valid_connection(s, id, &group.url)
            {
                q["connection"] = json!(id);
            }
        }
        q["url"] = json!(crate::render::rustgis_tiles::logins::inner(&group.url));
        q["type"] = json!(group.kind);
        q["name"] = json!(group.name);
    }
    if anonymous {
        q["anonymous"] = json!(true);
        if let Some(q) = q.as_object_mut() {
            q.remove("connection");
        }
    }
    super::url_of(&q)?;
    Ok(crate::service_logins::request_params(s, &q)?.into_owned())
}

fn valid_connection(s: &Session, id: &str, url: &str) -> bool {
    let origin = crate::render::rustgis_tiles::logins::origin(url);
    origin.is_some()
        && s.connections.connections.iter().any(|c| {
            c.id == id && crate::service_logins::is_service(c) && crate::render::rustgis_tiles::logins::origin(&c.address) == origin
        })
}

fn record_connection(s: &mut Session, layer: u32, request: &Value, anonymous: bool) {
    if anonymous || request.get("anonymous").and_then(Value::as_bool) == Some(true) {
        if let Some(layer) = s.map_mut().layer_mut(layer) {
            layer.anonymous = true;
            layer.connection.clear();
        }
        return;
    }
    let Some(id) = ps(request, "connection") else { return };
    let Some(url) = s.map().layer(layer).and_then(|l| l.dataset.source.as_deref()) else { return };
    if valid_connection(s, id, url)
        && let Some(layer) = s.map_mut().layer_mut(layer)
    {
        layer.connection = id.to_string();
    }
}

fn plan(s: &mut Session, p: &Value) -> CmdResult {
    if p.get("sourceOnly").and_then(Value::as_bool) == Some(true) {
        return request(s, p);
    }
    listing(s, p)
}

fn listing(s: &mut Session, p: &Value) -> CmdResult {
    let q = request(s, p)?;
    let mut found = capabilities(s, &q)?;
    super::notify_withheld(s, found.service.withheld_addresses);
    if let Some(error) = found.error {
        return Err(error);
    }
    if found.excluded_layers {
        return Err("the service contains disallowed addresses; the group was not changed".into());
    }
    if !found.service.kind.starts_with("OGC API") {
        return Err("group import requires an OGC API feature service".into());
    }
    if found.incomplete {
        return Err(
            "the service lists collections RustGIS cannot read (more than 2000, an address too long to read them all, or unreadable addresses); the group was not changed".into(),
        );
    }
    let previous = target(s, p)?;
    let child = previous.as_ref().and_then(|(g, id)| id.and_then(|id| g.children.iter().find(|c| c.layer == id).cloned()));
    let anonymous: std::collections::HashSet<&str> = previous
        .as_ref()
        .into_iter()
        .flat_map(|(g, _)| &g.children)
        .filter(|c| s.map().layer(c.layer).is_some_and(|l| l.anonymous))
        .map(|c| c.collection.as_str())
        .collect();
    let mut identities = std::collections::HashSet::new();
    let mut offered = Vec::new();
    for (layer, source) in found.service.layers.into_iter().zip(found.sources) {
        let mut source = source;
        if !matches!(layer.offer, Offer::Features { .. }) {
            continue;
        }
        source.crossed_origin |= anonymous.contains(layer.name.as_str());
        if !identities.insert((source.address.clone(), layer.name.clone())) {
            return Err("the service advertises duplicate collection identifiers".into());
        }
        offered.push((layer, source));
    }
    if offered.len() > 2000 {
        return Err("a service group supports at most 2000 collections".into());
    }
    // One child: its collection at its saved source, or else the one collection with its id (a
    // rotated query or a moved collections document keeps the child's identity).
    if let Some(c) = &child {
        let exact = offered.iter().position(|(l, src)| l.name == c.collection && super::same_address(&src.address, &c.source_url));
        let pos = match exact {
            Some(i) => Some(i),
            None => {
                let mut same = offered.iter().enumerate().filter(|(_, (l, _))| l.name == c.collection).map(|(i, _)| i);
                let first = same.next();
                if first.is_some() && same.next().is_some() {
                    return Err("the service advertises the collection identifier more than once".into());
                }
                first
            }
        };
        offered = pos.and_then(|i| offered.into_iter().nth(i)).into_iter().collect();
    }
    let (selected, sources): (Vec<_>, Vec<_>) = offered.into_iter().unzip();
    if found.next.is_none() && child.is_some() && selected.is_empty() {
        return Err("the collection is no longer advertised by this service".into());
    }
    found.service.layers = selected;
    let mut out = service_json(&found.service);
    if let Some(layers) = out.get_mut("layers").and_then(Value::as_array_mut) {
        for (layer, source) in layers.iter_mut().zip(sources) {
            super::layer_source(layer, source);
        }
    }
    if let Some(next) = found.next {
        out["next"] = json!(next);
    }
    out["request"] = json!({"url": q.get("url"), "type": ps(&q, "type").unwrap_or("ogcapi")});
    if q.get("anonymous").and_then(Value::as_bool) == Some(true) {
        out["request"]["anonymous"] = json!(true);
    } else if let Some(connection) = q.get("connection").filter(|v| !v.is_null()) {
        out["request"]["connection"] = connection.clone();
    }
    Ok(out)
}

#[cfg(not(target_arch = "wasm32"))]
fn download(s: &mut Session, name: &str, url: &str, max: usize) -> Result<Vec<u8>, String> {
    super::download_features(s, name, url, max)
}

#[cfg(target_arch = "wasm32")]
fn download(_: &mut Session, _: &str, _: &str, _: usize) -> Result<Vec<u8>, String> {
    Err("the browser must supply downloaded feature responses".into())
}

fn read(s: &mut Session, p: &Value) -> Result<(Value, Vec<(ServiceChild, Dataset, bool)>), String> {
    let listing = listing(s, p)?;
    if listing.get("next").is_some() {
        return Err("complete service discovery before importing or refreshing".into());
    }
    let layers = listing.get("layers").and_then(Value::as_array).ok_or("missing collections")?;
    let responses = p.get("responses").map(|v| v.as_array().ok_or("responses must be an array")).transpose()?;
    if responses.is_some_and(|r| r.len() > 2000) {
        return Err("at most 2000 feature responses may be supplied".into());
    }
    let mut bytes_read = 0usize;
    let mut reads = Vec::new();
    for layer in layers {
        let name = ps(layer, "title").unwrap_or("Collection");
        let url = layer.get("offer").and_then(|o| ps(o, "url")).ok_or("missing features address")?;
        let bytes = match responses {
            Some(responses) => {
                let text = responses
                    .iter()
                    .find(|r| ps(r, "url").is_some_and(|u| super::same_address(u, url)))
                    .and_then(|r| ps(r, "text"))
                    .ok_or_else(|| format!("{name}: missing downloaded response"))?;
                if text.len() > MAX_GROUP_BYTES.saturating_sub(bytes_read) {
                    return Err("service group data exceeds 64 MB".into());
                }
                text.as_bytes().to_vec()
            }
            None => download(s, name, url, MAX_GROUP_BYTES.saturating_sub(bytes_read))?,
        };
        bytes_read = bytes_read.saturating_add(bytes.len());
        if bytes_read > MAX_GROUP_BYTES {
            return Err("service group data exceeds 64 MB".into());
        }
        // The collection's bounds, from the listing: none read leaves the axes as they come.
        let bounds = super::bounds_of(layer.get("bbox")).ok().flatten();
        let bytes = super::features_answer(s, name, bytes, bounds);
        let text = std::str::from_utf8(&bytes).map_err(|e| format!("{name}: {e}"))?;
        let (mut dataset, report) = rustgis_data::io::read_geojson(text, name).map_err(|e| format!("{name}: {e}"))?;
        if dataset.features.is_empty() && !feature_collection(text) {
            return Err(format!("{name}: the service's answer is not a GeoJSON FeatureCollection with a features array"));
        }
        // Empty collections remain visible as children and can acquire geometry on refresh.
        dataset.source = Some(url.to_string());
        for message in report.messages {
            s.notify(message, false);
        }
        reads.push((
            ServiceChild {
                layer: 0,
                collection: ps(layer, "name").unwrap_or_default().to_string(),
                source_url: ps(layer, "source_url").unwrap_or_default().to_string(),
            },
            dataset,
            layer.get("anonymous").and_then(Value::as_bool) == Some(true),
        ));
    }
    Ok((listing, reads))
}

/// Whether an answer with no features is a FeatureCollection whose `features` is an array (an
/// explicit `[]`), not a malformed envelope that would erase a refreshed child's features.
fn feature_collection(text: &str) -> bool {
    serde_json::from_str::<Value>(text)
        .is_ok_and(|v| v.get("type").and_then(Value::as_str) == Some("FeatureCollection") && v.get("features").is_some_and(Value::is_array))
}

/// Imports every feature collection a service advertises as a group of layers, framed on a map
/// that shows nothing yet.
fn import(s: &mut Session, p: &Value) -> CmdResult {
    if p.get("group").is_some() || p.get("layer").is_some() {
        return Err("use service.refresh for an existing group or child".into());
    }
    // `super` is `service`, which lacks the helper, so the path starts at the crate root.
    let empty_before = crate::cmd::map_is_empty(s);
    let (listing, reads) = read(s, p)?;
    if reads.is_empty() {
        return Err("the service has no feature collections".into());
    }
    let mut children = Vec::new();
    let mut extent = rustgis_geom::Extent::EMPTY;
    for (mut child, dataset, anonymous) in reads {
        child.layer = crate::cmd::layer::add_dataset(s, dataset);
        if let Some(request) = listing.get("request") {
            record_connection(s, child.layer, request, anonymous);
        }
        if let Some(layer) = s.map().layer(child.layer) {
            extent = extent.union(&layer.extent());
        }
        children.push(child);
    }
    let name = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).unwrap_or("My OGC Layer").chars().take(200).collect();
    let req = listing.get("request").ok_or("missing service request")?;
    let group = ServiceGroup {
        id: 0,
        name,
        url: ps(req, "url").unwrap_or_default().into(),
        kind: ps(req, "type").unwrap_or("ogcapi").into(),
        anonymous: req.get("anonymous").and_then(Value::as_bool) == Some(true),
        expanded: true,
        children,
    };
    let ids: Vec<u32> = group.children.iter().map(|c| c.layer).collect();
    let id = s.map_mut().add_service_group(group);
    // As adding a service layer: the first group on an empty map is framed, a later one leaves
    // the view where it is.
    if empty_before && !extent.is_empty() {
        s.zoom_to(&extent);
    }
    Ok(json!({"group":id,"layers":ids}))
}

fn refresh(s: &mut Session, p: &Value) -> CmdResult {
    let (old, child_id) = target(s, p)?.ok_or("group or layer is required")?;
    let hidden = old.children.iter().any(|c| s.map().layer(c.layer).is_some())
        && old.children.iter().filter_map(|c| s.map().layer(c.layer)).all(|l| !l.visible);
    let (listing, reads) = read(s, p)?;
    if !s.map().service_groups.iter().any(|g| g.id == old.id) {
        return Err("the group was removed".into());
    }
    let read_count = |name: &str| reads.iter().filter(|(c, _, _)| c.collection == name).count();
    let old_count = |name: &str| old.children.iter().filter(|c| c.collection == name).count();
    // A child refresh updates that child. A group refresh keeps a child whose collection is at the
    // same source, or else whose collection id is unique on both sides (the source address may
    // carry a rotated token or have moved).
    let matches: Vec<Option<u32>> = match child_id {
        Some(id) => reads.iter().map(|_| Some(id)).collect(),
        None => reads
            .iter()
            .map(|(child, _, _)| {
                old.children
                    .iter()
                    .find(|c| c.collection == child.collection && super::same_address(&c.source_url, &child.source_url))
                    .or_else(|| {
                        let unique = read_count(&child.collection) == 1 && old_count(&child.collection) == 1;
                        old.children.iter().find(|c| unique && c.collection == child.collection)
                    })
                    .map(|c| c.layer)
            })
            .collect(),
    };
    let mut children = Vec::new();
    // Label expressions a refresh left unreadable, said once each layer is read; and the layers
    // whose classes no longer read, drawn with a single symbol.
    let mut notes = Vec::new();
    let mut singled = Vec::new();
    let mut area_notes = Vec::new();
    let mut moved = 0;
    for ((mut child, mut dataset, anonymous), existing) in reads.into_iter().zip(matches) {
        let existing = existing.and_then(|id| s.map_mut().layer_mut(id));
        if let Some(layer) = existing {
            if dataset.geometry_type == rustgis_data::GeometryType::None {
                dataset.geometry_type = layer.geometry_type();
            }
            let changed_type = layer.geometry_type() != dataset.geometry_type;
            dataset.name = layer.name.clone();
            layer.dataset = dataset;
            layer.selection.clear();
            moved += layer.labels.in_order().map(|class| class.moved.len()).sum::<usize>();
            layer.labels.default.moved.clear();
            layer.labels.classes.iter_mut().for_each(|class| class.moved.clear());
            let invalid_renderer = match &layer.renderer {
                rustgis_map::Renderer::Single(_) => false,
                rustgis_map::Renderer::Unique { expression: Some(e), .. } => rustgis_data::query::parse_fields(e, &layer.dataset).is_err(),
                rustgis_map::Renderer::Unique { field, .. } => layer.dataset.field_index(field).is_none(),
                rustgis_map::Renderer::Graduated { field, .. } | rustgis_map::Renderer::Proportional { field, .. } => {
                    layer.dataset.field_index(field).and_then(|i| layer.dataset.fields.get(i)).is_none_or(|f| !f.ty.is_numeric())
                }
            };
            if changed_type || invalid_renderer {
                layer.renderer = rustgis_map::Renderer::Single(rustgis_map::Symbol::default_for(layer.geometry_type(), layer.id));
                if invalid_renderer {
                    singled.push(layer.name.clone());
                }
            }
            // (Labels by a text expression that still reads need no field.)
            let reads = |t: &Option<String>, ds: &rustgis_data::Dataset| {
                t.as_deref().is_some_and(|t| rustgis_data::query::parse_fields(t, ds).is_ok())
            };
            let by_text = |c: &rustgis_map::LabelClass, ds: &rustgis_data::Dataset| reads(&c.by.text, ds);
            if layer.dataset.field_index(&layer.labels.default.field).is_none() {
                // A field the service dropped isn't kept, even under a text expression (a web style
                // would label by it).
                layer.labels.default.field.clear();
                // Off, unless a shown class the map draws still labels as it stands: Default by its
                // text, another on its own field or by its text. (Classes repointed below onto a
                // fallback field don't count: with Default's field gone, labels go off as they
                // always did.)
                let ds = &layer.dataset;
                if !layer.labels.in_order().any(|c| c.visible && (ds.field_index(&c.field).is_some() || by_text(c, ds))) {
                    layer.labels.enabled = false;
                }
            }
            // Expressions that no longer read against the service's fields are kept (a field may
            // come back), but draw as if unset: said once for the refresh.
            let lost: usize = layer
                .labels
                .in_order()
                .map(|c| {
                    [&c.by.text, &c.by.size, &c.by.color, &c.by.angle]
                        .into_iter()
                        .filter(|t| t.is_some() && !reads(t, &layer.dataset))
                        .count()
                })
                .sum();
            if lost > 0 {
                notes.push((layer.name.clone(), lost));
            }
            layer.labels.repoint_missing_fields(&layer.dataset);
            if layer.definition_query.as_deref().is_some_and(|q| rustgis_data::query::parse_fields(q, &layer.dataset).is_err()) {
                layer.definition_query = None;
            }
            layer.display_filters.retain(|f| rustgis_data::query::parse_fields(&f.where_, &layer.dataset).is_ok());
            area_notes.push(layer.id);
            layer.touch_geometry();
            child.layer = layer.id;
            // Locked labels name the old features by id, and the new ones are numbered afresh.
            if let Some(lock) = &mut s.map_mut().label_lock {
                lock.retain(|((id, _, _, _), _)| *id != child.layer);
            }
            if let Some(request) = listing.get("request") {
                record_connection(s, child.layer, request, anonymous);
            }
        } else {
            child.layer = crate::cmd::layer::add_dataset(s, dataset);
            if let Some(request) = listing.get("request") {
                record_connection(s, child.layer, request, anonymous);
            }
            if hidden && let Some(layer) = s.map_mut().layer_mut(child.layer) {
                layer.visible = false;
            }
        }
        children.push(child);
    }
    if moved > 0 {
        s.notify(format!("{moved} saved label move(s) reset in refreshed service layers"), false);
    }
    if let Some(note) = crate::cmd::design::display_area_messages(area_notes.into_iter().filter_map(|id| s.map().layer(id))) {
        s.notify(note, false);
    }
    if !notes.is_empty() {
        let lost: usize = notes.iter().map(|(_, n)| n).sum();
        let names: Vec<&str> = notes.iter().map(|(name, _)| name.as_str()).collect();
        s.notify(
            format!(
                "label expressions that don't read against the refreshed fields are kept but left out when drawing ({lost}, in {})",
                names.join(", ")
            ),
            false,
        );
    }
    if !singled.is_empty() {
        s.notify(
            format!(
                "layers whose classes read fields the refreshed service no longer has, or no longer read as numbers, or whose class expression no longer reads, are drawn with a single symbol ({}: {})",
                singled.len(),
                singled.join(", ")
            ),
            false,
        );
    }
    let ids: Vec<u32> = children.iter().map(|c| c.layer).collect();
    if child_id.is_none() {
        let gone: Vec<u32> = old.children.iter().map(|c| c.layer).filter(|id| !ids.contains(id)).collect();
        let mut gone_moved = 0;
        for l in s.map().layers.iter().filter(|l| gone.contains(&l.id)) {
            gone_moved += l.labels.in_order().map(|class| class.moved.len()).sum::<usize>();
        }
        crate::cmd::layer::drop_layers(s, &gone);
        if gone_moved > 0 {
            s.notify(format!("{gone_moved} saved label move(s) dropped with layers the refreshed service no longer offers"), false);
        }
        if let Some(lock) = &mut s.map_mut().label_lock {
            lock.retain(|((id, _, _, _), _)| !gone.contains(id));
        }
    }
    let map = s.map_mut();
    let group = map.service_groups.iter_mut().find(|g| g.id == old.id).ok_or("the group was removed")?;
    if child_id.is_none() {
        group.anonymous |= listing["request"].get("anonymous").and_then(Value::as_bool) == Some(true);
        group.children = children;
    } else {
        // The child's provenance follows the service (a new source address or token).
        for new in children {
            if let Some(c) = group.children.iter_mut().find(|c| c.layer == new.layer) {
                *c = new;
            }
        }
    }
    Ok(json!({"group":old.id,"layers":ids}))
}

fn set_group(s: &mut Session, p: &Value) -> CmdResult {
    let (group, _) = target(s, p)?.ok_or("group is required")?;
    if p.get("remove").and_then(Value::as_bool) == Some(true) {
        let ids: Vec<u32> = group.children.iter().map(|c| c.layer).collect();
        crate::cmd::layer::drop_layers(s, &ids);
        s.map_mut().service_groups.retain(|g| g.id != group.id);
        return Ok(Value::Null);
    }
    if let Some(visible) = p.get("visible").and_then(Value::as_bool) {
        for layer in &mut s.map_mut().layers {
            if group.children.iter().any(|c| c.layer == layer.id) {
                layer.visible = visible;
            }
        }
    }
    let g = s.map_mut().service_groups.iter_mut().find(|g| g.id == group.id).ok_or("no such service group")?;
    if let Some(name) = ps(p, "name") {
        let name: String = name.trim().chars().take(200).collect();
        if name.is_empty() {
            return Err("name is required".into());
        }
        g.name = name;
    }
    if let Some(expanded) = p.get("expanded").and_then(Value::as_bool) {
        g.expanded = expanded;
    }
    Ok(json!({"group":group.id}))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn document(ids: &[&str]) -> Value {
        json!({"collections": ids.iter().map(|id| json!({
            "id": id, "title": id,
            "links": [{"rel": "items", "type": "application/geo+json",
                "href": format!("https://group-none757.example.org/collections/{id}/items")}]
        })).collect::<Vec<_>>()})
    }

    fn features() -> String {
        json!({"type": "FeatureCollection", "features": [{
            "type": "Feature", "geometry": {"type": "Point", "coordinates": [1, 2]},
            "properties": {"collection": "any"}
        }]})
        .to_string()
    }

    /// Answers keyed by the offer addresses `service.layers` reports (a service may spell them
    /// its own way), exactly as `service.import` and `service.refresh` look them up.
    fn answers(s: &mut Session, p: &Value, text: &str) -> Value {
        let plan = s.run("service.layers", p).unwrap();
        json!(plan["layers"].as_array().unwrap().iter().map(|l| json!({"url": l["offer"]["url"], "text": text})).collect::<Vec<_>>())
    }

    #[test]
    fn refresh_new_children_keep_the_unambiguous_auth_none_connection() {
        let mut s = Session::new(crate::Project::default());
        let connection = s
            .run(
                "connection.new",
                &json!({
                    "kind": "ogcapi", "name": "Public", "auth": "none",
                    "address": "https://group-none757.example.org/collections"
                }),
            )
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let mut p = json!({
            "url": "https://group-none757.example.org/collections", "type": "ogcapi",
            "connection": connection, "text": document(&["old"]).to_string()
        });
        p["responses"] = answers(&mut s, &p, &features());
        let imported = s.run("service.import", &p).unwrap();
        let old = imported["layers"][0].as_u64().unwrap() as u32;
        let mut q = json!({
            "url": "https://group-none757.example.org/collections",
            "group": imported["group"], "text": document(&["old", "new"]).to_string()
        });
        q["responses"] = answers(&mut s, &q, &features());
        s.run("service.refresh", &q).unwrap();
        assert_eq!(s.map().layers.len(), 2);
        assert!(s.map().layer(old).is_some());
        for layer in &s.map().layers {
            assert_eq!(layer.connection, connection);
            assert_eq!(layer.dataset.features.len(), 1);
        }
    }

    #[test]
    fn refresh_with_an_explicit_connection_records_it_on_existing_children() {
        let mut s = Session::new(crate::Project::default());
        let url = "https://group-none757.example.org/collections";
        let mut p = json!({"url": url, "type": "ogcapi", "text": document(&["old"]).to_string()});
        p["responses"] = answers(&mut s, &p, &features());
        let imported = s.run("service.import", &p).unwrap();
        let old = imported["layers"][0].as_u64().unwrap() as u32;
        assert_eq!(s.map().layer(old).unwrap().connection, "");
        let definition = json!({"kind": "ogcapi", "name": "Public", "auth": "none", "address": url});
        let connection = s.run("connection.new", &definition).unwrap()["id"].as_str().unwrap().to_string();
        let mut q = json!({"url": url, "group": imported["group"], "connection": connection, "text": document(&["old"]).to_string()});
        q["responses"] = answers(&mut s, &q, &features());
        s.run("service.refresh", &q).unwrap();
        assert_eq!(s.map().layer(old).unwrap().connection, connection);
    }

    #[test]
    fn an_unrelated_explicit_connection_is_not_attributed_to_imported_children() {
        let mut s = Session::new(crate::Project::default());
        let connection = s
            .run(
                "connection.new",
                &json!({
                    "kind": "ogcapi", "name": "Elsewhere", "auth": "none",
                    "address": "https://elsewhere757.example.org/collections"
                }),
            )
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let mut p = json!({
            "url": "https://group-none757.example.org/collections", "type": "ogcapi",
            "connection": connection, "text": document(&["new"]).to_string()
        });
        p["responses"] = answers(&mut s, &p, &features());
        s.run("service.import", &p).unwrap();
        assert_eq!(s.map().layers.len(), 1);
        assert_eq!(s.map().layers[0].dataset.features.len(), 1);
        assert!(s.map().layers[0].connection.is_empty());
    }
}
