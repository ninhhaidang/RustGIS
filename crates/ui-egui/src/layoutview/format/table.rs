//! A table's Format rows.

use super::*;
use crate::RustGisApp;
use crate::widgets::Dragged;
use egui::Ui;
use rustgis_engine::map::layout::{Element, ElementKind};
use serde_json::{Value, json};

/// A table's rows in the grid.
pub(super) fn table_rows(app: &mut RustGisApp, ui: &mut Ui, li: usize, e: &Element, update: &dyn Fn(&mut RustGisApp, Value)) {
    let ElementKind::Table { rows, .. } = &e.kind else { return };
    let id = e.id;
    let drag_key = |field: &str| number_key(li, id, field);
    let input_label = ui.label(tl!("Rows"));
    let mut n = *rows as u64;
    let drag = Dragged::lend(ui, drag_key("rows"), &mut n);
    let r = ui.add(egui::DragValue::new(&mut n).update_while_editing(false).range(1..=500)).labelled_by(input_label.id);
    if drag.settled(ui, &r, &mut n) && n as usize != *rows {
        update(app, json!({"rows": n}));
    }
    ui.end_row();
}
