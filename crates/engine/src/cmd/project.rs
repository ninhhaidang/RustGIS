//! Project tab (backstage), undo/redo and tool switching.

use std::path::PathBuf;

use serde_json::{Value, json};

use super::ps;
use crate::{CmdResult, CommandSpec as C, Project, Session, Tool};

pub fn commands() -> Vec<C> {
    vec![
        // Replacing or saving the project isn't an undoable edit: these are `.view()` commands
        // that manage `dirty` and the history themselves.
        C::new("project.new", "New Project", "Project", new).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Replaces)).key("Mod+N").view().params(r#"{"name"?: string}"#),
        C::new("project.sample", "World Atlas Sample", "Project", sample).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Replaces)).view(),
        C::new("project.open", "Open Project", "Project", open).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Replaces).with(crate::policy::Effect::Files)).key("Mod+O").view().params(r#"{"path": string}"#),
        C::new("project.openText", "Open Project (from text)", "Project", open_text).roles(&[crate::ParamRole { key: "text", role: crate::Role::Document(crate::Document::Project) }]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Replaces).with(crate::policy::Effect::Network)).view().params(r#"{"text": project JSON, "fileName"?: string, "package"?: "true for package contents: never restore saved layer secrets"}"#),
        C::new("project.save", "Save Project", "Project", save).inputs(save_inputs).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files)).key("Mod+S").view().params(r#"{"path"?: string}"#),
        C::new("project.saveAs", "Save Project As", "Project", save_as).inputs(save_as_inputs).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files)).key("Mod+Shift+S").view().params(r#"{"path": string}"#),
        C::new("project.rename", "Rename Project", "Project", rename).roles(&[]).effects(crate::policy::Effects::NONE).params(r#"{"name": string}"#),
        C::new("edit.undo", "Undo", "Quick Access Toolbar", |s, _| if s.undo() { Ok(Value::Null) } else { Err("nothing to undo".into()) }).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::History))
            .key("Mod+Z")
            .view()
            .when(|s| if s.can_undo() { None } else { Some("nothing to undo") }),
        C::new("edit.redo", "Redo", "Quick Access Toolbar", |s, _| if s.redo() { Ok(Value::Null) } else { Err("nothing to redo".into()) }).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::History))
            .key("Mod+Y")
            .view()
            .when(|s| if s.can_redo() { None } else { Some("nothing to redo") }),
        C::new("tool.set", "Set Map Tool", "Home › Navigate", set_tool).roles(&[]).effects(crate::policy::Effects::NONE)
            .view()
            .params(r#"{"tool": "explore"|"select"|"selectPolygon"|"selectLasso"|"selectRadius"|"zoomIn"|"measure"|"measureArea"|"createPoint"|"createLine"|"createPolygon"|"editVertices"|"cutPolygon"|"reshape"|"graphicPoint"|"graphicText"|"graphicLine"|"graphicCurve"|"graphicFreehand"|"graphicPolygon"|"graphicLasso"|"graphicRectangle"|"graphicCircle"|"graphicEllipse"|"graphicPicture"}"#),
        C::new("info.project", "Project Info", "Project", info).inputs(super::tiles::listing_inputs).roles(&[]).effects(crate::policy::Effects::NONE).view().params(r#"{"nameFilter"?: string, "includeFields"?: bool}"#),
        C::new("info.commands", "List Commands", "Help", list).roles(&[]).effects(crate::policy::Effects::NONE).view(),
        // The session log (#797): read, written out, emptied, turned on or off. It sits outside
        // the project, so none of these changes the project or takes an undo step.
        C::new("project.log", "Session Log", "Project", log).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files)).view().params(
            r#"{"since"?: milliseconds since 1970 (UTC), "until"?: milliseconds, "command"?: "a command id, or the start of one (layer.)", "failed"?: bool (true: only those that failed; false: only those that succeeded), "limit"?: entries, a whole number (default 100, the newest; 1 to 1000)} → {"entries": [{"at", "command", "params", "ok", "error"?, "millis", "project": the project open when it ran, "partial"?: true when params were cut or had secrets taken out, "plugin"?: the session plugin that asked for it between its commands}], "count": matching, "total": in the log, "on": bool}"#,
        ),
        C::new("project.exportLog", "Export Session Log", "Project", export_log).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files))
            .view()
            .params(r#"{"path": ".csv, .jsonl or .ndjson file", "format"?: "csv"|"jsonl" (for a path with no extension; one given must match the extension)}"#),
        C::new("project.clearLog", "Clear Session Log", "Project", clear_log).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Reserved)).view(),
        C::new("project.logCommands", "Log Commands", "Project", log_commands).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Reserved)).view().params(r#"{"on"?: bool (default: turn it the other way)}"#),
    ]
}

/// `project.log`: the log's entries that match, newest last.
fn log(s: &mut Session, p: &Value) -> CmdResult {
    // Milliseconds as a whole number or not (1.7e12), never below 0.
    let num = |k: &str| -> Result<Option<u64>, String> {
        match p.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => v
                .as_f64()
                .filter(|n| n.is_finite() && *n >= 0.0)
                .map(|n| Some(n as u64))
                .ok_or_else(|| format!("{k} must be a number, 0 or more")),
        }
    };
    let (since, until) = (num("since")?, num("until")?);
    let command = match p.get("command") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_str().ok_or("command must be a command id, or the start of one")?.trim()).filter(|c| !c.is_empty()),
    };
    let failed = match p.get("failed") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_bool().ok_or("failed must be true or false")?),
    };
    // At most a thousand (some 4 MB at the most), so one answer can't fill an assistant's
    // context or a script's memory; `count` still says how many match.
    let limit = match p.get("limit") {
        None | Some(Value::Null) => 100,
        Some(v) => match v.as_f64().filter(|n| n.is_finite() && *n >= 1.0 && n.fract() == 0.0) {
            Some(n) => n.min(1_000.0) as usize,
            None => return Err("limit must be a whole number, 1 or more".into()),
        },
    };
    let keep = |e: &crate::project_log::LogEntry| {
        since.is_none_or(|t| e.at >= t)
            && until.is_none_or(|t| e.at <= t)
            && command.is_none_or(|c| e.command == c || (c.ends_with('.') && e.command.starts_with(c)))
            && failed.is_none_or(|f| e.ok != f)
    };
    let (shown, count, total) = s.log.select(keep, limit);
    Ok(json!({"entries": shown, "count": count, "total": total, "on": s.log.on()}))
}

/// `project.exportLog`: the log written out, as CSV (one row an entry, the parameters as a JSON
/// column) or JSON Lines (one entry a line).
fn export_log(s: &mut Session, p: &Value) -> CmdResult {
    let path = PathBuf::from(ps(p, "path").ok_or("path is required")?);
    let kind = |f: &str| match f {
        "csv" => Some("csv"),
        "jsonl" | "ndjson" => Some("jsonl"),
        _ => None,
    };
    // The path's extension, when it has one, is the format's whatever `format` says: the log
    // never goes over a project, a JSON document or any other kind of file.
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
    let by_ext =
        match ext.as_deref() {
            None => None,
            Some(e) => Some(kind(e).ok_or_else(|| {
                format!("the log is written to a .csv or .jsonl file, not a .{e} one (JSON Lines isn't one JSON document)")
            })?),
        };
    let asked = ps(p, "format").map(str::to_ascii_lowercase);
    let format = match (asked.as_deref(), by_ext) {
        (Some(f), by_ext) => {
            let f = kind(f).ok_or("format is csv or jsonl")?;
            if by_ext.is_some_and(|e| e != f) {
                return Err(format!("format {f} doesn't match the path's .{}", ext.as_deref().unwrap_or_default()));
            }
            f
        }
        (None, Some(e)) => e,
        (None, None) => return Err("give a .csv or .jsonl path, or a format (csv or jsonl)".into()),
    };
    // Written entry by entry, so a long log isn't held as text first; a failure part way leaves
    // an existing file as it was.
    let n = s.log.len();
    rustgis_data::io::write_atomic_with(&path, |w| s.log.write(w, format == "csv")).map_err(|e| format!("{}: {e}", path.display()))?;
    // Entries dropped past the log's limits aren't in the file: said.
    let dropped = s.log.dropped_said().map(|d| format!("; not in it: {d}")).unwrap_or_default();
    s.notify(format!("Exported the session log ({n} entries) to {}{dropped}", path.display()), false);
    Ok(json!({"path": path.display().to_string(), "entries": n, "format": format}))
}

/// `project.clearLog`: empties the log, which then holds this command's own entry, so a cleared
/// log says it was cleared.
fn clear_log(s: &mut Session, _: &Value) -> CmdResult {
    let n = s.log.clear();
    Ok(json!({"cleared": n}))
}

/// `project.logCommands`: turns the log on or off; off, nothing more goes in (what is there stays).
fn log_commands(s: &mut Session, p: &Value) -> CmdResult {
    let on = match p.get("on") {
        None | Some(Value::Null) => !s.log.on(),
        Some(v) => v.as_bool().ok_or("on must be true or false")?,
    };
    s.log.set_on(on);
    Ok(json!({"on": on}))
}

/// Replaces the project, clearing history and per-project state.
pub(crate) fn reset_to(s: &mut Session, p: Project) {
    reset(s, p);
}

/// A provider loss diagnostic beginning with a definite or explicitly unknown item count.
/// The provider is empty for a note with no `provider: ` prefix ("3 rows could not be read…").
fn counted_conversion_loss(note: &str) -> Option<(&str, Option<u128>, &str)> {
    note.split_once(": ").and_then(|(provider, body)| counted_body(provider, body)).or_else(|| counted_body("", note))
}

fn counted_body<'a>(provider: &'a str, body: &'a str) -> Option<(&'a str, Option<u128>, &'a str)> {
    if let Some(tail) = body.strip_prefix("an unknown number of ") {
        return Some((provider, None, tail));
    }
    let (count, tail) = body.split_once(' ')?;
    if count.is_empty() || !count.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((provider, Some(count.parse().ok()?), tail))
}

/// Whether a note states an amount: a leading count, "on N source feature(s)", or a GDAL
/// "(N occurrence(s))" suffix. A note with none needs no per-layer qualifier.
fn shows_count(note: &str) -> bool {
    let digits_before = |marker: &str| {
        note.split(marker)
            .next()
            .and_then(|head| head.rsplit([' ', '(']).next())
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            && note.contains(marker)
    };
    note.split_whitespace().next().is_some_and(|w| w.bytes().all(|b| b.is_ascii_digit()))
        || counted_conversion_loss(note).is_some()
        || digits_before(" source feature")
        || digits_before(" occurrence(s))")
}

struct ConversionLoss {
    layers: usize,
    first: String,
    note: String,
    total: Option<u128>,
    unknown: bool,
    /// The note whose wording the grouped notice uses: the first unknown one once any count is
    /// unknown, so a definite "were left out" isn't claimed for layers that may have lost none.
    wording: String,
}

impl ConversionLoss {
    fn new(first: &str, note: String) -> Self {
        let total = counted_conversion_loss(&note).and_then(|(_, count, _)| count);
        let unknown = note.contains("an unknown number of");
        Self { layers: 1, first: first.into(), wording: note.clone(), note, total, unknown }
    }

    fn add(&mut self, note: &str) {
        self.layers += 1;
        let amount = counted_conversion_loss(note).and_then(|(_, count, _)| count);
        self.total = self.total.zip(amount).and_then(|(total, amount)| total.checked_add(amount));
        if note.contains("an unknown number of") && !self.unknown {
            self.unknown = true;
            self.wording = note.into();
        }
    }

    fn notice(self) -> String {
        let Self { layers, first, note, total, unknown, wording } = self;
        if layers == 1 {
            return format!("{first}: {note}");
        }
        let note = if let Some((provider, _, tail)) = counted_conversion_loss(if unknown { &wording } else { &note })
            && (total.is_some() || unknown)
        {
            let count = total.filter(|_| !unknown).map_or_else(|| "an unknown number of".into(), |n| n.to_string());
            if provider.is_empty() { format!("{count} {tail}") } else { format!("{provider}: {count} {tail}") }
        } else if unknown {
            format!("{note} (count shown is {first}'s; the total is unknown)")
        } else if shows_count(&note) {
            format!("{note} (count shown is {first}'s)")
        } else {
            note
        };
        format!("{layers} layers: {note}; first: {first}")
    }
}

/// Says fresh opening losses once per kind, summing leading provider counts only when all are
/// known, and saved-conversion fallbacks once with the layer count and first failure. Ordinary
/// source notes are not opening conversion diagnostics.
pub(crate) fn say_conversion_fallbacks(s: &mut Session) {
    let mut fallback: Option<(usize, String, String)> = None;
    let mut losses = std::collections::BTreeMap::<(String, String), ConversionLoss>::new();
    for l in s.project.maps.iter_mut().flat_map(|m| &mut m.layers) {
        let Some(source) = l.source.as_mut() else { continue };
        if let Some(note) = source.take_conversion_fallback() {
            match &mut fallback {
                Some((count, _, _)) => *count += 1,
                None => fallback = Some((1, l.name.clone(), note)),
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        for note in source.take_conversion_notes() {
            // Source layer names and feature counts vary within a loss kind. Count affected
            // layers once each and total leading feature/row counts across those layers.
            let kind = [
                "Z or M coordinates",
                "another geometry type",
                "rows without a geometry",
                "no declared coordinate system",
                "rows could not be read while checking the replacement",
                "Integer overflow",
            ]
            .into_iter()
            .find(|kind| note.contains(*kind))
            // An unlisted GDAL warning, without its per-layer occurrence count.
            .unwrap_or_else(|| note.rsplit_once(" (").filter(|(_, tail)| tail.ends_with("occurrence(s))")).map_or(&note, |(base, _)| base));
            let provider = note.split_once(": ").map_or("", |(provider, _)| provider);
            let key = (provider.to_owned(), kind.to_owned());
            if seen.insert(key.clone()) {
                losses.entry(key).and_modify(|loss| loss.add(&note)).or_insert_with(|| ConversionLoss::new(&l.name, note));
            }
        }
    }
    if let Some((count, name, note)) = fallback {
        let text = if count == 1 {
            format!("{name}: {note}")
        } else {
            format!("{count} layers could not re-read their originals and are showing saved conversions; first: {name}: {note}")
        };
        s.notify(text, false);
    }
    for loss in losses.into_values() {
        s.notify(loss.notice(), false);
    }
}

/// Replaces project authority, clearing the exchange record, item trust and active connections.
fn reset(s: &mut Session, p: Project) {
    s.exchange = None;
    super::connections::clear_active();
    s.database_logins.clear_activations();
    s.database_logins.clear_requests();
    // Kept until the command ends: a failure gives the project back with its history.
    s.put_aside();
    s.project = p;
    // Reads begun for the previous project aren't joined by the new one's layers.
    #[cfg(not(target_arch = "wasm32"))]
    s.zarr_reads.forget_shared(None);
    for text in s.project.take_read_notices(&crate::project::ALL_PARTS) {
        s.notify(text, false);
    }
    s.opened_package = None;
    s.opened = s.opened.wrapping_add(1);
    s.sync_policy_generation();
    s.active_layer = s.map().layers.first().map(|l| l.id);
    s.table_layer = None;
    s.table_standalone = None;
    s.active_layout = None;
    s.tool = Tool::Explore;
    s.clear_history();
    s.file_identity = s.state();
    s.history_from_file = s.project.gp_history.len();
    // Zarr layers that follow the time slider read the step it is at.
    super::zarr::follow_time(s);
    // The sources that read an S3 bucket through a saved connection wait for it (or its choice):
    // nothing of the new project is fetched before.
    s.s3_pending.clear();
    crate::s3_connections::resolve_project_s3(s);
    // Its rasters read in place, begun together as it was read: waited for where the session
    // waits (the command line, Python), landing as the map draws on the desktop.
    s.wait_in_place_reads();
    // What reading a raster left out (a colour table, names, a sidecar's scale) is said again as
    // the project opens, as when it was added. One still read in the background isn't here yet.
    let mut lost: Vec<(String, String)> = Vec::new();
    for l in s.project.maps.iter_mut().flat_map(|m| &mut m.layers) {
        if let Some(rl) = &mut l.raster {
            lost.extend(rl.lost_unsaid().into_iter().map(|what| (l.name.clone(), what)));
        }
    }
    super::raster::say_losses(s, lost);
    say_conversion_fallbacks(s);
    // A terrain saved on a DEM name two layers share draws the first of them: said, as the
    // project opens (its layers keep their names).
    let shared: Vec<(String, String)> = s
        .project
        .maps
        .iter()
        .filter_map(|m| match &m.view.terrain {
            Some(rustgis_map::view::Terrain { source: rustgis_map::view::TerrainSource::Layer(n), .. })
                if m.layers.iter().filter(|l| l.name.eq_ignore_ascii_case(n)).count() > 1 =>
            {
                Some((m.name.clone(), n.clone()))
            }
            _ => None,
        })
        .collect();
    // One notice, with a count when several maps are so.
    match shared.as_slice() {
        [] => {}
        [(map, dem)] => s.notify(
            format!("{map}: its terrain's DEM name '{dem}' is shared by two layers, so the first of them gives the heights; rename one and turn Terrain on again"),
            false,
        ),
        _ => {
            let maps: Vec<String> = shared.iter().map(|(map, dem)| format!("{map} ('{dem}')")).collect();
            s.notify(
                format!(
                    "{} maps have a terrain whose DEM name is shared by two layers, so the first of them gives the heights: {}; rename one and turn Terrain on again",
                    shared.len(),
                    maps.join(", ")
                ),
                false,
            );
        }
    }
}

fn new(s: &mut Session, p: &Value) -> CmdResult {
    reset(s, Project::new(super::ps(p, "name").unwrap_or("Untitled")));
    s.path = None;
    Ok(Value::Null)
}

fn sample(s: &mut Session, _: &Value) -> CmdResult {
    reset(s, crate::sample::world_project());
    s.active_layer = s.map().layer_by_name("Countries").map(|l| l.id);
    s.path = None;
    Ok(Value::Null)
}

/// Largest project package opened (1 GiB).
const MAX_PACKAGE: u64 = 1 << 30;

/// `project.open`: a project file, or a package (`.rgispkg`) opened unsaved; either must be a
/// regular file within its limit.
fn open(s: &mut Session, p: &Value) -> CmdResult {
    let path = PathBuf::from(super::ps(p, "path").ok_or("path is required")?);
    let is_package = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("rgispkg"));
    let origin = if is_package { path.canonicalize() } else { std::path::absolute(&path) }
        .map_err(|error| format!("{}: {error}", path.display()))?;
    super::connections::clear_active();
    let proj = (|| {
        if is_package {
            let bytes = crate::project::read_file(&path, MAX_PACKAGE)?;
            Project::from_import_json(&super::items::package_text(&bytes)?, None)
        } else {
            Project::load(&path)
        }
    })();
    let proj = match proj {
        Ok(proj) => proj,
        Err(error) => {
            super::connections::restore_active(s);
            return Err(error);
        }
    };
    reset(s, proj);
    // A package opens as a new, unsaved project (saving writes a plain project file).
    if !is_package {
        name_from_file(s, &path);
    }
    if is_package {
        s.opened_package = Some(origin);
        s.path = None;
    } else {
        s.path = Some(origin);
    }
    s.notify(format!("Opened {}", s.project.name), false);
    super::connections::resolve_project(s);
    super::connections::note_waiting(s);
    Ok(Value::Null)
}

fn open_text(s: &mut Session, p: &Value) -> CmdResult {
    let text = super::ps(p, "text").ok_or("text is required")?;
    super::connections::clear_active();
    let proj = if super::b(p, "package").unwrap_or(false) { Project::from_import_json(text, None) } else { Project::from_json(text) };
    let proj = match proj {
        Ok(proj) => proj,
        Err(error) => {
            super::connections::restore_active(s);
            return Err(error);
        }
    };
    reset(s, proj);
    // The browser opens files by name and contents: the name still names the project.
    if let Some(file) = super::ps(p, "fileName") {
        name_from_file(s, std::path::Path::new(file));
    }
    s.path = None;
    s.notify(format!("Opened {}", s.project.name), false);
    super::connections::resolve_project(s);
    super::connections::note_waiting(s);
    Ok(Value::Null)
}

/// Saves the project to `path` (`.rgis` added when it has no extension) and makes it the
/// project's file. The project as it is now is the saved state undo can return to.
///
/// The path must name a project file (`.rgis`, or the legacy `.rgisx`, in any case), so a save
/// never goes over a data file, a package or any other kind of file, including when the
/// project was opened from a file with another extension.
fn write(s: &mut Session, path: PathBuf) -> CmdResult {
    let path = if path.extension().is_none() { path.with_extension(crate::project::EXTENSION) } else { path };
    let destination = std::path::absolute(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    if !crate::project::is_project_file(&path.to_string_lossy()) {
        let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        return Err(if ext == crate::project::PACKAGE_EXTENSION {
            "a project saves to a .rgis file; Package Project (project.package) writes a .rgispkg package".into()
        } else {
            format!("a project saves to a .rgis file, not a .{ext} one")
        });
    }
    let report = s.project.save(&destination)?;
    let stripped = report.layer_addresses();
    s.mark_saved();
    s.edit_counts.clear();
    s.notify(format!("Saved {}", path.display()), false);
    crate::durable::notify(s, &report);
    s.path = Some(destination);
    Ok(json!({"path": path.display().to_string(), "stripped": stripped}))
}

/// Names the project after its file, as Save As does, so a renamed or copied file opens under
/// its own name.
fn name_from_file(s: &mut Session, file: &std::path::Path) {
    if let Some(stem) = file.file_stem().and_then(|x| x.to_str()).map(str::trim).filter(|x| !x.is_empty()) {
        s.project.name = stem.chars().take(200).collect();
    }
}

fn save(s: &mut Session, p: &Value) -> CmdResult {
    let path = save_path(s, p)?;
    write(s, path)
}

/// Saves with the destination's name, restoring the current name if writing fails.
fn save_as(s: &mut Session, p: &Value) -> CmdResult {
    let path = PathBuf::from(super::ps(p, "path").ok_or("path is required")?);
    let previous_name = path.file_stem().and_then(|x| x.to_str()).map(|stem| std::mem::replace(&mut s.project.name, stem.to_string()));
    let result = write(s, path);
    // Only undoable commands get a Session snapshot; restore this view command's name by hand.
    if result.is_err()
        && let Some(name) = previous_name
    {
        s.project.name = name;
    }
    result
}

fn rename(s: &mut Session, p: &Value) -> CmdResult {
    let n = super::ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).ok_or("name is required")?;
    s.project.name = n.chars().take(200).collect();
    Ok(Value::Null)
}

/// Picks the map tool: a Create tool needs a layer of its kind selected, and the tools that
/// change features (Edit Vertices, Cut Polygon, Reshape) need editing on.
fn set_tool(s: &mut Session, p: &Value) -> CmdResult {
    let t = parse_tool(p)?;
    if t.creates() {
        let id = s.layer_id(&Value::Null).map_err(|_| "select the layer to edit in the Layers pane first".to_string())?;
        let gt = s.map().layer(id).map(|l| l.geometry_type());
        let ok = matches!(
            (t, gt),
            (Tool::CreatePoint, Some(rustgis_data::GeometryType::Point))
                | (Tool::CreateLine, Some(rustgis_data::GeometryType::Polyline))
                | (Tool::CreatePolygon, Some(rustgis_data::GeometryType::Polygon))
        );
        if !ok {
            return Err(format!("{} needs a {} layer", t.label(), t.label().trim_start_matches("Create ").to_lowercase()));
        }
    }
    if matches!(t, Tool::EditVertices | Tool::CutPolygon | Tool::Reshape) && !s.map().editing.settings.enabled {
        return Err("editing is turned off (Edit › Edit)".into());
    }
    s.tool = t;
    Ok(Value::Null)
}

fn parse_tool(p: &Value) -> Result<Tool, String> {
    let name = ps(p, "tool").ok_or("tool is required")?;
    Tool::from_code(name).ok_or_else(|| format!("unknown tool '{name}'"))
}

/// A longitude/latitude extent as `[west, south, east, north]`, or null when there is none.
pub(crate) fn bbox(e: rustgis_geom::Extent) -> Value {
    // Rounded outwards, so the bounds still hold every coordinate.
    let (down, up) = (|v: f64| (v * 1e6).floor() / 1e6, |v: f64| (v * 1e6).ceil() / 1e6);
    if e.is_empty() { Value::Null } else { json!([down(e.min_x), down(e.min_y), up(e.max_x), up(e.max_y)]) }
}

/// Everything about the open project in one call (the AI Assistant's and MCP's `list_layers`).
/// Layers are in table-of-contents order, top first. `nameFilter` keeps the layers and tables
/// whose name holds it (ignoring case); `includeFields: false` leaves out their fields.
fn info(s: &mut Session, p: &Value) -> CmdResult {
    let filter = ps(p, "nameFilter").map(|f| f.trim().to_lowercase()).filter(|f| !f.is_empty());
    let named = |name: &str| filter.as_deref().is_none_or(|f| name.to_lowercase().contains(f));
    let fields = super::b(p, "includeFields").unwrap_or(true);
    let field_list = |fs: &[rustgis_data::Field]| json!(fs.iter().map(|f| format!("{} ({})", f.name, f.ty.label())).collect::<Vec<_>>());
    // Addresses lose passwords, keys and tokens: the AI Assistant sends this to its provider.
    let source = |l: &rustgis_map::Layer| -> Option<String> {
        let from = match (&l.raster, &l.source) {
            (Some(r), _) => r.path.clone().or_else(|| r.file_name.clone()),
            (None, Some(src)) => src.path.clone().or_else(|| src.file_name.clone()),
            (None, None) => l.dataset.source.clone(),
        };
        from.filter(|f| !f.is_empty()).map(|f| crate::sources::redact(&f))
    };
    let extent = super::workspace::view_extent(s);
    let m = s.map();
    let active = s.active_layer.and_then(|id| m.layer(id)).map(|l| json!({"id": l.id, "name": l.name}));
    let layers: Vec<Value> = m
        .layers
        .iter()
        .filter(|l| named(&l.name))
        .map(|l| {
            let mut v = match &l.raster {
                Some(rl) => json!({
                    "id": l.id, "name": l.name, "type": "Raster", "visible": l.visible, "renderer": rl.style.renderer.label(),
                    "size": rl.data.as_ref().map(|d| json!([d.width, d.height, d.band_count()])),
                    "missing": rl.data.is_none(),
                }),
                None => json!({
                    "id": l.id, "name": l.name, "type": l.geometry_type().label(), "features": l.dataset.features.len(),
                    "visible": l.visible, "renderer": l.renderer.kind_label(), "selected": l.selection.len(),
                    "definitionQuery": l.definition_query.as_deref().filter(|q| !q.trim().is_empty()),
                    // The text expression it labels by while that reads, as the map draws, else its field.
                    "labels": l.labels.enabled.then(|| {
                        let d = &l.labels.default;
                        d.by.text.as_deref().filter(|t| rustgis_data::query::parse_fields(t, &l.dataset).is_ok())
                            .or(Some(d.field.as_str()).filter(|f| !f.is_empty()))
                    }).flatten(),
                    "labelClasses": label_classes_set(&l.labels)
                        .then(|| l.labels.in_order().map(|c| json!({"name": c.name, "where": c.where_, "field": c.field, "text": c.by.text, "visible": c.visible})).collect::<Vec<_>>()),
                    "fields": field_list(&l.dataset.fields),
                }),
            };
            if let Some(o) = v.as_object_mut() {
                o.insert("group".into(), json!(m.group_of(l.id).map(|g| g.name.as_str())));
                o.insert("transparency".into(), json!(l.transparency));
                o.insert("source".into(), json!(source(l)));
                o.insert("extent".into(), bbox(l.extent()));
                if !fields {
                    o.remove("fields");
                }
            }
            v
        })
        .collect();
    let layouts: Vec<Value> = s
        .project
        .layouts
        .iter()
        .map(|lo| {
            let frames: Vec<Value> = lo
                .elements
                .iter()
                .filter_map(|e| match &e.kind {
                    rustgis_map::layout::ElementKind::MapFrame { map, scale, .. } => {
                        Some(json!({"id": e.id, "name": e.name, "map": map, "scale": scale.round()}))
                    }
                    _ => None,
                })
                .collect();
            // The page in millimetres, as `layout.page` sets it.
            json!({"name": lo.name, "width": lo.width, "height": lo.height, "mapFrames": frames})
        })
        .collect();
    Ok(json!({
        "name": s.project.name,
        "path": s.path.as_ref().map(|p| p.display().to_string()),
        "unsaved": s.dirty(),
        "maps": s.project.maps.iter().map(|m| m.name.clone()).collect::<Vec<_>>(),
        "activeMap": m.name,
        "basemap": m.basemap.code(),
        "projection": m.projection.code(),
        "scale": m.scale().round(),
        "center": {"lon": m.center_lonlat().x, "lat": m.center_lonlat().y},
        "extent": bbox(extent),
        "bearing": m.camera.bearing,
        "pitch": m.camera.pitch,
        "activeLayer": active,
        "layers": layers,
        "groups": m.groups.iter().map(|g| json!({
            "id": g.id, "name": g.name, "visible": g.visible, "transparency": g.transparency,
            "minScale": g.min_scale, "maxScale": g.max_scale, "layers": g.children, "parent": g.parent,
        })).collect::<Vec<_>>(),
        // Web tile layers (XYZ, WMS, vector tiles, styles, archives), each drawn over the map layer
        // its `over` names, or under them all.
        // Their addresses lose keys and tokens: the AI Assistant sends this to its provider.
        "tileLayers": super::tiles::list(s, true),
        "tables": s.project.tables.iter().filter(|t| named(&t.name)).map(|t| {
            let mut v = json!({"id": t.id, "name": t.name, "rows": t.features.len(), "fields": field_list(&t.fields)});
            if let (false, Some(o)) = (fields, v.as_object_mut()) {
                o.remove("fields");
            }
            v
        }).collect::<Vec<_>>(),
        "layouts": layouts,
    }))
}

fn list(s: &mut Session, _: &Value) -> CmdResult {
    let builtin = s
        .registry
        .commands
        .iter()
        .map(|c| json!({"id": c.id, "label": c.label, "location": c.location, "shortcut": c.shortcut, "params": c.params}));
    let plugins = rustgis_plugins::commands().into_iter().map(|c| {
        json!({"id": c.id, "label": c.decl.label, "location": format!("Plugins › {}", c.plugin.manifest.name), "shortcut": "", "params": c.decl.params})
    });
    Ok(Value::Array(builtin.chain(plugins).collect()))
}

/// Whether a layer's labels go beyond a plain Default class: other classes, or a Default that
/// is renamed, hidden or has a clause of its own. Then `info.project` lists the classes.
fn label_classes_set(labels: &rustgis_map::Labeling) -> bool {
    let d = &labels.default;
    !labels.classes.is_empty()
        || !d.visible
        || d.name != rustgis_map::symbology::DEFAULT_LABEL_CLASS
        || d.where_.as_deref().is_some_and(|w| !w.trim().is_empty())
}

/// Save uses the explicit string path, otherwise the current native project path.
fn save_path(input: &impl super::InputSource, p: &Value) -> Result<PathBuf, String> {
    let context = input.input_context();
    let s = &context;
    match super::ps(p, "path") {
        Some(x) => Ok(PathBuf::from(x)),
        None => s.path.clone().ok_or_else(|| "this project hasn't been saved yet; use Save As".into()),
    }
}
/// Binds the full exported project and the same implicit destination used by Save.
fn save_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let mut inputs = super::items::project_inputs(input, p)?;
    inputs.state = json!({"path":save_path(input,p)?});
    Ok(inputs)
}
/// Save As requires its explicit path; there is no stored-path fallback.
fn save_as_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let mut inputs = super::items::project_inputs(input, p)?;
    inputs.state = json!({"path":ps(p,"path").ok_or("path is required")?});
    Ok(inputs)
}

#[cfg(test)]
mod conversion_loss_tests {
    use super::*;

    #[test]
    fn grouped_counts_sum_without_a_provider_and_uncounted_notes_take_no_qualifier() {
        let mut loss = ConversionLoss::new("First", "3 rows could not be read while checking the replacement".into());
        loss.add("4 rows could not be read while checking the replacement");
        assert_eq!(loss.notice(), "2 layers: 7 rows could not be read while checking the replacement; first: First");

        let note = "GDAL: 'a' has no declared coordinate system; its coordinates were left unprojected";
        let mut loss = ConversionLoss::new("First", note.into());
        loss.add("GDAL: 'b' has no declared coordinate system; its coordinates were left unprojected");
        assert_eq!(loss.notice(), format!("2 layers: {note}; first: First"));

        let note = "GDAL: 'a' declares Z or M coordinates on 3 source feature(s); conversion keeps only XY coordinates";
        let mut loss = ConversionLoss::new("First", note.into());
        loss.add("GDAL: 'b' declares Z or M coordinates on 5 source feature(s); conversion keeps only XY coordinates");
        assert_eq!(loss.notice(), format!("2 layers: {note} (count shown is First's); first: First"));
    }

    #[test]
    fn grouped_conversion_losses_keep_unknown_totals_in_either_order() {
        let definite = "GDAL: 3 features of another geometry type were left out";
        let unknown = "GDAL: an unknown number of features of another geometry type may have been left out";
        for notes in [[definite, unknown], [unknown, definite]] {
            let mut loss = ConversionLoss::new("First", notes[0].into());
            loss.add(notes[1]);
            let notice = loss.notice();
            assert!(notice.starts_with("2 layers: GDAL: an unknown number of features of another geometry type"), "{notice}");
            assert!(!notice.contains("GDAL: 3 "), "{notice}");
            assert!(notice.contains("may have been left out") && !notice.contains("were left out"), "{notice}");
        }
    }

    #[test]
    fn grouped_known_conversion_counts_sum_and_single_layers_stay_unchanged() {
        let note = "GDAL: 3 features of another geometry type were left out";
        let mut loss = ConversionLoss::new("First", note.into());
        loss.add("GDAL: 4 features of another geometry type were left out");
        assert_eq!(loss.notice(), "2 layers: GDAL: 7 features of another geometry type were left out; first: First");

        let single = ConversionLoss::new("First", note.into());
        assert_eq!(single.notice(), format!("First: {note}"));
    }
}
