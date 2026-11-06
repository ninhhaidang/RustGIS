//! Export Features, Export Raster, Export Map and Export Layout on the desktop write their file
//! on a thread of their own (#1291 step 3), so a large layer, raster or map doesn't hold the
//! window: the status bar shows the export under way, how far it has come and Cancel, and
//! [`land`] says what it wrote, as the command says when it runs here.

use std::sync::Arc;

use egui::Ui;
use rustgis_engine::analysis::control::Control;
use rustgis_engine::cmd::exports::{self, Wrote};
use serde_json::{Value, json};

use crate::RustGisApp;
use crate::theme::Tokens;

/// An export under way: what it is called, its Cancel and progress, and what it wrote once done.
pub struct ExportJob {
    started: exports::Started,
    name: &'static str,
    control: Arc<Control>,
    slot: crate::Slot<Result<Wrote, String>>,
}

/// Refuses another export while one runs, whichever way it would run (one at a time).
pub fn idle(app: &RustGisApp) -> Result<(), String> {
    match &app.export_job {
        Some(job) => Err(crate::i18n::fmt("{tool} is still running: wait for it, or cancel it.", &[("tool", tl_raw!(job.name))])),
        None => Ok(()),
    }
}

/// Starts `command` ([`exports::runs_in_background`]) with `p` on a thread of its own. One at a
/// time: a second is refused while the first runs.
pub fn start(app: &mut RustGisApp, command: &'static str, p: Value) -> Result<Value, String> {
    idle(app)?;
    let run = exports::export_start(&mut app.session, command, &p)?;
    let (started, name) = (run.started(), run.name);
    // Cancel stops it.
    let control = Control::stoppable();
    let (ctl, slot) = (control.clone(), crate::Slot::default());
    let out = slot.clone();
    // A streamed layer's source read on the thread keeps its requests in the session's diagnostics.
    let diagnostics = app.session.diagnostics.capture();
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let _diagnostics = diagnostics.enter();
            let wrote = run.run(&ctl);
            // Filled even when its lock is poisoned, or the export would show as running forever.
            *out.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(wrote);
        })
        .map_err(|e| e.to_string())?;
    app.export_job = Some(ExportJob { started, name, control, slot });
    Ok(json!({"started": name}))
}

/// Says what an export wrote once it's done (or why it failed): called every frame. `true` while
/// one is still running.
pub fn land(app: &mut RustGisApp) -> bool {
    let Some(job) = &app.export_job else { return false };
    let done = job.slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
    let Some(done) = done else { return true };
    let started = job.started.clone();
    app.export_job = None;
    // Its notices (a failure among them) are the command's; the answer was for a script, which
    // waits for its own.
    let _ = exports::export_land(&mut app.session, started, done);
    false
}

/// Cancels the export under way and waits up to `wait` for its thread to stop: the writers stop
/// within a moment and remove their temporary file. One that is still going then is left to the
/// process's end.
pub fn stop(app: &mut RustGisApp, wait: std::time::Duration) {
    let Some(job) = app.export_job.take() else { return };
    job.control.cancel();
    let end = std::time::Instant::now() + wait;
    while job.slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_none() && std::time::Instant::now() < end {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// The export under way in the status bar: its name, progress and Cancel.
pub fn status(app: &RustGisApp, ui: &mut Ui, t: &Tokens) {
    let Some(job) = &app.export_job else { return };
    ui.spinner();
    // Two labels, not one sentence put together from translated pieces.
    ui.label(egui::RichText::new(tl!("Running…")).size(11.5).color(t.text_dim));
    ui.label(egui::RichText::new(tl!(job.name)).size(11.5).strong());
    // How far its current step has come, when its writer counts.
    if let Some((done, total)) = job.control.progress() {
        ui.add(egui::ProgressBar::new(done as f32 / total.max(1) as f32).show_percentage().desired_width(120.0).desired_height(12.0));
    }
    if ui.add_enabled(!job.control.is_cancelled(), egui::Button::new(tl!("Cancel")).small()).clicked() {
        job.control.cancel();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn export_features_writes_on_a_thread_and_lands() {
        let mut app = RustGisApp::with_sample();
        app.session.active_layer = app.session.map().layer_by_name("Cities").map(|l| l.id);
        let dir = std::env::temp_dir().join(format!("rustgis-export-job-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (path, other) = (dir.join("cities.geojson"), dir.join("again.geojson"));
        let pick = |p: &std::path::Path| crate::dialogs::PICKED.with(|c| *c.borrow_mut() = Some(p.display().to_string()));
        pick(&path);
        assert_eq!(app.run("ui.exportLayer", json!({"format": "geojson"})).unwrap()["started"], "Export Features");
        // One at a time: a second waits for the first.
        pick(&other);
        assert!(app.run("ui.exportLayer", json!({"format": "geojson"})).err().unwrap().contains("still running"));
        let end = std::time::Instant::now() + std::time::Duration::from_secs(60);
        // Another layer picked while it writes: the export still lands, for the layer it started on.
        app.session.active_layer = app.session.map().layer_by_name("Countries").map(|l| l.id);
        while land(&mut app) {
            assert!(std::time::Instant::now() < end, "the export never landed");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(app.export_job.is_none());
        let exported = format!("Exported {}", path.display());
        assert!(app.session.notices.iter().any(|n| n.text == exported && !n.error), "{:?}", app.session.notices.last());
        assert!(!other.exists(), "the refused export wrote nothing");
        let (written, _) = rustgis_engine::data::io::read_path(&path).unwrap();
        assert_eq!(written.features.len(), app.session.map().layer_by_name("Cities").unwrap().dataset.features.len());
        // Closing the app cancels an export under way and waits for it: no temporary file is left.
        pick(&other);
        app.run("ui.exportLayer", json!({"format": "geojson"})).unwrap();
        stop(&mut app, std::time::Duration::from_secs(60));
        assert!(app.export_job.is_none());
        let left: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name()).collect();
        assert!(left.iter().all(|n| !n.to_string_lossy().ends_with(".tmp")), "{left:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn export_map_and_export_layout_draw_on_a_thread() {
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"page": "a4", "template": "map"})).unwrap();
        let dir = std::env::temp_dir().join(format!("rustgis-export-drawn-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (command, name, file) in [("ui.exportMap", "Export Map", "map.png"), ("ui.exportLayout", "Export Layout", "layout.png")] {
            let path = dir.join(file);
            crate::dialogs::PICKED.with(|c| *c.borrow_mut() = Some(path.display().to_string()));
            assert_eq!(app.run(command, Value::Null).unwrap()["started"], name);
            let end = std::time::Instant::now() + std::time::Duration::from_secs(120);
            while land(&mut app) {
                assert!(std::time::Instant::now() < end, "{name} never landed");
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            let exported = format!("Exported {}", path.display());
            assert!(app.session.notices.iter().any(|n| n.text == exported && !n.error), "{:?}", app.session.notices.last());
            assert!(std::fs::metadata(&path).is_ok_and(|m| m.len() > 100), "{file}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
