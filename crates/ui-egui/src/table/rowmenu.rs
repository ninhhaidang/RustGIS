//! The menu a right-click on a row of the attribute table opens.

use egui::{Response, Ui};
use rustgis_engine::data::Value;
use serde_json::json;

use super::ViewKey;
use crate::RustGisApp;
use crate::widgets::{menu_row, shortcut_text, why_disabled};

/// The cell the menu was opened on.
#[derive(Clone, Copy)]
pub(super) struct Opened {
    view: ViewKey,
    /// The row's feature: when the row holds another one (the data changed under the menu), the
    /// menu closes rather than act on it.
    fid: u64,
    /// The cell's field, or None for the Feature ID cell.
    col: Option<usize>,
    /// The fields the table had, and which standalone table it was ([`schema`]): with another
    /// set, `col` may be another field, and the menu closes rather than copy its value.
    fields: u64,
}

/// A row on screen that was just right-clicked, or whose menu is open.
pub(super) struct RowAt {
    /// The union of the row's cells and the text in them.
    pub resp: Response,
    /// The row's position in the dataset, whatever the table's sort and filter show.
    pub row: usize,
    pub fid: u64,
    /// The field of the cell right-clicked in this frame (None: the Feature ID cell).
    pub col: Option<usize>,
}

/// Opens or keeps open the menu of a row. A right-click on a row that isn't selected first selects
/// it alone, as a click on it does (so nothing stays selected in other layers either); one inside
/// the selection keeps every selection, and the menu acts on this layer's.
pub(super) fn show(app: &mut RustGisApp, view: ViewKey, at: RowAt) {
    if at.resp.secondary_clicked() {
        app.table.menu = schema(app, view).map(|fields| Opened { view, fid: at.fid, col: at.col, fields });
        // Once, as the menu opens, and before it is drawn: its rows are judged by the selection.
        if let Some(layer) = layer_id(app, view)
            && app.session.map().layer(layer).is_some_and(|l| !l.selection.contains(&at.row))
        {
            let _ = app.run("select.rows", json!({"layer": layer, "rows": [at.row], "mode": "new"}));
        }
    }
    at.resp.context_menu(|ui| match view {
        ViewKey::Layer(_) => layer_rows(app, ui, view, at.row),
        ViewKey::Table(id) => table_row(app, ui, view, id, at.row),
    });
}

/// The id of the layer a table view shows, on the map shown.
fn layer_id(app: &RustGisApp, view: ViewKey) -> Option<u32> {
    let ViewKey::Layer(instance) = view else { return None };
    app.session.map().layers.iter().find(|l| l.instance_id() == instance).map(|l| l.id)
}

/// The names of the fields of the layer or table `view` shows, in order, as one number: it
/// changes when a field is added, removed, renamed or moved. A standalone table's also changes
/// when another table takes its place under the same id (a layer's view is already its own).
fn schema(app: &RustGisApp, view: ViewKey) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let fields = match view {
        ViewKey::Layer(_) => &app.session.map().layer(layer_id(app, view)?)?.dataset.fields,
        ViewKey::Table(id) => {
            let table = app.session.project.table(id)?;
            table.instance_id().hash(&mut hasher);
            &table.dataset.fields
        }
    };
    for field in fields {
        field.name.hash(&mut hasher);
    }
    Some(hasher.finish())
}

/// The field of the cell the open menu was opened on, if row `row` of `view` still holds the
/// feature it had then (`live`, its id now) and the fields are the ones it had.
fn opened_cell(app: &RustGisApp, view: ViewKey, live: Option<u64>) -> Option<Option<usize>> {
    let fields = schema(app, view);
    app.table.menu.filter(|m| m.view == view && Some(m.fid) == live && Some(m.fields) == fields).map(|m| m.col)
}

/// A row of the menu that can't be chosen, saying why.
fn greyed(ui: &mut Ui, label: &str, why: &str) {
    menu_row(ui, "", label, "", false).on_disabled_hover_text(why);
}

/// A cell's value as Copy Value copies it: what the table shows, but a number in full, where the
/// table rounds it to six decimals.
fn cell_text(value: &Value) -> String {
    match value {
        Value::Float(number) => number.to_string(),
        shown => shown.to_string(),
    }
}

/// Copy Value: the text of the cell the menu was opened on, as the table shows it (a picture,
/// shown as a word, gives its stored value; a number is copied in full).
fn copy_value(app: &mut RustGisApp, ui: &mut Ui, text: String) {
    if menu_row(ui, "", tl!("Copy Value"), "", true).clicked() {
        app.ui.copy_out = Some(text);
        ui.close();
    }
}

/// The menu of a standalone table's row: only Copy Value can act, as no command selects, copies or
/// deletes such a row, and it has no shape on the map.
fn table_row(app: &mut RustGisApp, ui: &mut Ui, view: ViewKey, table: u32, row: usize) {
    let cell = app.session.project.table(table).and_then(|t| {
        let feature = t.dataset.features.row(row)?;
        let col = opened_cell(app, view, Some(feature.id()))?;
        Some(col.map_or_else(|| feature.id().to_string(), |c| cell_text(feature.value(c))))
    });
    let Some(text) = cell else {
        ui.close();
        return;
    };
    greyed(ui, tl!("Zoom To"), tl!("A standalone table has no features on the map."));
    greyed(ui, tl!("Remove from Selection"), tl!("A standalone table has no selection."));
    ui.separator();
    greyed(ui, tl!("Copy"), tl!("A standalone table's rows can't be copied as features."));
    copy_value(app, ui, text);
    ui.separator();
    greyed(ui, tl!("Delete"), tl!("A standalone table's rows can't be deleted."));
}

/// What the menu of a layer's row shows, read from the layer as it is now.
struct LayerRow {
    layer: u32,
    /// The text Copy Value copies.
    text: String,
    selected: bool,
    /// How many rows Zoom To, Copy and Delete act on: the layer's whole selection, when the row
    /// is in it, as the toolbar's buttons take it (rows a filter hides among them).
    count: usize,
    /// Whether any of them has a shape to zoom to.
    shaped: bool,
    /// Whether no other layer has a selection: the Copy and Delete keys, which take every
    /// layer's, then do what the menu's rows do, and the rows show them.
    alone: bool,
    streamed: bool,
}

/// Reads [`LayerRow`] for row `row`, or None when the layer or the row has gone since the menu
/// opened, or the row holds another feature.
fn read_layer_row(app: &RustGisApp, view: ViewKey, row: usize) -> Option<LayerRow> {
    let layer = layer_id(app, view)?;
    let l = app.session.map().layer(layer)?;
    let features = &l.dataset.features;
    let feature = features.row(row)?;
    let col = opened_cell(app, view, Some(feature.id()))?;
    let selected = l.selection.contains(&row);
    let acted: &[usize] = if selected { &l.selection } else { std::slice::from_ref(&row) };
    Some(LayerRow {
        layer,
        text: col.map_or_else(|| feature.id().to_string(), |c| cell_text(feature.value(c))),
        selected,
        count: acted.len(),
        shaped: acted.iter().any(|&r| features.geometry(r).is_some()),
        alone: app.session.map().layers.iter().all(|o| o.id == layer || o.selection.is_empty()),
        streamed: l.is_streamed(),
    })
}

/// Makes the rows the menu acts on the whole selection of the map, which is what Zoom To
/// Selection, Copy and Delete take: the layer's selection when `row` is in it, else `row` alone,
/// with nothing selected in other layers. False when that selection couldn't be made, so the
/// command isn't run on another.
fn select_acted(app: &mut RustGisApp, layer: u32, row: usize) -> bool {
    let map = app.session.map();
    let Some(l) = map.layer(layer) else { return false };
    let selected = l.selection.contains(&row);
    if selected && map.layers.iter().all(|o| o.id == layer || o.selection.is_empty()) {
        return true;
    }
    let rows = if selected { l.selection.clone() } else { vec![row] };
    app.run("select.rows", json!({"layer": layer, "rows": rows, "mode": "new"})).is_ok()
}

/// Runs `command` on the rows the menu acts on (see [`select_acted`]).
fn run_on_acted(app: &mut RustGisApp, layer: u32, row: usize, command: &str) {
    if select_acted(app, layer, row) {
        // A refusal is said by `run`.
        let _ = app.run(command, serde_json::Value::Null);
    }
}

/// The menu of a layer's row: Zoom To, Remove from Selection, Copy, Copy Value and Delete. Each
/// runs the command the table's toolbar or the ribbon runs.
fn layer_rows(app: &mut RustGisApp, ui: &mut Ui, view: ViewKey, row: usize) {
    let Some(at) = read_layer_row(app, view, row) else {
        ui.close();
        return;
    };
    let several = at.count > 1;
    let n = at.count.to_string();
    let args = [("n", n.as_str())];

    let zoom = if several { crate::i18n::fmt("Zoom To {n} Rows", &args) } else { tl!("Zoom To").to_string() };
    let why = if several { tl!("None of these rows has a shape to zoom to.") } else { tl!("This row has no shape to zoom to.") };
    if menu_row(ui, "", &zoom, "", at.shaped).on_disabled_hover_text(why).clicked() {
        run_on_acted(app, at.layer, row, "select.zoomTo");
        ui.close();
    }
    // This row's alone: the whole selection is the toolbar's Clear selection.
    let remove = if several { tl!("Remove This Row from Selection") } else { tl!("Remove from Selection") };
    if menu_row(ui, "", remove, "", at.selected).on_disabled_hover_text(tl!("This row isn't selected.")).clicked() {
        let _ = app.run("select.rows", json!({"layer": at.layer, "rows": [row], "mode": "remove"}));
        ui.close();
    }
    ui.separator();
    // A key is shown only where it does what its row does: with a selection in another layer,
    // the key takes that one too, and the row doesn't; and with this row no longer selected (a
    // command changed the selection under the open menu), the key takes other rows.
    let same = at.alone && at.selected;
    let key = |app: &RustGisApp, ui: &Ui, command: &str| if same { shortcut_text(ui.ctx(), app, command) } else { String::new() };
    let shortcut = key(app, ui, "edit.copy");
    let copy = if several { crate::i18n::fmt("Copy {n} Rows", &args) } else { tl!("Copy").to_string() };
    if menu_row(ui, "", &copy, &shortcut, true).clicked() {
        run_on_acted(app, at.layer, row, "ui.copy");
        ui.close();
    }
    copy_value(app, ui, at.text);
    ui.separator();
    let shortcut = key(app, ui, "edit.delete");
    let delete = if several { crate::i18n::fmt("Delete {n} Rows", &args) } else { tl!("Delete").to_string() };
    // Greyed as the ribbon's Delete is, and for a layer whose edits the command refuses.
    let why = why_disabled(app, "edit.delete").map(|why| tl!(why)).or_else(|| {
        at.streamed.then(|| tl!("This layer is read in place from its file. Use Load All Features in the Layers pane to edit it."))
    });
    if menu_row(ui, "", &delete, &shortcut, why.is_none()).on_disabled_hover_text(why.unwrap_or_default()).clicked() {
        run_on_acted(app, at.layer, row, "edit.delete");
        ui.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Copy Value copies a number in full, not rounded to the six decimals the table shows.
    #[test]
    fn copy_value_keeps_a_numbers_decimals() {
        assert_eq!(cell_text(&Value::Float(1.234_567_89)), "1.23456789");
        assert_eq!(cell_text(&Value::Float(0.000_000_1)), "0.0000001");
        assert_eq!(cell_text(&Value::Float(42.0)), "42");
        assert_eq!(cell_text(&Value::Text("Alpha".into())), "Alpha");
        assert_eq!(cell_text(&Value::Null), "<Null>");
    }
}
