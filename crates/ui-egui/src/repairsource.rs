//! Repair a layer's linked file or a local tile archive without replacing its configuration.
use crate::RustGisApp;
#[cfg(not(target_arch = "wasm32"))]
use crate::theme::Tokens;
use rustgis_engine::Session;
#[cfg(not(target_arch = "wasm32"))]
use serde_json::json;

/// The map-local item whose linked file is being repaired.
#[derive(Clone, Copy)]
pub enum Target {
    Layer(u32),
    Tiles(usize),
}

/// Captured source details and the replacement selected in Repair Data Source.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub struct Form {
    target: Target,
    opened: u64,
    map: (usize, String),
    /// An index is not an identity: closing on a different archive prevents accidental retargeting.
    archive: Option<(String, rustgis_engine::map::TileKind)>,
    name: String,
    saved: String,
    read_error: Option<String>,
    path: String,
    listed_for: String,
    #[cfg(not(target_arch = "wasm32"))]
    listing: Option<(String, crate::Slot<Result<serde_json::Value, String>>)>,
    tables: Vec<String>,
    table: String,
    error: Option<String>,
}

impl Form {
    /// Opens on a specific item, retaining its project and map rather than the current selection.
    pub fn new(s: &Session, target: Target) -> Result<Self, String> {
        let (name, saved, read_error, table) = match target {
            Target::Layer(id) => {
                let l = s.map().layer(id).ok_or_else(|| tl!("Layer not found").to_string())?;
                let saved = l.linked_file().ok_or_else(|| {
                    tl!("This layer's data is kept in the project or read from a database or web address: there's no file to repair.")
                        .to_string()
                })?;
                let read_error =
                    l.raster.as_ref().and_then(|r| r.error.clone()).or_else(|| l.source.as_ref().and_then(|s| s.error.clone()));
                (l.name.clone(), saved.to_string(), read_error, l.linked_table().unwrap_or_default().to_string())
            }
            Target::Tiles(index) => {
                let t = s.map().tiles.get(index).ok_or_else(|| tl!("Tile layer not found").to_string())?;
                let saved = t
                    .archive_file()
                    .ok_or_else(|| tl!("Only a tile archive read from a file (PMTiles or MBTiles) can be repaired.").to_string())?;
                (t.name.clone(), saved.to_string(), t.error.clone(), String::new())
            }
        };
        Ok(Self {
            target,
            opened: s.opened,
            map: (s.project.active_map, s.map().name.clone()),
            archive: match target {
                Target::Layer(_) => None,
                Target::Tiles(index) => s.map().tiles.get(index).map(|t| (t.id.clone(), t.kind)),
            },
            name,
            saved,
            read_error,
            path: String::new(),
            listed_for: String::new(),
            #[cfg(not(target_arch = "wasm32"))]
            listing: None,
            tables: Vec::new(),
            table,
            error: None,
        })
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Form {
    /// Editing only invalidates discovery; reading the file requires an explicit action.
    fn reset_listing(&mut self) {
        if self.path != self.listed_for {
            self.listed_for.clone_from(&self.path);
            self.tables.clear();
            self.error = None;
            // A worker owns only its old slot, so even changing back to this path cannot revive it.
            self.listing = None;
        }
    }

    fn list_datasets(&mut self, ctx: &egui::Context) {
        let path = self.path.clone();
        let slot = crate::Slot::default();
        let (out, ctx) = (slot.clone(), ctx.clone());
        self.error = None;
        self.listing = Some((path.clone(), slot));
        std::thread::spawn(move || {
            let mut session = Session::new(rustgis_engine::Project::default());
            let result = session.run("info.fileLayers", &json!({"path": path.trim()}));
            *out.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
            ctx.request_repaint();
        });
    }

    fn poll_listing(&mut self) {
        let Some((path, slot)) = &self.listing else { return };
        if path != &self.path {
            self.listing = None;
            return;
        }
        let result = slot.lock().unwrap_or_else(|e| e.into_inner()).take();
        let Some(result) = result else { return };
        self.listing = None;
        match result {
            Ok(result) => {
                self.tables.clear();
                if result["count"].as_u64().is_some_and(|n| n > 1) {
                    self.tables = result["layers"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(String::from)).collect();
                    if !self.tables.contains(&self.table) {
                        self.table.clear();
                    }
                }
            }
            Err(error) => self.error = Some(error),
        }
    }

    fn can_repair(&self) -> bool {
        !self.path.trim().is_empty() && self.listing.is_none() && (self.tables.is_empty() || !self.table.is_empty())
    }
}

/// Shows the captured source and runs the appropriate undoable repair command.
pub fn show(app: &mut RustGisApp, ctx: &egui::Context) {
    let Some(f) = app.dialogs.repair.take() else {
        app.dialogs.open = None;
        return;
    };
    if app.session.opened != f.opened
        || app.session.project.active_map != f.map.0
        || app.session.map().name != f.map.1
        || match f.target {
            Target::Layer(id) => app.session.map().layer(id).is_none(),
            Target::Tiles(index) => app.session.map().tiles.get(index).is_none_or(|t| {
                t.archive_file() != Some(f.saved.as_str()) || f.archive.as_ref().is_none_or(|(id, kind)| t.id != *id || t.kind != *kind)
            }),
        }
    {
        app.dialogs.open = None;
        return;
    }
    #[cfg(not(target_arch = "wasm32"))]
    let mut f = f;
    let mut open = true;
    let mut close = false;
    #[cfg(not(target_arch = "wasm32"))]
    let t = Tokens::get(ctx);
    egui::Window::new(tl!("Repair Data Source"))
        .id(egui::Id::unique("repair_source"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center() + egui::vec2(0.0, -40.0))
        .show(ctx, |ui| {
            ui.set_width(440.0);
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
            #[cfg(target_arch = "wasm32")]
            {
                ui.label(tl!("The web app can't link to files on this computer: add the file again with Add Data."));
                close = crate::widgets::dialog_button(ui, tl!("Close"), None, egui::vec2(84.0, 28.0)).clicked();
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                egui::Grid::new("repair_source_details").num_columns(2).spacing(egui::vec2(8.0, 8.0)).show(ui, |ui| {
                    ui.label(tl!("Layer"));
                    ui.add(egui::Label::new(&f.name).wrap());
                    ui.end_row();
                    ui.label(tl!("Saved location"));
                    ui.add(egui::Label::new(egui::RichText::new(&f.saved).monospace()).wrap().selectable(true));
                    ui.end_row();
                    if let Some(error) = &f.read_error {
                        ui.label(tl!("Read error"));
                        ui.add(egui::Label::new(egui::RichText::new(error).color(t.red)).wrap().selectable(true));
                        ui.end_row();
                    }
                });
                ui.separator();
                let replacement_label = ui.label(tl!("Replacement"));
                ui.add(egui::TextEdit::singleline(&mut f.path).desired_width(f32::INFINITY)).labelled_by(replacement_label.id);
                ui.horizontal(|ui| {
                    if ui.button(tl!("Choose File…")).clicked()
                        && let Some(path) = crate::dialogs::pick_open(&[], tl!("Data"))
                    {
                        f.path = path;
                    }
                    if ui.button(tl!("Choose Folder…")).clicked()
                        && let Some(path) = crate::dialogs::pick_folder()
                    {
                        f.path = path;
                    }
                });
                f.reset_listing();
                f.poll_listing();
                if matches!(f.target, Target::Layer(_)) {
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(!f.path.trim().is_empty() && f.listing.is_none(), egui::Button::new(tl!("List Datasets")))
                            .clicked()
                        {
                            f.list_datasets(ctx);
                        }
                        if f.listing.is_some() {
                            ui.spinner();
                            ui.label(tl!("Listing datasets…"));
                        }
                    });
                }
                if !f.tables.is_empty() {
                    let dataset_label = ui.label(tl!("Dataset"));
                    egui::ComboBox::from_id_salt("repair_source_dataset")
                        .selected_text(&f.table)
                        .width(ui.available_width())
                        .show_ui(ui, |ui| {
                            for table in &f.tables {
                                ui.selectable_value(&mut f.table, table.clone(), table);
                            }
                        })
                        .response
                        .labelled_by(dataset_label.id);
                }
                if let Some(error) = &f.error {
                    ui.colored_label(t.red, error);
                }
                ui.separator();
                let mut repair = false;
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                        repair = ui
                            .add_enabled_ui(f.can_repair(), |ui| {
                                crate::widgets::dialog_button(ui, tl!("Repair"), Some(t.accent), egui::vec2(84.0, 28.0)).clicked()
                            })
                            .inner;
                        close = crate::widgets::dialog_button(ui, tl!("Cancel"), None, egui::vec2(84.0, 28.0)).clicked();
                    });
                });
                if repair {
                    let result = match f.target {
                        Target::Layer(id) => {
                            let mut p = json!({"layer": id, "path": f.path.trim()});
                            if !f.table.is_empty() {
                                p["table"] = json!(f.table);
                            }
                            app.run("layer.repairSource", p)
                        }
                        Target::Tiles(index) => app.run("map.repairTiles", json!({"index": index, "path": f.path.trim()})),
                    };
                    match result {
                        Ok(result) => {
                            if result.get("started").is_none() && result.get("waiting").is_none() {
                                app.session.notify(crate::i18n::fmt("Repaired {name}", &[("name", &f.name)]), false);
                            }
                            close = true;
                        }
                        Err(error) => f.error = Some(error),
                    }
                }
            }
        });
    if !open || close {
        app.dialogs.open = None;
    } else {
        app.dialogs.repair = Some(f);
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn form() -> Form {
        Form {
            target: Target::Layer(1),
            opened: 0,
            map: (0, String::new()),
            archive: None,
            name: "Cities".into(),
            saved: "old.gpkg".into(),
            read_error: None,
            path: "replacement.gpkg".into(),
            listed_for: String::new(),
            listing: None,
            tables: Vec::new(),
            table: "cities".into(),
            error: None,
        }
    }

    fn pending(f: &mut Form) -> crate::Slot<Result<serde_json::Value, String>> {
        let slot = crate::Slot::default();
        f.listing = Some((f.path.clone(), slot.clone()));
        slot
    }

    #[test]
    fn editing_only_invalidates_discovery_and_keeps_the_table_default() {
        let mut f = form();
        f.reset_listing();
        let old = pending(&mut f);
        f.tables = vec!["cities".into(), "roads".into()];
        f.path = "another.gpkg".into();
        f.reset_listing();
        assert!(f.listing.is_none(), "typing must not start another lookup");
        assert!(f.tables.is_empty(), "the old file's list must disappear immediately");
        assert_eq!(f.table, "cities");

        // An old worker finishing after a path edit (even an A -> B -> A edit) is discarded.
        f.path = "replacement.gpkg".into();
        f.reset_listing();
        *old.lock().unwrap() = Some(Ok(json!({"count": 2, "layers": ["stale", "other"]})));
        f.poll_listing();
        assert!(f.tables.is_empty());
        assert_eq!(f.table, "cities");
        assert!(f.can_repair());
    }

    #[test]
    fn discovery_keeps_an_available_default_and_requires_a_known_multi_table_selection() {
        let mut f = form();
        f.reset_listing();
        let slot = pending(&mut f);
        assert!(!f.can_repair(), "Repair is disabled while discovery is in flight");
        *slot.lock().unwrap() = Some(Ok(json!({"count": 2, "layers": ["cities", "roads"]})));
        f.poll_listing();
        assert_eq!(f.table, "cities");
        assert!(f.can_repair());

        let slot = pending(&mut f);
        *slot.lock().unwrap() = Some(Ok(json!({"count": 2, "layers": ["towns", "roads"]})));
        f.poll_listing();
        assert!(f.table.is_empty(), "only a discovered list can invalidate the saved table");
        assert!(!f.can_repair());
        f.table = "towns".into();
        assert!(f.can_repair());
    }

    #[test]
    fn stale_result_cannot_replace_a_new_paths_discovery() {
        let mut f = form();
        f.reset_listing();
        let old = pending(&mut f);
        f.path = "another.gpkg".into();
        f.reset_listing();
        let current = pending(&mut f);
        *old.lock().unwrap() = Some(Ok(json!({"count": 2, "layers": ["stale", "other"]})));
        f.poll_listing();
        assert!(!f.can_repair());
        assert!(f.tables.is_empty());
        *current.lock().unwrap() = Some(Ok(json!({"count": 2, "layers": ["cities", "current"]})));
        f.poll_listing();
        assert_eq!(f.tables, ["cities", "current"]);
        assert_eq!(f.table, "cities");
    }
}
