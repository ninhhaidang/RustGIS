//! A large tiled GeoTIFF on this computer is read in place (#2761): the map reads the part in view
//! at the level the view needs instead of drawing one overview at every zoom, geoprocessing tools
//! read the file, and a project keeps it as the file it is. A file that fits, or one with a
//! sidecar the in-place reader wouldn't apply, is held whole as before.

use super::*;
use crate::cmd::raster::HELD_WHOLE;
use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster};

/// A directory of its own for one test's files, removed when it goes, whether the test passed.
struct Dir(std::path::PathBuf);

impl std::ops::Deref for Dir {
    type Target = std::path::Path;
    fn deref(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn dir(name: &str) -> Dir {
    let d = std::env::temp_dir().join(format!("rustgis-2761-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    Dir(d)
}

/// Holds files whole up to `max` samples on this thread while it lives, then as before.
struct HeldWhole;

impl HeldWhole {
    fn at(max: usize) -> HeldWhole {
        HELD_WHOLE.with(|c| c.set(Some(max)));
        HeldWhole
    }
}

impl Drop for HeldWhole {
    fn drop(&mut self) {
        HELD_WHOLE.with(|c| c.set(None));
    }
}

/// A 1024-pixel RGB cloud-optimised GeoTIFF over 10°–11° E, 45°–46° N in tiles of 512, with its
/// 512 overview: 3.1 million samples. Its red band is a checkerboard of single pixels (200 and
/// 20), which an overview averages away; green and blue climb across and down it.
fn checkerboard(dir: &std::path::Path) -> String {
    const N: u32 = 1024;
    let at = |f: fn(u32, u32) -> f32| (0..N * N).map(|i| f(i % N, i / N)).collect::<Vec<f32>>();
    let bands = vec![at(|x, y| if (x + y) % 2 == 0 { 200.0 } else { 20.0 }), at(|x, _| (x / 4) as f32), at(|_, y| (y / 4) as f32)];
    let gt = GeoTransform { origin_x: 10.0, origin_y: 46.0, pixel_width: 1.0 / f64::from(N), pixel_height: -1.0 / f64::from(N) };
    let r = Raster::new("checks", N, N, bands, gt, Crs::LonLat, None, ColorHint::Rgb { alpha: false }, "UInt8").unwrap();
    let path = dir.join("checks.tif");
    let info = rustgis_data::raster::cogwrite::write_file(&r, 0, &path).unwrap();
    assert_eq!(info.levels, vec![(N, N), (N / 2, N / 2)], "the fixture's levels");
    path.display().to_string()
}

/// The notices given since `mark`, as text.
fn said(s: &Session, mark: u64) -> Vec<String> {
    s.notices_since(mark).iter().map(|n| n.text.clone()).collect()
}

/// The raster layer named `name`.
fn raster<'a>(s: &'a Session, name: &str) -> &'a rustgis_map::raster::RasterLayer {
    s.map().layer_by_name(name).and_then(|l| l.raster.as_ref()).unwrap()
}

/// The map zoomed in on 80 of the file's pixels a side, drawn 320 pixels a side: each of its
/// pixels four of the export's.
fn zoomed_in(s: &mut Session) -> tiny_skia::Pixmap {
    s.run("map.basemap", &json!({"name": "blank"})).unwrap();
    let side = 80.0 / 1024.0;
    s.run("map.zoomToExtent", &json!({"extent": [10.4, 45.4, 10.4 + side, 45.4 + side]})).unwrap();
    let png = crate::cmd::share::map_png(s, Some(320.0), Some(320.0), None).unwrap();
    crate::render::decode_png(&png).unwrap()
}

/// The mean difference of two drawings' red values, pixel for pixel.
fn red_difference(a: &tiny_skia::Pixmap, b: &tiny_skia::Pixmap) -> f64 {
    assert_eq!((a.width(), a.height()), (b.width(), b.height()));
    let total: u64 = a.pixels().iter().zip(b.pixels()).map(|(p, q)| u64::from(p.demultiply().red().abs_diff(q.demultiply().red()))).sum();
    total as f64 / a.pixels().len() as f64
}

/// Added past the size held whole, the file is read in place: the layer holds the 512 overview,
/// says so (and that geoprocessing tools read the file), and draws zoomed in as the file held whole
/// does, its single-pixel checks and all; drawn from the overview it holds, the checks are gone.
#[test]
fn a_large_local_tiled_geotiff_draws_its_full_image_zoomed_in() {
    let d = dir("draw");
    let path = checkerboard(&d);
    // Held whole (the file fits): the full image, drawn from the pixels held.
    let mut whole = Session::new(Project::default());
    whole.run("layer.addData", &json!({"path": path})).unwrap();
    assert!(!raster(&whole, "checks").tiled, "a file that fits is held whole");
    assert_eq!(raster(&whole, "checks").data.as_ref().map(|r| (r.width, r.overview)), Some((1024, None)));
    let reference = zoomed_in(&mut whole);
    // Past the size held whole: read in place.
    let mut s = Session::new(Project::default());
    let mark = s.notice_mark();
    {
        let _held = HeldWhole::at(1_000_000);
        s.run("layer.addData", &json!({"path": path})).unwrap();
    }
    let rl = raster(&s, "checks");
    assert!(rl.tiled && !rl.in_place, "read in place as a file");
    assert_eq!(rl.data.as_ref().map(|r| (r.width, r.height, r.overview)), Some((512, 512, Some((1024, 1024)))));
    let notes = said(&s, mark);
    assert!(
        notes.iter().any(|n| n.starts_with("checks: read in place: the map draws its 1024 × 1024 pixels")
            && n.contains("geoprocessing tools and the commands that make a raster read the file")
            && n.contains("read a 512 × 512 overview")),
        "{notes:?}"
    );
    assert!(!notes.iter().any(|n| n.contains("showing a")), "not said as an overview shown at every zoom: {notes:?}");
    let in_place = zoomed_in(&mut s);
    let near = red_difference(&in_place, &reference);
    assert!(near < 2.0, "zoomed in, the full image is drawn: {near}");
    // Drawn from the overview the layer holds (as before #2761), the checks average away.
    s.map_mut().layers.iter_mut().filter_map(|l| l.raster.as_mut()).for_each(|rl| rl.tiled = false);
    let held = zoomed_in(&mut s);
    let far = red_difference(&held, &reference);
    assert!(far > 40.0, "the overview drawn instead differs: {far}");
}

/// A project keeps the file as a file (nothing says it is read in place), and reads it in place
/// again as it opens, as long as it is still too large to hold whole.
#[test]
fn a_project_keeps_a_tiled_file_as_a_file_and_reads_it_in_place_again() {
    let d = dir("project");
    let path = checkerboard(&d);
    let _held = HeldWhole::at(1_000_000);
    let mut s = Session::new(Project::default());
    s.run("layer.addData", &json!({"path": path})).unwrap();
    let project = d.join("checks.rgis");
    s.run("project.save", &json!({"path": project.display().to_string()})).unwrap();
    let text = std::fs::read_to_string(&project).unwrap();
    assert!(!text.contains("in_place") && !text.contains("tiled"), "saved as a file: {text}");
    let mut opened = Session::new(Project::default());
    opened.run("project.open", &json!({"path": project.display().to_string()})).unwrap();
    let rl = raster(&opened, "checks");
    assert!(rl.tiled, "read in place again");
    assert_eq!(rl.data.as_ref().map(|r| (r.width, r.overview)), Some((512, Some((1024, 1024)))));
    // Reload Data reads it in place again too, not whole.
    opened.run("layer.reloadData", &json!({"layer": "checks"})).unwrap();
    let rl = raster(&opened, "checks");
    assert!(rl.tiled, "reloaded in place");
    assert_eq!(rl.data.as_ref().map(|r| (r.width, r.overview)), Some((512, Some((1024, 1024)))));
}

/// A geoprocessing tool reads the file, not the overview the layer holds: Zonal Statistics over
/// the whole image counts each of its 1024 × 1024 pixels.
#[test]
fn a_tool_reads_a_tiled_file_at_its_full_size() {
    let d = dir("tool");
    let path = checkerboard(&d);
    let _held = HeldWhole::at(1_000_000);
    let mut s = Session::new(Project::default());
    s.run("layer.addData", &json!({"path": path})).unwrap();
    assert!(raster(&s, "checks").tiled);
    let zone = d.join("zone.geojson");
    std::fs::write(
        &zone,
        r#"{"type": "FeatureCollection", "features": [{"type": "Feature", "properties": {"id": 1},
            "geometry": {"type": "Polygon", "coordinates": [[[9.9, 44.9], [11.1, 44.9], [11.1, 46.1], [9.9, 46.1], [9.9, 44.9]]]}}]}"#,
    )
    .unwrap();
    s.run("layer.addData", &json!({"path": zone.display().to_string(), "name": "zone"})).unwrap();
    let r = s.run("gp.run", &json!({"tool": "zonalStats", "params": {"raster": "checks", "polygons": "zone", "output": "stats"}})).unwrap();
    let out = &s.map().layer_by_name("stats").unwrap_or_else(|| panic!("{r}")).dataset;
    let names: Vec<&str> = out.fields.iter().map(|f| f.name.as_str()).collect();
    let count = names.iter().position(|n| n.ends_with("_count")).unwrap_or_else(|| panic!("{names:?}"));
    assert_eq!(out.value(0, count), &rustgis_data::Value::Int(1024 * 1024), "every pixel of the file, not of its overview: {names:?}");
}

/// Held whole as before: a file that fits, and one too large with an `.aux.xml` beside it, which
/// the in-place reader wouldn't apply.
#[test]
fn a_file_that_fits_or_has_a_sidecar_is_held_whole() {
    let d = dir("whole");
    let path = checkerboard(&d);
    let mut s = Session::new(Project::default());
    let mark = s.notice_mark();
    s.run("layer.addData", &json!({"path": path})).unwrap();
    assert!(!raster(&s, "checks").tiled);
    assert!(!said(&s, mark).iter().any(|n| n.contains("read in place")), "{:?}", said(&s, mark));
    std::fs::write(format!("{path}.aux.xml"), "<PAMDataset></PAMDataset>").unwrap();
    let _held = HeldWhole::at(1_000_000);
    let mut s = Session::new(Project::default());
    s.run("layer.addData", &json!({"path": path})).unwrap();
    assert!(!raster(&s, "checks").tiled, "a sidecar keeps it whole");
}

/// How each kind of run reads a raster layer read in place from its file: a built-in tool's run
/// reads the file, a plugin's reads the overview the layer holds and says so, and a suite tool
/// (which stages the file by its path) neither reads nor says anything. A layer held whole is
/// read as it is, with nothing said.
#[test]
fn each_kind_of_run_reads_a_tiled_file_as_it_says() {
    use crate::cmd::gp::{Tiled, tiled_inputs};
    let d = dir("kinds");
    let path = checkerboard(&d);
    let spec = rustgis_analysis::TOOLS.iter().find(|t| t.id == "zonalStats").unwrap();
    let params = json!({"raster": "checks", "polygons": "zone"});
    let read = |s: &Session, tiled: Tiled| {
        let mut messages = Vec::new();
        let read = tiled_inputs(s.map(), Some(spec), &params, tiled, &mut messages, &mut Vec::new()).unwrap();
        (read.get("checks").map(|r| (r.width, r.overview)), messages)
    };
    let mut s = Session::new(Project::default());
    {
        let _held = HeldWhole::at(1_000_000);
        s.run("layer.addData", &json!({"path": path})).unwrap();
    }
    assert_eq!(read(&s, Tiled::File), (Some((1024, None)), Vec::new()), "the full image, as it fits");
    let (none, said) = read(&s, Tiled::Overview);
    assert_eq!(none, None, "the layer's own overview is read");
    assert_eq!(
        said,
        [
            "read as the layer holds it, as plugin tools don't read a raster read in place from its file: checks at the 512 × 512 overview of the 1024 × 1024 raster it holds"
        ]
    );
    assert_eq!(read(&s, Tiled::ByPath), (None, Vec::new()));
    // A suite tool would stage the held overview of a file no longer there: refused.
    let moved = d.join("moved.tif");
    std::fs::rename(&path, &moved).unwrap();
    let e = tiled_inputs(s.map(), Some(spec), &params, Tiled::ByPath, &mut Vec::new(), &mut Vec::new()).unwrap_err();
    assert!(e.starts_with("checks: its file") && e.contains("is no longer there"), "{e}");
    std::fs::rename(&moved, &path).unwrap();
    // Held whole: nothing read again, nothing said.
    let mut whole = Session::new(Project::default());
    whole.run("layer.addData", &json!({"path": path})).unwrap();
    assert_eq!(read(&whole, Tiled::Overview), (None, Vec::new()));
}

/// A batch reads a raster file read in place once for its items (kept only while it runs), and
/// each item reads the file, not the overview the layer holds: Zonal Statistics batched over two
/// zone layers counts every pixel of the file in each.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_batch_over_a_tiled_file_reads_the_file_for_each_item() {
    let d = dir("batch");
    let path = checkerboard(&d);
    let _held = HeldWhole::at(1_000_000);
    let mut s = Session::new(Project::default());
    s.run("layer.addData", &json!({"path": path})).unwrap();
    assert!(raster(&s, "checks").tiled);
    let box_around = r#"{"type": "FeatureCollection", "features": [{"type": "Feature", "properties": {"id": 1},
        "geometry": {"type": "Polygon", "coordinates": [[[9.9, 44.9], [11.1, 44.9], [11.1, 46.1], [9.9, 46.1], [9.9, 44.9]]]}}]}"#;
    for name in ["one", "two"] {
        let zone = d.join(format!("{name}.geojson"));
        std::fs::write(&zone, box_around).unwrap();
        s.run("layer.addData", &json!({"path": zone.display().to_string(), "name": name})).unwrap();
    }
    let reads = || crate::cmd::gp::TILED_READS.with(std::cell::Cell::get);
    let before = reads();
    let p = json!({"tool": "zonalStats", "inputs": ["one", "two"], "param": "polygons", "params": {"raster": "checks"}, "outputPattern": "{input}_stats"});
    let r = s.run("gp.batch", &p).unwrap();
    assert_eq!(reads() - before, 1, "the file read once for both items");
    assert!(!crate::cmd::gp::batch_reads_kept(), "nothing kept once the batch ends");
    // Outside a batch, each run reads the file again: nothing is kept between runs.
    let before = reads();
    for out in ["again_1", "again_2"] {
        s.run("gp.run", &json!({"tool": "zonalStats", "params": {"raster": "checks", "polygons": "one", "output": out}})).unwrap();
    }
    assert_eq!(reads() - before, 2);
    for name in ["one_stats", "two_stats"] {
        let out = &s.map().layer_by_name(name).unwrap_or_else(|| panic!("no {name}: {r}")).dataset;
        let count = out.fields.iter().position(|f| f.name.ends_with("_count")).unwrap();
        assert_eq!(out.value(0, count), &rustgis_data::Value::Int(1024 * 1024), "{name}");
    }
}

/// Original image grids read a tiled file's own grid, as for a file held whole: its overview
/// records the file's revision, which the grid read is checked against, and a file written again
/// since is refused.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn an_original_image_grid_reads_a_tiled_files_grid() {
    let d = dir("grid");
    let path = checkerboard(&d);
    let _held = HeldWhole::at(1_000_000);
    let mut s = Session::new(Project::default());
    s.run("layer.addData", &json!({"path": path})).unwrap();
    assert!(raster(&s, "checks").tiled);
    crate::image_grid::read(raster(&s, "checks")).unwrap();
    // Written again (the same bytes, a new file): the revision the overview came from is gone.
    let bytes = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, bytes).unwrap();
    let e = crate::image_grid::read(raster(&s, "checks")).err().unwrap_or_default();
    assert!(e.contains("changed since its pixels were loaded"), "{e}");
}

/// A command that makes a raster reads the file, not the overview the layer holds, as it runs
/// where the caller waits and as the app runs it on another thread: an index over the file has
/// its 1024 × 1024 pixels, and a resample by 2 of it 512 × 512. Two criteria of a suitability on
/// the same layer read its file once. The full image fits, so nothing is said of an overview.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_command_that_makes_a_raster_reads_a_tiled_file_at_its_full_size() {
    use crate::cmd::workflows::{raster_land, raster_start};
    let d = dir("made");
    let path = checkerboard(&d);
    let _held = HeldWhole::at(1_000_000);
    let mut s = Session::new(Project::default());
    s.run("layer.addData", &json!({"path": path})).unwrap();
    assert!(raster(&s, "checks").tiled);
    let size = |s: &Session, name: &str| raster(s, name).data.as_ref().map(|r| (r.width, r.height, r.overview));
    let mark = s.notice_mark();
    s.run("raster.index", &json!({"layer": "checks", "index": "ndvi", "red": 1, "nir": 2, "name": "index"})).unwrap();
    assert_eq!(size(&s, "index"), Some((1024, 1024, None)), "where the caller waits");
    // As the app runs it: started here, made by the run (on its own thread in the app; on this
    // one here), added when run again.
    let p = json!({"layer": "checks", "operation": "resample", "factor": 2, "name": "halved"});
    let run = raster_start(&mut s, "raster.process", &p).unwrap();
    let id = run.id;
    let made = run.run(&rustgis_analysis::control::Control::new()).unwrap();
    assert!(raster_land(&mut s, id, made).unwrap());
    s.run("raster.process", &json!({"ran": id})).unwrap();
    assert_eq!(size(&s, "halved"), Some((512, 512, None)), "made by the run");
    let reads = || crate::cmd::gp::TILED_READS.with(std::cell::Cell::get);
    let before = reads();
    let criterion = json!({"layer": "checks", "ranges": [[0, 255, 1]]});
    s.run("analysis.suitability", &json!({"criteria": [criterion, criterion], "name": "suits"})).unwrap();
    assert_eq!(reads() - before, 1, "the file read once for the run");
    assert_eq!(size(&s, "suits"), Some((1024, 1024, None)));
    let notes = said(&s, mark);
    assert!(!notes.iter().any(|n| n.contains("finest level") || n.contains("rounded")), "{notes:?}");
}

/// A run whose work panics leaves nothing of its reads behind: on the same thread afterwards, a
/// command's input outside a run is the overview the layer holds, not its file.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_run_that_panics_reads_no_file_after_it() {
    use crate::cmd::workflows::{Input, reading_files};
    let d = dir("panic");
    let path = checkerboard(&d);
    let _held = HeldWhole::at(1_000_000);
    let mut s = Session::new(Project::default());
    s.run("layer.addData", &json!({"path": path})).unwrap();
    let input = Input::of(&s, "checks").unwrap();
    let mut within = 0;
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        reading_files("Indices", || {
            within = input.read().unwrap().width;
            panic!("the work failed");
        })
    }));
    assert!(ran.is_err());
    assert_eq!(within, 1024, "the file, within the run");
    assert_eq!(input.read().unwrap().width, 512, "the overview the layer holds, after the run");
}
