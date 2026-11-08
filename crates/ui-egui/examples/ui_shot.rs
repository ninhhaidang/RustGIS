//! Headless UI screenshots: renders the whole RustGIS window offscreen (wgpu).
//!
//! `cargo run -p rustgis-ui-egui --example ui_shot -- script.jsonl`
//!
//! The script is JSON lines: `{"cmd": id, "params": {...}}` runs a command (ui.* included),
//! `{"shot": "/abs/out.png"}` saves the window, `{"steps": n}` runs frames, `{"sleep": ms}` runs frames
//! for that long, `{"click": label}` clicks the first widget whose label contains the text (with
//! `"exact": true`, whose whole label is the text), `{"combo": text}` opens the combo box showing
//! that text, `{"type": text}` types into the focused box,
//! `{"key": "Enter"}` presses a key (egui's key names), `{"dark": true}` switches theme,
//! `{"drag": [x, y, dx, dy]}` presses the pointer at (x, y) and moves it by (dx, dy), holding
//! it (shots then catch the map mid-drag; `{"release": true}` lets go; `"button": "secondary"`; `"ctrl": true` holds Ctrl
//! drags with the right button),
//! `{"rclick": [x, y]}` right-clicks at (x, y) (the map's menu),
//! `{"pointer":[x,y],"pressed":true|false,"shift":bool,"ctrl":bool}` moves the primary
//! pointer and optionally presses/releases it, stepping one frame; omit `pressed` for motion.
//! `{"scroll": [x, y, dy]}` turns the mouse wheel by `dy` points over (x, y) (negative zooms
//! out) and runs one frame (shots then catch the map mid-zoom),
//! `{"plugin": path}` loads a `.rgplugin` or a plugin folder for the run (as `rustgis-cli run
//! --plugin` does), and `{"testPlugin": "panel"}` the plugin tests use, with a panel,
//! `{"titlebar": {"left_inset": 80, "height": 34}}`
//! draws the header as a macOS unified title bar (`null` turns it off), and
//! `{"width": w, "height": h}` on the first line sets the size (default 1480×900),
//! and `"pixelsPerPoint": scale` sets the output scale (default 2). The scale must be finite and
//! positive, with physical dimensions from 1 pixel through the GPU's maximum 2D texture size.
//! `"dt": seconds` sets each frame's length, default 0.25; a short one such as 0.016 catches the map mid-motion.
//! The window opens the World Atlas sample.
//! `"readsOffFrame": true` on the first line runs Add Data, Load All, models and scripts on a
//! thread of their own, as the desktop app does (the status bar shows the run and Cancel).
//! `"recovery": "/abs/dir"` on the first line gives the run that recovery folder, as the desktop
//! app has one (Settings › Autosave; a copy left there is offered by the recovery dialog).
//!
//! Add `"ribbonSplit": true` to the first line for a compact reusable split-button showcase
//! instead of the app. Its "Disable arrows", "Disable enclosing UI", and "Long two-line labels"
//! checkboxes are script-clickable. `{"cmd":"ui.language","params":{"code":"de"}}` changes the
//! language (untranslated strings fall back to English); `{"dark":true}` changes theme. The
//! three checkboxes precede the buttons in Tab order; click "Select options" to open its menu.
//! Shots retain pointer hover from the last click. No selection initially disables "Zoom To"
//! but leaves "Zoom options" enabled.

use egui_kittest::kittest::Queryable;
use rustgis_ui_egui::{RustGisApp, big_split_menu, i18n, theme};
use serde_json::{Value, json};

#[derive(Default)]
struct RibbonSplitShowcase {
    disable_arrows: bool,
    disable_enclosing_ui: bool,
    long_labels: bool,
    fonts_installed: bool,
    applied_look: Option<(String, bool, u64)>,
}

fn split_tool_menu(ui: &mut egui::Ui, app: &mut RustGisApp) {
    for (label, tool) in [("Pan", "explore"), ("Measure Distance", "measure")] {
        if ui.button(i18n::t(label)).clicked() {
            if let Err(e) = app.run("tool.set", json!({"tool": tool})) {
                eprintln!("tool.set: {e}");
            }
            ui.close();
        }
    }
}

impl RibbonSplitShowcase {
    fn show(&mut self, ui: &mut egui::Ui, app: &mut RustGisApp) {
        if !self.fonts_installed {
            theme::install_fonts(ui.ctx(), &[], None);
            self.fonts_installed = true;
        }
        // The revision is part of the key: a reload with the same name still reapplies the look.
        let look = (app.ui.theme_name.clone(), app.ui.dark, app.theme_revision());
        if self.applied_look.as_ref() != Some(&look) {
            let chosen = app.current_theme();
            theme::apply(ui.ctx(), &chosen.tokens, &chosen.metrics);
            self.applied_look = Some(look);
        }
        i18n::set_current(app.chosen_lang());
        ui.heading("Ribbon split buttons");
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.disable_arrows, "Disable arrows");
            ui.checkbox(&mut self.disable_enclosing_ui, "Disable enclosing UI");
            ui.checkbox(&mut self.long_labels, "Long two-line labels");
        });
        ui.add_space(8.0);
        let tokens = theme::Tokens::get(ui.ctx());
        egui::Frame::new().fill(tokens.ribbon).inner_margin(12.0).show(ui, |ui| {
            ui.add_enabled_ui(!self.disable_enclosing_ui, |ui| {
                ui.horizontal_top(|ui| {
                    let select_checked = app.session.tool.code() == "select";
                    big_split_menu(
                        ui,
                        app,
                        "select",
                        if self.long_labels { "Select Features\nwith a rectangle" } else { "Select" },
                        "Click or drag a box to select features. Shift adds, Ctrl removes.",
                        "tool.set",
                        json!({"tool": "select"}),
                        select_checked,
                        "Select options",
                        "Choose an alternative map tool.",
                        !self.disable_arrows,
                        split_tool_menu,
                    );
                    ui.add_space(20.0);
                    big_split_menu(
                        ui,
                        app,
                        "zoom_sel",
                        if self.long_labels { "Zoom To\nselected features" } else { "Zoom To" },
                        "Zoom to the selected features.",
                        "select.zoomTo",
                        Value::Null,
                        false,
                        "Zoom options",
                        "Choose a map tool even when nothing is selected.",
                        !self.disable_arrows,
                        split_tool_menu,
                    );
                });
            });
        });
        ui.add_space(8.0);
        ui.label(format!("Current tool: {} ({})", i18n::t(app.session.tool.label()), app.session.tool.code()));
        ui.label("Select always runs selection; either options menu can switch to Pan or Measure Distance.");
        ui.label("Zoom To needs selected features; its options remain available without a selection.");
    }
}

/// Matches the renderer's rounded physical target size and the active device's texture limit.
fn screenshot_scale(size: egui::Vec2, value: f64, max_dimension: u32) -> Result<f32, String> {
    let scale = value as f32;
    if !scale.is_finite() || scale <= 0.0 {
        return Err("pixelsPerPoint must be finite, positive, and representable as f32".into());
    }
    let physical = size * scale;
    if [physical.x, physical.y].into_iter().any(|v| !v.is_finite() || v.round() < 1.0 || v.round() > max_dimension as f32) {
        return Err(format!("scaled width and height must each round to 1..={max_dimension} pixels"));
    }
    Ok(scale)
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: ui_shot script.jsonl");
    let script = std::fs::read_to_string(&path).expect("read script");
    let lines: Vec<serde_json::Value> =
        script.lines().filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str(l).expect("json line")).collect();
    let w = lines.iter().find_map(|l| l.get("width").and_then(|v| v.as_f64())).unwrap_or(1480.0) as f32;
    let h = lines.iter().find_map(|l| l.get("height").and_then(|v| v.as_f64())).unwrap_or(900.0) as f32;
    let pixels_per_point = lines.iter().find_map(|l| l.get("pixelsPerPoint").and_then(|v| v.as_f64())).unwrap_or(2.0);
    let dt = lines.iter().find_map(|l| l.get("dt").and_then(|v| v.as_f64())).unwrap_or(0.25) as f32;
    let ribbon_split = lines.first().and_then(|l| l.get("ribbonSplit")).and_then(Value::as_bool).unwrap_or(false);
    let mut split_showcase = RibbonSplitShowcase::default();
    let mut app = RustGisApp::with_sample();
    // As the desktop app does: Add Data, Load All, models and scripts run on a thread of their own.
    app.reads_off_frame = lines.first().and_then(|l| l.get("readsOffFrame")).and_then(Value::as_bool).unwrap_or(false);
    if let Some(dir) = lines.first().and_then(|l| l.get("recovery")).and_then(Value::as_str) {
        let store = rustgis_ui_egui::FolderStore::open(std::path::Path::new(dir)).expect("recovery folder");
        app = app.with_services(rustgis_ui_egui::Services { recovery: Some(Box::new(store)), ..Default::default() });
    }
    // One shared wgpu render state: the app's GPU map backend and the harness's renderer must
    // live on the same device for the map's paint callback to be valid.
    let rs = {
        let setup = egui_wgpu::WgpuSetup::CreateNew(egui_wgpu::WgpuSetupCreateNew::without_display_handle());
        let instance = pollster::block_on(setup.new_instance());
        pollster::block_on(egui_wgpu::RenderState::create(
            &egui_wgpu::WgpuConfiguration { wgpu_setup: setup, ..Default::default() },
            &instance,
            None,
            egui_wgpu::RendererOptions::PREDICTABLE,
        ))
        .expect("render state")
    };
    let pixels_per_point = match screenshot_scale(egui::vec2(w, h), pixels_per_point, rs.device.limits().max_texture_dimension_2d) {
        Ok(scale) => scale,
        Err(error) => {
            eprintln!("invalid screenshot settings: {error}");
            std::process::exit(2);
        }
    };
    app.set_render_state(Some(rs.clone()));
    let mut harness = egui_kittest::Harness::builder()
        .with_size(egui::vec2(w, h))
        .with_pixels_per_point(pixels_per_point)
        .with_max_steps(10_000)
        .with_step_dt(dt)
        .renderer(egui_kittest::wgpu::WgpuTestRenderer::from_render_state(rs))
        .build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if ribbon_split {
                    // The split showcase skips `logic`, the only place the scale reaches egui.
                    if (ui.ctx().zoom_factor() - app.ui.interface_scale).abs() > 0.001 {
                        ui.ctx().set_zoom_factor(app.ui.interface_scale);
                    }
                    split_showcase.show(ui, app);
                } else {
                    let ctx = ui.ctx().clone();
                    app.logic(&ctx);
                    app.ui(ui);
                }
            },
            app,
        );
    for _ in 0..6 {
        harness.step();
    }
    // The split showcase skips frame logic, so load user themes before scripted selection.
    if ribbon_split && let Err(error) = harness.state_mut().run("ui.themeReload", Value::Null) {
        eprintln!("ui.themeReload: {error}");
    }
    // The button the last drag holds down.
    let mut held = egui::PointerButton::Primary;
    // And the modifiers held with it.
    let mut held_mods = egui::Modifiers::NONE;
    for l in lines {
        if let Some(p) = l.get("shot").and_then(|v| v.as_str()) {
            for _ in 0..4 {
                harness.step();
            }
            // A tilted map's exact frame is drawn off the UI thread: wait for it.
            let end = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while harness.state().map.drawing() && std::time::Instant::now() < end {
                std::thread::sleep(std::time::Duration::from_millis(20));
                harness.step();
            }
            harness.step();
            match harness.render() {
                Ok(img) => {
                    img.save(p).expect("save png");
                    println!("{p}");
                }
                Err(e) => eprintln!("render failed: {e}"),
            }
        } else if let Some(ms) = l.get("sleep").and_then(|v| v.as_u64()) {
            // Real time passing, for work off the frame (downloads, the AI assistant's model).
            let end = std::time::Instant::now() + std::time::Duration::from_millis(ms);
            while std::time::Instant::now() < end {
                std::thread::sleep(std::time::Duration::from_millis(100));
                harness.step();
            }
        } else if let Some(n) = l.get("steps").and_then(|v| v.as_u64()) {
            for _ in 0..n {
                harness.step();
            }
        } else if let Some(id) = l.get("cmd").and_then(|v| v.as_str()) {
            let params = l.get("params").cloned().unwrap_or(serde_json::Value::Null);
            if let Err(e) = harness.state_mut().run(id, params) {
                eprintln!("{id}: {e}");
            }
            for _ in 0..3 {
                harness.step();
            }
        } else if let Some(p) = l.get("plugin").and_then(|v| v.as_str()) {
            let loaded = rustgis_plugins::Package::from_path(std::path::Path::new(p)).and_then(|pkg| rustgis_plugins::load(pkg, p));
            if let Err(e) = loaded {
                eprintln!("{p}: {e}");
            }
            for _ in 0..3 {
                harness.step();
            }
        } else if let Some(which) = l.get("testPlugin").and_then(|v| v.as_str()) {
            let bytes = match which {
                "panel" => rustgis_plugins::testing::panel_package("test-panel"),
                _ => rustgis_plugins::testing::package("test-plugin", &[]),
            };
            if let Err(e) = rustgis_plugins::Package::from_zip(&bytes).and_then(|pkg| rustgis_plugins::load(pkg, "test")) {
                eprintln!("{which}: {e}");
            }
            for _ in 0..3 {
                harness.step();
            }
        } else if let Some(t) = l.get("titlebar") {
            let num = |k: &str| t.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
            harness.state_mut().ui.titlebar = t.is_object().then(|| rustgis_ui_egui::TitlebarMetrics {
                left_inset: num("left_inset"),
                height: num("height"),
                ..rustgis_ui_egui::TitlebarMetrics::FALLBACK
            });
            for _ in 0..3 {
                harness.step();
            }
        } else if let Some(d) = l.get("dark").and_then(|v| v.as_bool()) {
            harness.state_mut().ui.dark = d;
            for _ in 0..3 {
                harness.step();
            }
        } else if let Some(label) = l.get("click").and_then(|v| v.as_str()) {
            // `"exact": true`: the label is the whole of it (an Add button, not Add Data).
            let node = if l.get("exact").and_then(|v| v.as_bool()) == Some(true) {
                harness.query_all_by_label(label).next()
            } else {
                harness.query_all_by_label_contains(label).next()
            };
            match node {
                Some(node) => node.click(),
                None => eprintln!("nothing labelled '{label}'"),
            }
            for _ in 0..3 {
                harness.step();
            }
        } else if let Some(shown) = l.get("combo").and_then(|v| v.as_str()) {
            // A combo box has no label of its own: it is found by what it shows.
            match harness.query_all_by_role(egui::accesskit::Role::ComboBox).find(|n| n.value().as_deref() == Some(shown)) {
                Some(node) => node.click(),
                None => eprintln!("no combo box showing '{shown}'"),
            }
            for _ in 0..3 {
                harness.step();
            }
        } else if let Some(d) = l.get("pointer").and_then(Value::as_array) {
            let Some((x, y)) = d.first().and_then(Value::as_f64).zip(d.get(1).and_then(Value::as_f64)) else {
                eprintln!("pointer takes [x, y]");
                continue;
            };
            let pos = egui::pos2(x as f32, y as f32);
            if held != egui::PointerButton::Primary
                && let Some(previous) = harness.ctx.input(|i| i.pointer.latest_pos())
            {
                harness.event(egui::Event::PointerButton { pos: previous, button: held, pressed: false, modifiers: held_mods });
            }
            held = egui::PointerButton::Primary;
            let ctrl = l.get("ctrl").and_then(Value::as_bool).unwrap_or(false);
            held_mods = egui::Modifiers {
                shift: l.get("shift").and_then(Value::as_bool).unwrap_or(false),
                ctrl,
                command: ctrl,
                ..egui::Modifiers::NONE
            };
            harness.event(egui::Event::ModifiersChanged(held_mods));
            harness.event(egui::Event::PointerMoved(pos));
            if let Some(pressed) = l.get("pressed").and_then(Value::as_bool) {
                harness.event(egui::Event::PointerButton { pos, button: held, pressed, modifiers: held_mods });
            }
            harness.step();
        } else if let Some(d) = l.get("drag").and_then(|v| v.as_array()) {
            let n: Vec<f32> = d.iter().map(|v| v.as_f64().unwrap_or(0.0) as f32).collect();
            let [x, y, dx, dy] = n[..] else {
                eprintln!("drag takes [x, y, dx, dy]");
                continue;
            };
            let next = match l.get("button").and_then(|v| v.as_str()) {
                Some("secondary") => egui::PointerButton::Secondary,
                _ => egui::PointerButton::Primary,
            };
            // A drag with another button lets go of the one still held.
            if next != held
                && let Some(pos) = harness.ctx.input(|i| i.pointer.latest_pos())
            {
                harness.event(egui::Event::PointerButton { pos, button: held, pressed: false, modifiers: egui::Modifiers::NONE });
            }
            held = next;
            held_mods = if l.get("ctrl").and_then(|v| v.as_bool()) == Some(true) {
                egui::Modifiers { ctrl: true, command: true, ..egui::Modifiers::NONE }
            } else {
                egui::Modifiers::NONE
            };
            let button = |pos: egui::Pos2, pressed: bool| egui::Event::PointerButton { pos, button: held, pressed, modifiers: held_mods };
            harness.event(egui::Event::ModifiersChanged(held_mods));
            harness.event(egui::Event::PointerMoved(egui::pos2(x, y)));
            harness.event(button(egui::pos2(x, y), true));
            harness.step();
            for i in 1..=8 {
                let t = i as f32 / 8.0;
                harness.event(egui::Event::ModifiersChanged(held_mods));
                harness.event(egui::Event::PointerMoved(egui::pos2(x + dx * t, y + dy * t)));
                harness.step();
            }
        } else if let Some(d) = l.get("rclick").and_then(|v| v.as_array()) {
            let n: Vec<f32> = d.iter().map(|v| v.as_f64().unwrap_or(0.0) as f32).collect();
            let [x, y] = n[..] else {
                eprintln!("rclick takes [x, y]");
                continue;
            };
            let pos = egui::pos2(x, y);
            harness.event(egui::Event::PointerMoved(pos));
            for pressed in [true, false] {
                harness.event(egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Secondary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                });
            }
            for _ in 0..3 {
                harness.step();
            }
        } else if let Some(d) = l.get("scroll").and_then(|v| v.as_array()) {
            let n: Vec<f32> = d.iter().map(|v| v.as_f64().unwrap_or(0.0) as f32).collect();
            let [x, y, dy] = n[..] else {
                eprintln!("scroll takes [x, y, dy]");
                continue;
            };
            harness.event(egui::Event::PointerMoved(egui::pos2(x, y)));
            harness.event(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, dy),
                phase: egui::TouchPhase::Move,
                modifiers: egui::Modifiers::NONE,
                source: egui::MouseWheelSource::Wheel,
            });
            harness.step();
        } else if l.get("release").is_some() {
            if let Some(pos) = harness.ctx.input(|i| i.pointer.latest_pos()) {
                harness.event(egui::Event::PointerButton { pos, button: held, pressed: false, modifiers: held_mods });
            }
            held_mods = egui::Modifiers::NONE;
            harness.event(egui::Event::ModifiersChanged(held_mods));
            for _ in 0..3 {
                harness.step();
            }
        } else if let Some(text) = l.get("type").and_then(|v| v.as_str()) {
            harness.event(egui::Event::Text(text.to_string()));
            for _ in 0..3 {
                harness.step();
            }
        } else if let Some(k) = l.get("key").and_then(|v| v.as_str()) {
            match egui::Key::from_name(k) {
                Some(key) => harness.key_press(key),
                None => eprintln!("no key named '{k}'"),
            }
            for _ in 0..3 {
                harness.step();
            }
        } else if let Some(t) = l.get("tab").and_then(|v| v.as_str()) {
            harness.state_mut().ui.tab = t.to_string();
            for _ in 0..3 {
                harness.step();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::screenshot_scale;
    use egui::vec2;

    #[test]
    fn screenshot_scale_rejects_invalid_values_and_target_sizes() {
        for scale in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY, f64::MAX, 1e-300, 1e-30, 8.01] {
            assert!(screenshot_scale(vec2(1024.0, 512.0), scale, 8192).is_err(), "{scale}");
        }
        assert!(screenshot_scale(vec2(512.0, 1024.0), 8.01, 8192).is_err());
    }

    #[test]
    fn screenshot_scale_accepts_default_and_rounded_renderer_boundaries() {
        assert_eq!(screenshot_scale(vec2(1366.0, 768.0), 2.0, 8192), Ok(2.0));
        assert_eq!(screenshot_scale(vec2(1024.0, 512.0), 8.0, 8192), Ok(8.0));
        assert_eq!(screenshot_scale(vec2(1.0, 1.0), 0.5, 8192), Ok(0.5));
        assert!(screenshot_scale(vec2(1.0, 1.0), 0.49, 8192).is_err());
        assert_eq!(screenshot_scale(vec2(8192.25, 512.0), 1.0, 8192), Ok(1.0));
        assert!(screenshot_scale(vec2(8192.5, 512.0), 1.0, 8192).is_err());
    }
}
