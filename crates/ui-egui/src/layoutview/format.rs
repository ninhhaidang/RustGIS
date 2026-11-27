//! The Format pane of the selected layout element; each element's own rows are in its file under `format/`.

mod chart;
mod colorbar;
mod frame;
mod legend;
mod north;
mod scalebar;
mod shapes;
mod table;
mod text;

use super::*;
use crate::RustGisApp;
use crate::theme::{Tokens, semibold};
use crate::widgets::Dragged;
use chart::*;
use colorbar::*;
use egui::{Color32, Ui, vec2};
use frame::*;
use legend::*;
use north::*;
use rustgis_engine::map::layout::{
    Background, ElementKind, INK, LAYOUT_TEXT_SIZE, Layout, MAX_BACKGROUND_PADDING, MAX_BACKGROUND_RADIUS, MAX_BACKGROUND_WIDTH,
    Orientation,
};
use rustgis_engine::map::symbology::Dash;
use scalebar::*;
use serde_json::{Value, json};
use shapes::*;
use table::*;
use text::*;

/// A row picking horizontal or vertical: the one picked when it changes, as its command code.
pub(super) fn orientation_row(ui: &mut Ui, current: Orientation) -> Option<&'static str> {
    ui.label(tl!("Orientation"));
    let mut picked = None;
    ui.horizontal(|ui| {
        for (label, o, code) in [("Horizontal", Orientation::Horizontal, "horizontal"), ("Vertical", Orientation::Vertical, "vertical")] {
            if ui.selectable_label(current == o, tl!(label)).clicked() && current != o {
                picked = Some(code);
            }
        }
    });
    ui.end_row();
    picked
}

/// A text field whose typing is kept in the view's state while it has focus. Returns the new text
/// when the field loses focus with a change.
pub(super) fn text_field(
    app: &mut RustGisApp,
    ui: &mut Ui,
    key: (usize, u32, &'static str),
    current: &str,
    multiline: bool,
    label_id: egui::Id,
) -> Option<String> {
    hinted_text_field(app, ui, key, current, multiline, None, label_id)
}

/// [`text_field`] showing `hint` while it's empty.
pub(super) fn hinted_text_field(
    app: &mut RustGisApp,
    ui: &mut Ui,
    key: (usize, u32, &'static str),
    current: &str,
    multiline: bool,
    hint: Option<&str>,
    label_id: egui::Id,
) -> Option<String> {
    let mut buf = app.layout.typing.remove(&key).unwrap_or_else(|| current.to_string());
    let mut edit = if multiline {
        egui::TextEdit::multiline(&mut buf).desired_rows(3).desired_width(240.0)
    } else {
        egui::TextEdit::singleline(&mut buf)
    };
    if let Some(hint) = hint {
        edit = edit.hint_text(hint);
    }
    let r = ui.add(edit).labelled_by(label_id);
    if r.lost_focus() {
        return (buf != current).then_some(buf);
    }
    if r.has_focus() {
        app.layout.typing.insert(key, buf);
    }
    None
}

/// An outline in a map frame's properties (its extent indicator, its border): the command key its
/// changes go under, its width's drag id, and its colour and width (points) within `range`.
pub(super) struct Outline {
    key: &'static str,
    drag: egui::Id,
    color: rustgis_engine::map::Rgba,
    width: f64,
    range: std::ops::RangeInclusive<f64>,
}

/// An outline's Color and Line width rows, one component for the extent indicator and the border
/// (#2557): each change goes to `update` as `{key: {"color"|"width": …}}`, the width with `hint`
/// on hover.
pub(super) fn outline_rows(
    app: &mut RustGisApp,
    ui: &mut Ui,
    grid: impl egui::AsIdSalt,
    line: Outline,
    hint: Option<&str>,
    update: &dyn Fn(&mut RustGisApp, Value),
) {
    egui::Grid::new(grid).num_columns(2).show(ui, |ui| {
        ui.label(tl!("Color"));
        let mut c = crate::theme::c32(line.color);
        let resp = crate::widgets::color_button(ui, &mut c);
        if resp.changed() {
            crate::widgets::one_step_per_press(app, &resp);
            update(app, json!({line.key: {"color": crate::theme::rgba(c).hex()}}));
        }
        ui.end_row();
        let input_label = ui.label(tl!("Line width"));
        let mut w = line.width;
        let drag = Dragged::lend(ui, line.drag, &mut w);
        let mut r = ui
            .add(egui::DragValue::new(&mut w).update_while_editing(false).range(line.range).speed(0.05).max_decimals(2).suffix(" pt"))
            .labelled_by(input_label.id);
        if let Some(hint) = hint {
            r = r.on_hover_text(hint);
        }
        if drag.settled(ui, &r, &mut w) && (w - line.width).abs() > 1e-6 {
            update(app, json!({line.key: {"width": w}}));
        }
        ui.end_row();
    });
}

/// The Format pane (#516): the selected layout element's properties, while its layout is on
/// screen. On the map (no layout open) or under the Model Builder, it waits for a selection.
pub fn format(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    let on_screen = app.session.active_layout.filter(|_| !app.mb.shown());
    let layout = on_screen.and_then(|li| Some((li, shown_layout(app, li)?)));
    let selected = layout.as_ref().and_then(|(li, l)| Some((*li, app.layout.selected.filter(|id| l.element(*id).is_some())?)));
    match (layout, selected) {
        (Some((li, layout)), Some(drawn)) => {
            app.layout.drawn = Some(drawn);
            let Some(e) = layout.element(drawn.1) else { return };
            // Where it is ("Layout: Text"), as the pane also formats a text on the map ("Map: Text").
            let heading = crate::i18n::fmt("Layout: {kind}", &[("kind", tl_raw!(e.kind.label()))]);
            ui.label(egui::RichText::new(heading).font(semibold(14.0)));
            ui.separator();
            // Locked (#536), its properties show but take no change until it is unlocked.
            let mut lock = e.locked;
            if ui
                .checkbox(&mut lock, tl!("Locked"))
                .on_hover_text(tl!("Keep it from being moved, resized, deleted or edited by mistake"))
                .changed()
            {
                let _ = app.run("layout.update", json!({"layout": li, "element": e.id, "locked": lock}));
            }
            ui.add_enabled_ui(!e.locked, |ui| properties(app, ui, &layout, li));
            element_actions(app, ui, &layout, li, e.id);
        }
        _ => {
            ui.label(egui::RichText::new(tl!("Select an element to format it.")).color(t.text_dim));
        }
    }
}

/// What the Format pane says of a text's lines left out (#2346), below its box or past the 200
/// a text draws, as the page drawn counts them: `None` when they all fit.
pub(super) fn lines_left_out_label(loss: &rustgis_engine::render::layout::TextLoss) -> Option<String> {
    // Lines past the 200 a text draws aren't the box's: no box draws them. Lines left out of
    // both are said of both, as the command's notice says them.
    let n = || loss.left_out.max(1).to_string();
    match (loss.left_out, loss.at_least, loss.below_box, loss.past_limit) {
        (0, false, ..) => None,
        (1, false, _, false) => Some(tl!("1 line doesn't fit the box").to_string()),
        (_, false, _, false) => Some(crate::i18n::fmt("{n} lines don't fit the box", &[("n", &n())])),
        (_, true, _, false) => Some(crate::i18n::fmt("{n} or more lines don't fit the box", &[("n", &n())])),
        (_, false, true, true) => Some(crate::i18n::fmt("{n} lines don't fit the box or the 200 a text draws", &[("n", &n())])),
        (_, true, true, true) => Some(crate::i18n::fmt("{n} or more lines don't fit the box or the 200 a text draws", &[("n", &n())])),
        (1, false, false, true) => Some(tl!("1 line isn't drawn: a text draws 200 at most").to_string()),
        (_, false, false, true) => Some(crate::i18n::fmt("{n} lines aren't drawn: a text draws 200 at most", &[("n", &n())])),
        (_, true, false, true) => Some(crate::i18n::fmt("{n} or more lines aren't drawn: a text draws 200 at most", &[("n", &n())])),
    }
}

/// Each number kept across a drag until it is let go (#607): one key per layout, element and field.
pub(super) fn number_key(li: usize, element: u32, field: &str) -> egui::Id {
    egui::Id::unique(("layout_drag", li, element, field))
}

/// The selected element's properties.
pub(super) fn properties(app: &mut RustGisApp, ui: &mut Ui, layout: &Layout, li: usize) {
    let Some(e) = app.layout.selected.and_then(|id| layout.element(id)).cloned() else { return };
    let id = e.id;
    // Each number kept across a drag until it is let go (#607): one key per layout, element and field.
    let drag_key = |field: &str| number_key(li, id, field);
    let update = |app: &mut RustGisApp, p: Value| {
        let mut p = p;
        p["layout"] = json!(li);
        p["element"] = json!(id);
        let _ = app.run("layout.update", p);
    };
    crate::textform::section(ui, "Element", |ui| {
        egui::Grid::new(("layout_props", id)).num_columns(2).min_col_width(crate::textform::LABEL_W).spacing(vec2(8.0, 6.0)).show(
            ui,
            |ui| {
                let input_label = ui.label(tl!("Name"));
                if let Some(name) = text_field(app, ui, (li, id, "name"), &e.name, false, input_label.id) {
                    update(app, json!({"name": name}));
                }
                ui.end_row();
                for (label, key, v) in [("X (mm)", "x", e.x), ("Y (mm)", "y", e.y), ("Width (mm)", "w", e.w), ("Height (mm)", "h", e.h)] {
                    let input_label = ui.label(tl!(label));
                    let mut val = v;
                    let drag = Dragged::lend(ui, drag_key(key), &mut val);
                    let r = ui
                        .add(egui::DragValue::new(&mut val).update_while_editing(false).speed(0.5).max_decimals(1))
                        .labelled_by(input_label.id);
                    if drag.settled(ui, &r, &mut val) && (val - v).abs() > 1e-6 {
                        update(app, json!({key: val}));
                    }
                    ui.end_row();
                }
                let input_label = ui.label(tl!("Visible"));
                let mut vis = e.visible;
                if ui.checkbox(&mut vis, "").labelled_by(input_label.id).changed() {
                    update(app, json!({"visible": vis}));
                }
                ui.end_row();
                match &e.kind {
                    ElementKind::Text { .. } => text_rows(app, ui, li, &e, &update),
                    ElementKind::Legend { .. } => legend_property_rows(app, ui, li, &e, &update),
                    ElementKind::Colorbar { .. } => colorbar_rows(app, ui, layout, li, &e, &update),
                    ElementKind::MapFrame { .. } => frame_rows(app, ui, li, &e, &update),
                    ElementKind::NorthArrow { .. } => north_arrow_rows(app, ui, &e, &update),
                    ElementKind::ScaleBar { .. } => scale_bar_rows(app, ui, layout, li, &e, &update),
                    ElementKind::Chart { .. } => chart_rows(app, ui, layout, li, &e, &update),
                    ElementKind::Table { .. } => table_rows(app, ui, li, &e, &update),
                    ElementKind::Rectangle { .. } | ElementKind::Ellipse { .. } => shape_rows(app, ui, li, &e, &update),
                    ElementKind::Line { .. } => line_rows(app, ui, li, &e, &update),
                    _ => {}
                }
                // A legend's, colorbar's, table's or chart's ink (#2257), on and off as a background
                // is: off is the default dark grey, which isn't the same as a dark grey ink (a table
                // keeps its white cells only without one). Turned on, it starts white over a dark
                // background and dark grey otherwise, so its text never disappears.
                if e.kind.takes_ink() {
                    let ink = e.kind.ink();
                    let input_label = ui.label(tl!("Ink"));
                    ui.horizontal(|ui| {
                        let mut on = ink.is_some();
                        let hint = tl!("Its own colour for its text and lines; off, the default dark grey");
                        if ui.checkbox(&mut on, "").labelled_by(input_label.id).on_hover_text(hint).changed() {
                            let page = layout.paper.unwrap_or(rustgis_engine::map::Rgba::rgb(0xFF, 0xFF, 0xFF));
                            update(app, json!({"color": on.then(|| first_ink(e.background.as_ref(), page).hex())}));
                        }
                        let Some(current) = ink else { return };
                        let mut c = crate::theme::c32(current);
                        let resp = crate::widgets::color_button(ui, &mut c);
                        if resp.changed() {
                            crate::widgets::one_step_per_press(app, &resp);
                            update(app, json!({"color": crate::theme::rgba(c).hex()}));
                        }
                    });
                    ui.end_row();
                }
            },
        );
    });
    text_section(app, ui, layout, li, &e, &update);
    legend_layers(app, ui, layout, &e, &update);
    frame_sections(app, ui, layout, li, &e, &update);
    // Its background (#2257): a box behind its content, which a map frame, drawing its own paper,
    // doesn't take.
    if e.kind.takes_background() {
        crate::textform::section(ui, "Background", |ui| {
            let mut on = e.background.is_some();
            if ui.checkbox(&mut on, tl!("Background")).on_hover_text(tl!("A box behind the element, which moves with it.")).changed() {
                // Turned on, it starts white with no outline; the options below change it.
                update(app, json!({"background": if on { json!({}) } else { Value::Null }}));
            }
            let Some(b) = e.background else { return };
            egui::Grid::new(("layout_background", id)).num_columns(2).min_col_width(crate::textform::LABEL_W).spacing(vec2(8.0, 6.0)).show(
                ui,
                |ui| {
                    for (label, key, current) in [("Fill", "fill", b.fill), ("Outline", "outline", b.outline)] {
                        ui.label(tl!(label));
                        let mut c = crate::theme::c32(current);
                        let resp = crate::widgets::color_button(ui, &mut c);
                        if resp.changed() {
                            crate::widgets::one_step_per_press(app, &resp);
                            update(app, json!({"background": {key: crate::theme::rgba(c).hex()}}));
                        }
                        ui.end_row();
                    }
                    for (label, key, drag_id, current, max, suffix) in [
                        ("Outline width", "width", "background_width", b.width, MAX_BACKGROUND_WIDTH, " pt"),
                        ("Corner radius", "radius", "background_radius", b.radius, MAX_BACKGROUND_RADIUS, " mm"),
                        ("Padding", "padding", "background_padding", b.padding, MAX_BACKGROUND_PADDING, " mm"),
                    ] {
                        let input_label = ui.label(tl!(label));
                        let mut v = current;
                        let drag = Dragged::lend(ui, drag_key(drag_id), &mut v);
                        let r = ui
                            .add(
                                egui::DragValue::new(&mut v)
                                    .update_while_editing(false)
                                    .range(0.0..=max)
                                    .speed(0.1)
                                    .max_decimals(1)
                                    .suffix(suffix),
                            )
                            .labelled_by(input_label.id);
                        if drag.settled(ui, &r, &mut v) && (v - current).abs() > 1e-6 {
                            update(app, json!({"background": {key: v}}));
                        }
                        ui.end_row();
                    }
                },
            );
        });
    }
    ui.add_space(8.0);
    if let ElementKind::MapFrame { .. } = e.kind {
        ui.toggle_value(&mut app.layout.navigate, tl!("Pan and Zoom Map"))
            .on_hover_text(tl!("Drag inside map frames to pan their maps and turn the wheel to zoom them, instead of moving them."));
        ui.horizontal_wrapped(|ui| {
            for (label, to) in [("Zoom To Map View", "view"), ("Full Extent", "full"), ("Selection", "selection")] {
                if ui.button(tl!(label)).clicked() {
                    let _ = app.run("layout.frameZoom", json!({"layout": li, "element": id, "to": to}));
                }
            }
        });
    }
}

/// The dash pattern of a layout line or outline `width` points wide, picked from the Symbology
/// pane's presets (#1763), scaled by the width as that pane scales them: the pattern
/// `layout.update` takes (`[]` for solid) when one other than `dash` is picked. A pattern set by
/// command that isn't a preset shows as its lengths.
pub(super) fn dash_picked(ui: &mut Ui, salt: (&str, u32), dash: Option<Dash>, width: f64, label: egui::Id) -> Option<Vec<f32>> {
    let scale = (width as f32).max(1.0);
    let preset = |steps: Option<&[f32]>| steps.and_then(|s| Dash::new(&s.iter().map(|v| v * scale).collect::<Vec<_>>()));
    let shown = match crate::panes::DASH_PRESETS.iter().find(|(_, steps)| preset(*steps) == dash) {
        Some((name, _)) => tl!(name).to_string(),
        None => dash.map(|d| d.steps().iter().map(|v| v.to_string()).collect::<Vec<_>>().join(" ")).unwrap_or_default(),
    };
    let mut picked = None;
    egui::ComboBox::from_id_salt(salt)
        .selected_text(shown)
        .show_ui(ui, |ui| {
            for (name, steps) in crate::panes::DASH_PRESETS {
                let d = preset(steps);
                if ui.selectable_label(d == dash, tl!(name)).clicked() && d != dash {
                    picked = Some(d.map_or_else(Vec::new, |d| d.steps().to_vec()));
                }
            }
        })
        .response
        .labelled_by(label);
    picked
}

/// A drop-down of `items` (shown text, whether it is the one set, the value it sends) showing
/// `current`, labelled by `label`; the value of the item picked, when it isn't the one set.
pub(super) fn choice(ui: &mut Ui, salt: (&str, u32), label: egui::Id, current: &str, items: &[(String, bool, Value)]) -> Option<Value> {
    let mut picked = None;
    egui::ComboBox::from_id_salt(salt)
        .selected_text(current)
        .show_ui(ui, |ui| {
            for (text, set, value) in items {
                if ui.selectable_label(*set, text.as_str()).clicked() && !*set {
                    picked = Some(value.clone());
                }
            }
        })
        .response
        .labelled_by(label);
    picked
}

/// The ink an element starts with when Ink is turned on (#2257): white over what shows under its
/// text, its background drawn on the page (`page`, the page's colour, #2855), when that is dark,
/// else the default dark grey, so its text never disappears.
pub(super) fn first_ink(background: Option<&Background>, page: rustgis_engine::map::Rgba) -> rustgis_engine::map::Rgba {
    // No background: nothing drawn over the page, so the page itself shows under the text.
    let fill = background.map_or(rustgis_engine::map::Rgba::new(0, 0, 0, 0), |b| b.fill);
    let a = u16::from(fill.a());
    let over = |c: u8, under: u8| u8::try_from((u16::from(c) * a + u16::from(under) * (255 - a)) / 255).unwrap_or(u8::MAX);
    let shown = Color32::from_rgb(over(fill.r(), page.r()), over(fill.g(), page.g()), over(fill.b(), page.b()));
    let white = rustgis_engine::map::Rgba::rgb(0xFF, 0xFF, 0xFF);
    if crate::theme::contrast(shown, Color32::WHITE) > crate::theme::contrast(shown, crate::theme::c32(INK)) { white } else { INK }
}

/// A row of a layout element's text size in points, within [`LAYOUT_TEXT_SIZE`], labelled `label`
/// (#2581): the size it settles on when it differs from `current`.
pub(super) fn text_size_row(ui: &mut egui::Ui, label: impl Into<egui::WidgetText>, key: egui::Id, current: f64) -> Option<f64> {
    let input_label = ui.label(label);
    let mut pt = current;
    let drag = Dragged::lend(ui, key, &mut pt);
    let r = ui
        .add(egui::DragValue::new(&mut pt).update_while_editing(false).range(LAYOUT_TEXT_SIZE).speed(0.1).max_decimals(1).suffix(" pt"))
        .labelled_by(input_label.id);
    let settled = drag.settled(ui, &r, &mut pt) && (pt - current).abs() > 1e-6;
    ui.end_row();
    settled.then_some(pt)
}
