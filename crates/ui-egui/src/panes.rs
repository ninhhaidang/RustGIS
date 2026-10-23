//! Docked panes: Layers and Browser on the left; Geoprocessing, Symbology, History and Details
//! on the right.

use std::collections::HashMap;

use egui::{Align2, CornerRadius, Rect, Sense, Stroke, Ui, pos2, vec2};
use rustgis_engine::Session;
use rustgis_engine::analysis::{ParamKind, tools};
use rustgis_engine::data::GeometryType;
use rustgis_engine::map::raster::{Normalizer, RasterLegend, RasterStyle};
use rustgis_engine::map::symbology::{
    ClassRef, ClassStyle, LineCap, LineJoin, LineStroke, MAX_LABEL, MAX_STROKE_OFFSET, MAX_STROKE_WIDTH, MAX_STROKES, MAX_UNIQUE,
    normalize_label,
};
use rustgis_engine::map::tiles::{TileKind, TileLayer};
use rustgis_engine::map::{
    ClassMethod, Layer, Marker, Ramp, RampGroup, RasterDefaults, RasterRenderer, Renderer, Resampling, Stretch, Symbol,
};
use serde_json::{Value, json};

use crate::theme::{Tokens, c32, regular, rgba, semibold};
use crate::widgets::Dragged;
use crate::{LayersView, LeftPane, RightPane, RustGisApp, icons};

pub fn show(app: &mut RustGisApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let frame = egui::Frame::NONE.fill(t.pane).inner_margin(egui::Margin::ZERO);
    if let Some(left) = app.ui.left {
        egui::Panel::left("left_pane").resizable(true).default_size(270.0).size_range(200.0..=480.0).frame(frame).show(ui, |ui| {
            left_header(app, ui, left, &t);
            // The search box sits at the foot of every left pane, the app's lower-left corner.
            crate::searchbox::show(app, ui, &t);
            // As on the right: a row wider than the pane (a long folder path, a long favourite) is
            // clipped at its edge, never reported to the panel, which would widen to fit it (#353).
            let rect = ui.available_rect_before_wrap();
            let mut body = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(*ui.layout()));
            match left {
                LeftPane::Contents => contents(app, &mut body, &t),
                LeftPane::Catalog => catalog(app, &mut body, &t),
                LeftPane::Sources => crate::sourcespane::show(app, &mut body, &t),
            }
            ui.advance_cursor_after_rect(rect);
        });
    }
    // A point picked for the Time Series pane is taken only while that pane shows: with it
    // closed or another in view, the pick ends rather than waiting with a stale click.
    if app.ui.right != Some(RightPane::TimeSeries) && app.workflow.pick.as_ref().is_some_and(|p| p.purpose == "ts_point") {
        app.workflow.pick = None;
    }
    // A Zarr summary or a trend running on its thread lands whatever pane is in view (both are
    // looked at: `|`, not `||`), and the frames go on while one runs so it's taken when it ends.
    #[cfg(not(target_arch = "wasm32"))]
    if crate::workspacepanes::land_summary(app) | crate::analysispane::land_trend(app) {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
    }
    // The pane in view always has its tab, however it was set.
    if let Some(right) = app.ui.right
        && !app.ui.right_open.contains(&right)
    {
        app.ui.right_open.push(right);
    }
    if let Some(right) = app.ui.right {
        // The Web Data pane's searches and lists want room: it keeps its own width.
        let (id, width) = if right == RightPane::WebData { ("right_pane_web", 440.0) } else { ("right_pane", 320.0) };
        egui::Panel::right(id).resizable(true).default_size(width).size_range(260.0..=640.0).frame(frame).show(ui, |ui| {
            // The pane is as wide as the user made it. Content wider than that is clipped, never
            // reported to the panel: egui would keep the panel's edge where the content ends, off
            // the window, and give the map the room it no longer covers (#307).
            let rect = ui.available_rect_before_wrap();
            let mut body = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(*ui.layout()));
            right_pane(app, &mut body, right, &t);
            ui.advance_cursor_after_rect(rect);
        });
    }
    // Here rather than in the Symbology and Labeling panes, so a drag still finishes if the pane
    // closes meanwhile; and after the panes, so a change the picker reports as the pointer is let
    // go joins the drag instead of running a command of its own.
    finish_class_color_drag(app, ui);
    crate::labelpane::finish_color_drag(app, ui);
    crate::ribbon::finish_label_size_drag(app, ui);
}

/// Closes the pane that command `id` (a `ui.pane.*`) opens, on its side; false when `id` isn't a
/// pane command. `{"close": true}` means this for every pane command, whatever else it is given.
/// Only the open panes are looked through (closing one that isn't open does nothing), so a new
/// right pane needs nothing here.
pub(crate) fn close_pane_of(app: &mut RustGisApp, id: &str) -> bool {
    if !id.starts_with("ui.pane.") {
        return false;
    }
    if let Some((pane, ..)) = LEFT_PANES.iter().find(|p| p.3 == id) {
        if app.ui.left == Some(*pane) {
            app.ui.left = None;
        }
    } else if let Some(pane) = app.ui.right_open.iter().copied().find(|p| right_title(*p).1 == id) {
        app.ui.close_right(pane);
    }
    true
}

/// A right pane's title (its tab) and the command that opens, shows or closes it.
fn right_title(pane: RightPane) -> (&'static str, &'static str) {
    match pane {
        RightPane::Geoprocessing => ("Tools", "ui.pane.geoprocessing"),
        RightPane::Symbology => ("Symbology", "ui.pane.symbology"),
        RightPane::History => ("History", "ui.pane.history"),
        RightPane::Details => ("Details", "ui.pane.details"),
        RightPane::Editing => ("Editing", "ui.pane.editing"),
        RightPane::Design => ("Data Design", "ui.pane.design"),
        RightPane::Workflows => ("Workflows", "ui.pane.workflows"),
        RightPane::Script => ("Script", "ui.pane.script"),
        RightPane::Tasks => ("Tasks", "ui.pane.tasks"),
        RightPane::Jobs => ("Workflow Manager", "ui.pane.jobs"),
        RightPane::Profile => ("Profile", "ui.pane.profile"),
        RightPane::TimeSeries => ("Time Series", "ui.pane.timeSeries"),
        RightPane::Diagnostics => ("Diagnostic Monitor", "ui.pane.diagnostics"),
        RightPane::Infographics => ("Infographics", "ui.pane.infographics"),
        RightPane::Report => ("Report", "ui.pane.report"),
        RightPane::Presentation => ("Presentation", "ui.pane.presentation"),
        RightPane::Notebook => ("Notebook", "ui.pane.notebook"),
        RightPane::Assistant => ("AI Assistant", "ui.pane.assistant"),
        RightPane::Shares => ("Shared Items", "ui.pane.shares"),
        RightPane::Sql => ("SQL", "ui.pane.sql"),
        RightPane::Directions => ("Directions", "ui.pane.directions"),
        RightPane::Bookmarks => ("Bookmarks", "ui.pane.bookmarks"),
        RightPane::Format => ("Format", "ui.pane.format"),
        RightPane::Labeling => ("Labeling", "ui.pane.labeling"),
        // Its tab says which service it shows (see `right_name`).
        RightPane::WebData => ("Web Data", "ui.pane.webData"),
        // Its tab says whose panel it is (see `right_name`).
        RightPane::Plugin => ("Plugin", "ui.pane.plugin"),
    }
}

/// A right pane's tab, in the interface language: a plugin's panel is named by its title.
fn right_name(app: &RustGisApp, pane: RightPane) -> String {
    match pane {
        RightPane::Plugin => crate::pluginpanel::title(app),
        RightPane::WebData => tl!(web_data_title(app)).to_string(),
        _ => tl!(right_title(pane).0).to_string(),
    }
}

fn right_pane(app: &mut RustGisApp, ui: &mut Ui, right: RightPane, t: &Tokens) {
    right_header(app, ui, right, t);
    // The chat lays out its own scrolling, with the message box kept at the bottom.
    if right == RightPane::Assistant {
        crate::assistantpane::show(app, ui, t);
        return;
    }
    // The SQL pane scrolls its own results sideways.
    if right == RightPane::Sql {
        crate::sqlpane::show(app, ui, t);
        return;
    }
    // The Script pane fits its box to the pane's height, the buttons kept in view (#653).
    if right == RightPane::Script {
        crate::analysispane::script_pane(app, ui, t);
        return;
    }
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        egui::Frame::NONE.inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| match right {
            RightPane::Geoprocessing => geoprocessing(app, ui, t),
            RightPane::Symbology => symbology(app, ui, t),
            RightPane::History => history(app, ui, t),
            RightPane::Details => details(app, ui, t),
            RightPane::Editing => crate::editpanes::editing(app, ui, t),
            RightPane::Design => crate::editpanes::design(app, ui, t),
            RightPane::Workflows => crate::analysispane::workflows(app, ui, t),
            RightPane::Tasks => crate::workspacepanes::tasks(app, ui, t),
            RightPane::Jobs => crate::workspacepanes::jobs(app, ui, t),
            RightPane::Profile => crate::workspacepanes::profile(app, ui, t),
            RightPane::TimeSeries => crate::workspacepanes::time_series(app, ui, t),
            RightPane::Diagnostics => crate::workspacepanes::diagnostics(app, ui, t),
            RightPane::Infographics => crate::workspacepanes::infographics(app, ui, t),
            RightPane::Report => crate::reportpane::report(app, ui, t),
            RightPane::Presentation => crate::docpanes::presentation(app, ui, t),
            RightPane::Notebook => crate::docpanes::notebook(app, ui, t),
            RightPane::Assistant | RightPane::Sql | RightPane::Script => {}
            RightPane::Shares => crate::docpanes::shares(app, ui, t),
            RightPane::Directions => crate::navui::show(app, ui, t),
            RightPane::Bookmarks => crate::bookmarkpane::show(app, ui, t),
            RightPane::Format => crate::graphicformat::show(app, ui, t),
            RightPane::Labeling => crate::labelpane::show(app, ui, t),
            RightPane::Plugin => crate::pluginpanel::show(app, ui, t),
            RightPane::WebData => web_data(app, ui),
        });
    });
}

/// The Web Data services shown in the Web Data pane, by the `ui.dialog` name that opens them, and
/// their titles.
const WEB_DATA: [(&str, &str); 15] = [
    ("opendata", "Search Open Data"),
    ("stac", "STAC Search"),
    ("osm", "OpenStreetMap"),
    ("boundaries", "Administrative Boundaries"),
    ("overture", "Overture Maps"),
    ("sourcecoop", "Source Cooperative"),
    ("naturalEarth", "Natural Earth"),
    ("nationalMap", "The National Map"),
    ("nasaEarthdata", "NASA Earthdata"),
    ("enviroAtlas", "EnviroAtlas"),
    ("femaNfhl", "FEMA NFHL"),
    ("fieldsOfTheWorld", "Fields of the World"),
    ("earthaccess", "NASA Earthaccess"),
    ("nldi", "USGS NLDI"),
    ("embeddings", "Satellite Embeddings"),
];

/// The Web Data service a `ui.dialog` name opens, if it is one.
pub fn web_data_name(name: &str) -> Option<&'static str> {
    WEB_DATA.iter().find(|(n, _)| *n == name).map(|(n, _)| *n)
}

/// The Web Data pane's title: its service's.
fn web_data_title(app: &RustGisApp) -> &'static str {
    match app.ui.web_data {
        "opendata" => app.ui.opendata.title(),
        "stac" => crate::stacui::title(app),
        n => WEB_DATA.iter().find(|(k, _)| *k == n).map(|(_, t)| *t).unwrap_or("Web Data"),
    }
}

/// The Web Data pane: its service's search, choices and results.
fn web_data(app: &mut RustGisApp, ui: &mut Ui) {
    ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
    match app.ui.web_data {
        "stac" => crate::stacui::dialog(app, ui),
        "osm" => crate::portalsui::osm(app, ui),
        "boundaries" => crate::portalsui::boundaries(app, ui),
        "overture" => crate::catalogsui::overture(app, ui),
        "sourcecoop" | "naturalEarth" => crate::catalogsui::sourcecoop(app, ui),
        "nationalMap" => crate::webdataui::national_map(app, ui),
        "nasaEarthdata" => crate::webdataui::nasa_earthdata(app, ui),
        "enviroAtlas" => crate::webdataui::enviroatlas(app, ui),
        "femaNfhl" => crate::webdataui::fema_nfhl(app, ui),
        "fieldsOfTheWorld" => crate::webdataui::fields_of_the_world(app, ui),
        "earthaccess" => crate::earthaccessui::pane(app, ui),
        "nldi" => crate::nldiui::pane(app, ui),
        "embeddings" => crate::embeddingsui::pane(app, ui),
        _ => crate::opendataui::dialog(app, ui),
    }
}

/// The right side's header (#303): a tab for each pane open, in the order opened, the one in view
/// underlined. A tab's ✕ closes its pane, a click on another tab shows it, and a right-click
/// closes it, the others or all. Tabs that don't fit are listed in a » menu, and the pane in view
/// always keeps its tab on the strip. The ✕ at the end closes every pane.
fn right_header(app: &mut RustGisApp, ui: &mut Ui, right: RightPane, t: &Tokens) {
    let r = header_strip(ui, t);
    let (pad, cross, gap, more_w) = (9.0, 16.0, 2.0, 28.0);
    let open = app.ui.right_open.clone();
    let names: Vec<String> = open.iter().map(|p| right_name(app, *p)).collect();
    let font = |on: bool| if on { semibold(12.5) } else { regular(12.5) };
    let widths: Vec<f32> =
        open.iter().zip(&names).map(|(p, n)| pad + crate::widgets::text_w(ui, n, font(*p == right)) + 4.0 + cross + 5.0).collect();
    // Which tabs fit (by index): all, or the first that leave room for the » and the one in view.
    // Room left of the ✕ that closes them all.
    let all_w = 30.0;
    let room = r.width() - 6.0 - 4.0 - all_w;
    let mut shown: Vec<usize> = (0..open.len()).collect();
    if widths.iter().map(|w| w + gap).sum::<f32>() > room {
        let room = room - more_w;
        let mut used = 0.0;
        shown = (0..open.len())
            .take_while(|i| {
                used += widths.get(*i).copied().unwrap_or(0.0) + gap;
                used <= room
            })
            .collect();
        if let Some(on) = open.iter().position(|p| *p == right).filter(|on| !shown.contains(on)) {
            let need = widths.get(on).copied().unwrap_or(0.0) + gap;
            while !shown.is_empty() && shown.iter().map(|i| widths.get(*i).copied().unwrap_or(0.0) + gap).sum::<f32>() + need > room {
                shown.pop();
            }
            shown.push(on);
        }
    }
    let mut x = r.min.x + 6.0;
    for &i in &shown {
        let (Some(pane), Some(name), Some(w)) = (open.get(i).copied(), names.get(i).map(String::as_str), widths.get(i).copied()) else {
            continue;
        };
        let (_, cmd) = right_title(pane);
        let on = pane == right;
        let tr = Rect::from_min_size(pos2(x, r.min.y), vec2(w, r.height()));
        let resp = ui.interact(tr, egui::Id::new(("right_pane_tab", pane as u8)), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, name));
        let p = ui.painter();
        if resp.hovered() && !on {
            p.rect_filled(tr.shrink2(vec2(1.0, 5.0)), CornerRadius::same(4), t.hover);
        }
        let ink = if on { t.text } else { t.text_dim };
        p.with_clip_rect(tr).text(pos2(tr.min.x + pad, tr.center().y), Align2::LEFT_CENTER, name, font(on), ink);
        if on {
            p.line_segment([pos2(tr.min.x + 4.0, r.max.y - 1.5), pos2(tr.max.x - 4.0, r.max.y - 1.5)], Stroke::new(2.0, t.accent));
        }
        // The tab's ✕ closes its pane, in view or not.
        let cr = Rect::from_center_size(pos2(tr.max.x - 5.0 - cross / 2.0, tr.center().y), vec2(cross, cross));
        let x_resp = ui.interact(cr, egui::Id::new(("right_pane_close", pane as u8)), Sense::click());
        let tip = crate::i18n::fmt("Close {name}", &[("name", name)]);
        x_resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &tip));
        if x_resp.hovered() {
            ui.painter().rect_filled(cr, CornerRadius::same(3), t.hover);
        }
        icons::paint(ui.painter(), cr.shrink(4.0), "close", t.text_dim, t.accent);
        if x_resp.on_hover_text(tip).clicked() {
            let _ = app.run(cmd, json!({"close": true}));
        } else if resp.clicked() && !on {
            let _ = app.run(cmd, Value::Null);
        }
        resp.context_menu(|ui| {
            let close: Option<Vec<RightPane>> = if ui.button(tl!("Close")).clicked() {
                Some(vec![pane])
            } else if ui.add_enabled(open.len() > 1, egui::Button::new(tl!("Close Others"))).clicked() {
                Some(open.iter().copied().filter(|p| *p != pane).collect())
            } else if ui.button(tl!("Close All")).clicked() {
                Some(open.clone())
            } else {
                None
            };
            if let Some(panes) = close {
                close_right_panes(app, &panes);
                ui.close();
            }
        });
        x += w + gap;
    }
    // The ✕ at the end closes them all.
    let all = Rect::from_center_size(pos2(r.max.x - 18.0, r.center().y), vec2(22.0, 22.0));
    let all_resp = ui.interact(all, egui::Id::new("right_pane_close_all"), Sense::click());
    all_resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tl!("Close all panes")));
    if all_resp.hovered() {
        ui.painter().rect_filled(all, CornerRadius::same(4), t.hover);
    }
    icons::paint(ui.painter(), all.shrink(5.0), "close", t.text_dim, t.accent);
    if all_resp.on_hover_text(tl!("Close all panes")).clicked() {
        close_right_panes(app, &open);
    }
    // The rest, in a » menu.
    let hidden: Vec<RightPane> = (0..open.len()).filter(|i| !shown.contains(i)).filter_map(|i| open.get(i).copied()).collect();
    if !hidden.is_empty() {
        let mr = Rect::from_center_size(pos2(r.max.x - all_w - more_w / 2.0, r.center().y), vec2(more_w - 4.0, 22.0));
        let more = ui.interact(mr, egui::Id::new("right_pane_more"), Sense::click());
        let tip = crate::i18n::fmt("{n} more panes", &[("n", &hidden.len().to_string())]);
        more.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &tip));
        if more.hovered() {
            ui.painter().rect_filled(mr, CornerRadius::same(4), t.hover);
        }
        ui.painter().text(mr.center(), Align2::CENTER_CENTER, "»", semibold(14.0), t.text_dim);
        let more = more.on_hover_text(tip);
        egui::Popup::menu(&more).show(|ui| {
            for pane in hidden {
                let cmd = right_title(pane).1;
                if ui.button(right_name(app, pane)).clicked() {
                    let _ = app.run(cmd, Value::Null);
                    ui.close();
                }
            }
        });
    }
}

/// Closes right panes `panes`, each through its command (`{"close": true}`).
fn close_right_panes(app: &mut RustGisApp, panes: &[RightPane]) {
    for pane in panes {
        let _ = app.run(right_title(*pane).1, json!({"close": true}));
    }
}

/// The 34 px band a pane header sits on.
fn header_strip(ui: &mut Ui, t: &Tokens) -> Rect {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::hover());
    ui.painter().rect_filled(r, 0.0, t.pane_header);
    ui.painter().line_segment([pos2(r.min.x, r.max.y - 0.5), pos2(r.max.x, r.max.y - 0.5)], Stroke::new(1.0, t.border));
    r
}

/// The ✕ at the right end of a pane header `r`, running `close`.
fn close_button(app: &mut RustGisApp, ui: &mut Ui, r: Rect, close: &str, salt: &str, t: &Tokens) {
    let cr = Rect::from_center_size(pos2(r.max.x - 18.0, r.center().y), vec2(22.0, 22.0));
    let resp = ui.interact(cr, egui::Id::new(("pane_close", salt)), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(cr, CornerRadius::same(4), t.hover);
    }
    icons::paint(ui.painter(), cr.shrink(5.0), "close", t.text_dim, t.accent);
    if resp.on_hover_text(tl!("Close")).clicked() {
        let _ = app.run(close, Value::Null);
    }
}

/// The left panes, each with its icon and the command that opens it (#299).
const LEFT_PANES: [(LeftPane, &str, &str, &str); 3] = [
    (LeftPane::Contents, "Layers", "contents", "ui.pane.contents"),
    (LeftPane::Catalog, "Browser", "catalog", "ui.pane.catalog"),
    (LeftPane::Sources, "Data Sources", "web_map", "ui.pane.sources"),
];

/// The left pane's header: a tab per left pane (so each is a click away, not only in
/// View › Windows) and the ✕. A tab opens its pane through the same `ui.pane.*` command as the
/// ribbon; the open pane's tab does nothing, so a tab never closes the side. Too narrow for the
/// names (a small pane, a long translation), the tabs show their icons, named on hover.
fn left_header(app: &mut RustGisApp, ui: &mut Ui, left: LeftPane, t: &Tokens) {
    let r = header_strip(ui, t);
    let pad = 9.0;
    let names: Vec<&str> = LEFT_PANES.iter().map(|p| tl!(p.1)).collect();
    let widths: Vec<f32> = names.iter().map(|n| crate::widgets::text_w(ui, n, semibold(12.5)) + 2.0 * pad).collect();
    // Room left of the ✕, with the gaps between tabs.
    let room = r.width() - 6.0 - 36.0 - 2.0 * (LEFT_PANES.len() as f32 - 1.0);
    let icons_only = widths.iter().sum::<f32>() > room;
    let mut x = r.min.x + 6.0;
    for (i, (((pane, _, icon, open), name), width)) in LEFT_PANES.iter().zip(names).zip(widths).enumerate() {
        let w = if icons_only { 32.0 } else { width };
        let tr = Rect::from_min_size(pos2(x, r.min.y), vec2(w, r.height()));
        let on = *pane == left;
        let mut resp = ui.interact(tr, egui::Id::new(("left_pane_tab", i)), Sense::click());
        // Named and marked open for screen readers (and scripted clicks), icons or not.
        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, name));
        let p = ui.painter();
        if resp.hovered() && !on {
            p.rect_filled(tr.shrink2(vec2(1.0, 5.0)), CornerRadius::same(4), t.hover);
        }
        let ink = if on { t.text } else { t.text_dim };
        if icons_only {
            icons::paint(p, Rect::from_center_size(tr.center(), vec2(18.0, 18.0)), icon, ink, t.accent);
            resp = resp.on_hover_text(name);
        } else {
            p.text(tr.center(), Align2::CENTER_CENTER, name, if on { semibold(12.5) } else { regular(12.5) }, ink);
        }
        if on {
            p.line_segment([pos2(tr.min.x + 4.0, r.max.y - 1.5), pos2(tr.max.x - 4.0, r.max.y - 1.5)], Stroke::new(2.0, t.accent));
        }
        if resp.clicked() && !on {
            let _ = app.run(open, Value::Null);
        }
        x += w + 2.0;
    }
    let close = LEFT_PANES.iter().find(|p| p.0 == left).map_or("ui.pane.contents", |p| p.3);
    close_button(app, ui, r, close, "left", t);
}

// ---------------------------------------------------------------------------------------------
// Contents

/// Paints a symbol patch for a layer of geometry type `g`.
pub fn patch(p: &egui::Painter, r: Rect, s: &Symbol, g: GeometryType) {
    match g {
        GeometryType::Point => {
            let rad = (s.size.clamp(4.0, 14.0)) / 2.0;
            let c = r.center();
            match s.marker {
                Marker::Square => {
                    p.rect_filled(Rect::from_center_size(c, vec2(rad * 1.8, rad * 1.8)), 0.0, c32(s.fill));
                    p.rect_stroke(
                        Rect::from_center_size(c, vec2(rad * 1.8, rad * 1.8)),
                        0.0,
                        Stroke::new(s.stroke_width.min(2.0), c32(s.stroke)),
                        egui::StrokeKind::Middle,
                    );
                }
                Marker::Triangle => {
                    p.add(egui::Shape::convex_polygon(
                        vec![c + vec2(0.0, -rad), c + vec2(rad, rad * 0.8), c + vec2(-rad, rad * 0.8)],
                        c32(s.fill),
                        Stroke::new(s.stroke_width.min(2.0), c32(s.stroke)),
                    ));
                }
                Marker::Diamond => {
                    p.add(egui::Shape::convex_polygon(
                        vec![c + vec2(0.0, -rad), c + vec2(rad, 0.0), c + vec2(0.0, rad), c + vec2(-rad, 0.0)],
                        c32(s.fill),
                        Stroke::new(s.stroke_width.min(2.0), c32(s.stroke)),
                    ));
                }
                _ => {
                    p.circle_filled(c, rad, c32(s.fill));
                    p.circle_stroke(c, rad, Stroke::new(s.stroke_width.min(2.0), c32(s.stroke)));
                }
            }
        }
        GeometryType::Polyline => {
            let a = pos2(r.min.x + 2.0, r.center().y + 3.0);
            let b = pos2(r.center().x, r.center().y - 3.0);
            let c = pos2(r.max.x - 2.0, r.center().y + 1.0);
            p.add(egui::Shape::line(vec![a, b, c], Stroke::new(s.stroke_width.clamp(1.0, 4.0), c32(s.stroke))));
        }
        _ => {
            let rr = r.shrink(2.0);
            p.rect_filled(rr, CornerRadius::same(2), c32(s.fill));
            p.rect_stroke(rr, CornerRadius::same(2), Stroke::new(s.stroke_width.clamp(0.5, 2.0), c32(s.stroke)), egui::StrokeKind::Inside);
        }
    }
}

/// Keeps the session in step with the Layers pane's filter (#300): clears it when another map
/// comes into view, and keeps the active layer one the filter shows. Run every frame, whichever
/// pane (if any) is open, so a map visited or a layer renamed while Layers was hidden counts.
pub fn follow_layer_filter(app: &mut RustGisApp) {
    let map = &app.session.map().name;
    if app.ui.layer_filter_map != *map {
        app.ui.layer_filter.clear();
        app.ui.layer_filter_map.clone_from(map);
        app.ui.layer_filter_hid = None;
    }
    // The active layer follows the filter: one it hides stops being active (the Style and Data
    // tabs never act on a layer out of sight), and is selected again once the filter shows it,
    // unless another layer was picked meanwhile.
    let q = filter_query(&app.ui.layer_filter);
    // Whether the filter shows layer `id` (None: the map has no such layer).
    let shown = |s: &Session, id: u32| s.map().layer(id).map(|l| filter_shows(&q, &l.name));
    // Layers picked together are the ones in sight too: a removal must not take one the filter hides.
    let picked = app.session.selected_layers();
    if picked.len() > 1 {
        let keep: Vec<u32> = picked.iter().copied().filter(|id| shown(&app.session, *id) == Some(true)).collect();
        if keep.len() < picked.len() && !keep.is_empty() {
            let _ = app.run("layer.select", json!({"layer": keep}));
        }
    }
    match (app.session.active_layer, app.ui.layer_filter_hid) {
        (Some(id), _) if shown(&app.session, id) == Some(false) => {
            // No command clears the active layer; it is view state, like a click on an empty row.
            app.session.active_layer = None;
            app.ui.layer_filter_hid = Some(id);
        }
        (Some(_), Some(_)) => app.ui.layer_filter_hid = None,
        (None, Some(id)) => match shown(&app.session, id) {
            Some(true) => {
                app.ui.layer_filter_hid = None;
                let _ = app.run("layer.select", json!({"layer": id}));
            }
            Some(false) => {}
            None => app.ui.layer_filter_hid = None,
        },
        _ => {}
    }
}

/// Clears the Layers filter if it hides layer `id`, which would otherwise stop being the active
/// layer on the next frame (Show Me makes a step's layer active to open its symbology).
pub fn reveal_in_filter(app: &mut RustGisApp, id: u32) {
    let q = filter_query(&app.ui.layer_filter);
    if app.session.map().layer(id).is_some_and(|l| !filter_shows(&q, &l.name)) {
        app.ui.layer_filter.clear();
        app.ui.layer_filter_hid = None;
    }
}

/// The filter's text as matched: trimmed and lowercased (empty: no filter).
fn filter_query(text: &str) -> String {
    text.trim().to_lowercase()
}

/// Whether a name passes the filter `q` (from [`filter_query`]): it contains it, ignoring case.
fn filter_shows(q: &str, name: &str) -> bool {
    q.is_empty() || name.to_lowercase().contains(q)
}

fn contents(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    // The name filter (#300), above the list so it stays in view.
    let map_name = app.session.map().name.clone();
    ui.add_space(8.0);
    filter_box(app, ui);
    // (Which layer is active as the filter changes: `follow_layer_filter`, every frame.)
    app.ui.layer_picked = app.session.selected_layers();
    app.ui.layer_rows.clear();
    let q = filter_query(&app.ui.layer_filter);
    let filtering = !q.is_empty();
    let shows = |name: &str| filter_shows(&q, name);
    let view = app.ui.layers_view;
    // Only the drawing order view, unfiltered, reorders: elsewhere rows dropped or moved would
    // land among layers listed apart or hidden.
    let reorder = view == LayersView::DrawingOrder && !filtering;
    // The view's name, and the buttons that pick the view (#301).
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space(12.0);
        let title = match view {
            LayersView::DrawingOrder => "Drawing Order",
            LayersView::DataSource => "Data Source",
            LayersView::Selection => "Selection",
        };
        ui.label(egui::RichText::new(tl!(title)).size(11.5).color(t.text_dim));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(8.0);
            for &(v, icon, hover) in LAYERS_VIEWS.iter().rev() {
                if view_button(ui, icon, tl!(hover), v == view, t).clicked() {
                    app.ui.layers_view = v;
                }
            }
            // The celestial body switcher, lit off the Earth (#223).
            ui.add_space(6.0);
            let body = app.session.map().body();
            let tip = crate::i18n::fmt("Celestial body: {body}", &[("body", tl_raw!(body.name))]);
            let resp = view_button(ui, "planet", &tip, !body.is_earth(), t);
            egui::Popup::menu(&resp).show(|ui| {
                ui.set_min_width(150.0);
                crate::ribbon::body_menu(ui, app);
            });
        });
    });
    if filtering && view == LayersView::DrawingOrder {
        ui.horizontal_wrapped(|ui| {
            ui.add_space(12.0);
            ui.label(egui::RichText::new(tl!("Clear the filter to reorder")).size(11.0).color(t.text_disabled));
        });
    }
    ui.add_space(2.0);
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        let header = ui
            .horizontal(|ui| {
                ui.add_space(8.0);
                let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
                icons::paint(ui.painter(), r, "map", t.icon, t.accent);
                ui.label(egui::RichText::new(&map_name).font(semibold(12.5)));
            })
            .response;
        // The map's row takes copied layers (#491), at the top: there is one even with no layers.
        // Across the pane, like the layer rows, not just its name.
        let row = Rect::from_min_max(header.rect.min, pos2(ui.max_rect().max.x, header.rect.max.y));
        let header = ui.interact(row, egui::Id::new("layers_map_header"), Sense::click());
        header.context_menu(|ui| {
            if paste_layers_button(app, ui) {
                ui.close();
            }
        });
        let layers: Vec<Layer> = app.session.map().layers.clone();
        let groups = app.session.map().service_groups.clone();
        let layer_groups = app.session.map().groups.clone();
        let tiles: Vec<TileLayer> = app.session.map().tiles.clone();
        let scale = app.session.scale();
        // How many layer, service group and tile entries the view lists.
        let listed = match view {
            LayersView::DrawingOrder => {
                let mut shown = std::collections::HashSet::new();
                let mut rows: Vec<DropRow> = Vec::new();
                // A layer group lists at its topmost layer (its layers are kept together); while
                // filtering, when one of its layers or its own name matches.
                for layer in
                    layers.iter().filter(|l| shows(&l.name) || layer_groups.iter().any(|g| g.children.contains(&l.id) && shows(&g.name)))
                {
                    if let Some(group) = groups.iter().find(|g| g.children.iter().any(|c| c.layer == layer.id)) {
                        if shows(&layer.name) && shown.insert(group.id) {
                            service_group_row(app, ui, group, &layers, scale, t);
                        }
                    } else if let Some(group) = layer_groups.iter().find(|g| g.children.contains(&layer.id)) {
                        if shown.insert(group.id) {
                            layer_group_row(app, ui, group, &layers, scale, reorder, &mut rows, t);
                        }
                    } else {
                        let rect = layer_row(app, ui, layer, scale, reorder, t);
                        rows.push(DropRow { id: layer.id, rect, group: None, header: false });
                    }
                }
                for group in &groups {
                    if (!filtering || (group.children.is_empty() && shows(&group.name))) && shown.insert(group.id) {
                        service_group_row(app, ui, group, &layers, scale, t);
                    }
                }
                // Groups with no layers yet, at the bottom.
                for group in &layer_groups {
                    if shows(&group.name) && shown.insert(group.id) {
                        layer_group_row(app, ui, group, &layers, scale, reorder, &mut rows, t);
                    }
                }
                if reorder {
                    layer_drop(app, ui, &rows, t);
                }
                // Web tile layers (services, tile addresses) draw over the basemap and under the layers.
                let mut n = rows.iter().filter(|r| r.group.is_none() && !r.header).count() + shown.len();
                for (i, tile) in tiles.iter().enumerate().filter(|(_, x)| shows(&x.name)) {
                    tile_row(app, ui, i, tiles.len(), reorder, tile, t);
                    n += 1;
                }
                n
            }
            LayersView::DataSource => source_groups(app, ui, &layers, &tiles, scale, &shows, t),
            LayersView::Selection => selection_groups(app, ui, &layers, &shows, t),
        };
        resolve_layer_range(app);
        if app.ui.service_import.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(tl!("Downloading…"));
            });
        }
        let tables: Vec<(u32, String)> =
            app.session.project.tables.iter().filter(|x| shows(&x.name)).map(|x| (x.id, x.name.clone())).collect();
        // A map with no layers says so below instead.
        if filtering && !layers.is_empty() && listed == 0 && tables.is_empty() {
            ui.add_space(12.0);
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new(tl!("No layers match")).color(t.text_dim));
            });
        }
        // The basemap row at the bottom of the drawing order, like a desktop GIS (not a layer, so
        // not while filtering). The built-in basemaps are the Earth's: on another body, its
        // basemap is a tile layer, listed above.
        if view == LayersView::DrawingOrder && !filtering && app.session.map().body().is_earth() {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.add_space(30.0);
                let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
                icons::paint(ui.painter(), r, "basemap", t.icon, t.accent);
                ui.label(
                    egui::RichText::new(format!("{} · {}", tl!("Basemap"), tl!(app.session.map().basemap.label())))
                        .size(12.0)
                        .color(t.text_dim),
                );
            });
        }
        if layers.is_empty() && groups.is_empty() && layer_groups.is_empty() {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new(tl!("This map has no layers yet.")).color(t.text_dim));
                if ui.button(tl!("Add Data…")).clicked() {
                    let _ = app.run("ui.addData", Value::Null);
                }
                if ui.button(tl!("Add sample countries")).clicked() {
                    let _ = app.run("layer.addSample", json!({"name": "countries"}));
                }
                // Hidden, not just off, until something is copied: these are ways to start the
                // map, unlike the menus, where an item that is off says it exists.
                if app.session.layer_clipboard.is_some() {
                    paste_layers_button(app, ui);
                }
            });
        }
        // Standalone tables (those the filter keeps).
        if !tables.is_empty() {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(egui::RichText::new(tl!("Standalone Tables")).size(11.5).color(t.text_dim));
            });
            for (id, name) in tables {
                ui.horizontal(|ui| {
                    ui.add_space(30.0);
                    let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                    icons::paint(ui.painter(), r, "table", t.icon, t.accent);
                    let resp = ui.add(egui::Label::new(egui::RichText::new(&name).size(12.0)).sense(Sense::click()));
                    let mut open = resp.double_clicked() || resp.clicked();
                    resp.context_menu(|ui| {
                        if ui.button(tl!("Attribute Table")).clicked() {
                            open = true;
                            ui.close();
                        }
                        if ui.button(tl!("XY Table To Point…")).clicked() {
                            let _ = app.run("ui.gp.open", json!({"tool": "xyTableToPoint", "table": id}));
                            ui.close();
                        }
                        // Names can repeat: the id says which table. A view open on it closes itself.
                        if ui.button(tl!("Remove")).clicked() {
                            let _ = app.run("table.remove", json!({"table": id}));
                            ui.close();
                        }
                    });
                    if open {
                        app.session.table_standalone = Some(id);
                        app.session.table_layer = None;
                    }
                });
            }
        }
    });
}

/// What pasting the copy adds (#491), translated: the copied group(s), or else the layer(s).
/// Into a group (`into_group`) only layers go, as groups don't nest.
fn paste_label(app: &RustGisApp, into_group: bool) -> &'static str {
    let Some(c) = &app.session.layer_clipboard else { return tl!("Paste Layer") };
    // Layers copied apart from any copied group.
    let loose = c.layers.iter().any(|l| !c.groups.iter().any(|g| g.children.contains(&l.id)));
    match (into_group, c.groups.len(), loose) {
        (false, 1, false) => tl!("Paste Group"),
        (false, n, false) if n > 1 => tl!("Paste Groups"),
        _ if c.layers.len() > 1 => tl!("Paste Layers"),
        _ => tl!("Paste Layer"),
    }
}

/// A button pasting the copied layers (#491) on top of the map in view, off while none are;
/// returns whether it was clicked.
fn paste_layers_button(app: &mut RustGisApp, ui: &mut Ui) -> bool {
    let clicked = ui.add_enabled(app.session.layer_clipboard.is_some(), egui::Button::new(paste_label(app, false))).clicked();
    if clicked {
        let _ = app.run("layer.paste", Value::Null);
    }
    clicked
}

/// The Layers pane's filter box: the shared search field (#337) within the pane's 10 px margins,
/// over a separator.
fn filter_box(app: &mut RustGisApp, ui: &mut Ui) {
    egui::Frame::NONE.inner_margin(egui::Margin::symmetric(10, 0)).show(ui, |ui| {
        crate::widgets::search_field(ui, &mut app.ui.layer_filter, tl!("Filter layers"));
    });
    ui.separator();
}

/// The Layers pane's views (#301): each with its button's icon and hover text.
const LAYERS_VIEWS: [(LayersView, &str, &str); 3] = [
    (LayersView::DrawingOrder, "contents", "List by drawing order"),
    (LayersView::DataSource, "web_map", "List by data source"),
    (LayersView::Selection, "select", "List by selection"),
];

/// A small icon button that picks a Layers view; `on` marks the view shown.
fn view_button(ui: &mut Ui, icon: &str, hover: &str, on: bool, t: &Tokens) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, hover));
    if on {
        ui.painter().rect_filled(r, CornerRadius::same(5), t.selection);
    } else if resp.hovered() {
        ui.painter().rect_filled(r, CornerRadius::same(5), t.hover);
    }
    icons::paint(ui.painter(), r.shrink(4.0), icon, if on { t.text } else { t.text_dim }, t.accent);
    resp.on_hover_text(hover)
}

/// Where layers come from, as the Data Source view groups them: `key` tells sources apart,
/// `label` names the group and `hover` says more.
struct SourceName {
    key: String,
    label: String,
    hover: String,
}

/// Address schemes naming a database server: grouped by server and database, not server alone.
const DATABASE_SCHEMES: [&str; 6] = ["postgres", "postgresql", "postgis", "mysql", "mariadb", "mssql"];

/// A source path, web address or database name as a group: a database address by its server and
/// database, any other address by its host (never its credentials, path or query), and a path
/// (`file://` addresses too) by its last part, with the whole path as hover text.
fn source_name(src: &str) -> SourceName {
    if let Some((scheme, rest)) = src.split_once("://") {
        let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
        let host = host.rsplit('@').next().unwrap_or(host);
        let scheme = scheme.to_ascii_lowercase();
        // `file:///data/x.gpkg` (`localhost` is the same as no host), or an address with no host:
        // a path, its %-escapes decoded.
        if scheme == "file" || host.is_empty() {
            let path = if host.eq_ignore_ascii_case("localhost") { rest.find('/').and_then(|i| rest.get(i..)).unwrap_or("") } else { rest };
            return path_name(&percent_decoded(path));
        }
        // Only the host ignores case: a database's name doesn't.
        let db = DATABASE_SCHEMES
            .contains(&scheme.as_str())
            .then(|| rest.split(['?', '#']).next().unwrap_or(rest).split('/').nth(1).filter(|d| !d.is_empty()))
            .flatten();
        let key = format!("web:{}/{}", host.to_lowercase(), db.unwrap_or_default());
        let name = db.map_or_else(|| host.to_string(), |d| format!("{host}/{d}"));
        return SourceName { key, label: name.clone(), hover: name };
    }
    path_name(src)
}

/// `%XX` escapes in a `file://` path decoded (`My%20Data.gpkg` is `My Data.gpkg`); a `%` not
/// followed by two hex digits stays as it is.
fn percent_decoded(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while let Some(&c) = b.get(i) {
        // Two hex digits exactly (`from_str_radix` alone would take a sign).
        let digits = b.get(i + 1..i + 3).filter(|h| h.iter().all(u8::is_ascii_hexdigit));
        let hex = digits.and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
        match (c, hex) {
            (b'%', Some(v)) => {
                out.push(v);
                i += 3;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A path as a source group: its last part, with the whole path as hover text.
fn path_name(path: &str) -> SourceName {
    let name = path.rsplit(['/', '\\']).find(|part| !part.is_empty()).unwrap_or(path);
    SourceName { key: format!("file:{path}"), label: name.to_string(), hover: path.to_string() }
}

/// The source a layer was read from (a raster's file, or the dataset's file, address or
/// database), if any: layers made in the app (samples, drawings, tool outputs) have none.
fn layer_source(l: &Layer) -> Option<SourceName> {
    let raster = l.raster.as_ref().and_then(|r| r.path.as_ref().or(r.file_name.as_ref()));
    raster.or(l.dataset.source.as_ref()).filter(|s| !s.trim().is_empty()).map(|s| source_name(s.as_str()))
}

/// A group's heading in a Layers view, with optional hover text.
fn group_header(ui: &mut Ui, title: &str, hover: Option<&str>, t: &Tokens) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space(12.0);
        let resp = ui.label(egui::RichText::new(title).font(semibold(12.0)).color(t.text_dim));
        if let Some(h) = hover {
            resp.on_hover_text(h);
        }
    });
}

/// The Data Source view: layers and web tile layers under the file, web address or database
/// they come from (by name), then those from none; within a group, in drawing order. Returns
/// how many rows it lists.
fn source_groups(
    app: &mut RustGisApp,
    ui: &mut Ui,
    layers: &[Layer],
    tiles: &[TileLayer],
    scale: f64,
    shows: &dyn Fn(&str) -> bool,
    t: &Tokens,
) -> usize {
    enum Item<'a> {
        Layer(&'a Layer),
        Tile(usize, &'a TileLayer),
    }
    type Groups<'a> = Vec<(Option<SourceName>, Vec<Item<'a>>)>;
    // Into the group with the same source, or a new one.
    fn add<'a>(groups: &mut Groups<'a>, src: Option<SourceName>, item: Item<'a>) {
        let key = src.as_ref().map(|s| s.key.clone());
        match groups.iter_mut().find(|g| g.0.as_ref().map(|s| &s.key) == key.as_ref()) {
            Some(g) => g.1.push(item),
            None => groups.push((src, vec![item])),
        }
    }
    let mut groups: Groups<'_> = Vec::new();
    for l in layers.iter().filter(|l| shows(&l.name)) {
        add(&mut groups, layer_source(l), Item::Layer(l));
    }
    for (i, tile) in tiles.iter().enumerate().filter(|(_, x)| shows(&x.name)) {
        add(&mut groups, Some(source_name(&tile.url)), Item::Tile(i, tile));
    }
    groups.sort_by(|a, b| match (&a.0, &b.0) {
        (Some(x), Some(y)) => x.label.to_lowercase().cmp(&y.label.to_lowercase()),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    let mut n = 0;
    for (src, items) in groups {
        match &src {
            Some(s) => group_header(ui, &s.label, Some(&s.hover), t),
            None => group_header(ui, tl!("In memory"), Some(tl!("Made in RustGIS: samples, drawings and tool outputs")), t),
        }
        for item in items {
            match item {
                Item::Layer(l) => {
                    layer_row(app, ui, l, scale, false, t);
                }
                Item::Tile(i, tile) => tile_row(app, ui, i, tiles.len(), false, tile, t),
            }
            n += 1;
        }
    }
    n
}

/// The Selection view: the feature layers that can be selected, then those that can't, each
/// with its selected feature count (rasters and web tile layers are never selected). Returns how
/// many rows it lists.
fn selection_groups(app: &mut RustGisApp, ui: &mut Ui, layers: &[Layer], shows: &dyn Fn(&str) -> bool, t: &Tokens) -> usize {
    let mut n = 0;
    for (selectable, title) in [(true, "Selectable"), (false, "Not Selectable")] {
        let rows: Vec<&Layer> = layers.iter().filter(|l| !l.is_raster() && l.selectable == selectable && shows(&l.name)).collect();
        if rows.is_empty() {
            continue;
        }
        group_header(ui, tl!(title), None, t);
        for l in rows {
            selection_row(app, ui, l, t);
            n += 1;
        }
    }
    n
}

/// The active layer's row (the one the Style and Data tabs act on) among those picked: a thin
/// outline, as a file browser marks the item with focus.
fn active_outline(ui: &Ui, r: Rect, active: bool, t: &Tokens) {
    if active {
        let stroke = Stroke::new(1.0, t.accent);
        ui.painter().rect_stroke(r.shrink2(vec2(4.0, 1.0)), CornerRadius::same(5), stroke, egui::StrokeKind::Inside);
    }
}

/// A click on a layer's row: alone makes it the only layer picked, with Ctrl or Command it joins
/// or leaves the layer selection, with Shift the rows between the last one clicked and this one
/// are picked (once the pane has drawn them: `resolve_layer_range`).
fn click_layer(app: &mut RustGisApp, ui: &Ui, id: u32) {
    let (shift, toggle) = ui.input(|i| (i.modifiers.shift, i.modifiers.command));
    if shift {
        app.ui.layer_range = Some(id);
        return;
    }
    let mode = if toggle { "toggle" } else { "replace" };
    let _ = app.run("layer.select", json!({"layer": id, "mode": mode}));
    app.ui.layer_anchor = Some(id);
}

/// Picks the rows from the anchor to the layer a Shift+click was made on, in the order the pane
/// lists them (collapsed groups' layers and filtered ones are not listed, so not picked); the
/// layer clicked ends up active. Without a listed anchor, only that layer.
fn resolve_layer_range(app: &mut RustGisApp) {
    let Some(to) = app.ui.layer_range.take() else { return };
    let rows = &app.ui.layer_rows;
    let from = app.ui.layer_anchor.or(app.session.active_layer);
    let span = match (from.and_then(|f| rows.iter().position(|r| *r == f)), rows.iter().position(|r| *r == to)) {
        (Some(a), Some(b)) if a <= b => rows[a..=b].to_vec(),
        (Some(a), Some(b)) => rows[b..=a].iter().rev().copied().collect(),
        _ => vec![to],
    };
    let _ = app.run("layer.select", json!({"layer": span}));
    if app.ui.layer_anchor.is_none() {
        app.ui.layer_anchor = from;
    }
}

/// A layer's row in the Selection view: a box for whether it can be selected
/// (`layer.selectable`), its name, and how many of its features are selected with a ✕ that clears
/// them (`select.rows`). A click makes it the active layer; right-click opens its menu.
fn selection_row(app: &mut RustGisApp, ui: &mut Ui, l: &Layer, t: &Tokens) {
    app.ui.layer_rows.push(l.id);
    let count = l.selection.len();
    // The name starts at 48 and stops short of the count and its ✕.
    let counted = if count > 0 { 22.0 + 4.0 + crate::widgets::text_w(ui, &count.to_string(), regular(12.0)) + 8.0 } else { 0.0 };
    let color = if l.selectable { t.text } else { t.text_disabled };
    let name = row_name(ui, l.name.clone(), regular(12.5), ui.available_width() - 48.0 - 8.0 - counted);
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), name.row_h), Sense::click());
    let cy = r.min.y + 13.0;
    if app.ui.layer_picked.contains(&l.id) {
        ui.painter().rect_filled(r.shrink2(vec2(4.0, 1.0)), CornerRadius::same(5), t.selection);
    } else if resp.hovered() {
        ui.painter().rect_filled(r.shrink2(vec2(4.0, 1.0)), CornerRadius::same(5), t.hover);
    }
    active_outline(ui, r, app.session.active_layer == Some(l.id), t);
    // Lined up with the other views' checkboxes and names.
    let cb = Rect::from_center_size(pos2(r.min.x + 33.0, cy), vec2(14.0, 14.0));
    let cb_resp = ui.interact(hit(cb), egui::Id::new(("layer_selectable", l.id)), Sense::click());
    cb_resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, l.selectable, tl!("Selectable")));
    visibility_box(ui, cb, l.selectable, t);
    // Only the map's interactive selection reads it: the tools that name a layer (Select By
    // Attributes, Select Layer By Location, the attribute table) select in any layer.
    if cb_resp.on_hover_text(tl!("Selectable on the map: the Select tool picks its features")).clicked() {
        let _ = app.run("layer.selectable", json!({"layer": l.id}));
    }
    if count > 0 {
        let cr = Rect::from_center_size(pos2(r.max.x - 8.0 - 11.0, cy), vec2(22.0, 22.0));
        let clear = ui.interact(cr, egui::Id::new(("layer_selection_clear", l.id)), Sense::click());
        clear.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tl!("Clear selection")));
        if clear.hovered() {
            ui.painter().rect_filled(cr, CornerRadius::same(5), t.hover);
        }
        icons::paint(ui.painter(), cr.shrink(5.0), "close", t.text_dim, t.accent);
        if clear.on_hover_text(tl!("Clear selection")).clicked() {
            let _ = app.run("select.rows", json!({"layer": l.id, "rows": []}));
        }
        let g = ui.painter().layout_no_wrap(count.to_string(), regular(12.0), t.text_dim);
        ui.painter().galley(pos2(cr.min.x - 4.0 - g.size().x, cy - g.size().y / 2.0), g, t.text_dim);
    }
    let elided = name.elided;
    name.paint(ui, r, r.min.x + 48.0, color);
    let tip = match (elided, count > 0) {
        (true, true) => Some(format!("{}\n{}", l.name, selected_text(count))),
        (true, false) => Some(l.name.clone()),
        (false, true) => Some(selected_text(count)),
        (false, false) => None,
    };
    let resp = match tip {
        Some(tip) => resp.on_hover_text(tip),
        None => resp,
    };
    if resp.clicked() {
        click_layer(app, ui, l.id);
    }
    resp.context_menu(|ui| layer_menu(app, ui, l, false));
}

/// "{n} selected", a Selection view row's hover text.
fn selected_text(n: usize) -> String {
    crate::i18n::fmt("{n} selected", &[("n", &n.to_string())])
}

fn service_group_row(
    app: &mut RustGisApp,
    ui: &mut Ui,
    group: &rustgis_engine::map::ServiceGroup,
    layers: &[Layer],
    scale: f64,
    t: &Tokens,
) {
    let q = filter_query(&app.ui.layer_filter);
    let filtering = !q.is_empty();
    let mut children: Vec<&Layer> = layers.iter().filter(|l| group.children.iter().any(|c| c.layer == l.id)).collect();
    children.sort_by_cached_key(|l| (l.name.to_lowercase(), l.id));
    ui.push_id(("service_group", group.id), |ui| {
        // Laid out as a layer row: the arrow, the box, the icon and the name (two lines at most)
        // in the same places.
        let name = row_name(ui, group.name.clone(), semibold(12.5), ui.available_width() - NAME_X - 8.0);
        let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), name.row_h), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &group.name));
        if resp.hovered() {
            ui.painter().rect_filled(r.shrink2(vec2(4.0, 1.0)), CornerRadius::same(5), t.hover);
        }
        let cy = r.min.y + 13.0;
        let ar = Rect::from_center_size(pos2(r.min.x + 16.0, cy), vec2(14.0, 14.0));
        icons::paint(ui.painter(), ar, if group.expanded { "chevron_down" } else { "chevron_right" }, t.text_dim, t.accent);
        // `interact` takes the id as it is (not under the `push_id`): each group's own.
        let arrow = ui.interact(hit(ar), egui::Id::new(("group_expand", group.id)), Sense::click());
        let visible = !children.is_empty() && children.iter().all(|l| l.visible);
        let cb = Rect::from_center_size(pos2(r.min.x + 33.0, cy), vec2(14.0, 14.0));
        let cb_resp = ui.interact(hit(cb), egui::Id::new(("group_visible", group.id)), Sense::click());
        cb_resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, visible, tl!("Show or hide")));
        visibility_box(ui, cb, visible, t);
        icons::paint(ui.painter(), Rect::from_center_size(pos2(r.min.x + 57.0, cy), vec2(18.0, 16.0)), "map", t.icon, t.accent);
        let elided = name.elided;
        name.paint(ui, r, r.min.x + NAME_X, t.text);
        if cb_resp.on_hover_text(tl!("Show or hide")).clicked() {
            let _ = app.run("service.group", json!({"group":group.id,"visible":!visible}));
        } else if arrow.clicked() || resp.double_clicked() {
            let _ = app.run("service.group", json!({"group":group.id,"expanded":!group.expanded}));
        }
        let resp = if elided { resp.on_hover_text(&group.name) } else { resp };
        resp.context_menu(|ui| {
            if ui.add_enabled(app.ui.service_import.is_none(), egui::Button::new(tl!("Refresh"))).clicked() {
                let _ = app.run("ui.refreshService", json!({"group":group.id}));
                ui.close();
            }
            if ui.button(tl!("Rename…")).clicked() {
                let _ = app.run("ui.dialog", json!({"name":"renameServiceGroup"}));
                app.dialogs.layer = group.id.to_string();
                app.dialogs.text = group.name.clone();
                ui.close();
            }
            if ui.button(tl!("Remove")).clicked() {
                let _ = app.run("service.group", json!({"group":group.id,"remove":true}));
                ui.close();
            }
        });
        if group.expanded || filtering {
            ui.indent("children", |ui| {
                for layer in children.iter().filter(|l| filter_shows(&q, &l.name)) {
                    layer_row(app, ui, layer, scale, false, t);
                }
            });
        }
    });
}

/// A layer group's row (#413): its arrow folds it, its box shows or hides its layers together,
/// and right-click renames it, sets its transparency, ungroups or removes it. With `reorder` the
/// row drags the whole group, and it and its layers' rows join `rows`, where layers drop (see
/// [`layer_drop`]).
fn layer_group_row(
    app: &mut RustGisApp,
    ui: &mut Ui,
    group: &rustgis_engine::map::LayerGroup,
    layers: &[Layer],
    scale: f64,
    reorder: bool,
    rows: &mut Vec<DropRow>,
    t: &Tokens,
) {
    let q = filter_query(&app.ui.layer_filter);
    let filtering = !q.is_empty();
    // In drawing order, as they draw.
    let children: Vec<&Layer> = layers.iter().filter(|l| group.children.contains(&l.id)).collect();
    ui.push_id(("layer_group", group.id), |ui| {
        let name = row_name(ui, group.name.clone(), semibold(12.5), ui.available_width() - NAME_X - 8.0);
        let sense = if reorder { Sense::click_and_drag() } else { Sense::click() };
        let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), name.row_h), sense);
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &group.name));
        if reorder && resp.drag_started_by(egui::PointerButton::Primary) {
            egui::DragAndDrop::set_payload(ui.ctx(), DraggedLayer(group.id));
        }
        let dragging = egui::DragAndDrop::payload::<DraggedLayer>(ui.ctx()).is_some_and(|d| d.0 == group.id);
        rows.push(DropRow { id: group.id, rect: r, group: None, header: true });
        if resp.hovered() || dragging {
            ui.painter().rect_filled(r.shrink2(vec2(4.0, 1.0)), CornerRadius::same(5), t.hover);
        }
        let cy = r.min.y + 13.0;
        let ar = Rect::from_center_size(pos2(r.min.x + 16.0, cy), vec2(14.0, 14.0));
        icons::paint(ui.painter(), ar, if group.expanded { "chevron_down" } else { "chevron_right" }, t.text_dim, t.accent);
        let arrow = ui.interact(hit(ar), egui::Id::new(("layer_group_expand", group.id)), Sense::click());
        let cb = Rect::from_center_size(pos2(r.min.x + 33.0, cy), vec2(14.0, 14.0));
        let cb_resp = ui.interact(hit(cb), egui::Id::new(("layer_group_visible", group.id)), Sense::click());
        cb_resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, group.visible, tl!("Show or hide")));
        visibility_box(ui, cb, group.visible, t);
        icons::paint(ui.painter(), Rect::from_center_size(pos2(r.min.x + 57.0, cy), vec2(18.0, 16.0)), "contents", t.icon, t.accent);
        let elided = name.elided;
        let out_of_range = !group.in_scale_range(scale);
        name.paint(ui, r, r.min.x + NAME_X, if group.visible && !out_of_range { t.text } else { t.text_disabled });
        if cb_resp.on_hover_text(tl!("Show or hide the group's layers")).clicked() {
            let _ = app.run("group.set", json!({"group": group.id, "visible": !group.visible}));
        } else if arrow.clicked() || resp.double_clicked() {
            let _ = app.run("group.expand", json!({"group": group.id}));
        }
        let resp = if elided { resp.on_hover_text(&group.name) } else { resp };
        resp.context_menu(|ui| {
            if ui.button(if group.visible { tl!("Hide Group") } else { tl!("Show Group") }).clicked() {
                let _ = app.run("group.set", json!({"group": group.id, "visible": !group.visible}));
                ui.close();
            }
            if ui.button(tl!("Rename…")).clicked() {
                let _ = app.run("ui.dialog", json!({"name": "renameGroup"}));
                app.dialogs.layer = group.id.to_string();
                app.dialogs.text = group.name.clone();
                ui.close();
            }
            let mut tr = group.transparency;
            ui.horizontal(|ui| {
                ui.label(tl!("Transparency"));
                let r = ui.add(egui::Slider::new(&mut tr, 0.0..=100.0).suffix(" %").integer());
                if r.drag_stopped() || (r.changed() && !r.dragged()) {
                    let _ = app.run("group.set", json!({"group": group.id, "transparency": tr}));
                }
            });
            if (group.min_scale.is_some() || group.max_scale.is_some()) && ui.button(tl!("Clear Visibility Range")).clicked() {
                let _ = app.run("group.scaleRange", json!({"group": group.id}));
                ui.close();
            }
            ui.separator();
            if ui.button(tl!("Copy Group")).clicked() {
                let _ = app.run("layer.copy", json!({"group": group.id}));
                ui.close();
            }
            if ui.add_enabled(app.session.layer_clipboard.is_some(), egui::Button::new(paste_label(app, true))).clicked() {
                let _ = app.run("layer.paste", json!({"group": group.id}));
                ui.close();
            }
            ui.separator();
            if ui.button(tl!("Ungroup")).clicked() {
                let _ = app.run("group.ungroup", json!({"group": group.id}));
                ui.close();
            }
            if ui.button(tl!("Remove")).clicked() {
                let _ = app.run("group.remove", json!({"group": group.id}));
                ui.close();
            }
        });
        if group.expanded || filtering {
            ui.indent("children", |ui| {
                for layer in children.iter().filter(|l| filter_shows(&q, &l.name) || filter_shows(&q, &group.name)) {
                    let rect = layer_row(app, ui, layer, scale, reorder, t);
                    rows.push(DropRow { id: layer.id, rect, group: Some(group.id), header: false });
                }
            });
        }
    });
}

/// The layer a row drag carries (egui's drag-and-drop payload).
#[derive(Clone, Copy)]
struct DraggedLayer(u32);

/// A row of the drawing order view a dragged layer can drop on: a layer's (in a layer `group` or
/// not) or a layer group's `header`.
#[derive(Clone, Copy)]
struct DropRow {
    id: u32,
    rect: Rect,
    group: Option<u32>,
    header: bool,
}

/// Dragging a layer or layer group row over the list: marks where it would drop and, on release,
/// puts it there in one undo step. A layer dropped above one of a group's layers joins the group
/// there, dropped elsewhere leaves its group, and dropped onto a group's row joins it at its end
/// nearest the layer (`layer.group`); a group moves whole and only between groups (`group.move`). Released
/// anywhere but over the rows, the drag does nothing.
fn layer_drop(app: &mut RustGisApp, ui: &Ui, rows: &[DropRow], t: &Tokens) {
    let Some(dragged) = egui::DragAndDrop::payload::<DraggedLayer>(ui.ctx()) else { return };
    let Some(from) = rows.iter().position(|r| r.id == dragged.0) else { return };
    let view = ui.clip_rect();
    let Some(pointer) = ui.ctx().pointer_interact_pos().filter(|p| view.contains(*p)) else { return };
    // Near the top or bottom edge the list scrolls, faster the closer the pointer is.
    let edge = 28.0;
    let push = ((view.min.y + edge - pointer.y).max(0.0) - (pointer.y - (view.max.y - edge)).max(0.0)) / edge;
    if push != 0.0 {
        let dt = ui.input(|i| i.stable_dt).min(0.1);
        ui.scroll_with_delta_animation(vec2(0.0, push * 600.0 * dt), egui::style::ScrollAnimation::none());
        ui.ctx().request_repaint();
    }
    // Only the layer rows take a drop: not the map's name above them, the basemap or a table below.
    let (Some(first), Some(last)) = (rows.first(), rows.last()) else { return };
    if pointer.y < first.rect.min.y || pointer.y > last.rect.max.y {
        return;
    }
    let released = ui.input(|i| i.pointer.any_released());
    let m = app.session.map();
    let moving_group = m.group(dragged.0).is_some();
    let current = m.group_of(dragged.0).map(|g| g.id);
    // The topmost layer of a group, in drawing order.
    let top_of = |g: u32| m.group(g).and_then(|g| m.layers.iter().find(|l| g.children.contains(&l.id)).map(|l| l.id));
    // Onto the middle of another group's row: into that group, at its end nearest the layer; onto
    // its own group's row, nothing.
    if !moving_group
        && let Some(h) = rows
            .iter()
            .find(|r| r.header && pointer.y > r.rect.min.y + r.rect.height() / 4.0 && pointer.y < r.rect.max.y - r.rect.height() / 4.0)
    {
        if Some(h.id) == current {
            return;
        }
        ui.painter().rect_stroke(
            h.rect.shrink2(vec2(4.0, 1.0)),
            CornerRadius::same(5),
            Stroke::new(2.0, t.accent),
            egui::StrokeKind::Inside,
        );
        if released {
            let _ = app.run("layer.group", json!({"layer": dragged.0, "group": h.id}));
        }
        return;
    }
    // The gap above the first row whose middle is below the pointer, or the one after the last.
    let gap = rows.iter().position(|r| pointer.y < r.rect.center().y).unwrap_or(rows.len());
    let below = rows.get(gap);
    // Above one of a group's layers is in that group; anywhere else, in none.
    let into = below.filter(|r| !r.header).and_then(|r| r.group);
    // The layer the drop goes above (a group's row stands for its topmost layer); none at the end.
    let before = below.and_then(|r| if r.header { top_of(r.id) } else { Some(r.id) });
    if moving_group {
        // A group's own rows run from its header to its last layer; next to them it stays put,
        // and it never goes into another group.
        let end = rows.iter().rposition(|r| r.group == Some(dragged.0)).unwrap_or(from);
        if (gap >= from && gap <= end + 1) || into.is_some() {
            return;
        }
    } else if into == current && (gap == from || gap == from + 1) {
        return;
    }
    let line = match below {
        Some(r) => Some((r.rect.min.y, r.rect)),
        None => rows.last().map(|r| (r.rect.max.y, r.rect)),
    };
    if let Some((y, r)) = line {
        // Indented when the drop goes into a group.
        let x = match (into, below) {
            (Some(_), Some(b)) => b.rect.min.x,
            _ => first.rect.min.x,
        };
        ui.painter().line_segment([pos2(x + 8.0, y), pos2(r.max.x - 8.0, y)], Stroke::new(2.0, t.accent));
    }
    if !released {
        return;
    }
    if let Some(g) = m.group(dragged.0) {
        // Among the other layers, the place of the one it goes above.
        let others: Vec<u32> = m.layers.iter().map(|l| l.id).filter(|id| !g.children.contains(id)).collect();
        let pos = match before {
            Some(id) => others.iter().position(|l| *l == id),
            None => Some(others.len()),
        };
        if let Some(pos) = pos {
            let _ = app.run("group.move", json!({"group": dragged.0, "to": pos}));
        }
        return;
    }
    let _ = app.run("layer.group", json!({"layer": dragged.0, "group": into, "before": before}));
}

/// A layer row's visibility checkbox, ticked when the layer is shown.
fn visibility_box(ui: &Ui, cb: Rect, visible: bool, t: &Tokens) {
    ui.painter().rect_filled(cb, CornerRadius::same(3), if visible { t.accent } else { t.input });
    ui.painter().rect_stroke(
        cb,
        CornerRadius::same(3),
        Stroke::new(1.0, if visible { t.accent } else { t.input_border }),
        egui::StrokeKind::Inside,
    );
    if visible {
        ui.painter().add(egui::Shape::line(
            vec![cb.min + vec2(3.0, 7.0), cb.min + vec2(6.0, 10.0), cb.min + vec2(11.0, 4.0)],
            Stroke::new(1.8, egui::Color32::WHITE),
        ));
    }
}

/// A web tile layer's row (index `i` of `count`, top first as the map lists them): its checkbox
/// shows or hides it; right-click zooms to it (known bounds, an archive or GeoTIFF), sets its
/// opacity, moves it up or down (not with `reorder` off: the list is filtered), or removes it.
fn tile_row(app: &mut RustGisApp, ui: &mut Ui, i: usize, count: usize, reorder: bool, tile: &TileLayer, t: &Tokens) {
    let shown = if tile.overlay { format!("{} · {}", tile.name, tl!("overlay")) } else { tile.name.clone() };
    let name = row_name(ui, shown.clone(), regular(12.5), ui.available_width() - NAME_X - 8.0);
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), name.row_h), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(r.shrink2(vec2(4.0, 1.0)), CornerRadius::same(5), t.hover);
    }
    // Lined up with the layer rows' checkboxes, icons and names.
    let cy = r.min.y + 13.0;
    let mut x = r.min.x + 26.0;
    let cb = Rect::from_center_size(pos2(x + 7.0, cy), vec2(14.0, 14.0));
    let cb_resp = ui.interact(hit(cb), egui::Id::new(("tile_vis", i)), Sense::click()).on_hover_text(tl!("Show or hide"));
    visibility_box(ui, cb, tile.visible, t);
    x += 22.0;
    icons::paint(ui.painter(), Rect::from_center_size(pos2(x + 9.0, cy), vec2(18.0, 16.0)), "basemap", t.icon, t.accent);
    x += 24.0;
    // Greyed when it draws nothing: hidden, transparent, or a data cube tile with no file at the
    // slider's step (the box still shows the choice to show it).
    let color = if tile.draws() && tile.opacity > 0.0 { t.text } else { t.text_disabled };
    let elided = name.elided;
    name.paint(ui, r, x, color);
    let mut action: Option<(&str, Value)> = None;
    if cb_resp.clicked() {
        action = Some(("map.setTiles", json!({"index": i, "visible": !tile.visible})));
    }
    let resp = resp.on_hover_text(if elided { format!("{shown}\n{}", tile.url) } else { tile.url.clone() });
    resp.context_menu(|ui| {
        // Stored bounds, an archive or GeoTIFF say what the layer covers.
        if tile.bounds.is_some() || matches!(tile.kind, TileKind::PmTiles | TileKind::Cog) {
            if ui.button(tl!("Zoom To Layer")).clicked() {
                action = Some(("map.zoomToTiles", json!({"index": i})));
                ui.close();
            }
            ui.separator();
        }
        ui.horizontal(|ui| {
            ui.label(tl!("Opacity"));
            let mut o = tile.opacity * 100.0;
            let slider = ui.add(egui::Slider::new(&mut o, 0.0..=100.0).suffix("%"));
            if slider.drag_stopped() || (slider.changed() && !slider.dragged()) {
                action = Some(("map.setTiles", json!({"index": i, "opacity": o / 100.0})));
            }
        });
        ui.menu_button(crate::i18n::fmt("Blend: {mode}", &[("mode", tl_raw!(tile.blend.label()))]), |ui| {
            for b in rustgis_engine::map::effects::Blend::ALL {
                if ui.radio(tile.blend == b, tl!(b.label())).clicked() {
                    action = Some(("map.setTiles", json!({"index": i, "blend": b.code()})));
                    ui.close();
                }
            }
        })
        .response
        .on_hover_text(tl!("How the layer's colours combine with the tile layers and basemap beneath."));
        ui.separator();
        if ui.add_enabled(reorder && i > 0, egui::Button::new(tl!("Move up"))).clicked() {
            action = Some(("map.setTiles", json!({"index": i, "move": "up"})));
            ui.close();
        }
        if ui.add_enabled(reorder && i + 1 < count, egui::Button::new(tl!("Move down"))).clicked() {
            action = Some(("map.setTiles", json!({"index": i, "move": "down"})));
            ui.close();
        }
        if ui.button(tl!("Remove")).clicked() {
            action = Some(("map.removeTiles", json!({"index": i})));
            ui.close();
        }
    });
    if let Some((cmd, p)) = action {
        let _ = app.run(cmd, p);
    }
}

/// The hit target of a row's 14-point arrow or box at `at`: wider, and the first line's full
/// height, so a click near it lands.
fn hit(at: Rect) -> Rect {
    Rect::from_center_size(at.center(), vec2(at.width() + 4.0, 26.0))
}

/// Where a layer row's name starts: after the expand arrow, the visibility box and the symbol.
const NAME_X: f32 = 10.0 + 16.0 + 22.0 + 24.0;

/// A Layers pane row's name, laid out by [`row_name`].
struct RowName {
    galley: std::sync::Arc<egui::Galley>,
    /// The height of one of its lines.
    line_h: f32,
    /// The row's height: 26 points for one line, a line more for two.
    row_h: f32,
    /// The width it was given.
    room: f32,
    /// Whether some of it doesn't show (the tooltip then shows it whole).
    elided: bool,
}

/// A Layers pane row's name laid out in `room` points: on a second line when it doesn't fit (a
/// word too long for a line is broken anywhere), then cut with "…".
fn row_name(ui: &Ui, name: String, font: egui::FontId, room: f32) -> RowName {
    // Laid out in the room it is clipped to, so the two agree; a point at least.
    let room = room.max(1.0);
    let mut job = egui::text::LayoutJob::simple(name, font, egui::Color32::PLACEHOLDER, room);
    job.wrap.max_rows = 2;
    let galley = ui.painter().layout_job(job);
    let line_h = galley.size().y / galley.rows.len().max(1) as f32;
    let row_h = (galley.size().y + 26.0 - line_h).max(26.0);
    // Cut with "…", or (a glyph wider than the room) by the clip: either way, not all shows.
    let elided = galley.elided || galley.size().x > room + 0.5;
    RowName { galley, line_h, row_h, room, elided }
}

impl RowName {
    /// Paints the name from `x` in row `r`, its first line on the icons' line (13 points down),
    /// clipped to its room: never under the row's badge, count or the pane's edge.
    fn paint(self, ui: &Ui, r: Rect, x: f32, color: egui::Color32) {
        let clip = Rect::from_min_max(pos2(x, r.min.y), pos2(x + self.room, r.max.y));
        ui.painter().with_clip_rect(clip).galley(pos2(x, r.min.y + 13.0 - self.line_h / 2.0), self.galley, color);
    }
}

/// A layer's row and, expanded, its legend; returns the rectangle of both. With `reorder` off
/// (the list is filtered or children are sorted by name) the row can't be dragged.
fn layer_row(app: &mut RustGisApp, ui: &mut Ui, l: &Layer, scale: f64, reorder: bool, t: &Tokens) -> Rect {
    app.ui.layer_rows.push(l.id);
    let active = app.session.active_layer == Some(l.id);
    let picked = app.ui.layer_picked.contains(&l.id);
    // The name takes two lines when it must (the tooltip has it whole when cut), leaving room for
    // the selection badge.
    let badge = (!l.selection.is_empty()).then(|| {
        let text = l.selection.len().to_string();
        let w = crate::widgets::text_w(ui, &text, regular(10.5)) + 10.0;
        (text, w)
    });
    let shown = if l.definition_query.is_some() { format!("{}  {}", l.name, tl!("(filtered)")) } else { l.name.clone() };
    // Every picked row looks alike; the active one has an outline (`active_outline`), not a
    // bolder name, which would read as the only one picked.
    let font = regular(12.5);
    let room = ui.available_width() - NAME_X - badge.as_ref().map_or(8.0, |(_, w)| w + 18.0);
    let name = row_name(ui, shown.clone(), font, room);
    let sense = if reorder { Sense::click_and_drag() } else { Sense::click() };
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), name.row_h), sense);
    // The icons and the badge line up with the name's first line.
    let cy = r.min.y + 13.0;
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &l.name));
    if reorder && resp.drag_started_by(egui::PointerButton::Primary) {
        egui::DragAndDrop::set_payload(ui.ctx(), DraggedLayer(l.id));
    }
    let dragging = egui::DragAndDrop::payload::<DraggedLayer>(ui.ctx()).is_some_and(|d| d.0 == l.id);
    if picked {
        ui.painter().rect_filled(r.shrink2(vec2(4.0, 1.0)), CornerRadius::same(5), t.selection);
    } else if resp.hovered() || dragging {
        ui.painter().rect_filled(r.shrink2(vec2(4.0, 1.0)), CornerRadius::same(5), t.hover);
    }
    active_outline(ui, r, active, t);
    let mut x = r.min.x + 10.0;
    // Expand arrow.
    let raster_legend = l.raster.as_ref().map(|r| r.legend());
    // What the layer draws at this scale, as the layout legend lists it (#207); out of its scale
    // range, what its definition query keeps, so the pane still shows its symbology.
    let vector_legend = match &raster_legend {
        Some(_) => Vec::new(),
        None => l
            .shown_legend(scale)
            .or_else(|| (!l.in_scale_range(scale)).then(|| l.renderer.legend_for(&l.dataset, &l.visible_rows())))
            .unwrap_or_default(),
    };
    let has_legend = match &raster_legend {
        Some(rl) => !rl.is_empty(),
        None => !vector_legend.is_empty(),
    };
    let ar = Rect::from_center_size(pos2(x + 6.0, cy), vec2(14.0, 14.0));
    if has_legend {
        icons::paint(ui.painter(), ar, if l.expanded { "chevron_down" } else { "chevron_right" }, t.text_dim, t.accent);
    }
    let ar_resp = ui.interact(hit(ar), egui::Id::new(("expand", l.id)), Sense::click());
    x += 16.0;
    // Visibility checkbox.
    let cb = Rect::from_center_size(pos2(x + 7.0, cy), vec2(14.0, 14.0));
    let cb_resp = ui.interact(hit(cb), egui::Id::new(("vis", l.id)), Sense::click());
    visibility_box(ui, cb, l.visible, t);
    x += 22.0;
    // Symbol patch (single symbol) or layer-type icon.
    let pr = Rect::from_center_size(pos2(x + 9.0, cy), vec2(18.0, 16.0));
    if l.is_raster() {
        icons::paint(ui.painter(), pr, "raster_layer", t.icon, t.accent);
    } else if has_legend || matches!(l.renderer, Renderer::Unique { .. }) {
        // A Unique Values layer listing nothing shows its type, not a class it doesn't draw.
        let icon = match l.geometry_type() {
            GeometryType::Point => "point_layer",
            GeometryType::Polyline => "line_layer",
            _ => "polygon_layer",
        };
        icons::paint(ui.painter(), pr, icon, t.icon, t.accent);
    } else {
        patch(ui.painter(), pr, &l.renderer.base_symbol(), l.geometry_type());
    }
    x += 24.0;
    let out_of_range = !app.session.map().in_scale_range(l, scale);
    let group_hidden = !app.session.map().shows(l) && l.visible;
    let missing = l.raster.as_ref().is_some_and(|r| r.data.is_none()) || l.source.as_ref().is_some_and(|s| s.error.is_some());
    let color = if missing {
        t.red
    } else if !l.visible || out_of_range || group_hidden {
        t.text_disabled
    } else {
        t.text
    };
    let elided = name.elided;
    name.paint(ui, r, x, color);
    if let Some((badge, bw)) = badge {
        let br = Rect::from_center_size(pos2(r.max.x - 12.0 - bw / 2.0, cy), vec2(bw, 16.0));
        ui.painter().rect_filled(br, CornerRadius::same(8), egui::Color32::from_rgb(0x00, 0xA8, 0xC0));
        ui.painter().text(br.center(), Align2::CENTER_CENTER, badge, regular(10.5), egui::Color32::WHITE);
    }
    if cb_resp.clicked() {
        let _ = app.run("layer.visibility", json!({"layer": l.id}));
    } else if ar_resp.clicked() {
        let _ = app.run("layer.expand", json!({"layer": l.id}));
    } else if resp.clicked() {
        click_layer(app, ui, l.id);
    }
    if resp.double_clicked() {
        let _ = app.run("layer.select", json!({"layer": l.id}));
        app.ui.open_right(RightPane::Symbology);
    }
    let tip = match &l.raster {
        Some(rl) => match &rl.data {
            Some(d) => crate::i18n::fmt(
                "Raster · {w} × {h} · {n} bands · {renderer}",
                &[
                    ("w", &d.width.to_string()),
                    ("h", &d.height.to_string()),
                    ("n", &d.band_count().to_string()),
                    ("renderer", tl_raw!(rl.style.renderer.label())),
                ],
            ),
            None => crate::i18n::fmt("The raster's data couldn't be read: {path}", &[("path", rl.path.as_deref().unwrap_or(""))]),
        },
        None => match &l.source {
            Some(src) if src.error.is_some() => {
                crate::i18n::fmt("The layer's file couldn't be read: {error}", &[("error", src.error.as_deref().unwrap_or(""))])
            }
            Some(src) => {
                let format = src.info().map(|i| i.format).unwrap_or("");
                let total = src.info().and_then(|i| i.count).map(|c| c.to_string()).unwrap_or_else(|| "?".into());
                match src.sampled() {
                    Some((n, matched)) => crate::i18n::fmt(
                        "{type} · {format}, read in place · a sample of {n} of the {matched} features in view (zoom in to see them all) · {renderer}",
                        &[
                            ("type", tl_raw!(l.geometry_type().label())),
                            ("format", format),
                            ("n", &n.to_string()),
                            ("matched", &matched.to_string()),
                            ("renderer", tl_raw!(l.renderer.kind_label())),
                        ],
                    ),
                    None => crate::i18n::fmt(
                        "{type} · {format}, read in place · {n} of {total} features in view · {renderer}",
                        &[
                            ("type", tl_raw!(l.geometry_type().label())),
                            ("format", format),
                            ("n", &l.dataset.features.len().to_string()),
                            ("total", &total),
                            ("renderer", tl_raw!(l.renderer.kind_label())),
                        ],
                    ),
                }
            }
            None => crate::i18n::fmt(
                "{type} · {n} features · {renderer}",
                &[
                    ("type", tl_raw!(l.geometry_type().label())),
                    ("n", &l.dataset.features.len().to_string()),
                    ("renderer", tl_raw!(l.renderer.kind_label())),
                ],
            ),
        },
    };
    // A name cut short shows whole above the details.
    let tip = if elided { format!("{shown}\n{tip}") } else { tip };
    let resp = resp.on_hover_text(tip);
    resp.context_menu(|ui| layer_menu(app, ui, l, reorder));
    // Legend classes.
    if l.expanded
        && let Some(entries) = &raster_legend
    {
        raster_legend_rows(ui, entries, 74.0, t);
    } else if l.expanded && has_legend {
        let field = l.renderer.field().unwrap_or("").to_string();
        ui.horizontal(|ui| {
            ui.add_space(74.0);
            ui.label(egui::RichText::new(field).size(11.0).color(t.text_dim));
        });
        for e in vector_legend.iter().take(40) {
            let (r, response) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
            response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &e.label));
            let pr = Rect::from_center_size(pos2(r.min.x + 84.0, r.center().y), vec2(18.0, 14.0));
            patch(ui.painter(), pr, &e.symbol, l.geometry_type());
            ui.painter().text(pos2(r.min.x + 100.0, r.center().y), Align2::LEFT_CENTER, &e.label, regular(11.5), t.text);
        }
    }
    Rect::from_min_max(r.min, pos2(r.max.x, ui.min_rect().max.y.max(r.max.y)))
}

/// A layer's right-click menu (its row in any Layers view). Move Up and Move Down only with
/// `reorder` (the drawing order view, unfiltered).
/// Makes a group of layers from a menu, then asks for its name, as the default one says nothing.
fn new_group(app: &mut RustGisApp, params: Value) {
    let Ok(made) = app.run("group.add", params) else { return };
    let Some(id) = made.get("group").and_then(Value::as_u64) else { return };
    let name = app.session.map().groups.iter().find(|g| u64::from(g.id) == id).map(|g| g.name.clone()).unwrap_or_default();
    let _ = app.run("ui.dialog", json!({"name": "renameGroup"}));
    app.dialogs.layer = id.to_string();
    app.dialogs.text = name;
}

/// The menu of several layers picked together: what acts on all of them; what needs one layer is
/// there, dimmed, so it is known why it can't be used.
fn layers_menu(app: &mut RustGisApp, ui: &mut Ui, picked: &[u32], reorder: bool) {
    let in_service = picked.iter().any(|id| app.session.map().service_groups.iter().any(|g| g.children.iter().any(|c| c.layer == *id)));
    let ids = json!(picked);
    let act = |app: &mut RustGisApp, ui: &mut Ui, id: &str, extra: Value| {
        let mut p = json!({"layer": ids});
        if let (Some(p), Some(extra)) = (p.as_object_mut(), extra.as_object()) {
            p.extend(extra.clone());
        }
        let _ = app.run(id, p);
        ui.close();
    };
    if ui.button(tl!("Zoom To Layers")).clicked() {
        act(app, ui, "layer.zoomTo", Value::Null);
    }
    ui.separator();
    // "Layers", not "Selected": the selection a bare word means is of features, and the next
    // two items are about that one.
    if ui.button(tl!("Show Layers")).clicked() {
        act(app, ui, "layer.visibility", json!({"visible": true}));
    }
    if ui.button(tl!("Hide Layers")).clicked() {
        act(app, ui, "layer.visibility", json!({"visible": false}));
    }
    let tip = tl!("Whether the Select tool picks these layers' features on the map");
    if ui.button(tl!("Allow Feature Selection")).on_hover_text(tip).clicked() {
        act(app, ui, "layer.selectable", json!({"on": true}));
    }
    if ui.button(tl!("Block Feature Selection")).on_hover_text(tip).clicked() {
        act(app, ui, "layer.selectable", json!({"on": false}));
    }
    ui.separator();
    // Not while the list is filtered: the layers would move among hidden ones.
    if ui.add_enabled(reorder, egui::Button::new(tl!("Move To Top"))).clicked() {
        act(app, ui, "layer.move", json!({"to": 0}));
    }
    if ui.add_enabled(reorder, egui::Button::new(tl!("Move To Bottom"))).clicked() {
        act(app, ui, "layer.move", json!({"to": 1_000_000_000u64}));
    }
    // As one layer's Group menu; a service's collections stay in their service group.
    if in_service {
        ui.add_enabled(false, egui::Button::new(tl!("Group")))
            .on_disabled_hover_text(tl!("A service's layers stay in their service group."));
    } else {
        let groups: Vec<(u32, String)> = app.session.map().groups.iter().map(|g| (g.id, g.name.clone())).collect();
        let grouped = picked.iter().any(|id| app.session.map().group_of(*id).is_some());
        ui.menu_button(tl!("Group"), |ui| {
            if ui.button(tl!("New Group")).clicked() {
                new_group(app, json!({"layers": picked}));
                ui.close();
            }
            if grouped && ui.button(tl!("Remove From Group")).clicked() {
                act(app, ui, "layer.group", json!({"group": null}));
            }
            if !groups.is_empty() {
                ui.separator();
                for (id, name) in &groups {
                    if ui.button(crate::i18n::fmt("Move To {group}", &[("group", name)])).clicked() {
                        act(app, ui, "layer.group", json!({"group": id}));
                    }
                }
            }
        });
    }
    ui.separator();
    for label in ["Attribute Table", "Symbology"] {
        ui.add_enabled(false, egui::Button::new(tl!(label))).on_disabled_hover_text(tl!("Pick a single layer."));
    }
    ui.separator();
    if ui.button(tl!("Remove")).clicked() {
        act(app, ui, "layer.remove", Value::Null);
    }
}

fn layer_menu(app: &mut RustGisApp, ui: &mut Ui, l: &Layer, reorder: bool) {
    // Once, as the menu opens: a command each frame it stays open would draw the map again each
    // time (a command moves the session's revision).
    // A layer outside the layer selection becomes the only one picked; one inside keeps it.
    if !app.session.selected_layers().contains(&l.id) {
        let _ = app.session.run("layer.select", &json!({"layer": l.id}));
    }
    let picked = app.session.selected_layers();
    if picked.len() > 1 {
        layers_menu(app, ui, &picked, reorder);
        return;
    }
    let service_child = app.session.map().service_groups.iter().any(|g| g.children.iter().any(|c| c.layer == l.id));
    if service_child && ui.add_enabled(app.ui.service_import.is_none(), egui::Button::new(tl!("Refresh"))).clicked() {
        let _ = app.run("ui.refreshService", json!({"layer":l.id}));
        ui.close();
    }
    let items: &[(&str, &str)] = if l.is_raster() {
        &[("Zoom To Layer", "layer.zoomTo"), ("Symbology", "ui.pane.symbology")]
    } else {
        &[("Zoom To Layer", "layer.zoomTo"), ("Attribute Table", "table.open"), ("Symbology", "ui.pane.symbology")]
    };
    for &(label, id) in items {
        if ui.button(tl!(label)).clicked() {
            let p = if id.starts_with("ui.") { Value::Null } else { json!({"layer": l.id}) };
            if id == "ui.pane.symbology" {
                app.ui.open_right(RightPane::Symbology);
            } else {
                let _ = app.run(id, p);
            }
            ui.close();
        }
    }
    if !l.is_raster() && ui.button(tl!(if l.labels.enabled { "Turn Labels Off" } else { "Label" })).clicked() {
        let _ = app.run("labels.enable", json!({"layer": l.id}));
        ui.close();
    }
    if !l.is_raster() && ui.button(tl!("Labeling Properties…")).clicked() {
        if let Err(e) = app.run("ui.pane.labeling", json!({"layer": l.id})) {
            app.session.notify(e, true);
        }
        ui.close();
    }
    if l.is_streamed() && ui.button(tl!("Load All Features")).clicked() {
        let _ = app.run("layer.loadAll", json!({"layer": l.id}));
        ui.close();
    }
    ui.separator();
    // Not while the list is filtered: the layer would move among hidden ones.
    if ui.add_enabled(reorder, egui::Button::new(tl!("Move Up"))).clicked() {
        let _ = app.run("layer.moveUp", json!({"layer": l.id}));
        ui.close();
    }
    if ui.add_enabled(reorder, egui::Button::new(tl!("Move Down"))).clicked() {
        let _ = app.run("layer.moveDown", json!({"layer": l.id}));
        ui.close();
    }
    ui.separator();
    // Whole layers (#491): pasted on a layer, the copies go above it, or at the top of its group.
    if ui.button(tl!("Copy Layer")).clicked() {
        let _ = app.run("layer.copy", json!({"layer": l.id}));
        ui.close();
    }
    let in_group = app.session.map().group_of(l.id).is_some();
    if ui.add_enabled(app.session.layer_clipboard.is_some(), egui::Button::new(paste_label(app, in_group))).clicked() {
        let m = app.session.map();
        let at = match m.group_of(l.id) {
            Some(g) => json!({"group": g.id}),
            None => json!({"to": m.layer_index(l.id).unwrap_or(0)}),
        };
        let _ = app.run("layer.paste", at);
        ui.close();
    }
    ui.separator();
    if ui.button(tl!("Rename…")).clicked() {
        let _ = app.run("ui.dialog", json!({"name": "renameLayer"}));
        ui.close();
    }
    // Layer groups (#413): a service's collections stay in their service group.
    if !service_child {
        let groups: Vec<(u32, String)> = app.session.map().groups.iter().map(|g| (g.id, g.name.clone())).collect();
        let current = app.session.map().group_of(l.id).map(|g| g.id);
        ui.menu_button(tl!("Group"), |ui| {
            if ui.button(tl!("New Group")).clicked() {
                new_group(app, json!({"layers": [l.id]}));
                ui.close();
            }
            if current.is_some() && ui.button(tl!("Remove From Group")).clicked() {
                let _ = app.run("layer.group", json!({"layer": l.id, "group": null}));
                ui.close();
            }
            let others: Vec<&(u32, String)> = groups.iter().filter(|(id, _)| Some(*id) != current).collect();
            if !others.is_empty() {
                ui.separator();
                for (id, name) in others {
                    if ui.button(crate::i18n::fmt("Move To {group}", &[("group", name)])).clicked() {
                        let _ = app.run("layer.group", json!({"layer": l.id, "group": id}));
                        ui.close();
                    }
                }
            }
        });
    }
    if l.is_raster() {
        if ui.button(tl!("Export Raster…")).clicked() {
            let _ = app.run("ui.exportRaster", Value::Null);
            ui.close();
        }
    } else {
        if ui.button(tl!("Definition Query…")).clicked() {
            let _ = app.run("ui.dialog", json!({"name": "definitionQuery"}));
            ui.close();
        }
        if l.time.is_some() {
            if ui.button(tl!("Unbind From Time Slider")).clicked() {
                let _ = app.run("time.unbind", json!({"layer": l.id}));
                ui.close();
            }
        } else if ui
            .add_enabled(!rustgis_engine::map::time::time_fields(&l.dataset).is_empty(), egui::Button::new(tl!("Bind To Time Slider")))
            .on_disabled_hover_text(tl!("No field of this layer holds dates."))
            .clicked()
        {
            app.time.hidden = false;
            let _ = app.run("time.bind", json!({"layer": l.id}));
            ui.close();
        }
        if ui.button(tl!("Export Features…")).clicked() {
            let _ = app.run("ui.exportLayer", Value::Null);
            ui.close();
        }
    }
    ui.separator();
    if ui.button(tl!("Remove")).clicked() {
        let _ = app.run("layer.remove", json!({"layer": l.id}));
        ui.close();
    }
}

// ---------------------------------------------------------------------------------------------
// Catalog

fn catalog(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        egui::Frame::NONE.inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
            egui::CollapsingHeader::new(egui::RichText::new(tl!("Maps")).font(semibold(12.5))).default_open(true).show(ui, |ui| {
                let maps: Vec<String> = app.session.project.maps.iter().map(|m| m.name.clone()).collect();
                for (i, m) in maps.iter().enumerate() {
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                        icons::paint(ui.painter(), r, "map", t.icon, t.accent);
                        let active = i == app.session.project.active_map;
                        let resp = ui.selectable_label(active, m);
                        if resp.clicked() {
                            let _ = app.run("map.activate", json!({"name": m}));
                        }
                        resp.context_menu(|ui| map_menu(app, ui, m));
                    });
                }
            });
            egui::CollapsingHeader::new(egui::RichText::new(tl!("Toolboxes")).font(semibold(12.5))).default_open(true).show(ui, |ui| {
                let mut boxes: Vec<&str> = tools().map(|t| t.toolbox.split(" › ").next().unwrap_or(t.toolbox)).collect();
                boxes.sort();
                boxes.dedup();
                let link = |app: &mut RustGisApp, ui: &mut Ui, tool: &rustgis_engine::analysis::ToolSpec| {
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                        icons::paint(ui.painter(), r, "tools", t.icon, t.accent);
                        if ui.link(tl!(tool.name)).on_hover_text(tl!(tool.summary)).clicked() {
                            let _ = app.run("ui.gp.open", json!({"tool": tool.id}));
                        }
                    });
                };
                for b in boxes {
                    egui::CollapsingHeader::new(crate::i18n::location(b)).default_open(false).show(ui, |ui| {
                        let in_box: Vec<&rustgis_engine::analysis::ToolSpec> =
                            tools().filter(|x| x.toolbox.split(" › ").next() == Some(b)).collect();
                        // A large toolbox (the GeoLibre suite) opens one level at a time:
                        // `GeoLibre › Raster › General`.
                        if in_box.len() > 40 {
                            toolbox_levels(app, ui, &in_box, 1, &link);
                        } else {
                            for tool in in_box {
                                link(app, ui, tool);
                            }
                        }
                    });
                }
            });
            let layouts: Vec<String> = app.session.project.layouts.iter().map(|l| l.name.clone()).collect();
            if !layouts.is_empty() {
                egui::CollapsingHeader::new(egui::RichText::new(tl!("Layouts")).font(semibold(12.5))).default_open(true).show(ui, |ui| {
                    for n in layouts {
                        ui.horizontal(|ui| {
                            let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                            icons::paint(ui.painter(), r, "new_layout", t.icon, t.accent);
                            let resp = ui.selectable_label(
                                app.session
                                    .project
                                    .layouts
                                    .get(app.session.active_layout.unwrap_or(usize::MAX))
                                    .is_some_and(|l| l.name == n),
                                &n,
                            );
                            if resp.clicked() {
                                let _ = app.run("layout.activate", json!({"name": n}));
                                app.ui.tab = "Publish".into();
                            }
                            resp.context_menu(|ui| layout_menu(app, ui, &n));
                        });
                    }
                });
            }
            let styles: Vec<String> = app.session.project.styles.iter().map(|st| st.name.clone()).collect();
            if !styles.is_empty() {
                egui::CollapsingHeader::new(egui::RichText::new(tl!("Styles")).font(semibold(12.5))).default_open(true).show(ui, |ui| {
                    for n in styles {
                        ui.horizontal(|ui| {
                            let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                            icons::paint(ui.painter(), r, "symbology", t.icon, t.accent);
                            let resp = ui.link(&n).on_hover_text(tl!("Apply to the selected layer"));
                            if resp.clicked() {
                                let _ = app.run("style.apply", json!({"name": n}));
                            }
                            resp.context_menu(|ui| {
                                if ui.button(tl!("Delete")).clicked() {
                                    let _ = app.run("style.remove", json!({"name": n}));
                                    ui.close();
                                }
                            });
                        });
                    }
                });
            }
            let ramps: Vec<Ramp> = app.session.project.ramps.iter().map(|r| Ramp::Custom(Box::new(r.clone()))).collect();
            if !ramps.is_empty() {
                egui::CollapsingHeader::new(egui::RichText::new(tl!("Color Ramps")).font(semibold(12.5))).default_open(true).show(
                    ui,
                    |ui| {
                        for r in ramps {
                            ui.horizontal(|ui| {
                                ramp_swatch(ui, &r, 16.0, false, t);
                                let resp = ui.link(r.name()).on_hover_text(tl!("Apply to the selected layer"));
                                if resp.clicked() {
                                    let _ = app.run("layer.ramp", json!({"ramp": r.name()}));
                                }
                                resp.context_menu(|ui| {
                                    if ui.button(tl!("Delete")).clicked() {
                                        let _ = app.run("ramp.remove", json!({"name": r.name()}));
                                        ui.close();
                                    }
                                });
                            });
                        }
                    },
                );
            }
            let tasks: Vec<(String, usize)> = app.session.project.tasks.iter().map(|tk| (tk.name.clone(), tk.steps.len())).collect();
            if !tasks.is_empty() {
                egui::CollapsingHeader::new(egui::RichText::new(tl!("Tasks")).font(semibold(12.5))).default_open(true).show(ui, |ui| {
                    for (n, steps) in tasks {
                        ui.horizontal(|ui| {
                            let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                            icons::paint(ui.painter(), r, "run", t.icon, t.accent);
                            let resp = ui
                                .link(crate::i18n::fmt("{name} ({n} steps)", &[("name", &n), ("n", &steps.to_string())]))
                                .on_hover_text(tl!("Open the task's steps"));
                            // A click opens the steps; running them all at once is a deliberate
                            // choice, in the context menu.
                            if resp.clicked() {
                                app.ws.task = n.clone();
                                app.ws.task_step = 0;
                                // An edit open on another task's step mustn't be saved into this one.
                                app.ws.task_edit = None;
                                app.ui.right = Some(RightPane::Tasks);
                            }
                            resp.context_menu(|ui| {
                                if ui.button(tl!("Run All Steps")).clicked() {
                                    let _ = app.run("task.run", json!({"task": n}));
                                    app.map.invalidate();
                                    ui.close();
                                }
                                if ui.button(tl!("Delete")).clicked() {
                                    let _ = app.run("task.remove", json!({"task": n}));
                                    ui.close();
                                }
                            });
                        });
                    }
                });
            }
            let folders = app.session.project.folders.clone();
            if !folders.is_empty() && !cfg!(target_arch = "wasm32") {
                egui::CollapsingHeader::new(egui::RichText::new(tl!("Folder Connections")).font(semibold(12.5))).default_open(true).show(
                    ui,
                    |ui| {
                        for f in folders {
                            ui.horizontal(|ui| {
                                let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                                icons::paint(ui.painter(), r, "add_folder", t.icon, t.accent);
                                let resp = ui.link(&f).on_hover_text(tl!("Browse this folder below"));
                                if resp.clicked() {
                                    app.table.folder = Some(std::path::PathBuf::from(&f));
                                }
                                resp.context_menu(|ui| {
                                    if ui.button(tl!("Remove")).clicked() {
                                        let _ = app.run("project.removeFolder", json!({"path": f}));
                                        ui.close();
                                    }
                                });
                            });
                        }
                    },
                );
            }
            egui::CollapsingHeader::new(egui::RichText::new(tl!("Tables")).font(semibold(12.5))).default_open(true).show(ui, |ui| {
                let tables: Vec<(u32, String)> = app.session.project.tables.iter().map(|t| (t.id, t.name.clone())).collect();
                if tables.is_empty() {
                    ui.label(egui::RichText::new(tl!("No standalone tables")).small().weak());
                }
                for (id, n) in tables {
                    if ui.link(&n).clicked() {
                        app.session.table_standalone = Some(id);
                        app.session.table_layer = None;
                    }
                }
            });
            // A web page can't list local folders.
            if !cfg!(target_arch = "wasm32") {
                egui::CollapsingHeader::new(egui::RichText::new(tl!("Folders")).font(semibold(12.5))).default_open(true).show(ui, |ui| {
                    folder_browser(app, ui, t);
                });
            }
        });
    });
}

fn folder_browser(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    let dir = app
        .table
        .folder
        .clone()
        .or_else(|| app.session.path.as_ref().and_then(|p| p.parent().map(|d| d.to_path_buf())))
        .or_else(|| std::env::current_dir().ok());
    let Some(dir) = dir else { return };
    ui.horizontal(|ui| {
        if ui.small_button("↑").on_hover_text(tl!("Parent folder")).clicked()
            && let Some(p) = dir.parent()
        {
            app.table.folder = Some(p.to_path_buf());
        }
        ui.label(egui::RichText::new(dir.display().to_string()).small().weak());
    });
    let Ok(rd) = std::fs::read_dir(&dir) else {
        ui.label(egui::RichText::new(tl!("Can't read this folder")).small().weak());
        return;
    };
    let mut entries: Vec<(bool, String, std::path::PathBuf)> = rd
        .filter_map(|e| e.ok())
        .map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            // A File Geodatabase's folder is data, listed and added like a file.
            (e.path().is_dir() && !rustgis_engine::data::source::is_gdb_name(&name), name, e.path())
        })
        .filter(|(d, n, _)| {
            !n.starts_with('.')
                && (*d
                    || [
                        "geojson",
                        "json",
                        "csv",
                        "tsv",
                        "shp",
                        "tif",
                        "tiff",
                        "asc",
                        // A File Geodatabase's folder, listed as data above.
                        "gdb",
                        "rgis",
                        "rgisx",
                        "rgispkg",
                        "rgismap",
                        "rgismpk",
                        "rgislyr",
                        "rgislayout",
                    ]
                    .iter()
                    .any(|x| n.to_lowercase().ends_with(&format!(".{x}"))))
        })
        .collect();
    entries.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.to_lowercase().cmp(&b.1.to_lowercase())));
    for (is_dir, name, path) in entries.into_iter().take(300) {
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
            let icon = if is_dir {
                "add_folder"
            } else if rustgis_engine::project::is_project_file(&name) {
                "logo"
            } else {
                "add_data"
            };
            icons::paint(ui.painter(), r, icon, t.icon, t.accent);
            let resp = ui.add(egui::Label::new(egui::RichText::new(&name).size(12.0)).sense(Sense::click()));
            let resp = resp.on_hover_text(tl!(if is_dir { "Open folder" } else { "Double-click to add" }));
            if resp.double_clicked() || (is_dir && resp.clicked()) {
                if is_dir {
                    app.table.folder = Some(path.clone());
                } else if rustgis_engine::project::opens_as_project(&name) {
                    let _ = app.run("project.open", json!({"path": path.display().to_string()}));
                    app.map.invalidate();
                } else if name.ends_with(".rgismap") || name.ends_with(".rgismpk") {
                    let _ = app.run("map.import", json!({"path": path.display().to_string()}));
                    app.map.invalidate();
                } else if name.ends_with(".rgislayout") {
                    let _ = app.run("layout.import", json!({"path": path.display().to_string()}));
                } else {
                    let _ = app.run("layer.addData", json!({"path": path.display().to_string()}));
                }
            }
        });
    }
}

/// `tools` grouped by the `depth`-th part of their toolbox (`GeoLibre › Raster › General`), one
/// closed header per group and nested again below it; tools whose toolbox stops here are listed.
fn toolbox_levels(
    app: &mut RustGisApp,
    ui: &mut Ui,
    tools: &[&'static rustgis_engine::analysis::ToolSpec],
    depth: usize,
    link: &dyn Fn(&mut RustGisApp, &mut Ui, &rustgis_engine::analysis::ToolSpec),
) {
    let part = |x: &rustgis_engine::analysis::ToolSpec| x.toolbox.split(" › ").nth(depth);
    let mut groups: Vec<&str> = tools.iter().filter_map(|x| part(x)).collect();
    groups.sort();
    groups.dedup();
    for g in groups {
        let inside: Vec<_> = tools.iter().copied().filter(|x| part(x) == Some(g)).collect();
        let path: Vec<&str> = inside.first().map(|x| x.toolbox.split(" › ").take(depth + 1).collect()).unwrap_or_default();
        egui::CollapsingHeader::new(crate::i18n::location(g)).id_salt(("toolbox", path)).default_open(false).show(ui, |ui| {
            toolbox_levels(app, ui, &inside, depth + 1, link);
        });
    }
    for tool in tools.iter().filter(|x| part(x).is_none()) {
        link(app, ui, tool);
    }
}

// ---------------------------------------------------------------------------------------------
// Geoprocessing

#[derive(Default)]
pub struct GpForm {
    pub tool: Option<&'static str>,
    pub values: HashMap<&'static str, String>,
    /// The standalone table picked for an input, by its id (names can repeat); the input's value
    /// is then the table's name, for show.
    pub tables: HashMap<&'static str, u32>,
    pub search: String,
    pub last: Option<(bool, Vec<String>)>,
    /// The fields that can hold coordinates, for the project revision and the input (its name,
    /// and its id for a table) they were read from: telling them reads rows, too much to do
    /// every frame.
    pub coordinate_fields: Option<((u64, String, Option<u32>), Vec<String>)>,
    /// A GeoLibre runner install in progress (it runs on a background thread).
    pub install: Option<std::sync::Arc<std::sync::Mutex<Install>>>,
    /// The browser's GeoLibre tools loading.
    pub web_load: Option<crate::Slot<Result<String, String>>>,
    /// A tool running in the browser's GeoLibre runner: its run id, and where the result lands.
    pub web_run: Option<(u64, &'static str, crate::Slot<rustgis_engine::cmd::gp::ExternalOutput>)>,
    /// A plugin tool running on a thread of its own, so the window stays responsive.
    pub plugin_run: Option<PluginJob>,
    /// Files picked in the browser for file parameters, which have no paths there: (name,
    /// contents), sent with the run while the parameter still shows that name.
    pub files: HashMap<&'static str, (String, Vec<u8>)>,
    /// The tool and file parameter the browser's file picker is choosing for.
    pub pending_file: Option<(&'static str, &'static str)>,
    /// Paths being typed into a layer list, by its parameter (cleared when a tool opens).
    pub drafts: HashMap<&'static str, String>,
    /// Uninstall was pressed once: the button now asks to confirm.
    pub confirm_uninstall: bool,
    /// Why the last uninstall failed (apart from `last`, which a tool run sets too).
    pub uninstall_error: Option<String>,
}

/// A plugin tool running on a thread of its own.
pub struct PluginJob {
    pub id: u64,
    pub tool: &'static str,
    /// What the run returned, once it has.
    pub slot: crate::Slot<Result<rustgis_engine::analysis::ToolResult, String>>,
    /// Stops the run, and shows what it last logged.
    pub control: std::sync::Arc<rustgis_engine::plugins::tools::Control>,
}

/// Starts a plugin tool on a thread of its own; [`poll_plugin_run`] applies what it returns.
#[cfg(not(target_arch = "wasm32"))]
fn start_plugin_run(app: &mut RustGisApp, tool: &'static str, params: &Value, ctx: &egui::Context) {
    match rustgis_engine::cmd::gp::plugin_start(&mut app.session, tool, params) {
        Ok(run) => {
            let slot = crate::Slot::default();
            let control = std::sync::Arc::new(rustgis_engine::plugins::tools::Control::default());
            let (out, ctl, ctx) = (slot.clone(), control.clone(), ctx.clone());
            let id = run.id;
            let spawned = std::thread::Builder::new().name(tool.into()).spawn(move || {
                let r = run.run(&ctl);
                // Filled even when poisoned, or the pane would wait for it forever.
                *out.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(r);
                ctx.request_repaint();
            });
            match spawned {
                Ok(_) => {
                    app.gp.plugin_run = Some(PluginJob { id, tool, slot, control });
                    app.gp.last = None;
                }
                Err(e) => {
                    rustgis_engine::cmd::gp::plugin_abandon(&mut app.session, id);
                    app.gp.last = Some((false, vec![e.to_string()]));
                }
            }
        }
        Err(e) => app.gp.last = Some((false, vec![e])),
    }
}

/// Applies a plugin tool run that finished on its thread.
pub fn poll_plugin_run(app: &mut RustGisApp, ctx: &egui::Context) {
    let Some(job) = &app.gp.plugin_run else { return };
    // Its project was closed: stop it, as its result has nowhere to go.
    if !rustgis_engine::cmd::gp::plugin_waiting(&app.session, job.id) {
        job.control.cancel();
        app.gp.plugin_run = None;
        return;
    }
    let Some(result) = take(&job.slot) else {
        // Its progress line changes as it logs.
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
        return;
    };
    let (id, tool) = (job.id, job.tool);
    app.gp.plugin_run = None;
    let r = rustgis_engine::cmd::gp::plugin_complete(&mut app.session, id, result).and_then(|_| app.run("gp.finish", json!({"id": id})));
    finished(app, tool, r);
}

/// Shows how an asynchronous tool run went: under its form when it's open, else as a notice.
fn finished(app: &mut RustGisApp, tool: &'static str, r: Result<Value, String>) {
    let last = match r {
        Ok(_) => (true, app.session.project.gp_history.last().map(|h| h.messages.clone()).unwrap_or_default()),
        Err(e) => (false, vec![e]),
    };
    // Under its own form only; with another tool open, a notification says how it went.
    if app.gp.tool == Some(tool) {
        app.gp.last = Some(last);
    } else if !last.0 {
        app.session.notify(last.1.join("\n"), true);
    } else {
        let name = rustgis_engine::analysis::tool(tool).map(|t| t.name).unwrap_or(tool);
        app.session.notify(crate::i18n::fmt("Finished: {tool}", &[("tool", tl_raw!(name))]), false);
    }
    app.map.invalidate();
}

/// Starts loading the browser's GeoLibre tools (the runner downloads once, then the browser
/// caches it).
pub fn start_geolibre_load(app: &mut RustGisApp) {
    if app.gp.web_load.is_some() {
        return;
    }
    let Some(load) = &app.services.geolibre_manifests else { return };
    let slot = crate::Slot::default();
    load(slot.clone());
    app.gp.web_load = Some(slot);
}

fn take<T>(slot: &crate::Slot<T>) -> Option<T> {
    slot.lock().ok().and_then(|mut s| s.take())
}

/// Picks up what the browser's GeoLibre runner finished: the tool list, or a tool run.
pub fn poll_geolibre(app: &mut RustGisApp, ctx: &egui::Context) {
    if let Some(r) = app.gp.web_load.as_ref().and_then(take) {
        app.gp.web_load = None;
        match r.and_then(|json| rustgis_engine::cmd::gp::external_load_manifests(&json)) {
            Ok(_) => {
                if let Some(save) = &app.services.save_pref {
                    save("geolibre_web", "on");
                }
            }
            Err(e) => app.gp.last = Some((false, vec![e])),
        }
    }
    if let Some((id, tool, out)) = app.gp.web_run.as_ref().and_then(|(id, tool, slot)| take(slot).map(|o| (*id, *tool, o))) {
        app.gp.web_run = None;
        let r = rustgis_engine::cmd::gp::external_complete(&mut app.session, id, out).and_then(|_| app.run("gp.finish", json!({"id": id})));
        finished(app, tool, r);
    }
    if app.gp.web_load.is_some() || app.gp.web_run.is_some() {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}

/// Progress of a one-click GeoLibre runner install.
#[derive(Default)]
pub struct Install {
    pub done: u64,
    pub total: Option<u64>,
    /// The installed program, or why it failed; set when the install ends.
    pub result: Option<Result<String, String>>,
}

/// A parameter taking several layers in order (a GeoLibre tool's list of rasters, such as the
/// bands of a composite): one row per layer, which can be moved up, down or removed, and a list
/// adding one of the `offered` layers not yet in it. The value is the names separated by `;`.
fn layer_list(form: &mut GpForm, ui: &mut Ui, param: &'static str, cur: &str, offered: Vec<String>) {
    let mut names: Vec<String> = cur.split(';').map(str::trim).filter(|n| !n.is_empty()).map(str::to_string).collect();
    let mut changed = false;
    let mut action: Option<(usize, i8)> = None;
    let count = names.len();
    for (i, name) in names.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(format!("{}.", i + 1)).size(12.0));
            // The buttons on the right, the name in what they leave.
            let row = vec2(ui.available_width(), ui.spacing().interact_size.y);
            ui.allocate_ui_with_layout(row, egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if crate::widgets::icon_button(ui, "close", tl!("Remove")).clicked() {
                    action = Some((i, 0));
                }
                if ui.add_enabled(i + 1 < count, egui::Button::new("▼").small()).on_hover_text(tl!("Move down")).clicked() {
                    action = Some((i, 1));
                }
                if ui.add_enabled(i > 0, egui::Button::new("▲").small()).on_hover_text(tl!("Move up")).clicked() {
                    action = Some((i, -1));
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.add(egui::Label::new(egui::RichText::new(name).size(12.0)).truncate());
                });
            });
        });
    }
    match action {
        Some((i, 0)) if i < names.len() => {
            names.remove(i);
            changed = true;
        }
        Some((i, -1)) if i > 0 && i < names.len() => {
            names.swap(i, i - 1);
            changed = true;
        }
        Some((i, 1)) if i + 1 < names.len() => {
            names.swap(i, i + 1);
            changed = true;
        }
        _ => {}
    }
    // A name holding the separator would come back as two entries: not offered.
    let left: Vec<String> = offered.into_iter().filter(|o| !o.contains(';') && !names.contains(o)).collect();
    ui.add_enabled_ui(!left.is_empty(), |ui| {
        egui::ComboBox::from_id_salt(("gp_list", param)).selected_text(tl!("Add")).width(ui.available_width() - 4.0).show_ui(ui, |ui| {
            for o in left {
                if ui.selectable_label(false, &o).clicked() {
                    names.push(o);
                    changed = true;
                }
            }
        });
    });
    // A file not opened as a layer, typed in (the runner reads paths on disk).
    #[cfg(not(target_arch = "wasm32"))]
    {
        let path = form.drafts.entry(param).or_default();
        let edit = ui.add(egui::TextEdit::singleline(path).hint_text(tl!("Add a file path")).desired_width(ui.available_width() - 4.0));
        let path_name = path.trim();
        if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !path_name.is_empty() && !path_name.contains(';') {
            names.push(path_name.to_string());
            changed = true;
            path.clear();
        }
    }
    if changed {
        form.values.insert(param, names.join(";"));
    }
}

impl GpForm {
    /// Opens a tool, pre-filling layer parameters from the active layer.
    pub fn open(&mut self, tool: Option<&'static str>, s: &Session) {
        self.tool = tool;
        self.values.clear();
        self.tables.clear();
        self.files.clear();
        self.drafts.clear();
        self.last = None;
        let Some(spec) = tool.and_then(rustgis_engine::analysis::tool) else { return };
        let active = s.active_layer.and_then(|id| s.map().layer(id));
        for p in spec.params {
            let v = match p.kind {
                ParamKind::Layer => active.map(|l| l.name.clone()),
                ParamKind::PolygonLayer => active
                    .filter(|l| l.geometry_type() == GeometryType::Polygon)
                    .or_else(|| s.map().layers.iter().find(|l| l.geometry_type() == GeometryType::Polygon))
                    .map(|l| l.name.clone()),
                ParamKind::PointLayer => active
                    .filter(|l| l.geometry_type() == GeometryType::Point)
                    .or_else(|| s.map().layers.iter().find(|l| l.geometry_type() == GeometryType::Point))
                    .map(|l| l.name.clone()),
                ParamKind::RasterLayer => {
                    active.filter(|l| l.is_raster()).or_else(|| s.map().layers.iter().find(|l| l.is_raster())).map(|l| l.name.clone())
                }
                ParamKind::VectorLayer => {
                    active.filter(|l| !l.is_raster()).or_else(|| s.map().layers.iter().find(|l| !l.is_raster())).map(|l| l.name.clone())
                }
                _ => serde_json::from_str::<Value>(p.default).ok().map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())),
            };
            if let Some(v) = v {
                self.values.insert(p.name, v);
            }
        }
    }

    /// Puts a standalone table, by its id, in the open tool's first input that takes one.
    pub fn pick_table(&mut self, id: u32, s: &Session) {
        let spec = self.tool.and_then(rustgis_engine::analysis::tool);
        let input = spec.and_then(|t| t.params.iter().find(|p| p.kind == ParamKind::Layer));
        if let (Some(p), Some(t)) = (input, s.project.table(id)) {
            self.values.insert(p.name, t.name.clone());
            self.tables.insert(p.name, id);
        }
    }

    /// Fills the open tool's form with `given` values by parameter name, over its own (a run
    /// opened again from the History pane, #539), as text in the form each field takes and sends
    /// back: several layers `;`-separated, a standalone table (given by its id) by its name and
    /// picked by its id, a switch `true`/`false`.
    pub fn fill(&mut self, given: &serde_json::Map<String, Value>) {
        let Some(spec) = self.tool.and_then(rustgis_engine::analysis::tool) else { return };
        for p in spec.params {
            let text = match (p.kind, given.get(p.name)) {
                (_, None | Some(Value::Null)) => continue,
                (ParamKind::RasterLayers | ParamKind::VectorLayers, Some(Value::Array(a))) => {
                    a.iter().map(|x| x.as_str().map_or_else(|| x.to_string(), str::to_string)).collect::<Vec<_>>().join(";")
                }
                (ParamKind::Layer, Some(Value::Number(n))) => {
                    let Some(id) = n.as_u64().and_then(|id| u32::try_from(id).ok()) else { continue };
                    self.tables.insert(p.name, id);
                    given.get(&format!("{}_name", p.name)).and_then(Value::as_str).map_or_else(|| id.to_string(), str::to_string)
                }
                (_, Some(Value::String(t))) => t.clone(),
                (_, Some(other)) => other.to_string(),
            };
            self.values.insert(p.name, text);
        }
    }
}

/// Where to get the GeoLibre tools when the runner isn't installed (desktop and CLI builds only).
fn geolibre_card(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    // Read directly, not through Session::run: a command each frame would bump the session's
    // revision and make the views redraw. The status itself is cached in the runner module.
    let status = rustgis_engine::cmd::gp::geolibre_status();
    if status["supported"] != true {
        return;
    }
    // An install that just finished: use the runner, and remember it.
    let finished = app.gp.install.as_ref().and_then(|st| st.lock().ok().and_then(|mut s| s.result.take()));
    if let Some(result) = finished {
        app.gp.install = None;
        match result.and_then(|path| app.run("gp.setGeolibre", json!({"path": path})).map(|_| path)) {
            Ok(path) => {
                if let Some(save) = &app.services.save_pref {
                    save("geolibre_runner", &path);
                }
                app.gp.last = None;
            }
            Err(e) => app.gp.last = Some((false, vec![e])),
        }
        return;
    }
    if status["web"] == true {
        geolibre_web_card(app, ui, t);
        return;
    }
    // An installed runner that lists no tools (broken, or a failed download of its list): it
    // can still be removed from here.
    if status["installed"] == true {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(tl!("The installed GeoLibre runner lists no tools.")).size(11.0).color(t.text_dim));
            geolibre_uninstall(app, ui, t);
        });
        if let Some(e) = &app.gp.uninstall_error {
            ui.label(egui::RichText::new(e).size(11.0).color(t.red));
        }
    }
    let download = status["download"].as_str().unwrap_or("https://github.com/opengeos/geolibre-rust/releases/latest").to_string();
    ui.add_space(6.0);
    egui::Frame::NONE.stroke(Stroke::new(1.0, t.border)).corner_radius(CornerRadius::same(6)).inner_margin(egui::Margin::same(10)).show(
        ui,
        |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new(tl!("Whitebox and GeoLibre tools")).font(semibold(13.0)));
            ui.label(
                egui::RichText::new(tl!(
                    "Over 1,000 raster, vector, LiDAR, hydrology and terrain tools, from an optional download of about 20 MB."
                ))
                .size(11.5)
                .color(t.text_dim),
            );
            ui.add_space(6.0);
            let progress = app.gp.install.as_ref().and_then(|st| st.lock().ok().map(|s| (s.done, s.total)));
            match progress {
                Some((done, total)) => {
                    let mb = |b: u64| format!("{:.1}", b as f64 / 1_048_576.0);
                    let text = crate::i18n::fmt("Downloading… {mb} MB", &[("mb", &mb(done))]);
                    let bar = egui::ProgressBar::new(total.map(|t| done as f32 / t.max(1) as f32).unwrap_or(0.0)).text(text);
                    ui.add(if total.is_some() { bar } else { bar.animate(true) });
                    // Keep drawing while the download runs on its thread.
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
                }
                None => {
                    #[cfg(not(target_arch = "wasm32"))]
                    if crate::widgets::primary_button(ui, tl!("Install Whitebox and GeoLibre tools"), true, vec2(0.0, 28.0)).clicked() {
                        let st = std::sync::Arc::new(std::sync::Mutex::new(Install::default()));
                        app.gp.install = Some(st.clone());
                        app.gp.last = None;
                        let ctx = ui.ctx().clone();
                        std::thread::spawn(move || {
                            let result = rustgis_engine::cmd::gp::install_geolibre(&|done, total| {
                                if let Ok(mut s) = st.lock() {
                                    s.done = done;
                                    s.total = total;
                                }
                            });
                            if let Ok(mut s) = st.lock() {
                                s.result = Some(result);
                            }
                            ctx.request_repaint();
                        });
                    }
                    ui.add_space(2.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.label(egui::RichText::new(tl!("Or")).size(11.0).color(t.text_dim));
                        ui.add(egui::Hyperlink::from_label_and_url(tl!("Download the GeoLibre runner"), download).open_in_new_tab(true));
                        #[cfg(not(target_arch = "wasm32"))]
                        if ui.small_button(tl!("Locate…")).clicked()
                            && let Some(path) = rfd::FileDialog::new().set_title(tl!("Locate the GeoLibre runner")).pick_file()
                        {
                            let path = path.display().to_string();
                            match app.run("gp.setGeolibre", json!({"path": path})) {
                                Ok(_) => {
                                    if let Some(save) = &app.services.save_pref {
                                        save("geolibre_runner", &path);
                                    }
                                    app.gp.last = None;
                                }
                                Err(e) => app.gp.last = Some((false, vec![e])),
                            }
                        }
                    });
                }
            }
            if let Some((false, msgs)) = &app.gp.last {
                for m in msgs {
                    ui.label(egui::RichText::new(m).size(11.0).color(t.red));
                }
            }
        },
    );
}

/// Uninstalls the GeoLibre tools (a second press confirms): natively the runner that Install
/// put in place (not one of the user's own, found with Locate… or on the `PATH`); in the
/// browser, the loaded tools, which then stop loading at startup.
fn geolibre_uninstall(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    let status = rustgis_engine::cmd::gp::geolibre_status();
    let web = status["web"] == true;
    if !web && status["installed"] != true {
        return;
    }
    let label = match (web, app.gp.confirm_uninstall) {
        (_, true) => tl!("Confirm uninstall"),
        (true, false) => tl!("Remove"),
        (false, false) => tl!("Uninstall"),
    };
    let tip = if web {
        tl!("Unload the GeoLibre tools and stop loading them when RustGIS starts.")
    } else {
        tl!("Delete the GeoLibre runner that RustGIS installed. Install it again at any time.")
    };
    let text = egui::RichText::new(label).size(11.0);
    let resp = ui.small_button(if app.gp.confirm_uninstall { text.color(t.red) } else { text }).on_hover_text(tip);
    if !resp.clicked() {
        // A click elsewhere takes back the first press.
        if app.gp.confirm_uninstall && ui.input(|i| i.pointer.any_click()) && !resp.hovered() {
            app.gp.confirm_uninstall = false;
        }
        return;
    }
    if !app.gp.confirm_uninstall {
        app.gp.confirm_uninstall = true;
        return;
    }
    app.gp.confirm_uninstall = false;
    match app.run("gp.uninstallGeolibre", Value::Null) {
        Ok(_) => {
            if let Some(save) = &app.services.save_pref {
                // Forget the runner Install remembered (or, in the browser, the loaded tools).
                if web {
                    save("geolibre_web", "off");
                } else {
                    save("geolibre_runner", "");
                }
            }
            app.gp.uninstall_error = None;
        }
        Err(e) => app.gp.uninstall_error = Some(e),
    }
}

/// The browser's version of the card: load the WASI runner's tools.
fn geolibre_web_card(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    ui.add_space(6.0);
    egui::Frame::NONE
        .stroke(Stroke::new(1.0, t.border))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new(tl!("Whitebox and GeoLibre tools")).font(semibold(13.0)));
            ui.label(
                egui::RichText::new(tl!(
                    "Over 1,000 raster, vector, LiDAR, hydrology and terrain tools that run in the browser, from a one-time download of about 25 MB."
                ))
                .size(11.5)
                .color(t.text_dim),
            );
            ui.add_space(6.0);
            if app.gp.web_load.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(egui::RichText::new(tl!("Loading the GeoLibre tools…")).size(11.5));
                });
            } else if crate::widgets::primary_button(ui, tl!("Load Whitebox and GeoLibre tools"), true, vec2(0.0, 28.0)).clicked() {
                app.gp.last = None;
                start_geolibre_load(app);
            }
            if let Some((false, msgs)) = &app.gp.last {
                for m in msgs {
                    ui.label(egui::RichText::new(m).size(11.0).color(t.red));
                }
            }
        });
}

fn geoprocessing(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    let Some(spec) = app.gp.tool.and_then(rustgis_engine::analysis::tool) else {
        // Tool finder.
        crate::widgets::search_field(ui, &mut app.gp.search, tl!("Find Tools"));
        ui.separator();
        ui.add_space(4.0);
        let q = app.gp.search.to_lowercase();
        let matches = |x: &rustgis_engine::analysis::ToolSpec| {
            q.is_empty() || x.name.to_lowercase().contains(&q) || x.summary.to_lowercase().contains(&q) || x.id.to_lowercase().contains(&q)
        };
        let row = |app: &mut RustGisApp, ui: &mut Ui, tool: &'static rustgis_engine::analysis::ToolSpec| {
            let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 42.0), Sense::click());
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tl!(tool.name)));
            if resp.hovered() {
                ui.painter().rect_filled(r, CornerRadius::same(6), t.hover);
            }
            icons::paint(
                ui.painter(),
                Rect::from_center_size(pos2(r.min.x + 16.0, r.center().y), vec2(20.0, 20.0)),
                tool_icon(tool.id),
                t.icon,
                t.accent,
            );
            ui.painter().text(pos2(r.min.x + 34.0, r.min.y + 13.0), Align2::LEFT_CENTER, tl!(tool.name), semibold(12.5), t.accent_text);
            let summary: String = tl!(tool.summary).chars().take(52).collect();
            ui.painter().text(pos2(r.min.x + 34.0, r.min.y + 29.0), Align2::LEFT_CENTER, summary, regular(11.0), t.text_dim);
            if resp.on_hover_text(tl!(tool.summary)).clicked() {
                app.gp.open(Some(tool.id), &app.session);
            }
        };
        // The GeoLibre suite: a thousand tools, so its toolboxes stay closed until opened (or,
        // without the runner, a card says where to get it). Above the built-in tools when
        // browsing, so it isn't lost below them; after them when searching.
        let external = rustgis_engine::cmd::gp::geolibre_tools();
        let geolibre_section = |app: &mut RustGisApp, ui: &mut Ui| {
            if external.is_empty() {
                if q.is_empty() {
                    geolibre_card(app, ui, t);
                    ui.add_space(8.0);
                }
                return;
            }
            if q.is_empty() {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(tl!("GeoLibre")).size(11.0).color(t.text_dim));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| geolibre_uninstall(app, ui, t));
                });
                if let Some(e) = &app.gp.uninstall_error {
                    ui.label(egui::RichText::new(e).size(11.0).color(t.red));
                }
                // As in GeoLibre's Processing menu: category › subcategory › tools
                // (`GeoLibre › Terrain › Derivatives`; a tool without a subcategory sits
                // directly in its category).
                let level = |tool: &rustgis_engine::analysis::ToolSpec, i: usize| tool.toolbox.split(" › ").nth(i);
                let mut tops: Vec<&str> = external.iter().filter_map(|x| level(x, 1)).collect();
                tops.sort();
                tops.dedup();
                for top in tops {
                    let in_top: Vec<_> = external.iter().filter(|x| level(x, 1) == Some(top)).collect();
                    egui::CollapsingHeader::new(format!("{} ({})", crate::i18n::location(top), in_top.len()))
                        .id_salt(("gp-box", top))
                        .default_open(false)
                        .show(ui, |ui| {
                            let mut subs: Vec<&str> = in_top.iter().filter_map(|x| level(x, 2)).collect();
                            subs.sort();
                            subs.dedup();
                            for sub in subs {
                                let in_sub: Vec<_> = in_top.iter().copied().filter(|x| level(x, 2) == Some(sub)).collect();
                                egui::CollapsingHeader::new(format!("{} ({})", crate::i18n::location(sub), in_sub.len()))
                                    .id_salt(("gp-box", top, sub))
                                    .default_open(false)
                                    .show(ui, |ui| {
                                        for tool in in_sub {
                                            row(app, ui, tool);
                                        }
                                    });
                            }
                            for tool in in_top.iter().copied().filter(|x| level(x, 2).is_none()) {
                                row(app, ui, tool);
                            }
                        });
                }
                ui.add_space(8.0);
            } else {
                const MAX: usize = 60;
                let found: Vec<_> = external.iter().filter(|x| matches(x)).collect();
                if !found.is_empty() {
                    ui.label(egui::RichText::new(tl!("GeoLibre")).size(11.0).color(t.text_dim));
                }
                for tool in found.iter().take(MAX) {
                    row(app, ui, tool);
                }
                if found.len() > MAX {
                    let more = crate::i18n::fmt("Refine the search to see {n} more", &[("n", &(found.len() - MAX).to_string())]);
                    ui.label(egui::RichText::new(more).size(11.0).color(t.text_dim));
                }
            }
        };
        // Plugin tools, by toolbox ("Plugins › Vector Toolkit"): first when browsing (a user
        // who just installed a plugin looks for it), with the matches when searching.
        let plugin_tools: Vec<&'static rustgis_engine::analysis::ToolSpec> =
            rustgis_engine::analysis::external_tools_of(rustgis_engine::plugins::tools::SOURCE).iter().filter(|x| matches(x)).collect();
        if !plugin_tools.is_empty() {
            ui.label(egui::RichText::new(tl!("Plugins")).size(11.0).color(t.text_dim));
            let mut boxes: Vec<&str> = plugin_tools.iter().map(|t| t.toolbox).collect();
            boxes.dedup();
            for b in boxes {
                let in_box: Vec<_> = plugin_tools.iter().filter(|x| x.toolbox == b).collect();
                egui::CollapsingHeader::new(format!("{} ({})", b.rsplit(" › ").next().unwrap_or(b), in_box.len()))
                    .id_salt(("gp-plugin", b))
                    .default_open(true)
                    .show(ui, |ui| {
                        for tool in in_box {
                            row(app, ui, tool);
                        }
                    });
            }
            ui.add_space(6.0);
        }
        if q.is_empty() {
            geolibre_section(app, ui);
        }
        let builtin = rustgis_engine::analysis::TOOLS;
        if q.is_empty() {
            // Browsing: one collapsible toolbox per top-level group, like the GeoLibre ones
            // (open: there are only a few built-in tools).
            ui.label(egui::RichText::new(tl!("RustGIS")).size(11.0).color(t.text_dim));
            let mut tops: Vec<&str> = builtin.iter().map(|t| t.toolbox.split(" › ").next().unwrap_or(t.toolbox)).collect();
            tops.sort();
            tops.dedup();
            for top in tops {
                let in_top: Vec<_> = builtin.iter().filter(|x| x.toolbox.split(" › ").next() == Some(top)).collect();
                egui::CollapsingHeader::new(format!("{} ({})", crate::i18n::location(top), in_top.len()))
                    .id_salt(("gp-builtin", top))
                    .default_open(true)
                    .show(ui, |ui| {
                        for tool in in_top {
                            row(app, ui, tool);
                        }
                    });
            }
            ui.add_space(6.0);
        } else {
            // Searching: the matches under their toolbox.
            let mut boxes: Vec<&str> = builtin.iter().map(|t| t.toolbox).collect();
            boxes.sort();
            boxes.dedup();
            for b in boxes {
                let found: Vec<_> = builtin.iter().filter(|x| x.toolbox == b && matches(x)).collect();
                if found.is_empty() {
                    continue;
                }
                ui.label(egui::RichText::new(crate::i18n::location(b)).size(11.0).color(t.text_dim));
                for tool in found {
                    row(app, ui, tool);
                }
                ui.add_space(6.0);
            }
        }
        if !q.is_empty() {
            geolibre_section(app, ui);
        }
        return;
    };
    // Tool form.
    ui.horizontal(|ui| {
        if crate::widgets::icon_button(ui, "back", tl!("Back to tools")).clicked() {
            app.gp.tool = None;
        }
        let (r, _) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
        icons::paint(ui.painter(), r, tool_icon(spec.id), t.icon, t.accent);
        ui.label(egui::RichText::new(tl!(spec.name)).font(semibold(15.0)));
    });
    ui.label(egui::RichText::new(tl!(spec.summary)).size(11.5).color(t.text_dim));
    ui.add_space(4.0);
    ui.separator();
    ui.add_space(4.0);
    // A standalone table is picked by its id, since names can repeat, and shown by its name.
    let tables: Vec<(u32, String)> = app.session.project.tables.iter().map(|t| (t.id, t.name.clone())).collect();
    let layer_names = |filter: Option<GeometryType>| -> Vec<String> {
        app.session.map().layers.iter().filter(|l| filter.is_none_or(|g| l.geometry_type() == g)).map(|l| l.name.clone()).collect()
    };
    let kind_names = |raster: bool| -> Vec<String> {
        app.session.map().layers.iter().filter(|l| l.is_raster() == raster).map(|l| l.name.clone()).collect()
    };
    let plugin_tool = spec.id.starts_with(rustgis_engine::plugins::PREFIX);
    let first_layer = spec
        .params
        .iter()
        .find(|p| matches!(p.kind, ParamKind::Layer | ParamKind::PolygonLayer | ParamKind::PointLayer | ParamKind::VectorLayer))
        .map(|p| p.name);
    // The file parameter whose Browse was pressed: picked once the form is drawn.
    let mut browse_for = None;
    for p in spec.params {
        let label = ui.label(egui::RichText::new(format!("{}{}", tl!(p.label), if p.required { " *" } else { "" })).size(12.0));
        if !p.help.is_empty() {
            label.on_hover_text(p.help);
        }
        let mut cur = app.gp.values.get(p.name).cloned().unwrap_or_default();
        let options: Option<Vec<String>> = match p.kind {
            // Layers by their names, then tables: the value is the name shown, and a table
            // picked is kept by its id beside it.
            ParamKind::Layer => {
                let picked = app.gp.tables.get(p.name).copied();
                let shown = if cur.is_empty() { "—" } else { cur.as_str() };
                egui::ComboBox::from_id_salt(("gp", p.name)).selected_text(shown).width(ui.available_width() - 4.0).show_ui(ui, |ui| {
                    for name in layer_names(None) {
                        if ui.selectable_label(picked.is_none() && name == cur, &name).clicked() {
                            app.gp.values.insert(p.name, name);
                            app.gp.tables.remove(p.name);
                        }
                    }
                    for (id, name) in &tables {
                        if ui.selectable_label(picked == Some(*id), name).clicked() {
                            app.gp.values.insert(p.name, name.clone());
                            app.gp.tables.insert(p.name, *id);
                        }
                    }
                });
                ui.add_space(4.0);
                continue;
            }
            ParamKind::PolygonLayer => Some(layer_names(Some(GeometryType::Polygon))),
            ParamKind::PointLayer => Some(layer_names(Some(GeometryType::Point))),
            ParamKind::Field | ParamKind::NumericField | ParamKind::CoordinateField => {
                let lname = first_layer.and_then(|n| app.gp.values.get(n)).cloned().unwrap_or_default();
                let table = first_layer.and_then(|n| app.gp.tables.get(n)).copied();
                // The table picked; else the layer or table of that name, as the tool finds it.
                let ds = match table {
                    Some(id) => app.session.project.table(id).map(|t| &t.dataset),
                    None => app.session.map().layer_by_name(&lname).map(|l| &l.dataset).or_else(|| {
                        let tables = &app.session.project.tables;
                        rustgis_engine::project::find_table(tables, &lname).and_then(|i| tables.get(i)).map(|t| &t.dataset)
                    }),
                };
                let offered = |d: &rustgis_engine::data::Dataset, i: usize, f: &rustgis_engine::data::Field| match p.kind {
                    ParamKind::NumericField => f.ty.is_numeric(),
                    ParamKind::CoordinateField => rustgis_engine::analysis::holds_coordinates(d, i),
                    _ => true,
                };
                let fields = |d: &rustgis_engine::data::Dataset| -> Vec<String> {
                    d.fields.iter().enumerate().filter(|(i, f)| offered(d, *i, f)).map(|(_, f)| f.name.clone()).collect()
                };
                // Telling which fields hold coordinates reads their rows, too much for every frame:
                // the list is kept until the project or the input changes.
                let coordinates = p.kind == ParamKind::CoordinateField;
                let key = (app.session.revision, lname, table);
                let kept = match &app.gp.coordinate_fields {
                    Some((of, kept)) if coordinates && *of == key => Some(kept.clone()),
                    _ => None,
                };
                let mut v: Vec<String> = match (ds, kept) {
                    (None, _) => Vec::new(),
                    (Some(_), Some(kept)) => kept,
                    (Some(d), None) => {
                        let v = fields(d);
                        if coordinates {
                            app.gp.coordinate_fields = Some((key, v.clone()));
                        }
                        v
                    }
                };
                // A field picked for another layer, which this one doesn't have: unset.
                if ds.is_some() && !cur.is_empty() && !v.iter().any(|f| f.eq_ignore_ascii_case(&cur)) {
                    app.gp.values.remove(p.name);
                    cur.clear();
                }
                if !p.required {
                    v.insert(0, String::new());
                }
                Some(v)
            }
            ParamKind::Choice(c) => Some(c.iter().map(|s| s.to_string()).collect()),
            ParamKind::RasterLayer | ParamKind::VectorLayer => {
                let mut v = kind_names(p.kind == ParamKind::RasterLayer);
                if !p.required {
                    v.insert(0, String::new());
                }
                Some(v)
            }
            ParamKind::RasterLayers | ParamKind::VectorLayers => {
                let raster = p.kind == ParamKind::RasterLayers;
                let offered = app.session.map().layers.iter().filter(|l| l.is_raster() == raster).map(|l| l.name.clone()).collect();
                layer_list(&mut app.gp, ui, p.name, &cur, offered);
                ui.add_space(4.0);
                continue;
            }
            ParamKind::Bool => {
                let mut on = cur == "true";
                if ui.checkbox(&mut on, "").changed() {
                    app.gp.values.insert(p.name, on.to_string());
                }
                ui.add_space(4.0);
                continue;
            }
            _ => None,
        };
        match options {
            Some(opts) => {
                let choice = matches!(p.kind, ParamKind::Choice(_));
                let shown = |v: &str| {
                    if v.is_empty() {
                        "—".to_string()
                    } else if choice {
                        tl!(v).to_string()
                    } else {
                        v.to_string()
                    }
                };
                egui::ComboBox::from_id_salt(("gp", p.name)).selected_text(shown(&cur)).width(ui.available_width() - 4.0).show_ui(
                    ui,
                    |ui| {
                        for o in opts {
                            if ui.selectable_label(o == cur, shown(&o)).clicked() {
                                app.gp.values.insert(p.name, o);
                            }
                        }
                    },
                );
            }
            None => {
                let mut v = cur;
                let hint = match p.kind {
                    ParamKind::Number => tl!("number"),
                    ParamKind::Path => tl!("file path"),
                    _ => "",
                };
                let mut browse = false;
                // Browse picks a file to read: on the desktop for any file input (GeoLibre's too);
                // in the browser, where the file itself goes with the run, only for plugins.
                let browsable = p.kind == ParamKind::Path && (app.services.open_async.is_none() || plugin_tool);
                let changed = if browsable {
                    // One row: the path box takes what the button leaves.
                    let row = vec2(ui.available_width(), ui.spacing().interact_size.y);
                    ui.allocate_ui_with_layout(row, egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        browse = ui.button(tl!("Browse")).clicked();
                        ui.add(egui::TextEdit::singleline(&mut v).hint_text(hint).desired_width(f32::INFINITY)).changed()
                    })
                    .inner
                } else {
                    ui.add(egui::TextEdit::singleline(&mut v).hint_text(hint).desired_width(f32::INFINITY)).changed()
                };
                if changed {
                    // A path typed in replaces a file picked in the browser.
                    app.gp.files.remove(p.name);
                    app.gp.values.insert(p.name, v);
                }
                if browse {
                    browse_for = Some(p.name);
                }
            }
        }
        ui.add_space(4.0);
    }
    if let Some(param) = browse_for {
        browse_file(app, spec.id, param);
    }
    ui.add_space(8.0);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
        // One browser run at a time, and one plugin run.
        let idle = app.gp.web_run.is_none() && app.gp.plugin_run.is_none();
        if crate::widgets::primary_button(ui, tl!("Run"), idle, vec2(96.0, 30.0)).clicked() {
            let mut params = serde_json::Map::new();
            // The tools of a suite or a plugin read their inputs as text; a plugin's is handed
            // every parameter, so it gets only those it declares.
            let builtin = rustgis_engine::analysis::external_source(spec).is_none();
            let plugin = spec.id.starts_with(rustgis_engine::plugins::PREFIX);
            for p in spec.params {
                if let Some(v) = app.gp.values.get(p.name).filter(|v| !v.trim().is_empty()) {
                    let val = match (p.kind, app.gp.tables.get(p.name)) {
                        // A table goes by its id: a number, which no name is taken for. Its name
                        // goes beside it (`input_name`) for whoever reads the run afterwards; the
                        // tools read only the parameters they know.
                        (ParamKind::Layer, Some(id)) => {
                            let note = format!("{}_name", p.name);
                            if !plugin && !spec.params.iter().any(|q| q.name == note) {
                                params.insert(note, json!(v));
                            }
                            if builtin { json!(id) } else { json!(id.to_string()) }
                        }
                        (ParamKind::Number, _) => v.trim().parse::<f64>().map(|n| json!(n)).unwrap_or(json!(v)),
                        // A file picked in the browser goes with its contents.
                        (ParamKind::Path, _) if app.gp.files.get(p.name).is_some_and(|(n, _)| n == v) => {
                            use base64::Engine;
                            let bytes = app.gp.files.get(p.name).map(|(_, b)| b.as_slice()).unwrap_or_default();
                            json!({"name": v, "data": base64::engine::general_purpose::STANDARD.encode(bytes)})
                        }
                        (ParamKind::Bool, _) => json!(v == "true"),
                        _ => json!(v),
                    };
                    params.insert(p.name.to_string(), val);
                }
            }
            let params = Value::Object(params);
            if rustgis_engine::cmd::gp::is_geolibre_tool(spec) && app.services.geolibre_run.is_some() {
                // The browser's runner is asynchronous: start it, and `poll_geolibre` finishes.
                match rustgis_engine::cmd::gp::external_start(&mut app.session, spec.id, &params) {
                    Ok(run) => {
                        let (id, slot) = (run.id, crate::Slot::default());
                        if let Some(start) = &app.services.geolibre_run {
                            start(run, slot.clone());
                        }
                        app.gp.web_run = Some((id, spec.id, slot));
                        app.gp.last = None;
                    }
                    Err(e) => app.gp.last = Some((false, vec![e])),
                }
            } else if plugin && !cfg!(target_arch = "wasm32") {
                // On a thread of its own: a plugin tool may run for minutes.
                #[cfg(not(target_arch = "wasm32"))]
                start_plugin_run(app, spec.id, &params, ui.ctx());
            } else {
                let r = app.run("gp.run", json!({"tool": spec.id, "params": params}));
                app.gp.last = Some(match r {
                    Ok(_) => (true, app.session.project.gp_history.last().map(|h| h.messages.clone()).unwrap_or_default()),
                    Err(e) => (false, vec![e]),
                });
                app.map.invalidate();
            }
        }
    });
    if app.gp.web_run.is_some() {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(egui::RichText::new(tl!("Running…")).size(12.0));
        });
    }
    if let Some(job) = &app.gp.plugin_run {
        ui.add_space(8.0);
        let mut cancel = false;
        ui.horizontal(|ui| {
            ui.spinner();
            let name = rustgis_engine::analysis::tool(job.tool).map(|t| t.name).unwrap_or(job.tool);
            // Two labels, not one sentence put together from translated pieces.
            ui.label(egui::RichText::new(tl!("Running…")).size(12.0));
            ui.label(egui::RichText::new(tl!(name)).size(12.0).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                cancel = ui.add_enabled(!job.control.is_cancelled(), egui::Button::new(tl!("Cancel"))).clicked();
            });
        });
        // What the tool last logged, as its progress.
        if let Some(line) = job.control.last_log() {
            ui.label(egui::RichText::new(line).size(11.0).color(t.text_dim));
        }
        if cancel {
            job.control.cancel();
        }
    }
    if let Some((ok, msgs)) = &app.gp.last {
        ui.add_space(8.0);
        let (fill, stroke, head) =
            if *ok { (t.green.gamma_multiply(0.12), t.green, "Completed") } else { (t.red.gamma_multiply(0.12), t.red, "Failed") };
        egui::Frame::NONE
            .fill(fill)
            .stroke(Stroke::new(1.0, stroke))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(egui::Margin::same(8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(egui::RichText::new(tl!(head)).font(semibold(12.5)).color(stroke));
                for m in msgs {
                    ui.label(egui::RichText::new(m).size(11.5));
                }
            });
    }
}

/// Picks a file for a tool's file parameter: its path on the desktop, and in the browser, which
/// has no paths, the file itself (it arrives in [`received_file`]).
fn browse_file(app: &mut RustGisApp, tool: &'static str, param: &'static str) {
    let exts = rustgis_engine::plugins::tools::file_extensions(tool, param);
    if let Some(open) = &app.services.open_async {
        app.gp.pending_file = Some((tool, param));
        open(&format!("{}{}", crate::TOOL_FILE_PURPOSE, exts.join(",")));
        return;
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let exts: Vec<&str> = exts.iter().map(String::as_str).collect();
        let dialog = rfd::FileDialog::new();
        let dialog = if exts.is_empty() { dialog } else { dialog.add_filter(exts.join(", "), &exts).add_filter(tl!("All files"), &["*"]) };
        if let Some(path) = dialog.pick_file() {
            app.gp.files.remove(param);
            app.gp.values.insert(param, path.display().to_string());
        }
    }
}

/// A file the browser's picker chose for a tool's file parameter: kept for the run, if that
/// tool is still open.
pub fn received_file(app: &mut RustGisApp, name: &str, bytes: &[u8]) {
    let Some((_, param)) = app.gp.pending_file.take().filter(|(t, _)| app.gp.tool == Some(*t)) else { return };
    if bytes.len() > rustgis_engine::plugins::tools::MAX_FILE {
        let mb = rustgis_engine::plugins::tools::MAX_FILE / (1024 * 1024);
        app.gp.last = Some((false, vec![crate::i18n::fmt("{name} is larger than {mb} MB", &[("name", name), ("mb", &mb.to_string())])]));
        return;
    }
    app.gp.values.insert(param, name.to_string());
    app.gp.files.insert(param, (name.to_string(), bytes.to_vec()));
}

pub fn tool_icon(id: &str) -> &'static str {
    match id {
        "buffer" => "buffer",
        "clip" => "clip",
        "erase" => "erase",
        "dissolve" => "dissolve",
        "convexHull" => "hull",
        "featureToPoint" => "centroid",
        "simplify" => "simplify",
        "selectByLocation" => "select_loc",
        "selectByAttributes" => "select_attr",
        "countPoints" => "count",
        "calculateGeometry" => "calc_geom",
        "summaryStatistics" => "stats",
        "exportFeatures" => "export_layer",
        "idwInterpolation" => "relief",
        _ => "tools",
    }
}

// ---------------------------------------------------------------------------------------------
// Symbology

fn ramp_swatch(ui: &mut Ui, ramp: &Ramp, w: f32, selected: bool, t: &Tokens) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(w, 18.0), Sense::click());
    let n = 24;
    for i in 0..n {
        let c = if ramp.is_qualitative() { ramp.pick(i * 10 / n, 10) } else { ramp.at(i as f64 / (n - 1) as f64) };
        let x0 = r.min.x + r.width() * i as f32 / n as f32;
        let x1 = r.min.x + r.width() * (i + 1) as f32 / n as f32 + 0.5;
        ui.painter().rect_filled(Rect::from_min_max(pos2(x0, r.min.y), pos2(x1, r.max.y)), 0.0, c32(c));
    }
    ui.painter().rect_stroke(
        r,
        CornerRadius::same(2),
        Stroke::new(if selected { 2.0 } else { 1.0 }, if selected { t.accent } else { t.border_strong }),
        egui::StrokeKind::Outside,
    );
    resp
}

fn symbology(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    let Some(l) = app.session.active_layer.and_then(|id| app.session.map().layer(id)).cloned() else {
        ui.label(egui::RichText::new(tl!("Select a layer in the Layers pane.")).color(t.text_dim));
        return;
    };
    if l.is_raster() {
        raster_symbology(app, ui, &l, t);
        return;
    }
    let g = l.geometry_type();
    ui.label(egui::RichText::new(&l.name).font(semibold(14.0)));
    ui.add_space(6.0);
    ui.label(egui::RichText::new(tl!("Primary symbology")).size(11.5).color(t.text_dim));
    let kinds: Vec<(&str, &str)> = if g == GeometryType::Point {
        vec![
            ("single", "Single Symbol"),
            ("unique", "Unique Values"),
            ("graduated", "Graduated Colors"),
            ("proportional", "Proportional Symbols"),
        ]
    } else {
        vec![("single", "Single Symbol"), ("unique", "Unique Values"), ("graduated", "Graduated Colors")]
    };
    let cur_kind = l.renderer.kind_label();
    ui.horizontal_wrapped(|ui| {
        for (code, label) in &kinds {
            let icon = match *code {
                "single" => "single",
                "unique" => "unique",
                "graduated" => "graduated",
                _ => "proportional",
            };
            let sel = cur_kind == *label;
            let (r, resp) = ui.allocate_exact_size(vec2(68.0, 56.0), Sense::click());
            ui.painter().rect_filled(
                r,
                CornerRadius::same(6),
                if sel {
                    t.checked
                } else if resp.hovered() {
                    t.hover
                } else {
                    t.pane
                },
            );
            ui.painter().rect_stroke(
                r,
                CornerRadius::same(6),
                Stroke::new(1.0, if sel { t.accent } else { t.border }),
                egui::StrokeKind::Inside,
            );
            icons::paint(
                ui.painter(),
                Rect::from_center_size(pos2(r.center().x, r.min.y + 19.0), vec2(24.0, 24.0)),
                icon,
                t.icon,
                t.accent,
            );
            let short = tl!(label).split(' ').next().unwrap_or(label);
            ui.painter().text(pos2(r.center().x, r.max.y - 11.0), Align2::CENTER_CENTER, short, regular(11.0), t.text);
            if resp.on_hover_text(tl!(*label)).clicked() && !sel {
                let _ = app.run("layer.symbology", json!({"type": code}));
            }
        }
    });
    ui.add_space(8.0);
    let numeric: Vec<String> = l.dataset.fields.iter().filter(|f| f.ty.is_numeric()).map(|f| f.name.clone()).collect();
    let all: Vec<String> = l.dataset.fields.iter().map(|f| f.name.clone()).collect();
    match &l.renderer {
        Renderer::Single(s) => symbol_editor(app, ui, *s, g, t),
        Renderer::Unique { field, classes, ramp, .. } => {
            field_combo(app, ui, "Field", field, &all, "unique");
            ramp_picker(app, ui, ramp, t);
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(crate::i18n::fmt("{n} classes", &[("n", &classes.len().to_string())])).size(11.5).color(t.text_dim),
            );
            legend_list(app, ui, &l, t);
        }
        Renderer::Graduated { field, breaks, ramp, method, .. } => {
            field_combo(app, ui, "Field", field, &numeric, "graduated");
            ui.horizontal(|ui| {
                ui.label(tl!("Method"));
                egui::ComboBox::from_id_salt("sym_method").selected_text(tl!(method.label())).width(180.0).show_ui(ui, |ui| {
                    for m in ClassMethod::ALL {
                        if ui.selectable_label(m == *method, tl!(m.label())).clicked() {
                            let _ = app.run("layer.symbology", json!({"type": "graduated", "field": field, "method": format!("{m:?}"), "classes": breaks.len(), "ramp": ramp.name()}));
                        }
                    }
                });
            });
            ui.horizontal(|ui| {
                ui.label(tl!("Classes"));
                let mut k = breaks.len();
                if ui.add(egui::DragValue::new(&mut k).range(1..=12)).changed() {
                    let _ = app.run(
                        "layer.symbology",
                        json!({"type": "graduated", "field": field, "method": format!("{method:?}"), "classes": k, "ramp": ramp.name()}),
                    );
                }
            });
            ramp_picker(app, ui, ramp, t);
            ui.add_space(6.0);
            legend_list(app, ui, &l, t);
        }
        Renderer::Proportional { field, base, .. } => {
            field_combo(app, ui, "Field", field, &numeric, "proportional");
            symbol_editor(app, ui, *base, g, t);
            legend_list(app, ui, &l, t);
        }
    }
    if g == GeometryType::Polyline && !l.graphics {
        ui.add_space(8.0);
        line_strokes_editor(app, ui, &l, t);
    }
    ui.add_space(10.0);
    ui.separator();
    ui.label(egui::RichText::new(tl!("Layer")).size(11.5).color(t.text_dim));
    let mut tr = l.transparency;
    ui.horizontal(|ui| {
        ui.label(tl!("Transparency"));
        ui.spacing_mut().slider_width = 150.0;
        let r = ui.add(egui::Slider::new(&mut tr, 0.0..=100.0).suffix(" %").integer());
        transparency_drag(app, ui, &r, &l, tr);
    });
}

/// The Symbology pane's Transparency slider: the map follows it while it's dragged, and letting
/// go runs `layer.transparency` once, so the drag is one undo step.
fn transparency_drag(app: &mut RustGisApp, ui: &Ui, r: &egui::Response, l: &Layer, tr: f32) {
    let id = l.id;
    crate::ribbon::live_drag(
        app,
        ui,
        r,
        l.transparency as f64,
        tr as f64,
        |app, x| {
            if let Some(l) = app.session.map_mut().layer_mut(id) {
                l.transparency = x as f32;
            }
        },
        |app, x| {
            let _ = app.run("layer.transparency", json!({"value": x}));
        },
    );
}

fn field_combo(app: &mut RustGisApp, ui: &mut Ui, label: &str, cur: &str, fields: &[String], kind: &str) {
    ui.horizontal(|ui| {
        ui.label(tl!(label));
        egui::ComboBox::from_id_salt(("sym_field", kind)).selected_text(cur).width(200.0).show_ui(ui, |ui| {
            for f in fields {
                if ui.selectable_label(f == cur, f).clicked() {
                    let _ = app.run("layer.symbology", json!({"type": kind, "field": f}));
                }
            }
        });
    });
}

fn ramp_picker(app: &mut RustGisApp, ui: &mut Ui, cur: &Ramp, t: &Tokens) {
    ui.add_space(4.0);
    ui.label(egui::RichText::new(tl!("Color scheme")).size(11.5).color(t.text_dim));
    let grid = |app: &mut RustGisApp, ui: &mut Ui, id: &str, ramps: Vec<Ramp>| {
        // Two to a row, in the width left inside the group's indent.
        let w = (ui.available_width() - 8.0) / 2.0;
        egui::Grid::new(id).spacing(vec2(8.0, 6.0)).show(ui, |ui| {
            for (i, r) in ramps.iter().enumerate() {
                // Built-in names are translated; a custom ramp's is the user's own.
                let tip = if matches!(r, Ramp::Custom(_)) { r.label() } else { tl!(r.label()) };
                if ramp_swatch(ui, r, w, r == cur, t).on_hover_text(tip).clicked() {
                    let _ = app.run("layer.ramp", json!({"ramp": r.name()}));
                }
                if i % 2 == 1 {
                    ui.end_row();
                }
            }
        });
    };
    // One collapsible list per group; the current scheme's group starts open.
    let custom: Vec<Ramp> = app.session.project.ramps.iter().map(|r| Ramp::Custom(Box::new(r.clone()))).collect();
    for g in RampGroup::ALL {
        let ramps: Vec<Ramp> =
            if g == RampGroup::Custom { custom.clone() } else { Ramp::ALL.into_iter().filter(|r| r.group() == g).collect() };
        if ramps.is_empty() {
            continue;
        }
        egui::CollapsingHeader::new(egui::RichText::new(tl!(g.label())).size(11.5).color(t.text_dim))
            .id_salt(("ramp_group", g.label()))
            .default_open(cur.group() == g)
            .show(ui, |ui| grid(app, ui, g.label(), ramps));
    }
    if ui
        .small_button(tl!("Add Color Ramp File…"))
        .on_hover_text(tl!("Add color ramps to the project from a .rgisramp file, a QGIS style, a GDAL color file or a GMT palette."))
        .clicked()
    {
        let _ = app.run("ui.openItem", json!({"kind": "ramps"}));
    }
}

/// Save as Default and Reset Default: how single-band rasters added from now on are drawn.
fn raster_default_buttons(app: &mut RustGisApp, ui: &mut Ui, style: &RasterStyle) {
    let custom = matches!(style.stretch, Stretch::Custom { .. });
    ui.horizontal(|ui| {
        let tip = if custom {
            tl!("A custom stretch is in this raster's own units: pick another stretch type to save it as the default.")
        } else {
            tl!("Draw single-band rasters added from now on with this color scheme, stretch and resampling.")
        };
        if ui
            .add_enabled(!custom, egui::Button::new(tl!("Save as Default")).small())
            .on_hover_text(tip)
            .on_disabled_hover_text(tip)
            .clicked()
        {
            match app.run("raster.setDefaultStyle", json!({"fromLayer": true})) {
                Ok(_) => app.session.notify(tl!("Saved as the default style for new single-band rasters."), false),
                Err(e) => app.session.notify(e, true),
            }
        }
        let changed = app.session.raster_defaults != RasterDefaults::default();
        if ui
            .add_enabled(changed, egui::Button::new(tl!("Reset Default")).small())
            .on_hover_text(tl!("Draw new single-band rasters with the inverted grey stretch again."))
            .clicked()
            && let Err(e) = app.run("raster.setDefaultStyle", json!({"reset": true}))
        {
            app.session.notify(e, true);
        }
    });
}

/// Where a class colour being dragged in the Symbology pane's picker waits (#314).
const CLASS_COLOR_DRAG: &str = "class_color_drag";

/// A class colour being dragged: it previews in the project, and when the pointer is let go
/// `finish_class_color_drag` puts `before` back and runs `params` (a layer.classStyle) once, so a
/// colour pick is one undo step holding the pre-drag renderer (as `live_drag` does for the
/// ribbon's sliders).
#[derive(Clone)]
struct ClassColorDrag {
    layer: u32,
    params: Value,
    before: Renderer,
}

fn finish_class_color_drag(app: &mut RustGisApp, ui: &Ui) {
    // Still dragging: the pointer is down and the window has focus (a release outside a window
    // that lost focus may never arrive).
    if ui.input(|i| i.pointer.primary_down() && i.focused) {
        return;
    }
    let key = egui::Id::new(CLASS_COLOR_DRAG);
    let Some(drag) = ui.data_mut(|d| {
        let drag = d.get_temp::<ClassColorDrag>(key);
        d.remove::<ClassColorDrag>(key);
        drag
    }) else {
        return;
    };
    apply_class_color_drag(app, drag);
}

/// Puts the pre-drag renderer back and runs the drag's layer.classStyle: one undo step.
fn apply_class_color_drag(app: &mut RustGisApp, drag: ClassColorDrag) {
    // A layer removed mid-drag took its preview with it: there is nothing to restore or style.
    let Some(layer) = app.session.map_mut().layer_mut(drag.layer) else { return };
    layer.renderer = drag.before;
    run_class_style(app, drag.params);
}

/// Runs a layer.classStyle from the pane; a refusal (the class is gone, say) shows as a notice.
fn run_class_style(app: &mut RustGisApp, params: Value) {
    if let Err(e) = app.run("layer.classStyle", params) {
        app.session.notify(e, true);
    }
}

/// The renderer's legend rows. A Unique Values or Graduated Colors class can be edited in place
/// (#314): its colour button and its legend label, which applies when the field loses focus and
/// goes back to the value or range when cleared.
fn legend_list(app: &mut RustGisApp, ui: &mut Ui, l: &Layer, t: &Tokens) {
    // Each row's class, as layer.classStyle names it (and as the renderer does); none for
    // renderers without classes.
    let classes: Vec<(Value, ClassRef)> = match &l.renderer {
        Renderer::Unique { classes, .. } => classes
            .iter()
            .map(|(k, _)| (json!({"value": k}), ClassRef::Value(k.clone())))
            .chain(std::iter::once((json!({"other": true}), ClassRef::Other)))
            .collect(),
        Renderer::Graduated { symbols, .. } => (0..symbols.len()).map(|i| (json!({"index": i}), ClassRef::Index(i))).collect(),
        _ => Vec::new(),
    };
    egui::Frame::NONE.stroke(Stroke::new(1.0, t.border)).corner_radius(CornerRadius::same(6)).inner_margin(egui::Margin::same(6)).show(
        ui,
        |ui| {
            ui.set_width(ui.available_width());
            let legend = l.renderer.legend();
            // Unique Values legends sort their classes by label: row i shows class `order[i]`,
            // and the rows past them (`<all other values>`) keep their place.
            let order = l.renderer.class_order();
            // At most MAX_UNIQUE classes (a style import can bring more), then Unique Values'
            // `<all other values>`, the legend's last row, whatever their number.
            let rows: Vec<usize> = match &l.renderer {
                Renderer::Unique { .. } => {
                    (0..legend.len().saturating_sub(1)).take(MAX_UNIQUE).chain(legend.len().checked_sub(1)).collect()
                }
                _ => (0..legend.len()).take(MAX_UNIQUE + 1).collect(),
            };
            let mut seen = std::collections::HashSet::new();
            for i in rows {
                let Some(e) = legend.get(i) else { continue };
                // A value listed twice is drawn with its first class (`symbol_for`): the repeat
                // is shown but not editable, since editing it would change the first.
                let editable = classes.get(order.get(i).copied().unwrap_or(i)).filter(|(_, r)| match r {
                    ClassRef::Value(v) => seen.insert(v.clone()),
                    _ => true,
                });
                let Some((class, class_ref)) = editable else {
                    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
                    patch(
                        ui.painter(),
                        Rect::from_center_size(pos2(r.min.x + 12.0, r.center().y), vec2(18.0, 14.0)),
                        &e.symbol,
                        l.geometry_type(),
                    );
                    ui.painter().text(pos2(r.min.x + 30.0, r.center().y), Align2::LEFT_CENTER, &e.label, regular(11.5), t.text);
                    continue;
                };
                let with = |key: &str, v: Value| {
                    let mut p = class.clone();
                    if let Some(o) = p.as_object_mut() {
                        o.insert("layer".into(), json!(l.id));
                        o.insert(key.into(), v);
                    }
                    p
                };
                ui.horizontal(|ui| {
                    // Construct hue independently of alpha: Color32 discards hidden RGB at zero.
                    let fill = e.symbol.fill;
                    let mut hsva = egui::ecolor::Hsva::from_srgb([fill.r(), fill.g(), fill.b()]);
                    hsva.a = f32::from(fill.a()) / 255.0;
                    let alpha = hsva.a;
                    // egui creates the popup from the context, not this Ui's scoped style.
                    // Borrow linear numeric editing only while a transparent picker is open.
                    let styles = (fill.a() == 0 && egui::Popup::is_any_open(ui.ctx()))
                        .then(|| [egui::Theme::Light, egui::Theme::Dark].map(|theme| (theme, ui.ctx().style_of(theme))));
                    if styles.is_some() {
                        ui.ctx().global_style_mut(|style| style.visuals.numeric_color_space = egui::style::NumericColorSpace::Linear);
                    }
                    let response = egui::color_picker::color_edit_button_hsva(ui, &mut hsva, egui::color_picker::Alpha::OnlyBlend);
                    if let Some(styles) = styles {
                        // The popup's numeric-mode button can change both themes; restore both
                        // immediately so no other picker or later frame inherits the override.
                        for (theme, style) in styles {
                            ui.ctx().set_style_of(theme, style);
                        }
                    }
                    let picked = response.on_hover_text(tl!("Class color")).changed().then(|| {
                        let edited = rustgis_engine::map::Rgba(hsva.to_srgba_unmultiplied());
                        // The numeric alpha editor also rounds RGB through Color32; an
                        // opacity-only edit must keep the class's original channels exactly.
                        if hsva.a != alpha { fill.with_alpha(edited.a()) } else { edited }
                    });
                    if let Some(color) = picked {
                        // Keep FF too: six-digit polygon colours deliberately preserve translucency.
                        let params = with("color", json!(color.hex8()));
                        let key = egui::Id::new(CLASS_COLOR_DRAG);
                        let mut dragging = ui.data(|d| d.get_temp::<ClassColorDrag>(key));
                        // One left pending on another layer (the pane switched layers mid-drag) is
                        // finished first rather than dropped with its preview in place.
                        if let Some(other) = dragging.take_if(|d| d.layer != l.id) {
                            ui.data_mut(|d| d.remove::<ClassColorDrag>(key));
                            apply_class_color_drag(app, other);
                        }
                        if ui.input(|i| i.pointer.primary_down()) || dragging.is_some() {
                            // A drag in the picker: preview it, and let finish_class_color_drag run
                            // the command once the pointer is let go.
                            let before = dragging.map_or_else(|| l.renderer.clone(), |d| d.before);
                            if let Some(layer) = app.session.map_mut().layer_mut(l.id) {
                                let g = layer.geometry_type();
                                let _ =
                                    layer.renderer.set_class_style(class_ref, &ClassStyle { color: Some(color), ..Default::default() }, g);
                            }
                            app.session.revision = app.session.revision.wrapping_add(1);
                            ui.data_mut(|d| d.insert_temp(key, ClassColorDrag { layer: l.id, params, before }));
                        } else {
                            run_class_style(app, params);
                        }
                    }
                    // The typing lives in the pane's state while the field has focus, under the
                    // class it names (not the row, which another class can take meanwhile). Enter or
                    // clicking away applies it; Escape drops it.
                    let id = egui::Id::new(("class_label", l.id, l.renderer.field().unwrap_or_default(), class.to_string()));
                    let typing = id.with("typing");
                    let focused = ui.memory(|m| m.has_focus(id));
                    let mut buf = match focused {
                        true => ui.data_mut(|d| d.get_temp::<String>(typing)).unwrap_or_else(|| e.label.clone()),
                        false => {
                            // Left over from a field that stopped being drawn while focused.
                            ui.data_mut(|d| d.remove::<String>(typing));
                            e.label.clone()
                        }
                    };
                    let r = ui
                        .add(egui::TextEdit::singleline(&mut buf).id(id).char_limit(MAX_LABEL).desired_width(ui.available_width()))
                        .on_hover_text(tl!("Legend label. Clear it to show the value again."));
                    if r.lost_focus() {
                        ui.data_mut(|d| d.remove::<String>(typing));
                        // Compared as the command keeps labels: spaces, a pasted line break or a
                        // cut alone are no change, and neither is clearing a class with no label
                        // of its own.
                        let text = normalize_label(&buf);
                        let has_label = match (&l.renderer, class_ref) {
                            (Renderer::Unique { class_labels, .. }, ClassRef::Value(v)) => class_labels.contains_key(v),
                            (Renderer::Unique { other_label, .. }, ClassRef::Other) => other_label.is_some(),
                            (Renderer::Graduated { class_labels, .. }, ClassRef::Index(i)) => class_labels.contains_key(i),
                            _ => false,
                        };
                        let changed = text != normalize_label(&e.label) && (has_label || !text.is_empty());
                        if changed && !ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            run_class_style(app, with("label", json!(buf)));
                        }
                    } else if r.has_focus() {
                        ui.data_mut(|d| d.insert_temp(typing, buf));
                    }
                });
            }
        },
    );
}

fn symbol_editor(app: &mut RustGisApp, ui: &mut Ui, s: Symbol, g: GeometryType, t: &Tokens) {
    egui::Frame::NONE.fill(t.pane_header).corner_radius(CornerRadius::same(8)).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(44.0, 36.0), Sense::hover());
            ui.painter().rect_filled(r, CornerRadius::same(4), t.ribbon);
            patch(ui.painter(), r.shrink(6.0), &s, g);
            ui.label(egui::RichText::new(tl!("Symbol")).font(semibold(12.5)));
        });
        egui::Grid::new("symbol_grid").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
            if g != GeometryType::Polyline {
                ui.label(tl!("Color"));
                let mut c = c32(s.fill);
                if crate::widgets::color_button(ui, &mut c).changed() {
                    let _ = app.run("layer.color", json!({"color": rgba(c).hex8()}));
                }
                ui.end_row();
            }
            ui.label(tl!(if g == GeometryType::Polyline { "Line color" } else { "Outline color" }));
            let mut c = c32(s.stroke);
            if crate::widgets::color_button(ui, &mut c).changed() {
                let key = if g == GeometryType::Polyline { "layer.color" } else { "layer.outline" };
                let _ = app.run(key, json!({"color": rgba(c).hex()}));
            }
            ui.end_row();
            ui.label(tl!(if g == GeometryType::Polyline { "Line width" } else { "Outline width" }));
            let mut w = s.stroke_width;
            if ui.add(egui::DragValue::new(&mut w).range(0.0..=12.0).speed(0.05).suffix(" pt")).changed() {
                let _ = app.run("layer.outline", json!({"width": w}));
            }
            ui.end_row();
            if g == GeometryType::Point {
                ui.label(tl!("Size"));
                let mut sz = s.size;
                if ui.add(egui::DragValue::new(&mut sz).range(1.0..=60.0).speed(0.1).suffix(" pt")).changed() {
                    let _ = app.run("layer.marker", json!({"size": sz}));
                }
                ui.end_row();
                ui.label(tl!("Shape"));
                egui::ComboBox::from_id_salt("marker_shape").selected_text(tl!(s.marker.label())).show_ui(ui, |ui| {
                    for m in Marker::ALL {
                        if ui.selectable_label(m == s.marker, tl!(m.label())).clicked() {
                            let _ = app.run("layer.marker", json!({"shape": m.label()}));
                        }
                    }
                });
                ui.end_row();
            }
        });
    });
}

/// A stroke as `layer.lineStrokes` takes it.
fn stroke_json(st: &LineStroke) -> Value {
    json!({
        "color": st.color.map(|c| c.hex8()),
        "width": st.width,
        "offset": st.offset,
        "cap": st.cap.code(),
        "join": st.join.code(),
    })
}

fn set_strokes(app: &mut RustGisApp, strokes: &[LineStroke]) {
    let _ = app.run("layer.lineStrokes", json!({"strokes": strokes.iter().map(stroke_json).collect::<Vec<_>>()}));
}

/// The Strokes section of a line layer's symbology: strokes stacked in one symbol, each with
/// its own colour, width, sideways offset, cap and join. Listed top first, as they are seen.
fn line_strokes_editor(app: &mut RustGisApp, ui: &mut Ui, l: &rustgis_engine::map::Layer, t: &Tokens) {
    let base = l.renderer.base_symbol();
    let strokes = l.line_strokes.clone();
    egui::Frame::NONE.fill(t.pane_header).corner_radius(CornerRadius::same(8)).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(tl!("Strokes")).font(semibold(12.5)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let add = ui.add_enabled(strokes.len() < MAX_STROKES, egui::Button::new(tl!("Add Stroke")).small());
                if add.on_hover_text(tl!("Stack another stroke on top")).clicked() {
                    let mut next = strokes.clone();
                    // The first stroke stands for the symbol's own; later ones copy the top one.
                    next.push(strokes.last().copied().unwrap_or_default());
                    set_strokes(app, &next);
                }
            });
        });
        // The composed symbol along a line drawn left to right: a positive offset shows below it.
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::hover());
        ui.painter().rect_filled(r, CornerRadius::same(4), t.ribbon);
        let preview: Vec<LineStroke> = if strokes.is_empty() { vec![LineStroke::default()] } else { strokes.clone() };
        let reach = preview.iter().map(|st| st.offset.abs() + st.resolve(&base).1 / 2.0).fold(1.0_f32, f32::max);
        let k = (18.0 / reach).min(1.0);
        let (x0, x1) = (r.min.x + 14.0, r.max.x - 14.0);
        let cap_pad = |st: &LineStroke, w: f32| if st.cap == LineCap::Butt { 0.0 } else { w / 2.0 };
        for st in &preview {
            let (c, w) = st.resolve(&base);
            let w = (w * k).max(0.5);
            let y = r.center().y + st.offset * k;
            let pad = cap_pad(st, w);
            if st.cap == LineCap::Round {
                ui.painter().circle_filled(pos2(x0, y), w / 2.0, c32(c));
                ui.painter().circle_filled(pos2(x1, y), w / 2.0, c32(c));
            }
            let (a, b) = if st.cap == LineCap::Square { (x0 - pad, x1 + pad) } else { (x0, x1) };
            ui.painter().line_segment([pos2(a, y), pos2(b, y)], Stroke::new(w, c32(c)));
        }
        // The line's direction.
        let (ay, ax) = (r.max.y - 6.0, r.max.x - 8.0);
        ui.painter().line_segment([pos2(ax - 10.0, ay), pos2(ax, ay)], Stroke::new(1.0, t.text_dim));
        ui.painter().line_segment([pos2(ax - 3.0, ay - 3.0), pos2(ax, ay)], Stroke::new(1.0, t.text_dim));
        ui.painter().line_segment([pos2(ax - 3.0, ay + 3.0), pos2(ax, ay)], Stroke::new(1.0, t.text_dim));
        if strokes.is_empty() {
            ui.label(
                egui::RichText::new(tl!("One stroke, from the symbol. Add strokes for casings, parallel lines and offsets."))
                    .size(11.5)
                    .color(t.text_dim),
            );
            return;
        }
        let n = strokes.len();
        for i in (0..n).rev() {
            let Some(st) = strokes.get(i).copied() else { continue };
            ui.add_space(4.0);
            ui.separator();
            ui.horizontal(|ui| {
                let title = crate::i18n::fmt("Stroke {n}", &[("n", &(n - i).to_string())]);
                ui.label(egui::RichText::new(title).size(11.5).color(t.text_dim));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if crate::widgets::icon_button(ui, "close", tl!("Remove")).clicked() {
                        let mut next = strokes.clone();
                        next.remove(i);
                        set_strokes(app, &next);
                    }
                    if ui.add_enabled(i > 0, egui::Button::new("↓").small()).on_hover_text(tl!("Move down")).clicked() {
                        let mut next = strokes.clone();
                        next.swap(i, i - 1);
                        set_strokes(app, &next);
                    }
                    if ui.add_enabled(i + 1 < n, egui::Button::new("↑").small()).on_hover_text(tl!("Move up")).clicked() {
                        let mut next = strokes.clone();
                        next.swap(i, i + 1);
                        set_strokes(app, &next);
                    }
                });
            });
            let set = |app: &mut RustGisApp, new: LineStroke| {
                let mut next = strokes.clone();
                if let Some(slot) = next.get_mut(i) {
                    *slot = new;
                }
                set_strokes(app, &next);
            };
            egui::Grid::new(("stroke_grid", i)).num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
                let (c, w) = st.resolve(&base);
                ui.label(tl!("Color"));
                ui.horizontal(|ui| {
                    let mut cc = c32(c);
                    if crate::widgets::color_button(ui, &mut cc).changed() {
                        set(app, LineStroke { color: Some(rgba(cc)), ..st });
                    }
                    let mut from_symbol = st.color.is_none();
                    if ui.checkbox(&mut from_symbol, tl!("From symbol")).on_hover_text(tl!("Use each feature's line color")).changed() {
                        set(app, LineStroke { color: if from_symbol { None } else { Some(c) }, ..st });
                    }
                });
                ui.end_row();
                ui.label(tl!("Width"));
                ui.horizontal(|ui| {
                    let mut ww = w;
                    let drag = egui::DragValue::new(&mut ww).range(0.0..=MAX_STROKE_WIDTH).speed(0.05).suffix(" pt");
                    if ui.add_enabled(st.width.is_some(), drag).changed() {
                        set(app, LineStroke { width: Some(ww), ..st });
                    }
                    let mut from_symbol = st.width.is_none();
                    if ui.checkbox(&mut from_symbol, tl!("From symbol")).on_hover_text(tl!("Use each feature's line width")).changed() {
                        set(app, LineStroke { width: if from_symbol { None } else { Some(w) }, ..st });
                    }
                });
                ui.end_row();
                ui.label(tl!("Offset"));
                let mut off = st.offset;
                let drag = egui::DragValue::new(&mut off).range(-MAX_STROKE_OFFSET..=MAX_STROKE_OFFSET).speed(0.05).suffix(" pt");
                if ui
                    .add(drag)
                    .on_hover_text(tl!("Shifts the stroke sideways: positive to the right of the line's direction, negative to the left"))
                    .changed()
                {
                    set(app, LineStroke { offset: off, ..st });
                }
                ui.end_row();
                ui.label(tl!("Cap"));
                egui::ComboBox::from_id_salt(("stroke_cap", i)).selected_text(tl!(st.cap.label())).show_ui(ui, |ui| {
                    for c in LineCap::ALL {
                        if ui.selectable_label(c == st.cap, tl!(c.label())).clicked() {
                            set(app, LineStroke { cap: c, ..st });
                        }
                    }
                });
                ui.end_row();
                ui.label(tl!("Join"));
                egui::ComboBox::from_id_salt(("stroke_join", i)).selected_text(tl!(st.join.label())).show_ui(ui, |ui| {
                    for j in LineJoin::ALL {
                        if ui.selectable_label(j == st.join, tl!(j.label())).clicked() {
                            set(app, LineStroke { join: j, ..st });
                        }
                    }
                });
                ui.end_row();
            });
        }
        ui.add_space(4.0);
        if ui.small_button(tl!("Use a Single Stroke")).on_hover_text(tl!("Remove the strokes and draw the symbol's line alone")).clicked() {
            set_strokes(app, &[]);
        }
    });
}

// ---------------------------------------------------------------------------------------------
// History and Details

fn history(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    let hist = app.session.project.gp_history.clone();
    if hist.is_empty() {
        ui.label(egui::RichText::new(tl!("Tools you run appear here.")).color(t.text_dim));
        return;
    }
    for (i, h) in hist.iter().enumerate().rev() {
        egui::Frame::NONE.stroke(Stroke::new(1.0, t.border)).corner_radius(CornerRadius::same(6)).inner_margin(egui::Margin::same(8)).show(
            ui,
            |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                    ui.painter().circle_filled(r.center(), 4.5, if h.ok { t.green } else { t.red });
                    // Right-clicking the tool's name, or its ⋯, gives the run as code (#539).
                    let name = ui.add(egui::Label::new(egui::RichText::new(tl!(&h.tool)).font(semibold(12.5))).sense(Sense::click()));
                    name.context_menu(|ui| history_menu(app, ui, i, h));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.menu_button("⋯", |ui| history_menu(app, ui, i, h))
                            .response
                            .on_hover_text(tl!("Copy this run as code, or open it in its tool"));
                        ui.label(egui::RichText::new(format!("{} ms", h.millis)).size(11.0).color(t.text_dim));
                    });
                });
                // When it ran, as ArcGIS Pro's history says (UTC: the app has no time zone).
                if h.at > 0 {
                    let (date, time) = rustgis_engine::map::graphics::civil(h.at);
                    ui.label(egui::RichText::new(format!("{date} {time} UTC")).size(11.0).color(t.text_dim));
                }
                for m in &h.messages {
                    ui.label(egui::RichText::new(m).size(11.0).color(t.text_dim));
                }
            },
        );
        ui.add_space(4.0);
    }
    ui.horizontal_wrapped(|ui| {
        if ui.button(tl!("Save as Python Script…")).on_hover_text(tl!("Every run, in order, as a script that opens the project")).clicked()
        {
            save_history_script(app);
        }
        if ui.button(tl!("Clear History")).clicked() {
            let _ = app.run("gp.clearHistory", Value::Null);
        }
    });
}

/// What can be done with run `i` of the history (`h`): copy it as code, or open it in its tool.
/// A run the history can't tell how to run again has them greyed out, saying why.
fn history_menu(app: &mut RustGisApp, ui: &mut Ui, i: usize, h: &rustgis_engine::GpRecord) {
    let replay = rustgis_engine::cmd::history::replay(h);
    let why = replay.as_ref().err().cloned().unwrap_or_default();
    for (label, kind, hint) in [
        ("Copy as Python", "python", "A call for the rustgis Python package"),
        ("Copy as Command", "command", "A line for the Script pane"),
        // Quoted for a POSIX shell: cmd.exe and PowerShell read quotes otherwise.
        ("Copy as CLI Command", "cli", "A rustgis-cli command, for a POSIX shell (macOS, Linux, WSL, Git Bash)"),
    ] {
        let r = ui.add_enabled(replay.is_ok(), egui::Button::new(tl!(label)));
        if r.on_hover_text(tl!(hint)).on_disabled_hover_text(why.as_str()).clicked() {
            match app.run("gp.historyCode", json!({"run": i, "as": kind})) {
                Ok(v) => ui.ctx().copy_text(v["code"].as_str().unwrap_or("").to_string()),
                Err(e) => app.session.notify(e, true),
            }
            ui.close();
        }
    }
    ui.separator();
    // A tool run with `gp.run` opens in its form, with the values it ran with as they were: the
    // form is the user's own, so nothing is left out (copied code leaves keys out). A point
    // buffered on the map has no layer to give it; a plugin's tool needs its plugin loaded.
    let id = rustgis_engine::cmd::history::tool_of(h);
    let known = id.as_deref().and_then(rustgis_engine::analysis::tool).is_some();
    let why = match &id {
        None => tl!("This run has no tool form to open"),
        Some(_) if !known => tl!("Its tool isn't available now (a plugin's, not loaded, or a suite's, not installed)"),
        Some(_) => "",
    };
    let r = ui.add_enabled(known, egui::Button::new(tl!("Open in Tool")));
    if r.on_disabled_hover_text(why).clicked() {
        let _ = app.run("ui.gp.open", json!({"tool": id, "values": h.params}));
        ui.close();
    }
}

/// Saves the history as a Python script (#539): downloaded in the browser, else where the user
/// picks.
fn save_history_script(app: &mut RustGisApp) {
    match app.run("gp.historyScript", Value::Null) {
        Ok(v) => {
            let name = v["fileName"].as_str().unwrap_or("history.py").to_string();
            let bytes = v["text"].as_str().unwrap_or("").as_bytes().to_vec();
            if app.services.download.is_some() {
                app.download_bytes(&name, bytes);
            } else if let Some(path) = crate::dialogs::pick_save(&name, &["py"], tl!("Python script")) {
                match rustgis_engine::data::io::write_atomic(std::path::Path::new(&path), &bytes) {
                    Ok(()) => app.session.notify(crate::i18n::fmt("Saved {path}", &[("path", &path)]), false),
                    Err(e) => return app.session.notify(e.to_string(), true),
                }
            } else {
                // Cancelled: nothing was saved.
                return;
            }
            // The script opens the project's saved file, which lacks what changed since.
            if app.session.dirty || app.session.path.is_none() {
                app.session.notify(tl!("Save the project too: the script opens its saved file, without changes made since."), false);
            }
        }
        Err(e) => app.session.notify(e, true),
    }
}

fn details(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    let Some(l) = app.session.active_layer.and_then(|id| app.session.map().layer(id)) else {
        ui.label(egui::RichText::new(tl!("Select a layer in the Layers pane.")).color(t.text_dim));
        return;
    };
    let e = l.extent();
    ui.label(egui::RichText::new(&l.name).font(semibold(14.0)));
    if let Some(rl) = &l.raster {
        raster_details(ui, rl, t);
        return;
    }
    egui::Grid::new("details").num_columns(2).striped(true).spacing(vec2(12.0, 4.0)).show(ui, |ui| {
        let rows = [
            ("Geometry", tl!(l.geometry_type().label()).to_string()),
            ("Features", l.dataset.features.len().to_string()),
            ("Fields", l.dataset.fields.len().to_string()),
            ("Selected", l.selection.len().to_string()),
            ("Symbology", tl!(l.renderer.kind_label()).to_string()),
            ("Source", l.dataset.source.clone().unwrap_or_else(|| tl!("In project").into())),
            ("West", format!("{:.4}°", e.min_x)),
            ("East", format!("{:.4}°", e.max_x)),
            ("South", format!("{:.4}°", e.min_y)),
            ("North", format!("{:.4}°", e.max_y)),
            ("Definition query", l.definition_query.clone().unwrap_or_else(|| "—".into())),
        ];
        for (k, v) in rows {
            ui.label(egui::RichText::new(tl!(k)).color(t.text_dim));
            ui.label(v);
            ui.end_row();
        }
    });
    ui.add_space(8.0);
    ui.label(egui::RichText::new(tl!("Fields")).font(semibold(12.5)));
    for f in &l.dataset.fields {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(&f.name).monospace().size(11.5));
            ui.label(egui::RichText::new(tl!(f.ty.label())).size(11.0).color(t.text_dim));
        });
    }
}

// ---------------------------------------------------------------------------------------------
// Rasters

/// A band's display name: "Band 3" in the interface language.
pub fn band_name(name: &str) -> String {
    let n = name.trim_start_matches(|c: char| !c.is_ascii_digit());
    if !n.is_empty() && name == format!("Band {n}") { crate::i18n::fmt("Band {n}", &[("n", n)]) } else { name.to_string() }
}

/// Legend rows for a raster layer, indented by `indent`.
fn raster_legend_rows(ui: &mut Ui, entries: &[RasterLegend], indent: f32, t: &Tokens) {
    for e in entries.iter().take(40) {
        match e {
            RasterLegend::Ramp { ramp, invert, high, low } => {
                let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 52.0), Sense::hover());
                let bar = Rect::from_min_size(pos2(r.min.x + indent + 3.0, r.min.y + 4.0), vec2(14.0, 44.0));
                let n = 22;
                for i in 0..n {
                    let tt = 1.0 - i as f64 / (n - 1) as f64;
                    let c = ramp.at(if *invert { 1.0 - tt } else { tt });
                    let y0 = bar.min.y + bar.height() * i as f32 / n as f32;
                    let y1 = bar.min.y + bar.height() * (i + 1) as f32 / n as f32 + 0.5;
                    ui.painter().rect_filled(Rect::from_min_max(pos2(bar.min.x, y0), pos2(bar.max.x, y1)), 0.0, c32(c));
                }
                ui.painter().rect_stroke(bar, CornerRadius::same(2), Stroke::new(1.0, t.border_strong), egui::StrokeKind::Outside);
                let shown = |v: &str| match v {
                    "Lit" => tl!("Lit").to_string(),
                    "Shaded" => tl!("Shaded").to_string(),
                    other => other.to_string(),
                };
                ui.painter().text(pos2(bar.max.x + 8.0, bar.min.y + 4.0), Align2::LEFT_CENTER, shown(high), regular(11.5), t.text);
                ui.painter().text(pos2(bar.max.x + 8.0, bar.max.y - 4.0), Align2::LEFT_CENTER, shown(low), regular(11.5), t.text);
            }
            RasterLegend::Class { color, label } | RasterLegend::Channel { color, label } => {
                let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
                let sw = Rect::from_center_size(pos2(r.min.x + indent + 10.0, r.center().y), vec2(16.0, 12.0));
                ui.painter().rect_filled(sw, CornerRadius::same(2), c32(*color));
                ui.painter().rect_stroke(sw, CornerRadius::same(2), Stroke::new(1.0, t.border_strong), egui::StrokeKind::Inside);
                let shown = match e {
                    RasterLegend::Channel { color, label } => {
                        let ch = if color.r() > color.g() && color.r() > color.b() {
                            tl!("Red")
                        } else if color.g() > color.b() {
                            tl!("Green")
                        } else {
                            tl!("Blue")
                        };
                        format!("{ch}: {}", band_name(label))
                    }
                    _ => label.clone(),
                };
                ui.painter().text(pos2(sw.max.x + 8.0, r.center().y), Align2::LEFT_CENTER, shown, regular(11.5), t.text);
            }
        }
    }
}

fn raster_details(ui: &mut Ui, rl: &rustgis_engine::map::RasterLayer, t: &Tokens) {
    let Some(d) = &rl.data else {
        ui.label(egui::RichText::new(tl!("The raster's data couldn't be read.")).color(t.red));
        if let Some(p) = &rl.path {
            ui.label(egui::RichText::new(p).monospace().size(11.0));
        }
        return;
    };
    let e = d.extent();
    let (cx, cy) = d.cell_size();
    let unit = if d.crs.is_geographic() { "°" } else { " m" };
    let mut rows = vec![
        ("Size", format!("{} × {}", d.width, d.height)),
        ("Bands", d.band_count().to_string()),
        ("Pixel type", d.data_type.clone()),
        ("Cell size", format!("{} × {}{unit}", rustgis_engine::map::symbology::fmt_num(cx), rustgis_engine::map::symbology::fmt_num(cy))),
        ("Coordinate system", d.crs.label()),
        ("NoData", d.nodata.map(|v| v.to_string()).unwrap_or_else(|| "—".into())),
        ("Symbology", tl!(rl.style.renderer.label()).to_string()),
        ("Stretch", tl!(rl.style.stretch.label()).to_string()),
        ("Source", rl.path.clone().or(rl.file_name.clone()).unwrap_or_else(|| tl!("In project").into())),
        ("West", format!("{:.4}°", e.min_x)),
        ("East", format!("{:.4}°", e.max_x)),
        ("South", format!("{:.4}°", e.min_y)),
        ("North", format!("{:.4}°", e.max_y)),
    ];
    if let Some((w, h)) = d.overview {
        rows.insert(1, ("Full size", format!("{w} × {h}")));
    }
    let value_w = (ui.available_width() - 140.0).max(80.0);
    egui::Grid::new("raster_details").num_columns(2).striped(true).max_col_width(value_w).spacing(vec2(12.0, 4.0)).show(ui, |ui| {
        for (k, v) in rows {
            ui.label(egui::RichText::new(tl!(k)).color(t.text_dim));
            // Long paths are cut short instead of widening the pane; hover shows them whole.
            ui.add(egui::Label::new(&v).truncate()).on_hover_text(&v);
            ui.end_row();
        }
    });
    ui.add_space(8.0);
    ui.label(egui::RichText::new(tl!("Statistics")).font(semibold(12.5)));
    let f = rustgis_engine::map::symbology::fmt_num;
    egui::Grid::new("raster_stats").num_columns(5).striped(true).spacing(vec2(10.0, 4.0)).show(ui, |ui| {
        for h in ["Band", "Min", "Max", "Mean", "Std. Dev."] {
            ui.label(egui::RichText::new(tl!(h)).size(11.0).color(t.text_dim));
        }
        ui.end_row();
        for b in d.bands.iter().take(64) {
            ui.label(egui::RichText::new(band_name(&b.name)).size(11.5));
            for v in [b.stats.min, b.stats.max, b.stats.mean, b.stats.std_dev] {
                ui.label(egui::RichText::new(f(v)).size(11.5).monospace());
            }
            ui.end_row();
        }
    });
}

/// A Min and Max for each colour channel of an RGB layer: a stretch to the bands' own minimum and
/// maximum or to bounds of your own, which an edit turns it into. Edits are kept here until every
/// pair is valid, so the layer's values don't snap back.
fn rgb_stretch_bounds(app: &mut RustGisApp, ui: &mut Ui, layer: impl std::hash::Hash + std::fmt::Debug, t: &Tokens, shown: [[f64; 2]; 3]) {
    type Bounds = [[f64; 2]; 3];
    let key = ui.id().with(("custom_stretch_rgb", layer));
    let pending = ui.data(|d| d.get_temp::<(Bounds, Bounds)>(key)).filter(|(base, _)| *base == shown);
    let mut edit = pending.map_or(shown, |(_, e)| e);
    let (mut done, mut changed) = (false, false);
    for (name, pair) in [tl!("Red"), tl!("Green"), tl!("Blue")].into_iter().zip(edit.iter_mut()) {
        let [mut lo, mut hi] = *pair;
        let speed = ((hi - lo).abs() / 200.0).clamp(1e-6, 1e6);
        ui.horizontal(|ui| {
            ui.add_sized([44.0, 18.0], egui::Label::new(name));
            ui.label(tl!("Min"));
            let a = ui.add(egui::DragValue::new(&mut lo).speed(speed));
            ui.label(tl!("Max"));
            let b = ui.add(egui::DragValue::new(&mut hi).speed(speed));
            done |= a.drag_stopped() || b.drag_stopped() || ((a.changed() || b.changed()) && !a.dragged() && !b.dragged());
            changed |= a.changed() || b.changed();
        });
        if !(lo.is_finite() && hi.is_finite() && lo < hi) {
            ui.label(egui::RichText::new(tl!("Min must be less than Max")).size(11.5).color(t.red));
        }
        *pair = [lo, hi];
    }
    let valid = edit.iter().all(|[lo, hi]| lo.is_finite() && hi.is_finite() && lo < hi);
    if valid && done && edit != shown {
        ui.data_mut(|d| d.remove::<(Bounds, Bounds)>(key));
        let col = |i: usize| edit.map(|p| p[i]);
        let _ = app.run("raster.stretch", json!({"type": "custom", "min": col(0), "max": col(1)}));
    } else if changed {
        ui.data_mut(|d| d.insert_temp(key, (shown, edit)));
    }
}

fn raster_symbology(app: &mut RustGisApp, ui: &mut Ui, l: &Layer, t: &Tokens) {
    ui.label(egui::RichText::new(&l.name).font(semibold(14.0)));
    let Some(rl) = l.raster.clone() else { return };
    let Some(d) = rl.data.clone() else {
        ui.label(egui::RichText::new(tl!("The raster's data couldn't be read.")).color(t.red));
        return;
    };
    let style = rl.style.clone();
    ui.add_space(6.0);
    ui.label(egui::RichText::new(tl!("Primary symbology")).size(11.5).color(t.text_dim));
    let has_categories = d.bands.first().is_some_and(|b| b.stats.categories.is_some());
    let palette = matches!(d.color, rustgis_engine::data::raster::ColorHint::Palette(_));
    ui.horizontal_wrapped(|ui| {
        for (code, label) in RasterRenderer::KINDS {
            let ok = match code {
                "rgb" => d.band_count() >= 3,
                "unique" => has_categories,
                "colormap" => palette,
                _ => true,
            };
            let icon = match code {
                "stretch" => "stretch",
                "rgb" => "rgb",
                "classify" => "classify",
                "unique" => "unique",
                "colormap" => "raster_layer",
                _ => "relief",
            };
            let sel = style.renderer.code() == code;
            let (r, resp) = ui.allocate_exact_size(vec2(68.0, 56.0), if ok { Sense::click() } else { Sense::hover() });
            let fill = if sel {
                t.checked
            } else if resp.hovered() && ok {
                t.hover
            } else {
                t.pane
            };
            ui.painter().rect_filled(r, CornerRadius::same(6), fill);
            ui.painter().rect_stroke(
                r,
                CornerRadius::same(6),
                Stroke::new(1.0, if sel { t.accent } else { t.border }),
                egui::StrokeKind::Inside,
            );
            let ink = if ok { t.icon } else { t.text_disabled };
            icons::paint(ui.painter(), Rect::from_center_size(pos2(r.center().x, r.min.y + 19.0), vec2(24.0, 24.0)), icon, ink, t.accent);
            let shown = tl!(label);
            let short = if shown.chars().count() > 10 { shown.split(' ').next().unwrap_or(shown) } else { shown };
            ui.painter().text(
                pos2(r.center().x, r.max.y - 11.0),
                Align2::CENTER_CENTER,
                short,
                regular(11.0),
                if ok { t.text } else { t.text_disabled },
            );
            if resp.on_hover_text(shown).clicked() && ok && !sel {
                let _ = app.run("raster.symbology", json!({"type": code}));
            }
        }
    });
    ui.add_space(8.0);
    let bands: Vec<String> = d.bands.iter().map(|b| b.name.clone()).collect();
    let band_combo = |app: &mut RustGisApp, ui: &mut Ui, label: &str, cur: usize, run: &dyn Fn(usize) -> (String, Value)| {
        ui.horizontal(|ui| {
            ui.label(tl!(label));
            egui::ComboBox::from_id_salt(("raster_band", label))
                .selected_text(band_name(bands.get(cur).map(String::as_str).unwrap_or("")))
                .width(170.0)
                .show_ui(ui, |ui| {
                    for (i, b) in bands.iter().enumerate() {
                        if ui.selectable_label(i == cur, band_name(b)).clicked() {
                            let (id, p) = run(i);
                            let _ = app.run(&id, p);
                        }
                    }
                });
        });
    };
    match &style.renderer {
        RasterRenderer::Stretch { band, ramp, invert } => {
            let code = style.renderer.code();
            band_combo(app, ui, "Band", *band, &|i| ("raster.symbology".into(), json!({"type": code, "band": i + 1})));
            ramp_picker(app, ui, ramp, t);
            let mut inv = *invert;
            if ui.checkbox(&mut inv, tl!("Invert color scheme")).changed() {
                let _ = app.run("raster.symbology", json!({"type": "stretch", "invert": inv, "ramp": ramp.name()}));
            }
        }
        RasterRenderer::Rgb { red, green, blue } => {
            for (label, key, cur) in [("Red", "red", *red), ("Green", "green", *green), ("Blue", "blue", *blue)] {
                band_combo(app, ui, label, cur, &|i| ("raster.bands".into(), json!({ key: i + 1 })));
            }
        }
        RasterRenderer::Classify { band, method, ramp, breaks, classes } => {
            let k = if *classes > 0 { *classes } else { breaks.len() };
            let (m, r) = (*method, ramp.name().to_string());
            band_combo(app, ui, "Band", *band, &|i| {
                ("raster.symbology".into(), json!({"type": "classify", "band": i + 1, "classes": k, "method": format!("{m:?}")}))
            });
            ui.horizontal(|ui| {
                ui.label(tl!("Method"));
                egui::ComboBox::from_id_salt("raster_method").selected_text(tl!(method.label())).width(180.0).show_ui(ui, |ui| {
                    for mm in ClassMethod::ALL {
                        if ui.selectable_label(mm == m, tl!(mm.label())).clicked() {
                            let _ = app
                                .run("raster.symbology", json!({"type": "classify", "method": format!("{mm:?}"), "classes": k, "ramp": r}));
                        }
                    }
                });
            });
            ui.horizontal(|ui| {
                ui.label(tl!("Classes"));
                let mut kk = k;
                if ui.add(egui::DragValue::new(&mut kk).range(1..=32)).changed() {
                    let _ = app.run("raster.symbology", json!({"type": "classify", "method": format!("{m:?}"), "classes": kk, "ramp": r}));
                }
            });
            ramp_picker(app, ui, ramp, t);
        }
        RasterRenderer::UniqueValues { band, ramp, values } => {
            band_combo(app, ui, "Band", *band, &|i| ("raster.symbology".into(), json!({"type": "unique", "band": i + 1})));
            ramp_picker(app, ui, ramp, t);
            ui.label(
                egui::RichText::new(crate::i18n::fmt("{n} classes", &[("n", &values.len().to_string())])).size(11.5).color(t.text_dim),
            );
        }
        RasterRenderer::Colormap => {
            ui.label(egui::RichText::new(tl!("Colors from the raster's own color table.")).size(11.5).color(t.text_dim));
        }
        RasterRenderer::ShadedRelief { band, ramp, azimuth, altitude, z_factor } => {
            let (az, alt, zf, rp) = (*azimuth, *altitude, *z_factor, ramp.as_ref().map(|r| r.name().to_string()));
            let relief = move |band: usize, az: f64, alt: f64, zf: f64| {
                let mut p = json!({"type": "shadedRelief", "band": band + 1, "azimuth": az, "altitude": alt, "zFactor": zf});
                match &rp {
                    Some(r) => p["ramp"] = json!(r),
                    None => p["grey"] = json!(true),
                }
                p
            };
            band_combo(app, ui, "Band", *band, &|i| ("raster.symbology".into(), relief(i, az, alt, zf)));
            egui::Grid::new("relief").num_columns(2).spacing(vec2(10.0, 4.0)).show(ui, |ui| {
                for (label, v, range, speed, which) in
                    [("Azimuth", az, 0.0..=360.0, 1.0, 0), ("Altitude", alt, 0.0..=90.0, 0.5, 1), ("Z factor", zf, 0.001..=1000.0, 0.01, 2)]
                {
                    ui.label(tl!(label));
                    let mut x = v;
                    // Kept across a drag until it is let go (#607).
                    let drag = Dragged::lend(ui, egui::Id::new(("relief_drag", l.id, which)), &mut x);
                    let r = ui.add(egui::DragValue::new(&mut x).range(range).speed(speed).max_decimals(3));
                    if drag.settled(ui, &r, &mut x) && x != v {
                        let (a, b, c) = match which {
                            0 => (x, alt, zf),
                            1 => (az, x, zf),
                            _ => (az, alt, x),
                        };
                        let _ = app.run("raster.symbology", relief(*band, a, b, c));
                    }
                    ui.end_row();
                }
            });
            let mut tint = ramp.is_some();
            if ui.checkbox(&mut tint, tl!("Tint by elevation")).changed() {
                let mut p = relief(*band, az, alt, zf);
                if tint {
                    p["ramp"] = json!("Terrain");
                    p["grey"] = json!(false);
                } else {
                    p["grey"] = json!(true);
                }
                let _ = app.run("raster.symbology", p);
            }
            if let Some(r) = ramp {
                ramp_picker(app, ui, r, t);
            }
        }
    }
    if !matches!(style.renderer, RasterRenderer::Colormap | RasterRenderer::UniqueValues { .. } | RasterRenderer::Classify { .. }) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(tl!("Stretch type"));
            egui::ComboBox::from_id_salt("raster_stretch").selected_text(tl!(style.stretch.label())).width(170.0).show_ui(ui, |ui| {
                for (code, label) in Stretch::KINDS {
                    if ui.selectable_label(style.stretch.code() == code, tl!(label)).clicked() {
                        let _ = app.run("raster.stretch", json!({"type": code}));
                    }
                }
            });
        });
        match style.stretch {
            Stretch::PercentClip { low, high } => {
                ui.horizontal(|ui| {
                    let (mut lo, mut hi) = (low, high);
                    // Each kept across a drag until it is let go (#607); both are sent together.
                    ui.label(tl!("Min %"));
                    let drag = Dragged::lend(ui, egui::Id::new(("percent_clip_drag", l.id, "low")), &mut lo);
                    let a = ui.add(egui::DragValue::new(&mut lo).range(0.0..=49.0).speed(0.1).max_decimals(2));
                    let low_settled = drag.settled(ui, &a, &mut lo);
                    ui.label(tl!("Max %"));
                    let drag = Dragged::lend(ui, egui::Id::new(("percent_clip_drag", l.id, "high")), &mut hi);
                    let b = ui.add(egui::DragValue::new(&mut hi).range(0.0..=49.0).speed(0.1).max_decimals(2));
                    let high_settled = drag.settled(ui, &b, &mut hi);
                    if (low_settled || high_settled) && (lo != low || hi != high) {
                        let _ = app.run("raster.stretch", json!({"type": "percentClip", "low": lo, "high": hi}));
                    }
                });
            }
            Stretch::Custom { .. } | Stretch::MinMax if matches!(style.renderer, RasterRenderer::Rgb { .. }) => {
                let masked = rl.masked_stats();
                let shown: [[f64; 2]; 3] = std::array::from_fn(|i| {
                    let band = style.renderer.bands().get(i).copied().unwrap_or(0);
                    let stats = masked.as_ref().and_then(|m| m.get(i)).or_else(|| d.bands.get(band).map(|b| &b.stats));
                    stats.map_or([0.0, 1.0], |st| {
                        let n = Normalizer::for_channel(style.stretch, i, st, &d.data_type);
                        [n.lo, n.hi]
                    })
                });
                rgb_stretch_bounds(app, ui, l.id, t, shown);
            }
            Stretch::Custom { min, max, .. } => {
                // Edits are kept here until both bounds are valid, so the layer's values don't snap back.
                // They remember the layer's bounds they started from, and go stale when those change.
                let key = ui.id().with(("custom_stretch", l.id));
                let pending = ui.data(|d| d.get_temp::<([f64; 2], [f64; 2])>(key)).filter(|(base, _)| *base == [min, max]);
                let [mut lo, mut hi] = pending.map_or([min, max], |(_, edit)| edit);
                let speed = ((max - min).abs() / 200.0).clamp(1e-6, 1e6);
                ui.horizontal(|ui| {
                    ui.label(tl!("Min"));
                    let a = ui.add(egui::DragValue::new(&mut lo).speed(speed));
                    ui.label(tl!("Max"));
                    let b = ui.add(egui::DragValue::new(&mut hi).speed(speed));
                    let done = a.drag_stopped() || b.drag_stopped() || ((a.changed() || b.changed()) && !a.dragged() && !b.dragged());
                    if lo.is_finite() && hi.is_finite() && lo < hi && done {
                        ui.data_mut(|d| d.remove::<([f64; 2], [f64; 2])>(key));
                        let _ = app.run("raster.stretch", json!({"type": "custom", "min": lo, "max": hi}));
                    } else if a.changed() || b.changed() {
                        ui.data_mut(|d| d.insert_temp(key, ([min, max], [lo, hi])));
                    }
                });
                if !(lo.is_finite() && hi.is_finite() && lo < hi) {
                    ui.label(egui::RichText::new(tl!("Min must be less than Max")).size(11.5).color(t.red));
                }
            }
            Stretch::StandardDeviation { n } => {
                ui.horizontal(|ui| {
                    let mut nn = n;
                    ui.label(tl!("Deviations"));
                    // Kept across a drag until it is let go (#607).
                    let drag = Dragged::lend(ui, egui::Id::new(("deviations_drag", l.id)), &mut nn);
                    let a = ui.add(egui::DragValue::new(&mut nn).range(0.1..=10.0).speed(0.05).max_decimals(2));
                    if drag.settled(ui, &a, &mut nn) && nn != n {
                        let _ = app.run("raster.stretch", json!({"type": "standardDeviation", "n": nn}));
                    }
                });
            }
            _ => {}
        }
        let mut dra = style.dra;
        if ui.checkbox(&mut dra, tl!("Dynamic range adjustment (stretch the pixels in view)")).changed() {
            let _ = app.run("raster.dra", json!({"on": dra}));
        }
    }
    ui.horizontal(|ui| {
        ui.label(tl!("Resampling"));
        egui::ComboBox::from_id_salt("raster_resampling").selected_text(tl!(style.resampling.label())).width(170.0).show_ui(ui, |ui| {
            for r in Resampling::ALL {
                if ui.selectable_label(r == style.resampling, tl!(r.label())).clicked() {
                    let _ = app.run("raster.resampling", json!({"type": format!("{r:?}")}));
                }
            }
        });
    });
    if matches!(style.renderer, RasterRenderer::Stretch { .. }) {
        raster_default_buttons(app, ui, &style);
    }
    raster_masking(app, ui, l, &d, &style, t);
    ui.add_space(6.0);
    egui::Frame::NONE.stroke(Stroke::new(1.0, t.border)).corner_radius(CornerRadius::same(6)).inner_margin(egui::Margin::same(6)).show(
        ui,
        |ui| {
            ui.set_width(ui.available_width());
            raster_legend_rows(ui, &rl.legend(), 2.0, t);
        },
    );
    ui.add_space(10.0);
    ui.separator();
    ui.label(egui::RichText::new(tl!("Layer")).size(11.5).color(t.text_dim));
    let mut tr = l.transparency;
    ui.horizontal(|ui| {
        ui.label(tl!("Transparency"));
        ui.spacing_mut().slider_width = 150.0;
        let r = ui.add(egui::Slider::new(&mut tr, 0.0..=100.0).suffix(" %").integer());
        transparency_drag(app, ui, &r, l, tr);
    });
}

/// The Masking section of a raster's symbology (#348): values drawn as no data, and a range of
/// values shown.
fn raster_masking(app: &mut RustGisApp, ui: &mut Ui, l: &Layer, d: &rustgis_engine::data::raster::Raster, style: &RasterStyle, t: &Tokens) {
    let mask = &style.mask;
    let title = if mask.is_empty() { tl!("Masking").to_string() } else { format!("{} ●", tl!("Masking")) };
    egui::CollapsingHeader::new(egui::RichText::new(title).font(semibold(12.5)))
        .id_salt(("raster_mask", l.id))
        .default_open(!mask.is_empty())
        .show(ui, |ui| {
            // The values typed are kept here until they are applied, remembering the layer's
            // values they started from (they go stale when those change).
            let text: String = mask.values.iter().map(|v| format!("{v}")).collect::<Vec<_>>().join(", ");
            let key = ui.id().with(("mask_values", l.id));
            let pending = ui.data(|d| d.get_temp::<(String, String, bool)>(key)).filter(|(base, _, _)| *base == text);
            let (mut edit, bad) = pending.map_or((text.clone(), false), |(_, e, bad)| (e, bad));
            ui.label(tl!("Hide values (no data)"));
            let r = ui
                .add(egui::TextEdit::singleline(&mut edit).hint_text(tl!("e.g. 0, 255, -9999")).desired_width(ui.available_width() - 4.0));
            if r.lost_focus() && edit != text {
                let ok = app.run("raster.mask", json!({"values": edit})).is_ok();
                ui.data_mut(|d| {
                    if ok {
                        d.remove::<(String, String, bool)>(key);
                    } else {
                        d.insert_temp(key, (text.clone(), edit.clone(), true));
                    }
                });
            } else if r.changed() {
                ui.data_mut(|d| d.insert_temp(key, (text.clone(), edit.clone(), false)));
            }
            if bad {
                ui.label(egui::RichText::new(tl!("Type numbers separated by commas")).size(11.5).color(t.red));
            }
            ui.add_space(4.0);
            let ranged = mask.min.is_some() || mask.max.is_some();
            let modes = [(0, tl!("Show all values")), (1, tl!("Hide values outside a range")), (2, tl!("Hide values inside a range"))];
            let cur = if !ranged {
                0
            } else if mask.inside {
                2
            } else {
                1
            };
            ui.horizontal(|ui| {
                ui.label(tl!("Range"));
                let shown = modes.iter().find(|(m, _)| *m == cur).map(|(_, label)| label.to_string()).unwrap_or_default();
                egui::ComboBox::from_id_salt(("raster_mask_range", l.id)).selected_text(shown).width(190.0).show_ui(ui, |ui| {
                    for (m, label) in modes {
                        if ui.selectable_label(m == cur, label).clicked() && m != cur {
                            let p = match m {
                                0 => json!({"min": null, "max": null, "inside": false}),
                                // Starting from the values of the band drawn.
                                _ if !ranged => {
                                    let band = style.renderer.bands().first().copied().unwrap_or(0);
                                    let (lo, hi) = d.bands.get(band).map_or((0.0, 1.0), |b| (b.stats.min, b.stats.max));
                                    json!({"min": lo, "max": hi, "inside": m == 2})
                                }
                                _ => json!({"inside": m == 2}),
                            };
                            let _ = app.run("raster.mask", p);
                        }
                    }
                });
            });
            if ranged {
                // As with a custom stretch: kept here until both bounds are valid.
                let (min, max) = (mask.min.unwrap_or(f64::NEG_INFINITY), mask.max.unwrap_or(f64::INFINITY));
                let key = ui.id().with(("mask_range", l.id));
                let pending = ui.data(|d| d.get_temp::<([f64; 2], [f64; 2])>(key)).filter(|(base, _)| *base == [min, max]);
                let [mut lo, mut hi] = pending.map_or([min, max], |(_, edit)| edit);
                let span = if min.is_finite() && max.is_finite() { (max - min).abs() } else { 100.0 };
                let speed = (span / 200.0).clamp(1e-6, 1e6);
                ui.horizontal(|ui| {
                    ui.label(tl!("Min"));
                    let a = ui.add(egui::DragValue::new(&mut lo).speed(speed));
                    ui.label(tl!("Max"));
                    let b = ui.add(egui::DragValue::new(&mut hi).speed(speed));
                    let done = a.drag_stopped() || b.drag_stopped() || ((a.changed() || b.changed()) && !a.dragged() && !b.dragged());
                    if lo <= hi && done {
                        ui.data_mut(|d| d.remove::<([f64; 2], [f64; 2])>(key));
                        let bound = |v: f64| if v.is_finite() { json!(v) } else { Value::Null };
                        let _ = app.run("raster.mask", json!({"min": bound(lo), "max": bound(hi)}));
                    } else if a.changed() || b.changed() {
                        ui.data_mut(|d| d.insert_temp(key, ([min, max], [lo, hi])));
                    }
                });
                if lo > hi {
                    ui.label(egui::RichText::new(tl!("Min must not be more than Max")).size(11.5).color(t.red));
                }
            }
            if !mask.is_empty() && ui.button(tl!("Clear mask")).clicked() {
                let _ = app.run("raster.mask", json!({"clear": true}));
            }
        });
}

/// A map's menu, in the Browser and on its view tab: rename, duplicate, paste layers, delete.
pub(crate) fn map_menu(app: &mut RustGisApp, ui: &mut Ui, m: &str) {
    if ui.button(tl!("Rename…")).clicked() {
        let _ = app.run("ui.dialog", json!({"name": "renameMap", "target": m}));
        ui.close();
    }
    // The copy comes into view, out of an open layout like a map tab.
    if ui.button(tl!("Duplicate")).clicked() {
        if app.run("map.duplicate", json!({"map": m})).is_ok() {
            let _ = app.run("layout.close", Value::Null);
            app.map.popup = None;
        }
        ui.close();
    }
    // Copied layers go on top of this map's, whichever map is in view (#491).
    if ui.add_enabled(app.session.layer_clipboard.is_some(), egui::Button::new(paste_label(app, false))).clicked() {
        let _ = app.run("layer.paste", json!({"map": m}));
        ui.close();
    }
    // A project keeps at least one map (`map.remove` refuses the last).
    let delete = ui.add_enabled(app.session.project.maps.len() > 1, egui::Button::new(tl!("Delete")));
    if delete.on_disabled_hover_text(tl!("A project keeps at least one map")).clicked() {
        app.map.deleting = Some(crate::mapview::Deletion::Map(m.to_string()));
        ui.close();
    }
}

/// A layout's menu, in the Browser and on its view tab: rename, duplicate, delete.
pub(crate) fn layout_menu(app: &mut RustGisApp, ui: &mut Ui, n: &str) {
    if ui.button(tl!("Rename…")).clicked() {
        let _ = app.run("ui.dialog", json!({"name": "renameLayout", "target": n}));
        ui.close();
    }
    // The copy opens, on the Publish tab like a click on a layout.
    if ui.button(tl!("Duplicate")).clicked() {
        if app.run("layout.duplicate", json!({"layout": n})).is_ok() {
            app.ui.tab = "Publish".into();
        }
        ui.close();
    }
    if ui.button(tl!("Delete")).clicked() {
        app.map.deleting = Some(crate::mapview::Deletion::Layout(n.to_string()));
        ui.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A run opened again from the History pane (#539) fills its tool's form as the form would
    /// have it: a standalone table, given by its id, by its name and picked by its id; numbers
    /// and choices as text; what wasn't given left alone.
    #[test]
    fn a_run_fills_its_tool_s_form_as_the_form_takes_it() {
        let mut f = GpForm { tool: Some("buffer"), ..GpForm::default() };
        f.values.insert("output", "kept".into());
        let given = json!({"input": 3, "input_name": "Trips", "distance": 100, "dissolve": "All", "output": null});
        f.fill(given.as_object().unwrap());
        assert_eq!(f.values.get("input").map(String::as_str), Some("Trips"));
        assert_eq!(f.tables.get("input"), Some(&3));
        assert_eq!(
            (f.values.get("distance").map(String::as_str), f.values.get("dissolve").map(String::as_str)),
            (Some("100"), Some("All"))
        );
        assert_eq!(f.values.get("output").map(String::as_str), Some("kept"), "not given");
    }

    fn layer(app: &RustGisApp, name: &str) -> u32 {
        app.session.map().layer_by_name(name).map(|l| l.id).unwrap_or_default()
    }

    /// The Data Source view (#301) names a group by a file's name (its path on hover) or a web
    /// address's host, never the address's credentials or path.
    #[test]
    fn sources_are_named_by_file_or_host() {
        let f = source_name("/data/world/countries.gpkg");
        assert_eq!((f.label.as_str(), f.hover.as_str()), ("countries.gpkg", "/data/world/countries.gpkg"));
        assert_eq!(source_name(r"C:\GIS\rivers.shp").label, "rivers.shp");
        let w = source_name("https://user:secret@tiles.example.com/{z}/{x}/{y}.png?key=abc");
        assert_eq!((w.label.as_str(), w.hover.as_str()), ("tiles.example.com", "tiles.example.com"));
        assert!(!w.key.contains("secret") && !w.hover.contains("secret") && !w.label.contains("key="));
        // Two addresses on one host share a group; two files with one name in other folders don't.
        assert_eq!(source_name("https://Tiles.Example.com/a").key, source_name("https://tiles.example.com/b").key);
        assert_ne!(source_name("/a/x.gpkg").key, source_name("/b/x.gpkg").key);
        assert_eq!(source_name("osm").label, "osm", "a database name");
        // A file:// address is a path, in the same group as the plain path.
        let u = source_name("file:///data/world/countries.gpkg");
        assert_eq!((u.label.as_str(), u.hover.as_str()), ("countries.gpkg", "/data/world/countries.gpkg"));
        assert_eq!(u.key, f.key);
        assert_eq!(source_name("file://C:/GIS/rivers.shp").label, "rivers.shp");
        // A database address: by server and database, never its credentials or options.
        let db1 = source_name("postgresql://user:secret@db.example.com:5432/gis?sslmode=require");
        assert_eq!(db1.label, "db.example.com:5432/gis");
        assert!(!db1.label.contains("secret") && !db1.key.contains("secret") && !db1.label.contains("sslmode"));
        assert_ne!(db1.key, source_name("postgresql://user@db.example.com:5432/other").key, "two databases, two groups");
        // The host ignores case, the database doesn't.
        assert_eq!(db1.key, source_name("postgresql://DB.Example.com:5432/gis").key);
        assert_ne!(db1.key, source_name("postgresql://db.example.com:5432/GIS").key);
        // file://localhost is no host; %-escapes are decoded, a stray % stays.
        assert_eq!(source_name("file://localhost/data/world/countries.gpkg").key, f.key);
        assert_eq!(source_name("file:///data/My%20Data.gpkg").label, "My Data.gpkg");
        assert_eq!(source_name("file:///data/100%+x%2.gpkg").label, "100%+x%2.gpkg");
        assert_eq!(source_name("file://user@localhost/x.gpkg").label, "x.gpkg");
    }

    /// #353: a row wider than the left pane (a long folder path in the Browser, a long favourite in
    /// Data Sources) widened the pane to its 480 px cap, and it stayed that wide on every pane.
    /// The pane keeps the width it was given; the row is cut off at its edge.
    #[test]
    fn wide_rows_never_widen_the_left_pane() {
        let long = format!("/{}", ["a_deeply_nested_folder_with_a_long_name"; 12].join("/"));
        let mut app = RustGisApp::with_sample();
        app.session.project.folders.push(long.clone());
        let fav = rustgis_engine::sources::Source { kind: "file".into(), name: long.clone(), address: long, ..Default::default() };
        app.session.sources.favourites.push(fav);
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1400.0, 860.0)).with_max_steps(64).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app,
        );
        let width = |h: &mut egui_kittest::Harness<'_, RustGisApp>, pane: LeftPane| {
            h.state_mut().ui.left = Some(pane);
            for _ in 0..6 {
                h.step();
            }
            egui::containers::panel::PanelState::load(&h.ctx, egui::Id::new("left_pane")).map(|s| s.outer_rect.width())
        };
        let start = width(&mut h, LeftPane::Contents);
        assert!(start.is_some_and(|w| w < 300.0), "the Layers pane starts at its default width: {start:?}");
        for pane in [LeftPane::Catalog, LeftPane::Sources, LeftPane::Contents] {
            let w = width(&mut h, pane);
            assert_eq!(w, start, "{pane:?} changed the pane's width");
        }
    }

    #[test]
    fn the_filter_matches_names_ignoring_case_and_spaces() {
        let q = filter_query("  CI ");
        assert_eq!(q, "ci");
        assert!(filter_shows(&q, "Cities") && !filter_shows(&q, "Countries"));
        assert!(filter_shows(&filter_query("   "), "Anything"), "an empty filter shows everything");
    }

    /// The active layer follows the Layers filter (#300): a layer it hides stops being active and
    /// comes back once the filter shows it, unless another layer was picked meanwhile.
    #[test]
    fn the_active_layer_follows_the_layer_filter() {
        let mut app = RustGisApp::with_sample();
        let (countries, cities) = (layer(&app, "Countries"), layer(&app, "Cities"));
        app.session.active_layer = Some(countries);
        follow_layer_filter(&mut app);
        app.ui.layer_filter = "ci".into();
        follow_layer_filter(&mut app);
        assert_eq!((app.session.active_layer, app.ui.layer_filter_hid), (None, Some(countries)), "hidden, so not active");
        follow_layer_filter(&mut app);
        assert_eq!(app.session.active_layer, None, "still hidden");
        // Escape (the filter cleared): Countries is active again.
        app.ui.layer_filter.clear();
        follow_layer_filter(&mut app);
        assert_eq!((app.session.active_layer, app.ui.layer_filter_hid), (Some(countries), None));
        // Another layer picked while Countries is hidden wins.
        app.ui.layer_filter = "ci".into();
        follow_layer_filter(&mut app);
        app.session.active_layer = Some(cities);
        follow_layer_filter(&mut app);
        app.ui.layer_filter.clear();
        follow_layer_filter(&mut app);
        assert_eq!((app.session.active_layer, app.ui.layer_filter_hid), (Some(cities), None));
        // Another map comes into view: no filter, nothing remembered.
        app.ui.layer_filter = "lakes".into();
        follow_layer_filter(&mut app);
        assert_eq!(app.ui.layer_filter_hid, Some(cities));
        let _ = app.run("map.new", json!({"name": "Second"}));
        follow_layer_filter(&mut app);
        assert_eq!((app.ui.layer_filter.as_str(), app.ui.layer_filter_hid), ("", None));
    }

    /// The Symbology pane's Transparency slider changes the layer while it's dragged, not only
    /// when it's let go, and the whole drag is one undo step back to where it started.
    #[test]
    fn transparency_follows_the_slider_while_dragging() {
        use egui::{Event, Modifiers, PointerButton};
        use egui_kittest::kittest::Queryable;
        let mut app = RustGisApp::with_sample();
        let id = layer(&app, "Countries");
        app.session.active_layer = Some(id);
        app.ui.open_right(RightPane::Symbology);
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1400.0, 1400.0)).with_max_steps(64).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app,
        );
        h.run_steps(6);
        let transparency = |h: &egui_kittest::Harness<'_, RustGisApp>| h.state().session.map().layer(id).map(|l| l.transparency);
        let start = transparency(&h);
        // The pane's slider, at its foot (the window is tall enough to show it): the rightmost one.
        let rect = h.query_all_by_role(egui::accesskit::Role::Slider).map(|n| n.rect()).max_by(|a, b| a.min.x.total_cmp(&b.min.x)).unwrap();
        let (from, to) = (rect.left_center(), rect.center());
        let button = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        h.event(Event::PointerMoved(from));
        h.step();
        h.event(button(from, true));
        h.step();
        for i in 1..=4 {
            h.event(Event::PointerMoved(from + (to - from) * i as f32 / 4.0));
            h.step();
        }
        let mid = transparency(&h);
        assert!(mid.is_some_and(|t| (40.0..=60.0).contains(&t)), "the layer follows the drag: {start:?} → {mid:?}");
        h.event(button(to, false));
        h.step();
        assert_eq!(transparency(&h), mid, "letting go keeps the value");
        h.state_mut().run("edit.undo", Value::Null).unwrap();
        assert_eq!(transparency(&h), start, "one undo takes the whole drag back");
    }

    /// Editing opacity must not turn a transparent red or a near-transparent colour into black.
    /// Exercise the real class picker and command, including one-step undo after the edit.
    #[test]
    fn class_opacity_edits_keep_unmultiplied_rgb() {
        use egui::accesskit::{Action, ActionData, ActionRequest, Role};
        use egui_kittest::kittest::{NodeT, Queryable};
        use rustgis_engine::map::Rgba;

        for original in [Rgba::new(255, 0, 0, 0), Rgba::new(47, 162, 214, 1)] {
            let mut app = RustGisApp::with_sample();
            app.run("layer.symbology", json!({"layer": "Countries", "type": "graduated", "field": "pop_est", "classes": 1})).unwrap();
            app.run("layer.classStyle", json!({"layer": "Countries", "index": 0, "color": original.hex8()})).unwrap();
            let id = layer(&app, "Countries");
            let mut h = egui_kittest::Harness::builder().with_size(vec2(600.0, 600.0)).with_max_steps(64).build_ui_state(
                move |ui, app: &mut RustGisApp| {
                    finish_class_color_drag(app, ui);
                    let layer = app.session.map().layer(id).unwrap().clone();
                    legend_list(app, ui, &layer, &Tokens::light());
                },
                app,
            );
            h.run_steps(3);
            let fill =
                |h: &egui_kittest::Harness<'_, RustGisApp>| h.state().session.map().layer(id).unwrap().renderer.legend()[0].symbol.fill;
            assert_eq!(fill(&h), original, "displaying the picker must not change the class");
            h.get_by_role(Role::ColorWell).click();
            h.run_steps(3);
            for alpha in [255, 0, 128] {
                let before = h.state().session.undo_depth();
                let previous = fill(&h);
                // The popup's RGBA editors are its four spin buttons, with alpha last.
                let (node, tree) = h.query_all_by_role(Role::SpinButton).last().unwrap().accesskit_node().locate();
                h.event(egui::Event::AccessKitActionRequest(ActionRequest {
                    action: Action::SetValue,
                    target_tree: tree,
                    target_node: node,
                    data: Some(ActionData::NumericValue(if previous.a() == 0 { f64::from(alpha) / 255.0 } else { f64::from(alpha) })),
                }));
                h.run_steps(3);
                assert_eq!(fill(&h), original.with_alpha(alpha), "opacity edits must retain the original RGB");
                assert_eq!(h.state().session.undo_depth(), before + 1);
                h.state_mut().run("edit.undo", Value::Null).unwrap();
                assert_eq!(fill(&h), previous, "one undo restores the previous colour and opacity");
                h.state_mut().run("edit.redo", Value::Null).unwrap();
                assert_eq!(fill(&h), original.with_alpha(alpha));
                // Closing and reopening must reconstruct the same hidden colour, even at zero.
                h.key_press(egui::Key::Escape);
                h.run_steps(3);
                h.get_by_role(Role::ColorWell).click();
                h.run_steps(3);
            }
        }
    }

    /// Numeric RGB edits at alpha zero keep the newly selected colour, not transparent black
    /// or the previous RGB. Reopening and raising opacity must reveal that selection.
    #[test]
    fn transparent_class_numeric_rgb_edits_survive_reopening() {
        use egui::accesskit::{Action, ActionData, ActionRequest, Role};
        use egui_kittest::kittest::{NodeT, Queryable};
        use rustgis_engine::map::Rgba;

        let mut app = RustGisApp::with_sample();
        app.run("layer.symbology", json!({"layer": "Countries", "type": "graduated", "field": "pop_est", "classes": 1})).unwrap();
        app.run("layer.classStyle", json!({"layer": "Countries", "index": 0, "color": "#FF000000"})).unwrap();
        let id = layer(&app, "Countries");
        let mut h = egui_kittest::Harness::builder().with_size(vec2(600.0, 600.0)).with_max_steps(64).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                finish_class_color_drag(app, ui);
                let layer = app.session.map().layer(id).unwrap().clone();
                legend_list(app, ui, &layer, &Tokens::light());
            },
            app,
        );
        h.run_steps(3);
        let fill = |h: &egui_kittest::Harness<'_, RustGisApp>| h.state().session.map().layer(id).unwrap().renderer.legend()[0].symbol.fill;
        let inactive_theme = match h.ctx.theme() {
            egui::Theme::Light => egui::Theme::Dark,
            egui::Theme::Dark => egui::Theme::Light,
        };
        h.ctx.style_mut_of(inactive_theme, |style| style.visuals.numeric_color_space = egui::style::NumericColorSpace::Linear);
        let styles = [egui::Theme::Light, egui::Theme::Dark].map(|theme| h.ctx.style_of(theme).visuals.numeric_color_space);
        h.get_by_role(Role::ColorWell).click();
        h.run_steps(3);
        // Fully transparent fills use linear 0..1 numeric controls: green 1 makes yellow.
        let edit = |h: &mut egui_kittest::Harness<'_, RustGisApp>, channel, value| {
            let (node, tree) = h.query_all_by_role(Role::SpinButton).nth(channel).unwrap().accesskit_node().locate();
            h.event(egui::Event::AccessKitActionRequest(ActionRequest {
                action: Action::SetValue,
                target_tree: tree,
                target_node: node,
                data: Some(ActionData::NumericValue(value)),
            }));
            h.run_steps(3);
        };
        edit(&mut h, 1, 1.0);
        assert_eq!(fill(&h), Rgba::new(255, 255, 0, 0));
        // Even the popup's mode toggle must not leak changes into either theme.
        h.get_by_role_and_label(Role::Button, "F").click();
        h.run_steps(3);
        assert_eq!([egui::Theme::Light, egui::Theme::Dark].map(|theme| h.ctx.style_of(theme).visuals.numeric_color_space), styles);
        h.state_mut().run("edit.undo", Value::Null).unwrap();
        assert_eq!(fill(&h), Rgba::new(255, 0, 0, 0));
        h.state_mut().run("edit.redo", Value::Null).unwrap();
        assert_eq!(fill(&h), Rgba::new(255, 255, 0, 0));
        h.key_press(egui::Key::Escape);
        h.run_steps(3);
        h.get_by_role(Role::ColorWell).click();
        h.run_steps(3);
        edit(&mut h, 3, 1.0);
        assert_eq!(fill(&h), Rgba::new(255, 255, 0, 255));
        // Opaque colours retain the ordinary byte-based editor.
        edit(&mut h, 1, 64.0);
        assert_eq!(fill(&h), Rgba::new(255, 64, 0, 255));
        assert_eq!([egui::Theme::Light, egui::Theme::Dark].map(|theme| h.ctx.style_of(theme).visuals.numeric_color_space), styles);
    }

    /// The layer menu selects its layer as it opens, not on every frame it stays open: each
    /// command moves the session's revision, which drew the whole map again every frame.
    #[test]
    fn an_open_layer_menu_leaves_the_map_alone() {
        let app = RustGisApp::with_sample();
        let layer = app.session.map().layers.iter().find(|l| l.name == "Cities").cloned().unwrap();
        let mut h = egui_kittest::Harness::builder().build_ui_state(|ui, app: &mut RustGisApp| layer_menu(app, ui, &layer, true), app);
        h.step();
        assert_eq!(h.state().session.active_layer, Some(layer.id));
        let rev = h.state().session.revision;
        for _ in 0..5 {
            h.step();
        }
        assert_eq!(h.state().session.revision, rev, "the menu ran a command on every frame");
    }
}
