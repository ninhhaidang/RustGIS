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
    assert!(!h.get_by_role_and_label(Role::Button, "Test").accesskit_node().is_disabled());
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
fn service_http_test_and_save_need_confirmation() {
    let mut h = session_ui();
    header_form(&mut h, "http://insecure-web-widget.example/wms");
    assert!(!h.get_by_role_and_label(Role::Button, "Test").accesskit_node().is_disabled());
    assert!(h.state().session.connections.connections.is_empty());
    h.get_by_role_and_label(Role::Button, "Test").click();
    settle(&mut h);
    assert!(h.state().ui.connection.test.is_none());
    assert!(h.state().dialogs.error.as_deref().unwrap().contains("unencrypted"));
    assert_eq!(h.state().ui.connection.api_key, "web-widget-secret-sentinel", "unsent credentials stay editable");
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

/// One delayed response lets the UI edit or cancel while authenticated I/O is in flight.
fn probe_server() -> (String, std::sync::mpsc::Receiver<String>, std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>) {
    probe_server_with_answer(None)
}

fn probe_server_with_answer(
    answer: Option<&str>,
) -> (String, std::sync::mpsc::Receiver<String>, std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>) {
    let answer = answer.map(str::to_owned);
    use std::io::{Read, Write};
    use std::time::{Duration, Instant};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}/wmts", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    let (sent, requests) = std::sync::mpsc::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false).unwrap();
                    break stream;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => panic!("the Test did not connect: {e}"),
            }
        };
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            let mut buf = [0; 1024];
            let n = stream.read(&mut buf).unwrap();
            assert!(n > 0);
            request.extend_from_slice(&buf[..n]);
        }
        sent.send(String::from_utf8(request).unwrap()).unwrap();
        wait.recv_timeout(Duration::from_secs(10)).unwrap();
        let body = r#"<Capabilities xmlns="http://www.opengis.net/wmts/1.0" xmlns:ows="http://www.opengis.net/ows/1.1" version="1.0.0"><Contents>
<Layer><ows:Title>Test layer</ows:Title><ows:Identifier>test</ows:Identifier><TileMatrixSetLink><TileMatrixSet>gm</TileMatrixSet></TileMatrixSetLink>
<ResourceURL resourceType="tile" format="image/png" template="https://tiles.example/{TileMatrix}/{TileRow}/{TileCol}.png"/></Layer>
<TileMatrixSet><ows:Identifier>gm</ows:Identifier><ows:SupportedCRS>EPSG:3857</ows:SupportedCRS>
<TileMatrix><ows:Identifier>0</ows:Identifier><ScaleDenominator>559082264.0287178</ScaleDenominator><TopLeftCorner>-20037508.34278925 20037508.34278925</TopLeftCorner><TileWidth>256</TileWidth><TileHeight>256</TileHeight></TileMatrix>
</TileMatrixSet></Contents></Capabilities>"#;
        let title = format!(
            "<ows:ServiceIdentification><ows:Title>Fixture web-widget-secret-sentinel {}</ows:Title></ows:ServiceIdentification><Contents>",
            rustgis_engine::render::rustgis_tiles::logins::basic("reader", "web-widget-secret-sentinel")
        );
        let body = body.replace("<Contents>", &title);
        let body = answer.unwrap_or(body);
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    });
    (address, requests, release, server)
}

fn finish_probe(h: &mut Harness) {
    for _ in 0..250 {
        h.step();
        if h.state().ui.connection.test.is_none() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!("the Test did not finish");
}

#[test]
fn unsaved_wmts_test_uses_typed_header_or_basic_without_touching_saved_login() {
    use rustgis_engine::service_logins as web;
    for basic in [false, true] {
        let (address, requests, release, server) = probe_server();
        let mut h = session_ui();
        let made =
            h.state_mut().run("connection.new", json!({"kind":"wmts","address":address,"auth":"header","keyName":"X-Api-Key"})).unwrap();
        h.state_mut().run("connection.login", json!({"id":made["id"],"apiKey":"existing-saved-login","allowInsecure":true})).unwrap();
        let saved = h.state().session.connections.find(made["id"].as_str().unwrap()).unwrap().clone();
        header_form(&mut h, &address);
        h.state_mut().ui.connection.kind = "wmts".into();
        if basic {
            h.get_by_role_and_label(Role::ComboBox, "Method").click();
            settle(&mut h);
            h.get_by_label("Basic authentication").click();
            settle(&mut h);
            type_field(&mut h, "User", "reader", false);
            type_field(&mut h, "Password", "web-widget-secret-sentinel", true);
        }
        h.get_by_label("Send the login over unencrypted http://").click();
        settle(&mut h);
        h.get_by_role_and_label(Role::Button, "Test").click();
        h.step();
        let request = requests.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        let expected = if basic {
            rustgis_engine::render::rustgis_tiles::logins::basic("reader", "web-widget-secret-sentinel")
        } else {
            "web-widget-secret-sentinel".into()
        };
        assert!(request.contains(&expected), "{request}");
        assert!(request.to_ascii_lowercase().contains(if basic { "authorization:" } else { "x-api-key:" }));
        assert!(!request.contains("existing-saved-login"));
        assert!(request.contains("GetCapabilities"));
        assert!(h.state().ui.connection.test.is_some(), "the temporary login survives the asynchronous request");
        assert_redacted(&h);
        release.send(()).unwrap();
        finish_probe(&mut h);
        assert_eq!(h.state().dialogs.open, Some("newConnection"));
        assert_eq!(h.state().dialogs.error, None);
        assert!(h.state().dialogs.info.as_deref().unwrap().contains("1 layers"));
        assert!(h.state().dialogs.info.as_deref().unwrap().contains("[redacted]"));
        assert!(!h.state().dialogs.info.as_deref().unwrap().contains(&expected));
        assert!(h.state().ui.connection.id.is_none());
        assert_eq!(h.state().session.connections.connections, vec![saved.clone()]);
        assert_eq!(web::known_secret(&h.state().session, &saved).unwrap().expose(), "existing-saved-login");
        for entry in h.state().session.log.entries().filter(|e| e.command == "connection.test") {
            assert!(entry.params.get("id").is_none());
            assert!(entry.params.get("password").is_none());
            assert!(entry.params.get("apiKey").is_none());
            assert!(!entry.params.to_string().contains("web-widget-secret-sentinel"));
        }
        assert_redacted(&h);
        server.join().unwrap();
    }
}

#[test]
fn short_service_answers_do_not_retain_echoed_credentials_in_history() {
    for answer in [
        "web-widget-secret-sentinel",
        "<Exception>web-widget-secret-sentinel</Exception>",
        "<ServiceExceptionReport><ServiceException>Rejected credential web-widget-secret-sentinel</ServiceException></ServiceExceptionReport>",
    ] {
        let (address, requests, release, server) = probe_server_with_answer(Some(answer));
        let mut h = session_ui();
        header_form(&mut h, &address);
        h.get_by_label("Send the login over unencrypted http://").click();
        settle(&mut h);
        h.get_by_role_and_label(Role::Button, "Test").click();
        h.step();
        requests.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        release.send(()).unwrap();
        finish_probe(&mut h);
        let ui_error = h.state().dialogs.error.as_deref().expect("the service response must produce a UI error");
        assert!(!ui_error.contains("web-widget-secret-sentinel"), "the UI error must not retain the credential");
        let entry = h.state().session.log.entries().filter(|e| e.command == "connection.test").last().unwrap();
        assert!(entry.params.get("answer").is_some(), "the response parsing command must actually be recorded");
        assert!(!entry.params.to_string().contains("web-widget-secret-sentinel"), "the response must not retain the credential");
        let logged_error = entry.error.as_deref().expect("the response parsing error must actually be recorded");
        assert!(!logged_error.contains("web-widget-secret-sentinel"), "the command log error must not retain the credential");
        if answer.starts_with("<ServiceExceptionReport>") {
            for error in [logged_error, ui_error] {
                assert!(error.contains("the service answered with an error"), "the service exception must reach the consumer: {error}");
                assert!(error.contains("Rejected credential [redacted]"), "the echoed credential must be redacted in the error: {error}");
            }
        }
        assert_redacted(&h);
        server.join().unwrap();
    }
}

#[test]
fn unsaved_authenticated_test_discards_late_results_after_edit_cancel_or_project_change() {
    for action in ["edit", "cancel", "project"] {
        let (address, requests, release, server) = probe_server();
        let mut h = session_ui();
        header_form(&mut h, &address);
        h.state_mut().ui.connection.kind = "wmts".into();
        h.get_by_label("Send the login over unencrypted http://").click();
        settle(&mut h);
        h.get_by_role_and_label(Role::Button, "Test").click();
        h.step();
        requests.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        let old = h.state().ui.connection.test.as_ref().unwrap().1.clone();
        match action {
            "edit" => type_field(&mut h, "Key", "replacement-form-key", true),
            "cancel" => {
                h.get_by_role_and_label(Role::Button, "Cancel").click();
                settle(&mut h);
            }
            "project" => {
                h.state_mut().run("project.new", json!({})).unwrap();
                settle(&mut h);
            }
            _ => unreachable!(),
        }
        assert!(h.state().ui.connection.test.is_none(), "{action}");
        release.send(()).unwrap();
        for _ in 0..250 {
            h.step();
            if old.lock().unwrap().is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(old.lock().unwrap().is_some(), "the detached request must actually complete");
        assert_eq!(h.state().dialogs.info, None, "{action}");
        assert_eq!(h.state().dialogs.error, None, "{action}");
        assert!(h.state().session.connections.connections.is_empty());
        server.join().unwrap();
    }
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
