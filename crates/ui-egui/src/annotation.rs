//! Session-only image/grid choices shared by Annotate and the ordinary Edit tools.
use crate::{Landed, RightPane, RustGisApp};
use rustgis_engine::{Tool, box_edit::Frame, image_grid::Source};
use serde_json::{Value, json};

#[derive(Default)]
pub(crate) struct State {
    context: Option<(u64, u64)>,
    image: Option<u32>,
    image_name: Option<String>,
    image_chosen: bool,
    image_filename: bool,
    source: Option<Source>,
    pixels: bool,
    frame: Option<Frame>,
    pending: Option<u64>,
    next: u64,
    error: Option<String>,
}
fn retire_read(app: &mut RustGisApp) {
    if let Some(request) = app.annotation.pending.take() {
        #[cfg(not(target_arch = "wasm32"))]
        app.reads.cancel_annotation_grid(request);
        #[cfg(target_arch = "wasm32")]
        let _ = request;
    }
}
fn cancel(app: &mut RustGisApp) {
    retire_read(app);
    if crate::boxedit::new_drawing(app) || crate::circleedit::new_drawing(app) {
        drawing_cancelled(app);
    }
    crate::boxedit::cancel(app);
    crate::circleedit::cancel(app);
}
pub(crate) fn drawing_cancelled(app: &mut RustGisApp) {
    app.session.notify(
        tl!("Drawing cancelled because the map, image or construction grid changed. Check the grid, then start a new shape."),
        false,
    );
}
pub(crate) fn sync(app: &mut RustGisApp) {
    let context = (app.session.opened, app.session.map().instance_id());
    if app.annotation.context != Some(context) {
        cancel(app);
        let pixels = app.annotation.pixels;
        let error = pixels.then(|| {
            tl!("The project or map changed. Choose an image again or select Web Mercator grid metres before drawing.").to_string()
        });
        app.annotation =
            State { context: Some(context), next: app.annotation.next, pixels, image_chosen: pixels, error, ..State::default() };
        if let Some(error) = &app.annotation.error {
            app.session.notify(error.clone(), true);
        }
    } else {
        if let Some(name) = app.annotation.image.and_then(|id| app.session.map().layer(id)).filter(|l| l.raster.is_some()).map(|l| &l.name)
            && app.annotation.image_name.as_deref() != Some(name.as_str())
        {
            app.annotation.image_name = Some(name.clone());
        }
        if app.annotation.source.as_ref().is_some_and(|s| !s.current(&app.session)) {
            // Only a new shape drawn in this image's grid depends on it: a resize keeps its own
            // frame, and a new shape in map metres never used the image.
            retire_read(app);
            let (boxes, circles) = (crate::boxedit::new_drawing(app), crate::circleedit::new_drawing(app));
            if app.annotation.pixels && (boxes || circles) {
                if boxes {
                    crate::boxedit::cancel(app);
                }
                if circles {
                    crate::circleedit::cancel(app);
                }
                drawing_cancelled(app);
            }
            app.annotation.frame = None;
            app.annotation.source = None;
            app.annotation.error = Some(tl!("The image changed or was removed. Select it again or reload its grid.").into());
        }
    }
}
pub(crate) fn open(app: &mut RustGisApp) -> Result<(), String> {
    sync(app);
    app.ui.open_right(RightPane::Annotation);
    if !app.annotation.image_chosen
        && app.annotation.image.is_none()
        && let Some(id) = app.session.active_layer.filter(|id| app.session.map().layer(*id).is_some_and(|l| l.raster.is_some()))
        && let Err(error) = select_image(app, &json!({"image":id}))
    {
        app.annotation.image_chosen = true;
        app.annotation.error = Some(error);
    }
    Ok(())
}
pub(crate) fn select_image(app: &mut RustGisApp, p: &Value) -> Result<(), String> {
    sync(app);
    // The key is required: only an explicit null chooses No image (and map metres).
    let image = match p.get("image").ok_or_else(|| tl!("image is required; use null for No image").to_string())? {
        v if v.is_null() => None,
        v => Some(app.session.layer_id(&json!({"layer": v}))?),
    };
    let source = image.map(|id| Source::capture(&app.session, id)).transpose()?;
    let request = app.annotation.next.checked_add(1).ok_or_else(|| tl!("Image-grid request limit reached").to_string())?;
    cancel(app);
    app.annotation.next = request;
    app.annotation.image = image;
    app.annotation.image_name = image.and_then(|id| app.session.map().layer(id)).map(|l| l.name.clone());
    app.annotation.image_chosen = true;
    app.annotation.source = source;
    app.annotation.pixels = image.is_some();
    app.annotation.frame = None;
    app.annotation.error = None;
    app.annotation.pending = image.map(|_| request);
    if let Some(id) = image {
        #[cfg(not(target_arch = "wasm32"))]
        let _ = crate::readjob::run_image_grid(app, json!({"layer":id}), request);
        #[cfg(target_arch = "wasm32")]
        {
            let result = app.run("raster.imageGrid", json!({"layer":id}));
            landed(app, request, Landed::Ran(result));
        }
    }
    Ok(())
}
pub(crate) fn select_grid(app: &mut RustGisApp, pixels: bool) -> Result<(), String> {
    sync(app);
    if pixels && app.annotation.image.is_none() {
        return Err(tl!("Choose an image before using original-image pixels.").into());
    }
    if pixels && app.annotation.frame.is_none() {
        return select_image(app, &json!({"image":app.annotation.image}));
    }
    cancel(app);
    app.annotation.pixels = pixels;
    app.annotation.error = None;
    Ok(())
}
pub(crate) fn cancel_read(app: &mut RustGisApp) {
    cancel(app);
    app.annotation.error = Some(tl!("Grid read cancelled. Reload the grid to draw in image pixels.").into());
}
pub(crate) fn landed(app: &mut RustGisApp, request: u64, result: Landed) {
    if app.annotation.pending != Some(request) {
        return;
    }
    sync(app);
    if app.annotation.pending != Some(request) {
        return;
    }
    app.annotation.pending = None;
    match result {
        Landed::Ran(Ok(value)) => match serde_json::from_value(value) {
            Ok(frame) => app.annotation.frame = Some(frame),
            Err(e) => app.annotation.error = Some(e.to_string()),
        },
        Landed::Ran(Err(e)) | Landed::NotRun(e) => app.annotation.error = Some(e),
    }
    // The pending reason is obsolete on success or failure; grid_controls shows any failure.
    // Validation errors from an active gesture belong to that gesture and remain untouched.
    crate::boxedit::clear_idle_error(app);
    crate::circleedit::clear_idle_error(app);
}
/// Command Search's read-only grid query says its answer after background or inline delivery.
pub(crate) fn show_grid_result(app: &mut RustGisApp, result: Landed) {
    let Landed::Ran(Ok(value)) = result else { return };
    match serde_json::from_value::<Frame>(value) {
        Ok(frame) => app.session.notify(
            crate::i18n::fmt(
                "Original image grid: {width} × {height} pixels, EPSG:{epsg}",
                &[("width", &frame.width.to_string()), ("height", &frame.height.to_string()), ("epsg", &frame.epsg.to_string())],
            ),
            false,
        ),
        Err(_) => app.session.notify(tl!("The original image grid result could not be displayed."), true),
    }
}
/// Only new objects use this choice. Existing objects keep their own frozen frame.
pub(crate) fn drawing_frame(app: &RustGisApp) -> Result<Frame, String> {
    if !app.annotation.pixels {
        return Ok(Frame::default());
    }
    if app.annotation.context != Some((app.session.opened, app.session.map().instance_id())) {
        return Err(tl!("The image context changed. Select the image again before drawing.").into());
    }
    if app.annotation.image.is_none() {
        return Err(tl!("Choose an image or Web Mercator grid metres before drawing.").into());
    }
    if !app.annotation.source.as_ref().is_some_and(|s| s.current(&app.session)) {
        return Err(tl!("The image changed. Reload its grid before drawing.").into());
    }
    app.annotation
        .frame
        .clone()
        .ok_or_else(|| app.annotation.error.clone().unwrap_or_else(|| tl!("Wait for the original image grid before drawing.").into()))
}
fn polygon_layer(app: &RustGisApp, id: u32) -> bool {
    app.session.map().layer(id).is_some_and(is_polygon_layer)
}
fn is_polygon_layer(l: &rustgis_engine::map::Layer) -> bool {
    !l.is_streamed()
        && !l.graphics
        && l.raster.is_none()
        && matches!(l.geometry_type(), rustgis_engine::data::GeometryType::Polygon | rustgis_engine::data::GeometryType::None)
}
pub(crate) fn filename_option(app: &mut RustGisApp, on: bool) {
    sync(app);
    app.annotation.image_filename = on;
}
fn image_export_name(app: &RustGisApp) -> Option<String> {
    if app.annotation.context != Some((app.session.opened, app.session.map().instance_id()))
        || !app.annotation.source.as_ref()?.current(&app.session)
    {
        return None;
    }
    let raster = app.session.map().layer(app.annotation.image?)?.raster.as_ref()?;
    let file = raster.file_name.as_deref().or_else(|| raster.path.as_deref().filter(|p| rustgis_engine::map::paths::is_local(p)))?;
    // Use only the basename, including for Windows paths opened on another platform.
    let leaf = file.rsplit(['/', '\\']).next()?;
    let stem = std::path::Path::new(leaf).file_stem()?.to_str()?;
    if stem.is_empty() || matches!(stem, "." | "..") {
        return None;
    }
    Some(format!("{stem}.geojson"))
}
/// An annotation-only filename suggestion. The ordinary export action keeps its layer name.
/// Returns a basename, never a destination path; the existing save dialog chooses the location.
pub(crate) fn export_name(app: &RustGisApp, layer: u32, ext: &str, annotation: bool) -> Result<String, String> {
    if annotation && ext != "geojson" {
        return Err(tl!("Annotation export uses GeoJSON.").into());
    }
    if annotation && app.annotation.image_filename {
        return image_export_name(app)
            .ok_or_else(|| tl!("Select an image with an available filename, or uncheck Use image filename.").into());
    }
    Ok(app.session.map().layer(layer).map(|l| format!("{}.{ext}", l.name)).unwrap_or_default())
}
pub(crate) fn select_layer(app: &mut RustGisApp, p: &Value) -> Result<(), String> {
    let id = app.session.layer_id(p)?;
    if !polygon_layer(app, id) {
        return Err(tl!("Choose a loaded editable polygon layer.").into());
    }
    app.run("layer.select", json!({"layer":id})).map(|_| ())
}
pub(crate) fn grid_controls(app: &mut RustGisApp, ui: &mut egui::Ui) {
    sync(app);
    let mut image = app.annotation.image;
    let mut picked = false;
    let live = image.and_then(|id| app.session.map().layer(id)).filter(|l| l.raster.is_some());
    let available = live.is_some();
    let name = if let Some(layer) = live {
        layer.name.clone()
    } else if image.is_some() {
        app.annotation
            .image_name
            .as_deref()
            .map_or_else(|| tl!("Unavailable image").to_string(), |name| crate::i18n::fmt("Unavailable image: {name}", &[("name", name)]))
    } else {
        tl!("No image").to_string()
    };
    egui::ComboBox::from_label(tl!("Image")).selected_text(name).show_ui(ui, |ui| {
        picked |= ui.selectable_value(&mut image, None, tl!("No image")).clicked();
        for l in app.session.map().layers.iter().filter(|l| l.raster.is_some()) {
            picked |= ui.selectable_value(&mut image, Some(l.id), &l.name).clicked();
        }
    });
    if picked {
        let _ = app.run("ui.annotationImage", json!({"image":image}));
    }
    let mut pixels = app.annotation.pixels;
    egui::ComboBox::new("annotation-construction-grid", tl!("Grid"))
        .selected_text(if pixels { tl!("Original image pixels") } else { tl!("Web Mercator grid metres") })
        .show_ui(ui, |ui| {
            ui.add_enabled_ui(available, |ui| ui.selectable_value(&mut pixels, true, tl!("Original image pixels")));
            ui.selectable_value(&mut pixels, false, tl!("Web Mercator grid metres"));
        });
    if pixels != app.annotation.pixels {
        let _ = app.run("ui.annotationGrid", json!({"pixels":pixels}));
    }
    if app.annotation.pixels {
        ui.small(tl!("New shapes use the original GeoTIFF pixels, including rotation and shear. Existing objects keep their own grid."));
        if let Some(f) = &app.annotation.frame {
            ui.small(crate::i18n::fmt(
                "{width} × {height} pixels · EPSG:{epsg}",
                &[("width", &f.width.to_string()), ("height", &f.height.to_string()), ("epsg", &f.epsg.to_string())],
            ));
        }
        if app.annotation.pending.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(tl!("Reading original image grid…"));
            });
            if ui.button(tl!("Cancel Grid Read")).clicked() {
                let _ = app.run("ui.annotationCancel", Value::Null);
            }
        } else if ui.add_enabled(available, egui::Button::new(tl!("Reload Grid"))).clicked() {
            let _ = app.run("ui.annotationImage", json!({"image":image}));
        }
    } else {
        ui.small(tl!("Web Mercator grid metres, not ground metres. Image pixels are the default when an image is selected."));
    }
    if image.is_some() && !available {
        ui.colored_label(egui::Color32::RED, tl!("This image is unavailable. Choose another image or No image."));
    } else if let Some(error) = &app.annotation.error {
        ui.colored_label(egui::Color32::RED, error);
    }
}
pub(crate) fn panel(app: &mut RustGisApp, ui: &mut egui::Ui) {
    ui.heading(tl!("Image annotation"));
    grid_controls(app, ui);
    ui.separator();
    let active = app.session.active_layer.filter(|id| polygon_layer(app, *id));
    let mut layer = active;
    let name =
        layer.and_then(|id| app.session.map().layer(id)).map_or_else(|| tl!("Choose a polygon layer").to_string(), |l| l.name.clone());
    egui::ComboBox::from_label(tl!("Target layer")).selected_text(name).show_ui(ui, |ui| {
        for l in app.session.map().layers.iter().filter(|l| is_polygon_layer(l)) {
            ui.selectable_value(&mut layer, Some(l.id), &l.name);
        }
    });
    if layer != active
        && let Some(id) = layer
    {
        let _ = app.run("ui.annotationLayer", json!({"layer":id}));
    }
    if layer.is_none() {
        ui.label(tl!("Add or create a polygon feature layer using Data, then choose it here."));
    }
    let mut enabled = app.session.map().editing.settings.enabled;
    if ui.checkbox(&mut enabled, tl!("Enable editing")).changed() {
        let _ = app.run("edit.enable", json!({"on":enabled}));
    }
    let ready = crate::boxedit::ready(app) && drawing_frame(app).is_ok();
    ui.add_enabled_ui(ready, |ui| {
        ui.horizontal(|ui| {
            for (tool, name) in [(Tool::RotatedBox, tl!("Rotated Box")), (Tool::Circle, tl!("Circle"))] {
                if ui.selectable_label(app.session.tool == tool, name).clicked() {
                    let _ = app.run("tool.set", json!({"tool":tool.code()}));
                }
            }
        });
    });
    if !crate::boxedit::ready(app) {
        let hidden = app.session.active_layer.and_then(|id| app.session.map().layer(id)).is_some_and(|l| {
            rustgis_engine::box_edit::check_tool(&app.session, l.id).is_ok() && !app.session.map().draws_at(l, app.session.scale())
        });
        if hidden {
            ui.small(tl!("The polygon layer doesn't draw at this scale. Zoom in or out until it draws, then draw."));
        } else {
            ui.small(tl!("Drawing requires editing enabled, a loaded polygon layer and a flat Earth map with zero pitch."));
        }
    }
    ui.small(tl!("Space or middle-drag pans; releasing Space returns to the active drawing tool. Undo/Redo use the existing commands."));
    if matches!(app.session.tool, Tool::Circle | Tool::RotatedBox) {
        let details =
            egui::CollapsingHeader::new(tl!("Shape details")).id_salt("annotation_shape_details").default_open(false).show(ui, |ui| {
                crate::boxedit::panel(app, ui);
                crate::circleedit::panel(app, ui);
            });
        if details.body_response.is_none()
            && let Some(error) = if app.session.tool == Tool::Circle { crate::circleedit::error(app) } else { crate::boxedit::error(app) }
        {
            ui.colored_label(egui::Color32::RED, error);
        }
    }
    ui.separator();
    let mut image_filename = app.annotation.image_filename;
    if ui
        .checkbox(&mut image_filename, tl!("Use image filename"))
        .on_hover_text(tl!("For img1.tiff, suggest img1.geojson. Choose the location in the save dialog."))
        .changed()
    {
        let _ = app.run("ui.annotationFilename", json!({"on":image_filename}));
    }
    let name = if image_filename { image_export_name(app) } else { None };
    if image_filename {
        if let Some(name) = &name {
            ui.small(crate::i18n::fmt("Suggested filename: {name}", &[("name", name)]));
        } else {
            ui.small(tl!("Select an image with an available filename, or uncheck Use image filename."));
        }
    }
    ui.add_enabled_ui(layer.is_some() && (!image_filename || name.is_some()), |ui| {
        if ui.button(tl!("Export GeoJSON…")).clicked() {
            let _ = app.run("ui.annotationExport", json!({"format":"geojson"}));
        }
    });
    ui.small(tl!(
        "Exports contain ordinary polygons and feature properties. Image/grid choices and exact constructions remain session-only."
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_engine::data::raster::{ColorHint, Crs, GeoTransform, Raster};
    use rustgis_engine::{
        Project, Session,
        data::{Dataset, GeometryType},
        map::RasterLayer,
    };

    // A minimal uncompressed TIFF with the original ModelTransformation, not a north-up
    // display raster. Encoded here so the end-to-end test needs no external image or library.
    fn tiff(frame: &Frame) -> Vec<u8> {
        let matrix_at = 8 + 2 + 11 * 12 + 4;
        let keys_at = matrix_at + 16 * 8;
        let pixels_at = keys_at + 16 * 2;
        let entries: [(u16, u16, u32, u32); 11] = [
            (256, 4, 1, frame.width),
            (257, 4, 1, frame.height),
            (258, 3, 1, 8),
            (259, 3, 1, 1),
            (262, 3, 1, 1),
            (273, 4, 1, pixels_at),
            (277, 3, 1, 1),
            (278, 4, 1, frame.height),
            (279, 4, 1, frame.width * frame.height),
            (34264, 12, 16, matrix_at),
            (34735, 3, 16, keys_at),
        ];
        let mut out = b"II\x2a\x00\x08\x00\x00\x00".to_vec();
        out.extend(11_u16.to_le_bytes());
        for (tag, kind, count, value) in entries {
            out.extend(tag.to_le_bytes());
            out.extend(kind.to_le_bytes());
            out.extend(count.to_le_bytes());
            out.extend(value.to_le_bytes());
        }
        out.extend(0_u32.to_le_bytes());
        let a = frame.affine;
        for v in [a[1], a[2], 0., a[0], a[4], a[5], 0., a[3], 0., 0., 1., 0., 0., 0., 0., 1.] {
            out.extend(v.to_le_bytes());
        }
        for v in [1_u16, 1, 0, 3, 1024, 0, 1, 1, 1025, 0, 1, 1, 3072, 0, 1, frame.epsg as u16] {
            out.extend(v.to_le_bytes());
        }
        out.resize(pixels_at as usize + (frame.width * frame.height) as usize, 100);
        out
    }
    fn app() -> (RustGisApp, u32, u32, Frame) {
        let frame = Frame { epsg: 3857, width: 128, height: 128, affine: [0., 2., 0.4, 2000., 0.2, -3.] };
        let display = Raster::new(
            "overview",
            1,
            1,
            vec![vec![100.]],
            GeoTransform { origin_x: 0., origin_y: 2000., pixel_width: 1000., pixel_height: -1000. },
            Crs::from_epsg(3857).unwrap(),
            None,
            ColorHint::Gray,
            "UInt8",
        )
        .unwrap();
        let mut s = Session::new(Project::default());
        let image = s.map_mut().add_raster("Test TIFF", RasterLayer::from_bytes(display, "test.tif", tiff(&frame)));
        let layer = s.map_mut().add_dataset(Dataset::new("Objects", GeometryType::Polygon, vec![]));
        s.active_layer = Some(layer);
        s.map_mut().camera.center = frame.to_lonlat([64., 64.]).unwrap();
        s.map_mut().camera.res = 1.;
        s.run("edit.enable", &json!({"on":true})).unwrap();
        (RustGisApp::new(s), image, layer, frame)
    }
    fn screen(app: &RustGisApp, frame: &Frame, point: [f64; 2]) -> egui::Pos2 {
        crate::mapview::lonlat_to_screen(app, rect(), frame.to_lonlat(point).unwrap()).unwrap()
    }
    fn rect() -> egui::Rect {
        egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000., 800.))
    }
    fn drag(app: &mut RustGisApp, frame: &Frame, start: [f64; 2], end: [f64; 2], new: bool) {
        let start = screen(app, frame, start);
        let end = screen(app, frame, end);
        if app.session.tool == Tool::Circle {
            assert!(crate::circleedit::drag_start(app, rect(), start, new));
            crate::circleedit::drag_to(app, rect(), end);
            crate::circleedit::drag_stop(app);
        } else {
            assert!(crate::boxedit::drag_start(app, rect(), start, new));
            crate::boxedit::drag_to(app, rect(), end, false);
            crate::boxedit::drag_stop(app);
        }
    }
    #[test]
    fn annotation_grid_defaults_to_original_header_and_explicit_mercator_is_respected() {
        let (mut app, image, layer, frame) = app();
        app.run("ui.annotationImage", json!({"image":image})).unwrap();
        assert_eq!(drawing_frame(&app).unwrap(), frame);
        assert_eq!(app.session.active_layer, Some(layer));
        app.run("ui.annotationGrid", json!({"pixels":false})).unwrap();
        assert_eq!(drawing_frame(&app).unwrap(), Frame::default());
        app.run("ui.annotationImage", json!({"image":image})).unwrap();
        assert_eq!(drawing_frame(&app).unwrap(), frame);
        app.run("ui.annotationImage", json!({"image":null})).unwrap();
        assert_eq!(drawing_frame(&app).unwrap(), Frame::default());
        app.run("layer.select", json!({"layer":image})).unwrap();
        open(&mut app).unwrap();
        assert!(app.annotation.image.is_none(), "reopening the pane must respect an explicit No image choice");
    }
    #[test]
    fn annotation_grid_drags_in_rotated_sheared_pixels_and_edits_keep_their_frame() {
        for tool in [Tool::Circle, Tool::RotatedBox] {
            let (mut app, image, layer, frame) = app();
            app.run("ui.annotationImage", json!({"image":image})).unwrap();
            app.run("tool.set", json!({"tool":tool.code()})).unwrap();
            let depth = app.session.undo_depth();
            drag(&mut app, &frame, [20., 20.], [80., 60.], true);
            assert_eq!(app.session.undo_depth(), depth + 1);
            if tool == Tool::Circle {
                let c = app.session.circle_spec(layer, 0).unwrap();
                assert_eq!(c.frame, frame);
                assert!((c.center[0] - 20.).abs() < 0.01 && (c.radius - 60_f64.hypot(40.)).abs() < 0.01);
            } else {
                let b = app.session.box_spec(layer, 0).unwrap();
                assert_eq!(b.frame, frame);
                assert!((b.width - 60.).abs() < 0.01 && (b.height - 40.).abs() < 0.01 && (b.center[0] - 50.).abs() < 0.01);
            }
            app.run("edit.undo", json!({})).unwrap();
            assert!(app.session.map().layer(layer).unwrap().dataset.features.is_empty());
            assert_eq!(drawing_frame(&app).unwrap(), frame);
            app.run("edit.redo", json!({})).unwrap();
            assert_eq!(drawing_frame(&app).unwrap(), frame);
            app.run("ui.annotationGrid", json!({"pixels":false})).unwrap();
            if tool == Tool::Circle {
                let c = app.session.circle_spec(layer, 0).unwrap().clone();
                drag(&mut app, &frame, [c.center[0] + c.radius, c.center[1]], [c.center[0] + 100., c.center[1]], false);
                let edited = app.session.circle_spec(layer, 0).unwrap();
                assert_eq!(edited.frame, frame);
                assert!((edited.radius - 100.).abs() < 0.01);
            } else {
                drag(&mut app, &frame, [80., 60.], [90., 70.], false);
                let edited = app.session.box_spec(layer, 0).unwrap();
                assert_eq!(edited.frame, frame);
                assert!((edited.width - 70.).abs() < 0.01 && (edited.height - 50.).abs() < 0.01);
            }
        }
    }
    #[test]
    fn annotation_grid_failures_and_replaced_sources_never_fall_back_silently() {
        let (mut app, image, _, _) = app();
        app.run("ui.annotationImage", json!({"image":image})).unwrap();
        app.session.map_mut().layer_mut(image).unwrap().raster.as_mut().unwrap().embedded = Some(std::sync::Arc::new(vec![0; 16]));
        sync(&mut app);
        assert!(app.annotation.source.is_none());
        assert!(drawing_frame(&app).is_err());
        app.run("ui.annotationImage", json!({"image":image})).unwrap();
        assert!(app.annotation.error.is_some() && drawing_frame(&app).is_err());
        app.run("tool.set", json!({"tool":"circle"})).unwrap();
        assert!(!crate::circleedit::drag_start(&mut app, rect(), egui::pos2(400., 350.), true));
        app.run("ui.annotationGrid", json!({"pixels":false})).unwrap();
        assert_eq!(drawing_frame(&app).unwrap(), Frame::default());
    }
    #[test]
    fn annotation_grid_changed_linked_files_block_drawing_until_the_display_is_reloaded() {
        let (mut app, image, layer, _) = app();
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("image.tif");
        let bytes = app.session.map().layer(image).unwrap().raster.as_ref().unwrap().embedded.clone().unwrap();
        std::fs::write(&path, bytes.as_slice()).unwrap();
        let pixels = rustgis_engine::data::raster::read_path(&path).unwrap();
        app.session.map_mut().layer_mut(image).unwrap().raster = Some(RasterLayer::from_path(pixels, path.display().to_string()));
        let replacement = Frame { width: 128, height: 128, affine: [1000., 2., 0.4, 4000., 0.2, -3.], ..Frame::default() };
        let original_modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::fs::write(&path, tiff(&replacement)).unwrap();
        // A coarse filesystem clock could keep the same time: make the revision differ anyway.
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(original_modified + std::time::Duration::from_secs(1)))
            .unwrap();
        let depth = app.session.undo_depth();
        app.run("ui.annotationImage", json!({"image":image})).unwrap();
        assert!(app.annotation.error.as_deref().unwrap().contains("Reload Data"));
        assert!(drawing_frame(&app).is_err());
        for tool in ["circle", "rotatedBox"] {
            app.run("tool.set", json!({"tool":tool})).unwrap();
            let started = if tool == "circle" {
                crate::circleedit::drag_start(&mut app, rect(), egui::pos2(400., 350.), true)
            } else {
                crate::boxedit::drag_start(&mut app, rect(), egui::pos2(400., 350.), true)
            };
            assert!(!started);
        }
        assert!(app.session.map().layer(layer).unwrap().dataset.features.is_empty());
        assert_eq!(app.session.undo_depth(), depth);
        app.run("layer.reloadData", json!({"layer":image})).unwrap();
        app.run("ui.annotationImage", json!({"image":image})).unwrap();
        assert_eq!(drawing_frame(&app).unwrap(), replacement);
        app.session.map_mut().camera.center = replacement.to_lonlat([64., 64.]).unwrap();
        app.run("tool.set", json!({"tool":"circle"})).unwrap();
        drag(&mut app, &replacement, [20., 20.], [25., 20.], true);
        assert_eq!(app.session.map().layer(layer).unwrap().dataset.features.len(), 1);
    }

    #[test]
    fn annotation_grid_source_changes_during_a_drag_cannot_commit_in_the_old_grid() {
        for tool in [Tool::Circle, Tool::RotatedBox] {
            let (mut app, image, layer, frame) = app();
            app.run("ui.annotationImage", json!({"image":image})).unwrap();
            app.run("tool.set", json!({"tool":tool.code()})).unwrap();
            let start = screen(&app, &frame, [20., 20.]);
            let end = screen(&app, &frame, [80., 60.]);
            let depth = app.session.undo_depth();
            if tool == Tool::Circle {
                assert!(crate::circleedit::drag_start(&mut app, rect(), start, true));
                crate::circleedit::drag_to(&mut app, rect(), end);
            } else {
                assert!(crate::boxedit::drag_start(&mut app, rect(), start, true));
                crate::boxedit::drag_to(&mut app, rect(), end, false);
            }
            let mark = app.session.notice_mark();
            // Also protects a source replacement received directly through MCP before UI sync.
            app.session.map_mut().layer_mut(image).unwrap().raster.as_mut().unwrap().embedded = Some(std::sync::Arc::new(vec![0; 32]));
            if tool == Tool::Circle {
                crate::circleedit::drag_stop(&mut app);
            } else {
                crate::boxedit::drag_stop(&mut app);
            }
            sync(&mut app);
            crate::boxedit::cancel_stale(&mut app);
            crate::circleedit::cancel_stale(&mut app);
            assert_eq!(app.session.notices_since(mark).iter().filter(|n| n.text.starts_with("Drawing cancelled")).count(), 1);
            assert!(app.session.map().layer(layer).unwrap().dataset.features.is_empty());
            assert_eq!(app.session.undo_depth(), depth);
        }
    }
    #[test]
    fn annotation_grid_changes_report_cancelled_new_drags_once_without_committing() {
        for tool in [Tool::Circle, Tool::RotatedBox] {
            for change in ["grid", "image", "reload", "map", "project"] {
                let (mut app, image, _, frame) = app();
                let folder = tempfile::tempdir().unwrap();
                if change == "reload" {
                    // Reload Data applies to linked local files; embedded snapshots are re-added.
                    let path = folder.path().join("image.tif");
                    std::fs::write(&path, tiff(&frame)).unwrap();
                    let pixels = rustgis_engine::data::raster::read_path(&path).unwrap();
                    app.session.map_mut().layer_mut(image).unwrap().raster =
                        Some(RasterLayer::from_path(pixels, path.display().to_string()));
                }
                app.run("ui.annotationImage", json!({"image":image})).unwrap();
                app.run("tool.set", json!({"tool":tool.code()})).unwrap();
                let start = screen(&app, &frame, [20., 20.]);
                let end = screen(&app, &frame, [60., 60.]);
                if tool == Tool::Circle {
                    assert!(crate::circleedit::drag_start(&mut app, rect(), start, true));
                    crate::circleedit::drag_to(&mut app, rect(), end);
                } else {
                    assert!(crate::boxedit::drag_start(&mut app, rect(), start, true));
                    crate::boxedit::drag_to(&mut app, rect(), end, false);
                }
                let mark = app.session.notice_mark();
                match change {
                    "grid" => app.run("ui.annotationGrid", json!({"pixels":false})),
                    "image" => app.run("ui.annotationImage", json!({"image":image})),
                    "reload" => app.run("layer.reloadData", json!({"layer":image})),
                    "map" => app.run("map.new", json!({"name":"Other"})),
                    _ => app.run("project.new", json!({})),
                }
                .unwrap();
                assert!(!crate::boxedit::drawing(&app) && !crate::circleedit::drawing(&app));
                let depth = app.session.undo_depth();
                crate::boxedit::drag_stop(&mut app);
                crate::circleedit::drag_stop(&mut app);
                sync(&mut app);
                assert_eq!(
                    app.session.notices_since(mark).iter().filter(|n| n.text.starts_with("Drawing cancelled")).count(),
                    1,
                    "{tool:?}: {change}"
                );
                assert_eq!(app.session.undo_depth(), depth, "release must not commit the abandoned preview");
                assert!(app.session.project.maps.iter().flat_map(|map| &map.layers).all(|layer| layer.dataset.features.is_empty()));
            }
        }
    }

    #[test]
    fn annotation_grid_replaced_requests_ignore_old_failures_and_cancellation() {
        let (mut app, image, _, frame) = app();
        app.reads_off_frame = true;
        app.run("ui.annotationImage", json!({"image":image})).unwrap();
        let old = app.annotation.pending.unwrap();
        app.run("ui.annotationImage", json!({"image":image})).unwrap();
        let current = app.annotation.pending.unwrap();
        assert_ne!(old, current);
        for r in [
            Landed::Ran(Err("old failure".into())),
            Landed::NotRun("old cancel".into()),
            Landed::Ran(Ok(serde_json::to_value(&frame).unwrap())),
        ] {
            landed(&mut app, old, r);
            assert_eq!(app.annotation.pending, Some(current));
            assert!(app.annotation.error.is_none());
        }
        app.run("ui.annotationCancel", Value::Null).unwrap();
        landed(&mut app, current, Landed::Ran(Ok(serde_json::to_value(&frame).unwrap())));
        assert!(drawing_frame(&app).is_err());
        app.run("project.new", json!({})).unwrap();
        assert!(app.annotation.image.is_none());
        assert!(drawing_frame(&app).is_err());
    }
    #[test]
    fn annotation_grid_stale_pixel_context_never_substitutes_map_metres() {
        for project_changed in [false, true] {
            let (mut app, image, _, frame) = app();
            app.run("ui.annotationImage", json!({"image":image})).unwrap();
            let context = app.annotation.context.unwrap();
            // Model a stale binding before the next UI synchronization, while the source is live.
            app.annotation.context =
                Some(if project_changed { (context.0.wrapping_add(1), context.1) } else { (context.0, context.1.wrapping_add(1)) });
            assert!(app.annotation.source.as_ref().unwrap().current(&app.session));
            assert!(drawing_frame(&app).unwrap_err().contains("context changed"));
            assert!(app.annotation.pixels);
            assert_eq!(app.annotation.frame.as_ref(), Some(&frame));
            app.annotation.pixels = false;
            assert_eq!(drawing_frame(&app).unwrap(), Frame::default(), "explicit map units remain valid");
            app.annotation.pixels = true;
            app.annotation.context = Some(context);
            assert_eq!(drawing_frame(&app).unwrap(), frame);
        }
    }

    #[test]
    fn annotation_grid_real_context_switch_preserves_pixels_and_requires_explicit_recovery() {
        for command in ["project.new", "map.new"] {
            for pixels in [false, true] {
                let (mut app, image, _, _) = app();
                app.run("ui.annotationImage", json!({"image":image})).unwrap();
                app.run("ui.annotationGrid", json!({"pixels":pixels})).unwrap();
                let old = app.annotation.context;
                let next = app.annotation.next;
                let old_request = next;
                let mark = app.session.notice_mark();
                app.run(command, json!({"name":"Other"})).unwrap();
                app.logic(&egui::Context::default());
                assert_ne!(app.annotation.context, old);
                assert_eq!(app.annotation.pixels, pixels);
                assert!(app.annotation.image.is_none() && app.annotation.source.is_none() && app.annotation.frame.is_none());
                assert!(app.annotation.pending.is_none());
                assert_eq!(app.annotation.next, next);
                if pixels {
                    assert!(drawing_frame(&app).unwrap_err().contains("Choose an image"));
                    assert!(app.annotation.error.as_ref().unwrap().contains("project or map changed"));
                    assert_eq!(app.session.notices_since(mark).iter().filter(|n| n.text.contains("project or map changed")).count(), 1);
                    landed(&mut app, old_request, Landed::Ran(Ok(serde_json::to_value(Frame::default()).unwrap())));
                    assert!(drawing_frame(&app).is_err(), "late completions must not repair another context");
                    open(&mut app).unwrap();
                    assert!(drawing_frame(&app).is_err(), "opening the pane must not clear the refusal");
                    app.run("ui.annotationGrid", json!({"pixels":false})).unwrap();
                    assert_eq!(drawing_frame(&app).unwrap(), Frame::default());
                    let original = self::app();
                    let raster = original.0.session.map().layer(original.1).unwrap().raster.clone().unwrap();
                    let image = app.session.map_mut().add_raster("New image", raster);
                    app.run("ui.annotationImage", json!({"image":image})).unwrap();
                    assert_eq!(drawing_frame(&app).unwrap(), original.3);
                } else {
                    assert_eq!(drawing_frame(&app).unwrap(), Frame::default());
                    assert!(app.annotation.error.is_none());
                    assert!(!app.session.notices_since(mark).iter().any(|n| n.text.contains("project or map changed")));
                }
            }
        }
    }

    #[test]
    fn annotation_grid_open_keeps_automatic_selection_errors_inline() {
        use egui_kittest::kittest::Queryable;
        let (mut app, image, _, _) = app();
        sync(&mut app);
        app.annotation.next = u64::MAX;
        app.run("layer.select", json!({"layer":image})).unwrap();
        app.run("ui.pane.annotation", Value::Null).unwrap();
        assert_eq!(app.ui.right, Some(RightPane::Annotation));
        assert_eq!(app.annotation.error.as_deref(), Some("Image-grid request limit reached"));
        assert!(app.annotation.image_chosen);
        let mut h = egui_kittest::Harness::builder().build_ui_state(|ui, app: &mut RustGisApp| panel(app, ui), app);
        assert!(h.query_by_label("Image-grid request limit reached").is_some());
        h.state_mut().run("ui.pane.annotation", Value::Null).unwrap();
        h.state_mut().annotation.error = None;
        h.state_mut().run("ui.pane.annotation", Value::Null).unwrap();
        assert!(h.state().annotation.error.is_none(), "reopening must not retry a failed automatic choice");
        assert!(h.state_mut().run("ui.annotationImage", json!({"image":image})).is_err(), "explicit selection still reports errors");
    }

    #[test]
    fn annotation_grid_stale_or_cancelled_image_keeps_its_selected_name() {
        use egui_kittest::kittest::Queryable;
        for replace in [false, true] {
            let (mut app, image, _, _) = app();
            app.reads_off_frame = !replace;
            app.run("ui.annotationImage", json!({"image":image})).unwrap();
            if replace {
                app.session.map_mut().layer_mut(image).unwrap().raster.as_mut().unwrap().embedded = Some(std::sync::Arc::new(vec![0; 16]));
            } else {
                app.run("ui.annotationCancel", Value::Null).unwrap();
            }
            let h = egui_kittest::Harness::builder().build_ui_state(|ui, app: &mut RustGisApp| grid_controls(app, ui), app);
            assert!(h.query_all_by_role(egui::accesskit::Role::ComboBox).any(|n| n.value().as_deref() == Some("Test TIFF")));
            assert_eq!(h.state().annotation.image, Some(image));
            assert!(h.state().annotation.error.is_some());
            assert!(drawing_frame(h.state()).is_err());
            assert!(h.query_by_label("Reload Grid").is_some());
        }
    }

    #[test]
    fn annotation_grid_removed_image_keeps_its_name_blocks_reload_and_has_explicit_recovery() {
        use egui_kittest::kittest::{NodeT, Queryable};
        for recovery in ["undo", "another", "none", "metres"] {
            let (mut app, image, _, frame) = app();
            let raster = app.session.map().layer(image).unwrap().raster.clone().unwrap();
            app.run("ui.annotationImage", json!({"image":image})).unwrap();
            app.run("layer.rename", json!({"layer":image,"name":"Survey image"})).unwrap();
            app.run("layer.remove", json!({"layer":image})).unwrap();
            let mut h = egui_kittest::Harness::builder().build_ui_state(|ui, app: &mut RustGisApp| grid_controls(app, ui), app);
            assert!(
                h.query_all_by_role(egui::accesskit::Role::ComboBox)
                    .any(|n| n.value().as_deref() == Some("Unavailable image: Survey image"))
            );
            assert!(h.get_by_label("Reload Grid").accesskit_node().is_disabled());
            assert!(h.query_by_label("This image is unavailable. Choose another image or No image.").is_some());
            assert!(h.state().annotation.pixels);
            assert!(drawing_frame(h.state()).is_err());
            match recovery {
                "undo" => {
                    h.state_mut().run("edit.undo", json!({})).unwrap();
                    h.run();
                    assert!(!h.get_by_label("Reload Grid").accesskit_node().is_disabled());
                    h.get_by_label("Reload Grid").click();
                    h.run();
                    assert_eq!(drawing_frame(h.state()).unwrap(), frame);
                }
                "another" => {
                    let next = h.state_mut().session.map_mut().add_raster("Other image", raster);
                    h.state_mut().run("ui.annotationImage", json!({"image":next})).unwrap();
                    h.run();
                    assert_eq!(h.state().annotation.image, Some(next));
                    assert_eq!(drawing_frame(h.state()).unwrap(), frame);
                }
                "none" => {
                    h.state_mut().run("ui.annotationImage", json!({"image":null})).unwrap();
                    h.run();
                    assert!(h.state().annotation.image_name.is_none());
                    assert!(h.query_all_by_role(egui::accesskit::Role::ComboBox).any(|n| n.value().as_deref() == Some("No image")));
                    assert_eq!(drawing_frame(h.state()).unwrap(), Frame::default());
                }
                _ => {
                    h.state_mut().run("ui.annotationGrid", json!({"pixels":false})).unwrap();
                    h.run();
                    assert!(!h.state().annotation.pixels);
                    assert_eq!(drawing_frame(h.state()).unwrap(), Frame::default());
                }
            }
        }
    }

    #[test]
    fn annotation_grid_search_queries_the_grid_without_toggling_the_pane() {
        for queued in [false, true] {
            let (mut app, image, _, _) = app();
            app.run("layer.select", json!({"layer":image})).unwrap();
            open(&mut app).unwrap();
            app.reads_off_frame = queued;
            app.ui.command_search = Some("raster.imageGrid".into());
            let mut h = egui_kittest::Harness::builder()
                .with_size(egui::vec2(1366., 900.))
                .build_ui_state(|ui, app: &mut RustGisApp| crate::chrome::command_search(app, ui.ctx()), app);
            h.key_press(egui::Key::Enter);
            h.run();
            let end = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while crate::readjob::land(h.state_mut()) {
                assert!(std::time::Instant::now() < end, "the grid query did not land");
                std::thread::sleep(std::time::Duration::from_millis(5)); // test-wait: await the background grid result before checking its user-facing summary
            }
            assert!(h.state().ui.command_search.is_none());
            assert_eq!(h.state().ui.right, Some(RightPane::Annotation));
            let notices = h.state().session.notices_since(0);
            assert!(notices.iter().all(|n| !n.error));
            assert_eq!(notices.iter().filter(|n| n.text == "Original image grid: 128 × 128 pixels, EPSG:3857").count(), 1);
        }
    }

    #[test]
    fn annotation_grid_pane_entry_defaults_and_layer_selection_share_commands() {
        use egui_kittest::kittest::Queryable;
        let (mut app, image, layer, _) = app();
        app.run("layer.select", json!({"layer":image})).unwrap();
        app.run("ui.tab", json!({"tab":"Analyze"})).unwrap();
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1366., 900.)).with_max_steps(1000).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app,
        );
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Annotate").click();
        h.run();
        assert_eq!(h.state().ui.right, Some(RightPane::Annotation));
        assert!(h.query_all_by_role(egui::accesskit::Role::ComboBox).any(|n| n.value().as_deref() == Some("Original image pixels")));
        h.get_by_label("Target layer").click();
        h.run();
        h.query_all_by_role_and_label(egui::accesskit::Role::Button, "Objects").find(|n| n.rect().min.x > 800.).unwrap().click();
        h.run();
        assert_eq!(h.state().session.active_layer, Some(layer));
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Circle").click();
        h.run();
        assert_eq!(h.state().session.tool, Tool::Circle);
    }
    #[test]
    fn annotation_grid_refused_gestures_notify_with_shape_details_collapsed() {
        use egui_kittest::kittest::Queryable;
        for tool in [Tool::Circle, Tool::RotatedBox] {
            let (mut app, image, layer, frame) = app();
            app.run("ui.annotationImage", json!({"image":image})).unwrap();
            app.run("ui.pane.annotation", Value::Null).unwrap();
            app.run("tool.set", json!({"tool":tool.code()})).unwrap();
            // A pending lookup has no frame yet; its failure must be visible outside details.
            app.annotation.frame = None;
            app.annotation.error = None;
            app.annotation.pending = Some(app.annotation.next);
            let depth = app.session.undo_depth();
            let mark = app.session.notice_mark();
            let origin = screen(&app, &frame, [32., 32.]);
            for _ in 0..3 {
                let accepted = if tool == Tool::Circle {
                    crate::circleedit::drag_start(&mut app, rect(), origin, true)
                } else {
                    crate::boxedit::drag_start(&mut app, rect(), origin, true)
                };
                assert!(!accepted);
            }
            let error = "Wait for the original image grid before drawing.";
            assert_eq!(app.session.notices_since(mark).iter().filter(|n| n.error && n.text == error).count(), 1);
            assert_eq!(app.session.undo_depth(), depth);
            assert!(app.session.map().layer(layer).unwrap().dataset.features.is_empty());
            // Render the pane without the toast/status bar to prove the inline reason persists.
            let mut h = egui_kittest::Harness::builder()
                .with_size(egui::vec2(600., 1000.))
                .with_max_steps(1000)
                .build_ui_state(|ui, app| panel(app, ui), app);
            // Pending reads keep the spinner repainting; inspect a frame rather than await idle.
            h.run_steps(1);
            assert!(h.query_by_label("Shape details").is_some());
            assert!(h.query_by_label("Radius").is_none() && h.query_by_label("Width").is_none());
            assert!(h.query_all_by_label(error).next().is_some(), "refusal is visible without expanding details");
            let request = h.state().annotation.pending.unwrap();
            landed(h.state_mut(), request, Landed::Ran(Ok(serde_json::to_value(&frame).unwrap())));
            h.run_steps(1);
            assert!(h.query_all_by_label(error).next().is_none(), "a completed grid clears the stale waiting reason");
            let app = h.state_mut();
            let mark = app.session.notice_mark();
            drag(app, &frame, [32., 32.], [64., 64.], true);
            assert_eq!(app.session.map().layer(layer).unwrap().dataset.features.len(), 1);
            assert!(!app.session.notices_since(mark).iter().any(|n| n.error), "valid recovery must not produce a refusal");
        }
    }

    #[test]
    fn annotation_shape_details_start_hidden_and_toggle_without_changing_the_object() {
        use egui_kittest::kittest::Queryable;
        for tool in [Tool::Circle, Tool::RotatedBox] {
            let (mut app, image, layer, frame) = app();
            app.run("ui.annotationImage", json!({"image":image})).unwrap();
            app.run("tool.set", json!({"tool":tool.code()})).unwrap();
            drag(&mut app, &frame, [20., 20.], [80., 60.], true);
            let feature = app.session.map().layer(layer).unwrap().dataset.features[0].clone();
            let depth = app.session.undo_depth();
            let mut h = egui_kittest::Harness::builder()
                .with_size(egui::vec2(600., 1400.))
                .with_max_steps(1000)
                .build_ui_state(|ui, app| panel(app, ui), app);
            let dimension = if tool == Tool::Circle { "Radius" } else { "Width" };
            assert!(h.query_by_label(dimension).is_none());
            h.get_by_label("Shape details").click();
            h.run();
            assert!(h.query_by_label(dimension).is_some());
            h.get_by_label("Shape details").click();
            h.run();
            assert!(h.query_by_label(dimension).is_none());
            assert!(h.query_by_label("Export GeoJSON…").is_some());
            assert_eq!(h.state().session.tool, tool);
            assert_eq!(h.state().session.undo_depth(), depth);
            assert_eq!(h.state().session.map().layer(layer).unwrap().dataset.features[0], feature);
            assert!(if tool == Tool::Circle {
                h.state().session.circle_spec(layer, 0).is_some()
            } else {
                h.state().session.box_spec(layer, 0).is_some()
            });
        }
    }

    #[test]
    fn annotation_filename_uses_source_basename_instead_of_layer_title() {
        for (file, expected, local) in [
            ("img1.tiff", "img1.geojson", false),
            ("folder/scene.v2.TIFF", "scene.v2.geojson", false),
            ("C:\\images\\été 1.tif", "été 1.geojson", false),
            ("/images/flight.v2.tiff", "flight.v2.geojson", true),
        ] {
            let (mut app, image, layer, _) = app();
            let raster = app.session.map_mut().layer_mut(image).unwrap().raster.as_mut().unwrap();
            raster.file_name = (!local).then(|| file.into());
            raster.path = local.then(|| file.into());
            app.run("ui.annotationImage", json!({"image":image})).unwrap();
            app.run("layer.rename", json!({"layer":image,"name":"Renamed image title"})).unwrap();
            assert_eq!(export_name(&app, layer, "geojson", true).unwrap(), "Objects.geojson");
            app.run("ui.annotationFilename", json!({"on":true})).unwrap();
            assert_eq!(export_name(&app, layer, "geojson", true).unwrap(), expected);
            assert_eq!(export_name(&app, layer, "geojson", false).unwrap(), "Objects.geojson");
        }
    }
    #[test]
    fn annotation_filename_checkbox_controls_real_geojson_download_without_affecting_other_exports() {
        use egui_kittest::kittest::Queryable;
        let (mut app, image, _, frame) = app();
        app.session.map_mut().layer_mut(image).unwrap().raster.as_mut().unwrap().file_name = Some("img1.tiff".into());
        app.run("ui.annotationImage", json!({"image":image})).unwrap();
        app.run("tool.set", json!({"tool":"circle"})).unwrap();
        drag(&mut app, &frame, [20., 20.], [80., 60.], true);
        let downloads = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let output = downloads.clone();
        app.services.download = Some(Box::new(move |name, bytes| {
            output.lock().unwrap().push((name.to_string(), bytes));
            Ok(())
        }));
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(600., 1400.))
            .with_max_steps(1000)
            .build_ui_state(|ui, app| panel(app, ui), app);
        h.get_by_label("Use image filename").click();
        h.run();
        assert!(h.state().annotation.image_filename);
        assert!(h.query_by_label("Suggested filename: img1.geojson").is_some());
        h.get_by_label("Export GeoJSON…").click();
        h.run();
        let mut app = h.into_state();
        app.run("ui.exportLayer", json!({"format":"geojson"})).unwrap();
        app.run("ui.annotationFilename", json!({"on":false})).unwrap();
        app.run("ui.annotationExport", Value::Null).unwrap();
        let output = downloads.lock().unwrap();
        assert_eq!(
            output.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(),
            ["img1.geojson", "Objects.geojson", "Objects.geojson"]
        );
        let geojson: Value = serde_json::from_slice(&output[0].1).unwrap();
        assert_eq!(geojson["type"], "FeatureCollection");
        assert_eq!(geojson["features"].as_array().unwrap().len(), 1);
    }
    #[test]
    fn annotation_filename_missing_image_does_not_silently_export_under_another_name() {
        let (mut app, image, _, _) = app();
        app.run("ui.annotationFilename", json!({"on":true})).unwrap();
        app.run("ui.annotationImage", json!({"image":image})).unwrap();
        app.run("ui.annotationImage", json!({"image":null})).unwrap();
        let downloads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let output = downloads.clone();
        app.services.download = Some(Box::new(move |_, _| {
            output.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }));
        assert!(app.run("ui.annotationExport", Value::Null).unwrap_err().contains("filename"));
        assert_eq!(downloads.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
}
