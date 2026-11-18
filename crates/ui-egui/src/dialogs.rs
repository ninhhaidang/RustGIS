//! Dialogs (Select By Attributes, Go To XY, bookmarks, fields, definition query, rename) and
//! native file pickers.

use egui::{Align2, vec2};
use rustgis_engine::Session;
use rustgis_engine::cmd::service_added::Removal;
use serde_json::{Value, json};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::theme::{Tokens, semibold};
use crate::{RustGisApp, i18n};

#[derive(Default)]
pub struct Dialogs {
    /// The layer captured when Define Projection opens; changing selection must not retarget it.
    pub projection_layer: Option<u32>,
    /// Map-local layer IDs require the dialog to retain its original project and map as well.
    pub projection_map: Option<(u64, usize, String)>,
    pub geometry: Option<crate::geometrycalc::Form>,
    pub repair: Option<crate::repairsource::Form>,
    #[cfg(not(target_arch = "wasm32"))]
    pub reload: Option<crate::reloaddata::Form>,
    pub open: Option<&'static str>,
    pub layer: String,
    /// The standalone table Add Field and Calculate Field act on, when opened from its view: the
    /// project it is in ([`Session::opened`]) and its id, which another project may give again;
    /// None for the active layer.
    pub table: Option<(u64, u32)>,
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
    /// Export Features' GeoPackage destination: one of [`GPKG_MODES`]' codes (empty: a new file).
    pub gpkg_mode: &'static str,
    /// The layer Export Features was opened on (its id and instance), exported whatever is
    /// selected while the dialog stays open.
    pub export_layer: Option<(u32, u64)>,
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
    /// Explicit column selections survive coordinate-system changes, including a pick of None.
    pub csv_picked: bool,
    /// Coordinate-system edits are applied on focus loss, rather than parsed while typing.
    pub csv_crs_dirty: bool,
    /// Import DXF's file, picked before the dialog opens.
    pub dxf: Option<DxfFile>,
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

/// A drawing picked for Import DXF.
pub struct DxfFile {
    pub name: String,
    pub source: DxfSource,
}

/// Where a drawing picked for Import DXF is: on disk, or (in the browser) its bytes, kept until it
/// is added.
pub enum DxfSource {
    Path(String),
    Bytes(Vec<u8>),
}

impl Dialogs {
    /// Opens Add Field or Calculate Field (`which`) on standalone table `id`.
    pub fn open_on_table(&mut self, which: &str, s: &Session, id: u32) -> Result<(), String> {
        // The name kept is a `&'static str`, as `open` keeps it.
        let name: &'static str = match which {
            "addField" => "addField",
            "calculateField" => "calculateField",
            other => return Err(format!("dialog '{other}' doesn't open on a table")),
        };
        let dataset = &s.project.table(id).ok_or("no such table")?.dataset;
        *self = Dialogs { open: Some(name), mode: "new", reset_collapse: true, table: Some((s.opened, id)), ..Dialogs::default() };
        match name {
            "addField" => self.b = "Double".into(),
            _ => self.a = calculation_fields(dataset).last().map(|f| f.name.clone()).unwrap_or_default(),
        }
        Ok(())
    }

    /// The standalone table the dialog acts on, as `"table"` names it in `p`; refused once another
    /// project is open, whose table of that id is another table.
    fn on_table(&self, s: &Session, mut p: Value) -> Result<Value, String> {
        match self.table {
            Some((opened, _)) if opened != s.opened => {
                Err(tl!("Another project is open now: open the dialog again from its table.").into())
            }
            Some((_, id)) => {
                p["table"] = json!(id);
                Ok(p)
            }
            None => Ok(p),
        }
    }

    /// Opens a dialog on the current selection, capturing the target of CRS changes.
    pub fn open(&mut self, which: &str, s: &Session) -> Result<(), String> {
        let active = s.active_layer.and_then(|id| s.map().layer(id));
        let name: &'static str = match which {
            "selectByAttributes" => "selectByAttributes",
            "goToXY" => "goToXY",
            "bookmark" => "bookmark",
            "addField" => "addField",
            "calculateField" => "calculateField",
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
            "addDxf" => "addDxf",
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
            "exportOrbit" => "exportOrbit",
            other => return Err(format!("unknown dialog '{other}'")),
        };
        if matches!(name, "addField" | "calculateField" | "renameLayer" | "exportLayer" | "defineProjection") && active.is_none() {
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
        if name == "exportOrbit" && cfg!(target_arch = "wasm32") {
            return Err("exporting an orbit needs the desktop app".into());
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
                self.text = active.and_then(|l| l.dataset.crs).map(|c| c.definition_text()).unwrap_or_else(|| "EPSG:4326".into());
            }
            "coordinateSystem" => {
                self.text = s.map().projection.code();
                // An EPSG code goes in the code box; presets are picked from the list.
                let is_code = self.text.to_ascii_lowercase().starts_with("epsg");
                self.a = if is_code { self.text.trim_start_matches(|c: char| !c.is_ascii_digit()).to_string() } else { String::new() };
            }
            "addDxf" => {
                // A drawing never says its coordinate system: the map's, where it is drawn.
                self.text = map_crs(&s.map().projection);
                // Its blocks (图块) are drawn unless the user says otherwise.
                self.flag = true;
            }
            "goToXY" => {
                // The map's center, the longitude within ±180 even when the view has been panned
                // past the antimeridian.
                let c = s.map().center_lonlat();
                self.a = format!("{:.4}", crate::mapmenu::wrap_lon(c.x));
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
            // The command's defaults: 60 frames once round, 640 × 400 points, 50 ms a frame.
            "exportOrbit" => [self.a, self.b, self.c, self.d, self.e] = ["60", "360", "640", "400", "50"].map(String::from),
            "calculateField" => {
                self.a = active.and_then(|l| calculation_fields(&l.dataset).last()).map(|f| f.name.clone()).unwrap_or_default()
            }
            "renameLayer" => self.text = active.map(|l| l.name.clone()).unwrap_or_default(),
            // A file name can't hold a folder separator, which a project name may.
            "saveProject" => self.text = format!("{}.rgis", s.project.name.replace(['/', '\\'], "-")),
            "exportLayer" => {
                self.mode = "geojson";
                self.gpkg_mode = "newFile";
                self.export_layer = active.map(|l| (l.id, l.instance_id()));
                // The GeoPackage table's name.
                self.text = active.map(|l| l.name.clone()).unwrap_or_default();
            }
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
    #[cfg(not(target_arch = "wasm32"))]
    if which == "reloadData" {
        crate::reloaddata::show(app, ctx);
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
        "addDxf" => "Import DXF",
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
        "exportOrbit" => "Export Orbit",
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
                // UTM and MGRS are the Earth's grids, so another body's map offers degrees alone.
                // A grid reference, once typed, is where Go goes: the degree boxes gray out.
                let earth = app.session.map().body().is_earth();
                let by_grid = earth && !app.dialogs.text.trim().is_empty();
                egui::Grid::new("xy").num_columns(2).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
                    let input_label = ui.label(tl!("Longitude (X)"));
                    ui.add_enabled(!by_grid, egui::TextEdit::singleline(&mut app.dialogs.a).hint_text("-74.006 · 74°0′22″W").desired_width(300.0))
                        .labelled_by(input_label.id);
                    ui.end_row();
                    let input_label = ui.label(tl!("Latitude (Y)"));
                    ui.add_enabled(!by_grid, egui::TextEdit::singleline(&mut app.dialogs.b).hint_text("40.7128 · 40°42′46″N").desired_width(300.0))
                        .labelled_by(input_label.id);
                    ui.end_row();
                    if earth {
                        let input_label = ui.label(tl!("Or grid reference"));
                        ui.add(egui::TextEdit::singleline(&mut app.dialogs.text).hint_text("18TWL8395907350 · 18north 583959 4507351").desired_width(300.0))
                            .labelled_by(input_label.id);
                        ui.end_row();
                    }
                    let input_label = ui.label(tl!("Scale 1:"));
                    ui.add(egui::TextEdit::singleline(&mut app.dialogs.c).hint_text(tl!("keep current")).desired_width(300.0)).labelled_by(input_label.id);
                    ui.end_row();
                });
                // Below the boxes, wrapped to the dialog's width rather than widening it.
                let hint = if earth {
                    tl!("Degrees as decimals or degrees, minutes and seconds; or a UTM or MGRS reference in place of both.")
                } else {
                    tl!("Degrees as decimals or degrees, minutes and seconds.")
                };
                ui.label(egui::RichText::new(hint).weak());
                close = buttons(app, ui, "Go", |app| {
                    let grid = app.dialogs.text.trim();
                    let mut p = if app.session.map().body().is_earth() && !grid.is_empty() {
                        json!({"position": grid})
                    } else {
                        json!({"lon": app.dialogs.a.trim(), "lat": app.dialogs.b.trim()})
                    };
                    // The command refuses a scale it can't use: commas go only where they group
                    // thousands (50,000), so a decimal comma (1,5) is refused, not read as 15.
                    let scale = scale_text(&app.dialogs.c);
                    if !scale.is_empty() {
                        p["scale"] = json!(scale);
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
            "addDxf" => close = add_dxf(app, ui),
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
            "exportOrbit" => {
                ui.label(tl!("Turns the view about its centre, keeping its scale and pitch, and writes a frame a step: a .gif loops, a .zip holds the frames as PNGs."));
                egui::Grid::new("orbit").num_columns(2).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
                    let d = &mut app.dialogs;
                    for (label, v) in [
                        ("Frames", &mut d.a),
                        ("Turn (degrees)", &mut d.b),
                        ("Width (points)", &mut d.c),
                        ("Height (points)", &mut d.d),
                        ("Frame time (ms, GIF)", &mut d.e),
                    ] {
                        let input_label = ui.label(tl!(label));
                        ui.add(egui::TextEdit::singleline(v).desired_width(120.0)).labelled_by(input_label.id);
                        ui.end_row();
                    }
                });
                // A file picker cancelled keeps the dialog open, the values typed still in it:
                // `ui.exportOrbit` answers null when no file was picked.
                let mut picked = true;
                close = buttons(app, ui, "Export…", |app| {
                    // Empty keeps the command's default; anything else must be a number, which
                    // the command checks against its range.
                    let mut p = json!({});
                    let d = &app.dialogs;
                    for (key, label, v) in [("frames", "Frames", &d.a), ("turn", "Turn", &d.b), ("width", "Width", &d.c), ("height", "Height", &d.d), ("speed", "Frame time", &d.e)] {
                        let v = v.trim();
                        if !v.is_empty() {
                            let n = v.parse::<f64>().ok().filter(|n| n.is_finite());
                            p[key] = json!(n.ok_or_else(|| crate::i18n::fmt("{what} must be a number", &[("what", tl_raw!(label))]))?);
                        }
                    }
                    picked = !app.run("ui.exportOrbit", p)?.is_null();
                    Ok(())
                }) && picked;
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
                    // Each type as the command names it, and as the interface shows it.
                    let types =
                        [("Long", tl!("Long")), ("Double", tl!("Double")), ("Text", tl!("Text")), ("Boolean", tl!("Boolean")), ("Date", tl!("Date")), ("DateTime", tl!("Date and time"))];
                    let shown = types.iter().find(|(ty, _)| *ty == app.dialogs.b).map_or_else(|| app.dialogs.b.clone(), |(_, label)| label.to_string());
                    egui::ComboBox::from_id_salt("af_type").selected_text(shown).show_ui(ui, |ui| {
                        for (ty, label) in &types {
                            ui.selectable_value(&mut app.dialogs.b, ty.to_string(), label.to_string());
                        }
                    }).response.labelled_by(input_label.id);
                    ui.end_row();
                });
                close = buttons(app, ui, "Add", |app| {
                    let p = app.dialogs.on_table(&app.session, json!({"name": app.dialogs.a, "type": app.dialogs.b}))?;
                    app.run("edit.addField", p).map(|_| ())
                });
            }
            "calculateField" => {
                /// The data Calculate Field reads: the table's it was opened on, else the active layer's.
                fn data(app: &crate::RustGisApp) -> Option<&rustgis_engine::data::Dataset> {
                    data_of(app.dialogs.table, &app.session)
                }
                /// The same, from the dialog's table and the session apart, so the box can be
                /// written while it's read.
                fn data_of(
                    table: Option<(u64, u32)>,
                    session: &rustgis_engine::Session,
                ) -> Option<&rustgis_engine::data::Dataset> {
                    match table {
                        Some((opened, id)) => session.project.table(id).filter(|_| opened == session.opened).map(|t| &t.dataset),
                        None => session.active_layer.and_then(|id| session.map().layer(id)).map(|l| &l.dataset),
                    }
                }
                let fields: Vec<String> = data(app).map(|d| calculation_fields(d).map(|f| f.name.clone()).collect()).unwrap_or_default();
                ui.horizontal(|ui| {
                    let input_label = ui.label(tl!("Field"));
                    egui::ComboBox::from_id_salt("cf_field").selected_text(app.dialogs.a.clone()).width(220.0).show_ui(ui, |ui| {
                        for f in &fields {
                            ui.selectable_value(&mut app.dialogs.a, f.clone(), f);
                        }
                    }).response.labelled_by(input_label.id);
                });
                let input_label = ui.label(format!("{} =", app.dialogs.a));
                // Suggestions as it's typed: the fields, functions, variables and keywords (#1240).
                let ds = data_of(app.dialogs.table, &app.session);
                fn style(edit: egui::TextEdit<'_>) -> egui::TextEdit<'_> {
                    edit.hint_text(tl!("e.g.  pop_est / 1000000   or   upper(name) || ' ' || round(area, 1)"))
                        .char_limit(rustgis_engine::data::query::MAX_LEN)
                        .font(egui::TextStyle::Monospace)
                        .desired_width(f32::INFINITY)
                }
                let id = egui::Id::unique("cf_expression");
                crate::exprcomplete::expr_edit(ui, id, &mut app.dialogs.text, ds, style).labelled_by(input_label.id);
                // Read once no suggestion is offered: a word still being chosen isn't an error yet.
                let text = app.dialogs.text.trim();
                let reads = data(app).filter(|_| !text.is_empty() && !crate::exprcomplete::offering(ui, id)).map(|d| rustgis_engine::cmd::edit::check_calculation(text, d));
                if let Some(Err(e)) = reads {
                    let t = Tokens::get(ui.ctx());
                    ui.label(egui::RichText::new(crate::i18n::fmt("Invalid: {error}", &[("error", &e)])).small().color(t.red));
                }
                // A standalone table has no selection: every row.
                let help = if app.dialogs.table.is_some() {
                    tl!("Fields, numbers, 'text', + - * / % ^, || to join text, comparisons, CASE and functions such as round(), upper(), if() or scale_linear(). Applies to every row of the table.")
                } else {
                    tl!("Fields, numbers, 'text', + - * / % ^, || to join text, comparisons, CASE and functions such as round(), upper(), if() or scale_linear(). Applies to the selected rows, or every row when none is selected.")
                };
                ui.label(egui::RichText::new(help).small().weak());
                close = buttons(app, ui, "Calculate", |app| {
                    if !fields.contains(&app.dialogs.a) {
                        return Err(tl!("Choose a field from the list: the previous choice is missing or shadowed by an earlier field with the same name ignoring case.").into());
                    }
                    let p = app.dialogs.on_table(&app.session, json!({"field": app.dialogs.a, "expression": app.dialogs.text}))?;
                    app.run("edit.calculateField", p).map(|_| ())
                });
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
            let translated = i18n::file_size_error(e, i18n::fmt);
            ui.label(egui::RichText::new(translated.as_deref().unwrap_or(e)).color(t.red));
        }
        if let Some(i) = &app.dialogs.info {
            ui.label(egui::RichText::new(i).color(t.green));
        }
    });
    if (!open || close) && app.dialogs.open == Some(which) {
        app.dialogs.open = None;
        if which == "addDxf" {
            app.dialogs.dxf = None;
        }
        // Drawings picked or dropped together: the next opens as one closes.
        if which == "addDxf"
            && let Some(next) = app.ui.dxf_waiting.pop_front()
            && app.dialogs.open("addDxf", &app.session).is_ok()
        {
            app.dialogs.dxf = Some(next);
        }
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
    let place = if cfg!(any(target_os = "android", target_os = "ios")) {
        tl!("You choose where the project is saved next.")
    } else {
        tl!("The project downloads as this file, to the folder your browser saves downloads in.")
    };
    ui.label(egui::RichText::new(place).small().weak());
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

/// Where Export Features puts a GeoPackage's table: (`share.exportLayer` mode, label shown).
pub const GPKG_MODES: &[(&str, &str)] =
    &[("newFile", "A new GeoPackage"), ("addTable", "A new table in a GeoPackage"), ("replaceTable", "A table replaced in a GeoPackage")];

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
        // A GeoPackage can take the layer as one more table (desktop only: the browser offers none).
        if app.dialogs.mode == "gpkg" && !web {
            let into_label = ui.label(tl!("Into"));
            let shown = GPKG_MODES.iter().find(|(m, _)| *m == app.dialogs.gpkg_mode).map_or(GPKG_MODES[0].1, |(_, l)| *l);
            egui::ComboBox::from_id_salt("export_gpkg_mode")
                .selected_text(tl!(shown))
                .width(220.0)
                .show_ui(ui, |ui| {
                    for (mode, label) in GPKG_MODES {
                        ui.selectable_value(&mut app.dialogs.gpkg_mode, *mode, tl!(*label));
                    }
                })
                .response
                .labelled_by(into_label.id);
            ui.end_row();
            if !matches!(app.dialogs.gpkg_mode, "" | "newFile") {
                let table_label = ui.label(tl!("Table"));
                ui.add(egui::TextEdit::singleline(&mut app.dialogs.text).desired_width(220.0)).labelled_by(table_label.id);
                ui.end_row();
            }
        }
    });
    if app.dialogs.mode == "shp" && web {
        ui.label(egui::RichText::new(tl!("The Shapefile's files download as one zip.")).small().weak());
    }
    let into_existing = app.dialogs.mode == "gpkg" && !web && !matches!(app.dialogs.gpkg_mode, "" | "newFile");
    if into_existing {
        ui.label(egui::RichText::new(tl!("Pick the GeoPackage next. Its other tables are kept.")).small().weak());
    }
    buttons(app, ui, if web { "Download" } else { "Export…" }, |app| {
        let mut p = json!({"format": app.dialogs.mode});
        if let Some((layer, instance)) = app.dialogs.export_layer {
            p["layer"] = json!(layer);
            p["instance"] = json!(instance);
        }
        if into_existing {
            p["mode"] = json!(app.dialogs.gpkg_mode);
            p["tableName"] = json!(app.dialogs.text);
        }
        app.run("ui.exportLayer", p).map(|_| ())
    })
}

/// Add Delimited Text's separators: (code, name).
const SEPARATORS: [(&str, &str); 5] = [("auto", "Automatic"), (",", "Comma"), (";", "Semicolon"), ("\t", "Tab"), ("|", "Pipe")];

/// Opens Add Delimited Text for a file picked: its columns, with the ones found by name picked.
pub fn open_csv(app: &mut RustGisApp, name: &str, source: CsvSource) -> Result<Value, String> {
    app.run("ui.dialog", json!({"name": "addCsv"}))?;
    let file = CsvFile { name: name.to_string(), source };
    app.dialogs.mode = "auto";
    app.dialogs.csv_picked = false;
    let found = csv_columns(&file, "auto", &app.dialogs.text);
    show_columns(&mut app.dialogs, found);
    app.dialogs.csv = Some(file);
    Ok(Value::Null)
}

/// Opens Import DXF for a drawing picked: its coordinate system (the map's, to start with) and
/// whether its blocks (图块) are drawn. A drawing picked while another waits opens after it, so
/// neither is lost.
pub fn open_dxf(app: &mut RustGisApp, name: &str, source: DxfSource) -> Result<Value, String> {
    let file = DxfFile { name: name.to_string(), source };
    if app.dialogs.open == Some("addDxf") && app.dialogs.dxf.is_some() {
        app.ui.dxf_waiting.push_back(file);
        return Ok(Value::Null);
    }
    app.run("ui.dialog", json!({"name": "addDxf"}))?;
    app.dialogs.dxf = Some(file);
    Ok(Value::Null)
}

/// Opens Import DXF for a drawing on disk (a drop, Add Data, the Browser pane, `ui.addFile`).
pub fn open_dxf_path(app: &mut RustGisApp, path: &str) -> Result<Value, String> {
    open_dxf(app, path, DxfSource::Path(path.to_string()))
}

/// The current map's coordinate system, as a DXF import can be given it: Web Mercator and the
/// globe as their EPSG codes; another flat projection as its EPSG code or, for a preset, what the
/// map projects with, which is the map's exactly (a definition `Crs::parse` reads).
fn map_crs(p: &rustgis_engine::map::Projection) -> String {
    use rustgis_engine::map::Projection;
    match p {
        Projection::WebMercator => "EPSG:3857".into(),
        Projection::Globe => "EPSG:4326".into(),
        Projection::Custom(code) => {
            let up = code.trim().to_ascii_uppercase();
            if up.starts_with("EPSG:") {
                up
            } else {
                rustgis_engine::map::projection::lookup(code).map(|(def, _)| def).unwrap_or_else(|_| code.trim().to_string())
            }
        }
    }
}

/// Import DXF, for the drawing picked: the coordinate system its coordinates are in (blank keeps
/// them as they are) and whether its blocks (图块) are drawn.
fn add_dxf(app: &mut RustGisApp, ui: &mut egui::Ui) -> bool {
    let Some(name) = app.dialogs.dxf.as_ref().map(|f| f.name.clone()) else {
        // Opened without a file (`ui.dialog`): the file first.
        ui.label(tl!("Choose a DXF drawing, then set its coordinate system and whether its blocks are drawn."));
        let close = buttons(app, ui, "Choose File…", |app| app.run("ui.addDxf", Value::Null).map(|_| ()));
        return close && app.dialogs.dxf.is_none();
    };
    let shown = std::path::Path::new(&name).file_name().and_then(|n| n.to_str()).unwrap_or(&name).to_string();
    ui.label(egui::RichText::new(shown).strong());
    ui.label(
        egui::RichText::new(tl!(
            "A drawing never says its coordinate system: its coordinates are read in this one. Leave it blank to keep them as they are."
        ))
        .weak(),
    );
    egui::Grid::new("dxf").num_columns(2).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
        let input_label = ui.label(tl!("Coordinate system"));
        let response = ui.add(egui::TextEdit::singleline(&mut app.dialogs.text).hint_text("EPSG:3857").desired_width(260.0));
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, tl!("Coordinate system")));
        response.labelled_by(input_label.id);
        ui.end_row();
    });
    ui.add_space(4.0);
    ui.checkbox(&mut app.dialogs.flag, tl!("Import block contents"));
    buttons(app, ui, "Add", |app| {
        let mut p = json!({"blocks": app.dialogs.flag});
        if !app.dialogs.text.trim().is_empty() {
            p["crs"] = json!(app.dialogs.text.trim());
        }
        let file = app.dialogs.dxf.take().ok_or("choose a file first")?;
        let r = match &file.source {
            DxfSource::Path(path) => {
                p["path"] = json!(path);
                add_dxf_file(app, p)
            }
            DxfSource::Bytes(bytes) => app.import_dxf_bytes(&file.name, bytes, p),
        };
        // Kept for another try.
        if r.is_err() {
            app.dialogs.dxf = Some(file);
        }
        r.map(|_| ())
    })
}

/// Adds a drawing on disk, read as Add Data reads a file (on a thread of its own when the desktop
/// app reads it so, [`crate::readjob`]).
pub(crate) fn add_dxf_file(app: &mut RustGisApp, p: Value) -> Result<Value, String> {
    #[cfg(not(target_arch = "wasm32"))]
    return crate::readjob::run(app, "layer.addData", p);
    #[cfg(target_arch = "wasm32")]
    app.run("layer.addData", p)
}

/// The columns of a file picked for Add Delimited Text, split at `separator` (a code of
/// [`SEPARATORS`]), respecting the coordinate system entered in the dialog.
fn csv_columns(file: &CsvFile, separator: &str, crs: &str) -> Result<Value, String> {
    let mut p = if separator == "auto" { json!({}) } else { json!({"delimiter": separator}) };
    if !crs.trim().is_empty() {
        p["crs"] = json!(crs.trim());
    }
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
            // Nothing picked is left, so a corrected coordinate system finds the columns again.
            d.csv_picked = false;
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
    } else {
        // The picks are gone with the old columns, so later defaults may follow the CRS again.
        d.csv_picked = false;
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
    ui.label(
        egui::RichText::new(tl!(
            "Automatic chooses columns using the whole file. Open a list to see the preview suggestions or choose columns yourself."
        ))
        .weak(),
    );
    let mut reread = false;
    let mut crs_committed = false;
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
            let selected = if !d.csv_picked {
                tl!("Automatic").to_string()
            } else if v.is_empty() {
                tl!("None").to_string()
            } else {
                v.clone()
            };
            let menu = egui::ComboBox::from_id_salt(("csv_column", i)).selected_text(selected).width(240.0).show_ui(ui, |ui| {
                let mut picked = ui.selectable_value(v, String::new(), tl!("None")).clicked();
                for c in &columns {
                    picked |= ui.selectable_value(v, c.clone(), c.as_str()).clicked();
                }
                picked
            });
            menu.response.labelled_by(input_label.id);
            // Selecting the suggested name also pins it as an explicit choice.
            d.csv_picked |= menu.inner.unwrap_or(false);
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
        let response = ui
            .add(egui::TextEdit::singleline(&mut app.dialogs.text).hint_text("EPSG:4326").desired_width(240.0))
            .labelled_by(input_label.id);
        app.dialogs.csv_crs_dirty |= response.changed();
        if response.lost_focus() && app.dialogs.csv_crs_dirty {
            app.dialogs.csv_crs_dirty = false;
            crs_committed = true;
        }
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
    // Only untouched defaults follow a committed CRS edit (Enter also ends a single-line edit).
    // Parsing a PROJ string registers it, so never parse while the user is still typing.
    // Another separator keeps explicit picks when their columns still exist.
    let crs = app.dialogs.text.trim();
    if (reread || (crs_committed && !app.dialogs.csv_picked))
        && let Some(file) = &app.dialogs.csv
    {
        let found = csv_columns(file, app.dialogs.mode, crs);
        let picked = [app.dialogs.a.clone(), app.dialogs.b.clone(), app.dialogs.c.clone()];
        show_columns(&mut app.dialogs, found);
        if reread && app.dialogs.csv_picked {
            keep_picked(&mut app.dialogs, picked);
        }
    }
    buttons(app, ui, "Add", |app| {
        let mut p = json!({});
        if app.dialogs.csv_picked {
            for (key, v) in [("x", &app.dialogs.a), ("y", &app.dialogs.b), ("wkt", &app.dialogs.c)] {
                if !v.trim().is_empty() {
                    p[key] = json!(v.trim());
                }
            }
        }
        if !app.dialogs.text.trim().is_empty() {
            p["crs"] = json!(app.dialogs.text.trim());
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
    let connection = app.ui.service.as_ref().and_then(|source| source.connection.clone());
    match app.run("ui.addService", json!({"url": url, "type": kind, "connection": connection})) {
        Ok(_) => app.ui.service_browse.back = way,
        Err(e) => {
            app.ui.service_browse.back = before;
            app.dialogs.error = Some(e);
        }
    }
}

/// How many of a service's layers Add From Service lists.
const SHOWN_LAYERS: usize = 500;

/// How long typing pauses before Add From Service searches all of a listing that stops short:
/// reading the documents again takes a moment for a large one (IBGE's WFS, 12 MB), so not on
/// every key (#1577).
pub(crate) const SEARCH_PAUSE: web_time::Duration = web_time::Duration::from_millis(300);

/// A filter's words as `service.layers` takes them: lower case, one space apart.
fn filter_key(filter: &str) -> String {
    filter.split_whitespace().map(str::to_lowercase).collect::<Vec<_>>().join(" ")
}

/// The search of all of a service's layers with the filter, when its listing stops short of them
/// (`listedNote`) and typing has paused for [`SEARCH_PAUSE`]: `service.layers` again on the
/// documents the dialog keeps, with `filter`, so nothing is downloaded. A listing that holds them
/// all is filtered in the dialog, as before. Gives how long until a search is due, to draw again
/// then. On the desktop, admitted read-ahead binds the saved login here and parses off the
/// frame, as for document arrivals (`poll_service`). The result retains its original filter;
/// continued typing cannot replace a newer filter's rows with an earlier search.
fn search_all(app: &mut RustGisApp) -> Option<web_time::Duration> {
    let conn = app.ui.service.as_mut()?;
    let key = filter_key(&app.ui.service_browse.filter);
    let short = conn.listing.as_ref().is_some_and(|v| v.get("listedNote").is_some());
    if !short || key.is_empty() || conn.pending.is_some() || conn.search.as_ref().is_some_and(|s| s.filter == key) {
        return None;
    }
    let now = web_time::Instant::now();
    match &conn.typed {
        Some((typed, at)) if *typed == key => {
            let waited = now.duration_since(*at);
            if waited < SEARCH_PAUSE {
                return Some(SEARCH_PAUSE - waited);
            }
        }
        _ => {
            conn.typed = Some((key, now));
            return Some(SEARCH_PAUSE);
        }
    }
    let mut p = json!({"url": conn.url, "text": conn.text, "documents": conn.documents, "filter": key});
    if conn.kind != "auto" {
        p["type"] = json!(conn.kind);
    }
    if let Some(connection) = &conn.connection {
        p["connection"] = json!(connection);
    }
    #[cfg(not(target_arch = "wasm32"))]
    if app.reads_off_frame {
        match crate::serviceui::ListingRead::start(&mut app.session, &p) {
            Ok(reading) => {
                conn.reading = Some(reading);
                conn.pending = Some(conn.url.clone());
            }
            Err(error) => conn.search = Some(crate::ServiceSearch { filter: key, listing: Err(error) }),
        }
        return None;
    }
    // Not `app.run`: an error belongs in the dialog, as the listing's does.
    let listing = app.session.run("service.layers", &p);
    conn.search = Some(crate::ServiceSearch { filter: key, listing });
    conn.added = None;
    None
}

/// The search's answer for the filter as typed now, when there is one and it holds layers.
fn searched<'a>(conn: &'a crate::ServiceConn, filter: &str) -> Option<&'a Value> {
    conn.search.as_ref().filter(|s| s.filter == filter_key(filter)).and_then(|s| s.listing.as_ref().ok())
}

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
    // Every layer matching the filter, when all of a listing that stops short were searched: its
    // answer holds only matches (the first 2000), and counts them all.
    let search = searched(conn, filter);
    let was_searched = search.is_some();
    let listing = search.or(conn.listing.as_ref());
    let layers: &[Value] = listing.and_then(|v| v.get("layers")).and_then(Value::as_array).map_or(&[], Vec::as_slice);
    let (matched, shown): (usize, Vec<usize>) = match search {
        Some(v) => {
            (v.get("matched").and_then(Value::as_u64).map_or(layers.len(), |m| m as usize), (0..layers.len().min(SHOWN_LAYERS)).collect())
        }
        None => {
            let matching = matching_rows(layers, filter);
            (matching.len(), matching.into_iter().take(SHOWN_LAYERS).map(|(i, _)| i).collect())
        }
    };
    let removals = rustgis_engine::cmd::service_added::added(app.session.map(), layers, &shown);
    let rows = shown.into_iter().map(|i| (i, removals.get(i).cloned().flatten())).collect();
    conn.added = Some(crate::ServiceAdded { revision, map, filter: filter.clone(), matched, searched: was_searched, rows });
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
        // The address as typed: one read through a login is matched and kept without its binding.
        let url = rustgis_engine::data::login_address::inner(url);
        crate::connectionui::picked(&list, app.dialogs.connection.as_deref(), url).is_none().then(|| (url.to_string(), kind))
    });
    let mut keep_root = false;
    if let Some(due) = search_all(app) {
        ui.ctx().request_repaint_after(due);
    }
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
            // A listing that stops short counts them all, and says so here in the dialog's own
            // words instead of its note: the filter searches all of them (`search_all`).
            let listed_note = v.get("listedNote").and_then(Value::as_str);
            let count = crate::connectionui::layer_count(v);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(crate::i18n::fmt(
                        "{kind} {title}: {count} layers",
                        &[("kind", kind), ("title", title), ("count", &count)],
                    ))
                    .strong(),
                );
                if keepable.is_some() && conn.pending.is_none() && conn.error.is_none() && conn.failure.is_none() {
                    keep_root = crate::connectionui::keep_button(ui);
                }
            });
            for n in v.get("notes").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
                if Some(n) != listed_note {
                    ui.label(egui::RichText::new(n).small().weak());
                }
            }
            // The filter searches all of them, once typing pauses (`search_all`).
            if listed_note.is_some() {
                let key = filter_key(&app.ui.service_browse.filter);
                let line = if key.is_empty() {
                    Some(crate::i18n::fmt(
                        "The first {n} of them are listed; the filter searches all of them.",
                        &[("n", &layers.len().to_string())],
                    ))
                } else {
                    match conn.search.as_ref().filter(|s| s.filter == key).map(|s| &s.listing) {
                        None => {
                            let total = v.get("total").and_then(Value::as_u64).map_or(layers.len().to_string(), |n| n.to_string());
                            Some(crate::i18n::fmt("Searching all {total} layers…", &[("total", &total)]))
                        }
                        Some(Err(e)) => {
                            ui.colored_label(t.red, e);
                            None
                        }
                        // The search's own notes, beside the listing's: more matches than it
                        // lists (its `listedNote`), or a list cut by its addresses' length.
                        Some(Ok(found)) => {
                            let shown: Vec<&str> =
                                v.get("notes").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
                            for n in found.get("notes").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
                                if !shown.contains(&n) {
                                    ui.label(egui::RichText::new(n).small().weak());
                                }
                            }
                            None
                        }
                    }
                };
                if let Some(line) = line {
                    ui.label(egui::RichText::new(line).small().weak());
                }
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
            // A search's rows are places in its answer, which holds only matches.
            let row_layers = match added.filter(|a| a.searched) {
                Some(_) => {
                    searched(conn, &app.ui.service_browse.filter).and_then(|v| v.get("layers")).and_then(Value::as_array).unwrap_or(&none)
                }
                None => layers,
            };
            if let Some(a) = added.filter(|a| a.matched > SHOWN_LAYERS) {
                ui.label(
                    egui::RichText::new(crate::i18n::fmt(
                        "{n} of {m} are listed.",
                        &[("n", &SHOWN_LAYERS.to_string()), ("m", &a.matched.to_string())],
                    ))
                    .small()
                    .weak(),
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
                            let Some(l) = row_layers.get(*i) else { continue };
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
                if let Some(reading) = &conn.reading
                    && ui.button(tl!("Cancel")).clicked()
                {
                    reading.control.cancel();
                }
            });
        }
    }
    if keep_root && let Some((url, kind)) = keepable {
        // "auto" is the dialog's detection mode; saved connections call that kind "service".
        match crate::connectionui::keep(app, json!({"kind": if kind == "auto" { "service" } else { kind }, "address": url})) {
            Ok(id) => {
                app.dialogs.connection = Some(id.clone());
                if let Some(source) = &mut app.ui.service {
                    source.connection = Some(id);
                }
            }
            Err(e) => app.dialogs.error = Some(e),
        }
    }
    if let Some(mut p) = import {
        let connection = app.ui.service.as_ref().and_then(|source| source.connection.clone());
        let url = p.get("url").and_then(Value::as_str).unwrap_or("");
        if let Some(id) = crate::serviceui::connection_for(app, connection.as_deref(), url) {
            p["connection"] = json!(id);
        }
        if let Err(e) = app.run("ui.importService", p) {
            app.dialogs.error = Some(e);
        }
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
        // Features are downloaded in the background, as From URL… does; map layers are added now.
        let time = crate::webdataui::fixed_or_slider(app.ui.service_browse.fixed, &app.ui.service_browse.time);
        // The view's area, for a feature layer only: a map layer never asks for one, so an
        // unknown view can't stop it.
        let features = l["offer"]["type"] == "features";
        let area =
            (features && app.ui.service_browse.area).then(|| rustgis_engine::cmd::catalogs::area(&app.session, &Value::Null)).transpose();
        // A new add's line replaces the last one's, error included (#1453): a download still on its
        // way then lands without it, so it can't hide this add's error.
        app.dialogs.info = None;
        app.dialogs.error = None;
        match area.and_then(|area| crate::serviceui::add_layer(app, &src, &l, &time, area, true)) {
            Ok(line) => app.dialogs.info = Some(line),
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
    buttons_enabled(app, ui, ok, true, f)
}

/// OK/Cancel row whose submit can be disabled without disabling Cancel.
pub(crate) fn buttons_enabled(
    app: &mut RustGisApp,
    ui: &mut egui::Ui,
    ok: &str,
    enabled: bool,
    f: impl FnOnce(&mut RustGisApp) -> Result<(), String>,
) -> bool {
    let mut close = false;
    ui.add_space(4.0);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
        let t = Tokens::get(ui.ctx());
        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift);
        let submit = ui.add_enabled_ui(enabled, |ui| crate::widgets::dialog_button(ui, tl!(ok), Some(t.accent), vec2(84.0, 28.0))).inner;
        if enabled && (submit.clicked() || enter) {
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

/// Adds `word` to the end of the clause `text`, set apart from what the clause ends with, so a
/// name typed just before isn't run into it (`nameLIKE`), as the buttons beside a clause do.
pub(crate) fn add_word(text: &mut String, word: &str) {
    if !text.is_empty() && !text.ends_with(char::is_whitespace) && !text.ends_with('(') {
        text.push(' ');
    }
    text.push_str(word);
    // A call left open (`round(`) goes on with its arguments.
    if !word.ends_with('(') {
        text.push(' ');
    }
}

/// The most fields a clause's buttons offer (a wider table is typed).
const MAX_FIELD_CHIPS: usize = 40;

/// Buttons that add one of `fields` (the first [`MAX_FIELD_CHIPS`]), as the expression names it
/// (`"Pop Est"` in quotes), an operator, a function or a variable to the clause `text`: the one
/// row Select By Attributes, the Layer Properties pane's Query tab and a label class's SQL query
/// offer. A field an earlier one shadows (`name` after `Name`: the expression reads the first,
/// letter case aside) gets no button, as the completion list doesn't offer it.
pub(crate) fn field_chips(ui: &mut egui::Ui, fields: &[rustgis_engine::data::Field], text: &mut String) {
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new(tl!("Fields:")).small().weak());
        let mut seen = std::collections::HashSet::new();
        for f in fields.iter().filter(|f| seen.insert(f.name.to_ascii_lowercase())).take(MAX_FIELD_CHIPS) {
            if ui.small_button(&f.name).clicked() {
                add_word(text, &crate::exprcomplete::field_ref(&f.name));
            }
        }
    });
    // Wrapped, as the fields are: a narrow pane (the Query tab) would cut the last ones off.
    ui.horizontal_wrapped(|ui| {
        for op in ["=", "<>", ">", "<", ">=", "<=", "LIKE", "IN", "IS NULL", "AND", "OR", "NOT"] {
            if ui.small_button(op).clicked() {
                add_word(text, op);
            }
        }
        if let Some(word) = crate::exprcomplete::menus(ui) {
            add_word(text, &word);
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
    let fields = app.session.map().layer_by_name(&app.dialogs.layer).map(|l| l.dataset.fields.as_slice()).unwrap_or_default();
    field_chips(ui, fields, &mut app.dialogs.text);
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
    let dialog = crate::filedialog::FileDialog::new();
    let dialog = if exts.is_empty() { dialog } else { dialog.add_filter(label, exts) };
    dialog.add_filter("All files", &["*"]).pick_file().map(|p| p.display().to_string())
}

#[cfg(not(target_arch = "wasm32"))]
pub fn pick_folder() -> Option<String> {
    crate::filedialog::FileDialog::new().pick_folder().map(|p| p.display().to_string())
}

/// Asks where to save, suggesting file `name`, with the system's dialog; `None` when cancelled.
#[cfg(all(not(target_arch = "wasm32"), not(test)))]
pub fn pick_save(name: &str, exts: &[&str], label: &str) -> Option<String> {
    crate::filedialog::FileDialog::new().set_file_name(name).add_filter(label, exts).save_file().map(|p| p.display().to_string())
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
#[expect(dead_code, reason = "the browser has no folder picker; kept beside the desktop one")]
pub fn pick_folder() -> Option<String> {
    None
}

#[cfg(target_arch = "wasm32")]
pub fn pick_save(_: &str, _: &[&str], _: &str) -> Option<String> {
    None
}

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

/// A scale box's text as the command reads it: commas that group thousands (`50,000`,
/// `1,000,000.5`) taken out, any other comma (a decimal comma, `1,5`) left in, so the command
/// refuses it rather than reading `15`.
fn scale_text(text: &str) -> String {
    let text = text.trim();
    let whole = text.split_once('.').map_or(text, |(w, _)| w);
    let grouped = whole.contains(',') && {
        let mut groups = whole.split(',');
        let first = groups.next().unwrap_or("");
        (1..=3).contains(&first.len())
            && first.chars().all(|c| c.is_ascii_digit())
            && groups.all(|g| g.len() == 3 && g.chars().all(|c| c.is_ascii_digit()))
    };
    let decimals = text.split_once('.').is_none_or(|(_, d)| !d.contains(','));
    if grouped && decimals { text.replace(',', "") } else { text.to_string() }
}

/// The columns a name-based edit can reach. Keep both the initial choice and the picker in
/// step with the command's case-insensitive lookup, including duplicate names in imported data.
fn calculation_fields(dataset: &rustgis_engine::data::Dataset) -> impl Iterator<Item = &rustgis_engine::data::Field> {
    dataset.fields.iter().enumerate().filter(|(i, field)| dataset.field_index(&field.name) == Some(*i)).map(|(_, field)| field)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A clause's buttons add a field as the expression names it (`"Pop Est"` in quotes, a plain
    /// name as it is), and every operator, `>=` and `NOT` among them; at most `MAX_FIELD_CHIPS`
    /// fields are offered, and none an earlier one shadows (`POP` after `pop`), which takes no
    /// place among them.
    #[test]
    fn the_buttons_add_a_field_as_the_expression_names_it() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        use rustgis_engine::data::{Field, FieldType};
        let mut fields =
            vec![Field::new("Pop Est", FieldType::Integer), Field::new("pop", FieldType::Integer), Field::new("POP", FieldType::Integer)];
        fields.extend((0..MAX_FIELD_CHIPS).map(|i| Field::new(format!("f{i}"), FieldType::Integer)));
        // Without the interface fonts (`theme::install_fonts`) egui's faces have no "▾".
        let mut h = egui_kittest::Harness::builder()
            .allow_missing_glyphs()
            .build_ui_state(move |ui, text: &mut String| field_chips(ui, &fields, text), String::new());
        h.run();
        for button in ["Pop Est", ">=", "pop", "AND", "NOT", "IN"] {
            h.get_by_role_and_label(Role::Button, button).click();
            h.run();
        }
        assert_eq!(h.state(), "\"Pop Est\" >= pop AND NOT IN ");
        assert!(h.query_by_role_and_label(Role::Button, "POP").is_none(), "a shadowed field");
        let last = format!("f{}", MAX_FIELD_CHIPS - 3);
        assert!(h.query_by_role_and_label(Role::Button, &last).is_some(), "the first {MAX_FIELD_CHIPS} fields");
        let past = format!("f{}", MAX_FIELD_CHIPS - 2);
        assert!(h.query_by_role_and_label(Role::Button, &past).is_none(), "no more");
    }

    /// The Functions and Variables menus are lists as a box's suggestions are: each entry named
    /// as the list names it, the first highlighted with its help beside (a function's description
    /// and example), the help following the pointer; a click adds the entry (a function open),
    /// ↓ and Enter add the next one, Escape closes the list.
    #[test]
    fn the_menus_are_lists_with_their_help() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        use rustgis_engine::data::query;
        let fs: Vec<_> = query::functions().take(2).collect();
        let (first, second) = (fs[0], fs[1]);
        let name = |f: query::FuncSpec| format!("{}()", f.name);
        let open = |f: query::FuncSpec| if f.max > 0 { format!("{}(", f.name) } else { format!("{}() ", f.name) };
        let var = *query::VARS[1].names.first().unwrap();
        let mut h = egui_kittest::Harness::builder()
            .with_size(vec2(1000.0, 700.0))
            .allow_missing_glyphs()
            .build_ui_state(move |ui, text: &mut String| field_chips(ui, &[], text), String::new());
        h.run();
        let shows = |h: &egui_kittest::Harness<'_, String>, label: &str| h.query_by_role_and_label(Role::Label, label).is_some();
        h.get_by_role_and_label(Role::Button, "Functions ▾").click();
        h.run();
        assert!(h.query_by_role_and_label(Role::Button, &name(first)).is_some(), "named as the list names it");
        assert!(shows(&h, first.description) && shows(&h, first.example), "the first highlighted, its help shown");
        // Moved onto it: a first position has no motion (egui's pointer delta), so it starts beside.
        let at = h.get_by_role_and_label(Role::Button, &name(second)).rect().center();
        for at in [at - vec2(40.0, 0.0), at] {
            h.event(egui::Event::PointerMoved(at));
            h.run();
        }
        assert!(shows(&h, second.description) && shows(&h, second.example), "the help follows the pointer");
        assert!(!shows(&h, first.description), "one help at a time");
        h.get_by_role_and_label(Role::Button, &name(second)).click();
        h.run();
        assert_eq!(*h.state(), open(second), "added, a function open");
        assert!(h.query_by_role_and_label(Role::Button, &name(second)).is_none(), "the list closed");
        h.get_by_role_and_label(Role::Button, "Variables ▾").click();
        h.run();
        h.key_press(egui::Key::ArrowDown);
        h.run();
        assert!(shows(&h, query::VARS[1].description), "the help follows the highlight");
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(*h.state(), format!("{}{var} ", open(second)), "↓ and Enter add the next");
        h.get_by_role_and_label(Role::Button, "Functions ▾").click();
        h.run();
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(h.query_by_role_and_label(Role::Button, &name(first)).is_none(), "Escape closes it");
        assert_eq!(*h.state(), format!("{}{var} ", open(second)), "adding nothing");
    }

    /// From the keyboard: Tab to Functions ▾ and Enter opens it, ↓ moves the highlight and Enter
    /// adds that function, the focus staying on the button (the arrows don't move it to another
    /// button for Enter to press); Escape closes the list.
    #[test]
    fn the_menus_work_from_the_keyboard() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        use rustgis_engine::data::query;
        let fs: Vec<_> = query::functions().take(2).collect();
        let (first, second) = (format!("{}()", fs[0].name), fs[1]);
        let mut h = egui_kittest::Harness::builder()
            .with_size(vec2(1000.0, 700.0))
            .allow_missing_glyphs()
            .build_ui_state(move |ui, text: &mut String| field_chips(ui, &[], text), String::new());
        h.run();
        let on_button = |h: &egui_kittest::Harness<'_, String>| h.get_by_role_and_label(Role::Button, "Functions ▾").is_focused();
        for _ in 0..20 {
            if on_button(&h) {
                break;
            }
            h.key_press(egui::Key::Tab);
            h.run();
        }
        assert!(on_button(&h), "reached with Tab");
        h.key_press(egui::Key::Enter);
        h.run();
        assert!(h.query_by_role_and_label(Role::Button, &first).is_some(), "Enter opens it");
        assert_eq!(*h.state(), "", "and adds nothing yet");
        h.key_press(egui::Key::ArrowDown);
        h.run();
        assert!(on_button(&h), "the arrow moves the highlight, not the focus");
        h.key_press(egui::Key::Enter);
        h.run();
        let added = if second.max > 0 { format!("{}(", second.name) } else { format!("{}() ", second.name) };
        assert_eq!(*h.state(), added, "Enter adds the highlighted one");
        assert!(h.query_by_role_and_label(Role::Button, &first).is_none(), "and closes the list");
        h.key_press(egui::Key::Enter);
        h.run();
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(h.query_by_role_and_label(Role::Button, &first).is_none(), "Escape closes it");
        assert_eq!(*h.state(), added, "adding nothing");
        // Tab on with the list open: it closes at once, and the very next Enter (the next frame)
        // is the next widget's, Variables ▾, rather than the list's.
        h.key_press(egui::Key::Enter);
        h.run();
        assert!(h.query_by_role_and_label(Role::Button, &first).is_some(), "open again");
        h.key_press(egui::Key::Tab);
        h.step();
        h.key_press(egui::Key::Enter);
        h.step();
        h.run();
        assert!(!on_button(&h), "the focus moved on");
        assert!(h.query_by_role_and_label(Role::Button, &first).is_none(), "Tab closed it");
        let var = rustgis_engine::data::query::VARS[0].names[0];
        assert!(h.query_by_role_and_label(Role::Button, var).is_some(), "Enter opened Variables ▾");
        assert_eq!(*h.state(), added, "and added nothing");
    }

    /// In the app, Enter on Functions ▾ (focused from the keyboard) opens its list and is taken
    /// by it: the dialog doesn't also run with the clause half written.
    #[test]
    fn enter_on_a_clause_menu_does_not_run_the_dialog() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        let mut app = RustGisApp::with_sample();
        app.run("ui.dialog", json!({"name": "selectByAttributes"})).unwrap();
        app.dialogs.text = "pop_est >".into();
        let mut h = app_harness(app);
        h.run_steps(3);
        h.get_by_role_and_label(Role::Button, "Functions ▾").focus();
        h.run();
        h.key_press(egui::Key::Enter);
        h.run();
        let first = format!("{}()", rustgis_engine::data::query::functions().next().unwrap().name);
        assert!(h.query_by_role_and_label(Role::Button, &first).is_some(), "the list opened");
        assert_eq!(h.state().dialogs.open, Some("selectByAttributes"), "the dialog is still open");
        assert_eq!(h.state().dialogs.error, None, "and didn't run");
    }

    /// In the app (its whole frame, shortcuts included), Escape closes a clause's Functions list:
    /// the Escape shortcut (the Explore tool) doesn't take it first, so the tool is left as it was.
    #[test]
    fn escape_closes_a_clause_list_in_the_app() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        let mut app = RustGisApp::with_sample();
        app.run("tool.set", json!({"tool": "zoomIn"})).unwrap();
        app.run("ui.dialog", json!({"name": "selectByAttributes"})).unwrap();
        app.dialogs.text = "pop_est > 1".into();
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1400.0, 1000.0)).with_max_steps(64).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app,
        );
        h.run_steps(4);
        // Typing in the clause first: a click on the button doesn't leave the box's focus behind
        // to close the list it opens.
        h.query_all_by_role(Role::MultilineTextInput).find(|n| n.value().as_deref() == Some("pop_est > 1")).unwrap().click();
        h.run_steps(2);
        assert!(
            h.query_all_by_role(Role::MultilineTextInput).any(|n| n.value().as_deref() == Some("pop_est > 1") && n.is_focused()),
            "the clause has the focus"
        );
        h.get_by_role_and_label(Role::Button, "Functions ▾").click();
        h.run_steps(2);
        let first = format!("{}()", rustgis_engine::data::query::functions().next().unwrap().name);
        assert!(h.query_by_role_and_label(Role::Button, &first).is_some(), "open, and still open a frame later");
        // A shortcut with Ctrl (⌘) isn't the list's: it still runs (Mod+Minus zooms out).
        let res = h.state().session.map().camera.res;
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Minus);
        h.run_steps(2);
        assert!(h.state().session.map().camera.res > res, "zoomed out");
        assert!(h.query_by_role_and_label(Role::Button, &first).is_some(), "with the list still open");
        h.key_press(egui::Key::Escape);
        h.run_steps(2);
        assert!(h.query_by_role_and_label(Role::Button, &first).is_none(), "Escape closed it");
        assert_eq!(h.state().dialogs.open, Some("selectByAttributes"), "not the dialog");
        assert_eq!(h.state().session.tool, rustgis_engine::Tool::ZoomIn, "nor switched the tool");
    }

    /// A scale's commas go only where they group thousands: a decimal comma is left for the
    /// command to refuse, not read as a bigger number.
    #[test]
    fn a_scale_box_takes_commas_only_between_thousands() {
        for (typed, sent) in [
            ("50,000", "50000"),
            (" 1,000,000 ", "1000000"),
            ("1,000,000.5", "1000000.5"),
            ("50000", "50000"),
            ("1,5", "1,5"),
            ("50,00", "50,00"),
            ("1234,567", "1234,567"),
            (",500", ",500"),
            ("1,000.5,5", "1,000.5,5"),
            ("", ""),
        ] {
            assert_eq!(scale_text(typed), sent, "{typed}");
        }
        let mut s = rustgis_engine::Session::new(rustgis_engine::Project::default());
        let refused = s.run("map.goToXY", &json!({"lon": 0, "lat": 0, "scale": scale_text("1,5")})).unwrap_err();
        assert!(refused.contains("scale must be a positive number"), "{refused}");
    }

    /// A field dialog opened on a standalone table acts on that table, and on no other once
    /// another project is open: table ids start again in each project.
    #[test]
    fn a_table_dialog_refuses_another_project() {
        let mut app = RustGisApp::with_sample();
        let csv = json!({"fileName": "sites.csv", "text": "id,street\n1,Rue Cler\n"});
        app.run("layer.addDataText", csv.clone()).unwrap();
        let id = app.session.project.tables[0].id;
        app.run("ui.dialog", json!({"name": "addField", "table": id})).unwrap();
        let p = app.dialogs.on_table(&app.session, json!({"name": "note"})).unwrap();
        assert_eq!(p["table"], json!(id));
        app.run("project.new", json!({})).unwrap();
        app.run("layer.addDataText", csv).unwrap();
        assert_eq!(app.session.project.tables[0].id, id, "the new project's table has the same id");
        assert!(app.dialogs.on_table(&app.session, json!({"name": "note"})).is_err());
    }

    /// A case-shadowed field cannot be selected or submitted as another column by the dialog.
    #[test]
    fn table_calculation_dialog_refuses_a_shadowed_field() {
        use egui_kittest::kittest::Queryable;
        let mut app = RustGisApp::new(Session::new(rustgis_engine::Project::default()));
        app.run("layer.addDataText", json!({"fileName": "names.csv", "text": "Name,name\nfirst,second\n"})).unwrap();
        app.run("edit.enable", json!({"on": true})).unwrap();
        let id = app.session.project.tables[0].id;
        app.run("ui.dialog", json!({"name": "calculateField", "table": id})).unwrap();
        let mut h = app_harness(app);
        h.run_steps(3);
        // A stale choice (for example after a schema change) must not reach the first column.
        h.state_mut().dialogs.a = "name".into();
        h.state_mut().dialogs.text = "'changed'".into();
        h.run_steps(1);
        h.get_by_label("Calculate").click();
        h.run_steps(2);
        assert_eq!(h.state().dialogs.open, Some("calculateField"));
        assert!(h.state().dialogs.error.is_some());
        let ds = &h.state().session.project.tables[0].dataset;
        assert_eq!(ds.value(0, 0), &rustgis_engine::data::Value::Text("first".into()));
        assert_eq!(ds.value(0, 1), &rustgis_engine::data::Value::Text("second".into()));
        // The reachable first name is still offered and works.
        h.get_by_role(egui::accesskit::Role::ComboBox).click();
        h.run_steps(2);
        assert!(h.query_by_label("name").is_none(), "the shadowed option is not offered");
        h.get_by_label("Name").click();
        h.run_steps(2);
        h.get_by_label("Calculate").click();
        h.run_steps(2);
        assert_eq!(h.state().dialogs.open, None);
        let ds = &h.state().session.project.tables[0].dataset;
        assert_eq!(ds.value(0, 0), &rustgis_engine::data::Value::Text("changed".into()));
        assert_eq!(ds.value(0, 1), &rustgis_engine::data::Value::Text("second".into()));
    }

    /// Calculate Field suggests as the expression is typed (#1240): Enter takes the highlighted
    /// field, quoted as the expression needs, and doesn't calculate; Escape closes the list, or
    /// a call's signature, and leaves the dialog and the text as they are.
    #[test]
    fn calculate_field_suggests_the_fields_as_its_expression_is_typed() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        let mut app = RustGisApp::new(Session::new(rustgis_engine::Project::default()));
        app.run("layer.addDataText", json!({"fileName": "pops.csv", "text": "Pop Est,name\n1,a\n"})).unwrap();
        app.run("edit.enable", json!({"on": true})).unwrap();
        let id = app.session.project.tables[0].id;
        app.run("ui.dialog", json!({"name": "calculateField", "table": id})).unwrap();
        let mut h = app_harness(app);
        h.run_steps(3);
        h.ctx.memory_mut(|m| m.request_focus(egui::Id::unique("cf_expression")));
        h.run();
        h.event(egui::Event::Text("po".into()));
        h.run();
        assert!(h.query_by_role_and_label(Role::Button, "\"Pop Est\"").is_some(), "the field offered, as written");
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(h.state().dialogs.text, "\"Pop Est\"");
        assert_eq!(h.state().dialogs.open, Some("calculateField"), "Enter took the field, not Calculate");
        h.event(egui::Event::Text(" * ro".into()));
        h.run();
        assert!(h.query_by_role_and_label(Role::Button, "round()").is_some(), "the function offered");
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(h.query_by_role_and_label(Role::Button, "round()").is_none(), "the list closed");
        assert_eq!(h.state().dialogs.text, "\"Pop Est\" * ro");
        assert_eq!(h.state().dialogs.open, Some("calculateField"));
        // In a call its signature shows; Escape closes it, not the dialog.
        h.event(egui::Event::Text(" + round(".into()));
        h.run();
        assert!(h.query_by_role_and_label(Role::Label, "round(x[, places])").is_some(), "the signature shown");
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(h.query_by_role_and_label(Role::Label, "round(x[, places])").is_none(), "the signature closed");
        assert_eq!(h.state().dialogs.open, Some("calculateField"), "Escape took the signature, not the dialog");
    }

    /// A word still being chosen from the suggestions isn't called invalid: `pop_` (with
    /// `pop_est` offered) shows no error; `pop_x`, which nothing matches, does.
    #[test]
    fn calculate_field_reads_the_expression_once_no_suggestion_is_offered() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        let mut app = RustGisApp::new(Session::new(rustgis_engine::Project::default()));
        app.run("layer.addDataText", json!({"fileName": "pops.csv", "text": "pop_est,name\n1,a\n"})).unwrap();
        app.run("edit.enable", json!({"on": true})).unwrap();
        let id = app.session.project.tables[0].id;
        app.run("ui.dialog", json!({"name": "calculateField", "table": id})).unwrap();
        let mut h = app_harness(app);
        h.run_steps(3);
        h.ctx.memory_mut(|m| m.request_focus(egui::Id::unique("cf_expression")));
        h.run();
        // Counted, as the Field box may show `pop_est` too.
        let shown = |h: &egui_kittest::Harness<'static, RustGisApp>| h.query_all_by_role_and_label(Role::Button, "pop_est").count();
        h.event(egui::Event::Text("pop_".into()));
        h.run();
        let offered = shown(&h);
        assert!(h.query_by_label_contains("Invalid:").is_none(), "no error while it is offered");
        h.event(egui::Event::Text("x".into()));
        h.run();
        assert_eq!(shown(&h) + 1, offered, "the field was offered, and no longer is");
        assert!(h.query_by_label_contains("no field named 'pop_x'").is_some(), "the error once nothing is");
        // Offered again, then closed with Escape: the word is read as it is, and the dialog stays.
        h.key_press(egui::Key::Backspace);
        h.run();
        assert_eq!(shown(&h), offered, "offered again");
        assert!(h.query_by_label_contains("Invalid:").is_none(), "no error while it is offered");
        h.key_press(egui::Key::Escape);
        h.run();
        assert_eq!(shown(&h) + 1, offered, "the list closed");
        assert!(h.query_by_label_contains("no field named 'pop_'").is_some(), "the error once the list is closed");
        assert_eq!(h.state().dialogs.open, Some("calculateField"), "Escape took the list, not the dialog");
    }

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

    /// CRS typing leaves the column choices alone; committing updates defaults, while explicit
    /// X/Y and None selections survive both Enter and focus loss.
    #[test]
    fn delimited_columns_follow_committed_crs_without_replacing_picks() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        for pick in [None, Some("chosen_x"), Some("None")] {
            let mut app = RustGisApp::new(Session::new(rustgis_engine::Project::default()));
            open_csv(
                &mut app,
                "points.csv",
                CsvSource::Bytes(b"X,Y,Longitude,Latitude,chosen_x,chosen_y\n1000000,200000,-74,40,1000001,200001\n".to_vec()),
            )
            .unwrap();
            let mut h = app_harness(app);
            h.run_steps(3);
            assert_eq!((&*h.state().dialogs.a, &*h.state().dialogs.b), ("Longitude", "Latitude"));
            if let Some(pick) = pick {
                h.get_by_role_and_label(Role::ComboBox, "X column").click();
                h.run_steps(2);
                h.get_by_role_and_label(Role::Button, pick).click();
                h.run_steps(2);
                if pick == "chosen_x" {
                    h.get_by_role_and_label(Role::ComboBox, "Y column").click();
                    h.run_steps(2);
                    h.get_by_role_and_label(Role::Button, "chosen_y").click();
                    h.run_steps(2);
                }
            }
            let before = (h.state().dialogs.a.clone(), h.state().dialogs.b.clone());
            h.get_by_role_and_label(Role::TextInput, "Coordinate system").click();
            h.run_steps(2);
            h.event(egui::Event::Text("EPSG:2263".into()));
            h.run_steps(2);
            assert_eq!(h.state().dialogs.text, "EPSG:2263");
            assert_eq!((&h.state().dialogs.a, &h.state().dialogs.b), (&before.0, &before.1), "typing must not recompute defaults");
            h.key_press(if pick == Some("None") { egui::Key::Tab } else { egui::Key::Enter });
            h.run_steps(2);
            let expected = if pick.is_none() { ("X", "Y") } else { (before.0.as_str(), before.1.as_str()) };
            assert_eq!((&*h.state().dialogs.a, &*h.state().dialogs.b), expected, "pick {pick:?}");
            // Enter also confirms Add through the dialog's usual OK/Cancel row.
            assert_eq!(h.state().dialogs.open, if pick == Some("None") { Some("addCsv") } else { None });
            assert!(h.state().dialogs.error.is_none());
        }
    }

    /// The dialog previews the first thousand rows and only submits explicit column picks.
    #[test]
    fn delimited_suggestions_use_whole_file_inference_until_picked() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        let mut csv = String::from("Longitude,Latitude,X,Y\n");
        csv.push_str(&"-93,44,20,40\n".repeat(1_000));
        csv.push_str(",,21,41\n");
        for explicit in [false, true] {
            let mut app = RustGisApp::new(Session::new(rustgis_engine::Project::default()));
            open_csv(&mut app, "points.csv", CsvSource::Bytes(csv.as_bytes().to_vec())).unwrap();
            let mut h = app_harness(app);
            h.run_steps(3);
            assert_eq!((&*h.state().dialogs.a, &*h.state().dialogs.b), ("Longitude", "Latitude"));
            assert_eq!(h.get_by_role_and_label(Role::ComboBox, "X column").value().as_deref(), Some("Automatic"));
            if explicit {
                h.get_by_role_and_label(Role::ComboBox, "X column").click();
                h.run_steps(2);
                h.get_by_role_and_label(Role::Button, "Longitude").click();
                h.run_steps(2);
                assert_eq!(h.get_by_role_and_label(Role::ComboBox, "X column").value().as_deref(), Some("Longitude"));
            }
            h.get_by_label("Add").click();
            h.run_steps(3);
            assert!(h.state().dialogs.error.is_none(), "{:?}", h.state().dialogs.error);
            let features = &h.state().session.map().layers.last().unwrap().dataset.features;
            assert_eq!(features.len(), 1_001);
            assert_eq!(features.last().unwrap().geometry.is_some(), !explicit);
        }
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

    /// The Rename… dialog shows why a name over 200 characters isn't taken and stays open with
    /// it, the layer keeping its name; a name of 200 renames it and closes the dialog (#1817).
    #[test]
    fn the_rename_dialog_says_a_name_is_too_long() {
        use egui_kittest::kittest::Queryable;
        let mut app = RustGisApp::with_sample();
        let id = app.session.map().layers.first().map(|l| l.id).unwrap();
        app.run("layer.select", json!({"layer": id})).unwrap();
        let old = app.session.map().layer(id).unwrap().name.clone();
        app.run("ui.dialog", json!({"name": "renameLayer"})).unwrap();
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(900.0, 700.0)).build_ui_state(
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
        h.state_mut().dialogs.text = "é".repeat(201);
        h.run_steps(1);
        h.get_by_label("Rename").click();
        h.run_steps(2);
        let error = h.state().dialogs.error.clone().unwrap_or_default();
        assert!(error.contains("201 characters") && error.contains("at most 200"), "{error}");
        assert_eq!(h.state().dialogs.open, Some("renameLayer"), "stays open with the reason");
        assert_eq!(h.state().session.map().layer(id).unwrap().name, old);
        h.state_mut().dialogs.text = "é".repeat(200);
        h.run_steps(1);
        h.get_by_label("Rename").click();
        h.run_steps(2);
        assert_eq!(h.state().session.map().layer(id).unwrap().name, "é".repeat(200));
        assert_eq!(h.state().dialogs.open, None);
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
            connection: None,
            kind: "wms",
            slot: Default::default(),
            reading: None,
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
            typed: None,
            search: None,
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
            connection: None,
            kind: "wfs",
            slot: Default::default(),
            reading: None,
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
            typed: None,
            search: None,
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
            connection: None,
            kind: "wcs",
            slot: Default::default(),
            reading: None,
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
            typed: None,
            search: None,
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
            connection: None,
            kind: "ogcapi",
            slot: Default::default(),
            reading: None,
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
            typed: None,
            search: None,
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
