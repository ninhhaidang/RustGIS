//! A text graphic in the Format pane (#516): the one text selected on the map, edited with the
//! form a layout's text has, each change one `graphics.edit` (one undo step). A text is placed as
//! a layout's is: the Text tool puts one down, selected, and the pane opens on it with the text
//! ready to type over. The pane shows a layout's element instead while a layout is on screen.

use serde_json::json;

use rustgis_engine::map::graphics;

use crate::RustGisApp;
use crate::theme::Tokens;

/// A text graphic, by map (`Map::instance_id`, whatever its place among the project's maps), layer
/// id and row: layer ids and rows are a map's own.
type Key = (u64, u32, usize);

#[derive(Default)]
pub struct GraphicFormat {
    /// The text the pane last opened for: a new one opens it again, so closing the pane keeps the
    /// selection without the pane coming back by itself.
    formatted: Option<Key>,
    /// The text being typed: its graphic, the project it is for (`Session::opened`), and the text.
    draft: Option<(Key, u64, String)>,
    /// The graphic the pane drew the form of this frame: a draft for any other is saved at the
    /// end of the frame (`save_draft`).
    drawn: Option<Key>,
    /// A text just placed: its field is focused and selected whole, so typing replaces it.
    select: Option<Key>,
}

/// The text graphic the Format pane edits: the one selected on the map, when it is the only
/// thing selected there and a text.
pub(crate) fn selected(app: &RustGisApp) -> Option<(u32, usize)> {
    let mut rows = app.session.map().layers.iter().flat_map(|l| l.selection.iter().map(move |r| (l, *r)));
    let (l, row) = rows.next()?;
    if rows.next().is_some() || !l.graphics || graphics::kind(&l.dataset, row) != "text" {
        return None;
    }
    Some((l.id, row))
}

fn key(app: &RustGisApp, (layer, row): (u32, usize)) -> Key {
    (app.session.map().instance_id(), layer, row)
}

/// Opens the Format pane when another text is selected on the map (run each frame the map shows).
pub(crate) fn follow(app: &mut RustGisApp, ctx: &egui::Context) {
    let now = selected(app).map(|s| key(app, s));
    if now != app.graphic_format.formatted {
        app.graphic_format.formatted = now;
        if now.is_some() {
            app.ui.open_right(crate::RightPane::Format);
            ctx.request_repaint();
        }
    }
}

/// Places a text at `lon`/`lat` with the Text tool: `text` (a dynamic text picked from the ribbon),
/// else "Text", selected and open in the Format pane to be typed over. The tool goes back to
/// Explore unless Keep Tool is on.
pub(crate) fn place(app: &mut RustGisApp, lon: f64, lat: f64, text: Option<&str>) -> Result<(), String> {
    let text = text.unwrap_or("Text");
    let added = app.run("graphics.add", json!({"shape": "text", "points": [[lon, lat]], "text": text}))?;
    let layer = added["layer"].as_u64().and_then(|v| u32::try_from(v).ok()).ok_or("no layer")?;
    let row = added["row"].as_u64().and_then(|v| usize::try_from(v).ok()).ok_or("no row")?;
    edit(app, layer, row)?;
    app.graphic_format.select = Some(key(app, (layer, row)));
    if !app.session.keep_tool {
        app.session.tool = rustgis_engine::Tool::Explore;
    }
    Ok(())
}

/// Selects a placed text and opens the Format pane on it (a double-click, Edit Text…, or
/// `ui.dialog` for a text by its layer and row), even closed over the same text.
pub(crate) fn edit(app: &mut RustGisApp, layer: u32, row: usize) -> Result<(), String> {
    let found = app.session.map().layer(layer).filter(|l| l.graphics).is_some_and(|l| graphics::kind(&l.dataset, row) == "text");
    if !found {
        return Err(tl!("no text graphic there").to_string());
    }
    app.run("select.rows", json!({"layer": layer, "rows": [row]}))?;
    app.graphic_format.formatted = Some(key(app, (layer, row)));
    app.ui.open_right(crate::RightPane::Format);
    Ok(())
}

/// The Format pane: a layout's element while a layout is on screen, else the text selected on
/// the map.
pub fn show(app: &mut RustGisApp, ui: &mut egui::Ui, t: &Tokens) {
    if app.session.active_layout.is_some() && !app.mb.shown() {
        crate::layoutview::format(app, ui, t);
        return;
    }
    let Some((layer, row)) = selected(app) else {
        ui.label(egui::RichText::new(tl!("Select one text on the map, or a layout element, to format it.")).color(t.text_dim));
        return;
    };
    let Some(l) = app.session.map().layer(layer) else { return };
    let ds = &l.dataset;
    let current = graphics::label(ds, row).unwrap_or_default().to_string();
    let (mut size, mut color, mut bold) =
        (f64::from(graphics::font_size(ds, row)), crate::theme::c32(graphics::text_color(ds, row)), graphics::bold(ds, row));
    let (mut italic, mut align, mut spacing) =
        (graphics::italic(ds, row), graphics::align(ds, row), f64::from(graphics::letter_spacing(ds, row)));
    let k = key(app, (layer, row));
    let opened = app.session.opened;
    app.graphic_format.drawn = Some(k);
    // A draft for another text is left for `save_draft`.
    let mut text = match app.graphic_format.draft.take() {
        Some((dk, p, t)) if dk == k && p == opened => t,
        other => {
            app.graphic_format.draft = other;
            current.clone()
        }
    };
    let heading = crate::i18n::fmt("Map: {kind}", &[("kind", tl_raw!("Text"))]);
    ui.label(egui::RichText::new(heading).font(crate::theme::semibold(14.0)));
    ui.separator();
    let select = app.graphic_format.select.take_if(|s| *s == k).is_some();
    let fields = crate::textform::Fields {
        text: &mut text,
        size: &mut size,
        color: Some(&mut color),
        bold: Some(&mut bold),
        italic: Some(&mut italic),
        align: Some(&mut align),
        wrap: None,
        warning: None,
        letter_spacing: Some(&mut spacing),
        font: None,
        font_name: None,
        named_fonts: false,
        select,
    };
    let settled = crate::textform::section(ui, "Text", |ui| crate::textform::show(ui, ("graphic_format", k), &[graphics::TOKENS], fields))
        .unwrap_or_default();
    let save = |app: &mut RustGisApp, change: serde_json::Value| {
        let mut p = json!({"layer": layer, "row": row});
        if let (Some(p), Some(c)) = (p.as_object_mut(), change.as_object()) {
            p.extend(c.clone());
        }
        let _ = app.run("graphics.edit", p);
    };
    if settled.typing {
        app.graphic_format.draft = Some((k, opened, text));
    } else if settled.text && text != current {
        save(app, json!({"text": text}));
    }
    if settled.size {
        save(app, json!({"fontSize": size}));
    }
    if let Some(resp) = &settled.color {
        crate::widgets::one_step_per_press(app, resp);
        save(app, json!({"fill": crate::theme::rgba(color).hex8()}));
    }
    if settled.bold {
        save(app, json!({"bold": bold}));
    }
    if settled.italic {
        save(app, json!({"italic": italic}));
    }
    if settled.align {
        save(app, json!({"align": align.code()}));
    }
    if settled.letter_spacing {
        save(app, json!({"letterSpacing": spacing}));
    }
}

/// Saves the text typed for a graphic the pane didn't draw this frame (the pane was closed or
/// hidden, or the selection changed) before its field could say it lost focus. A draft for
/// another project, or another map than the one shown, is dropped: its layer ids and rows aren't
/// this one's. Runs once a frame, after the panes and the view.
pub(crate) fn save_draft(app: &mut RustGisApp) {
    let drawn = app.graphic_format.drawn.take();
    let Some((k, opened, text)) = app.graphic_format.draft.take() else { return };
    if Some(k) == drawn && opened == app.session.opened {
        app.graphic_format.draft = Some((k, opened, text));
        return;
    }
    let (map, layer, row) = k;
    let here = opened == app.session.opened && map == app.session.map().instance_id();
    let current = app.session.map().layer(layer).and_then(|l| graphics::label(&l.dataset, row)).map(str::to_string);
    // A blank text goes to `graphics.edit` too, which refuses it and says so, as when Enter ends it.
    if here && current.is_some_and(|c| c != text) {
        let _ = app.run("graphics.edit", json!({"layer": layer, "row": row, "text": text}));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RightPane::Format;
    use egui::accesskit::{Action, ActionData, ActionRequest, Role};
    use egui_kittest::kittest::{NodeT, Queryable};
    use rustgis_engine::map::layout::TextAlign;

    fn harness(app: RustGisApp) -> egui_kittest::Harness<'static, RustGisApp> {
        let mut fonts = false;
        egui_kittest::Harness::builder().with_size(egui::vec2(1000.0, 800.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                follow(app, &ui.ctx().clone());
                let t = Tokens::get(ui.ctx());
                if app.ui.right == Some(Format) {
                    show(app, ui, &t);
                }
                save_draft(app);
            },
            app,
        )
    }

    /// Selecting one text on the map opens the Format pane on it; its form saves each change as
    /// one undo step, and text being typed when the pane closes is saved.
    #[test]
    fn a_selected_text_is_formatted_in_the_pane() {
        let mut app = RustGisApp::with_sample();
        let added = app.run("graphics.add", json!({"shape": "text", "points": [[10, 50]], "text": "Old"})).unwrap();
        let (layer, row) = (added["layer"].as_u64().unwrap() as u32, added["row"].as_u64().unwrap() as usize);
        let mut h = harness(app);
        h.run_steps(2);
        assert_eq!(h.state().ui.right, None, "nothing selected, nothing opened");
        h.state_mut().run("select.rows", json!({"layer": layer, "rows": [row]})).unwrap();
        h.run_steps(3);
        assert_eq!(h.state().ui.right, Some(Format));
        let ds = |h: &egui_kittest::Harness<'_, RustGisApp>| h.state().session.map().layer(layer).unwrap().dataset.clone();
        // Bold, at once and undoable.
        h.get_by_role_and_label(Role::CheckBox, "Bold").click();
        h.run_steps(2);
        assert!(graphics::bold(&ds(&h), row));
        assert_eq!(h.state().session.undo_label(), Some("Edit Graphic"));
        // Italic, alignment and letter spacing too, as a layout's text has them (#535), each saved.
        h.get_by_role_and_label(Role::CheckBox, "Italic").click();
        h.run_steps(2);
        assert!(graphics::italic(&ds(&h), row));
        h.get_by_role(Role::ComboBox).click();
        h.run_steps(2);
        h.get_by_role_and_label(Role::Button, "Center").click();
        h.run_steps(2);
        assert_eq!(graphics::align(&ds(&h), row), TextAlign::Center);
        // Set as assistive technology sets a number.
        let (node, tree) = h.get_by_role_and_label(Role::SpinButton, "Letter spacing").accesskit_node().locate();
        h.event(egui::Event::AccessKitActionRequest(ActionRequest {
            action: Action::SetValue,
            target_tree: tree,
            target_node: node,
            data: Some(ActionData::NumericValue(3.5)),
        }));
        h.run_steps(3);
        assert_eq!(graphics::letter_spacing(&ds(&h), row), 3.5);
        // Typed, then the pane closed: the text is saved, the text still selected.
        h.query_all_by_role(Role::MultilineTextInput).find(|n| n.value().as_deref() == Some("Old")).unwrap().focus();
        h.run_steps(2);
        h.query_all_by_role(Role::MultilineTextInput).find(|n| n.is_focused()).unwrap().type_text("New");
        h.run_steps(2);
        assert_eq!(graphics::label(&ds(&h), row), Some("Old"), "still being typed");
        h.state_mut().run("ui.pane.format", json!({})).unwrap();
        h.run_steps(2);
        assert!(graphics::label(&ds(&h), row).is_some_and(|t| t.contains("New")), "{:?}", graphics::label(&ds(&h), row));
        assert_eq!(selected(h.state()), Some((layer, row)));
        assert_eq!(h.state().ui.right, None, "closed, it stays closed");
        // Editing it again (a double-click, Edit Text…) brings it back.
        edit(h.state_mut(), layer, row).unwrap();
        assert_eq!(h.state().ui.right, Some(Format));
        assert!(edit(h.state_mut(), layer, row + 99).is_err());
    }

    /// The map with the text being typed moves (its tab, Move Up, `map.move`) and stays in view: the
    /// pane keeps the text and saves it when it ends, instead of dropping it for "another map" (#2959).
    #[test]
    fn moving_the_map_keeps_the_text_being_typed() {
        let mut app = RustGisApp::with_sample();
        let added = app.run("graphics.add", json!({"shape": "text", "points": [[10, 50]], "text": "Old"})).unwrap();
        let (layer, row) = (added["layer"].as_u64().unwrap() as u32, added["row"].as_u64().unwrap() as usize);
        let first = app.session.map().name.clone();
        let mut h = harness(app);
        h.run_steps(2);
        h.state_mut().run("select.rows", json!({"layer": layer, "rows": [row]})).unwrap();
        h.run_steps(3);
        h.query_all_by_role(Role::MultilineTextInput).find(|n| n.value().as_deref() == Some("Old")).unwrap().focus();
        h.run_steps(2);
        h.query_all_by_role(Role::MultilineTextInput).find(|n| n.is_focused()).unwrap().type_text("New");
        h.run_steps(2);
        h.state_mut().run("map.new", json!({"name": "Second"})).unwrap();
        h.state_mut().run("map.activate", json!({"name": first})).unwrap();
        h.state_mut().run("map.move", json!({"name": first, "to": 1})).unwrap();
        assert_eq!(h.state().session.project.active_map, 1, "the map in view moved to the end");
        h.state_mut().run("select.rows", json!({"layer": layer, "rows": [row]})).unwrap();
        h.run_steps(3);
        h.state_mut().run("ui.pane.format", json!({})).unwrap();
        h.run_steps(2);
        let label = h.state().session.map().layer(layer).and_then(|l| graphics::label(&l.dataset, row)).map(str::to_string);
        assert!(label.as_deref().is_some_and(|t| t.contains("New")), "{label:?}");
    }

    /// A text cleared in the pane is refused, as `graphics.edit` refuses one: when Enter ends it and
    /// when the pane closes over it alike. The text keeps what it said, and stays on the map.
    #[test]
    fn a_cleared_text_is_refused_however_it_ends() {
        for close in [false, true] {
            let mut app = RustGisApp::with_sample();
            let added = app.run("graphics.add", json!({"shape": "text", "points": [[10, 50]], "text": "Old"})).unwrap();
            let (layer, row) = (added["layer"].as_u64().unwrap() as u32, added["row"].as_u64().unwrap() as usize);
            app.run("select.rows", json!({"layer": layer, "rows": [row]})).unwrap();
            let mut h = harness(app);
            h.run_steps(3);
            h.query_all_by_role(Role::MultilineTextInput).find(|n| n.value().as_deref() == Some("Old")).unwrap().focus();
            h.run_steps(2);
            h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
            h.key_press(egui::Key::Backspace);
            h.run_steps(2);
            if close {
                h.state_mut().run("ui.pane.format", json!({})).unwrap();
            } else {
                h.key_press(egui::Key::Enter);
            }
            h.run_steps(2);
            let ds = &h.state().session.map().layer(layer).unwrap().dataset;
            assert_eq!(graphics::label(ds, row), Some("Old"), "closed: {close}");
            assert!(h.state().session.notices.iter().any(|n| n.error && n.text.contains("text is required")), "closed: {close}");
        }
    }

    /// Only one text is formatted: two graphics selected, or one that isn't a text, leave the
    /// pane waiting.
    #[test]
    fn only_one_selected_text_is_formatted() {
        let mut app = RustGisApp::with_sample();
        let a = app.run("graphics.add", json!({"shape": "text", "points": [[10, 50]], "text": "A"})).unwrap();
        let layer = a["layer"].as_u64().unwrap();
        let b = app.run("graphics.add", json!({"shape": "text", "points": [[20, 40]], "text": "B", "layer": layer})).unwrap();
        let point = app.run("graphics.add", json!({"shape": "point", "points": [[30, 30]], "layer": layer})).unwrap();
        app.run("select.rows", json!({"layer": layer, "rows": [a["row"], b["row"]]})).unwrap();
        assert_eq!(selected(&app), None, "two texts");
        app.run("select.rows", json!({"layer": layer, "rows": [point["row"]]})).unwrap();
        assert_eq!(selected(&app), None, "a point");
        app.run("select.rows", json!({"layer": layer, "rows": [b["row"]]})).unwrap();
        assert_eq!(selected(&app), Some((layer as u32, b["row"].as_u64().unwrap() as usize)));
    }
}
