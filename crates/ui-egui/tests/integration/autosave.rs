//! Autosave and crash recovery (#873), through the app's frames and its recovery dialog: a copy of
//! unsaved work is kept in the recovery folder, removed once the work is saved or the app closes
//! cleanly, and offered back by the next start when a run ended without closing.
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use egui::accesskit::Role;
use egui_kittest::kittest::Queryable;
use rustgis_engine::project::{Task, TaskStep};
use rustgis_ui_egui::{FolderStore, RecoveryStore, RustGisApp, Services};
use serde_json::json;

/// This run's copy and the one an earlier run left, as the folder store names them.
const LIVE: &str = "autosave.recovery";
/// The name the first copy set aside gets.
const PENDING: &str = "recovered-1.recovery";

/// A folder removed when the test ends, passed or failed.
struct TempDir(PathBuf);

impl TempDir {
    /// A fresh, empty folder for test `name`, apart from other tests and runs.
    fn new(name: &str) -> TempDir {
        let dir = std::env::temp_dir().join(format!("rustgis-autosave-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
}

impl Drop for TempDir {
    /// Removes the folder and what the test left in it.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Preferences shared by every run of one test, as `prefs.json` is between launches.
type Prefs = Arc<Mutex<std::collections::HashMap<String, String>>>;

/// One launch of the desktop app: the sample, with the recovery folder `dir` (holding the run
/// marker) and the preferences `prefs`.
fn launch(dir: &Path, prefs: &Prefs) -> egui_kittest::Harness<'static, RustGisApp> {
    let (load, save) = (prefs.clone(), prefs.clone());
    let services = Services {
        load_pref: Some(Box::new(move |k| load.lock().unwrap().get(k).cloned())),
        save_pref: Some(Box::new(move |k, v| {
            save.lock().unwrap().insert(k.to_string(), v.to_string());
        })),
        recovery: Some(Box::new(FolderStore::open(&dir.join("recovery")).unwrap())),
        ..Default::default()
    };
    let mut app = RustGisApp::with_sample().with_services(services);
    app.start(&Default::default()).unwrap();
    egui_kittest::Harness::builder().with_size(egui::vec2(1400.0, 860.0)).with_max_steps(64).build_ui_state(
        |ui, app: &mut RustGisApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        app,
    )
}

/// Runs `n` frames. A click is handled on the frame after it and the dialog it closes was
/// drawn by then, so its result shows a frame later; a dialog that grew is centred again a frame
/// after that.
fn frames(h: &mut egui_kittest::Harness<'_, RustGisApp>, n: usize) {
    for _ in 0..n {
        h.step();
    }
}

/// Runs a few frames, then waits until the store has done what they asked of it.
fn settle(h: &mut egui_kittest::Harness<'_, RustGisApp>) {
    for _ in 0..3 {
        h.step();
    }
    h.state().services.recovery.as_ref().unwrap().flush();
}

/// A file in the recovery folder of a launch in `dir`.
fn file(dir: &Path, name: &str) -> PathBuf {
    dir.join("recovery").join(name)
}

/// Quits as the user does, with unsaved work: the window's close is asked for, the save prompt
/// answered Don't Save, and the app ends cleanly (`on_exit`).
fn quit_without_saving(h: &mut egui_kittest::Harness<'_, RustGisApp>) {
    h.input_mut().viewports.entry(egui::ViewportId::ROOT).or_default().events.push(egui::ViewportEvent::Close);
    frames(h, 3);
    h.get_by_role_and_label(Role::Button, "Don't Save").click();
    frames(h, 3);
    assert!(h.state().quit_requested);
    h.state_mut().end_autosave();
}

/// An edit to the project: a new layer.
fn edit(h: &mut egui_kittest::Harness<'_, RustGisApp>, name: &str) {
    h.state_mut().run("layer.template", json!({"template": "points", "name": name})).unwrap();
}

#[test]
fn unsaved_work_is_kept_and_offered_back_after_a_crash() {
    let dir = TempDir::new("crash");
    let prefs = Prefs::default();
    let project = dir.0.join("Parcels.rgis");
    let mut h = launch(&dir.0, &prefs);
    // Nothing unsaved, nothing kept; nor after a change of view only.
    settle(&mut h);
    h.state_mut().run("map.zoomFull", json!({})).unwrap();
    settle(&mut h);
    assert!(!file(&dir.0, LIVE).exists());
    h.state_mut().run("project.saveAs", json!({"path": project.display().to_string()})).unwrap();
    edit(&mut h, "Inspections");
    settle(&mut h);
    let copy = std::fs::read_to_string(file(&dir.0, LIVE)).unwrap();
    let (about, text) = copy.split_once('\n').unwrap();
    let about: serde_json::Value = serde_json::from_str(about).unwrap();
    assert_eq!(about["name"], "Parcels");
    assert_eq!(about["path"], json!(std::path::absolute(&project).unwrap().display().to_string()));
    assert!(text.contains("Inspections"));
    // The app stops without closing: no clean exit. Then the file is changed on disk (another
    // program, a copy synced from elsewhere) after the copy was made.
    drop(h);
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(120);
    std::fs::File::options().write(true).open(&project).unwrap().set_modified(later).unwrap();

    let mut h = launch(&dir.0, &prefs);
    assert!(file(&dir.0, PENDING).is_file() && !file(&dir.0, LIVE).exists(), "set aside to be offered");
    assert_eq!(h.state_mut().run("ui.autosave", json!({})).unwrap()["pending"]["name"], "Parcels");
    h.step();
    h.get_by_label("Recover unsaved work?");
    h.get_by_label("Parcels.rgis has changed since this copy was made.");
    h.get_by_role_and_label(Role::Button, "Recover").click();
    settle(&mut h);
    assert!(h.query_by_label("Recover unsaved work?").is_none());
    let app = h.state();
    assert!(app.session.map().layer_by_name("Inspections").is_some());
    assert_eq!(app.session.path.as_deref(), Some(std::path::absolute(&project).unwrap().as_path()), "Save writes back to its file");
    assert!(app.session.dirty(), "recovered work is unsaved");
    assert!(
        !app.session.notices.iter().any(|n| n.text.contains("without credentials or URL fragments")),
        "a clean recovery reports no credential loss"
    );
    // The copy is this run's now, unchanged, until the work is saved.
    assert!(!file(&dir.0, PENDING).exists() && file(&dir.0, LIVE).is_file());
    h.state_mut().run("project.save", json!({})).unwrap();
    settle(&mut h);
    assert!(!file(&dir.0, LIVE).exists(), "saved work needs no copy");
    assert!(std::fs::read_to_string(&project).unwrap().contains("Inspections"));
    drop(h);

    // This task is stored, never run: recovering its credential needs no network request.
    // Nothing in this scenario is explicitly saved, so the saved-text cache cannot restore it.
    let dir = TempDir::new("crash-credentials");
    let mut h = launch(&dir.0, &prefs);
    let secret = "RGSYNTH_autosave_20261105";
    h.state_mut().session.project.tasks.push(Task {
        name: "Credential recovery".into(),
        description: String::new(),
        steps: vec![TaskStep {
            title: "Log in".into(),
            instructions: String::new(),
            command: "connection.login".into(),
            params: json!({"id": "autosave-unused", "apiKey": secret}),
        }],
    });
    edit(&mut h, "Credential recovery");
    settle(&mut h);
    assert!(!std::fs::read_to_string(file(&dir.0, LIVE)).unwrap().contains(secret), "the recovery file contains no token");
    assert_eq!(h.state().session.project.tasks.last().unwrap().steps[0].params["apiKey"], secret, "autosave preserves the live token");
    drop(h);

    let mut h = launch(&dir.0, &prefs);
    h.step();
    h.get_by_role_and_label(Role::Button, "Recover").click();
    settle(&mut h);
    let app = h.state();
    assert!(app.session.map().layer_by_name("Credential recovery").is_some());
    assert!(!serde_json::to_string(&app.session.project).unwrap().contains(secret), "the recovered state contains no token");
    let losses: Vec<_> = app.session.notices.iter().filter(|n| n.text.contains("without credentials or URL fragments")).collect();
    assert_eq!(losses.len(), 1, "recovery reports the credential loss once");
    assert_eq!(
        losses[0].text,
        "1 executable step(s) were written without credentials or URL fragments; supply missing values again before replay"
    );
    assert!(!losses[0].error);
}

#[test]
fn a_clean_exit_leaves_nothing_to_recover() {
    let dir = TempDir::new("clean");
    let prefs = Prefs::default();
    let mut h = launch(&dir.0, &prefs);
    edit(&mut h, "Unsaved");
    settle(&mut h);
    assert!(file(&dir.0, LIVE).is_file());
    quit_without_saving(&mut h);
    assert!(!file(&dir.0, LIVE).exists());
    drop(h);
    let mut h = launch(&dir.0, &prefs);
    h.step();
    assert!(h.query_by_label("Recover unsaved work?").is_none());
    assert!(h.state_mut().run("ui.recover", json!({})).is_err(), "nothing to recover");
}

/// The app ended with unsaved work and the save prompt never answered (macOS's Quit, which
/// skips the window's close request): the work is kept, though the exit was clean, and the next
/// start offers it back, saying it was quit. With autosave off nothing is kept.
#[test]
fn quitting_unasked_keeps_the_work() {
    let dir = TempDir::new("unasked");
    let prefs = Prefs::default();
    let mut h = launch(&dir.0, &prefs);
    frames(&mut h, 2);
    // Edited after the last copy: the one kept is the work as it was at the end.
    edit(&mut h, "Early");
    settle(&mut h);
    edit(&mut h, "Last");
    h.state_mut().end_autosave();
    drop(h);
    let mut h = launch(&dir.0, &prefs);
    h.step();
    h.get_by_label("RustGIS was quit last time without saving your changes.");
    assert!(h.query_by_label("RustGIS didn't close properly last time.").is_none());
    h.get_by_role_and_label(Role::Button, "Recover").click();
    settle(&mut h);
    assert!(h.state().session.map().layer_by_name("Last").is_some() && h.state().session.dirty());
    // Autosave off: no copy to keep.
    h.state_mut().run("project.new", json!({})).unwrap();
    h.state_mut().run("ui.autosave", json!({"minutes": 0})).unwrap();
    edit(&mut h, "Gone");
    h.state_mut().end_autosave();
    drop(h);
    let mut h = launch(&dir.0, &prefs);
    h.step();
    assert!(h.query_by_label("Recover unsaved work?").is_none());
}

#[test]
fn later_offers_it_again_and_discard_deletes_it() {
    let dir = TempDir::new("later");
    let prefs = Prefs::default();
    let mut h = launch(&dir.0, &prefs);
    edit(&mut h, "Kept");
    settle(&mut h);
    drop(h);
    // Later: this run goes on with what it opened; the copy waits for the next start, and this
    // run's own unsaved work doesn't replace it.
    let mut h = launch(&dir.0, &prefs);
    h.step();
    h.get_by_role_and_label(Role::Button, "Later").click();
    frames(&mut h, 2);
    assert!(h.query_by_label("Recover unsaved work?").is_none());
    assert!(h.state().session.map().layer_by_name("Kept").is_none());
    edit(&mut h, "Other");
    settle(&mut h);
    quit_without_saving(&mut h);
    drop(h);
    let mut h = launch(&dir.0, &prefs);
    assert!(std::fs::read_to_string(file(&dir.0, PENDING)).unwrap().contains("Kept"));
    h.step();
    h.get_by_role_and_label(Role::Button, "Discard").click();
    settle(&mut h);
    assert!(h.query_by_label("Recover unsaved work?").is_none());
    assert!(!file(&dir.0, PENDING).exists());
    assert!(h.state().session.map().layer_by_name("Kept").is_none());
}

#[test]
fn a_damaged_copy_is_refused_and_can_be_discarded() {
    let dir = TempDir::new("damaged");
    let prefs = Prefs::default();
    let pending = file(&dir.0, PENDING);
    let bad: [&[u8]; 6] = [
        b"",
        b"no header line",
        b"{\"name\":\"Cut\"}\n{\"format\":1,\"maps\":[{\"name\"",
        b"{\"name\":\"Newer\"}\n{\"format\":999}",
        b"garbled header\n{}{}",
        &[0xff, 0xfe, b'\n', 0x80],
    ];
    for bytes in bad {
        std::fs::create_dir_all(pending.parent().unwrap()).unwrap();
        std::fs::write(&pending, bytes).unwrap();
        let mut h = launch(&dir.0, &prefs);
        h.step();
        h.get_by_role_and_label(Role::Button, "Recover").click();
        frames(&mut h, 4);
        assert!(h.query_by_label_contains("It didn't open").is_some(), "{bytes:?}");
        assert!(h.query_by_role_and_label(Role::Button, "Recover").is_none(), "a copy that didn't open isn't offered again");
        assert!(h.state().session.map().layer_by_name("Countries").is_some(), "the sample is still open");
        h.get_by_role_and_label(Role::Button, "Discard").click();
        settle(&mut h);
        assert!(!pending.exists());
    }
}

#[test]
fn the_interval_is_a_setting_and_off_keeps_no_copy() {
    let dir = TempDir::new("interval");
    let prefs = Prefs::default();
    let mut h = launch(&dir.0, &prefs);
    let r = h.state_mut().run("ui.autosave", json!({})).unwrap();
    assert_eq!((r["available"].clone(), r["minutes"].clone(), r["pending"].clone()), (json!(true), json!(5), json!(null)));
    for bad in [json!(-1), json!(61), json!(2.5), json!("5"), json!(null), json!(u64::MAX)] {
        assert!(h.state_mut().run("ui.autosave", json!({"minutes": bad})).is_err(), "{bad}");
    }
    for bad in [json!("yes"), json!(1), json!([])] {
        assert!(h.state_mut().run("ui.recover", json!({"discard": bad})).is_err(), "{bad}");
    }
    edit(&mut h, "Unsaved");
    settle(&mut h);
    assert!(file(&dir.0, LIVE).is_file());
    // Off: the copy goes, and none is made while it stays off.
    assert_eq!(h.state_mut().run("ui.autosave", json!({"minutes": 0})).unwrap()["minutes"], 0);
    settle(&mut h);
    assert!(!file(&dir.0, LIVE).exists());
    edit(&mut h, "Another");
    settle(&mut h);
    assert!(!file(&dir.0, LIVE).exists());
    assert_eq!(prefs.lock().unwrap().get("autosave_minutes").map(String::as_str), Some("0"));
    // Remembered by the next launch; a value edited out of range means the default.
    drop(h);
    let mut h = launch(&dir.0, &prefs);
    assert_eq!(h.state_mut().run("ui.autosave", json!({})).unwrap()["minutes"], 0);
    prefs.lock().unwrap().insert("autosave_minutes".into(), "9000".into());
    let mut h2 = launch(&dir.0, &prefs);
    assert_eq!(h2.state_mut().run("ui.autosave", json!({})).unwrap()["minutes"], 5);
    // A shell without a recovery store (the browser) has no autosave to set.
    let mut web = RustGisApp::with_sample();
    assert_eq!(web.run("ui.autosave", json!({})).unwrap()["available"], false);
    assert!(web.run("ui.autosave", json!({"minutes": 5})).is_err());
    assert!(web.run("ui.recover", json!({})).is_err());
}

/// Later, more work, another crash: neither copy replaces the other. The newest is offered first,
/// recovering it leaves the other for the next start (it would replace the recovered work), and
/// recovery never replaces a project with unsaved work of its own.
#[test]
fn copies_from_two_crashes_are_both_offered_in_turn() {
    let dir = TempDir::new("two");
    let prefs = Prefs::default();
    let mut h = launch(&dir.0, &prefs);
    edit(&mut h, "First");
    settle(&mut h);
    drop(h);
    let mut h = launch(&dir.0, &prefs);
    h.step();
    h.get_by_role_and_label(Role::Button, "Later").click();
    frames(&mut h, 2);
    edit(&mut h, "Second");
    let refused = h.state_mut().run("ui.recover", json!({})).unwrap_err();
    assert!(refused.contains("unsaved changes"), "{refused}");
    assert!(h.state().session.map().layer_by_name("Second").is_some(), "the open work is kept");
    settle(&mut h);
    drop(h);

    let mut h = launch(&dir.0, &prefs);
    assert!(h.state_mut().run("ui.autosave", json!({})).unwrap()["pending"].is_object());
    h.step();
    h.get_by_label("Older copies waiting: 1.");
    h.get_by_role_and_label(Role::Button, "Recover").click();
    settle(&mut h);
    assert!(h.state().session.map().layer_by_name("Second").is_some(), "the newest first");
    assert!(h.query_by_label("Recover unsaved work?").is_none(), "the other waits for the next start");
    quit_without_saving(&mut h);
    drop(h);

    let mut h = launch(&dir.0, &prefs);
    h.step();
    h.get_by_role_and_label(Role::Button, "Recover").click();
    settle(&mut h);
    assert!(h.state().session.map().layer_by_name("First").is_some());
    assert!(h.state_mut().run("ui.recover", json!({})).is_err(), "none left");
}

/// The recovery commands with junk parameters (as `hostile_params_never_panic` does for the
/// engine's): refused or ignored, never a panic, and the waiting copy isn't touched by a refusal.
#[test]
fn recovery_commands_take_junk_parameters() {
    let dir = TempDir::new("junk");
    let prefs = Prefs::default();
    let mut h = launch(&dir.0, &prefs);
    edit(&mut h, "Kept");
    settle(&mut h);
    drop(h);
    let mut h = launch(&dir.0, &prefs);
    let junk = [
        json!(null),
        json!(true),
        json!(-1),
        json!(1e308),
        json!(u64::MAX),
        json!("x"),
        json!([1, 2]),
        json!({"nested": {"minutes": 5}}),
        json!("\u{0}"),
    ];
    for v in &junk {
        for key in ["minutes", "discard", "text", "path"] {
            let _ = h.state_mut().run("ui.autosave", json!({key: v}));
            if key != "discard" || v.as_bool() != Some(true) {
                let _ = h.state_mut().run("ui.recover", json!({key: v}));
            }
        }
        let _ = h.state_mut().run("ui.autosave", v.clone());
    }
    // `{"text": …}` and the like are ignored: the first such run recovered the copy.
    assert!(h.state().session.map().layer_by_name("Kept").is_some());
    settle(&mut h);
}

/// Work done before answering the dialog: Recover waits until it is saved, rather
/// than failing and going for the rest of the session, and then recovers the copy.
#[test]
fn recover_returns_once_the_open_work_is_saved() {
    let dir = TempDir::new("blocked");
    let prefs = Prefs::default();
    let mut h = launch(&dir.0, &prefs);
    edit(&mut h, "Kept");
    settle(&mut h);
    drop(h);
    let mut h = launch(&dir.0, &prefs);
    edit(&mut h, "Meanwhile");
    frames(&mut h, 2);
    h.get_by_label("The open project has unsaved changes: save them, then recover this copy.");
    assert!(h.query_by_role_and_label(Role::Button, "Recover").is_none());
    let saved = dir.0.join("Meanwhile.rgis");
    h.state_mut().run("project.saveAs", json!({"path": saved.display().to_string()})).unwrap();
    frames(&mut h, 2);
    assert!(h.query_by_label_contains("unsaved changes: save them").is_none());
    h.get_by_role_and_label(Role::Button, "Recover").click();
    settle(&mut h);
    assert!(h.state().session.map().layer_by_name("Kept").is_some());
    assert!(h.query_by_label("Recover unsaved work?").is_none());
}

/// A copy that fails in the background wakes the interface, so the failure is told
/// on an idle window too, not at the next input.
#[test]
fn a_failed_copy_wakes_the_interface() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let dir = TempDir::new("wake");
    let recovery = dir.0.join("recovery");
    let store = FolderStore::open(&recovery).unwrap();
    // A folder where the copy goes: the write can't replace it.
    std::fs::create_dir_all(recovery.join(LIVE).join("blocked")).unwrap();
    let woken = std::sync::Arc::new(AtomicBool::new(false));
    let flag = woken.clone();
    store.set_wake(Box::new(move || flag.store(true, Ordering::SeqCst)));
    store.write(rustgis_engine::sample::world_project(), rustgis_ui_egui::Recovery::default());
    store.flush();
    assert!(woken.load(Ordering::SeqCst), "woken after the failure");
    assert!(store.take_error().is_some());
}
