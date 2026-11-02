//! Window chrome: the header (logo, Quick Access Toolbar, title, command search), the status bar,
//! keyboard shortcuts, the command search palette, About and notifications.

use egui::{Align2, CornerRadius, Key, KeyboardShortcut, Modifiers, Rect, Sense, Stroke, Ui, pos2, vec2};
use serde_json::{Value, json};

use crate::mapview::Toast;
use crate::theme::{Tokens, regular, semibold};
use crate::widgets::{row_button, small, text_w, why_disabled};
use crate::{RustGisApp, icons};

/// The native title bar the header is drawn into (macOS: a transparent title bar over a
/// full-size content view). The app reads it from the window system; `None` means the system
/// draws its own title bar above the header.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TitlebarMetrics {
    /// Width the window buttons (close, minimise, zoom) take from the left edge, in points.
    pub left_inset: f32,
    /// Height of the native title bar, in points; the window buttons are centred in it.
    pub height: f32,
    /// What double-clicking the title bar does, per the system setting.
    pub double_click: DoubleClick,
}

/// The system's "double-click a window's title bar to…" setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DoubleClick {
    Zoom,
    Minimize,
    Nothing,
}

impl TitlebarMetrics {
    /// What to assume before the window system has answered: the usual strip of window buttons.
    pub const FALLBACK: TitlebarMetrics = TitlebarMetrics { left_inset: 80.0, height: HEADER_HEIGHT, double_click: DoubleClick::Zoom };
}

/// The window command for a double-click on the header; `maximized` is the window's current state.
pub fn double_click_command(action: DoubleClick, maximized: bool) -> Option<egui::ViewportCommand> {
    match action {
        DoubleClick::Zoom => Some(egui::ViewportCommand::Maximized(!maximized)),
        DoubleClick::Minimize => Some(egui::ViewportCommand::Minimized(true)),
        DoubleClick::Nothing => None,
    }
}

/// Size and left padding of the header.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeaderLayout {
    pub height: f32,
    pub left_pad: f32,
    /// Space kept free at the bottom so the content centres on the window buttons when the native
    /// title bar is shorter than the header.
    pub bottom_pad: f32,
}

/// The usual header height, and the least a unified one gets; the title bar size is
/// `TitlebarMetrics::height`, which the app falls back to this for.
pub const HEADER_HEIGHT: f32 = 34.0;
const HEADER_PAD: f32 = 10.0;
/// How long the status bar names the MCP relay agent's last call beside the client count.
#[cfg(not(target_arch = "wasm32"))]
const RELAY_ACTION_SHOWN: std::time::Duration = std::time::Duration::from_secs(10);
/// Most a native title bar can add, and the widest strip of window buttons, in points.
const MAX_TITLEBAR_HEIGHT: f32 = 80.0;
const MAX_LEFT_INSET: f32 = 120.0;

pub fn header_layout(titlebar: Option<TitlebarMetrics>) -> HeaderLayout {
    // Whatever the window system reports is clamped; NaN falls back to the plain header.
    let clamp = |v: f32, lo: f32, hi: f32| if v.is_nan() { lo } else { v.clamp(lo, hi) };
    match titlebar {
        None => HeaderLayout { height: HEADER_HEIGHT, left_pad: HEADER_PAD, bottom_pad: 0.0 },
        Some(m) => {
            let native = clamp(m.height, 0.0, MAX_TITLEBAR_HEIGHT);
            let height = native.max(HEADER_HEIGHT);
            HeaderLayout { height, left_pad: clamp(m.left_inset, HEADER_PAD, MAX_LEFT_INSET), bottom_pad: height - native }
        }
    }
}

pub fn header(app: &mut RustGisApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let layout = header_layout(app.ui.titlebar);
    egui::Panel::top("header")
        .exact_size(layout.height)
        .show_separator_line(false)
        .frame(egui::Frame::NONE.fill(t.header).inner_margin(egui::Margin {
            left: layout.left_pad.round() as i8,
            right: HEADER_PAD as i8,
            top: 0,
            bottom: layout.bottom_pad.round() as i8,
        }))
        .show(ui, |ui| {
            let full = ui.max_rect();
            // In a unified title bar the header is the window's title bar: drag it to move the
            // window, double-click to zoom. Registered first so the buttons above it win clicks.
            if app.ui.titlebar.is_some() {
                let bar = Rect::from_min_max(
                    pos2(full.min.x - layout.left_pad, full.min.y),
                    pos2(full.max.x + HEADER_PAD, full.max.y + layout.bottom_pad),
                );
                let drag = ui.interact(bar, egui::Id::unique("header_drag"), Sense::click_and_drag());
                if drag.double_clicked() {
                    let maximized = ui.ctx().input(|i| i.viewport().maximized).unwrap_or(false);
                    let action = app.ui.titlebar.map_or(DoubleClick::Zoom, |m| m.double_click);
                    if let Some(cmd) = double_click_command(action, maximized) {
                        ui.ctx().send_viewport_cmd(cmd);
                    }
                } else if drag.drag_started() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
            }
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing = vec2(2.0, 0.0);
                let (r, _) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::hover());
                app.paint_logo(ui, r);
                ui.add_space(6.0);
                ui.label(egui::RichText::new(tl!("RustGIS")).font(semibold(13.0)).color(t.text));
                ui.add_space(10.0);
                // Quick Access Toolbar.
                small(ui, app, "new", "", "New project", "project.new", Value::Null, false);
                small(ui, app, "open", "", "Open project", "ui.open", Value::Null, false);
                small(ui, app, "save", "", "Save project", "ui.save", Value::Null, false);
                small(
                    ui,
                    app,
                    "undo",
                    "",
                    app.session
                        .undo_label()
                        .map(|l| crate::i18n::fmt("Undo {action}", &[("action", tl_raw!(l))]))
                        .unwrap_or(tl!("Undo").into())
                        .as_str(),
                    "edit.undo",
                    Value::Null,
                    false,
                );
                small(
                    ui,
                    app,
                    "redo",
                    "",
                    app.session
                        .redo_label()
                        .map(|l| crate::i18n::fmt("Redo {action}", &[("action", tl_raw!(l))]))
                        .unwrap_or(tl!("Redo").into())
                        .as_str(),
                    "edit.redo",
                    Value::Null,
                    false,
                );
                for id in app.ui.quick_access.clone() {
                    let label = crate::ribbon::QUICK.iter().find(|q| q.0 == id).map_or(id.as_str(), |q| q.1).to_string();
                    small(ui, app, crate::ribbon::quick_icon(&id), "", &label, &id, Value::Null, false);
                }
                // Everything right of the Quick Access Toolbar is laid out by measuring, so on a
                // narrow window the search box shrinks to an icon and the title shortens or hides
                // instead of overlapping other controls.
                let qat_end = ui.cursor().min.x + 8.0;
                // Command search on the right: full width when there is room, a round icon button
                // otherwise. It never reaches into the toolbar.
                let label = tl!("Command Search");
                // As egui reads the keys: ⌘ on a Mac, the browser's too (from its user agent).
                let hint = if ui.ctx().os().is_mac() { "⌘K" } else { "Ctrl+K" };
                let right = full.max.x;
                let room = right - qat_end;
                let full_w = (text_w(ui, label, regular(12.0)) + text_w(ui, hint, regular(11.0)) + 70.0).clamp(200.0, 260.0);
                // Room kept for the title next to the search box.
                let reserve = (text_w(ui, &app.session.project.name, semibold(12.5)) + 16.0).min(160.0);
                let sw = if room >= full_w + reserve + 24.0 {
                    full_w
                } else if room >= 150.0 + reserve {
                    (room - reserve).clamp(150.0, full_w)
                } else {
                    26.0
                };
                let sr = Rect::from_min_max(pos2(right - sw, full.center().y - 12.0), pos2(right, full.center().y + 12.0));
                let resp = ui.interact(sr, egui::Id::unique("command_search_box"), Sense::click());
                ui.painter().rect_filled(sr, CornerRadius::same(12), if resp.hovered() { t.hover } else { t.input });
                ui.painter().rect_stroke(sr, CornerRadius::same(12), Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
                let compact = sw < 100.0;
                let icon_x = if compact { sr.center().x } else { sr.min.x + 16.0 };
                icons::paint(
                    ui.painter(),
                    Rect::from_center_size(pos2(icon_x, sr.center().y), vec2(14.0, 14.0)),
                    "search",
                    t.text_dim,
                    t.accent,
                );
                if !compact {
                    let show_hint = sw >= full_w - 1.0;
                    let text_room = sr.width() - 40.0 - if show_hint { text_w(ui, hint, regular(11.0)) + 12.0 } else { 0.0 };
                    if text_w(ui, label, regular(12.0)) <= text_room {
                        ui.painter().text(pos2(sr.min.x + 30.0, sr.center().y), Align2::LEFT_CENTER, label, regular(12.0), t.text_dim);
                    }
                    if show_hint {
                        ui.painter().text(pos2(sr.max.x - 12.0, sr.center().y), Align2::RIGHT_CENTER, hint, regular(11.0), t.text_disabled);
                    }
                }
                let resp = if compact { resp.on_hover_text(format!("{label} ({hint})")) } else { resp };
                if resp.clicked() {
                    app.ui.command_search = Some(String::new());
                }
                // Title: centred when it fits, otherwise in the free gap; the map name and then the
                // project name drop out when there is no room.
                let title = format!("{}{}", app.session.project.name, if app.session.dirty() { " •" } else { "" });
                let sub = format!("  —  {}", app.session.map().name);
                let (title_w, sub_w) = (text_w(ui, &title, semibold(12.5)), text_w(ui, &sub, regular(12.0)));
                let (a, b) = (qat_end + 8.0, sr.min.x - 16.0);
                if b > a {
                    let with_sub = title_w + sub_w <= b - a;
                    let tw = if with_sub { title_w + sub_w } else { title_w };
                    if tw <= b - a {
                        let cx = (full.center().x - tw / 2.0).min(b - tw).max(a);
                        ui.painter().text(pos2(cx, full.center().y), Align2::LEFT_CENTER, &title, semibold(12.5), t.text);
                        if with_sub {
                            ui.painter().text(pos2(cx + title_w, full.center().y), Align2::LEFT_CENTER, &sub, regular(12.0), t.text_dim);
                        }
                    }
                }
            });
        });
}

pub fn status_bar(app: &mut RustGisApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    egui::Panel::bottom("status")
        .exact_size(26.0)
        .show_separator_line(false)
        .frame(egui::Frame::NONE.fill(t.status_bar).inner_margin(egui::Margin { left: 10, right: 10, top: 0, bottom: 0 }))
        .show(ui, |ui| {
            let r = ui.max_rect();
            ui.painter().line_segment([pos2(r.min.x - 10.0, r.min.y), pos2(r.max.x + 10.0, r.min.y)], Stroke::new(1.0, t.border));
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing = vec2(10.0, 0.0);
                // Scale: typed (1:12,345, 12345, 25k, …) and set with Enter, or picked from the
                // list. Esc, or clicking away, leaves the map's scale as it was.
                let scale = app.session.scale();
                let shown = rustgis_engine::map::graphics::scale_ratio(scale);
                let mut text = app.ui.scale_text.take().unwrap_or_else(|| shown.clone());
                let mut edit = egui::TextEdit::singleline(&mut text).id_salt("status_scale").desired_width(96.0).show(ui);
                // Clicking in selects the scale, so typing replaces it.
                if edit.response.gained_focus() {
                    let end = edit.galley.text().chars().count();
                    edit.state.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(end))));
                    edit.state.store(ui.ctx(), edit.response.id);
                }
                let r = edit.response.response.on_hover_text(tl!("Type a scale, such as 1:12,345, 12345 or 25k, and press Enter"));
                // The box's own focus, not the window's: switching windows keeps a half-typed scale.
                if ui.ctx().memory(|m| m.has_focus(r.id)) {
                    app.ui.scale_text = Some(text);
                } else if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && text != shown {
                    match parse_scale(&text) {
                        Some(s) => {
                            let _ = app.run("map.setScale", json!({"scale": s}));
                        }
                        None => app.session.notify(crate::i18n::fmt("'{text}' isn't a scale: type one such as 1:12,345", &[("text", &text)]), true),
                    }
                }
                let presets = ui.add(egui::Button::new("▾").small().frame(false)).on_hover_text(tl!("Common scales"));
                egui::Popup::menu(&presets).show(|ui| {
                    for s in [
                        5_000.0,
                        25_000.0,
                        100_000.0,
                        500_000.0,
                        1_000_000.0,
                        5_000_000.0,
                        10_000_000.0,
                        25_000_000.0,
                        50_000_000.0,
                        100_000_000.0,
                    ] {
                        if ui.selectable_label(false, format!("1:{}", crate::ribbon::fmt_thousands(s))).clicked() {
                            let _ = app.run("map.setScale", json!({"scale": s}));
                            ui.close();
                        }
                    }
                });
                // The map's zoom level, bearing and pitch; clicking them on a turned or tilted map
                // faces north and looks straight down again.
                let cam = app.session.map().camera;
                let zoom = app.session.zoom().map_or_else(|| "—".to_string(), |z| format!("{z:.1}"));
                let text = crate::i18n::fmt(
                    "Zoom {z} · Bearing {b}° · Pitch {p}°",
                    // Rounded first, then + 0.0: a bearing just below zero reads 0, not -0.
                    &[
                        ("z", &zoom),
                        ("b", &format!("{:.0}", cam.bearing.round() + 0.0)),
                        ("p", &format!("{:.0}", cam.pitch.round() + 0.0)),
                    ],
                );
                let level = cam.bearing.round() == 0.0 && cam.pitch.round() == 0.0;
                let resp = ui.add(
                    egui::Label::new(egui::RichText::new(text).monospace().size(11.5).color(if level { t.text_dim } else { t.accent_text }))
                        .sense(Sense::click()),
                );
                let resp = resp.on_hover_text(tl!(
                    "Zoom: the map's zoom level, as web maps count it. Bearing: the direction the top of the map faces, clockwise from north. Pitch: the tilt from looking straight down. Click to reset bearing and pitch."
                ));
                if resp.clicked() && !level {
                    let _ = app.run("map.resetNorth", json!({}));
                }
                sep(ui, &t);
                // Pointer coordinates.
                let coord = app
                    .map
                    .pointer_lonlat
                    .map(|c| if app.map.dms { rustgis_engine::geom::format_dms(c) } else { rustgis_engine::geom::format_lonlat(c) });
                let resp = ui.add(
                    egui::Label::new(egui::RichText::new(coord.unwrap_or_else(|| "—".into())).monospace().size(11.5).color(t.text_dim))
                        .sense(Sense::click()),
                );
                if resp.on_hover_text(tl!("Click to switch between decimal degrees and DMS")).clicked() {
                    app.map.dms = !app.map.dms;
                }
                let projection = match &app.session.map().projection {
                    rustgis_engine::map::Projection::Globe => tl!("WGS 84 / Globe").to_string(),
                    rustgis_engine::map::Projection::WebMercator => tl!("WGS 84 / Web Mercator").to_string(),
                    p => tl!(p.label().as_str()).to_string(),
                };
                let resp = ui.add(egui::Label::new(egui::RichText::new(projection).size(11.0).color(t.text_dim)).sense(Sense::click()));
                if resp.on_hover_text(tl!("Change the map's coordinate system")).clicked() {
                    let _ = app.run("ui.dialog", json!({"name": "coordinateSystem"}));
                }
                sep(ui, &t);
                let n = app.session.map().selection_count();
                ui.label(
                    egui::RichText::new(crate::i18n::fmt("Selected Features: {n}", &[("n", &n.to_string())])).size(11.5).color(if n > 0 {
                        t.accent_text
                    } else {
                        t.text_dim
                    }),
                );
                if n > 0 {
                    let (r, resp) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
                    icons::paint(ui.painter(), r, "clear_sel", t.icon, t.accent);
                    if resp.on_hover_text(tl!("Clear selection")).clicked() {
                        let _ = app.run("select.clear", Value::Null);
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // The CPU/GPU backend toggle, then the frame rate (live while the map moves,
                    // "— fps" when idle), then the last render's cost.
                    let gpu = app.map.gpu_enabled;
                    let resp = ui.add(
                        egui::Label::new(egui::RichText::new(if gpu { "GPU" } else { "CPU" }).size(11.0).monospace().color(if gpu {
                            t.accent_text
                        } else {
                            t.text_dim
                        }))
                        .sense(Sense::click()),
                    );
                    if resp.on_hover_text(tl!("Switch between GPU and CPU map rendering")).clicked() {
                        let _ = app.run("ui.map.gpu", Value::Null);
                    }
                    sep(ui, &t);
                    let fps = app.map.fps(ui.input(|i| i.time)).map(|n| format!("{n} fps")).unwrap_or_else(|| "— fps".into());
                    ui.label(egui::RichText::new(fps).size(11.0).monospace().color(t.text_disabled))
                        .on_hover_text(tl!("Map frames per second (live while the map moves)"));
                    sep(ui, &t);
                    ui.label(egui::RichText::new(format!("{} ms", app.map.render_ms)).size(11.0).color(t.text_disabled))
                        .on_hover_text(tl!("Last map render time"));
                    sep(ui, &t);
                    // The MCP relay: while it runs, an attached agent works in this session, so
                    // its authenticated client count is shown and a click stops it. Desktop only.
                    #[cfg(not(target_arch = "wasm32"))]
                    {
                        if let Some((n, last, full)) = app.relay.as_ref().map(|r| (r.attached_clients(), r.last_action(), r.full_access())) {
                            // Full access is shown apart: agents may run anything then.
                            let (mut text, mut hover, mut color) = if full {
                                (
                                    crate::i18n::fmt("MCP relay · full access · {n} attached", &[("n", &n.to_string())]),
                                    tl!("Agents have full access: they may run scripts, tasks, models and plugins. Click to stop the MCP relay")
                                        .to_string(),
                                    t.red,
                                )
                            } else {
                                (
                                    crate::i18n::fmt("MCP relay · {n} attached", &[("n", &n.to_string())]),
                                    tl!("Stop MCP relay").to_string(),
                                    if n > 0 { t.accent_text } else { t.text_dim },
                                )
                            };
                            // What the agent just ran, while the user watches it work: for a few
                            // seconds beside the count, and on hover until the next call.
                            if let Some(a) = last {
                                let ago = a.at.elapsed();
                                if ago < RELAY_ACTION_SHOWN {
                                    text = format!("{text} · {}", a.label);
                                    if !a.ok {
                                        color = t.red;
                                    }
                                    ui.ctx().request_repaint_after(RELAY_ACTION_SHOWN.saturating_sub(ago));
                                }
                                let said = if a.ok { "Last agent call: {call} ({secs} s ago)" } else { "Last agent call failed: {call} ({secs} s ago)" };
                                hover = format!(
                                    "{hover}\n{}",
                                    crate::i18n::fmt(said, &[("call", &a.detail), ("secs", &ago.as_secs().to_string())])
                                );
                            }
                            let resp = ui.add(egui::Label::new(egui::RichText::new(text).size(11.5).color(color)).sense(Sense::click()));
                            if resp.on_hover_text(hover).clicked() {
                                let _ = app.run("ui.mcpRelay", json!({"stop": true}));
                            }
                            sep(ui, &t);
                        } else if app.file_bridge.is_some() {
                            // Agents on with the relay down (its port taken): the file bridge alone.
                            let resp = ui.add(
                                egui::Label::new(egui::RichText::new(tl!("File bridge only")).size(11.5).color(t.amber)).sense(Sense::click()),
                            );
                            let hover = tl!("The MCP relay couldn't start; agents can still use the file bridge. Click to turn agents off");
                            if resp.on_hover_text(hover).clicked() {
                                let _ = app.run("ui.mcpRelay", json!({"stop": true}));
                            }
                            sep(ui, &t);
                        }
                    }
                    if ui.small_button(tl!("Diagnostics")).on_hover_text(tl!("Review messages and failed network requests")).clicked() {
                        app.ui.open_right(crate::RightPane::DiagnosticLog);
                    }
                    // Cut to the room left (the whole notice on hover), so it doesn't run
                    // over the selection count and coordinates. A click opens its history.
                    if let Some(n) = app.session.notices.last()
                        && ui.add(
                            egui::Button::new(egui::RichText::new(&n.text).size(11.5).color(if n.error { t.red } else { t.text_dim }))
                                .frame(false)
                                .small()
                                .truncate(),
                        )
                        .on_hover_text(crate::i18n::fmt("{message}\n{open}", &[("message", &n.text), ("open", tl_raw!("Open Diagnostics"))]))
                        .clicked() {
                        app.ui.open_right(crate::RightPane::DiagnosticLog);
                    }
                });
            });
        });
}

fn sep(ui: &mut Ui, t: &Tokens) {
    let (r, _) = ui.allocate_exact_size(vec2(1.0, 14.0), Sense::hover());
    ui.painter().rect_filled(r, 0.0, t.border_strong);
}

/// A map scale typed into the status bar, as its denominator: `1:12,345`, `1/12345`, `12345`,
/// `12 345`, `12'345`, `12.345` (dots grouping thousands, as many languages write it), `25k` or
/// `2.5m`, or larger than life as `2:1` (#902). None unless it's a plain number of at least 1: no
/// exponents, and commas or apostrophes only between groups of three digits (`1:2,5m` is a typo,
/// not 25 million).
fn parse_scale(text: &str) -> Option<f64> {
    let t: String = text.to_lowercase().chars().filter(|c| !c.is_whitespace()).collect();
    if let Some(up) = t.strip_suffix(":1").or_else(|| t.strip_suffix("/1")).filter(|u| !u.contains([':', '/'])) {
        // Times life size, as the status bar writes it: a dot is a decimal point (`1.5:1`), commas
        // group thousands (`2,000:1`), and there are no `k`/`m` suffixes.
        let times = if up.contains('.') {
            up.bytes().all(|b| b.is_ascii_digit() || b == b'.').then(|| up.parse::<f64>().ok()).flatten()?
        } else if up.ends_with(['k', 'm']) {
            return None;
        } else {
            parse_scale(up)?
        };
        return (times.is_finite() && times > 0.0).then(|| 1.0 / times);
    }
    let chars: Vec<char> = t.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if matches!(c, ',' | '\'' | '’') {
            let before = i.checked_sub(1).and_then(|j| chars.get(j)).is_some_and(|c| c.is_ascii_digit());
            let group = chars.get(i + 1..i + 4).is_some_and(|g| g.iter().all(|c| c.is_ascii_digit()));
            if !before || !group || chars.get(i + 4).is_some_and(|c| c.is_ascii_digit()) {
                return None;
            }
        }
    }
    let t: String = t.chars().filter(|c| !matches!(c, ',' | '_' | '\'' | '’')).collect();
    let t = t.strip_prefix("1:").or_else(|| t.strip_prefix("1/")).unwrap_or(&t);
    let (num, unit) = match (t.strip_suffix('k'), t.strip_suffix('m')) {
        (Some(n), _) => (n, 1e3),
        (_, Some(n)) => (n, 1e6),
        _ => (t, 1.0),
    };
    let mut groups = num.split('.');
    let grouped = unit == 1.0
        && groups.next().is_some_and(|g| (1..=3).contains(&g.len()))
        && num.contains('.')
        && groups.all(|g| g.len() == 3 && g.bytes().all(|b| b.is_ascii_digit()));
    let num = if grouped { num.replace('.', "") } else { num.to_string() };
    if !num.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return None;
    }
    let v = num.parse::<f64>().ok()? * unit;
    (v.is_finite() && v >= 1.0).then_some(v)
}

/// Global keyboard shortcuts.
pub fn shortcuts(app: &mut RustGisApp, ctx: &egui::Context) {
    // Behind a dialog (Save changes?, Recover unsaved work?, Command Search…) the keys are the
    // dialog's: Escape answers it rather than switching tools, and Ctrl+N can't start what it
    // asks about.
    if ctx.memory(|m| m.top_modal_layer()).is_some() || app.ui.shortcut_editor.capturing() {
        return;
    }
    crate::modelbuilder::shortcuts(app, ctx);
    // The search box (or a table cell) was typed in until egui took its focus away on Escape:
    // that Escape clears the box (leaves the cell as it was), not the tool.
    let pass = ctx.cumulative_pass_nr();
    let typing =
        ctx.egui_wants_keyboard_input() || crate::searchbox::had_focus(&app.ui.search, pass) || crate::table::had_focus(&app.table, pass);
    // A text field focused, not any widget (a button reached with Tab): it has an undo of its own.
    let text_field = ctx.text_edit_focused();
    // An element selected in the open layout, for the layout's own keys.
    let element = app.session.active_layout.is_some() && app.layout.selected.is_some();
    let mut run: Vec<(String, Value)> = Vec::new();
    let keymap = &app.ui.keymap;
    ctx.input_mut(|i| {
        for b in keymap.match_order() {
            // While a text field has focus, it keeps the keys it acts on: those without Ctrl (⌘),
            // which type text, and undo and redo, whatever they're bound to (#685), as ModelBuilder
            // leaves them.
            // Pause unmodified shortcuts during first-person movement, retaining the existing Escape, Home and Delete controls.
            if (typing && !b.chord.command)
                || (text_field && b.chord.is_text_undo())
                || (app.session.map().view.first_person
                    && !b.chord.command
                    && !b.chord.alt
                    && !b.chord.shift
                    && !matches!(b.chord.key, Key::Escape | Key::Home | Key::Delete))
            {
                continue;
            }
            // Duplicating needs a selected layout element, and not a text field being typed in;
            // otherwise the key is left alone.
            if b.command == "layout.duplicateElement" && (!element || typing) {
                continue;
            }
            // With a layout element selected, the arrows nudge it (below), whatever else they run;
            // the nudge never takes Ctrl (⌘), so a shortcut with it still runs.
            if element && !b.chord.command && matches!(b.chord.key, Key::ArrowLeft | Key::ArrowRight | Key::ArrowUp | Key::ArrowDown) {
                continue;
            }
            if i.consume_shortcut(&KeyboardShortcut::new(b.chord.modifiers(), b.chord.key)) {
                run.push((b.command.clone(), b.params.clone()));
            }
        }
        if !typing {
            // A Mac keyboard's delete key is Backspace: it deletes a layout element too.
            if element && i.consume_key(Modifiers::NONE, Key::Backspace) {
                run.push(("edit.delete".into(), Value::Null));
            }
            // Arrow keys nudge the selected layout element, further with Shift; held, it keeps
            // going. Shift first: a plain arrow's pattern takes Shift-arrows too.
            if element {
                use crate::layoutview::{NUDGE_FAST_MM, NUDGE_MM};
                for (key, dx, dy) in
                    [(Key::ArrowLeft, -1.0, 0.0), (Key::ArrowRight, 1.0, 0.0), (Key::ArrowUp, 0.0, -1.0), (Key::ArrowDown, 0.0, 1.0)]
                {
                    for (m, step) in [(Modifiers::SHIFT, NUDGE_FAST_MM), (Modifiers::NONE, NUDGE_MM)] {
                        for _ in 0..i.count_and_consume_key(m, key) {
                            run.push(("ui.layoutNudge".into(), json!({"dx": dx * step, "dy": dy * step})));
                        }
                    }
                }
            }
            // Clipboard keys arrive as events, not key presses.
            for e in &i.events {
                match e {
                    egui::Event::Copy => run.push(("ui.copy".into(), Value::Null)),
                    egui::Event::Cut => run.push(("ui.cut".into(), Value::Null)),
                    egui::Event::Paste(text) => run.push(("edit.paste".into(), json!({"text": text}))),
                    _ => {}
                }
            }
        }
    });
    for (id, p) in &run {
        let (id, p) = (id.as_str(), p.clone());
        if id == "tool.set" {
            app.map.cancel_sketch();
            app.layout.cancel_tool();
        }
        // An arrow key nudged the selected layout element (unless it's locked): a held key, or
        // taps close together, are one step to undo.
        if id == "ui.layoutNudge" {
            let (dx, dy) = (p["dx"].as_f64().unwrap_or(0.0), p["dy"].as_f64().unwrap_or(0.0));
            if let (Some(li), Some(el)) = (app.session.active_layout, app.layout.selected)
                && let Some((x, y)) =
                    app.session.project.layouts.get(li).and_then(|l| l.element(el)).filter(|e| !e.locked).map(|e| (e.x + dx, e.y + dy))
            {
                let key = app.layout.nudge_key(li, el, ctx.input(|i| i.time));
                app.session.merge_undo(key);
                let _ = app.run("layout.update", json!({"layout": li, "element": el, "x": x, "y": y}));
            }
            continue;
        }
        // Ctrl+D (Cmd+D on a Mac) duplicates the selected layout element and selects the copy.
        if id == "layout.duplicateElement" {
            if let (Some(li), Some(el)) = (app.session.active_layout, app.layout.selected)
                && let Ok(v) = app.run(id, json!({"layout": li, "element": el}))
            {
                app.layout.selected = v["element"].as_u64().and_then(|v| u32::try_from(v).ok());
            }
            continue;
        }
        // With Edit Vertices, Delete removes the chosen vertex rather than the feature.
        if id == "edit.delete"
            && app.session.active_layout.is_none()
            && app.session.tool == rustgis_engine::Tool::EditVertices
            && crate::vertexedit::delete_chosen(app)
        {
            continue;
        }
        // In a layout, Delete removes the selected element (and never the map's hidden features).
        if id == "edit.delete"
            && let Some(li) = app.session.active_layout
        {
            // A locked element (or a frame with a locked surround) stays, and says why (#536).
            let kept = app
                .layout
                .selected
                .is_some_and(|el| app.session.project.layouts.get(li).is_some_and(|l| l.locked_in_removal(el).is_some()));
            if kept {
                let text = crate::i18n::fmt(
                    "{command}: {reason}",
                    &[("command", tl_raw!("Delete Element")), ("reason", crate::layoutview::locked_delete())],
                );
                if !app.map.toasts.iter().any(|t| t.text == text) {
                    app.session.notify(text, true);
                }
            } else if let Some(el) = app.layout.selected.take() {
                let _ = app.run("layout.removeElement", json!({"layout": li, "element": el}));
            }
            continue;
        }
        if matches!(id, "edit.delete" | "ui.copy" | "ui.cut") && app.session.map().selection_count() == 0 {
            continue;
        }
        // Pasting text that isn't features (a stray copy from elsewhere) does nothing.
        if id == "edit.paste" && !p.get("text").and_then(Value::as_str).is_some_and(|t| t.trim_start().starts_with('{')) {
            continue;
        }
        // Exhausted undo/redo shortcuts are inert, like their disabled toolbar buttons.
        // Do not dispatch them: a failed command is retained by Diagnostics.
        if (id == "edit.undo" && !app.session.can_undo()) || (id == "edit.redo" && !app.session.can_redo()) {
            continue;
        }
        // A command that can't run now says why, as its greyed ribbon button does (#456).
        // Running out of undo or redo needs no word.
        if !matches!(id, "edit.undo" | "edit.redo")
            && let Some(why) = why_disabled(app, id)
        {
            let label = app.session.registry.get(id).map_or(id, |s| s.label);
            let text = crate::i18n::fmt("{command}: {reason}", &[("command", tl_raw!(label)), ("reason", tl_raw!(why))]);
            // Once while it shows: a held key repeats, and each repeat comes here.
            if !app.map.toasts.iter().any(|t| t.text == text) {
                app.session.notify(text, true);
            }
            continue;
        }
        let _ = app.run_by_user(id, p);
    }
}

/// Whether a UI command is available on the current platform.
pub(crate) fn command_available(id: &str, web: bool) -> bool {
    !web || !matches!(id, "ui.checkUpdate" | "ui.mcpRelay")
}

/// The `ui.*` commands Command Search lists: (id, label, ribbon location).
pub(crate) const UI_COMMANDS: &[(&str, &str, &str)] = &[
    ("ui.shortcuts.import", "Import Keyboard Shortcuts…", "Settings › Customize › Keyboard Shortcuts"),
    ("ui.shortcuts.export", "Export Keyboard Shortcuts…", "Settings › Customize › Keyboard Shortcuts"),
    ("ui.customizeShortcuts", "Keyboard Shortcuts", "Settings › Customize"),
    ("ui.addData", "Add Data…", "Home › Add"),
    ("ui.addDelimited", "Add Delimited Text…", "Home › Add › Add Data"),
    ("ui.addUrl", "Add Data From URL…", "Home › Add › Add Data"),
    ("ui.addZarr", "Add Zarr Variable…", "Home › Add › Add Data"),
    ("ui.addService", "Add From Service…", "Home › Add › Add Data"),
    ("ui.searchOpenData", "Search Open Data…", "Home › Add › Web Data"),
    ("ui.stac.planet", "Planet Open Data…", "Home › Add › Web Data"),
    ("ui.stac.portolan", "Portolan…", "Home › Add › Web Data"),
    ("ui.stac.sentinel2", "Sentinel-2 Explorer…", "Home › Add › Web Data"),
    ("ui.stac.vantor", "Vantor…", "Home › Add › Web Data"),
    ("ui.stac.usgsLidar", "USGS LiDAR…", "Home › Add › Web Data › United States"),
    ("ui.pane.sources", "Data Sources", "Home › Add"),
    ("ui.plugins", "Plugin Manager…", "Plugins"),
    ("ui.mcpRelay", "MCP Relay", "Help › Help"),
    ("ui.pane.geoprocessing", "Tools Pane", "View › Windows"),
    ("ui.pane.symbology", "Symbology Pane", "Style › Drawing"),
    ("ui.pane.labeling", "Labeling Pane", "View › Windows"),
    ("ui.pane.diagnosticLog", "Diagnostics", "View › Windows"),
    ("ui.pane.assistant", "AI Assistant", "Analyze › AI"),
    ("ui.pane.sql", "SQL Pane", "Analyze › Workflows"),
    ("ui.pane.directions", "Directions", "Home › Navigate"),
    ("ui.pane.bookmarks", "Bookmarks Pane", "Home › Navigate › Bookmarks"),
    ("ui.theme", "Toggle Dark Mode", "View"),
    ("ui.map.gpu", "Toggle GPU Rendering", "View"),
    ("ui.importShapefile", "Import Shapefile with Projection…", "Home › Add"),
    ("ui.measurementUnits", "Toggle Imperial Measurements", "Home › Inspect › Measure"),
    ("ui.moveSharedVertices", "Shared Vertices", "Edit › Features"),
    ("ui.exportMap", "Export Map…", "Publish › Output"),
    ("ui.checkUpdate", "Check for Updates…", "Help › Help"),
];

/// Command Search's interactive entry points. These collect input without changing the engine API.
#[derive(Clone, Copy)]
enum SearchAction {
    Command(&'static str),
    Dialog(&'static str),
    OpenItem(&'static str),
    SaveItem(&'static str),
    Pane(crate::RightPane),
    Workflow(&'static str),
}

/// Routes commands requiring user input to their dialogs rather than executing empty params.
fn search_action(id: &str) -> Option<SearchAction> {
    use crate::RightPane;
    use SearchAction::{Command, Dialog, OpenItem, Pane, SaveItem, Workflow};
    Some(match id {
        "project.open" => Command("ui.open"),
        "project.save" => Command("ui.save"),
        "project.saveAs" => Command("ui.saveAs"),
        "project.addFolder" => Command("ui.addFolder"),
        "project.fromTemplate" => OpenItem("template"),
        "project.saveTemplate" | "project.newTemplate" => SaveItem("template"),
        "project.package" => SaveItem("projectPackage"),
        "map.import" => OpenItem("map"),
        "map.save" => SaveItem("map"),
        "map.package" => SaveItem("mapPackage"),
        "map.rename" => Dialog("renameMap"),
        "map.goToXY" => Dialog("goToXY"),
        "map.findPlace" => Dialog("findPlace"),
        "map.camera" | "map.setScale" => Dialog("camera"),
        "map.basemap" => Dialog("basemap"),
        "map.body" => Dialog("body"),
        "map.addTiles" => Dialog("addTiles"),
        "map.repairTiles" => Dialog("repairTiles"),
        "bookmark.add" => Dialog("bookmark"),
        "bookmark.import" => OpenItem("bookmarks"),
        "bookmark.save" => SaveItem("bookmarks"),
        "layer.addData" | "layer.addPhotos" => Command("ui.addData"),
        "layer.addUrl" => Command("ui.addUrl"),
        "service.add" | "service.import" => Command("ui.addService"),
        "connection.new" => Command("ui.connection"),
        "connection.resolveS3" => Command("ui.s3Connections"),
        "layer.addSql" => Pane(RightPane::Sql),
        "layer.addDatabase" => Dialog("addDatabase"),
        // Add From Database with Load for editing ticked.
        "pg.connect" | "pg.addLayer" => Dialog("pgAddLayer"),
        "pg.saveEdits" => Dialog("pgSave"),
        "pg.discardEdits" => Dialog("pgDiscard"),
        "gpkg.addLayer" => Dialog("gpkgAddLayer"),
        "gpkg.saveEdits" => Dialog("gpkgSave"),
        "gpkg.discardEdits" => Dialog("gpkgDiscard"),
        "layer.addZarr" => Command("ui.addZarr"),
        "layer.addLayerFile" => OpenItem("layer"),
        "layer.save" => SaveItem("layer"),
        "layer.addSample" => Dialog("addSample"),
        "layer.template" => Dialog("layerTemplate"),
        "layer.addEncodedPolyline" => Dialog("addPolyline"),
        "layer.geocode" => Dialog("geocode"),
        "layer.rename" => Dialog("renameLayer"),
        "layer.defineProjection" => Dialog("defineProjection"),
        "layer.repairSource" => Dialog("repairSource"),
        "ui.importShapefile" => Dialog("addShapefile"),
        "layer.definitionQuery" => Dialog("definitionQuery"),
        "layer.symbology" | "raster.symbology" | "layer.color" | "layer.outline" | "layer.lineStrokes" | "layer.fillLayers"
        | "layer.ramp" => Pane(RightPane::Symbology),
        "select.byAttributes" => Dialog("selectByAttributes"),
        "select.rows" => Command("table.open"),
        "edit.addField" => Dialog("addField"),
        "edit.calculateField" => Dialog("calculateField"),
        "edit.move" | "edit.rotate" | "edit.scale" | "edit.template" | "edit.elevation" | "edit.groundToGrid" => Pane(RightPane::Editing),
        "data.domain" | "data.subtypes" | "data.attributeRule" | "data.contingentValues" | "data.relate" | "data.archiveRestore" => {
            Pane(RightPane::Design)
        }
        "style.add" => OpenItem("styles"),
        "style.import" => OpenItem("importStyles"),
        "style.importQgis" => OpenItem("qgisStyle"),
        "project.importGeolibre" => OpenItem("geolibreProject"),
        "project.importQgis" => OpenItem("qgisProject"),
        "style.save" => SaveItem("styles"),
        "ramp.add" => OpenItem("ramps"),
        "ramp.save" => SaveItem("ramps"),
        "layout.import" => Command("ui.importLayout"),
        "layout.save" => Command("ui.saveLayout"),
        "layout.export" => Command("ui.exportLayout"),
        "layout.rename" => Dialog("renameLayout"),
        "report.open" => OpenItem("report"),
        "report.export" => Command("ui.exportReport"),
        "report.save" => SaveItem("report"),
        "report.saveTemplate" => SaveItem("reportTemplate"),
        "report.existingTemplate" => Command("ui.reportLayoutFile"),
        "presentation.import" => OpenItem("presentation"),
        "presentation.export" => Command("ui.exportPresentation"),
        "presentation.save" => SaveItem("presentation"),
        "presentation.imagePage" => Command("ui.presentationPicture"),
        "presentation.videoPage" => Command("ui.presentationVideo"),
        "notebook.open" => OpenItem("notebook"),
        "notebook.save" => SaveItem("notebook"),
        "task.import" => OpenItem("task"),
        "task.save" => SaveItem("task"),
        "share.exportMap" => Command("ui.exportMap"),
        "share.exportLayer" => Command("ui.exportLayer"),
        "share.exportRaster" => Command("ui.exportRaster"),
        "gp.run" => Command("ui.gp.open"),
        "gp.package" => SaveItem("gpPackage"),
        "gp.env" => Workflow("environments"),
        "raster.function" => Workflow("rasterFunctions"),
        "raster.functionChain" => Workflow("functionEditor"),
        "raster.georeference" => Workflow("georeference"),
        "raster.change" => Workflow("change"),
        "raster.spectral" => Workflow("spectral"),
        "raster.classify" => Workflow("classify"),
        "raster.classifySupervised" => Workflow("supervised"),
        "raster.index" => Workflow("indices"),
        "raster.process" => Workflow("process"),
        "raster.editPixels" => Workflow("pixels"),
        "raster.measure" => Workflow("mensuration"),
        "analysis.suitability" => Workflow("suitability"),
        "analysis.viewshed" => Workflow("visibility"),
        "analysis.lineOfSight" => Workflow("lineOfSight"),
        "analysis.interpolate" => Workflow("interpolate"),
        "analysis.route" => Workflow("network"),
        "analysis.neighborhood" => Workflow("neighborhood"),
        "analysis.simulate" => Workflow("simulation"),
        "data.profile" => Workflow("profile"),
        "data.explore" => Workflow("explore"),
        "data.similar" => Workflow("similar"),
        "data.convert" => Workflow("interop"),
        "dl.classify" => Workflow("deepLearning"),
        "dl.train" => Workflow("deepLearning"),
        "dl.import" => OpenItem("dlPackage"),
        "dl.package" => SaveItem("dlPackage"),
        "imagery.hostedImagery" => Workflow("hosted"),
        "imagery.workspace" | "imagery.mosaic" => Workflow("ortho"),
        "imagery.mesh" => Workflow("reality"),
        "model.run" => Workflow("models"),
        "model.import" => Command("ui.importModel"),
        "osm.download" => Dialog("osm"),
        "overture.add" => Dialog("overture"),
        "boundaries.add" => Dialog("boundaries"),
        "embeddings.add" | "embeddings.search" => Dialog("embeddings"),
        "cube.catalog" | "cube.open" => Dialog("dataCubes"),
        "nationalMap.add" => Dialog("nationalMap"),
        "fieldsOfTheWorld.add" => Dialog("fieldsOfTheWorld"),
        "portal.add" | "portal.search" => Command("ui.searchOpenData"),
        "stac.addAsset" | "stac.addFootprints" | "stac.search" => Command("ui.stac"),
        _ => return None,
    })
}

/// Desktop-only interactive actions stay visible but disabled in web Command Search.
fn search_platform_disabled(id: &str, web: bool) -> Option<&'static str> {
    if !web {
        return None;
    }
    match id {
        "project.addFolder" => Some("folder connections need the desktop app"),
        "layer.addDatabase" => Some("databases are read through DuckDB, a program the browser can't run"),
        "pg.connect" | "pg.addLayer" | "pg.saveEdits" | "pg.discardEdits" => Some("PostGIS tables are edited in the desktop app"),
        "gpkg.addLayer" | "gpkg.saveEdits" | "gpkg.discardEdits" => Some("GeoPackage tables are edited in the desktop app"),
        _ => None,
    }
}

/// Availability of the interactive entry point, not the parameterized engine operation.
fn search_disabled(app: &RustGisApp, id: &str) -> Option<&'static str> {
    if id.starts_with("gp:") || id.starts_with("panel:") || id.starts_with("pane:") {
        return None;
    }
    if matches!(search_action(id), Some(SearchAction::Workflow(_))) {
        return None; // The workflow lets the user choose its inputs, including the layer.
    }
    if let Some(reason) = search_platform_disabled(id, cfg!(target_arch = "wasm32")) {
        return Some(reason);
    }
    why_disabled(app, id).or_else(|| {
        (search_action(id).is_none() && app.session.registry.get(id).is_some_and(rustgis_engine::CommandSpec::needs_input))
            .then_some("it needs input: open it from its pane or button, or run it from a script or the assistant")
    })
}

/// Whether the query matches a registry command's search row.
fn command_matches(c: &rustgis_engine::CommandSpec, ql: &str) -> bool {
    ql.is_empty() || c.label.to_lowercase().contains(ql) || c.id.to_lowercase().contains(ql) || c.location.to_lowercase().contains(ql)
}

/// The command palette: type to filter every command, Enter runs the first.
pub fn command_search(app: &mut RustGisApp, ctx: &egui::Context) {
    let Some(mut q) = app.ui.command_search.take() else { return };
    let t = Tokens::get(ctx);
    let mut keep = true;
    let mut chosen: Option<String> = None;
    // Fits small screens: never wider or taller than the window allows.
    let screen = ctx.content_rect();
    let width = (screen.width() - 48.0).clamp(220.0, 520.0);
    let list_h = (screen.height() - 190.0).clamp(120.0, 360.0);
    let modal = egui::Modal::new(egui::Id::unique("command_search")).show(ctx, |ui| {
        ui.set_width(width);
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
            icons::paint(ui.painter(), r, "search", t.text_dim, t.accent);
            let te = ui.add(
                egui::TextEdit::singleline(&mut q)
                    .hint_text(tl!("Search commands — e.g. buffer, basemap, labels"))
                    .desired_width(width - 56.0)
                    .frame(egui::Frame::NONE),
            );
            te.request_focus();
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (r, resp) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
                if resp.hovered() {
                    ui.painter().rect_filled(r, CornerRadius::same(5), t.hover);
                }
                icons::paint(ui.painter(), r.shrink(5.0), "close", t.text_dim, t.accent);
                if resp.on_hover_text(tl!("Close (Esc)")).clicked() {
                    keep = false;
                }
            });
        });
        ui.separator();
        let ql = q.to_lowercase();
        let mut hits: Vec<(String, String, String)> = Vec::new();
        // Geoprocessing tools first when the query names one (a few dozen at most: the GeoLibre
        // suite has a thousand).
        for tool in rustgis_engine::analysis::tools() {
            if hits.len() >= 40 {
                break;
            }
            if !ql.is_empty()
                && (tool.name.to_lowercase().contains(&ql)
                    || tool.id.to_lowercase().contains(&ql)
                    || tl!(tool.name).to_lowercase().contains(&ql))
            {
                hits.push((
                    format!("gp:{}", tool.id),
                    crate::i18n::fmt("{name} (tool)", &[("name", tl_raw!(tool.name))]),
                    crate::i18n::location(tool.toolbox),
                ));
            }
        }
        // Workflow titles are searchable even when they differ from the engine command's label.
        // A command with the same title that routes to the workflow is listed instead, if it matches.
        if !ql.is_empty() {
            for &(section, label) in crate::analysispane::SECTIONS {
                if (label.to_lowercase().contains(&ql) || section.to_lowercase().contains(&ql) || tl!(label).to_lowercase().contains(&ql))
                    && !app.session.registry.commands.iter().any(|c| {
                        c.label == label
                            && matches!(search_action(c.id), Some(SearchAction::Workflow(s)) if s == section)
                            && command_matches(c, &ql)
                    })
                {
                    hits.push((format!("pane:{section}"), tl!(label).to_string(), crate::i18n::location("Workflows")));
                }
            }
        }
        for c in &app.session.registry.commands {
            if command_matches(c, &ql) {
                hits.push((c.id.to_string(), tl!(c.label).to_string(), crate::i18n::location(c.location)));
            }
        }
        let lang = app.lang().code();
        for c in rustgis_engine::plugins::commands() {
            let m = &c.plugin.manifest;
            let label = m.translate(lang, &c.decl.label);
            if ql.is_empty()
                || label.to_lowercase().contains(&ql)
                || c.decl.label.to_lowercase().contains(&ql)
                || m.name.to_lowercase().contains(&ql)
            {
                hits.push((c.id.clone(), label.to_string(), format!("{} › {}", tl!("Plugins"), m.translate(lang, &m.name))));
            }
        }
        // Plugins' panels (`panel:<plugin>`).
        for p in rustgis_engine::plugins::list().into_iter().filter(|p| p.enabled) {
            let m = &p.manifest;
            let Some(d) = &m.panel else { continue };
            let title = m.translate(lang, &d.title);
            if ql.is_empty()
                || title.to_lowercase().contains(&ql)
                || d.title.to_lowercase().contains(&ql)
                || m.name.to_lowercase().contains(&ql)
            {
                let label = crate::i18n::fmt("{name} (panel)", &[("name", title)]);
                hits.push((format!("panel:{}", m.id), label, format!("{} › {}", tl!("Plugins"), m.translate(lang, &m.name))));
            }
        }
        for &(id, label, loc) in UI_COMMANDS {
            // The web app always serves the latest release (no Check for Updates button) and is
            // one process a loopback relay could not reach (`ui.mcpRelay` is desktop only).
            if !command_available(id, cfg!(target_arch = "wasm32")) {
                continue;
            }
            if ql.is_empty() || label.to_lowercase().contains(&ql) || tl!(label).to_lowercase().contains(&ql) {
                hits.push((id.into(), tl!(label).into(), crate::i18n::location(loc)));
            }
        }
        // The ribbon location is dropped when the palette is too narrow to show it.
        let show_loc = width >= 400.0;
        // A solid scroll bar takes its own gutter instead of floating over the locations.
        ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
        // The modal's area starts from its last size, so after a short result list the scroll
        // area would get only that room back (egui's 64 px floor): ask for the full height
        // whenever the list scrolls. A few hits still shrink the palette to fit.
        egui::ScrollArea::vertical().max_height(list_h).min_scrolled_height(list_h).show(ui, |ui| {
            let row_w = ui.available_width();
            // Enter runs the first command that is available now.
            let mut first = true;
            for (id, label, loc) in hits.iter().take(40) {
                // Unavailable commands stay listed, greyed out, with the reason on hover.
                let why = search_disabled(app, id);
                let on = why.is_none();
                let mut b = egui::Button::new(egui::RichText::new(label).size(13.0)).frame(false).min_size(vec2(row_w, 26.0));
                // The command's shortcut after its location.
                let keys = if id.contains(':') && !id.starts_with("plugin:") {
                    String::new()
                } else {
                    crate::keymap::shortcut_text(ui.ctx(), app, id, &Value::Null)
                };
                let right = match (show_loc, keys.is_empty()) {
                    (true, false) => format!("{loc}   {keys}"),
                    (true, true) => loc.clone(),
                    (false, _) => keys,
                };
                if !right.is_empty() {
                    b = b.right_text(egui::RichText::new(right).size(11.0).weak());
                }
                // The scroll area would clip a fill wider than the row.
                let mut resp = row_button(ui, on, b, 0.0);
                if let Some(why) = why {
                    resp = resp.on_disabled_hover_text(crate::i18n::fmt("Unavailable: {reason}", &[("reason", tl_raw!(why))]));
                }
                let mut customize = false;
                if (!id.contains(':') || id.starts_with("plugin:")) && crate::shortcutui::can_customize(app, id) {
                    resp.context_menu(|ui| {
                        if ui.button(tl!("Keyboard Shortcuts")).clicked() {
                            customize = true;
                            ui.close();
                        }
                    });
                }
                if customize {
                    app.ui.shortcut_editor.open = true;
                    app.ui.shortcut_editor.filter = id.clone();
                    keep = false;
                }
                let enter = on && first && ui.input(|i| i.key_pressed(Key::Enter));
                first &= !on;
                if resp.clicked() || enter {
                    chosen = Some(id.clone());
                }
            }
        });
        if ui.input(|i| i.key_pressed(Key::Escape)) {
            keep = false;
        }
    });
    // A click outside the palette closes it too.
    if modal.backdrop_response.clicked() {
        keep = false;
    }
    if let Some(id) = chosen {
        keep = false;
        if let Some(tool) = id.strip_prefix("gp:") {
            let _ = app.run("ui.gp.open", json!({"tool": tool}));
        } else if let Some(plugin) = id.strip_prefix("panel:") {
            let _ = app.run("ui.pane.plugin", json!({"id": plugin}));
        } else if let Some(section) = id.strip_prefix("pane:") {
            let _ = app.run("ui.pane.workflows", json!({"section": section}));
        } else if let Some(action) = search_action(&id) {
            let _ = match action {
                SearchAction::Command(command) => app.run_by_user(command, Value::Null),
                SearchAction::Dialog(name) => app.run("ui.dialog", json!({"name": name})),
                SearchAction::OpenItem(kind) => app.run("ui.openItem", json!({"kind": kind})),
                SearchAction::SaveItem(kind) => app.run("ui.saveItem", json!({"kind": kind})),
                SearchAction::Workflow(section) => app.run("ui.pane.workflows", json!({"section": section})),
                SearchAction::Pane(pane) => {
                    app.ui.open_right(pane);
                    Ok(Value::Null)
                }
            };
        } else {
            let _ = app.run_by_user(&id, Value::Null);
        }
    }
    if keep {
        app.ui.command_search = Some(q);
    }
}

/// The project's website and source code, opened in a new tab in the browser build so the app
/// (and its map) stays open.
pub(crate) fn project_links(ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.add(egui::Hyperlink::from_label_and_url(tl!("Website"), "https://rustgis.app").open_in_new_tab(true));
        ui.label("·");
        ui.add(egui::Hyperlink::from_label_and_url(tl!("GitHub"), "https://github.com/opengeos/RustGIS").open_in_new_tab(true));
    });
}

/// Links to the licence notices of the software and data RustGIS is built with. The web app
/// serves its own copy of `THIRD-PARTY-LICENSES.txt`; the desktop packages ship one next to
/// LICENSE, and link to the current one on GitHub.
pub(crate) fn licence_links(ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        let notices = if cfg!(target_arch = "wasm32") {
            "THIRD-PARTY-LICENSES.txt"
        } else {
            "https://github.com/opengeos/RustGIS/blob/main/THIRD-PARTY-LICENSES.txt"
        };
        ui.add(egui::Hyperlink::from_label_and_url(tl!("Third-party licences"), notices).open_in_new_tab(true));
        ui.label("·");
        let attribution = "https://github.com/opengeos/RustGIS/blob/main/ATTRIBUTION.md";
        ui.add(egui::Hyperlink::from_label_and_url(tl!("Attribution"), attribution).open_in_new_tab(true));
    });
}

pub fn about(app: &mut RustGisApp, ctx: &egui::Context) {
    if app.ui.about {
        let logo_tex = app.logo(ctx);
        let mut open = true;
        egui::Window::new(tl!("About RustGIS")).open(&mut open).collapsible(false).resizable(false).pivot(Align2::CENTER_CENTER).default_pos(ctx.content_rect().center()).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(56.0, 56.0), Sense::hover());
                logo_tex.inspect(|id| {
                    ui.painter().image(*id, r, egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), egui::Color32::WHITE);
                });
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new(tl!("RustGIS")).font(semibold(20.0)));
                    ui.label(egui::RichText::new(crate::i18n::fmt("Version {version}", &[("version", env!("CARGO_PKG_VERSION"))])).weak());
                });
            });
            ui.add_space(8.0);
            let t = Tokens::get(ctx);
            crate::i18n::paragraph(
                ui,
                "A fast, free and open-source GIS written in pure Rust, for mapping, analyzing and sharing geospatial data on the desktop and in the browser.",
                regular(12.5),
                t.text,
            );
            ui.add_space(4.0);
            project_links(ui);
            ui.add_space(6.0);
            crate::i18n::paragraph(
                ui,
                "Sample data: Natural Earth (public domain). Licensed under Apache-2.0.",
                regular(10.5),
                ui.visuals().weak_text_color(),
            );
            let (fonts_from, _) = (rustgis_engine::render::fonts::ui_fonts().source.clone(), ());
            ui.label(egui::RichText::new(crate::i18n::fmt("Interface font: {font}", &[("font", &fonts_from)])).small().weak());
            licence_links(ui);
        });
        app.ui.about = open;
    }
    if app.ui.shortcuts {
        let mut open = true;
        egui::Window::new(tl!("Keyboard Shortcuts"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .pivot(Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .show(ctx, |ui| {
                let mac = ctx.os().is_mac();
                egui::Grid::new("keys").striped(true).spacing(vec2(24.0, 6.0)).show(ui, |ui| {
                    // The keys that run now, from the keymap.
                    let rows: Vec<(String, String)> = app
                        .ui
                        .keymap
                        .grouped()
                        .into_iter()
                        .map(|(command, params, keys)| {
                            let keys: Vec<String> = keys.into_iter().map(|c| c.shown(ctx)).collect();
                            (keys.join(" / "), crate::keymap::label(app, command, params))
                        })
                        .collect();
                    for (k, d) in &rows {
                        ui.label(egui::RichText::new(k).monospace());
                        ui.label(d);
                        ui.end_row();
                    }
                    // Clipboard keys are events, outside the keymap.
                    let clipboard: Vec<String> = crate::keymap::CLIPBOARD
                        .iter()
                        .filter_map(|(keys, _)| crate::keymap::Chord::parse(keys))
                        .map(|c| c.text(mac))
                        .collect();
                    let clipboard = clipboard.join(" / ");
                    for (k, d) in [
                        (clipboard.as_str(), "Copy / Cut / Paste features"),
                        ("Mouse wheel", "Zoom about the pointer"),
                        ("Drag / middle-drag", "Pan (Explore) · any tool"),
                        ("Shift+drag", "Zoom to a box (Explore)"),
                        ("Double-click", "Zoom in · finish a sketch"),
                    ] {
                        ui.label(egui::RichText::new(tl!(k)).monospace());
                        ui.label(tl!(d));
                        ui.end_row();
                    }
                });
            });
        app.ui.shortcuts = open;
    }
}

/// Toasts for recent notices (errors stay longer, and any stays while the pointer is on it, so
/// its text can be selected and copied).
pub fn notices(app: &mut RustGisApp, ctx: &egui::Context) {
    let t = Tokens::get(ctx);
    let now = ctx.input(|i| i.time);
    let total = app.session.notices.len();
    if total > app.map.notices_seen {
        for n in app.session.notices.iter().skip(app.map.notices_seen) {
            app.map.toasts_made += 1;
            app.map.toasts.push(Toast { text: n.text.clone(), error: n.error, at: now, id: app.map.toasts_made });
        }
        app.map.notices_seen = total;
    }
    app.map.toasts.retain(|toast| now - toast.at < if toast.error { 6.0 } else { 3.0 });
    if app.map.toasts.is_empty() {
        return;
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(250));
    let screen = ctx.content_rect();
    let mut y = screen.max.y - 40.0;
    for toast in app.map.toasts.iter_mut().rev().take(3) {
        // Keyed by the notice, not the slot, so a selection stays with its text as toasts come and go.
        let shown = egui::Area::new(egui::Id::unique(("toast", toast.id)))
            .fixed_pos(pos2(screen.max.x - 380.0, y - 44.0))
            .order(egui::Order::Tooltip)
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .fill(t.menu)
                    .stroke(Stroke::new(1.0, if toast.error { t.red } else { t.border_strong }))
                    .corner_radius(CornerRadius::same(8))
                    .shadow(egui::Shadow { offset: [0, 4], blur: 14, spread: 0, color: t.shadow })
                    .inner_margin(egui::Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.set_width(340.0);
                        ui.horizontal(|ui| {
                            let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                            ui.painter().circle_filled(r.center(), 6.0, if toast.error { t.red } else { t.green });
                            ui.add(egui::Label::new(egui::RichText::new(toast.text.as_str()).size(12.0)).wrap().selectable(true));
                        });
                    });
            });
        if shown.response.contains_pointer() {
            toast.at = now;
        }
        y -= 52.0;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn desktop_only_search_actions_are_disabled_only_on_the_web() {
        for id in [
            "project.addFolder",
            "layer.addDatabase",
            "pg.addLayer",
            "pg.saveEdits",
            "gpkg.addLayer",
            "gpkg.saveEdits",
            "gpkg.discardEdits",
        ] {
            assert!(search_platform_disabled(id, true).is_some(), "{id} must explain its web restriction");
            assert_eq!(search_platform_disabled(id, false), None, "{id} stays available on desktop");
        }
        assert_eq!(search_platform_disabled("ui.addData", true), None, "browser file uploads remain available");
    }

    #[test]
    fn typed_scales_read_as_people_write_them() {
        for (text, want) in [
            ("1:12,345", 12_345.0),
            ("1:12345", 12_345.0),
            ("1 : 25 000", 25_000.0),
            ("1/50000", 50_000.0),
            ("12345", 12_345.0),
            ("12'345", 12_345.0),
            ("12.345", 12_345.0),
            ("1.234.567", 1_234_567.0),
            // A dot before exactly three digits groups thousands; otherwise it's a decimal point.
            ("1.234", 1_234.0),
            ("1.5", 1.5),
            ("12.5", 12.5),
            ("25k", 25_000.0),
            ("2.5M", 2_500_000.0),
            ("1:1", 1.0),
            ("  7500  ", 7_500.0),
            // Larger than life (#902).
            ("2:1", 0.5),
            ("2.5:1", 0.4),
            ("10 : 1", 0.1),
            ("4/1", 0.25),
            // A dot is a decimal point there, as the status bar shows it (#923).
            ("1.234:1", 1.0 / 1.234),
            ("1.5:1", 1.0 / 1.5),
            ("2,000:1", 1.0 / 2000.0),
        ] {
            assert_eq!(parse_scale(text), Some(want), "{text}");
        }
        for text in [
            "", "1:", "abc", "0", "1:0", "-5000", "1:-5000", "inf", "NaN", "12.34.5", "k", "1e5", "1:5e300", "0.5", "1:2,5m", "1:12,34",
            ",345", "12,3456", "0:1", "1:2:1", ":1", "25k:1", "1.2.3:1", ".:1",
        ] {
            assert_eq!(parse_scale(text), None, "{text}");
        }
    }

    /// With a text field focused, Ctrl/⌘+Z undoes its typing, not the project; with none, the
    /// project (#685).
    #[test]
    fn undo_in_a_text_field_undoes_its_text() {
        use egui_kittest::kittest::Queryable;
        let mut app = RustGisApp::with_sample();
        app.run("labels.style", serde_json::json!({"layer": "Cities", "size": 20})).unwrap();
        let mut h = egui_kittest::Harness::builder().with_size(vec2(500.0, 700.0)).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                shortcuts(app, &ctx);
                crate::analysispane::script_pane(app, ui, &Tokens::light());
            },
            app,
        );
        h.run();
        h.get_by_role(egui::accesskit::Role::MultilineTextInput).focus();
        h.run();
        h.event(egui::Event::Text("map.zoomFull".into()));
        h.run();
        let depth = h.state().session.undo_depth();
        let undo = egui::Event::Key { key: Key::Z, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::COMMAND };
        h.event(undo.clone());
        h.run();
        assert_eq!(h.state().session.undo_depth(), depth, "the project untouched");
        assert_eq!(h.state().workflow.script, "", "the typing undone");
        h.ctx.memory_mut(|m| m.surrender_focus(egui::Id::unique("script_text")));
        h.run();
        h.event(undo);
        h.run();
        assert_eq!(h.state().session.undo_depth(), depth - 1, "no field focused: the project");
    }

    #[test]
    fn first_person_preserves_default_controls_and_pauses_custom_single_keys() {
        let mut app = RustGisApp::with_sample();
        app.session.map_mut().view.first_person = true;
        app.run("tool.set", json!({"tool": "select"})).unwrap();
        app.ui.keymap.assign(crate::keymap::Chord::parse("T").unwrap(), "ui.theme", Value::Null);
        let before = app.ui.dark;
        let mut h = egui_kittest::Harness::builder().build_ui_state(
            |ui, app: &mut RustGisApp| {
                shortcuts(app, ui.ctx());
            },
            app,
        );
        h.key_press(Key::T);
        h.run();
        assert_eq!(h.state().ui.dark, before);
        h.key_press(Key::Escape);
        h.run();
        assert_eq!(h.state().session.tool.code(), "explore");
        // Home and Delete must also reach the keymap while navigating.
        for key in ["Home", "Delete"] {
            h.state_mut().ui.keymap.assign(crate::keymap::Chord::parse(key).unwrap(), "ui.theme", Value::Null);
            let before = h.state().ui.dark;
            h.key_press(Key::from_name(key).unwrap());
            h.run();
            assert_ne!(h.state().ui.dark, before);
        }
    }

    #[test]
    fn typing_keeps_shift_and_alt_shortcuts_in_the_text_field() {
        use egui_kittest::kittest::Queryable;
        let mut app = RustGisApp::with_sample();
        for key in ["Shift+T", "Alt+T"] {
            app.ui.keymap.assign(crate::keymap::Chord::parse(key).unwrap(), "ui.theme", Value::Null);
        }
        let before = app.ui.dark;
        let mut h = egui_kittest::Harness::builder().build_ui_state(
            |ui, app: &mut RustGisApp| {
                shortcuts(app, ui.ctx());
                let label = ui.label("Script");
                ui.text_edit_singleline(&mut app.workflow.script).labelled_by(label.id);
            },
            app,
        );
        h.run();
        h.get_by_role(egui::accesskit::Role::TextInput).focus();
        h.run();
        for modifiers in [Modifiers::SHIFT, Modifiers::ALT] {
            h.key_press_modifiers(modifiers, Key::T);
            h.run();
            assert_eq!(h.state().ui.dark, before);
        }
    }

    #[test]
    fn header_without_a_unified_title_bar_keeps_its_usual_size() {
        let l = header_layout(None);
        assert_eq!((l.height, l.left_pad), (34.0, 10.0));
    }

    #[test]
    fn header_with_a_unified_title_bar_clears_the_window_buttons() {
        let l = header_layout(Some(TitlebarMetrics { left_inset: 78.0, height: 28.0, ..TitlebarMetrics::FALLBACK }));
        assert_eq!(l.left_pad, 78.0);
        assert_eq!(l.height, 34.0, "never shorter than the usual header");
    }

    #[test]
    fn content_is_centred_on_the_window_buttons_even_in_a_short_title_bar() {
        // Buttons sit at 14 in a 28 high bar; the 34 high header's content must centre there too.
        let l = header_layout(Some(TitlebarMetrics { left_inset: 78.0, height: 28.0, ..TitlebarMetrics::FALLBACK }));
        assert_eq!(l.height / 2.0 - l.bottom_pad / 2.0, 14.0);
        let tall = header_layout(Some(TitlebarMetrics { left_inset: 90.0, height: 40.0, ..TitlebarMetrics::FALLBACK }));
        assert_eq!(tall.bottom_pad, 0.0);
        assert_eq!(header_layout(None).bottom_pad, 0.0);
    }

    #[test]
    fn double_clicking_the_header_does_what_the_system_setting_says() {
        use egui::ViewportCommand::{Maximized, Minimized};
        assert!(matches!(double_click_command(DoubleClick::Zoom, false), Some(Maximized(true))));
        assert!(matches!(double_click_command(DoubleClick::Zoom, true), Some(Maximized(false))));
        assert!(matches!(double_click_command(DoubleClick::Minimize, false), Some(Minimized(true))));
        assert!(double_click_command(DoubleClick::Nothing, false).is_none());
    }

    #[test]
    fn a_taller_native_title_bar_makes_the_header_as_tall() {
        let l = header_layout(Some(TitlebarMetrics { left_inset: 90.0, height: 40.0, ..TitlebarMetrics::FALLBACK }));
        assert_eq!(l.height, 40.0);
    }

    #[test]
    fn hostile_metrics_fall_back_to_sane_values() {
        for (inset, height) in [(f32::NAN, f32::NAN), (-5.0, -5.0), (f32::INFINITY, f32::INFINITY), (1.0e9, 1.0e9)] {
            let l = header_layout(Some(TitlebarMetrics { left_inset: inset, height, ..TitlebarMetrics::FALLBACK }));
            assert!(l.left_pad.is_finite() && (10.0..=120.0).contains(&l.left_pad), "{l:?}");
            assert!(l.height.is_finite() && (34.0..=80.0).contains(&l.height), "{l:?}");
        }
    }

    #[test]
    fn every_new_connection_entry_opens_its_form() {
        for (kind, _) in crate::connectionui::KINDS {
            // The menu leaves database kinds out on the web, where `open_new` refuses them.
            if cfg!(target_arch = "wasm32") && rustgis_engine::connections::DATABASE_KINDS.contains(&kind) {
                continue;
            }
            let mut app = RustGisApp::with_sample();
            app.run("ui.connection", serde_json::json!({"kind": kind})).unwrap();
            assert_eq!(app.dialogs.open, Some("newConnection"), "{kind}");
            assert_eq!(app.ui.connection.kind, kind);
        }
    }
}
