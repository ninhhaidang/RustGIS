//! Panes for tasks, the Workflow Manager, elevation profiles, the Diagnostic Monitor and
//! infographics, and the Browser view shown in place of the map.

use egui::{Color32, Sense, Ui, pos2, vec2};
use serde_json::{Value, json};

use crate::RustGisApp;
use crate::theme::{Tokens, regular, semibold};

/// State of these panes.
#[derive(Default)]
pub struct WorkspaceState {
    pub task: String,
    pub task_step: usize,
    /// The steps recorded so far while Record Task is on.
    pub recording: Option<Vec<rustgis_engine::project::TaskStep>>,
    /// The recording reached a task's 200 steps (and said so).
    pub recording_full: bool,
    /// The recording said that Undo keeps a recorded step.
    pub recording_undo_said: bool,
    /// Layer names the recording said two layers share.
    pub recording_shared_said: Vec<String>,
    /// The recording said a step keeps a `key` parameter.
    pub recording_key_said: bool,
    /// The name typed for the task being recorded.
    pub record_name: String,
    /// The Tasks pane shows its editing controls.
    pub task_editing: bool,
    /// The step being edited: its index, title and instructions as typed so far.
    pub task_edit: Option<(usize, String, String)>,
    pub job_name: String,
    pub profile_layer: String,
    pub profile_lines: String,
    pub profile: Option<Value>,
    /// The Time Series pane's Zarr layer, and the series read at points (`zarr.series`), newest last.
    pub series_layer: String,
    pub series: Vec<Value>,
    /// The Time Series pane's summary over time (#556): the statistic, its threshold, the period
    /// and months as typed, and the run under way.
    pub reduce_stat: String,
    pub reduce_above: f64,
    pub reduce_start: String,
    pub reduce_end: String,
    pub reduce_months: String,
    #[cfg(not(target_arch = "wasm32"))]
    pub reducing: Option<Reducing>,
    pub message: Option<(String, bool)>,
    pub catalog_view: bool,
    pub item_popups: bool,
    pub link_cursors: bool,
    /// A second map shown beside the active one, by name: deleting or renaming it (by any
    /// command) turns the split off, undoing that brings it back, and deleting other maps
    /// doesn't move it. Read it through `mapview::split_map`.
    pub split: Option<String>,
    pub playing: Option<(f64, usize)>,
    /// Diagnostics (refreshed every second) and infographics (when the view or data change),
    /// with what they were worked out for.
    pub diagnostics: Option<(f64, Value)>,
    pub infographic: Option<([f64; 4], Value)>,
}

fn heading(ui: &mut Ui, text: &str) {
    ui.add_space(6.0);
    ui.label(egui::RichText::new(tl!(text)).font(semibold(12.5)));
    ui.add_space(2.0);
}

fn message(app: &RustGisApp, ui: &mut Ui) {
    if let Some((m, err)) = &app.ws.message {
        let c = if *err { Color32::from_rgb(0xC0, 0x30, 0x30) } else { ui.visuals().weak_text_color() };
        ui.label(egui::RichText::new(m.as_str()).small().color(c));
    }
}

fn run(app: &mut RustGisApp, id: &str, p: Value) -> Option<Value> {
    match app.run(id, p) {
        Ok(v) => {
            app.ws.message = None;
            Some(v)
        }
        Err(e) => {
            app.ws.message = Some((e, true));
            None
        }
    }
}

/// After step `from` moves to `to` (as `task.moveStep` does), the step being edited (and the
/// current one) keep pointing at the same steps, so Save writes the text where it was typed: the
/// moved step is at `to`, and those it passed shift by one towards `from`.
pub fn follow_move(app: &mut RustGisApp, from: usize, to: usize) {
    let swap = |i: usize| {
        if i == from {
            to
        } else if from < to && (from + 1..=to).contains(&i) {
            i - 1
        } else if to < from && (to..from).contains(&i) {
            i + 1
        } else {
            i
        }
    };
    if let Some(edit) = app.ws.task_edit.as_mut() {
        edit.0 = swap(edit.0);
    }
    app.ws.task_step = swap(app.ws.task_step);
}

/// After a step is deleted: an edit open on it closes, one open on a later step (and the current
/// step) moves up with it, so typed text stays with its step.
pub fn follow_delete(app: &mut RustGisApp, deleted: usize) {
    match app.ws.task_edit.as_ref().map(|e| e.0) {
        Some(e) if e == deleted => app.ws.task_edit = None,
        Some(e) if e > deleted => {
            if let Some(edit) = app.ws.task_edit.as_mut() {
                edit.0 -= 1;
            }
        }
        _ => {}
    }
    if app.ws.task_step > deleted {
        app.ws.task_step -= 1;
    }
    // Deleting the last step leaves the current one on the step before it.
    let n = app.session.project.tasks.iter().find(|t| t.name == app.ws.task).map_or(0, |t| t.steps.len());
    app.ws.task_step = app.ws.task_step.min(n.saturating_sub(1));
}

/// Record Task: the button that starts or stops recording, and how far it has got.
fn record_row(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    ui.horizontal_wrapped(|ui| match app.ws.recording.as_ref().map(Vec::len) {
        Some(n) => {
            ui.label(egui::RichText::new("●").color(t.red));
            let input_label = ui.label(crate::i18n::fmt("Recording: {n} steps", &[("n", &n.to_string())]));
            ui.add(egui::TextEdit::singleline(&mut app.ws.record_name).hint_text(tl!("Task name")).desired_width(150.0))
                .labelled_by(input_label.id);
            if ui.button(tl!("Stop Recording")).on_hover_text(tl!("Make a task of the steps recorded")).clicked() {
                let name = app.ws.record_name.clone();
                run(app, "ui.taskRecord", json!({"name": name}));
            }
        }
        None => {
            if ui
                .button(tl!("Record Task"))
                .on_hover_text(tl!("Do the exercise once: each command that changes the project becomes a step"))
                .clicked()
            {
                run(app, "ui.taskRecord", json!({}));
            }
        }
    });
}

/// Tasks: pick one, read each step's instructions and run it; record a task, and edit its steps.
pub fn tasks(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    // Rows wrap at the pane's width rather than widen it: long titles and instructions would
    // otherwise be cut at its edge.
    ui.set_max_width(ui.available_width());
    message(app, ui);
    record_row(app, ui, t);
    let tasks = app.session.project.tasks.clone();
    if tasks.is_empty() {
        ui.label(
            egui::RichText::new(tl!("No tasks yet. Record one, or make one from your geoprocessing history with Analyze › Task.")).weak(),
        );
        return;
    }
    if !tasks.iter().any(|t| t.name == app.ws.task) {
        app.ws.task = tasks.first().map(|t| t.name.clone()).unwrap_or_default();
        app.ws.task_step = 0;
        app.ws.task_edit = None;
    }
    ui.horizontal_wrapped(|ui| {
        egui::ComboBox::from_id_salt("task_pick")
            .selected_text(app.ws.task.as_str())
            .width((ui.available_width() - 120.0).max(160.0))
            .show_ui(ui, |ui| {
                for t in &tasks {
                    if ui.selectable_label(t.name == app.ws.task, t.name.as_str()).clicked() {
                        app.ws.task = t.name.clone();
                        app.ws.task_step = 0;
                        app.ws.task_edit = None;
                    }
                }
            })
            .response
            .on_hover_text(tl!("Task"));
        // Leaving edit mode drops a step's unsaved edit, which would come back stale.
        if ui.checkbox(&mut app.ws.task_editing, tl!("Edit Steps")).changed() && !app.ws.task_editing {
            app.ws.task_edit = None;
        }
    });
    let Some(task) = tasks.iter().find(|t| t.name == app.ws.task) else { return };
    if !task.description.is_empty() {
        ui.label(egui::RichText::new(&task.description));
    }
    let name = task.name.clone();
    let last = task.steps.len().saturating_sub(1);
    for (i, st) in task.steps.iter().enumerate() {
        let current = i == app.ws.task_step;
        egui::Frame::NONE
            .inner_margin(egui::Margin::symmetric(6, 4))
            .fill(if current { ui.visuals().selection.bg_fill.gamma_multiply(0.25) } else { Color32::TRANSPARENT })
            .show(ui, |ui| {
                ui.set_max_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    let title =
                        egui::RichText::new(format!("{}. {}", i + 1, st.title)).font(if current { semibold(12.5) } else { regular(12.5) });
                    // Clicking a step's title makes it the current one, to read it.
                    if ui.add(egui::Label::new(title).sense(Sense::click())).clicked() {
                        app.ws.task_step = i;
                    }
                    if app.ws.task_editing {
                        // (`RustGisApp::run` keeps the open edit on its step: `follow_move`, `follow_delete`.)
                        if ui.add_enabled(i > 0, egui::Button::new("▲").small()).on_hover_text(tl!("Move Up")).clicked() {
                            run(app, "task.moveStep", json!({"task": name, "step": i, "to": i - 1}));
                        }
                        if ui.add_enabled(i < last, egui::Button::new("▼").small()).on_hover_text(tl!("Move Down")).clicked() {
                            run(app, "task.moveStep", json!({"task": name, "step": i, "to": i + 1}));
                        }
                        if ui.small_button(tl!("Edit")).on_hover_text(tl!("Edit the title and instructions")).clicked() {
                            app.ws.task_edit = Some((i, st.title.clone(), st.instructions.clone()));
                            app.ws.task_step = i;
                        }
                        if ui.small_button(tl!("Delete")).on_hover_text(tl!("Delete the step")).clicked() {
                            run(app, "task.removeStep", json!({"task": name, "step": i}));
                        }
                    } else {
                        // Practice: Show Me says where to do the step; Run does it.
                        if ui
                            .small_button(tl!("Show Me"))
                            .on_hover_text(tl!("Show where this step's command is, to do it yourself"))
                            .clicked()
                        {
                            app.ws.task_step = i;
                            let said = crate::showme::start(app, st);
                            app.ws.message = Some((said, false));
                        }
                        if ui.small_button(tl!("Run")).on_hover_text(tl!("Do the step for me")).clicked()
                            && run(app, "task.run", json!({"task": name, "step": i})).is_some()
                        {
                            app.ws.task_step = i + 1;
                            app.ws.message = Some((crate::i18n::fmt("Step {n} done", &[("n", &(i + 1).to_string())]), false));
                            app.map.invalidate();
                        }
                    }
                });
                let editing = app.ws.task_editing && app.ws.task_edit.as_ref().is_some_and(|e| e.0 == i);
                if editing {
                    let mut save = false;
                    let mut cancel = false;
                    if let Some((_, title, text)) = app.ws.task_edit.as_mut() {
                        // As long as task.editStep keeps them, so nothing typed is cut on Save.
                        ui.add(egui::TextEdit::singleline(title).desired_width(f32::INFINITY).char_limit(200).hint_text(tl!("Title")));
                        ui.add(
                            egui::TextEdit::multiline(text)
                                .char_limit(4000)
                                .desired_rows(5)
                                .desired_width(f32::INFINITY)
                                .hint_text(tl!("What to do, where to find it and why")),
                        );
                        ui.horizontal(|ui| {
                            save = ui.button(tl!("Save")).clicked();
                            cancel = ui.button(tl!("Cancel")).clicked();
                        });
                    }
                    if save && let Some((step, title, text)) = app.ws.task_edit.clone() {
                        // Saving what the step already says only closes the editor.
                        let unchanged = app
                            .session
                            .project
                            .tasks
                            .iter()
                            .find(|t| t.name == name)
                            .and_then(|t| t.steps.get(step))
                            .is_some_and(|s| s.title == title.trim().chars().take(200).collect::<String>() && s.instructions == text);
                        if unchanged
                            || run(app, "task.editStep", json!({"task": name, "step": step, "title": title, "instructions": text}))
                                .is_some()
                        {
                            app.ws.task_edit = None;
                        }
                    } else if cancel {
                        app.ws.task_edit = None;
                    }
                } else if !st.instructions.is_empty() && current {
                    // Read on a lab screen: the normal text size, not dimmed.
                    ui.label(egui::RichText::new(&st.instructions).font(regular(12.5)));
                }
            });
    }
    if !app.ws.task_editing && app.ws.task_step >= task.steps.len() && !task.steps.is_empty() {
        ui.label(egui::RichText::new(tl!("All steps done.")).small());
        if ui.button(tl!("Start Over")).clicked() {
            app.ws.task_step = 0;
        }
    }
}

/// The Workflow Manager: jobs with their status.
pub fn jobs(app: &mut RustGisApp, ui: &mut Ui, _t: &Tokens) {
    message(app, ui);
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut app.ws.job_name).hint_text(tl!("New job name")).desired_width(170.0));
        if ui.button(tl!("Add Job")).on_hover_text(tl!("The job covers the map view")).clicked() {
            let name = app.ws.job_name.trim().to_string();
            if run(app, "job.new", json!({"name": name, "viewExtent": true})).is_some() {
                app.ws.job_name.clear();
            }
        }
    });
    for status in ["new", "in progress", "on hold", "done"] {
        let jobs: Vec<_> = app.session.project.jobs.iter().filter(|j| j.status == status).cloned().collect();
        if jobs.is_empty() {
            continue;
        }
        heading(
            ui,
            match status {
                "new" => "New",
                "in progress" => "In Progress",
                "on hold" => "On Hold",
                _ => "Done",
            },
        );
        for j in jobs {
            ui.horizontal(|ui| {
                ui.label(format!("#{} {}", j.id, j.name));
                if let Some(e) = j.extent
                    && ui.small_button(tl!("Zoom")).clicked()
                {
                    app.session.zoom_to(&e);
                    app.map.invalidate();
                }
                let next = match status {
                    "new" => Some(("Start", "in progress")),
                    "in progress" => Some(("Done", "done")),
                    "on hold" => Some(("Resume", "in progress")),
                    _ => None,
                };
                if let Some((label, to)) = next
                    && ui.small_button(tl!(label)).clicked()
                {
                    run(app, "job.update", json!({"job": j.id, "status": to}));
                }
                if status == "in progress" && ui.small_button(tl!("Hold")).clicked() {
                    run(app, "job.update", json!({"job": j.id, "status": "on hold"}));
                }
                if ui.small_button("×").clicked() {
                    run(app, "job.remove", json!({"job": j.id}));
                }
            });
        }
    }
}

/// Elevation profiles, drawn as a chart.
pub fn profile(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    message(app, ui);
    let map = app.session.map();
    let rasters: Vec<String> = map.layers.iter().filter(|l| l.is_raster()).map(|l| l.name.clone()).collect();
    let lines: Vec<String> =
        map.layers.iter().filter(|l| l.geometry_type() == rustgis_engine::data::GeometryType::Polyline).map(|l| l.name.clone()).collect();
    if !rasters.contains(&app.ws.profile_layer) {
        app.ws.profile_layer = rasters.first().cloned().unwrap_or_default();
    }
    if rasters.is_empty() {
        ui.label(egui::RichText::new(tl!("Add an elevation raster first.")).weak());
        return;
    }
    egui::ComboBox::from_id_salt("prof_layer")
        .selected_text(app.ws.profile_layer.as_str())
        .show_ui(ui, |ui| {
            for r in &rasters {
                ui.selectable_value(&mut app.ws.profile_layer, r.clone(), r.as_str());
            }
        })
        .response
        .on_hover_text(tl!("Layer"));
    ui.horizontal_wrapped(|ui| {
        for (label, purpose, n) in [
            ("Create (click points)", "p_create", 100),
            ("Auto-complete (2 clicks)", "p_auto", 2),
            ("Cross Section (2 clicks)", "p_cross", 2),
        ] {
            let active = app.workflow.pick.as_ref().is_some_and(|p| p.purpose == purpose);
            if ui.selectable_label(active, tl!(label)).clicked() {
                app.workflow.pick = if active { None } else { Some(crate::analysispane::Pick { purpose, needed: n, points: Vec::new() }) };
            }
        }
    });
    let layer = app.ws.profile_layer.clone();
    let done = |app: &RustGisApp, purpose: &str, min: usize| {
        app.workflow
            .pick
            .as_ref()
            .filter(|p| p.purpose == purpose && p.points.len() >= min)
            .map(|p| p.points.iter().map(|c| [c.x, c.y]).collect::<Vec<_>>())
    };
    for (purpose, cmd) in [("p_auto", "profile.autoComplete"), ("p_cross", "profile.crossSection")] {
        if let Some(pts) = done(app, purpose, 2) {
            app.workflow.pick = None;
            app.ws.profile = run(app, cmd, json!({"layer": layer, "points": pts}));
        }
    }
    if let Some(pts) = done(app, "p_create", 2)
        && ui.button(tl!("Finish Profile")).clicked()
    {
        app.workflow.pick = None;
        app.ws.profile = run(app, "profile.create", json!({"layer": layer, "points": pts}));
    }
    ui.horizontal(|ui| {
        if !lines.contains(&app.ws.profile_lines) {
            app.ws.profile_lines = lines.first().cloned().unwrap_or_default();
        }
        egui::ComboBox::from_id_salt("prof_lines")
            .selected_text(app.ws.profile_lines.as_str())
            .width(120.0)
            .show_ui(ui, |ui| {
                for l in &lines {
                    ui.selectable_value(&mut app.ws.profile_lines, l.clone(), l.as_str());
                }
            })
            .response
            .on_hover_text(tl!("Layer"));
        if ui.button(tl!("Along Selected Line")).clicked() {
            app.ws.profile = run(app, "profile.alongLine", json!({"layer": layer, "lines": app.ws.profile_lines}));
        }
        if ui.button(tl!("All Selected")).clicked() {
            app.ws.profile = run(app, "profile.alongSelected", json!({"layer": layer, "lines": app.ws.profile_lines}))
                .and_then(|v| v.as_array().and_then(|a| a.first().cloned()));
        }
    });
    let Some(p) = app.ws.profile.clone() else { return };
    let pts: Vec<(f64, f64)> = p["samples"]
        .as_array()
        .map(|a| a.iter().filter_map(|s| Some((s["distance"].as_f64()?, s["elevation"].as_f64()?))).collect())
        .unwrap_or_default();
    if pts.len() < 2 {
        return;
    }
    let (lo, hi) = (p["min"].as_f64().unwrap_or(0.0), p["max"].as_f64().unwrap_or(1.0).max(p["min"].as_f64().unwrap_or(0.0) + 1.0));
    let len = p["length"].as_f64().unwrap_or(1.0);
    // A degenerate profile still needs a plotting span; its reported length stays unchanged.
    let plot_len = if len > 0.0 { len } else { 1.0 };
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 150.0), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, t.canvas);
    let to = |(d, z): (f64, f64)| {
        pos2(
            rect.min.x + 6.0 + (d / plot_len) as f32 * (rect.width() - 12.0),
            rect.max.y - 16.0 - ((z - lo) / (hi - lo)) as f32 * (rect.height() - 30.0),
        )
    };
    let line: Vec<egui::Pos2> = pts.iter().map(|p| to(*p)).collect();
    painter.add(egui::Shape::mesh(area_under(&line, rect.max.y - 16.0, t.accent.gamma_multiply(0.18))));
    painter.add(egui::Shape::line(line, egui::Stroke::new(2.0, t.accent)));
    let fmt = |v: f64| crate::measure_units::elevation(v, app.ui.measurement_imperial);
    painter.text(rect.left_top() + vec2(6.0, 4.0), egui::Align2::LEFT_TOP, fmt(hi), regular(10.0), t.text_dim);
    painter.text(rect.left_bottom() + vec2(6.0, -4.0), egui::Align2::LEFT_BOTTOM, fmt(lo), regular(10.0), t.text_dim);
    painter.text(
        rect.right_bottom() + vec2(-6.0, -4.0),
        egui::Align2::RIGHT_BOTTOM,
        crate::measure_units::distance(len, app.ui.measurement_imperial),
        regular(10.0),
        t.text_dim,
    );
    ui.label(
        egui::RichText::new(crate::i18n::fmt(
            "Length {length} · gain {gain} · loss {loss}",
            &[
                ("length", &crate::measure_units::distance(len, app.ui.measurement_imperial)),
                ("gain", &fmt(p["gain"].as_f64().unwrap_or(0.0))),
                ("loss", &fmt(p["loss"].as_f64().unwrap_or(0.0))),
            ],
        ))
        .small(),
    );
}

/// A summary over time running on a thread of its own: Cancel, the slices read, and the result.
#[cfg(not(target_arch = "wasm32"))]
pub struct Reducing {
    pub run: rustgis_engine::cmd::zarr::ReduceRun,
    /// Its place among the project's downloads: a summary that lands after another project
    /// opened is dropped.
    pub id: u64,
    pub cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub done: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    pub result: std::sync::Arc<std::sync::Mutex<Option<Result<Vec<u8>, String>>>>,
}

/// The area between a profile's line and its base, as one quad (two triangles) per segment: a
/// filled path is drawn as a fan from its first point, which only fills a convex shape, and the
/// area under a profile rarely is.
fn area_under(line: &[egui::Pos2], base: f32, colour: Color32) -> egui::Mesh {
    let mut mesh = egui::Mesh::default();
    for w in line.windows(2) {
        let i = mesh.vertices.len() as u32;
        for p in [w[0], w[1], pos2(w[1].x, base), pos2(w[0].x, base)] {
            mesh.colored_vertex(p, colour);
        }
        mesh.add_triangle(i, i + 1, i + 2);
        mesh.add_triangle(i, i + 2, i + 3);
    }
    mesh
}

/// A summary that has finished, added to the map (or its error kept for the pane): called
/// every frame, whatever pane is in view, so a run lands though the Time Series pane closed or
/// its layer went. `true` while one is still running.
#[cfg(not(target_arch = "wasm32"))]
pub fn land_summary(app: &mut RustGisApp) -> bool {
    use std::sync::atomic::Ordering;
    let Some(r) = &app.ws.reducing else { return false };
    let Some(res) = r.result.lock().ok().and_then(|mut g| g.take()) else { return true };
    let (root, variable, name, id) = (r.run.root.clone(), r.run.variable.clone(), r.run.name.clone(), r.id);
    // A run the user cancelled stops quietly: no error for it.
    let cancelled = r.cancel.load(Ordering::Relaxed);
    app.ws.reducing = None;
    match res {
        // Cancelled as it finished: nothing added either.
        Ok(_) if cancelled => app.session.download_failed(id),
        // Added to the project it was run in: one opened meanwhile doesn't get it.
        Ok(tif) => {
            if app.session.download_landed(id, tif) {
                let key = if root.starts_with("http://") || root.starts_with("https://") { "url" } else { "path" };
                // `run` puts any error in the pane's message.
                let _ = run(app, "layer.addZarr", json!({key: root, "variable": variable, "ran": id, "summary": true, "name": name}));
            } else {
                app.ws.message =
                    Some((tl!("The summary finished after another project was opened, so it wasn't added").to_string(), false));
            }
        }
        Err(_) if cancelled => app.session.download_failed(id),
        Err(e) => {
            app.session.download_failed(id);
            app.ws.message = Some((e, true));
        }
    }
    false
}

/// The Time Series pane's Summarise over time section (#556): a statistic of the chosen Zarr
/// layer's steps over a period (and months), computed on a thread with progress and Cancel, and
/// added as a raster layer.
#[cfg(not(target_arch = "wasm32"))]
fn summarise(app: &mut RustGisApp, ui: &mut Ui) {
    use std::sync::atomic::Ordering;
    // A run under way: its progress and Cancel (its result lands through `land_summary`).
    if land_summary(app)
        && let Some(r) = &app.ws.reducing
    {
        let (done, total) = (r.done.load(Ordering::Relaxed), r.run.total().max(1));
        // The section's title stays over its progress.
        ui.label(egui::RichText::new(tl!("Summarise over time")).strong());
        ui.horizontal(|ui| {
            ui.add(egui::ProgressBar::new(done as f32 / total as f32).text(format!("{done} / {total}")).desired_width(200.0));
            if ui.button(tl!("Cancel")).clicked() {
                r.cancel.store(true, Ordering::Relaxed);
            }
        });
        return;
    }
    egui::CollapsingHeader::new(tl!("Summarise over time")).id_salt("series_reduce").show(ui, |ui| {
        if app.ws.reduce_stat.is_empty() {
            app.ws.reduce_stat = "mean".into();
        }
        egui::Grid::new("reduce_grid").num_columns(2).spacing(vec2(8.0, 6.0)).show(ui, |ui| {
            let input_label = ui.label(tl!("Statistic"));
            egui::ComboBox::from_id_salt("reduce_stat")
                .selected_text(app.ws.reduce_stat.as_str())
                .show_ui(ui, |ui| {
                    for st in ["mean", "min", "max", "sum", "std", "count", "countAbove"] {
                        ui.selectable_value(&mut app.ws.reduce_stat, st.to_string(), st);
                    }
                })
                .response
                .labelled_by(input_label.id);
            ui.end_row();
            if app.ws.reduce_stat == "countAbove" {
                let input_label = ui.label(tl!("Above"));
                ui.add(egui::DragValue::new(&mut app.ws.reduce_above).speed(0.5)).labelled_by(input_label.id);
                ui.end_row();
            }
            let input_label = ui.label(tl!("From"));
            ui.add(egui::TextEdit::singleline(&mut app.ws.reduce_start).hint_text("1991-01-01").desired_width(110.0))
                .labelled_by(input_label.id);
            ui.end_row();
            let input_label = ui.label(tl!("To"));
            ui.add(egui::TextEdit::singleline(&mut app.ws.reduce_end).hint_text("2020-12-31").desired_width(110.0))
                .labelled_by(input_label.id);
            ui.end_row();
            let input_label = ui.label(tl!("Months"));
            ui.add(egui::TextEdit::singleline(&mut app.ws.reduce_months).hint_text("12, 1, 2").desired_width(110.0))
                .labelled_by(input_label.id);
            ui.end_row();
        });
        if ui.button(tl!("Summarise")).clicked() {
            let mut p = json!({"layer": app.ws.series_layer, "stat": app.ws.reduce_stat});
            if app.ws.reduce_stat == "countAbove" {
                p["above"] = json!(app.ws.reduce_above);
            }
            for (k, v) in [("start", &app.ws.reduce_start), ("end", &app.ws.reduce_end)] {
                if !v.trim().is_empty() {
                    p[k] = json!(v.trim());
                }
            }
            // Every month written a number: one that isn't (`Dec`, a typo) is said, not dropped.
            let typed: Vec<&str> = app.ws.reduce_months.split([',', ' ']).map(str::trim).filter(|m| !m.is_empty()).collect();
            let months: Vec<Value> = typed.iter().filter_map(|m| m.parse::<u64>().ok()).map(|m| json!(m)).collect();
            if months.len() != typed.len() {
                app.ws.message = Some((tl!("Months are numbers from 1 to 12 (12, 1, 2 for December to February)").to_string(), true));
                return;
            }
            if !months.is_empty() {
                p["months"] = Value::Array(months);
            }
            match rustgis_engine::cmd::zarr::reduce_start(&app.session, &p) {
                Ok(job) => {
                    let id = app.session.download_started();
                    let r =
                        Reducing { run: job.clone(), id, cancel: Default::default(), done: Default::default(), result: Default::default() };
                    let (cancel, done, result) = (r.cancel.clone(), r.done.clone(), r.result.clone());
                    // A panic on the thread (a hostile store) lands as an error, so the run always
                    // ends; a thread the system can't start is said.
                    let diagnostics = app.session.diagnostics.capture();
                    let spawned = std::thread::Builder::new().name("zarr-summary".into()).spawn(move || {
                        let _diagnostics = diagnostics.enter();
                        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            job.run(&cancel, &done)
                                .and_then(|r| rustgis_engine::data::raster::geotiff::write(&r, 0).map_err(|e| e.to_string()))
                        }))
                        .unwrap_or_else(|_| Err(tl!("The summary failed unexpectedly").to_string()));
                        if let Ok(mut g) = result.lock() {
                            *g = Some(out);
                        }
                    });
                    match spawned {
                        Ok(_) => app.ws.reducing = Some(r),
                        Err(e) => {
                            app.session.download_failed(id);
                            app.ws.message =
                                Some((crate::i18n::fmt("The summary couldn't start: {error}", &[("error", &e.to_string())]), true))
                        }
                    }
                }
                Err(e) => app.ws.message = Some((e, true)),
            }
        }
    });
}

/// The most series the Time Series pane keeps (one line each).
const MAX_SERIES: usize = 6;

/// A Zarr variable's time series at points clicked on the map (opengeos/RustGIS#555): a line a
/// point, the time slider's step marked, and a click on the chart takes the slider there.
pub fn time_series(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    message(app, ui);
    let map = app.session.map();
    // Zarr layers: one that follows the time slider, or a slice (`t2m[time=3]`).
    let layers: Vec<String> = map
        .layers
        .iter()
        .filter(|l| l.raster.as_ref().is_some_and(|r| r.time_zarr.is_some() || r.subdataset.as_deref().is_some_and(|s| s.contains('['))))
        .map(|l| l.name.clone())
        .collect();
    if layers.is_empty() {
        ui.label(egui::RichText::new(tl!("Add a Zarr variable first (Home › Add › Add Data › Zarr…).")).weak());
        return;
    }
    if !layers.contains(&app.ws.series_layer) {
        app.ws.series_layer = layers.first().cloned().unwrap_or_default();
    }
    egui::ComboBox::from_id_salt("series_layer")
        .selected_text(app.ws.series_layer.as_str())
        .show_ui(ui, |ui| {
            for l in &layers {
                ui.selectable_value(&mut app.ws.series_layer, l.clone(), l.as_str());
            }
        })
        .response
        .on_hover_text(tl!("Layer"));
    ui.horizontal_wrapped(|ui| {
        let active = app.workflow.pick.as_ref().is_some_and(|p| p.purpose == "ts_point");
        if ui.selectable_label(active, tl!("Pick a Point")).clicked() {
            app.workflow.pick =
                if active { None } else { Some(crate::analysispane::Pick { purpose: "ts_point", needed: 1, points: Vec::new() }) };
        }
        if !app.ws.series.is_empty() {
            if ui.button(tl!("Clear")).clicked() {
                app.ws.series.clear();
            }
            #[cfg(not(target_arch = "wasm32"))]
            if ui.button(tl!("Export CSV…")).clicked()
                && let Some(path) = crate::dialogs::pick_save("time_series.csv", &["csv"], tl!("Time series"))
            {
                app.ws.message = Some(match std::fs::write(&path, series_csv(&app.ws.series)) {
                    Ok(()) => (crate::i18n::fmt("Saved {path}", &[("path", &path)]), false),
                    Err(e) => (format!("{path}: {e}"), true),
                });
            }
        }
    });
    // A point picked: its series, under the layer chosen.
    let picked = app.workflow.pick.as_ref().filter(|p| p.purpose == "ts_point").and_then(|p| p.points.first().copied());
    if let Some(c) = picked {
        app.workflow.pick = None;
        let layer = app.ws.series_layer.clone();
        if let Some(v) = run(app, "zarr.series", json!({"layer": layer, "lon": c.x, "lat": c.y})) {
            app.ws.series.push(v);
            let excess = app.ws.series.len().saturating_sub(MAX_SERIES);
            app.ws.series.drain(..excess);
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    summarise(app, ui);
    if app.ws.series.is_empty() {
        ui.label(egui::RichText::new(tl!("Pick a point on the map to see the variable's values through time there.")).weak().small());
        return;
    }
    // One chart for every line: the dates and values of all of them, each line in runs between
    // steps with no data, so a gap shows as one rather than as a straight join.
    let lines: Vec<Vec<Vec<(f64, f64)>>> = app
        .ws
        .series
        .iter()
        .map(|s| {
            // Read in place, not copied: the chart is drawn every frame.
            let dates = s["dates"].as_array().map(Vec::as_slice).unwrap_or(&[]);
            let values = s["values"].as_array().map(Vec::as_slice).unwrap_or(&[]);
            let mut runs: Vec<Vec<(f64, f64)>> = vec![Vec::new()];
            for (d, v) in dates.iter().zip(values) {
                match (d.as_f64(), v.as_f64()) {
                    (Some(d), Some(v)) => runs.last_mut().into_iter().for_each(|r| r.push((d, v))),
                    _ if runs.last().is_some_and(|r| !r.is_empty()) => runs.push(Vec::new()),
                    _ => {}
                }
            }
            runs.retain(|r| !r.is_empty());
            runs
        })
        .collect();
    let all = lines.iter().flatten().flatten();
    let (t0, t1) = all.clone().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), (d, _)| (a.min(*d), b.max(*d)));
    let (lo, hi) = all.fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), (_, v)| (a.min(*v), b.max(*v)));
    if !(t0.is_finite() && lo.is_finite()) {
        ui.label(egui::RichText::new(tl!("No data at the points picked.")).weak().small());
        return;
    }
    let (t1, hi) = (if t1 > t0 { t1 } else { t0 + 1.0 }, if hi > lo { hi } else { lo + 1.0 });
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 170.0), Sense::click());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, t.canvas);
    let plot = rect.shrink2(vec2(8.0, 18.0));
    let to = |(d, v): (f64, f64)| {
        pos2(plot.min.x + ((d - t0) / (t1 - t0)) as f32 * plot.width(), plot.max.y - ((v - lo) / (hi - lo)) as f32 * plot.height())
    };
    // The time slider's step.
    if let Some(sl) = app.session.map().time.as_ref()
        && (t0..=t1).contains(&sl.current)
    {
        let x = to((sl.current, lo)).x;
        painter.line_segment([pos2(x, plot.min.y), pos2(x, plot.max.y)], egui::Stroke::new(1.0, t.text_dim));
    }
    let colours = [
        t.accent,
        egui::Color32::from_rgb(214, 96, 77),
        egui::Color32::from_rgb(77, 146, 33),
        egui::Color32::from_rgb(118, 42, 131),
        egui::Color32::from_rgb(230, 171, 2),
        egui::Color32::from_rgb(27, 158, 119),
    ];
    for (i, runs) in lines.iter().enumerate() {
        let colour = colours.get(i % colours.len()).copied().unwrap_or(egui::Color32::GRAY);
        for run in runs {
            let pts: Vec<egui::Pos2> = run.iter().map(|p| to(*p)).collect();
            if let [point] = pts.as_slice() {
                painter.circle_filled(*point, 2.5, colour);
            } else {
                painter.add(egui::Shape::line(pts, egui::Stroke::new(1.6, colour)));
            }
        }
    }
    let fmt = |v: f64| rustgis_engine::map::symbology::fmt_num(v);
    // The units, when every line has the same (lines of other variables share the scale without them).
    let mut all_units = app.ws.series.iter().map(|s| s["units"].as_str().unwrap_or(""));
    let first_units = all_units.next().unwrap_or("");
    let units = if all_units.all(|u| u == first_units) { first_units.to_string() } else { String::new() };
    painter.text(rect.left_top() + vec2(6.0, 3.0), egui::Align2::LEFT_TOP, format!("{} {units}", fmt(hi)), regular(10.0), t.text_dim);
    painter.text(
        rect.left_bottom() + vec2(6.0, -3.0),
        egui::Align2::LEFT_BOTTOM,
        format!("{} {units}", fmt(lo)),
        regular(10.0),
        t.text_dim,
    );
    let day = |ms: f64| rustgis_engine::data::time::format(ms, "YYYY-MM-DD");
    painter.text(
        rect.right_bottom() + vec2(-6.0, -3.0),
        egui::Align2::RIGHT_BOTTOM,
        format!("{} – {}", day(t0), day(t1)),
        regular(10.0),
        t.text_dim,
    );
    // A click on the chart takes the time slider to that date.
    if resp.clicked()
        && app.session.map().time.is_some()
        && let Some(pos) = resp.interact_pointer_pos()
    {
        let ms = t0 + f64::from(((pos.x - plot.min.x) / plot.width().max(1.0)).clamp(0.0, 1.0)) * (t1 - t0);
        let _ = app.run("time.goTo", json!({"date": ms}));
    }
    // The points, in their lines' colours.
    for (i, s) in app.ws.series.iter().enumerate() {
        let colour = colours.get(i % colours.len()).copied().unwrap_or(egui::Color32::GRAY);
        let text = format!(
            "● {:.4}°, {:.4}° · {}",
            s["lon"].as_f64().unwrap_or(0.0),
            s["lat"].as_f64().unwrap_or(0.0),
            s["variable"].as_str().unwrap_or("")
        );
        ui.label(egui::RichText::new(text).small().color(colour));
    }
}

/// The Time Series pane's lines as CSV: one row a date and point.
#[cfg(not(target_arch = "wasm32"))]
fn series_csv(series: &[Value]) -> String {
    let mut out = String::from("lon,lat,variable,date,value\n");
    for s in series {
        let (lon, lat, var) = (s["lon"].as_f64().unwrap_or(0.0), s["lat"].as_f64().unwrap_or(0.0), s["variable"].as_str().unwrap_or(""));
        let labels = s["labels"].as_array().map(Vec::as_slice).unwrap_or(&[]);
        let values = s["values"].as_array().map(Vec::as_slice).unwrap_or(&[]);
        for (l, v) in labels.iter().zip(values) {
            let v = v.as_f64().map(|v| v.to_string()).unwrap_or_default();
            out.push_str(&format!("{lon},{lat},\"{}\",{},{v}\n", var.replace('"', "\"\""), l.as_str().unwrap_or("")));
        }
    }
    out
}

/// The Diagnostic Monitor.
pub fn diagnostics(app: &mut RustGisApp, ui: &mut Ui, _t: &Tokens) {
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
    let dt = ui.input(|i| i.unstable_dt).max(1e-4);
    let now = ui.input(|i| i.time);
    egui::Grid::new("diag").num_columns(2).striped(true).spacing(vec2(12.0, 4.0)).show(ui, |ui| {
        let mut row = |k: &str, v: String| {
            ui.label(egui::RichText::new(tl!(k)).small());
            ui.label(egui::RichText::new(v).small().monospace());
            ui.end_row();
        };
        row("Frame rate", format!("{:.0} fps", 1.0 / dt));
        row("Map draw", format!("{} ms", app.map.render_ms()));
        if app.ws.diagnostics.as_ref().is_none_or(|(t, _)| now - t > 1.0) {
            app.ws.diagnostics = Some((now, rustgis_engine::cmd::workspace::diagnostics(&app.session)));
        }
        if let Some((_, v)) = &app.ws.diagnostics
            && let Some(o) = v.as_object()
        {
            for (k, v) in o {
                row(k, v.to_string());
            }
        }
    });
}

/// Infographics: cards summarising what's in view.
pub fn infographics(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    ui.label(egui::RichText::new(tl!("Live summary of the features in view.")).small().color(t.text_dim));
    // Worked out again only when the view or the data change.
    let cam = app.session.map().camera;
    // The revision changes with every edit, undo and redo (the undo depth stops at its limit).
    let key = [cam.center.x, cam.center.y, cam.res, app.session.revision as f64];
    if app.ws.infographic.as_ref().is_none_or(|(k, _)| *k != key) {
        app.ws.infographic = Some((key, rustgis_engine::cmd::workspace::infographic_of(&app.session, None)));
    }
    let Some((_, v)) = app.ws.infographic.clone() else { return };
    for card in v["layers"].as_array().cloned().unwrap_or_default() {
        egui::Frame::NONE.fill(t.canvas).corner_radius(8).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new(card["layer"].as_str().unwrap_or("")).font(semibold(13.0)));
            ui.label(
                egui::RichText::new(crate::i18n::fmt("{n} features in view", &[("n", &card["count"].to_string())]))
                    .size(20.0)
                    .color(t.accent),
            );
            for n in card["numbers"].as_array().cloned().unwrap_or_default() {
                let sum = n["sum"].as_f64().map(rustgis_engine::map::symbology::fmt_num).unwrap_or_default();
                ui.label(
                    egui::RichText::new(crate::i18n::fmt(
                        "{field}: total {sum}",
                        &[("field", n["field"].as_str().unwrap_or("")), ("sum", &sum)],
                    ))
                    .small(),
                );
            }
            let top = card["top"].as_array().cloned().unwrap_or_default();
            let max = top.iter().filter_map(|x| x["count"].as_f64()).fold(1.0, f64::max);
            for x in top {
                ui.horizontal(|ui| {
                    let c = x["count"].as_f64().unwrap_or(0.0);
                    let (r, _) = ui.allocate_exact_size(vec2(80.0, 10.0), Sense::hover());
                    ui.painter().rect_filled(egui::Rect::from_min_size(r.min, vec2(80.0 * (c / max) as f32, 10.0)), 2.0, t.accent);
                    ui.label(egui::RichText::new(format!("{} ({c})", x["value"].as_str().unwrap_or(""))).small());
                });
            }
        });
        ui.add_space(6.0);
    }
}

/// The Browser view: the project's items as a grid of cards, in place of the map.
pub fn catalog_view(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    let p = &app.session.project;
    let mut items: Vec<(&str, String, &str, Option<Value>)> = Vec::new();
    for m in &p.maps {
        items.push(("map", m.name.clone(), "Map", Some(json!({"cmd": "map.activate", "name": m.name}))));
    }
    for l in &p.layouts {
        items.push(("new_layout", l.name.clone(), "Layout", Some(json!({"cmd": "layout.activate", "name": l.name}))));
    }
    for tb in &p.tables {
        items.push(("table", tb.name.clone(), "Table", None));
    }
    for s in &p.styles {
        items.push(("symbology", s.name.clone(), "Style", None));
    }
    for tk in &p.tasks {
        items.push(("run", tk.name.clone(), "Task", None));
    }
    for m in &p.models {
        items.push(("modelbuilder", m.name.clone(), "Model", Some(json!({"cmd": "ui.modelBuilder", "name": m.name}))));
    }
    for f in &p.folders {
        items.push(("add_folder", f.clone(), "Folder", None));
    }
    for a in &p.offline_areas {
        items.push(("map", a.name.clone(), "Offline area", Some(json!({"cmd": "offline.open", "name": a.name}))));
    }
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(tl!("Browser")).font(semibold(16.0)));
        if ui.button(tl!("Back to the Map")).clicked() {
            app.ws.catalog_view = false;
        }
    });
    ui.add_space(8.0);
    let mut open: Option<Value> = None;
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            for (icon, name, kind, action) in &items {
                let (r, resp) = ui.allocate_exact_size(vec2(150.0, 110.0), Sense::click());
                ui.painter().rect_filled(r, 8.0, if resp.hovered() { t.hover } else { t.canvas });
                crate::icons::paint(
                    ui.painter(),
                    egui::Rect::from_center_size(pos2(r.center().x, r.min.y + 36.0), vec2(40.0, 40.0)),
                    icon,
                    t.icon,
                    t.accent,
                );
                let short: String = name.chars().take(20).collect();
                ui.painter().text(pos2(r.center().x, r.min.y + 72.0), egui::Align2::CENTER_CENTER, short, semibold(12.0), t.text);
                ui.painter().text(pos2(r.center().x, r.min.y + 90.0), egui::Align2::CENTER_CENTER, tl!(*kind), regular(10.5), t.text_dim);
                let resp = if app.ws.item_popups { resp.on_hover_text(format!("{} · {name}", tl!(*kind))) } else { resp };
                if resp.double_clicked() {
                    open = action.clone();
                }
            }
        });
    });
    if let Some(a) = open {
        let cmd = a["cmd"].as_str().unwrap_or("").to_string();
        let _ = app.run(&cmd, json!({"name": a["name"]}));
        app.ws.catalog_view = false;
        if cmd == "layout.activate" {
            app.ui.tab = "Publish".into();
        }
        app.map.invalidate();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// The area under a profile is filled segment by segment: every triangle stays between two
    /// neighbouring samples and between the line and the base, however the line bends.
    #[test]
    fn a_profile_fills_under_its_line_only() {
        // Up, down below the start, up again: a concave outline, which a fan from its first point
        // would cross.
        let line = [pos2(0.0, 50.0), pos2(10.0, 10.0), pos2(20.0, 90.0), pos2(30.0, 40.0)];
        let mesh = area_under(&line, 100.0, Color32::WHITE);
        assert_eq!((mesh.vertices.len(), mesh.indices.len()), (12, 18));
        for tri in mesh.indices.chunks(3) {
            let v: Vec<egui::Pos2> = tri.iter().map(|&i| mesh.vertices[i as usize].pos).collect();
            let (x0, x1) = (v.iter().map(|p| p.x).fold(f32::MAX, f32::min), v.iter().map(|p| p.x).fold(f32::MIN, f32::max));
            assert!(x1 - x0 <= 10.0, "a triangle spans more than one segment: {v:?}");
            let seg = line.windows(2).find(|w| w[0].x == x0 && w[1].x == x1).unwrap();
            for p in &v {
                assert!(p.y <= 100.0 && p.y >= seg[0].y.min(seg[1].y), "{p:?} is outside {seg:?} and the base");
            }
        }
        // Its area is the sum of the trapezoids: (50+90)/2·10 + (90+10)/2·10 + (10+60)/2·10 (the
        // depth below each sample, the base at 100).
        let area: f32 = mesh
            .indices
            .chunks(3)
            .map(|t| {
                let [a, b, c] = [0, 1, 2].map(|k| mesh.vertices[t[k] as usize].pos);
                ((b.x - a.x) * (c.y - a.y) - (c.x - a.x) * (b.y - a.y)).abs() / 2.0
            })
            .sum();
        assert!((area - 1550.0).abs() < 1e-3, "{area}");
        assert!(area_under(&line[..1], 100.0, Color32::WHITE).vertices.is_empty());
    }

    #[test]
    fn the_step_being_edited_follows_a_move() {
        let mut app = RustGisApp::with_sample();
        app.ws.task_edit = Some((2, "Title".into(), "Text".into()));
        app.ws.task_step = 2;
        // Step 2 moves up: the text typed for it stays with it, now at 1.
        follow_move(&mut app, 2, 1);
        assert_eq!(app.ws.task_edit.as_ref().map(|e| e.0), Some(1));
        assert_eq!(app.ws.task_step, 1);
        // The step above moves down into its place: it moves up, out of the way, to 0.
        follow_move(&mut app, 0, 1);
        assert_eq!(app.ws.task_edit.as_ref().map(|e| e.0), Some(0));
        // Steps elsewhere don't touch it.
        follow_move(&mut app, 4, 5);
        assert_eq!(app.ws.task_edit.as_ref().map(|e| e.0), Some(0));
        // A move past it, several places at once, shifts it by one: step 0 to 3 moves it to 0's
        // old place's neighbour, and back.
        app.ws.task_edit = Some((2, "Title".into(), "Text".into()));
        follow_move(&mut app, 0, 3);
        assert_eq!(app.ws.task_edit.as_ref().map(|e| e.0), Some(1));
        follow_move(&mut app, 3, 0);
        assert_eq!(app.ws.task_edit.as_ref().map(|e| e.0), Some(2));
        // The edited step itself moving far goes where it went.
        follow_move(&mut app, 2, 5);
        assert_eq!(app.ws.task_edit.as_ref().map(|e| e.0), Some(5));
    }

    #[test]
    fn deleting_a_step_keeps_an_edit_on_another() {
        let mut app = RustGisApp::with_sample();
        // The task as follow_delete sees it, after each deletion (it only reads the count).
        let task = |app: &mut RustGisApp, n: usize| {
            let steps: Vec<_> = (0..n).map(|i| json!({"title": format!("Step {i}"), "command": "map.zoomFull", "params": {}})).collect();
            let _ = app.session.run("task.remove", &json!({"task": "Lesson"}));
            app.session.run("task.new", &json!({"name": "Lesson", "steps": steps})).unwrap();
            app.ws.task = "Lesson".into();
        };
        task(&mut app, 5);
        app.ws.task_edit = Some((3, "Title".into(), "Typed".into()));
        app.ws.task_step = 3;
        // An earlier step goes: the edit stays, one place up.
        task(&mut app, 4);
        follow_delete(&mut app, 1);
        assert_eq!(app.ws.task_edit.as_ref().map(|e| (e.0, e.2.as_str())), Some((2, "Typed")));
        assert_eq!(app.ws.task_step, 2);
        // A later step goes: nothing moves.
        task(&mut app, 3);
        follow_delete(&mut app, 3);
        assert_eq!(app.ws.task_edit.as_ref().map(|e| e.0), Some(2));
        // The step being edited goes: its edit closes.
        task(&mut app, 2);
        follow_delete(&mut app, 2);
        assert!(app.ws.task_edit.is_none());
        // The last step going leaves the current one on the step before it.
        app.ws.task_step = 1;
        task(&mut app, 1);
        follow_delete(&mut app, 1);
        assert_eq!(app.ws.task_step, 0);
    }

    /// The Time Series pane (#555): a point picked on the map reads the series under the Zarr
    /// layer chosen, the chart takes the slider to a date clicked, and Clear empties it.
    #[test]
    fn the_time_series_pane_reads_a_point() {
        use egui_kittest::kittest::Queryable;
        // A store of `t` over three monthly steps on a 3 × 4 grid (step k's values 100·k + i).
        let dir = std::env::temp_dir().join(format!("rustgis-ui-series-{}", std::process::id()));
        let store = dir.join("air.zarr");
        let _ = std::fs::remove_dir_all(&dir);
        for d in ["t", "lat", "lon", "time"] {
            std::fs::create_dir_all(store.join(d)).unwrap();
        }
        let arr = |shape: &str, dtype: &str, chunks: &str| json!({"shape": serde_json::from_str::<Value>(shape).unwrap(), "chunks": serde_json::from_str::<Value>(chunks).unwrap(), "dtype": dtype, "compressor": null, "fill_value": null, "filters": null, "order": "C", "zarr_format": 2});
        let meta = json!({"metadata": {
            ".zgroup": {"zarr_format": 2},
            "t/.zarray": arr("[3,3,4]", "<f4", "[1,3,4]"),
            "t/.zattrs": {"_ARRAY_DIMENSIONS": ["time","lat","lon"], "units": "K"},
            "lat/.zarray": arr("[3]", "<f4", "[3]"),
            "lat/.zattrs": {"_ARRAY_DIMENSIONS": ["lat"], "units": "degrees_north"},
            "lon/.zarray": arr("[4]", "<f4", "[4]"),
            "lon/.zattrs": {"_ARRAY_DIMENSIONS": ["lon"], "units": "degrees_east"},
            "time/.zarray": arr("[3]", "<i4", "[3]"),
            "time/.zattrs": {"_ARRAY_DIMENSIONS": ["time"], "units": "days since 2001-01-01"},
        }, "zarr_consolidated_format": 1});
        std::fs::write(store.join(".zmetadata"), meta.to_string()).unwrap();
        std::fs::write(store.join(".zgroup"), r#"{"zarr_format":2}"#).unwrap();
        let f32s = |v: &[f32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
        std::fs::write(store.join("lat/0"), f32s(&[10.0, 20.0, 30.0])).unwrap();
        std::fs::write(store.join("lon/0"), f32s(&[1.0, 2.0, 3.0, 4.0])).unwrap();
        std::fs::write(store.join("time/0"), [0i32, 31, 59].iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>()).unwrap();
        for k in 0..3 {
            let v: Vec<f32> = (0..12).map(|i| (k * 100 + i) as f32).collect();
            std::fs::write(store.join(format!("t/{k}.0.0")), f32s(&v)).unwrap();
        }
        let mut app = RustGisApp::with_sample();
        app.run("layer.addZarr", json!({"path": store.display().to_string(), "variable": "t", "time": true, "name": "air"})).unwrap();
        // A point picked at 1°E 30°N.
        app.workflow.pick =
            Some(crate::analysispane::Pick { purpose: "ts_point", needed: 1, points: vec![rustgis_engine::geom::Coord::new(1.0, 30.0)] });
        // Without the interface fonts (`theme::install_fonts`) egui's faces have no "●".
        let mut h = egui_kittest::Harness::builder().allow_missing_glyphs().with_size(egui::vec2(420.0, 600.0)).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let t = crate::theme::Tokens::get(ui.ctx());
                time_series(app, ui, &t);
            },
            app,
        );
        h.run_steps(3);
        assert_eq!(h.state().ws.series_layer, "air");
        assert_eq!(h.state().ws.series.len(), 1);
        assert_eq!(h.state().ws.series[0]["values"], json!([8.0, 108.0, 208.0]));
        assert!(h.state().workflow.pick.is_none(), "the pick is used up");
        // The CSV: one row a step.
        let csv = series_csv(&h.state().ws.series);
        assert_eq!(csv.lines().count(), 4, "{csv}");
        assert!(csv.lines().nth(2).is_some_and(|l| l.ends_with("2001-02-01 00:00,108")), "{csv}");
        // Clear empties the chart.
        h.get_by_label("Clear").click();
        h.run_steps(2);
        assert!(h.state().ws.series.is_empty());
        // Summarise over time: the mean of the layer's three steps, computed on a thread and added.
        h.get_by_label("Summarise over time").click();
        h.run_steps(2);
        h.get_by_label("Summarise").click();
        for _ in 0..500 {
            h.run_steps(1);
            if h.state().ws.reducing.is_none() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(h.state().ws.reducing.is_none(), "the summary lands");
        let mean = h
            .state()
            .session
            .map()
            .layer_by_name("t mean 2001-01-01–2001-03-01")
            .and_then(|l| l.raster.as_ref())
            .and_then(|r| r.data.clone());
        assert_eq!(mean.map(|d| d.bands[0].data[0]), Some(108.0));
        // A summary is no slice of the variable: Time Series doesn't offer it as one.
        let summary = h.state().session.map().layer_by_name("t mean 2001-01-01–2001-03-01").and_then(|l| l.raster.as_ref());
        assert_eq!(summary.and_then(|r| r.subdataset.clone()), None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
