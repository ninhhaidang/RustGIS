//! Choose Layers (#934): a file holding several layers (a File Geodatabase, a GeoPackage, a DuckDB
//! database, a file GDAL reads) asks which to add when it is dropped on the window, picked with
//! Add Data or double-clicked in the Browser. Built on `info.fileLayers` (read off the frame) and
//! `layer.addData`'s `"layer"`; with Home › Add Data › Ask Which Layers to Add off, every layer is
//! added, as before. A file Add Data can link instead of copying asks which, at the foot of the
//! same window, or in it alone for a file of one layer (#2350).
use crate::RustGisApp;
use crate::theme::Tokens;
use serde_json::{Value, json};

/// A file whose layers are being listed, then chosen.
pub struct Chooser {
    path: String,
    /// The listing on its way (`info.fileLayers`, off the frame).
    listing: Option<crate::Slot<Result<Value, String>>>,
    /// Its layers, and whether each is ticked.
    layers: Vec<(String, bool)>,
    filter: String,
    /// Whether its layers are chosen (Ask Which Layers to Add on, and a file that lists them).
    pick: bool,
    /// Copy into the project or link to the file, for a file Add Data can link (`"link"`).
    link: Option<Link>,
    /// The project it was opened for (`Session::opened`): another project opened since drops it,
    /// so its layers never go to the wrong project.
    opened: u64,
    /// Set when it is dropped (answered, cancelled, closed, or its project replaced): the listing
    /// still on its way stops between a GeoPackage's tables.
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for Chooser {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Add Data's question for a file it can link instead of copying (#2350), from `info.fileLayers`'
/// `"link"`: `linked` starts as what Add Data does without asking, and `copy` says whether the
/// file can be read whole at all. `rule` is what Add Data does unasked: an answer left untouched
/// at it sends nothing, so each of a GeoPackage's tables keeps its own size rule; a click on either
/// answer (`answered`) is sent for every table.
struct Link {
    linked: bool,
    copy: bool,
    rule: bool,
    answered: bool,
}

/// Whether Add Data asks whether to copy a file into the project or link to it: the formats it
/// reads in place on the desktop (`info.fileLayers` says how, [`Link`]). A `.json` may be a city
/// model, and LiDAR is always linked: neither asks. The browser has no paths to link.
fn links(path: &str) -> bool {
    let lower = path.trim().to_ascii_lowercase();
    !cfg!(target_arch = "wasm32") && [".geojson", ".shp", ".gpkg", ".parquet", ".geoparquet", ".fgb"].iter().any(|e| lower.ends_with(e))
}

/// Whether a file lists its layers without reading their features, so it can ask which to add:
/// a File Geodatabase (or one of its tables), a GeoPackage, a DuckDB database or a file GDAL reads.
/// Files read whole (KML, GPX, zips), those that always hold one layer (GeoParquet, FlatGeobuf,
/// LAS/LAZ) and those whose "layer" means something else (a DXF's drawing layers, a spreadsheet's
/// sheet) add as before.
pub fn lists_layers(path: &str) -> bool {
    if cfg!(target_arch = "wasm32") {
        return false;
    }
    let lower = path.trim().trim_end_matches(['/', '\\']).to_ascii_lowercase();
    if lower.ends_with(".gdb") || lower.ends_with(".gdbtable") {
        return true;
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use rustgis_engine::data::source;
        if source::duckdb::is_duckdb_name(&lower) || source::gdal::is_gdal_name(&lower) {
            return true;
        }
    }
    // Of the formats read in place, only a GeoPackage holds several layers.
    lower.ends_with(".gpkg")
}

/// Adds a file the user dropped, picked or double-clicked: a project opens, and a file with
/// several layers asks which first (when the setting says to).
pub fn add_file(app: &mut RustGisApp, path: &str) -> Result<Value, String> {
    // Opening a project replaces this one: asked about first when it has unsaved changes. Only
    // the interface comes here (drops, Add Data, the Browser pane, `ui.addFile`).
    let command = rustgis_engine::project::open_command(path);
    if command != "layer.addData" {
        return app.run_by_user(command, json!({"path": path}));
    }
    // An ArcGIS Pro database connection file opens the geodatabase connection form, filled from it.
    if path.trim().to_ascii_lowercase().ends_with(".sde") {
        return app.run("ui.connection", json!({"kind": "sqlserver", "form": true, "sdeFile": path}));
    }
    // A drawing asks for its coordinate system and whether its blocks are drawn first.
    if rustgis_engine::data::dxf::is_dxf_name(path) {
        return crate::dialogs::open_dxf_path(app, path);
    }
    let pick = app.ui.ask_layers && lists_layers(path);
    if !pick && !links(path) {
        return add(app, json!({"path": path}));
    }
    // Listed off the frame: a geodatabase's or GDAL's listing can take a while.
    let slot: crate::Slot<Result<Value, String>> = crate::Slot::default();
    let out = slot.clone();
    let listed = path.to_string();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stopped = stop.clone();
    crate::work_off_frame("rustgis-list-layers", &slot, move || {
        // A chooser cancelled or closed stops it between a GeoPackage's tables.
        let _stop = rustgis_engine::data::progress::also_stop_when(stopped);
        let mut session = rustgis_engine::Session::new(rustgis_engine::Project::default());
        let result = session.run("info.fileLayers", &json!({"path": listed, "link": true}));
        *out.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
    });
    let opened = app.session.opened;
    app.ui.choosing.push(Chooser {
        path: path.to_string(),
        listing: Some(slot),
        layers: Vec::new(),
        filter: String::new(),
        pick,
        link: None,
        opened,
        stop,
    });
    Ok(json!({"choosing": path}))
}

/// Adds a file, read on a thread of its own when the desktop app reads it so ([`crate::readjob`]).
fn add(app: &mut RustGisApp, p: Value) -> Result<Value, String> {
    #[cfg(not(target_arch = "wasm32"))]
    return crate::readjob::run(app, "layer.addData", p);
    #[cfg(target_arch = "wasm32")]
    app.run("layer.addData", p)
}

/// Adds the layers ticked, each by name (a failure is told, by the command, and the rest still
/// added), or the whole file when none are listed; `linked` as the user answered (#2350).
fn add_chosen(app: &mut RustGisApp, path: &str, layers: &[String], linked: Option<bool>) {
    let with = |mut p: Value| {
        if let Some(linked) = linked {
            p["stream"] = json!(linked);
        }
        p
    };
    if layers.is_empty() {
        let _ = add(app, with(json!({"path": path})));
    }
    // After any file read before them, in turn.
    for layer in layers {
        let _ = add(app, with(json!({"path": path, "layer": layer})));
    }
    app.map.invalidate();
}

/// Shows the first file waiting to be chosen from; one that turns out to hold a single layer (or
/// can't be listed) is added as it would have been.
pub fn show(app: &mut RustGisApp, ctx: &egui::Context) {
    // Files chosen from for a project since replaced (new, open, sample) are dropped.
    let opened = app.session.opened;
    app.ui.choosing.retain(|c| c.opened == opened);
    let Some(c) = app.ui.choosing.first_mut() else { return };
    if let Some(slot) = &c.listing {
        let landed = slot.lock().unwrap_or_else(|e| e.into_inner()).take();
        match landed {
            None => ctx.request_repaint_after(std::time::Duration::from_millis(100)),
            Some(Ok(v)) if (c.pick && v["count"].as_u64().is_some_and(|n| n > 1)) || v["link"].is_object() => {
                if c.pick && v["count"].as_u64().is_some_and(|n| n > 1) {
                    c.layers =
                        v["layers"].as_array().into_iter().flatten().filter_map(Value::as_str).map(|l| (l.to_string(), true)).collect();
                }
                // One that can't be read whole can only be linked.
                c.link = v["link"]["copy"].as_bool().map(|copy| {
                    let rule = v["link"]["linked"] == true;
                    Link { linked: !copy || rule, copy, rule, answered: false }
                });
                c.listing = None;
            }
            // One layer, or a listing error: layer.addData adds it, or says what is wrong.
            Some(_) => {
                let path = c.path.clone();
                app.ui.choosing.remove(0);
                // A failure is told by the command.
                let _ = add(app, json!({"path": path}));
                app.map.invalidate();
                return;
            }
        }
    }
    // Enter and Escape are the chooser's only while nothing else has the keyboard: a key pressed
    // in a text box elsewhere (or in the chooser's filter) is that box's.
    let keys_free = ctx.memory(|m| m.focused().is_none());
    let t = Tokens::get(ctx);
    let mut open = true;
    let mut add: Option<Vec<String>> = None;
    let mut cancel = false;
    let file = std::path::Path::new(c.path.trim_end_matches(['/', '\\']))
        .file_name()
        .map_or_else(|| c.path.clone(), |n| n.to_string_lossy().into_owned());
    let title = if c.layers.is_empty() { tl!("Add Data") } else { tl!("Choose Layers") };
    egui::Window::new(title)
        .id(egui::Id::unique(("choose_layers", &c.path)))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center() + egui::vec2(0.0, -40.0))
        .show(ctx, |ui| {
            ui.set_width(420.0);
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
            ui.add(egui::Label::new(egui::RichText::new(&file).strong()).wrap()).on_hover_text(&c.path);
            if c.listing.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(tl!("Reading the file's layers…"));
                });
                ui.horizontal(|ui| {
                    cancel = crate::widgets::dialog_button(ui, tl!("Cancel"), None, egui::vec2(84.0, 28.0)).clicked();
                });
                return;
            }
            if let Some(link) = &mut c.link
                && c.layers.is_empty()
            {
                link_choice(ui, link, &t);
                ui.horizontal(|ui| {
                    if crate::widgets::dialog_button(ui, tl!("Add"), Some(t.accent), egui::vec2(84.0, 28.0)).clicked()
                        || (keys_free && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)))
                    {
                        add = Some(Vec::new());
                    }
                    cancel = crate::widgets::dialog_button(ui, tl!("Cancel"), None, egui::vec2(84.0, 28.0)).clicked();
                });
                return;
            }
            let total = c.layers.len().to_string();
            ui.label(egui::RichText::new(crate::i18n::fmt("{n} layers: choose the ones to add.", &[("n", &total)])).color(t.text_dim));
            // A filter for a long list (a geodatabase can hold hundreds of tables).
            if c.layers.len() > 8 {
                ui.add(egui::TextEdit::singleline(&mut c.filter).hint_text(tl!("Filter layers")).desired_width(f32::INFINITY));
            }
            let needle = c.filter.trim().to_lowercase();
            let shows = |name: &str| needle.is_empty() || name.to_lowercase().contains(&needle);
            ui.horizontal(|ui| {
                if ui.small_button(tl!("Select All")).clicked() {
                    c.layers.iter_mut().filter(|(n, _)| shows(n)).for_each(|(_, on)| *on = true);
                }
                if ui.small_button(tl!("Select None")).clicked() {
                    c.layers.iter_mut().filter(|(n, _)| shows(n)).for_each(|(_, on)| *on = false);
                }
            });
            egui::Frame::NONE.stroke(egui::Stroke::new(1.0, t.border)).corner_radius(6).inner_margin(6).show(ui, |ui| {
                egui::ScrollArea::vertical().max_height(320.0).auto_shrink([false, true]).show(ui, |ui| {
                    for (name, on) in c.layers.iter_mut().filter(|(n, _)| shows(n)) {
                        ui.checkbox(on, name.as_str());
                    }
                });
            });
            if let Some(link) = &mut c.link {
                link_choice(ui, link, &t);
            }
            let chosen: Vec<String> = c.layers.iter().filter(|(_, on)| *on).map(|(n, _)| n.clone()).collect();
            ui.horizontal(|ui| {
                let n = chosen.len().to_string();
                let label = crate::i18n::fmt("Add {n}", &[("n", &n)]);
                let enabled = !chosen.is_empty();
                let clicked = ui
                    .add_enabled_ui(enabled, |ui| crate::widgets::dialog_button(ui, &label, Some(t.accent), egui::vec2(84.0, 28.0)))
                    .inner
                    .clicked();
                if enabled && (clicked || (keys_free && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)))) {
                    add = Some(chosen.clone());
                }
                cancel = crate::widgets::dialog_button(ui, tl!("Cancel"), None, egui::vec2(84.0, 28.0)).clicked();
            });
        });
    if keys_free && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        cancel = true;
    }
    if let Some(layers) = add {
        let path = c.path.clone();
        // A GeoJSON file's answer is always sent: it starts from its size, and `layer.addData` unasked
        // would decide by its count once it is read in place.
        let geojson = path.trim().to_ascii_lowercase().ends_with(".geojson");
        let linked = c.link.as_ref().filter(|l| geojson || l.answered || l.linked != l.rule).map(|l| l.linked);
        app.ui.choosing.remove(0);
        add_chosen(app, &path, &layers, linked);
    } else if cancel || !open {
        app.ui.choosing.remove(0);
    }
}

/// Copy into the project or link to the file (#2350), with what each means: a linked layer is
/// read in place, so editing it or defining its projection asks for Load All Features first.
fn link_choice(ui: &mut egui::Ui, link: &mut Link, t: &Tokens) {
    let dim = |text: &str| egui::RichText::new(text.to_string()).color(t.text_dim);
    ui.add_enabled_ui(link.copy, |ui| {
        link.answered |= ui.radio_value(&mut link.linked, false, tl!("Copy into the project")).clicked();
        ui.label(dim(tl!("Editable. The project file grows with its data.")));
    })
    .response
    .on_disabled_hover_text(tl!("Too large to read whole: it can only be linked."));
    link.answered |= ui.radio_value(&mut link.linked, true, tl!("Link to the file")).clicked();
    ui.add(
        egui::Label::new(dim(tl!(
            "The project keeps where the file is. Read in place, a view at a time: to edit it or define its projection, Load All Features first."
        )))
        .wrap(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_engine::{Project, Session};

    /// A `.sde` file dropped or added opens the geodatabase form filled from it, and adds no layer.
    #[test]
    fn a_sde_file_added_opens_the_geodatabase_form() {
        let path = format!("{}/../data/testdata/sde/sqlserver-os.sde", env!("CARGO_MANIFEST_DIR"));
        let mut app = crate::RustGisApp::with_sample();
        let layers = app.session.project.maps.first().map_or(0, |m| m.layers.len());
        add_file(&mut app, &path).unwrap();
        assert_eq!(app.dialogs.open, Some("newConnection"));
        assert!(app.ui.connection.form);
        assert_eq!(app.ui.connection.server, "gisdb.example.invalid");
        assert_eq!(app.session.project.maps.first().map_or(0, |m| m.layers.len()), layers);
    }

    /// The app drawn for a test, past its first frames (which install the fonts).
    fn harness(app: RustGisApp) -> egui_kittest::Harness<'static, RustGisApp> {
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1400.0, 900.0)).with_max_steps(64).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app,
        );
        h.run_steps(4);
        h
    }

    /// A chooser whose listing has already landed, for the project `opened`.
    fn listed(opened: u64, path: &str, result: Result<Value, String>) -> Chooser {
        let slot: crate::Slot<Result<Value, String>> = crate::Slot::default();
        *slot.lock().unwrap() = Some(result);
        Chooser {
            path: path.to_string(),
            listing: Some(slot),
            layers: Vec::new(),
            filter: String::new(),
            pick: true,
            link: None,
            opened,
            stop: Default::default(),
        }
    }

    const KML: &str = r#"<kml xmlns="http://www.opengis.net/kml/2.2"><Document>
<Placemark><name>A</name><Point><coordinates>8.5,47.4</coordinates></Point></Placemark>
<Placemark><name>B</name><LineString><coordinates>8.5,47.4 8.6,47.5</coordinates></LineString></Placemark>
</Document></kml>"#;

    #[test]
    fn geolibre_local_drop_and_browser_route_import_without_replacing_work() {
        let dir = std::env::temp_dir().join(format!("rustgis-geolibre-add-file-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let text = json!({"version":"0.2.0","name":"Dropped map","mapView":{},"basemapStyleUrl":"","layers":[]}).to_string();
        for name in ["map.geolibre", "map.geolibre.json"] {
            let path = dir.join(name);
            std::fs::write(&path, &text).unwrap();
            let mut app = RustGisApp::with_sample();
            let original = app.session.map().name.clone();
            let maps = app.session.project.maps.len();
            add_file(&mut app, &path.to_string_lossy()).unwrap();
            assert_eq!(app.session.project.maps.len(), maps + 1);
            assert_eq!(app.session.project.maps[0].name, original);
            assert_eq!(app.session.map().name, "Dropped map");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A file in Recent or Favourites with several layers asks which to add, as Add Data does; one
    /// recorded with its layer adds that layer.
    #[test]
    fn a_recent_file_with_several_layers_asks_which() {
        let mut app = RustGisApp::new(Session::new(Project::default()));
        let gdb = std::env::temp_dir().join("rustgis-recent-asks.gdb").display().to_string();
        let file =
            |extra: Value| rustgis_engine::sources::Source { kind: "file".into(), name: "Parcels".into(), address: gdb.clone(), extra };
        crate::sourcespane::open(&mut app, &file(Value::Null)).unwrap();
        assert_eq!(app.ui.choosing.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(), [gdb.as_str()]);
        app.ui.choosing.clear();
        // (The geodatabase isn't there, so adding its layer fails: but it asks nothing.)
        let _ = crate::sourcespane::open(&mut app, &file(json!({"layer": "roads"})));
        assert!(app.ui.choosing.is_empty());
    }

    /// A file with several layers lists them, all ticked, and adds the ones chosen, each by name;
    /// one with a single layer, or that can't be listed, is added as before.
    #[test]
    fn several_layers_are_chosen_one_is_added() {
        use egui_kittest::kittest::Queryable;
        let dir = tempfile::tempdir().unwrap();
        let kml = dir.path().join("two.kml");
        std::fs::write(&kml, KML).unwrap();
        let kml = kml.display().to_string();
        let mut app = RustGisApp::new(Session::new(Project::default()));
        let listing = app.session.run("info.fileLayers", &json!({"path": kml}));
        let names: Vec<String> =
            listing.as_ref().unwrap()["layers"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
        assert_eq!(names.len(), 2, "{listing:?}");
        let mut h = harness(app);
        let opened = h.state().session.opened;
        let before = h.state().session.map().layers.len();
        h.state_mut().ui.choosing.push(listed(opened, &kml, listing));
        h.run_steps(2);
        let c = h.state().ui.choosing.first().unwrap();
        assert!(c.listing.is_none());
        assert_eq!(c.layers, names.iter().map(|n| (n.clone(), true)).collect::<Vec<_>>());
        // The first unticked, the second added.
        h.get_by_label(&names[0]).click();
        h.run_steps(2);
        h.get_by_label("Add 1").click();
        h.run_steps(2);
        assert!(h.state().ui.choosing.is_empty());
        let map = h.state().session.map();
        assert_eq!(map.layers.len(), before + 1);
        assert_eq!(map.layers.last().map(|l| l.name.as_str()), Some(names[1].as_str()));
        // One layer: added without asking. A listing error: layer.addData says what is wrong.
        let geojson = dir.path().join("one.geojson");
        std::fs::write(&geojson, r#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[1,2]}}]}"#).unwrap();
        let geojson = geojson.display().to_string();
        h.state_mut().ui.choosing.push(listed(opened, &geojson, Ok(json!({"count": 1, "layers": ["one"]}))));
        h.run_steps(2);
        assert!(h.state().ui.choosing.is_empty());
        assert_eq!(h.state().session.map().layers.len(), before + 2);
        let mark = h.state().session.notice_mark();
        h.state_mut().ui.choosing.push(listed(opened, "/nonexistent/x.gpkg", Err("no such file".into())));
        h.run_steps(2);
        assert!(h.state().ui.choosing.is_empty());
        assert_eq!(h.state().session.notices_since(mark).len(), 1, "a failure is told once");
        // Cancel adds nothing.
        h.state_mut().ui.choosing.push(listed(opened, &kml, Ok(json!({"count": 2, "layers": names.clone()}))));
        h.run_steps(2);
        h.get_by_label("Cancel").click();
        h.run_steps(2);
        assert!(h.state().ui.choosing.is_empty());
        assert_eq!(h.state().session.map().layers.len(), before + 2);
        // Another project opened while a file waits: its layers never go to the new project.
        h.state_mut().ui.choosing.push(listed(opened, &kml, Ok(json!({"count": 2, "layers": names}))));
        h.state_mut().session.run("project.new", &json!({})).unwrap();
        assert_ne!(h.state().session.opened, opened);
        let fresh = h.state().session.map().layers.len();
        h.run_steps(2);
        assert!(h.state().ui.choosing.is_empty());
        assert_eq!(h.state().session.map().layers.len(), fresh);
    }

    /// A file Add Data can link asks whether to copy it into the project or link to it (#2350),
    /// starting from what Add Data does without asking; a GeoPackage of several tables asks once,
    /// at the foot of Choose Layers, or on its own with Ask Which Layers to Add off.
    #[test]
    fn a_linkable_file_asks_whether_to_copy_or_link() {
        use egui_kittest::kittest::Queryable;
        use rustgis_engine::data::{Coord, Dataset, Field, FieldType, Geometry, GeometryType, Value as V};
        let dir = tempfile::tempdir().unwrap();
        let mut ds = Dataset::new("points", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
        for i in 0..3 {
            ds.push(Some(Geometry::Point(Coord::new(i as f64, 1.0))), vec![V::Text(format!("p{i}"))]);
        }
        let fgb = dir.path().join("one.fgb");
        rustgis_engine::data::io::write_path(&ds, &fgb).unwrap();
        let gpkg = dir.path().join("two.gpkg");
        for table in ["a", "b"] {
            let add = rustgis_engine::data::io::GpkgTable { mode: rustgis_engine::data::io::GpkgMode::AddTable, name: Some(table.into()) };
            rustgis_engine::data::io::write_path_to(&ds, &gpkg, rustgis_engine::data::raster::Crs::LonLat, &add).unwrap();
        }
        let (fgb, gpkg) = (fgb.display().to_string(), gpkg.display().to_string());
        let mut app = RustGisApp::new(Session::new(Project::default()));
        // Dropped, picked or double-clicked, it waits for the answer.
        assert_eq!(add_file(&mut app, &fgb).unwrap(), json!({"choosing": fgb}));
        assert!(app.ui.choosing.len() == 1 && app.session.map().layers.is_empty());
        app.ui.choosing.clear();
        let listing = |h: &mut egui_kittest::Harness<'static, RustGisApp>, path: &str| {
            let v = h.state_mut().session.run("info.fileLayers", &json!({"path": path, "link": true}));
            let opened = h.state().session.opened;
            let mut c = listed(opened, path, v);
            c.pick = h.state().ui.ask_layers && lists_layers(path);
            h.state_mut().ui.choosing.push(c);
            h.run_steps(2);
        };
        let newest_linked =
            |h: &egui_kittest::Harness<'static, RustGisApp>| h.state().session.map().layers.first().map(|l| l.is_streamed());
        let geojson = dir.path().join("big.geojson");
        rustgis_engine::data::io::write_path(&ds, &geojson).unwrap();
        let geojson = geojson.display().to_string();
        let mut h = harness(app);
        // A GeoJSON file starts at Link by its size, and `layer.addData` unasked would decide by
        // its count once read in place: the answer shown is sent, untouched, so Add does it.
        let opened = h.state().session.opened;
        h.state_mut().ui.choosing.push(listed(
            opened,
            &geojson,
            Ok(json!({"count": 1, "layers": ["big"], "link": {"linked": true, "copy": true}})),
        ));
        h.run_steps(2);
        h.get_by_label("Add").click();
        h.run_steps(2);
        assert_eq!(newest_linked(&h), Some(true), "Link shown: linked");
        // Small: Copy is chosen, so Add (or Enter) adds it as today.
        listing(&mut h, &fgb);
        h.get_by_label("Link to the file");
        h.get_by_label("Add").click();
        h.run_steps(2);
        assert!(h.state().ui.choosing.is_empty());
        assert_eq!(newest_linked(&h), Some(false));
        // Link chosen: read in place, the project keeps its path.
        listing(&mut h, &fgb);
        h.get_by_label("Link to the file").click();
        h.run_steps(2);
        h.get_by_label("Add").click();
        h.run_steps(2);
        assert_eq!(newest_linked(&h), Some(true));
        // Too large to read whole: Copy can't be chosen.
        let opened = h.state().session.opened;
        h.state_mut().ui.choosing.push(listed(
            opened,
            &fgb,
            Ok(json!({"count": 1, "layers": ["one"], "link": {"linked": false, "copy": false}})),
        ));
        h.run_steps(2);
        h.get_by_label("Copy into the project").click();
        h.run_steps(2);
        h.get_by_label("Add").click();
        h.run_steps(2);
        assert_eq!(newest_linked(&h), Some(true));
        // Several tables: one choice for the tables ticked.
        let before = h.state().session.map().layers.len();
        listing(&mut h, &gpkg);
        h.get_by_label("Link to the file").click();
        h.run_steps(2);
        h.get_by_label("Add 2").click();
        h.run_steps(2);
        let layers = &h.state().session.map().layers;
        assert_eq!(layers.len(), before + 2);
        // New layers go on top.
        assert!(layers[..2].iter().all(|l| l.is_streamed()));
        // The answer left as it starts sends nothing, so each table keeps its own size rule (a
        // small table of a GeoPackage whose large one starts the answer at Link is still copied).
        let before = h.state().session.map().layers.len();
        let opened = h.state().session.opened;
        let mut c = listed(opened, &gpkg, Ok(json!({"count": 2, "layers": ["a", "b"], "link": {"linked": true, "copy": true}})));
        c.pick = true;
        h.state_mut().ui.choosing.push(c);
        h.run_steps(2);
        h.get_by_label("Add 2").click();
        h.run_steps(2);
        let layers = &h.state().session.map().layers;
        assert_eq!(layers.len(), before + 2);
        assert!(layers[..2].iter().all(|l| !l.is_streamed()), "small tables keep their rule: copied");
        // Link clicked, though it was already selected: every table ticked is linked.
        let mut c = listed(opened, &gpkg, Ok(json!({"count": 2, "layers": ["a", "b"], "link": {"linked": true, "copy": true}})));
        c.pick = true;
        h.state_mut().ui.choosing.push(c);
        h.run_steps(2);
        h.get_by_label("Link to the file").click();
        h.run_steps(1);
        h.get_by_label("Add 2").click();
        h.run_steps(2);
        let layers = &h.state().session.map().layers;
        assert_eq!(layers.len(), before + 4);
        assert!(layers[..2].iter().all(|l| l.is_streamed()), "Link answered: every table linked");
        let before = before + 2;
        // Ask Which Layers to Add off: every table, with the same question.
        h.state_mut().ui.ask_layers = false;
        assert_eq!(add_file(h.state_mut(), &gpkg).unwrap(), json!({"choosing": gpkg}));
        h.state_mut().ui.choosing.clear();
        listing(&mut h, &gpkg);
        assert!(h.query_by_label("Select All").is_none(), "no list of tables");
        h.get_by_label("Add").click();
        h.run_steps(2);
        let layers = &h.state().session.map().layers;
        assert_eq!(layers.len(), before + 4);
        assert!(layers[..2].iter().all(|l| !l.is_streamed()));
    }

    /// Cancel, or the window closed, stops the listing still on its way (AGENTS.md › Never hold
    /// the window): a GeoPackage's tables stop being opened and counted (#2350).
    #[test]
    fn cancel_stops_the_listing_on_its_way() {
        use egui_kittest::kittest::Queryable;
        let app = RustGisApp::new(Session::new(Project::default()));
        let mut h = harness(app);
        let opened = h.state().session.opened;
        let mut c = listed(opened, "/nonexistent/survey.gpkg", Ok(json!({})));
        c.listing = Some(crate::Slot::default());
        let stop = c.stop.clone();
        h.state_mut().ui.choosing.push(c);
        h.run_steps(2);
        assert!(!stop.load(std::sync::atomic::Ordering::Relaxed));
        h.get_by_label("Cancel").click();
        h.run_steps(2);
        assert!(h.state().ui.choosing.is_empty());
        assert!(stop.load(std::sync::atomic::Ordering::Relaxed), "the listing is told to stop");
    }

    /// Another project opened while the listing is on its way drops the chooser, and the listing
    /// stops as on Cancel (#2827).
    #[test]
    fn another_project_stops_the_listing_on_its_way() {
        let app = RustGisApp::new(Session::new(Project::default()));
        let mut h = harness(app);
        let opened = h.state().session.opened;
        let mut c = listed(opened, "/nonexistent/survey.gpkg", Ok(json!({})));
        c.listing = Some(crate::Slot::default());
        let stop = c.stop.clone();
        h.state_mut().ui.choosing.push(c);
        h.run_steps(2);
        assert!(!stop.load(std::sync::atomic::Ordering::Relaxed));
        h.state_mut().session.run("project.new", &json!({})).unwrap();
        h.run_steps(2);
        assert!(h.state().ui.choosing.is_empty());
        assert!(stop.load(std::sync::atomic::Ordering::Relaxed), "the listing is told to stop");
    }

    /// The setting: off, a file is added whole without listing it; it is kept as a preference.
    #[test]
    fn the_setting_adds_every_layer() {
        let mut app = RustGisApp::new(Session::new(Project::default()));
        let saved = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = saved.clone();
        app.services.save_pref = Some(Box::new(move |k, v| log.lock().unwrap().push((k.to_string(), v.to_string()))));
        assert!(app.ui.ask_layers);
        assert_eq!(app.run("ui.askLayers", json!({"on": false})).unwrap(), json!({"on": false}));
        // A value that isn't true or false is refused and changes nothing.
        for junk in [json!({"on": "true"}), json!({"on": 1}), json!({"on": [true]})] {
            assert!(app.run("ui.askLayers", junk).is_err());
            assert!(!app.ui.ask_layers);
        }
        assert_eq!(saved.lock().unwrap().last().cloned(), Some(("ask_file_layers".to_string(), "off".to_string())));
        // A geodatabase that isn't there: added straight away (and refused), never listed. (A
        // GeoPackage is listed either way, to ask whether to copy or link it: #2350.)
        assert!(app.run("ui.addFile", json!({"path": "/nonexistent/x.gdb"})).is_err());
        assert!(app.ui.choosing.is_empty());
        app.run("ui.askLayers", json!({"on": true})).unwrap();
        assert!(app.run("ui.addFile", json!({"path": "/nonexistent/x.gdb"})).is_ok());
        assert_eq!(app.ui.choosing.len(), 1);
        assert!(app.run("ui.addFile", json!({})).is_err());
    }

    #[test]
    fn files_that_list_their_layers() {
        if cfg!(target_arch = "wasm32") {
            return;
        }
        for p in ["/data/roads.gdb", "/data/roads.gdb/", "/data/roads.gdb/a00000009.gdbtable", "x.gpkg", "x.duckdb"] {
            assert!(lists_layers(p), "{p}");
        }
        for p in ["x.geojson", "x.kml", "x.zip", "x.dxf", "x.xlsx", "x.tif", "x.csv", "x.parquet", "x.fgb", "x.las", "x.laz", "x.shp"] {
            assert!(!lists_layers(p), "{p}");
        }
    }
}
