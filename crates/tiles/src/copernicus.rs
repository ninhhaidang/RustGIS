//! The Copernicus Data Space Ecosystem (CDSE): its sign-in, so the files on its data hosts
//! (the Copernicus Land Monitoring Service's COGs, the Sentinel mosaics) can be read by window.
//! Its STAC catalog is open; its files answer 401 without a token.
//!
//! [`login`] exchanges a CDSE account's username and password (and a one-time code, for an
//! account with two-factor authentication) for an access token and a refresh token at CDSE's
//! Keycloak ([`TOKEN_URL`], public client [`CLIENT_ID`]). The password is sent to that address
//! only. The tokens live in memory for the session: never in a project, a setting or
//! the credential store. The access token is renewed with the refresh token shortly before it
//! expires ([`renewal_due`]); once the refresh token has expired too, the sign-in ends and the
//! user signs in again.
//!
//! A sign-in may also be remembered on this computer (`login` with `remember`): the username
//! and password go to the credential store [`set_secret_store`] gives (the one Earthdata's token
//! and saved database passwords use), at [`ACCOUNT`], and nowhere else. When a data host is asked
//! for and no sign-in is kept (after launch, or once the refresh token has expired), they sign in
//! again (`restore`). An account with two-factor authentication isn't remembered: each sign-in
//! needs its one-time code.
//!
//! [`authorization`] gives the access token, as `Authorization: Bearer`, only to HTTPS addresses
//! on CDSE's data hosts ([`DATA_HOSTS`]), never to the sign-in host. The native fetcher asks for
//! it at every hop, so a redirect that leaves those hosts goes without it, as Earthdata's does.
//!
//! The requests and answers are built and read here (sans I/O); the desktop and the command line
//! send them here (`native`), the web app with the browser's fetch ([`login_answer`],
//! [`renewal_due`] and [`renewal_done`]).

use std::sync::{Arc, Mutex};

use rustgis_data::secrets::{SecretError, SecretStore};
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
    /// Until when it is renewed (Unix seconds), if it can be and that is said.
    pub renews_until: Option<i64>,
    /// Whether it is renewed at all (a refresh token was given): with no `renews_until`, until
    /// CDSE refuses the refresh token.
    pub renews: bool,
}

/// The sign-in kept, if one is.
pub fn signed_in() -> Option<Info> {
    let mut st = state();
    drop_if_expired(&mut st, now_secs());
    st.session.as_ref().map(|s| Info {
        username: s.username.clone(),
        expires_at: s.expires_at,
        renews_until: s.refresh.as_ref().map(|_| s.refresh_expires_at).filter(|t| *t < i64::MAX),
        renews: s.refresh.is_some(),
    })
}

/// Whether a sign-in ended because its tokens expired, and none was made since.
pub fn expired() -> bool {
    let mut st = state();
    !drop_if_expired(&mut st, now_secs()) && st.expired
}

/// Forgets the sign-in for the session; whether there was one. [`forget`] also deletes the one
/// remembered on this computer.
pub fn logout() -> bool {
    let mut st = state();
    st.expired = false;
    st.session.take().is_some()
}

/// The credential store's account of the sign-in remembered on this computer.
pub const ACCOUNT: &str = "copernicus/sign-in";
/// `login` with `remember` where there is no credential store (the command line and Python
/// unless they opt in to saved secrets).
pub const NO_STORE: &str = "The sign-in is kept for this session only: there is no credential store here to remember it in.";
/// `login` with `remember`, when the credential store didn't keep the sign-in.
pub const NOT_REMEMBERED: &str = "The sign-in is kept for this session, but the system's credential store didn't remember it.";
/// `login` with `remember` and a one-time code.
pub const TWO_FACTOR: &str = "The sign-in is kept for this session only: an account with two-factor authentication needs its one-time code at each sign-in, so it isn't remembered.";
/// The credential store didn't delete the sign-in remembered earlier.
pub const NOT_FORGOTTEN: &str = "The sign-in remembered on this computer couldn't be deleted from the system's credential store.";

/// What this session knows of the credential store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
// The browser has no store: `retry_at` is read by the native `restore` only.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
enum Known {
    /// Not read yet.
    Unread,
    /// It holds no sign-in, as far as this session knows.
    Nothing,
    /// It holds one; a sign-in with it isn't tried before `retry_at` (Unix seconds).
    Kept { retry_at: i64 },
    /// It refused to be read (locked): it may hold one, never signed in with this session.
    Unreadable,
}

impl Known {
    /// What a failed store call leaves known: nothing, where there is no store at all.
    fn after_failure(e: &SecretError) -> Known {
        if matches!(e, SecretError::Unavailable(_)) { Known::Nothing } else { Known::Unreadable }
    }
}

/// The sign-in remembered on this computer: where it is kept, and what this session knows of it.
/// Never holds the password: it is read from the store each time it signs in.
struct Remembered {
    store: Mutex<Option<Arc<dyn SecretStore>>>,
    /// Held while the store is used (a sign-in with it included), so one sign-in serves every
    /// request waiting for it, and a sign-out waits for one under way.
    known: Mutex<Known>,
}

static REMEMBERED: Remembered = Remembered { store: Mutex::new(None), known: Mutex::new(Known::Unread) };

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn secret_store() -> Option<Arc<dyn SecretStore>> {
    lock(&REMEMBERED.store).clone()
}

/// Gives the credential store a sign-in is remembered in (the desktop's keychain; the command
/// line's and Python's when they opt in to saved secrets).
pub fn set_secret_store(store: Option<Arc<dyn SecretStore>>) {
    let mut known = lock(&REMEMBERED.known);
    *lock(&REMEMBERED.store) = store;
    *known = Known::Unread;
}

/// Reads whether the store holds a sign-in, if this session doesn't know yet; never signs in.
fn read_store(known: &mut Known, store: &dyn SecretStore) {
    if *known == Known::Unread {
        // A failing store is asked once: `Reporting` tells the user why.
        *known = match store.load(ACCOUNT) {
            Ok(Some(_)) => Known::Kept { retry_at: 0 },
            Ok(None) => Known::Nothing,
            Err(e) => Known::after_failure(&e),
        };
    }
}

/// Whether a sign-in is remembered on this computer. Reads the store the first time (the system
/// may ask for it to be unlocked); never signs in.
pub fn is_remembered() -> bool {
    let mut known = lock(&REMEMBERED.known);
    if let Some(store) = secret_store() {
        read_store(&mut known, store.as_ref());
    }
    matches!(*known, Known::Kept { .. })
}

/// [`is_remembered`] for a frame: never waits and never reads the store (the system may ask for
/// it to be unlocked). `false` until it has been read, off the frame: by `restore` at launch, or
/// a sign-in or sign-out. `None` while the store is in use (a sign-in with the remembered one under
/// way, which may wait up to a request's timeout).
pub fn is_remembered_now() -> Option<bool> {
    known_now().map(|k| matches!(k, Known::Kept { .. }))
}

/// [`is_remembered_now`] for one a request can still sign in with: not one refused or damaged
/// that the store couldn't delete, which this session never tries again.
pub fn is_usable_remembered_now() -> Option<bool> {
    known_now().map(usable_remembered)
}

/// What the store holds, as this session knows it; `None` while the store is in use.
fn known_now() -> Option<Known> {
    match REMEMBERED.known.try_lock() {
        Ok(k) => Some(*k),
        Err(std::sync::TryLockError::Poisoned(e)) => Some(*e.into_inner()),
        Err(std::sync::TryLockError::WouldBlock) => None,
    }
}

/// Whether `known` holds a sign-in a request can still use: `retry_at` is `i64::MAX` only for one
/// refused or damaged that couldn't be deleted (or kept when a sign-out couldn't delete it).
fn usable_remembered(known: Known) -> bool {
    matches!(known, Known::Kept { retry_at } if retry_at != i64::MAX)
}

/// The username and password as the store keeps them.
#[cfg(not(target_arch = "wasm32"))]
fn remembered_value(username: &str, password: &str) -> rustgis_data::secrets::Secret {
    rustgis_data::secrets::Secret::new(serde_json::json!({"username": username.trim(), "password": password}).to_string())
}

#[cfg(not(target_arch = "wasm32"))]
fn read_remembered(value: &str) -> Option<(String, String)> {
    let v: Value = serde_json::from_str(value).ok()?;
    let text = |k: &str| v.get(k).and_then(Value::as_str).filter(|t| !t.is_empty()).map(str::to_string);
    Some((text("username")?, text("password")?))
}

/// After a sign-in: remembers it (with `remember`, and no one-time code), or deletes the one
/// remembered before, here or in another run, so the next run doesn't bring it back. Returns a
/// warning when it is kept for the session only against the wish to remember it, or the one
/// remembered before couldn't be deleted.
#[cfg(not(target_arch = "wasm32"))]
fn remember_after(known: &mut Known, username: &str, password: &str, totp: bool, remember: bool) -> Option<&'static str> {
    let Some(store) = secret_store() else { return remember.then_some(NO_STORE) };
    let was = matches!(*known, Known::Kept { .. });
    if remember && !totp && store.save(ACCOUNT, &remembered_value(username, password)).is_ok() {
        *known = Known::Kept { retry_at: 0 };
        return None;
    }
    let deleted = store.delete(ACCOUNT);
    // Unread: an earlier run may have remembered one, unless there's no store at all.
    let unknown = matches!(*known, Known::Unread | Known::Unreadable);
    let lingers = |e: &SecretError| was || (unknown && !matches!(e, SecretError::Unavailable(_)));
    let left = deleted.as_ref().err().is_some_and(lingers);
    // One known to be there that couldn't be deleted is never signed in with again this session;
    // one that may be there stays unknown (`remembered` reports only what was found).
    *known = match deleted {
        Err(_) if was => Known::Kept { retry_at: i64::MAX },
        Err(_) if left => Known::Unreadable,
        _ => Known::Nothing,
    };
    match deleted {
        // Said first: a password remembered before may still be there.
        Err(_) if left => Some(NOT_FORGOTTEN),
        _ if remember && totp => Some(TWO_FACTOR),
        _ if remember => Some(NOT_REMEMBERED),
        // A system without a credential store had nowhere to keep one.
        Ok(()) | Err(SecretError::Unavailable(_)) => None,
        Err(_) => Some(NOT_FORGOTTEN),
    }
}

/// Signs out and deletes the sign-in remembered on this computer, even one this session didn't
/// read; `Err` when the credential store couldn't delete it.
pub fn forget() -> Result<(), &'static str> {
    // Taken first: a sign-in with the remembered one under way finishes, then goes too.
    let mut known = lock(&REMEMBERED.known);
    logout();
    let was = matches!(*known, Known::Kept { .. });
    match secret_store().map_or(Ok(()), |store| store.delete(ACCOUNT)) {
        Ok(()) => {
            *known = Known::Nothing;
            Ok(())
        }
        Err(SecretError::Unavailable(_)) if !was => {
            *known = Known::Nothing;
            Ok(())
        }
        // Known to be there, or may be: `remembered` reports only what was found.
        Err(e) => {
            *known = if was { Known::Kept { retry_at: i64::MAX } } else { Known::after_failure(&e) };
            Err(NOT_FORGOTTEN)
        }
    }
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

/// Whether Keycloak refused the grant itself: wrong credentials or an expired or revoked refresh
/// token with 400 or 401 `invalid_grant`, a refused client with 401 and its own `error`
/// (`invalid_client`…). Any other answer (a rate limit, a malformed-request 400, a proxy's page,
/// even a 401 one) says nothing of the credentials.
fn refused(status: u16, body: &[u8]) -> bool {
    matches!((status, grant_error(body).as_deref()), (400 | 401, Some("invalid_grant")) | (401, Some(_)))
}

/// Whether Keycloak refused the username and password themselves (`invalid_grant`): only that
/// deletes a remembered sign-in. A refused client (`invalid_client`…) says nothing of them.
#[cfg(not(target_arch = "wasm32"))]
fn wrong_credentials(status: u16, body: &[u8]) -> bool {
    matches!((status, grant_error(body).as_deref()), (400 | 401, Some("invalid_grant")))
}

/// Lands the answer to a sign-in sent elsewhere (the web app's fetch): [`login_body`] POSTed to
/// [`TOKEN_URL`], whose status and answer are given here. Keeps the sign-in as `login` does,
/// replacing the one kept; a refused one leaves it. Never remembers it: the browser has no
/// credential store.
pub fn login_answer(username: &str, status: u16, body: &[u8]) -> Result<Info, String> {
    answered(TOKEN_URL, username, status, body)
}

/// A sign-in's answer from `endpoint`: a server error says nothing of the credentials.
fn answered(endpoint: &str, username: &str, status: u16, body: &[u8]) -> Result<Info, String> {
    if status >= 500 {
        return Err(format!("Copernicus Data Space's sign-in answered HTTP {status}: try again later"));
    }
    keep(endpoint, username, status, body)
}

/// Keeps a sign-in made at `endpoint` from its token answer; what the status tells of it.
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
    // Anything but Keycloak's refusal keeps the sign-in, renewed again later.
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
    refused_message_in(url, cfg!(target_arch = "wasm32"))
}

/// [`refused_message`], for the web app (`web`) or not: the web app can't sign in yet (its panel
/// doesn't offer it), so its message sends the user where they can.
fn refused_message_in(url: &str, web: bool) -> String {
    let how =
        if web { "in the desktop app or the command line (copernicus.login): the web app can't sign in yet" } else { "(copernicus.login)" };
    // An OData address names its file in its last node: `…/Nodes(f.tif)/$value`.
    let last = url.split(['?', '#']).next().unwrap_or(url).trim_end_matches("/$value").rsplit('/').next().unwrap_or("");
    let file = last.strip_prefix("Nodes(").and_then(|n| n.strip_suffix(')')).unwrap_or(last);
    if let Some(info) = signed_in() {
        // Kept, but its token expired and the sign-in host didn't renew it (it holds off a while).
        if info.expires_at <= now_secs() {
            format!(
                "{file}: the Copernicus Data Space sign-in's token expired and couldn't be renewed yet: try again in a minute, or sign in again {how}"
            )
        } else {
            format!("{file}: Copernicus Data Space didn't accept the sign-in's token: sign in again {how}")
        }
    } else if expired() {
        format!("{file}: the Copernicus Data Space sign-in expired: sign in again {how}")
    } else {
        format!("{file}: Copernicus Data Space asks for a sign-in for this file: sign in with a free account {how}")
    }
}

#[cfg(all(feature = "testing", not(target_arch = "wasm32")))]
pub use native::set_test_token_url;
#[cfg(not(target_arch = "wasm32"))]
pub use native::{Restored, authorization_fresh, login, restore};

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;

    use super::Known;

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
    /// With `remember`, the sign-in is also remembered on this computer; without it, one
    /// remembered before is deleted. Returns a warning as `remember_after` does.
    pub fn login(
        username: &str,
        password: &str,
        totp: Option<&str>,
        remember: bool,
    ) -> Result<(super::Info, Option<&'static str>), String> {
        login_remembering_at(&token_url(), username, password, totp, remember)
    }

    pub(crate) fn login_remembering_at(
        endpoint: &str,
        username: &str,
        password: &str,
        totp: Option<&str>,
        remember: bool,
    ) -> Result<(super::Info, Option<&'static str>), String> {
        let mut known = super::lock(&super::REMEMBERED.known);
        let info = login_at(endpoint, username, password, totp)?;
        let totp = totp.is_some_and(|t| !t.trim().is_empty());
        Ok((info, super::remember_after(&mut known, username, password, totp, remember)))
    }

    pub(crate) fn login_at(endpoint: &str, username: &str, password: &str, totp: Option<&str>) -> Result<super::Info, String> {
        sign_in(endpoint, username, password, totp).map_err(|(_, e)| e)
    }

    /// Signs in; the error says whether Keycloak refused the username and password.
    fn sign_in(endpoint: &str, username: &str, password: &str, totp: Option<&str>) -> Result<super::Info, (bool, String)> {
        let body = super::login_body(username, password, totp).map_err(|e| (true, e))?;
        let (status, answer) = post_form(endpoint, &body, &None).map_err(|e| (false, e))?;
        super::answered(endpoint, username, status, &answer).map_err(|e| (super::wrong_credentials(status, &answer), e))
    }

    /// The token endpoint: [`TOKEN_URL`](super::TOKEN_URL), or a test's fake one.
    fn token_url() -> String {
        #[cfg(any(test, feature = "testing"))]
        if let Some(url) = TEST_TOKEN_URL.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            return url;
        }
        super::TOKEN_URL.to_string()
    }

    #[cfg(any(test, feature = "testing"))]
    pub(crate) static TEST_TOKEN_URL: Mutex<Option<String>> = Mutex::new(None);

    /// Points `login` at a fake token endpoint on this computer (`http://127.0.0.1:<port>/…`), or
    /// back at CDSE's with `None`, for other crates' tests (the `testing` feature). Panics on any
    /// other address, so a password is never sent elsewhere.
    #[cfg(feature = "testing")]
    pub fn set_test_token_url(url: Option<&str>) {
        // The host itself, not a prefix: `http://127.0.0.1:@elsewhere/` has 127.0.0.1 as user info.
        let local = |u: &str| {
            let port = u.strip_prefix("http://127.0.0.1:").and_then(|r| r.split('/').next()).unwrap_or("");
            !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit())
        };
        assert!(url.is_none_or(local), "a test token endpoint is on 127.0.0.1");
        *TEST_TOKEN_URL.lock().unwrap_or_else(|e| e.into_inner()) = url.map(str::to_string);
    }

    /// What signing in with the remembered sign-in did.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Restored {
        /// Nothing: no store, nothing remembered, a sign-in already kept, or its retry not due.
        Nothing,
        /// Signed in with it.
        SignedIn,
        /// Copernicus Data Space refused it (a changed password): it was deleted, and the user is
        /// to sign in again.
        Refused,
        /// It couldn't be read (damaged): it was deleted, and the user is to sign in again.
        Damaged,
    }

    /// Signs in with the sign-in remembered on this computer when none is kept: after launch (the
    /// app, off the frame; the command line and Python before a command that names a data host
    /// binds its accounts) and when a data host is asked for after the sign-in ended. When the
    /// sign-in host can't be reached, it is tried again 30 s later. A refused one is deleted.
    /// Reading the store may ask the system to unlock it.
    pub fn restore() -> Restored {
        let mut known = super::lock(&super::REMEMBERED.known);
        let Some(store) = super::secret_store() else { return Restored::Nothing };
        let now = super::now_secs();
        if super::signed_in().is_some()
            || matches!(*known, Known::Nothing | Known::Unreadable)
            || matches!(*known, Known::Kept { retry_at } if now < retry_at)
        {
            return Restored::Nothing;
        }
        let secret = match store.load(super::ACCOUNT) {
            Ok(Some(secret)) => secret,
            Ok(None) => {
                *known = Known::Nothing;
                return Restored::Nothing;
            }
            Err(e) => {
                *known = Known::after_failure(&e);
                return Restored::Nothing;
            }
        };
        let Some((username, password)) = super::read_remembered(secret.expose()) else {
            // Damaged: of no use to anyone. Said as a refused one is: the sign-in ended, sign in again.
            log::warn!("the Copernicus Data Space sign-in remembered on this computer couldn't be read, and was deleted");
            *known = if store.delete(super::ACCOUNT).is_ok() { Known::Nothing } else { Known::Kept { retry_at: i64::MAX } };
            super::state().expired = true;
            return Restored::Damaged;
        };
        match sign_in(&token_url(), &username, &password, None) {
            Ok(_) => {
                *known = Known::Kept { retry_at: 0 };
                Restored::SignedIn
            }
            Err((true, e)) => {
                log::warn!("the Copernicus Data Space sign-in remembered on this computer was refused: {e}");
                *known = if store.delete(super::ACCOUNT).is_ok() { Known::Nothing } else { Known::Kept { retry_at: i64::MAX } };
                super::state().expired = true;
                Restored::Refused
            }
            Err((false, e)) => {
                log::warn!("couldn't sign in to Copernicus Data Space with the sign-in remembered on this computer: {e}");
                // From now: the request may have taken its whole timeout.
                *known = Known::Kept { retry_at: super::now_secs() + super::RETRY_RENEWAL };
                Restored::Nothing
            }
        }
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
        // No sign-in kept: the one remembered on this computer signs in, if there is one.
        if super::signed_in().is_none() {
            restore();
        }
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
pub(crate) use native::{TEST_TOKEN_URL, authorization_fresh_using, login_at, login_remembering_at};

#[cfg(test)]
mod tests;
