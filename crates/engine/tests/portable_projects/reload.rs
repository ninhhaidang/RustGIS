use super::common::*;
use rustgis_engine::{
    Session,
    data::{
        Coord, Dataset, Field, FieldType, Geometry, GeometryType, Value, io,
        raster::{ColorHint, Crs, geotiff},
    },
    map::{RasterLayer, RasterRenderer, symbology::MovedLabel},
};
use serde_json::json;
use std::{path::Path, sync::Arc};

fn points(fields: &[&str], x: Option<f64>) -> Dataset {
    let mut ds = Dataset::new("Points", GeometryType::Point, fields.iter().map(|n| Field::new(*n, FieldType::Integer)).collect());
    if let Some(x) = x {
        ds.push(Some(Geometry::Point(Coord::new(x, 48.0))), fields.iter().map(|_| Value::Int(7)).collect());
    }
    ds
}

// Atomically publish a complete replacement rather than exposing a partially written source.
// A GeoPackage is updated in place, in one transaction, as GIS tools do: the open layer holds the
// file, which Windows won't let another file be renamed over.
fn replace_points(path: &Path, ds: &Dataset) {
    if path.extension().is_some_and(|e| e == "gpkg") {
        let target = io::GpkgTable { mode: io::GpkgMode::ReplaceTable, name: Some(ds.name.clone()) };
        rustgis_engine::data::source::gpkg::write_table_in(ds, path, Crs::LonLat, &target).unwrap();
        return;
    }
    let next = path.with_file_name("replacement").with_extension(path.extension().unwrap());
    io::write_path(ds, &next).unwrap();
    std::fs::rename(next, path).unwrap();
}

fn loaded(s: &Session, id: u32) -> Dataset {
    s.map().layer(id).unwrap().load_all().unwrap()
}

fn first_point(ds: &Dataset) -> Coord {
    match ds.features[0].geometry.as_ref().unwrap() {
        Geometry::Point(at) => *at,
        other => panic!("expected fixture point, got {other:?}"),
    }
}

fn reject_reload(s: &mut Session, id: u32, expected: &str) {
    let source = s.map().layer(id).unwrap().source.clone();
    let before = serde_json::to_value(s.map().layer(id).unwrap()).unwrap();
    let mark = s.notice_mark();
    let error = s.run("layer.reloadData", &json!({"layer": id})).unwrap_err();
    assert!(error.to_lowercase().contains(&expected.to_lowercase()), "{error}");
    assert_eq!(serde_json::to_value(s.map().layer(id).unwrap()).unwrap(), before);
    if let Some(source) = source {
        assert!(s.map().layer(id).unwrap().source.as_ref().unwrap().same_opening(&source));
    }
    assert!(s.notices_since(mark).iter().any(|n| n.error && n.text.contains(&error)));
    assert!(s.notices_since(mark).iter().all(|n| n.error), "failed reload must not claim installation or lost moves");
}

#[test]
fn reload_reopens_fgb_preserving_presentation_and_resets_feature_state_with_undo() {
    let f = Fixture::new("reload-fgb");
    let path = f.file("points.fgb");
    io::write_path(&points(&["rank"], Some(2.0)), &path).unwrap();
    let mut s = session();
    let id = add_stream(&mut s, &path, None);
    s.map_mut().layer_mut(id).unwrap().sync_source(&rustgis_engine::geom::Extent::new(-180.0, -80.0, 180.0, 80.0), true);
    assert_eq!(s.map().layer(id).unwrap().source.as_ref().unwrap().reads, 1);
    let other = s.map_mut().add_dataset(points(&[], Some(9.0)));
    s.run("group.add", &json!({"name": "Survey", "layers": [id]})).unwrap();
    s.run("layer.rename", &json!({"layer": id, "name": "Survey points"})).unwrap();
    s.run("layer.symbology", &json!({"layer": id, "type": "unique", "field": "rank"})).unwrap();
    let fid = s.map().layer(id).unwrap().dataset.features[0].id;
    let layer = s.map_mut().layer_mut(id).unwrap();
    layer.visible = false;
    layer.transparency = 37.0;
    layer.min_scale = Some(500_000.0);
    layer.max_scale = Some(100.0);
    layer.labels.enabled = true;
    layer.labels.default.field = "rank".into();
    layer.labels.default.size = 17.0;
    layer.labels.default.moved.push(MovedLabel { id: fid, at: [12.0, 34.0] });
    layer.selection = vec![0];
    let renderer = serde_json::to_value(&layer.renderer).unwrap();
    let mut labels = layer.labels.clone();
    labels.default.moved.clear();
    s.map_mut().camera.res = 1234.0;
    s.map_mut().label_lock = Some(vec![((id, fid, 0, 0), rustgis_engine::map::LockedPlace { place: 0, callout: None })]);
    let camera = serde_json::to_value(s.map().camera).unwrap();
    let groups = serde_json::to_value(&s.map().groups).unwrap();
    let order = s.map().layers.iter().map(|l| l.id).collect::<Vec<_>>();
    s.active_layer = Some(id);
    assert!(order.contains(&other));
    let signature = std::fs::metadata(&path).unwrap();
    replace_points(&path, &points(&["rank"], Some(8.0)));
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(signature.modified().unwrap()))
        .unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().len(), signature.len());
    let mark = s.notice_mark();
    let result = s.run("layer.reloadData", &json!({})).unwrap();
    assert_eq!(result, json!({"layer": id, "path": path, "table": null}));
    assert_eq!(first_point(&loaded(&s, id)), Coord::new(8.0, 48.0));
    let layer = s.map().layer(id).unwrap();
    assert_eq!(layer.name, "Survey points");
    assert!(!layer.visible);
    assert_eq!(layer.transparency, 37.0);
    assert_eq!((layer.min_scale, layer.max_scale), (Some(500_000.0), Some(100.0)));
    assert_eq!(serde_json::to_value(&layer.renderer).unwrap(), renderer);
    assert_eq!(serde_json::to_value(&layer.labels).unwrap(), serde_json::to_value(labels).unwrap());
    assert!(layer.selection.is_empty());
    assert!(s.map().label_lock.is_none());
    assert_eq!(serde_json::to_value(s.map().camera).unwrap(), camera);
    assert_eq!(serde_json::to_value(&s.map().groups).unwrap(), groups);
    assert_eq!(s.map().layers.iter().map(|l| l.id).collect::<Vec<_>>(), order);
    assert_eq!(s.active_layer, Some(id));
    let notices = s.notices_since(mark);
    assert!(notices.iter().any(|n| n.text.contains("1 saved label move")));
    s.run("edit.undo", &json!({})).unwrap();
    assert_eq!(s.map().layer(id).unwrap().labels.default.moved[0].at, [12.0, 34.0]);
    assert_eq!(first_point(&io::read_path(&path).unwrap().0), Coord::new(8.0, 48.0));
    s.run("edit.redo", &json!({})).unwrap();
    assert_eq!(first_point(&loaded(&s, id)), Coord::new(8.0, 48.0));
    let mark = s.notice_mark();
    s.run("layer.reloadData", &json!({"layer": "Survey points"})).unwrap();
    assert!(s.notices_since(mark).iter().all(|n| !n.text.contains("reset")), "no feature state means no loss notice");
}

#[test]
fn compatible_empty_fgb_is_a_successful_reload() {
    let f = Fixture::new("reload-empty");
    let path = f.file("polygons.fgb");
    sample_file(&path, "Countries");
    let mut s = session();
    let id = add_stream(&mut s, &path, None);
    let fields = s.map().layer(id).unwrap().dataset.fields.clone();
    let field = fields[0].name.clone();
    s.run("layer.symbology", &json!({"layer": id, "type": "unique", "field": field})).unwrap();
    // The built-in point writer uses Unknown to allow Point/MultiPoint mixtures. Polygon
    // output carries a declaration even without rows, so this is genuinely compatible empty data.
    replace_points(&path, &Dataset::new("Empty", GeometryType::Polygon, fields));
    s.run("layer.reloadData", &json!({"layer": id})).unwrap();
    assert!(loaded(&s, id).features.is_empty());
    assert_eq!(s.map().layer(id).unwrap().dataset.geometry_type, GeometryType::Polygon);
    assert_eq!(s.map().layer(id).unwrap().renderer.field(), Some(field.as_str()));
}

#[test]
fn required_style_label_symbol_and_filter_fields_are_checked_before_reload_and_retry_works() {
    for binding in [
        "renderer",
        "label",
        "label expression",
        "disabled label",
        "disabled label expression",
        "hidden class",
        "symbol expression",
        "filter expression",
    ] {
        let f = Fixture::new("reload-fields");
        let path = f.file("points.fgb");
        io::write_path(&points(&["required"], Some(2.0)), &path).unwrap();
        let mut s = session();
        let id = add_stream(&mut s, &path, None);
        if binding == "renderer" {
            s.run("layer.symbology", &json!({"layer": id, "type": "unique", "field": "required"})).unwrap();
        } else {
            let l = s.map_mut().layer_mut(id).unwrap();
            // Isolate each consumer: an incidental default label must not cause the rejection.
            l.labels.default.field.clear();
            match binding {
                "label" => {
                    l.labels.enabled = true;
                    l.labels.default.field = "required".into();
                }
                "label expression" => {
                    l.labels.enabled = true;
                    l.labels.default.by.text = Some("required + 1".into());
                }
                "disabled label" => {
                    l.labels.enabled = false;
                    l.labels.default.field = "required".into();
                }
                "disabled label expression" => {
                    l.labels.enabled = false;
                    l.labels.default.by.text = Some("required + 1".into());
                }
                "hidden class" => {
                    let mut class = rustgis_engine::map::LabelClass::default_for(&l.dataset);
                    class.visible = false;
                    class.field = "required".into();
                    l.labels.classes.push(class);
                }
                "symbol expression" => l.symbol_by.size = Some("required * 2".into()),
                "filter expression" => l.definition_query = Some("required > 0".into()),
                _ => unreachable!(),
            }
        }
        let name = s.map().layer(id).unwrap().name.clone();
        s.run("layer.rename", &json!({"layer": id, "name": "Before failed reload"})).unwrap();
        replace_points(&path, &points(&[], Some(8.0)));
        reject_reload(&mut s, id, "required");
        s.run("edit.undo", &json!({})).unwrap();
        assert_eq!(s.map().layer(id).unwrap().name, name, "a failed reload must not add an undo step: {binding}");
        replace_points(&path, &points(&["required"], Some(8.0)));
        s.run("layer.reloadData", &json!({"layer": id})).unwrap();
        assert_eq!(first_point(&loaded(&s, id)), Coord::new(8.0, 48.0));
    }
}

#[test]
fn numeric_reload_bindings_reject_text_but_accept_numeric_interchange_and_empty_data() {
    let polygons = |x| {
        let mut ds = points(&["rank"], Some(x));
        ds.geometry_type = GeometryType::Polygon;
        ds.features[0].geometry = Some(Geometry::Polygon(vec![vec![vec![
            Coord::new(x, 48.0),
            Coord::new(x + 0.1, 48.0),
            Coord::new(x + 0.1, 48.1),
            Coord::new(x, 48.0),
        ]]]));
        ds
    };
    for binding in ["graduated", "proportional", "rotation", "extrusion", "diagram", "categorical"] {
        let data = |x| if binding == "extrusion" { polygons(x) } else { points(&["rank"], Some(x)) };
        let f = Fixture::new("reload-numeric");
        let path = f.file("points.gpkg");
        io::write_path(&data(2.0), &path).unwrap();
        let mut s = session();
        let id = add_stream(&mut s, &path, None);
        s.map_mut().layer_mut(id).unwrap().sync_source(&rustgis_engine::geom::Extent::new(-180.0, -80.0, 180.0, 80.0), true);
        s.map_mut().layer_mut(id).unwrap().labels.default.field.clear();
        match binding {
            "graduated" | "proportional" | "categorical" => {
                let kind = if binding == "categorical" { "unique" } else { binding };
                s.run("layer.symbology", &json!({"layer": id, "type": kind, "field": "rank"})).unwrap();
            }
            "rotation" => {
                s.map_mut().layer_mut(id).unwrap().marker_rotation = Some(serde_json::from_value(json!({"field": "rank"})).unwrap());
            }
            "extrusion" => {
                s.map_mut().layer_mut(id).unwrap().extrusion = Some(serde_json::from_value(json!({"field": "rank"})).unwrap());
            }
            "diagram" => {
                s.run("layer.diagram", &json!({"layer": id, "kind": "pie", "fields": ["rank"], "size": 12})).unwrap();
            }
            _ => unreachable!(),
        }
        let mut text = data(8.0);
        text.fields[0].ty = FieldType::Text;
        text.features[0].attrs[0] = Value::Text("category".into());
        replace_points(&path, &text);
        if binding == "categorical" {
            s.run("layer.reloadData", &json!({"layer": id})).unwrap();
            assert_eq!(loaded(&s, id).value(0, 0), &Value::Text("category".into()));
        } else {
            reject_reload(&mut s, id, "numeric");
        }
        let mut doubles = data(9.0);
        doubles.fields[0].ty = FieldType::Double;
        doubles.features[0].attrs[0] = Value::Float(7.5);
        replace_points(&path, &doubles);
        s.run("layer.reloadData", &json!({"layer": id})).unwrap();
        assert_eq!(loaded(&s, id).value(0, 0).as_f64(), Some(7.5));
        replace_points(&path, &Dataset::new("Points", doubles.geometry_type, doubles.fields));
        s.run("layer.reloadData", &json!({"layer": id})).unwrap();
        assert!(loaded(&s, id).features.is_empty(), "{binding}");
    }
}

#[test]
fn already_broken_saved_label_configuration_does_not_prevent_reload() {
    let f = Fixture::new("reload-broken-label");
    let path = f.file("points.fgb");
    io::write_path(&points(&["rank"], Some(2.0)), &path).unwrap();
    let mut s = session();
    let id = add_stream(&mut s, &path, None);
    let l = s.map_mut().layer_mut(id).unwrap();
    l.labels.enabled = false;
    l.labels.default.field = "already_missing".into();
    l.labels.default.by.text = Some("also_missing + 1".into());
    replace_points(&path, &points(&["rank"], Some(8.0)));
    s.run("layer.reloadData", &json!({"layer": id})).unwrap();
    assert_eq!(first_point(&loaded(&s, id)), Coord::new(8.0, 48.0));
}

#[test]
fn reload_discloses_whole_map_label_lock_and_anchor_resets_without_target_moves() {
    for (locks, anchors) in [(true, false), (false, true), (true, true), (false, false)] {
        let f = Fixture::new("reload-map-lock");
        let path = f.file("points.fgb");
        io::write_path(&points(&[], Some(2.0)), &path).unwrap();
        let mut s = session();
        let id = add_stream(&mut s, &path, None);
        let other = s.map_mut().add_dataset(points(&[], Some(9.0)));
        let key = (other, 1, 0, 0);
        if locks {
            s.map_mut().label_lock = Some(vec![(key, rustgis_engine::map::LockedPlace { place: 0, callout: None })]);
        }
        if anchors {
            s.map_mut().label_lock_line_anchors.insert(key, Coord::new(9.0, 48.0));
        }
        let map = s.map().name.clone();
        let layer = s.map().layer(id).unwrap().name.clone();
        let mark = s.notice_mark();
        s.run("layer.reloadData", &json!({"layer": id})).unwrap();
        assert!(s.map().label_lock.is_none() && s.map().label_lock_line_anchors.is_empty());
        let notes: Vec<_> = s.notices_since(mark).iter().filter(|n| n.text.contains("across the map")).collect();
        assert_eq!(notes.len(), usize::from(locks || anchors));
        if let Some(note) = notes.first() {
            assert!(note.text.contains(&map) && note.text.contains(&layer), "{}", note.text);
            assert!(note.text.contains(&format!("{} locked label", usize::from(locks))));
            assert!(note.text.contains(&format!("{} line anchor", usize::from(anchors))));
        }
        s.run("edit.undo", &json!({})).unwrap();
        assert_eq!(s.map().label_lock.as_ref().map_or(0, Vec::len), usize::from(locks));
        assert_eq!(s.map().label_lock_line_anchors.len(), usize::from(anchors));
    }
}

#[test]
fn wrong_geometry_missing_file_and_missing_chosen_table_leave_the_layer_unchanged() {
    let f = Fixture::new("reload-source-errors");
    let path = f.file("points.fgb");
    io::write_path(&points(&[], Some(2.0)), &path).unwrap();
    let mut s = session();
    let id = add_stream(&mut s, &path, None);
    let next = f.file("replacement.fgb");
    sample_file(&next, "Countries");
    std::fs::rename(&next, &path).unwrap();
    reject_reload(&mut s, id, "Polygon");
    std::fs::remove_file(&path).unwrap();
    reject_reload(&mut s, id, "file");
    replace_points(&path, &points(&[], Some(8.0)));
    s.run("layer.reloadData", &json!({"layer": id})).unwrap();
    let gpkg = f.file("tables.gpkg");
    gpkg_file(&gpkg, &["a", "b"]);
    let id = add_stream(&mut s, &gpkg, Some("b"));
    let c = rusqlite::Connection::open(&gpkg).unwrap();
    c.execute_batch(
        "DROP TABLE b; DELETE FROM gpkg_contents WHERE table_name = 'b'; DELETE FROM gpkg_geometry_columns WHERE table_name = 'b';",
    )
    .unwrap();
    reject_reload(&mut s, id, "b");
    assert_eq!(s.map().layer(id).unwrap().linked_table(), Some("b"));
}

#[test]
fn editable_gpkg_reload_observes_wal_refuses_every_dirty_kind_and_discards_only_in_memory() {
    for kind in ["insert", "update", "delete"] {
        let f = Fixture::new("reload-gpkg-edits");
        let path = f.file("tables.gpkg");
        gpkg_file(&path, &["a", "b"]);
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
            UPDATE gpkg_contents SET last_change = '2026-01-01T00:00:00.000Z';
            ALTER TABLE b ADD COLUMN name TEXT; UPDATE b SET name = 'original';
            INSERT INTO b (geom, name) SELECT geom, 'second' FROM b WHERE fid = 1;",
        )
        .unwrap();
        let mut s = session();
        let id = s.run("gpkg.addLayer", &json!({"path": path, "table": "b"})).unwrap()["layer"].as_u64().unwrap() as u32;
        match kind {
            "insert" => {
                s.run("edit.addFeature", &json!({"layer": id, "geometry": {"type": "Point", "coordinates": [3.0, 49.0]}, "attributes": {"name": "local insert"}})).unwrap();
            }
            "update" => {
                s.run("edit.setValue", &json!({"layer": id, "row": 0, "field": "name", "value": "local update"})).unwrap();
            }
            "delete" => {
                s.run("select.rows", &json!({"layer": id, "rows": [1]})).unwrap();
                s.run("edit.delete", &json!({})).unwrap();
            }
            _ => unreachable!(),
        }
        let dirty = s.map().layer(id).unwrap().dataset.clone();
        let pending = s.run("gpkg.pendingEdits", &json!({"layer": id})).unwrap();
        assert_eq!(pending[format!("{kind}s")], 1);
        c.execute_batch(
            "UPDATE b SET name = 'external' WHERE fid = 1;
            UPDATE gpkg_contents SET last_change = '2026-01-02T00:00:00.000Z' WHERE table_name = 'b';",
        )
        .unwrap();
        reject_reload(&mut s, id, "discardEdits");
        assert_eq!(s.map().layer(id).unwrap().dataset, dirty);
        let result = s.run("layer.reloadData", &json!({"layer": id, "discardEdits": true})).unwrap();
        assert_eq!(result, json!({"layer": id, "path": path, "table": "b"}));
        let fresh = &s.map().layer(id).unwrap().dataset;
        assert_eq!(fresh.features.len(), 2);
        assert_eq!(fresh.value(0, fresh.field_index("name").unwrap()), &Value::Text("external".into()));
        let pending = s.run("gpkg.pendingEdits", &json!({"layer": id})).unwrap();
        assert_eq!((pending["inserts"].as_u64(), pending["updates"].as_u64(), pending["deletes"].as_u64()), (Some(0), Some(0), Some(0)));
        s.run("edit.undo", &json!({})).unwrap();
        assert_eq!(s.map().layer(id).unwrap().dataset, dirty);
        assert_eq!(s.run("gpkg.pendingEdits", &json!({"layer": id})).unwrap()[format!("{kind}s")], 1);
        let disk: String = c.query_row("SELECT name FROM b WHERE fid = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(disk, "external", "reload and undo must not write the GeoPackage");
        let count: i64 = c.query_row("SELECT count(*) FROM b", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 2);
        s.run("edit.redo", &json!({})).unwrap();
        s.run("edit.setValue", &json!({"layer": id, "row": 0, "field": "name", "value": "saved after reload"})).unwrap();
        s.run("gpkg.saveEdits", &json!({"layer": id})).unwrap();
        let disk: String = c.query_row("SELECT name FROM b WHERE fid = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(disk, "saved after reload", "saving must use the refreshed optimistic baseline");
        let untouched: i64 = c.query_row("SELECT count(*) FROM a", [], |r| r.get(0)).unwrap();
        assert_eq!(untouched, 1, "the selected table must not drift to the first table");
    }
}

#[test]
fn raster_reload_refreshes_pixels_preserves_style_and_rejects_missing_bands() {
    let f = Fixture::new("reload-raster");
    let path = f.file("rgb.tif");
    raster_file(&path, 3);
    let mut s = session();
    s.run("layer.addData", &json!({"path": path})).unwrap();
    let id = s.active_layer.unwrap();
    let raster = s.map_mut().layer_mut(id).unwrap().raster.as_mut().unwrap();
    raster.style.renderer = RasterRenderer::Rgb { red: 2, green: 0, blue: 1 };
    raster.style.gamma = 1.8;
    raster.style.brightness = 17.0;
    raster.style.mask.values = vec![2.0];
    let style = serde_json::to_value(&raster.style).unwrap();
    let opening = raster.clone();
    s.map_mut().camera.res = 1234.0;
    let camera = serde_json::to_value(s.map().camera).unwrap();
    let mut changed = rustgis_engine::data::raster::read_path(&path).unwrap();
    changed.bands[2].data[0] = 99.0;
    std::fs::write(&path, geotiff::write(&changed, 0).unwrap()).unwrap();
    s.run("layer.reloadData", &json!({"layer": id})).unwrap();
    let raster = s.map().layer(id).unwrap().raster.as_ref().unwrap();
    assert_eq!(raster.data.as_ref().unwrap().value(2, 0, 0), Some(99.0));
    assert_eq!(serde_json::to_value(&raster.style).unwrap(), style);
    assert!(!raster.same_opening(&opening), "a successful reload owns a fresh runtime read");
    assert_eq!(serde_json::to_value(s.map().camera).unwrap(), camera);
    let saved = serde_json::to_string(raster).unwrap();
    assert!(!saved.contains("read_generation"), "read identities are never saved");
    let reopened: RasterLayer = serde_json::from_str(&saved).unwrap();
    assert!(!raster.same_opening(&reopened), "deserialization cannot reuse a prior opening");
    let data = Arc::clone(raster.data.as_ref().unwrap());
    raster_file(&path, 1);
    reject_reload(&mut s, id, "band");
    assert!(Arc::ptr_eq(s.map().layer(id).unwrap().raster.as_ref().unwrap().data.as_ref().unwrap(), &data));
    s.run("edit.undo", &json!({})).unwrap();
    assert_eq!(s.map().layer(id).unwrap().raster.as_ref().unwrap().data.as_ref().unwrap().value(2, 0, 0), Some(40.0));
}

#[test]
fn raster_reload_requires_the_palette_used_by_a_saved_colormap() {
    use rustgis_engine::data::raster::cog::{Example, example_with};
    let f = Fixture::new("reload-palette");
    let path = f.file("palette.tif");
    let palette = example_with(&Example { size: 300, tile: 128, classes: true, ..Example::default() });
    std::fs::write(&path, &palette).unwrap();
    let mut s = session();
    s.run("layer.addData", &json!({"path": path})).unwrap();
    let id = s.active_layer.unwrap();
    let raster = s.map().layer(id).unwrap().raster.as_ref().unwrap();
    assert!(matches!(raster.style.renderer, RasterRenderer::Colormap));
    let data = Arc::clone(raster.data.as_ref().unwrap());
    std::fs::write(&path, example_with(&Example { size: 300, tile: 128, classes: true, no_table: true, ..Example::default() })).unwrap();
    reject_reload(&mut s, id, "palette");
    assert!(Arc::ptr_eq(s.map().layer(id).unwrap().raster.as_ref().unwrap().data.as_ref().unwrap(), &data));
    std::fs::write(&path, palette).unwrap();
    s.run("layer.reloadData", &json!({"layer": id})).unwrap();
    assert!(matches!(s.map().layer(id).unwrap().raster.as_ref().unwrap().data.as_ref().unwrap().color, ColorHint::Palette(_)));
}

#[test]
fn raster_reload_and_repair_disclose_fresh_losses_once_without_repeating_them_on_a_frame() {
    use rustgis_engine::data::raster::cog::{Example, example_with};
    let f = Fixture::new("reload-raster-losses");
    let path = f.file("grey.tif");
    let moved = f.file("moved.tif");
    raster_file(&path, 1);
    let mut s = session();
    s.run("layer.addData", &json!({"path": path})).unwrap();
    let id = s.active_layer.unwrap();
    let lossy = example_with(&Example { size: 300, tile: 128, classes: true, no_table: true, ..Example::default() });
    std::fs::write(&path, &lossy).unwrap();
    std::fs::write(&moved, lossy).unwrap();
    for (command, params) in [("layer.reloadData", json!({"layer": id})), ("layer.repairSource", json!({"layer": id, "path": moved}))] {
        let mark = s.notice_mark();
        s.run(command, &params).unwrap();
        let losses = s.map().layer(id).unwrap().raster.as_ref().unwrap().data.as_ref().unwrap().lost.clone();
        assert!(!losses.is_empty(), "the real TIFF fixture lacks its required colour table");
        s.notice_landed_rasters();
        s.notice_landed_rasters();
        for loss in losses {
            assert_eq!(s.notices_since(mark).iter().filter(|n| n.text.ends_with(&loss)).count(), 1, "{command}: {loss}");
        }
    }
    raster_file(&moved, 1);
    let mark = s.notice_mark();
    s.run("layer.reloadData", &json!({"layer": id})).unwrap();
    s.notice_landed_rasters();
    assert!(s.map().layer(id).unwrap().raster.as_ref().unwrap().data.as_ref().unwrap().lost.is_empty());
    assert!(s.notices_since(mark).iter().all(|n| n.text.contains("data reloaded")), "a clean replacement reports no losses");
}

#[test]
fn raster_reload_excludes_remote_cogs_and_does_not_convert_them_to_local_sources() {
    let f = Fixture::new("reload-remote-cog");
    let path = f.file("local.tif");
    raster_file(&path, 1);
    let pixels = rustgis_engine::data::raster::read_path(&path).unwrap();
    let remote = RasterLayer::in_place(pixels, "https://example.invalid/remote.tif");
    assert!(remote.relinked(path.to_str().unwrap(), None).unwrap_err().contains("read in place"));
    let opening = remote.clone();
    let mut s = session();
    let id = s.map_mut().add_raster("Remote COG", remote);
    reject_reload(&mut s, id, "local");
    let raster = s.map().layer(id).unwrap().raster.as_ref().unwrap();
    assert!(raster.in_place && raster.same_opening(&opening));
    assert!(Arc::ptr_eq(raster.data.as_ref().unwrap(), opening.data.as_ref().unwrap()));
}

#[test]
fn detached_snapshot_and_remote_source_cannot_be_reloaded_as_local_files() {
    let f = Fixture::new("reload-unsupported");
    let path = f.file("points.fgb");
    io::write_path(&points(&[], Some(2.0)), &path).unwrap();
    let mut s = session();
    let id = add_stream(&mut s, &path, None);
    s.run("layer.loadAll", &json!({"layer": id})).unwrap();
    assert!(s.map().layer(id).unwrap().source.is_none());
    reject_reload(&mut s, id, "local");
    let id = add_stream(&mut s, &path, None);
    // Supply remote provenance without issuing a network request to construct the fixture.
    s.map_mut().layer_mut(id).unwrap().source.as_mut().unwrap().path = Some("https://example.invalid/points.fgb".into());
    reject_reload(&mut s, id, "local");
}

#[test]
fn shapefile_reload_reads_changed_dbf_and_prj_sidecars() {
    rustgis_engine::data::source::shp::use_temporary_index_dir();
    let f = Fixture::new("reload-shapefile");
    let path = f.file("points.shp");
    let original = points(&["rank"], Some(2.0));
    for (ext, bytes) in io::shapefile_parts_in(&original, Crs::WebMercator).unwrap().0 {
        std::fs::write(path.with_extension(ext), bytes).unwrap();
    }
    let mut s = session();
    let id = add_stream(&mut s, &path, None);
    assert!(first_point(&loaded(&s, id)).x.abs() < 0.001);
    let mut replacement = original;
    replacement.features[0].attrs[0] = Value::Int(12345);
    // Leave .shp untouched: both changed observations must come from sidecars.
    for (ext, bytes) in io::shapefile_parts_in(&replacement, Crs::LonLat).unwrap().0 {
        if matches!(ext, "dbf" | "prj") {
            std::fs::write(path.with_extension(ext), bytes).unwrap();
        }
    }
    s.run("layer.reloadData", &json!({"layer": id})).unwrap();
    let ds = loaded(&s, id);
    assert_eq!(first_point(&ds), Coord::new(2.0, 48.0));
    assert_eq!(ds.value(0, ds.field_index("rank").unwrap()).as_f64(), Some(12345.0));
}

#[test]
fn reload_reconverts_original_gdal_folder_and_accepts_its_declared_empty_table() {
    use rustgis_engine::{
        data::source::gdal,
        map::source::{GdalOrigin, SourceLayer},
    };
    if !gdal::usable() {
        return;
    }
    let f = Fixture::new("reload-gdal-folder");
    let folder = f.file("survey.gdb");
    let input = f.file("survey.geojson");
    let publish = |name: &str, x: Option<f64>, z: bool| {
        let value = x.unwrap_or(0.0);
        let coordinates = if z { format!("[{value},48,100]") } else { format!("[{value},48]") };
        let features =
            format!(r#"{{"type":"Feature","properties":{{"rank":7}},"geometry":{{"type":"Point","coordinates":{coordinates}}}}}"#);
        std::fs::write(&input, format!(r#"{{"type":"FeatureCollection","name":"survey","features":[{features}]}}"#)).unwrap();
        let mut command = std::process::Command::new(gdal::program("ogr2ogr").unwrap());
        command.args(["-f", "OpenFileGDB", "-overwrite", "-nln", name, "-nlt", if z { "POINTZ" } else { "POINT" }]);
        if x.is_none() {
            command.args(["-where", "1=0"]);
        }
        if folder.exists() {
            command.arg("-update");
        }
        let output = command.arg(&folder).arg(&input).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    };
    for i in 0..64 {
        publish(&format!("sibling_{i:02}"), Some(1.0), false);
    }
    publish("survey", Some(2.0), false);
    let listed = gdal::layers(&folder).unwrap();
    assert_eq!(listed.len(), 64, "the chooser's listing cap is unchanged");
    assert!(!listed.iter().any(|name| name == "survey"), "the selected target must lie beyond the listing cap");
    let original = gdal::convert_one(&folder, "SuRvEy", None, None).unwrap().unwrap();
    assert!(gdal::convert_one(&folder, "truly_absent", None, None).is_err());
    let handle = rustgis_engine::data::source::open_path(&original, None).unwrap();
    let mut source = SourceLayer::from_path(original.to_str().unwrap(), None, handle);
    source.gdal = Some(GdalOrigin { path: folder.to_str().unwrap().into(), layer: "SuRvEy".into(), family: None, crs: None });
    let mut s = session();
    let id = s.map_mut().add_streamed("Folder survey", source);
    publish("survey", Some(8.0), false);
    s.run("layer.reloadData", &json!({"layer": id})).unwrap();
    assert_eq!(first_point(&loaded(&s, id)), Coord::new(8.0, 48.0));
    assert_ne!(s.map().layer(id).unwrap().source.as_ref().unwrap().path.as_deref(), original.to_str());
    assert_eq!(s.map().layer(id).unwrap().linked_file(), folder.to_str());
    assert_eq!(s.map().layers.len(), 1);
    let source = s.map().layer(id).unwrap().source.as_ref().unwrap();
    let retained = source.clone();
    let escaped = source.visit_source().unwrap();
    let fresh_path = std::path::PathBuf::from(source.path.as_ref().unwrap());
    let fresh_workspace = fresh_path.parent().unwrap().to_path_buf();
    let live_project = f.file("live-conversion.rgis");
    s.run("project.saveAs", &json!({"path": live_project})).unwrap();
    let mut opened_while_live = session();
    opened_while_live.run("project.open", &json!({"path": live_project})).unwrap();
    let live_path = std::path::PathBuf::from(opened_while_live.map().layer(id).unwrap().source.as_ref().unwrap().path.as_ref().unwrap());
    let live_workspace = live_path.parent().unwrap().to_path_buf();
    assert_ne!(live_path, fresh_path, "a saved link cannot borrow another session's disposable FGB");
    assert_eq!(opened_while_live.map().layers.len(), 1);
    let abandoned = abandoned_reload_workspace(&fresh_workspace);
    let candidate = retained.relinked(folder.to_str().unwrap(), None).unwrap();
    assert!(!abandoned.exists(), "a new GDAL conversion reclaims crash-abandoned workspaces");
    assert!(fresh_workspace.exists(), "the live reader's locked workspace must survive pruning");
    assert_eq!(first_point(&retained.read_all().unwrap().0), Coord::new(8.0, 48.0));
    let candidate_path = std::path::PathBuf::from(candidate.path.as_ref().unwrap());
    let candidate_workspace = candidate_path.parent().unwrap().to_path_buf();
    assert!(candidate_path.exists());
    drop(candidate);
    assert!(!candidate_workspace.exists(), "an uninstalled candidate has no owner");
    assert!(retained.relinked(folder.to_str().unwrap(), Some("truly_absent")).is_err());
    publish("survey", None, false);
    s.run("layer.reloadData", &json!({"layer": id})).unwrap();
    assert!(loaded(&s, id).features.is_empty());
    assert_eq!(s.map().layer(id).unwrap().dataset.geometry_type, GeometryType::Point);
    let empty_source = s.map().layer(id).unwrap().source.as_ref().unwrap();
    assert_eq!(empty_source.info().unwrap().count, Some(0));
    let empty_path = std::path::PathBuf::from(empty_source.path.as_ref().unwrap());
    let empty_workspace = empty_path.parent().unwrap().to_path_buf();
    let empty_origin = empty_source.gdal.clone();
    let empty_project = f.file("empty-conversion.rgis");
    s.run("project.saveAs", &json!({"path": empty_project})).unwrap();
    s.run("edit.undo", &json!({})).unwrap();
    assert_eq!(first_point(&loaded(&s, id)), Coord::new(8.0, 48.0), "undo must retain its actual converted reader");
    s.run("edit.redo", &json!({})).unwrap();
    assert!(loaded(&s, id).features.is_empty());
    assert!(fresh_path.exists() && empty_path.exists());

    for (x, z) in [(12.0, true), (14.0, false)] {
        publish("survey", Some(x), z);
        let mark = s.notice_mark();
        s.run("layer.reloadData", &json!({"layer": id})).unwrap();
        assert_eq!(s.notices_since(mark).iter().any(|n| n.text.contains("Z or M coordinates")), z);
        s.run("layer.loadAll", &json!({"layer": id})).unwrap();
        assert!(s.map().layer(id).unwrap().source.is_none());
        assert_eq!(s.map().layer(id).unwrap().dataset.zm_dropped, z);
        let output = f.file(if z { "xyz-export.geojson" } else { "xy-export.geojson" });
        let mark = s.notice_mark();
        s.run("share.exportLayer", &json!({"layer": id, "path": output})).unwrap();
        assert_eq!(
            s.notices_since(mark).iter().filter(|n| n.text.contains("its geometry has no Z or M values")).count(),
            usize::from(z),
            "{:?}",
            s.notices_since(mark)
        );
        assert_eq!(first_point(&io::read_path(&output).unwrap().0), Coord::new(x, 48.0));
        s.run("edit.undo", &json!({})).unwrap();
        assert!(s.map().layer(id).unwrap().source.is_some());
    }
    drop(s);
    assert!(!empty_path.exists(), "the final active/undo owner releases its private conversion");
    assert!(!empty_workspace.exists(), "all original owners of the saved empty conversion have dropped");
    assert_eq!(first_point(&loaded(&opened_while_live, id)), Coord::new(8.0, 48.0));
    drop(opened_while_live);
    assert!(!live_workspace.exists(), "the independently restored reader owns its workspace");
    publish("survey", None, false);
    let mut reopened = session();
    reopened.run("project.open", &json!({"path": empty_project})).unwrap();
    assert_eq!(reopened.map().layers.len(), 1, "restoring a selected table must not add siblings");
    let restored = reopened.map().layer(id).unwrap().source.as_ref().unwrap();
    assert_eq!(restored.gdal, empty_origin);
    assert_eq!(restored.linked_file(), folder.to_str());
    assert_eq!(restored.info().unwrap().count, Some(0));
    assert_eq!(restored.info().unwrap().geometry_type, GeometryType::Point);
    let restored_path = std::path::PathBuf::from(restored.path.as_ref().unwrap());
    let restored_workspace = restored_path.parent().unwrap().to_path_buf();
    assert_ne!(restored_path, empty_path);
    let restored_reader = restored.visit_source().unwrap();
    assert!(loaded(&reopened, id).features.is_empty());
    drop(reopened);
    assert!(restored_workspace.exists(), "an escaped restored reader pins its fresh workspace");
    let batch = restored_reader.read(&rustgis_engine::data::source::Request { extent: None, limit: 1 }).unwrap();
    assert!(batch.complete && batch.features.is_empty() && batch.skipped == 0);
    drop(restored_reader);
    assert!(!restored_workspace.exists(), "the last restored reader releases all conversion artifacts");
    assert!(fresh_path.exists(), "a cloned SourceLayer and escaping export reader pin their file");
    drop(retained);
    assert!(fresh_path.exists(), "the escaping concrete reader owns its conversion independently of SourceLayer");
    let mut exported = Vec::new();
    let report = escaped
        .visit_chunks(None, 4096, &mut |features| {
            exported.extend(features);
            Ok(())
        })
        .unwrap();
    assert!(report.complete);
    assert_eq!(exported.len(), 1);
    assert_eq!(exported[0].geometry.as_ref().unwrap().extent().center(), Coord::new(8.0, 48.0));
    drop(escaped);
    assert!(!fresh_workspace.exists(), "the last concrete reader releases all private artifacts");
    assert!(original.exists(), "ordinary reusable conversions keep their cache policy");
}

#[test]
fn reload_reconverts_duckdb_table_and_keeps_only_its_original_geometry_branch() {
    use rustgis_engine::data::source::duckdb;
    if !duckdb::available() {
        return;
    }
    let f = Fixture::new("reload-duckdb");
    let path = f.file("survey.duckdb");
    let sql = |statement: &str| {
        let output = std::process::Command::new(duckdb::program().unwrap())
            .arg(&path)
            .arg("-c")
            .arg(format!("LOAD spatial; {statement}"))
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    };
    sql("CREATE TABLE survey AS SELECT 7 AS rank, ST_Point(2,48) AS geom");
    let mut s = session();
    s.run("layer.addDatabase", &json!({"database": path, "table": "survey", "stream": true})).unwrap();
    let id = s.active_layer.unwrap();
    assert_eq!(first_point(&loaded(&s, id)), Coord::new(2.0, 48.0));
    sql("UPDATE survey SET geom=ST_Point(8,48); INSERT INTO survey VALUES(9,ST_GeomFromText('LINESTRING(0 0,1 1)'))");
    let mark = s.notice_mark();
    s.run("layer.reloadData", &json!({"layer": id})).unwrap();
    assert_eq!(
        s.notices_since(mark).iter().filter(|n| n.text.contains("DuckDB: 1 rows of another geometry type were left out")).count(),
        1
    );
    let fresh = loaded(&s, id);
    assert_eq!(first_point(&fresh), Coord::new(8.0, 48.0));
    assert_eq!(fresh.features.len(), 1);
    assert_eq!(s.map().layers.len(), 1);
    let source = s.map().layer(id).unwrap().source.as_ref().unwrap();
    let retained = source.clone();
    let escaped = source.visit_source().unwrap();
    let fresh_path = std::path::PathBuf::from(source.path.as_ref().unwrap());
    let fresh_workspace = fresh_path.parent().unwrap().to_path_buf();
    assert_eq!(s.map().layer(id).unwrap().linked_file(), path.to_str());
    let abandoned = abandoned_reload_workspace(&fresh_workspace);
    let candidate = source.relinked(path.to_str().unwrap(), None).unwrap();
    assert!(!abandoned.exists(), "a new DuckDB conversion reclaims crash-abandoned workspaces");
    assert!(fresh_workspace.exists(), "the live reader's locked workspace must survive pruning");
    assert_eq!(first_point(&retained.read_all().unwrap().0), Coord::new(8.0, 48.0));
    let candidate_path = std::path::PathBuf::from(candidate.path.as_ref().unwrap());
    let candidate_workspace = candidate_path.parent().unwrap().to_path_buf();
    drop(candidate);
    assert!(!candidate_workspace.exists(), "an uninstalled candidate must release every Parquet and sidecar");
    let output = f.file("fresh-export.geojson");
    s.run("share.exportLayer", &json!({"layer": id, "path": output})).unwrap();
    assert_eq!(first_point(&io::read_path(&output).unwrap().0), Coord::new(8.0, 48.0));
    sql("DELETE FROM survey");
    s.run("layer.reloadData", &json!({"layer": id})).unwrap();
    assert!(loaded(&s, id).features.is_empty());
    let empty_source = s.map().layer(id).unwrap().source.as_ref().unwrap();
    assert_eq!(empty_source.info().unwrap().count, Some(0));
    assert_eq!(empty_source.info().unwrap().geometry_type, GeometryType::Point);
    let empty_path = std::path::PathBuf::from(empty_source.path.as_ref().unwrap());
    let empty_workspace = empty_path.parent().unwrap().to_path_buf();
    let empty_origin = empty_source.duckdb.clone();
    let empty_project = f.file("empty-conversion.rgis");
    s.run("project.saveAs", &json!({"path": empty_project})).unwrap();
    s.run("edit.undo", &json!({})).unwrap();
    assert_eq!(first_point(&loaded(&s, id)), Coord::new(8.0, 48.0));
    s.run("edit.redo", &json!({})).unwrap();
    assert!(loaded(&s, id).features.is_empty());
    assert!(fresh_path.exists() && empty_path.exists());
    sql("DROP TABLE survey");
    reject_reload(&mut s, id, "survey");
    drop(s);
    assert!(!empty_path.exists());
    assert!(!empty_workspace.exists(), "all original owners of the saved empty conversion have dropped");
    sql("CREATE TABLE survey AS SELECT 7 AS rank, ST_Point(0,0) AS geom WHERE false; CREATE TABLE sibling AS SELECT ST_Point(1,1) AS geom");
    let mut reopened = session();
    reopened.run("project.open", &json!({"path": empty_project})).unwrap();
    assert_eq!(reopened.map().layers.len(), 1, "restoring a selected table must not add siblings");
    let restored = reopened.map().layer(id).unwrap().source.as_ref().unwrap();
    assert_eq!(restored.duckdb, empty_origin);
    assert_eq!(restored.linked_file(), path.to_str());
    assert_eq!(restored.info().unwrap().count, Some(0));
    assert_eq!(restored.info().unwrap().geometry_type, GeometryType::Point);
    let restored_path = std::path::PathBuf::from(restored.path.as_ref().unwrap());
    let restored_workspace = restored_path.parent().unwrap().to_path_buf();
    assert_ne!(restored_path, empty_path);
    let restored_reader = restored.visit_source().unwrap();
    assert!(loaded(&reopened, id).features.is_empty());
    drop(reopened);
    assert!(restored_workspace.exists(), "an escaped restored reader pins its fresh workspace");
    let batch = restored_reader.read(&rustgis_engine::data::source::Request { extent: None, limit: 1 }).unwrap();
    assert!(batch.complete && batch.features.is_empty() && batch.skipped == 0);
    drop(restored_reader);
    assert!(!restored_workspace.exists(), "the last restored reader releases all conversion artifacts");
    assert!(fresh_path.exists());
    drop(retained);
    assert!(fresh_path.exists(), "the escaping concrete reader must survive the layer and undo stack");
    let mut exported = Vec::new();
    let report = escaped
        .visit_chunks(None, 4096, &mut |features| {
            exported.extend(features);
            Ok(())
        })
        .unwrap();
    assert!(report.complete);
    assert_eq!(exported.len(), 1);
    assert_eq!(exported[0].geometry.as_ref().unwrap().extent().center(), Coord::new(8.0, 48.0));
    drop(escaped);
    assert!(!fresh_workspace.exists(), "close Parquet readers before removing their workspace on Windows too");
}

#[test]
fn saved_gdal_conversion_restores_fresh_data_and_loss_notes_despite_an_unchanged_signature() {
    use rustgis_engine::{
        data::source::{Family, gdal},
        map::source::{GdalOrigin, SourceLayer},
    };
    if !gdal::usable() {
        return;
    }
    let f = Fixture::new("restore-gdal-signature");
    // GeoJSON Lines: a file type GDAL reads (plain GeoJSON is read without it).
    let path = f.file("survey.geojsonl");
    let publish = |x| {
        std::fs::write(
            &path,
            format!(r#"{{"type":"Feature","properties":{{"rank":7}},"geometry":{{"type":"Point","coordinates":[{x},48,100]}}}}"#),
        )
        .unwrap();
    };
    publish(2);
    let signature = std::fs::metadata(&path).unwrap();
    let origin = GdalOrigin {
        path: path.to_str().unwrap().into(),
        layer: "survey".into(),
        family: Some(Family::Points),
        crs: Some("EPSG:4326".into()),
    };
    let cached = gdal::convert_one(&path, &origin.layer, origin.family, origin.crs.as_deref()).unwrap().unwrap();
    let handle = rustgis_engine::data::source::open_path(&cached, None).unwrap();
    let mut source = SourceLayer::from_path(cached.to_str().unwrap(), None, handle);
    source.gdal = Some(origin.clone());
    source.open().unwrap();
    assert_eq!(source.path.as_deref(), cached.to_str(), "nonmissing reusable conversions retain their ordinary policy");
    let mut source = source.relinked(path.to_str().unwrap(), None).unwrap();
    source.limit = 17;
    source.byte_budget = 12345;
    source.reads = 23;
    let private_path = std::path::PathBuf::from(source.path.as_ref().unwrap());
    let private_workspace = private_path.parent().unwrap().to_path_buf();
    let serialized = serde_json::to_value(&source).unwrap();
    let mut s = session();
    let id = s.map_mut().add_streamed("Survey", source);
    let project = f.file("survey.rgis");
    s.run("project.saveAs", &json!({"path": project})).unwrap();
    drop(s);
    assert!(!private_workspace.exists());

    publish(8);
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(signature.modified().unwrap()))
        .unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().len(), signature.len());
    assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), signature.modified().unwrap());
    assert_eq!(gdal::convert_one(&path, &origin.layer, origin.family, origin.crs.as_deref()).unwrap().unwrap(), cached);
    assert_eq!(first_point(&io::read_path(&cached).unwrap().0), Coord::new(2.0, 48.0), "the reusable cache really is stale");
    let mut reopened = session();
    reopened.run("project.open", &json!({"path": project})).unwrap();
    assert!(reopened.notices_since(0).iter().all(|n| !n.text.contains("showing the saved conversion")));
    let losses = reopened.notices_since(0).iter().filter(|n| n.text.contains("Z or M coordinates")).collect::<Vec<_>>();
    assert_eq!(losses.len(), 1, "{losses:?}");
    assert!(losses[0].text.starts_with("Survey: "), "{}", losses[0].text);
    assert_eq!(reopened.map().layers.len(), 1);
    assert_eq!(first_point(&loaded(&reopened, id)), Coord::new(8.0, 48.0));
    let restored = reopened.map().layer(id).unwrap().source.as_ref().unwrap();
    assert_eq!(restored.gdal, Some(origin.clone()));
    assert!(restored.info().unwrap().zm_dropped);
    assert!(restored.info().unwrap().notes.iter().any(|note| note.contains("Z or M coordinates")));
    drop(reopened);

    // Explicit open retains runtime settings and the read generation. A disposable converted
    // file must never become a fallback when its provenance disappears.
    let mut restored: SourceLayer = serde_json::from_value(serialized).unwrap();
    restored.limit = 17;
    restored.byte_budget = 12345;
    restored.reads = 23;
    restored.open().unwrap();
    assert_eq!((restored.limit, restored.byte_budget, restored.reads), (17, 12345, 23));
    let restored_path = restored.path.clone();
    let restored_origin = restored.gdal.clone();
    std::fs::remove_file(&path).unwrap();
    let error = restored.open().unwrap_err();
    assert_eq!(restored.path, restored_path);
    assert_eq!(restored.gdal, restored_origin);
    assert_eq!(restored.error.as_deref(), Some(error.as_str()));
    assert_eq!(first_point(&restored.read_all().unwrap().0), Coord::new(8.0, 48.0));
}

#[test]
fn saved_duckdb_conversion_restores_fresh_selected_family_despite_an_unchanged_signature() {
    use rustgis_engine::data::source::duckdb;
    if !duckdb::available() {
        return;
    }
    let f = Fixture::new("restore-duckdb-signature");
    let path = f.file("survey.duckdb");
    let stale_db = f.file("stale.duckdb");
    let sql = |db: &std::path::Path, statement: &str| {
        let output = std::process::Command::new(duckdb::program().unwrap())
            .arg(db)
            .arg("-c")
            .arg(format!("LOAD spatial; {statement}"))
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    };
    sql(&path, "CREATE TABLE survey AS SELECT 7 AS rank, ST_Point(2,48) AS geom; CREATE TABLE sibling AS SELECT ST_Point(1,1) AS geom");
    sql(&stale_db, "CREATE TABLE survey AS SELECT 7 AS rank, ST_Point(99,48) AS geom");
    let mut s = session();
    s.run("layer.addDatabase", &json!({"database": path, "layer": "survey", "stream": true})).unwrap();
    let id = s.active_layer.unwrap();
    let cached_path = std::path::PathBuf::from(s.map().layer(id).unwrap().source.as_ref().unwrap().path.clone().unwrap());
    s.run("layer.reloadData", &json!({"layer": id})).unwrap();
    let source = s.map().layer(id).unwrap().source.as_ref().unwrap();
    let origin = source.duckdb.clone();
    let private_path = std::path::PathBuf::from(source.path.as_ref().unwrap());
    let private_workspace = private_path.parent().unwrap().to_path_buf();
    let project = f.file("survey.rgis");
    s.run("project.saveAs", &json!({"path": project})).unwrap();
    drop(s);
    assert!(!private_workspace.exists());

    // Make the ordinary signature cache hold different data while the database is unchanged:
    // restoring through that cache would revive the stale conversion.
    let stale = duckdb::Database::File(stale_db.clone());
    let stale_table = duckdb::find_table_of(&stale, "survey").unwrap();
    let stale_out = duckdb::convert_table_of(&stale, &stale_table, None).unwrap();
    std::fs::copy(&stale_out.parts[0].path, &cached_path).unwrap();
    let database = duckdb::Database::File(path.clone());
    let table = duckdb::find_table_of(&database, "survey").unwrap();
    let cached = duckdb::convert_table_of(&database, &table, None).unwrap();
    assert_eq!(cached.parts[0].path, cached_path, "the ordinary signature cache remains reusable");
    assert_eq!(first_point(&io::read_path(&cached.parts[0].path).unwrap().0), Coord::new(99.0, 48.0), "the reusable cache really is stale");
    let mut reopened = session();
    reopened.run("project.open", &json!({"path": project})).unwrap();
    assert!(reopened.notices_since(0).iter().all(|n| !n.text.contains("showing the saved conversion")));
    assert_eq!(reopened.map().layers.len(), 1);
    let restored = reopened.map().layer(id).unwrap().source.as_ref().unwrap();
    assert_eq!(restored.duckdb, origin);
    assert_eq!(restored.linked_file(), path.to_str());
    assert_eq!(first_point(&loaded(&reopened, id)), Coord::new(2.0, 48.0));
}

fn abandoned_reload_workspace(live: &Path) -> std::path::PathBuf {
    let lock = std::fs::File::options().read(true).write(true).open(live.join("owner.lock")).unwrap();
    assert!(lock.try_lock().is_err(), "a live concrete reader owns an exclusive workspace lock");
    let abandoned = live.with_file_name(format!("reload-abandoned-{}", live.file_name().unwrap().to_string_lossy()));
    std::fs::create_dir(&abandoned).unwrap();
    // A crashed RustGIS left its own ownership marker (the live lock can't be read on Windows).
    std::fs::write(abandoned.join("owner.lock"), format!("RustGIS reload workspace v1\n{}\n", std::process::id())).unwrap();
    std::fs::write(abandoned.join("leftover"), b"crash-abandoned conversion").unwrap();
    age_reload_directory(&abandoned);
    abandoned
}

fn age_reload_directory(path: &Path) {
    let mut options = std::fs::File::options();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.access_mode(0x0100).custom_flags(0x0200_0000); // FILE_WRITE_ATTRIBUTES, FILE_FLAG_BACKUP_SEMANTICS.
    }
    options
        .open(path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(61)))
        .unwrap();
}

fn reload_duckdb_sql(path: &Path, statement: &str) {
    use rustgis_engine::data::source::duckdb;
    let output = std::process::Command::new(duckdb::program().unwrap())
        .arg(path)
        .arg("-c")
        .arg(format!("LOAD spatial; {statement}"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
}

#[test]
fn duckdb_reload_reports_other_family_rows_only_for_mixed_tables() {
    use rustgis_engine::data::source::duckdb;
    if !duckdb::available() {
        return;
    }
    let f = Fixture::new("reload-duckdb-family-notice");
    let path = f.file("survey.duckdb");
    reload_duckdb_sql(
        &path,
        "CREATE TABLE survey AS SELECT 7 AS rank, ST_Point(2,48) AS geom;
         INSERT INTO survey VALUES (8,ST_GeomFromText('LINESTRING(0 0,1 1)')), (9,ST_GeomFromText('LINESTRING(2 2,3 3)'))",
    );
    let mut s = session();
    s.run("layer.addDatabase", &json!({"database": path, "table": "survey", "stream": true})).unwrap();
    let id = s.map().layers.iter().find(|l| l.dataset.geometry_type == GeometryType::Point).unwrap().id;
    let mark = s.notice_mark();
    s.run("layer.reloadData", &json!({"layer": id})).unwrap();
    let notices = s.notices_since(mark);
    let losses = notices.iter().filter(|n| n.text.contains("rows of another geometry type were left out")).collect::<Vec<_>>();
    assert_eq!(losses.len(), 1);
    assert!(losses[0].text.contains("DuckDB: 2 rows of another geometry type were left out"), "{}", losses[0].text);
    assert_eq!(loaded(&s, id).features.len(), 1);
    reload_duckdb_sql(&path, "DELETE FROM survey WHERE ST_Dimension(geom)=1");
    let mark = s.notice_mark();
    s.run("layer.reloadData", &json!({"layer": id})).unwrap();
    assert!(s.notices_since(mark).iter().all(|n| !n.text.contains("rows of another geometry type were left out")));
    assert_eq!(loaded(&s, id).features.len(), 1);
}

#[test]
fn duckdb_repair_refuses_other_family_only_tables_while_reload_installs_an_empty_branch() {
    use rustgis_engine::data::source::{Family, duckdb};
    if !duckdb::available() {
        return;
    }
    for (family, geometry, dimension, left_out) in
        [(Family::Points, GeometryType::Point, 0, 2), (Family::Lines, GeometryType::Polyline, 1, 1)]
    {
        let f = Fixture::new("reload-duckdb-empty-family");
        let path = f.file("survey.duckdb");
        reload_duckdb_sql(
            &path,
            "CREATE TABLE survey AS SELECT 7 AS rank, ST_Point(2,48) AS geom;
             INSERT INTO survey VALUES (8,ST_GeomFromText('LINESTRING(0 0,1 1)')), (9,ST_GeomFromText('LINESTRING(2 2,3 3)'))",
        );
        let mut s = session();
        s.run("layer.addDatabase", &json!({"database": path, "table": "survey", "stream": true})).unwrap();
        let id = s
            .map()
            .layers
            .iter()
            .find(|l| l.source.as_ref().and_then(|s| s.duckdb.as_ref()).is_some_and(|d| d.family == family))
            .unwrap()
            .id;
        reload_duckdb_sql(&path, &format!("DELETE FROM survey WHERE ST_Dimension(geom)={dimension}"));
        let prior = s.map().layer(id).unwrap().source.as_ref().unwrap().clone();
        let before = serde_json::to_value(s.map().layer(id).unwrap()).unwrap();
        let mark = s.notice_mark();
        let error = s.run("layer.repairSource", &json!({"layer": id, "path": path, "table": "survey"})).unwrap_err();
        assert!(error.contains(&format!("replacement table contains no {}", family.label())), "{error}");
        assert!(error.contains(&format!("DuckDB: {left_out} rows of another geometry type were left out")), "{error}");
        assert_eq!(serde_json::to_value(s.map().layer(id).unwrap()).unwrap(), before);
        assert!(s.map().layer(id).unwrap().source.as_ref().unwrap().same_opening(&prior));
        assert!(s.notices_since(mark).iter().any(|n| n.error && n.text.contains(&error)));
        assert!(s.notices_since(mark).iter().all(|n| n.error), "refused repair must not report installation");
        let mark = s.notice_mark();
        s.run("layer.reloadData", &json!({"layer": id})).unwrap();
        let notices = s.notices_since(mark);
        let losses = notices.iter().filter(|n| n.text.contains("rows of another geometry type were left out")).collect::<Vec<_>>();
        assert_eq!(losses.len(), 1);
        assert!(losses[0].text.contains(&format!("DuckDB: {left_out} rows of another geometry type were left out")));
        let layer = s.map().layer(id).unwrap();
        assert_eq!(layer.dataset.geometry_type, geometry);
        assert_eq!(layer.source.as_ref().unwrap().info().unwrap().count, Some(0));
        let ds = loaded(&s, id);
        assert!(ds.features.is_empty());
    }
}

/// A layer file or imported map whose GDAL link can't re-read its original says the saved
/// conversion stands in, as opening a project does.
#[test]
fn layer_files_and_imported_maps_say_when_a_saved_conversion_stands_in() {
    use rustgis_engine::{
        data::source::Family,
        map::source::{GdalOrigin, SourceLayer},
    };
    let f = Fixture::new("restore-item-conversions");
    let original = f.file("removed.gdb");
    let converted = f.file("converted.fgb");
    io::write_path(&points(&["rank"], Some(8.0)), &converted).unwrap();
    let handle = rustgis_engine::data::source::open_path(&converted, None).unwrap();
    let mut source = SourceLayer::from_path(converted.to_str().unwrap(), None, handle);
    source.gdal =
        Some(GdalOrigin { path: original.to_str().unwrap().into(), layer: "survey".into(), family: Some(Family::Points), crs: None });
    let mut s = session();
    s.map_mut().add_streamed("Saved conversion", source);
    let layer_file = f.file("survey.rgislyr");
    s.run("layer.save", &json!({"layer": "Saved conversion", "path": layer_file})).unwrap();
    let map_file = f.file("survey.rgismap");
    s.run("map.save", &json!({"path": map_file})).unwrap();
    drop(s);
    for (command, path) in [("layer.addLayerFile", &layer_file), ("map.import", &map_file)] {
        let mut s = session();
        let mark = s.notice_mark();
        s.run(command, &json!({"path": path})).unwrap();
        let notices = s.notices_since(mark);
        let fallbacks = notices.iter().filter(|n| n.text.contains("showing the saved conversion")).collect::<Vec<_>>();
        assert_eq!(fallbacks.len(), 1, "{command}: {notices:?}");
        assert!(fallbacks[0].text.contains(original.to_str().unwrap()), "{}", fallbacks[0].text);
    }
}

#[test]
fn project_open_does_not_repeat_ordinary_source_conversion_notes() {
    use rustgis_engine::map::source::SourceLayer;

    let f = Fixture::new("restore-source-notes");
    let path = f.file("survey.geojson");
    std::fs::write(
        &path,
        r#"{"type":"FeatureCollection","features":[
            {"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[8,48]}},
            {"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":["bad",48]}}
        ]}"#,
    )
    .unwrap();
    let handle = rustgis_engine::data::source::open_path(&path, None).unwrap();
    let source = SourceLayer::from_path(path.to_str().unwrap(), None, handle);
    assert!(source.info().unwrap().notes.iter().any(|note| note.contains("1 features had unreadable geometry")));
    let mut s = session();
    let id = s.map_mut().add_streamed("Survey", source);
    let project = f.file("survey.rgis");
    s.run("project.saveAs", &json!({"path": project})).unwrap();
    drop(s);

    let mut reopened = session();
    let mark = reopened.notice_mark();
    reopened.run("project.open", &json!({"path": project})).unwrap();
    let notes = &reopened.map().layer(id).unwrap().source.as_ref().unwrap().info().unwrap().notes;
    assert!(notes.iter().any(|note| note.contains("1 features had unreadable geometry")));
    let notices = reopened.notices_since(mark);
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].text.starts_with("Opened "), "{notices:?}");
    assert_eq!(first_point(&loaded(&reopened, id)), Coord::new(8.0, 48.0));
}

#[test]
fn two_saved_conversion_fallbacks_report_one_notice_with_layer_count_and_first_failure() {
    use rustgis_engine::{
        data::source::Family,
        map::source::{DuckdbOrigin, GdalOrigin, SourceLayer},
    };

    let f = Fixture::new("restore-grouped-conversions");
    let mut s = session();
    let first_original = f.file("removed.gdb");
    for duckdb in [false, true] {
        let original = if duckdb { f.file("removed.duckdb") } else { first_original.clone() };
        let converted = f.file(if duckdb { "converted.parquet" } else { "converted.fgb" });
        io::write_path(&points(&["rank"], Some(8.0)), &converted).unwrap();
        let handle = rustgis_engine::data::source::open_path(&converted, None).unwrap();
        let mut source = SourceLayer::from_path(converted.to_str().unwrap(), None, handle);
        if duckdb {
            source.duckdb = Some(DuckdbOrigin {
                sql: None,
                database: Some(original.to_str().unwrap().into()),
                table: Some("survey".into()),
                crs: None,
                family: Family::Points,
            });
        } else {
            source.gdal = Some(GdalOrigin {
                path: original.to_str().unwrap().into(),
                layer: "survey".into(),
                family: Some(Family::Points),
                crs: None,
            });
        }
        s.map_mut().add_streamed(if duckdb { "Second conversion" } else { "First conversion" }, source);
    }
    let project = f.file("survey.rgis");
    s.run("project.saveAs", &json!({"path": project})).unwrap();
    drop(s);

    let mut reopened = session();
    let mark = reopened.notice_mark();
    reopened.run("project.open", &json!({"path": project})).unwrap();
    let notices = reopened.notices_since(mark);
    let fallbacks = notices.iter().filter(|n| n.text.contains("saved conversion")).collect::<Vec<_>>();
    assert_eq!(fallbacks.len(), 1, "{notices:?}");
    let notice = fallbacks[0];
    assert!(notice.text.starts_with("2 layers "), "{}", notice.text);
    assert!(notice.text.contains("showing saved conversions"), "{}", notice.text);
    // The first in the map's layer order is named, with its own original.
    assert!(notice.text.contains("Second conversion"), "{}", notice.text);
    assert!(notice.text.contains(f.file("removed.duckdb").to_str().unwrap()), "{}", notice.text);
    assert!(notice.text.contains("failed ("), "{}", notice.text);
    assert!(!notice.error);
    for layer in &mut reopened.map_mut().layers {
        let source = layer.source.as_mut().unwrap();
        assert!(source.error.is_none(), "{:?}", source.error);
        assert!(source.take_conversion_fallback().is_none(), "project reset consumed each fallback once");
        assert!(source.info().unwrap().notes.iter().all(|note| !note.contains("saved conversion")));
        assert_eq!(first_point(&layer.load_all().unwrap()), Coord::new(8.0, 48.0));
    }
}

#[test]
fn saved_durable_conversion_outside_current_cache_reopens_with_one_original_source_failure_notice() {
    use rustgis_engine::{
        data::source::Family,
        map::source::{DuckdbOrigin, GdalOrigin, SourceLayer},
    };
    for duckdb in [false, true] {
        let f = Fixture::new("restore-external-conversion");
        let original = f.file(if duckdb { "removed.duckdb" } else { "removed.gdb" });
        let converted = f.file(if duckdb { "converted.parquet" } else { "converted.fgb" });
        io::write_path(&points(&["rank"], Some(8.0)), &converted).unwrap();
        let handle = rustgis_engine::data::source::open_path(&converted, None).unwrap();
        let mut source = SourceLayer::from_path(converted.to_str().unwrap(), None, handle);
        if duckdb {
            source.duckdb = Some(DuckdbOrigin {
                sql: None,
                database: Some(original.to_str().unwrap().into()),
                table: Some("survey".into()),
                crs: None,
                family: Family::Points,
            });
        } else {
            source.gdal = Some(GdalOrigin {
                path: original.to_str().unwrap().into(),
                layer: "survey".into(),
                family: Some(Family::Points),
                crs: None,
            });
        }
        let mut s = session();
        let id = s.map_mut().add_streamed("Saved conversion", source);
        let project = f.file("survey.rgis");
        s.run("project.saveAs", &json!({"path": project})).unwrap();
        drop(s);
        assert!(converted.exists());
        assert!(!original.exists());
        let mut reopened = session();
        let mark = reopened.notice_mark();
        reopened.run("project.open", &json!({"path": project})).unwrap();
        assert_eq!(reopened.map().layers.len(), 1);
        let source = reopened.map().layer(id).unwrap().source.as_ref().unwrap();
        assert!(source.error.is_none(), "{:?}", source.error);
        assert_eq!(source.path.as_deref(), converted.to_str());
        assert!(source.info().unwrap().notes.iter().all(|note| !note.contains("saved conversion")));
        assert_eq!(first_point(&loaded(&reopened, id)), Coord::new(8.0, 48.0));
        let notices = reopened.notices_since(mark);
        let fallbacks = notices.iter().filter(|n| n.text.contains("showing the saved conversion")).collect::<Vec<_>>();
        assert_eq!(fallbacks.len(), 1, "{notices:?}");
        assert!(fallbacks[0].text.contains(original.to_str().unwrap()), "{}", fallbacks[0].text);
        assert!(fallbacks[0].text.contains("failed ("), "{}", fallbacks[0].text);
        assert!(!fallbacks[0].error, "the durable fallback opened successfully, but warns about stale data");
        drop(reopened);
        std::fs::remove_file(&converted).unwrap();
        let mut missing = session();
        missing.run("project.open", &json!({"path": project})).unwrap();
        let source = missing.map().layer(id).unwrap().source.as_ref().unwrap();
        assert!(source.error.is_some(), "losing both provenance and conversion remains an error");
    }
}

#[test]
fn saved_conversion_cannot_borrow_another_sessions_reload_workspace_when_original_is_missing() {
    use rustgis_engine::{
        data::source::Family,
        map::source::{DuckdbOrigin, GdalOrigin, SourceLayer},
    };
    for duckdb in [false, true] {
        let f = Fixture::new("restore-borrowed-conversion");
        let original = f.file(if duckdb { "removed.duckdb" } else { "removed.gdb" });
        let workspace = f.file("reload-another-session");
        std::fs::create_dir(&workspace).unwrap();
        let converted = workspace.join(if duckdb { "converted.parquet" } else { "converted.fgb" });
        io::write_path(&points(&["rank"], Some(8.0)), &converted).unwrap();
        let handle = rustgis_engine::data::source::open_path(&converted, None).unwrap();
        let mut source = SourceLayer::from_path(converted.to_str().unwrap(), None, handle);
        if duckdb {
            source.duckdb = Some(DuckdbOrigin {
                sql: None,
                database: Some(original.to_str().unwrap().into()),
                table: Some("survey".into()),
                crs: None,
                family: Family::Points,
            });
        } else {
            source.gdal = Some(GdalOrigin {
                path: original.to_str().unwrap().into(),
                layer: "survey".into(),
                family: Some(Family::Points),
                crs: None,
            });
        }
        let mut owner = session();
        let id = owner.map_mut().add_streamed("Borrowed conversion", source);
        let project = f.file("survey.rgis");
        owner.run("project.saveAs", &json!({"path": project})).unwrap();
        let mut reopened = session();
        reopened.run("project.open", &json!({"path": project})).unwrap();
        assert!(converted.exists(), "the other session's conversion is still readable");
        assert_eq!(first_point(&loaded(&owner, id)), Coord::new(8.0, 48.0));
        let source = reopened.map().layer(id).unwrap().source.as_ref().unwrap();
        assert!(!source.is_open(), "the restored layer must not install an unowned disposable reader");
        let error = source.error.as_ref().expect("the unavailable original must remain a layer error");
        assert!(error.contains(original.to_str().unwrap()), "{error}");
        assert!(reopened.notices_since(0).iter().all(|n| !n.text.contains("showing the saved conversion")));
    }
}

#[test]
fn load_all_after_empty_duckdb_reload_exports_a_point_geopackage() {
    use rustgis_engine::data::source::duckdb;
    if !duckdb::available() {
        return;
    }
    let f = Fixture::new("reload-duckdb-empty-export");
    let path = f.file("survey.duckdb");
    reload_duckdb_sql(&path, "CREATE TABLE survey AS SELECT 7 AS rank, ST_Point(2,48) AS geom");
    let mut s = session();
    s.run("layer.addDatabase", &json!({"database": path, "table": "survey", "stream": true})).unwrap();
    let id = s.active_layer.unwrap();
    reload_duckdb_sql(&path, "DELETE FROM survey");
    s.run("layer.reloadData", &json!({"layer": id})).unwrap();
    s.run("layer.loadAll", &json!({"layer": id})).unwrap();
    let layer = s.map().layer(id).unwrap();
    assert!(layer.source.is_none());
    assert!(layer.dataset.features.is_empty());
    assert_eq!(layer.dataset.geometry_type, GeometryType::Point);
    let output = f.file("empty.gpkg");
    s.run("share.exportLayer", &json!({"layer": id, "path": output})).unwrap();
    let exported = io::read_path(&output).unwrap().0;
    assert!(exported.features.is_empty());
    assert_eq!(exported.geometry_type, GeometryType::Point, "Load All must not turn the GeoPackage into an attributes-only table");
}

#[test]
fn direct_export_after_empty_duckdb_reload_is_a_point_geopackage() {
    use rustgis_engine::data::source::duckdb;
    if !duckdb::available() {
        return;
    }
    let f = Fixture::new("reload-duckdb-empty-direct-export");
    let path = f.file("survey.duckdb");
    reload_duckdb_sql(&path, "CREATE TABLE survey AS SELECT 7 AS rank, ST_Point(2,48) AS geom");
    let mut s = session();
    s.run("layer.addDatabase", &json!({"database": path, "table": "survey", "stream": true})).unwrap();
    let id = s.active_layer.unwrap();
    reload_duckdb_sql(&path, "DELETE FROM survey");
    s.run("layer.reloadData", &json!({"layer": id})).unwrap();
    assert!(s.map().layer(id).unwrap().source.is_some(), "the layer is still streamed");
    let output = f.file("empty.gpkg");
    s.run("share.exportLayer", &json!({"layer": id, "path": output})).unwrap();
    let exported = io::read_path(&output).unwrap().0;
    assert!(exported.features.is_empty());
    assert_eq!(exported.geometry_type, GeometryType::Point, "a direct export must keep the layer's declared geometry type");
}

#[test]
fn project_open_reports_fresh_conversion_losses_once_per_kind_but_not_clean_conversions() {
    use rustgis_engine::{
        data::source::{Family, gdal},
        map::source::{GdalOrigin, SourceLayer},
    };
    if !gdal::usable() {
        return;
    }
    for layer_count in [1, 2] {
        for lossy in [true, false] {
            let f = Fixture::new("restore-fresh-conversion-notices");
            let mut saved = session();
            let mut originals = Vec::new();
            let mut workspaces = Vec::new();
            for index in 0..layer_count {
                let path = f.file(&format!("survey{index}.geojsonl"));
                let publish = |coordinates: &str| {
                    std::fs::write(
                        &path,
                        format!(r#"{{"type":"Feature","properties":{{}},"geometry":{{"type":"Point","coordinates":{coordinates}}}}}"#),
                    )
                    .unwrap();
                };
                publish("[2,48]");
                let seed = f.file(&format!("seed{index}.fgb"));
                io::write_path(&points(&[], Some(2.0)), &seed).unwrap();
                let handle = rustgis_engine::data::source::open_path(&seed, None).unwrap();
                let mut source = SourceLayer::from_path(seed.to_str().unwrap(), None, handle);
                source.gdal = Some(GdalOrigin {
                    path: path.to_str().unwrap().into(),
                    layer: format!("survey{index}"),
                    family: Some(Family::Points),
                    crs: None,
                });
                let source = source.relinked(path.to_str().unwrap(), None).unwrap();
                assert!(source.info().unwrap().notes.is_empty(), "the saved conversion itself was clean");
                workspaces.push(std::path::PathBuf::from(source.path.as_ref().unwrap()).parent().unwrap().to_path_buf());
                saved.map_mut().add_streamed(format!("Survey {index}"), source);
                publish(if lossy { "[8,48,100]" } else { "[8,48]" });
                originals.push(path);
            }
            let project = f.file("survey.rgis");
            saved.run("project.saveAs", &json!({"path": project})).unwrap();
            drop(saved);
            assert!(workspaces.iter().all(|path| !path.exists()), "saved disposable workspaces must be gone before reopening");
            assert!(originals.iter().all(|path| path.exists()));

            let mut reopened = session();
            let mark = reopened.notice_mark();
            reopened.run("project.open", &json!({"path": project})).unwrap();
            let notices = reopened.notices_since(mark);
            let losses = notices.iter().filter(|notice| notice.text.contains("Z or M coordinates")).collect::<Vec<_>>();
            if lossy {
                assert_eq!(losses.len(), 1, "{notices:?}");
                let first = format!("Survey {}", layer_count - 1);
                if layer_count == 1 {
                    assert!(losses[0].text.starts_with(&format!("{first}: ")), "{}", losses[0].text);
                } else {
                    assert!(losses[0].text.starts_with("2 layers: "), "{}", losses[0].text);
                    assert!(losses[0].text.ends_with(&format!("first: {first}")), "{}", losses[0].text);
                }
                assert!(losses[0].text.contains("1 source feature(s)"), "{}", losses[0].text);
                assert!(!losses[0].error);
            } else {
                assert!(losses.is_empty(), "{notices:?}");
                assert_eq!(notices.len(), 1, "clean conversions only report that the project opened: {notices:?}");
            }
            for layer in &mut reopened.map_mut().layers {
                let source = layer.source.as_mut().unwrap();
                assert_eq!(source.info().unwrap().zm_dropped, lossy);
                assert!(source.take_conversion_notes().is_empty(), "opening consumed conversion diagnostics exactly once");
                assert_eq!(first_point(&layer.load_all().unwrap()), Coord::new(8.0, 48.0));
            }
        }
    }
}

#[test]
fn project_open_sums_counted_conversion_losses_across_layers_and_keeps_single_layer_wording() {
    use rustgis_engine::{
        data::source::{Family, gdal},
        map::source::{GdalOrigin, SourceLayer},
    };
    if !gdal::usable() {
        return;
    }
    for layer_count in [1, 2, 3] {
        for lossy in [true, false] {
            let f = Fixture::new("restore-counted-conversion-losses");
            let mut saved = session();
            for index in 0..layer_count {
                let path = f.file(&format!("survey{index}.geojsonl"));
                let point = r#"{"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[2,48]}}"#;
                let line = r#"{"type":"Feature","properties":{},"geometry":{"type":"LineString","coordinates":[[2,48],[3,48]]}}"#;
                std::fs::write(&path, format!("{point}\n")).unwrap();
                let seed = f.file(&format!("seed{index}.fgb"));
                io::write_path(&points(&[], Some(2.0)), &seed).unwrap();
                let handle = rustgis_engine::data::source::open_path(&seed, None).unwrap();
                let mut source = SourceLayer::from_path(seed.to_str().unwrap(), None, handle);
                source.gdal = Some(GdalOrigin {
                    path: path.to_str().unwrap().into(),
                    layer: format!("survey{index}"),
                    family: Some(Family::Points),
                    crs: None,
                });
                let source = source.relinked(path.to_str().unwrap(), None).unwrap();
                assert!(source.info().unwrap().notes.is_empty(), "the saved conversion was clean");
                saved.map_mut().add_streamed(format!("Survey {index}"), source);
                let mut input = format!("{point}\n");
                if lossy {
                    for _ in 0..=index {
                        input.push_str(line);
                        input.push('\n');
                    }
                }
                std::fs::write(&path, input).unwrap();
            }
            let project = f.file("survey.rgis");
            saved.run("project.saveAs", &json!({"path": project})).unwrap();
            drop(saved);
            let mut reopened = session();
            let mark = reopened.notice_mark();
            reopened.run("project.open", &json!({"path": project})).unwrap();
            let notices = reopened.notices_since(mark);
            let losses = notices.iter().filter(|notice| notice.text.contains("another geometry type")).collect::<Vec<_>>();
            if lossy {
                let total = layer_count * (layer_count + 1) / 2;
                let note = format!("GDAL: {total} features of another geometry type were left out");
                let expected = if layer_count == 1 {
                    format!("Survey 0: {note}")
                } else {
                    format!("{layer_count} layers: {note}; first: Survey {}", layer_count - 1)
                };
                assert_eq!(losses.len(), 1, "{notices:?}");
                assert_eq!(losses[0].text, expected);
                assert!(!losses[0].error);
            } else {
                assert!(losses.is_empty(), "{notices:?}");
                assert_eq!(notices.len(), 1, "clean conversions only announce opening: {notices:?}");
            }
        }
    }
}

#[test]
fn reload_generic_gdal_layer_reports_selected_zm_loss_but_not_xy_or_other_families() {
    use rustgis_engine::{
        data::source::{Family, gdal},
        map::source::{GdalOrigin, SourceLayer},
    };
    if !gdal::usable() {
        return;
    }
    let f = Fixture::new("reload-gdal-generic-dimensions");
    let path = f.file("survey.geojsonl");
    let publish = |point_coordinates: &str, line_coordinates: &str| {
        std::fs::write(
            &path,
            format!(
                "{{\"type\":\"Feature\",\"properties\":{{}},\"geometry\":{{\"type\":\"Point\",\"coordinates\":[2,48]}}}}\n\
                 {{\"type\":\"Feature\",\"properties\":{{}},\"geometry\":{{\"type\":\"Point\",\"coordinates\":{point_coordinates}}}}}\n\
                 {{\"type\":\"Feature\",\"properties\":{{}},\"geometry\":{{\"type\":\"LineString\",\"coordinates\":{line_coordinates}}}}}\n"
            ),
        )
        .unwrap();
    };
    publish("[3,48]", "[[2,48],[3,48]]");
    let original = gdal::convert_one(&path, "survey", Some(Family::Points), None).unwrap().unwrap();
    let handle = rustgis_engine::data::source::open_path(&original, None).unwrap();
    let mut source = SourceLayer::from_path(original.to_str().unwrap(), None, handle);
    source.gdal = Some(GdalOrigin { path: path.to_str().unwrap().into(), layer: "survey".into(), family: Some(Family::Points), crs: None });
    let mut s = session();
    let id = s.map_mut().add_streamed("Generic survey", source);
    for (point_coordinates, line_coordinates, lossy) in
        [("[8,48,100]", "[[2,48],[3,48]]", true), ("[8,48]", "[[2,48],[3,48]]", false), ("[8,48]", "[[2,48,100],[3,48,100]]", false)]
    {
        publish(point_coordinates, line_coordinates);
        let mark = s.notice_mark();
        s.run("layer.reloadData", &json!({"layer": id})).unwrap();
        let notices = s.notices_since(mark);
        let losses = notices.iter().filter(|notice| notice.text.contains("Z or M coordinates")).collect::<Vec<_>>();
        if lossy {
            assert_eq!(losses.len(), 1, "{notices:?}");
            assert!(losses[0].text.contains("Z or M coordinates on 1 source feature(s)"), "{}", losses[0].text);
            assert!(!losses[0].error);
        } else {
            assert!(losses.is_empty(), "{notices:?}");
        }
        let source = s.map().layer(id).unwrap().source.as_ref().unwrap();
        assert_eq!(source.info().unwrap().zm_dropped, lossy);
        assert_eq!(source.info().unwrap().notes.iter().any(|note| note.contains("Z or M coordinates")), lossy);
        let dataset = loaded(&s, id);
        assert_eq!(dataset.features.len(), 2);
        assert_eq!(dataset.geometry_type, GeometryType::Point);
        let mut xs = dataset.features.iter().map(|feature| feature.geometry.as_ref().unwrap().extent().center().x).collect::<Vec<_>>();
        xs.sort_by(f64::total_cmp);
        assert_eq!(xs, [2.0, 8.0]);
    }
}
