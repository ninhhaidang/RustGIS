//! "Let your Agents Control RustGIS" (desktop), in the AI Assistant settings. Ticking it turns
//! on the MCP relay ([`rustgis_assistant::relay`]): AI agents on this computer that connect
//! (Claude Code, Codex, Cursor…) work on the map open here, with file and network access, no
//! `--yes` needed. While it's on, the section shows what an agent needs to connect, and nothing
//! else. The choice is remembered: the relay starts with the app.

use egui::RichText;
use serde_json::json;

use crate::RustGisApp;
use crate::theme::Tokens;

/// The preference that remembers the choice.
pub const PREF: &str = "agents_control";

#[derive(Default)]
pub struct ConnectState {
    /// The choice, once read from the preferences.
    on: Option<bool>,
    /// The relay was started for a remembered choice (once, at launch).
    resumed: bool,
    /// Why the relay couldn't start, when it couldn't.
    error: Option<String>,
    /// The folder the file bridge uses instead of the relay's private one (tests).
    #[cfg(test)]
    bridge_dir: Option<std::path::PathBuf>,
    /// Full access: ticked for this session only, never remembered, so each launch asks afresh.
    full: bool,
}

/// Whether the user lets agents control RustGIS.
pub fn enabled(app: &RustGisApp) -> bool {
    app.connect.on.unwrap_or_else(|| app.services.load_pref.as_ref().and_then(|l| l(PREF)).as_deref() == Some("on"))
}

/// Whether agents have full access: no command refused, scripts, tasks, models and plugins
/// included. Only while agents are on, and only once ticked in this session.
pub fn full_access(app: &RustGisApp) -> bool {
    enabled(app) && app.connect.full
}

/// At launch, starts the relay when the user had agents turned on.
pub fn resume(app: &mut RustGisApp) {
    // Read only once, and only said when agents are on (`resume_with` returns early otherwise).
    if app.connect.resumed {
        return;
    }
    let port = rustgis_assistant::relay::default_port();
    resume_with(app, port);
}

/// [`resume`] on a given port (tests, with a port of their own).
#[cfg(test)]
fn resume_on(app: &mut RustGisApp, port: u16) {
    resume_with(app, Ok(port));
}

/// [`resume`] on `port`, or with why there is none (a `RUSTGIS_MCP_PORT` that isn't a port).
fn resume_with(app: &mut RustGisApp, port: Result<u16, String>) {
    if std::mem::replace(&mut app.connect.resumed, true) {
        return;
    }
    let on = enabled(app);
    app.connect.on = Some(on);
    if !on {
        return;
    }
    let bridge = start_bridge(app);
    // A RUSTGIS_MCP_PORT that isn't a port fails the relay as a busy port would.
    let relay = match port {
        _ if app.relay.is_some() => Ok(()),
        Ok(port) => app.run("ui.mcpRelay", json!({"port": port})).map(|_| ()),
        Err(e) => Err(e),
    };
    started(app, relay, bridge);
    start_http(app);
}

/// Starts MCP over HTTP beside the relay ([`rustgis_assistant::relay::Relay::serve_http`]) for
/// clients that speak HTTP, while agents are on. Says why when it can't; the relay works without.
fn start_http(app: &mut RustGisApp) {
    let Some(relay) = app.relay.clone() else { return };
    if relay.http_port().is_some() {
        return;
    }
    match relay.serve_http(rustgis_assistant::relay::default_http_port()) {
        Ok(port) => {
            let text = crate::i18n::fmt("MCP over HTTP on 127.0.0.1:{port}", &[("port", &port.to_string())]);
            app.session.notify(text, false);
        }
        Err(e) => {
            // Kept in the settings section too, not only a passing notice.
            let said = crate::i18n::fmt("MCP over HTTP couldn't start: {error}", &[("error", &e)]);
            app.session.notify(said.clone(), true);
            // Beside the relay's own error, if it had one.
            app.connect.error = Some(match app.connect.error.take() {
                Some(before) => format!("{before}\n{said}"),
                None => said,
            });
        }
    }
}

/// Says how starting agents went, once: the relay's failure (with the bridge's, when both
/// failed), or the bridge's alone when only it failed.
fn started(app: &mut RustGisApp, relay: Result<(), String>, bridge: Result<(), String>) {
    match (relay, bridge) {
        (Ok(()), Ok(())) => {}
        (Ok(()), Err(e)) => app.session.notify(crate::i18n::fmt("The file bridge couldn't start: {error}", &[("error", &e)]), true),
        (Err(e), bridge) => relay_failed(app, e, bridge),
    }
}

/// The relay couldn't start (its port is taken, say). With the file bridge running, agents stay
/// on and can use it; without, the choice goes back off.
fn relay_failed(app: &mut RustGisApp, error: String, bridge: Result<(), String>) {
    let said = match bridge {
        Ok(()) => crate::i18n::fmt("MCP relay couldn't start ({error}); agents can still use the file bridge", &[("error", &error)]),
        Err(why) => {
            app.connect.on = Some(false);
            if let Some(save) = &app.services.save_pref {
                save(PREF, "off");
            }
            crate::i18n::fmt("{error}; the file bridge couldn't start either: {why}", &[("error", &error), ("why", &why)])
        }
    };
    app.session.notify(said.clone(), true);
    app.connect.error = Some(said);
}

/// Starts the file bridge ([`rustgis_assistant::filebridge`]) unless it runs already: agents
/// turned on may use files and the network through it, as through the relay.
fn start_bridge(app: &mut RustGisApp) -> Result<(), String> {
    if app.file_bridge.is_some() {
        return Ok(());
    }
    // Tests never touch the real folder (a running RustGIS holds its lock): one of their own, or none.
    #[cfg(test)]
    let started = match app.connect.bridge_dir.clone() {
        Some(dir) => rustgis_assistant::filebridge::FileBridge::start_in(dir),
        None => Err("no file bridge folder in this test".to_string()),
    };
    #[cfg(not(test))]
    let started = rustgis_assistant::filebridge::FileBridge::start();
    let bridge = started?;
    bridge.set_allow_access(true);
    bridge.set_full_access(full_access(app));
    // What scripts do to the user's project is kept, as agents' calls are.
    bridge.set_audit_dir(rustgis_assistant::audit::dir());
    app.file_bridge = Some(bridge);
    Ok(())
}

/// Every explicit stop, including the status bar/command, also withdraws remembered consent.
pub(crate) fn stopped(app: &mut RustGisApp) {
    let changed = app.connect.on != Some(false);
    app.connect.on = Some(false);
    // Agents turned on again start without full access.
    app.connect.full = false;
    app.connect.error = None;
    if let Some(bridge) = app.file_bridge.take() {
        bridge.stop();
    }
    if changed && let Some(save) = &app.services.save_pref {
        save(PREF, "off");
    }
}

/// Literal argument syntax for the shell named beside the copyable command.
fn shell_arg(text: &str) -> String {
    #[cfg(windows)]
    let escaped = text.replace('\'', "''");
    #[cfg(not(windows))]
    let escaped = text.replace('\'', "'\\''");
    format!("'{escaped}'")
}

/// `rustgis-cli` beside the app, or its name (for a PATH lookup) when it isn't there.
fn cli() -> String {
    let name = if cfg!(windows) { "rustgis-cli.exe" } else { "rustgis-cli" };
    std::env::current_exe()
        .ok()
        .map(|p| p.with_file_name(name))
        .filter(|p| p.is_file())
        .map_or(name.to_string(), |p| p.display().to_string())
}

/// A line to copy: shown in monospace, with a Copy button.
fn copyable(ui: &mut egui::Ui, text: &str) {
    // The button first, at the right, so long text wraps in the room left instead of pushing it out.
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
        if ui.small_button(tl!("Copy")).clicked() {
            ui.ctx().copy_text(text.to_string());
        }
        ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
            ui.add(egui::Label::new(RichText::new(text).monospace().size(11.5)).wrap());
        });
    });
}

/// The section in the assistant settings: the switch in a header that folds away the access
/// switch and the connection details (folded until agents are turned on).
pub fn section(app: &mut RustGisApp, ui: &mut egui::Ui, t: &Tokens) {
    let mut on = enabled(app);
    let hint = tl!(
        "Agents on this computer that connect (Claude Code, Codex, Cursor and others) work on the map open here, with RustGIS's tools: they can open and save files and use the web. So can a script any program of yours writes to the file bridge shown below. Scripts and plugin management stay off. The choice is remembered across launches."
    );
    let id = ui.make_persistent_id("connect_agents_section");
    let mut changed = false;
    let mut header = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false)
        .show_header(ui, |ui| changed = ui.checkbox(&mut on, tl!("Let your Agents Control RustGIS")).on_hover_text(hint).changed());
    if changed {
        app.connect.on = Some(on);
        app.connect.error = None;
        if let Some(save) = &app.services.save_pref {
            save(PREF, if on { "on" } else { "off" });
        }
        if on {
            // The relay not started (the port is taken, say): with the file bridge agents stay
            // on; without, the box and the remembered choice go back off.
            let bridge = start_bridge(app);
            let relay = app.run("ui.mcpRelay", json!({})).map(|_| ());
            // Access as the box says before HTTP opens, as for the relay.
            if let Some(relay) = &app.relay {
                relay.set_allow_access(true);
            }
            started(app, relay, bridge);
            start_http(app);
            on = enabled(app);
        } else if let Err(e) = app.run("ui.mcpRelay", json!({"stop": true})) {
            app.connect.error = Some(e);
        }
        if let Some(relay) = &app.relay {
            relay.set_allow_access(on);
            relay.set_full_access(full_access(app));
        }
        // Turned on: show what to give the agents.
        header.set_open(on);
    }
    header.body(|ui| {
        if on {
            details(app, ui);
        } else {
            ui.label(RichText::new(hint).small().color(t.text_dim));
        }
    });
    if let Some(e) = &app.connect.error {
        ui.label(RichText::new(e).small().color(t.red));
    }
}

/// The open section's body: full access and how agents connect.
fn details(app: &mut RustGisApp, ui: &mut egui::Ui) {
    let mut full = full_access(app);
    let hint = tl!(
        "Agents may also run scripts, tasks and models, and install, remove and run plugins: anything you could do yourself. Turn it on only for an agent you trust with your computer. It lasts until RustGIS closes or agents are turned off."
    );
    if ui.checkbox(&mut full, tl!("Full access")).on_hover_text(hint).changed() {
        app.connect.full = full;
        if let Some(relay) = &app.relay {
            relay.set_full_access(full);
        }
        if let Some(bridge) = &app.file_bridge {
            bridge.set_full_access(full);
        }
    }
    // Agents that can't use MCP: a script written to a file.
    if let Some(bridge) = &app.file_bridge {
        let (command, result) = (bridge.command_path().display().to_string(), bridge.result_path().display().to_string());
        ui.label(RichText::new(tl!("Agents without MCP can write a script (one command per line) to:")).small());
        copyable(ui, &command);
        ui.label(RichText::new(tl!("RustGIS runs it and writes the outcome to:")).small());
        copyable(ui, &result);
    }
    let Some(port) = app.relay.as_ref().map(|r| r.port()) else { return };
    // What an agent needs: RustGIS's MCP server, attached to this app.
    let cli = cli();
    let mut args = vec!["mcp".to_string(), "--attach".to_string()];
    if port != rustgis_assistant::relay::DEFAULT_PORT {
        args.push(port.to_string());
    }
    let config = serde_json::to_string_pretty(&json!({"mcpServers": {"rustgis-app": {"command": cli, "args": args}}})).unwrap_or_default();
    ui.label(RichText::new(tl!("Add this MCP server to your agent:")).small());
    copyable(ui, &config);
    let hint = if cfg!(windows) {
        tl!("Or run in PowerShell, for Claude Code or Codex:")
    } else {
        tl!("Or run in a POSIX shell, for Claude Code or Codex:")
    };
    ui.label(RichText::new(hint).small());
    let quoted = shell_arg(&cli);
    for agent in ["claude", "codex"] {
        copyable(ui, &format!("{agent} mcp add rustgis-app -- {quoted} {}", args.join(" ")));
    }
    // Clients that speak MCP over HTTP: registered with the stable token, kept off the screen.
    if let Some(http) = app.relay.as_ref().and_then(|r| r.http_port()) {
        let text = crate::i18n::fmt("Or, for agents that use MCP over HTTP (port {port}):", &[("port", &http.to_string())]);
        ui.label(RichText::new(text).small());
        // PowerShell runs a quoted program only through its call operator.
        let call = if cfg!(windows) { "& " } else { "" };
        let mut register = format!("{call}{quoted} mcp-register --client claude");
        if http != rustgis_assistant::relay::HTTP_PORT {
            register.push_str(&format!(" --port {http}"));
        }
        copyable(ui, &register);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A remembered relay that cannot start leaves agents on with the file bridge, and says so.
    #[test]
    fn failed_relay_resume_keeps_the_file_bridge() {
        use std::sync::{Arc, Mutex};
        let occupied = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = occupied.local_addr().unwrap().port();
        let saved = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
        let mut app = RustGisApp::with_sample();
        app.services.load_pref = Some(Box::new(|key: &str| (key == PREF).then(|| "on".to_string())));
        let sink = saved.clone();
        app.services.save_pref = Some(Box::new(move |key: &str, value: &str| {
            sink.lock().unwrap().push((key.into(), value.into()));
        }));
        let bridge_dir = tempfile::tempdir().unwrap();
        app.connect.bridge_dir = Some(bridge_dir.path().to_path_buf());
        let mark = app.session.notice_mark();
        resume_on(&mut app, port);
        assert!(app.relay.is_none());
        assert!(enabled(&app) && app.file_bridge.is_some(), "agents stay on with the file bridge");
        assert!(!saved.lock().unwrap().iter().any(|(k, v)| k == PREF && v == "off"));
        let error = app.connect.error.as_ref().unwrap();
        assert!(error.contains("file bridge"), "{error}");
        assert!(app.session.notices_since(mark).iter().any(|n| &n.text == error));
        // An explicit stop ends the bridge with the choice.
        app.run("ui.mcpRelay", json!({"stop": true})).unwrap();
        assert!(app.file_bridge.is_none() && !enabled(&app));
        assert!(saved.lock().unwrap().iter().any(|(k, v)| k == PREF && v == "off"));
    }
    /// A port variable that isn't a port is said only when agents are on.
    #[test]
    fn a_bad_port_variable_is_said_only_with_agents_on() {
        let mut app = RustGisApp::with_sample();
        let mark = app.session.notice_mark();
        resume_with(&mut app, Err("RUSTGIS_MCP_PORT=x is not a port number".into()));
        assert!(app.session.notices_since(mark).is_empty() && app.connect.error.is_none(), "agents are off");
        let mut app = RustGisApp::with_sample();
        app.services.load_pref = Some(Box::new(|key: &str| (key == PREF).then(|| "on".to_string())));
        resume_with(&mut app, Err("RUSTGIS_MCP_PORT=x is not a port number".into()));
        assert!(app.connect.error.as_deref().is_some_and(|e| e.contains("RUSTGIS_MCP_PORT")));
        assert!(app.relay.is_none());
    }

    /// The app runs what an agent writes to the file bridge on its frame, and says how it went.
    #[test]
    fn the_app_runs_file_bridge_scripts() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = RustGisApp::with_sample();
        let bridge = rustgis_assistant::filebridge::FileBridge::start_in(dir.path().to_path_buf()).unwrap();
        app.file_bridge = Some(bridge.clone());
        let layers = app.session.map().layers.len();
        let ctx = egui::Context::default();
        app.logic(&ctx);
        assert!(bridge.has_wake(), "the frame gives the bridge its wake");
        // Writes a script and runs frames until the app says how it went.
        let run = |app: &mut RustGisApp, script: &str| {
            let mark = app.session.notice_mark();
            std::fs::write(bridge.command_path(), script).unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                if let Some(n) = app.session.notices_since(mark).iter().find(|n| n.text.starts_with("File bridge")) {
                    return n.clone();
                }
                assert!(std::time::Instant::now() < deadline, "the frame never ran the script");
                std::thread::sleep(std::time::Duration::from_millis(50)); // test-wait: legacy polling for the bridge script to run
                app.logic(&ctx);
            }
        };
        let said = run(&mut app, "layer.addSample {\"name\": \"countries\"}\n");
        assert!(said.text == "File bridge: the script ran" && !said.error, "{}", said.text);
        assert_eq!(app.session.map().layers.len(), layers + 1);
        let result: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(bridge.result_path()).unwrap()).unwrap();
        assert_eq!(result["success"], true, "{result}");
        // A failing script: an error notice naming the line, and nothing kept.
        let said = run(&mut app, "layer.addSample {\"name\": \"rivers\"}\nmap.basemap {\"name\": \"nowhere\"}\n");
        assert!(said.error && said.text.starts_with("File bridge: line 2"), "{}", said.text);
        assert_eq!(app.session.map().layers.len(), layers + 1);
    }

    /// Full access counts only while agents are on, is never read from the preferences, and goes
    /// when agents are turned off.
    #[test]
    fn full_access_needs_agents_on_and_a_fresh_opt_in() {
        let mut app = RustGisApp::with_sample();
        app.services.load_pref = Some(Box::new(|key: &str| (key == PREF || key == "agents_full_access").then(|| "on".to_string())));
        assert!(enabled(&app) && !full_access(&app), "a launch never restores full access");
        app.connect.full = true;
        assert!(full_access(&app));
        app.connect.on = Some(false);
        assert!(!full_access(&app), "agents are off");
        app.connect.on = Some(true);
        stopped(&mut app);
        app.connect.on = Some(true);
        assert!(!full_access(&app), "turning agents off ends full access");
    }

    #[test]
    fn explicit_stop_updates_cached_and_remembered_choice() {
        use std::sync::{Arc, Mutex};
        let saved = Arc::new(Mutex::new(String::from("on")));
        let mut app = RustGisApp::with_sample();
        let read = saved.clone();
        app.services.load_pref = Some(Box::new(move |key| (key == PREF).then(|| read.lock().unwrap().clone())));
        let write = saved.clone();
        app.services.save_pref = Some(Box::new(move |key, value| {
            if key == PREF {
                *write.lock().unwrap() = value.into();
            }
        }));
        let bridge_dir = tempfile::tempdir().unwrap();
        app.connect.bridge_dir = Some(bridge_dir.path().to_path_buf());
        resume_on(&mut app, 0);
        assert!(app.relay.is_some());
        assert!(enabled(&app));
        app.run("ui.mcpRelay", json!({"stop":true})).unwrap();
        assert!(app.relay.is_none());
        assert!(!enabled(&app));
        assert_eq!(*saved.lock().unwrap(), "off");
    }

    #[test]
    fn copyable_program_paths_are_literal_shell_arguments() {
        #[cfg(not(windows))]
        {
            let value = r#"space path/'"$(printf BAD)`printf BAD`\agent"#;
            let script = format!("printf '%s' {}", shell_arg(value));
            let result = std::process::Command::new("/bin/sh").args(["-c", &script]).output().unwrap();
            assert!(result.status.success());
            assert_eq!(String::from_utf8(result.stdout).unwrap(), value);
        }
        #[cfg(windows)]
        assert_eq!(shell_arg("C:\\a'b\\$file.exe"), "'C:\\a''b\\$file.exe'");
    }
}
