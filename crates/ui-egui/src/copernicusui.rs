//! Web Data › Copernicus Data Space: signs in to the Copernicus Data Space Ecosystem (CDSE) with an
//! account's username and password (`copernicus.login`), so the Copernicus Land Monitoring
//! Service's COGs and the Sentinel mosaics it publishes can be read in place. The sign-in is kept
//! for the session, or remembered on this computer (`remember`); Sign out runs
//! `copernicus.logout`. Like the Earthdata Login section of NASA Earthaccess, the password is
//! never kept in the pane: it is handed to the command and emptied. In the web app, where a
//! command can't wait for the browser's fetch, Sign in hands it to `Services::copernicus_sign_in`
//! instead, and the sign-in lasts at most as long as the page (a refused renewal ends it sooner).

use rustgis_engine::render::rustgis_tiles::{copernicus, earthdata};
use serde_json::{Value, json};

use crate::RustGisApp;

/// What is being typed, and what the last sign-in or sign-out said.
#[derive(Default)]
pub struct State {
    pub username: String,
    /// Emptied once it is handed to `copernicus.login`, whatever the answer.
    pub password: String,
    /// The one-time code, for an account with two-factor authentication.
    pub totp: String,
    /// Remember the sign-in on this computer (off unless ticked, or one is remembered already).
    pub remember: bool,
    /// Whether a sign-in was remembered at the last frame, so `remember` follows a change.
    was_remembered: bool,
    pub error: Option<String>,
    pub warning: Option<String>,
    /// A sign-in sent by `Services::copernicus_sign_in` (the browser), until it answers.
    pub pending: Option<crate::Slot<Result<String, String>>>,
}

/// The Copernicus Data Space pane.
pub fn pane(app: &mut RustGisApp, ui: &mut egui::Ui) {
    let t = crate::theme::Tokens::get(ui.ctx());
    ui.label(egui::RichText::new(tl!("The Copernicus Data Space Ecosystem publishes the Copernicus Land Monitoring Service's global products (NDVI, LAI, Burnt Area, Water Bodies, Land Cover…) and the Sentinel-1 and Sentinel-2 mosaics as COGs. Its STAC catalog is open, but reading its files needs a sign-in with a free account.")).weak());
    let done = app.ui.copernicus.pending.as_ref().and_then(|slot| slot.lock().unwrap_or_else(|e| e.into_inner()).take());
    if let Some(r) = done {
        app.ui.copernicus.pending = None;
        answered(app, r.map(|_| Value::Null));
    }
    let sending = app.ui.copernicus.pending.is_some();
    // `None` while a sign-in with the remembered account is under way (it may wait for its
    // request): nothing here waits for it.
    let remembered = copernicus::is_remembered_now();
    if let Some(on) = remembered
        && on != app.ui.copernicus.was_remembered
    {
        // The box follows whether a sign-in is remembered (read back, saved or forgotten).
        app.ui.copernicus.was_remembered = on;
        app.ui.copernicus.remember = on;
    }
    let busy = remembered.is_none() || sending;
    if busy {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
    }
    match copernicus::signed_in() {
        Some(info) => signed_in(app, ui, &info, remembered),
        None => {
            let line = if sending {
                tl!("Signing in…")
            } else if busy {
                tl!("Signing in with the account remembered on this computer…")
            } else if copernicus::expired() {
                tl!("The sign-in expired. Sign in again.")
            } else {
                tl!("Not signed in.")
            };
            ui.label(egui::RichText::new(line).strong());
            // Kept when its host couldn't be reached: it can still be deleted from here.
            if remembered == Some(true) {
                ui.label(egui::RichText::new(tl!("A sign-in is remembered on this computer: it signs in when a file needs it.")).small());
                sign_out(app, ui, true);
            }
            form(app, ui, busy);
        }
    }
    let st = &app.ui.copernicus;
    if let Some(e) = &st.error {
        ui.label(egui::RichText::new(e).color(t.red));
    }
    if let Some(w) = &st.warning {
        ui.label(egui::RichText::new(w).small());
    }
    ui.hyperlink_to(tl!("Create a free account"), copernicus::SIGN_UP);
}

/// The sign-in kept: as whom, its token, whether it is remembered, and Sign out.
fn signed_in(app: &mut RustGisApp, ui: &mut egui::Ui, info: &copernicus::Info, remembered: Option<bool>) {
    ui.label(egui::RichText::new(crate::i18n::fmt("Signed in as {name}.", &[("name", &info.username)])).strong());
    let token = match info.renews_until {
        Some(until) => crate::i18n::fmt("The token is renewed until {date}.", &[("date", &earthdata::date_of(until))]),
        // A refresh token whose lifetime wasn't said renews it until CDSE refuses it.
        None if info.renews => tl!("The token is renewed until you sign out.").to_string(),
        None => crate::i18n::fmt("The token expires on {date}.", &[("date", &earthdata::date_of(info.expires_at))]),
    };
    ui.label(egui::RichText::new(token).small());
    if remembered == Some(true) {
        ui.label(egui::RichText::new(tl!("It is remembered on this computer.")).small());
    }
    sign_out(app, ui, remembered.is_some());
}

/// The Sign out button (`copernicus.logout`), off while the remembered sign-in is under way.
fn sign_out(app: &mut RustGisApp, ui: &mut egui::Ui, enabled: bool) {
    if ui
        .add_enabled(enabled, egui::Button::new(tl!("Sign out")))
        .on_hover_text(tl!("Forgets the sign-in, deletes the one remembered on this computer and empties the download cache."))
        .clicked()
    {
        let r = app.run("copernicus.logout", Value::Null);
        answered(app, r);
    }
}

/// Username, password, one-time code, Remember and Sign in. The fields are off while a sign-in is
/// under way, so nothing is typed into them that its answer would leave behind.
fn form(app: &mut RustGisApp, ui: &mut egui::Ui, busy: bool) {
    let w = ui.available_width();
    let mut enter = false;
    ui.add_enabled_ui(!busy, |ui| {
        egui::Grid::new("cdse_sign_in").num_columns(2).spacing(egui::vec2(10.0, 6.0)).show(ui, |ui| {
            let st = &mut app.ui.copernicus;
            let field = (w - 130.0).max(120.0);
            let label = ui.label(tl!("Username"));
            ui.add(egui::TextEdit::singleline(&mut st.username).hint_text(tl!("Your account's email address")).desired_width(field))
                .labelled_by(label.id);
            ui.end_row();
            let label = ui.label(tl!("Password"));
            let r = ui.add(egui::TextEdit::singleline(&mut st.password).password(true).desired_width(field)).labelled_by(label.id);
            enter |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.end_row();
            let label = ui.label(tl!("One-time code"));
            let r = ui
                .add(egui::TextEdit::singleline(&mut st.totp).hint_text(tl!("Only with two-factor authentication")).desired_width(field))
                .labelled_by(label.id);
            enter |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.end_row();
        });
    });
    if can_remember(app) {
        ui.checkbox(&mut app.ui.copernicus.remember, tl!("Remember on this computer")).on_hover_text(tl!(
            "Keeps the username and password in the system's credential store, so RustGIS signs in by itself when it starts again. Not for an account with two-factor authentication. Unticked, a sign-in remembered before is deleted when you sign in."
        ));
    }
    let st = &app.ui.copernicus;
    let ready = !busy && !st.username.trim().is_empty() && !st.password.is_empty();
    let clicked = ui.add_enabled(ready, egui::Button::new(tl!("Sign in"))).clicked();
    ui.label(egui::RichText::new(tl!("The password is sent only to Copernicus Data Space's sign-in server and never kept in the project. The token it gives is sent only to its data servers.")).small().weak());
    if (clicked || enter) && ready {
        sign_in(app);
    }
}

/// Whether a sign-in can be remembered here: there is a credential store, and the sign-in is the
/// command's (one sent by the browser is never remembered).
fn can_remember(app: &RustGisApp) -> bool {
    app.services.secrets.is_some() && app.services.copernicus_sign_in.is_none()
}

/// Runs `copernicus.login` with what was typed, or hands it to `Services::copernicus_sign_in`
/// where there is one (the browser); the password and the one-time code are emptied whatever it
/// answers.
fn sign_in(app: &mut RustGisApp) {
    let remember = can_remember(app);
    let st = &mut app.ui.copernicus;
    let password = std::mem::take(&mut st.password);
    let totp = std::mem::take(&mut st.totp);
    if let Some(send) = &app.services.copernicus_sign_in {
        let slot = crate::Slot::default();
        send(st.username.trim().to_string(), password, totp.trim().to_string(), slot.clone());
        (st.error, st.warning, st.pending) = (None, None, Some(slot));
        return;
    }
    let mut p = json!({"username": st.username.trim(), "password": password, "remember": st.remember && remember});
    if !totp.trim().is_empty() {
        p["totp"] = json!(totp.trim());
    }
    let two_factor = p.get("totp").is_some();
    let r = app.run("copernicus.login", p);
    // An account with two-factor authentication isn't remembered: the box says so too.
    if two_factor && r.is_ok() {
        app.ui.copernicus.remember = false;
    }
    answered(app, r);
}

/// Shows what a sign-in or sign-out said: its error, or the warning it gave. The password and the
/// one-time code are emptied whatever it says.
fn answered(app: &mut RustGisApp, r: Result<Value, String>) {
    let st = &mut app.ui.copernicus;
    st.password.clear();
    st.totp.clear();
    (st.error, st.warning) = match r {
        Ok(v) => (None, v.get("warning").and_then(Value::as_str).map(str::to_string)),
        Err(e) => (Some(e), None),
    };
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use egui_kittest::kittest::{NodeT, Queryable};
    use rustgis_engine::data::secrets::{MemorySecrets, SecretStore};
    use std::io::{Read, Write};

    /// A fake CDSE token endpoint on 127.0.0.1: a token for the synthetic password, Keycloak's
    /// refusal for any other. Returns its address and how many sign-ins it was asked for.
    fn keycloak() -> std::io::Result<(String, std::sync::Arc<std::sync::atomic::AtomicUsize>)> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = asked.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
                let mut request = Vec::new();
                let mut buf = [0u8; 4096];
                while let Ok(n) = stream.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    request.extend_from_slice(&buf[..n]);
                    let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
                    let head = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                    let length = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if request.len() >= end + 4 + length {
                        break;
                    }
                }
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let request = String::from_utf8_lossy(&request);
                let (status, body) = if request.contains("grant_type=password") && request.contains("password=synthetic-password") {
                    (
                        "200 OK",
                        r#"{"access_token":"synthetic-access","expires_in":1800,"refresh_token":"synthetic-refresh","refresh_expires_in":3600}"#,
                    )
                } else {
                    ("401 Unauthorized", r#"{"error":"invalid_grant","error_description":"Invalid user credentials"}"#)
                };
                let reply = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(reply.as_bytes());
            }
        });
        Ok((format!("http://{addr}/token"), asked))
    }

    fn harness(app: RustGisApp) -> egui_kittest::Harness<'static, RustGisApp> {
        let mut fonts = false;
        egui_kittest::Harness::builder().with_size(egui::vec2(420.0, 520.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                pane(app, ui);
            },
            app,
        )
    }

    fn type_into(h: &mut egui_kittest::Harness<'_, RustGisApp>, field: &str, text: &str) {
        let role = if field == "Password" { egui::accesskit::Role::PasswordInput } else { egui::accesskit::Role::TextInput };
        h.get_by_role_and_label(role, field).focus();
        h.run();
        h.get_by_role_and_label(role, field).type_text(text);
        h.run();
    }

    fn sign_in_on(h: &egui_kittest::Harness<'_, RustGisApp>) -> bool {
        !h.get_by_role_and_label(egui::accesskit::Role::Button, "Sign in").accesskit_node().is_disabled()
    }

    /// The pane signs in with what was typed and empties the password whatever the answer; a
    /// refused sign-in says why and keeps nothing; Remember keeps the username and password in
    /// the credential store, and Sign out deletes them. Only a fake endpoint on 127.0.0.1 is asked.
    #[test]
    fn the_pane_signs_in_remembers_and_signs_out() {
        // The sign-in and its store are process-wide: this scenario runs alone.
        if crate::isolate_account_test("copernicusui::tests::the_pane_signs_in_remembers_and_signs_out") {
            return;
        }
        let (endpoint, asked) = keycloak().unwrap();
        copernicus::set_test_token_url(Some(&endpoint));
        let store = std::sync::Arc::new(MemorySecrets::new());
        let app = RustGisApp::with_sample().with_services(crate::Services { secrets: Some(store.clone()), ..Default::default() });
        let mut h = harness(app);
        h.run();
        h.get_by_label("Not signed in.");
        assert!(!sign_in_on(&h), "nothing typed, Sign in is off");
        type_into(&mut h, "Username", "synthetic.user@example.invalid");
        assert!(!sign_in_on(&h), "no password yet");

        type_into(&mut h, "Password", "not-the-password");
        assert!(sign_in_on(&h));
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Sign in").click();
        h.run();
        let st = &h.state().ui.copernicus;
        assert!(st.error.as_deref().is_some_and(|e| e.contains("Invalid user credentials")), "{:?}", st.error);
        assert!(st.password.is_empty(), "a refused password is emptied too");
        assert_eq!(st.username, "synthetic.user@example.invalid", "the username stays for another try");
        assert_eq!(store.load(copernicus::ACCOUNT).unwrap(), None);
        assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 1);
        h.get_by_label("Not signed in.");

        type_into(&mut h, "Password", "synthetic-password");
        h.get_by_label("Remember on this computer").click();
        h.run();
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Sign in").click();
        h.run();
        h.run();
        let st = &h.state().ui.copernicus;
        assert!(st.error.is_none() && st.warning.is_none(), "{:?} {:?}", st.error, st.warning);
        assert!(st.password.is_empty(), "the password isn't kept in the pane");
        h.get_by_label("Signed in as synthetic.user@example.invalid.");
        h.get_by_label("It is remembered on this computer.");
        let kept = store.load(copernicus::ACCOUNT).unwrap().expect("remembered");
        assert!(kept.expose().contains("synthetic-password"), "the store keeps the password, never a token");
        assert!(!kept.expose().contains("synthetic-access"));

        h.get_by_role_and_label(egui::accesskit::Role::Button, "Sign out").click();
        h.run();
        h.run();
        h.get_by_label("Not signed in.");
        assert_eq!(store.load(copernicus::ACCOUNT).unwrap(), None, "Sign out deletes the remembered sign-in");
        assert!(!h.state().ui.copernicus.remember, "Remember follows: nothing is remembered now");
        assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 2, "a sign-out asks nothing");
        copernicus::set_test_token_url(None);
        copernicus::set_secret_store(None);
    }

    /// A sign-in remembered but not kept (its host couldn't be reached at launch) can be deleted
    /// from the pane: Sign out is there with the form, and no request is made.
    #[test]
    fn a_remembered_sign_in_without_a_session_can_be_signed_out() {
        use rustgis_engine::data::secrets::Secret;
        // The sign-in and its store are process-wide: this scenario runs alone.
        if crate::isolate_account_test("copernicusui::tests::a_remembered_sign_in_without_a_session_can_be_signed_out") {
            return;
        }
        let store = std::sync::Arc::new(MemorySecrets::new());
        store
            .save(copernicus::ACCOUNT, &Secret::new(r#"{"username":"synthetic.user@example.invalid","password":"synthetic-password"}"#))
            .unwrap();
        let app = RustGisApp::with_sample().with_services(crate::Services { secrets: Some(store.clone()), ..Default::default() });
        // Read as the launch's restore reads it, with no sign-in made.
        assert!(copernicus::is_remembered() && copernicus::signed_in().is_none());
        let mut h = harness(app);
        h.run();
        h.get_by_label("Not signed in.");
        assert!(h.state().ui.copernicus.remember, "the box shows it is remembered");
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Sign out").click();
        h.run();
        h.run();
        assert_eq!(store.load(copernicus::ACCOUNT).unwrap(), None, "Sign out deletes it");
        assert!(h.query_by_label("Sign out").is_none() && !h.state().ui.copernicus.remember);
        copernicus::set_secret_store(None);
    }

    /// The test hook takes 127.0.0.1 itself, never an address with it as user info.
    #[test]
    fn the_test_endpoint_is_on_this_computer_only() {
        for elsewhere in [
            "http://127.0.0.1:@evil.example/token",
            "http://127.0.0.1:80@evil.example/token",
            "http://127.0.0.1:/token",
            "https://identity.example/token",
        ] {
            assert!(std::panic::catch_unwind(|| copernicus::set_test_token_url(Some(elsewhere))).is_err(), "{elsewhere}");
        }
    }

    /// Where the browser sends the sign-in (`Services::copernicus_sign_in`), Sign in hands it what
    /// was typed instead of running `copernicus.login`, empties the password and the one-time code,
    /// and waits for its answer; a refusal is shown and the form is back for another try.
    #[test]
    fn the_browser_sends_the_sign_in() {
        type Sent = (String, String, String, crate::Slot<Result<String, String>>);
        let sent: std::sync::Arc<std::sync::Mutex<Vec<Sent>>> = Default::default();
        let to = sent.clone();
        let services = crate::Services {
            copernicus_sign_in: Some(Box::new(move |username, password, totp, slot| {
                to.lock().unwrap().push((username, password, totp, slot))
            })),
            ..Default::default()
        };
        let mut h = harness(RustGisApp::with_sample().with_services(services));
        h.run();
        assert!(h.query_by_label("Remember on this computer").is_none(), "a browser's sign-in isn't remembered");
        type_into(&mut h, "Username", " synthetic.user@example.invalid ");
        type_into(&mut h, "Password", "synthetic-password");
        type_into(&mut h, "One-time code", " 123456 ");
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Sign in").click();
        h.run();
        let slot = {
            let sent = sent.lock().unwrap();
            assert_eq!(sent.len(), 1);
            let (username, password, totp, slot) = &sent[0];
            assert_eq!(
                (username.as_str(), password.as_str(), totp.as_str()),
                ("synthetic.user@example.invalid", "synthetic-password", "123456")
            );
            slot.clone()
        };
        let st = &h.state().ui.copernicus;
        assert!(st.password.is_empty() && st.totp.is_empty(), "nothing typed is kept in the pane");
        h.get_by_label("Signing in…");
        assert!(!sign_in_on(&h), "one sign-in at a time");
        assert!(
            h.get_by_role_and_label(egui::accesskit::Role::PasswordInput, "Password").accesskit_node().is_disabled(),
            "no typing while it waits"
        );
        type_into(&mut h, "Password", "typed-while-waiting");
        type_into(&mut h, "One-time code", "654321");
        let st = &h.state().ui.copernicus;
        assert!(st.password.is_empty() && st.totp.is_empty(), "the fields are off while it waits");
        // Even if something got in meanwhile, the answer leaves nothing behind.
        h.state_mut().ui.copernicus.password = "typed-while-waiting".into();
        h.state_mut().ui.copernicus.totp = "654321".into();

        *slot.lock().unwrap() = Some(Err("Copernicus Data Space refused the sign-in: Invalid user credentials".into()));
        h.run();
        let st = &h.state().ui.copernicus;
        assert!(st.pending.is_none() && st.error.as_deref().is_some_and(|e| e.contains("Invalid user credentials")), "{:?}", st.error);
        assert!(st.password.is_empty() && st.totp.is_empty(), "nothing typed while it waited is kept");
        h.get_by_label("Not signed in.");
        type_into(&mut h, "Password", "synthetic-password");
        assert!(sign_in_on(&h), "the form is back for another try");
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Sign in").click();
        h.run();
        let slot = sent.lock().unwrap()[1].3.clone();
        h.state_mut().ui.copernicus.password = "typed-while-waiting".into();
        *slot.lock().unwrap() = Some(Ok("synthetic.user@example.invalid".into()));
        h.run();
        let st = &h.state().ui.copernicus;
        assert!(st.pending.is_none() && st.error.is_none() && st.warning.is_none(), "{:?}", st.error);
        assert!(st.password.is_empty(), "a signed-in pane keeps no password for after Sign out");
        assert_eq!(sent.lock().unwrap().len(), 2);
    }

    /// Without a credential store (the command line, a test) there is no Remember to tick.
    #[test]
    fn no_store_no_remember() {
        let mut h = harness(RustGisApp::with_sample());
        h.run();
        assert!(h.query_by_label("Remember on this computer").is_none());
        h.get_by_label("Create a free account");
    }
}
