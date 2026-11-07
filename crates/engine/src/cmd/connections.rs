//! Saved connections and session-only logins. Listing handlers are called directly: these
//! commands never edit the project, create undo snapshots or note recently added sources.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::{b, ps};
use crate::connections::{Connection, DATABASE_KINDS, DESKTOP_ONLY, KINDS};
use crate::render::rustgis_tiles::{s3, stac};
use crate::s3_keys as keys;
use crate::service_logins as web;
use crate::{CmdResult, CommandSpec as C, Session};

#[path = "connection_browse.rs"]
mod browse;

#[path = "connection_logins.rs"]
mod logins;
#[cfg(not(target_arch = "wasm32"))]
use logins::request_password;
pub(crate) use logins::{clear_active, resolve_keys, resolve_project, restore_active};
#[path = "connection_use.rs"]
mod connection_use;

pub fn commands() -> Vec<C> {
    vec![
        C::new("connection.list", "List Connections", "Home › Add › Data Sources", list).roles(&[]).effects(crate::policy::Effects::NONE).view().params(
            r#"{"kind"?: "postgres"|"mysql"|"sqlserver"|"service"|"wms"|"wmts"|"wfs"|"wcs"|"ogcapi"|"arcgis"|"csw"|"stac"|"s3", "group"?: text}"#,
        ),
        C::new("connection.new", "New Connection", "Home › Add › Data Sources", new).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Reserved)).view().params(
            r#"{"kind": "postgres"|"mysql"|"sqlserver"|"service"|"wms"|"wmts"|"wfs"|"wcs"|"ogcapi"|"arcgis"|"csw"|"stac"|"s3", "address"?: "as the kind's add command takes it, without its password or token (postgres:host=… dbname=… user=…, mysql://user@host/db, mssql://user@host/db, a service's or STAC API's https:// address, an S3 endpoint: AWS when absent)", "name"?: "unique (default from the address)", "group"?: text, "options"?: {"region"?: text, "buckets"?: "a,b", "credentials"?: "keys"|"profile"|"environment"|"none", "profile"?: text} (S3 only), "auth"?: "none"|"basic"|"header": how a web-service connection logs in (basic authentication or an API key in a header; the password or key is given to connection.login, never kept here), "user"?: "the basic login's user name", "keyName"?: "the header the API key goes in, e.g. X-API-Key"}"#,
        ),
        C::new("connection.edit", "Edit Connection", "Home › Add › Data Sources", edit).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Reserved)).view().params(
            r#"{"id": "a connection's id or name", "name"?: text, "group"?: text, "address"?: text, "options"?: "as connection.new (replaces them)", "auth"?: "as connection.new", "user"?: text, "keyName"?: text, "kind"?: "its kind (unchanged)"} (another origin, login method, user or key name ends the web connection's session login)"#,
        ),
        C::new("connection.duplicate", "Duplicate Connection", "Home › Add › Data Sources", duplicate).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Reserved)).view().params(r#"{"id": "a connection's id or name", "name"?: text}"#),
        C::new("connection.remove", "Remove Connection", "Home › Add › Data Sources", remove).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Reserved)).view().params(r#"{"id": "a connection's id or name"} (deletes its saved database password or S3 keys; ends its web-service session login)"#),
        C::new("connection.login", "Log In to Connection", "Home › Add › Data Sources", login_command).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Reserved)).view().params(
            r#"{"id": "a connection's id or name; blank credentials explicitly use this ID's session credentials (or saved credentials for database/S3 connections)", "password"?: "a database password, or a web service's basic-authentication password", "apiKey"?: "a web service's API key (auth header)", "allowInsecure"?: "boolean, default false: confirms sending a web service's login to its plain http:// address, unencrypted", "accessKeyId"?: text, "secretAccessKey"?: text, "sessionToken"?: "temporary S3 credentials, never persisted", "remember"?: "boolean, default false: opt into this computer's credential store for a newly typed database password or complete S3 key pair (saved keys mode only). Web-service credentials are session-only and reject remember:true. No credentials are stored in connections or projects; a failed database/S3 save preserves the session login"} (a web service's login is sent only with this session's requests bound to that connection and only to its address's origin: the same scheme, host and port; logging in to one of several connections chooses it for unassigned requests to that address)"#,
        ),
        C::new("connection.logout", "Log Out of Connection", "Home › Add › Data Sources", logout).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Reserved)).view().params(
            r#"{"id": "a web-service connection's id or name"} (clears this session's login and its password or key, which is no longer sent)"#,
        ),
        C::new("connection.forgetSavedKeys", "Forget Saved Keys", "Home › Add › Data Sources", forget_saved_keys).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Reserved)).view().params(
            r#"{"id": "a saved S3 connection's id or name"} (deletes stored keys; a login in this session keeps working)"#,
        ),
        C::new("connection.resolveS3", "Choose S3 Connection", "Home › Add › Data Sources", resolve_s3).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Reserved).with(crate::policy::Effect::Files)).view().params(
            r#"{"source": "a pending project S3 source address", "id": "a candidate saved connection id"} → {id, reopened, failed, reading}: explicitly grants this bucket and opens only its waiting sources; reading counts the rasters read in place whose overview is still being read (the desktop doesn't wait for it: it lands as the map draws, and one that fails is said then)"#,
        ),
        C::new("connection.forgetPassword", "Forget Saved Password", "Home › Add › Data Sources", forget_password).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Reserved)).view().params(r#"{"id": "a database connection's id or name"}"#),
        C::new("connection.test", "Test Connection", "Home › Add › Data Sources", test).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Reserved)).view().params(
            r#"{"id"?: "a connection's id or name", "kind"?: "postgres"|"mysql"|"sqlserver"|"service"|"wms"|"wmts"|"wfs"|"wcs"|"ogcapi"|"arcgis"|"csw"|"stac"|"s3", "name"?: text, "group"?: text, "address"?: text, "options"?: "an unsaved connection, as connection.new (when no id)", "auth"?: text, "user"?: text, "keyName"?: "as connection.new (when no id)", "password"?: "for a named database, authentication refusal restores prior session credentials; a later listing failure keeps the new password; never persisted. A saved web service's: as connection.login (logs in first)", "apiKey"?: "a saved web service's, as connection.login (logs in first)", "allowInsecure"?: "as connection.login", "accessKeyId"?: text, "secretAccessKey"?: text, "sessionToken"?: "as connection.login (logs in first)", "probe"?: "true: only the address the test reads, {fetch} (null for databases, which their driver tests)", "answer"?: "non-database kinds only: what that address returned, when the app fetched it (no network request then); databases ignore answer and test through their driver"}"#,
        ),
        C::new("connection.browse", "Browse Connection", "Home › Add › Data Sources", browse::browse).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Reserved).with(crate::policy::Effect::Files)).view().params(
            r#"{"id": "a connection's id or name", "node"?: "a child's node, as a listing gives it (the connection's top level when absent)", "probe"?: "true: only the address this level reads, {fetch} (null: read without a fetch, or by the database driver); an S3 connection logs in first, as connection.test does", "answer"?: "what that address returned, when the app fetched it", "documents"?: [{"url": "a further document a listing asked for (next)", "text": "its answer"}]} → {connection, node, children: [{key, name, what, detail?, node? (browse it), children? (listed already), add? {command, params}: the command that adds it}], truncated?, matched?, notes?, service? {url, type}, next? (fetch it, then ask again with answer and documents)}. Database roots list schemas; open a schema node to list its tables."#,
        ),
    ]
}

fn shown(s: &Session, c: &Connection) -> Value {
    let database = DATABASE_KINDS.contains(&c.kind.as_str());
    let available = !database || !cfg!(target_arch = "wasm32");
    let logged_in = if c.kind == "s3" {
        s.has_s3_login(&c.id)
    } else if web::is_service(c) {
        web::logged_in(s, c)
    } else {
        database && s.database_logins.known_password(&c.id).is_some()
    };
    let add_with = "connection.browse {id}, then {id, node} for a child's node; run a dataset's add {command, params}";
    let mut out = serde_json::to_value(c).unwrap_or(Value::Null);
    out["available"] = json!(available);
    if !available {
        out["why"] = json!(DESKTOP_ONLY);
    }
    out["loggedIn"] = json!(logged_in);
    // Whether the keys were saved when last asked of the credential store (never a key; listing
    // doesn't ask the store).
    if c.kind == "s3" {
        out["savedKeys"] = json!(c.saved_keys);
    }
    if web::is_service(c) {
        if let Some(out) = out.as_object_mut() {
            out.remove("remembered");
        }
    } else {
        out["remembered"] = json!(c.remembered);
    }
    // A web login over plain http:// is sent only once confirmed (`allowInsecure`).
    if !c.auth.is_empty() && c.address.get(..7).is_some_and(|s| s.eq_ignore_ascii_case("http://")) {
        out["insecure"] = json!(true);
    }
    out["addWith"] = json!(add_with);
    out
}

fn found<'a>(s: &'a Session, p: &Value) -> Result<&'a Connection, String> {
    let id = ps(p, "id").unwrap_or("");
    s.connections.find(id).ok_or_else(|| format!("no connection '{id}'"))
}

fn options(p: &Value) -> Result<BTreeMap<String, String>, String> {
    let object = p.as_object().ok_or("options is an object of texts")?;
    object
        .iter()
        .map(|(k, v)| v.as_str().map(|t| (k.clone(), t.to_owned())).ok_or_else(|| "options is an object of texts".into()))
        .collect()
}

/// The login method's fields a command gives (`auth`, `user`, `keyName`), each text.
fn login_fields(p: &Value, c: &mut Connection) -> Result<(), String> {
    for (key, field) in [("auth", &mut c.auth), ("user", &mut c.user), ("keyName", &mut c.key_name)] {
        if let Some(value) = p.get(key) {
            *field = value.as_str().ok_or_else(|| format!("{key} is text"))?.into();
        }
    }
    Ok(())
}

/// Logs the project's layers read from these connections' origins in again (a connection was
/// added, changed or removed: what is clear for each origin may have changed).
fn resolve_origins(s: &mut Session, cs: &[&Connection]) {
    let origins: std::collections::BTreeSet<String> = cs.iter().filter_map(|c| crate::connections::login_origin(c)).collect();
    if !origins.is_empty() {
        web::resolve(s, Some(&origins));
    }
}

/// Saved database connections, in list order, with how many project layers wait for their login.
pub(crate) fn waiting(s: &Session) -> Vec<(Connection, usize)> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut counts = BTreeMap::<String, usize>::new();
        for layer in s.project.maps.iter().flat_map(|m| &m.layers) {
            let Some(src) = layer.source.as_ref().filter(|src| src.login) else { continue };
            let Some((kind, database)) = crate::connections::layer_database(src) else { continue };
            if let [c] = s.connections.matching_database(kind, database).as_slice() {
                *counts.entry(c.id.clone()).or_default() += 1;
            }
        }
        s.connections.connections.iter().filter_map(|c| counts.get(&c.id).map(|&n| (c.clone(), n))).collect()
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = s;
        Vec::new()
    }
}

/// Says, once per connection, which layers wait for its login (a project just opened).
pub(crate) fn note_waiting(s: &mut Session) {
    for (c, n) in waiting(s) {
        s.notify(format!("{n} layer(s) read the database of the connection '{}' and need its password: right-click the connection in Data Sources › Connections, or one of the layers, and choose Log In…", c.name), false);
    }
    let mut ambiguous = BTreeMap::<Vec<String>, usize>::new();
    for l in s.project.maps.iter().flat_map(|m| &m.layers) {
        let Some(src) = l.source.as_ref().filter(|src| src.login) else { continue };
        let Some((kind, database)) = crate::connections::layer_database(src) else { continue };
        let candidates = s.connections.matching_database(kind, database);
        if candidates.len() > 1 {
            let labels = candidates.iter().map(|c| format!("{} ({})", c.name, c.id)).collect();
            *ambiguous.entry(labels).or_default() += 1;
        }
    }
    for (candidates, count) in ambiguous {
        s.notify(
            format!("{count} layer(s) need an explicit connection choice: {}. Use connection.login {{id}}.", candidates.join(", ")),
            false,
        );
    }
}

fn list(s: &mut Session, p: &Value) -> CmdResult {
    let kind = ps(p, "kind");
    if let Some(k) = kind
        && !KINDS.contains(&k)
    {
        return Err(format!("unknown kind '{k}' ({})", KINDS.join(", ")));
    }
    let group = ps(p, "group");
    let waiting: BTreeMap<_, _> = waiting(s).into_iter().map(|(c, n)| (c.id, n)).collect();
    Ok(json!({"connections": s.connections.connections.iter()
        .filter(|c| kind.is_none_or(|k| c.kind == k) && group.is_none_or(|g| c.group.chars().flat_map(char::to_lowercase).eq(g.chars().flat_map(char::to_lowercase))))
        .map(|c| {
            let mut out = shown(s, c);
            if let Some(&n) = waiting.get(&c.id) {
                out["waiting"] = json!(n);
            }
            out
        }).collect::<Vec<_>>(),
        "loginRequests": s.database_logins.requests(s.opened).map(|r| {
            let mut out = json!({"reason":r.reason.name(), "connections":r.connections, "layers":r.layers.len() + r.tiles});
            if r.kind == crate::connections::WEB {
                out["origin"] = json!(r.key);
            } else {
                out["database"] = json!(crate::connections::database_label(r.kind, &r.key));
            }
            out
        }).collect::<Vec<_>>() }))
}

fn new(s: &mut Session, p: &Value) -> CmdResult {
    let mut c = Connection {
        kind: ps(p, "kind").ok_or_else(|| format!("kind is required ({})", KINDS.join(", ")))?.trim().into(),
        address: ps(p, "address").unwrap_or("").into(),
        name: ps(p, "name").unwrap_or("").into(),
        group: ps(p, "group").unwrap_or("").into(),
        options: p.get("options").map(options).transpose()?.unwrap_or_default(),
        ..Connection::default()
    };
    login_fields(p, &mut c)?;
    let explicit = !c.name.trim().is_empty();
    let (c, stripped) = s.connections.add(c, explicit)?;
    resolve_keys(s, &address_keys(&c.address));
    resolve_origins(s, &[&c]);
    Ok(shown_stripped(s, &c, stripped))
}

fn shown_stripped(s: &Session, c: &Connection, stripped: bool) -> Value {
    let mut out = shown(s, c);
    if stripped {
        out["stripped"] = json!(true);
    }
    out
}

/// Whether an edit changes the endpoint a connection's keys are for: another canonical endpoint
/// (a different spelling of the same one isn't a change).
fn endpoint_changed(old: &Connection, new: &Connection) -> bool {
    match (s3::canonical(&old.address), s3::canonical(&new.address)) {
        (Ok(a), Ok(b)) => a != b,
        _ => old.address.trim() != new.address.trim(),
    }
}

fn edit(s: &mut Session, p: &Value) -> CmdResult {
    let old = found(s, p)?.clone();
    let mut c = old.clone();
    if let Some(kind) = ps(p, "kind")
        && kind.trim() != c.kind
    {
        return Err("a connection's kind doesn't change: make a new one".into());
    }
    for (key, field) in [("name", &mut c.name), ("group", &mut c.group), ("address", &mut c.address)] {
        if let Some(value) = p.get(key) {
            *field = value.as_str().ok_or_else(|| format!("{key} is text"))?.into();
        }
    }
    if let Some(v) = p.get("options") {
        c.options = options(v)?;
    }
    login_fields(p, &mut c)?;
    // Validate everything, then locate the commit target before touching the store.
    let (mut prepared, stripped) = s.connections.prepare_update(&old.id, c)?;
    let database_changed = DATABASE_KINDS.contains(&old.kind.as_str()) && old.address != prepared.address;
    // A web login is for one origin, method, user and key name.
    let login_changed = web::is_service(&old) && !crate::connections::same_login(&old, &prepared);
    let credentials_changed = database_changed
        || (old.kind == "s3"
            && (endpoint_changed(&old, &prepared)
                || (old.options.get("credentials").is_some_and(|v| v == "keys")
                    && prepared.options.get("credentials").is_none_or(|v| v != "keys"))));
    let slot = s.connections.connections.iter_mut().find(|c| c.id == old.id).ok_or("connection no longer exists")?;
    if credentials_changed {
        delete_account(&mut s.database_logins, &old)?;
        prepared.saved_keys = false;
        prepared.remembered = false;
    }
    let c = crate::connections::Connections::commit_update(slot, &mut s.connections.generation, prepared);
    if database_changed {
        clear_identity(s, &old.id);
    }
    if login_changed {
        web::forget(s, &old);
    }
    if web::is_service(&c) && (login_changed || old.address != c.address) {
        resolve_origins(s, &[&old, &c]);
    }
    if old.address != c.address {
        // Only the two databases this definition names can change: another database's layers
        // waiting for a login keep their request, and an unrelated down server is not retried.
        let mut keys = address_keys(&old.address);
        keys.extend(address_keys(&c.address));
        resolve_keys(s, &keys);
    }
    if c.kind == "s3" && (old.name != c.name || old.address != c.address || old.options != c.options) {
        s3::disconnect(&old.name);
        if old.name != c.name {
            s3::disconnect(&c.name);
        }
    }
    Ok(shown_stripped(s, &c, stripped))
}

fn duplicate(s: &mut Session, p: &Value) -> CmdResult {
    let mut c = found(s, p)?.clone();
    c.id.clear();
    let explicit = ps(p, "name").is_some_and(|n| !n.trim().is_empty());
    if let Some(name) = ps(p, "name") {
        c.name = name.into();
    }
    // A copy has no saved keys: `add` starts it without any.
    let (c, _) = s.connections.add(c, explicit)?;
    resolve_keys(s, &address_keys(&c.address));
    resolve_origins(s, &[&c]);
    Ok(shown(s, &c))
}

fn remove(s: &mut Session, p: &Value) -> CmdResult {
    let c = found(s, p)?.clone();
    if DATABASE_KINDS.contains(&c.kind.as_str()) || c.kind == "s3" {
        delete_account(&mut s.database_logins, &c)?;
    }
    if web::is_service(&c) {
        web::forget(s, &c);
    }
    if DATABASE_KINDS.contains(&c.kind.as_str()) {
        clear_identity(s, &c.id);
    }
    s.connections.remove(&c.id);
    // The removed definition named one database: only its layers can change. The requests naming
    // it went with `clear_identity`.
    resolve_keys(s, &address_keys(&c.address));
    resolve_origins(s, &[&c]);
    if c.kind == "s3" {
        s3::disconnect(&c.name);
        s.s3_bindings.remove(&c.id);
    }
    Ok(json!({"removed": c.id}))
}

/// Deletes the keys saved for an S3 connection from the credential store. The login of this
/// session keeps working.
fn forget_saved_keys(s: &mut Session, p: &Value) -> CmdResult {
    let c = found(s, p)?.clone();
    if c.kind != "s3" {
        return Err(format!("{} connections have no saved keys", c.kind));
    }
    delete_account(&mut s.database_logins, &c)?;
    s.connections.set_saved_keys(&c.id, false);
    let c = s.connections.find(&c.id).cloned().unwrap_or(c);
    Ok(shown(s, &c))
}

/// Ends this session's login of a web-service connection and clears its transient secret.
fn logout(s: &mut Session, p: &Value) -> CmdResult {
    let c = found(s, p)?.clone();
    if !web::is_service(&c) {
        return Err(format!("{} connections don't log out: only web-service connections do", c.kind));
    }
    web::logout(s, &c);
    Ok(shown(s, &c))
}

fn delete_account(logins: &mut crate::connections::DatabaseLogins, c: &Connection) -> Result<(), String> {
    let (marked, error) = if c.kind == "s3" {
        (c.saved_keys, keys::NOT_FORGOTTEN)
    } else {
        (c.remembered, "The saved password couldn't be deleted. The connection was left unchanged.")
    };
    if logins.has_store() {
        logins.delete(&c.id).map_err(|_| error.to_string())
    } else if marked {
        Err(error.into())
    } else {
        Ok(())
    }
}

fn clear_identity(s: &mut Session, id: &str) {
    s.database_logins.forget_connection(id);
    #[cfg(not(target_arch = "wasm32"))]
    rustgis_data::source::duckdb::forget_connection_password(id);
}

/// The databases an address names (empty for kinds that read no database).
fn address_keys(address: &str) -> Vec<(&'static str, String)> {
    crate::connections::database_key(address).into_iter().collect()
}

/// The database a layer reads, as its driver names it (the same key `resolve_project` groups by).
pub(crate) fn layer_keys(src: &rustgis_map::source::SourceLayer) -> Vec<(&'static str, String)> {
    let Some((_, database)) = crate::connections::layer_database(src) else { return Vec::new() };
    crate::connections::database_key(&crate::sources::redact(database)).into_iter().collect()
}

fn forget_password(s: &mut Session, p: &Value) -> CmdResult {
    let c = found(s, p)?.clone();
    if !DATABASE_KINDS.contains(&c.kind.as_str()) {
        return Err("only database connections have saved passwords".into());
    }
    delete_account(&mut s.database_logins, &c)?;
    s.connections.set_remembered(&c.id, false);
    s.database_logins.forget_saved(&c.id);
    #[cfg(not(target_arch = "wasm32"))]
    {
        use rustgis_data::source::duckdb;
        let key = crate::connections::database_key(&c.address).map(|(_, key)| key);
        let was_active = key.as_ref().is_some_and(|key| duckdb::has_connection_password(key, &c.id));
        duckdb::forget_connection_password(&c.id);
        if was_active
            && let Some(key) = key
            && !duckdb::has_explicit_password(&key)
            && let Some(pw) = s.database_logins.session_password(&c.id)
        {
            // Restore this ID's explicit session value only if it owned the active slot.
            // An inactive ID must not replace another ID or a typed address password.
            duckdb::set_connection_password(&key, &c.id, Some(pw.expose()));
            s.database_logins.note_activation(&key, &c.id);
        }
    }
    let mut c = c;
    c.remembered = false;
    Ok(shown(s, &c))
}

#[derive(Default)]
struct Reopened {
    opened: usize,
    failed: usize,
    /// A login that worked but couldn't do all it was asked (the keys weren't kept).
    warning: Option<&'static str>,
    /// The server refused this attempt's password: desktop database reopen only.
    #[cfg(not(target_arch = "wasm32"))]
    refused: bool,
    password_saved: Option<bool>,
    /// A store save was asked for and failed: the fixed warning belongs to this case only.
    save_failed: bool,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Copy)]
enum ReopenMode {
    Automatic,
    Login { all_unopened: bool },
}

/// Reopens this connection's database sources and aggregates refusals. Automatic resolution
/// preserves peer reconnect barriers; only an explicit matching login releases them.
#[cfg(not(target_arch = "wasm32"))]
fn reopen(s: &mut Session, c: &Connection, saved: bool, session: bool, mode: ReopenMode) -> Reopened {
    let Some(key) = crate::connections::database_key(&c.address) else { return Reopened::default() };
    let mut result = Reopened::default();
    let mut first_error = None;
    for layer in s.project.maps.iter_mut().flat_map(|m| m.layers.iter_mut()) {
        let Some(src) = layer.source.as_mut().filter(|src| match mode {
            ReopenMode::Automatic => !src.reconnect_required() && !src.is_open(),
            ReopenMode::Login { all_unopened } => src.login || src.reconnect_required() || all_unopened && !src.is_open(),
        }) else {
            continue;
        };
        let Some((kind, database)) = crate::connections::layer_database(src) else { continue };
        if kind != key.0 || crate::connections::database_key(&crate::sources::redact(database)).as_ref() != Some(&key) {
            continue;
        }
        if result.refused {
            // The server has already refused this attempt's password: the group's other layers
            // must not send it again (one attempt per database, typed or saved).
            src.login = true;
            src.error = first_error.clone();
            result.failed += 1;
            continue;
        }
        if matches!(mode, ReopenMode::Login { .. }) {
            src.allow_reconnect();
        }
        src.error = None;
        match src.open() {
            Ok(()) => result.opened += 1,
            Err(e) => {
                result.failed += 1;
                result.refused |= src.login;
                first_error.get_or_insert(e);
            }
        }
    }
    if result.refused {
        use crate::connections::{LoginReason, LoginRequest};
        if saved {
            s.database_logins.reject(&c.id);
        } else if session {
            s.database_logins.reject_session_password(c);
        }
        if saved || session {
            rustgis_data::source::duckdb::forget_connection_password(&c.id);
            s.database_logins.drop_activation(&key.1);
        }
        let layers = s
            .project
            .maps
            .iter()
            .flat_map(|m| &m.layers)
            .filter_map(|l| {
                let src = l.source.as_ref().filter(|src| src.login)?;
                let (_, db) = crate::connections::layer_database(src)?;
                (crate::connections::database_key(&crate::sources::redact(db)).as_ref() == Some(&key)).then_some(l.id)
            })
            .collect();
        s.database_logins.request(LoginRequest {
            reason: LoginReason::Password,
            kind: key.0,
            key: key.1.clone(),
            connections: vec![c.id.clone()],
            layers,
            tiles: 0,
            opened: s.opened,
            prompted: false,
        });
    } else {
        s.database_logins.clear_requests_for(key.0, &key.1);
    }
    if result.opened > 0 {
        s.notify(format!("{} layer(s) from the connection '{}' opened again", result.opened, c.name), false);
    }
    if result.failed > 0 {
        s.notify(
            format!(
                "{} layer(s) from the connection '{}' still can't be read: {}",
                result.failed,
                c.name,
                first_error.as_deref().unwrap_or("")
            ),
            true,
        );
    }
    result
}

/// `remember` of a login: only `true` or `false`, judged before anything happens.
fn remember_flag(p: &Value) -> Result<bool, String> {
    match p.get("remember") {
        None => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err("remember is true or false".into()),
    }
}

/// The access key pair typed in a login: both or neither (never half a pair).
fn typed_keys(p: &Value) -> Result<Option<(String, String)>, String> {
    let text = |k: &str| ps(p, k).map(str::trim).filter(|v| !v.is_empty()).map(str::to_string);
    match (text("accessKeyId"), text("secretAccessKey")) {
        (Some(k), Some(s)) => Ok(Some((k, s))),
        (None, None) => Ok(None),
        _ => Err("give both accessKeyId and secretAccessKey".into()),
    }
}

/// Logs in to a saved S3 connection that has no login yet, with what needs no typing: its
/// profile or the environment, anonymously, or the keys saved on this computer (a user action
/// that may ask the system to unlock its store). An unsaved connection with typed keys has none
/// to read: the caller says to give them.
pub(super) fn ensure_login(s: &mut Session, c: &Connection) -> Result<(), String> {
    if s.has_s3_login(&c.id) {
        return Ok(());
    }
    let keys_mode = c.options.get("credentials").is_some_and(|v| v == "keys");
    if keys_mode && c.id.is_empty() {
        return Ok(());
    }
    login(s, c, &Value::Null).map(|_| ())
}

/// The token of this session's login of the saved connection `c`, when the live S3 connection
/// `conn` is that login (an ordinary `s3.connect` of the same name isn't).
pub(super) fn saved_binding(s: &Session, c: &Connection, conn: &s3::Connection) -> Option<String> {
    let mine = s.s3_binding(&c.id)?;
    (s3::binding_of(&conn.name).as_deref() == Some(mine)).then(|| mine.to_string())
}

/// The address a fetch of `url` through the live S3 connection `conn` of the saved connection `c`
/// uses: one that names this session's login of `c`, so only that login signs it. (A live
/// connection of that name that isn't the saved login — an ordinary `s3.connect` — keeps its
/// address.)
pub(super) fn through(s: &Session, c: &Connection, conn: &s3::Connection, url: String) -> Result<String, String> {
    match saved_binding(s, c, conn) {
        Some(binding) => s3::bound_url(&binding, &url),
        None => Ok(url),
    }
}

/// Lets the live connection `conn` of the saved connection `c` serve `bucket`: through this
/// session's login of `c` when that is what it is, as a connection always did otherwise.
pub(super) fn grant(s: &Session, c: &Connection, conn: &s3::Connection, bucket: &str) {
    match saved_binding(s, c, conn) {
        Some(binding) => {
            let _ = s3::grant_saved(&binding, bucket);
        }
        None => s3::grant(&conn.name, bucket),
    }
}

fn login(s: &mut Session, c: &Connection, p: &Value) -> Result<Reopened, String> {
    let remember = remember_flag(p)?;
    if DATABASE_KINDS.contains(&c.kind.as_str()) {
        #[cfg(target_arch = "wasm32")]
        {
            return Err(DESKTOP_ONLY.into());
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let key = crate::connections::database_key(&c.address).ok_or("not a database connection")?;
            let typed = ps(p, "password").filter(|t| !t.is_empty());
            if typed.is_none() {
                s.database_logins.retry_saved(&c.id);
            }
            // A password typed for this address wins over any saved one (explicit input first).
            let address_typed = typed.is_none() && rustgis_data::source::duckdb::has_explicit_password(&key.1);
            let (password, saved) = match typed {
                Some(_) => (None, false),
                None if address_typed => (None, false),
                None => {
                    // Resolved before this connection takes the address over: a login that turns
                    // out to have no password leaves another connection's activation alone.
                    let (pw, saved) = credential(s, c)?.ok_or("password is required")?;
                    (Some(pw), saved)
                }
            };
            let all_unopened = s.database_logins.requests(s.opened).any(|r| r.kind == key.0 && r.key == key.1);
            let mut result = if let Some(pw) = typed {
                // Keep selection and activation unchanged until this attempt is accepted.
                rustgis_data::source::duckdb::try_connection_password(
                    &key.1,
                    &c.id,
                    pw,
                    || reopen(s, c, false, false, ReopenMode::Login { all_unopened }),
                    |result| result.refused,
                )
            } else {
                // Another saved ID's activation is not authority to read this connection;
                // a password typed for the address itself stays.
                rustgis_data::source::duckdb::set_connection_password(&key.1, &c.id, None);
                s.database_logins.drop_activation(&key.1);
                s.database_logins.select(key.0, &key.1, &c.id);
                if let Some(password) = &password {
                    activate(s, c, password.expose());
                }
                let session = password.is_some() && !saved;
                reopen(s, c, saved, session, ReopenMode::Login { all_unopened })
            };
            if let Some(pw) = &typed
                && !result.refused
            {
                // A refused password is not this session's credential: it was sent, and the user
                // is asked for another one.
                s.database_logins.set_session_password(&c.id, pw);
                s.database_logins.select(key.0, &key.1, &c.id);
                s.database_logins.note_activation(&key.1, &c.id);
            }
            if remember {
                let stored = !result.refused && typed.is_some_and(|pw| s.database_logins.save(c, pw));
                if stored {
                    s.connections.set_remembered(&c.id, true);
                }
                result.save_failed = !stored && typed.is_some() && !result.refused;
                result.password_saved = Some(stored);
            }
            return Ok(result);
        }
    }
    if web::is_service(c) {
        return service_login(s, c, p, remember);
    }
    if c.kind != "s3" {
        return Err(format!("{} connections don't log in yet", c.kind));
    }
    let mode = c.options.get("credentials").map(String::as_str).unwrap_or("none");
    if remember && (mode != "keys" || c.id.is_empty()) {
        return Err("remember keeps the typed keys of a saved S3 connection that uses keys (credentials: keys)".into());
    }
    let mut q = json!({"name": c.name, "endpoint": c.address});
    for key in ["region", "buckets"] {
        if let Some(v) = c.options.get(key).filter(|v| !v.is_empty()) {
            q[key] = json!(v);
        }
    }
    // The typed pair, when it is to be kept after the login (a login with a session token isn't).
    let mut to_keep = None;
    let mut temporary = false;
    match mode {
        "keys" => {
            let typed = typed_keys(p)?;
            let token = ps(p, "sessionToken").map(str::trim).filter(|t| !t.is_empty());
            match typed {
                Some((key, secret)) => {
                    if let Some(t) = token {
                        q["sessionToken"] = json!(t);
                        temporary = true;
                    }
                    q["accessKeyId"] = json!(key);
                    q["secretAccessKey"] = json!(secret);
                    if remember {
                        to_keep = Some((key, secret));
                    }
                }
                None => {
                    if token.is_some() {
                        return Err("a session token needs accessKeyId and secretAccessKey".into());
                    }
                    if remember {
                        return Err("remember needs the keys typed in this login: give accessKeyId and secretAccessKey".into());
                    }
                    if c.id.is_empty() {
                        return Err("accessKeyId and secretAccessKey are required".into());
                    }
                    // The saved pair, if there is one. Another connection's keys, a profile and the
                    // environment are never tried instead.
                    match keys::load(s.database_logins.secret_store(), c) {
                        keys::Loading::Keys(key, secret) => {
                            s.connections.set_saved_keys(&c.id, true);
                            q["accessKeyId"] = json!(key);
                            q["secretAccessKey"] = json!(secret);
                        }
                        keys::Loading::Missing => {
                            s.connections.set_saved_keys(&c.id, false);
                            return Err(keys::NONE_SAVED.into());
                        }
                        keys::Loading::Unavailable => return Err(keys::UNAVAILABLE.into()),
                        keys::Loading::Unusable => return Err(keys::UNUSABLE.into()),
                    }
                }
            }
        }
        "profile" => {
            let profile = c.options.get("profile").filter(|v| !v.trim().is_empty()).ok_or("the connection names no AWS profile")?;
            q["profile"] = json!(profile);
        }
        "environment" => q["environment"] = json!(true),
        _ => q["anonymous"] = json!(true),
    }
    if c.id.is_empty() {
        // A definition not saved (Test): an ordinary session connection.
        super::s3::connect(s, &q)?;
        return Ok(Reopened::default());
    }
    // A saved connection's login is live under this session's token for it, and signs only the
    // addresses that name it.
    let binding = match s.s3_bindings.get(&c.id) {
        Some(b) => b.clone(),
        None => s3::new_binding()?,
    };
    super::s3::connect_saved(s, &q, &binding, &c.id)?;
    s.s3_bindings.insert(c.id.clone(), binding);
    let mut done = Reopened::default();
    // Kept only once the login has worked, so what is stored is a pair that signed in; a failed
    // save leaves the login as it is (and an earlier saved pair as it was).
    if let Some((key, secret)) = to_keep {
        if temporary {
            done.warning = Some(keys::TEMPORARY);
        } else if keys::save(s.database_logins.secret_store(), c, &key, &secret) {
            s.connections.set_saved_keys(&c.id, true);
        } else {
            done.warning = Some(keys::NOT_REMEMBERED);
        }
    }
    Ok(done)
}

fn login_command(s: &mut Session, p: &Value) -> CmdResult {
    let c = found(s, p)?.clone();
    // Logging in is the user's answer to a request waiting at this origin (the login ends it).
    let answers = web::is_service(&c) && web::answers(s, &c);
    let result = login(s, &c, p)?;
    if web::is_service(&c) {
        if answers {
            web::adopt(s, &c);
        }
        resolve_origins(s, &[&c]);
    }
    let c = s.connections.find(&c.id).cloned().unwrap_or(c);
    let mut out = shown(s, &c);
    if let Some(saved) = result.password_saved {
        out["passwordSaved"] = json!(saved);
        // A refused password, or one nothing was typed for, isn't "available for this session":
        // the fixed warning belongs to a store that couldn't keep a usable password.
        if result.save_failed {
            out["warning"] = json!("The password is available for this session, but wasn't remembered on this computer.");
        }
    }
    if result.opened + result.failed > 0 {
        out["reopened"] = json!(result.opened);
        out["failed"] = json!(result.failed);
    }
    if let Some(warning) = result.warning {
        out["warning"] = json!(warning);
    }
    Ok(out)
}

/// This session's login of the saved S3 connection `c`, made if it has none (with what needs no
/// typing: its profile, the environment, anonymously, or the keys saved on this computer), and
/// its token: what an address names to be read through it. One that fails says why, in words
/// fit for a notice.
pub(crate) fn login_saved(s: &mut Session, c: &Connection) -> Result<String, String> {
    ensure_login(s, c)?;
    s.s3_binding(&c.id).map(str::to_string).ok_or_else(|| "the connection didn't log in".to_string())
}

/// Binds a project's S3 sources that wait for a choice to the saved connection chosen.
fn resolve_s3(s: &mut Session, p: &Value) -> CmdResult {
    let source = ps(p, "source").unwrap_or("");
    let id = ps(p, "id").unwrap_or("");
    crate::s3_connections::choose(s, source, id)
}
#[cfg(not(target_arch = "wasm32"))]
fn activate(s: &mut Session, c: &Connection, password: &str) {
    if let Some((_, key)) = crate::connections::database_key(&c.address) {
        rustgis_data::source::duckdb::set_connection_password(&key, &c.id, Some(password));
        s.database_logins.note_activation(&key, &c.id);
    }
}

/// The password a saved database connection logs in with, best first: one given this session
/// (typed for it); then, for a connection that keeps a saved password, the environment's for its
/// kind (`PGPASSWORD`, `MYSQL_PWD`, `SQLCMDPASSWORD`), which never asks the store; then the saved
/// one. A password typed for the address itself and one passed to a command are ranked above all
/// of these by the callers. The environment never reaches a connection that isn't saved with a
/// password, so a project can't name a new address to receive it. The flag is `true` for a saved
/// password (the server's refusal rejects it), `false` for the others.
#[cfg(not(target_arch = "wasm32"))]
fn credential(s: &mut Session, c: &Connection) -> Result<Option<(rustgis_data::secrets::Secret, bool)>, String> {
    if let Some(pw) = s.database_logins.session_password(&c.id) {
        return Ok(Some((pw.clone(), false)));
    }
    if !c.remembered || s.database_logins.is_rejected(&c.id) {
        return Ok(None);
    }
    if let Some(pw) = s.database_logins.ambient(c) {
        return Ok(Some((pw, false)));
    }
    match s.database_logins.load(c) {
        crate::connections::SavedPassword::Found(pw) => Ok(Some((pw, true))),
        crate::connections::SavedPassword::Absent => {
            s.connections.set_remembered(&c.id, false);
            s.notify("The saved password is missing or doesn't match this connection.", true);
            Ok(None)
        }
        crate::connections::SavedPassword::Unusable => Err("The saved password couldn't be used on this computer.".into()),
    }
}

/// `allowInsecure` of a login: only `true` or `false`.
fn insecure_flag(p: &Value) -> Result<bool, String> {
    match p.get("allowInsecure") {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err("allowInsecure is true or false".into()),
    }
}

/// Logs in to a saved web-service connection with the password (basic) or key (header)
/// typed, or else the one given this session. The login is sent to the connection's origin,
/// and this connection is chosen for it. A connection without a login method is chosen for
/// its origin, which then gets no login.
fn service_login(s: &mut Session, c: &Connection, p: &Value, remember: bool) -> Result<Reopened, String> {
    if remember {
        return Err(web::SESSION_ONLY.into());
    }
    let insecure = insecure_flag(p)?;
    if c.id.is_empty() {
        return Err(web::UNSAVED.into());
    }
    let password = ps(p, "password").filter(|t| !t.is_empty());
    let key = ps(p, "apiKey").filter(|t| !t.is_empty());
    let typed = match (c.auth.as_str(), password, key) {
        ("", None, None) => {
            web::activate(s, c, None, false)?;
            return Ok(Reopened::default());
        }
        ("", ..) => return Err("this connection has no login method: set auth (basic or header) with connection.edit first".into()),
        ("basic", typed, None) => typed,
        ("basic", _, Some(_)) => return Err("a basic login takes a password, not an apiKey".into()),
        (_, None, typed) => typed,
        (_, Some(_), _) => return Err("an API-key login takes an apiKey, not a password".into()),
    };
    let origin = crate::connections::login_origin(c).ok_or(web::NO_ORIGIN)?;
    let Some(typed) = typed else {
        // Already logged in: choose it again, keeping its binding and its cached tiles.
        if web::logged_in(s, c) {
            web::reselect(s, c, &origin);
            return Ok(Reopened::default());
        }
        // Only the active login given this session can be reused.
        let secret = web::known_secret(s, c)?;
        web::activate(s, c, Some(&secret), true)?;
        return Ok(Reopened::default());
    };
    web::check_login_secret(c, typed)?;
    if origin.starts_with("http://") && !insecure {
        return Err(web::INSECURE.into());
    }
    let secret = rustgis_data::secrets::Secret::new(typed);
    web::activate(s, c, Some(&secret), insecure)?;
    s.database_logins.set_session_password(&c.id, typed);
    Ok(Reopened::default())
}

fn tested(s: &Session, p: &Value) -> Result<Connection, String> {
    if ps(p, "id").is_some() {
        let c = found(s, p)?;
        // Only a web-service login is tied to its saved definition; other kinds test as saved.
        if web::is_service(c) {
            for (field, saved) in
                [("kind", c.kind.as_str()), ("address", &c.address), ("auth", &c.auth), ("user", &c.user), ("keyName", &c.key_name)]
            {
                if let Some(value) = p.get(field) {
                    let value = value.as_str().ok_or(web::SAVE_BEFORE_TEST)?.trim();
                    let value = if field == "auth" && value == "none" { "" } else { value };
                    if value != saved {
                        return Err(web::SAVE_BEFORE_TEST.into());
                    }
                }
            }
        }
        return Ok(c.clone());
    }
    let mut c = Connection {
        kind: ps(p, "kind").ok_or_else(|| format!("id, or kind and address, is required ({})", KINDS.join(", ")))?.trim().into(),
        address: ps(p, "address").unwrap_or("").into(),
        name: ps(p, "name").unwrap_or("").into(),
        group: ps(p, "group").unwrap_or("").into(),
        options: p.get("options").map(options).transpose()?.unwrap_or_default(),
        ..Connection::default()
    };
    login_fields(p, &mut c)?;
    Ok(crate::connections::clean(c)?.0)
}

fn first_bucket(c: &Connection) -> Option<String> {
    c.options.get("buckets")?.split(',').map(str::trim).find(|b| !b.is_empty()).map(str::to_owned)
}

fn probe_url(s: &mut Session, c: &Connection) -> Result<Option<String>, String> {
    let url = match c.kind.as_str() {
        "postgres" | "mysql" | "sqlserver" => return Ok(None),
        "service" => rustgis_data::ogc::capabilities_url(&c.address, None),
        "wms" | "wmts" | "wfs" | "wcs" | "ogcapi" | "arcgis" => rustgis_data::ogc::capabilities_url(&c.address, Some(&c.kind)),
        "csw" => super::service::records_request(s, &json!({"url": c.address}))?,
        "stac" => {
            let api = stac::api(Some(&c.address))?;
            if stac::catalog::is_static(&api) { api } else { stac::collections_url(&api) }
        }
        "s3" => {
            let conn = s3::connection(&c.name).ok_or("log in first: connection.login {id, accessKeyId, secretAccessKey}")?;
            if let Some(bucket) = first_bucket(c) {
                grant(s, c, &conn, &bucket);
                through(s, c, &conn, s3::list_url(&bucket, "", None, Some(&conn)))?
            } else if conn.credentials.is_some() {
                through(s, c, &conn, s3::buckets_url(&conn))?
            } else {
                return Err("name a bucket under Buckets to test an anonymous S3 connection".into());
            }
        }
        _ => return Err(format!("unknown kind '{}'", c.kind)),
    };
    let url = if web::is_service(c) { web::request(s, (!c.id.is_empty()).then_some(c.id.as_str()), &url)? } else { url };
    Ok(Some(url))
}

fn answered(s: &mut Session, c: &Connection, text: &str) -> CmdResult {
    match c.kind.as_str() {
        "service" => super::service::layers(s, &json!({"url": c.address, "connection":c.id, "text": text})),
        "wms" | "wmts" | "wfs" | "wcs" | "ogcapi" | "arcgis" => {
            super::service::layers(s, &json!({"url": c.address, "connection":c.id, "type": c.kind, "text": text}))
        }
        "csw" => super::service::catalog_search(s, &json!({"url": c.address, "connection":c.id, "text": text})),
        "stac" => super::stac::collections_json(&stac::api(Some(&c.address))?, text.as_bytes()),
        "s3" => {
            if let Some(bucket) = first_bucket(c) {
                let l = s3::parse_list(text.as_bytes())?;
                Ok(json!({"bucket": bucket, "count": l.prefixes.len() + l.objects.len(), "truncated": l.next.is_some()}))
            } else {
                Ok(json!({"buckets": s3::parse_buckets(text.as_bytes())?}))
            }
        }
        _ => Err(format!("unknown kind '{}'", c.kind)),
    }
}

fn test(s: &mut Session, p: &Value) -> CmdResult {
    let c = tested(s, p)?;
    // An unsaved web service reads anonymously, whatever saved connection shares its origin.
    let anonymous = web::is_service(&c) && c.id.is_empty();
    web::anonymously(s, anonymous, |s| test_connection(s, p, c)).unwrap_or_else(|_| Err("connection.test failed unexpectedly".into()))
}

fn test_connection(s: &mut Session, p: &Value, c: Connection) -> CmdResult {
    if web::is_service(&c) && c.id.is_empty() && !c.auth.is_empty() {
        return Err(web::UNSAVED.into());
    }
    if DATABASE_KINDS.contains(&c.kind.as_str()) && b(p, "probe").unwrap_or(false) {
        return Ok(json!({"connection":c.id, "fetch":null}));
    }
    let secrets =
        ["password", "apiKey", "accessKeyId", "secretAccessKey", "sessionToken"].iter().any(|k| ps(p, k).is_some_and(|v| !v.is_empty()));
    if !DATABASE_KINDS.contains(&c.kind.as_str()) && secrets {
        let mut input = p.clone();
        if let Some(o) = input.as_object_mut() {
            o.remove("remember");
        }
        login(s, &c, &input)?;
    }
    if c.kind == "s3" && !secrets {
        let keys_mode = c.options.get("credentials").is_some_and(|v| v == "keys");
        // Other sources are logged in again (a profile may have changed); saved keys are read
        // when there is no login yet.
        if !keys_mode || (!c.id.is_empty() && !s.has_s3_login(&c.id)) {
            login(s, &c, &Value::Null)?;
        }
    }
    // Missing or unusable session logins must not look like a successful anonymous test.
    if web::is_service(&c) && !c.auth.is_empty() && !c.id.is_empty() && !secrets && !web::logged_in(s, &c) {
        login(s, &c, &Value::Null)?;
    }
    let mut out = json!({});
    if !c.id.is_empty() {
        out["connection"] = json!(c.id);
    }
    if b(p, "probe").unwrap_or(false) {
        out["fetch"] = json!(probe_url(s, &c)?);
        return Ok(out);
    }
    if let Some(text) = ps(p, "answer")
        && !DATABASE_KINDS.contains(&c.kind.as_str())
    {
        out["result"] = answered(s, &c, text)?;
        return Ok(out);
    }
    let result = match c.kind.as_str() {
        "postgres" | "mysql" | "sqlserver" => {
            #[cfg(target_arch = "wasm32")]
            return Err(DESKTOP_ONLY.into());
            #[cfg(not(target_arch = "wasm32"))]
            {
                if c.id.is_empty() {
                    super::layer::database_tables(s, &json!({"database":c.address,"password":ps(p,"password")}))?
                } else {
                    let tables = connection_use::tables(s, &c, ps(p, "password").filter(|pw| !pw.is_empty()))?;
                    let layers: Vec<_> = tables.iter().filter_map(|v| v["label"].as_str()).collect();
                    json!({"count":layers.len(),"layers":layers})
                }
            }
        }
        "service" => super::service::layers(s, &json!({"url": c.address, "connection":c.id}))?,
        "wms" | "wmts" | "wfs" | "wcs" | "ogcapi" | "arcgis" => {
            super::service::layers(s, &json!({"url": c.address, "connection":c.id, "type": c.kind}))?
        }
        "csw" => super::service::catalog_search(s, &json!({"url": c.address, "connection":c.id}))?,
        "stac" => super::stac::collections(s, &json!({"url": c.address, "connection":c.id}))?,
        "s3" => {
            s3::connection(&c.name).ok_or("log in first: connection.login {id, accessKeyId, secretAccessKey}")?;
            let mut q = json!({"connection": c.name, "limit": 100});
            if let Some(bucket) = first_bucket(&c) {
                q["bucket"] = json!(bucket);
            }
            super::s3::list(s, &q)?
        }
        _ => return Err(format!("unknown kind '{}'", c.kind)),
    };
    out["result"] = result;
    Ok(out)
}
