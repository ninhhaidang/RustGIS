//! What each layout command reads.

use super::*;

/// Selects the explicit or first feature layer in the stored frame's map.
pub(in crate::cmd::layout) fn frame_layer_name(
    input: &impl crate::cmd::InputSource,
    l: &Layout,
    frame: u32,
    p: &Value,
) -> Result<String, String> {
    let context = input.input_context();
    let s = &context;
    let map = match l.element(frame).map(|e| &e.kind) {
        Some(ElementKind::MapFrame { map, .. }) => map.clone(),
        _ => return Err("no such frame".into()),
    };
    let m = s.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(&map)).ok_or_else(|| format!("no map '{map}'"))?;
    let found = match ps(p, "layer") {
        Some(n) => m.layer_by_name(n),
        None => m.layers.iter().find(|l| !l.is_raster() && !l.graphics),
    };
    found.map(|l| l.name.clone()).ok_or_else(|| "no such layer in the frame's map".to_string())
}

/// Resolves a frame's stored map literally; missing frame maps are errors here.
pub(in crate::cmd::layout) fn frame_map<'a>(
    input: &'a impl crate::cmd::InputSource,
    l: &Layout,
    frame: u32,
) -> Result<&'a rustgis_map::Map, String> {
    let context = input.input_context();
    let s = &context;
    let map = match l.element(frame).map(|e| &e.kind) {
        Some(ElementKind::MapFrame { map, .. }) => map,
        _ => return Err("no such frame".into()),
    };
    s.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(map)).ok_or_else(|| format!("no map '{map}'"))
}
/// Binds the chosen frame map and the layers whose extent this zoom consumes.
pub(in crate::cmd::layout) fn frame_zoom_inputs(input: &crate::cmd::InputContext<'_>, p: &Value) -> Result<crate::cmd::Inputs, String> {
    let s = input;
    let layout = s.project.layouts.get(index(input, p)?).ok_or("no layout")?;
    let frame = zoom_frame(layout, p)?;
    let map = frame_map(input, layout, frame)?;
    let mut inputs = match ps(p, "to").unwrap_or("view") {
        "layer" => crate::cmd::Inputs::named(map, [ps(p, "layer").ok_or("layer is required")?])?,
        "full" | "selection" => crate::cmd::Inputs::layers(map.layers.iter().map(|l| (map, l))),
        _ => crate::cmd::Inputs::layers(std::iter::empty()),
    };
    inputs.maps.push(map.instance_id());
    inputs.state = json!({"layout":index(input,p)?,"frame":frame});
    Ok(inputs)
}
/// Binds a new series to its actual frame map, unless the call only turns it off.
pub(in crate::cmd::layout) fn series_inputs(input: &crate::cmd::InputContext<'_>, p: &Value) -> Result<crate::cmd::Inputs, String> {
    let s = input;
    if b(p, "off") == Some(true) {
        return Ok(crate::cmd::Inputs::layers(std::iter::empty()));
    }
    let layout = s.project.layouts.get(index(input, p)?).ok_or("no layout")?;
    let frame = frame_of(layout, p)?;
    let map = frame_map(input, layout, frame)?;
    let mut inputs = crate::cmd::Inputs::named(map, [ps(p, "layer").ok_or("layer is required")?])?;
    inputs.maps.push(map.instance_id());
    inputs.state = json!({"layout":index(input,p)?,"frame":frame});
    Ok(inputs)
}
/// Binds the stored series frame/layer before changing its current page.
pub(in crate::cmd::layout) fn series_page_inputs(input: &crate::cmd::InputContext<'_>, p: &Value) -> Result<crate::cmd::Inputs, String> {
    let s = input;
    let li = index(input, p)?;
    let (layer, _) = series_layer(input, li).ok_or("the layout has no map series")?;
    let mut inputs = crate::cmd::Inputs::layers(
        s.project.maps.iter().flat_map(|m| m.layers.iter().filter(|l| std::ptr::eq(*l, layer)).map(move |l| (m, l))),
    );
    inputs.state = json!({"layout":li,"series":s.project.layouts.get(li).and_then(|l|l.series.as_ref())});
    Ok(inputs)
}
/// Collects only the selectors the requested surround validates, including chart field precedence.
pub(in crate::cmd::layout) fn element_inputs(input: &crate::cmd::InputContext<'_>, p: &Value) -> Result<crate::cmd::Inputs, String> {
    let s = input;
    let kind = ps(p, "kind").ok_or("kind is required")?;
    if !matches!(kind, "table" | "chart" | "colorbar" | "legend") {
        let mut inputs = object_inputs(input, p)?;
        if matches!(kind, "northArrow" | "scaleBar" | "extentIndicator" | "grid") {
            let layout = s.project.layouts.get(index(input, p)?).ok_or("no layout")?;
            inputs.state["frame"] = json!(frame_of(layout, p)?);
        }
        return Ok(inputs);
    }
    let layout = s.project.layouts.get(index(input, p)?).ok_or("no layout")?;
    let frame = frame_of(layout, p)?;
    if kind == "legend" && legend_exclude(input, layout, frame, p)?.is_none() && legend_labels(input, layout, frame, p)?.is_none() {
        let mut inputs = object_inputs(input, p)?;
        inputs.state["frame"] = json!(frame);
        return Ok(inputs);
    }
    let map = frame_map(input, layout, frame)?;
    let mut inputs = crate::cmd::Inputs::layers(std::iter::empty());
    if matches!(kind, "table" | "chart") {
        let name = frame_layer_name(input, layout, frame, p)?;
        inputs.extend(crate::cmd::Inputs::named(map, [name.as_str()])?);
    } else if kind == "colorbar" {
        if let Some(name) = colorbar_layer(input, layout, frame, ps(p, "layer"))? {
            inputs.extend(crate::cmd::Inputs::named(map, [name.as_str()])?);
        }
    } else {
        legend_inputs(&mut inputs, map, legend_exclude(input, layout, frame, p)?, legend_labels(input, layout, frame, p)?);
    }
    inputs.maps.push(map.instance_id());
    inputs.state = json!({"layout":index(input,p)?,"frame":frame,"selectors":inputs.state});
    Ok(inputs)
}
/// Resolves the same active/name/index layout used by export before its dependency walk.
pub(in crate::cmd::layout) fn layout_inputs(input: &crate::cmd::InputContext<'_>, p: &Value) -> Result<crate::cmd::Inputs, String> {
    Ok(render_inputs_at(input, index(input, p)?))
}
/// Collects only maps actually referenced by renderer frames; never renders or opens a source.
pub(crate) fn render_inputs_at(input: &impl crate::cmd::InputSource, li: usize) -> crate::cmd::Inputs {
    let context = input.input_context();
    let s = &context;
    let Some(layout) = s.project.layouts.get(li) else { return crate::cmd::Inputs::unresolved() };
    let mut inputs = crate::cmd::Inputs::layers(std::iter::empty());
    for element in &layout.elements {
        // A missing stored map renders a placeholder, and consumes no map/layer source.
        if let ElementKind::MapFrame { map, .. } = &element.kind
            && let Some(map) = rustgis_render::layout::map_index(&s.project.maps, map).and_then(|i| s.project.maps.get(i))
        {
            inputs.extend(crate::cmd::tiles::render_inputs(map));
        }
    }
    inputs.state = json!({"layout":li,"name":layout.name,"selectors":layout.elements.iter().filter_map(|e| element_selectors(&e.kind).map(|v|(e.id,v))).collect::<Vec<_>>(),"series":layout.series,"tiles":inputs.state});
    inputs
}

/// Finds only the generated, unreferenced globe map that publication may replace.
pub(in crate::cmd::layout) fn locator_orphan(input: &impl crate::cmd::InputSource) -> Option<usize> {
    let context = input.input_context();
    let s = &context;
    let src = s.map();
    let base = format!("{} Locator", src.name);
    let shown = |n: &str| {
        s.project
            .layouts
            .iter()
            .flat_map(|l| &l.elements)
            .any(|e| matches!(&e.kind, ElementKind::MapFrame { map, .. } if map.eq_ignore_ascii_case(n)))
    };
    let ours = |n: &str| {
        n.strip_prefix(base.as_str())
            .is_some_and(|rest| rest.is_empty() || rest.strip_prefix(' ').is_some_and(|k| k.parse::<u32>().is_ok()))
    };
    // And made as this command makes them: no web tiles, only unlabelled polygon layers,
    // so a globe map made by hand under that name is left alone.
    let made_here = |x: &rustgis_map::Map| {
        x.tiles.is_empty() && x.layers.iter().all(|l| l.geometry_type() == rustgis_data::GeometryType::Polygon && !l.labels.enabled)
    };
    s.project.maps.iter().position(|x| x.projection.is_globe() && ours(&x.name) && !shown(&x.name) && x.name != src.name && made_here(x))
}

/// Binds the active shown inputs and the optional generated map replacement.
pub(in crate::cmd::layout) fn publication_inputs(input: &crate::cmd::InputContext<'_>, p: &Value) -> Result<crate::cmd::Inputs, String> {
    let s = input;
    let (_, profile) = publication_profile(p)?;
    let locator = publication_locator(p, profile.sheet, shows_whole_globe(s.map(), p))?;
    let map = s.map();
    let mut inputs = crate::cmd::Inputs::layers(map.layers.iter().filter(|l| map.shows(l)).map(|l| (map, l)));
    inputs.maps.push(map.instance_id());
    if matches!(locator, Locator::Globe) {
        if let Some(old) = locator_orphan(input).and_then(|i| s.project.maps.get(i)) {
            inputs.maps.push(old.instance_id());
            inputs.layers.extend(old.layers.iter().map(|l| (old.instance_id(), l.id)));
        } else {
            inputs.state = json!({"locator":"new map"});
        }
    }
    Ok(inputs)
}

/// String names select another map; other values retain the active-map convention.
pub(in crate::cmd::layout) fn added_frame_map<'a>(
    input: &'a impl crate::cmd::InputSource,
    p: &Value,
) -> Result<&'a rustgis_map::Map, String> {
    let context = input.input_context();
    let s = &context;
    match ps(p, "map") {
        Some(n) => s.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(n)).ok_or_else(|| format!("no map '{n}'")),
        None => Ok(s.map()),
    }
}
/// Records the map whose current view initializes the new frame.
pub(in crate::cmd::layout) fn add_frame_inputs(input: &crate::cmd::InputContext<'_>, p: &Value) -> Result<crate::cmd::Inputs, String> {
    let mut inputs = object_inputs(input, p)?;
    inputs.maps.push(added_frame_map(input, p)?.instance_id());
    Ok(inputs)
}
/// Records only stored references actually validated by this element update.
pub(in crate::cmd::layout) fn update_inputs(input: &crate::cmd::InputContext<'_>, p: &Value) -> Result<crate::cmd::Inputs, String> {
    let s = input;
    let li = index(input, p)?;
    let id = element_id(p)?;
    let l = s.project.layouts.get(li).ok_or("no layout")?;
    let mut inputs = crate::cmd::Inputs::layers(std::iter::empty());
    if let Some((name, _)) = checked_update(input, p, li, id)? {
        let frame = match l.element(id).map(|e| &e.kind) {
            Some(ElementKind::Table { frame, .. } | ElementKind::Chart { frame, .. }) => *frame,
            _ => return Ok(inputs),
        };
        inputs.extend(crate::cmd::Inputs::named(frame_map(input, l, frame)?, [name.as_str()])?);
    }
    match l.element(id).map(|e| &e.kind) {
        Some(ElementKind::MapFrame { .. }) if ps(p, "map").is_some() => inputs.maps.push(added_frame_map(input, p)?.instance_id()),
        Some(ElementKind::Colorbar { frame, .. }) if ps(p, "layer").is_some_and(|n| !n.trim().is_empty()) => {
            if let Some(name) = colorbar_layer(input, l, *frame, ps(p, "layer"))? {
                inputs.extend(crate::cmd::Inputs::named(frame_map(input, l, *frame)?, [name.as_str()])?);
            }
        }
        Some(ElementKind::Legend { frame, .. }) => {
            let (names, labels) = (legend_exclude(input, l, *frame, p)?, legend_labels(input, l, *frame, p)?);
            if names.is_some() || labels.is_some() {
                legend_inputs(&mut inputs, frame_map(input, l, *frame)?, names, labels);
            }
        }
        _ => {}
    }
    inputs.state = json!({"layout":li,"element":id,"kind":l.element(id).map(|e|e.kind.code()),"stored":l.element(id).and_then(|e|element_selectors(&e.kind)),"selectors":inputs.state});
    Ok(inputs)
}

/// The layers a legend's `exclude` and `labels` name, as inputs. Tile layers' names are stored
/// selector text, not invented layer/table identities.
pub(in crate::cmd::layout) fn legend_inputs(
    inputs: &mut crate::cmd::Inputs,
    map: &Map,
    names: Option<Vec<String>>,
    labels: Option<Vec<(String, Option<String>)>>,
) {
    let named = names.iter().flatten().chain(labels.iter().flatten().map(|(n, _)| n));
    inputs.layers.extend(named.filter_map(|n| map.layer_by_name(n)).map(|l| (map.instance_id(), l.id)));
    inputs.state = json!({"exclude": names, "labels": labels});
}

/// Reference-only operations bind the actual chosen layout, without reading its stored maps.
pub(in crate::cmd::layout) fn object_inputs(input: &crate::cmd::InputContext<'_>, p: &Value) -> Result<crate::cmd::Inputs, String> {
    let s = input;
    let i = index(input, p)?;
    let layout = s.project.layouts.get(i).ok_or("no layout")?;
    let mut inputs = crate::cmd::Inputs::layers(std::iter::empty());
    inputs.state = json!({"layout":i,"name":layout.name});
    Ok(inputs)
}
/// Browser name-based actions use their existing name-to-layout parameter projection.
pub(in crate::cmd::layout) fn named_object_inputs(input: &crate::cmd::InputContext<'_>, p: &Value) -> Result<crate::cmd::Inputs, String> {
    object_inputs(input, &by_name(p))
}
/// Closing the layout view succeeds even when none is open.
pub(in crate::cmd::layout) fn close_inputs(input: &crate::cmd::InputContext<'_>, _: &Value) -> Result<crate::cmd::Inputs, String> {
    let s = input;
    let mut inputs = crate::cmd::Inputs::layers(std::iter::empty());
    inputs.state = json!({"activeLayout":s.active_layout});
    Ok(inputs)
}

/// Stored surround references consumed by rendering or validation, kept as private selector text.
pub(in crate::cmd::layout) fn element_selectors(kind: &ElementKind) -> Option<Value> {
    Some(match kind {
        ElementKind::MapFrame { map, extent_of, .. } => json!({"map":map,"extentOf":extent_of}),
        ElementKind::Table { frame, layer, .. } => json!({"frame":frame,"layer":layer}),
        ElementKind::Chart { frame, layer, field, label, group, stack, .. } => {
            json!({"frame":frame,"layer":layer,"field":field,"label":label,"group":group,"stack":stack})
        }
        ElementKind::Legend { frame, exclude, labels, .. } => json!({"frame":frame,"exclude":exclude,"labels":labels}),
        ElementKind::Colorbar { frame, layer, .. } => json!({"frame":frame,"layer":layer}),
        ElementKind::NorthArrow { frame, .. } | ElementKind::ScaleBar { frame, .. } => json!({"frame":frame}),
        _ => return None,
    })
}
