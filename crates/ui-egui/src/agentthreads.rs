//! Agent threads on this computer (desktop). The assistant hands tasks to coding agents through
//! ACP ([`rustgis_assistant::acp`]); each task starts a thread that works in the background while
//! the conversation goes on, with a small window showing what it does and asking the user what it
//! asks. When it finishes, its report goes to the assistant, and the thread waits for follow-ups
//! with what it did in its context. Several threads can work at once.
//!
//! Threads end with a new conversation, when the user closes them, or after half an hour idle.

use std::path::PathBuf;
use std::time::Duration;

use egui::{Align, Layout, RichText, Ui, vec2};
use rustgis_assistant::acp::{Choice, Event, Permission};

use crate::RustGisApp;
use crate::localagents::Run;
use crate::theme::{Tokens, semibold};

/// How long a thread may wait for a follow-up before it's closed.
const IDLE_LIMIT: Duration = Duration::from_secs(30 * 60);

/// A line in a thread's window.
enum Feed {
    /// A task it was given.
    Task(String),
    Said(String),
    Tool(String),
    Plan(String),
}

struct ThreadRun {
    id: String,
    label: &'static str,
    run: Run,
    feed: Vec<Feed>,
    requests: Vec<Permission>,
    working: bool,
    /// When the task started, or when it went idle.
    since: web_time::Instant,
    window: bool,
    /// The newest of its RustGIS actions seen waiting (their ids only grow): the window opens when
    /// a newer one waits, so Hide still works while the same ones wait.
    map_seen: Option<u64>,
}

/// The most entries a thread's window keeps (the oldest go first).
const MAX_FEED: usize = 500;

impl ThreadRun {
    fn feed(&mut self, f: Feed) {
        if self.feed.len() >= MAX_FEED {
            self.feed.remove(0);
        }
        self.feed.push(f);
    }
}

/// The threads, and whether they started the relay (and stop it when the last one ends).
#[derive(Default)]
pub struct Threads {
    runs: Vec<ThreadRun>,
    relay_ours: bool,
}

impl Threads {
    /// Publish the shared approval setting synchronously to every live ACP session.
    pub(crate) fn set_auto(&self, auto: bool) {
        for t in &self.runs {
            t.run.set_auto(auto);
        }
    }
}

/// Put the real process fixture into the same live-thread collection the checkbox updates.
#[cfg(test)]
pub(crate) fn insert_test_run(app: &mut RustGisApp, run: Run) {
    app.assistant.threads.runs.push(ThreadRun {
        id: "goose-1".into(),
        label: "goose",
        run,
        feed: Vec::new(),
        requests: Vec::new(),
        working: true,
        since: web_time::Instant::now(),
        window: false,
        map_seen: None,
    });
}

impl Drop for Threads {
    fn drop(&mut self) {
        // Ordinary Close/New conversation clears runs without blocking. Actual application
        // shutdown also waits for cleanup workers, so no child is abandoned on process exit.
        self.runs.clear();
        crate::localagents::finish_stops();
    }
}

/// `rustgis-cli` beside the app.
fn cli() -> Option<PathBuf> {
    let name = if cfg!(windows) { "rustgis-cli.exe" } else { "rustgis-cli" };
    std::env::current_exe().ok().map(|p| p.with_file_name(name)).filter(|p| p.is_file())
}

/// Starts the tasks the assistant handed out, passes on what threads did, asked and reported,
/// and gives the assistant the reports once it's free.
pub fn poll(app: &mut RustGisApp, ctx: &egui::Context) {
    // Apply restored/programmatic setting changes before queued tasks or follow-ups start.
    let auto = app.assistant.agents_auto;
    app.assistant.threads.set_auto(auto);
    let delegations = app.assistant.agent.as_mut().map(|a| a.take_delegations()).unwrap_or_default();
    for d in delegations {
        start(app, d, ctx);
    }
    let (mut reports, mut closed) = (Vec::new(), Vec::new());
    for t in &mut app.assistant.threads.runs {
        let newest = t.run.map_approvals().iter().map(|a| a.id).max();
        if newest > t.map_seen {
            t.window = true;
            t.map_seen = newest;
        }
        // Automatic approval allows once; a request without a once-only option waits for the
        // user (Always allow or Decline) instead of being cancelled.
        if auto {
            let (once, other): (Vec<_>, Vec<_>) = std::mem::take(&mut t.requests).into_iter().partition(|p| p.can_once);
            for p in once {
                t.run.answer(&p, Choice::Allow);
            }
            t.requests = other;
        }
        for e in t.run.events() {
            match e {
                Event::Permission(p) if auto && p.can_once => t.run.answer(&p, Choice::Allow),
                Event::Permission(p) => {
                    t.requests.push(p);
                    t.window = true;
                }
                Event::Text(s) => t.feed(Feed::Said(s)),
                Event::Tool(s) => t.feed(Feed::Tool(s)),
                Event::Plan(p) => t.feed(Feed::Plan(p.join(" · "))),
                Event::Done { report, error } => {
                    t.working = false;
                    t.window = !t.run.map_approvals().is_empty();
                    t.requests.clear();
                    t.since = web_time::Instant::now();
                    reports.push((t.id.clone(), format!("{report}\n\n(Its files: {})", t.run.work.display()), error));
                }
                Event::Closed => closed.push(t.id.clone()),
            }
        }
        if !t.working && (t.run.finished() || t.since.elapsed() > IDLE_LIMIT) {
            closed.push(t.id.clone());
        }
    }
    if let Some(agent) = app.assistant.agent.as_mut() {
        for (id, report, error) in reports {
            agent.thread_report(&id, report, error);
        }
    }
    for id in closed {
        close(app, &id);
    }
    deliver(app, ctx);
}

/// The threads' reports, to the assistant once it's free.
fn deliver(app: &mut RustGisApp, ctx: &egui::Context) {
    let Some(mut agent) = app.assistant.agent.take() else { return };
    let step = agent.deliver(&app.session);
    app.assistant.agent = Some(agent);
    if let Some(step) = step {
        crate::assistantpane::follow(app, step, ctx);
    }
}

/// Starts a thread on a task, or sends a follow-up to one.
fn start(app: &mut RustGisApp, d: rustgis_assistant::agent::Delegation, ctx: &egui::Context) {
    if d.follow_up {
        let auto = app.assistant.agents_auto;
        let sent = app.assistant.threads.runs.iter_mut().find(|t| t.id == d.thread).is_some_and(|t| {
            let sent = !t.working && t.run.follow_up(&d.task, auto);
            if sent {
                t.working = true;
                t.window = true;
                t.since = web_time::Instant::now();
                t.feed(Feed::Task(d.task.clone()));
            }
            sent
        });
        if !sent {
            fail(app, &d.thread, format!("{} couldn't take the follow-up: its thread has ended.", d.agent.label));
        }
        return;
    }
    // The agent reaches the map through the app's MCP relay, started for it if it's off.
    if app.relay.is_none() {
        app.assistant.threads.relay_ours = app.run("ui.mcpRelay", serde_json::json!({})).is_ok() && app.relay.is_some();
    }
    let auto = app.assistant.agents_auto;
    let started = match (app.relay.as_ref(), rustgis_assistant::acp::command(d.agent), cli()) {
        (Some(relay), Some(command), Some(cli)) => crate::localagents::work_folder(d.agent.id)
            .and_then(|work| Run::start(d.agent, &command, &d.task, relay, &cli, work, auto, ctx.clone())),
        (None, _, _) => Err("the MCP relay couldn't start".to_string()),
        (_, None, _) => Err(format!("{} can't be started here (it, or Node.js for its ACP adapter, wasn't found)", d.agent.label)),
        (_, _, None) => Err("rustgis-cli wasn't found beside RustGIS".to_string()),
    };
    match started {
        Ok(run) => app.assistant.threads.runs.push(ThreadRun {
            id: d.thread,
            label: d.agent.label,
            run,
            feed: vec![Feed::Task(d.task)],
            requests: Vec::new(),
            working: true,
            since: web_time::Instant::now(),
            window: true,
            map_seen: None,
        }),
        Err(e) => fail(app, &d.thread, e),
    }
}

/// A thread that couldn't do its task: the assistant hears why, and the thread is gone.
fn fail(app: &mut RustGisApp, id: &str, why: String) {
    if let Some(agent) = app.assistant.agent.as_mut() {
        agent.thread_report(id, why, true);
    }
    close(app, id);
}

/// Ends a thread (stopping its agent and everything it started).
fn close(app: &mut RustGisApp, id: &str) {
    app.assistant.threads.runs.retain(|t| t.id != id);
    if let Some(agent) = app.assistant.agent.as_mut() {
        agent.thread_closed(id);
    }
    stop_relay_if_unused(app);
}

/// Stops a working thread at the user's word: the assistant hears that, and the thread ends.
fn stop(app: &mut RustGisApp, id: &str) {
    let label = app.assistant.threads.runs.iter().find(|t| t.id == id).map(|t| t.label).unwrap_or_default();
    let working = app.assistant.threads.runs.iter().any(|t| t.id == id && t.working);
    if working && let Some(agent) = app.assistant.agent.as_mut() {
        agent.thread_report(id, crate::i18n::fmt("{agent} was stopped by you.", &[("agent", label)]), true);
    }
    close(app, id);
}

fn stop_relay_if_unused(app: &mut RustGisApp) {
    if app.assistant.threads.runs.is_empty() && std::mem::take(&mut app.assistant.threads.relay_ours) && !crate::connectui::enabled(app) {
        let _ = app.run("ui.mcpRelay", serde_json::json!({"stop": true}));
    }
}

/// Ends every thread (a new conversation).
pub fn end_all(app: &mut RustGisApp) {
    app.assistant.threads.runs.clear();
    stop_relay_if_unused(app);
}

fn clock(since: web_time::Instant) -> String {
    let secs = since.elapsed().as_secs();
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// The threads in the assistant pane: each one's state, Show or Hide its window, and Close.
pub fn list(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    if app.assistant.threads.runs.is_empty() {
        return;
    }
    let mut close_id = None;
    ui.label(RichText::new(tl!("Agent threads")).small().strong());
    for th in &mut app.assistant.threads.runs {
        ui.horizontal(|ui| {
            let (state, color) = if !th.requests.is_empty() || !th.run.map_approvals().is_empty() {
                (tl!("Waiting for your answer").to_string(), t.amber)
            } else if th.working {
                (format!("{} {}", tl!("Working"), clock(th.since)), t.accent_text)
            } else {
                (tl!("Idle").to_string(), t.text_dim)
            };
            ui.label(RichText::new(format!("{} · {}", th.id, th.label)).small());
            ui.label(RichText::new(state).small().color(color));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.small_button("×").on_hover_text(tl!("Close thread")).clicked() {
                    close_id = Some(th.id.clone());
                }
                if th.working || !th.requests.is_empty() || !th.run.map_approvals().is_empty() {
                    let label = if th.window { tl!("Hide") } else { tl!("Show") };
                    if ui.small_button(label).clicked() {
                        th.window = !th.window;
                    }
                }
            });
        });
    }
    ui.add_space(4.0);
    if let Some(id) = close_id {
        stop(app, &id);
    }
    if app.assistant.threads.runs.iter().any(|th| th.working || !th.run.map_approvals().is_empty()) {
        ui.ctx().request_repaint_after(Duration::from_millis(500));
    }
}

/// A small window for each working thread: what its agent says and does, what it asks, and Stop.
pub fn windows(app: &mut RustGisApp, ctx: &egui::Context) {
    if app.assistant.threads.runs.is_empty() {
        return;
    }
    let t = Tokens::get(ctx);
    let screen = ctx.content_rect();
    let mut stop_id = None;
    let mut answers = Vec::new();
    for (i, th) in app.assistant.threads.runs.iter_mut().enumerate() {
        if !th.window || !(th.working || !th.requests.is_empty() || !th.run.map_approvals().is_empty()) {
            continue;
        }
        let offset = 28.0 * i as f32;
        let mut open = true;
        egui::Window::new(format!("{} · {}", th.label, th.id))
            .id(egui::Id::unique(("acp_thread", th.id.as_str())))
            .open(&mut open)
            .collapsible(true)
            .resizable(true)
            .default_size(vec2(380.0, 320.0))
            .default_pos(egui::pos2(screen.right() - 400.0 - offset, screen.bottom() - 380.0 - offset))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new(clock(th.since)).small().color(t.text_dim));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button(tl!("Stop")).clicked() {
                            stop_id = Some(th.id.clone());
                        }
                        if ui.small_button(tl!("Open folder")).clicked() {
                            let _ = crate::open_with_system(&th.run.work);
                        }
                    });
                });
                if let Some(p) = th.run.map_approvals().first() {
                    egui::Frame::NONE.fill(t.amber.gamma_multiply(0.15)).corner_radius(6).inner_margin(8).show(ui, |ui| {
                        ui.label(RichText::new(tl!("Allow this agent's RustGIS action?")).strong());
                        if th.run.map_approvals().len() > 1 {
                            ui.label(crate::i18n::fmt("{count} requests waiting", &[("count", &th.run.map_approvals().len().to_string())]));
                        }
                        // Overview and exact Details share one scroll area; controls stay outside.
                        egui::ScrollArea::vertical().max_height(160.0).id_salt(("acp_approval", p.id)).show(ui, |ui| {
                            if p.reviews.is_empty() {
                                ui.label(RichText::new(&p.summary).monospace().small());
                            } else {
                                for (index, review) in p.reviews.iter().enumerate() {
                                    ui.push_id(index, |ui| {
                                        if p.reviews.len() > 1 {
                                            ui.separator();
                                            ui.label(crate::i18n::fmt(
                                                "Request {current} of {count}",
                                                &[("current", &(index + 1).to_string()), ("count", &p.reviews.len().to_string())],
                                            ));
                                        }
                                        crate::approvalcard::show(ui, &app.session.registry, review);
                                    });
                                }
                            }
                        });
                        ui.horizontal(|ui| {
                            if ui.button(tl!("Allow")).clicked() {
                                th.run.answer_map(p.id, true);
                            }
                            if ui.button(tl!("Decline")).clicked() {
                                th.run.answer_map(p.id, false);
                            }
                        });
                    });
                }
                if let Some(p) = th.requests.first() {
                    egui::Frame::NONE
                        .fill(t.amber.gamma_multiply(0.15))
                        .corner_radius(6)
                        .inner_margin(8)
                        .stroke(egui::Stroke::new(1.0, t.amber))
                        .show(ui, |ui| {
                            ui.label(RichText::new(tl!("An agent on this computer asks to:")).font(semibold(12.5)));
                            ui.label(RichText::new(&p.title).small());
                            if !p.detail.is_empty() {
                                // The whole input (up to 4 KB), scrolled rather than cut.
                                egui::ScrollArea::vertical().max_height(160.0).id_salt(("acp_permission", i)).show(ui, |ui| {
                                    ui.label(RichText::new(&p.detail).monospace().small());
                                });
                            }
                            if !p.can_once {
                                ui.label(tl!("This request has no one-time allow option."));
                            }
                            ui.horizontal(|ui| {
                                if crate::widgets::primary_button(ui, tl!("Allow"), p.can_once && p.complete, vec2(70.0, 26.0)).clicked() {
                                    answers.push((i, Choice::Allow));
                                }
                                if p.can_always && ui.add_enabled(p.complete, egui::Button::new(tl!("Always allow"))).clicked() {
                                    answers.push((i, Choice::AllowAlways));
                                }
                                if ui.button(tl!("Decline")).clicked() {
                                    answers.push((i, Choice::Reject));
                                }
                            });
                        });
                }
                ui.separator();
                egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
                    for f in &th.feed {
                        match f {
                            Feed::Task(s) => {
                                ui.label(RichText::new(s).small().strong());
                            }
                            Feed::Said(s) => {
                                ui.label(RichText::new(s).small());
                            }
                            Feed::Tool(s) => {
                                ui.label(RichText::new(format!("• {s}")).small().color(t.text_dim));
                            }
                            Feed::Plan(s) => {
                                ui.label(RichText::new(s).small().italics().color(t.text_dim));
                            }
                        }
                    }
                });
            });
        // Closing the window hides it; the thread goes on (Show brings it back).
        if !open {
            th.window = false;
        }
    }
    for (i, choice) in answers {
        if let Some(th) = app.assistant.threads.runs.get_mut(i)
            && !th.requests.is_empty()
        {
            let p = th.requests.remove(0);
            th.run.answer(&p, choice);
        }
    }
    if let Some(id) = stop_id {
        stop(app, &id);
    }
    // Clocks and feeds move without input.
    ctx.request_repaint_after(Duration::from_millis(500));
}
