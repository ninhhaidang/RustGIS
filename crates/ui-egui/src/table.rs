//! The attribute table, docked under the map: sort, filter to the selection, select rows (synced
//! with the map) and edit values in place.

use std::collections::HashMap;

use egui::{Sense, Stroke, Ui, vec2};
use egui_extras::{Column, TableBuilder};
use rustgis_engine::data::{Dataset, Field, Value as V};
use serde_json::{Value, json};

use crate::theme::{Tokens, semibold};
use crate::widgets::small;
use crate::{RustGisApp, icons};

#[derive(Default)]
pub struct TableState {
    pub folder: Option<std::path::PathBuf>,
    sort: Option<(usize, bool)>,
    selected_only: bool,
    editing: Option<CellEdit>,
    /// The pass the cell editor last had the focus in (see [`had_focus`]).
    focus_pass: Option<u64>,
    filter: String,
    /// Which columns are hidden and in what order, per table (view only, kept for the session).
    views: HashMap<ViewKey, ColumnView>,
    /// The project the views are for ([`rustgis_engine::Session::opened`]).
    opened: u64,
}

/// A table's identity for its view: a live layer, by its instance (so a layer that takes a removed
/// one's id, after an undo or in another project, starts afresh), or a standalone table's id.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum ViewKey {
    Layer(u64),
    Table(u32),
}

/// Brings each column view up to date with its table's fields, and forgets those whose layer or
/// table is gone. Run after every command, so a field deleted and added again while the table is
/// closed still starts visible.
pub(crate) fn prune_views(app: &mut RustGisApp) {
    // Another project, however it was opened (the app, an agent, a script): its tables' ids start
    // again at 1, so no view carries over.
    if app.table.opened != app.session.opened {
        app.table.views.clear();
        app.table.opened = app.session.opened;
    }
    let project = &app.session.project;
    app.table.views.retain(|key, view| {
        let fields = match *key {
            ViewKey::Layer(instance) => {
                project.maps.iter().flat_map(|m| &m.layers).find(|l| l.instance_id() == instance).map(|l| &l.dataset.fields)
            }
            ViewKey::Table(id) => project.table(id).map(|t| &t.dataset.fields),
        };
        fields.map(|f| view.repair(f)).is_some()
    });
}

/// What a column's right-click menu asked for.
#[derive(Clone, Copy)]
enum ColumnAction {
    Hide,
    Left,
    Right,
}

/// The order and visibility of a table's field columns. Fields are kept by [`tag`] (name, and
/// which field of that name), not by index, so deleting or adding a field leaves the other columns'
/// state where it was. Fields missing from `order` (added since) follow at the end, in dataset
/// order; tags no longer in the dataset are dropped by [`ColumnView::repair`].
#[derive(Default, Clone, Debug, PartialEq)]
struct ColumnView {
    /// Fields' tags in display order.
    order: Vec<(String, usize)>,
    /// Hidden fields' tags.
    hidden: Vec<(String, usize)>,
}

/// Field `i`'s name, and how many fields of that name come before it: which of them it is, as a
/// file can repeat a name.
fn tag(fields: &[Field], i: usize) -> Option<(String, usize)> {
    let name = &fields.get(i)?.name;
    Some((name.clone(), fields.iter().take(i).filter(|f| &f.name == name).count()))
}

impl ColumnView {
    /// Forget fields that are gone, so a field added later under the same name starts visible.
    fn repair(&mut self, fields: &[Field]) {
        let has = |(n, k): &(String, usize)| fields.iter().filter(|f| &f.name == n).count() > *k;
        self.order.retain(has);
        self.hidden.retain(has);
    }

    /// Every field's index in display order, hidden ones included.
    fn layout(&self, fields: &[Field]) -> Vec<usize> {
        let tags: Vec<_> = (0..fields.len()).map(|i| tag(fields, i)).collect();
        let mut used = vec![false; fields.len()];
        let mut out = Vec::with_capacity(fields.len());
        for t in &self.order {
            if let Some(i) = tags.iter().position(|u| u.as_ref() == Some(t))
                && let Some(u) = used.get_mut(i)
                && !*u
            {
                *u = true;
                out.push(i);
            }
        }
        out.extend((0..fields.len()).filter(|&i| used.get(i) == Some(&false)));
        out
    }

    /// Whether field `i` is hidden.
    fn is_hidden(&self, fields: &[Field], i: usize) -> bool {
        tag(fields, i).is_some_and(|t| self.hidden.contains(&t))
    }

    /// The field indices to show, in display order.
    fn shown(&self, fields: &[Field]) -> Vec<usize> {
        self.layout(fields).into_iter().filter(|&i| !self.is_hidden(fields, i)).collect()
    }

    /// How many fields are hidden.
    fn hidden_count(&self, fields: &[Field]) -> usize {
        (0..fields.len()).filter(|&i| self.is_hidden(fields, i)).count()
    }

    /// Hides field `i`.
    fn hide(&mut self, fields: &[Field], i: usize) {
        if let Some(t) = tag(fields, i)
            && !self.hidden.contains(&t)
        {
            self.hidden.push(t);
        }
    }

    /// Shows every field again, keeping the order.
    fn show_all(&mut self) {
        self.hidden.clear();
    }

    /// The shown column `i` would swap places with (`left` or right), if any.
    fn neighbour(&self, fields: &[Field], i: usize, left: bool) -> Option<usize> {
        let shown = self.shown(fields);
        let p = shown.iter().position(|&c| c == i)?;
        if left { p.checked_sub(1).and_then(|q| shown.get(q)) } else { shown.get(p + 1) }.copied()
    }

    /// Moves column `i` past the shown column beside it (hidden ones keep their place).
    fn shift(&mut self, fields: &[Field], i: usize, left: bool) {
        let Some(j) = self.neighbour(fields, i, left) else { return };
        let mut full = self.layout(fields);
        let (Some(a), Some(b)) = (full.iter().position(|&c| c == i), full.iter().position(|&c| c == j)) else { return };
        full.swap(a, b);
        self.order = full.into_iter().filter_map(|c| tag(fields, c)).collect();
    }
}

/// Whether a cell was being typed in during the pass before `pass`: egui takes the focus away on
/// Escape before the shortcuts run, and that Escape cancels the cell, not the tool.
pub fn had_focus(st: &TableState, pass: u64) -> bool {
    st.focus_pass.is_some_and(|p| p + 1 == pass)
}

/// A value being typed into a cell.
struct CellEdit {
    /// The layer the table showed: shown another (Ctrl+T), the edit is dropped.
    layer: Option<u32>,
    /// The feature's id, not its row: rows deleted meanwhile, it still finds its feature (or,
    /// gone, writes nothing).
    fid: u64,
    col: usize,
    text: String,
    /// The text it opened with: left as it was, nothing is written (a null stays null).
    was: String,
    /// Just opened: the editor takes the keyboard focus this frame. Taken every frame, Enter
    /// couldn't let it go and nothing was ever kept (#1182).
    focus: bool,
}

impl CellEdit {
    fn new(layer: Option<u32>, fid: u64, col: usize, text: String) -> CellEdit {
        CellEdit { layer, fid, col, was: text.clone(), text, focus: true }
    }
}

pub fn show(app: &mut RustGisApp, ui: &mut Ui) {
    let layer_id = app.session.table_layer;
    // An edit is the layer's it was opened on: the table closed (however) or showing another
    // layer, it is dropped rather than written later to whatever row has its place.
    if app.table.editing.as_ref().is_some_and(|e| e.layer != layer_id) {
        app.table.editing = None;
    }
    if layer_id.is_none() && app.session.table_standalone.is_none() {
        return;
    }
    let t = Tokens::get(ui.ctx());
    let pass = ui.ctx().cumulative_pass_nr();
    egui::Panel::bottom("attribute_table")
        .resizable(true)
        .default_size(250.0)
        .size_range(120.0..=600.0)
        .frame(egui::Frame::NONE.fill(t.pane))
        .show(ui, |ui| {
            // Data source: a layer or a standalone table.
            let (ds, selection, title, key): (Dataset, Vec<usize>, String, ViewKey) =
                match layer_id.and_then(|id| app.session.map().layer(id)) {
                    Some(l) => (l.dataset.clone(), l.selection.clone(), l.name.clone(), ViewKey::Layer(l.instance_id())),
                    None => match app.session.table_standalone.and_then(|id| app.session.project.table(id)) {
                        Some(tb) => (tb.dataset.clone(), Vec::new(), tb.name.clone(), ViewKey::Table(tb.id)),
                        None => {
                            app.session.table_standalone = None;
                            return;
                        }
                    },
                };
            let is_layer = layer_id.is_some();
            prune_views(app);
            let view = app.table.views.entry(key).or_default();
            view.repair(&ds.fields);
            let shown = view.shown(&ds.fields);
            let hidden_n = view.hidden_count(&ds.fields);
            let mut show_all = false;
            // Header.
            let (hr, _) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::hover());
            ui.painter().rect_filled(hr, 0.0, t.pane_header);
            ui.painter().line_segment([egui::pos2(hr.min.x, hr.min.y), egui::pos2(hr.max.x, hr.min.y)], Stroke::new(1.0, t.border_strong));
            let mut header_ui = ui.new_child(
                egui::UiBuilder::new().max_rect(hr.shrink2(vec2(10.0, 4.0))).layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            {
                let ui = &mut header_ui;
                let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
                icons::paint(ui.painter(), r, "table", t.icon, t.accent);
                ui.label(egui::RichText::new(&title).font(semibold(13.0)));
                ui.label(
                    egui::RichText::new(crate::i18n::fmt(
                        "{n} of {total} selected",
                        &[("n", &selection.len().to_string()), ("total", &ds.features.len().to_string())],
                    ))
                    .size(11.5)
                    .color(t.text_dim),
                );
                if hidden_n > 0
                    && ui
                        .add(egui::Button::new(
                            egui::RichText::new(crate::i18n::fmt("Hidden columns: {n}", &[("n", &hidden_n.to_string())]))
                                .size(11.5)
                                .color(t.accent),
                        ))
                        .on_hover_text(tl!("Show All Columns"))
                        .clicked()
                {
                    show_all = true;
                }
                ui.add_space(10.0);
                if is_layer {
                    small(
                        ui,
                        app,
                        "select_attr",
                        "Select By Attributes",
                        "Select rows with a SQL expression.",
                        "ui.dialog",
                        json!({"name": "selectByAttributes"}),
                        false,
                    );
                    small(ui, app, "zoom_sel", "", "Zoom to selection", "select.zoomTo", Value::Null, false);
                    small(ui, app, "select", "", "Switch selection", "select.switch", Value::Null, false);
                    small(ui, app, "clear_sel", "", "Clear selection", "select.clear", Value::Null, false);
                    small(ui, app, "delete", "", "Delete selected rows", "edit.delete", Value::Null, false);
                    small(ui, app, "field", "", "Add field", "ui.dialog", json!({"name": "addField"}), false);
                    small(ui, app, "calculate", "", "Calculate field", "ui.dialog", json!({"name": "calculateField"}), false);
                    // The table's layer, even when another layer is active in Layers.
                    small(
                        ui,
                        app,
                        "calc_geom",
                        "",
                        "Calculate Geometry",
                        "ui.dialog",
                        json!({"name": "calculateGeometry", "layer": layer_id}),
                        false,
                    );
                    ui.add_space(6.0);
                    ui.checkbox(&mut app.table.selected_only, egui::RichText::new(tl!("Selected only")).size(11.5));
                }
                ui.add(egui::TextEdit::singleline(&mut app.table.filter).hint_text(tl!("Filter rows…")).desired_width(160.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(egui::Button::new(tl!("×")).frame(false)).on_hover_text(tl!("Close table")).clicked() {
                        app.session.table_layer = None;
                        app.session.table_standalone = None;
                        app.table.editing = None;
                    }
                });
            }
            // Rows to show.
            let mut rows: Vec<usize> =
                if app.table.selected_only && is_layer { selection.clone() } else { (0..ds.features.len()).collect() };
            let f = app.table.filter.trim().to_lowercase();
            if !f.is_empty() {
                rows.retain(|r| ds.features.get(*r).is_some_and(|ft| ft.attrs.iter().any(|v| v.to_string().to_lowercase().contains(&f))));
            }
            if let Some((c, asc)) = app.table.sort {
                rows.sort_by(|a, b| {
                    let o = ds.value(*a, c).partial_cmp(ds.value(*b, c)).unwrap_or(std::cmp::Ordering::Equal);
                    if asc { o } else { o.reverse() }
                });
            }
            let mut clicked: Option<(usize, bool)> = None;
            let mut commits: Vec<(u64, usize, String, String)> = Vec::new();
            // Whether the cell being typed in was laid out (its row scrolled into view).
            let mut drawn = false;
            let mut cancel: Option<(u64, usize)> = None;
            let mut sort_click: Option<usize> = None;
            let mut column_action: Option<(usize, ColumnAction)> = None;
            egui::ScrollArea::horizontal().show(ui, |ui| {
                let mut tb = TableBuilder::new(ui)
                    .striped(true)
                    .resizable(true)
                    .sense(Sense::click())
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                    .column(Column::initial(90.0).at_least(54.0));
                for _ in &shown {
                    tb = tb.column(Column::initial(120.0).at_least(50.0).clip(true));
                }
                tb.min_scrolled_height(0.0)
                    .header(24.0, |mut h| {
                        h.col(|ui| {
                            ui.label(egui::RichText::new(tl!("Feature ID")).font(semibold(11.5)).color(t.text_dim))
                                .on_hover_text(tl!("Feature identifier, not a dataset field or a row position."));
                        });
                        for &i in &shown {
                            h.col(|ui| {
                                let Some(fd) = ds.fields.get(i) else { return };
                                let arrow = match app.table.sort {
                                    Some((c, true)) if c == i => " ↑",
                                    Some((c, false)) if c == i => " ↓",
                                    _ => "",
                                };
                                let r = ui.add(
                                    egui::Label::new(egui::RichText::new(format!("{}{arrow}", fd.display())).font(semibold(11.5)))
                                        .sense(Sense::click()),
                                );
                                let r = r.on_hover_text(crate::i18n::fmt(
                                    "{field} ({type}) · click to sort, right-click for columns",
                                    &[("field", &fd.name), ("type", tl_raw!(fd.ty.label()))],
                                ));
                                if r.clicked() {
                                    sort_click = Some(i);
                                }
                                r.context_menu(|ui| {
                                    if ui.button(tl!("Hide Column")).clicked() {
                                        column_action = Some((i, ColumnAction::Hide));
                                        ui.close();
                                    }
                                    let can = |left| app.table.views.get(&key).is_some_and(|v| v.neighbour(&ds.fields, i, left).is_some());
                                    if ui.add_enabled(can(true), egui::Button::new(tl!("Move Left"))).clicked() {
                                        column_action = Some((i, ColumnAction::Left));
                                        ui.close();
                                    }
                                    if ui.add_enabled(can(false), egui::Button::new(tl!("Move Right"))).clicked() {
                                        column_action = Some((i, ColumnAction::Right));
                                        ui.close();
                                    }
                                    ui.separator();
                                    if ui.add_enabled(hidden_n > 0, egui::Button::new(tl!("Show All Columns"))).clicked() {
                                        show_all = true;
                                        ui.close();
                                    }
                                });
                            });
                        }
                    })
                    .body(|body| {
                        body.rows(22.0, rows.len(), |mut row| {
                            let Some(&r) = rows.get(row.index()) else { return };
                            let sel = selection.contains(&r);
                            row.set_selected(sel);
                            let oid = ds.features.get(r).map(|f| f.id).unwrap_or(0);
                            row.col(|ui| {
                                ui.label(egui::RichText::new(oid.to_string()).size(11.5).color(t.text_dim));
                            });
                            for &c in &shown {
                                row.col(|ui| {
                                    let editing = app.table.editing.as_ref().is_some_and(|e| e.fid == oid && e.col == c);
                                    if editing {
                                        if let Some(e) = app.table.editing.as_mut() {
                                            drawn = true;
                                            // Named for its field (a screen reader says what it edits; an empty cell shows it).
                                            let name = ds.fields.get(c).map(|f| f.name.as_str()).unwrap_or_default();
                                            let te = ui
                                                .add(egui::TextEdit::singleline(&mut e.text).hint_text(name).desired_width(f32::INFINITY));
                                            if std::mem::take(&mut e.focus) {
                                                te.request_focus();
                                            }
                                            if te.has_focus() {
                                                app.table.focus_pass = Some(pass);
                                            }
                                            // Enter, Tab or a click elsewhere keeps the value; Escape
                                            // leaves the cell as it was (the shortcuts leave it to the
                                            // cell, see `had_focus`).
                                            if te.lost_focus() {
                                                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                                    cancel = Some((oid, c));
                                                } else {
                                                    commits.push((oid, c, e.text.clone(), e.was.clone()));
                                                }
                                            }
                                        }
                                    } else {
                                        let v = ds.value(r, c);
                                        // A picture (a photo's thumbnail) is named, not spelled out.
                                        let picture = matches!(v, V::Text(s) if crate::mapview::is_picture(s));
                                        let shown = if picture { tl!("picture").to_string() } else { v.to_string() };
                                        let txt = egui::RichText::new(shown).size(11.5);
                                        let txt = if matches!(v, V::Null) || picture { txt.color(t.text_disabled).italics() } else { txt };
                                        let resp = ui.add(egui::Label::new(txt).sense(Sense::click()).truncate());
                                        if resp.double_clicked() && is_layer {
                                            // The cell being typed in is kept first (once: its
                                            // editor may have kept it on losing the focus).
                                            if let Some(e) = app.table.editing.take()
                                                && !commits.iter().any(|q| q.0 == e.fid && q.1 == e.col)
                                                && cancel != Some((e.fid, e.col))
                                            {
                                                commits.push((e.fid, e.col, e.text, e.was));
                                            }
                                            let text = if matches!(v, V::Null) { String::new() } else { v.to_string() };
                                            app.table.editing = Some(CellEdit::new(layer_id, oid, c, text));
                                        } else if resp.clicked() {
                                            clicked = Some((r, ui.input(|i| i.modifiers.command || i.modifiers.shift)));
                                        }
                                    }
                                });
                            }
                            if row.response().clicked() {
                                clicked = Some((r, row.response().ctx.input(|i| i.modifiers.command || i.modifiers.shift)));
                            }
                        });
                    });
            });
            if let Some(i) = sort_click {
                app.table.sort = match app.table.sort {
                    Some((c, true)) if c == i => Some((i, false)),
                    Some((c, false)) if c == i => None,
                    _ => Some((i, true)),
                };
            }
            if let Some(view) = app.table.views.get_mut(&key) {
                match column_action {
                    Some((i, ColumnAction::Hide)) => {
                        view.hide(&ds.fields, i);
                        // Rows aren't left ordered by a column that can't be seen.
                        if app.table.sort.is_some_and(|(c, _)| c == i) {
                            app.table.sort = None;
                        }
                    }
                    Some((i, ColumnAction::Left)) => view.shift(&ds.fields, i, true),
                    Some((i, ColumnAction::Right)) => view.shift(&ds.fields, i, false),
                    None => {}
                }
                if show_all {
                    view.show_all();
                }
            }
            if let Some((r, add)) = clicked
                && is_layer
            {
                let mode = if add { if selection.contains(&r) { "remove" } else { "add" } } else { "new" };
                let _ = app.run("select.rows", json!({"layer": layer_id, "rows": [r], "mode": mode}));
            }
            // A cell scrolled out of view loses the focus unseen: kept, as a click elsewhere (left
            // as it was on Escape).
            if !drawn
                && let Some(e) = app.table.editing.take_if(|e| !e.focus)
                && !(had_focus(&app.table, pass) && ui.input(|i| i.key_pressed(egui::Key::Escape)))
            {
                commits.push((e.fid, e.col, e.text, e.was));
            }
            let ends = |editing: &Option<CellEdit>, fid: u64, c: usize| editing.as_ref().is_some_and(|e| e.fid == fid && e.col == c);
            if let Some((fid, c)) = cancel
                && ends(&app.table.editing, fid, c)
            {
                app.table.editing = None;
            }
            for (fid, c, text, was) in commits {
                if ends(&app.table.editing, fid, c) {
                    app.table.editing = None;
                }
                // Its feature where it is now, in the layer as it is (the table's own Delete runs
                // after `ds` was read); gone, nothing is written.
                let live =
                    layer_id.and_then(|id| app.session.map().layer(id)).and_then(|l| l.dataset.features.iter().position(|f| f.id == fid));
                let (Some(fd), Some(r)) = (ds.fields.get(c), live) else { continue };
                if text == was {
                    continue;
                }
                // Text that isn't a value of the field (a number mistyped) is refused, not
                // written as null or false.
                if !fits(&text, fd.ty) {
                    let msg = crate::i18n::fmt(
                        "“{text}” isn't a {type} value: {field} is left as it was.",
                        &[("text", text.trim()), ("type", tl_raw!(fd.ty.label())), ("field", &fd.name)],
                    );
                    app.session.notify(msg, true);
                    continue;
                }
                // Refused (editing turned off, a domain or rule): said, unless the command said so.
                let mark = app.session.notice_mark();
                if let Err(e) = app.run("edit.setValue", json!({"layer": layer_id, "row": r, "field": fd.name, "value": text}))
                    && app.session.notices_since(mark).is_empty()
                {
                    app.session.notify(e, true);
                }
            }
        });
}

/// Whether `text` reads as a value of a `ty` field (`Value::parse`'s reading): blank is null;
/// a number that doesn't parse (or isn't finite, or for a whole number is out of range), or a
/// word that isn't yes or no, isn't.
fn fits(text: &str, ty: rustgis_engine::data::FieldType) -> bool {
    use rustgis_engine::data::FieldType as T;
    let t = text.trim();
    t.is_empty()
        || t.eq_ignore_ascii_case("<null>")
        || match ty {
            T::Text => true,
            // A whole number, also written as a float (`1e3`), in range (`Value::parse` would
            // truncate a fraction or saturate past the range).
            T::Integer => {
                t.parse::<i64>().is_ok() || t.parse::<f64>().is_ok_and(|f| f.fract() == 0.0 && f >= i64::MIN as f64 && f < i64::MAX as f64)
            }
            T::Double => t.parse::<f64>().is_ok_and(f64::is_finite),
            T::Boolean => matches!(t.to_ascii_lowercase().as_str(), "true" | "false" | "yes" | "no" | "1" | "0" | "y" | "n"),
        }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::Queryable;

    /// A value typed into a cell is kept on Enter, and Escape leaves the cell as it was (not the
    /// tool: the shortcuts run first, egui has dropped the focus on Escape by then). The editor
    /// took the focus back every frame, so Enter never ended it and nothing was kept (#1182).
    #[test]
    fn a_value_typed_into_a_cell_is_kept_on_enter() {
        let mut app = RustGisApp::with_sample();
        let id = app.session.map().layers.first().map(|l| l.id).unwrap();
        app.session.table_layer = Some(id);
        app.run("tool.set", json!({"tool": "select"})).unwrap();
        let ds = &app.session.map().layer(id).unwrap().dataset;
        let col = (0..ds.fields.len()).find(|&c| matches!(ds.value(0, c), V::Text(_))).expect("a text field");
        let num = (0..ds.fields.len()).find(|&c| matches!(ds.value(0, c), V::Int(_) | V::Float(_))).expect("a number field");
        let fid: Vec<u64> = ds.features.iter().map(|f| f.id).collect();
        let value = |app: &RustGisApp, row: usize| app.session.map().layer(id).unwrap().dataset.value(row, col).to_string();
        let (first, second) = (value(&app, 0), value(&app, 1));
        let number = app.session.map().layer(id).unwrap().dataset.value(0, num).to_string();
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1400.0, 1000.0)).with_max_steps(64).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app,
        );
        h.run_steps(4);
        let tool = h.state().session.tool;
        // Opened as a double-click opens it, the text selected by nothing: typing adds to it.
        h.state_mut().table.editing = Some(CellEdit::new(Some(id), fid[0], col, first.clone()));
        h.run_steps(2);
        h.event(egui::Event::Text("Zed".into()));
        h.run_steps(2);
        h.key_press(egui::Key::Enter);
        h.run_steps(2);
        assert_eq!(value(h.state(), 0), format!("{first}Zed"));
        assert!(h.state().table.editing.is_none(), "the editor closed");
        // Escape: typed, then left as it was, the tool kept.
        h.state_mut().table.editing = Some(CellEdit::new(Some(id), fid[1], col, second.clone()));
        h.run_steps(2);
        h.event(egui::Event::Text("Nope".into()));
        h.run_steps(2);
        h.key_press(egui::Key::Escape);
        h.run_steps(2);
        assert_eq!(value(h.state(), 1), second);
        assert!(h.state().table.editing.is_none(), "the editor closed");
        assert_eq!(h.state().session.tool, tool, "Escape left the cell, not the tool");
        // Shown another layer meanwhile (Ctrl+T), an open edit is dropped, not written there;
        // so is one left open when the table closes.
        h.state_mut().table.editing = Some(CellEdit::new(Some(id + 1000), fid[0], col, String::new()));
        h.run_steps(2);
        assert!(h.state().table.editing.is_none());
        // A number mistyped is refused, not written as null.
        h.state_mut().table.editing = Some(CellEdit::new(Some(id), fid[0], num, String::new()));
        h.run_steps(2);
        h.event(egui::Event::Text("12a".into()));
        h.run_steps(2);
        h.key_press(egui::Key::Enter);
        h.run_steps(2);
        assert_eq!(h.state().session.map().layer(id).unwrap().dataset.value(0, num).to_string(), number);
        assert!(h.state().table.editing.is_none());
        // With editing turned off, a value isn't kept, and the table says why.
        h.state_mut().session.map_mut().editing.settings.enabled = false;
        let mark = h.state().session.notice_mark();
        h.state_mut().table.editing = Some(CellEdit::new(Some(id), fid[0], col, String::new()));
        h.run_steps(2);
        h.event(egui::Event::Text("Off".into()));
        h.run_steps(2);
        h.key_press(egui::Key::Enter);
        h.run_steps(2);
        assert_eq!(value(h.state(), 0), format!("{first}Zed"));
        assert!(h.state().session.notices_since(mark).iter().any(|n| n.text.contains("editing is turned off")), "said why");
        h.state_mut().session.map_mut().editing.settings.enabled = true;
        h.state_mut().table.editing = Some(CellEdit::new(Some(id), fid[0], col, String::new()));
        h.state_mut().session.table_layer = None;
        h.run_steps(2);
        assert!(h.state().table.editing.is_none(), "dropped when the table closed");
    }

    fn fields(names: &[&str]) -> Vec<Field> {
        names.iter().map(|n| Field::new(*n, rustgis_engine::data::FieldType::Text)).collect()
    }

    fn names(fields: &[Field], idx: &[usize]) -> Vec<String> {
        idx.iter().filter_map(|&i| fields.get(i)).map(|f| f.name.clone()).collect()
    }

    #[test]
    fn columns_hide_reorder_and_show_all() {
        let fs = fields(&["a", "b", "c", "d"]);
        let mut v = ColumnView::default();
        assert_eq!(v.shown(&fs), vec![0, 1, 2, 3]);
        // The ends can't move outward.
        assert_eq!(v.neighbour(&fs, 0, true), None);
        assert_eq!(v.neighbour(&fs, 3, false), None);
        v.shift(&fs, 2, true);
        assert_eq!(names(&fs, &v.shown(&fs)), ["a", "c", "b", "d"]);
        v.shift(&fs, 0, false);
        assert_eq!(names(&fs, &v.shown(&fs)), ["c", "a", "b", "d"]);
        // A hidden column is skipped, and keeps its place among the others.
        v.hide(&fs, 0);
        v.hide(&fs, 0);
        assert_eq!(v.hidden_count(&fs), 1);
        assert_eq!(names(&fs, &v.shown(&fs)), ["c", "b", "d"]);
        v.shift(&fs, 1, true);
        assert_eq!(names(&fs, &v.shown(&fs)), ["b", "c", "d"]);
        v.shift(&fs, 1, false);
        v.show_all();
        assert_eq!(v.hidden_count(&fs), 0);
        assert_eq!(names(&fs, &v.layout(&fs)), ["c", "a", "b", "d"]);
    }

    #[test]
    fn columns_survive_added_and_deleted_fields() {
        let mut fs = fields(&["a", "b", "c"]);
        let mut v = ColumnView::default();
        v.hide(&fs, 1);
        v.shift(&fs, 2, true);
        v.shift(&fs, 0, false);
        assert_eq!(names(&fs, &v.shown(&fs)), ["c", "a"]);
        // A field deleted before the hidden one: "b" stays hidden, the rest keep their order.
        fs.remove(0);
        v.repair(&fs);
        assert_eq!(names(&fs, &v.shown(&fs)), ["c"]);
        assert_eq!(v.hidden_count(&fs), 1);
        // A new field shows, at the end.
        fs.push(Field::new("e", rustgis_engine::data::FieldType::Text));
        v.repair(&fs);
        assert_eq!(names(&fs, &v.shown(&fs)), ["c", "e"]);
        // A deleted then re-added field starts visible, not hidden again.
        fs.remove(0);
        v.repair(&fs);
        fs.insert(0, Field::new("b", rustgis_engine::data::FieldType::Text));
        v.repair(&fs);
        assert_eq!(v.hidden_count(&fs), 0);
        assert_eq!(names(&fs, &v.shown(&fs)), ["c", "b", "e"]);
    }

    /// Two fields of one name, as a file can have, are hidden apart.
    #[test]
    fn columns_of_one_name_hide_apart() {
        let fs = fields(&["a", "a", "b"]);
        let mut v = ColumnView::default();
        v.hide(&fs, 1);
        assert_eq!(v.shown(&fs), vec![0, 2]);
        assert_eq!(v.hidden_count(&fs), 1);
        // With the second gone, the one left is shown.
        let fs = fields(&["a", "b"]);
        v.repair(&fs);
        assert_eq!(v.shown(&fs), vec![0, 1]);
    }

    /// Two fields of one name move apart, and a third added later follows them, not before.
    #[test]
    fn columns_of_one_name_move_apart() {
        let mut fs = fields(&["a", "a", "b"]);
        let mut v = ColumnView::default();
        v.shift(&fs, 1, true);
        assert_eq!(v.shown(&fs), vec![1, 0, 2]);
        v.shift(&fs, 1, false);
        v.shift(&fs, 2, true);
        assert_eq!(v.shown(&fs), vec![0, 2, 1]);
        fs.push(Field::new("a", rustgis_engine::data::FieldType::Text));
        v.repair(&fs);
        assert_eq!(v.shown(&fs), vec![0, 2, 1, 3]);
        // The last one gone, the others keep their places.
        fs.pop();
        fs.remove(1);
        v.repair(&fs);
        assert_eq!(names(&fs, &v.shown(&fs)), ["a", "b"]);
    }

    /// A column view follows commands run while the table is closed, and belongs to its project:
    /// a field deleted and added again starts visible, a removed layer's view is forgotten, and
    /// another project starts with every column shown.
    #[test]
    fn column_views_follow_commands_and_projects() {
        let mut app = RustGisApp::with_sample();
        // The table follows this project from here.
        prune_views(&mut app);
        assert_eq!(app.session.table_layer, None, "the table is closed");
        let hidden = |app: &RustGisApp, name: &str| {
            let l = app.session.map().layers.iter().find(|l| l.name == name).unwrap();
            let fields = &l.dataset.fields;
            app.table.views.get(&ViewKey::Layer(l.instance_id())).map(|v| v.hidden_count(fields))
        };
        let hide = |app: &mut RustGisApp, name: &str, field: &str| {
            let l = app.session.map().layers.iter().find(|l| l.name == name).unwrap();
            let i = l.dataset.field_index(field).unwrap();
            let mut v = ColumnView::default();
            v.hide(&l.dataset.fields, i);
            app.table.views.insert(ViewKey::Layer(l.instance_id()), v);
        };
        hide(&mut app, "Countries", "pop_est");
        assert_eq!(hidden(&app, "Countries"), Some(1));
        app.run("edit.deleteField", json!({"layer": "Countries", "field": "pop_est"})).unwrap();
        app.run("edit.addField", json!({"layer": "Countries", "name": "pop_est", "type": "long"})).unwrap();
        assert_eq!(hidden(&app, "Countries"), Some(0), "added again, shown");
        // A removed layer's view goes with it.
        hide(&mut app, "Cities", "name");
        app.run("layer.remove", json!({"layer": "Cities"})).unwrap();
        assert_eq!(app.table.views.len(), 1, "only Countries' view is left");
        // Another project: its layers reuse the ids and names, but none of the views.
        hide(&mut app, "Countries", "pop_est");
        app.run("project.sample", json!({})).unwrap();
        assert!(app.table.views.is_empty());
        assert_eq!(hidden(&app, "Countries"), None);
    }

    /// A project opened past the app (an agent's or a script's command, run on the session) still
    /// leaves the old one's views behind: a standalone table there, under the same id and with the
    /// same fields, shows every column.
    #[test]
    fn column_views_are_forgotten_when_an_agent_opens_a_project() {
        let table = |app: &mut RustGisApp| {
            let fields = fields(&["a", "b"]);
            let id = app.session.project.add_table(Dataset::new("Notes", rustgis_engine::data::GeometryType::Point, fields.clone()));
            (id, fields)
        };
        let mut app = RustGisApp::with_sample();
        // The table follows this project from here.
        prune_views(&mut app);
        let (id, fs) = table(&mut app);
        let mut v = ColumnView::default();
        v.hide(&fs, 0);
        app.table.views.insert(ViewKey::Table(id), v);
        prune_views(&mut app);
        assert_eq!(app.table.views.get(&ViewKey::Table(id)).map(|v| v.hidden_count(&fs)), Some(1), "kept in its project");
        app.session.run("project.new", &json!({})).unwrap();
        let (again, _) = table(&mut app);
        assert_eq!(again, id, "the new project's table takes the same id");
        prune_views(&mut app);
        assert!(app.table.views.is_empty());
    }

    /// Typing into a cell of a reordered table, with a column hidden before it, writes the field
    /// shown there, not the one at that position in the dataset.
    #[test]
    fn a_cell_in_a_reordered_table_writes_its_own_field() {
        let mut app = RustGisApp::with_sample();
        // The table follows this project from here.
        prune_views(&mut app);
        let id = app.session.map().layers.first().map(|l| l.id).unwrap();
        app.session.table_layer = Some(id);
        let ds = &app.session.map().layer(id).unwrap().dataset;
        let text = |c: usize| (0..ds.features.len()).all(|r| matches!(ds.value(r, c), V::Text(_)));
        let col = (1..ds.fields.len()).find(|&c| text(c)).expect("a text field");
        let value = |app: &RustGisApp, c: usize| app.session.map().layer(id).unwrap().dataset.value(0, c).to_string();
        let before: Vec<String> = (0..ds.fields.len()).map(|c| value(&app, c)).collect();
        let fields = ds.fields.clone();
        // Hide field 0 and move `col` to the front.
        let mut v = ColumnView::default();
        v.hide(&fields, 0);
        while v.neighbour(&fields, col, true).is_some() {
            v.shift(&fields, col, true);
        }
        let shown = v.shown(&fields);
        assert_eq!(shown.first(), Some(&col));
        let instance = app.session.map().layer(id).map(|l| l.instance_id()).unwrap();
        app.table.views.insert(ViewKey::Layer(instance), v);
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1400.0, 1000.0)).with_max_steps(64).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app,
        );
        h.run_steps(4);
        // Double-click the first row's cell in the first column shown: the editor opens on the
        // field drawn there, not the dataset's first.
        let cell = h
            .query_all_by_label(&before[col])
            .map(|n| n.rect())
            .min_by(|a, b| (a.min.y, a.min.x).partial_cmp(&(b.min.y, b.min.x)).unwrap_or(std::cmp::Ordering::Equal))
            .expect("the cell is drawn");
        h.event(egui::Event::PointerMoved(cell.center()));
        for _ in 0..2 {
            for pressed in [true, false] {
                h.event(egui::Event::PointerButton {
                    pos: cell.center(),
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::default(),
                });
            }
        }
        h.run_steps(2);
        assert_eq!(h.state().table.editing.as_ref().map(|e| e.col), Some(col), "edits the field drawn there");
        h.event(egui::Event::Text("Zed".into()));
        h.run_steps(2);
        h.key_press(egui::Key::Enter);
        h.run_steps(2);
        for (c, was) in before.iter().enumerate() {
            let want = if c == col { format!("{was}Zed") } else { was.clone() };
            assert_eq!(value(h.state(), c), want, "field {c}");
        }
        // The view survives the frames, and the table drew only the shown columns.
        assert_eq!(h.state().table.views.get(&ViewKey::Layer(instance)).map(|v| v.shown(&fields)), Some(shown));
    }

    #[test]
    fn typed_text_fits_its_field() {
        use rustgis_engine::data::FieldType as T;
        for (text, ty, ok) in [
            ("12", T::Integer, true),
            ("1.5", T::Double, true),
            ("12a", T::Integer, false),
            ("1e3", T::Integer, true),
            ("-7", T::Integer, true),
            ("1e30", T::Integer, false),
            ("1.5", T::Integer, false),
            ("9.21e18", T::Integer, true),
            ("1e19", T::Integer, false),
            ("NaN", T::Integer, false),
            ("inf", T::Double, false),
            ("tru", T::Boolean, false),
            ("No", T::Boolean, true),
            ("", T::Double, true),
            (" <null> ", T::Integer, true),
            ("anything", T::Text, true),
        ] {
            assert_eq!(fits(text, ty), ok, "{text:?} as {ty:?}");
        }
    }
}
