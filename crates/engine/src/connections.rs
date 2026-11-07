//! Named, reusable endpoints kept with the app's preferences, not the project. Never a
//! secret: addresses are stripped of passwords, tokens and signatures before they are kept.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use rustgis_data::secrets::{Secret, SecretError, SecretStore};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::sources::{self, Sources};

/// The desktop preference saved connections are kept under.
pub const PREF: &str = "connections";

pub const MAX_CONNECTIONS: usize = 200;
const MAX_TEXT: usize = 2048;
const MAX_NAME: usize = 200;
pub const KINDS: &[&str] =
    &["postgres", "mysql", "sqlserver", "service", "wms", "wmts", "wfs", "wcs", "ogcapi", "arcgis", "csw", "stac", "s3"];
pub const DATABASE_KINDS: &[&str] = &["postgres", "mysql", "sqlserver"];
/// The web-service kinds, which can log in with a [`SERVICE_AUTH`] method.
pub const SERVICE_KINDS: &[&str] = &["service", "wms", "wmts", "wfs", "wcs", "ogcapi", "arcgis", "csw", "stac"];
/// How a web-service connection logs in: not at all, HTTP basic authentication (a user name and a
/// password), or an API key in a header.
pub const SERVICE_AUTH: &[&str] = &["none", "basic", "header"];
/// The kind of a [`LoginRequest`] for a web service's origin (its `key`).
pub const WEB: &str = "web";
pub const S3_CREDENTIALS: &[&str] = &["keys", "profile", "environment", "none"];
pub const DESKTOP_ONLY: &str = "database connections are used by the desktop app";

/// Whether a serialized flag is off (so it is left out).
fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Connection {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub group: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub address: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub options: BTreeMap<String, String>,
    /// An S3 connection whose keys the user asked to keep in the computer's credential store
    /// (`connection.login` with `remember`). Only a marker, never a key; not proof that the store
    /// still has them. Commands never take it from a caller.
    #[serde(default, rename = "savedKeys", skip_serializing_if = "is_false")]
    pub saved_keys: bool,
    /// A database connection's password was saved in the system's secret store (never the
    /// password itself, which is `DatabaseLogins`' to read). Set only by a login that asked to
    /// remember it, and cleared when the saved password is forgotten or no longer fits.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub remembered: bool,
    /// How a web-service connection logs in ([`SERVICE_AUTH`]; empty: it doesn't). The method
    /// only: the password or key is given to `connection.login`, never kept here.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub auth: String,
    /// The user name of a basic-authentication login.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub user: String,
    /// The header an API key goes in (`auth` header).
    #[serde(default, rename = "keyName", skip_serializing_if = "String::is_empty")]
    pub key_name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Connections {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub next: u64,
    #[serde(default)]
    pub connections: Vec<Connection>,
    #[serde(skip)]
    pub generation: u64,
}

pub fn database_kind(address: &str) -> Option<&'static str> {
    if rustgis_data::source::myconn::split_password(address).is_some() {
        Some("mysql")
    } else if rustgis_data::source::msconn::split_password(address).is_some() {
        Some("sqlserver")
    } else {
        let lower = address.trim().to_ascii_lowercase();
        (lower.starts_with("postgres:") || lower.starts_with("postgresql:")).then_some("postgres")
    }
}

/// The database kind and session-password key, as the driver receives the password-free address.
/// None for other addresses, and always in the browser.
pub fn database_key(address: &str) -> Option<(&'static str, String)> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        use rustgis_data::source::duckdb::Database;
        match Database::parse(address) {
            Database::Postgres(d) => Some(("postgres", d)),
            Database::Mysql(d) => Some(("mysql", d)),
            Database::Sqlserver(d) => Some(("sqlserver", d)),
            Database::File(_) => None,
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = address;
        None
    }
}

/// The kind and database address a streamed layer reads.
pub fn layer_database(src: &rustgis_map::source::SourceLayer) -> Option<(&'static str, &str)> {
    if let Some(origin) = &src.postgis {
        Some(("postgres", origin.database.as_str()))
    } else if let Some(origin) = &src.mysql {
        Some(("mysql", origin.database.as_str()))
    } else {
        src.sqlserver.as_ref().map(|origin| ("sqlserver", origin.database.as_str()))
    }
}

/// The password-free label for a driver's exact database key.
pub fn database_label(kind: &str, key: &str) -> String {
    if kind == "postgres" && !key.contains("://") { format!("postgres:{key}") } else { key.to_string() }
}

fn same_name(a: &str, b: &str) -> bool {
    a.chars().flat_map(char::to_lowercase).eq(b.chars().flat_map(char::to_lowercase))
}

fn name_text(t: &str) -> String {
    t.trim().chars().filter(|c| !c.is_control()).take(MAX_NAME).collect()
}

/// Checks a connection's login method and the fields it uses.
fn clean_login(c: &mut Connection) -> Result<(), String> {
    use crate::render::rustgis_tiles::logins;
    let auth = c.auth.trim().to_ascii_lowercase();
    c.auth = if auth == "none" { String::new() } else { auth };
    c.user = c.user.trim().to_string();
    c.key_name = c.key_name.trim().to_string();
    if c.auth.is_empty() {
        c.user.clear();
        c.key_name.clear();
        return Ok(());
    }
    if !SERVICE_KINDS.contains(&c.kind.as_str()) {
        return Err(format!("auth is for web-service connections ({}), not {}", SERVICE_KINDS.join(", "), c.kind));
    }
    match c.auth.as_str() {
        "basic" => {
            c.key_name.clear();
            if c.user.is_empty() || c.user.chars().count() > MAX_NAME || c.user.chars().any(|ch| ch.is_control() || ch == ':') {
                return Err(format!(
                    "basic authentication needs a user of at most {MAX_NAME} characters, without ':' or control characters"
                ));
            }
            Ok(())
        }
        "header" => {
            c.user.clear();
            if !logins::valid_header_name(&c.key_name) {
                return Err(format!(
                    "keyName must be an HTTP header name of at most {} letters, digits or one of !#$%&'*+-.^_`|~ (not one the app sets itself: Host, Cookie, Range, Proxy-… or Sec-…, among others)",
                    logins::MAX_NAME
                ));
            }
            Ok(())
        }
        other => Err(format!("unknown auth '{other}' ({})", SERVICE_AUTH.join(", "))),
    }
}

/// The origin (`scheme://host[:port]`) a web-service connection's login goes to: the only one.
pub fn login_origin(c: &Connection) -> Option<String> {
    crate::render::rustgis_tiles::logins::origin(&c.address)
}

/// Whether two definitions retain the same login identity: a database password uses the same
/// database address; a session web login uses the same origin, method, user and key name.
pub fn same_login(a: &Connection, b: &Connection) -> bool {
    a.kind == b.kind
        && if DATABASE_KINDS.contains(&a.kind.as_str()) {
            a.address == b.address
        } else {
            a.auth == b.auth && a.user == b.user && a.key_name == b.key_name && login_origin(a) == login_origin(b)
        }
}

pub fn clean(mut c: Connection) -> Result<(Connection, bool), String> {
    if !KINDS.contains(&c.kind.as_str()) {
        return Err(format!("unknown kind '{}' ({})", c.kind, KINDS.join(", ")));
    }
    let t = c.address.trim();
    if t.len() > MAX_TEXT || t.chars().any(char::is_control) {
        return Err("a connection address must be at most 2048 bytes and contain no control characters".into());
    }
    let stored = sources::redact(t);
    let stripped = stored != t;
    if DATABASE_KINDS.contains(&c.kind.as_str()) {
        if stored.is_empty() || database_kind(&stored) != Some(c.kind.as_str()) {
            return Err(format!(
                "'{stored}' isn't a {} address (postgres:host=… dbname=… user=…, mysql://user@host/db, mssql://user@host/db)",
                c.kind
            ));
        }
    } else {
        let lower = stored.to_ascii_lowercase();
        if !(lower.starts_with("http://") || lower.starts_with("https://") || c.kind == "s3" && stored.is_empty()) {
            return Err(format!("a {} connection needs an http:// or https:// address", c.kind));
        }
    }
    c.address = stored;
    clean_login(&mut c)?;
    c.name = name_text(&c.name);
    if c.name.is_empty() {
        c.name = if c.kind == "s3" && c.address.is_empty() { "AWS S3".into() } else { sources::default_name(&c.address) };
    }
    c.group = name_text(&c.group);
    if c.kind != "s3" {
        c.saved_keys = false;
    }
    // Only a database connection has a password to remember.
    c.remembered &= DATABASE_KINDS.contains(&c.kind.as_str());
    if c.kind != "s3" && !c.options.is_empty() {
        return Err(format!("{} connections take no options", c.kind));
    }
    for (k, v) in &mut c.options {
        match k.as_str() {
            "region" | "profile" => {
                let max = if k == "region" { 64 } else { MAX_NAME };
                if v.chars().count() > max || v.chars().any(char::is_control) {
                    return Err(format!("{k} must be at most {max} characters and contain no control characters"));
                }
            }
            "buckets" => {
                if !v.is_empty() {
                    let buckets: Vec<_> = v.split(',').map(str::trim).collect();
                    if buckets.iter().any(|b| !crate::render::rustgis_tiles::s3::valid_bucket(b)) {
                        return Err("invalid S3 bucket name".into());
                    }
                    *v = buckets.join(",");
                }
            }
            "credentials" => {
                if !v.is_empty() && !S3_CREDENTIALS.contains(&v.as_str()) {
                    return Err(format!("credentials must be one of {}", S3_CREDENTIALS.join(", ")));
                }
            }
            _ => return Err(format!("'{k}' isn't an S3 connection option (region, buckets, credentials, profile)")),
        }
    }
    if c.options.get("credentials").map(String::as_str) != Some("profile") {
        c.options.remove("profile");
    }
    c.options.retain(|_, v| !v.is_empty());
    Ok((c, stripped))
}

impl Connections {
    /// Every saved connection of a database's kind whose address names the database, in list order.
    /// Only the wrapper, surrounding whitespace and password the address may carry are set aside:
    /// the driver addresses are compared as text, so another form of the same server (a URI for a
    /// connection string, reordered options, another name for the host) is another database.
    pub fn matching_database(&self, kind: &str, database: &str) -> Vec<&Connection> {
        match database_key(&sources::redact(database)) {
            Some((k, key)) if k == kind => self.matching_key(kind, &key),
            _ => Vec::new(),
        }
    }

    /// Every saved connection of a kind with this driver address (see [`database_key`]).
    pub fn matching_key(&self, kind: &str, key: &str) -> Vec<&Connection> {
        self.connections
            .iter()
            .filter(|c| c.kind == kind && database_key(&c.address).is_some_and(|(k, address)| k == kind && address == key))
            .collect()
    }

    /// The saved connection a database comes from, when exactly one names it. Two that do leave
    /// it to the user's choice (see [`Connections::matching_database`]): none is first.
    pub fn for_database(&self, kind: &str, database: &str) -> Option<&Connection> {
        match self.matching_database(kind, database).as_slice() {
            [only] => Some(*only),
            _ => None,
        }
    }

    /// The saved connection a layer that reads a database comes from, when exactly one does.
    pub fn for_layer(&self, l: &rustgis_map::Layer) -> Option<&Connection> {
        let (kind, database) = layer_database(l.source.as_ref()?)?;
        self.for_database(kind, database)
    }

    pub fn from_json(text: &str) -> Self {
        let v: Value = serde_json::from_str(text).unwrap_or(Value::Null);
        let mut s = Self { version: 1, next: v.get("next").and_then(Value::as_u64).unwrap_or(1).max(1), ..Self::default() };
        let mut ids = BTreeSet::new();
        if let Some(rows) = v.get("connections").and_then(Value::as_array) {
            for row in rows {
                let Ok(c) = serde_json::from_value::<Connection>(row.clone()) else { continue };
                let Ok((c, _)) = clean(c) else { continue };
                if c.id.is_empty()
                    || c.id.len() > 32
                    || !c.id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                    || !ids.insert(c.id.clone())
                {
                    continue;
                }
                if let Some(n) = c.id.strip_prefix('c').and_then(|n| n.parse::<u64>().ok()) {
                    s.next = s.next.max(n.saturating_add(1));
                }
                s.connections.push(c);
                if s.connections.len() == MAX_CONNECTIONS {
                    break;
                }
            }
        }
        // All IDs must be known before a name is accepted: a later row can reserve it.
        // Normalize in order so the first duplicate name keeps precedence.
        for i in 0..s.connections.len() {
            let Some(c) = s.connections.get(i) else { continue };
            let name = s.unique_name(&c.name, None, s.connections.get(..i).unwrap_or_default());
            if let Some(c) = s.connections.get_mut(i) {
                c.name = name;
            }
        }
        s
    }

    pub fn import(saved: &Sources, s3_saved: &str) -> Self {
        let mut s = Self::default();
        for old in &saved.saved {
            let kind = match old.kind.as_str() {
                "service" => old
                    .extra
                    .get("type")
                    .and_then(Value::as_str)
                    .filter(|k| matches!(*k, "wms" | "wmts" | "wfs" | "wcs" | "ogcapi" | "arcgis" | "csw"))
                    .unwrap_or("service"),
                "database" => match database_kind(&old.address) {
                    Some(k) => k,
                    None => continue,
                },
                "stac" if old.extra.get("api").is_some() => "stac",
                _ => continue,
            };
            if s.connections.iter().any(|c| c.kind == kind && c.address == old.address) {
                continue;
            }
            let _ = s.add_unique(Connection {
                name: old.name.clone(),
                kind: kind.into(),
                address: old.address.clone(),
                ..Connection::default()
            });
        }
        let rows: Value = serde_json::from_str(s3_saved).unwrap_or(Value::Null);
        if let Some(rows) = rows.as_array() {
            for row in rows.iter().take(32) {
                let Some(row) = row.as_object() else { continue };
                let text = |k: &str| row.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
                let source = text("source");
                let credentials = if S3_CREDENTIALS.contains(&source.as_str()) { source } else { "keys".into() };
                let _ = s.add_unique(Connection {
                    name: text("name"),
                    kind: "s3".into(),
                    address: text("endpoint"),
                    options: BTreeMap::from([
                        ("region".into(), text("region")),
                        ("credentials".into(), credentials),
                        ("profile".into(), text("profile")),
                    ]),
                    ..Connection::default()
                });
            }
        }
        s
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(&json!({"version": 1, "next": self.next, "connections": self.connections})).unwrap_or_default()
    }

    pub fn find(&self, key: &str) -> Option<&Connection> {
        self.connections.iter().find(|c| c.id == key).or_else(|| self.connections.iter().find(|c| same_name(&c.name, key.trim())))
    }

    /// IDs win lookup, so names must not occupy either a stored ID or a future generated ID.
    fn name_is_id(&self, name: &str) -> bool {
        let generated = name.get(..1).is_some_and(|c| c.eq_ignore_ascii_case("c"))
            && name.get(1..).is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        generated || self.connections.iter().any(|c| same_name(&c.id, name))
    }

    // Loading checks names only against earlier rows, but IDs against the whole collection.
    fn unique_name(&self, base: &str, except: Option<&str>, named: &[Connection]) -> String {
        let taken = |name: &str| self.name_is_id(name) || named.iter().any(|c| Some(c.id.as_str()) != except && same_name(&c.name, name));
        if !taken(base) {
            return base.into();
        }
        for n in 2.. {
            let suffix = format!(" ({n})");
            let name = format!("{}{suffix}", base.chars().take(MAX_NAME.saturating_sub(suffix.len())).collect::<String>());
            if !taken(&name) {
                return name;
            }
        }
        base.into()
    }

    pub fn add(&mut self, c: Connection, explicit_name: bool) -> Result<(Connection, bool), String> {
        let (mut c, stripped) = clean(c)?;
        // A new connection (or a copy) has no saved keys of its own.
        c.saved_keys = false;
        if self.connections.len() >= MAX_CONNECTIONS {
            return Err(format!("up to {MAX_CONNECTIONS} connections"));
        }
        if explicit_name && self.name_is_id(&c.name) {
            return Err("a connection name cannot be a connection id".into());
        }
        let name = self.unique_name(&c.name, None, &self.connections);
        if explicit_name && name != c.name {
            return Err(format!("a connection named '{}' exists", c.name));
        }
        c.name = name;
        let mut next = self.next.max(1);
        while self.connections.iter().any(|c| c.id == format!("c{next}")) {
            next = next.checked_add(1).ok_or("connection ids exhausted")?;
        }
        self.next = next.checked_add(1).ok_or("connection ids exhausted")?;
        c.id = format!("c{next}");
        // A new connection, a copy or an import has no saved password: it is saved by a login.
        c.remembered = false;
        self.connections.push(c.clone());
        self.version = 1;
        self.generation = self.generation.wrapping_add(1);
        Ok((c, stripped))
    }

    fn add_unique(&mut self, c: Connection) -> Result<(Connection, bool), String> {
        self.add(c, false)
    }

    /// What [`Connections::update`] would store for connection `id` given `c` (checked, named
    /// uniquely, keeping its id and its saved-keys marker), and whether an address was stripped of
    /// secrets, without changing anything.
    pub(crate) fn prepare_update(&self, id: &str, c: Connection) -> Result<(Connection, bool), String> {
        let explicit_name = !c.name.trim().is_empty();
        let (mut c, stripped) = clean(c)?;
        if explicit_name && self.name_is_id(&c.name) {
            return Err("a connection name cannot be a connection id".into());
        }
        let name = self.unique_name(&c.name, Some(id), &self.connections);
        if explicit_name && name != c.name {
            return Err(format!("a connection named '{}' exists", c.name));
        }
        c.name = name;
        let slot = self.connections.iter().find(|c| c.id == id).ok_or_else(|| format!("no connection '{id}'"))?;
        c.id = id.into();
        c.saved_keys = slot.saved_keys && c.kind == "s3";
        // Password identity includes the cleaned database kind, server and user.
        c.remembered = slot.remembered && slot.kind == c.kind && slot.address == c.address;
        Ok((c, stripped))
    }

    /// Commits to a target located before credential deletion. No lookup or validation remains.
    pub(crate) fn commit_update(slot: &mut Connection, generation: &mut u64, c: Connection) -> Connection {
        *slot = c.clone();
        *generation = generation.wrapping_add(1);
        c
    }

    pub fn update(&mut self, id: &str, c: Connection) -> Result<(Connection, bool), String> {
        let (c, stripped) = self.prepare_update(id, c)?;
        let slot = self.connections.iter_mut().find(|slot| slot.id == id).ok_or_else(|| format!("no connection '{id}'"))?;
        Ok((Self::commit_update(slot, &mut self.generation, c), stripped))
    }

    /// Records whether connection `id` has saved keys (the marker): after a store call said so.
    /// Changing it bumps `generation`, as every change to the preference does.
    pub fn set_saved_keys(&mut self, id: &str, saved: bool) {
        if let Some(c) = self.connections.iter_mut().find(|c| c.id == id && c.kind == "s3")
            && c.saved_keys != saved
        {
            c.saved_keys = saved;
            self.generation = self.generation.wrapping_add(1);
        }
    }

    pub fn remove(&mut self, id: &str) -> Option<Connection> {
        let index = self.connections.iter().position(|c| c.id == id)?;
        let c = self.connections.remove(index);
        self.generation = self.generation.wrapping_add(1);
        Some(c)
    }

    /// Says whether a database connection's password is saved; whether that changed the row.
    pub fn set_remembered(&mut self, id: &str, on: bool) -> bool {
        let Some(c) = self.connections.iter_mut().find(|c| c.id == id && DATABASE_KINDS.contains(&c.kind.as_str())) else {
            return false;
        };
        if c.remembered == on {
            return false;
        }
        c.remembered = on;
        self.generation = self.generation.wrapping_add(1);
        true
    }
}

/// What a saved database password's lookup found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SavedPassword {
    /// The password saved for this definition.
    Found(Secret),
    /// None: no account, or one saved for another server or user (or damaged). The connection's
    /// marker should be cleared.
    Absent,
    /// The store couldn't be asked (none here, or locked, denied, unavailable): the marker says
    /// what it said.
    Unusable,
}

/// Why a database's layers wait for the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginReason {
    /// More than one saved connection with a saved password names the database: the user picks
    /// which one this session uses.
    Choose,
    /// No password is usable (the saved one was refused): the user types one.
    Password,
    /// Layers recorded a saved connection this computer doesn't have (or one for another
    /// address), and none of its connections logs in to their address: the user makes one.
    Connection,
}

impl LoginReason {
    pub fn name(self) -> &'static str {
        match self {
            LoginReason::Choose => "choose",
            LoginReason::Password => "password",
            LoginReason::Connection => "connection",
        }
    }
}

/// A database's layers waiting for a login the app should ask the user for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginRequest {
    pub reason: LoginReason,
    pub kind: &'static str,
    /// The database as its driver names it, without a password.
    pub key: String,
    /// The saved connections it may be, by id.
    pub connections: Vec<String>,
    /// The layers that wait, by id.
    pub layers: Vec<u32>,
    /// How many tile layers wait (they have no id).
    pub tiles: usize,
    /// [`crate::Session::opened`] when it was made: a project opened since has other layers.
    pub opened: u64,
    /// The app has asked the user.
    pub prompted: bool,
}

/// The shared account for a saved connection's database password or S3 key pair.
pub fn account(id: &str) -> String {
    format!("connection/{id}")
}

/// What the store keeps for a saved password: the definition it was saved for beside it, so an id
/// reused by another server (ids are sequential) never gets this one's password.
fn envelope(c: &Connection, password: &str) -> Secret {
    Secret::new(json!({"kind": c.kind, "address": c.address, "password": password}).to_string())
}

/// The password in a stored envelope, if it was saved for exactly this definition.
fn password_in(c: &Connection, secret: &Secret) -> Option<Secret> {
    let v: Value = serde_json::from_str(secret.expose()).ok()?;
    let o = v.as_object()?;
    if o.len() != 3 || o.get("kind")?.as_str()? != c.kind || o.get("address")?.as_str()? != c.address {
        return None;
    }
    o.get("password")?.as_str().filter(|p| !p.is_empty()).map(Secret::new)
}

/// The database logins of this run: the system's secret store, the passwords given or loaded, which
/// connection was chosen for each database, and the logins the app should ask for. Never saved
/// with the project or the preferences, and never shown (its `Debug` counts, it doesn't print).
#[derive(Default)]
pub struct DatabaseLogins {
    store: Option<Arc<dyn SecretStore>>,
    /// Passwords given to `connection.login`, by connection id, for this session.
    session: BTreeMap<String, Secret>,
    /// Saved passwords read from the store, by connection id.
    saved: BTreeMap<String, Secret>,
    /// Passwords the database client's environment gave (`PGPASSWORD`…) to connections that keep
    /// a saved one, by connection id: used before the saved one, which then isn't read. Only in a
    /// session that opted into the desktop's saved secrets (see `prefer_environment_passwords`).
    ambient: BTreeMap<String, Secret>,
    /// Whether the environment's passwords rank above the saved ones (the command line and Python
    /// with `--use-saved-secrets` / `use_saved_secrets`); the desktop never reads them.
    environment_first: bool,
    unavailable: BTreeSet<String>,
    /// The connection chosen for each database (kind and driver address) by a login.
    selected: BTreeMap<(&'static str, String), String>,
    /// Connections whose saved or environment password the server refused this session.
    rejected: BTreeSet<String>,
    /// The connection whose password the driver uses for each database address (driver key),
    /// as set by a login or the project's resolution: what is set again when the driver's
    /// passwords were cleared around reading a project's text.
    activations: BTreeMap<String, String>,
    requests: Vec<LoginRequest>,
}

impl std::fmt::Debug for DatabaseLogins {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DatabaseLogins")
            .field("store", &self.store.is_some())
            .field("session", &self.session.len())
            .field("saved", &self.saved.len())
            .field("selected", &self.selected.len())
            .field("rejected", &self.rejected.len())
            .field("requests", &self.requests.len())
            .finish()
    }
}

impl DatabaseLogins {
    /// Gives the system's secret store to use (the desktop app's; `None` in the browser, on the
    /// command line and in scripts, where a password lasts for the session).
    pub fn set_store(&mut self, store: Option<Arc<dyn SecretStore>>) {
        self.store = store;
        self.saved.clear();
        self.ambient.clear();
        self.unavailable.clear();
    }

    pub fn has_store(&self) -> bool {
        self.store.is_some()
    }

    /// The same desktop credential store for all saved connection kinds.
    pub fn secret_store(&self) -> Option<&Arc<dyn SecretStore>> {
        self.store.as_ref()
    }

    /// Whether `id` has a password given this session.
    pub fn has_session_password(&self, id: &str) -> bool {
        self.session.contains_key(id)
    }

    pub fn session_password(&self, id: &str) -> Option<&Secret> {
        self.session.get(id)
    }

    pub fn set_session_password(&mut self, id: &str, password: &str) {
        self.session.insert(id.to_string(), Secret::new(password));
    }

    /// Drops a refused session credential and suppresses an equivalent saved value for this run.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn reject_session_password(&mut self, c: &Connection) {
        let Some(refused) = self.session.remove(&c.id) else {
            // Nothing was given this session, so the refused password came from the environment:
            // it isn't tried again, and the connection is rejected for the session, so the saved
            // password isn't tried automatically either (the user types one, as with any refusal).
            if self.ambient.remove(&c.id).is_some() && c.remembered {
                self.rejected.insert(c.id.clone());
            }
            return;
        };
        if !c.remembered || self.is_rejected(&c.id) {
            return;
        }
        let same = match self.saved.get(&c.id) {
            Some(saved) => saved == &refused,
            // Explicit login/Refresh may have invalidated the lazy cache. Validate the bound
            // account through the existing lookup before deciding whether it is the same value.
            None => matches!(self.load(c), SavedPassword::Found(saved) if saved == refused),
        };
        if same {
            self.reject(&c.id);
        }
    }

    /// Reads the password saved for a definition (once: later calls use what was read).
    pub fn load(&mut self, c: &Connection) -> SavedPassword {
        if let Some(p) = self.saved.get(&c.id) {
            return SavedPassword::Found(p.clone());
        }
        if self.unavailable.contains(&c.id) {
            return SavedPassword::Unusable;
        }
        let Some(store) = &self.store else {
            self.unavailable.insert(c.id.clone());
            return SavedPassword::Unusable;
        };
        match store.load(&account(&c.id)) {
            Ok(Some(secret)) => match password_in(c, &secret) {
                Some(p) => {
                    self.saved.insert(c.id.clone(), p.clone());
                    SavedPassword::Found(p)
                }
                None => SavedPassword::Absent,
            },
            Ok(None) => SavedPassword::Absent,
            Err(_) => {
                self.unavailable.insert(c.id.clone());
                SavedPassword::Unusable
            }
        }
    }

    /// Saves a definition's password; whether it was saved. A saved password replaces an older
    /// one and is trusted again if the server had refused the older one this session.
    pub fn save(&mut self, c: &Connection, password: &str) -> bool {
        let Some(store) = &self.store else { return false };
        if store.save(&account(&c.id), &envelope(c, password)).is_err() {
            return false;
        }
        self.saved.insert(c.id.clone(), Secret::new(password));
        self.unavailable.remove(&c.id);
        self.rejected.remove(&c.id);
        true
    }

    /// Deletes the account of a saved password (none to delete is success). Without a store the
    /// error says so.
    pub fn delete(&mut self, id: &str) -> Result<(), SecretError> {
        let store = self.store.as_ref().ok_or_else(|| SecretError::Unavailable("no secret store".into()))?;
        store.delete(&account(id))?;
        self.saved.remove(id);
        self.unavailable.remove(id);
        self.rejected.remove(id);
        Ok(())
    }

    /// Says the server refused the saved password of `id`: it is dropped from memory (the store
    /// keeps it) and not tried again this session.
    pub fn reject(&mut self, id: &str) {
        self.saved.remove(id);
        self.rejected.insert(id.to_string());
    }

    pub fn is_rejected(&self, id: &str) -> bool {
        self.rejected.contains(id)
    }

    /// The connection a login chose for a database, if any.
    pub fn selection(&self, kind: &str, key: &str) -> Option<&str> {
        self.selected.iter().find(|((k, d), _)| *k == kind && d == key).map(|(_, id)| id.as_str())
    }

    /// Current web-service choices, including choices whose login is presently absent.
    pub(crate) fn web_selections(&self) -> Vec<(&str, &str)> {
        self.selected.iter().filter(|((kind, _), _)| *kind == WEB).map(|((_, origin), id)| (origin.as_str(), id.as_str())).collect()
    }

    pub fn select(&mut self, kind: &'static str, key: &str, id: &str) {
        self.selected.insert((kind, key.to_string()), id.to_string());
    }

    /// Forgets everything held for a connection that is going away or changing server: the
    /// passwords given and read, the choice, the refusal, and the requests naming it.
    pub fn forget_connection(&mut self, id: &str) {
        self.session.remove(id);
        self.saved.remove(id);
        self.ambient.remove(id);
        self.unavailable.remove(id);
        self.rejected.remove(id);
        self.selected.retain(|_, chosen| chosen != id);
        self.activations.retain(|_, active| active != id);
        self.requests.retain(|r| !r.connections.iter().any(|c| c == id));
    }

    /// Forgets the choice made for a database.
    pub fn drop_selection(&mut self, kind: &str, key: &str) {
        self.selected.retain(|(k, d), _| !(*k == kind && d == key));
    }

    /// The password a connection logs in with where it is known here: the one given this session,
    /// else the environment's, else the saved one read (never reading the store).
    pub fn known_password(&self, id: &str) -> Option<&Secret> {
        self.session.get(id).or_else(|| self.ambient.get(id)).or_else(|| self.saved.get(id))
    }

    /// Lets the database clients' environment passwords serve saved connections before the store.
    /// For the command line and Python that opted into the desktop's saved secrets; the desktop
    /// itself never calls it.
    pub fn prefer_environment_passwords(&mut self) {
        self.environment_first = true;
    }

    /// The environment's password for a database connection that keeps a saved one (`PGPASSWORD`,
    /// `MYSQL_PWD` or `SQLCMDPASSWORD` of its kind), which ranks above the saved one: a login that
    /// has it never asks the store. It is read once per connection, and only after
    /// `prefer_environment_passwords`.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn ambient(&mut self, c: &Connection) -> Option<Secret> {
        if !self.environment_first {
            return None;
        }
        if let Some(p) = self.ambient.get(&c.id) {
            return Some(p.clone());
        }
        let (kind, _) = database_key(&c.address)?;
        let p = Secret::new(rustgis_data::source::duckdb::ambient_password(kind)?);
        self.ambient.insert(c.id.clone(), p.clone());
        Some(p)
    }

    /// Retries a lazy lookup on explicit login or Refresh, without retrying a refused password.
    pub fn retry_saved(&mut self, id: &str) {
        self.saved.remove(id);
        self.unavailable.remove(id);
    }

    /// Notes that `id`'s password is the one in use for a database address.
    pub fn note_activation(&mut self, key: &str, id: &str) {
        self.activations.insert(key.to_string(), id.to_string());
    }

    /// Notes that no connection's password is in use for a database address.
    pub fn drop_activation(&mut self, key: &str) {
        self.activations.remove(key);
    }

    /// The database addresses with the connection whose password is in use for each.
    pub fn activations(&self) -> Vec<(String, String)> {
        self.activations.iter().map(|(k, id)| (k.clone(), id.clone())).collect()
    }

    pub fn clear_activations(&mut self) {
        self.activations.clear();
    }

    /// Forgets only the saved password of a connection, as the user asked to (a password given this
    /// session stays).
    pub fn forget_saved(&mut self, id: &str) {
        self.saved.remove(id);
        self.ambient.remove(id);
        self.rejected.remove(id);
        if !self.session.contains_key(id) {
            self.activations.retain(|_, active| active != id);
        }
    }

    /// Adds a request, replacing the one for the same database. The app is asked again only when
    /// what it would ask has changed, within the same project (another project has other layers).
    pub fn request(&mut self, mut request: LoginRequest) {
        if let Some(old) = self.requests.iter().find(|r| (r.kind, &r.key) == (request.kind, &request.key)) {
            request.prompted =
                old.opened == request.opened && old.prompted && old.reason == request.reason && old.connections == request.connections;
        }
        self.requests.retain(|r| (r.kind, &r.key) != (request.kind, &request.key));
        self.requests.push(request);
    }

    /// Drops the requests for a database (it logged in, or its layers went).
    pub fn clear_requests_for(&mut self, kind: &str, key: &str) {
        self.requests.retain(|r| (r.kind, r.key.as_str()) != (kind, key));
    }

    /// Drops every request: the project they came from is gone, or the saved connections changed.
    pub fn clear_requests(&mut self) {
        self.requests.clear();
    }

    /// The requests of the project opened as `opened`.
    pub fn requests(&self, opened: u64) -> impl Iterator<Item = &LoginRequest> {
        self.requests.iter().filter(move |r| r.opened == opened)
    }

    /// The next request the app hasn't asked the user about, marked as asked.
    pub fn next_unprompted(&mut self, opened: u64) -> Option<LoginRequest> {
        let r = self.requests.iter_mut().find(|r| r.opened == opened && !r.prompted)?;
        r.prompted = true;
        Some(r.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_connections() {
        let saved = Sources::from_json(
            &json!({"saved": [
                {"kind":"service", "name":"Shared", "address":"https://w.example/wms", "extra":{"type":"wms"}},
                {"kind":"service", "name":"Shared", "address":"https://s.example"},
                {"kind":"database", "name":"PG", "address":"postgres:host=h dbname=g user=u"},
                {"kind":"database", "name":"My", "address":"mysql://u@h/db"},
                {"kind":"database", "name":"MS", "address":"mssql://u@h/db"},
                {"kind":"database", "name":"File", "address":"/x/a.duckdb"},
                {"kind":"stac", "name":"API", "address":"https://stac.example", "extra":{"api":"true"}},
                {"kind":"stac", "name":"Item", "address":"https://stac.example/item"},
                {"kind":"service", "name":"Duplicate", "address":"https://w.example/wms", "extra":{"type":"wms"}}
            ]})
            .to_string(),
        );
        let s = Connections::import(
            &saved,
            r#"[{"name":"Keys","source":""},{"name":"Profile","source":"profile","profile":"work"},{"name":"Public","source":"none"}]"#,
        );
        let kinds: Vec<_> = s.connections.iter().map(|c| c.kind.as_str()).collect();
        assert_eq!(kinds, ["wms", "service", "postgres", "mysql", "sqlserver", "stac", "s3", "s3", "s3"]);
        assert_eq!(s.connections[1].name, "Shared (2)");
        assert_eq!(s.find("Keys").unwrap().options["credentials"], "keys");
        assert_eq!(s.find("Profile").unwrap().options["profile"], "work");
        assert_eq!(s.find("Public").unwrap().options["credentials"], "none");
        assert_eq!(Connections::from_json(&s.to_json()).connections, s.connections);
    }

    #[test]
    fn connections_tolerate_junk_and_preserve_identity() {
        let mut s = Connections::from_json(
            &json!({"next":2,"connections":[
                null, {"id":"bad!","name":"Bad","kind":"wms","address":"https://a"},
                {"id":"c90","name":"A","kind":"wms","address":"https://a"},
                {"id":"c90","name":"B","kind":"wms","address":"https://b"},
                {"id":"custom","name":"a","kind":"wms","address":"https://b"},
                {"id":"c999","name":"Bad","kind":"unknown","address":"https://b"}
            ]})
            .to_string(),
        );
        assert_eq!(s.connections.len(), 2);
        assert_eq!(s.connections[1].name, "a (2)");
        let (c, stripped) = s
            .add(
                Connection {
                    kind: "postgres".into(),
                    name: "DB".into(),
                    address: "postgresql://u:pw@h/db".into(),
                    ..Connection::default()
                },
                true,
            )
            .unwrap();
        assert_eq!(c.id, "c91");
        assert!(stripped);
        assert!(!s.to_json().contains("pw"));
        let (other, _) = s.add(Connection { name: "Other".into(), ..c.clone() }, true).unwrap();
        assert_ne!(c.id, other.id);
        assert_eq!(c.address, other.address);
        assert_eq!(Connections::from_json(&s.to_json()).connections, s.connections);
    }

    #[test]
    fn connections_names_cannot_shadow_stored_or_future_ids() {
        let mut s = Connections::from_json(
            &json!({"connections": [
                {"id":"first","name":"REMOTE","kind":"s3"},
                {"id":"remote","name":"Remote (2)","kind":"s3"},
                {"id":"third","name":"c4","kind":"s3"},
                {"id":"fourth","name":"remote","kind":"s3"}
            ]})
            .to_string(),
        );
        assert_eq!(s.connections[0].name, "REMOTE (2)");
        assert_eq!(s.connections[1].name, "Remote (2) (2)");
        assert_eq!(s.connections[2].name, "c4 (2)");
        assert_eq!(s.connections[3].name, "remote (3)");
        for c in &s.connections {
            assert_eq!(s.find(&c.name), Some(c));
        }
        let template = s.connections[0].clone();
        for name in ["remote", "FIRST", "c1", "c5", "C999"] {
            let c = Connection { name: name.into(), ..template.clone() };
            assert!(s.add(c.clone(), true).is_err(), "{name}");
            assert!(s.update("first", c).is_err(), "{name}");
        }
        let (generated, _) = s.add(Connection { name: "c1".into(), ..template.clone() }, false).unwrap();
        assert_eq!(generated.id, "c1");
        assert_eq!(generated.name, "c1 (2)");
        assert_eq!(s.find(&generated.name), Some(&generated));
        assert_eq!(Connections::from_json(&s.to_json()).connections, s.connections);
    }

    #[test]
    fn connections_blank_edit_name_uses_a_unique_default() {
        let mut s = Connections::default();
        let c = Connection { kind: "wms".into(), address: "https://a.example/wms".into(), ..Connection::default() };
        let (first, _) = s.add(c.clone(), false).unwrap();
        let (second, _) = s.add(Connection { name: "Other".into(), ..c }, true).unwrap();
        let (edited, _) = s.update(&second.id, Connection { name: "  ".into(), ..second.clone() }).unwrap();
        assert_eq!(edited.name, "wms (2)");
        assert_eq!(s.find(&first.name), Some(&first));
        assert!(s.update(&second.id, Connection { name: first.name, ..edited }).is_err());
    }

    #[test]
    fn connections_validate_options_and_bounds() {
        let c = Connection {
            kind: "s3".into(),
            options: BTreeMap::from([
                ("credentials".into(), "profile".into()),
                ("profile".into(), "work".into()),
                ("buckets".into(), " b11, b22 ".into()),
            ]),
            ..Connection::default()
        };
        let (c, _) = clean(c).unwrap();
        assert_eq!(c.options["buckets"], "b11,b22");
        assert!(clean(Connection { options: BTreeMap::from([("password".into(), "secret".into())]), ..c.clone() }).is_err());
        assert!(clean(Connection { options: BTreeMap::from([("buckets".into(), "NO_".into())]), ..c }).is_err());
        let mut s = Connections::from_json(r#"{"next":18446744073709551615}"#);
        assert!(s.add(Connection { kind: "s3".into(), ..Connection::default() }, false).is_err());
    }
}
