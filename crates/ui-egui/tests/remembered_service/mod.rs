use super::*;
use egui::accesskit::Role;
use egui_kittest::kittest::NodeT;
use rustgis_engine::data::secrets::{MemorySecrets, Secret, SecretError, SecretStore};
use rustgis_engine::service_logins as web;
use rustgis_ui_egui::Services;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

type Harness = egui_kittest::Harness<'static, RustGisApp>;
const SECRET: &str = "remembered-web-widget-secret-sentinel";

#[derive(Default)]
struct CountedStore {
    inner: MemorySecrets,
    loads: AtomicUsize,
}
impl SecretStore for CountedStore {
    fn load(&self, account: &str) -> Result<Option<Secret>, SecretError> {
        // First-frame Earthdata restoration shares this store and can run on a worker.
        if account.starts_with("connection/") {
            self.loads.fetch_add(1, Ordering::Relaxed);
        }
        self.inner.load(account)
    }
    fn save(&self, account: &str, secret: &Secret) -> Result<(), SecretError> {
        self.inner.save(account, secret)
    }
    fn delete(&self, account: &str) -> Result<(), SecretError> {
        self.inner.delete(account)
    }
}

fn with_store(store: Option<Arc<dyn SecretStore>>) -> Harness {
    with_app(RustGisApp::with_sample().with_services(Services { secrets: store, ..Default::default() }))
}
fn with_app(app: RustGisApp) -> Harness {
    egui_kittest::Harness::builder().with_size(egui::vec2(1400.0, 1000.0)).with_max_steps(64).build_ui_state(
        |ui, app: &mut RustGisApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        app,
    )
}
fn settle(h: &mut Harness) {
    h.run_steps(4);
}
fn type_field(h: &mut Harness, label: &str, text: &str, secret: bool) {
    h.get_by_role_and_label(if secret { Role::PasswordInput } else { Role::TextInput }, label).focus();
    h.step();
    h.event(egui::Event::Text(text.into()));
    settle(h);
}
fn form(h: &mut Harness, method: &str, address: &str) {
    h.state_mut().run("ui.connection", json!({"kind":"wms"})).unwrap();
    settle(h);
    type_field(h, "Name", "Remembered web login", false);
    type_field(h, "Address", address, false);
    h.get_by_role_and_label(Role::ComboBox, "Method").click();
    settle(h);
    h.get_by_label(if method == "query" { "API key in the query" } else { "API key in a header" }).click();
    settle(h);
    type_field(
        h,
        if method == "query" { "Parameter name" } else { "Header name" },
        if method == "query" { "api_key" } else { "X-Api-Key" },
        false,
    );
    type_field(h, "Key", SECRET, true);
    assert!(!h.state().ui.connection.remember, "Remember is always an opt-in");
}
fn save(h: &mut Harness) {
    h.get_by_role_and_label(Role::Button, "Save").click();
    settle(h);
}
fn assert_redacted(h: &Harness) {
    let app = h.state();
    assert!(!app.session.connections.to_json().to_string().contains(SECRET));
    assert!(!app.session.notices.iter().any(|n| n.text.contains(SECRET)));
    assert!(app.ui.connection.api_key.is_empty());
    assert!(app.ui.connection.password.is_empty());
    assert!(!app.ui.connection.remember);
    assert!(!app.ui.connection.allow_insecure);
    assert!(!app.dialogs.error.as_deref().unwrap_or("").contains(SECRET));
    assert!(!app.dialogs.info.as_deref().unwrap_or("").contains(SECRET));
    assert!(!app.ui.connection.warned.as_deref().unwrap_or("").contains(SECRET));
    for entry in app.session.log.entries().filter(|entry| entry.command == "connection.login") {
        assert!(!entry.params.to_string().contains(SECRET));
    }
}

#[test]
fn remembered_service_save_metadata_status_sticky_logout_and_forget() {
    let store = Arc::new(CountedStore::default());
    let mut h = with_store(Some(store.clone()));
    form(&mut h, "header", "https://remembered-widget.example/wms");
    h.get_by_label("Remember on this computer").click();
    settle(&mut h);
    save(&mut h);
    assert_eq!(h.state().dialogs.open, None);
    let c = h.state().session.connections.find("Remembered web login").unwrap().clone();
    assert!(c.remembered);
    let account = format!("connection/{}", c.id);
    assert!(store.inner.load(&account).unwrap().unwrap().expose().contains(SECRET));
    assert_redacted(&h);

    let loads = store.loads.load(Ordering::Relaxed);
    h.state_mut().run("ui.connection", json!({"id":c.id})).unwrap();
    settle(&mut h);
    h.get_by_label("Logged in");
    h.get_by_label("Saved on this computer");
    h.get_by_label("Remember on this computer");
    assert!(!h.state().ui.connection.remember, "a saved login never preselects Remember");
    h.state_mut().run("connection.list", json!({})).unwrap();
    assert_eq!(store.loads.load(Ordering::Relaxed), loads, "painting and listing only use metadata");

    h.get_by_role_and_label(Role::Button, "Log Out").click();
    settle(&mut h);
    assert!(!web::logged_in(&h.state().session, &c));
    h.get_by_label("Saved on this computer");
    assert!(store.inner.load(&account).unwrap().is_some());
    assert_eq!(web::known_secret(&mut h.state_mut().session, &c).unwrap_err(), web::LOGGED_OUT);
    assert_eq!(store.loads.load(Ordering::Relaxed), loads, "logout prevents automatic store reads");

    h.get_by_role_and_label(Role::Button, "Cancel").click();
    h.state_mut().run("ui.pane.sources", json!({})).unwrap();
    settle(&mut h);
    h.get_by_label(&c.name).click_secondary();
    settle(&mut h);
    h.get_by_role_and_label(Role::Button, "Add With Options…").click();
    settle(&mut h);
    assert_eq!(h.state().dialogs.open, Some("editConnection"));
    assert_eq!(h.state().dialogs.error.as_deref(), Some(web::LOGGED_OUT));
    assert!(h.state().ui.connection.login, "browse asks for an explicit login after logout");
    assert!(!web::logged_in(&h.state().session, &c));
    assert_eq!(store.loads.load(Ordering::Relaxed), loads, "browse never undoes a deliberate logout");

    save(&mut h);
    assert!(web::logged_in(&h.state().session, &c), "an explicit blank login reads the saved secret");
    assert_eq!(store.loads.load(Ordering::Relaxed), loads + 1);
    assert_redacted(&h);
    h.state_mut().run("ui.connection", json!({"id":c.id})).unwrap();
    settle(&mut h);
    h.get_by_role_and_label(Role::Button, "Forget Saved Login").click();
    settle(&mut h);
    assert!(!h.state().session.connections.find(&c.id).unwrap().remembered);
    assert!(store.inner.load(&account).unwrap().is_none());
    assert!(web::logged_in(&h.state().session, &c), "Forget retains this session's login");
    assert!(h.query_by_label("Saved on this computer").is_none());
    assert!(h.query_by_label("Forget Saved Login").is_none());
}

#[test]
fn remembered_query_login_reconnects_in_a_fresh_shell_without_storing_it_in_definitions() {
    let store = Arc::new(MemorySecrets::new());
    let mut first = with_store(Some(store.clone()));
    form(&mut first, "query", "https://remembered-query-widget.example/wms");
    first.get_by_label(
        "The key is added to each request to this address as this parameter. It is never saved in the connection or project.",
    );
    first.get_by_label("Remember on this computer").click();
    settle(&mut first);
    save(&mut first);
    let c = first.state().session.connections.find("Remembered web login").unwrap().clone();
    let definitions = first.state().session.connections.to_json().to_string();
    assert!(c.remembered);
    assert!(!definitions.contains(SECRET));
    assert_redacted(&first);
    drop(first);

    let app = RustGisApp::with_sample().with_services(Services {
        secrets: Some(store),
        load_pref: Some(Box::new(move |key| (key == rustgis_engine::connections::PREF).then(|| definitions.clone()))),
        ..Default::default()
    });
    let mut fresh = with_app(app);
    assert!(!web::logged_in(&fresh.state().session, &c));
    fresh.state_mut().run("ui.connection", json!({"id":c.id})).unwrap();
    fresh.state_mut().ui.connection.login = true;
    settle(&mut fresh);
    fresh.get_by_label("Saved on this computer");
    assert!(!fresh.state().ui.connection.remember);
    save(&mut fresh);
    assert_eq!(fresh.state().dialogs.open, None);
    assert!(web::logged_in(&fresh.state().session, &c));
    assert_eq!(web::known_secret(&mut fresh.state_mut().session, &c).unwrap().expose(), SECRET);
    assert_redacted(&fresh);
}

#[test]
fn repeatedly_opening_a_service_keeps_a_failed_saved_read_until_explicit_login() {
    let store = Arc::new(CountedStore { inner: MemorySecrets::failing(SecretError::Denied), ..Default::default() });
    let mut h = with_store(Some(store.clone()));
    let made = h
        .state_mut()
        .run(
            "connection.new",
            json!({
                "kind":"wms", "name":"Unavailable saved login", "address":"https://remembered-read-failure-widget.example/wms",
                "auth":"header", "keyName":"X-Api-Key"
            }),
        )
        .unwrap();
    let id = made["id"].as_str().unwrap();
    h.state_mut().session.connections.set_remembered(id, true);
    h.state_mut().run("ui.pane.sources", json!({})).unwrap();
    settle(&mut h);

    for _ in 0..3 {
        h.get_by_label("Unavailable saved login").click_secondary();
        settle(&mut h);
        h.get_by_role_and_label(Role::Button, "Add With Options…").click();
        settle(&mut h);
        assert_eq!(h.state().dialogs.open, Some("editConnection"));
        assert_eq!(h.state().dialogs.error.as_deref(), Some(web::UNAVAILABLE));
        assert!(h.state().ui.connection.login);
        assert_eq!(store.loads.load(Ordering::Relaxed), 1, "opening a source is not a saved-store retry");
        assert!(!h.state().session.log.entries().any(|entry| entry.command == "connection.login"));
        h.get_by_role_and_label(Role::Button, "Cancel").click();
        settle(&mut h);
    }

    h.get_by_label("Unavailable saved login").click_secondary();
    settle(&mut h);
    h.get_by_role_and_label(Role::Button, "Add With Options…").click();
    settle(&mut h);
    save(&mut h);
    assert_eq!(h.state().dialogs.error.as_deref(), Some(web::UNAVAILABLE));
    assert_eq!(store.loads.load(Ordering::Relaxed), 2, "explicit login retries the saved store");
    assert_redacted(&h);
}

#[test]
fn remembered_service_failed_save_warns_and_retains_only_the_session_login() {
    let mut h = with_store(Some(Arc::new(MemorySecrets::failing(SecretError::Denied))));
    form(&mut h, "header", "https://remembered-warning-widget.example/wms");
    h.get_by_label("Remember on this computer").click();
    settle(&mut h);
    save(&mut h);
    assert_eq!(h.state().dialogs.open, Some("editConnection"));
    h.get_by_label("Logged in for this session");
    h.get_by_label(web::NOT_REMEMBERED);
    assert_eq!(h.state().ui.connection.warned.as_deref(), Some(web::NOT_REMEMBERED));
    let c = h.state().session.connections.find("Remembered web login").unwrap();
    assert!(!c.remembered);
    assert!(web::logged_in(&h.state().session, c));
    assert_redacted(&h);
    h.get_by_role_and_label(Role::Button, "Close").click();
    settle(&mut h);
    assert_eq!(h.state().dialogs.open, None);
}

#[test]
fn remembered_service_failed_forget_keeps_saved_metadata_and_session_login() {
    let store = Arc::new(MemorySecrets::new());
    let mut h = with_store(Some(store.clone()));
    form(&mut h, "header", "https://remembered-forget-widget.example/wms");
    h.get_by_label("Remember on this computer").click();
    settle(&mut h);
    save(&mut h);
    let c = h.state().session.connections.find("Remembered web login").unwrap().clone();
    h.state_mut().session.database_logins.set_store(Some(Arc::new(MemorySecrets::failing(SecretError::Denied))));
    h.state_mut().run("ui.connection", json!({"id":c.id})).unwrap();
    settle(&mut h);
    h.get_by_role_and_label(Role::Button, "Forget Saved Login").click();
    settle(&mut h);
    assert_eq!(h.state().dialogs.error.as_deref(), Some(web::NOT_FORGOTTEN));
    assert!(h.state().session.connections.find(&c.id).unwrap().remembered);
    assert!(store.load(&format!("connection/{}", c.id)).unwrap().is_some());
    assert!(web::logged_in(&h.state().session, &c));
    assert_redacted(&h);
}

#[test]
fn remember_without_a_typed_secret_does_not_edit_the_connection() {
    let mut h = with_store(Some(Arc::new(MemorySecrets::new())));
    let c = h
        .state_mut()
        .run(
            "connection.new",
            json!({
                "kind":"wms", "name":"Unchanged login", "address":"https://remembered-validation-widget.example/wms",
                "auth":"header", "keyName":"X-Api-Key"
            }),
        )
        .unwrap();
    let saved = h.state().session.connections.find(c["id"].as_str().unwrap()).unwrap().clone();
    h.state_mut().run("ui.connection", json!({"id":c["id"]})).unwrap();
    settle(&mut h);
    type_field(&mut h, "Group", "Must not be saved", false);
    h.get_by_label("Remember on this computer").click();
    settle(&mut h);
    save(&mut h);
    assert_eq!(h.state().dialogs.open, Some("editConnection"));
    assert_eq!(h.state().dialogs.error.as_deref(), Some("Type the password or key to remember it on this computer."));
    assert_eq!(h.state().session.connections.find(&saved.id), Some(&saved));
}

#[test]
fn service_without_a_store_hides_remember_and_marks_saved_login_unavailable() {
    let mut h = with_store(None);
    let c = h
        .state_mut()
        .run(
            "connection.new",
            json!({
                "kind":"wms", "address":"https://remembered-unavailable-widget.example/wms", "auth":"header", "keyName":"X-Api-Key"
            }),
        )
        .unwrap();
    let id = c["id"].as_str().unwrap();
    h.state_mut().session.connections.set_remembered(id, true);
    h.state_mut().run("ui.connection", json!({"id":id})).unwrap();
    settle(&mut h);
    h.get_by_label("Saved login unavailable");
    assert!(h.query_by_label("Remember on this computer").is_none());
    assert!(h.query_by_label("Saved on this computer").is_none());
    assert!(h.get_by_role_and_label(Role::Button, "Forget Saved Login").accesskit_node().is_disabled());
    assert!(h.state().session.connections.find(id).unwrap().remembered, "drawing never tries to read the saved login");
}
