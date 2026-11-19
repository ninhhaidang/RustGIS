//! The coordinate system box: type a code or words of a name (`2056`, `swiss lv95`, `ESRI:102033`)
//! and the matching systems of the catalog drop down under it, the recently used ones first.
//! ↑/↓ move the highlight, Enter takes it, Escape closes the list; clicking a row takes it too.
//! A typed text the list doesn't hold (a PROJ string, a preset) stays as typed.

use crate::theme::Tokens;
use egui::{Id, Key, Ui};
use rustgis_engine::data::raster::crs_search::{self, Found, Kind};
use std::sync::Arc;

/// The most systems the list holds; more words narrow it.
const LIMIT: usize = 60;
/// The most recently used systems kept.
pub(crate) const RECENT_MAX: usize = 10;
/// The preference holding them, one code per line, the latest first.
pub(crate) const RECENT_PREF: &str = "recent_crs";
/// The list's height before it scrolls.
const LIST_HEIGHT: f32 = 260.0;

/// What a box keeps between frames.
#[derive(Clone, Default)]
struct State {
    open: bool,
    /// The highlighted row.
    at: usize,
    /// Whether ↑/↓ moved the highlight since the text last changed: Enter in a blank box takes a
    /// row only then, a blank box being a choice of its own (no system, or the map's own).
    moved: bool,
    /// The list last drawn, so a click outside both closes it.
    drawn: Option<egui::Rect>,
    /// The pass the box was last drawn in: one not drawn the pass before (its menu or dialog
    /// closed) opens with its list closed.
    pass: u64,
    /// The rows last found, and what they were found for: the search runs when that changes,
    /// not every frame.
    found: Option<(String, Offer, Vec<String>, Arc<Vec<Row>>)>,
}

/// Where the pass a box's list was last open, or its box had the focus, is kept.
fn open_id() -> Id {
    Id::unique("crs_box_open")
}

/// Whether a coordinate system box had the focus, or its list was open, this pass or the last:
/// Escape and the other unmodified keys are then the box's, not shortcuts (egui drops a box's
/// focus on Escape before the shortcuts run).
pub(crate) fn list_is_open(ctx: &egui::Context) -> bool {
    let pass = ctx.cumulative_pass_nr();
    ctx.data(|d| d.get_temp::<u64>(open_id())).is_some_and(|open| open + 1 >= pass)
}

/// Where the pass a box's list was last shown is kept.
fn shown_id() -> Id {
    Id::unique("crs_box_shown")
}

/// Whether a coordinate system box's list was shown this pass or the last (not only its box
/// focused).
pub(crate) fn list_is_shown(ctx: &egui::Context) -> bool {
    let pass = ctx.cumulative_pass_nr();
    ctx.data(|d| d.get_temp::<u64>(shown_id())).is_some_and(|shown| shown + 1 >= pass)
}

/// One row of the list.
#[derive(Clone)]
struct Row {
    code: String,
    name: String,
    /// What the row's tooltip says: the projection's method and the area of use.
    about: Option<String>,
    section: Section,
}

/// What a box offers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Offer {
    /// Every system: a layer's data may be in degrees.
    All,
    /// Projected systems only (a measured graticule, an analysis's cells).
    Projected,
    /// What a map can be drawn in: projected systems and the map's own projections.
    Map,
}

/// The parts of the list, in order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Section {
    Recent,
    /// The map's own projections (Robinson, Equal Earth…), offered by a map's box only.
    Map,
    Catalog,
}

/// The rows for `text`: the recently used systems that match it (all of them for no text), for a
/// map its own projections whose names match, then the catalog's, as `offer` allows.
fn rows(text: &str, recent: &[String], offer: Offer) -> Vec<Row> {
    let map = offer == Offer::Map;
    let text = text.trim();
    let words: Vec<String> = text.to_lowercase().split_whitespace().map(str::to_string).collect();
    let matches = |hay: &str| words.iter().all(|w| hay.contains(w.as_str()));
    // A code typed (`2056`, `EPSG:2056`): the recent systems whose code begins with it, or only
    // that system when the catalog lists it, so a recent EPSG:23035 never comes before 3035.
    let compact = text.to_ascii_lowercase().replace(' ', "");
    // `EPSG 2056` too, as the catalog reads it.
    let exact = crs_search::lookup(text).map(|f| f.code.to_ascii_lowercase());
    let code_typed = exact.is_some() || (!compact.is_empty() && (compact.bytes().all(|b| b.is_ascii_digit()) || compact.contains(':')));
    let mut out: Vec<Row> = Vec::new();
    for code in recent {
        if out.len() == RECENT_MAX {
            break;
        }
        let Some(found) = crs_search::lookup(code) else { continue };
        let usable = match offer {
            Offer::All => true,
            Offer::Projected => found.kind == Kind::Projected,
            Offer::Map => map_usable(code),
        };
        if !usable {
            continue;
        }
        let lower = code.to_ascii_lowercase();
        let hit = match (&exact, code_typed) {
            (Some(exact), _) => &lower == exact,
            (None, true) => lower.starts_with(&compact) || lower.rsplit(':').next().is_some_and(|d| d.starts_with(&compact)),
            // As the catalog's own are found: by name, method or area of use.
            (None, false) => words.is_empty() || crs_search::matches(code, text),
        };
        if hit {
            out.push(Row { code: code.clone(), name: found.name.clone(), about: None, section: Section::Recent });
        }
    }
    if map && !words.is_empty() {
        let own = [("mercator", "Web Mercator"), ("globe", "Globe")]
            .into_iter()
            .chain(rustgis_engine::map::projection::PRESETS.iter().map(|(id, label, _)| (*id, *label)));
        for (id, label) in own {
            let hay = format!("{} {} {}", id.to_lowercase(), label.to_lowercase(), crate::i18n::t(label).to_lowercase());
            if matches(&hay) {
                out.push(Row { code: id.to_string(), name: crate::i18n::t(label).to_string(), about: None, section: Section::Map });
            }
        }
    }
    // A map's box leaves out what a map can't be drawn in, so it asks for more.
    let limit = if map { LIMIT * 2 } else { LIMIT };
    for Found { code, name, area, method, .. } in crs_search::search(text, offer != Offer::All, limit) {
        if out.len() >= LIMIT + recent.len() {
            break;
        }
        // A projected system a map can't be drawn in (a method RustGIS can't compute) isn't offered.
        if map && !map_usable(&code) {
            continue;
        }
        if !out.iter().any(|r| r.code.eq_ignore_ascii_case(&code)) {
            let about = match (method, area) {
                (Some(m), Some(a)) => Some(format!("{m}\n{a}")),
                (m, a) => m.or(a),
            };
            out.push(Row { code, name, about, section: Section::Catalog });
        }
    }
    out
}

/// Whether a map can be drawn in `code`, asked once a run per code: [`Projection::from_code`]
/// builds the projection.
///
/// [`Projection::from_code`]: rustgis_engine::map::Projection::from_code
pub(crate) fn map_usable(code: &str) -> bool {
    use std::collections::HashMap;
    use std::sync::Mutex;
    static KNOWN: Mutex<Option<HashMap<String, bool>>> = Mutex::new(None);
    if let Some(known) = KNOWN.lock().ok().and_then(|k| k.as_ref().and_then(|k| k.get(code).copied())) {
        return known;
    }
    let usable = rustgis_engine::map::Projection::from_code(code).is_ok();
    if let Ok(mut known) = KNOWN.lock() {
        known.get_or_insert_with(HashMap::new).insert(code.to_string(), usable);
    }
    usable
}

/// A coordinate system box `width` wide, editing `text`. `recent` are the recently used codes,
/// the latest first. Returns the box's response, and whether a row of the list was taken this
/// frame (its code is then in `text`).
pub(crate) fn crs_box(
    ui: &mut Ui,
    salt: &str,
    text: &mut String,
    recent: &[String],
    offer: Offer,
    width: f32,
    hint: &str,
    t: &Tokens,
) -> (egui::Response, bool) {
    let id = Id::unique(("crs_box", salt));
    let mut state: State = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    let pass = ui.ctx().cumulative_pass_nr();
    if state.pass + 1 < pass {
        state.open = false;
    }
    state.pass = pass;
    let r = ui.add(egui::TextEdit::singleline(text).id(id.with("edit")).hint_text(hint).desired_width(width));
    if r.gained_focus() || r.clicked() || r.changed() {
        state.open = true;
    }
    // Focus selects what the box holds, so typing searches anew rather than adding to a code.
    if r.gained_focus()
        && let Some(mut edit) = egui::text_edit::TextEditState::load(ui.ctx(), r.id)
    {
        let end = text.chars().count();
        edit.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(end))));
        edit.store(ui.ctx(), r.id);
    }
    // A list opened anew, or its text changed, starts at its first row with nothing chosen.
    if r.changed() || r.gained_focus() || !state.open {
        state.at = 0;
        state.moved = false;
    }
    let rows = if state.open {
        let fresh = state.found.as_ref().is_none_or(|(was, o, r, _)| was != text || *o != offer || r.as_slice() != recent);
        if fresh {
            state.found = Some((text.clone(), offer, recent.to_vec(), Arc::new(rows(text, recent, offer))));
        }
        state.found.as_ref().map(|f| f.3.clone()).unwrap_or_default()
    } else {
        Arc::default()
    };
    let showing = state.open && (!rows.is_empty() || !text.trim().is_empty());
    if (state.open && showing) || r.has_focus() || r.lost_focus() {
        ui.data_mut(|d| d.insert_temp(open_id(), pass));
    }
    if showing {
        ui.data_mut(|d| d.insert_temp(shown_id(), pass));
    }
    let mut taken = None;
    // Enter ends a single-line edit, so the box has just lost its focus when Enter takes a row.
    if state.open && (r.has_focus() || r.lost_focus()) {
        let (up, down, enter, escape) = ui
            .input(|i| (i.key_pressed(Key::ArrowUp), i.key_pressed(Key::ArrowDown), i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
        if down && !rows.is_empty() {
            state.at = (state.at + 1).min(rows.len() - 1);
            state.moved = true;
        }
        if up {
            state.at = state.at.saturating_sub(1);
            state.moved = true;
        }
        // The list's keys are its own: Escape doesn't close the dialog, Enter doesn't submit it.
        if escape && showing {
            ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape));
        }
        if escape {
            state.open = false;
        }
        // Enter in a blank box is that blank (no system, the map's own), unless a row was chosen
        // with ↑/↓.
        let chosen = !text.trim().is_empty() || state.moved;
        if enter
            && !escape
            && chosen
            && let Some(row) = rows.get(state.at)
        {
            // A code typed in full (`2263`, `EPSG:2263`) is taken and Enter goes on to the dialog,
            // which submits.
            let typed_in_full = crs_search::lookup(text).is_some_and(|f| f.code == row.code);
            if !typed_in_full {
                ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Enter));
            }
            taken = Some(row.code.clone());
        }
    }
    // In a menu the list is drawn in it, under the box: a menu closes on a press in any other
    // layer, a popup's too. It stays open there until taken or Escape, since closing on a press
    // would move what is under it before the click ends (a menu closes on a click elsewhere anyway).
    let inline = egui::containers::menu::is_in_menu(ui);
    // Enter ends a single-line edit, which loses focus: that is the take, not a close.
    // Enter that took no row (a blank box) closes it too.
    if state.open && !inline && r.lost_focus() && taken.is_none() {
        // A click on the list loses the box's focus first: the list closes on a click elsewhere.
        let on_list = ui.input(|i| i.pointer.interact_pos()).is_some_and(|p| state.drawn.is_some_and(|d| d.contains(p)));
        if !on_list {
            state.open = false;
        }
    }
    if state.open
        && !inline
        && ui.input(|i| i.pointer.any_pressed())
        && let Some(p) = ui.input(|i| i.pointer.interact_pos())
        && !r.rect.contains(p)
        && !state.drawn.is_some_and(|d| d.contains(p))
    {
        state.open = false;
    }
    state.at = state.at.min(rows.len().saturating_sub(1));
    let showing = state.open && (!rows.is_empty() || !text.trim().is_empty());
    if showing && inline {
        let shown = egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(width.max(300.0));
            list(ui, &rows, &mut state.at, t)
        });
        state.drawn = Some(shown.response.rect);
        if let Some(i) = shown.inner {
            taken = rows.get(i).map(|row| row.code.clone());
        }
    } else if showing {
        let shown = egui::Popup::from_response(&r)
            .id(id.with("list"))
            .open(true)
            .close_behavior(egui::PopupCloseBehavior::IgnoreClicks)
            .width(width.max(360.0))
            .show(|ui| {
                // As wide as asked, and as tall as its rows up to its height: the popup keeps its
                // first frame's size otherwise, a row or two when the list opened on one code.
                ui.set_min_width(width.max(360.0));
                list(ui, &rows, &mut state.at, t)
            });
        if let Some(shown) = shown {
            state.drawn = Some(shown.response.rect);
            if let Some(i) = shown.inner {
                taken = rows.get(i).map(|row| row.code.clone());
            }
        }
    } else {
        state.drawn = None;
    }
    if let Some(code) = &taken {
        *text = code.clone();
        state.open = false;
    }
    ui.data_mut(|d| d.insert_temp(id, state));
    (r, taken.is_some())
}

/// Draws the rows; the one clicked, if any.
fn list(ui: &mut Ui, rows: &[Row], at: &mut usize, t: &Tokens) -> Option<usize> {
    if rows.is_empty() {
        ui.label(egui::RichText::new(tl!("No coordinate system matches; a code or PROJ string is used as typed.")).weak());
        return None;
    }
    let mut clicked = None;
    let tall = (rows.len() as f32 * 30.0 + 24.0).min(LIST_HEIGHT);
    egui::ScrollArea::vertical().max_height(LIST_HEIGHT).min_scrolled_height(tall).show(ui, |ui| {
        ui.with_layout(egui::Layout::top_down_justified(egui::Align::LEFT), |ui| {
            let mut heading = None;
            for (i, row) in rows.iter().enumerate() {
                let section = match row.section {
                    Section::Recent => tl!("Recently used"),
                    Section::Map => tl!("Map projections"),
                    Section::Catalog => tl!("Coordinate systems"),
                };
                if heading != Some(section) {
                    if heading.is_some() {
                        ui.separator();
                    }
                    ui.label(egui::RichText::new(section).size(11.0).color(t.text_dim));
                    heading = Some(section);
                }
                let mut job = egui::text::LayoutJob::default();
                // A map's own projection is named only; it has no code.
                if row.section != Section::Map {
                    job.append(
                        &row.code,
                        0.0,
                        egui::TextFormat { font_id: egui::FontId::monospace(11.5), color: t.text_dim, ..Default::default() },
                    );
                }
                if !row.name.is_empty() {
                    job.append(
                        &row.name,
                        if job.is_empty() { 0.0 } else { 10.0 },
                        egui::TextFormat {
                            font_id: egui::FontId::proportional(13.0),
                            color: ui.visuals().text_color(),
                            ..Default::default()
                        },
                    );
                }
                let r = ui.selectable_label(*at == i, job);
                let r = match &row.about {
                    Some(about) => r.on_hover_text(about),
                    None => r,
                };
                if *at == i && ui.input(|inp| inp.key_pressed(Key::ArrowDown) || inp.key_pressed(Key::ArrowUp)) {
                    r.scroll_to_me(None);
                }
                if r.clicked() {
                    *at = i;
                    clicked = Some(i);
                }
            }
        });
    });
    clicked
}

/// Notes `code` as the latest coordinate system used, keeping [`RECENT_MAX`]; blank is ignored.
pub(crate) fn remember(recent: &mut Vec<String>, code: &str) -> bool {
    let code = code.trim();
    if code.is_empty() {
        return false;
    }
    // As long as the list was: [`RECENT_MAX`], or a longer one a newer RustGIS saved.
    let keep = recent.len().max(RECENT_MAX);
    recent.retain(|c| !c.eq_ignore_ascii_case(code));
    recent.insert(0, code.to_string());
    recent.truncate(keep);
    true
}

/// The recently used codes saved as [`RECENT_PREF`], every distinct line kept (a line repeated in
/// another case is one system): a code this catalog doesn't
/// list, and lines past [`RECENT_MAX`] (a newer RustGIS's), are saved again as they were and only
/// left out of the lists. The catalog isn't read here, at start-up.
pub(crate) fn from_pref(value: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in value.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if !out.iter().any(|c| c.eq_ignore_ascii_case(line)) {
            out.push(line.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recently_used_systems_come_first_and_only_once() {
        let recent = vec!["EPSG:2056".to_string(), "EPSG:4326".to_string()];
        let found = rows("lv95", &recent, Offer::All);
        assert_eq!(found[0].code, "EPSG:2056");
        assert_eq!(found[0].section, Section::Recent);
        assert_eq!(found.iter().filter(|r| r.code == "EPSG:2056").count(), 1);
        // No text: the recent ones alone.
        let all = rows("", &recent, Offer::All);
        assert_eq!(all.iter().map(|r| r.code.as_str()).collect::<Vec<_>>(), ["EPSG:2056", "EPSG:4326"]);
        assert_eq!(all[1].name, "WGS 84");
        // A map can't be drawn in degrees.
        assert!(!rows("", &recent, Offer::Map).iter().any(|r| r.code == "EPSG:4326"));
        assert_eq!(rows("", &recent, Offer::Projected).iter().map(|r| r.code.as_str()).collect::<Vec<_>>(), ["EPSG:2056"]);
        assert!(!rows("robinson", &[], Offer::Projected).iter().any(|r| r.section == Section::Map), "a preset isn't a system");
        // A map's own projections are found by name, before the catalog's systems.
        let robinson = rows("robinson", &[], Offer::Map);
        assert_eq!((robinson[0].code.as_str(), robinson[0].section), ("robinson", Section::Map));
        assert!(robinson.iter().any(|r| r.code == "ESRI:54030"));
        assert!(!rows("robinson", &[], Offer::All).iter().any(|r| r.section == Section::Map), "data is never in a preset");
        // A code typed is that system first, not a recent one holding its digits.
        let recent = vec!["EPSG:23035".to_string(), "EPSG:3031".to_string()];
        assert_eq!(rows("3035", &recent, Offer::All)[0].code, "EPSG:3035");
        assert_eq!(rows("EPSG:3035", &recent, Offer::All)[0].code, "EPSG:3035");
        // A code begun: the recent ones it begins first, and only those.
        let begun = rows("303", &recent, Offer::All);
        assert_eq!((begun[0].code.as_str(), begun[0].section), ("EPSG:3031", Section::Recent));
        assert!(!begun.iter().any(|r| r.code == "EPSG:23035" && r.section == Section::Recent));
    }

    #[test]
    fn the_recent_list_keeps_the_latest_first_without_repeats() {
        let mut recent = from_pref("EPSG:2056\n\nepsg:2056\nEPSG:3857\n");
        assert_eq!(recent, ["EPSG:2056", "EPSG:3857"]);
        // A code this catalog doesn't list is kept, and only left out of the lists.
        let newer = from_pref("EPSG:99999999\nEPSG:2056");
        assert_eq!(newer, ["EPSG:99999999", "EPSG:2056"]);
        assert_eq!(rows("", &newer, Offer::All).iter().map(|r| r.code.as_str()).collect::<Vec<_>>(), ["EPSG:2056"]);
        // A recent system found by its area of use is listed as recent.
        let swiss = rows("switzerland", &["EPSG:2056".to_string()], Offer::Map);
        assert_eq!((swiss[0].code.as_str(), swiss[0].section), ("EPSG:2056", Section::Recent));
        assert!(remember(&mut recent, "EPSG:3857"));
        assert_eq!(recent, ["EPSG:3857", "EPSG:2056"]);
        assert!(!remember(&mut recent, "  "));
        for n in 0..20 {
            remember(&mut recent, &format!("EPSG:{}", 32600 + n));
        }
        assert_eq!(recent.len(), RECENT_MAX);
        assert_eq!(recent[0], "EPSG:32619");
        // A longer list (a newer RustGIS's) is kept whole and stays as long; ten are listed.
        let saved: Vec<String> = (0..12).map(|n| format!("EPSG:{}", 32601 + n)).collect();
        let mut longer = from_pref(&saved.join("\n"));
        assert_eq!(longer, saved);
        remember(&mut longer, "EPSG:2056");
        assert_eq!((longer.len(), longer[0].as_str()), (12, "EPSG:2056"));
        assert_eq!(rows("", &longer, Offer::All).len(), RECENT_MAX);
    }
}
