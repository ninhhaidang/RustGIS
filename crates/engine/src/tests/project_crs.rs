//! The project's coordinate system (#2032 C3a): saved only when set, with `project-crs`; new maps
//! drawn in it and new empty layers held in it; taken from the first layer added when asked.

use rustgis_data::raster::Crs;
use serde_json::{Value, json};

use crate::{Project, Session};

/// Swiss LV95, a projected system a map can be drawn in.
const LV95: Crs = Crs::Epsg(2056);

fn saved(s: &Session) -> Value {
    serde_json::to_value(&s.project).unwrap()
}

fn requires(v: &Value) -> Vec<String> {
    v.get("requires")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|c| c.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

#[test]
fn a_project_without_a_coordinate_system_saves_as_before() {
    let s = Session::new(Project::default());
    let v = saved(&s);
    assert!(v.get("coordinate_system").is_none() && v.get("crs_from_first_layer").is_none(), "{v}");
    assert!(!requires(&v).contains(&"project-crs".to_string()));
    let info = Session::new(Project::default()).run("info.project", &json!({})).unwrap();
    assert_eq!(info["coordinateSystem"], json!({"crs": null, "label": null, "mapsCanUse": null, "fromFirstLayer": false}));
}

#[test]
fn the_coordinate_system_is_set_saved_reopened_and_undone() {
    let mut s = Session::new(Project::default());
    let out = s.run("project.coordinateSystem", &json!({"crs": "2056"})).unwrap();
    assert_eq!(out, json!({"crs": "EPSG:2056", "label": "EPSG:2056", "mapsCanUse": true, "fromFirstLayer": false}));
    let v = saved(&s);
    assert_eq!(requires(&v), ["project-crs"]);
    assert!(v.get("crs_from_first_layer").is_none());
    let mut t = Session::new(Project::default());
    t.run("project.openText", &json!({"text": v.to_string()})).unwrap();
    assert_eq!(t.project.coordinate_system, Some(LV95));
    // The option alone needs the capability too: an older reader would save the project without it.
    s.run("project.coordinateSystem", &json!({"crs": null, "fromFirstLayer": true})).unwrap();
    assert_eq!((s.project.coordinate_system, s.project.crs_from_first_layer), (None, true));
    assert_eq!(requires(&saved(&s)), ["project-crs"]);
    // Omitted keys are kept; Undo takes each step back.
    s.run("project.coordinateSystem", &json!({"crs": "EPSG:32632"})).unwrap();
    assert_eq!((s.project.coordinate_system, s.project.crs_from_first_layer), (Some(Crs::Epsg(32632)), true));
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!((s.project.coordinate_system, s.project.crs_from_first_layer), (None, true));
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!((s.project.coordinate_system, s.project.crs_from_first_layer), (Some(LV95), false));
}

#[test]
fn the_command_refuses_what_it_cant_read() {
    let mut s = Session::new(Project::default());
    for p in
        [json!({}), json!({"crs": 7}), json!({"crs": "EPSG:999999999"}), json!({"crs": "not a system"}), json!({"fromFirstLayer": "yes"})]
    {
        assert!(s.run("project.coordinateSystem", &p).is_err(), "{p}");
    }
    assert_eq!((s.project.coordinate_system, s.project.crs_from_first_layer), (None, false));
    // An empty code clears it, as null does.
    s.run("project.coordinateSystem", &json!({"crs": "2056"})).unwrap();
    s.run("project.coordinateSystem", &json!({"crs": " "})).unwrap();
    assert_eq!(s.project.coordinate_system, None);
}

#[test]
fn a_new_map_is_drawn_in_the_project_system_where_the_active_map_looks() {
    let mut s = Session::new(Project::default());
    s.run("map.new", &json!({"name": "Before"})).unwrap();
    assert_eq!(s.map().projection, rustgis_map::Projection::WebMercator, "without one, as before");
    s.run("map.zoomToExtent", &json!({"extent": [6.0, 45.8, 10.5, 47.8]})).unwrap();
    let centre = s.map().center_lonlat();
    s.run("project.coordinateSystem", &json!({"crs": "EPSG:2056"})).unwrap();
    // The maps already there keep theirs.
    assert_eq!(s.map().projection, rustgis_map::Projection::WebMercator);
    s.run("map.new", &json!({"name": "Swiss"})).unwrap();
    assert_eq!(s.map().projection.code(), "EPSG:2056");
    let at = s.map().center_lonlat();
    assert!((at.x - centre.x).abs() < 1e-6 && (at.y - centre.y).abs() < 1e-6, "{at:?} vs {centre:?}");
}

#[test]
fn a_new_map_in_a_system_a_map_cant_use_says_why() {
    let mut s = Session::new(Project::default());
    // ETRS89, in degrees.
    s.run("project.coordinateSystem", &json!({"crs": "EPSG:4258"})).unwrap();
    assert_eq!(s.run("info.project", &json!({})).unwrap()["coordinateSystem"]["mapsCanUse"], json!(false));
    let mark = s.notice_mark();
    s.run("map.new", &json!({"name": "Degrees"})).unwrap();
    assert_eq!(s.map().projection, rustgis_map::Projection::WebMercator);
    let said: Vec<String> = s.notices_since(mark).iter().map(|n| n.text.clone()).collect();
    assert!(said.iter().any(|t| t.contains("Web Mercator") && t.contains("EPSG:4258")), "{said:?}");
}

#[test]
fn a_new_template_layer_is_held_in_the_project_system_and_edited_there() {
    let mut s = Session::new(Project::default());
    s.run("layer.template", &json!({"template": "points", "name": "Plain"})).unwrap();
    assert_eq!(s.map().layer_by_name("Plain").unwrap().dataset.coord_crs(), Crs::LonLat, "without one, as before");
    s.run("project.coordinateSystem", &json!({"crs": "EPSG:2056"})).unwrap();
    s.run("layer.template", &json!({"template": "points", "name": "Swiss"})).unwrap();
    let l = s.map().layer_by_name("Swiss").unwrap();
    assert_eq!((l.dataset.coord_crs(), l.dataset.crs), (LV95, Some(LV95)));
    // A point drawn at Bern's place is held in LV95 metres.
    s.map_mut().editing.settings.enabled = true;
    s.run("edit.addFeature", &json!({"layer": "Swiss", "geometry": {"type": "Point", "coordinates": [7.43863, 46.95108]}})).unwrap();
    let l = s.map().layer_by_name("Swiss").unwrap();
    let Some(rustgis_data::Geometry::Point(c)) = l.dataset.features.first().and_then(|f| f.geometry.clone()) else { panic!("no point") };
    assert!((c.x - 2_600_000.0).abs() < 5.0 && (c.y - 1_200_000.0).abs() < 5.0, "{c:?}");
    // Saved, it needs the native coordinates as any held layer does.
    let r = requires(&saved(&s));
    assert!(r.contains(&"native-coordinates".to_string()) && r.contains(&"project-crs".to_string()), "{r:?}");
}

/// A GeoPackage with a point at Bern in LV95, made in a project of that system.
fn lv95_file(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("rustgis-project-crs-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("bern.gpkg");
    let mut w = Session::new(Project::default());
    w.run("project.coordinateSystem", &json!({"crs": "EPSG:2056"})).unwrap();
    w.run("layer.template", &json!({"template": "points", "name": "Bern"})).unwrap();
    w.map_mut().editing.settings.enabled = true;
    w.run("edit.addFeature", &json!({"layer": "Bern", "geometry": {"type": "Point", "coordinates": [7.43863, 46.95108]}})).unwrap();
    w.run("share.exportLayer", &json!({"layer": "Bern", "path": path})).unwrap();
    (dir, path)
}

#[test]
fn the_first_layer_added_gives_its_system_when_asked() {
    let (dir, path) = lv95_file("first");
    // Off (the default): nothing is taken.
    let mut s = Session::new(Project::default());
    s.run("layer.addData", &json!({"path": path, "name": "Off"})).unwrap();
    assert_eq!(s.project.coordinate_system, None);
    // On: the next layer added with a system of its own gives it, said, and Undo takes both back.
    s.run("project.coordinateSystem", &json!({"fromFirstLayer": true})).unwrap();
    let mark = s.notice_mark();
    s.run("layer.addData", &json!({"path": path, "name": "On"})).unwrap();
    assert_eq!(s.project.coordinate_system, Some(LV95));
    assert!(s.notices_since(mark).iter().any(|n| n.text.contains("EPSG:2056") && n.text.contains("'On'")), "{:?}", s.notices);
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(s.project.coordinate_system, None);
    assert!(s.map().layer_by_name("On").is_none());
    // Redo brings back the layer and the system the step set.
    s.run("edit.redo", &Value::Null).unwrap();
    assert_eq!(s.project.coordinate_system, Some(LV95));
    // Once set, a later layer doesn't change it.
    s.run("project.coordinateSystem", &json!({"crs": "EPSG:32632"})).unwrap();
    s.run("layer.addData", &json!({"path": path, "name": "Later"})).unwrap();
    assert_eq!(s.project.coordinate_system, Some(Crs::Epsg(32632)));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_layer_without_a_system_of_its_own_gives_none() {
    let mut s = Session::new(Project::default());
    s.run("project.coordinateSystem", &json!({"fromFirstLayer": true})).unwrap();
    // A template's layer is made in longitude/latitude with no system named.
    s.run("layer.template", &json!({"template": "lines"})).unwrap();
    assert_eq!(s.project.coordinate_system, None);
}

/// An ASCII grid of 4 × 3 cells at (x, y), 0.5 units a side.
fn grid(dir: &std::path::Path, name: &str, x: f64, y: f64, prj: Option<String>) -> std::path::PathBuf {
    let path = dir.join(format!("{name}.asc"));
    std::fs::write(&path, format!("ncols 4\nnrows 3\nxllcorner {x}\nyllcorner {y}\ncellsize 0.5\n1 2 3 4\n5 6 7 8\n9 10 11 12\n")).unwrap();
    if let Some(prj) = prj {
        std::fs::write(dir.join(format!("{name}.prj")), prj).unwrap();
    }
    path
}

#[test]
fn a_raster_read_as_wgs84_by_assumption_gives_none_and_one_that_names_its_system_gives_it() {
    let dir = std::env::temp_dir().join(format!("rustgis-project-crs-{}-grids", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = Session::new(Project::default());
    s.run("project.coordinateSystem", &json!({"fromFirstLayer": true})).unwrap();
    // No .prj, coordinates that look like degrees: WGS 84 assumed, so no system of its own.
    let assumed = grid(&dir, "assumed", 8.0, 46.0, None);
    s.run("layer.addData", &json!({"path": assumed})).unwrap();
    let l = s.map().layers.last().unwrap();
    assert!(l.raster.as_ref().unwrap().data.as_ref().unwrap().crs_assumed);
    assert_eq!(s.project.coordinate_system, None);
    // With its .prj: LV95.
    let named = grid(&dir, "named", 2_600_000.0, 1_200_000.0, rustgis_data::raster::wkt::to_wkt(LV95));
    s.run("layer.addData", &json!({"path": named})).unwrap();
    assert_eq!(s.project.coordinate_system, Some(LV95));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_system_given_back_reads_as_the_same_system() {
    let mut s = Session::new(Project::default());
    // A WKT whose PROJ definition alone loses its grads, and an ESRI code.
    let grads = r#"GEOGCS["NTF (Paris)",DATUM["Nouvelle_Triangulation_Francaise_Paris",SPHEROID["Clarke 1880 (IGN)",6378249.2,293.466021293627,AUTHORITY["EPSG","7011"]],AUTHORITY["EPSG","6807"]],PRIMEM["Paris",2.33722917,AUTHORITY["EPSG","8903"]],UNIT["grad",0.0157079632679489,AUTHORITY["EPSG","9105"]]]"#;
    for given in [grads, "ESRI:102033", "EPSG:2056"] {
        let out = s.run("project.coordinateSystem", &json!({"crs": given})).unwrap();
        let first = s.project.coordinate_system.unwrap();
        s.run("project.coordinateSystem", &json!({"crs": null})).unwrap();
        s.run("project.coordinateSystem", &json!({"crs": out["crs"]})).unwrap();
        let again = s.project.coordinate_system.unwrap();
        assert_eq!((first.definition_text(), first.label()), (again.definition_text(), again.label()), "{given}");
    }
    let out = s.run("project.coordinateSystem", &json!({"crs": "ESRI:102033"})).unwrap();
    assert_eq!(out["crs"], "ESRI:102033");
}

#[test]
fn a_script_step_gives_the_system_to_the_steps_after_it() {
    let (dir, path) = lv95_file("script");
    let mut s = Session::new(Project::default());
    s.run("project.coordinateSystem", &json!({"fromFirstLayer": true})).unwrap();
    let script = format!("layer.addData {}\nmap.new {}", json!({"path": path}), json!({"name": "After"}));
    s.run("script.run", &json!({"script": script})).unwrap();
    assert_eq!(s.project.coordinate_system, Some(LV95));
    assert_eq!(s.map().projection.code(), "EPSG:2056", "the new map, made after the layer, is in it");
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(s.project.coordinate_system, None, "one step with the script");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_raster_whose_pixels_land_between_commands_still_gives_its_system() {
    let mut s = Session::new(Project::default());
    s.run("project.coordinateSystem", &json!({"fromFirstLayer": true})).unwrap();
    let gt = rustgis_data::raster::GeoTransform { origin_x: 2_600_000.0, origin_y: 1_200_000.0, pixel_width: 10.0, pixel_height: -10.0 };
    let r = rustgis_data::raster::Raster::new(
        "dem",
        2,
        2,
        vec![vec![1.0; 4]],
        gt,
        LV95,
        None,
        rustgis_data::raster::ColorHint::Gray,
        "Float32",
    )
    .unwrap();
    // Added with its pixels still to come, as a raster read in place is.
    let spec = *s.registry.get("layer.addData").unwrap();
    let watch = crate::cmd::project::first_layer_watch(&s, &spec).unwrap();
    let mut layer = rustgis_map::RasterLayer::from_path(r, "dem.tif");
    let pixels = layer.data.take();
    s.map_mut().add_raster("dem", layer);
    crate::cmd::project::take_first_layer_system(&mut s, &watch);
    assert_eq!(s.project.coordinate_system, None);
    // Removed and brought back by Undo while waiting, it still waits.
    s.run("layer.remove", &json!({"layer": "dem"})).unwrap();
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(s.project.coordinate_system, None);
    // Its pixels land while the map draws, between commands. A command that changes nothing
    // leaves the project as it is; the next that changes it finds the raster, unsaved and undone
    // with it.
    let id = s.map().layer_by_name("dem").unwrap().id;
    s.map_mut().layer_mut(id).unwrap().raster.as_mut().unwrap().data = pixels;
    s.mark_saved();
    s.run("map.zoomIn", &json!({})).unwrap();
    assert_eq!(s.project.coordinate_system, None);
    assert!(!s.dirty());
    s.run("project.rename", &json!({"name": "Swiss"})).unwrap();
    assert_eq!(s.project.coordinate_system, Some(LV95));
    assert!(s.dirty());
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(s.project.coordinate_system, None);
}

#[test]
fn a_script_that_fails_after_taking_the_system_gives_it_back() {
    let (dir, path) = lv95_file("failing");
    let mut s = Session::new(Project::default());
    s.run("project.coordinateSystem", &json!({"fromFirstLayer": true})).unwrap();
    let script = format!("layer.addData {}\nlayer.remove {}", json!({"path": path}), json!({"layer": "No such layer"}));
    assert!(s.run("script.run", &json!({"script": script})).is_err());
    assert_eq!(s.project.coordinate_system, None);
    assert!(s.map().layers.is_empty());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_tool_output_gives_no_system_only_data_added_does() {
    let mut s = Session::new(Project::default());
    s.project.crs_from_first_layer = true;
    // With no raster waiting: only a command adding data looks.
    for (id, watched) in [
        ("gp.run", false),
        ("model.run", false),
        ("layer.template", false),
        ("project.rename", false),
        ("layer.addData", true),
        ("layer.addUrl", true),
        ("service.add", true),
        ("map.import", true),
        ("layer.paste", true),
        ("offline.open", true),
    ] {
        let spec = *s.registry.get(id).unwrap_or_else(|| panic!("{id} is a command"));
        assert_eq!(crate::cmd::project::first_layer_watch(&s, &spec).is_some(), watched, "{id}");
    }
}

#[test]
fn a_script_line_that_clears_the_system_isnt_undone_by_the_script() {
    let (dir, path) = lv95_file("cleared");
    let mut s = Session::new(Project::default());
    s.run("project.coordinateSystem", &json!({"fromFirstLayer": true})).unwrap();
    let script = format!("layer.addData {}\nproject.coordinateSystem {}", json!({"path": path}), json!({"crs": null}));
    s.run("script.run", &json!({"script": script})).unwrap();
    assert_eq!(s.project.coordinate_system, None, "the layer was looked at once, by its own line");
    std::fs::remove_dir_all(&dir).ok();
}

/// A Swiss raster added to the map with its pixels still to come, as one read in place is, by a
/// step adding data; its pixels, to land later.
fn waiting_raster(s: &mut Session) -> Option<std::sync::Arc<rustgis_data::raster::Raster>> {
    let gt = rustgis_data::raster::GeoTransform { origin_x: 2_600_000.0, origin_y: 1_200_000.0, pixel_width: 10.0, pixel_height: -10.0 };
    let r = rustgis_data::raster::Raster::new(
        "dem",
        2,
        2,
        vec![vec![1.0; 4]],
        gt,
        LV95,
        None,
        rustgis_data::raster::ColorHint::Gray,
        "Float32",
    )
    .unwrap();
    let spec = *s.registry.get("layer.addData").unwrap();
    let watch = crate::cmd::project::first_layer_watch(s, &spec).unwrap();
    let mut layer = rustgis_map::RasterLayer::from_path(r, "dem.tif");
    let pixels = layer.data.take();
    s.map_mut().add_raster("dem", layer);
    crate::cmd::project::take_first_layer_system(s, &watch);
    pixels
}

fn land(s: &mut Session, pixels: Option<std::sync::Arc<rustgis_data::raster::Raster>>) {
    let id = s.map().layer_by_name("dem").unwrap().id;
    s.map_mut().layer_mut(id).unwrap().raster.as_mut().unwrap().data = pixels;
}

#[test]
fn a_waiting_raster_taken_by_a_step_undone_or_rolled_back_is_still_to_be_found() {
    let mut s = Session::new(Project::default());
    s.run("project.coordinateSystem", &json!({"fromFirstLayer": true})).unwrap();
    let pixels = waiting_raster(&mut s);
    land(&mut s, pixels);
    // A script whose first line takes it, failing later, gives the project back as it was.
    let script = format!("project.rename {}\nlayer.remove {}", json!({"name": "A"}), json!({"layer": "No such layer"}));
    assert!(s.run("script.run", &json!({"script": script})).is_err());
    assert_eq!(s.project.coordinate_system, None);
    // An edit takes it; Undo gives it back, and the next edit takes it again.
    s.run("project.rename", &json!({"name": "B"})).unwrap();
    assert_eq!(s.project.coordinate_system, Some(LV95));
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(s.project.coordinate_system, None);
    s.run("project.rename", &json!({"name": "C"})).unwrap();
    assert_eq!(s.project.coordinate_system, Some(LV95));
}

#[test]
fn a_raster_already_in_the_project_isnt_added_by_the_next_data() {
    let mut s = Session::new(Project::default());
    // In the project before the option: its pixels still to come.
    let gt = rustgis_data::raster::GeoTransform { origin_x: 2_600_000.0, origin_y: 1_200_000.0, pixel_width: 10.0, pixel_height: -10.0 };
    let r = rustgis_data::raster::Raster::new(
        "old",
        2,
        2,
        vec![vec![1.0; 4]],
        gt,
        LV95,
        None,
        rustgis_data::raster::ColorHint::Gray,
        "Float32",
    )
    .unwrap();
    let mut layer = rustgis_map::RasterLayer::from_path(r, "old.tif");
    let pixels = layer.data.take();
    let id = s.map_mut().add_raster("old", layer);
    s.run("project.coordinateSystem", &json!({"fromFirstLayer": true})).unwrap();
    let point = r#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[7.4,46.9]}}]}"#;
    s.run("layer.addDataText", &json!({"fileName": "p.geojson", "text": point})).unwrap();
    s.map_mut().layer_mut(id).unwrap().raster.as_mut().unwrap().data = pixels;
    s.run("project.rename", &json!({"name": "Later"})).unwrap();
    assert_eq!(s.project.coordinate_system, None);
}

#[test]
fn a_raster_waiting_when_a_system_is_chosen_or_the_option_turned_off_gives_none_later() {
    for settle in [json!({"crs": "EPSG:32632"}), json!({"fromFirstLayer": false})] {
        let mut s = Session::new(Project::default());
        s.run("project.coordinateSystem", &json!({"fromFirstLayer": true})).unwrap();
        let pixels = waiting_raster(&mut s);
        s.run("project.coordinateSystem", &settle).unwrap();
        land(&mut s, pixels);
        s.run("project.coordinateSystem", &json!({"crs": null, "fromFirstLayer": true})).unwrap();
        s.run("project.rename", &json!({"name": "Later"})).unwrap();
        assert_eq!(s.project.coordinate_system, None, "{settle}");
    }
}
