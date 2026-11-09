//! The History pane's session log (opengeos/RustGIS#797, part 2): the commands run in the
//! session, newest first, each with its project, filtered by command, to those that failed and to
//! the project open now; copied back as Script pane lines; saved as CSV or JSON Lines; cleared or
//! turned off. It shows the session's log (`Session::log`) as `project.log`
//! gives it, and changes it only through the log's commands, so what the pane does is logged as
//! a script's would be.

use egui::{CornerRadius, Sense, Stroke, Ui, vec2};
use rustgis_engine::project_log::LogEntry;
use serde_json::{Value, json};

use crate::RustGisApp;
use crate::theme::{Tokens, semibold};

/// Most entries the pane lists, the newest: it says how many more match.
pub const SHOWN: usize = 200;

/// What the listed rows were picked from: the log's [`changes`](rustgis_engine::project_log::SessionLog::changes)
/// and the filters, the query as typed (trimmed), and the project open now when only its entries
/// are listed.
type Key = (u64, String, bool, Option<u64>);

/// An entry as the pane draws it: its texts made once, when the rows are picked.
struct Row {
    e: LogEntry,
    /// Its parameters as one line of JSON (none for no parameters), and that line as listed.
    params: String,
    line: String,
    /// How long it took, and when it started.
    took: String,
    at: String,
    /// Its command can run as a script's line: one the session has (not a plugin's, which a
    /// script can't run) and not one refused there (scripts, notebooks, the interface's own).
    scriptable: bool,
}

impl Row {
    fn of(e: LogEntry, registry: &rustgis_engine::Registry) -> Row {
        let scriptable = registry.get(&e.command).is_some() && !rustgis_engine::cmd::workflows::refused_in_script(&e.command);
        let params = params_text(&e);
        // One line in the list; the whole of it (up to the log's 4 KB) on hover.
        let mut line: String = params.chars().take(120).collect();
        if line.len() < params.len() {
            line.push('…');
        }
        let took = crate::i18n::fmt("{ms} ms", &[("ms", &e.millis.to_string())]);
        let at = if e.project.is_empty() {
            crate::i18n::fmt("{time} UTC", &[("time", &e.time_utc())])
        } else {
            crate::i18n::fmt("{time} UTC · {project}", &[("time", &e.time_utc()), ("project", &e.project)])
        };
        Row { e, params, line, took, at, scriptable }
    }
}

/// Which list the History pane shows, and the log's filters.
#[derive(Default)]
pub struct State {
    /// The session log rather than the tool runs.
    pub open: bool,
    /// Words the command's id holds (`layer.`, `export`), in any case.
    pub query: String,
    /// Only the commands that failed.
    pub failed: bool,
    /// Only the commands run in the project open now.
    pub this_project: bool,
    /// The rows last listed and how many match and are in the log, kept while their [`Key`]
    /// holds: the pane is drawn each frame, and picking them goes through the whole log.
    cache: Option<(Key, Vec<Row>, usize, usize)>,
}

/// The switch at the top of the History pane, between its two records (named as Mavrick-F
/// proposed on opengeos/RustGIS#1216): the tool runs and the session log; whether the log is the
/// one shown.
pub fn tabs(app: &mut RustGisApp, ui: &mut Ui) -> bool {
    ui.horizontal(|ui| {
        let s = &mut app.ui.session_log;
        ui.selectable_value(&mut s.open, false, tl!("Tool runs"))
            .on_hover_text(tl!("The tools run on this project: saved with it, and each can run again"));
        ui.selectable_value(&mut s.open, true, tl!("Session log"))
            .on_hover_text(tl!("Everything done in this session, from the app, scripts, the AI Assistant or plugins"));
    });
    ui.add_space(4.0);
    app.ui.session_log.open
}

/// Whether entry `e` is one the filters keep: `query` already in lower case; `project`, when
/// only one project's entries are listed, that project ([`rustgis_engine::Session::opened`]).
fn listed(e: &LogEntry, query: &str, failed: bool, project: Option<u64>) -> bool {
    (!failed || !e.ok) && (query.is_empty() || e.command.to_lowercase().contains(query)) && project.is_none_or(|p| e.opened == p)
}

/// The session log: its filters, its newest entries that match, and what can be done with it.
pub fn pane(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    // Wrapped: in a narrow pane the filters go on to the next line, the field kept as wide.
    ui.horizontal_wrapped(|ui| {
        let s = &mut app.ui.session_log;
        ui.add(egui::TextEdit::singleline(&mut s.query).hint_text(tl!("Filter by command")).desired_width(170.0))
            .on_hover_text(tl!("Words in the command's id, such as layer. or export"));
        ui.toggle_value(&mut s.this_project, tl!("This project")).on_hover_text(tl!("Only the commands run in the project open now"));
        ui.checkbox(&mut s.failed, tl!("Failed only"));
    });
    // Compared in place, so a repaint that changes nothing picks, formats and clones nothing;
    // the rows (and their texts) are made again only when the key changed.
    let (log, registry, State { query, failed, this_project, cache, .. }) =
        (&app.session.log, &app.session.registry, &mut app.ui.session_log);
    let raw = query.trim();
    let project = this_project.then_some(app.session.opened);
    let same = cache.as_ref().is_some_and(|(k, ..)| k.0 == log.changes() && k.1 == raw && k.2 == *failed && k.3 == project);
    if !same {
        let lower = raw.to_lowercase();
        let (shown, count, total) = log.select(|e| listed(e, &lower, *failed, project), SHOWN);
        let rows = shown.into_iter().map(|e| Row::of(e, registry)).collect();
        *cache = Some(((log.changes(), raw.to_string(), *failed, project), rows, count, total));
    }
    let (count, total, on_screen) = cache.as_ref().map_or((0, 0, 0), |(_, rows, matched, all)| (*matched, *all, rows.len()));
    let note = if total == 0 {
        tl!("Commands that change the project, or that name a file, folder or address, appear here.").to_string()
    } else if count == 0 {
        tl!("No command matches").to_string()
    } else if count > on_screen {
        crate::i18n::fmt("The newest {shown} of {count} entries", &[("shown", &on_screen.to_string()), ("count", &count.to_string())])
    } else if count < total {
        crate::i18n::fmt("{count} of {total} entries", &[("count", &count.to_string()), ("total", &total.to_string())])
    } else if count == 1 {
        tl!("1 entry").to_string()
    } else {
        crate::i18n::fmt("{count} entries", &[("count", &count.to_string())])
    };
    ui.label(egui::RichText::new(note).size(11.0).color(t.text_dim));
    // Entries dropped past the log's limits aren't listed, nor saved: said.
    if let Some(said) = app.session.log.dropped_said() {
        ui.label(egui::RichText::new(said).size(11.0).color(t.text_dim));
    }
    if !app.session.log.on() {
        ui.label(egui::RichText::new(tl!("Logging is off: commands run now aren't recorded.")).size(11.0).color(t.red));
    }
    ui.add_space(4.0);
    let running = crate::analysispane::script_running(app);
    let mut picked = None;
    if let Some((_, rows, ..)) = &app.ui.session_log.cache {
        for r in rows.iter().rev() {
            picked = row(ui, r, t, running).or(picked);
            ui.add_space(4.0);
        }
    }
    match picked {
        Some(Picked::Open(script)) => crate::analysispane::open_script(app, ui.ctx(), script),
        Some(Picked::CopiedPartial) => app
            .session
            .notify(tl!("Copied. The log cut its parameters short or took their secrets out, so it may not run as it ran."), false),
        None => {}
    }
    ui.horizontal_wrapped(|ui| {
        let tip = tl!("The whole log, as CSV or JSON Lines (.jsonl): it may name files and addresses, so look at it before sharing it");
        if ui.add_enabled(total > 0, egui::Button::new(tl!("Save Log As…"))).on_hover_text(tip).clicked() {
            save(app);
        }
        if ui.add_enabled(total > 0, egui::Button::new(tl!("Clear Log"))).clicked()
            && let Err(e) = app.run("project.clearLog", Value::Null)
        {
            app.session.notify(e, true);
        }
        let mut on = app.session.log.on();
        let tip = tl!("Off, the commands run aren't recorded; what is in the log stays");
        if ui.checkbox(&mut on, tl!("Log Commands")).on_hover_text(tip).changed()
            && let Err(e) = app.run("project.logCommands", json!({"on": on}))
        {
            app.session.notify(e, true);
        }
    });
}

/// An entry's parameters as one line of JSON: none for no parameters.
fn params_text(e: &LogEntry) -> String {
    match &e.params {
        Value::Null => String::new(),
        Value::Object(m) if m.is_empty() => String::new(),
        p => p.to_string(),
    }
}

/// A Script pane line for entry `e`: its command and parameters, as History's Copy as Command
/// writes one. Undo and redo alone: what the log keeps of them is the step's label, not a
/// parameter.
fn command_line(e: &LogEntry) -> String {
    let params = params_text(e);
    if params.is_empty() || matches!(e.command.as_str(), "edit.undo" | "edit.redo") {
        e.command.clone()
    } else {
        format!("{} {params}", e.command)
    }
}

/// The script a `script.run` entry ran, as the log kept it.
fn script_of(e: &LogEntry) -> Option<&str> {
    if e.command == "script.run" { e.params.get("script").and_then(Value::as_str) } else { None }
}

/// What a row's menu asked for that the pane does once the rows are drawn.
enum Picked {
    /// Open in Script Pane, with this script.
    Open(String),
    /// Copy Command or Copy Parameters, of parameters the log cut short or took secrets out of:
    /// said.
    CopiedPartial,
}

/// One entry: whether it worked, its command, how long it took, when it started and in which
/// project, its error and its parameters (all of them on hover). A right-click copies them, or
/// the entry as a Script pane line; a script run, its script, or opens it in the Script pane
/// (not while `running`, a run of the pane's own under way). The right-click is sensed behind
/// the row's contents, which only sense hover, so their hover texts still show.
fn row(ui: &mut Ui, item: &Row, t: &Tokens, running: bool) -> Option<Picked> {
    let (e, params) = (&item.e, item.params.as_str());
    let frame =
        egui::Frame::NONE.stroke(Stroke::new(1.0, t.border)).corner_radius(CornerRadius::same(6)).inner_margin(egui::Margin::same(8));
    let r = ui.scope_builder(egui::UiBuilder::new().sense(Sense::click()), |ui| {
        frame.show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                // Worked or failed, said in words too (on hover, and to a screen reader), not by its
                // colour alone.
                let (r, dot) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                ui.painter().circle_filled(r.center(), 4.5, if e.ok { t.green } else { t.red });
                let worked = if e.ok { tl!("Worked") } else { tl!("Failed") };
                dot.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, worked));
                dot.on_hover_text(worked);
                text(ui, egui::RichText::new(&e.command).font(semibold(12.5)));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    text(ui, egui::RichText::new(&item.took).size(11.0).color(t.text_dim));
                });
            });
            text(ui, egui::RichText::new(&item.at).size(11.0).color(t.text_dim));
            if let Some(err) = &e.error {
                text(ui, egui::RichText::new(err).size(11.0).color(t.red));
            }
            if !params.is_empty() {
                ui.add(
                    egui::Label::new(egui::RichText::new(&item.line).monospace().size(10.5).color(t.text_dim)).truncate().selectable(false),
                )
                .on_hover_text(egui::RichText::new(params).monospace());
            }
        })
    });
    let mut picked = None;
    r.response.context_menu(|ui| {
        if ui.add_enabled(!params.is_empty(), egui::Button::new(tl!("Copy Parameters"))).clicked() {
            ui.ctx().copy_text(params.to_string());
            picked = e.partial.then_some(Picked::CopiedPartial);
            ui.close();
        }
        if let Some(script) = script_of(e) {
            // A script cut short would run only its first part: neither copied nor opened.
            let cut = tl!("The log cut it short or took its secrets out: it would run only in part");
            let copy = ui.add_enabled(!e.partial, egui::Button::new(tl!("Copy Script")));
            if copy.on_hover_text(tl!("The script, for the Script pane")).on_disabled_hover_text(cut).clicked() {
                ui.ctx().copy_text(script.to_string());
                ui.close();
            }
            let undo = if ui.ctx().os().is_mac() { "⌘+Z" } else { "Ctrl+Z" };
            let tip = crate::i18n::fmt("In place of the Script pane's script ({keys} brings it back)", &[("keys", undo)]);
            let why = if e.partial { cut } else { tl!("Wait for the Script pane's run to finish") };
            let open = ui.add_enabled(!e.partial && !running, egui::Button::new(tl!("Open in Script Pane")));
            if open.on_hover_text(tip).on_disabled_hover_text(why).clicked() {
                picked = Some(Picked::Open(script.to_string()));
                ui.close();
            }
        } else {
            let copy = ui.add_enabled(item.scriptable, egui::Button::new(tl!("Copy Command")));
            if copy.on_hover_text(tl!("A line for the Script pane")).on_disabled_hover_text(tl!("It can't run in a script")).clicked() {
                ui.ctx().copy_text(command_line(e));
                picked = e.partial.then_some(Picked::CopiedPartial);
                ui.close();
            }
        }
    });
    picked
}

/// A row's text: not selectable, so a right-click on it reaches the row.
fn text(ui: &mut Ui, t: egui::RichText) {
    ui.add(egui::Label::new(t).selectable(false));
}

/// Saves the log: downloaded as CSV in the browser, where `project.exportLog` can't write a
/// file (so the download isn't itself an entry in the log, said in the user guide); else to the
/// CSV or JSON Lines file the user picks, through `project.exportLog`, which the log records.
fn save(app: &mut RustGisApp) {
    if app.services.download.is_some() {
        let mut bytes = Vec::new();
        match app.session.log.write(&mut bytes, true) {
            Ok(()) => {
                // Entries dropped past the log's limits aren't in the file: said, as an export
                // says it on the desktop (not of a download that failed, which says why).
                if app.download_bytes("session-log.csv", bytes)
                    && let Some(said) = app.session.log.dropped_said()
                {
                    app.session
                        .notify(crate::i18n::fmt("{name}: not in it: {said}", &[("name", "session-log.csv"), ("said", &said)]), false);
                }
            }
            Err(e) => app.session.notify(e.to_string(), true),
        }
        return;
    }
    let Some(path) = crate::dialogs::pick_save("session-log.csv", &["csv", "jsonl"], tl!("Session log")) else {
        // Cancelled: nothing was saved.
        return;
    };
    // A name typed without an extension is saved as CSV, the first kind offered.
    let mut params = json!({"path": path});
    if std::path::Path::new(&path).extension().is_none() {
        params["format"] = "csv".into();
    }
    if let Err(e) = app.run("project.exportLog", params) {
        app.session.notify(e, true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::Queryable;

    fn harness(app: RustGisApp) -> egui_kittest::Harness<'static, RustGisApp> {
        let mut fonts = false;
        egui_kittest::Harness::builder().with_size(vec2(520.0, 2400.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                // The History pane as the app draws it: its two views.
                let t = crate::theme::Tokens::light();
                crate::panes::history(app, ui, &t);
            },
            app,
        )
    }

    /// The History pane's session log lists the session's commands, newest first, filtered by
    /// command and to those that failed, and says how many match; reading is left out, as in
    /// the log.
    #[test]
    fn the_session_log_lists_the_commands_run_newest_first() {
        let mut app = RustGisApp::with_sample();
        app.session.log.clear();
        app.run("map.new", json!({"name": "Second"})).unwrap();
        assert!(app.run("layer.remove", json!({"layer": 999_999})).is_err());
        app.run("info.project", Value::Null).unwrap();
        app.ui.session_log.open = true;
        let mut h = harness(app);
        h.run();
        assert!(h.query_by_label("Tool runs").is_some() && h.query_by_label("Session log").is_some(), "the two views, by name");
        assert!(h.query_by_label("2 entries").is_some());
        let y = |h: &egui_kittest::Harness<'_, RustGisApp>, name: &str| h.get_by_label(name).rect().center().y;
        assert!(y(&h, "layer.remove") < y(&h, "map.new"), "newest first");
        assert!(h.query_by_label("info.project").is_none(), "reading isn't logged");
        // Its parameters, on one line.
        assert!(h.query_by_label(r#"{"name":"Second"}"#).is_some());
        h.state_mut().ui.session_log.failed = true;
        h.run();
        assert!(h.query_by_label("1 of 2 entries").is_some());
        assert!(h.query_by_label("map.new").is_none());
        let s = &mut h.state_mut().ui.session_log;
        (s.failed, s.query) = (false, "MAP.".into());
        h.run();
        assert!(h.query_by_label("map.new").is_some() && h.query_by_label("layer.remove").is_none());
        h.state_mut().ui.session_log.query = "no such command".into();
        h.run();
        assert!(h.query_by_label("No command matches").is_some());
        // Tool runs shows the tools run instead: none here, then one.
        h.state_mut().ui.session_log.open = false;
        h.run();
        assert!(h.query_by_label("Filter by command").is_none());
        assert!(h.query_by_label("Tools you run appear here.").is_some());
        h.state_mut().session.project.gp_history.push(rustgis_engine::GpRecord {
            tool: "Buffer".into(),
            id: "buffer".into(),
            params: json!({}),
            command: None,
            at: 0,
            ok: true,
            messages: vec!["Made 3 features".into()],
            millis: 7,
            outputs: Vec::new(),
        });
        h.run();
        assert!(h.query_by_label("Buffer").is_some() && h.query_by_label("Made 3 features").is_some());
        assert!(h.query_by_label("Tools you run appear here.").is_none());
    }

    /// A step joined to the newest entry (a held key's nudges) shows in the pane at once, though
    /// the log's length is the same.
    #[test]
    fn a_joined_entry_is_shown_as_it_is_now() {
        let mut app = RustGisApp::with_sample();
        app.session.log.clear();
        let nudge = |millis| LogEntry {
            at: 1,
            command: "layout.nudge".into(),
            params: json!({"dx": 1}),
            ok: true,
            error: None,
            millis,
            ..Default::default()
        };
        app.session.log.push(nudge(2));
        app.ui.session_log.open = true;
        let mut h = harness(app);
        h.run();
        assert!(h.query_by_label("2 ms").is_some());
        h.state_mut().session.log.join_last(nudge(3));
        h.run();
        assert!(h.query_by_label("5 ms").is_some() && h.query_by_label("2 ms").is_none());
        assert!(h.query_by_label("1 entry").is_some());
    }

    /// Past [`SHOWN`] entries the pane lists the newest and says how many match.
    #[test]
    fn a_long_log_shows_its_newest_entries() {
        let mut app = RustGisApp::with_sample();
        app.session.log.clear();
        for i in 0..SHOWN + 5 {
            app.run("map.new", json!({"name": format!("Map {i}")})).unwrap();
        }
        app.ui.session_log.open = true;
        let mut h = harness(app);
        h.run();
        assert!(h.query_by_label(&format!("The newest {SHOWN} of {} entries", SHOWN + 5)).is_some());
        assert!(h.query_by_label(&format!(r#"{{"name":"Map {}"}}"#, SHOWN + 4)).is_some());
        assert!(h.query_by_label(r#"{"name":"Map 4"}"#).is_none());
        // Nothing dropped, nothing said; each entry says whether it worked, in words.
        assert!(h.query_by_label_contains("were dropped past").is_none());
        assert!(h.query_all_by_label("Worked").next().is_some());
    }

    /// Past the log's limit, the pane says how many of the oldest entries it dropped.
    #[test]
    fn a_log_past_its_limit_says_what_it_dropped() {
        let mut app = RustGisApp::with_sample();
        app.session.log.clear();
        for i in 0..rustgis_engine::project_log::LOG_LIMIT + 7 {
            app.session.log.push(LogEntry {
                at: i as u64,
                command: "x".into(),
                params: Value::Null,
                ok: true,
                error: None,
                millis: 0,
                ..Default::default()
            });
        }
        app.ui.session_log.open = true;
        let mut h = harness(app);
        h.run();
        assert!(h.query_by_label("7 oldest entries were dropped past the log's limit of 10000").is_some());
        // Past the plugins' limit, an older plugin command's entry goes, and is said as that.
        let app = h.state_mut();
        app.session.log.clear();
        for i in 0..rustgis_engine::project_log::LOG_PLUGIN_LIMIT + 2 {
            let command = format!("{}eng/f", rustgis_plugins::PREFIX);
            app.session.log.push(LogEntry {
                at: i as u64,
                command,
                params: Value::Null,
                ok: true,
                error: None,
                millis: 0,
                ..Default::default()
            });
        }
        h.run();
        assert!(h.query_by_label("2 older plugin command entries were dropped past the 1000 kept for plugins").is_some());
    }

    /// Save Log As… writes the log where the user picks, as CSV or JSON Lines by the name's
    /// extension (CSV without one), through `project.exportLog`; Clear Log empties it, leaving its
    /// own entry; Log Commands turns it off, saying so.
    #[test]
    fn the_session_log_is_saved_cleared_and_turned_off() {
        let mut app = RustGisApp::with_sample();
        app.session.log.clear();
        app.run("map.new", json!({"name": "Second"})).unwrap();
        let dir = tempfile::tempdir().unwrap();
        app.ui.session_log.open = true;
        let mut h = harness(app);
        h.run();
        for (name, first) in [("log.csv", "time_utc,"), ("log.jsonl", "{\"at\":"), ("log", "time_utc,")] {
            let path = dir.path().join(name);
            crate::dialogs::PICKED.with(|p| *p.borrow_mut() = Some(path.display().to_string()));
            h.get_by_label("Save Log As…").click();
            h.run();
            let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(text.starts_with(first), "{name}: {text}");
            assert!(text.contains("map.new"), "{name}");
        }
        // Each save is in the log, as a script's would be.
        assert_eq!(h.state().session.log.entries().filter(|e| e.command == "project.exportLog").count(), 3);
        h.get_by_label("Clear Log").click();
        h.run();
        let commands: Vec<String> = h.state().session.log.entries().map(|e| e.command.clone()).collect();
        assert_eq!(commands, ["project.clearLog"]);
        h.get_by_label("Log Commands").click();
        h.run();
        assert!(!h.state().session.log.on());
        assert!(h.query_by_label("Logging is off: commands run now aren't recorded.").is_some());
        h.state_mut().run("map.new", json!({"name": "Third"})).unwrap();
        assert!(h.state().session.log.entries().all(|e| e.command != "map.new"));
    }

    /// In the browser, where `project.exportLog` can't write a file, Save Log As… downloads the
    /// log as CSV.
    #[test]
    fn in_the_browser_the_session_log_is_downloaded_as_csv() {
        let mut app = RustGisApp::with_sample();
        app.session.log.clear();
        app.run("map.new", json!({"name": "Second"})).unwrap();
        let got = std::rc::Rc::new(std::cell::RefCell::new(None));
        let sink = got.clone();
        app.services.download = Some(Box::new(move |name, bytes| {
            *sink.borrow_mut() = Some((name.to_string(), String::from_utf8(bytes).unwrap()));
            Ok(())
        }));
        save(&mut app);
        let (name, text) = got.borrow_mut().take().expect("downloaded");
        assert_eq!(name, "session-log.csv");
        assert!(text.starts_with("time_utc,at,command,ok,millis,error,params,plugin,project\n"), "{text}");
        assert!(text.contains("\"map.new\",true"), "{text}");
        let dropped = |app: &RustGisApp| {
            app.session.notices.iter().any(|n| n.text.contains("session-log.csv: not in it: 4 oldest entries were dropped past"))
        };
        assert!(!dropped(&app), "nothing dropped, nothing said");
        // Past the log's limit, the download says how many it lacks.
        for i in 0..rustgis_engine::project_log::LOG_LIMIT + 3 {
            app.session.log.push(LogEntry {
                at: i as u64,
                command: "x".into(),
                params: Value::Null,
                ok: true,
                error: None,
                millis: 0,
                ..Default::default()
            });
        }
        save(&mut app);
        assert!(got.borrow_mut().take().is_some());
        assert!(dropped(&app), "{:?}", app.session.notices);
        // A download that fails says why, not what a file it didn't deliver lacks.
        app.session.notices.clear();
        app.services.download = Some(Box::new(|_, _| Err("no room".into())));
        save(&mut app);
        assert!(app.session.notices.iter().any(|n| n.text.contains("no room")), "{:?}", app.session.notices);
        assert!(!dropped(&app), "{:?}", app.session.notices);
    }

    /// The row's right-click doesn't cover its hover texts (whether it worked, and the whole of
    /// its parameters), and works on its text too.
    #[test]
    fn a_rows_hover_texts_show_under_its_context_menu() {
        let mut app = RustGisApp::with_sample();
        app.session.log.clear();
        let long = "n".repeat(200);
        app.run("map.new", json!({"name": long})).unwrap();
        app.ui.session_log.open = true;
        let mut h = harness(app);
        h.run();
        let line = format!(r#"{{"name":"{}"#, "n".repeat(20));
        h.get_by_label_contains(&line).hover();
        h.run();
        h.run();
        let whole = format!(r#"{{"name":"{long}"}}"#);
        assert!(h.query_by_label(&whole).is_some(), "the whole parameters on hover");
        h.get_by_label("Worked").hover();
        h.run();
        h.run();
        assert!(h.query_all_by_label("Worked").count() > 1, "the dot's hover text");
        // A right-click on the row's text still opens its menu.
        h.get_by_label("map.new").click_secondary();
        h.run();
        assert!(h.query_by_label("Copy Command").is_some());
    }

    /// Each row says the project it ran in; This project lists only the project open now's, not
    /// another of the same name.
    #[test]
    fn each_entry_names_its_project_and_this_project_lists_the_open_ones() {
        let mut app = RustGisApp::with_sample();
        app.session.log.clear();
        app.run("project.rename", json!({"name": "Same"})).unwrap();
        app.run("map.new", json!({"name": "Before"})).unwrap();
        app.run("project.new", json!({"name": "Same"})).unwrap();
        app.run("map.new", json!({"name": "After"})).unwrap();
        app.ui.session_log.open = true;
        let mut h = harness(app);
        h.run();
        assert!(h.query_by_label("4 entries").is_some());
        assert_eq!(h.query_all_by_label_contains("UTC · Same").count(), 4, "both projects, by one name");
        h.get_by_label("This project").click();
        h.run();
        assert!(h.state().ui.session_log.this_project);
        assert!(h.query_by_label("2 of 4 entries").is_some(), "the open and what came after, not the other Same");
        assert!(h.query_by_label(r#"{"name":"Before"}"#).is_none() && h.query_by_label(r#"{"name":"After"}"#).is_some());
        // Another project opened with logging off (the log unchanged): the list follows it.
        h.state_mut().run("project.logCommands", json!({"on": false})).unwrap();
        h.run();
        assert!(h.query_by_label("3 of 5 entries").is_some(), "the switch's own entry");
        h.state_mut().run("project.new", json!({"name": "Third"})).unwrap();
        h.run();
        assert!(h.query_by_label("No command matches").is_some());
    }

    /// Right-clicks row `row` and picks `item` in its menu: the text it put on the clipboard.
    fn pick(h: &mut egui_kittest::Harness<'_, RustGisApp>, row: &str, item: &str) -> Vec<String> {
        h.get_by_label(row).click_secondary();
        h.run();
        h.get_by_label(item).click();
        let mut copied = Vec::new();
        for _ in 0..4 {
            h.step();
            for c in &h.output().platform_output.commands {
                if let egui::OutputCommand::CopyText(t) = c {
                    copied.push(t.clone());
                }
            }
        }
        copied
    }

    /// Copy Command copies a Script pane line; a script run copies its script, or opens it in the
    /// Script pane in place of the pane's, which Ctrl+Z brings back.
    #[test]
    fn a_row_copies_its_line_and_a_script_run_opens_in_the_script_pane() {
        let mut app = RustGisApp::with_sample();
        app.session.log.clear();
        app.run("map.new", json!({"name": "Second"})).unwrap();
        let script = "map.new {\"name\": \"Third\"}";
        app.run("script.run", json!({"script": script})).unwrap();
        app.workflow.script = "kept".into();
        app.ui.session_log.open = true;
        let mut h = harness(app);
        h.run();
        assert_eq!(pick(&mut h, "map.new", "Copy Command"), [r#"map.new {"name":"Second"}"#]);
        assert_eq!(pick(&mut h, "script.run", "Copy Script"), [script]);
        h.get_by_label("script.run").click_secondary();
        h.run();
        assert!(h.query_by_label("Copy Command").is_none(), "a script can't run in a script");
        h.get_by_label("Open in Script Pane").click();
        h.run();
        assert_eq!(h.state().workflow.script, script);
        assert_eq!(h.state().ui.right, Some(crate::RightPane::Script));
        let id = egui::Id::unique("script_text");
        let state = egui::widgets::text_edit::TextEditState::load(&h.ctx, id).expect("the box's state");
        let mut undoer = state.undoer();
        let at = state.cursor.char_range().expect("a cursor");
        assert_eq!(undoer.undo(&(at, script.to_string())).map(|(_, t)| t.as_str()), Some("kept"), "Ctrl+Z brings the script back");
        // Opened after another project replaced the pane's, the script left in it still comes back
        // with Ctrl+Z: the pane starting over doesn't lose it first.
        h.state_mut().workflow.script = "unrun".into();
        h.state_mut().run("project.new", json!({"name": "Other"})).unwrap();
        // Drawn again first: the new entry moves the rows down.
        h.run();
        h.get_by_label("script.run").click_secondary();
        h.run();
        h.get_by_label("Open in Script Pane").click();
        h.run();
        assert_eq!(h.state().workflow.script, script);
        let state = egui::widgets::text_edit::TextEditState::load(&h.ctx, id).expect("the box's state");
        let at = state.cursor.char_range().expect("a cursor");
        assert_eq!(state.undoer().undo(&(at, script.to_string())).map(|(_, t)| t.as_str()), Some("unrun"));
    }

    /// Undo copies as itself (the log keeps its step's label, no parameter); a command that can't
    /// run in a script (refused there, or a plugin's) isn't copied as a line; parameters the log
    /// cut or took secrets out of say so when copied, and such a script is neither copied nor
    /// opened.
    #[test]
    fn partial_and_unscriptable_entries_are_copied_as_they_can_run() {
        use egui_kittest::kittest::NodeT;
        let mut app = RustGisApp::with_sample();
        app.session.log.clear();
        let entry =
            |command: &str, params: Value, partial| LogEntry { command: command.into(), params, ok: true, partial, ..Default::default() };
        app.session.log.push(entry("edit.undo", json!({"step": "Rename"}), false));
        app.session.log.push(entry("notebook.run", json!({}), false));
        app.session.log.push(entry(&format!("{}eng/f", rustgis_plugins::PREFIX), json!({}), false));
        app.session.log.push(entry("map.new", json!({"name": "S…"}), true));
        app.session.log.push(entry("script.run", json!({"script": "map.new {\"na…"}), true));
        app.ui.session_log.open = true;
        let mut h = harness(app);
        h.run();
        assert_eq!(pick(&mut h, "edit.undo", "Copy Command"), ["edit.undo"]);
        assert_eq!(pick(&mut h, "map.new", "Copy Command"), [r#"map.new {"name":"S…"}"#]);
        let said = |h: &egui_kittest::Harness<'_, RustGisApp>| {
            h.state().session.notices.iter().filter(|n| n.text.contains("may not run as it ran")).count()
        };
        assert_eq!(said(&h), 1, "{:?}", h.state().session.notices);
        // Its parameters alone, copied, say it too.
        assert_eq!(pick(&mut h, "map.new", "Copy Parameters"), [r#"{"name":"S…"}"#]);
        assert_eq!(said(&h), 2, "{:?}", h.state().session.notices);
        h.get_by_label("notebook.run").click_secondary();
        h.run();
        assert!(h.get_by_label("Copy Command").accesskit_node().is_disabled());
        h.key_press(egui::Key::Escape);
        h.run();
        // A plugin's command: a script can't run it either.
        h.get_by_label(&format!("{}eng/f", rustgis_plugins::PREFIX)).click_secondary();
        h.run();
        assert!(h.get_by_label("Copy Command").accesskit_node().is_disabled());
        h.key_press(egui::Key::Escape);
        h.run();
        h.get_by_label("script.run").click_secondary();
        h.run();
        assert!(h.get_by_label("Copy Script").accesskit_node().is_disabled());
        assert!(h.get_by_label("Open in Script Pane").accesskit_node().is_disabled());
    }
}
