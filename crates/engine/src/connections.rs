//! Named, reusable endpoints kept with the app's preferences, not the project. Never a
//! secret: addresses are stripped of passwords, tokens and signatures before they are kept.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use rustgis_data::secrets::{Secret, SecretError, SecretStore};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::sources::{self, Sources};

/// The desktop preference saved connections are kept under.
pub const PREF: &str = "connections";
/// The version of that preference this RustGIS reads and writes. One from a newer RustGIS isn't
/// read, and is kept as it is.
pub const VERSION: u64 = 1;
/// Why the preference wasn't read when it couldn't be (`{why}`), and what to do.
const UNREADABLE: &str =
    "the saved connections couldn't be read ({why}); fix or remove \"connections\" in the preferences file to use or change them";

pub const MAX_CONNECTIONS: usize = 200;
const MAX_TEXT: usize = 2048;
const MAX_NAME: usize = 200;
pub const KINDS: &[&str] =
    &["postgres", "mysql", "sqlserver", "service", "wms", "wmts", "wfs", "wcs", "ogcapi", "arcgis", "csw", "stac", "s3"];
pub const DATABASE_KINDS: &[&str] = &["postgres", "mysql", "sqlserver"];
/// The web-service kinds, which can log in with a [`SERVICE_AUTH`] method.
pub const SERVICE_KINDS: &[&str] = &["service", "wms", "wmts", "wfs", "wcs", "ogcapi", "arcgis", "csw", "stac"];
/// How a web-service connection logs in: not at all, HTTP basic authentication (a user name and a
/// password), an API key in a header, or an API key in a query parameter.
pub const SERVICE_AUTH: &[&str] = &["none", "basic", "header", "query"];
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
    /// The header (`auth` header) or query parameter (`auth` query) an API key goes in.
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
    /// What of the preference this version couldn't use, written back as it was.
    #[serde(skip)]
    kept: Kept,
}

/// What of the saved connections this version couldn't use. A newer RustGIS may add rows, fields
/// and kinds without a new version: they are kept and written back unchanged, so editing a
/// connection here never drops what the newer one wrote.
#[derive(Clone, Debug, Default, PartialEq)]
struct Kept {
    /// The whole preference as read, when it is from a newer RustGIS or couldn't be read at all,
    /// and why: no connection of it is used, and it is never written over.
    whole: Option<(String, String)>,
    /// Rows that couldn't be read (an unknown kind, a bad id…) or are past [`MAX_CONNECTIONS`].
    rows: Vec<KeptRow>,
    /// The ids those rows carry, which a new connection never takes.
    ids: BTreeSet<String>,
    /// Fields of read connections this version doesn't know, by connection id.
    fields: BTreeMap<String, Map<String, Value>>,
    /// Keys beside `version`, `next` and `connections` this version doesn't know.
    top: Map<String, Value>,
    /// Rows and fields that couldn't be read and held a password, key or token: not kept, since
    /// this version can't strip it from what it doesn't understand.
    secrets: usize,
    /// Readable rows whose address had a password, key or token taken out when read: saved so.
    stripped: usize,
    /// Readable rows whose name or group was cut (past [`MAX_NAME`] characters, or with control
    /// characters).
    cut: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum KeptReason {
    /// From a newer RustGIS, or damaged.
    Unreadable,
    /// Readable, but past [`MAX_CONNECTIONS`].
    PastLimit,
    /// Readable, but its query login's parameter name can't be held for redaction this run.
    NameRegistry,
}

#[derive(Clone, Debug, PartialEq)]
struct KeptRow {
    value: Value,
    reason: KeptReason,
}

impl Kept {
    /// The entries of `o` without a password, key or token, counting those left out. Every entry is
    /// classified on the original object first, beside the others and `address` (the one a row
    /// keeps apart from them): a bare `key` is a secret beside a web address, and an address
    /// holding a secret doesn't stop being one. Only then are the classified entries removed.
    fn without_secrets(&mut self, mut o: Map<String, Value>, address: &str) -> Map<String, Value> {
        let secret: Vec<String> = o
            .iter()
            .filter(|(k, v)| {
                let mut probe = Map::new();
                probe.insert((*k).clone(), (*v).clone());
                if web_address(&Value::String(address.into()), 0)
                    || o.iter().any(|(other, v)| {
                        other != *k && !other.eq_ignore_ascii_case("data") && !other.eq_ignore_ascii_case("text") && web_address(v, 0)
                    })
                {
                    probe.insert("\u{1}beside".into(), "https://a".into());
                }
                sources::holds_secret(&Value::Object(probe))
            })
            .map(|(k, _)| k.clone())
            .collect();
        self.secrets += secret.len();
        o.retain(|k, _| !secret.contains(k));
        o
    }
}

/// Whether `v` is, or holds, a web address (not under a file's `data` or `text`).
fn web_address(v: &Value, depth: usize) -> bool {
    match v {
        Value::String(t) => t.trim_start().get(..4).is_some_and(|p| p.eq_ignore_ascii_case("http")),
        Value::Object(o) if depth < 4 => {
            o.iter().any(|(k, v)| !k.eq_ignore_ascii_case("data") && !k.eq_ignore_ascii_case("text") && web_address(v, depth + 1))
        }
        Value::Array(a) if depth < 4 => a.iter().any(|v| web_address(v, depth + 1)),
        _ => false,
    }
}

/// A refused row may still name a credential in opaque metadata. Inspect it using that name
/// without adding the rejected definition to the process-wide registry.
fn holds_query_secret(row: &Value) -> bool {
    fn walk(value: &Value, name: &str, depth: usize) -> bool {
        match value {
            Value::String(address) => crate::render::rustgis_tiles::logins::without_query_parameter(address, name) != *address,
            Value::Object(o) if depth < 16 => o.values().any(|v| walk(v, name, depth + 1)),
            Value::Array(a) if depth < 16 => a.iter().any(|v| walk(v, name, depth + 1)),
            Value::Object(_) | Value::Array(_) => true,
            _ => false,
        }
    }
    row.get("auth").and_then(Value::as_str).is_some_and(|auth| auth.trim().eq_ignore_ascii_case("query"))
        && row.get("keyName").and_then(Value::as_str).is_some_and(|name| !name.trim().is_empty() && walk(row, name.trim(), 0))
}

/// A saved row: the connection, and the fields this version doesn't know.
#[derive(Deserialize)]
struct Row {
    #[serde(flatten)]
    connection: Connection,
    #[serde(flatten)]
    other: Map<String, Value>,
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

/// Checks a connection's login method ([`SERVICE_AUTH`]) and the fields it uses, leaving out what
/// the method doesn't use. A query login's parameter is taken out of the address (the key is given
/// to the login, never kept there): whether it was.
fn clean_login(c: &mut Connection) -> Result<bool, String> {
    use crate::render::rustgis_tiles::logins;
    let auth = c.auth.trim().to_ascii_lowercase();
    c.auth = if auth == "none" { String::new() } else { auth };
    c.user = c.user.trim().to_string();
    c.key_name = c.key_name.trim().to_string();
    if c.auth.is_empty() {
        c.user.clear();
        c.key_name.clear();
        return Ok(false);
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
            Ok(false)
        }
        "header" => {
            c.user.clear();
            if !logins::valid_header_name(&c.key_name) {
                return Err(format!(
                    "keyName must be an HTTP header name of at most {} letters, digits or one of !#$%&'*+-.^_`|~ (not one the app sets itself: Host, Cookie, Range, Proxy-… or Sec-…, among others)",
                    logins::MAX_NAME
                ));
            }
            Ok(false)
        }
        "query" => {
            c.user.clear();
            if !logins::valid_query_name(&c.key_name) {
                return Err(format!(
                    "keyName must be a query parameter name of at most {} letters, digits or one of -_.~, with a letter or digit",
                    logins::MAX_NAME
                ));
            }
            let kept = logins::without_query_parameter(&c.address, &c.key_name);
            let stripped = kept != c.address;
            c.address = kept;
            Ok(stripped)
        }
        other => Err(format!("unknown auth '{other}' ({})", SERVICE_AUTH.join(", "))),
    }
}

/// Registers a query login only after its definition has been accepted, before storing it.
/// Validation and previews must not reserve names in the bounded, process-wide registry.
pub(crate) fn register_query_name(c: &Connection) -> Result<(), String> {
    if c.auth == "query" && !rustgis_data::address::note_login_query_name(&c.key_name) {
        return Err(crate::render::rustgis_tiles::logins::TOO_MANY_QUERY_NAMES.into());
    }
    Ok(())
}

/// The names (never the values) of the query parameters `given` had that `saved` lacks, which a
/// query login can carry: the keys an address held and saving left out, to offer as the login's
/// parameter. At most four, each once. Only `name=value` pairs count: a bare segment can't be a
/// login's parameter, and when it is a key written as a name, its text is the secret.
pub fn stripped_parameters(given: &str, saved: &str) -> Vec<String> {
    use crate::render::rustgis_tiles::logins;
    fn names(address: &str) -> Vec<&str> {
        let query = address.split('#').next().and_then(|a| a.split_once('?')).map_or("", |(_, query)| query);
        query.split('&').filter_map(|pair| pair.split_once('=').map(|(name, _)| name)).filter(|name| !name.is_empty()).collect()
    }
    let kept = names(saved);
    let mut left_out: Vec<String> = Vec::new();
    for name in names(given) {
        if left_out.len() < 4
            && logins::valid_query_name(name)
            && !kept.contains(&name)
            && !left_out.iter().any(|known| known.eq_ignore_ascii_case(name))
        {
            left_out.push(name.to_string());
        }
    }
    left_out
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
    // A query login's own parameter goes before anything quotes the address (an error included):
    // its name may not be registered yet, so the generic redaction can't see it.
    let query_name = Some(c.key_name.trim()).filter(|name| c.auth.trim().eq_ignore_ascii_case("query") && !name.is_empty());
    let given = query_name.map_or_else(|| t.to_string(), |name| crate::render::rustgis_tiles::logins::without_query_parameter(t, name));
    let stored = sources::redact(&given);
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
    let stripped = clean_login(&mut c)? || stripped;
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

    /// The connections the `connections` preference holds, each checked and cleaned again (the
    /// preferences are a file anyone could have edited). What this version can't use is kept, to
    /// be written back by [`Connections::to_json`] as it was: rows it can't read and fields it
    /// doesn't know, or the whole preference when it is from a newer RustGIS (a higher
    /// `version`) or isn't a JSON object, in which case no connection is read and none can be
    /// added. [`Connections::unread`] says what was kept.
    pub fn from_json(text: &str) -> Self {
        let mut s = Self { version: 1, next: 1, ..Self::default() };
        if text.trim().is_empty() {
            return s;
        }
        let whole = |why: String| Kept { whole: Some((text.to_string(), why)), ..Kept::default() };
        let mut o = match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(o)) => o,
            _ => {
                s.kept = whole(UNREADABLE.replace("{why}", "they aren't a JSON object"));
                return s;
            }
        };
        match o.get("version").map(Value::as_u64) {
            None | Some(Some(0..=VERSION)) => {}
            Some(Some(v)) => {
                s.kept = whole(format!(
                    "the saved connections are from a newer RustGIS (version {v}; this one reads {VERSION}); update RustGIS to use or change them"
                ));
                return s;
            }
            Some(None) => {
                s.kept = whole(UNREADABLE.replace("{why}", "their version isn't a whole number"));
                return s;
            }
        }
        let rows = match o.remove("connections") {
            None => Vec::new(),
            Some(Value::Array(rows)) => rows,
            Some(_) => {
                s.kept = whole(UNREADABLE.replace("{why}", "their list isn't a JSON array"));
                return s;
            }
        };
        s.next = o.get("next").and_then(Value::as_u64).unwrap_or(1).max(1);
        o.remove("version");
        o.remove("next");
        let mut ids = BTreeSet::new();
        let mut pending_rows = Vec::new();
        let mut stripped_flags = Vec::new();
        for row in rows {
            // Readability is checked even past the limit, without registering the login's name.
            let read = serde_json::from_value::<Row>(row.clone()).ok().and_then(|r| {
                let name = r.connection.name.trim().to_string();
                let group = r.connection.group.trim().to_string();
                let (c, stripped) = clean(r.connection).ok()?;
                // What reading changed in the row, to be said: a secret taken out of the address, a name
                // or group cut.
                let changed = (stripped, (!name.is_empty() && c.name != name) || (!group.is_empty() && c.group != group));
                Some(((c, changed), r.other))
            });
            let read = read.filter(|((c, _), _)| {
                !c.id.is_empty()
                    && c.id.len() <= 32
                    && c.id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                    && !ids.contains(&c.id)
            });
            let over_limit = read.is_some() && s.connections.len() >= MAX_CONNECTIONS;
            let read = read.filter(|_| !over_limit);
            let registry_full = read.as_ref().is_some_and(|((c, _), _)| register_query_name(c).is_err());
            let read = read.filter(|_| !registry_full);
            let reason = if over_limit {
                KeptReason::PastLimit
            } else if registry_full {
                KeptReason::NameRegistry
            } else {
                KeptReason::Unreadable
            };
            let id = match &read {
                Some(((c, _), _)) => c.id.clone(),
                None => row.get("id").and_then(Value::as_str).unwrap_or_default().to_string(),
            };
            match read {
                Some(((c, (stripped, cut)), other)) => {
                    s.kept.stripped += usize::from(stripped);
                    stripped_flags.push(stripped);
                    s.kept.cut += usize::from(cut);
                    if let Some(n) = id.strip_prefix('c').and_then(|n| n.parse::<u64>().ok()) {
                        s.next = s.next.max(n.saturating_add(1));
                    }
                    ids.insert(c.id.clone());
                    if !other.is_empty() {
                        s.kept.fields.insert(c.id.clone(), other);
                    }
                    s.connections.push(c);
                }
                None if holds_query_secret(&row) => s.kept.secrets += 1,
                None => pending_rows.push((row, reason, id)),
            }
        }
        // All accepted names are now known. Earlier addresses and opaque settings must be
        // classified using the same names as later rows, independent of file order.
        s.kept.top = s.kept.without_secrets(o, "");
        for (c, stripped) in s.connections.iter_mut().zip(stripped_flags) {
            let address = sources::redact(&c.address);
            s.kept.stripped += usize::from(!stripped && address != c.address);
            c.address = address;
        }
        let fields = std::mem::take(&mut s.kept.fields);
        for (id, other) in fields {
            let address = s.connections.iter().find(|c| c.id == id).map_or("", |c| c.address.as_str());
            let other = s.kept.without_secrets(other, address);
            if !other.is_empty() {
                s.kept.fields.insert(id, other);
            }
        }
        for (row, reason, id) in pending_rows {
            if sources::holds_secret(&row) {
                s.kept.secrets += 1;
            } else {
                // Keep the readability reason with the row if later redaction removes others.
                if !id.is_empty() {
                    s.kept.ids.insert(id);
                }
                s.kept.rows.push(KeptRow { value: row, reason });
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

    /// What of the preference read by [`Connections::from_json`] isn't used here, one line per
    /// kind (none when all of it is).
    pub fn unread(&self) -> Vec<String> {
        let k = &self.kept;
        if let Some((_, why)) = &k.whole {
            return vec![format!("No saved connection is used, and none can be added: {why}. They are kept as they are.")];
        }
        let mut said = Vec::new();
        let count = |reason| k.rows.iter().filter(|row| row.reason == reason).count();
        let (unreadable, over_limit, registry) =
            (count(KeptReason::Unreadable), count(KeptReason::PastLimit), count(KeptReason::NameRegistry));
        if unreadable > 0 {
            let n = unreadable;
            said.push(format!(
                "{n} saved connection{} couldn't be read (from a newer RustGIS, or damaged): not listed, and kept as {} when connections are saved",
                if n == 1 { "" } else { "s" },
                if n == 1 { "it was" } else { "they were" }
            ));
        }
        if over_limit > 0 {
            let n = over_limit;
            said.push(format!(
                "{n} saved connection{} past the {MAX_CONNECTIONS}-connection limit: not listed, and kept as {} when connections are saved",
                if n == 1 { " is" } else { "s are" },
                if n == 1 { "it was" } else { "they were" }
            ));
        }
        if registry > 0 {
            let n = registry;
            said.push(format!(
                "{n} saved connection{} a query parameter name past the {} this run can hold for redaction (restart RustGIS to use {}): not listed, and kept as {} when connections are saved",
                if n == 1 { " uses" } else { "s use" },
                rustgis_data::address::MAX_LOGIN_NAMES,
                if n == 1 { "it" } else { "them" },
                if n == 1 { "it was" } else { "they were" }
            ));
        }
        if !k.fields.is_empty() {
            let n = k.fields.len();
            said.push(format!(
                "{n} saved connection{} settings this version doesn't know (from a newer RustGIS): ignored here, and kept when connections are saved",
                if n == 1 { " has" } else { "s have" }
            ));
        }
        if k.cut > 0 {
            let n = k.cut;
            said.push(format!(
                "{n} saved connection name{} or group{} cut to {MAX_NAME} characters or without control characters: saved cut",
                if n == 1 { "" } else { "s" },
                if n == 1 { " was" } else { "s were" }
            ));
        }
        said.extend(self.redaction_notices_since((0, 0)));
        said
    }

    /// Counts already reported on load, so a command reports only its newly removed secrets.
    pub(crate) fn redaction_counts(&self) -> (usize, usize) {
        (self.kept.stripped, self.kept.secrets)
    }

    pub(crate) fn redaction_notices_since(&self, before: (usize, usize)) -> Vec<String> {
        let mut said = Vec::new();
        let (addresses, entries) = self.redaction_counts();
        let addresses = addresses.saturating_sub(before.0);
        let entries = entries.saturating_sub(before.1);
        if addresses > 0 {
            said.push(format!(
                "{addresses} saved connection address{} had a password, key or token removed: saved without {}",
                if addresses == 1 { "" } else { "es" },
                if addresses == 1 { "it" } else { "them" }
            ));
        }
        if entries > 0 {
            said.push(format!(
                "{entries} entr{} of the saved connections {} dropped because {} held a password, key or token",
                if entries == 1 { "y" } else { "ies" },
                if entries == 1 { "was" } else { "were" },
                if entries == 1 { "it" } else { "they" }
            ));
        }
        said
    }

    /// A newly accepted query name can identify secrets in earlier addresses and opaque settings.
    /// Refresh those owners before saving; previews and rejected definitions never reach here.
    pub(crate) fn refresh_redaction(&mut self) {
        if self.kept.whole.is_some() {
            return;
        }
        for c in &mut self.connections {
            let address = sources::redact(&c.address);
            self.kept.stripped += usize::from(address != c.address);
            c.address = address;
        }
        let top = std::mem::take(&mut self.kept.top);
        self.kept.top = self.kept.without_secrets(top, "");
        let fields = std::mem::take(&mut self.kept.fields);
        for (id, other) in fields {
            let address = self.connections.iter().find(|c| c.id == id).map_or("", |c| c.address.as_str());
            let other = self.kept.without_secrets(other, address);
            if !other.is_empty() {
                self.kept.fields.insert(id, other);
            }
        }
        let mut dropped = 0;
        self.kept.rows.retain(|row| {
            let secret = sources::holds_secret(&row.value);
            dropped += usize::from(secret);
            !secret
        });
        self.kept.secrets += dropped;
        if dropped > 0 {
            self.kept.ids = self
                .kept
                .rows
                .iter()
                .filter_map(|row| row.value.get("id").and_then(Value::as_str).filter(|id| !id.is_empty()).map(str::to_owned))
                .collect();
        }
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

    /// The `connections` preference: these connections, with what [`Connections::from_json`]
    /// kept written back as it was (a preference it didn't read at all, unchanged).
    pub fn to_json(&self) -> String {
        if let Some((text, _)) = &self.kept.whole {
            return text.clone();
        }
        let mut rows: Vec<Value> = Vec::with_capacity(self.connections.len() + self.kept.rows.len());
        for c in &self.connections {
            let mut row = serde_json::to_value(c).unwrap_or(Value::Null);
            if let (Value::Object(row), Some(other)) = (&mut row, self.kept.fields.get(&c.id)) {
                for (k, v) in other {
                    row.entry(k.clone()).or_insert_with(|| v.clone());
                }
            }
            rows.push(row);
        }
        rows.extend(self.kept.rows.iter().map(|row| row.value.clone()));
        let mut o = self.kept.top.clone();
        o.insert("version".into(), json!(VERSION));
        o.insert("next".into(), json!(self.next));
        o.insert("connections".into(), Value::Array(rows));
        Value::Object(o).to_string()
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

    /// Adds a connection under a new id (never one a kept row has), named uniquely; whether its
    /// address was stripped of secrets. Refused while the preference is kept whole (from a newer
    /// RustGIS, or unreadable), since it couldn't be saved without losing it.
    pub fn add(&mut self, c: Connection, explicit_name: bool) -> Result<(Connection, bool), String> {
        if let Some((_, why)) = &self.kept.whole {
            return Err(format!("a connection can't be added: {why}"));
        }
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
        while self.connections.iter().any(|c| c.id == format!("c{next}")) || self.kept.ids.contains(&format!("c{next}")) {
            next = next.checked_add(1).ok_or("connection ids exhausted")?;
        }
        let following = next.checked_add(1).ok_or("connection ids exhausted")?;
        register_query_name(&c)?;
        self.next = following;
        c.id = format!("c{next}");
        // A new connection, a copy or an import has no saved password: it is saved by a login.
        c.remembered = false;
        self.connections.push(c.clone());
        if c.auth == "query" {
            self.refresh_redaction();
        }
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
    /// The caller refreshes retained redaction after committing a query login.
    pub(crate) fn commit_update(slot: &mut Connection, generation: &mut u64, c: Connection) -> Connection {
        *slot = c.clone();
        *generation = generation.wrapping_add(1);
        c
    }

    pub fn update(&mut self, id: &str, c: Connection) -> Result<(Connection, bool), String> {
        let (c, stripped) = self.prepare_update(id, c)?;
        register_query_name(&c)?;
        let slot = self.connections.iter_mut().find(|slot| slot.id == id).ok_or_else(|| format!("no connection '{id}'"))?;
        let c = Self::commit_update(slot, &mut self.generation, c);
        if c.auth == "query" {
            self.refresh_redaction();
        }
        Ok((c, stripped))
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

    /// Removes connection `id`, with the fields this version didn't know that it kept.
    pub fn remove(&mut self, id: &str) -> Option<Connection> {
        let index = self.connections.iter().position(|c| c.id == id)?;
        let c = self.connections.remove(index);
        self.kept.fields.remove(id);
        self.kept.rows.retain(|row| row.value.get("id").and_then(Value::as_str) != Some(id));
        self.kept.ids.remove(id);
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

    /// Rows, fields and keys a newer RustGIS added without a new version are kept through an edit
    /// here, and a new connection never takes the id of a row that couldn't be read.
    #[test]
    fn connections_keep_rows_and_fields_this_version_cannot_read() {
        let future = json!({"id":"c7","name":"Bucket","kind":"gcs","address":"https://g.example","region":"eu"});
        let text = json!({"version":1,"next":7,"layout":{"folded":["c1"]},"connections":[
            {"id":"c1","name":"WMS","kind":"wms","address":"https://w.example/wms","refresh":30},
            future.clone(),
            null
        ]})
        .to_string();
        let mut s = Connections::from_json(&text);
        assert_eq!(s.connections.len(), 1);
        assert_eq!(s.unread().len(), 2, "{:?}", s.unread());
        assert!(s.unread()[0].starts_with("2 saved connections couldn't be read"), "{:?}", s.unread());
        let (added, _) =
            s.add(Connection { kind: "wms".into(), address: "https://n.example/wms".into(), ..Connection::default() }, false).unwrap();
        assert_eq!(added.id, "c8", "the unread row's id stays its own");
        s.update("c1", Connection { name: "Renamed".into(), ..s.connections[0].clone() }).unwrap();
        let saved: Value = serde_json::from_str(&s.to_json()).unwrap();
        assert_eq!(saved["layout"], json!({"folded":["c1"]}));
        let rows = saved["connections"].as_array().unwrap();
        assert_eq!((rows[0]["name"].as_str(), &rows[0]["refresh"]), (Some("Renamed"), &json!(30)));
        assert_eq!(rows[1]["id"], "c8");
        assert_eq!(&rows[2..], [future, Value::Null]);
        // Read again, the same is kept; a removed connection takes its unknown fields with it.
        let mut again = Connections::from_json(&s.to_json());
        assert_eq!(again.connections, s.connections);
        again.remove("c1");
        assert!(!again.to_json().contains("refresh"));
        assert!(Connections::from_json(&again.to_json()).unread()[0].starts_with("2 saved"));
        // What it can't read and that holds a secret isn't kept: this version can't strip it.
        let secret = json!({"version":1,"password":"top","connections":[
            {"id":"c1","name":"A","kind":"wms","address":"https://a","token":"hidden"},
            {"id":"c2","name":"B","kind":"gcs","address":"https://u:hidden@g.example"}
        ]});
        let s = Connections::from_json(&secret.to_string());
        assert!(
            s.unread()
                .iter()
                .any(|t| t.starts_with("3 entries of the saved connections were dropped because they held a password, key or token")),
            "{:?}",
            s.unread()
        );
        assert!(!s.to_json().contains("hidden") && !s.to_json().contains("top"));
        // A key is one beside a web address, even when both are fields this version doesn't know.
        let beside =
            json!({"connections":[{"id":"c1","name":"A","kind":"wms","address":"https://a","endpoint":"https://api.example","key":"abc"}]});
        let s = Connections::from_json(&beside.to_string());
        assert!(!s.to_json().contains("\"abc\""), "{}", s.to_json());
        assert!(s.to_json().contains("api.example"));
        // Or beside an address among the preference's own keys.
        let top = json!({"address":"https://api.example","key":"abc","connections":[]});
        assert!(!Connections::from_json(&top.to_string()).to_json().contains("\"abc\""));
        // A file's contents beside a field isn't an address for it.
        let file = json!({"data":"https://example.invalid/a.csv","key":"feature_id","connections":[]});
        assert!(Connections::from_json(&file.to_string()).to_json().contains("feature_id"));
        // Even when that address holds a secret itself.
        let top = json!({"address":"https://api.example?token=hidden","key":"abc","connections":[]});
        let saved = Connections::from_json(&top.to_string()).to_json();
        assert!(!saved.contains("abc") && !saved.contains("hidden"), "{saved}");
        // Or beside the row's own address.
        let own = json!({"connections":[{"id":"c1","name":"A","kind":"wms","address":"https://a","key":"abc","note":"n"}]});
        let s = Connections::from_json(&own.to_string());
        assert!(!s.to_json().contains("\"abc\"") && s.to_json().contains("\"note\""), "{}", s.to_json());
        // What reading cuts from a readable row is said: a secret in the address, a long name.
        let cut = json!({"connections":[
            {"id":"c1","name":"A","kind":"wms","address":"https://a/wms?token=hidden"},
            {"id":"c2","name":"n".repeat(300),"kind":"wms","address":"https://b/wms"},
            {"id":"c3","name":" C ","kind":"wms","address":"https://c/wms"},
            {"id":"c4","name":"D","group":"g".repeat(300),"kind":"wms","address":"https://d/wms"}
        ]});
        let s = Connections::from_json(&cut.to_string());
        assert!(s.unread().iter().any(|t| t.starts_with("1 saved connection address had a password")), "{:?}", s.unread());
        assert!(s.unread().iter().any(|t| t.starts_with("2 saved connection names or groups were cut")), "{:?}", s.unread());
        assert_eq!(s.unread().len(), 2);
        // Rows past the limit are kept and said as that, not as unreadable.
        let many: Vec<Value> = (1..=MAX_CONNECTIONS + 1)
            .map(|i| json!({"id": format!("c{i}"), "name": format!("N{i}"), "kind": "wms", "address": "https://a"}))
            .collect();
        let s = Connections::from_json(&json!({ "connections": many }).to_string());
        assert_eq!(s.connections.len(), MAX_CONNECTIONS);
        assert_eq!(
            s.unread(),
            [format!(
                "1 saved connection is past the {MAX_CONNECTIONS}-connection limit: not listed, and kept as it was when connections are saved"
            )]
        );
        assert!(s.to_json().contains(&format!("\"c{}\"", MAX_CONNECTIONS + 1)));
        // Removing a connection removes a second row with its id too, so it can't come back.
        let twice = json!({"connections":[
            {"id":"c1","name":"A","kind":"wms","address":"https://a"},
            {"id":"c1","name":"B","kind":"wms","address":"https://b"}
        ]});
        let mut s = Connections::from_json(&twice.to_string());
        s.remove("c1");
        assert!(Connections::from_json(&s.to_json()).connections.is_empty() && !s.to_json().contains("\"c1\""), "{}", s.to_json());
    }

    #[test]
    fn rejected_query_definitions_do_not_register_names() {
        let mut s = Connections::default();
        let make = |name: &str, key: &str| Connection {
            kind: "wmts".into(),
            name: name.into(),
            address: "https://rejected757.example/wmts".into(),
            auth: "query".into(),
            key_name: key.into(),
            ..Connection::default()
        };
        let (accepted, _) = s.add(make("Accepted", "zqacceptreject757"), true).unwrap();
        assert!(s.add(make("Accepted", "zqduplicateadd757"), true).is_err());
        assert!(s.update("missing", make("Missing", "zqmissingedit757")).is_err());
        let (other, _) = s.add(make("Other", "zqotherreject757"), true).unwrap();
        assert!(s.update(&other.id, make("Accepted", "zqduplicateedit757")).is_err());
        let mut preview = accepted.clone();
        preview.key_name = "zqpreview757".into();
        s.prepare_update(&accepted.id, preview).unwrap();
        clean(make("Clean preview", "zqcleanpreview757")).unwrap();
        let rows = json!({"connections":[
            {"id":"same","name":"Accepted row","kind":"wmts","address":"https://rejected757.example/wmts","auth":"query","keyName":"zqrowaccepted757"},
            {"id":"same","name":"Duplicate row","kind":"wmts","address":"https://rejected757.example/wmts","auth":"query","keyName":"zqduplicaterow757"},
            {"id":"bad!","name":"Invalid row","kind":"wmts","address":"https://rejected757.example/wmts","auth":"query","keyName":"zqinvalidrow757"}
        ]});
        let loaded = Connections::from_json(&rows.to_string());
        assert_eq!(loaded.connections.len(), 1);
        assert!(loaded.unread().iter().any(|t| t.starts_with("2 saved connections couldn't be read")));
        for key in [
            "zqduplicateadd757",
            "zqmissingedit757",
            "zqduplicateedit757",
            "zqpreview757",
            "zqcleanpreview757",
            "zqduplicaterow757",
            "zqinvalidrow757",
        ] {
            let original = format!("https://rejected757.example/wmts?{key}=RGSYNTH_rejected");
            let mut address = original.clone();
            rustgis_data::address::redact_query(&mut address);
            assert_eq!(address, original, "rejected definition registered {key}");
        }
        for key in ["zqacceptreject757", "zqrowaccepted757"] {
            let mut address = format!("https://rejected757.example/wmts?{key}=RGSYNTH_accepted");
            rustgis_data::address::redact_query(&mut address);
            assert_eq!(address, "https://rejected757.example/wmts");
        }
    }

    #[test]
    fn refused_query_rows_never_preserve_their_keys() {
        // Fill the process-wide registry in an isolated subprocess, not the full suite's process.
        const CHILD: &str = "RUSTGIS_QUERY_REGISTRY_TEST";
        if std::env::var_os(CHILD).is_none() {
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "connections::tests::refused_query_rows_never_preserve_their_keys", "--nocapture"])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(result.status.success(), "{}\n{}", String::from_utf8_lossy(&result.stdout), String::from_utf8_lossy(&result.stderr));
            return;
        }
        for i in 0..rustgis_data::address::MAX_LOGIN_NAMES {
            assert!(rustgis_data::address::note_login_query_name(&format!("zqfull{i}")));
        }
        let row = |address: &str| json!({"id":"c1","name":"Refused","kind":"wmts","auth":"query","keyName":"custom_unregistered757","address":address});
        let key = "RGSYNTH_refused_query_secret";
        let lossy = Connections::from_json(
            &json!({"connections":[row(&format!("https://refused757.example/wmts?custom_unregistered757={key}"))]}).to_string(),
        );
        assert!(lossy.connections.is_empty());
        assert!(!lossy.to_json().contains(key));
        assert_eq!(lossy.kept.secrets, 1);
        assert!(lossy.unread().iter().any(|t| t.contains("dropped")));
        let clean = Connections::from_json(&json!({"connections":[row("https://refused757.example/wmts?layer=a")]}).to_string());
        assert!(clean.connections.is_empty());
        assert!(clean.to_json().contains("custom_unregistered757"), "a clean unread row is preserved");
        // Readable, refused only because the registry is full: said as that, not as unreadable.
        let said = clean.unread();
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(said[0].contains("query parameter name past the 256") && said[0].contains("restart RustGIS"), "{said:?}");
    }

    #[test]
    fn unsupported_query_names_do_not_preserve_explicit_secrets() {
        let row = |address: &str| json!({"id":"c1","name":"Unsupported","kind":"wmts","auth":"query","keyName":"_","address":address,"future":{"nested":[{"url":"https://h/p?layer=a"}]}});
        let secret = "RGSYNTH_unsupported_query";
        let lossy = Connections::from_json(&json!({"connections":[row(&format!("https://h/p?%255f={secret}"))]}).to_string());
        assert!(!lossy.to_json().contains(secret));
        assert_eq!(lossy.kept.secrets, 1);
        assert!(lossy.unread().iter().any(|t| t.contains("dropped")));
        let mut metadata = row("https://h/p?layer=a");
        metadata["future"] = json!({"nested":[{"url":format!("https://h/p?%255f={secret}")}]});
        let lossy_metadata = Connections::from_json(&json!({"connections":[metadata]}).to_string());
        assert!(!lossy_metadata.to_json().contains(secret));
        assert_eq!(lossy_metadata.kept.secrets, 1);
        assert!(lossy_metadata.unread().iter().any(|t| t.contains("dropped")));
        let clean = Connections::from_json(&json!({"connections":[row("https://h/p?layer=a")]}).to_string());
        assert!(clean.to_json().contains("Unsupported"));
        assert!(clean.to_json().contains("future"));
        assert_eq!(clean.kept.secrets, 0);
        assert!(!clean.unread().iter().any(|t| t.contains("dropped")));
    }

    #[test]
    fn query_login_names_preserve_distinct_punctuation() {
        let saved = json!({"connections":[
            {"id":"c1","name":"Query","kind":"wmts","address":"https://h/p?zqpunct757=dataset","auth":"query","keyName":"zq_punct757"}
        ]});
        let s = Connections::from_json(&saved.to_string());
        assert_eq!(s.connections[0].address, "https://h/p?zqpunct757=dataset");
        assert!(s.unread().is_empty());
        assert!(s.to_json().contains("zqpunct757=dataset"));
    }

    #[test]
    fn query_names_clean_encoded_addresses_and_earlier_preserved_settings() {
        let secret = "RGSYNTH_ordered_query";
        let query = json!({"id":"c2","name":"Query","kind":"wmts","auth":"query","keyName":"zqordering757","address":format!("https://h/p?zq%256frdering757={secret}&layer=a")});
        let saved = json!({
            "futureEndpoint": format!("https://h/p?zqordering757={secret}"),
            "futureClean": "https://h/p?layer=a",
            "connections": [
                {"id":"c1","name":"Earlier","kind":"wms","address":format!("https://h/p?zqordering757={secret}"),"futureEndpoint":format!("https://h/p?zqordering757={secret}"),"futureClean":"https://h/p?layer=a"},
                {"id":"bad!","name":"Unread","kind":"future","address":format!("https://h/p?zqordering757={secret}")},
                query
            ]
        });
        let s = Connections::from_json(&saved.to_string());
        assert_eq!(s.connections.len(), 2);
        assert_eq!(s.connections[0].address, "https://h/p");
        assert_eq!(s.connections[1].address, "https://h/p?layer=a");
        assert!(!s.to_json().contains(secret));
        assert!(s.to_json().contains("futureClean"));
        assert_eq!(s.kept.stripped, 2);
        assert_eq!(s.kept.secrets, 3, "top field, earlier row's field, and unread row");
        assert!(s.unread().iter().any(|t| t.contains("dropped")));
        let clean = Connections::from_json(&json!({"futureClean":"https://h/p?layer=a","connections":[{"id":"c1","name":"Clean","kind":"wmts","auth":"query","keyName":"zqordering757","address":"https://h/p?layer=a"}]}).to_string());
        assert!(clean.unread().is_empty());
        assert!(clean.to_json().contains("futureClean"));
    }

    /// A saved query login that can't be read is kept and said. Readability is checked past
    /// the limit too, without registering those rows' query names.
    #[test]
    fn query_login_rows_refused_or_past_the_limit_are_said_and_hold_no_name() {
        let query = |i: usize, key: &str| json!({"id": format!("c{i}"), "name": format!("Q{i}"), "kind": "wmts", "address": "https://limit757.example/wmts", "auth": "query", "keyName": key});
        let mut rows = vec![query(1, "_")];
        rows.extend((2..=MAX_CONNECTIONS + 1).map(|i| query(i, "zqshared757")));
        rows.push(query(MAX_CONNECTIONS + 2, "zqbeyond757"));
        let s = Connections::from_json(&json!({ "connections": rows }).to_string());
        assert_eq!(s.connections.len(), MAX_CONNECTIONS);
        let said = s.unread();
        assert!(said.iter().any(|t| t.starts_with("1 saved connection couldn't be read")), "{said:?}");
        assert!(
            said.iter().any(|t| t.starts_with(&format!("1 saved connection is past the {MAX_CONNECTIONS}-connection limit"))),
            "{said:?}"
        );
        let text = s.to_json();
        assert!(text.contains("zqbeyond757") && text.contains("\"c1\""), "kept as they were: {text}");
        let mut beyond = "https://limit757.example/wmts?zqbeyond757=RGSYNTH_v".to_string();
        rustgis_data::address::redact_query(&mut beyond);
        assert!(beyond.contains("RGSYNTH_v"), "a row past the limit registered its name: {beyond}");
        let mut shared = "https://limit757.example/wmts?zqshared757=RGSYNTH_v&a=1".to_string();
        rustgis_data::address::redact_query(&mut shared);
        assert_eq!(shared, "https://limit757.example/wmts?a=1");
    }

    #[test]
    fn malformed_rows_past_the_limit_keep_their_readability_reason() {
        let full: Vec<Value> = (1..=MAX_CONNECTIONS)
            .map(|i| json!({"id":format!("c{i}"),"name":format!("N{i}"),"kind":"wmts","address":"https://tail757.example/wmts"}))
            .collect();
        let clean = Connections::from_json(&json!({"connections":full}).to_string());
        assert!(clean.unread().is_empty());
        let mut rows = full.clone();
        let tail = vec![
            Value::Null,
            json!({"id":"future","name":"Future","kind":"future","address":"https://tail757.example/wmts","auth":"query","keyName":"zqtailkind757"}),
            json!({"id":"bad!","name":"Bad id","kind":"wmts","address":"https://tail757.example/wmts","auth":"query","keyName":"zqtailid757"}),
            json!({"id":"malformed","name":7,"kind":"wmts","address":"https://tail757.example/wmts","auth":"query","keyName":"zqtailmalformed757"}),
            json!({"id":"c1","name":"Duplicate","kind":"wmts","address":"https://tail757.example/wmts","auth":"query","keyName":"zqtailduplicate757"}),
            json!({"id":"readable","name":"Readable","kind":"wmts","address":"https://tail757.example/wmts","auth":"query","keyName":"zqtailreadable757"}),
        ];
        rows.extend(tail.clone());
        let s = Connections::from_json(&json!({"connections":rows}).to_string());
        assert_eq!(s.connections.len(), MAX_CONNECTIONS);
        let said = s.unread();
        assert_eq!(said.len(), 2, "{said:?}");
        assert!(said[0].starts_with("5 saved connections couldn't be read"), "{said:?}");
        assert!(said[1].starts_with("1 saved connection is past the 200-connection limit"), "{said:?}");
        let saved: Value = serde_json::from_str(&s.to_json()).unwrap();
        assert_eq!(&saved["connections"].as_array().unwrap()[MAX_CONNECTIONS..], tail);
        for key in ["zqtailkind757", "zqtailid757", "zqtailmalformed757", "zqtailduplicate757", "zqtailreadable757"] {
            let original = format!("https://tail757.example/wmts?{key}=RGSYNTH_tail");
            assert_eq!(sources::redact(&original), original, "unaccepted tail registered {key}");
        }
        let mut readable = full;
        readable.push(tail.last().unwrap().clone());
        let s = Connections::from_json(&json!({"connections":readable}).to_string());
        assert_eq!(s.unread().len(), 1);
        assert!(s.unread()[0].starts_with("1 saved connection is past the 200-connection limit"));
    }

    #[test]
    fn public_query_mutations_refresh_previously_retained_secrets() {
        for edit in [false, true] {
            for lossy in [false, true] {
                // Each case has its own name: the process-wide registry must not pre-clean it.
                let key = format!("zqpublicrefresh757{}{}", usize::from(edit), usize::from(lossy));
                let endpoint = if lossy {
                    format!("https://refresh757.example/wmts?{key}=RGSYNTH_late_public&layer=a")
                } else {
                    "https://refresh757.example/wmts?layer=a".to_string()
                };
                let unread = json!({"id":"future","kind":"future","address":endpoint,"futureSetting":17});
                let safe_unread =
                    json!({"id":"safe","kind":"future","address":"https://refresh757.example/wmts?layer=a","futureSetting":23});
                let saved = json!({
                    "futureEndpoint":endpoint,"futureClean":{"folded":["c1"]},
                    "connections":[
                        {"id":"c1","name":"Earlier","kind":"wmts","address":endpoint,"futureEndpoint":endpoint,"futureClean":31},
                        {"id":"c2","name":"Target","kind":"wmts","address":"https://refresh757.example/target"},
                        unread,safe_unread
                    ]
                });
                let mut s = Connections::from_json(&saved.to_string());
                assert_eq!(s.redaction_counts(), (0, 0));
                let before = s.redaction_counts();
                let query = Connection {
                    name: "Query".into(),
                    kind: "wmts".into(),
                    address: "https://refresh757.example/target".into(),
                    auth: "query".into(),
                    key_name: key,
                    ..Connection::default()
                };
                if edit {
                    s.update("c2", query).unwrap();
                } else {
                    s.add(query, true).unwrap();
                }
                let text = s.to_json();
                let output: Value = serde_json::from_str(&text).unwrap();
                assert_eq!(output["futureClean"], saved["futureClean"]);
                assert_eq!(output["connections"][0]["futureClean"], 31);
                assert_eq!(s.find("c1").unwrap().address, "https://refresh757.example/wmts?layer=a");
                assert!(output["connections"].as_array().unwrap().contains(&safe_unread));
                if lossy {
                    assert!(!text.contains("RGSYNTH_late_public"), "{text}");
                    assert!(output.get("futureEndpoint").is_none());
                    assert!(output["connections"][0].get("futureEndpoint").is_none());
                    assert!(!output["connections"].as_array().unwrap().contains(&unread));
                    assert_eq!(s.redaction_counts(), (1, 3));
                    assert_eq!(s.redaction_notices_since(before).len(), 2);
                } else {
                    assert_eq!(output["futureEndpoint"], saved["futureEndpoint"]);
                    assert_eq!(output["connections"][0]["futureEndpoint"], saved["connections"][0]["futureEndpoint"]);
                    assert!(output["connections"].as_array().unwrap().contains(&unread));
                    assert_eq!(s.redaction_counts(), (0, 0));
                    assert!(s.redaction_notices_since(before).is_empty());
                }
            }
        }
    }

    #[test]
    fn unsupported_literal_plus_query_rows_withhold_only_the_secret_case() {
        let row = |address: &str| {
            json!({
                "id":"plus","name":"Unsupported plus","kind":"wmts","auth":"query","keyName":"zq+custom757",
                "address":address,"futureClean":{"choice":13}
            })
        };
        let lossy = Connections::from_json(
            &json!({"connections":[row("https://plus757.example/wmts?zq%2Bcustom757=RGSYNTH_refused_plus&layer=a")]}).to_string(),
        );
        assert!(lossy.connections.is_empty());
        assert!(!lossy.to_json().contains("RGSYNTH_refused_plus"));
        assert_eq!(lossy.redaction_counts(), (0, 1));
        assert!(lossy.unread().iter().any(|said| said.starts_with("1 entry of the saved connections was dropped")));
        let clean_row = row("https://plus757.example/wmts?zqcustom757=dataset&layer=a");
        let clean = Connections::from_json(&json!({"connections":[clean_row]}).to_string());
        let saved: Value = serde_json::from_str(&clean.to_json()).unwrap();
        assert_eq!(saved["connections"][0], clean_row);
        assert_eq!(clean.redaction_counts(), (0, 0));
        assert!(!clean.unread().iter().any(|said| said.contains("dropped")));
    }

    /// A preference from a newer RustGIS, or one that can't be read at all, isn't used: it is said,
    /// adding a connection is refused, and it is written back byte for byte. Clean input says
    /// nothing.
    #[test]
    fn connections_from_a_newer_version_or_unreadable_are_kept_whole() {
        let newer = r#"{"version": 2, "next": 4, "connections": [{"id":"c3","name":"A","kind":"wms","address":"https://a"}]}"#;
        for (text, said) in [
            (newer, "from a newer RustGIS (version 2; this one reads 1)"),
            ("[1, 2]", "aren't a JSON object"),
            (r#"{"version": "2"}"#, "isn't a whole number"),
            (r#"{"connections": {"c1": {}}}"#, "isn't a JSON array"),
        ] {
            let mut s = Connections::from_json(text);
            assert!(s.connections.is_empty(), "{text}");
            assert!(matches!(s.unread().as_slice(), [one] if one.contains(said)), "{text}: {:?}", s.unread());
            let c = Connection { kind: "wms".into(), address: "https://n.example".into(), ..Connection::default() };
            assert!(s.add(c, false).unwrap_err().contains(said), "{text}");
            assert_eq!(s.to_json(), text);
        }
        for clean in [
            "",
            r#"{"connections": []}"#,
            r#"{"version": 1, "next": 2, "connections": [{"id":"c1","name":"A","kind":"wms","address":"https://a"}]}"#,
        ] {
            let s = Connections::from_json(clean);
            assert!(s.unread().is_empty(), "{clean}: {:?}", s.unread());
        }
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
