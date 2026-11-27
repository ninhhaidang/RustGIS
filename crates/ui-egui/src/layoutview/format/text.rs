//! A layout text's Format rows.

use super::*;
use crate::RustGisApp;
use crate::widgets::Dragged;
use egui::Ui;
use rustgis_engine::map::layout::{Element, ElementKind, Layout};
use serde_json::{Value, json};

/// A text's own rows in the grid: its turn.
pub(super) fn text_rows(app: &mut RustGisApp, ui: &mut Ui, li: usize, e: &Element, update: &dyn Fn(&mut RustGisApp, Value)) {
    let ElementKind::Text { rotation, .. } = &e.kind else { return };
    let id = e.id;
    let drag_key = |field: &str| number_key(li, id, field);
    // Its text, size, colour and boldness are the shared form, below the grid.
    // Its turn is clockwise about its centre, as a frame's bearing: 90 reads top
    // to bottom, -90 bottom to top.
    let input_label = ui.label(tl!("Rotation"));
    ui.horizontal(|ui| {
        let mut a = *rotation;
        let drag = Dragged::lend(ui, drag_key("rotation"), &mut a);
        let r = ui
            .add(egui::DragValue::new(&mut a).update_while_editing(false).range(-180.0..=180.0).speed(1.0).max_decimals(1).suffix("°"))
            .labelled_by(input_label.id);
        if drag.settled(ui, &r, &mut a) && (a - rotation).abs() > 1e-6 {
            update(app, json!({"rotation": a}));
        }
        for quick in [0.0, 90.0, -90.0] {
            if ui.selectable_label(*rotation == quick, format!("{quick:.0}°")).clicked() && *rotation != quick {
                update(app, json!({"rotation": quick}));
            }
        }
    });
    ui.end_row();
}

/// The shared text form below the grid.
pub(super) fn text_section(
    app: &mut RustGisApp,
    ui: &mut Ui,
    layout: &Layout,
    li: usize,
    e: &Element,
    update: &dyn Fn(&mut RustGisApp, Value),
) {
    let id = e.id;
    // A layout's text: the form a text graphic has too (#516, #535), with the page's dynamic text.
    if let ElementKind::Text {
        text,
        size,
        color,
        bold,
        italic,
        align,
        letter_spacing,
        font,
        font_name,
        wrap,
        valign,
        line_spacing,
        case,
        ..
    } = &e.kind
    {
        let key = (li, id, "text");
        let mut buf = app.layout.typing.remove(&key).unwrap_or_else(|| text.clone());
        let (mut sz, mut c, mut bb) = (*size, crate::theme::c32(*color), *bold);
        let (mut it, mut al, mut ls, mut fam, mut named) = (*italic, *align, *letter_spacing, *font, font_name.clone());
        let (mut wr, mut va, mut line_sp, mut cs) = (*wrap, valign.kept(), *line_spacing, case.kept());
        let tokens = [rustgis_engine::map::graphics::TOKENS, rustgis_engine::map::layout::PAGE_TOKENS];
        let layers = || crate::textform::StatLayer::of(layout, &app.session.project.maps);
        let warning = app
            .layout
            .text_losses
            .as_ref()
            .filter(|(drawn, _)| *drawn == li)
            .and_then(|(_, losses)| losses.iter().find(|l| l.id == id))
            .and_then(lines_left_out_label);
        let fields = crate::textform::Fields {
            text: &mut buf,
            size: &mut sz,
            color: Some(&mut c),
            bold: Some(&mut bb),
            italic: Some(&mut it),
            align: Some(&mut al),
            wrap: Some(&mut wr),
            warning,
            valign: Some(&mut va),
            line_spacing: Some(&mut line_sp),
            case: Some(&mut cs),
            letter_spacing: Some(&mut ls),
            font: Some(&mut fam),
            font_name: Some(&mut named),
            named_fonts: true,
            select: false,
        };
        let settled =
            crate::textform::section(ui, "Text", |ui| crate::textform::show(ui, ("layout_text", li, id), &tokens, layers, fields))
                .unwrap_or_default();
        if settled.typing {
            app.layout.typing.insert(key, buf);
        } else if settled.text && buf != *text {
            update(app, json!({"text": buf}));
        }
        if settled.size {
            update(app, json!({"size": sz}));
        }
        if let Some(resp) = &settled.color {
            crate::widgets::one_step_per_press(app, resp);
            update(app, json!({"color": crate::theme::rgba(c).hex()}));
        }
        if settled.bold {
            update(app, json!({"bold": bb}));
        }
        if settled.italic {
            update(app, json!({"italic": it}));
        }
        if settled.align {
            update(app, json!({"align": al.code()}));
        }
        if settled.wrap {
            update(app, json!({"wrap": wr}));
        }
        if settled.valign {
            update(app, json!({"valign": va.code()}));
        }
        if settled.line_spacing {
            update(app, json!({"lineSpacing": line_sp}));
        }
        if settled.case {
            update(app, json!({"case": cs.code()}));
        }
        if settled.letter_spacing {
            update(app, json!({"letterSpacing": ls}));
        }
        if settled.font {
            // A named family with its fallback (#762), else the generic one.
            let font = match &named {
                Some(n) => json!({"font": n, "fallback": fam.code()}),
                None => json!({"font": fam.code()}),
            };
            update(app, font);
        }
    }
}
