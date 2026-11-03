//! Dialogs (Select By Attributes, Go To XY, bookmarks, fields, definition query, rename) and
//! native file pickers.

use egui::{Align2, vec2};
use rustgis_engine::Session;
use rustgis_engine::cmd::service_added::Removal;
use serde_json::{Value, json};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::RustGisApp;
use crate::theme::{Tokens, semibold};

#[derive(Default)]
pub struct Dialogs {
    /// The layer captured when Define Projection opens; changing selection must not retarget it.
    pub projection_layer: Option<u32>,
    /// Map-local layer IDs require the dialog to retain its original project and map as well.
    pub projection_map: Option<(u64, usize, String)>,
    pub geometry: Option<crate::geometrycalc::Form>,
    pub repair: Option<crate::repairsource::Form>,
    pub open: Option<&'static str>,
    pub layer: String,
    /// GeoPackage save/discard targets use IDs because display names can repeat.
    pub gpkg_layer: Option<u32>,
    /// The live instance captured with the map-local ID; Undo can make that ID reusable.
    pub gpkg_instance: Option<u64>,
    /// Layer IDs are map-local; source writes must stay in the captured project and map.
    pub gpkg_map: Option<(u64, usize, String)>,
    /// The source-backed layer last counted: its name for PostGIS, numeric ID for GeoPackage.
    pub counted: String,
    /// Revision after the last count command, which itself advances the session revision.
    pub counted_revision: Option<u64>,
    pub counted_instance: Option<u64>,
    pub text: String,
    pub a: String,
    pub b: String,
    pub c: String,
    pub d: String,
    pub e: String,
    /// Add From Database: load the PostgreSQL table whole, to be edited and saved back.
    pub edit: bool,
    pub mode: &'static str,
    pub flag: bool,
    pub error: Option<String>,
    pub info: Option<String>,
    /// The saved connection this dialog was opened on, by id (see `connectionui::picked`).
    pub connection: Option<String>,
    /// Choices found for the dialog (database or GeoPackage tables; delimited text columns).
    pub list: Vec<String>,
    /// The database or GeoPackage file whose tables were last listed.
    pub listed: Option<String>,
    /// Add Delimited Text's file, picked before the dialog opens.
    pub csv: Option<CsvFile>,
    /// Reset collapsed then closed dialogs when reopening
    pub reset_collapse: bool,
}

/// A file picked for Add Delimited Text.
pub struct CsvFile {
    pub name: String,
    pub source: CsvSource,
}

/// Where a file picked for Add Delimited Text is: on disk, or (in the browser) its bytes, kept
/// until it is added.
pub enum CsvSource {
    Path(String),
    Bytes(Vec<u8>),
}

impl Dialogs {
    /// Opens a dialog on the current selection, capturing the target of CRS changes.
    pub fn open(&mut self, which: &str, s: &Session) -> Result<(), String> {
        let active = s.active_layer.and_then(|id| s.map().layer(id));
        let name: &'static str = match which {
            "selectByAttributes" => "selectByAttributes",
            "goToXY" => "goToXY",
            "bookmark" => "bookmark",
            "addField" => "addField",
            "calculateField" => "calculateField",
            "definitionQuery" => "definitionQuery",
            "renameLayer" => "renameLayer",
            "renameMap" => "renameMap",
            "renameLayout" => "renameLayout",
            "renameServiceGroup" => "renameServiceGroup",
            "renameGroup" => "renameGroup",
            "coordinateSystem" => "coordinateSystem",
            "defineProjection" => "defineProjection",
            "addShapefile" => "addShapefile",
            "camera" => "camera",
            "addTiles" => "addTiles",
            "basemap" => "basemap",
            "body" => "body",
            "addSample" => "addSample",
            "layerTemplate" => "layerTemplate",
            "addPolyline" => "addPolyline",
            "addCsv" => "addCsv",
            "findPlace" => "findPlace",
            "geocode" => "geocode",
            "addUrl" => "addUrl",
            "addService" => "addService",
            "addDatabase" => "addDatabase",
            // Add From Database with Load for editing ticked.
            "pgAddLayer" => "addDatabase",
            "pgSave" => "pgSave",
            "pgDiscard" => "pgDiscard",
            "gpkgAddLayer" => "gpkgAddLayer",
            "gpkgSave" => "gpkgSave",
            "gpkgDiscard" => "gpkgDiscard",
            "chooseConnection" => "chooseConnection",
            "s3" => "s3",
            "zarr" => "zarr",
            "exportLayer" => "exportLayer",
            "saveProject" => "saveProject",
            other => return Err(format!("unknown dialog '{other}'")),
        };
        if matches!(name, "addField" | "calculateField" | "definitionQuery" | "renameLayer" | "exportLayer" | "defineProjection")
            && active.is_none()
        {
            return Err("select a layer in the Layers pane first".into());
        }
        if name == "exportLayer" && active.is_some_and(|l| l.is_raster()) {
            return Err("Export Features needs a vector layer (Data › Export Raster writes a raster)".into());
        }
        if matches!(name, "pgSave" | "pgDiscard") && !s.map().layers.iter().any(|l| l.postgis.is_some()) {
            return Err("the map has no PostGIS layer loaded for editing (Home › Add Data › Database…, Load for editing)".into());
        }
        if matches!(name, "gpkgSave" | "gpkgDiscard") && !s.map().layers.iter().any(|l| l.gpkg.is_some()) {
            return Err(tl!("the map has no GeoPackage layer loaded for editing (Home › Add Data › Edit GeoPackage Table…)").into());
        }
        if name == "selectByAttributes" && s.map().layers.is_empty() {
            return Err("the map has no layers".into());
        }
        *self = Dialogs { open: Some(name), mode: "new", edit: which == "pgAddLayer", reset_collapse: true, ..Dialogs::default() };
        self.layer = active.or_else(|| s.map().layers.first()).map(|l| l.name.clone()).unwrap_or_default();
        match name {
            "defineProjection" => {
                self.projection_layer = active.map(|l| l.id);
                self.projection_map = Some((s.opened, s.project.active_map, s.map().name.clone()));
                self.text = active
                    .and_then(|l| l.dataset.crs)
                    .map(|c| c.custom_def().map(|d| d.proj4.clone()).unwrap_or_else(|| c.label()))
                    .unwrap_or_else(|| "EPSG:4326".into());
            }
            "coordinateSystem" => {
                self.text = s.map().projection.code();
                // An EPSG code goes in the code box; presets are picked from the list.
                let is_code = self.text.to_ascii_lowercase().starts_with("epsg");
                self.a = if is_code { self.text.trim_start_matches(|c: char| !c.is_ascii_digit()).to_string() } else { String::new() };
            }
            "goToXY" => {
                let c = s.map().center_lonlat();
                self.a = format!("{:.4}", c.x);
                self.b = format!("{:.4}", c.y);
            }
            "bookmark" => self.text = rustgis_engine::cmd::navigate::new_bookmark_name(s.map()),
            "camera" => {
                let c = s.map().center_lonlat();
                self.a = format!("{:.5}", c.x);
                self.b = format!("{:.5}", c.y);
                self.c = format!("{:.0}", s.scale());
            }
            "addField" => self.b = "Double".into(),
            "calculateField" => self.a = active.and_then(|l| l.dataset.fields.last()).map(|f| f.name.clone()).unwrap_or_default(),
            "definitionQuery" => self.text = active.and_then(|l| l.definition_query.clone()).unwrap_or_default(),
            "renameLayer" => self.text = active.map(|l| l.name.clone()).unwrap_or_default(),
            // A file name can't hold a folder separator, which a project name may.
            "saveProject" => self.text = format!("{}.rgis", s.project.name.replace(['/', '\\'], "-")),
            "exportLayer" => self.mode = "geojson",
            // The active layer when it is from PostGIS, else the first that is.
            "pgSave" | "pgDiscard" => {
                let pg = |l: &&rustgis_engine::map::Layer| l.postgis.is_some();
                self.layer = active.filter(pg).or_else(|| s.map().layers.iter().find(pg)).map(|l| l.name.clone()).unwrap_or_default();
            }
            "gpkgSave" | "gpkgDiscard" => {
                let gpkg = |l: &&rustgis_engine::map::Layer| l.gpkg.is_some();
                let layer = active.filter(gpkg).or_else(|| s.map().layers.iter().find(gpkg));
                self.gpkg_layer = layer.map(|l| l.id);
                self.gpkg_instance = layer.map(|l| l.instance_id());
                self.gpkg_map = Some((s.opened, s.project.active_map, s.map().name.clone()));
            }
            // A table of addresses is what's usually geocoded: picked by its id (`flag` says the
            // choice is a table), since names can repeat.
            "geocode" => {
                if let Some(t) = s.project.tables.last() {
                    (self.layer, self.flag) = (t.id.to_string(), true);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn gpkg_owner(&self, s: &Session) -> Result<(), String> {
        if !self
            .gpkg_map
            .as_ref()
            .is_some_and(|(project, index, name)| *project == s.opened && *index == s.project.active_map && *name == s.map().name)
        {
            return Err(tl!("The active project or map changed; reopen the GeoPackage dialog for the intended layer.").into());
        }
        Ok(())
    }

    fn gpkg_target(&self, s: &Session) -> Result<u32, String> {
        self.gpkg_owner(s)?;
        let id = self.gpkg_layer.ok_or_else(|| tl!("Choose a GeoPackage layer").to_string())?;
        if !s.map().layer(id).is_some_and(|l| l.gpkg.is_some() && Some(l.instance_id()) == self.gpkg_instance) {
            return Err(tl!("The selected GeoPackage layer is no longer available; choose a layer").to_string());
        }
        Ok(id)
    }
}

/// Whether `error` is a target refusal from [`Dialogs::gpkg_target`], which a later valid
/// target (Redo restoring the layer) makes stale. Save failures and other errors aren't.
fn is_gpkg_target_error(error: &str) -> bool {
    [
        tl!("The active project or map changed; reopen the GeoPackage dialog for the intended layer."),
        tl!("Choose a GeoPackage layer"),
        tl!("The selected GeoPackage layer is no longer available; choose a layer"),
    ]
    .contains(&error)
}

/// Draws the active dialog and leaves it open on validation failure or a newly opened dialog.
pub fn show(app: &mut RustGisApp, ctx: &egui::Context) {
    let Some(which) = app.dialogs.open else { return };
    if which == "calculateGeometry" {
        crate::geometrycalc::show(app, ctx);
        return;
    }
    if which == "repairSource" {
        crate::repairsource::show(app, ctx);
        return;
    }
    let title = match which {
        "selectByAttributes" => "Select By Attributes",
        "goToXY" => "By Coordinates",
        "bookmark" => "New Bookmark",
        "addField" => "Add Field",
        "calculateField" => "Calculate Field",
        "definitionQuery" => "Definition Query",
        "coordinateSystem" => "Coordinate System",
        "defineProjection" => "Define Projection",
        "addShapefile" => "Import Shapefile with Projection",
        "camera" => "Camera",
        "addTiles" => "Add Tile Layer",
        "basemap" => "Basemap",
        "body" => "Celestial Body",
        "addSample" => "Add Sample Data",
        "layerTemplate" => "Layer Template",
        "addPolyline" => "Add Encoded Polyline",
        "addCsv" => "Add Delimited Text",
        "findPlace" => "By Place or Address",
        "geocode" => "Geocode Addresses",
        "addUrl" => "Add Data From URL",
        "addService" => "Add From Service",
        "addDatabase" => "Add From Database",
        "pgSave" => "Save to PostGIS",
        "pgDiscard" => "Discard PostGIS Edits",
        "gpkgAddLayer" => "Edit GeoPackage Table",
        "gpkgSave" => "Save to GeoPackage",
        "gpkgDiscard" => "Discard GeoPackage Edits",
        "newConnection" => "New Connection",
        "editConnection" => "Edit Connection",
        "chooseConnection" => "Choose Connection",
        "s3" => "S3 Buckets",
        "s3Connections" => "Choose S3 connection",
        "zarr" => "Add Zarr Variable",
        "exportLayer" => "Export Features",
        "saveProject" => "Save Project As",
        "renameMap" => "Rename Map",
        "renameLayout" => "Rename Layout",
        "renameServiceGroup" => "Rename Service Group",
        "renameGroup" => "Rename Group",
        _ => "Rename Layer",
    };

    let is_add_service = which == "addService";

    let t = Tokens::get(ctx);
    let mut open = true;
    let mut close = false;

    let window_id = egui::Id::unique(("dialog", which));

    if app.dialogs.reset_collapse {
        let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(ctx, window_id.with("collapsing"), true);
        state.set_open(true);
        state.store(ctx);
        app.dialogs.reset_collapse = false;
    }

    // Opens centred and can be moved; each dialog remembers where it was put.
    let window = egui::Window::new(tl!(title))
        .id(window_id)
        .open(&mut open)
        .collapsible(is_add_service)
        .resizable(false)
        .pivot(Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center() + vec2(0.0, -40.0));
    let window = if is_add_service { window.auto_sized() } else { window };
    window.show(ctx, |ui| {
        ui.set_width(match which {
            "coordinateSystem" | "zarr" => 660.0,
            "addTiles" | "addUrl" | "addService" | "s3" => 560.0,
            "newConnection" | "editConnection" | "chooseConnection" | "s3Connections" => 520.0,
            _ => 440.0,
        });
        ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
        match which {
            "selectByAttributes" => close = select_by_attributes(app, ui, &t),
            "goToXY" => {
                egui::Grid::new("xy").num_columns(2).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
                    let input_label = ui.label(tl!("Longitude (X)"));
                    ui.add(egui::TextEdit::singleline(&mut app.dialogs.a).desired_width(200.0)).labelled_by(input_label.id);
                    ui.end_row();
                    let input_label = ui.label(tl!("Latitude (Y)"));
                    ui.add(egui::TextEdit::singleline(&mut app.dialogs.b).desired_width(200.0)).labelled_by(input_label.id);
                    ui.end_row();
                    let input_label = ui.label(tl!("Scale 1:"));
                    ui.add(egui::TextEdit::singleline(&mut app.dialogs.c).hint_text(tl!("keep current")).desired_width(200.0)).labelled_by(input_label.id);
                    ui.end_row();
                });
                close = buttons(app, ui, "Go", |app| {
                    let mut p = json!({"lon": app.dialogs.a.trim(), "lat": app.dialogs.b.trim()});
                    if let Ok(s) = app.dialogs.c.trim().replace(',', "").parse::<f64>() {
                        p["scale"] = json!(s);
                    }
                    app.run("map.goToXY", p).map(|_| ())
                });
            }
            "coordinateSystem" => close = coordinate_system(app, ui, &t),
            "addShapefile" => {
                ui.label(tl!("Choose the source coordinate system before importing. It replaces a missing or incorrect .prj without changing the original file."));
                let input_label = ui.label(tl!("Source coordinate system"));
                let response = ui.add(egui::TextEdit::singleline(&mut app.dialogs.text).hint_text("EPSG:2263").desired_width(f32::INFINITY)).labelled_by(input_label.id);
                response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, tl!("Source coordinate system")));
                ui.label(tl!("In the browser, pick the .shp and .dbf together, with any sidecars, or pick a ZIP. The override applies to every Shapefile in the ZIP."));
                close = buttons(app, ui, "Choose File…", |app| app.run("ui.importShapefile", json!({"crs": app.dialogs.text.trim()})).map(|_| ()));
            }
            "defineProjection" => {
                ui.label(egui::RichText::new(&app.dialogs.layer).strong());
                ui.label(tl!("Interpret the layer's native coordinates in this coordinate system. This changes where it draws; the source file is unchanged."));
                let input_label = ui.label(tl!("Source coordinate system"));
                let label = ui.add(egui::TextEdit::singleline(&mut app.dialogs.text).hint_text("EPSG:2263").desired_width(f32::INFINITY)).labelled_by(input_label.id);
                label.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, tl!("Source coordinate system")));
                ui.label(egui::RichText::new(tl!("Geometry lost during import cannot be recovered here. Use Import Original File to choose its coordinate system before reading it. Streamed layers must first be loaded into memory.")).weak());
                if ui.button(tl!("Import Original File…")).clicked() {
                    let crs = app.dialogs.text.clone();
                    if app.dialogs.open("addShapefile", &app.session).is_ok() {
                        app.dialogs.text = crs;
                    }
                }
                close = buttons(app, ui, "Apply", |app| {
                    if !app.dialogs.projection_map.as_ref().is_some_and(|(project, index, name)| {
                        *project == app.session.opened && *index == app.session.project.active_map && *name == app.session.map().name
                    }) {
                        return Err(tl!("The active project or map changed; reopen Define Projection for the intended layer.").into());
                    }
                    let id = app.dialogs.projection_layer.ok_or(tl!("No layer selected"))?;
                    app.run("layer.defineProjection", json!({"layer": id, "crs": app.dialogs.text.trim()})).map(|_| ())
                });
            }
            "basemap" | "body" | "addSample" | "layerTemplate" => {
                // Menu items close their nearest closable parent. Keep that local to the
                // gallery so picking an item doesn't persistently collapse the Window's body.
                let gallery = ui.scope_builder(egui::UiBuilder::new().closable(), |ui| {
                    if which == "basemap" {
                        egui::ScrollArea::vertical().max_height((ctx.content_rect().height() - 180.0).max(120.0)).show(ui, |ui| {
                            crate::ribbon::basemap_gallery(ui, app);
                        });
                    } else if which == "body" {
                        crate::ribbon::body_menu(ui, app);
                    } else if which == "addSample" {
                        crate::ribbon::sample_data_gallery(ui, app);
                    } else {
                        crate::ribbon::layer_templates_gallery(ui, app);
                    }
                });
                close = gallery.response.should_close();
            }
            "addPolyline" => {
                ui.label(tl!("Encoded polyline"));
                ui.add(text_box(&mut app.dialogs.text).desired_width(f32::INFINITY).desired_rows(3));
                ui.label(tl!("Name"));
                ui.text_edit_singleline(&mut app.dialogs.a);
                ui.checkbox(&mut app.dialogs.flag, tl!("Precision 6 (OSRM, Valhalla)"));
                close = buttons(app, ui, "Add", |app| {
                    let mut p = json!({"polyline": app.dialogs.text.trim(), "precision": if app.dialogs.flag { 6 } else { 5 }});
                    if !app.dialogs.a.trim().is_empty() {
                        p["name"] = json!(app.dialogs.a.trim());
                    }
                    app.run("layer.addEncodedPolyline", p).map(|_| ())
                });
            }
            "addTiles" => close = add_tiles(app, ui),
            "addCsv" => close = add_csv(app, ui),
            "findPlace" => close = crate::geocodeui::find_place(app, ui),
            "geocode" => close = crate::geocodeui::geocode(app, ui),
            "addUrl" => close = add_url(app, ui),
            "addService" => close = add_service(app, ui),
            "addDatabase" => close = add_database(app, ui),
            "pgSave" => close = pg_save(app, ui),
            "pgDiscard" => close = pg_discard(app, ui),
            "gpkgAddLayer" => close = gpkg_add_layer(app, ui),
            "gpkgSave" => close = gpkg_save(app, ui),
            "gpkgDiscard" => close = gpkg_discard(app, ui),
            "newConnection" | "editConnection" => close = crate::connectionui::dialog(app, ui),
            "chooseConnection" => close = crate::connectionui::chooser(app, ui),
            "s3" => crate::s3ui::dialog(app, ui),
            "s3Connections" => close = crate::connectionui::s3_chooser(app, ui),
            "zarr" => close = crate::zarrui::dialog(app, ui),
            "exportLayer" => close = export_layer(app, ui),
            "camera" => {
                egui::Grid::new("cam").num_columns(2).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
                    for (label, v) in [("Longitude", &mut app.dialogs.a), ("Latitude", &mut app.dialogs.b), ("Scale 1:", &mut app.dialogs.c)] {
                        let input_label = ui.label(tl!(label));
                        ui.add(egui::TextEdit::singleline(v).desired_width(160.0)).labelled_by(input_label.id);
                        ui.end_row();
                    }
                    ui.label(tl!("Projection"));
                    ui.label(app.session.map().projection.label());
                    ui.end_row();
                });
                close = buttons(app, ui, "Apply", |app| {
                    // Empty keeps the current value; anything else must be a number.
                    let n = |s: &str, what: &str| -> Result<Option<f64>, String> {
                        let s = s.trim();
                        if s.is_empty() {
                            return Ok(None);
                        }
                        s.parse::<f64>().ok().filter(|v| v.is_finite()).map(Some).ok_or_else(|| crate::i18n::fmt("{what} must be a number", &[("what", tl_raw!(what))]))
                    };
                    let (lon, lat, scale) = (n(&app.dialogs.a, "Longitude")?, n(&app.dialogs.b, "Latitude")?, n(&app.dialogs.c, "Scale")?);
                    app.run("map.camera", json!({"lon": lon, "lat": lat, "scale": scale})).map(|_| ())
                });
            }
            "bookmark" => {
                let input_label = ui.label(tl!("Name"));
                ui.add(egui::TextEdit::singleline(&mut app.dialogs.text).desired_width(f32::INFINITY)).labelled_by(input_label.id);
                // What the place is (#351); Enter creates the bookmark, Shift+Enter starts a line.
                ui.label(tl!("Description"));
                let hint = tl!("Optional. Shift+Enter starts a new line.");
                ui.add(text_box(&mut app.dialogs.a).hint_text(hint).desired_rows(3).desired_width(f32::INFINITY));
                close = buttons(app, ui, "Create", |app| {
                    app.run("bookmark.add", json!({"name": app.dialogs.text, "description": app.dialogs.a})).map(|_| ())
                });
            }
            "addField" => {
                egui::Grid::new("af").num_columns(2).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
                    let input_label = ui.label(tl!("Field name"));
                    ui.add(egui::TextEdit::singleline(&mut app.dialogs.a).desired_width(220.0)).labelled_by(input_label.id);
                    ui.end_row();
                    let input_label = ui.label(tl!("Data type"));
                    egui::ComboBox::from_id_salt("af_type").selected_text(app.dialogs.b.clone()).show_ui(ui, |ui| {
                        for ty in ["Long", "Double", "Text", "Boolean"] {
                            ui.selectable_value(&mut app.dialogs.b, ty.to_string(), ty);
                        }
                    }).response.labelled_by(input_label.id);
                    ui.end_row();
                });
                close = buttons(app, ui, "Add", |app| app.run("edit.addField", json!({"name": app.dialogs.a, "type": app.dialogs.b})).map(|_| ()));
            }
            "calculateField" => {
                let fields: Vec<String> = app.session.active_layer.and_then(|id| app.session.map().layer(id)).map(|l| l.dataset.fields.iter().map(|f| f.name.clone()).collect()).unwrap_or_default();
                ui.horizontal(|ui| {
                    let input_label = ui.label(tl!("Field"));
                    egui::ComboBox::from_id_salt("cf_field").selected_text(app.dialogs.a.clone()).width(220.0).show_ui(ui, |ui| {
                        for f in &fields {
                            ui.selectable_value(&mut app.dialogs.a, f.clone(), f);
                        }
                    }).response.labelled_by(input_label.id);
                });
                let input_label = ui.label(format!("{} =", app.dialogs.a));
                let edit = egui::TextEdit::singleline(&mut app.dialogs.text)
                    .hint_text(tl!("e.g.  pop_est / 1000000   or   upper(name) || ' ' || round(area, 1)"))
                    .char_limit(rustgis_engine::data::query::MAX_LEN)
                    .font(egui::TextStyle::Monospace)
                    .desired_width(f32::INFINITY);
                ui.add(edit).labelled_by(input_label.id);
                let text = app.dialogs.text.trim();
                let reads = app.session.active_layer.and_then(|id| app.session.map().layer(id)).filter(|_| !text.is_empty()).map(|l| rustgis_engine::cmd::edit::check_calculation(text, &l.dataset));
                if let Some(Err(e)) = reads {
                    let t = Tokens::get(ui.ctx());
                    ui.label(egui::RichText::new(crate::i18n::fmt("Invalid: {error}", &[("error", &e)])).small().color(t.red));
                }
                ui.label(egui::RichText::new(tl!("Fields, numbers, 'text', + - * / % ^, || to join text, comparisons, CASE and functions such as round(), upper(), if() or scale_linear(). Applies to the selected rows, or every row when none is selected.")).small().weak());
                close = buttons(app, ui, "Calculate", |app| app.run("edit.calculateField", json!({"field": app.dialogs.a, "expression": app.dialogs.text})).map(|_| ()));
            }
            "definitionQuery" => {
                ui.label(egui::RichText::new(crate::i18n::fmt("Only features matching this expression draw in {layer}.", &[("layer", &app.dialogs.layer)])).weak());
                let hint = tl!("e.g. continent = 'Europe'");
                let edit = text_box(&mut app.dialogs.text).hint_text(hint).desired_rows(3).font(egui::TextStyle::Monospace);
                ui.add(edit.desired_width(f32::INFINITY));
                field_chips(app, ui);
                close = buttons(app, ui, "Apply", |app| app.run("layer.definitionQuery", json!({"where": app.dialogs.text})).map(|_| ()));
            }
            "saveProject" => close = save_project(app, ui),
            // `a` is the map or layout being renamed; an empty or unchanged name just closes.
            "renameMap" | "renameLayout" => {
                let input_label = ui.label(tl!("Name"));
                ui.add(egui::TextEdit::singleline(&mut app.dialogs.text).desired_width(f32::INFINITY)).labelled_by(input_label.id);
                close = buttons(app, ui, "Rename", |app| {
                    let (old, new) = (app.dialogs.a.clone(), app.dialogs.text.trim().to_string());
                    if new.is_empty() || new == old {
                        return Ok(());
                    }
                    let r = if which == "renameMap" {
                        app.run("map.rename", json!({"map": old, "name": new}))
                    } else {
                        app.run("layout.rename", json!({"layout": old, "newName": new}))
                    };
                    r.map(|_| ())
                });
            }
            "renameServiceGroup" => {
                let input_label = ui.label(tl!("Name"));
                ui.add(egui::TextEdit::singleline(&mut app.dialogs.text).desired_width(f32::INFINITY)).labelled_by(input_label.id);
                close = buttons(app, ui, "Rename", |app| {
                    let group = app.dialogs.layer.parse::<u32>().map_err(|e| e.to_string())?;
                    app.run("service.group", json!({"group":group,"name":app.dialogs.text})).map(|_| ())
                });
            }
            "renameGroup" => {
                let input_label = ui.label(tl!("Name"));
                ui.add(egui::TextEdit::singleline(&mut app.dialogs.text).desired_width(f32::INFINITY)).labelled_by(input_label.id);
                close = buttons(app, ui, "Rename", |app| {
                    let group = app.dialogs.layer.parse::<u32>().map_err(|e| e.to_string())?;
                    app.run("group.set", json!({"group": group, "name": app.dialogs.text})).map(|_| ())
                });
            }
            _ => {
                let input_label = ui.label(tl!("Name"));
                ui.add(egui::TextEdit::singleline(&mut app.dialogs.text).desired_width(f32::INFINITY)).labelled_by(input_label.id);
                close = buttons(app, ui, "Rename", |app| app.run("layer.rename", json!({"name": app.dialogs.text})).map(|_| ()));
            }
        }
        if let Some(e) = &app.dialogs.error {
            ui.label(egui::RichText::new(e).color(t.red));
        }
        if let Some(i) = &app.dialogs.info {
            ui.label(egui::RichText::new(i).color(t.green));
        }
    });
    if (!open || close) && app.dialogs.open == Some(which) {
        app.dialogs.open = None;
    }
}

/// Save Project As in the browser, which has no save dialog: the file name to download the
/// project as. The browser decides where downloads go.
fn save_project(app: &mut RustGisApp, ui: &mut egui::Ui) -> bool {
    ui.label(tl!("File name"));
    let mut edit = egui::TextEdit::singleline(&mut app.dialogs.text).desired_width(f32::INFINITY).show(ui);
    // Opened: the name is selected, so typing replaces it.
    if app.dialogs.mode == "new" {
        app.dialogs.mode = "";
        edit.response.request_focus();
        let end = app.dialogs.text.chars().count();
        edit.state.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(end))));
        edit.state.store(ui.ctx(), edit.response.id);
    }
    ui.label(egui::RichText::new(tl!("The project downloads as this file, to the folder your browser saves downloads in.")).small().weak());
    buttons(app, ui, "Save", |app| {
        let file = app.dialogs.text.clone();
        app.save_download(&file)
    })
}

/// The formats Export Features writes: (name, extension). Format names aren't translated.
pub const EXPORT_FORMATS: &[(&str, &str)] = &[
    ("GeoJSON", "geojson"),
    ("GeoPackage", "gpkg"),
    ("GeoParquet", "parquet"),
    ("Shapefile", "shp"),
    ("FlatGeobuf", "fgb"),
    ("CSV", "csv"),
    ("KML", "kml"),
    ("KMZ", "kmz"),
    ("GPX", "gpx"),
];

/// The formats Export Features offers: all of [`EXPORT_FORMATS`] on the desktop. The web app
/// leaves out GeoPackage, written with SQLite, and FlatGeobuf, whose writer needs a temporary
/// file; the browser has neither.
pub(crate) fn export_formats(web: bool) -> impl Iterator<Item = &'static (&'static str, &'static str)> {
    EXPORT_FORMATS.iter().filter(move |(_, e)| !(web && matches!(*e, "gpkg" | "fgb")))
}

/// Export Features: the active layer, in a format picked here (a file's type isn't changed by
/// the filter chosen in a save dialog, and the web app has no save dialog).
fn export_layer(app: &mut RustGisApp, ui: &mut egui::Ui) -> bool {
    let web = app.services.download.is_some();
    egui::Grid::new("export_layer").num_columns(2).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
        ui.label(tl!("Layer"));
        ui.label(&app.dialogs.layer);
        ui.end_row();
        let input_label = ui.label(tl!("Format"));
        let shown = EXPORT_FORMATS.iter().find(|(_, e)| *e == app.dialogs.mode).map(|(l, _)| *l).unwrap_or("GeoJSON");
        egui::ComboBox::from_id_salt("export_format")
            .selected_text(shown)
            .width(220.0)
            .show_ui(ui, |ui| {
                for (label, ext) in export_formats(web) {
                    ui.selectable_value(&mut app.dialogs.mode, *ext, *label);
                }
            })
            .response
            .labelled_by(input_label.id);
        ui.end_row();
    });
    if app.dialogs.mode == "shp" && web {
        ui.label(egui::RichText::new(tl!("The Shapefile's files download as one zip.")).small().weak());
    }
    buttons(app, ui, if web { "Download" } else { "Export…" }, |app| {
        app.run("ui.exportLayer", json!({"format": app.dialogs.mode})).map(|_| ())
    })
}

/// Add Delimited Text's separators: (code, name).
const SEPARATORS: [(&str, &str); 5] = [("auto", "Automatic"), (",", "Comma"), (";", "Semicolon"), ("\t", "Tab"), ("|", "Pipe")];

/// Opens Add Delimited Text for a file picked: its columns, with the ones found by name picked.
pub fn open_csv(app: &mut RustGisApp, name: &str, source: CsvSource) -> Result<Value, String> {
    app.run("ui.dialog", json!({"name": "addCsv"}))?;
    let file = CsvFile { name: name.to_string(), source };
    app.dialogs.mode = "auto";
    show_columns(&mut app.dialogs, csv_columns(&file, "auto"));
    app.dialogs.csv = Some(file);
    Ok(Value::Null)
}

/// The columns of a file picked for Add Delimited Text, split at `separator` (a code of
/// [`SEPARATORS`]).
fn csv_columns(file: &CsvFile, separator: &str) -> Result<Value, String> {
    let p = if separator == "auto" { json!({}) } else { json!({"delimiter": separator}) };
    match &file.source {
        CsvSource::Path(path) => rustgis_engine::cmd::layer::columns_in_file(path, &p),
        CsvSource::Bytes(bytes) => rustgis_engine::cmd::layer::columns_of(&file.name, bytes, &p),
    }
}

/// Puts the columns read (`info.fileColumns`' answer) in the dialog, the ones found picked; an
/// error leaves none.
fn show_columns(d: &mut Dialogs, found: Result<Value, String>) {
    let text = |v: &Value| v.as_str().unwrap_or("").to_string();
    match found {
        Ok(v) => {
            d.list = v["columns"].as_array().map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
            (d.a, d.b, d.c) = (text(&v["x"]), text(&v["y"]), text(&v["wkt"]));
            if let Some((code, _)) = SEPARATORS.iter().find(|(c, _)| v["delimiter"].as_str() == Some(*c)) {
                d.mode = *code;
            }
            d.error = None;
        }
        Err(e) => {
            d.list.clear();
            (d.a, d.b, d.c) = (String::new(), String::new(), String::new());
            d.error = Some(e);
        }
    }
}

/// After the columns are read again: the x/y columns (or the WKT column) picked before stay picked
/// when the file still has them; otherwise the ones found are.
fn keep_picked(d: &mut Dialogs, [x, y, wkt]: [String; 3]) {
    let has = |c: &String| !c.is_empty() && d.list.contains(c);
    let (xy, w) = (has(&x) && has(&y), has(&wkt));
    if xy {
        (d.a, d.b, d.c) = (x, y, String::new());
    } else if w {
        (d.a, d.b, d.c) = (String::new(), String::new(), wkt);
    }
}

/// Add Delimited Text, for the file picked: which of its columns hold the coordinates (or WKT
/// geometry), their coordinate system and the separator.
fn add_csv(app: &mut RustGisApp, ui: &mut egui::Ui) -> bool {
    let Some(name) = app.dialogs.csv.as_ref().map(|f| f.name.clone()) else {
        // Opened without a file (`ui.dialog`): the file first.
        ui.label(tl!("Choose a delimited text file or a spreadsheet, then pick its columns."));
        let close = buttons(app, ui, "Choose File…", |app| app.run("ui.addDelimited", Value::Null).map(|_| ()));
        // On the desktop the dialog is open again, for the file picked.
        return close && app.dialogs.csv.is_none();
    };
    let sheet = rustgis_engine::data::sheets::is_sheet_name(&name);
    let shown = std::path::Path::new(&name).file_name().and_then(|n| n.to_str()).unwrap_or(&name).to_string();
    ui.label(egui::RichText::new(shown).strong());
    ui.label(egui::RichText::new(tl!("Columns named like longitude/latitude, easting/northing or geometry are picked for you.")).weak());
    let mut reread = false;
    egui::Grid::new("csv").num_columns(2).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
        let columns = app.dialogs.list.clone();
        for (i, label) in ["X column", "Y column", "WKT column"].into_iter().enumerate() {
            let input_label = ui.label(tl!(label));
            let d = &mut app.dialogs;
            let v = match i {
                0 => &mut d.a,
                1 => &mut d.b,
                _ => &mut d.c,
            };
            let before = v.clone();
            let selected = if v.is_empty() { tl!("None").to_string() } else { v.clone() };
            egui::ComboBox::from_id_salt(("csv_column", i))
                .selected_text(selected)
                .width(240.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(v, String::new(), tl!("None"));
                    for c in &columns {
                        ui.selectable_value(v, c.clone(), c.as_str());
                    }
                })
                .response
                .labelled_by(input_label.id);
            // x/y columns or a WKT column, not both.
            if *v != before && !v.is_empty() {
                if i == 2 {
                    (d.a, d.b) = (String::new(), String::new());
                } else {
                    d.c.clear();
                }
            }
            ui.end_row();
        }
        let input_label = ui.label(tl!("Coordinate system"));
        ui.add(egui::TextEdit::singleline(&mut app.dialogs.text).hint_text("EPSG:4326").desired_width(240.0)).labelled_by(input_label.id);
        ui.end_row();
        // A sheet has no separator.
        if !sheet {
            let input_label = ui.label(tl!("Separator"));
            let before = app.dialogs.mode;
            let shown = SEPARATORS.iter().find(|(c, _)| *c == app.dialogs.mode).map(|(_, l)| tl!(*l).to_string()).unwrap_or_default();
            egui::ComboBox::from_id_salt("csv_sep")
                .selected_text(shown)
                .width(240.0)
                .show_ui(ui, |ui| {
                    for (code, label) in SEPARATORS {
                        ui.selectable_value(&mut app.dialogs.mode, code, tl!(label));
                    }
                })
                .response
                .labelled_by(input_label.id);
            reread = app.dialogs.mode != before;
            ui.end_row();
        }
    });
    // Another separator splits the header differently: its columns are read again.
    if reread && let Some(file) = &app.dialogs.csv {
        let found = csv_columns(file, app.dialogs.mode);
        let picked = [app.dialogs.a.clone(), app.dialogs.b.clone(), app.dialogs.c.clone()];
        show_columns(&mut app.dialogs, found);
        keep_picked(&mut app.dialogs, picked);
    }
    buttons(app, ui, "Add", |app| {
        let mut p = json!({});
        for (key, v) in [("x", &app.dialogs.a), ("y", &app.dialogs.b), ("wkt", &app.dialogs.c), ("crs", &app.dialogs.text)] {
            if !v.trim().is_empty() {
                p[key] = json!(v.trim());
            }
        }
        if app.dialogs.mode != "auto" && !sheet {
            p["delimiter"] = json!(app.dialogs.mode);
        }
        let file = app.dialogs.csv.take().ok_or("choose a file first")?;
        let r = match &file.source {
            CsvSource::Path(path) => {
                p["path"] = json!(path);
                app.run("layer.addData", p)
            }
            CsvSource::Bytes(bytes) => app.open_bytes_with(&file.name, bytes, Some(p)),
        };
        // Kept for another try.
        if r.is_err() {
            app.dialogs.csv = Some(file);
        }
        r.map(|_| ())
    })
}

/// Add From Service connects to a directory's folder or service (or back to one): the way back
/// becomes `way` once connected, and stays `before` when connecting fails.
fn go_to(app: &mut RustGisApp, url: &str, kind: &str, way: Vec<(String, &'static str)>, before: Vec<(String, &'static str)>) {
    app.dialogs.info = None;
    match app.run("ui.addService", json!({"url": url, "type": kind})) {
        Ok(_) => app.ui.service_browse.back = way,
        Err(e) => {
            app.ui.service_browse.back = before;
            app.dialogs.error = Some(e);
        }
    }
}

/// How many of a service's layers Add From Service lists.
const SHOWN_LAYERS: usize = 500;

/// The listed layers matching `filter` (words their title or name contains), with their place in
/// the listing.
fn matching_rows<'a>(layers: &'a [Value], filter: &str) -> Vec<(usize, &'a Value)> {
    let words: Vec<String> = filter.split_whitespace().map(str::to_lowercase).collect();
    let text = |l: &Value, k: &str| l.get(k).and_then(Value::as_str).unwrap_or("").to_lowercase();
    layers
        .iter()
        .enumerate()
        .filter(|(_, l)| words.iter().all(|w| text(l, "title").contains(w.as_str()) || text(l, "name").contains(w.as_str())))
        .collect()
}

/// A fingerprint of what `service_added::added` reads from the map (each layer's id and source,
/// and each tile layer's address). The UI changes them through `map_mut` and `layer_mut`, which
/// leave the session revision alone, so Add From Service's rows are measured against this too.
fn map_stamp(map: &rustgis_engine::map::Map) -> u64 {
    let mut h = DefaultHasher::new();
    map.layers.len().hash(&mut h);
    for l in &map.layers {
        l.id.hash(&mut h);
        l.dataset.source.hash(&mut h);
    }
    map.tiles.len().hash(&mut h);
    for t in &map.tiles {
        t.url.hash(&mut h);
    }
    h.finish()
}

/// The rows Add From Service shows (the first [`SHOWN_LAYERS`] matching the filter), with the
/// command that removes each when it is on the map (`service_added::added`), worked out again only
/// when the session, the map, the listing or the filter changed, not on every frame.
fn refresh_added(app: &mut RustGisApp) {
    let revision = app.session.revision;
    let map = map_stamp(app.session.map());
    let Some(conn) = app.ui.service.as_mut() else { return };
    let filter = &app.ui.service_browse.filter;
    if conn.added.as_ref().is_some_and(|a| a.revision == revision && a.map == map && a.filter == *filter) {
        return;
    }
    let layers: &[Value] = conn.listing.as_ref().and_then(|v| v.get("layers")).and_then(Value::as_array).map_or(&[], Vec::as_slice);
    let matching = matching_rows(layers, filter);
    let matched = matching.len();
    let shown: Vec<usize> = matching.into_iter().take(SHOWN_LAYERS).map(|(i, _)| i).collect();
    let removals = rustgis_engine::cmd::service_added::added(app.session.map(), layers, &shown);
    let rows = shown.into_iter().map(|i| (i, removals.get(i).cloned().flatten())).collect();
    conn.added = Some(crate::ServiceAdded { revision, map, filter: filter.clone(), matched, rows });
}

/// Add From Service: a WMS, WMTS, WFS, WCS, OGC API Features or ArcGIS REST service by its address
/// (or a catalogue searched for them, `cswui`).
/// Connect fetches its capabilities in the background; its layers are then listed, each with an
/// Add button (map layers become tile layers, features are downloaded).
fn add_service(app: &mut RustGisApp, ui: &mut egui::Ui) -> bool {
    const KINDS: [(&str, &str); 8] = [
        ("auto", "Automatic"),
        ("wms", "WMS"),
        ("wmts", "WMTS"),
        ("wfs", "WFS"),
        ("wcs", "WCS"),
        ("ogcapi", "OGC API Features and Tiles"),
        ("arcgis", "ArcGIS REST"),
        ("csw", "Catalogue search (CSW)"),
    ];
    if app.dialogs.mode == "new" {
        app.dialogs.mode = "auto";
    }
    let list = crate::sourcespane::connections_snapshot(app);
    let address = if app.dialogs.mode == "csw" { &app.ui.csw.url } else { &app.dialogs.text };
    let current = crate::connectionui::picked(&list, app.dialogs.connection.as_deref(), address);
    if let Some(c) = crate::connectionui::picker(ui, "service_saved", &list, crate::connectionui::SERVICE_KINDS, current) {
        if let Err(e) = crate::connectionui::open_connection(app, &c) {
            app.dialogs.error = Some(e);
        }
        return false;
    }
    let kinds = |app: &mut RustGisApp, ui: &mut egui::Ui| {
        let input_label = ui.label(tl!("Type"));
        let shown = KINDS.iter().find(|(c, _)| *c == app.dialogs.mode).map(|(_, l)| tl!(*l).to_string()).unwrap_or_default();
        egui::ComboBox::from_id_salt("service_kind")
            .selected_text(shown)
            .width(240.0)
            .show_ui(ui, |ui| {
                for (code, label) in KINDS {
                    ui.selectable_value(&mut app.dialogs.mode, code, tl!(label));
                }
            })
            .response
            .labelled_by(input_label.id);
    };
    // A catalogue: searched for records, whose links connect here or download.
    if app.dialogs.mode == "csw" {
        ui.label(egui::RichText::new(tl!("A catalogue's (CSW 2.0.2) address: search its records by words and, if you like, the map's area, then add the services and files they link to.")).weak());
        crate::cswui::form(app, ui, kinds);
        return false;
    }
    ui.label(egui::RichText::new(tl!("A map, feature or coverage service's address: WMS, WMTS, WFS, WCS, OGC API Features or Tiles, or an ArcGIS FeatureServer, MapServer, ImageServer or VectorTileServer (or a services directory, to browse).")).weak());
    let mut connect = false;
    egui::Grid::new("service").num_columns(2).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
        let input_label = ui.label(tl!("Address"));
        let r = ui
            .add(egui::TextEdit::singleline(&mut app.dialogs.text).hint_text("https://…/wms").desired_width(420.0))
            .labelled_by(input_label.id);
        connect |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        ui.end_row();
        kinds(app, ui);
        ui.end_row();
    });
    let t = Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        connect |= crate::widgets::dialog_button(ui, tl!("Connect"), Some(t.accent), vec2(96.0, 28.0)).clicked();
    });
    // A secured ArcGIS service: sign in to its portal first.
    if crate::arcgisui::wanted(app) {
        crate::arcgisui::section(app, ui);
    }
    if connect {
        let url = app.dialogs.text.trim().to_string();
        app.dialogs.error = None;
        app.dialogs.info = None;
        if url.is_empty() {
            app.dialogs.error = Some(tl!("Give the service's web address").to_string());
        } else if let Err(e) = app.run("ui.addService", json!({"url": url, "type": app.dialogs.mode})) {
            app.dialogs.error = Some(e);
        }
    }
    // The service: connecting, an error, or its layers.
    let mut add: Option<Value> = None;
    let mut remove: Option<(Removal, String)> = None;
    let mut import = None;
    // A directory's folder or service to open, or Back to the directory before.
    let mut open: Option<String> = None;
    let mut back = false;
    let keepable = app.ui.service.as_ref().and_then(|conn| {
        let (url, kind) = app.ui.service_browse.back.first().map(|(u, k)| (u.as_str(), *k)).unwrap_or((&conn.url, conn.kind));
        crate::connectionui::picked(&list, app.dialogs.connection.as_deref(), url).is_none().then(|| (url.to_string(), kind))
    });
    let mut keep_root = false;
    refresh_added(app);
    if let Some(conn) = &app.ui.service {
        ui.separator();
        if let Some(e) = &conn.error {
            ui.colored_label(t.red, e);
        }
        // A refused or failed request: plainly, with what the server said folded below (#509).
        if let Some(f) = &conn.failure {
            crate::failureui::show(ui, f, &t);
        }
        // A folder or service that didn't load (no list drawn, so no Back there): Back here.
        if conn.listing.is_none()
            && (conn.error.is_some() || conn.failure.is_some())
            && !app.ui.service_browse.back.is_empty()
            && ui.button(tl!("Back")).clicked()
        {
            back = true;
        }
        if let Some(v) = &conn.listing {
            let kind = v.get("kind").and_then(Value::as_str).unwrap_or("");
            let title = v.get("title").and_then(Value::as_str).unwrap_or("");
            // Borrowed, not cloned: the dialog draws every frame.
            let none = Vec::new();
            let layers = v.get("layers").and_then(Value::as_array).unwrap_or(&none);
            let grouped = kind.starts_with("OGC API");
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(crate::i18n::fmt(
                        "{kind} {title}: {count} layers",
                        &[("kind", kind), ("title", title), ("count", &layers.len().to_string())],
                    ))
                    .strong(),
                );
                if keepable.is_some() && conn.pending.is_none() && conn.error.is_none() && conn.failure.is_none() {
                    keep_root = crate::connectionui::keep_button(ui);
                }
            });
            for n in v.get("notes").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
                ui.label(egui::RichText::new(n).small().weak());
            }
            // A long list is filtered by words; a directory's services lead back to it.
            let browse = &mut app.ui.service_browse;
            if !browse.back.is_empty() || layers.len() > 12 {
                ui.horizontal(|ui| {
                    if !browse.back.is_empty() && ui.button(tl!("Back")).clicked() {
                        back = true;
                    }
                    if layers.len() > 12 {
                        let input_label = ui.label(tl!("Filter"));
                        ui.add(egui::TextEdit::singleline(&mut browse.filter).hint_text(tl!("words in the name")).desired_width(260.0))
                            .labelled_by(input_label.id);
                    }
                });
            }
            // Layers that change with time (NASA GIBS's, swisstopo's maps by year) are added at a
            // date; empty, at the service's default.
            if layers.iter().any(|l| l.get("time").is_some()) {
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        let mut follow = !browse.fixed;
                        let input_label = ui.label(tl!("Date"));
                        ui.add_enabled(
                            browse.fixed,
                            egui::TextEdit::singleline(&mut browse.time).hint_text(tl!("its default")).desired_width(120.0),
                        )
                        .labelled_by(input_label.id);
                        if ui
                            .checkbox(&mut follow, tl!("Follow the time slider"))
                            .on_hover_text(tl!("Step through the layer's times with the time slider (View › Time), which starts with them"))
                            .changed()
                        {
                            browse.fixed = !follow;
                        }
                    });
                    ui.label(
                        egui::RichText::new(tl!("for the layers that change with time, as they list their times (2024-06-01, 1995)"))
                            .small()
                            .weak(),
                    );
                });
                ui.separator();
            }
            // Feature layers come whole, or only the features crossing the map's view.
            if !grouped && layers.iter().any(|l| l["offer"]["type"] == "features") {
                ui.checkbox(&mut browse.area, tl!("Only the map's area"))
                    .on_hover_text(tl!("Feature layers: ask the service only for the features that cross the map's view"));
            }
            if grouped && layers.iter().any(|l| l["offer"]["type"] == "features") {
                ui.horizontal(|ui| {
                    let input_label = ui.label(tl!("Name"));
                    ui.add(egui::TextEdit::singleline(&mut app.ui.service_group_name)).labelled_by(input_label.id);
                    let ready = conn.pending.is_none() && conn.error.is_none() && conn.failure.is_none() && app.ui.service_import.is_none();
                    if ui.add_enabled(ready, egui::Button::new(tl!("Import All"))).clicked() {
                        let mut p = json!({"url":conn.url,"name":app.ui.service_group_name,"text":conn.text,"documents":conn.documents});
                        if conn.kind != "auto" {
                            p["type"] = json!(conn.kind);
                        }
                        import = Some(p);
                    }
                });
            }
            let added = conn.added.as_ref();
            let rows: &[(usize, Option<Removal>)] = added.map_or(&[], |a| a.rows.as_slice());
            if added.is_some_and(|a| a.matched > SHOWN_LAYERS) {
                ui.label(
                    egui::RichText::new(crate::i18n::fmt("The first {n} are listed.", &[("n", &SHOWN_LAYERS.to_string())])).small().weak(),
                );
            }
            let ui_width = ui.available_width();
            egui::Resize::default()
                .id_salt("add_service_layers")
                .default_height(280.0)
                .min_height(100.0)
                .max_height(350.0)
                .resizable(true)
                .min_width(ui_width.min(560.0))
                .max_width(750.0)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical().id_salt("add_service_layers_scroll").auto_shrink([false, false]).show(ui, |ui| {
                        let style = ui.style().clone();
                        for (i, removal) in rows {
                            let Some(l) = layers.get(*i) else { continue };
                            ui.horizontal(|ui| {
                                ui.add_space(2.0);
                                let kind = l.get("offer").and_then(|o| o.get("type")).and_then(Value::as_str);

                                let name = l.get("name").and_then(Value::as_str).unwrap_or("");
                                let title = l.get("title").and_then(Value::as_str).unwrap_or(name);
                                let removal = removal.as_ref();
                                if kind == Some("service") {
                                    if ui.small_button(tl!("Open")).clicked() {
                                        open = l.get("offer").and_then(|o| o.get("url")).and_then(Value::as_str).map(str::to_string);
                                    }
                                } else if grouped && kind == Some("features") {
                                    // Import All adds these as one group: the row says whether its
                                    // collection is in it; the child is removed in the Layers pane.
                                    if removal.is_some() {
                                        ui.label(egui::RichText::new(tl!("On the map")).small().weak());
                                    }
                                } else {
                                    let label = if removal.is_some() { tl!("Remove") } else { tl!("Add") };
                                    if ui.small_button(label).clicked() {
                                        match removal {
                                            Some((command, params)) => remove = Some(((command, params.clone()), title.to_string())),
                                            None => add = Some(l.clone()),
                                        }
                                    }
                                }
                                let what = match kind {
                                    Some("features") => tl!("features"),
                                    Some("style") => tl!("style"),
                                    Some("coverage") => tl!("coverage"),
                                    Some("service") if name.ends_with('/') => tl!("folder"),
                                    Some("service") => tl!("service"),
                                    _ if l.get("time").is_some() => tl!("map by date"),
                                    _ => tl!("map"),
                                };

                                let mut job = egui::text::LayoutJob::default();
                                egui::RichText::new(format!("{title}  ")).append_to(
                                    &mut job,
                                    &style,
                                    egui::FontSelection::Default,
                                    egui::Align::Center,
                                );
                                egui::RichText::new(what).small().weak().append_to(
                                    &mut job,
                                    &style,
                                    egui::FontSelection::Default,
                                    egui::Align::Center,
                                );
                                ui.add(egui::Label::new(job).wrap()).on_hover_ui(|ui| {
                                    ui.label(name);
                                    if let Some(t) = l.get("time") {
                                        let values: Vec<&str> =
                                            t["values"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
                                        let default = t["default"].as_str().unwrap_or("");
                                        // The first and last few of a long list (`service.layers` lists
                                        // those, with how many there are).
                                        let count = t["count"].as_u64().and_then(|c| usize::try_from(c).ok()).unwrap_or(values.len());
                                        let half = values.len() / 2;
                                        let times = if count > values.len() {
                                            format!(
                                                "{}, …, {}",
                                                values.get(..half).unwrap_or(&[]).join(", "),
                                                values.get(half..).unwrap_or(&[]).join(", ")
                                            )
                                        } else {
                                            values.join(", ")
                                        };
                                        ui.label(crate::i18n::fmt(
                                            "Its default: {default}. Its times: {times}",
                                            &[("default", default), ("times", &times)],
                                        ));
                                    }
                                });
                            });
                        }
                    });
                });
        }
        if let Some(pending) = &conn.pending {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(crate::i18n::fmt("Connecting to {url}…", &[("url", pending)]));
            });
        }
    }
    if keep_root && let Some((url, kind)) = keepable {
        // "auto" is the dialog's detection mode; saved connections call that kind "service".
        match crate::connectionui::keep(app, json!({"kind": if kind == "auto" { "service" } else { kind }, "address": url})) {
            Ok(id) => app.dialogs.connection = Some(id),
            Err(e) => app.dialogs.error = Some(e),
        }
    }
    if let Some(p) = import
        && let Err(e) = app.run("ui.importService", p)
    {
        app.dialogs.error = Some(e);
    }
    if let Some(((command, params), title)) = remove {
        match app.run(command, params) {
            Ok(_) => app.dialogs.info = Some(crate::i18n::fmt("Removed {name}", &[("name", &title)])),
            Err(e) => app.dialogs.error = Some(e),
        }
        app.map.invalidate();
    }
    // Into a directory's folder or service, keeping the way back; or back out of it.
    let here = app.ui.service.as_ref().map(|c| (c.url.clone(), c.kind));
    if let (Some(url), Some(here)) = (open, here) {
        let before = app.ui.service_browse.back.clone();
        let mut way = before.clone();
        way.push(here);
        go_to(app, &url, "arcgis", way, before);
    } else if back && let Some((url, kind)) = app.ui.service_browse.back.pop() {
        let way = app.ui.service_browse.back.clone();
        let mut before = way.clone();
        before.push((url.clone(), kind));
        go_to(app, &url, kind, way, before);
    }
    if app.ui.service_import.is_some() {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(tl!("Downloading…"));
        });
    }
    if let (Some(l), Some(conn)) = (add, &app.ui.service) {
        let src = crate::serviceui::ServiceSource {
            url: conn.url.clone(),
            kind: conn.kind,
            text: conn.text.clone().unwrap_or_default(),
            documents: conn.documents.clone(),
        };
        let name = l.get("name").and_then(Value::as_str).unwrap_or("");
        let title = l.get("title").and_then(Value::as_str).unwrap_or(name).to_string();
        // Features are downloaded in the background, as From URL… does; map layers are added now.
        let time = crate::webdataui::fixed_or_slider(app.ui.service_browse.fixed, &app.ui.service_browse.time);
        // The view's area, for a feature layer only: a map layer never asks for one, so an
        // unknown view can't stop it.
        let features = l["offer"]["type"] == "features";
        let area =
            (features && app.ui.service_browse.area).then(|| rustgis_engine::cmd::catalogs::area(&app.session, &Value::Null)).transpose();
        match area.and_then(|area| crate::serviceui::add_layer(app, &src, &l, &time, area, true)) {
            Ok(m) => app.dialogs.info = Some(format!("{m} {title}")),
            Err(e) => app.dialogs.error = Some(e),
        }
        app.map.invalidate();
    }
    if let Some(i) = &app.dialogs.info {
        ui.label(egui::RichText::new(i).small());
    }
    false
}

/// Add Data From URL: a file on the web, downloaded and added (a PMTiles archive is read in place).
fn add_url(app: &mut RustGisApp, ui: &mut egui::Ui) -> bool {
    let hint = if cfg!(target_arch = "wasm32") {
        tl!(
            "GeoJSON, CSV, GeoParquet, FlatGeobuf, GeoPackage, zipped Shapefiles, KML, GPX or GeoTIFF, up to 256 MB; the server must allow requests from web pages (CORS)."
        )
    } else {
        tl!("GeoJSON, CSV, GeoParquet, FlatGeobuf, GeoPackage, zipped Shapefiles, KML, GPX or GeoTIFF, up to 512 MB.")
    };
    ui.label(egui::RichText::new(hint).weak());
    egui::Grid::new("url").num_columns(2).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
        let input_label = ui.label(tl!("Address"));
        ui.add(egui::TextEdit::singleline(&mut app.dialogs.text).hint_text("https://…/data.geojson").desired_width(400.0))
            .labelled_by(input_label.id);
        ui.end_row();
        let input_label = ui.label(tl!("Name"));
        ui.add(egui::TextEdit::singleline(&mut app.dialogs.a).hint_text(tl!("from the address")).desired_width(400.0))
            .labelled_by(input_label.id);
        ui.end_row();
    });
    buttons(app, ui, "Add", |app| {
        let url = app.dialogs.text.trim().to_string();
        if url.is_empty() {
            return Err(tl!("Give the file's web address").to_string());
        }
        app.run("ui.addUrl", json!({"url": url, "name": app.dialogs.a.trim()})).map(|_| ())
    })
}

/// Add From Database: a PostgreSQL/PostGIS, MySQL/MariaDB or SQL Server connection (or a DuckDB
/// file), its password (kept for the session only) and one table or all of them.
fn add_database(app: &mut RustGisApp, ui: &mut egui::Ui) -> bool {
    ui.label(egui::RichText::new(tl!("A PostgreSQL/PostGIS connection (host=… port=… dbname=… user=…, or a postgresql:// address), a MySQL or MariaDB address (mysql://user@host:3306/database), a SQL Server address (mssql://user@host:1433/database) or a DuckDB file. PostGIS, MySQL, MariaDB and SQL Server tables are read live, large ones a view at a time; the password is kept until RustGIS closes, never in the project.")).weak());
    // A bare connection string is a PostgreSQL one.
    let database = |a: &str| {
        let a = a.trim();
        if a.contains('=') && !a.contains("://") && !a.to_ascii_lowercase().starts_with("postgres:") {
            format!("postgres:{a}")
        } else {
            a.to_string()
        }
    };
    let connections = crate::sourcespane::connections_snapshot(app);
    let typed = database(&app.dialogs.a);
    let current = crate::connectionui::picked(&connections, app.dialogs.connection.as_deref(), &typed);
    #[cfg(not(target_arch = "wasm32"))]
    let postgres = rustgis_engine::connections::database_kind(&typed) == Some("postgres");
    let mut pick = None;
    let mut list = false;
    egui::Grid::new("database").num_columns(2).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
        let kinds = rustgis_engine::connections::DATABASE_KINDS;
        if connections.iter().any(|c| kinds.contains(&c.kind.as_str())) {
            ui.label(tl!("Saved connection"));
            pick = crate::connectionui::picker_combo(ui, "database_saved", &connections, kinds, current);
            ui.end_row();
        }
        let input_label = ui.label(tl!("Connection"));
        ui.add(egui::TextEdit::singleline(&mut app.dialogs.a).hint_text("host=localhost dbname=gis user=postgres").desired_width(280.0))
            .labelled_by(input_label.id);
        ui.end_row();
        let input_label = ui.label(tl!("Password"));
        ui.add(
            egui::TextEdit::singleline(&mut app.dialogs.b)
                .password(true)
                .hint_text(tl!("none, or from PGPASSWORD, MYSQL_PWD or SQLCMDPASSWORD"))
                .desired_width(280.0),
        )
        .labelled_by(input_label.id);
        ui.end_row();
        ui.label(tl!("Table"));
        ui.horizontal(|ui| {
            let shown = if app.dialogs.c.is_empty() { tl!("All tables with geometry").to_string() } else { app.dialogs.c.clone() };
            egui::ComboBox::from_id_salt("db_table")
                .selected_text(shown)
                .width(190.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut app.dialogs.c, String::new(), tl!("All tables with geometry"));
                    for t in app.dialogs.list.clone() {
                        ui.selectable_value(&mut app.dialogs.c, t.clone(), t);
                    }
                })
                .response
                .on_hover_text(tl!("Table"));
            list = ui.button(tl!("List Tables")).clicked();
        });
        ui.end_row();
        let input_label = ui.label(tl!("Coordinate system"));
        ui.add(egui::TextEdit::singleline(&mut app.dialogs.text).hint_text(tl!("of a geometry that doesn't say")).desired_width(280.0))
            .labelled_by(input_label.id);
        ui.end_row();
        // A PostgreSQL table can instead be loaded whole, to be edited and saved back to it.
        #[cfg(not(target_arch = "wasm32"))]
        if postgres {
            ui.label("");
            ui.checkbox(&mut app.dialogs.edit, tl!("Load for editing")).on_hover_text(tl!(
                "Load the table whole (up to 100,000 rows) so its edits can be saved back to it with Edit › Save to PostGIS. It needs a single-column integer primary key."
            ));
            ui.end_row();
            if app.dialogs.edit {
                let input_label = ui.label(tl!("Condition"));
                ui.add(
                    egui::TextEdit::singleline(&mut app.dialogs.d)
                        .hint_text(tl!("optional, such as population > 10000"))
                        .desired_width(280.0),
                )
                .labelled_by(input_label.id);
                ui.end_row();
                let input_label = ui.label(tl!("Most rows"));
                ui.add(egui::TextEdit::singleline(&mut app.dialogs.e).hint_text("100000").desired_width(280.0)).labelled_by(input_label.id);
                ui.end_row();
            }
        }
    });
    // The connection that layers loaded for editing save through, until RustGIS closes.
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(dsn) = app.session.postgis.clone() {
        ui.horizontal(|ui| {
            let short = rustgis_engine::data::source::duckdb::Database::Postgres(dsn).short();
            ui.label(egui::RichText::new(crate::i18n::fmt("Connected for editing: {db}", &[("db", &short)])).weak());
            if ui
                .button(tl!("Disconnect"))
                .on_hover_text(tl!("Close the connection that PostGIS layers loaded for editing save through."))
                .clicked()
                && let Err(e) = app.run("pg.disconnect", Value::Null)
            {
                app.dialogs.error = Some(e);
            }
        });
    }
    // The tables listed and the one chosen belong to the address they were listed for.
    if app.dialogs.listed.as_deref().is_some_and(|l| l != database(&app.dialogs.a)) {
        app.dialogs.listed = None;
        app.dialogs.list.clear();
        app.dialogs.c.clear();
        app.dialogs.info = None;
    }
    if let Some(c) = pick {
        if let Err(e) = crate::connectionui::open_connection(app, &c) {
            app.dialogs.error = Some(e);
        }
        return false;
    }
    if list {
        let p = json!({"database": database(&app.dialogs.a), "password": app.dialogs.b});
        match app.run("info.databaseTables", p) {
            Ok(v) => {
                app.dialogs.list = v["layers"].as_array().into_iter().flatten().filter_map(|t| t.as_str().map(str::to_string)).collect();
                app.dialogs.listed = Some(database(&app.dialogs.a));
                app.dialogs.error = None;
                app.dialogs.info = Some(crate::i18n::fmt("{n} tables with geometry", &[("n", &app.dialogs.list.len().to_string())]));
            }
            Err(e) => {
                // What a failed listing leaves can't be tied to an address any more, so it goes too.
                app.dialogs.listed = None;
                app.dialogs.list.clear();
                app.dialogs.c.clear();
                app.dialogs.info = None;
                app.dialogs.error = Some(e);
            }
        }
    }
    ui.horizontal(|ui| {
        let address = database(&app.dialogs.a);
        if app.dialogs.listed.as_deref() == Some(address.as_str())
            && let Some(kind) = rustgis_engine::connections::database_kind(&address)
            && crate::connectionui::picked(&connections, app.dialogs.connection.as_deref(), &address).is_none()
            && crate::connectionui::keep_button(ui)
        {
            match crate::connectionui::keep(app, json!({"kind": kind, "address": address})) {
                Ok(id) => app.dialogs.connection = Some(id),
                Err(e) => app.dialogs.error = Some(e),
            }
        }
    });
    buttons(app, ui, "Add", |app| {
        let address = database(&app.dialogs.a);
        if app.dialogs.edit && rustgis_engine::connections::database_kind(&address) == Some("postgres") {
            return add_for_editing(app, address);
        }
        let mut p = json!({"database": address, "password": app.dialogs.b});
        for (key, v) in [("layer", &app.dialogs.c), ("crs", &app.dialogs.text)] {
            if !v.trim().is_empty() {
                p[key] = json!(v.trim());
            }
        }
        let r = app.run("layer.addDatabase", p).map(|_| ());
        // The password is the session's to keep, not the dialog's.
        if r.is_ok() {
            app.dialogs.b.clear();
        }
        r
    })
}

/// Add From Database with Load for editing: connects to the PostgreSQL database for editing
/// (`pg.connect`) and loads the chosen table whole (`pg.addLayer`).
fn add_for_editing(app: &mut RustGisApp, address: String) -> Result<(), String> {
    let d = &app.dialogs;
    let table = d.c.trim();
    if table.is_empty() {
        return Err(tl!("Choose a table to load for editing").to_string());
    }
    let mut p = json!({"table": table});
    for (key, v) in [("where", &d.d), ("crs", &d.text)] {
        if !v.trim().is_empty() {
            p[key] = json!(v.trim());
        }
    }
    if !d.e.trim().is_empty() {
        let n: u64 = d.e.trim().replace([',', '.'], "").parse().map_err(|_| tl!("Most rows: a whole number").to_string())?;
        p["limit"] = json!(n);
    }
    let connect = json!({"connection": address, "password": d.b});
    app.run("pg.connect", connect)?;
    // The password is the session's to keep, not the dialog's.
    app.dialogs.b.clear();
    app.run("pg.addLayer", p).map(|_| ())
}

/// Load one GeoPackage feature table whole, to be edited and saved back to its file.
fn gpkg_add_layer(app: &mut RustGisApp, ui: &mut egui::Ui) -> bool {
    ui.label(egui::RichText::new(tl!("Loads one feature table of a GeoPackage on this computer (up to 100,000 rows) so its edits can be saved back to that table with Edit › Save to GeoPackage. It needs an INTEGER PRIMARY KEY.")).weak());
    let mut list = false;
    egui::Grid::new("gpkg_add_layer").num_columns(2).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
        let input_label = ui.label(tl!("File"));
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut app.dialogs.a).desired_width(260.0)).labelled_by(input_label.id);
            #[cfg(not(target_arch = "wasm32"))]
            if ui.button(tl!("Browse…")).clicked()
                && let Some(path) = pick_open(&["gpkg"], tl!("GeoPackage"))
            {
                app.dialogs.a = path;
            }
        });
        ui.end_row();
        let input_label = ui.label(tl!("Table"));
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("gpkg_table")
                .selected_text(app.dialogs.c.clone())
                .width(190.0)
                .show_ui(ui, |ui| {
                    for table in app.dialogs.list.clone() {
                        ui.selectable_value(&mut app.dialogs.c, table.clone(), table);
                    }
                })
                .response
                .labelled_by(input_label.id);
            list = ui.button(tl!("List Tables")).clicked();
        });
        ui.end_row();
        let input_label = ui.label(tl!("Most rows"));
        ui.add(egui::TextEdit::singleline(&mut app.dialogs.e).hint_text("100000").desired_width(280.0)).labelled_by(input_label.id);
        ui.end_row();
    });
    if app.dialogs.listed.as_deref().is_some_and(|path| path != app.dialogs.a) {
        app.dialogs.listed = None;
        app.dialogs.list.clear();
        app.dialogs.c.clear();
        app.dialogs.info = None;
    }
    if list {
        match app.run("info.fileLayers", json!({"path": app.dialogs.a})) {
            Ok(v) => {
                app.dialogs.list = v["layers"].as_array().into_iter().flatten().filter_map(|t| t.as_str().map(str::to_string)).collect();
                app.dialogs.listed = Some(app.dialogs.a.clone());
                app.dialogs.error = None;
                app.dialogs.info = Some(crate::i18n::fmt("{n} tables with geometry", &[("n", &app.dialogs.list.len().to_string())]));
            }
            Err(e) => {
                app.dialogs.listed = None;
                app.dialogs.list.clear();
                app.dialogs.c.clear();
                app.dialogs.info = None;
                app.dialogs.error = Some(e);
            }
        }
    }
    buttons(app, ui, "Load", |app| {
        let mut p = json!({"path": app.dialogs.a.trim()});
        if !app.dialogs.c.trim().is_empty() {
            p["table"] = json!(app.dialogs.c.trim());
        }
        if !app.dialogs.e.trim().is_empty() {
            let input = app.dialogs.e.trim();
            let mut groups = input.split(',');
            let first = groups.next().unwrap_or_default();
            let grouped = first.len() != input.len();
            if grouped
                && (first.is_empty()
                    || first.len() > 3
                    || !first.bytes().all(|b| b.is_ascii_digit())
                    || !groups.all(|group| group.len() == 3 && group.bytes().all(|b| b.is_ascii_digit())))
            {
                return Err(tl!("Most rows: a whole number").to_string());
            }
            let n: u64 = if grouped { input.replace(',', "").parse() } else { input.parse() }
                .map_err(|_| tl!("Most rows: a whole number").to_string())?;
            p["limit"] = json!(n);
        }
        app.run("gpkg.addLayer", p).map(|_| ())
    })
}

/// A source-backed layer to save or discard the edits of, and what changed in it: its table
/// and how many rows are new, changed and deleted (cached until the selection or session changes).
fn source_layer_edits(app: &mut RustGisApp, ui: &mut egui::Ui, prefix: &'static str) {
    if prefix == "gpkg"
        && let Err(error) = app.dialogs.gpkg_owner(&app.session)
    {
        app.dialogs.list.clear();
        app.dialogs.counted.clear();
        app.dialogs.error = Some(error);
        return;
    }
    let layers: Vec<(u32, u64, String)> = app
        .session
        .map()
        .layers
        .iter()
        .filter(|l| if prefix == "pg" { l.postgis.is_some() } else { l.gpkg.is_some() })
        .map(|l| (l.id, l.instance_id(), l.name.clone()))
        .collect();
    egui::Grid::new(("source_edits", prefix)).num_columns(2).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
        ui.label(tl!("Layer"));
        let selected = if prefix == "gpkg" {
            layers
                .iter()
                .find(|(id, instance, _)| Some(*id) == app.dialogs.gpkg_layer && Some(*instance) == app.dialogs.gpkg_instance)
                .map(|(id, _, name)| format!("{name} (#{id})"))
                .unwrap_or_else(|| tl!("Choose a layer").to_string())
        } else {
            app.dialogs.layer.clone()
        };
        let previous = app.dialogs.gpkg_layer.zip(app.dialogs.gpkg_instance);
        let mut selected_instance = previous;
        egui::ComboBox::from_id_salt(("source_edit_layer", prefix)).selected_text(selected).width(260.0).show_ui(ui, |ui| {
            for (id, instance, name) in &layers {
                if prefix == "gpkg" {
                    ui.selectable_value(&mut selected_instance, Some((*id, *instance)), format!("{name} (#{id})"));
                } else {
                    ui.selectable_value(&mut app.dialogs.layer, name.clone(), name);
                }
            }
        });
        if prefix == "gpkg" && selected_instance != previous {
            app.dialogs.gpkg_layer = selected_instance.map(|(id, _)| id);
            app.dialogs.gpkg_instance = selected_instance.map(|(_, instance)| instance);
            app.dialogs.error = None;
        }
        ui.end_row();
        let counted =
            if prefix == "gpkg" { app.dialogs.gpkg_layer.map(|id| id.to_string()).unwrap_or_default() } else { app.dialogs.layer.clone() };
        let invalid = if prefix == "gpkg" { app.dialogs.gpkg_target(&app.session).err() } else { None };
        if invalid.is_some()
            || app.dialogs.counted != counted
            || (prefix == "gpkg" && app.dialogs.counted_revision != Some(app.session.revision))
            || (prefix == "gpkg" && app.dialogs.counted_instance != app.dialogs.gpkg_instance)
        {
            app.dialogs.counted = counted;
            app.dialogs.counted_instance = app.dialogs.gpkg_instance;
            let result = if let Some(error) = invalid {
                Err(error)
            } else {
                let target = if prefix == "gpkg" { json!(app.dialogs.gpkg_layer) } else { json!(app.dialogs.layer) };
                app.run(&format!("{prefix}.pendingEdits"), json!({"layer": target}))
            };
            app.dialogs.list = match result {
                Ok(v) => {
                    if prefix == "gpkg" && app.dialogs.error.as_deref().is_some_and(is_gpkg_target_error) {
                        app.dialogs.error = None;
                    }
                    vec![
                        v["table"].as_str().unwrap_or_default().to_string(),
                        v["inserts"].to_string(),
                        v["updates"].to_string(),
                        v["deletes"].to_string(),
                        v["file"].as_str().unwrap_or_default().to_string(),
                    ]
                }
                Err(e) => {
                    app.dialogs.error = Some(e);
                    Vec::new()
                }
            };
            app.dialogs.counted_revision = Some(app.session.revision);
        }
        let shown = app.dialogs.list.clone();
        if let Some(file) = shown.get(4).filter(|file| !file.is_empty()) {
            ui.label(tl!("File"));
            ui.label(egui::RichText::new(file).strong());
            ui.end_row();
        }
        for (label, value) in [
            (tl!("Table"), shown.first()),
            (tl!("New rows"), shown.get(1)),
            (tl!("Changed rows"), shown.get(2)),
            (tl!("Deleted rows"), shown.get(3)),
        ] {
            ui.label(label);
            ui.label(egui::RichText::new(value.cloned().unwrap_or_default()).strong());
            ui.end_row();
        }
    });
}

/// Save PostGIS Edits: a PostGIS layer's edits written to its table in one transaction, or none
/// of them when a row changed there since.
fn pg_save(app: &mut RustGisApp, ui: &mut egui::Ui) -> bool {
    ui.label(egui::RichText::new(tl!("Saves the layer's edits to its table in one transaction. When a row you changed or deleted was changed or deleted in the database since it was loaded, or a new row's key is taken, nothing is saved and the rows are listed.")).weak());
    source_layer_edits(app, ui, "pg");
    buttons(app, ui, "Save", |app| {
        let r = app.run("pg.saveEdits", json!({"layer": app.dialogs.layer})).map(|_| ());
        // Counted again, whatever happened.
        app.dialogs.counted.clear();
        r
    })
}

/// Discard PostGIS Edits: a PostGIS layer's rows loaded again as they are in the database.
fn pg_discard(app: &mut RustGisApp, ui: &mut egui::Ui) -> bool {
    ui.label(egui::RichText::new(tl!("Loads the layer's rows again as they are in the database now. Its edits that weren't saved are dropped (Undo brings them back).")).weak());
    source_layer_edits(app, ui, "pg");
    buttons(app, ui, "Discard", |app| app.run("pg.discardEdits", json!({"layer": app.dialogs.layer})).map(|_| ()))
}

/// Save a GeoPackage layer's edits to its table in one transaction.
fn gpkg_save(app: &mut RustGisApp, ui: &mut egui::Ui) -> bool {
    ui.label(egui::RichText::new(tl!("Saves the layer's edits to its table in the GeoPackage file in one transaction. When a row you changed or deleted was changed or deleted in the file since it was loaded, nothing is saved and the rows are listed. Saving the project keeps edits in the project only; exporting writes a new file.")).weak());
    source_layer_edits(app, ui, "gpkg");
    buttons(app, ui, "Save", |app| {
        let id = app.dialogs.gpkg_target(&app.session)?;
        let r = app.run("gpkg.saveEdits", json!({"layer": id})).map(|_| ());
        app.dialogs.counted.clear();
        r
    })
}

/// Reload a GeoPackage layer's rows as they are in the file.
fn gpkg_discard(app: &mut RustGisApp, ui: &mut egui::Ui) -> bool {
    ui.label(
        egui::RichText::new(tl!(
            "Loads the layer's rows again as they are in the file now. Its edits that weren't saved are dropped (Undo brings them back)."
        ))
        .weak(),
    );
    source_layer_edits(app, ui, "gpkg");
    buttons(app, ui, "Discard", |app| {
        let id = app.dialogs.gpkg_target(&app.session)?;
        app.run("gpkg.discardEdits", json!({"layer": id})).map(|_| ())
    })
}

/// Add Tile Layer: an XYZ/TMS/WMS template, vector tiles or a MapLibre style by address.
fn add_tiles(app: &mut RustGisApp, ui: &mut egui::Ui) -> bool {
    use rustgis_engine::map::TileKind;
    ui.label(egui::RichText::new(tl!("A tile service's address: raster tiles with {z}/{x}/{y} (or {-y}, {q}, {bbox-epsg-3857} for WMS), vector tiles (.pbf) or TileJSON, a MapLibre style, a PMTiles archive (a web address, or a file on this computer), or an MBTiles file on this computer.")).weak());
    egui::Grid::new("tiles").num_columns(2).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
        ui.label(tl!("Address"));
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut app.dialogs.text)
                    .hint_text("https://tile.openstreetmap.org/{z}/{x}/{y}.png")
                    .desired_width(if cfg!(target_arch = "wasm32") { 400.0 } else { 320.0 }),
            );
            // A PMTiles or MBTiles file on this computer (the web app reads PMTiles archives from
            // web addresses, and MBTiles files through Add Data).
            if !cfg!(target_arch = "wasm32")
                && ui.button(tl!("Tile Archive File…")).clicked()
                && let Some(path) = pick_open(&["pmtiles", "mbtiles"], tl!("Tile archive"))
            {
                app.dialogs.mode = if path.to_ascii_lowercase().ends_with(".mbtiles") { "mbtiles" } else { "pmtiles" };
                app.dialogs.text = path;
            }
        });
        ui.end_row();
        let input_label = ui.label(tl!("Type"));
        let guessed = TileKind::guess(app.dialogs.text.trim());
        let shown = match app.dialogs.mode {
            "auto" | "new" => crate::i18n::fmt("Automatic ({kind})", &[("kind", tl_raw!(guessed.label()))]),
            code => TileKind::from_code(code).map(|k| tl!(k.label()).to_string()).unwrap_or_default(),
        };
        egui::ComboBox::from_id_salt("tiles_kind")
            .selected_text(shown)
            .width(260.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut app.dialogs.mode, "auto", tl!("Automatic"));
                for k in TileKind::ALL {
                    ui.selectable_value(&mut app.dialogs.mode, k.code(), tl!(k.label()));
                }
            })
            .response
            .labelled_by(input_label.id);
        ui.end_row();
        let input_label = ui.label(tl!("Name"));
        ui.add(egui::TextEdit::singleline(&mut app.dialogs.a).hint_text(tl!("from the address")).desired_width(400.0))
            .labelled_by(input_label.id);
        ui.end_row();
        let input_label = ui.label(tl!("Attribution"));
        ui.add(egui::TextEdit::singleline(&mut app.dialogs.b).hint_text("© OpenStreetMap contributors").desired_width(400.0))
            .labelled_by(input_label.id);
        ui.end_row();
        let input_label = ui.label(tl!("Maximum zoom"));
        ui.add(egui::TextEdit::singleline(&mut app.dialogs.c).hint_text(tl!("from the service")).desired_width(60.0))
            .labelled_by(input_label.id);
        ui.end_row();
    });
    ui.checkbox(
        &mut app.dialogs.flag,
        tl!("Overlay (on top of the map's layers, such as labels, roads or a WMS layer; otherwise a basemap under them)"),
    );
    buttons(app, ui, "Add", |app| {
        let mut p = json!({"url": app.dialogs.text.trim(), "overlay": app.dialogs.flag});
        if !matches!(app.dialogs.mode, "auto" | "new") {
            p["kind"] = json!(app.dialogs.mode);
        }
        for (key, v) in [("name", &app.dialogs.a), ("attribution", &app.dialogs.b)] {
            if !v.trim().is_empty() {
                p[key] = json!(v.trim());
            }
        }
        if let Ok(z) = app.dialogs.c.trim().parse::<f64>() {
            p["maxZoom"] = json!(z);
        }
        app.run("map.addTiles", p).map(|_| ())
    })
}

/// A dialog's multi-line text box. [`buttons`] takes Enter to confirm the dialog, so only
/// Shift+Enter breaks a line here: a plain Enter would also add one where the cursor is, in the
/// same frame (egui's default, which also takes Shift+Enter).
pub(crate) fn text_box(text: &mut String) -> egui::TextEdit<'_> {
    let new_line = egui::KeyboardShortcut::new(egui::Modifiers::SHIFT, egui::Key::Enter);
    egui::TextEdit::multiline(text).return_key(new_line)
}

/// OK/Cancel row; returns true when the dialog should close.
pub(crate) fn buttons(app: &mut RustGisApp, ui: &mut egui::Ui, ok: &str, f: impl FnOnce(&mut RustGisApp) -> Result<(), String>) -> bool {
    let mut close = false;
    ui.add_space(4.0);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
        let t = Tokens::get(ui.ctx());
        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift);
        if crate::widgets::dialog_button(ui, tl!(ok), Some(t.accent), vec2(84.0, 28.0)).clicked() || enter {
            match f(app) {
                Ok(()) => close = true,
                Err(e) => app.dialogs.error = Some(e),
            }
        }
        if crate::widgets::dialog_button(ui, tl!("Cancel"), None, vec2(84.0, 28.0)).clicked() {
            close = true;
        }
    });
    close
}

fn field_chips(app: &mut RustGisApp, ui: &mut egui::Ui) {
    let fields: Vec<String> = app
        .session
        .map()
        .layer_by_name(&app.dialogs.layer)
        .map(|l| l.dataset.fields.iter().map(|f| f.name.clone()).collect())
        .unwrap_or_default();
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new(tl!("Fields:")).small().weak());
        for f in fields.iter().take(24) {
            if ui.small_button(f).clicked() {
                if !app.dialogs.text.is_empty() && !app.dialogs.text.ends_with(' ') {
                    app.dialogs.text.push(' ');
                }
                app.dialogs.text.push_str(f);
                app.dialogs.text.push(' ');
            }
        }
    });
    ui.horizontal(|ui| {
        for op in ["=", "<>", ">", "<", "LIKE", "AND", "OR", "IS NULL"] {
            if ui.small_button(op).clicked() {
                app.dialogs.text.push_str(&format!("{op} "));
            }
        }
    });
}

fn select_by_attributes(app: &mut RustGisApp, ui: &mut egui::Ui, t: &Tokens) -> bool {
    let layers: Vec<String> = app.session.map().layers.iter().map(|l| l.name.clone()).collect();
    egui::Grid::new("sba").num_columns(2).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
        let input_label = ui.label(tl!("Input Rows"));
        egui::ComboBox::from_id_salt("sba_layer")
            .selected_text(app.dialogs.layer.clone())
            .width(260.0)
            .show_ui(ui, |ui| {
                for l in &layers {
                    ui.selectable_value(&mut app.dialogs.layer, l.clone(), l);
                }
            })
            .response
            .labelled_by(input_label.id);
        ui.end_row();
        let input_label = ui.label(tl!("Selection Type"));
        egui::ComboBox::from_id_salt("sba_mode")
            .selected_text(match app.dialogs.mode {
                "add" => tl!("Add To Current Selection"),
                "remove" => tl!("Remove From Current Selection"),
                "subset" => tl!("Select Subset From Current Selection"),
                _ => tl!("New Selection"),
            })
            .width(260.0)
            .show_ui(ui, |ui| {
                for (m, l) in [
                    ("new", tl!("New Selection")),
                    ("add", tl!("Add To Current Selection")),
                    ("remove", tl!("Remove From Current Selection")),
                    ("subset", tl!("Select Subset From Current Selection")),
                ] {
                    ui.selectable_value(&mut app.dialogs.mode, m, l);
                }
            })
            .response
            .labelled_by(input_label.id);
        ui.end_row();
    });
    ui.label(egui::RichText::new(tl!("Where")).font(semibold(12.5)));
    ui.add(
        text_box(&mut app.dialogs.text)
            .hint_text(tl!("e.g. pop_est > 100000000 AND continent = 'Asia'"))
            .desired_rows(3)
            .desired_width(f32::INFINITY)
            .font(egui::TextStyle::Monospace),
    );
    field_chips(app, ui);
    // Live validation.
    let check =
        app.session.map().layer_by_name(&app.dialogs.layer).map(|l| rustgis_engine::data::query::select(&app.dialogs.text, &l.dataset));
    match (&check, app.dialogs.text.trim().is_empty()) {
        (_, true) => {}
        (Some(Ok(rows)), _) => {
            ui.label(
                egui::RichText::new(crate::i18n::fmt("Valid expression · {n} matching rows", &[("n", &rows.len().to_string())]))
                    .small()
                    .color(t.green),
            );
        }
        (Some(Err(e)), _) => {
            ui.label(egui::RichText::new(crate::i18n::fmt("Invalid: {error}", &[("error", e)])).small().color(t.red));
        }
        _ => {}
    }
    buttons(app, ui, "Apply", |app| {
        let layer = app.dialogs.layer.clone();
        app.run("select.byAttributes", json!({"layer": layer, "where": app.dialogs.text, "mode": app.dialogs.mode})).map(|_| ())
    })
}

/// Asks for a file to open with the system's dialog, offering `exts` under `label` (and all
/// files); `None` when cancelled.
#[cfg(all(not(target_arch = "wasm32"), not(test)))]
pub fn pick_open(exts: &[&str], label: &str) -> Option<String> {
    let dialog = rfd::FileDialog::new();
    let dialog = if exts.is_empty() { dialog } else { dialog.add_filter(label, exts) };
    dialog.add_filter("All files", &["*"]).pick_file().map(|p| p.display().to_string())
}

#[cfg(not(target_arch = "wasm32"))]
pub fn pick_folder() -> Option<String> {
    rfd::FileDialog::new().pick_folder().map(|p| p.display().to_string())
}

/// Asks where to save, suggesting file `name`, with the system's dialog; `None` when cancelled.
#[cfg(all(not(target_arch = "wasm32"), not(test)))]
pub fn pick_save(name: &str, exts: &[&str], label: &str) -> Option<String> {
    rfd::FileDialog::new().set_file_name(name).add_filter(label, exts).save_file().map(|p| p.display().to_string())
}

// Unit tests can't answer the system's file dialogs: a picker answers what the test put in
// `PICKED` instead, once, and is cancelled when it holds nothing.
#[cfg(test)]
thread_local! {
    pub(crate) static PICKED: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// In a unit test: the path the test put in [`PICKED`], or cancelled.
#[cfg(all(not(target_arch = "wasm32"), test))]
pub fn pick_open(_: &[&str], _: &str) -> Option<String> {
    PICKED.with(|p| p.borrow_mut().take())
}

/// In a unit test: the path the test put in [`PICKED`], or cancelled.
#[cfg(all(not(target_arch = "wasm32"), test))]
pub fn pick_save(_: &str, _: &[&str], _: &str) -> Option<String> {
    PICKED.with(|p| p.borrow_mut().take())
}

#[cfg(target_arch = "wasm32")]
pub fn pick_open(_: &[&str], _: &str) -> Option<String> {
    None
}

#[cfg(target_arch = "wasm32")]
#[allow(dead_code)]
pub fn pick_folder() -> Option<String> {
    None
}

#[cfg(target_arch = "wasm32")]
pub fn pick_save(_: &str, _: &[&str], _: &str) -> Option<String> {
    None
}

#[allow(dead_code)]
fn unused(_: Value) {}

/// The map's coordinate system: Web Mercator, the globe, a preset, or any projected EPSG code. The
/// list is on the left; the right shows what the selection is, where it can be used, its PROJ
/// definition, and a link to its page on epsg.io.
fn coordinate_system(app: &mut RustGisApp, ui: &mut egui::Ui, t: &Tokens) -> bool {
    let current = app.session.map().projection.code();
    ui.label(egui::RichText::new(tl!("The map is drawn in this coordinate system; layers are projected on the fly.")).weak());
    let mut choices: Vec<(String, String)> = vec![("mercator".into(), "Web Mercator".into()), ("globe".into(), "Globe".into())];
    choices.extend(rustgis_engine::map::projection::PRESETS.iter().map(|(id, label, _)| (id.to_string(), label.to_string())));
    // Two panes of a fixed height, so the dialog doesn't stretch to the window.
    let pane_h = 340.0;
    ui.allocate_ui_with_layout(vec2(ui.available_width(), pane_h), egui::Layout::left_to_right(egui::Align::Min), |ui| {
        ui.set_height(pane_h);
        ui.vertical(|ui| {
            ui.set_width(270.0);
            egui::ScrollArea::vertical().id_salt("crs_list").max_height(pane_h - 44.0).auto_shrink([false, false]).show(ui, |ui| {
                ui.with_layout(egui::Layout::top_down_justified(egui::Align::LEFT), |ui| {
                    let picked = rustgis_engine::map::projection::parse_preset(&app.dialogs.text).ok().flatten().map(|(id, _)| id);
                    for (id, label) in &choices {
                        let selected = app.dialogs.text.eq_ignore_ascii_case(id) || picked == Some(id.as_str());
                        if ui.selectable_label(selected, tl!(label.as_str())).clicked() {
                            app.dialogs.text = id.clone();
                            app.dialogs.a.clear();
                        }
                    }
                });
            });
            ui.separator();
            ui.horizontal(|ui| {
                let input_label = ui.label(tl!("EPSG or ESRI code"));
                let r = ui
                    .add(egui::TextEdit::singleline(&mut app.dialogs.a).hint_text("3035, vn2000tm3").desired_width(120.0))
                    .labelled_by(input_label.id);
                if r.changed() && !app.dialogs.a.trim().is_empty() {
                    app.dialogs.text = typed_crs(&app.dialogs.a);
                }
            });
        });
        ui.separator();
        ui.vertical(|ui| {
            ui.set_width(340.0);
            egui::ScrollArea::vertical().id_salt("crs_details").max_height(pane_h).show(ui, |ui| {
                preset_fields(app, ui);
                crs_details(ui, &app.dialogs.text, &current, t);
            });
        });
    });
    ui.separator();
    let chosen = app.dialogs.text.clone();
    let check = rustgis_engine::map::Projection::from_code(&chosen);
    let same = check.as_ref().is_ok_and(|p| p.code() == current);
    let mut close = false;
    ui.horizontal(|ui| {
        if ui.add_enabled(check.is_ok() && !same, egui::Button::new(tl!("Apply"))).clicked() {
            match app.run("map.projection", json!({"type": chosen})) {
                Ok(_) => {
                    let _ = app.run("map.zoomFull", Value::Null);
                    close = true;
                }
                Err(e) => app.session.notify(e, true),
            }
        }
        if ui.button(tl!("Cancel")).clicked() {
            close = true;
        }
    });
    close
}

/// Formats degrees as an integer if whole (e.g. 105°), or with decimal places if fractional (e.g. 105.75°).
pub(crate) fn format_degrees(deg: f64) -> String {
    if (deg - deg.round()).abs() < 1e-6 { format!("{:.0}°", deg) } else { format!("{deg}°") }
}

/// The chosen preset's parameters (opengeos/RustGIS#517): the central meridian, and a conic's
/// standard parallels, each written back into the chosen code. Nothing for a preset without any.
fn preset_fields(app: &mut RustGisApp, ui: &mut egui::Ui) {
    use rustgis_engine::map::projection::{VN2000_UI_PROVINCES, parse_preset, preset_code, preset_params, preset_value};
    // A province typed by name is already the preset (`typed_crs`), and `vn2000:hanoi` is one too.
    let Ok(Some((id, params))) = parse_preset(&app.dialogs.text) else { return };
    let takes = preset_params(id);
    if takes.is_empty() {
        return;
    }
    let mut next = params.clone();
    if id == "vn2000tm3" {
        let default_lon = preset_value(id, "lon_0").unwrap_or(105.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new(tl!("Provinces:")).weak());
            for &(p_name, lon) in VN2000_UI_PROVINCES {
                let label = crate::i18n::fmt("{name} ({lon})", &[("name", p_name), ("lon", &format_degrees(lon))]);
                if ui.small_button(&label).clicked() {
                    next.retain(|(k, _)| *k != "lon_0");
                    if (lon - default_lon).abs() > 1e-6 {
                        next.push(("lon_0", lon));
                    }
                }
            }
        });
        ui.add_space(2.0);
    }
    egui::Grid::new("preset_params").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
        for p in takes {
            let label = match p.key {
                "lon_0" => tl!("Central meridian"),
                "lat_1" => tl!("First standard parallel"),
                _ => tl!("Second standard parallel"),
            };
            let input_label = ui.label(label);
            let mut v = params.iter().find(|(k, _)| *k == p.key).map(|(_, v)| *v).or_else(|| preset_value(id, p.key)).unwrap_or(0.0);
            let r = ui
                .add(egui::DragValue::new(&mut v).range(p.min..=p.max).speed(0.5).max_decimals(4).suffix("°"))
                .labelled_by(input_label.id);
            r.widget_info(|| egui::WidgetInfo { label: Some(label.to_string()), ..egui::WidgetInfo::drag_value(true, v) });
            if r.changed() {
                next.retain(|(k, _)| *k != p.key);
                // To the 4 decimals shown, so no float noise from a drag reaches the code.
                next.push((p.key, (v * 1e4).round() / 1e4));
            }
            ui.end_row();
        }
    });
    if next != params {
        // As the map will write it: in the preset's order, without the preset's own values.
        let next: Vec<(&str, f64)> = takes
            .iter()
            .filter_map(|p| next.iter().find(|(k, _)| *k == p.key))
            .filter(|(k, v)| preset_value(id, k) != Some(*v))
            .copied()
            .collect();
        app.dialogs.text = preset_code(id, &next);
    }
    ui.add_space(4.0);
}

/// A typed coordinate system code: `ESRI:102033` or `Epsg: 3035` as it is (`lookup` reads the
/// prefix in any case and spacing), a bare number as an EPSG code, or a Vietnamese province with
/// the `vn2000:` prefix (`vn2000:hanoi`); a bare name such as `hue` is not taken for one.
pub(crate) fn typed_crs(text: &str) -> String {
    let text = text.trim();
    if text.starts_with(|c: char| c.is_ascii_digit()) {
        format!("EPSG:{text}")
    } else if let Some(lon) = rustgis_engine::map::projection::resolve_vn2000_alias(text) {
        format!("vn2000tm3 +lon_0={lon}")
    } else {
        text.to_string()
    }
}

/// What a coordinate system is: its kind, area of use, definition and epsg.io page.
fn crs_details(ui: &mut egui::Ui, chosen: &str, current: &str, t: &Tokens) {
    use rustgis_engine::map::projection::{Domain, Projector, lookup};
    let projection = match rustgis_engine::map::Projection::from_code(chosen) {
        Ok(p) => p,
        Err(e) => {
            ui.label(egui::RichText::new(e).color(t.red));
            return;
        }
    };
    let code = projection.code();
    ui.label(egui::RichText::new(tl!(projection.label().as_str())).font(crate::theme::semibold(14.0)));
    if code == current {
        ui.label(egui::RichText::new(tl!("In use")).size(11.0).color(t.accent_text));
    }
    let lower = code.to_ascii_lowercase();
    let epsg = lower.strip_prefix("epsg:").or_else(|| lower.strip_prefix("esri:")).map(str::to_string);
    // A preset with parameters is described as its preset, without the page of its standard form.
    let preset = rustgis_engine::map::projection::parse_preset(&code).ok().flatten();
    let tuned = preset.as_ref().is_some_and(|(_, p)| !p.is_empty());
    let (kind, page): (&str, Option<String>) = match preset.map_or(code.as_str(), |(id, _)| id) {
        "mercator" => (tl!("World, conformal: the usual web map; areas grow towards the poles."), Some("3857".into())),
        "globe" => (tl!("A 3D globe seen from space."), None),
        "equirectangular" => (tl!("World, cylindrical: equal steps of longitude and latitude."), Some("4087".into())),
        "miller" => (tl!("World, cylindrical: a compromise between Mercator and equal steps."), Some("54003".into())),
        "mollweide" => (tl!("World, equal-area: true sizes, shapes bend towards the edges."), Some("54009".into())),
        "cylindricalEqualArea" => (tl!("World, equal-area, cylindrical: true sizes, flattened near the poles."), Some("54034".into())),
        "robinson" => (tl!("World, compromise: neither equal-area nor conformal, balanced for wall maps."), Some("54030".into())),
        "equalEarth" => (tl!("World, equal-area: true sizes with a Robinson-like look."), Some("8857".into())),
        "naturalEarth" => (tl!("World, compromise: rounded corners, gentle shapes near the poles."), None),
        "winkelTripel" => (tl!("World, compromise: low overall distortion, used for world atlases."), Some("54042".into())),
        "eckertIV" => (tl!("World, equal-area: true sizes with rounded poles."), Some("54012".into())),
        "northPolar" => (tl!("Polar, conformal: the Arctic and the northern lands."), None),
        "southPolar" => (tl!("Polar, conformal: Antarctica and the Southern Ocean."), Some("3031".into())),
        "europeEqualArea" => (tl!("Regional, equal-area: the European standard for statistics."), Some("3035".into())),
        "usaAlbers" => (tl!("Regional, equal-area: the contiguous United States."), Some("5070".into())),
        "albers" => {
            (tl!("Conic, equal-area: true sizes across a mid-latitude region; set its meridian and parallels to the region."), None)
        }
        "lambertConformal" => {
            (tl!("Conic, conformal: true shapes across a mid-latitude region; set its meridian and parallels to the region."), None)
        }
        "vn2000tm3" => (
            tl!(
                "Vietnam national coordinate system (VN-2000) using 3-degree Transverse Mercator with a 7-parameter Helmert shift to WGS 84."
            ),
            Some("5897".into()),
        ),
        "vn2000Utm48" => {
            (tl!("Vietnam national coordinate system (VN-2000) UTM zone 48N (102°E–108°E, central meridian 105°E)."), Some("3405".into()))
        }
        "vn2000Utm49" => {
            (tl!("Vietnam national coordinate system (VN-2000) UTM zone 49N (108°E–114°E, central meridian 111°E)."), Some("3406".into()))
        }
        _ => (tl!("A projected coordinate system."), epsg.clone()),
    };
    let page = page.filter(|_| !tuned);
    ui.label(kind);
    if let Ok(p) = Projector::new(&code) {
        let area = match p.domain() {
            Domain::World => tl!("Area of use: the whole world").to_string(),
            Domain::Box(e) => crate::i18n::fmt(
                "Area of use: longitude {w}° to {e}°, latitude {s}° to {n}°",
                &[
                    ("w", &format!("{:.0}", e.min_x)),
                    ("e", &format!("{:.0}", e.max_x)),
                    ("s", &format!("{:.0}", e.min_y)),
                    ("n", &format!("{:.0}", e.max_y)),
                ],
            ),
            Domain::Cap { center, radius } => crate::i18n::fmt(
                "Area of use: within {r}° of {lon}°, {lat}°",
                &[("r", &format!("{radius:.0}")), ("lon", &format!("{:.0}", center.x)), ("lat", &format!("{:.0}", center.y))],
            ),
            Domain::Band { lon0, half } => crate::i18n::fmt(
                "Area of use: within {r}° of the {lon}° meridian",
                &[("r", &format!("{half:.0}")), ("lon", &format!("{lon0:.0}"))],
            ),
        };
        ui.label(egui::RichText::new(area).size(11.5).color(t.text_dim));
    }
    if let Ok((def, _)) = lookup(&code) {
        ui.add_space(4.0);
        ui.label(egui::RichText::new(tl!("PROJ definition")).size(11.0).color(t.text_dim));
        ui.add(egui::Label::new(egui::RichText::new(def).monospace().size(10.5)).wrap());
    }
    if let Some(n) = page {
        ui.add_space(4.0);
        // ESRI codes, and the five-digit codes from 53000 up the presets link to, are Esri's.
        let esri = lower.starts_with("esri:") || (n.len() == 5 && n.starts_with('5'));
        let prefix = if esri { "ESRI" } else { "EPSG" };
        let text = crate::i18n::fmt("Open {code} on epsg.io", &[("code", &format!("{prefix}:{n}"))]);
        // epsg.io's pages carry no authority: an ESRI number that is also an EPSG code (ESRI:31491,
        // a Germany zone) goes to its search instead, which tells the two apart.
        let shared = lower.starts_with("esri:") && lookup(&format!("EPSG:{n}")).is_ok();
        let url = if shared { format!("https://epsg.io/?q=ESRI:{n}") } else { format!("https://epsg.io/{n}") };
        // In the browser build the link must not replace the app (and the map) in its tab.
        ui.add(egui::Hyperlink::from_label_and_url(text, url).open_in_new_tab(true));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The app in a harness, its fonts installed on the first pass (the dialog draws on the next).
    fn app_harness(app: RustGisApp) -> egui_kittest::Harness<'static, RustGisApp> {
        let mut fonts = false;
        egui_kittest::Harness::builder().with_size(vec2(1200.0, 900.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                let ctx = ui.ctx().clone();
                super::show(app, &ctx);
            },
            app,
        )
    }

    /// The map stamp Add From Service checks: the UI changes the map through `map_mut` and
    /// `layer_mut`, which leave the session revision alone, so the stamp follows a layer, its
    /// source and a tile layer's address.
    #[test]
    fn the_map_stamp_follows_the_map() {
        let mut map = rustgis_engine::map::Map::new("Map");
        let empty = map_stamp(&map);
        let mut ds = rustgis_engine::data::Dataset::new("Roads", rustgis_engine::data::GeometryType::Polyline, Vec::new());
        ds.source = Some("https://h/items?f=json".into());
        map.layers.push(rustgis_engine::map::Layer::new(1, ds));
        let listed = map_stamp(&map);
        assert_ne!(empty, listed);
        map.layers[0].dataset.source = Some("https://h/items?f=json&api_key=k".into());
        assert_ne!(listed, map_stamp(&map));
        let sourced = map_stamp(&map);
        map.tiles.push(rustgis_engine::map::TileLayer::new("T", rustgis_engine::map::TileKind::Raster, "https://h/{z}/{x}/{y}.png"));
        assert_ne!(sourced, map_stamp(&map));
        let tiled = map_stamp(&map);
        map.tiles[0].url = "https://h/other/{z}/{x}/{y}.png".into();
        assert_ne!(tiled, map_stamp(&map));
    }

    /// The web app's Export Features offers only formats it can write: no GeoPackage (SQLite) or
    /// FlatGeobuf (whose writer needs a temporary file; choosing it aborted the app). The
    /// dialog's own Format list is opened and read, as a user would.
    #[test]
    fn the_web_export_dialog_offers_no_geopackage_or_flatgeobuf() {
        use egui_kittest::kittest::Queryable;
        for web in [true, false] {
            let mut app = RustGisApp::with_sample();
            if web {
                app = app.with_services(crate::Services { download: Some(Box::new(|_, _| Ok(()))), ..Default::default() });
            }
            app.session.active_layer = app.session.map().layers.first().map(|l| l.id);
            app.run("ui.dialog", json!({"name": "exportLayer"})).unwrap();
            let mut h = egui_kittest::Harness::builder().with_size(vec2(1400.0, 900.0)).with_max_steps(64).build_ui_state(
                |ui, app: &mut RustGisApp| {
                    let ctx = ui.ctx().clone();
                    app.logic(&ctx);
                    app.ui(ui);
                },
                app,
            );
            h.run_steps(4);
            h.get_by_label("Format").click();
            h.run_steps(4);
            let offered = |label: &str| h.query_all_by_label(label).count() > 0;
            assert!(offered("GeoParquet") && offered("KML"), "web {web}: the Format list is open");
            assert_eq!(offered("FlatGeobuf"), !web, "web {web}");
            assert_eq!(offered("GeoPackage"), !web, "web {web}");
        }
        let web: Vec<&str> = export_formats(true).map(|(_, e)| *e).collect();
        assert_eq!(web, ["geojson", "parquet", "shp", "csv", "kml", "kmz", "gpx"]);
    }

    /// The Coordinate System dialog re-centres a preset (opengeos/RustGIS#517): the central
    /// meridian, set as assistive technology does, goes into the code that Apply uses.
    #[test]
    fn the_coordinate_system_dialog_sets_a_central_meridian() {
        use egui::accesskit::{Action, ActionData, ActionRequest, Role};
        use egui_kittest::kittest::{NodeT, Queryable};
        let mut app = RustGisApp::with_sample();
        app.run("map.projection", json!({"type": "robinson"})).unwrap();
        app.run("ui.dialog", json!({"name": "coordinateSystem"})).unwrap();
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 900.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                let ctx = ui.ctx().clone();
                super::show(app, &ctx);
            },
            app,
        );
        h.run_steps(3);
        // A world preset takes only its meridian.
        assert!(h.query_by_role_and_label(Role::SpinButton, "First standard parallel").is_none());
        let (node, tree) = h.get_by_role_and_label(Role::SpinButton, "Central meridian").accesskit_node().locate();
        h.event(egui::Event::AccessKitActionRequest(ActionRequest {
            action: Action::SetValue,
            target_tree: tree,
            target_node: node,
            data: Some(ActionData::NumericValue(150.000_000_000_03)),
        }));
        h.run_steps(3);
        assert_eq!(h.state().dialogs.text, "robinson +lon_0=150");
        h.get_by_label("Apply").click();
        h.run_steps(3);
        assert_eq!(h.state().session.map().projection.code(), "robinson +lon_0=150");
        // Set back to the preset's own value, the code is the preset alone.
        h.state_mut().dialogs.open = Some("coordinateSystem");
        h.run_steps(3);
        let (node, tree) = h.get_by_role_and_label(Role::SpinButton, "Central meridian").accesskit_node().locate();
        h.event(egui::Event::AccessKitActionRequest(ActionRequest {
            action: Action::SetValue,
            target_tree: tree,
            target_node: node,
            data: Some(ActionData::NumericValue(0.0)),
        }));
        h.run_steps(3);
        assert_eq!(h.state().dialogs.text, "robinson");
        // Parallels that make no cone: the fields stay, the reason shows below them, and Apply is off.
        h.state_mut().dialogs.text = "albers +lat_1=-30 +lat_2=30".into();
        h.run_steps(3);
        assert!(h.query_by_role_and_label(Role::SpinButton, "First standard parallel").is_some());
        assert!(h.query_by_label_contains("makes no cone").is_some());
        h.get_by_label("Apply").click();
        h.run_steps(3);
        assert_eq!(h.state().session.map().projection.code(), "robinson +lon_0=150");
    }

    #[test]
    fn vn2000_in_coordinate_system_dialog() {
        let mut app = RustGisApp::with_sample();
        app.run("ui.dialog", json!({"name": "coordinateSystem"})).unwrap();
        // A bare "vn2000" names no projection: it is passed on, and refused as ambiguous.
        app.dialogs.a = "vn2000".into();
        app.dialogs.text = typed_crs(&app.dialogs.a);
        assert_eq!(app.dialogs.text, "vn2000");
        let err = rustgis_engine::map::Projection::from_code(&app.dialogs.text).unwrap_err();
        assert!(err.contains("ambiguous"), "{err}");

        // Explicit "vn2000tm3" resolves to VN-2000 TM-3 preset
        app.dialogs.a = "vn2000tm3".into();
        app.dialogs.text = typed_crs(&app.dialogs.a);
        assert_eq!(app.dialogs.text, "vn2000tm3");
        let proj = rustgis_engine::map::Projection::from_code(&app.dialogs.text).unwrap();
        assert_eq!(proj.label(), "VN-2000 / TM-3 (Vietnam)");

        // Explicit "VN-2000 / UTM zone 48N" resolves to UTM 48N
        let proj_utm = rustgis_engine::map::Projection::from_code("VN-2000 / UTM zone 48N").unwrap();
        assert_eq!(proj_utm.label(), "VN-2000 / UTM zone 48N");

        // A province with the vn2000: prefix, in ASCII or Vietnamese.
        for (name, code) in [
            ("vn2000:hanoi", "vn2000tm3"),
            ("vn2000:Hà Nội", "vn2000tm3"),
            ("VN2000/Đà Nẵng", "vn2000tm3 +lon_0=107.75"),
            ("vn2000:tphcm", "vn2000tm3 +lon_0=105.75"),
            ("vn2000:vithanh", "vn2000tm3"),
            ("vn2000:vitri", "vn2000tm3"),
        ] {
            app.dialogs.a = name.into();
            app.dialogs.text = typed_crs(&app.dialogs.a);
            let proj_p = rustgis_engine::map::Projection::from_code(&app.dialogs.text).unwrap();
            assert!(proj_p.label().contains("VN-2000"), "{name}");
            assert_eq!(proj_p.code(), code, "{name}");
        }
        // A bare province name is not a coordinate system: words such as "hue" stay as typed,
        // as `map.projection` reads them.
        for name in ["hanoi", "Hà Nội", "hue", "vinh", "tphcm", "vithanh", "vitri", "vietnam"] {
            assert_eq!(typed_crs(name), name);
            assert!(rustgis_engine::map::Projection::from_code(name).is_err(), "{name}");
        }
        // A prefixed province given to the command is saved as the preset too.
        app.run("map.projection", json!({"type": "VN2000/Đà Nẵng"})).unwrap();
        assert_eq!(app.session.map().projection.code(), "vn2000tm3 +lon_0=107.75");

        // Applying VN-2000 TM-3
        app.run("map.projection", json!({"type": "vn2000tm3"})).unwrap();
        assert_eq!(app.session.map().projection.code(), "vn2000tm3");

        // Custom meridian for HCMC (105.75)
        app.run("map.projection", json!({"type": "vn2000tm3 +lon_0=105.75"})).unwrap();
        assert_eq!(app.session.map().projection.code(), "vn2000tm3 +lon_0=105.75");
    }

    #[test]
    fn ui_degree_formatting_preserves_fractions() {
        assert_eq!(format_degrees(105.0), "105°");
        assert_eq!(format_degrees(107.0), "107°");
        assert_eq!(format_degrees(105.75), "105.75°");
        assert_eq!(format_degrees(108.5), "108.5°");
        assert_eq!(format_degrees(104.25), "104.25°");
    }

    /// Add From Service's row button says Remove for a service layer already on the map, and
    /// removes it; once it is gone the row says Add again.
    #[test]
    fn a_service_layers_row_offers_to_remove_it_once_added() {
        use egui_kittest::kittest::Queryable;
        let mut app = RustGisApp::with_sample();
        let url = "https://example.com/service/{z}/{x}/{y}.png";
        app.run("map.addTiles", json!({"url": url, "name": "Service Roads", "overlay": true})).unwrap();
        app.ui.service = Some(crate::ServiceConn {
            url: "https://example.com/service".into(),
            kind: "wms",
            slot: Default::default(),
            text: None,
            documents: Vec::new(),
            pending: None,
            listing: Some(json!({
                "kind": "WMS",
                "title": "Roads",
                "count": 1,
                "layers": [{
                    "name": "roads",
                    "title": "Service Roads",
                    "bbox": null,
                    "offer": {"type": "tiles", "url": url},
                }],
            })),
            added: None,
            error: None,
            failure: None,
        });
        app.run("ui.dialog", json!({"name": "addService"})).unwrap();
        app.dialogs.mode = "wms";
        let mut h = app_harness(app);
        h.run_steps(4);
        assert!(h.query_by_label("Add").is_none(), "the layer is already on the map, so Add is not offered");
        h.get_by_label("Remove").click();
        h.run_steps(4);
        assert!(h.state().session.map().tiles.iter().all(|t| t.url != url), "Remove took the tile layer off the map");
        assert!(h.query_by_label("Add").is_some());
        assert!(h.query_by_label("Remove").is_none(), "nothing is left to remove");
    }

    /// A service's feature layer, downloaded in the background, keeps the listed address as its
    /// source even under another name: its row offers Remove, which takes it off the map and
    /// offers Add again.
    #[test]
    fn a_service_feature_layer_is_known_once_added_by_its_address() {
        use egui_kittest::kittest::Queryable;
        let mut app = RustGisApp::with_sample();
        let url = "https://example.com/collections/places/items";
        let id = app.session.download_started();
        assert!(app.session.download_landed(
            id,
            br#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[0,0]}}]}"#.to_vec(),
        ));
        // A name of its own, so only the address it came from tells the dialog it is there.
        app.run("layer.addDownload", json!({"id": id, "url": url, "name": "Places copy", "service": true})).unwrap();
        let layer = app.session.map().layer_by_name("Places copy").unwrap().id;
        app.ui.service = Some(crate::ServiceConn {
            url: "https://example.com/collections".into(),
            kind: "wfs",
            slot: Default::default(),
            text: None,
            documents: Vec::new(),
            pending: None,
            listing: Some(json!({
                "kind": "WFS",
                "title": "Places",
                "count": 1,
                "layers": [{
                    "name": "places",
                    "title": "Service Places",
                    "bbox": null,
                    "offer": {"type": "features", "url": url},
                }],
            })),
            added: None,
            error: None,
            failure: None,
        });
        app.run("ui.dialog", json!({"name": "addService"})).unwrap();
        app.dialogs.mode = "wfs";
        let mut h = app_harness(app);
        h.run_steps(4);
        assert!(h.query_by_label("Add").is_none(), "the layer is already on the map, so Add is not offered");
        h.get_by_label("Remove").click();
        h.run_steps(4);
        assert!(h.state().session.map().layer(layer).is_none(), "Remove took the feature layer off the map");
        assert!(h.query_by_label("Add").is_some());
        assert!(h.query_by_label("Remove").is_none(), "nothing is left to remove");
    }

    /// A coverage row is known by its GetCoverage request: the service's address and its own id.
    /// Its Remove takes the layer off the map and offers Add again.
    #[test]
    fn a_service_coverage_is_known_once_added_by_its_request() {
        use egui_kittest::kittest::Queryable;
        let mut app = RustGisApp::with_sample();
        app.run(
            "layer.addDataText",
            json!({
                "fileName": "soil.geojson",
                "name": "Soil carbon copy",
                "text": r#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[0,0]}}]}"#,
            }),
        )
        .unwrap();
        let layer = app.session.map().layer_by_name("Soil carbon copy").unwrap().id;
        // What a coverage download keeps (a feature layer stands in for the raster here: the
        // dialog reads the layer's source and the GetCoverage request, not its kind).
        let wcs = "https://example.com/wcs";
        let get = "https://example.com/wcs?SERVICE=WCS&VERSION=2.0.1&REQUEST=GetCoverage&COVERAGEID=soc&FORMAT=image%2Ftiff";
        app.session.map_mut().layer_mut(layer).unwrap().dataset.source = Some(get.into());
        app.ui.service = Some(crate::ServiceConn {
            url: wcs.into(),
            kind: "wcs",
            slot: Default::default(),
            text: None,
            documents: Vec::new(),
            pending: None,
            listing: Some(json!({
                "kind": "WCS",
                "title": "Soils",
                "count": 1,
                "layers": [{
                    "name": "soc",
                    "title": "Soil carbon",
                    "bbox": null,
                    "offer": {
                        "type": "coverage",
                        "url": wcs,
                        "describe": "https://example.com/wcs?SERVICE=WCS&VERSION=2.0.1&REQUEST=DescribeCoverage&COVERAGEID=soc",
                        "version": "2.0.1",
                    },
                }],
            })),
            added: None,
            error: None,
            failure: None,
        });
        app.run("ui.dialog", json!({"name": "addService"})).unwrap();
        app.dialogs.mode = "wcs";
        let mut h = app_harness(app);
        h.run_steps(4);
        assert!(h.query_by_label("Add").is_none(), "the coverage is already on the map, so Add is not offered");
        h.get_by_label("Remove").click();
        h.run_steps(4);
        assert!(h.state().session.map().layer(layer).is_none(), "Remove took the coverage off the map");
        assert!(h.query_by_label("Add").is_some());
        assert!(h.query_by_label("Remove").is_none(), "nothing is left to remove");
    }

    /// A grouped OGC API collection imported with Import All says it is on the map; its row has no
    /// Add or Remove (the group's child is removed in the Layers pane).
    #[test]
    fn an_imported_collection_says_it_is_on_the_map() {
        use egui_kittest::kittest::Queryable;
        let mut app = RustGisApp::with_sample();
        let url = "https://example.com/collections/places/items";
        app.run(
            "layer.addDataText",
            json!({
                "fileName": "places.geojson",
                "name": "Places copy",
                "text": r#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[0,0]}}]}"#,
            }),
        )
        .unwrap();
        let layer = app.session.map().layer_by_name("Places copy").unwrap().id;
        // What `service.import` gives each collection's child.
        app.session.map_mut().layer_mut(layer).unwrap().dataset.source = Some(url.into());
        app.ui.service = Some(crate::ServiceConn {
            url: "https://example.com/collections".into(),
            kind: "ogcapi",
            slot: Default::default(),
            text: None,
            documents: Vec::new(),
            pending: None,
            listing: Some(json!({
                "kind": "OGC API Features",
                "title": "Places",
                "count": 1,
                "layers": [{
                    "name": "places",
                    "title": "Service Places",
                    "bbox": null,
                    "offer": {"type": "features", "url": url},
                }],
            })),
            added: None,
            error: None,
            failure: None,
        });
        app.run("ui.dialog", json!({"name": "addService"})).unwrap();
        app.dialogs.mode = "ogcapi";
        let mut h = app_harness(app);
        h.run_steps(4);
        assert!(h.query_by_label("On the map").is_some(), "the collection is in the imported group");
        assert!(h.query_by_label("Add").is_none(), "grouped features have no individual Add");
        assert!(h.query_by_label("Remove").is_none(), "grouped features are removed in the Layers pane");
    }
}
