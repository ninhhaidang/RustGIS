//! Constrained circle gestures. A drag previews only; release commits one ordinary edit command.
use crate::mapview::{lonlat_to_screen, screen_to_lonlat};
use crate::{RightPane, RustGisApp};
use egui::{Color32, Painter, Pos2, Rect, Stroke};
use rustgis_engine::box_edit::Frame;
use rustgis_engine::circle_edit::CircleSpec;
use rustgis_engine::data::Feature;
use rustgis_engine::{Tool, geom::Coord};
use serde_json::json;

#[derive(Default)]
pub(crate) struct State {
    gesture: Option<Gesture>,
    preview: Option<CircleSpec>,
    error: Option<String>,
}
struct Gesture {
    project: u64,
    map: usize,
    map_name: String,
    layer: u32,
    instance: u64,
    replacement: u64,
    rows: std::sync::Weak<Vec<Feature>>,
    row: Option<usize>,
    feature: Option<u64>,
    original: CircleSpec,
    start: [f64; 2],
    handle: Handle,
}
#[derive(Clone, Copy)]
enum Handle {
    New,
    Move,
    Radius,
}

pub(crate) fn ready(app: &RustGisApp) -> bool {
    app.session.active_layer.is_some_and(|id| {
        rustgis_engine::circle_edit::check_tool(&app.session, id).is_ok()
            && app.session.map().layer(id).is_some_and(|l| app.session.map().draws_at(l, app.session.scale()))
    })
}
fn selected(app: &RustGisApp) -> Option<(u32, usize, CircleSpec)> {
    let id = app.session.active_layer?;
    let l = app.session.map().layer(id)?;
    let [row] = l.selection.as_slice() else { return None };
    Some((id, *row, app.session.circle_spec(id, *row)?))
}
pub(crate) fn drawing(app: &RustGisApp) -> bool {
    app.circles.gesture.is_some()
}
pub(crate) fn cancel(app: &mut RustGisApp) {
    app.circles = State::default();
    app.map.cancel_circle_drag();
}
fn valid(app: &RustGisApp, g: &Gesture) -> bool {
    ready(app)
        && app.session.tool == Tool::Circle
        && g.project == app.session.opened
        && g.map == app.session.project.active_map
        && g.map_name == app.session.map().name
        && app.session.active_layer == Some(g.layer)
        && app.session.map().layer(g.layer).is_some_and(|l| {
            l.instance_id() == g.instance
                && l.replacement_revision() == g.replacement
                && g.rows.ptr_eq(&l.dataset.features.allocation_key())
                && g.row.is_none_or(|row| l.dataset.features.get(row).map(|f| f.id) == g.feature)
        })
}
pub(crate) fn cancel_stale(app: &mut RustGisApp) {
    if app.circles.gesture.as_ref().is_some_and(|g| !valid(app, g)) {
        cancel(app);
    }
    if !app.forms.template.is_empty()
        && !app
            .session
            .active_layer
            .and_then(|id| app.session.map().layer(id))
            .is_some_and(|l| l.design.templates.iter().any(|t| t.name == app.forms.template))
    {
        app.forms.template.clear();
    }
}
fn at(app: &RustGisApp, rect: Rect, frame: &Frame, p: [f64; 2]) -> Option<Pos2> {
    lonlat_to_screen(app, rect, frame.to_lonlat(p)?)
}
fn handles(app: &RustGisApp, rect: Rect, b: &CircleSpec) -> Vec<(Handle, Pos2)> {
    at(app, rect, &b.frame, b.point(0.)).map(|p| vec![(Handle::Radius, p)]).unwrap_or_default()
}
fn handle_at(app: &RustGisApp, rect: Rect, p: Pos2, b: &CircleSpec) -> Option<Handle> {
    handles(app, rect, b).into_iter().rev().find(|(_, at)| at.distance(p) <= 8.).map(|(h, _)| h)
}
pub(crate) use crate::boxedit::click;
use crate::boxedit::hits;
pub(crate) fn drag_start(app: &mut RustGisApp, rect: Rect, origin: Pos2, force_new: bool) -> bool {
    if !ready(app) {
        return false;
    }
    let Some(c) = screen_to_lonlat(app, rect, origin) else { return false };
    let chosen = if force_new {
        None
    } else {
        selected(app)
            .and_then(|(id, row, b)| {
                handle_at(app, rect, origin, &b).or_else(|| contains(&b, c).then_some(Handle::Move)).map(|h| (id, row, b, h))
            })
            .or_else(|| hits(app, c).into_iter().find_map(|(id, row)| app.session.circle_spec(id, row).map(|b| (id, row, b, Handle::Move))))
    };
    let Some(id) = app.session.active_layer else { return false };
    let (row, original, handle) = match chosen {
        Some((_, row, b, h)) => (Some(row), b, h),
        None => (None, CircleSpec { frame: Frame::default(), center: [0., 0.], radius: 1., tolerance: 0.25 }, Handle::New),
    };
    let Some(start) = original.frame.to_local(c) else { return false };
    if let Some(row) = row {
        rustgis_engine::cmd::select::set(&mut app.session, id, vec![row], rustgis_engine::cmd::select::Mode::New);
        app.map.invalidate();
    }
    let Some(l) = app.session.map().layer(id) else { return false };
    app.circles = State {
        gesture: Some(Gesture {
            project: app.session.opened,
            map: app.session.project.active_map,
            map_name: app.session.map().name.clone(),
            layer: id,
            instance: l.instance_id(),
            replacement: l.replacement_revision(),
            rows: l.dataset.features.allocation_key(),
            row,
            feature: row.and_then(|r| l.dataset.features.get(r).map(|f| f.id)),
            original,
            start,
            handle,
        }),
        preview: None,
        error: None,
    };
    true
}
fn moved(b: &CircleSpec, start: [f64; 2], p: [f64; 2], h: Handle) -> CircleSpec {
    let mut next = b.clone();
    match h {
        Handle::New => {
            next.center = start;
            next.radius = (p[0] - start[0]).hypot(p[1] - start[1]);
            next.tolerance = (next.radius * 1e-4).max(0.25);
        }
        Handle::Move => next.center = [b.center[0] + p[0] - start[0], b.center[1] + p[1] - start[1]],
        Handle::Radius => next.radius = (p[0] - b.center[0]).hypot(p[1] - b.center[1]),
    }
    next
}
pub(crate) fn drag_to(app: &mut RustGisApp, rect: Rect, p: Pos2) {
    cancel_stale(app);
    let Some(g) = app.circles.gesture.as_ref() else { return };
    let Some(p) = screen_to_lonlat(app, rect, p).and_then(|c| g.original.frame.to_local(c)) else {
        app.circles.preview = None;
        app.circles.error = Some(tl!("Choose a position within the supported map and construction grid.").into());
        return;
    };
    let b = moved(&g.original, g.start, p, g.handle);
    match b.geometry() {
        Ok(_) => {
            app.circles.error = None;
            app.circles.preview = Some(b);
        }
        // Keep the last valid outline, so `draw` can show it in red beside the error.
        Err(e) => app.circles.error = Some(e),
    }
}
pub(crate) fn drag_stop(app: &mut RustGisApp) {
    cancel_stale(app);
    let State { gesture, preview, error } = std::mem::take(&mut app.circles);
    if let Some(e) = error {
        app.session.notify(e, true);
        return;
    }
    let (Some(g), Some(b)) = (gesture, preview) else { return };
    if let Some(row) = g.row {
        if b != g.original {
            let _ = app.run("edit.updateCircle", json!({"layer":g.layer,"row":row,"featureId":g.feature,"circle":b}));
        }
    } else {
        let mut p = json!({"layer":g.layer,"circle":b});
        if app.session.map().layer(g.layer).is_some_and(|l| l.design.templates.iter().any(|t| t.name == app.forms.template)) {
            p["template"] = json!(app.forms.template);
        }
        let _ = app.run("edit.addCircle", p);
    }
}
pub(crate) fn cursor(app: &RustGisApp, rect: Rect, p: Pos2) -> Option<egui::CursorIcon> {
    let (_, _, b) = selected(app)?;
    if handle_at(app, rect, p, &b).is_some() {
        return Some(egui::CursorIcon::Crosshair);
    }
    contains(&b, screen_to_lonlat(app, rect, p)?).then_some(egui::CursorIcon::Grab)
}
fn contains(b: &CircleSpec, c: Coord) -> bool {
    let Some(q) = b.frame.to_local(c) else { return false };
    let d = [q[0] - b.center[0], q[1] - b.center[1]];
    d[0].hypot(d[1]) <= b.radius
}
pub(crate) fn draw(app: &RustGisApp, painter: &Painter, rect: Rect, accent: Color32) {
    if !ready(app) {
        return;
    }
    let b = app.circles.preview.clone().or_else(|| selected(app).map(|(_, _, b)| b));
    let Some(b) = b else { return };
    let Ok(rustgis_engine::data::Geometry::Polygon(polys)) = b.geometry() else { return };
    let Some(ring) = polys.first().and_then(|p| p.first()) else { return };
    let points: Option<Vec<Pos2>> = ring.iter().map(|c| lonlat_to_screen(app, rect, *c)).collect();
    let Some(points) = points else { return };
    let color = if app.circles.error.is_some() { Color32::RED } else { accent };
    painter.add(egui::Shape::closed_line(points, Stroke::new(1.5, color)));
    if let (Some(center), Some((_, p))) = (at(app, rect, &b.frame, b.center), handles(app, rect, &b).first()) {
        painter.line_segment([center, *p], Stroke::new(1., color));
        painter.circle_filled(center, 3., color);
        painter.circle_filled(*p, 6., Color32::WHITE);
        painter.circle_stroke(*p, 6., Stroke::new(1.5, color));
    }
}

pub(crate) fn panel(app: &mut RustGisApp, ui: &mut egui::Ui) {
    if app.session.tool != Tool::Circle {
        return;
    }
    ui.separator();
    ui.strong(tl!("Circle"));
    ui.small(tl!("Drag from centre to edge to draw. Drag inside to move; drag the radius handle to resize. Ctrl/Cmd+drag draws over existing features. Space or middle-drag pans; releasing Space keeps this tool."));
    if !ready(app) {
        ui.label(tl!("Turn editing on and choose a loaded polygon layer in a flat Earth map."));
        return;
    }
    if let Some((layer, row, mut b)) = selected(app) {
        let feature = app.session.map().layer(layer).and_then(|l| l.dataset.features.get(row)).map(|f| f.id);
        let name = app.session.map().layer(layer).map(|l| l.name.clone()).unwrap_or_default();
        let shown = feature.map(|id| id.to_string()).unwrap_or_default();
        ui.small(crate::i18n::fmt("Layer: {layer} · Feature {feature}", &[("layer", &name), ("feature", &shown)]));
        ui.label(if b.frame == Frame::default() {
            tl!("Web Mercator grid metres, not ground metres.")
        } else {
            tl!("Original grid pixels. A circle may appear elliptical in the map projection.")
        });
        let mut changed = false;
        let [cx, cy] = &mut b.center;
        egui::Grid::new("circle_dimensions").show(ui, |ui| {
            for (name, v) in
                [(tl!("Center X"), cx), (tl!("Center Y"), cy), (tl!("Radius"), &mut b.radius), (tl!("Tolerance"), &mut b.tolerance)]
            {
                let label = ui.label(name);
                let r = ui.add(egui::DragValue::new(v).update_while_editing(false).speed(0.1)).labelled_by(label.id);
                if crate::widgets::changed(&r) {
                    crate::widgets::one_step_per_press(app, &r);
                    changed = true;
                }
                ui.end_row();
            }
        });
        if changed {
            let _ = app.run("edit.updateCircle", json!({"layer":layer,"row":row,"featureId":feature,"circle":b}));
        }
        if ui.button(tl!("Delete Selected Circle")).clicked() {
            let _ = app.run("edit.deleteCircle", json!({"layer":layer,"row":row,"featureId":feature}));
        }
    } else {
        ui.label(tl!("Drag a new circle or click one created in this session. Other polygons use Edit Vertices."));
    }
    ui.small(tl!("Saved and exported circles are polygon approximations. Centre and radius are kept only in this session. New drags use a tolerance of 0.25 grid units or radius/10000, whichever is larger, before projection."));
    if let Some(e) = &app.circles.error {
        ui.colored_label(Color32::RED, e);
    }
}
pub(crate) fn context_menu(app: &mut RustGisApp, ui: &mut egui::Ui) {
    if selected(app).is_some() {
        if ui.button(tl!("Edit Circle Dimensions")).clicked() {
            app.ui.open_right(RightPane::Editing);
            ui.close();
        }
        if ui.button(tl!("Delete Selected Circle")).clicked() {
            if let Some((id, row, _)) = selected(app) {
                let _ = app.run("edit.deleteCircle", json!({"layer":id,"row":row}));
            }
            ui.close();
        }
    } else {
        ui.label(tl!("Select a circle created in this session."));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::vec2;
    use rustgis_engine::{
        Project, Session,
        data::{Dataset, GeometryType},
    };
    fn app() -> RustGisApp {
        let mut s = Session::new(Project::default());
        let id = s.map_mut().add_dataset(Dataset::new("Boxes", GeometryType::Polygon, vec![]));
        s.active_layer = Some(id);
        s.map_mut().camera.res = 1000.;
        s.map_mut().camera.center = Coord::new(0., 0.);
        s.run("edit.enable", &json!({"on":true})).unwrap();
        s.run("tool.set", &json!({"tool":"circle"})).unwrap();
        RustGisApp::new(s)
    }
    fn harness(app: RustGisApp) -> egui_kittest::Harness<'static, RustGisApp> {
        egui_kittest::Harness::builder().with_size(vec2(1000., 800.)).with_max_steps(1000).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app,
        )
    }
    fn rect() -> Rect {
        Rect::from_min_size(Pos2::ZERO, vec2(1000., 800.))
    }
    fn create(app: &mut RustGisApp) -> (u32, usize, CircleSpec) {
        assert!(drag_start(app, rect(), Pos2::new(400., 350.), false));
        drag_to(app, rect(), Pos2::new(600., 450.));
        drag_stop(app);
        selected(app).unwrap()
    }
    #[test]
    fn circle_edit_drag_previews_without_mutation_and_commits_one_undo_step() {
        let mut app = app();
        let id = app.session.active_layer.unwrap();
        let depth = app.session.undo_depth();
        assert!(drag_start(&mut app, rect(), Pos2::new(400., 350.), false));
        for x in [420., 450., 500., 600.] {
            drag_to(&mut app, rect(), Pos2::new(x, 450.));
        }
        assert_eq!(app.session.map().layer(id).unwrap().dataset.features.len(), 0);
        assert_eq!(app.session.undo_depth(), depth);
        drag_stop(&mut app);
        assert_eq!(app.session.map().layer(id).unwrap().dataset.features.len(), 1);
        assert_eq!(app.session.undo_depth(), depth + 1);
        app.run("edit.undo", json!({})).unwrap();
        assert_eq!(app.session.map().layer(id).unwrap().dataset.features.len(), 0);
        app.run("edit.redo", json!({})).unwrap();
        assert!(app.session.circle_spec(id, 0).is_some());
    }
    #[test]
    fn circle_edit_replaced_rows_or_project_cancel_a_held_gesture() {
        let mut app = app();
        let (id, row, b) = create(&mut app);
        let center = at(&app, rect(), &b.frame, b.center).unwrap();
        assert!(drag_start(&mut app, rect(), center, false));
        drag_to(&mut app, rect(), center + vec2(50., 20.));
        app.session.map_mut().layer_mut(id).unwrap().dataset.features[row].id += 1;
        drag_stop(&mut app);
        assert!(app.session.circle_spec(id, row).is_none());
        let depth = app.session.undo_depth();
        assert!(drag_start(&mut app, rect(), Pos2::new(100., 100.), true));
        drag_to(&mut app, rect(), Pos2::new(200., 200.));
        app.session.opened = app.session.opened.wrapping_add(1);
        drag_stop(&mut app);
        assert_eq!(app.session.undo_depth(), depth);
    }
    #[test]
    fn circle_edit_cancel_and_tool_switch_never_commit_a_preview() {
        let mut app = app();
        let id = app.session.active_layer.unwrap();
        assert!(drag_start(&mut app, rect(), Pos2::new(100., 100.), true));
        drag_to(&mut app, rect(), Pos2::new(200., 200.));
        cancel(&mut app);
        drag_stop(&mut app);
        assert_eq!(app.session.tool, Tool::Circle);
        assert!(drag_start(&mut app, rect(), Pos2::new(100., 100.), true));
        drag_to(&mut app, rect(), Pos2::new(200., 200.));
        app.run("tool.set", json!({"tool":"explore"})).unwrap();
        drag_stop(&mut app);
        assert!(app.session.map().layer(id).unwrap().dataset.features.is_empty());
    }
    #[test]
    fn circle_edit_escape_cancels_before_leaving_the_tool() {
        let mut h = harness(app());
        for _ in 0..6 {
            h.step();
        }
        let view = h.state().map.view_rect.unwrap().0;
        let id = h.state().session.active_layer.unwrap();
        let start = view.center() - vec2(100., 100.);
        assert!(drag_start(h.state_mut(), view, start, true));
        drag_to(h.state_mut(), view, start + vec2(100., 100.));
        let key = |pressed| egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        h.input_mut().events.push(key(true));
        h.step();
        assert!(!drawing(h.state()));
        assert_eq!(h.state().session.tool, Tool::Circle);
        assert!(h.state().session.map().layer(id).unwrap().dataset.features.is_empty());
        h.input_mut().events.push(key(false));
        h.step();
        h.input_mut().events.push(key(true));
        h.step();
        assert_eq!(h.state().session.tool, Tool::Explore);
    }

    #[test]
    fn circle_edit_alt_click_cycles_overlap_and_forced_draw_keeps_existing_circles() {
        let mut app = app();
        let (id, _, b) = create(&mut app);
        app.run("edit.addCircle", json!({"circle":b})).unwrap();
        let c = b.frame.to_lonlat(b.center).unwrap();
        click(&mut app, c, false, false);
        let one = selected(&app).unwrap().1;
        click(&mut app, c, false, true);
        let two = selected(&app).unwrap().1;
        assert_ne!(one, two);
        let center = at(&app, rect(), &b.frame, b.center).unwrap();
        assert!(drag_start(&mut app, rect(), center, true));
        drag_to(&mut app, rect(), center + vec2(50., 50.));
        drag_stop(&mut app);
        assert_eq!(app.session.map().layer(id).unwrap().dataset.features.len(), 3);
    }
    #[test]
    fn circle_edit_cycles_past_ordinary_polygons_and_moves_the_selected_overlap() {
        let mut app = app();
        let (id, first, b) = create(&mut app);
        app.run("edit.addCircle", json!({"circle":b})).unwrap();
        app.run("edit.addFeature", json!({"geometry":rustgis_engine::data::io::geometry_to_geojson(&b.geometry().unwrap())})).unwrap();
        let c = b.frame.to_lonlat(b.center).unwrap();
        click(&mut app, c, false, false);
        assert_eq!(app.session.map().layer(id).unwrap().selection, vec![2]);
        assert!(selected(&app).is_none());
        click(&mut app, c, false, true);
        assert_eq!(selected(&app).unwrap().1, 1);
        click(&mut app, c, false, true);
        assert_eq!(selected(&app).unwrap().1, first);
        let p = at(&app, rect(), &b.frame, b.center).unwrap();
        assert!(drag_start(&mut app, rect(), p, false));
        drag_to(&mut app, rect(), p + vec2(20., 10.));
        drag_stop(&mut app);
        assert_eq!(selected(&app).unwrap().1, first);
        assert_ne!(app.session.circle_spec(id, first).unwrap().center, b.center);
        assert_eq!(app.session.circle_spec(id, 1), Some(b));
    }

    #[test]
    fn circle_edit_ignores_another_layers_template_and_keeps_valid_defaults() {
        let mut app = app();
        let target = app.session.map_mut().add_dataset(Dataset::new(
            "Target",
            GeometryType::Polygon,
            vec![rustgis_engine::data::Field::new("class", rustgis_engine::data::FieldType::Text)],
        ));
        app.forms.template = "Previous layer template".into();
        app.session.active_layer = Some(target);
        cancel_stale(&mut app);
        assert!(app.forms.template.is_empty());
        assert!(drag_start(&mut app, rect(), Pos2::new(400., 350.), false));
        drag_to(&mut app, rect(), Pos2::new(600., 450.));
        app.forms.template = "Previous layer template".into();
        drag_stop(&mut app);
        assert_eq!(app.session.map().layer(target).unwrap().dataset.features.len(), 1);
        app.session.map_mut().layer_mut(target).unwrap().design.templates.push(rustgis_engine::map::design::FeatureTemplate {
            name: "Vehicle".into(),
            description: String::new(),
            defaults: vec![("class".into(), rustgis_engine::data::Value::Text("vehicle".into()))],
        });
        app.forms.template = "Vehicle".into();
        assert!(drag_start(&mut app, rect(), Pos2::new(400., 350.), true));
        drag_to(&mut app, rect(), Pos2::new(600., 450.));
        drag_stop(&mut app);
        assert_eq!(app.session.map().layer(target).unwrap().dataset.value(1, 0), &rustgis_engine::data::Value::Text("vehicle".into()));
    }

    #[test]
    fn circle_edit_context_menu_wins_over_a_stale_ordinary_map_menu() {
        use egui_kittest::kittest::Queryable;
        let mut app = app();
        let (id, row, b) = create(&mut app);
        app.map.menu_lonlat = Some(b.frame.to_lonlat(b.center).unwrap());
        let mut h = harness(app);
        for _ in 0..6 {
            h.step();
        }
        let view = h.state().map.view_rect.unwrap().0;
        let p = at(h.state(), view, &b.frame, b.center).unwrap();
        h.input_mut().events.push(egui::Event::PointerMoved(p));
        for pressed in [true, false] {
            h.input_mut().events.push(egui::Event::PointerButton {
                pos: p,
                button: egui::PointerButton::Secondary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
            h.step();
        }
        h.step();
        assert!(h.query_by_label("Delete Selected Circle").is_some());
        assert!(h.query_by_label("Edit Circle Dimensions").is_some());
        assert_eq!(h.state().session.map().layer(id).unwrap().selection, vec![row]);
    }

    #[test]
    fn circle_edit_space_pan_returns_to_editing_without_an_identify_popup_or_new_feature() {
        let mut app = app();
        let (id, row, _) = create(&mut app);
        let mut h = harness(app);
        for _ in 0..6 {
            h.step();
        }
        let view = h.state().map.view_rect.unwrap().0;
        let center = view.center();
        let camera = h.state().session.map().camera.center;
        h.input_mut().events.push(egui::Event::Key {
            key: egui::Key::Space,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        h.input_mut().events.push(egui::Event::PointerMoved(center));
        h.input_mut().events.push(egui::Event::PointerButton {
            pos: center,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        h.step();
        h.input_mut().events.push(egui::Event::PointerMoved(center + vec2(50., 30.)));
        h.step();
        h.input_mut().events.push(egui::Event::PointerButton {
            pos: center + vec2(50., 30.),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        h.step();
        h.input_mut().events.push(egui::Event::Key {
            key: egui::Key::Space,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        h.step();
        assert_ne!(h.state().session.map().camera.center, camera);
        assert_eq!(h.state().session.tool, Tool::Circle);
        let b = h.state().session.circle_spec(id, row).unwrap();
        let view = h.state().map.view_rect.unwrap().0;
        let p = at(h.state(), view, &b.frame, b.center).unwrap();
        h.input_mut().events.push(egui::Event::PointerMoved(p));
        h.input_mut().events.push(egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        h.step();
        h.input_mut().events.push(egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        h.step();
        assert_eq!(h.state().session.tool, Tool::Circle);
        assert_eq!(h.state().session.map().layer(id).unwrap().selection, vec![row]);
        assert_eq!(h.state().session.map().layer(id).unwrap().dataset.features.len(), 1);
        assert!(h.state().map.popup.is_none());
    }
    #[test]
    fn circle_edit_radius_handle_keeps_centre_and_move_keeps_radius() {
        let mut app = app();
        let (id, row, b) = create(&mut app);
        let depth = app.session.undo_depth();
        let handle = handles(&app, rect(), &b)[0].1;
        assert!(drag_start(&mut app, rect(), handle, false));
        drag_to(&mut app, rect(), handle + vec2(35., 20.));
        drag_stop(&mut app);
        let resized = app.session.circle_spec(id, row).unwrap();
        assert_eq!(resized.center, b.center);
        assert_ne!(resized.radius, b.radius);
        assert_eq!(app.session.undo_depth(), depth + 1);
        let center = at(&app, rect(), &b.frame, b.center).unwrap();
        assert!(drag_start(&mut app, rect(), center, false));
        drag_to(&mut app, rect(), center + vec2(20., 20.));
        drag_stop(&mut app);
        let moved = app.session.circle_spec(id, row).unwrap();
        assert_eq!(moved.radius, resized.radius);
        assert_ne!(moved.center, resized.center);
        assert_eq!(moved.frame, b.frame);
        app.run("edit.undo", json!({})).unwrap();
        assert_eq!(app.session.circle_spec(id, row), Some(resized));
    }
    #[test]
    fn circle_edit_browser_project_download_reports_loss_only_after_acceptance() {
        for accept in [false, true] {
            let mut app = app();
            let (id, row, b) = create(&mut app);
            let before = app.session.project.name.clone();
            app.services.download = Some(Box::new(move |_, _| if accept { Ok(()) } else { Err("rejected download".into()) }));
            app.session.notices.clear();
            assert_eq!(app.save_download("Circle sample.rgis").is_ok(), accept);
            assert_eq!(app.session.notices.iter().filter(|n| n.text.starts_with("1 circle")).count(), usize::from(accept));
            assert_eq!(app.session.circle_spec(id, row), Some(b));
            if !accept {
                assert_eq!(app.session.project.name, before);
            }
        }
    }
    #[test]
    fn circle_edit_item_download_reports_construction_loss_only_after_acceptance() {
        for kind in ["layer", "map", "webLayer", "webMap", "obj", "stl"] {
            for accept in [false, true] {
                let mut app = app();
                let (id, row, b) = create(&mut app);
                app.services.download = Some(Box::new(move |_, _| if accept { Ok(()) } else { Err("rejected download".into()) }));
                app.session.notices.clear();
                app.run("ui.saveItem", json!({"kind":kind,"layer":id})).unwrap();
                assert_eq!(app.session.notices.iter().filter(|n| n.text.starts_with("1 circle")).count(), usize::from(accept), "{kind}");
                assert_eq!(app.session.circle_spec(id, row), Some(b));
            }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn circle_edit_item_file_reports_construction_loss_only_after_successful_write() {
        let mut app = app();
        let (id, row, b) = create(&mut app);
        let dir = std::env::temp_dir().join(format!("rustgis-circle-item-save-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (path, expected) in [(None, false), (Some(dir.clone()), false), (Some(dir.join("saved.rgislyr")), true)] {
            let write_fails = path.as_ref() == Some(&dir);
            crate::dialogs::PICKED.with(|p| *p.borrow_mut() = path.map(|p| p.display().to_string()));
            app.session.notices.clear();
            let result = app.run("ui.saveItem", json!({"kind":"layer","layer":id}));
            assert_eq!(result.is_err(), write_fails);
            assert_eq!(app.session.notices.iter().filter(|n| n.text.starts_with("1 circle")).count(), usize::from(expected));
            assert_eq!(app.session.circle_spec(id, row), Some(b.clone()));
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn circle_edit_dragging_an_unselected_circle_selects_and_moves_it() {
        let mut app = app();
        let (id, row, b) = create(&mut app);
        app.run("select.clear", json!({})).unwrap();
        let p = at(&app, rect(), &b.frame, b.center).unwrap();
        assert!(drag_start(&mut app, rect(), p, false));
        drag_to(&mut app, rect(), p + vec2(20., 10.));
        drag_stop(&mut app);
        assert_eq!(app.session.map().layer(id).unwrap().selection, vec![row]);
        assert_eq!(app.session.map().layer(id).unwrap().dataset.features.len(), 1);
        assert_ne!(app.session.circle_spec(id, row).unwrap().center, b.center);
    }
    #[test]
    fn circle_edit_invalid_release_never_commits_the_last_valid_preview() {
        let mut app = app();
        let (id, row, b) = create(&mut app);
        let p = at(&app, rect(), &b.frame, b.center).unwrap();
        for force_new in [false, true] {
            let depth = app.session.undo_depth();
            let count = app.session.map().layer(id).unwrap().dataset.features.len();
            assert!(drag_start(&mut app, rect(), p, force_new));
            drag_to(&mut app, rect(), p + vec2(20., 10.));
            assert!(app.circles.preview.is_some());
            drag_to(&mut app, rect(), Pos2::new(f32::NAN, 0.));
            assert!(app.circles.preview.is_none());
            drag_stop(&mut app);
            assert_eq!(app.session.undo_depth(), depth);
            assert_eq!(app.session.map().layer(id).unwrap().dataset.features.len(), count);
            assert_eq!(app.session.circle_spec(id, row), Some(b.clone()));
            assert!(app.session.notices.last().is_some_and(|n| n.error && n.text.contains("construction grid")));
        }
    }
    #[test]
    fn circle_edit_browser_layer_download_reports_both_shape_kinds_only_after_acceptance() {
        for accept in [false, true] {
            let mut app = app();
            let (constructed, _, b) = create(&mut app);
            app.run("edit.addBox", json!({"layer":constructed,"box":{"center":[0,0],"width":100,"height":80,"angle":0}})).unwrap();
            let ordinary = app.session.map_mut().add_dataset(Dataset::new("Ordinary", GeometryType::Polygon, vec![]));
            app.run(
                "edit.addFeature",
                json!({"layer":ordinary,"geometry":rustgis_engine::data::io::geometry_to_geojson(&b.geometry().unwrap())}),
            )
            .unwrap();
            app.services.download = Some(Box::new(move |_, _| if accept { Ok(()) } else { Err("rejected download".into()) }));
            for layer in [constructed, ordinary] {
                app.session.notices.clear();
                let result = app.run("ui.exportLayer", json!({"layer":layer,"format":"geojson"}));
                if accept {
                    assert!(result.is_ok());
                }
                let has_notes = accept && layer == constructed;
                for prefix in ["1 circle", "1 rotated box"] {
                    assert_eq!(app.session.notices.iter().filter(|n| n.text.starts_with(prefix)).count(), usize::from(has_notes));
                }
                assert_eq!(app.session.circle_spec(constructed, 0), Some(b.clone()));
            }
        }
    }
}
