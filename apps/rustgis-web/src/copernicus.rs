//! Signing in to Copernicus Data Space in the browser: the username and password are POSTed to
//! CDSE's token endpoint with the browser's fetch (its Keycloak answers web pages from any
//! origin), and the answer is kept as on the desktop (`rustgis_tiles::copernicus::login_answer`).
//! The password goes to that endpoint only and is not kept, not even in the page's storage: the
//! browser has no credential store, so a sign-in lasts as long as the page and its renewals.
//! Access tokens are renewed before they expire, by the fetcher ([`renew_if_due`]), which sends
//! the token to CDSE's data hosts only.

use std::cell::Cell;

use rustgis_ui_egui::Slot;
use rustgis_ui_egui::rustgis_engine::render::rustgis_tiles::{cache, copernicus};

/// The service, as the messages name it.
const WHO: &str = "Copernicus Data Space";
/// How often a fetch waiting for another one's renewal looks again (milliseconds).
const WAIT_MS: i32 = 50;

thread_local! {
    /// A sign-in is under way (one at a time).
    static SIGNING_IN: Cell<bool> = const { Cell::new(false) };
    /// A renewal is under way: the fetches meanwhile use the token still valid, or wait for it.
    static RENEWING: Cell<bool> = const { Cell::new(false) };
}

/// Signs in to Copernicus Data Space with an account's username, password and one-time code
/// (blank when the account has no two-factor authentication), replacing the sign-in kept. The
/// slot gets who signed in, or why not.
pub fn sign_in(username: String, password: String, totp: String, slot: Slot<Result<String, String>>, ctx: egui::Context) {
    let put = move |r: Result<String, String>| {
        *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
        ctx.request_repaint();
    };
    if SIGNING_IN.get() {
        return put(Err("a sign-in is already under way".into()));
    }
    // Checked before anything is sent; the form is all that is kept of the password.
    let body = match copernicus::login_body(&username, &password, Some(&totp)) {
        Ok(body) => body,
        Err(e) => return put(Err(e)),
    };
    drop(password);
    // A sign-out while it is on its way wins: its answer isn't kept then.
    let sent = copernicus::sending();
    SIGNING_IN.set(true);
    wasm_bindgen_futures::spawn_local(async move {
        let result = async {
            let (status, answer) = crate::arcgis::form_request(copernicus::TOKEN_URL, Some(&body), None, WHO).await?;
            let info = copernicus::login_answer(&sent, &username, status, &answer)?;
            // Files refused before the sign-in are asked for again.
            cache::clear();
            Ok(info.username)
        }
        .await;
        SIGNING_IN.set(false);
        put(result);
    });
}

/// Renews the sign-in's access token when `url` is on a CDSE data host and the token is about to
/// expire (one renewal at a time; the fetches meanwhile use the token while it is still valid, and
/// once it has expired, the page left idle, wait for the renewal, as on the desktop, rather than
/// go without it and be refused). A refused renewal ends the sign-in; an unreachable host keeps
/// it and is tried again later. An answer that lands after the fetch was cancelled is still kept: the server may have spent the
/// refresh token, and `renewal_done` changes nothing for a sign-in replaced or ended since.
pub async fn renew_if_due(url: &str, still: Option<cache::Still>) {
    if !copernicus::is_data_host(url) || cache::check_still(&still).is_err() {
        return;
    }
    if RENEWING.get() {
        while RENEWING.get() && copernicus::authorization(url).is_none() {
            if cache::check_still(&still).is_err() || crate::arcgis::sleep(WAIT_MS).await.is_err() {
                return;
            }
        }
        return;
    }
    let Some(r) = copernicus::renewal_due() else { return };
    /// Clears the flag however the renewal ends, the fetch that started it dropped included.
    struct Renewing;
    impl Drop for Renewing {
        fn drop(&mut self) {
            RENEWING.set(false);
        }
    }
    RENEWING.set(true);
    let _renewing = Renewing;
    let answer = crate::arcgis::form_request(&r.endpoint, Some(&r.body), still, WHO).await;
    copernicus::renewal_done(&r, answer.as_ref().map(|(status, body)| (*status, body.as_slice())).map_err(String::clone));
}
