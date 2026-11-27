//! Legends and colorbars.

use super::*;

pub(in crate::cmd::layout) const ONLY_LEGEND_COLUMNS: &str = "only legends have columns";
/// Refuse the optional no-data key on an element that cannot display it.
pub(in crate::cmd::layout) const ONLY_LEGEND_NO_DATA: &str = "only legends take showNoData";
pub(in crate::cmd::layout) const ONLY_LEGEND_LABELS: &str = "only legends label layers";

/// A box sized to its content, less the padding again, can come out a rounding error short of it:
/// that still fits.
pub(in crate::cmd::layout) const LEGEND_FIT_MM: f64 = 1e-6;

/// What a legend's rows `size` in `columns` columns lose in a box `room`, in mm, each said once.
pub(in crate::cmd::layout) fn legend_fit_warnings(columns: usize, (w, h): (f64, f64), (room_w, room_h): (f64, f64)) -> Vec<&'static str> {
    // With no rows there is nothing to lose, in a box shorter than its title too.
    let (too_wide, too_tall) = (h > 0.0 && w > room_w + LEGEND_FIT_MM, h > 0.0 && h > room_h + LEGEND_FIT_MM);
    let mut said = Vec::new();
    if columns > 1 {
        // Its columns close up rather than leave one out.
        if too_wide {
            said.push("the legend's columns are wider than its box: they close up, long labels run into the next, and the last column's past the box");
        }
        if too_tall {
            said.push("the legend lists more than fits its box: its last rows are left out");
        }
    } else if too_wide || too_tall {
        said.push("the legend lists more than fits its box: its last rows or the ends of long labels are left out");
    }
    said
}

/// What a legend of `map` can't key, or keys only in part, for the rasters it shows by classes
/// (#1679), those `exclude` leaves out aside: one warning a kind, with how many and their names.
/// A colour table over more values than a legend can list (over 256, or not whole numbers) gives
/// no class and no key; classes taken from an overview's statistics (a COG read in place), or from
/// a sample of a large raster under a mask, can miss a rare one the map draws, unless
/// `all_classes` lists every class of the renderer.
pub(in crate::cmd::layout) fn raster_key_notices(map: &Map, exclude: &[String], all_classes: bool) -> Vec<String> {
    let shown: Vec<(&str, &rustgis_map::RasterLayer)> = map
        .layers
        .iter()
        .filter(|l| map.shows(l) && !exclude.iter().any(|n| n.eq_ignore_ascii_case(&l.name)))
        .filter_map(|l| Some((l.name.as_str(), l.raster.as_ref()?)))
        .collect();
    let names = |kind: &dyn Fn(&rustgis_map::RasterLayer) -> bool| -> Vec<&str> {
        shown.iter().filter(|(_, r)| kind(r)).map(|(n, _)| *n).collect()
    };
    let unlisted = names(&rustgis_map::RasterLayer::classes_unknown);
    let reduced = names(&|r| r.classes_from_overview(all_classes));
    let sampled = names(&|r| r.classes_from_sample(all_classes));
    let mut said = Vec::new();
    if !unlisted.is_empty() {
        said.push(format!(
            "{} raster(s) drawn by a colour table over more values than a legend can list (over 256, or not whole numbers) have no key in the legend: {}",
            unlisted.len(),
            unlisted.join(", ")
        ));
    }
    if !reduced.is_empty() {
        said.push(format!(
            "{} raster(s) drawn by classes were read through an overview, so the legend lists the classes found there and may miss a rare one the map draws: {}",
            reduced.len(),
            reduced.join(", ")
        ));
    }
    if !sampled.is_empty() {
        said.push(format!(
            "{} raster(s) drawn by classes under a mask have more cells than their masked statistics read, so the legend lists the classes of a sample and may miss a rare one the map draws: {}",
            sampled.len(),
            sampled.join(", ")
        ));
    }
    said
}

/// Says, a kind once, what the legend `id` of the layout `li` can't key ([`raster_key_notices`]),
/// as `layout.publication` does for its own: for a legend added, or told what it leaves out or
/// whether it lists every class.
pub(in crate::cmd::layout) fn say_raster_keys(s: &mut Session, li: usize, id: u32) {
    let Some(l) = s.project.layouts.get(li) else { return };
    let Some(ElementKind::Legend { frame, exclude, all_classes, .. }) = l.element(id).map(|e| &e.kind) else { return };
    let Some(ElementKind::MapFrame { map, .. }) = l.element(*frame).map(|f| &f.kind) else { return };
    let said = s.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(map)).map(|m| raster_key_notices(m, exclude, *all_classes));
    for said in said.unwrap_or_default() {
        s.warn(said);
    }
}

/// Whether a legend `kind`, after a command with `p`, says what it loses in its box (#2581): the
/// one rule `layout.addElement` and `layout.update` both take. Told when it is given what sets
/// what it needs of its box (its columns, its missing-value row, or its text size, `null`
/// included, which gives the default back) or labels (text chosen for the box), and, in columns
/// or at a text size of its own, when its box or what it lists changes. A move changes neither.
pub(in crate::cmd::layout) fn tells_legend_fit(kind: &ElementKind, p: &Value) -> bool {
    let ElementKind::Legend { columns, text_size, .. } = kind else { return false };
    let given = |k: &str| p.get(k).is_some_and(|v| !v.is_null());
    let watched = *columns > 1 || *text_size != rustgis_map::layout::LEGEND_TEXT_PT;
    let refitted = ["w", "h", "title", "allClasses", "hideOther", "exclude", "labels"].iter().any(|k| given(k));
    ["columns", "showNoData", "labels"].iter().any(|k| given(k)) || p.get("textSize").is_some() || (watched && refitted)
}

/// Says what a legend loses in its box, as `layout.publication` does for its own, or that it can't
/// tell.
pub(in crate::cmd::layout) fn say_legend_fit(s: &mut Session, li: usize, id: u32) {
    let Some(l) = s.project.layouts.get(li) else { return };
    let Some(e) = l.element(id) else { return };
    let ElementKind::Legend { frame, title, all_classes, hide_other, show_no_data, exclude, columns, labels, text_size, .. } = &e.kind
    else {
        return;
    };
    let shown = match l.element(*frame).map(|f| &f.kind) {
        Some(ElementKind::MapFrame { map, center, scale, .. }) => Some((map.clone(), *center, *scale)),
        _ => None,
    };
    let (room_w, room_h) = (e.w, e.h - if title.is_empty() { 0.0 } else { crate::render::layout::legend_title_mm(*text_size) });
    let (all_classes, hide_other, show_no_data, exclude, columns, labels, text_size) =
        (*all_classes, *hide_other, *show_no_data, exclude.clone(), *columns, labels.clone(), *text_size);
    let found = shown
        .and_then(|(name, center, scale)| Some((s.project.maps.iter_mut().find(|m| m.name.eq_ignore_ascii_case(&name))?, center, scale)));
    let Some((map, center, scale)) = found else {
        s.warn("the legend's frame shows no map of this project, so the legend lists nothing");
        return;
    };
    let at = crate::render::layout::frame_render_scale(map, center, scale);
    let Some(sizes) = crate::render::layout::legend_sizes_with_no_data_mm(
        map,
        at,
        all_classes,
        hide_other,
        (show_no_data, text_size),
        (&exclude, &labels),
    ) else {
        s.warn("a layer's source couldn't be read through, so the legend's fit in its box wasn't checked; exporting says why");
        return;
    };
    let size = sizes[columns.clamp(1, MAX_LEGEND_COLUMNS) - 1];
    for said in legend_fit_warnings(columns, size, (room_w, room_h)) {
        s.warn(said);
    }
}

/// What a scale bar is given that changes its labels (#2581): its text size (`null` included, which
/// gives the default back), its units and divisions, which change what they say, and its details,
/// which change its rows, its labels and the room they have (`null` included: a unit label given
/// back is its abbreviation again).
pub(in crate::cmd::layout) const BAR_FIT_KEYS: [&str; 10] = [
    "textSize",
    "units",
    "divisions",
    "subdivisions",
    "divisionsBeforeZero",
    "labelPosition",
    "unitLabel",
    "unitLabelPosition",
    "decimals",
    "fit",
];

/// The `"columns"` a legend is given, refused outside 1 to [`MAX_LEGEND_COLUMNS`].
pub(in crate::cmd::layout) fn legend_columns_param(p: &Value) -> Result<Option<usize>, String> {
    let Some(v) = p.get("columns").filter(|v| !v.is_null()) else { return Ok(None) };
    v.as_u64()
        .and_then(|n| usize::try_from(n).ok())
        .filter(|n| (1..=MAX_LEGEND_COLUMNS).contains(n))
        .map(Some)
        .ok_or_else(|| format!("columns is a whole number from 1 to {MAX_LEGEND_COLUMNS}"))
}

/// What a colorbar with no layer of its own on `frame` says when the frame's map shows no raster
/// with a colour scale for it but one drawn by classes (#1679): it draws an empty placeholder; the
/// legend lists those classes, unless a colour table has more values than it can list. `None`
/// otherwise. Said when the colorbar is added, or set back to no layer.
pub(in crate::cmd::layout) fn colorbar_without_raster(s: &Session, li: usize, frame: u32) -> Option<String> {
    let l = s.project.layouts.get(li)?;
    let Some(ElementKind::MapFrame { map, .. }) = l.element(frame).map(|f| &f.kind) else { return None };
    let m = s.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(map))?;
    if rustgis_map::layout::colorbar_layer(m, None).is_some() {
        return None;
    }
    // Those the legend lists, and colour tables over more values than it can list, which nothing keys.
    let (mut listed, mut unlisted) = (Vec::new(), Vec::new());
    for l in m.layers.iter().filter(|l| m.shows(l)) {
        match l.raster.as_ref().filter(|r| r.style.renderer.draws_classes()) {
            Some(r) if r.classes_unknown() => unlisted.push(l.name.as_str()),
            Some(_) => listed.push(l.name.as_str()),
            None => {}
        }
    }
    let mut why = Vec::new();
    if !listed.is_empty() {
        why.push(format!(
            "{} is drawn by classes, which a legend lists rather than a colorbar (give \"layer\" to show its colours on one anyway)",
            listed.join(", ")
        ));
    }
    if !unlisted.is_empty() {
        why.push(format!(
            "{} draws a colour table over more values than a legend or a colorbar can list (over 256, or not whole numbers), so nothing keys it",
            unlisted.join(", ")
        ));
    }
    (!why.is_empty())
        .then(|| format!("the colorbar has no raster with a colour scale to show, so it draws an empty placeholder: {}", why.join("; ")))
}

/// The raster layer a colorbar on `frame` shows: `name` checked against the frame's map, or the
/// map's top visible raster with a colour scale, not drawn by classes
/// ([`rustgis_map::layout::colorbar_layer`]; `None` when the map has none yet).
pub(in crate::cmd::layout) fn colorbar_layer(
    input: &impl crate::cmd::InputSource,
    l: &Layout,
    frame: u32,
    name: Option<&str>,
) -> Result<Option<String>, String> {
    let context = input.input_context();
    let s = &context;
    let map = match l.element(frame).map(|e| &e.kind) {
        Some(ElementKind::MapFrame { map, .. }) => map.clone(),
        _ => return Err("no such frame".into()),
    };
    let m = s.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(&map)).ok_or_else(|| format!("no map '{map}'"))?;
    match name.map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => match rustgis_map::layout::colorbar_layer(m, Some(n)) {
            Some(layer) => Ok(Some(layer.name.clone())),
            None => Err(format!("no raster layer '{n}' in {map}")),
        },
        None => Ok(rustgis_map::layout::colorbar_layer(m, None).map(|l| l.name.clone())),
    }
}

/// A legend's `exclude` from `p`: the names of layers in its frame's map, as the map spells them;
/// `None` when not given. Not a list of names, or a name the map hasn't, is an error.
pub(in crate::cmd::layout) fn legend_exclude(
    input: &impl crate::cmd::InputSource,
    l: &Layout,
    frame: u32,
    p: &Value,
) -> Result<Option<Vec<String>>, String> {
    let Some(v) = p.get("exclude").filter(|v| !v.is_null()) else { return Ok(None) };
    let names = v.as_array().ok_or("exclude is a list of layer names")?;
    if names.len() > rustgis_map::layout::MAX_LEGEND_EXCLUDED {
        return Err(format!("a legend leaves out at most {} layers", rustgis_map::layout::MAX_LEGEND_EXCLUDED));
    }
    let map = legend_map(input, l, frame)?;
    let mut out: Vec<String> = Vec::new();
    for n in names {
        let n = n.as_str().ok_or("exclude is a list of layer names")?;
        let name = legend_layer(map, n)?;
        if !out.iter().any(|o| o.eq_ignore_ascii_case(name)) {
            out.push(name.clone());
        }
    }
    Ok(Some(out))
}

/// The map a legend's frame shows.
pub(in crate::cmd::layout) fn legend_map<'m>(input: &'m impl crate::cmd::InputSource, l: &Layout, frame: u32) -> Result<&'m Map, String> {
    let context = input.input_context();
    let s = &context;
    let map_name = match l.element(frame).map(|e| &e.kind) {
        Some(ElementKind::MapFrame { map, .. }) => map,
        _ => return Err("the legend's frame isn't a map frame".into()),
    };
    s.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(map_name)).ok_or_else(|| format!("no map '{map_name}'"))
}

/// The name of the layer `n` (any case, ends trimmed) of a legend's `map`, as the map spells it:
/// one of its layers, or a WMS or WMTS layer's (whose legend is its service's picture). One the
/// map hasn't is an error.
pub(in crate::cmd::layout) fn legend_layer<'m>(map: &'m Map, n: &str) -> Result<&'m String, String> {
    map.layer_by_name(n.trim())
        .map(|l| &l.name)
        .or_else(|| map.tiles.iter().find(|t| t.name.eq_ignore_ascii_case(n.trim())).map(|t| &t.name))
        .ok_or_else(|| format!("no layer '{n}' in map '{}'", map.name))
}

/// The shape a legend's `labels` takes.
pub(in crate::cmd::layout) const LABELS_SHAPE: &str = r#"labels is an object of layer names and labels: {"<layer>": "<label>" | null}"#;

/// A legend's `labels` from `p` (#1654): each layer's name, as its frame's map spells it, with the
/// label to show for it, or `None` to show its name again (a `null` or blank label). `None` when
/// not given; an empty list for `{}`, which shows every layer's name again. Not an object of
/// labels, a name the map hasn't, or a layer named twice is an error.
pub(in crate::cmd::layout) fn legend_labels(
    input: &impl crate::cmd::InputSource,
    l: &Layout,
    frame: u32,
    p: &Value,
) -> Result<Option<Vec<(String, Option<String>)>>, String> {
    let Some(v) = p.get("labels").filter(|v| !v.is_null()) else { return Ok(None) };
    let given = v.as_object().ok_or(LABELS_SHAPE)?;
    if given.len() > MAX_LEGEND_LABELS {
        return Err(format!("a legend can label at most {MAX_LEGEND_LABELS} layers"));
    }
    let map = legend_map(input, l, frame)?;
    let mut out: Vec<(String, Option<String>)> = Vec::new();
    for (n, label) in given {
        let label = match label {
            Value::Null => None,
            Value::String(t) if t.trim().is_empty() => None,
            // Refused rather than cut, so a label is never shown other than as given.
            Value::String(t) if t.chars().count() > MAX_LEGEND_LABEL_CHARS => {
                return Err(format!("a legend's label is at most {MAX_LEGEND_LABEL_CHARS} characters ('{n}' has {})", t.chars().count()));
            }
            Value::String(t) => Some(t.clone()),
            _ => return Err(LABELS_SHAPE.into()),
        };
        let name = legend_layer(map, n)?;
        // Which of two spellings of a name counts is not the caller's to guess: refused.
        if out.iter().any(|(o, _)| o.eq_ignore_ascii_case(name)) {
            return Err(format!("labels names layer '{name}' twice"));
        }
        out.push((name.clone(), label));
    }
    Ok(Some(out))
}

/// A legend's `labels` with those `given` ([`legend_labels`]) put in: each layer's label replaced,
/// or dropped for `None`; none at all for an empty list. More than a legend keeps is an error.
pub(in crate::cmd::layout) fn with_labels(
    labels: &BTreeMap<String, String>,
    given: Vec<(String, Option<String>)>,
) -> Result<BTreeMap<String, String>, String> {
    let mut out = if given.is_empty() { BTreeMap::new() } else { labels.clone() };
    for (name, label) in given {
        out.retain(|n, _| !n.eq_ignore_ascii_case(&name));
        if let Some(t) = label {
            out.insert(name, t);
        }
    }
    if out.len() > MAX_LEGEND_LABELS {
        return Err(format!("a legend can label at most {MAX_LEGEND_LABELS} layers"));
    }
    Ok(out)
}
