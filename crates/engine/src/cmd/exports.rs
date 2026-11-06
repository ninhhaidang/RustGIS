//! Exports that write a file from what the session shares (a layer's features, a raster's
//! pixels, a copy of the map or project to draw). A script or the command line writes it where
//! the command is called; the app writes it on a thread of its own with progress and Cancel
//! (#1291 step 3): [`export_start`] reads the session, [`ExportRun::run`] writes the file, and
//! [`export_land`] says what was written, as the command would have. It lands whatever layer is
//! active or project is open by then: the file is written either way, and an export changes
//! nothing in the project.

use serde_json::Value;

use crate::{CmdResult, Session};

/// What an export wrote: the command's answer, and what it says of the file (the last says it
/// was exported, unless `then` says it).
pub struct Wrote {
    pub(crate) answer: Value,
    pub(crate) notes: Vec<String>,
    /// What it does to the session once written, after its notes, and adds to its answer (a
    /// GeoPackage table replaced: the layers reading it read it again).
    pub(crate) then: Option<Then>,
}

/// What an export does to the session once its file is written, and adds to its answer; or the
/// failure it says once what came before it was said (a layout's warnings, then its error).
pub(crate) type Then = Box<dyn FnOnce(&mut Session, &mut Value) -> Result<(), String> + Send>;

/// The writing of an export, its inputs read: it needs no session.
pub(crate) type Work = Box<dyn FnOnce() -> Result<Wrote, String> + Send>;

/// What an export reads of the session, and the work that writes its file.
type Start = fn(&Session, &Value) -> Result<Work, String>;

/// The exports the app writes on another thread ([`export_start`]), each with what it is called
/// (in its progress, and a cancelled run's error) and what it reads of the session.
const IN_BACKGROUND: &[(&str, &str, Start)] = &[
    ("share.exportLayer", "Export Features", super::share::layer_work),
    ("share.exportRaster", "Export Raster", super::raster::export_work),
    ("share.exportMap", "Export Map", super::share::map_work),
    ("layout.export", "Export Layout", super::layout::export_work),
];

/// `command` and what it is called, when the app writes its file on another thread (see
/// [`export_start`]).
pub fn runs_in_background(command: &str) -> Option<(&'static str, &'static str)> {
    IN_BACKGROUND.iter().find(|(c, ..)| *c == command).map(|(c, name, _)| (*c, *name))
}

/// `command`'s export written here: it says what it wrote.
pub(crate) fn write_here(s: &mut Session, p: &Value, command: &str) -> CmdResult {
    let &(_, name, start) = IN_BACKGROUND.iter().find(|(c, ..)| *c == command).ok_or_else(|| format!("{command} isn't an export"))?;
    let work = start(s, p)?;
    // Where a person at a terminal can stop it with Ctrl-C, as on the app's thread.
    let wrote = if s.stoppable { written_under(name, &rustgis_analysis::control::Control::stoppable(), work)? } else { work()? };
    said(s, wrote)
}

/// `work` under `control`, as [`rustgis_analysis::run_under`] runs it, except that a file once
/// placed is written: a Cancel that comes after the last step doesn't turn it into a failure.
fn written_under(name: &str, control: &std::sync::Arc<rustgis_analysis::control::Control>, work: Work) -> Result<Wrote, String> {
    let mut wrote = None;
    let res = rustgis_analysis::run_under(name, control, || work().map(|w| wrote = Some(w)));
    match wrote {
        Some(w) => Ok(w),
        None => Err(res.err().unwrap_or_else(|| format!("{name} wrote nothing"))),
    }
}

/// Says what an export wrote, and gives its answer (or the failure its `then` says).
fn said(s: &mut Session, wrote: Wrote) -> CmdResult {
    let Wrote { mut answer, notes, then } = wrote;
    for note in notes {
        s.notify(note, false);
    }
    if let Some(then) = then {
        then(s, &mut answer)?;
    }
    Ok(answer)
}

/// Lands an export drawn on a copy of the map or project (Export Map, Export Layout): what
/// drawing it said, with each notice's severity; the streamed layers whose source failed on the
/// copy, marked on their live layers as drawing them there would have (or, where that layer is
/// gone or opened afresh, said in one notice to be missing from the file); then that it was
/// written to `path`, or why not. A cancelled one says only that it was cancelled.
pub(crate) fn drawn(
    maps: &[rustgis_map::Map],
    notices: Vec<(rustgis_data::diagnostics::Severity, String)>,
    written: Result<Value, String>,
    path: String,
) -> Result<Wrote, String> {
    if let (Err(e), Err(_)) = (&written, rustgis_data::progress::check()) {
        return Err(e.clone());
    }
    // Each by its live layer, with the source it failed on: one the user removed, or opened
    // afresh meanwhile (`SourceLayer::same_opening`), is left as it is.
    let failed: Vec<_> = maps
        .iter()
        .flat_map(|m| &m.layers)
        .filter_map(|l| {
            let src = l.source.as_ref()?;
            Some((l.instance_id(), l.name.clone(), src.error.clone()?, src.clone()))
        })
        .collect();
    let answer = written.as_ref().map_or(Value::Null, Value::clone);
    let then: Then = Box::new(move |s: &mut Session, _: &mut Value| {
        use rustgis_data::diagnostics::Severity;
        for (severity, text) in notices {
            match severity {
                Severity::Error => s.notify(text, true),
                Severity::Warning => s.warn(text),
                Severity::Info => s.notify(text, false),
            }
        }
        let mut missing = Vec::new();
        for (instance, name, error, copied) in failed {
            let live = s.project.maps.iter_mut().flat_map(|m| m.layers.iter_mut()).find(|l| l.instance_id() == instance);
            match live.and_then(|l| l.source.as_mut()) {
                // The same open: its failure shows on the layer (it may already).
                Some(src) if src.same_opening(&copied) => {
                    src.error.get_or_insert(error);
                }
                _ => missing.push((name, error)),
            }
        }
        // One notice for them all, with a count, naming the first.
        if let Some((name, error)) = missing.first() {
            let n = missing.len();
            s.warn(if n == 1 {
                format!("{path}: {name} isn't in the file, as its source couldn't be read: {error}")
            } else {
                format!("{path}: {n} layers aren't in the file, as their sources couldn't be read; the first, {name}: {error}")
            });
        }
        written?;
        s.notify(format!("Exported {path}"), false);
        Ok(())
    });
    Ok(Wrote { answer, notes: Vec::new(), then: Some(then) })
}

#[cfg(not(target_arch = "wasm32"))]
/// An export ([`IN_BACKGROUND`]) started to write its file on another thread ([`ExportRun::run`])
/// while the session goes on: its inputs are taken now (cheaply: the features and pixels are
/// shared, and a streamed layer's source is read on the export's thread), and [`export_land`]
/// says what it wrote.
pub struct ExportRun {
    /// What the export is called, for its progress and a cancelled run's error.
    pub name: &'static str,
    started: Started,
    work: Work,
}

#[cfg(not(target_arch = "wasm32"))]
/// The command an export started on another thread runs, with what and when: [`export_land`]
/// says what it wrote as that command, and puts it in the session's log as `Session::run` does.
#[derive(Clone)]
pub struct Started {
    pub(crate) command: &'static str,
    pub(crate) params: Value,
    pub(crate) at: u64,
    pub(crate) start: web_time::Instant,
}

#[cfg(not(target_arch = "wasm32"))]
impl ExportRun {
    /// What [`export_land`] needs of it once it has run.
    pub fn started(&self) -> Started {
        self.started.clone()
    }

    /// Writes the file, blocking until it's written; `control` stops it and counts its progress
    /// (see [`rustgis_analysis::control`]). A cancelled export leaves no file, or the one there
    /// before.
    pub fn run(self, control: &std::sync::Arc<rustgis_analysis::control::Control>) -> Result<Wrote, String> {
        let ExportRun { name, work, .. } = self;
        // A bug in a writer must not take the app down.
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| written_under(name, control, work)))
            .unwrap_or_else(|_| Err(format!("{name} failed unexpectedly")))
    }
}

#[cfg(not(target_arch = "wasm32"))]
/// Starts `command` (one of [`IN_BACKGROUND`]'s) with `p`, to write its file on another thread:
/// its parameters checked and its inputs taken now.
pub fn export_start(s: &mut Session, command: &str, p: &Value) -> Result<ExportRun, String> {
    let &(command, name, start) =
        IN_BACKGROUND.iter().find(|(c, ..)| *c == command).ok_or_else(|| format!("{command} doesn't run in the background"))?;
    let started = Started { command, params: p.clone(), at: crate::project_log::now_ms(), start: web_time::Instant::now() };
    Ok(ExportRun { name, started, work: start(s, p)? })
}

#[cfg(not(target_arch = "wasm32"))]
/// Says what [`ExportRun::run`] wrote (or why it failed), as its command would have, and gives
/// its answer: the troubleshooting history files what it says (its failure too) under that
/// command, and the session's log has its entry, as when it runs here.
pub fn export_land(s: &mut Session, started: Started, done: Result<Wrote, String>) -> CmdResult {
    let Started { command, params, at, start } = started;
    let before = s.diagnostic_source.replace(command);
    let mark = s.notice_mark();
    let landed = done.and_then(|wrote| said(s, wrote));
    // Its failure said once, under the command, as `Session::run` says one.
    if let Err(e) = &landed {
        let text = Session::redacted(e);
        if !s.notices_since(mark).iter().any(|n| n.error && n.text == text.as_str()) {
            s.notify_redacted(text, true);
        }
    }
    s.diagnostic_source = before;
    if s.log.on() && crate::project_log::is_logged(s, command, &params) {
        let entry = crate::project_log::entry(command, crate::project_log::kept_params(&params), &landed, at, start.elapsed());
        s.log.push(entry);
    }
    landed
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use rustgis_analysis::control::Control;

    #[test]
    fn a_cancel_after_the_file_is_placed_leaves_it_written() {
        let control = Control::new();
        let late = control.clone();
        // The work places its file, then Cancel comes before the run returns.
        let work: Work = Box::new(move || {
            late.cancel();
            Ok(Wrote { answer: serde_json::json!({"path": "x"}), notes: vec!["Exported x".into()], then: None })
        });
        let wrote = written_under("Export Raster", &control, work).unwrap();
        assert_eq!(wrote.notes, ["Exported x"]);
        // A writer that stopped for the cancel is the cancelled run's error.
        let control = Control::new();
        let stopped = control.clone();
        let work: Work = Box::new(move || {
            stopped.cancel();
            Err("cancelled".into())
        });
        assert!(written_under("Export Raster", &control, work).err().unwrap().contains("was cancelled"));
    }
}
