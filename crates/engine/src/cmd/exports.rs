//! Exports that write a file from what the session shares (a layer's features, a raster's
//! pixels). A script or the command line writes it where the command is called; the app writes it
//! on a thread of its own with progress and Cancel (#1291 step 3): [`export_start`] reads the
//! session, [`ExportRun::run`] writes the file, and [`export_land`] says what was written, as the
//! command would have. It lands whatever layer is active or project is open by then: the file is
//! written either way, and an export changes nothing in the project.

use serde_json::Value;

use crate::{CmdResult, Session};

/// What an export wrote: the command's answer, and what it says of the file (the last says it
/// was exported).
pub struct Wrote {
    pub(crate) answer: Value,
    pub(crate) notes: Vec<String>,
    /// What it does to the session once written, after its notes, and adds to its answer (a
    /// GeoPackage table replaced: the layers reading it read it again).
    pub(crate) then: Option<Then>,
}

/// What an export does to the session once its file is written, and adds to its answer.
pub(crate) type Then = Box<dyn FnOnce(&mut Session, &mut Value) + Send>;

/// The writing of an export, its inputs read: it needs no session.
pub(crate) type Work = Box<dyn FnOnce() -> Result<Wrote, String> + Send>;

/// What an export reads of the session, and the work that writes its file.
type Start = fn(&Session, &Value) -> Result<Work, String>;

/// The exports the app writes on another thread ([`export_start`]), each with what it is called
/// (in its progress, and a cancelled run's error) and what it reads of the session.
const IN_BACKGROUND: &[(&str, &str, Start)] = &[
    ("share.exportLayer", "Export Features", super::share::layer_work),
    ("share.exportRaster", "Export Raster", super::raster::export_work),
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
    Ok(said(s, wrote))
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

/// Says what an export wrote, and gives its answer.
fn said(s: &mut Session, wrote: Wrote) -> Value {
    let Wrote { mut answer, notes, then } = wrote;
    for note in notes {
        s.notify(note, false);
    }
    if let Some(then) = then {
        then(s, &mut answer);
    }
    answer
}

#[cfg(not(target_arch = "wasm32"))]
/// An export ([`IN_BACKGROUND`]) started to write its file on another thread ([`ExportRun::run`])
/// while the session goes on: its inputs are taken now (cheaply: the features and pixels are
/// shared, and a streamed layer's source is read on the export's thread), and [`export_land`]
/// says what it wrote.
pub struct ExportRun {
    /// What the export is called, for its progress and a cancelled run's error.
    pub name: &'static str,
    work: Work,
}

#[cfg(not(target_arch = "wasm32"))]
impl ExportRun {
    /// Writes the file, blocking until it's written; `control` stops it and counts its progress
    /// (see [`rustgis_analysis::control`]). A cancelled export leaves no file, or the one there
    /// before.
    pub fn run(self, control: &std::sync::Arc<rustgis_analysis::control::Control>) -> Result<Wrote, String> {
        let ExportRun { name, work } = self;
        // A bug in a writer must not take the app down.
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| written_under(name, control, work)))
            .unwrap_or_else(|_| Err(format!("{name} failed unexpectedly")))
    }
}

#[cfg(not(target_arch = "wasm32"))]
/// Starts `command` (one of [`IN_BACKGROUND`]'s) with `p`, to write its file on another thread:
/// its parameters checked and its inputs taken now.
pub fn export_start(s: &mut Session, command: &str, p: &Value) -> Result<ExportRun, String> {
    let &(_, name, start) =
        IN_BACKGROUND.iter().find(|(c, ..)| *c == command).ok_or_else(|| format!("{command} doesn't run in the background"))?;
    Ok(ExportRun { name, work: start(s, p)? })
}

#[cfg(not(target_arch = "wasm32"))]
/// Says what [`ExportRun::run`] wrote, as its command would have, and gives its answer.
pub fn export_land(s: &mut Session, wrote: Wrote) -> Value {
    said(s, wrote)
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
