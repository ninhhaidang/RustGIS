//! RustGIS desktop app.
//!
//! `rustgis [project.rgis | data.geojson|csv|shp …] [--sample] [--backend dx12|vulkan|gl|metal]`
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use eframe::wgpu;
use rustgis_engine::{Project, Session};
use rustgis_ui_egui::RustGisApp;
use serde_json::json;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
mod menu;

/// The app, and this run's marker (see [`running`]), cleared when eframe closes it.
struct App(RustGisApp, running::Shared, Menus);

/// macOS: the menu bar (built once the app runs), the ids of the items chosen from it, and
/// the commands chosen and not run yet.
#[cfg(target_os = "macos")]
#[derive(Default)]
struct Menus {
    bar: Option<menu::MenuBar>,
    /// What the last build was for: a failed one is tried again only when that changes.
    tried: Option<menu::Signature>,
    /// The language and keymap the signature was last taken for: it's taken again only then.
    seen: Option<(rustgis_ui_egui::i18n::Lang, u64)>,
    chosen: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    pending: Vec<menu::Action>,
}
#[cfg(not(target_os = "macos"))]
#[derive(Default)]
struct Menus {}

/// Preferences in `prefs.json` beside the platform's other app settings.
mod prefs {
    use std::path::PathBuf;

    use rustgis_ui_egui::Services;
    use serde_json::{Map, Value};

    pub fn dir() -> Option<PathBuf> {
        rustgis_engine::data::dirs::config_dir()
    }

    fn read() -> Map<String, Value> {
        static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        rustgis_engine::prefs::read().unwrap_or_else(|error| {
            if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                log::warn!("{error}");
            }
            Map::new()
        })
    }

    pub fn services() -> Services {
        let secrets: std::sync::Arc<dyn rustgis_engine::data::secrets::SecretStore> =
            std::sync::Arc::new(rustgis_keychain::Keychain::default());
        // The ChatGPT sign-in's tokens go to the same keychain (its accounts stay in the file).
        rustgis_assistant::chatgpt::set_secret_store(Some(secrets.clone()));
        Services {
            load_pref: Some(Box::new(|key: &str| read().get(key).and_then(Value::as_str).map(str::to_string))),
            save_pref: Some(Box::new(|key: &str, value: &str| {
                if let Err(error) = rustgis_engine::prefs::write(key, value) {
                    log::warn!("{error}");
                }
            })),
            secrets: Some(secrets),
            ..Services::default()
        }
    }
}

/// Crash reports. A panic, caught or not, is written to `crash.log` in the settings folder (a
/// release build on Windows has no console to print it to), and the next launch says where it is.
mod crash {
    use std::io::Write;
    use std::path::{Path, PathBuf};

    /// The log never grows past this size.
    const MAX_LOG: u64 = 256 * 1024;

    /// Writes every panic to `<dir>/crash.log`, then reports it as before.
    pub fn install(dir: Option<PathBuf>) {
        let Some(dir) = dir else { return };
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            write(&dir, info);
            previous(info);
        }));
    }

    fn write(dir: &Path, info: &std::panic::PanicHookInfo<'_>) {
        let path = dir.join("crash.log");
        let used = std::fs::metadata(&path).map_or(0, |m| m.len());
        let thread = std::thread::current();
        let entry = format!(
            "--- RustGIS {} on {} {}, {} UTC, thread {}\n{info}\n{}\n\n",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH,
            utc_now(),
            thread.name().unwrap_or("unnamed"),
            std::backtrace::Backtrace::force_capture(),
        );
        // The log starts over when this entry doesn't fit, and one huge entry is cut to the cap.
        let append = used.saturating_add(entry.len() as u64) <= MAX_LOG;
        let mut cut = entry.len().min(usize::try_from(MAX_LOG).unwrap_or(usize::MAX));
        while !entry.is_char_boundary(cut) {
            cut -= 1;
        }
        let file = std::fs::create_dir_all(dir).and_then(|_| {
            let mut options = std::fs::OpenOptions::new();
            options.create(true).write(true).append(append).truncate(!append);
            // Readable by this user only: an error message can quote file paths or data.
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
            options.open(&path)
        });
        if let Ok(mut file) = file {
            let _ = file.write_all(entry.get(..cut).unwrap_or_default().as_bytes());
        }
    }

    fn utc_now() -> String {
        utc(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()))
    }

    /// Seconds since 1970 as `YYYY-MM-DD HH:MM:SS` (UTC).
    pub(crate) fn utc(secs: u64) -> String {
        let (days, rest) = (secs / 86_400, secs % 86_400);
        // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
        let z = i64::try_from(days).unwrap_or(0) + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = yoe + era * 400 + i64::from(month <= 2);
        format!("{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}", rest / 3600, rest % 3600 / 60, rest % 60)
    }

    /// The crash log an earlier session left, moved to `crash-last.log` so it is mentioned once.
    pub fn take_previous(dir: Option<PathBuf>) -> Option<PathBuf> {
        let dir = dir?;
        let log = dir.join("crash.log");
        if !log.is_file() {
            return None;
        }
        let seen = dir.join("crash-last.log");
        std::fs::rename(&log, &seen).ok()?;
        Some(seen)
    }
}

/// Whether the last launch ended without a clean exit, told by a marker file this run holds
/// (locked) and empties when it closes. A crash of any kind (a panic, a native abort, a stack
/// overflow, running out of memory), a kill or a power cut leaves it set; a panic that was caught
/// and recovered from doesn't. A second copy running alongside can't lock it, so it doesn't count.
mod running {
    use std::fs::{File, OpenOptions};
    use std::io::{Read, Seek, Write};
    use std::path::Path;

    /// The marker, held while this run lasts.
    pub struct Marker {
        file: File,
        /// This run has it to itself: no other copy can take it while this one runs.
        locked: bool,
    }

    /// Takes `<dir>/running` for this run: the marker, or why there is none (another copy holds
    /// it, or the folder can't be written), and whether the previous run didn't close cleanly.
    pub fn start(dir: Option<&Path>) -> (Result<Marker, String>, bool) {
        let Some(dir) = dir else { return (Err("this computer has no settings folder for RustGIS".into()), false) };
        let file = std::fs::create_dir_all(dir)
            .and_then(|_| OpenOptions::new().read(true).write(true).create(true).truncate(false).open(dir.join("running")));
        let mut file = match file {
            Ok(f) => f,
            Err(e) => {
                log::warn!("no run marker in {} ({e}): an unclean exit won't be noticed", dir.display());
                return (Err(format!("the settings folder {} can't be written ({e})", dir.display())), false);
            }
        };
        let locked = match file.try_lock() {
            Ok(()) => true,
            Err(std::fs::TryLockError::WouldBlock) => {
                log::info!("the run marker is held: another copy of RustGIS is running");
                return (Err("another copy of RustGIS is running, and only the first keeps a recovery copy".into()), false);
            }
            // The folder can't lock files (some network or FUSE folders): keep the marker unlocked,
            // so unclean exits are still noticed. Without a lock, copies running side by side share
            // it: the second is taken for an unclean exit (it only loses its start-up project), and
            // when it closes first it clears the marker, so a later crash of the first goes
            // unnoticed. Every copy still clears the marker on a clean exit: none holds a lock here,
            // and a marker nobody clears would make every later launch look unclean.
            Err(std::fs::TryLockError::Error(e)) => {
                log::warn!("could not lock the run marker ({e}): a second copy running alongside will look like an unclean exit");
                false
            }
        };
        // A clean exit leaves the marker empty: anything in it (whatever it reads as) means the
        // last run didn't close, and so does a marker that can't be read.
        let mut bytes = Vec::new();
        let unclean = match file.read_to_end(&mut bytes) {
            Ok(_) => !bytes.is_empty(),
            Err(e) => {
                log::warn!("could not read the run marker ({e}): taking the last run as not closed");
                true
            }
        };
        // Written over what is there, then cut to its length: a write that fails leaves the old
        // content, so a marker that was set never reads as a clean exit.
        const RUNNING: &[u8] = b"running";
        let marked =
            file.rewind().and_then(|_| file.write_all(RUNNING)).and_then(|_| file.set_len(RUNNING.len() as u64)).and_then(|_| file.flush());
        if let Err(e) = marked {
            log::warn!("could not mark this run: {e}");
        }
        (Ok(Marker { file, locked }), unclean)
    }

    impl Marker {
        /// Whether no other copy can hold the marker while this run lasts: only then is the
        /// recovery folder this run's alone.
        pub fn locked(&self) -> bool {
            self.locked
        }

        /// A clean exit: the next launch starts as usual. Dropping the file releases the lock.
        pub fn close(self) {
            if let Err(e) = self.file.set_len(0) {
                log::warn!("could not clear the run marker: {e}");
            }
        }
    }

    /// The marker as the app and `main` both hold it: whichever sees the clean exit first
    /// clears it (the app's `on_exit`, or `main` when `run_native` returns, which also covers
    /// eframe failing to start before the app exists).
    pub type Shared = std::sync::Arc<std::sync::Mutex<Option<Marker>>>;

    /// Clears a shared marker once.
    pub fn close(marker: &Shared) {
        let taken = marker.lock().map(|mut m| m.take()).unwrap_or_else(|poisoned| poisoned.into_inner().take());
        if let Some(m) = taken {
            m.close();
        }
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // The GPU map backend draws inside egui's wgpu pass; hand over the render state.
        self.0.set_render_state(frame.wgpu_render_state().cloned());
        #[cfg(target_os = "macos")]
        {
            let titlebar = macos::titlebar();
            // egui repaints on events only: when the window system answers late, draw again.
            if titlebar != self.0.ui.titlebar {
                ctx.request_repaint();
            }
            self.0.ui.titlebar = titlebar;
        }
        self.0.logic(ctx);
        #[cfg(target_os = "macos")]
        self.menus(ctx);
        if self.0.quit_requested {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.0.ui(ui);
    }
    /// macOS: the Edit items arrive as clipboard events and keys (Undo and Redo in a focused text
    /// field), so the app reads them as it reads typed ones; the others wait for `logic`. While
    /// the shortcut editor is recording, the menu takes the keys it shows first: they go to it.
    #[cfg(target_os = "macos")]
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        let Some(bar) = &self.2.bar else { return };
        let capturing = self.0.capturing_shortcut();
        let press = |events: &mut Vec<egui::Event>, k: egui::KeyboardShortcut| {
            for pressed in [true, false] {
                events.push(egui::Event::Key { key: k.logical_key, physical_key: None, pressed, repeat: false, modifiers: k.modifiers });
            }
        };
        for (action, shown) in bar.take_chosen() {
            if capturing {
                if let Some(k) = shown {
                    press(&mut raw_input.events, k);
                }
                continue;
            }
            match action {
                menu::Action::Cut => raw_input.events.push(egui::Event::Cut),
                menu::Action::Copy => raw_input.events.push(egui::Event::Copy),
                menu::Action::Paste => {
                    if let Some(text) = menu::clipboard_text() {
                        raw_input.events.push(egui::Event::Paste(text));
                    }
                }
                menu::Action::SelectAll => {
                    if let Some(k) = shown {
                        press(&mut raw_input.events, k);
                    }
                }
                // Undo and Redo as when typed: a focused text field or ModelBuilder's canvas takes
                // the Mac's key, the keymap the key it gives them; with none, the command runs.
                menu::Action::Keys { command, .. } => {
                    let own = ctx.text_edit_focused() || (self.0.mb.shown() && !ctx.egui_wants_keyboard_input());
                    let typed = shown.filter(|k| self.0.shortcut_command(*k).as_deref() == Some(command));
                    match if own { menu::text_key(action) } else { typed } {
                        Some(k) => press(&mut raw_input.events, k),
                        None => self.2.pending.push(action),
                    }
                }
                _ => self.2.pending.push(action),
            }
        }
    }
    /// Every way eframe closes the app comes here, Cmd+Q on macOS included, which may not
    /// return from `run_native`: the run ended cleanly.
    fn on_exit(&mut self) {
        self.0.end_exports();
        self.0.end_autosave();
        running::close(&self.1);
    }
}

#[cfg(target_os = "macos")]
impl App {
    /// The menu bar: built once the app runs, again for a new language or shortcut; its items
    /// greyed out as their commands are; the commands chosen from it run, unless a dialog (Save
    /// changes?) has the keys, as the keymap's do.
    fn menus(&mut self, ctx: &egui::Context) {
        // The shortcuts shown can only change with the language or the keymap.
        let seen = (self.0.chosen_lang(), self.0.shortcuts_fingerprint());
        if self.2.seen != Some(seen) {
            self.2.seen = Some(seen);
            let now = menu::signature(&self.0);
            if self.2.bar.as_ref().is_none_or(|b| *b.built_for() != now) && self.2.tried.as_ref() != Some(&now) {
                self.2.tried = Some(now);
                match menu::MenuBar::new(&self.0, self.2.chosen.clone()) {
                    Ok(bar) => self.2.bar = Some(bar),
                    Err(e) => log::warn!("menu bar: {e}"),
                }
            }
        }
        // While the shortcut editor records, the items stay on: their keys go to it (raw_input_hook).
        let blocked = ctx.memory(|m| m.top_modal_layer()).is_some();
        if let Some(bar) = &mut self.2.bar {
            bar.follow(&self.0, blocked);
        }
        if blocked {
            self.2.pending.clear();
            return;
        }
        // One a frame: one that opens a dialog (Save changes?) keeps the next from running.
        if self.2.pending.is_empty() {
            return;
        }
        let action = self.2.pending.remove(0);
        if !self.2.pending.is_empty() {
            ctx.request_repaint();
        }
        if action == menu::Action::CloseWindow {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if let Some((id, params)) = menu::command_of(action) {
            match self.0.run_by_user(id, params) {
                // Running out of undo isn't news, as with the keys.
                Err(_) if matches!(action, menu::Action::Keys { .. }) => {}
                Err(e) => self.0.session.notify(e, true),
                Ok(_) => {}
            }
        }
    }
}

/// This run's recovery folder, `recovery` in the app-data folder `data` (see
/// `docs/installation.md`), or why there is none: the app then starts without autosave and says so.
fn recovery_store(data: Option<std::path::PathBuf>) -> Result<Box<dyn rustgis_ui_egui::RecoveryStore>, String> {
    let dir = data.ok_or("this computer has no folder for RustGIS's app data")?.join("recovery");
    let store = rustgis_ui_egui::FolderStore::open(&dir)?;
    Ok(Box::new(store))
}

/// Opens the files named on the command line: a project or a project package as the project, any
/// other file as data. Returns what didn't open, one message per file.
fn open_files(app: &mut RustGisApp, files: &[&String]) -> Vec<String> {
    let mut errors = Vec::new();
    for f in files {
        if let Err(e) = app.run(rustgis_engine::project::open_command(f), json!({"path": f})) {
            errors.push(format!("{f}: {e}"));
        }
    }
    errors
}

/// The value of `--name value` or `--name=value`, the first one given.
fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == name {
            return it.next().map(String::as_str);
        }
        if let Some(v) = a.strip_prefix(name).and_then(|r| r.strip_prefix('=')) {
            return Some(v);
        }
    }
    None
}

/// The graphics backends the window may open with: `--backend` (a comma-separated list of `dx12`,
/// `vulkan`, `gl` and `metal`), else `WGPU_BACKEND`, else every backend but Vulkan on Windows and
/// everything wgpu offers elsewhere. Windows starts on Direct3D 12 because some Vulkan drivers
/// (Intel's for UHD 630 graphics, #470) crash while wgpu looks for an adapter, which closes the app
/// before it can show or log anything; Direct3D 12 is the native API there. A list naming none of
/// those four counts as not given (wgpu also knows `webgpu` and `noop`, which draw no window).
fn backends(flag: Option<&str>, env: Option<&str>, windows: bool) -> wgpu::Backends {
    let desktop = wgpu::Backends::DX12 | wgpu::Backends::VULKAN | wgpu::Backends::GL | wgpu::Backends::METAL;
    let parse = |list: &str| Some(wgpu::Backends::from_comma_list(list) & desktop).filter(|b| !b.is_empty());
    let chosen = flag.and_then(parse).or_else(|| env.and_then(parse));
    chosen.unwrap_or(if windows { wgpu::Backends::DX12 | wgpu::Backends::GL } else { wgpu::Backends::PRIMARY | wgpu::Backends::GL })
}

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("rustgis {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    // What an earlier session left, before this one can write a new log.
    let crashed = crash::take_previous(prefs::dir());
    // Whether it closed cleanly: if not, the Start With project isn't opened (it may be what
    // crashed, and reopening it on every launch would leave no way into Settings).
    let (marker, unclean) = running::start(prefs::dir().as_deref());
    crash::install(prefs::dir());
    let backend = flag(&args, "--backend");
    // Options and the value after `--backend` aren't files.
    let files: Vec<&String> = args
        .iter()
        .enumerate()
        .filter(|(i, a)| !(a.starts_with("--") || *i > 0 && args.get(i - 1).is_some_and(|p| p == "--backend")))
        .map(|(_, a)| a)
        .collect();
    // Installed plugins, before the first session lists commands and tools.
    let plugin_errors = rustgis_engine::cmd::plugins::load_installed();
    let app = if args.iter().any(|a| a == "--sample") || files.is_empty() {
        RustGisApp::with_sample()
    } else {
        RustGisApp::new(Session::new(Project::default()))
    };
    let mut services = prefs::services();
    // Autosave, in a recovery folder only the copy holding the run marker uses: a second copy
    // running alongside would offer, or overwrite, the first one's unsaved work.
    // Without the marker to itself, a run can't tell whether another copy uses the folder: it
    // doesn't autosave, and says why.
    let no_autosave = match &marker {
        Ok(m) if m.locked() => match recovery_store(rustgis_engine::data::dirs::app_data_dir()) {
            Ok(store) => {
                services.recovery = Some(store);
                None
            }
            Err(e) => Some(e),
        },
        Ok(_) => Some("the settings folder can't lock files, so a second copy of RustGIS couldn't be told apart".into()),
        Err(why) => Some(why.clone()),
    };
    // The keys the macOS menu bar keeps, before the stored shortcuts load: the keymap leaves them
    // out and the shortcut editor refuses them.
    #[cfg(target_os = "macos")]
    let app = {
        let mut app = app;
        app.ui.reserved_shortcuts = menu::reserved();
        app
    };
    let mut app = app.with_services(services);
    // Settings › Start-up; `--sample` still opens the sample.
    let blank = if args.iter().any(|a| a == "--sample") { Some(false) } else { (!files.is_empty()).then_some(true) };
    if let Err(e) = app.start(&rustgis_ui_egui::Start { blank, after_crash: unclean, ..Default::default() }) {
        log::warn!("start-up: {e}");
    }
    if let Some(log) = crashed {
        // Notices are shown as written: translate this one now that the language is known.
        rustgis_ui_egui::i18n::set_current(app.chosen_lang());
        let text = rustgis_ui_egui::i18n::fmt(
            "RustGIS ran into an internal error last time. The details are in {path}; please attach them to a bug report.",
            &[("path", &log.display().to_string())],
        );
        app.session.notify(text, true);
    }
    if let Some(e) = no_autosave {
        log::warn!("no autosave: {e}");
        rustgis_ui_egui::i18n::set_current(app.chosen_lang());
        let text = rustgis_ui_egui::i18n::fmt("Autosave is off: {error}", &[("error", &e)]);
        app.session.notify(text, true);
    }
    for e in plugin_errors {
        log::warn!("{e}");
        app.session.notify(e, true);
    }
    for e in open_files(&mut app, &files) {
        eprintln!("rustgis: {e}");
    }
    app.session.mark_saved();
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("RustGIS")
        .with_app_id("org.rustgis.RustGIS")
        .with_inner_size([1480.0, 920.0])
        .with_min_inner_size([900.0, 560.0])
        .with_drag_and_drop(true);
    // macOS: the header is the title bar, with the window buttons floating over its left end.
    // The bundle's rustgis.icns is the Dock icon. A window icon would replace it with the bare
    // logo, and no icon makes eframe fall back to egui's; an empty icon opts out of both (eframe
    // 0.37: its icon setter skips an empty one). Check the Dock again when eframe is updated.
    #[cfg(target_os = "macos")]
    {
        viewport = viewport
            .with_fullsize_content_view(true)
            .with_titlebar_shown(false)
            .with_title_shown(false)
            .with_icon(egui::IconData::default());
    }
    #[cfg(not(target_os = "macos"))]
    if let Some((size, rgba)) = rustgis_engine::render::logo::rgba(256) {
        viewport = viewport.with_icon(egui::IconData { rgba, width: size, height: size });
    }
    let mut wgpu_options = eframe::WgpuConfiguration::default();
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut wgpu_options.wgpu_setup {
        let env = std::env::var("WGPU_BACKEND").ok();
        setup.instance_descriptor.backends = backends(backend, env.as_deref(), cfg!(windows));
    }
    #[allow(unused_mut)]
    let mut options = eframe::NativeOptions { viewport, wgpu_options, ..Default::default() };
    // macOS: the app's own menu bar (menu.rs) in place of winit's minimal one.
    #[cfg(target_os = "macos")]
    {
        options.event_loop_builder = Some(Box::new(|builder| {
            use winit::platform::macos::EventLoopBuilderExtMacOS as _;
            builder.with_default_menu(false);
        }));
    }
    let marker: running::Shared = std::sync::Arc::new(std::sync::Mutex::new(marker.ok()));
    let held = marker.clone();
    let menus = Menus::default();
    let result = eframe::run_native(
        "RustGIS",
        options,
        Box::new(move |_cc| {
            // The menu's choices wake the app: egui draws only on events.
            #[cfg(target_os = "macos")]
            menu::listen(menus.chosen.clone(), _cc.egui_ctx.clone());
            Ok(Box::new(App(app, held, menus)))
        }),
    );
    running::close(&marker);
    result
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::{backends, crash, flag, running};
    use eframe::wgpu::Backends;

    #[test]
    fn windows_starts_without_vulkan_unless_asked() {
        // #470: Intel's Vulkan driver crashed the app before its window opened.
        let default = backends(None, None, true);
        assert!(default.contains(Backends::DX12) && !default.contains(Backends::VULKAN));
        assert!(backends(None, None, false).contains(Backends::VULKAN));
        // The flag first, then WGPU_BACKEND; a list with nothing known in it is ignored.
        assert_eq!(backends(Some("vulkan"), Some("gl"), true), Backends::VULKAN);
        assert_eq!(backends(None, Some("gl"), true), Backends::GL);
        assert_eq!(backends(Some("GL, dx12"), None, true), Backends::GL | Backends::DX12);
        assert_eq!(backends(Some("nonsense"), None, true), default);
        assert_eq!(backends(Some("nonsense"), Some("gl"), true), Backends::GL);
        assert_eq!(backends(Some("webgpu,noop"), None, false), backends(None, None, false));
        assert_eq!(backends(Some("noop,dx12"), None, true), Backends::DX12);
        let args: Vec<String> = ["a.rgis", "--backend", "gl"].map(String::from).into();
        assert_eq!(flag(&args, "--backend"), Some("gl"));
        assert_eq!(flag(&["--backend=dx12".to_string()], "--backend"), Some("dx12"));
        assert_eq!(flag(&["--backend".to_string()], "--backend"), None);
    }

    #[test]
    fn a_run_that_did_not_close_is_noticed_once() {
        let dir = std::env::temp_dir().join(format!("rustgis-running-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // Removed however the test ends (declared first, so dropped after the markers).
        let _cleanup = TempDir(dir.clone());
        let (first, unclean) = running::start(Some(&dir));
        assert!(first.is_ok() && !unclean, "a first run");
        // Held alone: autosave may use the recovery folder.
        assert!(first.as_ref().is_ok_and(running::Marker::locked));
        // Another copy running alongside can't take it, and doesn't count as a crash.
        let (second, unclean) = running::start(Some(&dir));
        // It says why, which the app shows as it starts without autosave.
        assert!(second.as_ref().err().is_some_and(|why| why.contains("another copy")) && !unclean);
        // This run ends without closing (a crash): the next one notices, once it closes cleanly.
        drop(first);
        let (after_crash, unclean) = running::start(Some(&dir));
        assert!(unclean);
        after_crash.unwrap().close();
        let (next, unclean) = running::start(Some(&dir));
        assert!(!unclean);
        drop(next);
    }

    /// A folder removed when the test ends, passed or failed.
    struct TempDir(std::path::PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_project_package_on_the_command_line_opens_as_the_project() {
        let dir = std::env::temp_dir().join(format!("rustgis-args-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let _cleanup = TempDir(dir.clone());
        let pkg = dir.join("lesson.RGISPKG").display().to_string();
        let mut s = rustgis_engine::Session::new(rustgis_engine::sample::world_project());
        s.run("project.package", &serde_json::json!({"path": pkg})).unwrap();
        let maps = s.project.maps.len();
        let mut app = super::RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        let errors = super::open_files(&mut app, &[&pkg]);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(app.session.project.maps.len(), maps);
        assert!(app.session.path.is_none(), "a package opens unsaved, so Save asks where to write a project");
    }

    /// A recovery folder that can't be made (a file in its way) or no app-data folder at all gives
    /// a reason, which the app shows as it starts without autosave; a usable one
    /// gives the store.
    #[test]
    fn a_recovery_folder_that_does_not_open_says_why() {
        let dir = std::env::temp_dir().join(format!("rustgis-recovery-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _cleanup = TempDir(dir.clone());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("recovery"), "a file, not a folder").unwrap();
        let blocked = super::recovery_store(Some(dir.clone())).err().unwrap();
        assert!(blocked.contains("recovery"), "{blocked}");
        assert!(super::recovery_store(None).err().unwrap().contains("app data"));
        assert!(super::recovery_store(Some(dir.join("fresh"))).is_ok());
    }

    #[test]
    fn crash_times_are_utc_dates() {
        assert_eq!(crash::utc(0), "1970-01-01 00:00:00");
        assert_eq!(crash::utc(951_782_400), "2000-02-29 00:00:00");
        assert_eq!(crash::utc(1_700_000_000), "2023-11-14 22:13:20");
    }

    #[test]
    fn a_panic_is_logged_and_reported_once() {
        let dir = std::env::temp_dir().join(format!("rustgis-crash-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        crash::install(Some(dir.clone()));
        let _ = std::thread::spawn(|| panic!("boom for the crash log")).join();
        let log = std::fs::read_to_string(dir.join("crash.log")).unwrap();
        assert!(log.contains("boom for the crash log") && log.contains(env!("CARGO_PKG_VERSION")), "{log}");
        // The next launch finds it once.
        let seen = crash::take_previous(Some(dir.clone())).unwrap();
        assert!(seen.ends_with("crash-last.log") && seen.is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&seen).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert!(crash::take_previous(Some(dir.clone())).is_none());
        let _ = std::panic::take_hook();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
