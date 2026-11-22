//! Data › Offline › Conflicts: the versions that lost when an offline area's edits came back to its
//! map (#2468), kept on the area until they are dismissed. It shows what `offline.conflicts` lists
//! and dismisses through `offline.dismissConflicts`, so each dismissal can be undone.
use rustgis_engine::Session;
use rustgis_engine::project::{AreaConflict, ConflictKind};
use serde_json::json;

use crate::RustGisApp;
use crate::i18n;

/// The rows of one page of the window.
const PAGE: usize = 200;

/// The open Conflicts window: the project it was opened in ([`Session::opened`]) and its area, by
/// identity (by name for an area made before areas had one).
pub struct Form {
    opened: u64,
    area: String,
    name: String,
    /// The page shown, from 0.
    page: usize,
}

impl Form {
    /// The window for offline area `name`.
    pub fn new(s: &Session, name: &str) -> Result<Self, String> {
        let a = s
            .project
            .offline_areas
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| i18n::fmt("There is no offline area '{name}'.", &[("name", name)]))?;
        Ok(Self { opened: s.opened, area: a.id.clone(), name: a.name.clone(), page: 0 })
    }

    /// The area's place in the project, while it is the one the window was opened on.
    fn index(&self, s: &Session) -> Option<usize> {
        if s.opened != self.opened {
            return None;
        }
        s.project.offline_areas.iter().position(|a| if self.area.is_empty() { a.name == self.name } else { a.id == self.area })
    }
}

/// What happened to the feature, as the window says it.
fn happened(kind: ConflictKind) -> &'static str {
    match kind {
        ConflictKind::Replaced => tl!("Edited in both: the field's version went in"),
        ConflictKind::Deleted => tl!("Deleted in the field after a change in the map"),
        ConflictKind::Kept => tl!("Deleted in the field, kept for a change in the map"),
        ConflictKind::Dropped => tl!("Edited in the field, deleted in the map meanwhile"),
    }
}

/// The version that lost, as text: `field = value` for each value, one a line, and the shape when
/// it lost.
fn lost(c: &AreaConflict) -> String {
    // Numbers in full, as a file writes them, not rounded for display.
    let shown = |v: &rustgis_engine::data::Value| if v.is_null() { "<null>".to_string() } else { v.to_file_text() };
    let mut parts: Vec<String> = c.values.iter().map(|(n, v)| format!("{n} = {}", shown(v))).collect();
    if c.shape {
        parts.push(if c.geometry.is_some() { tl!("its shape").to_string() } else { tl!("no shape").to_string() });
    }
    if parts.is_empty() {
        return if c.kind == ConflictKind::Kept { tl!("the deletion").to_string() } else { "—".to_string() };
    }
    parts.join("\n")
}

/// Shows the window while it is open.
pub fn show(app: &mut RustGisApp, ctx: &egui::Context) {
    let Some(mut form) = app.dialogs.conflicts.take() else { return };
    let Some(ai) = form.index(&app.session) else {
        app.dialogs.open = None;
        return;
    };
    let Some(area) = app.session.project.offline_areas.get(ai) else { return };
    let (map, conflicts) = (area.map.clone(), &area.conflicts);
    // The last page still there, after dismissals.
    let pages = conflicts.len().div_ceil(PAGE).max(1);
    form.page = form.page.min(pages - 1);
    let first = form.page * PAGE;
    let mut open = true;
    let (mut dismiss, mut show_at, mut all) = (None, None, false);
    egui::Window::new(i18n::fmt("Conflicts: {area}", &[("area", &form.name)]))
        .id(egui::Id::unique("offline_conflicts"))
        .open(&mut open)
        .collapsible(false)
        .default_width(640.0)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .show(ctx, |ui| {
            if conflicts.is_empty() {
                ui.label(i18n::fmt("No conflicts are kept for the offline area '{area}'.", &[("area", &form.name)]));
                return;
            }
            ui.label(i18n::fmt(
                "The versions that lost when edits came back to the map '{map}'. Dismiss one once it has been dealt with; Undo brings it back.",
                &[("map", &map)],
            ));
            ui.add_space(4.0);
            egui::ScrollArea::vertical().max_height((ctx.content_rect().height() * 0.6).max(160.0)).show(ui, |ui| {
                egui::Grid::new("offline_conflicts_grid").num_columns(5).striped(true).spacing(egui::vec2(10.0, 6.0)).show(ui, |ui| {
                    for title in [tl!("Feature"), tl!("What happened"), tl!("The version that lost"), "", ""] {
                        ui.strong(title);
                    }
                    ui.end_row();
                    for (i, c) in conflicts.iter().enumerate().skip(first).take(PAGE) {
                        ui.label(format!("{} #{}", c.layer_name, c.feature));
                        ui.label(happened(c.kind));
                        ui.add(egui::Label::new(lost(c)).wrap());
                        // A feature the map kept; one deleted there has nothing to show.
                        let here = matches!(c.kind, ConflictKind::Replaced | ConflictKind::Kept);
                        let shown = ui.add_enabled(here, egui::Button::new(tl!("Show")));
                        let shown = if here {
                            shown.on_hover_text(tl!("Select the feature in the map and zoom to it."))
                        } else {
                            shown.on_disabled_hover_text(tl!("The feature was deleted from the map."))
                        };
                        if shown.clicked() {
                            show_at = Some((c.layer, c.feature));
                        }
                        if ui.button(tl!("Dismiss")).clicked() {
                            dismiss = Some(i);
                        }
                        ui.end_row();
                    }
                });
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                all = ui.button(tl!("Dismiss All")).clicked();
                if pages > 1 {
                    ui.separator();
                    if ui.add_enabled(form.page > 0, egui::Button::new(tl!("Previous"))).clicked() {
                        form.page -= 1;
                    }
                    let last = (first + PAGE).min(conflicts.len());
                    ui.label(i18n::fmt(
                        "{from}–{to} of {count}",
                        &[("from", &(first + 1).to_string()), ("to", &last.to_string()), ("count", &conflicts.len().to_string())],
                    ));
                    if ui.add_enabled(form.page + 1 < pages, egui::Button::new(tl!("Next"))).clicked() {
                        form.page += 1;
                    }
                }
            });
        });
    if let Some((layer, feature)) = show_at {
        show_feature(app, &map, layer, feature);
    }
    if let Some(i) = dismiss {
        // The indexes are those of the list as it was drawn, in this state of the project.
        let state = app.session.state();
        let _ = app.run("offline.dismissConflicts", json!({"name": form.name, "conflicts": [i], "state": state}));
    } else if all {
        let _ = app.run("offline.dismissConflicts", json!({"name": form.name}));
    }
    if open {
        app.dialogs.conflicts = Some(form);
    } else {
        app.dialogs.open = None;
    }
}

/// Selects feature `feature` of layer `layer` in map `map`, made the active map, and zooms to it;
/// says so when the map, the layer or the feature is no longer there.
fn show_feature(app: &mut RustGisApp, map: &str, layer: u32, feature: u64) {
    let row = app
        .session
        .project
        .maps
        .iter()
        .find(|m| m.name.eq_ignore_ascii_case(map))
        .and_then(|m| m.layers.iter().find(|l| l.id == layer && !l.is_raster()))
        .and_then(|l| l.dataset.features.rows().position(|r| r.id() == feature));
    let Some(row) = row else {
        app.session
            .notify(i18n::fmt("Feature #{id} is no longer in the map '{map}'.", &[("id", &feature.to_string()), ("map", map)]), true);
        return;
    };
    if app.run("map.activate", json!({"name": map})).is_ok() && app.run("select.rows", json!({"layer": layer, "rows": [row]})).is_ok() {
        // A feature without a shape is selected, with nothing to zoom to.
        let _ = app.run("select.zoomTo", json!({}));
    }
    app.map.invalidate();
}
