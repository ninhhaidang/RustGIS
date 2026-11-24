//! Explicit, cancellable previews of polygon-to-shape replacement, using the shared read queue.
use crate::{RightPane, RustGisApp};
use rustgis_engine::shape_conversion::{Proposal, Shape};
use rustgis_engine::{data::Feature, geom::Coord};
use serde_json::{Value, json};

#[derive(Default)]
pub(crate) struct State {
    menu: Option<MenuTarget>,
    form: Option<Form>,
    next_request: u64,
}
#[derive(Clone, Copy)]
struct MenuTarget {
    layer: u32,
    row: usize,
    map: u64,
    instance: u64,
    revision: u64,
    replacement: u64,
    feature: u64,
}
impl MenuTarget {
    fn current(self, app: &RustGisApp) -> bool {
        app.session.map().instance_id() == self.map
            && app.session.map().layer(self.layer).is_some_and(|l| {
                l.instance_id() == self.instance
                    && l.geometry_rev == self.revision
                    && l.replacement_revision() == self.replacement
                    && l.selection == [self.row]
                    && l.dataset.features.get(self.row).is_some_and(|f| f.id == self.feature)
            })
    }
}
struct Form {
    previous_tool: rustgis_engine::Tool,
    image_stamp: Option<(u64, u64, u64)>,
    request: u64,
    layer: u32,
    row: usize,
    map: u64,
    instance: u64,
    replacement: u64,
    rows: std::sync::Weak<Vec<Feature>>,
    kind: String,
    image: Option<u32>,
    pending: bool,
    apply_after_fit: bool,
    proposal: Option<Proposal>,
    error: Option<String>,
}
fn valid(app: &RustGisApp, f: &Form) -> bool {
    app.session.map().instance_id() == f.map
        && (!f.apply_after_fit || app.session.tool == rustgis_engine::Tool::Explore)
        && app.session.active_layer == Some(f.layer)
        && f.image.is_none_or(|id| {
            app.session.map().layer(id).is_some_and(|l| {
                l.raster.is_some() && f.image_stamp.is_none_or(|stamp| stamp == (l.instance_id(), l.replacement_revision(), l.geometry_rev))
            })
        })
        && rustgis_engine::box_edit::check_tool(&app.session, f.layer).is_ok()
        && app.session.map().layer(f.layer).is_some_and(|l| {
            l.instance_id() == f.instance
                && l.replacement_revision() == f.replacement
                && l.selection == [f.row]
                && l.dataset.features.allocation_key().ptr_eq(&f.rows)
        })
}
pub(crate) fn pick(app: &mut RustGisApp, c: Coord, alt: bool) {
    app.conversion.menu = None;
    if !matches!(
        app.session.tool,
        rustgis_engine::Tool::Explore
            | rustgis_engine::Tool::Select
            | rustgis_engine::Tool::SelectPolygon
            | rustgis_engine::Tool::SelectLasso
            | rustgis_engine::Tool::SelectRadius
            | rustgis_engine::Tool::EditVertices
            | rustgis_engine::Tool::Circle
            | rustgis_engine::Tool::RotatedBox
    ) || !crate::boxedit::ready(app)
    {
        return;
    }
    let hits = crate::boxedit::hits(app, c);
    let current = app.session.active_layer.and_then(|id| {
        let [row] = app.session.map().layer(id)?.selection.as_slice() else { return None };
        hits.iter().position(|h| *h == (id, *row))
    });
    let index = if alt { current.map_or(0, |i| (i + 1) % hits.len().max(1)) } else { current.unwrap_or(0) };
    if let Some(&(layer, row)) = hits.get(index) {
        let _ = app.run("select.rows", json!({"layer":layer,"rows":[row],"mode":"new"}));
        if let Some(l) = app.session.map().layer(layer)
            && let Some(f) = l.dataset.features.get(row)
        {
            app.conversion.menu = Some(MenuTarget {
                layer,
                row,
                map: app.session.map().instance_id(),
                instance: l.instance_id(),
                revision: l.geometry_rev,
                replacement: l.replacement_revision(),
                feature: f.id,
            });
        }
    } else if matches!(app.session.tool, rustgis_engine::Tool::Circle | rustgis_engine::Tool::RotatedBox) {
        let _ = app.run("select.clear", json!({}));
    }
}

pub(crate) fn open(app: &mut RustGisApp, p: &Value) -> Result<(), String> {
    let layer = app.session.layer_id(p)?;
    rustgis_engine::box_edit::check_tool(&app.session, layer)?;
    let l = app.session.map().layer(layer).ok_or("no layer")?;
    let [row] = l.selection.as_slice() else { return Err(tl!("Select one polygon to convert.").into()) };
    let default_kind = match app.session.tool {
        rustgis_engine::Tool::RotatedBox => "rotatedBox",
        _ => "circle",
    };
    let kind = p.get("kind").and_then(Value::as_str).unwrap_or(default_kind);
    if !matches!(kind, "circle" | "rotatedBox") {
        return Err(tl!("Choose Circle or Rotated Box.").into());
    }
    app.conversion.next_request =
        app.conversion.next_request.checked_add(1).ok_or_else(|| tl!("conversion request limit reached").to_string())?;
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(old) = &app.conversion.form {
        app.reads.cancel_shape_conversion(old.request);
    }
    app.conversion.form = Some(Form {
        previous_tool: app.session.tool,
        image_stamp: None,
        request: app.conversion.next_request,
        layer,
        row: *row,
        map: app.session.map().instance_id(),
        instance: l.instance_id(),
        replacement: l.replacement_revision(),
        rows: l.dataset.features.allocation_key(),
        kind: kind.into(),
        image: None,
        pending: false,
        apply_after_fit: false,
        proposal: None,
        error: None,
    });
    let _ = app.run("tool.set", json!({"tool":"explore"}));
    app.map.cancel_sketch();
    app.ui.open_right(RightPane::Editing);
    Ok(())
}
pub(crate) fn menu(app: &mut RustGisApp, ui: &mut egui::Ui) {
    let Some(target) = app.conversion.menu.filter(|t| t.current(app)) else { return };
    let layer = target.layer;
    if ui.button(tl!("Convert Polygon…")).clicked() {
        let _ = app.run("ui.convertShape", json!({"layer":layer}));
        ui.close();
    }
}
pub(crate) fn landed(app: &mut RustGisApp, request: Option<u64>, result: crate::Landed) {
    if app.conversion.form.as_ref().is_none_or(|f| Some(f.request) != request || !f.pending) {
        return;
    }
    let Some(mut f) = app.conversion.form.take() else { return };
    if !valid(app, &f) {
        #[cfg(not(target_arch = "wasm32"))]
        app.reads.cancel_shape_conversion(f.request);
        app.session.notify(tl!("Conversion closed because the source, selection or tool changed."), false);
        return;
    }
    if let crate::Landed::Ran(Ok(ref answer)) = result
        && answer.get("request").and_then(Value::as_u64) != Some(f.request)
    {
        app.conversion.form = Some(f);
        return;
    }
    f.pending = false;
    match result {
        crate::Landed::Ran(Ok(answer)) => match serde_json::from_value::<Proposal>(answer) {
            Ok(p) if p.layer == f.layer && p.row == f.row && p.current(&app.session) => f.proposal = Some(p),
            _ => f.error = Some(tl!("The source changed. Preview the conversion again.").into()),
        },
        crate::Landed::Ran(Err(e)) | crate::Landed::NotRun(e) => f.error = Some(e),
    }
    let apply_after_fit = f.apply_after_fit;
    f.apply_after_fit = false;
    if apply_after_fit && apply_proposal(app, &mut f) {
        return;
    }
    app.conversion.form = Some(f);
}
/// Synchronous, bounded Apply after the shared cancellable fit has landed and passed its source
/// checks. Both the reviewed and direct paths use the same engine transaction and editor.
fn apply_proposal(app: &mut RustGisApp, f: &mut Form) -> bool {
    let Some(p) = &f.proposal else { return false };
    match app.run("edit.applyShapeConversion", json!({"preview":p,"confirm":true})) {
        Ok(_) => {
            let _ = app.run("tool.set", json!({"tool":f.kind}));
            true
        }
        Err(e) => {
            f.error = Some(e);
            false
        }
    }
}
pub(crate) fn panel(app: &mut RustGisApp, ui: &mut egui::Ui) {
    let Some(mut f) = app.conversion.form.take() else {
        if let Some(layer) = app.session.active_layer
            && app.session.map().layer(layer).is_some_and(|l| l.selection.len() == 1)
            && crate::boxedit::ready(app)
            && ui.button(tl!("Convert Selected Polygon…")).clicked()
        {
            let _ = app.run("ui.convertShape", json!({"layer":layer}));
        }
        return;
    };
    if !valid(app, &f) {
        #[cfg(not(target_arch = "wasm32"))]
        app.reads.cancel_shape_conversion(f.request);
        app.session.notify(tl!("Conversion closed because the source, selection or tool changed."), false);
        return;
    }
    ui.heading(tl!("Convert Polygon"));
    ui.small(tl!("Replaces only this polygon. Its ID and attributes are preserved. Apply is undoable."));
    let mut changed = false;
    ui.add_enabled_ui(!f.pending, |ui| {
        egui::ComboBox::from_label(tl!("Shape"))
            .selected_text(if f.kind == "circle" { tl!("Circle") } else { tl!("Rotated Box") })
            .show_ui(ui, |ui| {
                changed |= ui.selectable_value(&mut f.kind, "circle".into(), tl!("Circle")).changed();
                changed |= ui.selectable_value(&mut f.kind, "rotatedBox".into(), tl!("Rotated Box")).changed();
            });
    });
    if changed {
        f.proposal = None;
        f.image_stamp = None;
        f.error = None;
    }
    let reviewed = f.proposal.is_some();
    let can_apply = f.proposal.as_ref().is_some_and(|p| p.shape.geometry().is_ok());
    let convert_label = tl!("Convert Now");
    let apply_label = tl!("Apply Conversion");
    let action_width = [convert_label, apply_label]
        .into_iter()
        .map(|label| {
            ui.painter().layout_no_wrap(label.into(), egui::TextStyle::Button.resolve(ui.style()), ui.visuals().text_color()).size().x
        })
        .fold(0., f32::max)
        + ui.spacing().button_padding.x * 2.;
    let (preview, primary) = ui
        .horizontal(|ui| {
            let primary = ui
                .add_enabled_ui(!f.pending && (!reviewed || can_apply), |ui| {
                    ui.add_sized(
                        [action_width, ui.spacing().interact_size.y],
                        egui::Button::new(if reviewed { apply_label } else { convert_label }),
                    )
                })
                .inner
                .clicked();
            let preview = ui.add_enabled(!f.pending, egui::Button::new(tl!("Preview Conversion"))).clicked();
            (preview, primary)
        })
        .inner;
    let convert = primary && !reviewed;
    let apply = primary && reviewed;
    let close_label = if f.pending { tl!("Cancel Fitting") } else { tl!("Close") };
    if ui
        .button(close_label)
        .on_hover_text(tl!("Close without applying the preview and stop any pending fit. Use Undo after applying a conversion."))
        .clicked()
    {
        #[cfg(not(target_arch = "wasm32"))]
        app.reads.cancel_shape_conversion(f.request);
        if app.session.tool == rustgis_engine::Tool::Explore {
            let _ = app.run("tool.set", json!({"tool":f.previous_tool.code()}));
        }
        return;
    }
    ui.add_enabled_ui(!f.pending, |ui| {
        let title = f
            .image
            .and_then(|id| app.session.map().layer(id))
            .map(|l| l.name.clone())
            .unwrap_or_else(|| crate::annotation::layer_grid_name(app));
        egui::ComboBox::from_label(tl!("Construction Grid")).selected_text(title).show_ui(ui, |ui| {
            changed |= ui.selectable_value(&mut f.image, None, crate::annotation::layer_grid_name(app)).changed();
            for l in app.session.map().layers.iter().filter(|l| l.raster.is_some()) {
                changed |= ui.selectable_value(&mut f.image, Some(l.id), &l.name).changed();
            }
        });
    });
    if changed {
        f.proposal = None;
        f.image_stamp = None;
        f.error = None;
    }
    let shape_layer_units = crate::annotation::grid_units(&crate::annotation::layer_frame(app)).unwrap_or_default();
    ui.small(if f.image.is_some() {
        tl!("Original GeoTIFF pixels, including rotation and shear. Only local or embedded GeoTIFFs are supported.")
    } else {
        &shape_layer_units
    });
    ui.label(if f.kind == "circle" {
        tl!("Fit: centre of the vertex bounds, radius enclosing all vertices. This is not recovery of the original circle.")
    } else {
        tl!("Fit: minimum-area enclosing rectangle of the vertices in the chosen grid.")
    });
    ui.small(tl!("The preview replaces the boundary; curved grid edges remain straight-edge approximations after projection. Saved/exported results are ordinary polygons."));
    if f.pending {
        ui.spinner();
        ui.label(tl!("Fitting in progress. Cancel is also available in the status bar."));
    }
    if f.proposal.is_none() {
        ui.small(tl!("Convert Now fits and applies immediately. Preview Conversion lets you review or adjust first."));
    }
    if let Some(p) = &mut f.proposal {
        let frame = match &p.shape {
            Shape::Circle(c) => &c.frame,
            Shape::RotatedBox(b) => &b.frame,
        };
        ui.label(crate::i18n::fmt("Grid EPSG:{epsg}", &[("epsg", &frame.epsg.to_string())]));
        match &mut p.shape {
            Shape::Circle(c) => {
                number(ui, tl!("Center X"), &mut c.center[0]);
                number(ui, tl!("Center Y"), &mut c.center[1]);
                number(ui, tl!("Radius"), &mut c.radius);
                number(ui, tl!("Tolerance"), &mut c.tolerance);
            }
            Shape::RotatedBox(b) => {
                number(ui, tl!("Center X"), &mut b.center[0]);
                number(ui, tl!("Center Y"), &mut b.center[1]);
                number(ui, tl!("Width"), &mut b.width);
                number(ui, tl!("Height"), &mut b.height);
                number(ui, tl!("Angle"), &mut b.angle);
            }
        }
        if let Err(e) = p.shape.geometry() {
            ui.colored_label(egui::Color32::RED, e);
        }
    }
    if let Some(e) = &f.error {
        ui.colored_label(egui::Color32::RED, e);
    }
    ui.separator();
    if apply && apply_proposal(app, &mut f) {
        return;
    }
    let params = (preview || convert).then(|| {
        app.conversion.next_request = app.conversion.next_request.saturating_add(1);
        f.request = app.conversion.next_request;
        f.image_stamp =
            f.image.and_then(|id| app.session.map().layer(id)).map(|l| (l.instance_id(), l.replacement_revision(), l.geometry_rev));
        f.pending = true;
        f.apply_after_fit = convert;
        f.proposal = None;
        f.error = None;
        let mut p = json!({"layer":f.layer,"row":f.row,"kind":f.kind,"request":f.request});
        if let Some(image) = f.image {
            p["image"] = json!(image);
        }
        p
    });
    app.conversion.form = Some(f);
    if let Some(p) = params {
        let _ = app.run("ui.previewShapeConversion", p);
    }
}
fn number(ui: &mut egui::Ui, label: &str, value: &mut f64) {
    ui.horizontal(|ui| {
        let label = ui.label(label);
        ui.add(egui::DragValue::new(value).speed(0.1)).labelled_by(label.id);
    });
}
pub(crate) fn draw(app: &RustGisApp, painter: &egui::Painter, rect: egui::Rect) {
    let Some(f) = app.conversion.form.as_ref().filter(|f| valid(app, f)) else { return };
    let Some(p) = &f.proposal else { return };
    let Ok(rustgis_engine::data::Geometry::Polygon(polys)) = p.shape.geometry() else { return };
    let Some(ring) = polys.first().and_then(|p| p.first()) else { return };
    let points: Option<Vec<egui::Pos2>> = ring.iter().map(|c| crate::mapview::lonlat_to_screen(app, rect, *c)).collect();
    if let Some(points) = points {
        painter.add(egui::Shape::closed_line(points, egui::Stroke::new(2.5, egui::Color32::from_rgb(255, 155, 30))));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::Queryable;
    use rustgis_engine::{
        Project, Session, Tool,
        box_edit::{BoxSpec, Frame},
        data::{Dataset, GeometryType},
    };
    pub(super) fn app() -> RustGisApp {
        let mut s = Session::new(Project::default());
        let id = s.map_mut().add_dataset(Dataset::new("Imported", GeometryType::Polygon, vec![]));
        s.active_layer = Some(id);
        s.run("edit.enable", &json!({"on":true})).unwrap();
        let b = BoxSpec { frame: Frame::default(), center: [1000., 1000.], width: 200., height: 80., angle: 25. };
        s.run("edit.addFeature", &json!({"geometry":rustgis_engine::data::io::geometry_to_geojson(&b.geometry().unwrap())})).unwrap();
        let mut app = RustGisApp::new(s);
        open(&mut app, &json!({"kind":"rotatedBox"})).unwrap();
        app
    }
    fn harness(app: RustGisApp) -> egui_kittest::Harness<'static, RustGisApp> {
        egui_kittest::Harness::builder()
            .with_size(egui::vec2(500., 1100.))
            .with_max_steps(1000)
            .build_ui_state(|ui, app| panel(app, ui), app)
    }
    #[test]
    fn shape_conversion_defaults_follow_the_active_tool_and_close_restores_it() {
        for (tool, explicit, expected) in [
            (Tool::Circle, None, "circle"),
            (Tool::RotatedBox, None, "rotatedBox"),
            (Tool::Explore, None, "circle"),
            (Tool::RotatedBox, Some("circle"), "circle"),
            (Tool::Circle, Some("rotatedBox"), "rotatedBox"),
        ] {
            let mut app = app();
            app.conversion.form = None;
            app.run("tool.set", json!({"tool":tool.code()})).unwrap();
            let depth = app.session.undo_depth();
            let mut params = json!({});
            if let Some(kind) = explicit {
                params["kind"] = json!(kind);
            }
            app.run("ui.convertShape", params).unwrap();
            assert_eq!(app.conversion.form.as_ref().unwrap().kind, expected);
            let mut h = harness(app);
            let shown = if expected == "circle" { "Circle" } else { "Rotated Box" };
            assert!(h.query_all_by_role(egui::accesskit::Role::ComboBox).any(|node| node.value().as_deref() == Some(shown)));
            h.get_by_label("Close").click();
            h.run();
            assert!(h.state().conversion.form.is_none());
            assert_eq!(h.state().session.tool, tool);
            assert_eq!(h.state().session.undo_depth(), depth);
        }
    }

    #[test]
    fn shape_conversion_preview_keeps_actions_and_grid_in_place() {
        for kind in ["circle", "rotatedBox"] {
            let mut app = app();
            open(&mut app, &json!({"kind":kind})).unwrap();
            let mut h = harness(app);
            let primary = h.get_by_label("Convert Now").rect();
            let preview = h.get_by_label("Preview Conversion").rect();
            let close = h.get_by_label("Close").rect();
            let grid = h.get_by_label("Construction Grid").rect();
            assert!(primary.max.y < grid.min.y);
            for _ in 0..2 {
                h.get_by_label("Preview Conversion").click();
                h.run();
                assert!(h.state().conversion.form.as_ref().unwrap().proposal.is_some());
                assert_eq!(h.get_by_label("Apply Conversion").rect(), primary);
                assert_eq!(h.get_by_label("Preview Conversion").rect(), preview);
                assert_eq!(h.get_by_label("Close").rect(), close);
                assert_eq!(h.get_by_label("Construction Grid").rect(), grid);
            }
            h.get_by_label("Apply Conversion").click();
            h.run();
            assert!(h.state().conversion.form.is_none());
        }
    }

    #[test]
    fn shape_conversion_preview_cancel_and_apply_use_the_existing_editor() {
        for apply in [false, true] {
            let app = app();
            let id = app.session.active_layer.unwrap();
            let before = app.session.map().layer(id).unwrap().dataset.features[0].clone();
            let depth = app.session.undo_depth();
            let mut h = harness(app);
            h.get_by_label("Preview Conversion").click();
            h.run();
            assert!(h.state().conversion.form.as_ref().unwrap().proposal.is_some());
            assert_eq!(h.state().session.undo_depth(), depth);
            assert_eq!(h.state().session.map().layer(id).unwrap().dataset.features[0], before);
            h.get_by_label(if apply { "Apply Conversion" } else { "Close" }).click();
            h.run();
            assert!(h.state().conversion.form.is_none());
            assert_eq!(h.state().session.undo_depth(), depth + usize::from(apply));
            assert_eq!(h.state().session.box_spec(id, 0).is_some(), apply);
            if apply {
                assert_eq!(h.state().session.tool, Tool::RotatedBox);
            }
        }
    }
    #[test]
    fn shape_conversion_convert_now_applies_once_and_uses_the_chosen_shape() {
        for kind in ["circle", "rotatedBox"] {
            let mut app = app();
            open(&mut app, &json!({"kind":"circle"})).unwrap();
            let id = app.session.active_layer.unwrap();
            let before = app.session.map().layer(id).unwrap().dataset.features[0].clone();
            let depth = app.session.undo_depth();
            let mut h = harness(app);
            if kind == "rotatedBox" {
                h.query_all_by_role(egui::accesskit::Role::ComboBox)
                    .find(|node| node.value().as_deref() == Some("Circle"))
                    .unwrap()
                    .click();
                h.run();
                h.get_by_label("Rotated Box").click();
                h.run();
            }
            assert_eq!(h.state().session.undo_depth(), depth, "choosing the shape must not convert it");
            assert_eq!(h.state().session.map().layer(id).unwrap().dataset.features[0], before);
            h.get_by_label("Convert Now").click();
            h.run();
            let app = h.state_mut();
            assert!(app.conversion.form.is_none());
            assert_eq!(app.session.undo_depth(), depth + 1);
            assert_eq!(app.session.tool.code(), kind);
            let after = &app.session.map().layer(id).unwrap().dataset.features[0];
            assert_eq!(after.id, before.id);
            assert_eq!(after.attrs, before.attrs);
            assert_eq!(app.session.circle_spec(id, 0).is_some(), kind == "circle");
            assert_eq!(app.session.box_spec(id, 0).is_some(), kind == "rotatedBox");
            app.run("edit.undo", json!({})).unwrap();
            assert_eq!(app.session.map().layer(id).unwrap().dataset.features[0], before);
            assert!(app.session.circle_spec(id, 0).is_none() && app.session.box_spec(id, 0).is_none());
            app.run("edit.redo", json!({})).unwrap();
            assert_eq!(app.session.circle_spec(id, 0).is_some(), kind == "circle");
            assert_eq!(app.session.box_spec(id, 0).is_some(), kind == "rotatedBox");
        }
    }

    #[test]
    fn shape_conversion_convert_now_uses_the_selected_original_image_grid() {
        use rustgis_engine::data::raster::{ColorHint, Crs, GeoTransform, Raster, geotiff};
        let mut app = app();
        let id = app.session.active_layer.unwrap();
        let raster = Raster::new(
            "image",
            16,
            16,
            vec![vec![1.; 256]],
            GeoTransform { origin_x: 840., origin_y: -840., pixel_width: 20., pixel_height: -20. },
            Crs::from_epsg(3857).unwrap(),
            None,
            ColorHint::Gray,
            "UInt8",
        )
        .unwrap();
        let bytes = geotiff::write(&raster, 0).unwrap();
        let expected = Frame::from_grid(&geotiff::read_image_grid(std::io::Cursor::new(&bytes)).unwrap()).unwrap();
        let image =
            app.session.map_mut().add_raster("Image pixels", rustgis_engine::map::RasterLayer::from_bytes(raster, "image.tif", bytes));
        app.conversion.form.as_mut().unwrap().image = Some(image);
        let mut h = harness(app);
        h.get_by_label("Convert Now").click();
        h.run();
        assert!(h.state().conversion.form.is_none());
        assert_eq!(h.state().session.box_spec(id, 0).unwrap().frame, expected);
    }

    #[test]
    fn shape_conversion_not_recorded_as_a_session_bound_task() {
        for direct in [false, true] {
            let mut app = app();
            app.ws.recording = Some(Vec::new());
            let mut h = harness(app);
            h.get_by_label(if direct { "Convert Now" } else { "Preview Conversion" }).click();
            h.run();
            if !direct {
                h.get_by_label("Apply Conversion").click();
                h.run();
            }
            assert!(h.state().conversion.form.is_none());
            assert!(h.state().ws.recording.as_ref().unwrap().is_empty());
            assert!(h.state().session.notices.iter().any(|n| n.text.contains("Shape conversion isn't recorded")));
        }
    }

    #[test]
    fn shape_conversion_convert_now_respects_fit_and_edit_rule_failures() {
        for bad_fit in [false, true] {
            let mut app = app();
            if bad_fit {
                let g = app.session.map().layers[0].dataset.features[0].geometry.clone().unwrap();
                let rustgis_engine::data::Geometry::Polygon(mut polygons) = g else { panic!() };
                let hole = polygons[0][0].clone();
                polygons[0].push(hole);
                app.run("edit.setGeometry", json!({"row":0,"geometry":rustgis_engine::data::io::geometry_to_geojson(&rustgis_engine::data::Geometry::Polygon(polygons))})).unwrap();
            } else {
                app.run("data.attributeRule", json!({"name":"Never","kind":"constraint","expression":"1 = 0"})).unwrap();
            }
            open(&mut app, &json!({"kind":"circle"})).unwrap();
            let id = app.session.active_layer.unwrap();
            let before = app.session.map().layer(id).unwrap().dataset.features[0].clone();
            let depth = app.session.undo_depth();
            let mut h = harness(app);
            h.get_by_label("Convert Now").click();
            h.run();
            let app = h.state();
            let form = app.conversion.form.as_ref().unwrap();
            assert!(form.error.is_some());
            assert!(!form.pending && !form.apply_after_fit);
            assert_eq!(app.session.undo_depth(), depth);
            assert_eq!(app.session.map().layer(id).unwrap().dataset.features[0], before);
            assert!(app.session.circle_spec(id, 0).is_none());
        }
    }

    #[test]
    fn shape_conversion_convert_now_does_not_apply_after_changing_tools() {
        let mut app = app();
        let request = app.conversion.form.as_ref().unwrap().request;
        let answer = app.session.run("edit.previewShapeConversion", &json!({"row":0,"kind":"rotatedBox","request":request})).unwrap();
        let form = app.conversion.form.as_mut().unwrap();
        form.pending = true;
        form.apply_after_fit = true;
        let depth = app.session.undo_depth();
        app.run("tool.set", json!({"tool":"measure"})).unwrap();
        landed(&mut app, Some(request), crate::Landed::Ran(Ok(answer)));
        assert!(app.conversion.form.is_none());
        assert_eq!(app.session.undo_depth(), depth);
        assert_eq!(app.session.tool, Tool::Measure);
        assert!(app.session.box_spec(app.session.active_layer.unwrap(), 0).is_none());
    }

    #[test]
    fn shape_conversion_direct_late_results_cannot_apply_after_cancel_or_retarget() {
        for cancel in [false, true] {
            let mut app = app();
            let request = app.conversion.form.as_ref().unwrap().request;
            let answer = app.session.run("edit.previewShapeConversion", &json!({"row":0,"kind":"rotatedBox","request":request})).unwrap();
            let form = app.conversion.form.as_mut().unwrap();
            form.pending = true;
            form.apply_after_fit = true;
            let depth = app.session.undo_depth();
            let id = app.session.active_layer.unwrap();
            let before = app.session.map().layer(id).unwrap().dataset.features[0].clone();
            if cancel {
                let mut h = harness(app);
                h.get_by_label("Cancel Fitting").click();
                h.run();
                app = h.into_state();
            } else {
                open(&mut app, &json!({"kind":"circle"})).unwrap();
                let form = app.conversion.form.as_mut().unwrap();
                form.pending = true;
                form.apply_after_fit = true;
            }
            landed(&mut app, Some(request), crate::Landed::Ran(Ok(answer)));
            assert_eq!(app.session.undo_depth(), depth);
            assert_eq!(app.session.map().layer(id).unwrap().dataset.features[0], before);
            assert!(app.session.box_spec(id, 0).is_none() && app.session.circle_spec(id, 0).is_none());
            if cancel {
                assert!(app.conversion.form.is_none());
            } else {
                assert!(app.conversion.form.as_ref().unwrap().pending);
                app.run("select.clear", json!({})).unwrap();
                let request = app.conversion.form.as_ref().unwrap().request;
                landed(&mut app, Some(request), crate::Landed::Ran(Err("cancelled".into())));
                assert!(app.conversion.form.is_none());
            }
        }
    }

    #[test]
    fn shape_conversion_reopened_form_ignores_every_old_completion() {
        for kind in ["circle", "rotatedBox"] {
            let mut app = app();
            let id = app.session.active_layer.unwrap();
            let geometry = app.session.map().layer(id).unwrap().dataset.features[0].geometry.clone().unwrap();
            app.run("edit.addFeature", json!({"geometry":rustgis_engine::data::io::geometry_to_geojson(&geometry)})).unwrap();
            app.run("select.rows", json!({"rows":[0],"mode":"new"})).unwrap();
            open(&mut app, &json!({"kind":kind})).unwrap();
            let old = app.conversion.form.as_ref().unwrap().request;
            let answer = app.session.run("edit.previewShapeConversion", &json!({"row":0,"kind":kind,"request":old})).unwrap();
            app.conversion.form.as_mut().unwrap().pending = true;
            app.conversion.form.as_mut().unwrap().apply_after_fit = true;
            app.run("select.rows", json!({"rows":[1],"mode":"new"})).unwrap();
            open(&mut app, &json!({"kind":kind})).unwrap();
            let current = app.conversion.form.as_ref().unwrap().request;
            assert_ne!(current, old);
            app.conversion.form.as_mut().unwrap().pending = true;
            app.conversion.form.as_mut().unwrap().apply_after_fit = true;
            let before = app.session.map().layer(id).unwrap().dataset.features.to_vec();
            let depth = app.session.undo_depth();
            for result in [
                crate::Landed::Ran(Err("old fit failed".into())),
                crate::Landed::NotRun("old fit cancelled".into()),
                crate::Landed::Ran(Ok(answer)),
            ] {
                landed(&mut app, Some(old), result);
                let f = app.conversion.form.as_ref().unwrap();
                assert_eq!((f.request, f.row), (current, 1));
                assert!(f.pending && f.apply_after_fit && f.error.is_none() && f.proposal.is_none());
                assert_eq!(app.session.undo_depth(), depth);
                assert_eq!(app.session.map().layer(id).unwrap().dataset.features.to_vec(), before);
            }
            let answer = app.session.run("edit.previewShapeConversion", &json!({"row":1,"kind":kind,"request":current})).unwrap();
            landed(&mut app, Some(current), crate::Landed::Ran(Ok(answer)));
            assert!(app.conversion.form.is_none(), "the new Convert Now must still apply");
            assert_eq!(app.session.undo_depth(), depth + 1);
            assert_eq!(app.session.map().layer(id).unwrap().dataset.features[0], before[0]);
            assert!(if kind == "circle" { app.session.circle_spec(id, 1).is_some() } else { app.session.box_spec(id, 1).is_some() });
        }
    }

    #[test]
    fn shape_conversion_selection_changes_and_late_answers_never_retarget() {
        let mut app = app();
        let request = app.conversion.form.as_ref().unwrap().request;
        let answer = app.session.run("edit.previewShapeConversion", &json!({"row":0,"kind":"circle","request":request})).unwrap();
        open(&mut app, &json!({"kind":"rotatedBox"})).unwrap();
        landed(&mut app, Some(request), crate::Landed::Ran(Ok(answer)));
        assert!(app.conversion.form.as_ref().unwrap().proposal.is_none());
        app.session.run("select.clear", &json!({})).unwrap();
        let mut h = harness(app);
        h.run();
        assert!(h.state().conversion.form.is_none());
    }
}

#[cfg(test)]
mod overlap_tests {
    use super::*;
    #[test]
    fn shape_conversion_selection_tools_offer_the_right_click_menu() {
        use egui_kittest::kittest::Queryable;
        use rustgis_engine::Tool;
        for tool in [Tool::Explore, Tool::Select, Tool::SelectPolygon, Tool::SelectLasso, Tool::SelectRadius] {
            let mut app = super::tests::app();
            app.conversion.form = None;
            let id = app.session.active_layer.unwrap();
            let g = app.session.map().layer(id).unwrap().dataset.features[0].geometry.clone().unwrap();
            app.run("edit.addFeature", json!({"geometry":rustgis_engine::data::io::geometry_to_geojson(&g)})).unwrap();
            app.run("select.rows", json!({"rows":[0,1],"mode":"new"})).unwrap();
            app.session.tool = tool;
            app.run("map.zoomToExtent", json!({"extent":[-0.02,-0.02,0.04,0.04]})).unwrap();
            let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1400., 860.)).with_max_steps(1000).build_ui_state(
                |ui, app: &mut RustGisApp| {
                    let ctx = ui.ctx().clone();
                    app.logic(&ctx);
                    app.ui(ui);
                },
                app,
            );
            h.run_steps(6);
            let c = rustgis_engine::box_edit::Frame::default().to_lonlat([1000., 1000.]).unwrap();
            let at = crate::mapview::lonlat_to_screen(h.state(), h.state().map.view_rect.unwrap().0, c).unwrap();
            h.event(egui::Event::PointerMoved(at));
            for pressed in [true, false] {
                h.event(egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Secondary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                });
            }
            h.run_steps(3);
            assert_eq!(h.state().session.map().layer(id).unwrap().selection, [1], "{tool:?}");
            assert!(h.query_by_label("Circle…").is_none());
            assert!(h.query_by_label("Rotated Box…").is_none());
            h.get_by_label("Convert Polygon…").click();
            h.run();
            assert_eq!(h.state().conversion.form.as_ref().unwrap().row, 1, "{tool:?}");
        }
    }

    #[test]
    fn shape_conversion_right_click_respects_tool_and_empty_space_selection() {
        use rustgis_engine::Tool;
        for tool in [
            Tool::Explore,
            Tool::Select,
            Tool::SelectPolygon,
            Tool::SelectLasso,
            Tool::SelectRadius,
            Tool::EditVertices,
            Tool::CreatePolygon,
            Tool::Measure,
            Tool::Circle,
            Tool::RotatedBox,
        ] {
            let mut app = super::tests::app();
            app.conversion.form = None;
            app.session.tool = tool;
            let id = app.session.active_layer.unwrap();
            let frame = rustgis_engine::box_edit::Frame::default();
            pick(&mut app, frame.to_lonlat([1e7, 1e7]).unwrap(), false);
            let clears = matches!(tool, Tool::Circle | Tool::RotatedBox);
            assert_eq!(app.session.map().layer(id).unwrap().selection.is_empty(), clears, "{tool:?}");
            assert!(app.conversion.menu.is_none(), "{tool:?}");
            if !clears {
                app.run("select.clear", json!({})).unwrap();
            }
            pick(&mut app, frame.to_lonlat([1000., 1000.]).unwrap(), false);
            let draws = matches!(tool, Tool::CreatePolygon | Tool::Measure);
            assert_eq!(app.session.map().layer(id).unwrap().selection.is_empty(), draws, "{tool:?}");
            assert_eq!(app.conversion.menu.is_none(), draws, "{tool:?}");
        }
    }

    #[test]
    fn shape_conversion_right_click_keeps_selected_overlap_and_alt_cycles() {
        let mut app = super::tests::app();
        let id = app.session.active_layer.unwrap();
        let g = app.session.map().layer(id).unwrap().dataset.features[0].geometry.clone().unwrap();
        app.session.run("edit.addFeature", &json!({"geometry":rustgis_engine::data::io::geometry_to_geojson(&g)})).unwrap();
        app.session.run("select.rows", &json!({"rows":[0],"mode":"new"})).unwrap();
        let c = rustgis_engine::box_edit::Frame::default().to_lonlat([1000., 1000.]).unwrap();
        pick(&mut app, c, false);
        assert_eq!(app.conversion.menu.map(|t| (t.layer, t.row)), Some((id, 0)));
        pick(&mut app, c, true);
        assert_eq!(app.conversion.menu.map(|t| (t.layer, t.row)), Some((id, 1)));
    }
}
