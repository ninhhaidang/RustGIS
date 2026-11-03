//! The search box at the foot of the left pane: one box for a latitude and longitude, the map's
//! layers, bookmarks and attribute values (as one types), and places (on Enter: the public
//! geocoders don't allow searching as one types). A pick goes there; a place or a coordinate
//! is marked on the map until the box is cleared.

use egui::{Align2, Color32, CornerRadius, Rect, Sense, Stroke, Ui, pos2, vec2};
use rustgis_engine::cmd::search::{self, Hit, Results};
use rustgis_engine::geom::Coord;
use rustgis_engine::render::rustgis_tiles::geocode::{Geocoder, Place};
use serde_json::json;

use crate::theme::{Tokens, regular, semibold};
use crate::{RustGisApp, icons};

type Answer = crate::Slot<Result<Vec<u8>, String>>;

/// Seconds after the last keystroke before the map is searched.
const DEBOUNCE: f64 = 0.12;
/// Most places asked of the geocoder.
const PLACES: usize = 6;

/// Where a places search stands.
#[derive(Default, PartialEq)]
enum Places {
    /// Not asked for this query.
    #[default]
    Idle,
    Searching,
    Found(Vec<Place>),
    Failed(String),
}

/// The search box's text, what it found and the marker it left on the map.
#[derive(Default)]
pub struct State {
    pub query: String,
    /// The query the results are for.
    searched: String,
    edited_at: f64,
    results: Results,
    places: Places,
    pending: Option<(Geocoder, Answer)>,
    open: bool,
    /// The row picked, waiting a pass for the field that had the focus to set its number.
    held_pick: Option<Row>,
    /// The row the arrow keys are on, and whether the list scrolls to it.
    active: usize,
    scroll: bool,
    /// The pass the box last had the focus in: egui takes the focus away on Escape before the
    /// next pass, so that Escape is the box's when it had the focus one pass ago.
    pub focus_pass: Option<u64>,
    /// The place or coordinate picked last, and its name.
    pub marker: Option<(Coord, String)>,
}

/// One row of the list.
#[derive(Clone, Copy)]
enum Row {
    Hit(usize),
    Place(usize),
    SearchPlaces,
}

fn take(slot: &Answer) -> Option<Result<Vec<u8>, String>> {
    slot.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// Lands a places search.
fn poll(app: &mut RustGisApp, ctx: &egui::Context) {
    let st = &mut app.ui.search;
    let Some((g, slot)) = &st.pending else { return };
    match take(slot) {
        Some(r) => {
            st.places = match r.and_then(|b| g.parse(&b)) {
                Ok(p) => {
                    // The first place is the one Enter picks now.
                    if !p.is_empty() {
                        st.active = st.results.hits.len();
                        st.scroll = true;
                    }
                    Places::Found(p)
                }
                Err(e) => Places::Failed(e),
            };
            st.pending = None;
            st.open = true;
        }
        None => ctx.request_repaint_after(std::time::Duration::from_millis(100)),
    }
}

fn rows(st: &State) -> Vec<Row> {
    let mut v: Vec<Row> = (0..st.results.hits.len()).map(Row::Hit).collect();
    match &st.places {
        Places::Found(p) => v.extend((0..p.len()).map(Row::Place)),
        // The query the results are for, so the rows agree until the next search lands.
        Places::Idle if st.searched.trim().chars().count() >= 2 && !is_coordinate(st) => v.push(Row::SearchPlaces),
        _ => {}
    }
    v
}

/// Whether the box had the focus in the pass before `pass`. Stale when the box isn't drawn (the
/// left pane closed), so the shortcuts aren't held off by a box no longer shown.
pub fn had_focus(st: &State, pass: u64) -> bool {
    st.focus_pass.is_some_and(|p| p + 1 == pass)
}

fn is_coordinate(st: &State) -> bool {
    matches!(st.results.hits.first(), Some(Hit::Coordinate { .. }))
}

/// The box, at the bottom of the left pane (`ui` is the pane).
pub fn show(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    poll(app, ui.ctx());
    egui::Panel::bottom("search_box")
        .frame(egui::Frame::NONE.fill(t.pane).inner_margin(egui::Margin::symmetric(8, 7)))
        .show(ui, |ui| boxed(app, ui, t));
}

fn boxed(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    let now = ui.input(|i| i.time);
    let width = ui.available_width();
    let (frame_r, _) = ui.allocate_exact_size(vec2(width, 28.0), Sense::hover());
    let edit_r = Rect::from_min_max(pos2(frame_r.min.x + 26.0, frame_r.min.y + 2.0), pos2(frame_r.max.x - 24.0, frame_r.max.y - 2.0));
    let id = egui::Id::unique("map_search_box");
    let focused = ui.memory(|m| m.has_focus(id));
    // The arrows are the list's, taken before the text box sees them.
    let (down, up) = if focused {
        ui.input_mut(|i| {
            (i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown), i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp))
        })
    } else {
        (false, false)
    };
    let p = ui.painter().clone();
    p.rect_filled(frame_r, CornerRadius::same(6), t.input);
    p.rect_stroke(
        frame_r,
        CornerRadius::same(6),
        Stroke::new(1.0, if focused { t.accent } else { t.input_border }),
        egui::StrokeKind::Inside,
    );
    icons::paint(
        &p,
        Rect::from_center_size(pos2(frame_r.min.x + 14.0, frame_r.center().y), vec2(15.0, 15.0)),
        "search",
        t.text_dim,
        t.accent,
    );
    let st = &mut app.ui.search;
    let resp = ui.put(
        edit_r,
        egui::TextEdit::singleline(&mut st.query)
            .id(id)
            .frame(egui::Frame::NONE)
            .hint_text(tl!("Search layers, places, lat/lon…"))
            .font(regular(12.5))
            .desired_width(edit_r.width())
            .align(egui::Align2::LEFT_CENTER),
    );
    // Named for screen readers (and scripted clicks): the hint text isn't the box's name.
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, tl!("Search the map")));
    let resp = resp.on_hover_text(tl!(
        "A latitude and longitude (40.7, -74.0 or 40°42'N 74°0'W), a layer, a bookmark or a value in a layer's table. Enter picks the highlighted row; Search places looks the text up as a place"
    ));
    if resp.changed() {
        st.edited_at = now;
        st.open = true;
        st.active = 0;
        st.places = Places::Idle;
        st.pending = None;
    }
    if resp.gained_focus() && !st.query.is_empty() {
        st.open = true;
    }
    let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    // Escape (the text box gives up the focus on it) and the ✕ clear the box and its marker.
    let pass = ui.ctx().cumulative_pass_nr();
    let mut clear = had_focus(st, pass) && ui.input(|i| i.key_pressed(egui::Key::Escape));
    st.focus_pass = resp.has_focus().then_some(pass).or(st.focus_pass);
    if !st.query.is_empty() || st.marker.is_some() {
        let x = Rect::from_center_size(pos2(frame_r.max.x - 13.0, frame_r.center().y), vec2(18.0, 18.0));
        let xr = ui.interact(x, id.with("clear"), Sense::click());
        xr.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, tl!("Clear search")));
        if xr.hovered() {
            p.rect_filled(x, CornerRadius::same(4), t.hover);
        }
        icons::paint(&p, x.shrink(3.5), "close", t.text_dim, t.accent);
        if xr.on_hover_text(tl!("Clear search")).clicked() {
            clear = true;
        }
    }
    if clear {
        *st = State::default();
        app.map.invalidate();
        return;
    }
    // The map is searched a moment after the typing stops.
    if st.query != st.searched {
        if now - st.edited_at >= DEBOUNCE {
            st.searched = st.query.clone();
            st.results = search::search(&app.session, &st.query, search::LIMIT);
            st.active = 0;
        } else {
            ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(DEBOUNCE));
        }
    }
    let st = &mut app.ui.search;
    let list = rows(st);
    if down || up {
        st.scroll = true;
    }
    if down {
        st.open = true;
        st.active = (st.active + 1).min(list.len().saturating_sub(1));
    }
    if up {
        st.active = st.active.saturating_sub(1);
    }
    let mut picked = None;
    if enter && st.query == st.searched {
        picked = list.get(st.active).copied();
        // Enter keeps the box ready for the next query.
        resp.request_focus();
    }
    let show_list = st.open
        && !st.query.trim().is_empty()
        && (focused || resp.has_focus() || st.pending.is_some() || matches!(st.places, Places::Found(_) | Places::Failed(_)));
    if show_list && let Some(r) = list_area(app, ui, frame_r, &list, t) {
        picked = Some(r);
    }
    if let Some(r) = picked.or(app.ui.search.held_pick.take()) {
        // A number being typed elsewhere is set on its own layer before a pick can change the
        // active one: the field lets go now and the pick waits for the next pass (#1194).
        match crate::widgets::focused_at_pass_start(ui.ctx()).filter(|field| *field != id) {
            Some(field) => {
                ui.memory_mut(|m| m.surrender_focus(field));
                app.ui.search.held_pick = Some(r);
                ui.ctx().request_repaint();
            }
            None => pick(app, r),
        }
    }
}

/// The list over the map above the box; the row clicked, if any.
fn list_area(app: &mut RustGisApp, ui: &Ui, frame_r: Rect, list: &[Row], t: &Tokens) -> Option<Row> {
    let st = &app.ui.search;
    let searching = st.pending.is_some();
    let failed = if let Places::Failed(e) = &st.places { Some(e.clone()) } else { None };
    let none = st.query == st.searched && list.is_empty() && !searching && failed.is_none();
    if list.is_empty() && !searching && failed.is_none() && !none {
        return None;
    }
    let width = frame_r.width().max(260.0);
    let mut picked = None;
    let area = egui::Area::new(egui::Id::unique("map_search_list"))
        .order(egui::Order::Foreground)
        .pivot(Align2::LEFT_BOTTOM)
        .fixed_pos(pos2(frame_r.min.x, frame_r.min.y - 4.0))
        .show(ui.ctx(), |ui| {
            // An area anchored by its bottom is laid out in the room below where it sat last
            // frame; the list has its own height.
            ui.set_max_height(340.0);
            egui::Frame::popup(ui.style()).fill(t.menu).inner_margin(egui::Margin::symmetric(0, 4)).show(ui, |ui| {
                ui.set_width(width);
                let st = &app.ui.search;
                let scroll = st.scroll;
                egui::ScrollArea::vertical().max_height(280.0).show(ui, |ui| {
                    let mut heading: Option<String> = None;
                    for (k, row) in list.iter().enumerate() {
                        if matches!(row, Row::SearchPlaces) {
                            continue;
                        }
                        let (group, icon, label, detail) = describe(st, row);
                        if heading.as_deref() != Some(group.as_str()) {
                            group_heading(ui, &group, t);
                            heading = Some(group);
                        }
                        let r = row_button(ui, k == st.active, icon, &label, detail.as_deref(), t);
                        if k == st.active && scroll {
                            r.scroll_to_me(None);
                        }
                        if r.clicked() {
                            picked = Some(k);
                        }
                    }
                    if none {
                        note(ui, tl!("Nothing found"), t.text_dim);
                    }
                });
                // Places stay in sight under the map's matches: the row that searches them, or
                // where that search stands.
                if let Some(k) = list.iter().position(|r| matches!(r, Row::SearchPlaces)) {
                    let (group, icon, label, detail) = describe(st, &Row::SearchPlaces);
                    if !list.iter().any(|r| matches!(r, Row::Hit(_))) {
                        group_heading(ui, &group, t);
                    } else {
                        ui.separator();
                    }
                    if row_button(ui, k == st.active, icon, &label, detail.as_deref(), t).clicked() {
                        picked = Some(k);
                    }
                }
                if searching {
                    ui.horizontal(|ui| {
                        ui.add_space(10.0);
                        ui.spinner();
                        ui.label(egui::RichText::new(tl!("Searching places…")).size(12.0).color(t.text_dim));
                    });
                }
                if let Some(e) = failed {
                    note(ui, &e, t.red);
                }
                if matches!(&st.places, Places::Found(p) if p.is_empty()) {
                    note(ui, tl!("No place found"), t.text_dim);
                }
                if matches!(st.places, Places::Found(_)) {
                    ui.horizontal(|ui| {
                        ui.add_space(10.0);
                        ui.label(egui::RichText::new(tl!("Search results © OpenStreetMap contributors")).small().weak());
                    });
                }
            });
        });
    app.ui.search.scroll = false;
    // Clicking the list mustn't count as clicking away from the box.
    if area.response.contains_pointer() && ui.input(|i| i.pointer.any_pressed()) {
        ui.memory_mut(|m| m.request_focus(egui::Id::unique("map_search_box")));
    }
    list.get(picked?).copied()
}

/// A row's group, icon, text and the line under it.
fn describe(st: &State, row: &Row) -> (String, &'static str, String, Option<String>) {
    match row {
        Row::Hit(i) => match st.results.hits.get(*i) {
            Some(Hit::Coordinate { lon, lat }) => (
                tl!("Coordinate").to_string(),
                "locate",
                crate::i18n::fmt("Go to {coordinate}", &[("coordinate", &search::format_lat_lon(*lat, *lon))]),
                None,
            ),
            Some(Hit::Layer { name, .. }) => (tl!("Layers").to_string(), "contents", name.clone(), None),
            Some(Hit::Tile { name, overlay, .. }) => {
                (tl!("Layers").to_string(), "basemap", if *overlay { format!("{} · {}", name, tl!("overlay")) } else { name.clone() }, None)
            }
            Some(Hit::Bookmark { name }) => (tl!("Bookmarks").to_string(), "bookmark", name.clone(), None),
            Some(Hit::Feature { layer, layer_name, field, value, name, .. }) => {
                let partial = st.results.partial.contains(layer);
                let group =
                    if partial { crate::i18n::fmt("{layer} (first rows only)", &[("layer", layer_name)]) } else { layer_name.clone() };
                // A feature found by another field shows its name, with the value found under it.
                match name {
                    Some(n) => (group, "table", n.clone(), Some(format!("{field}: {value}"))),
                    None => (group, "table", value.clone(), Some(field.clone())),
                }
            }
            None => (String::new(), "search", String::new(), None),
        },
        Row::Place(i) => {
            let name =
                if let Places::Found(p) = &st.places { p.get(*i).map(|p| p.name.clone()).unwrap_or_default() } else { String::new() };
            (tl!("Places").to_string(), "globe_view", name, None)
        }
        Row::SearchPlaces => (
            tl!("Places").to_string(),
            "globe_view",
            crate::i18n::fmt("Search places for “{query}”", &[("query", st.searched.trim())]),
            st.results.hits.is_empty().then(|| tl!("Enter").to_string()),
        ),
    }
}

fn note(ui: &mut Ui, text: &str, color: Color32) {
    ui.horizontal(|ui| {
        ui.add_space(10.0);
        ui.label(egui::RichText::new(text).size(12.0).color(color));
    });
}

fn group_heading(ui: &mut Ui, text: &str, t: &Tokens) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
    ui.painter().text(pos2(r.min.x + 10.0, r.center().y), Align2::LEFT_CENTER, text.to_uppercase(), semibold(10.0), t.text_dim);
}

/// One pickable row.
fn row_button(ui: &mut Ui, active: bool, icon: &str, label: &str, detail: Option<&str>, t: &Tokens) -> egui::Response {
    let w = ui.available_width();
    let g = ui.painter().layout(label.to_string(), regular(12.5), t.text, w - 44.0);
    let h = g.size().y + if detail.is_some() { 20.0 } else { 8.0 };
    let (r, resp) = ui.allocate_exact_size(vec2(w, h.max(26.0)), Sense::click());
    // Named for screen readers (and scripted clicks), as the box is: with the line under it, which
    // tells apart rows of the same text.
    let name = detail.map_or_else(|| label.to_string(), |d| format!("{label}, {d}"));
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &name));
    let p = ui.painter();
    if active || resp.hovered() {
        p.rect_filled(r.shrink2(vec2(4.0, 0.0)), CornerRadius::same(4), if active { t.selection } else { t.hover });
    }
    icons::paint(p, Rect::from_min_size(pos2(r.min.x + 12.0, r.min.y + 5.0), vec2(15.0, 15.0)), icon, t.icon, t.accent);
    let top = r.min.y + 4.0;
    p.galley(pos2(r.min.x + 34.0, top), g.clone(), t.text);
    if let Some(d) = detail {
        p.text(pos2(r.min.x + 34.0, top + g.size().y + 1.0), Align2::LEFT_TOP, d, regular(10.5), t.text_dim);
    }
    resp
}

fn pick(app: &mut RustGisApp, row: Row) {
    let st = &mut app.ui.search;
    match row {
        Row::SearchPlaces => {
            match Geocoder::new(None, None) {
                Ok(g) => {
                    let slot: Answer = crate::Slot::default();
                    let land = slot.clone();
                    let url = g.url(&st.searched, PLACES);
                    rustgis_engine::render::rustgis_tiles::cache::download(
                        &url,
                        Box::new(move |r| *land.lock().unwrap_or_else(|e| e.into_inner()) = Some(r)),
                    );
                    st.pending = Some((g, slot));
                    st.places = Places::Searching;
                    st.open = true;
                    // The place rows come after the map's.
                    st.active = st.results.hits.len();
                }
                Err(e) => st.places = Places::Failed(e),
            }
        }
        Row::Place(i) => {
            let Places::Found(places) = &st.places else { return };
            let Some(p) = places.get(i).cloned() else { return };
            rustgis_engine::cmd::geocode::go_to_place(&mut app.session, &p);
            // The pin is named by the first parts of an address (the place, its town).
            let short = p.name.split(", ").take(2).collect::<Vec<_>>().join(", ");
            settle(app, p.name.clone(), Some((Coord::new(p.lon, p.lat), short)));
        }
        Row::Hit(i) => {
            let Some(hit) = st.results.hits.get(i).cloned() else { return };
            let result = if let Hit::Tile { index, identity, .. } = &hit {
                let Some(at) = rustgis_engine::cmd::tiles::index_of_identity(&app.session, identity, *index) else {
                    app.session.notify(tl!("Tile layer has changed; search again"), true);
                    return;
                };
                app.run("map.zoomToTiles", json!({"index": at}))
            } else {
                // The query the list was found for (the box may have been typed in since), with the
                // list's limit, so the index picked is the row picked.
                let query = st.searched.clone();
                app.run("map.search", json!({"query": query, "limit": search::LIMIT, "go": i}))
            };
            if let Err(e) = result {
                app.session.notify(e, true);
                return;
            }
            let (text, marker) = match &hit {
                Hit::Coordinate { lon, lat } => {
                    let label = search::format_lat_lon(*lat, *lon);
                    (label.clone(), Some((Coord::new(*lon, *lat), label)))
                }
                Hit::Layer { name, .. } | Hit::Tile { name, .. } | Hit::Bookmark { name } => (name.clone(), None),
                Hit::Feature { value, name, .. } => (name.clone().unwrap_or_else(|| value.clone()), None),
            };
            settle(app, text, marker);
        }
    }
}

/// After a pick: the box shows what was picked, the list closes and the map redraws.
fn settle(app: &mut RustGisApp, text: String, marker: Option<(Coord, String)>) {
    let st = &mut app.ui.search;
    st.query = text.clone();
    st.searched = text;
    st.results = Results::default();
    st.places = Places::Idle;
    st.pending = None;
    st.open = false;
    st.active = 0;
    st.marker = marker;
    app.map.invalidate();
}

/// The marker a picked place or coordinate leaves on the map: a pin with its name.
pub fn paint_marker(p: &egui::Painter, at: egui::Pos2, name: &str, t: &Tokens) {
    let red = Color32::from_rgb(0xD1, 0x3C, 0x3C);
    let head = at - vec2(0.0, 22.0);
    p.add(egui::Shape::convex_polygon(vec![at, head + vec2(-7.5, 4.0), head + vec2(7.5, 4.0)], red, Stroke::NONE));
    p.circle_filled(head, 9.0, red);
    p.circle_stroke(head, 9.0, Stroke::new(1.5, Color32::WHITE));
    p.circle_filled(head, 3.5, Color32::WHITE);
    if !name.is_empty() {
        let short: String = name.chars().take(60).collect();
        let g = p.layout_no_wrap(short, regular(11.5), t.text);
        let bg = Rect::from_min_size(head + vec2(13.0, -g.size().y / 2.0 - 3.0), g.size() + vec2(10.0, 6.0));
        p.rect_filled(bg, CornerRadius::same(4), t.ribbon.gamma_multiply(0.92));
        p.rect_stroke(bg, CornerRadius::same(4), Stroke::new(1.0, t.border_strong), egui::StrokeKind::Inside);
        p.galley(bg.min + vec2(5.0, 3.0), g, t.text);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use egui::accesskit::Role;
    use egui_kittest::kittest::Queryable;

    use super::*;

    /// A row's name carries the line under it, which tells apart rows of the same text.
    #[test]
    fn a_row_is_named_with_the_line_under_it() {
        let h = egui_kittest::Harness::new_ui(|ui| {
            let t = Tokens::get(ui.ctx());
            row_button(ui, false, "table", "Bogota", Some("name"), &t);
            row_button(ui, false, "contents", "Cities", None, &t);
        });
        assert!(h.query_by_role_and_label(Role::Button, "Bogota, name").is_some());
        assert!(h.query_by_role_and_label(Role::Button, "Cities").is_some());
    }

    /// A number typed for one layer, then another layer picked in a list a places search left
    /// open: the number is set on the layer it was typed for, even when the other had the same
    /// value (#1194). The field is drawn after the box, as the right pane is.
    #[test]
    fn a_number_typed_is_set_on_its_own_layer_when_the_list_picks_another() {
        let mut app = RustGisApp::with_sample();
        let width = |app: &RustGisApp, name: &str| {
            let l = app.session.map().layers.iter().find(|l| l.name == name).unwrap();
            l.renderer.base_symbol().stroke_width
        };
        for name in ["Countries", "Cities"] {
            app.session.run("layer.select", &json!({"layer": name})).unwrap();
            app.session.run("layer.outline", &json!({"width": 1.0})).unwrap();
        }
        let results = search::search(&app.session, "Countries", search::LIMIT);
        let st = &mut app.ui.search;
        st.query = "Countries".into();
        st.searched = "Countries".into();
        st.results = results;
        st.places = Places::Found(Vec::new());
        st.open = true;
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(800.0, 800.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                show(app, ui, &Tokens::get(&ctx));
                if let Some(layer) = app.session.active_layer.and_then(|id| app.session.map().layer(id)).cloned() {
                    let mut w = layer.renderer.base_symbol().stroke_width;
                    let label = ui.label("Width");
                    let r = ui.add(egui::DragValue::new(&mut w).update_while_editing(false)).labelled_by(label.id);
                    if crate::widgets::changed(&r) {
                        let _ = app.run("layer.outline", json!({"width": w}));
                    }
                }
            },
            app,
        );
        h.run();
        h.get_by_role(Role::SpinButton).focus();
        h.run();
        h.event(egui::Event::Text("3".into()));
        h.run();
        let at = h.get_by_role_and_label(Role::Button, "Countries").rect().center();
        h.hover_at(at);
        h.drag_at(at);
        h.step();
        h.drop_at(at);
        h.run();
        let countries = h.state().session.map().layers.iter().find(|l| l.name == "Countries").map(|l| l.id);
        assert_eq!(h.state().session.active_layer, countries, "the list picked Countries");
        assert_eq!(width(h.state(), "Cities"), 3.0);
        assert_eq!(width(h.state(), "Countries"), 1.0);
    }
}
