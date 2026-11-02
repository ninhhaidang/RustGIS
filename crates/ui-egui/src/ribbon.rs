//! The ribbon: the tab strip (with the Project button) and every tab's groups. Each control runs a
//! command by id.

use egui::{Align2, CornerRadius, Rect, Sense, Stroke, Ui, pos2, vec2};
use rustgis_engine::Tool;
use rustgis_engine::data::GeometryType;
use rustgis_engine::map::planets::EARTH_BASEMAP;
use rustgis_engine::map::view::Weather;
use rustgis_engine::map::{Basemap, RasterRenderer, Renderer, Resampling, Stretch};
use serde_json::{Value, json};

use crate::theme::{Tokens, regular, semibold};
use crate::widgets::{
    CONTENT_H, Dragged, LABEL_H, big, big_form_menu, big_menu, big_split_menu, col, group, menu_item, small, small_form_menu, small_menu,
    submenu_row, text_w,
};
use crate::{RightPane, RustGisApp, icons};

pub const TABS: [&str; 11] = ["Home", "Data", "Style", "Labeling", "Edit", "Analyze", "View", "Publish", "Plugins", "Settings", "Help"];

/// Tabs that always show: Settings holds Customize › Ribbon, the way to show hidden tabs again.
pub const FIXED_TABS: [&str; 2] = ["Settings", "Help"];

/// The tab that holds what an older tab name (in saved pane sets, tasks and scripts) held; Home for
/// a name that is no tab. Plugins' tabs pass through.
pub fn current_tab(name: &str) -> &str {
    let tab = match name {
        "Map" => "Home",
        "Insert" | "Raster Data" => "Data",
        "Feature Layer" | "Raster Layer" => "Style",
        "Analysis" | "Imagery" => "Analyze",
        "Share" | "Layout" => "Publish",
        _ => name,
    };
    if TABS.contains(&tab) || tab.starts_with("plugin:") { tab } else { "Home" }
}

/// Saved hidden tabs under the current names: renamed tabs carry over, and names of tabs whose
/// groups were spread over several others are dropped (they could no longer be shown again), and so
/// are the tabs that always show.
pub fn current_hidden_tabs(saved: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for name in saved {
        let tab = match name.as_str() {
            "Map" => "Home",
            "Analysis" => "Analyze",
            "Share" => "Publish",
            n => n,
        };
        if (TABS.contains(&tab) || tab.starts_with("plugin:")) && !FIXED_TABS.contains(&tab) && !out.iter().any(|t| t == tab) {
            out.push(tab.to_string());
        }
    }
    out
}

/// The tab id of a plugin's own tab.
pub fn plugin_tab_id(plugin: &str) -> String {
    format!("plugin:{plugin}")
}

/// The enabled plugins with a tab of their own: (tab id, caption in the interface language).
fn plugin_tabs(app: &RustGisApp) -> Vec<(String, String)> {
    let lang = app.lang().code();
    rustgis_engine::plugins::list()
        .into_iter()
        .filter(|p| p.enabled)
        .filter_map(|p| p.manifest.tab.as_ref().map(|t| (plugin_tab_id(&p.manifest.id), p.manifest.translate(lang, &t.label).to_string())))
        .collect()
}

pub fn show(app: &mut RustGisApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    // A plugin's tab whose plugin was turned off or removed.
    if app.ui.tab.starts_with("plugin:") && !plugin_tabs(app).iter().any(|(id, _)| *id == app.ui.tab) {
        app.ui.tab = "Plugins".into();
    }
    egui::Panel::top("tabs")
        .exact_size(32.0)
        .show_separator_line(false)
        .frame(egui::Frame::NONE.fill(t.tab_strip).inner_margin(egui::Margin { left: 8, right: 8, top: 0, bottom: 0 }))
        .show(ui, |ui| tab_strip(app, ui, &t));
    if app.ui.ribbon_minimized && !app.ui.ribbon_peek {
        return;
    }
    // A peeked ribbon closes on the next click outside the tab strip and the ribbon itself.
    // Popups opened from the ribbon (menus, combos, colour pickers) draw outside it, so a click in
    // one must not close it.
    let peek_started = app.ui.ribbon_peek && ui.input(|i| i.pointer.any_click()) && !egui::Popup::is_any_open(ui.ctx());
    let ribbon = egui::Panel::top("ribbon")
        .exact_size(CONTENT_H + LABEL_H + 12.0)
        .show_separator_line(false)
        .frame(egui::Frame::NONE.fill(t.ribbon).inner_margin(egui::Margin { left: 8, right: 8, top: 5, bottom: 3 }))
        .show(ui, |ui| {
            let r = ui.max_rect();
            let full = Rect::from_min_max(pos2(r.min.x - 8.0, r.max.y + 3.0), pos2(r.max.x + 8.0, r.max.y + 4.0));
            ui.painter().rect_filled(full, 0.0, t.border);
            let tips = [tl!("Scroll the ribbon left"), tl!("Scroll the ribbon right")];
            arrow_scroll_area(ui, &t, "ribbon_groups", egui::ScrollArea::horizontal(), tips, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = vec2(2.0, 0.0);
                    let tab = app.ui.tab.clone();
                    match tab.as_str() {
                        t if t.starts_with("plugin:") => plugin_tab(app, ui, t.trim_start_matches("plugin:")),
                        "Data" => data_tab(app, ui),
                        "Style" => style_tab(app, ui),
                        "Labeling" => labeling_tab(app, ui),
                        "Edit" => edit_tab(app, ui),
                        "Analyze" => analyze_tab(app, ui),
                        "View" => view_tab(app, ui),
                        "Publish" => publish_tab(app, ui),
                        "Plugins" => plugins_tab(app, ui),
                        "Settings" => settings_tab(app, ui),
                        "Help" => help_tab(app, ui),
                        _ => home_tab(app, ui),
                    }
                });
            });
        });
    if peek_started {
        let pos = ui.input(|i| i.pointer.interact_pos());
        let inside = pos.is_some_and(|p| ribbon.response.rect.contains(p) || app.ui.tab_strip_rect.is_some_and(|r| r.contains(p)));
        if !inside {
            app.ui.ribbon_peek = false;
        }
    }
}

const SCROLL_BUTTON_W: f32 = 18.0;

/// Arrow buttons at the edges of a horizontal scroll area (the ribbon's groups, the tab strip)
/// whose content is wider than the window: the scroll bar is hidden (it would cover the group
/// labels and tabs) and the wheel alone is undiscoverable.
///
/// The offset a click asked for, if one did.
fn scroll_buttons<R>(ui: &mut Ui, t: &Tokens, scroll: &egui::scroll_area::ScrollAreaOutput<R>, tips: [&str; 2]) -> Option<f32> {
    let view = scroll.inner_rect;
    let max = (scroll.content_size.x - view.width()).max(0.0);
    if max < 1.0 {
        return None;
    }
    let offset = scroll.state.clamped_offset().x;
    let step = (view.width() * 0.6).max(120.0);
    let mut moved = None;
    for (left, show) in [(true, offset > 0.5), (false, offset < max - 0.5)] {
        if !show {
            continue;
        }
        let x = if left { view.min.x } else { view.max.x - SCROLL_BUTTON_W };
        let r = Rect::from_min_size(pos2(x, view.min.y), vec2(SCROLL_BUTTON_W, view.height()));
        let resp = ui.interact(r, scroll.id.with(left), Sense::click());
        let fill = if resp.hovered() { t.hover } else { t.ribbon };
        ui.painter().rect_filled(r, CornerRadius::same(4), fill);
        ui.painter().rect_stroke(r, CornerRadius::same(4), Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
        let icon = if left { "chevron_left" } else { "chevron_right" };
        let ir = Rect::from_center_size(r.center(), vec2(16.0, 16.0));
        icons::paint(ui.painter(), ir, icon, t.text_dim, t.accent);
        let tip = if left { tips[0] } else { tips[1] };
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, tip));
        if resp.on_hover_text(tip).clicked() {
            moved = Some(if left { (offset - step).max(0.0) } else { (offset + step).min(max) });
        }
    }
    moved
}

/// A horizontal scroll area (`area`, named `name`) without a scroll bar, with [`scroll_buttons`]
/// at its edges.
fn arrow_scroll_area<R>(ui: &mut Ui, t: &Tokens, name: &str, area: egui::ScrollArea, tips: [&str; 2], add: impl FnOnce(&mut Ui) -> R) {
    // egui sets a scroll area's offset only as it is built, so a click moves it on the next frame.
    // It is asked for once, on a frame egui keeps: a discarded one doesn't move the area, and asking
    // on every frame would pin the area there against the wheel.
    let key = ui.make_persistent_id(("arrow_scroll_offset", name));
    let mut area = area.scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden);
    let pending = ui.data(|d| d.get_temp::<f32>(key));
    if let Some(x) = pending {
        area = area.horizontal_scroll_offset(x);
    }
    let scroll = area.show(ui, add);
    if pending.is_some() && !ui.ctx().will_discard() {
        ui.data_mut(|d| d.remove_temp::<f32>(key));
    }
    if let Some(x) = scroll_buttons(ui, t, &scroll, tips) {
        ui.data_mut(|d| d.insert_temp(key, x));
        ui.ctx().request_repaint();
    }
}

/// The collapse button at the right of the tab strip.
const COLLAPSE_W: f32 = 24.0;

/// The active tool indicator at the right of the tab strip, unless the Explore tool is on: its
/// label and width.
fn tool_indicator(app: &RustGisApp, ui: &Ui) -> Option<(String, f32)> {
    let tool = app.session.tool;
    if tool == Tool::Explore {
        return None;
    }
    let label = crate::i18n::fmt("{tool} tool active · Esc to stop", &[("tool", tl_raw!(tool.label()))]);
    let w = text_w(ui, &label, regular(11.5)) + 18.0;
    Some((label, w))
}

fn tab_strip(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing = vec2(2.0, 0.0);
        // The Project button opens the backstage.
        let pw = text_w(ui, tl!("Project"), semibold(12.5)) + 26.0;
        let (pr, presp) = ui.allocate_exact_size(vec2(pw, 26.0), Sense::click());
        let fill = if presp.hovered() { crate::theme::BRAND_DARK } else { crate::theme::BRAND };
        ui.painter().rect_filled(pr, CornerRadius::same(6), fill);
        ui.painter().text(pr.center(), Align2::CENTER_CENTER, tl!("Project"), semibold(12.5), egui::Color32::WHITE);
        if presp.on_hover_text(tl!("Open, save and create projects")).clicked() {
            app.ui.backstage = true;
        }
        ui.add_space(6.0);
        // RustGIS's tabs, then plugins' own tabs; the user can hide any of them.
        let mut tabs: Vec<(String, String)> = TABS.iter().map(|t| (t.to_string(), tl!(*t).to_string())).collect();
        tabs.extend(plugin_tabs(app));
        tabs.retain(|(id, _)| !app.ui.hidden_tabs.iter().any(|h| h == id));
        // The tabs scroll between the Project button and the right-side controls (the collapse
        // button and the active tool indicator) when the window is too narrow for them.
        let tool = tool_indicator(app, ui);
        let tool_w = tool.as_ref().map_or(0.0, |(_, w)| w + ui.spacing().item_spacing.x);
        let room = (ui.available_width() - COLLAPSE_W - 8.0 - tool_w).max(60.0);
        // Tighter tabs when the window is too narrow for the usual padding.
        let text: f32 = tabs.iter().map(|(_, c)| text_w(ui, c, semibold(12.5))).sum();
        let pad = ((room - text) / tabs.len().max(1) as f32).clamp(8.0, 22.0);
        // The active tab scrolls into view for a few frames (a pass that egui discards drops its
        // scroll request) after it changes, or the strip's room or tab widths do.
        let active_id = egui::Id::unique("tab_strip_active");
        let key = (app.ui.tab.clone(), room.round() as i32, text.round() as i32);
        let (shown, scrolls): ((String, i32, i32), u8) = ui.data(|d| d.get_temp(active_id)).unwrap_or_default();
        let scrolls = if shown == key { scrolls } else { 0 };
        if scrolls < 3 {
            ui.data_mut(|d| d.insert_temp(active_id, (key, scrolls + 1)));
        }
        let area = egui::ScrollArea::horizontal().id_salt("tab_strip").max_width(room);
        let tips = [tl!("Scroll the tabs left"), tl!("Scroll the tabs right")];
        arrow_scroll_area(ui, t, "tab_strip", area, tips, |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing = vec2(2.0, 0.0);
                for (tab, caption) in &tabs {
                    let tab = tab.as_str();
                    let active = app.ui.tab == tab;
                    let font = if active { semibold(12.5) } else { regular(12.5) };
                    let shown = caption.as_str();
                    let w = text_w(ui, shown, font.clone()) + pad;
                    let (r, resp) = ui.allocate_exact_size(vec2(w, 32.0), Sense::click());
                    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, active, shown));
                    let body = Rect::from_min_max(pos2(r.min.x, r.min.y + 3.0), r.max);
                    if active {
                        ui.painter().rect_filled(body, CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 }, t.ribbon);
                        ui.painter().rect_stroke(
                            body,
                            CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 },
                            Stroke::new(1.0, t.border),
                            egui::StrokeKind::Inside,
                        );
                        ui.painter().rect_filled(
                            Rect::from_min_max(pos2(r.min.x + 1.0, r.max.y - 1.5), pos2(r.max.x - 1.0, r.max.y + 1.0)),
                            0.0,
                            t.ribbon,
                        );
                        let bar = t.accent;
                        ui.painter().rect_filled(
                            Rect::from_min_max(pos2(r.min.x + 8.0, r.max.y - 3.0), pos2(r.max.x - 8.0, r.max.y - 1.0)),
                            CornerRadius::same(1),
                            bar,
                        );
                    } else if resp.hovered() {
                        ui.painter().rect_filled(body.shrink2(vec2(1.0, 2.0)), CornerRadius::same(5), t.hover);
                    }
                    let color = if active { t.text } else { t.text_dim };
                    ui.painter().text(pos2(r.center().x, r.center().y + 1.5), Align2::CENTER_CENTER, shown, font, color);
                    if resp.double_clicked() {
                        let _ = app.run("ui.ribbon", Value::Null);
                    } else if resp.clicked() {
                        if app.ui.ribbon_minimized {
                            app.ui.ribbon_peek = !(app.ui.ribbon_peek && app.ui.tab == tab);
                        }
                        app.ui.tab = tab.to_string();
                    }
                    // A tab that became active (clicked, or opened by a command) scrolls
                    // into view, clear of the arrow buttons.
                    if active && scrolls < 3 {
                        ui.scroll_to_rect_animation(r.expand2(vec2(SCROLL_BUTTON_W, 0.0)), None, egui::style::ScrollAnimation::none());
                    }
                }
            });
        });
        app.ui.tab_strip_rect = Some(ui.max_rect());
        // Right side: collapse/expand the ribbon, then the quick tool indicator.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let (cr, cresp) = ui.allocate_exact_size(vec2(COLLAPSE_W, COLLAPSE_W), Sense::click());
            if cresp.hovered() {
                ui.painter().rect_filled(cr, CornerRadius::same(5), t.hover);
            }
            let icon = if app.ui.ribbon_minimized { "chevron_down" } else { "up" };
            icons::paint(ui.painter(), cr.shrink(5.0), icon, t.text_dim, t.accent);
            let tip = if app.ui.ribbon_minimized { tl!("Expand the ribbon (Ctrl+F1)") } else { tl!("Collapse the ribbon (Ctrl+F1)") };
            if cresp.on_hover_text(tip).clicked() {
                let _ = app.run("ui.ribbon", Value::Null);
            }
            if let Some((label, w)) = tool {
                let (r, resp) = ui.allocate_exact_size(vec2(w, 22.0), Sense::click());
                ui.painter().rect_filled(r, CornerRadius::same(11), t.checked);
                ui.painter().text(r.center(), Align2::CENTER_CENTER, label, regular(11.5), t.accent_text);
                if resp.clicked() {
                    app.session.tool = Tool::Explore;
                    app.map.cancel_sketch();
                }
            }
        });
    });
}

fn tool_btn(ui: &mut Ui, app: &mut RustGisApp, icon: &str, label: &str, desc: &str, tool: Tool) {
    let checked = app.session.tool == tool;
    let r = big(ui, app, icon, label, desc, "tool.set", json!({"tool": tool.code()}), checked, false);
    if r.clicked() {
        app.map.cancel_sketch();
    }
}

/// The Clipboard group of the Edit tab.
fn clipboard_group(app: &mut RustGisApp, ui: &mut Ui) {
    group(ui, "Clipboard", |ui| {
        big_menu(ui, app, "paste", "Paste", "Paste copied features into the selected layer, or as a new layer.", |ui, app| {
            menu_item(ui, app, "paste", "Paste", "edit.paste", Value::Null);
            menu_item(ui, app, "paste", "Paste Special (as a new layer)", "edit.pasteSpecial", Value::Null);
        });
        col(ui, |ui| {
            small(ui, app, "cut", "Cut", "Copy the selected features and delete them.", "ui.cut", Value::Null, false);
            small(ui, app, "copy", "Copy", "Copy the selected features (also as GeoJSON text).", "ui.copy", Value::Null, false);
            small(ui, app, "copy_path", "Copy Path", "Copy the selected layer's data source path.", "ui.copyPath", Value::Null, false);
        });
    });
}

fn home_tab(app: &mut RustGisApp, ui: &mut Ui) {
    // New maps and layouts first: where work starts (#328).
    project_group(app, ui);
    group(ui, "Navigate", |ui| {
        tool_btn(
            ui,
            app,
            "explore",
            "Pan",
            "Pan with the mouse, zoom with the wheel and click features to see their attributes.",
            Tool::Explore,
        );
        big_menu(ui, app, "bookmark", "Bookmarks", "Go to a saved extent or save the current one.", |ui, app| {
            let names: Vec<String> = app.session.map().bookmarks.iter().map(|b| b.name.clone()).collect();
            if names.is_empty() {
                ui.label(egui::RichText::new(tl!("No bookmarks yet")).weak());
            }
            for n in names {
                // The name goes to the bookmark; the button at the right removes it.
                let (go, remove) = egui::Sides::new().show(
                    ui,
                    |ui| ui.add(egui::Button::new(n.as_str()).frame(false).min_size(vec2(0.0, 26.0))).clicked(),
                    |ui| {
                        let tip = crate::i18n::fmt("Remove {name}", &[("name", &n)]);
                        let r = crate::widgets::icon_button(ui, "delete", &tip);
                        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &tip));
                        r.clicked()
                    },
                );
                if go {
                    let _ = app.run("bookmark.go", json!({"name": n}));
                    ui.close();
                }
                if remove {
                    let _ = app.run("bookmark.remove", json!({"name": n}));
                }
            }
            ui.separator();
            menu_item(ui, app, "bookmark_new", "New Bookmark…", "ui.dialog", json!({"name": "bookmark"}));
            // Every map's bookmarks, renamed, described, updated and ordered in place (#351).
            menu_item(ui, app, "pane_right", "Manage Bookmarks", "ui.pane.bookmarks", Value::Null);
            // The files apart from the bookmarks themselves.
            ui.separator();
            menu_item(ui, app, "bookmark_import", "Import Bookmarks…", "ui.openItem", json!({"kind": "bookmarks"}));
            menu_item(ui, app, "bookmark_export", "Export Bookmarks…", "ui.saveItem", json!({"kind": "bookmarks"}));
        });
        col(ui, |ui| {
            small(ui, app, "zoom_full", "Full Extent", "Zoom to the extent of all layers.", "map.zoomFull", Value::Null, false);
            small(ui, app, "zoom_in", "Zoom In", "Zoom in on the centre of the map.", "map.zoomIn", Value::Null, false);
            small(ui, app, "zoom_out", "Zoom Out", "Zoom out from the centre of the map.", "map.zoomOut", Value::Null, false);
        });
        col(ui, |ui| {
            small(ui, app, "prev_extent", "Previous", "Go back to the previous extent.", "map.prevExtent", Value::Null, false);
            small(ui, app, "next_extent", "Next", "Go forward to the next extent.", "map.nextExtent", Value::Null, false);
            small(
                ui,
                app,
                "zoom_box",
                "Zoom Box",
                "Drag a box to zoom into.",
                "tool.set",
                json!({"tool": "zoomIn"}),
                app.session.tool == Tool::ZoomIn,
            );
        });
        col(ui, |ui| {
            let fp = app.session.map().view.first_person;
            small_menu(
                ui,
                app,
                "explore",
                if fp { "First-Person" } else { "Planar" },
                "Navigate with the mouse (planar) or walk with W A S D, Q and E (first-person).",
                move |ui, app| {
                    if ui.radio(!fp, tl!("Planar Navigation")).clicked() {
                        let _ = app.run("map.navigation", json!({"mode": "planar"}));
                        ui.close();
                    }
                    if ui.radio(fp, tl!("First-Person Navigation")).clicked() {
                        let _ = app.run("map.navigation", json!({"mode": "firstPerson"}));
                        ui.close();
                    }
                },
            );
            let mode = app.session.map().popup_mode;
            small_menu(ui, app, "popup", "Identify", "Choose which layers a click identifies.", |ui, app| {
                for m in rustgis_engine::map::PopupMode::ALL {
                    if ui.radio(mode == m, tl!(m.label())).clicked() {
                        let _ = app.run("map.popupMode", json!({"mode": m.code()}));
                        ui.close();
                    }
                }
            });
            small_menu(ui, app, "search", "Locate", "Find a place by coordinates, name, or address.", |ui, app| {
                menu_item(ui, app, "goto_xy", "By Coordinates…", "ui.dialog", json!({"name": "goToXY"}));
                menu_item(ui, app, "search", "By Place or Address…", "ui.dialog", json!({"name": "findPlace"}));
            });
        });
        let on = app.ui.right == Some(crate::RightPane::Directions);
        big(
            ui,
            app,
            "directions",
            "Directions",
            "Plan a route between places and follow it turn by turn (Valhalla, on OpenStreetMap data).",
            "ui.pane.directions",
            Value::Null,
            on,
            false,
        );
    });
    group(ui, "Add", |ui| {
        big_form_menu(ui, app, "basemap", "Basemap", "Change the map's backdrop.", basemap_gallery);
        big_split_menu(
            ui,
            app,
            "add_data",
            "Add\nData",
            "Add a data file to the map.",
            "ui.addData",
            Value::Null,
            false,
            "Add Data options",
            "Add data from another source or use a data-adding option.",
            true,
            |ui, app| {
                menu_item(ui, app, "", "Delimited Text…", "ui.addDelimited", Value::Null);
                menu_item(ui, app, "", "Geocode Addresses…", "ui.dialog", json!({"name": "geocode"}));
                menu_item(ui, app, "", "From URL…", "ui.addUrl", Value::Null);
                menu_item(ui, app, "", "From Service…", "ui.addService", Value::Null);
                menu_item(ui, app, "", "Zarr…", "ui.dialog", json!({"name": "zarr"}));
                // Databases are read through DuckDB, a program the browser can't run.
                #[cfg(not(target_arch = "wasm32"))]
                menu_item(ui, app, "", "Database…", "ui.dialog", json!({"name": "addDatabase"}));
                #[cfg(not(target_arch = "wasm32"))]
                menu_item(ui, app, "", "Edit GeoPackage Table…", "ui.dialog", json!({"name": "gpkgAddLayer"}));
                menu_item(ui, app, "", "S3…", "ui.dialog", json!({"name": "s3"}));
                submenu_row(ui, "", tl!("New Connection"), |ui| {
                    for (kind, label) in crate::connectionui::KINDS {
                        // Database drivers aren't built for the browser.
                        if cfg!(target_arch = "wasm32") && rustgis_engine::connections::DATABASE_KINDS.contains(&kind) {
                            continue;
                        }
                        menu_item(ui, app, "", label, "ui.connection", json!({"kind": kind}));
                    }
                });
                menu_item(ui, app, "graphics_layer", "Graphics Layer", "graphics.newLayer", Value::Null);
                #[cfg(not(target_arch = "wasm32"))]
                menu_item(ui, app, "add_folder", "Add Folder…", "ui.addFolder", Value::Null);
                // A file with several layers (a geodatabase, a GeoPackage) asks which to add.
                #[cfg(not(target_arch = "wasm32"))]
                {
                    let mut ask = app.ui.ask_layers;
                    let resp = ui
                        .checkbox(&mut ask, tl!("Ask Which Layers to Add"))
                        .on_hover_text(tl!("When a file holds several layers, choose the ones to add instead of adding them all."));
                    if resp.clicked() {
                        let _ = app.run("ui.askLayers", json!({"on": ask}));
                    }
                }
                ui.separator();
                sample_data_gallery(ui, app);
            },
        );
        big(
            ui,
            app,
            "web_map",
            "Data\nSources",
            "Show or hide the Data Sources pane.",
            "ui.pane.sources",
            Value::Null,
            app.ui.left == Some(crate::LeftPane::Sources),
            false,
        );
        big_menu(ui, app, "web_data", "Web\nData", "Add data from public web services and catalogs.", web_data_menu);
    });
    group(ui, "Selection", |ui| {
        let checked = app.session.tool == Tool::Select;
        let (primary, _) = big_split_menu(
            ui,
            app,
            "select",
            "Select",
            "Click or drag a box to select features. Shift adds, Ctrl removes.",
            "tool.set",
            json!({"tool": "select"}),
            checked,
            "Select options",
            "Choose polygon, lasso, or radius selection.",
            true,
            |ui, app| {
                for tool in [Tool::SelectPolygon, Tool::SelectLasso, Tool::SelectRadius] {
                    let row = ui.add(egui::Button::selectable(app.session.tool == tool, tl!(tool.label())));
                    let row = if tool == Tool::SelectRadius {
                        row.on_hover_text(tl!(
                            "Approximate local-distance selection. The retained geographic prefilter can exclude features near the outline."
                        ))
                    } else {
                        row
                    };
                    if row.clicked() && app.run("tool.set", json!({"tool": tool.code()})).is_ok() {
                        app.map.cancel_sketch();
                        ui.close();
                    }
                }
            },
        );
        if primary.clicked() {
            app.map.cancel_sketch();
        }
        big(
            ui,
            app,
            "select_attr",
            "Select By\nAttributes",
            "Select features with a SQL expression.",
            "ui.dialog",
            json!({"name": "selectByAttributes"}),
            false,
            false,
        );
        big(
            ui,
            app,
            "select_loc",
            "Select By\nLocation",
            "Select features that intersect another layer.",
            "ui.gp.open",
            json!({"tool": "selectByLocation"}),
            false,
            false,
        );
        col(ui, |ui| {
            small(ui, app, "clear_sel", "Clear", "Clear the selection in every layer.", "select.clear", Value::Null, false);
            small(ui, app, "zoom_sel", "Zoom To", "Zoom to the selected features.", "select.zoomTo", Value::Null, false);
            small(ui, app, "table", "Attributes", "Open the attribute table of the selected layer.", "table.open", Value::Null, false);
        });
    });
    group(ui, "Inspect", |ui| {
        big_menu(ui, app, "measure", "Measure", "Measure geodesic distances and areas on the map.", |ui, app| {
            if ui.button(tl!("Measure Distance")).clicked() {
                let _ = app.run("tool.set", json!({"tool": "measure"}));
                app.map.cancel_sketch();
                ui.close();
            }
            if ui.button(tl!("Measure Area")).clicked() {
                let _ = app.run("tool.set", json!({"tool": "measureArea"}));
                app.map.cancel_sketch();
                ui.close();
            }
            ui.separator();
            ui.label(tl!("Measurement units"));
            for (imperial, units, label) in [(false, "metric", "Metric"), (true, "imperial", "Imperial")] {
                if ui.radio(app.ui.measurement_imperial == imperial, tl!(label)).clicked() {
                    let _ = app.run("ui.measurementUnits", json!({"units": units}));
                    ui.close();
                }
            }
        });
        small(
            ui,
            app,
            "chart",
            "Infographics",
            "Counts, totals and top categories of what's in view.",
            "ui.pane.infographics",
            Value::Null,
            app.ui.right == Some(RightPane::Infographics),
        );
    });
}

/// The Web Data menu: public web services and catalogs, the ones covering the world in one
/// alphabetical list (a service fits one place, where a list by topic would leave it in several).
/// Services covering one country go in a submenu named after it.
fn web_data_menu(ui: &mut Ui, app: &mut RustGisApp) {
    let svc = |id: &str| json!({"service": id});
    let pane = |name: &str| json!({"name": name});
    menu_item(ui, app, "", "ArcGIS Portal…", "ui.searchOpenData", svc("arcgis-portal"));
    menu_item(ui, app, "", "Boundaries…", "ui.dialog", pane("boundaries"));
    menu_item(ui, app, "", "Data Cubes…", "ui.dialog", pane("dataCubes"));
    menu_item(ui, app, "", "Dynamical…", "ui.searchOpenData", svc("dynamical"));
    menu_item(ui, app, "", "Earthdata GIS…", "ui.searchOpenData", svc("earthdata-gis"));
    menu_item(ui, app, "", "Fields of the World…", "ui.dialog", pane("fieldsOfTheWorld"));
    menu_item(ui, app, "", "GeoLens…", "ui.searchOpenData", svc("geolens"));
    menu_item(ui, app, "", "Hugging Face…", "ui.searchOpenData", svc("huggingface"));
    menu_item(ui, app, "", "NASA Earthaccess…", "ui.dialog", pane("earthaccess"));
    menu_item(ui, app, "", "NASA Earthdata…", "ui.dialog", pane("nasaEarthdata"));
    menu_item(ui, app, "", "Natural Earth…", "ui.dialog", pane("naturalEarth"));
    menu_item(ui, app, "", "Ocean Data Platform…", "ui.searchOpenData", svc("ocean-data"));
    menu_item(ui, app, "", "OpenAerialMap…", "ui.searchOpenData", svc("openaerialmap"));
    menu_item(ui, app, "", "OpenStreetMap…", "ui.dialog", pane("osm"));
    menu_item(ui, app, "", "Overture Maps…", "ui.dialog", pane("overture"));
    menu_item(ui, app, "", "Planet Open Data…", "ui.stac.planet", Value::Null);
    menu_item(ui, app, "", "Portolan…", "ui.stac.portolan", Value::Null);
    menu_item(ui, app, "", "Satellite Embeddings…", "ui.dialog", pane("embeddings"));
    menu_item(ui, app, "", "Search Open Data…", "ui.searchOpenData", Value::Null);
    menu_item(ui, app, "", "Sentinel-2 Explorer…", "ui.stac.sentinel2", Value::Null);
    menu_item(ui, app, "", "Source Cooperative…", "ui.dialog", pane("sourcecoop"));
    menu_item(ui, app, "", "STAC Search…", "ui.stac", Value::Null);
    menu_item(ui, app, "", "Vantor…", "ui.stac.vantor", Value::Null);
    ui.separator();
    submenu_row(ui, "", tl!("France"), |ui| {
        menu_item(ui, app, "", "IGN LiDAR HD…", "ui.searchOpenData", svc("ign-lidar"));
    });
    submenu_row(ui, "", tl!("United States"), |ui| {
        menu_item(ui, app, "", "EnviroAtlas…", "ui.dialog", pane("enviroAtlas"));
        menu_item(ui, app, "", "FEMA NFHL…", "ui.dialog", pane("femaNfhl"));
        menu_item(ui, app, "", "Tennessee GIS…", "ui.searchOpenData", svc("tennessee"));
        menu_item(ui, app, "", "The National Map…", "ui.dialog", pane("nationalMap"));
        menu_item(ui, app, "", "US Federal GIS…", "ui.searchOpenData", svc("us-federal"));
        menu_item(ui, app, "", "US Local GIS…", "ui.searchOpenData", svc("us-local"));
        menu_item(ui, app, "", "US State GIS…", "ui.searchOpenData", svc("us-state"));
        menu_item(ui, app, "", "USGS 3DEP DEM…", "ui.searchOpenData", svc("usgs-dem"));
        menu_item(ui, app, "", "USGS LiDAR…", "ui.stac.usgsLidar", Value::Null);
        menu_item(ui, app, "", "USGS NLDI…", "ui.dialog", pane("nldi"));
    });
}

pub(crate) fn sample_data_gallery(ui: &mut Ui, app: &mut RustGisApp) {
    ui.label(egui::RichText::new(tl!("Sample data (Natural Earth)")).small().weak());
    for (name, label) in
        [("countries", "World Countries"), ("cities", "Populated Places"), ("rivers", "Major Rivers"), ("lakes", "Major Lakes")]
    {
        menu_item(ui, app, "", label, "layer.addSample", json!({"name": name}));
    }
}

pub(crate) fn basemap_gallery(ui: &mut Ui, app: &mut RustGisApp) {
    let t = Tokens::get(ui.ctx());
    ui.label(egui::RichText::new(tl!("Basemaps")).font(semibold(12.5)));
    ui.add_space(4.0);
    egui::Grid::new("basemaps").spacing(vec2(8.0, 8.0)).show(ui, |ui| {
        for (i, b) in Basemap::ALL.into_iter().enumerate() {
            let (water, land, outline, _) = b.palette();
            let (r, resp) = ui.allocate_exact_size(vec2(92.0, 74.0), Sense::click());
            // Web basemaps cover the built-in one.
            let sel = app.session.map().basemap == b && app.session.map().tiles.iter().all(|t| t.overlay);
            resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, sel, tl!(b.label())));
            let thumb = Rect::from_min_size(r.min + vec2(4.0, 4.0), vec2(84.0, 48.0));
            ui.painter().rect_filled(thumb, CornerRadius::same(4), crate::theme::c32(water));
            // A little continent.
            let pts = [(0.18, 0.3), (0.42, 0.18), (0.6, 0.35), (0.55, 0.7), (0.3, 0.82), (0.15, 0.6)];
            let poly: Vec<egui::Pos2> =
                pts.iter().map(|(x, y)| pos2(thumb.min.x + x * thumb.width(), thumb.min.y + y * thumb.height())).collect();
            ui.painter().add(egui::Shape::convex_polygon(poly, crate::theme::c32(land), Stroke::new(1.0, crate::theme::c32(outline))));
            let poly2: Vec<egui::Pos2> = [(0.68, 0.22), (0.88, 0.3), (0.84, 0.55), (0.7, 0.5)]
                .iter()
                .map(|(x, y)| pos2(thumb.min.x + x * thumb.width(), thumb.min.y + y * thumb.height()))
                .collect();
            ui.painter().add(egui::Shape::convex_polygon(poly2, crate::theme::c32(land), Stroke::new(1.0, crate::theme::c32(outline))));
            if sel || resp.hovered() {
                ui.painter().rect_stroke(
                    r,
                    CornerRadius::same(6),
                    Stroke::new(if sel { 2.0 } else { 1.0 }, t.accent),
                    egui::StrokeKind::Inside,
                );
            }
            ui.painter().text(pos2(r.center().x, r.max.y - 11.0), Align2::CENTER_CENTER, tl!(b.label()), regular(11.5), t.text);
            if resp.clicked() {
                let _ = app.run("map.basemap", json!({"name": b.code()}));
                ui.close();
            }
            if i % 3 == 2 {
                ui.end_row();
            }
        }
    });
    web_basemaps(ui, app, &t);
    planet_basemaps(ui, app);
    tile_layers(ui, app, &t);
}

/// The celestial body switcher's menu (the Layers pane's planet button): picking a body moves the
/// map there, with one of its basemaps; the Earth selects USGS Imagery.
pub fn body_menu(ui: &mut Ui, app: &mut RustGisApp) {
    ui.label(egui::RichText::new(tl!("Celestial Body")).font(semibold(12.0)));
    let current = app.session.map().body().id;
    for b in rustgis_engine::geom::BODIES {
        if ui.add(egui::Button::selectable(b.id == current, tl!(b.name))).clicked() {
            let _ = app.run("map.body", json!({"name": b.id}));
            ui.close();
        }
    }
}

/// The celestial bodies, and the basemaps of the one the map is on (#223).
fn planet_basemaps(ui: &mut Ui, app: &mut RustGisApp) {
    ui.add_space(6.0);
    ui.separator();
    ui.label(egui::RichText::new(tl!("Celestial Bodies")).font(semibold(12.5)));
    ui.add_space(2.0);
    let body = app.session.map().body();
    ui.horizontal_wrapped(|ui| {
        ui.set_max_width(300.0);
        ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
        for b in rustgis_engine::geom::BODIES {
            let tip = tl!("Measure on this body, with one of its basemaps.");
            if ui.add(egui::Button::selectable(b.id == body.id, tl!(b.name))).on_hover_text(tip).clicked() {
                let _ = app.run("map.body", json!({"name": b.id}));
                ui.close();
            }
        }
    });
    if body.is_earth() {
        return;
    }
    ui.add_space(4.0);
    let shown: Vec<String> = app.session.map().tiles.iter().filter(|l| !l.overlay && l.draws()).map(|l| l.id.clone()).collect();
    egui::Grid::new("planet-basemaps").spacing(vec2(6.0, 4.0)).show(ui, |ui| {
        for (i, m) in rustgis_engine::map::planets::basemaps_of(body.id).enumerate() {
            let sel = shown.first().map(String::as_str) == Some(m.id);
            let resp = ui.add_sized(vec2(140.0, 22.0), egui::Button::selectable(sel, m.name).truncate()).on_hover_text(m.attribution);
            if resp.clicked() {
                let _ = app.run("map.body", json!({"name": body.id, "basemap": m.id}));
                ui.close();
            }
            if i % 2 == 1 {
                ui.end_row();
            }
        }
    });
}

const OPENFREEMAP: &str = "OpenFreeMap © OpenMapTiles Data from OpenStreetMap";

/// Web basemaps that need no API key, offered next to the built-in ones:
/// (id, name, kind, url, attribution, max zoom).
const WEB_BASEMAPS: [(&str, &str, &str, &str, &str, u8); 6] = [
    ("openfreemap-liberty", "OpenFreeMap Liberty", "style", "https://tiles.openfreemap.org/styles/liberty", OPENFREEMAP, 14),
    ("openfreemap-bright", "OpenFreeMap Bright", "style", "https://tiles.openfreemap.org/styles/bright", OPENFREEMAP, 14),
    ("openfreemap-positron", "OpenFreeMap Positron", "style", "https://tiles.openfreemap.org/styles/positron", OPENFREEMAP, 14),
    ("openfreemap-dark", "OpenFreeMap Dark", "style", "https://tiles.openfreemap.org/styles/dark", OPENFREEMAP, 14),
    ("openfreemap-fiord", "OpenFreeMap Fiord", "style", "https://tiles.openfreemap.org/styles/fiord", OPENFREEMAP, 14),
    (EARTH_BASEMAP.id, EARTH_BASEMAP.name, "raster", EARTH_BASEMAP.url, EARTH_BASEMAP.attribution, EARTH_BASEMAP.max_zoom),
];

/// The id of the registry plugin with many more web basemaps.
const BASEMAPS_PLUGIN: &str = "basemaps";

/// Web basemaps (OpenFreeMap, USGS) that replace the other tile basemaps or stack on them, and
/// where to get more.
fn web_basemaps(ui: &mut Ui, app: &mut RustGisApp, t: &Tokens) {
    ui.add_space(6.0);
    ui.separator();
    ui.label(egui::RichText::new(tl!("Web Basemaps")).font(semibold(12.5)));
    ui.add_space(2.0);
    // Stacked: a web basemap goes over the others (to blend with them in the Layers pane)
    // rather than replacing them.
    let stack_id = egui::Id::unique("web-basemaps-stack");
    let mut stack = ui.data_mut(|d| *d.get_persisted_mut_or_default::<bool>(stack_id));
    let resp = ui.checkbox(&mut stack, tl!("Stack on the current basemaps"));
    let tip =
        tl!("Add a web basemap over the others instead of replacing them, to blend it with them. Click one that shows to take it off.");
    if resp.on_hover_text(tip).changed() {
        ui.data_mut(|d| d.insert_persisted(stack_id, stack));
    }
    // The web basemaps showing: the one on top, or every one when stacking.
    let shown: Vec<String> = app.session.map().tiles.iter().filter(|l| !l.overlay && l.draws()).map(|l| l.id.clone()).collect();
    egui::Grid::new("web-basemaps").spacing(vec2(6.0, 4.0)).show(ui, |ui| {
        for (i, (id, name, kind, url, attribution, max_zoom)) in WEB_BASEMAPS.into_iter().enumerate() {
            let sel = if stack { shown.iter().any(|s| s == id) } else { shown.first().map(String::as_str) == Some(id) };
            let resp = ui.add_sized(vec2(140.0, 22.0), egui::Button::selectable(sel, name)).on_hover_text(url);
            if resp.clicked() {
                // Stacking, a shown basemap's button takes it off, keeping the others' blending;
                // adding it again would replace it with a fresh layer on top.
                let _ = if stack && sel {
                    app.run("map.removeTiles", json!({"id": id}))
                } else {
                    let p = json!({"url": url, "kind": kind, "name": name, "attribution": attribution, "maxZoom": max_zoom, "id": id, "replace": !stack});
                    app.run("map.addTiles", p)
                };
                ui.close();
            }
            if i % 2 == 1 {
                ui.end_row();
            }
        }
    });
    ui.add_space(4.0);
    // The plugin's tab when it shows; else the Plugin Manager, to turn the plugin on or install it.
    let tab = plugin_tab_id(BASEMAPS_PLUGIN);
    let (ask, link, hint, then) = if plugin_tabs(app).iter().any(|(id, _)| *id == tab) {
        (tl!("More basemaps:"), tl!("Open the Basemaps tab"), tl!("The Basemaps plugin's tab of public basemaps."), None)
    } else if rustgis_engine::plugins::get(BASEMAPS_PLUGIN).is_some() {
        (
            tl!("More basemaps:"),
            tl!("Turn on the Basemaps plugin"),
            tl!("It's installed but turned off: turn it on in the Plugin Manager."),
            Some(json!({"open": true, "tab": "installed"})),
        )
    } else {
        (
            tl!("Want more basemaps?"),
            tl!("Install the Basemaps plugin"),
            tl!("OpenStreetMap, Esri, USGS, NASA and more, from the plugin registry."),
            Some(json!({"open": true, "tab": "browse", "filter": BASEMAPS_PLUGIN})),
        )
    };
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.label(egui::RichText::new(ask).weak().small());
        let resp = ui.add(egui::Link::new(egui::RichText::new(link).small().color(t.accent)));
        if resp.on_hover_text(hint).clicked() {
            match then {
                Some(p) => {
                    let _ = app.run("ui.plugins", p);
                }
                None => app.ui.tab = tab.clone(),
            }
            ui.close();
        }
    });
}

/// The map's web tile layers (top first, in the drawing order): shown, opacity, order among
/// them and removal, and adding one.
fn tile_layers(ui: &mut Ui, app: &mut RustGisApp, t: &Tokens) {
    ui.add_space(6.0);
    ui.separator();
    ui.label(egui::RichText::new(tl!("Web Tile Layers")).font(semibold(12.5)));
    let tiles = app.session.map().tiles.clone();
    if tiles.is_empty() {
        ui.label(egui::RichText::new(tl!("None. Add one by address, or from a basemap plugin.")).weak().small());
    }
    // Each tile layer's place in the drawing order of all the map's layers (#808), top first,
    // and the map layer it's drawn over.
    let slots = app.session.map().tile_slots();
    let places: Vec<(usize, usize)> = app
        .session
        .map()
        .stack()
        .into_iter()
        .enumerate()
        .filter_map(|(at, d)| if let rustgis_engine::map::Drawn::Tile(i) = d { Some((i, at)) } else { None })
        .collect();
    let mut action: Option<(&str, Value)> = None;
    for (k, (i, _)) in places.iter().copied().enumerate() {
        let Some(layer) = tiles.get(i) else { continue };
        // Up or down past the tile layer next to it over the same map layer (a basemap among the
        // basemaps): past the map's layers, the Layers pane moves it.
        let same = |o: &&(usize, usize)| slots.get(o.0) == slots.get(i);
        let (above, below) = (k.checked_sub(1).and_then(|k| places.get(k)).filter(same), places.get(k + 1).filter(same));
        ui.horizontal(|ui| {
            let mut vis = layer.visible;
            if ui.checkbox(&mut vis, "").on_hover_text(tl!("Show or hide")).changed() {
                action = Some(("map.setTiles", json!({"index": i, "visible": vis})));
            }
            let name = if layer.overlay { format!("{} · {}", layer.name, tl!("overlay")) } else { layer.name.clone() };
            ui.allocate_ui_with_layout(vec2(160.0, 18.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.set_width(160.0);
                ui.add(egui::Label::new(egui::RichText::new(name).color(t.text)).truncate()).on_hover_text(&layer.url);
            });
            let mut o = layer.opacity * 100.0;
            let slider = ui.add(egui::Slider::new(&mut o, 0.0..=100.0).suffix("%").show_value(false)).on_hover_text(tl!("Opacity"));
            if slider.drag_stopped() || (slider.changed() && !slider.dragged()) {
                action = Some(("map.setTiles", json!({"index": i, "opacity": o / 100.0})));
            }
            if ui.add_enabled(above.is_some(), egui::Button::new("▲").small()).on_hover_text(tl!("Move up")).clicked()
                && let Some((_, to)) = above
            {
                action = Some(("map.setTiles", json!({"index": i, "to": to})));
                ui.close();
            }
            // (Without it, the place below the next one is that one's place now.)
            if ui.add_enabled(below.is_some(), egui::Button::new("▼").small()).on_hover_text(tl!("Move down")).clicked()
                && let Some((_, to)) = below
            {
                action = Some(("map.setTiles", json!({"index": i, "to": to})));
                ui.close();
            }
            if crate::widgets::icon_button(ui, "close", tl!("Remove")).clicked() {
                action = Some(("map.removeTiles", json!({"index": i})));
                ui.close();
            }
        });
    }
    if let Some((cmd, p)) = action {
        let _ = app.run(cmd, p);
    }
    ui.horizontal(|ui| {
        if ui.button(tl!("Add Tile Layer…")).on_hover_text(tl!("Add raster tiles, vector tiles or a map style by address.")).clicked() {
            let _ = app.run("ui.dialog", json!({"name": "addTiles"}));
            ui.close();
        }
        if !tiles.is_empty() && ui.button(tl!("Reload")).on_hover_text(tl!("Fetch the tiles and styles of the web layers again.")).clicked()
        {
            let _ = app.run("map.reloadWebLayers", json!({}));
            ui.close();
        }
        if !tiles.is_empty() && ui.button(tl!("Remove All")).clicked() {
            let _ = app.run("map.removeTiles", json!({}));
            ui.close();
        }
    });
}

fn offline_group(app: &mut RustGisApp, ui: &mut Ui) {
    group(ui, "Offline", |ui| {
        let areas: Vec<String> = app.session.project.offline_areas.iter().map(|a| a.name.clone()).collect();
        big(
            ui,
            app,
            "export",
            "Download\nMap",
            "Take the data in view offline, to edit and sync back later.",
            "offline.download",
            Value::Null,
            false,
            false,
        );
        col(ui, |ui| {
            for (icon, label, cmd, desc) in [
                ("redo", "Sync", "offline.sync", "Send offline edits back and refresh."),
                ("map", "Open", "offline.open", "Open an offline area to edit."),
                ("delete", "Remove", "offline.remove", "Remove an offline area."),
            ] {
                let a = areas.clone();
                small_menu(ui, app, icon, label, desc, move |ui, app| {
                    if a.is_empty() {
                        ui.label(egui::RichText::new(tl!("No offline areas")).weak());
                    }
                    for n in &a {
                        if ui.button(n.as_str()).clicked() {
                            let _ = app.run(cmd, json!({"name": n}));
                            app.map.invalidate();
                            ui.close();
                        }
                    }
                });
            }
        });
    });
}

fn feature_cache_group(app: &mut RustGisApp, ui: &mut Ui) {
    group(ui, "Feature Cache", |ui| {
        col(ui, |ui| {
            small(ui, app, "add_data", "Fill", "Prepare every layer's geometry for this projection.", "cache.fill", Value::Null, false);
            small(ui, app, "clear_sel", "Empty", "Free the prepared geometry.", "cache.empty", Value::Null, false);
        });
        col(ui, |ui| {
            small(ui, app, "close", "Cancel", "Stop filling the cache.", "cache.cancel", Value::Null, false);
            small(
                ui,
                app,
                "redo",
                "Auto Cache",
                "Fill the cache whenever the projection changes.",
                "cache.auto",
                Value::Null,
                app.session.auto_cache,
            );
        });
    });
}

/// The label settings of the whole map (#897): they work without a layer, so the Labeling tab
/// shows them whatever is selected.
fn map_labeling_group(app: &mut RustGisApp, ui: &mut Ui) {
    group(ui, "Map Labeling", |ui| {
        let d = app.session.map().label_display;
        col(ui, |ui| {
            small(
                ui,
                app,
                "label_lock",
                "Lock",
                "Keep the labelled features fixed as the map moves.",
                "labels.lock",
                Value::Null,
                d.locked,
            );
            small(ui, app, "label_pause", "Pause", "Stop drawing labels, for faster navigation.", "labels.pause", Value::Null, d.paused);
            small(
                ui,
                app,
                "label_unplaced",
                "View Unplaced",
                "Show the labels that don't fit, in red.",
                "labels.viewUnplaced",
                Value::Null,
                d.show_unplaced,
            );
        });
        big(
            ui,
            app,
            "label_convert",
            "Convert",
            "Convert the selected layer's labels into an annotation layer.",
            "labels.convert",
            Value::Null,
            false,
            false,
        );
    });
}

/// Runs `layout.publication` for `profile` from the New Layout menu, in `style` on a sheet: a sheet
/// is titled with the map's name, a figure's title being its caption's.
fn publication_layout(app: &mut RustGisApp, profile: &rustgis_engine::cmd::layout::PublicationProfile, style: Option<&str>) {
    let mut p = json!({"profile": profile.id});
    if profile.sheet {
        p["title"] = json!(app.session.map().name.clone());
    }
    if let Some(style) = style {
        p["style"] = json!(style);
    }
    // The view's extent may not be known (an empty map): said, and the tab stays.
    match app.run("layout.publication", p) {
        Ok(r) => {
            // What the layout couldn't do as asked (a legend clipped, a layer still reading).
            let said: Vec<&str> = r["warnings"].as_array().into_iter().flatten().filter_map(|w| w.as_str()).collect();
            if !said.is_empty() {
                app.session.notify(said.join("\n"), false);
            }
            app.ui.tab = "Publish".into();
        }
        Err(e) => app.session.notify(e, true),
    }
}

/// A sheet style's name in the menu and what it looks like, `tl!` literals (as
/// [`publication_label`]); `None` for a style added without them.
fn publication_style(id: &str) -> Option<(&'static str, &'static str)> {
    Some(match id {
        "classic" => (tl!("Classic"), tl!("The title over the map, the sources and the reference in boxes under it.")),
        "banner" => (tl!("Banner"), tl!("The title and subtitle in a band of colour across the top.")),
        "panel" => (tl!("Panel"), tl!("The map at the left; the title, locator, legend, sources and reference in a panel at the right.")),
        "editorial" => (tl!("Editorial"), tl!("A large serif title; the note, the locator and the legend in a column at the right.")),
        "modular" => (tl!("Modular"), tl!("Cards on a light page: the map, the locator and the legend each in one.")),
        "poster" => (tl!("Poster"), tl!("The page in colour, the title in light text and the map in a light card.")),
        _ => return None,
    })
}

/// A publication profile's name in the menu: a `tl!` literal each, so the catalogs find it (the
/// engine's labels, which it matches in English); `None` for a profile added without one.
fn publication_label(id: &str) -> Option<&'static str> {
    Some(match id {
        "a4-portrait" => tl!("A4 portrait sheet"),
        "a4-landscape" => tl!("A4 landscape sheet"),
        "a3-portrait" => tl!("A3 portrait sheet"),
        "a3-landscape" => tl!("A3 landscape sheet"),
        "a2-portrait" => tl!("A2 portrait sheet"),
        "a2-landscape" => tl!("A2 landscape sheet"),
        "figure-1col" => tl!("Journal figure, 1 column (90 mm)"),
        "figure-1.5col" => tl!("Journal figure, 1.5 columns (140 mm)"),
        "figure-2col" => tl!("Journal figure, 2 columns (190 mm)"),
        "thesis-full" => tl!("Thesis figure, full page (160 × 225 mm)"),
        "thesis-half" => tl!("Thesis figure, half page (160 × 110 mm)"),
        _ => return None,
    })
}

fn project_group(app: &mut RustGisApp, ui: &mut Ui) {
    group(ui, "Project", |ui| {
        big(ui, app, "new_map", "New\nMap", "Add a new map to the project.", "map.new", Value::Null, false, false);
        big_menu(ui, app, "new_layout", "New\nLayout", "Add a page layout for printing and exporting the map.", |ui, app| {
            for (id, label, w, h) in rustgis_engine::map::layout::PAGES {
                let text = format!("{} ({w:.0} × {h:.0} mm)", tl!(*label));
                if ui.button(text).clicked() {
                    let _ = app.run("layout.new", json!({"page": id}));
                    app.ui.tab = "Publish".into();
                    ui.close();
                }
            }
            // A finished sheet or figure of the map view, its texts to fill in on the page.
            ui.menu_button(tl!("Publication Layout"), |ui| {
                for profile in rustgis_engine::cmd::layout::PUBLICATION_PROFILES {
                    let label = publication_label(profile.id).unwrap_or_else(|| tl!(profile.label));
                    if profile.sheet {
                        // A sheet is arranged in one of the styles; a figure leaves that to its caption.
                        ui.menu_button(label, |ui| {
                            for (style, name) in rustgis_engine::cmd::layout::PUBLICATION_STYLES {
                                let (name, about) = publication_style(style).unwrap_or_else(|| (tl!(*name), ""));
                                if ui.button(name).on_hover_text(about).clicked() {
                                    publication_layout(app, profile, Some(style));
                                    ui.close();
                                }
                            }
                        });
                    } else if ui.button(label).clicked() {
                        publication_layout(app, profile, None);
                        ui.close();
                    }
                }
            });
            ui.separator();
            if ui.button(tl!("Blank Letter page")).clicked() {
                let _ = app.run("layout.new", json!({"template": "blank"}));
                app.ui.tab = "Publish".into();
                ui.close();
            }
            menu_item(ui, app, "open", "Import Layout…", "ui.importLayout", Value::Null);
        });
    });
}

fn report_group(app: &mut RustGisApp, ui: &mut Ui) {
    group(ui, "Report", |ui| {
        big(
            ui,
            app,
            "table",
            "New\nReport",
            "A report listing the selected layer's records, page after page.",
            "ui.newReport",
            Value::Null,
            false,
            false,
        );
        col(ui, |ui| {
            small_menu(
                ui,
                app,
                "new_layout",
                "Report Section",
                "Add pages, layouts, related reports and related records to the report.",
                |ui, app| {
                    menu_item(ui, app, "new_layout", "Supplemental Page", "report.supplementalPage", Value::Null);
                    menu_item(
                        ui,
                        app,
                        "new_layout",
                        "Supplemental Page After",
                        "report.supplementalPage",
                        json!({"title": "Notes", "end": true}),
                    );
                    menu_item(ui, app, "new_layout", "New Template (a layout page)", "report.newTemplate", Value::Null);
                    let layouts: Vec<String> = app.session.project.layouts.iter().map(|l| l.name.clone()).collect();
                    ui.menu_button(tl!("Existing Template"), |ui| {
                        for l in layouts {
                            menu_item(ui, app, "", &l, "report.existingTemplate", json!({"layout": l}));
                        }
                        menu_item(ui, app, "open", "Layout File…", "ui.reportLayoutFile", Value::Null);
                    });
                    let active = rustgis_engine::cmd::report::index(&app.session, &Value::Null).ok();
                    let others: Vec<String> = app
                        .session
                        .project
                        .reports
                        .iter()
                        .enumerate()
                        .filter(|(j, _)| Some(*j) != active)
                        .map(|(_, r)| r.name.clone())
                        .collect();
                    ui.menu_button(tl!("Related Report"), |ui| {
                        if others.is_empty() {
                            ui.label(egui::RichText::new(tl!("No other reports")).weak());
                        }
                        for o in others {
                            menu_item(ui, app, "", &o, "report.relatedReport", json!({"related": o}));
                        }
                    });
                    let rels: Vec<String> =
                        app.session.project.maps.iter().flat_map(|m| m.editing.relationships.iter().map(|r| r.name.clone())).collect();
                    ui.menu_button(tl!("Subreport"), |ui| {
                        if rels.is_empty() {
                            ui.label(egui::RichText::new(tl!("No relationships (Data Design › Relationships)")).weak());
                        }
                        for r in rels {
                            menu_item(ui, app, "", &r, "report.subreport", json!({"relationship": r}));
                        }
                    });
                },
            );
            small_menu(
                ui,
                app,
                "field",
                "Data",
                "Fields, statistics, groups, pictures, attachments and charts in the report.",
                |ui, app| {
                    let fields: Vec<String> = rustgis_engine::cmd::report::index(&app.session, &Value::Null)
                        .ok()
                        .and_then(|i| rustgis_engine::cmd::report::layer_of(&app.session, i).ok())
                        .map(|l| l.dataset.fields.iter().map(|f| f.name.clone()).collect())
                        .unwrap_or_default();
                    let field_menu = |ui: &mut Ui, app: &mut RustGisApp, label: &str, id: &str, key: &str| {
                        ui.menu_button(tl!(label), |ui| {
                            for f in &fields {
                                menu_item(ui, app, "", f, id, json!({key: f}));
                            }
                        });
                    };
                    field_menu(ui, app, "Field", "report.field", "field");
                    menu_item(ui, app, "field", "Autofill Fields", "report.autofill", Value::Null);
                    ui.menu_button(tl!("Statistics"), |ui| {
                        for f in &fields {
                            ui.menu_button(f.as_str(), |ui| {
                                for k in ["count", "sum", "mean", "min", "max"] {
                                    menu_item(ui, app, "", k, "report.statistics", json!({"field": f, "kind": k}));
                                }
                            });
                        }
                    });
                    field_menu(ui, app, "Group", "report.group", "field");
                    field_menu(ui, app, "Attachments", "report.attachments", "field");
                    field_menu(ui, app, "Dynamic Picture", "report.dynamicPicture", "field");
                    field_menu(ui, app, "Chart Frame", "report.chart", "category");
                },
            );
            small(
                ui,
                app,
                "pane",
                "Report Pane",
                "Preview and set up the report.",
                "ui.pane.report",
                Value::Null,
                app.ui.right == Some(RightPane::Report),
            );
        });
        small(
            ui,
            app,
            "open",
            "Import Report",
            "Add a report file, or a report template for the selected layer.",
            "ui.openItem",
            json!({"kind": "report"}),
            false,
        );
    });
}

fn presentation_group(app: &mut RustGisApp, ui: &mut Ui) {
    group(ui, "Presentation", |ui| {
        big(
            ui,
            app,
            "web_map",
            "New\nPresentation",
            "Pages of maps, text, images and videos, shown one after another.",
            "ui.newPresentation",
            Value::Null,
            false,
            false,
        );
        col(ui, |ui| {
            small_menu(ui, app, "new_layout", "Page", "Add a page to the presentation.", |ui, app| {
                menu_item(ui, app, "web_map", "Map (the current view)", "presentation.mapPage", Value::Null);
                menu_item(ui, app, "new_layout", "Blank", "presentation.blankPage", json!({"title": "Title"}));
                menu_item(ui, app, "picture", "Image…", "ui.presentationPicture", Value::Null);
                menu_item(ui, app, "export_map", "Video…", "ui.presentationVideo", Value::Null);
            });
            small(
                ui,
                app,
                "open",
                "Import Presentation",
                "Add a presentation file.",
                "ui.openItem",
                json!({"kind": "presentation"}),
                false,
            );
            small(
                ui,
                app,
                "pane",
                "Presentation Pane",
                "Preview, edit and play the presentation.",
                "ui.pane.presentation",
                Value::Null,
                app.ui.right == Some(RightPane::Presentation),
            );
        });
    });
}

fn notebook_group(app: &mut RustGisApp, ui: &mut Ui) {
    group(ui, "Notebook", |ui| {
        big(ui, app, "python", "New\nNotebook", "A notebook of text and command cells.", "ui.newNotebook", Value::Null, false, false);
        col(ui, |ui| {
            small(
                ui,
                app,
                "open",
                "Add and Open",
                "Open a RustGIS or Jupyter notebook.",
                "ui.openItem",
                json!({"kind": "notebook"}),
                false,
            );
            small(
                ui,
                app,
                "pane",
                "Notebook Pane",
                "Edit and run the notebook.",
                "ui.pane.notebook",
                Value::Null,
                app.ui.right == Some(RightPane::Notebook),
            );
        });
    });
}

fn layer_templates_group(app: &mut RustGisApp, ui: &mut Ui) {
    group(ui, "Layer Templates", |ui| {
        big_menu(
            ui,
            app,
            "graphics_layer",
            "Layer\nTemplates",
            "Add an empty layer with a ready-made schema, to draw into.",
            layer_templates_gallery,
        );
    });
}

pub(crate) fn layer_templates_gallery(ui: &mut Ui, app: &mut RustGisApp) {
    for (id, label) in [
        ("points", "Points"),
        ("lines", "Lines"),
        ("polygons", "Polygons"),
        ("places", "Places (name, category, address, rating)"),
        ("routes", "Routes (name, surface, length)"),
        ("areas", "Areas (name, land use, area)"),
        ("observations", "Observations (observer, date, species, count)"),
    ] {
        menu_item(ui, app, "add_data", label, "layer.template", json!({"template": id}));
    }
}

fn styles_group(app: &mut RustGisApp, ui: &mut Ui) {
    group(ui, "Styles", |ui| {
        big_menu(ui, app, "symbology", "Styles", "Saved symbology: apply it to the selected layer, or save the layer's.", |ui, app| {
            let styles: Vec<(String, String)> =
                app.session.project.styles.iter().map(|t| (t.name.clone(), t.geometry.label().to_string())).collect();
            if styles.is_empty() {
                ui.label(egui::RichText::new(tl!("No styles yet")).weak());
            }
            for (n, g) in styles {
                menu_item(ui, app, "symbology", &format!("{n} ({})", tl!(&g)), "style.apply", json!({"name": n}));
            }
            ui.separator();
            menu_item(ui, app, "symbology", "New Style From Layer", "style.new", Value::Null);
            menu_item(ui, app, "open", "Add Style File…", "ui.openItem", json!({"kind": "styles"}));
            menu_item(ui, app, "open", "Import Styles From…", "ui.openItem", json!({"kind": "importStyles"}));
            menu_item(ui, app, "open", "Add Color Ramp File…", "ui.openItem", json!({"kind": "ramps"}));
        });
    });
}

fn graphics_group(app: &mut RustGisApp, ui: &mut Ui) {
    group(ui, "Graphics", |ui| {
        let tools: [&[(&str, &str, &str, &str)]; 4] = [
            &[
                ("g_point", "Point", "Click to place a point graphic.", "graphicPoint"),
                ("g_line", "Line", "Click the vertices of a line; double-click to finish.", "graphicLine"),
                ("g_curve", "Curve", "Click points a smooth curve passes through; double-click to finish.", "graphicCurve"),
            ],
            &[
                ("g_freehand", "Freehand", "Drag to draw a freehand line.", "graphicFreehand"),
                ("g_polygon", "Polygon", "Click the corners of a polygon; double-click to close it.", "graphicPolygon"),
                ("g_lasso", "Lasso", "Drag to draw a freehand polygon.", "graphicLasso"),
            ],
            &[
                ("g_rect", "Rectangle", "Drag to draw a rectangle.", "graphicRectangle"),
                ("g_circle", "Circle", "Drag from the centre to draw a circle.", "graphicCircle"),
                ("g_ellipse", "Ellipse", "Drag a box to draw an ellipse.", "graphicEllipse"),
            ],
            &[("keep_tool", "Keep Tool", "Keep the last drawing tool active after each graphic.", "")],
        ];
        for column in tools {
            col(ui, |ui| {
                for (icon, label, desc, code) in column {
                    if code.is_empty() {
                        let on = app.session.keep_tool;
                        small(ui, app, icon, label, desc, "graphics.keepTool", Value::Null, on);
                        continue;
                    }
                    let checked = app.session.tool.code() == *code;
                    if small(ui, app, icon, label, desc, "tool.set", json!({"tool": code}), checked).clicked() {
                        app.map.cancel_sketch();
                    }
                }
            });
        }
    });
}

fn graphics_and_text_group(app: &mut RustGisApp, ui: &mut Ui) {
    group(ui, "Graphics and Text", |ui| {
        let text_tool = app.session.tool.code() == "graphicText";
        let dynamic = app.ui.dynamic_text.is_some();
        let text = json!({"tool": "graphicText"});
        if big(ui, app, "g_text", "Text", "Click the map to place text.", "tool.set", text, text_tool && !dynamic, false).clicked() {
            app.map.cancel_sketch();
        }
        // A menu of the values, as in Publish › Text: the one picked starts the text placed.
        let resp = big(
            ui,
            app,
            "dynamic_text",
            "Dynamic\nText",
            "Pick a live value, such as the scale or date, then click the map to place it.",
            "ui.menu",
            Value::Null,
            text_tool && dynamic,
            true,
        );
        egui::Popup::menu(&resp).show(|ui| {
            ui.set_min_width(200.0);
            for &(token, what) in rustgis_engine::map::graphics::TOKENS {
                if ui.button(format!("{}  {token}", tl!(what))).clicked() {
                    app.map.cancel_sketch();
                    let _ = app.run("ui.dynamicText", json!({"text": token}));
                    ui.close();
                }
            }
        });
        if big(
            ui,
            app,
            "picture",
            "Picture",
            "Drag a rectangle, then pick a PNG or JPEG image to place in it.",
            "tool.set",
            json!({"tool": "graphicPicture"}),
            app.session.tool.code() == "graphicPicture",
            false,
        )
        .clicked()
        {
            app.map.cancel_sketch();
        }
    });
}

fn thumbnail_group(app: &mut RustGisApp, ui: &mut Ui) {
    group(ui, "Thumbnail", |ui| {
        col(ui, |ui| {
            small(
                ui,
                app,
                "export_map",
                "Create Thumbnail",
                "The project's thumbnail, from the map view.",
                "project.thumbnail",
                json!({"create": true}),
                app.session.project.thumbnail.is_some(),
            );
            small(ui, app, "open", "Import Thumbnail", "The project's thumbnail, from an image.", "ui.importThumbnail", Value::Null, false);
        });
        col(ui, |ui| {
            small(
                ui,
                app,
                "map",
                "Map Thumbnail",
                "This map's thumbnail, from the view.",
                "map.thumbnail",
                json!({"create": true}),
                app.session.map().thumbnail.is_some(),
            );
            small(ui, app, "open", "Import", "This map's thumbnail, from an image.", "ui.importThumbnail", json!({"map": true}), false);
        });
    });
}

/// Commands the Quick Access Toolbar can show: (id, label). Their icons are in [`quick_icon`].
pub const QUICK: &[(&str, &str)] = &[
    ("ui.saveAs", "Save As"),
    ("ui.exportMap", "Export Map"),
    ("ui.printMap", "Print Map"),
    ("table.open", "Attribute Table"),
    ("map.zoomFull", "Full Extent"),
    ("select.clear", "Clear Selection"),
    ("ui.pane.geoprocessing", "Tools Pane"),
    ("ui.commandSearch", "Command Search"),
];

/// The icon of a Quick Access command.
pub fn quick_icon(id: &str) -> &'static str {
    match id {
        "ui.saveAs" => "save_as",
        "ui.exportMap" => "export_map",
        "ui.printMap" => "print",
        "table.open" => "table",
        "map.zoomFull" => "zoom_full",
        "select.clear" => "clear_sel",
        "ui.pane.geoprocessing" => "toolbox",
        _ => "search",
    }
}

/// Analysis gallery entries: (tool id, icon, caption).
const GALLERY: &[(&str, &str, &str)] = &[
    ("buffer", "buffer", "Buffer"),
    ("clip", "clip", "Clip"),
    ("erase", "erase", "Erase"),
    ("intersect", "intersect", "Intersect"),
    ("dissolve", "dissolve", "Dissolve"),
    ("countPoints", "count", "Summarize\nWithin"),
    ("convexHull", "hull", "Bounding\nGeometry"),
    ("featureToPoint", "centroid", "Feature\nTo Point"),
    ("simplify", "simplify", "Simplify"),
    ("calculateGeometry", "calc_geom", "Calculate\nGeometry"),
    ("summaryStatistics", "stats", "Summary\nStatistics"),
    ("exportFeatures", "export_layer", "Export\nFeatures"),
    ("idwInterpolation", "relief", "IDW\nInterpolation"),
];

fn analysis_groups(app: &mut RustGisApp, ui: &mut Ui) {
    group(ui, "Geoprocessing", |ui| {
        big(
            ui,
            app,
            "history",
            "History",
            "See the tools you've run.",
            "ui.pane.history",
            Value::Null,
            app.ui.right == Some(RightPane::History),
            false,
        );
        big(
            ui,
            app,
            "toolbox",
            "Tools",
            "Find and run geoprocessing tools.",
            "ui.pane.geoprocessing",
            Value::Null,
            app.ui.right == Some(RightPane::Geoprocessing),
            false,
        );
        big(
            ui,
            app,
            "modelbuilder",
            "Model\nBuilder",
            "Draw a model: chain tools from input layers to the results to keep.",
            "ui.modelBuilder",
            Value::Null,
            app.mb.shown(),
            false,
        );
        col(ui, |ui| {
            small(
                ui,
                app,
                "python",
                "Script",
                "Run commands from text, one per line.",
                "ui.pane.script",
                Value::Null,
                app.ui.right == Some(RightPane::Script),
            );
            small(
                ui,
                app,
                "def_query",
                "SQL",
                "Query files, web addresses and databases with DuckDB Spatial; add results to the map.",
                "ui.pane.sql",
                Value::Null,
                app.ui.right == Some(RightPane::Sql),
            );
            small(
                ui,
                app,
                "environments",
                "Environments",
                "Limit where tools work, and the cell size of new rasters.",
                "ui.pane.workflows",
                json!({"section": "environments"}),
                false,
            );
        });
    });
    // Tasks (guided steps, for teaching and repeated work) belong with the analysis tools, not on
    // Home, which keeps what most users need.
    group(ui, "Tasks", |ui| {
        col(ui, |ui| {
            small(ui, app, "run", "Tasks", "Step through a task.", "ui.pane.tasks", Value::Null, app.ui.right == Some(RightPane::Tasks));
            small_menu(ui, app, "run", "Task", "Tasks: guided steps that each run a command.", |ui, app| {
                let record = if app.ws.recording.is_some() { "Stop Recording" } else { "Record Task" };
                menu_item(ui, app, "edit", record, "ui.taskRecord", json!({}));
                menu_item(ui, app, "history", "New Task From History", "task.new", json!({"name": "Workflow", "fromHistory": true}));
                menu_item(ui, app, "open", "Import Task…", "ui.openItem", json!({"kind": "task"}));
                let tasks: Vec<String> = app.session.project.tasks.iter().map(|t| t.name.clone()).collect();
                if !tasks.is_empty() {
                    ui.separator();
                }
                for t in tasks {
                    if ui.button(crate::i18n::fmt("Run {name}", &[("name", &t)])).clicked() {
                        let _ = app.run("task.run", json!({"task": t}));
                        app.map.invalidate();
                        ui.close();
                    }
                }
            });
        });
    });
    group(ui, "AI", |ui| {
        big(
            ui,
            app,
            "assistant",
            "AI\nAssistant",
            "Ask about your data, or tell the assistant what to do with the map.",
            "ui.pane.assistant",
            Value::Null,
            app.ui.right == Some(RightPane::Assistant),
            false,
        );
    });
    group(ui, "Portal", |ui| {
        for (kind, icon, label, desc) in [
            ("feature", "buffer", "Feature\nAnalysis", "Tools that analyse features."),
            ("raster", "relief", "Raster\nAnalysis", "Tools that analyse rasters."),
        ] {
            big_menu(ui, app, icon, label, desc, |ui, app| {
                if let Ok(Value::Array(tools)) = rustgis_engine::cmd::workflows::gallery_list(kind) {
                    for t in tools {
                        let (id, name, cmd) = (
                            t["id"].as_str().unwrap_or("").to_string(),
                            t["name"].as_str().unwrap_or("").to_string(),
                            t["command"].as_str().unwrap_or(""),
                        );
                        if cmd == "gp.run" {
                            menu_item(ui, app, "tools", &name, "ui.gp.open", json!({"tool": id}));
                        } else {
                            let section = match id.as_str() {
                                "raster.function" => "rasterFunctions",
                                "analysis.suitability" => "suitability",
                                "analysis.viewshed" => "visibility",
                                _ => "interpolate",
                            };
                            menu_item(ui, app, "relief", &name, "ui.pane.workflows", json!({"section": section}));
                        }
                    }
                }
            });
        }
    });
    group(ui, "Workflows", |ui| {
        big_menu(
            ui,
            app,
            "geoprocessing",
            "Workflows",
            "Guided analysis: suitability, visibility, networks, interpolation and more.",
            |ui, app| {
                for (id, title) in crate::analysispane::SECTIONS
                    .iter()
                    .filter(|s| !matches!(s.0, "rasterFunctions" | "functionEditor" | "environments" | "models"))
                {
                    menu_item(ui, app, "geoprocessing", title, "ui.pane.workflows", json!({"section": id}));
                }
            },
        );
    });
    group(ui, "Raster", |ui| {
        col(ui, |ui| {
            small(
                ui,
                app,
                "relief",
                "Raster Functions",
                "Slope, aspect, hillshade, NDVI, band math, thresholds.",
                "ui.pane.workflows",
                json!({"section": "rasterFunctions"}),
                false,
            );
            small(
                ui,
                app,
                "bands",
                "Function Editor",
                "Chain raster functions.",
                "ui.pane.workflows",
                json!({"section": "functionEditor"}),
                false,
            );
        });
    });
    group(ui, "Tools", |ui| {
        let t = Tokens::get(ui.ctx());
        // A framed gallery like a desktop GIS analysis gallery.
        let r = ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
            ui.spacing_mut().item_spacing = vec2(1.0, 0.0);
            for (id, icon, label) in GALLERY {
                let tool = rustgis_engine::analysis::tool(id);
                let desc = tool.map(|t| t.summary).unwrap_or("");
                big(ui, app, icon, label, desc, "ui.gp.open", json!({"tool": id}), false, false);
            }
        });
        ui.painter().rect_stroke(r.response.rect.expand(1.0), CornerRadius::same(6), Stroke::new(1.0, t.border), egui::StrokeKind::Outside);
    });
    group(ui, "Selection", |ui| {
        big(
            ui,
            app,
            "select_attr",
            "By\nAttributes",
            "Select features with a SQL expression.",
            "ui.dialog",
            json!({"name": "selectByAttributes"}),
            false,
            false,
        );
        big(
            ui,
            app,
            "select_loc",
            "By\nLocation",
            "Select features by their spatial relationship.",
            "ui.gp.open",
            json!({"tool": "selectByLocation"}),
            false,
            false,
        );
    });
}

fn view_tab(app: &mut RustGisApp, ui: &mut Ui) {
    group(ui, "Display", |ui| {
        let g = app.session.map().graticule;
        big(ui, app, "graticule", "Graticule", "Show latitude/longitude lines on the basemap.", "map.graticule", Value::Null, g, false);
        let dark = app.ui.dark;
        big(ui, app, "dark", "Dark\nMode", "Switch between light and dark themes.", "ui.theme", Value::Null, dark, false);
        let gpu = app.map.gpu_enabled;
        big(
            ui,
            app,
            "map",
            "GPU\nRendering",
            "Draw the map on the GPU (beta; the globe, view effects and colour-vision simulation stay on the CPU renderer).",
            "ui.map.gpu",
            Value::Null,
            gpu,
            false,
        );
    });
    group(ui, "View", |ui| {
        let on = app.session.map().projection.is_globe();
        big(
            ui,
            app,
            "globe_view",
            "Globe",
            "Show the map as a 3D globe, or as a flat map again.",
            "map.projection",
            Value::Null,
            on,
            false,
        );
        let current = app.session.map().projection.code();
        let local = current.starts_with("local:");
        big_menu(ui, app, "basemap", "Projection", "Choose how the flat map lays out the Earth.", move |ui, app| {
            let mut items: Vec<(String, String)> = vec![("mercator".into(), "Web Mercator".into())];
            items.extend(rustgis_engine::map::projection::PRESETS.iter().map(|(id, label, _)| (id.to_string(), label.to_string())));
            for (id, label) in items {
                if ui.add(egui::Button::selectable(is_projection(&current, &id), tl!(label.as_str()))).clicked() {
                    if app.run("map.projection", json!({"type": id})).is_ok() {
                        let _ = app.run("map.zoomFull", Value::Null);
                    }
                    ui.close();
                }
            }
            ui.separator();
            if ui.button(tl!("Coordinate System…")).clicked() {
                let _ = app.run("ui.dialog", json!({"name": "coordinateSystem"}));
                ui.close();
            }
        });
        col(ui, |ui| {
            small(ui, app, "globe_view", "Global", "A globe scene.", "map.viewMode", json!({"mode": "global"}), on);
            if rustgis_engine::map::projection::local_views_supported() {
                small(
                    ui,
                    app,
                    "map",
                    "Local",
                    "A flat scene centred on the view (azimuthal equidistant).",
                    "map.viewMode",
                    json!({"mode": "local"}),
                    local,
                );
            }
        });
    });
    let v = app.session.map().view.clone();
    group(ui, "Effects", |ui| {
        col(ui, |ui| {
            let sun = v.effects.sun_time.is_some();
            small_menu(ui, app, "brightness", "Illumination", "Day and night shading for a time.", move |ui, app| {
                if ui.radio(!sun, tl!("Off")).clicked() {
                    let _ = app.run("map.effects", json!({"sunTime": null}));
                    ui.close();
                }
                if ui.button(tl!("Now")).clicked() {
                    let _ = app.run("map.effects", json!({"sunTime": "now"}));
                    ui.close();
                }
                let now = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
                let day = (now / 86_400.0).floor() * 86_400.0;
                for (label, h) in [("06:00 UTC", 6.0), ("12:00 UTC", 12.0), ("18:00 UTC", 18.0), ("00:00 UTC", 0.0)] {
                    if ui.button(label).clicked() {
                        let _ = app.run("map.effects", json!({"sunTime": day + h * 3600.0}));
                        ui.close();
                    }
                }
            });
            let wx = v.effects.weather;
            small_menu(ui, app, "graticule", "Weather", "Haze, fog, rain or snow over the map.", move |ui, app| {
                for (label, code, w) in [
                    ("None", "none", Weather::None),
                    ("Haze", "haze", Weather::Haze),
                    ("Fog", "fog", Weather::Fog),
                    ("Rain", "rain", Weather::Rain),
                    ("Snow", "snow", Weather::Snow),
                ] {
                    if ui.radio(wx == w, tl!(label)).clicked() {
                        let _ = app.run("map.effects", json!({"weather": code}));
                        ui.close();
                    }
                }
            });
        });
        col(ui, |ui| {
            let (vig, edges, tilt, grain) = (v.effects.vignette, v.effects.edges, v.effects.tilt_shift, v.effects.grain);
            let stars = v.effects.stars;
            small_form_menu(ui, app, "eye", "Visual Effect", "Vignette, edge outlines and the stars around the globe.", move |ui, app| {
                if ui.checkbox(&mut vig.clone(), tl!("Vignette")).clicked() {
                    let _ = app.run("map.effects", json!({"vignette": !vig}));
                }
                if ui.checkbox(&mut edges.clone(), tl!("Edges")).clicked() {
                    let _ = app.run("map.effects", json!({"edges": !edges}));
                }
                if ui.checkbox(&mut stars.clone(), tl!("Stars")).clicked() {
                    let _ = app.run("map.effects", json!({"stars": !stars}));
                }
            });
            small_form_menu(ui, app, "zoom_box", "Camera Effect", "Tilt-shift blur and film grain.", move |ui, app| {
                if ui.checkbox(&mut tilt.clone(), tl!("Tilt-shift")).clicked() {
                    let _ = app.run("map.effects", json!({"tiltShift": !tilt}));
                }
                if ui.checkbox(&mut grain.clone(), tl!("Film grain")).clicked() {
                    let _ = app.run("map.effects", json!({"grain": !grain}));
                }
            });
        });
        col(ui, |ui| {
            let fx = v.effects.clone();
            small_menu(
                ui,
                app,
                "contrast",
                "Postprocessing",
                "Brightness, contrast, saturation and sepia for the whole map.",
                move |ui, app| {
                    for (label, key, mut val, lo) in [
                        ("Brightness", "brightness", fx.brightness, -100.0),
                        ("Contrast", "contrast", fx.contrast, -100.0),
                        ("Saturation", "saturation", fx.saturation, -100.0),
                        ("Sepia", "sepia", fx.sepia, 0.0),
                    ] {
                        ui.horizontal(|ui| {
                            let input_label = ui.label(tl!(label));
                            let r = ui.add(egui::Slider::new(&mut val, lo..=100.0)).labelled_by(input_label.id);
                            if r.drag_stopped() || (r.changed() && !r.dragged()) {
                                let _ = app.run("map.effects", json!({key: val}));
                            }
                        });
                    }
                    if ui.button(tl!("Reset Effects")).clicked() {
                        let _ = app.run("map.effects", json!({"reset": true}));
                        ui.close();
                    }
                },
            );
            let cv = v.color_vision;
            small_menu(ui, app, "eye", "Color Vision", "See the map as people with colour vision deficiencies do.", move |ui, app| {
                for c in rustgis_engine::map::view::ColorVision::ALL {
                    if ui.radio(cv == c, tl!(c.label())).clicked() {
                        let _ = app.run("map.colorVision", json!({"mode": c.code()}));
                        ui.close();
                    }
                }
            });
        });
    });
    group(ui, "Scene", |ui| {
        let on = v.terrain.is_some();
        // The active DEM by name, so a recorded step replays on it, not on whatever is active then.
        let dem =
            app.session.active_layer.and_then(|id| app.session.map().layer(id)).filter(|l| l.raster.is_some()).map(|l| l.name.clone());
        big(
            ui,
            app,
            "relief",
            "Terrain",
            "Drape the map over the heights of the active raster layer (a DEM, in metres), and tilt the map to see the relief. Click again to flatten it.",
            "map.terrain",
            match (on, dem) {
                (true, _) => json!({"off": true}),
                (false, Some(layer)) => json!({"layer": layer}),
                (false, None) => json!({}),
            },
            on,
            false,
        );
        col(ui, |ui| {
            small(
                ui,
                app,
                "polygon_tool",
                "Wireframe",
                "Draw polygons as outlines only.",
                "map.drawingMode",
                json!({"mode": if v.wireframe { "standard" } else { "wireframe" }}),
                v.wireframe,
            );
            let pr = app.session.active_layer.and_then(|id| app.session.map().layer(id)).map_or(0, |l| l.priority);
            small(
                ui,
                app,
                "up",
                "Raise Priority",
                "Draw the selected layer above others.",
                "layer.priority",
                json!({"value": pr + 1}),
                pr > 0,
            );
            small(
                ui,
                app,
                "down",
                "Lower Priority",
                "Draw the selected layer below others.",
                "layer.priority",
                json!({"value": pr - 1}),
                pr < 0,
            );
        });
        col(ui, |ui| {
            let clip = v.clip_distance;
            small_menu(ui, app, "scale_range", "View Clipping", "Hide features far from the view's centre.", move |ui, app| {
                if ui.radio(clip.is_none(), tl!("None")).clicked() {
                    let _ = app.run("map.clipping", json!({"mode": "none"}));
                    ui.close();
                }
                for km in [100.0, 500.0, 1000.0, 3000.0] {
                    if ui
                        .radio(clip == Some(km * 1000.0), crate::measure_units::distance(km * 1000.0, app.ui.measurement_imperial))
                        .clicked()
                    {
                        let _ = app.run("map.clipping", json!({"mode": "distance", "distance": km * 1000.0}));
                        ui.close();
                    }
                }
            });
            small(
                ui,
                app,
                "dra",
                "Speed Display",
                "Faster drawing: no antialiasing.",
                "map.displayType",
                json!({"mode": if v.fast { "quality" } else { "speed" }}),
                v.fast,
            );
        });
    });
    group(ui, "Windows", |ui| {
        big(
            ui,
            app,
            "contents",
            "Layers",
            "Show or hide the Layers pane.",
            "ui.pane.contents",
            Value::Null,
            app.ui.left == Some(crate::LeftPane::Contents),
            false,
        );
        big(
            ui,
            app,
            "catalog",
            "Browser\nPane",
            "Show or hide the Browser pane.",
            "ui.pane.catalog",
            Value::Null,
            app.ui.left == Some(crate::LeftPane::Catalog),
            false,
        );
        big(
            ui,
            app,
            "web_map",
            "Data\nSources",
            "Show or hide the Data Sources pane: favourites, recent data, saved services, databases and STAC APIs, and a library of public sources.",
            "ui.pane.sources",
            Value::Null,
            app.ui.left == Some(crate::LeftPane::Sources),
            false,
        );
        big(
            ui,
            app,
            "geoprocessing",
            "Tools\nPane",
            "Show or hide the Tools pane.",
            "ui.pane.geoprocessing",
            Value::Null,
            app.ui.right == Some(RightPane::Geoprocessing),
            false,
        );
        col(ui, |ui| {
            small(
                ui,
                app,
                "symbology",
                "Symbology",
                "Show the Symbology pane for the selected layer.",
                "ui.pane.symbology",
                Value::Null,
                app.ui.right == Some(RightPane::Symbology),
            );
            small(
                ui,
                app,
                "history",
                "History",
                "Geoprocessing history.",
                "ui.pane.history",
                Value::Null,
                app.ui.right == Some(RightPane::History),
            );
            small(
                ui,
                app,
                "identify",
                "Details",
                "Details of the selected layer.",
                "ui.pane.details",
                Value::Null,
                app.ui.right == Some(RightPane::Details),
            );
        });
        col(ui, |ui| {
            small(
                ui,
                app,
                "assistant",
                "AI Assistant",
                "Show or hide the AI Assistant.",
                "ui.pane.assistant",
                Value::Null,
                app.ui.right == Some(RightPane::Assistant),
            );
            small(
                ui,
                app,
                "bookmark",
                "Bookmarks",
                "Show or hide the Bookmarks pane: every map's bookmarks.",
                "ui.pane.bookmarks",
                Value::Null,
                app.ui.right == Some(RightPane::Bookmarks),
            );
            small(
                ui,
                app,
                "label",
                "Labeling",
                "Show the Labeling pane for the selected layer.",
                "ui.pane.labeling",
                Value::Null,
                app.ui.right == Some(RightPane::Labeling),
            );
        });
        small(
            ui,
            app,
            "table",
            "Attribute Table",
            "Open the selected layer's table.",
            "table.open",
            Value::Null,
            app.session.table_layer.is_some(),
        );
    });
    group(ui, "Navigation", |ui| {
        big(ui, app, "zoom_world", "World", "Zoom to the whole world.", "map.zoomWorld", Value::Null, false, false);
        col(ui, |ui| {
            small(
                ui,
                app,
                "explore",
                "Navigator",
                "On-screen compass, zoom slider and pan pad.",
                "map.navigator",
                Value::Null,
                v.navigator,
            );
            small(
                ui,
                app,
                "goto_xy",
                "Camera",
                "See and set the camera's position and scale.",
                "ui.dialog",
                json!({"name": "camera"}),
                false,
            );
            small(
                ui,
                app,
                "search",
                "Magnifier",
                "A magnifying lens that follows the pointer.",
                "ui.magnifier",
                Value::Null,
                app.map.magnifier,
            );
        });
    });
    group(ui, "Link", |ui| {
        col(ui, |ui| {
            let split = crate::mapview::split_map(app).is_some();
            small(ui, app, "pane_right", "Side by Side", "Show a second map beside this one.", "ui.splitView", Value::Null, split);
            small(ui, app, "zoom_full", "Link Views", "Maps move together.", "view.linkViews", Value::Null, app.session.project.link_views);
            small(
                ui,
                app,
                "goto_xy",
                "Link Cursors",
                "Show the pointer's place on the other map.",
                "ui.linkCursors",
                Value::Null,
                app.ws.link_cursors,
            );
        });
    });
    group(ui, "Workspace", |ui| {
        col(ui, |ui| {
            small(
                ui,
                app,
                "catalog",
                "Browser View",
                "The project's items in place of the map.",
                "ui.catalogView",
                Value::Null,
                app.ws.catalog_view,
            );
            small(ui, app, "run", "Tasks", "Step through a task.", "ui.pane.tasks", Value::Null, app.ui.right == Some(RightPane::Tasks));
            small(
                ui,
                app,
                "history",
                "Workflow Manager",
                "Track jobs and their status.",
                "ui.pane.jobs",
                Value::Null,
                app.ui.right == Some(RightPane::Jobs),
            );
        });
        col(ui, |ui| {
            let sets: Vec<String> = app.ui.pane_sets.iter().map(|x| x.0.clone()).collect();
            small_menu(ui, app, "pane", "Pane Sets", "Save the panes you have open, and switch back to them.", move |ui, app| {
                for n in &sets {
                    if ui.button(n.as_str()).clicked() {
                        let _ = app.run("ui.paneSet", json!({"load": n}));
                        ui.close();
                    }
                }
                ui.separator();
                for n in ["Mapping", "Editing", "Analysis"] {
                    if ui.button(crate::i18n::fmt("Save as {name}", &[("name", tl_raw!(n))])).clicked() {
                        let _ = app.run("ui.paneSet", json!({"save": tl!(n)}));
                        ui.close();
                    }
                }
            });
            small(
                ui,
                app,
                "identify",
                "Item Pop-ups",
                "Show details when hovering items in the Browser view.",
                "ui.itemPopups",
                Value::Null,
                app.ws.item_popups,
            );
        });
    });
    group(ui, "Animation", |ui| {
        let n = app.session.map().animation.len();
        col(ui, |ui| {
            small(
                ui,
                app,
                "bookmark",
                &crate::i18n::fmt("Add ({n})", &[("n", &n.to_string())]),
                "Add the current view as a keyframe.",
                "animation.add",
                Value::Null,
                false,
            );
            small(ui, app, "delete", "Remove", "Remove the last keyframe.", "animation.remove", Value::Null, false);
        });
        col(ui, |ui| {
            small(ui, app, "run", "Play", "Play the animation.", "ui.animationPlay", Value::Null, app.ws.playing.is_some());
            small(ui, app, "export", "Export", "Export the animation as PNG frames (zip).", "ui.exportAnimation", Value::Null, false);
        });
    });
    group(ui, "Time", |ui| {
        big(
            ui,
            app,
            "time_slider",
            "Time\nSlider",
            "Show layers through time: only the features whose date falls in the slider's window, and dated imagery for its date.",
            "ui.timeSlider",
            Value::Null,
            crate::timebar::open(app),
            false,
        );
    });
    group(ui, "Profile", |ui| {
        big(
            ui,
            app,
            "measure",
            "Profile",
            "Elevation profiles along lines, cross sections and more.",
            "ui.pane.profile",
            Value::Null,
            app.ui.right == Some(RightPane::Profile),
            false,
        );
        // A Zarr series is read by the desktop app (the web app can't read one yet).
        if !cfg!(target_arch = "wasm32") {
            big(
                ui,
                app,
                "measure",
                "Time\nSeries",
                "A Zarr variable's values through time at points clicked on the map.",
                "ui.pane.timeSeries",
                Value::Null,
                app.ui.right == Some(RightPane::TimeSeries),
                false,
            );
        }
    });
}

/// The Data tab: bringing data in, tables, fields, queries, relationships and export.
fn data_tab(app: &mut RustGisApp, ui: &mut Ui) {
    layer_templates_group(app, ui);
    data_source_group(app, ui);
    if active_layer(app).is_some_and(|l| l.is_raster()) {
        raster_data_groups(app, ui);
    } else {
        feature_data_groups(app, ui);
    }
    offline_group(app, ui);
    feature_cache_group(app, ui);
}

fn data_source_group(app: &mut RustGisApp, ui: &mut Ui) {
    group(ui, "Data Source", |ui| {
        big(
            ui,
            app,
            "open",
            "Repair\nSource",
            "Point the selected layer at its file after the file moved or was renamed.",
            "ui.dialog",
            json!({"name": "repairSource"}),
            false,
            false,
        );
    });
}

/// The Style tab: the drawing of the layer selected in Layers (its symbology, labels and
/// effects), or a hint when none is.
fn style_tab(app: &mut RustGisApp, ui: &mut Ui) {
    match active_layer(app) {
        Some(l) if l.is_raster() => raster_style_groups(app, ui),
        Some(l) if l.geometry_type() != GeometryType::None => feature_style_groups(app, ui),
        _ => layer_hint(ui, "Select a layer in Layers to style it."),
    }
    styles_group(app, ui);
}

/// The Labeling tab (#897): how the map is labelled. The selected feature layer's groups, in the
/// order of the Labeling pane's tabs, then the settings of the whole map, which need no layer.
fn labeling_tab(app: &mut RustGisApp, ui: &mut Ui) {
    match active_layer(app) {
        Some(l) if l.is_raster() => layer_hint(ui, "Labels are drawn for feature layers, not rasters."),
        Some(l) if l.geometry_type() != GeometryType::None => label_groups(app, ui),
        _ => layer_hint(ui, "Select a feature layer in Layers to label it."),
    }
    map_labeling_group(app, ui);
}

/// What a layer-dependent tab shows in place of its layer's groups: why there are none.
fn layer_hint(ui: &mut Ui, text: &str) {
    group(ui, "Layer", |ui| {
        let t = Tokens::get(ui.ctx());
        ui.add_sized(vec2(220.0, CONTENT_H), egui::Label::new(egui::RichText::new(tl!(text)).color(t.text_dim)).wrap());
    });
}

/// The Analyze tab: geoprocessing, workflows, imagery and notebooks.
fn analyze_tab(app: &mut RustGisApp, ui: &mut Ui) {
    analysis_groups(app, ui);
    notebook_group(app, ui);
    imagery_groups(app, ui);
}

/// The Publish tab: the open layout's groups first, then reports, presentations, export and
/// sharing.
fn publish_tab(app: &mut RustGisApp, ui: &mut Ui) {
    layout_groups(app, ui);
    report_group(app, ui);
    presentation_group(app, ui);
    share_groups(app, ui);
    thumbnail_group(app, ui);
}

/// The layer selected in Layers.
fn active_layer(app: &RustGisApp) -> Option<&rustgis_engine::map::Layer> {
    app.session.active_layer.and_then(|id| app.session.map().layer(id))
}

/// Keeps edit commands and transient vertex modes together, with deeper settings in Editing.
fn edit_tab(app: &mut RustGisApp, ui: &mut Ui) {
    let editing = app.session.map().editing.clone();
    let pane = app.ui.right == Some(RightPane::Editing);
    group(ui, "Manage Edits", |ui| {
        big(ui, app, "edit", "Edit", "Turn editing on or off for the map.", "edit.enable", Value::Null, editing.settings.enabled, false);
        big(ui, app, "save", "Save", "Save the project with your edits.", "ui.save", Value::Null, false, false);
        col(ui, |ui| {
            small(ui, app, "undo", "Undo", "Undo the last change.", "edit.undo", Value::Null, false);
            small(ui, app, "redo", "Redo", "Redo the change you undid.", "edit.redo", Value::Null, false);
            small(ui, app, "history", "Status", "Edits since the project was saved.", "ui.pane.editing", Value::Null, pane);
        });
        // A layer loaded from PostGIS for editing saves its edits to its table, not the project
        // (desktop only: the browser doesn't reach databases).
        #[cfg(not(target_arch = "wasm32"))]
        if app.session.map().layers.iter().any(|l| l.postgis.is_some()) {
            col(ui, |ui| {
                small(
                    ui,
                    app,
                    "save",
                    "Save to PostGIS…",
                    "Save a PostGIS layer's edits to its table, unless rows changed there since.",
                    "ui.dialog",
                    json!({"name": "pgSave"}),
                    false,
                );
                small(
                    ui,
                    app,
                    "update",
                    "Discard PostGIS Edits…",
                    "Load a PostGIS layer's rows again as they are in the database, dropping its edits.",
                    "ui.dialog",
                    json!({"name": "pgDiscard"}),
                    false,
                );
            });
        }
        #[cfg(not(target_arch = "wasm32"))]
        if app.session.map().layers.iter().any(|l| l.gpkg.is_some()) {
            col(ui, |ui| {
                small(
                    ui,
                    app,
                    "save",
                    "Save to GeoPackage…",
                    "Save a GeoPackage layer's edits to its table in the file, unless rows changed there since.",
                    "ui.dialog",
                    json!({"name": "gpkgSave"}),
                    false,
                );
                small(
                    ui,
                    app,
                    "update",
                    "Discard GeoPackage Edits…",
                    "Load a GeoPackage layer's rows again as they are in the file, dropping its edits.",
                    "ui.dialog",
                    json!({"name": "gpkgDiscard"}),
                    false,
                );
            });
        }
        col(ui, |ui| {
            small(ui, app, "settings", "Settings", "Editing settings.", "ui.pane.editing", Value::Null, pane);
            small(
                ui,
                app,
                "point_tool",
                "Templates",
                "Feature templates: default values for new features.",
                "ui.pane.editing",
                Value::Null,
                pane,
            );
            small_menu(ui, app, "polygon_tool", "Topology", "Topology rules and the errors they find.", |ui, app| {
                menu_item(ui, app, "settings", "Topology Options…", "ui.pane.editing", Value::Null);
                if ui.button(tl!("Error Inspector (validate)")).clicked() {
                    let _ = app.run("topology.validate", Value::Null);
                    app.ui.open_right(RightPane::Editing);
                    ui.close();
                }
            });
        });
    });
    group(ui, "Snapping", |ui| {
        big(
            ui,
            app,
            "goto_xy",
            "Snapping",
            "Snap new vertices to nearby vertices, edges and line ends.",
            "edit.snapping",
            Value::Null,
            editing.snapping.enabled,
            false,
        );
    });
    group(ui, "Features", |ui| {
        let gt = app.session.active_layer.and_then(|id| app.session.map().layer(id)).map(|l| l.geometry_type());
        let (tool, icon) = match gt {
            Some(GeometryType::Polyline) => (Tool::CreateLine, "line_tool"),
            Some(GeometryType::Polygon) => (Tool::CreatePolygon, "polygon_tool"),
            _ => (Tool::CreatePoint, "point_tool"),
        };
        let checked = app.session.tool.creates();
        let r = big(
            ui,
            app,
            icon,
            "Create",
            "Draw new features in the selected layer. Double-click to finish lines and polygons.",
            "tool.set",
            json!({"tool": tool.code()}),
            checked,
            false,
        );
        if r.clicked() {
            app.map.cancel_sketch();
        }
        big(ui, app, "delete", "Delete", "Delete the selected features.", "edit.delete", Value::Null, false, false);
        let r = big(
            ui,
            app,
            "vertices",
            "Edit\nVertices",
            "Drag the selected features' vertices to move them, click an edge to add one, Delete to remove one.",
            "tool.set",
            json!({"tool": "editVertices"}),
            app.session.tool == Tool::EditVertices,
            false,
        );
        if r.clicked() {
            app.map.cancel_sketch();
        }
        big(
            ui,
            app,
            "calc_geom",
            "Modify",
            "Move, rotate, scale, merge, explode, split or flip the selected features.",
            "ui.pane.editing",
            Value::Null,
            pane,
            false,
        );
        col(ui, |ui| {
            small(
                ui,
                app,
                "vertices",
                "Shared Vertices",
                "Move polygon vertices stored at the same coordinate in this layer, including point-only contacts.",
                "ui.moveSharedVertices",
                Value::Null,
                app.map.move_shared_vertices,
            );
        });
    });
    group(ui, "Tools", |ui| {
        big_menu(ui, app, "tools", "Editor\nTools", "Every editing tool.", |ui, app| {
            for (label, id, p) in [
                ("Merge", "edit.merge", Value::Null),
                ("Explode", "edit.explode", Value::Null),
                ("Flip Line", "edit.flip", Value::Null),
                ("Rotate 90°", "edit.rotate", json!({"angle": 90})),
                ("Scale ×2", "edit.scale", json!({"factor": 2})),
                ("Scale ×½", "edit.scale", json!({"factor": 0.5})),
            ] {
                menu_item(ui, app, "calc_geom", label, id, p);
            }
            if ui.button(tl!("Split (click the line)")).clicked() {
                app.map.split_mode = true;
                ui.close();
            }
            menu_item(ui, app, "vertices", "Edit Vertices", "tool.set", json!({"tool": "editVertices"}));
            menu_item(ui, app, "calc_geom", "Move, Rotate, Scale…", "ui.pane.editing", Value::Null);
        });
    });
    group(ui, "Elevation", |ui| {
        let mode = match &editing.settings.elevation {
            rustgis_engine::map::design::Elevation::None => "None",
            rustgis_engine::map::design::Elevation::Constant(_) => "Constant",
            rustgis_engine::map::design::Elevation::Surface(_) => "Surface",
        };
        small_menu(ui, app, "relief", mode, "Where new features get their elevation (the z field).", |ui, app| {
            if ui.button(tl!("None")).clicked() {
                let _ = app.run("edit.elevation", json!({"mode": "none"}));
                ui.close();
            }
            menu_item(ui, app, "relief", "Constant…", "ui.pane.editing", Value::Null);
            let rasters: Vec<String> = app.session.map().layers.iter().filter(|l| l.is_raster()).map(|l| l.name.clone()).collect();
            for r in rasters {
                if ui.button(crate::i18n::fmt("Surface: {layer}", &[("layer", &r)])).clicked() {
                    let _ = app.run("edit.elevation", json!({"mode": "surface", "surface": r}));
                    ui.close();
                }
            }
        });
    });
    group(ui, "Corrections", |ui| {
        let on = editing.settings.ground_to_grid.is_some();
        big(
            ui,
            app,
            "scale_range",
            "Ground\nTo Grid",
            "Scale and rotate entered distances and directions to the grid.",
            "ui.pane.editing",
            Value::Null,
            on,
            false,
        );
    });
    graphics_group(app, ui);
    graphics_and_text_group(app, ui);
    clipboard_group(app, ui);
}

fn share_groups(app: &mut RustGisApp, ui: &mut Ui) {
    group(ui, "Output", |ui| {
        big(
            ui,
            app,
            "export_map",
            "Export\nMap",
            "Export the map view as a PNG image (with labels and a scale bar).",
            "ui.exportMap",
            Value::Null,
            false,
            false,
        );
        big(
            ui,
            app,
            "export_layer",
            "Export\nFeatures",
            "Export the selected layer to GeoJSON or CSV.",
            "ui.exportLayer",
            Value::Null,
            false,
            false,
        );
        col(ui, |ui| {
            small(
                ui,
                app,
                "print",
                "Print Map",
                "Print the map view (through the system's PDF viewer).",
                "ui.printMap",
                Value::Null,
                false,
            );
            small(
                ui,
                app,
                "copy",
                "Capture To Clipboard",
                "Copy an image of the map to the clipboard.",
                "ui.captureMap",
                Value::Null,
                false,
            );
            small_menu(ui, app, "print", "Report", "Export the report, or save it as a report file or template.", |ui, app| {
                menu_item(ui, app, "print", "Export Report…", "ui.exportReport", Value::Null);
                menu_item(ui, app, "save_as", "Save As Report File…", "ui.saveItem", json!({"kind": "report"}));
                menu_item(ui, app, "save_as", "Save As Report Template…", "ui.saveItem", json!({"kind": "reportTemplate"}));
            });
            small_menu(
                ui,
                app,
                "web_map",
                "Presentation",
                "Export the presentation as a slideshow or PDF, or save it as a file.",
                |ui, app| {
                    menu_item(ui, app, "web_map", "Export Presentation…", "ui.exportPresentation", Value::Null);
                    menu_item(ui, app, "save_as", "Save As Presentation File…", "ui.saveItem", json!({"kind": "presentation"}));
                },
            );
            small(
                ui,
                app,
                "export_layer",
                "Export To 3D",
                "Export the selected layer as a 3D model (OBJ), polygons extruded.",
                "ui.saveItem",
                json!({"kind": "obj"}),
                false,
            );
        });
    });
    group(ui, "Package", |ui| {
        big(
            ui,
            app,
            "package",
            "Project\nPackage",
            "Bundle the project and its data into one file to share.",
            "ui.saveItem",
            json!({"kind": "projectPackage"}),
            false,
            false,
        );
        col(ui, |ui| {
            small(ui, app, "map", "Map", "Bundle the active map and its data.", "ui.saveItem", json!({"kind": "mapPackage"}), false);
            small(
                ui,
                app,
                "map",
                "Mobile Map",
                "Bundle the visible layers, with simplified geometry for phones and tablets.",
                "ui.saveItem",
                json!({"kind": "mobilePackage"}),
                false,
            );
            small(
                ui,
                app,
                "history",
                "Results",
                "Bundle the geoprocessing history with the layers it made.",
                "ui.saveItem",
                json!({"kind": "gpPackage"}),
                false,
            );
        });
    });
    group(ui, "Share As", |ui| {
        big(
            ui,
            app,
            "web_map",
            "Web\nMap",
            "Share the map as a web page that needs no server.",
            "ui.share",
            json!({"kind": "webMap"}),
            false,
            false,
        );
        col(ui, |ui| {
            small(
                ui,
                app,
                "export_layer",
                "Web Layer",
                "Share the selected layer as an OGC API Features collection with its style.",
                "ui.share",
                json!({"kind": "webLayer"}),
                false,
            );
            small(
                ui,
                app,
                "symbology",
                "Web Style",
                "Share the selected layer's symbology as a MapLibre style.",
                "ui.share",
                json!({"kind": "webStyle"}),
                false,
            );
            small(
                ui,
                app,
                "search",
                "Locator",
                "Share the selected layer's names as a place-search locator.",
                "ui.share",
                json!({"kind": "locator"}),
                false,
            );
        });
        col(ui, |ui| {
            small(
                ui,
                app,
                "package",
                "Geodata Service",
                "Share every layer and table as OGC API Features collections.",
                "ui.share",
                json!({"kind": "geodataService"}),
                false,
            );
            small(
                ui,
                app,
                "modelbuilder",
                "Web Tool",
                "Share the last model as a tool file.",
                "ui.share",
                json!({"kind": "webTool"}),
                false,
            );
            small(
                ui,
                app,
                "globe_view",
                "Web Scene",
                "Share the map in 3D (glTF), polygons extruded.",
                "ui.share",
                json!({"kind": "webScene"}),
                false,
            );
        });
        col(ui, |ui| {
            small(
                ui,
                app,
                "graphics_layer",
                "Web Link Chart",
                "Share the map's relationships as a link chart page.",
                "ui.share",
                json!({"kind": "linkChart"}),
                false,
            );
            small(
                ui,
                app,
                "history",
                "Jobs",
                "Shares made, with Manage tools to update them.",
                "ui.pane.shares",
                Value::Null,
                app.ui.right == Some(RightPane::Shares),
            );
            small(
                ui,
                app,
                "modelbuilder",
                "Deep Learning",
                "Package a trained deep learning model.",
                "ui.saveItem",
                json!({"kind": "dlPackage"}),
                false,
            );
        });
    });
    group(ui, "Layout", |ui| {
        big(ui, app, "export", "Export\nLayout", "Export the open layout as PDF or PNG.", "ui.exportLayout", Value::Null, false, false);
        big(
            ui,
            app,
            "print",
            "Print\nLayout",
            "Print the open layout (through the system's PDF viewer).",
            "ui.printLayout",
            Value::Null,
            false,
            false,
        );
    });
    group(ui, "Save As", |ui| {
        big(ui, app, "save_as", "Project", "Save the project under a new name.", "ui.saveAs", Value::Null, false, false);
        big(ui, app, "new_layout", "Layout\nFile", "Save the open layout as a layout file.", "ui.saveLayout", Value::Null, false, false);
        col(ui, |ui| {
            small(
                ui,
                app,
                "map",
                "Map File",
                "Save the active map (with its data) as a map file.",
                "ui.saveItem",
                json!({"kind": "map"}),
                false,
            );
            small(
                ui,
                app,
                "add_data",
                "Layer File",
                "Save the selected layer with its symbology as a layer file.",
                "ui.saveItem",
                json!({"kind": "layer"}),
                false,
            );
            small(
                ui,
                app,
                "symbology",
                "Style Items",
                "Save the project's styles to a style file.",
                "ui.saveItem",
                json!({"kind": "styles"}),
                false,
            );
            small(
                ui,
                app,
                "symbology",
                "Color Ramps",
                "Save the project's custom color ramps to a color ramp file.",
                "ui.saveItem",
                json!({"kind": "ramps"}),
                false,
            );
        });
        col(ui, |ui| {
            small(
                ui,
                app,
                "save_as",
                "Project Template",
                "Save the project's setup, without its features, as a template.",
                "ui.saveItem",
                json!({"kind": "template"}),
                false,
            );
            small(ui, app, "run", "Task Item", "Save the last task as a task file.", "ui.saveItem", json!({"kind": "task"}), false);
        });
    });
}

/// The selected layout element if it's a map frame, else the layout's first frame.
fn layout_frame(app: &RustGisApp) -> Option<u32> {
    let l = app.session.active_layout.and_then(|i| app.session.project.layouts.get(i))?;
    app.layout
        .selected
        .filter(|id| matches!(l.element(*id).map(|e| &e.kind), Some(rustgis_engine::map::layout::ElementKind::MapFrame { .. })))
        .or_else(|| l.first_frame())
}

fn layout_groups(app: &mut RustGisApp, ui: &mut Ui) {
    use rustgis_engine::map::layout::ElementKind;
    let Some(li) = app.session.active_layout else { return };
    let Some(layout) = app.session.project.layouts.get(li).cloned() else { return };
    let frame = layout_frame(app);
    let add = |app: &mut RustGisApp, mut p: Value| {
        p["layout"] = json!(li);
        if let Some(f) = frame {
            p["frame"] = json!(f);
        }
        match app.run("layout.addElement", p) {
            Ok(v) => app.layout.selected = v["element"].as_u64().map(|v| v as u32),
            // A locked frame takes no grid or extent indicator: say so rather than nothing (#536).
            Err(e) => app.session.notify(e, true),
        }
    };
    group(ui, "Page", |ui| {
        big_menu(ui, app, "new_layout", "Page\nSize", "Change the page size and orientation.", |ui, app| {
            for (id, label, w, h) in rustgis_engine::map::layout::PAGES {
                let landscape = layout.width > layout.height;
                let (pw, ph) = if landscape { (h.max(*w), h.min(*w)) } else { (*w, *h) };
                let on = (layout.width - pw).abs() < 0.5 && (layout.height - ph).abs() < 0.5;
                if ui.radio(on, format!("{} ({w:.0} × {h:.0} mm)", tl!(*label))).clicked() {
                    let _ = app.run(
                        "layout.page",
                        json!({"layout": li, "page": id, "orientation": if landscape { "landscape" } else { "portrait" }}),
                    );
                    ui.close();
                }
            }
            ui.separator();
            for o in ["Portrait", "Landscape"] {
                if ui.button(tl!(o)).clicked() {
                    let _ = app.run("layout.page", json!({"layout": li, "orientation": o.to_lowercase()}));
                    ui.close();
                }
            }
        });
        col(ui, |ui| {
            let snap = !app.layout.no_snap;
            let desc = "Snap elements to guides, the page and each other as you move or resize them. Hold Alt to place one freely.";
            if small(ui, app, "keep_tool", "Snap", desc, "ui.menu", Value::Null, snap).clicked() {
                app.layout.no_snap = snap;
            }
            let shown = !app.layout.no_rulers;
            let desc = "Show rulers. Drag from a ruler to add a guide; drag a guide back onto it to remove it.";
            if small(ui, app, "measure", "Rulers", desc, "ui.menu", Value::Null, shown).clicked() {
                app.layout.no_rulers = shown;
            }
            if !layout.guides.is_empty() {
                small(
                    ui,
                    app,
                    "delete",
                    "Clear Guides",
                    "Remove every guide from this layout.",
                    "layout.guides",
                    json!({"layout": li, "clear": true}),
                    false,
                );
            }
        });
    });
    group(ui, "Map Frames", |ui| {
        big_menu(ui, app, "map", "Map\nFrame", "Add a map frame showing a map of the project.", |ui, app| {
            for (shape, label) in [("rectangle", "Rectangle"), ("circle", "Circle"), ("ellipse", "Ellipse")] {
                if ui.button(tl!(label)).clicked() {
                    if let Ok(v) = app.run("layout.addFrame", json!({"layout": li, "shape": shape})) {
                        app.layout.selected = v["element"].as_u64().map(|v| v as u32);
                    }
                    ui.close();
                }
            }
            for (shape, label) in [("polygon", "Polygon (draw)"), ("lasso", "Lasso (draw)")] {
                if ui.button(tl!(label)).clicked() {
                    let _ = app.run("ui.layoutTool", json!({"shape": shape}));
                    ui.close();
                }
            }
        });
        let navigate = app.layout.navigate;
        let desc = "Drag inside map frames to pan their maps and turn the wheel to zoom them, instead of moving them. Esc stops.";
        if big(ui, app, "explore", "Pan/Zoom\nMap", desc, "ui.menu", Value::Null, navigate, false).clicked() {
            app.layout.navigate = !navigate;
        }
        col(ui, |ui| {
            let has_grid =
                frame.and_then(|f| layout.element(f)).is_some_and(|e| matches!(&e.kind, ElementKind::MapFrame { grid: Some(_), .. }));
            if small(
                ui,
                app,
                "graticule",
                "Grid",
                "Draw a labelled latitude/longitude grid over the map frame.",
                "ui.menu",
                Value::Null,
                has_grid,
            )
            .clicked()
            {
                add(app, json!({"kind": "grid", "off": has_grid}));
            }
            let others: Vec<(u32, String)> = layout
                .elements
                .iter()
                .filter(|e| Some(e.id) != frame && matches!(e.kind, ElementKind::MapFrame { .. }))
                .map(|e| (e.id, e.name.clone()))
                .collect();
            small_menu(ui, app, "g_rect", "Extent Indicator", "Outline another frame's extent in this map frame.", |ui, app| {
                if others.is_empty() {
                    ui.label(egui::RichText::new(tl!("Add a second map frame first")).weak());
                }
                for (id, name) in &others {
                    if ui.button(format!("{name} ({id})")).clicked() {
                        add(app, json!({"kind": "extentIndicator", "of": id}));
                        ui.close();
                    }
                }
            });
            let names: Vec<String> = frame
                .and_then(|f| match layout.element(f).map(|e| &e.kind) {
                    Some(ElementKind::MapFrame { map, .. }) => app.session.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(map)),
                    _ => None,
                })
                .map(|m| m.layers.iter().filter(|l| !l.is_raster() && !l.graphics).map(|l| l.name.clone()).collect())
                .unwrap_or_default();
            small_menu(ui, app, "bookmark", "Map Series", "One page per feature of an index layer.", |ui, app| {
                for n in &names {
                    if ui.radio(layout.series.as_ref().is_some_and(|s| s.layer == *n), n.as_str()).clicked() {
                        let _ = app.run("layout.series", json!({"layout": li, "layer": n, "frame": frame}));
                        ui.close();
                    }
                }
                if layout.series.is_some() && ui.button(tl!("Turn Off")).clicked() {
                    let _ = app.run("layout.series", json!({"layout": li, "off": true}));
                    ui.close();
                }
            });
        });
        if layout.series.is_some() {
            col(ui, |ui| {
                small(
                    ui,
                    app,
                    "prev_extent",
                    "Previous Page",
                    "Show the previous map series page.",
                    "layout.seriesPage",
                    json!({"layout": li, "page": "previous"}),
                    false,
                );
                small(
                    ui,
                    app,
                    "next_extent",
                    "Next Page",
                    "Show the next map series page.",
                    "layout.seriesPage",
                    json!({"layout": li, "page": "next"}),
                    false,
                );
                small(
                    ui,
                    app,
                    "redo",
                    "Refresh",
                    "Refresh the map series from its index layer.",
                    "layout.seriesRefresh",
                    json!({"layout": li}),
                    false,
                );
            });
        }
    });
    group(ui, "Map Surrounds", |ui| {
        for (icon, label, kind, desc) in [
            ("north_arrow", "North\nArrow", "northArrow", "Add a north arrow for the map frame."),
            ("scale_bar", "Scale\nBar", "scaleBar", "Add a scale bar for the map frame."),
            ("legend", "Legend", "legend", "Add a legend of the map frame's layers."),
            ("colorbar", "Colorbar", "colorbar", "Add a colorbar of a raster layer's colour scale in the map frame."),
        ] {
            if big(ui, app, icon, label, desc, "ui.menu", Value::Null, false, false).clicked() {
                add(app, json!({"kind": kind}));
            }
        }
        col(ui, |ui| {
            if small(ui, app, "table", "Table Frame", "Add the attribute table of a layer in the map frame.", "ui.menu", Value::Null, false)
                .clicked()
            {
                add(app, json!({"kind": "table"}));
            }
            if small(ui, app, "chart", "Chart Frame", "Add a bar chart of a layer's numeric field.", "ui.menu", Value::Null, false)
                .clicked()
            {
                add(app, json!({"kind": "chart"}));
            }
            small_menu(ui, app, "add_data", "More", "Additional surrounds.", |ui, app| {
                if ui.button(tl!("Neatline")).clicked() {
                    add(app, json!({"kind": "rectangle"}));
                    ui.close();
                }
                if ui.button(tl!("Backdrop")).clicked() {
                    add(app, json!({"kind": "rectangle", "fill": "#F1F4F8", "width": 0}));
                    ui.close();
                }
            });
        });
    });
    group(ui, "Shapes", |ui| {
        // In the middle of the page, ready to be moved and sized.
        let (pw, ph) = (layout.width, layout.height);
        let (w, h) = (pw * 0.25, ph * 0.2);
        let desc = "Add a rectangle, to frame or highlight part of the page.";
        if big(ui, app, "g_rect", "Rectangle", desc, "ui.menu", Value::Null, false, false).clicked() {
            add(app, json!({"kind": "rectangle", "x": (pw - w) / 2.0, "y": (ph - h) / 2.0, "w": w, "h": h}));
        }
        col(ui, |ui| {
            if small(ui, app, "g_circle", "Circle", "Add a circle.", "ui.menu", Value::Null, false).clicked() {
                add(app, json!({"kind": "circle"}));
            }
            if small(ui, app, "g_ellipse", "Ellipse", "Add an ellipse.", "ui.menu", Value::Null, false).clicked() {
                add(app, json!({"kind": "ellipse"}));
            }
        });
        col(ui, |ui| {
            small_menu(ui, app, "g_line", "Line", "Add a straight line.", |ui, app| {
                for d in rustgis_engine::map::layout::LineDir::ALL {
                    if ui.button(tl!(d.label())).clicked() {
                        add(app, json!({"kind": "line", "dir": d.code()}));
                        ui.close();
                    }
                }
            });
            // Pointing right; the round handle above it turns it any way.
            let desc = "Add an arrow. Turn it with the round handle above it, or set its angle in its properties.";
            if small(ui, app, "g_arrow", "Arrow", desc, "ui.menu", Value::Null, false).clicked() {
                add(app, json!({"kind": "arrow", "angle": 0}));
            }
        });
    });
    group(ui, "Text", |ui| {
        if big(ui, app, "g_text", "Text", "Add a text element.", "ui.menu", Value::Null, false, false).clicked() {
            add(app, json!({"kind": "text"}));
        }
        big_menu(ui, app, "dynamic_text", "Dynamic\nText", "Add text that updates: page, date, scale, map and more.", |ui, app| {
            for (token, what) in [
                ("{date}", "Current date"),
                ("{time}", "Current time"),
                ("{scale}", "Map scale"),
                ("{map}", "Map name"),
                ("{crs}", "Coordinate system"),
                ("{page} / {pages}", "Page number"),
                ("{pageName}", "Map series page name"),
                ("{layout}", "Layout name"),
            ] {
                if ui.button(format!("{}  {token}", tl!(what))).clicked() {
                    add(app, json!({"kind": "dynamicText", "text": token}));
                    ui.close();
                }
            }
        });
        if big(ui, app, "picture", "Picture", "Add a PNG or JPEG picture to the page.", "ui.layoutPicture", Value::Null, false, false)
            .clicked()
        {}
    });
    group(ui, "Export", |ui| {
        big(ui, app, "export", "Export", "Export the layout as PDF or PNG.", "ui.exportLayout", Value::Null, false, false);
        big(ui, app, "print", "Print", "Print the layout.", "ui.printLayout", Value::Null, false, false);
        col(ui, |ui| {
            small(ui, app, "save_as", "Save Layout File", "Save the layout as a layout file.", "ui.saveLayout", Value::Null, false);
            small(ui, app, "map", "Show Map", "Close the layout and show the map.", "layout.close", Value::Null, false);
        });
    });
}

fn plugins_tab(app: &mut RustGisApp, ui: &mut Ui) {
    group(ui, "Plugins", |ui| {
        big(
            ui,
            app,
            "plugin",
            "Plugin\nManager",
            "Browse, install, update and remove plugins.",
            "ui.plugins",
            json!({"open": true}),
            false,
            false,
        );
        col(ui, |ui| {
            small(
                ui,
                app,
                "plugin_add",
                "Install From File",
                "Install a plugin from a .rgplugin file.",
                "ui.installPlugin",
                Value::Null,
                false,
            );
            small(
                ui,
                app,
                "search",
                "Browse Plugins",
                "Find plugins in the RustGIS plugin registry.",
                "ui.plugins",
                json!({"open": true, "tab": "browse"}),
                false,
            );
        });
    });
    // A group per enabled plugin with commands, in the interface language when it has one
    // (plugins with a tab of their own are on that tab instead).
    let lang = app.lang().code();
    // Its tools too: each opens in the Tools pane.
    for p in rustgis_engine::plugins::list()
        .into_iter()
        .filter(|p| p.enabled && p.manifest.tab.is_none() && !(p.manifest.commands.is_empty() && p.manifest.tools.is_empty()))
    {
        let m = &p.manifest;
        group(ui, m.translate(lang, &m.name), |ui| {
            // Its panel first, when it has one.
            if let Some(d) = &m.panel {
                let (icon, label, help) = panel_button(m, d, lang);
                let open = panel_open(app, &m.id);
                big(ui, app, icon, &label, &help, "ui.pane.plugin", json!({"id": m.id}), open, false);
            }
            for c in &m.commands {
                let icon = if crate::icons::ALL.contains(&c.icon.as_str()) { c.icon.as_str() } else { "plugin" };
                let id = rustgis_engine::plugins::command_id(&m.id, &c.id);
                big(ui, app, icon, m.translate(lang, &c.label), m.translate(lang, &c.help), &id, Value::Null, false, false);
            }
            for t in &m.tools {
                let id = rustgis_engine::plugins::command_id(&m.id, &t.id);
                let label = m.translate(lang, &t.name).replacen(' ', "\n", 1);
                big(ui, app, "toolbox", &label, m.translate(lang, &t.summary), "ui.gp.open", json!({"tool": id}), false, false);
            }
        });
    }
}

/// The button that opens a plugin's panel: its icon, label (the panel's title, on two lines when
/// long) and help, in language `lang`.
fn panel_button(
    m: &rustgis_engine::plugins::Manifest,
    d: &rustgis_engine::plugins::PanelDecl,
    lang: &str,
) -> (&'static str, String, String) {
    let icon = crate::icons::ALL.iter().copied().find(|i| *i == d.icon).unwrap_or("plugin");
    let title = m.translate(lang, &d.title);
    let label = if title.chars().count() > 12 { title.replacen(' ', "\n", 1) } else { title.to_string() };
    let help = if d.help.is_empty() { tl!("Open the plugin's panel.").to_string() } else { m.translate(lang, &d.help).to_string() };
    (icon, label, help)
}

/// Whether plugin `id`'s panel is in view.
fn panel_open(app: &RustGisApp, id: &str) -> bool {
    app.ui.right == Some(crate::RightPane::Plugin) && app.ui.plugin_panel.plugin.as_deref() == Some(id)
}

/// A plugin entry, shared by ribbon buttons and menu rows (`layout == None`).
fn plugin_entry(
    app: &mut RustGisApp,
    ui: &mut Ui,
    m: &rustgis_engine::plugins::Manifest,
    lang: &str,
    entry: &str,
    layout: Option<rustgis_engine::plugins::Layout>,
) {
    use rustgis_engine::plugins::{Entry, Layout, command_id};
    let open = panel_open(app, &m.id);
    let mut show = |icon: &str, label: &str, help: &str, id: &str, params: Value| {
        let label = if layout != Some(Layout::Large) && label.contains('\n') {
            std::borrow::Cow::Owned(label.replace('\n', " "))
        } else {
            std::borrow::Cow::Borrowed(label)
        };
        let checked = open && id == "ui.pane.plugin";
        match layout {
            Some(Layout::Large) => {
                big(ui, app, icon, &label, help, id, params, checked, false);
            }
            Some(Layout::Small) => {
                small(ui, app, icon, &label, help, id, params, checked);
            }
            None => menu_item(ui, app, icon, &label, id, params),
        }
    };
    match m.entry(entry) {
        Some(Entry::Command(c)) => {
            let icon = if crate::icons::ALL.contains(&c.icon.as_str()) { c.icon.as_str() } else { "plugin" };
            show(icon, m.translate(lang, &c.label), m.translate(lang, &c.help), &command_id(&m.id, &c.id), Value::Null);
        }
        Some(Entry::Tool(t)) => {
            let name = m.translate(lang, &t.name);
            let label = if layout == Some(Layout::Large) {
                std::borrow::Cow::Owned(name.replacen(' ', "\n", 1))
            } else {
                std::borrow::Cow::Borrowed(name)
            };
            show("toolbox", &label, m.translate(lang, &t.summary), "ui.gp.open", json!({"tool": command_id(&m.id, &t.id)}));
        }
        Some(Entry::Panel(d)) => {
            let (icon, label, help) = panel_button(m, d, lang);
            show(icon, &label, &help, "ui.pane.plugin", json!({"id": m.id}));
        }
        None => {}
    }
}

/// Popup entries always use menu rows; nested menus cascade rather than opening another ribbon.
fn plugin_menu_items(
    app: &mut RustGisApp,
    ui: &mut Ui,
    m: &rustgis_engine::plugins::Manifest,
    lang: &str,
    items: &[rustgis_engine::plugins::ItemDecl],
) {
    use rustgis_engine::plugins::ItemDecl;
    for (index, item) in items.iter().enumerate() {
        ui.push_id(index, |ui| match item {
            ItemDecl::Entry(e) => plugin_entry(app, ui, m, lang, e, None),
            ItemDecl::Menu(menu) => {
                let icon = if crate::icons::ALL.contains(&menu.icon.as_str()) { menu.icon.as_str() } else { "plugin" };
                let label = m.translate(lang, &menu.menu);
                let label = if label.contains('\n') {
                    std::borrow::Cow::Owned(label.replace('\n', " "))
                } else {
                    std::borrow::Cow::Borrowed(label)
                };
                submenu_row(ui, icon, &label, |ui| plugin_menu_items(app, ui, m, lang, &menu.items));
            }
        });
    }
}

fn plugin_item_layout(
    item: &rustgis_engine::plugins::ItemDecl,
    inherited: rustgis_engine::plugins::Layout,
) -> rustgis_engine::plugins::Layout {
    match item {
        rustgis_engine::plugins::ItemDecl::Entry(_) => inherited,
        rustgis_engine::plugins::ItemDecl::Menu(menu) => menu.layout.unwrap_or(inherited),
    }
}

fn plugin_ribbon_item(
    app: &mut RustGisApp,
    ui: &mut Ui,
    m: &rustgis_engine::plugins::Manifest,
    lang: &str,
    item: &rustgis_engine::plugins::ItemDecl,
    layout: rustgis_engine::plugins::Layout,
) {
    use rustgis_engine::plugins::{ItemDecl, Layout};
    match item {
        ItemDecl::Entry(e) => plugin_entry(app, ui, m, lang, e, Some(layout)),
        ItemDecl::Menu(menu) => {
            let icon = if crate::icons::ALL.contains(&menu.icon.as_str()) { menu.icon.as_str() } else { "plugin" };
            let label = m.translate(lang, &menu.menu);
            let help = m.translate(lang, &menu.help);
            let contents = |ui: &mut Ui, app: &mut RustGisApp| plugin_menu_items(app, ui, m, lang, &menu.items);
            match layout {
                Layout::Large => big_menu(ui, app, icon, label, help, contents),
                Layout::Small => {
                    let label = if label.contains('\n') {
                        std::borrow::Cow::Owned(label.replace('\n', " "))
                    } else {
                        std::borrow::Cow::Borrowed(label)
                    };
                    small_menu(ui, app, icon, &label, help, contents);
                }
            }
        }
    }
}

/// A plugin's own tab: large controls and runs of three small controls per column.
fn plugin_tab(app: &mut RustGisApp, ui: &mut Ui, plugin: &str) {
    use rustgis_engine::plugins::Layout;
    let Some(p) = rustgis_engine::plugins::get(plugin).filter(|p| p.enabled) else { return };
    let Some(tab) = p.manifest.tab.as_ref() else { return };
    let m = &p.manifest;
    let lang = app.lang().code();
    for (group_index, g) in tab.groups.iter().enumerate() {
        ui.push_id(group_index, |ui| {
            group(ui, m.translate(lang, &g.label), |ui| {
                let mut remaining = g.items.as_slice();
                let mut offset = 0;
                while let Some(item) = remaining.first() {
                    let layout = plugin_item_layout(item, g.layout);
                    let count = if layout == Layout::Small {
                        remaining.iter().take(3).take_while(|item| plugin_item_layout(item, g.layout) == Layout::Small).count()
                    } else {
                        1
                    };
                    let (column, rest) = remaining.split_at(count);
                    let mut draw = |ui: &mut Ui| {
                        for (index, item) in column.iter().enumerate() {
                            ui.push_id(offset + index, |ui| plugin_ribbon_item(app, ui, m, lang, item, layout));
                        }
                    };
                    if layout == Layout::Small {
                        col(ui, draw);
                    } else {
                        draw(ui);
                    }
                    offset += count;
                    remaining = rest;
                }
            });
        });
    }
}

/// The Settings tab: ribbon customization, language, start-up and (on the desktop) autosave.
fn settings_tab(app: &mut RustGisApp, ui: &mut Ui) {
    group(ui, "Customize", |ui| {
        let hidden = app.ui.hidden_tabs.clone();
        // RustGIS's tabs and plugins' tabs.
        let mut all: Vec<(String, String)> =
            TABS.iter().filter(|t| !FIXED_TABS.contains(*t)).map(|t| (t.to_string(), tl!(*t).to_string())).collect();
        all.extend(plugin_tabs(app));
        big_form_menu(ui, app, "settings", "Ribbon", "Choose which tabs the ribbon shows.", move |ui, app| {
            for (tab, caption) in &all {
                let mut on = !hidden.iter().any(|h| h == tab);
                if ui.checkbox(&mut on, caption.as_str()).clicked() {
                    let _ = app.run("ui.customizeRibbon", json!({"tab": tab}));
                }
            }
        });
        big(ui, app, "command", "Keyboard\nShortcuts", "Customize keyboard shortcuts.", "ui.customizeShortcuts", Value::Null, false, false);
        let qa = app.ui.quick_access.clone();
        big_form_menu(ui, app, "command", "Quick\nAccess", "Choose extra buttons for the Quick Access Toolbar.", move |ui, app| {
            for (id, label) in QUICK {
                let mut on = qa.iter().any(|c| c == id);
                if ui.checkbox(&mut on, tl!(*label)).clicked() {
                    let _ = app.run("ui.quickAccess", json!({"command": id}));
                }
            }
        });
    });
    group(ui, "Language", |ui| {
        language_menu(app, ui);
        worldview_menu(app, ui);
    });
    group(ui, "Start-up", |ui| start_project_menu(app, ui));
    // Only a shell that keeps recovery copies (the desktop app) has autosave.
    if app.services.recovery.is_some() {
        group(ui, "Autosave", |ui| autosave_menu(app, ui));
    }
}

/// An interval's name: "Off" or "Every 5 min".
fn autosave_interval(minutes: u32) -> String {
    if minutes == 0 { tl!("Off").to_string() } else { crate::i18n::fmt("Every {n} min", &[("n", &minutes.to_string())]) }
}

/// Settings › Autosave: how often a recovery copy of unsaved work is kept (#873).
fn autosave_menu(app: &mut RustGisApp, ui: &mut Ui) {
    let minutes = app.autosave.minutes;
    let label = format!("{}\n{}", tl!("Autosave"), autosave_interval(minutes));
    let tip = "How often RustGIS keeps a copy of unsaved work, to offer it back after a crash.";
    big_menu(ui, app, "save", &label, tip, move |ui, app| {
        for m in crate::autosave::CHOICES {
            if ui.add(egui::Button::selectable(minutes == m, autosave_interval(m))).clicked() {
                let _ = app.run("ui.autosave", json!({"minutes": m}));
                ui.close();
            }
        }
    });
}

/// Settings › Start-up › Start With: the World Atlas sample, an empty project, or (in the
/// desktop app) a project picked once or the last one opened or saved (#377).
fn start_project_menu(app: &mut RustGisApp, ui: &mut Ui) {
    use crate::StartWith;
    let with = app.ui.start_with.clone();
    // The button's second line is the file's name, kept short so the ribbon keeps its width.
    let short_name = |p: &str| {
        let name = std::path::Path::new(p).file_name().map_or_else(|| p.to_string(), |n| n.to_string_lossy().into_owned());
        if name.chars().count() > 24 { format!("{}…", name.chars().take(23).collect::<String>()) } else { name }
    };
    let current = match &with {
        StartWith::Sample => tl!("World Atlas").to_string(),
        StartWith::Blank => tl!("Blank Map").to_string(),
        StartWith::Last => tl!("Last Project").to_string(),
        StartWith::File(p) => short_name(p),
    };
    let label = format!("{}\n{current}", tl!("Start With"));
    let files = app.opens_files();
    let tip = if files {
        "What RustGIS opens with: the World Atlas sample, a blank map, a project you choose, or the last project opened or saved."
    } else {
        "What RustGIS opens with: the World Atlas sample or a blank map."
    };
    big_menu(ui, app, "map", &label, tip, move |ui, app| {
        for (w, name) in [(StartWith::Sample, tl!("World Atlas")), (StartWith::Blank, tl!("Blank Map"))] {
            if ui.add(egui::Button::selectable(with == w, name)).clicked() {
                let _ = app.run("ui.startup", json!({"project": w.json()}));
                ui.close();
            }
        }
        if !files {
            return;
        }
        // Last Project: offered once there is one (or while it's the choice).
        let last = app.ui.last_project.clone();
        let item = ui
            .add_enabled(last.is_some() || with == StartWith::Last, egui::Button::selectable(with == StartWith::Last, tl!("Last Project")))
            .on_hover_text(last.clone().unwrap_or_else(|| tl!("No project opened or saved yet").to_string()))
            .on_disabled_hover_text(tl!("No project opened or saved yet"));
        if item.clicked() {
            let _ = app.run("ui.startup", json!({"project": "last"}));
            ui.close();
        }
        let chosen = match &with {
            StartWith::File(p) => p.clone(),
            _ => String::new(),
        };
        let item = ui.add(egui::Button::selectable(matches!(with, StartWith::File(_)), tl!("Choose Project…")));
        let item = if chosen.is_empty() { item } else { item.on_hover_text(chosen) };
        if item.clicked() {
            ui.close();
            if let Some(path) = crate::dialogs::pick_open(&["rgis", "rgisx"], tl!("RustGIS project"))
                && let Err(e) = app.run("ui.startup", json!({"project": {"path": path}}))
            {
                app.session.notify(e, true);
            }
        }
    });
    start_projection_menu(app, ui);
    start_extent_menu(app, ui);
}

/// Settings › Start-up › Extent: the full extent or a saved view.
fn start_extent_menu(app: &mut RustGisApp, ui: &mut Ui) {
    let saved = app.ui.start_extent;
    let current = if saved.is_some() { tl!("Saved View") } else { tl!("Full Extent") };
    let label = format!("{}\n{current}", tl!("Extent"));
    big_menu(ui, app, "zoom_full", &label, "Where the map opens: the full extent or a view you saved.", move |ui, app| {
        if ui.add(egui::Button::selectable(saved.is_none(), tl!("Full Extent"))).clicked() {
            let _ = app.run("ui.startup", json!({"extent": null}));
            ui.close();
        }
        if ui.button(tl!("Use Current View")).clicked() {
            if let Err(e) = app.run("ui.startup", json!({"extent": "view"})) {
                app.session.notify(e, true);
            }
            ui.close();
        }
        if let Some(e) = saved {
            ui.separator();
            ui.label(format!("{:.4}, {:.4}, {:.4}, {:.4}", e[0], e[1], e[2], e[3]));
        }
    });
}

/// Whether projection code `current` is menu item `id`: the same code, or that preset with
/// parameters (`robinson +lon_0=150` is Robinson).
fn is_projection(current: &str, id: &str) -> bool {
    current == id || rustgis_engine::map::projection::parse_preset(current).ok().flatten().is_some_and(|(p, _)| p == id)
}

/// Settings › Start-up › Projection: the projection the map opens in.
fn start_projection_menu(app: &mut RustGisApp, ui: &mut Ui) {
    let current = if app.ui.start_projection.is_empty() { "mercator".to_string() } else { app.ui.start_projection.clone() };
    let shown = rustgis_engine::map::Projection::from_code(&current).map(|p| p.label()).unwrap_or_else(|_| current.clone());
    let label = format!("{}\n{}", tl!("Projection"), tl!(shown.as_str()));
    big_form_menu(ui, app, "basemap", &label, "The projection the map opens in.", move |ui, app| {
        let mut items: Vec<(String, String)> = vec![("mercator".into(), "Web Mercator".into()), ("globe".into(), "Globe".into())];
        items.extend(rustgis_engine::map::projection::PRESETS.iter().map(|(id, label, _)| (id.to_string(), label.to_string())));
        for (id, label) in items {
            if ui.add(egui::Button::selectable(is_projection(&current, &id), tl!(label.as_str()))).clicked() {
                let _ = app.run("ui.startup", json!({"projection": id}));
                ui.close();
            }
        }
        ui.separator();
        ui.label(tl!("EPSG or ESRI code"));
        ui.horizontal(|ui| {
            let edit = ui.add(egui::TextEdit::singleline(&mut app.ui.start_projection_input).hint_text("3010").desired_width(90.0));
            let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.button(tl!("Use")).clicked() || enter {
                let code = crate::dialogs::typed_crs(&app.ui.start_projection_input);
                match app.run("ui.startup", json!({"projection": code})) {
                    Ok(_) => {
                        app.ui.start_projection_input.clear();
                        ui.close();
                    }
                    Err(e) => app.session.notify(e, true),
                }
            }
        });
        if let Ok(rustgis_engine::map::Projection::Custom(c)) = rustgis_engine::map::Projection::from_code(&current)
            && (c.starts_with("EPSG:") || c.starts_with("ESRI:"))
        {
            ui.add(egui::Button::selectable(true, c.as_str()));
        }
    });
}

fn help_tab(app: &mut RustGisApp, ui: &mut Ui) {
    group(ui, "Performance", |ui| {
        big(
            ui,
            app,
            "dra",
            "Diagnostic\nMonitor",
            "Frame rate, draw time, data size and memory.",
            "ui.pane.diagnostics",
            Value::Null,
            app.ui.right == Some(RightPane::Diagnostics),
            false,
        );
    });
    group(ui, "Learn", |ui| {
        big(ui, app, "help", "Learning\nResources", "Open the RustGIS user guide.", "ui.learn", Value::Null, false, false);
    });
    group(ui, "Help", |ui| {
        big(ui, app, "help", "About", "About RustGIS.", "ui.about", Value::Null, false, false);
        if !cfg!(target_arch = "wasm32") {
            big(
                ui,
                app,
                "update",
                "Check for\nUpdates",
                "See whether a newer RustGIS has been released.",
                "ui.checkUpdate",
                Value::Null,
                false,
                false,
            );
        }
        big(ui, app, "command", "Command\nSearch", "Find any command by name (Ctrl+K).", "ui.commandSearch", Value::Null, false, false);
        big(ui, app, "settings", "Shortcuts", "Keyboard shortcuts.", "ui.shortcuts", Value::Null, false, false);
    });
}

// ---------------------------------------------------------------------------------------------
// Style and Data groups for the selected layer

pub(crate) const SCALES: [f64; 10] =
    [1_000.0, 10_000.0, 50_000.0, 100_000.0, 500_000.0, 1_000_000.0, 5_000_000.0, 10_000_000.0, 50_000_000.0, 100_000_000.0];

pub(crate) fn scale_label(s: Option<f64>) -> String {
    match s {
        None => tl!("<None>").into(),
        Some(v) => rustgis_engine::map::graphics::scale_ratio(v),
    }
}

pub fn fmt_thousands(v: f64) -> String {
    let s = format!("{:.0}", v.abs());
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if v < 0.0 { format!("-{out}") } else { out }
}

fn feature_style_groups(app: &mut RustGisApp, ui: &mut Ui) {
    let Some(layer) = app.session.active_layer.and_then(|id| app.session.map().layer(id)).cloned() else { return };
    visibility_and_effects(app, ui, &layer);
    drawing_group(app, ui, &layer);
    effects_groups(app, ui, &layer);
    layer_group(app, ui, &layer);
}

/// The polygon layers that could mask `layer`.
fn mask_menu(ui: &mut Ui, app: &mut RustGisApp, layer: &rustgis_engine::map::Layer) {
    let polys: Vec<String> = app
        .session
        .map()
        .layers
        .iter()
        .filter(|l| l.id != layer.id && l.geometry_type() == GeometryType::Polygon)
        .map(|l| l.name.clone())
        .collect();
    let cur = layer.mask.clone();
    small_menu(ui, app, "polygon_layer", "Masking", "Show the layer only inside another layer's polygons.", move |ui, app| {
        if ui.radio(cur.is_none(), tl!("No mask")).clicked() {
            let _ = app.run("layer.mask", json!({"mask": null}));
            ui.close();
        }
        for n in &polys {
            if ui.radio(cur.as_deref() == Some(n.as_str()), n.as_str()).clicked() {
                let _ = app.run("layer.mask", json!({"mask": n}));
                ui.close();
            }
        }
    });
}

/// Masking, display filters, aggregation and extrusion for feature layers.
fn effects_groups(app: &mut RustGisApp, ui: &mut Ui, layer: &rustgis_engine::map::Layer) {
    let gt = layer.geometry_type();
    group(ui, "Drawing Effects", |ui| {
        col(ui, |ui| {
            mask_menu(ui, app, layer);
            let filters = layer.display_filters.len();
            let label = if filters > 0 {
                crate::i18n::fmt("Display Filters ({n})", &[("n", &filters.to_string())])
            } else {
                tl!("Display Filters").to_string()
            };
            small_form_menu(ui, app, "filter", &label, "Show different features at different scales.", |ui, app| {
                let input_label =
                    ui.label(egui::RichText::new(tl!("Below 1:5,000,000 show everything; beyond it only what matches:")).small().weak());
                ui.add(egui::TextEdit::singleline(&mut app.forms.expression).hint_text("pop_max > 1000000").desired_width(220.0))
                    .labelled_by(input_label.id);
                if ui.button(tl!("Set Filter")).clicked() {
                    let w = app.forms.expression.trim().to_string();
                    let _ =
                        app.run("layer.displayFilters", json!({"filters": [{"name": "Small scales", "maxScale": 5_000_000, "where": w}]}));
                    ui.close();
                }
                if ui.button(tl!("Clear Filters")).clicked() {
                    let _ = app.run("layer.displayFilters", json!({"filters": []}));
                    ui.close();
                }
            });
            if gt == GeometryType::Point {
                let cur = layer.aggregation;
                small_menu(ui, app, "count", "Aggregation", "Draw points as bins coloured by how many fall in each.", move |ui, app| {
                    for (label, shape) in [("None", "none"), ("Hexagons", "hexagon"), ("Squares", "square")] {
                        let on = match shape {
                            "none" => cur.is_none(),
                            "hexagon" => cur.is_some_and(|a| a.hexagons),
                            _ => cur.is_some_and(|a| !a.hexagons),
                        };
                        if ui.radio(on, tl!(label)).clicked() {
                            let _ = app.run("layer.aggregation", json!({"shape": shape, "size": cur.map_or(24.0, |a| a.size)}));
                            ui.close();
                        }
                    }
                });
            }
        });
    });
    if gt == GeometryType::Polygon {
        let x = layer.extrusion.clone();
        let numeric: Vec<String> = layer.dataset.fields.iter().filter(|f| f.ty.is_numeric()).map(|f| f.name.clone()).collect();
        group(ui, "Extrusion", |ui| {
            let cur = x.clone();
            big_menu(ui, app, "polygon_layer", "Type", "Raise polygons by a height or a field.", move |ui, app| {
                if ui.radio(cur.is_none(), tl!("None")).clicked() {
                    let _ = app.run("layer.extrusion", json!({"type": "none"}));
                    ui.close();
                }
                if ui.radio(cur.as_ref().is_some_and(|x| x.field.is_none()), tl!("Constant height")).clicked() {
                    let _ = app.run("layer.extrusion", json!({"type": "height", "height": 50_000}));
                    ui.close();
                }
                for f in &numeric {
                    if ui
                        .radio(
                            cur.as_ref().and_then(|x| x.field.as_deref()) == Some(f.as_str()),
                            crate::i18n::fmt("By {field}", &[("field", f)]),
                        )
                        .clicked()
                    {
                        let _ = app.run("layer.extrusion", json!({"type": "field", "field": f}));
                        ui.close();
                    }
                }
            });
            if let Some(xx) = &x {
                ui.vertical(|ui| {
                    ui.add_space(8.0);
                    let mut k = xx.factor;
                    ui.horizontal(|ui| {
                        let input_label = ui.label(egui::RichText::new(tl!("Factor")).size(11.5));
                        // Kept across a drag until it is let go (#607).
                        let drag = Dragged::lend(ui, egui::Id::unique(("extrusion_factor", layer.id)), &mut k);
                        let r = ui
                            .add(egui::DragValue::new(&mut k).range(0.0..=1e6).speed(xx.factor.max(0.0001) * 0.02))
                            .labelled_by(input_label.id);
                        if drag.settled(ui, &r, &mut k) && k != xx.factor {
                            let t = if xx.field.is_some() { "field" } else { "height" };
                            let _ = app.run("layer.extrusion", json!({"type": t, "field": xx.field, "factor": k}));
                        }
                    });
                });
            }
        });
        if let Some(xx) = x {
            group(ui, "Faces", |ui| {
                col(ui, |ui| {
                    small(
                        ui,
                        app,
                        "single",
                        "Face Culling",
                        "Hide walls facing away from you.",
                        "layer.faces",
                        json!({"culling": !xx.face_culling}),
                        xx.face_culling,
                    );
                    small(
                        ui,
                        app,
                        "brightness",
                        "Lighting",
                        "Shade walls by the way they face the light.",
                        "layer.faces",
                        json!({"lighting": !xx.lighting}),
                        xx.lighting,
                    );
                });
            });
            group(ui, "Light and Shadow", |ui| {
                col(ui, |ui| {
                    small(
                        ui,
                        app,
                        "brightness",
                        "Enable Lighting",
                        "Light the extruded shapes from the north-west.",
                        "layer.lightShadow",
                        json!({"lighting": !xx.lighting}),
                        xx.lighting,
                    );
                    small(
                        ui,
                        app,
                        "contrast",
                        "Cast Shadow",
                        "Extruded shapes cast shadows on the map.",
                        "layer.lightShadow",
                        json!({"shadows": !xx.shadows}),
                        xx.shadows,
                    );
                });
            });
        }
    }
}

fn imagery_groups(app: &mut RustGisApp, ui: &mut Ui) {
    let wf = |section: &str| json!({"section": section});
    group(ui, "Ortho Mapping", |ui| {
        big(
            ui,
            app,
            "raster_layer",
            "New\nWorkspace",
            "Mosaic the visible rasters into an orthomosaic.",
            "ui.pane.workflows",
            wf("ortho"),
            false,
            false,
        );
        small(ui, app, "settings", "Options", "Workspace cell size and blending.", "ui.pane.workflows", wf("ortho"), false);
    });
    group(ui, "Reality Mapping", |ui| {
        big(
            ui,
            app,
            "relief",
            "New\nWorkspace",
            "Merge elevation rasters into a surface model and export a 3D mesh.",
            "ui.pane.workflows",
            wf("reality"),
            false,
            false,
        );
        small(ui, app, "settings", "Options", "Workspace cell size and blending.", "ui.pane.workflows", wf("reality"), false);
    });
    group(ui, "Alignment", |ui| {
        big(
            ui,
            app,
            "goto_xy",
            "Georeference",
            "Place a raster on the map from control points or the view.",
            "ui.pane.workflows",
            wf("georeference"),
            false,
            false,
        );
    });
    group(ui, "Image Analysis", |ui| {
        col(ui, |ui| {
            small(
                ui,
                app,
                "relief",
                "Raster Functions",
                "Slope, hillshade, NDVI and more.",
                "ui.pane.workflows",
                wf("rasterFunctions"),
                false,
            );
            small(ui, app, "bands", "Function Editor", "Chain raster functions.", "ui.pane.workflows", wf("functionEditor"), false);
        });
        col(ui, |ui| {
            small(
                ui,
                app,
                "contrast",
                "Change Detection",
                "Difference, ratio or percent change between two rasters.",
                "ui.pane.workflows",
                wf("change"),
                false,
            );
            small(ui, app, "graduated", "Spectral Analysis", "Every band's value at a pixel.", "ui.pane.workflows", wf("spectral"), false);
        });
    });
    group(ui, "Image Classification", |ui| {
        big(
            ui,
            app,
            "classify",
            "Classification\nWizard",
            "Unsupervised classes (k-means).",
            "ui.pane.workflows",
            wf("classify"),
            false,
            false,
        );
        big(
            ui,
            app,
            "unique",
            "Classification\nTools",
            "Classes from training polygons.",
            "ui.pane.workflows",
            wf("supervised"),
            false,
            false,
        );
        big(
            ui,
            app,
            "modelbuilder",
            "Deep Learning\nTools",
            "Train a neural network on training polygons and classify every pixel.",
            "ui.pane.workflows",
            wf("deepLearning"),
            false,
            false,
        );
    });
    group(ui, "Imagery Sharing", |ui| {
        big(
            ui,
            app,
            "raster_layer",
            "Create Hosted\nImagery",
            "Cut a raster into web map tiles (XYZ PNG) with a viewer page.",
            "ui.pane.workflows",
            wf("hosted"),
            false,
            false,
        );
    });
    group(ui, "Mensuration", |ui| {
        big(
            ui,
            app,
            "measure",
            "Mensuration",
            "Distances along the surface, heights, areas and volumes.",
            "ui.pane.workflows",
            wf("mensuration"),
            false,
            false,
        );
        small(ui, app, "history", "Results", "Measurements so far.", "ui.pane.workflows", wf("mensuration"), false);
    });
    group(ui, "Imagery Tools", |ui| {
        col(ui, |ui| {
            small(ui, app, "resample", "Process", "Clip a raster to the view, or resample it.", "ui.pane.workflows", wf("process"), false);
            small(ui, app, "stretch", "Indices", "NDVI, NDWI, SAVI, NBR, NDBI, EVI.", "ui.pane.workflows", wf("indices"), false);
            small(ui, app, "edit", "Pixel Editor", "Set the pixels inside a polygon.", "ui.pane.workflows", wf("pixels"), false);
        });
    });
}

/// The Visibility Range and Effects groups shared by feature and raster layers.
fn visibility_and_effects(app: &mut RustGisApp, ui: &mut Ui, layer: &rustgis_engine::map::Layer) {
    group(ui, "Visibility Range", |ui| {
        ui.vertical(|ui| {
            ui.add_space(6.0);
            for (label, cur, key) in [("Out Beyond", layer.min_scale, "min"), ("In Beyond", layer.max_scale, "max")] {
                ui.horizontal(|ui| {
                    let input_label = ui.label(egui::RichText::new(tl!(label)).size(11.5));
                    egui::ComboBox::from_id_salt(format!("scale_{key}"))
                        .selected_text(scale_label(cur))
                        .width(118.0)
                        .show_ui(ui, |ui| {
                            let mut choices: Vec<Option<f64>> = vec![None];
                            choices.extend(SCALES.iter().map(|s| Some(*s)));
                            for c in choices {
                                if ui.selectable_label(c == cur, scale_label(c)).clicked() {
                                    let (mut min, mut max) = (layer.min_scale, layer.max_scale);
                                    if key == "min" {
                                        min = c;
                                    } else {
                                        max = c;
                                    }
                                    let _ = app.run("layer.scaleRange", json!({"min": min, "max": max}));
                                }
                            }
                        })
                        .response
                        .labelled_by(input_label.id);
                });
            }
        });
        big(ui, app, "scale_range", "Clear\nLimits", "Draw the layer at every scale.", "layer.scaleRange", json!({}), false, false);
    });
    group(ui, "Effects", |ui| {
        ui.vertical(|ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let t = Tokens::get(ui.ctx());
                icons::paint(ui.painter(), Rect::from_min_size(ui.cursor().min, vec2(20.0, 20.0)), "transparency", t.icon, t.accent);
                ui.add_space(24.0);
                ui.label(egui::RichText::new(tl!("Transparency")).size(11.5));
            });
            let mut v = layer.transparency;
            ui.spacing_mut().slider_width = 130.0;
            let r = ui.add(egui::Slider::new(&mut v, 0.0..=100.0).suffix(" %").integer()).on_hover_text(tl!("Transparency"));
            live_drag(
                app,
                ui,
                &r,
                layer.transparency as f64,
                v as f64,
                |app, x| {
                    if let Some(l) = app.session.active_layer.and_then(|id| app.session.map_mut().layer_mut(id)) {
                        l.transparency = x as f32;
                    }
                },
                |app, x| {
                    let _ = app.run("layer.transparency", json!({"value": x}));
                },
            );
        });
    });
}

/// A dragged control with a live preview: `set` writes the value straight into the project while
/// dragging, and on release the pre-drag value is put back before `commit` runs the command, so
/// the command's undo snapshot holds the old value.
pub(crate) fn live_drag(
    app: &mut RustGisApp,
    ui: &Ui,
    resp: &egui::Response,
    before: f64,
    value: f64,
    set: impl Fn(&mut RustGisApp, f64),
    commit: impl FnOnce(&mut RustGisApp, f64),
) {
    let key = resp.id.with("live_drag_original");
    if resp.drag_started() {
        ui.data_mut(|d| d.insert_temp(key, before));
    }
    if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
        if let Some(original) = ui.data_mut(|d| d.remove_temp::<f64>(key)) {
            set(app, original);
        }
        commit(app, value);
    } else if resp.dragged() {
        set(app, value);
        app.session.revision = app.session.revision.wrapping_add(1);
    }
}

/// Style ribbon controls for the layer's renderer, fill, outline and marker size.
fn drawing_group(app: &mut RustGisApp, ui: &mut Ui, layer: &rustgis_engine::map::Layer) {
    group(ui, "Drawing", |ui| {
        big(
            ui,
            app,
            "symbology",
            "Symbology",
            "Open the Symbology pane.",
            "ui.pane.symbology",
            Value::Null,
            app.ui.right == Some(RightPane::Symbology),
            false,
        );
        let numeric: Vec<String> = layer.dataset.fields.iter().filter(|f| f.ty.is_numeric()).map(|f| f.name.clone()).collect();
        let text_f: Vec<String> = layer.dataset.fields.iter().map(|f| f.name.clone()).collect();
        let is_point = layer.geometry_type() == GeometryType::Point;
        big_menu(ui, app, renderer_icon(&layer.renderer), "Primary\nSymbology", "Choose how the layer is drawn.", move |ui, app| {
            if ui.button(tl!("Single Symbol")).clicked() {
                let _ = app.run("layer.symbology", json!({"type": "single"}));
                ui.close();
            }
            ui.menu_button(tl!("Unique Values"), |ui| {
                for f in &text_f {
                    if ui.button(f).clicked() {
                        let _ = app.run("layer.symbology", json!({"type": "unique", "field": f}));
                        ui.close();
                    }
                }
            });
            ui.menu_button(tl!("Graduated Colors"), |ui| {
                for f in &numeric {
                    if ui.button(f).clicked() {
                        let _ = app.run("layer.symbology", json!({"type": "graduated", "field": f}));
                        ui.close();
                    }
                }
            });
            if is_point {
                ui.menu_button(tl!("Proportional Symbols"), |ui| {
                    for f in &numeric {
                        if ui.button(f).clicked() {
                            let _ = app.run("layer.symbology", json!({"type": "proportional", "field": f}));
                            ui.close();
                        }
                    }
                });
            }
        });
        col(ui, |ui| {
            let mut c = crate::theme::c32(layer.renderer.base_symbol().fill);
            ui.horizontal(|ui| {
                let resp = crate::widgets::color_button(ui, &mut c);
                if resp.changed() {
                    crate::widgets::one_step_per_press(app, &resp);
                    let _ = app.run("layer.color", json!({"color": crate::theme::rgba(c).hex8()}));
                }
                ui.label(egui::RichText::new(tl!("Color")).size(11.5));
            });
            let mut w = layer.renderer.base_symbol().stroke_width;
            ui.horizontal(|ui| {
                let resp = ui.add(egui::DragValue::new(&mut w).range(0.0..=10.0).speed(0.05).suffix(" pt"));
                if resp.changed() {
                    crate::widgets::one_step_per_press(app, &resp);
                    let _ = app.run("layer.outline", json!({"width": w}));
                }
                ui.label(egui::RichText::new(tl!("Outline")).size(11.5));
            });
            if layer.geometry_type() == GeometryType::Point {
                let mut s = layer.renderer.base_symbol().size;
                ui.horizontal(|ui| {
                    let resp = ui.add(egui::DragValue::new(&mut s).range(1.0..=60.0).speed(0.1).suffix(" pt"));
                    if resp.changed() {
                        crate::widgets::one_step_per_press(app, &resp);
                        let _ = app.run("layer.marker", json!({"size": s}));
                    }
                    ui.label(egui::RichText::new(tl!("Size")).size(11.5));
                });
            }
            if layer.has_geometry(GeometryType::Polygon) && !layer.graphics {
                small(
                    ui,
                    app,
                    "no_fill",
                    "No Fill",
                    "Draw only polygon outlines, keeping their colors and widths.",
                    "layer.noFill",
                    Value::Null,
                    false,
                );
            }
        });
    });
}

fn layer_group(app: &mut RustGisApp, ui: &mut Ui, layer: &rustgis_engine::map::Layer) {
    group(ui, "Layer", |ui| {
        big(ui, app, "zoom_full", "Zoom To\nLayer", "Zoom to the layer's extent.", "layer.zoomTo", Value::Null, false, false);
        col(ui, |ui| {
            if layer.is_raster() {
                small(
                    ui,
                    app,
                    "symbology",
                    "Symbology",
                    "Open the Symbology pane.",
                    "ui.pane.symbology",
                    Value::Null,
                    app.ui.right == Some(RightPane::Symbology),
                );
            } else {
                small(ui, app, "table", "Attribute Table", "Open the layer's table.", "table.open", Value::Null, false);
            }
            small(ui, app, "eye", "Visible", "Show or hide the layer.", "layer.visibility", Value::Null, layer.visible);
            small(ui, app, "delete", "Remove", "Remove the layer from the map.", "layer.remove", Value::Null, false);
        });
    });
}

fn raster_style_groups(app: &mut RustGisApp, ui: &mut Ui) {
    let Some(layer) = app.session.active_layer.and_then(|id| app.session.map().layer(id)).cloned() else { return };
    let Some(rl) = layer.raster.clone() else { return };
    visibility_and_effects(app, ui, &layer);
    let Some(data) = rl.data.clone() else {
        group(ui, "Rendering", |ui| {
            ui.label(egui::RichText::new(tl!("The raster's data couldn't be read.")).size(11.5));
        });
        return;
    };
    let style = rl.style.clone();
    group(ui, "Compare", |ui| {
        let on = app.session.map().swipe.is_some_and(|(id, _)| id == layer.id);
        big(
            ui,
            app,
            "contrast",
            "Swipe",
            "Show the layer only left of a divider, to compare it with what's beneath.",
            "layer.swipe",
            if on { json!({"off": true}) } else { json!({"position": 0.5}) },
            on,
            false,
        );
        if on {
            let mut x = app.session.map().swipe.map_or(0.5, |(_, x)| x);
            ui.vertical(|ui| {
                ui.add_space(14.0);
                let resp = ui.add(egui::Slider::new(&mut x, 0.0..=1.0).show_value(false)).on_hover_text(tl!("Swipe"));
                if resp.changed() {
                    crate::widgets::one_step_per_press(app, &resp);
                    let _ = app.run("layer.swipe", json!({"position": x}));
                }
            });
        }
    });
    group(ui, "Raster Effects", |ui| {
        col(ui, |ui| {
            let cur = layer.blend;
            small_menu(ui, app, "transparency", "Layer Blend", "How the layer's colours combine with what's beneath.", move |ui, app| {
                for b in rustgis_engine::map::effects::Blend::ALL {
                    if ui.radio(cur == b, tl!(b.label())).clicked() {
                        let _ = app.run("layer.blend", json!({"mode": b.code()}));
                        ui.close();
                    }
                }
            });
            mask_menu(ui, app, &layer);
            let mut a = style.rotation;
            ui.horizontal(|ui| {
                // Kept across a drag until it is let go (#607).
                let drag = Dragged::lend(ui, egui::Id::unique(("raster_rotation", layer.id)), &mut a);
                let r = ui.add(egui::DragValue::new(&mut a).range(0.0..=359.0).speed(1.0).suffix("°"));
                if drag.settled(ui, &r, &mut a) && a != style.rotation {
                    let _ = app.run("raster.rotation", json!({"angle": a}));
                }
                ui.label(egui::RichText::new(tl!("Rotation")).size(11.5));
            });
        });
    });
    group(ui, "Rendering", |ui| {
        let n = data.band_count();
        let has_categories = data.bands.first().is_some_and(|b| b.stats.categories.is_some());
        let palette = matches!(data.color, rustgis_engine::data::raster::ColorHint::Palette(_));
        let kind = style.renderer.code();
        big_menu(ui, app, raster_renderer_icon(&style.renderer), "Symbology", "Choose how the raster is drawn.", move |ui, app| {
            for (code, label) in RasterRenderer::KINDS {
                let ok = match code {
                    "rgb" => n >= 3,
                    "unique" => has_categories,
                    "colormap" => palette,
                    _ => true,
                };
                let checked = kind == code;
                if ui.add_enabled(ok, egui::Button::selectable(checked, tl!(label))).clicked() {
                    let _ = app.run("raster.symbology", json!({"type": code}));
                    ui.close();
                }
            }
            ui.separator();
            if ui.button(tl!("Symbology Pane…")).clicked() {
                app.ui.open_right(RightPane::Symbology);
                ui.close();
            }
        });
        let current = style.stretch;
        big_menu(ui, app, "stretch", "Stretch\nType", "Choose a contrast stretch for the raster layer.", move |ui, app| {
            for (code, label) in Stretch::KINDS {
                if ui.add(egui::Button::selectable(current.code() == code, tl!(label))).clicked() {
                    let _ = app.run("raster.stretch", json!({"type": code}));
                    ui.close();
                }
            }
        });
        col(ui, |ui| {
            small(
                ui,
                app,
                "dra",
                "DRA",
                "Dynamic range adjustment: stretch using the pixels in view.",
                "raster.dra",
                Value::Null,
                style.dra,
            );
            let res = style.resampling;
            small_menu(ui, app, "resample", "Resampling", "Choose a resampling type for the raster layer.", move |ui, app| {
                for r in Resampling::ALL {
                    if ui.add(egui::Button::selectable(r == res, tl!(r.label()))).clicked() {
                        let _ = app.run("raster.resampling", json!({"type": format!("{r:?}")}));
                        ui.close();
                    }
                }
            });
            let bands: Vec<String> = data.bands.iter().map(|b| b.name.clone()).collect();
            let cur = match style.renderer {
                RasterRenderer::Rgb { red, green, blue } => Some([red, green, blue]),
                _ => None,
            };
            small_menu(ui, app, "bands", "Band Combination", "Choose a band combination to display the raster layer.", move |ui, app| {
                if bands.len() < 3 {
                    ui.label(tl!("Needs three or more bands."));
                    return;
                }
                if ui.add(egui::Button::selectable(cur == Some([0, 1, 2]), tl!("Natural Color (1, 2, 3)"))).clicked() {
                    let _ = app.run("raster.bands", json!({"preset": "natural"}));
                    ui.close();
                }
                if bands.len() >= 4 && ui.add(egui::Button::selectable(cur == Some([3, 0, 1]), tl!("Color Infrared (4, 1, 2)"))).clicked() {
                    let _ = app.run("raster.bands", json!({"preset": "colorInfrared"}));
                    ui.close();
                }
                ui.separator();
                for (key, label, slot) in [("red", "Red", 0usize), ("green", "Green", 1), ("blue", "Blue", 2)] {
                    ui.menu_button(tl!(label), |ui| {
                        for (i, b) in bands.iter().enumerate() {
                            if ui
                                .add(egui::Button::selectable(
                                    cur.and_then(|c| c.get(slot).copied()) == Some(i),
                                    crate::panes::band_name(b),
                                ))
                                .clicked()
                            {
                                let _ = app.run("raster.bands", json!({ key: i + 1 }));
                                ui.close();
                            }
                        }
                    });
                }
            });
        });
    });
    group(ui, "Enhancement", |ui| {
        ui.vertical(|ui| {
            ui.add_space(2.0);
            for (key, label, icon, value, range, speed) in [
                ("brightness", "Brightness", "brightness", style.brightness, -100.0..=100.0, 0.5),
                ("contrast", "Contrast", "contrast", style.contrast, -100.0..=100.0, 0.5),
                ("gamma", "Gamma", "gamma", style.gamma, 0.1..=10.0, 0.01),
            ] {
                ui.horizontal(|ui| {
                    let t = Tokens::get(ui.ctx());
                    let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
                    icons::paint(ui.painter(), r, icon, t.icon, t.accent);
                    let input_label = ui.label(egui::RichText::new(tl!(label)).size(11.5));
                    ui.add_space((70.0 - text_w(ui, tl!(label), regular(11.5))).max(0.0));
                    let mut v = value;
                    let resp = ui.add(egui::DragValue::new(&mut v).range(range).speed(speed).max_decimals(2)).labelled_by(input_label.id);
                    live_drag(
                        app,
                        ui,
                        &resp,
                        value,
                        v,
                        |app, x| {
                            let rl =
                                app.session.active_layer.and_then(|id| app.session.map_mut().layer_mut(id)).and_then(|l| l.raster.as_mut());
                            if let Some(rl) = rl {
                                match key {
                                    "brightness" => rl.style.brightness = x,
                                    "contrast" => rl.style.contrast = x,
                                    _ => rl.style.gamma = x,
                                }
                            }
                        },
                        |app, x| {
                            let _ = app.run("raster.enhance", json!({ key: x }));
                        },
                    );
                });
            }
        });
        big(
            ui,
            app,
            "undo",
            "Reset",
            "Reset brightness, contrast and gamma.",
            "raster.enhance",
            json!({"brightness": 0, "contrast": 0, "gamma": 1}),
            false,
            false,
        );
    });
    layer_group(app, ui, &layer);
}

fn raster_renderer_icon(r: &RasterRenderer) -> &'static str {
    match r {
        RasterRenderer::Stretch { .. } => "stretch",
        RasterRenderer::Rgb { .. } => "rgb",
        RasterRenderer::Classify { .. } => "classify",
        RasterRenderer::UniqueValues { .. } => "unique",
        RasterRenderer::Colormap => "raster_layer",
        RasterRenderer::ShadedRelief { .. } => "relief",
    }
}

fn raster_data_groups(app: &mut RustGisApp, ui: &mut Ui) {
    group(ui, "Information", |ui| {
        big(
            ui,
            app,
            "identify",
            "Raster\nInformation",
            "Size, bands, statistics and coordinate system.",
            "ui.pane.details",
            Value::Null,
            app.ui.right == Some(RightPane::Details),
            false,
        );
    });
    group(ui, "Export", |ui| {
        big(ui, app, "export_layer", "Export\nRaster", "Save the raster as a GeoTIFF.", "ui.exportRaster", Value::Null, false, false);
    });
}

fn renderer_icon(r: &Renderer) -> &'static str {
    match r {
        Renderer::Single(_) => "single",
        Renderer::Unique { .. } => "unique",
        Renderer::Graduated { .. } => "graduated",
        Renderer::Proportional { .. } => "proportional",
    }
}

/// A ribbon size preview belongs to the layer and label class where the gesture began, even if
/// selection moves.
#[derive(Clone, Default)]
struct LabelSizeDrag {
    layer: u32,
    /// The class dragged on: `None` for Default.
    class: Option<String>,
    before: f32,
    // Keep the widget's precision for command recording; layer storage is f32.
    size: f64,
}

/// Retains the starting size, layer and class while applying each frame's preview.
fn label_size_drag(app: &mut RustGisApp, ui: &Ui, layer: &rustgis_engine::map::Layer, c: &crate::labelpane::Class, size: f64) {
    let key = egui::Id::unique("ribbon_label_size_drag");
    let pending = ui.data_mut(|d| d.remove_temp::<LabelSizeDrag>(key));
    let before = match pending {
        Some(drag) if drag.layer == layer.id && drag.class.as_deref() == c.name => drag.before,
        Some(drag) => {
            apply_label_size_drag(app, drag);
            c.it.size
        }
        None => c.it.size,
    };
    ui.data_mut(|d| d.insert_temp(key, LabelSizeDrag { layer: layer.id, class: c.name.map(String::from), before, size }));
    if let Some(k) = app.session.map_mut().layer_mut(layer.id).and_then(|l| crate::labelpane::class_mut(&mut l.labels, c.name)) {
        k.size = size as f32;
    }
    app.session.revision = app.session.revision.wrapping_add(1);
}

/// Called after the panes even if selection is cleared or the ribbon is hidden mid-gesture.
pub(crate) fn finish_label_size_drag(app: &mut RustGisApp, ui: &Ui) {
    if ui.input(|i| i.pointer.primary_down() && i.focused) {
        return;
    }
    if let Some(drag) = ui.data_mut(|d| d.remove_temp::<LabelSizeDrag>(egui::Id::unique("ribbon_label_size_drag"))) {
        apply_label_size_drag(app, drag);
    }
}

/// Restores the original size before committing on the owning layer and class, so Undo captures
/// that size. A layer or class removed mid-gesture took its preview with it.
fn apply_label_size_drag(app: &mut RustGisApp, drag: LabelSizeDrag) {
    let class = drag.class.as_deref();
    let Some(k) = app.session.map_mut().layer_mut(drag.layer).and_then(|l| crate::labelpane::class_mut(&mut l.labels, class)) else {
        return;
    };
    k.size = drag.before;
    if drag.before != drag.size as f32 {
        let selected = app.session.active_layer.replace(drag.layer);
        let _ = app.run("labels.style", crate::labelpane::named(json!({"size": drag.size}), "class", class));
        app.session.active_layer = selected;
    }
}

/// The label groups edit the class chosen in the Labeling pane, and choose it too (#858).
fn label_groups(app: &mut RustGisApp, ui: &mut Ui) {
    let Some(layer) = app.session.active_layer.and_then(|id| app.session.map().layer(id)).cloned() else { return };
    let c = crate::labelpane::chosen(ui, app, &layer);
    group(ui, "Labels", |ui| {
        let on = layer.labels.enabled;
        big(ui, app, if on { "label" } else { "label_off" }, "Label", "Turn labels on or off.", "labels.enable", Value::Null, on, false);
        big(
            ui,
            app,
            "label_properties",
            "Properties",
            "Every label setting of this layer, in the Labeling pane.",
            "ui.pane.labeling",
            Value::Null,
            app.ui.right == Some(RightPane::Labeling),
            false,
        );
    });
    group(ui, "Label Class", |ui| {
        ui.vertical(|ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let input_label = ui.label(egui::RichText::new(tl!("Class")).size(11.5));
                crate::labelpane::class_combo(app, ui, &layer, &c, 140.0)
                    .on_hover_text(tl!("The class these label groups and the Labeling pane edit."))
                    .labelled_by(input_label.id);
            });
            let fields: Vec<String> = layer.dataset.fields.iter().map(|f| f.name.clone()).collect();
            if let Some(f) = crate::widgets::ribbon_combo(ui, "label_field", "Field", &c.it.field, &fields, 140.0) {
                let _ = app.run("labels.field", c.params(json!({"field": f})));
            }
        });
    });
    group(ui, "Text Symbol", |ui| {
        col(ui, |ui| {
            // The whole range `labels.style` accepts (#457), so a larger label isn't cut back.
            let mut size = f64::from(c.it.size);
            ui.horizontal(|ui| {
                let input_label = ui.label(egui::RichText::new(tl!("Size")).size(11.5));
                let range = rustgis_engine::map::symbology::LABEL_SIZE;
                // Another layer or class gets a distinct widget, so an in-flight drag cannot move to it.
                ui.push_id(("label_size", layer.id, c.name), |ui| {
                    if ui.add(egui::DragValue::new(&mut size).range(range).speed(0.1).suffix(" pt")).labelled_by(input_label.id).changed() {
                        label_size_drag(app, ui, &layer, &c, size);
                    }
                });
            });
            let mut color = crate::theme::c32(c.it.color);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(tl!("Color")).size(11.5));
                ui.push_id(("label_color", layer.id, c.name), |ui| {
                    if crate::widgets::color_button(ui, &mut color).changed() {
                        crate::labelpane::color_drag(app, ui, &layer, c.name, crate::theme::rgba(color));
                    }
                });
            });
        });
        col(ui, |ui| {
            small(ui, app, "label", "Bold", "Bold label text.", "labels.style", c.params(json!({"bold": !c.it.bold})), c.it.bold);
            small(
                ui,
                app,
                "single",
                "Halo",
                "Draw a halo around label text.",
                "labels.style",
                c.params(json!({"halo": !c.it.halo})),
                c.it.halo,
            );
        });
    });
    group(ui, "Label Placement", |ui| {
        let cur = c.it.placement;
        big_menu(ui, app, "placement", "Placement", "Where labels sit relative to their features.", |ui, app| {
            for pl in rustgis_engine::map::LabelPlacement::ALL {
                if ui.radio(cur == pl, tl!(pl.label())).clicked() {
                    let _ = app.run("labels.placement", c.params(json!({"position": pl.code()})));
                    ui.close();
                }
            }
        });
    });
    group(ui, "Label Range", |ui| {
        ui.vertical(|ui| {
            ui.add_space(6.0);
            for (label, cur, key) in [("Out Beyond", c.it.min_scale, "min"), ("In Beyond", c.it.max_scale, "max")] {
                ui.horizontal(|ui| {
                    let input_label = ui.label(egui::RichText::new(tl!(label)).size(11.5));
                    egui::ComboBox::from_id_salt(format!("label_scale_{key}"))
                        .selected_text(scale_label(cur))
                        .width(118.0)
                        .show_ui(ui, |ui| {
                            let mut choices: Vec<Option<f64>> = vec![None];
                            choices.extend(SCALES.iter().map(|s| Some(*s)));
                            for s in choices {
                                if ui.selectable_label(s == cur, scale_label(s)).clicked() {
                                    let _ = app.run("labels.scaleRange", c.params(json!({key: s})));
                                }
                            }
                        })
                        .response
                        .labelled_by(input_label.id);
                });
            }
        });
        col(ui, |ui| {
            for (label, cur, key, desc) in [
                ("Closest", c.it.closest, "closest", "Hide labels when the viewer is closer than this."),
                ("Farthest", c.it.farthest, "farthest", "Hide labels when the viewer is farther than this."),
            ] {
                small_menu(ui, app, "eye", label, desc, |ui, app| {
                    for d in [None, Some(1_000.0), Some(10_000.0), Some(100_000.0), Some(1_000_000.0), Some(10_000_000.0)] {
                        let text = d
                            .map(|d| crate::measure_units::distance(d, app.ui.measurement_imperial))
                            .unwrap_or_else(|| tl!("<None>").to_string());
                        if ui.radio(cur == d, text).clicked() {
                            let _ = app.run("labels.distanceRange", c.params(json!({key: d})));
                            ui.close();
                        }
                    }
                });
            }
            small(
                ui,
                app,
                "scale_label",
                "Clear Limits",
                "Draw labels at every scale and distance.",
                "labels.scaleRange",
                c.params(json!({"clear": true})),
                false,
            );
        });
    });
}

fn feature_data_groups(app: &mut RustGisApp, ui: &mut Ui) {
    group(ui, "Definition Query", |ui| {
        let has = app.session.active_layer.and_then(|id| app.session.map().layer(id)).is_some_and(|l| l.definition_query.is_some());
        big(
            ui,
            app,
            "def_query",
            "Definition\nQuery",
            "Limit which features draw with a SQL expression.",
            "ui.dialog",
            json!({"name": "definitionQuery"}),
            has,
            false,
        );
    });
    group(ui, "Table", |ui| {
        big(ui, app, "table", "Attribute\nTable", "Open the layer's attribute table.", "table.open", Value::Null, false, false);
    });
    group(ui, "Selection", |ui| {
        col(ui, |ui| {
            small(ui, app, "select", "All", "Select every feature.", "select.all", Value::Null, false);
            small(ui, app, "select_loc", "Switch", "Invert the selection.", "select.switch", Value::Null, false);
            small(ui, app, "clear_sel", "Clear", "Clear the selection.", "select.clear", Value::Null, false);
        });
        big(
            ui,
            app,
            "add_data",
            "Layer From\nSelection",
            "Make a new layer of the selected features.",
            "select.layerFromSelection",
            Value::Null,
            false,
            false,
        );
    });
    group(ui, "Data Design", |ui| {
        big(ui, app, "field", "Fields", "Add a field.", "ui.dialog", json!({"name": "addField"}), false, false);
        big(ui, app, "calculate", "Calculate", "Calculate field values.", "ui.dialog", json!({"name": "calculateField"}), false, false);
        let open = app.ui.right == Some(RightPane::Design);
        col(ui, |ui| {
            small(
                ui,
                app,
                "unique",
                "Subtypes",
                "Group features by an integer code, each with default values.",
                "ui.pane.design",
                Value::Null,
                open,
            );
            small(ui, app, "filter", "Domains", "Limit a field to coded values or a range.", "ui.pane.design", Value::Null, open);
        });
        col(ui, |ui| {
            small(
                ui,
                app,
                "calculate",
                "Attribute Rules",
                "Calculate fields and enforce conditions on every edit.",
                "ui.pane.design",
                Value::Null,
                open,
            );
            small(
                ui,
                app,
                "table",
                "Contingent Values",
                "Allow only certain combinations of values across fields.",
                "ui.pane.design",
                Value::Null,
                open,
            );
        });
    });
    let layer = app.session.active_layer.and_then(|id| app.session.map().layer(id)).cloned();
    group(ui, "Archiving", |ui| {
        let on = layer.as_ref().is_some_and(|l| l.design.archive.is_some());
        big(
            ui,
            app,
            "history",
            "Archive",
            "Keep a history of every edit to the layer, to look back or restore.",
            "data.archive",
            json!({"on": !on}),
            on,
            false,
        );
    });
    group(ui, "Relationship", |ui| {
        big_menu(ui, app, "select_loc", "Related\nData", "Select the rows related to the selection.", |ui, app| {
            let rels: Vec<String> = app.session.map().editing.relationships.iter().map(|r| r.name.clone()).collect();
            if rels.is_empty() {
                ui.label(egui::RichText::new(tl!("No relationships yet")).weak());
            }
            for r in rels {
                menu_item(ui, app, "select_loc", &r, "data.related", json!({"name": r}));
            }
            ui.separator();
            menu_item(ui, app, "add_data", "New Relationship…", "ui.pane.design", Value::Null);
        });
    });
    group(ui, "Export", |ui| {
        small_menu(ui, app, "export_layer", "Export", "Export the layer.", |ui, app| {
            menu_item(ui, app, "", "Export Features…", "ui.exportLayer", Value::Null);
            menu_item(ui, app, "", "Export Features (tool)", "ui.gp.open", json!({"tool": "exportFeatures"}));
        });
    });
}

/// A language's own name, or its English name when no installed font can draw it (CJK names on the
/// web before a CJK font has been downloaded).
fn shown_name(ui: &Ui, l: crate::i18n::Lang) -> &'static str {
    if ui.ctx().fonts_mut(|f| f.has_glyphs(&regular(12.5), l.name())) { l.name() } else { l.english() }
}

/// Whose view of disputed borders the built-in countries and world outline show (Settings › Language).
fn worldview_menu(app: &mut RustGisApp, ui: &mut Ui) {
    use rustgis_engine::sample::Worldview;
    let name = |w: Worldview| match w {
        Worldview::Default => tl!("Default"),
        Worldview::China => tl!("China"),
    };
    let auto = app.ui.worldview == crate::i18n::AUTO;
    let current = rustgis_engine::sample::worldview();
    let label = if auto { format!("{} ({})", tl!("Automatic"), name(current)) } else { name(current).to_string() };
    let label = format!("{}\n{label}", tl!("Worldview"));
    big_menu(ui, app, "globe_view", &label, "Whose borders the sample countries and the basemap outline show.", move |ui, app| {
        if ui.selectable_label(auto, tl!("Automatic (system time zone and region)")).clicked() {
            let _ = app.run("ui.worldview", json!({"worldview": crate::i18n::AUTO}));
            ui.close();
        }
        ui.separator();
        for w in [Worldview::Default, Worldview::China] {
            if ui.selectable_label(!auto && w == current, name(w)).clicked() {
                let _ = app.run("ui.worldview", json!({"worldview": w.code()}));
                ui.close();
            }
        }
    });
}

/// The interface language picker (Settings › Language).
fn language_menu(app: &mut RustGisApp, ui: &mut Ui) {
    let current = app.chosen_lang();
    let auto = app.ui.language == crate::i18n::AUTO;
    let label = if auto { format!("{} ({})", tl!("Automatic"), shown_name(ui, current)) } else { shown_name(ui, current).to_string() };
    big_menu(ui, app, "globe_lang", &label, "Choose the interface language.", move |ui, app| {
        if ui.selectable_label(auto, tl!("Automatic (system language)")).clicked() {
            let _ = app.run("ui.language", json!({"code": crate::i18n::AUTO}));
            ui.close();
        }
        ui.separator();
        for l in crate::i18n::Lang::all() {
            let sel = !auto && l == current;
            if ui.selectable_label(sel, shown_name(ui, l)).on_hover_text(l.english()).clicked() {
                let _ = app.run("ui.language", json!({"code": l.code()}));
                ui.close();
            }
        }
    });
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use egui_kittest::kittest::Queryable;

    /// The sample with Cities selected, recording the steps run from here on.
    fn cities() -> RustGisApp {
        let mut app = RustGisApp::with_sample();
        app.session.run("layer.select", &json!({"layer": "Cities"})).unwrap();
        app.ws.recording = Some(Vec::new());
        app
    }

    /// One tab's groups on `app`, as the ribbon draws them.
    fn tab_harness(app: RustGisApp, tab: fn(&mut RustGisApp, &mut Ui)) -> egui_kittest::Harness<'static, RustGisApp> {
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(2400.0, 300.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                ui.horizontal(|ui| tab(app, ui));
            },
            app,
        );
        h.run();
        h
    }

    /// The Labeling tab holds every label group (#897): a feature layer's, then the map's, which
    /// show with no layer too; Style and View have none left.
    #[test]
    fn the_labeling_tab_holds_the_label_groups() {
        let has = |h: &egui_kittest::Harness<'static, RustGisApp>, label: &str| {
            h.query_by_role_and_label(egui::accesskit::Role::Button, label).is_some()
        };
        let h = tab_harness(cities(), labeling_tab);
        assert!(has(&h, "Closest") && has(&h, "View Unplaced"), "a feature layer's groups and the map's");
        let mut app = cities();
        app.session.active_layer = None;
        let h = tab_harness(app, labeling_tab);
        assert!(h.query_by_label("Select a feature layer in Layers to label it.").is_some());
        assert!(!has(&h, "Closest") && has(&h, "View Unplaced"), "the map's settings need no layer");
        assert!(!has(&tab_harness(cities(), style_tab), "Closest"), "Style keeps no label groups");
        assert!(!has(&tab_harness(cities(), view_tab), "View Unplaced"), "View keeps no label groups");
    }

    /// The Drawing group for the selected layer (Cities), as the Style tab draws it.
    fn drawing_harness() -> egui_kittest::Harness<'static, RustGisApp> {
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 800.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                if let Some(layer) = app.session.active_layer.and_then(|id| app.session.map().layer(id)).cloned() {
                    ui.horizontal(|ui| drawing_group(app, ui, &layer));
                }
            },
            cities(),
        );
        h.run();
        h
    }

    /// Drags a number of the harness from `at` by 16, 32 and 48 points, calling `each` between.
    fn drag_by(h: &mut egui_kittest::Harness<'static, RustGisApp>, at: egui::Pos2, mut each: impl FnMut(&RustGisApp)) {
        h.hover_at(at);
        h.drag_at(at);
        h.step();
        for dx in [16.0, 32.0, 48.0] {
            h.hover_at(at + vec2(dx, 0.0));
            h.step();
            each(h.state());
        }
        h.drop_at(at + vec2(48.0, 0.0));
        h.run();
    }

    /// A drag of the point size runs `layer.marker` as it goes (the map follows it), and is one
    /// undo step; the next drag is a step of its own (#1194).
    #[test]
    fn ribbon_marker_size_drag_is_one_undo_step() {
        let mut h = drawing_harness();
        let id = h.state().session.active_layer.unwrap();
        let size = |app: &RustGisApp| app.session.map().layer(id).unwrap().renderer.base_symbol().size;
        let before = size(h.state());
        let undo = h.state().session.undo_depth();
        // Outline, then Size.
        let at = h.query_all_by_role(egui::accesskit::Role::SpinButton).nth(1).unwrap().rect().center();
        let mut seen = Vec::new();
        drag_by(&mut h, at, |app| {
            seen.push(size(app));
            assert_eq!(app.session.undo_depth(), undo + 1, "one step however many frames");
        });
        assert!(seen.windows(2).all(|w| w[1] > w[0]) && seen[0] > before, "the map follows the drag: {seen:?}");
        let first = size(h.state());
        drag_by(&mut h, at, |_| {});
        let second = size(h.state());
        assert!(second > first);
        assert_eq!(h.state().session.undo_depth(), undo + 2, "a second drag is a step of its own");
        h.state_mut().run("edit.undo", json!(null)).unwrap();
        assert_eq!(size(h.state()), first);
        h.state_mut().run("edit.undo", json!(null)).unwrap();
        assert_eq!(size(h.state()), before);
        h.state_mut().run("edit.redo", json!(null)).unwrap();
        assert_eq!(size(h.state()), first);
    }

    /// A drag in the colour picker is one undo step back to the colour from before it (#1194).
    #[test]
    fn ribbon_colour_drag_is_one_undo_step() {
        let mut h = drawing_harness();
        let id = h.state().session.active_layer.unwrap();
        let color = |app: &RustGisApp| app.session.map().layer(id).unwrap().renderer.base_symbol().fill;
        let before = color(h.state());
        let undo = h.state().session.undo_depth();
        h.get_by_role(egui::accesskit::Role::ColorWell).click();
        h.run();
        // Outline and Size precede the picker's R/G/B/A numbers.
        let at = h.query_all_by_role(egui::accesskit::Role::SpinButton).nth(2).unwrap().rect().center();
        drag_by(&mut h, at, |app| {
            assert_ne!(color(app), before, "the map follows the picker");
            assert_eq!(app.session.undo_depth(), undo + 1, "one step however many frames");
        });
        let after = color(h.state());
        h.state_mut().run("edit.undo", json!(null)).unwrap();
        assert_eq!(color(h.state()), before);
        h.state_mut().run("edit.redo", json!(null)).unwrap();
        assert_eq!(color(h.state()), after);
    }

    /// Isolates the label controls; the interrupted-gesture test below checks full-frame wiring.
    fn label_harness() -> egui_kittest::Harness<'static, RustGisApp> {
        label_harness_with(cities(), false)
    }

    /// The label controls on `app`, with the Labeling pane under them when `pane`.
    fn label_harness_with(app: RustGisApp, pane: bool) -> egui_kittest::Harness<'static, RustGisApp> {
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1600.0, 1000.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                if !app.ui.ribbon_minimized {
                    ui.horizontal(|ui| label_groups(app, ui));
                }
                if pane {
                    crate::labelpane::show(app, ui, &crate::theme::Tokens::light());
                }
                finish_label_size_drag(app, ui);
                crate::labelpane::finish_color_drag(app, ui);
            },
            app,
        );
        h.run();
        h
    }

    #[test]
    fn ribbon_label_size_drag_is_one_undo_and_recorded_step() {
        let mut h = label_harness();
        let id = h.state().session.active_layer.unwrap();
        let size = |app: &RustGisApp| app.session.map().layer(id).unwrap().labels.default.size;
        let before = size(h.state());
        let undo = h.state().session.undo_depth();
        let at = h.get_by_role(egui::accesskit::Role::SpinButton).rect().center();
        h.hover_at(at);
        h.drag_at(at);
        h.step();
        for dx in [16.0, 32.0, 48.0] {
            h.hover_at(at + vec2(dx, 0.0));
            h.step();
            assert!(size(h.state()) > before, "the map previews the size while dragging");
            assert_eq!(h.state().session.undo_depth(), undo, "a drag does not run a command each frame");
            assert!(h.state().ws.recording.as_ref().unwrap().is_empty());
        }
        let after = size(h.state());
        h.drop_at(at + vec2(48.0, 0.0));
        h.run();
        assert_eq!(h.state().session.undo_depth(), undo + 1);
        let steps = h.state().ws.recording.as_ref().unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].command, "labels.style");
        assert_eq!(steps[0].params["layer"], "Cities");
        assert_eq!(size(h.state()), after);
        h.state_mut().run("edit.undo", json!(null)).unwrap();
        assert_eq!(size(h.state()), before);
        h.state_mut().run("edit.redo", json!(null)).unwrap();
        assert_eq!(size(h.state()), after);
    }

    #[test]
    fn ribbon_label_size_drag_keeps_its_layer_when_selection_changes() {
        for clear_selection in [false, true] {
            let mut h = label_harness();
            let cities = h.state().session.active_layer.unwrap();
            let countries = h.state().session.map().layers.iter().find(|l| l.name == "Countries").unwrap().id;
            let size = |app: &RustGisApp, id| app.session.map().layer(id).unwrap().labels.default.size;
            let before = size(h.state(), cities);
            let countries_before = size(h.state(), countries);
            let undo = h.state().session.undo_depth();
            let at = h.get_by_role(egui::accesskit::Role::SpinButton).rect().center();
            h.hover_at(at);
            h.drag_at(at);
            h.step();
            h.hover_at(at + vec2(32.0, 0.0));
            h.step();
            let after = size(h.state(), cities);
            assert!(after > before);
            // Selection may also change through the Assistant or relay while the pointer is held.
            let selected = if clear_selection { None } else { Some(countries) };
            h.state_mut().session.active_layer = selected;
            h.step();
            h.drop_at(at + vec2(32.0, 0.0));
            h.run();
            assert_eq!(size(h.state(), countries), countries_before);
            assert_eq!(size(h.state(), cities), after);
            assert_eq!(h.state().session.active_layer, selected);
            assert_eq!(h.state().session.undo_depth(), undo + 1);
            let steps = h.state().ws.recording.as_ref().unwrap();
            assert_eq!(steps.len(), 1);
            assert_eq!(steps[0].params["layer"], "Cities");
            h.state_mut().run("edit.undo", json!(null)).unwrap();
            assert_eq!(size(h.state(), cities), before);
            assert_eq!(size(h.state(), countries), countries_before);
            h.state_mut().run("edit.redo", json!(null)).unwrap();
            assert_eq!(size(h.state(), cities), after);
        }
    }

    /// Exercises the app frame so removing the pane finalizer breaks gesture completion.
    #[test]
    fn ribbon_label_size_drag_finishes_when_hidden_or_unfocused() {
        for hide_ribbon in [false, true] {
            let mut app = RustGisApp::with_sample();
            app.session.run("layer.select", &json!({"layer": "Cities"})).unwrap();
            app.ui.tab = "Labeling".into();
            let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 800.0)).with_max_steps(64).build_ui_state(
                |ui, app: &mut RustGisApp| {
                    let ctx = ui.ctx().clone();
                    app.logic(&ctx);
                    app.ui(ui);
                },
                app,
            );
            h.run();
            h.state_mut().ws.recording = Some(Vec::new());
            let id = h.state().session.active_layer.unwrap();
            let size = |app: &RustGisApp| app.session.map().layer(id).unwrap().labels.default.size;
            let before = size(h.state());
            let undo = h.state().session.undo_depth();
            // Text Symbol's size is the Labeling tab's numeric control, drawn after any other.
            let at = h.query_all_by_role(egui::accesskit::Role::SpinButton).last().unwrap().rect().center();
            h.hover_at(at);
            h.drag_at(at);
            h.step();
            h.hover_at(at + vec2(32.0, 0.0));
            h.step();
            let after = size(h.state());
            assert!(after > before);
            if hide_ribbon {
                h.state_mut().ui.ribbon_minimized = true;
                h.step();
                h.drop_at(at + vec2(32.0, 0.0));
            } else {
                h.input_mut().focused = false;
                h.event(egui::Event::WindowFocused(false));
            }
            h.run();
            assert_eq!(size(h.state()), after);
            assert_eq!(h.state().session.undo_depth(), undo + 1);
            assert_eq!(h.state().ws.recording.as_ref().unwrap().len(), 1);
            h.state_mut().run("edit.undo", json!(null)).unwrap();
            assert_eq!(size(h.state()), before);
        }
    }

    #[test]
    fn ribbon_label_colour_drag_is_one_undo_and_recorded_step() {
        let mut h = label_harness();
        let id = h.state().session.active_layer.unwrap();
        let color = |app: &RustGisApp| app.session.map().layer(id).unwrap().labels.default.color;
        let before = color(h.state());
        let undo = h.state().session.undo_depth();
        h.get_by_role(egui::accesskit::Role::ColorWell).click();
        h.run();
        // The ribbon size precedes the colour picker's R/G/B/A numeric controls.
        let at = h.query_all_by_role(egui::accesskit::Role::SpinButton).nth(1).unwrap().rect().center();
        h.hover_at(at);
        h.drag_at(at);
        h.step();
        for dx in [16.0, 32.0, 48.0] {
            h.hover_at(at + vec2(dx, 0.0));
            h.step();
            assert_ne!(color(h.state()), before, "the map previews the colour while dragging");
            assert_eq!(h.state().session.undo_depth(), undo, "the picker does not run a command each frame");
            assert!(h.state().ws.recording.as_ref().unwrap().is_empty());
        }
        let after = color(h.state());
        h.drop_at(at + vec2(48.0, 0.0));
        h.run();
        assert_eq!(h.state().session.undo_depth(), undo + 1);
        let steps = h.state().ws.recording.as_ref().unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].command, "labels.style");
        assert_eq!(steps[0].params["layer"], "Cities");
        assert_eq!(color(h.state()), after);
        h.state_mut().run("edit.undo", json!(null)).unwrap();
        assert_eq!(color(h.state()), before);
        h.state_mut().run("edit.redo", json!(null)).unwrap();
        assert_eq!(color(h.state()), after);
    }

    #[test]
    fn ribbon_label_colour_drag_keeps_its_layer_when_selection_changes() {
        let mut h = label_harness();
        let cities = h.state().session.active_layer.unwrap();
        let countries = h.state().session.map().layers.iter().find(|l| l.name == "Countries").unwrap().id;
        let color = |app: &RustGisApp, id| app.session.map().layer(id).unwrap().labels.default.color;
        let before = color(h.state(), cities);
        let countries_before = color(h.state(), countries);
        let undo = h.state().session.undo_depth();
        h.get_by_role(egui::accesskit::Role::ColorWell).click();
        h.run();
        let at = h.query_all_by_role(egui::accesskit::Role::SpinButton).nth(1).unwrap().rect().center();
        h.hover_at(at);
        h.drag_at(at);
        h.step();
        h.hover_at(at + vec2(32.0, 0.0));
        h.step();
        let after = color(h.state(), cities);
        assert_ne!(after, before);
        h.state_mut().session.active_layer = Some(countries);
        h.step();
        h.hover_at(at + vec2(64.0, 0.0));
        h.step();
        h.drop_at(at + vec2(64.0, 0.0));
        h.run();
        assert_eq!(color(h.state(), countries), countries_before);
        assert_eq!(color(h.state(), cities), after);
        assert_eq!(h.state().session.active_layer, Some(countries));
        assert_eq!(h.state().session.undo_depth(), undo + 1);
        let steps = h.state().ws.recording.as_ref().unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].params["layer"], "Cities");
        h.state_mut().run("edit.undo", json!(null)).unwrap();
        assert_eq!(color(h.state(), cities), before);
        assert_eq!(color(h.state(), countries), countries_before);
        h.state_mut().run("edit.redo", json!(null)).unwrap();
        assert_eq!(color(h.state(), cities), after);
    }

    #[test]
    fn ribbon_label_size_text_input_preserves_decimal_command_value() {
        let mut h = label_harness();
        let id = h.state().session.active_layer.unwrap();
        let before = h.state().session.map().layer(id).unwrap().labels.default.size;
        let undo = h.state().session.undo_depth();
        h.get_by_role(egui::accesskit::Role::SpinButton).focus();
        h.run();
        // Inserts the complete value in one text event; per-keystroke undo grouping is separate.
        h.get_by_role(egui::accesskit::Role::SpinButton).type_text("14.3");
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(h.state().session.map().layer(id).unwrap().labels.default.size, 14.3_f32);
        assert_eq!(h.state().session.undo_depth(), undo + 1);
        assert_eq!(h.state().ws.recording.as_ref().unwrap().len(), 1);
        assert_eq!(h.state().ws.recording.as_ref().unwrap()[0].params["size"], json!(14.3));
        h.state_mut().run("edit.undo", json!(null)).unwrap();
        assert_eq!(h.state().session.map().layer(id).unwrap().labels.default.size, before);
    }

    /// The sample's Cities with a second label class, Big, added before any step is recorded.
    fn cities_with_big() -> RustGisApp {
        let mut app = cities();
        app.session.run("labels.class.add", &json!({"name": "Big"})).unwrap();
        app
    }

    fn labels(app: &RustGisApp) -> rustgis_engine::map::symbology::Labeling {
        app.session.map().layer(app.session.active_layer.unwrap()).unwrap().labels.clone()
    }

    /// The classes the Class choosers show, the ribbon's first.
    fn classes_shown(h: &egui_kittest::Harness<'static, RustGisApp>) -> Vec<String> {
        h.query_all_by_role_and_label(egui::accesskit::Role::ComboBox, "Class").map(|n| n.value().unwrap_or_default()).collect()
    }

    /// Chooses class `to` in the `nth` Class chooser (the ribbon's first).
    fn pick_class(h: &mut egui_kittest::Harness<'static, RustGisApp>, nth: usize, to: &str) {
        h.query_all_by_role_and_label(egui::accesskit::Role::ComboBox, "Class").nth(nth).unwrap().click();
        h.run();
        h.get_by_label(to).click();
        h.run();
    }

    /// The ribbon and the Labeling pane choose one class: a class chosen in either is chosen in both
    /// (#858).
    #[test]
    fn the_ribbon_and_the_pane_choose_one_class() {
        let mut h = label_harness_with(cities_with_big(), true);
        assert_eq!(classes_shown(&h), ["Default", "Default"]);
        pick_class(&mut h, 0, "Big");
        assert_eq!(classes_shown(&h), ["Big", "Big"], "chosen in the ribbon");
        pick_class(&mut h, 1, "Default");
        assert_eq!(classes_shown(&h), ["Default", "Default"], "chosen in the pane");
    }

    /// The ribbon's label groups edit the chosen class, and only it; its steps name it, and
    /// Default's name none.
    #[test]
    fn the_ribbon_edits_the_chosen_class() {
        let mut h = label_harness_with(cities_with_big(), false);
        let before = labels(h.state()).default;
        pick_class(&mut h, 0, "Big");
        h.get_by_label("Bold").click();
        h.run();
        h.get_by_label("Clear Limits").click();
        h.run();
        let l = labels(h.state());
        assert_eq!(l.class("Big").unwrap().bold, !before.bold);
        assert_eq!(l.default.bold, before.bold, "Default is left as it was");
        pick_class(&mut h, 0, "Default");
        h.get_by_label("Halo").click();
        h.run();
        assert_ne!(labels(h.state()).default.halo, before.halo);
        let steps = h.state().ws.recording.clone().unwrap();
        let classes: Vec<_> = steps.iter().map(|s| (s.command.as_str(), s.params.get("class").cloned())).collect();
        assert_eq!(
            classes,
            [("labels.style", Some(json!("Big"))), ("labels.scaleRange", Some(json!("Big"))), ("labels.style", None)],
            "Default's step names no class"
        );
    }

    /// A size dragged on a class previews on it and lets go as one step on it; Default is left as it
    /// was.
    #[test]
    fn ribbon_label_size_drag_on_a_class_is_one_step_on_it() {
        let mut h = label_harness_with(cities_with_big(), false);
        pick_class(&mut h, 0, "Big");
        let size = |app: &RustGisApp| labels(app).class("Big").unwrap().size;
        let (before, default) = (size(h.state()), labels(h.state()).default.size);
        let undo = h.state().session.undo_depth();
        let at = h.get_by_role(egui::accesskit::Role::SpinButton).rect().center();
        h.hover_at(at);
        h.drag_at(at);
        h.step();
        h.hover_at(at + vec2(32.0, 0.0));
        h.step();
        assert!(size(h.state()) > before, "the map previews the size on the class");
        let after = size(h.state());
        h.drop_at(at + vec2(32.0, 0.0));
        h.run();
        assert_eq!(size(h.state()), after);
        assert_eq!(labels(h.state()).default.size, default);
        assert_eq!(h.state().session.undo_depth(), undo + 1);
        let steps = h.state().ws.recording.as_ref().unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].params["class"], "Big");
        h.state_mut().run("edit.undo", json!(null)).unwrap();
        assert_eq!(size(h.state()), before);
    }

    /// Every publication profile has its literal name in the menu, the engine's label word for word
    /// (the test runs in English), so neither can be renamed without the other.
    #[test]
    fn every_publication_profile_is_named_by_a_literal() {
        for p in rustgis_engine::cmd::layout::PUBLICATION_PROFILES {
            assert_eq!(publication_label(p.id), Some(p.label), "{}", p.id);
        }
    }

    /// Every sheet style has its literal name in the menu, the engine's name word for word, and
    /// says what it looks like.
    #[test]
    fn every_publication_style_is_named_by_a_literal() {
        for (id, name) in rustgis_engine::cmd::layout::PUBLICATION_STYLES {
            let (label, about) = publication_style(id).unwrap_or_else(|| panic!("{id}"));
            assert_eq!(label, *name, "{id}");
            assert!(!about.is_empty(), "{id}");
        }
    }

    /// New Layout › Publication Layout › a sheet › a style makes that sheet in that style, titled
    /// with the map's name; a figure has no styles to pick and is made at once.
    #[test]
    fn a_publication_sheet_is_made_in_the_style_picked() {
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1600.0, 1000.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                ui.horizontal(|ui| project_group(app, ui));
            },
            cities(),
        );
        h.run();
        let open = |h: &mut egui_kittest::Harness<'static, RustGisApp>, path: &[&str]| {
            h.get_by_label("New Layout").click();
            h.run();
            for row in path {
                h.get_by_label(row).hover();
                h.run();
            }
        };
        let before = h.state().session.project.layouts.len();
        open(&mut h, &["Publication Layout", "A4 portrait sheet"]);
        h.get_by_label("Panel").click();
        h.run();
        let app = h.state();
        assert_eq!(app.session.project.layouts.len(), before + 1);
        let l = app.session.project.layouts.last().unwrap();
        assert!(l.elements.iter().any(|e| e.name == "Panel Box"), "the panel style");
        let title = app.session.map().name.clone();
        assert!(
            l.elements.iter().any(|e| matches!(&e.kind, rustgis_engine::map::layout::ElementKind::Text { text, .. } if *text == title)),
            "titled with the map's name"
        );
        assert_eq!(app.ui.tab, "Publish");
        assert!(h.query_by_label("Banner").is_none(), "the menu closed");
        // A figure: no styles under it.
        open(&mut h, &["Publication Layout"]);
        h.get_by_label("Journal figure, 1 column (90 mm)").click();
        h.run();
        assert_eq!(h.state().session.project.layouts.len(), before + 2);
        let l = h.state().session.project.layouts.last().unwrap();
        assert!(l.elements.iter().all(|e| e.name != "Panel Box" && e.name != "Title"), "a figure, its title left to the caption");
    }

    #[test]
    fn saved_hidden_tabs_never_hide_the_fixed_tabs() {
        let saved = ["Settings", "Help", "Edit", "Share", "Edit"].iter().map(|t| t.to_string()).collect();
        assert_eq!(current_hidden_tabs(saved), vec!["Edit".to_string(), "Publish".to_string()]);
    }
}
