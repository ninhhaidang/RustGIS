//! The first-check test and boundary sweep share fixtures, lists and desktop completion paths.
use super::*;
use rustgis_analysis::control::Control;

/// Runs the sample commands under first-check cancellation or successive injected cancellations.
pub(super) fn long_commands(boundaries: bool) {
    // Removed however the test ends.
    struct Dir(std::path::PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let dir = Dir(std::env::temp_dir().join(format!("rustgis-cancel-{boundaries}-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir.0);
    std::fs::create_dir_all(&dir.0).unwrap();
    let out = |f: &str| dir.0.join(f).display().to_string();
    let mut s = world();
    if boundaries {
        // A few sample rows and simple shapes exercise the same commands without a world-sized
        // overlay on every retry. Fields, names and renderers remain those of the sample.
        for layer in &mut s.map_mut().layers {
            layer.dataset.features.truncate(4);
            for (i, feature) in layer.dataset.features.iter_mut().enumerate() {
                use rustgis_data::{Coord, Geometry};
                let x = 10.0 + i as f64 * 0.01;
                let a = Coord::new(x, 45.0);
                let b = Coord::new(x + 0.02, 45.03);
                feature.geometry = feature.geometry.as_ref().map(|g| match g {
                    Geometry::Point(_) => Geometry::Point(Coord::new(x, 45.005 + i as f64 * 0.005)),
                    Geometry::Line(_) => Geometry::Line(vec![vec![a, b]]),
                    _ => Geometry::Polygon(vec![vec![vec![a, Coord::new(b.x, a.y), b, Coord::new(a.x, b.y), a]]]),
                });
            }
            layer.touch_geometry();
        }
    }
    s.run("layer.addDataBytes", &json!({"fileName": "dem.tif", "data": if boundaries { tiny_raster(false) } else { dem_b64() }})).unwrap();
    let dem = s.map().layers.iter().find(|l| l.is_raster()).unwrap().name.clone();
    // What some of them read: a later DEM, a slope, training squares, an imagery workspace, a
    // two-class scene with its training polygons, and a layout.
    s.run("raster.function", &json!({"layer": dem, "function": "bandMath", "expression": "b1 + 10", "name": "dem_later"})).unwrap();
    s.run("raster.function", &json!({"layer": dem, "function": "slope", "name": "dem_slope"})).unwrap();
    s.run("layer.template", &json!({"template": "polygons", "name": "Squares"})).unwrap();
    for (name, x) in [("low", 10.002), ("high", 10.018)] {
        let square = [[x, 45.016], [x + 0.008, 45.016], [x + 0.008, 45.024], [x, 45.024], [x, 45.016]];
        let geometry = json!({"type": "Polygon", "coordinates": [square]});
        s.run("edit.addFeature", &json!({"layer": "Squares", "geometry": geometry, "attributes": {"name": name}})).unwrap();
    }
    s.run("imagery.workspace", &json!({"kind": "ortho", "name": "Valley", "images": [dem, "dem_later"]})).unwrap();
    s.run("imagery.workspaceOptions", &json!({"name": "Valley", "cellSize": 0.002})).unwrap();
    s.run("layer.addDataBytes", &json!({"fileName": "scene.tif", "data": if boundaries { tiny_raster(true) } else { two_class_b64() }}))
        .unwrap();
    let training = json!({"type": "FeatureCollection", "features": [
        {"type": "Feature", "properties": {"class": "water"}, "geometry": {"type": "Polygon", "coordinates": [[[10.002, 45.002], [10.015, 45.002], [10.015, 45.035], [10.002, 45.035], [10.002, 45.002]]]}},
        {"type": "Feature", "properties": {"class": "field"}, "geometry": {"type": "Polygon", "coordinates": [[[10.025, 45.002], [10.038, 45.002], [10.038, 45.035], [10.025, 45.035], [10.025, 45.002]]]}}]});
    s.run("layer.addDataText", &json!({"fileName": "training.geojson", "text": training.to_string()})).unwrap();
    s.run("layout.new", &json!({"page": "a4", "orientation": "landscape", "template": "blank"})).unwrap();
    s.run("layout.addFrame", &json!({"x": 20, "y": 20, "w": 150, "h": 150})).unwrap();
    // A layer read from a Shapefile, edited, to publish.
    s.run("share.exportLayer", &json!({"layer": "Cities", "path": out("towns.shp")})).unwrap();
    s.run("layer.addData", &json!({"path": out("towns.shp"), "name": "towns"})).unwrap();
    s.run("edit.setValue", &json!({"layer": "towns", "row": 0, "field": "name", "value": "Renamed"})).unwrap();

    let rasters = [
        ("analysis.viewshed", json!({"layer": dem, "lon": 10.0055, "lat": 45.0395, "height": 2})),
        ("analysis.interpolate", json!({"layer": "Cities", "field": "pop_max", "cellSize": if boundaries { 0.01 } else { 1.0 }})),
        (
            "analysis.suitability",
            json!({"criteria": [
                {"layer": dem, "ranges": [[0, 250, 1], [250, 1000, 10]], "weight": 2},
                {"layer": "dem_slope", "ranges": [[0, 5, 10], [5, 90, 1]], "weight": 1}]}),
        ),
        ("raster.function", json!({"layer": dem, "function": "hillshade"})),
        ("raster.functionChain", json!({"layer": dem, "steps": [{"function": "slope"}, {"function": "threshold", "value": 10}]})),
        ("raster.classify", json!({"layer": dem, "classes": 3})),
        ("raster.classifySupervised", json!({"layer": dem, "training": "Squares", "classField": "name"})),
        ("raster.change", json!({"before": dem, "after": "dem_later", "method": "difference"})),
        ("raster.index", json!({"layer": dem, "index": "ndvi", "nir": 1, "red": 1})),
        ("raster.process", json!({"layer": dem, "operation": "resample", "factor": 4})),
        ("imagery.mosaic", json!({"name": "Valley"})),
        (
            "dl.classify",
            json!({"layer": "scene", "training": "training", "classField": "class", "bands": [1, 2], "epochs": if boundaries { 2 } else { 30 }, "model": "landcover"}),
        ),
        // Cancelled before the index is read: nothing reaches the network.
        ("embeddings.similarity", json!({"dataset": "alphaearth", "point": [10.01, 45.01], "bbox": [10.0, 45.0, 10.02, 45.02]})),
        ("embeddings.change", json!({"dataset": "alphaearth", "bbox": [10.0, 45.0, 10.02, 45.02]})),
        // Cancelled before a band's file is read: nothing reaches the network.
        (
            "stac.addBands",
            json!({"item": {"type": "Feature", "id": "scene", "bbox": [10.0, 45.0, 10.02, 45.02], "properties": {"datetime": "2024-06-01T00:00:00Z"},
                "assets": {
                    "red": {"href": "https://example.org/red.tif", "type": "image/tiff; application=geotiff; profile=cloud-optimized", "eo:bands": [{"common_name": "red"}]},
                    "green": {"href": "https://example.org/green.tif", "type": "image/tiff; application=geotiff; profile=cloud-optimized", "eo:bands": [{"common_name": "green"}]},
                    "blue": {"href": "https://example.org/blue.tif", "type": "image/tiff; application=geotiff; profile=cloud-optimized", "eo:bands": [{"common_name": "blue"}]}}},
                "bbox": [10.0, 45.0, 10.02, 45.02]}),
        ),
        // The same: an index reads two files and nothing reaches the network.
        (
            "stac.addIndex",
            json!({"item": {"type": "Feature", "id": "scene", "bbox": [10.0, 45.0, 10.02, 45.02], "properties": {"datetime": "2024-06-01T00:00:00Z"},
                "assets": {
                    "red": {"href": "https://example.org/red.tif", "type": "image/tiff; application=geotiff; profile=cloud-optimized", "eo:bands": [{"common_name": "red"}]},
                    "nir": {"href": "https://example.org/nir.tif", "type": "image/tiff; application=geotiff; profile=cloud-optimized", "eo:bands": [{"common_name": "nir"}]}}},
                "index": "ndvi", "bbox": [10.0, 45.0, 10.02, 45.02]}),
        ),
    ];
    // Every command that runs in the background is here, and only those.
    /// Keeps the shared fixture list in step with the production registry.
    fn listed<'a>(l: &[(&'a str, Value)]) -> std::collections::BTreeSet<&'a str> {
        l.iter().map(|(c, _)| *c).collect()
    }
    let registered = crate::cmd::workflows::IN_BACKGROUND.iter().map(|(c, ..)| *c).collect::<std::collections::BTreeSet<_>>();
    assert_eq!(listed(&rasters), registered, "add a new background raster command here");
    for (command, p) in &rasters {
        // Embeddings and STAC bands read from the network beyond their first check. Keep the
        // first-check coverage, but never run their boundary sweep or retry against the network.
        if !boundaries || !(command.starts_with("embeddings.") || matches!(*command, "stac.addBands" | "stac.addIndex")) {
            exercise(&s, command, p, Kind::Raster, &dir.0, boundaries);
        }
    }

    let exports = [
        ("data.convert", json!({"layer": "Cities", "format": "geojson", "path": out("converted.geojson")})),
        ("share.exportLayer", json!({"layer": "Cities", "path": out("cities.geojson")})),
        (
            "share.exportTable",
            json!({"table": "ranks", "path": out("ranks.csv"), "sort": [{"field": "score", "descending": true}], "limit": 1}),
        ),
        ("share.exportRaster", json!({"layer": dem, "path": out("dem.tif")})),
        ("share.exportMap", json!({"path": out("map.png"), "width": 64, "height": 48})),
        ("map.exportOrbit", json!({"path": out("orbit.zip"), "frames": 3, "width": 64, "height": 64, "dpi": 48})),
        ("map.exportBlock", json!({"path": out("block.glb"), "texture": 256})),
        ("layout.export", json!({"path": out("layout.pdf"), "dpi": 36})),
        ("report.export", json!({"path": out("report.pdf"), "dpi": 36})),
        ("source.publish", json!({"layer": "towns"})),
    ];
    s.run("report.new", &json!({"layer": "Cities"})).unwrap();
    // A standalone table to export ranked and limited.
    s.run("layer.addDataText", &json!({"fileName": "ranks.csv", "text": "name,score\na,1\nb,3\n"})).unwrap();
    let registered = crate::cmd::exports::IN_BACKGROUND.iter().map(|(c, ..)| *c).collect::<std::collections::BTreeSet<_>>();
    assert_eq!(listed(&exports), registered, "add a new background export here");
    for (command, p) in &exports {
        // A block's model needs a block on the DEM, which the other exports don't draw.
        let block = *command == "map.exportBlock";
        if block {
            s.run("map.terrain", &json!({"layer": dem, "block": {}})).unwrap();
        }
        exercise(&s, command, p, Kind::Export, &dir.0, boundaries);
        if block {
            s.run("map.terrain", &json!({"off": true})).unwrap();
        }
    }

    // The Add Service dialog replays supplied metadata off the desktop frame too.
    let params = json!({"url":"https://maps.example.org/dataset","type":"ogcapi","text":"{\"collections\":[]}"});
    for command in ["service.layers", "service.add"] {
        // Adding a service needs a real collection and remote features to succeed. An empty
        // listing is useful only for the original first-check cancellation test.
        if !boundaries || command == "service.layers" {
            exercise(&s, command, &params, Kind::Read, &dir.0, boundaries);
        }
    }
    // Shape fitting uses the same cancellable read queue, without modifying its source. Its
    // fixture is an editing session of its own, which the boundary sweep's sessions don't carry,
    // so it keeps first-check coverage.
    if !boundaries {
        let mut shapes = Session::new(Project::default());
        let id = shapes.map_mut().add_dataset(rustgis_data::Dataset::new("Conversion", rustgis_data::GeometryType::Polygon, vec![]));
        shapes.active_layer = Some(id);
        shapes.run("edit.enable", &json!({"on":true})).unwrap();
        shapes.run("edit.addBox", &json!({"box":{"center":[0,0],"width":100,"height":60,"angle":25}})).unwrap();
        let before = contents(&shapes.project);
        let control = Control::cancelled_once_started();
        let result = Kind::Read.run(&mut shapes, "edit.previewShapeConversion", &json!({"layer":id,"row":0,"kind":"rotatedBox"}), &control);
        assert!(result.as_ref().is_err_and(|e| e.contains("cancelled")), "edit.previewShapeConversion ran on after Cancel: {result:?}");
        assert!(control.looked() > 0, "edit.previewShapeConversion never looks at Cancel");
        assert!(contents(&shapes.project) == before, "edit.previewShapeConversion changed the project after Cancel");
    }
    // A PDF's page is read on the same queue: the file, then its drawing, Cancel looked at between
    // (#2336).
    s.run("layout.new", &json!({"page": "a4", "template": "map"})).unwrap();
    s.run("layout.export", &json!({"path": out("page.pdf"), "dpi": 36, "allPages": false})).unwrap();
    exercise(&s, "layer.addData", &json!({"path": out("page.pdf")}), Kind::Read, &dir.0, boundaries);
    exercise(&s, "table.statistics", &json!({"layer": "Cities", "field": "pop_max"}), Kind::Read, &dir.0, boundaries);
    // A Zarr point series is read on a thread of its own by the Time Series pane (#2282). It isn't
    // one of the queues above (it has no read-ahead), so it is run here: a local store, swept at
    // each of its checks in the boundary mode. It changes nothing in the project.
    {
        // A store per mode: both run in one test process, and each removes its store when done.
        let (store, path) = zarr_store_over_time(if boundaries { "cancel-series-boundaries" } else { "cancel-series-first" });
        let p = json!({"path": path, "variable": "t", "lon": 1.0, "lat": 30.0});
        let run = crate::cmd::zarr::series_start(&s, &p).unwrap();
        let before = contents(&s.project);
        if boundaries {
            let count = Control::cancelled_at_look(u64::MAX);
            let expected = run.run(&count).0.unwrap();
            assert!(count.looked() > 0 && count.looked() < 256, "zarr.series: {} checks", count.looked());
            for n in 1..=count.looked() + 1 {
                let control = Control::cancelled_at_look(n);
                let (result, _) = run.run(&control);
                if n <= count.looked() {
                    assert!(result.as_ref().is_err_and(|e| e.contains("cancelled")), "zarr.series, check {n}: {result:?}");
                    // A retry at once gives the whole series.
                    assert_eq!(run.run(&Control::new()).0.unwrap(), expected, "zarr.series, check {n}: retry");
                } else {
                    assert_eq!(result.unwrap(), expected, "zarr.series, check {n}: past its last check");
                }
            }
        } else {
            let control = Control::cancelled_once_started();
            let (result, _) = run.run(&control);
            assert!(result.as_ref().is_err_and(|e| e.contains("cancelled")), "zarr.series ran on after Cancel: {result:?}");
            assert!(control.looked() > 0, "zarr.series never looks at Cancel");
        }
        assert!(contents(&s.project) == before, "zarr.series changed the project");
        let _ = std::fs::remove_dir_all(store);
    }
    // Automatic size bounds scan the numeric field on that queue too.
    s.run("layer.symbology", &json!({"layer": "Cities", "type": "unique", "field": "name"})).unwrap();
    exercise(&s, "layer.sizeBy", &json!({"layer": "Cities", "field": "pop_max"}), Kind::Read, &dir.0, boundaries);

    s.run("layer.addDataText", &json!({"fileName": "calc.csv", "text": "n,day\n1,01/02/2024\n2,02/03/2024\n"})).unwrap();
    s.run("edit.enable", &json!({"on": true})).unwrap();
    let edits = [
        ("edit.setValue", json!({"table": "calc", "row": 0, "field": "n", "value": 3})),
        ("edit.addFeature", json!({"table": "calc", "attributes": {"n": 3}})),
        ("edit.addField", json!({"table": "calc", "name": "extra", "type": "text"})),
        ("edit.deleteField", json!({"table": "calc", "field": "day"})),
        ("edit.convertField", json!({"table": "calc", "field": "day", "type": "date", "format": "%m/%d/%Y"})),
        ("edit.calculateField", json!({"table": "calc", "field": "n", "expression": "n * 2"})),
    ];
    assert_eq!(listed(&edits), crate::cmd::edit::TABLE_EDITS.iter().map(|(id, ..)| *id).collect(), "add a new table worker here");
    for (command, p) in edits {
        exercise(&s, command, &p, Kind::Read, &dir.0, boundaries);
        let script = json!({"script": format!("{command} {p}")});
        exercise(&s, "script.run", &script, Kind::Read, &dir.0, boundaries);
    }
    s.run(
        "layer.addDataText",
        &json!({"fileName": "dated.geojson", "text": json!({
        "type": "FeatureCollection", "features": [{"type": "Feature", "properties": {"day": "01/02/2024"},
        "geometry": {"type": "Point", "coordinates": [10.01, 45.01]}}]}).to_string()}),
    )
    .unwrap();
    let params = json!({"layer":"dated", "field":"day", "type":"date", "format":"%m/%d/%Y"});
    exercise(&s, "edit.convertField", &params, Kind::Read, &dir.0, boundaries);

    // Replacement validation can now scan temporal fields as well as probing geometry.
    let mut dates = rustgis_data::Dataset::new(
        "dates",
        rustgis_data::GeometryType::Point,
        vec![rustgis_data::Field::new("day", rustgis_data::FieldType::Date)],
    );
    dates.push(
        Some(rustgis_data::Geometry::Point(rustgis_data::Coord::new(0.0, 0.0))),
        vec![rustgis_data::Value::parse("2024-02-29", rustgis_data::FieldType::Date)],
    );
    let replacement = out("cancel-repair-source.fgb");
    std::fs::write(&replacement, rustgis_data::source::fgb::write(&dates).unwrap()).unwrap();
    let id = s.run("layer.addData", &json!({"path": replacement, "stream": true})).unwrap()["layer"].as_u64().unwrap();
    exercise(&s, "layer.repairSource", &json!({"layer": id, "path": replacement}), Kind::Read, &dir.0, boundaries);
    std::fs::remove_file(replacement).unwrap();

    // The tools whose work grows past a second or two on real data: buffers, overlays, joins,
    // hulls, densities, interpolation and network searches. Run in the background as the
    // Analysis pane runs them; a new long tool is added here (AGENTS.md › Never hold the window).
    let tools = [
        ("buffer", json!({"input": "Cities", "distance": 100})),
        ("dissolve", json!({"input": "Countries"})),
        ("spatialJoin", json!({"target": "Cities", "join": "Countries"})),
        ("clip", json!({"input": "Rivers", "clip": "Countries"})),
        ("erase", json!({"input": "Rivers", "erase": "Countries"})),
        ("intersect", json!({"input": "Rivers", "overlay": "Countries"})),
        ("identity", json!({"input": "Rivers", "overlay": "Countries"})),
        ("union", json!({"input": "Lakes", "overlay": "Countries"})),
        ("symmetricalDifference", json!({"input": "Lakes", "overlay": "Countries"})),
        ("update", json!({"input": "Countries", "update": "Lakes"})),
        ("split", json!({"input": "Rivers", "split": "Countries"})),
        ("simplify", json!({"input": "Countries", "tolerance": 1})),
        ("countPoints", json!({"polygons": "Countries", "points": "Cities"})),
        // Case fields alone: no statistic, so only the group loop looks at Cancel.
        ("summaryStatistics", json!({"input": "Cities", "case": "featurecla"})),
        ("convexHull", json!({"input": "Countries", "group": "None", "output": "Hulls_each"})),
        ("convexHull", json!({"input": "Countries", "group": "All", "output": "Hulls_all"})),
        ("kernelDensity", json!({"input": "Cities", "crs": "EPSG:3857", "pixelSize": if boundaries { 2000 } else { 1000000 }})),
        ("kriging", json!({"input": "Cities", "field": "pop_max", "pixelSize": if boundaries { 0.01 } else { 1.0 }})),
        ("tabulateArea", json!({"raster": dem, "zones": "Countries"})),
        ("tabulateIntersection", json!({"zones": "Countries", "classes": "Squares", "includeEmptyZones": true})),
        ("serviceArea", json!({"network": "Rivers", "facilities": "Cities", "breaks": "50", "snapDistance": 100000})),
        ("odCostMatrix", json!({"network": "Rivers", "origins": "Cities", "destinations": "Cities", "count": 1, "snapDistance": 100000})),
    ];
    for (tool, params) in &tools {
        exercise(&s, tool, params, Kind::Tool, &dir.0, boundaries);
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Raster,
    Export,
    Read,
    Tool,
}

impl Kind {
    /// Uses the same prepare/run/land (or abandon) sequence as the desktop, including history.
    fn run(self, s: &mut Session, command: &str, p: &Value, control: &std::sync::Arc<Control>) -> crate::CmdResult {
        match self {
            Self::Raster => {
                let run = crate::cmd::workflows::raster_start(s, command, p)?;
                let id = run.id;
                match run.run(control) {
                    Ok(made) => {
                        assert!(crate::cmd::workflows::raster_land(s, id, made)?);
                        let mut p = p.clone();
                        p["ran"] = json!(id);
                        s.run(command, &p)
                    }
                    Err(e) => {
                        crate::cmd::workflows::raster_abandon(s, id);
                        Err(e)
                    }
                }
            }
            Self::Export => {
                let run = crate::cmd::exports::export_start(s, command, p)?;
                let started = run.started();
                let done = run.run(control);
                crate::cmd::exports::export_land(s, started, done)
            }
            Self::Read => {
                let read = crate::cmd::reads::read_ahead(s, command, p)?;
                assert!(read.reads(), "{command} should prepare background work");
                let done = read.run(control)?;
                crate::cmd::reads::read_land(s, done)
            }
            Self::Tool => {
                let run = crate::cmd::gp::tool_start(s, command, p)?;
                let id = run.id;
                let done = run.run(control);
                crate::cmd::gp::tool_complete(s, id, done)?;
                s.run("gp.finish", &json!({"id": id}))
            }
        }
    }
}

/// Saved project data plus decoded raster pixels (which project serialization omits). Pixel
/// bits retain NaNs and signed zero. Include streamed rows omitted by project serialization;
/// normalize GP record timestamps/durations and geometry cache generations, which naturally
/// differ on a successful retry or Undo/Redo. All features, pixels and layer settings remain.
fn contents(project: &Project) -> Value {
    let pixels: Vec<_> = project
        .maps
        .iter()
        .flat_map(|m| &m.layers)
        .map(|l| {
            l.raster
                .as_ref()
                .and_then(|r| r.data.as_ref())
                .map(|r| r.bands.iter().map(|b| b.data.iter().map(|v| v.to_bits()).collect::<Vec<_>>()).collect::<Vec<_>>())
        })
        .collect();
    let windows: Vec<_> =
        project.maps.iter().flat_map(|m| &m.layers).map(|l| l.dataset.windowed.then(|| json!(&l.dataset.features))).collect();
    let mut saved = serde_json::to_value(project).unwrap();
    for map in saved["maps"].as_array_mut().unwrap() {
        for layer in map["layers"].as_array_mut().unwrap() {
            layer.as_object_mut().unwrap().remove("geometry_rev");
        }
    }
    for record in saved["gp_history"].as_array_mut().unwrap() {
        record["at"] = json!(0);
        record["millis"] = json!(0);
    }
    json!({"project": saved, "pixels": pixels, "windows": windows})
}

/// Includes both history branches' contents, labels, actors and state identities, so a cancelled
/// command cannot leave a hidden undo entry or discard Redo while appearing to preserve the map.
fn history(s: &Session) -> Value {
    let steps =
        |steps: &[crate::Step]| steps.iter().map(|step| json!([contents(&step.0), step.1, step.2, step.3, step.4])).collect::<Vec<_>>();
    json!([
        steps(&s.undo),
        steps(&s.redo),
        s.state(),
        s.dirty(),
        s.active_layer,
        s.active_layout,
        s.pending.len(),
        s.plugin_runs.len(),
        s.raster_runs.len()
    ])
}

/// A fresh session with meaningful Undo and Redo, on the same fixture, for each boundary.
fn session(fixture: &Session) -> Session {
    let mut s = Session::new(fixture.project.clone());
    s.active_layout = fixture.active_layout;
    s.active_layer = fixture.active_layer;
    for name in ["Cities before", "Cities", "Cities redo"] {
        let id = s.map().layers.iter().find(|l| l.name.starts_with("Cities")).unwrap().id;
        s.run("layer.rename", &json!({"layer": id, "name": name})).unwrap();
    }
    assert!(s.undo());
    s.mark_saved();
    s
}

/// Reads every entry, including sidecars, staging files and empty directories. Comparing the
/// whole tree catches partial outputs even when the intended destination doesn't exist.
fn files(dir: &std::path::Path) -> std::collections::BTreeMap<std::path::PathBuf, Option<Vec<u8>>> {
    /// Retains relative paths so even unexpected staging names take part in the comparison.
    fn visit(root: &std::path::Path, dir: &std::path::Path, out: &mut std::collections::BTreeMap<std::path::PathBuf, Option<Vec<u8>>>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let folder = path.is_dir();
            out.insert(path.strip_prefix(root).unwrap().to_owned(), if folder { None } else { Some(std::fs::read(&path).unwrap()) });
            if folder {
                visit(root, &path, out);
            }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    visit(dir, dir, &mut out);
    out
}

/// Restores only this test's private directory between independent runs; never before a retry.
fn restore(dir: &std::path::Path, before: &std::collections::BTreeMap<std::path::PathBuf, Option<Vec<u8>>>) {
    std::fs::remove_dir_all(dir).unwrap();
    std::fs::create_dir_all(dir).unwrap();
    for (path, bytes) in before {
        let path = dir.join(path);
        if let Some(bytes) = bytes {
            std::fs::write(path, bytes).unwrap();
        } else {
            std::fs::create_dir_all(path).unwrap();
        }
    }
}

/// Sweeps existing checks until a run finishes without reaching N, bounded at 256 to keep this
/// a seconds-long regression, not a stress test. A fixture exceeding the cap must be made smaller.
/// Exports that have already committed a complete file may report success after a late Cancel
/// (`exports::written_under`); they must match a normal run's complete files and session.
fn exercise(fixture: &Session, command: &str, p: &Value, kind: Kind, dir: &std::path::Path, boundaries: bool) {
    const MAX_CHECKS: u64 = 256;
    let files = || {
        let mut found = files(dir);
        if command == "source.publish"
            && let Some(lock) = found.remove(std::path::Path::new(".towns.shp.rustgis-lock"))
        {
            // publish::source_lock deliberately keeps its empty sidecar: unlinking a lock
            // could let concurrent publishers lock different inodes. It contains no data.
            assert_eq!(lock, Some(Vec::new()), "the persistent publisher lock must be empty");
        }
        found
    };
    let before_files = files();
    let mut normal = session(fixture);
    let expected = if boundaries {
        let count = Control::cancelled_at_look(u64::MAX);
        kind.run(&mut normal, command, p, &count).unwrap_or_else(|e| panic!("{command} {p}: normal run: {e}"));
        assert!(count.looked() > 0, "{command} {p}: never looks at Cancel");
        assert!(count.looked() < MAX_CHECKS, "{command} {p}: {} checks; shrink the fixture to fit the cap", count.looked());
        let result = (contents(&normal.project), files());
        restore(dir, &before_files);
        Some(result)
    } else {
        None
    };
    for n in 1..=MAX_CHECKS {
        let mut s = session(fixture);
        let before = contents(&s.project);
        let undo = history(&s);
        let depth = s.undo_depth();
        let control = if boundaries { Control::cancelled_at_look(n) } else { Control::cancelled_once_started() };
        let result = kind.run(&mut s, command, p, &control);
        let what = format!("{command} {p}, check {n} (looked {})", control.looked());
        if let Err(error) = &result {
            assert!(error.contains("cancelled"), "{what}: {result:?}");
            assert!(control.is_cancelled() && control.looked() >= n, "{what}: didn't reach Cancel");
            assert!(contents(&s.project) == before, "{what}: project changed after Cancel");
            assert!(history(&s) == undo, "{what}: history changed after Cancel");
            assert!(files() == before_files, "{what}: partial output or changed input after Cancel");
            if !boundaries {
                return;
            }
            kind.run(&mut s, command, p, &Control::new()).unwrap_or_else(|e| panic!("{what}: retry: {e}"));
        } else {
            assert!(boundaries, "{what}: ran on after Cancel");
        }
        let (expected, complete_files) = expected.as_ref().unwrap();
        assert!(&contents(&s.project) == expected, "{what}: completion differs from normal run");
        assert!(&files() == complete_files, "{what}: completion left incomplete files");
        if result.is_ok() && control.is_cancelled() {
            // A committed export is already complete, and a subsequent invocation still works.
            kind.run(&mut s, command, p, &Control::new()).unwrap_or_else(|e| panic!("{what}: retry after commit: {e}"));
            assert!(&contents(&s.project) == expected, "{what}: retry after commit changed completion");
            assert!(&files() == complete_files, "{what}: retry after commit left incomplete files");
        }
        // Retry directly on the cancelled session before doing anything that could reset it.
        // Then exercise Undo/Redo: an added step restores the pre-command map. A publication's
        // sync record follows its external file and deliberately isn't undone (publish.rs).
        let completed = contents(&s.project);
        let added_step = s.undo_depth() > depth;
        if completed != before && command != "source.publish" {
            assert!(added_step, "{what}: changed the project without an undo step");
        }
        assert!(s.undo(), "{what}: lost Undo");
        if added_step {
            // Table ids are never reused: undo keeps the counter (see the standalone-table removal regression).
            let mut undone = contents(&s.project);
            undone["project"]["next_table_id"] = before["project"]["next_table_id"].clone();
            assert!(undone == before, "{what}: undo didn't restore the map");
            assert!(!s.dirty(), "{what}: undo didn't return to the saved state");
        }
        assert!(s.redo(), "{what}: lost Redo");
        assert!(contents(&s.project) == completed, "{what}: redo didn't restore completion");
        restore(dir, &before_files);
        if !control.is_cancelled() {
            return;
        }
    }
    panic!("{command} {p}: exceeded {MAX_CHECKS} cancellation checks; shrink the fixture");
}

/// An 8×8 DEM or two-class scene over the existing training squares, without I/O shortcuts.
fn tiny_raster(scene: bool) -> String {
    use base64::Engine;
    use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster, geotiff};
    let gt = GeoTransform { origin_x: 10.0, origin_y: 45.04, pixel_width: 0.005, pixel_height: -0.005 };
    let bands = (0..if scene { 3 } else { 1 })
        .map(|b| {
            (0..64)
                .map(|i| if scene { (if i % 8 < 4 { 20 + b * 30 } else { 120 - b * 20 }) as f32 } else { (200 + i * 2) as f32 })
                .collect()
        })
        .collect();
    let raster = Raster::new(
        "fixture",
        8,
        8,
        bands,
        gt,
        Crs::LonLat,
        None,
        if scene { ColorHint::Rgb { alpha: false } } else { ColorHint::Gray },
        "Float32",
    )
    .unwrap();
    base64::engine::general_purpose::STANDARD.encode(geotiff::write(&raster, 0).unwrap())
}
