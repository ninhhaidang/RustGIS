//! Exports that write a file from what the session shares (a layer's features, a raster's
//! pixels, a copy of the map or project to draw). A script or the command line writes it where
//! the command is called; the app writes it on a thread of its own with progress and Cancel
//! (#1291 step 3): [`export_start`] reads the session, [`ExportRun::run`] writes the file, and
//! [`export_land`] says what was written, as the command would have. The admitted operation owns
//! preparation, file commit and delivery; a completed write remains written even if delivery
//! is subsequently refused.

use serde_json::Value;

use crate::{CmdResult, Session};

/// What an export wrote: the command's answer, and what it says of the file (the last says it
/// was exported, unless `then` says it).
pub struct Wrote {
    pub(crate) answer: Value,
    /// What it says once written, each loss with its code.
    pub(crate) notes: rustgis_data::loss::Notes,
    /// Drawing diagnostics available before the session finalizer, with their original severity.
    pub(crate) diagnostics: Vec<(rustgis_data::diagnostics::Severity, String)>,
    /// Source failures to report if delivery cannot update their live layers.
    pub(crate) refused_notes: Vec<String>,
    /// A physical write completed, even if authority to deliver its result is later refused.
    pub(crate) written_path: Option<String>,
    /// What it does to the session once written, after its notes, and adds to its answer (a
    /// GeoPackage table replaced: the layers reading it read it again).
    pub(crate) then: Option<Then>,
    /// Authority retained for delivery from the native export worker.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) owner: Option<crate::policy::Continuation>,
    /// The coordinate systems the worker thread used, as (definition, label), for the session to
    /// say which needed a missing datum grid (`Session::say_grid_notes`).
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) crs_used: Vec<(String, String)>,
}

impl Wrote {
    fn completion_notes(&self) -> Vec<String> {
        self.notes
            .texts()
            .chain(self.diagnostics.iter().map(|(_, text)| text.as_str()))
            .chain(self.refused_notes.iter().map(String::as_str))
            .map(str::to_string)
            .collect()
    }
}

/// Completes an export on the live session and may update its answer. A field package's finalizer
/// checks that its home still accepts the new area or identity before installing the staged file.
pub(crate) type Then = Box<dyn FnOnce(&mut Session, &mut Value) -> Result<(), String> + Send>;

/// The writing of an export, its inputs read: it needs no session.
pub(crate) type Work = Box<dyn FnOnce() -> Result<Wrote, String> + Send>;

/// What an export reads of the session, and the work that writes its file.
type Start = fn(&Session, &Value) -> Result<Work, String>;

/// The exports the app writes on another thread ([`export_start`]), each with what it is called
/// (in its progress, and a cancelled run's error) and what it reads of the session.
pub(crate) const IN_BACKGROUND: &[(&str, &str, Start)] = &[
    ("data.convert", "Data Interop", super::workflows::convert_work),
    ("share.exportLayer", "Export Features", super::share::layer_work),
    ("share.exportTable", "Export Table", super::share::table_work),
    ("share.exportRaster", "Export Raster", super::raster::export_work),
    ("share.exportMap", "Export Map", super::share::map_work),
    ("map.exportOrbit", "Export Orbit", super::view::orbit_work),
    ("map.exportBlock", "Export Block", super::block_model::export_work),
    ("layout.export", "Export Layout", super::layout::export_work),
    ("report.export", "Export Report", super::report::export_work),
    ("source.publish", "Publish to Source", super::publish::publish_work),
    #[cfg(not(target_arch = "wasm32"))]
    ("offline.package", "Package for the Field", super::field::export_work),
];

/// `command` and what it is called, when the app writes its file on another thread (see
/// [`export_start`]).
pub fn runs_in_background(command: &str) -> Option<(&'static str, &'static str)> {
    IN_BACKGROUND.iter().find(|(c, ..)| *c == command).map(|(c, name, _)| (*c, *name))
}

/// `command`'s export written here: it says what it wrote.
pub(crate) fn write_here(s: &mut Session, p: &Value, command: &str) -> CmdResult {
    let &(_, name, start) = IN_BACKGROUND.iter().find(|(c, ..)| *c == command).ok_or_else(|| format!("{command} isn't an export"))?;
    let owner = s.continuation()?;
    owner.check()?;
    let current = owner.clone();
    let still = std::sync::Arc::new(move || current.check().is_ok());
    // The probe keeps authority checks at file commit even when the user cannot cancel this run.
    let control = if s.stoppable { rustgis_analysis::control::Control::stoppable() } else { rustgis_analysis::control::Control::new() };
    let wrote = crate::render::rustgis_tiles::cache::with_still(still, || {
        let work = start(s, p)?;
        written_under(name, &control, work)
    })?;
    s.check_continuation(&owner)
        .map_err(|why| completion_with_notes(command, wrote.written_path.as_deref(), &wrote.completion_notes(), why))?;
    said(s, wrote)
}

/// Keeps a completed file visible when its result cannot be delivered to the session.
pub(crate) fn completion_refused(written_path: Option<&str>, why: String) -> String {
    match written_path {
        Some(path) => format!("Exported {path}; completion was refused: {why}"),
        None => why,
    }
}

/// A committed export's format notices remain part of its refusal diagnostic; its finalizer
/// stays blocked, so reporting the write cannot update a revoked layer's record.
fn completion_with_notes(command: &str, written_path: Option<&str>, notes: &[String], why: String) -> String {
    let Some(path) = written_path else { return why };
    let mut message = if command == "source.publish" {
        format!("Published {path}; completion was refused: {why}; the layer's record wasn't updated; add the file again to compare it")
    } else {
        completion_refused(Some(path), why)
    };
    for note in notes {
        message.push('\n');
        message.push_str(note);
    }
    message
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
    let Wrote { mut answer, notes, diagnostics, then, written_path, .. } = wrote;
    let mark = s.notice_mark();
    say(s, diagnostics);
    if written_path.is_some() {
        s.retain_written_notices(mark);
    }
    for note in notes {
        if written_path.is_some() {
            s.say_written(note);
        } else {
            s.say(note);
        }
    }
    if let Some(then) = then {
        rustgis_data::provenance::without(|| then(s, &mut answer))?;
    }
    Ok(answer)
}

/// Lands an export drawn on a copy of the map or project (Export Map, Export Layout): what
/// drawing it said, with each notice's severity; the streamed layers whose source failed on the
/// copy, reported for this export even when the screen already said them. Matching live layers
/// keep the failure marked said so subsequent screen frames do not repeat it;
/// then that it was written to `path`, or why not. A cancelled one says only that it was cancelled.
pub(crate) fn drawn(
    maps: &[rustgis_map::Map],
    mut notices: Vec<(rustgis_data::diagnostics::Severity, String)>,
    written: Result<Value, String>,
    path: String,
) -> Result<Wrote, String> {
    if let (Err(e), Err(_)) = (&written, rustgis_data::progress::check()) {
        return Err(e.clone());
    }
    // Held layers' places the drawing couldn't give in longitude/latitude ([`rustgis_map::Layer::unplaced`]):
    // drawn without them, and said of this export, as the screen says them of the map.
    let (mut unplaced, mut partly, mut undrawn) = (0usize, Vec::new(), Vec::new());
    for m in maps {
        for l in &m.layers {
            let named = format!("map '{}', layer '{}'", m.name, l.name);
            // Only the layers this export drew: a hidden one, or a map the layout doesn't
            // show, keeps a count from an earlier drawing.
            match l.unplaced_drawn() {
                Some(Ok(n)) if *n > 0 => {
                    unplaced += n;
                    partly.push(format!("{named} ({n})"));
                }
                Some(Err(e)) => undrawn.push(format!("{named}: {e}")),
                _ => {}
            }
        }
    }
    if written.is_ok() {
        let warn = rustgis_data::diagnostics::Severity::Warning;
        if unplaced > 0 {
            notices.push((
                warn,
                format!(
                    "{path}: {unplaced} feature(s) have a place that can't be given in longitude/latitude, drawn without it ({})",
                    crate::listed(&partly)
                ),
            ));
        }
        if !undrawn.is_empty() {
            notices.push((
                warn,
                format!(
                    "{path}: {} layer(s) aren't drawn, as their system can't be given in longitude/latitude: {}",
                    undrawn.len(),
                    crate::listed(&undrawn)
                ),
            ));
        }
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
    // Every output needs its own counted notice, independent of screen suppression.
    use crate::notice_text::NoticeText;
    let mut source_errors = Vec::new();
    let mut linked = 0;
    for (_, name, error, source) in &failed {
        let path = source.linked_file();
        linked += usize::from(path.is_some());
        if source_errors.len() < 5 {
            source_errors.push(match path {
                Some(path) => NoticeText::format(
                    "{name}: {path} ({error})",
                    [("name", name.clone().into()), ("path", path.to_string().into()), ("error", error.clone().into())],
                ),
                None => NoticeText::format("{name}: {error}", [("name", name.clone().into()), ("error", error.clone().into())]),
            });
        }
    }
    let source_notice = NoticeText::source_errors(failed.len(), source_errors, linked)
        .map(|message| NoticeText::format("{path}: {message}", [("path", path.clone().into()), ("message", message)]));
    let refused_notes = source_notice.iter().map(NoticeText::english).collect();
    let written_path = written.is_ok().then(|| path.clone());
    let answer = written.as_ref().map_or(Value::Null, Value::clone);
    let then: Then = Box::new(move |s: &mut Session, _: &mut Value| {
        for (instance, _, error, copied) in failed {
            let live = s.project.maps.iter_mut().flat_map(|m| m.layers.iter_mut()).find(|l| l.instance_id() == instance);
            if let Some(src) = live.and_then(|l| l.source.as_mut())
                && src.same_opening(&copied)
            {
                src.error.get_or_insert(error);
                // The export reports this failure below; the screen need not say it again.
                if src.error == copied.error {
                    let _ = src.error_unsaid();
                }
            }
        }
        let mark = s.notice_mark();
        if let Some(message) = source_notice {
            s.notify_text(message, true);
        }
        if written.is_ok() {
            s.retain_written_notices(mark);
        }
        // Background exports land without Session::run's display diagnostics. Source failures
        // already reported above retain their screen markers, so this cannot duplicate them.
        s.notice_landed_pixels();
        written?;
        s.notify_written(format!("Exported {path}"));
        Ok(())
    });
    Ok(Wrote {
        answer,
        notes: Default::default(),
        diagnostics: notices,
        refused_notes,
        written_path,
        then: Some(then),
        #[cfg(not(target_arch = "wasm32"))]
        owner: None,
        #[cfg(not(target_arch = "wasm32"))]
        crs_used: Vec::new(),
    })
}

/// What drawing an export on `copy` (a copy of the session) said, each notice with its severity,
/// to say again where the export lands; past the notices a session keeps, how many earlier ones
/// there were. `path` names the file in that count.
pub(crate) fn copy_notices(copy: &mut Session, path: &str) -> Vec<(rustgis_data::diagnostics::Severity, String)> {
    copy_notices_coded(copy, path).into_iter().map(|(severity, note)| (severity, note.text)).collect()
}

/// [`copy_notices`] with each notice's loss code, for an export that says them itself
/// ([`say_coded`]).
pub(crate) fn copy_notices_coded(copy: &mut Session, path: &str) -> Vec<(rustgis_data::diagnostics::Severity, rustgis_data::loss::Note)> {
    let kept = std::mem::take(&mut copy.notices);
    let earlier = copy.notices_given().saturating_sub(kept.len() as u64);
    let mut said = Vec::new();
    if earlier > 0 {
        said.push((
            rustgis_data::diagnostics::Severity::Warning,
            rustgis_data::loss::Note::plain(format!("{path}: {earlier} earlier notices from drawing it aren't shown")),
        ));
    }
    said.extend(kept.into_iter().map(|n| (n.severity, rustgis_data::loss::Note { loss: n.loss(), text: n.text })));
    said
}

/// [`say`] for notices with their loss codes: one with a code is said as that loss.
pub(crate) fn say_coded(s: &mut Session, notices: Vec<(rustgis_data::diagnostics::Severity, rustgis_data::loss::Note)>) {
    use rustgis_data::diagnostics::Severity;
    for (severity, note) in notices {
        if note.loss.is_some() {
            s.say(note);
            continue;
        }
        match severity {
            Severity::Error => s.notify(note.text, true),
            Severity::Warning => s.warn(note.text),
            Severity::Info => s.notify(note.text, false),
        }
    }
}

/// Says `notices` (what an export said on a copy of the session) in `s`, each as it was said.
pub(crate) fn say(s: &mut Session, notices: Vec<(rustgis_data::diagnostics::Severity, String)>) {
    use rustgis_data::diagnostics::Severity;
    for (severity, text) in notices {
        match severity {
            Severity::Error => s.notify(text, true),
            Severity::Warning => s.warn(text),
            Severity::Info => s.notify(text, false),
        }
    }
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
    /// The admitted invocation; cloned metadata never selects replacement authority.
    pub(crate) owner: crate::policy::Continuation,
    /// The project it started in, its name as the log keeps it and its `Session::opened`: the
    /// log's entry names that one, not one opened while it ran.
    pub(crate) project: (String, u64),
    /// Shared with the worker so failed exports retain the reads preceding the error, too.
    inputs: std::sync::Arc<std::sync::Mutex<Vec<rustgis_data::provenance::Fingerprint>>>,
    capture_inputs: bool,
}

#[cfg(not(target_arch = "wasm32"))]
impl ExportRun {
    /// What [`export_land`] needs of it once it has run.
    pub fn started(&self) -> Started {
        self.started.clone()
    }

    /// Writes the file, or stages it if installing it needs the live session. `control` stops the
    /// work and counts its progress (see [`rustgis_analysis::control`]). A cancelled export leaves
    /// no file, or the one there before.
    pub fn run(self, control: &std::sync::Arc<rustgis_analysis::control::Control>) -> Result<Wrote, String> {
        let inputs = self.started.capture_inputs.then(rustgis_data::provenance::Capture::start);
        let ExportRun { name, work, started } = self;
        let owner = started.owner;
        owner.check()?;
        let current = owner.clone();
        let still = std::sync::Arc::new(move || current.check().is_ok());
        // A bug in a writer must not take the app down.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::render::rustgis_tiles::cache::with_still(still, || written_under(name, control, work))
        }))
        .unwrap_or_else(|_| Err(format!("{name} failed unexpectedly")))
        .map(|w| Wrote { owner: Some(owner), crs_used: rustgis_data::raster::crs::take_used(), ..w });
        if let Some(inputs) = inputs {
            started.inputs.lock().unwrap_or_else(|e| e.into_inner()).extend(inputs.inputs());
        }
        result
    }
}

#[cfg(not(target_arch = "wasm32"))]
/// Starts `command` (one of [`IN_BACKGROUND`]'s) with `p`, to write its file on another thread:
/// its parameters checked and its inputs taken now.
pub fn export_start(s: &mut Session, command: &str, p: &Value) -> Result<ExportRun, String> {
    s.start_export(command, p)
}

#[cfg(not(target_arch = "wasm32"))]
/// Prepares only a registered, admitted export and seals its initiating authority.
pub(crate) fn export_start_raw(s: &mut Session, op: crate::policy::Authorized<crate::policy::CommandOp>) -> Result<ExportRun, String> {
    let crate::policy::CommandOp::Registered { spec, params } = op.op() else { return Err("not a registered export".into()) };
    let inputs = s.log.on().then(rustgis_data::provenance::Capture::start);
    let (command, p) = (spec.id, params);
    let &(_, name, start) =
        IN_BACKGROUND.iter().find(|(c, ..)| *c == command).ok_or_else(|| format!("{command} doesn't run in the background"))?;
    let owner = s.continuation()?;
    owner.check()?;
    let current = owner.clone();
    let still = std::sync::Arc::new(move || current.check().is_ok());
    let project = (crate::project_log::kept_name(&s.title()), s.opened);
    let started = Started {
        command,
        params: p.clone(),
        at: crate::project_log::now_ms(),
        start: web_time::Instant::now(),
        owner,
        project,
        inputs: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        capture_inputs: s.log.on(),
    };
    let work = crate::render::rustgis_tiles::cache::with_still(still, || start(s, p))?;
    if let Some(inputs) = inputs {
        *started.inputs.lock().unwrap_or_else(|e| e.into_inner()) = inputs.inputs();
    }
    Ok(ExportRun { name, started, work })
}

#[cfg(not(target_arch = "wasm32"))]
/// Says what [`ExportRun::run`] wrote (or why it failed), as its command would have, and gives
/// its answer: the troubleshooting history files what it says (its failure too) under that
/// command, and the session's log has its entry, as when it runs here.
pub fn export_land(s: &mut Session, started: Started, done: Result<Wrote, String>) -> CmdResult {
    let landed = export_land_floored(s, started, done);
    // A landed export (a field package's) may record ids packages hold: kept as any command's are.
    s.keep_field_floors();
    landed
}

/// [`export_land`], before field packages' id floors are kept.
#[cfg(not(target_arch = "wasm32"))]
fn export_land_floored(s: &mut Session, started: Started, done: Result<Wrote, String>) -> CmdResult {
    let _inputs = s.log.on().then(rustgis_data::provenance::Capture::start);
    rustgis_data::provenance::record(started.inputs.lock().unwrap_or_else(|e| e.into_inner()).iter().cloned());
    let Started { command, params, at, start, owner, project, .. } = started;
    let before = s.diagnostic_source.replace(command);
    let mark = s.notice_mark();
    let landed: CmdResult = (|| {
        if let Ok(wrote) = &done {
            let completed = wrote.owner.as_ref().ok_or("the export has no initiating authority")?;
            if !owner.same_run(completed) {
                return Err("this export result belongs to another run".into());
            }
        }
        let written_path = done.as_ref().ok().and_then(|wrote| wrote.written_path.clone());
        if let Ok(wrote) = &done {
            s.crs_used_elsewhere.extend(wrote.crs_used.iter().cloned());
        }
        let notes = done.as_ref().ok().map(Wrote::completion_notes).unwrap_or_default();
        s.finish_export(&owner, |s| done.and_then(|wrote| said(s, wrote)))
            .map_err(|why| completion_with_notes(command, written_path.as_deref(), &notes, why))?
    })();
    // The coordinate systems its worker used that needed a missing datum grid, as the command
    // run here says them after it.
    s.say_grid_notes();
    // Its failure said once, under the command, as `Session::run` says one.
    if let Err(e) = &landed {
        let text = Session::redacted(e);
        if !s.notices_since(mark).iter().any(|n| n.error && n.text == text.as_str()) {
            s.notify_redacted(text, true);
        }
    }
    s.diagnostic_source = before;
    if s.log.on() && crate::project_log::is_logged(s, command, &params) {
        let (kept, partial) = crate::project_log::kept_and_partial(&params);
        // The project it started in, not one opened while it ran.
        s.log_entry(crate::project_log::entry(project, command, kept, partial, &landed, at, start.elapsed()), false);
    }
    landed
}

#[cfg(test)]
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
            Ok(Wrote {
                answer: serde_json::json!({"path": "x"}),
                diagnostics: Vec::new(),
                refused_notes: Vec::new(),
                notes: ["Exported x"].into_iter().collect(),
                written_path: Some("x".into()),
                then: None,
                #[cfg(not(target_arch = "wasm32"))]
                owner: None,
                #[cfg(not(target_arch = "wasm32"))]
                crs_used: Vec::new(),
            })
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
