//! A WMS or WMTS layer's legend in the Layers pane (#942), as ArcGIS Pro's Contents pane shows a
//! map service layer's: under the layer, behind the same expand arrow a feature layer's classes
//! have, indented as they are, at its own size (a pixel a point; one wider than the pane scrolls
//! across). A WMS layer's legend is the one its service draws (GetLegendGraphic), its style's
//! picture if that can't be shown; a WMTS layer may give a legend per range of scales, and the
//! one for the map's scale shows.
//!
//! The pictures are fetched in the background when first shown (from addresses on the web, which
//! a layer from a project made elsewhere may not send into the local network), decoded within
//! bounds, and kept as textures (a few dozen; one that couldn't be fetched is asked for again a
//! minute later).

use std::collections::HashMap;

use egui::{RichText, Sense, Ui, vec2};
use rustgis_engine::map::tiles::TileLayer;

use crate::RustGisApp;
use crate::theme::Tokens;

/// A legend picture: on its way, shown, or why it couldn't be fetched (and when).
pub enum Pic {
    Loading(crate::Slot<Result<Vec<u8>, String>>),
    Ready(egui::TextureHandle, [u32; 2]),
    Failed(String, web_time::Instant),
}

/// A picture kept, and the pass it was last shown in.
pub struct Entry {
    pub pic: Pic,
    seen: u64,
}

/// The legend pictures fetched this session, by address.
#[derive(Default)]
pub struct Legends {
    pics: HashMap<String, Entry>,
}

impl Legends {
    /// Keeps `pic` for `url` (as shown now).
    #[cfg(test)]
    pub fn insert(&mut self, url: &str, pic: Pic) {
        self.pics.insert(url.to_string(), Entry { pic, seen: 0 });
    }
}

/// The most pictures kept: when more are wanted, those not shown on this pass make room.
const MAX_PICS: usize = 64;
/// The most bytes a legend picture may have.
const MAX_BYTES: usize = 4 << 20;
/// How long a picture that couldn't be fetched waits before it is asked for again.
const RETRY: std::time::Duration = std::time::Duration::from_secs(60);
/// Where a legend starts, as a feature layer's classes do (after the arrow, box and symbol).
pub const INDENT: f32 = 74.0;

/// What of a legend picture can be shown now.
enum Shown {
    Loading,
    Ready(egui::TextureId, [u32; 2]),
    Failed(String),
}

/// Shows the legend for the map's scale of `tile`, the tile layer at `index`, under its row: the
/// first of its legends there that can be shown.
pub fn show(app: &mut RustGisApp, ui: &mut Ui, index: usize, tile: &TileLayer, t: &Tokens) {
    let scale = app.session.map().scale();
    let urls: Vec<String> = rustgis_engine::data::ogc::legends_at(&tile.legends, scale).map(|l| l.url.clone()).collect();
    if urls.is_empty() {
        note(ui, tl!("No legend at this scale."), None, t);
        return;
    }
    let mut why = String::new();
    for url in urls {
        match pic(app, ui.ctx(), &url, &tile.url) {
            Shown::Loading => {
                note(ui, tl!("Loading the legend…"), None, t);
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(150));
                return;
            }
            Shown::Ready(id, [w, h]) => {
                picture(ui, index, &tile.name, id, vec2(w as f32, h as f32));
                return;
            }
            // The next, if there is one.
            Shown::Failed(e) => why = e,
        }
    }
    note(ui, tl!("The legend couldn't be loaded."), Some(&why), t);
}

/// The legend picture at `url` for the tile layer at `layer`: fetched when first asked for (again
/// a while after it couldn't be), and once landed decoded and made a texture.
fn pic(app: &mut RustGisApp, ctx: &egui::Context, url: &str, layer: &str) -> Shown {
    // The address comes from the service's capabilities or a project (hostile input): it must not
    // reach into the local network from a service on the web. Checked for each layer, as another
    // with the same legend may be allowed what this one isn't.
    if let Err(e) = rustgis_engine::render::rustgis_tiles::address::check(url, Some(layer)) {
        return Shown::Failed(e);
    }
    let pass = ctx.cumulative_pass_nr();
    let pics = &mut app.ui.tile_legends.pics;
    let retry = pics.get(url).is_some_and(|e| matches!(e.pic, Pic::Failed(_, at) if at.elapsed() >= RETRY));
    if retry || !pics.contains_key(url) {
        if !pics.contains_key(url) && pics.len() >= MAX_PICS {
            // Those not shown on this pass make room; when all are, it waits for a later pass.
            pics.retain(|_, e| e.seen == pass);
            if pics.len() >= MAX_PICS {
                return Shown::Failed(tl!("Too many legends are showing at once.").to_string());
            }
        }
        pics.insert(url.to_string(), Entry { pic: Pic::Loading(crate::fetch_into_limited(url, MAX_BYTES)), seen: pass });
    }
    let Some(entry) = pics.get_mut(url) else { return Shown::Loading };
    entry.seen = pass;
    if let Pic::Loading(slot) = &entry.pic {
        let landed = slot.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(r) = landed {
            // (A service's error answers XML, which isn't a picture.)
            entry.pic = match r.and_then(|bytes| crate::mapview::decode_picture(&bytes)) {
                Ok(img) => {
                    let size = [img.width() as usize, img.height() as usize];
                    let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
                    let texture = ctx.load_texture(format!("tile_legend:{url}"), color, egui::TextureOptions::LINEAR);
                    Pic::Ready(texture, [img.width(), img.height()])
                }
                Err(e) => Pic::Failed(e, web_time::Instant::now()),
            };
        }
    }
    match &entry.pic {
        Pic::Loading(_) => Shown::Loading,
        Pic::Ready(texture, size) => Shown::Ready(texture.id(), *size),
        Pic::Failed(e, _) => Shown::Failed(e.clone()),
    }
}

/// A legend picture `size` points across and down under the tile layer at `index` (named `name`,
/// for screen readers), indented as a feature layer's classes; one wider than the pane scrolls
/// across.
fn picture(ui: &mut Ui, index: usize, name: &str, id: egui::TextureId, size: egui::Vec2) {
    ui.horizontal(|ui| {
        ui.add_space(INDENT);
        let room = (ui.available_width() - 8.0).max(16.0);
        let image = |ui: &mut Ui| {
            ui.add(egui::Image::new(egui::load::SizedTexture::new(id, size)).sense(Sense::hover()))
                .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Image, true, crate::i18n::fmt("Legend: {name}", &[("name", name)])));
        };
        if size.x > room {
            // Its own scroll for each layer, even two showing the same picture.
            egui::ScrollArea::horizontal().id_salt(("tile_legend", index)).max_width(room).show(ui, image);
        } else {
            image(ui);
        }
    });
}

/// A short line in the legend's place: loading, none at this scale, or why it can't be shown.
fn note(ui: &mut Ui, text: &str, why: Option<&str>, t: &Tokens) {
    ui.horizontal(|ui| {
        ui.add_space(INDENT);
        let label = ui.label(RichText::new(text).size(11.0).color(t.text_dim));
        if let Some(why) = why {
            label.on_hover_text(why);
        }
    });
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use egui_kittest::kittest::Queryable;
    use serde_json::json;

    use super::*;

    fn harness(app: RustGisApp) -> egui_kittest::Harness<'static, RustGisApp> {
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1400.0, 1000.0)).with_max_steps(64).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app,
        );
        h.run_steps(4);
        h
    }

    /// A tile layer with a legend has the arrow a feature layer's classes have; it shows the
    /// legend under the layer (the picture, or why there is none) and hides it again, each a
    /// command.
    #[test]
    fn a_tile_layers_legend_opens_under_it() {
        let _turn = crate::fetching::turn();
        let mut app = RustGisApp::with_sample();
        for c in ["Cities", "Countries"] {
            app.session.run("layer.expand", &json!({"layer": c, "on": false})).unwrap();
        }
        // A layer without a legend has no arrow.
        app.session
            .run("map.addTiles", &json!({"url": "https://plain.example.org/{z}/{x}/{y}.png", "name": "Plain", "overlay": true}))
            .unwrap();
        // A web layer's legend in the local network isn't fetched: it says why it isn't there.
        let local = json!({"url": "https://tiles.example.org/{z}/{x}/{y}.png", "name": "Walls", "overlay": true, "legend": "http://127.0.0.1/legend.png"});
        app.session.run("map.addTiles", &local).unwrap();
        let mut h = harness(app);
        assert_eq!(h.query_all_by_label("Show legend").count(), 1, "only the layer with a legend");
        h.get_by_label("Show legend").click();
        h.run_steps(4);
        assert!(h.state().session.map().tiles.iter().any(|t| t.name == "Walls" && t.expanded));
        assert!(h.query_by_label("The legend couldn't be loaded.").is_some());
        // Its next legend, when it has one, is tried instead.
        let url = "https://tiles.example.org/legend.png";
        let texture =
            h.ctx.load_texture("test_legend", egui::ColorImage::filled([30, 12], egui::Color32::RED), egui::TextureOptions::LINEAR);
        h.state_mut().ui.tile_legends.insert(url, Pic::Ready(texture, [30, 12]));
        let both = json!({"name": "Walls", "legend": ["http://127.0.0.1/legend.png", url]});
        h.state_mut().session.run("map.setTiles", &both).unwrap();
        h.run_steps(4);
        let image = h.get_by_label("Legend: Walls");
        assert!(h.query_by_label("The legend couldn't be loaded.").is_none());
        // At its own size, indented as a feature layer's classes are.
        assert!((image.rect().width() - 30.0).abs() < 0.5 && (image.rect().height() - 12.0).abs() < 0.5, "{:?}", image.rect());
        let row = h.get_by_label("Walls").rect();
        assert!((image.rect().min.x - row.min.x - INDENT).abs() < 1.0, "{:?} {row:?}", image.rect());
        // None for the map's scale: said so.
        let elsewhere = json!({"name": "Walls", "legend": [{"url": url, "maxScale": 1}]});
        h.state_mut().session.run("map.setTiles", &elsewhere).unwrap();
        h.run_steps(4);
        assert!(h.query_by_label("No legend at this scale.").is_some());
        h.get_by_label("Hide legend").click();
        h.run_steps(4);
        assert!(h.query_by_label("No legend at this scale.").is_none());
        assert!(h.state().session.map().tiles.iter().any(|t| t.name == "Walls" && !t.expanded));
    }
}
