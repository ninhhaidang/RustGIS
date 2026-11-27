//! A north arrow's Format rows.

use crate::RustGisApp;
use egui::Ui;
use rustgis_engine::map::layout::{ArrowStyle, Element, ElementKind};
use serde_json::{Value, json};

/// A north arrow's rows in the grid.
pub(super) fn north_arrow_rows(app: &mut RustGisApp, ui: &mut Ui, e: &Element, update: &dyn Fn(&mut RustGisApp, Value)) {
    let ElementKind::NorthArrow { style, fill, outline, .. } = &e.kind else { return };
    let id = e.id;
    let input_label = ui.label(tl!("Style"));
    egui::ComboBox::from_id_salt(("arrow_style", id))
        .selected_text(tl!(style.label()))
        .show_ui(ui, |ui| {
            for v in ArrowStyle::ALL {
                if ui.selectable_label(*style == v, tl!(v.label())).clicked() && *style != v {
                    update(app, json!({"style": v.code()}));
                }
            }
        })
        .response
        .labelled_by(input_label.id);
    ui.end_row();
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
}
