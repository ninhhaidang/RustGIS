//! The GPU camera fast path must yield one full re-render once the camera settles:
//! the scene's contents depend on the render-time camera (extent culling, the 0.35 px
//! decimation, label placement). Regression test for the PR #309 review (coderabbit +
//! github-actions, both flagging the missing settle render).
//! Planetary tile scenes also reuse uploaded images and preview layer changes immediately.

use rustgis_ui_egui::RustGisApp;
use serde_json::{Value, json};

/// The tile cache and the streamed layers' reads are global: their generations are part of
/// what a frame was drawn for, so a tile another test fetched, arriving mid-motion, draws the
/// map again. The tests here run one at a time.
fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Waits for worker results, downloaded tiles and the GPU callbacks that upload their pictures.
/// UI steps alone do not run paced uploads; every candidate settled frame must be painted.
fn wait_for_frame(h: &mut egui_kittest::Harness<'_, RustGisApp>) {
    use rustgis_ui_egui::rustgis_engine::render::rustgis_tiles::cache;
    // A hang guard: a software adapter (WARP on the Windows runners) is many times slower.
    let end = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        assert!(std::time::Instant::now() < end, "frame workers, tiles or GPU uploads did not settle");
        let tiles = cache::generation();
        for _ in 0..40 {
            h.step();
        }
        // Painting can discover another upload backlog. Let the next UI frame adopt it
        // before treating drawing=false as proof that the displayed scene is complete.
        // Uploads are paced per paint (fewer a paint on a slow adapter): paint until done.
        loop {
            h.render().expect("paint frame and advance tile uploads");
            h.step();
            if !h.state().map.drawing() || std::time::Instant::now() >= end {
                break;
            }
        }
        if !h.state().map.drawing() && cache::in_flight() == 0 && cache::generation() == tiles {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2)); // test-wait: legacy polling for a settled GPU frame
    }
}

fn serve_planet_tiles() {
    use rustgis_ui_egui::rustgis_engine::render::rustgis_tiles;
    let mut png = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_fn(256, 256, |x, y| image::Rgba([100 + (x / 2) as u8, 30 + (y / 8) as u8, 50, 255]))
        .write_to(&mut png, image::ImageFormat::Png)
        .expect("tile");
    rustgis_tiles::cache::set_fetcher(move |_, done| done(Ok(png.get_ref().clone())));
}

/// A wgpu harness sharing one render state with the app (the ui_shot pattern), so the GPU
/// backend engages. `None` on hosts without an adapter.
fn harness() -> Option<egui_kittest::Harness<'static, RustGisApp>> {
    let setup = egui_wgpu::WgpuSetup::CreateNew(egui_wgpu::WgpuSetupCreateNew::without_display_handle());
    let instance = pollster::block_on(setup.new_instance());
    let rs = pollster::block_on(egui_wgpu::RenderState::create(
        &egui_wgpu::WgpuConfiguration { wgpu_setup: setup, ..Default::default() },
        &instance,
        None,
        egui_wgpu::RendererOptions::PREDICTABLE,
    ))
    .ok()?;
    let mut app = RustGisApp::with_sample();
    app.set_render_state(Some(rs.clone()));
    app.run("ui.map.gpu", json!({"on": true})).expect("gpu on");
    Some(
        egui_kittest::Harness::builder()
            .with_size(egui::vec2(900.0, 550.0))
            .with_pixels_per_point(1.0)
            .with_max_steps(10_000)
            .renderer(egui_kittest::wgpu::WgpuTestRenderer::from_render_state(rs))
            .build_ui_state(
                |ui, app: &mut RustGisApp| {
                    let ctx = ui.ctx().clone();
                    app.logic(&ctx);
                    app.ui(ui);
                },
                app,
            ),
    )
}

#[test]
fn settling_after_a_camera_animation_re_renders_fully() {
    let _serial = one_at_a_time();
    let Some(mut h) = harness() else { return };
    for _ in 0..6 {
        h.step();
    }
    let app = h.state_mut();
    app.run("animation.add", Value::Null).expect("keyframe 1");
    app.run("map.setScale", json!({"scale": 20_000_000.0})).expect("zoom keyframe");
    app.run("animation.add", Value::Null).expect("keyframe 2");
    app.run("ui.animationPlay", Value::Null).expect("play");
    // Pump frames until the animation settles (playing is None again), then a few more.
    let mut settled = false;
    for _ in 0..9_000 {
        h.step();
        if h.state().ws.playing.is_none() {
            settled = true;
            break;
        }
    }
    assert!(settled, "the animation finished");
    // A flat map's settled frame is drawn off the UI thread (#1750): wait for it to arrive.
    wait_for_frame(&mut h);
    let app = h.state();
    let (cam, tex_cam) = (app.session.map().camera, app.map.full_frame_camera());
    assert_eq!((tex_cam.center, tex_cam.res), (cam.center, cam.res), "a full re-render followed the settled camera (tex_camera refreshed)");
}

/// Compare the displayed GPU picture with the exact ground under the current camera.
fn assert_gpu_picture_matches_cpu(h: &mut egui_kittest::Harness<'_, RustGisApp>) {
    assert!(h.output().shapes.iter().any(|s| matches!(s.shape, egui::Shape::Callback(_))), "the GPU backend engaged");
    // Exercise the actual blit shader as well: a shifted/scaled image must sample the same
    // ground as an exact CPU render, even when its rectangle extends past the window.
    let image = h.render().expect("moving GPU image");
    let rect = h.state().map.view_rect.expect("map rectangle").0;
    let view = rustgis_ui_egui::rustgis_engine::render::View {
        width: rect.width() as f64,
        height: rect.height() as f64,
        pixels_per_point: 1.0,
        camera: h.state().session.map().camera,
        sky: true,
    };
    let expected = rustgis_ui_egui::rustgis_engine::render::render(&mut h.state().session.map().clone(), &view).expect("CPU reference");
    let mut error = 0u32;
    let mut samples = 0u32;
    for y in (rect.height() as u32 / 3..rect.height() as u32 * 2 / 3).step_by(11) {
        for x in (rect.width() as u32 / 3..rect.width() as u32 * 2 / 3).step_by(11) {
            let actual = image.get_pixel(rect.min.x.round() as u32 + x, rect.min.y.round() as u32 + y);
            let expected = expected.pixmap.pixel(x, y).expect("reference pixel");
            error += actual[0].abs_diff(expected.red()) as u32 + actual[1].abs_diff(expected.green()) as u32;
            samples += 2;
        }
    }
    assert!(error < samples * 6, "cached GPU image moved to the wrong ground: mean color error {}", error / samples);
}

#[test]
fn zooming_a_planet_with_raster_tiles_reuses_the_gpu_frame() {
    use rustgis_ui_egui::rustgis_engine::map::{TileKind, TileLayer};
    let _serial = one_at_a_time();
    serve_planet_tiles();
    let Some(mut h) = harness() else { return };
    h.state_mut().run("map.body", json!({"name": "moon", "basemap": false})).expect("Moon");
    h.state_mut().session.map_mut().layers.clear();
    h.state_mut().session.map_mut().graticule = false;
    // Keep both pictures at the same tile level: this test compares geometry, while the
    // exact render can otherwise choose finer tiles than the cached moving picture.
    let mut tiles = TileLayer::new("Moon", TileKind::Raster, "https://planet.test/{z}/{x}/{y}.png");
    tiles.min_zoom = 3;
    tiles.max_zoom = 3;
    h.state_mut().session.map_mut().tiles.push(tiles);
    wait_for_frame(&mut h);
    assert!(h.output().shapes.iter().any(|s| matches!(s.shape, egui::Shape::Callback(_))), "the GPU backend engaged");
    let before = h.state().map.full_frame_camera();
    for _ in 0..8 {
        h.input_mut().events.push(egui::Event::PointerMoved(egui::pos2(600.0, 350.0)));
        h.input_mut().events.push(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, 2.0),
            phase: egui::TouchPhase::Move,
            modifiers: Default::default(),
            source: egui::MouseWheelSource::Wheel,
        });
        h.step();
    }
    assert_ne!(h.state().session.map().camera.res, before.res, "zoom engaged");
    assert_eq!(h.state().map.full_frame_camera(), before, "navigation must reuse the uploaded image instead of redrawing every frame");
    assert_gpu_picture_matches_cpu(&mut h);
    wait_for_frame(&mut h);
    // Live tiles are placed for every frame: sharp at rest without drawing the scene again (#635).
    if h.state().map.shows_live_tiles() {
        assert_gpu_picture_matches_cpu(&mut h);
    } else {
        assert_eq!(h.state().map.full_frame_camera(), h.state().session.map().camera, "settling sharpens the image");
    }
    // Start from a CPU preview, then force the first GPU frame to fill a pan gap. Its
    // oversized picture must use cropped UVs immediately, including this very first frame.
    h.state_mut().map.invalidate();
    let from = egui::pos2(600.0, 350.0);
    h.input_mut().events.push(egui::Event::PointerMoved(from));
    h.input_mut().events.push(egui::Event::PointerButton {
        pos: from,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: Default::default(),
    });
    h.step();
    h.state_mut().map.invalidate();
    h.input_mut().events.push(egui::Event::PointerMoved(from + egui::vec2(16.0, 0.0)));
    h.step();
    assert_gpu_picture_matches_cpu(&mut h);
}

#[test]
fn planet_layer_toggles_show_in_the_next_frame_and_stale_jobs_do_not_restore_them() {
    use rustgis_ui_egui::rustgis_engine::{
        data::{GeometryType, io::read_geojson},
        map::{Renderer, Rgba, Symbol, TileKind, TileLayer},
    };
    let _serial = one_at_a_time();
    serve_planet_tiles();
    let Some(mut h) = harness() else { return };
    h.state_mut().run("map.body", json!({"name": "moon", "basemap": false})).expect("Moon");
    let m = h.state_mut().session.map_mut();
    m.layers.clear();
    m.graticule = false;
    m.tiles.push(TileLayer::new("Moon", TileKind::Raster, "https://planet.test/{z}/{x}/{y}.png"));
    let (data, _) = read_geojson(r#"{"type":"Feature","properties":{},"geometry":{"type":"Polygon","coordinates":[[[-30,-20],[50,-20],[50,55],[-30,55],[-30,-20]]]}}"#, "Overlay").expect("overlay");
    let id = m.add_dataset(data);
    m.layer_mut(id).expect("layer").renderer =
        Renderer::Single(Symbol { fill: Rgba::rgb(20, 220, 20), ..Symbol::default_for(GeometryType::Polygon, 0) });
    for projection in ["globe", "mercator"] {
        for gpu in [false, true] {
            h.state_mut().run("map.projection", json!({"type": projection})).expect("projection");
            h.state_mut().run("ui.map.gpu", json!({"on": gpu})).expect("backend");
            wait_for_frame(&mut h);
            for visible in [true, false, true, false] {
                h.state_mut().run("layer.visibility", json!({"layer": id, "visible": visible})).expect("toggle");
                h.step();
                assert!(h.state().map.drawing(), "exact drawing runs off the UI thread");
                let image = h.render().expect("visibility preview");
                let center = h.state().map.view_rect.expect("map").0.center();
                let p = image.get_pixel(center.x.round() as u32, center.y.round() as u32);
                assert_eq!(
                    u16::from(p[1]) > u16::from(p[0]) * 2,
                    visible,
                    "{projection} GPU={gpu} visibility did not update immediately: {p:?}"
                );
            }
            wait_for_frame(&mut h);
            let image = h.render().expect("settled frame");
            let center = h.state().map.view_rect.expect("map").0.center();
            let p = image.get_pixel(center.x.round() as u32, center.y.round() as u32);
            // The overlay's green, as above: a tile still to come shows the body's plain grey,
            // which is no overlay either (macOS CI drew one such frame).
            assert!(u16::from(p[1]) <= u16::from(p[0]) * 2, "{projection} GPU={gpu}: an old worker restored a hidden overlay: {p:?}");
        }
    }
}

#[test]
fn tiled_camera_playback_does_not_start_settling_jobs() {
    use rustgis_ui_egui::rustgis_engine::map::{TileKind, TileLayer};
    let _serial = one_at_a_time();
    serve_planet_tiles();
    for gpu in [false, true] {
        let Some(mut h) = harness() else { return };
        h.state_mut().run("ui.map.gpu", json!({"on": gpu})).expect("backend");
        h.state_mut().run("map.body", json!({"name": "moon", "basemap": false})).expect("Moon");
        h.state_mut().session.map_mut().layers.clear();
        h.state_mut().session.map_mut().tiles.push(TileLayer::new("Moon", TileKind::Raster, "https://planet.test/{z}/{x}/{y}.png"));
        wait_for_frame(&mut h);
        let start = h.state().session.map().camera;
        h.state_mut().run("animation.add", Value::Null).expect("first frame");
        h.state_mut().run("map.setScale", json!({"scale": 20_000_000.0})).expect("zoom");
        h.state_mut().run("animation.add", Value::Null).expect("second frame");
        h.state_mut().run("ui.animationPlay", Value::Null).expect("play");
        for _ in 0..4 {
            h.step();
            assert!(h.state().ws.playing.is_some(), "animation is playing");
            assert!(!h.state().map.drawing(), "playback must render the current frame, not start a stale settling job");
            // On the GPU the tiles move with the camera, as meshes do (#568).
            if !(gpu && h.state().map.moves_on_gpu()) {
                assert_eq!(h.state().map.full_frame_camera(), h.state().session.map().camera);
            }
        }
        assert_ne!(h.state().session.map().camera.res, start.res, "playback moved the camera");
    }
}

/// A flat map with raster tiles stays sharp while it moves (#568): its tiles are placed on the
/// GPU and move with the camera, as vector meshes do, rather than a picture of the view being
/// shifted and stretched until the motion settles; zoomed in far enough, the tiles for the new
/// zoom are placed mid-motion.
#[test]
fn raster_tiles_move_with_the_camera_on_the_gpu() {
    use rustgis_ui_egui::rustgis_engine::map::{TileKind, TileLayer};
    let _serial = one_at_a_time();
    serve_planet_tiles();
    let Some(mut h) = harness() else { return };
    let m = h.state_mut().session.map_mut();
    m.layers.clear();
    m.graticule = false;
    m.tiles.push(TileLayer::new("Tiles", TileKind::Raster, "https://flat.test/{z}/{x}/{y}.png"));
    // The harness drew a first frame of the sample: these edits bypass commands, so draw again.
    h.state_mut().map.invalidate();
    wait_for_frame(&mut h);
    assert!(h.state().map.moves_on_gpu(), "the tiles are placed on the GPU, not drawn into a picture of the view");
    let before = h.state().map.full_frame_camera();
    let wheel = |h: &mut egui_kittest::Harness<'_, RustGisApp>, steps: usize| {
        for _ in 0..steps {
            h.input_mut().events.push(egui::Event::PointerMoved(egui::pos2(450.0, 300.0)));
            h.input_mut().events.push(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, 0.5),
                phase: egui::TouchPhase::Move,
                modifiers: Default::default(),
                source: egui::MouseWheelSource::Wheel,
            });
            h.step();
        }
    };
    // A little: the same tiles, moved by the camera.
    wheel(&mut h, 1);
    let res = h.state().session.map().camera.res;
    assert!(res < before.res && res > before.res / 1.1, "zoomed in a little: {} to {res}", before.res);
    assert_eq!(h.state().map.full_frame_camera(), before, "the tiles move with the camera, not drawn again");
    assert_gpu_picture_matches_cpu(&mut h);
    // Further: the tiles for the new zoom, before the motion settles.
    for _ in 0..200 {
        if h.state().session.map().camera.res < before.res / 1.3 {
            break;
        }
        wheel(&mut h, 1);
    }
    // Live tiles take the new zoom's tiles in the same scene (#635); otherwise a new scene does.
    if !h.state().map.shows_live_tiles() {
        assert_ne!(h.state().map.full_frame_camera(), before, "zoomed in past the tiles' own zoom, the new zoom's tiles are placed");
    }
    assert!(h.state().map.moves_on_gpu());
    assert_gpu_picture_matches_cpu(&mut h);
}

/// Waiting for a live scene must include paints: tile uploads are paced in the GPU callback,
/// so CPU composition and finished downloads alone do not make the displayed frame complete.
#[test]
fn waiting_for_live_tiles_paints_until_the_top_layer_is_uploaded() {
    use rustgis_ui_egui::rustgis_engine::{
        map::{TileKind, TileLayer},
        render::rustgis_tiles,
    };
    let _serial = one_at_a_time();
    let png = |color| {
        let mut out = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(256, 256, image::Rgba(color)).write_to(&mut out, image::ImageFormat::Png).expect("tile");
        out.into_inner()
    };
    let bottom = png([180, 40, 40, 255]);
    let top = png([30, 40, 200, 255]);
    rustgis_tiles::cache::set_fetcher(move |url, done| done(Ok(if url.contains("upload-top") { top.clone() } else { bottom.clone() })));
    let Some(mut h) = harness() else { return };
    h.state_mut().run("map.body", json!({"name": "moon", "basemap": false})).expect("Moon");
    h.state_mut().run("map.projection", json!({"type": "mercator"})).expect("projection");
    h.state_mut().run("map.setScale", json!({"scale": 10_000_000.0})).expect("tile scale");
    let m = h.state_mut().session.map_mut();
    m.layers.clear();
    m.graticule = false;
    // More independent pictures than one upload frame accepts, even on a fast adapter.
    for n in 0..31 {
        m.tiles.push(TileLayer::new(
            format!("Bottom {n}"),
            TileKind::Raster,
            format!("https://upload-bottom-{n}.test/{{z}}/{{x}}/{{y}}.png"),
        ));
    }
    m.tiles.insert(0, TileLayer::new("Top", TileKind::Raster, "https://upload-top.test/{z}/{x}/{y}.png"));
    h.state_mut().map.invalidate();
    wait_for_frame(&mut h);
    let image = h.render().expect("settled tiles");
    let center = h.state().map.view_rect.expect("map").0.center();
    let p = image.get_pixel(center.x.round() as u32, center.y.round() as u32);
    assert!(u16::from(p[2]) > u16::from(p[0]) * 2, "the waiter returned before the top live layer was uploaded: {p:?}");
    h.step();
    assert!(!h.state().map.drawing(), "painting the settled frame must not reveal an upload backlog");
}

/// Zoomed in past where f32 holds the basemap's meshes, they are drawn on the CPU (#902): the
/// scene is a picture, shifted and scaled until it is drawn again, and the live tiles over it
/// must move with it, not be placed for the new camera and then moved again (a WMS layer
/// flickered and stepped back on each wheel step out).
#[test]
fn live_tiles_move_with_a_scene_shown_as_a_picture() {
    use rustgis_ui_egui::rustgis_engine::geom::Coord;
    use rustgis_ui_egui::rustgis_engine::map::{TileKind, TileLayer};
    use rustgis_ui_egui::rustgis_engine::render::rustgis_tiles;
    let _serial = one_at_a_time();
    // A checkerboard: tiles out of place show at once.
    let mut png = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_fn(256, 256, |x, y| {
        image::Rgba(if (x / 16 + y / 16) % 2 == 0 { [250, 250, 250, 255] } else { [10, 10, 10, 255] })
    })
    .write_to(&mut png, image::ImageFormat::Png)
    .expect("tile");
    rustgis_tiles::cache::set_fetcher(move |_, done| done(Ok(png.get_ref().clone())));
    let Some(mut h) = harness() else { return };
    let m = h.state_mut().session.map_mut();
    m.layers.clear();
    m.graticule = false;
    m.tiles.push(TileLayer::new("WMS", TileKind::Raster, "https://wms.test/?BBOX={bbox-epsg-3857}&WIDTH=256&HEIGHT=256"));
    // Bern, past the layer's max zoom (19).
    m.camera.center = Coord::new(828_000.0, 5_933_000.0);
    m.camera.res = 0.01;
    h.state_mut().map.invalidate();
    wait_for_frame(&mut h);
    assert!(!h.state().map.moves_on_gpu(), "the basemap is drawn on the CPU this far in: the scene is a picture");
    let start = h.state().map.full_frame_camera();
    let mut shifted = 0;
    for _ in 0..12 {
        h.input_mut().events.push(egui::Event::PointerMoved(egui::pos2(450.0, 300.0)));
        h.input_mut().events.push(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -40.0),
            phase: egui::TouchPhase::Move,
            modifiers: Default::default(),
            source: egui::MouseWheelSource::Wheel,
        });
        h.step();
        if h.state().map.full_frame_camera() != h.state().session.map().camera {
            shifted += 1;
        }
        assert_gpu_picture_matches_cpu(&mut h);
    }
    assert!(shifted > 0, "some steps show the picture shifted and scaled, not drawn again");
    assert!(h.state().session.map().camera.res > start.res * 2.0, "zoomed out");
}
