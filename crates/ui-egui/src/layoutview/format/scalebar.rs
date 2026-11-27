//! A scale bar's Format rows.

use super::*;
use crate::RustGisApp;
use crate::widgets::Dragged;
use egui::Ui;
use rustgis_engine::map::layout::{
    Element, ElementKind, Layout, MAX_SCALE_BEFORE_ZERO, MAX_SCALE_DECIMALS, MAX_SCALE_DIVISIONS, MAX_SCALE_SUBDIVISIONS, ScaleBarFit,
    ScaleBarStyle, ScaleLabelPosition, ScaleUnits, UnitLabelPosition,
};
use serde_json::{Value, json};

/// A scale bar's rows in the grid.
pub(super) fn scale_bar_rows(
    app: &mut RustGisApp,
    ui: &mut Ui,
    layout: &Layout,
    li: usize,
    e: &Element,
    update: &dyn Fn(&mut RustGisApp, Value),
) {
    let ElementKind::ScaleBar {
        units,
        divisions,
        style,
        color,
        fill,
        text_size,
        subdivisions,
        divisions_before_zero,
        label_position,
        unit_label,
        unit_label_position,
        decimals,
        fit,
        ..
    } = &e.kind
    else {
        return;
    };
    let id = e.id;
    let drag_key = |field: &str| number_key(li, id, field);
    let input_label = ui.label(tl!("Style"));
    egui::ComboBox::from_id_salt(("scale_style", id))
        .selected_text(tl!(style.label()))
        .show_ui(ui, |ui| {
            for v in ScaleBarStyle::ALL {
                if ui.selectable_label(*style == v, tl!(v.label())).clicked() && *style != v {
                    update(app, json!({"style": v.code()}));
                }
            }
        })
        .response
        .labelled_by(input_label.id);
    ui.end_row();
    let input_label = ui.label(tl!("Units"));
    egui::ComboBox::from_id_salt(("scale_units", id))
        .selected_text(tl!(units.label()))
        .show_ui(ui, |ui| {
            for v in ScaleUnits::ALL {
                if ui.selectable_label(*units == v, tl!(v.label())).clicked() && *units != v {
                    update(app, json!({"units": v.code()}));
                }
            }
        })
        .response
        .labelled_by(input_label.id);
    ui.end_row();
    let input_label = ui.label(tl!("Divisions"));
    let mut n = *divisions as u64;
    let drag = Dragged::lend(ui, drag_key("divisions"), &mut n);
    let r = ui
        .add(
            egui::DragValue::new(&mut n)
                .update_while_editing(false)
                .range(1..=MAX_SCALE_DIVISIONS.saturating_sub(*divisions_before_zero).max(1) as u64),
        )
        .labelled_by(input_label.id);
    if drag.settled(ui, &r, &mut n) && n as usize != *divisions {
        update(app, json!({"divisions": n}));
    }
    ui.end_row();
    // Its labels' points; their room follows, the bar keeping its size (#2581).
    if let Some(pt) = text_size_row(ui, tl!("Text size"), drag_key("textSize"), *text_size) {
        update(app, json!({"textSize": pt}));
    }
    // Each division split into parts, and divisions left of zero.
    for (label, key, current, max) in [
        (tl!("Subdivisions"), "subdivisions", *subdivisions, MAX_SCALE_SUBDIVISIONS),
        (
            tl!("Divisions before zero"),
            "divisionsBeforeZero",
            *divisions_before_zero,
            MAX_SCALE_BEFORE_ZERO.min(MAX_SCALE_DIVISIONS.saturating_sub(*divisions)),
        ),
    ] {
        let input_label = ui.label(label);
        let mut n = current as u64;
        let drag = Dragged::lend(ui, drag_key(key), &mut n);
        let r = ui.add(egui::DragValue::new(&mut n).update_while_editing(false).range(0..=max as u64)).labelled_by(input_label.id);
        let settled = drag.settled(ui, &r, &mut n);
        // One subdivision is none: the bar keeps 0, so 1 changes nothing.
        if key == "subdivisions" && n == 1 {
            n = 0;
        }
        if settled && n as usize != current {
            update(app, json!({key: n}));
        }
        ui.end_row();
    }
    // The numbers' decimal places, as a chart's: automatic, as each label needs, or fixed.
    let input_label = ui.label(tl!("Decimals"));
    let auto = tl!("Automatic");
    let mut items = vec![(auto.to_string(), decimals.is_none(), json!("auto"))];
    items.extend((0..=MAX_SCALE_DECIMALS as u8).map(|d| (d.to_string(), *decimals == Some(d), json!(d))));
    let current = decimals.map_or(auto.to_string(), |d| d.to_string());
    if let Some(v) = choice(ui, ("scale_decimals", id), input_label.id, &current, &items) {
        update(app, json!({"decimals": v}));
    }
    ui.end_row();
    let input_label = ui.label(tl!("Label position"));
    egui::ComboBox::from_id_salt(("scale_labels", id))
        .selected_text(tl!(label_position.label()))
        .show_ui(ui, |ui| {
            for v in ScaleLabelPosition::ALL {
                if ui.selectable_label(*label_position == v, tl!(v.label())).clicked() && *label_position != v {
                    update(app, json!({"labelPosition": v.code()}));
                }
            }
        })
        .response
        .labelled_by(input_label.id);
    ui.end_row();
    let input_label = ui.label(tl!("Fit"));
    egui::ComboBox::from_id_salt(("scale_fit", id))
        .selected_text(tl!(fit.label()))
        .show_ui(ui, |ui| {
            for v in ScaleBarFit::ALL {
                if ui.selectable_label(*fit == v, tl!(v.label())).clicked() && *fit != v {
                    update(app, json!({"fit": v.code()}));
                }
            }
        })
        .response
        .labelled_by(input_label.id);
    ui.end_row();
    // Unticked, the bar prints no unit; ticked again, the label typed before
    // it was hidden (kept for the session), else the unit's abbreviation.
    let input_label = ui.label(tl!("Show unit label"));
    let shows_unit = unit_label.as_deref() != Some("");
    let mut on = shows_unit;
    if ui.checkbox(&mut on, "").labelled_by(input_label.id).changed() {
        // Kept per project and per layout name too, so another project's
        // bar, or another layout's once a layout before it is deleted, never
        // gets this one's text; a renamed layout forgets it.
        let kept = egui::Id::unique(("scale_unit_label_kept", app.session.opened, li, layout.name.as_str(), id));
        // The label being typed is dropped, so the field losing its focus
        // doesn't write it back; hidden, it is the text kept.
        let draft = app.layout.typing.remove(&(li, id, "unitLabel"));
        if on {
            // Read, not taken: a show that is undone can be shown again.
            let typed = ui.ctx().data(|d| d.get_temp::<String>(kept)).filter(|t| !t.is_empty());
            update(app, json!({"unitLabel": typed.map_or(Value::Null, Value::String)}));
        } else {
            // Every hide keeps the text hidden now, and forgets an older one
            // once the label is back to the abbreviation.
            match draft.or_else(|| unit_label.clone()).filter(|t| !t.is_empty()) {
                Some(text) => {
                    ui.ctx().data_mut(|d| d.insert_temp(kept, text));
                }
                None => {
                    ui.ctx().data_mut(|d| d.remove_temp::<String>(kept));
                }
            }
            update(app, json!({"unitLabel": ""}));
        }
    }
    ui.end_row();
    if shows_unit {
        let input_label = ui.label(tl!("Unit label"));
        let current = unit_label.as_deref().unwrap_or("");
        // Left empty, the bar prints the unit's abbreviation; an automatic
        // unit's depends on the frame's scale.
        let hint = match *units {
            ScaleUnits::Auto => tl!("Unit's abbreviation"),
            fixed => fixed.resolve(0.0).1,
        };
        if let Some(text) = hinted_text_field(app, ui, (li, id, "unitLabel"), current, false, Some(hint), input_label.id) {
            // Emptied, the field gives the unit's abbreviation back.
            update(app, json!({"unitLabel": if text.is_empty() { Value::Null } else { json!(text) }}));
        }
        ui.end_row();
        let input_label = ui.label(tl!("Unit label position"));
        egui::ComboBox::from_id_salt(("scale_unit_label", id))
            .selected_text(tl!(unit_label_position.label()))
            .show_ui(ui, |ui| {
                for v in UnitLabelPosition::ALL {
                    if ui.selectable_label(*unit_label_position == v, tl!(v.label())).clicked() && *unit_label_position != v {
                        update(app, json!({"unitLabelPosition": v.code()}));
                    }
                }
            })
            .response
            .labelled_by(input_label.id);
        ui.end_row();
    }
    // The fill is the alternating bar's light divisions: the other styles draw none.
    let colors = [("Color", "color", *color), ("Fill", "fill", *fill)];
    let shown = if *style == ScaleBarStyle::Alternating { 2 } else { 1 };
    for (label, key, current) in colors.into_iter().take(shown) {
        ui.label(tl!(label));
        let mut c = crate::theme::c32(current);
        let resp = crate::widgets::color_button(ui, &mut c);
        if resp.changed() {
            crate::widgets::one_step_per_press(app, &resp);
            update(app, json!({key: crate::theme::rgba(c).hex()}));
        }
        ui.end_row();
    }
}
