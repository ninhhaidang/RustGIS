//! The Plugin Manager: installed plugins, the plugin registry, installing from a file.
//!
//! Downloads run off the frame (a thread on the desktop, the browser's fetch on the web) and
//! land in slots that [`poll`] checks each frame. Every install shows what the plugin may do
//! before it is loaded. The desktop keeps installed packages in its plugins folder; the web
//! app remembers registry installs (and small packages from files) in its preferences and
//! loads them again on the next visit.

use std::sync::{Arc, Mutex};

use egui::{Align2, CornerRadius, Stroke, vec2};
use rustgis_engine::plugins::{self as rp, Manifest, Package, registry::Entry};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::theme::{Tokens, semibold};
use crate::{RustGisApp, Slot};

/// The largest package the web app keeps in its preferences (browser storage is small).
const MAX_SAVED: usize = 1536 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Installed,
    Browse,
    Settings,
}

/// A plugin the web app loads again on the next visit.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Saved {
    id: String,
    sha256: String,
    /// Where to download it (registry installs).
    #[serde(default)]
    url: Option<String>,
    /// The package itself, base64 (small installs from files).
    #[serde(default)]
    data: Option<String>,
    #[serde(default = "yes")]
    enabled: bool,
}

fn yes() -> bool {
    true
}

type Download = Slot<Result<Vec<u8>, String>>;

enum Job {
    Registry { url: String, slot: Download },
    Blocklist { registry: String, slot: Download },
    Download { entry: Box<Entry>, slot: Download },
    Restore { saved: Saved, slot: Download, retry: bool }, // `retry`: from the Retry button, not at start
}

/// A package waiting for the user to accept what it may do.
pub struct Confirm {
    manifest: Manifest,
    bytes: Vec<u8>,
    from: Option<Box<Entry>>,
}

/// A plugin the web app saved but couldn't load at start (or on Retry): listed on the
/// Installed tab with why, until it loads or the user removes it.
struct Unloaded {
    saved: Saved,
    reason: String,
}

#[derive(Default)]
pub struct PluginsState {
    pub open: bool,
    pub tab: Tab,
    /// The registry address (empty: the hosted one).
    pub url: String,
    /// The registry as last fetched (set through [`PluginsState::set_registry`]).
    registry: Option<Result<Vec<Entry>, String>>,
    /// Counts registry changes, so the Browse tab's cache knows when to rebuild.
    registry_rev: u64,
    pub filter: String,
    /// The Browse tab's category filter (empty: all).
    pub category: String,
    browse: BrowseCache,
    jobs: Vec<Job>,
    pub confirm: Option<Confirm>,
    /// The outcome of the last action: (ok, text).
    pub message: Option<(bool, String)>,
    /// Python environments being made for plugins (desktop), by plugin id.
    envs: std::collections::HashMap<String, Arc<Mutex<EnvJob>>>,
    /// Saved plugins (web app) that weren't loaded, by id (one entry each).
    unloaded: Vec<Unloaded>,
    /// Ids of plugins that failed to load at start, not yet told (one notice for all, once every
    /// start download is done).
    start_failures: Vec<String>,
}

/// A plugin's Python environment being made on a background thread.
#[derive(Default)]
struct EnvJob {
    /// What it's doing now (uv's last line).
    now: String,
    /// When it's over: the interpreter, or why it failed.
    done: Option<Result<String, String>>,
}

/// Starts making a plugin's Python environment on a background thread (desktop).
#[cfg(not(target_arch = "wasm32"))]
fn prepare_env(app: &mut RustGisApp, id: &str) {
    if app.plugins.envs.get(id).is_some_and(|j| j.lock().is_ok_and(|j| j.done.is_none())) {
        return;
    }
    let job = Arc::new(Mutex::new(EnvJob::default()));
    app.plugins.envs.insert(id.to_string(), job.clone());
    let id = id.to_string();
    std::thread::spawn(move || {
        let set = |f: &mut dyn FnMut(&mut EnvJob)| {
            if let Ok(mut j) = job.lock() {
                f(&mut j);
            }
        };
        let r = rp::pyenv::prepare(&id, &mut |line| set(&mut |j| j.now = line.to_string()));
        set(&mut |j| j.done = Some(r.clone().map(|p| p.display().to_string())));
    });
}

#[cfg(target_arch = "wasm32")]
fn prepare_env(_: &mut RustGisApp, _: &str) {}

/// Whether a plugin uses a Python environment and it's ready (`None`: it doesn't use one).
fn env_ready(id: &str) -> Option<bool> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        rp::pyenv::status(id)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = id;
        None
    }
}

/// A Python plugin's environment, on its card: being made, ready, or not (with Prepare).
fn env_status(app: &mut RustGisApp, ui: &mut egui::Ui, t: &Tokens, id: &str) {
    let Some(ready) = env_ready(id) else { return };
    let job = app.plugins.envs.get(id).and_then(|j| j.lock().ok().map(|j| (j.now.clone(), j.done.clone())));
    match job {
        Some((now, None)) => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(egui::RichText::new(tl!("Preparing its Python environment…")).size(11.5));
            });
            if !now.is_empty() {
                ui.label(egui::RichText::new(now).size(10.5).color(t.text_dim));
            }
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        }
        _ if ready => {
            ui.label(egui::RichText::new(tl!("Python environment ready")).size(11.5).color(t.green));
        }
        done => {
            ui.horizontal(|ui| {
                let text = match done {
                    Some((_, Some(Err(e)))) => crate::i18n::fmt("Python environment failed: {error}", &[("error", &e)]),
                    _ => tl!("Python environment not prepared").to_string(),
                };
                ui.label(egui::RichText::new(text).size(11.5).color(t.red));
                if ui.small_button(tl!("Prepare")).clicked() {
                    prepare_env(app, id);
                }
            });
        }
    }
}

/// What the Browse tab lists, worked out once per registry and filter rather than every frame:
/// a registry can hold thousands of plugins.
#[derive(Default)]
struct BrowseCache {
    /// The registry revision it was made for, and the filter.
    key: Option<(u64, String, String)>,
    /// Each entry's searchable text, lower-cased (by registry index).
    text: Vec<String>,
    /// The categories in the registry.
    categories: Vec<String>,
    /// The entries shown (registry indexes).
    shown: Vec<usize>,
}

impl BrowseCache {
    fn update(&mut self, rev: u64, list: &[Entry], query: &str, category: &str) {
        let key = (rev, query.to_string(), category.to_string());
        if self.key.as_ref() == Some(&key) {
            return;
        }
        if self.key.as_ref().is_none_or(|k| k.0 != rev) {
            self.text = list
                .iter()
                .map(|e| format!("{} {} {} {} {}", e.name, e.id, e.description, e.author, e.categories.join(" ")).to_lowercase())
                .collect();
            let mut cats: Vec<String> = list.iter().flat_map(|e| e.categories.iter().cloned()).collect();
            cats.sort();
            cats.dedup();
            self.categories = cats;
        }
        let words: Vec<&str> = query.split_whitespace().collect();
        self.shown = (0..list.len())
            .filter(|&i| {
                let text = self.text.get(i).map(String::as_str).unwrap_or_default();
                words.iter().all(|w| text.contains(w))
                    && (category.is_empty() || list.get(i).is_some_and(|e| e.categories.iter().any(|c| c == category)))
            })
            .collect();
        self.key = Some(key);
    }
}

impl PluginsState {
    /// Replaces the registry (`None`: fetch it again when Browse is shown).
    pub fn set_registry(&mut self, r: Option<Result<Vec<Entry>, String>>) {
        self.registry = r;
        self.registry_rev = self.registry_rev.wrapping_add(1);
    }

    fn registry_url(&self) -> String {
        let u = self.url.trim();
        if u.is_empty() { rp::registry::DEFAULT_URL.to_string() } else { u.to_string() }
    }
    fn busy(&self) -> bool {
        !self.jobs.is_empty()
    }
}

/// Downloads `url` off the frame.
pub(crate) fn fetch(app: &RustGisApp, url: &str, limit: usize, ctx: Option<egui::Context>) -> Download {
    let slot: Download = Slot::default();
    // The same rule on every platform: HTTPS, or plain HTTP to this computer.
    if !rp::registry::allowed_url(url) {
        if let Ok(mut s) = slot.lock() {
            *s = Some(Err(crate::i18n::fmt("{url}: only HTTPS addresses are allowed", &[("url", url)])));
        }
        return slot;
    }
    if let Some(f) = &app.services.fetch {
        f(url, slot.clone());
        return slot;
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let (url, out) = (url.to_string(), slot.clone());
        std::thread::spawn(move || {
            let r = rp::registry::fetch(&url, limit);
            if let Ok(mut s) = out.lock() {
                *s = Some(r);
            }
            if let Some(ctx) = ctx {
                ctx.request_repaint();
            }
        });
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (limit, ctx);
        if let Ok(mut s) = slot.lock() {
            *s = Some(Err("this build can't download plugins".into()));
        }
    }
    slot
}

pub(crate) fn take(slot: &Download) -> Option<Result<Vec<u8>, String>> {
    slot.lock().ok().and_then(|mut s| s.take())
}

/// Whether the web app keeps installed plugins in its preferences (the desktop keeps them in
/// its plugins folder).
fn keeps_prefs() -> bool {
    cfg!(target_arch = "wasm32")
}

fn saved(app: &RustGisApp) -> Vec<Saved> {
    app.services.load_pref.as_ref().and_then(|l| l("plugins_web")).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save(app: &RustGisApp, list: &[Saved]) {
    if let (Some(s), Ok(text)) = (&app.services.save_pref, serde_json::to_string(list)) {
        s("plugins_web", &text);
    }
}

fn update_saved(app: &RustGisApp, f: impl FnOnce(&mut Vec<Saved>)) {
    if !keeps_prefs() {
        return;
    }
    let mut list = saved(app);
    f(&mut list);
    save(app, &list);
}

/// Loads the plugins the web app installed in earlier visits.
pub fn restore(app: &mut RustGisApp) {
    if !keeps_prefs() {
        return;
    }
    if let Some(u) = app.services.load_pref.as_ref().and_then(|l| l("plugin_registry")) {
        app.plugins.url = u;
    }
    restore_list(app, saved(app));
    // The blocklist, so a pulled plugin isn't loaded again.
    let registry = app.plugins.registry_url();
    let slot = fetch(app, &rp::registry::blocklist_url(&registry), 4 << 20, None);
    app.plugins.jobs.push(Job::Blocklist { registry, slot });
}

/// Starts loading each saved plugin; the ones that can't be loaded are listed on the Installed
/// tab and told once (see [`tell_unloaded`]).
fn restore_list(app: &mut RustGisApp, list: Vec<Saved>) {
    for s in list {
        start_restore(app, s, None, false);
    }
    // Every one may have failed at once, with nothing left to download.
    tell_unloaded(app);
}

/// Loads a saved plugin from the copy kept in the browser, or starts downloading it again.
/// `retry`: started from its Retry button, not at start.
fn start_restore(app: &mut RustGisApp, s: Saved, ctx: Option<egui::Context>, retry: bool) {
    if let Some(data) = &s.data {
        use base64::Engine;
        let got = base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|e| crate::i18n::fmt("its copy kept in the browser is damaged: {error}", &[("error", &e.to_string())]));
        finish_restore(app, s, got, retry);
    } else if let Some(url) = s.url.clone() {
        let slot = fetch(app, &url, rp::package::MAX_PACKAGE, ctx);
        app.plugins.jobs.push(Job::Restore { saved: s, slot, retry });
    } else {
        // Not made by an install: a record edited by hand.
        let reason = tl!("the browser kept neither a copy of it nor where to download it").to_string();
        finish_restore(app, s, Err(reason), retry);
    }
}

/// Loads a saved plugin from what was downloaded. A failure is listed (never forgotten: offline
/// and a missing file look the same on the web), the saved record stays.
fn finish_restore(app: &mut RustGisApp, s: Saved, got: Result<Vec<u8>, String>, retry: bool) {
    let r = got.and_then(|bytes| {
        let sha = rp::package::sha256_hex(&bytes);
        if !sha.eq_ignore_ascii_case(&s.sha256) {
            return Err(tl!("it changed since it was installed: install it again from the Plugin Manager").to_string());
        }
        rustgis_engine::cmd::plugins::install_bytes(&bytes)
    });
    match r {
        Ok(p) => {
            if !s.enabled {
                let _ = rp::set_enabled(&p.manifest.id, false);
            }
            app.plugins.unloaded.retain(|u| u.saved.id != s.id);
            if retry {
                app.plugins.message = Some((true, crate::i18n::fmt("Loaded {id}.", &[("id", &s.id)])));
                app.session.revision = app.session.revision.wrapping_add(1);
            }
        }
        Err(reason) => {
            if retry {
                app.plugins.message = Some((false, crate::i18n::fmt("{id} wasn't loaded: {error}", &[("id", &s.id), ("error", &reason)])));
            } else {
                app.plugins.start_failures.push(s.id.clone());
            }
            let entry = Unloaded { saved: s, reason };
            match app.plugins.unloaded.iter_mut().find(|u| u.saved.id == entry.saved.id) {
                Some(u) => *u = entry,
                None => app.plugins.unloaded.push(entry),
            }
        }
    }
}

/// Tells, in one notice, which plugins failed to load at start, once every start download is
/// done. The Plugin Manager lists them, with Remove (and Retry for those it can download again).
fn tell_unloaded(app: &mut RustGisApp) {
    if app.plugins.start_failures.is_empty() || app.plugins.jobs.iter().any(|j| matches!(j, Job::Restore { retry: false, .. })) {
        return;
    }
    let ids = std::mem::take(&mut app.plugins.start_failures);
    // One each, leaving out any the user has already retried successfully or removed.
    let mut failed: Vec<(String, String)> = Vec::new();
    for id in ids {
        if failed.iter().any(|(f, _)| *f == id) {
            continue;
        }
        if let Some(u) = app.plugins.unloaded.iter().find(|u| u.saved.id == id) {
            failed.push((id, u.reason.clone()));
        }
    }
    let text = match failed.as_slice() {
        [] => return,
        [(id, reason)] => crate::i18n::fmt(
            "The plugin {id} wasn't loaded. Open Plugins › Plugin Manager to see it. {reason}",
            &[("id", id), ("reason", reason)],
        ),
        _ => {
            let ids: Vec<&str> = failed.iter().map(|(id, _)| id.as_str()).collect();
            crate::i18n::fmt(
                "{n} plugins weren't loaded: {ids}. Open Plugins › Plugin Manager to see them.",
                &[("n", &failed.len().to_string()), ("ids", &ids.join(", "))],
            )
        }
    };
    app.session.notify(text, true);
}

/// Checks a package that arrived (a file, a download) and asks the user to accept it.
pub fn received(app: &mut RustGisApp, bytes: Vec<u8>, from: Option<Box<Entry>>) {
    let r = (|| {
        if let Some(e) = &from {
            let sha = rp::package::sha256_hex(&bytes);
            if !sha.eq_ignore_ascii_case(&e.sha256) {
                return Err(crate::i18n::fmt(
                    "{name}: the download doesn't match the registry; it wasn't installed.",
                    &[("name", &e.name)],
                ));
            }
        }
        let pkg = Package::from_zip(&bytes)?;
        // The browser can't start programs.
        if cfg!(target_arch = "wasm32") && pkg.manifest.runtime == rp::Runtime::Process {
            return Err(crate::i18n::fmt(
                "{name} runs as a program on your computer: it needs the desktop app or rustgis-cli.",
                &[("name", &pkg.manifest.name)],
            ));
        }
        if let Some(e) = &from
            && pkg.manifest.id != e.id
        {
            return Err(crate::i18n::fmt("{name}: the download is a different plugin; it wasn't installed.", &[("name", &e.name)]));
        }
        if let Some(b) = rp::blocked(&pkg.manifest.id, &pkg.sha256) {
            return Err(crate::i18n::fmt(
                "{name} was withdrawn from the plugin registry: {reason}",
                &[("name", &pkg.manifest.name), ("reason", &b.reason)],
            ));
        }
        Ok(pkg.manifest)
    })();
    match r {
        Ok(manifest) => {
            app.plugins.open = true;
            app.plugins.confirm = Some(Confirm { manifest, bytes, from });
        }
        Err(e) => {
            app.plugins.message = Some((false, e.clone()));
            app.session.notify(e, true);
        }
    }
}

/// Installs the package the user accepted.
fn install_confirmed(app: &mut RustGisApp) {
    let Some(c) = app.plugins.confirm.take() else { return };
    match rustgis_engine::cmd::plugins::install_bytes(&c.bytes) {
        Ok(p) => {
            app.plugins.unloaded.retain(|u| u.saved.id != p.manifest.id);
            // A start-up download of the same plugin still running would bring the old record back.
            app.plugins.jobs.retain(|j| !matches!(j, Job::Restore { saved, .. } if saved.id == p.manifest.id));
            // That download may have been the last one the start-up notice was waiting for.
            tell_unloaded(app);
            let entry = Saved {
                id: p.manifest.id.clone(),
                sha256: p.sha256.clone(),
                url: c.from.as_ref().map(|e| e.package.clone()),
                data: (c.from.is_none() && c.bytes.len() <= MAX_SAVED).then(|| {
                    use base64::Engine;
                    base64::engine::general_purpose::STANDARD.encode(&c.bytes)
                }),
                enabled: true,
            };
            let kept = entry.url.is_some() || entry.data.is_some();
            update_saved(app, |list| {
                list.retain(|s| s.id != entry.id);
                if kept {
                    list.push(entry);
                }
            });
            let mut text = crate::i18n::fmt(
                "Installed {name} {version}. Find it on the Plugins tab; its tools are also in the Tools pane.",
                &[("name", &p.manifest.name), ("version", &p.manifest.version)],
            );
            if keeps_prefs() && !kept {
                text = format!("{text} {}", tl!("It's too large to keep in the browser: install it again on your next visit."));
            }
            app.plugins.message = Some((true, text));
            app.plugins.tab = Tab::Installed;
            // A Python plugin's environment is made now, in the background.
            if env_ready(&p.manifest.id) == Some(false) {
                prepare_env(app, &p.manifest.id);
            }
            app.session.revision = app.session.revision.wrapping_add(1);
        }
        Err(e) => app.plugins.message = Some((false, e)),
    }
}

fn uninstall(app: &mut RustGisApp, id: &str) {
    match app.run("plugin.uninstall", json!({"id": id})) {
        Ok(_) => {
            update_saved(app, |list| list.retain(|s| s.id != id));
            app.plugins.message = Some((true, crate::i18n::fmt("Uninstalled {id}.", &[("id", id)])));
        }
        Err(e) => app.plugins.message = Some((false, e)),
    }
}

/// Downloads an unloaded plugin again and loads it; the outcome is the message on the Installed
/// tab.
fn retry(app: &mut RustGisApp, id: &str, ctx: &egui::Context) {
    let Some(saved) = app.plugins.unloaded.iter().find(|u| u.saved.id == id).map(|u| u.saved.clone()) else { return };
    if app.plugins.jobs.iter().any(|j| matches!(j, Job::Restore { saved, .. } if saved.id == id)) {
        return;
    }
    app.plugins.message = None;
    start_restore(app, saved, Some(ctx.clone()), true);
}

/// Forgets an unloaded plugin: the app stops trying to load it at start.
fn remove_unloaded(app: &mut RustGisApp, id: &str) {
    app.plugins.unloaded.retain(|u| u.saved.id != id);
    update_saved(app, |list| list.retain(|s| s.id != id));
    app.plugins.message = Some((true, crate::i18n::fmt("Removed {id}.", &[("id", id)])));
}

fn set_enabled(app: &mut RustGisApp, id: &str, on: bool) {
    match app.run("plugin.enable", json!({"id": id, "enabled": on})) {
        Ok(_) => update_saved(app, |list| {
            if let Some(s) = list.iter_mut().find(|s| s.id == id) {
                s.enabled = on;
            }
        }),
        Err(e) => app.plugins.message = Some((false, e)),
    }
}

/// Fetches the registry (and its blocklist).
fn refresh(app: &mut RustGisApp, ctx: &egui::Context) {
    let url = app.plugins.registry_url();
    if app.plugins.jobs.iter().any(|j| matches!(j, Job::Registry { .. })) {
        return;
    }
    let slot = fetch(app, &url, rp::registry::MAX_REGISTRY_BYTES, Some(ctx.clone()));
    app.plugins.jobs.push(Job::Registry { url, slot });
}

fn download(app: &mut RustGisApp, e: &Entry, ctx: &egui::Context) {
    // Not downloaded only to be refused (the Browse row offers no Install for it either).
    if !runs_here(e.runtime, cfg!(target_arch = "wasm32")) {
        let msg =
            crate::i18n::fmt("{name} runs as a program on your computer: it needs the desktop app or rustgis-cli.", &[("name", &e.name)]);
        app.plugins.message = Some((false, msg));
        return;
    }
    let slot = fetch(app, &e.package, rp::package::MAX_PACKAGE, Some(ctx.clone()));
    app.plugins.message = None;
    app.plugins.jobs.push(Job::Download { entry: Box::new(e.clone()), slot });
}

/// Picks a package file to install (or reads `path`, from scripts), then asks before
/// installing it.
pub fn install_from_file(app: &mut RustGisApp, path: Option<&str>) {
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(path) = path {
        match std::fs::read(path) {
            Ok(bytes) => received(app, bytes, None),
            Err(e) => app.plugins.message = Some((false, format!("{path}: {e}"))),
        }
        return;
    }
    #[cfg(target_arch = "wasm32")]
    let _ = path;
    if let Some(open) = &app.services.open_async {
        open("plugin");
        return;
    }
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(path) =
        rfd::FileDialog::new().set_title(tl!("Install a plugin")).add_filter(tl!("RustGIS plugin"), &["rgplugin"]).pick_file()
    {
        match std::fs::read(&path) {
            Ok(bytes) => received(app, bytes, None),
            Err(e) => app.plugins.message = Some((false, format!("{}: {e}", path.display()))),
        }
    }
}

/// Applies finished downloads.
pub fn poll(app: &mut RustGisApp, ctx: &egui::Context) {
    if app.plugins.jobs.is_empty() {
        return;
    }
    let jobs = std::mem::take(&mut app.plugins.jobs);
    let mut waiting = Vec::new();
    for job in jobs {
        match job {
            Job::Registry { url, slot } => match take(&slot) {
                None => waiting.push(Job::Registry { url, slot }),
                Some(r) => {
                    app.plugins.set_registry(Some(r.and_then(|b| rp::registry::parse(&String::from_utf8_lossy(&b), &url))));
                    let slot = fetch(app, &rp::registry::blocklist_url(&url), 4 << 20, Some(ctx.clone()));
                    waiting.push(Job::Blocklist { registry: url, slot });
                }
            },
            Job::Blocklist { registry, slot } => match take(&slot) {
                None => waiting.push(Job::Blocklist { registry, slot }),
                Some(Ok(b)) => {
                    let text = String::from_utf8_lossy(&b).to_string();
                    #[cfg(not(target_arch = "wasm32"))]
                    let pulled = rp::store::save_blocklist(&registry, &text).unwrap_or_default();
                    #[cfg(target_arch = "wasm32")]
                    let pulled = rp::registry::parse_blocklist(&text).map(|l| rp::apply_blocklist(&registry, l)).unwrap_or_default();
                    for name in pulled {
                        app.session.notify(
                            crate::i18n::fmt("{name} was withdrawn from the plugin registry and has been turned off.", &[("name", &name)]),
                            true,
                        );
                    }
                }
                // No blocklist: nothing is blocked.
                Some(Err(_)) => {}
            },
            Job::Download { entry, slot } => match take(&slot) {
                None => waiting.push(Job::Download { entry, slot }),
                Some(Ok(bytes)) => received(app, bytes, Some(entry)),
                Some(Err(e)) => app.plugins.message = Some((false, format!("{}: {e}", entry.name))),
            },
            Job::Restore { saved, slot, retry } => match take(&slot) {
                None => waiting.push(Job::Restore { saved, slot, retry }),
                Some(r) => finish_restore(app, saved, r, retry),
            },
        }
    }
    // Jobs added while applying (the blocklist) come after the ones still waiting.
    waiting.append(&mut app.plugins.jobs);
    app.plugins.jobs = waiting;
    tell_unloaded(app);
    if !app.plugins.jobs.is_empty() {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}

fn chip(ui: &mut egui::Ui, t: &Tokens, text: &str, color: egui::Color32) -> egui::Response {
    egui::Frame::NONE
        .fill(color.gamma_multiply(0.14))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(egui::Margin::symmetric(5, 1))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).size(10.5).color(if t.dark { color.gamma_multiply(1.3) } else { color }));
        })
        .response
}

/// Whether a plugin that runs as `runtime` can be installed here: the browser can't start
/// programs, so a process plugin is for the desktop app and the command line.
fn runs_here(runtime: rp::Runtime, web: bool) -> bool {
    !(web && runtime == rp::Runtime::Process)
}

fn permissions(ui: &mut egui::Ui, t: &Tokens, perms: &[rp::Permission], native: bool) {
    ui.horizontal_wrapped(|ui| {
        // A process plugin can do anything: said first, whatever its permissions.
        if native {
            chip(ui, t, tl!("Not sandboxed"), t.red);
        }
        permission_chips(ui, t, perms)
    });
}

/// The permission chips, in the current layout (one line in a Browse row).
fn permission_chips(ui: &mut egui::Ui, t: &Tokens, perms: &[rp::Permission]) {
    chip(ui, t, tl!("Reads the map"), t.accent);
    for p in perms {
        let label = match p {
            rp::Permission::Edit => tl!("Edits the project"),
            rp::Permission::Files => tl!("Reads and writes files"),
            rp::Permission::Network => tl!("Loads map tiles from the web"),
        };
        chip(ui, t, label, if *p == rp::Permission::Edit { t.green } else { t.red });
    }
}

fn card(ui: &mut egui::Ui, t: &Tokens, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::NONE.stroke(Stroke::new(1.0, t.border)).corner_radius(CornerRadius::same(6)).inner_margin(egui::Margin::same(10)).show(
        ui,
        |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        },
    );
    ui.add_space(6.0);
}

fn installed_tab(app: &mut RustGisApp, ui: &mut egui::Ui, t: &Tokens) {
    let plugins = rp::list();
    if plugins.is_empty() && app.plugins.unloaded.is_empty() {
        ui.add_space(12.0);
        ui.label(tl!("No plugins are installed. Browse the plugin registry, or install a .rgplugin file."));
        ui.add_space(8.0);
        if crate::widgets::primary_button(ui, tl!("Browse Plugins"), true, vec2(0.0, 28.0)).clicked() {
            app.plugins.tab = Tab::Browse;
        }
        return;
    }
    // The plugins that weren't loaded come first: (id, why, whether Retry is running, whether
    // Retry can download it: a copy kept in the browser would only fail the same way again).
    let unloaded: Vec<(String, String, bool, bool)> = app
        .plugins
        .unloaded
        .iter()
        .map(|u| {
            let id = &u.saved.id;
            let trying = app.plugins.jobs.iter().any(|j| matches!(j, Job::Restore { saved, .. } if saved.id == *id));
            let downloads = u.saved.data.is_none() && u.saved.url.is_some();
            (id.clone(), u.reason.clone(), trying, downloads)
        })
        .collect();
    // What was clicked: (id, true for Retry, false for Remove).
    let mut act: Option<(String, bool)> = None;
    for (id, reason, trying, downloads) in &unloaded {
        card(ui, t, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(id).font(semibold(13.5)));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if *trying {
                        ui.spinner();
                        ui.label(egui::RichText::new(tl!("Trying again…")).size(11.5));
                    } else {
                        if ui
                            .small_button(tl!("Remove"))
                            .on_hover_text(tl!("Forget this plugin: the app stops trying to load it at start."))
                            .clicked()
                        {
                            act = Some((id.clone(), false));
                        }
                        if *downloads && ui.small_button(tl!("Retry")).on_hover_text(tl!("Download it again and load it.")).clicked() {
                            act = Some((id.clone(), true));
                        }
                    }
                });
            });
            chip(ui, t, tl!("Not loaded"), t.red);
            ui.label(egui::RichText::new(reason).size(11.5).color(t.text_dim));
        });
    }
    match act {
        Some((id, true)) => retry(app, &id, ui.ctx()),
        Some((id, false)) => remove_unloaded(app, &id),
        None => {}
    }
    let lang = app.lang().code();
    let newer: Vec<(String, String)> = match &app.plugins.registry {
        Some(Ok(list)) => list.iter().map(|e| (e.id.clone(), e.version.clone())).collect(),
        _ => Vec::new(),
    };
    for p in plugins {
        let m = &p.manifest;
        card(ui, t, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(m.translate(lang, &m.name)).font(semibold(13.5)));
                ui.label(egui::RichText::new(&m.version).size(11.5).color(t.text_dim));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button(tl!("Uninstall")).clicked() {
                        uninstall(app, &m.id);
                    }
                    let mut on = p.enabled;
                    if ui.checkbox(&mut on, tl!("Enabled")).changed() {
                        set_enabled(app, &m.id, on);
                    }
                    if let Some((_, v)) = newer.iter().find(|(id, v)| *id == m.id && rp::manifest::version_cmp(v, &m.version).is_gt())
                        && ui.button(crate::i18n::fmt("Update to {version}", &[("version", v)])).clicked()
                        && let Some(Ok(list)) = &app.plugins.registry
                        && let Some(e) = list.iter().find(|e| e.id == m.id).cloned()
                    {
                        download(app, &e, ui.ctx());
                    }
                });
            });
            if !m.author.is_empty() {
                ui.label(egui::RichText::new(crate::i18n::fmt("by {author}", &[("author", &m.author)])).size(11.5).color(t.text_dim));
            }
            if !m.description.is_empty() {
                ui.label(egui::RichText::new(m.translate(lang, &m.description)).size(12.0));
            }
            ui.add_space(4.0);
            permissions(ui, t, &m.permissions, m.runtime == rp::Runtime::Process);
            env_status(app, ui, t, &m.id);
            let adds = crate::i18n::fmt(
                "Commands on the Plugins tab: {commands} · Tools in the Tools pane: {tools}",
                &[("commands", &m.commands.len().to_string()), ("tools", &m.tools.len().to_string())],
            );
            ui.label(egui::RichText::new(adds).size(11.0).color(t.text_dim));
            if !m.skills.is_empty() {
                let names: Vec<String> = m.skills.iter().map(|s| format!("{}:{s}", m.id)).collect();
                let skills = crate::i18n::fmt("Skills for the AI Assistant: {names}", &[("names", &names.join(", "))]);
                ui.label(egui::RichText::new(skills).size(11.0).color(t.text_dim));
            }
            if !m.homepage.is_empty() {
                ui.add(
                    egui::Hyperlink::from_label_and_url(egui::RichText::new(tl!("Homepage")).size(11.0), &m.homepage).open_in_new_tab(true),
                );
            }
        });
    }
}

/// The height of one plugin in the Browse list (rows are a fixed height so only the visible
/// ones are laid out).
const ROW_H: f32 = 104.0;

fn browse_tab(app: &mut RustGisApp, ui: &mut egui::Ui, t: &Tokens) {
    if app.plugins.registry.is_none() && !app.plugins.busy() {
        refresh(app, ui.ctx());
    }
    ui.horizontal(|ui| {
        let PluginsState { filter, category, browse, .. } = &mut app.plugins;
        ui.add(egui::TextEdit::singleline(filter).hint_text(tl!("Search plugins")).desired_width(220.0));
        let all = tl!("All categories");
        let shown = if category.is_empty() { all.to_string() } else { category.clone() };
        egui::ComboBox::from_id_salt("plugin-category")
            .selected_text(shown)
            .width(150.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(category, String::new(), all);
                for c in &browse.categories {
                    ui.selectable_value(category, c.clone(), c.as_str());
                }
            })
            .response
            .on_hover_text(tl!("Category"));
        if ui.button(tl!("Refresh")).clicked() {
            app.plugins.set_registry(None);
            refresh(app, ui.ctx());
        }
        if app.plugins.jobs.iter().any(|j| matches!(j, Job::Registry { .. } | Job::Download { .. })) {
            ui.spinner();
        }
    });
    let rev = app.plugins.registry_rev;
    let list = match &app.plugins.registry {
        None => return,
        Some(Err(e)) => {
            ui.add_space(6.0);
            ui.label(egui::RichText::new(crate::i18n::fmt("Couldn't load the plugin registry: {error}", &[("error", e)])).color(t.red));
            return;
        }
        Some(Ok(list)) => list,
    };
    let query = app.plugins.filter.trim().to_lowercase();
    app.plugins.browse.update(rev, list, &query, &app.plugins.category);
    let total = list.len();
    let n = app.plugins.browse.shown.len();
    let count = if n == total {
        crate::i18n::fmt("{total} plugins", &[("total", &total.to_string())])
    } else {
        crate::i18n::fmt("{shown} of {total} plugins", &[("shown", &n.to_string()), ("total", &total.to_string())])
    };
    ui.label(egui::RichText::new(count).size(11.0).color(t.text_dim));
    ui.add_space(4.0);
    if n == 0 {
        ui.label(tl!("No plugins match."));
        return;
    }
    let mut install: Option<Entry> = None;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, ROW_H, n, |ui, rows| {
        let Some(Ok(list)) = &app.plugins.registry else { return };
        for row in rows {
            let Some(e) = app.plugins.browse.shown.get(row).and_then(|&i| list.get(i)) else { continue };
            if browse_row(ui, t, e, cfg!(target_arch = "wasm32")) {
                install = Some(e.clone());
            }
        }
    });
    if let Some(e) = install {
        download(app, &e, ui.ctx());
    }
}

/// One plugin in the Browse list, in a fixed-height row; returns whether Install was clicked.
/// `web`: in the web app, which can't install process plugins.
fn browse_row(ui: &mut egui::Ui, t: &Tokens, e: &Entry, web: bool) -> bool {
    let lang = crate::i18n::current().code();
    let installed = rp::get(&e.id).map(|p| p.manifest.version.clone());
    let blocked = rp::blocked(&e.id, &e.sha256);
    let mut clicked = false;
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, ROW_H), egui::Sense::hover());
    let mut child =
        ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(10.0, 6.0))).layout(egui::Layout::top_down(egui::Align::Min)));
    let ui = &mut child;
    ui.painter().line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0, t.border));
    let reason = blocked.as_ref().map(|b| b.reason.clone()).filter(|r| !r.trim().is_empty());
    ui.horizontal(|ui| {
        // The status first (right-aligned), then the name in what's left, truncated.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let newer = installed.as_deref().is_some_and(|v| rp::manifest::version_cmp(&e.version, v).is_gt());
            if blocked.is_some() {
                let w = ui.label(egui::RichText::new(tl!("Withdrawn")).color(t.red));
                if let Some(r) = &reason {
                    w.on_hover_text(r);
                }
            } else if installed.is_some() && !newer {
                ui.label(egui::RichText::new(tl!("Installed")).color(t.green));
            } else if !runs_here(e.runtime, web) {
                // Listed rather than hidden, so the web app's list is the registry's, and says
                // where the plugin can be installed.
                ui.label(egui::RichText::new(tl!("Desktop and command line only")).color(t.text_dim)).on_hover_text(crate::i18n::fmt(
                    "{name} runs as a program on your computer: it needs the desktop app or rustgis-cli.",
                    &[("name", e.translate(lang, &e.name))],
                ));
            } else {
                let label = if newer { tl!("Update") } else { tl!("Install") };
                clicked = crate::widgets::primary_button(ui, label, true, vec2(84.0, 24.0)).clicked();
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.add(egui::Label::new(egui::RichText::new(e.translate(lang, &e.name)).font(semibold(13.5))).truncate());
                ui.label(egui::RichText::new(&e.version).size(11.5).color(t.text_dim));
            });
        });
    });
    let mut by = Vec::new();
    if !e.author.is_empty() {
        by.push(crate::i18n::fmt("by {author}", &[("author", &e.author)]));
    }
    if !e.categories.is_empty() {
        by.push(e.categories.join(", "));
    }
    if e.size >= 1_048_576 {
        by.push(format!("{:.1} MB", e.size as f64 / 1_048_576.0));
    } else if e.size > 0 {
        by.push(format!("{} KB", e.size.div_ceil(1024)));
    }
    ui.add(egui::Label::new(egui::RichText::new(by.join(" · ")).size(11.0).color(t.text_dim)).truncate());
    // One line of the description; all of it (and why it was withdrawn) on hover. The reason is
    // also on the Withdrawn label, for a plugin without a description.
    let description = e.translate(lang, &e.description);
    let mut tip = description.to_string();
    if let Some(r) = &reason {
        tip = format!("{tip}\n\n{r}");
    }
    // The explicit tooltip below includes the withdrawal reason; don't also show egui's elision tooltip.
    let desc = ui.add(egui::Label::new(egui::RichText::new(description).size(12.0)).truncate().show_tooltip_when_elided(false));
    if !tip.trim().is_empty() {
        desc.on_hover_text(tip);
    }
    ui.horizontal(|ui| {
        // A process plugin can do anything: said first, as for installed ones, with what
        // installing it downloads.
        if e.runtime == rp::Runtime::Process {
            chip(ui, t, tl!("Not sandboxed"), t.red).on_hover_text(tl!(
                "This plugin runs as a program on your computer, outside RustGIS's sandbox. It can do anything you can: read and change your files, use the network and run other programs. Install it only if you trust its author."
            ));
            if let Some(v) = &e.python {
                chip(ui, t, &format!("Python {v}"), t.text_dim).on_hover_text(crate::i18n::fmt(
                    "Installing it downloads Python {version} and the packages it needs from the internet.",
                    &[("version", v)],
                ));
            }
        }
        permission_chips(ui, t, &e.permissions);
        if !e.homepage.is_empty() {
            ui.add(egui::Hyperlink::from_label_and_url(egui::RichText::new(tl!("Homepage")).size(11.0), &e.homepage).open_in_new_tab(true));
        }
    });
    clicked
}

fn settings_tab(app: &mut RustGisApp, ui: &mut egui::Ui, t: &Tokens) {
    card(ui, t, |ui| {
        ui.label(egui::RichText::new(tl!("Install from a file")).font(semibold(13.0)));
        ui.label(
            egui::RichText::new(tl!("Plugins are .rgplugin files. Install only plugins from people you trust."))
                .size(11.5)
                .color(t.text_dim),
        );
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if crate::widgets::primary_button(ui, tl!("Install From File…"), true, vec2(0.0, 28.0)).clicked() {
                install_from_file(app, None);
            }
            #[cfg(not(target_arch = "wasm32"))]
            if ui
                .button(tl!("Add Plugin Folder…"))
                .on_hover_text(tl!("Load an unpacked plugin from its folder while you develop it."))
                .clicked()
                && let Some(dir) = rfd::FileDialog::new().set_title(tl!("Plugin folder")).pick_folder()
            {
                let r = app.run("plugin.install", json!({"path": dir.display().to_string()}));
                app.plugins.message = Some(match r {
                    Ok(v) => {
                        (true, crate::i18n::fmt("Loaded {name} from its folder.", &[("name", v["name"].as_str().unwrap_or_default())]))
                    }
                    Err(e) => (false, e),
                });
            }
        });
    });
    card(ui, t, |ui| {
        ui.label(egui::RichText::new(tl!("Plugin registry")).font(semibold(13.0)));
        ui.label(
            egui::RichText::new(tl!("The list of plugins to browse. Leave it empty for the RustGIS plugin registry."))
                .size(11.5)
                .color(t.text_dim),
        );
        ui.add_space(4.0);
        let resp =
            ui.add(egui::TextEdit::singleline(&mut app.plugins.url).hint_text(rp::registry::DEFAULT_URL).desired_width(f32::INFINITY));
        if resp.lost_focus() {
            if let Some(s) = &app.services.save_pref {
                s("plugin_registry", app.plugins.url.trim());
            }
            app.plugins.set_registry(None);
        }
    });
    let folder = app.session.run("plugin.list", &Value::Null).ok().and_then(|v| v["folder"].as_str().map(str::to_string));
    card(ui, t, |ui| {
        ui.label(egui::RichText::new(tl!("About plugins")).font(semibold(13.0)));
        ui.label(
            egui::RichText::new(tl!(
                "Plugins run in a sandbox: they can't reach your files or the network, and they change the project only through RustGIS's own commands, within the permissions you accept."
            ))
            .size(11.5),
        );
        ui.label(
            egui::RichText::new(tl!("Plugins marked Not sandboxed run as programs on your computer instead, and can do anything you can."))
                .size(11.5),
        );
        if let Some(f) = folder {
            ui.label(
                egui::RichText::new(crate::i18n::fmt("Installed plugins are kept in {folder}", &[("folder", &f)]))
                    .size(11.0)
                    .color(t.text_dim),
            );
        }
        ui.add(
            egui::Hyperlink::from_label_and_url(
                egui::RichText::new(tl!("Write a plugin")).size(11.5),
                "https://plugins.rustgis.app/develop/",
            )
            .open_in_new_tab(true),
        );
    });
}

fn confirm(app: &mut RustGisApp, ctx: &egui::Context, t: &Tokens) {
    let Some(c) = &app.plugins.confirm else { return };
    let m = c.manifest.clone();
    let mut decision = None;
    egui::Modal::new(egui::Id::unique("plugin_confirm")).show(ctx, |ui| {
        ui.set_width(420.0);
        ui.label(
            egui::RichText::new(crate::i18n::fmt("Install {name} {version}?", &[("name", &m.name), ("version", &m.version)]))
                .font(semibold(15.0)),
        );
        if !m.author.is_empty() {
            ui.label(egui::RichText::new(crate::i18n::fmt("by {author}", &[("author", &m.author)])).size(11.5).color(t.text_dim));
        }
        if !m.description.is_empty() {
            ui.add_space(4.0);
            ui.label(&m.description);
        }
        ui.add_space(8.0);
        let native = m.runtime == rp::Runtime::Process;
        if native {
            // Not sandboxed: said first and plainly, with what it will run.
            egui::Frame::NONE
                .fill(t.red.gamma_multiply(0.12))
                .stroke(Stroke::new(1.0, t.red))
                .corner_radius(CornerRadius::same(6))
                .inner_margin(egui::Margin::same(8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(egui::RichText::new(tl!("Not sandboxed")).font(semibold(12.5)).color(t.red));
                    ui.label(tl!(
                        "This plugin runs as a program on your computer, outside RustGIS's sandbox. It can do anything you can: read and change your files, use the network and run other programs. Install it only if you trust its author."
                    ));
                    if let Some(py) = &m.python {
                        ui.label(crate::i18n::fmt(
                            "Installing it downloads Python {version} and the packages it needs from the internet.",
                            &[("version", &py.version)],
                        ));
                    }
                    if let Some(argv) = m.run.as_ref().and_then(|r| r.for_this_platform()) {
                        ui.label(egui::RichText::new(crate::i18n::fmt("It runs: {program}", &[("program", &argv.join(" "))])).monospace().size(11.5));
                    }
                });
            ui.add_space(6.0);
        }
        let skills_only = m.runtime == rp::Runtime::Skills;
        if !m.skills.is_empty() {
            let names: Vec<String> = m.skills.iter().map(|s| format!("{}:{s}", m.id)).collect();
            ui.label(crate::i18n::fmt("Skills for the AI Assistant: {names}", &[("names", &names.join(", "))]));
            ui.label(
                egui::RichText::new(tl!(
                    "The assistant follows a skill's instructions when it uses it. Whatever they say, its commands that need your approval still ask."
                ))
                .size(11.5)
                .color(t.text_dim),
            );
            ui.add_space(4.0);
        }
        if skills_only {
            ui.label(egui::RichText::new(tl!("It has no code: nothing in it runs.")).size(11.5).color(t.text_dim));
        } else {
            ui.label(egui::RichText::new(if native { tl!("In RustGIS, it can:") } else { tl!("This plugin can:") }).font(semibold(12.5)));
            ui.label(format!("• {}", tl!("Read the map's layers and features")));
            for p in &m.permissions {
                ui.label(format!("• {}", tl!(p.describe())));
            }
        }
        if !native && !skills_only {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(tl!("It runs in a sandbox with no other access to your computer or the network."))
                    .size(11.5)
                    .color(t.text_dim),
            );
        }
        ui.add_space(10.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
            if crate::widgets::primary_button(ui, tl!("Install"), true, vec2(88.0, 28.0)).clicked() {
                decision = Some(true);
            }
            if ui.button(tl!("Cancel")).clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                decision = Some(false);
            }
        });
    });
    match decision {
        Some(true) => install_confirmed(app),
        Some(false) => app.plugins.confirm = None,
        None => {}
    }
}

/// The Plugin Manager window, and the install prompt.
pub fn show(app: &mut RustGisApp, ctx: &egui::Context) {
    let t = Tokens::get(ctx);
    confirm(app, ctx, &t);
    if !app.plugins.open {
        return;
    }
    let mut open = true;
    let screen = ctx.content_rect();
    egui::Window::new(tl!("Plugin Manager"))
        .id(egui::Id::unique("plugin_manager"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size(vec2(560.0_f32.min(screen.width() - 32.0), 520.0_f32.min(screen.height() - 80.0)))
        .pivot(Align2::CENTER_CENTER)
        .default_pos(screen.center())
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                for (tab, label) in [(Tab::Installed, tl!("Installed")), (Tab::Browse, tl!("Browse")), (Tab::Settings, tl!("Settings"))] {
                    if ui.selectable_label(app.plugins.tab == tab, label).clicked() {
                        app.plugins.tab = tab;
                    }
                }
            });
            ui.separator();
            if let Some((ok, text)) = app.plugins.message.clone() {
                ui.label(egui::RichText::new(text).size(12.0).color(if ok { t.green } else { t.red }));
                ui.add_space(4.0);
            }
            match app.plugins.tab {
                // Browse scrolls its own rows (only the visible ones are laid out).
                Tab::Browse => browse_tab(app, ui, &t),
                Tab::Installed => {
                    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| installed_tab(app, ui, &t));
                }
                Tab::Settings => {
                    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| settings_tab(app, ui, &t));
                }
            }
        });
    if !open {
        app.plugins.open = false;
        app.plugins.message = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_web_app_offers_no_process_plugins() {
        for r in [rp::Runtime::Wasm, rp::Runtime::Process, rp::Runtime::Skills] {
            assert!(runs_here(r, false), "{r:?} on the desktop");
        }
        assert!(runs_here(rp::Runtime::Wasm, true) && runs_here(rp::Runtime::Skills, true));
        assert!(!runs_here(rp::Runtime::Process, true));
    }

    /// A saved registry install.
    fn record(id: &str, sha256: &str) -> Saved {
        Saved {
            id: id.into(),
            sha256: sha256.into(),
            url: Some(format!("https://example.invalid/{id}.rgplugin")),
            data: None,
            enabled: true,
        }
    }

    /// An app whose downloads all end the way `answer` says, at once.
    fn app_fetching(answer: impl Fn(&str) -> Result<Vec<u8>, String> + 'static) -> RustGisApp {
        let mut app = RustGisApp::with_sample();
        app.services.fetch = Some(Box::new(move |url: &str, slot: Slot<Result<Vec<u8>, String>>| {
            if let Ok(mut s) = slot.lock() {
                *s = Some(answer(url));
            }
        }));
        app
    }

    /// What `restore_list` and `poll` told for `list`: the notices given, as (text, error).
    fn start(app: &mut RustGisApp, list: Vec<Saved>) -> Vec<(String, bool)> {
        let mark = app.session.notice_mark();
        restore_list(app, list);
        poll(app, &egui::Context::default());
        app.session.notices_since(mark).iter().map(|n| (n.text.clone(), n.error)).collect()
    }

    /// Plugins the web app couldn't load at start are listed, kept, and told once (#1352).
    #[test]
    fn plugins_that_fail_at_start_are_listed_and_told_once() {
        // Two that can't be downloaded: one notice naming both, a count, and where to go.
        let mut app = app_fetching(|_| Err("unreachable".into()));
        let told = start(&mut app, vec![record("demo", "0"), record("gone", "0")]);
        assert_eq!(told.len(), 1, "{told:?}");
        assert!(told[0].1, "an error: {told:?}");
        assert!(told[0].0.starts_with("2 plugins weren't loaded: demo, gone."), "{told:?}");
        assert!(told[0].0.contains("Plugin Manager"), "{told:?}");
        let listed: Vec<(&str, &str)> = app.plugins.unloaded.iter().map(|u| (u.saved.id.as_str(), u.reason.as_str())).collect();
        assert_eq!(listed, [("demo", "unreachable"), ("gone", "unreachable")]);
        assert!(app.plugins.start_failures.is_empty() && app.plugins.jobs.is_empty());

        // One: its id first, then why.
        let mut app = app_fetching(|_| Err("unreachable".into()));
        let told = start(&mut app, vec![record("demo", "0")]);
        assert_eq!(told.len(), 1, "{told:?}");
        assert!(told[0].0.starts_with("The plugin demo wasn't loaded."), "{told:?}");
        assert!(told[0].0.ends_with("unreachable"), "{told:?}");

        // A download that isn't the one installed.
        let mut app = app_fetching(|_| Ok(b"x".to_vec()));
        start(&mut app, vec![record("demo", "0")]);
        let why = app.plugins.unloaded.first().map(|u| u.reason.clone()).unwrap_or_default();
        assert!(why.contains("changed since it was installed"), "{why}");

        // A copy kept in the browser that can't be read, with nothing to download: told at once.
        let mut app = app_fetching(|_| Err("unreachable".into()));
        let damaged = Saved { data: Some("!not base64!".into()), url: None, ..record("damaged", "0") };
        let told = start(&mut app, vec![damaged]);
        assert_eq!(told.len(), 1, "{told:?}");
        assert!(told[0].0.starts_with("The plugin damaged wasn't loaded.") && told[0].0.contains("damaged:"), "{told:?}");

        // Loaded: nothing to tell, nothing listed.
        struct Cleanup(#[allow(dead_code)] tempfile::TempDir);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = rustgis_plugins::store::uninstall("restore-ok");
                let _ = rustgis_plugins::store::uninstall("restore-new");
                rustgis_plugins::store::set_dir(None);
            }
        }
        let dir = tempfile::tempdir().unwrap();
        rustgis_plugins::store::set_dir(Some(dir.path().to_path_buf()));
        let _cleanup = Cleanup(dir);
        let package = rustgis_plugins::testing::skills_package("restore-ok", &["s"]);
        let sha = rp::package::sha256_hex(&package);
        let mut app = app_fetching(move |_| Ok(package.clone()));
        let told = start(&mut app, vec![record("restore-ok", &sha)]);
        assert!(told.is_empty(), "{told:?}");
        assert!(app.plugins.unloaded.is_empty() && app.plugins.start_failures.is_empty());
        assert!(rp::get("restore-ok").is_some());

        // Installed again while its start-up download still runs: that download is dropped, so it
        // can't list the plugin as not loaded afterwards, and the start-up notice it held back
        // (here for a damaged copy) is given.
        let package = rustgis_plugins::testing::skills_package("restore-new", &["s"]);
        let mut app = RustGisApp::with_sample();
        app.services.fetch = Some(Box::new(|_: &str, _: Slot<Result<Vec<u8>, String>>| {}));
        let mark = app.session.notice_mark();
        let damaged = Saved { data: Some("!not base64!".into()), url: None, ..record("damaged", "0") };
        restore_list(&mut app, vec![damaged, record("restore-new", "0")]);
        assert_eq!(app.plugins.jobs.len(), 1);
        assert!(app.session.notices_since(mark).is_empty());
        let manifest = Package::from_zip(&package).unwrap().manifest;
        app.plugins.confirm = Some(Confirm { manifest, bytes: package, from: None });
        install_confirmed(&mut app);
        assert!(app.plugins.jobs.is_empty());
        let listed: Vec<&str> = app.plugins.unloaded.iter().map(|u| u.saved.id.as_str()).collect();
        assert_eq!(listed, ["damaged"]);
        let told: Vec<&str> = app.session.notices_since(mark).iter().map(|n| n.text.as_str()).collect();
        assert!(told.len() == 1 && told[0].starts_with("The plugin damaged wasn't loaded."), "{told:?}");
        assert!(rp::get("restore-new").is_some());
    }

    /// A plugin that wasn't loaded is listed on the Installed tab, with Retry and Remove (#1352).
    #[test]
    fn unloaded_plugins_offer_retry_and_remove() {
        use egui_kittest::kittest::Queryable;
        let mut app = app_fetching(|_| Err("still unreachable".into()));
        app.plugins.unloaded.push(Unloaded { saved: record("demo", "0"), reason: "unreachable".into() });
        let mut fonts = false;
        // A widget shown for the first time asks for one more pass to take its style.
        let mut h = egui_kittest::Harness::builder().with_size(vec2(560.0, 600.0)).with_max_steps(8).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                installed_tab(app, ui, &Tokens::light());
            },
            app,
        );
        h.run();
        for label in ["demo", "Not loaded", "Retry", "Remove", "unreachable"] {
            assert!(h.query_by_label(label).is_some(), "{label}");
        }
        assert!(h.query_by_label_contains("No plugins are installed").is_none());

        // Retry downloads again; while it runs the buttons give way to a spinner.
        let mark = h.state().session.notice_mark();
        h.get_by_label("Retry").click();
        h.step();
        h.step();
        assert!(h.query_by_label("Trying again…").is_some());
        assert!(h.query_by_label("Retry").is_none() && h.query_by_label("Remove").is_none());
        poll(h.state_mut(), &egui::Context::default());
        h.run();
        let app = h.state();
        assert_eq!(app.plugins.unloaded.first().map(|u| u.reason.as_str()), Some("still unreachable"));
        assert_eq!(app.plugins.message, Some((false, "demo wasn't loaded: still unreachable".to_string())));
        // Said on the tab, not as a notice.
        assert!(app.session.notices_since(mark).is_empty());
        assert!(h.query_by_label("Retry").is_some() && h.query_by_label("still unreachable").is_some());

        // Remove forgets it.
        h.get_by_label("Remove").click();
        h.run();
        let app = h.state();
        assert!(app.plugins.unloaded.is_empty());
        assert_eq!(app.plugins.message, Some((true, "Removed demo.".to_string())));
        assert!(h.query_by_label("Not loaded").is_none());

        // A copy kept in the browser can't be downloaded again: Remove only.
        h.state_mut()
            .plugins
            .unloaded
            .push(Unloaded { saved: Saved { url: None, data: Some("AA==".into()), ..record("copy", "0") }, reason: "damaged".into() });
        h.run();
        assert!(h.query_by_label("Remove").is_some());
        assert!(h.query_by_label("Retry").is_none());
    }
}
