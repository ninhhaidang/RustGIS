//! The Labeling pane (#617): every label setting of the selected feature layer in one place, opened
//! from a layer's right-click menu (Labeling Properties…) or View › Windows › Labeling. The ribbon
//! keeps its label groups; both run the same `labels.*` commands, so the pane holds no state of its
//! own (it reads the layer each frame, as the Symbology pane does) and every change undoes. It
//! edits one label class at a time, the one chosen in its header (#722), in a tab per topic; the
//! ribbon's label groups edit and choose the same one (#858). The map-wide settings (Lock, Pause,
//! View Unplaced) are on the ribbon only, and Convert on the ribbon and the layer's menu.

use egui::{Align2, CornerRadius, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use rustgis_engine::map::symbology::{
    DEFAULT_LABEL_CLASS, HALO_WIDTH, LABEL_SIZE, LabelClass, Labeling, MAX_LABEL_CLASSES, label_class_name,
};
use rustgis_engine::map::{LabelPlacement, Layer, Rgba};
use rustgis_engine::render::families;
use serde_json::{Value, json};

use crate::RustGisApp;
use crate::textform::LABEL_W;
use crate::theme::{Tokens, c32, regular, rgba, semibold};
use crate::widgets::text_w;

/// The placements as they sit around a feature, row by row; Automatic sits apart, above them.
const GRID: [[LabelPlacement; 3]; 3] = [
    [LabelPlacement::AboveLeft, LabelPlacement::Above, LabelPlacement::AboveRight],
    [LabelPlacement::Left, LabelPlacement::Center, LabelPlacement::Right],
    [LabelPlacement::BelowLeft, LabelPlacement::Below, LabelPlacement::BelowRight],
];

/// The viewer distances the Closest and Farthest limits offer, in metres (as the ribbon's).
const DISTANCES: [f64; 5] = [1_000.0, 10_000.0, 100_000.0, 1_000_000.0, 10_000_000.0];

pub fn show(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    let Some(l) = app.session.active_layer.and_then(|id| app.session.map().layer(id)).cloned() else {
        ui.label(RichText::new(tl!("Select a layer in the Layers pane.")).color(t.text_dim));
        return;
    };
    ui.label(RichText::new(&l.name).font(semibold(14.0)));
    if l.is_raster() {
        ui.add_space(6.0);
        ui.label(RichText::new(tl!("Labels are drawn for feature layers, not rasters.")).color(t.text_dim));
        return;
    }
    ui.add_space(6.0);
    let mut on = l.labels.enabled;
    if ui.checkbox(&mut on, tl!("Label features in this layer")).changed() {
        run(app, "labels.enable", json!({"on": on}));
    }
    let c = chosen(ui, app, &l);
    // The class in a card of its own, the tabs right under it: they are the parts of the class
    // chosen in it, not of the layer.
    ui.add_space(4.0);
    let card = egui::Frame::NONE.fill(t.pane_header).corner_radius(CornerRadius::same(8));
    card.inner_margin(egui::Margin::same(10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        classes(app, ui, &l, &c, t);
    });
    ui.add_space(8.0);
    match topic(ui, t) {
        Topic::Content => content(app, ui, &l, &c, t),
        Topic::Symbol => text_symbol(app, ui, &l, &c),
        Topic::Position => position(app, ui, &l, &c),
        Topic::Range => visibility(app, ui, &c),
    }
}

/// The pane's tabs under its header, one per topic (#858), as ArcGIS Pro's: each short, so the
/// pane doesn't grow downwards with every setting.
#[derive(Clone, Copy, PartialEq)]
enum Topic {
    Content,
    Symbol,
    Position,
    Range,
}

/// The tab row, and the tab chosen in it. It is the pane's, not a layer's or a class's: choosing
/// another keeps the topic in view. The tabs are drawn as the ribbon's, the chosen one open onto
/// the pane below with its accent bar, so they don't read as buttons; each has the icon of its
/// ribbon control. A pane too narrow for icons and names shows the names alone, and one too
/// narrow for those the icons alone, named on hover.
fn topic(ui: &mut Ui, t: &Tokens) -> Topic {
    let key = egui::Id::unique("label_pane_topic");
    let mut tab = ui.data(|d| d.get_temp::<Topic>(key)).unwrap_or(Topic::Content);
    let tabs = [
        ("Content", "unique", Topic::Content),
        ("Symbol", "g_text", Topic::Symbol),
        ("Position", "placement", Topic::Position),
        ("Range", "scale_label", Topic::Range),
    ];
    let font = |on: bool| if on { semibold(12.5) } else { regular(12.5) };
    let (pad, icon, gap, h) = (8.0, 16.0, 5.0, 30.0);
    // Measured in the chosen tab's font, so the tabs don't move as another is chosen.
    let text = tabs.map(|(label, _, _)| text_w(ui, tl!(label), semibold(12.5)));
    let room = ui.available_width() - 2.0 * (tabs.len() as f32 - 1.0);
    // A tab's room besides its name, with or without its icon.
    let extra = |icons: bool| 2.0 * pad + if icons { icon + gap } else { 0.0 };
    let fits = |icons: bool| text.iter().map(|w| w + extra(icons)).sum::<f32>() <= room;
    let (icons, names) = if fits(true) {
        (true, true)
    } else if fits(false) {
        (false, true)
    } else {
        (true, false)
    };
    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
    let base = row.max.y - 0.5;
    ui.painter().hline(row.x_range(), base, Stroke::new(1.0, t.border));
    let mut x = row.min.x;
    for ((label, name, it), w) in tabs.into_iter().zip(text) {
        let shown = tl!(label);
        let on = it == tab;
        let w = if names { extra(icons) + w } else { 36.0 };
        let r = Rect::from_min_size(pos2(x, row.min.y), vec2(w, h));
        x = r.max.x + 2.0;
        let mut resp = ui.interact(r, key.with(label), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, on, shown));
        let p = ui.painter();
        let body = Rect::from_min_max(pos2(r.min.x, r.min.y + 3.0), r.max);
        let round = CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 };
        if on {
            p.rect_filled(body, round, t.pane);
            p.rect_stroke(body, round, Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
            // Open onto the pane: the line under the tabs stops at the chosen one.
            p.rect_filled(Rect::from_min_max(pos2(r.min.x + 1.0, base - 1.0), pos2(r.max.x - 1.0, row.max.y + 1.0)), 0.0, t.pane);
            p.rect_filled(
                Rect::from_min_max(pos2(r.min.x + 8.0, r.max.y - 3.0), pos2(r.max.x - 8.0, r.max.y - 1.0)),
                CornerRadius::same(1),
                t.accent,
            );
        } else if resp.hovered() {
            p.rect_filled(body.shrink2(vec2(1.0, 2.0)), CornerRadius::same(5), t.hover);
        }
        let cy = body.center().y - 1.0;
        if !names {
            crate::icons::paint(p, Rect::from_center_size(pos2(r.center().x, cy), vec2(icon, icon)), name, t.icon, t.accent);
            resp = resp.on_hover_text(shown);
        } else {
            let mut tx = r.min.x + pad;
            if icons {
                crate::icons::paint(p, Rect::from_min_size(pos2(tx, cy - icon / 2.0), vec2(icon, icon)), name, t.icon, t.accent);
                tx += icon + gap;
            }
            let ink = if on { t.text } else { t.text_dim };
            p.text(pos2(tx, cy), Align2::LEFT_CENTER, shown, font(on), ink);
        }
        if resp.clicked() {
            tab = it;
        }
    }
    ui.data_mut(|d| d.insert_temp(key, tab));
    ui.add_space(4.0);
    tab
}

/// The label class the pane's sections and the ribbon's label groups show and edit (#722).
pub(crate) struct Class<'a> {
    pub(crate) it: &'a LabelClass,
    /// `None` for Default: the commands change it when they get no class, so a step recorded on
    /// it stays as it was before classes could be chosen.
    pub(crate) name: Option<&'a str>,
}

impl Class<'_> {
    /// A command's parameters for this class.
    pub(crate) fn params(&self, p: Value) -> Value {
        named(p, "class", self.name)
    }
}

/// `p` with `name` as its `key`, or as it is without one.
pub(crate) fn named(mut p: Value, key: &str, name: Option<&str>) -> Value {
    if let (Some(n), Some(o)) = (name, p.as_object_mut()) {
        o.insert(key.into(), json!(n));
    }
    p
}

/// The class `name` names on `labels` to change: Default when none.
pub(crate) fn class_mut<'a>(labels: &'a mut Labeling, name: Option<&str>) -> Option<&'a mut LabelClass> {
    match name {
        None => Some(&mut labels.default),
        Some(n) => labels.class_mut(n),
    }
}

/// The classes listed before Default, as many as count.
fn extras(l: &Layer) -> impl Iterator<Item = &LabelClass> {
    l.labels.classes.iter().take(MAX_LABEL_CLASSES)
}

/// A class's name as the pane shows it: Default's own once renamed, else "Default" translated.
fn shown(c: &Class) -> String {
    match c.name {
        None if c.it.name == DEFAULT_LABEL_CLASS => tl!("Default").to_string(),
        _ => c.it.name.clone(),
    }
}

/// `l` as a layer of this map of this project: another map, or another project, may reuse its id.
/// A map goes by its place and its name, as Define Projection's does: deleting a map moves the
/// next one into its place, and a copy keeps its layer ids.
pub(crate) fn layer_key(app: &RustGisApp, l: &Layer) -> egui::Id {
    egui::Id::unique((app.session.opened, app.session.project.active_map, app.session.map().name.as_str(), l.id))
}

/// Where the class chosen on a layer is kept.
fn chosen_key(app: &RustGisApp, l: &Layer) -> egui::Id {
    egui::Id::unique(("label_pane_class", layer_key(app, l)))
}

/// The class chosen on `l`, by the name it has now; Default when none is, or when the one chosen
/// went away (an undo, a script, the Assistant).
pub(crate) fn chosen<'a>(ui: &Ui, app: &RustGisApp, l: &'a Layer) -> Class<'a> {
    let name = ui.data(|d| d.get_temp::<String>(chosen_key(app, l)));
    match name.and_then(|n| extras(l).find(|c| c.name.eq_ignore_ascii_case(&n))) {
        Some(it) => Class { it, name: Some(&it.name) },
        None => Class { it: &l.labels.default, name: None },
    }
}

/// The class chooser, the pane's and the ribbon's: a class chosen in either is chosen in both. Its
/// id is made within the parent `Ui`, so the two don't clash.
pub(crate) fn class_combo(app: &RustGisApp, ui: &mut Ui, l: &Layer, c: &Class, width: f32) -> egui::Response {
    egui::ComboBox::from_id_salt("label_pane_class")
        .selected_text(shown(c))
        .width(width)
        .show_ui(ui, |ui| {
            for k in extras(l) {
                if ui.selectable_label(c.name == Some(k.name.as_str()), &k.name).clicked() {
                    choose(ui, app, l, Some(&k.name));
                }
            }
            let default = Class { it: &l.labels.default, name: None };
            if ui.selectable_label(c.name.is_none(), shown(&default)).clicked() {
                choose(ui, app, l, None);
            }
        })
        .response
}

fn choose(ui: &Ui, app: &RustGisApp, l: &Layer, name: Option<&str>) {
    let key = chosen_key(app, l);
    ui.data_mut(|d| match name {
        Some(n) => {
            d.insert_temp(key, n.to_string());
        }
        None => d.remove::<String>(key),
    });
}

/// Runs a label command from the pane on the active layer, the one it shows; a refusal shows as a
/// notice. The commands don't name the layer, as the ribbon's don't: a step recorded from them
/// (Record Task) then names it, so it replays on that layer by name (`RustGisApp::record_step`).
fn run(app: &mut RustGisApp, id: &str, params: Value) -> Option<Value> {
    app.run(id, params).map_err(|e| app.session.notify(e, true)).ok()
}

fn grid(ui: &mut Ui, id: &str, add: impl FnOnce(&mut Ui)) {
    egui::Grid::new(("label_pane", id)).num_columns(2).min_col_width(LABEL_W).spacing(vec2(8.0, 6.0)).show(ui, add);
}

/// The chosen class's field and SQL clause (#618) on one page, as ArcGIS Pro's Class tab shows the
/// fields above the expression: what the label shows, then the features it labels. Named Content,
/// not Class, as the class itself is the one chosen in the card above the tabs.
fn content(app: &mut RustGisApp, ui: &mut Ui, l: &Layer, c: &Class, t: &Tokens) {
    field(app, ui, l, c);
    ui.add_space(4.0);
    // Closed until the class has an expression.
    let owner = egui::Id::unique((layer_key(app, l), c.name));
    egui::CollapsingHeader::new(RichText::new(tl!("By Expression")).font(semibold(12.5)))
        .id_salt(owner.with("label_by_open"))
        .default_open(!c.it.by.is_empty())
        .show(ui, |ui| by_expression(app, ui, l, c, t));
    ui.add_space(4.0);
    heading(ui, "SQL Query");
    sql(app, ui, l, c, t);
}

/// The class's text, size, colour and angle per feature from expressions (`labels.by`); each
/// empty takes the field, or the class's own setting.
fn by_expression(app: &mut RustGisApp, ui: &mut Ui, l: &Layer, c: &Class, t: &Tokens) {
    ui.label(
        RichText::new(tl!(
            "Text, e.g. \"name\" || ' (' || round(\"pop\" / 1e6, 1) || ' M)', in place of the field; size in points, color as '#RRGGBB', angle in degrees clockwise. Empty: the field's or the class's own."
        ))
        .small()
        .color(t.text_dim),
    );
    let by = &c.it.by;
    let mut rows = vec![("text", tl!("Text"), &by.text), ("size", tl!("Size"), &by.size), ("color", tl!("Color"), &by.color)];
    // Labels along lines keep the line's angle (one set by command still shows, to clear it).
    if !(l.geometry_type() == rustgis_engine::data::GeometryType::Polyline && c.it.placement.along_line()) || by.angle.is_some() {
        rows.push(("angle", tl!("Angle"), &by.angle));
    }
    let owner = egui::Id::unique((layer_key(app, l), c.name));
    for (key, label, cur) in rows {
        if let Some(text) = crate::panes::expression_box(ui, owner.with(("label_by", key)), label, cur.as_deref(), &l.dataset, t) {
            run(app, "labels.by", c.params(json!({ key: text })));
        }
    }
}

fn heading(ui: &mut Ui, text: &str) {
    ui.add_space(6.0);
    ui.label(RichText::new(tl!(text)).font(semibold(12.5)));
    ui.add_space(2.0);
}

/// A class being renamed: the name typed so far, for the class it was started on.
#[derive(Clone)]
struct Renaming {
    /// The project, map, layer and class it was started on.
    owner: egui::Id,
    text: String,
    focused: bool,
    /// The pass it was last shown on: one the pane skipped (closed, or behind another tab) ends it.
    pass: u64,
}

/// The pane's header for the chosen class (#858): its chooser, a row with New, Rename, Move Up,
/// Move Down and Remove for it (#722), and Visible. Default, last in the list, can be renamed or
/// hidden, but not removed or moved.
fn classes(app: &mut RustGisApp, ui: &mut Ui, l: &Layer, c: &Class, t: &Tokens) {
    let rename_key = egui::Id::unique("label_pane_rename");
    let owner = egui::Id::unique((layer_key(app, l), c.name));
    // A rename started on another class or layer (chosen since, or by an undo), or left when the
    // pane was last hidden, is dropped.
    let pass = ui.ctx().cumulative_pass_nr();
    let renaming = ui.data(|d| d.get_temp::<Renaming>(rename_key)).filter(|r| r.owner == owner && r.pass + 1 >= pass);
    ui.data_mut(|d| d.remove::<Renaming>(rename_key));
    let editing = renaming.is_some();
    grid(ui, "class_pick", |ui| {
        let input_label = ui.label(tl!("Class"));
        match renaming {
            Some(r) => rename_box(app, ui, l, c, rename_key, input_label.id, r),
            None => {
                class_combo(app, ui, l, c, 180.0).labelled_by(input_label.id);
            }
        }
        ui.end_row();
    });
    // Off while the class is renamed: a click on them ends the rename (the box loses focus), so
    // they would act on the class by the name it had before.
    ui.add_enabled_ui(!editing, |ui| {
        let count = extras(l).count();
        let at = c.name.and_then(|n| extras(l).position(|k| k.name == n));
        ui.horizontal(|ui| {
            let full = l.labels.classes.len() >= MAX_LABEL_CLASSES;
            let max = MAX_LABEL_CLASSES.to_string();
            let new = ui
                .add_enabled(!full, egui::Button::new(tl!("New")))
                .on_hover_text(tl!("Add a class with this one's field, text symbol, placement and range."))
                .on_disabled_hover_text(crate::i18n::fmt("A layer keeps at most {max} label classes besides Default.", &[("max", &max)]));
            if new.clicked() {
                let p = named(json!({"name": new_name(&l.labels)}), "from", c.name);
                if let Some(added) = run(app, "labels.class.add", p) {
                    choose(ui, app, l, added.get("class").and_then(Value::as_str));
                }
            }
            if ui.button(tl!("Rename")).clicked() {
                let r = Renaming { owner, text: c.it.name.clone(), focused: false, pass };
                ui.data_mut(|d| d.insert_temp(rename_key, r));
            }
            let up = at.filter(|i| *i > 0).map(|i| i - 1);
            let down = at.filter(|i| i + 1 < count).map(|i| i + 1);
            let moves = [
                ("up", "Move Up", "Move up: labels of the classes listed first are placed first when they compete for room.", up),
                ("down", "Move Down", "Move down: labels of the classes listed first are placed first when they compete for room.", down),
            ];
            for (icon, label, tip, to) in moves {
                let moved = ui
                    .add_enabled_ui(to.is_some(), |ui| {
                        // Named for screen readers (and tests) by its label, as the Bookmarks
                        // pane's icons; its one tooltip says why the order matters.
                        let enabled = ui.is_enabled();
                        let r = crate::widgets::icon_button(ui, icon, tl!(tip));
                        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, tl!(label)));
                        r
                    })
                    .inner;
                if moved.clicked() {
                    run(app, "labels.class.move", c.params(json!({"to": to})));
                }
            }
            // At the far end and in red: it is the one that deletes, and its name is close to
            // Rename's.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let remove = ui
                    .add_enabled(at.is_some(), egui::Button::new(RichText::new(tl!("Remove")).color(t.red)))
                    .on_disabled_hover_text(tl!("Default can't be removed. You can rename or hide it."));
                if remove.clicked() {
                    // Default chosen outright: a class added later by the removed one's name (an
                    // undo, a script) isn't chosen again by the name left behind.
                    if run(app, "labels.class.remove", c.params(json!({}))).is_some() {
                        choose(ui, app, l, None);
                    }
                }
            });
        });
        let mut visible = c.it.visible;
        if ui.checkbox(&mut visible, tl!("Visible")).on_hover_text(tl!("A hidden class leaves its features unlabelled.")).changed() {
            run(app, "labels.class.update", c.params(json!({"visible": visible})));
        }
    });
}

/// The box the chosen class is renamed in: Enter (or clicking away) renames it, Escape leaves it as
/// it was. A name refused (empty, or another class's) keeps the box open, with the notice saying why.
fn rename_box(app: &mut RustGisApp, ui: &mut Ui, l: &Layer, c: &Class, key: egui::Id, label: egui::Id, mut r: Renaming) {
    let keys = egui::EventFilter { escape: true, ..Default::default() };
    let edit = ui.add(egui::TextEdit::singleline(&mut r.text).desired_width(180.0).event_filter(keys)).labelled_by(label);
    if !r.focused {
        edit.request_focus();
        r.focused = true;
    }
    if edit.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        return;
    }
    if edit.lost_focus() {
        let name = label_class_name(&r.text);
        if name == c.it.name {
            return;
        }
        if run(app, "labels.class.rename", json!({"class": c.it.name, "name": name})).is_some() {
            // Default stays chosen by no name; another class goes by its new one.
            if c.name.is_some() {
                choose(ui, app, l, Some(&name));
            }
            return;
        }
        r.focused = false;
    }
    r.pass = ui.ctx().cumulative_pass_nr();
    ui.data_mut(|d| d.insert_temp(key, r));
}

/// The first "Class N" no class on `labels` goes by.
fn new_name(labels: &Labeling) -> String {
    (1..=MAX_LABEL_CLASSES + 2)
        .map(|n| crate::i18n::fmt("Class {n}", &[("n", &n.to_string())]))
        .find(|n| labels.class(n).is_none())
        .unwrap_or_default()
}

/// How a list with a search box closes: on a click outside it or a pick ([`end_search`]), not on
/// any click (egui's default), which would close it on clicking its search box.
pub(crate) const SEARCHED_LIST_CLOSES: egui::PopupCloseBehavior = egui::PopupCloseBehavior::CloseOnClickOutside;

/// A list's search box, as its first row: the text kept for `id` while the list is reopened,
/// lowercased to match names by. The Field and Font boxes share it, so they search alike.
pub(crate) fn list_search(ui: &mut Ui, id: egui::Id, hint: &str) -> String {
    let mut text = ui.data(|d| d.get_temp::<String>(id)).unwrap_or_default();
    if ui.add(egui::TextEdit::singleline(&mut text).hint_text(hint)).changed() {
        ui.data_mut(|d| d.insert_temp(id, text.clone()));
    }
    text.to_lowercase()
}

/// A pick ends the search and closes the list (a searched list stays open on other clicks, its
/// search box's: `SEARCHED_LIST_CLOSES`): the list opens whole next time.
pub(crate) fn end_search(ui: &Ui, id: egui::Id) {
    ui.data_mut(|d| d.remove::<String>(id));
    ui.close();
}

/// The field the class labels with, chosen as the font is: the box shows it (or says it is gone,
/// as a style or a service can leave it), and the list has a search for a layer with many.
fn field(app: &mut RustGisApp, ui: &mut Ui, l: &Layer, c: &Class) {
    let fields = &l.dataset.fields;
    let cur = &c.it.field;
    // A layer without fields leaves its classes on none.
    let shown = match l.dataset.field_index(cur) {
        Some(_) => cur.clone(),
        None if cur.is_empty() => tl!("<None>").to_string(),
        None => crate::i18n::fmt("{name} (not in the layer)", &[("name", cur)]),
    };
    let search_id = layer_key(app, l).with("label_pane_field_search");
    grid(ui, "field", |ui| {
        let input_label = ui.label(tl!("Field"));
        egui::ComboBox::from_id_salt("label_pane_field")
            .selected_text(shown)
            .width(180.0)
            .height(320.0)
            .close_behavior(SEARCHED_LIST_CLOSES)
            .show_ui(ui, |ui| {
                let search = list_search(ui, search_id, tl!("Search fields"));
                let mut matching = fields.iter().filter(|f| f.name.to_lowercase().contains(&search)).peekable();
                if fields.is_empty() {
                    ui.weak(tl!("This layer has no fields."));
                } else if matching.peek().is_none() {
                    ui.weak(tl!("No fields match"));
                }
                for f in matching {
                    let on = f.name.eq_ignore_ascii_case(cur);
                    // Any pick ends the search, the chosen field's too: the box closes on it.
                    if ui.selectable_label(on, &f.name).clicked() {
                        if !on {
                            run(app, "labels.field", c.params(json!({"field": f.name})));
                        }
                        end_search(ui, search_id);
                    }
                }
            })
            .response
            .labelled_by(input_label.id);
        ui.end_row();
    });
}

/// The class's SQL clause: its labels, and their style, go only to the features that match it;
/// none takes every feature.
fn sql(app: &mut RustGisApp, ui: &mut Ui, l: &Layer, c: &Class, t: &Tokens) {
    let labels = &l.labels;
    // One note, about what decides the labels first: labels off, then a hidden class, then
    // other visible classes (those the map draws, Default included). Each is a whole sentence,
    // as joined ones would read in the wrong order right to left.
    if !labels.enabled {
        ui.label(RichText::new(tl!("Labels are off for this layer: the clause applies once they are on.")).small().color(t.text_dim));
    } else if !c.it.visible {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(tl!("This class is hidden, so it labels nothing.")).small().color(t.text_dim));
            if ui.small_button(tl!("Show labels")).clicked() {
                run(app, "labels.class.update", c.params(json!({"visible": true})));
            }
        });
    } else if labels.in_order().filter(|k| k.visible).count() > 1 {
        let said = tl!("Each class labels the features its own clause matches, so a feature can get a label from more than one.");
        ui.label(RichText::new(said).small().color(t.text_dim));
    }
    // Counted among the features the layer draws at the map's scale, as its labels are.
    let scale = app.session.map().scale();
    let count = |w: &str| -> Result<(usize, usize), String> {
        let e = rustgis_engine::data::query::parse(w, &l.dataset)?;
        let rows = l.drawn_rows(scale);
        let of = rows.iter().filter(|r| **r).count();
        let n = rows.iter().enumerate().filter(|(i, r)| **r && rustgis_engine::data::query::eval(&e, &l.dataset, *i)).count();
        Ok((n, of))
    };
    // A draft belongs to this class of this layer.
    let owner = egui::Id::unique((layer_key(app, l), c.name));
    // A count holds for this project state at this scale (display filters follow the scale).
    let version = egui::Id::unique((app.session.revision, scale.to_bits())).value();
    let applied = match crate::sqlbox::show(ui, ("label_sql", owner), owner, version, c.it.where_.as_deref(), &l.dataset, count) {
        Some(crate::sqlbox::Apply::Clause(w)) => json!({"where": w}),
        Some(crate::sqlbox::Apply::Clear) => json!({"where": null}),
        None => return,
    };
    run(app, "labels.class.update", c.params(applied));
}

fn text_symbol(app: &mut RustGisApp, ui: &mut Ui, l: &Layer, c: &Class) {
    grid(ui, "symbol", |ui| {
        font(app, ui, l, c);
        ui.end_row();
        let input_label = ui.label(tl!("Size"));
        // The whole range `labels.style` accepts (#457); the map follows a drag and letting
        // go runs the command once, so a drag is one undo step.
        let mut size = f64::from(c.it.size);
        let r = ui
            .add(egui::DragValue::new(&mut size).update_while_editing(false).range(LABEL_SIZE).speed(0.1).suffix(" pt"))
            .labelled_by(input_label.id);
        let id = l.id;
        crate::ribbon::live_drag(
            app,
            ui,
            &r,
            f64::from(c.it.size),
            size,
            |app, x| {
                if let Some(k) = app.session.map_mut().layer_mut(id).and_then(|l| class_mut(&mut l.labels, c.name)) {
                    k.size = x.clamp(*LABEL_SIZE.start(), *LABEL_SIZE.end()) as f32;
                }
            },
            |app, x| {
                run(app, "labels.style", c.params(json!({"size": x})));
            },
        );
        ui.end_row();
        ui.label(tl!("Color"));
        let mut color = c32(c.it.color);
        if crate::widgets::color_button(ui, &mut color).changed() {
            color_drag(app, ui, l, c.name, rgba(color));
        }
        ui.end_row();
        ui.label("");
        ui.horizontal(|ui| {
            let mut bold = c.it.bold;
            if ui.checkbox(&mut bold, tl!("Bold")).changed() {
                run(app, "labels.style", c.params(json!({"bold": bold})));
            }
            let mut italic = c.it.italic;
            if ui.checkbox(&mut italic, tl!("Italic")).changed() {
                run(app, "labels.style", c.params(json!({"italic": italic})));
            }
            let mut halo = c.it.halo;
            if ui.checkbox(&mut halo, tl!("Halo")).changed() {
                run(app, "labels.style", c.params(json!({"halo": halo})));
            }
        });
        ui.end_row();
        letter_spacing(app, ui, l, c);
        ui.end_row();
        wrapping(app, ui, l, c);
        if c.it.halo {
            halo_style(app, ui, l, c);
        }
        background(app, ui, l, c);
    });
}

/// A rectangle behind the text: its fill and outline (each may be none), the outline's width, how
/// far it reaches past the text, and its corners' radius.
fn background(app: &mut RustGisApp, ui: &mut Ui, l: &Layer, c: &Class) {
    ui.label("");
    let mut on = c.it.background.is_some();
    if ui.checkbox(&mut on, tl!("Background")).on_hover_text(tl!("A rectangle behind the text.")).changed() {
        run(app, "labels.style", c.params(json!({"background": on})));
    }
    ui.end_row();
    let Some(b) = c.it.background.clone() else { return };
    for (label, part, cur) in [("Fill", Part::Fill, b.fill), ("Outline", Part::Outline, b.stroke)] {
        ui.label(tl!(label));
        ui.horizontal(|ui| {
            let mut none = cur.is_none();
            if ui.checkbox(&mut none, tl!("None")).changed() {
                // Back on in the default's colour (and an outline as wide, if it had none).
                let d = rustgis_engine::map::LabelBackground::default();
                let change = match (part, none) {
                    (Part::Fill, true) => json!({"fill": ""}),
                    (Part::Fill, false) => json!({"fill": d.fill.map(|f| f.hex())}),
                    (_, true) => json!({"stroke": ""}),
                    (_, false) => json!({"stroke": d.stroke.map(|f| f.hex()), "strokeWidth": b.stroke_width.max(d.stroke_width)}),
                };
                run(app, "labels.style", c.params(json!({"background": change})));
            }
            if let Some(cur) = cur {
                let mut color = c32(cur);
                if crate::widgets::color_button(ui, &mut color).changed() {
                    pick_drag(app, ui, l, c.name, part, rgba(color));
                }
            }
        });
        ui.end_row();
    }
    let id = l.id;
    // A number of the background's, dragged live and set by `labels.style` once let go.
    let number = |app: &mut RustGisApp, ui: &mut Ui, label: &str, cur: f32, range: std::ops::RangeInclusive<f64>, key: &'static str| {
        let input_label = ui.label(tl!(label));
        let mut v = f64::from(cur);
        let r = ui
            .add(egui::DragValue::new(&mut v).update_while_editing(false).range(range.clone()).speed(0.05).suffix(" pt"))
            .labelled_by(input_label.id);
        crate::ribbon::live_drag(
            app,
            ui,
            &r,
            f64::from(cur),
            v,
            |app, x| {
                let x = x.clamp(*range.start(), *range.end()) as f32;
                if let Some(b) =
                    app.session.map_mut().layer_mut(id).and_then(|l| class_mut(&mut l.labels, c.name)).and_then(|k| k.background.as_mut())
                {
                    match key {
                        "strokeWidth" => b.stroke_width = x,
                        "radius" => b.radius = x,
                        "across" => b.padding[0] = x,
                        _ => b.padding[1] = x,
                    }
                }
            },
            |app, x| {
                let change = match key {
                    "across" => json!({"padding": [x, b.padding[1]]}),
                    "upDown" => json!({"padding": [b.padding[0], x]}),
                    other => json!({other: x}),
                };
                run(app, "labels.style", c.params(json!({"background": change})));
            },
        );
        ui.end_row();
    };
    use rustgis_engine::map::symbology::{BACKGROUND_PADDING, BACKGROUND_STROKE};
    if b.stroke.is_some() {
        number(app, ui, "Outline width", b.stroke_width, BACKGROUND_STROKE, "strokeWidth");
    }
    number(app, ui, "Padding across", b.padding[0], BACKGROUND_PADDING, "across");
    number(app, ui, "Padding up and down", b.padding[1], BACKGROUND_PADDING, "upDown");
    number(app, ui, "Corner radius", b.radius, BACKGROUND_PADDING, "radius");
}

/// Wrapping: a character the text breaks at (typed, set once the field is left), a longest line
/// in characters (0: none), and how the lines line up.
fn wrapping(app: &mut RustGisApp, ui: &mut Ui, l: &Layer, c: &Class) {
    use rustgis_engine::map::layout::TextAlign;
    use rustgis_engine::map::symbology::MAX_WRAP_LENGTH;
    let input_label = ui.label(tl!("Wrap at"));
    let key = layer_key(app, l).with(("label_pane_wrap_char", c.name));
    let current = c.it.wrap_char.clone().unwrap_or_default();
    let mut buf = ui.data(|d| d.get_temp::<String>(key)).unwrap_or_else(|| current.clone());
    let r = ui
        .add(egui::TextEdit::singleline(&mut buf).char_limit(rustgis_engine::map::symbology::MAX_WRAP_CHAR).desired_width(60.0))
        .labelled_by(input_label.id)
        .on_hover_text(tl!("A line break wherever this occurs, left out (a space puts each word on its own line)."));
    if r.has_focus() {
        ui.data_mut(|d| d.insert_temp(key, buf));
    } else {
        ui.data_mut(|d| d.remove::<String>(key));
        if r.lost_focus() && buf != current {
            run(app, "labels.style", c.params(json!({"wrapChar": buf})));
        }
    }
    ui.end_row();
    let input_label = ui.label(tl!("Wrap length"));
    let mut n = f64::from(c.it.wrap_length);
    let r = ui
        .add(
            egui::DragValue::new(&mut n)
                .update_while_editing(false)
                .range(0..=MAX_WRAP_LENGTH)
                .speed(0.2)
                .max_decimals(0)
                .custom_formatter(|v, _| {
                    if v <= 0.0 { tl!("Off").to_string() } else { crate::i18n::fmt("{n} characters", &[("n", &v.to_string())]) }
                }),
        )
        .labelled_by(input_label.id)
        .on_hover_text(tl!("Lines longer than this are broken between their words."));
    let id = l.id;
    crate::ribbon::live_drag(
        app,
        ui,
        &r,
        f64::from(c.it.wrap_length),
        n,
        |app, x| {
            if let Some(k) = app.session.map_mut().layer_mut(id).and_then(|l| class_mut(&mut l.labels, c.name)) {
                k.wrap_length = x.clamp(0.0, f64::from(MAX_WRAP_LENGTH)) as u32;
            }
        },
        |app, x| {
            run(app, "labels.style", c.params(json!({"wrapLength": x.round()})));
        },
    );
    ui.end_row();
    ui.label(tl!("Line alignment"));
    ui.horizontal(|ui| {
        for a in TextAlign::ALL {
            if ui.selectable_label(c.it.line_align == a, tl!(a.label())).clicked() && c.it.line_align != a {
                run(app, "labels.style", c.params(json!({"lineAlign": a.code()})));
            }
        }
    });
    ui.end_row();
}

/// The space between the letters, in points (negative: tighter).
fn letter_spacing(app: &mut RustGisApp, ui: &mut Ui, l: &Layer, c: &Class) {
    let input_label = ui.label(tl!("Letter spacing"));
    let (lo, hi) = (rustgis_engine::map::layout::MIN_LETTER_SPACING, rustgis_engine::map::layout::MAX_LETTER_SPACING);
    let mut spacing = f64::from(c.it.spacing);
    let r = ui
        .add(egui::DragValue::new(&mut spacing).update_while_editing(false).range(lo..=hi).speed(0.05).suffix(" pt"))
        .labelled_by(input_label.id);
    let id = l.id;
    crate::ribbon::live_drag(
        app,
        ui,
        &r,
        f64::from(c.it.spacing),
        spacing,
        |app, x| {
            if let Some(k) = app.session.map_mut().layer_mut(id).and_then(|l| class_mut(&mut l.labels, c.name)) {
                k.spacing = x.clamp(lo, hi) as f32;
            }
        },
        |app, x| {
            run(app, "labels.style", c.params(json!({"spacing": x})));
        },
    );
}

/// The halo's colour (automatic: white or dark, whichever stands out from the text) and width:
/// the thin halo, or one of its own width in points.
fn halo_style(app: &mut RustGisApp, ui: &mut Ui, l: &Layer, c: &Class) {
    ui.label(tl!("Halo color"));
    ui.horizontal(|ui| {
        let mut auto = c.it.halo_color.is_none();
        if ui.checkbox(&mut auto, tl!("Automatic")).on_hover_text(tl!("White behind dark text, dark behind light.")).changed() {
            let v = if auto { json!("") } else { json!(c.it.halo_color_for(c.it.color).hex()) };
            run(app, "labels.style", c.params(json!({"haloColor": v})));
        }
        if !auto {
            let mut color = c32(c.it.halo_color_for(c.it.color));
            if crate::widgets::color_button(ui, &mut color).changed() {
                pick_drag(app, ui, l, c.name, Part::Halo, rgba(color));
            }
        }
    });
    ui.end_row();
    let input_label = ui.label(tl!("Halo width"));
    ui.horizontal(|ui| {
        let mut thin = c.it.halo_width.is_none();
        if ui.checkbox(&mut thin, tl!("Thin")).changed() {
            let v = if thin { json!(null) } else { json!(1.0) };
            run(app, "labels.style", c.params(json!({"haloWidth": v})));
        }
        let Some(width) = c.it.halo_width else { return };
        let mut w = f64::from(width);
        let r = ui
            .add(egui::DragValue::new(&mut w).update_while_editing(false).range(HALO_WIDTH).speed(0.05).suffix(" pt"))
            .labelled_by(input_label.id);
        let id = l.id;
        crate::ribbon::live_drag(
            app,
            ui,
            &r,
            f64::from(width),
            w,
            |app, x| {
                if let Some(k) = app.session.map_mut().layer_mut(id).and_then(|l| class_mut(&mut l.labels, c.name)) {
                    k.halo_width = Some(x.clamp(*HALO_WIDTH.start(), *HALO_WIDTH.end()) as f32);
                }
            },
            |app, x| {
                run(app, "labels.style", c.params(json!({"haloWidth": x})));
            },
        );
    });
    ui.end_row();
}

/// The most installed families the font list shows at once (a machine can have thousands); the
/// rest are found by typing part of a name.
const MAX_FONTS_SHOWN: usize = 200;

/// The installed family the labels are drawn in: the label font, or one picked from the
/// installed families (found by typing part of the name). A family this machine lacks, from a
/// project made on another, is shown as missing and drawn in the label font.
fn font(app: &mut RustGisApp, ui: &mut Ui, l: &Layer, c: &Class) {
    let input_label = ui.label(tl!("Font"));
    let search_id = layer_key(app, l).with("label_pane_font_search");
    let picked = family_picker(ui, "label_pane_font", search_id, c.it.font.as_deref());
    picked.response.labelled_by(input_label.id);
    if let Some(name) = picked.chosen {
        run(app, "labels.style", c.params(json!({"font": name.unwrap_or_default()})));
    }
}

/// What [`family_picker`] showed, and what was picked: `Some(None)` the label font, `Some(Some(name))`
/// an installed family.
pub(crate) struct Picked {
    pub response: egui::Response,
    pub chosen: Option<Option<String>>,
}

/// A list of the installed families to pick from (found by typing part of the name, kept under
/// `search_id`), the label font first; `cur` the one in use (none: the label font). A family this
/// machine lacks, from a project made on another, is shown as missing. Until the installed fonts
/// are indexed (on a thread of their own), a disabled button says so.
pub(crate) fn family_picker(ui: &mut Ui, salt: impl std::hash::Hash + std::fmt::Debug, search_id: egui::Id, cur: Option<&str>) -> Picked {
    let Some(installed) = families::installed_now() else {
        families::warm_up();
        let response = ui.add_enabled(false, egui::Button::new(tl!("Reading the installed fonts…")));
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        return Picked { response, chosen: None };
    };
    let shown = match cur {
        None => tl!("Label font").to_string(),
        Some(name) if families::find(name).is_some() => name.to_string(),
        Some(name) => crate::i18n::fmt("{name} (not installed)", &[("name", name)]),
    };
    let mut chosen = None;
    let response = egui::ComboBox::from_id_salt(salt)
        .selected_text(shown)
        .width(180.0)
        .height(320.0)
        .close_behavior(SEARCHED_LIST_CLOSES)
        .show_ui(ui, |ui| {
            let search = list_search(ui, search_id, tl!("Search fonts"));
            if ui.selectable_label(cur.is_none(), tl!("Label font")).clicked() {
                if cur.is_some() {
                    chosen = Some(None);
                }
                end_search(ui, search_id);
            }
            let mut matching = installed.iter().filter(|f| f.listed && (search.is_empty() || f.name.to_lowercase().contains(&search)));
            for f in matching.by_ref().take(MAX_FONTS_SHOWN) {
                let on = cur.is_some_and(|c| c.eq_ignore_ascii_case(&f.name));
                if ui.selectable_label(on, &f.name).clicked() {
                    if !on {
                        chosen = Some(Some(f.name.clone()));
                    }
                    end_search(ui, search_id);
                }
            }
            let more = matching.count();
            if more > 0 {
                ui.weak(crate::i18n::fmt("{count} more: type part of a name", &[("count", &more.to_string())]));
            }
        })
        .response;
    Picked { response, chosen }
}

fn position(app: &mut RustGisApp, ui: &mut Ui, l: &Layer, c: &Class) {
    let cur = c.it.placement;
    let pick = |app: &mut RustGisApp, ui: &mut Ui, pl: LabelPlacement| {
        if ui.add_sized(vec2(92.0, 22.0), egui::Button::selectable(cur == pl, tl!(pl.label())).truncate()).clicked() && cur != pl {
            run(app, "labels.placement", c.params(json!({"position": pl.code()})));
        }
    };
    ui.horizontal(|ui| pick(app, ui, LabelPlacement::Auto));
    ui.add_space(2.0);
    egui::Grid::new(("label_pane", "position")).spacing(vec2(4.0, 4.0)).show(ui, |ui| {
        for row in GRID {
            for pl in row {
                pick(app, ui, pl);
            }
            ui.end_row();
        }
    });
    // Points: the other places round the point, tried when the label's own is taken (#1011).
    if l.geometry_type() == rustgis_engine::data::GeometryType::Point {
        ui.add_space(4.0);
        let mut on = c.it.other_positions;
        let help = tl!("When its place is taken, the label tries the other places round the point, corners first.");
        if ui.checkbox(&mut on, tl!("Try other places round the point")).on_hover_text(help).changed() {
            run(app, "labels.placement", c.params(json!({"otherPositions": on})));
        }
    }
    // Lines: the text along them, straight or bending with them.
    if l.geometry_type() == rustgis_engine::data::GeometryType::Polyline {
        ui.add_space(4.0);
        ui.label(RichText::new(tl!("Along lines")).size(11.5));
        ui.horizontal(|ui| {
            pick(app, ui, LabelPlacement::Parallel);
            pick(app, ui, LabelPlacement::Curved);
        });
        if cur.along_line() {
            repeat(app, ui, l, c);
            // One label a line: other places along it, tried when its own won't do (#1187). Each
            // repeat is already a place of its own.
            if c.it.repeat.is_none() {
                ui.add_space(4.0);
                let mut on = c.it.other_positions;
                let help = tl!("If its place is taken or (curved) the line bends too much, the label tries other places along the line.");
                if ui.checkbox(&mut on, tl!("Try other places along the line")).on_hover_text(help).changed() {
                    run(app, "labels.placement", c.params(json!({"otherPositions": on})));
                }
            }
        }
    }
    ui.add_space(6.0);
    offset(app, ui, l, c);
}

/// Along a line, a label every so many points, spaced out from the one at its middle; off, the one
/// at its middle.
fn repeat(app: &mut RustGisApp, ui: &mut Ui, l: &Layer, c: &Class) {
    use rustgis_engine::map::symbology::LABEL_REPEAT;
    let id = l.id;
    grid(ui, "repeat", |ui| {
        let input_label = ui.label(tl!("Repeat"));
        ui.horizontal(|ui| {
            let mut on = c.it.repeat.is_some();
            if ui.checkbox(&mut on, "").labelled_by(input_label.id).on_hover_text(tl!("A label every so far along the line.")).changed() {
                // On, every 2 inches of paper (144 points) to start with.
                let v = if on { json!(144.0) } else { json!(null) };
                run(app, "labels.placement", c.params(json!({"repeat": v})));
            }
            let Some(every) = c.it.repeat else { return };
            let mut v = f64::from(every);
            let r = ui
                .add(
                    egui::DragValue::new(&mut v)
                        .update_while_editing(false)
                        .range(LABEL_REPEAT)
                        .speed(1.0)
                        .max_decimals(1)
                        // The space stays out of the key: the whitespace hook strips it from catalog lines.
                        .prefix(format!("{} ", tl!("every")))
                        .suffix(" pt"),
                )
                .labelled_by(input_label.id);
            crate::ribbon::live_drag(
                app,
                ui,
                &r,
                f64::from(every),
                v,
                |app, x| {
                    if let Some(k) = app.session.map_mut().layer_mut(id).and_then(|l| class_mut(&mut l.labels, c.name)) {
                        k.repeat = rustgis_engine::map::symbology::label_repeat(x);
                    }
                },
                |app, x| {
                    run(app, "labels.placement", c.params(json!({"repeat": x})));
                },
            );
        });
        ui.end_row();
    });
}

/// How far the label moves from where its placement puts it, right and down, in points.
fn offset(app: &mut RustGisApp, ui: &mut Ui, l: &Layer, c: &Class) {
    use rustgis_engine::map::symbology::LABEL_OFFSET;
    let id = l.id;
    let cur = c.it.offset;
    grid(ui, "offset", |ui| {
        for (i, label) in [(0, "Offset right"), (1, "Offset down")] {
            let input_label = ui.label(tl!(label));
            let mut v = f64::from(cur.get(i).copied().unwrap_or(0.0));
            let r = ui
                .add(egui::DragValue::new(&mut v).update_while_editing(false).range(LABEL_OFFSET).speed(0.1).suffix(" pt"))
                .labelled_by(input_label.id)
                .on_hover_text(tl!("A label along a line isn't moved."));
            crate::ribbon::live_drag(
                app,
                ui,
                &r,
                f64::from(cur.get(i).copied().unwrap_or(0.0)),
                v,
                |app, x| {
                    if let Some(k) = app.session.map_mut().layer_mut(id).and_then(|l| class_mut(&mut l.labels, c.name))
                        && let Some(o) = k.offset.get_mut(i)
                    {
                        *o = x.clamp(*LABEL_OFFSET.start(), *LABEL_OFFSET.end()) as f32;
                    }
                },
                |app, x| {
                    let mut o = cur.map(f64::from);
                    if let Some(slot) = o.get_mut(i) {
                        *slot = x;
                    }
                    run(app, "labels.placement", c.params(json!({"offset": o})));
                },
            );
            ui.end_row();
        }
    });
}

fn visibility(app: &mut RustGisApp, ui: &mut Ui, c: &Class) {
    grid(ui, "range", |ui| {
        for (label, cur, key) in [("Out Beyond", c.it.min_scale, "min"), ("In Beyond", c.it.max_scale, "max")] {
            let input_label = ui.label(tl!(label));
            egui::ComboBox::from_id_salt(("label_pane_scale", key))
                .selected_text(crate::ribbon::scale_label(cur))
                .width(140.0)
                .show_ui(ui, |ui| {
                    for s in std::iter::once(None).chain(crate::ribbon::SCALES.iter().map(|s| Some(*s))) {
                        if ui.selectable_label(s == cur, crate::ribbon::scale_label(s)).clicked() {
                            run(app, "labels.scaleRange", c.params(json!({key: s})));
                        }
                    }
                })
                .response
                .labelled_by(input_label.id);
            ui.end_row();
        }
        for (label, cur, key) in [("Closest", c.it.closest, "closest"), ("Farthest", c.it.farthest, "farthest")] {
            let input_label = ui.label(tl!(label));
            let imperial = app.ui.measurement_imperial;
            let text = |d: Option<f64>| d.map(|d| crate::measure_units::distance(d, imperial)).unwrap_or_else(|| tl!("<None>").to_string());
            egui::ComboBox::from_id_salt(("label_pane_distance", key))
                .selected_text(text(cur))
                .width(140.0)
                .show_ui(ui, |ui| {
                    for d in std::iter::once(None).chain(DISTANCES.iter().map(|d| Some(*d))) {
                        if ui.selectable_label(d == cur, text(d)).clicked() {
                            run(app, "labels.distanceRange", c.params(json!({key: d})));
                        }
                    }
                })
                .response
                .labelled_by(input_label.id);
            ui.end_row();
        }
    });
    let limited = [c.it.min_scale, c.it.max_scale, c.it.closest, c.it.farthest].iter().any(Option::is_some);
    if ui
        .add_enabled(limited, egui::Button::new(tl!("Clear Limits")))
        .on_hover_text(tl!("Draw labels at every scale and distance."))
        .clicked()
    {
        run(app, "labels.scaleRange", c.params(json!({"clear": true})));
    }
}

/// Where a label colour being dragged in the pane or ribbon's picker waits.
const COLOR_DRAG: &str = "label_pane_color_drag";

/// A label colour being dragged: it previews in the project, and when the pointer is let go
/// `finish_color_drag` puts `before` back and runs labels.style once, so a pick is one undo step
/// holding the pre-drag colour (as the Symbology pane's class colours do, #314).
#[derive(Clone)]
struct ColorDrag {
    layer: u32,
    /// The class picked on: `None` for Default.
    class: Option<String>,
    /// Which of its colours.
    part: Part,
    /// (A halo's `None` is its automatic colour; a background's, none.)
    before: Option<Rgba>,
    color: Rgba,
}

/// Which of a label class's colours a pick changes.
#[derive(Clone, Copy, PartialEq)]
enum Part {
    Text,
    Halo,
    /// Its background's fill and outline.
    Fill,
    Outline,
}

/// The colour a pick changes on a class: the text's, the halo's own (`None`: automatic), or its
/// background's (`None`: none, or no background).
fn picked(k: &LabelClass, part: Part) -> Option<Rgba> {
    match part {
        Part::Text => Some(k.color),
        Part::Halo => k.halo_color,
        Part::Fill => k.background.as_ref().and_then(|b| b.fill),
        Part::Outline => k.background.as_ref().and_then(|b| b.stroke),
    }
}

fn set_picked(k: &mut LabelClass, part: Part, c: Option<Rgba>) {
    match (part, c) {
        (Part::Text, Some(c)) => k.color = c,
        (Part::Text, None) => {}
        (Part::Halo, c) => k.halo_color = c,
        (Part::Fill, c) => {
            if let Some(b) = &mut k.background {
                b.fill = c;
            }
        }
        (Part::Outline, c) => {
            if let Some(b) = &mut k.background {
                b.stroke = c;
            }
        }
    }
}

/// Previews a colour pick on its layer's class (`None`: Default) until `finish_color_drag` commits
/// the completed gesture.
pub(crate) fn color_drag(app: &mut RustGisApp, ui: &Ui, l: &Layer, class: Option<&str>, color: Rgba) {
    pick_drag(app, ui, l, class, Part::Text, color);
}

fn pick_drag(app: &mut RustGisApp, ui: &Ui, l: &Layer, class: Option<&str>, part: Part, color: Rgba) {
    let key = egui::Id::unique(COLOR_DRAG);
    let mut pending = ui.data(|d| d.get_temp::<ColorDrag>(key));
    // One left pending on another layer, class or colour (the selection changed mid-pick, from
    // the Assistant or a shortcut) is finished first rather than dropped with its preview in place.
    if let Some(other) = pending.take_if(|d| d.layer != l.id || d.class.as_deref() != class || d.part != part) {
        ui.data_mut(|d| d.remove::<ColorDrag>(key));
        apply_color_drag(app, other);
    }
    let Some(now) = class.map_or(Some(&l.labels.default), |n| l.labels.class(n)).map(|k| picked(k, part)) else { return };
    // The colour before the first frame of this pick, kept while it goes on.
    let before = pending.map_or(now, |d| d.before);
    ui.data_mut(|d| d.insert_temp(key, ColorDrag { layer: l.id, class: class.map(String::from), part, before, color }));
    if let Some(k) = app.session.map_mut().layer_mut(l.id).and_then(|l| class_mut(&mut l.labels, class)) {
        set_picked(k, part, Some(color));
    }
    app.session.revision = app.session.revision.wrapping_add(1);
}

/// Ends a colour pick once the pointer is let go. Called after the panes (from `panes::show`), so
/// it still ends if the pane closes meanwhile. A change made with no pointer down (typed in the
/// picker's hex field, or with the keyboard) ends on the frame it is made: each is its own undo
/// step, and none is lost, as in the Symbology pane's class colours.
pub(crate) fn finish_color_drag(app: &mut RustGisApp, ui: &Ui) {
    // Still dragging: the pointer is down and the window has focus (a release outside a window
    // that lost focus may never arrive).
    if ui.input(|i| i.pointer.primary_down() && i.focused) {
        return;
    }
    let key = egui::Id::unique(COLOR_DRAG);
    let Some(drag) = ui.data_mut(|d| {
        let drag = d.get_temp::<ColorDrag>(key);
        d.remove::<ColorDrag>(key);
        drag
    }) else {
        return;
    };
    apply_color_drag(app, drag);
}

/// Puts the pre-pick colour back and runs the pick's labels.style: one undo step.
fn apply_color_drag(app: &mut RustGisApp, drag: ColorDrag) {
    // A layer or class removed mid-pick took its preview with it: there is nothing to restore or
    // style.
    let Some(k) = app.session.map_mut().layer_mut(drag.layer).and_then(|l| class_mut(&mut l.labels, drag.class.as_deref())) else {
        return;
    };
    set_picked(k, drag.part, drag.before);
    if drag.before != Some(drag.color) {
        // On the layer the pick was made on, as the active one: the command reaches it by id
        // (a name two layers share would reach the first), and a recorded step names it, as
        // for the pane's other commands. The selection, if it moved mid-pick, is put back (as
        // `Session::run` does to check a named layer's conditions).
        let selected = app.session.active_layer.replace(drag.layer);
        let hex = drag.color.hex();
        let change = match drag.part {
            Part::Text => json!({"color": hex}),
            Part::Halo => json!({"haloColor": hex}),
            Part::Fill => json!({"background": {"fill": hex}}),
            Part::Outline => json!({"background": {"stroke": hex}}),
        };
        run(app, "labels.style", named(change, "class", drag.class.as_deref()));
        app.session.active_layer = selected;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use egui_kittest::kittest::{NodeT, Queryable};
    use rustgis_engine::map::Rgba;
    use serde_json::json;

    use crate::{RightPane, RustGisApp};

    fn layer_id(app: &RustGisApp, name: &str) -> u32 {
        app.session.map().layers.iter().find(|l| l.name == name).unwrap().id
    }

    fn with_cities() -> RustGisApp {
        // Keep the font picker ready before the harness checks its role or settles the layout.
        super::families::installed();
        let mut app = RustGisApp::with_sample();
        let id = layer_id(&app, "Cities");
        app.session.run("layer.select", &json!({"layer": id})).unwrap();
        app
    }

    fn harness(app: RustGisApp) -> egui_kittest::Harness<'static, RustGisApp> {
        // The pane's fonts (its semibold heading) are installed on the first frame, as the app does.
        let mut fonts = false;
        egui_kittest::Harness::builder().with_size(egui::vec2(360.0, 900.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                super::show(app, ui, &crate::theme::Tokens::light());
                super::finish_color_drag(app, ui);
            },
            app,
        )
    }

    /// Opens the pane's tab for `topic` (#858).
    fn open_tab(h: &mut egui_kittest::Harness<'static, RustGisApp>, topic: &str) {
        h.get_by_role_and_label(egui::accesskit::Role::Button, topic).click();
        h.run();
    }

    /// Inputs remain reachable by their purpose, independently of the selected value (#790), each
    /// in its topic's tab (#858).
    #[test]
    fn inputs_are_reachable_by_their_row_labels() {
        use egui::accesskit::Role;
        let mut h = harness(with_cities());
        h.run();
        for (tab, inputs) in [
            ("Content", &["Class", "Field"][..]),
            ("Symbol", &["Font", "Class"]),
            ("Range", &["Out Beyond", "In Beyond", "Closest", "Farthest"]),
        ] {
            open_tab(&mut h, tab);
            for label in inputs {
                assert!(h.query_by_role_and_label(Role::ComboBox, label).is_some(), "missing input: {label}");
            }
        }
        assert!(h.query_by_role_and_label(Role::ComboBox, "Font").is_none(), "one tab at a time");
        open_tab(&mut h, "Symbol");
        assert!(h.query_by_role_and_label(Role::SpinButton, "Size").is_some());
        open_tab(&mut h, "Content");
        h.get_by_role_and_label(Role::ComboBox, "Field").click();
        h.run();
        h.get_by_label("pop_max").click();
        h.run();
        assert_eq!(h.state().session.map().layer_by_name("Cities").unwrap().labels.default.field, "pop_max");
        assert!(h.query_by_label("+ New expression").is_some(), "the SQL query is beside the field");
    }

    #[test]
    fn the_halos_colour_and_width_and_the_letter_spacing_are_set_in_the_pane() {
        use egui::accesskit::Role;
        let mut h = harness(with_cities());
        h.run();
        open_tab(&mut h, "Symbol");
        assert!(h.query_by_role_and_label(Role::SpinButton, "Letter spacing").is_some());
        assert!(h.query_by_role_and_label(Role::SpinButton, "Halo width").is_none(), "thin: no width to set");
        let class = |h: &egui_kittest::Harness<'_, RustGisApp>| {
            let c = &h.state().session.map().layer_by_name("Cities").unwrap().labels.default;
            (c.halo_color, c.halo_width)
        };
        assert_eq!(class(&h), (None, None));
        // Off Automatic: the colour it had, now its own; off Thin: a width of its own.
        h.get_by_label("Automatic").click();
        h.run();
        h.get_by_label("Thin").click();
        h.run();
        assert_eq!(class(&h), (Some(Rgba::new(255, 255, 255, 225)), Some(1.0)));
        assert!(h.query_by_role_and_label(Role::SpinButton, "Halo width").is_some());
        h.get_by_label("Thin").click();
        h.run();
        assert_eq!(class(&h).1, None);
        // Without a halo, neither row.
        h.get_by_label("Halo").click();
        h.run();
        assert!(h.query_by_label("Automatic").is_none() && h.query_by_label("Thin").is_none());
    }

    /// Wrapping in the Symbol tab (the character typed and set once the field is left, a length,
    /// the lines' alignment), and in the Position tab a line's repeat, shown for a placement along
    /// lines only.
    #[test]
    fn wrapping_and_the_repeat_along_lines_are_set_in_the_pane() {
        use egui::accesskit::Role;
        use rustgis_engine::map::layout::TextAlign;
        super::families::installed();
        let mut app = RustGisApp::with_sample();
        let id = layer_id(&app, "Rivers");
        app.session.run("layer.select", &json!({"layer": id})).unwrap();
        let mut h = harness(app);
        h.run();
        let class = |h: &egui_kittest::Harness<'_, RustGisApp>| h.state().session.map().layer(id).unwrap().labels.default.clone();
        open_tab(&mut h, "Symbol");
        assert!(h.query_by_role_and_label(Role::SpinButton, "Wrap length").is_some());
        h.get_by_role_and_label(Role::TextInput, "Wrap at").focus();
        h.run();
        h.get_by_role_and_label(Role::TextInput, "Wrap at").type_text("/");
        h.run();
        assert_eq!(class(&h).wrap_char, None, "not while typing");
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(class(&h).wrap_char.as_deref(), Some("/"));
        h.get_by_label("Center").click();
        h.run();
        assert_eq!(class(&h).line_align, TextAlign::Center);
        // Repeat: only for a placement along lines.
        open_tab(&mut h, "Position");
        h.state_mut().session.run("labels.placement", &json!({"layer": id, "position": "above"})).unwrap();
        h.run();
        assert!(h.query_by_label("Repeat").is_none(), "not off the line");
        h.state_mut().session.run("labels.placement", &json!({"layer": id, "position": "parallel"})).unwrap();
        h.run();
        h.get_by_role_and_label(Role::CheckBox, "Repeat").click();
        h.run();
        assert_eq!(class(&h).repeat, Some(144.0));
        assert!(h.query_by_role_and_label(Role::SpinButton, "Repeat").is_some(), "its distance to set");
        h.get_by_role_and_label(Role::CheckBox, "Repeat").click();
        h.run();
        assert_eq!(class(&h).repeat, None);
    }

    /// A point layer's labels can try the other places round their points (#1011); a line or
    /// polygon layer's have no such box.
    #[test]
    fn other_positions_are_set_in_the_pane_for_points() {
        use egui::accesskit::Role;
        const BOX: &str = "Try other places round the point";
        let mut h = harness(with_cities());
        h.run();
        open_tab(&mut h, "Position");
        let on = |h: &egui_kittest::Harness<'_, RustGisApp>| {
            h.state().session.map().layer_by_name("Cities").unwrap().labels.default.other_positions
        };
        assert!(!on(&h));
        h.get_by_role_and_label(Role::CheckBox, BOX).click();
        h.run();
        assert!(on(&h));
        h.get_by_role_and_label(Role::CheckBox, BOX).click();
        h.run();
        assert!(!on(&h));
        let rivers = layer_id(h.state(), "Rivers");
        h.state_mut().session.run("layer.select", &json!({"layer": rivers})).unwrap();
        h.run();
        assert!(h.query_by_role_and_label(Role::CheckBox, BOX).is_none(), "not for lines");
        let countries = layer_id(h.state(), "Countries");
        h.state_mut().session.run("layer.select", &json!({"layer": countries})).unwrap();
        h.run();
        assert!(h.query_by_role_and_label(Role::CheckBox, BOX).is_none(), "not for polygons");
    }

    /// A line layer's labels can try other places along the line (#1187): the box is there for a
    /// placement along the line without a repeat, and hiding it leaves what it was set to.
    #[test]
    fn other_positions_are_set_in_the_pane_along_lines() {
        use egui::accesskit::Role;
        const BOX: &str = "Try other places along the line";
        let mut h = harness(with_cities());
        h.run();
        let rivers = layer_id(h.state(), "Rivers");
        h.state_mut().session.run("layer.select", &json!({"layer": rivers})).unwrap();
        h.run();
        open_tab(&mut h, "Position");
        let on = |h: &egui_kittest::Harness<'_, RustGisApp>| {
            h.state().session.map().layer_by_name("Rivers").unwrap().labels.default.other_positions
        };
        assert!(h.query_by_role_and_label(Role::CheckBox, BOX).is_none(), "not off the line");
        for position in ["parallel", "curved"] {
            h.state_mut().session.run("labels.placement", &json!({"layer": rivers, "position": position})).unwrap();
            h.run();
            assert!(h.query_by_role_and_label(Role::CheckBox, BOX).is_some(), "{position}");
        }
        h.get_by_role_and_label(Role::CheckBox, BOX).click();
        h.run();
        assert!(on(&h));
        h.state_mut().session.run("labels.placement", &json!({"layer": rivers, "repeat": 144})).unwrap();
        h.run();
        assert!(h.query_by_role_and_label(Role::CheckBox, BOX).is_none(), "not with a repeat");
        assert!(on(&h), "hidden, it's kept");
        h.state_mut().session.run("labels.placement", &json!({"layer": rivers, "repeat": null})).unwrap();
        h.run();
        h.get_by_role_and_label(Role::CheckBox, BOX).click();
        h.run();
        assert!(!on(&h));
        let countries = layer_id(h.state(), "Countries");
        h.state_mut().session.run("layer.select", &json!({"layer": countries})).unwrap();
        h.run();
        assert!(h.query_by_role_and_label(Role::CheckBox, BOX).is_none(), "not for polygons");
    }

    /// The box along the line sets the class chosen, not Default (#1187).
    #[test]
    fn other_positions_along_lines_set_the_chosen_class() {
        let mut app = with_cities();
        let rivers = layer_id(&app, "Rivers");
        app.session.run("layer.select", &json!({"layer": rivers})).unwrap();
        app.run("labels.class.add", json!({"name": "Named"})).unwrap();
        app.run("labels.placement", json!({"class": "Named", "position": "curved"})).unwrap();
        let mut h = harness(app);
        h.run();
        pick_class(&mut h, "Default", "Named");
        open_tab(&mut h, "Position");
        h.get_by_role_and_label(egui::accesskit::Role::CheckBox, "Try other places along the line").click();
        h.run();
        let l = labels(h.state());
        assert!(l.class("Named").unwrap().other_positions);
        assert!(!l.default.other_positions);
    }

    #[test]
    fn the_background_and_the_offset_are_set_in_the_pane() {
        use egui::accesskit::Role;
        let mut h = harness(with_cities());
        h.run();
        open_tab(&mut h, "Symbol");
        let background = |h: &egui_kittest::Harness<'_, RustGisApp>| {
            h.state().session.map().layer_by_name("Cities").unwrap().labels.default.background.clone()
        };
        assert_eq!(background(&h), None);
        assert!(h.query_by_role_and_label(Role::SpinButton, "Padding across").is_none(), "no background: no rows");
        h.get_by_label("Background").click();
        h.run();
        assert_eq!(background(&h), Some(rustgis_engine::map::LabelBackground::default()));
        for row in ["Outline width", "Padding across", "Padding up and down", "Corner radius"] {
            assert!(h.query_by_role_and_label(Role::SpinButton, row).is_some(), "{row}");
        }
        // A colour picked for its fill is one labels.style, the pre-pick colour undone to.
        let app = h.state_mut();
        let layer = app.session.map().layer_by_name("Cities").unwrap().id;
        let pick = super::ColorDrag {
            layer,
            class: None,
            part: super::Part::Fill,
            before: Some(Rgba::rgb(255, 255, 255)),
            color: Rgba::rgb(1, 2, 3),
        };
        super::apply_color_drag(app, pick);
        assert_eq!(background(&h).and_then(|b| b.fill), Some(Rgba::rgb(1, 2, 3)));
        h.state_mut().session.run("edit.undo", &json!({})).unwrap();
        assert_eq!(background(&h).and_then(|b| b.fill), Some(Rgba::rgb(255, 255, 255)));
        h.run();
        h.get_by_label("Background").click();
        h.run();
        assert_eq!(background(&h), None);
        assert!(h.query_by_role_and_label(Role::SpinButton, "Corner radius").is_none());
        open_tab(&mut h, "Position");
        assert!(h.query_by_role_and_label(Role::SpinButton, "Offset right").is_some());
        assert!(h.query_by_role_and_label(Role::SpinButton, "Offset down").is_some());
    }

    #[test]
    fn the_pane_command_needs_a_feature_layer() {
        let mut app = RustGisApp::with_sample();
        app.session.active_layer = None;
        assert!(app.run("ui.pane.labeling", json!(null)).is_err(), "no layer selected");
        let raster = app.session.map().layers.iter().find(|l| l.is_raster()).map(|l| l.id);
        let cities = layer_id(&app, "Cities");
        if let Some(raster) = raster {
            app.session.run("layer.select", &json!({"layer": raster})).unwrap();
            assert!(app.run("ui.pane.labeling", json!(null)).is_err(), "a raster has no labels");
            // Refused for a raster given, the selection stays as it was.
            app.session.run("layer.select", &json!({"layer": cities})).unwrap();
            assert!(app.run("ui.pane.labeling", json!({"layer": raster})).is_err());
            assert_eq!(app.session.active_layer, Some(cities), "a refusal doesn't select the raster");
        }
        app.session.run("layer.select", &json!({"layer": cities})).unwrap();
        assert!(app.run("ui.pane.labeling", json!({"layer": "No Such Layer"})).is_err());
        assert_eq!(app.session.active_layer, Some(cities), "nor an unknown layer");
        app.session.active_layer = None;
        app.run("ui.pane.labeling", json!({"layer": cities})).unwrap();
        assert_eq!(app.session.active_layer, Some(cities), "the layer given is selected");
        assert_eq!(app.ui.right, Some(RightPane::Labeling));
        app.run("ui.pane.labeling", json!({"close": true})).unwrap();
        assert!(!app.ui.right_open.contains(&RightPane::Labeling));
    }

    #[test]
    fn the_pane_runs_the_label_commands_on_the_selected_layer() {
        let app = with_cities();
        let before = app.session.map().layer(app.session.active_layer.unwrap()).unwrap().labels.clone();
        let mut h = harness(app);
        h.run();
        h.get_by_label("Label features in this layer").click();
        h.run();
        open_tab(&mut h, "Position");
        h.get_by_label("Below Right").click();
        h.run();
        open_tab(&mut h, "Symbol");
        h.get_by_label("Bold").click();
        h.run();
        let app = h.state();
        let l = app.session.map().layer(app.session.active_layer.unwrap()).unwrap();
        assert_ne!(l.labels.enabled, before.enabled, "the checkbox toggles labels");
        assert_eq!(l.labels.default.placement, rustgis_engine::map::LabelPlacement::BelowRight);
        assert_ne!(l.labels.default.bold, before.default.bold, "bold toggles");
        // Each change is its own command, so each undoes on its own.
        let app = h.state_mut();
        app.run("edit.undo", json!(null)).unwrap();
        let l = app.session.map().layer(app.session.active_layer.unwrap()).unwrap();
        assert_eq!(l.labels.default.bold, before.default.bold, "undo takes back the last change only");
        assert_eq!(l.labels.default.placement, rustgis_engine::map::LabelPlacement::BelowRight);
    }

    /// Record Task names the layer in a step made from the pane, as it does for the ribbon, so the
    /// step replays on that layer by name (#617 review).
    #[test]
    fn a_step_recorded_from_the_pane_names_the_layer() {
        let mut app = with_cities();
        app.ws.recording = Some(Vec::new());
        let mut h = harness(app);
        h.run();
        open_tab(&mut h, "Position");
        h.get_by_label("Below Right").click();
        h.run();
        open_tab(&mut h, "Symbol");
        h.get_by_label("Bold").click();
        h.run();
        let steps = h.state().ws.recording.clone().unwrap();
        let recorded: Vec<(&str, Option<&serde_json::Value>)> = steps.iter().map(|s| (s.command.as_str(), s.params.get("layer"))).collect();
        assert_eq!(
            recorded,
            [("labels.placement", Some(&json!("Cities"))), ("labels.style", Some(&json!("Cities")))],
            "each step names the layer, not its id"
        );
    }

    /// A pick left pending on one layer when the selection moves to another mid-pick (the
    /// Assistant, a shortcut) is finished as its own step, not left as an unrecorded preview.
    #[test]
    fn a_pick_pending_on_another_layer_is_finished_first() {
        let mut app = with_cities();
        app.ws.recording = Some(Vec::new());
        let cities = app.session.active_layer.unwrap();
        let countries = layer_id(&app, "Countries");
        let color = |app: &RustGisApp, id| app.session.map().layer(id).unwrap().labels.default.color;
        let (cities_before, countries_before) = (color(&app, cities), color(&app, countries));
        let steps = app.session.undo_depth();
        let mut h = egui_kittest::Harness::builder().build_ui_state(
            |ui, (app, pick): &mut (RustGisApp, Option<Rgba>)| {
                if let Some(c) = pick.take() {
                    let l = app.session.map().layer(app.session.active_layer.unwrap()).unwrap().clone();
                    super::color_drag(app, ui, &l, None, c);
                }
                super::finish_color_drag(app, ui);
            },
            (app, None),
        );
        let button = |pressed| egui::Event::PointerButton {
            pos: egui::pos2(4.0, 4.0),
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let (red, blue) = (Rgba::new(200, 10, 10, 255), Rgba::new(10, 10, 200, 255));
        h.event(button(true));
        h.step();
        h.state_mut().1 = Some(red);
        h.step();
        // The pointer still down, the selection moves and the picker reports a colour for it.
        h.state_mut().0.session.run("layer.select", &json!({"layer": countries})).unwrap();
        h.state_mut().1 = Some(blue);
        h.step();
        let app = &h.state().0;
        assert_eq!(color(app, cities), red, "the first pick is kept on its layer");
        assert_eq!(app.session.undo_depth(), steps + 1, "and run as a step of its own");
        assert_eq!(color(app, countries), blue, "the new pick previews");
        assert_eq!(app.session.active_layer, Some(countries), "the selection stays where it moved");
        h.event(button(false));
        h.step();
        let app = &mut h.state_mut().0;
        assert_eq!(app.session.undo_depth(), steps + 2);
        // Both steps name their layer, the first one too, though it was no longer selected.
        let steps_recorded: Vec<_> =
            app.ws.recording.iter().flatten().map(|s| (s.command.clone(), s.params.get("layer").cloned())).collect();
        assert_eq!(
            steps_recorded,
            [("labels.style".to_string(), Some(json!("Cities"))), ("labels.style".to_string(), Some(json!("Countries")))]
        );
        app.run("edit.undo", json!(null)).unwrap();
        assert_eq!(color(app, countries), countries_before);
        app.run("edit.undo", json!(null)).unwrap();
        assert_eq!(color(app, cities), cities_before, "each pick undoes on its own");
    }

    #[test]
    fn a_colour_pick_is_one_undo_step() {
        let app = with_cities();
        let id = app.session.active_layer.unwrap();
        let before = app.session.map().layer(id).unwrap().labels.default.color;
        let steps = app.session.undo_depth();
        // The picker's colour each frame, if it reports one, as `text_symbol` hands it on.
        let mut h = egui_kittest::Harness::builder().build_ui_state(
            |ui, (app, pick): &mut (RustGisApp, Option<Rgba>)| {
                if let Some(c) = pick.take() {
                    let l = app.session.map().layer(app.session.active_layer.unwrap()).unwrap().clone();
                    super::color_drag(app, ui, &l, None, c);
                }
                super::finish_color_drag(app, ui);
            },
            (app, None),
        );
        let button = |pressed| egui::Event::PointerButton {
            pos: egui::pos2(4.0, 4.0),
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        h.event(button(true));
        h.step();
        let mut picked = before;
        for c in [[200u8, 10, 10], [10, 200, 10], [10, 10, 200]] {
            picked = Rgba::new(c[0], c[1], c[2], 255);
            h.state_mut().1 = Some(picked);
            h.step();
            let app = &h.state().0;
            assert_eq!(app.session.map().layer(id).unwrap().labels.default.color, picked, "the map previews the pick");
            assert_eq!(app.session.undo_depth(), steps, "no command while the pointer is down");
        }
        h.event(button(false));
        h.step();
        let app = &mut h.state_mut().0;
        assert_eq!(app.session.map().layer(id).unwrap().labels.default.color, picked);
        assert_eq!(app.session.undo_depth(), steps + 1, "letting go runs labels.style once");
        app.run("edit.undo", json!(null)).unwrap();
        assert_eq!(app.session.map().layer(id).unwrap().labels.default.color, before, "one undo takes the whole pick back");
    }

    /// The Content tab's By Expression boxes set the class's text and size per feature in one
    /// step each; one that doesn't read says why and isn't applied.
    #[test]
    fn label_expressions_are_set_in_the_content_tab() {
        use egui::accesskit::Role;
        let app = with_cities();
        let steps = app.session.undo_depth();
        let mut h = harness(app);
        h.run();
        let by = |h: &egui_kittest::Harness<'_, RustGisApp>| {
            h.state().session.map().layer(h.state().session.active_layer.unwrap()).unwrap().labels.default.by.clone()
        };
        h.get_by_label("By Expression").click();
        h.run();
        let enter = |h: &mut egui_kittest::Harness<'static, RustGisApp>, label: &str, text: &str| {
            h.get_by_role_and_label(Role::TextInput, label).focus();
            h.run();
            h.get_by_role_and_label(Role::TextInput, label).type_text(text);
            h.run();
            h.key_press(egui::Key::Enter);
            h.run();
        };
        enter(&mut h, "Text", "nosuch || 'x'");
        assert!(h.query_by_label_contains("Invalid:").is_some(), "says why");
        assert_eq!((by(&h).text, h.state().session.undo_depth()), (None, steps), "not applied");
        h.get_by_role_and_label(Role::TextInput, "Text").focus();
        h.run();
        h.key_press(egui::Key::Escape);
        h.run();
        enter(&mut h, "Text", "upper(name)");
        assert_eq!(by(&h).text.as_deref(), Some("upper(name)"));
        enter(&mut h, "Size", "scale_linear(pop_max, 0, 2e7, 8, 20)");
        assert_eq!(by(&h).size.as_deref(), Some("scale_linear(pop_max, 0, 2e7, 8, 20)"));
        assert_eq!(h.state().session.undo_depth(), steps + 2, "a step each");
        assert!(h.query_by_role_and_label(Role::TextInput, "Angle").is_some(), "points take an angle");
    }

    fn clause(app: &RustGisApp) -> Option<String> {
        app.session.map().layer(app.session.active_layer.unwrap()).unwrap().labels.default.where_.clone()
    }

    fn type_clause(h: &mut egui_kittest::Harness<'static, RustGisApp>, text: &str) {
        h.get_by_role(egui::accesskit::Role::MultilineTextInput).focus();
        h.run();
        h.get_by_role(egui::accesskit::Role::MultilineTextInput).type_text(text);
        h.run();
    }

    /// The SQL tab writes a draft: typing applies nothing, Check counts, Apply sets the clause in
    /// one undo step, and Clear takes it away in another (#618).
    #[test]
    fn the_sql_tab_applies_a_clause_in_one_step() {
        let app = with_cities();
        let steps = app.session.undo_depth();
        let mut h = harness(app);
        h.run();
        h.get_by_label("There is no expression defined.");
        h.get_by_label("+ New expression").click();
        h.run();
        type_clause(&mut h, "pop_max > 5000000");
        assert_eq!((clause(h.state()), h.state().session.undo_depth()), (None, steps), "typing applies nothing");
        h.get_by_label("Not applied yet.");
        h.get_by_label("Check").click();
        h.run();
        let l = h.state().session.map().layer(h.state().session.active_layer.unwrap()).unwrap().clone();
        let n = rustgis_engine::data::query::select("pop_max > 5000000", &l.dataset).unwrap().len();
        let all = l.drawn_rows(h.state().session.map().scale()).iter().filter(|r| **r).count();
        h.get_by_label(&format!("Valid: {n} of {all} features match"));
        h.get_by_label("Apply").click();
        h.run();
        assert_eq!(clause(h.state()).as_deref(), Some("pop_max > 5000000"));
        assert_eq!(h.state().session.undo_depth(), steps + 1, "one step");
        assert!(h.query_by_label("Not applied yet.").is_none());
        h.get_by_label("Clear").click();
        h.run();
        assert_eq!((clause(h.state()), h.state().session.undo_depth()), (None, steps + 2));
        h.get_by_label("There is no expression defined.");
        let app = h.state_mut();
        app.run("edit.undo", json!(null)).unwrap();
        assert_eq!(clause(app).as_deref(), Some("pop_max > 5000000"), "undo brings the clause back");
    }

    /// A clause that doesn't read says why, and Apply leaves the layer as it was.
    #[test]
    fn a_clause_that_doesnt_read_is_not_applied() {
        let app = with_cities();
        let steps = app.session.undo_depth();
        let mut h = harness(app);
        h.run();
        h.get_by_label("+ New expression").click();
        h.run();
        type_clause(&mut h, "pop_max >");
        assert!(h.query_by_label_contains("Invalid:").is_some());
        h.get_by_label("Apply").click();
        h.run();
        assert_eq!((clause(h.state()), h.state().session.undo_depth()), (None, steps), "Apply is off");
    }

    /// A draft follows the clause when it changes underneath (undo, a script), until it is edited;
    /// another layer has its own clause, and the draft is dropped.
    #[test]
    fn the_draft_follows_the_layer_and_its_clause() {
        let app = with_cities();
        let mut h = harness(app);
        h.run();
        h.state_mut().run("labels.class.update", json!({"where": "pop_max > 1"})).unwrap();
        h.run();
        assert_eq!(h.get_by_role(egui::accesskit::Role::MultilineTextInput).value().as_deref(), Some("pop_max > 1"));
        type_clause(&mut h, " AND pop_max < 9");
        h.state_mut().run("labels.class.update", json!({"where": "pop_max > 2"})).unwrap();
        h.run();
        let text = h.get_by_role(egui::accesskit::Role::MultilineTextInput).value().unwrap_or_default();
        assert!(text.contains("AND pop_max < 9") && text != "pop_max > 2", "an edited draft is kept: {text}");
        h.get_by_label("Not applied yet.");
        let countries = layer_id(h.state(), "Countries");
        h.state_mut().session.run("layer.select", &json!({"layer": countries})).unwrap();
        h.run();
        h.get_by_label("There is no expression defined.");
    }

    /// The SQL Query section of the Content tab has one note, on what decides the labels first:
    /// labels off, then a hidden Default (Show labels shows it again), then other visible classes.
    #[test]
    fn the_sql_tab_says_what_else_decides_the_labels() {
        let mut app = with_cities();
        app.run("labels.class.add", json!({"name": "Big", "where": "pop_max > 5000000"})).unwrap();
        app.run("labels.class.update", json!({"visible": false})).unwrap();
        let mut h = harness(app);
        h.run();
        let rule = "Each class labels the features its own clause matches";
        assert!(h.query_by_label_contains("Labels are off for this layer").is_some());
        assert!(h.query_by_label("Show labels").is_none(), "no hidden-class note while labels are off");
        assert!(h.query_by_label_contains(rule).is_none(), "no rule note while labels are off");
        h.state_mut().run("labels.enable", json!({"on": true})).unwrap();
        h.run();
        assert!(h.query_by_label_contains(rule).is_none(), "no rule note while Default is hidden");
        h.get_by_label("Show labels").click();
        h.run();
        let l = &h.state().session.map().layer(h.state().session.active_layer.unwrap()).unwrap().labels;
        assert!(l.default.visible);
        assert!(h.query_by_label_contains(rule).is_some());
        // A hidden extra class labels nothing, so only Default labels: no rule either.
        h.state_mut().run("labels.class.update", json!({"class": "Big", "visible": false})).unwrap();
        h.run();
        assert!(h.query_by_label_contains(rule).is_none(), "no rule note with one visible class");
    }

    /// A draft belongs to its layer in its project: another project, though it reuses the
    /// layer's id, starts without it.
    #[test]
    fn a_draft_is_dropped_with_its_project() {
        let app = with_cities();
        let mut h = harness(app);
        h.run();
        h.get_by_label("+ New expression").click();
        h.run();
        type_clause(&mut h, "pop_max > 5000000");
        h.get_by_label("Not applied yet.");
        let app = h.state_mut();
        app.run("project.sample", json!(null)).unwrap();
        let cities = layer_id(app, "Cities");
        app.session.run("layer.select", &json!({"layer": cities})).unwrap();
        h.run();
        h.get_by_label("There is no expression defined.");
    }

    /// A Check's count shows while nothing has changed since: after a change, it is asked again.
    #[test]
    fn a_check_is_dropped_after_a_change() {
        let app = with_cities();
        let mut h = harness(app);
        h.run();
        h.get_by_label("+ New expression").click();
        h.run();
        type_clause(&mut h, "pop_max > 5000000");
        h.get_by_label("Check").click();
        h.run();
        assert!(h.query_by_label_contains("features match").is_some());
        h.state_mut().run("labels.enable", json!({"on": true})).unwrap();
        h.run();
        assert!(h.query_by_label_contains("features match").is_none(), "the count may be stale");
        // A zoom changes no revision, but the scale: display filters may draw other features.
        h.get_by_label("Check").click();
        h.run();
        assert!(h.query_by_label_contains("features match").is_some());
        h.state_mut().session.map_mut().camera.res *= 2.0;
        h.run();
        assert!(h.query_by_label_contains("features match").is_none(), "the count was for another scale");
    }

    /// A clause whose field was deleted since says so: it no longer reads, and labels nothing.
    #[test]
    fn a_clause_that_no_longer_reads_says_so() {
        let mut app = with_cities();
        app.run("labels.class.update", json!({"where": "pop_max > 1"})).unwrap();
        app.run("edit.deleteField", json!({"field": "pop_max"})).unwrap();
        let mut h = harness(app);
        h.run();
        assert!(h.query_by_label_contains("This clause no longer reads").is_some());
    }

    /// A clause applied from the pane is recorded on the layer by name, for Default (no class).
    #[test]
    fn a_clause_recorded_from_the_pane_names_the_layer() {
        let mut app = with_cities();
        app.ws.recording = Some(Vec::new());
        let mut h = harness(app);
        h.run();
        h.get_by_label("+ New expression").click();
        h.run();
        type_clause(&mut h, "pop_max > 5000000");
        h.get_by_label("Apply").click();
        h.run();
        let steps = h.state().ws.recording.clone().unwrap();
        let last = steps.last().unwrap();
        assert_eq!(last.command, "labels.class.update");
        assert_eq!(last.params.get("layer"), Some(&json!("Cities")));
        assert_eq!(last.params.get("class"), None);
    }

    fn labels(app: &RustGisApp) -> rustgis_engine::map::symbology::Labeling {
        app.session.map().layer(app.session.active_layer.unwrap()).unwrap().labels.clone()
    }

    /// The layer's class names, in the order they are listed.
    fn names(app: &RustGisApp) -> Vec<String> {
        labels(app).in_order().map(|c| c.name.clone()).collect()
    }

    /// The class the pane's chooser shows.
    fn shown(h: &egui_kittest::Harness<'static, RustGisApp>, name: &str) -> bool {
        h.query_all_by_role(egui::accesskit::Role::ComboBox).any(|n| n.value().as_deref() == Some(name))
    }

    /// Chooses class `to` in the pane's chooser, which shows `from`.
    fn pick_class(h: &mut egui_kittest::Harness<'static, RustGisApp>, from: &str, to: &str) {
        h.query_all_by_role(egui::accesskit::Role::ComboBox).find(|n| n.value().as_deref() == Some(from)).unwrap().click();
        h.run();
        h.get_by_label(to).click();
        h.run();
    }

    /// The field box searches the layer's fields, says when none matches, and names a field the
    /// layer doesn't have rather than showing none (#858).
    #[test]
    fn the_field_box_searches_and_names_a_missing_field() {
        use egui::accesskit::Role;
        let mut app = with_cities();
        // A style or a service can leave a class on a field the layer lacks.
        let id = layer_id(&app, "Cities");
        app.session.map_mut().layer_mut(id).unwrap().labels.default.field = "gone".into();
        let mut h = harness(app);
        h.run();
        assert!(shown(&h, "gone (not in the layer)"));
        let search = |h: &mut egui_kittest::Harness<'static, RustGisApp>, text: &str| {
            h.get_by_role_and_label(Role::ComboBox, "Field").click();
            h.run();
            h.get_by_role(Role::TextInput).focus();
            h.run();
            h.get_by_role(Role::TextInput).type_text(text);
            h.run();
        };
        search(&mut h, "pop");
        assert!(h.query_by_label("adm0name").is_none(), "the search narrows the list");
        h.get_by_label("pop_max").click();
        h.run();
        assert_eq!(labels(h.state()).default.field, "pop_max");
        assert!(shown(&h, "pop_max"));
        // Picking the chosen field again ends a search too; the pick before had ended the first.
        search(&mut h, "pop");
        assert_eq!(h.get_by_role(Role::TextInput).value().as_deref(), Some("pop"), "the pick ended the search");
        h.get_by_role_and_label(Role::Button, "pop_max").click();
        h.run();
        // A pick ends the search: the box reopens on the whole list.
        h.get_by_role_and_label(Role::ComboBox, "Field").click();
        h.run();
        assert!(h.query_by_label("adm0name").is_some(), "the list opens whole after a pick");
        h.get_by_role_and_label(Role::ComboBox, "Field").click();
        h.run();
        search(&mut h, "zzz");
        assert_eq!(h.get_by_role(Role::TextInput).value().as_deref(), Some("zzz"), "the pick ended the search");
        assert!(h.query_by_label("No fields match").is_some());
    }

    /// A layer without fields has no label field to offer: the box shows none, and its list says
    /// why rather than that nothing matches.
    #[test]
    fn a_layer_without_fields_has_no_label_field() {
        use egui::accesskit::Role;
        use rustgis_engine::data::{Dataset, GeometryType};
        let mut app = RustGisApp::with_sample();
        let id = app.session.map_mut().add_dataset(Dataset::new("Bare", GeometryType::Point, vec![]));
        app.session.run("layer.select", &json!({"layer": id})).unwrap();
        let mut h = harness(app);
        h.run();
        assert!(shown(&h, "<None>"));
        h.get_by_role_and_label(Role::ComboBox, "Field").click();
        h.run();
        assert!(h.query_by_label("This layer has no fields.").is_some());
        assert!(h.query_by_label("No fields match").is_none());
    }

    /// The pane edits the class chosen, and only it; its steps name it, and Default's name none
    /// (#722).
    #[test]
    fn the_chosen_class_is_the_one_edited() {
        let mut app = with_cities();
        app.run("labels.class.add", json!({"name": "Big", "where": "pop_max > 5000000"})).unwrap();
        app.ws.recording = Some(Vec::new());
        let before = labels(&app).default;
        let mut h = harness(app);
        h.run();
        pick_class(&mut h, "Default", "Big");
        open_tab(&mut h, "Position");
        h.get_by_label("Below Right").click();
        h.run();
        open_tab(&mut h, "Symbol");
        h.get_by_label("Bold").click();
        h.run();
        let l = labels(h.state());
        let big = l.class("Big").unwrap();
        assert_eq!((big.placement, big.bold), (rustgis_engine::map::LabelPlacement::BelowRight, !before.bold));
        assert_eq!((l.default.placement, l.default.bold), (before.placement, before.bold), "Default is left as it was");
        pick_class(&mut h, "Big", "Default");
        h.get_by_label("Halo").click();
        h.run();
        assert_ne!(labels(h.state()).default.halo, before.halo);
        let steps = h.state().ws.recording.clone().unwrap();
        let classes: Vec<_> = steps.iter().map(|s| (s.command.as_str(), s.params.get("class").cloned())).collect();
        assert_eq!(
            classes,
            [("labels.placement", Some(json!("Big"))), ("labels.style", Some(json!("Big"))), ("labels.style", None)],
            "Default's step names no class"
        );
    }

    /// Font, italic and line placement edits affect the chosen class and leave Default unchanged.
    #[test]
    fn font_italic_and_line_placement_edit_the_chosen_class() {
        let mut app = with_cities();
        let rivers = layer_id(&app, "Rivers");
        app.session.run("layer.select", &json!({"layer": rivers})).unwrap();
        app.run("labels.class.add", json!({"name": "Named"})).unwrap();
        app.run("labels.style", json!({"class": "Named", "font": "Missing test font"})).unwrap();
        let before = labels(&app).default;
        let steps = app.session.undo_depth();
        app.ws.recording = Some(Vec::new());
        let mut h = harness(app);
        h.run();
        pick_class(&mut h, "Default", "Named");
        open_tab(&mut h, "Symbol");
        h.get_by_label("Italic").click();
        h.run();
        open_tab(&mut h, "Position");
        h.get_by_label("Curved").click();
        h.run();
        open_tab(&mut h, "Symbol");
        h.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Font").click();
        h.run();
        h.get_by_label("Label font").click();
        h.run();
        let l = labels(h.state());
        let named = l.class("Named").unwrap();
        assert_eq!(named.italic, !before.italic);
        assert_eq!(named.placement, rustgis_engine::map::LabelPlacement::Curved);
        assert_eq!(named.font, None);
        assert_eq!((l.default.italic, l.default.placement, l.default.font), (before.italic, before.placement, before.font));
        assert_eq!(h.state().session.undo_depth(), steps + 3);
        for step in h.state().ws.recording.as_ref().unwrap() {
            assert_eq!(step.params.get("class"), Some(&json!("Named")));
        }
        h.state_mut().run("edit.undo", json!(null)).unwrap();
        assert_eq!(labels(h.state()).class("Named").unwrap().font.as_deref(), Some("Missing test font"));
    }

    /// Clicking in a list's search box leaves the list open to type in (it closed on any click,
    /// the box too, so nothing could be searched); a pick closes it and ends the search.
    #[test]
    fn a_list_stays_open_to_search_it_and_closes_on_a_pick() {
        use egui::accesskit::Role;
        let mut h = harness(with_cities());
        h.run();
        let search_for = |h: &mut egui_kittest::Harness<'static, RustGisApp>, tab: &str, list: &str, text: &str| {
            open_tab(h, tab);
            let before = h.query_all_by_role(Role::TextInput).count();
            h.get_by_role_and_label(Role::ComboBox, list).click();
            h.run();
            // (The list's search box is the input with no label: the tab's own have theirs, and can
            // come after it, as Wrap at does on Symbol.)
            let search_box = |h: &egui_kittest::Harness<'static, RustGisApp>| {
                h.query_all_by_role(Role::TextInput).find(|n| n.accesskit_node().label().is_none()).map(|n| n.accesskit_node().id())
            };
            let id = search_box(h).unwrap();
            h.get_by(|n| n.id() == id).click();
            h.run();
            assert_eq!(h.query_all_by_role(Role::TextInput).count(), before + 1, "{list}: clicking its search box left it open");
            let id = search_box(h).unwrap();
            h.get_by(|n| n.id() == id).type_text(text);
            h.run();
            before
        };
        // The label's field.
        let before = search_for(&mut h, "Content", "Field", "nam");
        h.get_by_role_and_label(Role::Button, "name").click();
        h.run();
        assert_eq!(h.query_all_by_role(Role::TextInput).count(), before, "a pick closes it");
        // The label's font: found by part of its name, and used.
        let family = super::families::installed().iter().find(|f| f.listed).map(|f| f.name.clone());
        let Some(family) = family else { return };
        let part: String = family.chars().take(4).collect();
        let before = search_for(&mut h, "Symbol", "Font", &part);
        h.get_by_role_and_label(Role::Button, family.as_str()).click();
        h.run();
        assert_eq!(labels(h.state()).default.font.as_deref(), Some(family.as_str()));
        assert_eq!(h.query_all_by_role(Role::TextInput).count(), before, "a pick closes it");
    }

    /// A class chosen that goes away (an undo, a script, the Assistant) leaves Default chosen.
    #[test]
    fn the_pane_goes_back_to_default_when_its_class_goes_away() {
        let mut app = with_cities();
        app.run("labels.class.add", json!({"name": "Big"})).unwrap();
        let before = labels(&app).default.bold;
        let mut h = harness(app);
        h.run();
        pick_class(&mut h, "Default", "Big");
        h.state_mut().run("labels.class.remove", json!({"class": "Big"})).unwrap();
        h.run();
        assert!(shown(&h, "Default"));
        open_tab(&mut h, "Symbol");
        h.get_by_label("Bold").click();
        h.run();
        assert_ne!(labels(h.state()).default.bold, before, "the pane edits Default again");
    }

    /// The class chosen belongs to its map: a copy of the map has the same layer ids, and starts on
    /// Default.
    #[test]
    fn the_chosen_class_stays_with_its_map() {
        let mut app = with_cities();
        app.run("labels.class.add", json!({"name": "Big"})).unwrap();
        let map = app.session.map().name.clone();
        let mut h = harness(app);
        h.run();
        pick_class(&mut h, "Default", "Big");
        let copy = h.state_mut().run("map.duplicate", json!({})).unwrap()["name"].as_str().unwrap().to_string();
        h.run();
        assert_eq!(h.state().session.map().name, copy, "the copy is in view");
        assert!(shown(&h, "Default"));
        h.state_mut().run("map.activate", json!({"name": map})).unwrap();
        let id = layer_id(h.state(), "Cities");
        h.state_mut().run("layer.select", json!({"layer": id})).unwrap();
        h.run();
        assert!(shown(&h, "Big"), "back on its map");
        // Deleted, its copy moves into its place, and still starts on Default.
        let at = h.state().session.project.active_map;
        h.state_mut().run("map.remove", json!({"name": map})).unwrap();
        h.state_mut().run("map.activate", json!({"name": copy})).unwrap();
        assert_eq!(h.state().session.project.active_map, at, "the copy is where its map was");
        let id = layer_id(h.state(), "Cities");
        h.state_mut().run("layer.select", json!({"layer": id})).unwrap();
        h.run();
        assert!(shown(&h, "Default"));
    }

    /// New, Move Up, Rename and Remove, under the header's chooser (#858), each run their command
    /// once: one undo step each.
    #[test]
    fn classes_are_made_renamed_ordered_and_removed_from_the_pane() {
        let app = with_cities();
        let steps = app.session.undo_depth();
        let mut h = harness(app);
        h.run();
        assert!(h.get_by_label("Remove").accesskit_node().is_disabled(), "Default stays");
        h.get_by_label("New").click();
        h.run();
        assert_eq!(names(h.state()), ["Class 1", "Default"]);
        assert!(shown(&h, "Class 1"), "the new class is chosen");
        h.get_by_label("New").click();
        h.run();
        assert_eq!(names(h.state()), ["Class 1", "Class 2", "Default"]);
        assert!(h.get_by_label("Move Down").accesskit_node().is_disabled(), "the last before Default");
        h.get_by_label("Move Up").click();
        h.run();
        assert_eq!(names(h.state()), ["Class 2", "Class 1", "Default"]);
        h.get_by_label("Rename").click();
        h.run();
        h.get_by_role(egui::accesskit::Role::TextInput).type_text(" big");
        h.run();
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(names(h.state()), ["Class 2 big", "Class 1", "Default"]);
        assert!(shown(&h, "Class 2 big"), "still chosen by its new name");
        h.get_by_label("Remove").click();
        h.run();
        assert_eq!(names(h.state()), ["Class 1", "Default"]);
        assert!(shown(&h, "Default"));
        assert_eq!(h.state().session.undo_depth(), steps + 5);
        // Undone, the class is back, but the pane doesn't choose it again by its name.
        h.state_mut().run("edit.undo", json!(null)).unwrap();
        h.run();
        assert_eq!(names(h.state()), ["Class 2 big", "Class 1", "Default"]);
        assert!(shown(&h, "Default"));
    }

    /// A name refused keeps the box open, saying why; Escape leaves the name as it was.
    #[test]
    fn a_refused_class_name_keeps_the_box_open() {
        let mut app = with_cities();
        app.run("labels.class.add", json!({"name": "Big"})).unwrap();
        app.run("labels.class.add", json!({"name": "Big 2"})).unwrap();
        let mut h = harness(app);
        h.run();
        pick_class(&mut h, "Default", "Big");
        h.get_by_label("Rename").click();
        h.run();
        h.get_by_role(egui::accesskit::Role::TextInput).type_text(" 2");
        h.run();
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(names(h.state()), ["Big", "Big 2", "Default"]);
        assert!(h.state().session.notices.iter().any(|n| n.error && n.text.contains("already a label class")));
        assert!(h.query_by_role(egui::accesskit::Role::TextInput).is_some(), "the box stays open");
        for off in ["New", "Rename", "Move Up", "Move Down", "Remove", "Visible"] {
            assert!(h.get_by_label(off).accesskit_node().is_disabled(), "{off} waits for the rename");
        }
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(h.query_by_role(egui::accesskit::Role::TextInput).is_none());
        assert_eq!(names(h.state()), ["Big", "Big 2", "Default"]);
        assert!(shown(&h, "Big"));
    }

    /// A name that changes only in case is a rename, not another class's name.
    #[test]
    fn a_class_is_renamed_to_another_case() {
        let mut app = with_cities();
        app.run("labels.class.add", json!({"name": "Big"})).unwrap();
        let steps = app.session.undo_depth();
        let mut h = harness(app);
        h.run();
        pick_class(&mut h, "Default", "Big");
        h.get_by_label("Rename").click();
        h.run();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        h.key_press(egui::Key::Backspace);
        h.run();
        h.get_by_role(egui::accesskit::Role::TextInput).type_text("big");
        h.run();
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(names(h.state()), ["big", "Default"]);
        assert!(shown(&h, "big"), "still chosen by its new name");
        assert_eq!(h.state().session.undo_depth(), steps + 1);
    }

    /// A rename left open when the pane is hidden is gone when it shows again.
    #[test]
    fn a_rename_ends_with_the_pane() {
        let mut app = with_cities();
        app.ui.right = Some(RightPane::Labeling);
        let mut fonts = false;
        // Drawn only while it is the right pane in view, as the app does.
        let mut h = egui_kittest::Harness::builder().with_accessibility_check(false).with_size(egui::vec2(360.0, 900.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                if app.ui.right == Some(RightPane::Labeling) {
                    super::show(app, ui, &crate::theme::Tokens::light());
                }
            },
            app,
        );
        h.run();
        h.get_by_label("Rename").click();
        h.run();
        assert!(h.query_by_role(egui::accesskit::Role::TextInput).is_some());
        h.state_mut().ui.right = None;
        h.run();
        h.state_mut().ui.right = Some(RightPane::Labeling);
        h.run();
        assert!(h.query_by_role(egui::accesskit::Role::TextInput).is_none(), "the box is closed");
        assert!(shown(&h, "Default"));
    }

    /// New is off once the layer has as many classes as it keeps.
    #[test]
    fn new_is_off_at_the_class_limit() {
        let mut app = with_cities();
        for i in 0..rustgis_engine::map::symbology::MAX_LABEL_CLASSES {
            app.run("labels.class.add", json!({"name": format!("C{i}")})).unwrap();
        }
        let steps = app.session.undo_depth();
        let mut h = harness(app);
        h.run();
        assert!(h.get_by_label("New").accesskit_node().is_disabled());
        h.get_by_label("New").click();
        h.run();
        assert_eq!(h.state().session.undo_depth(), steps);
    }

    #[test]
    fn sql_drafts_survive_switching_label_classes() {
        let mut app = with_cities();
        app.run("labels.class.add", json!({"name": "Big"})).unwrap();
        let mut h = harness(app);
        h.run();
        h.get_by_label("+ New expression").click();
        h.run();
        type_clause(&mut h, "pop_max > 123");
        pick_class(&mut h, "Default", "Big");
        h.get_by_label("+ New expression").click();
        h.run();
        type_clause(&mut h, "pop_max > 456");
        pick_class(&mut h, "Big", "Default");
        assert_eq!(h.get_by_role(egui::accesskit::Role::MultilineTextInput).value().as_deref(), Some("pop_max > 123"));
        pick_class(&mut h, "Default", "Big");
        assert_eq!(h.get_by_role(egui::accesskit::Role::MultilineTextInput).value().as_deref(), Some("pop_max > 456"));
    }

    /// The SQL tab shows and sets the chosen class's clause.
    #[test]
    fn the_sql_tab_edits_the_chosen_class() {
        let mut app = with_cities();
        app.run("labels.class.add", json!({"name": "Big", "where": "pop_max > 5000000"})).unwrap();
        let mut h = harness(app);
        h.run();
        pick_class(&mut h, "Default", "Big");
        assert_eq!(h.get_by_role(egui::accesskit::Role::MultilineTextInput).value().as_deref(), Some("pop_max > 5000000"));
        h.get_by_label("Clear").click();
        h.run();
        let l = labels(h.state());
        assert_eq!((l.class("Big").unwrap().where_.clone(), l.default.where_), (None, None));
    }

    /// A colour pick is kept on the class it was made on: one left pending when another class is
    /// chosen mid-pick is finished first, as its own step naming its class.
    #[test]
    fn a_colour_pick_stays_on_its_class() {
        let mut app = with_cities();
        app.run("labels.class.add", json!({"name": "Big"})).unwrap();
        app.ws.recording = Some(Vec::new());
        let before = labels(&app).default.color;
        let mut h = egui_kittest::Harness::builder().build_ui_state(
            |ui, (app, pick): &mut (RustGisApp, Option<(Option<&'static str>, Rgba)>)| {
                if let Some((class, c)) = pick.take() {
                    let l = app.session.map().layer(app.session.active_layer.unwrap()).unwrap().clone();
                    super::color_drag(app, ui, &l, class, c);
                }
                super::finish_color_drag(app, ui);
            },
            (app, None),
        );
        let button = |pressed| egui::Event::PointerButton {
            pos: egui::pos2(4.0, 4.0),
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let (red, blue) = (Rgba::new(200, 10, 10, 255), Rgba::new(10, 10, 200, 255));
        h.event(button(true));
        h.step();
        h.state_mut().1 = Some((Some("Big"), red));
        h.step();
        let l = labels(&h.state().0);
        assert_eq!((l.class("Big").unwrap().color, l.default.color), (red, before), "the pick previews on Big only");
        h.state_mut().1 = Some((None, blue));
        h.step();
        h.event(button(false));
        h.step();
        let app = &h.state().0;
        let l = labels(app);
        assert_eq!((l.class("Big").unwrap().color, l.default.color), (red, blue));
        let classes: Vec<_> = app.ws.recording.iter().flatten().map(|s| s.params.get("class").cloned()).collect();
        assert_eq!(classes, [Some(json!("Big")), None]);
    }
}
