//! A chart's Format rows.

use super::*;
use crate::RustGisApp;
use crate::widgets::Dragged;
use egui::Ui;
use rustgis_engine::map::layout::{
    ChartOrder, Element, ElementKind, Layout, MAX_CHART_BARS, MAX_CHART_DECIMALS, Orientation, TextAlign, chart_stack_refusal,
};
use rustgis_engine::map::report::StatKind;
use serde_json::{Value, json};

/// A chart's rows in the grid.
pub(super) fn chart_rows(
    app: &mut RustGisApp,
    ui: &mut Ui,
    layout: &Layout,
    li: usize,
    e: &Element,
    update: &dyn Fn(&mut RustGisApp, Value),
) {
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
        return;
    };
    let id = e.id;
    let drag_key = |field: &str| number_key(li, id, field);
    // The feature layers of the frame's map, and the fields of the chart's layer (#2487).
    let map = match layout.element(*frame).map(|f| &f.kind) {
        Some(ElementKind::MapFrame { map, .. }) => app.session.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(map)),
        _ => None,
    };
    let layers: Vec<String> =
        map.map(|m| m.layers.iter().filter(|l| !l.is_raster() && !l.graphics).map(|l| l.name.clone()).collect()).unwrap_or_default();
    let fields: Vec<(String, bool)> = map
        .and_then(|m| m.layer_by_name(layer))
        .map(|l| l.dataset.fields.iter().map(|f| (f.name.clone(), f.ty.is_numeric())).collect())
        .unwrap_or_default();
    let none = tl!("None");
    let named = |current: &Option<String>| {
        let mut items = vec![(none.to_string(), current.is_none(), json!(""))];
        items.extend(fields.iter().map(|(n, _)| (n.clone(), current.as_deref() == Some(n.as_str()), json!(n))));
        items
    };
    let input_label = ui.label(tl!("Layer"));
    let items: Vec<_> = layers.iter().map(|n| (n.clone(), n == layer, json!(n))).collect();
    if let Some(v) = choice(ui, ("chart_layer", id), input_label.id, layer, &items) {
        update(app, json!({"layer": v}));
    }
    ui.end_row();
    // A count reads no number, so it charts any field.
    let counts = group.is_some() && *statistic == StatKind::Count;
    let input_label = ui.label(tl!("Field"));
    let items: Vec<_> = fields.iter().filter(|(_, num)| counts || *num).map(|(n, _)| (n.clone(), n == field, json!(n))).collect();
    if let Some(v) = choice(ui, ("chart_field", id), input_label.id, field, &items) {
        update(app, json!({"field": v}));
    }
    ui.end_row();
    let input_label = ui.label(tl!("Group by"));
    if let Some(v) = choice(ui, ("chart_group", id), input_label.id, group.as_deref().unwrap_or(none), &named(group)) {
        update(app, json!({"group": v}));
    }
    ui.end_row();
    // Grouped, a bar is labelled with its value and measures a statistic;
    // otherwise it is a feature's, labelled by a field.
    if group.is_some() {
        let input_label = ui.label(tl!("Statistic"));
        let items: Vec<_> = [StatKind::Sum, StatKind::Count, StatKind::Mean, StatKind::Min, StatKind::Max]
            .into_iter()
            .map(|k| (tl!(k.label()).to_string(), k == *statistic, json!(k)))
            .collect();
        if let Some(v) = choice(ui, ("chart_statistic", id), input_label.id, tl!(statistic.label()), &items) {
            update(app, json!({"statistic": v}));
        }
    } else {
        let input_label = ui.label(tl!("Label"));
        if let Some(v) = choice(ui, ("chart_label", id), input_label.id, label.as_deref().unwrap_or(none), &named(label)) {
            update(app, json!({"label": v}));
        }
    }
    ui.end_row();
    // Grouped, summing or counting: stacked by another field (#2487 part 3b).
    if group.is_some() && matches!(statistic, StatKind::Sum | StatKind::Count) {
        let input_label = ui.label(tl!("Stack by"));
        // Only the fields it can stack by: not the one it groups by.
        let takes = |v: &Value| v.as_str().is_none_or(|f| f.is_empty() || chart_stack_refusal(group.as_deref(), *statistic, f).is_none());
        let items: Vec<_> = named(stack).into_iter().filter(|(_, _, v)| takes(v)).collect();
        if let Some(v) = choice(ui, ("chart_stack", id), input_label.id, stack.as_deref().unwrap_or(none), &items) {
            update(app, json!({"stack": v}));
        }
        ui.end_row();
    }
    let input_label = ui.label(tl!("Order"));
    let items: Vec<_> = ChartOrder::ALL.into_iter().map(|o| (tl!(o.label()).to_string(), o == *order, json!(o.code()))).collect();
    if let Some(v) = choice(ui, ("chart_order", id), input_label.id, tl!(order.label()), &items) {
        update(app, json!({"order": v}));
    }
    ui.end_row();
    let input_label = ui.label(tl!("Bars"));
    let mut n = *bars as u64;
    let drag = Dragged::lend(ui, drag_key("bars"), &mut n);
    let r = ui.add(egui::DragValue::new(&mut n).update_while_editing(false).range(1..=MAX_CHART_BARS as u64)).labelled_by(input_label.id);
    if drag.settled(ui, &r, &mut n) && n as usize != *bars {
        update(app, json!({"bars": n}));
    }
    ui.end_row();
    // The bars past the limit: left out (and said), or gathered in a last bar.
    ui.label(tl!("Past the limit"));
    ui.horizontal(|ui| {
        for (text, gather) in [("Leave out", false), ("Other bar", true)] {
            if ui.selectable_label(*other == gather, tl!(text)).clicked() && *other != gather {
                update(app, json!({"other": gather}));
            }
        }
    });
    ui.end_row();
    // How it draws its bars (#2487).
    let vertical = *orientation == Orientation::Vertical;
    if let Some(o) = orientation_row(ui, *orientation) {
        update(app, json!({"orientation": o}));
    }
    // Its own title is typed below; picking it starts from the title it is given.
    ui.label(tl!("Title"));
    let own = title.as_deref().is_some_and(|t| !t.is_empty());
    ui.horizontal(|ui| {
        let modes = [("Automatic", title.is_none()), ("None", title.as_deref() == Some("")), ("Own", own)];
        for (text, on) in modes {
            if ui.selectable_label(on, tl!(text)).clicked() && !on {
                // Its own title starts as the one it is given, cut to a title's 200 characters.
                let value = match text {
                    "Automatic" => Value::Null,
                    "None" => json!(""),
                    _ => json!(e.kind.chart_given_title().unwrap_or_default().chars().take(200).collect::<String>()),
                };
                // Its own title being typed is dropped, so the field losing its
                // focus, now or with the selection, doesn't bring it back.
                app.layout.typing.remove(&(li, id, "title"));
                update(app, json!({"title": value}));
            }
        }
    });
    ui.end_row();
    if let Some(t) = title.as_deref().filter(|_| own) {
        let input_label = ui.label(tl!("Title text"));
        if let Some(tt) = text_field(app, ui, (li, id, "title"), t, false, input_label.id) {
            update(app, json!({"title": tt}));
        }
        ui.end_row();
    }
    let input_label = ui.label(tl!("Values"));
    let mut on = *values;
    if ui.checkbox(&mut on, "").labelled_by(input_label.id).changed() {
        update(app, json!({"values": on}));
    }
    ui.end_row();
    if *values {
        let input_label = ui.label(tl!("Decimals"));
        let auto = tl!("Automatic");
        let mut items = vec![(auto.to_string(), decimals.is_none(), json!("auto"))];
        items.extend((0..=MAX_CHART_DECIMALS).map(|d| (d.to_string(), *decimals == Some(d), json!(d))));
        let current = decimals.map_or(auto.to_string(), |d| d.to_string());
        if let Some(v) = choice(ui, ("chart_decimals", id), input_label.id, &current, &items) {
            update(app, json!({"decimals": v}));
        }
        ui.end_row();
    }
    // Each bar in its features' colour on the map; the colour below for the rest (#2487).
    let input_label = ui.label(tl!("Color by class"));
    let mut on = *classes;
    if ui.checkbox(&mut on, "").labelled_by(input_label.id).changed() {
        update(app, json!({"classes": on}));
    }
    ui.end_row();
    ui.label(tl!("Bar color"));
    let mut c = crate::theme::c32(e.kind.chart_look().unwrap_or_default().bar_color);
    let resp = crate::widgets::color_button(ui, &mut c);
    if resp.changed() {
        crate::widgets::one_step_per_press(app, &resp);
        update(app, json!({"barColor": crate::theme::rgba(c).hex()}));
    }
    ui.end_row();
    // Facing another chart (#2487 part 4): its labels left out, its bars the other
    // way round, on a scale it shares.
    let input_label = ui.label(tl!("Hide labels"));
    let mut on = *hide_labels;
    if ui.checkbox(&mut on, "").labelled_by(input_label.id).changed() {
        update(app, json!({"hideLabels": on}));
    }
    ui.end_row();
    let input_label = ui.label(tl!("Mirror"));
    let mut on = *mirror;
    if ui.checkbox(&mut on, "").labelled_by(input_label.id).changed() {
        update(app, json!({"mirror": on}));
    }
    ui.end_row();
    let input_label = ui.label(tl!("Maximum"));
    ui.horizontal(|ui| {
        // As the chart reads it: a maximum not above 0, from a file, isn't used.
        let fixed = rustgis_engine::map::layout::chart_max(*max);
        let mut auto = fixed.is_none();
        if ui.checkbox(&mut auto, tl!("Automatic")).labelled_by(input_label.id).changed() {
            // Fixed, it starts from where its bars reach, so none runs past it yet.
            let reach = || {
                let (m, env) = rustgis_engine::render::layout::frame_page_env(layout, &app.session.project.maps, *frame)?;
                let data = e.kind.chart_data_of(m.layer_by_name(layer)?, &env)?;
                Some(rustgis_engine::render::layout::chart_span(&data).1).filter(|v| *v > 0.0)
            };
            let to = if auto { json!("auto") } else { json!(reach().unwrap_or(100.0)) };
            update(app, json!({"max": to}));
        }
        if let Some(m) = fixed {
            let mut v = m;
            let drag = Dragged::lend(ui, drag_key("chart_max"), &mut v);
            let r = ui
                .add(egui::DragValue::new(&mut v).update_while_editing(false).range(1e-6..=1e15).speed(m * 0.01))
                .labelled_by(input_label.id);
            if drag.settled(ui, &r, &mut v) && v != m {
                update(app, json!({"max": v}));
            }
        }
    });
    ui.end_row();
    // A line round each bar (#2487 part 4b): its colour and width once drawn.
    let input_label = ui.label(tl!("Bar outline"));
    ui.horizontal(|ui| {
        let shown = rustgis_engine::map::layout::chart_outline(*bar_outline);
        let mut on = shown.is_some();
        if ui.checkbox(&mut on, "").labelled_by(input_label.id).changed() {
            update(app, json!({"barOutline": if on { json!({}) } else { Value::Null }}));
        }
        if let Some(o) = shown {
            let mut c = crate::theme::c32(o.color);
            let resp = crate::widgets::color_button(ui, &mut c);
            if resp.changed() {
                crate::widgets::one_step_per_press(app, &resp);
                update(app, json!({"barOutline": {"color": crate::theme::rgba(c).hex()}}));
            }
            let mut w = o.width;
            let drag = Dragged::lend(ui, drag_key("barOutline"), &mut w);
            let max = rustgis_engine::map::layout::MAX_CHART_OUTLINE_WIDTH;
            let r = ui
                .add(
                    egui::DragValue::new(&mut w)
                        .update_while_editing(false)
                        // From 0, as the commands take it: any saved width
                        // shows as it is, and 0 takes the outline off.
                        .range(0.0..=max)
                        .speed(0.05)
                        .max_decimals(2)
                        .suffix(" pt"),
                )
                .labelled_by(input_label.id);
            // A width typed and left as the box is turned off in the same
            // frame is let go: sent after the null, it would bring the outline
            // back. `settled` still runs, so the edit is cleared.
            if drag.settled(ui, &r, &mut w) && on && (w - o.width).abs() > 1e-6 {
                update(app, json!({"barOutline": {"width": w}}));
            }
        }
    });
    ui.end_row();
    // Columns have their labels centred under them.
    if !vertical && !*hide_labels {
        ui.label(tl!("Labels"));
        ui.horizontal(|ui| {
            let right = *label_align == TextAlign::Right;
            for (text, v) in [("Left", false), ("By the bars", true)] {
                if ui.selectable_label(right == v, tl!(text)).clicked() && right != v {
                    update(app, json!({"labelAlign": if v { "right" } else { "left" }}));
                }
            }
        });
        ui.end_row();
    }
}
