//! A legend's Format rows.

use super::*;
use crate::RustGisApp;
use crate::widgets::Dragged;
use egui::Ui;
use rustgis_engine::map::layout::{Element, ElementKind, Layout, MAX_LEGEND_COLUMNS};
use serde_json::{Value, json};

/// A legend's rows in the grid.
pub(super) fn legend_property_rows(app: &mut RustGisApp, ui: &mut Ui, li: usize, e: &Element, update: &dyn Fn(&mut RustGisApp, Value)) {
    let ElementKind::Legend { title, all_classes, hide_other, show_no_data, columns, text_size, .. } = &e.kind else { return };
    let id = e.id;
    let drag_key = |field: &str| number_key(li, id, field);
    let input_label = ui.label(tl!("Title"));
    if let Some(tt) = text_field(app, ui, (li, id, "title"), title, false, input_label.id) {
        update(app, json!({"title": tt}));
    }
    ui.end_row();
    // Off, the legend lists only what the frame draws (#207).
    let input_label = ui.label(tl!("All classes"));
    let mut all = *all_classes;
    if ui.checkbox(&mut all, "").labelled_by(input_label.id).changed() {
        update(app, json!({"allClasses": all}));
    }
    ui.end_row();
    let input_label = ui.label(tl!("Hide other values"));
    let mut hide = *hide_other;
    if ui.checkbox(&mut hide, "").labelled_by(input_label.id).changed() {
        update(app, json!({"hideOther": hide}));
    }
    ui.end_row();
    let input_label = ui.label(tl!("Show no data"));
    let mut show = *show_no_data;
    if ui.checkbox(&mut show, "").labelled_by(input_label.id).changed() {
        update(app, json!({"showNoData": show}));
    }
    ui.end_row();
    let input_label = ui.label(tl!("Columns"));
    let mut n = *columns as u64;
    let drag = Dragged::lend(ui, drag_key("columns"), &mut n);
    let r =
        ui.add(egui::DragValue::new(&mut n).update_while_editing(false).range(1..=MAX_LEGEND_COLUMNS as u64)).labelled_by(input_label.id);
    if drag.settled(ui, &r, &mut n) && n as usize != *columns {
        update(app, json!({"columns": n}));
    }
    ui.end_row();
    // Its names' and labels' points; the title, rows and swatches follow (#2581).
    if let Some(pt) = text_size_row(ui, tl!("Text size"), drag_key("textSize"), *text_size) {
        update(app, json!({"textSize": pt}));
    }
}

/// The layers a legend lists, below the grid.
pub(super) fn legend_layers(app: &mut RustGisApp, ui: &mut Ui, layout: &Layout, e: &Element, update: &dyn Fn(&mut RustGisApp, Value)) {
    let id = e.id;
    // The layers a legend lists: a checkbox for each vector layer of its frame's map, each raster
    // drawn by classes (#1679) and each WMS or WMTS layer with a legend, ticked when listed. Names
    // gone from the map drop out of the list as it's written back.
    if let ElementKind::Legend { frame, exclude, .. } = &e.kind {
        let map = layout.element(*frame).and_then(|f| match &f.kind {
            ElementKind::MapFrame { map, .. } => app.session.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(map)),
            _ => None,
        });
        let names: Vec<String> = map
            .map(|m| {
                // Once each: layers may share a name, and the legend leaves names out.
                let mut names: Vec<String> = Vec::new();
                let listed = m.layers.iter().filter(|l| rustgis_engine::render::layout::legend_lists(l)).map(|l| &l.name);
                for name in listed.chain(m.tiles.iter().filter(|t| !t.legends.is_empty()).map(|t| &t.name)) {
                    if !names.iter().any(|n| n.eq_ignore_ascii_case(name)) {
                        names.push(name.clone());
                    }
                }
                names
            })
            .unwrap_or_default();
        // The names left out that the map still has, those not listed here (a raster's on a ramp)
        // too.
        let kept: Vec<String> = exclude
            .iter()
            .filter(|x| {
                map.is_some_and(|m| {
                    m.layers.iter().map(|l| &l.name).chain(m.tiles.iter().map(|t| &t.name)).any(|n| n.eq_ignore_ascii_case(x))
                })
            })
            .cloned()
            .collect();
        if !names.is_empty() {
            ui.add_space(4.0);
            egui::CollapsingHeader::new(tl!("Layers")).id_salt(("legend_layers", id)).default_open(!exclude.is_empty()).show(ui, |ui| {
                let left_out = |n: &str| exclude.iter().any(|x| x.eq_ignore_ascii_case(n));
                for n in &names {
                    let mut listed = !left_out(n);
                    if ui.checkbox(&mut listed, n.as_str()).on_hover_text(tl!("List this layer in the legend")).changed() {
                        let mut next: Vec<&String> = kept.iter().filter(|x| !x.eq_ignore_ascii_case(n)).collect();
                        if !listed {
                            next.push(n);
                        }
                        update(app, json!({"exclude": next}));
                    }
                }
            });
        }
    }
}
