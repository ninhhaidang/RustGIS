use super::common::*;
use rustgis_engine::{
    Session,
    data::{Coord, Dataset, Geometry, GeometryType},
    map::RasterRenderer,
};
use serde_json::json;

fn reject_source(s: &mut Session, id: u32, path: &str, expected: &str) {
    let before = serde_json::to_value(s.map().layer(id).unwrap()).unwrap();
    let error = s.run("layer.repairSource", &json!({"layer": id, "path": path})).unwrap_err();
    assert!(error.contains(expected), "{error}");
    assert_eq!(serde_json::to_value(s.map().layer(id).unwrap()).unwrap(), before, "{error}");
}

#[test]
fn incompatible_vector_repairs_leave_the_layer_unchanged() {
    let f = Fixture::new("repair-vector-rejections");
    let cities = f.file("cities.fgb");
    let countries = f.file("countries.fgb");
    let bare_points = f.file("bare-points.fgb");
    sample_file(&cities, "Cities");
    sample_file(&countries, "Countries");
    let mut points = Dataset::new("Bare points", GeometryType::Point, vec![]);
    points.push(Some(Geometry::Point(Coord::new(2.35, 48.85))), vec![]);
    rustgis_engine::data::io::write_path(&points, &bare_points).unwrap();

    let mut s = session();
    let id = add_stream(&mut s, &cities, None);
    assert_eq!(s.map().layer(id).unwrap().dataset.geometry_type, GeometryType::Point);
    reject_source(&mut s, id, countries.to_str().unwrap(), "Polygon");

    let field = s.map().layer(id).unwrap().dataset.fields[0].name.clone();
    s.run("layer.symbology", &json!({"layer": id, "type": "unique", "field": field})).unwrap();
    assert_eq!(s.map().layer(id).unwrap().renderer.field(), Some(field.as_str()));
    reject_source(&mut s, id, bare_points.to_str().unwrap(), &field);
    reject_source(&mut s, id, "https://example.invalid/cities.fgb", "give a file or folder on this computer");
    reject_source(&mut s, id, f.file("not-there.fgb").to_str().unwrap(), "no such file or folder");
}

#[test]
fn repair_refuses_a_raster_that_cannot_draw_the_saved_rgb_bands() {
    let f = Fixture::new("repair-raster-rejection");
    let rgb = f.file("rgb.tif");
    let gray = f.file("gray.tif");
    raster_file(&rgb, 3);
    raster_file(&gray, 1);
    let mut s = session();
    s.run("layer.addData", &json!({"path": rgb})).unwrap();
    let id = s.active_layer.unwrap();
    let raster = s.map_mut().layer_mut(id).unwrap().raster.as_mut().unwrap();
    assert_eq!(raster.data.as_ref().unwrap().band_count(), 3);
    raster.style.renderer = RasterRenderer::Rgb { red: 2, green: 1, blue: 0 };
    reject_source(&mut s, id, gray.to_str().unwrap(), "band");
    assert_eq!(s.map().layer(id).unwrap().raster.as_ref().unwrap().data.as_ref().unwrap().band_count(), 3);
}

#[test]
fn renamed_raster_repairs_with_its_renderer_and_data_intact() {
    let f = Fixture::new("repair-raster");
    let original = f.file("data/original.tif");
    let renamed = f.file("data/renamed.tif");
    let project = f.file("map.rgis");
    raster_file(&original, 3);
    let mut s = session();
    s.run("layer.addData", &json!({"path": original})).unwrap();
    let id = s.active_layer.unwrap();
    s.map_mut().layer_mut(id).unwrap().raster.as_mut().unwrap().style.renderer = RasterRenderer::Rgb { red: 2, green: 0, blue: 1 };
    let style = serde_json::to_value(&s.map().layer(id).unwrap().raster.as_ref().unwrap().style).unwrap();
    s.run("project.saveAs", &json!({"path": project})).unwrap();
    drop(s);
    std::fs::rename(&original, &renamed).unwrap();
    let mut s = session();
    s.run("project.open", &json!({"path": project})).unwrap();
    let raster = s.map().layer(id).unwrap().raster.as_ref().unwrap();
    assert!(raster.error.is_some());
    assert!(raster.data.is_none());

    let repaired = s.run("layer.repairSource", &json!({"layer": id, "path": renamed})).unwrap();
    assert_eq!(repaired["layer"], id);
    let raster = s.map().layer(id).unwrap().raster.as_ref().unwrap();
    assert_eq!(raster.path.as_deref(), renamed.to_str());
    assert!(raster.error.is_none());
    assert_eq!(raster.data.as_ref().unwrap().band_count(), 3);
    assert_eq!(serde_json::to_value(&raster.style).unwrap(), style);
    assert!(s.dirty());
}

#[test]
fn renamed_tile_archive_is_kept_unavailable_until_repaired() {
    let f = Fixture::new("repair-tiles");
    let original = f.file("tiles/original.pmtiles");
    let renamed = f.file("tiles/renamed.pmtiles");
    let project = f.file("map.rgis");
    archive_file(&original);
    let mut s = session();
    s.run("layer.addData", &json!({"path": original})).unwrap();
    assert_eq!(s.map().tiles.len(), 1);
    s.run("project.saveAs", &json!({"path": project})).unwrap();
    drop(s);
    std::fs::rename(&original, &renamed).unwrap();
    let mut s = session();
    s.run("project.open", &json!({"path": project})).unwrap();
    assert_eq!(s.map().tiles.len(), 1);
    assert!(s.map().tiles[0].error.is_some());
    assert!(!s.map().tiles[0].draws());
    let listed = s.run("map.tiles", &json!({})).unwrap();
    assert!(listed[0]["error"].is_string());

    s.run("map.repairTiles", &json!({"index": 0, "path": renamed})).unwrap();
    assert_eq!(s.map().tiles[0].url, renamed.to_str().unwrap());
    assert!(s.map().tiles[0].error.is_none());
    assert!(s.map().tiles[0].draws());
    let listed = s.run("map.tiles", &json!({})).unwrap();
    assert_eq!(listed[0].get("error"), Some(&serde_json::Value::Null));

    s.run("map.addTiles", &json!({"name": "Web tiles", "url": "https://example.invalid/{z}/{x}/{y}.png"})).unwrap();
    let index = s.map().tiles.iter().position(|t| t.name == "Web tiles").unwrap();
    let before = s.map().tiles[index].clone();
    let error = s.run("map.repairTiles", &json!({"index": index, "path": renamed})).unwrap_err();
    assert!(error.contains("only a tile archive read from a file"), "{error}");
    assert_eq!(s.map().tiles[index], before);
}

#[test]
fn vector_tile_layers_refuse_a_raster_archive_without_changing_the_layer() {
    use rustgis_engine::render::rustgis_tiles::pmtiles;

    let f = Fixture::new("repair-vector-tiles-rejection");
    let vector = f.file("vector.pmtiles");
    let raster = f.file("raster.pmtiles");
    archive_file(&vector);
    let png = tiny_skia::Pixmap::new(1, 1).unwrap().encode_png().unwrap();
    let archive = pmtiles::NewArchive {
        tile_type: pmtiles::TileType::Png,
        gzipped_tiles: false,
        bounds: [-180.0, -85.0, 180.0, 85.0],
        metadata: json!({}),
    };
    std::fs::write(&raster, pmtiles::write(vec![(0, 0, 0, png)], &archive).unwrap()).unwrap();
    let mut s = session();
    s.run("layer.addData", &json!({"path": vector})).unwrap();
    s.map_mut().tiles[0].layers = vec!["parks".into()];
    let before = s.map().tiles[0].clone();
    let error = s.run("map.repairTiles", &json!({"index": 0, "path": raster})).unwrap_err();
    assert!(error.contains("raster tiles"), "{error}");
    assert_eq!(s.map().tiles[0], before);
}

#[test]
fn geopackage_repair_preserves_or_explicitly_selects_the_table() {
    let f = Fixture::new("repair-gpkg");
    let original = f.file("data/original.gpkg");
    let renamed = f.file("data/renamed.gpkg");
    let only_a = f.file("data/only-a.gpkg");
    let project = f.file("map.rgis");
    gpkg_file(&original, &["a", "b"]);
    gpkg_file(&only_a, &["a"]);
    let mut s = session();
    let id = add_stream(&mut s, &original, Some("b"));
    assert_eq!(s.map().layer(id).unwrap().linked_table(), Some("b"));
    s.run("project.saveAs", &json!({"path": project})).unwrap();
    // Closing the session releases the source and undo snapshots before moving the file.
    drop(s);
    std::fs::rename(&original, &renamed).unwrap();
    let mut s = session();
    s.run("project.open", &json!({"path": project})).unwrap();
    assert!(s.map().layer(id).unwrap().source.as_ref().unwrap().error.is_some());

    let repaired = s.run("layer.repairSource", &json!({"layer": id, "path": renamed})).unwrap();
    assert_eq!(repaired["table"], "b");
    let layer = s.map().layer(id).unwrap();
    assert_eq!(layer.linked_table(), Some("b"));
    assert_eq!(layer.linked_file(), renamed.to_str());
    assert!(layer.source.as_ref().unwrap().is_open());
    assert!(layer.source.as_ref().unwrap().error.is_none());
    reject_source(&mut s, id, only_a.to_str().unwrap(), "has no table 'b'");

    // Older hand-written projects can link a container without naming a table.
    s.map_mut().layer_mut(id).unwrap().source.as_mut().unwrap().table = None;
    assert_eq!(s.map().layer(id).unwrap().linked_file(), renamed.to_str());
    reject_source(&mut s, id, renamed.to_str().unwrap(), "several tables");
    let repaired = s.run("layer.repairSource", &json!({"layer": id, "path": renamed, "table": "a"})).unwrap();
    assert_eq!(repaired["table"], "a");
    let layer = s.map().layer(id).unwrap();
    assert_eq!(layer.linked_table(), Some("a"));
    assert_eq!(layer.linked_file(), renamed.to_str());
    assert!(layer.source.as_ref().unwrap().is_open());
    assert!(layer.source.as_ref().unwrap().error.is_none());
}

#[test]
fn colormap_repair_refuses_a_raster_without_a_palette_without_losing_style_or_data() {
    use rustgis_engine::{data::raster::ColorHint, map::RasterLayer};
    use std::sync::Arc;

    let f = Fixture::new("repair-colormap-rejection");
    let original = f.file("original.tif");
    let gray = f.file("gray.tif");
    raster_file(&original, 1);
    raster_file(&gray, 1);
    let mut s = session();
    s.run("layer.addData", &json!({"path": original})).unwrap();
    let id = s.active_layer.unwrap();
    // The GeoTIFF exporter writes band values, not palettes; supply the original decoded
    // palette in memory while keeping a real linked file for the repair command.
    let mut pixels = rustgis_engine::data::raster::read_path(&original).unwrap();
    pixels.color = ColorHint::Palette(vec![[0, 0, 0, 255], [30, 120, 240, 255]]);
    let mut raster = RasterLayer::from_path(pixels, original.to_str().unwrap());
    assert!(matches!(raster.style.renderer, RasterRenderer::Colormap));
    raster.style.mask.values = vec![0.0];
    raster.style.mask.min = Some(1.0);
    raster.style.mask.max = Some(12.0);
    raster.style.mask.inside = true;
    raster.style.brightness = 17.0;
    raster.style.contrast = -23.0;
    raster.style.gamma = 1.8;
    raster.style.rotation = 29.0;
    raster.style.dra = true;
    let data = Arc::clone(raster.data.as_ref().unwrap());
    let style = serde_json::to_value(&raster.style).unwrap();
    s.map_mut().layer_mut(id).unwrap().raster = Some(raster);

    reject_source(&mut s, id, gray.to_str().unwrap(), "choose a raster with a color palette");
    let raster = s.map().layer(id).unwrap().raster.as_ref().unwrap();
    assert_eq!(serde_json::to_value(&raster.style).unwrap(), style);
    assert!(Arc::ptr_eq(raster.data.as_ref().unwrap(), &data));
    assert!(matches!(raster.data.as_ref().unwrap().color, ColorHint::Palette(_)));
}

#[test]
fn compatible_raster_repair_keeps_nondefault_display_settings() {
    let f = Fixture::new("repair-raster-display-settings");
    let original = f.file("original.tif");
    let replacement = f.file("replacement.tif");
    raster_file(&original, 3);
    raster_file(&replacement, 3);
    let mut s = session();
    s.run("layer.addData", &json!({"path": original})).unwrap();
    let id = s.active_layer.unwrap();
    let raster = s.map_mut().layer_mut(id).unwrap().raster.as_mut().unwrap();
    raster.style.renderer = RasterRenderer::Rgb { red: 2, green: 0, blue: 1 };
    raster.style.mask.values = vec![0.0, 2.0];
    raster.style.mask.min = Some(3.0);
    raster.style.mask.max = Some(12.0);
    raster.style.mask.inside = true;
    raster.style.brightness = 17.0;
    raster.style.contrast = -23.0;
    raster.style.gamma = 1.8;
    raster.style.rotation = 29.0;
    raster.style.dra = true;
    raster.style.resampling = rustgis_engine::map::Resampling::Bilinear;
    let style = serde_json::to_value(&raster.style).unwrap();

    s.run("layer.repairSource", &json!({"layer": id, "path": replacement})).unwrap();
    let raster = s.map().layer(id).unwrap().raster.as_ref().unwrap();
    assert_eq!(raster.path.as_deref(), replacement.to_str());
    assert_eq!(serde_json::to_value(&raster.style).unwrap(), style);
    assert_eq!(raster.data.as_ref().unwrap().band_count(), 3);
    assert!(raster.error.is_none());
}

/// Replacing data must not transfer a saved label move to a different feature with the same id.
#[test]
fn replacing_source_resets_saved_label_moves_with_notice_and_undo() {
    let f = Fixture::new("repair-label-moves");
    let original = f.file("original.fgb");
    let replacement = f.file("replacement.fgb");
    sample_file(&original, "Cities");
    sample_file(&replacement, "Cities");
    let mut s = session();
    let id = add_stream(&mut s, &original, None);
    let fid = 1;
    let l = s.map_mut().layer_mut(id).unwrap();
    l.labels.default.moved.push(rustgis_engine::map::symbology::MovedLabel { id: fid, at: [12.0, 34.0] });
    s.map_mut().label_lock = Some(vec![((id, fid, 0, 0), rustgis_engine::map::LockedPlace { place: 0, callout: Some([20.0, 10.0]) })]);
    let mark = s.notice_mark();
    s.run("layer.repairSource", &json!({"layer": id, "path": replacement})).unwrap();
    assert!(s.map().layer(id).unwrap().labels.default.moved.is_empty());
    assert!(s.map().label_lock.is_none(), "old callout locks must not attach to replacement ids");
    let notices = s.notices_since(mark);
    let name = &s.map().layer(id).unwrap().name;
    assert!(notices.iter().any(|n| n.text.contains("1 saved label move") && n.text.contains(name)));
    assert!(notices.iter().any(|n| {
        n.text.contains("1 locked label") && n.text.contains("0 line anchor") && n.text.contains(&s.map().name) && n.text.contains(name)
    }));
    s.run("edit.undo", &json!({})).unwrap();
    assert_eq!(s.map().layer(id).unwrap().labels.default.moved[0].at, [12.0, 34.0]);
    s.run("edit.redo", &json!({})).unwrap();
    let mark = s.notice_mark();
    s.run("layer.repairSource", &json!({"layer": id, "path": original})).unwrap();
    assert!(s.notices_since(mark).is_empty(), "no moves means no loss notice");
}
