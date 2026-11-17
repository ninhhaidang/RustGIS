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

mod arcgis_token {
    use super::*;
    use rustgis_engine::render::rustgis_tiles::arcgis_token::{self, Credential, Done};
    use rustgis_engine::render::rustgis_tiles::cache::Still;
    use rustgis_engine::render::rustgis_tiles::logins;
    use std::collections::BTreeMap;
    use std::sync::{LazyLock, Mutex, Once};

    const TOKEN: &str = "pasted-arcgis-widget-token-sentinel";
    const PASSWORD: &str = "generate-widget-password-sentinel";
    const FUTURE: i64 = 4_102_444_800_000;

    type Handler = Arc<dyn Fn(String, Still, Done) + Send + Sync>;
    static HANDLERS: LazyLock<Mutex<BTreeMap<String, Handler>>> = LazyLock::new(Mutex::default);

    /// A token server on this computer that holds each request until the test answers it. Each
    /// test has its own endpoint, so none shares another's requests.
    #[derive(Clone, Default)]
    struct Server {
        forms: Arc<Mutex<Vec<String>>>,
        held: Arc<Mutex<Vec<(Done, Still)>>>,
    }

    impl Server {
        fn at(endpoint: &str) -> Server {
            static ONCE: Once = Once::new();
            ONCE.call_once(|| {
                arcgis_token::set_generator(|endpoint, body, still, done| {
                    let handler = HANDLERS.lock().unwrap().get(&endpoint).cloned();
                    match handler {
                        Some(handler) => handler(body.expose().to_string(), still, done),
                        None => done(Err("no fake answers this endpoint".into())),
                    }
                });
            });
            let server = Server::default();
            let state = server.clone();
            let handler: Handler = Arc::new(move |form, still, done| {
                state.forms.lock().unwrap().push(form);
                state.held.lock().unwrap().push((done, still));
            });
            HANDLERS.lock().unwrap().insert(endpoint.to_string(), handler);
            server
        }

        fn forms(&self) -> Vec<String> {
            self.forms.lock().unwrap().clone()
        }

        fn take(&self) -> (Done, Still) {
            self.held.lock().unwrap().remove(0)
        }
    }

    fn form(h: &mut Harness, address: &str) {
        h.state_mut().run("ui.connection", json!({"kind":"arcgis"})).unwrap();
        settle(h);
        type_field(h, "Name", "ArcGIS token login", false);
        type_field(h, "Address", address, false);
        h.get_by_role_and_label(Role::ComboBox, "Method").click();
        settle(h);
        h.get_by_label("ArcGIS token").click();
        settle(h);
        type_field(h, "User", "alice", false);
    }

    fn generate_mode(h: &mut Harness, endpoint: &str) {
        h.get_by_label("Username and password").click();
        settle(h);
        type_field(h, "Token endpoint", endpoint, false);
        type_field(h, "Password", PASSWORD, true);
    }

    fn assert_clean(h: &Harness) {
        let app = h.state();
        let st = &app.ui.connection;
        assert!(st.password.is_empty() && st.token.is_empty() && st.token_expires.is_empty() && !st.remember);
        let visible = format!(
            "{}\n{:?}\n{}\n{}\n{}",
            app.session.connections.to_json(),
            app.session.notices,
            app.dialogs.error.as_deref().unwrap_or(""),
            app.dialogs.info.as_deref().unwrap_or(""),
            st.warned.as_deref().unwrap_or("")
        );
        for private in [TOKEN, PASSWORD, "rustgis-login"] {
            assert!(!visible.contains(private), "{private} is visible: {visible}");
        }
        for entry in app.session.log.entries().filter(|entry| entry.command.starts_with("connection.")) {
            let params = entry.params.to_string();
            assert!(!params.contains(TOKEN) && !params.contains(PASSWORD), "{} kept a secret: {params}", entry.command);
        }
    }

    #[test]
    fn arcgis_token_form_pastes_a_masked_token_and_remembers_only_when_asked() {
        let store = Arc::new(MemorySecrets::new());
        let mut h = with_store(Some(store.clone()));
        form(&mut h, "https://widget-arcgis-paste.example/arcgis/rest/services");
        // Paste is the way in; a token and its expiry are masked and plain; Remember is an opt-in.
        h.get_by_label("Paste token");
        assert!(!h.state().ui.connection.token_generate && !h.state().ui.connection.remember);
        assert!(h.query_by_label("Token endpoint").is_none());
        type_field(&mut h, "Token", TOKEN, true);
        type_field(&mut h, "Expires at (Unix milliseconds)", "soon", false);
        save(&mut h);
        // An expiry that isn't a time is said, not dropped; nothing is saved.
        assert!(h.state().dialogs.error.as_deref().unwrap().contains("Unix time in milliseconds"));
        assert!(h.state().session.connections.find("ArcGIS token login").is_none());
        h.state_mut().ui.connection.token_expires = FUTURE.to_string();
        settle(&mut h);
        h.get_by_label("Remember on this computer").click();
        settle(&mut h);
        save(&mut h);
        assert_eq!(h.state().dialogs.open, None);
        let c = h.state().session.connections.find("ArcGIS token login").unwrap().clone();
        assert_eq!((c.auth.as_str(), c.user.as_str(), c.remembered), ("arcgis", "alice", true));
        assert!(web::logged_in(&h.state().session, &c));
        let saved = store.load(&format!("connection/{}", c.id)).unwrap().unwrap();
        assert!(saved.expose().contains(TOKEN) && saved.expose().contains(&FUTURE.to_string()));
        assert_clean(&h);
    }

    #[test]
    fn arcgis_token_invalid_replacement_keeps_the_remembered_definition_and_login() {
        if crate::in_own_process(
            "remembered_service::arcgis_token::arcgis_token_invalid_replacement_keeps_the_remembered_definition_and_login",
        ) {
            return;
        }
        let store = Arc::new(MemorySecrets::new());
        let mut h = with_store(Some(store.clone()));
        let id = h
            .state_mut()
            .run(
                "connection.new",
                json!({
                    "kind":"arcgis", "name":"Existing ArcGIS login",
                    "address":"https://widget-arcgis-presave.example/arcgis/rest/services",
                    "auth":"arcgis", "user":"alice"
                }),
            )
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        h.state_mut().run("connection.login", json!({"id":id, "token":TOKEN, "remember":true})).unwrap();
        let c = h.state().session.connections.find(&id).unwrap().clone();
        let definition = h.state().session.connections.to_json();
        let account = format!("connection/{id}");
        let saved = store.load(&account).unwrap().unwrap().expose().to_string();
        let bound = web::request(&mut h.state_mut().session, Some(&id), &c.address).unwrap();

        for (generate, password) in [(false, String::new()), (true, "x".repeat(logins::MAX_VALUE + 1)), (true, "invalid\npassword".into())]
        {
            h.state_mut().run("ui.connection", json!({"id":id})).unwrap();
            {
                let st = &mut h.state_mut().ui.connection;
                // Either edit changes login identity and would discard the old remembered login.
                st.user = "bob".into();
                st.address = "https://widget-arcgis-presave-changed.example/arcgis/rest/services".into();
                st.token_generate = generate;
                if generate {
                    st.password = password;
                    st.token_endpoint = "https://widget-arcgis-presave-changed.example/arcgis/tokens/generateToken".into();
                } else {
                    st.token = "replacement-widget-token".into();
                    st.token_expires = "1".into(); // Positive, but already expired.
                }
            }
            settle(&mut h);
            save(&mut h);
            assert_eq!(
                h.state().dialogs.error.as_deref(),
                Some(if generate { rustgis_engine::service_tokens::BAD_PASSWORD } else { arcgis_token::EXPIRED })
            );
            assert_eq!(h.state().dialogs.open, Some("editConnection"));
            assert!(h.state().ui.connection.generating.is_none());
            assert_eq!(h.state().session.connections.to_json(), definition);
            assert_eq!(store.load(&account).unwrap().unwrap().expose(), saved);
            assert!(web::logged_in(&h.state().session, &c));
            assert_eq!(logins::check(&bound), Ok(()), "the original active binding survives");
        }
    }

    #[test]
    fn arcgis_token_form_generates_in_the_background_and_never_holds_the_password() {
        if crate::in_own_process(
            "remembered_service::arcgis_token::arcgis_token_form_generates_in_the_background_and_never_holds_the_password",
        ) {
            return;
        }
        let endpoint = "https://widget-arcgis-generate.example/arcgis/tokens/generateToken";
        let server = Server::at(endpoint);
        let store = Arc::new(MemorySecrets::new());
        let mut h = with_store(Some(store.clone()));
        form(&mut h, "https://widget-arcgis-generate.example/arcgis/rest/services");
        generate_mode(&mut h, endpoint);
        assert!(h.query_by_label("Expires at (Unix milliseconds)").is_none(), "pasting is another mode");
        save(&mut h);
        // The form stays to say how it goes; the password left it with the request.
        assert!(h.state().dialogs.open.is_some());
        assert!(h.state().ui.connection.generating.is_some());
        assert!(h.state().ui.connection.password.is_empty());
        h.get_by_role_and_label(Role::Button, "Cancel token request");
        assert_eq!(server.forms(), [format!("f=json&username=alice&password={PASSWORD}&client=requestip&expiration=60")]);
        // No second Save while it is under way.
        assert!(h.get_by_role_and_label(Role::Button, "Save").accesskit_node().is_disabled());
        assert_eq!(server.forms().len(), 1);
        let c = h.state().session.connections.find("ArcGIS token login").unwrap().clone();
        assert!(!web::logged_in(&h.state().session, &c));

        let (done, _) = server.take();
        done(Ok(Credential::new("generated-widget-token", Some(FUTURE)).ok().unwrap()));
        settle(&mut h);
        assert_eq!(h.state().dialogs.open, None);
        assert!(h.state().ui.connection.generating.is_none());
        assert!(web::logged_in(&h.state().session, &c));
        // Remember was off: only this session has the token.
        assert_eq!(store.load(&format!("connection/{}", c.id)).unwrap(), None);
        assert!(!h.state().session.connections.find(&c.id).unwrap().remembered);
        assert_clean(&h);
    }

    #[test]
    fn arcgis_token_form_says_what_is_missing_before_saving_or_sending() {
        let endpoint = "https://widget-arcgis-missing.example/arcgis/tokens/generateToken";
        let server = Server::at(endpoint);
        let mut h = with_store(Some(Arc::new(MemorySecrets::new())));
        form(&mut h, "https://widget-arcgis-missing.example/arcgis/rest/services");
        h.get_by_label("Username and password").click();
        settle(&mut h);
        type_field(&mut h, "Password", PASSWORD, true);
        save(&mut h);
        assert!(h.state().dialogs.error.as_deref().unwrap().contains("token endpoint"));
        type_field(&mut h, "Token endpoint", "http://widget-arcgis-missing.example/arcgis/tokens/generateToken", false);
        save(&mut h);
        assert!(h.state().dialogs.error.as_deref().unwrap().contains("https://"), "{:?}", h.state().dialogs.error);
        h.state_mut().ui.connection.token_endpoint = endpoint.into();
        h.state_mut().ui.connection.user.clear();
        settle(&mut h);
        save(&mut h);
        assert!(h.state().dialogs.error.as_deref().unwrap().contains("user"));
        assert!(h.state().session.connections.find("ArcGIS token login").is_none(), "nothing was saved");
        assert!(server.forms().is_empty(), "nothing was sent");
        // A Test of a token made from a password belongs to a saved connection.
        h.state_mut().ui.connection.user = "alice".into();
        settle(&mut h);
        h.get_by_role_and_label(Role::Button, "Test").click();
        settle(&mut h);
        assert!(h.state().dialogs.error.as_deref().unwrap().contains("Save the connection"));
        assert!(h.state().ui.connection.password.is_empty());
        assert!(server.forms().is_empty());
    }

    #[test]
    fn arcgis_token_test_with_an_expiry_but_no_token_says_so_instead_of_using_the_saved_login() {
        let mut h = with_store(Some(Arc::new(MemorySecrets::new())));
        let id = h
            .state_mut()
            .run(
                "connection.new",
                json!({"kind":"arcgis","name":"Orphan expiry","address":"https://widget-arcgis-orphan.example/arcgis/rest/services","auth":"arcgis","user":"alice"}),
            )
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        h.state_mut().run("connection.login", json!({"id":id,"token":TOKEN})).unwrap();
        h.state_mut().run("ui.connection", json!({"id":id})).unwrap();
        settle(&mut h);
        h.state_mut().ui.connection.token_expires = FUTURE.to_string();
        settle(&mut h);
        h.get_by_role_and_label(Role::Button, "Test").click();
        settle(&mut h);
        assert!(h.state().dialogs.error.as_deref().unwrap().contains("this expiry is for"), "{:?}", h.state().dialogs.error);
        assert!(h.state().ui.connection.test.is_none(), "no test ran with the saved login");
        assert!(h.state().ui.connection.token_expires.is_empty());
    }

    #[test]
    fn arcgis_token_form_cancels_its_request_and_closing_it_does_too() {
        if crate::in_own_process("remembered_service::arcgis_token::arcgis_token_form_cancels_its_request_and_closing_it_does_too") {
            return;
        }
        let endpoint = "https://widget-arcgis-cancel.example/arcgis/tokens/generateToken";
        let server = Server::at(endpoint);
        let mut h = with_store(Some(Arc::new(MemorySecrets::new())));
        form(&mut h, "https://widget-arcgis-cancel.example/arcgis/rest/services");
        generate_mode(&mut h, endpoint);
        save(&mut h);
        let c = h.state().session.connections.find("ArcGIS token login").unwrap().clone();
        let (done, still) = server.take();
        assert!(still());
        h.get_by_role_and_label(Role::Button, "Cancel token request").click();
        settle(&mut h);
        assert!(!still(), "Cancel stops the request");
        assert!(h.state().ui.connection.generating.is_none());
        assert!(h.state().dialogs.open.is_some(), "the form stays to try again");
        // What the server then says lands nowhere.
        done(Ok(Credential::new("late-widget-token", Some(FUTURE)).ok().unwrap()));
        settle(&mut h);
        assert!(!web::logged_in(&h.state().session, &c));

        // Closing the form stops a request too, and leaves an earlier login alone.
        h.state_mut().run("connection.login", json!({"id":c.id,"token":"earlier-widget-token"})).unwrap();
        h.state_mut().ui.connection.password = PASSWORD.into();
        h.state_mut().ui.connection.token_generate = true;
        settle(&mut h);
        save(&mut h);
        let (_, still) = server.take();
        assert!(still());
        h.get_by_role_and_label(Role::Button, "Cancel").click();
        settle(&mut h);
        assert!(!still(), "closing the form stops the request");
        assert_eq!(h.state().dialogs.open, None);
        assert!(web::logged_in(&h.state().session, &c), "the earlier login stays");
        assert_clean(&h);
    }
}
