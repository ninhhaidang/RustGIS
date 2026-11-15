//! The Copernicus Data Space Ecosystem (CDSE): its sign-in, so the files on its data hosts
//! (the Copernicus Land Monitoring Service's COGs, the Sentinel mosaics) can be read by window.
//! Its STAC catalog is open; its files answer 401 without a token.
//!
//! [`login`] exchanges a CDSE account's username and password (and a one-time code, for an
//! account with two-factor authentication) for an access token and a refresh token at CDSE's
//! Keycloak ([`TOKEN_URL`], public client [`CLIENT_ID`]). The password is sent once, to that
//! address only, and never kept. The tokens live in memory for the session: never in a project,
//! a setting or the credential store. The access token is renewed with the refresh token shortly
//! before it expires ([`renewal_due`]); once the refresh token has expired too, the sign-in ends
//! and the user signs in again.
//!
//! [`authorization`] gives the access token, as `Authorization: Bearer`, only to HTTPS addresses
//! on CDSE's data hosts ([`DATA_HOSTS`]), never to the sign-in host. The native fetcher asks for
//! it at every hop, so a redirect that leaves those hosts goes without it, as Earthdata's does.
//!
//! The requests and answers are built and read here (sans I/O); sending them is desktop and
//! command line only (`native`).

use std::sync::Mutex;

use serde_json::Value;

/// CDSE's Keycloak token endpoint: the only address the password and the refresh token go to.
pub const TOKEN_URL: &str = "https://identity.dataspace.copernicus.eu/auth/realms/CDSE/protocol/openid-connect/token";
/// CDSE's public OAuth client, which signs in with a username and password.
pub const CLIENT_ID: &str = "cdse-public";
/// The hosts whose files need the token: the only ones it is sent to, over HTTPS.
pub const DATA_HOSTS: &[&str] = &["download.dataspace.copernicus.eu", "zipper.dataspace.copernicus.eu"];
/// Where an account is made.
pub const SIGN_UP: &str = "https://dataspace.copernicus.eu";
/// An access token is renewed this long before it expires (seconds).
const REFRESH_MARGIN: i64 = 60;
/// After the sign-in host couldn't be reached for a renewal, how long until it is tried again (seconds).
const RETRY_RENEWAL: i64 = 30;
/// The longest token kept (CDSE's JWTs are about 1–2 kB).
const MAX_TOKEN: usize = 16 << 10;
/// The longest username and password taken.
const MAX_FIELD: usize = 1024;

/// A sign-in kept for the session. Holds tokens: no `Debug`, no serialization.
#[derive(Clone, PartialEq, Eq)]
struct Session {
    /// The token endpoint it was made at, where it is renewed.
    endpoint: String,
    username: String,
    access: String,
    /// Unix seconds.
    expires_at: i64,
    refresh: Option<String>,
    /// Unix seconds; when the refresh token can no longer renew the access token.
    refresh_expires_at: i64,
    /// Unix seconds before which a renewal isn't tried again (after the host couldn't be reached).
    retry_at: i64,
}

struct State {
    session: Option<Session>,
    /// Whether a sign-in ended because its tokens expired, until another sign-in or a sign-out.
    expired: bool,
}

fn state() -> std::sync::MutexGuard<'static, State> {
    static S: Mutex<State> = Mutex::new(State { session: None, expired: false });
    S.lock().unwrap_or_else(|e| e.into_inner())
}

fn now_secs() -> i64 {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Ends the session when neither token can be used any more; whether one is left.
fn drop_if_expired(st: &mut State, now: i64) -> bool {
    let gone = st.session.as_ref().is_some_and(|s| s.expires_at <= now && (s.refresh.is_none() || s.refresh_expires_at <= now));
    if gone {
        st.session = None;
        st.expired = true;
    }
    st.session.is_some()
}

/// An immutable, exact snapshot of the sign-in, for binding approvals to it. It holds tokens and
/// deliberately has no formatting, serialization or raw-value accessors. A renewal changes it, as
/// `RuntimeAccounts` asks; a renewal that failed (and only holds off the next try) doesn't.
#[derive(Clone)]
pub struct AccountIdentity {
    session: Option<Session>,
}

impl PartialEq for AccountIdentity {
    fn eq(&self, other: &Self) -> bool {
        let key = |s: &Session| (s.endpoint.clone(), s.username.clone(), s.access.clone(), s.refresh.clone());
        self.session.as_ref().map(key) == other.session.as_ref().map(key)
    }
}

impl Eq for AccountIdentity {}

impl AccountIdentity {
    /// Whether the sign-in is still the one snapshotted. Never renews or performs I/O.
    pub fn is_current(&self) -> bool {
        account_identity().is_ok_and(|now| now == *self)
    }
}

/// Snapshots the sign-in kept, including its absence.
pub fn account_identity() -> Result<AccountIdentity, String> {
    Ok(AccountIdentity { session: state().session.clone() })
}

/// What the status tells of the sign-in: never a token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Info {
    pub username: String,
    /// When the access token expires (Unix seconds).
    pub expires_at: i64,
    /// Until when it is renewed (Unix seconds), if it can be.
    pub renews_until: Option<i64>,
}

/// The sign-in kept, if one is.
pub fn signed_in() -> Option<Info> {
    let mut st = state();
    drop_if_expired(&mut st, now_secs());
    st.session.as_ref().map(|s| Info {
        username: s.username.clone(),
        expires_at: s.expires_at,
        renews_until: s.refresh.as_ref().map(|_| s.refresh_expires_at).filter(|t| *t < i64::MAX),
    })
}

/// Whether a sign-in ended because its tokens expired, and none was made since.
pub fn expired() -> bool {
    let mut st = state();
    !drop_if_expired(&mut st, now_secs()) && st.expired
}

/// Forgets the sign-in; whether there was one.
pub fn logout() -> bool {
    let mut st = state();
    st.expired = false;
    st.session.take().is_some()
}

/// A form value, percent-encoded (`application/x-www-form-urlencoded`).
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// A username, password or one-time code as given, checked: never quoted back in the error.
fn field<'a>(name: &str, value: &'a str, required: bool) -> Result<Option<&'a str>, String> {
    if value.is_empty() {
        return if required { Err(format!("{name} is required (a Copernicus Data Space account: {SIGN_UP})")) } else { Ok(None) };
    }
    if value.len() > MAX_FIELD || value.chars().any(char::is_control) {
        return Err(format!("{name} is too long or has control characters"));
    }
    Ok(Some(value))
}

/// The form that signs in: `grant_type=password` for the public client, with `totp` when given.
pub fn login_body(username: &str, password: &str, totp: Option<&str>) -> Result<String, String> {
    let username = field("username", username.trim(), true)?.unwrap_or_default();
    let password = field("password", password, true)?.unwrap_or_default();
    let mut body = format!("grant_type=password&client_id={CLIENT_ID}&username={}&password={}", encode(username), encode(password));
    if let Some(code) = field("totp", totp.unwrap_or("").trim(), false)? {
        if !(4..=10).contains(&code.len()) || !code.bytes().all(|b| b.is_ascii_digit()) {
            return Err("totp is the one-time code of the account's authenticator app (digits)".into());
        }
        body.push_str(&format!("&totp={code}"));
    }
    Ok(body)
}

fn refresh_body(refresh: &str) -> String {
    format!("grant_type=refresh_token&client_id={CLIENT_ID}&refresh_token={}", encode(refresh))
}

fn valid_token(t: &str) -> bool {
    !t.is_empty() && t.len() <= MAX_TOKEN && t.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.~+/=".contains(&b))
}

/// What a token answer gave.
struct Grant {
    access: String,
    expires_at: i64,
    refresh: Option<String>,
    refresh_expires_at: i64,
}

/// Reads Keycloak's token answer. A refusal (`error`, with HTTP 400 or 401) says why in
/// Keycloak's words; nothing else of the answer is quoted.
fn parse_grant(status: u16, body: &[u8], now: i64) -> Result<Grant, String> {
    let v: Value =
        serde_json::from_slice(body).map_err(|_| format!("Copernicus Data Space's sign-in answered HTTP {status}, not a token"))?;
    if let Some(e) = v.get("error").and_then(Value::as_str) {
        let why = v.get("error_description").and_then(Value::as_str).unwrap_or(e);
        let why: String = why.chars().filter(|c| !c.is_control()).take(200).collect();
        return Err(format!("Copernicus Data Space refused the sign-in: {why}"));
    }
    if !(200..300).contains(&status) {
        return Err(format!("Copernicus Data Space's sign-in answered HTTP {status}, not a token"));
    }
    let access =
        v.get("access_token").and_then(Value::as_str).filter(|t| valid_token(t)).ok_or("Copernicus Data Space gave no access token")?;
    let lifetime = |k: &str| v.get(k).and_then(Value::as_i64).filter(|s| *s > 0).map(|s| s.min(366 * 86_400));
    let refresh = v.get("refresh_token").and_then(Value::as_str).filter(|t| valid_token(t)).map(str::to_string);
    // Never guessed: a token whose lifetime isn't said can't be renewed in time.
    let lifetime_in = lifetime("expires_in").ok_or("Copernicus Data Space gave a token without its lifetime (expires_in)")?;
    Ok(Grant {
        access: access.to_string(),
        expires_at: now.saturating_add(lifetime_in),
        // Unsaid (or 0, Keycloak's "no limit"): renewed until refused.
        refresh_expires_at: lifetime("refresh_expires_in").map_or(i64::MAX, |l| now.saturating_add(l)),
        refresh,
    })
}

/// Keycloak's error code in a token answer (`invalid_grant`…), if it gave one.
fn grant_error(body: &[u8]) -> Option<String> {
    let v: Value = serde_json::from_slice(body).ok()?;
    v.get("error").and_then(Value::as_str).map(str::to_string)
}

/// Keeps a sign-in made at `endpoint` from its token answer; what the status tells of it.
#[cfg(not(target_arch = "wasm32"))]
fn keep(endpoint: &str, username: &str, status: u16, body: &[u8]) -> Result<Info, String> {
    let now = now_secs();
    let g = parse_grant(status, body, now)?;
    let mut st = state();
    st.expired = false;
    st.session = Some(Session {
        endpoint: endpoint.to_string(),
        // Whole: the form already caps it (`MAX_FIELD`).
        username: username.trim().to_string(),
        access: g.access,
        expires_at: g.expires_at,
        refresh_expires_at: g.refresh_expires_at,
        refresh: g.refresh,
        retry_at: 0,
    });
    drop(st);
    signed_in().ok_or_else(|| "Copernicus Data Space gave a token that has already expired".into())
}

/// The host of an HTTPS address, lowercase: no user info or port.
fn https_host(url: &str) -> Option<String> {
    let rest = url.get(..8).filter(|s| s.eq_ignore_ascii_case("https://")).and_then(|_| url.get(8..))?;
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.is_empty() || authority.contains(['@', ':', '[', '\\']) {
        return None;
    }
    Some(authority.trim_end_matches('.').to_ascii_lowercase())
}

/// Whether `url` is on a CDSE data host, the only place the token is sent: HTTPS, no user info
/// or port, the host exactly one of [`DATA_HOSTS`].
pub fn is_data_host(url: &str) -> bool {
    https_host(url).is_some_and(|h| DATA_HOSTS.contains(&h.as_str())) || tests_host(url)
}

/// Origins a test serves as a data host (plain HTTP on this computer). Tests only.
#[cfg(test)]
pub(crate) static TEST_HOSTS: Mutex<Vec<String>> = Mutex::new(Vec::new());

#[cfg(test)]
fn tests_host(url: &str) -> bool {
    TEST_HOSTS.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|o| url.strip_prefix(o.as_str()).is_some_and(|r| r.starts_with('/')))
}

#[cfg(not(test))]
fn tests_host(_url: &str) -> bool {
    false
}

/// The `Authorization` header a request to `url` carries: the access token, for a data host
/// only, while it hasn't expired. Never renews: see `native::authorization_fresh`.
pub fn authorization(url: &str) -> Option<String> {
    if !is_data_host(url) {
        return None;
    }
    let mut st = state();
    let now = now_secs();
    drop_if_expired(&mut st, now);
    st.session.as_ref().filter(|s| s.expires_at > now).map(|s| format!("Bearer {}", s.access))
}

/// A renewal to send. Holds the refresh token: no `Debug`.
#[derive(Clone, PartialEq, Eq)]
pub struct Renewal {
    /// The token endpoint, and the form to POST to it.
    pub endpoint: String,
    pub body: String,
    refresh: String,
}

/// The renewal the sign-in needs: its access token is about to expire, its refresh token hasn't,
/// and the sign-in host wasn't just found unreachable.
pub fn renewal_due() -> Option<Renewal> {
    let mut st = state();
    let now = now_secs();
    if !drop_if_expired(&mut st, now) {
        return None;
    }
    let s = st.session.as_ref()?;
    let refresh = s.refresh.as_ref()?;
    (s.expires_at - now <= REFRESH_MARGIN && s.refresh_expires_at > now && now >= s.retry_at).then(|| Renewal {
        endpoint: s.endpoint.clone(),
        body: refresh_body(refresh),
        refresh: refresh.clone(),
    })
}

/// Lands a renewal's answer (`Err`: the host wasn't reached): new tokens are kept; a refusal ends
/// the sign-in; an unreachable host, or a server error, keeps it and holds off renewing for a
/// while. Nothing changes for a sign-in replaced or ended since.
pub fn renewal_done(r: &Renewal, answer: Result<(u16, &[u8]), String>) {
    let mut st = state();
    let Some(s) = st.session.as_mut().filter(|s| s.refresh.as_deref() == Some(r.refresh.as_str())) else { return };
    let now = now_secs();
    // Keycloak refuses an expired or revoked refresh token with 400 `invalid_grant`, and a refused
    // client with 401 and its own `error` (`invalid_client`…). Any other answer (a rate limit, a
    // malformed-request 400, a proxy's page, even a 401 one) says nothing of the refresh token: the
    // sign-in is kept and renewed again later.
    let refused =
        |status: u16, body: &[u8]| matches!((status, grant_error(body).as_deref()), (400, Some("invalid_grant")) | (401, Some(_)));
    match answer.map(|(status, body)| (status, refused(status, body), parse_grant(status, body, now))) {
        Ok((_, _, Ok(g))) => {
            s.access = g.access;
            s.expires_at = g.expires_at;
            if g.refresh.is_some() {
                s.refresh = g.refresh;
                s.refresh_expires_at = g.refresh_expires_at;
            }
        }
        Ok((_, true, Err(e))) => {
            log::warn!("the Copernicus Data Space sign-in ended: {e}");
            st.session = None;
            st.expired = true;
        }
        Ok((status, false, Err(_))) => {
            log::warn!("couldn't renew the Copernicus Data Space sign-in: HTTP {status}");
            s.retry_at = now + RETRY_RENEWAL;
        }
        Err(e) => {
            log::warn!("couldn't renew the Copernicus Data Space sign-in: {e}");
            s.retry_at = now + RETRY_RENEWAL;
        }
    }
}

/// The message for a request a data host turned away (HTTP 401).
pub fn refused_message(url: &str) -> String {
    // An OData address names its file in its last node: `…/Nodes(f.tif)/$value`.
    let last = url.split(['?', '#']).next().unwrap_or(url).trim_end_matches("/$value").rsplit('/').next().unwrap_or("");
    let file = last.strip_prefix("Nodes(").and_then(|n| n.strip_suffix(')')).unwrap_or(last);
    if let Some(info) = signed_in() {
        // Kept, but its token expired and the sign-in host didn't renew it (it holds off a while).
        if info.expires_at <= now_secs() {
            format!(
                "{file}: the Copernicus Data Space sign-in's token expired and couldn't be renewed yet: try again in a minute, or sign in again (copernicus.login)"
            )
        } else {
            format!("{file}: Copernicus Data Space didn't accept the sign-in's token: sign in again (copernicus.login)")
        }
    } else if expired() {
        format!("{file}: the Copernicus Data Space sign-in expired: sign in again (copernicus.login)")
    } else {
        format!("{file}: Copernicus Data Space asks for a sign-in for this file: sign in with a free account (copernicus.login)")
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::{authorization_fresh, login};

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;

    const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

    /// The agent for token requests: never redirected (a redirect would replay the password or
    /// the refresh token), an error status answered for the caller to read.
    fn agent() -> &'static ureq::Agent {
        static A: OnceLock<ureq::Agent> = OnceLock::new();
        A.get_or_init(|| {
            ureq::Agent::config_builder()
                .tls_config(rustgis_data::http::tls_config())
                .http_status_as_error(false)
                .timeout_global(Some(REQUEST_TIMEOUT))
                .user_agent(concat!("RustGIS/", env!("CARGO_PKG_VERSION"), " (+https://rustgis.app)"))
                .max_redirects(0)
                .max_idle_connections(0)
                .build()
                .into()
        })
    }

    /// POSTs a form; its status and answer. The error never quotes the form.
    fn post_form(url: &str, body: &str, still: &Option<crate::cache::Still>) -> Result<(u16, Vec<u8>), String> {
        let req = agent().post(url).header("Content-Type", "application/x-www-form-urlencoded").header("Accept", "application/json");
        crate::cache::check_still(still)?;
        let resp = req.send(body).map_err(|e| format!("couldn't reach Copernicus Data Space's sign-in: {e}"))?;
        let status = resp.status().as_u16();
        let bytes = resp.into_body().with_config().limit(1 << 20).read_to_vec().map_err(|e| e.to_string())?;
        Ok((status, bytes))
    }

    /// Signs in to CDSE with an account's username and password (and its one-time code when it
    /// uses two-factor authentication), replacing the sign-in kept; a refused sign-in leaves it.
    pub fn login(username: &str, password: &str, totp: Option<&str>) -> Result<super::Info, String> {
        login_at(super::TOKEN_URL, username, password, totp)
    }

    pub(crate) fn login_at(endpoint: &str, username: &str, password: &str, totp: Option<&str>) -> Result<super::Info, String> {
        let body = super::login_body(username, password, totp)?;
        let (status, answer) = post_form(endpoint, &body, &None)?;
        if status >= 500 {
            return Err(format!("Copernicus Data Space's sign-in answered HTTP {status}: try again later"));
        }
        super::keep(endpoint, username, status, &answer)
    }

    /// Like [`authorization`](super::authorization), renewing the access token first when it is
    /// about to expire. A refused renewal ends the sign-in; an unreachable host keeps it.
    pub fn authorization_fresh(url: &str) -> Option<String> {
        authorization_fresh_using(url, crate::cache::current_still(), post_form)
    }

    pub(crate) fn authorization_fresh_using(
        url: &str,
        still: Option<crate::cache::Still>,
        post: impl FnOnce(&str, &str, &Option<crate::cache::Still>) -> Result<(u16, Vec<u8>), String>,
    ) -> Option<String> {
        if !super::is_data_host(url) {
            return None;
        }
        crate::cache::check_still(&still).ok()?;
        if super::renewal_due().is_some() {
            // One renewal at a time: the others wait for it, then find the token fresh.
            static RENEWING: Mutex<()> = Mutex::new(());
            let _one = RENEWING.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(r) = super::renewal_due() {
                crate::cache::check_still(&still).ok()?;
                let answer = post(&r.endpoint, &r.body, &still);
                super::renewal_done(&r, answer.as_ref().map(|(s, b)| (*s, b.as_slice())).map_err(String::clone));
            }
        }
        super::authorization(url)
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) use native::{authorization_fresh_using, login_at};

#[cfg(test)]
mod tests;
