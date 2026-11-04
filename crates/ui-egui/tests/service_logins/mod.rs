use super::*;
use egui::accesskit::Role;
use egui_kittest::kittest::NodeT;
use rustgis_engine::connections::{LoginReason, LoginRequest, WEB};

type Harness = egui_kittest::Harness<'static, RustGisApp>;

fn session_ui() -> Harness {
    with_app(RustGisApp::with_sample())
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
fn header_form(h: &mut Harness, address: &str) {
    h.state_mut().run("ui.connection", json!({"kind":"wms"})).unwrap();
    settle(h);
    type_field(h, "Name", "Saved web login", false);
    type_field(h, "Address", address, false);
    h.get_by_role_and_label(Role::ComboBox, "Method").click();
    settle(h);
    h.get_by_label("API key in a header").click();
    settle(h);
    type_field(h, "Header name", "X-Api-Key", false);
    type_field(h, "Key", "web-widget-secret-sentinel", true);
    assert!(!h.state().ui.connection.remember);
}
fn assert_redacted(h: &Harness) {
    let app = h.state();
    let secret = "web-widget-secret-sentinel";
    assert!(!app.session.connections.to_json().to_string().contains(secret));
    assert!(!app.session.notices.iter().any(|n| n.text.contains(secret)));
    assert!(app.ui.connection.api_key.is_empty());
    assert!(app.ui.connection.password.is_empty());
    assert!(!app.ui.connection.remember);
    assert!(!app.ui.connection.allow_insecure);
    assert!(!app.dialogs.error.as_deref().unwrap_or("").contains(secret));
    assert!(!app.dialogs.info.as_deref().unwrap_or("").contains(secret));
    assert!(!app.ui.connection.warned.as_deref().unwrap_or("").contains(secret));
}

#[test]
fn service_key_save_redaction_status_and_logout() {
    let mut h = session_ui();
    header_form(&mut h, "https://saved-web-widget.example/wms");
    assert!(h.query_by_label("Remember on this computer").is_none());
    h.get_by_role_and_label(Role::Button, "Save").click();
    settle(&mut h);
    assert_eq!(h.state().dialogs.open, None);
    let c = h.state().session.connections.find("Saved web login").unwrap().clone();
    assert!(!c.remembered);
    assert_redacted(&h);
    h.state_mut().run("ui.connection", json!({"id":c.id})).unwrap();
    settle(&mut h);
    h.get_by_label("Logged in");
    assert!(h.query_by_label("Saved on this computer").is_none());
    assert!(h.query_by_label("Forget Saved Login").is_none());
    assert!(rustgis_engine::service_logins::logged_in(&h.state().session, &c));
    h.get_by_role_and_label(Role::Button, "Log Out").click();
    settle(&mut h);
    assert!(!rustgis_engine::service_logins::logged_in(&h.state().session, &c));
}

#[test]
fn basic_login_is_masked_and_session_only_even_with_a_credential_store() {
    use rustgis_engine::data::secrets::{MemorySecrets, SecretError};
    let app = RustGisApp::with_sample().with_services(rustgis_ui_egui::Services {
        secrets: Some(std::sync::Arc::new(MemorySecrets::failing(SecretError::Denied))),
        ..Default::default()
    });
    let mut h = with_app(app);
    h.state_mut().run("ui.connection", json!({"kind":"wms"})).unwrap();
    settle(&mut h);
    assert!(!h.get_by_role_and_label(Role::Button, "Test").accesskit_node().is_disabled(), "new anonymous forms can test");
    type_field(&mut h, "Name", "Session basic login", false);
    type_field(&mut h, "Address", "https://basic-web-widget.example/wms", false);
    h.get_by_role_and_label(Role::ComboBox, "Method").click();
    settle(&mut h);
    h.get_by_label("Basic authentication").click();
    settle(&mut h);
    type_field(&mut h, "User", "reader", false);
    type_field(&mut h, "Password", "web-widget-secret-sentinel", true);
    assert!(h.query_by_label("Remember on this computer").is_none());
    assert!(h.get_by_role_and_label(Role::Button, "Test").accesskit_node().is_disabled());
    h.get_by_role_and_label(Role::Button, "Save").click();
    settle(&mut h);
    assert_eq!(h.state().dialogs.open, None);
    let c = h.state().session.connections.find("Session basic login").unwrap();
    assert!(!c.remembered);
    assert!(rustgis_engine::service_logins::logged_in(&h.state().session, c));
    assert_redacted(&h);
}

#[test]
fn service_secret_validation_precedes_saving_the_definition() {
    let mut h = session_ui();
    header_form(&mut h, "https://refused-web-widget.example/wms");
    let invalid = "web-widget-secret-sentinel".repeat(200);
    h.state_mut().ui.connection.api_key = invalid.clone();
    h.get_by_role_and_label(Role::Button, "Save").click();
    settle(&mut h);
    assert_eq!(h.state().dialogs.open, Some("newConnection"));
    assert!(h.state().dialogs.error.is_some());
    assert!(h.state().ui.connection.id.is_none());
    assert!(h.state().session.connections.connections.is_empty());
    assert_eq!(h.state().ui.connection.api_key, invalid, "unsent input remains editable");
}

#[test]
fn service_http_needs_confirmation_and_unsaved_login_cannot_be_tested() {
    let mut h = session_ui();
    header_form(&mut h, "http://insecure-web-widget.example/wms");
    assert!(h.get_by_role_and_label(Role::Button, "Test").accesskit_node().is_disabled());
    assert!(h.state().session.connections.connections.is_empty());
    h.get_by_role_and_label(Role::Button, "Save").click();
    settle(&mut h);
    assert!(h.state().session.connections.connections.is_empty());
    assert!(h.state().dialogs.error.as_deref().unwrap().contains("unencrypted"));
    h.get_by_label("Send the login over unencrypted http://").click();
    settle(&mut h);
    h.get_by_role_and_label(Role::Button, "Save").click();
    settle(&mut h);
    assert_eq!(h.state().dialogs.open, None);
    assert!(!h.state().session.connections.find("Saved web login").unwrap().remembered);
    assert_redacted(&h);
}

#[test]
fn edited_service_test_button_is_disabled_and_retains_the_unsent_key() {
    let mut h = session_ui();
    let c = h
        .state_mut()
        .run(
            "connection.new",
            json!({
                "kind":"wms", "address":"https://unchanged-widget.example/wms", "auth":"header", "keyName":"X-Api-Key"
            }),
        )
        .unwrap();
    h.state_mut().run("ui.connection", json!({"id":c["id"]})).unwrap();
    settle(&mut h);
    assert!(!h.get_by_role_and_label(Role::Button, "Test").accesskit_node().is_disabled());
    h.state_mut().ui.connection.address = "https://edited-widget.example/wms".into();
    h.state_mut().ui.connection.api_key = "unsent-widget-key".into();
    settle(&mut h);
    assert!(h.get_by_role_and_label(Role::Button, "Test").accesskit_node().is_disabled());
    assert_eq!(h.state().ui.connection.api_key, "unsent-widget-key");
    assert!(h.state().ui.connection.test.is_none());
    assert!(!h.state().session.database_logins.has_session_password(c["id"].as_str().unwrap()));
}

#[test]
fn a_typed_address_secret_blocks_testing_before_the_engine_refuses() {
    let mut h = session_ui();
    let c = h.state_mut().run("connection.new", json!({"kind":"wms", "address":"https://typed-widget.example/wms"})).unwrap();
    h.state_mut().run("ui.connection", json!({"id":c["id"]})).unwrap();
    settle(&mut h);
    assert!(!h.get_by_role_and_label(Role::Button, "Test").accesskit_node().is_disabled());
    h.state_mut().ui.connection.address.push_str("?token=abc");
    settle(&mut h);
    assert!(h.get_by_role_and_label(Role::Button, "Test").accesskit_node().is_disabled());
}

fn request(h: &mut Harness, reason: LoginReason, key: &str, connections: Vec<String>) {
    let opened = h.state().session.opened;
    h.state_mut().session.database_logins.request(LoginRequest {
        reason,
        kind: WEB,
        key: key.into(),
        connections,
        layers: Vec::new(),
        tiles: 1,
        opened,
        prompted: false,
    });
    settle(h);
}

#[test]
fn service_missing_connection_prompt_prefills_origin_and_cancel_stays_closed() {
    let mut h = session_ui();
    let origin = "https://missing-web-widget.example";
    request(&mut h, LoginReason::Connection, origin, Vec::new());
    assert_eq!(h.state().dialogs.open, Some("chooseConnection"));
    h.get_by_label("New Connection…").click();
    settle(&mut h);
    assert_eq!(h.state().dialogs.open, Some("newConnection"));
    assert_eq!(h.state().ui.connection.kind, "service");
    assert_eq!(h.state().ui.connection.address, origin);
    h.get_by_role_and_label(Role::Button, "Cancel").click();
    settle(&mut h);
    assert_eq!(h.state().dialogs.open, None);
}

#[test]
fn a_connection_made_for_a_missing_one_is_taken_by_the_layers_waiting_for_it() {
    let mut h = session_ui();
    let origin = "https://adopt-web-widget.example";
    h.state_mut().run("map.addTiles", json!({"url":format!("{origin}/tiles/{{z}}/{{x}}/{{y}}.png")})).unwrap();
    h.state_mut().session.map_mut().tiles[0].connection = "deleted-connection".into();
    request(&mut h, LoginReason::Connection, origin, Vec::new());
    h.get_by_label("New Connection…").click();
    settle(&mut h);
    type_field(&mut h, "Name", "Replacement", false);
    h.get_by_role_and_label(Role::Button, "Save").click();
    settle(&mut h);
    assert_eq!(h.state().dialogs.open, None);
    let id = h.state().session.connections.find("Replacement").unwrap().id.clone();
    assert_eq!(h.state().session.map().tiles[0].connection, id);
}

#[test]
fn service_chooser_accepts_auth_none_and_password_request_opens_login() {
    let mut h = session_ui();
    let origin = "https://choose-web-widget.example";
    let public = h.state_mut().run("connection.new", json!({"kind":"wms","name":"Public web service","address":origin})).unwrap();
    let private = h
        .state_mut()
        .run("connection.new", json!({"kind":"wms","name":"Private web service","address":origin,"auth":"basic","user":"reader"}))
        .unwrap();
    let public = public["id"].as_str().unwrap().to_string();
    let private = private["id"].as_str().unwrap().to_string();
    request(&mut h, LoginReason::Choose, origin, vec![public.clone(), private.clone()]);
    assert_eq!(h.state().dialogs.open, Some("chooseConnection"));
    h.query_all_by_label("Public web service").next().unwrap().click();
    settle(&mut h);
    h.get_by_role_and_label(Role::Button, "Use Connection").click();
    settle(&mut h);
    assert_eq!(h.state().dialogs.open, None);
    assert_eq!(h.state().session.database_logins.selection(WEB, origin), Some(public.as_str()));
    request(&mut h, LoginReason::Password, origin, vec![private.clone()]);
    assert_eq!(h.state().dialogs.open, Some("editConnection"));
    assert!(h.state().ui.connection.login);
    assert_eq!(h.state().ui.connection.id.as_deref(), Some(private.as_str()));
    h.get_by_role_and_label(Role::PasswordInput, "Password");
    assert!(h.query_by_label("Remember on this computer").is_none());
}
