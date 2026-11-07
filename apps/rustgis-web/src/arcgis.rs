//! Signing in to ArcGIS in the browser: the portal's sign-in page in a pop-up window, which comes
//! back to the app's own `oauth-callback.html` (same origin, so its address can be read here),
//! then the code is exchanged for tokens with the browser's fetch (ArcGIS answers web pages from
//! any origin) and the session is kept as on the desktop (see `rustgis_tiles::arcgis_auth`;
//! requests carry the token as their `token` parameter, see `arcgis_auth::with_token`).
//! Access tokens are renewed before they expire, by the fetcher.

use std::cell::Cell;

use rustgis_ui_egui::Slot;
use rustgis_ui_egui::rustgis_engine::render::rustgis_tiles::{arcgis_auth, cache};
use wasm_bindgen::JsCast as _;

/// How long a request to the portal may take before it is given up.
const REQUEST_MS: i32 = 30_000;
/// The largest answer read from the portal.
const MAX_ANSWER: usize = 4 << 20;
/// How long the user has to finish the portal's page.
const SIGN_IN_MS: f64 = 5.0 * 60_000.0;

thread_local! {
    /// A sign-in is under way (one at a time).
    static SIGNING_IN: Cell<bool> = const { Cell::new(false) };
    /// A renewal is under way: the fetches meanwhile use the token still valid.
    static RENEWING: Cell<bool> = const { Cell::new(false) };
}

fn err(e: wasm_bindgen::JsValue) -> String {
    format!("{e:?}")
}

/// 32 random bytes from the browser's crypto, as hex (a PKCE verifier or a state).
fn random() -> Result<String, String> {
    let crypto = web_sys::window().ok_or("no window")?.crypto().map_err(err)?;
    let mut bytes = [0u8; 32];
    crypto.get_random_values_with_u8_array(&mut bytes).map_err(err)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// The callback page next to the app's page (`…/index.html` → `…/oauth-callback.html`).
fn redirect() -> Result<String, String> {
    let location = web_sys::window().ok_or("no window")?.location();
    let origin = location.origin().map_err(err)?;
    let path = location.pathname().map_err(err)?;
    let dir = path.rsplit_once('/').map_or("", |(d, _)| d);
    Ok(format!("{origin}{dir}/{}", arcgis_auth::WEB_CALLBACK))
}

/// Waits `ms`; an error when no timer can be set (the caller gives up rather than spin).
async fn sleep(ms: i32) -> Result<(), String> {
    let window = web_sys::window().ok_or("no window")?;
    let mut timed = Ok(());
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        timed = window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms).map(drop);
    });
    timed.map_err(|_| "the browser has no timer for the sign-in".to_string())?;
    wasm_bindgen_futures::JsFuture::from(promise).await.map(drop).map_err(err)
}

/// A request to the portal: a form POSTed, or a GET. Never redirected, never with the page's
/// cookies.
async fn request(url: &str, form: Option<&str>) -> Result<Vec<u8>, String> {
    request_claimed(url, form, None).await
}

/// A renewal caused by a claimed download checks its owned predicate on first poll and send.
async fn request_claimed(url: &str, form: Option<&str>, still: Option<cache::Still>) -> Result<Vec<u8>, String> {
    cache::check_still(&still)?;
    let window = web_sys::window().ok_or("no window")?;
    let init = web_sys::RequestInit::new();
    init.set_redirect(web_sys::RequestRedirect::Error);
    init.set_credentials(web_sys::RequestCredentials::Omit);
    let headers = web_sys::Headers::new().map_err(err)?;
    headers.set("Accept", "application/json").map_err(err)?;
    if let Some(body) = form {
        init.set_method("POST");
        headers.set("Content-Type", "application/x-www-form-urlencoded").map_err(err)?;
        init.set_body(&wasm_bindgen::JsValue::from_str(body));
    }
    init.set_headers(&headers);
    // A portal that never answers is given up, so a sign-in or a renewal can't hang for good.
    let abort = web_sys::AbortController::new().map_err(err)?;
    init.set_signal(Some(&abort.signal()));
    let stop = {
        let abort = abort.clone();
        wasm_bindgen::closure::Closure::once_into_js(move || abort.abort())
    };
    let timer = window.set_timeout_with_callback_and_timeout_and_arguments_0(stop.unchecked_ref(), REQUEST_MS).ok();
    let read = async {
        let promise =
            crate::request::prepare_and_send(&still, || async { Ok(()) }, |()| window.fetch_with_str_and_init(url, &init)).await?;
        let resp: web_sys::Response = wasm_bindgen_futures::JsFuture::from(promise)
            .await
            .map_err(|_| "couldn't reach the portal".to_string())?
            .dyn_into()
            .map_err(err)?;
        // A server error says nothing of the request: it counts as the portal not being reached.
        if resp.status() >= 500 {
            return Err(format!("the portal answered HTTP {}", resp.status()));
        }
        let length = resp.headers().get("content-length").ok().flatten().and_then(|l| l.parse::<usize>().ok());
        if length.is_some_and(|n| n > MAX_ANSWER) {
            return Err("the portal's answer is too large".into());
        }
        let buf = wasm_bindgen_futures::JsFuture::from(resp.array_buffer().map_err(err)?)
            .await
            .map_err(|_| "couldn't reach the portal".to_string())?;
        let bytes = js_sys::Uint8Array::new(&buf);
        if bytes.length() as usize > MAX_ANSWER {
            return Err("the portal's answer is too large".into());
        }
        Ok(bytes.to_vec())
    };
    let result = read.await;
    if let Some(t) = timer {
        window.clear_timeout_with_handle(t);
    }
    result
}

/// Signs in to `portal` (blank: ArcGIS Online) with `client_id` (blank: RustGIS's own app on
/// ArcGIS Online): opens the portal's page in a pop-up and, when it comes back, keeps the session.
/// The slot gets who signed in, or why not. Called from a click (the pop-up must open right away).
pub fn sign_in(portal: String, client_id: String, slot: Slot<Result<String, String>>, ctx: egui::Context) {
    let put = move |r: Result<String, String>| {
        *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
        ctx.request_repaint();
    };
    if SIGNING_IN.get() {
        return put(Err("a sign-in is already open: finish it in its window, or close that window".into()));
    }
    let started = (|| {
        let flow = arcgis_auth::web_sign_in(&portal, &client_id, &redirect()?, &random()?, &random()?)?;
        let window = web_sys::window().ok_or("no window")?;
        let popup = window
            .open_with_url_and_target_and_features(&flow.url, "rustgis-arcgis", "popup,width=520,height=720")
            .map_err(err)?
            .ok_or("the browser blocked the sign-in window: allow pop-ups for this site, then Sign in again")?;
        Ok::<_, String>((flow, popup))
    })();
    let (flow, popup) = match started {
        Ok(f) => f,
        Err(e) => return put(Err(e)),
    };
    SIGNING_IN.set(true);
    wasm_bindgen_futures::spawn_local(async move {
        let result = finish(&flow, &popup).await;
        let _ = popup.close();
        SIGNING_IN.set(false);
        put(result);
    });
}

async fn finish(flow: &arcgis_auth::WebSignIn, popup: &web_sys::Window) -> Result<String, String> {
    let deadline = js_sys::Date::now() + SIGN_IN_MS;
    let (endpoint, body) = loop {
        sleep(300).await?;
        if popup.closed().unwrap_or(true) {
            return Err("the sign-in window was closed".into());
        }
        if js_sys::Date::now() > deadline {
            return Err("the sign-in timed out: finish it within five minutes".into());
        }
        // The portal's pages are another origin, so their address can't be read: only the
        // callback page's, once the portal sends the window back.
        let Ok(href) = popup.location().href() else { continue };
        if let Some(step) = flow.exchange(&href) {
            break step?;
        }
    };
    let grant = arcgis_auth::parse_grant(&request(&endpoint, Some(&body)).await?, js_sys::Date::now() as i64 / 1000)?;
    // The token as the request's parameter, like the fetcher's: a header's CORS preflight can be refused.
    let discovery = arcgis_auth::with_token_value(&arcgis_auth::discovery_url(&flow.portal), &grant.access_token);
    let discovered = match discovery {
        Some(url) => request(&url, None).await.map_err(|e| log::warn!("{}: couldn't read what the sign-in covers: {e}", flow.portal)).ok(),
        None => None,
    };
    let scopes = arcgis_auth::discovered_scopes(&flow.portal, discovered.as_deref());
    let username = grant.username.clone();
    arcgis_auth::sign_in(&flow.portal, &flow.client_id, grant, scopes);
    // Tiles and documents refused before the sign-in are asked for again.
    cache::clear();
    Ok(username)
}

/// Renews the session covering `url` when its access token is about to expire (one renewal at a
/// time; the fetches meanwhile use the token, still valid for a minute).
pub async fn renew_if_due(url: &str, still: Option<cache::Still>) {
    if cache::check_still(&still).is_err() {
        return;
    }
    if RENEWING.get() {
        return;
    }
    let Some(r) = arcgis_auth::renewal_due(url) else { return };
    /// Clears the flag however the renewal ends, the fetch that started it dropped included.
    struct Renewing;
    impl Drop for Renewing {
        fn drop(&mut self) {
            RENEWING.set(false);
        }
    }
    RENEWING.set(true);
    let _renewing = Renewing;
    let answer = request_claimed(&r.endpoint, Some(&r.body), still.clone()).await;
    if cache::check_still(&still).is_err() {
        return;
    }
    arcgis_auth::renewal_done(&r, answer.as_deref().map_err(String::clone));
}
