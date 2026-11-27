//! Chart and scale bar options.

use super::*;

/// What a chart's options given for another element are told.
pub(in crate::cmd::layout) const ONLY_CHART_OPTIONS: &str = "only charts take a label, group, statistic, order, bars, other, stack, values, decimals, barColor, \
     classes, labelAlign, mirror, max, hideLabels or barOutline";

/// The keys only a chart takes: what it draws, then how it draws it.
pub(in crate::cmd::layout) const CHART_DATA_KEYS: [&str; 7] = ["label", "group", "statistic", "order", "bars", "other", "stack"];
/// What a chart is given that changes the room its title and key leave its bars, beside its size.
pub(in crate::cmd::layout) const CHART_ROOM_KEYS: [&str; 4] = ["orientation", "title", "values", "hideLabels"];
pub(in crate::cmd::layout) const CHART_LOOK_KEYS: [&str; 9] =
    ["values", "decimals", "barColor", "classes", "labelAlign", "mirror", "max", "hideLabels", "barOutline"];

pub(in crate::cmd::layout) fn gives_chart_options(p: &Value) -> bool {
    gives_chart_data(p) || CHART_LOOK_KEYS.iter().any(|k| gives(p, k))
}

/// Whether `p` gives an element that isn't a chart the options only a chart takes: a scale bar
/// takes `decimals` as its own, in the same shape.
pub(in crate::cmd::layout) fn gives_chart_options_to(p: &Value, scale_bar: bool) -> bool {
    gives_chart_data(p) || CHART_LOOK_KEYS.iter().any(|k| !(scale_bar && *k == "decimals") && gives(p, k))
}

/// Whether `p` tells a chart what to draw, which it says its losses of again.
pub(in crate::cmd::layout) fn gives_chart_data(p: &Value) -> bool {
    CHART_DATA_KEYS.iter().any(|k| gives(p, k))
}

/// Whether `p` changes a chart's look: with a colorbar's and a legend's `orientation` and
/// `title`, and `null` too, which gives a chart's title or bar colour its default again.
pub(in crate::cmd::layout) fn changes_chart_look(p: &Value) -> bool {
    ["orientation", "title"].iter().chain(CHART_LOOK_KEYS.iter()).any(|k| p.get(*k).is_some())
}

/// What a scale bar's details given for another element are told.
pub(in crate::cmd::layout) const ONLY_SCALE_BAR_DETAILS: &str =
    "only scale bars take subdivisions, divisionsBeforeZero, labelPosition, unitLabel, unitLabelPosition or fit";

/// The keys only a scale bar takes: its details beyond its units, divisions, style and colours,
/// and its `decimals`, which a chart takes too.
pub(in crate::cmd::layout) const SCALE_BAR_KEYS: [&str; 6] =
    ["subdivisions", "divisionsBeforeZero", "labelPosition", "unitLabel", "unitLabelPosition", "fit"];

pub(in crate::cmd::layout) fn gives_scale_bar_details(p: &Value) -> bool {
    SCALE_BAR_KEYS.iter().any(|k| gives(p, k))
}

/// A whole number from 0 to `hi` given as `k`, when it's given (and not null): an error when it's
/// not a number, not whole or out of range.
pub(in crate::cmd::layout) fn given_count(p: &Value, k: &str, hi: usize) -> Result<Option<usize>, String> {
    let what = format!("a whole number from 0 to {hi}");
    match given_num(p, k, &what)? {
        Some(n) if n >= 0.0 && n.fract() == 0.0 && n <= hi as f64 => Ok(Some(n as usize)),
        Some(_) => Err(format!("{k} is {what}")),
        None => Ok(None),
    }
}

/// A choice given as `k` by name, when it's given (and not null): an error when it isn't one of
/// `names`.
pub(in crate::cmd::layout) fn given_choice<T>(p: &Value, k: &str, from: fn(&str) -> Option<T>, names: &str) -> Result<Option<T>, String> {
    match p.get(k).filter(|v| !v.is_null()) {
        Some(v) => v.as_str().and_then(from).map(Some).ok_or_else(|| format!("{k} is {names}")),
        None => Ok(None),
    }
}

/// A chart's options beyond its layer and field ([`ElementKind::Chart`], #2487).
pub(in crate::cmd::layout) struct ChartOptions {
    pub(in crate::cmd::layout) label: Option<String>,
    pub(in crate::cmd::layout) group: Option<String>,
    pub(in crate::cmd::layout) statistic: StatKind,
    pub(in crate::cmd::layout) order: ChartOrder,
    pub(in crate::cmd::layout) bars: usize,
    pub(in crate::cmd::layout) other: bool,
    pub(in crate::cmd::layout) orientation: Orientation,
    pub(in crate::cmd::layout) values: bool,
    pub(in crate::cmd::layout) decimals: Option<u8>,
    pub(in crate::cmd::layout) title: Option<String>,
    pub(in crate::cmd::layout) bar_color: Option<Rgba>,
    pub(in crate::cmd::layout) label_align: TextAlign,
    pub(in crate::cmd::layout) classes: bool,
    pub(in crate::cmd::layout) stack: Option<String>,
    pub(in crate::cmd::layout) mirror: bool,
    pub(in crate::cmd::layout) max: Option<f64>,
    pub(in crate::cmd::layout) hide_labels: bool,
    pub(in crate::cmd::layout) bar_outline: Option<ChartOutline>,
}

impl Default for ChartOptions {
    fn default() -> Self {
        ChartOptions {
            label: None,
            group: None,
            statistic: StatKind::default(),
            order: ChartOrder::default(),
            bars: CHART_BARS,
            other: false,
            orientation: Orientation::default(),
            values: false,
            decimals: None,
            title: None,
            bar_color: None,
            label_align: TextAlign::Left,
            classes: false,
            stack: None,
            mirror: false,
            max: None,
            hide_labels: false,
            bar_outline: None,
        }
    }
}

/// The chart options `p` gives over `base`, for a chart of `ds`: a label, group or stack field the
/// layer has (named as the layer names it; "" for none), a statistic, an order, a number of bars
/// and whether the rest go in an Other bar. Each one that isn't valid is refused, not dropped;
/// whether the chart can stack is [`stack_or_refuse`]'s.
pub(in crate::cmd::layout) fn chart_options(p: &Value, base: ChartOptions, ds: &rustgis_data::Dataset) -> Result<ChartOptions, String> {
    let field = |key: &str, what: &str| -> Result<Option<Option<String>>, String> {
        match p.get(key).filter(|v| !v.is_null()) {
            None => Ok(None),
            Some(v) => match v.as_str().map(str::trim) {
                Some("") => Ok(Some(None)),
                Some(n) => ds
                    .field_index(n)
                    .and_then(|i| ds.fields.get(i))
                    .map(|f| Some(Some(f.name.clone())))
                    .ok_or_else(|| format!("the layer has no field '{n}' to {what}")),
                None => Err(format!("{key} is a field's name, or \"\" for none")),
            },
        }
    };
    let label = field("label", "label the bars with")?.unwrap_or(base.label);
    let group = field("group", "group by")?.unwrap_or(base.group);
    let stack = field("stack", "stack by")?.unwrap_or(base.stack);
    let statistic = match p.get("statistic").filter(|v| !v.is_null()) {
        None => base.statistic,
        Some(v) => v.as_str().and_then(StatKind::from_code).ok_or("statistic is count, sum, mean, min or max")?,
    };
    let order = match p.get("order").filter(|v| !v.is_null()) {
        None => base.order,
        Some(v) => v.as_str().and_then(ChartOrder::from_code).ok_or("order is descending, ascending, category or layer")?,
    };
    let bars = match p.get("bars").filter(|v| !v.is_null()) {
        None => base.bars,
        Some(v) => v
            .as_u64()
            .filter(|n| (1..=MAX_CHART_BARS as u64).contains(n))
            .map(|n| n as usize)
            .ok_or_else(|| format!("bars is a whole number from 1 to {MAX_CHART_BARS}"))?,
    };
    let other = match p.get("other").filter(|v| !v.is_null()) {
        None => base.other,
        Some(v) => crate::cmd::bool_value(v).ok_or("other must be true or false")?,
    };
    chart_look_options(p, ChartOptions { label, group, statistic, order, bars, other, stack, ..base })
}

/// The look options `p` gives a chart over `base` (#2487): none reads its layer, so a chart
/// whose layer is gone still takes them. Each one that isn't valid is refused, not dropped.
pub(in crate::cmd::layout) fn chart_look_options(p: &Value, base: ChartOptions) -> Result<ChartOptions, String> {
    let orientation = orientation(p)?.unwrap_or(base.orientation);
    let values = match p.get("values").filter(|v| !v.is_null()) {
        None => base.values,
        Some(v) => crate::cmd::bool_value(v).ok_or("values must be true or false")?,
    };
    let decimals = match p.get("decimals").filter(|v| !v.is_null()) {
        None => base.decimals,
        Some(v) if v.as_str().is_some_and(|t| t.trim().eq_ignore_ascii_case("auto")) => None,
        Some(v) => Some(
            v.as_u64()
                .filter(|n| *n <= MAX_CHART_DECIMALS as u64)
                .map(|n| n as u8)
                .ok_or_else(|| format!("decimals is \"auto\" or a whole number from 0 to {MAX_CHART_DECIMALS}"))?,
        ),
    };
    // Null gives it back the title it is given, as it gives the bars their default colour.
    let title = match p.get("title") {
        None => base.title,
        Some(Value::Null) => None,
        Some(v) => Some(crate::cmd::within(
            "title",
            "a chart's title",
            v.as_str().ok_or("a chart's title is a text, \"\" for none, or null for the title it is given")?,
            200,
        )?),
    };
    let bar_color = match p.get("barColor") {
        None => base.bar_color,
        Some(Value::Null) => None,
        Some(_) => Some(color(p, "barColor", CHART_BAR)?),
    };
    // Columns centre their labels whatever it says, so a centre has nothing to set.
    let label_align = match p.get("labelAlign").filter(|v| !v.is_null()) {
        None => base.label_align,
        Some(v) => v.as_str().and_then(TextAlign::from_name).filter(|a| *a != TextAlign::Center).ok_or("labelAlign is left or right")?,
    };
    // Coloured by classes, it reads its layer's colours when drawn, not now.
    let classes = match p.get("classes").filter(|v| !v.is_null()) {
        None => base.classes,
        Some(v) => crate::cmd::bool_value(v).ok_or("classes must be true or false")?,
    };
    // Facing another chart (#2487 part 4): mirrored, on a fixed maximum, its labels hidden.
    let mirror = match p.get("mirror").filter(|v| !v.is_null()) {
        None => base.mirror,
        Some(v) => crate::cmd::bool_value(v).ok_or("mirror must be true or false")?,
    };
    // Null or "auto" gives it back its bars' own; a maximum not above 0 has no bars' room to end.
    let max = match p.get("max") {
        None => base.max,
        Some(Value::Null) => None,
        Some(v) if v.as_str().is_some_and(|t| t.trim().eq_ignore_ascii_case("auto")) => None,
        Some(v) => Some(
            v.as_f64()
                .and_then(|m| rustgis_map::layout::chart_max(Some(m)))
                .ok_or("max is a number above 0, or \"auto\" (or null) for its bars' own")?,
        ),
    };
    let hide_labels = match p.get("hideLabels").filter(|v| !v.is_null()) {
        None => base.hide_labels,
        Some(v) => crate::cmd::bool_value(v).ok_or("hideLabels must be true or false")?,
    };
    // A line round each bar (#2487 part 4b): the keys not given keep the outline's, or the one it
    // is first given; a width of 0, or null, none.
    let bar_outline = match p.get("barOutline") {
        None => base.bar_outline,
        Some(Value::Null) => None,
        Some(v @ Value::Object(_)) => {
            let current = base.bar_outline.unwrap_or(CHART_OUTLINE);
            let (color, width) = outline_style(v, (current.color, current.width), (0.0, MAX_CHART_OUTLINE_WIDTH))?;
            Some(ChartOutline { color, width }).filter(|o| o.width > 0.0)
        }
        Some(_) => return Err("barOutline is an object of color and width (points, 0 for none), or null for none".into()),
    };
    Ok(ChartOptions {
        orientation,
        values,
        decimals,
        title,
        bar_color,
        label_align,
        classes,
        mirror,
        max,
        hide_labels,
        bar_outline,
        ..base
    })
}

/// A stack field a chart of `o` can't draw ([`rustgis_map::layout::chart_stack_refusal`], #2487
/// part 3b): refused when `p` gives it, let go when it is kept from before and `p` changes what it
/// needs, which is returned to be said.
pub(in crate::cmd::layout) fn stack_or_refuse(p: &Value, o: &mut ChartOptions) -> Result<Option<String>, String> {
    let Some(st) = o.stack.as_deref() else { return Ok(None) };
    let Some(why) = rustgis_map::layout::chart_stack_refusal(o.group.as_deref(), o.statistic, st) else { return Ok(None) };
    if gives(p, "stack") {
        return Err(format!("the chart can't stack by '{st}': {why} (it stacks grouped by another field, summing or counting)"));
    }
    let said = format!("the chart no longer stacks by '{st}': {why}");
    o.stack = None;
    Ok(Some(said))
}

/// Whether a chart of these options charts `field`: a number, or any field when it groups and
/// counts, which reads none (ungrouped, its statistic isn't read).
pub(in crate::cmd::layout) fn chart_takes(field: &rustgis_data::Field, o: &ChartOptions) -> bool {
    field.ty.is_numeric() || o.group.is_some() && o.statistic == StatKind::Count
}

/// Refuses a chart field that [`chart_takes`] doesn't.
pub(in crate::cmd::layout) fn chart_measures(field: &rustgis_data::Field, o: &ChartOptions) -> Result<(), String> {
    if chart_takes(field, o) {
        return Ok(());
    }
    Err(format!("'{}' isn't a numeric field: a chart measures a number, unless it groups and counts", field.name))
}

/// The features of `layer` in the map of the frame `frame`, as a chart on it draws them.
pub(in crate::cmd::layout) fn frame_dataset<'a>(s: &'a Session, l: &Layout, frame: u32, layer: &str) -> Option<&'a rustgis_data::Dataset> {
    frame_layer(s, l, frame, layer).map(|l| &l.dataset)
}

/// The layer `layer` in the map of the frame `frame`, with its symbology for a chart coloured by it.
pub(in crate::cmd::layout) fn frame_layer<'a>(s: &'a Session, l: &Layout, frame: u32, layer: &str) -> Option<&'a rustgis_map::Layer> {
    let Some(ElementKind::MapFrame { map, .. }) = l.element(frame).map(|e| &e.kind) else { return None };
    s.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(map))?.layer_by_name(layer)
}

pub(in crate::cmd::layout) fn is_chart(s: &Session, li: usize, id: u32) -> bool {
    matches!(s.project.layouts.get(li).and_then(|l| l.element(id)).map(|e| &e.kind), Some(ElementKind::Chart { .. }))
}

/// A chart's options after `layout.update`, the field it now measures when it changes, and what it
/// says.
pub(in crate::cmd::layout) type ChartChange = (ChartOptions, Option<String>, Vec<String>);

/// A chart's options after `layout.update` (`p`) with the layer and field it will chart
/// (`checked`, from [`checked_update`]), and what it says of the label or group it loses on a
/// new layer without them; `None` when `p` changes neither. A field `p` doesn't name that the
/// chart no longer measures (ungrouped, or no longer counting) gives way to the layer's first
/// numeric field, which is said and returned. Look options alone read nothing of its layer.
/// Chart options given for another element are refused.
pub(in crate::cmd::layout) fn chart_update(
    s: &Session,
    p: &Value,
    li: usize,
    id: u32,
    checked: Option<&(String, Option<String>)>,
) -> Result<Option<ChartChange>, String> {
    let l = s.project.layouts.get(li).ok_or("no layout")?;
    let e = l.element(id).ok_or_else(|| format!("no element {id}"))?;
    let ElementKind::Chart {
        frame,
        layer,
        field,
        label,
        group,
        statistic,
        order,
        bars,
        other,
        orientation,
        values,
        decimals,
        title,
        bar_color,
        label_align,
        classes,
        stack,
        mirror,
        max,
        hide_labels,
        bar_outline,
        ..
    } = &e.kind
    else {
        let bar = matches!(e.kind, ElementKind::ScaleBar { .. });
        return if gives_chart_options_to(p, bar) { Err(ONLY_CHART_OPTIONS.into()) } else { Ok(None) };
    };
    if checked.is_none() && !gives_chart_options(p) && !changes_chart_look(p) {
        return Ok(None);
    }
    let look = ChartOptions {
        label: label.clone(),
        group: group.clone(),
        statistic: *statistic,
        order: *order,
        bars: *bars,
        other: *other,
        orientation: *orientation,
        values: *values,
        decimals: *decimals,
        title: title.clone(),
        bar_color: *bar_color,
        label_align: *label_align,
        classes: *classes,
        stack: stack.clone(),
        mirror: *mirror,
        max: *max,
        hide_labels: *hide_labels,
        bar_outline: *bar_outline,
    };
    // Told only how to draw, it reads nothing of its layer, which may be gone.
    if checked.is_none() && !gives_chart_data(p) {
        return Ok(Some((chart_look_options(p, look)?, None, Vec::new())));
    }
    let stored = field.as_str();
    let (layer, field) = match checked {
        Some((name, f)) => (name.as_str(), f.as_deref().unwrap_or(stored)),
        None => (layer.as_str(), stored),
    };
    let ds = frame_dataset(s, l, *frame, layer).ok_or_else(|| format!("no feature layer '{layer}' in the frame's map"))?;
    // On a new layer, a label, group or stack field it hasn't is let go, and said.
    let mut said = Vec::new();
    let mut keep = |name: &Option<String>, what: &str| {
        let n = name.as_deref()?;
        let kept = ds.field_index(n).and_then(|i| ds.fields.get(i)).map(|f| f.name.clone());
        if kept.is_none() {
            said.push(format!("{layer} has no field '{n}': the chart no longer {what}"));
        }
        kept
    };
    // A label, group or stack the update itself sets or takes away isn't said to be lost.
    let base = ChartOptions {
        label: if gives(p, "label") { None } else { keep(label, "labels its bars with it") },
        group: if gives(p, "group") { None } else { keep(group, "groups by it") },
        stack: if gives(p, "stack") { None } else { keep(stack, "stacks by it") },
        ..look
    };
    let mut o = chart_options(p, base, ds)?;
    said.extend(stack_or_refuse(p, &mut o)?);
    let f = ds.field_index(field).and_then(|i| ds.fields.get(i)).ok_or_else(|| format!("no field '{field}' in {layer}"))?;
    if gives(p, "field") || chart_takes(f, &o) {
        chart_measures(f, &o)?;
        // A field picked for it (on a new layer without one it can measure) is said too.
        if !gives(p, "field") && field != stored {
            said.push(format!("{layer} has no field '{stored}' the chart can measure: it now measures {field}"));
        }
        return Ok(Some((o, None, said)));
    }
    let to = ds.fields.iter().find(|f| f.ty.is_numeric()).ok_or_else(|| {
        format!("'{field}' isn't a numeric field and {layer} has none: a chart measures a number, unless it groups and counts")
    })?;
    said.push(format!("'{field}' isn't a number: the chart now measures {}", to.name));
    Ok(Some((o, Some(to.name.clone()), said)))
}

/// What a chart leaves out of what it is told to draw (an existing loss made said): the bars
/// past its limit, and the features with no number in its field; coloured by classes, the bars
/// that keep its bar colour, their features having several, and stacked, the values of its key
/// gathered in its Other or keeping their palette colour. It reads each feature of the
/// chart's layer once, on the command's thread, as drawing the chart does on every draw of the
/// page: run only when a chart is added, told what to draw or coloured by classes, not when it
/// is moved; resized or restyled, a stacked chart reads it for [`say_chart_crowded`] alone.
pub(in crate::cmd::layout) fn say_chart_losses(s: &mut Session, li: usize, id: u32) {
    say_chart(s, li, id, false);
}

/// [`say_chart_losses`]' warning of a chart whose title and key leave its bars no room, alone.
pub(in crate::cmd::layout) fn say_chart_crowded(s: &mut Session, li: usize, id: u32) {
    say_chart(s, li, id, true);
}

/// [`say_chart_losses`], or `crowded_only` [`say_chart_crowded`].
pub(in crate::cmd::layout) fn say_chart(s: &mut Session, li: usize, id: u32, crowded_only: bool) {
    let said = {
        let Some(l) = s.project.layouts.get(li) else { return };
        let Some(e) = l.element(id) else { return };
        let ElementKind::Chart { frame, layer, field, .. } = &e.kind else { return };
        // Only a stacked chart can be crowded, so for that alone an unstacked one reads nothing.
        if crowded_only && e.kind.chart_stack().is_none() {
            return;
        }
        // Read as the page draws it: at the frame's scale and on the series page shown.
        let Some((m, env)) = rustgis_render::layout::frame_page_env(l, &s.project.maps, *frame) else { return };
        let Some(data) = m.layer_by_name(layer).and_then(|fl| e.kind.chart_data_of(fl, &env)) else { return };
        let mut said = if crowded_only { Vec::new() } else { chart_losses(&e.kind, &data, layer, field) };
        if rustgis_render::layout::chart_crowded(&data, &e.kind.chart_look().unwrap_or_default(), e.w, e.h) {
            said.push("the chart's title and key leave its bars no room: it draws as a placeholder until its box is taller".to_string());
        }
        said
    };
    for w in said {
        s.warn(w);
    }
}

/// What [`say_chart_losses`] says of the chart `kind` of `layer` and `field`, drawing `data`.
pub(in crate::cmd::layout) fn chart_losses(
    kind: &ElementKind,
    data: &rustgis_map::layout::ChartData,
    layer: &str,
    field: &str,
) -> Vec<String> {
    let mut said = Vec::new();
    if data.left_out > 0 {
        let drawn = data.bars.len();
        said.push(format!(
            "the chart draws {drawn} of {} bars: {} left out (\"bars\" draws up to {MAX_CHART_BARS}, \"other\": true gathers the rest in an Other bar)",
            drawn + data.left_out,
            data.left_out
        ));
    }
    if data.out_of_range > 0 {
        let n = data.out_of_range;
        said.push(format!(
            "{n} bar{} of the chart left out: {} statistic is past a number's range",
            if n == 1 { "" } else { "s" },
            if n == 1 { "its" } else { "their" }
        ));
    }
    if data.no_number > 0 {
        let n = data.no_number;
        said.push(format!("{n} feature{} of {layer} with no number in {field} left out of the chart", if n == 1 { "" } else { "s" }));
    }
    // Coloured by classes, the bars that keep the bar colour all the same.
    for (n, why) in [(data.mixed, "have several colours"), (data.unshown, "show no colour")] {
        if n > 0 {
            let (plural, its) = if n == 1 { ("", "its") } else { ("s", "their") };
            let keep = if n == 1 { "keeps" } else { "keep" };
            said.push(format!("{n} bar{plural} {keep} the chart's bar colour: {its} features {why} on the map"));
        }
    }
    // Stacked, the values gathered in its key's Other, and coloured by classes, those that keep
    // their palette colour.
    if data.key_gathered > 0 {
        let n = data.key_gathered;
        let named = data.key.len().saturating_sub(1);
        let stack = kind.chart_stack().unwrap_or_default();
        said.push(format!(
            "the chart's key names {named} of the {} values of {stack} on its bars: {n} {} gathered in its grey Other",
            named + n,
            if n == 1 { "is" } else { "are" }
        ));
    }
    if data.key_kept > 0 {
        let n = data.key_kept;
        let (plural, its, keep) = if n == 1 { ("", "its", "keeps") } else { ("s", "their", "keep") };
        said.push(format!(
            "{n} value{plural} of the chart's key {keep} {its} palette colour: {its} features have several colours on the map, or show none"
        ));
    }
    if let Some(max) = kind.chart_look().and_then(|l| l.max) {
        // How far each bar reaches above zero, its stretches above it added up when stacked.
        let reach = |i: usize, v: f64| data.stretches.get(i).filter(|s| !s.is_empty()).map_or(v, |s| s.iter().filter(|v| **v > 0.0).sum());
        let n = data.bars.iter().enumerate().filter(|(i, (v, _))| reach(*i, *v) > max).count();
        if n > 0 {
            let (plural, run) = if n == 1 { ("", "runs") } else { ("s", "run") };
            let max = rustgis_map::layout::chart_value_text(max, None);
            said.push(format!("{n} bar{plural} of the chart {run} past its maximum of {max}: drawn to the end of its bars' room"));
        }
    }
    if data.stack_gone {
        let stack = kind.chart_stack().unwrap_or_default();
        said.push(format!("the chart draws its bars whole: {layer} has no field '{stack}' to stack them by"));
    }
    if data.whole > 0 {
        let n = data.whole;
        let (plural, is, its) = if n == 1 { ("", "is", "its") } else { ("s", "are", "their") };
        said.push(format!("{n} bar{plural} of the chart {is} drawn whole, not stacked: {its} stretches add up past the largest number"));
    }
    said
}
