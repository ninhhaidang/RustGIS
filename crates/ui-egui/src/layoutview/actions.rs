//! Duplicate, Delete and Arrange, from the Format pane and the element's menu.

use crate::RustGisApp;
use egui::Ui;
use rustgis_engine::map::layout::{Arrange, ElementKind, Layout};
use serde_json::json;

/// Duplicate and Delete, under the properties: a locked element can be copied (the copy isn't
/// locked) but not deleted, nor a frame with a locked surround. Then the drawing order: Bring to
/// Front, Bring Forward, Send Backward, Send to Back.
pub(super) fn element_actions(app: &mut RustGisApp, ui: &mut Ui, layout: &Layout, li: usize, id: u32) {
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.button(tl!("Duplicate")).on_hover_text(tl!("Copy this element (Ctrl+D, ⌘D on a Mac)")).clicked() {
            duplicate_element(app, li, id);
        }
        if ui
            .add_enabled(layout.locked_in_removal(id).is_none(), egui::Button::new(tl!("Delete Element")))
            .on_hover_text(tl!("Delete (or Backspace)"))
            .on_disabled_hover_text(locked_delete())
            .clicked()
        {
            delete_element(app, li, id);
        }
    });
    // The drawing order: a legend brought to the front is drawn over the map frame under it.
    ui.horizontal_wrapped(|ui| {
        for to in Arrange::ALL {
            if ui.add_enabled(can_arrange(layout, id, to), egui::Button::new(tl!(to.label()))).on_hover_text(tl!(arrange_tip(to))).clicked()
            {
                arrange_element(app, li, id, to);
            }
        }
    });
}

/// Copies an element and selects the copy (the Format pane's Duplicate and the right-click menu's).
fn duplicate_element(app: &mut RustGisApp, li: usize, id: u32) {
    if let Ok(v) = app.run("layout.duplicateElement", json!({"layout": li, "element": id})) {
        app.layout.selected = v["element"].as_u64().and_then(|v| u32::try_from(v).ok());
    }
}

/// Deletes an element (the Format pane's Delete Element and the right-click menu's). The caller
/// offers it only where [`Layout::locked_in_removal`] allows it; the command refuses otherwise.
fn delete_element(app: &mut RustGisApp, li: usize, id: u32) {
    let _ = app.run("layout.removeElement", json!({"layout": li, "element": id}));
    app.layout.selected = None;
}

/// Moves an element in the drawing order (the Format pane's buttons and the right-click menu's).
fn arrange_element(app: &mut RustGisApp, li: usize, id: u32, to: Arrange) {
    let _ = app.run("layout.arrange", json!({"layout": li, "element": id, "to": to.code()}));
}

/// Whether the step `to` in the drawing order moves element `id`: not one it can't take (already
/// on top, or at the bottom), and not a locked element's.
fn can_arrange(layout: &Layout, id: u32, to: Arrange) -> bool {
    let Some((at, n)) = layout.draw_order(id) else { return false };
    let locked = layout.element(id).is_some_and(|e| e.locked);
    let step = match to {
        Arrange::Front | Arrange::Forward => at + 1 < n,
        Arrange::Backward | Arrange::Back => at > 0,
    };
    step && !locked
}

/// What an arrange step does, for its tooltip.
fn arrange_tip(to: Arrange) -> &'static str {
    match to {
        Arrange::Front => "Draw it over every other element",
        Arrange::Forward => "Draw it over the element above it",
        Arrange::Backward => "Draw it under the element below it",
        Arrange::Back => "Draw it under every other element",
    }
}

/// The menu right-clicking element `id` opens: Duplicate and Delete (with their keys), the four
/// drawing-order steps, and for a map frame Zoom Frame. The rules are the Format pane's: a locked
/// element can be duplicated but not deleted, and an arrange step it can't take is greyed. Each
/// row runs the command the pane's button runs, so one Undo takes it back.
pub(super) fn element_menu(app: &mut RustGisApp, ui: &mut Ui, layout: &Layout, li: usize, id: u32) {
    use crate::widgets::{menu_row, shortcut_text};
    let duplicate = shortcut_text(ui.ctx(), app, "layout.duplicateElement");
    if menu_row(ui, "", tl!("Duplicate"), &duplicate, true).on_hover_text(tl!("Copy this element")).clicked() {
        duplicate_element(app, li, id);
        ui.close();
    }
    let delete = shortcut_text(ui.ctx(), app, "edit.delete");
    let can_delete = layout.locked_in_removal(id).is_none();
    if menu_row(ui, "", tl!("Delete Element"), &delete, can_delete).on_disabled_hover_text(locked_delete()).clicked() {
        delete_element(app, li, id);
        ui.close();
    }
    ui.separator();
    for to in Arrange::ALL {
        if menu_row(ui, "", tl!(to.label()), "", can_arrange(layout, id, to)).on_hover_text(tl!(arrange_tip(to))).clicked() {
            arrange_element(app, li, id, to);
            ui.close();
        }
    }
    if layout.element(id).is_some_and(|e| matches!(e.kind, ElementKind::MapFrame { .. })) {
        ui.separator();
        // As double-clicking the frame does: its map follows the map view.
        let tip = tl!("Show the map view's extent in this frame");
        if menu_row(ui, "", tl!("Zoom Frame"), "", true).on_hover_text(tip).clicked() {
            let _ = app.run("layout.frameZoom", json!({"layout": li, "element": id, "to": "view"}));
            ui.close();
        }
    }
}

/// Why a locked element, or a frame with a locked surround, isn't deleted.
pub(crate) fn locked_delete() -> &'static str {
    tl!("It is locked, or one of its surrounds is: unlock it first")
}
