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

/// A 1000-pixel RGB tiled GeoTIFF over 10°–11° E, 45°–46° N, its pixels a thousandth of a degree
/// (which binary fractions don't hold exactly, so a window found from another origin can round
/// another way), with its 500 overview: values vary across, down, and pixel by pixel.
fn thousandths(dir: &std::path::Path) -> String {
    const N: u32 = 1000;
    let at = |f: fn(u32, u32) -> f32| (0..N * N).map(|i| f(i % N, i / N)).collect::<Vec<f32>>();
    let bands = vec![at(|x, y| ((x * 7 + y * 13) % 256) as f32), at(|x, _| (x / 4) as f32), at(|_, y| (y / 4) as f32)];
    let gt = GeoTransform { origin_x: 10.0, origin_y: 46.0, pixel_width: 0.001, pixel_height: -0.001 };
    let r = Raster::new("checks", N, N, bands, gt, Crs::LonLat, None, ColorHint::Rgb { alpha: false }, "UInt8").unwrap();
    let path = dir.join("checks.tif");
    let info = rustgis_data::raster::cogwrite::write_file(&r, 0, &path).unwrap();
    assert_eq!(info.levels, vec![(N, N), (N / 2, N / 2)], "the fixture's levels");
    path.display().to_string()
}

/// Clip by extent of a layer read in place reads only the part of its file under the extent
/// (#2865): the clip is the one made of the file held whole, to the pixel, and the file isn't
/// read whole for it. A file replaced since the layer read it is read whole, so the clip is of the
/// file as it is.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_clip_by_extent_reads_only_its_part_of_a_tiled_file() {
    let d = dir("clip");
    let path = thousandths(&d);
    let clip = |extent: [f64; 4]| json!({"layer": "checks", "operation": "clip", "extent": extent, "name": "clip"});
    // The same clip of the file held whole, as a file this size is.
    let whole = |extent: [f64; 4]| {
        // Held whole, whatever limit this thread has set meanwhile.
        let limit = HELD_WHOLE.with(|c| c.replace(None));
        let mut whole = Session::new(Project::default());
        whole.run("layer.addData", &json!({"path": path})).unwrap();
        assert!(!raster(&whole, "checks").tiled);
        whole.run("raster.process", &clip(extent)).unwrap();
        HELD_WHOLE.with(|c| c.set(limit));
        raster(&whole, "clip").data.clone().unwrap()
    };
    let _held = HeldWhole::at(1_000_000);
    let mut s = Session::new(Project::default());
    s.run("layer.addData", &json!({"path": path})).unwrap();
    assert!(raster(&s, "checks").tiled);
    let reads = || crate::cmd::gp::TILED_READS.with(std::cell::Cell::get);
    let extents =
        [[10.3, 45.55, 10.42, 45.9], [10.123, 45.456, 10.789, 45.9], [10.007, 45.003, 10.509, 45.611], [10.25, 45.5, 10.5, 45.75]];
    for extent in extents {
        let (before, mark) = (reads(), s.notice_mark());
        s.run("layer.remove", &json!({"layer": "clip"})).ok();
        s.run("raster.process", &clip(extent)).unwrap();
        assert_eq!(reads(), before, "the file isn't read whole");
        let (got, want) = (raster(&s, "clip").data.clone().unwrap(), whole(extent));
        assert_eq!(got.overview, None, "{extent:?}: at the file's full size");
        assert_eq!((got.width, got.height, got.transform), (want.width, want.height, want.transform), "{extent:?}");
        for (g, w) in got.bands.iter().zip(&want.bands) {
            assert!(g.data == w.data, "{extent:?}: band {}", g.name);
        }
        let notes = said(&s, mark);
        assert!(!notes.iter().any(|n| n.contains("finest level")), "{notes:?}");
    }
    // A part too large for what the run holds is read from an overview, and said, with its size.
    crate::cmd::gp::PART_SAMPLES.with(|c| c.set(Some(40_000)));
    let mark = s.notice_mark();
    s.run("layer.remove", &json!({"layer": "clip"})).ok();
    s.run("raster.process", &clip([10.3, 45.55, 10.42, 45.9])).unwrap();
    crate::cmd::gp::PART_SAMPLES.with(|c| c.set(None));
    let got = raster(&s, "clip").data.clone().unwrap();
    assert_eq!((got.width, got.height), (60, 176), "the part at the 500 overview");
    let notes = said(&s, mark);
    let note = "Process: read at the finest level each can hold: checks at its 500 × 500 overview of the 1000 × 1000 raster, a 62 × 177 part of it";
    assert!(notes.iter().any(|n| n == note), "{notes:?}");
    // The file replaced by one a twentieth of a degree east: read whole, and clipped where it is.
    const N: u32 = 1000;
    let shifted = Raster::new(
        "checks",
        N,
        N,
        vec![(0..N * N).map(|i| (i % 251) as f32).collect()],
        GeoTransform { origin_x: 10.05, origin_y: 46.0, pixel_width: 0.001, pixel_height: -0.001 },
        Crs::LonLat,
        None,
        ColorHint::Gray,
        "UInt8",
    )
    .unwrap();
    rustgis_data::raster::cogwrite::write_file(&shifted, 0, std::path::Path::new(&path)).unwrap();
    let extent = [10.3, 45.55, 10.42, 45.9];
    s.run("layer.remove", &json!({"layer": "clip"})).ok();
    let before = reads();
    s.run("raster.process", &clip(extent)).unwrap();
    assert_eq!(reads(), before + 1, "a replaced file is read whole");
    let (got, want) = (raster(&s, "clip").data.clone().unwrap(), whole(extent));
    assert_eq!((got.width, got.height, got.transform), (want.width, want.height, want.transform));
    assert!(got.bands[0].data == want.bands[0].data, "the clip is of the file as it is now");
}

/// Holds files whole for a cell-by-cell command up to `max` samples on this thread while it
/// lives ([`crate::cmd::in_blocks::BLOCKS_PAST`]), then as before.
struct BlocksPast;

impl BlocksPast {
    fn at(max: usize) -> BlocksPast {
        crate::cmd::in_blocks::BLOCKS_PAST.with(|c| c.set(Some(max)));
        BlocksPast
    }
}

impl Drop for BlocksPast {
    fn drop(&mut self) {
        crate::cmd::in_blocks::BLOCKS_PAST.with(|c| c.set(None));
    }
}

/// Indices over a layer read in place whose file is too large to read whole at its full size run
/// a block at a time (#2865): the result is the index of the file held whole, pixel for pixel, at
/// its full size. One too large to hold is written to a tiled GeoTIFF in the project's results
/// folder, which the new layer reads in place and another command can read at once, and said; the
/// Assistant asks before such a run, as it writes a file. A run cancelled at any of its checks, or
/// never added, leaves nothing behind, the folder included, and one run again makes it. A result
/// that fits is held, with no file. A script step that can't be stopped is refused.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn indices_over_a_file_too_large_to_read_whole_run_a_block_at_a_time() {
    use crate::cmd::workflows::{raster_abandon, raster_land, raster_start};
    use crate::policy::{Call, Effects, Outcome, Revocation, Scope};
    struct Live;
    impl Revocation for Live {
        fn allows(&self, _: Effects) -> bool {
            true
        }
    }
    let d = dir("blocks");
    let path = thousandths(&d);
    let ndvi = json!({"layer": "checks", "index": "ndvi", "red": 1, "nir": 2, "name": "index"});
    // The index of the file held whole.
    let want = {
        let mut whole = Session::new(Project::default());
        whole.run("layer.addData", &json!({"path": path})).unwrap();
        whole.run("raster.index", &ndvi).unwrap();
        raster(&whole, "index").data.clone().unwrap()
    };
    let same = |got: &Raster, why: &str| {
        assert_eq!((got.width, got.height, got.transform, got.nodata), (want.width, want.height, want.transform, want.nodata), "{why}");
        assert!(got.bands[0].data == want.bands[0].data, "{why}: the index of the file held whole");
    };
    let _held = HeldWhole::at(1_000_000);
    let mut s = Session::new(Project::default());
    s.path = Some(d.join("Survey.rgis"));
    s.run("layer.addData", &json!({"path": path})).unwrap();
    let folder = d.join("Survey.rgis.data");
    {
        // Read in blocks, but a result of a million pixels fits: held, with no file.
        let _blocks = BlocksPast::at(1_000_000);
        let mark = s.notice_mark();
        s.run("raster.index", &ndvi).unwrap();
        let rl = raster(&s, "index");
        assert!(!rl.tiled && rl.path.is_none(), "held, with no file");
        same(rl.data.as_ref().unwrap(), "held");
        assert!(!folder.exists(), "no folder for a result held");
        assert!(!said(&s, mark).iter().any(|n| n.contains("written to")));
        s.run("layer.remove", &json!({"layer": "index"})).unwrap();
    }
    let _blocks = BlocksPast::at(900_000);
    // The Assistant asks before a run that writes a file.
    let scope = Scope::assistant(7, std::sync::Arc::new(Live));
    assert!(matches!(s.review(&scope, &Call::command("raster.index", ndvi.clone())), Outcome::NeedsApproval(_)));
    // A script step on the app's window thread is refused, with what to do.
    let script = format!("raster.index {ndvi}");
    s.on_window_thread = true;
    let e = s.run("script.run", &json!({"script": script})).unwrap_err();
    s.on_window_thread = false;
    assert!(e.contains("a block at a time") && e.contains("run it on its own"), "{e}");
    let run = |s: &mut Session, p: &serde_json::Value, control: &std::sync::Arc<rustgis_analysis::control::Control>| {
        let run = raster_start(s, "raster.index", p).unwrap();
        let id = run.id;
        (id, run.run(control))
    };
    let count = rustgis_analysis::control::Control::cancelled_at_look(u64::MAX);
    let (id, made) = run(&mut s, &ndvi, &count);
    let looks = count.looked();
    assert!(looks > 6, "{looks} checks: it looks at Cancel between its blocks");
    // Made but never added: its file and the folder made for it go with it.
    drop(made);
    raster_abandon(&mut s, id);
    assert!(!folder.exists(), "a result never added left {folder:?}");
    // Cancelled at its checks (a spread of them, and its last ones): nothing left. Its threads
    // share the count, so a run can end with fewer checks than the count's: one that never got to
    // check `n` makes its result, which goes when it isn't added.
    let checks: std::collections::BTreeSet<u64> =
        (1..=looks).step_by((looks as usize / 64).max(1)).chain(looks.saturating_sub(10)..=looks).collect();
    for n in checks {
        let control = rustgis_analysis::control::Control::cancelled_at_look(n);
        let (id, made) = run(&mut s, &ndvi, &control);
        match made {
            Err(e) => assert!(e.starts_with("Indices was cancelled"), "check {n}: {e}"),
            Ok(m) => {
                assert!(control.looked() < n, "check {n} of {}: not cancelled", control.looked());
                drop(m);
            }
        }
        raster_abandon(&mut s, id);
        assert!(!folder.exists(), "check {n}: left {folder:?}");
    }
    // Run to the end, and added as the app adds it.
    let mark = s.notice_mark();
    let (id, made) = run(&mut s, &ndvi, &rustgis_analysis::control::Control::new());
    assert!(raster_land(&mut s, id, made.unwrap()).unwrap());
    let v = s.run("raster.index", &json!({"ran": id})).unwrap();
    let file = folder.join("index.tif");
    assert_eq!(v["path"], json!(file.display().to_string()));
    let rl = raster(&s, "index");
    assert!(rl.tiled, "the result is read in place from its file");
    assert_eq!(rl.path.as_deref(), Some(file.display().to_string().as_str()));
    // It holds the file's overview the reader picks, as when the project is opened again.
    assert_eq!(rl.data.as_ref().map(|r| (r.width, r.overview)), Some((500, Some((1000, 1000)))));
    same(&rustgis_data::raster::read_path(&file).unwrap(), "written");
    let notes = said(&s, mark);
    assert!(
        notes.iter().any(|n| n.contains("a block at a time") && n.contains("1000 × 1000") && n.contains("Survey.rgis.data")),
        "{notes:?}"
    );
    // The result read in place feeds another run at once, a block at a time too.
    let again = json!({"layer": "index", "index": "ndvi", "red": 1, "nir": 1, "name": "again"});
    let (id, made) = run(&mut s, &again, &rustgis_analysis::control::Control::new());
    assert!(raster_land(&mut s, id, made.unwrap()).unwrap());
    s.run("raster.index", &json!({"ran": id})).unwrap();
    assert!(folder.join("again.tif").is_file());
    // Made again, it takes a name of its own.
    let (id, made) = run(&mut s, &ndvi, &rustgis_analysis::control::Control::new());
    assert!(raster_land(&mut s, id, made.unwrap()).unwrap());
    s.run("raster.index", &json!({"ran": id})).unwrap();
    assert!(folder.join("index 2.tif").is_file());
    // A script step off any window (the command line's, Python's) runs it.
    let scripted = json!({"layer": "checks", "index": "ndvi", "red": 1, "nir": 2, "name": "scripted"});
    s.run("script.run", &json!({"script": format!("raster.index {scripted}")})).unwrap();
    assert!(folder.join("scripted.tif").is_file());
}

/// The other commands that work cell by cell, or over a cell's neighbours, run a block at a time
/// over a file too large to read whole too (#2865), each making the raster it makes of the file
/// held whole, held or written to the results folder: Raster Functions by cell and over a cell's
/// neighbours (the cells around each block read with it; a hillshade held and written as bytes;
/// a remap's or threshold's overview of its classes), the Function Editor, and Process's
/// resample, which is made a block at a time when it can be held. What a remap meets is said as
/// of the whole raster. A chain that remaps after a step over neighbours reads the file as before,
/// and asks for no files.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn raster_functions_and_resamples_run_a_block_at_a_time() {
    let d = dir("functions");
    let path = thousandths(&d);
    // Training polygons: two of one class, a triangle of another straddling the rows and columns
    // of blocks, its edges across the grid's cells, and a third class's.
    let training = r#"{"type":"FeatureCollection","features":[
        {"type":"Feature","properties":{"class":"low"},"geometry":{"type":"Polygon","coordinates":[[[10.1,45.1],[10.3,45.1],[10.3,45.3],[10.1,45.3],[10.1,45.1]]]}},
        {"type":"Feature","properties":{"class":"high"},"geometry":{"type":"Polygon","coordinates":[[[10.35,45.2],[10.9,45.6],[10.6,45.85],[10.35,45.2]]]}},
        {"type":"Feature","properties":{"class":"low"},"geometry":{"type":"Polygon","coordinates":[[[10.5,45.2],[10.55,45.2],[10.55,45.25],[10.5,45.25],[10.5,45.2]]]}},
        {"type":"Feature","properties":{"class":"mid"},"geometry":{"type":"Polygon","coordinates":[[[10.7,45.05],[10.95,45.05],[10.95,45.15],[10.7,45.15],[10.7,45.05]]]}}]}"#;
    let open = |s: &mut Session| {
        s.run("layer.addData", &json!({"path": path})).unwrap();
        s.run("layer.addDataText", &json!({"fileName": "train.geojson", "text": training})).unwrap();
    };
    let cases = [
        ("raster.function", json!({"layer": "checks", "function": "slope", "band": 1, "name": "out"})),
        ("raster.function", json!({"layer": "checks", "function": "hillshade", "band": 2, "name": "out"})),
        (
            "raster.function",
            json!({"layer": "checks", "function": "remap", "band": 1, "ranges": [[0, 100, 1], [100, 300, 2], [7, 7, 3]], "name": "out"}),
        ),
        (
            "raster.function",
            json!({"layer": "checks", "function": "remap", "band": 1, "ranges": [[0, 100, 1], [7, 7, 3]], "keep": true, "name": "out"}),
        ),
        ("raster.function", json!({"layer": "checks", "function": "threshold", "band": 3, "value": 128, "name": "out"})),
        ("raster.function", json!({"layer": "checks", "function": "bandMath", "expression": "b1 * 2 - b3", "name": "out"})),
        (
            "raster.functionChain",
            json!({"layer": "checks", "steps": [{"function": "bandMath", "expression": "b1 + b2"}, {"function": "aspect"}, {"function": "slope"}], "name": "out"}),
        ),
        ("raster.process", json!({"layer": "checks", "operation": "resample", "factor": 3, "name": "out"})),
        (
            "raster.classifySupervised",
            json!({"layer": "checks", "training": "train", "classField": "class", "bands": [1, 2, 3], "name": "out"}),
        ),
    ];
    let reads = || crate::cmd::gp::TILED_READS.with(std::cell::Cell::get);
    for (command, p) in &cases {
        // Of the file held whole.
        let mut whole = Session::new(Project::default());
        open(&mut whole);
        let mark = whole.notice_mark();
        let answer = whole.run(command, p).unwrap();
        let want = raster(&whole, "out").data.clone().unwrap();
        let said_whole: Vec<String> = said(&whole, mark).into_iter().filter(|n| n.contains("matches no")).collect();
        assert_eq!(said_whole.is_empty(), p["function"] != "remap", "{command}: a range of the remap is empty, and said");
        // Held, then (one band past the limit, or for a resample its three bands) written, a block
        // at a time.
        let resample = *command == "raster.process";
        for (past, written) in [(1_000_000, false), (if resample { 300_000 } else { 900_000 }, true)] {
            let why = format!("{command} {p}, written {written}");
            let (_held, _blocks) = (HeldWhole::at(1_000_000), BlocksPast::at(past));
            let mut s = Session::new(Project::default());
            s.path = Some(d.join("Survey.rgis"));
            open(&mut s);
            assert!(raster(&s, "checks").tiled);
            let (before, mark) = (reads(), s.notice_mark());
            // The Assistant asks before a run that writes a file, and only then.
            let scope = crate::policy::Scope::assistant(8, std::sync::Arc::new(Live));
            match s.review(&scope, &crate::policy::Call::command(command, p.clone())) {
                crate::policy::Outcome::NeedsApproval(_) => assert!(written, "{why}: the Assistant asks only before a file is written"),
                crate::policy::Outcome::Allowed => assert!(!written, "{why}: the Assistant asks before a file is written"),
                crate::policy::Outcome::Refused(e) => panic!("{why}: refused: {e}"),
            }
            let got = s.run(command, p).unwrap();
            assert_eq!(reads(), before, "{why}: the file isn't read whole");
            let rl = raster(&s, "out");
            assert_eq!(rl.tiled, written, "{why}");
            let got_raster = if written {
                rustgis_data::raster::read_path(std::path::Path::new(rl.path.as_deref().unwrap())).unwrap()
            } else {
                rl.data.as_deref().unwrap().clone()
            };
            // A resample's file holds its means as floats.
            let kind = if resample && written { "Float32" } else { want.data_type.as_str() };
            assert_eq!(
                (
                    got_raster.width,
                    got_raster.height,
                    got_raster.transform,
                    got_raster.nodata,
                    got_raster.data_type.as_str(),
                    &got_raster.color
                ),
                (want.width, want.height, want.transform, want.nodata, kind, &want.color),
                "{why}"
            );
            assert_eq!(got_raster.bands.len(), want.bands.len(), "{why}: its bands");
            // A written resample of bytes is drawn on their 0 to 255, as the held one is, and of an
            // image without alpha says nothing of a no-data value.
            if resample && written {
                assert!(!said(&s, mark).iter().any(|n| n.contains("isn't written to its file")), "{why}");
                assert_eq!(raster(&s, "out").style.stretch, rustgis_map::Stretch::Custom { min: 0.0, max: 255.0, bands: None }, "{why}");
            }
            for (g, w) in got_raster.bands.iter().zip(&want.bands) {
                assert!(g.data == w.data, "{why}: band {} is the whole raster's", w.name);
                assert_eq!(g.name, w.name, "{why}");
            }
            // What a remap met is said as of the whole raster.
            let said_blocks: Vec<String> = said(&s, mark).into_iter().filter(|n| n.contains("matches no")).collect();
            assert_eq!(said_blocks, said_whole, "{why}");
            // The same classes and legend, drawn by the classes some pixel takes.
            if *command == "raster.classifySupervised" {
                assert_eq!(got["classes"], answer["classes"], "{why}");
                let drawn = |s: &Session| format!("{:?}", raster(s, "out").style.renderer);
                assert_eq!(drawn(&s), drawn(&whole), "{why}");
                assert!(answer["classes"].as_array().is_some_and(|c| c.len() == 3), "{why}: three classes");
            }
            // A remap's, threshold's or classes' overview held is of its classes, never an average
            // of two; one of a remap keeping the values it doesn't match averages them, as measures.
            if written && (matches!(p["function"].as_str(), Some("remap" | "threshold")) || *command == "raster.classifySupervised") {
                let classes: std::collections::BTreeSet<u32> = want.bands[0].data.iter().map(|v| v.to_bits()).collect();
                let held = rl.data.as_deref().unwrap();
                assert!(held.overview.is_some(), "{why}: an overview held");
                let only = held.bands[0].data.iter().all(|v| classes.contains(&v.to_bits()));
                assert_eq!(only, p["keep"] != true, "{why}: the overview holds only its values");
            }
            if p["function"] == "hillshade" {
                assert_eq!(rl.style.renderer, raster(&whole, "out").style.renderer, "{why}: drawn as a hillshade");
            }
            // Cancelled at its checks (a spread of them), it leaves nothing behind.
            let files = || std::fs::read_dir(d.join("Survey.rgis.data")).map(|r| r.count()).unwrap_or(0);
            let (before, count) = (files(), rustgis_analysis::control::Control::cancelled_at_look(u64::MAX));
            let run = crate::cmd::workflows::raster_start(&mut s, command, p).unwrap();
            let id = run.id;
            drop(run.run(&count));
            crate::cmd::workflows::raster_abandon(&mut s, id);
            assert!(count.looked() > 6, "{why}: it looks at Cancel between its blocks");
            for n in (1..=count.looked()).step_by((count.looked() as usize / 8).max(1)) {
                let control = rustgis_analysis::control::Control::cancelled_at_look(n);
                let run = crate::cmd::workflows::raster_start(&mut s, command, p).unwrap();
                let id = run.id;
                match run.run(&control) {
                    Err(e) => assert!(e.contains("was cancelled"), "{why}, check {n}: {e}"),
                    Ok(_) => assert!(control.looked() < n, "{why}, check {n}: not cancelled"),
                }
                crate::cmd::workflows::raster_abandon(&mut s, id);
                assert_eq!(files(), before, "{why}, check {n}: left a file");
            }
        }
    }
    // A resample this computer can't give the memory for now reads the file as before.
    {
        let (_held, _blocks) = (HeldWhole::at(1_000_000), BlocksPast::at(1_000_000));
        crate::cmd::in_blocks::AFFORDS.with(|c| c.set(Some(1 << 20)));
        let mut s = Session::new(Project::default());
        open(&mut s);
        let before = reads();
        let done = s.run("raster.process", &json!({"layer": "checks", "operation": "resample", "factor": 3, "name": "out"}));
        crate::cmd::in_blocks::AFFORDS.with(|c| c.set(None));
        done.unwrap();
        assert_eq!(reads(), before + 1, "read as before");
        assert!(!raster(&s, "out").tiled, "held, from the file read as before");
    }
    // A chain that remaps after a step over neighbours reads the file as before, writing nothing,
    // and the Assistant isn't asked for files for it.
    struct Live;
    impl crate::policy::Revocation for Live {
        fn allows(&self, _: crate::policy::Effects) -> bool {
            true
        }
    }
    let (_held, _blocks) = (HeldWhole::at(1_000_000), BlocksPast::at(900_000));
    let mut s = Session::new(Project::default());
    s.path = Some(d.join("Fallback.rgis"));
    open(&mut s);
    let p = json!({"layer": "checks", "steps": [{"function": "slope"}, {"function": "threshold", "value": 5}], "name": "out"});
    let scope = crate::policy::Scope::assistant(8, std::sync::Arc::new(Live));
    assert!(matches!(s.review(&scope, &crate::policy::Call::command("raster.functionChain", p.clone())), crate::policy::Outcome::Allowed));
    let before = reads();
    s.run("raster.functionChain", &p).unwrap();
    assert_eq!(reads(), before + 1, "read as before");
    assert!(!d.join("Fallback.rgis.data").exists());
}

/// A resample's pixel covering no pixel with a value takes the result's no-data value, the
/// file's, and one covering some averages those alone, whether the file is held whole or read a
/// block at a time.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_resampled_pixel_with_no_value_takes_the_no_data_value() {
    let d = dir("resample-nodata");
    const N: u32 = 600;
    let mut band: Vec<f32> = (0..N * N).map(|i| (i % 50) as f32).collect();
    for (x, y, v) in
        [(0, 0, -9999.0), (1, 0, -9999.0), (0, 1, -9999.0), (1, 1, -9999.0), (2, 0, 5.0), (3, 0, -9999.0), (2, 1, -9999.0), (3, 1, 7.0)]
    {
        band[(y * N + x) as usize] = v;
    }
    let gt = GeoTransform { origin_x: 10.0, origin_y: 46.0, pixel_width: 0.001, pixel_height: -0.001 };
    let r = Raster::new("gaps", N, N, vec![band], gt, Crs::LonLat, Some(-9999.0), ColorHint::Gray, "Float32").unwrap();
    let path = d.join("gaps.tif");
    rustgis_data::raster::cogwrite::write_file(&r, 0, &path).unwrap();
    let p = json!({"layer": "gaps", "operation": "resample", "factor": 2, "name": "out"});
    for in_blocks in [false, true] {
        let (_held, _blocks) = (HeldWhole::at(if in_blocks { 1000 } else { 1_000_000 }), BlocksPast::at(100_000));
        let mut s = Session::new(Project::default());
        s.run("layer.addData", &json!({"path": path.display().to_string()})).unwrap();
        assert_eq!(raster(&s, "gaps").tiled, in_blocks);
        let mark = s.notice_mark();
        s.run("raster.process", &p).unwrap();
        let out = raster(&s, "out").data.clone().unwrap();
        assert_eq!((out.width, out.nodata), (N / 2, Some(-9999.0)), "in blocks {in_blocks}");
        assert_eq!(out.bands[0].data[..2], [-9999.0, 6.0], "in blocks {in_blocks}");
        assert!(!said(&s, mark).iter().any(|n| n.contains("colour table")), "in blocks {in_blocks}: no palette, nothing said");
    }
}

/// A palette raster's resample says its colour table isn't kept (its indexes are averaged as grey
/// values), whether the file is held whole or read a block at a time.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_resampled_palette_says_its_colours_go() {
    let d = dir("resample-palette");
    const N: u32 = 600;
    let band: Vec<f32> = (0..N * N).map(|i| (i % 4) as f32).collect();
    let gt = GeoTransform { origin_x: 10.0, origin_y: 46.0, pixel_width: 0.001, pixel_height: -0.001 };
    let colours = (0..256u32).map(|i| [i as u8, 0, 255 - i as u8, 255]).collect();
    let r = Raster::new("classes", N, N, vec![band], gt, Crs::LonLat, None, ColorHint::Palette(colours), "UInt8").unwrap();
    let path = d.join("classes.tif");
    rustgis_data::raster::cogwrite::write_file(&r, 0, &path).unwrap();
    let p = json!({"layer": "classes", "operation": "resample", "factor": 2, "name": "out"});
    for in_blocks in [false, true] {
        let (_held, _blocks) = (HeldWhole::at(if in_blocks { 1000 } else { 1_000_000 }), BlocksPast::at(100_000));
        let mut s = Session::new(Project::default());
        s.run("layer.addData", &json!({"path": path.display().to_string()})).unwrap();
        assert_eq!(raster(&s, "classes").tiled, in_blocks);
        let mark = s.notice_mark();
        s.run("raster.process", &p).unwrap();
        let notes: Vec<String> = said(&s, mark).into_iter().filter(|n| n.contains("colour table isn't kept")).collect();
        assert_eq!(notes.len(), 1, "in blocks {in_blocks}: {notes:?}");
    }
}
