//! A map frame's Format rows, its grid, extent indicator and border.

use super::*;
use crate::RustGisApp;
use crate::widgets::Dragged;
use egui::{Ui, vec2};
use rustgis_engine::map::layout::{
    CoordFormat, Element, ElementKind, Grid, GridKind, GridStyle, Layout, MAX_FRAME_SCALE, MAX_GRID_WIDTH, MAX_TICK_MM, MIN_FRAME_SCALE,
    Orientation,
};
use serde_json::{Value, json};

/// A map frame's grid options; `set` sends the changed ones (`false` removes the grid).
fn grid_properties(app: &mut RustGisApp, ui: &mut Ui, grid: Option<&Grid>, id: u32, set: &dyn Fn(&mut RustGisApp, Value)) {
    let mut on = grid.is_some();
    if ui.checkbox(&mut on, tl!("Show grid")).changed() {
        set(app, json!(on));
    }
    let Some(g) = grid else { return };
    // Each number kept across a drag until it is let go (#607).
    let key = |field: &str| egui::Id::unique(("grid_drag", id, field));
    let measured = g.kind == GridKind::Measured;
    egui::Grid::new(("grid_props", id)).num_columns(2).spacing(vec2(8.0, 6.0)).show(ui, |ui| {
        // Latitudes and longitudes, or eastings and northings (ArcGIS Pro's measured grid).
        let input_label = ui.label(tl!("Type"));
        egui::ComboBox::from_id_salt(("grid_kind", id))
            .selected_text(tl!(g.kind.label()))
            .show_ui(ui, |ui| {
                for v in GridKind::ALL {
                    if ui.selectable_label(g.kind == v, tl!(v.label())).clicked() && g.kind != v {
                        set(app, json!({"type": v.code()}));
                    }
                }
            })
            .response
            .labelled_by(input_label.id);
        ui.end_row();
        if measured {
            crs_row(app, ui, g, id, set);
        }
        let input_label = ui.label(tl!("Interval"));
        let mut step = g.step;
        let drag = Dragged::lend(ui, key("step"), &mut step);
        let (most, speed) = if measured {
            (rustgis_engine::map::layout::MAX_MEASURED_STEP, 10.0)
        } else {
            (rustgis_engine::map::layout::MAX_GRATICULE_STEP, 0.05)
        };
        let r = ui
            .add(
                egui::DragValue::new(&mut step)
                    .update_while_editing(false)
                    .range(0.0..=most)
                    .speed(speed)
                    .max_decimals(4)
                    .custom_formatter(|v, _| {
                        if v <= 0.0 {
                            tl!("Auto").to_string()
                        } else if measured {
                            // (The system's units: metres for UTM.)
                            format!("{v}")
                        } else {
                            format!("{v}°")
                        }
                    }),
            )
            .on_hover_text(if measured { tl!("In the coordinate system's units: metres for UTM.") } else { tl!("In degrees.") })
            .labelled_by(input_label.id);
        if drag.settled(ui, &r, &mut step) && (step - g.step).abs() > 1e-9 {
            set(app, json!({"step": step}));
        }
        ui.end_row();
        let input_label = ui.label(tl!("Style"));
        egui::ComboBox::from_id_salt(("grid_style", id))
            .selected_text(tl!(g.style.label()))
            .show_ui(ui, |ui| {
                for v in GridStyle::ALL {
                    if ui.selectable_label(g.style == v, tl!(v.label())).clicked() && g.style != v {
                        set(app, json!({"style": v.code()}));
                    }
                }
            })
            .response
            .labelled_by(input_label.id);
        ui.end_row();
        ui.label(tl!("Color"));
        let mut c = crate::theme::c32(g.color);
        let resp = crate::widgets::color_button(ui, &mut c);
        if resp.changed() {
            crate::widgets::one_step_per_press(app, &resp);
            set(app, json!({"color": crate::theme::rgba(c).hex()}));
        }
        ui.end_row();
        let input_label = ui.label(tl!("Line width"));
        let mut w = g.width;
        let drag = Dragged::lend(ui, key("width"), &mut w);
        let r = ui
            .add(
                egui::DragValue::new(&mut w)
                    .update_while_editing(false)
                    .range(0.0..=MAX_GRID_WIDTH)
                    .speed(0.05)
                    .max_decimals(2)
                    .suffix(" pt"),
            )
            .labelled_by(input_label.id);
        if drag.settled(ui, &r, &mut w) && (w - g.width).abs() > 1e-9 {
            set(app, json!({"width": w}));
        }
        ui.end_row();
        if g.style.ticks() {
            let input_label = ui.label(tl!("Tick length"));
            let mut t = g.tick_length;
            let drag = Dragged::lend(ui, key("tickLength"), &mut t);
            let r = ui
                .add(
                    egui::DragValue::new(&mut t)
                        .update_while_editing(false)
                        .range(0.0..=MAX_TICK_MM)
                        .speed(0.1)
                        .max_decimals(1)
                        .suffix(" mm"),
                )
                .labelled_by(input_label.id);
            if drag.settled(ui, &r, &mut t) && (t - g.tick_length).abs() > 1e-9 {
                set(app, json!({"tickLength": t}));
            }
            ui.end_row();
        }
        let input_label = ui.label(tl!("Labels"));
        let mut labels = g.labels;
        if ui.checkbox(&mut labels, "").labelled_by(input_label.id).changed() {
            set(app, json!({"labels": labels}));
        }
        ui.end_row();
        if !g.labels {
            return;
        }
        // Its labels' points; their room from the edge and MGRS squares' names follow (#2581).
        if let Some(pt) = text_size_row(ui, tl!("Label size"), key("labelSize"), g.label_size) {
            set(app, json!({"labelSize": pt}));
        }
        if measured {
            let input_label = ui.label(tl!("MGRS labels"));
            let mut mgrs = g.mgrs;
            if ui
                .checkbox(&mut mgrs, "")
                .labelled_by(input_label.id)
                .on_hover_text(tl!(
                    "Each 100 km square named with its grid zone designation (32T MT), and the lines by their kilometres' last two digits. The grid is in a WGS 84 UTM zone: the one given, else the map's, else the one at the frame's centre."
                ))
                .changed()
            {
                set(app, json!({"mgrs": mgrs}));
            }
            ui.end_row();
            // (MGRS labels are short already.)
            if !g.mgrs {
                let input_label = ui.label(tl!("Short labels"));
                let mut short = g.short;
                if ui
                    .checkbox(&mut short, "")
                    .labelled_by(input_label.id)
                    .on_hover_text(tl!("The kilometres' last two digits, as topographic maps print them: 43 for 543000."))
                    .changed()
                {
                    set(app, json!({"short": short}));
                }
                ui.end_row();
            }
        } else {
            // Degrees: decimal or degrees, minutes and seconds, with hemisphere letters or signs.
            ui.label(tl!("Format"));
            ui.horizontal(|ui| {
                for (label, v, code) in [("Decimal", CoordFormat::Decimal, "decimal"), ("DMS", CoordFormat::Dms, "dms")] {
                    if ui.selectable_label(g.format == v, tl!(label)).clicked() && g.format != v {
                        set(app, json!({"format": code}));
                    }
                }
            });
            ui.end_row();
            let input_label = ui.label(tl!("N/S/E/W"));
            let mut hemi = g.hemisphere;
            if ui
                .checkbox(&mut hemi, "")
                .labelled_by(input_label.id)
                .on_hover_text(tl!("Off, west and south are negative: -112.5°."))
                .changed()
            {
                set(app, json!({"hemisphere": hemi}));
            }
            ui.end_row();
        }
        // Two rows, so the four edges fit the panel's width.
        let e = g.edges;
        let all = [("Top", "top", e.top), ("Bottom", "bottom", e.bottom), ("Left", "left", e.left), ("Right", "right", e.right)];
        for (row, label) in [(0, tl!("Edges")), (2, "")] {
            ui.label(label);
            ui.horizontal(|ui| {
                for (i, (label, _, shown)) in all.iter().enumerate().skip(row).take(2) {
                    if ui.selectable_label(*shown, tl!(label)).clicked() {
                        let names: Vec<&str> = all
                            .iter()
                            .enumerate()
                            .filter(|(j, (_, _, s))| if *j == i { !*s } else { *s })
                            .map(|(_, (_, n, _))| *n)
                            .collect();
                        set(app, json!({"edges": names}));
                    }
                }
            });
            ui.end_row();
        }
        ui.label(tl!("Side labels"));
        ui.horizontal(|ui| {
            let vertical = g.label_orientation == Orientation::Vertical;
            for (label, v) in [("Horizontal", false), ("Vertical", true)] {
                if ui.selectable_label(vertical == v, tl!(label)).clicked() && vertical != v {
                    set(app, json!({"labelOrientation": if v { "vertical" } else { "horizontal" }}));
                }
            }
        });
        ui.end_row();
    });
}

/// A measured grid's coordinate system, typed (EPSG:25832, WKT or a PROJ string) and set when the
/// field is left; empty is the map's when projected, else the UTM zone at the frame's centre.
fn crs_row(app: &mut RustGisApp, ui: &mut Ui, g: &Grid, id: u32, set: &dyn Fn(&mut RustGisApp, Value)) {
    let input_label = ui.label(tl!("Coordinate system"));
    let key = egui::Id::unique(("grid_crs", id));
    let current = g.crs.clone().unwrap_or_default();
    let mut buf = ui.data(|d| d.get_temp::<String>(key)).unwrap_or_else(|| current.clone());
    let (hint, help) = if g.mgrs {
        (
            tl!("The map's UTM zone, else the frame's"),
            tl!(
                "A WGS 84 UTM zone for MGRS labels, by its EPSG code (32632); empty: the map's when it is one, else the zone at the frame's centre."
            ),
        )
    } else {
        (
            tl!("The map's, else its UTM zone"),
            tl!("EPSG:25832, WKT or a PROJ string; empty: the map's when projected, else the UTM zone at the frame's centre."),
        )
    };
    let r =
        ui.add(egui::TextEdit::singleline(&mut buf).hint_text(hint).desired_width(150.0)).labelled_by(input_label.id).on_hover_text(help);
    if r.has_focus() {
        ui.data_mut(|d| d.insert_temp(key, buf));
    } else {
        ui.data_mut(|d| d.remove::<String>(key));
        if r.lost_focus() && buf.trim() != current {
            set(app, json!({"crs": buf.trim()}));
        }
    }
    ui.end_row();
}

/// A map frame's rows in the grid.
pub(super) fn frame_rows(app: &mut RustGisApp, ui: &mut Ui, li: usize, e: &Element, update: &dyn Fn(&mut RustGisApp, Value)) {
    let ElementKind::MapFrame { map, scale, bearing, pitch, .. } = &e.kind else { return };
    let id = e.id;
    let drag_key = |field: &str| number_key(li, id, field);
    let input_label = ui.label(tl!("Map"));
    let maps: Vec<String> = app.session.project.maps.iter().map(|m| m.name.clone()).collect();
    egui::ComboBox::from_id_salt(("frame_map", id))
        .selected_text(map.as_str())
        .show_ui(ui, |ui| {
            for m in maps {
                if ui.selectable_label(m == *map, m.as_str()).clicked() {
                    update(app, json!({"map": m}));
                }
            }
        })
        .response
        .labelled_by(input_label.id);
    ui.end_row();
    let input_label = ui.label(tl!("Scale 1:"));
    let mut sc = *scale;
    let speed = sc * 0.01;
    // Below 1:10, tenths: 1:0.5 is a frame at 2:1 (#902).
    let decimals = if sc < 10.0 { 1 } else { 0 };
    let drag = Dragged::lend(ui, drag_key("scale"), &mut sc);
    let r = ui
        .add(
            egui::DragValue::new(&mut sc)
                .update_while_editing(false)
                .range(MIN_FRAME_SCALE..=MAX_FRAME_SCALE)
                .speed(speed)
                .max_decimals(decimals),
        )
        .labelled_by(input_label.id);
    if drag.settled(ui, &r, &mut sc) && (sc / scale - 1.0).abs() > 1e-3 {
        update(app, json!({"scale": sc}));
    }
    ui.end_row();
    // Turned and tilted as the map view is (Ctrl+drag there; Shift+arrows).
    for (label, key, current, range) in [("Bearing", "bearing", *bearing, -180.0..=180.0), ("Pitch", "pitch", *pitch, 0.0..=85.0)] {
        let input_label = ui.label(tl!(label));
        let mut v = current;
        let drag = Dragged::lend(ui, drag_key(key), &mut v);
        let r = ui
            .add(egui::DragValue::new(&mut v).update_while_editing(false).range(range).speed(0.5).max_decimals(1).suffix("°"))
            .labelled_by(input_label.id);
        if drag.settled(ui, &r, &mut v) && (v - current).abs() > 1e-6 {
            update(app, json!({key: v}));
        }
        ui.end_row();
    }
}

/// A map frame's grid, extent indicator and border, below the grid.
pub(super) fn frame_sections(
    app: &mut RustGisApp,
    ui: &mut Ui,
    layout: &Layout,
    li: usize,
    e: &Element,
    update: &dyn Fn(&mut RustGisApp, Value),
) {
    let id = e.id;
    let drag_key = |field: &str| number_key(li, id, field);
    if let ElementKind::MapFrame { grid, .. } = &e.kind {
        ui.add_space(4.0);
        egui::CollapsingHeader::new(tl!("Grid")).id_salt(("frame_grid", id)).default_open(grid.is_some()).show(ui, |ui| {
            grid_properties(app, ui, grid.as_ref(), id, &|app, g| update(app, json!({"grid": g})));
        });
    }
    // The extent indicator this frame draws (another frame's extent): its colour and width.
    if let ElementKind::MapFrame { extent_of: Some(of), extent_color, extent_width, .. } = &e.kind {
        let other = layout.element(*of).map_or_else(|| of.to_string(), |o| o.name.clone());
        ui.add_space(4.0);
        egui::CollapsingHeader::new(tl!("Extent Indicator")).id_salt(("frame_indicator", id)).default_open(true).show(ui, |ui| {
            ui.label(egui::RichText::new(crate::i18n::fmt("Outlines {frame}", &[("frame", &other)])).size(11.5));
            let range = rustgis_engine::map::layout::MIN_INDICATOR_WIDTH..=rustgis_engine::map::layout::MAX_INDICATOR_WIDTH;
            let line = Outline { key: "extentIndicator", drag: drag_key("extentWidth"), color: *extent_color, width: *extent_width, range };
            outline_rows(app, ui, ("indicator_props", id), line, None, &update);
            if ui.button(tl!("Remove Extent Indicator")).clicked() {
                update(app, json!({"extentIndicator": false}));
            }
        });
    }
    // Its border along its shape (#2557): its colour and width, 0 for none.
    if let ElementKind::MapFrame { border_color, border_width, .. } = &e.kind {
        ui.add_space(4.0);
        egui::CollapsingHeader::new(tl!("Border")).id_salt(("frame_border", id)).show(ui, |ui| {
            let range = 0.0..=rustgis_engine::map::layout::MAX_FRAME_BORDER_WIDTH;
            let line = Outline { key: "border", drag: drag_key("borderWidth"), color: *border_color, width: *border_width, range };
            outline_rows(app, ui, ("border_props", id), line, Some(tl!("0 draws no border")), &update);
            if e.kind.has_custom_border() && ui.button(tl!("Default Border")).clicked() {
                update(app, json!({"border": null}));
            }
        });
    }
}
