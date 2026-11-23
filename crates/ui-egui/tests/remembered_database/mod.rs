#![expect(clippy::disallowed_methods, reason = "a thread the system refuses fails the test, as it should")]
use super::*;
use egui::accesskit::Role;
use rustgis_engine::data::secrets::{MemorySecrets, SecretError, SecretStore};
use rustgis_ui_egui::Services;
use std::sync::Arc;

fn with_store(store: Arc<dyn SecretStore>) -> egui_kittest::Harness<'static, RustGisApp> {
    let app = RustGisApp::with_sample().with_services(Services { secrets: Some(store), ..Default::default() });
    egui_kittest::Harness::builder().with_size(egui::vec2(1400.0, 860.0)).with_max_steps(64).build_ui_state(
        |ui, app: &mut RustGisApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        app,
    )
}
fn settle(h: &mut egui_kittest::Harness<'_, RustGisApp>) {
    for _ in 0..4 {
        h.step();
    }
}
const ADDRESS: &str = "postgres:host=127.0.0.1 port=1 dbname=ui_remembered user=smoke sslmode=disable";

#[test]
fn remembered_database_checkbox_test_save_forget_and_reset() {
    let store = Arc::new(MemorySecrets::new());
    let mut h = with_store(store.clone());
    h.state_mut().run("ui.connection", json!({"kind":"postgres"})).unwrap();
    h.state_mut().ui.connection.name = "Remembered UI".into();
    h.state_mut().ui.connection.address = ADDRESS.into();
    h.state_mut().ui.connection.password = "ui-password-sentinel".into();
    settle(&mut h);
    assert!(!h.state().ui.connection.remember);
    h.get_by_label("Remember on this computer").click();
    settle(&mut h);
    assert!(h.state().ui.connection.remember);
    h.get_by_role_and_label(Role::Button, "Test").click();
    settle(&mut h);
    assert!(h.state().session.connections.connections.is_empty());
    assert_eq!(store.load("connection/c1").unwrap(), None);
    h.get_by_role_and_label(Role::Button, "Save").click();
    settle(&mut h);
    assert_eq!(h.state().dialogs.open, None);
    let id = h.state().session.connections.find("Remembered UI").unwrap().id.clone();
    assert!(h.state().session.connections.find(&id).unwrap().remembered);
    assert!(store.load(&format!("connection/{id}")).unwrap().is_some());
    h.state_mut().run("ui.connection", json!({"id":id})).unwrap();
    settle(&mut h);
    assert!(!h.state().ui.connection.remember);
    assert!(h.state().ui.connection.password.is_empty());
    h.get_by_role_and_label(Role::Button, "Cancel").click();
    h.state_mut().run("ui.pane.sources", json!({})).unwrap();
    settle(&mut h);
    h.get_by_label("Remembered UI").click_secondary();
    settle(&mut h);
    h.get_by_label("Forget Saved Password").click();
    settle(&mut h);
    assert_eq!(store.load(&format!("connection/{id}")).unwrap(), None);
    assert!(!h.state().session.connections.find(&id).unwrap().remembered);
    assert!(h.state().session.database_logins.has_session_password(&id));
}

#[test]
fn remembered_database_failed_store_has_visible_notice() {
    let mut h = with_store(Arc::new(MemorySecrets::failing(SecretError::Denied)));
    h.state_mut().run("ui.connection", json!({"kind":"postgres"})).unwrap();
    h.state_mut().ui.connection.name = "Session only UI".into();
    h.state_mut().ui.connection.address = ADDRESS.into();
    h.state_mut().ui.connection.password = "session-only-sentinel".into();
    settle(&mut h);
    h.get_by_label("Remember on this computer").click();
    settle(&mut h);
    h.get_by_role_and_label(Role::Button, "Save").click();
    settle(&mut h);
    assert_eq!(h.state().dialogs.open, None);
    let c = h.state().session.connections.find("Session only UI").unwrap();
    assert!(!c.remembered);
    assert!(h.state().session.database_logins.has_session_password(&c.id));
    assert!(h.state().session.notices.iter().any(|n| n.error));
    assert!(!h.state().session.notices.iter().any(|n| n.text.contains("session-only-sentinel")));
}

#[test]
fn remembered_database_chooser_cancel_selection_and_project_scope() {
    // Its own database: passwords for an address are remembered process-wide, and one the other
    // tests leave for the shared address would open the project without asking which to use.
    const ADDRESS: &str = "postgres:host=127.0.0.1 port=1 dbname=ui_remembered_chooser user=smoke sslmode=disable";
    let store = Arc::new(MemorySecrets::new());
    let mut seed = with_store(store.clone());
    for name in ["First database", "Second database"] {
        let id = seed.state_mut().run("connection.new", json!({"kind":"postgres","name":name,"address":ADDRESS})).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        seed.state_mut().run("connection.login", json!({"id":id,"password":"chooser-ui-sentinel","remember":true})).unwrap();
    }
    let prefs = seed.state().session.connections.to_json();
    let mut project = seed.state().session.project.clone();
    project.map_mut().layers[0].source =
        Some(serde_json::from_value(json!({"postgis":{"database":ADDRESS,"table":"roads","family":"Lines"}})).unwrap());
    let text = serde_json::to_string(&project).unwrap();
    let mut h = with_store(store);
    h.state_mut().session.connections = rustgis_engine::connections::Connections::from_json(&prefs);
    h.state_mut().run("project.openText", json!({"text":text})).unwrap();
    settle(&mut h);
    assert_eq!(h.state().dialogs.open, Some("chooseConnection"));
    assert!(
        h.state().session.database_logins.selection("postgres", &rustgis_engine::connections::database_key(ADDRESS).unwrap().1).is_none()
    );
    h.get_by_role_and_label(Role::Button, "Cancel").click();
    settle(&mut h);
    assert_eq!(h.state().dialogs.open, None);
    assert!(!h.state().session.map().layers[0].source.as_ref().unwrap().is_open());
    h.state_mut().run("project.openText", json!({"text":text})).unwrap();
    settle(&mut h);
    h.query_all_by_label("Second database").next().unwrap().click();
    settle(&mut h);
    h.get_by_role_and_label(Role::Button, "Use Connection").click();
    settle(&mut h);
    let key = rustgis_engine::connections::database_key(ADDRESS).unwrap().1;
    assert_eq!(h.state().session.database_logins.selection("postgres", &key), Some("c2"));
    h.state_mut().run("project.new", json!({})).unwrap();
    settle(&mut h);
    assert_ne!(h.state().dialogs.open, Some("chooseConnection"));
    assert!(h.state().session.database_logins.requests(h.state().session.opened).next().is_none());
}

#[test]
fn remembered_database_refused_saved_login_opens_empty_unchecked_form() {
    if std::env::var_os("RUSTGIS_UI_REFUSAL_CHILD").is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "remembered_database::remembered_database_refused_saved_login_opens_empty_unchecked_form",
                "--test-threads=1",
            ])
            .env("RUSTGIS_UI_REFUSAL_CHILD", "1")
            .env_remove("PGPASSWORD")
            .env_remove("PGPASSFILE")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut summaries = stdout.lines().filter(|line| line.starts_with("test result:"));
        let ran_one = summaries.next().is_some_and(|line| line.starts_with("test result: ok. 1 passed; 0 failed; 0 ignored;"))
            && summaries.next().is_none();
        assert!(
            output.status.success() && ran_one,
            "isolated UI refusal failed or did not run exactly one test:\n{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let worker = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(std::time::Duration::from_secs(3))).unwrap();
            let mut size = [0; 4];
            if stream.read_exact(&mut size).is_err() {
                continue;
            }
            let size = u32::from_be_bytes(size);
            assert!((4..=65536).contains(&size));
            if stream.read_exact(&mut vec![0; (size - 4) as usize]).is_err() {
                continue;
            }
            stream.write_all(b"R\0\0\0\x08\0\0\0\x03").unwrap();
            let mut tag = [0];
            if stream.read_exact(&mut tag).is_err() {
                continue;
            }
            let mut size = [0; 4];
            if tag[0] != b'p' || stream.read_exact(&mut size).is_err() {
                continue;
            }
            let size = u32::from_be_bytes(size);
            assert!((4..=65536).contains(&size));
            let mut password = vec![0; (size - 4) as usize];
            stream.read_exact(&mut password).unwrap();
            assert_eq!(password, b"refused-ui-sentinel\0");
            let body = b"SFATAL\0VFATAL\0C28P01\0Mrefused refused-ui-sentinel\0\0";
            stream.write_all(b"E").unwrap();
            stream.write_all(&((body.len() + 4) as u32).to_be_bytes()).unwrap();
            stream.write_all(body).unwrap();
        }
    });
    let store = Arc::new(MemorySecrets::new());
    let address = format!("postgres:host=127.0.0.1 port={port} dbname=gis user=refused_ui sslmode=disable");
    let mut seed = with_store(store.clone());
    seed.state_mut().run("connection.new", json!({"kind":"postgres","name":"Refused UI","address":address})).unwrap();
    seed.state_mut().run("connection.login", json!({"id":"c1","password":"refused-ui-sentinel","remember":true})).unwrap();
    let prefs = seed.state().session.connections.to_json();
    let mut project = seed.state().session.project.clone();
    project.map_mut().layers[0].source =
        Some(serde_json::from_value(json!({"postgis":{"database":address,"table":"roads","family":"Lines"}})).unwrap());
    let mut h = with_store(store);
    h.state_mut().session.connections = rustgis_engine::connections::Connections::from_json(&prefs);
    h.state_mut().run("project.openText", json!({"text":serde_json::to_string(&project).unwrap()})).unwrap();
    worker.join().unwrap();
    settle(&mut h);
    assert_eq!(h.state().dialogs.open, Some("editConnection"));
    assert!(h.state().ui.connection.password.is_empty());
    assert!(!h.state().ui.connection.remember);
    assert!(h.state().dialogs.info.is_some());
    assert!(h.state().session.database_logins.is_rejected("c1"));
    assert!(!h.state().session.notices.iter().any(|n| n.text.contains("refused-ui-sentinel")));
}

#[test]
fn remembered_database_recording_history_recent_and_preferences_never_get_sentinel() {
    use std::collections::HashMap;
    use std::sync::Mutex;
    let store = Arc::new(MemorySecrets::new());
    let prefs = Arc::new(Mutex::new(HashMap::<String, String>::new()));
    let (load, save) = (prefs.clone(), prefs.clone());
    let mut app = RustGisApp::with_sample().with_services(Services {
        secrets: Some(store.clone()),
        load_pref: Some(Box::new(move |k| load.lock().unwrap().get(k).cloned())),
        save_pref: Some(Box::new(move |k, v| {
            save.lock().unwrap().insert(k.into(), v.into());
        })),
        ..Default::default()
    });
    app.run("ui.taskRecord", json!({})).unwrap();
    app.run("connection.new", json!({"kind":"postgres","name":"No leak","address":ADDRESS})).unwrap();
    app.run("connection.login", json!({"id":"No leak","password":"boundary-sentinel","remember":true})).unwrap();
    let mut project = app.session.project.clone();
    let (text, _) = project.saved_text(None).unwrap();
    let outputs = [
        serde_json::to_string(&app.ws.recording).unwrap(),
        app.run("sources.list", json!({})).unwrap().to_string(),
        app.run("connection.list", json!({})).unwrap().to_string(),
        app.run("gp.historyScript", json!({})).unwrap_or_default().to_string(),
        serde_json::to_string(&*prefs.lock().unwrap()).unwrap(),
        text,
        format!("{:?}", app.session.database_logins),
        format!("{:?}", app.session.notices),
    ];
    for output in outputs {
        assert!(!output.contains("boundary-sentinel"), "{output}");
        assert!(!output.contains("\"password\":\"boundary-sentinel\""), "{output}");
    }
    assert!(store.load("connection/c1").unwrap().is_some());
}
