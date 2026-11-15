use super::*;
use egui::accesskit::Role;
use rustgis_engine::data::secrets::{MemorySecrets, Secret, SecretError, SecretStore};
use rustgis_engine::render::rustgis_tiles::s3;
use rustgis_ui_egui::Services;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

type Harness = egui_kittest::Harness<'static, RustGisApp>;

#[derive(Default)]
struct CountedStore {
    inner: MemorySecrets,
    loads: AtomicUsize,
}
impl SecretStore for CountedStore {
    fn load(&self, account: &str) -> Result<Option<Secret>, SecretError> {
        self.loads.fetch_add(1, Ordering::Relaxed);
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
    let app = RustGisApp::with_sample().with_services(Services { secrets: store, ..Default::default() });
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
fn new_form(h: &mut Harness, name: &str) {
    h.state_mut().run("ui.connection", json!({"kind":"s3"})).unwrap();
    let st = &mut h.state_mut().ui.connection;
    st.name = name.into();
    st.address = "http://127.0.0.1:1".into();
    st.access_key = "SAVEDWIDGETKEY".into();
    st.secret_key = "saved-widget-secret".into();
    settle(h);
    assert!(!h.state().ui.connection.remember);
}
fn type_field(h: &mut Harness, label: &str, text: &str) {
    let role = if label == "Secret access key" { Role::PasswordInput } else { Role::TextInput };
    h.get_by_role_and_label(role, label).focus();
    h.step();
    h.event(egui::Event::Text(text.into()));
    settle(h);
}
fn expand_connection(h: &mut Harness) {
    if h.query_by_role_and_label(Role::Button, "Connect").is_none() {
        h.get_by_label("Connection").click();
        settle(h);
    }
}

#[test]
fn s3_saved_checkbox_save_row_forget_and_explicit_blank_login() {
    let store = Arc::new(CountedStore::default());
    let mut h = with_store(Some(store.clone()));
    let name = "s3-saved-widget-definition";
    new_form(&mut h, name);
    h.get_by_label("Remember on this computer").click();
    settle(&mut h);
    h.get_by_role_and_label(Role::Button, "Test").click();
    settle(&mut h);
    assert!(h.state().session.connections.connections.is_empty(), "Test never saves a definition");
    assert!(store.inner.load("connection/c1").unwrap().is_none(), "Test never remembers keys");
    for _ in 0..200 {
        if h.state().ui.connection.test.is_none() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10)); // test-wait: legacy polling for the S3 connection probe
        h.step();
    }
    assert!(h.state().ui.connection.test.is_none(), "localhost test must finish before Save");
    h.get_by_role_and_label(Role::Button, "Save").click();
    settle(&mut h);
    assert_eq!(h.state().dialogs.open, None);
    let id = h.state().session.connections.find(name).unwrap().id.clone();
    assert!(h.state().session.connections.find(&id).unwrap().saved_keys);
    assert!(store.inner.load(&format!("connection/{id}")).unwrap().is_some());
    assert!(h.state().ui.connection.access_key.is_empty());
    assert!(h.state().ui.connection.secret_key.is_empty());
    s3::disconnect(name);

    // A definition-only edit does not unlock the store or reconnect.
    h.state_mut().run("ui.connection", json!({"id":id})).unwrap();
    h.state_mut().ui.connection.group = "Widget group".into();
    settle(&mut h);
    assert!(!h.state().ui.connection.remember);
    let before = store.loads.load(Ordering::Relaxed);
    h.get_by_role_and_label(Role::Button, "Save").click();
    settle(&mut h);
    assert_eq!(store.loads.load(Ordering::Relaxed), before);
    assert!(s3::connection(name).is_none());

    h.state_mut().run("ui.pane.sources", json!({})).unwrap();
    settle(&mut h);
    h.get_by_label(name).click_secondary();
    settle(&mut h);
    h.get_by_label("Log In…").click();
    settle(&mut h);
    assert!(h.state().ui.connection.login);
    assert!(!h.state().ui.connection.remember);
    h.get_by_role_and_label(Role::Button, "Save").click();
    settle(&mut h);
    assert!(s3::connection(name).is_some());
    assert_eq!(s3::binding_of(name).as_deref(), h.state().session.s3_binding(&id));
    h.get_by_label("Saved keys");
    let before = store.loads.load(Ordering::Relaxed);
    h.run_steps(8);
    h.state_mut().run("connection.list", json!({})).unwrap();
    assert_eq!(store.loads.load(Ordering::Relaxed), before, "frames and lists never unlock secrets");
    h.get_by_label(name).click_secondary();
    settle(&mut h);
    h.get_by_label("Forget Saved Keys").click();
    settle(&mut h);
    assert!(!h.state().session.connections.find(&id).unwrap().saved_keys);
    assert!(store.inner.load(&format!("connection/{id}")).unwrap().is_none());
    assert!(s3::connection(name).is_some(), "Forget does not log out");
    assert!(h.query_by_label("Saved keys").is_none());
    s3::disconnect(name);
}

#[test]
fn s3_saved_connect_creates_once_and_same_name_edit_never_overwrites() {
    let store = Arc::new(MemorySecrets::new());
    let mut h = with_store(Some(store.clone()));
    h.state_mut().run("ui.dialog", json!({"name":"s3"})).unwrap();
    settle(&mut h);
    expand_connection(&mut h);
    type_field(&mut h, "Name", "s3-saved-widget-connect");
    type_field(&mut h, "Endpoint", "http://127.0.0.1:1");
    type_field(&mut h, "Access key ID", "SAVEDCONNECTKEY");
    type_field(&mut h, "Secret access key", "saved-connect-secret");
    h.get_by_label("Remember on this computer").click();
    settle(&mut h);
    h.get_by_role_and_label(Role::Button, "Connect").click();
    settle(&mut h);
    let c = h.state().session.connections.find("s3-saved-widget-connect").unwrap().clone();
    assert!(c.saved_keys);
    assert!(store.load(&format!("connection/{}", c.id)).unwrap().is_some());
    s3::disconnect(&c.name);
    // The same selected definition with blank keys logs in by its stable ID.
    h.get_by_role_and_label(Role::Button, "Connect").click();
    settle(&mut h);
    assert_eq!(h.state().session.connections.connections.len(), 1);
    assert_eq!(s3::binding_of(&c.name).as_deref(), h.state().session.s3_binding(&c.id));
    s3::disconnect(&c.name);
    type_field(&mut h, "Endpoint", "/changed");
    type_field(&mut h, "Access key ID", "OTHERCONNECTKEY");
    type_field(&mut h, "Secret access key", "other-connect-secret");
    h.get_by_label("Remember on this computer").click();
    settle(&mut h);
    h.get_by_role_and_label(Role::Button, "Connect").click();
    settle(&mut h);
    assert_eq!(h.state().session.connections.connections.len(), 1);
    assert_eq!(h.state().session.connections.find(&c.id).unwrap().address, c.address);
    assert!(s3::connection(&c.name).is_none());
    assert_eq!(h.get_by_role_and_label(Role::TextInput, "Access key ID").value().as_deref(), Some("OTHERCONNECTKEY"));
}

#[test]
fn s3_saved_failure_warning_stays_visible_until_close() {
    let mut h = with_store(Some(Arc::new(MemorySecrets::failing(SecretError::Denied))));
    let name = "s3-saved-widget-warning";
    new_form(&mut h, name);
    h.get_by_label("Remember on this computer").click();
    settle(&mut h);
    h.get_by_role_and_label(Role::Button, "Save").click();
    settle(&mut h);
    assert_eq!(h.state().dialogs.open, Some("editConnection"));
    h.get_by_label("Logged in for this session");
    assert!(h.state().ui.connection.warned.is_some());
    assert!(h.state().ui.connection.secret_key.is_empty());
    assert!(s3::connection(name).is_some());
    assert!(!h.state().session.connections.find(name).unwrap().saved_keys);
    h.get_by_role_and_label(Role::Button, "Close").click();
    settle(&mut h);
    assert_eq!(h.state().dialogs.open, None);
    s3::disconnect(name);
}

#[test]
fn s3_saved_chooser_cancel_selection_and_explicit_login_retry() {
    let mut h = with_store(Some(Arc::new(MemorySecrets::new())));
    let names = ["s3-saved-widget-choice-a", "s3-saved-widget-choice-b"];
    let mut ids = Vec::new();
    for name in names {
        let c = h
            .state_mut()
            .run(
                "connection.new",
                json!({"kind":"s3","name":name,"address":"http://127.0.0.1:1",
            "options":{"credentials":"keys","buckets":"s3-widget-choice-bucket"}}),
            )
            .unwrap();
        ids.push(c["id"].as_str().unwrap().to_string());
    }
    let inner = "s3://s3-widget-choice-bucket/fixture.pmtiles";
    let address = rustgis_engine::data::s3_address::bind("", "http://127.0.0.1:1", inner).unwrap();
    h.state_mut().run("map.addTiles", json!({"name":"S3 choice fixture","kind":"pmtiles","url":address})).unwrap();
    let text = serde_json::to_string(&h.state().session.project).unwrap();
    h.state_mut().run("project.openText", json!({"text":text})).unwrap();
    settle(&mut h);
    assert_eq!(h.state().dialogs.open, Some("s3Connections"));
    assert_eq!(h.state().session.s3_pending.len(), 1);
    // Clicking disabled Open must not choose the first matching saved connection.
    h.get_by_role_and_label(Role::Button, "Open").click();
    settle(&mut h);
    assert_eq!(h.state().session.s3_pending.len(), 1);
    assert!(h.state().session.s3_binding(&ids[0]).is_none());
    assert!(h.state().session.s3_binding(&ids[1]).is_none());
    h.get_by_role_and_label(Role::Button, "Cancel").click();
    settle(&mut h);
    assert_eq!(h.state().dialogs.open, None);
    assert_eq!(h.state().session.s3_pending.len(), 1);
    h.run_steps(8);
    assert_eq!(h.state().dialogs.open, None, "Cancel is not undone next frame");
    h.state_mut().run("ui.s3Connections", json!({})).unwrap();
    settle(&mut h);
    h.get_by_label("s3-saved-widget-choice-b — http://127.0.0.1:1").click();
    settle(&mut h);
    h.get_by_role_and_label(Role::Button, "Open").click();
    settle(&mut h);
    assert_eq!(h.state().dialogs.open, Some("editConnection"));
    assert_eq!(h.state().ui.connection.id.as_deref(), Some(ids[1].as_str()));
    assert!(!h.state().ui.connection.remember);
    h.state_mut().ui.connection.access_key = "CHOICEWIDGETKEY".into();
    h.state_mut().ui.connection.secret_key = "choice-widget-secret".into();
    h.get_by_role_and_label(Role::Button, "Save").click();
    settle(&mut h);
    assert!(h.state().session.s3_pending.is_empty());
    assert!(h.state().session.s3_binding(&ids[0]).is_none());
    let binding = h.state().session.s3_binding(&ids[1]).unwrap();
    assert!(h.state().session.map().tiles.iter().any(|t| t.url.contains(binding)));
    for name in names {
        s3::disconnect(name);
    }
}

#[test]
fn s3_saved_unavailable_and_temporary_forms_cannot_opt_in() {
    for (name, store, token) in [
        ("s3-saved-widget-no-store", None, ""),
        ("s3-saved-widget-temporary", Some(Arc::new(MemorySecrets::new()) as Arc<dyn SecretStore>), "widget-session-token"),
    ] {
        let mut h = with_store(store.clone());
        new_form(&mut h, name);
        h.state_mut().ui.connection.session_token = token.into();
        settle(&mut h);
        h.get_by_label("Remember on this computer").click();
        settle(&mut h);
        assert!(!h.state().ui.connection.remember);
        h.get_by_role_and_label(Role::Button, "Save").click();
        settle(&mut h);
        let c = h.state().session.connections.find(name).unwrap();
        assert!(!c.saved_keys);
        if let Some(store) = store {
            assert!(store.load(&format!("connection/{}", c.id)).unwrap().is_none());
        }
        assert!(s3::connection(name).is_some());
        s3::disconnect(name);
    }
}

#[test]
fn s3_saved_refused_forget_keeps_marker_session_and_error() {
    let mut h = with_store(Some(Arc::new(MemorySecrets::new())));
    let name = "s3-saved-widget-forget-denied";
    new_form(&mut h, name);
    h.get_by_label("Remember on this computer").click();
    settle(&mut h);
    h.get_by_role_and_label(Role::Button, "Save").click();
    settle(&mut h);
    let id = h.state().session.connections.find(name).unwrap().id.clone();
    h.state_mut().session.database_logins.set_store(Some(Arc::new(MemorySecrets::failing(SecretError::Denied))));
    h.state_mut().run("ui.connection", json!({"id":id})).unwrap();
    settle(&mut h);
    h.get_by_role_and_label(Role::Button, "Forget Saved Keys").click();
    settle(&mut h);
    assert_eq!(h.state().dialogs.open, Some("editConnection"));
    assert!(h.state().dialogs.error.is_some());
    assert!(h.state().session.connections.find(&id).unwrap().saved_keys);
    assert!(s3::connection(name).is_some());
    s3::disconnect(name);
}

#[test]
fn s3_saved_connect_warning_leaves_session_browser_usable() {
    let mut h = with_store(Some(Arc::new(MemorySecrets::failing(SecretError::Denied))));
    h.state_mut().run("ui.dialog", json!({"name":"s3"})).unwrap();
    settle(&mut h);
    expand_connection(&mut h);
    type_field(&mut h, "Name", "s3-saved-widget-connect-warning");
    type_field(&mut h, "Endpoint", "http://127.0.0.1:1");
    type_field(&mut h, "Access key ID", "CONNECTWARNINGKEY");
    type_field(&mut h, "Secret access key", "connect-warning-secret");
    h.get_by_label("Remember on this computer").click();
    settle(&mut h);
    h.get_by_role_and_label(Role::Button, "Connect").click();
    settle(&mut h);
    assert_eq!(h.state().dialogs.open, Some("s3"));
    h.get_by_label("Logged in for this session");
    h.get_by_role_and_label(Role::Button, "My Buckets");
    let c = h.state().session.connections.find("s3-saved-widget-connect-warning").unwrap();
    assert!(!c.saved_keys);
    assert!(s3::connection(&c.name).is_some());
    s3::disconnect(&c.name);
}
