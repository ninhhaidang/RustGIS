//! The Project backstage: start a project (blank or from the sample), open, save and export.

use egui::{Align2, CornerRadius, Rect, Sense, Stroke, Ui, pos2, vec2};
use serde_json::{Value, json};

use crate::chrome::header_layout;
use crate::theme::{BRAND, Tokens, regular, semibold};
use crate::{RustGisApp, TitlebarMetrics, icons};

const PAGES: [(&str, &str, &str); 6] = [
    ("open", "Start", "logo"),
    ("new", "New", "new"),
    ("openfile", "Open", "open"),
    ("save", "Save", "save"),
    ("saveas", "Save As", "save_as"),
    ("about", "About", "about"),
];

/// Space above the navigation. In a unified title bar the window buttons float over the top-left
/// corner (there is no header here to clear them), so the Back control starts below the bar.
fn nav_top_pad(titlebar: Option<TitlebarMetrics>) -> f32 {
    titlebar.map_or(16.0, |m| header_layout(Some(m)).height)
}

pub fn show(app: &mut RustGisApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let top = nav_top_pad(app.ui.titlebar).round() as i8;
    egui::Panel::left("backstage_nav")
        .exact_size(210.0)
        .frame(egui::Frame::NONE.fill(BRAND).inner_margin(egui::Margin { left: 0, right: 0, top, bottom: 16 }))
        .show(ui, |ui| {
            let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
            icons::paint(
                ui.painter(),
                Rect::from_center_size(pos2(r.min.x + 30.0, r.center().y), vec2(22.0, 22.0)),
                "back",
                egui::Color32::WHITE,
                egui::Color32::WHITE,
            );
            if resp.on_hover_text(tl!("Back to the map")).clicked() {
                app.ui.backstage = false;
            }
            ui.add_space(12.0);
            for (id, label, icon) in PAGES {
                let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
                let sel = app.ui.backstage_page == id;
                if sel {
                    ui.painter().rect_filled(r, 0.0, egui::Color32::from_white_alpha(46));
                } else if resp.hovered() {
                    ui.painter().rect_filled(r, 0.0, egui::Color32::from_white_alpha(22));
                }
                if icon != "logo" {
                    icons::paint(
                        ui.painter(),
                        Rect::from_center_size(pos2(r.min.x + 30.0, r.center().y), vec2(18.0, 18.0)),
                        icon,
                        egui::Color32::WHITE,
                        egui::Color32::from_rgb(0xFF, 0xD3, 0x6E),
                    );
                }
                ui.painter().text(
                    pos2(r.min.x + 50.0, r.center().y),
                    Align2::LEFT_CENTER,
                    crate::i18n::t(label),
                    if sel { semibold(14.0) } else { regular(14.0) },
                    egui::Color32::WHITE,
                );
                if resp.clicked() {
                    match id {
                        "openfile" => {
                            let _ = app.run("ui.open", Value::Null);
                        }
                        "save" => {
                            if app.run("ui.save", Value::Null).is_ok() {
                                app.ui.backstage = false;
                            }
                        }
                        "saveas" => {
                            if app.run("ui.saveAs", Value::Null).is_ok() {
                                app.ui.backstage = false;
                            }
                        }
                        _ => app.ui.backstage_page = id,
                    }
                }
            }
        });
    egui::CentralPanel::default().frame(egui::Frame::NONE.fill(t.ribbon).inner_margin(egui::Margin::symmetric(48, 36))).show(ui, |ui| {
        egui::ScrollArea::vertical().show(ui, |ui| match app.ui.backstage_page {
            "about" => about(ui, &t),
            _ => start(app, ui, &t),
        });
    });
}

/// Draws the project start choices; import cards dispatch commands without replacing the project.
fn start(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(44.0, 44.0), Sense::hover());
        app.paint_logo(ui, r);
        ui.add_space(6.0);
        ui.label(egui::RichText::new(tl!("Welcome to RustGIS")).font(semibold(26.0)));
    });
    ui.label(egui::RichText::new(tl!("Create a new project or start from the World Atlas sample.")).color(t.text_dim));
    ui.add_space(24.0);
    ui.label(egui::RichText::new(tl!("New Project")).font(semibold(15.0)));
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing = vec2(18.0, 0.0);
        if card(ui, t, "Map", "A blank map with a basemap", "new_map") {
            let _ = app.run_by_user("project.new", Value::Null);
            if !crate::saveprompt::waiting(app) {
                app.ui.backstage = false;
                app.map.invalidate();
            }
        }
        if card(ui, t, "World Atlas", "Countries, cities, rivers and lakes", "basemap") {
            let _ = app.run_by_user("project.sample", Value::Null);
            if !crate::saveprompt::waiting(app) {
                app.ui.backstage = false;
                app.map.invalidate();
            }
        }
        if card(ui, t, "Open…", "Open a .rgis project", "open") {
            let _ = app.run("ui.open", Value::Null);
        }
        if card(ui, t, "GeoLibre Project…", "A .geolibre project as a map", "open") {
            let _ = app.run("ui.openItem", json!({"kind": "geolibreProject"}));
            app.ui.backstage = false;
        }
        if card(ui, t, "QGIS Project…", "A .qgs or .qgz project as a map", "open") {
            let _ = app.run("ui.openItem", json!({"kind": "qgisProject"}));
            app.ui.backstage = false;
        }
    });
    ui.add_space(28.0);
    ui.label(egui::RichText::new(tl!("Current Project")).font(semibold(15.0)));
    ui.add_space(6.0);
    egui::Grid::new("proj_info").num_columns(2).spacing(vec2(16.0, 6.0)).show(ui, |ui| {
        ui.label(egui::RichText::new(tl!("Name")).color(t.text_dim));
        ui.label(&app.session.project.name);
        ui.end_row();
        ui.label(egui::RichText::new(tl!("Location")).color(t.text_dim));
        ui.label(app.session.path.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| tl!("Not saved yet").into()));
        ui.end_row();
        ui.label(egui::RichText::new(tl!("Maps")).color(t.text_dim));
        ui.label(app.session.project.maps.len().to_string());
        ui.end_row();
        ui.label(egui::RichText::new(tl!("Layers")).color(t.text_dim));
        ui.label(app.session.project.maps.iter().map(|m| m.layers.len()).sum::<usize>().to_string());
        ui.end_row();
        ui.label(egui::RichText::new(tl!("Coordinate system")).color(t.text_dim));
        ui.label(app.session.project.coordinate_system.map_or_else(|| tl!("None").to_string(), |c| c.label()));
        ui.end_row();
    });
    ui.add_space(6.0);
    if ui.button(tl!("Project Properties…")).clicked() {
        let _ = app.run("ui.dialog", json!({"name": "projectProperties"}));
        app.ui.backstage = false;
    }
}

/// A project choice whose visible title is also its accessible button name.
fn card(ui: &mut Ui, t: &Tokens, title: &str, sub: &str, icon: &str) -> bool {
    let (r, resp) = ui.allocate_exact_size(vec2(200.0, 160.0), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), tl!(title)));
    ui.painter().rect_filled(r, CornerRadius::same(10), if resp.hovered() { t.hover } else { t.pane });
    ui.painter().rect_stroke(
        r,
        CornerRadius::same(10),
        Stroke::new(if resp.hovered() { 1.5 } else { 1.0 }, if resp.hovered() { t.accent } else { t.border }),
        egui::StrokeKind::Inside,
    );
    let thumb = Rect::from_min_size(r.min + vec2(12.0, 12.0), vec2(176.0, 86.0));
    ui.painter().rect_filled(thumb, CornerRadius::same(6), t.canvas);
    icons::paint(ui.painter(), Rect::from_center_size(thumb.center(), vec2(48.0, 48.0)), icon, t.icon, t.accent);
    ui.painter().text(pos2(r.min.x + 14.0, r.min.y + 116.0), Align2::LEFT_CENTER, tl!(title), semibold(13.5), t.text);
    ui.painter().text(pos2(r.min.x + 14.0, r.min.y + 136.0), Align2::LEFT_CENTER, tl!(sub), regular(11.5), t.text_dim);
    resp.clicked()
}

fn about(ui: &mut Ui, t: &Tokens) {
    ui.label(egui::RichText::new(tl!("About RustGIS")).font(semibold(24.0)));
    ui.add_space(8.0);
    ui.label(crate::i18n::fmt("Version {version}", &[("version", env!("CARGO_PKG_VERSION"))]));
    ui.add_space(8.0);
    crate::i18n::paragraph(
        ui,
        "RustGIS is a free and open-source GIS written in pure Rust. It runs on the desktop and in the browser, and every action is a command you can also run from the command line.",
        regular(12.5),
        t.text,
    );
    ui.add_space(4.0);
    crate::chrome::project_links(ui);
    ui.add_space(8.0);
    crate::i18n::paragraph(
        ui,
        "Sample data © Natural Earth (public domain). RustGIS is licensed under Apache-2.0.",
        regular(12.5),
        t.text_dim,
    );
    crate::chrome::licence_links(ui);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_nav_clears_the_window_buttons_only_in_a_unified_title_bar() {
        assert_eq!(nav_top_pad(None), 16.0);
        let short = TitlebarMetrics { height: 28.0, ..TitlebarMetrics::FALLBACK };
        assert_eq!(nav_top_pad(Some(short)), 34.0, "never less than the header height");
        let tall = TitlebarMetrics { height: 44.0, ..TitlebarMetrics::FALLBACK };
        assert_eq!(nav_top_pad(Some(tall)), 44.0);
    }
}
