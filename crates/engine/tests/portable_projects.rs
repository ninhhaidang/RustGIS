//! Local links are relative on disk, absolute in memory, and repairable without losing configuration.

#[path = "portable_projects/common.rs"]
mod common;
#[path = "portable_projects/reload.rs"]
mod reload;
#[path = "portable_projects/reload_tiles.rs"]
mod reload_tiles;
#[path = "portable_projects/repair.rs"]
mod repair;

use common::*;
use rustgis_engine::{Project, Session};
use serde_json::{Value, json};
use std::path::Path;

const CITIES: &str = "survey/data/cities.fgb";
const COUNTRIES: &str = "shared/countries.fgb";
const DEM: &str = "survey/data/my dem ü.tif";
const TILES: &str = "survey/tiles/t.pmtiles";
const LINKS: [(&str, &str); 4] =
    [("./data/cities.fgb", CITIES), ("../shared/countries.fgb", COUNTRIES), ("./data/my dem ü.tif", DEM), ("./tiles/t.pmtiles", TILES)];

struct Layers {
    cities: u32,
    countries: u32,
    dem: u32,
}

fn populated(f: &Fixture) -> (Session, Layers) {
    sample_file(&f.file(CITIES), "Cities");
    sample_file(&f.file(COUNTRIES), "Countries");
    raster_file(&f.file(DEM), 1);
    archive_file(&f.file(TILES));
    let mut s = session();
    let cities = add_stream(&mut s, &f.file(CITIES), None);
    let countries = add_stream(&mut s, &f.file(COUNTRIES), None);
    let dem = s.run("layer.addData", &json!({"path": f.file(DEM)})).unwrap()["layer"].as_u64().unwrap() as u32;
    s.run("layer.addData", &json!({"path": f.file(TILES)})).unwrap();
    s.run("layer.symbology", &json!({"layer": cities, "type": "unique", "field": "adm0name"})).unwrap();
    let l = s.map_mut().layer_mut(cities).unwrap();
    l.labels.enabled = true;
    l.labels.default.field = "name".into();
    (s, Layers { cities, countries, dem })
}

fn paths(s: &mut Session) -> Vec<(String, bool)> {
    s.map_mut().file_paths_mut().into_iter().map(|(path, record)| (path.clone(), record)).collect()
}

fn configuration(s: &Session) -> Vec<(u32, Value, Value)> {
    s.map().layers.iter().map(|l| (l.id, serde_json::to_value(&l.renderer).unwrap(), serde_json::to_value(&l.labels).unwrap())).collect()
}

fn assert_stream(s: &Session, id: u32, path: &Path) {
    let l = s.map().layer(id).unwrap();
    let src = l.source.as_ref().unwrap();
    assert_eq!(src.error, None, "{}", l.name);
    assert!(src.is_open(), "{} is not open", l.name);
    assert_eq!(src.path.as_deref(), path.to_str());
    assert_eq!(l.linked_file(), path.to_str());
    assert_eq!(l.dataset.source.as_deref(), path.to_str());
}

fn assert_other_sources(s: &Session, layers: &Layers, root: &Path) {
    assert_stream(s, layers.countries, &native_join(root, COUNTRIES));
    let l = s.map().layer(layers.dem).unwrap();
    let raster = l.raster.as_ref().unwrap();
    assert_eq!(raster.error, None);
    assert!(raster.data.is_some());
    assert_eq!(raster.path.as_deref(), native_join(root, DEM).to_str());
    assert_eq!(l.linked_file(), native_join(root, DEM).to_str());
    assert_eq!(s.map().tiles.len(), 1);
    let tile = &s.map().tiles[0];
    assert_eq!(tile.error, None);
    assert_eq!(tile.url, native_join(root, TILES).to_str().unwrap());
    assert!(tile.draws());
}

fn assert_all_sources(s: &Session, layers: &Layers, root: &Path) {
    assert_eq!(s.map().layers.len(), 3);
    assert_stream(s, layers.cities, &native_join(root, CITIES));
    assert_other_sources(s, layers, root);
}

fn assert_no_root(text: &str, root: &Path) {
    let serialized = serde_json::to_string(root).unwrap();
    assert!(!text.contains(&serialized[1..serialized.len() - 1]), "saved links must not retain the old root");
}

fn save(s: &mut Session, f: &Fixture) -> String {
    let before = paths(s);
    assert!(before.iter().all(|(path, _)| Path::new(path).is_absolute()));
    s.run("project.saveAs", &json!({"path": f.file("survey/map.rgis")})).unwrap();
    assert_eq!(paths(s), before, "saving must not replace live absolute paths");
    let text = std::fs::read_to_string(f.file("survey/map.rgis")).unwrap();
    for (relative, _) in LINKS {
        assert!(text.contains(relative), "missing {relative}");
    }
    assert_no_root(&text, &f.root);
    text
}

fn open(path: &Path) -> Session {
    let mut s = session();
    s.run("project.open", &json!({"path": path})).unwrap();
    s
}

#[test]
fn moving_project_folder_keeps_every_local_source() {
    let mut f = Fixture::new("move");
    let (mut s, layers) = populated(&f);
    assert_all_sources(&s, &layers, &f.root);
    let config = configuration(&s);
    save(&mut s, &f);
    assert_all_sources(&s, &layers, &f.root);
    drop(s);
    let moved = f.root.with_file_name(format!("{}-moved", f.root.file_name().unwrap().to_str().unwrap()));
    std::fs::rename(&f.root, &moved).unwrap();
    f.root = moved;
    let reopened = open(&f.file("survey/map.rgis"));
    assert_all_sources(&reopened, &layers, &f.root);
    assert_eq!(configuration(&reopened), config);
}

#[test]
fn save_as_recomputes_links_without_changing_live_paths_or_original() {
    let f = Fixture::new("save-as");
    let (mut s, layers) = populated(&f);
    let original = save(&mut s, &f);
    let before = paths(&mut s);
    let config = configuration(&s);
    let copy = f.file("other/copy.rgis");
    s.run("project.saveAs", &json!({"path": copy})).unwrap();
    assert_eq!(paths(&mut s), before);
    assert_all_sources(&s, &layers, &f.root);
    let text = std::fs::read_to_string(&copy).unwrap();
    for relative in ["../survey/data/cities.fgb", "../survey/data/my dem ü.tif", "../survey/tiles/t.pmtiles", "../shared/countries.fgb"] {
        assert!(text.contains(relative), "missing {relative}");
    }
    assert_no_root(&text, &f.root);
    assert_eq!(std::fs::read_to_string(f.file("survey/map.rgis")).unwrap(), original);
    for path in [f.file("survey/map.rgis"), copy] {
        let reopened = open(&path);
        assert_all_sources(&reopened, &layers, &f.root);
        assert_eq!(configuration(&reopened), config);
    }
}

#[test]
fn absolute_path_projects_open_and_save_relative_without_restyling() {
    let f = Fixture::new("absolute");
    let (mut s, layers) = populated(&f);
    let config = configuration(&s);
    let mut text = save(&mut s, &f);
    // Hand-write the old format using absolute strings, including record-only provenance.
    for (relative, file) in LINKS {
        text = text.replace(&serde_json::to_string(relative).unwrap(), &serde_json::to_string(&native_join(&f.root, file)).unwrap());
    }
    let path = f.file("survey/absolute.rgis");
    std::fs::write(&path, text).unwrap();
    let mut reopened = open(&path);
    assert_all_sources(&reopened, &layers, &f.root);
    assert_eq!(configuration(&reopened), config);
    let before = paths(&mut reopened);
    reopened.run("project.save", &Value::Null).unwrap();
    assert_eq!(paths(&mut reopened), before);
    let saved = std::fs::read_to_string(&path).unwrap();
    for (relative, _) in LINKS {
        assert!(saved.contains(relative), "missing {relative}");
    }
    assert_no_root(&saved, &f.root);
    let again = open(&path);
    assert_all_sources(&again, &layers, &f.root);
    assert_eq!(configuration(&again), config);
}

#[test]
fn missing_stream_can_be_repaired_undone_redone_and_saved() {
    let f = Fixture::new("repair-stream");
    let (mut s, layers) = populated(&f);
    save(&mut s, &f);
    drop(s);
    let old = f.file(CITIES);
    let replacement = f.file("survey/data/towns.fgb");
    std::fs::rename(&old, &replacement).unwrap();
    let project = f.file("survey/map.rgis");
    let mut s = open(&project);
    assert!(!s.dirty());
    let l = s.map().layer(layers.cities).unwrap();
    let name = l.name.clone();
    let error = l.source.as_ref().unwrap().error.clone();
    assert!(error.is_some());
    assert!(!l.source.as_ref().unwrap().is_open());
    let config = configuration(&s);
    let order: Vec<_> = s.map().layers.iter().map(|l| l.id).collect();
    assert_other_sources(&s, &layers, &f.root);
    let result = s.run("layer.repairSource", &json!({"layer": layers.cities, "path": replacement})).unwrap();
    assert_eq!(result["layer"], layers.cities);
    assert_eq!(result["path"], replacement.to_str().unwrap());
    assert!(s.dirty());
    assert_stream(&s, layers.cities, &replacement);
    assert_eq!(s.map().layer(layers.cities).unwrap().name, name);
    assert_eq!(configuration(&s), config);
    assert_eq!(s.map().layers.iter().map(|l| l.id).collect::<Vec<_>>(), order);
    s.run("edit.undo", &Value::Null).unwrap();
    let l = s.map().layer(layers.cities).unwrap();
    assert_eq!(l.linked_file(), old.to_str());
    assert_eq!(l.source.as_ref().unwrap().error, error);
    assert!(!l.source.as_ref().unwrap().is_open());
    assert_eq!(configuration(&s), config);
    s.run("edit.redo", &Value::Null).unwrap();
    assert_stream(&s, layers.cities, &replacement);
    assert_eq!(configuration(&s), config);
    assert_eq!(s.map().layers.iter().map(|l| l.id).collect::<Vec<_>>(), order);
    let before = paths(&mut s);
    s.run("project.save", &Value::Null).unwrap();
    assert_eq!(paths(&mut s), before);
    let text = std::fs::read_to_string(&project).unwrap();
    assert!(text.contains("./data/towns.fgb"));
    assert!(!text.contains("./data/cities.fgb"));
    let reopened = open(&project);
    assert_stream(&reopened, layers.cities, &replacement);
    assert_other_sources(&reopened, &layers, &f.root);
    assert_eq!(configuration(&reopened), config);
}

#[test]
fn opening_relative_project_text_keeps_unavailable_sources_without_reading_them() {
    let f = Fixture::new("text");
    let (mut s, layers) = populated(&f);
    let text = save(&mut s, &f);
    let mut reopened = session();
    reopened.run("project.openText", &json!({"text": text})).unwrap();
    assert!(reopened.path.is_none());
    assert_eq!(reopened.map().layers.len(), 3);
    for (id, path) in [(layers.cities, "./data/cities.fgb"), (layers.countries, "../shared/countries.fgb")] {
        let l = reopened.map().layer(id).unwrap();
        let src = l.source.as_ref().unwrap();
        assert!(!src.is_open());
        assert_eq!(l.linked_file(), Some(path));
        assert!(src.error.as_ref().unwrap().contains("relative to the project's folder"));
    }
    let raster = reopened.map().layer(layers.dem).unwrap().raster.as_ref().unwrap();
    assert!(raster.data.is_none());
    assert_eq!(raster.path.as_deref(), Some("./data/my dem ü.tif"));
    assert!(raster.error.as_ref().unwrap().contains("relative to the project's folder"));
    assert_eq!(reopened.map().tiles.len(), 1);
    let tile = &reopened.map().tiles[0];
    assert_eq!(tile.url, "./tiles/t.pmtiles");
    assert!(tile.error.as_ref().unwrap().contains("relative to the project's folder"));
    assert!(!tile.draws());
}

#[test]
fn overly_long_relative_spelling_preserves_absolute_provenance_and_restoration_order() {
    use rustgis_engine::{
        data::{Dataset, GeometryType},
        map::Layer,
    };
    let dir = format!("/{}", "a/".repeat(1500));
    let first = format!("/{}cities.fgb", "b/".repeat(1500));
    let second = format!("{dir}countries.fgb");
    assert!(dir.len() < 4096 && first.len() < 4096 && second.len() < 4096);
    let mut p = Project::default();
    for (id, source) in [(1, &first), (2, &second)] {
        let mut dataset = Dataset::new(format!("layer-{id}"), GeometryType::Point, Vec::new());
        dataset.source = Some(source.clone());
        p.maps[0].layers.push(Layer::new(id, dataset));
    }
    let (text, _) = p.saved_text(Some(Path::new(&dir))).unwrap();
    assert_eq!(p.maps[0].layers[0].dataset.source.as_deref(), Some(first.as_str()));
    assert_eq!(p.maps[0].layers[1].dataset.source.as_deref(), Some(second.as_str()));
    let saved: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["maps"][0]["layers"][0]["dataset"]["source"], first);
    assert_eq!(saved["maps"][0]["layers"][1]["dataset"]["source"], "./countries.fgb");
}

#[test]
fn imported_map_resolves_bare_relative_links_against_its_folder() {
    let f = Fixture::new("import-map");
    let (mut s, layers) = populated(&f);
    let text = save(&mut s, &f);
    let mut saved: Value = serde_json::from_str(&text).unwrap();
    let map = &mut saved["maps"][0];
    for layer in map["layers"].as_array_mut().unwrap() {
        for key in ["source", "raster"] {
            if let Some(path) = layer.get_mut(key).and_then(|source| source.get_mut("path"))
                && let Some(bare) = path.as_str().and_then(|p| p.strip_prefix("./"))
            {
                *path = json!(bare);
            }
        }
    }
    for tile in map["tiles"].as_array_mut().unwrap() {
        if let Some(bare) = tile["url"].as_str().and_then(|p| p.strip_prefix("./")) {
            tile["url"] = json!(bare);
        }
    }
    let path = f.file("survey/import.rgismap");
    std::fs::write(&path, serde_json::to_vec(map).unwrap()).unwrap();
    let mut imported = session();
    imported.run("map.import", &json!({"path": path})).unwrap();
    assert_all_sources(&imported, &layers, &f.root);
}

#[test]
fn repair_source_accepts_a_local_file_uri() {
    let f = Fixture::new("repair-file-uri");
    let original = f.file("cities.fgb");
    let replacement = f.file("data/renamed cities.fgb");
    sample_file(&original, "Cities");
    std::fs::copy(&original, &replacement).unwrap();
    let mut s = session();
    let id = add_stream(&mut s, &original, None);
    let path = replacement.to_str().unwrap();
    let uri = if cfg!(windows) { format!("file:///{}", path.replace('\\', "/")) } else { format!("file://{path}") };
    s.run("layer.repairSource", &json!({"layer": id, "path": uri})).unwrap();
    assert_stream(&s, id, &replacement);
}
