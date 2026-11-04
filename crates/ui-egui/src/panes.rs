//! Docked panes: Layers and Browser on the left; Geoprocessing, Symbology, History and Details
//! on the right.

use std::collections::HashMap;

use egui::{Align2, CornerRadius, Rect, Sense, Stroke, Ui, pos2, vec2};
use rustgis_engine::Session;
use rustgis_engine::analysis::{ParamKind, tools};
use rustgis_engine::data::GeometryType;
use rustgis_engine::map::raster::{Normalizer, RasterLegend, RasterStyle};
use rustgis_engine::map::symbology::{
    ClassRef, ClassStyle, Dash, LineCap, LineJoin, LineMark, LineMarkers, LineStroke, MAX_LABEL, MAX_LINE_MARKER_SIZE, MAX_MARKER_INTERVAL,
    MAX_STROKE_OFFSET, MAX_STROKE_WIDTH, MAX_STROKES, MAX_UNIQUE, MIN_MARKER_INTERVAL, OTHER_VALUES, Placements, normalize_label,
};
use rustgis_engine::map::tiles::{TileKind, TileLayer};
use rustgis_engine::map::{
    ClassMethod, Drawn, Glyph, Layer, MAX_GROUP_DEPTH, Map, Marker, Outline, Ramp, RampGroup, RasterDefaults, RasterRenderer, Renderer,
    Resampling, Stretch, Symbol,
};
use serde_json::{Value, json};

use crate::theme::{Tokens, c32, regular, rgba, semibold};
use crate::widgets::Dragged;
use crate::{LayersView, LeftPane, RightPane, RustGisApp, icons};

/// The map's narrowest width beside a right pane dragged wide.
const MIN_MAP_WIDTH: f32 = 320.0;
/// How wide a right pane can always be dragged, as far as the window has room, even where that
/// leaves the map less than `MIN_MAP_WIDTH`.
const RIGHT_PANE_MAX: f32 = 640.0;

pub fn show(app: &mut RustGisApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let frame = egui::Frame::NONE.fill(t.pane).inner_margin(egui::Margin::ZERO);
    app.ui.pane_rects = [None; 2];
    if let Some(left) = app.ui.left {
        let pane =
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
        app.ui.pane_rects[0] = Some(pane.response.rect);
    }
    // A point picked for the Time Series pane is taken only while that pane shows: with it
    // closed or another in view, the pick ends rather than waiting with a stale click.
    if app.ui.right != Some(RightPane::TimeSeries) && app.workflow.pick.as_ref().is_some_and(|p| p.purpose == "ts_point") {
        app.workflow.pick = None;
    }
    // A failed renderer switch waits for a field only while the Symbology pane shows its layer.
    if app.ui.right == Some(RightPane::Symbology) {
        let _ = pending_symbology(app, ui.ctx());
    } else {
        clear_pending_symbology(ui.ctx());
    }
    // The pane in view always has its tab, however it was set.
    if let Some(right) = app.ui.right
        && !app.ui.right_open.contains(&right)
    {
        app.ui.right_open.push(right);
    }
    if let Some(right) = app.ui.right {
        // The pane can take all the room right of the left pane but the map's minimum, so a
        // notebook or a table can be read side by side with the map (#1056). Where that is less
        // than `RIGHT_PANE_MAX`, the pane keeps that much (as far as it fits), as it always could.
        let room = ui.available_width();
        let max = (room - MIN_MAP_WIDTH).max(RIGHT_PANE_MAX.min(room)).max(260.0);
        // The Web Data pane's searches and lists want room, and the Notebook opens on half of
        // it (at least 320 where the room allows), beside the map: each keeps its own width.
        let (id, width) = match right {
            RightPane::WebData => ("right_pane_web", 440.0),
            RightPane::Notebook => ("right_pane_notebook", (room / 2.0).max(320.0).min(max)),
            RightPane::DiagnosticLog => ("right_pane_diagnostic_log", 480.0),
            _ => ("right_pane", 320.0),
        };
        let pane = egui::Panel::right(id).resizable(true).default_size(width).size_range(260.0..=max).frame(frame).show(ui, |ui| {
            // The pane is as wide as the user made it. Content wider than that is clipped, never
            // reported to the panel: egui would keep the panel's edge where the content ends, off
            // the window, and give the map the room it no longer covers (#307).
            let rect = ui.available_rect_before_wrap();
            let mut body = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(*ui.layout()));
            right_pane(app, &mut body, right, &t);
            ui.advance_cursor_after_rect(rect);
        });
        app.ui.pane_rects[1] = Some(pane.response.rect);
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
        RightPane::DiagnosticLog => ("Diagnostics", "ui.pane.diagnosticLog"),
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
    if right == RightPane::DiagnosticLog {
        crate::diagnosticlog::show(app, ui, t);
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
            RightPane::Assistant | RightPane::Sql | RightPane::Script | RightPane::DiagnosticLog => {}
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
const WEB_DATA: [(&str, &str); 16] = [
    ("opendata", "Search Open Data"),
    ("stac", "STAC Search"),
    ("dataCubes", "Data Cubes"),
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
        "dataCubes" => crate::cubesui::pane(app, ui),
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
        let resp = ui.interact(tr, egui::Id::unique(("right_pane_tab", pane as u8)), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, on, name));
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
        let x_resp = ui.interact(cr, egui::Id::unique(("right_pane_close", pane as u8)), Sense::click());
        let tip = crate::i18n::fmt("Close {name}", &[("name", name)]);
        x_resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &tip));
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
    let all_resp = ui.interact(all, egui::Id::unique("right_pane_close_all"), Sense::click());
    all_resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, tl!("Close all panes")));
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
        let more = ui.interact(mr, egui::Id::unique("right_pane_more"), Sense::click());
        let tip = crate::i18n::fmt("{n} more panes", &[("n", &hidden.len().to_string())]);
        more.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &tip));
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
    let resp = ui.interact(cr, egui::Id::unique(("pane_close", salt)), Sense::click());
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
        let mut resp = ui.interact(tr, egui::Id::unique(("left_pane_tab", i)), Sense::click());
        // Named and marked open for screen readers (and scripted clicks), icons or not.
        resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, on, name));
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

/// Paints a character marker's character centred on `at`, `px` tall, in its installed family (the
/// interface font until egui has it, or where the machine lacks it).
fn glyph(p: &egui::Painter, at: egui::Pos2, g: Glyph, px: f32, c: egui::Color32) {
    use rustgis_engine::render::families;
    let font = families::find_ready(g.font.as_str());
    let family = crate::labelfonts::family(p.ctx(), font, false, false).map_or(egui::FontFamily::Proportional, |(f, _)| f);
    p.text(at, Align2::CENTER_CENTER, g.ch, egui::FontId::new(px, family), c);
}

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
                // An outline-only character shows in its outline's colour.
                Marker::Char(g) => {
                    let col = if s.fill.a() == 0 && s.stroke_width > 0.0 { s.stroke } else { s.fill };
                    glyph(p, c, g, (s.size * 1.4).clamp(8.0, 18.0), c32(col));
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
        let active = app.session.active_layer;
        // The active layer last, so it stays active if the filter shows it.
        let mut keep: Vec<u32> =
            picked.iter().copied().filter(|id| shown(&app.session, *id) == Some(true) && Some(*id) != active).collect();
        keep.extend(active.filter(|id| shown(&app.session, *id) == Some(true)));
        if keep.len() < picked.len() && !keep.is_empty() {
            let _ = app.run("layer.select", json!({"layer": keep}));
        }
    }
    // A tile layer picked since the filter hid the active layer (a row's click, a command or a
    // script) is picked instead: the hidden layer isn't made active again once it shows.
    if app.session.selected_tile().is_some() {
        app.ui.layer_filter_hid = None;
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
    // So does a tile layer picked: one the filter hides isn't picked any more (for good, as the
    // pick isn't kept to come back).
    if let Some(i) = app.session.selected_tile()
        && app.session.map().tiles.get(i).is_some_and(|t| !filter_shows(&q, &t.name))
        && let Some(t) = app.session.map_mut().tiles.get_mut(i)
    {
        t.selected = false;
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
        let header = ui.interact(row, egui::Id::unique("layers_map_header"), Sense::click());
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
                // Web tile layers list at their place in the drawing order of all the map's layers
                // (#808): over the layer (or group) they're drawn over, or under them all.
                let (slots, order): (Vec<Option<u32>>, Vec<Drawn>) = {
                    let m = app.session.map();
                    (m.tile_slots(), m.stack())
                };
                // The tile layers' rows over each layer's (or group's) first row, by that row: a
                // layer dropped there goes above them, as the line drawn says.
                let mut blocks: Vec<(usize, Rect)> = Vec::new();
                let mut tile_rows: Vec<(usize, Rect)> = Vec::new();
                let mut listed_tiles = 0;
                let mut tiles_at = |app: &mut RustGisApp, ui: &mut Ui, over: Option<u32>, tile_rows: &mut Vec<(usize, Rect)>| {
                    for (i, tile) in tiles.iter().enumerate().filter(|(i, x)| slots.get(*i) == Some(&over) && shows(&x.name)) {
                        let at = order.iter().position(|d| *d == Drawn::Tile(i));
                        let moves = [at.is_some_and(|a| a > 0), at.is_some_and(|a| a + 1 < order.len())];
                        let rect = tile_row(app, ui, i, moves, reorder, tile, t);
                        tile_rows.push((i, rect));
                        listed_tiles += 1;
                    }
                };
                // A layer group lists at its topmost layer (its layers, its subgroups' too, are
                // kept together), a group in another inside that one; while filtering, when its
                // name, a group's above it or one of its layers' or subgroups' names matches
                // (judged once per group, not once per layer).
                let mut listed: HashMap<u32, bool> = HashMap::new();
                for layer in &layers {
                    let tiles_before = tile_rows.len();
                    tiles_at(app, ui, Some(layer.id), &mut tile_rows);
                    let above = tile_rows.get(tiles_before..).and_then(|r| r.iter().map(|(_, r)| *r).reduce(|a, b| a.union(b)));
                    let rows_before = rows.len();
                    let top = app.session.map().groups_of(layer.id).last().map(|g| g.id);
                    if let Some(group) = groups.iter().find(|g| g.children.iter().any(|c| c.layer == layer.id)) {
                        if shows(&layer.name) && shown.insert(group.id) {
                            service_group_row(app, ui, group, &layers, scale, t);
                        }
                    } else if let Some(group) = top.and_then(|g| layer_groups.iter().find(|x| x.id == g)) {
                        let listed = *listed.entry(group.id).or_insert_with(|| group_listed(app.session.map(), group.id, &shows));
                        if listed && shown.insert(group.id) {
                            layer_group_row(app, ui, group, &layers, scale, reorder, &mut rows, 1, t);
                        }
                    } else if shows(&layer.name) {
                        let rect = layer_row(app, ui, layer, scale, reorder, t);
                        rows.push(DropRow { id: layer.id, rect, group: None, header: false });
                    }
                    if let Some(above) = above.filter(|_| rows.len() > rows_before) {
                        blocks.push((rows_before, above));
                    }
                }
                for group in &groups {
                    if (!filtering || (group.children.is_empty() && shows(&group.name))) && shown.insert(group.id) {
                        service_group_row(app, ui, group, &layers, scale, t);
                    }
                }
                // Top-level groups with no layers yet, at the bottom (one in another lists at
                // the end of that one).
                for group in &layer_groups {
                    let top = app.session.map().group_chain(group.id).last().map(|g| g.id) == Some(group.id);
                    if top && group_listed(app.session.map(), group.id, &shows) && shown.insert(group.id) {
                        layer_group_row(app, ui, group, &layers, scale, reorder, &mut rows, 1, t);
                    }
                }
                // Those under all the map's layers, over the basemap.
                tiles_at(app, ui, None, &mut tile_rows);
                if reorder {
                    let mut drop_rows = rows.clone();
                    for (k, above) in blocks {
                        if let Some(r) = drop_rows.get_mut(k) {
                            r.rect = r.rect.union(above);
                        }
                    }
                    layer_drop(app, ui, &drop_rows, t);
                    tile_drop(app, ui, &rows, &tile_rows, t);
                }
                rows.iter().filter(|r| r.group.is_none() && !r.header).count() + shown.len() + listed_tiles
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
/// Groups go into a group too (#836); a copied group's subgroups count with it.
fn paste_label(app: &RustGisApp) -> &'static str {
    let Some(c) = &app.session.layer_clipboard else { return tl!("Paste Layer") };
    // Layers copied apart from any copied group.
    let loose = c.layers.iter().any(|l| !c.groups.iter().any(|g| g.children.contains(&l.id)));
    // The copied groups not inside another copied one.
    let tops = c.groups.iter().filter(|g| !g.parent.is_some_and(|p| c.groups.iter().any(|x| x.id == p))).count();
    match (tops, loose) {
        (1, false) => tl!("Paste Group"),
        (n, false) if n > 1 => tl!("Paste Groups"),
        _ if c.layers.len() > 1 => tl!("Paste Layers"),
        _ => tl!("Paste Layer"),
    }
}

/// A button pasting the copied layers (#491) on top of the map in view, off while none are;
/// returns whether it was clicked.
fn paste_layers_button(app: &mut RustGisApp, ui: &mut Ui) -> bool {
    let clicked = ui.add_enabled(app.session.layer_clipboard.is_some(), egui::Button::new(paste_label(app))).clicked();
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
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, on, hover));
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
    // A service's downloaded raster keeps the address it came from as its dataset source, not
    // the file name it was read under: group it by that address, as a feature layer is. A layer
    // with no raster keeps its dataset source as it is (a database's name, say), and a raster
    // read from an address is grouped by that address rather than by a dataset's other one.
    let source = l.dataset.source.as_deref();
    let raster = l.raster.as_ref().and_then(|r| r.path.as_deref().or(r.file_name.as_deref()));
    let address = source.filter(|s| is_address(s) && !raster.is_some_and(is_address));
    address.or(raster).or(source).filter(|s| !s.trim().is_empty()).map(source_name)
}

/// Whether a source names a web address (`file://` paths are files, not addresses).
fn is_address(s: &str) -> bool {
    s.split_once("://").is_some_and(|(scheme, _)| !scheme.eq_ignore_ascii_case("file"))
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
                Item::Tile(i, tile) => {
                    tile_row(app, ui, i, [false, false], false, tile, t);
                }
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
    let cb_resp = ui.interact(hit(cb), egui::Id::unique(("layer_selectable", l.id)), Sense::click());
    cb_resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, true, l.selectable, tl!("Selectable")));
    visibility_box(ui, cb, l.selectable, t);
    // Only the map's interactive selection reads it: the tools that name a layer (Select By
    // Attributes, Select Layer By Location, the attribute table) select in any layer.
    if cb_resp.on_hover_text(tl!("Selectable on the map: the Select tool picks its features")).clicked() {
        let _ = app.run("layer.selectable", json!({"layer": l.id}));
    }
    if count > 0 {
        let cr = Rect::from_center_size(pos2(r.max.x - 8.0 - 11.0, cy), vec2(22.0, 22.0));
        let clear = ui.interact(cr, egui::Id::unique(("layer_selection_clear", l.id)), Sense::click());
        clear.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, tl!("Clear selection")));
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
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &group.name));
        if resp.hovered() {
            ui.painter().rect_filled(r.shrink2(vec2(4.0, 1.0)), CornerRadius::same(5), t.hover);
        }
        let cy = r.min.y + 13.0;
        let ar = Rect::from_center_size(pos2(r.min.x + 16.0, cy), vec2(14.0, 14.0));
        icons::paint(ui.painter(), ar, if group.expanded { "chevron_down" } else { "chevron_right" }, t.text_dim, t.accent);
        // `interact` takes the id as it is (not under the `push_id`): each group's own.
        let arrow = ui.interact(hit(ar), egui::Id::unique(("group_expand", group.id)), Sense::click());
        let visible = !children.is_empty() && children.iter().all(|l| l.visible);
        let cb = Rect::from_center_size(pos2(r.min.x + 33.0, cy), vec2(14.0, 14.0));
        let cb_resp = ui.interact(hit(cb), egui::Id::unique(("group_visible", group.id)), Sense::click());
        cb_resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, true, visible, tl!("Show or hide")));
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
/// and right-click renames it, sets its transparency, indents, outdents, ungroups or removes it.
/// Under it, indented, its layers and subgroups (#836) in drawing order; `depth` is its level (1
/// at the top). With `reorder` the row drags the whole group, and it and its layers' rows join
/// `rows`, where layers drop (see [`layer_drop`]).
#[allow(clippy::too_many_arguments)]
fn layer_group_row(
    app: &mut RustGisApp,
    ui: &mut Ui,
    group: &rustgis_engine::map::LayerGroup,
    layers: &[Layer],
    scale: f64,
    reorder: bool,
    rows: &mut Vec<DropRow>,
    depth: usize,
    t: &Tokens,
) {
    let q = filter_query(&app.ui.layer_filter);
    let filtering = !q.is_empty();
    ui.push_id(("layer_group", group.id), |ui| {
        let name = row_name(ui, group.name.clone(), semibold(12.5), ui.available_width() - NAME_X - 8.0);
        let sense = if reorder { Sense::click_and_drag() } else { Sense::click() };
        let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), name.row_h), sense);
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &group.name));
        if reorder && resp.drag_started_by(egui::PointerButton::Primary) {
            egui::DragAndDrop::set_payload(ui.ctx(), DraggedLayer(group.id));
        }
        let dragging = egui::DragAndDrop::payload::<DraggedLayer>(ui.ctx()).is_some_and(|d| d.0 == group.id);
        rows.push(DropRow { id: group.id, rect: r, group: group.parent, header: true });
        if resp.hovered() || dragging {
            ui.painter().rect_filled(r.shrink2(vec2(4.0, 1.0)), CornerRadius::same(5), t.hover);
        }
        let cy = r.min.y + 13.0;
        let ar = Rect::from_center_size(pos2(r.min.x + 16.0, cy), vec2(14.0, 14.0));
        icons::paint(ui.painter(), ar, if group.expanded { "chevron_down" } else { "chevron_right" }, t.text_dim, t.accent);
        let arrow = ui.interact(hit(ar), egui::Id::unique(("layer_group_expand", group.id)), Sense::click());
        let cb = Rect::from_center_size(pos2(r.min.x + 33.0, cy), vec2(14.0, 14.0));
        let cb_resp = ui.interact(hit(cb), egui::Id::unique(("layer_group_visible", group.id)), Sense::click());
        cb_resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, true, group.visible, tl!("Show or hide")));
        visibility_box(ui, cb, group.visible, t);
        icons::paint(ui.painter(), Rect::from_center_size(pos2(r.min.x + 57.0, cy), vec2(18.0, 16.0)), "contents", t.icon, t.accent);
        let elided = name.elided;
        // Dimmed when it, or a group holding it, is hidden or out of its range.
        let draws = app.session.map().group_chain(group.id).iter().all(|g| g.visible && g.in_scale_range(scale));
        name.paint(ui, r, r.min.x + NAME_X, if draws { t.text } else { t.text_disabled });
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
                let input_label = ui.label(tl!("Transparency"));
                let r = ui
                    .add(egui::Slider::new(&mut tr, 0.0..=100.0).update_while_editing(false).suffix(" %").integer())
                    .labelled_by(input_label.id);
                if r.drag_stopped() || (crate::widgets::changed(&r) && !r.dragged()) {
                    let _ = app.run("group.set", json!({"group": group.id, "transparency": tr}));
                }
            });
            if (group.min_scale.is_some() || group.max_scale.is_some()) && ui.button(tl!("Clear Visibility Range")).clicked() {
                let _ = app.run("group.scaleRange", json!({"group": group.id}));
                ui.close();
            }
            ui.separator();
            // Not while the list is filtered: the group would move among hidden rows.
            let tip = tl!("Into the group just above it, as its last item");
            if ui.add_enabled(reorder, egui::Button::new(tl!("Indent"))).on_hover_text(tip).clicked() {
                let _ = app.run("group.indent", json!({"group": group.id}));
                ui.close();
            }
            let tip = tl!("Out of the group it is in, just below that group");
            if ui.add_enabled(reorder && group.parent.is_some(), egui::Button::new(tl!("Outdent"))).on_hover_text(tip).clicked() {
                let _ = app.run("group.outdent", json!({"group": group.id}));
                ui.close();
            }
            ui.separator();
            if ui.button(tl!("Copy Group")).clicked() {
                let _ = app.run("layer.copy", json!({"group": group.id}));
                ui.close();
            }
            if ui.add_enabled(app.session.layer_clipboard.is_some(), egui::Button::new(paste_label(app))).clicked() {
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
            let items = app.session.map().outline(Some(group.id));
            // A name matching here or above lists all that is inside.
            let all = app.session.map().group_chain(group.id).iter().any(|g| filter_shows(&q, &g.name));
            let shows = |name: &str| filter_shows(&q, name);
            ui.indent("children", |ui| {
                for item in items {
                    match item {
                        Outline::Layer(id) => {
                            let Some(layer) = layers.iter().find(|l| l.id == id) else { continue };
                            if all || shows(&layer.name) {
                                let rect = layer_row(app, ui, layer, scale, reorder, t);
                                rows.push(DropRow { id, rect, group: Some(group.id), header: false });
                            }
                        }
                        Outline::Group(sub) => {
                            let Some(sub) = app.session.map().group(sub).cloned() else { continue };
                            if depth < MAX_GROUP_DEPTH && (all || group_listed(app.session.map(), sub.id, &shows)) {
                                layer_group_row(app, ui, &sub, layers, scale, reorder, rows, depth + 1, t);
                            }
                        }
                    }
                }
            });
        }
    });
}

/// Whether the drawing order view lists a layer group under a filter (`shows` tells the names
/// it matches; unfiltered, all): its name, a group's holding it, or one of its layers' or
/// subgroups' names.
fn group_listed(m: &Map, group: u32, shows: &dyn Fn(&str) -> bool) -> bool {
    m.group_chain(group).iter().any(|g| shows(&g.name))
        || m.group_layers(group).iter().filter_map(|id| m.layer(*id)).any(|l| shows(&l.name))
        || m.subgroups(group).iter().filter_map(|g| m.group(*g)).any(|g| shows(&g.name))
}

/// A group's name with the names of the groups holding it, outermost first: "Base map ›
/// Hydrography" (#836).
fn group_path(m: &Map, group: u32) -> String {
    let names: Vec<&str> = m.group_chain(group).iter().rev().map(|g| g.name.as_str()).collect();
    names.join(" › ")
}

/// The layer a row drag carries (egui's drag-and-drop payload).
#[derive(Clone, Copy)]
struct DraggedLayer(u32);

/// A web tile layer row being dragged (its index among the map's tile layers).
struct DraggedTile(usize);

/// A row of the drawing order view a dragged layer can drop on: a layer's or a layer group's
/// `header`, with the `group` it is in (a layer's own group; a header's, the group holding that
/// one).
#[derive(Clone, Copy)]
struct DropRow {
    id: u32,
    rect: Rect,
    group: Option<u32>,
    header: bool,
}

/// Dragging a layer or layer group row over the list: marks where it would drop and, on release,
/// puts it there in one undo step. Dropped above a row, it goes in the group that row is in
/// (none at the top level), just above it; dropped onto a group's row, it goes in that group at
/// its end nearest where it was (`layer.group`, `group.setParent`). Released anywhere but over
/// the rows, the drag does nothing.
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
    // Where it is: a layer's group, or the group holding a group.
    let current = if moving_group { m.group(dragged.0).and_then(|g| g.parent) } else { m.group_of(dragged.0).map(|g| g.id) };
    // A dragged group with its subgroups, which it can't go into, and the layers moving with it.
    let mut subtree = vec![dragged.0];
    let moving = if moving_group {
        subtree.extend(m.subgroups(dragged.0));
        m.group_layers(dragged.0)
    } else {
        vec![dragged.0]
    };
    // The topmost layer of a group, the moving ones left out. When they were all it had, the
    // next layer below them, if it is still in `into` (else none: the end of `into`).
    let top_of = |g: u32, into: Option<u32>| {
        let members = m.group_layers(g);
        let start = m.layers.iter().position(|l| members.contains(&l.id))?;
        m.layers.iter().skip(start).map(|l| l.id).find(|id| !moving.contains(id)).filter(|id| into.is_none_or(|p| m.layer_in(*id, p)))
    };
    // Onto the middle of another group's row: into that group, at its end nearest the layer or
    // group; onto the row of the group it is in, or of itself or one of its subgroups, nothing.
    if let Some(h) = rows
        .iter()
        .find(|r| r.header && pointer.y > r.rect.min.y + r.rect.height() / 4.0 && pointer.y < r.rect.max.y - r.rect.height() / 4.0)
    {
        if Some(h.id) == current || subtree.contains(&h.id) {
            return;
        }
        ui.painter().rect_stroke(
            h.rect.shrink2(vec2(4.0, 1.0)),
            CornerRadius::same(5),
            Stroke::new(2.0, t.accent),
            egui::StrokeKind::Inside,
        );
        if released {
            let _ = if moving_group {
                app.run("group.setParent", json!({"group": dragged.0, "parent": h.id}))
            } else {
                app.run("layer.group", json!({"layer": dragged.0, "group": h.id}))
            };
        }
        return;
    }
    // The entire block, including its expanded legend, has balanced above/below halves.
    let gap = rows.iter().position(|r| pointer.y < r.rect.center().y).unwrap_or(rows.len());
    let below = rows.get(gap);
    // Above a row is in the group that row is in; at the end, in none.
    let into = below.and_then(|r| r.group);
    // The layer the drop goes above (a group's row stands for its topmost layer); none at the end.
    let before = below.and_then(|r| if r.header { top_of(r.id, into) } else { Some(r.id) });
    if moving_group {
        // A group's own rows run from its header to its subgroups' last layer; next to them it
        // stays put.
        let own = |r: &DropRow| if r.header { subtree.contains(&r.id) } else { r.group.is_some_and(|g| subtree.contains(&g)) };
        let end = rows.iter().rposition(own).unwrap_or(from);
        if (gap >= from && gap <= end + 1) || into.is_some_and(|g| subtree.contains(&g)) {
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
        // Indented as the row it goes above.
        let x = match (into, below) {
            (Some(_), Some(b)) => b.rect.min.x,
            _ => first.rect.min.x,
        };
        ui.painter().line_segment([pos2(x + 8.0, y), pos2(r.max.x - 8.0, y)], Stroke::new(2.0, t.accent));
    }
    if !released {
        return;
    }
    let _ = if moving_group {
        app.run("group.setParent", json!({"group": dragged.0, "parent": into, "before": before}))
    } else {
        app.run("layer.group", json!({"layer": dragged.0, "group": into, "before": before}))
    };
}

/// Dragging a web tile layer's row over the list: marks where it would drop and, on release,
/// puts it there in the drawing order of all the map's layers (`map.setTiles` `to`). It never
/// goes into a group: a group's layers count as one row, its header's.
fn tile_drop(app: &mut RustGisApp, ui: &Ui, rows: &[DropRow], tile_rows: &[(usize, Rect)], t: &Tokens) {
    let Some(dragged) = egui::DragAndDrop::payload::<DraggedTile>(ui.ctx()) else { return };
    let view = ui.clip_rect();
    let Some(pointer) = ui.ctx().pointer_interact_pos().filter(|p| view.contains(*p)) else { return };
    let m = app.session.map();
    let order = m.stack();
    let Some(from) = order.iter().position(|d| *d == Drawn::Tile(dragged.0)) else { return };
    // Each top-level row with its place in the drawing order: a layer's, a group's topmost
    // layer's (its subgroups' rows count with it), a tile layer's.
    let layer_at = |id: u32| m.layer_index(id).and_then(|i| order.iter().position(|d| *d == Drawn::Layer(i)));
    let top_of = |g: u32| m.group_layers(g).first().copied();
    let mut places: Vec<(usize, Rect)> = rows
        .iter()
        .filter(|r| r.group.is_none())
        .filter_map(|r| layer_at(if r.header { top_of(r.id)? } else { r.id }).map(|at| (at, r.rect)))
        .chain(tile_rows.iter().filter_map(|(i, rect)| order.iter().position(|d| *d == Drawn::Tile(*i)).map(|at| (at, *rect))))
        .collect();
    places.sort_by(|a, b| a.1.min.y.total_cmp(&b.1.min.y));
    let (Some(first), Some(last)) = (places.first().map(|p| p.1), places.last().map(|p| p.1)) else { return };
    // A group's layers (expanded under its header) count with the group: the pointer over them
    // drops below the group.
    let bottom = rows.iter().map(|r| r.rect.max.y).fold(last.max.y, f32::max);
    if pointer.y < first.min.y || pointer.y > bottom {
        return;
    }
    let gap = places.iter().position(|(_, r)| pointer.y < r.center().y).unwrap_or(places.len());
    // The place among the other layers: that of the row below the gap, one less past itself.
    let to = places.get(gap).map_or(order.len() - 1, |(at, _)| if *at > from { at - 1 } else { *at });
    if to == from {
        return;
    }
    let (y, r) = match places.get(gap) {
        Some((_, r)) => (r.min.y, *r),
        None => (bottom, last),
    };
    ui.painter().line_segment([pos2(first.min.x + 8.0, y), pos2(r.max.x - 8.0, y)], Stroke::new(2.0, t.accent));
    if ui.input(|i| i.pointer.any_released()) {
        let _ = app.run("map.setTiles", json!({"index": dragged.0, "to": to}));
    }
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

/// A web tile layer's row (index `i` of `count`, top first as the map lists them): a click picks
/// it (`map.selectTiles`: the Selected in Layers pop-up mode identifies it); its checkbox
/// shows or hides it; right-click zooms to it (known bounds, an archive or GeoTIFF), sets its
/// opacity, moves it up or down (not with `reorder` off: the list is filtered), or removes it.
fn tile_row(app: &mut RustGisApp, ui: &mut Ui, i: usize, moves: [bool; 2], reorder: bool, tile: &TileLayer, t: &Tokens) -> Rect {
    let shown = if tile.overlay { format!("{} · {}", tile.name, tl!("overlay")) } else { tile.name.clone() };
    let name = row_name(ui, shown.clone(), regular(12.5), ui.available_width() - NAME_X - 8.0);
    let sense = if reorder { Sense::click_and_drag() } else { Sense::click() };
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), name.row_h), sense);
    // Named for screen readers as a layer's row is.
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &tile.name));
    // Dragged to another place in the drawing order, as a layer's row is.
    if reorder && resp.drag_started_by(egui::PointerButton::Primary) {
        egui::DragAndDrop::set_payload(ui.ctx(), DraggedTile(i));
    }
    let dragging = egui::DragAndDrop::payload::<DraggedTile>(ui.ctx()).is_some_and(|d| d.0 == i);
    // Picked, it looks as an active layer's row does.
    let picked = app.session.selected_tile() == Some(i);
    if picked {
        ui.painter().rect_filled(r.shrink2(vec2(4.0, 1.0)), CornerRadius::same(5), t.selection);
    } else if resp.hovered() || dragging {
        ui.painter().rect_filled(r.shrink2(vec2(4.0, 1.0)), CornerRadius::same(5), t.hover);
    }
    active_outline(ui, r, picked, t);
    // Lined up with the layer rows' arrows, checkboxes, icons and names.
    let cy = r.min.y + 13.0;
    // A WMS or WMTS layer's legend, a vector archive's source layers, open under it, as a feature
    // layer's classes do.
    let has_legend = !tile.legends.is_empty();
    let sources = rustgis_engine::render::tile_source_layers(tile).filter(|n| !n.is_empty());
    let expands = has_legend || sources.is_some();
    let ar = Rect::from_center_size(pos2(r.min.x + 16.0, cy), vec2(14.0, 14.0));
    if expands {
        icons::paint(ui.painter(), ar, if tile.expanded { "chevron_down" } else { "chevron_right" }, t.text_dim, t.accent);
    }
    let ar_resp = expands.then(|| {
        let r = ui.interact(hit(ar), egui::Id::unique(("tile_expand", i)), Sense::click());
        let what = match (tile.expanded, has_legend, sources.is_some()) {
            (true, true, true) => tl!("Hide legend and source layers"),
            (false, true, true) => tl!("Show legend and source layers"),
            (true, true, false) => tl!("Hide legend"),
            (false, true, false) => tl!("Show legend"),
            (true, false, _) => tl!("Hide source layers"),
            (false, false, _) => tl!("Show source layers"),
        };
        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, what));
        r
    });
    let mut x = r.min.x + 26.0;
    let cb = Rect::from_center_size(pos2(x + 7.0, cy), vec2(14.0, 14.0));
    let cb_resp = ui.interact(hit(cb), egui::Id::unique(("tile_vis", i)), Sense::click()).on_hover_text(tl!("Show or hide"));
    visibility_box(ui, cb, tile.visible, t);
    x += 22.0;
    icons::paint(ui.painter(), Rect::from_center_size(pos2(x + 9.0, cy), vec2(18.0, 16.0)), "basemap", t.icon, t.accent);
    x += 24.0;
    // Unavailable archives are red; otherwise grey when hidden, transparent, or outside the
    // slider's step (the box still shows the choice to show it).
    let color = if tile.error.is_some() {
        t.red
    } else if tile.draws() && tile.opacity > 0.0 {
        t.text
    } else {
        t.text_disabled
    };
    let elided = name.elided;
    name.paint(ui, r, x, color);
    let mut action: Option<(&str, Value)> = None;
    if cb_resp.clicked() {
        action = Some(("map.setTiles", json!({"index": i, "visible": !tile.visible})));
    } else if ar_resp.is_some_and(|a| a.clicked()) {
        action = Some(("map.expandTiles", json!({"index": i})));
    } else if resp.clicked() && !picked {
        action = Some(("map.selectTiles", json!({"index": i})));
    }
    let mut hover = if elided { format!("{shown}\n{}", tile.url) } else { tile.url.clone() };
    if let Some(error) = &tile.error {
        hover.push('\n');
        hover.push_str(error);
    }
    let resp = resp.on_hover_text(hover);
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
            let input_label = ui.label(tl!("Opacity"));
            let mut o = tile.opacity * 100.0;
            let slider = ui.add(egui::Slider::new(&mut o, 0.0..=100.0).update_while_editing(false).suffix("%")).labelled_by(input_label.id);
            if slider.drag_stopped() || (crate::widgets::changed(&slider) && !slider.dragged()) {
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
        // A layer its service draws on request: one picture of the view, or tiles (#1162).
        if tile.on_request() {
            let shown = if tile.single_image {
                tl!("Single image").to_string()
            } else if tile.sized_tiles() {
                crate::i18n::fmt("Tiles of {size} px", &[("size", &tile.tile_size.to_string())])
            } else {
                tl!("Tiles").to_string()
            };
            ui.menu_button(crate::i18n::fmt("Draw as: {how}", &[("how", &shown)]), |ui| {
                if ui.radio(tile.single_image, tl!("Single image")).on_hover_text(tl!("One picture of the view: symbols and labels aren't cut at tile edges, and the service is asked once a view. Drawn in tiles on the globe, on a tilted map, or in a projection the service doesn't offer.")).clicked() {
                    action = Some(("map.setTiles", json!({"index": i, "singleImage": true})));
                    ui.close();
                }
                if tile.sized_tiles() {
                    for side in [256u32, 512, 1024] {
                        let label = crate::i18n::fmt("Tiles of {size} px", &[("size", &side.to_string())]);
                        if ui.radio(!tile.single_image && tile.tile_size == side, label).clicked() {
                            action = Some(("map.setTiles", json!({"index": i, "singleImage": false, "tileSize": side})));
                            ui.close();
                        }
                    }
                } else if ui.radio(!tile.single_image, tl!("Tiles")).clicked() {
                    action = Some(("map.setTiles", json!({"index": i, "singleImage": false})));
                    ui.close();
                }
            })
            .response
            .on_hover_text(tl!("Whether the service is asked for one picture of the view or for tiles."));
        }
        ui.separator();
        if ui.add_enabled(reorder && moves[0], egui::Button::new(tl!("Move up"))).clicked() {
            action = Some(("map.setTiles", json!({"index": i, "move": "up"})));
            ui.close();
        }
        if ui.add_enabled(reorder && moves[1], egui::Button::new(tl!("Move down"))).clicked() {
            action = Some(("map.setTiles", json!({"index": i, "move": "down"})));
            ui.close();
        }
        if tile.archive_file().is_some() && ui.button(tl!("Repair Data Source…")).clicked() {
            action = Some(("ui.dialog", json!({"name": "repairTiles", "index": i})));
            ui.close();
        }
        if ui.button(tl!("Remove")).clicked() {
            action = Some(("map.removeTiles", json!({"index": i})));
            ui.close();
        }
    });
    if has_legend && tile.expanded {
        crate::tilelegend::show(app, ui, i, tile, t);
    }
    if let Some(names) = sources.filter(|_| tile.expanded)
        && let Some(a) = source_layer_rows(ui, i, tile, &names, t)
    {
        action = Some(a);
    }
    if let Some((cmd, p)) = action {
        let _ = app.run(cmd, p);
    }
    r
}

/// A vector archive's source layers under its row (tile layer `i`), each with its checkbox; a
/// click switches one on or off (`map.setTiles` with the new `hiddenLayers`).
fn source_layer_rows(ui: &mut Ui, i: usize, tile: &TileLayer, names: &[String], t: &Tokens) -> Option<(&'static str, Value)> {
    let mut action = None;
    for (k, name) in names.iter().enumerate() {
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
        let shown = !tile.hidden_layers.contains(name);
        let cb = Rect::from_center_size(pos2(r.min.x + crate::tilelegend::INDENT + 7.0, r.center().y), vec2(14.0, 14.0));
        let cb_resp = ui.interact(hit(cb), egui::Id::unique(("tile_source", i, k)), Sense::click());
        cb_resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, true, shown, name));
        visibility_box(ui, cb, shown, t);
        let color = if shown && tile.draws() { t.text } else { t.text_disabled };
        let x = cb.max.x + 8.0;
        let clip = Rect::from_min_max(pos2(x, r.min.y), pos2(r.max.x - 8.0, r.max.y));
        ui.painter().with_clip_rect(clip).text(pos2(x, r.center().y), Align2::LEFT_CENTER, name, regular(11.5), color);
        if cb_resp.clicked() {
            let hidden: Vec<&str> = if shown {
                tile.hidden_layers.iter().map(String::as_str).chain([name.as_str()]).collect()
            } else {
                tile.hidden_layers.iter().map(String::as_str).filter(|h| h != name).collect()
            };
            action = Some(("map.setTiles", json!({"index": i, "hiddenLayers": hidden})));
        }
    }
    action
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
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &l.name));
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
    let ar_resp = ui.interact(hit(ar), egui::Id::unique(("expand", l.id)), Sense::click());
    x += 16.0;
    // Visibility checkbox.
    let cb = Rect::from_center_size(pos2(x + 7.0, cy), vec2(14.0, 14.0));
    let cb_resp = ui.interact(hit(cb), egui::Id::unique(("vis", l.id)), Sense::click());
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
    // Not with Ctrl, Command or Shift held: that is picking layers, and would drop the others.
    if resp.double_clicked() && !ui.input(|i| i.modifiers.command || i.modifiers.shift) {
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
            None => crate::i18n::fmt(
                "The raster's data couldn't be read: {error}",
                &[("error", rl.error.as_deref().or(rl.path.as_deref()).unwrap_or(""))],
            ),
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
        // (By expression: the expression, cut short.)
        let field = match l.renderer.expression() {
            Some(e) if e.chars().count() > 60 => format!("{}…", e.chars().take(60).collect::<String>()),
            Some(e) => e.to_string(),
            None => l.renderer.field().unwrap_or("").to_string(),
        };
        ui.horizontal(|ui| {
            ui.add_space(74.0);
            ui.label(egui::RichText::new(field).size(11.0).color(t.text_dim));
        });
        for e in vector_legend.iter().take(40) {
            let (r, response) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
            response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, &e.label));
            let pr = Rect::from_center_size(pos2(r.min.x + 84.0, r.center().y), vec2(18.0, 14.0));
            patch(ui.painter(), pr, &e.symbol, l.geometry_type());
            ui.painter().text(pos2(r.min.x + 100.0, r.center().y), Align2::LEFT_CENTER, &e.label, regular(11.5), t.text);
        }
    }
    Rect::from_min_max(r.min, pos2(r.max.x, ui.min_rect().max.y.max(r.max.y)))
}

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
        let m = app.session.map();
        let groups: Vec<(u32, String)> = m.groups.iter().map(|g| (g.id, group_path(m, g.id))).collect();
        let grouped = picked.iter().any(|id| m.group_of(*id).is_some());
        // Layers all in one group make a group inside it (#836).
        let shared = picked
            .first()
            .and_then(|id| m.group_of(*id))
            .map(|g| g.id)
            .filter(|g| picked.iter().all(|id| m.group_of(*id).map(|x| x.id) == Some(*g)));
        ui.menu_button(tl!("Group"), |ui| {
            if ui.button(tl!("New Group")).clicked() {
                new_group(app, json!({"layers": picked, "parent": shared}));
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

/// A layer's right-click menu (its row in any Layers view). Move Up and Move Down only with
/// `reorder` (the drawing order view, unfiltered).
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
    let login = l.source.as_ref().and_then(|s| {
        let (kind, database) = rustgis_engine::connections::layer_database(s)?;
        let connections: Vec<_> = app.session.connections.matching_database(kind, database).into_iter().cloned().collect();
        let (_, key) = rustgis_engine::connections::database_key(database)?;
        if !s.login && !app.session.database_logins.requests(app.session.opened).any(|r| r.kind == kind && r.key == key) {
            return None;
        }
        Some((kind, key, connections))
    });
    if let Some((kind, key, connections)) = login {
        if connections.len() == 1 {
            let c = &connections[0];
            if ui.button(crate::i18n::fmt("Log In to {name}…", &[("name", &c.name)])).clicked() {
                crate::connectionui::log_in(app, &c.id);
                ui.close();
            }
        } else if connections.len() > 1 && ui.button(tl!("Choose Connection…")).clicked() {
            let opened = app.session.opened;
            crate::connectionui::choose(
                app,
                rustgis_engine::connections::LoginRequest {
                    reason: rustgis_engine::connections::LoginReason::Choose,
                    kind,
                    key,
                    connections: connections.iter().map(|c| c.id.clone()).collect(),
                    layers: vec![l.id],
                    opened,
                    prompted: true,
                },
            );
            ui.close();
        }
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
    // A layer's action, so with the layer's (#858); the Labeling pane keeps the class settings.
    if !l.is_raster()
        && ui.button(tl!("Convert To Annotation")).on_hover_text(tl!("Convert this layer's labels into an annotation layer.")).clicked()
    {
        if let Err(e) = app.run("labels.convert", json!({"layer": l.id})) {
            app.session.notify(e, true);
        }
        ui.close();
    }
    if l.is_streamed() && ui.button(tl!("Load All Features")).clicked() {
        let _ = app.run("layer.loadAll", json!({"layer": l.id}));
        ui.close();
    }
    if l.linked_file().is_some() && ui.button(tl!("Repair Data Source…")).clicked() {
        let _ = app.run("ui.dialog", json!({"name": "repairSource", "layer": l.id}));
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
    let tip = tl!("Into the group just above it, as its last item");
    if ui.add_enabled(reorder && !service_child, egui::Button::new(tl!("Indent"))).on_hover_text(tip).clicked() {
        let _ = app.run("layer.indent", json!({"layer": l.id}));
        ui.close();
    }
    let tip = tl!("Out of its group, just below that group");
    let grouped = app.session.map().group_of(l.id).is_some();
    if ui.add_enabled(reorder && grouped, egui::Button::new(tl!("Outdent"))).on_hover_text(tip).clicked() {
        let _ = app.run("layer.outdent", json!({"layer": l.id}));
        ui.close();
    }
    ui.separator();
    // Whole layers (#491): pasted on a layer, the copies go above it, or at the top of its group.
    if ui.button(tl!("Copy Layer")).clicked() {
        let _ = app.run("layer.copy", json!({"layer": l.id}));
        ui.close();
    }
    if ui.add_enabled(app.session.layer_clipboard.is_some(), egui::Button::new(paste_label(app))).clicked() {
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
    if !l.is_raster() && ui.button(tl!("Define Projection…")).clicked() {
        let _ = app.run("ui.dialog", json!({"name": "defineProjection"}));
        ui.close();
    }
    // Layer groups (#413): a service's collections stay in their service group.
    if !service_child {
        let m = app.session.map();
        let groups: Vec<(u32, String)> = m.groups.iter().map(|g| (g.id, group_path(m, g.id))).collect();
        let current = m.group_of(l.id).map(|g| g.id);
        ui.menu_button(tl!("Group"), |ui| {
            // In the layer's group, if it has one (#836).
            if ui.button(tl!("New Group")).clicked() {
                new_group(app, json!({"layers": [l.id], "parent": current}));
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
                        // Databases holding several layers, which ask which to add (#934).
                        "gpkg",
                        "duckdb",
                        "ddb",
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
                    let _ = app.run_by_user("project.open", json!({"path": path.display().to_string()}));
                    app.map.invalidate();
                } else if name.ends_with(".rgismap") || name.ends_with(".rgismpk") {
                    let _ = app.run("map.import", json!({"path": path.display().to_string()}));
                    app.map.invalidate();
                } else if name.ends_with(".rgislayout") {
                    let _ = app.run("layout.import", json!({"path": path.display().to_string()}));
                } else {
                    let _ = crate::layerchooser::add_file(app, &path.display().to_string());
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
    /// A built-in tool running on a thread of its own, likewise (the browser runs them at once).
    #[cfg(not(target_arch = "wasm32"))]
    pub tool_run: Option<ToolJob>,
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

/// A built-in tool running on a thread of its own.
#[cfg(not(target_arch = "wasm32"))]
pub struct ToolJob {
    pub id: u64,
    pub tool: &'static str,
    /// What the run made, once it has.
    pub slot: crate::Slot<rustgis_engine::cmd::gp::ToolDone>,
    /// Stops the run, and counts its progress.
    pub control: std::sync::Arc<rustgis_engine::analysis::control::Control>,
}

/// Starts a built-in tool on a thread of its own; [`poll_tool_run`] applies what it made.
#[cfg(not(target_arch = "wasm32"))]
fn start_tool_run(app: &mut RustGisApp, tool: &'static str, params: &Value, ctx: &egui::Context) {
    match rustgis_engine::cmd::gp::tool_start(&mut app.session, tool, params) {
        Ok(run) => {
            let slot = crate::Slot::default();
            // Cancel stops it, so it takes no work cap.
            let control = rustgis_engine::analysis::control::Control::stoppable();
            let (out, ctl, ctx) = (slot.clone(), control.clone(), ctx.clone());
            let id = run.id;
            let spawned = std::thread::Builder::new().name(tool.into()).spawn(move || {
                let done = run.run(&ctl);
                // Filled even when poisoned, or the pane would wait for it forever.
                *out.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(done);
                ctx.request_repaint();
            });
            match spawned {
                Ok(_) => {
                    app.gp.tool_run = Some(ToolJob { id, tool, slot, control });
                    app.gp.last = None;
                }
                Err(e) => {
                    rustgis_engine::cmd::gp::tool_abandon(&mut app.session, id);
                    app.gp.last = Some((false, vec![e.to_string()]));
                }
            }
        }
        Err(e) => app.gp.last = Some((false, vec![e])),
    }
}

/// Applies a built-in tool run that finished on its thread.
#[cfg(not(target_arch = "wasm32"))]
pub fn poll_tool_run(app: &mut RustGisApp, ctx: &egui::Context) {
    let Some(job) = &app.gp.tool_run else { return };
    // Its project was closed: stop it, as its result has nowhere to go, and say so.
    if !rustgis_engine::cmd::gp::tool_waiting(&app.session, job.id) {
        job.control.cancel();
        dropped(app, job.tool);
        app.gp.tool_run = None;
        return;
    }
    let Some(done) = take(&job.slot) else {
        // Its progress moves as it works.
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        return;
    };
    let (id, tool) = (job.id, job.tool);
    app.gp.tool_run = None;
    let r = rustgis_engine::cmd::gp::tool_complete(&mut app.session, id, done).and_then(|_| app.run("gp.finish", json!({"id": id})));
    finished(app, tool, r);
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
    // Its project was closed: stop it, as its result has nowhere to go, and say so.
    if !rustgis_engine::cmd::gp::plugin_waiting(&app.session, job.id) {
        job.control.cancel();
        dropped(app, job.tool);
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

/// Says that a tool's run was dropped: its project closed while it ran.
fn dropped(app: &mut RustGisApp, tool: &'static str) {
    let name = rustgis_engine::analysis::tool(tool).map(|t| t.name).unwrap_or(tool);
    app.session.notify(crate::i18n::fmt("{tool} was stopped: its project was closed while it ran.", &[("tool", tl_raw!(name))]), false);
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
        egui::ComboBox::from_id_salt(("gp_list", param))
            .selected_text(tl!("Add"))
            .width(ui.available_width() - 4.0)
            .show_ui(ui, |ui| {
                for o in left {
                    if ui.selectable_label(false, &o).clicked() {
                        names.push(o);
                        changed = true;
                    }
                }
            })
            .response
            .on_hover_text(tl!("Add"));
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
                (ParamKind::Fields { .. } | ParamKind::FieldStatistics { .. }, Some(v)) => v.to_string(),
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

/// Whether a tool's parameter of `kind` takes layer `l`, as the tool's form and the Script pane
/// offer layers: by its geometry, or raster or not; any for a `Layer`. False for a kind that isn't
/// a layer's.
pub(crate) fn takes_layer(kind: ParamKind, l: &rustgis_engine::map::Layer) -> bool {
    match kind {
        ParamKind::Layer => true,
        ParamKind::PolygonLayer => l.geometry_type() == GeometryType::Polygon,
        ParamKind::PointLayer => l.geometry_type() == GeometryType::Point,
        ParamKind::RasterLayer | ParamKind::RasterLayers => l.is_raster(),
        ParamKind::VectorLayer | ParamKind::VectorLayers => !l.is_raster(),
        _ => false,
    }
}

/// Whether a tool's parameter of `kind` takes field `i` of `d`: a numeric one for a
/// `NumericField`, one holding coordinates for a `CoordinateField`, one without a `,` or `;` for
/// `FieldNames` (runners split the text list at commas, some at semicolons too, so such a name
/// can't be one entry), any for a `Field`. False for a kind that isn't a field's.
pub(crate) fn takes_field(kind: ParamKind, d: &rustgis_engine::data::Dataset, i: usize) -> bool {
    match kind {
        ParamKind::Field | ParamKind::Fields { .. } => true,
        ParamKind::FieldNames => d.fields.get(i).is_some_and(|f| !f.name.contains([',', ';'])),
        ParamKind::NumericField => d.fields.get(i).is_some_and(|f| f.ty.is_numeric()),
        ParamKind::CoordinateField => rustgis_engine::analysis::holds_coordinates(d, i),
        _ => false,
    }
}

/// The tool's first layer parameter, whose fields its field parameters take.
pub(crate) fn first_layer(spec: &rustgis_engine::analysis::ToolSpec) -> Option<&rustgis_engine::analysis::ParamSpec> {
    spec.params
        .iter()
        .find(|p| matches!(p.kind, ParamKind::Layer | ParamKind::PolygonLayer | ParamKind::PointLayer | ParamKind::VectorLayer))
}

/// The most specific `<input>_…` prefix wins; otherwise use the first attribute input.
pub(crate) fn layer_for_param<'a>(
    spec: &'a rustgis_engine::analysis::ToolSpec,
    param_name: &str,
) -> Option<&'a rustgis_engine::analysis::ParamSpec> {
    spec.params
        .iter()
        .filter(|p| {
            matches!(p.kind, ParamKind::Layer | ParamKind::PolygonLayer | ParamKind::PointLayer | ParamKind::VectorLayer)
                && param_name.strip_prefix(p.name).is_some_and(|rest| rest.starts_with('_'))
        })
        .max_by_key(|p| p.name.len())
        .or_else(|| first_layer(spec))
}

/// Editable field names with suggestions: manifests may describe new columns, not just inputs.
/// A list appends names without replacing its earlier entries; manually typed syntax is retained.
/// `hint` shows in the empty box (the default a run uses).
pub(crate) fn field_suggestions<'a>(
    ui: &mut Ui,
    id: egui::Id,
    value: &mut String,
    label: &str,
    hint: &str,
    fields: impl Iterator<Item = &'a str>,
    multiple: bool,
) -> egui::Response {
    let mut picked = None;
    let mut response = ui
        .horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                egui::ComboBox::from_id_salt(id)
                    .selected_text(tl!("Fields"))
                    .width(70.0)
                    .show_ui(ui, |ui| {
                        for field in fields {
                            if ui.selectable_label(false, field).clicked() {
                                picked = Some(field);
                            }
                        }
                    })
                    .response
                    .on_hover_text(tl!("Choose a field"));
                ui.add(egui::TextEdit::singleline(&mut *value).hint_text(hint).desired_width(f32::INFINITY)).on_hover_text(label)
            })
            .inner
        })
        .inner;
    if let Some(field) = picked {
        if !multiple || value.trim().is_empty() {
            value.clear();
            value.push_str(field);
            response.mark_changed();
        } else if !value.split([',', ';']).any(|part| part.trim() == field) {
            let separator = if value.contains(';') { ';' } else { ',' };
            if !value.trim_end().ends_with([',', ';']) {
                value.push(separator);
            }
            value.push_str(field);
            response.mark_changed();
        }
    }
    response
}

/// The layer or table of name `name`, as a tool finds its input: a layer by its name, else a
/// table (by its name, else its id).
pub(crate) fn input_dataset<'a>(session: &'a Session, name: &str) -> Option<&'a rustgis_engine::data::Dataset> {
    session.map().layer_by_name(name).map(|l| &l.dataset).or_else(|| input_table(session, name).map(|t| &t.dataset))
}

/// The standalone table of name `name`, as a tool finds one: by its name, else its id.
pub(crate) fn input_table<'a>(session: &'a Session, name: &str) -> Option<&'a rustgis_engine::project::Table> {
    let tables = &session.project.tables;
    rustgis_engine::project::find_table(tables, name).and_then(|i| tables.get(i))
}

fn geoprocessing(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    let Some(spec) = app.gp.tool.and_then(rustgis_engine::analysis::tool) else {
        // Tool finder, under the tool running, if one is.
        let running = app.gp.web_run.is_some() || app.gp.plugin_run.is_some();
        #[cfg(not(target_arch = "wasm32"))]
        let running = running || app.gp.tool_run.is_some();
        if running {
            run_status(app, ui, t);
            ui.add_space(8.0);
        }
        crate::widgets::search_field(ui, &mut app.gp.search, tl!("Find Tools"));
        ui.separator();
        ui.add_space(4.0);
        let q = app.gp.search.to_lowercase();
        let matches = |x: &rustgis_engine::analysis::ToolSpec| {
            q.is_empty() || x.name.to_lowercase().contains(&q) || x.summary.to_lowercase().contains(&q) || x.id.to_lowercase().contains(&q)
        };
        let row = |app: &mut RustGisApp, ui: &mut Ui, tool: &'static rustgis_engine::analysis::ToolSpec| {
            let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 42.0), Sense::click());
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, tl!(tool.name)));
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
    let layer_names = |kind: ParamKind| -> Vec<String> {
        app.session.map().layers.iter().filter(|l| takes_layer(kind, l)).map(|l| l.name.clone()).collect()
    };
    let plugin_tool = spec.id.starts_with(rustgis_engine::plugins::PREFIX);
    #[cfg(not(target_arch = "wasm32"))]
    let first_layer = first_layer(spec).map(|p| p.name);
    // The file parameter whose Browse was pressed: picked once the form is drawn.
    let mut browse_for = None;
    // A parameter that doesn't apply to the options picked (Spatial Join's search distance with
    // Intersects) is hidden, and left out of the run.
    let applies = |app: &RustGisApp, name: &str| {
        let value = |n: &str| {
            let text = app.gp.values.get(n).cloned().unwrap_or_default();
            if spec.params.iter().any(|p| p.name == n && crate::fieldlists::input(p.kind).is_some()) {
                let value = crate::fieldlists::form_value(&text);
                value.as_str().map(str::to_string).unwrap_or_else(|| value.to_string())
            } else {
                text
            }
        };
        rustgis_engine::analysis::param_applies(spec.id, name, &value)
    };
    for p in spec.params {
        if !applies(app, p.name) {
            continue;
        }
        let label = ui.label(egui::RichText::new(format!("{}{}", tl!(p.label), if p.required { " *" } else { "" })).size(12.0));
        if !p.help.is_empty() {
            label.on_hover_text(p.help);
        }
        let mut cur = app.gp.values.get(p.name).cloned().unwrap_or_default();
        if let Some(input) = crate::fieldlists::input(p.kind) {
            let name = app.gp.values.get(input).map(String::as_str).unwrap_or_default();
            let dataset = match app.gp.tables.get(input) {
                Some(id) => app.session.project.table(*id).map(|t| &t.dataset),
                None => input_dataset(&app.session, name),
            };
            let value = crate::fieldlists::form_value(&cur);
            if let Some(value) = ui
                .push_id(("gp_list", p.name), |ui| crate::fieldlists::show(ui, p.kind, &value, dataset.map(|d| d.fields.as_slice())))
                .inner
                .value
            {
                app.gp.values.insert(p.name, value.to_string());
            }
            ui.add_space(4.0);
            continue;
        }
        let options: Option<Vec<String>> = match p.kind {
            // Layers by their names, then tables: the value is the name shown, and a table
            // picked is kept by its id beside it.
            ParamKind::Layer => {
                let picked = app.gp.tables.get(p.name).copied();
                let shown = if cur.is_empty() { "—" } else { cur.as_str() };
                egui::ComboBox::from_id_salt(("gp", p.name))
                    .selected_text(shown)
                    .width(ui.available_width() - 4.0)
                    .show_ui(ui, |ui| {
                        for name in layer_names(ParamKind::Layer) {
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
                    })
                    .response
                    .on_hover_text(tl!(p.label));
                ui.add_space(4.0);
                continue;
            }
            ParamKind::PolygonLayer | ParamKind::PointLayer => Some(layer_names(p.kind)),
            ParamKind::Field | ParamKind::FieldNames | ParamKind::NumericField | ParamKind::CoordinateField => {
                let target_param = layer_for_param(spec, p.name).map(|p| p.name);
                let lname = target_param.and_then(|n| app.gp.values.get(n)).cloned().unwrap_or_default();
                let table = target_param.and_then(|n| app.gp.tables.get(n)).copied();
                // The table picked; else the layer or table of that name, as the tool finds it.
                let ds = match table {
                    Some(id) => app.session.project.table(id).map(|t| &t.dataset),
                    None => input_dataset(&app.session, &lname),
                };
                let fields = |d: &rustgis_engine::data::Dataset| -> Vec<String> {
                    d.fields.iter().enumerate().filter(|(i, _)| takes_field(p.kind, d, *i)).map(|(_, f)| f.name.clone()).collect()
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
                if spec.id.starts_with("geolibre:") || p.kind == ParamKind::FieldNames {
                    if field_suggestions(
                        ui,
                        egui::Id::unique(("gp", p.name)),
                        &mut cur,
                        tl!(p.label),
                        "",
                        v.iter().map(String::as_str),
                        p.kind == ParamKind::FieldNames,
                    )
                    .changed()
                    {
                        app.gp.values.insert(p.name, cur);
                    }
                    ui.add_space(4.0);
                    continue;
                }
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
                let mut v = layer_names(p.kind);
                if !p.required {
                    v.insert(0, String::new());
                }
                Some(v)
            }
            ParamKind::RasterLayers | ParamKind::VectorLayers => {
                let offered = layer_names(p.kind);
                layer_list(&mut app.gp, ui, p.name, &cur, offered);
                ui.add_space(4.0);
                continue;
            }
            ParamKind::Bool => {
                let mut on = cur == "true";
                if ui.checkbox(&mut on, "").on_hover_text(tl!(p.label)).changed() {
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
                egui::ComboBox::from_id_salt(("gp", p.name))
                    .selected_text(shown(&cur))
                    .width(ui.available_width() - 4.0)
                    .show_ui(ui, |ui| {
                        for o in opts {
                            if ui.selectable_label(o == cur, shown(&o)).clicked() {
                                app.gp.values.insert(p.name, o);
                            }
                        }
                    })
                    .response
                    .on_hover_text(tl!(p.label));
            }
            None => {
                let mut v = cur;
                let hint = match p.kind {
                    ParamKind::Number => tl!("number"),
                    ParamKind::Path => tl!("file path"),
                    ParamKind::SaveFile { .. } if cfg!(target_arch = "wasm32") => tl!("File name"),
                    ParamKind::SaveFile { .. } => tl!("file path"),
                    _ => "",
                };
                let mut browse = false;
                // Browse picks a file to read: on the desktop for any file input (GeoLibre's too);
                // in the browser, where the file itself goes with the run, only for plugins.
                let browsable = (p.kind == ParamKind::Path && (app.services.open_async.is_none() || plugin_tool))
                    || (matches!(p.kind, ParamKind::SaveFile { .. }) && !cfg!(target_arch = "wasm32"));
                let changed = if browsable {
                    // One row: the path box takes what the button leaves.
                    let row = vec2(ui.available_width(), ui.spacing().interact_size.y);
                    ui.allocate_ui_with_layout(row, egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        browse =
                            ui.button(if matches!(p.kind, ParamKind::SaveFile { .. }) { tl!("Save As") } else { tl!("Browse") }).clicked();
                        ui.add(egui::TextEdit::singleline(&mut v).hint_text(hint).desired_width(f32::INFINITY))
                            .on_hover_text(tl!(p.label))
                            .changed()
                    })
                    .inner
                } else {
                    ui.add(egui::TextEdit::singleline(&mut v).hint_text(hint).desired_width(f32::INFINITY))
                        .on_hover_text(tl!(p.label))
                        .changed()
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
    if let Some(param) = browse_for
        && let Some(p) = spec.params.iter().find(|p| p.name == param)
    {
        if let ParamKind::SaveFile { ext } = p.kind {
            #[cfg(not(target_arch = "wasm32"))]
            {
                let name = app.gp.values.get(param).filter(|v| !v.trim().is_empty()).cloned().unwrap_or_else(|| {
                    format!("{}.{}", first_layer.and_then(|n| app.gp.values.get(n)).map(String::as_str).unwrap_or("output"), ext)
                });
                if let Some(path) = crate::dialogs::pick_save(&name, &[ext], tl!(p.label)) {
                    app.gp.values.insert(param, path);
                }
            }
            #[cfg(target_arch = "wasm32")]
            let _ = ext;
        } else {
            browse_file(app, spec.id, param);
        }
    }
    ui.add_space(8.0);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
        // One browser run at a time, and one background run.
        let idle = app.gp.web_run.is_none() && app.gp.plugin_run.is_none();
        #[cfg(not(target_arch = "wasm32"))]
        let idle = idle && app.gp.tool_run.is_none();
        if crate::widgets::primary_button(ui, tl!("Run"), idle, vec2(96.0, 30.0)).clicked() {
            // A blank output opens Save As before any work starts. Cancelling writes nothing.
            #[cfg(not(target_arch = "wasm32"))]
            for p in spec.params {
                if let ParamKind::SaveFile { ext } = p.kind
                    && app.gp.values.get(p.name).is_none_or(|v| v.trim().is_empty())
                {
                    let input = first_layer.and_then(|n| app.gp.values.get(n)).map(String::as_str).unwrap_or("output");
                    let Some(path) = crate::dialogs::pick_save(&format!("{input}.{ext}"), &[ext], tl!(p.label)) else { return };
                    app.gp.values.insert(p.name, path);
                }
            }
            let mut params = serde_json::Map::new();
            // The tools of a suite or a plugin read their inputs as text; a plugin's is handed
            // every parameter, so it gets only those it declares.
            let builtin = rustgis_engine::analysis::external_source(spec).is_none();
            let plugin = spec.id.starts_with(rustgis_engine::plugins::PREFIX);
            for p in spec.params.iter().filter(|p| applies(app, p.name)) {
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
                        (ParamKind::Fields { .. } | ParamKind::FieldStatistics { .. }, _) => crate::fieldlists::form_value(v),
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
            } else if !cfg!(target_arch = "wasm32") {
                // A built-in tool too, with its progress and Cancel; the browser has no threads.
                #[cfg(not(target_arch = "wasm32"))]
                start_tool_run(app, spec.id, &params, ui.ctx());
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
    run_status(app, ui, t);
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

/// The tool running, if one is: in the tool's form and in the finder, so that it can be cancelled
/// from either.
fn run_status(app: &RustGisApp, ui: &mut Ui, t: &Tokens) {
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
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(job) = &app.gp.tool_run {
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
        // How far its current step has come, when it counts its work.
        if let Some((done, total)) = job.control.progress() {
            ui.add(egui::ProgressBar::new(done as f32 / total.max(1) as f32).show_percentage().desired_height(14.0));
        }
        if cancel {
            job.control.cancel();
        }
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
        "intersect" => "intersect",
        "identity" => "identity",
        "union" => "union",
        "symmetricalDifference" => "symDiff",
        "update" => "updatePolygons",
        "split" => "split",
        "near" => "near",
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
    paint_ramp(ui.painter(), r, ramp);
    ui.painter().rect_stroke(
        r,
        CornerRadius::same(2),
        Stroke::new(if selected { 2.0 } else { 1.0 }, if selected { t.accent } else { t.border_strong }),
        egui::StrokeKind::Outside,
    );
    resp
}

/// A ramp's colours across `r`, low to high.
fn paint_ramp(painter: &egui::Painter, r: Rect, ramp: &Ramp) {
    let n = 24;
    for i in 0..n {
        // A qualitative ramp's `at` picks the nearest of all its swatches (tab20 shows all 20).
        let c = ramp.at(i as f64 / (n - 1) as f64);
        let x0 = r.min.x + r.width() * i as f32 / n as f32;
        let x1 = r.min.x + r.width() * (i + 1) as f32 / n as f32 + 0.5;
        painter.rect_filled(Rect::from_min_max(pos2(x0, r.min.y), pos2(x1, r.max.y)), 0.0, c32(c));
    }
}

const PENDING_SYMBOLOGY: &str = "symbology_pending_field";

/// A renderer switch that failed, kept so the pane can offer a field to retry with. It belongs to
/// one layer of one map in one project, and to the renderer the layer had when it failed.
#[derive(Clone)]
struct PendingSymbology {
    project: u64,
    map: usize,
    layer: u32,
    kind: &'static str,
    error: std::sync::Arc<str>,
    renderer: std::sync::Arc<Renderer>,
}

fn clear_pending_symbology(ctx: &egui::Context) {
    ctx.data_mut(|data| data.remove::<PendingSymbology>(egui::Id::unique(PENDING_SYMBOLOGY)));
}

/// The pending retry, unless another project, map or layer is in view or the layer's renderer
/// changed since the switch failed (an edit from the pane, the ribbon, an agent or undo
/// supersedes it); a stale one is dropped.
fn pending_symbology(app: &RustGisApp, ctx: &egui::Context) -> Option<PendingSymbology> {
    let pending = ctx.data(|data| data.get_temp::<PendingSymbology>(egui::Id::unique(PENDING_SYMBOLOGY)))?;
    let current = pending.project == app.session.opened
        && pending.map == app.session.project.active_map
        && Some(pending.layer) == app.session.active_layer
        && app.session.map().layer(pending.layer).is_some_and(|l| l.renderer == *pending.renderer);
    if !current {
        clear_pending_symbology(ctx);
    }
    current.then_some(pending)
}

/// Shows the active layer's renderer controls, including No Fill for polygon layers.
fn symbology(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    if app.ui.symbology_pending.as_ref().is_some_and(|pending| !pending.is_current(&app.session)) {
        app.ui.symbology_pending = None;
    }
    let Some(l) = app.session.active_layer.and_then(|id| app.session.map().layer(id)).cloned() else {
        ui.label(egui::RichText::new(tl!("Select a layer in the Layers pane.")).color(t.text_dim));
        return;
    };
    if l.is_raster() {
        raster_symbology(app, ui, &l, t);
        return;
    }
    let g = l.geometry_type();
    let numeric: Vec<String> = l.dataset.fields.iter().filter(|f| f.ty.is_numeric()).map(|f| f.name.clone()).collect();
    let all: Vec<String> = l.dataset.fields.iter().map(|f| f.name.clone()).collect();
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
            let sel = if app.ui.symbology_pending.is_some() { *code == "unique" } else { cur_kind == *label };
            let (r, resp) = ui.allocate_exact_size(vec2(68.0, 56.0), Sense::click());
            resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, sel, tl!(*label)));
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
                if *code == "unique" && !all.is_empty() {
                    let field = l.dataset.fields.iter().find(|f| !f.ty.is_numeric()).or_else(|| l.dataset.fields.first());
                    if let Some(field) = field {
                        app.ui.symbology_pending = Some(crate::PendingUniqueSymbology::new(&app.session, &l, field.name.clone()));
                        clear_pending_symbology(ui.ctx());
                    }
                    continue;
                }
                match app.run("layer.symbology", json!({"type": code})) {
                    Err(error) => {
                        let pending = PendingSymbology {
                            project: app.session.opened,
                            map: app.session.project.active_map,
                            layer: l.id,
                            kind: code,
                            error: error.into(),
                            renderer: std::sync::Arc::new(l.renderer.clone()),
                        };
                        ui.data_mut(|data| data.insert_temp(egui::Id::unique(PENDING_SYMBOLOGY), pending));
                    }
                    Ok(_) => {
                        clear_pending_symbology(ui.ctx());
                        app.ui.symbology_pending = None;
                    }
                }
            }
        }
    });
    ui.add_space(8.0);
    if let Some(mut pending) = app.ui.symbology_pending.clone() {
        let pending_field = pending.field.clone();
        let mut selected = if all.contains(&pending_field) { pending_field.clone() } else { all.first().cloned().unwrap_or_default() };
        ui.horizontal(|ui| {
            let input_label = ui.label(tl!("Field"));
            egui::ComboBox::from_id_salt("sym_pending_field")
                .selected_text(&selected)
                .width(200.0)
                .show_ui(ui, |ui| {
                    for field in &all {
                        ui.selectable_value(&mut selected, field.clone(), field);
                    }
                })
                .response
                .labelled_by(input_label.id);
            let applied = ui.button(tl!("Apply")).clicked()
                && match app.run("layer.symbology", json!({"type": "unique", "field": selected.clone()})) {
                    Ok(_) => true,
                    Err(error) => {
                        app.session.notify(error, true);
                        false
                    }
                };
            if applied {
                app.ui.symbology_pending = None;
            } else if selected != pending_field {
                pending.field = selected;
                app.ui.symbology_pending = Some(pending);
            }
        });
        ui.add_space(6.0);
    }
    if l.has_geometry(GeometryType::Polygon) && !l.graphics {
        if ui.button(tl!("No Fill")).on_hover_text(tl!("Draw only polygon outlines, keeping their colors and widths.")).clicked() {
            let _ = app.run("layer.noFill", Value::Null);
        }
        ui.add_space(6.0);
    }
    if let Some(pending) = pending_symbology(app, ui.ctx()) {
        ui.label(egui::RichText::new(pending.error.as_ref()).color(ui.visuals().error_fg_color));
        let fields = if pending.kind == "unique" { &all } else { &numeric };
        if !fields.is_empty() {
            ui.horizontal(|ui| {
                let input_label = ui.label(tl!("Choose a field to apply this renderer:"));
                egui::ComboBox::from_id_salt("symbology_pending_field_choice")
                    .selected_text(tl!("Select a field"))
                    .show_ui(ui, |ui| {
                        for field in fields {
                            if ui.selectable_label(false, field).clicked() {
                                match app.run("layer.symbology", json!({"type": pending.kind, "field": field})) {
                                    Ok(_) => clear_pending_symbology(ui.ctx()),
                                    Err(error) => {
                                        let pending = PendingSymbology { error: error.into(), ..pending.clone() };
                                        ui.data_mut(|data| data.insert_temp(egui::Id::unique(PENDING_SYMBOLOGY), pending));
                                    }
                                }
                            }
                        }
                    })
                    .response
                    .labelled_by(input_label.id);
            });
        }
        ui.add_space(6.0);
    }
    match &l.renderer {
        Renderer::Single(s) => symbol_editor(app, ui, *s, g, t),
        Renderer::Unique { field, classes, ramp, expression, .. } => {
            field_combo(app, ui, "Field", if expression.is_some() { tl!("(expression)") } else { field.as_str() }, &all, "unique");
            // Or by an expression's values: blank goes back to the field.
            let id = egui::Id::unique(("unique_expression", crate::labelpane::layer_key(app, &l)));
            if let Some(text) = expression_box(ui, id, tl!("Expression"), expression.as_deref(), &l.dataset, t) {
                let e = if text.is_empty() { Value::Null } else { json!(text) };
                let _ = app.run("layer.symbology", json!({"type": "unique", "expression": e, "ramp": ramp.name()}));
            }
            ramp_picker(app, ui, ramp, t);
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(crate::i18n::fmt("{n} classes", &[("n", &classes.len().to_string())])).size(11.5).color(t.text_dim),
            );
            legend_list(app, ui, &l, t);
        }
        Renderer::Graduated { field, breaks, ramp, method, min, .. } => {
            // Re-classing keeps the classes asked for, not the fewer some methods found.
            let asked = l.renderer.classes_asked();
            field_combo(app, ui, "Field", field, &numeric, "graduated");
            ui.horizontal(|ui| {
                let input_label = ui.label(tl!("Method"));
                egui::ComboBox::from_id_salt("sym_method").selected_text(tl!(method.label())).width(180.0).show_ui(ui, |ui| {
                    for m in methods_by_name() {
                        if ui.selectable_label(m == *method, tl!(m.label())).clicked() {
                            let _ = app.run("layer.symbology", json!({"type": "graduated", "field": field, "method": format!("{m:?}"), "classes": asked, "ramp": ramp.name()}));
                        }
                    }
                }).response.labelled_by(input_label.id);
            });
            if *method == ClassMethod::Manual {
                manual_breaks(app, ui, field, breaks, *min, ramp);
            } else {
                ui.horizontal(|ui| {
                    let input_label = ui.label(tl!("Classes"));
                    let mut k = asked;
                    let resp = ui.add(egui::DragValue::new(&mut k).update_while_editing(false).range(1..=12)).labelled_by(input_label.id);
                    if crate::widgets::changed(&resp) {
                        crate::widgets::one_step_per_press(app, &resp);
                        let _ = app.run(
                            "layer.symbology",
                            json!({"type": "graduated", "field": field, "method": format!("{method:?}"), "classes": k, "ramp": ramp.name()}),
                        );
                    }
                });
            }
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
    if g == GeometryType::Point && !l.graphics {
        ui.add_space(8.0);
        marker_rotation_row(app, ui, &l);
    }
    if matches!(g, GeometryType::Polyline | GeometryType::Point) && !l.graphics {
        ui.add_space(8.0);
        line_strokes_editor(app, ui, &l, t);
    }
    if l.has_geometry(GeometryType::Polygon) && !l.graphics {
        ui.add_space(8.0);
        crate::fill_editor::fills_editor(app, ui, &l, t);
    }
    if !l.graphics {
        ui.add_space(8.0);
        symbol_by_editor(app, ui, &l, t);
    }
    ui.add_space(10.0);
    ui.separator();
    ui.label(egui::RichText::new(tl!("Layer")).size(11.5).color(t.text_dim));
    let mut tr = l.transparency;
    ui.horizontal(|ui| {
        let input_label = ui.label(tl!("Transparency"));
        ui.spacing_mut().slider_width = 150.0;
        let r =
            ui.add(egui::Slider::new(&mut tr, 0.0..=100.0).update_while_editing(false).suffix(" %").integer()).labelled_by(input_label.id);
        transparency_drag(app, ui, &r, &l, tr);
    });
    if !l.graphics {
        reference_scale_row(app, ui, &l);
    }
    ui.add_space(6.0);
    if ui.button(tl!("Import QGIS Style…")).on_hover_text(tl!("Style this layer as a QGIS layer style file (.qml) does")).clicked() {
        let _ = app.run("ui.openItem", json!({"kind": "qgisStyle"}));
    }
}

/// The scales the Reference scale picker offers, besides none and the map's own.
const REFERENCE_SCALE_CHOICES: [f64; 8] = [5_000.0, 10_000.0, 25_000.0, 50_000.0, 100_000.0, 200_000.0, 500_000.0, 1_000_000.0];

/// The Symbology pane's Reference scale: the scale a layer's symbol and label sizes are set
/// for, so they grow and shrink with the map (`layer.referenceScale`).
fn reference_scale_row(app: &mut RustGisApp, ui: &mut Ui, l: &Layer) {
    let current = app.session.map().scale().round();
    ui.horizontal(|ui| {
        ui.label(tl!("Reference scale")).on_hover_text(tl!(
            "The scale the layer's symbol and label sizes are set for. At other scales they grow and shrink with the map, as on paper."
        ));
        egui::ComboBox::from_id_salt("reference_scale")
            .selected_text(crate::ribbon::scale_label(l.reference_scale))
            .width(130.0)
            .show_ui(ui, |ui| {
                let mut pick = |ui: &mut Ui, label: String, scale: Value, on: bool| {
                    if ui.selectable_label(on, label).clicked() {
                        let _ = app.run("layer.referenceScale", json!({"scale": scale}));
                    }
                };
                pick(ui, crate::ribbon::scale_label(None), Value::Null, l.reference_scale.is_none());
                for c in REFERENCE_SCALE_CHOICES {
                    pick(ui, crate::ribbon::scale_label(Some(c)), json!(c), l.reference_scale == Some(c));
                }
                let label = crate::i18n::fmt("Current scale ({scale})", &[("scale", &crate::ribbon::scale_label(Some(current)))]);
                pick(ui, label, json!("current"), l.reference_scale == Some(current) && !REFERENCE_SCALE_CHOICES.contains(&current));
            })
            .response
            .on_hover_text(tl!("Reference scale"));
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

/// The classification methods in alphabetical order of their names in the interface language.
fn methods_by_name() -> Vec<ClassMethod> {
    let mut m = ClassMethod::ALL.to_vec();
    m.sort_by_cached_key(|m| crate::i18n::t_raw(m.label()).to_lowercase());
    m
}

fn field_combo(app: &mut RustGisApp, ui: &mut Ui, label: &str, cur: &str, fields: &[String], kind: &str) {
    ui.horizontal(|ui| {
        let input_label = ui.label(tl!(label));
        egui::ComboBox::from_id_salt(("sym_field", kind))
            .selected_text(cur)
            .width(200.0)
            .show_ui(ui, |ui| {
                for f in fields {
                    if ui.selectable_label(f == cur, f).clicked() {
                        let _ = app.run("layer.symbology", json!({"type": kind, "field": f}));
                    }
                }
            })
            .response
            .labelled_by(input_label.id);
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
        let ramps: Vec<Ramp> = match g {
            RampGroup::Custom => custom.clone(),
            RampGroup::Matplotlib => Ramp::matplotlib_library(),
            _ => Ramp::ALL.into_iter().filter(|r| r.group() == g).collect(),
        };
        if ramps.is_empty() {
            continue;
        }
        egui::CollapsingHeader::new(egui::RichText::new(tl!(g.label())).size(11.5).color(t.text_dim))
            .id_salt(("ramp_group", g.label()))
            .default_open(cur.group() == g)
            .show(ui, |ui| if g == RampGroup::Matplotlib { ramp_list(app, ui, &ramps, cur, t) } else { grid(app, ui, g.label(), ramps) });
    }
    if ui
        .small_button(tl!("Add Color Ramp File…"))
        .on_hover_text(tl!("Add color ramps to the project from a .rgisramp file, a QGIS style, a GDAL color file or a GMT palette."))
        .clicked()
    {
        let _ = app.run("ui.openItem", json!({"kind": "ramps"}));
    }
}

/// A long list of ramps, a swatch and its name on each row, scrolling in a box of its own (only
/// the rows in view are drawn).
fn ramp_list(app: &mut RustGisApp, ui: &mut Ui, ramps: &[Ramp], cur: &Ramp, t: &Tokens) {
    const ROW_H: f32 = 24.0;
    let scroll = egui::ScrollArea::vertical().id_salt("ramp_list").max_height(260.0).auto_shrink([false, true]);
    scroll.show_rows(ui, ROW_H, ramps.len(), |ui, rows| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let w = ui.available_width();
        for r in ramps.get(rows).unwrap_or_default() {
            let selected = r == cur;
            let (rect, resp) = ui.allocate_exact_size(vec2(w, ROW_H), Sense::click());
            resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, selected, r.name()));
            if selected || resp.hovered() {
                ui.painter().rect_filled(rect, CornerRadius::same(3), if selected { t.accent.gamma_multiply(0.18) } else { t.hover });
            }
            let swatch = Rect::from_min_size(pos2(rect.min.x + 6.0, rect.center().y - 7.0), vec2(72.0, 14.0));
            paint_ramp(ui.painter(), swatch, r);
            ui.painter().rect_stroke(swatch, CornerRadius::same(2), Stroke::new(1.0, t.border_strong), egui::StrokeKind::Outside);
            // The library's own names, which aren't translated.
            ui.painter().text(
                pos2(swatch.max.x + 10.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                r.name(),
                egui::FontId::proportional(13.0),
                t.text,
            );
            if resp.clicked() {
                let _ = app.run("layer.ramp", json!({"ramp": r.name()}));
            }
        }
    });
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
            .on_hover_text(tl!("Draw new single-band rasters with the grey stretch again."))
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
    let key = egui::Id::unique(CLASS_COLOR_DRAG);
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
            // Unique Values legends sort their classes (by value or by label): row i shows class `order[i]`,
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
                        let key = egui::Id::unique(CLASS_COLOR_DRAG);
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
                    let id = egui::Id::unique(("class_label", l.id, l.renderer.field().unwrap_or_default(), class.to_string()));
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

/// Graduated Colors' bounds typed by hand: each class's upper bound as a number, a button to take
/// a bound out, and one to split the widest class in two. A bound being dragged or typed is held
/// here and applied once, when the drag stops or the field is left, so a drag is one undo step.
fn manual_breaks(app: &mut RustGisApp, ui: &mut Ui, field: &str, breaks: &[f64], min: f64, ramp: &Ramp) {
    let mut b = breaks.to_vec();
    let speed = ((b.last().copied().unwrap_or(min) - min).abs() / 200.0).max(1e-9);
    let mut apply = false;
    let mut remove = None;
    let (layer, last) = (app.session.active_layer, b.len().saturating_sub(1));
    for (i, v) in b.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            let id = egui::Id::unique(("manual-break", layer, field, i));
            if let Some(held) = ui.data(|d| d.get_temp::<f64>(id)) {
                *v = held;
            }
            let input_label = ui.label(crate::i18n::fmt("Class {n} up to", &[("n", &(i + 1).to_string())]));
            let r = ui.add(egui::DragValue::new(v).update_while_editing(false).speed(speed)).labelled_by(input_label.id);
            if crate::widgets::changed(&r) {
                ui.data_mut(|d| d.insert_temp(id, *v));
            }
            if (r.drag_stopped() || r.lost_focus()) && ui.data(|d| d.get_temp::<f64>(id)).is_some() {
                ui.data_mut(|d| d.remove_temp::<f64>(id));
                apply = true;
            }
            // The last bound closes the classes at the field's largest value: taking it out would
            // only bring it back.
            if i < last && ui.small_button("×").on_hover_text(tl!("Take this bound out")).clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        b.remove(i);
        apply = true;
    }
    if b.len() < 12 && ui.button(tl!("Split the widest class")).clicked() {
        let lower = |i: usize| if i == 0 { min } else { b[i - 1] };
        if let Some(i) = (0..b.len()).max_by(|&x, &y| (b[x] - lower(x)).total_cmp(&(b[y] - lower(y)))) {
            b.insert(i, (lower(i) + b[i]) / 2.0);
            apply = true;
        }
    }
    if apply
        && let Err(e) =
            app.run("layer.symbology", json!({"type": "graduated", "field": field, "method": "manual", "breaks": b, "ramp": ramp.name()}))
    {
        app.session.notify(e, true);
    }
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
                let resp = crate::widgets::color_button(ui, &mut c);
                if resp.changed() {
                    crate::widgets::one_step_per_press(app, &resp);
                    let _ = app.run("layer.color", json!({"color": rgba(c).hex8()}));
                }
                ui.end_row();
            }
            ui.label(tl!(if g == GeometryType::Polyline { "Line color" } else { "Outline color" }));
            let mut c = c32(s.stroke);
            let resp = crate::widgets::color_button(ui, &mut c);
            if resp.changed() {
                crate::widgets::one_step_per_press(app, &resp);
                let key = if g == GeometryType::Polyline { "layer.color" } else { "layer.outline" };
                let _ = app.run(key, json!({"color": rgba(c).hex()}));
            }
            ui.end_row();
            let input_label = ui.label(tl!(if g == GeometryType::Polyline { "Line width" } else { "Outline width" }));
            let mut w = s.stroke_width;
            let resp = ui
                .add(egui::DragValue::new(&mut w).update_while_editing(false).range(0.0..=12.0).speed(0.05).suffix(" pt"))
                .labelled_by(input_label.id);
            if crate::widgets::changed(&resp) {
                crate::widgets::one_step_per_press(app, &resp);
                let _ = app.run("layer.outline", json!({"width": w}));
            }
            ui.end_row();
            if g == GeometryType::Point {
                let input_label = ui.label(tl!("Size"));
                let mut sz = s.size;
                let resp = ui
                    .add(egui::DragValue::new(&mut sz).update_while_editing(false).range(1.0..=60.0).speed(0.1).suffix(" pt"))
                    .labelled_by(input_label.id);
                if crate::widgets::changed(&resp) {
                    crate::widgets::one_step_per_press(app, &resp);
                    let _ = app.run("layer.marker", json!({"size": sz}));
                }
                ui.end_row();
                let input_label = ui.label(tl!("Shape"));
                egui::ComboBox::from_id_salt("marker_shape")
                    .selected_text(tl!(s.marker.label()))
                    .show_ui(ui, |ui| {
                        for m in Marker::ALL {
                            if ui.selectable_label(m == s.marker, tl!(m.label())).clicked() {
                                let _ = app.run("layer.marker", json!({"shape": m.label()}));
                            }
                        }
                        let on = matches!(s.marker, Marker::Char(_));
                        if ui.selectable_label(on, tl!("Character")).clicked() && !on {
                            let _ = app.run("layer.marker", json!({"char": DEFAULT_CHAR.to_string()}));
                        }
                    })
                    .response
                    .labelled_by(input_label.id);
                ui.end_row();
                if let Marker::Char(g) = s.marker
                    && let Some((ch, font)) = char_rows(ui, "marker_char", g)
                {
                    let _ = app.run("layer.marker", json!({"char": ch.to_string(), "font": font.unwrap_or_default()}));
                }
            }
        });
    });
}

/// The dash patterns the stroke editor offers, in stroke widths (dash, gap, …).
const DASH_PRESETS: [(&str, Option<&[f32]>); 5] = [
    ("Solid", None),
    ("Dashed", Some(&[4.0, 2.0])),
    ("Dotted", Some(&[1.0, 2.0])),
    ("Dash-dot", Some(&[4.0, 2.0, 1.0, 2.0])),
    ("Long dash", Some(&[8.0, 3.0])),
];

/// A dash pattern being typed in the stroke editor: the layer, the class (`None` for the layer's
/// own strokes) and the text.
type DashDraft = (u32, Option<ClassRef>, String);

/// The text of `draft` if it was typed for `owner`'s stroke, else `None` (the stroke's own pattern
/// shows).
fn draft_for(draft: Option<DashDraft>, owner: &(u32, Option<ClassRef>)) -> Option<String> {
    draft.filter(|(layer, class, _)| *layer == owner.0 && *class == owner.1).map(|(.., text)| text)
}

/// A dash pattern as the stroke editor shows it: `4 2`. Each length is written as it is kept, so
/// the text reads back as the same pattern.
fn dash_text(d: &Dash) -> String {
    d.steps().iter().map(|v| v.to_string()).collect::<Vec<_>>().join(" ")
}

/// The pattern the dash box sets when it loses focus: `None` unless the text was typed (focusing
/// and leaving the box is no change), Escape wasn't what ended it, and it is a pattern other than
/// `current`.
fn dash_commit(typed: bool, escaped: bool, text: &str, current: Option<Dash>) -> Option<Option<Dash>> {
    parse_dash(text).filter(|d| typed && !escaped && *d != current)
}

/// A dash pattern typed in the stroke editor: `Some(None)` for none (solid), `None` when it isn't
/// one ([`Dash::new`]). Lengths are separated by spaces, commas or semicolons.
fn parse_dash(text: &str) -> Option<Option<Dash>> {
    let parts: Vec<&str> = text.split([' ', ',', ';']).filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return Some(None);
    }
    let steps: Vec<f32> = parts.iter().map(|p| p.parse::<f32>().ok()).collect::<Option<_>>()?;
    Dash::new(&steps).map(Some)
}

/// A stroke as `layer.lineStrokes` takes it.
fn stroke_json(st: &LineStroke) -> Value {
    json!({
        "color": st.color.map(|c| c.hex8()),
        "width": st.width,
        "offset": st.offset,
        "cap": st.cap.code(),
        "join": st.join.code(),
        "level": st.level,
        "dash": st.dash.map(|d| d.steps().to_vec()),
        "markers": st.markers.map(|m| markers_json(&m)),
    })
}

/// A stroke's markers as `layer.lineStrokes` takes them.
fn markers_json(m: &LineMarkers) -> Value {
    let glyph = match m.mark {
        LineMark::Marker(Marker::Char(g)) => Some(g),
        _ => None,
    };
    json!({
        "mark": match m.mark {
            LineMark::Hash => "hash".to_string(),
            LineMark::Marker(shape) => shape.label().to_ascii_lowercase(),
        },
        "char": glyph.map(|g| g.ch.to_string()),
        "font": glyph.map(|g| g.font.as_str()),
        "size": m.size,
        "at": m.at.codes(),
        "interval": m.interval,
        "offsetAlong": m.offset_along,
        "rotate": m.rotate,
        "averageAngle": m.average_angle,
        "angle": m.angle,
        "outline": m.outline.map(|c| c.hex8()),
    })
}

/// The character a marker switched to a character starts as (one the label font has).
const DEFAULT_CHAR: char = 'A';

/// A character marker's rows (`g`): its character, typed, and the installed family it is drawn
/// from. The character and family (none: the label font) when either changes.
fn char_rows(ui: &mut Ui, salt: impl std::hash::Hash + std::fmt::Debug + Copy, g: Glyph) -> Option<(char, Option<String>)> {
    let font = (!g.font.is_default()).then(|| g.font.as_str().to_string());
    let mut out = None;
    let input_label = ui.label(tl!("Character"));
    let mut text = g.ch.to_string();
    let r = ui.add(egui::TextEdit::singleline(&mut text).desired_width(48.0).char_limit(2)).labelled_by(input_label.id);
    // The character typed: the last one, so typing over the old one replaces it.
    if r.changed()
        && let Some(ch) = text.chars().rfind(|c| *c != g.ch).or_else(|| text.chars().next())
        && ch != g.ch
    {
        out = Some((ch, font.clone()));
    }
    ui.end_row();
    let input_label = ui.label(tl!("Font"));
    let picked = crate::labelpane::family_picker(ui, salt, egui::Id::unique((salt, "search")), font.as_deref());
    picked.response.labelled_by(input_label.id);
    if let Some(f) = picked.chosen {
        out = Some((g.ch, f));
    }
    ui.end_row();
    out
}

/// The places along a line the stroke editor offers for markers.
fn marker_places() -> [(&'static str, Placements); 7] {
    [
        ("At intervals", Placements::INTERVAL),
        ("At vertices", Placements::VERTICES),
        ("At inner vertices", Placements::INNER_VERTICES),
        ("At both ends", Placements::FIRST.with(Placements::LAST)),
        ("At the start", Placements::FIRST),
        ("At the end", Placements::LAST),
        ("In the middle", Placements::MIDDLE),
    ]
}

/// Sets the layer's stacked strokes, or `class`'s own.
fn set_strokes(app: &mut RustGisApp, class: Option<&ClassRef>, strokes: &[LineStroke]) {
    let mut p = json!({"strokes": strokes.iter().map(stroke_json).collect::<Vec<_>>()});
    match class {
        Some(ClassRef::Value(v)) => p["value"] = json!(v),
        Some(ClassRef::Index(i)) => p["index"] = json!(i),
        Some(ClassRef::Other) => p["other"] = json!(true),
        None => {}
    }
    let _ = app.run("layer.lineStrokes", p);
}

/// The classes a line layer's strokes can be set for, with their legend labels and symbols.
pub(crate) fn stroke_classes(r: &Renderer) -> Vec<(ClassRef, String, Symbol)> {
    match r {
        Renderer::Unique { classes, other, class_labels, other_label, .. } => {
            // A value listed twice is drawn with its first class, as the legend shows it.
            let mut seen = std::collections::HashSet::new();
            classes
                .iter()
                .filter(|(k, _)| seen.insert(k.as_str()))
                .map(|(k, s)| (ClassRef::Value(k.clone()), class_labels.get(k).cloned().unwrap_or_else(|| k.clone()), *s))
                .chain(std::iter::once((ClassRef::Other, other_label.clone().unwrap_or_else(|| tl!(OTHER_VALUES).to_string()), *other)))
                .collect()
        }
        Renderer::Graduated { symbols, .. } => {
            let legend = r.legend();
            symbols
                .iter()
                .enumerate()
                .map(|(i, s)| (ClassRef::Index(i), legend.get(i).map(|e| e.label.clone()).unwrap_or_else(|| i.to_string()), *s))
                .collect()
        }
        Renderer::Single(_) | Renderer::Proportional { .. } => Vec::new(),
    }
}

/// A stroke's markers along the editor's preview line, from `x.0` to `x.1` at `y`, scaled by `k`:
/// markers as dots (squares as squares), hash marks as ticks, `w` wide. A sketch, not the map's
/// drawing: other shapes and outlines aren't shown, and inner placements are marked at the middle.
fn marker_preview(ui: &Ui, m: &LineMarkers, x: (f32, f32), y: f32, k: f32, c: egui::Color32, w: f32) {
    let mut xs = Vec::new();
    if m.at.has(Placements::INTERVAL) {
        let step = (m.interval * k).max(4.0);
        let mut at = x.0 + (m.offset_along * k).rem_euclid(step);
        while at <= x.1 && xs.len() < 256 {
            xs.push(at);
            at += step;
        }
    }
    if m.at.has(Placements::VERTICES) || m.at.has(Placements::FIRST) {
        xs.push(x.0);
    }
    if m.at.has(Placements::VERTICES) || m.at.has(Placements::LAST) {
        xs.push(x.1);
    }
    if m.at.has(Placements::MIDDLE) || m.at.has(Placements::INNER_VERTICES) || m.at.has(Placements::SEGMENT_MIDDLES) {
        xs.push((x.0 + x.1) / 2.0);
    }
    let half = (m.size * k).max(1.0) / 2.0;
    for at in xs {
        match m.mark {
            LineMark::Hash => {
                let (sin, cos) = m.angle.to_radians().sin_cos();
                let (dx, dy) = (half * sin, half * cos);
                ui.painter().line_segment([pos2(at - dx, y + dy), pos2(at + dx, y - dy)], Stroke::new(w, c));
            }
            LineMark::Marker(Marker::Square) => {
                ui.painter().rect_filled(Rect::from_center_size(pos2(at, y), vec2(half * 1.8, half * 1.8)), CornerRadius::ZERO, c);
            }
            LineMark::Marker(Marker::Char(g)) => glyph(ui.painter(), pos2(at, y), g, (2.0 * half).max(6.0), c),
            LineMark::Marker(_) => {
                ui.painter().circle_filled(pos2(at, y), half, c);
            }
        }
    }
}

/// The stroke editor's rows for a stroke of markers or hash marks (`st`, its markers `m`): the
/// shape, its size, where it goes and how far apart, and whether it turns with the line.
#[allow(clippy::too_many_arguments)]
fn marker_rows(
    app: &mut RustGisApp,
    ui: &mut Ui,
    i: usize,
    st: LineStroke,
    m: LineMarkers,
    points: bool,
    set: &impl Fn(&mut RustGisApp, LineStroke),
) {
    let with = |m: LineMarkers| LineStroke { markers: Some(m), ..st };
    if let LineMark::Marker(shape) = m.mark {
        let input_label = ui.label(tl!("Shape"));
        egui::ComboBox::from_id_salt(("stroke_marker_shape", i))
            .selected_text(tl!(shape.label()))
            .show_ui(ui, |ui| {
                for s in Marker::ALL {
                    if ui.selectable_label(s == shape, tl!(s.label())).clicked() {
                        set(app, with(LineMarkers { mark: LineMark::Marker(s), ..m }));
                    }
                }
                let on = matches!(shape, Marker::Char(_));
                if ui.selectable_label(on, tl!("Character")).clicked() && !on {
                    set(app, with(LineMarkers { mark: LineMark::Marker(Marker::Char(Glyph::new(DEFAULT_CHAR, ""))), ..m }));
                }
            })
            .response
            .labelled_by(input_label.id);
        ui.end_row();
        if let Marker::Char(g) = shape
            && let Some((ch, font)) = char_rows(ui, ("stroke_marker_char", i), g)
        {
            let mark = LineMark::Marker(Marker::Char(Glyph::new(ch, font.as_deref().unwrap_or_default())));
            set(app, with(LineMarkers { mark, ..m }));
        }
    }
    let input_label = ui.label(if m.mark == LineMark::Hash { tl!("Length") } else { tl!("Size") });
    let mut size = m.size;
    let resp = ui
        .add(egui::DragValue::new(&mut size).update_while_editing(false).range(0.0..=MAX_LINE_MARKER_SIZE).speed(0.1).suffix(" pt"))
        .labelled_by(input_label.id);
    if crate::widgets::changed(&resp) {
        crate::widgets::one_step_per_press(app, &resp);
        set(app, with(LineMarkers { size, ..m }));
    }
    ui.end_row();
    // A point's mark sits at the point, shifted right (and down, by the stroke's offset).
    if points {
        let input_label = ui.label(tl!("Offset right"));
        let mut along = m.offset_along;
        let drag = egui::DragValue::new(&mut along)
            .update_while_editing(false)
            .range(-MAX_STROKE_OFFSET..=MAX_STROKE_OFFSET)
            .speed(0.05)
            .suffix(" pt");
        let resp = ui.add(drag).on_hover_text(tl!("Shifts the mark right of the point (left when negative)")).labelled_by(input_label.id);
        if crate::widgets::changed(&resp) {
            crate::widgets::one_step_per_press(app, &resp);
            set(app, with(LineMarkers { offset_along: along, ..m }));
        }
        ui.end_row();
    } else {
        let input_label = ui.label(tl!("Place"));
        let places = marker_places();
        let shown =
            places.iter().find(|(_, p)| *p == m.at).map_or_else(|| tl!("Several places").to_string(), |(label, _)| tl!(label).to_string());
        egui::ComboBox::from_id_salt(("stroke_marker_place", i))
            .selected_text(shown)
            .show_ui(ui, |ui| {
                for (label, p) in places {
                    if ui.selectable_label(p == m.at, tl!(label)).clicked() {
                        set(app, with(LineMarkers { at: p, ..m }));
                    }
                }
            })
            .response
            .labelled_by(input_label.id);
        ui.end_row();
        if m.at.has(Placements::INTERVAL) {
            let input_label = ui.label(tl!("Interval"));
            let mut interval = m.interval;
            let drag = egui::DragValue::new(&mut interval)
                .update_while_editing(false)
                .range(MIN_MARKER_INTERVAL..=MAX_MARKER_INTERVAL)
                .speed(0.2)
                .suffix(" pt");
            let resp = ui.add(drag).on_hover_text(tl!("The distance between two marks along the line")).labelled_by(input_label.id);
            if crate::widgets::changed(&resp) {
                crate::widgets::one_step_per_press(app, &resp);
                set(app, with(LineMarkers { interval, ..m }));
            }
            ui.end_row();
        }
    }
    ui.label(tl!("Angle"));
    ui.horizontal(|ui| {
        let mut angle = m.angle;
        let drag = egui::DragValue::new(&mut angle).update_while_editing(false).range(0.0..=359.9).speed(1.0).suffix("°");
        let tip = if m.mark == LineMark::Hash { tl!("0° crosses the line square") } else { tl!("Turned further, clockwise") };
        let resp = ui.add(drag).on_hover_text(tip);
        if crate::widgets::changed(&resp) {
            crate::widgets::one_step_per_press(app, &resp);
            set(app, with(LineMarkers { angle, ..m }));
        }
        let mut rotate = m.rotate;
        let (label, tip) = if points {
            (tl!("With the field"), tl!("Turn the mark by the layer's rotation field, its offset with it"))
        } else {
            (tl!("With the line"), tl!("Turn each mark with the line's direction"))
        };
        if ui.checkbox(&mut rotate, label).on_hover_text(tip).changed() {
            set(app, with(LineMarkers { rotate, ..m }));
        }
    });
    ui.end_row();
}

/// A point symbol's marker as a mark (the first of a point layer's marks): its shape, size, fill and
/// outline, taking its colours from the symbol.
fn symbol_mark(s: &Symbol) -> LineStroke {
    let outline = (s.stroke_width > 0.0 && s.stroke.a() > 0).then_some(s.stroke);
    let m = LineMarkers { mark: LineMark::Marker(s.marker), size: s.size, outline, at: Placements::FIRST, ..LineMarkers::default() };
    LineStroke { markers: Some(m), ..LineStroke::default() }
}

/// A point layer's marks at the middle of `r`, bottom first, shifted as they are on the map (their
/// offsets turned with their angles).
fn marks_preview(ui: &Ui, r: Rect, strokes: &[LineStroke], base: &Symbol) {
    let shown: Vec<LineStroke> = if strokes.is_empty() { vec![symbol_mark(base)] } else { strokes.to_vec() };
    let reach = shown.iter().filter_map(|st| st.markers.map(|m| m.offset_along.hypot(st.offset) + m.size / 2.0)).fold(1.0_f32, f32::max);
    let k = (18.0 / reach).min(1.0);
    for st in &shown {
        let Some(m) = st.markers else { continue };
        let (c, w) = st.resolve(base);
        // A mark of no colour of its own takes the symbol's fill.
        let c = if st.color.is_none() { base.fill } else { c };
        let (sin, cos) = m.angle.to_radians().sin_cos();
        let (ox, oy) = (m.offset_along * k, st.offset * k);
        let at = r.center() + vec2(ox * cos - oy * sin, ox * sin + oy * cos);
        let one = LineMarkers { at: Placements::FIRST, ..m };
        marker_preview(ui, &one, (at.x, at.x), at.y, k, c32(c), (w * k).max(0.5));
    }
}

/// The field a point layer's markers turn by (`layer.markerRotation`): none, or one of its number
/// fields, its angles clockwise or counterclockwise.
fn marker_rotation_row(app: &mut RustGisApp, ui: &mut Ui, l: &rustgis_engine::map::Layer) {
    let cur = l.marker_rotation.as_ref();
    ui.horizontal(|ui| {
        let input_label = ui.label(tl!("Rotation"));
        let shown = cur.map_or_else(|| tl!("None").to_string(), |r| r.field.clone());
        egui::ComboBox::from_id_salt(("marker_rotation", l.id))
            .selected_text(shown)
            .show_ui(ui, |ui| {
                if ui.selectable_label(cur.is_none(), tl!("None")).clicked() && cur.is_some() {
                    let _ = app.run("layer.markerRotation", json!({"field": null}));
                }
                for f in l.dataset.fields.iter().filter(|f| f.ty.is_numeric()) {
                    let on = cur.is_some_and(|r| r.field == f.name);
                    if ui.selectable_label(on, &f.name).clicked() && !on {
                        let ccw = cur.is_some_and(|r| r.counterclockwise);
                        let _ = app.run("layer.markerRotation", json!({"field": f.name, "counterclockwise": ccw}));
                    }
                }
            })
            .response
            .on_hover_text(tl!("Turn each point's marker by the angle in a field, in degrees"))
            .labelled_by(input_label.id);
        if let Some(r) = cur {
            let mut ccw = r.counterclockwise;
            if ui
                .checkbox(&mut ccw, tl!("Counterclockwise"))
                .on_hover_text(tl!("The angles run counterclockwise, as in mathematics"))
                .changed()
            {
                let _ = app.run("layer.markerRotation", json!({"field": r.field, "counterclockwise": ccw}));
            }
        }
    });
}

/// The By Expression section: the symbol's size, colors, outline width and (on points) angle per
/// feature, each from an expression (`layer.symbolBy`). An expression applies when its box loses
/// focus; one that doesn't read shows why as it is typed, and stays in its box unapplied until
/// it is fixed or Escape drops it.
fn symbol_by_editor(app: &mut RustGisApp, ui: &mut Ui, l: &rustgis_engine::map::Layer, t: &Tokens) {
    let g = l.geometry_type();
    let by = &l.symbol_by;
    let mut rows: Vec<(&str, &str, &Option<String>)> = Vec::new();
    match g {
        GeometryType::Point => rows.extend([
            ("size", tl!("Size"), &by.size),
            ("fill", tl!("Fill"), &by.fill),
            ("stroke", tl!("Outline"), &by.stroke),
            ("strokeWidth", tl!("Outline width"), &by.stroke_width),
            ("angle", tl!("Angle"), &by.angle),
        ]),
        GeometryType::Polyline => rows.extend([("stroke", tl!("Color"), &by.stroke), ("strokeWidth", tl!("Width"), &by.stroke_width)]),
        _ => rows.extend([
            ("fill", tl!("Fill"), &by.fill),
            ("stroke", tl!("Outline"), &by.stroke),
            ("strokeWidth", tl!("Outline width"), &by.stroke_width),
        ]),
    }
    // A layer's id is its map's: a draft left on one map's layer isn't another's.
    let scope = crate::labelpane::layer_key(app, l);
    egui::Frame::NONE.fill(t.pane_header).corner_radius(CornerRadius::same(8)).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(egui::RichText::new(tl!("By Expression")).font(semibold(12.5)));
        ui.label(
            egui::RichText::new(tl!(
                "Set per feature from an expression, e.g. \"pop\" / 1e5 or CASE WHEN … END; colors as '#RRGGBB'. Empty: the symbol's own."
            ))
            .small()
            .weak(),
        );
        for (key, label, cur) in rows {
            if let Some(text) = expression_box(ui, egui::Id::unique(("symbol_by", scope, key)), label, cur.as_deref(), &l.dataset, t) {
                let _ = app.run("layer.symbolBy", json!({ key: text }));
            }
        }
    });
}

/// One By Expression box: `label`, then the expression set (`cur`), checked against `ds` as it is
/// typed. Gives the text to apply (blank: none) when the box loses focus with one that reads and
/// differs from `cur`; one that doesn't read shows why, and stays in its box unapplied until it is
/// fixed or Escape drops it.
pub(crate) fn expression_box(
    ui: &mut Ui,
    id: egui::Id,
    label: &str,
    cur: Option<&str>,
    ds: &rustgis_engine::data::Dataset,
    t: &Tokens,
) -> Option<String> {
    let cur = cur.unwrap_or_default().to_string();
    let (typing, failed) = (id.with("typing"), id.with("failed"));
    let focused = ui.memory(|m| m.has_focus(id));
    // Each kept with the expression it was typed over: one set meanwhile (by undo or a
    // command) drops it. Typing left over from a box that stopped being drawn while
    // focused is dropped too.
    let over = |d: &mut egui::util::IdTypeMap, key: egui::Id| {
        let v = d.get_temp::<(String, String)>(key);
        if v.as_ref().is_some_and(|(was, _)| *was != cur) {
            d.remove::<(String, String)>(key);
        }
        v.filter(|(was, _)| *was == cur).map(|(_, text)| text)
    };
    if !focused {
        ui.data_mut(|d| d.remove::<(String, String)>(typing));
    }
    // Text that didn't read, kept until it is changed or dropped.
    let kept = ui.data_mut(|d| over(d, failed));
    let mut buf = match focused {
        true => ui.data_mut(|d| over(d, typing)).or_else(|| kept.clone()).unwrap_or_else(|| cur.clone()),
        false => kept.clone().unwrap_or_else(|| cur.clone()),
    };
    let r = ui
        .horizontal(|ui| {
            let input_label = ui
                .allocate_ui_with_layout(egui::vec2(96.0, 18.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.set_min_width(96.0);
                    ui.label(label)
                })
                .inner;
            ui.add(
                egui::TextEdit::singleline(&mut buf)
                    .id(id)
                    .hint_text(tl!("None"))
                    .char_limit(rustgis_engine::data::query::MAX_LEN)
                    .font(egui::TextStyle::Monospace)
                    .desired_width(f32::INFINITY),
            )
            .labelled_by(input_label.id)
        })
        .inner;
    // Checked here, as Calculate Field checks it, only while it differs from what is set.
    let text = buf.trim();
    let error = (text != cur.trim() && !text.is_empty()).then(|| rustgis_engine::data::query::parse(text, ds).err()).flatten();
    let mut apply = None;
    if r.lost_focus() {
        ui.data_mut(|d| d.remove::<(String, String)>(typing));
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            ui.data_mut(|d| d.remove::<(String, String)>(failed));
        } else if error.is_some() {
            ui.data_mut(|d| d.insert_temp(failed, (cur.clone(), buf.clone())));
        } else {
            ui.data_mut(|d| d.remove::<(String, String)>(failed));
            if text != cur.trim() {
                apply = Some(text.to_string());
            }
        }
    } else if r.has_focus() {
        ui.data_mut(|d| d.insert_temp(typing, (cur.clone(), buf.clone())));
    }
    if let Some(e) = error.filter(|_| focused || r.lost_focus() || kept.is_some()) {
        ui.label(egui::RichText::new(crate::i18n::fmt("Invalid: {error}", &[("error", &e)])).small().color(t.red));
    }
    apply
}

/// The Strokes section of a line layer's symbology: strokes stacked in one symbol, each with
/// its own colour, width, sideways offset, cap and join. Listed top first, as they are seen. A
/// layer with classes can stack strokes for one class of its own, picked at the top. A point
/// layer's are its Marks: markers stacked at the point, each shifted and turned.
fn line_strokes_editor(app: &mut RustGisApp, ui: &mut Ui, l: &rustgis_engine::map::Layer, t: &Tokens) {
    let points = l.geometry_type() == GeometryType::Point;
    let classes = stroke_classes(&l.renderer);
    let picked_id = egui::Id::unique(("stroke_class", l.id));
    // The class being edited, while the renderer still has it; `None` is the layer's own strokes.
    let picked: Option<ClassRef> =
        ui.data(|d| d.get_temp::<Option<ClassRef>>(picked_id)).flatten().filter(|c| classes.iter().any(|(k, ..)| k == c));
    let class = picked.as_ref();
    let base = class.and_then(|c| classes.iter().find(|(k, ..)| k == c)).map_or_else(|| l.renderer.base_symbol(), |(.., s)| *s);
    let own = class.and_then(|c| l.renderer.class_strokes(c).ok()).map(<[LineStroke]>::to_vec);
    // What the class draws: its own strokes, else the layer's.
    let strokes = match &own {
        Some(v) if !v.is_empty() => v.clone(),
        Some(_) | None => l.line_strokes.clone(),
    };
    // A class drawn with the layer's strokes edits them as a copy of its own.
    let borrowed = own.as_ref().is_some_and(Vec::is_empty);
    egui::Frame::NONE.fill(t.pane_header).corner_radius(CornerRadius::same(8)).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(if points { tl!("Marks") } else { tl!("Strokes") }).font(semibold(12.5)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (label, tip) = if points {
                    (tl!("Add Mark"), tl!("Stack another marker on top"))
                } else {
                    (tl!("Add Stroke"), tl!("Stack another stroke on top"))
                };
                let add = ui.add_enabled(strokes.len() < MAX_STROKES, egui::Button::new(label).small());
                if add.on_hover_text(tip).clicked() {
                    let mut next = strokes.clone();
                    // The first stroke stands for the symbol's own; later ones copy the top one. A
                    // point's first mark is its symbol's marker.
                    let first = if points { symbol_mark(&base) } else { LineStroke::default() };
                    next.push(strokes.last().copied().unwrap_or(first));
                    set_strokes(app, class, &next);
                }
            });
        });
        if !classes.is_empty() {
            ui.horizontal(|ui| {
                let input_label =
                    ui.label(egui::RichText::new(if points { tl!("Marks for") } else { tl!("Strokes for") }).size(11.5).color(t.text_dim));
                let name = |c: Option<&ClassRef>| match c.and_then(|c| classes.iter().find(|(k, ..)| k == c)) {
                    Some((_, label, _)) => label.clone(),
                    None => tl!("All classes").to_string(),
                };
                egui::ComboBox::from_id_salt(("stroke_class_pick", l.id))
                    .selected_text(name(class))
                    .show_ui(ui, |ui| {
                        if ui.selectable_label(class.is_none(), tl!("All classes")).on_hover_text(tl!("The layer's strokes")).clicked() {
                            ui.data_mut(|d| d.insert_temp::<Option<ClassRef>>(picked_id, None));
                        }
                        for (k, label, _) in &classes {
                            // A class with strokes of its own is marked.
                            let has_own = l.renderer.class_strokes(k).is_ok_and(|s| !s.is_empty());
                            let text = if has_own { format!("{label} •") } else { label.clone() };
                            if ui.selectable_label(class == Some(k), text).clicked() {
                                ui.data_mut(|d| d.insert_temp(picked_id, Some(k.clone())));
                            }
                        }
                    })
                    .response
                    .labelled_by(input_label.id);
            });
        }
        if l.has_strokes() && !points {
            let mut on = l.stroke_levels;
            let tip = tl!("Draw every feature's strokes of one level before the next, so casings join under the cores where lines meet");
            if ui.checkbox(&mut on, tl!("Draw strokes in levels")).on_hover_text(tip).changed() {
                let _ = app.run("layer.strokeLevels", json!({"on": on}));
            }
        }
        // The composed symbol along a line drawn left to right: a positive offset shows below it.
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::hover());
        ui.painter().rect_filled(r, CornerRadius::same(4), t.ribbon);
        if points {
            marks_preview(ui, r, &strokes, &base);
        }
        let mut preview: Vec<LineStroke> = if strokes.is_empty() { vec![LineStroke::default()] } else { strokes.clone() };
        // In levels, a stroke shows over the lower levels' (ties keep their stack order), as the map has it.
        if l.stroke_levels {
            let mut levelled: Vec<(u8, LineStroke)> = preview.iter().enumerate().map(|(i, st)| (st.level_at(i), *st)).collect();
            levelled.sort_by_key(|(level, _)| *level);
            preview = levelled.into_iter().map(|(_, st)| st).collect();
        }
        let reach = preview
            .iter()
            .map(|st| st.offset.abs() + st.resolve(&base).1.max(st.markers.map_or(0.0, |m| m.size)) / 2.0)
            .fold(1.0_f32, f32::max);
        let k = (18.0 / reach).min(1.0);
        let (x0, x1) = (r.min.x + 14.0, r.max.x - 14.0);
        let cap_pad = |st: &LineStroke, w: f32| if st.cap == LineCap::Butt { 0.0 } else { w / 2.0 };
        for st in preview.iter().filter(|_| !points) {
            let (c, w) = st.resolve(&base);
            let w = (w * k).max(0.5);
            let y = r.center().y + st.offset * k;
            if let Some(m) = &st.markers {
                marker_preview(ui, m, (x0, x1), y, k, c32(c), w);
                continue;
            }
            let pad = cap_pad(st, w);
            // The dashes along the preview, scaled as the strokes are. Solid when the map draws the
            // pattern solid: under a pixel a round on this screen, at the pattern's own size.
            let steps: Vec<f32> = st.dash.map(|d| d.even_steps().iter().map(|v| v * k).collect()).unwrap_or_default();
            let mut runs = Vec::new();
            if st.dash.is_some_and(|d| d.period() * ui.ctx().pixels_per_point() >= 1.0) && steps.iter().sum::<f32>() > 0.0 {
                let (mut x, mut on) = (x0, true);
                for step in steps.iter().cycle().take(4096) {
                    if x >= x1 {
                        break;
                    }
                    let end = (x + step).min(x1);
                    if on {
                        runs.push((x, end));
                    }
                    (x, on) = (end, !on);
                }
            } else {
                runs.push((x0, x1));
            }
            for (from, to) in runs {
                if st.cap == LineCap::Round {
                    ui.painter().circle_filled(pos2(from, y), w / 2.0, c32(c));
                    ui.painter().circle_filled(pos2(to, y), w / 2.0, c32(c));
                }
                let (a, b) = if st.cap == LineCap::Square { (from - pad, to + pad) } else { (from, to) };
                ui.painter().line_segment([pos2(a, y), pos2(b, y)], Stroke::new(w, c32(c)));
            }
        }
        // The line's direction.
        let (ay, ax) = (r.max.y - 6.0, r.max.x - 8.0);
        if points {
            // (A point has none.)
        } else {
            ui.painter().line_segment([pos2(ax - 10.0, ay), pos2(ax, ay)], Stroke::new(1.0, t.text_dim));
            ui.painter().line_segment([pos2(ax - 3.0, ay - 3.0), pos2(ax, ay)], Stroke::new(1.0, t.text_dim));
            ui.painter().line_segment([pos2(ax - 3.0, ay + 3.0), pos2(ax, ay)], Stroke::new(1.0, t.text_dim));
        }
        if borrowed {
            ui.label(
                egui::RichText::new(if points {
                    tl!("This class is drawn with the layer's marks. Changing them here gives it its own.")
                } else {
                    tl!("This class is drawn with the layer's strokes. Changing them here gives it its own.")
                })
                .size(11.5)
                .color(t.text_dim),
            );
        }
        if strokes.is_empty() {
            ui.label(
                egui::RichText::new(if points {
                    tl!("One marker, from the symbol. Add marks to stack markers, or to shift or turn them.")
                } else {
                    tl!("One stroke, from the symbol. Add strokes for casings, parallel lines and offsets.")
                })
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
                let title = crate::i18n::fmt(if points { "Mark {n}" } else { "Stroke {n}" }, &[("n", &(n - i).to_string())]);
                ui.label(egui::RichText::new(title).size(11.5).color(t.text_dim));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if crate::widgets::icon_button(ui, "close", tl!("Remove")).clicked() {
                        let mut next = strokes.clone();
                        next.remove(i);
                        set_strokes(app, class, &next);
                    }
                    if ui.add_enabled(i > 0, egui::Button::new("↓").small()).on_hover_text(tl!("Move down")).clicked() {
                        let mut next = strokes.clone();
                        next.swap(i, i - 1);
                        set_strokes(app, class, &next);
                    }
                    if ui.add_enabled(i + 1 < n, egui::Button::new("↑").small()).on_hover_text(tl!("Move up")).clicked() {
                        let mut next = strokes.clone();
                        next.swap(i, i + 1);
                        set_strokes(app, class, &next);
                    }
                });
            });
            let set = |app: &mut RustGisApp, new: LineStroke| {
                let mut next = strokes.clone();
                if let Some(slot) = next.get_mut(i) {
                    *slot = new;
                }
                set_strokes(app, class, &next);
            };
            egui::Grid::new(("stroke_grid", i)).num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
                let (c, w) = st.resolve(&base);
                // A point's marks are markers.
                if !points {
                    let input_label = ui.label(tl!("Draw"));
                    let kind = |st: &LineStroke| match st.markers.map(|m| m.mark) {
                        None => tl!("Line"),
                        Some(LineMark::Hash) => tl!("Hash marks"),
                        Some(LineMark::Marker(_)) => tl!("Markers"),
                    };
                    egui::ComboBox::from_id_salt(("stroke_draw", i))
                        .selected_text(kind(&st))
                        .show_ui(ui, |ui| {
                            let line = LineStroke { markers: None, ..st };
                            let markers = LineStroke {
                                markers: Some(LineMarkers { mark: LineMark::Marker(Marker::Circle), ..st.markers.unwrap_or_default() }),
                                ..st
                            };
                            let hash =
                                LineStroke { markers: Some(LineMarkers { mark: LineMark::Hash, ..st.markers.unwrap_or_default() }), ..st };
                            for option in [line, markers, hash] {
                                if ui.selectable_label(kind(&option) == kind(&st), kind(&option)).clicked() && kind(&option) != kind(&st) {
                                    set(app, option);
                                }
                            }
                        })
                        .response
                        .on_hover_text(tl!("A line, or markers or hash marks along it"))
                        .labelled_by(input_label.id);
                    ui.end_row();
                }
                ui.label(tl!("Color"));
                ui.horizontal(|ui| {
                    let mut cc = c32(c);
                    let resp = crate::widgets::color_button(ui, &mut cc);
                    if resp.changed() {
                        crate::widgets::one_step_per_press(app, &resp);
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
                    let drag =
                        egui::DragValue::new(&mut ww).update_while_editing(false).range(0.0..=MAX_STROKE_WIDTH).speed(0.05).suffix(" pt");
                    let resp = ui.add_enabled(st.width.is_some(), drag).on_hover_text(tl!("Width"));
                    if crate::widgets::changed(&resp) {
                        crate::widgets::one_step_per_press(app, &resp);
                        set(app, LineStroke { width: Some(ww), ..st });
                    }
                    let mut from_symbol = st.width.is_none();
                    if ui.checkbox(&mut from_symbol, tl!("From symbol")).on_hover_text(tl!("Use each feature's line width")).changed() {
                        set(app, LineStroke { width: if from_symbol { None } else { Some(w) }, ..st });
                    }
                });
                ui.end_row();
                ui.label(if points { tl!("Offset down") } else { tl!("Offset") });
                let mut off = st.offset;
                let drag = egui::DragValue::new(&mut off)
                    .update_while_editing(false)
                    .range(-MAX_STROKE_OFFSET..=MAX_STROKE_OFFSET)
                    .speed(0.05)
                    .suffix(" pt");
                let tip = if points {
                    tl!("Shifts the mark below the point (above when negative)")
                } else {
                    tl!("Shifts the stroke sideways: positive to the right of the line's direction, negative to the left")
                };
                let resp = ui.add(drag).on_hover_text(tip);
                if crate::widgets::changed(&resp) {
                    crate::widgets::one_step_per_press(app, &resp);
                    set(app, LineStroke { offset: off, ..st });
                }
                ui.end_row();
                if let Some(m) = st.markers {
                    marker_rows(app, ui, i, st, m, points, &set);
                } else {
                    let input_label = ui.label(tl!("Cap"));
                    egui::ComboBox::from_id_salt(("stroke_cap", i))
                        .selected_text(tl!(st.cap.label()))
                        .show_ui(ui, |ui| {
                            for c in LineCap::ALL {
                                if ui.selectable_label(c == st.cap, tl!(c.label())).clicked() {
                                    set(app, LineStroke { cap: c, ..st });
                                }
                            }
                        })
                        .response
                        .labelled_by(input_label.id);
                    ui.end_row();
                    let input_label = ui.label(tl!("Join"));
                    egui::ComboBox::from_id_salt(("stroke_join", i))
                        .selected_text(tl!(st.join.label()))
                        .show_ui(ui, |ui| {
                            for j in LineJoin::ALL {
                                if ui.selectable_label(j == st.join, tl!(j.label())).clicked() {
                                    set(app, LineStroke { join: j, ..st });
                                }
                            }
                        })
                        .response
                        .labelled_by(input_label.id);
                    ui.end_row();
                    ui.label(tl!("Dash"));
                    ui.horizontal(|ui| {
                        // The pattern as typed, kept while the box has focus; set when it loses it. The
                        // draft remembers whose stroke it is, so it never shows or lands on another
                        // layer's or class's stroke at the same place.
                        let id = ui.scope_id().with(("stroke_dash", i));
                        let owner = (l.id, class.cloned());
                        let shown = st.dash.map(|d| dash_text(&d)).unwrap_or_default();
                        let draft = draft_for(ui.data_mut(|m| m.get_temp::<DashDraft>(id)), &owner);
                        let typed = draft.is_some();
                        let mut text = draft.unwrap_or(shown);
                        let edit = egui::TextEdit::singleline(&mut text).hint_text(tl!("Solid")).desired_width(84.0);
                        let r = ui
                            .add(edit)
                            .on_hover_text(tl!("Dash and gap lengths in points, in turn, such as 4 2. Empty for a solid line."));
                        if r.changed() {
                            ui.data_mut(|m| m.insert_temp::<DashDraft>(id, (owner.0, owner.1.clone(), text.clone())));
                        }
                        // Text that isn't a pattern is outlined: it won't be kept when the box loses focus.
                        if parse_dash(&text).is_none() {
                            ui.painter().rect_stroke(
                                r.rect,
                                CornerRadius::same(2),
                                egui::Stroke::new(1.5, t.red),
                                egui::StrokeKind::Outside,
                            );
                        }
                        if r.lost_focus() {
                            ui.data_mut(|m| m.remove::<DashDraft>(id));
                            // As the class label box does, Escape drops what was typed.
                            let escaped = ui.input(|i| i.key_pressed(egui::Key::Escape));
                            if let Some(dash) = dash_commit(typed || r.changed(), escaped, &text, st.dash) {
                                set(app, LineStroke { dash, ..st });
                            }
                        }
                        let preset = egui::ComboBox::from_id_salt(("stroke_dash_preset", i)).selected_text("").width(24.0);
                        preset
                            .show_ui(ui, |ui| {
                                for (label, steps) in DASH_PRESETS {
                                    // Scaled by the stroke's width, so a thick line's dashes stay in proportion.
                                    let dash = steps.and_then(|s| Dash::new(&s.iter().map(|v| v * w.max(1.0)).collect::<Vec<_>>()));
                                    if ui.selectable_label(dash == st.dash, tl!(label)).clicked() {
                                        set(app, LineStroke { dash, ..st });
                                    }
                                }
                            })
                            .response
                            .on_hover_text(tl!("Dash"));
                    });
                    ui.end_row();
                }
                if l.stroke_levels {
                    ui.label(tl!("Level"));
                    ui.horizontal(|ui| {
                        let mut lv = st.level_at(i);
                        let resp = ui
                            .add_enabled(
                                st.level.is_some(),
                                egui::DragValue::new(&mut lv).update_while_editing(false).range(0..=255).speed(0.1),
                            )
                            .on_hover_text(tl!("Level"));
                        if crate::widgets::changed(&resp) {
                            crate::widgets::one_step_per_press(app, &resp);
                            set(app, LineStroke { level: Some(lv), ..st });
                        }
                        let mut auto = st.level.is_none();
                        let tip = tl!("Draw at its place in the stack, the bottom stroke first");
                        if ui.checkbox(&mut auto, tl!("Auto")).on_hover_text(tip).changed() {
                            set(app, LineStroke { level: if auto { None } else { Some(lv) }, ..st });
                        }
                    });
                    ui.end_row();
                }
            });
        }
        ui.add_space(4.0);
        if class.is_some() {
            let back = ui.add_enabled(!borrowed, egui::Button::new(tl!("Use the Layer's Strokes")).small());
            if back.on_hover_text(tl!("Remove this class's own strokes and draw it with the layer's")).clicked() {
                set_strokes(app, class, &[]);
            }
        } else if ui
            .small_button(tl!("Use a Single Stroke"))
            .on_hover_text(tl!("Remove the strokes and draw the symbol's line alone"))
            .clicked()
        {
            set_strokes(app, None, &[]);
        }
    });
}

// ---------------------------------------------------------------------------------------------
// History and Details

pub(crate) fn history(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    if crate::sessionlog::tabs(app, ui) {
        return crate::sessionlog::pane(app, ui, t);
    }
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
            if app.session.dirty() || app.session.path.is_none() {
                app.session.notify(tl!("Save the project too: the script opens its saved file, without changes made since."), false);
            }
        }
        Err(e) => app.session.notify(e, true),
    }
}

/// Shows the selected layer's source metadata alongside its internal longitude/latitude extent.
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
            // A web or database address without its keys, tokens or password.
            ("Source", l.dataset.source.as_deref().map_or_else(|| tl!("In project").into(), rustgis_engine::sources::redact)),
            ("Source coordinate system", l.dataset.crs.map(|c| c.label()).unwrap_or_else(|| "EPSG:4326".into())),
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
    let key = ui.scope_id().with(("custom_stretch_rgb", layer));
    let pending = ui.data(|d| d.get_temp::<(Bounds, Bounds)>(key)).filter(|(base, _)| *base == shown);
    let mut edit = pending.map_or(shown, |(_, e)| e);
    let (mut done, mut changed) = (false, false);
    for (name, pair) in [tl!("Red"), tl!("Green"), tl!("Blue")].into_iter().zip(edit.iter_mut()) {
        let [mut lo, mut hi] = *pair;
        let speed = ((hi - lo).abs() / 200.0).clamp(1e-6, 1e6);
        let typed_from = [lo, hi];
        ui.horizontal(|ui| {
            ui.add_sized([44.0, 18.0], egui::Label::new(name));
            let input_label = ui.label(tl!("Min"));
            let a = ui.add(egui::DragValue::new(&mut lo).update_while_editing(false).speed(speed)).labelled_by(input_label.id);
            let input_label = ui.label(tl!("Max"));
            let b = ui.add(egui::DragValue::new(&mut hi).update_while_editing(false).speed(speed)).labelled_by(input_label.id);
            // Escape drops a number typed (`widgets::escaped`).
            if crate::widgets::escaped(&a) {
                lo = typed_from[0];
            }
            if crate::widgets::escaped(&b) {
                hi = typed_from[1];
            }
            done |= a.drag_stopped()
                || b.drag_stopped()
                || ((crate::widgets::changed(&a) || crate::widgets::changed(&b)) && !a.dragged() && !b.dragged());
            changed |= crate::widgets::changed(&a) || crate::widgets::changed(&b);
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
            let input_label = ui.label(tl!(label));
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
                })
                .response
                .labelled_by(input_label.id);
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
                let input_label = ui.label(tl!("Method"));
                egui::ComboBox::from_id_salt("raster_method")
                    .selected_text(tl!(method.label()))
                    .width(180.0)
                    .show_ui(ui, |ui| {
                        // Manual bounds are for Graduated Colors; a raster's are worked out.
                        for mm in methods_by_name().into_iter().filter(|m| *m != ClassMethod::Manual) {
                            if ui.selectable_label(mm == m, tl!(mm.label())).clicked() {
                                let _ = app.run(
                                    "raster.symbology",
                                    json!({"type": "classify", "method": format!("{mm:?}"), "classes": k, "ramp": r}),
                                );
                            }
                        }
                    })
                    .response
                    .labelled_by(input_label.id);
            });
            ui.horizontal(|ui| {
                let input_label = ui.label(tl!("Classes"));
                let mut kk = k;
                let resp = ui.add(egui::DragValue::new(&mut kk).update_while_editing(false).range(1..=32)).labelled_by(input_label.id);
                if crate::widgets::changed(&resp) {
                    crate::widgets::one_step_per_press(app, &resp);
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
                    let input_label = ui.label(tl!(label));
                    let mut x = v;
                    // Kept across a drag until it is let go (#607).
                    let drag = Dragged::lend(ui, egui::Id::unique(("relief_drag", l.id, which)), &mut x);
                    let r = ui
                        .add(egui::DragValue::new(&mut x).update_while_editing(false).range(range).speed(speed).max_decimals(3))
                        .labelled_by(input_label.id);
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
            let input_label = ui.label(tl!("Stretch type"));
            egui::ComboBox::from_id_salt("raster_stretch")
                .selected_text(tl!(style.stretch.label()))
                .width(170.0)
                .show_ui(ui, |ui| {
                    for (code, label) in Stretch::KINDS {
                        if ui.selectable_label(style.stretch.code() == code, tl!(label)).clicked() {
                            let _ = app.run("raster.stretch", json!({"type": code}));
                        }
                    }
                })
                .response
                .labelled_by(input_label.id);
        });
        match style.stretch {
            Stretch::PercentClip { low, high } => {
                ui.horizontal(|ui| {
                    let (mut lo, mut hi) = (low, high);
                    // Each kept across a drag until it is let go (#607); both are sent together.
                    let input_label = ui.label(tl!("Min %"));
                    let drag = Dragged::lend(ui, egui::Id::unique(("percent_clip_drag", l.id, "low")), &mut lo);
                    let a = ui
                        .add(egui::DragValue::new(&mut lo).update_while_editing(false).range(0.0..=49.0).speed(0.1).max_decimals(2))
                        .labelled_by(input_label.id);
                    let low_settled = drag.settled(ui, &a, &mut lo);
                    let input_label = ui.label(tl!("Max %"));
                    let drag = Dragged::lend(ui, egui::Id::unique(("percent_clip_drag", l.id, "high")), &mut hi);
                    let b = ui
                        .add(egui::DragValue::new(&mut hi).update_while_editing(false).range(0.0..=49.0).speed(0.1).max_decimals(2))
                        .labelled_by(input_label.id);
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
                let key = ui.scope_id().with(("custom_stretch", l.id));
                let pending = ui.data(|d| d.get_temp::<([f64; 2], [f64; 2])>(key)).filter(|(base, _)| *base == [min, max]);
                let [mut lo, mut hi] = pending.map_or([min, max], |(_, edit)| edit);
                let speed = ((max - min).abs() / 200.0).clamp(1e-6, 1e6);
                let typed_from = [lo, hi];
                ui.horizontal(|ui| {
                    let input_label = ui.label(tl!("Min"));
                    let a = ui.add(egui::DragValue::new(&mut lo).update_while_editing(false).speed(speed)).labelled_by(input_label.id);
                    let input_label = ui.label(tl!("Max"));
                    let b = ui.add(egui::DragValue::new(&mut hi).update_while_editing(false).speed(speed)).labelled_by(input_label.id);
                    // Escape drops a number typed (`widgets::escaped`).
                    if crate::widgets::escaped(&a) {
                        lo = typed_from[0];
                    }
                    if crate::widgets::escaped(&b) {
                        hi = typed_from[1];
                    }
                    let done = a.drag_stopped()
                        || b.drag_stopped()
                        || ((crate::widgets::changed(&a) || crate::widgets::changed(&b)) && !a.dragged() && !b.dragged());
                    if lo.is_finite() && hi.is_finite() && lo < hi && done {
                        ui.data_mut(|d| d.remove::<([f64; 2], [f64; 2])>(key));
                        let _ = app.run("raster.stretch", json!({"type": "custom", "min": lo, "max": hi}));
                    } else if crate::widgets::changed(&a) || crate::widgets::changed(&b) {
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
                    let input_label = ui.label(tl!("Deviations"));
                    // Kept across a drag until it is let go (#607).
                    let drag = Dragged::lend(ui, egui::Id::unique(("deviations_drag", l.id)), &mut nn);
                    let a = ui
                        .add(egui::DragValue::new(&mut nn).update_while_editing(false).range(0.1..=10.0).speed(0.05).max_decimals(2))
                        .labelled_by(input_label.id);
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
        let input_label = ui.label(tl!("Resampling"));
        egui::ComboBox::from_id_salt("raster_resampling")
            .selected_text(tl!(style.resampling.label()))
            .width(170.0)
            .show_ui(ui, |ui| {
                for r in Resampling::ALL {
                    if ui.selectable_label(r == style.resampling, tl!(r.label())).clicked() {
                        let _ = app.run("raster.resampling", json!({"type": format!("{r:?}")}));
                    }
                }
            })
            .response
            .labelled_by(input_label.id);
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
        let input_label = ui.label(tl!("Transparency"));
        ui.spacing_mut().slider_width = 150.0;
        let r =
            ui.add(egui::Slider::new(&mut tr, 0.0..=100.0).update_while_editing(false).suffix(" %").integer()).labelled_by(input_label.id);
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
            let key = ui.scope_id().with(("mask_values", l.id));
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
                let input_label = ui.label(tl!("Range"));
                let shown = modes.iter().find(|(m, _)| *m == cur).map(|(_, label)| label.to_string()).unwrap_or_default();
                egui::ComboBox::from_id_salt(("raster_mask_range", l.id))
                    .selected_text(shown)
                    .width(190.0)
                    .show_ui(ui, |ui| {
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
                    })
                    .response
                    .labelled_by(input_label.id);
            });
            if ranged {
                // As with a custom stretch: kept here until both bounds are valid.
                let (min, max) = (mask.min.unwrap_or(f64::NEG_INFINITY), mask.max.unwrap_or(f64::INFINITY));
                let key = ui.scope_id().with(("mask_range", l.id));
                let pending = ui.data(|d| d.get_temp::<([f64; 2], [f64; 2])>(key)).filter(|(base, _)| *base == [min, max]);
                let [mut lo, mut hi] = pending.map_or([min, max], |(_, edit)| edit);
                let span = if min.is_finite() && max.is_finite() { (max - min).abs() } else { 100.0 };
                let speed = (span / 200.0).clamp(1e-6, 1e6);
                let typed_from = [lo, hi];
                ui.horizontal(|ui| {
                    let input_label = ui.label(tl!("Min"));
                    let a = ui.add(egui::DragValue::new(&mut lo).update_while_editing(false).speed(speed)).labelled_by(input_label.id);
                    let input_label = ui.label(tl!("Max"));
                    let b = ui.add(egui::DragValue::new(&mut hi).update_while_editing(false).speed(speed)).labelled_by(input_label.id);
                    // Escape drops a number typed (`widgets::escaped`).
                    if crate::widgets::escaped(&a) {
                        lo = typed_from[0];
                    }
                    if crate::widgets::escaped(&b) {
                        hi = typed_from[1];
                    }
                    let done = a.drag_stopped()
                        || b.drag_stopped()
                        || ((crate::widgets::changed(&a) || crate::widgets::changed(&b)) && !a.dragged() && !b.dragged());
                    if lo <= hi && done {
                        ui.data_mut(|d| d.remove::<([f64; 2], [f64; 2])>(key));
                        let bound = |v: f64| if v.is_finite() { json!(v) } else { Value::Null };
                        let _ = app.run("raster.mask", json!({"min": bound(lo), "max": bound(hi)}));
                    } else if crate::widgets::changed(&a) || crate::widgets::changed(&b) {
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
    if ui.add_enabled(app.session.layer_clipboard.is_some(), egui::Button::new(paste_label(app))).clicked() {
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

    /// Run starts a built-in tool on a thread of its own: the pane shows it running, with Cancel,
    /// and its result lands as gp.run's would, with its layer and history.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_geoprocessing_pane_runs_a_tool_in_the_background() {
        use egui_kittest::kittest::Queryable;
        let mut app = RustGisApp::with_sample();
        let _ = app.run("ui.gp.open", json!({"tool": "buffer"}));
        app.gp.values.insert("input", "Cities".into());
        app.gp.values.insert("distance", "50".into());
        app.ws.recording = Some(Vec::new());
        let layers = app.session.map().layers.len();
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(420.0, 1000.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                poll_tool_run(app, ui.ctx());
                geoprocessing(app, ui, &Tokens::light());
            },
            app,
        );
        h.run();
        h.get_by_label("Run").click();
        h.step();
        assert!(h.state().gp.tool_run.is_some(), "it runs in the background");
        let end = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while h.state().gp.tool_run.is_some() {
            assert!(std::time::Instant::now() < end, "the run never landed");
            std::thread::sleep(std::time::Duration::from_millis(10));
            h.step();
        }
        let app = h.state();
        assert_eq!(app.gp.last.as_ref().map(|l| l.0), Some(true), "{:?}", app.gp.last);
        assert_eq!(app.session.map().layers.len(), layers + 1);
        assert_eq!(app.session.project.gp_history.last().map(|r| (r.ok, r.tool.as_str())), Some((true, "Buffer")));
        // Recorded once, as its run.
        let steps = app.ws.recording.as_deref().unwrap_or_default();
        assert_eq!(steps.iter().map(|s| s.command.as_str()).collect::<Vec<_>>(), ["gp.run"]);
        assert_eq!((&steps[0].params["tool"], &steps[0].params["params"]["input"]), (&json!("buffer"), &json!("Cities")));
    }

    /// The tool running stays in view, with its Cancel button, when the pane goes back to the
    /// list of tools.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_tool_running_in_the_background_can_be_cancelled_from_the_tool_list() {
        use egui_kittest::kittest::Queryable;
        let mut app = RustGisApp::with_sample();
        let run = rustgis_engine::cmd::gp::tool_start(&mut app.session, "buffer", &json!({"input": "Cities", "distance": 50})).unwrap();
        let control = rustgis_engine::analysis::control::Control::new();
        app.gp.tool_run = Some(ToolJob { id: run.id, tool: run.tool, slot: crate::Slot::default(), control: control.clone() });
        app.gp.tool = None;
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(420.0, 1000.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                geoprocessing(app, ui, &Tokens::light());
            },
            app,
        );
        // Its spinner keeps repainting: steps, not run.
        h.run_steps(3);
        h.get_by_label("Cancel").click();
        h.run_steps(2);
        assert!(control.is_cancelled());
    }

    #[test]
    fn field_source_uses_the_longest_input_prefix() {
        use rustgis_engine::analysis::{ParamSpec, ToolSpec};
        const fn layer(name: &'static str) -> ParamSpec {
            ParamSpec { name, label: name, kind: ParamKind::VectorLayer, required: true, default: "", help: "" }
        }
        const PARAMS: &[ParamSpec] = &[layer("input"), layer("input_points"), layer("denominator")];
        let spec = ToolSpec {
            id: "field_sources",
            name: "",
            toolbox: "",
            summary: "",
            params: PARAMS,
            run: rustgis_engine::analysis::external_placeholder,
        };
        assert_eq!(layer_for_param(&spec, "input_points_weight_field").map(|p| p.name), Some("input_points"));
        assert_eq!(layer_for_param(&spec, "denominator_weight_field").map(|p| p.name), Some("denominator"));
        assert_eq!(layer_for_param(&spec, "denominators_field").map(|p| p.name), Some("input"), "prefix needs an underscore boundary");
        assert_eq!(layer_for_param(&spec, "weight_field").map(|p| p.name), Some("input"));
    }

    /// A `FieldNames` list is split at `,` (and by some runners at `;`), so a name holding either
    /// isn't offered for it; a single field takes any name.
    #[test]
    fn field_name_lists_leave_out_names_holding_a_separator() {
        let opts = rustgis_engine::data::io::CsvOptions { delimiter: Some(b'\t'), ..Default::default() };
        let (d, _) = rustgis_engine::data::io::read_csv_with("last,first\ta;b\tname\nx\ty\tz\n", "t", &opts).unwrap();
        let names = |kind| (0..d.fields.len()).filter(|&i| takes_field(kind, &d, i)).map(|i| d.fields[i].name.as_str()).collect::<Vec<_>>();
        assert_eq!(names(ParamKind::FieldNames), ["name"]);
        assert_eq!(names(ParamKind::Field), ["last,first", "a;b", "name"]);
    }

    #[test]
    fn field_suggestions_append_without_erasing_typed_names() {
        use egui_kittest::kittest::Queryable;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(400.0, 150.0)).build_ui_state(
            |ui, value: &mut String| {
                field_suggestions(ui, egui::Id::unique("fields"), value, "Fields", "", ["name", "population"].into_iter(), true);
            },
            String::from("new_column"),
        );
        h.run_steps(3);
        assert_eq!(h.state(), "new_column", "a name absent from the input is not cleared");
        h.get_by_role(egui::accesskit::Role::ComboBox).click();
        h.run_steps(3);
        h.get_by_label("population").click();
        h.run_steps(3);
        assert_eq!(h.state(), "new_column,population", "append without replacing earlier names");
        h.get_by_role(egui::accesskit::Role::TextInput).focus();
        h.run_steps(2);
        h.key_press(egui::Key::End);
        h.event(egui::Event::Text(",typed_column".into()));
        h.run_steps(3);
        assert_eq!(h.state(), "new_column,population,typed_column");
    }

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

    /// A dash being typed shows only on the stroke of the layer and class it was typed for.
    #[test]
    fn a_dash_draft_stays_with_its_layer_and_class() {
        let draft = || Some((3, Some(ClassRef::Index(1)), "4 2".to_string()));
        assert_eq!(draft_for(draft(), &(3, Some(ClassRef::Index(1)))).as_deref(), Some("4 2"));
        // Another layer, another class, or the layer's own strokes show their own pattern.
        assert_eq!(draft_for(draft(), &(4, Some(ClassRef::Index(1)))), None);
        assert_eq!(draft_for(draft(), &(3, Some(ClassRef::Index(2)))), None);
        assert_eq!(draft_for(draft(), &(3, None)), None);
        assert_eq!(draft_for(None, &(3, None)), None);
    }

    /// The dash box shows a pattern as kept, and sets one only when it was typed and not escaped:
    /// focusing and leaving it never rounds the stroke's pattern.
    #[test]
    fn the_dash_box_sets_only_a_typed_pattern() {
        let kept = Dash::new(&[5.332, 2.666]);
        let text = dash_text(&kept.unwrap());
        assert_eq!(text, "5.332 2.666");
        assert_eq!(parse_dash(&text), Some(kept), "the shown text reads back as the same pattern");
        assert_eq!(dash_commit(false, false, &text, kept), None, "focused and left");
        assert_eq!(dash_commit(false, false, "4 2", kept), None, "not typed");
        assert_eq!(dash_commit(true, true, "4 2", kept), None, "Escape drops it");
        assert_eq!(dash_commit(true, false, "4 2", kept), Some(Dash::new(&[4.0, 2.0])));
        assert_eq!(dash_commit(true, false, "", kept), Some(None), "emptied: solid");
        assert_eq!(dash_commit(true, false, "4 x", kept), None, "not a pattern");
        assert_eq!(dash_commit(true, false, "5.332 2.666", kept), None, "typed back as it was");
    }

    /// The Strokes for picker lists a value listed twice once, for its first class (the one its
    /// features draw with), then `<all other values>`.
    #[test]
    fn stroke_classes_list_a_repeated_value_once() {
        let (a, b) = (
            Symbol { stroke_width: 1.0, ..Symbol::default_for(GeometryType::Polyline, 0) },
            Symbol { stroke_width: 2.0, ..Symbol::default_for(GeometryType::Polyline, 0) },
        );
        let r = Renderer::Unique {
            field: "kind".into(),
            classes: vec![("road".into(), a), ("rail".into(), b), ("road".into(), b)],
            other: b,
            ramp: Ramp::default(),
            class_labels: Default::default(),
            other_label: None,
            class_strokes: Default::default(),
            other_strokes: Vec::new(),
            class_fills: Default::default(),
            other_fills: Vec::new(),
            expression: None,
        };
        let listed = stroke_classes(&r);
        let refs: Vec<&ClassRef> = listed.iter().map(|(c, ..)| c).collect();
        assert_eq!(refs, [&ClassRef::Value("road".into()), &ClassRef::Value("rail".into()), &ClassRef::Other]);
        assert_eq!(listed.first().map(|(.., s)| s.stroke_width), Some(1.0));
    }

    fn layer(app: &RustGisApp, name: &str) -> u32 {
        app.session.map().layer_by_name(name).map(|l| l.id).unwrap_or_default()
    }

    fn drag_layer(h: &mut egui_kittest::Harness<'_, RustGisApp>, from: egui::Pos2, to: egui::Pos2) {
        use egui::{Event, Modifiers, PointerButton};
        let button = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        h.event(Event::PointerMoved(from));
        h.step();
        h.event(button(from, true));
        h.step();
        h.event(Event::PointerMoved(from + vec2(10.0, 0.0)));
        h.step();
        h.event(Event::PointerMoved(to));
        h.step();
        h.event(button(to, false));
        h.step();
    }

    /// #808: a web tile layer is listed at its place among the map's layers, moves past them
    /// from its menu, and is dragged to another place.
    #[test]
    fn a_tile_layer_is_listed_and_moved_among_the_layers() {
        use egui_kittest::kittest::Queryable;
        let mut app = RustGisApp::with_sample();
        for c in ["Cities", "Countries"] {
            app.run("layer.expand", json!({"layer": c, "on": false})).unwrap();
        }
        app.run("map.addTiles", json!({"url": "https://wms.example.org/{z}/{x}/{y}.png", "name": "Depths", "overlay": true})).unwrap();
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 900.0)).with_max_steps(64).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app,
        );
        h.run_steps(6);
        let y = |h: &egui_kittest::Harness<'_, RustGisApp>, name: &str| h.get_by_label(name).rect().center().y;
        let names = |h: &egui_kittest::Harness<'_, RustGisApp>| {
            let m = h.state().session.map();
            m.stack()
                .into_iter()
                .map(|d| match d {
                    Drawn::Layer(i) => m.layers[i].name.clone(),
                    Drawn::Tile(i) => m.tiles[i].name.clone(),
                })
                .collect::<Vec<_>>()
        };
        // Added on top: listed above Cities.
        assert!(y(&h, "Depths") < y(&h, "Cities"));
        // Its menu moves it down, past Cities.
        h.get_by_label("Depths").click_secondary();
        h.run_steps(2);
        h.get_by_label("Move down").click();
        h.run_steps(4);
        assert_eq!(names(&h), ["Cities", "Depths", "Rivers", "Lakes", "Countries"]);
        assert!(y(&h, "Cities") < y(&h, "Depths") && y(&h, "Depths") < y(&h, "Rivers"));
        // Dragged below Lakes.
        let (from, lakes) = (h.get_by_label("Depths").rect().center(), h.get_by_label("Lakes").rect());
        drag_layer(&mut h, from, pos2(lakes.center().x, lakes.max.y - 2.0));
        h.run_steps(2);
        assert_eq!(names(&h), ["Cities", "Rivers", "Lakes", "Depths", "Countries"]);
        assert!(y(&h, "Lakes") < y(&h, "Depths") && y(&h, "Depths") < y(&h, "Countries"));
    }

    /// A vector archive's source layers open under its row, each with a checkbox that switches
    /// it off and on again (`map.setTiles` with `hiddenLayers`, an edit to undo).
    #[test]
    fn a_vector_archives_source_layers_open_under_it() {
        use egui_kittest::kittest::{NodeT, Queryable};
        use rustgis_engine::render::rustgis_tiles::{mvt, pmtiles};
        let new = pmtiles::NewArchive {
            tile_type: pmtiles::TileType::Mvt,
            gzipped_tiles: false,
            bounds: [-180.0, -85.0, 180.0, 85.0],
            // `water` twice: listed once.
            metadata: json!({"vector_layers": [{"id": "water"}, {"id": "roads"}, {"id": "water"}]}),
        };
        let dir = std::env::temp_dir().join(format!("rustgis-ui-source-layers-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("town.pmtiles");
        std::fs::write(&path, pmtiles::write(vec![(0, 0, 0, mvt::encode(&mvt::Tile::default()))], &new).unwrap()).unwrap();
        let mut app = RustGisApp::with_sample();
        for c in ["Cities", "Countries"] {
            app.run("layer.expand", json!({"layer": c, "on": false})).unwrap();
        }
        app.run("layer.addData", json!({"path": path.display().to_string()})).unwrap();
        // The archive's header arrives in the background.
        for _ in 0..500 {
            if rustgis_engine::render::tile_source_layers(&app.session.map().tiles[0]).is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 900.0)).with_max_steps(64).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app,
        );
        h.run_steps(4);
        assert!(h.query_by_role_and_label(egui::Role::CheckBox, "roads").is_none(), "closed at first");
        h.get_by_label("Show source layers").click();
        h.run_steps(4);
        assert!(h.state().session.map().tiles[0].expanded);
        let ticked = |h: &egui_kittest::Harness<'_, RustGisApp>, name: &str| {
            h.get_by_role_and_label(egui::Role::CheckBox, name).accesskit_node().toggled() == Some(egui::accesskit::Toggled::True)
        };
        assert!(ticked(&h, "water") && ticked(&h, "roads"));
        assert_eq!(h.query_all_by_role_and_label(egui::Role::CheckBox, "water").count(), 1);
        // Listed under the archive's row, in the archive's order.
        let (row, water, roads) = (
            h.get_by_label("town").rect(),
            h.get_by_role_and_label(egui::Role::CheckBox, "water").rect(),
            h.get_by_role_and_label(egui::Role::CheckBox, "roads").rect(),
        );
        assert!(row.max.y <= water.min.y && water.max.y <= roads.min.y, "{row:?} {water:?} {roads:?}");
        h.get_by_role_and_label(egui::Role::CheckBox, "roads").click();
        h.run_steps(4);
        assert_eq!(h.state().session.map().tiles[0].hidden_layers, ["roads"]);
        assert!(ticked(&h, "water") && !ticked(&h, "roads"));
        assert_eq!(h.state().session.undo_label(), Some("Tile Layer Properties"));
        h.get_by_role_and_label(egui::Role::CheckBox, "roads").click();
        h.run_steps(4);
        assert!(h.state().session.map().tiles[0].hidden_layers.is_empty());
        h.get_by_label("Hide source layers").click();
        h.run_steps(4);
        assert!(h.query_by_role_and_label(egui::Role::CheckBox, "roads").is_none());
        // With a legend too, the arrow says it opens both (a local-network legend isn't fetched).
        h.state_mut().session.run("map.setTiles", &json!({"index": 0, "legend": "http://127.0.0.1/legend.png"})).unwrap();
        h.run_steps(4);
        assert!(h.query_by_label("Show legend").is_none() && h.query_by_label("Show source layers").is_none());
        h.get_by_label("Show legend and source layers").click();
        h.run_steps(4);
        assert!(h.query_by_label("Hide legend and source layers").is_some());
        assert!(h.query_by_role_and_label(egui::Role::CheckBox, "roads").is_some());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A tile layer's row is picked with a click, as a layer's is (the Selected in Layers pop-up
    /// mode identifies it), and clicking a layer's row picks that instead.
    #[test]
    fn a_tile_layers_row_is_picked_with_a_click() {
        use egui_kittest::kittest::Queryable;
        let mut app = RustGisApp::with_sample();
        app.run("map.addTiles", json!({"url": "https://wms.example.org/{z}/{x}/{y}.png", "name": "Depths", "overlay": true})).unwrap();
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 900.0)).with_max_steps(64).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app,
        );
        h.run_steps(6);
        h.get_by_label("Depths").click();
        h.run_steps(2);
        let depths = h.state().session.map().tiles.iter().position(|t| t.name == "Depths");
        assert_eq!((h.state().session.active_layer, h.state().session.selected_tile()), (None, depths));
        // Hidden by the filter, it isn't picked any more, as an active layer isn't.
        h.state_mut().ui.layer_filter = "Cit".into();
        h.run_steps(2);
        assert_eq!(h.state().session.selected_tile(), None);
        h.state_mut().ui.layer_filter.clear();
        h.run_steps(2);
        assert_eq!(h.state().session.selected_tile(), None, "not picked again");
        h.get_by_label("Depths").click();
        h.run_steps(2);
        assert_eq!(h.state().session.selected_tile(), depths);
        // An active layer the filter hides isn't made active again when a tile layer was picked
        // meanwhile by a command, as by a click.
        h.state_mut().run("layer.select", json!({"layer": "Cities"})).unwrap();
        h.state_mut().ui.layer_filter = "Dep".into();
        h.run_steps(2);
        assert_eq!(h.state().session.active_layer, None, "hidden");
        h.state_mut().run("map.selectTiles", json!({"name": "Depths"})).unwrap();
        h.state_mut().ui.layer_filter.clear();
        h.run_steps(2);
        assert_eq!((h.state().session.active_layer, h.state().session.selected_tile()), (None, depths));
        h.get_by_label("Cities").click();
        h.run_steps(2);
        let cities = h.state().session.map().layer_by_name("Cities").map(|l| l.id);
        assert_eq!((h.state().session.active_layer, h.state().session.selected_tile()), (cities, None));
    }

    /// #836: both halves of the whole visible block accept drops, including multi-class
    /// legends and the first/last layers. Group membership and one-step undo follow the drop.
    #[test]
    fn layer_drop_uses_whole_block_halves() {
        use egui_kittest::kittest::Queryable;
        for position in [0, 2, 3] {
            for grouped in [false, true] {
                for (expanded, fraction) in [(false, 0.25), (false, 0.75), (true, 0.25), (true, 0.49), (true, 0.51), (true, 0.75)] {
                    let mut app = RustGisApp::with_sample();
                    let source = if position == 0 { "Lakes" } else { "Cities" };
                    let (moved, countries) = (layer(&app, source), layer(&app, "Countries"));
                    if position != 3 {
                        app.run("layer.move", json!({"layer": countries, "to": position})).unwrap();
                    }
                    app.run("layer.symbology", json!({"layer": countries, "type": "unique", "field": "income_grp"})).unwrap();
                    app.session.map_mut().layer_mut(countries).unwrap().expanded = expanded;
                    if grouped {
                        app.run("group.add", json!({"layers": [countries], "name": "Base"})).unwrap();
                    }
                    let original: Vec<_> = app.session.map().layers.iter().map(|l| l.id).collect();
                    let undo = app.session.undo_depth();
                    let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 900.0)).with_max_steps(64).build_ui_state(
                        |ui, app: &mut RustGisApp| {
                            let ctx = ui.ctx().clone();
                            app.logic(&ctx);
                            app.ui(ui);
                        },
                        app,
                    );
                    h.run_steps(6);
                    let from = h.get_by_label(source).rect().center();
                    let mut block = h.get_by_label("Countries").rect();
                    if expanded {
                        let legend = h.state().session.map().layer(countries).unwrap().shown_legend(h.state().session.scale()).unwrap();
                        for entry in legend {
                            block.max.y = block.max.y.max(h.get_by_label(&entry.label).rect().max.y);
                        }
                    }
                    let to = pos2(block.center().x, block.min.y + block.height() * fraction);
                    drag_layer(&mut h, from, to);
                    let mut expected: Vec<_> = original.iter().copied().filter(|id| *id != moved).collect();
                    let target = expected.iter().position(|id| *id == countries).unwrap() + usize::from(fraction >= 0.5);
                    expected.insert(target, moved);
                    let map = h.state().session.map();
                    assert_eq!(
                        map.layers.iter().map(|l| l.id).collect::<Vec<_>>(),
                        expected,
                        "position={position}, grouped={grouped}, expanded={expanded}, fraction={fraction}"
                    );
                    assert_eq!(map.group_of(moved).is_some(), grouped && fraction < 0.5);
                    assert_eq!(h.state().session.undo_depth(), undo + 1);
                    h.state_mut().run("edit.undo", Value::Null).unwrap();
                    assert_eq!(h.state().session.map().layers.iter().map(|l| l.id).collect::<Vec<_>>(), original);
                    assert!(h.state().session.map().group_of(moved).is_none());
                }
            }
        }
    }

    /// The first visible row's upper half moves layers out of groups and moves groups whole.
    #[test]
    fn layers_and_groups_drop_above_the_first_row() {
        use egui_kittest::kittest::Queryable;
        for case in 0..4 {
            let mut app = RustGisApp::with_sample();
            if case != 0 {
                let members = if case == 2 { ["Lakes", "Countries"] } else { ["Cities", "Rivers"] };
                app.run("group.add", json!({"name": "Base", "layers": members})).unwrap();
            }
            let source = if case == 2 {
                "Base"
            } else if case == 3 {
                "Cities"
            } else {
                "Countries"
            };
            let original: Vec<_> = app.session.map().layers.iter().map(|l| l.id).collect();
            let members: Vec<_> = app.session.map().groups.iter().map(|g| g.children.clone()).collect();
            let moved: Vec<_> = if case == 2 { members.concat() } else { vec![layer(&app, source)] };
            let undo = app.session.undo_depth();
            let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 900.0)).with_max_steps(64).build_ui_state(
                |ui, app: &mut RustGisApp| {
                    let ctx = ui.ctx().clone();
                    app.logic(&ctx);
                    app.ui(ui);
                },
                app,
            );
            h.run_steps(6);
            let from = h.get_by_label(source).rect().center();
            let first = h.get_by_label(if case == 1 || case == 3 { "Base" } else { "Cities" }).rect();
            let to = pos2(first.center().x, first.min.y + first.height() * 0.125);
            drag_layer(&mut h, from, to);
            let expected: Vec<_> = original
                .iter()
                .copied()
                .filter(|id| moved.contains(id))
                .chain(original.iter().copied().filter(|id| !moved.contains(id)))
                .collect();
            assert_eq!(h.state().session.map().layers.iter().map(|l| l.id).collect::<Vec<_>>(), expected, "case={case}");
            if case != 2 {
                assert!(h.state().session.map().group_of(moved[0]).is_none());
            }
            assert_eq!(h.state().session.undo_depth(), undo + 1);
            h.state_mut().run("edit.undo", Value::Null).unwrap();
            assert_eq!(h.state().session.map().layers.iter().map(|l| l.id).collect::<Vec<_>>(), original);
            assert_eq!(h.state().session.map().groups.iter().map(|g| g.children.clone()).collect::<Vec<_>>(), members);
        }
    }

    /// #836: a group in another lists inside it, a level further in; dropped above a subgroup's
    /// row, a layer goes in the group holding it; onto a subgroup's row, into the subgroup; and
    /// Alt+Shift+arrows move the active layer in the outline.
    #[test]
    fn nested_groups_list_indented_and_take_drops_and_outline_keys() {
        use egui_kittest::kittest::Queryable;
        let mut app = RustGisApp::with_sample();
        app.run("group.add", json!({"name": "Base", "layers": ["Rivers", "Lakes", "Countries"]})).unwrap();
        app.run("group.add", json!({"name": "Water", "parent": "Base", "layers": ["Rivers", "Lakes"]})).unwrap();
        let (base, water) = {
            let g = |n: &str| app.session.map().groups.iter().find(|g| g.name == n).unwrap().id;
            (g("Base"), g("Water"))
        };
        let (cities, rivers, countries) = (layer(&app, "Cities"), layer(&app, "Rivers"), layer(&app, "Countries"));
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 900.0)).with_max_steps(64).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app,
        );
        h.run_steps(6);
        let x = |h: &egui_kittest::Harness<'_, RustGisApp>, n: &str| h.get_by_label(n).rect().min.x;
        assert!(x(&h, "Base") < x(&h, "Water") && x(&h, "Water") < x(&h, "Rivers"));
        // Above the subgroup's row: in Base, just above Water.
        let from = h.get_by_label("Countries").rect().center();
        let row = h.get_by_label("Water").rect();
        drag_layer(&mut h, from, pos2(row.center().x, row.min.y + row.height() * 0.125));
        let ids = |h: &egui_kittest::Harness<'_, RustGisApp>| h.state().session.map().layers.iter().map(|l| l.id).collect::<Vec<_>>();
        let order = ids(&h);
        assert_eq!(order.iter().position(|id| *id == countries), Some(1), "{order:?}");
        assert_eq!(h.state().session.map().group_of(countries).map(|g| g.id), Some(base));
        // Onto the subgroup's row: into it.
        h.run_steps(2);
        let from = h.get_by_label("Cities").rect().center();
        let row = h.get_by_label("Water").rect();
        drag_layer(&mut h, from, row.center());
        assert_eq!(h.state().session.map().group_of(cities).map(|g| g.id), Some(water));
        // Alt+Shift+Left takes the active layer out one level, into Base below Water.
        h.state_mut().session.active_layer = Some(rivers);
        h.run_steps(2);
        h.key_press_modifiers(egui::Modifiers::ALT | egui::Modifiers::SHIFT, egui::Key::ArrowLeft);
        h.run_steps(2);
        let m = h.state().session.map();
        assert_eq!(m.group_of(rivers).map(|g| g.id), Some(base));
        assert_eq!(m.layers.last().map(|l| l.id), Some(rivers));
        // A subgroup's row dragged above its group's: out to the top level, with its layers;
        // undone, back inside.
        let members = m.group_layers(water);
        let undo = h.state().session.undo_depth();
        h.run_steps(2);
        let from = h.get_by_label("Water").rect().center();
        let row = h.get_by_label("Base").rect();
        drag_layer(&mut h, from, pos2(row.center().x, row.min.y + row.height() * 0.125));
        let m = h.state().session.map();
        assert_eq!(m.group(water).and_then(|g| g.parent), None);
        assert_eq!(m.layers.iter().take(members.len()).map(|l| l.id).collect::<Vec<_>>(), members);
        assert_eq!(h.state().session.undo_depth(), undo + 1);
        h.state_mut().run("edit.undo", Value::Null).unwrap();
        assert_eq!(h.state().session.map().group(water).and_then(|g| g.parent), Some(base));
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

    /// A service's downloaded raster groups under the address it came from, not the file name its
    /// download was read under; a local raster still groups by its file.
    #[test]
    fn a_service_rasters_source_is_its_address() {
        use rustgis_engine::data::raster::{ColorHint, Crs, GeoTransform, Raster};
        let raster = || {
            let gt = GeoTransform { origin_x: 0.0, origin_y: 1.0, pixel_width: 1.0, pixel_height: -1.0 };
            Raster::new("soil", 1, 1, vec![vec![1.0]], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap()
        };
        let mut ds = rustgis_engine::data::Dataset::new("Soil carbon", rustgis_engine::data::GeometryType::None, Vec::new());
        ds.source = Some("https://user:secret@h/wcs?SERVICE=WCS&COVERAGEID=soc&token=K".into());
        let mut l = Layer::new(7, ds);
        l.raster = Some(rustgis_engine::map::RasterLayer::from_bytes(raster(), "coverage.tif", Vec::new()));
        let src = layer_source(&l).unwrap();
        assert_eq!(src.label, "h");
        assert!(!src.key.contains("secret") && !src.hover.contains("secret"));
        // A local file is grouped by its path, as before.
        let mut local = Layer::new(8, rustgis_engine::data::Dataset::new("Local", rustgis_engine::data::GeometryType::None, Vec::new()));
        local.raster = Some(rustgis_engine::map::RasterLayer::from_path(raster(), r"C:\GIS\soil.tif"));
        assert_eq!(layer_source(&local).unwrap().label, "soil.tif");
        // A raster read from an address is grouped by that address, not by another address its
        // dataset names.
        let mut remote = Layer::new(10, rustgis_engine::data::Dataset::new("Hill", rustgis_engine::data::GeometryType::None, Vec::new()));
        remote.dataset.source = Some("https://catalog.example/items?f=json".into());
        remote.raster = Some(rustgis_engine::map::RasterLayer::from_path(raster(), "https://files.example/hill.tif"));
        assert_eq!(layer_source(&remote).unwrap().label, "files.example");
        // A layer with no raster keeps its dataset source, even one that names no address scheme
        // (a database connection kept by name).
        let mut db = rustgis_engine::data::Dataset::new("Roads", rustgis_engine::data::GeometryType::Polyline, Vec::new());
        db.source = Some("osm".into());
        assert_eq!(layer_source(&Layer::new(9, db)).unwrap().label, "osm");
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
            h.state().ui.pane_rects[0].map(|r| r.width())
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

    #[test]
    fn mixed_geojson_exposes_polygon_fill_controls() {
        use egui_kittest::kittest::Queryable;
        let mut app = RustGisApp::new(Session::new(rustgis_engine::Project::default()));
        app.run(
            "layer.addDataText",
            json!({"fileName":"mixed.geojson", "text":r#"{"type":"FeatureCollection","features":[
            {"type":"Feature","properties":{},"geometry":{"type":"LineString","coordinates":[[0,0],[1,1]]}},
            {"type":"Feature","properties":{},"geometry":{"type":"LineString","coordinates":[[1,0],[2,1]]}},
            {"type":"Feature","properties":{},"geometry":{"type":"Polygon","coordinates":[[[0,0],[1,0],[1,1],[0,0]]]}}
        ]}"#}),
        )
        .unwrap();
        assert_eq!(app.session.map().layer(app.session.active_layer.unwrap()).unwrap().geometry_type(), GeometryType::Polyline);
        app.ui.open_right(RightPane::Symbology);
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1400.0, 900.0)).with_max_steps(64).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app,
        );
        h.run_steps(6);
        h.get_by_label("No Fill").click();
        h.run_steps(2);
        let s = &h.state().session;
        let l = s.map().layer(s.active_layer.unwrap()).unwrap();
        assert!(l.fill_layers.iter().all(|f| !f.draws(l.symbol(2).fill)));
        assert!(!l.fill_layers.is_empty());
        assert!(h.query_by_label("Add Fill").is_some());
    }

    fn symbology_retry_harness() -> egui_kittest::Harness<'static, RustGisApp> {
        let mut app = RustGisApp::new(Session::new(rustgis_engine::Project::default()));
        app.run(
            "layer.addDataText",
            json!({
                "fileName": "retry.geojson",
                "text": r#"{"type":"FeatureCollection","features":[
                    {"type":"Feature","properties":{"a_empty":null,"b_empty":null,"c_valid":10},"geometry":{"type":"Point","coordinates":[14,47]}},
                    {"type":"Feature","properties":{"a_empty":null,"b_empty":null,"c_valid":20},"geometry":{"type":"Point","coordinates":[15,48]}},
                    {"type":"Feature","properties":{"a_empty":100,"b_empty":200,"c_valid":30},"geometry":{"type":"Point","coordinates":[16,49]}}
                ]}"#
            }),
        )
        .unwrap();
        app.run("layer.definitionQuery", json!({"where": "c_valid < 30"})).unwrap();
        app.ui.open_right(RightPane::Symbology);
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1400.0, 900.0)).with_max_steps(64).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app,
        );
        h.run_steps(6);
        h
    }

    /// Unique Values class by the expression typed under the field; clearing it goes back to a field.
    #[test]
    fn unique_values_are_classed_by_the_expression_typed() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        let mut h = symbology_retry_harness();
        let id = layer(h.state(), "retry");
        h.state_mut().run("layer.symbology", json!({"type": "unique", "field": "c_valid"})).unwrap();
        h.run_steps(3);
        let steps = h.state().session.undo_depth();
        let enter = |h: &mut egui_kittest::Harness<'static, RustGisApp>, text: &str| {
            h.get_by_role_and_label(Role::TextInput, "Expression").focus();
            h.run();
            h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
            h.get_by_role_and_label(Role::TextInput, "Expression").type_text(text);
            h.run();
            h.key_press(egui::Key::Enter);
            h.run_steps(3);
        };
        let renderer = |h: &egui_kittest::Harness<'static, RustGisApp>| h.state().session.map().layer(id).unwrap().renderer.clone();
        enter(&mut h, "nosuch + 1");
        assert!(h.query_by_label_contains("Invalid:").is_some(), "says why");
        assert_eq!((renderer(&h).expression(), h.state().session.undo_depth()), (None, steps), "not applied");
        h.get_by_role_and_label(Role::TextInput, "Expression").focus();
        h.run();
        h.key_press(egui::Key::Escape);
        h.run();
        enter(&mut h, "if(c_valid > 15, 'high', 'low')");
        let Renderer::Unique { classes, expression, .. } = renderer(&h) else { panic!() };
        assert_eq!(expression.as_deref(), Some("if(c_valid > 15, 'high', 'low')"));
        assert_eq!(classes.iter().map(|c| c.0.as_str()).collect::<Vec<_>>(), ["high", "low"], "the rows the query keeps");
        assert_eq!(h.state().session.undo_depth(), steps + 1);
        assert!(h.query_all_by_role(Role::ComboBox).any(|n| n.value().as_deref() == Some("(expression)")));
    }

    /// A different populated field fixes classification without changing the query or styling on failure.
    #[test]
    fn failed_symbology_switch_retries_another_numeric_field() {
        use egui_kittest::kittest::Queryable;
        let mut h = symbology_retry_harness();
        let id = layer(h.state(), "retry");
        let before = h.state().session.map().layer(id).unwrap().renderer.clone();
        let undo = h.state().session.undo_depth();
        h.get_by_label("Graduated Colors").click();
        h.run_steps(3);
        assert_eq!(h.state().session.map().layer(id).unwrap().renderer, before);
        assert_eq!(h.state().session.undo_depth(), undo);
        assert!(h.query_all_by_label("'a_empty' has no numeric values").next().is_some());

        for field in ["b_empty", "c_valid"] {
            h.query_all_by_role(egui::accesskit::Role::ComboBox).find(|n| n.value().as_deref() == Some("Select a field")).unwrap().click();
            h.run_steps(3);
            h.get_by_label(field).click();
            h.run_steps(3);
            if field == "b_empty" {
                assert_eq!(h.state().session.map().layer(id).unwrap().renderer, before);
                assert_eq!(h.state().session.undo_depth(), undo);
                assert!(h.query_all_by_label("'b_empty' has no numeric values").next().is_some());
            }
        }
        let Renderer::Graduated { field, breaks, .. } = &h.state().session.map().layer(id).unwrap().renderer else {
            panic!("the populated field must apply Graduated Colors");
        };
        assert_eq!(field, "c_valid");
        assert_eq!(breaks.last(), Some(&20.0), "only the rows kept by the query are classified");
        assert_eq!(h.state().session.undo_depth(), undo + 1);
        assert!(h.query_all_by_label("Choose a field to apply this renderer:").next().is_none());
    }

    /// No numeric candidates means an explanation, not an empty field selector.
    #[test]
    fn failed_symbology_without_numeric_fields_has_no_empty_picker() {
        use egui_kittest::kittest::Queryable;
        let mut h = symbology_retry_harness();
        h.state_mut()
            .run(
                "layer.addDataText",
                json!({
                    "fileName": "text-only.geojson",
                    "text": r#"{"type":"Feature","properties":{"name":"Liezen"},"geometry":{"type":"Point","coordinates":[14,47]}}"#
                }),
            )
            .unwrap();
        h.run_steps(3);
        h.get_by_label("Graduated Colors").click();
        h.run_steps(3);
        assert!(h.query_all_by_label("no numeric field is available for this symbology").next().is_some());
        assert!(
            h.query_all_by_role(egui::accesskit::Role::ComboBox).all(|n| n.value().as_deref() != Some("Select a field")),
            "an empty picker cannot help choose a numeric field"
        );
        let id = layer(h.state(), "text-only");
        assert!(matches!(h.state().session.map().layer(id).unwrap().renderer, Renderer::Single(_)));
    }

    /// A failed switch is not offered again after the pane closes, or after a layer or map round
    /// trip back to the same layer id.
    #[test]
    fn pending_symbology_ends_with_the_pane_or_its_layer() {
        use egui_kittest::kittest::Queryable;
        let mut h = symbology_retry_harness();
        let map = h.state().session.map().name.clone();
        let id = layer(h.state(), "retry");
        h.state_mut()
            .run("layer.addDataText", json!({"fileName": "other.csv", "text": "x,y,name\n1,2,Other\n", "x": "x", "y": "y"}))
            .unwrap();
        let other = layer(h.state(), "other");
        h.state_mut().run("map.new", json!({"name": "Other map"})).unwrap();
        h.state_mut()
            .run("layer.addDataText", json!({"fileName": "collision.csv", "text": "x,y,name\n1,2,Other\n", "x": "x", "y": "y"}))
            .unwrap();
        assert_eq!(layer(h.state(), "collision"), id, "layer ids overlap across maps");
        h.state_mut().run("map.activate", json!({"name": map})).unwrap();
        h.state_mut().run("layer.select", json!({"layer": id})).unwrap();
        h.run_steps(3);

        for change in ["close the pane", "layer round trip", "map round trip"] {
            h.get_by_label("Graduated Colors").click();
            h.run_steps(3);
            assert!(h.query_all_by_label("Choose a field to apply this renderer:").next().is_some());
            match change {
                "close the pane" => {
                    h.state_mut().ui.right = None;
                    h.run_steps(3);
                    h.state_mut().ui.open_right(RightPane::Symbology);
                }
                "layer round trip" => {
                    h.state_mut().session.active_layer = Some(other);
                    h.run_steps(3);
                    h.state_mut().session.active_layer = Some(id);
                }
                _ => {
                    // The other map's active layer has the same id: only the map tells them apart.
                    h.state_mut().run("map.activate", json!({"name": "Other map"})).unwrap();
                    assert_eq!(h.state().session.active_layer, Some(id));
                    h.run_steps(3);
                    h.state_mut().run("map.activate", json!({"name": map})).unwrap();
                    h.state_mut().run("layer.select", json!({"layer": id})).unwrap();
                }
            }
            h.run_steps(3);
            assert!(h.query_all_by_label("Choose a field to apply this renderer:").next().is_none(), "{change}");
            assert!(matches!(h.state().session.map().layer(id).unwrap().renderer, Renderer::Single(_)));
        }
    }

    /// Both the existing field editor and commands from outside the pane supersede a failed switch.
    #[test]
    fn successful_symbology_edits_supersede_a_pending_switch() {
        use egui_kittest::kittest::Queryable;
        let mut h = symbology_retry_harness();
        let id = layer(h.state(), "retry");
        for edit in ["pane field", "ribbon", "agent"] {
            h.state_mut().run("layer.symbology", json!({"type": "unique", "field": "a_empty"})).unwrap();
            h.run_steps(3);
            h.get_by_label("Graduated Colors").click();
            h.run_steps(3);
            assert!(h.query_all_by_label("Choose a field to apply this renderer:").next().is_some());
            let params = json!({"type": "unique", "field": "c_valid"});
            match edit {
                "pane field" => {
                    h.query_all_by_role(egui::accesskit::Role::ComboBox).find(|n| n.value().as_deref() == Some("a_empty")).unwrap().click();
                    h.run_steps(3);
                    h.get_by_label("c_valid").click();
                }
                "ribbon" => drop(h.state_mut().run("layer.symbology", params).unwrap()),
                // An attached agent's command runs on the session, not through the app.
                _ => drop(h.state_mut().session.run("layer.symbology", &params).unwrap()),
            }
            h.run_steps(3);
            let Renderer::Unique { field, .. } = &h.state().session.map().layer(id).unwrap().renderer else {
                panic!("the successful edit must keep Unique Values");
            };
            assert_eq!(field, "c_valid");
            assert!(h.query_all_by_label("Choose a field to apply this renderer:").next().is_none());
        }
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

    /// A feature layer's menu converts its labels into an annotation layer (#858), as View ›
    /// Labeling › Convert does, by the command's name.
    #[test]
    fn the_layer_menu_converts_labels_to_annotation() {
        use egui_kittest::kittest::Queryable;
        let app = RustGisApp::with_sample();
        let layer = app.session.map().layers.iter().find(|l| l.name == "Cities").cloned().unwrap();
        let mut h = egui_kittest::Harness::builder().build_ui_state(|ui, app: &mut RustGisApp| layer_menu(app, ui, &layer, true), app);
        h.step();
        h.get_by_label("Convert To Annotation").click();
        h.step();
        let map = h.state().session.map();
        assert!(map.layer_by_name("Cities Annotation").is_some(), "no annotation layer");
        assert!(!map.layer_by_name("Cities").unwrap().labels.enabled, "the layer's own labels give way to it");
    }
}
