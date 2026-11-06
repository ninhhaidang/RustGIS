//! Help › Check for Updates: asks GitHub for the latest release and compares it with this build,
//! showing a newer one's version and release notes. On Windows, a copy installed with
//! `setup.exe` can download that release's installer, check it and install it when RustGIS
//! closes (#1290, see [`install`]). Nothing is sent until the user asks.

use egui::Align2;
use serde_json::Value;

use crate::{RustGisApp, Slot};

#[cfg(not(target_arch = "wasm32"))]
mod install;

/// The latest release, as JSON naming its tag and page.
const LATEST: &str = "https://api.github.com/repos/opengeos/RustGIS/releases/latest";
/// Every release, for when the check can't reach GitHub.
const RELEASES: &str = "https://github.com/opengeos/RustGIS/releases";
/// The release JSON is a few kilobytes (some tens with long notes); anything far larger isn't it.
const MAX_BYTES: usize = 1024 * 1024;
/// The release notes shown, at most: the rest is on the release page.
const MAX_NOTES: usize = 20_000;

/// The Check for Updates window.
#[derive(Default)]
pub struct UpdateState {
    pub open: bool,
    /// Start a check on the next frame (commands have no egui context to repaint with).
    start: bool,
    pending: Option<Slot<Result<Vec<u8>, String>>>,
    result: Option<Result<Release, String>>,
    /// Download and Install: what it is doing.
    #[cfg(not(target_arch = "wasm32"))]
    install: Install,
    /// When to ask again what the last Install and Restart did: the installer may still have been
    /// at work as this run started.
    #[cfg(not(target_arch = "wasm32"))]
    recheck: Option<std::time::Instant>,
}

/// Download and Install's progress, from the click to the installer waiting for RustGIS to close.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Default)]
enum Install {
    #[default]
    Idle,
    /// Downloading and checking `version`'s installer.
    Downloading { version: String, progress: install::Progress, slot: Slot<Result<install::Saved, String>> },
    /// Checked and saved: it runs when RustGIS closes, once the user has said so (`at_exit`).
    Ready { version: String, saved: install::Saved, at_exit: bool },
    /// `version`'s download failed, or Install and Restart's (`installing`): shown with that
    /// release only. Either way Try Again downloads it again.
    Failed { version: String, error: String, installing: bool },
}

/// The installer the desktop app runs as it closes: the program, its switches, and the
/// `/D=<folder>` NSIS takes last and unquoted (pass it raw).
#[cfg(not(target_arch = "wasm32"))]
pub struct PendingInstall {
    pub installer: std::path::PathBuf,
    pub args: Vec<&'static str>,
    pub folder: std::ffi::OsString,
}

impl UpdateState {
    /// Opens the window and checks again.
    pub fn check(&mut self) {
        self.open = true;
        self.start = true;
        self.result = None;
    }

    /// The installer to run as RustGIS closes: set once the user chose Install and Restart. The
    /// desktop app runs it from its exit (`apps/rustgis`). Checked again here: the save question
    /// can keep RustGIS open long after the click. A file changed since doesn't run, and the next
    /// start says the update wasn't installed (its version is noted already).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn installer_at_exit(&self) -> Option<PendingInstall> {
        let Install::Ready { version, saved, at_exit: true } = &self.install else { return None };
        if let Err(e) = install::still_checked(&saved.path, &saved.sha256) {
            log::warn!("The update isn't installed: {e}");
            return None;
        }
        // Checked again: RustGIS's own uninstaller may have run since the window first asked.
        let dir = install::setup_install().filter(|d| install::setup_install_in(d))?;
        // Noted again as the installer starts: the note's age tells a RustGIS started meanwhile
        // that the installer may still be at work, however long the save question took.
        if let Err(e) = install::note_pending(version) {
            log::warn!("could not note the update being installed: {e}");
        }
        let (args, folder) = install::installer_args(&dir);
        Some(PendingInstall { installer: saved.path.clone(), args, folder })
    }

    /// At start, what the last Install and Restart did, as a notice and whether it is an error:
    /// RustGIS was updated, or the update didn't arrive (asked once; nothing could say so as
    /// RustGIS closed). `None` when nothing was installed, and while the installer may still be at
    /// work: then the window's frame asks again once it can't be (see [`show`]).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn last_install_notice(&mut self) -> Option<(String, bool)> {
        let current = env!("CARGO_PKG_VERSION");
        self.recheck = install::install_wait_left(current).map(|left| std::time::Instant::now() + left);
        if self.recheck.is_some() {
            return None;
        }
        last_install_notice(current)
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn last_install_notice(current: &str) -> Option<(String, bool)> {
    Some(match install::last_install(current)? {
        Ok(v) => (crate::i18n::fmt("RustGIS was updated to {version}.", &[("version", &v)]), false),
        Err(v) => (
            crate::i18n::fmt(
                "RustGIS {version} wasn't installed: try again from Help › Check for Updates, or install it from the release page.",
                &[("version", &v)],
            ),
            true,
        ),
    })
}

/// Whether `a` and `b` name the same version (`v0.6.0` and `0.6.0`).
#[cfg(not(target_arch = "wasm32"))]
fn version_eq(a: &str, b: &str) -> bool {
    version(a).is_some() && !is_newer(a, b) && !is_newer(b, a)
}

/// A file published with a release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
}

/// A published release: its version (the tag without its `v`), its page, its notes and files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub url: String,
    /// The release notes as plain text (see [`plain_notes`]), at most [`MAX_NOTES`] characters
    /// of them.
    pub notes: String,
    /// Whether the notes were cut: the window says the rest is on the release page.
    pub notes_cut: bool,
    /// Its files with an address RustGIS downloads from: HTTPS, or HTTP to this computer.
    pub assets: Vec<Asset>,
}

/// Reads GitHub's latest-release JSON.
pub fn parse_release(body: &[u8]) -> Result<Release, String> {
    let v: Value = serde_json::from_slice(body).map_err(|_| "GitHub's reply isn't a release".to_string())?;
    let tag =
        v.get("tag_name").and_then(Value::as_str).map(str::trim).filter(|t| !t.is_empty()).ok_or("GitHub's reply names no release")?;
    // A tag that isn't a version can't be compared: saying "up to date" would be a guess.
    if version(tag).is_none() {
        return Err("GitHub's latest release has no version number".into());
    }
    let url = v.get("html_url").and_then(Value::as_str).filter(|u| u.starts_with("https://")).unwrap_or(RELEASES);
    let body = v.get("body").and_then(Value::as_str).unwrap_or_default().trim();
    // Made plain once here, not each frame the window draws.
    let (notes, notes_cut) = match body.char_indices().nth(MAX_NOTES) {
        Some((cut, _)) => (format!("{}…", plain_notes(body.get(..cut).unwrap_or_default())), true),
        None => (plain_notes(body), false),
    };
    let assets = v
        .get("assets")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|a| {
            let name = a.get("name").and_then(Value::as_str)?;
            // HTTPS, or HTTP to this computer (a release served there to try an update).
            let url =
                a.get("browser_download_url").and_then(Value::as_str).filter(|u| rustgis_engine::plugins::registry::allowed_url(u))?;
            Some(Asset { name: name.to_string(), url: url.to_string() })
        })
        .collect();
    Ok(Release { version: tag.trim_start_matches(['v', 'V']).to_string(), url: url.to_string(), notes, notes_cut, assets })
}

/// Release notes as plain text, for a label: headings without their `#`, list items with a
/// bullet, links and images as their text, and no bold or code marks (`__` stays: `__init__`).
fn plain_notes(markdown: &str) -> String {
    let mut out = Vec::new();
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        let indent = line.get(..line.len() - trimmed.len()).unwrap_or_default();
        // A heading is `#`s then a space (or nothing): `#1290 fixed` stays as written.
        let heading = trimmed.trim_start_matches('#');
        let line = if heading.len() < trimmed.len() && (heading.is_empty() || heading.starts_with(' ')) {
            heading.trim_start().to_string()
        } else if let Some(item) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")) {
            format!("{indent}• {item}")
        } else {
            line.to_string()
        };
        // `[text](address)` reads as its text.
        let mut text = String::with_capacity(line.len());
        let mut rest = line.as_str();
        while let Some(open) = rest.find('[') {
            let (before, after) = rest.split_at(open);
            text.push_str(before);
            // The `](` right after this bracket's own `]`: `[a] and [b](c)` is no link at `[a]`.
            let link = after
                .find(']')
                .filter(|&mid| after.get(mid + 1..).is_some_and(|s| s.starts_with('(')))
                .and_then(|mid| link_end(after.get(mid + 1..)?).map(|close| (mid, mid + 1 + close)));
            match link {
                Some((mid, close)) => {
                    // An image, `![alt](src)`, reads as its alt text too.
                    if text.ends_with('!') {
                        text.pop();
                    }
                    text.push_str(after.get(1..mid).unwrap_or_default());
                    rest = after.get(close + 1..).unwrap_or_default();
                }
                None => {
                    text.push('[');
                    rest = after.get(1..).unwrap_or_default();
                }
            }
        }
        text.push_str(rest);
        out.push(text.replace("**", "").replace('`', ""));
    }
    out.join("\n")
}

/// Where the `(address)` that `s` starts with ends: at its matching `)`, so an address with
/// parentheses of its own (`…/Map_(GIS)`) stays whole. `None` when it doesn't close.
fn link_end(s: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth = depth.saturating_add(1),
            ')' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Where the check asks for the latest release: GitHub's API, or, to try an update against a
/// release served on this computer, the release JSON `RUSTGIS_UPDATE_RELEASE` names. A testing
/// aid, read by development builds only (released ones don't have it), and only for a local
/// address (`http://127.0.0.1:…`, `http://localhost:…`): it can't send the check, and the
/// installer it names, anywhere else.
fn latest_url() -> String {
    #[cfg(not(target_arch = "wasm32"))]
    if let Ok(url) = std::env::var("RUSTGIS_UPDATE_RELEASE") {
        let url = url.trim();
        if !cfg!(debug_assertions) {
            if !url.is_empty() {
                log::warn!("RUSTGIS_UPDATE_RELEASE is ignored: only development builds read it");
            }
        } else if local_release(url) {
            return url.to_string();
        } else if !url.is_empty() {
            log::warn!("RUSTGIS_UPDATE_RELEASE={url} is ignored: only a release served on this computer (http://127.0.0.1) is");
        }
    }
    LATEST.to_string()
}

/// Whether `url` is plain HTTP to this computer, as `RUSTGIS_UPDATE_RELEASE` must be.
#[cfg(not(target_arch = "wasm32"))]
fn local_release(url: &str) -> bool {
    !url.starts_with("https://") && rustgis_engine::plugins::registry::allowed_url(url)
}

/// A pre-release identifier: numbers sort numerically and before words (`beta.2` < `beta.10`,
/// `1` < `alpha`).
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Ident {
    Num(u64),
    Word(String),
}

/// A version's dotted numbers (one to three; missing ones are 0) and its pre-release
/// identifiers, without build metadata; `None` when it isn't a version.
fn version(v: &str) -> Option<([u64; 3], Vec<Ident>)> {
    let v = v.trim().trim_start_matches(['v', 'V']);
    let v = v.split('+').next().unwrap_or(v);
    let (core, pre) = v.split_once('-').map_or((v, None), |(c, p)| (c, Some(p)));
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() > 3 {
        return None;
    }
    let mut n = [0u64; 3];
    for (slot, part) in n.iter_mut().zip(&parts) {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *slot = part.parse().ok()?;
    }
    let pre = match pre {
        None => Vec::new(),
        Some("") => return None,
        Some(p) => p.split('.').map(|i| i.parse().map_or_else(|_| Ident::Word(i.to_string()), Ident::Num)).collect(),
    };
    Some((n, pre))
}

/// Whether version `latest` is newer than `current`, in semantic-version order: the numbers
/// first, then a pre-release (`0.2.0-beta.1`) before its release, and pre-releases by their
/// identifiers. False when either isn't a version.
pub fn is_newer(latest: &str, current: &str) -> bool {
    let (Some((a, a_pre)), Some((b, b_pre))) = (version(latest), version(current)) else { return false };
    if a != b {
        return a > b;
    }
    match (a_pre.is_empty(), b_pre.is_empty()) {
        (true, true) | (false, true) => false,
        (true, false) => true,
        (false, false) => a_pre > b_pre,
    }
}

/// What a click in the window asks for, done once it has drawn.
#[cfg(not(target_arch = "wasm32"))]
enum Action {
    /// Download and check the installer of `version` (Download and Install, or Try Again).
    Download {
        version: String,
        installer: Asset,
        sums: Asset,
    },
    Cancel,
    /// Install and Restart: close RustGIS, which runs the installer.
    InstallNow,
}

/// Megabytes, for a download's progress.
#[cfg(not(target_arch = "wasm32"))]
fn mb(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / (1024.0 * 1024.0))
}

/// Draws the window, starting and collecting the check.
pub fn show(app: &mut RustGisApp, ctx: &egui::Context) {
    // The installer was still at work as this run started: once it can't be, say what it did.
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(at) = app.update.recheck {
        match at.checked_duration_since(std::time::Instant::now()) {
            Some(left) if !left.is_zero() => ctx.request_repaint_after(left),
            _ => {
                if let Some((text, error)) = app.update.last_install_notice() {
                    app.session.notify(text, error);
                }
            }
        }
    }
    if !app.update.open {
        return;
    }
    // A check asked for while one is running waits for that one rather than sending another.
    if std::mem::take(&mut app.update.start) && app.update.pending.is_none() && !cfg!(target_arch = "wasm32") {
        app.update.pending = Some(crate::pluginsui::fetch(app, &latest_url(), MAX_BYTES, Some(ctx.clone())));
    }
    if let Some(r) = app.update.pending.as_ref().and_then(crate::pluginsui::take) {
        app.update.pending = None;
        app.update.result = Some(r.and_then(|body| parse_release(&body)));
    }
    #[cfg(not(target_arch = "wasm32"))]
    if let Install::Downloading { version, slot, .. } = &app.update.install
        && let Some(r) = slot.lock().ok().and_then(|mut s| s.take())
    {
        app.update.install = match r {
            Ok(saved) => Install::Ready { version: version.clone(), saved, at_exit: false },
            Err(error) => Install::Failed { version: version.clone(), error, installing: false },
        };
    }
    let current = env!("CARGO_PKG_VERSION");
    let mut open = true;
    let mut again = false;
    let mut visit: Option<String> = None;
    #[cfg(not(target_arch = "wasm32"))]
    let mut action: Option<Action> = None;
    egui::Window::new(tl!("Check for Updates"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .pivot(Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .show(ctx, |ui| {
            ui.set_width(420.0);
            if cfg!(target_arch = "wasm32") {
                ui.label(tl!("The web app always runs the latest release: reload the page to get it."));
                return;
            }
            match &app.update.result {
                None => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(tl!("Checking for updates…"));
                    });
                }
                Some(Ok(r)) if is_newer(&r.version, current) => {
                    ui.label(egui::RichText::new(crate::i18n::fmt("RustGIS {latest} is available.", &[("latest", &r.version)])).strong());
                    ui.label(crate::i18n::fmt("You have version {version}.", &[("version", current)]));
                    // What the release brings, before anything is installed.
                    if !r.notes.is_empty() {
                        ui.add_space(6.0);
                        ui.label(egui::RichText::new(tl!("What's new")).strong());
                        egui::Frame::group(ui.style()).show(ui, |ui| {
                            egui::ScrollArea::vertical().max_height(200.0).auto_shrink([false, true]).show(ui, |ui| {
                                ui.label(egui::RichText::new(&r.notes).small());
                            });
                        });
                        if r.notes_cut {
                            ui.label(egui::RichText::new(tl!("The notes are cut here: the release page has all of them.")).small().weak());
                        }
                    }
                    ui.add_space(6.0);
                    #[cfg(not(target_arch = "wasm32"))]
                    {
                        let setup = install::setup_install().is_some();
                        let assets = if setup { install::assets_for(r, std::env::consts::ARCH) } else { None };
                        let stage = match &app.update.install {
                            // A download of another release (an older check), or its failure,
                            // doesn't count, but an install already set for when RustGIS closes
                            // stays until it runs: a new download would drop it without a word.
                            Install::Downloading { version, .. }
                            | Install::Ready { version, at_exit: false, .. }
                            | Install::Failed { version, .. }
                                if *version != r.version =>
                            {
                                &Install::Idle
                            }
                            s => s,
                        };
                        match (stage, assets) {
                            (Install::Downloading { progress, .. }, _) => {
                                use std::sync::atomic::Ordering;
                                let (done, total) = (progress.done.load(Ordering::Relaxed), progress.total.load(Ordering::Relaxed));
                                let text = if total > 0 {
                                    crate::i18n::fmt("Downloading: {done} of {total} MB", &[("done", &mb(done)), ("total", &mb(total))])
                                } else {
                                    crate::i18n::fmt("Downloading: {done} MB", &[("done", &mb(done))])
                                };
                                let bar = if total > 0 {
                                    egui::ProgressBar::new(done as f32 / total as f32)
                                } else {
                                    egui::ProgressBar::new(0.0).animate(true)
                                };
                                ui.add(bar.text(text));
                                ui.add_space(6.0);
                                if ui.button(tl!("Cancel")).clicked() {
                                    action = Some(Action::Cancel);
                                }
                            }
                            (Install::Ready { version, at_exit, .. }, _) => {
                                ui.label(tl!(
                                    "The installer is downloaded and checked. RustGIS closes, installs the update and opens again."
                                ));
                                if *at_exit {
                                    let text = crate::i18n::fmt("RustGIS {version} installs when RustGIS closes.", &[("version", version)]);
                                    ui.label(egui::RichText::new(text).small().weak());
                                }
                                ui.add_space(6.0);
                                ui.horizontal(|ui| {
                                    if ui.button(tl!("Install and Restart")).clicked() {
                                        action = Some(Action::InstallNow);
                                    }
                                    if ui.button(tl!("Open the Release Page")).clicked() {
                                        visit = Some(r.url.clone());
                                    }
                                    if ui.button(tl!("Check Again")).clicked() {
                                        again = true;
                                    }
                                });
                            }
                            (stage, Some((installer, sums))) => {
                                if let Install::Failed { error, installing, .. } = stage {
                                    let error = [("error", tl_raw!(error.as_str()))];
                                    let text = if *installing {
                                        crate::i18n::fmt("Couldn't install the update: {error}", &error)
                                    } else {
                                        crate::i18n::fmt("Couldn't download the update: {error}", &error)
                                    };
                                    ui.label(egui::RichText::new(text).color(ui.visuals().error_fg_color));
                                    ui.add_space(4.0);
                                }
                                ui.horizontal(|ui| {
                                    let label = if matches!(stage, Install::Failed { .. }) {
                                        tl!("Try Again")
                                    } else {
                                        tl!("Download and Install")
                                    };
                                    if ui.button(label).clicked() {
                                        action = Some(Action::Download {
                                            version: r.version.clone(),
                                            installer: installer.clone(),
                                            sums: sums.clone(),
                                        });
                                    }
                                    if ui.button(tl!("Open the Release Page")).clicked() {
                                        visit = Some(r.url.clone());
                                    }
                                    if ui.button(tl!("Check Again")).clicked() {
                                        again = true;
                                    }
                                });
                            }
                            (_, None) => {
                                // An install this window can update, from a release without its
                                // installer or checksums: the release page has what there is.
                                let note = if setup {
                                    tl!("This release has no installer for this computer: get it from the release page.")
                                } else {
                                    tl!("Installed with Homebrew, winget, the AUR or COPR? Update it there.")
                                };
                                ui.label(egui::RichText::new(note).small().weak());
                                ui.add_space(6.0);
                                ui.horizontal(|ui| {
                                    if ui.button(tl!("Open the Release Page")).clicked() {
                                        visit = Some(r.url.clone());
                                    }
                                    if ui.button(tl!("Check Again")).clicked() {
                                        again = true;
                                    }
                                });
                            }
                        }
                    }
                }
                Some(Ok(_)) => {
                    ui.label(crate::i18n::fmt("RustGIS {version} is the latest version.", &[("version", current)]));
                    ui.add_space(6.0);
                    if ui.button(tl!("Check Again")).clicked() {
                        again = true;
                    }
                }
                Some(Err(e)) => {
                    ui.label(crate::i18n::fmt("Couldn't check for updates: {error}", &[("error", tl_raw!(e.as_str()))]));
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button(tl!("Open the Release Page")).clicked() {
                            visit = Some(RELEASES.to_string());
                        }
                        if ui.button(tl!("Check Again")).clicked() {
                            again = true;
                        }
                    });
                }
            }
        });
    #[cfg(not(target_arch = "wasm32"))]
    match action {
        Some(Action::Download { version, installer, sums }) => {
            let progress = install::Progress::default();
            let slot = install::start(installer, sums, progress.clone(), ctx.clone());
            // Another release's download, replaced: stopped, so it can't clear this one's file.
            if let Install::Downloading { progress: old, .. } = &app.update.install {
                old.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            app.update.install = Install::Downloading { version, progress, slot };
        }
        Some(Action::Cancel) => {
            if let Install::Downloading { progress, .. } = &app.update.install {
                progress.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            app.update.install = Install::Idle;
        }
        // The usual way out: a project with unsaved changes asks first (Cancel keeps RustGIS
        // open, and the update waits for it to close).
        // Checked again first, and the version noted for the next start: once RustGIS has closed
        // nothing can say that the update didn't happen. The hash runs here, on the window's
        // thread, so a failure shows before RustGIS closes; for an installer of a few tens of
        // MB it is a short pause, once, at the click.
        Some(Action::InstallNow) => {
            if let Install::Ready { version, saved, at_exit } = &mut app.update.install {
                match install::still_checked(&saved.path, &saved.sha256).and_then(|()| install::note_pending(version)) {
                    Ok(()) => {
                        *at_exit = true;
                        app.quit_requested = true;
                    }
                    Err(error) => app.update.install = Install::Failed { version: version.clone(), error, installing: true },
                }
            }
        }
        None => {}
    }
    if again {
        app.update.check();
    }
    if visit.is_some() {
        app.ui.open_url = visit;
    }
    if !open {
        app.update.open = false;
        app.update.pending = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number_with_pre_releases_first() {
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("v0.1.1", "0.1.0"));
        assert!(is_newer("1.0", "0.9.9"));
        assert!(is_newer("0.10.0", "0.9.0"));
        assert!(is_newer("0.2.0", "0.2.0-beta.1"));
        assert!(!is_newer("0.2.0-beta.1", "0.2.0"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
        assert!(is_newer("0.2.0-beta.2", "0.2.0-beta.1"));
        assert!(is_newer("0.2.0-beta.10", "0.2.0-beta.2"));
        assert!(is_newer("0.2.0-rc.1", "0.2.0-beta.3"));
        assert!(is_newer("0.2.0-beta", "0.2.0-1"));
        assert!(!is_newer("0.2.0-beta.1", "0.2.0-beta.1"));
        assert!(!is_newer("0.1.0+build.5", "0.1.0"));
        assert!(!is_newer("junk", "0.1.0"));
        assert!(!is_newer("release-2.0.0", "0.1.0"));
        assert!(!is_newer("1.2.3.4", "0.1.0"));
        assert!(!is_newer("", ""));
    }

    #[test]
    fn release_json_is_read_and_checked() {
        let r = parse_release(br#"{"tag_name":"v0.2.0","html_url":"https://github.com/opengeos/RustGIS/releases/tag/v0.2.0"}"#).unwrap();
        assert_eq!(r.version, "0.2.0");
        assert!(r.url.ends_with("/v0.2.0"));
        // A page that isn't HTTPS falls back to the releases list.
        assert_eq!(parse_release(br#"{"tag_name":"0.2.0","html_url":"javascript:alert(1)"}"#).unwrap().url, RELEASES);
        assert!(parse_release(b"<html>rate limited</html>").is_err());
        assert!(parse_release(br#"{"message":"Not Found"}"#).is_err());
        assert!(parse_release(br#"{"tag_name":"  "}"#).is_err());
        // A tag that isn't a version is an error, not "up to date".
        assert!(parse_release(br#"{"tag_name":"release-2.0.0"}"#).is_err());
        assert_eq!(parse_release(br#"{"tag_name":"v1.0.0-rc.1"}"#).unwrap().version, "1.0.0-rc.1");
    }

    #[test]
    fn release_notes_and_files_are_read() {
        let r = parse_release(
            br#"{"tag_name":"v0.6.0","body":"  ## Highlights\n- Faster maps  ","assets":[
                {"name":"SHA256SUMS","browser_download_url":"https://github.com/opengeos/RustGIS/releases/download/v0.6.0/SHA256SUMS"},
                {"name":"plain.exe","browser_download_url":"http://example.com/plain.exe"},
                {"name":"local.exe","browser_download_url":"http://127.0.0.1:8000/local.exe"},
                {"name":"nameless"},
                {"browser_download_url":"https://example.com/x"}
            ]}"#,
        )
        .unwrap();
        // Plain text, made once.
        assert_eq!(r.notes, "Highlights\n• Faster maps");
        assert!(!r.notes_cut);
        // Only files with a name and an address RustGIS downloads from: HTTPS, or HTTP to this
        // computer (a release served there to try an update), not plain HTTP elsewhere.
        assert_eq!(r.assets.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), ["SHA256SUMS", "local.exe"]);
        // No notes or files: empty, not an error.
        let r = parse_release(br#"{"tag_name":"v0.6.0","body":null,"assets":"none"}"#).unwrap();
        assert!(r.notes.is_empty() && r.assets.is_empty());
        // Long notes are cut, on a character.
        let long = format!(r#"{{"tag_name":"v0.6.0","body":"{}"}}"#, "é".repeat(MAX_NOTES + 10));
        let r = parse_release(long.as_bytes()).unwrap();
        assert_eq!(r.notes.chars().count(), MAX_NOTES + 1);
        assert!(r.notes.ends_with('…') && r.notes_cut);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn only_a_release_on_this_computer_replaces_github_for_testing() {
        assert!(local_release("http://127.0.0.1:8765/release.json"));
        assert!(local_release("http://localhost:8765/release.json"));
        // Elsewhere, HTTPS included: the check goes to GitHub as usual.
        assert!(!local_release("https://example.com/release.json"));
        assert!(!local_release("http://example.com/release.json"));
        assert!(!local_release("http://localhost.example.com/release.json"));
        assert!(!local_release(""));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn versions_are_the_same_with_or_without_their_v() {
        assert!(version_eq("v0.6.0", "0.6.0") && version_eq("0.6", "0.6.0"));
        assert!(!version_eq("0.6.0", "0.5.0") && !version_eq("0.6.0-rc.1", "0.6.0") && !version_eq("junk", "junk"));
    }

    #[test]
    fn release_notes_show_as_plain_text() {
        let md = "## Highlights\n- **Layouts**: see [the guide](https://rustgis.app/user-guide/layouts/).\n  * nested `code`\n\
                  Plain [brackets] stay, and [a link](https://x) too.\n# Fixes";
        assert_eq!(
            plain_notes(md),
            "Highlights\n• Layouts: see the guide.\n  • nested code\nPlain [brackets] stay, and a link too.\nFixes"
        );
        // `#` with no space after it isn't a heading: an issue number keeps it.
        assert_eq!(plain_notes("#1290 fixed\n### Notes"), "#1290 fixed\nNotes");
        assert_eq!(plain_notes("[unclosed](https://x"), "[unclosed](https://x");
        assert_eq!(plain_notes("See [Map (GIS)](https://example.org/Map_(GIS)) now."), "See Map (GIS) now.");
        assert_eq!(plain_notes(""), "");
        // An image reads as its alt text; `__` stays (a name such as `__init__`).
        assert_eq!(plain_notes("See ![the new window](https://x/shot.webp) and __init__."), "See the new window and __init__.");
    }
}
