//! The touch interface for phones (#1902, phase 2): the map with a bar of large buttons under it
//! and sheets that rise over it, in place of the desktop's ribbon and panes.
//!
//! It is drawn around the egui front end's own map view and runs the same commands: a shell
//! turns it on with `app.chrome = Some(rustgis_ui_touch::chrome)`, and keeps it in `app.touch` so
//! the desktop interface's View › Touch Interface can bring it back. Dialogs, notices and the
//! Project page stay the front end's.
//!
//! What is here: the map, the bar (Layers, Add Data, Capture, More), the Layers sheet (show,
//! hide, zoom to, the active layer), the Capture sheet (a point, line or polygon placed by the cross
//! on the map, with a form for its layer's attributes that follows the layer's data design: value
//! lists, ranges and feature templates) and the More sheet (full extent, project, undo, the
//! desktop interface, the device's position). Where a shell reports the device's position
//! (`RustGisApp::position`), it is drawn on the map and the capture cross can be put on it.
//! The form marks the fields the layer's constraint rules require, narrows a field to the values
//! its contingent value sets leave it, has a calendar for dates, and keeps the field being typed
//! in above the soft keyboard (see [`form`]).
//! Its Edit mode picks a feature under the cross and moves, inserts or deletes its vertices, or
//! deletes it; with Snap on, the cross's place snaps to the map's features (see [`edit`]).
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use egui::{Align, Color32, CornerRadius, Layout, Rect, RichText, Sense, Stroke, Ui, Vec2};
use rustgis_ui_egui::rustgis_engine::platform::{TextEntry, TextKind, Typed};
use rustgis_ui_egui::theme::Tokens;
use rustgis_ui_egui::{RustGisApp, i18n, icons, tl};
use serde_json::{Value, json};

mod copies;
mod edit;
mod form;
mod photos;
mod track;

/// The smallest side of anything a finger taps, in points (Android's guidance is 48).
pub const TARGET: f32 = 48.0;
/// The bar's height: an icon over its label.
const BAR: f32 = 64.0;
/// The most of the window's height a sheet takes, so the map stays in view above it.
const SHEET_SHARE: f32 = 0.55;
/// The most a sheet takes while the soft keyboard is wanted: what is typed in matters more than
/// the map then, and the keyboard has taken much of the window.
const TYPING_SHARE: f32 = 0.9;
/// Below this many rows of room for the Capture sheet's body while a field is typed in, the rows
/// above its fields step aside so the field stays in sight.
const CROWDED_ROWS: f32 = 7.0;
/// The room above and below a sheet's rows.
const SHEET_MARGIN: f32 = 4.0;
/// The More sheet's rows with the position hidden.
const MORE_ROWS: usize = 9;
/// The More sheet's rows shown only with the position on: Go to My Position, and the track.
const MORE_POSITION_ROWS: usize = 2;

/// The sheet risen over the map.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Sheet {
    #[default]
    None,
    Layers,
    /// Capturing a feature where the cross on the map is.
    Capture,
    More,
}

/// The sheet shown, kept in egui's memory: the interface has no state of its own in the app.
fn sheet(ctx: &egui::Context) -> Sheet {
    ctx.data(|d| d.get_temp(egui::Id::unique("touch_sheet"))).unwrap_or_default()
}

fn set_sheet(ctx: &egui::Context, sheet: Sheet) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::unique("touch_sheet"), sheet));
}

pub use copies::{COPYING, Copying, copy_owner, mark_copying, set_copy_owner};

/// Whether unused copies of picked files are being removed (Remove Unused Copies): a phone's shell
/// holds files sent to the app until it is done, as one could open a project naming a copy the
/// removal has already judged unused.
pub fn removing_copies(ctx: &egui::Context) -> bool {
    copies::removing(ctx)
}

/// The touch interface (a `rustgis_ui_egui::Chrome`): the bar, the sheet asked for, and the map
/// in what is left.
pub fn chrome(app: &mut RustGisApp, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let t = Tokens::get(&ctx);
    let shown = sheet(&ctx);
    // Each position the shell reported goes to the average and the track being taken; while a
    // track is being recorded, positions keep coming with the app out of sight.
    track::take(app, &ctx);
    // What Remove Unused Copies found or removed, off the window's thread.
    copies::poll(app, &ctx);
    // What the system's text field sent back goes in the Capture form's field it was opened for.
    if !app.typed.is_empty() {
        let mut state = form(&ctx);
        let typed = std::mem::take(&mut app.typed);
        state.take_typed(app, typed);
        set_form(&ctx, state);
    }
    // With positioning off (Hide My Position, or Android ending it), an average being taken
    // stops: the sheet no longer shows it, and positions from before would mix into the next.
    if !app.locate {
        let mut state = track::field(&ctx);
        if state.averaging.take().is_some() {
            track::set_field(&ctx, state);
        }
    }
    // Only while positioning is on: Android ending it (or Hide) needs no service out of sight.
    // Changed, a frame follows at once, so the phone hears of it before the app leaves sight.
    let keep = app.locate && track::field(&ctx).track.is_some();
    if keep != app.keep_locating {
        app.keep_locating = keep;
        ctx.request_repaint();
    }
    let frame = egui::Frame::NONE.fill(t.pane).stroke(Stroke::new(1.0, t.border));
    egui::Panel::bottom("touch_bar").exact_size(BAR).frame(frame).show(ui, |ui| bar(app, ui, &t, shown));
    // The device's position off while a track is recorded (Android stopped it): the next
    // position starts a run of its own, the track not joined across the gap.
    let mut field = track::field(&ctx);
    if !app.locate
        && let Some(recording) = field.track.as_mut().filter(|t| !t.broken)
    {
        recording.broken = true;
        track::set_field(&ctx, field);
    }
    // A track being recorded says so over the bar, with its Stop, whatever sheet is up.
    if let Some(recording) = track::field(&ctx).track {
        let margin = egui::Margin::symmetric(12, 0);
        egui::Panel::bottom("touch_track").exact_size(TARGET).frame(frame.inner_margin(margin)).show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.label(RichText::new(track::track_text(&recording, app.locate)).size(14.0).color(t.text));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if action(ui, &t, tl!("Stop"), 88.0, true, true) {
                        stop_track(app, &ctx);
                    }
                    if !app.locate && action(ui, &t, tl!("Resume"), 96.0, false, true) {
                        app.locate = true;
                    }
                });
            });
        });
    }
    // Work running off the window's thread (a file being read, an export) shows its progress
    // and Cancel over the bar, where the desktop's status bar would.
    if app.background_busy() {
        egui::Panel::bottom("touch_busy").exact_size(TARGET).frame(frame.inner_margin(egui::Margin::symmetric(12, 0))).show(ui, |ui| {
            // Scrolled sideways when it is wider than the phone (a long address): its Cancel is
            // then reached, not cut off.
            egui::ScrollArea::horizontal().show(ui, |ui| ui.horizontal_centered(|ui| app.background_status(ui)));
        });
    }
    if shown != Sheet::None {
        // As tall as its rows, up to its share of the window: a longer list scrolls.
        let rows = match shown {
            Sheet::Layers => (app.session.map().layers.len() + app.session.map().tiles.len()).max(1),
            // Go to My Position is there only while the position is shown.
            // The position rows while it is shown; the track's row alone while one is recorded
            // with it off.
            Sheet::More => {
                MORE_ROWS
                    + if app.locate {
                        MORE_POSITION_ROWS
                    } else if track::field(&ctx).track.is_some() {
                        1
                    } else {
                        0
                    }
                    + usize::from(can_send_field_package(app))
                    + copies_rows(&ctx)
            }
            Sheet::Capture => capture_rows(app, &ctx),
            Sheet::None => 0,
        };
        let spacing = ui.spacing().item_spacing.y;
        let wanted = (rows + 1) as f32 * (TARGET + spacing) + 2.0 * SHEET_MARGIN;
        let share = if ctx.egui_wants_keyboard_input() { TYPING_SHARE } else { SHEET_SHARE };
        let height = wanted.min(ui.available_height() * share);
        let margin = egui::Margin::symmetric(8, SHEET_MARGIN as i8);
        egui::Panel::bottom("touch_sheet").resizable(false).exact_size(height).frame(frame.inner_margin(margin)).show(
            ui,
            |ui| match shown {
                Sheet::Layers => layers(app, ui, &t),
                Sheet::More => more(app, ui, &t),
                Sheet::Capture => capture(app, ui, &t),
                Sheet::None => {}
            },
        );
    }
    egui::CentralPanel::default().frame(egui::Frame::NONE.fill(t.canvas)).show(ui, |ui| {
        app.map_view(ui);
        // While capturing, a cross marks the middle of the map: the point goes there, so the
        // map is moved under the cross, which a finger does more exactly than a tap.
        // Where the device is, while its position is shown.
        if let (Some(fix), Some((map, _))) = (app.position, app.map.view_rect) {
            position(app, &ui.painter().with_clip_rect(map), fix, &t);
        }
        // The track being recorded, as it grows.
        if let (Some(recording), Some((map, _))) = (track::field(&ctx).track, app.map.view_rect) {
            let painter = ui.painter().with_clip_rect(map);
            // One line for each run of points the view shows (a point behind the globe breaks it,
            // as do a break in the track and a step across the antimeridian, which would be drawn
            // the long way round), a point on the pixel of the last left out: hours of a track
            // stay a few shapes.
            let mut runs: Vec<Vec<egui::Pos2>> = vec![Vec::new()];
            let mut before: Option<f64> = None;
            for (i, f) in recording.points.iter().enumerate() {
                let across = before.is_some_and(|lon| (f.lon - lon).abs() > 180.0);
                before = Some(f.lon);
                if (across || recording.runs.contains(&i)) && runs.last().is_some_and(|r| !r.is_empty()) {
                    runs.push(Vec::new());
                }
                let at = app.lonlat_to_screen(f.lon, f.lat);
                match (at, runs.last_mut()) {
                    (Some(p), Some(run)) if run.last().is_none_or(|q| q.distance(p) >= 1.0) => run.push(p),
                    (Some(_), _) => {}
                    (None, _) => runs.push(Vec::new()),
                }
            }
            for run in runs.into_iter().filter(|r| r.len() > 1) {
                painter.add(egui::Shape::line(run.clone(), Stroke::new(5.0, Color32::WHITE)));
                painter.add(egui::Shape::line(run, Stroke::new(3.0, t.green)));
            }
        }
        if shown == Sheet::Capture
            && let Some((map, _)) = app.map.view_rect
        {
            let painter = ui.painter().with_clip_rect(map);
            // The shape being drawn, kept while Edit is on but drawn only in Add; the feature
            // being edited, and where the cross snapped to.
            let state = form(&ctx);
            // A streamed layer's features put in place by the map view this frame, after the
            // sheet looked for the snap: one more frame looks again, the cross held still.
            // Only while there is a layer to capture into, whose sheet then looks again.
            if capture_layer(app, &state).is_some() && state.snap_for.is_some_and(|f| f.shapes != shapes(app)) {
                ctx.request_repaint();
            }
            if !state.edit_mode {
                sketch(app, &painter, map.center(), &t, &ctx);
            }
            if state.edit_mode
                && let (Some(target), Some(editing)) = (capture_layer(app, &state), state.editing)
                && let Some(shape) = edit::row_of(app, target.id, editing.feature).and_then(|row| edit::shape_ref(app, target.id, row))
            {
                edit::draw(app, &painter, &shape, editing.vertex, &t);
            }
            if let Some(snapped) = state.snap {
                edit::draw_snap(app, &painter, snapped, map.center(), &t);
            }
            cross(ui.painter(), map.center(), &t);
        }
    });
}

/// Starts recording a track, turning the device's position on when it is off.
fn start_track(app: &mut RustGisApp, ctx: &egui::Context) {
    let mut state = track::field(ctx);
    state.track = Some(track::Track::new());
    track::set_field(ctx, state);
    app.locate = true;
    app.session.notify(tl!("Recording a track: its positions are drawn on the map. Stop adds it to the Tracks layer."), false);
}

/// Stops the track being recorded and adds it to the map. A track that couldn't be added is kept
/// being recorded, and that is said, so its positions aren't lost: Stop tries again. Says whether
/// no track is being recorded now.
fn stop_track(app: &mut RustGisApp, ctx: &egui::Context) -> bool {
    let mut state = track::field(ctx);
    let Some(recorded) = state.track.take() else { return true };
    track::set_field(ctx, state);
    let Err(kept) = track::finish(app, recorded) else { return true };
    let mut state = track::field(ctx);
    state.track = Some(kept);
    track::set_field(ctx, state);
    app.session.notify(tl!("The track is kept and still recorded: Stop tries to add it again."), true);
    false
}

/// Field provenance survives a tab rename. Old field packages can still have just the title. A
/// field package goes back with its one map only, so a field project with a map added doesn't
/// offer it (sending would refuse it).
fn can_send_field_package(app: &RustGisApp) -> bool {
    let map = app.session.map();
    let project = &app.session.project;
    if project.field_origin.is_some() {
        return project.maps.len() == 1;
    }
    // An area ID with no area (a damaged file) is no copy to send.
    let area = map.offline_area_id.as_deref().map(|id| project.offline_areas.iter().any(|a| a.id == id));
    area.unwrap_or_else(|| map.name.ends_with(rustgis_ui_egui::rustgis_engine::cmd::field::OFFLINE))
}

/// Draws where the device is: a dot, in a ring as wide as the position is good to.
fn position(app: &RustGisApp, painter: &egui::Painter, fix: rustgis_ui_egui::rustgis_engine::platform::Fix, t: &Tokens) {
    let Some(at) = app.lonlat_to_screen(fix.lon, fix.lat) else { return };
    // The ground a point of the screen covers where the fix is, not at the middle of the view:
    // on a projection whose scale changes with latitude the two differ.
    let map = app.session.map();
    let here = map.to_map(rustgis_ui_egui::rustgis_engine::geom::Coord::new(fix.lon, fix.lat));
    let metres_a_point = here.map_or(0.0, |at| map.camera.res * map.ground_per_unit(at));
    // A ring wider than the view has no edge in it to show, and an accuracy out of all measure
    // (it is the system's to give) would make one of no size a painter can take.
    let widest = painter.clip_rect().size().length();
    if let Some(radius) = fix.accuracy.filter(|_| metres_a_point > 0.0).map(|m| (m / metres_a_point) as f32)
        && radius > 9.0
        && radius <= widest
    {
        painter.circle(at, radius, t.accent.gamma_multiply(0.15), Stroke::new(1.0, t.accent.gamma_multiply(0.6)));
    }
    painter.circle(at, 8.0, Color32::WHITE, Stroke::NONE);
    painter.circle(at, 6.0, t.accent, Stroke::NONE);
}

/// Moves the map so the device's position is in its middle, under the capture cross. Says so
/// when there is no position yet.
fn go_to_position(app: &mut RustGisApp) {
    match app.position {
        Some(fix) => run(app, "map.goToXY", json!({"lon": fix.lon, "lat": fix.lat})),
        None => app.session.notify(tl!("This device's position isn't known yet."), false),
    }
}

/// The capture cross: dark with a light edge, to show over any map.
fn cross(painter: &egui::Painter, at: egui::Pos2, t: &Tokens) {
    let arm = 14.0;
    for (width, ink) in [(4.0, Color32::WHITE), (2.0, t.accent)] {
        let stroke = Stroke::new(width, ink);
        painter.line_segment([at - Vec2::new(arm, 0.0), at + Vec2::new(arm, 0.0)], stroke);
        painter.line_segment([at - Vec2::new(0.0, arm), at + Vec2::new(0.0, arm)], stroke);
    }
}

/// The bar: one large button for each thing done most, the sheet's own button marked while its
/// sheet is up.
fn bar(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens, shown: Sheet) {
    let ctx = ui.ctx().clone();
    let toggle = |to: Sheet| set_sheet(&ctx, if shown == to { Sheet::None } else { to });
    ui.columns(4, |cols| {
        if let [a, b, c, d] = cols {
            if bar_button(a, t, "contents", tl!("Layers"), shown == Sheet::Layers) {
                toggle(Sheet::Layers);
            }
            if bar_button(b, t, "add_data", tl!("Add Data"), false) {
                set_sheet(&ctx, Sheet::None);
                run(app, "ui.addData", Value::Null);
            }
            if bar_button(c, t, "vertices", tl!("Capture"), shown == Sheet::Capture) {
                toggle(Sheet::Capture);
            }
            if bar_button(d, t, "command", tl!("More"), shown == Sheet::More) {
                toggle(Sheet::More);
            }
        }
    });
}

/// One of the bar's buttons, as wide as its column: the icon over the label. Says whether it
/// was tapped.
fn bar_button(ui: &mut Ui, t: &Tokens, icon: &str, label: &str, on: bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), BAR), Sense::click());
    let response = response.on_hover_text(label);
    if on || response.is_pointer_button_down_on() {
        ui.painter().rect_filled(rect.shrink(4.0), CornerRadius::same(8), if on { t.checked } else { t.pressed });
    }
    let side = 26.0;
    let at = Rect::from_center_size(rect.center() - Vec2::new(0.0, 9.0), Vec2::splat(side));
    icons::paint(ui.painter(), at, icon, if on { t.accent_text } else { t.icon }, t.accent);
    let ink = if on { t.accent_text } else { t.text };
    ui.painter().text(rect.center() + Vec2::new(0.0, 19.0), egui::Align2::CENTER_CENTER, label, egui::FontId::proportional(12.0), ink);
    response.clicked()
}

/// A sheet's heading, with the button that lowers it.
fn heading(ui: &mut Ui, t: &Tokens, title: &str) {
    ui.horizontal(|ui| {
        ui.set_min_height(TARGET);
        ui.label(RichText::new(title).size(17.0).strong().color(t.text));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if icon_button(ui, t, "close", tl!("Close")) {
                set_sheet(ui.ctx(), Sheet::None);
            }
        });
    });
}

/// A square button of the smallest size a finger taps, showing an icon. Says whether it was
/// tapped.
fn icon_button(ui: &mut Ui, t: &Tokens, icon: &str, hint: &str) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(TARGET), Sense::click());
    let response = response.on_hover_text(hint);
    if response.is_pointer_button_down_on() {
        ui.painter().rect_filled(rect.shrink(4.0), CornerRadius::same(8), t.pressed);
    }
    icons::paint(ui.painter(), Rect::from_center_size(rect.center(), Vec2::splat(22.0)), icon, t.icon, t.accent);
    response.clicked()
}

/// A row of the Layers sheet: a feature or raster layer by its id, a tile layer by its place.
#[derive(Clone, Copy)]
enum Row {
    Layer(u32),
    Tile(usize),
}

/// The Layers sheet: the active map's layers, then its tile layers, each list top first. A row's
/// eye shows or hides its layer, its name makes it the one acted on, and its last button zooms
/// to it.
fn layers(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    heading(ui, t, tl!("Layers"));
    // Read first: a row's button runs a command, which changes the lists it is drawn from.
    let map = app.session.map();
    let active = app.session.active_layer;
    let picked = app.session.selected_tile();
    let mut rows: Vec<(Row, String, bool, bool)> =
        map.layers.iter().map(|l| (Row::Layer(l.id), l.name.clone(), l.visible, active == Some(l.id))).collect();
    rows.extend(map.tiles.iter().enumerate().map(|(i, tile)| (Row::Tile(i), tile.name.clone(), tile.visible, picked == Some(i))));
    if rows.is_empty() {
        ui.label(RichText::new(tl!("This map has no layers yet. Add Data puts one on it.")).color(t.text_dim));
        ui.add_space(8.0);
        return;
    }
    egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
        for (row, name, visible, chosen) in rows {
            ui.horizontal(|ui| {
                ui.set_min_height(TARGET);
                let eye = if visible { tl!("Hide this layer") } else { tl!("Show this layer") };
                if toggle_button(ui, t, "eye", eye, visible) {
                    match row {
                        Row::Layer(id) => run(app, "layer.visibility", json!({"layer": id, "visible": !visible})),
                        Row::Tile(index) => run(app, "map.setTiles", json!({"index": index, "visible": !visible})),
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if icon_button(ui, t, "zoom_full", tl!("Zoom to this layer")) {
                        match row {
                            Row::Layer(id) => run(app, "layer.zoomTo", json!({"layer": id})),
                            Row::Tile(index) => run(app, "map.zoomToTiles", json!({"index": index})),
                        }
                    }
                    // The name takes the room between the two buttons, cut short when it is long.
                    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), TARGET), Sense::click());
                    if chosen {
                        ui.painter().rect_filled(rect, CornerRadius::same(6), t.checked);
                    }
                    let ink = if visible { t.text } else { t.text_disabled };
                    let text = ui.painter().layout_no_wrap(name.clone(), egui::FontId::proportional(15.0), ink);
                    let clip = ui.painter().with_clip_rect(rect.shrink2(Vec2::new(8.0, 0.0)));
                    clip.galley(egui::pos2(rect.min.x + 8.0, rect.center().y - text.size().y / 2.0), text, ink);
                    if response.on_hover_text(name).clicked() {
                        match row {
                            Row::Layer(id) => run(app, "layer.select", json!({"layer": id})),
                            Row::Tile(index) => run(app, "map.selectTiles", json!({"index": index})),
                        }
                    }
                });
            });
        }
    });
}

/// An icon button that shows a state: drawn dim when `on` is false. Says whether it was tapped.
fn toggle_button(ui: &mut Ui, t: &Tokens, icon: &str, hint: &str, on: bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(TARGET), Sense::click());
    let response = response.on_hover_text(hint);
    if response.is_pointer_button_down_on() {
        ui.painter().rect_filled(rect.shrink(4.0), CornerRadius::same(8), t.pressed);
    }
    let ink = if on { t.icon } else { t.text_disabled };
    icons::paint(
        ui.painter(),
        Rect::from_center_size(rect.center(), Vec2::splat(22.0)),
        icon,
        ink,
        if on { t.accent } else { Color32::TRANSPARENT },
    );
    response.clicked()
}

/// What the Capture sheet holds between frames: the layer captured into, what is typed for the
/// next feature's fields (by field name), and the vertices placed so far of a line or polygon.
#[derive(Clone, Default)]
struct Form {
    /// The layer a chip chose; `None` follows the active layer. A layer is named here by its
    /// `Layer::instance_id`, not its id: ids start again in each map and each project, and a
    /// choice or a draft must never pass to another map's layer of the same id.
    layer: Option<u64>,
    /// The layer the fields were last shown for: what is typed, and what is drawn, belong to
    /// it alone.
    shown: Option<u64>,
    values: std::collections::BTreeMap<String, String>,
    /// The layer's feature template chosen for the next feature, by name: its defaults fill
    /// the fields the form leaves empty.
    template: Option<String>,
    /// The vertices of the line or polygon being drawn, as longitude and latitude, in order.
    vertices: Vec<(f64, f64)>,
    /// The date field whose calendar is open, by name, and the month it shows.
    calendar: Option<(String, form::Month)>,
    /// The sheet edits the shapes on the map instead of adding new ones.
    edit_mode: bool,
    /// The feature being edited, in Edit mode.
    editing: Option<edit::Editing>,
    /// Where the cross's place snapped to on the last frame, with snapping on.
    snap: Option<edit::Snapped>,
    /// The photos taken for the next feature, by where they were written. Kept when the layer
    /// changes: they are of the place, not of a layer.
    photos: Vec<std::path::PathBuf>,
    /// What [`Form::snap`] was found for: it is looked for again only when the cross moved, the
    /// map zoomed, the vertex being moved changed, the session changed or a layer's shapes did
    /// (a streamed layer's features put in place).
    snap_for: Option<SnapFor>,
    /// The height of the window the sheet was last drawn in: when it changes (the soft keyboard
    /// came or went), the field being typed in is scrolled back into view.
    window: f32,
    /// The system's text field open for one of the fields ([`Services::enter_text`]), until it
    /// closes.
    ///
    /// [`Services::enter_text`]: rustgis_ui_egui::Services::enter_text
    entering: Option<Entering>,
    /// How many times the system's text field was opened: each opening's id is its own.
    entries: u64,
}

/// The system's text field open for a field of the Capture form.
#[derive(Clone, Debug, PartialEq)]
struct Entering {
    /// The id it was opened with ([`entry_id`]).
    id: String,
    /// The layer whose field it is, by `Layer::instance_id`.
    instance: u64,
    /// The field's name.
    field: String,
}

impl Form {
    /// The form for the layer `instance`: what was typed or drawn for another layer is dropped,
    /// so one layer's text never goes into another's field of the same name, nor its vertices
    /// into another's feature.
    fn for_layer(mut self, instance: u64) -> Form {
        if self.shown != Some(instance) {
            self.values.clear();
            self.vertices.clear();
            self.template = None;
            self.calendar = None;
            self.editing = None;
            self.snap = None;
            self.snap_for = None;
            self.shown = Some(instance);
        }
        self
    }

    /// Takes what the system's text field sent back: the text of the one this form opened goes
    /// in its field as it changes, until it closes, while Capture still adds to that field's layer
    /// and the layer still has the field. Text that no longer has its field is said, once, and
    /// what follows it let go. What an earlier opening sends (its close, arriving after the field
    /// was opened again) belongs to no field here.
    fn take_typed(&mut self, app: &mut RustGisApp, typed: Vec<Typed>) {
        for typed in typed {
            let Some(open) = self.entering.clone() else { continue };
            match typed {
                Typed::Text { id, text } if id == open.id => {
                    let here = capture_layer(app, self).filter(|t| t.instance == open.instance && self.shown == Some(t.instance));
                    let has_field =
                        here.and_then(|t| app.session.map().layer(t.id)).is_some_and(|l| l.dataset.field_index(&open.field).is_some());
                    if has_field {
                        self.values.insert(open.field, text);
                    } else {
                        self.entering = None;
                        app.session.notify(
                            i18n::fmt(
                                "What was typed for {field} wasn't kept: Capture no longer adds to its layer, or the layer no longer has the field.",
                                &[("field", &open.field)],
                            ),
                            true,
                        );
                    }
                }
                Typed::Closed { id, .. } if id == open.id => self.entering = None,
                _ => {}
            }
        }
    }
}

/// The id the system's text field is opened with for the form's field `index` of the layer
/// `instance` (`Layer::instance_id`), the `n`th time it was opened: an answer for one layer's
/// field never fills another's, nor an earlier opening's close ends a later one. No name of the
/// user's goes in it, so it never holds a tab, which would end it on the event's line.
fn entry_id(instance: u64, index: usize, n: u64) -> String {
    format!("capture:{instance}:{index}:{n}")
}

impl Form {
    /// Places a vertex of the shape being drawn at `at`, unless the last one is there already
    /// (the button tapped twice with the map not moved): a vertex on the one before it adds
    /// nothing to the shape, and two of three in one place make a polygon of no area. Says
    /// whether it was placed.
    fn place(&mut self, at: (f64, f64)) -> bool {
        if self.vertices.last() == Some(&at) {
            return false;
        }
        self.vertices.push(at);
        true
    }
}

/// What the cross's place was snapped for: its place, the vertex being moved, the session's
/// revision, the layers' geometry revisions summed (a streamed layer's features are put in place
/// by the map view without a change to the session), the map's resolution and whether snapping
/// was on.
#[derive(Clone, Copy, Debug, PartialEq)]
struct SnapFor {
    at: Option<(f64, f64)>,
    moving: Option<(u32, usize, rustgis_ui_egui::rustgis_engine::data::VertexId)>,
    revision: u64,
    shapes: u64,
    res: f64,
    on: bool,
}

/// The map's layers' geometry revisions, summed: it moves whenever a layer's shapes change.
fn shapes(app: &RustGisApp) -> u64 {
    app.session.map().layers.iter().fold(0u64, |sum, l| sum.wrapping_add(l.geometry_rev))
}

fn form(ctx: &egui::Context) -> Form {
    ctx.data(|d| d.get_temp(egui::Id::unique("touch_capture"))).unwrap_or_default()
}

fn set_form(ctx: &egui::Context, form: Form) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::unique("touch_capture"), form));
}

use rustgis_ui_egui::rustgis_engine::data::GeometryType;

/// A layer a feature can be captured into.
#[derive(Clone)]
struct Target {
    id: u32,
    name: String,
    instance: u64,
    kind: GeometryType,
}

/// The map's point, line and polygon layers, top first: what a feature can be captured into.
fn capture_layers(app: &RustGisApp) -> Vec<Target> {
    app.session
        .map()
        .layers
        .iter()
        .filter(|l| matches!(l.geometry_type(), GeometryType::Point | GeometryType::Polyline | GeometryType::Polygon))
        .map(|l| Target { id: l.id, name: l.name.clone(), instance: l.instance_id(), kind: l.geometry_type() })
        .collect()
}

/// The layer the Capture sheet captures into: the one chosen in it while it is still a layer of
/// this map that takes features, else the active layer when it is one, else the top such layer.
fn capture_layer(app: &RustGisApp, form: &Form) -> Option<Target> {
    let layers = capture_layers(app);
    let chosen = form.layer.and_then(|instance| layers.iter().find(|l| l.instance == instance));
    let active = app.session.active_layer.and_then(|id| layers.iter().find(|l| l.id == id));
    chosen.or(active).or(layers.first()).cloned()
}

/// The Capture sheet's rows, for its height: its layers, its fields, where the cross is, and
/// its buttons (two rows of them for a line or polygon).
fn capture_rows(app: &RustGisApp, ctx: &egui::Context) -> usize {
    let state = form(ctx);
    let Some(target) = capture_layer(app, &state) else { return 2 };
    let layer = app.session.map().layer(target.id);
    let fields = layer
        .map_or(0, |l| l.dataset.fields.len() + usize::from(!l.design.templates.is_empty()) + usize::from(!form::required(l).is_empty()));
    // The mode row, then in Edit mode its words, its buttons and the layers; else the form.
    if state.edit_mode {
        let buttons = match (state.editing, target.kind) {
            (None, _) => 1,
            (Some(_), GeometryType::Point) => 2,
            (Some(_), _) => 3,
        };
        return 4 + buttons + usize::from(app.locate) + usize::from(!state.photos.is_empty());
    }
    // A date and time's calendar has its Now row too.
    let calendar = match (&state.calendar, layer) {
        (Some((name, _)), Some(l)) => {
            let timed =
                l.dataset.fields.iter().any(|f| &f.name == name && f.ty == rustgis_ui_egui::rustgis_engine::data::FieldType::DateTime);
            form::CALENDAR_ROWS + usize::from(timed)
        }
        _ => 0,
    };
    let averaging = usize::from(app.locate && track::field(ctx).averaging.is_some());
    let photo_row = usize::from(app.services.take_photo.is_some() || !state.photos.is_empty());
    1 + averaging + photo_row + fields + calendar + if target.kind == GeometryType::Point { 3 } else { 4 } + usize::from(app.locate)
}

/// A value typed for a field, as the command takes it: a number for a field of numbers, a truth
/// value for a yes/no field, the text otherwise. Nothing typed is no value. Text that isn't a
/// number, in a field of numbers, is an error here: the command would store nothing for it, and
/// the feature would be saved without what was typed.
fn typed(text: &str, ty: rustgis_ui_egui::rustgis_engine::data::FieldType) -> Result<Value, ()> {
    use rustgis_ui_egui::rustgis_engine::data::FieldType;
    let text = text.trim();
    if text.is_empty() {
        return Ok(Value::Null);
    }
    match ty {
        FieldType::Integer => text.parse::<i64>().map(Value::from).map_err(|_| ()),
        FieldType::Double => text.parse::<f64>().ok().filter(|n| n.is_finite()).map(Value::from).ok_or(()),
        FieldType::Boolean => match text {
            "true" => Ok(Value::from(true)),
            "false" => Ok(Value::from(false)),
            _ => Err(()),
        },
        // A date the field can't hold is refused by the command, by name.
        FieldType::Text | FieldType::Date | FieldType::DateTime => Ok(Value::from(text)),
    }
}

/// Where the cross is: the middle of the map, as longitude and latitude. `None` where the map's
/// projection has no such place for it (the middle of the view is off the projected world).
fn cross_place(app: &RustGisApp) -> Option<(f64, f64)> {
    let map = app.session.map();
    map.to_lonlat(map.camera.center).map(|at| (at.x, at.y))
}

/// The fewest vertices a feature of `kind` is drawn with.
fn fewest(kind: GeometryType) -> usize {
    match kind {
        GeometryType::Polygon => 3,
        GeometryType::Polyline => 2,
        _ => 1,
    }
}

/// The GeoJSON geometry of a feature of `kind` through `vertices` (longitude and latitude, in
/// order): a point at the first, a line through them, or a polygon whose ring closes on the
/// first. A polygon closed by hand (its last vertex placed on its first) is not closed twice.
/// `None` with fewer than the kind needs.
fn geometry(kind: GeometryType, vertices: &[(f64, f64)]) -> Option<Value> {
    let vertices = match vertices {
        [first, middle @ .., last] if kind == GeometryType::Polygon && first == last && !middle.is_empty() => {
            &vertices[..vertices.len() - 1]
        }
        _ => vertices,
    };
    if vertices.len() < fewest(kind) {
        return None;
    }
    let at = |(lon, lat): &(f64, f64)| json!([lon, lat]);
    match kind {
        GeometryType::Point => vertices.first().map(|v| json!({"type": "Point", "coordinates": at(v)})),
        GeometryType::Polyline => Some(json!({"type": "LineString", "coordinates": vertices.iter().map(at).collect::<Vec<_>>()})),
        GeometryType::Polygon => {
            let mut ring: Vec<Value> = vertices.iter().map(at).collect();
            ring.push(at(vertices.first()?));
            Some(json!({"type": "Polygon", "coordinates": [ring]}))
        }
        GeometryType::None => None,
    }
}

/// Adds a feature to layer `id` with `geometry` and what the form holds, turning editing on for
/// the map first when it is off (and saying so). Says whether it was added. It isn't, and the
/// reason is said, when a field of numbers holds text that isn't one, or when the command
/// refuses.
fn add_feature(app: &mut RustGisApp, id: u32, form: &Form, geometry: Value) -> bool {
    let Some(layer) = app.session.map().layer(id) else { return false };
    let (name, kind) = (layer.name.clone(), layer.geometry_type());
    let mut attributes = serde_json::Map::new();
    let mut not_numbers = Vec::new();
    for field in &layer.dataset.fields {
        let Some(text) = form.values.get(&field.name) else { continue };
        match typed(text, field.ty) {
            Ok(Value::Null) => {}
            Ok(value) => {
                attributes.insert(field.name.clone(), value);
            }
            Err(()) => not_numbers.push(field.name.clone()),
        }
    }
    if !not_numbers.is_empty() {
        let fields = not_numbers.join(", ");
        app.session.notify(
            rustgis_ui_egui::i18n::fmt("Nothing was added. Not a value the field can hold, in: {fields}.", &[("fields", &fields)]),
            true,
        );
        return false;
    }
    // A field a rule requires, left empty and not filled by the template or the subtype chosen,
    // is named here: the rule's own refusal would only quote its expression.
    let empty: Vec<String> = form::required(layer)
        .into_iter()
        .filter(|name| !attributes.contains_key(name) && !defaulted(layer, form, name))
        // The photos taken fill the photos field.
        .filter(|name| form.photos.is_empty() || photos::field(layer).as_deref() != Some(name.as_str()))
        .map(|name| layer.dataset.fields.iter().find(|f| f.name == name).map_or(name.clone(), |f| f.display().to_string()))
        .collect();
    if !empty.is_empty() {
        let fields = empty.join(", ");
        app.session.notify(rustgis_ui_egui::i18n::fmt("Nothing was added. Fill in: {fields}.", &[("fields", &fields)]), true);
        return false;
    }
    if !app.session.map().editing.settings.enabled {
        if app.run_by_user("edit.enable", json!({"on": true})).is_err() {
            return false;
        }
        app.session.notify(tl!("Editing was turned on for this map."), false);
    }
    // The photos taken for it go in the layer's photos field, made when it has none.
    let mut photos_in = None;
    if !form.photos.is_empty() {
        let Some((field, made)) = photos::ensure_field(app, id) else { return false };
        // After what was typed in the field, if it was.
        let typed = attributes.get(&field).and_then(Value::as_str).unwrap_or_default().to_string();
        let (text, count) = photos::joined(&typed, &form.photos);
        attributes.insert(field.clone(), json!(text));
        photos_in = Some((field, made, count));
    }
    let mut feature = json!({"layer": id, "geometry": geometry, "attributes": attributes});
    // A template chosen while the layer still has it: one removed since is not asked for.
    let template = form
        .template
        .as_ref()
        .filter(|name| app.session.map().layer(id).is_some_and(|l| l.design.templates.iter().any(|t| &t.name == *name)));
    if let Some(name) = template {
        feature["template"] = json!(name);
    }
    if app.run_by_user("edit.addFeature", feature).is_err() {
        // A photos field made for a feature that then wasn't added goes again: a refused add
        // leaves the layer's design as it was.
        if let Some((field, true, _)) = &photos_in
            && app.run_by_user("edit.deleteField", json!({"layer": id, "field": field})).is_ok()
        {
            app.session
                .notify(rustgis_ui_egui::i18n::fmt("The {field} field added for it was taken away again.", &[("field", field)]), false);
        }
        return false;
    }
    let added = match kind {
        GeometryType::Polyline => rustgis_ui_egui::i18n::fmt("Added a line to {layer}.", &[("layer", &name)]),
        GeometryType::Polygon => rustgis_ui_egui::i18n::fmt("Added a polygon to {layer}.", &[("layer", &name)]),
        _ => rustgis_ui_egui::i18n::fmt("Added a point to {layer}.", &[("layer", &name)]),
    };
    // Where its photos went: the layer's own field of that name, maybe.
    let added = match photos_in {
        Some((field, _, count)) => rustgis_ui_egui::i18n::fmt(
            "{added} Its {n} photo(s) are named in its {field} field.",
            &[("added", &added), ("n", &count.to_string()), ("field", &field)],
        ),
        None => added,
    };
    app.session.notify(added, false);
    true
}

/// Whether the field `name` of a new feature takes a value from what the form chose: the default
/// of the feature template chosen, or of the subtype its subtype field holds.
fn defaulted(layer: &rustgis_ui_egui::rustgis_engine::map::Layer, form: &Form, name: &str) -> bool {
    let has = |defaults: &[(String, rustgis_ui_egui::rustgis_engine::data::Value)]| {
        defaults.iter().any(|(field, value)| field.eq_ignore_ascii_case(name) && !value.is_null())
    };
    let template = form.template.as_ref().and_then(|t| layer.design.templates.iter().find(|x| &x.name == t));
    // The subtype the form chose, or else the one the template gives, as the engine reads it.
    let subtype = layer.design.subtypes.as_ref().and_then(|s| {
        let typed = form.values.iter().find(|(field, v)| field.eq_ignore_ascii_case(&s.field) && !v.trim().is_empty());
        let code = match typed {
            Some((_, v)) => v.trim().parse::<i64>().ok()?,
            None => template?.defaults.iter().find(|(field, _)| field.eq_ignore_ascii_case(&s.field))?.1.as_f64()? as i64,
        };
        s.subtypes.iter().find(|x| x.code == code)
    });
    template.is_some_and(|t| has(&t.defaults)) || subtype.is_some_and(|s| has(&s.defaults))
}

/// Adds a point to layer `id` where the middle of the map is (where it snapped to, with snapping
/// on), with what the form holds. A cross on no place of the map's projection adds none, and says
/// so.
fn add_point(app: &mut RustGisApp, id: u32, form: &Form) -> bool {
    let Some(at) = form.snap.map(|s| (s.lon, s.lat)).or_else(|| cross_place(app)) else {
        app.session.notify(tl!("Nothing was added: the cross is off the map. Move the map under it."), true);
        return false;
    };
    match geometry(GeometryType::Point, &[at]) {
        Some(point) => add_feature(app, id, form, point),
        None => false,
    }
}

/// The values a field takes from a list, as (what is stored, what is shown): the codes of its
/// coded-value domain, the layer's subtypes for the subtype field, or yes and no for a yes/no
/// field. `None` for a field that is typed into.
fn choices(
    layer: &rustgis_ui_egui::rustgis_engine::map::Layer,
    field: &rustgis_ui_egui::rustgis_engine::data::Field,
) -> Option<Vec<(String, String)>> {
    use rustgis_ui_egui::rustgis_engine::data::FieldType;
    use rustgis_ui_egui::rustgis_engine::map::design::DomainKind;
    let same = |name: &str| name.eq_ignore_ascii_case(&field.name);
    if let Some(subtypes) = layer.design.subtypes.as_ref().filter(|s| same(&s.field)) {
        return Some(subtypes.subtypes.iter().map(|s| (s.code.to_string(), s.name.clone())).collect());
    }
    if let Some(DomainKind::Coded(codes)) = layer.design.domains.iter().find(|d| same(&d.field)).map(|d| &d.kind) {
        // A code of no text is left out: it would be stored as nothing, and its chip would show
        // as chosen whenever nothing is.
        let list: Vec<(String, String)> = codes
            .iter()
            .map(|(code, says)| (code.to_string(), says))
            .filter(|(code, _)| !code.is_empty())
            .map(|(code, says)| if says.trim().is_empty() { (code.clone(), code) } else { (code, says.clone()) })
            .collect();
        // A list with nothing left to offer is no list: the field is typed into.
        if !list.is_empty() {
            return Some(list);
        }
    }
    (field.ty == FieldType::Boolean)
        .then(|| vec![("true".to_string(), tl!("Yes").to_string()), ("false".to_string(), tl!("No").to_string())])
}

/// A coded domain's codes with their descriptions, as (what is stored, what is shown); none for a
/// range.
fn described(domain: &rustgis_ui_egui::rustgis_engine::map::design::Domain) -> Vec<(String, String)> {
    use rustgis_ui_egui::rustgis_engine::map::design::DomainKind;
    match &domain.kind {
        DomainKind::Coded(codes) => codes.iter().map(|(code, says)| (form::stored(code), says.clone())).collect(),
        DomainKind::Range { .. } => Vec::new(),
    }
}

/// The limits of a field's range domain, when it has one.
fn range_of(layer: &rustgis_ui_egui::rustgis_engine::map::Layer, field: &str) -> Option<(f64, f64)> {
    use rustgis_ui_egui::rustgis_engine::map::design::DomainKind;
    layer.design.domains.iter().find(|d| d.field.eq_ignore_ascii_case(field)).and_then(|d| match d.kind {
        DomainKind::Range { min, max } => Some((min, max)),
        DomainKind::Coded(_) => None,
    })
}

/// A row of chips, one for each of `list` (what is stored, what is shown), scrolled sideways when
/// it is wider than the sheet. The one whose stored value is `value` is marked; tapping another
/// makes it the value, and tapping the marked one empties the value, so "nothing chosen" shows
/// as no chip marked.
fn chips(ui: &mut Ui, t: &Tokens, salt: impl egui::AsIdSalt, list: &[(String, String)], value: &mut String) {
    egui::ScrollArea::horizontal().id_salt(salt).show(ui, |ui| {
        ui.horizontal(|ui| {
            for (stored, shown) in list {
                let on = stored == value;
                let text = RichText::new(shown).size(15.0).color(if on { t.accent_text } else { t.text });
                let chip = egui::Button::new(text).fill(if on { t.checked } else { t.input }).min_size(Vec2::new(TARGET, TARGET - 8.0));
                if ui.add(chip).clicked() {
                    *value = if on { String::new() } else { stored.clone() };
                }
            }
        });
    });
}

/// Draws the line or polygon being captured over the map: its vertices, the line through them,
/// and a thinner one on to the cross, where the next vertex would go (and, for a polygon, back to
/// the first).
fn sketch(app: &RustGisApp, painter: &egui::Painter, cross: egui::Pos2, t: &Tokens, ctx: &egui::Context) {
    let state = form(ctx);
    let Some(target) = capture_layer(app, &state) else { return };
    if target.kind == GeometryType::Point || state.vertices.is_empty() {
        return;
    }
    // A vertex the view doesn't show (behind the globe) breaks the line there.
    let placed: Vec<Option<egui::Pos2>> = state.vertices.iter().map(|(lon, lat)| app.lonlat_to_screen(*lon, *lat)).collect();
    for pair in placed.windows(2) {
        if let [Some(a), Some(b)] = pair {
            painter.line_segment([*a, *b], Stroke::new(5.0, Color32::WHITE));
            painter.line_segment([*a, *b], Stroke::new(3.0, t.accent));
        }
    }
    let next = Stroke::new(1.5, t.accent);
    if let Some(Some(last)) = placed.last() {
        painter.line_segment([*last, cross], next);
    }
    if target.kind == GeometryType::Polygon
        && placed.len() > 1
        && let Some(Some(first)) = placed.first()
    {
        painter.line_segment([cross, *first], next);
    }
    for at in placed.into_iter().flatten() {
        painter.circle(at, 5.0, Color32::WHITE, Stroke::new(2.0, t.accent));
    }
}

/// A button of the Capture sheet, `width` wide and a finger tall: filled with the accent when it
/// is the one to tap next. Says whether it was tapped.
fn action(ui: &mut Ui, t: &Tokens, label: &str, width: f32, main: bool, enabled: bool) -> bool {
    let text = RichText::new(label).size(16.0).color(if main { t.on_accent } else { t.text });
    let button = egui::Button::new(text).fill(if main { t.accent } else { t.input }).wrap();
    ui.add_enabled_ui(enabled, |ui| ui.add_sized(Vec2::new(width, TARGET), button)).inner.clicked()
}

/// The Capture sheet: the layer to capture into, where the cross is, the buttons that capture
/// there, and a field for each of the layer's attributes. A point is added where the cross is.
/// A line or polygon is drawn a vertex at a time and added by Finish. Keep the actions pinned
/// while at least one field row fits below them. A shorter sheet scrolls as a single body.
fn capture(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    heading(ui, t, tl!("Capture"));
    let ctx = ui.ctx().clone();
    let available = ui.available_height();
    let crowded = ctx.egui_wants_keyboard_input() && available < CROWDED_ROWS * TARGET;
    // Measure the drawn controls so location, edit mode and wrapped labels all count.
    let layout_id = egui::Id::unique("touch_capture_layout");
    let previous = ctx.data(|data| data.get_temp::<(f32, bool)>(layout_id));
    let form_room = TARGET + ui.spacing().item_spacing.y;
    let fits = |actions_height| actions_height + form_room <= available;
    let scroll_all = crowded || previous.is_none_or(|(height, _)| !fits(height));
    let layout_changed = previous.is_none_or(|(_, scrolled)| scrolled != scroll_all);
    let height = if scroll_all {
        egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| capture_body(app, ui, t, crowded, true, layout_changed)).inner
    } else {
        capture_body(app, ui, t, crowded, false, layout_changed)
    };
    ctx.data_mut(|data| data.insert_temp(layout_id, (height, scroll_all)));
    if scroll_all != (crowded || !fits(height)) {
        ctx.request_repaint();
    }
}

/// Draws the Capture controls and form once, then returns the height needed to pin the controls.
fn capture_body(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens, crowded: bool, scroll_all: bool, layout_changed: bool) -> f32 {
    let start = ui.next_widget_position().y;
    let ctx = ui.ctx().clone();
    let mut state = form(&ctx);
    let layers = capture_layers(app);
    let Some(target) = capture_layer(app, &state) else {
        ui.label(RichText::new(tl!("This map has no layer to capture into. Add Data puts one on it.")).color(t.text_dim));
        ui.add_space(8.0);
        return ui.next_widget_position().y - start;
    };
    state = state.for_layer(target.instance);
    // A template chosen and since removed from the layer is let go, and said, before a button can
    // add a feature without the defaults the form still seemed to promise: nothing is added on
    // the frame that finds it, so the next tap is made knowing.
    let on_layer = |name: &String| app.session.map().layer(target.id).is_some_and(|l| l.design.templates.iter().any(|t| &t.name == name));
    let stale = state.template.clone().filter(|name| !on_layer(name));
    if let Some(gone) = &stale {
        state.template = None;
        app.session.notify(rustgis_ui_egui::i18n::fmt("The template {name} is no longer on this layer.", &[("name", gone)]), false);
    }
    let fresh = stale.is_none();
    // The feature being edited, while it is still there (an undo may have taken it away).
    let picked = state.editing.and_then(|e| edit::row_of(app, target.id, e.feature).map(|row| (row, e)));
    // A picked vertex an Undo moved or took away is let go.
    let picked = picked.map(|(row, e)| (row, e.checked(edit::shape_ref(app, target.id, row).as_deref())));
    state.editing = picked.map(|(_, e)| e);
    // Photos the camera took: for the feature being edited, attached to it now; else kept for
    // the next feature.
    if !app.photos.is_empty() {
        let taken = std::mem::take(&mut app.photos);
        match picked.filter(|_| state.edit_mode) {
            Some((row, _)) => {
                // Not attached (the refusal is said): where the photos are kept is said too, and
                // they stay in the form, for the next feature or to try again.
                if !photos::attach(app, target.id, row, &taken) {
                    let kept = taken.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ");
                    app.session.notify(
                        rustgis_ui_egui::i18n::fmt("The photo wasn't attached to the feature. It is kept at {paths}.", &[("paths", &kept)]),
                        true,
                    );
                    state.photos.extend(taken);
                }
            }
            None => state.photos.extend(taken),
        }
    }
    // The cross's place, snapped with snapping on: a vertex being moved doesn't snap to itself.
    let moving = picked.filter(|_| state.edit_mode).and_then(|(row, e)| e.vertex.map(|v| (target.id, row, v)));
    // Snapping looks through every visible layer, so it is done again only when what it depends
    // on changed, not on every frame.
    let at = cross_place(app);
    let map = app.session.map();
    let wants =
        SnapFor { at, moving, revision: app.session.revision, shapes: shapes(app), res: map.camera.res, on: map.editing.snapping.enabled };
    if state.snap_for != Some(wants) {
        state.snap = at.and_then(|at| edit::snap(app, at, moving));
        state.snap_for = Some(wants);
    }
    let cross = state.snap.map(|s| (s.lon, s.lat)).or(at);
    // Typing in a window the soft keyboard leaves short (a phone held sideways), the rows above
    // the fields would take all of it and hide the field: they step aside until the keyboard
    // goes.
    if crowded {
        ui.label(RichText::new(tl!("Close the keyboard for the buttons.")).size(13.0).color(t.text_dim));
    } else {
        let place = match (cross, state.snap) {
            (Some((lon, lat)), Some(snapped)) => rustgis_ui_egui::i18n::fmt(
                "At the cross, {snapped}: {lon}, {lat}",
                &[("snapped", edit::snapped_to(snapped.kind)), ("lon", &format!("{lon:.6}")), ("lat", &format!("{lat:.6}"))],
            ),
            (Some((lon, lat)), None) => {
                rustgis_ui_egui::i18n::fmt("At the cross: {lon}, {lat}", &[("lon", &format!("{lon:.6}")), ("lat", &format!("{lat:.6}"))])
            }
            (None, _) => tl!("The cross is off the map: move the map under it.").to_string(),
        };
        ui.label(RichText::new(place).size(13.0).color(t.text_dim));
        let width = ui.available_width();
        // With the device's position shown, one tap puts the cross on it; or the positions that come
        // while Average runs are averaged, and Use Average puts the cross on their average.
        if app.locate {
            position_buttons(app, ui, t, &ctx, width);
        }
        // Add or Edit, and Snap.
        ui.horizontal(|ui| {
            ui.set_min_height(TARGET);
            let gap = ui.spacing().item_spacing.x;
            let third = (width - 2.0 * gap) / 3.0;
            if toggle_chip(ui, t, tl!("Add"), third, !state.edit_mode) {
                state.edit_mode = false;
                state.editing = None;
            }
            // The vertices drawn so far are kept for Add to go on with.
            if toggle_chip(ui, t, tl!("Edit"), third, state.edit_mode) {
                state.edit_mode = true;
            }
            let snapping = app.session.map().editing.snapping.enabled;
            if toggle_chip(ui, t, tl!("Snap"), third, snapping) {
                run(app, "edit.snapping", json!({"on": !snapping}));
            }
        });
        if state.edit_mode {
            edit_sheet(app, ui, t, &target, &mut state, picked, cross, width);
        } else if target.kind == GeometryType::Point {
            if action(ui, t, tl!("Add Point Here"), width, true, true) && fresh && add_point(app, target.id, &state) {
                state.values.clear();
                state.photos.clear();
            }
        } else {
            let placed = state.vertices.len();
            // Enough, once the vertices make the layer's kind of shape.
            let enough = geometry(target.kind, &state.vertices).is_some();
            if action(ui, t, tl!("Add Vertex Here"), width, !enough, true) {
                match cross {
                    Some(at) if state.place(at) => {}
                    Some(_) => {
                        app.session.notify(tl!("No vertex was added: the cross is where the last one is. Move the map under it."), true)
                    }
                    None => app.session.notify(tl!("No vertex was added: the cross is off the map. Move the map under it."), true),
                }
            }
            ui.horizontal(|ui| {
                let gap = ui.spacing().item_spacing.x;
                let third = (width - 2.0 * gap) / 3.0;
                if action(ui, t, tl!("Undo Vertex"), third, false, placed > 0) {
                    state.vertices.pop();
                }
                if action(ui, t, tl!("Start Over"), third, false, placed > 0) {
                    state.vertices.clear();
                }
                // Finish is the one to tap once the shape has enough vertices.
                let finish = match target.kind {
                    GeometryType::Polygon => tl!("Finish Polygon"),
                    _ => tl!("Finish Line"),
                };
                if action(ui, t, finish, third, enough, enough)
                    && fresh
                    && let Some(shape) = geometry(target.kind, &state.vertices)
                    && add_feature(app, target.id, &state, shape)
                {
                    state.values.clear();
                    state.vertices.clear();
                    state.photos.clear();
                }
            });
        }
    }
    // The layer: one chip for each layer that takes features, in a row that scrolls sideways.
    egui::ScrollArea::horizontal().id_salt("touch_capture_layers").show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.set_min_height(TARGET);
            for layer in &layers {
                let on = layer.id == target.id;
                let text = RichText::new(&layer.name).size(15.0).color(if on { t.accent_text } else { t.text });
                let chip = egui::Button::new(text).fill(if on { t.checked } else { t.input }).min_size(Vec2::new(TARGET, TARGET - 8.0));
                // The next frame shows the chosen layer's fields, empty (`Form::for_layer`).
                if ui.add(chip).clicked() && !on {
                    state.layer = Some(layer.instance);
                }
            }
        });
    });
    let actions_height = ui.next_widget_position().y - start;
    let mut fields = |ui: &mut Ui| {
        // Keep focused fields and their widget state when the scroll viewport moves.
        ui.scope_builder(egui::UiBuilder::new().scope_id(egui::Id::unique("touch_capture_fields")), |ui| {
            capture_fields(app, ui, t, &target, &mut state, layout_changed);
        });
    };
    if scroll_all {
        fields(ui);
    } else {
        egui::ScrollArea::vertical().id_salt("touch_capture_form").auto_shrink([false, true]).show(ui, fields);
    }
    // The layer is kept only when a chip chose it: until then the sheet follows the active layer.
    set_form(&ctx, state);
    actions_height
}

/// Draws the rows for a new feature, keeping the focused field in view when the viewport changes.
fn capture_fields(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens, target: &Target, state: &mut Form, layout_changed: bool) {
    // Edit mode changes shapes, not attributes: the form is for a new feature.
    // Photos for the next feature, where the device has a camera to ask; in Edit mode those
    // already taken stay in sight (a photo for the feature edited is Add Photo's).
    if state.edit_mode {
        if !state.photos.is_empty() {
            photo_row(app, ui, t, &mut state.photos, false);
        }
    } else {
        if app.services.take_photo.is_some() || !state.photos.is_empty() {
            photo_row(app, ui, t, &mut state.photos, true);
        }
        // Read first: the rows change the form, not the layer.
        let layer = app.session.map().layer(target.id);
        let (fields, templates, lists, required): (Vec<_>, Vec<String>, Vec<Option<Vec<(String, String)>>>, Vec<String>) = match layer {
            Some(l) => (
                l.dataset.fields.clone(),
                l.design.templates.iter().map(|template| template.name.clone()).collect(),
                l.dataset.fields.iter().map(|f| choices(l, f)).collect(),
                form::required(l),
            ),
            None => Default::default(),
        };
        // What the contingent value sets leave each field, given what the others hold now.
        let left: Vec<Option<Vec<String>>> =
            fields.iter().map(|f| layer.and_then(|l| form::contingent(l, &f.name, &state.values))).collect();
        let ranges: Vec<Option<(f64, f64)>> = fields.iter().map(|f| layer.and_then(|l| range_of(l, &f.name))).collect();
        let described: Vec<Option<Vec<(String, String)>>> =
            fields.iter().map(|f| layer.and_then(|l| l.design.domain(&f.name)).map(described)).collect();
        if !required.is_empty() {
            ui.label(RichText::new(tl!("Fields marked * need a value.")).size(13.0).color(t.text_dim));
        }
        // The layer's feature templates: one fills in its defaults for what the form leaves empty.
        if !templates.is_empty() {
            ui.horizontal(|ui| {
                ui.set_min_height(TARGET);
                ui.add_sized([110.0, TARGET], egui::Label::new(RichText::new(tl!("Template")).size(14.0).color(t.text_dim)).truncate());
                let picked = state.template.clone().unwrap_or_default();
                let list: Vec<(String, String)> = templates.iter().map(|name| (name.clone(), name.clone())).collect();
                let mut chosen = picked.clone();
                chips(ui, t, "touch_capture_templates", &list, &mut chosen);
                if chosen != picked {
                    state.template = Some(chosen).filter(|name| !name.is_empty());
                }
            });
        }
        // The soft keyboard came or went: the field being typed in is brought back into view.
        let window = ui.ctx().content_rect().height();
        let resized = layout_changed || (window - state.window).abs() > 0.5;
        state.window = window;
        for (i, (field, list)) in fields.iter().zip(lists).enumerate() {
            use rustgis_ui_egui::rustgis_engine::data::FieldType;
            let name = field.alias.clone().filter(|a| !a.trim().is_empty()).unwrap_or_else(|| field.name.clone());
            let needed = required.contains(&field.name);
            let label = if needed { format!("{name} *") } else { name };
            let left = left.get(i).cloned().flatten();
            // A list narrowed by the contingent values: what the domain lists and the sets leave,
            // or, with no list of its own, the values the sets leave. The value chosen stays,
            // marked, even when the others no longer fit it, so the conflict shows.
            let list: Option<Vec<(String, String)>> = match (list, left.as_ref()) {
                (Some(list), Some(left)) => Some(list.into_iter().filter(|(stored, _)| left.contains(stored)).collect()),
                (None, Some(left)) => {
                    let said = described.get(i).cloned().flatten().unwrap_or_default();
                    Some(
                        left.iter().map(|v| (v.clone(), said.iter().find(|(c, _)| c == v).map_or(v.clone(), |(_, d)| d.clone()))).collect(),
                    )
                }
                (list, None) => list,
            };
            let chosen_from_list = list.is_some();
            let dated = matches!(field.ty, FieldType::Date | FieldType::DateTime);
            ui.horizontal(|ui| {
                ui.set_min_height(TARGET);
                let ink = if needed { t.text } else { t.text_dim };
                ui.add_sized([110.0, TARGET], egui::Label::new(RichText::new(&label).size(14.0).color(ink)).truncate());
                let value = state.values.entry(field.name.clone()).or_default();
                if let Some(mut list) = list {
                    if !value.is_empty() && !list.iter().any(|(stored, _)| stored == value) {
                        list.insert(0, (value.clone(), value.clone()));
                    }
                    if list.is_empty() {
                        ui.label(RichText::new(tl!("No value fits the others chosen.")).size(13.0).color(t.red));
                    } else {
                        // A list of values: one is tapped, and tapped again to leave the field empty.
                        chips(ui, t, ("touch_capture_values", &field.name), &list, value);
                    }
                } else {
                    let hint = match (field.ty, ranges.get(i).copied().flatten()) {
                        (_, Some((min, max))) => rustgis_ui_egui::i18n::fmt(
                            "a number from {min} to {max}",
                            &[("min", &min.to_string()), ("max", &max.to_string())],
                        ),
                        (FieldType::Integer | FieldType::Double, None) => tl!("a number").to_string(),
                        (FieldType::Date, None) => tl!("a date, as 2026-03-05").to_string(),
                        (FieldType::DateTime, None) => tl!("a date and time").to_string(),
                        _ => String::new(),
                    };
                    // A date field keeps room for the button that opens its calendar.
                    let room = if dated { ui.available_width() - TARGET - ui.spacing().item_spacing.x } else { ui.available_width() };
                    let size = Vec2::new(room.max(TARGET), TARGET - 6.0);
                    if let Some(enter) = &app.services.enter_text {
                        // Typed into the system's own field (a phone's, docked above its keyboard),
                        // which gives its paste, suggestions and dictation: this one shows the value.
                        let open = state.entering.as_ref().is_some_and(|e| e.instance == target.instance && e.field == field.name);
                        if entry_box(ui, t, size, value, &hint, open).clicked() {
                            let id = entry_id(target.instance, i, state.entries);
                            let kind = match field.ty {
                                FieldType::Integer | FieldType::Double => TextKind::Number,
                                _ => TextKind::Plain,
                            };
                            let entry = TextEntry {
                                id: id.clone(),
                                value: value.clone(),
                                label: label.clone(),
                                kind,
                                done: tl!("Done").to_string(),
                            };
                            state.entries += 1;
                            match enter(&entry) {
                                Ok(()) => state.entering = Some(Entering { id, instance: target.instance, field: field.name.clone() }),
                                Err(e) => app.session.notify(e, true),
                            }
                        }
                    } else {
                        let edit = egui::TextEdit::singleline(value)
                            .hint_text(hint)
                            .font(egui::FontId::proportional(15.0))
                            .margin(egui::Margin::symmetric(8, 10));
                        let typed = ui.add_sized(size, edit);
                        if typed.gained_focus() || (typed.has_focus() && resized) {
                            typed.scroll_to_me(Some(Align::Center));
                        }
                    }
                    if dated {
                        let open = state.calendar.as_ref().is_some_and(|(name, _)| name == &field.name);
                        let hint = if open { tl!("Close the calendar") } else { tl!("Choose a date") };
                        if toggle_button(ui, t, "calendar", hint, true) {
                            state.calendar = if open {
                                None
                            } else {
                                let day = form::day_of(value).or_else(form::today);
                                // Without a clock, the calendar starts at 1970's first month.
                                let month = day.map_or((1970, 1), |d| {
                                    let (y, m, _) = d.ymd();
                                    (y, m)
                                });
                                Some((field.name.clone(), month))
                            };
                        }
                    }
                }
            });
            // The calendar, under its field, while it is open and the field is typed into: a field
            // the contingent values narrowed to chips (its toggle gone) closes it.
            if chosen_from_list && state.calendar.as_ref().is_some_and(|(name, _)| name == &field.name) {
                state.calendar = None;
            }
            if let Some((name, month)) = state.calendar.as_mut().filter(|(name, _)| name == &field.name) {
                let name = name.clone();
                let value = state.values.entry(name).or_default();
                if field.ty == FieldType::DateTime
                    && let Some(now) = form::now()
                    && action(ui, t, tl!("Now"), ui.available_width(), false, true)
                {
                    *value = now;
                }
                if form::calendar(ui, t, month, value, field.ty) {
                    state.calendar = None;
                }
            }
        }
    }
}

/// A field drawn as a box showing its `value` (or `hint` while it is empty) that is typed into
/// with the system's text field, opened when it is tapped: outlined in the accent colour while
/// that field is `open` for it.
fn entry_box(ui: &mut Ui, t: &Tokens, size: Vec2, value: &str, hint: &str, open: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| egui::WidgetInfo::text_edit(ui.is_enabled(), value, value, hint));
    if ui.is_rect_visible(rect) {
        let stroke = if open { Stroke::new(2.0, t.accent) } else { Stroke::new(1.0, t.input_border) };
        ui.painter().rect(rect, CornerRadius::same(4), t.input, stroke, egui::StrokeKind::Inside);
        let (text, color) = if value.is_empty() { (hint, t.text_dim) } else { (value, t.text) };
        let mut layout = egui::text::LayoutJob::simple_singleline(text.to_owned(), egui::FontId::proportional(15.0), color);
        layout.wrap = egui::text::TextWrapping::truncate_at_width((rect.width() - 16.0).max(0.0));
        let galley = ui.painter().layout_job(layout);
        let at = egui::pos2(rect.left() + 8.0, rect.center().y - galley.size().y / 2.0);
        ui.painter().galley(at, galley, color);
    }
    response
}

/// The Capture sheet's buttons for the device's position: Go to My Position and Average, or,
/// while an average is being taken, what it holds, Use Average and Cancel.
fn position_buttons(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens, ctx: &egui::Context, width: f32) {
    let mut state = track::field(ctx);
    let half = (width - ui.spacing().item_spacing.x) / 2.0;
    match &state.averaging {
        None => {
            ui.horizontal(|ui| {
                if action(ui, t, tl!("Go to My Position"), half, false, app.position.is_some()) {
                    go_to_position(app);
                }
                if action(ui, t, tl!("Average Position"), half, false, true) {
                    state.averaging = Some(std::sync::Arc::default());
                }
            });
        }
        Some(fixes) => {
            ui.label(RichText::new(track::averaging_text(fixes)).size(14.0).color(t.text));
            let found = track::average(fixes);
            ui.horizontal(|ui| {
                let label = match found {
                    Some(a) => rustgis_ui_egui::i18n::fmt("Use Average ({n})", &[("n", &a.count.to_string())]),
                    None => tl!("Use Average").to_string(),
                };
                if action(ui, t, &label, half, found.is_some(), found.is_some())
                    && let Some(a) = found
                {
                    run(app, "map.goToXY", json!({"lon": a.lon, "lat": a.lat}));
                    state.averaging = None;
                }
                if action(ui, t, tl!("Cancel"), half, false, true) {
                    state.averaging = None;
                }
            });
        }
    }
    track::set_field(ctx, state);
}

/// The row of photos for the next feature: Take Photo (with `camera`), and a chip for each photo
/// taken, which a tap takes off (the file stays in the app's photos folder).
fn photo_row(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens, taken: &mut Vec<std::path::PathBuf>, camera: bool) {
    egui::ScrollArea::horizontal().id_salt("touch_capture_photos").show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.set_min_height(TARGET);
            if camera && app.services.take_photo.is_some() && action(ui, t, tl!("Take Photo"), 140.0, false, true) {
                take_photo(app);
            }
            let mut gone = None;
            for (i, path) in taken.iter().enumerate() {
                let text = RichText::new(format!("{}  ×", photos::name(path))).size(14.0).color(t.text);
                let chip = egui::Button::new(text).fill(t.checked).min_size(Vec2::new(TARGET, TARGET - 8.0));
                if ui.add(chip).on_hover_text(tl!("Take this photo off the feature")).clicked() {
                    gone = Some(i);
                }
            }
            if let Some(i) = gone {
                taken.remove(i);
            }
        });
    });
}

/// Asks the device's camera for a photo; what it refuses is said.
fn take_photo(app: &mut RustGisApp) {
    let asked = app.services.take_photo.as_ref().map(|take| take());
    if let Some(Err(e)) = asked {
        app.session.notify(e, true);
    }
}

/// A chip a finger's height, `width` wide, marked while `on`. Says whether it was tapped.
fn toggle_chip(ui: &mut Ui, t: &Tokens, label: &str, width: f32, on: bool) -> bool {
    let text = RichText::new(label).size(15.0).color(if on { t.accent_text } else { t.text });
    ui.add(egui::Button::new(text).fill(if on { t.checked } else { t.input }).min_size(Vec2::new(width, TARGET - 8.0))).clicked()
}

/// The Capture sheet in Edit mode: pick the feature under the cross, then a vertex of it near the
/// cross, and move the vertex to where the cross is next, insert one there after it, or delete it;
/// or move a point, or delete the feature. `cross` is the cross's place, snapped with snapping on.
fn edit_sheet(
    app: &mut RustGisApp,
    ui: &mut Ui,
    t: &Tokens,
    target: &Target,
    state: &mut Form,
    picked: Option<(usize, edit::Editing)>,
    cross: Option<(f64, f64)>,
    width: f32,
) {
    let gap = ui.spacing().item_spacing.x;
    let Some((row, editing)) = picked else {
        let says = rustgis_ui_egui::i18n::fmt("Put the cross on a feature of {layer} and pick it.", &[("layer", &target.name)]);
        ui.label(RichText::new(says).size(14.0).color(t.text));
        if action(ui, t, tl!("Pick Feature at Cross"), width, true, true) {
            match edit::feature_at_cross(app, target.id) {
                Some(feature) => state.editing = Some(edit::Editing::feature(feature)),
                None => app
                    .session
                    .notify(rustgis_ui_egui::i18n::fmt("No feature of {layer} is under the cross.", &[("layer", &target.name)]), true),
            }
        }
        return;
    };
    // The shape is borrowed for each look at it, not copied each frame: the buttons change it.
    let Some(status) = edit::shape_ref(app, target.id, row).map(|shape| edit::status(target.kind, &shape, editing.vertex)) else {
        // A feature with a place that can't be given in longitude/latitude (#2032 C2f-4b): drawn
        // on a map in its layer's own system, but shapes are edited here in longitude/latitude.
        // Said, as editing it ends.
        state.editing = None;
        if let Some(why) = edit::unplaced(app, target.id, row) {
            app.session.notify(why, true);
        }
        return;
    };
    ui.label(RichText::new(status).size(14.0).color(t.text));
    let off = || tl!("Nothing was changed: the cross is off the map. Move the map under it.");
    if target.kind == GeometryType::Point {
        if action(ui, t, tl!("Move Point Here"), width, true, true) {
            match cross {
                Some(at) => {
                    edit::move_point(app, target.id, row, at);
                }
                None => app.session.notify(off(), true),
            }
        }
    } else {
        let has = editing.vertex.is_some();
        if action(ui, t, tl!("Pick Vertex at Cross"), width, !has, true) {
            let middle = app.map.view_rect.map(|(rect, _)| rect.center());
            match middle.and_then(|at| edit::vertex_near(app, edit::shape_ref(app, target.id, row)?.as_ref(), at)) {
                Some(v) => state.editing = Some(editing.picking(Some(v), edit::shape_ref(app, target.id, row).as_deref())),
                None => app.session.notify(tl!("No vertex is near the cross. Move the map so the cross is on one."), true),
            }
        }
        ui.horizontal(|ui| {
            let third = (width - 2.0 * gap) / 3.0;
            for (label, act, main) in
                [(tl!("Move Here"), "move", true), (tl!("Add After"), "insert", false), (tl!("Delete Vertex"), "delete", false)]
            {
                if action(ui, t, label, third, main && has, has)
                    && let Some(v) = editing.vertex
                {
                    let at = if act == "delete" { None } else { cross };
                    if act != "delete" && at.is_none() {
                        app.session.notify(off(), true);
                    } else if let Some(at) = at
                        && edit::shape_ref(app, target.id, row).is_some_and(|shape| edit::doubles(app, &shape, v, act, at))
                    {
                        app.session.notify(
                            tl!("Nothing was changed: the cross is on the vertex next to it, and two in one place make an edge of no length. Move the map."),
                            true,
                        );
                    } else if let Some(next) = edit::vertex(app, target.id, row, v, act, at) {
                        state.editing = Some(editing.picking(next, edit::shape_ref(app, target.id, row).as_deref()));
                    }
                }
            }
        });
    }
    ui.horizontal(|ui| {
        // A photo taken now is attached to this feature when it arrives.
        let camera = app.services.take_photo.is_some();
        let share = if camera { (width - 2.0 * gap) / 3.0 } else { (width - gap) / 2.0 };
        if camera && action(ui, t, tl!("Add Photo"), share, false, true) {
            take_photo(app);
        }
        if action(ui, t, tl!("Delete Feature"), share, false, true) && edit::delete_feature(app, target.id, row) {
            state.editing = None;
        }
        if action(ui, t, tl!("Done"), share, false, true) {
            state.editing = None;
        }
    });
}

/// The More sheet: what is done now and then, each a row as tall as a finger.
fn more(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    heading(ui, t, tl!("More"));
    let rows: [(&str, &str, &str); 7] = [
        ("zoom_full", tl!("Full Extent"), "map.zoomFull"),
        ("open", tl!("Open Project"), "ui.open"),
        ("save", tl!("Save Project"), "ui.save"),
        // Save writes back where the project came from: this asks where.
        ("save", tl!("Save Project As…"), "ui.saveAs"),
        ("new", tl!("New Project"), "project.new"),
        ("undo", tl!("Undo"), "edit.undo"),
        ("redo", tl!("Redo"), "edit.redo"),
    ];
    egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
        for (icon, label, command) in rows {
            if row(ui, t, icon, label) {
                // While unused copies are being removed, nothing may bring back or open a layer
                // reading one: the project was looked at once, as the removal began.
                if copies::removing(ui.ctx()) && ["edit.undo", "edit.redo", "ui.open", "project.new"].contains(&command) {
                    app.session.notify(tl!("Wait for the unused copies to be removed."), false);
                    continue;
                }
                // Open and New ask first when the project has unsaved changes: the front end's
                // own question, as for the ribbon's buttons.
                set_sheet(ui.ctx(), Sheet::None);
                run(app, command, Value::Null);
            }
        }
        // The device's position, where the shell can report one (it asks the user's leave first).
        let ctx = ui.ctx().clone();
        let locate = if app.locate { tl!("Hide My Position") } else { tl!("Show My Position") };
        if row(ui, t, "explore", locate) {
            set_sheet(&ctx, Sheet::None);
            // Hidden, no more positions come: a track being recorded is added as it is.
            if app.locate {
                stop_track(app, &ctx);
            }
            app.locate = !app.locate;
        }
        if app.locate && row(ui, t, "zoom_full", tl!("Go to My Position")) {
            set_sheet(&ctx, Sheet::None);
            go_to_position(app);
        }
        // A track: where the device goes, drawn as it goes, and added to the map when stopped.
        let recording = track::field(&ctx).track.is_some();
        if app.locate || recording {
            let label = if recording { tl!("Stop the Track") } else { tl!("Record a Track") };
            if row(ui, t, "line_tool", label) {
                set_sheet(&ctx, Sheet::None);
                if recording {
                    stop_track(app, &ctx);
                } else {
                    start_track(app, &ctx);
                }
            }
        }
        // A map opened from a field package goes back as one: its edits and photos, for Bring Back
        // on the computer it came from.
        if can_send_field_package(app) && row(ui, t, "package", tl!("Send Field Package")) {
            set_sheet(&ctx, Sheet::None);
            run(app, "ui.saveItem", json!({"kind": "fieldPackage"}));
        }
        // The copies of picked files nothing in the project names, removed once agreed.
        if copies::folder().is_some_and(|f| f.is_dir()) {
            copies_row(app, ui, t, &ctx);
        }
        // The whole of RustGIS is still there, for a tablet or a job the touch interface has no
        // button for yet. View › Touch Interface (`ui.touchInterface`) brings this one back.
        if row(ui, t, "pane", tl!("Use the Desktop Interface")) {
            set_sheet(ui.ctx(), Sheet::None);
            // A track being recorded is added before the bar that stops it goes away; one that
            // couldn't be keeps this interface, which can still stop it.
            if stop_track(app, &ctx) {
                app.chrome = None;
                // Nothing to keep positions coming for out of sight now, and no interface that
                // would say so.
                app.keep_locating = false;
                app.session.notify(tl!("The desktop interface is on. View › Touch Interface brings this one back."), false);
            }
        }
    });
}

/// How many rows Remove Unused Copies takes in the More sheet: none on a computer, two while it
/// asks.
fn copies_rows(ctx: &egui::Context) -> usize {
    match copies::folder() {
        Some(f) if f.is_dir() => match copies::job(ctx) {
            copies::Job::Asking(_) => 2,
            _ => 1,
        },
        _ => 0,
    }
}

/// Remove Unused Copies: the row that starts it, what it is doing (with Cancel), or what it found
/// (with Remove and Keep).
fn copies_row(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens, ctx: &egui::Context) {
    match copies::job(ctx) {
        copies::Job::Idle => {
            if row(ui, t, "delete", tl!("Remove Unused Copies")) {
                copies::start(app, ctx);
            }
        }
        copies::Job::Scanning(..) | copies::Job::Removing(..) => {
            let doing = if matches!(copies::job(ctx), copies::Job::Removing(..)) {
                tl!("Removing the unused copies…")
            } else {
                tl!("Looking for unused copies…")
            };
            ui.horizontal(|ui| {
                ui.set_min_height(TARGET);
                ui.spinner();
                ui.label(RichText::new(doing).size(15.0).color(t.text));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if action(ui, t, tl!("Cancel"), 96.0, false, true) {
                        copies::cancel(ctx);
                    }
                });
            });
        }
        copies::Job::Asking(found) => {
            let (count, size) = (found.files.to_string(), copies::size(found.bytes));
            let template = if found.files == 1 {
                "1 copy of a picked file ({size}) isn't named by this project. A project saved on this phone that names it won't find it, nor will Undo bring back a layer that reads from it."
            } else {
                "{count} copies of picked files ({size}) aren't named by this project. A project saved on this phone that names one won't find it, nor will Undo bring back a layer that reads from one."
            };
            let text = i18n::fmt(template, &[("count", &count), ("size", &size)]);
            ui.label(RichText::new(text).size(14.0).color(t.text));
            ui.horizontal(|ui| {
                if action(ui, t, tl!("Remove Them"), 140.0, true, true) {
                    copies::confirm(app, ctx, found.clone());
                }
                if action(ui, t, tl!("Keep Them"), 120.0, false, true) {
                    copies::cancel(ctx);
                }
            });
        }
    }
}

/// A row of a sheet's list: an icon and a label, as wide as the sheet. Says whether it was tapped.
fn row(ui: &mut Ui, t: &Tokens, icon: &str, label: &str) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), TARGET), Sense::click());
    if response.is_pointer_button_down_on() {
        ui.painter().rect_filled(rect, CornerRadius::same(6), t.pressed);
    }
    icons::paint(
        ui.painter(),
        Rect::from_center_size(egui::pos2(rect.min.x + TARGET / 2.0, rect.center().y), Vec2::splat(22.0)),
        icon,
        t.icon,
        t.accent,
    );
    ui.painter().text(
        egui::pos2(rect.min.x + TARGET + 4.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(15.0),
        t.text,
    );
    response.clicked()
}

/// Runs a command the user tapped for. What it refuses, the front end says in the window itself,
/// as it does for the ribbon's buttons.
fn run(app: &mut RustGisApp, command: &str, params: Value) {
    let _ = app.run_by_user(command, params);
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_ui_egui::rustgis_engine::data::VertexId;

    /// Draws a frame of the interface for a phone's window with `shown` up.
    fn frame(app: &mut RustGisApp, ctx: &egui::Context, shown: Sheet) {
        set_sheet(ctx, shown);
        let input =
            egui::RawInput { screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(411.0, 914.0))), ..Default::default() };
        ctx.run_ui(input, |ui| chrome(app, ui)).drop_without_applying_deltas();
    }

    /// The polygon actions, layer chip and first field can all scroll through the short sheet.
    #[test]
    fn landscape_capture_scrolls_actions_and_form_into_view() {
        let ctx = egui::Context::default();
        let mut app = RustGisApp::with_sample();
        app.chrome = Some(chrome);
        let target = target(&mut app, GeometryType::Polygon);
        let field = &app.session.map().layer(target.id).unwrap().dataset.fields[0];
        let field_label = field.alias.as_ref().unwrap_or(&field.name).clone();
        set_sheet(&ctx, Sheet::Capture);
        let mut seen = [false; 4];
        let labels = ["Undo Vertex", "Finish Polygon", &target.name, &field_label];
        for step in 0..44 {
            let mut input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 360.0))),
                time: Some(f64::from(step) * 0.1),
                ..Default::default()
            };
            if step >= 4 {
                use egui::TouchPhase::{End, Move, Start};
                let (phase, y) = match step % 4 {
                    0 => (Start, 271.0),
                    1 => (Move, 241.0),
                    2 => (Move, 211.0),
                    _ => (End, 211.0),
                };
                let pos = egui::pos2(400.0, y);
                input.events.push(egui::Event::Touch { device_id: egui::TouchDeviceId(0), id: egui::TouchId(0), phase, pos, force: None });
                if phase != End {
                    input.events.push(egui::Event::PointerMoved(pos));
                }
                if matches!(phase, Start | End) {
                    input.events.push(egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: phase == Start,
                        modifiers: egui::Modifiers::NONE,
                    });
                }
                if phase == End {
                    input.events.push(egui::Event::PointerGone);
                }
            }
            let output = ctx.run_ui(input, |ui| {
                app.logic(&ctx);
                app.ui(ui);
            });
            for painted in &output.shapes {
                if let egui::epaint::Shape::Text(text) = &painted.shape
                    && painted.clip_rect.contains_rect(text.visual_bounding_rect())
                {
                    for (visible, label) in seen.iter_mut().zip(labels) {
                        *visible |= text.galley.text() == label;
                    }
                }
            }
            output.drop_without_applying_deltas();
            if seen.iter().all(|visible| *visible) {
                return;
            }
        }
        assert!(seen.iter().all(|visible| *visible), "visibility for {labels:?}: {seen:?}");
    }

    #[test]
    fn portrait_capture_keeps_finish_visible_while_the_form_scrolls() {
        let ctx = egui::Context::default();
        let mut app = RustGisApp::with_sample();
        app.chrome = Some(chrome);
        let target = target(&mut app, GeometryType::Polygon);
        for i in 0..8 {
            app.session.run("edit.addField", &json!({"layer": target.id, "name": format!("Extra_{i}"), "type": "text"})).unwrap();
        }
        set_sheet(&ctx, Sheet::Capture);
        set_form(&ctx, Form { shown: Some(target.instance), vertices: vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0)], ..Form::default() });
        let pos = egui::pos2(190.0, 660.0);
        let mut last_field_visible = false;
        for step in 0..18 {
            let mut input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(360.0, 760.0))),
                time: Some(f64::from(step) * 0.1),
                events: vec![egui::Event::PointerMoved(pos)],
                ..Default::default()
            };
            if step >= 5 {
                input.events.push(egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: Vec2::new(0.0, -160.0),
                    modifiers: egui::Modifiers::NONE,
                    phase: egui::TouchPhase::Move,
                    source: egui::MouseWheelSource::Wheel,
                });
            }
            let output = ctx.run_ui(input, |ui| {
                app.logic(&ctx);
                app.ui(ui);
            });
            let visible = |label| {
                output.shapes.iter().any(|painted| {
                    matches!(&painted.shape, egui::epaint::Shape::Text(text)
                        if text.galley.text() == label && painted.clip_rect.contains_rect(text.visual_bounding_rect()))
                })
            };
            let (finish, layer, last_field) = (visible("Finish Polygon"), visible(&target.name), visible("Extra_7"));
            output.drop_without_applying_deltas();
            if step == 4 {
                assert!(!last_field, "last field starts below the scroll viewport");
            }
            if step >= 4 {
                assert!(finish, "Finish went out of view on frame {step}");
                assert!(layer, "layer choice went out of view on frame {step}");
            }
            last_field_visible |= last_field;
        }
        assert!(last_field_visible, "the form scrolled to the last field");
    }

    #[test]
    fn every_sheet_draws_for_the_sample_and_for_an_empty_map() {
        let ctx = egui::Context::default();
        let mut app = RustGisApp::with_sample();
        for shown in [Sheet::None, Sheet::Layers, Sheet::Capture, Sheet::More] {
            frame(&mut app, &ctx, shown);
            assert_eq!(sheet(&ctx), shown);
        }
        // No layers: the sheet says so instead of listing.
        let ids: Vec<u32> = app.session.map().layers.iter().map(|l| l.id).collect();
        for id in ids {
            run(&mut app, "layer.remove", json!({"layer": id}));
        }
        assert!(app.session.map().layers.is_empty());
        frame(&mut app, &ctx, Sheet::Layers);
    }

    #[test]
    fn a_map_with_a_tile_layer_draws_its_sheet_and_takes_the_rows_commands() {
        let mut app = RustGisApp::with_sample();
        run(&mut app, "map.addTiles", json!({"url": "https://tiles.example.com/{z}/{x}/{y}.png", "name": "Imagery"}));
        assert_eq!(app.session.map().tiles.len(), 1);
        frame(&mut app, &egui::Context::default(), Sheet::Layers);
        run(&mut app, "map.setTiles", json!({"index": 0, "visible": false}));
        assert!(!app.session.map().tiles[0].visible);
    }

    #[test]
    fn a_project_with_unsaved_changes_is_kept_until_the_user_answers() {
        let mut app = RustGisApp::with_sample();
        app.session.mark_saved();
        let layers = app.session.map().layers.len();
        assert!(layers > 0);
        // Unsaved: New Project waits for the front end's question, and the project stays.
        let first = app.session.map().layers.first().map(|l| l.id);
        run(&mut app, "layer.rename", json!({"layer": first, "name": "Renamed"}));
        assert!(app.session.dirty());
        run(&mut app, "project.new", Value::Null);
        assert_eq!(app.session.map().layers.len(), layers);
    }

    #[test]
    fn a_saved_project_is_replaced_without_a_question() {
        let mut app = RustGisApp::with_sample();
        app.session.mark_saved();
        run(&mut app, "project.new", Value::Null);
        assert!(app.session.map().layers.is_empty());
    }

    /// The sample's layer of `kind`, made the active one, as the sheet resolves it.
    fn target(app: &mut RustGisApp, kind: GeometryType) -> Target {
        let id = app.session.map().layers.iter().find(|l| l.geometry_type() == kind).map(|l| l.id).unwrap();
        run(app, "layer.select", json!({"layer": id}));
        let target = capture_layer(app, &Form::default()).unwrap();
        assert_eq!(target.id, id);
        target
    }

    fn count(app: &RustGisApp, id: u32) -> usize {
        app.session.map().layer(id).unwrap().dataset.features.len()
    }

    #[test]
    fn a_point_is_captured_where_the_map_is_centred_with_what_the_form_holds() {
        let ctx = egui::Context::default();
        let mut app = RustGisApp::with_sample();
        // The sample's Cities are points.
        let cities = target(&mut app, GeometryType::Point);
        assert_eq!(cities.name, "Cities");
        let before = count(&app, cities.id);
        let layer = app.session.map().layer(cities.id).unwrap();
        let text = layer
            .dataset
            .fields
            .iter()
            .find(|f| f.ty == rustgis_ui_egui::rustgis_engine::data::FieldType::Text)
            .map(|f| f.name.clone())
            .unwrap();
        let mut state = Form { layer: Some(cities.instance), ..Form::default() };
        state.values.insert(text.clone(), "Field Camp".into());
        // With editing off, capturing turns it on, and says so.
        run(&mut app, "edit.enable", json!({"on": false}));
        assert!(!app.session.map().editing.settings.enabled);
        let mark = app.session.notice_mark();
        assert!(add_point(&mut app, cities.id, &state));
        assert!(app.session.map().editing.settings.enabled);
        assert!(app.session.notices_since(mark).iter().all(|n| !n.error));
        assert_eq!(app.session.notices_since(mark).len(), 2);
        assert_eq!(count(&app, cities.id), before + 1);
        // The sheet draws with the new point's layer, and for a map with nothing to capture into.
        frame(&mut app, &ctx, Sheet::Capture);
        let ids: Vec<u32> = app.session.map().layers.iter().map(|l| l.id).collect();
        for id in ids {
            run(&mut app, "layer.remove", json!({"layer": id}));
        }
        assert!(capture_layer(&app, &Form { layer: Some(cities.instance), ..Form::default() }).is_none());
        frame(&mut app, &ctx, Sheet::Capture);
    }

    #[test]
    fn a_line_and_a_polygon_are_captured_through_their_vertices() {
        let ctx = egui::Context::default();
        let mut app = RustGisApp::with_sample();
        // Too few vertices make no geometry; enough make the kind's own.
        let square = [(10.0, 10.0), (11.0, 10.0), (11.0, 11.0), (10.0, 11.0)];
        assert_eq!(geometry(GeometryType::Polyline, &square[..1]), None);
        assert_eq!(geometry(GeometryType::Polygon, &square[..2]), None);
        let line = geometry(GeometryType::Polyline, &square[..2]).unwrap();
        assert_eq!(line, json!({"type": "LineString", "coordinates": [[10.0, 10.0], [11.0, 10.0]]}));
        // A polygon's ring closes on its first vertex.
        let ring = geometry(GeometryType::Polygon, &square).unwrap();
        assert_eq!(ring["coordinates"][0].as_array().unwrap().len(), 5);
        assert_eq!(ring["coordinates"][0][0], ring["coordinates"][0][4]);
        for (kind, shape, word) in [(GeometryType::Polyline, line, "line"), (GeometryType::Polygon, ring, "polygon")] {
            let layer = target(&mut app, kind);
            let before = count(&app, layer.id);
            let mark = app.session.notice_mark();
            assert!(add_feature(&mut app, layer.id, &Form::default(), shape));
            assert_eq!(count(&app, layer.id), before + 1);
            let said: Vec<_> = app.session.notices_since(mark).iter().map(|n| n.text.clone()).collect();
            assert!(said.iter().any(|text| text.contains(word)), "{said:?}");
            // The sheet draws for the layer with a shape half drawn, its sketch over the map.
            set_form(&ctx, Form { vertices: square[..3].to_vec(), shown: Some(layer.instance), ..Form::default() });
            frame(&mut app, &ctx, Sheet::Capture);
            assert_eq!(form(&ctx).vertices.len(), 3);
        }
        // Closed by hand, on its first vertex: the ring is the same, and three in all are too few.
        let by_hand = [square[0], square[1], square[2], square[3], square[0]];
        assert_eq!(geometry(GeometryType::Polygon, &by_hand), geometry(GeometryType::Polygon, &square));
        assert_eq!(geometry(GeometryType::Polygon, &[square[0], square[1], square[0]]), None);
        // A line may come back to where it began.
        assert!(geometry(GeometryType::Polyline, &[square[0], square[1], square[0]]).is_some());
        // A vertex where the last one is isn't placed; one anywhere else is.
        let mut drawing = Form::default();
        assert!(drawing.place(square[0]));
        assert!(!drawing.place(square[0]));
        assert!(drawing.place(square[1]));
        assert_eq!(drawing.vertices, square[..2]);
        // A shape of the wrong kind for its layer is refused, and said.
        let cities = target(&mut app, GeometryType::Point);
        let before = count(&app, cities.id);
        let mark = app.session.notice_mark();
        assert!(!add_feature(&mut app, cities.id, &Form::default(), geometry(GeometryType::Polyline, &square).unwrap()));
        assert_eq!(count(&app, cities.id), before);
        assert_eq!(app.session.notices_since(mark).iter().filter(|n| n.error).count(), 1);
    }

    #[test]
    fn the_sheet_follows_the_active_layer_until_a_chip_chooses_another() {
        let mut app = RustGisApp::with_sample();
        let cities = target(&mut app, GeometryType::Point);
        let rivers = target(&mut app, GeometryType::Polyline);
        // Nothing chosen: the active layer, whichever it is, and it is followed as it changes.
        for layer in [&cities, &rivers] {
            run(&mut app, "layer.select", json!({"layer": layer.id}));
            assert_eq!(capture_layer(&app, &Form::default()).map(|l| l.id), Some(layer.id));
        }
        // Drawing the sheet doesn't choose: the next frame still follows the active layer.
        let ctx = egui::Context::default();
        frame(&mut app, &ctx, Sheet::Capture);
        assert_eq!(form(&ctx).layer, None);
        // A chip's choice wins over the active layer.
        run(&mut app, "layer.select", json!({"layer": cities.id}));
        assert_eq!(capture_layer(&app, &Form { layer: Some(rivers.instance), ..Form::default() }).map(|l| l.id), Some(rivers.id));
        // A choice made for another map's or project's layer is no choice here, whatever its id.
        assert_eq!(capture_layer(&app, &Form { layer: Some(u64::MAX), ..Form::default() }).map(|l| l.id), Some(cities.id));
        // The sample opened again: its Cities is another layer, though its id is the same.
        app.session.mark_saved();
        run(&mut app, "project.sample", Value::Null);
        let again = capture_layers(&app).into_iter().find(|l| l.id == cities.id).map(|l| l.instance);
        assert!(again.is_some() && again != Some(cities.instance), "{again:?}");
    }

    #[test]
    fn text_in_a_field_of_numbers_keeps_the_point_from_being_added_and_is_said() {
        use rustgis_ui_egui::rustgis_engine::data::FieldType;
        let mut app = RustGisApp::with_sample();
        let id = target(&mut app, GeometryType::Point).id;
        let layer = app.session.map().layer(id).unwrap();
        let before = layer.dataset.features.len();
        let number =
            layer.dataset.fields.iter().find(|f| matches!(f.ty, FieldType::Integer | FieldType::Double)).map(|f| f.name.clone()).unwrap();
        let mut state = Form::default();
        state.values.insert(number.clone(), "many".into());
        let mark = app.session.notice_mark();
        assert!(!add_point(&mut app, id, &state));
        assert_eq!(count(&app, id), before);
        let said: Vec<_> = app.session.notices_since(mark).iter().filter(|n| n.error).collect();
        assert_eq!(said.len(), 1);
        assert!(said[0].text.contains(&number), "{}", said[0].text);
        // A number there goes in, with no error.
        state.values.insert(number, "12".into());
        let mark = app.session.notice_mark();
        assert!(add_point(&mut app, id, &state));
        assert!(app.session.notices_since(mark).iter().all(|n| !n.error));
    }

    #[test]
    fn a_fields_list_range_and_template_come_from_the_layers_design() {
        use rustgis_ui_egui::rustgis_engine::data::{Field, FieldType};
        let ctx = egui::Context::default();
        let mut app = RustGisApp::with_sample();
        let cities = target(&mut app, GeometryType::Point);
        let fields = app.session.map().layer(cities.id).unwrap().dataset.fields.clone();
        let text = fields.iter().find(|f| f.ty == FieldType::Text).unwrap().clone();
        let number = fields.iter().find(|f| matches!(f.ty, FieldType::Integer | FieldType::Double)).unwrap().clone();
        // No design yet: both are typed into. A yes/no field always has its two values.
        let layer = app.session.map().layer(cities.id).unwrap();
        assert_eq!(choices(layer, &text), None);
        assert_eq!(range_of(layer, &number.name), None);
        let yes_no = choices(layer, &Field::new("surveyed", FieldType::Boolean)).unwrap();
        assert_eq!(yes_no.iter().map(|(stored, _)| stored.as_str()).collect::<Vec<_>>(), ["true", "false"]);
        // A coded domain gives the field a list, shown by description; a range gives its limits.
        let coded = json!({"layer": cities.id, "field": text.name, "coded": [["cap", "Capital"], ["town", ""]], "force": true});
        run(&mut app, "data.domain", coded);
        run(&mut app, "data.domain", json!({"layer": cities.id, "field": number.name, "min": 0, "max": 10, "force": true}));
        let layer = app.session.map().layer(cities.id).unwrap();
        assert_eq!(choices(layer, &text), Some(vec![("cap".into(), "Capital".into()), ("town".into(), "town".into())]));
        assert_eq!(range_of(layer, &number.name), Some((0.0, 10.0)));
        assert_eq!(choices(layer, &number), None);
        // A template's defaults fill what the form leaves empty.
        run(&mut app, "edit.template", json!({"layer": cities.id, "name": "Capital", "defaults": {text.name.clone(): "cap"}}));
        let state = Form { template: Some("Capital".into()), ..Form::default() };
        assert!(add_point(&mut app, cities.id, &state));
        let layer = app.session.map().layer(cities.id).unwrap();
        let column = layer.dataset.fields.iter().position(|f| f.name == text.name).unwrap();
        assert_eq!(layer.dataset.features.last().unwrap().attrs[column].to_string(), "cap");
        // A template the layer no longer has is not asked for: the point is still added.
        let before = count(&app, cities.id);
        assert!(add_point(&mut app, cities.id, &Form { template: Some("Gone".into()), ..Form::default() }));
        assert_eq!(count(&app, cities.id), before + 1);
        // A value outside the list is refused by the command, and said.
        let mut state = Form::default();
        state.values.insert(text.name.clone(), "village".into());
        let mark = app.session.notice_mark();
        assert!(!add_point(&mut app, cities.id, &state));
        assert!(app.session.notices_since(mark).iter().any(|n| n.error));
        // A code of no text is not offered: it would show as chosen whenever nothing is.
        run(
            &mut app,
            "data.domain",
            json!({"layer": cities.id, "field": text.name, "coded": [["", "None"], ["cap", "Capital"]], "force": true}),
        );
        let layer = app.session.map().layer(cities.id).unwrap();
        assert_eq!(choices(layer, &text), Some(vec![("cap".into(), "Capital".into())]));
        // With no code left to offer, the field is typed into, as one without a list is.
        // (The command itself drops a code of no text; a design read from a project may hold one.)
        use rustgis_ui_egui::rustgis_engine::data::Value as Stored;
        use rustgis_ui_egui::rustgis_engine::map::design::{Domain, DomainKind};
        let mut copy = app.session.map().layer(cities.id).unwrap().clone();
        copy.design.domains =
            vec![Domain { field: text.name.clone(), kind: DomainKind::Coded(vec![(Stored::Text(String::new()), "None".into())]) }];
        assert_eq!(choices(&copy, &text), None);
        // A template chosen and since removed is let go by the sheet, with a notice.
        set_form(&ctx, Form { shown: Some(cities.instance), template: Some("Gone".into()), ..Form::default() });
        let mark = app.session.notice_mark();
        frame(&mut app, &ctx, Sheet::Capture);
        assert_eq!(form(&ctx).template, None);
        assert!(app.session.notices_since(mark).iter().any(|n| n.text.contains("Gone")));
        // The sheet draws the lists, the range hint and the templates.
        set_form(&ctx, Form { shown: Some(cities.instance), template: Some("Capital".into()), ..Form::default() });
        frame(&mut app, &ctx, Sheet::Capture);
        assert_eq!(form(&ctx).template.as_deref(), Some("Capital"));
    }

    #[test]
    fn the_devices_position_is_drawn_and_the_map_goes_to_it() {
        use rustgis_ui_egui::rustgis_engine::platform::Fix;
        let ctx = egui::Context::default();
        let mut app = RustGisApp::with_sample();
        // No position yet: going to it says so, and the map stays.
        let before = cross_place(&app);
        let mark = app.session.notice_mark();
        go_to_position(&mut app);
        assert_eq!(cross_place(&app), before);
        assert_eq!(app.session.notices_since(mark).len(), 1);
        // With one, the map's middle (the capture cross) goes to it.
        app.locate = true;
        app.position = Some(Fix { lon: 30.0, lat: -2.0, accuracy: Some(5.0), time: None, elapsed_realtime_nanos: None });
        go_to_position(&mut app);
        let (lon, lat) = cross_place(&app).unwrap();
        assert!((lon - 30.0).abs() < 1e-6 && (lat + 2.0).abs() < 1e-6, "{lon} {lat}");
        // Every sheet draws with the position on the map, its ring too (a wide one, to be drawn).
        // An accuracy out of all measure draws too, without its ring.
        for accuracy in [500_000.0, 1e300, f64::INFINITY] {
            app.position = Some(Fix { lon: 30.0, lat: -2.0, accuracy: Some(accuracy), time: None, elapsed_realtime_nanos: None });
            for shown in [Sheet::None, Sheet::Capture, Sheet::More] {
                frame(&mut app, &ctx, shown);
            }
        }
    }

    #[test]
    fn a_map_naming_a_missing_area_is_not_offered_for_sending() {
        let mut app = RustGisApp::with_sample();
        app.session.map_mut().offline_area_id = Some("gone".into());
        assert!(!can_send_field_package(&app));
    }

    #[test]
    fn live_position_prefers_a_recent_accurate_android_fix() {
        use rustgis_ui_egui::rustgis_engine::platform::Event;

        let mut app = RustGisApp::with_sample();
        app.locate = true;
        let mut received = Vec::new();
        for (line, expected_lat) in [
            ("position\t2.2945\t48.85840\t5\t10000\t1000000000", 48.85840),
            // One second later, the network fix is 150 m north. The wall clock jumped ahead.
            ("position\t2.2945\t48.85975\t150\t20000\t2000000000", 48.85840),
            // Exactly five seconds after the GPS fix, the network fix can replace it.
            ("position\t2.2945\t48.85975\t150\t24000\t6000000000", 48.85975),
            // A newer, better fix replaces the network fix even if the wall clock went back.
            ("position\t2.2945\t48.85850\t4\t8000\t7000000000", 48.85850),
            // A late GPS fix is older, even though its wall clock time and accuracy look better.
            ("position\t2.2945\t48.85830\t3\t30000\t6999999999", 48.85850),
        ] {
            let Event::Position(fix) = Event::parse(line) else { panic!("invalid position event: {line}") };
            received.push(fix);
            app.report_position(fix);
            assert_eq!(app.position.unwrap().lat, expected_lat);
            go_to_position(&mut app);
            let (_, lat) = cross_place(&app).unwrap();
            assert!((lat - expected_lat).abs() < 1e-6, "{lat}");
        }
        // Track recording and averaging still receive every fix in arrival order.
        assert_eq!(app.fixes.into_iter().collect::<Vec<_>>(), received);
    }

    #[test]
    fn live_position_uses_the_fix_timestamp_without_a_monotonic_clock() {
        use rustgis_ui_egui::rustgis_engine::platform::Fix;

        let mut app = RustGisApp::with_sample();
        let current = Fix { lon: 30.0, lat: -2.0, accuracy: Some(5.0), time: Some(10_000), elapsed_realtime_nanos: None };
        app.report_position(current);
        // A little less accurate: GPS jitter still moves the marker.
        let jitter = Fix { accuracy: Some(8.0), time: Some(11_000), ..current };
        app.report_position(jitter);
        assert_eq!(app.position, Some(jitter));
        // Much less accurate and recent: held back until five seconds have passed.
        app.report_position(Fix { accuracy: None, time: Some(15_999), ..jitter });
        assert_eq!(app.position, Some(jitter));
        let stale = Fix { accuracy: None, time: Some(16_000), ..jitter };
        app.report_position(stale);
        assert_eq!(app.position, Some(stale));
        // A late fix, a little older than the shown one, is dropped even though it is better.
        app.report_position(Fix { accuracy: Some(1.0), time: Some(15_999), ..current });
        assert_eq!(app.position, Some(stale));
        // The clock set back: the fix is taken instead of freezing the position.
        let after_step = Fix { accuracy: Some(5.0), time: Some(1_000), ..current };
        app.report_position(after_step);
        assert_eq!(app.position, Some(after_step));
        let next = Fix { accuracy: Some(5.0), time: Some(2_000), ..current };
        app.report_position(next);
        assert_eq!(app.position, Some(next));
    }

    #[test]
    fn a_required_field_left_empty_is_named_and_a_default_fills_it() {
        use rustgis_ui_egui::rustgis_engine::data::FieldType;
        let ctx = egui::Context::default();
        let mut app = RustGisApp::with_sample();
        let cities = target(&mut app, GeometryType::Point);
        let text =
            app.session.map().layer(cities.id).unwrap().dataset.fields.iter().find(|f| f.ty == FieldType::Text).unwrap().name.clone();
        let rule = json!({"layer": cities.id, "name": "named", "kind": "constraint", "expression": format!("{text} IS NOT NULL")});
        run(&mut app, "data.attributeRule", rule);
        assert_eq!(form::required(app.session.map().layer(cities.id).unwrap()), std::slice::from_ref(&text));
        // Empty: nothing is added, and the field is named.
        let before = count(&app, cities.id);
        let mark = app.session.notice_mark();
        assert!(!add_point(&mut app, cities.id, &Form::default()));
        assert_eq!(count(&app, cities.id), before);
        let said: Vec<_> = app.session.notices_since(mark).iter().filter(|n| n.error).map(|n| n.text.clone()).collect();
        assert!(said.len() == 1 && said[0].contains(&text), "{said:?}");
        // Filled, or filled by the template chosen: added.
        let mut state = Form::default();
        state.values.insert(text.clone(), "Camp".into());
        assert!(add_point(&mut app, cities.id, &state));
        run(&mut app, "edit.template", json!({"layer": cities.id, "name": "Camp", "defaults": {text.clone(): "Camp"}}));
        assert!(add_point(&mut app, cities.id, &Form { template: Some("Camp".into()), ..Form::default() }));
        assert_eq!(count(&app, cities.id), before + 2);
        // The sheet draws the marked field, a contingent set's chips and an open calendar.
        let number =
            app.session.map().layer(cities.id).unwrap().dataset.fields.iter().find(|f| f.ty != FieldType::Text).unwrap().name.clone();
        let set = json!({"layer": cities.id, "name": "pair", "fields": [text, number], "combos": [["Camp", 1], ["Town", 2]]});
        run(&mut app, "data.contingentValues", set);
        assert_eq!(form::contingent(app.session.map().layer(cities.id).unwrap(), &text, &state.values).map(|v| v.len()), Some(2));
        // A calendar open on a field the contingent values narrow to chips closes: its toggle is
        // gone with the text box.
        set_form(&ctx, Form { shown: Some(cities.instance), calendar: Some((text, (2026, 3))), ..Form::default() });
        frame(&mut app, &ctx, Sheet::Capture);
        assert!(form(&ctx).calendar.is_none());
        // On a date field typed into, it stays open and draws.
        run(&mut app, "edit.addField", json!({"layer": cities.id, "name": "seen", "type": "date"}));
        set_form(&ctx, Form { shown: Some(cities.instance), calendar: Some(("seen".into(), (2026, 3))), ..Form::default() });
        frame(&mut app, &ctx, Sheet::Capture);
        assert!(form(&ctx).calendar.is_some());
    }

    #[test]
    fn the_snap_is_looked_for_again_once_a_layers_shapes_change() {
        let ctx = egui::Context::default();
        let mut app = RustGisApp::with_sample();
        target(&mut app, GeometryType::Polyline);
        frame(&mut app, &ctx, Sheet::Capture);
        assert!(form(&ctx).snap_for.is_some());
        // A stale snap the cache holds, then features read in the background: it is let go.
        let stale = edit::Snapped { lon: 1.0, lat: 2.0, kind: "vertex" };
        set_form(&ctx, Form { snap: Some(stale), ..form(&ctx) });
        frame(&mut app, &ctx, Sheet::Capture);
        // Nothing changed since the snap was found: the one held is kept.
        assert_eq!(form(&ctx).snap, Some(stale));
        // A layer's shapes put in place, as the map view does with a streamed layer's features.
        let id = app.session.map().layers[0].id;
        app.session.map_mut().layer_mut(id).unwrap().touch_geometry();
        frame(&mut app, &ctx, Sheet::Capture);
        assert_ne!(form(&ctx).snap, Some(stale));
    }

    #[test]
    fn a_vertex_put_on_the_one_next_to_it_is_found_and_a_place_apart_is_not() {
        let ctx = egui::Context::default();
        let mut app = RustGisApp::with_sample();
        let rivers = target(&mut app, GeometryType::Polyline);
        let shape = app.session.map().layer(rivers.id).unwrap().dataset.features[0].geometry.clone().unwrap();
        let vertices = shape.vertices();
        let ((first, a), (second, b), (_, c)) = (vertices[0], vertices[1], vertices[2]);
        run(&mut app, "map.goToXY", json!({"lon": b.x, "lat": b.y, "scale": 50_000}));
        frame(&mut app, &ctx, Sheet::Capture);
        // Moved onto the vertex before or after it, or one added after it on itself or the next.
        assert!(edit::doubles(&app, &shape, second, "move", (a.x, a.y)));
        assert!(edit::doubles(&app, &shape, second, "move", (c.x, c.y)));
        assert!(edit::doubles(&app, &shape, second, "insert", (b.x, b.y)));
        assert!(edit::doubles(&app, &shape, second, "insert", (c.x, c.y)));
        assert!(edit::doubles(&app, &shape, first, "insert", (b.x, b.y)));
        // A place between them is fine, and so is moving a vertex where it already is.
        let between = ((b.x + c.x) / 2.0, (b.y + c.y) / 2.0);
        assert!(!edit::doubles(&app, &shape, second, "move", between));
        assert!(!edit::doubles(&app, &shape, second, "insert", between));
        assert!(!edit::doubles(&app, &shape, second, "move", (b.x, b.y)));
        assert!(!edit::doubles(&app, &shape, second, "delete", (a.x, a.y)));
    }

    #[test]
    fn a_feature_under_the_cross_is_picked_and_its_vertices_moved_inserted_and_deleted() {
        let ctx = egui::Context::default();
        let mut app = RustGisApp::with_sample();
        let rivers = target(&mut app, GeometryType::Polyline);
        let shape = app.session.map().layer(rivers.id).unwrap().dataset.features[0].geometry.clone().unwrap();
        let feature = app.session.map().layer(rivers.id).unwrap().dataset.features[0].id;
        let (first, at) = shape.vertices()[1];
        // The cross on the river's second vertex, close in.
        run(&mut app, "map.goToXY", json!({"lon": at.x, "lat": at.y, "scale": 50_000}));
        frame(&mut app, &ctx, Sheet::Capture);
        assert_eq!(edit::feature_at_cross(&mut app, rivers.id), Some(feature));
        let row = edit::row_of(&app, rivers.id, feature).unwrap();
        let middle = app.map.view_rect.unwrap().0.center();
        assert_eq!(edit::vertex_near(&app, &shape, middle), Some(first));
        let vertices = shape.vertices().len();
        // Moved, then a vertex inserted after it (and picked), then that one deleted.
        let moved = (at.x + 0.001, at.y + 0.001);
        assert_eq!(edit::vertex(&mut app, rivers.id, row, first, "move", Some(moved)), Some(Some(first)));
        let now = edit::shape(&app, rivers.id, row).unwrap();
        assert_eq!(now.vertex(first).map(|c| (c.x, c.y)), Some(moved));
        let inserted = edit::vertex(&mut app, rivers.id, row, first, "insert", Some((at.x, at.y))).unwrap();
        assert_eq!(inserted, Some(VertexId::new(first.part, first.ring, first.index + 1)));
        assert_eq!(edit::shape(&app, rivers.id, row).unwrap().vertices().len(), vertices + 1);
        assert_eq!(edit::vertex(&mut app, rivers.id, row, inserted.unwrap(), "delete", None), Some(None));
        assert_eq!(edit::shape(&app, rivers.id, row).unwrap().vertices().len(), vertices);
        // The sheet draws in Edit mode with the feature and its vertex picked.
        set_form(
            &ctx,
            Form {
                shown: Some(rivers.instance),
                edit_mode: true,
                editing: Some(edit::Editing::feature(feature).picking(Some(first), edit::shape(&app, rivers.id, row).as_ref())),
                ..Form::default()
            },
        );
        frame(&mut app, &ctx, Sheet::Capture);
        assert_eq!(form(&ctx).editing.map(|e| e.feature), Some(feature));
        // Deleted, only it: the sheet lets it go.
        // With nothing selected anywhere it is deleted too (Pick Feature selects nothing).
        let cities = app.session.map().layers.iter().find(|l| l.geometry_type() == GeometryType::Point).unwrap().id;
        let before = count(&app, rivers.id);
        let last = before - 1;
        assert!(app.session.map().layers.iter().all(|l| l.selection.is_empty()));
        assert!(edit::delete_feature(&mut app, rivers.id, last));
        assert_eq!(count(&app, rivers.id), before - 1);
        run(&mut app, "edit.undo", Value::Null);
        assert_eq!(count(&app, rivers.id), before);
        // A selection on another layer is left as it was.
        run(&mut app, "select.rows", json!({"layer": cities, "rows": [0, 2]}));
        let cities_before = count(&app, cities);
        assert!(edit::delete_feature(&mut app, rivers.id, row));
        assert_eq!(count(&app, rivers.id), before - 1);
        assert_eq!(count(&app, cities), cities_before);
        assert_eq!(app.session.map().layer(cities).unwrap().selection, [0, 2]);
        assert_eq!(edit::row_of(&app, rivers.id, feature), None);
        frame(&mut app, &ctx, Sheet::Capture);
        assert_eq!(form(&ctx).editing, None);
        // Undone: it comes back.
        run(&mut app, "edit.undo", Value::Null);
        assert_eq!(count(&app, rivers.id), before);
    }

    #[test]
    fn with_snapping_on_the_cross_snaps_to_a_feature_in_reach() {
        let ctx = egui::Context::default();
        let mut app = RustGisApp::with_sample();
        let cities = target(&mut app, GeometryType::Point);
        let at = match app.session.map().layer(cities.id).unwrap().dataset.features[0].geometry.clone().unwrap() {
            rustgis_ui_egui::rustgis_engine::data::Geometry::Point(c) => c,
            other => panic!("{other:?}"),
        };
        run(&mut app, "map.goToXY", json!({"lon": at.x + 0.0005, "lat": at.y, "scale": 50_000}));
        frame(&mut app, &ctx, Sheet::Capture);
        let near = cross_place(&app).unwrap();
        // Off: no snapping. On: the city's place.
        run(&mut app, "edit.snapping", json!({"on": false}));
        assert_eq!(edit::snap(&mut app, near, None), None);
        run(&mut app, "edit.snapping", json!({"on": true}));
        let snapped = edit::snap(&mut app, near, None).unwrap();
        assert!((snapped.lon - at.x).abs() < 1e-9 && (snapped.lat - at.y).abs() < 1e-9, "{snapped:?}");
        // A point added then is where it snapped, and the sheet shows it snapped.
        frame(&mut app, &ctx, Sheet::Capture);
        let state = form(&ctx);
        assert!(state.snap.is_some());
        assert!(add_point(&mut app, cities.id, &state));
        // There to within a round trip through the map's projection.
        match app.session.map().layer(cities.id).unwrap().dataset.features.last().unwrap().geometry.clone() {
            Some(rustgis_ui_egui::rustgis_engine::data::Geometry::Point(c)) => {
                assert!((c.x - at.x).abs() < 1e-9 && (c.y - at.y).abs() < 1e-9)
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn positions_are_kept_coming_out_of_sight_while_a_track_is_recorded() {
        let ctx = egui::Context::default();
        let mut app = RustGisApp::with_sample();
        frame(&mut app, &ctx, Sheet::None);
        assert!(!app.keep_locating);
        start_track(&mut app, &ctx);
        assert!(app.locate);
        frame(&mut app, &ctx, Sheet::None);
        assert!(app.keep_locating);
        // Positioning ended by Android, the track still recording: no service out of sight.
        app.locate = false;
        frame(&mut app, &ctx, Sheet::None);
        assert!(!app.keep_locating);
        app.locate = true;
        frame(&mut app, &ctx, Sheet::None);
        assert!(app.keep_locating);
        // Stopped: no longer, and the track (too short here) is said, not kept.
        stop_track(&mut app, &ctx);
        frame(&mut app, &ctx, Sheet::None);
        assert!(!app.keep_locating);
    }

    /// On a phone with text fields of its own, a Capture field is a box that opens one when
    /// tapped, with the field's value, label and kind; what is typed there fills the field as it
    /// changes, and goes with the feature added. An answer for another field, or an earlier
    /// opening's close arriving after the field was opened again, fills or ends nothing; text for
    /// a field whose layer Capture no longer adds to is said, not kept.
    #[test]
    fn a_field_typed_into_with_the_systems_text_field_fills_the_feature() {
        let ctx = egui::Context::default();
        let mut app = RustGisApp::with_sample();
        let cities = target(&mut app, GeometryType::Point);
        run(&mut app, "edit.addField", json!({"layer": cities.id, "name": "note", "type": "text"}));
        let index = app.session.map().layer(cities.id).unwrap().dataset.field_index("note").unwrap();
        let asked: std::rc::Rc<std::cell::RefCell<Vec<TextEntry>>> = Default::default();
        let asking = asked.clone();
        app.services.enter_text = Some(Box::new(move |entry| {
            asking.borrow_mut().push(entry.clone());
            Ok(())
        }));
        set_form(&ctx, Form { shown: Some(cities.instance), values: [("note".to_string(), "Old".to_string())].into(), ..Form::default() });
        // The field's box is beside its label, which says where it is: on a tall window, as the
        // field added comes after the layer's own.
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(411.0, 2400.0));
        set_sheet(&ctx, Sheet::Capture);
        let mut label = None;
        for _ in 0..3 {
            let output = ctx.run_ui(egui::RawInput { screen_rect: Some(screen), ..Default::default() }, |ui| chrome(&mut app, ui));
            label = label.or_else(|| {
                output.shapes.iter().find_map(|painted| match &painted.shape {
                    egui::epaint::Shape::Text(text) if text.galley.text() == "note" => Some(text.visual_bounding_rect()),
                    _ => None,
                })
            });
            output.drop_without_applying_deltas();
        }
        let label = label.expect("the field's label is drawn");
        // Every frame on the same window: egui finds what a press is on by the frame before it.
        let draw = |app: &mut RustGisApp| {
            ctx.run_ui(egui::RawInput { screen_rect: Some(screen), ..Default::default() }, |ui| chrome(app, ui))
                .drop_without_applying_deltas();
        };
        let tap = |app: &mut RustGisApp| {
            draw(app);
            let at = egui::pos2(label.left() + 160.0, label.center().y);
            for pressed in [true, false] {
                let events = vec![
                    egui::Event::PointerMoved(at),
                    egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE },
                ];
                let input = egui::RawInput { screen_rect: Some(screen), events, ..Default::default() };
                ctx.run_ui(input, |ui| chrome(app, ui)).drop_without_applying_deltas();
            }
        };
        tap(&mut app);
        let first = entry_id(cities.instance, index, 0);
        assert_eq!(
            *asked.borrow(),
            vec![TextEntry { id: first.clone(), value: "Old".into(), label: "note".into(), kind: TextKind::Plain, done: "Done".into() }]
        );
        assert_eq!(form(&ctx).entering.map(|e| e.id), Some(first.clone()));
        // Typed: each change fills the field; another field's answer fills nothing.
        app.typed.push(Typed::Text { id: first.clone(), text: "Lan".into() });
        app.typed.push(Typed::Text { id: entry_id(cities.instance + 1, index, 0), text: "Elsewhere".into() });
        draw(&mut app);
        assert_eq!(form(&ctx).values.get("note").map(String::as_str), Some("Lan"));
        // Opened again before the first one's close arrived: that close ends nothing, and the
        // second one's text still fills the field.
        tap(&mut app);
        let second = entry_id(cities.instance, index, 1);
        assert_eq!(asked.borrow().last().map(|e| (e.id.clone(), e.value.clone())), Some((second.clone(), "Lan".into())));
        app.typed.push(Typed::Closed { id: first, failed: None });
        app.typed.push(Typed::Text { id: second.clone(), text: "Lantern\tpost".into() });
        app.typed.push(Typed::Closed { id: second.clone(), failed: None });
        app.typed.push(Typed::Text { id: second, text: "After it closed".into() });
        draw(&mut app);
        let state = form(&ctx);
        assert!(app.typed.is_empty());
        assert_eq!(state.entering, None);
        assert_eq!(state.values.get("note").map(String::as_str), Some("Lantern\tpost"));
        assert!(add_point(&mut app, cities.id, &state));
        let layer = app.session.map().layer(cities.id).unwrap();
        assert_eq!(layer.dataset.value(layer.dataset.features.len() - 1, index).to_string(), "Lantern\tpost");
        // Capture adds to another layer while the box is open: what is typed is said, not kept.
        tap(&mut app);
        let third = asked.borrow().last().unwrap().id.clone();
        let polygons = target(&mut app, GeometryType::Polygon);
        let mark = app.session.notice_mark();
        app.typed.push(Typed::Text { id: third, text: "Lost".into() });
        draw(&mut app);
        let state = form(&ctx);
        assert_eq!(state.shown, Some(polygons.instance));
        assert!(!state.values.values().any(|v| v == "Lost"));
        assert_eq!(state.entering, None);
        let said: Vec<String> = app.session.notices_since(mark).iter().map(|n| n.text.clone()).collect();
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(said[0].starts_with("What was typed for note wasn't kept: Capture no longer adds to its layer"), "{said:?}");
    }

    #[test]
    fn a_photo_taken_goes_with_the_next_feature_or_to_the_feature_being_edited() {
        let ctx = egui::Context::default();
        let mut app = RustGisApp::with_sample();
        let cities = target(&mut app, GeometryType::Point);
        assert!(photos::field(app.session.map().layer(cities.id).unwrap()).is_none());
        // A photo arrives while the sheet is up: kept for the next feature.
        app.photos.push("/data/photos/a.jpg".into());
        set_form(&ctx, Form { shown: Some(cities.instance), ..Form::default() });
        frame(&mut app, &ctx, Sheet::Capture);
        assert!(app.photos.is_empty());
        let state = form(&ctx);
        assert_eq!(state.photos.len(), 1);
        // Added with it: the layer gets a photos field, and the point names the photo.
        let mark = app.session.notice_mark();
        assert!(add_point(&mut app, cities.id, &state));
        let layer = app.session.map().layer(cities.id).unwrap();
        let field = photos::field(layer).unwrap();
        let row = layer.dataset.features.len() - 1;
        let column = layer.dataset.field_index(&field).unwrap();
        assert_eq!(layer.dataset.value(row, column).to_string(), "/data/photos/a.jpg");
        assert!(app.session.notices_since(mark).iter().any(|n| n.text.contains("photos field")));
        // In Edit mode, a photo arriving goes to the feature picked, after the one it has.
        let feature = layer.dataset.features[row].id;
        set_form(
            &ctx,
            Form { shown: Some(cities.instance), edit_mode: true, editing: Some(edit::Editing::feature(feature)), ..Form::default() },
        );
        app.photos.push("/data/photos/b.jpg".into());
        frame(&mut app, &ctx, Sheet::Capture);
        let layer = app.session.map().layer(cities.id).unwrap();
        assert_eq!(layer.dataset.value(row, column).to_string(), "/data/photos/a.jpg;/data/photos/b.jpg");
        assert!(form(&ctx).photos.is_empty());
        // One edit: Undo takes the second photo off again.
        run(&mut app, "edit.undo", Value::Null);
        let layer = app.session.map().layer(cities.id).unwrap();
        assert_eq!(layer.dataset.value(row, column).to_string(), "/data/photos/a.jpg");
    }

    #[test]
    fn a_subtype_the_template_chooses_fills_a_required_field_with_its_default() {
        let mut app = RustGisApp::with_sample();
        let cities = target(&mut app, GeometryType::Point);
        run(&mut app, "edit.addField", json!({"layer": cities.id, "name": "kind", "type": "long"}));
        run(&mut app, "edit.addField", json!({"layer": cities.id, "name": "status", "type": "text"}));
        let subtypes =
            json!({"layer": cities.id, "field": "kind", "subtypes": [{"code": 1, "name": "Well", "defaults": {"status": "active"}}]});
        run(&mut app, "data.subtypes", subtypes);
        run(
            &mut app,
            "data.attributeRule",
            json!({"layer": cities.id, "name": "status", "kind": "constraint", "expression": "status IS NOT NULL"}),
        );
        run(&mut app, "edit.template", json!({"layer": cities.id, "name": "Well", "defaults": {"kind": 1}}));
        let layer = app.session.map().layer(cities.id).unwrap();
        // The template gives the subtype, whose default fills the required field.
        let state = Form { template: Some("Well".into()), ..Form::default() };
        assert!(defaulted(layer, &state, "status"));
        assert!(!defaulted(layer, &Form::default(), "status"));
        assert!(add_point(&mut app, cities.id, &state));
    }

    #[test]
    fn an_average_being_taken_stops_with_the_position() {
        let ctx = egui::Context::default();
        let mut app = RustGisApp::with_sample();
        app.locate = true;
        track::set_field(&ctx, track::Field { averaging: Some(std::sync::Arc::default()), ..Default::default() });
        frame(&mut app, &ctx, Sheet::Capture);
        assert!(track::field(&ctx).averaging.is_some());
        // Hidden (or ended by the system): the average goes with it.
        app.locate = false;
        frame(&mut app, &ctx, Sheet::Capture);
        assert!(track::field(&ctx).averaging.is_none());
    }

    #[test]
    fn a_photos_field_made_for_a_feature_refused_is_taken_away_again() {
        let mut app = RustGisApp::with_sample();
        let cities = target(&mut app, GeometryType::Point);
        let fields = app.session.map().layer(cities.id).unwrap().dataset.fields.len();
        // A line into a point layer: refused, after the photos field was made for it.
        let line = geometry(GeometryType::Polyline, &[(1.0, 1.0), (2.0, 2.0)]).unwrap();
        let form = Form { photos: vec!["/data/photos/c.jpg".into()], ..Form::default() };
        let mark = app.session.notice_mark();
        assert!(!add_feature(&mut app, cities.id, &form, line));
        assert_eq!(app.session.map().layer(cities.id).unwrap().dataset.fields.len(), fields);
        assert!(app.session.notices_since(mark).iter().any(|n| n.text.contains("taken away again")));
        // Added, the field stays and the photo is counted once.
        let mark = app.session.notice_mark();
        assert!(add_point(&mut app, cities.id, &form));
        assert_eq!(app.session.map().layer(cities.id).unwrap().dataset.fields.len(), fields + 1);
        assert!(app.session.notices_since(mark).iter().any(|n| n.text.ends_with("Its 1 photo(s) are named in its photos field.")));
    }

    #[test]
    fn a_photos_field_the_layer_has_stays_typeable_and_one_not_of_text_is_said() {
        let mut app = RustGisApp::with_sample();
        let cities = target(&mut app, GeometryType::Point);
        run(&mut app, "edit.addField", json!({"layer": cities.id, "name": "Photo", "type": "text"}));
        // What is typed in it stays, the photo after it.
        let mut state = Form { photos: vec!["/data/photos/b.jpg".into()], ..Form::default() };
        state.values.insert("Photo".into(), "site.jpg".into());
        let mark = app.session.notice_mark();
        assert!(add_point(&mut app, cities.id, &state));
        // The field the photo went in, the layer's own, is named.
        assert!(app.session.notices_since(mark).iter().any(|n| n.text.ends_with("Its 1 photo(s) are named in its Photo field.")));
        let layer = app.session.map().layer(cities.id).unwrap();
        let column = layer.dataset.field_index("Photo").unwrap();
        assert_eq!(layer.dataset.value(layer.dataset.features.len() - 1, column).to_string(), "site.jpg;/data/photos/b.jpg");
        // A field of that name that isn't text: nothing is added, and the field is named.
        let rivers = target(&mut app, GeometryType::Polyline);
        run(&mut app, "edit.addField", json!({"layer": rivers.id, "name": "photos", "type": "long"}));
        let mark = app.session.notice_mark();
        let line = geometry(GeometryType::Polyline, &[(1.0, 1.0), (2.0, 2.0)]).unwrap();
        assert!(!add_feature(&mut app, rivers.id, &Form { photos: vec!["/data/photos/c.jpg".into()], ..Form::default() }, line));
        let said: Vec<_> = app.session.notices_since(mark).iter().filter(|n| n.error).map(|n| n.text.clone()).collect();
        assert!(said.iter().any(|t| t.contains("photos") && t.contains("isn't a text field")), "{said:?}");
    }

    #[test]
    fn a_picked_vertex_an_undo_moved_or_took_away_is_let_go() {
        use rustgis_ui_egui::rustgis_engine::data::{Coord, Geometry};
        let line = |points: &[(f64, f64)]| Geometry::Line(vec![points.iter().map(|(x, y)| Coord::new(*x, *y)).collect()]);
        let abc = line(&[(0.0, 0.0), (1.0, 0.0), (2.0, 0.0)]);
        // B picked, then N added after it and picked: index 2 is N.
        let with_n = line(&[(0.0, 0.0), (1.0, 0.0), (1.5, 1.0), (2.0, 0.0)]);
        let picked = edit::Editing::feature(7).picking(Some(VertexId::new(0, 0, 2)), Some(&with_n));
        assert_eq!(picked.checked(Some(&with_n)), picked);
        // Undone: index 2 is now C, elsewhere than N was: let go.
        assert_eq!(picked.checked(Some(&abc)).vertex, None);
        // N was the last vertex: the index is gone, and so is the pick.
        let n_last = line(&[(0.0, 0.0), (1.0, 0.0), (2.0, 0.0), (3.0, 0.0)]);
        let last = edit::Editing::feature(7).picking(Some(VertexId::new(0, 0, 3)), Some(&n_last));
        assert_eq!(last.checked(Some(&abc)).vertex, None);
        // A vertex the shape doesn't have isn't picked at all.
        assert_eq!(edit::Editing::feature(7).picking(Some(VertexId::new(0, 0, 9)), Some(&abc)).vertex, None);
    }

    /// A feature with a place that can't be given in longitude/latitude (#2032 C2f-4b) has no
    /// shape to edit here, and why is said, naming it; a placed one has.
    #[test]
    fn a_feature_with_no_place_in_longitude_latitude_says_why_it_cant_be_edited() {
        use rustgis_ui_egui::rustgis_engine::data::{Coord, Geometry, raster::Crs};
        let mut app = RustGisApp::with_sample();
        let cities = target(&mut app, GeometryType::Point);
        let layer = app.session.map_mut().layer_mut(cities.id).unwrap();
        layer.dataset.hold_in(Crs::from_epsg(8857).unwrap()).unwrap();
        let id = layer.dataset.features[0].id;
        layer.dataset.features[0].geometry = Some(Geometry::Point(Coord::new(1e9, 0.0)));
        assert!(edit::shape_ref(&app, cities.id, 0).is_none());
        let why = edit::unplaced(&app, cities.id, 0).unwrap();
        assert!(why.contains(&format!("feature {id} has a place")) && why.contains("can't be edited there"), "{why}");
        assert!(edit::shape_ref(&app, cities.id, 1).is_some());
        assert_eq!(edit::unplaced(&app, cities.id, 1), None);
    }

    #[test]
    fn move_point_here_moves_a_point_and_refuses_a_multipoint() {
        let mut app = RustGisApp::with_sample();
        let cities = target(&mut app, GeometryType::Point);
        assert!(edit::move_point(&mut app, cities.id, 0, (12.0, 34.0)));
        let moved = app.session.map().layer(cities.id).unwrap().dataset.features[0].geometry.clone();
        assert!(matches!(moved, Some(rustgis_ui_egui::rustgis_engine::data::Geometry::Point(c)) if c.x == 12.0 && c.y == 34.0));
        // A multipoint in a point layer: nothing moves, and that is said.
        let multi = json!({"layer": cities.id, "geometry": {"type": "MultiPoint", "coordinates": [[1.0, 1.0], [2.0, 2.0]]}});
        run(&mut app, "edit.addFeature", multi);
        let row = count(&app, cities.id) - 1;
        let before = app.session.map().layer(cities.id).unwrap().dataset.features[row].geometry.clone();
        let mark = app.session.notice_mark();
        assert!(!edit::move_point(&mut app, cities.id, row, (5.0, 5.0)));
        assert_eq!(app.session.map().layer(cities.id).unwrap().dataset.features[row].geometry, before);
        assert!(app.session.notices_since(mark).iter().any(|n| n.error && n.text.contains("multipoint")));
    }

    #[test]
    fn a_track_that_cant_be_added_is_kept_being_recorded() {
        use rustgis_ui_egui::rustgis_engine::platform::Fix;
        let ctx = egui::Context::default();
        let mut app = RustGisApp::with_sample();
        start_track(&mut app, &ctx);
        // A position the line can't hold makes the layer refuse the track.
        for lon in [f64::NAN, 10.0, 10.01] {
            app.report_position(Fix { lon, lat: 5.0, accuracy: Some(3.0), time: None, elapsed_realtime_nanos: None });
        }
        frame(&mut app, &ctx, Sheet::None);
        let mark = app.session.notice_mark();
        assert!(!stop_track(&mut app, &ctx));
        assert_eq!(track::field(&ctx).track.map(|t| t.points.len()), Some(3));
        // Why it was refused is said, then that it is kept.
        let said: Vec<String> = app.session.notices_since(mark).iter().filter(|n| n.error).map(|n| n.text.clone()).collect();
        assert!(said.len() >= 2 && said.last().is_some_and(|t| t.contains("kept")), "{said:?}");
        // The More sheet has room for its row while positioning is off.
        app.locate = false;
        frame(&mut app, &ctx, Sheet::More);
        // With positioning off, the next position starts a run of its own.
        assert!(track::field(&ctx).track.is_some_and(|t| t.broken));
    }

    #[test]
    fn positions_dropped_past_the_cap_are_said_while_a_track_is_recorded() {
        use rustgis_ui_egui::rustgis_engine::platform::Fix;
        let ctx = egui::Context::default();
        let mut app = RustGisApp::with_sample();
        start_track(&mut app, &ctx);
        for i in 0..rustgis_ui_egui::FIXES_KEPT + 5 {
            app.report_position(Fix {
                lon: 10.0,
                lat: 5.0,
                accuracy: Some(3.0),
                time: i64::try_from(i).ok(),
                elapsed_realtime_nanos: None,
            });
        }
        let mark = app.session.notice_mark();
        frame(&mut app, &ctx, Sheet::None);
        assert!(app.session.notices_since(mark).iter().any(|n| n.text.starts_with("5 of the oldest positions")));
        assert_eq!(app.fixes_dropped, 0);
    }

    #[test]
    fn the_desktop_interface_adds_the_track_being_recorded_first() {
        use rustgis_ui_egui::rustgis_engine::platform::Fix;
        let ctx = egui::Context::default();
        let mut app = RustGisApp::with_sample();
        start_track(&mut app, &ctx);
        for i in 0..3 {
            app.report_position(Fix {
                lon: 10.0 + f64::from(i) * 0.01,
                lat: 5.0,
                accuracy: Some(3.0),
                time: Some(i64::from(i) * 1000),
                elapsed_realtime_nanos: None,
            });
        }
        frame(&mut app, &ctx, Sheet::None);
        // As the More sheet's row does: the track is added, then the interface goes.
        let mut state = track::field(&ctx);
        assert_eq!(state.track.as_ref().map(|t| t.points.len()), Some(3));
        stop_track(&mut app, &ctx);
        state = track::field(&ctx);
        assert!(state.track.is_none());
        assert!(app.session.map().layers.iter().any(|l| l.name == track::TRACKS && l.dataset.features.len() == 1));
    }

    #[test]
    fn what_is_typed_for_one_layer_is_dropped_for_another() {
        let mut state = Form::default().for_layer(1);
        state.values.insert("name".into(), "Camp".into());
        // The same layer keeps it; another drops it.
        state.vertices.push((1.0, 2.0));
        state.template = Some("Camp".into());
        let state = state.for_layer(1);
        assert_eq!(state.template.as_deref(), Some("Camp"));
        assert_eq!((state.values.len(), state.vertices.len()), (1, 1));
        let other = state.for_layer(2);
        assert!(other.values.is_empty() && other.vertices.is_empty() && other.template.is_none());
    }

    #[test]
    fn what_is_typed_becomes_the_fields_kind_of_value() {
        use rustgis_ui_egui::rustgis_engine::data::FieldType;
        assert_eq!(typed("  ", FieldType::Text), Ok(Value::Null));
        assert_eq!(typed(" 42 ", FieldType::Integer), Ok(json!(42)));
        assert_eq!(typed("4.5", FieldType::Double), Ok(json!(4.5)));
        assert_eq!(typed("true", FieldType::Boolean), Ok(json!(true)));
        assert_eq!(typed("false", FieldType::Boolean), Ok(json!(false)));
        assert_eq!(typed("soon", FieldType::Date), Ok(json!("soon")));
        // Not what the field holds: an error, not a value the command would store as nothing.
        assert_eq!(typed("many", FieldType::Integer), Err(()));
        assert_eq!(typed("4.5", FieldType::Integer), Err(()));
        assert_eq!(typed("inf", FieldType::Double), Err(()));
        assert_eq!(typed("yes", FieldType::Boolean), Err(()));
    }

    #[test]
    fn a_refused_command_is_said() {
        let mut app = RustGisApp::with_sample();
        let before = app.session.notice_mark();
        run(&mut app, "layer.zoomTo", json!({"layer": "no such layer"}));
        // Once: the front end reports a refusal itself.
        assert_eq!(app.session.notices_since(before).iter().filter(|n| n.error).count(), 1);
    }
}
