//! A plugin's panel (`panel` in its `plugin.json`): a right pane of texts, checkboxes, sliders,
//! buttons, trees, dropdowns, date pickers, range sliders and picks bound to the plugin's
//! commands.
//!
//! The session keeps the panel's state ([`rustgis_engine::plugins::PanelState`]); this draws it
//! and turns the user's changes into commands: a widget with a command runs
//! `plugin:<plugin>/<command>` with `{"panel": {"<widget>": value}}`, one without sets the value
//! with `plugin.panel`. A slider or a range runs its command when it is let go (or stepped, or
//! moved with the keys), never while it is dragged: until then the pane shows the value it would
//! take. A date runs its command when a day is picked in its calendar, or a date typed is
//! confirmed (Enter, or leaving the box). A pick waits for the map: the next click there (or a
//! box dragged) is its value, and Esc, another tool, or the panel closing stops it waiting.

use egui::{Align2, CornerRadius, RichText, Sense, Stroke, Ui, pos2, vec2};
use std::collections::{HashMap, HashSet};

use rustgis_engine::Tool;
use rustgis_engine::geom::{Coord, Extent};
use rustgis_engine::plugins::{Manifest, PanelItemDecl, PanelState, TreeNode, command_id};
use serde_json::{Value, json};

use crate::theme::{Tokens, regular, semibold};
use crate::{RightPane, RustGisApp, icons};

/// The pane's state in the front end.
#[derive(Default)]
pub struct State {
    /// The plugin whose panel the pane shows.
    pub plugin: Option<String>,
    /// A slider being dragged: plugin, widget and the value it shows (not yet sent).
    dragging: Option<(String, String, i64)>,
    /// A range being dragged: plugin, widget and the values it shows (not yet sent).
    range_drag: Option<(String, String, i64, i64)>,
    /// What each date box holds while it is being typed in, by widget id: kept only while the box
    /// shows, so a date the plugin sets since shows instead.
    date_text: HashMap<String, String>,
    /// The frame the pane was last drawn on: one hidden in between drops what was being typed or
    /// dragged in it.
    drawn: u64,
    /// The month each date's calendar shows (year, month), by widget id.
    calendar: HashMap<String, (i32, u32)>,
    /// Each tree's open nodes, and what its search box holds, by widget id.
    open: HashMap<String, HashSet<String>>,
    search: HashMap<String, String>,
    /// Where each tree's chosen node last was (the branches above it, then it): a node chosen
    /// since, or moved (by the plugin or a script), opens the branches above it, once, so it
    /// shows and they can still be closed.
    revealed: HashMap<String, Vec<String>>,
    /// A pick waiting for the map: plugin and widget.
    picking: Option<(String, String)>,
}

/// The panel's title (its tab), in the interface language.
pub fn title(app: &RustGisApp) -> String {
    let lang = app.lang().code();
    app.ui
        .plugin_panel
        .plugin
        .as_deref()
        .and_then(rustgis_engine::plugins::get)
        .and_then(|p| p.manifest.panel.as_ref().map(|d| p.manifest.translate(lang, &d.title).to_string()))
        .unwrap_or_else(|| tl!("Plugin").to_string())
}

/// `ui.pane.plugin`: `{"id": plugin}` opens that plugin's panel (running its state command) or,
/// when it is in view, closes it; with no id, shows or hides the panel last opened.
pub fn open(app: &mut RustGisApp, p: &Value) -> Result<Value, String> {
    let Some(id) = p.get("id").and_then(Value::as_str) else {
        if app.ui.plugin_panel.plugin.is_none() {
            return Err("name the plugin whose panel to open: {\"id\": plugin id}".into());
        }
        app.ui.toggle_right(RightPane::Plugin);
        return Ok(Value::Null);
    };
    if app.ui.right == Some(RightPane::Plugin) && app.ui.plugin_panel.plugin.as_deref() == Some(id) {
        app.ui.close_right(RightPane::Plugin);
        return Ok(Value::Null);
    }
    let plugin = rustgis_engine::plugins::get(id).ok_or_else(|| format!("no plugin '{id}'"))?;
    if !plugin.enabled {
        return Err(format!("the plugin {} is disabled", plugin.manifest.name));
    }
    if plugin.manifest.panel.is_none() {
        return Err(format!("{} has no panel", plugin.manifest.name));
    }
    app.ui.plugin_panel = State { plugin: Some(id.to_string()), ..State::default() };
    app.ui.open_right(RightPane::Plugin);
    // The panel opens even if its state command fails: the error is in the status bar.
    if let Err(e) = app.run("plugin.panel", json!({"id": id, "refresh": true})) {
        app.session.notify(e, true);
    }
    Ok(Value::Null)
}

fn run(app: &mut RustGisApp, id: &str, p: Value) {
    if let Err(e) = app.run_by_user(id, p) {
        app.session.notify(e, true);
    }
}

/// Sends widget `id`'s new value: through its command, or kept for the plugin's next one.
fn send(app: &mut RustGisApp, plugin: &str, item: &PanelItemDecl, value: Value) {
    let values = json!({ item.key(): value });
    if item.command.is_empty() {
        run(app, "plugin.panel", json!({"id": plugin, "values": values}));
    } else {
        run(app, &command_id(plugin, &item.command), json!({ "panel": values }));
    }
}

pub fn show(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    let Some(pid) = app.ui.plugin_panel.plugin.clone() else {
        ui.label(RichText::new(tl!("No plugin panel is open.")).color(t.text_dim));
        return;
    };
    // A plugin turned off or removed takes its panel with it.
    let plugin = rustgis_engine::plugins::get(&pid).filter(|p| p.enabled);
    let (Some(plugin), Some(state)) = (plugin, app.session.plugin_panel(&pid).cloned()) else {
        app.ui.plugin_panel = State::default();
        app.ui.close_right(RightPane::Plugin);
        return;
    };
    let m = &plugin.manifest;
    let Some(decl) = m.panel.as_ref() else { return };
    let lang = app.lang().code();
    let tr = |s: &str| m.translate(lang, s).to_string();
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
        let icon = if icons::ALL.contains(&decl.icon.as_str()) { decl.icon.as_str() } else { "plugin" };
        icons::paint(ui.painter(), r, icon, t.icon, t.accent);
        ui.label(RichText::new(tr(&decl.title)).font(semibold(15.0)));
    });
    if !decl.help.is_empty() {
        ui.label(RichText::new(tr(&decl.help)).size(11.5).color(t.text_dim));
    }
    ui.add_space(4.0);
    ui.separator();
    ui.add_space(4.0);
    let items: Vec<&PanelItemDecl> = decl.items.iter().filter(|d| state.items.get(d.key()).is_none_or(|s| s.visible)).collect();
    // What was being typed or dragged in a pane, or a widget, out of sight since is let go: the
    // box would otherwise show the old text over what the plugin set meanwhile.
    let frame = ui.ctx().cumulative_frame_nr();
    let pane = &mut app.ui.plugin_panel;
    if pane.drawn.saturating_add(1) < frame {
        pane.date_text.clear();
        pane.dragging = None;
        pane.range_drag = None;
    }
    pane.drawn = frame;
    pane.date_text.retain(|k, _| items.iter().any(|d| d.kind == "date" && d.key() == k && enabled(d, &state)));
    let mut i = 0;
    while let Some(item) = items.get(i).copied() {
        // Buttons side by side, wrapping, as many as follow each other.
        if item.kind == "button" {
            let n = items.iter().skip(i).take_while(|d| d.kind == "button").count();
            ui.horizontal_wrapped(|ui| {
                for b in items.iter().skip(i).take(n) {
                    button(app, ui, &pid, m, b, &state, &tr);
                }
            });
            ui.add_space(8.0);
            i += n;
            continue;
        }
        match item.kind.as_str() {
            "pick" => pick_item(app, ui, &pid, m, item, &state, &tr, t),
            "text" => text(ui, item, &state, &tr, t),
            "checkbox" => checkbox(app, ui, &pid, item, &state, &tr),
            "slider" => slider(app, ui, &pid, item, &state, &tr, t),
            "tree" => tree(app, ui, &pid, item, &state, &tr, t),
            "choice" => choice(app, ui, &pid, item, &state, &tr, t),
            "date" => date(app, ui, &pid, item, &state, &tr, t),
            "range" => range(app, ui, &pid, item, &state, &tr, t),
            _ => {}
        }
        ui.add_space(8.0);
        i += 1;
    }
}

/// A widget's label: what the plugin set, or the declared one (translated).
fn label_of(item: &PanelItemDecl, state: &PanelState, tr: &dyn Fn(&str) -> String) -> String {
    state.items.get(item.key()).and_then(|s| s.label.clone()).unwrap_or_else(|| tr(&item.label))
}

fn enabled(item: &PanelItemDecl, state: &PanelState) -> bool {
    state.items.get(item.key()).is_none_or(|s| s.enabled)
}

fn text(ui: &mut Ui, item: &PanelItemDecl, state: &PanelState, tr: &dyn Fn(&str) -> String, t: &Tokens) {
    let label = label_of(item, state, tr);
    if !label.is_empty() {
        ui.label(RichText::new(label).size(11.0).color(t.text_dim));
    }
    let body = state.items.get(item.key()).map(|s| s.text.as_str()).unwrap_or_default();
    let color = if enabled(item, state) { t.text } else { t.text_disabled };
    let resp = ui.add(egui::Label::new(RichText::new(if body.is_empty() { "—" } else { body }).size(12.5).color(color)).wrap());
    if !item.help.is_empty() {
        resp.on_hover_text(tr(&item.help));
    }
}

fn checkbox(app: &mut RustGisApp, ui: &mut Ui, pid: &str, item: &PanelItemDecl, state: &PanelState, tr: &dyn Fn(&str) -> String) {
    let mut on = state.checked(item.key());
    let resp = ui.add_enabled(enabled(item, state), egui::Checkbox::new(&mut on, label_of(item, state, tr)));
    let resp = if item.help.is_empty() { resp } else { resp.on_hover_text(tr(&item.help)) };
    if resp.changed() {
        send(app, pid, item, json!(on));
    }
}

/// A button's or a pick's label, on one line: what the plugin set, the declared one, or its
/// command's; and its help: the declared one or its command's.
fn button_label(m: &Manifest, item: &PanelItemDecl, state: &PanelState, tr: &dyn Fn(&str) -> String) -> (String, String) {
    let cmd = m.commands.iter().find(|c| c.id == item.command);
    let label = state
        .items
        .get(item.key())
        .and_then(|s| s.label.clone())
        .or_else(|| (!item.label.is_empty()).then(|| tr(&item.label)))
        .or_else(|| cmd.as_ref().map(|c| tr(&c.label)))
        .unwrap_or_default()
        .replace('\n', " ");
    let help = if item.help.is_empty() { cmd.as_ref().map(|c| tr(&c.help)).unwrap_or_default() } else { tr(&item.help) };
    (label, help)
}

fn button(
    app: &mut RustGisApp,
    ui: &mut Ui,
    pid: &str,
    m: &Manifest,
    item: &PanelItemDecl,
    state: &PanelState,
    tr: &dyn Fn(&str) -> String,
) {
    let (label, help) = button_label(m, item, state, tr);
    let resp = ui.add_enabled(enabled(item, state), egui::Button::new(label).min_size(vec2(64.0, 26.0)));
    let resp = if help.is_empty() { resp } else { resp.on_hover_text(help) };
    if resp.clicked() {
        run(app, &command_id(pid, &item.command), Value::Null);
    }
}

/// Where a pick's value is, in words: a point's degrees, or a box's south-west and north-east
/// corners.
fn place_text(v: &serde_json::Map<String, Value>) -> Option<String> {
    let n = |v: Option<&Value>| v.and_then(Value::as_f64);
    if let Some(b) = v.get("bbox").and_then(Value::as_array) {
        let (w, s, e, north) = (n(b.first())?, n(b.get(1))?, n(b.get(2))?, n(b.get(3))?);
        let (sw, ne) = (rustgis_engine::geom::format_lonlat(Coord::new(w, s)), rustgis_engine::geom::format_lonlat(Coord::new(e, north)));
        return Some(crate::i18n::fmt("{southwest} to {northeast}", &[("southwest", &sw), ("northeast", &ne)]));
    }
    Some(rustgis_engine::geom::format_lonlat(Coord::new(n(v.get("lon"))?, n(v.get("lat"))?)))
}

/// A pick: a button that waits for a place on the map (a click, or a box dragged for a box), then
/// runs the widget's command with it. Clicked again while it waits, it stops waiting. Under it,
/// what it waits for, or the place picked last.
#[allow(clippy::too_many_arguments)]
fn pick_item(
    app: &mut RustGisApp,
    ui: &mut Ui,
    pid: &str,
    m: &Manifest,
    item: &PanelItemDecl,
    state: &PanelState,
    tr: &dyn Fn(&str) -> String,
    t: &crate::theme::Tokens,
) {
    let id = item.key();
    let waiting = app.ui.plugin_panel.picking.as_ref().is_some_and(|(p, i)| p == pid && i == id);
    let (label, help) = button_label(m, item, state, tr);
    let on = enabled(item, state);
    let resp = ui.add_enabled(on, egui::Button::new(format!("⌖ {label}")).selected(waiting).min_size(vec2(64.0, 26.0)));
    let resp = if help.is_empty() { resp } else { resp.on_hover_text(help) };
    if resp.clicked() {
        if waiting {
            app.ui.plugin_panel.picking = None;
        } else {
            // Clicks and drags on the map are the pick's, not another tool's: Explore, whose
            // drags still pan the map to find the place (for a point).
            if app.session.tool != Tool::Explore
                && let Err(e) = app.run("tool.set", json!({"tool": "explore"}))
            {
                app.session.notify(e, true);
                return;
            }
            app.map.popup = None;
            app.ui.plugin_panel.picking = Some((pid.to_string(), id.to_string()));
        }
    }
    let (line, color) = if waiting {
        let asks = if item.picks_box() { tl!("Drag a box on the map · Esc cancels") } else { tl!("Click the map · Esc cancels") };
        (Some(asks.to_string()), t.accent_text)
    } else {
        (state.picked(id).and_then(place_text), t.text_dim)
    };
    if let Some(line) = line {
        ui.add(egui::Label::new(RichText::new(line).size(11.0).color(color)).wrap());
    }
}

/// The pick waiting for the map, while it still can: its plugin's panel is open and the plugin
/// on, the widget is a pick that shows and is enabled, and the Explore tool is on (any other
/// tool, Esc among the ways to choose one, ends it). Anything else ends it.
fn waiting_pick(app: &mut RustGisApp) -> Option<(String, PanelItemDecl)> {
    let (pid, id) = app.ui.plugin_panel.picking.clone()?;
    let open = app.ui.right == Some(RightPane::Plugin) && app.ui.plugin_panel.plugin.as_deref() == Some(pid.as_str());
    let item = (open && app.session.tool == Tool::Explore)
        .then(|| rustgis_engine::plugins::get(&pid))
        .flatten()
        .filter(|p| p.enabled)
        .and_then(|p| p.manifest.panel.as_ref().and_then(|d| d.item(&id)).filter(|d| d.kind == "pick").cloned())
        .filter(|_| app.session.plugin_panels.get(&pid).and_then(|s| s.items.get(&id)).is_none_or(|s| s.enabled && s.visible));
    if item.is_none() {
        app.ui.plugin_panel.picking = None;
    }
    item.map(|i| (pid, i))
}

/// Ends a pick waiting for the map (another tool was chosen: Esc chooses Explore), dropping a box
/// being dragged for it.
pub(crate) fn cancel_pick(app: &mut RustGisApp) {
    if app.ui.plugin_panel.picking.take().is_some() {
        app.map.cancel_box_drag();
    }
}

/// What a pick waiting for the map asks for, for the map's hint and the status bar.
pub(crate) fn pick_hint(app: &mut RustGisApp) -> Option<String> {
    let (pid, item) = waiting_pick(app)?;
    let plugin = rustgis_engine::plugins::get(&pid)?;
    let state = app.session.plugin_panels.get(&pid)?;
    let lang = app.lang().code();
    let tr = |s: &str| plugin.manifest.translate(lang, s).to_string();
    let (label, _) = button_label(&plugin.manifest, &item, state, &tr);
    Some(if item.picks_box() {
        crate::i18n::fmt("{pick}: drag a box on the map · Esc cancels", &[("pick", &label)])
    } else {
        crate::i18n::fmt("{pick}: click the map · Esc cancels", &[("pick", &label)])
    })
}

/// Whether a pick waits for a box: a drag on the map draws one.
pub(crate) fn picks_box(app: &mut RustGisApp) -> bool {
    waiting_pick(app).is_some_and(|(_, i)| i.picks_box())
}

/// A longitude in -180..180.
fn wrap(lon: f64) -> f64 {
    let l = (lon + 180.0).rem_euclid(360.0) - 180.0;
    if l == -180.0 && lon > 0.0 { 180.0 } else { l }
}

/// The place picked: the pick stops waiting and runs its command with it (or keeps it for the
/// plugin's next one).
fn picked(app: &mut RustGisApp, pid: &str, item: &PanelItemDecl, value: Value) {
    app.ui.plugin_panel.picking = None;
    send(app, pid, item, value);
}

/// A click on the map at `c`, for a pick waiting for a point. A pick waiting for a box takes the
/// click too, and goes on waiting for a drag. False when no pick waits.
pub(crate) fn map_click(app: &mut RustGisApp, c: Coord) -> bool {
    let Some((pid, item)) = waiting_pick(app) else { return false };
    if !item.picks_box() {
        picked(app, &pid, &item, json!({"lon": wrap(c.x), "lat": c.y.clamp(-90.0, 90.0)}));
    }
    true
}

/// A box dragged on the map, `grid` the places on a `side` by `side` grid over it in degrees
/// (column by column, `None` off the globe) and `pole` the latitude of a pole inside it (the
/// globe's), for a pick waiting for one; false when none waits.
pub(crate) fn box_picked(app: &mut RustGisApp, grid: &[Option<Coord>], side: usize, pole: Option<f64>) -> bool {
    let Some((pid, item)) = waiting_pick(app).filter(|(_, i)| i.picks_box()) else { return false };
    let globe = app.session.map().projection == rustgis_engine::map::Projection::Globe;
    let Some(bbox) = box_of(grid, side, globe, pole) else { return false };
    picked(app, &pid, &item, json!({"bbox": bbox}));
    true
}

/// The box `[west, south, east, north]` around the places on `grid` (degrees, `side` by `side`,
/// column by column, `None` off the globe): a box wider than the world is the world, and one
/// across the antimeridian has a west longitude greater than its east. A flat map gives
/// longitudes past ±180 across it (its copies of the world); the globe wraps them, so there each
/// place's is read from a neighbour's already read, and a box over more than 180° of longitude
/// keeps its side. A box around a pole (`pole`, its latitude) reaches it, all the way round.
fn box_of(grid: &[Option<Coord>], side: usize, globe: bool, pole: Option<f64>) -> Option<[f64; 4]> {
    if grid.len() != side * side {
        return None;
    }
    let start = grid.iter().position(Option::is_some)?;
    let first = grid[start]?.x;
    let mut read: Vec<Option<f64>> = vec![None; grid.len()];
    if globe {
        read[start] = Some(first);
        let mut todo = vec![start];
        while let Some(k) = todo.pop() {
            let Some(at) = read[k] else { continue };
            let (i, j) = (k / side, k % side);
            let near = [(i > 0).then(|| k - side), (i + 1 < side).then(|| k + side), (j > 0).then(|| k - 1), (j + 1 < side).then(|| k + 1)];
            for m in near.into_iter().flatten() {
                if let (Some(c), None) = (grid[m], read[m]) {
                    read[m] = Some(at + wrap(c.x - at));
                    todo.push(m);
                }
            }
        }
    }
    let mut e = Extent::EMPTY;
    for (k, c) in grid.iter().enumerate() {
        let Some(c) = c else { continue };
        let x = if globe { read[k].unwrap_or_else(|| first + wrap(c.x - first)) } else { c.x };
        e.expand(Coord::new(x, c.y));
    }
    if let Some(lat) = pole {
        e.expand(Coord::new(first, lat));
        e.min_x = first - 180.0;
        e.max_x = first + 180.0;
    }
    let (west, east) = if e.width() >= 360.0 { (-180.0, 180.0) } else { (wrap(e.min_x), wrap(e.max_x)) };
    Some([west, e.min_y.clamp(-90.0, 90.0), east, e.max_y.clamp(-90.0, 90.0)])
}

/// The places the open panel's picks hold, on the map: a point's marker, a box's outline.
pub(crate) fn paint(app: &RustGisApp, p: &egui::Painter, t: &crate::theme::Tokens, to_screen: impl Fn(Coord) -> Option<egui::Pos2>) {
    if app.ui.right != Some(RightPane::Plugin) {
        return;
    }
    let Some(pid) = app.ui.plugin_panel.plugin.as_deref() else { return };
    let (Some(plugin), Some(state)) = (rustgis_engine::plugins::get(pid).filter(|p| p.enabled), app.session.plugin_panels.get(pid)) else {
        return;
    };
    let Some(decl) = plugin.manifest.panel.as_ref() else { return };
    let n = |v: Option<&Value>| v.and_then(Value::as_f64);
    let ring = Stroke::new(1.5, egui::Color32::WHITE);
    for d in decl.items.iter().filter(|d| d.kind == "pick" && state.items.get(d.key()).is_none_or(|s| s.visible)) {
        let Some(v) = state.picked(d.key()) else { continue };
        if let Some(b) = v.get("bbox").and_then(Value::as_array) {
            let (Some(w), Some(s), Some(e), Some(north)) = (n(b.first()), n(b.get(1)), n(b.get(2)), n(b.get(3))) else { continue };
            let e = if e < w { e + 360.0 } else { e };
            // Its edges followed, so a curved projection draws them curved.
            let steps = 24;
            let mut edge = Vec::new();
            for (a, z) in [((w, s), (e, s)), ((e, s), (e, north)), ((e, north), (w, north)), ((w, north), (w, s))] {
                for i in 0..steps {
                    let f = i as f64 / steps as f64;
                    edge.push(Coord::new(a.0 + (z.0 - a.0) * f, a.1 + (z.1 - a.1) * f));
                }
            }
            // The parts on show: a box partly on the globe's far side keeps the rest.
            let pts: Vec<Option<egui::Pos2>> = edge.into_iter().map(&to_screen).collect();
            if pts.iter().all(Option::is_some) {
                p.add(egui::Shape::closed_line(pts.into_iter().flatten().collect(), Stroke::new(2.0, t.accent)));
            } else {
                let closed = pts.iter().chain(pts.first());
                for run in closed.copied().collect::<Vec<_>>().split(Option::is_none) {
                    if run.len() > 1 {
                        p.add(egui::Shape::line(run.iter().flatten().copied().collect(), Stroke::new(2.0, t.accent)));
                    }
                }
            }
        } else if let (Some(lon), Some(lat)) = (n(v.get("lon")), n(v.get("lat")))
            && let Some(sp) = to_screen(Coord::new(lon, lat))
        {
            p.circle_filled(sp, 6.0, t.accent);
            p.circle_stroke(sp, 6.0, ring);
        }
    }
}

/// A 26 px button with a chevron pointing left or right, found by `tip`.
fn step_button(ui: &mut Ui, on: bool, left: bool, tip: &str, t: &Tokens) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(26.0, 26.0), if on { Sense::click() } else { Sense::hover() });
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, on, tip));
    if on && resp.hovered() {
        ui.painter().rect_filled(r, CornerRadius::same(5), t.hover);
    }
    ui.painter().rect_stroke(r, CornerRadius::same(5), Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
    let (c, d) = (r.center(), if left { 2.5 } else { -2.5 });
    let ink = if on { t.text } else { t.text_disabled };
    let stroke = Stroke::new(1.6, ink);
    ui.painter().line_segment([pos2(c.x + d, c.y - 5.0), pos2(c.x - d, c.y)], stroke);
    ui.painter().line_segment([pos2(c.x - d, c.y), pos2(c.x + d, c.y + 5.0)], stroke);
    resp.on_hover_text(tip)
}

fn slider(app: &mut RustGisApp, ui: &mut Ui, pid: &str, item: &PanelItemDecl, state: &PanelState, tr: &dyn Fn(&str) -> String, t: &Tokens) {
    let id = item.key();
    let Some(st) = state.items.get(id) else { return };
    let (min, max) = (st.min, st.max.max(st.min));
    let stored = state.slider(id);
    let dragging = app.ui.plugin_panel.dragging.as_ref().filter(|(p, i, _)| p == pid && i == id).map(|d| d.2);
    let mut v = dragging.unwrap_or(stored).clamp(min, max);
    let on = enabled(item, state) && max > min;
    // The caption, and the value it would take (shown as it is dragged).
    ui.horizontal(|ui| {
        let label = label_of(item, state, tr);
        ui.label(RichText::new(label).size(11.0).color(t.text_dim));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let color = if dragging.is_some() { t.accent_text } else { t.text };
            ui.label(RichText::new(state.slider_label(id, v)).font(semibold(12.5)).color(color));
        });
    });
    let mut stepped = None;
    ui.horizontal(|ui| {
        let gap = ui.spacing().item_spacing.x;
        let buttons = if item.steps { 2.0 * (26.0 + gap) } else { 0.0 };
        let room = ui.available_width();
        if item.steps && step_button(ui, on && v > min, true, tl!("Step back"), t).clicked() {
            stepped = Some(v - 1);
        }
        ui.spacing_mut().slider_width = (room - buttons - 4.0).max(60.0);
        let resp = ui.add_enabled(on, egui::Slider::new(&mut v, min..=max).show_value(false));
        let resp = if item.help.is_empty() { resp } else { resp.on_hover_text(tr(&item.help)) };
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Slider, on, label_of(item, state, tr)));
        if item.steps && step_button(ui, on && v < max, false, tl!("Step forward"), t).clicked() {
            stepped = Some(v + 1);
        }
        if resp.dragged() {
            app.ui.plugin_panel.dragging = Some((pid.to_string(), id.to_string(), v));
        } else if dragging.is_some() || v != stored {
            // Let go, clicked or moved with the keys: now it runs. A step button clicked on the
            // same frame wins (its value was taken from the slider's before this frame's move).
            // A range the plugin changed during a drag clamps the value each frame, and on release.
            app.ui.plugin_panel.dragging = None;
            if v != stored {
                stepped.get_or_insert(v);
            }
        }
    });
    // Its ends, and where it is among them.
    ui.horizontal(|ui| {
        let small = |s: String| RichText::new(s).size(10.5).color(t.text_dim);
        ui.label(small(state.slider_label(id, min)));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(small(state.slider_label(id, max)));
            let of = crate::i18n::fmt("{n} of {count}", &[("n", &(v - min + 1).to_string()), ("count", &(max - min + 1).to_string())]);
            let r = ui.available_rect_before_wrap();
            ui.painter().text(pos2(r.center().x, r.center().y), Align2::CENTER_CENTER, of, regular(10.5), t.text_dim);
        });
    });
    if let Some(n) = stepped.map(|n| n.clamp(min, max)).filter(|n| *n != stored) {
        send(app, pid, item, json!(n));
    }
}

/// A tree's row height, and how far each level is indented.
const TREE_ROW: f32 = 22.0;
const TREE_INDENT: f32 = 14.0;

/// A row of a tree as shown: a node, how deep it is, whether its children show below it, and
/// whether they must (a branch above what a search found).
struct TreeRow<'a> {
    node: &'a TreeNode,
    depth: usize,
    open: bool,
    held: bool,
}

/// Lower case without accents, so that "arbol" finds "Árboles".
/// Letters are decomposed and their marks dropped, whether the text came with them composed or not;
/// lower case first, since some letters lower-case into a letter and a mark (`İ`).
fn fold(s: &str) -> String {
    use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};
    s.to_lowercase().nfd().filter(|c| !is_combining_mark(*c)).collect()
}

/// The ids of the branches above node `id` in `nodes`, the top one first; false when it isn't
/// among them.
fn ancestors<'a>(nodes: &'a [TreeNode], id: &str, out: &mut Vec<&'a str>) -> bool {
    for n in nodes {
        if n.id == id {
            return true;
        }
        out.push(&n.id);
        if ancestors(&n.children, id, out) {
            return true;
        }
        out.pop();
    }
    false
}

/// Whether `label` holds every word of a search (folded as [`fold`] folds them).
fn holds_words(label: &str, words: &[String]) -> bool {
    let label = fold(label);
    words.iter().all(|w| label.contains(w.as_str()))
}

/// The rows of `nodes`: with no `words`, each node and, when open, its children. Searching, the
/// nodes whose label holds every word, and every branch above one held open, a branch found too;
/// a branch found with nothing found below it shows as it is, opening as ever.
fn tree_rows<'a>(nodes: &'a [TreeNode], depth: usize, words: &[String], open: &HashSet<String>, out: &mut Vec<TreeRow<'a>>) {
    for n in nodes {
        let at = out.len();
        out.push(TreeRow { node: n, depth, open: false, held: false });
        if !words.is_empty() {
            // What's found below it first.
            tree_rows(&n.children, depth + 1, words, open, out);
            if out.len() > at + 1 {
                if let Some(r) = out.get_mut(at) {
                    (r.open, r.held) = (true, true);
                }
                continue;
            }
            if !holds_words(&n.label, words) {
                out.pop();
                continue;
            }
        }
        if open.contains(&n.id) && !n.children.is_empty() {
            if let Some(r) = out.get_mut(at) {
                r.open = true;
            }
            tree_rows(&n.children, depth + 1, &[], open, out);
        }
    }
}

/// A tree: a row per node, a branch's children indented under it while it is open. A click
/// chooses a node (running the widget's command); the triangle, or a double click, opens or
/// closes a branch (a click on the triangle never chooses it; the first click of a double click
/// does, as a file browser's). With `search`, a box above it keeps the nodes whose label holds
/// the words typed, with the branches above them held open ([`tree_rows`]).
fn tree(app: &mut RustGisApp, ui: &mut Ui, pid: &str, item: &PanelItemDecl, state: &PanelState, tr: &dyn Fn(&str) -> String, t: &Tokens) {
    let id = item.key();
    let Some(st) = state.items.get(id) else { return };
    let label = label_of(item, state, tr);
    if !label.is_empty() {
        ui.label(RichText::new(label).size(11.0).color(t.text_dim));
    }
    let on = enabled(item, state);
    if item.search {
        // As the Layers pane's filter box: the shared search field, over a separator.
        let query = app.ui.plugin_panel.search.entry(id.to_string()).or_default();
        ui.add_enabled_ui(on, |ui| crate::widgets::search_field(ui, query, tl!("Search")));
        ui.separator();
    }
    // (Only with its box: a plugin updated without one leaves no search behind that can't be cleared.)
    let mut words: Vec<String> = match app.ui.plugin_panel.search.get(id) {
        Some(q) if item.search => fold(q).split_whitespace().map(String::from).collect(),
        _ => Vec::new(),
    };
    words.sort_unstable();
    words.dedup();
    let chosen = state.chosen(id);
    let mut at: Vec<String> = Vec::new();
    if let Some(c) = chosen {
        let mut above = Vec::new();
        if ancestors(&st.nodes, c, &mut above) {
            at = above.into_iter().map(String::from).chain([c.to_string()]).collect();
        }
    }
    if app.ui.plugin_panel.revealed.get(id).is_none_or(|was| *was != at) {
        if let Some((_, above)) = at.split_last() {
            app.ui.plugin_panel.open.entry(id.to_string()).or_default().extend(above.iter().cloned());
        }
        app.ui.plugin_panel.revealed.insert(id.to_string(), at);
    }
    let none = HashSet::new();
    let open = app.ui.plugin_panel.open.get(id).unwrap_or(&none);
    let mut rows = Vec::new();
    tree_rows(&st.nodes, 0, &words, open, &mut rows);
    if rows.is_empty() {
        let empty = if words.is_empty() { "—" } else { tl!("Nothing matches.") };
        ui.label(RichText::new(empty).size(12.5).color(t.text_dim));
        return;
    }
    let (mut toggle, mut pick) = (None, None);
    egui::ScrollArea::vertical().id_salt(("plugin-tree", pid, id)).max_height(320.0).auto_shrink([false, true]).show_rows(
        ui,
        TREE_ROW,
        rows.len(),
        |ui, range| {
            for r in rows.iter().skip(range.start).take(range.len()) {
                let (rect, resp) =
                    ui.allocate_exact_size(vec2(ui.available_width(), TREE_ROW), if on { Sense::click() } else { Sense::hover() });
                let here = chosen == Some(r.node.id.as_str());
                if here {
                    ui.painter().rect_filled(rect, CornerRadius::same(4), t.checked);
                } else if on && resp.hovered() {
                    ui.painter().rect_filled(rect, CornerRadius::same(4), t.hover);
                }
                let ink = if on { t.text } else { t.text_disabled };
                let x = rect.left() + 4.0 + r.depth as f32 * TREE_INDENT;
                let branch = !r.node.children.is_empty();
                let arrow = egui::Rect::from_min_size(pos2(x, rect.top()), vec2(TREE_INDENT, TREE_ROW));
                if branch {
                    let c = arrow.center();
                    let pts = if r.open {
                        vec![pos2(c.x - 4.0, c.y - 2.0), pos2(c.x + 4.0, c.y - 2.0), pos2(c.x, c.y + 3.0)]
                    } else {
                        vec![pos2(c.x - 2.0, c.y - 4.0), pos2(c.x + 3.0, c.y), pos2(c.x - 2.0, c.y + 4.0)]
                    };
                    ui.painter().add(egui::Shape::convex_polygon(pts, t.text_dim, Stroke::NONE));
                }
                // The count at the right, the label in what's left of the row: on a narrow pane,
                // deep down, the label keeps its room and the count goes.
                let mut right = rect.right() - 6.0;
                if let Some(n) = r.node.count {
                    let g = ui.painter().layout_no_wrap(format!("({n})"), regular(12.0), t.text_dim);
                    if right - g.size().x - 8.0 >= arrow.right() + 2.0 + 40.0 {
                        right -= g.size().x;
                        ui.painter().galley(pos2(right, rect.center().y - g.size().y / 2.0), g, t.text_dim);
                        right -= 8.0;
                    }
                }
                let text = egui::Rect::from_min_max(pos2(arrow.right() + 2.0, rect.top()), pos2(right, rect.bottom()));
                let font = if branch { semibold(12.5) } else { regular(12.5) };
                ui.painter().with_clip_rect(text).text(pos2(text.left(), rect.center().y), Align2::LEFT_CENTER, &r.node.label, font, ink);
                let resp = resp.on_hover_text(r.node.label.as_str());
                let spoken = match r.node.count {
                    Some(n) => format!("{} ({n})", r.node.label),
                    None => r.node.label.clone(),
                };
                resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::TreeItem, on, here, &spoken));
                // A branch held open over what a search found doesn't close.
                let on_arrow = resp.interact_pointer_pos().is_some_and(|p| arrow.contains(p));
                // On the triangle each click opens or closes, once for a double click too.
                let opens = if on_arrow { resp.clicked() && !resp.double_clicked() } else { resp.double_clicked() };
                if branch && !r.held && opens {
                    toggle = Some(r.node.id.clone());
                } else if resp.clicked() && !here && !(branch && on_arrow) {
                    pick = Some(r.node.id.clone());
                }
            }
        },
    );
    if let Some(n) = toggle {
        let open = app.ui.plugin_panel.open.entry(id.to_string()).or_default();
        if !open.remove(&n) {
            open.insert(n);
        }
    }
    if let Some(n) = pick {
        send(app, pid, item, json!(n));
    }
}

/// A dropdown: the chosen option's label, and the options in a list below it. Choosing one runs
/// the widget's command.
fn choice(app: &mut RustGisApp, ui: &mut Ui, pid: &str, item: &PanelItemDecl, state: &PanelState, tr: &dyn Fn(&str) -> String, t: &Tokens) {
    let id = item.key();
    let Some(st) = state.items.get(id) else { return };
    let label = label_of(item, state, tr);
    if !label.is_empty() {
        ui.label(RichText::new(&label).size(11.0).color(t.text_dim));
    }
    let chosen = state.chosen(id);
    // An option declared in plugin.json shows its label in the interface language; one the plugin
    // sent is worded by the plugin already.
    let declared: HashMap<&str, &str> =
        item.options.iter().filter_map(|o| Some((o.get("id")?.as_str()?, o.get("label")?.as_str()?))).collect();
    let named = |n: &TreeNode| if declared.get(n.id.as_str()) == Some(&n.label.as_str()) { tr(&n.label) } else { n.label.clone() };
    let shown = chosen.and_then(|c| st.nodes.iter().find(|n| n.id == c)).map_or_else(|| tl!("Choose…").to_string(), named);
    let on = enabled(item, state) && !st.nodes.is_empty();
    let mut pick = None;
    let width = ui.available_width();
    ui.add_enabled_ui(on, |ui| {
        let resp =
            egui::ComboBox::from_id_salt(("plugin-choice", pid, id)).selected_text(&shown).width(width).height(320.0).show_ui(ui, |ui| {
                for n in st.nodes.iter() {
                    if ui.selectable_label(chosen == Some(n.id.as_str()), named(n)).clicked() && chosen != Some(n.id.as_str()) {
                        pick = Some(n.id.clone());
                    }
                }
            });
        let resp = resp.response.on_hover_text(if item.help.is_empty() { shown.clone() } else { tr(&item.help) });
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, on, &label));
    });
    if let Some(n) = pick {
        send(app, pid, item, json!(n));
    }
}

/// The weekday of a date, 0 for Monday.
fn weekday(y: i32, m: u32, d: u32) -> u32 {
    // Sakamoto's method, which counts from Sunday.
    const T: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y = if m < 3 { y - 1 } else { y };
    let sunday = (y + y / 4 - y / 100 + y / 400 + T[(m - 1) as usize] + d as i32).rem_euclid(7) as u32;
    (sunday + 6) % 7
}

/// The month before (`by` -1) or after (`by` 1) year `y`, month `m`, kept within years 1 to 9999.
fn month_step(y: i32, m: u32, by: i32) -> (i32, u32) {
    let k = (y * 12 + m as i32 - 1 + by).clamp(12, 9999 * 12 + 11);
    (k / 12, (k % 12) as u32 + 1)
}

/// A date: a box for `YYYY-MM-DD` and a calendar button. Picking a day, or confirming a date typed
/// (Enter, or leaving the box), runs the widget's command; a date that isn't one is put back.
fn date(app: &mut RustGisApp, ui: &mut Ui, pid: &str, item: &PanelItemDecl, state: &PanelState, tr: &dyn Fn(&str) -> String, t: &Tokens) {
    use rustgis_engine::plugins::panel::{date as parse, days_in_month};
    let id = item.key();
    let label = label_of(item, state, tr);
    if !label.is_empty() {
        ui.label(RichText::new(&label).size(11.0).color(t.text_dim));
    }
    let on = enabled(item, state);
    let stored = state.chosen(id).unwrap_or_default().to_string();
    let mut picked: Option<Value> = None;
    ui.horizontal(|ui| {
        let editing = app.ui.plugin_panel.date_text.contains_key(id);
        let mut text = app.ui.plugin_panel.date_text.get(id).cloned().unwrap_or_else(|| stored.clone());
        let edit = egui::TextEdit::singleline(&mut text).hint_text("YYYY-MM-DD").desired_width(110.0).char_limit(10);
        let resp = ui.add_enabled(on, edit);
        let resp = if item.help.is_empty() { resp } else { resp.on_hover_text(tr(&item.help)) };
        if resp.changed() || (editing && resp.has_focus()) {
            app.ui.plugin_panel.date_text.insert(id.to_string(), text.clone());
        }
        if resp.lost_focus() {
            app.ui.plugin_panel.date_text.remove(id);
            let typed = text.trim();
            if typed != stored {
                if typed.is_empty() {
                    picked = Some(Value::Null);
                } else if parse(typed).is_some() {
                    picked = Some(json!(typed));
                } else {
                    app.session.notify(crate::i18n::fmt("'{date}' isn't a date: write it as YYYY-MM-DD", &[("date", typed)]), true);
                }
            }
        }
        // The calendar opens on the month of the date (today's when there is none); while it is
        // open, it keeps the month it was stepped to.
        let btn = calendar_button(ui, on, t);
        let opening = parse(&stored).map_or_else(today, |(y, m, _)| (y, m));
        let (y, m) = if btn.clicked() { opening } else { app.ui.plugin_panel.calendar.get(id).copied().unwrap_or(opening) };
        let mut shown = (y, m);
        // A date the plugin disables closes its calendar, so no day can be picked in it.
        let popup = egui::Popup::from_toggle_button_response(&btn);
        let popup = if on { popup } else { popup.open_memory(egui::SetOpenCommand::Bool(false)) };
        let open = popup.close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
            ui.horizontal(|ui| {
                if step_button(ui, true, true, tl!("Previous month"), t).clicked() {
                    shown = month_step(y, m, -1);
                }
                ui.label(RichText::new(format!("{y:04}-{m:02}")).font(semibold(13.0)));
                if step_button(ui, true, false, tl!("Next month"), t).clicked() {
                    shown = month_step(y, m, 1);
                }
            });
            let picked_day = parse(&stored).filter(|(sy, sm, _)| (*sy, *sm) == (y, m)).map(|d| d.2);
            egui::Grid::new(("plugin-calendar", pid, id)).spacing(vec2(2.0, 2.0)).show(ui, |ui| {
                for w in [tl!("Mo"), tl!("Tu"), tl!("We"), tl!("Th"), tl!("Fr"), tl!("Sa"), tl!("Su")] {
                    ui.label(RichText::new(w).size(10.5).color(t.text_dim));
                }
                ui.end_row();
                let first = weekday(y, m, 1);
                for _ in 0..first {
                    ui.label("");
                }
                let days = days_in_month(y, m).unwrap_or(30);
                for d in 1..=days {
                    let resp = ui.add(egui::Button::selectable(picked_day == Some(d), format!("{d}")).min_size(vec2(26.0, 22.0)));
                    if resp.clicked() {
                        picked = Some(json!(format!("{y:04}-{m:02}-{d:02}")));
                        ui.close();
                    }
                    if (first + d).is_multiple_of(7) {
                        ui.end_row();
                    }
                }
            });
            if !stored.is_empty() && ui.button(tl!("Clear")).clicked() {
                picked = Some(Value::Null);
                ui.close();
            }
        });
        if open.is_some() {
            app.ui.plugin_panel.calendar.insert(id.to_string(), shown);
        } else {
            app.ui.plugin_panel.calendar.remove(id);
        }
    });
    if let Some(v) = picked.filter(|v| v.as_str().unwrap_or_default() != stored) {
        app.ui.plugin_panel.date_text.remove(id);
        send(app, pid, item, v);
    }
}

/// A 28 × 24 px button with a calendar drawn on it, opening a date's calendar.
fn calendar_button(ui: &mut Ui, on: bool, t: &Tokens) -> egui::Response {
    let tip = tl!("Pick a date");
    let (r, resp) = ui.allocate_exact_size(vec2(28.0, 24.0), if on { Sense::click() } else { Sense::hover() });
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, on, tip));
    if on && resp.hovered() {
        ui.painter().rect_filled(r, CornerRadius::same(5), t.hover);
    }
    ui.painter().rect_stroke(r, CornerRadius::same(5), Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
    let ink = Stroke::new(1.4, if on { t.text } else { t.text_disabled });
    // A page with a binding across its top, two rings and a few days.
    let page = egui::Rect::from_center_size(r.center() + vec2(0.0, 1.0), vec2(14.0, 12.0));
    ui.painter().rect_stroke(page, CornerRadius::same(2), ink, egui::StrokeKind::Middle);
    ui.painter().line_segment([pos2(page.left(), page.top() + 3.5), pos2(page.right(), page.top() + 3.5)], ink);
    for x in [page.left() + 4.0, page.right() - 4.0] {
        ui.painter().line_segment([pos2(x, page.top() - 2.0), pos2(x, page.top() + 1.5)], ink);
    }
    for (dx, dy) in [(3.5, 6.5), (7.0, 6.5), (10.5, 6.5), (3.5, 9.5), (7.0, 9.5)] {
        ui.painter().circle_filled(pos2(page.left() + dx, page.top() + dy), 0.9, ink.color);
    }
    resp.on_hover_text(tip)
}

/// Today's year and month, for a calendar with no date to open on (UTC is near enough).
fn today() -> (i32, u32) {
    let days = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map_or(0, |d| d.as_secs() / 86_400) as i64;
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = (yoe + era * 400 + i64::from(m <= 2)) as i32;
    (y, m)
}

/// A range: a slider with two handles, low and high, on one track. It runs its command when a
/// handle is let go or moved with the keys (each handle takes the focus), or both are stepped by
/// ‹ and ›.
fn range(app: &mut RustGisApp, ui: &mut Ui, pid: &str, item: &PanelItemDecl, state: &PanelState, tr: &dyn Fn(&str) -> String, t: &Tokens) {
    let id = item.key();
    let Some(st) = state.items.get(id) else { return };
    let (min, max) = (st.min, st.max.max(st.min));
    let stored = state.range(id);
    let on = enabled(item, state) && max > min;
    let mine = |d: &(String, String, i64, i64)| d.0 == pid && d.1 == id;
    // A range the plugin disables while it is dragged lets the drag go, running nothing.
    if !on && app.ui.plugin_panel.range_drag.as_ref().is_some_and(mine) {
        app.ui.plugin_panel.range_drag = None;
    }
    let dragging = app.ui.plugin_panel.range_drag.as_ref().filter(|d| mine(d)).map(|d| (d.2, d.3));
    let (mut lo, mut hi) = dragging.unwrap_or(stored);
    (lo, hi) = (lo.clamp(min, max), hi.clamp(min, max).max(lo.clamp(min, max)));
    let label = label_of(item, state, tr);
    ui.horizontal(|ui| {
        ui.label(RichText::new(&label).size(11.0).color(t.text_dim));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let color = if dragging.is_some() { t.accent_text } else { t.text };
            let text = format!("{} – {}", state.slider_label(id, lo), state.slider_label(id, hi));
            ui.label(RichText::new(text).font(semibold(12.5)).color(color));
        });
    });
    let mut done = None;
    ui.horizontal(|ui| {
        let gap = ui.spacing().item_spacing.x;
        let buttons = if item.steps { 2.0 * (26.0 + gap) } else { 0.0 };
        let room = ui.available_width();
        if item.steps && step_button(ui, on && lo > min, true, tl!("Step back"), t).clicked() {
            done = Some((lo - 1, hi - 1));
        }
        ui.spacing_mut().slider_width = (room - buttons - 4.0).max(60.0);
        let resp = ui.add_enabled(on, egui::RangeSlider::new(&mut lo, &mut hi, min..=max).show_value(false));
        let resp = if item.help.is_empty() { resp } else { resp.on_hover_text(tr(&item.help)) };
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Slider, on, &label));
        if item.steps && step_button(ui, on && hi < max, false, tl!("Step forward"), t).clicked() {
            done = Some((lo + 1, hi + 1));
        }
        if resp.dragged() || resp.is_pointer_button_down_on() {
            app.ui.plugin_panel.range_drag = Some((pid.to_string(), id.to_string(), lo, hi));
        } else if on && (dragging.is_some() || (lo, hi) != stored) {
            // Let go, clicked or moved with the keys: now it runs (a step button clicked on the
            // same frame wins).
            app.ui.plugin_panel.range_drag = None;
            done.get_or_insert((lo, hi));
        }
        // A step taken while a handle is held (a screen reader's) ends the drag with it.
        if done.is_some() {
            app.ui.plugin_panel.range_drag = None;
        }
    });
    ui.horizontal(|ui| {
        let small = |s: String| RichText::new(s).size(10.5).color(t.text_dim);
        ui.label(small(state.slider_label(id, min)));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(small(state.slider_label(id, max)));
        });
    });
    if let Some((a, b)) = done {
        let (a, b) = (a.clamp(min, max), b.clamp(min, max));
        if (a.min(b), a.max(b)) != stored {
            send(app, pid, item, json!([a.min(b), a.max(b)]));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A box dragged across the antimeridian is the narrow box there, on the globe (longitudes
    /// wrapped) and on a flat map (past 180); a wide one stays wide, on a flat map and on the
    /// globe (a band under the pole over more than 180° of longitude), and one around a pole is
    /// the world's width.
    #[test]
    fn a_box_across_the_antimeridian_is_the_narrow_one() {
        const SIDE: usize = 13;
        // The places on a grid over the box, `at(u, v)` for u, v from 0 to 1, column by column.
        let grid = |at: &dyn Fn(f64, f64) -> Option<Coord>| -> Vec<Option<Coord>> {
            let t = |k: usize| k as f64 / (SIDE - 1) as f64;
            (0..SIDE).flat_map(|i| (0..SIDE).map(move |j| (t(i), t(j)))).map(|(u, v)| at(u, v)).collect()
        };
        let c = |x: f64, y: f64| Some(Coord::new(x, y));
        let globe = grid(&|u, v| c(wrap(179.0 + 2.0 * u), -1.0 + 2.0 * v));
        assert_eq!(box_of(&globe, SIDE, true, None), Some([179.0, -1.0, -179.0, 1.0]));
        let flat = grid(&|u, v| c(179.0 + 2.0 * u, -1.0 + 2.0 * v));
        assert_eq!(box_of(&flat, SIDE, false, None), Some([179.0, -1.0, -179.0, 1.0]));
        let wide = grid(&|u, v| c(-170.0 + 340.0 * u, 5.0 * v));
        assert_eq!(box_of(&wide, SIDE, false, None), Some([-170.0, 0.0, 170.0, 5.0]));
        assert_eq!(box_of(&wide, SIDE, true, None), Some([-170.0, 0.0, 170.0, 5.0]), "wide on the globe");
        // An orthographic globe centred on 30°N 0° (unit radius, y up), and a band across it
        // just under the north pole: its longitudes run from about -111 to 111 by way of 0.
        let (lat0, r) = (30f64.to_radians(), |x: f64, y: f64| (x * x + y * y).sqrt());
        let ortho = |x: f64, y: f64| {
            let rho = r(x, y);
            let s = rho.asin();
            let lat = (s.cos() * lat0.sin() + y * s.sin() * lat0.cos() / rho).asin();
            let lon = (x * s.sin()).atan2(rho * s.cos() * lat0.cos() - y * s.sin() * lat0.sin());
            (rho <= 1.0).then(|| Coord::new(wrap(lon.to_degrees()), lat.to_degrees()))
        };
        let band = grid(&|u, v| ortho(-1.0 + 2.0 * u, 0.6 + 0.2 * v));
        let b = box_of(&band, SIDE, true, None).unwrap_or_default();
        assert!(b[0] < -100.0 && b[2] > 100.0, "the band's own side, not the far one: {b:?}");
        // Off the globe on its left: the rest still reads.
        let part = grid(&|u, v| (u > 0.4).then(|| Coord::new(wrap(170.0 + 20.0 * u), v)));
        assert_eq!(box_of(&part, SIDE, true, None).map(|b| b[2]), Some(-170.0));
        let pole = grid(&|u, v| c(wrap(360.0 * u), 80.0 + 5.0 * v));
        assert_eq!(box_of(&pole, SIDE, true, Some(90.0)), Some([-180.0, 80.0, 180.0, 90.0]), "around a pole");
        let world = grid(&|u, _| c(-200.0 + 400.0 * u, 0.0));
        assert_eq!(box_of(&world, SIDE, false, None), Some([-180.0, 0.0, 180.0, 0.0]), "wider than the world");
        assert_eq!(box_of(&grid(&|_, _| None), SIDE, true, None), None, "nothing on the globe");
        assert_eq!(box_of(&globe, SIDE - 1, true, None), None, "a grid of another size");
    }

    fn node(id: &str, label: &str, children: Vec<TreeNode>) -> TreeNode {
        TreeNode { id: id.into(), label: label.into(), count: None, children }
    }

    /// The rows shown for `query` with `open` branches: each id indented by its depth, `/` when
    /// its children show, `*` when a search holds it open.
    fn shown(nodes: &[TreeNode], query: &str, open: &[&str]) -> Vec<String> {
        let words: Vec<String> = fold(query).split_whitespace().map(String::from).collect();
        let open: HashSet<String> = open.iter().map(|s| s.to_string()).collect();
        let mut rows = Vec::new();
        tree_rows(nodes, 0, &words, &open, &mut rows);
        rows.iter()
            .map(|r| format!("{}{}{}{}", "  ".repeat(r.depth), r.node.id, if r.open { "/" } else { "" }, if r.held { "*" } else { "" }))
            .collect()
    }

    #[test]
    fn calendar_arithmetic() {
        // 2023-04-01 was a Saturday, 2024-02-29 a Thursday, 2000-01-01 a Saturday.
        assert_eq!(weekday(2023, 4, 1), 5);
        assert_eq!(weekday(2024, 2, 29), 3);
        assert_eq!(weekday(2000, 1, 1), 5);
        assert_eq!(month_step(2023, 1, -1), (2022, 12));
        assert_eq!(month_step(2023, 12, 1), (2024, 1));
        assert_eq!(month_step(9999, 12, 1), (9999, 12), "kept within years 1 to 9999");
        assert_eq!(month_step(1, 1, -1), (1, 1));
        let (y, m) = today();
        assert!(y >= 2024 && (1..=12).contains(&m));
    }

    /// A tree shows its open branches' children; a search keeps what matches every word (case and
    /// accents aside) with the branches above it held open, and a branch found opens as ever.
    #[test]
    fn tree_rows_follow_open_branches_and_searches() {
        let tree = vec![
            node(
                "env",
                "Ambiente",
                vec![node("trees", "Arborización", vec![node("601", "Árboles urbanos", vec![])]), node("water", "Agua", vec![])],
            ),
            node("cul", "Cultura", vec![node("c1", "Teatro", vec![])]),
            node("arts", "Artes", vec![node("live", "Cultura viva", vec![]), node("film", "Cine", vec![])]),
        ];
        assert_eq!(shown(&tree, "", &[]), ["env", "cul", "arts"]);
        assert_eq!(shown(&tree, "", &["env"]), ["env/", "  trees", "  water", "cul", "arts"]);
        assert_eq!(shown(&tree, "", &["trees"]), ["env", "cul", "arts"], "an open branch under a closed one stays hidden");
        assert_eq!(shown(&tree, "arbol", &[]), ["env/*", "  trees/*", "    601"]);
        assert_eq!(shown(&tree, "ÁRBOLES urb", &[]), ["env/*", "  trees/*", "    601"], "every word, case and accents aside");
        assert_eq!(shown(&tree, "cultura", &[]), ["cul", "arts/*", "  live"], "a branch found shows as it is");
        assert_eq!(shown(&tree, "cultura", &["cul"]), ["cul/", "  c1", "arts/*", "  live"], "and opens as ever");
        // A branch found with something found below it is held open on that.
        let nested = vec![node("cul", "Cultura", vec![node("live", "Cultura viva", vec![]), node("c1", "Teatro", vec![])])];
        assert_eq!(shown(&nested, "cultura", &[]), ["cul/*", "  live"]);
        assert!(shown(&tree, "zzz", &["env"]).is_empty());
        assert_eq!(fold("Ñandú Çà Åland İzmir"), "nandu ca aland izmir");
        // A node chosen elsewhere opens the branches above it.
        let mut above = Vec::new();
        assert!(ancestors(&tree, "601", &mut above));
        assert_eq!(above, ["env", "trees"]);
        assert_eq!(shown(&tree, "", &above), ["env/", "  trees/", "    601", "  water", "cul", "arts"]);
        assert!(!ancestors(&tree, "nope", &mut Vec::new()));
        // Decomposed accents (a letter, then its mark) fold as composed ones do.
        let decomposed = vec![node("601", "A\u{301}rboles urbanos", vec![])];
        assert_eq!(shown(&decomposed, "arbol", &[]), ["601"]);
        assert_eq!(shown(&decomposed, "árbol arbol", &[]), ["601"], "a word given twice");
    }

    /// A plugin's command adding data by address doesn't wait for it: the app downloads it off
    /// the frame, as the browser does, and the layer lands as the plugin asked for it (#1144).
    #[test]
    fn a_plugin_adds_data_by_address_off_the_frame() {
        let _turn = crate::fetching::turn();
        use rustgis_engine::plugins;
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/places.csv", listener.local_addr().unwrap());
        // The server answers once told to: the command has returned before the data can land.
        let (go, wait) = std::sync::mpsc::channel::<()>();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { continue };
                let _ = wait.recv();
                let _ = s.read(&mut [0u8; 4096]);
                let body = "name,east,north\nA,1,2\nB,3,4\n";
                let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            }
        });
        let pkg = plugins::Package::from_zip(&plugins::testing::package("ui-url", &["edit", "network"])).unwrap();
        plugins::load(pkg, "test").unwrap();
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1400.0, 860.0)).with_step_dt(0.05).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            RustGisApp::with_sample(),
        );
        let app = h.state_mut();
        let params = json!({"url": url, "name": "Places", "x": "east", "y": "north"});
        let r = app.run("plugin:ui-url/f", json!({"op": "layer.addUrl", "params": params})).unwrap();
        assert_eq!(r["pending"], true);
        assert!(app.session.map().layer_by_name("Places").is_none(), "nothing waited");
        go.send(()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while h.state().session.map().layer_by_name("Places").is_none() {
            assert!(std::time::Instant::now() < deadline, "timed out waiting");
            h.step();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let places = h.state().session.map().layer_by_name("Places").unwrap();
        let e = places.dataset.extent();
        assert_eq!([e.min_x, e.min_y, e.max_x, e.max_y], [1.0, 2.0, 3.0, 4.0], "its points read from the columns the plugin named");
        plugins::unload("ui-url");
    }

    /// The real deferred request and probe loop refuse a withdrawn plugin before a fallback send.
    #[test]
    fn permission_policy_plugin_deferred_probe_refuses_before_fallback() {
        let _turn = crate::fetching::turn();
        use rustgis_engine::{plugins, render::rustgis_tiles::cache};
        use std::sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        };
        let sent = Arc::new(AtomicUsize::new(0));
        let count = sent.clone();
        let held = Arc::new(Mutex::new(Vec::new()));
        let callbacks = held.clone();
        cache::set_fetcher(move |_, done| {
            count.fetch_add(1, Ordering::SeqCst);
            callbacks.lock().unwrap().push(done);
        });
        let mut app = RustGisApp::with_sample();
        let load = || {
            plugins::load(plugins::Package::from_zip(&plugins::testing::package("ui-policy-probe", &["edit", "network"])).unwrap(), "test")
                .unwrap()
        };
        let p = json!({"url":"http://127.0.0.1:9/policy-held.fgb","name":"Held source"});
        load();
        app.run("plugin:ui-policy-probe/f", json!({"op":"layer.addUrl","params":p})).unwrap();
        plugins::unload("ui-policy-probe");
        assert!(app.session.take_url_requests().is_empty());
        assert_eq!(sent.load(Ordering::SeqCst), 0);
        load();
        app.run("plugin:ui-policy-probe/f", json!({"op":"layer.addUrl","params":p})).unwrap();
        let request = app.session.take_url_requests().pop().unwrap();
        app.add_url(request.params(), Some(&request), None).unwrap();
        assert_eq!(sent.load(Ordering::SeqCst), 1, "the actual probe starts under the live owner");
        assert_eq!(app.ui.cog_probes.len(), 1);
        plugins::unload("ui-policy-probe");
        *app.ui.cog_probes[0].2.lock().unwrap() = Some(Ok(false));
        app.poll_data_downloads();
        assert_eq!(sent.load(Ordering::SeqCst), 1, "revoked probe starts no whole-file fallback");
        assert!(app.session.map().layer_by_name("Held source").is_none());
        for done in std::mem::take(&mut *held.lock().unwrap()) {
            done(Err("probe revoked".into()));
        }
        // The same real probe can succeed while its owner remains loaded. Reuse the built-in
        // FGB writer and installed fetcher; no server or replacement completion token is needed.
        let bytes = rustgis_engine::data::source::fgb::write(&app.session.map().layer_by_name("Cities").unwrap().dataset).unwrap();
        cache::set_fetcher(move |url, done| {
            let (url, _) = cache::split_download_limit(&url);
            let (_, range) = cache::split_range(url);
            let answer = match range {
                Some((a, b)) => bytes.get(a as usize..b.saturating_add(1).min(bytes.len() as u64) as usize).unwrap_or_default().to_vec(),
                None => bytes.clone(),
            };
            done(Ok(answer));
        });
        load();
        let p = json!({"url":"http://127.0.0.1:9/policy-recorded.fgb", "name":"Recorded source"});
        app.run("plugin:ui-policy-probe/f", json!({"op":"layer.addUrl", "params":p})).unwrap();
        let request = app.session.take_url_requests().pop().unwrap();
        app.ws.recording = Some(Vec::new());
        app.add_url(request.params(), Some(&request), None).unwrap();
        assert_eq!(app.ui.cog_probes.len(), 1, "the actual live probe still lands through its UI receiver");
        app.poll_data_downloads();
        let steps = app.ws.recording.as_ref().unwrap();
        assert!(app.session.map().layer_by_name("Recorded source").is_some());
        assert_eq!(steps.len(), 1, "successful in-place delivery records its replayable command");
        assert_eq!(steps[0].command, "layer.addUrl");
        assert_eq!(steps[0].params["url"], p["url"]);
        assert!(steps[0].params.get("id").is_none(), "opaque completion ids are not replay parameters");
        assert!(steps[0].params.get("request").is_none(), "request receipts are not replay parameters");
        plugins::unload("ui-policy-probe");
    }

    /// A GeoTIFF asked for as a scan keeps only its pixels: the app downloads it whole, once,
    /// without first reading its header to see whether it could be read in place.
    #[test]
    fn a_scan_by_address_is_downloaded_whole_without_a_probe() {
        let _turn = crate::fetching::turn();
        use rustgis_engine::plugins;
        use std::io::{Read, Write};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/scene.tif", listener.local_addr().unwrap());
        let file = rustgis_engine::data::raster::cog::example(512, 256);
        let asked = std::sync::Arc::new(AtomicUsize::new(0));
        let counted = asked.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { continue };
                counted.fetch_add(1, Ordering::Relaxed);
                let _ = s.read(&mut [0u8; 4096]);
                let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", file.len());
                let _ = s.write_all(&file);
            }
        });
        let pkg = plugins::Package::from_zip(&plugins::testing::package("ui-scan", &["edit", "network"])).unwrap();
        plugins::load(pkg, "test").unwrap();
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1400.0, 860.0)).with_step_dt(0.05).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            RustGisApp::with_sample(),
        );
        let params = json!({"url": url, "as": "scan", "name": "Scan"});
        let r = h.state_mut().run("plugin:ui-scan/f", json!({"op": "layer.addUrl", "params": params})).unwrap();
        assert_eq!(r["pending"], true);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while h.state().session.map().layer_by_name("Scan").is_none() {
            assert!(std::time::Instant::now() < deadline, "timed out waiting");
            h.step();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(h.state().session.map().layer_by_name("Scan").is_some_and(|l| l.is_raster()));
        assert!(h.state().ui.cog_probes.is_empty());
        assert_eq!(asked.load(Ordering::Relaxed), 1, "one download, no probe first");
        plugins::unload("ui-scan");
    }

    /// A plugin's command asking for a window of a cloud-optimised GeoTIFF doesn't wait for it:
    /// the app reads it off the frame, adds it when it lands, and runs the command again, whose
    /// `layer.addUrl` answers what was added (#1330).
    #[test]
    fn a_plugin_reads_a_window_off_the_frame() {
        let _turn = crate::fetching::turn();
        use rustgis_engine::plugins;
        use std::sync::atomic::Ordering;
        let slow = crate::fetching::slow_ranges(rustgis_engine::data::raster::cog::example(1024, 256));
        let pkg = plugins::Package::from_zip(&plugins::testing::package("ui-window", &["edit", "network"])).unwrap();
        plugins::load(pkg, "test").unwrap();
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1400.0, 860.0)).with_step_dt(0.05).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            RustGisApp::with_sample(),
        );
        let url = format!("{}/scene.tif", slow.origin);
        // The header in hand, so only the window's tiles wait.
        rustgis_engine::render::rustgis_tiles::cog::info_blocking(&url).unwrap();
        slow.open.store(false, Ordering::Relaxed);
        let params = json!({"url": url, "bbox": [10.25, 45.5, 10.5, 45.75], "name": "Window"});
        let r = h.state_mut().run("plugin:ui-window/f", json!({"op": "layer.addUrl", "params": params})).unwrap();
        assert_eq!(r["pending"], true);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while slow.held.load(Ordering::Relaxed) == 0 {
            assert!(std::time::Instant::now() < deadline, "no request held");
            let frame = std::time::Instant::now();
            h.step();
            assert!(frame.elapsed() < std::time::Duration::from_secs(5), "the read held the frame");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(h.state().session.map().layer_by_name("Window").is_none(), "nothing waited");
        assert!(h.state().session.plugin_retries_waiting());
        slow.open.store(true, Ordering::Relaxed);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let landed = |app: &RustGisApp| app.session.map().layer_by_name("Window").is_some() && !app.session.plugin_retries_waiting();
        while !landed(h.state()) {
            assert!(std::time::Instant::now() < deadline, "timed out waiting: {:?}", h.state().session.notices);
            h.step();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let app = h.state();
        assert!(app.session.map().layer_by_name("Window").is_some_and(|l| l.is_raster()));
        assert!(app.session.map().layer_by_name("Window (2)").is_none(), "added once");
        assert!(app.session.notices.iter().all(|n| !n.error), "{:?}", app.session.notices);
        plugins::unload("ui-window");
    }

    /// A download that lands after its plugin command stopped waiting for it isn't added, and
    /// that is said once, with its address, whether or not that holds a key (#1202).
    #[test]
    fn a_late_download_is_refused_in_one_notice() {
        #[cfg(not(target_arch = "wasm32"))]
        if crate::isolate_account_test("pluginpanel::tests::a_late_download_is_refused_in_one_notice") {
            return;
        }
        use rustgis_engine::plugins;
        let pkg = plugins::Package::from_zip(&plugins::testing::package("ui-late-said", &["edit", "network"])).unwrap();
        plugins::load(pkg, "test").unwrap();
        for first in ["https://x.org/first.csv", "https://x.org/first.csv?token=hunter2"] {
            let mut app = RustGisApp::with_sample();
            app.run("plugin:ui-late-said/f", json!({"op": "layer.addUrl", "params": {"url": first}})).unwrap();
            // The app takes it and starts downloading; the command is run again for another
            // address, so nothing waits for the first now.
            let taken = app.session.take_url_requests();
            assert_eq!(taken.len(), 1);
            let id = app.session.download_started_for(&taken[0]).unwrap();
            app.run("plugin:ui-late-said/f", json!({"op": "layer.addUrl", "params": {"url": "https://x.org/second.csv"}})).unwrap();
            let mark = app.session.notice_mark();
            let layers = app.session.map().layers.len();
            let landed = crate::serviceui::Landed { id, url: first.to_string(), params: taken[0].params().clone(), origin: None };
            app.add_landed(landed, b"name,lon,lat\nA,10,50\n".to_vec());
            assert_eq!(app.session.map().layers.len(), layers, "not added");
            let said: Vec<String> = app.session.notices_since(mark).iter().filter(|n| n.error).map(|n| n.text.clone()).collect();
            assert_eq!(said.len(), 1, "{said:?}");
            assert!(said[0].starts_with("https://x.org/first.csv") && said[0].contains("stopped waiting"), "{said:?}");
            assert!(!said[0].contains("hunter2"), "{said:?}");
        }
        plugins::unload("ui-late-said");
    }

    /// A download that fails answers the command waiting for it at once, not when the wait runs
    /// out, from what the app keeps of the request: a file read whole, and a service's page (#1202).
    #[test]
    fn a_failed_download_answers_the_command_waiting_at_once() {
        #[cfg(not(target_arch = "wasm32"))]
        if crate::isolate_account_test("pluginpanel::tests::a_failed_download_answers_the_command_waiting_at_once") {
            return;
        }
        use rustgis_engine::plugins;
        use rustgis_engine::render::rustgis_tiles::cache;
        let _turn = crate::fetching::turn();
        cache::set_fetcher(|_, done| done(Err("404 Not Found".into())));
        let pkg = plugins::Package::from_zip(&plugins::testing::package("ui-fail", &["edit", "network"])).unwrap();
        plugins::load(pkg, "test").unwrap();
        for params in [json!({"url": "https://x.org/a.csv"}), json!({"url": "https://x.org/collections/a/items", "service": true})] {
            let mut app = RustGisApp::with_sample();
            app.run("plugin:ui-fail/f", json!({"op": "layer.addUrl", "params": params})).unwrap();
            let taken = app.session.take_url_requests();
            assert_eq!(taken.len(), 1);
            app.add_url(taken[0].params(), Some(&taken[0]), None).unwrap();
            let ctx = egui::Context::default();
            let mut ran = Vec::new();
            for _ in 0..400 {
                app.poll_data_downloads();
                crate::serviceui::poll_pages(&mut app, &ctx);
                ran = app.session.poll_plugin_retries();
                if !ran.is_empty() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert_eq!(ran.len(), 1, "{params}: {ran:?}");
            assert!(ran[0].1.as_ref().is_err_and(|e| e.contains("404")), "{params}: {ran:?}");
            assert!(!app.session.plugin_retries_waiting(), "{params}");
        }
        plugins::unload("ui-fail");
    }

    /// What a server was asked and hasn't answered yet.
    type Held = std::sync::Arc<std::sync::Mutex<Vec<(String, rustgis_engine::render::rustgis_tiles::cache::Done)>>>;

    /// A service that answers only when told to ([`answer_held`]): what the window does meanwhile
    /// can't be waiting for it. A test with no turn may start a download meanwhile: that one isn't
    /// counted as asked here, and is answered at once, as a fetcher must.
    fn hold_requests() -> Held {
        let held = Held::default();
        let keep = held.clone();
        rustgis_engine::render::rustgis_tiles::cache::set_fetcher(move |url, done| {
            if url.contains(".example.") {
                keep.lock().unwrap().push((url, done));
            } else {
                done(Err("not this test's service".into()));
            }
        });
        held
    }

    /// Answers what was asked by `answer`, and says what that was.
    fn answer_held(held: &Held, answer: &dyn Fn(&str) -> Result<Vec<u8>, String>) -> Vec<String> {
        let asked: Vec<_> = held.lock().unwrap().drain(..).collect();
        asked
            .into_iter()
            .map(|(url, done)| {
                done(answer(&url));
                url
            })
            .collect()
    }

    /// What the frame loop does for a waiting command: starts what it asked for, adds what came
    /// in, and runs it again when it can.
    fn frame(app: &mut RustGisApp) -> Vec<(String, Result<Value, String>)> {
        for request in app.session.take_url_requests() {
            let _ = app.add_url(request.params(), Some(&request), None);
        }
        app.poll_data_downloads();
        crate::serviceui::poll_pages(app, &egui::Context::default());
        app.session.poll_plugin_retries()
    }

    /// Frames until the command stops waiting: every address asked for, in order, and how it ended.
    fn run_out(
        app: &mut RustGisApp,
        held: &Held,
        answer: &dyn Fn(&str) -> Result<Vec<u8>, String>,
    ) -> (Vec<String>, Result<Value, String>) {
        let mut asked = Vec::new();
        let mut end = Err("it never ran again".to_string());
        for _ in 0..50 {
            asked.extend(answer_held(held, answer));
            if let Some((_, r)) = frame(app).pop() {
                end = r;
            }
            if !app.session.plugin_retries_waiting() && held.lock().unwrap().is_empty() {
                return (asked, end);
            }
        }
        panic!("it still waits: {asked:?} {end:?}");
    }

    const WFS: &str = r#"<WFS_Capabilities version="2.0.0"><OperationsMetadata><Operation name="GetFeature"><DCP><HTTP><Get href="https://wfs.example.org/wfs?"/></HTTP></DCP></Operation></OperationsMetadata>
<FeatureTypeList><FeatureType><Name>solos</Name><Title>Solos</Title><DefaultCRS>urn:ogc:def:crs:OGC:1.3:CRS84</DefaultCRS><WGS84BoundingBox><LowerCorner>-74 -34</LowerCorner><UpperCorner>-34 6</UpperCorner></WGS84BoundingBox></FeatureType></FeatureTypeList></WFS_Capabilities>"#;
    const FEATURES: &str = r#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"Point","coordinates":[-43,-5]},"properties":{"n":1}}]}"#;

    /// The WFS above: its capabilities, how many features it has, and its features.
    fn wfs(url: &str) -> Result<Vec<u8>, String> {
        Ok(if url.contains("GetCapabilities") {
            WFS
        } else if url.contains("RESULTTYPE=hits") {
            r#"<wfs:FeatureCollection xmlns:wfs="http://www.opengis.net/wfs/2.0" numberMatched="1" numberReturned="0"/>"#
        } else {
            FEATURES
        }
        .as_bytes()
        .to_vec())
    }

    fn solos(bbox: Value) -> Value {
        json!({"op": "service.add", "params": {"url": "https://wfs.example.org/wfs", "type": "wfs", "layer": "solos", "name": "Solos", "bbox": bbox}})
    }

    /// The requests for a layer's features among `asked`, leaving out asking how many there are.
    fn feature_requests(asked: &[String]) -> Vec<&String> {
        asked.iter().filter(|u| u.contains("GetFeature") && !u.contains("RESULTTYPE=hits")).collect()
    }

    /// Whether `r` answers the area `e`, as near as it is written.
    fn area_is(r: &Value, e: [f64; 4]) -> bool {
        r["area"].as_array().is_some_and(|a| a.len() == 4 && a.iter().zip(e).all(|(v, x)| v.as_f64().is_some_and(|v| (v - x).abs() < 1e-3)))
    }

    fn whole_layer_notices(app: &RustGisApp, mark: u64) -> usize {
        app.session.notices_since(mark).iter().filter(|n| n.text.contains("so the whole layer is asked for")).count()
    }

    /// A plugin's `service.add` waits for nothing on the frame (#1319): its command answers with
    /// the server still silent, the window says it is running, and the command runs again for each
    /// thing that comes in, going on with the layer. An area its request carries is said nowhere.
    #[test]
    fn a_plugins_service_add_reads_off_the_frame() {
        use rustgis_engine::plugins;
        let _turn = crate::fetching::turn();
        let held = hold_requests();
        let pkg = plugins::Package::from_zip(&plugins::testing::package("ui-wfs", &["edit", "network"])).unwrap();
        plugins::load(pkg, "test").unwrap();
        let mut app = RustGisApp::with_sample();
        // An error from before doesn't hide that it has started.
        app.session.notify("an old error".to_string(), true);
        let mark = app.session.notice_mark();
        let layers = app.session.map().layers.len();
        let r = app.run("plugin:ui-wfs/f", solos(json!([-44, -6, -42, -4]))).unwrap();
        // Nothing has answered, and the command has.
        assert_eq!(r["pending"], true, "{r}");
        assert!(r["downloading"][0].as_str().unwrap().contains("GetCapabilities"), "{r}");
        assert!(frame(&mut app).is_empty());
        assert!(frame(&mut app).is_empty(), "it waits, frame after frame");
        assert_eq!(held.lock().unwrap().len(), 1, "one request, for the capabilities");
        assert_eq!(app.session.map().layers.len(), layers);
        assert!(crate::pluginstart::running(&app).is_some_and(|t| t.contains("Running")));
        // An error said meanwhile isn't hidden by it.
        app.session.notify("something else went wrong".to_string(), true);
        assert_eq!(crate::pluginstart::running(&app), None);
        app.session.notify("back to normal".to_string(), false);
        // The capabilities in, it asks for the features, off the frame too.
        assert_eq!(answer_held(&held, &wfs).len(), 1);
        let again = frame(&mut app);
        assert_eq!(again.len(), 1, "{again:?}");
        assert_eq!(again[0].1.as_ref().unwrap()["pending"], true, "{again:?}");
        assert!(frame(&mut app).is_empty());
        assert_eq!(held.lock().unwrap().len(), 1, "one request, for the features");
        assert_eq!(app.session.map().layers.len(), layers, "not added before it comes");
        let (asked, end) = run_out(&mut app, &held, &wfs);
        let r = end.unwrap();
        let features = feature_requests(&asked);
        assert!(features.len() == 1 && features[0].contains("BBOX=-44,-6,-42,-4"), "{asked:?}");
        // It goes on with the layer as if `service.add` had waited: added once, known by its address.
        let added = app.session.map().layer_by_name("Solos").unwrap();
        assert_eq!(r["layer"], json!(added.id), "{r}");
        assert_eq!(r["area"], json!([-44.0, -6.0, -42.0, -4.0]), "{r}");
        assert!(added.dataset.source.as_deref().is_some_and(|s| s.contains("GetFeature")));
        assert_eq!(app.session.map().layers.len(), layers + 1);
        assert_eq!(whole_layer_notices(&app, mark), 0, "{:?}", app.session.notices_since(mark));
        assert_eq!(crate::pluginstart::running(&app), None);
        // Its capabilities given by the command: only its features are asked for.
        let mut given = solos(json!([-44, -6, -42, -4]));
        given["params"]["text"] = json!(WFS);
        given["params"]["name"] = json!("Given");
        let r = app.run("plugin:ui-wfs/f", given).unwrap();
        assert_eq!(r["pending"], true, "{r}");
        let (asked, end) = run_out(&mut app, &held, &wfs);
        assert!(asked.iter().all(|u| !u.contains("GetCapabilities")) && feature_requests(&asked).len() == 1, "{asked:?}");
        assert!(end.is_ok() && app.session.map().layer_by_name("Given").is_some(), "{end:?}");
        plugins::unload("ui-wfs");
    }

    /// An area its request can't carry is said once, not once for each time the command runs.
    #[test]
    fn a_whole_layer_asked_for_is_said_once_by_a_command_run_again() {
        use rustgis_engine::plugins;
        let _turn = crate::fetching::turn();
        let held = hold_requests();
        let pkg = plugins::Package::from_zip(&plugins::testing::package("ui-whole", &["edit", "network"])).unwrap();
        plugins::load(pkg, "test").unwrap();
        let mut app = RustGisApp::with_sample();
        let mark = app.session.notice_mark();
        app.run("plugin:ui-whole/f", solos(json!([170, -6, -170, -4]))).unwrap();
        let (asked, end) = run_out(&mut app, &held, &wfs);
        let r = end.unwrap();
        let features = feature_requests(&asked);
        assert!(features.len() == 1 && !features[0].contains("BBOX"), "{asked:?}");
        assert!(r.get("area").is_none() && r["layer"].is_u64(), "{r}");
        assert_eq!(whole_layer_notices(&app, mark), 1, "{:?}", app.session.notices_since(mark));
        plugins::unload("ui-whole");
    }

    /// A service that fails answers the command at once, and that is said once: its capabilities,
    /// and its features after them.
    #[test]
    fn a_service_that_fails_answers_a_plugins_service_add() {
        use rustgis_engine::plugins;
        let _turn = crate::fetching::turn();
        let held = hold_requests();
        let pkg = plugins::Package::from_zip(&plugins::testing::package("ui-down", &["edit", "network"])).unwrap();
        plugins::load(pkg, "test").unwrap();
        // Its features failing is said by the download, with the address, then by the command.
        let down: [(&dyn Fn(&str) -> Result<Vec<u8>, String>, usize); 2] = [
            (&|_| Err("502 Bad Gateway".to_string()), 1),
            (&|url| if url.contains("GetCapabilities") { wfs(url) } else { Err("502 Bad Gateway".to_string()) }, 2),
        ];
        for (answer, said) in down {
            let mut app = RustGisApp::with_sample();
            let mark = app.session.notice_mark();
            let layers = app.session.map().layers.len();
            app.run("plugin:ui-down/f", solos(json!([-44, -6, -42, -4]))).unwrap();
            let (_, end) = run_out(&mut app, &held, answer);
            let e = end.unwrap_err();
            assert!(e.contains("502"), "{e}");
            assert_eq!(app.session.map().layers.len(), layers);
            let notices = app.session.notices_since(mark);
            let told = notices.iter().filter(|n| n.error && n.text.contains("502")).count();
            assert_eq!(told, said, "{notices:?}");
            assert!(frame(&mut app).is_empty(), "nothing left to run");
        }
        plugins::unload("ui-down");
    }

    /// The window answers while a command waits, so the user may move the map or resize the window:
    /// the command asking for the view runs again with the one it began with, and asks for no other
    /// area. The user's view stays, and the layer comes once.
    #[test]
    fn a_view_moved_while_a_command_waits_asks_for_no_other_area() {
        use rustgis_engine::plugins;
        let _turn = crate::fetching::turn();
        let held = hold_requests();
        let pkg = plugins::Package::from_zip(&plugins::testing::package("ui-view", &["edit", "network"])).unwrap();
        plugins::load(pkg, "test").unwrap();
        let mut app = RustGisApp::with_sample();
        let layers = app.session.map().layers.len();
        app.session.run("map.zoomToExtent", &json!({"extent": [-44, -6, -42, -4]})).unwrap();
        let began = rustgis_engine::cmd::workspace::view_extent(&app.session);
        app.run("plugin:ui-view/f", solos(json!("view"))).unwrap();
        let mut moved = app.session.map().camera;
        moved.center.x += 1_000_000.0;
        app.session.map_mut().camera = moved;
        app.session.view_size = (600.0, 300.0);
        assert_ne!(rustgis_engine::cmd::workspace::view_extent(&app.session), began);
        let (asked, end) = run_out(&mut app, &held, &wfs);
        let r = end.unwrap();
        let features = feature_requests(&asked);
        assert!(features.len() == 1 && features[0].contains("BBOX"), "{asked:?}");
        assert!(r["layer"].is_u64(), "{r}");
        assert!(area_is(&r, [began.min_x, began.min_y, began.max_x, began.max_y]), "{r} {began:?}");
        assert_eq!(app.session.map().layers.len(), layers + 1);
        assert_eq!(app.session.map().camera, moved, "the user's view stays");
        assert_eq!(app.session.view_size, (600.0, 300.0));
        plugins::unload("ui-view");
    }

    /// Another map shown while a command waits: its run belongs to the map it began on, so it
    /// isn't run again (#1461). It stops with that error, said once, asks for no features and adds
    /// nothing, and the map shown keeps its view.
    #[test]
    fn another_map_shown_while_a_command_waits_stops_it() {
        use rustgis_engine::plugins;
        let _turn = crate::fetching::turn();
        let held = hold_requests();
        let pkg = plugins::Package::from_zip(&plugins::testing::package("ui-other-map", &["edit", "network"])).unwrap();
        plugins::load(pkg, "test").unwrap();
        let mut app = RustGisApp::with_sample();
        app.session.run("map.zoomToExtent", &json!({"extent": [-44, -6, -42, -4]})).unwrap();
        app.run("plugin:ui-other-map/f", solos(json!("view"))).unwrap();
        app.session.run("map.new", &json!({"name": "Second"})).unwrap();
        app.session.run("map.activate", &json!({"name": "Second"})).unwrap();
        let mut second = app.session.map().camera;
        second.center.x += 1_000_000.0;
        app.session.map_mut().camera = second;
        let mark = app.session.notice_mark();
        let (asked, end) = run_out(&mut app, &held, &wfs);
        let e = end.unwrap_err();
        assert!(e.contains("another map"), "{e}");
        let said = app.session.notices_since(mark).iter().filter(|n| n.error && n.text.contains("another map")).count();
        assert_eq!(said, 1);
        assert!(feature_requests(&asked).is_empty(), "{asked:?}");
        assert!(app.session.project.maps.iter().all(|m| m.layer_by_name("Solos").is_none()));
        assert_eq!(app.session.map().camera, second, "that map's view stays");
        assert!(!app.session.plugin_retries_waiting());
        plugins::unload("ui-other-map");
    }

    /// A box given moves the view when the layer is in, whatever the user did meanwhile: what the
    /// command itself does to the view stands.
    #[test]
    fn a_box_given_frames_the_layer_once_it_is_in() {
        use rustgis_engine::plugins;
        let _turn = crate::fetching::turn();
        let held = hold_requests();
        let pkg = plugins::Package::from_zip(&plugins::testing::package("ui-box", &["edit", "network"])).unwrap();
        plugins::load(pkg, "test").unwrap();
        let mut app = RustGisApp::with_sample();
        app.run("plugin:ui-box/f", solos(json!([-44, -6, -42, -4]))).unwrap();
        let mut moved = app.session.map().camera;
        moved.center.x += 1_000_000.0;
        app.session.map_mut().camera = moved;
        let (_, end) = run_out(&mut app, &held, &wfs);
        end.unwrap();
        assert_ne!(app.session.map().camera, moved);
        let center = app.session.map().camera.center_lonlat();
        assert!((center.x + 43.0).abs() < 0.01 && (center.y + 5.0).abs() < 0.01, "{center:?}");
        plugins::unload("ui-box");
    }

    /// A map layer needs only its service's documents: its capabilities, and an OGC API layer's
    /// tilesets, all off the frame; what it answers once they are in adds it.
    #[test]
    fn a_map_layer_of_a_plugins_service_add_needs_only_its_documents() {
        use rustgis_engine::plugins;
        const WMS: &str = r#"<WMS_Capabilities version="1.3.0" xmlns="http://www.opengis.net/wms" xmlns:xlink="http://www.w3.org/1999/xlink">
<Service><Title>Test WMS</Title></Service>
<Capability><Request><GetMap><Format>image/png</Format><DCPType><HTTP><Get><OnlineResource xlink:href="https://maps.example.com/wms?"/></Get></HTTP></DCPType></GetMap></Request>
<Layer><CRS>EPSG:3857</CRS><Layer><Name>roads</Name><Title>Roads</Title></Layer></Layer></Capability></WMS_Capabilities>"#;
        const API: &str = r#"{"collections": [{"id": "relief", "links": [{"rel": "tilesets-map", "href": "https://maps.example.com/relief/map/tiles"}]}]}"#;
        const TILESETS: &str = r#"{"tilesets": [{"tileMatrixSetId": "WebMercatorQuad", "links": [{"rel": "self", "href": "https://maps.example.com/relief/map/tiles/smerc"}]}]}"#;
        const TILESET: &str = r#"{"links": [{"rel": "item", "type": "image/png", "href": "https://maps.example.com/relief/{tileMatrix}/{tileRow}/{tileCol}.png"}]}"#;
        let _turn = crate::fetching::turn();
        let held = hold_requests();
        let pkg = plugins::Package::from_zip(&plugins::testing::package("ui-map", &["edit", "network"])).unwrap();
        plugins::load(pkg, "test").unwrap();
        let mut app = RustGisApp::with_sample();
        let wms = json!({"op": "service.add", "params": {"url": "https://maps.example.com/wms", "type": "wms", "layer": "roads"}});
        assert_eq!(app.run("plugin:ui-map/f", wms).unwrap()["pending"], true);
        assert!(app.session.map().tiles.iter().all(|t| t.name != "Roads"));
        let (asked, end) = run_out(&mut app, &held, &|_| Ok(WMS.as_bytes().to_vec()));
        assert_eq!(asked.len(), 1, "{asked:?}");
        assert!(end.is_ok() && app.session.map().tiles.iter().any(|t| t.name == "Roads"), "{end:?}");
        // An OGC API layer: its collections, the tileset list, then the tileset with its template.
        let api = json!({"op": "service.add", "params": {"url": "https://maps.example.com/collections", "type": "ogcapi", "layer": "relief (map tiles)"}});
        assert_eq!(app.run("plugin:ui-map/f", api).unwrap()["pending"], true);
        let (asked, end) = run_out(&mut app, &held, &|url| {
            Ok(if url.contains("smerc") {
                TILESET
            } else if url.contains("/relief/") {
                TILESETS
            } else {
                API
            }
            .as_bytes()
            .to_vec())
        });
        assert_eq!(asked.len(), 3, "{asked:?}");
        assert!(end.is_ok(), "{end:?}");
        assert!(app.session.map().tiles.iter().any(|t| t.url == "https://maps.example.com/relief/{z}/{y}/{x}.png"));
        let standard = |app: &RustGisApp, mark| {
            app.session.notices_since(mark).iter().filter(|n| n.text.contains("the standard tile address is used")).count()
        };
        assert_eq!(standard(&app, 0), 0, "the tiles it names are used");
        // Its tile sets not read: the standard address, and that is said, once.
        let mark = app.session.notice_mark();
        let api = json!({"op": "service.add", "params": {"url": "https://maps.example.com/collections", "type": "ogcapi", "layer": "relief (map tiles)", "name": "Standard"}});
        assert_eq!(app.run("plugin:ui-map/f", api).unwrap()["pending"], true);
        let (_, end) = run_out(&mut app, &held, &|url| {
            if url.contains("/relief/") { Err("502 Bad Gateway".to_string()) } else { Ok(API.as_bytes().to_vec()) }
        });
        assert!(end.is_ok(), "{end:?}");
        assert_eq!(standard(&app, mark), 1, "{:?}", app.session.notices_since(mark));
        plugins::unload("ui-map");
    }

    /// A coverage: its capabilities and description off the frame, then downloaded and added as
    /// Add From Service does, and the command goes on with it; the service's error is its answer.
    #[test]
    fn a_coverage_of_a_plugins_service_add_reads_off_the_frame() {
        use rustgis_engine::data::raster as r;
        use rustgis_engine::plugins;
        const WCS: &str = r#"<wcs:Capabilities version="2.0.1" xmlns:wcs="http://www.opengis.net/wcs/2.0" xmlns:ows="http://www.opengis.net/ows/2.0">
<ows:ServiceIdentification><ows:Title>Soils</ows:Title></ows:ServiceIdentification>
<wcs:Contents><wcs:CoverageSummary><wcs:CoverageId>soc</wcs:CoverageId><ows:Title>Soil carbon</ows:Title>
<ows:WGS84BoundingBox><ows:LowerCorner>5 49.2</ows:LowerCorner><ows:UpperCorner>5.8 50</ows:UpperCorner></ows:WGS84BoundingBox>
</wcs:CoverageSummary></wcs:Contents></wcs:Capabilities>"#;
        const DESCRIBE: &str = r#"<CoverageDescriptions><CoverageDescription><Envelope srsName="http://www.opengis.net/def/crs/EPSG/0/4326" axisLabels="Lat Long">
<lowerCorner>49.2 5</lowerCorner><upperCorner>50 5.8</upperCorner></Envelope></CoverageDescription></CoverageDescriptions>"#;
        let raster = r::Raster::new(
            "soc",
            8,
            8,
            vec![vec![3.0; 64]],
            r::GeoTransform { origin_x: 5.0, origin_y: 50.0, pixel_width: 0.1, pixel_height: -0.1 },
            r::Crs::LonLat,
            None,
            r::ColorHint::Gray,
            "Float32",
        )
        .unwrap();
        let tif = r::geotiff::write(&raster, 0).unwrap();
        let _turn = crate::fetching::turn();
        let held = hold_requests();
        let pkg = plugins::Package::from_zip(&plugins::testing::package("ui-wcs", &["edit", "network"])).unwrap();
        plugins::load(pkg, "test").unwrap();
        let mut app = RustGisApp::with_sample();
        let add = |name: &str| json!({"op": "service.add", "params": {"url": "https://wcs.example.org/wcs", "type": "wcs", "layer": "soc", "name": name, "bbox": [5, 49.2, 5.4, 50]}});
        assert_eq!(app.run("plugin:ui-wcs/f", add("Carbon")).unwrap()["pending"], true);
        let doc = |url: &str| {
            (if url.contains("GetCapabilities") {
                Some(WCS)
            } else if url.contains("DescribeCoverage") {
                Some(DESCRIBE)
            } else {
                None
            })
            .map(str::as_bytes)
            .map(<[u8]>::to_vec)
        };
        let (asked, end) = run_out(&mut app, &held, &|url| Ok(doc(url).unwrap_or_else(|| tif.clone())));
        let r = end.unwrap();
        assert!(asked.len() == 3 && asked[2].contains("GetCoverage"), "{asked:?}");
        let added = app.session.map().layer_by_name("Carbon").unwrap();
        assert!(added.is_raster() && r["layer"] == json!(added.id), "{r}");
        assert!(added.dataset.source.as_deref().is_some_and(|s| s.contains("GetCoverage")));
        // An address ending in .tif is a coverage too: downloaded whole, as a raster.
        let tif_address = json!({"op": "service.add", "params": {"url": "https://wcs.example.org/soc.tif", "type": "wcs", "layer": "soc", "name": "Named", "bbox": [5, 49.2, 5.8, 50]}});
        assert_eq!(app.run("plugin:ui-wcs/f", tif_address).unwrap()["pending"], true);
        let (asked, end) = run_out(&mut app, &held, &|url| Ok(doc(url).unwrap_or_else(|| tif.clone())));
        let r = end.unwrap();
        assert_eq!(asked.len(), 3, "the coverage asked for once, not probed: {asked:?}");
        assert!(app.session.map().layer_by_name("Named").is_some_and(|l| l.is_raster() && r["layer"] == json!(l.id)), "{r}");
        // The service answers with its own error, not a GeoTIFF.
        assert_eq!(app.run("plugin:ui-wcs/f", add("Refused")).unwrap()["pending"], true);
        let (_, end) =
            run_out(
                &mut app,
                &held,
                &|url| Ok(doc(url).unwrap_or_else(|| b"<ServiceExceptionReport>no</ServiceExceptionReport>".to_vec())),
            );
        assert!(end.is_err() && app.session.map().layer_by_name("Refused").is_none(), "{end:?}");
        plugins::unload("ui-wcs");
    }

    /// A service's page that fails is what the plugin command that asked for it is answered, when
    /// it fails, not the wait running out (#1202).
    #[test]
    fn a_failed_service_read_answers_the_command_waiting() {
        #[cfg(not(target_arch = "wasm32"))]
        if crate::isolate_account_test("pluginpanel::tests::a_failed_service_read_answers_the_command_waiting") {
            return;
        }
        use rustgis_engine::plugins;
        let pkg = plugins::Package::from_zip(&plugins::testing::package("ui-pages", &["edit", "network"])).unwrap();
        plugins::load(pkg, "test").unwrap();
        let mut app = RustGisApp::with_sample();
        let url = "https://x.org/collections/a/items";
        app.run("plugin:ui-pages/f", json!({"op": "layer.addUrl", "params": {"url": url, "service": true}})).unwrap();
        let taken = app.session.take_url_requests();
        assert_eq!(taken.len(), 1);
        let id = app.session.download_started_for(&taken[0]).unwrap();
        let landed = crate::serviceui::Landed { id, url: url.to_string(), params: taken[0].params().clone(), origin: None };
        crate::serviceui::go_on(&mut app, landed, 0, rustgis_engine::data::ogc::Step::Failed("its second page failed".into()));
        let ran = app.session.poll_plugin_retries();
        assert_eq!(ran.len(), 1, "{ran:?}");
        assert!(ran[0].1.as_ref().is_err_and(|e| e.contains("its second page failed")), "{ran:?}");
        assert!(!app.session.plugin_retries_waiting());
        plugins::unload("ui-pages");
    }
}
