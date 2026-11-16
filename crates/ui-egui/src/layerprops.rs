//! The Layer Properties pane (#1803): the selected layer's settings in one place, opened from a
//! layer's right-click menu (Properties…) or View › Windows › Layer Properties; on its Query tab
//! from Data › Definition Query; on its Source tab from View › Windows › Details and, for a raster,
//! Data › Raster Information (`ui.pane.details`). As the Labeling pane, it keeps no layer settings
//! of its own: it reads the layer each frame and runs the same commands as the ribbon and the
//! layer's menu, so every change undoes on its own. Only what it shows is kept: the tab chosen
//! (`UiState::layer_properties_tab`), and in egui's memory the header's light and a name or query
//! being typed. A tab per topic shows one part at a time: General, Source, Display and, for a
//! feature layer, Query and Time. The pane follows the selected layer, as the Symbology and
//! Labeling panes do; its header (the layer's type and name, also on the pane's tab) lights up for
//! a moment when the layer changes, so a change made next isn't made on another layer unawares.

use egui::{CornerRadius, RichText, Ui, vec2};
use rustgis_engine::data::GeometryType;
use rustgis_engine::map::Layer;
use serde_json::{Value, json};

use crate::RustGisApp;
use crate::theme::{Tokens, semibold};

/// How long the header stays lit after the layer changes, in seconds.
const FLASH: f64 = 1.2;

pub fn show(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    let Some(l) = app.session.active_layer.and_then(|id| app.session.map().layer(id)).cloned() else {
        ui.label(RichText::new(tl!("Select a layer in the Layers pane.")).color(t.text_dim));
        return;
    };
    let lit = layer_changed(ui, &l);
    drop_query_when_back(ui, &l);
    header(ui, &l, lit, t);
    ui.add_space(6.0);
    match topic(app, ui, &l, t) {
        Topic::General => general(app, ui, &l),
        Topic::Source => source(app, ui, &l, t),
        Topic::Display => display(app, ui, &l, t),
        Topic::Query => query(app, ui, &l, t),
        Topic::Time => time(app, ui, &l),
    }
}

/// Drops the query being typed for `l` when the pane comes back in view (it wasn't drawn the pass
/// before: another pane was in view, or the right side was hidden), as a name being typed is
/// dropped, so the box shows the query the layer has.
fn drop_query_when_back(ui: &Ui, l: &Layer) {
    let key = egui::Id::unique("layer_properties_drawn");
    let pass = ui.ctx().cumulative_pass_nr();
    let back = ui.data(|d| d.get_temp::<u64>(key)).is_none_or(|at| at + 1 < pass);
    ui.data_mut(|d| {
        d.insert_temp(key, pass);
        if back {
            d.remove_temp::<(String, String)>(query_key(l.instance_id()));
        }
    });
}

/// How lit the header is, from 1 when the pane has just come to show another layer than it showed
/// to 0 once [`FLASH`] has passed; 0 for the first layer it shows. A name or query being typed for
/// the layer it showed is dropped, as Escape drops it.
fn layer_changed(ui: &Ui, l: &Layer) -> f32 {
    let key = egui::Id::unique("layer_properties_shown");
    let now = ui.input(|i| i.time);
    let (shown, since) = ui.data(|d| d.get_temp::<(u64, f64)>(key)).unwrap_or((l.instance_id(), f64::NEG_INFINITY));
    let since = if shown == l.instance_id() {
        since
    } else {
        ui.data_mut(|d| {
            d.remove_temp::<String>(name_key(shown));
            d.remove_temp::<(String, String)>(query_key(shown));
        });
        now
    };
    ui.data_mut(|d| d.insert_temp(key, (l.instance_id(), since)));
    let lit = (1.0 - (now - since) / FLASH).clamp(0.0, 1.0) as f32;
    if lit > 0.0 {
        ui.ctx().request_repaint();
    }
    lit
}

/// The layer's type and name in a card, lit by `lit` (0 to 1) after the layer changes.
fn header(ui: &mut Ui, l: &Layer, lit: f32, t: &Tokens) {
    let icon = if l.is_raster() {
        "raster_layer"
    } else {
        match l.geometry_type() {
            GeometryType::Point => "point_layer",
            GeometryType::Polyline => "line_layer",
            _ => "polygon_layer",
        }
    };
    let fill = t.pane_header.lerp_to_gamma(t.selection, lit);
    egui::Frame::NONE.fill(fill).corner_radius(CornerRadius::same(8)).inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), egui::Sense::hover());
            crate::icons::paint(ui.painter(), r, icon, t.icon, t.accent);
            ui.add(egui::Label::new(RichText::new(&l.name).font(semibold(14.0))).truncate());
        });
    });
}

/// The pane's tab: its name and the layer it shows, so the layer is in view wherever the pane is.
pub(crate) fn tab_name(app: &RustGisApp) -> String {
    match app.session.active_layer.and_then(|id| app.session.map().layer(id)) {
        Some(l) => {
            let name: String = l.name.chars().take(24).collect();
            let cut = if name.len() < l.name.len() { "…" } else { "" };
            crate::i18n::fmt("Layer Properties · {layer}", &[("layer", &format!("{name}{cut}"))])
        }
        None => tl!("Layer Properties").to_string(),
    }
}

/// The pane's tabs, one per topic, as the Labeling pane's.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Topic {
    General,
    Source,
    Display,
    /// A feature layer's definition query.
    Query,
    /// A feature layer's binding to the time slider.
    Time,
}

impl Topic {
    /// The topic `ui.pane.layerProperties {"tab"}` names.
    pub fn parse(code: &str) -> Option<Topic> {
        Some(match code {
            "general" => Topic::General,
            "source" => Topic::Source,
            "display" => Topic::Display,
            "query" => Topic::Query,
            "time" => Topic::Time,
            _ => return None,
        })
    }

    /// Whether only a feature layer has this topic (a raster has no definition query or time binding).
    pub fn feature_only(self) -> bool {
        matches!(self, Topic::Query | Topic::Time)
    }
}

/// Whether the pane is in view on its Source tab: what `ui.pane.details` shows, and what lights
/// the ribbon's Details and Raster Information.
pub(crate) fn source_in_view(ui: &crate::UiState) -> bool {
    ui.right == Some(crate::RightPane::LayerProperties) && ui.layer_properties_tab == Topic::Source
}

/// The tab row, and the tab chosen in it. It is the pane's, not a layer's: choosing another layer
/// keeps the topic in view. A raster has no Query or Time tab: it shows General, and the tab comes
/// back with the next feature layer.
fn topic(app: &mut RustGisApp, ui: &mut Ui, l: &Layer, t: &Tokens) -> Topic {
    let chosen = app.ui.layer_properties_tab;
    let mut tab = if l.is_raster() && chosen.feature_only() { Topic::General } else { chosen };
    let shown = tab;
    let mut tabs = vec![
        crate::widgets::Tab { label: "General", icon: "identify", hover: None, value: Topic::General },
        crate::widgets::Tab { label: "Source", icon: "add_data", hover: None, value: Topic::Source },
        crate::widgets::Tab { label: "Display", icon: "scale_range", hover: None, value: Topic::Display },
    ];
    if !l.is_raster() {
        tabs.push(crate::widgets::Tab { label: "Query", icon: "def_query", hover: None, value: Topic::Query });
        tabs.push(crate::widgets::Tab { label: "Time", icon: "time_slider", hover: None, value: Topic::Time });
    }
    crate::widgets::tabs(ui, t, egui::Id::unique("layer_properties_topic"), &mut tab, &tabs);
    if tab != shown {
        app.ui.layer_properties_tab = tab;
    }
    ui.add_space(4.0);
    tab
}

fn run(app: &mut RustGisApp, id: &str, params: Value) -> Option<Value> {
    app.run(id, params).map_err(|e| app.session.notify(e, true)).ok()
}

/// A topic's part heading, as the Symbology pane's.
fn heading(ui: &mut Ui, text: &str, t: &Tokens) {
    ui.add_space(6.0);
    ui.label(RichText::new(tl!(text)).size(11.5).color(t.text_dim));
}

/// Name, visibility, whether a feature layer's features can be selected and whether the Layers
/// pane shows its legend. Symbology and labels keep their own panes.
fn general(app: &mut RustGisApp, ui: &mut Ui, l: &Layer) {
    name_row(app, ui, l);
    ui.add_space(4.0);
    let mut visible = l.visible;
    if ui.checkbox(&mut visible, tl!("Visible")).changed() {
        run(app, "layer.visibility", json!({"layer": l.id, "visible": visible}));
    }
    if !l.is_raster() {
        let mut selectable = l.selectable;
        if ui
            .checkbox(&mut selectable, tl!("Selectable"))
            .on_hover_text(tl!("Whether the Select tool picks this layer's features on the map"))
            .changed()
        {
            run(app, "layer.selectable", json!({"layer": l.id, "on": selectable}));
        }
    }
    let mut expanded = l.expanded;
    if ui
        .checkbox(&mut expanded, tl!("Show legend in Layers"))
        .on_hover_text(tl!("Whether the Layers pane lists the layer's classes"))
        .changed()
    {
        run(app, "layer.expand", json!({"layer": l.id, "on": expanded}));
    }
}

/// Where the name being typed for the layer `instance` is kept.
fn name_key(instance: u64) -> egui::Id {
    egui::Id::unique(("layer_properties_name", instance))
}

/// The most characters `layer.rename` takes of a name: the box takes no more (a longer paste is
/// cut to fit, and a note under the box says so), so the command refuses none.
const NAME_LIMIT: usize = rustgis_engine::cmd::layer::MAX_LAYER_NAME;

/// The layer's name, renamed (`layer.rename`) when the box is left (a click elsewhere, another
/// layer's included) or Enter is pressed, and put back as it was with Escape or when left empty. A
/// press on the Layers pane lets go of the box before the pane is drawn
/// (`widgets::let_go_on_panel_press`), so on that frame the box has lost its focus but the draft,
/// kept the frame before, still names the layer it was typed for. A draft is kept only while the
/// box has focus, so a rename from elsewhere (undo, a script) shows at once, and one older than the
/// last frame (the pane was hidden mid-edit) is dropped, not shown as if it were the name. A draft
/// begun from another name than the layer's (a script or the assistant renamed it meanwhile) is
/// dropped too, so leaving the box never undoes that rename. The box is the layer's own: when the
/// layer changes without the box being left (a script, the assistant), the draft is dropped (see
/// [`layer_changed`]).
fn name_row(app: &mut RustGisApp, ui: &mut Ui, l: &Layer) {
    let key = name_key(l.instance_id());
    let (kept_at, from) = (key.with("pass"), key.with("from"));
    let pass = ui.ctx().cumulative_pass_nr();
    let fresh = ui.data(|d| d.get_temp::<u64>(kept_at)).is_some_and(|at| at + 1 >= pass)
        && ui.data(|d| d.get_temp::<String>(from)).is_some_and(|n| n == l.name);
    let mut draft = ui.data(|d| d.get_temp::<String>(key)).filter(|_| fresh).unwrap_or_else(|| l.name.clone());
    // Read before the box, which may take the key for itself.
    let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
    let full = ui.horizontal(|ui| {
        let input_label = ui.label(tl!("Name"));
        let edit = egui::TextEdit::singleline(&mut draft)
            .id_salt(("layer_properties_name", l.instance_id()))
            .char_limit(NAME_LIMIT)
            .desired_width(f32::INFINITY);
        let r = ui.add(edit).labelled_by(input_label.id);
        let full = draft.chars().count() >= NAME_LIMIT;
        if r.has_focus() {
            ui.data_mut(|d| {
                d.insert_temp(key, draft);
                d.insert_temp(kept_at, pass);
                d.insert_temp(from, l.name.clone());
            });
        } else {
            ui.data_mut(|d| {
                d.remove_temp::<String>(key);
                d.remove_temp::<u64>(kept_at);
                d.remove_temp::<String>(from);
            });
            let name = draft.trim();
            if r.lost_focus() && !escape && !name.is_empty() && name != l.name {
                run(app, "layer.rename", json!({"layer": l.id, "name": name}));
            }
        }
        full
    });
    // The box takes no more, and cuts a longer paste to fit: said, not done unseen.
    if full.inner {
        ui.weak(tl!("Names keep up to 200 characters."));
    }
}

/// What the layer is read from, and the actions on its source: those the layer's menu has, each
/// where the menu shows it, and Define Projection…, which is here only (#1803).
fn source(app: &mut RustGisApp, ui: &mut Ui, l: &Layer, t: &Tokens) {
    crate::panes::source_details(ui, l, t);
    ui.add_space(8.0);
    ui.horizontal_wrapped(|ui| {
        if l.is_streamed() && ui.button(tl!("Load All Features")).clicked() {
            // Read on a thread of its own in the desktop app (see `readjob`).
            #[cfg(not(target_arch = "wasm32"))]
            let _ = crate::readjob::run(app, "layer.loadAll", json!({"layer": l.id}));
            #[cfg(target_arch = "wasm32")]
            run(app, "layer.loadAll", json!({"layer": l.id}));
        }
        if l.linked_file().is_some() && ui.button(tl!("Repair Data Source…")).clicked() {
            run(app, "ui.dialog", json!({"name": "repairSource", "layer": l.id}));
        }
        #[cfg(not(target_arch = "wasm32"))]
        if crate::reloaddata::supported(l) && ui.button(tl!("Reload Data")).on_hover_text(tl!(crate::reloaddata::HELP)).clicked() {
            run(app, "ui.reloadData", json!({"layer": l.id}));
        }
        if !l.is_raster() && ui.button(tl!("Define Projection…")).clicked() {
            run(app, "ui.dialog", json!({"name": "defineProjection"}));
        }
    });
}

/// When and how the layer draws: its visibility range, transparency, masking, a raster's blend or
/// a feature layer's reference scale, its depth priority and, for a polygon layer, its extrusion.
fn display(app: &mut RustGisApp, ui: &mut Ui, l: &Layer, t: &Tokens) {
    heading(ui, "Visibility Range", t);
    crate::ribbon::scale_range_rows(app, ui, l, "layer_properties_scale", 12.5);
    if (l.min_scale.is_some() || l.max_scale.is_some())
        && ui.button(tl!("Clear Limits")).on_hover_text(tl!("Draw the layer at every scale.")).clicked()
    {
        run(app, "layer.scaleRange", json!({"layer": l.id}));
    }
    heading(ui, "Effects", t);
    crate::panes::transparency_row(app, ui, l);
    if l.is_raster() {
        blend_row(app, ui, l);
    }
    mask_row(app, ui, l);
    if !l.is_raster() && !l.graphics {
        crate::panes::reference_scale_row(app, ui, l);
    }
    priority_row(app, ui, l);
    if !l.is_raster() && l.geometry_type() == GeometryType::Polygon {
        heading(ui, "Extrusion", t);
        extrusion(app, ui, l);
    }
}

/// A polygon layer's extrusion, as the ribbon's Style › Extrusion, Faces and Light and Shadow
/// groups set it, for the layer the pane shows: its Type, then once extruded the constant Height
/// or the field, Factor, Base, Face Culling, Lighting (the ribbon has it in both of its groups:
/// it is one setting) and Cast Shadow. The Type and Base lists and the Factor and Base (m) rows
/// are the ribbon's own.
fn extrusion(app: &mut RustGisApp, ui: &mut Ui, l: &Layer) {
    let x = l.extrusion.as_ref();
    let fields = crate::ribbon::numeric_fields(l);
    ui.horizontal(|ui| {
        let input_label = ui.label(tl!("Type"));
        let shown = match x {
            None => tl!("None").to_string(),
            Some(x) => match &x.field {
                None => tl!("Constant height").to_string(),
                Some(f) => crate::i18n::fmt("By {field}", &[("field", f)]),
            },
        };
        egui::ComboBox::from_id_salt(("layer_properties_extrusion", l.instance_id()))
            .selected_text(shown)
            .width(160.0)
            .show_ui(ui, |ui| {
                crate::ribbon::extrusion_type_choices(ui, app, l.id, x, &fields);
            })
            .response
            .labelled_by(input_label.id);
    });
    let Some(x) = x else { return };
    if x.field.is_none() {
        height_row(app, ui, l, x);
    }
    crate::ribbon::extrusion_factor_row(app, ui, l.id, x, "layer_properties", 12.5);
    ui.horizontal(|ui| {
        let input_label = ui.label(tl!("Base"));
        let shown = match &x.base_field {
            Some(f) => crate::i18n::fmt("By {field}", &[("field", f)]),
            None if x.base > 0.0 => crate::i18n::fmt("Constant ({m} m)", &[("m", &x.base.to_string())]),
            None => tl!("Ground").to_string(),
        };
        egui::ComboBox::from_id_salt(("layer_properties_extrusion_base", l.instance_id()))
            .selected_text(shown)
            .width(160.0)
            .show_ui(ui, |ui| {
                crate::ribbon::extrusion_base_choices(ui, app, l.id, x, &fields);
            })
            .response
            .labelled_by(input_label.id)
            .on_hover_text(tl!("What the walls stand on: the ground, a field or the constant Base (m)."));
    });
    crate::ribbon::extrusion_base_row(app, ui, l.id, x, "layer_properties", 12.5);
    let mut culling = x.face_culling;
    if ui.checkbox(&mut culling, tl!("Face Culling")).on_hover_text(tl!("Hide walls facing away from you.")).changed() {
        run(app, "layer.faces", json!({"layer": l.id, "culling": culling}));
    }
    let mut lighting = x.lighting;
    if ui.checkbox(&mut lighting, tl!("Lighting")).on_hover_text(tl!("Shade walls by the way they face the light.")).changed() {
        run(app, "layer.faces", json!({"layer": l.id, "lighting": lighting}));
    }
    let mut shadows = x.shadows;
    if ui.checkbox(&mut shadows, tl!("Cast Shadow")).on_hover_text(tl!("Extruded shapes cast shadows on the map.")).changed() {
        run(app, "layer.lightShadow", json!({"layer": l.id, "shadows": shadows}));
    }
}

/// The constant height of `l`'s extrusion, in metres before the Factor (`layer.extrusion`),
/// which the ribbon sets only by choosing Constant height. A drag is one step, set when let go; a
/// saved height out of range is shown, not written back.
fn height_row(app: &mut RustGisApp, ui: &mut Ui, l: &Layer, x: &rustgis_engine::map::effects::Extrusion) {
    let mut h = if x.height.is_finite() { x.height } else { 0.0 };
    ui.horizontal(|ui| {
        let input_label = ui.label(tl!("Height (m)"));
        let drag = crate::widgets::Dragged::lend(ui, egui::Id::unique(("layer_properties_extrusion_height", l.instance_id())), &mut h);
        let r = ui
            .add(
                egui::DragValue::new(&mut h)
                    .update_while_editing(false)
                    .range(0.0..=1e7)
                    .clamp_existing_to_range(false)
                    .speed(x.height.max(10.0) * 0.02),
            )
            .labelled_by(input_label.id);
        if drag.settled(ui, &r, &mut h) && h != x.height {
            run(app, "layer.extrusion", json!({"layer": l.id, "type": "height", "height": h}));
        }
    });
}

/// The layer's depth priority (`layer.priority`), as the ribbon's Raise and Lower Priority set it:
/// a higher one draws above lower ones, whatever the layer order. A drag is one step, set when let go.
fn priority_row(app: &mut RustGisApp, ui: &mut Ui, l: &Layer) {
    ui.horizontal(|ui| {
        let input_label = ui.label(tl!("Depth priority"));
        let mut v = l.priority;
        let drag = crate::widgets::Dragged::lend(ui, egui::Id::unique(("layer_properties_priority", l.instance_id())), &mut v);
        let r = ui
            .add(egui::DragValue::new(&mut v).range(-1000..=1000).speed(0.1))
            .on_hover_text(tl!("Higher draws on top of lower, whatever the layer order"))
            .labelled_by(input_label.id);
        if drag.settled(ui, &r, &mut v) && v != l.priority {
            run(app, "layer.priority", json!({"layer": l.id, "value": v}));
        }
    });
}

/// How a raster's colours combine with what's beneath (`layer.blend`), as the ribbon's Layer
/// Blend menu offers it.
fn blend_row(app: &mut RustGisApp, ui: &mut Ui, l: &Layer) {
    ui.horizontal(|ui| {
        let input_label = ui.label(tl!("Layer Blend"));
        egui::ComboBox::from_id_salt("layer_properties_blend")
            .selected_text(tl!(l.blend.label()))
            .width(130.0)
            .show_ui(ui, |ui| {
                for b in rustgis_engine::map::effects::Blend::ALL {
                    if ui.selectable_label(l.blend == b, tl!(b.label())).clicked() && l.blend != b {
                        run(app, "layer.blend", json!({"layer": l.id, "mode": b.code()}));
                    }
                }
            })
            .response
            .labelled_by(input_label.id);
    });
}

/// The polygon layer the layer shows only inside (`layer.mask`), as the ribbon's Masking menu
/// offers it.
fn mask_row(app: &mut RustGisApp, ui: &mut Ui, l: &Layer) {
    let choices = crate::ribbon::mask_choices(app, l);
    ui.horizontal(|ui| {
        let input_label = ui.label(tl!("Masking"));
        let none = tl!("No mask").to_string();
        egui::ComboBox::from_id_salt("layer_properties_mask")
            .selected_text(l.mask.clone().unwrap_or_else(|| none.clone()))
            .width(130.0)
            .show_ui(ui, |ui| {
                if ui.selectable_label(l.mask.is_none(), none).clicked() && l.mask.is_some() {
                    run(app, "layer.mask", json!({"layer": l.id, "mask": null}));
                }
                for n in &choices {
                    let on = l.mask.as_deref() == Some(n.as_str());
                    if ui.selectable_label(on, n.as_str()).clicked() && !on {
                        run(app, "layer.mask", json!({"layer": l.id, "mask": n}));
                    }
                }
            })
            .response
            .labelled_by(input_label.id);
    });
}

/// The layer's binding to the time slider (`time.bind`, `time.unbind`), as the slider's Layers
/// menu sets it: whether the slider's window picks the features that draw, and by which date
/// field. Finding the fields that hold dates reads up to 500 values of each, so it is done only
/// while the field list is open, as that menu does. The slider's own settings stay on View › Time.
fn time(app: &mut RustGisApp, ui: &mut Ui, l: &Layer) {
    let bound = l.time.as_ref().map(|b| b.field.clone());
    let mut on = bound.is_some();
    if ui
        .checkbox(&mut on, tl!("Bound to the time slider"))
        .on_hover_text(tl!("Only the features whose date falls in the slider's window draw"))
        .changed()
    {
        bind(app, l, on, None);
    }
    ui.horizontal(|ui| {
        let input_label = ui.label(tl!("Date field"));
        let shown = bound.clone().unwrap_or_else(|| tl!("Not bound").to_string());
        egui::ComboBox::from_id_salt(("layer_properties_time_field", l.instance_id()))
            .selected_text(shown)
            .width(160.0)
            .show_ui(ui, |ui| {
                // Read only while the list is open: finding date fields reads every field's values.
                let fields = rustgis_engine::map::time::time_fields(&l.dataset);
                if fields.is_empty() {
                    ui.label(tl!("No field of this layer holds dates."));
                }
                for (f, share) in &fields {
                    let on = bound.as_deref() == Some(f.as_str());
                    if ui.selectable_label(on, crate::timebar::field_share(f, *share)).clicked() && !on {
                        bind(app, l, true, Some(f));
                    }
                }
            })
            .response
            .labelled_by(input_label.id);
    });
    ui.weak(tl!("The slider's span, step and playback are on View › Time."));
}

/// Binds the layer to the time slider (by `field`, or the one most of whose values read as dates),
/// showing the slider, or unbinds it.
fn bind(app: &mut RustGisApp, l: &Layer, on: bool, field: Option<&str>) {
    if !on {
        run(app, "time.unbind", json!({"layer": l.id}));
        return;
    }
    let mut p = json!({"layer": l.id});
    if let Some(f) = field {
        p["field"] = json!(f);
    }
    if run(app, "time.bind", p).is_some() {
        app.time.hidden = false;
    }
}

/// Where the query being typed for the layer `instance` is kept, with the query it was begun from.
fn query_key(instance: u64) -> egui::Id {
    egui::Id::unique(("layer_properties_query", instance))
}

/// The layer's definition query (`layer.definitionQuery`): only the features matching the clause
/// draw. The clause typed is set with Apply or Ctrl+Enter, and Clear removes the query; until then
/// it is kept for this layer while a field or operator button is pressed or another tab is looked
/// at. It is dropped, and the box shows the layer's query, with Escape in the box (Escape
/// elsewhere is another control's: a map tool's, a menu's), when another layer is shown (see [`layer_changed`]), when the pane comes back in view (see [`drop_query_when_back`])
/// and when the query changes from elsewhere (undo, a script), as a name being typed is. The box
/// cuts nothing: a clause longer than the parser reads is said to be too long, and isn't applied.
fn query(app: &mut RustGisApp, ui: &mut Ui, l: &Layer, t: &Tokens) {
    let key = query_key(l.instance_id());
    let current = l.definition_query.clone().unwrap_or_default();
    let mut text = ui
        .data(|d| d.get_temp::<(String, String)>(key))
        .filter(|(_, from)| *from == current)
        .map_or_else(|| current.clone(), |(text, _)| text);
    // Read before the box, which takes Escape to let go.
    let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
    ui.label(RichText::new(tl!("Only the features matching this clause draw. Ctrl+Enter applies it.")).weak());
    let input_label = ui.label(RichText::new(tl!("Where")).font(semibold(12.5)));
    let r = ui
        .add(
            egui::TextEdit::multiline(&mut text)
                .id_salt(("layer_properties_query", l.instance_id()))
                .hint_text(tl!("e.g. continent = 'Europe'"))
                .desired_rows(3)
                .desired_width(f32::INFINITY)
                .font(egui::TextStyle::Monospace),
        )
        .labelled_by(input_label.id);
    if escape && r.lost_focus() {
        text = current.clone();
    }
    crate::dialogs::field_chips(ui, &l.dataset.fields, &mut text);
    let clause = text.trim();
    let invalid = if clause.is_empty() { None } else { rustgis_engine::data::query::parse(clause, &l.dataset).err() };
    if let Some(e) = &invalid {
        ui.label(RichText::new(crate::i18n::fmt("Invalid: {error}", &[("error", e)])).small().color(t.red));
    }
    let can_apply = invalid.is_none() && clause != current;
    let ctrl_enter = r.has_focus() && ui.input(|i| crate::sqlpane::run_pressed(&i.events));
    let mut done = false;
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if ui.add_enabled(can_apply, egui::Button::new(tl!("Apply"))).clicked() || (ctrl_enter && can_apply) {
            done = run(app, "layer.definitionQuery", json!({"layer": l.id, "where": clause})).is_some();
        }
        if ui.add_enabled(l.definition_query.is_some(), egui::Button::new(tl!("Clear"))).on_hover_text(tl!("Draw every feature.")).clicked()
        {
            done = run(app, "layer.definitionQuery", json!({"layer": l.id, "where": ""})).is_some();
        }
    });
    ui.data_mut(|d| {
        if done || text == current {
            d.remove_temp::<(String, String)>(key);
        } else {
            d.insert_temp(key, (text, current));
        }
    });
}

#[cfg(test)]
mod tests {
    use egui::accesskit::Role;
    use egui_kittest::kittest::Queryable;
    use serde_json::json;

    use crate::{RightPane, RustGisApp};

    fn layer_id(app: &RustGisApp, name: &str) -> u32 {
        app.session.map().layers.iter().find(|l| l.name == name).unwrap().id
    }

    fn with_cities() -> RustGisApp {
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
                // Another pane in view: this one isn't drawn, as in the app.
                if app.ui.right == Some(RightPane::Symbology) {
                    return;
                }
                super::show(app, ui, &crate::theme::Tokens::light());
            },
            app,
        )
    }

    fn open_tab(h: &mut egui_kittest::Harness<'static, RustGisApp>, topic: &str) {
        h.get_by_role_and_label(Role::Button, topic).click();
        h.run();
    }

    #[test]
    fn the_pane_command_needs_a_layer() {
        let mut app = RustGisApp::with_sample();
        app.session.active_layer = None;
        assert!(app.run("ui.pane.layerProperties", json!(null)).is_err(), "no layer selected");
        let cities = layer_id(&app, "Cities");
        app.session.run("layer.select", &json!({"layer": cities})).unwrap();
        assert!(app.run("ui.pane.layerProperties", json!({"layer": "No Such Layer"})).is_err());
        assert_eq!(app.session.active_layer, Some(cities), "a refusal leaves the selection as it was");
        app.session.active_layer = None;
        app.run("ui.pane.layerProperties", json!({"layer": cities})).unwrap();
        assert_eq!(app.session.active_layer, Some(cities), "the layer given is selected");
        assert_eq!(app.ui.right, Some(RightPane::LayerProperties));
        // A raster has properties too.
        if let Some(raster) = app.session.map().layers.iter().find(|l| l.is_raster()).map(|l| l.id) {
            app.run("ui.pane.layerProperties", json!({"layer": raster})).unwrap();
            assert_eq!(app.session.active_layer, Some(raster));
        }
        app.run("ui.pane.layerProperties", json!({"close": true})).unwrap();
        assert!(!app.ui.right_open.contains(&RightPane::LayerProperties));
    }

    /// `ui.pane.details` (the ribbon's Details and Raster Information) shows the Source tab, and
    /// hides the pane when it is in view on it; `close` hides it, and `layer` shows that layer's.
    #[test]
    fn details_shows_the_source_tab() {
        let mut app = RustGisApp::with_sample();
        app.session.active_layer = None;
        assert!(app.run("ui.pane.details", json!(null)).is_err(), "no layer selected");
        let cities = layer_id(&app, "Cities");
        app.session.run("layer.select", &json!({"layer": cities})).unwrap();
        app.run("ui.pane.layerProperties", json!({"tab": "query"})).unwrap();
        app.run("ui.pane.details", json!(null)).unwrap();
        assert!(super::source_in_view(&app.ui), "from another tab, Source is shown");
        app.run("ui.pane.details", json!(null)).unwrap();
        assert!(!app.ui.right_open.contains(&RightPane::LayerProperties), "a second press hides it");
        app.run("ui.pane.details", json!(null)).unwrap();
        app.run("ui.pane.details", json!({"close": true})).unwrap();
        assert!(!app.ui.right_open.contains(&RightPane::LayerProperties), "close hides it");
        // A 2 × 2 ASCII grid.
        let grid = "bmNvbHMgMgpucm93cyAyCnhsbGNvcm5lciAwCnlsbGNvcm5lciAwCmNlbGxzaXplIDEKTk9EQVRBX3ZhbHVlIC05OTk5CjEgMgozIDQK";
        app.session.run("layer.addDataBytes", &json!({"fileName": "grid.asc", "data": grid})).unwrap();
        let raster = app.session.map().layers.iter().find(|l| l.is_raster()).map(|l| l.id).unwrap();
        app.session.run("layer.select", &json!({"layer": cities})).unwrap();
        app.run("ui.pane.details", json!({"layer": raster})).unwrap();
        assert_eq!(app.session.active_layer, Some(raster), "the layer given is selected");
        assert!(super::source_in_view(&app.ui), "a raster's information is on Source too");
        app.run("ui.pane.details", json!({"layer": raster})).unwrap();
        assert!(super::source_in_view(&app.ui), "naming a layer shows it, even when in view");
    }

    /// A polygon layer's Display tab sets its extrusion with the ribbon's commands, for the layer
    /// the pane shows, each change a step of its own; choosing the type it has changes nothing. A
    /// point layer has no Extrusion part.
    #[test]
    fn the_display_tab_sets_a_polygon_layers_extrusion() {
        let app = with_cities();
        let countries = layer_id(&app, "Countries");
        let mut h = harness(app);
        h.run();
        open_tab(&mut h, "Display");
        assert!(h.query_by_role_and_label(Role::ComboBox, "Type").is_none(), "no extrusion for points");
        h.state_mut().session.run("layer.select", &json!({"layer": countries})).unwrap();
        h.run_steps(2);
        let x = |h: &egui_kittest::Harness<'static, RustGisApp>| h.state().session.map().layer(countries).unwrap().extrusion.clone();
        assert!(h.query_by_role_and_label(Role::ComboBox, "Base").is_none(), "nothing more until extruded");
        let steps = h.state().session.undo_depth();
        for _ in 0..2 {
            h.get_by_role_and_label(Role::ComboBox, "Type").click();
            h.run();
            h.get_by_role_and_label(Role::RadioButton, "Constant height").click();
            h.run();
        }
        let set = x(&h).unwrap();
        assert!(set.field.is_none() && set.height == 50_000.0, "{set:?}");
        assert_eq!(h.state().session.undo_depth(), steps + 1, "choosing it again changes nothing");
        for label in ["Height (m)", "Factor", "Base (m)"] {
            assert!(h.query_by_role_and_label(Role::SpinButton, label).is_some(), "missing input: {label}");
        }
        // Dragged right over several frames, then let go: one step.
        let at = h.get_by_role_and_label(Role::SpinButton, "Height (m)").rect().center();
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        h.event(egui::Event::PointerMoved(at));
        h.step();
        h.event(button(at, true));
        h.step();
        for i in 1..=4 {
            h.event(egui::Event::PointerMoved(at + egui::vec2(10.0 * i as f32, 0.0)));
            h.step();
        }
        h.event(button(at + egui::vec2(40.0, 0.0), false));
        h.step();
        h.run();
        assert!(x(&h).unwrap().height > 50_000.0, "{:?}", x(&h));
        assert_eq!(h.state().session.undo_depth(), steps + 2, "a drag is one step");
        h.get_by_role_and_label(Role::ComboBox, "Base").click();
        h.run();
        h.get_by_role_and_label(Role::RadioButton, "By pop_est").click();
        h.run();
        assert_eq!(x(&h).unwrap().base_field.as_deref(), Some("pop_est"));
        h.get_by_role_and_label(Role::ComboBox, "Base").click();
        h.run();
        h.get_by_role_and_label(Role::RadioButton, "By pop_est").click();
        h.run();
        assert_eq!(h.state().session.undo_depth(), steps + 3, "choosing the base it has changes nothing");
        h.get_by_role_and_label(Role::CheckBox, "Cast Shadow").click();
        h.run();
        h.get_by_role_and_label(Role::CheckBox, "Lighting").click();
        h.run();
        let set = x(&h).unwrap();
        assert!(set.shadows && !set.lighting && set.face_culling, "{set:?}");
        assert_eq!(h.state().session.undo_depth(), steps + 5, "each change a step of its own");
        h.get_by_role_and_label(Role::ComboBox, "Type").click();
        h.run();
        h.get_by_role_and_label(Role::RadioButton, "None").click();
        h.run();
        assert!(x(&h).is_none());
        assert!(h.query_by_role_and_label(Role::CheckBox, "Cast Shadow").is_none(), "nothing more once flat");
    }

    /// Changing the Factor keeps a height below 1,000 m as it is, constant or the fallback of a
    /// field extrusion (used where the field holds no number): left out of the command, the
    /// height would be raised to 1,000 m.
    #[test]
    fn the_factor_keeps_a_low_constant_height() {
        for kind in [json!({"type": "height", "height": 200}), json!({"type": "field", "field": "pop_est", "height": 200})] {
            factor_keeps_the_height(kind);
        }
    }

    fn factor_keeps_the_height(kind: serde_json::Value) {
        let mut app = with_cities();
        let countries = layer_id(&app, "Countries");
        app.session.run("layer.select", &json!({"layer": countries})).unwrap();
        app.session.run("layer.extrusion", &kind).unwrap();
        let mut h = harness(app);
        h.run();
        open_tab(&mut h, "Display");
        let at = h.get_by_role_and_label(Role::SpinButton, "Factor").rect().center();
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        h.event(egui::Event::PointerMoved(at));
        h.step();
        h.event(button(at, true));
        h.step();
        for i in 1..=4 {
            h.event(egui::Event::PointerMoved(at + egui::vec2(10.0 * i as f32, 0.0)));
            h.step();
        }
        h.event(button(at + egui::vec2(40.0, 0.0), false));
        h.step();
        h.run();
        let x = h.state().session.map().layer(countries).unwrap().extrusion.clone().unwrap();
        assert!(x.factor > 1.0, "the factor changed: {x:?}");
        assert_eq!(x.height, 200.0, "the height is kept: {kind} {x:?}");
    }

    /// Each tab's inputs are reachable by their row labels.
    #[test]
    fn inputs_are_reachable_by_their_row_labels() {
        let mut h = harness(with_cities());
        h.run();
        assert!(h.query_by_role_and_label(Role::TextInput, "Name").is_some());
        assert!(h.query_by_role_and_label(Role::CheckBox, "Visible").is_some());
        assert!(h.query_by_role_and_label(Role::CheckBox, "Selectable").is_some());
        open_tab(&mut h, "Display");
        for label in ["Out Beyond", "In Beyond", "Masking", "Reference scale"] {
            assert!(h.query_by_role_and_label(Role::ComboBox, label).is_some(), "missing input: {label}");
        }
        assert!(h.query_by_role_and_label(Role::SpinButton, "Depth priority").is_some());
        assert!(h.query_by_role_and_label(Role::ComboBox, "Layer Blend").is_none(), "blend is a raster's");
        open_tab(&mut h, "Source");
        assert!(h.query_by_label("Geometry").is_some(), "the Details rows");
        assert!(h.query_by_role_and_label(Role::Button, "Define Projection…").is_some(), "here, not in the layer menu");
        open_tab(&mut h, "Query");
        assert!(h.query_by_role_and_label(Role::MultilineTextInput, "Where").is_some());
    }

    /// A raster layer of 2 × 2 cells, added to the map; its id.
    fn add_raster(app: &mut RustGisApp) -> u32 {
        use rustgis_engine::data::raster::{ColorHint, Crs, GeoTransform, Raster};
        let gt = GeoTransform { origin_x: 0.0, origin_y: 1.0, pixel_width: 0.5, pixel_height: -0.5 };
        let r = Raster::new("dem", 2, 2, vec![vec![0.0; 4]], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
        app.session.map_mut().add_raster("dem", rustgis_engine::map::RasterLayer::from_path(r, "dem.tif"))
    }

    fn query_box(h: &egui_kittest::Harness<'static, RustGisApp>) -> String {
        h.get_by_role_and_label(Role::MultilineTextInput, "Where").value().unwrap_or_default()
    }

    fn type_query(h: &mut egui_kittest::Harness<'static, RustGisApp>, text: &str) {
        h.get_by_role_and_label(Role::MultilineTextInput, "Where").click();
        h.run();
        h.get_by_role_and_label(Role::MultilineTextInput, "Where").type_text(text);
        h.run();
    }

    /// `{"tab"}` shows that tab, and shows the pane even when it is already in view; the Query tab
    /// is refused for a raster, and an unknown tab is refused.
    #[test]
    fn the_pane_command_opens_a_tab() {
        let mut app = with_cities();
        let cities = app.session.active_layer.unwrap();
        app.run("ui.pane.layerProperties", json!({"tab": "query"})).unwrap();
        assert_eq!((app.ui.right, app.ui.layer_properties_tab), (Some(RightPane::LayerProperties), super::Topic::Query));
        app.run("ui.pane.layerProperties", json!({"tab": "query"})).unwrap();
        assert_eq!(app.ui.right, Some(RightPane::LayerProperties), "asked for a tab, the pane stays in view");
        for bad in [json!({"tab": "nope"}), json!({"tab": 3})] {
            assert!(app.run("ui.pane.layerProperties", bad).is_err());
        }
        let raster = add_raster(&mut app);
        assert!(app.run("ui.pane.layerProperties", json!({"layer": raster, "tab": "query"})).is_err(), "a raster has no query");
        assert!(app.run("ui.pane.layerProperties", json!({"layer": raster, "tab": "time"})).is_err(), "nor a time binding");
        assert_eq!(app.session.active_layer, Some(cities), "a refusal doesn't select the raster");
        app.run("ui.pane.layerProperties", json!({"layer": raster, "tab": "display"})).unwrap();
        assert_eq!((app.session.active_layer, app.ui.layer_properties_tab), (Some(raster), super::Topic::Display));
    }

    /// A feature layer of two points, added to the map and selected; its id. With `with_dates` they
    /// have two date fields (`when` and `seen`), else a name only.
    fn add_events(app: &mut RustGisApp, with_dates: bool) -> u32 {
        let props = |when: &str, seen: &str| if with_dates { json!({"when": when, "seen": seen}) } else { json!({"name": "Fair"}) };
        let text = json!({"type": "FeatureCollection", "features": [
            {"type": "Feature", "geometry": {"type": "Point", "coordinates": [0, 0]}, "properties": props("2025-01-01", "2024-06-01")},
            {"type": "Feature", "geometry": {"type": "Point", "coordinates": [1, 1]}, "properties": props("2025-02-01", "2024-07-01")},
        ]})
        .to_string();
        let (ds, _) = rustgis_engine::data::io::read_geojson(&text, "Events").unwrap();
        let id = app.session.map_mut().add_dataset(ds);
        app.session.run("layer.select", &json!({"layer": id})).unwrap();
        id
    }

    /// The Time tab binds the layer to the time slider and unbinds it, a step each, and binding
    /// shows the slider's bar.
    #[test]
    fn the_time_tab_binds_and_unbinds_the_layer() {
        let mut app = with_cities();
        let id = add_events(&mut app, true);
        app.time.hidden = true;
        let steps = app.session.undo_depth();
        let mut h = harness(app);
        h.run();
        open_tab(&mut h, "Time");
        h.get_by_role_and_label(Role::CheckBox, "Bound to the time slider").click();
        h.run();
        assert!(h.state().session.map().layer(id).unwrap().time.is_some(), "bound");
        assert!(!h.state().time.hidden, "the bar shows");
        assert_eq!(h.state().session.undo_depth(), steps + 1);
        h.get_by_role_and_label(Role::CheckBox, "Bound to the time slider").click();
        h.run();
        assert!(h.state().session.map().layer(id).unwrap().time.is_none(), "unbound");
        assert_eq!(h.state().session.undo_depth(), steps + 2);
    }

    /// The date field list offers the fields that hold dates, and choosing one binds the layer by
    /// it. `when` is chosen: binding with no field given would take `seen`, the first of the two.
    #[test]
    fn the_date_field_list_binds_by_the_field_chosen() {
        let mut app = with_cities();
        let id = add_events(&mut app, true);
        let mut h = harness(app);
        h.run();
        open_tab(&mut h, "Time");
        h.get_by_role_and_label(Role::ComboBox, "Date field").click();
        h.run();
        h.get_by_label("when (100%)").click();
        h.run();
        let field = h.state().session.map().layer(id).unwrap().time.as_ref().map(|t| t.field.clone());
        assert_eq!(field.as_deref(), Some("when"));
    }

    /// A layer with no dates isn't bound, and the notice says why.
    #[test]
    fn a_layer_without_dates_says_so() {
        let mut app = with_cities();
        let id = add_events(&mut app, false);
        let mark = app.session.notice_mark();
        let mut h = harness(app);
        h.run();
        open_tab(&mut h, "Time");
        h.get_by_role_and_label(Role::CheckBox, "Bound to the time slider").click();
        h.run();
        assert!(h.state().session.map().layer(id).unwrap().time.is_none());
        assert!(h.state().session.notices_since(mark).iter().any(|n| n.text.contains("no field of this layer holds dates")));
    }

    /// A raster has no Query or Time tab: the pane shows General instead, and the tab comes back
    /// with the next feature layer.
    #[test]
    fn a_raster_shows_general_for_the_query_and_time_tabs() {
        let mut app = with_cities();
        let cities = app.session.active_layer.unwrap();
        let raster = add_raster(&mut app);
        app.run("ui.pane.layerProperties", json!({"tab": "query"})).unwrap();
        app.session.run("layer.select", &json!({"layer": raster})).unwrap();
        let mut h = harness(app);
        h.run_steps(2);
        assert!(h.query_by_role_and_label(Role::Button, "Query").is_none(), "no Query tab");
        assert!(h.query_by_role_and_label(Role::TextInput, "Name").is_some(), "General in view");
        h.state_mut().session.run("layer.select", &json!({"layer": cities})).unwrap();
        h.run_steps(2);
        assert!(h.query_by_role_and_label(Role::MultilineTextInput, "Where").is_some(), "Query in view again");
        h.state_mut().ui.layer_properties_tab = super::Topic::Time;
        h.state_mut().session.run("layer.select", &json!({"layer": raster})).unwrap();
        h.run_steps(2);
        assert!(h.query_by_role_and_label(Role::Button, "Time").is_none(), "no Time tab");
        assert!(h.query_by_role_and_label(Role::TextInput, "Name").is_some(), "General in view");
        h.state_mut().session.run("layer.select", &json!({"layer": cities})).unwrap();
        h.run_steps(2);
        assert!(h.query_by_role_and_label(Role::CheckBox, "Bound to the time slider").is_some(), "Time in view again");
    }

    /// Apply sets the clause typed, Clear removes it, each a step of its own; a clause the layer
    /// can't read says why and isn't applied.
    #[test]
    fn the_query_tab_applies_and_clears_the_definition_query() {
        let app = with_cities();
        let id = app.session.active_layer.unwrap();
        let steps = app.session.undo_depth();
        let mut h = harness(app);
        h.run();
        open_tab(&mut h, "Query");
        type_query(&mut h, "no_such_field > 1");
        assert!(h.query_by_label_contains("Invalid:").is_some(), "the error is shown");
        h.get_by_role_and_label(Role::Button, "Apply").click();
        h.run();
        assert_eq!(h.state().session.map().layer(id).unwrap().definition_query, None, "not applied");
        h.get_by_role_and_label(Role::MultilineTextInput, "Where").click();
        h.run();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        h.key_press(egui::Key::Backspace);
        h.run();
        h.get_by_role_and_label(Role::MultilineTextInput, "Where").type_text("pop_max > 1000000");
        h.run();
        assert!(h.query_by_label_contains("Invalid:").is_none());
        h.get_by_role_and_label(Role::Button, "Apply").click();
        h.run();
        assert_eq!(h.state().session.map().layer(id).unwrap().definition_query.as_deref(), Some("pop_max > 1000000"));
        assert_eq!(h.state().session.undo_depth(), steps + 1, "one step");
        assert_eq!(query_box(&h), "pop_max > 1000000", "the box shows the layer's query");
        h.get_by_role_and_label(Role::Button, "Clear").click();
        h.run();
        assert_eq!(h.state().session.map().layer(id).unwrap().definition_query, None);
        assert_eq!(h.state().session.undo_depth(), steps + 2);
        assert_eq!(query_box(&h), "");
    }

    /// A clause longer than the parser reads is kept whole in the box, said to be too long and not
    /// applied: nothing is cut unsaid.
    #[test]
    fn a_clause_too_long_is_said_not_cut() {
        let app = with_cities();
        let id = app.session.active_layer.unwrap();
        let mut h = harness(app);
        h.run();
        open_tab(&mut h, "Query");
        let long = format!("name = '{}'", "x".repeat(rustgis_engine::data::query::MAX_LEN));
        type_query(&mut h, &long);
        assert_eq!(query_box(&h).chars().count(), long.chars().count(), "nothing cut");
        assert!(h.query_by_label_contains("too long").is_some(), "the length is said");
        h.get_by_role_and_label(Role::Button, "Apply").click();
        h.run();
        assert_eq!(h.state().session.map().layer(id).unwrap().definition_query, None, "not applied");
    }

    /// A clause being typed when the pane is hidden is dropped: back in view, the box shows the
    /// layer's query.
    #[test]
    fn hiding_the_pane_drops_the_query_being_typed() {
        let app = with_cities();
        let mut h = harness(app);
        h.run();
        open_tab(&mut h, "Query");
        type_query(&mut h, "pop_max > 1");
        h.state_mut().ui.right = Some(RightPane::Symbology);
        h.run();
        h.state_mut().ui.right = Some(RightPane::LayerProperties);
        h.run();
        assert_eq!(query_box(&h), "", "the layer's own query");
    }

    /// A field or operator button sets its word apart from a name typed just before it.
    #[test]
    fn the_buttons_set_their_word_apart() {
        let app = with_cities();
        let mut h = harness(app);
        h.run();
        open_tab(&mut h, "Query");
        type_query(&mut h, "name");
        h.get_by_role_and_label(Role::Button, "LIKE").click();
        h.run();
        assert_eq!(query_box(&h), "name LIKE ");
    }

    /// The Functions and Variables menus list the language's catalogs: a function is added open
    /// for its arguments, and what follows it goes inside, not set apart from the bracket.
    #[test]
    fn the_functions_and_variables_menus_add_from_the_catalogs() {
        use rustgis_engine::data::query::{VARS, functions};
        let app = with_cities();
        let mut h = harness(app);
        h.run();
        open_tab(&mut h, "Query");
        type_query(&mut h, "name =");
        // One that takes arguments, so it is added open.
        let first = functions().find(|f| f.max > 0).unwrap();
        h.get_by_role_and_label(Role::Button, "Functions ▾").click();
        h.run();
        h.get_by_role_and_label(Role::Button, &format!("{}()", first.name)).click();
        h.run();
        assert_eq!(query_box(&h), format!("name = {}(", first.name));
        let var = VARS[0].names[0];
        h.get_by_role_and_label(Role::Button, "Variables ▾").click();
        h.run();
        h.get_by_role_and_label(Role::Button, var).click();
        h.run();
        assert_eq!(query_box(&h), format!("name = {}({var} ", first.name));
    }

    /// Ctrl+Enter applies the clause without breaking the line; Escape drops what was typed.
    #[test]
    fn ctrl_enter_applies_and_escape_drops_the_query() {
        let app = with_cities();
        let id = app.session.active_layer.unwrap();
        let mut h = harness(app);
        h.run();
        open_tab(&mut h, "Query");
        type_query(&mut h, "pop_max > 1000000");
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Enter);
        h.run();
        assert_eq!(h.state().session.map().layer(id).unwrap().definition_query.as_deref(), Some("pop_max > 1000000"));
        assert_eq!(query_box(&h), "pop_max > 1000000", "no line break");
        h.get_by_role_and_label(Role::MultilineTextInput, "Where").type_text(" AND pop_max < 5000000");
        h.run();
        h.key_press(egui::Key::Escape);
        h.run();
        assert_eq!(query_box(&h), "pop_max > 1000000", "Escape drops the clause typed");
        assert_eq!(h.state().session.map().layer(id).unwrap().definition_query.as_deref(), Some("pop_max > 1000000"));
    }

    /// The field and operator buttons add to the clause; it is kept while another tab is looked
    /// at, and dropped when another layer is chosen or the query changes from elsewhere.
    #[test]
    fn the_query_being_typed_is_the_layers_own() {
        let app = with_cities();
        let cities = app.session.active_layer.unwrap();
        let countries = layer_id(&app, "Countries");
        let mut h = harness(app);
        h.run();
        open_tab(&mut h, "Query");
        h.get_by_role_and_label(Role::Button, "pop_max").click();
        h.run();
        h.get_by_role_and_label(Role::Button, ">").click();
        h.run();
        assert_eq!(query_box(&h), "pop_max > ");
        open_tab(&mut h, "General");
        open_tab(&mut h, "Query");
        assert_eq!(query_box(&h), "pop_max > ", "kept while another tab was in view");
        h.state_mut().session.run("layer.definitionQuery", &json!({"layer": cities, "where": "pop_max > 5"})).unwrap();
        h.run();
        assert_eq!(query_box(&h), "pop_max > 5", "a query set elsewhere shows at once");
        h.get_by_role_and_label(Role::Button, "megacity").click();
        h.run();
        assert_eq!(query_box(&h), "pop_max > 5 megacity ");
        // The header keeps repainting while it is lit, so these frames are stepped, not run.
        h.state_mut().session.run("layer.select", &json!({"layer": countries})).unwrap();
        h.run_steps(2);
        assert_eq!(query_box(&h), "", "Countries' own query");
        h.state_mut().session.run("layer.select", &json!({"layer": cities})).unwrap();
        h.run_steps(2);
        assert_eq!(query_box(&h), "pop_max > 5", "dropped, not back with Cities");
    }

    /// A new name is one rename, on Enter; Escape leaves the name as it was.
    #[test]
    fn the_name_box_renames_the_layer() {
        let app = with_cities();
        let id = app.session.active_layer.unwrap();
        let steps = app.session.undo_depth();
        let mut h = harness(app);
        h.run();
        h.get_by_role_and_label(Role::TextInput, "Name").click();
        h.run();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        h.key_press(egui::Key::Backspace);
        h.run();
        h.get_by_role_and_label(Role::TextInput, "Name").type_text("Towns");
        h.run();
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(h.state().session.map().layer(id).unwrap().name, "Towns");
        assert_eq!(h.state().session.undo_depth(), steps + 1, "one step");
        h.get_by_role_and_label(Role::TextInput, "Name").click();
        h.run();
        h.get_by_role_and_label(Role::TextInput, "Name").type_text(" and villages");
        h.run();
        h.key_press(egui::Key::Escape);
        h.run();
        assert_eq!(h.state().session.map().layer(id).unwrap().name, "Towns", "Escape keeps the name");
        assert_eq!(h.state().session.undo_depth(), steps + 1);
        // Escape lets go of the box, so no later click away can set what was escaped.
        assert!(!h.get_by_role_and_label(Role::TextInput, "Name").is_focused(), "Escape lets go of the box");
    }

    /// The box takes no more characters than `layer.rename` keeps, and says so once it is full, so
    /// what doesn't fit (a paste) isn't cut unsaid.
    #[test]
    fn the_name_box_takes_no_more_than_the_rename_keeps() {
        let app = with_cities();
        let id = app.session.active_layer.unwrap();
        let mut h = harness(app);
        h.run();
        h.get_by_role_and_label(Role::TextInput, "Name").click();
        h.run();
        assert!(h.query_by_label("Names keep up to 200 characters.").is_none(), "no note for a short name");
        // "Cities" and as many more: the box stops at the limit.
        h.get_by_role_and_label(Role::TextInput, "Name").type_text(&"x".repeat(super::NAME_LIMIT));
        h.run();
        let typed = h.get_by_role_and_label(Role::TextInput, "Name").value().unwrap_or_default();
        assert_eq!(typed.chars().count(), super::NAME_LIMIT);
        assert!(h.query_by_label("Names keep up to 200 characters.").is_some(), "the cut is said");
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(h.state().session.map().layer(id).unwrap().name, typed, "nothing cut");
    }

    /// A rename from elsewhere (a script, the assistant) while a name is being typed wins: the
    /// draft begun from the old name is dropped, and leaving the box doesn't undo the rename.
    #[test]
    fn a_rename_from_elsewhere_while_typing_is_kept() {
        let app = with_cities();
        let id = app.session.active_layer.unwrap();
        let mut h = harness(app);
        h.run();
        h.get_by_role_and_label(Role::TextInput, "Name").click();
        h.run();
        h.get_by_role_and_label(Role::TextInput, "Name").type_text(" draft");
        h.run();
        h.state_mut().session.run("layer.rename", &json!({"layer": id, "name": "Towns"})).unwrap();
        h.run();
        assert_eq!(h.get_by_role_and_label(Role::TextInput, "Name").value().as_deref(), Some("Towns"), "the box shows the new name");
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(h.state().session.map().layer(id).unwrap().name, "Towns");
    }

    /// A name being typed when another layer is chosen renames neither layer, and doesn't come
    /// back with the first; the header lights up for the new layer, and the tab names it.
    #[test]
    fn choosing_another_layer_drops_the_name_being_typed() {
        let app = with_cities();
        let cities = app.session.active_layer.unwrap();
        let countries = layer_id(&app, "Countries");
        let steps = app.session.undo_depth();
        let mut h = harness(app);
        h.run();
        h.get_by_role_and_label(Role::TextInput, "Name").click();
        h.run();
        h.get_by_role_and_label(Role::TextInput, "Name").type_text(" draft");
        h.run();
        let instance = h.state().session.map().layer(cities).unwrap().instance_id();
        assert!(h.ctx.data(|d| d.get_temp::<String>(super::name_key(instance))).is_some(), "typing keeps a draft");
        h.state_mut().session.run("layer.select", &json!({"layer": countries})).unwrap();
        // The header keeps repainting while it is lit, so these frames are stepped, not run.
        h.run_steps(2);
        let shown = h.ctx.data(|d| d.get_temp::<(u64, f64)>(egui::Id::unique("layer_properties_shown")));
        assert!(shown.is_some_and(|(_, since)| since.is_finite()), "the header is lit for the new layer");
        assert!(super::tab_name(h.state()).ends_with("Countries"), "{}", super::tab_name(h.state()));
        assert!(h.ctx.data(|d| d.get_temp::<String>(super::name_key(instance))).is_none(), "the draft is dropped");
        h.state_mut().session.run("layer.select", &json!({"layer": cities})).unwrap();
        h.run_steps(2);
        let m = h.state().session.map();
        assert_eq!((m.layer(cities).unwrap().name.as_str(), m.layer(countries).unwrap().name.as_str()), ("Cities", "Countries"));
        assert_eq!(h.state().session.undo_depth(), steps, "nothing renamed");
        assert!(h.ctx.data(|d| d.get_temp::<String>(super::name_key(instance))).is_none(), "nor does it come back");
    }

    /// A name typed and left by clicking another layer in the Layers pane renames the layer it was
    /// typed for, in one step, and the pane then shows the other: the press lets go of the box
    /// before the row picks its layer (`widgets::let_go_on_panel_press`), as in the whole app.
    #[test]
    fn clicking_another_layer_renames_the_layer_the_name_was_typed_for() {
        let mut app = RustGisApp::with_sample();
        app.run("ui.pane.layerProperties", json!({"layer": "Countries"})).unwrap();
        let countries = app.session.active_layer.unwrap();
        let cities = layer_id(&app, "Cities");
        let steps = app.session.undo_depth();
        let mut h =
            egui_kittest::Harness::builder().with_size(egui::vec2(1400.0, 900.0)).with_max_steps(64).with_step_dt(0.25).build_ui_state(
                |ui, app: &mut RustGisApp| {
                    let ctx = ui.ctx().clone();
                    app.logic(&ctx);
                    app.ui(ui);
                },
                app,
            );
        h.run_steps(6);
        h.get_by_role_and_label(Role::TextInput, "Name").click();
        h.run_steps(2);
        h.get_by_role_and_label(Role::TextInput, "Name").type_text(" 2");
        h.run_steps(2);
        let at = h.get_by_label("Cities").rect().center();
        h.hover_at(at);
        h.drag_at(at);
        h.step();
        h.drop_at(at);
        h.run_steps(3);
        let m = h.state().session.map();
        assert_eq!((m.layer(countries).unwrap().name.as_str(), m.layer(cities).unwrap().name.as_str()), ("Countries 2", "Cities"));
        assert_eq!(h.state().session.active_layer, Some(cities), "the click picked Cities");
        assert_eq!(h.state().session.undo_depth(), steps + 1, "one step");
    }

    /// A name being typed when the pane is hidden is dropped: back in view, the box shows the
    /// layer's name, and nothing was renamed.
    #[test]
    fn hiding_the_pane_drops_the_name_being_typed() {
        let app = with_cities();
        let id = app.session.active_layer.unwrap();
        let steps = app.session.undo_depth();
        let mut h = harness(app);
        h.run();
        h.get_by_role_and_label(Role::TextInput, "Name").click();
        h.run();
        h.get_by_role_and_label(Role::TextInput, "Name").type_text(" draft");
        h.run();
        let key = super::name_key(h.state().session.map().layer(id).unwrap().instance_id());
        assert!(h.ctx.data(|d| d.get_temp::<String>(key)).is_some(), "typing keeps a draft");
        h.state_mut().ui.right = Some(RightPane::Symbology);
        h.run();
        h.state_mut().ui.right = Some(RightPane::LayerProperties);
        h.run();
        assert!(h.ctx.data(|d| d.get_temp::<String>(key)).is_none(), "the draft is dropped");
        assert_eq!(h.get_by_role_and_label(Role::TextInput, "Name").value().as_deref(), Some("Cities"));
        assert_eq!(h.state().session.map().layer(id).unwrap().name, "Cities");
        assert_eq!(h.state().session.undo_depth(), steps, "nothing renamed");
    }

    /// The checkboxes run the layer's commands, each a step of its own.
    #[test]
    fn visible_and_selectable_run_their_commands() {
        let app = with_cities();
        let id = app.session.active_layer.unwrap();
        let mut h = harness(app);
        h.run();
        h.get_by_role_and_label(Role::CheckBox, "Visible").click();
        h.run();
        assert!(!h.state().session.map().layer(id).unwrap().visible);
        h.get_by_role_and_label(Role::CheckBox, "Selectable").click();
        h.run();
        assert!(!h.state().session.map().layer(id).unwrap().selectable);
        h.state_mut().session.run("edit.undo", &json!({})).unwrap();
        assert!(h.state().session.map().layer(id).unwrap().selectable);
        // The legend's fold is what the Layers pane shows, not an edit: no undo step
        // (`layer.expand` is a view command).
        let steps = h.state().session.undo_depth();
        h.get_by_role_and_label(Role::CheckBox, "Show legend in Layers").click();
        h.run();
        assert!(!h.state().session.map().layer(id).unwrap().expanded, "the legend is folded");
        assert_eq!(h.state().session.undo_depth(), steps, "no undo step");
    }
}
