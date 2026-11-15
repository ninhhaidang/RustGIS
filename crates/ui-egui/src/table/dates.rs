//! A cancellable scan of the whole column before the user chooses its date format.

use super::*;
use rustgis_engine::data::{
    FieldType,
    date_format::{DateFormat, PRESETS},
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc,
};

/// One fitting preset with a few examples to distinguish ambiguous date orders.
struct Choice {
    index: usize,
    examples: Vec<(String, String)>,
}

/// A format dialog owns its original rows, schema and target identity. Any edit invalidates it.
/// Dropping it cancels its worker, including when another project or table replaces the target.
pub(super) struct Dialog {
    key: ViewKey,
    table_instance: Option<u64>,
    dataset: Dataset,
    field: usize,
    ty: FieldType,
    params: Value,
    pivot: Option<i64>,
    selected: Option<usize>,
    choices: Option<Result<Vec<Choice>, String>>,
    result: mpsc::Receiver<Result<Vec<Choice>, String>>,
    cancel: Arc<AtomicBool>,
    done: Arc<AtomicUsize>,
}

impl Drop for Dialog {
    /// A closed dialog must not keep parsing a column no one will use.
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Dialog {
    /// Starts from the displayed column snapshot; no conversion is submitted until a choice.
    pub(super) fn new(key: ViewKey, table_instance: Option<u64>, dataset: Dataset, field: usize, ty: &str, params: Value) -> Self {
        let (_, result) = mpsc::channel();
        let mut dialog = Self {
            key,
            table_instance,
            dataset,
            field,
            ty: if ty == "date" { FieldType::Date } else { FieldType::DateTime },
            params,
            pivot: None,
            selected: None,
            choices: None,
            result,
            cancel: Arc::new(AtomicBool::new(false)),
            done: Arc::new(AtomicUsize::new(0)),
        };
        dialog.start();
        dialog
    }

    /// Restarts after the user specifies a century, cancelling the previous scan first.
    fn start(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.cancel = Arc::new(AtomicBool::new(false));
        self.done = Arc::new(AtomicUsize::new(0));
        self.selected = None;
        self.choices = None;
        let (send, receive) = mpsc::channel();
        self.result = receive;
        let (dataset, field, ty, pivot) = (self.dataset.clone(), self.field, self.ty, self.pivot);
        let (cancel, done) = (self.cancel.clone(), self.done.clone());
        let work = move || {
            let _ = send.send(scan(&dataset, field, ty, pivot, &cancel, &done));
        };
        #[cfg(not(target_arch = "wasm32"))]
        if let Err(e) = std::thread::Builder::new().name("date formats".into()).spawn(work) {
            self.choices = Some(Err(e.to_string()));
        }
        #[cfg(target_arch = "wasm32")]
        work();
    }

    /// Identity, schema and rows must still be those whose every value was checked.
    fn current(&self, app: &RustGisApp) -> bool {
        if app.table.opened != app.session.opened {
            return false;
        }
        let ds = match self.key {
            ViewKey::Layer(instance) => app.session.map().layers.iter().find(|l| l.instance_id() == instance).map(|l| &l.dataset),
            ViewKey::Table(id) => {
                app.session.project.table(id).filter(|t| Some(t.instance_id()) == self.table_instance).map(|t| &t.dataset)
            }
        };
        ds.is_some_and(|ds| ds.fields == self.dataset.fields && ds.features.shares(&self.dataset.features))
    }
}

/// Reads every non-empty value, retaining only presets that fit all of them. Cancellation is
/// checked between rows; the progress count never includes an unexamined row.
fn scan(
    ds: &Dataset,
    field: usize,
    ty: FieldType,
    pivot: Option<i64>,
    cancel: &AtomicBool,
    done: &AtomicUsize,
) -> Result<Vec<Choice>, String> {
    let mut candidates: Vec<_> = PRESETS
        .iter()
        .enumerate()
        .filter_map(|(index, (_, pattern))| {
            let year = if pattern.contains("%y") { pivot } else { None };
            DateFormat::new(pattern, year).ok().map(|reader| (reader, Choice { index, examples: Vec::new() }))
        })
        .collect();
    for (row, f) in ds.features.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        if let Some(value) = f.attrs.get(field).filter(|v| !v.asks_null()) {
            let text = value.to_string();
            candidates.retain_mut(|(reader, choice)| {
                let (v, _) = reader.parse(&text, ty);
                if v.is_null() {
                    return false;
                }
                if choice.examples.len() < 3 {
                    choice.examples.push((text.chars().take(60).collect(), v.to_string()));
                }
                true
            });
        }
        done.store(row + 1, Ordering::Relaxed);
    }
    Ok(candidates.into_iter().map(|(_, choice)| choice).collect())
}

/// Draws the pending choice even if the table pane closes. Conversion keeps the original
/// target, and a changed target closes the dialog instead of applying a stale preview.
pub(super) fn show(app: &mut RustGisApp, ctx: &egui::Context) {
    let Some(mut dialog) = app.table.dates.take() else { return };
    if !dialog.current(app) {
        return;
    }
    if dialog.choices.is_none() {
        match dialog.result.try_recv() {
            Ok(result) => dialog.choices = Some(result),
            Err(mpsc::TryRecvError::Disconnected) => dialog.choices = Some(Err(tl!("Date format scan stopped").into())),
            Err(mpsc::TryRecvError::Empty) => ctx.request_repaint_after(std::time::Duration::from_millis(100)),
        }
    }
    let (mut open, mut convert, mut restart, mut cancel) = (true, false, false, false);
    egui::Window::new(tl!("Convert Field to Dates")).id(egui::Id::unique("convert_dates"))
        .open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
            ui.set_max_width(540.0);
            if let Some(field) = dialog.dataset.fields.get(dialog.field) { ui.strong(&field.name); }
            ui.label(tl!("Choose what the text means. Dates are never guessed."));
            let mut two_digit = dialog.pivot.is_some();
            if ui.checkbox(&mut two_digit, tl!("Include two-digit years")).changed() {
                dialog.pivot = two_digit.then_some(1900);
                restart = true;
            }
            if let Some(pivot) = &mut dialog.pivot {
                ui.horizontal(|ui| {
                    ui.label(tl!("First year of 100-year window"));
                    restart |= ui.add(egui::DragValue::new(pivot).range(1..=9900)).changed();
                });
            }
            if restart { dialog.start(); }
            match &dialog.choices {
                None => {
                    ui.spinner();
                    ui.label(crate::i18n::fmt("Checking rows: {done} of {total}", &[
                        ("done", &dialog.done.load(Ordering::Relaxed).to_string()),
                        ("total", &dialog.dataset.features.len().to_string()),
                    ]));
                }
                Some(Err(e)) => { ui.label(e); }
                Some(Ok(choices)) => {
                    if choices.is_empty() { ui.label(tl!("No preset reads every non-empty value. Use an explicit format with edit.convertField, or correct the text first.")); }
                    egui::ScrollArea::vertical().max_height(340.0).show(ui, |ui| {
                        for choice in choices {
                            if let Some((label, _)) = PRESETS.get(choice.index) {
                                ui.radio_value(&mut dialog.selected, Some(choice.index), crate::i18n::t(label));
                                for (input, output) in &choice.examples {
                                    ui.small(format!("{input} → {output}"));
                                }
                            }
                        }
                    });
                }
            }
            ui.horizontal(|ui| {
                convert = ui.add_enabled(dialog.selected.is_some(), egui::Button::new(tl!("Convert"))).clicked();
                if ui.button(tl!("Cancel")).clicked() { cancel = true; }
            });
        });
    // Window::open handles its close button; Cancel uses the explicit flag below too.
    if convert {
        if let Some((_, pattern)) = dialog.selected.and_then(|i| PRESETS.get(i)) {
            dialog.params["format"] = json!(pattern);
            if pattern.contains("%y") {
                dialog.params["pivotYear"] = json!(dialog.pivot);
            }
            let mark = app.session.notice_mark();
            #[cfg(not(target_arch = "wasm32"))]
            let result = crate::readjob::run_date_preview(app, dialog.params.clone(), dialog.dataset.clone());
            #[cfg(target_arch = "wasm32")]
            let result = app.run("edit.convertField", dialog.params.clone());
            if let Err(e) = result
                && app.session.notices_since(mark).is_empty()
            {
                app.session.notify(e, true);
            }
        }
    } else if open && !cancel {
        app.table.dates = Some(dialog);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::{NodeT, Queryable};

    /// A read-in-place view is a sample, never evidence for the whole-column preview.
    #[test]
    fn date_dialog_refuses_streamed_layer_before_scan() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dates.geojson");
        std::fs::write(&path, r#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{"opened":"03/05/2026"},"geometry":{"type":"Point","coordinates":[8,47]}}]}"#).unwrap();
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        let id = app.run("layer.addData", json!({"path":path, "stream":true})).unwrap()["layer"].as_u64().unwrap();
        app.run("table.open", json!({"layer":id})).unwrap();
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1400.0, 1000.0)).with_max_steps(64).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app,
        );
        h.run_steps(4);
        h.get_by_label("opened").click_secondary();
        h.run_steps(2);
        h.get_by_label("Convert to Date").click();
        h.run_steps(3);
        assert!(h.state().table.dates.is_none());
        assert!(h.state().session.notices.iter().any(|n| n.text.contains("Load All Features")));
        assert!(h.state().session.map().layer(id as u32).unwrap().is_streamed());
    }

    /// Drives the real column menu and dialog: ambiguous formats stay unselected, Cancel leaves
    /// text intact, and the chosen order is the command's order and one undo step.
    #[test]
    fn date_dialog_requires_a_choice_and_converts_the_original_table() {
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        let added = app.run("layer.addDataText", json!({"fileName":"dates.csv", "text":"opened\n03/05/2026\n04/06/2026\n"})).unwrap();
        app.session.table_standalone = added["id"].as_u64().map(|n| n as u32);
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1400.0, 1000.0)).with_max_steps(64).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app,
        );
        h.run_steps(4);
        h.get_by_label("opened").click_secondary();
        h.run_steps(2);
        h.get_by_label("Convert to Date").click();
        h.run_steps(3);
        // Join only this tiny fixture's scan; production frames never block on its receiver.
        let d = h.state_mut().table.dates.as_mut().unwrap();
        if d.choices.is_none() {
            d.choices = Some(d.result.recv_timeout(std::time::Duration::from_secs(5)).unwrap());
        }
        assert!(d.selected.is_none());
        let choices = d.choices.as_ref().unwrap().as_ref().unwrap();
        assert_eq!(choices.iter().map(|c| PRESETS[c.index].0).collect::<Vec<_>>(), ["MM/DD/YYYY", "DD/MM/YYYY"]);
        h.run_steps(2);
        assert!(h.get_by_label("Convert").accesskit_node().is_disabled());
        h.get_by_label("Cancel").click();
        h.run_steps(2);
        assert!(h.state().table.dates.is_none());
        assert_eq!(h.state().session.project.tables[0].dataset.fields[0].ty, FieldType::Text);
        h.get_by_label("opened").click_secondary();
        h.run_steps(2);
        h.get_by_label("Convert to Date").click();
        h.run_steps(3);
        let d = h.state_mut().table.dates.as_mut().unwrap();
        if d.choices.is_none() {
            d.choices = Some(d.result.recv_timeout(std::time::Duration::from_secs(5)).unwrap());
        }
        h.run_steps(2);
        h.get_by_label("DD/MM/YYYY").click();
        h.run_steps(2);
        let depth = h.state().session.undo_depth();
        h.get_by_label("Convert").click();
        h.run_steps(3);
        assert!(h.state().table.dates.is_none());
        assert_eq!(h.state().session.project.tables[0].dataset.value(0, 0).to_string(), "2026-05-03");
        assert_eq!(h.state().session.undo_depth(), depth + 1);
        h.state_mut().run("edit.undo", json!({})).unwrap();
        assert_eq!(h.state().session.project.tables[0].dataset.value(0, 0).to_string(), "03/05/2026");
    }

    #[test]
    fn date_scan_checks_all_rows_and_cancel() {
        let mut ds = Dataset::new("dates", rustgis_engine::data::GeometryType::None, vec![Field::new("d", FieldType::Text)]);
        for text in ["03/05/2026", "", "<null>", "14/05/2026"] {
            ds.push(None, vec![V::Text(text.into())]);
        }
        let done = AtomicUsize::new(0);
        let choices = scan(&ds, 0, FieldType::Date, None, &AtomicBool::new(false), &done).unwrap();
        assert_eq!(done.load(Ordering::Relaxed), 4);
        assert_eq!(choices.iter().map(|c| PRESETS[c.index].0).collect::<Vec<_>>(), ["DD/MM/YYYY"]);
        assert!(scan(&ds, 0, FieldType::Date, None, &AtomicBool::new(true), &done).is_err());
        let mut ds = Dataset::new("dates", rustgis_engine::data::GeometryType::None, vec![Field::new("d", FieldType::Text)]);
        for text in ["03/05/26 14:30", "04/06/26 12:15"] {
            ds.push(None, vec![V::Text(text.into())]);
        }
        assert!(scan(&ds, 0, FieldType::DateTime, None, &AtomicBool::new(false), &done).unwrap().is_empty());
        let choices = scan(&ds, 0, FieldType::DateTime, Some(1926), &AtomicBool::new(false), &done).unwrap();
        assert_eq!(choices.iter().map(|c| PRESETS[c.index].0).collect::<Vec<_>>(), ["MM/DD/YY HH:MM", "DD/MM/YY HH:MM"]);
        assert_eq!(choices[0].examples[0].1, "1926-03-05T14:30:00");
    }
}
