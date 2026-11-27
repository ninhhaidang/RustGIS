//! The attribute table's current cell: the one last clicked, which the arrow keys move and Enter
//! edits. View state only: never saved, and no command sees it.

use std::sync::Weak;

use egui::{Context, Event, Key, Rangef, Stroke, StrokeKind, Ui};
use rustgis_engine::data::{Dataset, Feature, Value};

use super::{CellEdit, ViewKey, tag};
use crate::RustGisApp;
use crate::inuse::Pane;
use crate::theme::Tokens;

/// Takes the table's keys while it is the pane in use: the arrows with no modifier, and Enter
/// while there is a current cell. [`crate::chrome::shortcuts`] calls it once the shortcuts have
/// taken theirs, and not while text is being typed, so nothing drawn later in the frame takes
/// these too. [`super::show`] acts on them.
pub(crate) fn take_keys(app: &mut RustGisApp, ctx: &Context) {
    // An open menu is over the table: no cell moves or opens under it. And a widget that has the
    // keyboard (a toolbar button reached with Tab) keeps the keys: Enter presses it, and the
    // arrows move the focus on.
    if crate::inuse::pane(app) != Pane::Table || egui::Popup::is_any_open(ctx) || ctx.memory(|m| m.focused()).is_some() {
        return;
    }
    // A dialog that is up has Enter for itself.
    let enter = app.table.current.is_some() && app.table.dates.is_none() && app.dialogs.open.is_none() && app.ui.choosing.is_empty();
    let keys = &mut app.table.keys;
    ctx.input_mut(|input| {
        input.events.retain(|event| {
            let Event::Key { key, pressed: true, repeat, modifiers, .. } = event else { return true };
            match key {
                _ if !modifiers.is_none() => true,
                Key::ArrowUp | Key::ArrowDown | Key::ArrowLeft | Key::ArrowRight => {
                    keys.push(*key);
                    false
                }
                Key::Enter if enter => {
                    // Held after it kept a value, its repeats would open the cell again.
                    if !*repeat {
                        keys.push(*key);
                    }
                    false
                }
                _ => true,
            }
        });
    });
}

/// The table as the frame draws it.
pub(super) struct Shown<'a> {
    pub view: ViewKey,
    pub table_instance: Option<u64>,
    pub ds: &'a Dataset,
    /// The rows shown, each by its row in the dataset, or None for all of them in its order.
    pub rows: Option<&'a [usize]>,
    /// The fields shown, in the order of their columns, which follow Feature ID's.
    pub columns: &'a [usize],
}

impl Shown<'_> {
    /// The cell in the row at place `at` of those shown, in field `field` (None: Feature ID).
    pub fn cell(&self, at: usize, field: Option<usize>) -> Option<Current> {
        let row = match self.rows {
            Some(rows) => *rows.get(at)?,
            None => at,
        };
        let fid = self.ds.features.row(row)?.id();
        let field = match field {
            Some(i) => Some(tag(&self.ds.fields, i)?),
            None => None,
        };
        let named = field.as_ref().map_or(0, |(name, _)| self.ds.fields.iter().filter(|f| &f.name == name).count());
        let source = self.ds.features.allocation_key();
        Some(Current { view: self.view, table_instance: self.table_instance, fid, field, named, row, at, source })
    }

    /// Where an arrow key puts the current cell: one row or column on, as shown, or None at an
    /// edge. With no current cell, it goes to the first row and column shown; with its row or its
    /// column no longer shown, to the first of those, keeping the other.
    pub fn step(&self, current: Option<&mut Current>, key: Key) -> Option<Current> {
        let (at, column) = current.map_or((None, None), |c| (c.place(self), c.column(self)));
        let (at, column) = match (at, column) {
            (Some(at), Some(column)) => match key {
                Key::ArrowUp => (at.checked_sub(1)?, column),
                Key::ArrowDown => (at.checked_add(1)?, column),
                Key::ArrowLeft => (at, column.checked_sub(1)?),
                Key::ArrowRight => (at, column.checked_add(1)?),
                _ => return None,
            },
            (at, column) => (at.unwrap_or(0), column.unwrap_or(0)),
        };
        let field = match column.checked_sub(1) {
            Some(i) => Some(*self.columns.get(i)?),
            None => None,
        };
        self.cell(at, field)
    }

    /// Opens the editor on field `field` of feature `fid`, in row `row`: what a double-click on a
    /// cell does, and Enter on the current one. What the editor open before it holds is kept first,
    /// in `commits` (once: it may have been kept on losing the focus, or left by `cancel`). False,
    /// with nothing changed, for a column whose name finds another field, where its value would be
    /// written.
    pub fn open_editor(
        &self,
        (row, fid, field): (usize, u64, usize),
        editing: &mut Option<CellEdit>,
        commits: &mut Vec<(u64, usize, String, String)>,
        cancel: Option<(u64, usize)>,
    ) -> bool {
        let fields = &self.ds.fields;
        if fields.get(field).is_some_and(|f| self.ds.field_index(&f.name) != Some(field)) {
            return false;
        }
        if let Some(e) = editing.take()
            && !commits.iter().any(|q| q.0 == e.fid && q.1 == e.col)
            && cancel != Some((e.fid, e.col))
        {
            commits.push((e.fid, e.col, e.text, e.was));
        }
        let value = self.ds.value(row, field);
        let text = if matches!(value, Value::Null) { String::new() } else { value.to_string() };
        *editing = Some(CellEdit::new(self.view, fid, field, text, fields, self.table_instance));
        true
    }
}

/// The current cell. Its feature is kept by id and its field by [`tag`], so sorting, filtering,
/// hiding or moving columns and edits to other rows and fields leave it where it is.
pub(super) struct Current {
    view: ViewKey,
    /// Which standalone table it is of: another can take its id.
    table_instance: Option<u64>,
    fid: u64,
    /// None for the Feature ID cell.
    field: Option<(String, usize)>,
    /// How many fields had its field's name. With another number of them (one of a repeated name
    /// added or deleted), which of them it was can't be told, and the cell is dropped.
    named: usize,
    /// The feature's row in the dataset, and its place among the rows shown, when last found.
    /// Looked at first, so the rows are searched only once they or their order have changed.
    row: usize,
    at: usize,
    /// The rows it was found in ([`rustgis_engine::data::Features::allocation_key`]).
    source: Weak<Vec<Feature>>,
}

impl Current {
    /// The index of its field: Some(None) for Feature ID, None when the field has gone.
    fn field_index(&self, ds: &Dataset) -> Option<Option<usize>> {
        let Some((name, nth)) = &self.field else { return Some(None) };
        let named = || ds.fields.iter().enumerate().filter(|(_, f)| &f.name == name);
        if named().count() != self.named {
            return None;
        }
        named().nth(*nth).map(|(i, _)| Some(i))
    }

    /// Keeps up with the table as it is now. False, for the cell to be dropped, when the table
    /// shows another layer or table, or the feature or the field has gone. The feature is looked
    /// for only after a change to the rows, and in every row only when it left its own.
    pub fn follow(&mut self, table: &Shown) -> bool {
        if self.view != table.view || self.table_instance != table.table_instance || self.field_index(table.ds).is_none() {
            return false;
        }
        let source = table.ds.features.allocation_key();
        if !self.source.ptr_eq(&source) {
            if table.ds.features.row(self.row).is_none_or(|r| r.id() != self.fid) {
                let Some(row) = table.ds.features.rows().position(|r| r.id() == self.fid) else { return false };
                self.row = row;
            }
            self.source = source;
        }
        true
    }

    /// The cell to outline: its row in the dataset and its field (None: Feature ID).
    pub fn mark(&self, ds: &Dataset) -> Option<(usize, Option<usize>)> {
        Some((self.row, self.field_index(ds)?))
    }

    /// Its place among the rows shown, or None when the filter or Selected only leaves it out.
    pub fn place(&mut self, table: &Shown) -> Option<usize> {
        match table.rows {
            None => self.at = self.row,
            Some(rows) if rows.get(self.at) == Some(&self.row) => {}
            Some(rows) => self.at = rows.iter().position(|&r| r == self.row)?,
        }
        Some(self.at)
    }

    /// Its column among those shown, Feature ID's being the first; None when its field is hidden.
    fn column(&self, table: &Shown) -> Option<usize> {
        match self.field_index(table.ds)? {
            Some(field) => table.columns.iter().position(|&c| c == field).map(|i| i + 1),
            None => Some(0),
        }
    }

    /// What Enter edits: the row, feature and field of a cell that is shown. None for a Feature ID
    /// cell, which can't be edited, and for a cell the table leaves out.
    pub fn editable(&mut self, table: &Shown) -> Option<(usize, u64, usize)> {
        self.place(table)?;
        let field = self.field_index(table.ds)??;
        table.columns.contains(&field).then_some((self.row, self.fid, field))
    }
}

/// The current cell's outline, around the cell `ui` is. It reaches halfway into the gap before
/// the cell, to stand clear of the text, but not past `view`, the table's width on screen.
pub(super) fn outline(ui: &Ui, t: &Tokens, view: Rangef) {
    let gap = ui.spacing().item_spacing.x * 0.5;
    let (mut rect, mut clip) = (ui.max_rect(), ui.clip_rect());
    rect.min.x = (rect.min.x - gap).max(view.min);
    clip.min.x = (clip.min.x - gap).max(view.min);
    if rect.is_positive() {
        let mut painter = ui.painter().clone();
        painter.set_clip_rect(clip);
        painter.rect_stroke(rect, 2.0, Stroke::new(1.5, t.accent), StrokeKind::Inside);
    }
}
