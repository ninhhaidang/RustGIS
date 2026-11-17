//! Logins of saved web-service connections (WMS, WMTS, WFS, WCS, OGC API, ArcGIS REST, CSW,
//! STAC): HTTP basic authentication, an API key in a header, an API key in a query parameter, or an
//! ArcGIS token (pasted, or generated from a user name and password: see [`crate::service_tokens`]).
//!
//! The connection keeps the method, user name and header or parameter name, never the password
//! or key. A secret given to `connection.login` lasts for this session, held by connection id in
//! [`crate::connections::DatabaseLogins`], or on this computer too when the user asks to remember
//! it: one opaque secret at the account `connection/<id>`, the JSON
//! `{"version": 1, "origin", "auth", "user", "keyName", "secret"}`. The origin, method, user and header or
//! parameter name travel with the secret, so a connection whose definition changed outside the
//! app can never send it elsewhere or another way. Its runtime identity binds a session secret
//! the same way.
//!
//! Each session/connection login has an opaque request binding. Only requests carrying that
//! binding use its credential, and only at the connection's exact origin. Bindings distinguish
//! authenticated cache entries and are revoked on replacement, logout and session drop. Layers
//! record their saved connection id, never a trusted persisted runtime binding. A project opened
//! without its connection or login asks through the existing login-request queue.
//! Nothing here prints, returns or logs a secret: every failure is
//! a fixed sentence.

use rustgis_data::secrets::{Secret, SecretStore};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::Session;
use crate::connections::{Connection, LoginReason, LoginRequest, SERVICE_KINDS, WEB, account, login_origin};
use crate::render::rustgis_tiles::{arcgis_token, cache, logins};

/// A transient secret whose connection identity changed.
pub const UNUSABLE: &str = "The session login could not be used. Enter it again.";
/// A saved login that can't be used (damaged, or saved for another origin, method, user, header
/// or parameter).
pub const SAVED_UNUSABLE: &str = "The saved login could not be used. Enter it again.";
/// The connection is marked remembered, but the store has nothing for it.
pub const NONE_SAVED: &str = "No saved login for this connection. Enter it again.";
/// There is no credential store, or it refused.
pub const UNAVAILABLE: &str = "The saved login is unavailable. Enter it for this session.";
/// The login worked, the secret wasn't kept on this computer.
pub const NOT_REMEMBERED: &str = "The login is available for this session but was not remembered on this computer.";
/// A saved login couldn't be deleted, so what needed it gone didn't happen.
pub const NOT_FORGOTTEN: &str = "The saved login could not be forgotten. The connection was not changed.";
/// Forget found a login under this connection's id saved for another connection (or by a newer
/// RustGIS), and left it.
pub const OTHERS_LEFT: &str =
    "The login saved under this connection's id belongs to another connection or a newer RustGIS, so it was left unchanged.";
/// Automatic reads respect a deliberate logout until an explicit login.
pub const LOGGED_OUT: &str = "This connection is logged out. Log in explicitly to use it again.";
/// An ArcGIS token past its expiry: a new one is generated or pasted.
pub const TOKEN_EXPIRED: &str = arcgis_token::EXPIRED;
/// A login typed for a plain http:// address without the confirmation.
pub const INSECURE: &str = "This connection's address uses plain http://, so its login would be sent unencrypted. Confirm with allowInsecure: true to send it anyway.";
/// A password or key that can't be sent.
pub const BAD_SECRET: &str = "The password or key must be at most 4096 bytes, without control characters.";
/// An API key a header can't carry the same way everywhere (the browser sends Latin-1 only).
pub const HEADER_ASCII: &str = "An API key sent in a header must use plain ASCII characters.";
/// A login asked of a connection with nothing to log in with this session.
pub const REQUIRED: &str = "This connection's login is required: give its password (basic), apiKey (header, query) or token (arcgis).";
/// A login for a definition that isn't saved.
pub const UNSAVED: &str = "Save the connection first: a login belongs to a saved connection.";
/// An address a login can't go to.
pub const NO_ORIGIN: &str = "The connection's address has no http:// or https:// origin a login can go to.";
/// A Test must not send the edited form's credential to the saved endpoint.
pub const SAVE_BEFORE_TEST: &str = "Save changes before testing this connection.";
/// A bound address whose login is not one of this session's.
pub const NOT_THIS_SESSION: &str = "This request's login does not belong to this session.";
/// Several saved connections match an address and none was chosen.
pub const CHOOSE_CONNECTION: &str = "Several saved connections match this address. Choose a connection explicitly.";
/// A `connection` that is not a saved web-service connection's id.
pub const NO_SUCH_CONNECTION: &str = "No saved web-service connection with this id.";
/// A request's `connection` that isn't text.
pub const CONNECTION_TEXT: &str = "connection must be a saved web-service connection id (text)";
/// The commands that add layers from a service, and so may record the connection they came from
/// (their `connection` parameter, or the connection whose login their address is read with).
pub const ADD_COMMANDS: &[&str] = &[
    "service.add",
    "service.import",
    "service.refresh",
    "stac.search",
    "stac.addAsset",
    "stac.addCube",
    "layer.addUrl",
    "layer.addDownload",
    "map.addTiles",
];

/// The most bytes a stored value may have.
const MAX_STORED: usize = 8192;

#[derive(Serialize)]
struct Stored<'a> {
    version: u32,
    origin: &'a str,
    auth: &'a str,
    user: &'a str,
    #[serde(rename = "keyName")]
    key_name: &'a str,
    secret: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Loaded {
    version: u32,
    origin: String,
    auth: String,
    user: String,
    #[serde(rename = "keyName")]
    key_name: String,
    secret: String,
}

fn encode(c: &Connection, origin: &str, secret: &str) -> Result<Secret, &'static str> {
    let stored = Stored { version: 1, origin, auth: &c.auth, user: &c.user, key_name: &c.key_name, secret };
    let text = serde_json::to_string(&stored).map_err(|_| SAVED_UNUSABLE)?;
    if text.len() > MAX_STORED {
        return Err(SAVED_UNUSABLE);
    }
    Ok(Secret::new(text))
}

/// The secret of a stored value, when it was saved for exactly this origin, method, user and
/// header or parameter name, and can still be sent that way.
fn decode(value: &Secret, c: &Connection, origin: &str) -> Result<Secret, &'static str> {
    let text = value.expose();
    if text.len() > MAX_STORED {
        return Err(SAVED_UNUSABLE);
    }
    // A decoding error would quote the text: its details are dropped.
    let l: Loaded = serde_json::from_str(text).map_err(|_| SAVED_UNUSABLE)?;
    if l.version != 1
        || c.auth.is_empty()
        || l.origin != origin
        || l.auth != c.auth
        || l.user != c.user
        || l.key_name != c.key_name
        || check_login_secret(c, &l.secret).is_err()
    {
        return Err(SAVED_UNUSABLE);
    }
    Ok(Secret::new(l.secret))
}

/// What reading a connection's saved login gave.
pub enum Loading {
    Found(Secret),
    /// The store has none for it.
    Missing,
    /// There is no store, or it refused.
    Unavailable,
    /// What it holds can't be used (another origin, method, user or name, or damaged).
    Unusable,
}

type Store<'a> = Option<&'a Arc<dyn SecretStore>>;

/// Reads the login saved for `c` (the system may ask to unlock its store).
pub fn load(store: Store<'_>, c: &Connection) -> Loading {
    let Some(store) = store else { return Loading::Unavailable };
    let value = match store.load(&account(&c.id)) {
        Ok(Some(v)) => v,
        Ok(None) => return Loading::Missing,
        Err(_) => return Loading::Unavailable,
    };
    let Some(origin) = login_origin(c) else { return Loading::Unusable };
    match decode(&value, c, &origin) {
        Ok(secret) => Loading::Found(secret),
        Err(_) => Loading::Unusable,
    }
}

/// Whose a stored value is, for deciding whether to delete or overwrite it.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Owner {
    /// Saved for this connection's origin, method, user and header or parameter name.
    Mine,
    /// Not a JSON object, or a version-1 web envelope missing an identity field: no one can use
    /// it, so it never blocks this connection.
    Damaged,
    /// Another identity under the same id, a newer RustGIS's envelope, or another kind's
    /// credential (database passwords and S3 keys share the `connection/<id>` account and carry no
    /// `version`): left as it is.
    Other,
}

const FIELDS: [&str; 6] = ["version", "origin", "auth", "user", "keyName", "secret"];

/// Whose `value` is. It never looks at the secret. Every object without this version's `version`
/// and fields (another kind's credential, a newer version's envelope, an unknown shape) is
/// someone else's, never damaged.
fn owner(value: &Secret, c: &Connection, origin: Option<&str>) -> Owner {
    let text = value.expose();
    if text.len() > MAX_STORED {
        return Owner::Other;
    }
    let Ok(Value::Object(o)) = serde_json::from_str::<Value>(text) else { return Owner::Damaged };
    // Database passwords (`kind`, `address`, `password`) and S3 keys (`endpoint`, `accessKeyId`,
    // `secretAccessKey`) have no version, nor would any other shape this version doesn't know.
    if o.get("version").and_then(Value::as_u64) != Some(1) || !o.keys().all(|k| FIELDS.contains(&k.as_str())) {
        return Owner::Other;
    }
    let field = |name: &str| o.get(name).and_then(Value::as_str);
    let (Some(saved_origin), Some(auth), Some(user), Some(key_name)) = (field("origin"), field("auth"), field("user"), field("keyName"))
    else {
        return Owner::Damaged;
    };
    if origin == Some(saved_origin) && auth == c.auth && user == c.user && key_name == c.key_name { Owner::Mine } else { Owner::Other }
}

/// Serializes each check-then-write of a stored login in this process, so two sessions can't
/// both see an empty account and replace each other's. The store has no conditional write, so
/// another process writing the same account can still race this.
static STORE_WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Keeps the login of `c` on this computer, replacing an earlier one of its own or a damaged
/// value; whether it was kept. Another connection's or a newer version's value is left unchanged.
pub fn save(store: Store<'_>, c: &Connection, secret: &str) -> bool {
    let Some(store) = store else { return false };
    if c.id.is_empty() || c.auth.is_empty() {
        return false;
    }
    let Some(origin) = login_origin(c) else { return false };
    let Ok(value) = encode(c, &origin, secret) else { return false };
    let _held = STORE_WRITE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    match store.load(&account(&c.id)) {
        Ok(Some(previous)) if owner(&previous, c, Some(&origin)) == Owner::Other => return false,
        Err(_) => return false,
        _ => {}
    }
    store.save(&account(&c.id), &value).is_ok()
}

/// What deleting the login saved for a web connection did.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Forgetting {
    /// Its own (or a damaged) value was deleted, or there was none.
    Done,
    /// The value under its id is another connection's or a newer RustGIS's: left unchanged.
    Others,
    /// The store couldn't be read or refused the deletion.
    Refused,
}

/// Deletes the login saved for `c` when it is this connection's or damaged. The id alone isn't
/// ownership: another session can number a different connection the same.
pub fn forget_saved(logins: &mut crate::connections::DatabaseLogins, c: &Connection) -> Forgetting {
    let Some(store) = logins.secret_store().cloned() else { return Forgetting::Refused };
    let _held = STORE_WRITE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let value = match store.load(&account(&c.id)) {
        Ok(Some(value)) => value,
        Ok(None) => return Forgetting::Done,
        Err(_) => return Forgetting::Refused,
    };
    if owner(&value, c, login_origin(c).as_deref()) == Owner::Other {
        return Forgetting::Others;
    }
    if logins.delete(&c.id).is_ok() { Forgetting::Done } else { Forgetting::Refused }
}

/// Whether `c` is a web-service connection (one that can log in with a method).
pub fn is_service(c: &Connection) -> bool {
    SERVICE_KINDS.contains(&c.kind.as_str())
}

/// The secret a login of `c` sends, as its method sends it: in a header, or as a query parameter.
fn method(c: &Connection, secret: &str) -> Option<(logins::Method, String)> {
    match c.auth.as_str() {
        "basic" => Some((logins::Method::Header("Authorization".into()), logins::basic(&c.user, secret))),
        "header" => Some((logins::Method::Header(c.key_name.clone()), secret.to_string())),
        "query" => Some((logins::Method::Query(c.key_name.clone()), secret.to_string())),
        "arcgis" => {
            let credential = arcgis_token::Credential::parse(secret).ok()?;
            Some((logins::Method::Arcgis { expires_at: credential.expires_at }, credential.token.expose().to_string()))
        }
        _ => None,
    }
}

/// Checks a typed password or key.
pub fn check_secret(secret: &str) -> Result<(), &'static str> {
    if logins::valid_value(secret) { Ok(()) } else { Err(BAD_SECRET) }
}

/// Checks the complete wire credential before a form edit can invalidate an earlier login.
pub fn check_login_secret(c: &Connection, secret: &str) -> Result<(), String> {
    // The private payload of a token (its JSON wrapper) isn't a password or key: it has its own checks.
    if c.auth == "arcgis" {
        return arcgis_token::Credential::parse(secret).map(drop).map_err(str::to_string);
    }
    check_secret(secret).map_err(str::to_string)?;
    if c.auth == "basic" && !logins::valid_basic(&c.user, secret) {
        return Err("The encoded Basic login exceeds the supported header size or contains invalid characters.".into());
    }
    // The browser's fetch sends a header byte per character (Latin-1) and refuses characters past
    // U+00FF, while the desktop sends UTF-8: only ASCII reaches a server the same way from both.
    if c.auth == "header" && !secret.is_ascii() {
        return Err(HEADER_ASCII.into());
    }
    Ok(())
}

/// A form test owns its login independently of every saved connection and session.
/// Keep this guard alive until the fetch (including its asynchronous callback) has finished.
/// It is deliberately neither cloneable nor printable, and exposes no credential or binding.
#[must_use = "keep the temporary login alive until the service test fetch has finished"]
pub struct TemporaryProbe {
    binding: String,
}

impl Drop for TemporaryProbe {
    fn drop(&mut self) {
        // The networking registry also clears authenticated caches on revocation. Cache reads
        // and responses arriving after this drop recheck the binding and cannot reuse it.
        logins::deactivate(&self.binding);
    }
}

/// Binds an anonymous form-test probe to the login just typed in that form. This never reads
/// saved logins, selects a connection, or retains the typed credential in a session.
///
/// `url` is the address returned by the existing anonymous `connection.test` probe. Only a
/// plain, credential-free address at the cleaned definition's exact origin may carry it.
pub fn temporary_probe(c: &Connection, secret: &str, insecure: bool, url: &str) -> Result<(String, TemporaryProbe), String> {
    const BAD_DEFINITION: &str = "The service test requires a valid web-service connection and login.";
    const BAD_ADDRESS: &str = "The service test requires a credential-free HTTP or HTTPS address at the connection's origin.";
    if !c.id.is_empty() || !is_service(c) {
        return Err(BAD_DEFINITION.into());
    }
    let (c, stripped) = crate::connections::clean(c.clone()).map_err(|_| BAD_DEFINITION.to_string())?;
    if stripped || crate::sources::redact(url) != url {
        return Err(BAD_ADDRESS.into());
    }
    let origin = login_origin(&c).ok_or(BAD_ADDRESS)?;
    if rustgis_data::login_address::is_wrapper(url) || logins::origin(url).as_deref() != Some(origin.as_str()) {
        return Err(BAD_ADDRESS.into());
    }
    if c.auth.is_empty() {
        return Err(BAD_DEFINITION.into());
    }
    if secret.is_empty() {
        return Err(REQUIRED.into());
    }
    check_login_secret(&c, secret)?;
    if origin.starts_with("http://") && !insecure {
        return Err(INSECURE.into());
    }
    let (method, value) = method(&c, secret).ok_or(BAD_DEFINITION)?;
    let guard = TemporaryProbe { binding: logins::activate(&origin, "temporary-probe", method, &value, insecure).map_err(str::to_string)? };
    // Construct the guard before binding: even a too-long URL cannot leak a live login.
    let bound = logins::bind(&guard.binding, url).map_err(|_| BAD_ADDRESS.to_string())?;
    Ok((bound, guard))
}

/// An active login belongs to one connection in one session; the token is not a secret, but
/// saved projects must never treat it as authority to use another session's login.
struct ActiveLogin {
    origin: String,
    binding: String,
    auth: String,
    user: String,
    key_name: String,
}

impl ActiveLogin {
    fn matches(&self, c: &Connection) -> bool {
        login_origin(c).as_deref() == Some(self.origin.as_str())
            && self.auth == c.auth
            && self.user == c.user
            && self.key_name == c.key_name
    }
}

pub struct SessionLogins {
    session: u64,
    active: BTreeMap<String, ActiveLogin>,
    /// A Test of a definition that isn't saved, or a plugin's command, is running: what it asks
    /// for carries no login, whichever saved connection shares its origin.
    anonymous: bool,
    /// Connections logged out this session: their saved login isn't read again until an explicit
    /// `connection.login`.
    logged_out: BTreeSet<String>,
    /// Why a saved login couldn't be read this session (a fixed sentence): the store isn't asked
    /// again, nor is it said again, until an explicit `connection.login`.
    failed: BTreeMap<String, &'static str>,
    /// Connections whose saved-login failure resolution has said already.
    said: BTreeSet<String>,
    /// Token generations under way, by connection id (see [`crate::service_tokens`]).
    pub(crate) pending: BTreeMap<String, crate::service_tokens::Pending>,
}

impl SessionLogins {
    pub fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self {
            session: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            active: BTreeMap::new(),
            anonymous: false,
            logged_out: BTreeSet::new(),
            failed: BTreeMap::new(),
            said: BTreeSet::new(),
            pending: BTreeMap::new(),
        }
    }

    /// Starts or ends [`SessionLogins::anonymous`] reads, saying what it was.
    pub(crate) fn set_anonymous(&mut self, on: bool) -> bool {
        std::mem::replace(&mut self.anonymous, on)
    }

    /// Whether [`SessionLogins::anonymous`] reads are running: nothing may pick a login for them.
    pub(crate) fn is_anonymous(&self) -> bool {
        self.anonymous
    }

    /// The immutable runtime owner used by the networking crate's private account snapshot.
    pub(crate) fn session_id(&self) -> u64 {
        self.session
    }

    /// Sorted connection ids only; bearer bindings never enter the policy's JSON projection.
    pub(crate) fn active_ids(&self) -> Vec<&str> {
        self.active.keys().map(String::as_str).collect()
    }

    fn owner(&self, id: &str) -> String {
        format!("{}/{id}", self.session)
    }
}

impl Default for SessionLogins {
    fn default() -> Self {
        Self::new()
    }
}

/// Runs `f` with anonymous reads `on` (or off), and puts the earlier setting back even when `f`
/// panics: `Session::run` catches the panic and the session goes on, so a setting left behind
/// would silently send later requests without their login.
pub(crate) fn anonymously<T>(s: &mut Session, on: bool, f: impl FnOnce(&mut Session) -> T) -> std::thread::Result<T> {
    let was = s.web_logins.set_anonymous(on);
    let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(s)));
    s.web_logins.set_anonymous(was);
    out
}

impl Drop for SessionLogins {
    fn drop(&mut self) {
        let mut ended = false;
        for active in self.active.values() {
            ended |= logins::deactivate(&active.binding);
        }
        for op in self.pending.values() {
            op.cancel();
        }
        if ended {
            clear_cache();
        }
    }
}

#[cfg(test)]
thread_local! {
    static CLEARS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// How many times this thread's logins dropped the tile cache (tests count it per thread, since
/// the cache itself is shared by every test running at once).
#[cfg(test)]
pub(crate) fn cache_clears() -> u32 {
    CLEARS.with(std::cell::Cell::get)
}

/// Drops cached responses: a login that ends or is replaced leaves authenticated ones behind.
fn clear_cache() {
    #[cfg(test)]
    CLEARS.with(|c| c.set(c.get() + 1));
    cache::clear();
}

/// Replaces only this connection's binding, after the complete new credential has validated.
/// Other sessions and other connections at this origin retain their own bindings.
/// Returns the number of addresses that could not be rebound; resolution reports it once.
pub(crate) fn activate(s: &mut Session, c: &Connection, secret: Option<&Secret>, insecure: bool) -> Result<usize, String> {
    let origin = login_origin(c).ok_or(NO_ORIGIN)?;
    let next = match secret {
        Some(secret) => {
            let (method, value) = method(c, secret.expose()).ok_or(UNUSABLE)?;
            let binding = logins::activate(&origin, &s.web_logins.owner(&c.id), method, &value, insecure).map_err(str::to_string)?;
            Some(ActiveLogin { origin: origin.clone(), binding, auth: c.auth.clone(), user: c.user.clone(), key_name: c.key_name.clone() })
        }
        None => None,
    };
    let replaced = s.web_logins.active.remove(&c.id).map(|old| logins::deactivate(&old.binding)).is_some();
    if let Some(next) = next {
        s.web_logins.active.insert(c.id.clone(), next);
        if let Some(secret) = secret {
            s.database_logins.set_session_password(&c.id, secret.expose());
        }
    }
    retry(s, &c.id);
    let failed = reselect(s, c, &origin);
    // Only a login that was replaced or removed leaves cached authenticated responses to drop;
    // a first login, or none, changes no cached tile.
    if replaced {
        clear_cache();
    }
    Ok(failed)
}

/// Chooses `c` for its origin again and rebinds its layers, keeping its live binding (and so
/// its cached tiles): logging in again to a connection that is already logged in.
/// Returns the full connection's failed-address count for resolution to report.
pub(crate) fn reselect(s: &mut Session, c: &Connection, origin: &str) -> usize {
    s.database_logins.select(WEB, origin, &c.id);
    s.database_logins.clear_requests_for(WEB, origin);
    rebind_connection(s, c)
}

pub(crate) fn deactivate(s: &mut Session, c: &Connection, _origin: Option<String>) {
    if let Some(old) = s.web_logins.active.remove(&c.id) {
        logins::deactivate(&old.binding);
        clear_cache();
    }
    if let Some(origin) = login_origin(c)
        && s.database_logins.selection(WEB, &origin) == Some(c.id.as_str())
    {
        s.database_logins.drop_selection(WEB, &origin);
    }
}

/// Ends this session's login and keeps it ended: a login saved on this computer is read again
/// only by an explicit `connection.login`.
pub(crate) fn logout(s: &mut Session, c: &Connection) {
    forget(s, c);
    s.web_logins.logged_out.insert(c.id.clone());
}

/// Explicit login is the retry boundary for a failed store read or a deliberate logout.
pub(crate) fn retry(s: &mut Session, id: &str) {
    s.web_logins.failed.remove(id);
    s.web_logins.logged_out.remove(id);
    s.web_logins.said.remove(id);
}

/// Forgets a failed read of the saved login of `id` and that it was said, after the saved login
/// is deleted: a blank login then asks for the login instead of repeating the old failure. A
/// logout stays in place.
pub(crate) fn forget_failure(s: &mut Session, id: &str) {
    s.web_logins.failed.remove(id);
    s.web_logins.said.remove(id);
}

/// Ends this session's login of `c` and its session secret (its definition changed or it was
/// removed). The login saved on this computer is the caller's to delete.
pub(crate) fn forget(s: &mut Session, c: &Connection) {
    deactivate(s, c, None);
    crate::service_tokens::cancel_for(s, &c.id);
    retry(s, &c.id);
    s.database_logins.forget_connection(&c.id);
}

pub fn logged_in(s: &Session, c: &Connection) -> bool {
    s.web_logins.active.get(&c.id).is_some_and(|a| a.matches(c) && logins::is_active(&a.binding))
}

/// Whether this connection was deliberately logged out this session. Opening its sources must
/// ask for an explicit login rather than automatically reloading its saved credential.
pub fn logged_out(s: &Session, c: &Connection) -> bool {
    s.web_logins.logged_out.contains(&c.id)
}

/// A plain address at the origin of a logged-in service connection, bound to that login. Anything
/// else (another origin, a wrapper, a connection not logged in) comes back unchanged. An address
/// at that origin that can't carry the login is refused, never read without it; so is one a
/// plugin sets there.
pub(crate) fn bind_logged_in(s: &Session, connection: &str, url: &str) -> Result<String, String> {
    let Some(c) = s.connections.connections.iter().find(|c| c.id == connection && is_service(c)) else { return Ok(url.to_string()) };
    let Some(origin) = login_origin(c) else { return Ok(url.to_string()) };
    // At the origin by its text too: an address that doesn't parse (a space in it) still names it.
    let named = url.get(..origin.len()).is_some_and(|start| start.eq_ignore_ascii_case(&origin))
        && url.get(origin.len()..).is_some_and(|rest| rest.is_empty() || rest.starts_with(['/', '?', '#']));
    let at_origin = named || logins::origin(url).as_deref() == Some(origin.as_str());
    if !at_origin || rustgis_data::login_address::is_wrapper(url) {
        return Ok(url.to_string());
    }
    // Even a public connection can gain a login later. A plugin must not leave a legend for
    // that login to bind.
    if s.web_logins.is_anonymous() {
        return Err(format!("plugins can't set a legend at the address of a saved web-service connection ('{}')", c.name));
    }
    if !logged_in(s, c) {
        // An expired token is refused, never read anonymously (or with a portal sign-in's token).
        if c.auth == "arcgis"
            && let Some(active) = s.web_logins.active.get(&c.id).filter(|a| a.matches(c))
        {
            logins::bind(&active.binding, url).map_err(str::to_string)?;
        }
        // The expiry stands until a new login replaces the token: nothing falls back to anonymous.
        if c.auth == "arcgis" && s.web_logins.failed.get(&c.id).copied() == Some(TOKEN_EXPIRED) {
            return Err(TOKEN_EXPIRED.into());
        }
        return Ok(url.to_string());
    }
    let Some(active) = s.web_logins.active.get(&c.id) else { return Ok(url.to_string()) };
    logins::bind(&active.binding, url).map_err(|_| {
        crate::sources::message_with_address(
            url,
            "this address can't carry the connection's login (it has a space, a control character or a backslash)",
        )
    })
}

/// Whether `secret` is an ArcGIS token of `c` that is past its expiry (or expires on a clock that
/// can't be read). What can't be read as a token isn't judged here: activating it refuses it.
fn lapsed(c: &Connection, secret: &Secret) -> bool {
    c.auth == "arcgis" && arcgis_token::Credential::parse(secret.expose()).is_ok_and(|token| token.is_expired(arcgis_token::now_millis()))
}

/// Ends this session's use of an expired ArcGIS token of `c`: its login generation is revoked
/// (cached authenticated responses go with it), its session credential is cleared, and the
/// expiry stands in for the saved login's automatic reads until an explicit login, so the store
/// isn't asked for the same expired payload again. The saved login itself is left as it is.
/// The connection stays chosen for its origin, so asking for a new login finds it.
fn expire(s: &mut Session, c: &Connection) -> String {
    if let Some(old) = s.web_logins.active.remove(&c.id) {
        logins::deactivate(&old.binding);
        clear_cache();
    }
    s.database_logins.clear_session_password(&c.id);
    s.web_logins.failed.insert(c.id.clone(), TOKEN_EXPIRED);
    TOKEN_EXPIRED.into()
}

/// The secret `c` logs in with without one typed: the one given this session, else the one
/// saved on this computer (read once: a failure is kept, and said, until an explicit login).
/// Nothing is read for a connection logged out this session. An expired ArcGIS token is neither
/// used nor replaced by another: it needs a new one.
pub fn known_secret(s: &mut Session, c: &Connection) -> Result<Secret, String> {
    if s.web_logins.logged_out.contains(&c.id) {
        return Err(LOGGED_OUT.into());
    }
    if let Some(secret) = s.database_logins.session_password(&c.id).cloned() {
        if !s.web_logins.active.get(&c.id).is_some_and(|a| a.matches(c)) {
            return Err(UNUSABLE.into());
        }
        if lapsed(c, &secret) {
            return Err(expire(s, c));
        }
        return Ok(secret);
    }
    if let Some(error) = s.web_logins.failed.get(&c.id) {
        return Err((*error).into());
    }
    if !c.remembered || c.auth.is_empty() {
        return Err(REQUIRED.into());
    }
    let error = match load(s.database_logins.secret_store(), c) {
        Loading::Found(secret) if lapsed(c, &secret) => TOKEN_EXPIRED,
        Loading::Found(secret) => return Ok(secret),
        Loading::Missing => {
            s.connections.set_remembered(&c.id, false);
            NONE_SAVED
        }
        Loading::Unavailable => UNAVAILABLE,
        Loading::Unusable => SAVED_UNUSABLE,
    };
    s.web_logins.failed.insert(c.id.clone(), error);
    Err(error.into())
}

/// Binds a request to a connection chosen in this session. A bare address can resolve to the
/// explicit session choice or sole saved definition, never the first of several matches.
pub fn request(s: &mut Session, connection: Option<&str>, url: &str) -> Result<String, String> {
    let plain = logins::inner(url);
    let Some(origin) = logins::origin(plain) else { return Ok(plain.to_string()) };
    if s.web_logins.anonymous && connection.is_none() {
        return Ok(plain.to_string());
    }
    if plain != url && connection.is_none() {
        let token = rustgis_data::login_address::parse(url).ok().flatten().map(|b| b.binding);
        if s.web_logins.active.values().any(|a| token == Some(a.binding.as_str())) {
            logins::check(url).map_err(str::to_string)?;
            return Ok(url.to_string());
        }
        return Err(NOT_THIS_SESSION.into());
    }
    let candidates: Vec<_> =
        s.connections.connections.iter().filter(|c| is_service(c) && login_origin(c).as_deref() == Some(origin.as_str())).collect();
    let chosen = if let Some(id) = connection {
        let c = s.connections.connections.iter().find(|c| c.id == id && is_service(c)).ok_or(NO_SUCH_CONNECTION)?;
        // A service may link to another origin, but never grants it this login.
        if login_origin(c).as_deref() != Some(origin.as_str()) {
            return Ok(plain.to_string());
        }
        Some(c.clone())
    // A choice naming a connection no longer here (or no longer at this origin) is no choice:
    // the sole connection left still lends its login.
    } else if let Some(c) = s.database_logins.selection(WEB, &origin).and_then(|id| candidates.iter().find(|c| c.id == id)) {
        Some((*c).clone())
    } else if candidates.len() == 1 {
        candidates.first().map(|c| (*c).clone())
    } else {
        None
    };
    let Some(c) = chosen else {
        // Several connections, at least one with a login: the user picks. Public connections
        // alone all send the same anonymous request, so there is nothing to ask.
        if candidates.len() > 1 && candidates.iter().any(|c| !c.auth.is_empty()) {
            s.database_logins.request(LoginRequest {
                reason: LoginReason::Choose,
                kind: WEB,
                key: origin,
                connections: candidates.iter().map(|c| c.id.clone()).collect(),
                layers: Vec::new(),
                tiles: 0,
                opened: s.opened,
                prompted: false,
            });
            return Err(CHOOSE_CONNECTION.into());
        }
        return Ok(plain.to_string());
    };
    if c.auth.is_empty() {
        return Ok(plain.to_string());
    }
    if !logged_in(s, &c) {
        // Confirmed already: a web connection's session secret is kept only after its login passed
        // the plain-http:// check (`connection.login`), and is forgotten when the connection's
        // server changes (`connection.update`), so it never reaches a server nobody confirmed.
        let secret = known_secret(s, &c)?;
        activate(s, &c, Some(&secret), true)?;
    }
    let active = s.web_logins.active.get(&c.id).ok_or(REQUIRED)?;
    logins::bind(&active.binding, plain).map_err(str::to_string)
}

/// Opens a service using its existing or remembered login without retrying a failed store read
/// or undoing logout. A pending project connection choice is adopted only after success.
pub fn open_connection(s: &mut Session, c: &Connection) -> Result<(), String> {
    let answers = answers(s, c);
    request(s, Some(&c.id), &c.address)?;
    if answers && let Some(origin) = login_origin(c) {
        reselect(s, c, &origin);
        adopt(s, c);
        resolve(s, Some(&BTreeSet::from([origin])));
    }
    Ok(())
}

pub fn request_params<'a>(s: &mut Session, p: &'a Value) -> Result<std::borrow::Cow<'a, Value>, String> {
    let Some(url) = p.get("url").and_then(Value::as_str) else { return Ok(std::borrow::Cow::Borrowed(p)) };
    let id = match p.get("connection") {
        None | Some(Value::Null) => None,
        Some(Value::String(id)) if id.trim().is_empty() => None,
        Some(Value::String(id)) => Some(id.as_str()),
        Some(_) => return Err(CONNECTION_TEXT.into()),
    };
    if id.is_none() && (s.web_logins.anonymous || p.get("anonymous").and_then(Value::as_bool) == Some(true)) {
        // An address bound to a login is no authority for it here either.
        let plain = logins::inner(url);
        if plain == url {
            return Ok(std::borrow::Cow::Borrowed(p));
        }
        let mut owned = p.clone();
        if let Some(address) = owned.get_mut("url") {
            *address = Value::String(plain.to_string());
        }
        return Ok(std::borrow::Cow::Owned(owned));
    }
    let bound = request(s, id, url)?;
    // Carry the originating connection into internal add/download commands too. A linked
    // foreign URL must not silently select a different origin's session login.
    let implicit = if id.is_none() {
        let origin = logins::origin(url);
        let token = rustgis_data::login_address::parse(&bound).ok().flatten().map(|b| b.binding);
        s.web_logins
            .active
            .iter()
            .find(|(_, a)| token == Some(a.binding.as_str()))
            .map(|(id, _)| id.as_str())
            .or_else(|| origin.as_deref().and_then(|o| s.database_logins.selection(WEB, o)))
            .or_else(|| {
                let mut matches = s.connections.connections.iter().filter(|c| is_service(c) && login_origin(c) == origin);
                let first = matches.next()?;
                matches.next().is_none().then_some(first.id.as_str())
            })
    } else {
        None
    };
    if bound == url && implicit.is_none() {
        return Ok(std::borrow::Cow::Borrowed(p));
    }
    let mut owned = p.clone();
    *owned.get_mut("url").ok_or("url is required")? = Value::String(bound);
    if let Some(id) = implicit
        && let Some(object) = owned.as_object_mut()
    {
        object.insert("connection".into(), Value::String(id.to_string()));
    }
    Ok(std::borrow::Cow::Owned(owned))
}

/// The web address a tile layer reads (a tile template's archive for a PMTiles or COG template).
fn tile_address(url: &str) -> &str {
    crate::sources::template_archive(url).map_or(url, |(_, archive)| archive)
}

/// The web address a layer reads again later (a streamed source or a raster read in place).
fn layer_address(l: &rustgis_map::Layer) -> Option<&str> {
    l.source.as_ref().and_then(|s| s.path.as_deref()).or_else(|| l.raster.as_ref().and_then(|r| r.path.as_deref()))
}

/// Rebinds only addresses belonging to this connection. Cross-origin links stay anonymous.
/// Keeps failed addresses unchanged and returns their count without reporting it mid-activation.
fn rebind_connection(s: &mut Session, c: &Connection) -> usize {
    let binding = s.web_logins.active.get(&c.id).map(|a| a.binding.as_str());
    let origin = login_origin(c);
    let wait = s.in_place_wait;
    let mut failed = 0usize;
    let mut rewrite = |address: &mut String| {
        let (prefix, raw) = crate::sources::template_archive(address).unwrap_or(("", address.as_str()));
        let plain = logins::inner(raw);
        let next = if let Some(binding) = binding
            && logins::origin(plain) == origin
        {
            match logins::bind(binding, plain) {
                Ok(next) => next,
                Err(_) => {
                    failed += 1;
                    return false;
                }
            }
        } else {
            plain.to_string()
        };
        let next = if prefix.is_empty() { next } else { format!("{prefix}{next}") };
        if *address != next {
            *address = next;
            true
        } else {
            false
        }
    };
    for m in &mut s.project.maps {
        for t in m.tiles.iter_mut().filter(|t| !t.anonymous && t.connection == c.id) {
            rewrite(&mut t.url);
            if let Some(url) = &mut t.time_url {
                rewrite(url);
            }
            for (_, url) in &mut t.time_urls {
                rewrite(url);
            }
            for legend in &mut t.legends {
                rewrite(&mut legend.url);
            }
            if let Some(info) = &mut t.feature_info {
                rewrite(&mut info.url);
            }
        }
        for l in m.layers.iter_mut().filter(|l| !l.anonymous && l.connection == c.id) {
            if let Some(source) = l.source.as_mut()
                && let Some(path) = source.path.as_mut()
            {
                let changed = rewrite(path);
                // The changed address deserves a new try: an open source still reads through the
                // old binding, so it opens afresh (a failure is kept in its `error`, as any open's
                // is); one not open yet, or whose open failed, opens on the next sync.
                if changed {
                    if source.is_open() {
                        let _ = source.reopen();
                    } else {
                        source.clear_error();
                    }
                }
            }
            if let Some(raster) = l.raster.as_mut()
                && let Some(path) = raster.path.as_mut()
                && rewrite(path)
            {
                // Reload keeps the style and publishes any failure through raster.error. A layer
                // read in place lands as the map draws where the session doesn't wait for reads
                // in place (the desktop), so a login made again doesn't hold the window.
                let _ = if raster.in_place && !wait { raster.begin_reload() } else { raster.reload() };
            }
            if let Some(path) = &mut l.dataset.source {
                rewrite(path);
            }
        }
        for group in m.service_groups.iter_mut().filter(|g| !g.anonymous) {
            let ids: BTreeSet<_> = group
                .children
                .iter()
                .filter_map(|child| m.layers.iter().find(|l| l.id == child.layer && !l.anonymous).map(|l| l.connection.as_str()))
                .collect();
            if ids.len() == 1 && ids.contains(c.id.as_str()) {
                rewrite(&mut group.url);
                for child in &mut group.children {
                    rewrite(&mut child.source_url);
                }
            }
        }
    }
    failed
}

fn record_choice(s: &mut Session, origin: &str, old: &str, c: &Connection) {
    for m in &mut s.project.maps {
        for t in &mut m.tiles {
            if !t.anonymous && t.connection == old && logins::origin(tile_address(&t.url)).as_deref() == Some(origin) {
                t.connection = c.id.clone();
            }
        }
        let group_layers: BTreeSet<_> = m
            .service_groups
            .iter()
            .filter(|g| !g.anonymous && logins::origin(&g.url).as_deref() == Some(origin))
            .flat_map(|g| g.children.iter().map(|child| child.layer))
            .collect();
        for l in &mut m.layers {
            if !l.anonymous
                && l.connection == old
                && (layer_address(l).and_then(logins::origin).as_deref() == Some(origin) || group_layers.contains(&l.id))
            {
                l.connection = c.id.clone();
            }
        }
    }
}

/// What a project reads from one origin again later: its layers and tile layers, and the saved
/// connections they record.
#[derive(Default)]
struct Reads {
    recorded: BTreeSet<String>,
    layers: Vec<u32>,
    tiles: usize,
    layer_keys: BTreeSet<(usize, u32)>,
}

/// The origins the project reads from, with what reads each.
fn reads(s: &Session) -> BTreeMap<(String, String), Reads> {
    let mut out = BTreeMap::<(String, String), Reads>::new();
    for (map_index, m) in s.project.maps.iter().enumerate() {
        for t in m.tiles.iter().filter(|t| !t.anonymous) {
            let recorded_origin = s.connections.connections.iter().find(|c| c.id == t.connection && is_service(c)).and_then(login_origin);
            // A dated link on another origin still belongs to the recorded connection, not to
            // that other origin's saved login. If the connection is gone, only the displayed
            // address is evidence for the request: do not guess which dated origin owned it.
            let origin = if let Some(recorded) = recorded_origin {
                std::iter::once(t.url.as_str())
                    .chain(t.time_url.as_deref())
                    .chain(t.time_urls.iter().map(|(_, url)| url.as_str()))
                    .any(|url| logins::origin(tile_address(url)).as_deref() == Some(recorded.as_str()))
                    .then_some(recorded)
                    .or_else(|| logins::origin(tile_address(&t.url)))
            } else {
                logins::origin(tile_address(&t.url))
            };
            let Some(origin) = origin else { continue };
            let r = out.entry((origin, t.connection.clone())).or_default();
            r.tiles += 1;
            if !t.connection.is_empty() {
                r.recorded.insert(t.connection.clone());
            }
        }
        for l in m.layers.iter().filter(|l| !l.anonymous) {
            let Some(origin) = layer_address(l).and_then(logins::origin) else { continue };
            let r = out.entry((origin, l.connection.clone())).or_default();
            if r.layer_keys.insert((map_index, l.id)) {
                r.layers.push(l.id);
            }
            if !l.connection.is_empty() {
                r.recorded.insert(l.connection.clone());
            }
        }
        // An imported service's group is read again by a refresh, from its address: it logs in
        // with the connection its layers record.
        for g in m.service_groups.iter().filter(|g| !g.anonymous) {
            let Some(origin) = logins::origin(&g.url) else { continue };
            for child in &g.children {
                let layer = m.layers.iter().find(|l| l.id == child.layer);
                if layer.is_some_and(|l| l.anonymous) {
                    continue;
                }
                let id = layer.map(|l| l.connection.as_str()).unwrap_or("");
                let r = out.entry((origin.clone(), id.to_string())).or_default();
                if r.layer_keys.insert((map_index, child.layer)) {
                    r.layers.push(child.layer);
                }
                if !id.is_empty() {
                    r.recorded.insert(id.to_string());
                }
            }
        }
    }
    out
}

/// Matches what the project reads from each origin (every origin, or those `only` names) to the
/// saved connections of that origin, and logs in with the one that is clear: the connection the
/// layers record, the one chosen this session, or the only one. Several, a missing login, or a
/// recorded connection this computer doesn't have are asked about ([`LoginRequest`]), never
/// guessed.
pub(crate) fn resolve(s: &mut Session, only: Option<&BTreeSet<String>>) {
    // Imports and nested commands a plugin runs must not adopt a login before their additions
    // have been marked anonymous at the plugin boundary.
    if s.web_logins.is_anonymous() {
        return;
    }
    let mut pending = Vec::new();
    let mut touched = BTreeSet::new();
    let mut rebind_failures = BTreeMap::new();
    for ((origin, recorded_id), r) in reads(s) {
        if only.is_some_and(|only| !only.contains(&origin)) {
            continue;
        }
        touched.insert(origin.clone());
        let candidates: Vec<Connection> = s
            .connections
            .connections
            .iter()
            .filter(|c| is_service(c) && login_origin(c).as_deref() == Some(origin.as_str()))
            .cloned()
            .collect();
        // The recorded ids that are connections of this origin (looked up by id only: a name in a
        // project is no one's choice).
        let recorded: Vec<&Connection> = candidates.iter().filter(|c| r.recorded.contains(&c.id)).collect();
        let selected = s.database_logins.selection(WEB, &origin).and_then(|id| candidates.iter().find(|c| c.id == id));
        let chosen = match recorded.as_slice() {
            [only] => Some(*only),
            // A recorded connection this computer doesn't have is asked about: not replaced by
            // the origin's only connection, nor by an earlier choice (see [`adopt`]).
            [] if !r.recorded.is_empty() => None,
            [] => selected.or_else(|| (candidates.len() == 1).then(|| candidates.first()).flatten()),
            _ => None,
        };
        let chosen = chosen.cloned();
        let opened = s.opened;
        let request = |reason, connections: Vec<String>| LoginRequest {
            reason,
            kind: WEB,
            key: origin.clone(),
            connections,
            layers: r.layers.clone(),
            tiles: r.tiles,
            opened,
            prompted: false,
        };
        let Some(c) = chosen else {
            if (candidates.len() > 1 || recorded.len() > 1) && candidates.iter().any(|c| !c.auth.is_empty()) {
                // Several connections for the address, at least one with a login, and none of them
                // clear: the user picks. Public ones alone read the same anonymously.
                let ids: Vec<String> = if recorded.len() > 1 {
                    recorded.iter().map(|c| c.id.clone()).collect()
                } else {
                    candidates.iter().map(|c| c.id.clone()).collect()
                };
                pending.push(request(LoginReason::Choose, ids));
            } else if recorded.is_empty() && !r.recorded.is_empty() {
                // Layers came from a connection this computer doesn't have, or whose address has
                // since moved to another origin (a layer records only a connection of its origin).
                pending.push(request(LoginReason::Connection, Vec::new()));
            }
            continue;
        };
        if recorded_id != c.id {
            record_choice(s, &origin, &recorded_id, &c);
        }
        let rebound = if c.auth.is_empty() || logged_in(s, &c) {
            Ok(rebind_connection(s, &c))
        } else {
            // A session or saved secret was confirmed for this exact origin when it was typed.
            known_secret(s, &c).and_then(|secret| activate(s, &c, Some(&secret), true))
        };
        match rebound {
            Ok(failed) => {
                // Each rebind covers the whole connection, not only this read entry.
                if failed > 0 {
                    rebind_failures.insert(c.id, (c.name, failed));
                } else {
                    rebind_failures.remove(&c.id);
                }
            }
            Err(e) => {
                let already_asked = s
                    .database_logins
                    .requests(s.opened)
                    .any(|r| r.kind == WEB && r.key == origin && r.connections == [c.id.clone()] && r.reason == LoginReason::Password);
                // A saved login that can't be read is said once per session (the failure is kept
                // until an explicit login); a missing login or a deliberate logout only asks.
                let saved_failure = [NONE_SAVED, UNAVAILABLE, SAVED_UNUSABLE, TOKEN_EXPIRED].contains(&e.as_str());
                let say =
                    if saved_failure { s.web_logins.said.insert(c.id.clone()) } else { e != REQUIRED && e != LOGGED_OUT && !already_asked };
                if say {
                    s.notify(format!("{}: {e}", c.name), true);
                }
                pending.push(request(LoginReason::Password, vec![c.id.clone()]));
            }
        }
    }
    for (_, (name, failed)) in rebind_failures {
        if failed > 0 {
            s.notify(format!("{name}: {failed} address(es) could not carry the service login; their previous addresses were kept."), true);
        }
    }
    for origin in touched {
        if !pending.iter().any(|request| request.key == origin) {
            s.database_logins.clear_requests_for(WEB, &origin);
        }
    }
    for request in pending {
        ask(s, request);
    }
}

/// Whether logging in to `c` answers a request waiting at its origin: the user choosing a
/// connection for layers whose recorded one isn't here. Asked before the login, which ends it.
pub(crate) fn answers(s: &Session, c: &Connection) -> bool {
    let Some(origin) = login_origin(c) else { return false };
    s.database_logins.requests(s.opened).any(|r| {
        r.kind == WEB
            && r.key == origin
            && (r.reason == LoginReason::Connection || r.reason == LoginReason::Choose && r.connections.contains(&c.id))
    })
}

/// Gives `c` to the layers of its origin that record a connection this computer doesn't have for
/// that origin: the answer to a request ([`answers`]), never a guess.
pub(crate) fn adopt(s: &mut Session, c: &Connection) {
    let Some(origin) = login_origin(c) else { return };
    let here: BTreeSet<String> = s
        .connections
        .connections
        .iter()
        .filter(|k| is_service(k) && login_origin(k).as_deref() == Some(origin.as_str()))
        .map(|k| k.id.clone())
        .collect();
    let gone: BTreeSet<String> =
        reads(s).into_keys().filter(|(o, id)| *o == origin && !id.is_empty() && !here.contains(id)).map(|(_, id)| id).collect();
    for old in gone {
        record_choice(s, &origin, &old, c);
    }
}

/// Adds a request for the app to ask, saying it once (for the command line and scripts too).
fn ask(s: &mut Session, request: LoginRequest) {
    let known = s
        .database_logins
        .requests(s.opened)
        .any(|r| r.kind == request.kind && r.key == request.key && r.reason == request.reason && r.connections == request.connections);
    if !known {
        let n = request.layers.len() + request.tiles;
        let names: Vec<String> = request
            .connections
            .iter()
            .filter_map(|id| s.connections.connections.iter().find(|c| &c.id == id))
            .map(|c| format!("'{}' ({})", c.name, c.id))
            .collect();
        let text = match request.reason {
            LoginReason::Choose => format!(
                "{n} layer(s) read {} and several saved connections match it: {}. Choose one with connection.login {{id}}.",
                request.key,
                names.join(", ")
            ),
            LoginReason::Password => {
                format!(
                    "{n} layer(s) read {} and need the login of the connection {}: connection.login {{id, …}}.",
                    request.key,
                    names.join(", ")
                )
            }
            LoginReason::Connection => format!(
                "{n} layer(s) read {} with a saved connection this computer doesn't have: add a connection for it (connection.new) and log in.",
                request.key
            ),
        };
        s.notify(text, false);
    }
    s.database_logins.request(request);
}

/// What the project held before an add command: its layers by live identity, tile layers and
/// service groups, so even an empty group added by a plugin retains anonymous provenance.
pub(crate) struct Before {
    layers: Vec<BTreeSet<u64>>,
    tiles: Vec<Vec<(String, String, Vec<String>)>>,
    groups: Vec<BTreeSet<u32>>,
}

/// The connection an add command names (its `connection`), checked before it runs: a saved
/// web-service connection, by id.
fn named(s: &Session, params: &Value) -> Result<Option<Connection>, String> {
    match params.get("connection") {
        // Blank is no connection, as `request_params` reads it.
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(id)) if id.trim().is_empty() => Ok(None),
        Some(Value::String(id)) => {
            let c = s.connections.connections.iter().find(|c| &c.id == id).filter(|c| is_service(c)).ok_or(NO_SUCH_CONNECTION)?;
            Ok(Some(c.clone()))
        }
        Some(_) => Err(CONNECTION_TEXT.into()),
    }
}

/// What to compare after command `id` runs, if it adds layers; an error for a `connection` that
/// isn't a saved web-service connection's id.
pub(crate) fn before(s: &Session, id: &str, params: &Value) -> Result<Option<Before>, String> {
    let anonymous = s.web_logins.is_anonymous();
    if !anonymous && !ADD_COMMANDS.contains(&id) {
        return Ok(None);
    }
    if !anonymous {
        named(s, params)?;
    }
    Ok(Some(Before {
        layers: s.project.maps.iter().map(|m| m.layers.iter().map(|l| l.instance_id()).collect()).collect(),
        groups: if anonymous {
            s.project.maps.iter().map(|m| m.service_groups.iter().map(|g| g.id).collect()).collect()
        } else {
            Vec::new()
        },
        tiles: s
            .project
            .maps
            .iter()
            .map(|m| {
                m.tiles
                    .iter()
                    .map(|t| {
                        let legends = if anonymous { t.legends.iter().map(|legend| legend.url.clone()).collect() } else { Vec::new() };
                        (t.url.clone(), t.name.clone(), legends)
                    })
                    .collect()
            })
            .collect(),
    }))
}

/// Records on the layers an add command made the connection they came from: the one it names
/// (`connection`) for those read from its origin or holding no address, else the one whose login
/// their origin is read with. Then logs in for their origins as a project's layers do.
/// Anonymous plugin additions instead keep a persisted opt-out from all later login adoption.
pub(crate) fn tag(s: &mut Session, params: &Value, before: Before, out: &Value) -> Result<(), String> {
    let anonymous = s.web_logins.is_anonymous();
    let named = if anonymous { None } else { named(s, params)? };
    let named_origin = named.as_ref().and_then(login_origin);
    let mut origins = BTreeSet::new();
    // The connection a layer of `origin` (none: it holds no address) records.
    let pick = |origin: Option<&String>| -> Option<String> {
        match (&named, origin) {
            (Some(c), None) => Some(c.id.clone()),
            (Some(c), Some(o)) if named_origin.as_ref() == Some(o) => Some(c.id.clone()),
            (_, Some(o)) => {
                if let Some(id) = s.database_logins.selection(WEB, o) {
                    return Some(id.to_string());
                }
                let mut matches = s.connections.connections.iter().filter(|c| is_service(c) && login_origin(c).as_ref() == Some(o));
                let first = matches.next()?;
                matches.next().is_none().then(|| first.id.clone())
            }
            (None, None) => None,
        }
    };
    let mut old_tiles = before.tiles.into_iter();
    for (i, m) in s.project.maps.iter_mut().enumerate() {
        let mut old = old_tiles.next().unwrap_or_default();
        for t in &mut m.tiles {
            // Tile layers have no id: those not there before (each old one matched once).
            if let Some(at) = old.iter().position(|(url, name, _)| *url == t.url && (anonymous || *name == t.name)) {
                let (_, _, legends) = old.swap_remove(at);
                // An unconnected user layer may be adopted later too. A plugin changing its
                // legends must not leave a path for that future login to bind.
                if anonymous
                    && t.connection.is_empty()
                    && !legends.iter().map(String::as_str).eq(t.legends.iter().map(|legend| legend.url.as_str()))
                {
                    t.anonymous = true;
                }
                continue;
            }
            if anonymous || t.anonymous {
                t.anonymous = true;
                t.connection.clear();
                continue;
            }
            let origin = logins::origin(tile_address(&t.url));
            if let Some(id) = pick(origin.as_ref()) {
                t.connection = id;
            }
            origins.extend(origin);
        }
        let old_layers = before.layers.get(i);
        let mut added = BTreeSet::new();
        for l in m.layers.iter_mut().filter(|l| old_layers.is_none_or(|old| !old.contains(&l.instance_id()))) {
            added.insert(l.id);
            if anonymous || l.anonymous {
                l.anonymous = true;
                l.connection.clear();
                continue;
            }
            let origin = layer_address(l).or(l.dataset.source.as_deref()).and_then(logins::origin);
            if let Some(id) = pick(origin.as_ref()).filter(|_| named.is_some() || l.connection.is_empty()) {
                l.connection = id;
            }
            if layer_address(l).is_some() {
                origins.extend(origin);
            }
        }
        // An imported service's group is refreshed from its address later.
        for g in &mut m.service_groups {
            if anonymous && before.groups.get(i).is_none_or(|old| !old.contains(&g.id)) {
                g.anonymous = true;
            }
            if !anonymous && !g.anonymous && g.children.iter().any(|c| added.contains(&c.layer)) {
                origins.extend(logins::origin(&g.url));
            }
        }
    }
    // A tile layer the command says it added (`index`, with its `kind`): one that replaced a layer
    // of the same address and name looks unchanged to the comparison above.
    let tile_kinds = ["raster", "vector", "style", "pmtiles", "mbtiles", "cog"];
    if let (Some(at), Some(kind)) = (out.get("index").and_then(Value::as_u64), out.get("kind").and_then(Value::as_str))
        && tile_kinds.contains(&kind)
        && let Some(t) = usize::try_from(at).ok().and_then(|at| s.project.map_mut().tiles.get_mut(at))
    {
        if anonymous || t.anonymous {
            t.anonymous = true;
            t.connection.clear();
            return Ok(());
        }
        let origin = logins::origin(tile_address(&t.url));
        if let Some(id) = pick(origin.as_ref()) {
            t.connection = id;
        }
        origins.extend(origin);
    }
    if !origins.is_empty() {
        resolve(s, Some(&origins));
    }
    Ok(())
}

#[cfg(test)]
mod stored_tests {
    use super::*;

    #[test]
    fn saved_envelope_binds_origin_method_user_and_parameter_without_echoing_errors() {
        let c = Connection {
            id: "c1".into(),
            kind: "wmts".into(),
            address: "https://remember-envelope.example.org/wmts".into(),
            auth: "query".into(),
            key_name: "api_key".into(),
            ..Default::default()
        };
        let origin = login_origin(&c).unwrap();
        let value = encode(&c, &origin, "envelope-secret-1979").unwrap();
        assert_eq!(decode(&value, &c, &origin).unwrap().expose(), "envelope-secret-1979");
        for foreign in ["http://remember-envelope.example.org", "https://remember-envelope.example.org:8443", "https://foreign.example.org"]
        {
            assert_eq!(decode(&value, &c, foreign), Err(SAVED_UNUSABLE));
        }
        for (field, replacement) in [("auth", "header"), ("keyName", "another_key"), ("user", "another-user")] {
            let mut edited = c.clone();
            match field {
                "auth" => edited.auth = replacement.into(),
                "keyName" => edited.key_name = replacement.into(),
                _ => edited.user = replacement.into(),
            }
            assert_eq!(decode(&value, &edited, &origin), Err(SAVED_UNUSABLE));
        }
        let mut raw: Value = serde_json::from_str(value.expose()).unwrap();
        for field in ["version", "origin", "auth", "user", "keyName", "secret"] {
            let mut missing = raw.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert_eq!(decode(&Secret::new(missing.to_string()), &c, &origin), Err(SAVED_UNUSABLE));
        }
        for version in [serde_json::json!(0), serde_json::json!(2), serde_json::json!("1")] {
            let mut newer = raw.clone();
            newer["version"] = version;
            assert_eq!(decode(&Secret::new(newer.to_string()), &c, &origin), Err(SAVED_UNUSABLE));
        }
        raw["extra"] = Value::Bool(true);
        assert_eq!(decode(&Secret::new(raw.to_string()), &c, &origin), Err(SAVED_UNUSABLE));
        for bad in ["not json envelope-secret-1979", "[]", &"x".repeat(MAX_STORED + 1)] {
            assert_eq!(decode(&Secret::new(bad), &c, &origin), Err(SAVED_UNUSABLE));
        }
        let invalid = encode(&c, &origin, "invalid\nsecret").unwrap();
        assert_eq!(decode(&invalid, &c, &origin), Err(SAVED_UNUSABLE));
    }
}
