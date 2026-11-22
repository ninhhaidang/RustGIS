//! The attribute table, docked under the map: sort, filter to the selection, select rows (synced
//! with the map) and edit values in place, a layer's or a standalone table's.

mod dates;

use std::collections::HashMap;
use std::sync::Weak;

use egui::{Sense, Stroke, Ui, vec2};
use egui_extras::{Column, TableBuilder};
use rustgis_engine::data::{Dataset, Feature, Field, Value as V};
use serde_json::{Value, json};

use crate::theme::{Tokens, semibold};
use crate::widgets::{small, small_for};
use crate::{RustGisApp, icons};

#[derive(Default)]
pub struct TableState {
    pub folder: Option<std::path::PathBuf>,
    /// A date conversion choice, tied to the rows it previews.
    dates: Option<dates::Dialog>,
    sort: Option<(usize, bool)>,
    /// The displayed schema that gives a positional sort its meaning, until a command changes it.
    sort_schema: Option<(ViewKey, Vec<Field>)>,
    selected_only: bool,
    editing: Option<CellEdit>,
    /// The pass the cell editor last had the focus in (see [`had_focus`]).
    focus_pass: Option<u64>,
    filter: String,
    /// The sorted or filtered row indices for the table on screen.
    row_order: Option<RowOrder>,
    /// Which columns are hidden and in what order, per table (view only, kept for the session).
    views: HashMap<ViewKey, ColumnView>,
    /// The project the views are for ([`rustgis_engine::Session::opened`]).
    opened: u64,
    /// The answer of Statistics… for a column, shown in a window until it is closed.
    stats: Option<ColumnStats>,
}

struct RowOrder {
    /// A weak key sees edits and replacements without keeping the old features alive.
    source: Weak<Vec<Feature>>,
    sort: Option<(usize, bool)>,
    filter: String,
    /// Selection participates only in the selected-only view.
    selection: Option<Vec<usize>>,
    indices: Vec<usize>,
}

impl RowOrder {
    /// Reuses the row indices until their inputs change. An unsorted, unfiltered view uses the
    /// selection slice directly, or returns None to display the dataset's rows in place.
    fn displayed<'a>(
        cache: &'a mut Option<Self>,
        ds: &Dataset,
        sort: Option<(usize, bool)>,
        filter: &str,
        selection: Option<&'a [usize]>,
    ) -> Option<&'a [usize]> {
        let filter = filter.trim().to_lowercase();
        if sort.is_none() && filter.is_empty() {
            *cache = None;
            return selection;
        }
        let source = ds.features.allocation_key();
        if cache.as_ref().is_none_or(|order| {
            !order.source.ptr_eq(&source) || order.sort != sort || order.filter != filter || order.selection.as_deref() != selection
        }) {
            let mut indices = selection.map_or_else(|| (0..ds.features.len()).collect::<Vec<_>>(), <[usize]>::to_vec);
            if !filter.is_empty() {
                indices.retain(|r| {
                    ds.features.get(*r).is_some_and(|ft| ft.attrs.iter().any(|v| v.to_string().to_lowercase().contains(&filter)))
                });
            }
            if let Some((c, asc)) = sort {
                indices.sort_by(|a, b| {
                    let o = ds.value(*a, c).partial_cmp(ds.value(*b, c)).unwrap_or(std::cmp::Ordering::Equal);
                    if asc { o } else { o.reverse() }
                });
            }
            *cache = Some(Self { source, sort, filter, selection: selection.map(<[usize]>::to_vec), indices });
        }
        cache.as_ref().map(|order| order.indices.as_slice())
    }
}

/// What `table.statistics` answered for a column of a layer, kept to show.
struct ColumnStats {
    /// The layer it was run on, by its id and by its instance (a layer that takes a removed one's
    /// id is another): shown another table, the window goes.
    layer: u32,
    instance: u64,
    /// The project and map it was run in ([`rustgis_engine::Session::opened`], the map's
    /// instance): a script or an agent can change them between two frames, so they are compared
    /// before the window shows.
    opened: u64,
    map: u64,
    /// The rows it counted, by their allocation ([`rustgis_engine::data::Features::allocation_key`]):
    /// an edit to a value, or a row added or deleted, stops it matching, so the window goes rather
    /// than show a sum the layer no longer has.
    rows: std::sync::Weak<Vec<Feature>>,
    /// Selected-only answers cease to match when the layer's selection changes.
    selection: Option<Vec<usize>>,
    answer: Value,
}

impl ColumnStats {
    /// Whether it is still of the table showing: the same project, map and layer instance, and the
    /// rows it counted.
    fn of(&self, app: &RustGisApp, layer: Option<u32>) -> bool {
        let live = layer.and_then(|id| app.session.map().layer(id));
        Some(self.layer) == layer
            && self.opened == app.session.opened
            && self.map == app.session.map().instance_id()
            && live.is_some_and(|l| {
                l.instance_id() == self.instance
                    && l.dataset.features.allocation_key().ptr_eq(&self.rows)
                    && self.selection.as_ref().is_none_or(|s| *s == l.selection)
            })
    }
}

impl RustGisApp {
    /// What the Statistics window holds, for tests.
    #[cfg(test)]
    pub(crate) fn table_stats_kept(&self) -> Option<&Value> {
        self.table.stats.as_ref().map(|s| &s.answer)
    }
}

/// Keeps what `table.statistics` answered for the window, once it has counted (on a thread of its
/// own on the desktop, so here as it lands). Shown only if the layer is still the one it counted.
pub(crate) fn statistics_landed(app: &mut RustGisApp, landed: crate::Landed) {
    // Said already if it failed or never ran.
    let crate::Landed::Ran(Ok(answer)) = landed else { return };
    let Some(layer) = answer.get("layerId").and_then(Value::as_u64).and_then(|id| u32::try_from(id).ok()) else { return };
    let Some(l) = app.session.map().layer(layer) else {
        return;
    };
    app.table.stats = Some(ColumnStats {
        layer,
        instance: l.instance_id(),
        opened: app.session.opened,
        map: app.session.map().instance_id(),
        rows: l.dataset.features.allocation_key(),
        selection: (answer.get("selected").and_then(Value::as_bool) == Some(true)).then(|| l.selection.clone()),
        answer,
    });
}

/// Asks `table.statistics` for a column and keeps what it gives the window. On the desktop the
/// count runs on a thread of its own and lands through [`statistics_landed`] (its start answers
/// `started`, which has no layer and is ignored); the browser, or a run not off the frame, gets the
/// answer back at once, so it is kept here too.
fn ask_statistics(app: &mut RustGisApp, asked: Value) {
    // Refused (a layer read in place, not all loaded): said, unless the command said so.
    let mark = app.session.notice_mark();
    match app.run("table.statistics", asked) {
        Ok(answer) => statistics_landed(app, crate::Landed::Ran(Ok(answer))),
        Err(e) if app.session.notices_since(mark).is_empty() => app.session.notify(e, true),
        Err(_) => {}
    }
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
        app.table.stats = None;
        app.table.editing = None;
        app.table.sort = None;
        app.table.sort_schema = None;
        app.table.row_order = None;
        app.table.opened = app.session.opened;
    }
    let project = &app.session.project;
    let fields_for = |key: ViewKey| match key {
        ViewKey::Layer(instance) => {
            project.maps.iter().flat_map(|m| &m.layers).find(|l| l.instance_id() == instance).map(|l| &l.dataset.fields)
        }
        ViewKey::Table(id) => project.table(id).map(|t| &t.dataset.fields),
    };
    app.table.views.retain(|key, view| fields_for(*key).map(|fields| view.repair(fields)).is_some());
    if let Some((key, schema)) = &app.table.sort_schema {
        let current = fields_for(*key);
        if current.is_none_or(|fields| fields != schema) {
            let key = *key;
            app.table.sort = None;
            app.table.sort_schema = current.map(|fields| (key, fields.clone()));
        }
    }
    if app.table.editing.as_ref().is_some_and(|edit| {
        fields_for(edit.view).is_none_or(|fields| fields != &edit.schema)
            || matches!(edit.view, ViewKey::Table(id) if project.table(id).is_none_or(|t| Some(t.instance_id()) != edit.table_instance))
    }) {
        app.table.editing = None;
    }
}

/// Why a column can't be changed, translated: the edit commands find a field by name, ignoring
/// case, and an earlier field has this one's name but for case.
fn shadowed() -> &'static str {
    tl!("An earlier field has this name but for case, and edits by name reach that one: this column can't be changed here.")
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
    /// The layer or table the view showed: shown another (Ctrl+T), the edit is dropped.
    view: ViewKey,
    /// The feature's id, not its row: rows deleted meanwhile, it still finds its feature (or,
    /// gone, writes nothing).
    fid: u64,
    col: usize,
    /// The schema this positional column belongs to. A schema change discards the draft before
    /// the old index can identify another field, including fields with duplicate names.
    schema: Vec<Field>,
    /// The standalone table contents this draft belongs to; ordinary edits preserve the identity.
    table_instance: Option<u64>,
    text: String,
    /// The text it opened with: left as it was, nothing is written (a null stays null).
    was: String,
    /// Just opened: the editor takes the keyboard focus this frame. Taken every frame, Enter
    /// couldn't let it go and nothing was ever kept (#1182).
    focus: bool,
}

impl CellEdit {
    /// An edit of column `col` in its opening schema, starting from `text` and taking focus.
    /// A schema change or table replacement discards it instead of redirecting the draft.
    fn new(view: ViewKey, fid: u64, col: usize, text: String, fields: &[Field], table_instance: Option<u64>) -> CellEdit {
        CellEdit { view, fid, col, table_instance, schema: fields.to_vec(), was: text.clone(), text, focus: true }
    }
}

/// The attribute table of the layer (`table_layer`) or standalone table (`table_standalone`)
/// shown, docked under the map: its rows, toolbar and cell editor. Edits run as commands.
pub fn show(app: &mut RustGisApp, ui: &mut Ui) {
    dates::show(app, ui.ctx());
    let layer_id = app.session.table_layer;
    // An edit is the layer's or table's it was opened on: the view closed (however) or showing
    // another, it is dropped rather than written later to whatever row has its place.
    let showing = match layer_id.and_then(|id| app.session.map().layer(id)) {
        Some(l) => Some(ViewKey::Layer(l.instance_id())),
        None => app.session.table_standalone.map(ViewKey::Table),
    };
    if app.table.editing.as_ref().is_some_and(|e| Some(e.view) != showing) {
        app.table.editing = None;
    }
    if layer_id.is_none() && app.session.table_standalone.is_none() {
        app.table.stats = None;
        app.table.row_order = None;
        return;
    }
    if app.table.stats.as_ref().is_some_and(|s| !s.of(app, layer_id)) {
        app.table.stats = None;
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
            let (ds, selection, title, key, table_instance): (Dataset, Vec<usize>, String, ViewKey, Option<u64>) =
                match layer_id.and_then(|id| app.session.map().layer(id)) {
                    Some(l) => (l.dataset.clone(), l.selection.clone(), l.name.clone(), ViewKey::Layer(l.instance_id()), None),
                    None => match app.session.table_standalone.and_then(|id| app.session.project.table(id)) {
                        Some(tb) => (tb.dataset.clone(), Vec::new(), tb.name.clone(), ViewKey::Table(tb.id), Some(tb.instance_id())),
                        None => {
                            app.session.table_standalone = None;
                            return;
                        }
                    },
                };
            let is_layer = layer_id.is_some();
            prune_views(app);
            if app.table.sort_schema.as_ref().is_none_or(|(view, fields)| *view != key || *fields != ds.fields) {
                app.table.sort = None;
                app.table.sort_schema = Some((key, ds.fields.clone()));
            }
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
                } else if let ViewKey::Table(id) = key {
                    // A standalone table: rows and fields, judged as the commands judge them.
                    let add_row = json!({"table": id});
                    let why = app.session.why_blocked("edit.addFeature", &add_row);
                    small_for(ui, app, "new", "", "Add row", "edit.addFeature", add_row, why);
                    small(ui, app, "field", "", "Add field", "ui.dialog", json!({"name": "addField", "table": id}), false);
                    small(ui, app, "calculate", "", "Calculate field", "ui.dialog", json!({"name": "calculateField", "table": id}), false);
                    ui.add_space(6.0);
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
            let rows = RowOrder::displayed(
                &mut app.table.row_order,
                &ds,
                app.table.sort,
                &app.table.filter,
                (app.table.selected_only && is_layer).then_some(selection.as_slice()),
            );
            let row_count = rows.map_or(ds.features.len(), <[usize]>::len);
            let mut clicked: Option<(usize, bool)> = None;
            let mut commits: Vec<(u64, usize, String, String)> = Vec::new();
            // Whether the cell being typed in was laid out (its row scrolled into view).
            let mut drawn = false;
            let mut cancel: Option<(u64, usize)> = None;
            let mut sort_click: Option<usize> = None;
            let mut column_action: Option<(usize, ColumnAction)> = None;
            // A text field to convert to dates ("date") or date-times ("datetime").
            let mut convert: Option<(usize, &str)> = None;
            // A field to show the statistics of.
            let mut statistics: Option<usize> = None;
            // A field to delete.
            let mut delete_field: Option<usize> = None;
            // A cell double-clicked in a column whose name finds another field.
            let mut by_other_name = false;
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
                                    if is_layer {
                                        ui.separator();
                                        if ui.button(tl!("Statistics…")).clicked() {
                                            statistics = Some(i);
                                            ui.close();
                                        }
                                    }
                                    // The edit commands find a field by name, ignoring case: a
                                    // column whose name finds another one isn't changed by name.
                                    let named = ds.field_index(&fd.name) == Some(i);
                                    if fd.ty == rustgis_engine::data::FieldType::Text {
                                        ui.separator();
                                        if ui
                                            .add_enabled(named, egui::Button::new(tl!("Convert to Date")))
                                            .on_disabled_hover_text(shadowed())
                                            .clicked()
                                        {
                                            convert = Some((i, "date"));
                                            ui.close();
                                        }
                                        if ui
                                            .add_enabled(named, egui::Button::new(tl!("Convert to Date and Time")))
                                            .on_disabled_hover_text(shadowed())
                                            .clicked()
                                        {
                                            convert = Some((i, "datetime"));
                                            ui.close();
                                        }
                                    }
                                    // A layer's fields are deleted from Data › Data Design; a
                                    // table has no other place to delete them.
                                    if !is_layer {
                                        ui.separator();
                                        if ui
                                            .add_enabled(named, egui::Button::new(tl!("Delete Field")))
                                            .on_disabled_hover_text(shadowed())
                                            .clicked()
                                        {
                                            delete_field = Some(i);
                                            ui.close();
                                        }
                                    }
                                });
                            });
                        }
                    })
                    .body(|body| {
                        body.rows(22.0, row_count, |mut row| {
                            let r = match rows {
                                Some(rows) => {
                                    let Some(&index) = rows.get(row.index()) else { return };
                                    index
                                }
                                None => row.index(),
                            };
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
                                        if resp.double_clicked() && ds.fields.get(c).is_some_and(|f| ds.field_index(&f.name) != Some(c)) {
                                            // Its value would be written to the field its name finds.
                                            by_other_name = true;
                                        } else if resp.double_clicked() {
                                            // The cell being typed in is kept first (once: its
                                            // editor may have kept it on losing the focus).
                                            if let Some(e) = app.table.editing.take()
                                                && !commits.iter().any(|q| q.0 == e.fid && q.1 == e.col)
                                                && cancel != Some((e.fid, e.col))
                                            {
                                                commits.push((e.fid, e.col, e.text, e.was));
                                            }
                                            let text = if matches!(v, V::Null) { String::new() } else { v.to_string() };
                                            app.table.editing = Some(CellEdit::new(key, oid, c, text, &ds.fields, table_instance));
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
            if let Some(i) = statistics
                && let Some(fd) = ds.fields.get(i)
                && let Some(layer) = layer_id
            {
                // By the column's place, so a second column of the same name is its own.
                let asked = json!({"layer": layer, "field": fd.name, "index": i, "selected": app.table.selected_only});
                ask_statistics(app, asked);
            }
            // The layer or table the view shows, as the edit commands name it.
            let target = |mut p: Value| {
                match key {
                    ViewKey::Layer(_) => p["layer"] = json!(layer_id),
                    ViewKey::Table(id) => p["table"] = json!(id),
                }
                p
            };
            if let Some((i, ty)) = convert
                && let Some(fd) = ds.fields.get(i)
            {
                let params = target(json!({"field": fd.name, "type": ty}));
                if is_layer && layer_id.and_then(|id| app.session.map().layer(id)).is_some_and(|l| l.is_streamed()) {
                    // Use the ordinary command admission refusal before scanning a view sample.
                    let _ = app.run("edit.convertField", params);
                } else {
                    app.table.dates = Some(dates::Dialog::new(key, table_instance, ds.clone(), i, ty, params));
                }
            }
            if by_other_name {
                app.session.notify(shadowed().to_string(), true);
            }
            if let Some(fd) = delete_field.and_then(|i| ds.fields.get(i)) {
                // Refused (a definition query reading it): said, unless the command said so.
                let mark = app.session.notice_mark();
                if let Err(e) = app.run("edit.deleteField", target(json!({"field": fd.name})))
                    && app.session.notices_since(mark).is_empty()
                {
                    app.session.notify(e, true);
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
                // Its feature where it is now, in the layer or table as it is (the table's own Delete
                // runs after `ds` was read); gone, nothing is written.
                let rows = match key {
                    ViewKey::Layer(_) => layer_id.and_then(|id| app.session.map().layer(id)).map(|l| &l.dataset.features),
                    ViewKey::Table(id) => app.session.project.table(id).map(|t| &t.dataset.features),
                };
                let live = rows.and_then(|rows| rows.iter().position(|f| f.id == fid));
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
                if let Err(e) = app.run("edit.setValue", target(json!({"row": r, "field": fd.name, "value": text})))
                    && app.session.notices_since(mark).is_empty()
                {
                    app.session.notify(e, true);
                }
            }
        });
    statistics_window(app, ui.ctx());
}

/// The window with a column's statistics, until it is closed.
fn statistics_window(app: &mut RustGisApp, ctx: &egui::Context) {
    let Some(st) = app.table.stats.as_ref() else { return };
    let (title, covers, lines) = (stats_title(&st.answer), stats_coverage(&st.answer), stats_lines(&st.answer));
    let mut open = true;
    egui::Window::new(tl!("Column Statistics"))
        .id(egui::Id::unique("column_statistics"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.label(egui::RichText::new(title).strong());
            ui.label(egui::RichText::new(covers).weak());
            ui.add_space(4.0);
            egui::Grid::new("column_statistics_grid").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
                for (name, value, _) in &lines {
                    ui.label(name);
                    ui.label(egui::RichText::new(value).monospace());
                    ui.end_row();
                }
            });
            ui.add_space(4.0);
            if ui.button(tl!("Copy")).clicked() {
                // The values as the answer holds them, not as the window rounds them, for a spreadsheet.
                ui.ctx().copy_text(stats_tsv(&lines));
            }
        });
    if !open {
        app.table.stats = None;
    }
}

/// The statistics as tab-separated lines, one per statistic: a tab or line break inside a name
/// or value (a text column's minimum, say) becomes a space so it can't split the pasted table.
fn stats_tsv(lines: &[(String, String, String)]) -> String {
    let cell = |s: &str| s.replace(['\t', '\r', '\n'], " ");
    let text: Vec<String> = lines.iter().map(|(n, _, full)| format!("{}\t{}", cell(n), cell(full))).collect();
    text.join("\n")
}

/// The window's first line: the layer and the field.
fn stats_title(answer: &Value) -> String {
    let text = |k: &str| answer.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    crate::i18n::fmt("{layer} › {field}", &[("layer", &text("layer")), ("field", &text("field"))])
}

/// Which rows the statistics cover: all of them, or the selected ones.
fn stats_coverage(answer: &Value) -> String {
    let n = answer.get("rows").and_then(Value::as_u64).unwrap_or(0);
    let selected = answer.get("selected").and_then(Value::as_bool).unwrap_or(false);
    let count = fmt_stat(n as f64);
    let args = [("n", count.as_str())];
    match (selected, n) {
        (true, 1) => crate::i18n::fmt("The 1 selected row", &args),
        (true, _) => crate::i18n::fmt("The {n} selected rows", &args),
        (false, 1) => crate::i18n::fmt("All 1 row", &args),
        (false, _) => crate::i18n::fmt("All {n} rows", &args),
    }
}

/// The rows of the statistics window: each statistic's name, its value as the window shows it, and
/// its value as the answer holds it, in full, for the copy. The field's kind decides the rows (a
/// text or date field has no sum or mean); the values left out, when there are any, last.
fn stats_lines(answer: &Value) -> Vec<(String, String, String)> {
    let number = answer.get("kind").and_then(Value::as_str) == Some("number");
    let count = |k: &str| fmt_stat(answer.get(k).and_then(Value::as_f64).unwrap_or(0.0));
    let value = |k: &str| match answer.get(k) {
        // A whole number past 2^53 is an integer (or its digits), shown as it is, never as a decimal.
        Some(Value::Number(n)) if !n.is_f64() => n.to_string().parse::<i128>().map(fmt_whole).unwrap_or_else(|_| n.to_string()),
        Some(Value::Number(n)) => match n.as_f64() {
            // A whole number a decimal holds is grouped as a whole, not in scientific form from 1e15.
            Some(x) if number && x.fract() == 0.0 && x.abs() < 9_007_199_254_740_992.0 => fmt_whole(x as i128),
            x => x.map(fmt_stat).unwrap_or_default(),
        },
        Some(Value::String(s)) if number => s.parse::<i128>().map(fmt_whole).unwrap_or_else(|_| s.clone()),
        Some(Value::String(s)) => s.clone(),
        _ => tl!("none").to_string(),
    };
    // The same value as the answer holds it: a number's digits as they are, text as it is.
    let full = |k: &str| match answer.get(k) {
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    };
    let line = |name: &str, shown: String, exact: String| (name.to_string(), shown, exact);
    let mut out = vec![line(tl!("Count"), count("count"), full("count")), line(tl!("Empty"), count("nulls"), full("nulls"))];
    if number {
        out.push(line(tl!("Sum"), value("sum"), full("sum")));
        out.push(line(tl!("Mean"), value("mean"), full("mean")));
        out.push(line(tl!("Minimum"), value("min"), full("min")));
        out.push(line(tl!("Maximum"), value("max"), full("max")));
        out.push(line(tl!("Standard deviation"), value("stdDev"), full("stdDev")));
        if answer.get("skipped").and_then(Value::as_u64).unwrap_or(0) > 0 {
            out.push(line(tl!("Not numbers"), count("skipped"), full("skipped")));
        }
    } else {
        // The copy keeps "more than" for a capped count, with the digits plain.
        let capped = answer.get("distinctCapped").and_then(Value::as_bool).unwrap_or(false);
        let more = |v: String| if capped { crate::i18n::fmt("more than {n}", &[("n", &v)]) } else { v };
        out.push(line(tl!("Distinct"), more(count("distinct")), more(full("distinct"))));
        out.push(line(tl!("Minimum"), value("min"), full("min")));
        out.push(line(tl!("Maximum"), value("max"), full("max")));
    }
    out
}

/// A whole number with all its digits, grouped by thousands.
fn fmt_whole(v: i128) -> String {
    let digits = v.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if v < 0 { format!("-{out}") } else { out }
}

/// A number for reading: digits grouped by thousands, a fraction to the precision its size
/// deserves (6 places below 1, 4 below 1,000, else 2) without trailing zeros, and very large or
/// small ones in scientific form.
fn fmt_stat(x: f64) -> String {
    if !x.is_finite() {
        return x.to_string();
    }
    let a = x.abs();
    if a != 0.0 && !(1e-6..1e15).contains(&a) {
        return format!("{x:e}");
    }
    let places = if x.fract() == 0.0 {
        0
    } else if a >= 1000.0 {
        2
    } else if a >= 1.0 {
        4
    } else {
        6
    };
    let text = format!("{a:.places$}");
    let (whole, fraction) = text.split_once('.').unwrap_or((text.as_str(), ""));
    let fraction = fraction.trim_end_matches('0');
    let mut out = String::new();
    for (i, c) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if !fraction.is_empty() {
        out.push('.');
        out.push_str(fraction);
    }
    // A value that rounds to nothing is no minus zero.
    if x < 0.0 && out.chars().any(|c| c.is_ascii_digit() && c != '0') { format!("-{out}") } else { out }
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
            // ISO 8601: `2026-03-05`; for a date-time, that and `14:30`, with or without a zone.
            T::Date => rustgis_engine::data::Date::parse(t).is_some(),
            T::DateTime => rustgis_engine::data::DateTime::parse(t).is_some(),
        }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::Queryable;

    /// The docked table keeps its row order across frames, then refreshes when a view option
    /// changes or an edit reaches the dataset through the session.
    #[test]
    fn table_reuses_row_order_until_inputs_change() {
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        let mut ds = Dataset::new("Parcels", rustgis_engine::data::GeometryType::Point, fields(&["Name"]));
        for name in ["Bravo parcel", "Alpha parcel", "Other lot"] {
            ds.push(None, vec![V::Text(name.into())]);
        }
        let id = app.session.map_mut().add_dataset(ds);
        app.session.table_layer = Some(id);
        app.session.run("edit.enable", &json!({"on": true})).unwrap();
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1400.0, 1000.0)).build_ui_state(
            |ui, app: &mut RustGisApp| {
                app.logic(&ui.ctx().clone());
                show(app, ui);
            },
            app,
        );
        h.run_steps(2);
        h.get_by_label("Name").click();
        h.run_steps(4);
        let indices: fn(&RustGisApp) -> &[usize] = |app| app.table.row_order.as_ref().unwrap().indices.as_slice();
        assert_eq!(indices(h.state()), [1, 0, 2]);
        let before = indices(h.state()).as_ptr();
        h.run_steps(3);
        assert_eq!(indices(h.state()).as_ptr(), before, "unchanged frames reuse the sorted indices");

        h.state_mut().table.filter = " PARCEL ".into();
        h.run_steps(2);
        assert_eq!(indices(h.state()), [1, 0]);
        assert!(h.query_by_label("Other lot").is_none());

        h.get_by_label("Selected only").click();
        h.state_mut().session.run("select.rows", &json!({"layer": id, "rows": [0, 1], "mode": "new"})).unwrap();
        h.run_steps(3);
        assert_eq!(indices(h.state()), [1, 0]);
        h.state_mut().session.run("select.rows", &json!({"layer": id, "rows": [1, 2], "mode": "new"})).unwrap();
        h.run_steps(2);
        assert_eq!(indices(h.state()), [1]);
        assert!(h.query_by_label("Bravo parcel").is_none());

        h.state_mut().session.run("edit.setValue", &json!({"layer": id, "row": 2, "field": "Name", "value": "Aardvark parcel"})).unwrap();
        h.run_steps(2);
        assert_eq!(indices(h.state()), [2, 1]);
        assert!(h.query_by_label("Aardvark parcel").is_some());
    }

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
        let key = ViewKey::Layer(app.session.map().layer(id).unwrap().instance_id());
        let value = |app: &RustGisApp, row: usize| app.session.map().layer(id).unwrap().dataset.value(row, col).to_string();
        let (first, second) = (value(&app, 0), value(&app, 1));
        let number = app.session.map().layer(id).unwrap().dataset.value(0, num).to_string();
        let schema = ds.fields.clone();
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
        h.state_mut().table.editing = Some(CellEdit::new(key, fid[0], col, first.clone(), &schema, None));
        h.run_steps(2);
        h.event(egui::Event::Text("Zed".into()));
        h.run_steps(2);
        h.key_press(egui::Key::Enter);
        h.run_steps(2);
        assert_eq!(value(h.state(), 0), format!("{first}Zed"));
        assert!(h.state().table.editing.is_none(), "the editor closed");
        // Escape: typed, then left as it was, the tool kept.
        h.state_mut().table.editing = Some(CellEdit::new(key, fid[1], col, second.clone(), &schema, None));
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
        h.state_mut().table.editing = Some(CellEdit::new(ViewKey::Layer(u64::MAX), fid[0], col, String::new(), &schema, None));
        h.run_steps(2);
        assert!(h.state().table.editing.is_none());
        // A number mistyped is refused, not written as null.
        h.state_mut().table.editing = Some(CellEdit::new(key, fid[0], num, String::new(), &schema, None));
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
        h.state_mut().table.editing = Some(CellEdit::new(key, fid[0], col, String::new(), &schema, None));
        h.run_steps(2);
        h.event(egui::Event::Text("Off".into()));
        h.run_steps(2);
        h.key_press(egui::Key::Enter);
        h.run_steps(2);
        assert_eq!(value(h.state(), 0), format!("{first}Zed"));
        assert!(h.state().session.notices_since(mark).iter().any(|n| n.text.contains("editing is turned off")), "said why");
        h.state_mut().session.map_mut().editing.settings.enabled = true;
        h.state_mut().table.editing = Some(CellEdit::new(key, fid[0], col, String::new(), &schema, None));
        h.state_mut().session.table_layer = None;
        h.run_steps(2);
        assert!(h.state().table.editing.is_none(), "dropped when the table closed");
        // A standalone table's cell is edited the same way, its layer's view left alone.
        h.state_mut().run("layer.addDataText", json!({"fileName": "sites.csv", "text": "id,street\n1,Rue Cler\n"})).unwrap();
        let table = h.state().session.project.tables[0].id;
        let tfid = h.state().session.project.tables[0].dataset.features[0].id;
        let table_fields = h.state().session.project.tables[0].dataset.fields.clone();
        let table_instance = Some(h.state().session.project.tables[0].instance_id());
        h.state_mut().session.table_standalone = Some(table);
        h.run_steps(2);
        h.state_mut().table.editing = Some(CellEdit::new(ViewKey::Table(table), tfid, 1, "Rue Cler".into(), &table_fields, table_instance));
        h.run_steps(2);
        h.event(egui::Event::Text(" 2".into()));
        h.run_steps(2);
        h.key_press(egui::Key::Enter);
        h.run_steps(2);
        assert_eq!(h.state().session.project.tables[0].dataset.value(0, 1).to_string(), "Rue Cler 2");
        assert!(h.state().table.editing.is_none(), "the editor closed");
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
        let table_instance = Some(app.session.project.table(id).unwrap().instance_id());
        let mut v = ColumnView::default();
        v.hide(&fs, 0);
        app.table.views.insert(ViewKey::Table(id), v);
        prune_views(&mut app);
        assert_eq!(app.table.views.get(&ViewKey::Table(id)).map(|v| v.hidden_count(&fs)), Some(1), "kept in its project");
        app.table.editing = Some(CellEdit::new(ViewKey::Table(id), 1, 0, "old draft".into(), &fs, table_instance));
        app.session.run("project.new", &json!({})).unwrap();
        let (again, _) = table(&mut app);
        assert_eq!(again, id, "the new project's table takes the same id");
        prune_views(&mut app);
        assert!(app.table.views.is_empty());
        assert!(app.table.editing.is_none(), "a pending draft must not reach the new project's same-id table");
    }

    /// Schema changes and tool replacements discard a focused draft, but ordinary edits keep it.
    #[test]
    fn a_cell_draft_cannot_write_replaced_data() {
        for change in ["schema", "replacement", "edit"] {
            let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
            let added =
                app.run("layer.addDataText", json!({"fileName": "draft.csv", "text": "gone,edited,victim\nA,B,C\nD,E,F\n"})).unwrap();
            app.run("edit.enable", json!({"on": true})).unwrap();
            let id = added["id"].as_u64().unwrap() as u32;
            app.session.table_standalone = Some(id);
            let fields = app.session.project.tables[0].dataset.fields.clone();
            let fid = app.session.project.tables[0].dataset.features[0].id;
            let table_instance = Some(app.session.project.tables[0].instance_id());
            let mut h = egui_kittest::Harness::builder().with_size(vec2(1400.0, 1000.0)).with_max_steps(64).build_ui_state(
                |ui, app: &mut RustGisApp| {
                    let ctx = ui.ctx().clone();
                    app.logic(&ctx);
                    app.ui(ui);
                },
                app,
            );
            h.run_steps(4);
            h.state_mut().table.editing = Some(CellEdit::new(ViewKey::Table(id), fid, 1, "B".into(), &fields, table_instance));
            h.run_steps(2);
            h.event(egui::Event::Text(" draft".into()));
            h.run_steps(2);
            match change {
                "schema" => {
                    h.state_mut().session.run("edit.deleteField", &json!({"table": id, "field": "gone"})).unwrap();
                }
                "replacement" => {
                    let mut data = h.state().session.project.table(id).unwrap().dataset.clone();
                    data.features[0].attrs[1] = V::Text("replacement".into());
                    assert_eq!(h.state_mut().session.project.put_table(data), id);
                }
                _ => {
                    h.state_mut()
                        .session
                        .run("edit.setValue", &json!({"table": id, "row": 1, "field": "edited", "value": "ordinary edit"}))
                        .unwrap();
                }
            }
            h.key_press(egui::Key::Enter);
            h.run_steps(4);
            let data = &h.state().session.project.tables[0].dataset;
            assert_eq!(data.value(0, data.field_index("victim").unwrap()).to_string(), "C");
            assert_eq!(
                data.value(0, data.field_index("edited").unwrap()).to_string(),
                match change {
                    "replacement" => "replacement",
                    "edit" => "B draft",
                    _ => "B",
                }
            );
            assert!(h.state().table.editing.is_none());
        }
    }

    /// Deleting through the menu clears sorting when the schema lands, including a sort picked
    /// while the desktop worker is pending. Rendering alone deliberately does not land the job.
    #[test]
    fn deleting_a_field_from_its_menu_clears_the_sort() {
        for background in [false, true] {
            let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
            let added = app
                .run("layer.addDataText", json!({"fileName": "columns.csv", "text": "alpha,beta,gamma,delta\n1,8,3,4\n2,7,4,5\n"}))
                .unwrap();
            app.session.table_standalone = Some(added["id"].as_u64().unwrap() as u32);
            app.reads_off_frame = background;
            let mut h = egui_kittest::Harness::builder().with_size(vec2(1400.0, 1000.0)).with_max_steps(64).build_ui_state(
                |ui, app: &mut RustGisApp| {
                    app.logic(&ui.ctx().clone());
                    show(app, ui);
                },
                app,
            );
            h.run_steps(4);
            h.state_mut().table.sort = Some((2, true));
            h.run_steps(2);
            h.get_by_label("alpha").click_secondary();
            h.run_steps(2);
            h.get_by_label("Delete Field").click();
            h.run_steps(4);
            if background {
                assert!(h.state().session.project.tables[0].dataset.field_index("alpha").is_some());
                assert_eq!(h.state().table.sort, Some((2, true)), "queuing is not a schema change");
                h.state_mut().table.sort = Some((2, false));
                h.run_steps(2);
                let end = std::time::Instant::now() + std::time::Duration::from_secs(60);
                while crate::readjob::showing(h.state()) {
                    assert!(std::time::Instant::now() < end);
                    crate::readjob::land(h.state_mut());
                    std::thread::sleep(std::time::Duration::from_millis(1)); // test-wait: legacy polling for field deletion completion
                }
                h.run_steps(2);
            }
            assert!(h.state().session.project.tables[0].dataset.field_index("alpha").is_none());
            assert!(h.state().table.sort.is_none(), "the old index would now sort by delta instead of gamma");
        }
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

    #[test]
    fn statistics_are_formatted_for_reading() {
        assert_eq!(fmt_stat(1234567.0), "1,234,567");
        assert_eq!(fmt_stat(-1234.5), "-1,234.5");
        assert_eq!(fmt_stat(0.25), "0.25");
        assert_eq!(fmt_stat(12.345678), "12.3457");
        assert_eq!(fmt_stat(12345.678), "12,345.68");
        assert_eq!(fmt_stat(0.0), "0");
        assert_eq!(fmt_stat(1e20), "1e20");
        let all = json!({"layer": "L", "field": "f", "selected": false, "rows": 1234});
        assert_eq!(stats_coverage(&all), "All 1,234 rows");
        let some = json!({"layer": "L", "field": "f", "selected": true, "rows": 12});
        assert_eq!(stats_coverage(&some), "The 12 selected rows");
        let numbers = json!({"kind": "number", "count": 3, "nulls": 1, "skipped": 0, "sum": 12000.5, "mean": 4000.1667, "min": 2.0, "max": 6.0, "stdDev": 1.5});
        let lines = stats_lines(&numbers);
        assert_eq!(lines.iter().find(|(n, _, _)| n == "Sum").map(|l| l.1.as_str()), Some("12,000.5"));
        assert!(lines.iter().all(|(n, _, _)| n != "Not numbers"));
        let text = json!({"kind": "text", "count": 3, "nulls": 0, "distinct": 2, "min": "a", "max": "b"});
        let names: Vec<String> = stats_lines(&text).into_iter().map(|l| l.0).collect();
        assert_eq!(names, ["Count", "Empty", "Distinct", "Minimum", "Maximum"]);
    }

    /// A count made here, not off the frame, is kept for the window too: the answer is not dropped.
    #[test]
    fn statistics_counted_at_once_are_kept_for_the_window() {
        let mut app = RustGisApp::with_sample();
        let id = app.session.map().layers.first().map(|l| l.id).unwrap();
        let ds = &app.session.map().layer(id).unwrap().dataset;
        let num = ds.fields.iter().find(|f| f.ty.is_numeric()).map(|f| f.name.clone()).expect("a number field");
        assert!(app.table_stats_kept().is_none());
        ask_statistics(&mut app, json!({"layer": id, "field": num}));
        let kept = app.table_stats_kept().expect("kept at once");
        assert_eq!(kept["field"], num);
    }

    /// Statistics… on a column answers in a window that names the rows it covers and shows the sum.
    #[test]
    fn the_statistics_window_shows_the_column_sum() {
        let mut app = RustGisApp::with_sample();
        let id = app.session.map().layers.first().map(|l| l.id).unwrap();
        let ds = &app.session.map().layer(id).unwrap().dataset;
        let num = ds.fields.iter().find(|f| f.ty.is_numeric()).map(|f| f.name.clone()).expect("a number field");
        let rows = ds.features.len();
        app.session.table_layer = Some(id);
        let answer = app.run("table.statistics", json!({"layer": id, "field": num})).unwrap();
        let sum = fmt_stat(answer["sum"].as_f64().unwrap());
        statistics_landed(&mut app, crate::Landed::Ran(Ok(answer)));
        assert!(app.table.stats.is_some(), "the answer is kept for the window");
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1400.0, 1000.0)).with_max_steps(64).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app,
        );
        h.run_steps(4);
        assert!(h.query_by_label(&sum).is_some(), "the sum {sum} is shown");
        assert!(h.query_by_label(&format!("All {} rows", fmt_stat(rows as f64))).is_some());
        // Closing the table drops the window.
        h.state_mut().session.table_layer = None;
        h.run_steps(2);
        assert!(h.state().table.stats.is_none());
    }

    /// A whole-number sum from 1e15 up to 2^53 is grouped, not in scientific form, and the copy
    /// holds the digits the window rounds.
    #[test]
    fn a_whole_sum_from_1e15_is_grouped_and_the_copy_keeps_the_digits() {
        let whole = json!({"kind": "number", "count": 2, "nulls": 0, "skipped": 0, "sum": 2e15, "mean": 1e15,
            "min": 1, "max": 1999999999999999_i64, "stdDev": 0.5});
        let lines = stats_lines(&whole);
        let of = |n: &str| lines.iter().find(|l| l.0 == n).map(|l| (l.1.clone(), l.2.clone()));
        assert_eq!(of("Sum"), Some(("2,000,000,000,000,000".to_string(), json!(2e15).to_string())));
        assert_eq!(of("Mean").map(|l| l.0).as_deref(), Some("1,000,000,000,000,000"));
        let decimal = json!({"kind": "number", "count": 2, "nulls": 0, "skipped": 0, "sum": 12345.678, "mean": 0.1234567,
            "min": 1.0, "max": 2.0, "stdDev": 0.5});
        let lines = stats_lines(&decimal);
        let of = |n: &str| lines.iter().find(|l| l.0 == n).map(|l| (l.1.clone(), l.2.clone()));
        assert_eq!(of("Sum"), Some(("12,345.68".to_string(), "12345.678".to_string())));
        assert_eq!(of("Mean"), Some(("0.123457".to_string(), "0.1234567".to_string())));
    }

    #[test]
    fn a_tab_or_line_break_in_a_text_value_keeps_one_copied_line_per_statistic() {
        let text = json!({"kind": "text", "count": 2, "nulls": 0, "distinct": 2, "min": "a\tb", "max": "c\r\nd"});
        let lines = stats_lines(&text);
        let copied = stats_tsv(&lines);
        assert_eq!(copied.lines().count(), lines.len());
        assert!(copied.lines().all(|l| l.matches('\t').count() == 1), "{copied}");
        assert!(copied.contains("a b") && copied.contains("c  d"), "{copied}");
    }

    #[test]
    fn whole_numbers_past_2_53_and_a_capped_distinct_count_are_shown_as_they_are() {
        assert_eq!(fmt_whole(9_007_199_254_740_993), "9,007,199,254,740,993");
        assert_eq!(fmt_whole(-1_234), "-1,234");
        assert_eq!(fmt_whole(0), "0");
        let numbers = json!({"kind": "number", "count": 2, "nulls": 0, "skipped": 0, "sum": 18_014_398_509_481_986_i64,
            "mean": 9.0e15, "min": 9_007_199_254_740_993_i64, "max": "170141183460469231731687303715884105727", "stdDev": 0.0});
        let lines = stats_lines(&numbers);
        let of = |n: &str| lines.iter().find(|l| l.0 == n).map(|l| l.1.clone());
        assert_eq!(of("Sum").as_deref(), Some("18,014,398,509,481,986"));
        assert_eq!(of("Minimum").as_deref(), Some("9,007,199,254,740,993"));
        assert_eq!(of("Maximum").as_deref(), Some("170,141,183,460,469,231,731,687,303,715,884,105,727"));
        // A text column's minimum is text, never regrouped as a number.
        let text =
            json!({"kind": "text", "count": 1, "nulls": 0, "distinct": 100000, "distinctCapped": true, "min": "12345", "max": "12345"});
        let lines = stats_lines(&text);
        assert_eq!(lines.iter().find(|l| l.0 == "Distinct").map(|l| l.1.as_str()), Some("more than 100,000"));
        assert_eq!(lines.iter().find(|l| l.0 == "Minimum").map(|l| l.1.as_str()), Some("12345"));
        let exact = json!({"kind": "text", "count": 1, "nulls": 0, "distinct": 5, "min": "a", "max": "b"});
        assert_eq!(stats_lines(&exact).iter().find(|l| l.0 == "Distinct").map(|l| l.1.as_str()), Some("5"));
    }

    /// The copy holds the counts as plain digits (a comma would read as a decimal in a spreadsheet),
    /// and a capped distinct count keeps "more than" with the digits plain.
    #[test]
    fn copied_counts_are_plain_digits() {
        let numbers = json!({"kind": "number", "count": 1234, "nulls": 1234, "skipped": 1234, "sum": 1.0, "mean": 1.0,
            "min": 1.0, "max": 1.0, "stdDev": 0.0});
        let lines = stats_lines(&numbers);
        let of = |n: &str| lines.iter().find(|l| l.0 == n).map(|l| (l.1.clone(), l.2.clone()));
        assert_eq!(of("Count"), Some(("1,234".to_string(), "1234".to_string())));
        assert_eq!(of("Empty"), Some(("1,234".to_string(), "1234".to_string())));
        assert_eq!(of("Not numbers"), Some(("1,234".to_string(), "1234".to_string())));
        let text = json!({"kind": "text", "count": 1, "nulls": 0, "distinct": 100000, "distinctCapped": true, "min": "a", "max": "b"});
        let lines = stats_lines(&text);
        let of = |n: &str| lines.iter().find(|l| l.0 == n).map(|l| (l.1.clone(), l.2.clone()));
        assert_eq!(of("Distinct"), Some(("more than 100,000".to_string(), "more than 100000".to_string())));
    }

    /// Statistics left from another project, map or layer instance are not shown.
    #[test]
    fn statistics_of_another_project_map_or_layer_are_not_kept() {
        let mut app = RustGisApp::with_sample();
        let id = app.session.map().layers.first().map(|l| l.id).unwrap();
        let field = app.session.map().layer(id).unwrap().dataset.fields.iter().find(|f| f.ty.is_numeric()).map(|f| f.name.clone()).unwrap();
        app.session.table_layer = Some(id);
        let answer = app.run("table.statistics", json!({"layer": id, "field": field})).unwrap();
        statistics_landed(&mut app, crate::Landed::Ran(Ok(answer)));
        let st = app.table.stats.as_ref().unwrap();
        assert!(st.of(&app, Some(id)), "the table it was run on");
        assert!(!st.of(&app, Some(id + 1000)), "another layer");
        assert!(!st.of(&app, None), "no table");
        let mut other_project = ColumnStats {
            layer: st.layer,
            instance: st.instance,
            opened: st.opened + 1,
            map: st.map,
            rows: st.rows.clone(),
            selection: st.selection.clone(),
            answer: Value::Null,
        };
        assert!(!other_project.of(&app, Some(id)), "another project");
        other_project.opened = st.opened;
        other_project.map += 1;
        assert!(!other_project.of(&app, Some(id)), "another map");
        other_project.map = st.map;
        other_project.instance += 1;
        assert!(!other_project.of(&app, Some(id)), "a layer that took the id of another");
        // A project opened between two frames drops it with the views.
        app.session.opened += 1;
        prune_views(&mut app);
        assert!(app.table.stats.is_none());
    }

    /// Of two columns of one name, Statistics… counts the one chosen, by its place.
    #[test]
    fn statistics_of_a_second_column_of_the_same_name_count_that_column() {
        use rustgis_engine::data::{Field, FieldType, GeometryType};
        let mut app = RustGisApp::with_sample();
        let mut ds =
            Dataset::new("twins", GeometryType::Point, vec![Field::new("v", FieldType::Double), Field::new("v", FieldType::Double)]);
        ds.push(None, vec![V::Float(1.0), V::Float(100.0)]);
        rustgis_engine::cmd::layer::add_dataset(&mut app.session, ds);
        let id = app.session.map().layer_by_name("twins").map(|l| l.id).unwrap();
        let second = app.run("table.statistics", json!({"layer": id, "field": "v", "index": 1})).unwrap();
        assert_eq!((second["sum"].clone(), second["index"].clone()), (json!(100.0), json!(1)));
        let by_name = app.run("table.statistics", json!({"layer": id, "field": "v"})).unwrap();
        assert_eq!(by_name["sum"], 1.0, "by name alone, the first");
    }

    /// Statistics kept for a layer stop showing once a value is edited or a row is added: the
    /// window would otherwise keep a sum the layer no longer has.
    #[test]
    fn statistics_stop_showing_once_the_layers_rows_change() {
        use rustgis_engine::data::{FieldType, GeometryType};
        let mut app = RustGisApp::with_sample();
        let mut ds = Dataset::new("counted", GeometryType::Point, vec![Field::new("n", FieldType::Integer)]);
        ds.push(None, vec![V::Int(1)]);
        ds.push(None, vec![V::Int(2)]);
        let id = rustgis_engine::cmd::layer::add_dataset(&mut app.session, ds);
        let answer = app.run("table.statistics", json!({"layer": id, "field": "n"})).unwrap();
        statistics_landed(&mut app, crate::Landed::Ran(Ok(answer)));
        assert!(app.table.stats.as_ref().is_some_and(|s| s.of(&app, Some(id))), "kept while the rows are as counted");
        app.session.run("edit.setValue", &json!({"layer": id, "row": 0, "field": "n", "value": 7})).unwrap();
        assert!(app.table.stats.as_ref().is_some_and(|s| !s.of(&app, Some(id))), "an edited value changes the rows counted");
        let answer = app.run("table.statistics", json!({"layer": id, "field": "n"})).unwrap();
        statistics_landed(&mut app, crate::Landed::Ran(Ok(answer)));
        assert_eq!(app.table.stats.as_ref().map(|s| s.answer["sum"].clone()), Some(json!(9.0)), "counted again after the edit");
        let added = json!({"layer": id, "geometry": {"type": "Point", "coordinates": [1.0, 2.0]}, "attributes": {"n": 5}});
        app.session.run("edit.addFeature", &added).unwrap();
        assert!(app.table.stats.as_ref().is_some_and(|s| !s.of(&app, Some(id))), "an added row changes the rows counted");
    }

    #[test]
    fn selected_statistics_stop_matching_when_the_selection_changes() {
        use rustgis_engine::data::{FieldType, GeometryType};
        for selected in [false, true] {
            let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
            let mut ds = Dataset::new("counted", GeometryType::Point, vec![Field::new("n", FieldType::Integer)]);
            ds.push(None, vec![V::Int(1)]);
            ds.push(None, vec![V::Int(2)]);
            let id = rustgis_engine::cmd::layer::add_dataset(&mut app.session, ds);
            app.session.table_layer = Some(id);
            app.session.map_mut().layer_mut(id).unwrap().selection = vec![0];
            let answer = app.run("table.statistics", json!({"layer":id,"field":"n","selected":selected})).unwrap();
            statistics_landed(&mut app, crate::Landed::Ran(Ok(answer)));
            assert!(app.table.stats.as_ref().unwrap().of(&app, Some(id)));
            app.session.map_mut().layer_mut(id).unwrap().selection = vec![1];
            let mut output = egui::Context::default().run_ui(Default::default(), |ui| show(&mut app, ui));
            output.textures_delta.clear();
            assert_eq!(app.table.stats.is_some(), !selected, "only the selected-row window closes");
        }
    }
}
