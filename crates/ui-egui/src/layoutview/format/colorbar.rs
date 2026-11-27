//! A colorbar's Format rows.

use super::*;
use crate::RustGisApp;
use crate::widgets::Dragged;
use egui::Ui;
use rustgis_engine::map::layout::{Element, ElementKind, Layout, MAX_COLORBAR_TICKS, Orientation, TickSide};
use serde_json::{Value, json};

/// A colorbar's rows in the grid.
pub(super) fn colorbar_rows(
    app: &mut RustGisApp,
    ui: &mut Ui,
    layout: &Layout,
    li: usize,
    e: &Element,
    update: &dyn Fn(&mut RustGisApp, Value),
) {
    let ElementKind::Colorbar { frame, layer, title, orientation, ticks, tick_side, reverse, .. } = &e.kind else { return };
    let id = e.id;
    let drag_key = |field: &str| number_key(li, id, field);
    let input_label = ui.label(tl!("Title"));
    if let Some(tt) = text_field(app, ui, (li, id, "title"), title, false, input_label.id) {
        update(app, json!({"title": tt}));
    }
    ui.end_row();
    // The rasters of the frame's map; none picked follows the top visible one.
    let input_label = ui.label(tl!("Layer"));
    let rasters: Vec<String> = match layout.element(*frame).map(|f| &f.kind) {
        Some(ElementKind::MapFrame { map, .. }) => app
            .session
            .project
            .maps
            .iter()
            .find(|m| m.name.eq_ignore_ascii_case(map))
            .map(|m| m.layers.iter().filter(|l| l.is_raster()).map(|l| l.name.clone()).collect())
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    let top = tl!("Top raster with a colour scale, not drawn by classes");
    egui::ComboBox::from_id_salt(("colorbar_layer", id))
        .selected_text(layer.as_deref().unwrap_or(top))
        .show_ui(ui, |ui| {
            if ui.selectable_label(layer.is_none(), top).clicked() {
                update(app, json!({"layer": ""}));
            }
            for n in rasters {
                if ui.selectable_label(layer.as_deref() == Some(n.as_str()), n.as_str()).clicked() {
                    update(app, json!({"layer": n}));
                }
            }
        })
        .response
        .labelled_by(input_label.id);
    ui.end_row();
    let vertical = *orientation == Orientation::Vertical;
    if let Some(o) = orientation_row(ui, *orientation) {
        update(app, json!({"orientation": o}));
    }
    ui.label(tl!("Labels"));
    ui.horizontal(|ui| {
        let sides = if vertical { [("Right", "right"), ("Left", "left")] } else { [("Below", "below"), ("Above", "above")] };
        for ((label, code), side) in sides.into_iter().zip([TickSide::End, TickSide::Start]) {
            if ui.selectable_label(*tick_side == side, tl!(label)).clicked() && *tick_side != side {
                update(app, json!({"tickSide": code}));
            }
        }
    });
    ui.end_row();
    let input_label = ui.label(tl!("Ticks"));
    let mut n = *ticks as u64;
    let drag = Dragged::lend(ui, drag_key("ticks"), &mut n);
    let r =
        ui.add(egui::DragValue::new(&mut n).update_while_editing(false).range(2..=MAX_COLORBAR_TICKS as u64)).labelled_by(input_label.id);
    if drag.settled(ui, &r, &mut n) && n as usize != *ticks {
        update(app, json!({"ticks": n}));
    }
    ui.end_row();
    let input_label = ui.label(tl!("Reverse"));
    let mut rev = *reverse;
    if ui.checkbox(&mut rev, "").labelled_by(input_label.id).changed() {
        update(app, json!({"reverse": rev}));
    }
    ui.end_row();
}
