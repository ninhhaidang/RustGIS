//! Logins of saved web-service connections (WMS, WMTS, WFS, WCS, OGC API, ArcGIS REST, CSW,
//! STAC): HTTP basic authentication or an API key in a header.
//!
//! The connection keeps the method, user name and header name, never the password or key.
//! A secret given to `connection.login` lasts only for this session, held by connection id in
//! [`crate::connections::DatabaseLogins`]. Its runtime identity also binds it to the canonical
//! origin, method, user and header, so changing a definition cannot send an old secret elsewhere.
//!
//! Each session/connection login has an opaque request binding. Only requests carrying that
//! binding use its credential, and only at the connection's exact origin. Bindings distinguish
//! authenticated cache entries and are revoked on replacement, logout and session drop. Layers
//! record their saved connection id, never a trusted persisted runtime binding. A project opened
//! without its connection or login asks through the existing login-request queue.
//! Nothing here prints, returns or logs a secret: every failure is
//! a fixed sentence.

use rustgis_data::secrets::Secret;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

use crate::Session;
use crate::connections::{Connection, LoginReason, LoginRequest, SERVICE_KINDS, WEB, login_origin};
use crate::render::rustgis_tiles::{cache, logins};

/// A transient secret whose connection identity changed.
pub const UNUSABLE: &str = "The session login could not be used. Enter it again.";
/// Persistence is a separate feature; a service login never accesses the credential store.
pub const SESSION_ONLY: &str = "Web-service logins last for this session only.";
/// A login typed for a plain http:// address without the confirmation.
pub const INSECURE: &str = "This connection's address uses plain http://, so its login would be sent unencrypted. Confirm with allowInsecure: true to send it anyway.";
/// A password or key that can't be sent.
pub const BAD_SECRET: &str = "The password or key must be at most 4096 bytes, without control characters.";
/// An API key a header can't carry the same way everywhere (the browser sends Latin-1 only).
pub const HEADER_ASCII: &str = "An API key sent in a header must use plain ASCII characters.";
/// A login asked of a connection with nothing to log in with this session.
pub const REQUIRED: &str = "This connection's login is required: give its password (basic) or apiKey (header).";
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

/// Whether `c` is a web-service connection (one that can log in with a method).
pub fn is_service(c: &Connection) -> bool {
    SERVICE_KINDS.contains(&c.kind.as_str())
}

/// The secret a login of `c` sends, as its header method sends it.
fn method(c: &Connection, secret: &str) -> Option<(logins::Method, String)> {
    match c.auth.as_str() {
        "basic" => Some((logins::Method::Header("Authorization".into()), logins::basic(&c.user, secret))),
        "header" => Some((logins::Method::Header(c.key_name.clone()), secret.to_string())),
        _ => None,
    }
}

/// Checks a typed password or key.
pub fn check_secret(secret: &str) -> Result<(), &'static str> {
    if logins::valid_value(secret) { Ok(()) } else { Err(BAD_SECRET) }
}

/// Checks the complete wire credential before a form edit can invalidate an earlier login.
pub fn check_login_secret(c: &Connection, secret: &str) -> Result<(), String> {
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
}

impl SessionLogins {
    pub fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self { session: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed), active: BTreeMap::new(), anonymous: false }
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
    let next = match secret.and_then(|secret| method(c, secret.expose())) {
        Some((method, value)) => {
            let binding = logins::activate(&origin, &s.web_logins.owner(&c.id), method, &value, insecure).map_err(str::to_string)?;
            Some(ActiveLogin { origin: origin.clone(), binding, auth: c.auth.clone(), user: c.user.clone(), key_name: c.key_name.clone() })
        }
        None => None,
    };
    let replaced = s.web_logins.active.remove(&c.id).map(|old| logins::deactivate(&old.binding)).is_some();
    if let Some(next) = next {
        s.web_logins.active.insert(c.id.clone(), next);
    }
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

pub(crate) fn logout(s: &mut Session, c: &Connection) {
    forget(s, c);
}

pub(crate) fn forget(s: &mut Session, c: &Connection) {
    deactivate(s, c, None);
    s.database_logins.forget_connection(&c.id);
}

pub fn logged_in(s: &Session, c: &Connection) -> bool {
    s.web_logins.active.get(&c.id).is_some_and(|a| a.matches(c) && logins::is_active(&a.binding))
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
        return Ok(url.to_string());
    }
    let Some(active) = s.web_logins.active.get(&c.id) else { return Ok(url.to_string()) };
    logins::bind(&active.binding, url)
        .map_err(|_| format!("{url}: this address can't carry the connection's login (it has a space, a control character or a backslash)"))
}

pub fn known_secret(s: &Session, c: &Connection) -> Result<Secret, String> {
    let Some(secret) = s.database_logins.session_password(&c.id) else {
        return Err(REQUIRED.into());
    };
    if !s.web_logins.active.get(&c.id).is_some_and(|a| a.matches(c)) {
        return Err(UNUSABLE.into());
    }
    Ok(secret.clone())
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
                        source.error = None;
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
            // A secret given this session was confirmed for this origin when it was typed.
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
                if e != REQUIRED && !already_asked {
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
