//! Choose Layers (#934): a file holding several layers (a File Geodatabase, a GeoPackage, a DuckDB
//! database, a file GDAL reads) asks which to add when it is dropped on the window, picked with
//! Add Data or double-clicked in the Browser. Built on `info.fileLayers` (read off the frame) and
//! `layer.addData`'s `"layer"`; with Home › Add Data › Ask Which Layers to Add off, every layer is
//! added, as before.
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
    /// The project it was opened for (`Session::opened`): another project opened since drops it,
    /// so its layers never go to the wrong project.
    opened: u64,
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
    if !(app.ui.ask_layers && lists_layers(path)) {
        return app.run("layer.addData", json!({"path": path}));
    }
    // Listed off the frame: a geodatabase's or GDAL's listing can take a while.
    let slot: crate::Slot<Result<Value, String>> = crate::Slot::default();
    let out = slot.clone();
    let listed = path.to_string();
    std::thread::spawn(move || {
        let mut session = rustgis_engine::Session::new(rustgis_engine::Project::default());
        let result = session.run("info.fileLayers", &json!({"path": listed}));
        *out.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
    });
    let opened = app.session.opened;
    app.ui.choosing.push(Chooser { path: path.to_string(), listing: Some(slot), layers: Vec::new(), filter: String::new(), opened });
    Ok(json!({"choosing": path}))
}

/// Adds the layers ticked, each by name (a failure is told, by the command, and the rest still
/// added).
fn add_chosen(app: &mut RustGisApp, path: &str, layers: &[String]) {
    for layer in layers {
        let _ = app.run("layer.addData", json!({"path": path, "layer": layer}));
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
            Some(Ok(v)) if v["count"].as_u64().is_some_and(|n| n > 1) => {
                c.layers = v["layers"].as_array().into_iter().flatten().filter_map(Value::as_str).map(|l| (l.to_string(), true)).collect();
                c.listing = None;
            }
            // One layer, or a listing error: layer.addData adds it, or says what is wrong.
            Some(_) => {
                let path = c.path.clone();
                app.ui.choosing.remove(0);
                // A failure is told by the command.
                let _ = app.run("layer.addData", json!({"path": path}));
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
    egui::Window::new(tl!("Choose Layers"))
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
        app.ui.choosing.remove(0);
        add_chosen(app, &path, &layers);
    } else if cancel || !open {
        app.ui.choosing.remove(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_engine::{Project, Session};

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
        Chooser { path: path.to_string(), listing: Some(slot), layers: Vec::new(), filter: String::new(), opened }
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
        // A GeoPackage that isn't there: added straight away (and refused), never listed.
        assert!(app.run("ui.addFile", json!({"path": "/nonexistent/x.gpkg"})).is_err());
        assert!(app.ui.choosing.is_empty());
        app.run("ui.askLayers", json!({"on": true})).unwrap();
        assert!(app.run("ui.addFile", json!({"path": "/nonexistent/x.gpkg"})).is_ok());
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
