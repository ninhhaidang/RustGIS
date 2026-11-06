//! A plugin command the user asks for in the interface starts a frame or two later, so the window
//! first shows that it is running (#1319): the command may wait for a server on this thread, and
//! nothing is drawn until it has finished. Clicks made meanwhile, which the window system holds
//! until then, are dropped, so a second click out of impatience doesn't run it again. Scripts, the
//! AI Assistant and attached agents run commands at once ([`RustGisApp::run`]).
//!
//! A command that isn't a plugin's, asked for during the wait, runs first. One that replaces the
//! project takes the commands waiting with it, and says so: they name the old project's layers.

use crate::{RustGisApp, i18n};
use serde_json::Value;
use std::collections::VecDeque;
use std::time::Duration;

/// Frames drawn before a command runs: the one it was asked in may have drawn the status bar
/// already, so the next is the one that shows it.
const FRAMES_SHOWN: u8 = 2;

/// A run this long has clicks held for it that were meant for a window that wasn't answering.
#[cfg(not(test))]
const STALE_AFTER: Duration = Duration::from_millis(150);
/// In tests, every run: the commands there are quick.
#[cfg(test)]
const STALE_AFTER: Duration = Duration::ZERO;

struct Start {
    id: String,
    params: Value,
    frames: u8,
    /// The project it was asked for in ([`rustgis_engine::Session::opened`]).
    opened: u64,
}

#[derive(Default)]
pub struct State {
    waiting: VecDeque<Start>,
    /// A command that took long has just run: the clicks arriving next were made during it.
    clicks_stale: bool,
}

/// Leaves plugin command `id` to start once the window has shown it. The same command with the
/// same parameters, already waiting, isn't asked twice.
pub(crate) fn ask(app: &mut RustGisApp, id: &str, params: Value) {
    let opened = app.session.opened;
    let waiting = &mut app.ui.plugin_start.waiting;
    if !waiting.iter().any(|s| s.id == id && s.params == params && s.opened == opened) {
        waiting.push_back(Start { id: id.to_string(), params, frames: 0, opened });
    }
}

/// Drops the commands asked for in a project another has replaced, and says which: they would act
/// on a namesake in the new one.
fn drop_replaced(app: &mut RustGisApp) {
    let opened = app.session.opened;
    let (kept, dropped): (VecDeque<_>, VecDeque<_>) =
        std::mem::take(&mut app.ui.plugin_start.waiting).into_iter().partition(|s| s.opened == opened);
    app.ui.plugin_start.waiting = kept;
    if !dropped.is_empty() {
        let commands = dropped.iter().map(|s| title(app, &s.id)).collect::<Vec<_>>().join(", ");
        app.session.notify(i18n::fmt("Not run, because another project opened: {commands}.", &[("commands", &commands)]), true);
    }
}

/// Runs the command that has been shown. Its errors are notices, as for any command.
///
/// Not while a pointer button is down, or in the frame it comes up: the run would swallow that
/// release ([`RustGisApp::drop_stale_clicks`]), and egui would hold the button down until the next
/// click. The click that release makes reaches [`ask`] after this, in the frame's drawing, and
/// finds the command still waiting. A window that has lost focus doesn't wait: the release may
/// never come, and egui clears no button then. Should it come, it isn't swallowed: a run begun with
/// a button down drops no clicks.
pub(crate) fn run_next(app: &mut RustGisApp, ctx: &egui::Context) {
    drop_replaced(app);
    if app.ui.plugin_start.waiting.front().is_none_or(|s| s.frames < FRAMES_SHOWN)
        || ctx.input(|i| i.focused && (i.pointer.any_down() || i.pointer.any_released()))
    {
        return;
    }
    let Some(start) = app.ui.plugin_start.waiting.pop_front() else { return };
    let held = ctx.input(|i| i.pointer.any_down());
    let began = web_time::Instant::now();
    let _ = app.run(&start.id, start.params);
    app.ui.plugin_start.clicks_stale = !held && began.elapsed() >= STALE_AFTER;
}

/// Runs again the commands whose downloads have come in. A long run holds clicks too, and one of
/// them could run the command once more, so they go as [`run_next`]'s do; nothing else may ask for
/// the frame that drops them.
pub(crate) fn run_again(app: &mut RustGisApp, ctx: &egui::Context) {
    let held = ctx.input(|i| i.pointer.any_down());
    let began = web_time::Instant::now();
    let ran = !app.session.poll_plugin_retries().is_empty();
    if ran && !held && began.elapsed() >= STALE_AFTER {
        app.ui.plugin_start.clicks_stale = true;
        ctx.request_repaint();
    }
}

/// At the end of a frame: a command waiting has been shown once more, with a busy cursor, and
/// another frame is wanted (nothing else may ask for one).
pub(crate) fn frame_drawn(app: &mut RustGisApp, ctx: &egui::Context) {
    if let Some(start) = app.ui.plugin_start.waiting.front_mut() {
        start.frames = start.frames.saturating_add(1);
        ctx.set_cursor_icon(egui::CursorIcon::Wait);
        ctx.request_repaint();
    }
}

/// What the status bar says while a command waits to start: `Running BrasilData: Solos…`.
pub(crate) fn running(app: &RustGisApp) -> Option<String> {
    let title = title(app, &app.ui.plugin_start.waiting.front()?.id);
    Some(i18n::fmt("Running {command}…", &[("command", &title)]))
}

/// A plugin command as the user knows it: `BrasilData: Solos`.
fn title(app: &RustGisApp, id: &str) -> String {
    let plugin = rustgis_engine::plugins::split_id(id).and_then(|(plugin, item)| Some((rustgis_engine::plugins::get(plugin)?, item)));
    plugin.map_or_else(
        || id.to_string(),
        |(p, item)| {
            let m = &p.manifest;
            let label = m.commands.iter().find(|c| c.id == item).map_or(item, |c| m.translate(app.lang().code(), &c.label));
            i18n::fmt("{plugin}: {command}", &[("plugin", &m.name), ("command", &label.replace('\n', " "))])
        },
    )
}

impl RustGisApp {
    /// Before a frame, from the window: drops the clicks made during a long plugin command, press
    /// and release alike, so none is half taken. It only follows a run begun with no button down,
    /// so this drops no release of a press egui has seen (`run_next`); a release left for the next
    /// frame, its press dropped, is no click to egui.
    ///
    /// These events carry no time, and the window reads them in one batch: a click made after the
    /// command returned, before this frame's input was read, goes with them. That frame comes right
    /// after the one that ran the command, as egui repaints twice for `frame_drawn`'s request, so
    /// this is a frame's time at most.
    pub fn drop_stale_clicks(&mut self, input: &mut egui::RawInput) {
        if std::mem::take(&mut self.ui.plugin_start.clicks_stale) {
            input.events.retain(|e| !matches!(e, egui::Event::PointerButton { .. }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, Modifiers, PointerButton, pos2, vec2};
    use egui_kittest::kittest::Queryable;
    use rustgis_engine::plugins;
    use serde_json::json;

    fn launch(plugin: &str) -> egui_kittest::Harness<'static, RustGisApp> {
        let pkg = plugins::Package::from_zip(&plugins::testing::package(plugin, &["edit"])).unwrap();
        plugins::load(pkg, "test").unwrap();
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1400.0, 860.0)).with_max_steps(64).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            RustGisApp::with_sample(),
        );
        h.run();
        h
    }

    fn rename(from: &str, to: &str) -> Value {
        json!({"op": "layer.rename", "params": {"layer": from, "name": to}})
    }

    /// Asked for by the user, a command runs only after a frame has said so; asked twice, once.
    #[test]
    fn a_command_is_shown_running_before_it_runs() {
        let mut h = launch("ui-start");
        let id = "plugin:ui-start/f";
        let mark = h.state().session.notice_mark();
        h.state_mut().run_by_user(id, rename("Rivers", "Streams")).unwrap();
        h.state_mut().run_by_user(id, rename("Rivers", "Streams")).unwrap();
        h.step();
        assert!(h.state().session.map().layer_by_name("Rivers").is_some(), "not yet");
        h.step();
        assert!(h.state().session.map().layer_by_name("Rivers").is_some(), "not before it is shown");
        assert!(running(h.state()).is_some_and(|t| t.starts_with("Running") && t.contains("ui-start")), "{:?}", running(h.state()));
        let _ = h.get_by_label_contains("Running");
        h.run();
        assert!(h.state().session.map().layer_by_name("Streams").is_some());
        // Run again, it would find no Rivers.
        assert!(!h.state().session.notices_since(mark).iter().any(|n| n.error), "run once");
        assert!(running(h.state()).is_none());
        plugins::unload("ui-start");
    }

    /// Another command asked for meanwhile runs after it, in turn; `run` (scripts, the assistant)
    /// doesn't wait.
    #[test]
    fn commands_asked_for_run_in_turn() {
        let mut h = launch("ui-turn");
        let id = "plugin:ui-turn/f";
        h.state_mut().run_by_user(id, rename("Rivers", "Streams")).unwrap();
        h.state_mut().run_by_user(id, rename("Streams", "Brooks")).unwrap();
        h.run();
        assert!(h.state().session.map().layer_by_name("Brooks").is_some());
        h.state_mut().run(id, rename("Brooks", "Creeks")).unwrap();
        assert!(h.state().session.map().layer_by_name("Creeks").is_some());
        plugins::unload("ui-turn");
    }

    /// Clicks are dropped once, after a long run; what the pointer did otherwise stays.
    #[test]
    fn clicks_made_during_a_long_run_are_dropped_once() {
        let mut app = RustGisApp::with_sample();
        let at = pos2(10.0, 10.0);
        let button = |pressed| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        let events = || vec![Event::PointerMoved(at), button(true), button(false)];
        let mut input = egui::RawInput { events: events(), ..Default::default() };
        app.drop_stale_clicks(&mut input);
        assert_eq!(input.events.len(), 3, "none stale");
        app.ui.plugin_start.clicks_stale = true;
        app.drop_stale_clicks(&mut input);
        assert_eq!(input.events, vec![Event::PointerMoved(at)]);
        let mut input = egui::RawInput { events: events(), ..Default::default() };
        app.drop_stale_clicks(&mut input);
        assert_eq!(input.events.len(), 3, "only those of that run");
    }

    /// A button held when a command comes to start: it waits for the release, and a frame more.
    /// Run while the button was down, the release would come in the frame whose clicks are
    /// dropped, and egui would keep the button down until the next click.
    #[test]
    fn a_command_waits_for_a_held_button() {
        let mut h = launch("ui-held");
        let id = "plugin:ui-held/f";
        let at = pos2(700.0, 400.0);
        let button = |pressed| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        h.event(Event::PointerMoved(at));
        h.event(button(true));
        h.step();
        h.state_mut().run_by_user(id, rename("Rivers", "Streams")).unwrap();
        for _ in 0..4 {
            h.step();
        }
        // What a long run leaves behind, if it has run.
        let ran = h.state().session.map().layer_by_name("Streams").is_some();
        h.state_mut().ui.plugin_start.clicks_stale = ran;
        let mut input = egui::RawInput { events: vec![button(false)], ..Default::default() };
        h.state_mut().drop_stale_clicks(&mut input);
        for event in input.events {
            h.event(event);
        }
        h.step();
        assert!(!h.ctx.input(|i| i.pointer.any_down()), "the button is up again");
        assert!(!ran, "it waited for the release");
        assert!(h.state().session.map().layer_by_name("Rivers").is_some(), "the release's own click comes after `logic`");
        h.step();
        assert!(h.state().session.map().layer_by_name("Streams").is_some());
        plugins::unload("ui-held");
    }

    /// A window that loses focus with a button down may never hear the release, and egui keeps the
    /// button down: the command doesn't wait for it.
    #[test]
    fn a_command_does_not_wait_for_a_button_once_the_window_has_lost_focus() {
        let mut h = launch("ui-left");
        let at = pos2(700.0, 400.0);
        h.event(Event::PointerMoved(at));
        h.event(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
        h.step();
        h.state_mut().run_by_user("plugin:ui-left/f", rename("Rivers", "Streams")).unwrap();
        for _ in 0..4 {
            h.step();
        }
        assert!(h.state().session.map().layer_by_name("Rivers").is_some(), "it waits while the window has focus");
        h.input_mut().focused = false;
        h.event(Event::WindowFocused(false));
        h.step();
        assert!(h.ctx.input(|i| i.pointer.any_down()), "egui keeps the button down");
        assert!(h.state().session.map().layer_by_name("Streams").is_some());
        plugins::unload("ui-left");
    }

    /// The release of a button held through a run, in a window that lost focus, reaches egui, which
    /// would otherwise keep the button down until the next click. With no button down, a long
    /// run's clicks are dropped.
    #[test]
    fn a_release_after_a_run_begun_with_a_button_down_is_kept() {
        let mut h = launch("ui-late");
        let id = "plugin:ui-late/f";
        let at = pos2(700.0, 400.0);
        let button = |pressed| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        h.event(Event::PointerMoved(at));
        h.event(button(true));
        h.step();
        h.state_mut().run_by_user(id, rename("Rivers", "Streams")).unwrap();
        h.input_mut().focused = false;
        h.event(Event::WindowFocused(false));
        for _ in 0..4 {
            h.step();
        }
        assert!(h.state().session.map().layer_by_name("Streams").is_some());
        let mut input = egui::RawInput { events: vec![button(false)], ..Default::default() };
        h.state_mut().drop_stale_clicks(&mut input);
        for event in input.events {
            h.event(event);
        }
        h.step();
        assert!(!h.ctx.input(|i| i.pointer.any_down()), "the button is up again");
        h.state_mut().run_by_user(id, rename("Streams", "Brooks")).unwrap();
        for _ in 0..4 {
            h.step();
        }
        assert!(h.state().session.map().layer_by_name("Brooks").is_some());
        assert!(h.state().ui.plugin_start.clicks_stale);
        plugins::unload("ui-late");
    }

    /// A command run again when its download lands holds clicks as a first run does; the frames
    /// before it, with nothing to run, drop none. With a button down, none either: its release has
    /// to reach egui.
    #[test]
    fn clicks_held_for_a_command_run_again_are_dropped() {
        let mut h = launch("ui-again");
        let pkg = plugins::Package::from_zip(&plugins::testing::package("ui-again-net", &["edit", "network"])).unwrap();
        plugins::load(pkg, "test").unwrap();
        let at = pos2(700.0, 400.0);
        let button = |pressed| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        h.event(Event::PointerMoved(at));
        for held in [false, true] {
            let url = format!("https://x.org/{held}.csv");
            let params = json!({"op": "layer.addUrl", "params": {"url": url}});
            h.state_mut().run("plugin:ui-again-net/f", params).unwrap();
            // Taken before a frame would: it would start the download.
            let taken = h.state_mut().session.take_url_requests();
            assert_eq!(taken.len(), 1, "{taken:?}");
            let id = h.state_mut().session.download_started();
            h.step();
            assert!(!h.state().ui.plugin_start.clicks_stale, "nothing ran");
            assert!(h.state().session.plugin_retries_waiting());
            if held {
                h.event(button(true));
                h.step();
            }
            let landed = crate::serviceui::Landed { id, url, params: taken[0].clone(), origin: None };
            h.state_mut().add_landed(landed, b"name,lon,lat\nA,10,50\n".to_vec());
            h.step();
            assert!(!h.state().session.plugin_retries_waiting(), "it ran again");
            assert_eq!(h.state().ui.plugin_start.clicks_stale, !held);
            h.state_mut().ui.plugin_start.clicks_stale = false;
        }
        plugins::unload("ui-again-net");
        plugins::unload("ui-again");
    }

    /// The frame that runs a command drops the clicks held for it in the next one, which has to
    /// come whatever the user does. It does with no other command waiting: `frame_drawn` asked for
    /// the frame that runs it, and egui answers such a request with one frame more.
    #[test]
    fn egui_gives_a_frame_after_the_one_a_repaint_was_asked_for() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let asked = std::sync::Arc::new(AtomicU32::new(0));
        let ctx = egui::Context::default();
        let count = asked.clone();
        ctx.set_request_repaint_callback(move |info| {
            if info.delay.is_zero() {
                count.fetch_add(1, Ordering::Relaxed);
            }
        });
        let frame = |ctx: &egui::Context, ask: bool| {
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                if ask {
                    ui.ctx().request_repaint();
                }
            });
            output.textures_delta.clear();
        };
        for _ in 0..4 {
            frame(&ctx, false);
        }
        assert!(!ctx.has_requested_repaint(), "settled");
        frame(&ctx, true);
        asked.store(0, Ordering::Relaxed);
        frame(&ctx, false);
        assert_eq!(asked.load(Ordering::Relaxed), 1, "the frame that runs the command asks for the next");
        asked.store(0, Ordering::Relaxed);
        frame(&ctx, false);
        assert_eq!(asked.load(Ordering::Relaxed), 0, "and that one for no more");
    }

    /// A project replacing this one takes the commands waiting in it, which name its layers, and
    /// says so; with none waiting, it says nothing.
    #[test]
    fn a_new_project_drops_the_commands_waiting_and_says_so() {
        let mut h = launch("ui-gone");
        let id = "plugin:ui-gone/f";
        let said = |h: &egui_kittest::Harness<'static, RustGisApp>, mark| {
            h.state().session.notices_since(mark).iter().filter(|n| n.text.contains("Not run") && n.text.contains("ui-gone")).count()
        };
        let mark = h.state().session.notice_mark();
        h.state_mut().run_by_user("project.sample", json!({})).unwrap();
        h.run();
        assert_eq!(said(&h, mark), 0, "none waiting");
        h.state_mut().run_by_user(id, rename("Rivers", "Streams")).unwrap();
        h.state_mut().run_by_user("project.sample", json!({})).unwrap();
        h.run();
        assert!(h.state().session.map().layer_by_name("Rivers").is_some(), "the new project's namesake is left alone");
        assert_eq!(said(&h, mark), 1);
        assert!(running(h.state()).is_none());
        // Asked for in the new project, it runs there.
        h.state_mut().run_by_user(id, rename("Rivers", "Streams")).unwrap();
        h.run();
        assert!(h.state().session.map().layer_by_name("Streams").is_some());
        assert_eq!(said(&h, mark), 1);
        plugins::unload("ui-gone");
    }
}
