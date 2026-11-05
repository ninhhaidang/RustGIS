//! The time slider, docked under the map: play, pause and step through the map's time span on a
//! timeline whose bars count the features of each step, choose the step and the window shown, and
//! bind layers to it.

use egui::{Color32, Rect, Sense, Stroke, Ui, pos2, vec2};
use rustgis_engine::data::time::Unit;
use rustgis_engine::map::time::{TimeSlider, has_dates, histogram, instants, time_fields};
use serde_json::{Value, json};

use crate::theme::{Tokens, regular, semibold};
use crate::widgets::small;
use crate::{RustGisApp, icons};

/// The bar's own state.
#[derive(Default)]
pub struct TimeBarState {
    /// Closed by its close button (the slider stays on; Time Slider opens it again).
    pub hidden: bool,
    /// Playing: the milliseconds since the last step.
    pub playing: Option<f64>,
    /// The features of each step, and what they were counted for: the map and steps, and each
    /// bound layer's rows and fields (shared until an edit changes them).
    counts: Option<(CountsKey, Vec<Bound>, Vec<u32>)>,
    /// The step's interval as dragged or typed, until it is applied.
    interval: Option<u32>,
    /// The slider's steps, and the slider they were listed for.
    steps: Option<(TimeSlider, std::sync::Arc<Vec<f64>>)>,
}

/// The slider's steps, listed again only when its span or steps change (not when it moves).
fn steps(app: &mut RustGisApp, t: &TimeSlider) -> std::sync::Arc<Vec<f64>> {
    let same = |a: &TimeSlider| (a.start, a.end, a.unit, a.interval, &a.dates) == (t.start, t.end, t.unit, t.interval, &t.dates);
    if let Some((k, s)) = &app.time.steps
        && same(k)
    {
        return s.clone();
    }
    let s = std::sync::Arc::new(t.steps());
    app.time.steps = Some((t.clone(), s.clone()));
    s
}

/// A window preset (`time.set`'s `show`), as the Show menu names it.
fn show_label(code: &str) -> String {
    match code {
        "step" => tl!("This step"),
        "wide" => tl!("Wide (±1 step)"),
        "wider" => tl!("Wider (±3 steps)"),
        "cumulative" => tl!("Cumulative"),
        _ => tl!("Custom"),
    }
    .to_string()
}

fn unit_label(u: Unit) -> String {
    match u {
        Unit::Hour => tl!("Hour"),
        Unit::Day => tl!("Day"),
        Unit::Month => tl!("Month"),
        Unit::Year => tl!("Year"),
    }
    .to_string()
}

/// Whether the bar shows: the map has a slider and the bar wasn't closed.
pub fn open(app: &RustGisApp) -> bool {
    app.session.map().time.is_some() && !app.time.hidden
}

/// Opens the time slider: shows the bar when the map has one, else binds the active layer (or the
/// first with a date field).
pub fn toggle(app: &mut RustGisApp) -> Result<Value, String> {
    if app.session.map().time.is_some() {
        app.time.hidden = !app.time.hidden;
        if app.time.hidden {
            app.time.playing = None;
        }
        return Ok(Value::Null);
    }
    // A layer with real dates, not just numbers that look like years (bind those from the layer's
    // menu or the bar's Layers menu).
    let m = app.session.map();
    let dated = |id: u32| m.layer(id).is_some_and(|l| !l.is_raster() && has_dates(&l.dataset));
    let layer = app
        .session
        .active_layer
        .filter(|id| dated(*id))
        .or_else(|| m.layers.iter().find(|l| !l.is_raster() && has_dates(&l.dataset)).map(|l| l.id))
        .ok_or("no layer has a field of dates: add one that has, or a tile layer whose address has a date (map.addTiles)")?;
    app.time.hidden = false;
    app.run("time.bind", json!({"layer": layer}))
}

/// Starts or stops playing (from the start when at the end and not looping).
pub fn play(app: &mut RustGisApp) -> Result<Value, String> {
    let Some(t) = app.session.map().time.clone() else { return Err("the time slider is off".into()) };
    if app.time.playing.take().is_none() {
        let all = steps(app, &t);
        if t.index_in(&all) + 1 >= all.len() && !t.looping {
            app.run("time.goTo", json!({"step": 0}))?;
        }
        app.time.playing = Some(0.0);
        app.time.hidden = false;
    }
    Ok(Value::Null)
}

/// Exports the steps as an animated GIF.
pub fn export(app: &mut RustGisApp) -> Result<Value, String> {
    if app.services.download.is_some() {
        return Err("exporting time animations needs the desktop app".into());
    }
    let name = crate::i18n::fmt("{map} time.gif", &[("map", &app.session.map().name)]);
    match crate::dialogs::pick_save(&name, &["gif"], tl!("Animated GIF")) {
        Some(path) => app.run("time.export", json!({"path": path})),
        None => Ok(Value::Null),
    }
}

/// Steps while playing.
fn advance(app: &mut RustGisApp, ui: &Ui) {
    let Some(elapsed) = app.time.playing else { return };
    let Some(t) = app.session.map().time.clone() else {
        app.time.playing = None;
        return;
    };
    let elapsed = elapsed + f64::from(ui.input(|i| i.stable_dt).min(0.25)) * 1000.0;
    // A Zarr step still being read holds the next one, and shows for a whole step once it lands:
    // playing goes at the pace the steps are read, as it does where the read waits, and each
    // step shows.
    if app.session.zarr_waiting() {
        app.time.playing = Some(0.0);
        ui.ctx().request_repaint_after(web_time::Duration::from_millis(50));
        return;
    }
    if elapsed >= f64::from(t.speed_ms) {
        if t.at_end() && !t.looping {
            app.time.playing = None;
            return;
        }
        let _ = app.run("time.step", json!({"by": 1, "wrap": t.looping}));
        app.time.playing = Some(0.0);
    } else {
        app.time.playing = Some(elapsed);
    }
    // Woken for the next step, not every frame (soon enough to stay on time).
    let left = f64::from(t.speed_ms) - app.time.playing.unwrap_or(0.0);
    ui.ctx().request_repaint_after(web_time::Duration::from_millis(left.clamp(1.0, 50.0) as u64));
}

/// The project, map and steps the counts were made for.
type CountsKey = (u64, usize, Option<f64>, Option<f64>, usize, Unit, u32);

/// A bound layer as its counts were made from.
type Bound = (u32, String, rustgis_engine::data::Features, Vec<rustgis_engine::data::Field>);

/// The features of the bound layers in each step's own span (a step, whatever the window);
/// worked out again only when the layers or steps change.
fn counts(app: &mut RustGisApp, t: &TimeSlider, steps: &[f64]) -> Vec<u32> {
    let key = (
        app.session.opened,
        app.session.project.active_map,
        steps.first().copied(),
        steps.last().copied(),
        steps.len(),
        t.unit,
        t.interval,
    );
    // Compared with the bound layers as they are, without copying them (this runs every frame).
    let m = app.session.map();
    let live = || m.layers.iter().filter_map(|l| l.time.as_ref().map(|b| (l, b)));
    if let Some((k, b, c)) = &app.time.counts
        && *k == key
        && b.len() == live().count()
        && b.iter()
            .zip(live())
            .all(|(x, (l, t))| x.0 == l.id && x.1 == t.field && x.2.shares(&l.dataset.features) && x.3 == l.dataset.fields)
    {
        return c.clone();
    }
    let bound: Vec<Bound> = live().map(|(l, b)| (l.id, b.field.clone(), l.dataset.features.clone(), l.dataset.fields.clone())).collect();
    let mut out = vec![0u32; steps.len()];
    for (id, field, ..) in &bound {
        let Some(l) = app.session.map().layer(*id) else { continue };
        let h = histogram(instants(&l.dataset, field).into_iter().flatten(), steps, t.unit, t.interval);
        for (o, c) in out.iter_mut().zip(h) {
            *o = o.saturating_add(c);
        }
    }
    app.time.counts = Some((key, bound, out.clone()));
    out
}

pub fn show(app: &mut RustGisApp, ui: &mut Ui) {
    if !open(app) {
        app.time.playing = None;
        return;
    }
    advance(app, ui);
    let Some(t) = app.session.map().time.clone() else { return };
    let tk = Tokens::get(ui.ctx());
    egui::Panel::bottom("time_slider").exact_size(92.0).frame(egui::Frame::NONE.fill(tk.pane)).show(ui, |ui| {
        let full = ui.max_rect();
        ui.painter().line_segment([full.left_top(), full.right_top()], Stroke::new(1.0, tk.border_strong));
        // Header: the date, the window, and the settings.
        let (hr, _) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::hover());
        let mut h = ui.new_child(
            egui::UiBuilder::new().max_rect(hr.shrink2(vec2(10.0, 3.0))).layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        let steps = steps(app, &t);
        let cur = t.index_in(&steps);
        header(app, &mut h, &t, &steps, &tk);
        // Transport and timeline.
        let (br, _) = ui.allocate_exact_size(vec2(ui.available_width(), ui.available_height()), Sense::hover());
        let mut b = ui.new_child(
            egui::UiBuilder::new().max_rect(br.shrink2(vec2(10.0, 4.0))).layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        transport(app, &mut b, &steps);
        timeline(app, &mut b, &t, &steps, cur, &tk);
    });
}

fn header(app: &mut RustGisApp, ui: &mut Ui, t: &TimeSlider, steps: &[f64], tk: &Tokens) {
    let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
    icons::paint(ui.painter(), r, "time_slider", tk.icon, tk.accent);
    ui.label(egui::RichText::new(tl!("Time Slider")).font(semibold(13.0)));
    ui.add_space(8.0);
    ui.label(egui::RichText::new(t.label(t.current)).font(semibold(15.0)).color(tk.accent_text));
    // The window by the steps it takes in: the first, and the last before its end.
    let (from, to) = t.window();
    let last = steps.iter().rev().find(|s| **s < to).copied().unwrap_or(t.current);
    let window = if !from.is_finite() {
        crate::i18n::fmt("showing everything through {to}", &[("to", &t.label(last))])
    } else if t.label(from) == t.label(last) {
        crate::i18n::fmt("showing {date}", &[("date", &t.label(from))])
    } else {
        crate::i18n::fmt("showing {from} to {to}", &[("from", &t.label(from)), ("to", &t.label(last))])
    };
    ui.label(egui::RichText::new(window).font(regular(11.5)).color(tk.text_dim));
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        small(
            ui,
            app,
            "close",
            "",
            "Close the time slider (it stays on; Time Slider opens it again).",
            "ui.timeSlider",
            Value::Null,
            false,
        );
        if small(ui, app, "time_off", "", "Turn the time slider off: every feature shows again.", "time.set", json!({"off": true}), false)
            .clicked()
        {
            app.time.playing = None;
        }
        small(ui, app, "export", "", "Export the steps as an animated GIF.", "ui.timeExport", Value::Null, false);
        ui.separator();
        layers_menu(app, ui);
        // Speed, show, step.
        let speeds = [(2000u32, "0.5×"), (1200, "0.75×"), (800, "1×"), (400, "2×"), (200, "4×")];
        let now = speeds.iter().min_by_key(|(ms, _)| ms.abs_diff(t.speed_ms)).map_or("1×", |(_, l)| *l);
        egui::ComboBox::from_id_salt("time_speed")
            .selected_text(now)
            .width(56.0)
            .show_ui(ui, |ui| {
                for (ms, l) in speeds {
                    if ui.selectable_label(t.speed_ms == ms, l).clicked() {
                        let _ = app.run("time.set", json!({"speed": ms}));
                    }
                }
            })
            .response
            .on_hover_text(tl!("Speed"));
        let looped = t.looping;
        if ui.selectable_label(looped, tl!("Loop")).on_hover_text(tl!("Start over at the end while playing.")).clicked() {
            let _ = app.run("time.set", json!({"loop": !looped}));
        }
        let presets = rustgis_engine::cmd::time::SHOWS;
        let shown = presets.iter().find(|(_, b, a, c)| *b == t.before && *a == t.after && *c == t.cumulative).map_or("custom", |p| p.0);
        egui::ComboBox::from_id_salt("time_show")
            .selected_text(show_label(shown))
            .width(130.0)
            .show_ui(ui, |ui| {
                for (code, ..) in presets {
                    if ui.button(show_label(code)).clicked() {
                        let _ = app.run("time.set", json!({"show": code}));
                    }
                }
            })
            .response
            .on_hover_text(tl!("Show"));
        let input_label = ui.label(egui::RichText::new(tl!("Show")).font(regular(11.5)).color(tk.text_dim));
        // Applied when the drag or the typing ends: one undo step, not one a frame.
        let mut every = app.time.interval.unwrap_or(t.interval);
        let r = ui
            .add(egui::DragValue::new(&mut every).update_while_editing(false).range(1..=1000).speed(0.1))
            .labelled_by(input_label.id)
            .on_hover_text(tl!("Units per step"));
        if crate::widgets::changed(&r) {
            app.time.interval = Some(every);
        }
        let done = r.drag_stopped() || r.lost_focus() || (crate::widgets::changed(&r) && !r.dragged() && !r.has_focus());
        if done && let Some(n) = app.time.interval.take() {
            let _ = app.run("time.set", json!({"interval": n}));
        }
        egui::ComboBox::from_id_salt("time_unit")
            .selected_text(unit_label(t.unit))
            .width(70.0)
            .show_ui(ui, |ui| {
                for u in Unit::ALL {
                    if ui.selectable_label(t.unit == u, unit_label(u)).clicked() {
                        let _ = app.run("time.set", json!({"unit": u.code()}));
                    }
                }
            })
            .response
            .on_hover_text(tl!("Unit"));
        ui.label(egui::RichText::new(tl!("Step")).font(regular(11.5)).color(tk.text_dim));
    });
}

/// Binds and unbinds layers, choosing each one's date field.
fn layers_menu(app: &mut RustGisApp, ui: &mut Ui) {
    let m = app.session.map();
    let n = m.layers.iter().filter(|l| l.time.is_some()).count() + m.tiles.iter().filter(|t| t.time_url.is_some()).count();
    let button = ui.button(crate::i18n::fmt("Layers ({n})", &[("n", &n.to_string())]));
    egui::Popup::menu(&button).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        // Read only while the menu is open: finding date fields reads every field's values.
        let layers: Vec<(u32, String, Option<String>, Vec<(String, f64)>)> = app
            .session
            .map()
            .layers
            .iter()
            .filter(|l| !l.is_raster())
            .map(|l| (l.id, l.name.clone(), l.time.as_ref().map(|t| t.field.clone()), time_fields(&l.dataset)))
            .filter(|(_, _, bound, fields)| bound.is_some() || !fields.is_empty())
            .collect();
        let dated_tiles: Vec<String> = app.session.map().tiles.iter().filter(|t| t.time_url.is_some()).map(|t| t.name.clone()).collect();
        if layers.is_empty() && dated_tiles.is_empty() {
            ui.label(tl!("No layer has a field of dates."));
        }
        for (id, name, bound, fields) in &layers {
            ui.horizontal(|ui| {
                let mut on = bound.is_some();
                if ui.checkbox(&mut on, name.as_str()).changed() {
                    let _ = if on { app.run("time.bind", json!({"layer": id})) } else { app.run("time.unbind", json!({"layer": id})) };
                }
                let current = bound.clone().or_else(|| fields.first().map(|f| f.0.clone())).unwrap_or_default();
                egui::ComboBox::from_id_salt(("time_field", *id))
                    .selected_text(current.as_str())
                    .show_ui(ui, |ui| {
                        for (f, share) in fields {
                            let label =
                                crate::i18n::fmt("{field} ({share}%)", &[("field", f), ("share", &format!("{:.0}", share * 100.0))]);
                            if ui.selectable_label(*f == current, label).clicked() {
                                let _ = app.run("time.bind", json!({"layer": id, "field": f}));
                            }
                        }
                    })
                    .response
                    .on_hover_text(tl!("Field"));
            });
        }
        if !dated_tiles.is_empty() {
            ui.separator();
            for name in &dated_tiles {
                ui.label(crate::i18n::fmt("{name} (dated tiles)", &[("name", name)]));
            }
        }
    });
}

fn transport(app: &mut RustGisApp, ui: &mut Ui, steps: &[f64]) {
    let playing = app.time.playing.is_some();
    small(ui, app, "time_first", "", "Go to the first step.", "time.goTo", json!({"step": 0}), false);
    small(ui, app, "time_prev", "", "Go back a step.", "time.step", json!({"by": -1}), false);
    small(
        ui,
        app,
        if playing { "time_pause" } else { "run" },
        "",
        if playing { "Pause." } else { "Play through the steps." },
        "ui.timePlay",
        Value::Null,
        playing,
    );
    small(ui, app, "time_next", "", "Go forward a step.", "time.step", json!({"by": 1}), false);
    small(ui, app, "time_last", "", "Go to the last step.", "time.goTo", json!({"step": steps.len().saturating_sub(1)}), false);
    ui.add_space(6.0);
}

/// The timeline: a bar per step (its height the features it holds), the window shaded, and the
/// current step marked; click or drag to move.
fn timeline(app: &mut RustGisApp, ui: &mut Ui, t: &TimeSlider, steps: &[f64], cur: usize, tk: &Tokens) {
    let counts = counts(app, t, steps);
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ui.available_height().max(40.0)), Sense::click_and_drag());
    let p = ui.painter_at(r);
    let track = Rect::from_min_max(pos2(r.min.x + 6.0, r.min.y + 2.0), pos2(r.max.x - 6.0, r.max.y - 16.0));
    p.rect_filled(track, 3.0, tk.input);
    p.rect_stroke(track, 3.0, Stroke::new(1.0, tk.input_border), egui::StrokeKind::Inside);
    let n = steps.len().max(1);
    let x_of = |i: f64| track.min.x + (i as f32 + 0.5) / n as f32 * track.width();
    let i_of = |x: f32| (((x - track.min.x) / track.width() * n as f32 - 0.5).round().max(0.0) as usize).min(n - 1);
    // The window, shaded.
    let (from, to) = t.window();
    let first = steps.iter().position(|s| *s >= from).unwrap_or(0);
    let last = steps.iter().rposition(|s| *s < to).unwrap_or(cur);
    if last >= first {
        let a = x_of(first as f64) - track.width() / n as f32 / 2.0;
        let b = x_of(last as f64) + track.width() / n as f32 / 2.0;
        p.rect_filled(Rect::from_min_max(pos2(a.max(track.min.x), track.min.y), pos2(b.min(track.max.x), track.max.y)), 2.0, tk.checked);
    }
    // Feature counts.
    let top = counts.iter().copied().max().unwrap_or(0).max(1) as f32;
    let bw = (track.width() / n as f32 * 0.7).clamp(1.0, 14.0);
    for (i, c) in counts.iter().enumerate().filter(|(_, c)| **c > 0) {
        let x = x_of(i as f64);
        let h = (*c as f32 / top) * (track.height() - 6.0);
        let col = if i == cur { tk.accent } else { tk.accent.gamma_multiply(0.45) };
        p.rect_filled(Rect::from_min_max(pos2(x - bw / 2.0, track.max.y - 3.0 - h), pos2(x + bw / 2.0, track.max.y - 3.0)), 1.0, col);
    }
    // Tick labels: about one per 90 px.
    let every = ((n as f32 * 90.0 / track.width().max(1.0)).ceil() as usize).max(1);
    for (i, s) in steps.iter().enumerate().step_by(every) {
        let x = x_of(i as f64);
        p.line_segment([pos2(x, track.max.y), pos2(x, track.max.y + 3.0)], Stroke::new(1.0, tk.text_dim));
        // Kept inside the bar at its ends.
        let g = p.layout_no_wrap(t.label(*s), regular(10.5), tk.text_dim);
        let left = (x - g.size().x / 2.0).clamp(r.min.x, (r.max.x - g.size().x).max(r.min.x));
        p.galley(pos2(left, track.max.y + 4.0), g, tk.text_dim);
    }
    // The current step.
    let x = x_of(cur as f64);
    p.line_segment([pos2(x, track.min.y - 1.0), pos2(x, track.max.y + 1.0)], Stroke::new(2.0, tk.accent));
    p.circle_filled(pos2(x, track.min.y + 3.0), 4.5, tk.accent);
    p.circle_stroke(pos2(x, track.min.y + 3.0), 4.5, Stroke::new(1.5, Color32::WHITE));
    if let Some(pos) = resp.interact_pointer_pos()
        && (resp.clicked() || resp.dragged())
    {
        let i = i_of(pos.x);
        if i != cur {
            let _ = app.run("time.goTo", json!({"step": i}));
        }
    }
    if let Some(pos) = resp.hover_pos() {
        let i = i_of(pos.x);
        if let Some(s) = steps.get(i) {
            let c = counts.get(i).copied().unwrap_or(0);
            resp.on_hover_text(crate::i18n::fmt("{date}: {n} features", &[("date", &t.label(*s)), ("n", &c.to_string())]));
        }
    }
}
