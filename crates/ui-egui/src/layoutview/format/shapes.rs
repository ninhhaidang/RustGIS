//! A rectangle's, ellipse's and line's Format rows.

use super::*;
use crate::RustGisApp;
use crate::widgets::Dragged;
use egui::Ui;
use rustgis_engine::map::layout::{Element, ElementKind, LineHead, line_angle};
use serde_json::{Value, json};

/// A rectangle's or ellipse's rows in the grid.
pub(super) fn shape_rows(app: &mut RustGisApp, ui: &mut Ui, li: usize, e: &Element, update: &dyn Fn(&mut RustGisApp, Value)) {
    let (ElementKind::Rectangle { fill, outline, width, dash, .. } | ElementKind::Ellipse { fill, outline, width, dash }) = &e.kind else {
        return;
    };
    let id = e.id;
    let drag_key = |field: &str| number_key(li, id, field);
    // A fill with no opacity leaves only the outline; an outline 0 wide, only the fill.
    for (label, key, current) in [("Fill", "fill", *fill), ("Outline", "outline", *outline)] {
        ui.label(tl!(label));
        let mut c = crate::theme::c32(current);
        let resp = crate::widgets::color_button(ui, &mut c);
        if resp.changed() {
            crate::widgets::one_step_per_press(app, &resp);
            update(app, json!({key: crate::theme::rgba(c).hex()}));
        }
        ui.end_row();
    }
    let input_label = ui.label(tl!("Outline width"));
    let mut wd = *width;
    let drag = Dragged::lend(ui, drag_key("width"), &mut wd);
    let r = ui
        .add(egui::DragValue::new(&mut wd).update_while_editing(false).range(0.0..=50.0).speed(0.1).max_decimals(1).suffix(" pt"))
        .labelled_by(input_label.id);
    if drag.settled(ui, &r, &mut wd) && (wd - width).abs() > 1e-6 {
        update(app, json!({"width": wd}));
    }
    ui.end_row();
    let input_label = ui.label(tl!("Dash"));
    if let Some(d) = dash_picked(ui, ("layout_dash", id), *dash, *width, input_label.id) {
        update(app, json!({"dash": d}));
    }
    ui.end_row();
}

/// A line's rows in the grid.
pub(super) fn line_rows(app: &mut RustGisApp, ui: &mut Ui, li: usize, e: &Element, update: &dyn Fn(&mut RustGisApp, Value)) {
    let ElementKind::Line { color, width, dir, head, dash } = &e.kind else { return };
    let id = e.id;
    let drag_key = |field: &str| number_key(li, id, field);
    ui.label(tl!("Color"));
    let mut c = crate::theme::c32(*color);
    let resp = crate::widgets::color_button(ui, &mut c);
    if resp.changed() {
        crate::widgets::one_step_per_press(app, &resp);
        update(app, json!({"color": crate::theme::rgba(c).hex()}));
    }
    ui.end_row();
    let input_label = ui.label(tl!("Width"));
    let mut wd = *width;
    let drag = Dragged::lend(ui, drag_key("width"), &mut wd);
    let r = ui
        .add(egui::DragValue::new(&mut wd).update_while_editing(false).range(0.1..=50.0).speed(0.1).max_decimals(1).suffix(" pt"))
        .labelled_by(input_label.id);
    if drag.settled(ui, &r, &mut wd) && (wd - width).abs() > 1e-6 {
        update(app, json!({"width": wd}));
    }
    ui.end_row();
    // Only the shaft is dashed: arrowheads stay solid.
    let input_label = ui.label(tl!("Dash"));
    if let Some(d) = dash_picked(ui, ("layout_dash", id), *dash, *width, input_label.id) {
        update(app, json!({"dash": d}));
    }
    ui.end_row();
    let input_label = ui.label(tl!("Arrowheads"));
    egui::ComboBox::from_id_salt(("line_head", id))
        .selected_text(tl!(head.label()))
        .show_ui(ui, |ui| {
            for h in LineHead::ALL {
                if ui.selectable_label(h == *head, tl!(h.label())).clicked() {
                    update(app, json!({"head": h.code()}));
                }
            }
        })
        .response
        .labelled_by(input_label.id);
    ui.end_row();
    // Anticlockwise from pointing right; the rotation handle above it turns it too.
    let input_label = ui.label(tl!("Angle"));
    let current = line_angle((e.x, e.y, e.w, e.h), *dir, *head);
    let mut a = current;
    let drag = Dragged::lend(ui, drag_key("angle"), &mut a);
    let r = ui
        .add(egui::DragValue::new(&mut a).update_while_editing(false).range(-180.0..=180.0).speed(1.0).max_decimals(1).suffix("°"))
        .labelled_by(input_label.id);
    if drag.settled(ui, &r, &mut a) && (a - current).abs() > 1e-6 {
        update(app, json!({"angle": a}));
    }
    ui.end_row();
}
