//! Original GeoTIFF construction grids, read independently of display overviews.
use std::sync::Arc;

use crate::{CmdResult, Session, box_edit::Frame};
use rustgis_data::progress;
use rustgis_map::raster::RasterLayer;
use serde_json::Value;

/// Session-local identity of an image source. Style changes do not invalidate a grid; replacing
/// its pixels, file reference, embedded bytes, layer or map does. No file reads occur here.
#[derive(Clone)]
pub struct Source {
    map: u64,
    layer: u32,
    instance: u64,
    replacement: u64,
    raster: RasterLayer,
}
fn same_arc<T>(a: &Option<Arc<T>>, b: &Option<Arc<T>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}
impl Source {
    pub fn capture(s: &Session, layer: u32) -> Result<Self, String> {
        Self::from_layer(s.map().instance_id(), s.map().layer(layer).ok_or("no image layer")?)
    }
    fn from_layer(map: u64, layer: &rustgis_map::Layer) -> Result<Self, String> {
        let raster = layer.raster.clone().ok_or("choose a GeoTIFF image layer")?;
        Ok(Self { map, layer: layer.id, instance: layer.instance_id(), replacement: layer.replacement_revision(), raster })
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn key(&self) -> (u64, u32, u64, u64) {
        (self.map, self.layer, self.instance, self.replacement)
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn same(&self, other: &Self) -> bool {
        self.key() == other.key()
            && self.raster.path == other.raster.path
            && same_arc(&self.raster.embedded, &other.raster.embedded)
            && same_arc(&self.raster.data, &other.raster.data)
    }
    pub fn current(&self, s: &Session) -> bool {
        s.map().instance_id() == self.map
            && s.map().layer(self.layer).is_some_and(|l| {
                l.instance_id() == self.instance
                    && l.replacement_revision() == self.replacement
                    && l.raster.as_ref().is_some_and(|r| {
                        r.path == self.raster.path && same_arc(&r.embedded, &self.raster.embedded) && same_arc(&r.data, &self.raster.data)
                    })
            })
    }
}

/// Cancel is checked before/after the bounded header parse; a filesystem read/seek itself
/// cannot be interrupted until the OS returns. No raster pixel decoding takes place.
/// This counts the header phase; conversion fitting starts its own progress phase afterwards.
pub(crate) fn read(r: &RasterLayer) -> Result<Frame, String> {
    // A scan (or a picture laid over the view) is drawn where it was placed, not where its
    // header puts it: the header's grid would put new shapes elsewhere than the image.
    let placed = || -> Result<(), String> {
        if r.placement.is_some() || r.data.as_ref().is_some_and(|d| !d.georeferenced) {
            return Err("This image is drawn where it was placed, not on its file's own grid. Add the GeoTIFF as a georeferenced image (not as a scan) to draw in its original pixels, or choose map metres.".into());
        }
        Ok(())
    };
    progress::begin(2);
    progress::step(1).map_err(|e| e.to_string())?;
    let grid = if let Some(bytes) = &r.embedded {
        placed()?;
        rustgis_data::raster::geotiff::read_image_grid(std::io::Cursor::new(bytes.as_slice()))
    } else {
        let path = r
            .path
            .as_deref()
            .filter(|p| rustgis_map::paths::is_local(p))
            .ok_or("choose a local or embedded GeoTIFF; remote original grids must be downloaded and added first")?;
        #[cfg(not(target_arch = "wasm32"))]
        {
            let tiff = std::path::Path::new(path)
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("tif") || e.eq_ignore_ascii_case("tiff"));
            if !tiff {
                return Err("Original image grids require a GeoTIFF source. Export this image as a GeoTIFF and add that file.".into());
            }
            placed()?;
            if r.in_place {
                return Err("Original image grids need the image's pixels loaded whole, and this image is read in place a view at a time. Choose map metres, or add a clipped copy that opens whole.".into());
            }
            let input = rustgis_data::provenance::open(path).map_err(|e| e.to_string())?;
            let displayed = r.data.as_ref().ok_or("The image pixels are not loaded. Use Reload Data, then retry the operation.")?;
            let path = std::path::Path::new(path);
            displayed.check_original_handle(&input)?;
            let grid = rustgis_data::raster::geotiff::read_image_grid(&input);
            // Both the handle and path must still name the revision that supplied the display.
            // A replacement during parsing must not pair new coordinates with old pixels.
            displayed.check_original_handle(&input)?;
            displayed.check_original_file(path)?;
            grid
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = path;
            return Err("upload the original GeoTIFF to use its grid in the browser".into());
        }
    }
    .map_err(|e| e.to_string())?;
    progress::step(1).map_err(|e| e.to_string())?;
    Frame::from_grid(&grid)
}
pub(crate) struct Prepared {
    source: Source,
    result: Result<Frame, String>,
}
/// Script workers record read-only grids; landing consumes them without repeating file I/O.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) enum Ahead {
    Recording(Vec<Prepared>),
    Taking(Replay),
}
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct Replay {
    grids: std::collections::VecDeque<Prepared>,
    // Only exact outputs of accepted tool results may bridge worker/live layer instances.
    outputs: std::collections::HashMap<(u64, u32, u64, u64), (Source, Source)>,
    invalidated: bool,
}
#[cfg(not(target_arch = "wasm32"))]
impl Ahead {
    pub(crate) fn taking(grids: Vec<Prepared>) -> Self {
        Self::Taking(Replay { grids: grids.into(), outputs: Default::default(), invalidated: false })
    }
}
/// A rejected tool invalidates all dependent queries, including queries of existing images.
/// Keep an empty, invalid replay rather than letting a later query read on the UI thread.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn invalidate_script_grids(s: &mut Session) {
    if let Some(Ahead::Taking(replay)) = &mut s.grids_ahead {
        replay.invalidated |= !replay.grids.is_empty();
        replay.grids.clear();
        replay.outputs.clear();
    }
}
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn script_grids_invalidated(s: &Session) -> bool {
    matches!(&s.grids_ahead, Some(Ahead::Taking(replay)) if replay.invalidated)
}
/// Called only after an admitted, snapshot-validated tool result has been applied successfully.
/// Its newly created raster layers keep live IDs/style, but share the accepted worker pixels,
/// embedded bytes and placeholder rows. Following tool snapshots can then consume that data too.
/// Bind queries by output identity, never by a name or a layer ID alone; no headers are reread.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn replay_tool_outputs(s: &mut Session, recorded: &[rustgis_map::Layer], landed: &[rustgis_map::Layer]) -> Result<(), String> {
    if !matches!(&s.grids_ahead, Some(Ahead::Taking(replay)) if !replay.grids.is_empty()) {
        return Ok(());
    }
    if recorded.len() != landed.len()
        || recorded.iter().zip(landed).any(|(old, new)| {
            old.name != new.name
                || match (&old.raster, &new.raster) {
                    (None, None) => false,
                    (Some(a), Some(b)) => a.path != b.path || a.file_name != b.file_name || a.placement != b.placement,
                    _ => true,
                }
        })
    {
        invalidate_script_grids(s);
        return Err("the script's tool outputs changed; run the script again".into());
    }
    let map = s.map().instance_id();
    for (old, new) in recorded.iter().zip(landed).filter(|(old, _)| old.raster.is_some()) {
        let from = Source::from_layer(map, old)?;
        let layer = s.map_mut().layer_mut(new.id).ok_or("the script's output layer disappeared")?;
        let raster = layer.raster.as_mut().ok_or("the script's raster output disappeared")?;
        raster.data.clone_from(&from.raster.data);
        raster.embedded.clone_from(&from.raster.embedded);
        layer.dataset.features.clone_from(&old.dataset.features);
        let to = Source::capture(s, new.id)?;
        if let Some(Ahead::Taking(replay)) = &mut s.grids_ahead {
            replay.outputs.insert(from.key(), (from, to));
        }
    }
    Ok(())
}
pub(crate) fn ahead(s: &Session, p: &Value) -> Option<crate::cmd::reads::ReadWork> {
    let source = Source::capture(s, s.layer_id(p).ok()?).ok()?;
    Some(crate::cmd::reads::ahead("Read Original Image Grid", "imageGrid".into(), move || {
        let result = read(&source.raster);
        Prepared { source, result }
    }))
}
/// Desktop UI calls require the cancellable read queue. Headless calls read synchronously on
/// the caller's thread; the browser reads uploaded bytes inline. Reads only headers, never pixels.
pub(crate) fn query(s: &mut Session, p: &Value) -> CmdResult {
    let source = Source::capture(s, s.layer_id(p)?)?;
    let ready = crate::cmd::reads::take::<Prepared>(s, "imageGrid");
    #[cfg(not(target_arch = "wasm32"))]
    let ready = if let Some(Ahead::Taking(replay)) = &mut s.grids_ahead {
        if replay.invalidated {
            return Err("the script inputs changed; run the script again".into());
        }
        let mut prepared = replay.grids.pop_front().ok_or("the script's image grid was not prepared; run the query separately")?;
        if let Some((from, to)) = replay.outputs.get(&prepared.source.key()) {
            if !prepared.source.same(from) {
                return Err("the script's image source changed; run the script again".into());
            }
            prepared.source = to.clone();
        }
        Some(prepared)
    } else {
        ready
    };
    #[cfg(not(target_arch = "wasm32"))]
    if ready.is_none() && s.statistics_window_thread == Some(std::thread::current().id()) {
        return Err("original image grids need the desktop background read queue".into());
    }
    let frame = match ready {
        Some(done) if done.source.current(s) && done.source.layer == source.layer => done.result?,
        Some(_) => return Err("image source changed while reading its grid; select the image again".into()),
        None => {
            let result = read(&source.raster);
            #[cfg(not(target_arch = "wasm32"))]
            if let Some(Ahead::Recording(grids)) = &mut s.grids_ahead {
                grids.push(Prepared { source, result: result.clone() });
            }
            result?
        }
    };
    serde_json::to_value(frame).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster, geotiff};
    use serde_json::json;
    fn session() -> (Session, u32, Frame) {
        let raster = Raster::new(
            "original",
            4,
            6,
            vec![vec![1.; 24]],
            GeoTransform { origin_x: 100., origin_y: 500., pixel_width: 2., pixel_height: -3. },
            Crs::from_epsg(3857).unwrap(),
            None,
            ColorHint::Gray,
            "UInt8",
        )
        .unwrap();
        let bytes = geotiff::write(&raster, 0).unwrap();
        let frame = Frame::from_grid(&geotiff::read_image_grid(std::io::Cursor::new(&bytes)).unwrap()).unwrap();
        let overview = Raster::new("overview", 1, 1, vec![vec![1.]], raster.transform, raster.crs, None, ColorHint::Gray, "UInt8").unwrap();
        let mut s = Session::new(crate::Project::default());
        let id = s.map_mut().add_raster("Image", RasterLayer::from_bytes(overview, "image.tif", bytes));
        (s, id, frame)
    }
    #[test]
    fn annotation_grid_command_reads_original_header_and_never_mutates_the_project() {
        let (mut s, id, expected) = session();
        let depth = s.undo_depth();
        let frame: Frame = serde_json::from_value(s.run("raster.imageGrid", &json!({"layer":id})).unwrap()).unwrap();
        assert_eq!(frame, expected);
        assert_eq!((frame.width, frame.height), (4, 6));
        assert_eq!(s.undo_depth(), depth);
    }
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn annotation_grid_command_waits_for_the_desktop_queue_when_asked() {
        let (mut s, id, expected) = session();
        s.statistics_need_read_ahead(true);
        assert!(s.run("raster.imageGrid", &json!({"layer":id})).unwrap_err().contains("background"));
        let ahead = crate::cmd::reads::read_ahead(&mut s, "raster.imageGrid", &json!({"layer":id})).unwrap();
        let done = ahead.run(&rustgis_analysis::control::Control::new()).unwrap();
        assert_eq!(serde_json::from_value::<Frame>(crate::cmd::reads::read_land(&mut s, done).unwrap()).unwrap(), expected);
    }
    #[test]
    fn annotation_grid_refuses_an_image_drawn_as_a_scan() {
        let (mut s, id, _) = session();
        let raster = s.map_mut().layer_mut(id).unwrap().raster.as_mut().unwrap();
        let shown = Arc::unwrap_or_clone(raster.data.clone().unwrap()).into_scan().unwrap();
        raster.data = Some(Arc::new(shown));
        let depth = s.undo_depth();
        let error = s.run("raster.imageGrid", &json!({"layer":id})).unwrap_err();
        assert!(error.contains("not on its file's own grid"), "{error}");
        assert_eq!(s.undo_depth(), depth);
        let raster = s.map_mut().layer_mut(id).unwrap().raster.as_mut().unwrap();
        raster.placement = Some([0., 0., 1., 1.]);
        assert!(s.run("raster.imageGrid", &json!({"layer":id})).unwrap_err().contains("not on its file's own grid"));
    }
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn annotation_grid_file_changes_before_selection_or_queued_read_require_a_pixel_reload() {
        for queued in [false, true] {
            let (mut s, id, original) = session();
            let folder = std::env::temp_dir().join(format!("rustgis-annotation-grid-file-{}-{queued}", std::process::id()));
            std::fs::create_dir_all(&folder).unwrap();
            let path = folder.join("image.tif");
            let bytes = s.map().layer(id).unwrap().raster.as_ref().unwrap().embedded.clone().unwrap();
            std::fs::write(&path, bytes.as_slice()).unwrap();
            let pixels = rustgis_data::raster::read_path(&path).unwrap();
            s.map_mut().layer_mut(id).unwrap().raster = Some(RasterLayer::from_path(pixels, path.display().to_string()));
            let displayed = s.map().layer(id).unwrap().raster.as_ref().unwrap().data.clone().unwrap();
            assert_eq!(serde_json::from_value::<Frame>(s.run("raster.imageGrid", &json!({"layer":id})).unwrap()).unwrap(), original);
            let ahead = queued.then(|| crate::cmd::reads::read_ahead(&mut s, "raster.imageGrid", &json!({"layer":id})).unwrap());
            let replacement = Raster::new(
                "new",
                7,
                3,
                vec![vec![2.; 21]],
                GeoTransform { origin_x: 5000., origin_y: 9000., pixel_width: 10., pixel_height: -10. },
                Crs::from_epsg(3857).unwrap(),
                None,
                ColorHint::Gray,
                "UInt8",
            )
            .unwrap();
            let replacement = geotiff::write(&replacement, 0).unwrap();
            let original_modified = std::fs::metadata(&path).unwrap().modified().unwrap();
            std::fs::write(&path, &replacement).unwrap();
            // Same length, and a coarse filesystem clock can keep the same time: make it differ.
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(original_modified + std::time::Duration::from_secs(1)))
                .unwrap();
            let depth = s.undo_depth();
            let result = if let Some(ahead) = ahead {
                let done = ahead.run(&rustgis_analysis::control::Control::new()).unwrap();
                crate::cmd::reads::read_land(&mut s, done)
            } else {
                s.run("raster.imageGrid", &json!({"layer":id}))
            };
            assert!(result.unwrap_err().contains("Reload Data"));
            assert_eq!(s.undo_depth(), depth);
            assert!(Arc::ptr_eq(&displayed, s.map().layer(id).unwrap().raster.as_ref().unwrap().data.as_ref().unwrap()));
            s.run("layer.reloadData", &json!({"layer":id})).unwrap();
            let expected = Frame::from_grid(&geotiff::read_image_grid(std::io::Cursor::new(&replacement)).unwrap()).unwrap();
            let frame: Frame = serde_json::from_value(s.run("raster.imageGrid", &json!({"layer":id})).unwrap()).unwrap();
            assert_eq!(frame, expected);
            assert_ne!(frame, original);
            std::fs::remove_dir_all(folder).unwrap();
        }
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn annotation_grid_converted_sources_offer_geotiff_recovery_without_a_reload_loop() {
        struct Dir(std::path::PathBuf);
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let dir = Dir(std::env::temp_dir().join(format!("rustgis-grid-converted-{}", std::process::id())));
        std::fs::create_dir_all(&dir.0).unwrap();
        let (mut s, id, expected) = session();
        let cache = dir.0.join("converted.tif");
        std::fs::write(&cache, s.map().layer(id).unwrap().raster.as_ref().unwrap().embedded.as_ref().unwrap().as_slice()).unwrap();
        let converted =
            rustgis_data::raster::gdal::Converted { path: cache.clone(), subdataset: None, others: Vec::new(), assumed_lonlat: false };
        for extension in ["nc", "jp2"] {
            let source = dir.0.join(format!("original.{extension}"));
            std::fs::write(&source, b"original source represented by the conversion result").unwrap();
            for queued in [false, true] {
                // Model the result of GDAL conversion, without needing GDAL installed for this test.
                for _ in 0..2 {
                    let pixels = rustgis_data::raster::read_converted(&source, &converted).unwrap();
                    s.map_mut().layer_mut(id).unwrap().raster = Some(RasterLayer::from_path(pixels, source.display().to_string()));
                    let depth = s.undo_depth();
                    let params = json!({"layer":id});
                    let result = if queued {
                        let read = crate::cmd::reads::read_ahead(&mut s, "raster.imageGrid", &params).unwrap();
                        let done = read.run(&rustgis_analysis::control::Control::new()).unwrap();
                        crate::cmd::reads::read_land(&mut s, done)
                    } else {
                        s.run("raster.imageGrid", &params)
                    };
                    let error = result.unwrap_err();
                    assert!(error.contains("GeoTIFF") && error.contains("Export") && error.contains("add"), "{error}");
                    assert!(!error.contains("changed") && !error.contains("Reload Data"), "{error}");
                    assert_eq!(s.undo_depth(), depth);
                }
            }
        }
        // Adding the actual GeoTIFF supplies a verified original grid.
        let pixels = rustgis_data::raster::read_path(&cache).unwrap();
        s.map_mut().layer_mut(id).unwrap().raster = Some(RasterLayer::from_path(pixels, cache.display().to_string()));
        assert_eq!(serde_json::from_value::<Frame>(s.run("raster.imageGrid", &json!({"layer":id})).unwrap()).unwrap(), expected);
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn annotation_grid_other_local_formats_offer_geotiff_recovery_without_a_reload_loop() {
        struct Dir(std::path::PathBuf);
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let dir = Dir(std::env::temp_dir().join(format!("rustgis-grid-local-formats-{}", std::process::id())));
        std::fs::create_dir_all(&dir.0).unwrap();
        let (mut s, id, expected) = session();
        for extension in ["asc", "png", "jpg", "jpeg"] {
            let path = dir.0.join(format!("image.{extension}"));
            if extension == "asc" {
                std::fs::write(&path, "ncols 2\nnrows 2\nxllcorner 0\nyllcorner 0\ncellsize 1\n1 2\n3 4\n").unwrap();
            } else {
                image::RgbImage::from_pixel(2, 2, image::Rgb([64, 128, 192])).save(&path).unwrap();
            }
            let pixels = rustgis_data::raster::read_path(&path).unwrap();
            s.map_mut().layer_mut(id).unwrap().raster = Some(RasterLayer::from_path(pixels, path.display().to_string()));
            for queued in [false, true] {
                for _ in 0..2 {
                    let depth = s.undo_depth();
                    let params = json!({"layer":id});
                    let result = if queued {
                        let read = crate::cmd::reads::read_ahead(&mut s, "raster.imageGrid", &params).unwrap();
                        let done = read.run(&rustgis_analysis::control::Control::new()).unwrap();
                        crate::cmd::reads::read_land(&mut s, done)
                    } else {
                        s.run("raster.imageGrid", &params)
                    };
                    let error = result.unwrap_err();
                    assert!(error.contains("GeoTIFF") && error.contains("Export") && error.contains("add"), "{extension}: {error}");
                    assert!(!error.contains("Reload Data") && !error.contains("file changed"), "{extension}: {error}");
                    assert_eq!(s.undo_depth(), depth);
                    s.run("layer.reloadData", &params).unwrap();
                }
            }
        }
        // A real TIFF, including an uppercase extension, still supplies its original grid.
        let path = dir.0.join("original.TIFF");
        let (source, source_id, _) = session();
        std::fs::write(&path, source.map().layer(source_id).unwrap().raster.as_ref().unwrap().embedded.as_ref().unwrap().as_slice())
            .unwrap();
        let pixels = rustgis_data::raster::read_path(&path).unwrap();
        s.map_mut().layer_mut(id).unwrap().raster = Some(RasterLayer::from_path(pixels, path.display().to_string()));
        assert_eq!(serde_json::from_value::<Frame>(s.run("raster.imageGrid", &json!({"layer":id})).unwrap()).unwrap(), expected);
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn annotation_grid_script_worker_returns_frames_without_reopening_on_the_window_thread() {
        for replace in [false, true] {
            let (mut s, id, expected) = session();
            let dir = std::env::temp_dir().join(format!("rustgis-grid-script-{}-{replace}", std::process::id()));
            struct Dir(std::path::PathBuf);
            impl Drop for Dir {
                fn drop(&mut self) {
                    let _ = std::fs::remove_dir_all(&self.0);
                }
            }
            let dir = Dir(dir);
            std::fs::create_dir_all(&dir.0).unwrap();
            let path = dir.0.join("image.tif");
            let bytes = s.map().layer(id).unwrap().raster.as_ref().unwrap().embedded.clone().unwrap();
            std::fs::write(&path, bytes.as_slice()).unwrap();
            s.map_mut().layer_mut(id).unwrap().raster =
                Some(RasterLayer::from_path(rustgis_data::raster::read_path(&path).unwrap(), path.display().to_string()));
            let params = json!({"script":format!("raster.imageGrid {}\nraster.imageGrid {}", json!({"layer":id}), json!({"layer":id}))});
            let direct = s.run("script.run", &params).unwrap();
            assert_eq!(serde_json::from_value::<Frame>(direct["ran"][0]["result"].clone()).unwrap(), expected);
            s.statistics_need_read_ahead(true);
            let ahead = crate::cmd::reads::read_ahead(&mut s, "script.run", &params).unwrap();
            let done = ahead.run(&rustgis_analysis::control::Control::new()).unwrap();
            if replace {
                let raster = s.map().layer(id).unwrap().raster.clone().unwrap();
                s.map_mut().layer_mut(id).unwrap().raster = Some(RasterLayer::from_bytes(
                    raster.data.as_ref().unwrap().as_ref().clone(),
                    "replacement.tif",
                    bytes.as_ref().clone(),
                ));
            }
            // No file can be reopened at landing. Frames remain bound to the loaded pixels.
            std::fs::remove_file(&path).unwrap();
            let result = crate::cmd::reads::read_land(&mut s, done);
            if replace {
                assert!(result.unwrap_err().contains("source changed"));
            } else {
                let answer = result.unwrap();
                for index in [0, 1] {
                    assert_eq!(serde_json::from_value::<Frame>(answer["ran"][index]["result"].clone()).unwrap(), expected);
                    assert!(answer["ran"][index]["result"].get("started").is_none());
                }
            }
            assert!(s.grids_ahead.is_none(), "prepared queries must not leak into later scripts");
            let unsafe_prefix = json!({"script":format!("project.new {{}}\nraster.imageGrid {}", json!({"layer":id}))});
            let before = s.opened;
            assert!(crate::cmd::reads::read_ahead(&mut s, "script.run", &unsafe_prefix).err().unwrap().contains("separate scripts"));
            assert_eq!(s.opened, before);
        }
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn annotation_grid_header_read_records_provenance_without_a_content_hash() {
        let (s, id, expected) = session();
        let dir = std::env::temp_dir().join(format!("rustgis-grid-provenance-{}", std::process::id()));
        struct Dir(std::path::PathBuf);
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let dir = Dir(dir);
        std::fs::create_dir_all(&dir.0).unwrap();
        let path = dir.0.join("image.tif");
        std::fs::write(&path, s.map().layer(id).unwrap().raster.as_ref().unwrap().embedded.as_ref().unwrap().as_slice()).unwrap();
        let layer = RasterLayer::from_path(rustgis_data::raster::read_path(&path).unwrap(), path.display().to_string());
        let capture = rustgis_data::provenance::Capture::start();
        assert_eq!(read(&layer).unwrap(), expected);
        let inputs = capture.inputs();
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].path, path.display().to_string());
        assert_eq!(inputs[0].size, Some(std::fs::metadata(&path).unwrap().len()));
        assert!(inputs[0].sha256.is_none());
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn interpolation_session() -> (Session, u32) {
        use rustgis_data::{Dataset, Field, FieldType, Geometry, GeometryType, Value as Cell};
        let (mut s, _, _) = session();
        let mut samples = Dataset::new("Samples", GeometryType::Point, vec![Field::new("v", FieldType::Double)]);
        samples.crs = Some(Crs::LonLat);
        for (x, y, value) in [(0., 0., 1.), (2., 0., 2.), (0., 2., 3.), (2., 2., 4.)] {
            samples.push(Some(Geometry::Point(rustgis_geom::Coord::new(x, y))), vec![Cell::Float(value)]);
        }
        let id = s.map_mut().add_dataset(samples);
        (s, id)
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn interpolation_line(tool: &str, cell: f64) -> String {
        format!(
            "gp.run {}",
            json!({"tool":tool,"params":{"input":"Samples","field":"v","extent":"0,0,2,2","pixelSize":cell,"output":"Surface"}})
        )
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn grid_line(layer: &str) -> String {
        format!("raster.imageGrid {}", json!({"layer":layer}))
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn annotation_grid_script_replays_generated_rasters_and_every_queued_query() {
        use rustgis_map::raster::Resampling;
        for multi_output in [false, true] {
            let (lines, indices, taken) = if multi_output {
                (
                    vec![
                        interpolation_line("kriging", 0.5),
                        grid_line("Surface"),
                        grid_line("Surface_variance"),
                        grid_line("Surface"),
                        grid_line("Surface_variance"),
                    ],
                    vec![1, 2, 3, 4],
                    1,
                )
            } else {
                (
                    vec![
                        interpolation_line("idwInterpolation", 0.5),
                        grid_line("Surface"),
                        grid_line("Image"),
                        grid_line("Surface"),
                        interpolation_line("idwInterpolation", 1.),
                        grid_line("Surface (2)"),
                        grid_line("Surface"),
                        grid_line("Image"),
                    ],
                    vec![1, 2, 3, 5, 6, 7],
                    2,
                )
            };
            let params = json!({"script":lines.join("\n")});
            let (mut direct, _) = interpolation_session();
            let expected = direct.run("script.run", &params).unwrap();
            let (mut queued, _) = interpolation_session();
            queued.statistics_need_read_ahead(true);
            let ahead = crate::cmd::reads::read_ahead(&mut queued, "script.run", &params).unwrap();
            let done = ahead.run(&rustgis_analysis::control::Control::new()).unwrap();
            // Worker data identity must not replace the user's current display defaults.
            queued.raster_defaults.resampling = Resampling::Bilinear;
            let result = crate::cmd::reads::read_land(&mut queued, done).unwrap();
            assert_eq!(queued.tools_taken, taken, "later tools must take their worker result too");
            for index in indices {
                let actual: Frame = serde_json::from_value(result["ran"][index]["result"].clone()).unwrap();
                let expected: Frame = serde_json::from_value(expected["ran"][index]["result"].clone()).unwrap();
                assert_eq!(actual, expected, "grid at script line {}", index + 1);
            }
            for name in if multi_output { ["Surface", "Surface_variance"] } else { ["Surface", "Surface (2)"] } {
                let output = queued.map().layer_by_name(name).unwrap();
                assert_eq!(output.raster.as_ref().unwrap().style.resampling, Resampling::Bilinear);
            }
            assert!(queued.grids_ahead.is_none() && queued.tools_ahead.is_none());
        }
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn annotation_grid_rejected_tool_discards_all_script_grids_and_allows_a_fresh_retry() {
        let (mut s, samples) = interpolation_session();
        let script =
            [interpolation_line("idwInterpolation", 0.5), grid_line("Surface"), grid_line("Image"), grid_line("Surface")].join("\n");
        let params = json!({"script":script});
        s.statistics_need_read_ahead(true);
        let ahead = crate::cmd::reads::read_ahead(&mut s, "script.run", &params).unwrap();
        let done = ahead.run(&rustgis_analysis::control::Control::new()).unwrap();
        s.run("edit.enable", &json!({"on":true})).unwrap();
        s.run("edit.setValue", &json!({"layer":samples,"row":0,"field":"v","value":9})).unwrap();
        let before = (s.undo_depth(), s.map().layers.len());
        let error = crate::cmd::reads::read_land(&mut s, done).unwrap_err();
        assert!(error.contains("script inputs changed"), "{error}");
        assert_eq!(s.tools_taken, 0);
        assert_eq!((s.undo_depth(), s.map().layers.len()), before);
        assert!(s.map().layer_by_name("Surface").is_none());
        assert!(s.grids_ahead.is_none() && s.tools_ahead.is_none());
        // A new read must prepare every result again, including the existing image's grid.
        let ahead = crate::cmd::reads::read_ahead(&mut s, "script.run", &params).unwrap();
        let done = ahead.run(&rustgis_analysis::control::Control::new()).unwrap();
        let answer = crate::cmd::reads::read_land(&mut s, done).unwrap();
        assert_eq!(s.tools_taken, 1);
        for (index, name) in [(1, "Surface"), (2, "Image"), (3, "Surface")] {
            let layer = s.map().layer_by_name(name).unwrap();
            let expected = read(layer.raster.as_ref().unwrap()).unwrap();
            let frame: Frame = serde_json::from_value(answer["ran"][index]["result"].clone()).unwrap();
            assert_eq!(frame, expected);
        }
        assert!(s.grids_ahead.is_none() && s.tools_ahead.is_none());
    }

    #[test]
    fn annotation_grid_background_read_cancels_and_rejects_replaced_sources() {
        let (mut s, id, _) = session();
        let ahead = crate::cmd::reads::read_ahead(&mut s, "raster.imageGrid", &json!({"layer":id})).unwrap();
        let control = rustgis_analysis::control::Control::new();
        control.cancel();
        assert!(ahead.run(&control).is_err());
        let ahead = crate::cmd::reads::read_ahead(&mut s, "raster.imageGrid", &json!({"layer":id})).unwrap();
        let done = ahead.run(&rustgis_analysis::control::Control::new()).unwrap();
        s.map_mut().layer_mut(id).unwrap().raster.as_mut().unwrap().embedded = Some(Arc::new(vec![0; 32]));
        assert!(crate::cmd::reads::read_land(&mut s, done).is_err());
    }
    #[test]
    fn annotation_grid_scoped_reads_preserve_approved_selectors_and_refuse_retargeting() {
        use crate::policy::{Call, Outcome, Scope};
        struct Live;
        impl crate::policy::Revocation for Live {
            fn allows(&self, _: crate::policy::Effects) -> bool {
                true
            }
        }
        for named in [false, true] {
            for retarget in [false, true] {
                let (mut s, id, expected) = session();
                s.active_layer = Some(id);
                let params = if named { json!({"layer":"Image"}) } else { json!({}) };
                let call = Call::command("raster.imageGrid", params);
                let scope = Scope::assistant(1717, Arc::new(Live));
                let Outcome::NeedsApproval(pending) = s.review(&scope, &call) else { panic!("image reads require Files approval") };
                let read = s.start_scoped_archive_read(scope, &call, Some(pending.allow())).unwrap();
                let done = read.run(&rustgis_analysis::control::Control::new()).unwrap();
                if retarget {
                    s.run("layer.rename", &json!({"layer":id,"name":"Old image"})).unwrap();
                    let raster = s.map().layer(id).unwrap().raster.clone().unwrap();
                    let other = s.map_mut().add_raster("Image", raster);
                    s.active_layer = Some(other);
                }
                let answer = crate::cmd::reads::read_land(&mut s, done);
                if retarget {
                    assert!(answer.is_err());
                } else {
                    assert_eq!(serde_json::from_value::<Frame>(answer.unwrap()).unwrap(), expected);
                }
            }
        }
    }
}
