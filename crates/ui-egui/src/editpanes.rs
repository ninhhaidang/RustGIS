//! The Editing pane (modify tools, templates, snapping, elevation, ground to grid, topology and
//! edit status) and the Data Design pane (domains, subtypes, attribute rules, contingent values,
//! archiving and relationships). Every action runs a command.

use egui::{Ui, vec2};
use rustgis_engine::map::design::{DomainKind, Elevation, RuleKind};
use serde_json::{Value, json};

use crate::RustGisApp;
use crate::theme::{Tokens, semibold};

pub(crate) const DISTANCE_UNITS: &[&str] = &["Meters", "Feet", "Kilometers", "Miles"];
pub(crate) const DISTANCE_UNITS_PREF: &str = "editing_distance_units";

/// Text typed into the panes' forms.
#[derive(Default)]
pub struct Forms {
    pub dx: String,
    pub dy: String,
    /// Remembered units for Move and constant elevation.
    pub distance_units: String,
    pub angle: String,
    pub factor: String,
    pub template: String,
    pub z: String,
    pub g2g_scale: String,
    pub g2g_rotation: String,
    pub rule_kind: String,
    pub rule_layer: String,
    pub rule_by: String,
    pub field: String,
    pub coded: String,
    pub min: String,
    pub max: String,
    /// Each form keeps its own text.
    pub template_name: String,
    pub rule_name: String,
    pub calc_field: String,
    pub subtype_field: String,
    pub subtype_list: String,
    pub contingent_name: String,
    pub contingent_combos: String,
    pub rel_name: String,
    pub expression: String,
    pub calc: bool,
    pub fields: String,
    pub rel_origin: String,
    pub rel_origin_key: String,
    pub rel_dest: String,
    pub rel_dest_key: String,
    pub restore_minutes: String,
    pub message: Option<(String, bool)>,
}

/// A number typed in a form, or an error shown in the pane (so a typo isn't taken as 0).
fn number(app: &mut RustGisApp, text: &str, what: &str) -> Option<f64> {
    match text.trim().parse::<f64>() {
        Ok(v) if v.is_finite() => Some(v),
        _ => {
            app.forms.message = Some((crate::i18n::fmt("{what} must be a number", &[("what", tl_raw!(what))]), true));
            None
        }
    }
}

fn heading(ui: &mut Ui, text: &str) {
    ui.add_space(6.0);
    ui.label(egui::RichText::new(tl!(text)).font(semibold(12.5)));
    ui.add_space(2.0);
}

/// Runs a command and shows its result (or error) at the top of the pane.
fn run(app: &mut RustGisApp, id: &str, p: Value) -> Option<Value> {
    match app.run(id, p) {
        Ok(v) => {
            app.forms.message = None;
            Some(v)
        }
        Err(e) => {
            app.forms.message = Some((e, true));
            None
        }
    }
}

fn message(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    if let Some((m, err)) = &app.forms.message {
        let color = if *err { egui::Color32::from_rgb(0xC0, 0x30, 0x30) } else { t.text_dim };
        ui.label(egui::RichText::new(m.as_str()).color(color).small());
    }
}

fn layer_names(app: &RustGisApp, pred: impl Fn(&rustgis_engine::map::Layer) -> bool) -> Vec<String> {
    app.session.map().layers.iter().filter(|l| pred(l)).map(|l| l.name.clone()).collect()
}

fn combo(ui: &mut Ui, id: &str, name: &str, value: &mut String, options: &[String]) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(value.as_str())
        .width(170.0)
        .show_ui(ui, |ui| {
            for o in options {
                ui.selectable_value(value, o.clone(), o.as_str());
            }
        })
        .response
        .on_hover_text(tl!(name));
}

pub fn editing(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    message(app, ui, t);
    let settings = app.session.map().editing.settings.clone();
    ui.horizontal(|ui| {
        let mut on = settings.enabled;
        if ui.checkbox(&mut on, tl!("Editing on")).changed() {
            run(app, "edit.enable", json!({"on": on}));
        }
        let mut v = settings.show_vertices;
        if ui.checkbox(&mut v, tl!("Show vertices")).changed() {
            run(app, "edit.settings", json!({"showVertices": v}));
        }
    });

    let mut units_changed = false;
    ui.horizontal(|ui| {
        let unit_label = ui.label(tl!("Distance units"));
        let _ = egui::ComboBox::from_id_salt(DISTANCE_UNITS_PREF)
            .selected_text(tl!(&app.forms.distance_units))
            .show_ui(ui, |ui| {
                for &unit in DISTANCE_UNITS {
                    units_changed |= ui.selectable_value(&mut app.forms.distance_units, unit.to_string(), tl!(unit)).changed();
                }
            })
            .response
            .labelled_by(unit_label.id);
    });
    if units_changed && let Some(save) = &app.services.save_pref {
        save(DISTANCE_UNITS_PREF, &app.forms.distance_units);
    }
    let units = app.forms.distance_units.clone();

    heading(ui, "Modify Selected Features");
    // Texts alone (#516): of these tools only Move applies to them (the others turn, scale or cut
    // shapes, and a text is a point); they are formatted in the Format pane.
    let only_texts = {
        let mut rows = app.session.map().layers.iter().flat_map(|l| l.selection.iter().map(move |r| (l, *r))).peekable();
        rows.peek().is_some() && rows.all(|(l, r)| l.graphics && rustgis_engine::map::graphics::kind(&l.dataset, r) == "text")
    };
    let not_for_texts = tl!("Not for texts: a text is edited in the Format pane.");
    egui::Grid::new("modify").num_columns(3).spacing(vec2(6.0, 6.0)).show(ui, |ui| {
        ui.label(tl!("Move"));
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut app.forms.dx).hint_text(tl!("east")).desired_width(52.0));
            ui.add(egui::TextEdit::singleline(&mut app.forms.dy).hint_text(tl!("north")).desired_width(52.0));
        });
        if ui.button(tl!("Move")).clicked() {
            let (tx, ty) = (app.forms.dx.clone(), app.forms.dy.clone());
            if let (Some(dx), Some(dy)) = (number(app, &tx, "East"), number(app, &ty, "North")) {
                run(app, "edit.move", json!({"dx": dx, "dy": dy, "units": units}));
            }
        }
        ui.end_row();
        let input_label = ui.label(tl!("Rotate (°)"));
        ui.add(egui::TextEdit::singleline(&mut app.forms.angle).hint_text("90").desired_width(124.0)).labelled_by(input_label.id);
        if ui.add_enabled(!only_texts, egui::Button::new(tl!("Rotate"))).on_disabled_hover_text(not_for_texts).clicked() {
            let t = app.forms.angle.clone();
            if let Some(a) = number(app, &t, "Angle") {
                run(app, "edit.rotate", json!({"angle": a}));
            }
        }
        ui.end_row();
        let input_label = ui.label(tl!("Scale (×)"));
        ui.add(egui::TextEdit::singleline(&mut app.forms.factor).hint_text("2").desired_width(124.0)).labelled_by(input_label.id);
        if ui.add_enabled(!only_texts, egui::Button::new(tl!("Scale"))).on_disabled_hover_text(not_for_texts).clicked() {
            let t = app.forms.factor.clone();
            if let Some(k) = number(app, &t, "Scale") {
                run(app, "edit.scale", json!({"factor": k}));
            }
        }
        ui.end_row();
    });
    ui.horizontal_wrapped(|ui| {
        for (label, id) in [("Merge", "edit.merge"), ("Explode", "edit.explode"), ("Flip Line", "edit.flip")] {
            if ui.add_enabled(!only_texts, egui::Button::new(tl!(label))).on_disabled_hover_text(not_for_texts).clicked() {
                run(app, id, Value::Null);
            }
        }
        let split = app.map.split_mode;
        // Toggles: one that is on stays enabled, so it can be turned off whatever is selected.
        let r = ui.add_enabled(!only_texts || split, egui::Button::selectable(split, tl!("Split (click the line)")));
        if r.on_disabled_hover_text(not_for_texts).clicked() {
            app.map.split_mode = !split;
        }
        for (tool, label, hint) in [
            (rustgis_engine::Tool::CutPolygon, "Cut Polygon", "Draw a line across the selected polygons; double-click to cut them"),
            (rustgis_engine::Tool::Reshape, "Reshape", "Draw a line across the selected feature's edge; double-click to reshape it"),
        ] {
            let on = app.session.tool == tool;
            let r = ui.add_enabled(!only_texts || on, egui::Button::selectable(on, tl!(label)));
            if r.on_hover_text(tl!(hint)).on_disabled_hover_text(not_for_texts).clicked() {
                app.map.cancel_sketch();
                run(app, "tool.set", json!({"tool": if on { "explore" } else { tool.code() }}));
            }
        }
        let on = app.map.trace_edges;
        if ui
            .add(egui::Button::selectable(on, tl!("Trace")))
            .on_hover_text(tl!("While drawing, click two points on a feature's edge to follow the edge between them"))
            .clicked()
        {
            run(app, "ui.traceEdges", Value::Null);
        }
        let on = app.session.tool == rustgis_engine::Tool::EditVertices;
        let r = ui.add_enabled(!only_texts || on, egui::Button::selectable(on, tl!("Edit Vertices")));
        if r.on_hover_text(tl!("Drag vertices on the map; click an edge to add one")).on_disabled_hover_text(not_for_texts).clicked() {
            app.map.cancel_sketch();
            run(app, "tool.set", json!({"tool": if on { "explore" } else { "editVertices" }}));
        }
    });

    heading(ui, "Feature Templates");
    let active = app.session.active_layer.and_then(|id| app.session.map().layer(id)).cloned();
    if let Some(l) = &active {
        for tpl in &l.design.templates {
            ui.horizontal(|ui| {
                let on = app.forms.template == tpl.name;
                if ui.selectable_label(on, tpl.name.as_str()).on_hover_text(tl!("New features use this template's values")).clicked() {
                    app.forms.template = if on { String::new() } else { tpl.name.clone() };
                }
                if ui.small_button("×").on_hover_text(tl!("Delete")).clicked() {
                    run(app, "edit.template", json!({"name": tpl.name, "remove": true}));
                }
            });
        }
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut app.forms.template_name).hint_text(tl!("Template name")).desired_width(150.0));
            if ui.button(tl!("From Selection")).on_hover_text(tl!("A template with the first selected feature's values")).clicked() {
                let defaults: serde_json::Map<String, Value> = l
                    .selection
                    .first()
                    .and_then(|r| l.dataset.features.get(*r))
                    .map(|f| {
                        l.dataset
                            .fields
                            .iter()
                            .zip(&f.attrs)
                            .filter(|(_, v)| !v.is_null())
                            .map(|(fd, v)| (fd.name.clone(), v.to_json()))
                            .collect()
                    })
                    .unwrap_or_default();
                let name = app.forms.template_name.trim().to_string();
                run(app, "edit.template", json!({"name": name, "defaults": defaults}));
            }
        });
    } else {
        ui.label(egui::RichText::new(tl!("Select a layer in the Layers pane")).weak().small());
    }

    heading(ui, "Snapping");
    let sn = app.session.map().editing.snapping;
    ui.horizontal_wrapped(|ui| {
        let mut on = sn.enabled;
        if ui.checkbox(&mut on, tl!("On")).changed() {
            run(app, "edit.snapping", json!({"on": on}));
        }
        for (label, key, v) in [("Vertex", "vertex", sn.vertex), ("Edge", "edge", sn.edge), ("End", "end", sn.end)] {
            let mut x = v;
            if ui.checkbox(&mut x, tl!(label)).changed() {
                run(app, "edit.snapping", json!({"on": sn.enabled, key: x}));
            }
        }
        let mut tol = sn.tolerance_px;
        let resp = ui.add(egui::DragValue::new(&mut tol).update_while_editing(false).range(1.0..=100.0).suffix(" px"));
        if crate::widgets::changed(&resp) {
            crate::widgets::one_step_per_press(app, &resp);
            run(app, "edit.snapping", json!({"on": sn.enabled, "tolerancePx": tol}));
        }
    });

    heading(ui, "Elevation");
    let rasters = layer_names(app, |l| l.is_raster());
    ui.horizontal_wrapped(|ui| {
        let mode = match &settings.elevation {
            Elevation::None => "none",
            Elevation::Constant(_) => "constant",
            Elevation::Surface(_) => "surface",
        };
        if ui.radio(mode == "none", tl!("None")).clicked() {
            run(app, "edit.elevation", json!({"mode": "none"}));
        }
        let constant = ui.radio(mode == "constant", tl!("Constant")).clicked();
        ui.add(egui::TextEdit::singleline(&mut app.forms.z).hint_text(tl!("z")).desired_width(60.0));
        let apply = ui.button(tl!("Apply elevation")).clicked();
        if constant || apply {
            let t = app.forms.z.clone();
            if let Some(z) = number(app, &t, "Elevation") {
                run(app, "edit.elevation", json!({"mode": "constant", "z": z, "units": units}));
            }
        }
        for r in &rasters {
            let on = matches!(&settings.elevation, Elevation::Surface(n) if n == r);
            if ui.radio(on, crate::i18n::fmt("Surface: {layer}", &[("layer", r)])).clicked() {
                run(app, "edit.elevation", json!({"mode": "surface", "surface": r}));
            }
        }
    });

    ui.weak(tl!("The current elevation stays in effect while you edit inputs. Click Apply elevation to apply the value and units."));

    heading(ui, "Ground To Grid");
    ui.horizontal(|ui| {
        let on = settings.ground_to_grid.is_some();
        ui.add(egui::TextEdit::singleline(&mut app.forms.g2g_scale).hint_text(tl!("scale 0.9996")).desired_width(80.0));
        ui.add(egui::TextEdit::singleline(&mut app.forms.g2g_rotation).hint_text(tl!("rotation °")).desired_width(70.0));
        let mut x = on;
        if ui.checkbox(&mut x, tl!("On")).changed() {
            // Turning it off needs no numbers; blank fields mean no correction.
            let (ts, tr) = (app.forms.g2g_scale.clone(), app.forms.g2g_rotation.clone());
            let k = if ts.trim().is_empty() || !x { Some(1.0) } else { number(app, &ts, "Scale") };
            let r = if tr.trim().is_empty() || !x { Some(0.0) } else { number(app, &tr, "Rotation") };
            if let (Some(k), Some(r)) = (k, r) {
                run(app, "edit.groundToGrid", json!({"on": x, "scale": k, "rotation": r}));
            }
        }
    });

    heading(ui, "Topology");
    let rules = app.session.map().editing.topology.clone();
    for (i, r) in rules.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(format!("{} · {}", tl!(r.label()), r.layer()));
            if ui.small_button("×").clicked() {
                run(app, "topology.rules", json!({"remove": i}));
            }
        });
    }
    let feature_layers = layer_names(app, |l| !l.is_raster() && !l.graphics);
    ui.horizontal_wrapped(|ui| {
        let kinds: Vec<String> = ["mustNotOverlap", "mustNotHaveGaps", "mustNotSelfIntersect", "mustNotHaveDangles", "mustBeCoveredBy"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        if app.forms.rule_kind.is_empty() {
            app.forms.rule_kind = "mustNotOverlap".into();
        }
        combo(ui, "topo_kind", "Rule", &mut app.forms.rule_kind, &kinds);
        combo(ui, "topo_layer", "Layer", &mut app.forms.rule_layer, &feature_layers);
        if app.forms.rule_kind == "mustBeCoveredBy" {
            combo(ui, "topo_by", "By layer", &mut app.forms.rule_by, &feature_layers);
        }
        if ui.button(tl!("Add Rule")).clicked() {
            let add = json!({"rule": app.forms.rule_kind, "layer": app.forms.rule_layer, "by": app.forms.rule_by});
            run(app, "topology.rules", json!({"add": add}));
        }
    });
    if ui.button(tl!("Validate")).clicked() {
        run(app, "topology.validate", Value::Null);
    }
    let errors = app.session.topology_errors.clone();
    if !errors.is_empty() {
        ui.label(egui::RichText::new(crate::i18n::fmt("{n} errors", &[("n", &errors.len().to_string())])).small());
        egui::ScrollArea::vertical().id_salt("topo_errors").max_height(160.0).show(ui, |ui| {
            for e in errors.iter().take(300) {
                let rows: Vec<String> = e.rows.iter().map(|r| r.to_string()).collect();
                let text = format!(
                    "{} · {} {}",
                    tl!(&e.rule),
                    e.layer,
                    if rows.is_empty() { String::new() } else { format!("#{}", rows.join(", #")) }
                );
                if ui.link(text).on_hover_text(tl!("Zoom to the error")).clicked() {
                    let _ = app.run("map.goToXY", json!({"lon": e.lon, "lat": e.lat}));
                    if let Some(l) = app.session.map().layer_by_name(&e.layer).map(|l| l.id)
                        && !e.rows.is_empty()
                    {
                        let _ = app.run("select.rows", json!({"layer": l, "rows": e.rows}));
                    }
                }
            }
        });
    }

    heading(ui, "Edit Status");
    // Read directly: running a command every frame would mark the map changed each time.
    let counts: Vec<(String, rustgis_engine::cmd::design::EditCounts)> =
        app.session.edit_counts.iter().filter_map(|(id, c)| app.session.map().layer(*id).map(|l| (l.name.clone(), *c))).collect();
    if counts.is_empty() {
        ui.label(egui::RichText::new(tl!("No edits since the project was saved")).weak().small());
    }
    for (layer, c) in counts {
        let text = crate::i18n::fmt(
            "{layer}: {added} added, {modified} changed, {deleted} deleted",
            &[
                ("layer", &layer),
                ("added", &c.added.to_string()),
                ("modified", &c.modified.to_string()),
                ("deleted", &c.deleted.to_string()),
            ],
        );
        ui.label(egui::RichText::new(text).small());
    }
}

pub fn design(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    message(app, ui, t);
    let Some(l) = app.session.active_layer.and_then(|id| app.session.map().layer(id)).filter(|l| !l.is_raster()).cloned() else {
        ui.label(egui::RichText::new(tl!("Select a feature layer in the Layers pane")).weak());
        return;
    };
    ui.label(egui::RichText::new(&l.name).font(semibold(13.0)));
    let fields: Vec<String> = l.dataset.fields.iter().map(|f| f.name.clone()).collect();

    heading(ui, "Domains");
    for d in &l.design.domains {
        let desc = match &d.kind {
            DomainKind::Coded(c) => c.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(", "),
            DomainKind::Range { min, max } => format!("{min} – {max}"),
        };
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(format!("{}: {desc}", d.field)).small());
            if ui.small_button("×").clicked() {
                run(app, "data.domain", json!({"field": d.field, "clear": true}));
            }
        });
    }
    ui.horizontal(|ui| {
        combo(ui, "dom_field", "Field", &mut app.forms.field, &fields);
        ui.add(egui::TextEdit::singleline(&mut app.forms.min).hint_text("min").desired_width(46.0));
        ui.add(egui::TextEdit::singleline(&mut app.forms.max).hint_text("max").desired_width(46.0));
    });
    ui.add(
        egui::TextEdit::multiline(&mut app.forms.coded)
            .hint_text(tl!("Coded values, one per line: code=description"))
            .desired_rows(2)
            .desired_width(f32::INFINITY),
    );
    if ui.button(tl!("Set Domain")).clicked() {
        let field = app.forms.field.clone();
        let p = if app.forms.coded.trim().is_empty() {
            json!({"field": field, "min": app.forms.min.trim().parse::<f64>().ok(), "max": app.forms.max.trim().parse::<f64>().ok()})
        } else {
            let coded: Vec<Value> = app
                .forms
                .coded
                .lines()
                .filter(|x| !x.trim().is_empty())
                .map(|x| {
                    let (c, d) = x.split_once('=').unwrap_or((x, x));
                    json!([c.trim(), d.trim()])
                })
                .collect();
            json!({"field": field, "coded": coded})
        };
        run(app, "data.domain", p);
    }

    heading(ui, "Subtypes");
    if let Some(st) = &l.design.subtypes {
        let list: Vec<String> = st.subtypes.iter().map(|x| format!("{}={}", x.code, x.name)).collect();
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(format!("{}: {}", st.field, list.join(", "))).small());
            if ui.small_button("×").clicked() {
                run(app, "data.subtypes", json!({"clear": true}));
            }
        });
    }
    let ints: Vec<String> =
        l.dataset.fields.iter().filter(|f| f.ty == rustgis_engine::data::FieldType::Integer).map(|f| f.name.clone()).collect();
    ui.horizontal(|ui| {
        combo(ui, "sub_field", "Field", &mut app.forms.subtype_field, &ints);
        if ui.button(tl!("Set Subtypes")).on_hover_text(tl!("One per line: code=name")).clicked() {
            let subs: Vec<Value> = app
                .forms
                .subtype_list
                .lines()
                .filter_map(|x| {
                    let (c, n) = x.split_once('=')?;
                    Some(json!({"code": c.trim().parse::<i64>().ok()?, "name": n.trim()}))
                })
                .collect();
            run(app, "data.subtypes", json!({"field": app.forms.subtype_field, "subtypes": subs}));
        }
    });
    ui.add(
        egui::TextEdit::multiline(&mut app.forms.subtype_list)
            .hint_text(tl!("Subtypes, one per line: code=name"))
            .desired_rows(2)
            .desired_width(f32::INFINITY),
    );

    heading(ui, "Attribute Rules");
    for r in &l.design.rules {
        let kind =
            if r.kind == RuleKind::Calculation { format!("{} = {}", r.field, r.expression) } else { format!("✓ {}", r.expression) };
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(format!("{}: {kind}", r.name)).small());
            if ui.small_button("×").clicked() {
                run(app, "data.attributeRule", json!({"name": r.name, "remove": true}));
            }
        });
    }
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut app.forms.rule_name).hint_text(tl!("Rule name")).desired_width(110.0));
        ui.checkbox(&mut app.forms.calc, tl!("Calculation"));
        if app.forms.calc {
            combo(ui, "rule_field", "Field", &mut app.forms.calc_field, &fields);
        }
    });
    ui.horizontal(|ui| {
        let hint = if app.forms.calc { tl!("field * 2, 'text', a + b") } else { tl!("SQL that must be true, e.g. pop > 0") };
        ui.add(egui::TextEdit::singleline(&mut app.forms.expression).hint_text(hint).desired_width(190.0));
        if ui.button(tl!("Add")).clicked() {
            let kind = if app.forms.calc { "calculation" } else { "constraint" };
            run(
                app,
                "data.attributeRule",
                json!({"name": app.forms.rule_name, "kind": kind, "field": app.forms.calc_field, "expression": app.forms.expression}),
            );
        }
    });

    heading(ui, "Contingent Values");
    for c in &l.design.contingent {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(format!("{}: {} ({} combinations)", c.name, c.fields.join(", "), c.combos.len())).small());
            if ui.small_button("×").clicked() {
                run(app, "data.contingentValues", json!({"name": c.name, "remove": true}));
            }
        });
    }
    ui.add(
        egui::TextEdit::multiline(&mut app.forms.contingent_combos)
            .hint_text(tl!("Valid combinations, one per line: a, b"))
            .desired_rows(2)
            .desired_width(f32::INFINITY),
    );
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut app.forms.contingent_name).hint_text(tl!("Name")).desired_width(80.0));
        ui.add(egui::TextEdit::singleline(&mut app.forms.fields).hint_text(tl!("fields, comma separated")).desired_width(150.0));
        if ui.button(tl!("Set")).on_hover_text(tl!("Combinations from the box above, one per line")).clicked() {
            let fs: Vec<&str> = app.forms.fields.split(',').map(str::trim).filter(|x| !x.is_empty()).collect();
            let combos: Vec<Vec<&str>> = app
                .forms
                .contingent_combos
                .lines()
                .filter(|x| !x.trim().is_empty())
                .map(|x| x.split(',').map(str::trim).collect())
                .collect();
            let name =
                if app.forms.contingent_name.trim().is_empty() { fs.join(" by ") } else { app.forms.contingent_name.trim().to_string() };
            run(app, "data.contingentValues", json!({"name": name, "fields": fs, "combos": combos}));
        }
    });

    heading(ui, "Archiving");
    let archived = l.design.archive.as_ref().map(Vec::len);
    ui.horizontal(|ui| {
        let mut on = archived.is_some();
        if ui.checkbox(&mut on, tl!("Keep an archive of edits")).changed() {
            run(app, "data.archive", json!({"on": on}));
        }
        if let Some(n) = archived {
            ui.label(egui::RichText::new(crate::i18n::fmt("{n} edits", &[("n", &n.to_string())])).small().weak());
        }
    });
    if archived.is_some() {
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut app.forms.restore_minutes).hint_text(tl!("minutes ago")).desired_width(80.0));
            if ui.button(tl!("Restore")).on_hover_text(tl!("Put the layer back as it was then")).clicked() {
                let t = app.forms.restore_minutes.clone();
                if let Some(mins) = number(app, &t, "Minutes") {
                    let now = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
                    run(app, "data.archiveRestore", json!({"time": now - mins.max(0.0) * 60.0}));
                }
            }
        });
    }

    heading(ui, "Relationships");
    let rels = app.session.map().editing.relationships.clone();
    for r in &rels {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("{}: {}.{} → {}.{}", r.name, r.origin, r.origin_key, r.destination, r.destination_key)).small(),
            );
            if ui.small_button(tl!("Select Related")).clicked() {
                run(app, "data.related", json!({"name": r.name}));
            }
            if ui.small_button("×").clicked() {
                run(app, "data.relate", json!({"name": r.name, "remove": true}));
            }
        });
    }
    let layers = layer_names(app, |l| !l.is_raster() && !l.graphics);
    let fields_of = |app: &RustGisApp, n: &str| -> Vec<String> {
        app.session.map().layer_by_name(n).map(|l| l.dataset.fields.iter().map(|f| f.name.clone()).collect()).unwrap_or_default()
    };
    let (of, df) = (fields_of(app, &app.forms.rel_origin), fields_of(app, &app.forms.rel_dest));
    egui::Grid::new("rel").num_columns(2).spacing(vec2(6.0, 4.0)).show(ui, |ui| {
        combo(ui, "rel_o", "Origin", &mut app.forms.rel_origin, &layers);
        combo(ui, "rel_ok", "Origin key", &mut app.forms.rel_origin_key, &of);
        ui.end_row();
        combo(ui, "rel_d", "Destination", &mut app.forms.rel_dest, &layers);
        combo(ui, "rel_dk", "Destination key", &mut app.forms.rel_dest_key, &df);
        ui.end_row();
    });
    ui.add(egui::TextEdit::singleline(&mut app.forms.rel_name).hint_text(tl!("Relationship name")).desired_width(180.0));
    if ui.button(tl!("Relate")).clicked() {
        let name = if app.forms.rel_name.trim().is_empty() {
            format!("{} to {}", app.forms.rel_origin, app.forms.rel_dest)
        } else {
            app.forms.rel_name.trim().to_string()
        };
        run(
            app,
            "data.relate",
            json!({"name": name, "origin": app.forms.rel_origin, "originKey": app.forms.rel_origin_key, "destination": app.forms.rel_dest, "destinationKey": app.forms.rel_dest_key}),
        );
    }
}
