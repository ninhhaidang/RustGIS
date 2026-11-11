//! A plugin command's dialog (`commands[].form` in `plugin.json`): the command's button, Command
//! Search entry or shortcut opens a window with its fields; **Run** runs the command with their values as parameters and shows
//! what it answers (its `report` text) under them, so each step of a plugin has its own
//! parameters and result instead of sharing one panel.
//!
//! **Run** asks for the command as a ribbon button does ([`crate::pluginstart`]): it starts once
//! the window has shown it running, clicks held meanwhile are dropped, and a command waiting for
//! the web answers the dialog when it runs again.

use crate::RustGisApp;
use egui::{Align2, RichText, Ui};
use rustgis_engine::analysis::ParamKind;
use rustgis_engine::plugins::ParamDecl;
use serde_json::{Map, Value, json};
use std::collections::HashMap;

#[derive(Default)]
pub struct State {
    open: Option<Dialog>,
    /// The values each dialog had when last closed: opened again, it starts from them.
    kept: HashMap<(String, String), Map<String, Value>>,
}

struct Dialog {
    plugin: String,
    command: String,
    values: Map<String, Value>,
    /// Number fields as typed: empty leaves the parameter out, and one that isn't a number
    /// holds Run.
    texts: HashMap<String, String>,
    /// The parameters of the run asked for and not answered yet.
    asked: Option<Value>,
    /// The last run's report, or its error.
    result: Option<Result<String, String>>,
}

/// `ui.pluginForm`: `{"plugin": id, "command": id}` opens that plugin command's dialog.
pub fn open(app: &mut RustGisApp, p: &Value) -> Result<Value, String> {
    let plugin = p.get("plugin").and_then(Value::as_str).ok_or("plugin is required")?;
    let command = p.get("command").and_then(Value::as_str).ok_or("command is required")?;
    let loaded = rustgis_engine::plugins::get(plugin).filter(|p| p.enabled).ok_or_else(|| format!("no plugin '{plugin}'"))?;
    let c = loaded.manifest.commands.iter().find(|c| c.id == command).ok_or_else(|| format!("{plugin} has no command '{command}'"))?;
    if c.form.is_empty() {
        return Err(format!("{plugin}/{command} has no dialog"));
    }
    let key = (plugin.to_string(), command.to_string());
    let state = &mut app.ui.plugin_dialog;
    // The dialog it replaces keeps its values too.
    if let Some(d) = state.open.take() {
        let form = rustgis_engine::plugins::get(&d.plugin)
            .and_then(|p| p.manifest.commands.iter().find(|c| c.id == d.command).map(|c| c.form.clone()))
            .unwrap_or_default();
        state.kept.insert((d.plugin, d.command), keep(&d.values, &form));
    }
    // Defaults the field can show: one of another kind would be sent unseen (the manifest
    // refuses those already; this is a backstop).
    let defaults: Map<String, Value> =
        c.form.iter().filter_map(|f| f.default.clone().filter(|d| fits(f, d)).map(|d| (f.name.clone(), d))).collect();
    // Kept values where they still fit the form, which a newer version of the plugin may have
    // changed: a field left empty stays empty, a field gone is dropped, and one of another kind,
    // or added since, starts at its default.
    let values = match state.kept.get(&key) {
        Some(kept) => c
            .form
            .iter()
            .filter_map(|f| match kept.get(&f.name) {
                Some(Value::Null) => None,
                Some(v) if fits(f, v) => Some((f.name.clone(), v.clone())),
                _ => defaults.get(&f.name).map(|d| (f.name.clone(), d.clone())),
            })
            .collect(),
        None => defaults,
    };
    state.open = Some(Dialog { plugin: key.0, command: key.1, values, texts: HashMap::new(), asked: None, result: None });
    Ok(Value::Null)
}

/// Whether the open dialog asked for plugin command `id` with `params` and waits for its answer.
pub(crate) fn asked(app: &RustGisApp, id: &str, params: &Value) -> bool {
    app.ui
        .plugin_dialog
        .open
        .as_ref()
        .is_some_and(|d| d.asked.as_ref() == Some(params) && rustgis_engine::plugins::command_id(&d.plugin, &d.command) == id)
}

/// Plugin command `id` has run: the dialog waiting for it shows what it answered, unless its own
/// run still waits to start, or now waits for the web, to run again once that has come in.
pub(crate) fn answered(app: &mut RustGisApp, id: &str, answer: &Result<Value, String>) {
    let Some(d) = app.ui.plugin_dialog.open.as_ref() else { return };
    let Some(params) = d.asked.as_ref() else { return };
    // Not while its own run waits to start or for the web: this answer is then another run's.
    if rustgis_engine::plugins::command_id(&d.plugin, &d.command) != id || crate::pluginstart::waits(app, id, params) {
        return;
    }
    let Some(d) = app.ui.plugin_dialog.open.as_mut() else { return };
    d.asked = None;
    d.result = Some(answer.as_ref().map(|a| shown(report(a))).map_err(|e| error_shown(e)));
}

/// A dialog's values as kept when it closes: with `null` for each field left empty, which tells
/// it apart from a field a newer version of the plugin adds (that one starts at its default).
fn keep(values: &Map<String, Value>, form: &[ParamDecl]) -> Map<String, Value> {
    let mut kept = values.clone();
    for f in form {
        kept.entry(f.name.clone()).or_insert(Value::Null);
    }
    kept
}

/// A command's error as the dialog shows it: redacted as its notice is (an address's password or
/// token left out), and cut like an answer.
fn error_shown(e: &str) -> String {
    shown(rustgis_engine::Session::redacted(e).into_string())
}

/// The most of an answer the dialog shows, in bytes: a longer one would hold the window (in a
/// test, a frame took 100 ms with 1 MB shown and 1.9 s with 16 MB; 11 ms with this much).
const SHOWN: usize = 64 * 1024;

/// `text` as the dialog shows it: its first [`SHOWN`] bytes, and how much more there is.
fn shown(mut text: String) -> String {
    if text.len() <= SHOWN {
        return text;
    }
    let mut end = SHOWN;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let more = text[end..].chars().count();
    text.truncate(end);
    text.push('\n');
    text.push_str(&crate::i18n::fmt("… {n} more characters not shown", &[("n", &more.to_string())]));
    text
}

/// Why **Run** waits: the required fields to fill in, and the numbers typed that aren't numbers.
/// Those are named as such, not as fields to fill in, though they hold no value either.
fn held<'a>(
    form: &'a [ParamDecl],
    values: &Map<String, Value>,
    texts: &HashMap<String, String>,
) -> (Vec<&'a ParamDecl>, Vec<&'a ParamDecl>) {
    let bad: Vec<&ParamDecl> = form
        .iter()
        .filter(|f| f.kind == "number" && texts.get(&f.name).is_some_and(|t| !t.trim().is_empty() && number(t).is_none()))
        .collect();
    let missing = form.iter().filter(|f| unfilled(f, values) && !bad.iter().any(|b| b.name == f.name)).collect();
    (missing, bad)
}

/// Whether required field `f` has no value **Run** would send: none, empty text, or one of
/// another kind, which [`form_params`] leaves out (a plugin replaced while its dialog is open
/// may have changed the field's kind).
fn unfilled(f: &ParamDecl, values: &Map<String, Value>) -> bool {
    f.required && !values.get(&f.name).is_some_and(|v| fits(f, v) && v.as_str() != Some(""))
}

/// The parameters **Run** sends: the values of the form as it is now. The plugin may have been
/// replaced since they were set, dropping a field or changing its kind.
fn form_params(form: &[ParamDecl], values: &Map<String, Value>) -> Value {
    Value::Object(form.iter().filter_map(|f| values.get(&f.name).filter(|v| fits(f, v)).map(|v| (f.name.clone(), v.clone()))).collect())
}

/// Whether `v` is a value field `f` gives: a number, a checkbox's bool, or a name or text.
fn fits(f: &ParamDecl, v: &Value) -> bool {
    match f.kind.as_str() {
        "number" => rustgis_engine::plugins::form_number_value(v).is_some(),
        "bool" => v.is_boolean(),
        _ => v.is_string(),
    }
}

/// What a command answered, as the dialog shows it: its `report` text, or the answer itself
/// (without the panel state it may carry).
fn report(answer: &Value) -> String {
    if let Some(t) = answer.get("report").and_then(Value::as_str) {
        return t.to_string();
    }
    match answer {
        Value::Null => tl!("Done.").to_string(),
        Value::Object(o) => {
            let rest: Map<String, Value> = o.iter().filter(|(k, _)| *k != "panel").map(|(k, v)| (k.clone(), v.clone())).collect();
            serde_json::to_string_pretty(&rest).unwrap_or_default()
        }
        other => other.to_string(),
    }
}

pub fn show(app: &mut RustGisApp, ctx: &egui::Context) {
    let Some(mut d) = app.ui.plugin_dialog.open.take() else { return };
    // A plugin turned off or removed takes its dialog with it.
    let Some(loaded) = rustgis_engine::plugins::get(&d.plugin).filter(|p| p.enabled) else { return };
    let m = &loaded.manifest;
    let Some(c) = m.commands.iter().find(|c| c.id == d.command) else { return };
    let lang = app.lang().code().to_string();
    let id = rustgis_engine::plugins::command_id(&d.plugin, &d.command);
    // A run that ended without an answer: another project opened before it started, or its
    // downloads never came. The notices say which.
    if let Some(params) = &d.asked
        && !crate::pluginstart::waits(app, &id, params)
    {
        d.asked = None;
        d.result = Some(Err(tl!("It stopped without an answer: see the notices.").to_string()));
    }
    // Fields to fill in before it can run.
    let (missing, bad) = held(&c.form, &d.values, &d.texts);
    let missing: Vec<&str> = missing.iter().map(|f| m.translate(&lang, &f.label)).collect();
    let bad: Vec<&str> = bad.iter().map(|f| m.translate(&lang, &f.label)).collect();
    let mut run = false;

    let mut open = true;
    let title = format!("{} › {}", m.translate(&lang, &m.name), m.translate(&lang, &c.label).replace('\n', " "));
    egui::Window::new(title)
        .id(egui::Id::unique(("plugin_form", &d.plugin, &d.command)))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_width(460.0)
        .pivot(Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .show(ctx, |ui| {
            if !c.help.is_empty() {
                ui.label(RichText::new(m.translate(&lang, &c.help)).weak());
                ui.separator();
            }
            egui::Grid::new(("plugin_form_grid", &d.plugin, &d.command)).num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                // A field lists the fields of the layer chosen above it.
                let mut layer: Option<String> = None;
                for f in &c.form {
                    let text = m.translate(&lang, &f.label);
                    let mut label = ui.label(if f.required { format!("{text} *") } else { text.to_string() });
                    if !f.help.is_empty() {
                        label = label.on_hover_text(m.translate(&lang, &f.help));
                    }
                    field(app, ui, f, &mut d.values, &mut d.texts, layer.as_deref(), m, &lang).labelled_by(label.id);
                    if layer_kind(&f.kind).is_some() {
                        layer = d.values.get(&f.name).and_then(Value::as_str).map(str::to_string);
                    }
                    ui.end_row();
                }
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let running = d.asked.is_some();
                let ready = missing.is_empty() && bad.is_empty();
                let button = ui.add_enabled(!running && ready, egui::Button::new(RichText::new(tl!("Run")).strong()));
                // Each reason it waits, on its own line.
                let mut why = Vec::new();
                if !missing.is_empty() {
                    why.push(crate::i18n::fmt("Fill in: {fields}", &[("fields", &missing.join(", "))]));
                }
                if !bad.is_empty() {
                    why.push(crate::i18n::fmt("Not a number: {fields}", &[("fields", &bad.join(", "))]));
                }
                let button = if why.is_empty() { button } else { button.on_disabled_hover_text(why.join("\n")) };
                if button.clicked() {
                    run = true;
                }
                if running {
                    ui.spinner();
                    ui.label(tl!("Running…"));
                }
            });
            if let Some(r) = &d.result {
                ui.separator();
                ui.label(RichText::new(tl!("Result")).strong());
                egui::ScrollArea::vertical().min_scrolled_height(260.0).max_height(420.0).auto_shrink([false, true]).show(
                    ui,
                    |ui| match r {
                        Ok(t) => {
                            ui.add(egui::Label::new(RichText::new(t).monospace()).selectable(true));
                        }
                        Err(e) => {
                            ui.colored_label(ui.visuals().error_fg_color, e);
                        }
                    },
                );
            }
        });
    if run {
        let params = form_params(&c.form, &d.values);
        d.result = None;
        // Asked as a ribbon button would, but not through `run_by_user`, where no parameters
        // would open this dialog again: errors before it starts are notices, and the answer.
        match app.ask_plugin(&id, params.clone()) {
            Ok(()) => d.asked = Some(params),
            Err(e) => d.result = Some(Err(error_shown(&e))),
        }
    }
    if open {
        app.ui.plugin_dialog.open = Some(d);
    } else {
        app.ui.plugin_dialog.kept.insert((d.plugin, d.command), keep(&d.values, &c.form));
    }
}

/// The layer kind a field kind picks, if it picks a layer.
fn layer_kind(kind: &str) -> Option<ParamKind> {
    Some(match kind {
        "layer" => ParamKind::Layer,
        "vectorLayer" => ParamKind::VectorLayer,
        "polygonLayer" => ParamKind::PolygonLayer,
        "pointLayer" => ParamKind::PointLayer,
        "rasterLayer" => ParamKind::RasterLayer,
        _ => return None,
    })
}

/// A drop-down of `(value, label)` options for `values[key]`. A required one never stays unset:
/// an unset or vanished value becomes `start`, or the first option. An optional one offers `—`,
/// which leaves it out, and starts there.
fn combo(
    ui: &mut Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    values: &mut Map<String, Value>,
    key: &str,
    options: &[(String, String)],
    required: bool,
    start: Option<&str>,
) -> egui::Response {
    let cur = values.get(key).and_then(Value::as_str).map(str::to_string);
    let known = cur.as_ref().is_some_and(|c| options.iter().any(|o| o.0 == *c));
    if !known {
        let first = start.filter(|s| options.iter().any(|o| o.0 == *s)).or(options.first().map(|o| o.0.as_str()));
        match first {
            Some(v) if required => {
                values.insert(key.to_string(), json!(v));
            }
            _ => {
                values.remove(key);
            }
        }
    }
    let cur = values.get(key).and_then(Value::as_str).map(str::to_string);
    let shown = cur.as_ref().and_then(|c| options.iter().find(|o| o.0 == *c)).map_or("—", |o| o.1.as_str());
    egui::ComboBox::from_id_salt(id)
        .selected_text(shown)
        .width(280.0)
        .show_ui(ui, |ui| {
            if !required && ui.selectable_label(cur.is_none(), "—").clicked() {
                values.remove(key);
            }
            for (v, label) in options {
                if ui.selectable_label(cur.as_deref() == Some(v), label).clicked() {
                    values.insert(key.to_string(), json!(v));
                }
            }
        })
        .response
}

/// The number typed in `text`, read as the manifest reads a number default
/// ([`rustgis_engine::plugins::form_number`]).
fn number(text: &str) -> Option<Value> {
    rustgis_engine::plugins::form_number(text)
}

fn field(
    app: &RustGisApp,
    ui: &mut Ui,
    f: &ParamDecl,
    values: &mut Map<String, Value>,
    texts: &mut HashMap<String, String>,
    layer: Option<&str>,
    m: &rustgis_engine::plugins::Manifest,
    lang: &str,
) -> egui::Response {
    let id = ("plugin_form_field", f.name.as_str());
    if let Some(kind) = layer_kind(&f.kind) {
        let options: Vec<(String, String)> = app
            .session
            .map()
            .layers
            .iter()
            .filter(|l| crate::panes::takes_layer(kind, l))
            .map(|l| (l.name.clone(), l.name.clone()))
            .collect();
        // A required layer starts at the active one, when it is of the kind.
        let active = app.session.active_layer.and_then(|a| app.session.map().layers.iter().find(|l| l.id == a)).map(|l| l.name.as_str());
        return combo(ui, id, values, &f.name, &options, f.required, active);
    }
    match f.kind.as_str() {
        "field" | "numericField" => {
            let kind = if f.kind == "field" { ParamKind::Field } else { ParamKind::NumericField };
            let options: Vec<(String, String)> = layer
                .and_then(|n| crate::panes::input_dataset(&app.session, n))
                .map(|d| {
                    (0..d.fields.len())
                        .filter(|i| crate::panes::takes_field(kind, d, *i))
                        .filter_map(|i| d.fields.get(i).map(|f| (f.name.clone(), f.name.clone())))
                        .collect()
                })
                .unwrap_or_default();
            // The field named like the default, when the layer has one.
            let named = f.default.as_ref().and_then(Value::as_str);
            combo(ui, id, values, &f.name, &options, f.required, named)
        }
        "number" => {
            // Typed, so that an optional one can be left empty, and out.
            let text = texts.entry(f.name.clone()).or_insert_with(|| match values.get(&f.name) {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Null) | None => String::new(),
                Some(v) => v.to_string(),
            });
            let r = ui.add(egui::TextEdit::singleline(text).desired_width(120.0));
            match number(text) {
                Some(v) => {
                    values.insert(f.name.clone(), v);
                }
                None => {
                    values.remove(&f.name);
                }
            }
            r
        }
        "choice" => {
            let options: Vec<(String, String)> = f.choices.iter().map(|c| (c.clone(), m.translate(lang, c).to_string())).collect();
            combo(ui, id, values, &f.name, &options, f.required || f.default.is_some(), None)
        }
        "bool" => {
            // Sent once ticked or given a default: an optional one never set is left out, and a
            // required one always has a value, unticked until it is ticked.
            let mut on = values.get(&f.name).and_then(Value::as_bool).unwrap_or(false);
            let r = ui.checkbox(&mut on, "");
            if r.changed() || (f.required && !values.contains_key(&f.name)) {
                values.insert(f.name.clone(), json!(on));
            }
            r
        }
        // `text`.
        _ => {
            let mut s = values.get(&f.name).and_then(Value::as_str).unwrap_or_default().to_string();
            let r = ui.add(egui::TextEdit::singleline(&mut s).desired_width(280.0));
            if r.changed() {
                values.insert(f.name.clone(), json!(s));
            }
            r
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::vec2;
    use egui_kittest::kittest::Queryable;
    use rustgis_engine::plugins;

    /// The test plugin, with dialogs on its echo command `q` and its failing command `e`.
    fn launch(plugin: &str) -> egui_kittest::Harness<'static, RustGisApp> {
        let mut m: Value = serde_json::from_str(&plugins::testing::manifest(plugin, &["edit"])).unwrap();
        let form = json!([
            {"name": "input", "label": "Input", "kind": "vectorLayer", "required": true},
            {"name": "other", "label": "Other", "kind": "pointLayer"},
            {"name": "trees", "label": "Trees", "kind": "number", "default": 50},
            {"name": "method", "label": "Method", "kind": "choice", "choices": ["a", "b"], "default": "b"},
            {"name": "note", "label": "Note", "kind": "text", "required": true}
        ]);
        let commands = m["commands"].as_array_mut().unwrap();
        commands.push(json!({"id": "q", "label": "Echo", "form": form}));
        // Echoes too (the test plugin answers by the id's first letter), with optional fields only.
        commands.push(json!({"id": "qq", "label": "Echo Optional", "form": [
            {"name": "count", "label": "Count", "kind": "number"},
            {"name": "flag", "label": "Flag", "kind": "bool"},
            {"name": "agree", "label": "Agree", "kind": "bool", "required": true}
        ]}));
        for c in commands.iter_mut().filter(|c| c["id"] == "e") {
            c["form"] = json!([{"name": "trees", "label": "Trees", "kind": "number"}]);
        }
        let manifest = m.to_string();
        let zip = plugins::testing::zip_of(&[("plugin.json", manifest.as_bytes()), ("plugin.wasm", &plugins::testing::wasm())]);
        plugins::load(plugins::Package::from_zip(&zip).unwrap(), "test").unwrap();
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1400.0, 860.0)).with_max_steps(64).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            RustGisApp::with_sample(),
        );
        h.run();
        h
    }

    fn dialog<'a>(h: &'a egui_kittest::Harness<'static, RustGisApp>) -> &'a Dialog {
        h.state().ui.plugin_dialog.open.as_ref().unwrap()
    }

    /// Run waits for the required fields, asks for the command as a ribbon button does, and
    /// shows what it answered; the values come back when the dialog opens again.
    #[test]
    fn a_command_dialog_runs_with_its_values_and_shows_the_answer() {
        let mut h = launch("ui-dialog");
        // Its button (or Command Search, or a shortcut) opens the dialog instead of running it.
        h.state_mut().run_by_user("plugin:ui-dialog/q", Value::Null).unwrap();
        h.run();
        assert!(dialog(&h).asked.is_none() && dialog(&h).result.is_none());
        // The required layer is picked, the optional one left out, the defaults set.
        let values = &dialog(&h).values;
        let input = values["input"].as_str().unwrap().to_string();
        assert!(h.state().session.map().layer_by_name(&input).is_some_and(|l| !l.is_raster()), "{input}");
        assert!(!values.contains_key("other"), "{values:?}");
        assert_eq!((values["trees"].clone(), values["method"].clone()), (json!(50), json!("b")));
        // The note is required and empty: Run waits for it.
        h.get_by_label("Run").click();
        h.run();
        assert!(dialog(&h).asked.is_none() && dialog(&h).result.is_none());
        h.state_mut().ui.plugin_dialog.open.as_mut().unwrap().values.insert("note".into(), json!("hi"));
        h.run();
        h.get_by_label("Run").click();
        h.step();
        assert!(dialog(&h).asked.is_some(), "waits for the window to show it running");
        h.run();
        let Some(Ok(answer)) = &dialog(&h).result else { panic!("{:?}", dialog(&h).result) };
        let echoed: Value = serde_json::from_str(answer).unwrap();
        assert_eq!(echoed["params"], json!({"input": input, "trees": 50, "method": "b", "note": "hi"}));
        // Another dialog keeps this one's values for when it opens again.
        h.state_mut().ui.plugin_dialog.open.as_mut().unwrap().values.insert("trees".into(), json!(7));
        h.state_mut().run("ui.pluginForm", json!({"plugin": "ui-dialog", "command": "e"})).unwrap();
        h.run();
        h.get_by_label("Run").click();
        h.run();
        assert!(matches!(&dialog(&h).result, Some(Err(e)) if e.ends_with("nope")), "{:?}", dialog(&h).result);
        h.state_mut().run("ui.pluginForm", json!({"plugin": "ui-dialog", "command": "q"})).unwrap();
        assert_eq!(dialog(&h).values["trees"], json!(7));
        assert!(h.state_mut().run("ui.pluginForm", json!({"plugin": "ui-dialog", "command": "h"})).is_err(), "no dialog");
        plugins::unload("ui-dialog");
    }

    /// Optional numbers and checkboxes never set are left out; a required checkbox always has a
    /// value, so it never holds Run. `{}` opens the dialog as `null`
    /// does, and its Run with no values runs the command rather than opening the dialog again.
    #[test]
    fn optional_fields_left_alone_are_left_out() {
        let mut h = launch("ui-dialog-optional");
        h.state_mut().run_by_user("plugin:ui-dialog-optional/qq", json!({})).unwrap();
        h.run();
        let values = &dialog(&h).values;
        assert!(!values.contains_key("count") && !values.contains_key("flag"), "{values:?}");
        h.get_by_label("Run").click();
        h.run();
        let Some(Ok(answer)) = &dialog(&h).result else { panic!("{:?}", dialog(&h).result) };
        let echoed: Value = serde_json::from_str(answer).unwrap();
        assert_eq!(echoed["params"], json!({"agree": false}), "a required checkbox is sent unticked");
        // A number typed is sent as one; text that isn't a number holds Run.
        let d = h.state_mut().ui.plugin_dialog.open.as_mut().unwrap();
        d.texts.insert("count".into(), "2,5".into());
        d.result = None;
        h.run();
        h.get_by_label("Run").click();
        h.run();
        assert!(dialog(&h).asked.is_none() && dialog(&h).result.is_none(), "not run");
        h.state_mut().ui.plugin_dialog.open.as_mut().unwrap().texts.insert("count".into(), "2.5".into());
        h.run();
        h.get_by_label("Run").click();
        h.run();
        let Some(Ok(answer)) = &dialog(&h).result else { panic!("{:?}", dialog(&h).result) };
        let echoed: Value = serde_json::from_str(answer).unwrap();
        assert_eq!(echoed["params"], json!({"count": 2.5, "agree": false}));
        plugins::unload("ui-dialog-optional");
    }

    /// While the dialog's run waits to start, an answer of the same command run with other
    /// parameters (a retry landing, say) isn't taken for its own.
    #[test]
    fn another_runs_answer_is_not_the_dialogs() {
        let mut h = launch("ui-dialog-other");
        h.state_mut().run_by_user("plugin:ui-dialog-other/qq", Value::Null).unwrap();
        h.run();
        h.get_by_label("Run").click();
        h.step();
        assert!(dialog(&h).asked.is_some(), "waits to start");
        answered(h.state_mut(), "plugin:ui-dialog-other/qq", &Ok(json!({"report": "another run"})));
        assert!(dialog(&h).asked.is_some() && dialog(&h).result.is_none(), "{:?}", dialog(&h).result);
        h.run();
        let Some(Ok(answer)) = &dialog(&h).result else { panic!("{:?}", dialog(&h).result) };
        assert!(answer.contains("\"params\""), "{answer}");
        plugins::unload("ui-dialog-other");
    }

    /// Values kept from before a plugin changed its form are taken only where they still fit:
    /// a field gone or of another kind starts over, and a field added since starts at its
    /// default; one the user left empty stays empty.
    #[test]
    fn kept_values_that_no_longer_fit_start_over() {
        let mut h = launch("ui-dialog-stale");
        // `other` was left empty; `trees` came with an update, after these values were kept.
        let kept = json!({"input": "Countries", "note": 50, "other": null, "gone": 1});
        let kept = kept.as_object().cloned().unwrap();
        h.state_mut().ui.plugin_dialog.kept.insert(("ui-dialog-stale".into(), "q".into()), kept);
        h.state_mut().run("ui.pluginForm", json!({"plugin": "ui-dialog-stale", "command": "q"})).unwrap();
        let values = &dialog(&h).values;
        assert_eq!(values.get("input"), Some(&json!("Countries")));
        assert_eq!(values.get("trees"), Some(&json!(50)), "a new field starts at its default: {values:?}");
        assert!(!values.contains_key("other"), "left empty: {values:?}");
        assert!(!values.contains_key("note") && !values.contains_key("gone"), "{values:?}");
        plugins::unload("ui-dialog-stale");
    }

    /// Run sends only the values the form has now, of its fields' kinds: a plugin replaced
    /// while its dialog is open may have dropped a field, or changed its kind.
    #[test]
    fn run_sends_only_values_of_the_form_as_it_is() {
        let form: Vec<ParamDecl> = serde_json::from_value(json!([
            {"name": "input", "label": "Input", "kind": "vectorLayer"},
            {"name": "method", "label": "Method", "kind": "number"}
        ]))
        .unwrap();
        let values = json!({"input": "Countries", "method": "b", "trees": 50}).as_object().cloned().unwrap();
        assert_eq!(form_params(&form, &values), json!({"input": "Countries"}));
        // A required field holding a value of another kind holds Run, rather than run without it.
        let mut method = form[1].clone();
        method.required = true;
        assert!(unfilled(&method, &values));
        assert!(!unfilled(&method, &json!({"method": 3}).as_object().cloned().unwrap()));
        // A required number holding text that isn't one is said to be that, not to be filled in.
        let texts = HashMap::from([("method".to_string(), "2,5".to_string())]);
        let (missing, bad) = held(std::slice::from_ref(&method), &Map::new(), &texts);
        assert!(missing.is_empty() && bad.len() == 1);
    }

    /// A `report` text is shown as it is; any other answer as JSON, without its panel state; a
    /// long one is cut, saying how much more there is.
    #[test]
    fn the_answer_shown_is_its_report() {
        assert_eq!(report(&json!({"report": "3 classes\nKappa 0.91", "panel": {"values": {}}})), "3 classes\nKappa 0.91");
        assert_eq!(report(&json!({"n": 1, "panel": {"values": {}}})), "{\n  \"n\": 1\n}");
        assert_eq!(report(&Value::Null), "Done.");
        // Numbers typed: integers exactly, past 2^53 too; one past i64 isn't taken, not rounded.
        assert_eq!(number("9007199254740993"), Some(json!(9_007_199_254_740_993_i64)));
        assert_eq!(number(" 2.0 "), Some(json!(2)));
        assert_eq!(number("2.5"), Some(json!(2.5)));
        assert_eq!(number("99999999999999999999"), None);
        assert_eq!(number("9007199254740993.0"), None, "would be rounded");
        // From 2^52 a float holds no fraction; just below, it holds halves.
        assert_eq!(number("4503599627370496.5"), None);
        assert_eq!(number("4503599627370495.5"), Some(json!(4_503_599_627_370_495.5)));
        assert_eq!(number("1e16"), None);
        assert_eq!(number("1e3"), Some(json!(1000)));
        assert_eq!(number("1e-400"), None, "too small for a float: would be sent as 0");
        // Errors are redacted as their notices are.
        let e = error_shown("https://host/data?sig=abc: HTTP 503");
        assert!(!e.contains("abc") && e.starts_with("https://host/data"), "{e}");
        assert_eq!(number("0e10"), Some(json!(0)));
        assert_eq!(number(""), None);
        // A long one is cut, and says how much was left out.
        let long = shown("é".repeat(40_000));
        assert!(long.len() < SHOWN + 100, "{}", long.len());
        assert!(long.ends_with("… 7232 more characters not shown"), "{}", &long[long.len() - 60..]);
    }
}
