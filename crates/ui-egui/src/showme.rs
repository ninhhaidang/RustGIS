//! Show Me, the Tasks pane's practice help (#410): where a step's command is, for the user to do
//! the step themselves, rather than Run doing it for them.
//!
//! A command on the ribbon gets its tab opened and its button outlined for a few seconds. A tool
//! is outlined in the Analyze gallery, or opened, empty, in the Tools pane. Symbology opens in the
//! Symbology pane of the step's layer. Anything else, a command no button runs (a context menu's,
//! or one inside a menu), opens Command Search on its name, which says where it is.

use egui::{CornerRadius, Rect, Stroke, Ui};
use rustgis_engine::project::TaskStep;
use serde_json::{Value, json};

use crate::{RightPane, RustGisApp, ribbon};

/// How long the button stays outlined (seconds).
const SHOWN_FOR: f64 = 4.0;
/// Frames to wait for the button to be drawn before taking it for not on the ribbon.
const FRAMES_TO_FIND: u8 = 3;

/// A button being shown: the command it runs (and the parameter that tells it from the others
/// running that command), what to do if no such button is drawn, and how it is going.
#[derive(Clone, Debug)]
pub struct ShowMe {
    pub id: String,
    pub key: Option<(&'static str, Value)>,
    pub label: String,
    pub fallback: Option<(&'static str, Value)>,
    pub frames: u8,
    pub found: bool,
    pub started: Option<f64>,
}

/// Ribbon buttons that open a dialog for a command, rather than running it: the dialog's name.
const DIALOGS: &[(&str, &str)] = &[("select.byAttributes", "selectByAttributes"), ("map.goToXY", "goToXY")];

/// Starts showing where a step's command is; returns what to tell the user.
pub fn start(app: &mut RustGisApp, st: &TaskStep) -> String {
    let id = st.command.as_str();
    if matches!(id, "layer.color" | "layer.outline" | "layer.lineStrokes" | "layer.fillLayers" | "layer.symbology" | "layer.transparency") {
        // Making the step's layer the active one is what shows its symbology. It runs on the
        // session directly, so a recording doesn't keep it as a step.
        // A null layer (a task from a file may hold one) is the active layer, as the command reads it.
        if let Some(layer) = st.params.get("layer").filter(|v| !v.is_null())
            && app.session.run("layer.select", &json!({"layer": layer})).is_err()
        {
            let name = layer.as_str().map_or_else(|| layer.to_string(), str::to_string);
            return crate::i18n::fmt("The step's layer {name} isn't in the map.", &[("name", &name)]);
        }
        if let Some(id) = app.session.active_layer {
            crate::panes::reveal_in_filter(app, id);
        }
        app.ui.open_right(RightPane::Symbology);
        return tl!("It's in the Symbology pane, open on the step's layer.").to_string();
    }
    // A command of an extension that is off isn't on the ribbon: say how to turn it on.
    if let Some(why) = app.session.extensions().blocks(id) {
        return why;
    }
    let spec = app.session.registry.get(id);
    let label = spec.map_or_else(|| id.to_string(), |c| tl!(c.label).to_string());
    let (target, key, fallback) = match id {
        "gp.run" => {
            let Some(tool) = st.params.get("tool").filter(|t| t.is_string()).cloned() else {
                return tl!("This step names no tool to show.").to_string();
            };
            ("ui.gp.open", Some(("tool", tool.clone())), Some(("ui.gp.open", json!({"tool": tool}))))
        }
        _ => match DIALOGS.iter().find(|(c, _)| *c == id) {
            Some((_, dialog)) => ("ui.dialog", Some(("name", json!(dialog))), None),
            None => (id, None, None),
        },
    };
    // A command of an extension with a tab of its own is on that tab (#1752).
    let tab = match rustgis_engine::extensions::owner(id).filter(|e| e.tab.is_some()) {
        Some(e) => Some(rustgis_engine::extensions::tab_id(e.id)),
        None => spec.and_then(|c| c.location.split(" › ").next()).filter(|t| ribbon::TABS.contains(t)).map(str::to_string),
    };
    if let Some(tab) = tab.filter(|t| ribbon::tab_exists(app, t)) {
        app.ui.tab = tab;
        app.ui.backstage = false;
        // A minimized ribbon draws no buttons: show it, as a click on a tab does.
        if app.ui.ribbon_minimized {
            app.ui.ribbon_peek = true;
        }
    }
    let label_shown = match id {
        "gp.run" => st
            .params
            .get("tool")
            .and_then(Value::as_str)
            .and_then(rustgis_engine::analysis::tool)
            .map_or(label.clone(), |t| tl!(t.name).to_string()),
        _ => label.clone(),
    };
    app.ui.show_me =
        Some(ShowMe { id: target.to_string(), key, label: label_shown.clone(), fallback, frames: 0, found: false, started: None });
    crate::i18n::fmt("Look for {name}, outlined on the ribbon.", &[("name", &label_shown)])
}

/// Outlines a ribbon button being shown: called for every button drawn, with what it runs.
pub fn ring(ui: &Ui, app: &mut RustGisApp, r: Rect, id: &str, params: &Value) {
    let Some(sm) = app.ui.show_me.as_mut() else { return };
    if sm.id != id || sm.key.as_ref().is_some_and(|(k, v)| params.get(*k) != Some(v)) {
        return;
    }
    sm.found = true;
    let accent = crate::theme::Tokens::get(ui.ctx()).accent;
    ui.painter().rect_stroke(r.expand(2.0), CornerRadius::same(7), Stroke::new(2.5, accent), egui::StrokeKind::Outside);
}

/// Each frame: falls back when the button isn't on the ribbon, and stops after a while.
pub fn tick(app: &mut RustGisApp, ctx: &egui::Context) {
    let Some(sm) = app.ui.show_me.as_mut() else { return };
    let now = ctx.input(|i| i.time);
    let started = *sm.started.get_or_insert(now);
    sm.frames = sm.frames.saturating_add(1);
    if !sm.found && sm.frames > FRAMES_TO_FIND {
        let Some(sm) = app.ui.show_me.take() else { return };
        match sm.fallback {
            Some((cmd, p)) => {
                let _ = app.run(cmd, p);
            }
            // Not a button: Command Search finds it by name and says where it is.
            None => app.ui.command_search = Some(sm.label),
        }
    } else if now - started > SHOWN_FOR {
        app.ui.show_me = None;
        // Redraw once more, so an idle window doesn't keep the outline.
        ctx.request_repaint();
    } else {
        ctx.request_repaint_after(web_time::Duration::from_millis(100));
    }
}
