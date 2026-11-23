//! Named endpoint definitions, optional remembered passwords, web logins and S3 keys, off-frame
//! web probes, and explicit saved-connection choices for project sources.

use egui::vec2;
use rustgis_engine::connections::tile_test::{self, Outcome, Sample};
use rustgis_engine::connections::{
    Connection, DATABASE_KINDS, LoginReason, LoginRequest, SERVICE_KINDS as WEB_KINDS, WEB, database_key, database_label, login_origin,
};
use rustgis_engine::render::rustgis_tiles::{arcgis_token, cache, logins, s3};
use rustgis_engine::s3_connections::STALE;
use rustgis_engine::s3_keys as keys;
use rustgis_engine::service_logins as web;
use rustgis_engine::service_tokens as tokens;
use rustgis_engine::sources::Source;
use serde_json::{Value, json};

use crate::{RustGisApp, Slot, i18n};

#[derive(Default)]
pub struct State {
    pub id: Option<String>,
    pub kind: String,
    pub name: String,
    pub group: String,
    pub address: String,
    pub region: String,
    pub buckets: String,
    pub credentials: String,
    pub profile: String,
    pub password: String,
    pub auth: String,
    pub user: String,
    pub key_name: String,
    pub api_key: String,
    pub allow_insecure: bool,
    pub remember: bool,
    /// An ArcGIS token login: make the token from the user and a password (true) or paste it
    /// (false). The choice is only the form's.
    pub token_generate: bool,
    /// A pasted ArcGIS token and its optional expiry (Unix milliseconds): never kept once sent.
    pub token: String,
    pub token_expires: String,
    /// The https:// address a token is generated at. Not a secret, and never saved.
    pub token_endpoint: String,
    /// A token being generated for the saved connection the form is on: each frame asks how it
    /// is getting on, and closing or changing the form cancels it.
    pub generating: Option<Generating>,
    /// The current project's login prompt, never a password.
    pub request: Option<LoginRequest>,
    pub selected: Option<String>,
    pub access_key: String,
    pub secret_key: String,
    pub session_token: String,
    pub test: Option<(Value, Slot<Result<TestAnswer, String>>, TestContext)>,
    /// The form was opened to log in (Log In…): Save logs in even with no secret typed,
    /// using the connection's existing session login (or saved DB/S3 credentials).
    pub login: bool,
    /// A login that worked with a warning (translated, e.g. the keys weren't remembered): the
    /// dialog stays open to say so until it is closed or edited again.
    pub warned: Option<String>,
    /// A project's S3 source waiting for this connection (the chooser's), as (source address,
    /// connection id): its choice is made again once that connection has logged in.
    pub resolving: Option<(String, String)>,
    /// A SQL Server connection entered as an enterprise geodatabase is in ArcGIS Pro (server,
    /// sign-in, database) in place of the single address; [`definition`] composes the address.
    pub form: bool,
    /// The server, with `\instance` or `,port` when it has them.
    pub server: String,
    /// `"sql"` (a database user and password) or `"windows"` (the Windows user); `""` is `"sql"`.
    /// The database user is [`Self::user`], as a web service's basic login is.
    pub sign_in: String,
    pub database: String,
    /// Trust the server's certificate without checking who issued it (SQL Server's own
    /// self-signed certificate): `trust-server-certificate=true` in the address.
    pub trust_cert: bool,
    /// The Name was typed: a `.sde` file read afterwards leaves it as it is.
    pub typed_name: bool,
    /// The address's options the form has no field for (`encrypt=strict&ca=…`), as written:
    /// shown in the form's Options row and kept in the address it makes.
    pub form_options: String,
}

pub enum TestAnswer {
    Document(Vec<u8>),
    Tile(Sample, Outcome),
}

/// A token generation this form started: the connection it is for, and whether the form's Test
/// follows it (a Test never keeps the token past this session).
pub struct Generating {
    id: String,
    then_test: bool,
}

/// Authority and form identity for one off-frame Test. Dropping the pending Test revokes its
/// temporary login, including when a late download still owns the detached result slot.
pub struct TestContext {
    opened: u64,
    form: Value,
    /// Capture the displayed translation so a language change cannot orphan this Test's status.
    status: &'static str,
    _login: Option<web::TemporaryProbe>,
}

/// The chooser of the saved S3 connection that reads a project's S3 sources (Data Sources ›
/// Choose S3 connection…): what was picked, for the project it is for. Nothing is picked for the
/// user.
#[derive(Default)]
pub struct Chooser {
    /// The candidate picked for each resource waiting, by the source address that names it.
    picked: std::collections::BTreeMap<String, String>,
    /// The project (`Session::opened`) this state is for.
    project: Option<u64>,
    /// Whether the chooser may still open by itself for this project (once).
    auto: bool,
    error: Option<String>,
}

/// A fixed sentence the engine says about saved S3 keys and S3 connection choices, in the
/// current language; any other message as it is.
pub(crate) fn translated(e: &str) -> String {
    match e {
        keys::UNUSABLE => tl!("Saved S3 keys could not be used. Enter the keys again."),
        keys::NONE_SAVED => tl!("No saved S3 keys. Enter accessKeyId and secretAccessKey."),
        keys::UNAVAILABLE => tl!("Saved S3 keys are unavailable. Enter the keys for this session."),
        keys::NOT_REMEMBERED => tl!("Keys are available for this session but were not remembered on this computer."),
        keys::TEMPORARY => tl!("Temporary credentials are session-only. Previously saved keys are unchanged."),
        keys::NOT_FORGOTTEN => tl!("Saved S3 keys could not be forgotten. The connection was not changed."),
        s3::UNRESOLVED => tl!("Choose a saved S3 connection before opening this source."),
        s3::MISMATCH => tl!("The saved S3 connection no longer matches this source."),
        STALE => tl!("This S3 connection choice is no longer available."),
        web::UNUSABLE => tl!("The session login could not be used. Enter it again."),
        web::SAVED_UNUSABLE => tl!("The saved login could not be used. Enter it again."),
        web::NONE_SAVED => tl!("No saved login for this connection. Enter it again."),
        web::UNAVAILABLE => tl!("The saved login is unavailable. Enter it for this session."),
        web::NOT_REMEMBERED => tl!("The login is available for this session but was not remembered on this computer."),
        web::NOT_FORGOTTEN => tl!("The saved login could not be forgotten. The connection was not changed."),
        web::LOGGED_OUT => tl!("This connection is logged out. Log in explicitly to use it again."),
        web::TOKEN_EXPIRED => tl!("The ArcGIS token has expired. Log in to this connection again."),
        arcgis_token::BAD_TOKEN => tl!("The ArcGIS token must be at most 4096 ASCII letters, digits or - _ . ~ + / = characters."),
        arcgis_token::BAD_EXPIRY => tl!("expiresAt must be a Unix time in milliseconds, in the future."),
        arcgis_token::NO_CLOCK => tl!("This computer's clock can't be read, so an expiring ArcGIS token can't be used."),
        arcgis_token::BAD_ENDPOINT => tl!(
            "The token endpoint must be an https:// address at this connection's own origin, without user information, a query or a fragment."
        ),
        arcgis_token::REFUSED => tl!("The server didn't give a token. Check the user name, password and token endpoint."),
        arcgis_token::BAD_ANSWER => tl!("The server's answer wasn't an ArcGIS token."),
        arcgis_token::FAILED => tl!("The token request failed."),
        arcgis_token::TIMED_OUT => tl!("The server didn't answer the token request in time."),
        arcgis_token::TOO_LARGE => tl!("The server's answer to the token request was too large."),
        arcgis_token::CANCELLED => tl!("The token request was cancelled."),
        arcgis_token::NO_WORKER => tl!("The token request couldn't be started."),
        arcgis_token::NO_TRANSPORT => tl!("This app can't request ArcGIS tokens."),
        tokens::ALREADY => tl!("A token is already being generated for this connection. Cancel it first."),
        tokens::NO_USER => tl!("This connection has no user: set user (connection.edit) to generate a token."),
        tokens::BAD_PASSWORD => tl!("The password must be at most 4096 bytes, without control characters."),
        tokens::CHANGED => tl!("The connection changed while its token was being generated. Generate it again."),
        web::INSECURE => tl!("This connection uses unencrypted http://. Tick the confirmation before sending its password or key."),
        web::BAD_SECRET => tl!("The password or key must fit a 4096-byte HTTP header, without control characters."),
        web::HEADER_ASCII => tl!("An API key sent in a header must use plain ASCII characters."),
        web::REQUIRED => tl!("Enter this connection's password, API key or token to log in."),
        web::UNSAVED => tl!("Save the connection first: a login belongs to a saved connection."),
        web::NO_ORIGIN => tl!("The connection's address has no http:// or https:// origin a login can go to."),
        web::SAVE_BEFORE_TEST => tl!("Save changes before testing this connection."),
        tile_test::NOT_TILE => tl!("The server answered, but the response is not a usable tile."),
        tile_test::UNEXPANDED => tl!("The tile URL has an unsupported placeholder."),
        tile_test::OTHER_ORIGIN => tl!("The sample tile changes the connection's origin. Use a concrete hostname to test with a saved login."),
        web::NOT_THIS_SESSION => tl!("This request's login does not belong to this session."),
        web::CHOOSE_CONNECTION => tl!("Several saved connections match this address. Choose a connection explicitly."),
        web::NO_SUCH_CONNECTION => tl!("No saved web-service connection with this id."),
        web::BAD_DEFINITION => tl!("The service test requires a valid web-service connection and login."),
        web::BAD_ADDRESS => tl!("The service test requires a credential-free HTTP or HTTPS address at the connection's origin."),
        web::CONNECTION_TEXT => tl!("connection must be a saved web-service connection id (text)"),
        other => other,
    }
    .to_string()
}

/// Whether a login failed for want of usable saved keys: typing them is the way on.
pub(crate) fn needs_keys(e: &str) -> bool {
    matches!(e, keys::NONE_SAVED | keys::UNAVAILABLE | keys::UNUSABLE)
}

/// Why the Remember checkbox can't be used, when it can't: no credential store (the browser, or a
/// shell without one), or temporary credentials (a session token), which are never kept.
pub(crate) fn remember_blocked(store: bool, session_token: &str) -> Option<&'static str> {
    if !store {
        Some(tl!("Keys are kept for this session only: there is no credential store to remember them in here."))
    } else if !session_token.trim().is_empty() {
        Some(tl!("Temporary credentials (with a session token) are kept for this session only."))
    } else {
        None
    }
}

/// The checkbox, its reason when it can't be used, and what it does when it can. Unticked when
/// it can't be used.
pub(crate) fn remember_checkbox(ui: &mut egui::Ui, remember: &mut bool, blocked: Option<&str>) -> bool {
    if blocked.is_some() {
        *remember = false;
    }
    let changed = ui.add_enabled(blocked.is_none(), egui::Checkbox::new(remember, tl!("Remember on this computer"))).changed();
    let note = blocked.unwrap_or(tl!(
        "Keeps the access key ID and secret access key in this computer's credential store, never in the connection or the project."
    ));
    ui.label(egui::RichText::new(note).small().weak());
    changed
}

/// Forgets the keys saved on this computer for the S3 connection `id`; its login in this session
/// keeps working. A failure leaves everything as it was.
pub(crate) fn forget_keys(app: &mut RustGisApp, id: &str) -> Result<(), String> {
    app.run("connection.forgetSavedKeys", json!({"id": id})).map(|_| ()).map_err(|e| translated(&e))
}

pub(crate) const KINDS: [(&str, &str); 13] = [
    ("postgres", "PostgreSQL/PostGIS"),
    ("mysql", "MySQL/MariaDB"),
    ("sqlserver", "SQL Server"),
    ("service", "Web service (detected)"),
    ("wms", "WMS"),
    ("wmts", "WMTS"),
    ("wfs", "WFS"),
    ("wcs", "WCS"),
    ("ogcapi", "OGC API"),
    ("arcgis", "ArcGIS REST"),
    ("csw", "Catalogue (CSW)"),
    ("stac", "STAC API"),
    ("s3", "S3"),
];

/// The kinds Add From Service connects to.
pub(crate) const SERVICE_KINDS: &[&str] = &["service", "wms", "wmts", "wfs", "wcs", "ogcapi", "arcgis", "csw"];

/// The connection a dialog was opened on, while its trimmed, redacted address still matches.
/// Never infers identity from an address alone.
pub(crate) fn picked<'a>(list: &'a [Connection], id: Option<&str>, address: &str) -> Option<&'a Connection> {
    let id = id?;
    let c = list.iter().find(|c| c.id == id)?;
    (c.address == rustgis_engine::sources::redact(address.trim())).then_some(c)
}

/// Draws a saved-connection row when these kinds exist; returns only a newly picked connection.
pub(crate) fn picker(ui: &mut egui::Ui, id: &str, list: &[Connection], kinds: &[&str], current: Option<&Connection>) -> Option<Connection> {
    if !list.iter().any(|c| kinds.contains(&c.kind.as_str())) {
        return None;
    }
    ui.horizontal(|ui| {
        ui.label(tl!("Saved connection"));
        picker_combo(ui, id, list, kinds, current)
    })
    .inner
}

/// The picker control without its label, for a dialog's existing form grid.
pub(crate) fn picker_combo(
    ui: &mut egui::Ui,
    id: &str,
    list: &[Connection],
    kinds: &[&str],
    current: Option<&Connection>,
) -> Option<Connection> {
    let label = |c: &Connection| if c.group.is_empty() { c.name.clone() } else { format!("{} › {}", c.group, c.name) };
    let mut picked = None;
    egui::ComboBox::from_id_salt(id)
        .width(280.0)
        .selected_text(current.map(label).unwrap_or_else(|| tl!("Pick one…").to_string()))
        .show_ui(ui, |ui| {
            for c in list.iter().filter(|c| kinds.contains(&c.kind.as_str())) {
                let selected = current.is_some_and(|x| x.id == c.id);
                let hover = if c.kind == "s3" && c.address.is_empty() { tl!("Amazon S3") } else { &c.address };
                if ui.selectable_label(selected, label(c)).on_hover_text(hover).clicked() && !selected {
                    picked = Some(c.clone());
                }
            }
        })
        .response
        .on_hover_text(tl!("Saved connection"));
    picked
}

/// Keeps an endpoint, never its secrets, as a new connection even at an existing address.
pub(crate) fn keep(app: &mut RustGisApp, p: Value) -> Result<String, String> {
    let v = app.run("connection.new", p)?;
    let id = v["id"].as_str().map(str::to_owned).ok_or_else(|| "connection.new returned no id".to_string())?;
    app.session.notify(i18n::fmt("Saved as the connection {name}", &[("name", v["name"].as_str().unwrap_or(""))]), false);
    Ok(id)
}

/// Offers to keep the endpoint connected by an add dialog.
pub(crate) fn keep_button(ui: &mut egui::Ui) -> bool {
    ui.button(tl!("Save as Connection"))
        .on_hover_text(tl!("Keep this endpoint under Data Sources › Connections, without its password or keys"))
        .clicked()
}

pub(crate) fn label(kind: &str) -> &'static str {
    KINDS.iter().find(|(k, _)| *k == kind).map(|(_, l)| *l).unwrap_or("Connection")
}

pub fn open_new(app: &mut RustGisApp, kind: &str) -> Result<(), String> {
    if !KINDS.iter().any(|(k, _)| *k == kind) {
        return Err(format!("unknown kind '{kind}'"));
    }
    if cfg!(target_arch = "wasm32") && DATABASE_KINDS.contains(&kind) {
        return Err(tl!("Database connections are used by the desktop app").into());
    }
    replace_form(
        app,
        State { kind: kind.into(), credentials: if kind == "s3" { "keys".into() } else { String::new() }, ..Default::default() },
    );
    app.dialogs = crate::dialogs::Dialogs { open: Some("newConnection"), ..Default::default() };
    Ok(())
}

pub fn open_edit(app: &mut RustGisApp, id: &str) -> Result<(), String> {
    let c = app.session.connections.find(id).cloned().ok_or_else(|| format!("no connection '{id}'"))?;
    replace_form(
        app,
        State {
            region: c.options.get("region").cloned().unwrap_or_default(),
            buckets: c.options.get("buckets").cloned().unwrap_or_default(),
            credentials: c.options.get("credentials").cloned().unwrap_or_else(|| "none".into()),
            profile: c.options.get("profile").cloned().unwrap_or_default(),
            auth: c.auth,
            user: c.user,
            key_name: c.key_name,
            id: Some(c.id),
            kind: c.kind,
            name: c.name,
            group: c.group,
            address: c.address,
            ..Default::default()
        },
    );
    app.dialogs = crate::dialogs::Dialogs { open: Some("editConnection"), ..Default::default() };
    Ok(())
}

/// Opens the connection's form to log in (Log In…): Save then logs in. True when it opened.
pub fn log_in(app: &mut RustGisApp, id: &str) -> bool {
    match app.run("ui.connection", json!({"id": id})) {
        Ok(_) => {
            app.ui.connection.login = true;
            let saved = app.ui.connection.kind == "s3"
                && app.ui.connection.credentials == "keys"
                && app.ui.connection.id.as_deref().and_then(|id| app.session.connections.find(id)).is_some_and(|c| c.saved_keys);
            app.dialogs.info = Some(if DATABASE_KINDS.contains(&app.ui.connection.kind.as_str()) {
                tl!("Type the password, then Save. Remember on this computer is optional.").to_string()
            } else if WEB_KINDS.contains(&app.ui.connection.kind.as_str()) && app.ui.connection.auth == "arcgis" {
                tl!("Paste a token, or type the password to make one, or leave it blank to use this connection's session or saved login, then Save.").to_string()
            } else if WEB_KINDS.contains(&app.ui.connection.kind.as_str()) {
                tl!("Type the password or key, or leave it blank to use this connection's session or saved login, then Save.").to_string()
            } else if saved {
                tl!("Leave the keys blank to log in with the keys saved on this computer, or type others, then Save.").to_string()
            } else {
                tl!("Type the keys to log in for this session, then Save.").to_string()
            });
            true
        }
        Err(e) => {
            app.session.notify(e, true);
            false
        }
    }
}

/// What an `mssql://` connection address says, to fill the geodatabase form with.
#[derive(Debug, PartialEq)]
pub(crate) struct Mssql {
    /// `host`, with `\instance` and `,port` when the address has them (1433, the default port,
    /// is left out).
    pub server: String,
    pub user: Option<String>,
    pub database: String,
    /// `trust-server-certificate=true` (or `yes`) is in the address.
    pub trust_cert: bool,
    /// The address has a password, token or key in it (`user:password@`, `pwd=`, …), which the
    /// form doesn't show.
    pub password: bool,
    /// The query's other options, as written and in order (`encrypt=strict&ca=…`).
    pub options: String,
    /// The last `instance` is empty (`?instance=` or a bare `?instance`), which the engine reads
    /// as a route of its own and the form can't show.
    pub empty_instance: bool,
}

/// The server, user, database and certificate choice of an
/// `mssql://user@host:port/database?instance=name` address; `None` for any other address.
pub(crate) fn parse_mssql(address: &str) -> Option<Mssql> {
    use rustgis_engine::data::{address::percent_decode, source::msconn};
    // The engine's own reading takes any password, token or key out (one with an unencoded `/`,
    // `?` or `@` too); the rest is read from what is left.
    let address = address.trim();
    let (public, _) = msconn::split_password(address)?;
    let password = public != address;
    let rest = public.get(..8).filter(|scheme| scheme.eq_ignore_ascii_case("mssql://")).and_then(|_| public.get(8..))?;
    let (rest, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (authority, _path) = rest.split_once('/').unwrap_or((rest, ""));
    let (user, hostport) = match authority.rsplit_once('@') {
        Some((user, hostport)) => (Some(percent_decode(user)), hostport),
        None => (None, authority),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => (host, Some(port)),
        _ => (hostport, None),
    };
    let key = |kv: &str| kv.split('=').next().unwrap_or("").to_ascii_lowercase().replace('_', "-");
    // The certificate choice, read as the engine reads it: `Some` for a value it takes.
    let trust = |kv: &str| match key(kv).as_str() {
        "trust-server-certificate" | "trustservercertificate" => {
            match percent_decode(kv.split_once('=').map_or("", |(_, v)| v)).to_ascii_lowercase().as_str() {
                "true" | "yes" | "1" => Some(true),
                "false" | "no" | "0" => Some(false),
                _ => None,
            }
        }
        _ => None,
    };
    // What the form has fields for; everything else (a certificate value the engine refuses too,
    // for Test to say why) is kept as written.
    let options =
        query.split('&').filter(|kv| !kv.is_empty() && key(kv) != "instance" && trust(kv).is_none()).collect::<Vec<_>>().join("&");
    // The last `instance` is the one, as the engine reads it; a bare key is an empty one.
    let last_instance =
        query.split('&').filter(|kv| key(kv) == "instance").map(|kv| percent_decode(kv.split_once('=').map_or("", |(_, v)| v))).next_back();
    let empty_instance = last_instance.as_deref().is_some_and(str::is_empty);
    let instance = last_instance.filter(|v| !v.is_empty());
    let trust_cert = query.split('&').filter_map(trust).next_back().unwrap_or(false);
    let mut server = percent_decode(host);
    let named_instance = instance.is_some();
    if let Some(instance) = instance {
        server.push('\\');
        server.push_str(&instance);
    }
    // 1433, the default port, is left out; with an instance a port given is the route, and kept.
    if let Some(port) = port.filter(|p| *p != "1433" || named_instance) {
        server.push(',');
        server.push_str(port);
    }
    // The database as the engine reads it: boundary slashes aren't part of it.
    let database = msconn::database_name(&public).unwrap_or_default();
    Some(Mssql { server, user: user.filter(|u| !u.is_empty()), database, trust_cert, password, options, empty_instance })
}

/// The address the geodatabase form makes, or why it can't.
fn form_address(st: &State) -> Result<String, String> {
    use rustgis_engine::data::source::sdefile::{SdeConnection, mssql_address};
    if st.server.trim().is_empty() {
        return Err(tl!("Type the server").into());
    }
    let windows = st.sign_in == "windows";
    // No user is Windows sign-in to the engine: a database user must be named.
    if !windows && st.user.trim().is_empty() {
        return Err(tl!("Type the user name, or choose Windows sign-in").into());
    }
    let mut address = mssql_address(&SdeConnection {
        platform: "sqlserver".into(),
        server: st.server.clone(),
        database: st.database.trim().to_owned(),
        user: (!windows).then(|| st.user.trim().to_owned()).filter(|u| !u.is_empty()),
        os_auth: windows,
        version: None,
    })
    .map_err(|e| e.to_string())?;
    let options = st.form_options.trim().trim_matches(|c| c == '&' || c == '?');
    // The instance has its place in Server, and the certificate choice its box: the form reads
    // them back from there.
    let keys = || options.split('&').map(|kv| kv.split('=').next().unwrap_or("").trim().to_ascii_lowercase().replace('_', "-"));
    if keys().any(|k| k == "instance") {
        return Err(tl!("Put the instance in Server, not in Options").into());
    }
    if keys().any(|k| k == "trust-server-certificate" || k == "trustservercertificate") {
        return Err(tl!("Tick Trust the server's certificate instead of typing it in Options").into());
    }
    for option in [options, if st.trust_cert { "trust-server-certificate=true" } else { "" }] {
        if !option.is_empty() {
            address.push(if address.contains('?') { '&' } else { '?' });
            address.push_str(option);
        }
    }
    // A password, token or key typed in Options would be shown and then left out on Save: it
    // belongs in the Password box.
    if rustgis_engine::data::source::msconn::split_password(&address).is_some_and(|(public, _)| public != address) {
        return Err(tl!("Type the password in the Password box, not in Options").into());
    }
    Ok(address)
}

/// Why the geodatabase form can't make an address (`None` outside the form, or when it can).
fn form_error(st: &State) -> Option<String> {
    if st.form { form_address(st).err() } else { None }
}

/// Switches `st` to the geodatabase form, filled from the address box's `mssql://` address
/// (with its other options kept in the Options row), then with these. An address with a
/// password in it, or one that isn't `mssql://`, stays in the address box, and why is the error;
/// an empty one opens the form as it was.
pub(crate) fn use_form(st: &mut State, server: Option<&str>, database: Option<&str>) -> Result<(), String> {
    let parsed = parse_mssql(&st.address);
    if parsed.is_none() && !st.address.trim().is_empty() {
        return Err(tl!(
            "This address isn't a SQL Server address (mssql://…), so the form can't be filled from it. Fix it, or keep using the address."
        )
        .into());
    }
    if let Some(m) = parsed {
        if m.empty_instance {
            return Err(tl!(
                "This address names an empty instance, which the form can't show: name the instance or take it out, or keep using the address."
            )
            .into());
        }
        if m.password {
            return Err(tl!(
                "This address has a password or key in it, which the form doesn't show: take it out of the address and type the password in the Password box, or keep using the address."
            )
            .into());
        }
        // Filled only when the form can write the address back (an IPv6 server it can't).
        let mut filled = State {
            sign_in: if m.user.is_some() { "sql" } else { "windows" }.into(),
            user: m.user.unwrap_or_default(),
            server: m.server,
            database: m.database,
            trust_cert: m.trust_cert,
            form_options: m.options,
            ..Default::default()
        };
        match form_address(&filled) {
            Err(why) => {
                return Err(i18n::fmt("The form can't show this address ({why}): keep using the address.", &[("why", &why)]));
            }
            // The address it writes must name the same database (the form trims what is typed).
            Ok(back) if rustgis_engine::data::source::msconn::database_name(&back).unwrap_or_default() != filled.database => {
                return Err(tl!("The form can't show this address's database name: keep using the address.").into());
            }
            Ok(_) => {}
        }
        st.sign_in = std::mem::take(&mut filled.sign_in);
        st.user = std::mem::take(&mut filled.user);
        st.server = std::mem::take(&mut filled.server);
        st.database = std::mem::take(&mut filled.database);
        st.trust_cert = filled.trust_cert;
        st.form_options = std::mem::take(&mut filled.form_options);
    }
    st.form = true;
    if st.sign_in.is_empty() {
        st.sign_in = "sql".into();
    }
    if let Some(server) = server {
        st.server = server.to_owned();
    }
    if let Some(database) = database {
        st.database = database.to_owned();
    }
    Ok(())
}

/// "Enter an address instead" / "Use the form instead": what the form composed goes into the
/// address box, and the address box's address fills the form. A form with a server typed that
/// can't make an address stays, and why is the error; so does an address the form can't show.
/// A form with nothing typed has nothing to lose: the address box is left as it was.
fn swap_form(st: &mut State) -> Result<(), String> {
    if st.form {
        let empty = st.server.trim().is_empty()
            && st.user.trim().is_empty()
            && st.database.trim().is_empty()
            && st.form_options.trim().is_empty()
            && !st.trust_cert;
        if !empty {
            st.address = form_address(st)?;
        }
        st.form = false;
        Ok(())
    } else {
        use_form(st, None, None)
    }
}

/// The geodatabase form's rows in the dialog's grid: platform, server, how to sign in, the user
/// and password (for a database user), database, the certificate choice, name and group. True
/// when edited.
fn form_rows(ui: &mut egui::Ui, st: &mut State) -> bool {
    let mut changed = false;
    if st.sign_in.is_empty() {
        st.sign_in = "sql".into();
    }
    let input_label = ui.label(tl!("Platform"));
    // SQL Server is the only platform read, so the choice is shown but not offered.
    ui.add_enabled_ui(false, |ui| {
        egui::ComboBox::from_id_salt("connection_platform")
            .selected_text(tl!("SQL Server"))
            .width(360.0)
            .show_ui(ui, |ui| {
                let _ = ui.selectable_label(true, tl!("SQL Server"));
            })
            .response
            .labelled_by(input_label.id)
    });
    ui.end_row();
    let input_label = ui.label(tl!("Server"));
    changed |= ui
        .add(egui::TextEdit::singleline(&mut st.server).hint_text(tl!("Server or server\\instance")).desired_width(360.0))
        .labelled_by(input_label.id)
        .changed();
    ui.end_row();
    let input_label = ui.label(tl!("Sign in"));
    let shown = if st.sign_in == "windows" { tl!("Windows") } else { tl!("Database user") };
    egui::ComboBox::from_id_salt("connection_sign_in")
        .selected_text(shown)
        .width(360.0)
        .show_ui(ui, |ui| {
            changed |= ui.selectable_value(&mut st.sign_in, "sql".to_owned(), tl!("Database user")).changed();
            changed |= ui.selectable_value(&mut st.sign_in, "windows".to_owned(), tl!("Windows")).changed();
        })
        .response
        .labelled_by(input_label.id);
    ui.end_row();
    if st.sign_in == "windows" {
        // The Windows user signs in: there is no user name or password to type or keep.
        st.password.clear();
        st.remember = false;
    } else {
        let input_label = ui.label(tl!("User"));
        changed |= ui.add(egui::TextEdit::singleline(&mut st.user).desired_width(360.0)).labelled_by(input_label.id).changed();
        ui.end_row();
        let input_label = ui.label(tl!("Password"));
        changed |= ui
            .add(egui::TextEdit::singleline(&mut st.password).password(true).hint_text(tl!("Optional")).desired_width(360.0))
            .labelled_by(input_label.id)
            .changed();
        ui.end_row();
        ui.label("");
        changed |= ui.checkbox(&mut st.remember, tl!("Remember on this computer")).changed();
        ui.end_row();
    }
    let input_label = ui.label(tl!("Database"));
    changed |= ui.add(egui::TextEdit::singleline(&mut st.database).desired_width(360.0)).labelled_by(input_label.id).changed();
    ui.end_row();
    ui.label("");
    changed |= ui
        .checkbox(&mut st.trust_cert, tl!("Trust the server's certificate"))
        .on_hover_text(tl!(
            "Connect even though the server's certificate isn't from an authority this computer trusts, as ArcGIS Pro and SQL Server Management Studio do with Trust server certificate. SQL Server's own self-signed certificate needs it. Tick it only for your organisation's server."
        ))
        .changed();
    ui.end_row();
    // The address's options the form has no field for, shown so none is kept out of sight.
    if !st.form_options.is_empty() {
        let input_label = ui.label(tl!("Options"));
        changed |= ui
            .add(egui::TextEdit::singleline(&mut st.form_options).desired_width(360.0))
            .labelled_by(input_label.id)
            .on_hover_text(tl!("The address's other options, as written, such as encrypt=strict or ca=…: they stay in the address."))
            .changed();
        ui.end_row();
    }
    let input_label = ui.label(tl!("Name"));
    let default = i18n::fmt("Geodatabase on {server}", &[("server", st.server.trim())]);
    let edited =
        ui.add(egui::TextEdit::singleline(&mut st.name).hint_text(default).desired_width(360.0)).labelled_by(input_label.id).changed();
    st.typed_name |= edited;
    changed |= edited;
    ui.end_row();
    let input_label = ui.label(tl!("Group"));
    changed |=
        ui.add(egui::TextEdit::singleline(&mut st.group).hint_text(tl!("None")).desired_width(360.0)).labelled_by(input_label.id).changed();
    ui.end_row();
    changed
}

/// From .sde file…: `connection.readSde` reads the ArcGIS Pro connection file and the form is
/// filled from what it says. Nothing is saved until Save, and the file's password is never read.
/// The name is the file's unless one was typed. A file that is refused leaves the form as it was.
pub(crate) fn fill_from_sde(app: &mut RustGisApp, path: &str) -> Result<(), String> {
    let v = app.run("connection.readSde", json!({"path": path}))?;
    let text = |key: &str| v[key].as_str().unwrap_or_default().to_owned();
    let windows = v["authentication"] == "os";
    let st = &mut app.ui.connection;
    st.kind = "sqlserver".into();
    st.form = true;
    // The certificate choice and the address's options were for the server before.
    let cleared = !st.server.trim().is_empty()
        && !st.server.trim().eq_ignore_ascii_case(text("server").trim())
        && (st.trust_cert || !st.form_options.is_empty());
    if cleared {
        st.trust_cert = false;
        st.form_options.clear();
    }
    st.server = text("server");
    st.database = text("database");
    st.sign_in = if windows { "windows" } else { "sql" }.into();
    st.user = if windows { String::new() } else { text("user") };
    // A saved connection's name was typed when it was made.
    if st.id.is_none() && !st.typed_name {
        st.name = std::path::Path::new(path).file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    }
    st.password.clear();
    st.remember = false;
    st.test = None;
    app.dialogs.error = None;
    let next = if windows {
        tl!("This file signs in as the Windows user: Save, or Test to check the connection.")
    } else if app.ui.connection.user.is_empty() {
        tl!("Type the user and the password, then Save.")
    } else {
        tl!("Type the password, then Save.")
    };
    app.dialogs.info = Some(if cleared {
        i18n::fmt("The certificate choice and the options were for the previous server, and were cleared. {next}", &[("next", next)])
    } else {
        next.to_string()
    });
    Ok(())
}

fn definition(st: &State) -> Value {
    if st.form && st.kind == "sqlserver" {
        // An address the form can't make is left empty: Save and Test say why first.
        let address = form_address(st).unwrap_or_default();
        let name =
            // Saved data, so not translated; only the Name box's hint is.
            if st.name.trim().is_empty() { format!("Geodatabase on {}", st.server.trim()) } else { st.name.clone() };
        return json!({"kind":st.kind, "name":name, "group":st.group, "address":address});
    }
    let address = st.address.trim();
    let lower = address.to_ascii_lowercase();
    let address = if st.kind == "postgres" && !address.is_empty() && !lower.starts_with("postgres:") && !lower.starts_with("postgresql:") {
        format!("postgres:{address}")
    } else {
        address.to_owned()
    };
    let mut p = json!({"kind":st.kind, "name":st.name, "group":st.group, "address":address});
    if st.kind == "s3" {
        p["options"] = json!({"region":st.region, "buckets":st.buckets, "credentials":st.credentials});
        if st.credentials == "profile" {
            p["options"]["profile"] = json!(st.profile);
        }
    }
    if WEB_KINDS.contains(&st.kind.as_str()) {
        p["auth"] = json!(if st.auth.is_empty() { "none" } else { &st.auth });
        p["user"] = json!(st.user);
        p["keyName"] = json!(st.key_name);
    }
    p
}

fn secrets(st: &State, p: &mut Value) -> Result<(), String> {
    if DATABASE_KINDS.contains(&st.kind.as_str()) && !st.password.is_empty() {
        p["password"] = json!(st.password);
    }
    if st.kind == "s3" && st.credentials == "keys" {
        for (key, value) in [("accessKeyId", &st.access_key), ("secretAccessKey", &st.secret_key), ("sessionToken", &st.session_token)] {
            if !value.is_empty() {
                p[key] = json!(value);
            }
        }
    }
    if WEB_KINDS.contains(&st.kind.as_str()) && st.auth == "arcgis" {
        // What was typed is refused with its reason, never dropped to log in with another login.
        if let Some(token) = pasted_token(st)? {
            p["token"] = json!(token.token.expose());
            if let Some(at) = token.expires_at {
                p["expiresAt"] = json!(at);
            }
            p["allowInsecure"] = json!(st.allow_insecure);
        }
    } else if WEB_KINDS.contains(&st.kind.as_str()) {
        let (key, value) = if st.auth == "basic" { ("password", &st.password) } else { ("apiKey", &st.api_key) };
        if !st.auth.is_empty() && !value.is_empty() {
            p[key] = json!(value);
            p["allowInsecure"] = json!(st.allow_insecure);
        }
    }
    Ok(())
}

/// The ArcGIS token typed in the form (pasted mode) with its expiry, checked: `None` when no token
/// is typed. An expiry that isn't a Unix time in milliseconds is an error, not a guess.
fn pasted_token(st: &State) -> Result<Option<arcgis_token::Credential>, String> {
    if st.token_generate {
        return Ok(None);
    }
    let token = st.token.trim();
    let expires = st.token_expires.trim();
    if token.is_empty() {
        return if expires.is_empty() { Ok(None) } else { Err(tl!("Type the token this expiry is for.").into()) };
    }
    let expires_at = if expires.is_empty() {
        None
    } else {
        Some(
            expires
                .parse::<i64>()
                .ok()
                .filter(|at| *at > 0)
                .ok_or_else(|| tl!("Enter the expiry as a Unix time in milliseconds, or leave it blank.").to_string())?,
        )
    };
    let credential = arcgis_token::Credential::new(token, expires_at).map_err(translated)?;
    let now = arcgis_token::now_millis();
    if credential.is_expired(now) {
        return Err(translated(if now.is_none() { arcgis_token::NO_CLOCK } else { arcgis_token::EXPIRED }));
    }
    Ok(Some(credential))
}

/// The secret a service login of the form sends as its method does: the password (basic), the
/// key, or an ArcGIS token's private payload.
fn login_secret(st: &State) -> Result<String, String> {
    match st.auth.as_str() {
        "basic" => Ok(st.password.clone()),
        "arcgis" => Ok(pasted_token(st)?.map(|token| token.payload().expose().to_string()).unwrap_or_default()),
        _ => Ok(st.api_key.clone()),
    }
}

/// Whether the form generates an ArcGIS token from the typed password.
fn generating_form(st: &State) -> bool {
    WEB_KINDS.contains(&st.kind.as_str()) && st.auth == "arcgis" && st.token_generate
}

/// What generating a token needs, checked before anything is saved or sent: the user, the
/// password and a token endpoint at the connection's own origin.
fn check_generation(st: &State) -> Result<(), String> {
    if st.user.trim().is_empty() {
        return Err(tl!("Enter the user to generate a token for.").into());
    }
    if st.password.is_empty() {
        return Err(tl!("Enter the password to generate a token.").into());
    }
    if !logins::valid_value(&st.password) {
        return Err(translated(tokens::BAD_PASSWORD));
    }
    if st.token_endpoint.trim().is_empty() {
        return Err(tl!("Enter the token endpoint (https://host/arcgis/tokens/generateToken).").into());
    }
    let (connection, _) = rustgis_engine::connections::clean(Connection {
        kind: st.kind.clone(),
        address: st.address.clone(),
        auth: st.auth.clone(),
        user: st.user.clone(),
        ..Default::default()
    })?;
    let origin = login_origin(&connection).ok_or_else(|| translated(web::NO_ORIGIN))?;
    arcgis_token::check_endpoint(st.token_endpoint.trim(), &origin).map_err(translated)
}

/// A saved service Test uses its saved login-relevant definition, never an edited endpoint
/// with an old id. A new endpoint uses only credentials explicitly typed into its form.
fn test_blocked(app: &RustGisApp) -> Option<&'static str> {
    let st = &app.ui.connection;
    let saved = st.id.as_deref().and_then(|id| app.session.connections.find(id));
    if !WEB_KINDS.contains(&st.kind.as_str()) && !saved.is_some_and(web::is_service) {
        return None;
    }
    let auth = if st.auth == "none" { "" } else { &st.auth };
    st.id.as_ref()?;
    let matches = saved.is_some_and(|c| {
        c.kind == st.kind
            && c.address == st.address.trim()
            && c.auth == auth
            && c.user == st.user.trim()
            && c.key_name == st.key_name.trim()
    });
    (!matches).then_some(tl!("Save changes before testing this connection."))
}

pub fn start_test(app: &mut RustGisApp) {
    app.dialogs.error = None;
    app.dialogs.info = None;
    app.ui.connection.test = None;
    if let Some(e) = form_error(&app.ui.connection) {
        app.dialogs.error = Some(e);
        return;
    }
    let mut p = definition(&app.ui.connection);
    let service = WEB_KINDS.contains(&app.ui.connection.kind.as_str());
    if let Some(reason) = test_blocked(app) {
        app.dialogs.error = Some(reason.into());
        return;
    }
    // A token made from a password goes through the same background command a Save uses, never
    // remembered (remember is off); the Test runs once it lands. It belongs to a saved connection.
    if generating_form(&app.ui.connection) && !app.ui.connection.password.is_empty() {
        let Some(id) = app.ui.connection.id.clone() else {
            clear_secrets(&mut app.ui.connection);
            app.dialogs.error = Some(tl!("Save the connection before generating a token.").into());
            return;
        };
        if let Err(e) = check_generation(&app.ui.connection).and_then(|()| start_generating(app, &id, false, true)) {
            app.dialogs.error = Some(e);
        }
        return;
    }
    if service && typed_secrets(&app.ui.connection) && insecure(&app.ui.connection) && !app.ui.connection.allow_insecure {
        app.dialogs.error = Some(translated(web::INSECURE));
        return;
    }
    // A pasted ArcGIS token is tested through a temporary login too, saved connection or not: the
    // saved one (and its session login) is neither used nor replaced, and the probe ends with the Test.
    let typed_token =
        service && app.ui.connection.auth == "arcgis" && !app.ui.connection.token_generate && !app.ui.connection.token.trim().is_empty();
    let temporary = service && !matches!(app.ui.connection.auth.as_str(), "" | "none") && (app.ui.connection.id.is_none() || typed_token);
    if service
        && !temporary
        && let Some(id) = &app.ui.connection.id
    {
        p["id"] = json!(id);
    }
    if temporary && app.ui.connection.auth == "query" && !logins::valid_query_name(app.ui.connection.key_name.trim()) {
        clear_secrets(&mut app.ui.connection);
        app.dialogs.error = Some(tl!("Enter a valid query parameter name before testing this connection.").into());
        return;
    }
    // The anonymous command must not record a pasted query key before the temporary login
    // exists. Keep its cleaned definition for the response command and temporary probe too.
    let query_address = (temporary && app.ui.connection.auth == "query")
        .then(|| logins::without_query_parameter(&app.ui.connection.address, app.ui.connection.key_name.trim()));
    if temporary {
        // Public commands remain anonymous. The typed secret is passed directly to the helper,
        // never through command parameters, command history or a saved connection.
        p["auth"] = json!("none");
        if let Some(address) = &query_address {
            p["address"] = json!(address);
        }
    } else if let Err(e) = secrets(&app.ui.connection, &mut p) {
        clear_secrets(&mut app.ui.connection);
        app.dialogs.error = Some(e);
        return;
    }
    if DATABASE_KINDS.contains(&app.ui.connection.kind.as_str()) {
        match app.run("connection.test", p) {
            Ok(v) => app.dialogs.info = Some(summary(&app.ui.connection.kind, &v["result"])),
            Err(e) => app.dialogs.error = Some(e),
        }
        return;
    }
    p["probe"] = json!(true);
    let result = app.run("connection.test", p);
    if service && !temporary {
        clear_secrets(&mut app.ui.connection);
    }
    let v = match result {
        Ok(v) => v,
        Err(e) => {
            if temporary {
                clear_secrets(&mut app.ui.connection);
            }
            app.dialogs.error = Some(translated(&e));
            return;
        }
    };
    let Some(url) = v["fetch"].as_str() else {
        if temporary {
            clear_secrets(&mut app.ui.connection);
        }
        app.dialogs.error = Some(tl!("Nothing to test").into());
        return;
    };
    let mut q = definition(&app.ui.connection);
    let (url, login) = if temporary {
        q["auth"] = json!("none");
        if let Some(address) = &query_address {
            q["address"] = json!(address);
        }
        let secret = match login_secret(&app.ui.connection) {
            Ok(secret) => secret,
            Err(e) => {
                clear_secrets(&mut app.ui.connection);
                app.dialogs.error = Some(e);
                return;
            }
        };
        let st = &app.ui.connection;
        let connection = Connection {
            kind: st.kind.clone(),
            address: query_address.as_ref().unwrap_or(&st.address).clone(),
            auth: st.auth.clone(),
            user: st.user.clone(),
            key_name: st.key_name.clone(),
            ..Default::default()
        };
        let result = web::temporary_probe(&connection, &secret, st.allow_insecure, url);
        clear_secrets(&mut app.ui.connection);
        match result {
            Ok((url, login)) => (url, Some(login)),
            Err(e) => {
                app.dialogs.error = Some(translated(&e));
                return;
            }
        }
    } else {
        (url.to_string(), None)
    };
    if service
        && !temporary
        && let Some(id) = &app.ui.connection.id
    {
        q["id"] = json!(id);
    }
    let status = tl!("Testing…");
    let context = TestContext { opened: app.session.opened, form: definition(&app.ui.connection), status, _login: login };
    let slot: Slot<Result<TestAnswer, String>> = Slot::default();
    if let Ok(sample) = serde_json::from_value::<Sample>(v["tile"].clone()) {
        let land = std::sync::Arc::downgrade(&slot);
        let watch = land.clone();
        cache::with_still(std::sync::Arc::new(move || watch.strong_count() != 0), || {
            cache::fetch_tile_sample(
                &url,
                Box::new(move |r| {
                    if let Some(land) = land.upgrade() {
                        let outcome = sample.check(r).map(|outcome| TestAnswer::Tile(sample, outcome));
                        *land.lock().unwrap_or_else(|e| e.into_inner()) = Some(outcome);
                    }
                }),
            );
        });
    } else {
        let land = slot.clone();
        cache::download(&url, Box::new(move |r| *land.lock().unwrap_or_else(|e| e.into_inner()) = Some(r.map(TestAnswer::Document))));
    }
    app.ui.connection.test = Some((q, slot, context));
    app.dialogs.info = Some(status.into());
}

/// A request remains authorized only for its original project and exact saved definitions.
fn candidates(app: &RustGisApp, request: &LoginRequest) -> Vec<Connection> {
    if request.opened != app.session.opened {
        return Vec::new();
    }
    request
        .connections
        .iter()
        .filter_map(|id| {
            let c = app.session.connections.find(id)?;
            if request.kind == WEB {
                return (web::is_service(c) && login_origin(c).as_deref() == Some(&request.key)).then(|| c.clone());
            }
            let (kind, key) = database_key(&c.address)?;
            (kind == request.kind && key == request.key).then(|| c.clone())
        })
        .collect()
}

fn password_request(app: &RustGisApp, id: &str) -> Option<LoginRequest> {
    app.session
        .database_logins
        .requests(app.session.opened)
        .find(|r| r.reason == LoginReason::Password && r.connections.iter().any(|c| c == id))
        .cloned()
}

fn password_prompt(app: &mut RustGisApp, id: &str, request: Option<LoginRequest>, error: Option<String>) {
    match open_edit(app, id) {
        Ok(()) => {
            app.ui.connection.request = request;
            if WEB_KINDS.contains(&app.ui.connection.kind.as_str()) {
                app.ui.connection.login = true;
                app.dialogs.info = Some(if app.ui.connection.auth == "arcgis" {
                    tl!("Paste a token for this connection, or make one from its user and password. Remember on this computer is optional.")
                        .into()
                } else {
                    tl!("Enter the password or key for this connection. Remember on this computer is optional.").into()
                });
                app.dialogs.error = error.map(|e| translated(&e));
                return;
            }
            app.dialogs.info = Some(tl!("The saved password could not be used. Type a password to try again; check Remember on this computer to replace the saved password.").into());
            app.dialogs.error = error;
        }
        Err(e) => app.session.notify(e, true),
    }
}

pub(crate) fn choose(app: &mut RustGisApp, request: LoginRequest) {
    if candidates(app, &request).is_empty() {
        return;
    }
    replace_form(app, State { request: Some(request), ..Default::default() });
    app.dialogs = crate::dialogs::Dialogs { open: Some("chooseConnection"), ..Default::default() };
}

pub(crate) fn chooser(app: &mut RustGisApp, ui: &mut egui::Ui) -> bool {
    let Some(request) = app.ui.connection.request.clone() else { return true };
    if request.kind == WEB && request.reason == LoginReason::Connection {
        ui.label(i18n::fmt("This project uses a connection for {origin} that is not saved on this computer.", &[("origin", &request.key)]));
        return ui
            .horizontal(|ui| {
                if ui.button(tl!("New Connection…")).clicked() {
                    match open_new(app, "service") {
                        Ok(()) => {
                            app.ui.connection.address = request.key.clone();
                            // Saving it logs in, so the layers waiting for a connection take it.
                            app.ui.connection.request = Some(request.clone());
                        }
                        Err(e) => app.dialogs.error = Some(e),
                    }
                    return false;
                }
                ui.button(tl!("Cancel")).clicked()
            })
            .inner;
    }
    let list = candidates(app, &request);
    if list.is_empty() {
        return true;
    }
    if request.kind == WEB {
        ui.label(tl!("Choose which saved connection to use for this address. This choice lasts for this session."));
        ui.label(&request.key);
    } else {
        ui.label(tl!("Choose which saved connection to use for this database. This choice lasts for this session."));
        ui.label(database_label(request.kind, &request.key));
    }
    egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
        for c in &list {
            ui.group(|ui| {
                ui.selectable_value(&mut app.ui.connection.selected, Some(c.id.clone()), &c.name);
                if !c.group.is_empty() {
                    ui.label(egui::RichText::new(&c.group).weak());
                }
                ui.label(egui::RichText::new(&c.address).small().weak());
            });
        }
    });
    let selected = app.ui.connection.selected.clone().filter(|id| list.iter().any(|c| &c.id == id));
    ui.horizontal(|ui| {
        if ui.add_enabled(selected.is_some(), egui::Button::new(tl!("Use Connection"))).clicked()
            && let Some(id) = selected
        {
            let result = app.run("connection.login", json!({"id":id}));
            crate::connectiontree::refresh(app, Some(&id));
            let refused = password_request(app, &id);
            if result.is_err() || refused.is_some() {
                password_prompt(app, &id, refused.or(Some(request)), result.err());
                return false;
            }
            return true;
        }
        ui.button(tl!("Cancel")).clicked()
    })
    .inner
}

fn poll_requests(app: &mut RustGisApp) {
    if let Some(request) = &app.ui.connection.request
        && matches!(app.dialogs.open, Some("chooseConnection" | "editConnection"))
        && (request.opened != app.session.opened || request.reason != LoginReason::Connection && candidates(app, request).is_empty())
    {
        app.dialogs.open = None;
        replace_form(app, State::default());
    }
    // A pending request never displaces an unrelated dialog, including a form being edited.
    if app.dialogs.open.is_some() {
        return;
    }
    while let Some(request) = app.session.database_logins.next_unprompted(app.session.opened) {
        if app.ui.connection.request.as_ref().is_some_and(|shown| {
            shown.opened == request.opened
                && shown.kind == request.kind
                && shown.key == request.key
                && shown.reason == request.reason
                && shown.connections == request.connections
        }) {
            // A layer-menu prompt may have shown this request before automatic polling reached
            // it. In particular, cancelling it must not immediately open it again.
            continue;
        }
        if request.kind == WEB && request.reason == LoginReason::Connection {
            replace_form(app, State { request: Some(request), ..Default::default() });
            app.dialogs = crate::dialogs::Dialogs { open: Some("chooseConnection"), ..Default::default() };
            break;
        }
        let list = candidates(app, &request);
        if list.is_empty() {
            continue;
        }
        if request.reason == LoginReason::Choose || list.len() > 1 {
            choose(app, request);
        } else {
            password_prompt(app, &list[0].id, Some(request), None);
        }
        break;
    }
}

pub fn poll(app: &mut RustGisApp, ctx: &egui::Context) {
    poll_requests(app);
    poll_generating(app, ctx);
    if !matches!(app.dialogs.open, Some("newConnection" | "editConnection")) {
        if let Some((_, _, context)) = app.ui.connection.test.take() {
            if app.dialogs.open.is_none() {
                app.dialogs.info = None;
                app.dialogs.error = None;
            } else if app.dialogs.info.as_deref() == Some(context.status) {
                app.dialogs.info = None;
            }
        }
        return;
    }
    let stale = app
        .ui
        .connection
        .test
        .as_ref()
        .is_some_and(|(_, _, context)| context.opened != app.session.opened || context.form != definition(&app.ui.connection));
    if stale {
        app.ui.connection.test = None;
        app.dialogs.info = None;
        app.dialogs.error = None;
        return;
    }
    let Some((_, slot, _)) = app.ui.connection.test.as_mut() else { return };
    let result = slot.lock().unwrap_or_else(|e| e.into_inner()).take();
    let Some(result) = result else {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        return;
    };
    // Hold the temporary login while the answer is parsed too: response/error redaction still
    // needs it, and no saved login may be substituted during this anonymous command.
    let Some((mut q, _, _context)) = app.ui.connection.test.take() else { return };
    app.dialogs.info = None;
    match result {
        Ok(TestAnswer::Document(bytes)) => {
            q["answer"] = json!(String::from_utf8_lossy(&bytes));
            let kind = q["kind"].as_str().unwrap_or("").to_owned();
            match app.run("connection.test", q) {
                Ok(v) => app.dialogs.info = Some(rustgis_engine::render::rustgis_tiles::logins::scrub(&summary(&kind, &v["result"]))),
                Err(e) => app.dialogs.error = Some(rustgis_engine::render::rustgis_tiles::logins::scrub(&e)),
            }
        }
        Ok(TestAnswer::Tile(sample, outcome)) => {
            let zoom = sample.zoom.to_string();
            match outcome {
                Outcome::Received => app.dialogs.info = Some(i18n::fmt("Received tile {z}/0/0.", &[("z", &zoom)])),
                Outcome::Missing => {
                    app.dialogs.error = Some(i18n::fmt(
                        "Server reachable, but no tile at {z}/0/0. Check the address and the layer's zoom range.",
                        &[("z", &zoom)],
                    ));
                }
            }
        }
        Err(e) => app.dialogs.error = Some(logins::scrub(&translated(&e))),
    }
}

/// How many layers a service's listing has: all of them when the list stops short (`total`),
/// with a `+` when the document has more than were read.
pub(crate) fn layer_count(r: &Value) -> String {
    let count = r["total"].as_u64().unwrap_or(r["layers"].as_array().map_or(0, Vec::len) as u64);
    if r["atLeast"] == true { format!("{count}+") } else { count.to_string() }
}

fn summary(kind: &str, r: &Value) -> String {
    let count = r["count"].as_u64().unwrap_or(0).to_string();
    match kind {
        "postgres" | "mysql" | "sqlserver" => {
            let tables = i18n::fmt("{n} tables with geometry", &[("n", &count)]);
            let geodatabase = r.get("geodatabase").filter(|g| g.is_object());
            let mut said = match geodatabase {
                Some(g) => match g.get("release").and_then(Value::as_str).filter(|v| !v.is_empty()) {
                    Some(release) => i18n::fmt("Enterprise geodatabase {release}, {tables}", &[("release", release), ("tables", &tables)]),
                    None => i18n::fmt("Enterprise geodatabase, {tables}", &[("tables", &tables)]),
                },
                None => tables,
            };
            // What the listing says beside its rows: a catalog cut short, or one it couldn't read.
            if let Some(note) = geodatabase.and_then(|g| g.get("note")).and_then(Value::as_str) {
                said.push_str(" · ");
                said.push_str(note);
            }
            if let Some(why) = r.get("geodatabaseProblem").and_then(Value::as_str) {
                said.push_str(" · ");
                said.push_str(&i18n::fmt("the geodatabase catalog couldn't be read: {why}", &[("why", why)]));
            }
            said
        }
        "csw" => {
            let n = r["matched"].as_u64().map(|n| n.to_string()).unwrap_or(count);
            i18n::fmt("{n} records in the catalogue", &[("n", &n)])
        }
        "stac" => i18n::fmt("{n} collections", &[("n", &count)]),
        "s3" => {
            if let Some(bucket) = r["bucket"].as_str() {
                let n = if r["truncated"] == true { format!("{count}+") } else { count };
                i18n::fmt("{n} items in {bucket}", &[("n", &n), ("bucket", bucket)])
            } else {
                i18n::fmt("{n} buckets", &[("n", &r["buckets"].as_array().map_or(0, Vec::len).to_string())])
            }
        }
        _ => {
            let kind = r["kind"].as_str().unwrap_or("");
            let title = r["title"].as_str().unwrap_or("");
            let count = layer_count(r);
            if r["layers"].as_array().is_none_or(Vec::is_empty) && r["next"].is_string() {
                i18n::fmt("Connected to {kind} {title}", &[("kind", kind), ("title", title)])
            } else {
                i18n::fmt("{kind} {title}: {count} layers", &[("kind", kind), ("title", title), ("count", &count)])
            }
        }
    }
}

fn insecure(st: &State) -> bool {
    !st.auth.is_empty() && st.address.trim().get(..7).is_some_and(|s| s.eq_ignore_ascii_case("http://"))
}

/// Whether a password or any key is typed in the form.
fn typed_secrets(st: &State) -> bool {
    (DATABASE_KINDS.contains(&st.kind.as_str()) && !st.password.is_empty())
        || (WEB_KINDS.contains(&st.kind.as_str())
            && match st.auth.as_str() {
                "basic" => !st.password.is_empty(),
                "header" | "query" => !st.api_key.is_empty(),
                "arcgis" => {
                    if st.token_generate {
                        !st.password.is_empty()
                    } else {
                        !st.token.trim().is_empty()
                    }
                }
                _ => false,
            })
        || (st.kind == "s3"
            && st.credentials == "keys"
            && [&st.access_key, &st.secret_key, &st.session_token].iter().any(|v| !v.is_empty()))
}

fn clear_secrets(st: &mut State) {
    st.password.clear();
    st.api_key.clear();
    st.token.clear();
    st.token_expires.clear();
    st.allow_insecure = false;
    st.access_key.clear();
    st.secret_key.clear();
    st.session_token.clear();
    st.remember = false;
}

/// Saves the definition, then logs in when a secret is typed, or the form was opened to log in
/// (using this connection's session or saved credentials). Secrets are remembered only when
/// asked. A login that worked with a warning keeps the dialog open to say so.
fn save(app: &mut RustGisApp) -> Result<(), String> {
    let store = app.session.database_logins.has_store();
    let st = &app.ui.connection;
    if let Some(e) = form_error(st) {
        return Err(e);
    }
    let service = WEB_KINDS.contains(&st.kind.as_str());
    if service && typed_secrets(st) {
        let (connection, _) = rustgis_engine::connections::clean(Connection {
            kind: st.kind.clone(),
            address: st.address.clone(),
            auth: st.auth.clone(),
            user: st.user.clone(),
            key_name: st.key_name.clone(),
            ..Default::default()
        })?;
        if generating_form(st) {
            check_generation(st)?;
        } else {
            let secret = login_secret(st)?;
            web::check_login_secret(&connection, &secret).map_err(|e| translated(&e))?;
        }
    } else if service && st.auth == "arcgis" {
        // An expiry typed with no token is a mistake to say, not to drop.
        pasted_token(st)?;
    }
    if st.generating.is_some() {
        return Err(tl!("A token is being generated. Cancel it before saving again.").into());
    }
    if service && typed_secrets(st) && insecure(st) && !st.allow_insecure && !generating_form(st) {
        return Err(translated(web::INSECURE));
    }
    if service && st.remember && !typed_secrets(st) {
        return Err(if st.auth == "arcgis" {
            tl!("Type the token, or the password to generate one, to remember it on this computer.").into()
        } else {
            tl!("Type the password or key to remember it on this computer.").into()
        });
    }
    let keys_mode = st.kind == "s3" && st.credentials == "keys";
    let remember = keys_mode && st.remember && remember_blocked(store, &st.session_token).is_none();
    if remember && (st.access_key.trim().is_empty() || st.secret_key.trim().is_empty()) {
        return Err(tl!("Type the access key ID and the secret access key to remember them on this computer.").into());
    }
    if DATABASE_KINDS.contains(&st.kind.as_str()) && st.remember && st.password.is_empty() {
        return Err(tl!("Type the password to remember it on this computer.").into());
    }
    let adopts = service && st.request.as_ref().is_some_and(|r| r.kind == WEB && r.reason == LoginReason::Connection);
    let wants_login = typed_secrets(st) || (st.login && (keys_mode || service) && st.id.is_some()) || adopts;
    let mut p = definition(st);
    let command = if let Some(id) = &st.id {
        p["id"] = json!(id);
        if st.kind != "s3"
            && let Some(object) = p.as_object_mut()
        {
            object.remove("options");
        }
        "connection.edit"
    } else {
        // The geodatabase form names a connection after its server; an address is named by the
        // engine.
        if st.name.trim().is_empty()
            && !(st.form && st.kind == "sqlserver")
            && let Some(object) = p.as_object_mut()
        {
            object.remove("name");
        }
        "connection.new"
    };
    let v = app.run(command, p)?;
    let id = v["id"].as_str().map(str::to_owned).ok_or_else(|| "the connection has no id".to_string())?;
    // Kept at once: a Save tried again edits this connection rather than adding another.
    let created = app.ui.connection.id.is_none();
    app.ui.connection.id = Some(id.clone());
    // A token made from a password is asked of the server in the background: the password leaves
    // the form with the request, and the dialog stays to say how it is going (see `poll_generating`).
    if generating_form(&app.ui.connection) && !app.ui.connection.password.is_empty() {
        let remember = store && app.ui.connection.remember;
        crate::connectiontree::refresh(app, Some(&id));
        return start_generating(app, &id, remember, false).inspect_err(|_| {
            if created {
                app.dialogs.open = Some("editConnection");
            }
        });
    }
    if !wants_login {
        clear_secrets(&mut app.ui.connection);
        crate::connectiontree::refresh(app, Some(&id));
        return Ok(());
    }
    let mut login = json!({"id": id});
    secrets(&app.ui.connection, &mut login)?;
    let database = DATABASE_KINDS.contains(&app.ui.connection.kind.as_str());
    if database || service && store {
        login["remember"] = json!(app.ui.connection.remember);
    } else if remember {
        login["remember"] = json!(true);
    }
    let result = app.run("connection.login", login);
    if service {
        // A web login is not kept in the form once sent, whether it worked or not.
        clear_secrets(&mut app.ui.connection);
        crate::connectiontree::refresh(app, Some(&id));
    }
    if database {
        crate::connectiontree::refresh(app, Some(&id));
        let refused = password_request(app, &id);
        if result.is_err() || refused.is_some() {
            app.ui.connection.password.clear();
            app.ui.connection.remember = false;
            app.ui.connection.request = refused;
            app.dialogs.open = Some("editConnection");
            return Err(result.err().unwrap_or_else(|| tl!("The password was refused. Type a password to try again.").into()));
        }
    }
    let v = match result {
        Ok(v) => v,
        Err(e) => {
            if created {
                app.dialogs.open = Some("editConnection");
            }
            return Err(translated(&e));
        }
    };
    clear_secrets(&mut app.ui.connection);
    app.ui.connection.login = false;
    if let Some(warning) = v["warning"].as_str() {
        if database {
            // Database login retains its established notice-and-close behavior.
            app.session.notify(warning.to_string(), true);
        } else {
            app.ui.connection.warned = Some(translated(warning));
            app.dialogs.open = Some("editConnection");
        }
    }
    resolve_waiting(app, &id);
    Ok(())
}

/// Asks for a token for the saved connection `id` from the typed password, which leaves the form
/// with the request (a failure doesn't keep it either). The form then waits for the answer.
fn start_generating(app: &mut RustGisApp, id: &str, remember: bool, then_test: bool) -> Result<(), String> {
    let st = &mut app.ui.connection;
    let params = json!({"id": id, "tokenEndpoint": st.token_endpoint.trim(), "password": st.password, "remember": remember});
    clear_secrets(st);
    app.dialogs.error = None;
    app.run("connection.generateToken", params).map_err(|e| translated(&e))?;
    app.ui.connection.generating = Some(Generating { id: id.to_string(), then_test });
    app.dialogs.info = None;
    Ok(())
}

/// Stops the token this form is generating, if any; the login the connection had stays.
fn cancel_generating(app: &mut RustGisApp) {
    if let Some(generating) = app.ui.connection.generating.take() {
        // The connection may be gone already, with its generation: nothing is left to stop.
        let _ = app.run("connection.cancelToken", json!({"id": generating.id}));
    }
}

/// Replaces the form, first ending a token generation it started.
fn replace_form(app: &mut RustGisApp, next: State) {
    cancel_generating(app);
    app.ui.connection = next;
}

/// Asks how the generation of this form is getting on, once a frame, until it lands. Closing the
/// form, or changing to another, cancels it.
fn poll_generating(app: &mut RustGisApp, ctx: &egui::Context) {
    let Some(id) = app.ui.connection.generating.as_ref().map(|g| g.id.clone()) else { return };
    let open = matches!(app.dialogs.open, Some("newConnection" | "editConnection")) && app.ui.connection.id.as_deref() == Some(id.as_str());
    if !open {
        cancel_generating(app);
        return;
    }
    match app.run("connection.tokenStatus", json!({"id": id})) {
        Ok(v) if v["pending"] == true => ctx.request_repaint_after(std::time::Duration::from_millis(100)),
        Ok(v) => token_landed(app, &id, &v),
        Err(e) => {
            app.ui.connection.generating = None;
            app.dialogs.info = None;
            app.dialogs.error = Some(translated(&e));
        }
    }
}

/// A generated token is the connection's login: refresh what shows it, say what it couldn't do,
/// log the waiting layers in and either run the Test that asked for it or close the form.
fn token_landed(app: &mut RustGisApp, id: &str, v: &Value) {
    let then_test = app.ui.connection.generating.take().is_some_and(|g| g.then_test);
    app.dialogs.info = None;
    crate::connectiontree::refresh(app, Some(id));
    app.ui.connection.login = false;
    let warning = v["warning"].as_str().map(translated);
    resolve_waiting(app, id);
    if let Some(warning) = warning {
        app.ui.connection.warned = Some(warning);
    } else if then_test {
        start_test(app);
    } else {
        app.dialogs.open = None;
        replace_form(app, State::default());
    }
}

pub fn dialog(app: &mut RustGisApp, ui: &mut egui::Ui) -> bool {
    let intro = if DATABASE_KINDS.contains(&app.ui.connection.kind.as_str()) {
        tl!(
            "A named endpoint you can reuse. Passwords last for this session unless you choose to remember them in this computer's password store."
        )
    } else if app.ui.connection.kind == "s3" {
        tl!(
            "A named endpoint you can reuse. Keys are kept until RustGIS closes unless you choose to remember them on this computer; the connection itself never keeps them."
        )
    } else {
        tl!(
            "A named endpoint you can reuse. Passwords and keys last for this session unless you choose to remember them on this computer; the connection itself never keeps them."
        )
    };
    ui.label(egui::RichText::new(intro).weak());
    let store = app.session.database_logins.has_store();
    // Status metadata only: rendering must never read the credential store.
    let saved_keys =
        app.ui.connection.id.as_deref().and_then(|id| app.session.connections.find(id)).is_some_and(|c| c.kind == "s3" && c.saved_keys);
    let current = app.ui.connection.id.as_deref().and_then(|id| app.session.connections.find(id));
    let remembered = current.is_some_and(|c| web::is_service(c) && c.remembered);
    let logged_in = current.is_some_and(|c| web::logged_in(&app.session, c));
    let generating = app.ui.connection.generating.is_some();
    let st = &mut app.ui.connection;
    let database = DATABASE_KINDS.contains(&st.kind.as_str());
    let service = WEB_KINDS.contains(&st.kind.as_str());
    let mut changed = false;
    let mut source_changed = false;
    let mut forget = false;
    let mut logout = false;
    let form_mode = st.form && st.kind == "sqlserver";
    let mut toggle_form = false;
    let mut from_sde = false;
    ui.add_enabled_ui(!generating, |ui| {
    egui::Grid::new("connection").num_columns(2).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
        if form_mode {
            changed |= form_rows(ui, st);
            return;
        }
        ui.label(tl!("Type"));
        ui.label(tl!(label(&st.kind)));
        ui.end_row();
        let input_label = ui.label(tl!("Name"));
        let edited = ui
            .add(egui::TextEdit::singleline(&mut st.name).hint_text(tl!("From the address")).desired_width(360.0))
            .labelled_by(input_label.id)
            .changed();
        st.typed_name |= edited;
        changed |= edited;
        ui.end_row();
        let input_label = ui.label(tl!("Group"));
        changed |= ui
            .add(egui::TextEdit::singleline(&mut st.group).hint_text(tl!("None")).desired_width(360.0))
            .labelled_by(input_label.id)
            .changed();
        ui.end_row();
        if database {
            let input_label = ui.label(tl!("Connection"));
            let hint = match st.kind.as_str() {
                "mysql" => "mysql://user@host:3306/db",
                "sqlserver" => "mssql://user@host:1433/db",
                _ => "host=… dbname=… user=…",
            };
            changed |= ui
                .add(egui::TextEdit::singleline(&mut st.address).hint_text(hint).desired_width(360.0))
                .labelled_by(input_label.id)
                .changed();
            ui.end_row();
            // An enterprise geodatabase is entered as ArcGIS Pro asks for it.
            if st.kind == "sqlserver" && !cfg!(target_arch = "wasm32") {
                ui.label("");
                toggle_form = ui.link(tl!("Use the form instead")).clicked();
                ui.end_row();
            }
            ui.label(tl!("Password"));
            changed |= ui
                .add(egui::TextEdit::singleline(&mut st.password).password(true).hint_text(tl!("Optional")).desired_width(360.0))
                .changed();
            ui.end_row();
            ui.label("");
            changed |= ui.checkbox(&mut st.remember, tl!("Remember on this computer")).changed();
            ui.end_row();
        } else if st.kind == "s3" {
            for (label, field, hint) in [
                (tl!("Endpoint"), &mut st.address, tl!("Blank for Amazon S3")),
                (tl!("Region"), &mut st.region, s3::DEFAULT_REGION),
                (tl!("Buckets"), &mut st.buckets, tl!("Comma-separated (optional)")),
            ] {
                let input_label = ui.label(label);
                changed |=
                    ui.add(egui::TextEdit::singleline(field).hint_text(hint).desired_width(360.0)).labelled_by(input_label.id).changed();
                ui.end_row();
            }
            let input_label = ui.label(tl!("Keys from"));
            let shown = crate::s3ui::SOURCES.iter().find(|(c, _)| *c == st.credentials).map(|(_, l)| tl!(*l)).unwrap_or_default();
            egui::ComboBox::from_id_salt("connection_credentials")
                .selected_text(shown)
                .width(360.0)
                .show_ui(ui, |ui| {
                    for (code, label) in crate::s3ui::SOURCES {
                        if cfg!(target_arch = "wasm32") && matches!(code, "profile" | "environment") {
                            continue;
                        }
                        source_changed |= ui.selectable_value(&mut st.credentials, code.to_owned(), tl!(label)).changed();
                        changed |= source_changed;
                    }
                })
                .response
                .labelled_by(input_label.id);
            ui.end_row();
            if st.credentials == "profile" {
                let input_label = ui.label(tl!("Profile"));
                changed |= ui.add(egui::TextEdit::singleline(&mut st.profile).desired_width(360.0)).labelled_by(input_label.id).changed();
                ui.end_row();
            }
            if st.credentials == "keys" {
                for (label, field, secret) in [
                    (tl!("Access key ID"), &mut st.access_key, false),
                    (tl!("Secret access key"), &mut st.secret_key, true),
                    (tl!("Session token"), &mut st.session_token, true),
                ] {
                    let input_label = ui.label(label);
                    changed |= ui
                        .add(egui::TextEdit::singleline(field).password(secret).desired_width(360.0))
                        .labelled_by(input_label.id)
                        .changed();
                    ui.end_row();
                }
                ui.label("");
                let blocked = remember_blocked(store, &st.session_token);
                ui.vertical(|ui| changed |= remember_checkbox(ui, &mut st.remember, blocked));
                ui.end_row();
                if saved_keys {
                    ui.label(tl!("Saved keys"));
                    ui.vertical(|ui| {
                        let note = if store {
                            tl!("This computer keeps keys for this connection: leave the keys blank to log in with them.")
                        } else {
                            tl!("Keys were saved for this connection, but there is no credential store here to read them from.")
                        };
                        ui.label(egui::RichText::new(note).small().weak());
                        forget = ui.button(tl!("Forget Saved Keys")).clicked();
                    });
                    ui.end_row();
                }
            }
        } else {
            let input_label = ui.label(tl!("Address"));
            let hint = match st.kind.as_str() {
                "stac" => "https://…/stac/v1",
                "arcgis" => "https://…/rest/services",
                "csw" => "https://…/csw",
                _ => "https://…",
            };
            changed |= ui
                .add(egui::TextEdit::singleline(&mut st.address).hint_text(hint).desired_width(360.0))
                .labelled_by(input_label.id)
                .changed();
            ui.end_row();
            if service {
                if tile_test::is_template(&st.address) && tile_test::has_path_key(&st.address) {
                    ui.label("");
                    ui.label(egui::RichText::new(tl!("An API key in the URL path is saved as typed, not in the credential store.")).small());
                    ui.end_row();
                }
                ui.label(egui::RichText::new(tl!("Login")).strong());
                ui.label("");
                ui.end_row();
                let input_label = ui.label(tl!("Method"));
                let methods = [
                    ("", tl!("None")),
                    ("basic", tl!("Basic authentication")),
                    ("header", tl!("API key in a header")),
                    ("query", tl!("API key in the query")),
                    ("arcgis", tl!("ArcGIS token")),
                ];
                let shown = methods.iter().find(|(code, _)| *code == st.auth).map_or(tl!("None"), |(_, name)| *name);
                let mut method_changed = false;
                egui::ComboBox::from_id_salt("connection_auth")
                    .selected_text(shown)
                    .width(360.0)
                    .show_ui(ui, |ui| {
                        for (code, name) in methods {
                            method_changed |= ui.selectable_value(&mut st.auth, code.to_string(), name).changed();
                        }
                    })
                    .response
                    .labelled_by(input_label.id);
                ui.end_row();
                if method_changed {
                    clear_secrets(st);
                    changed = true;
                }
                if matches!(st.auth.as_str(), "basic" | "arcgis") {
                    let input_label = ui.label(tl!("User"));
                    let mut user = egui::TextEdit::singleline(&mut st.user).desired_width(360.0);
                    if st.auth == "arcgis" {
                        user = user.hint_text(tl!("Needed to make a token from a password"));
                    }
                    changed |= ui.add(user).labelled_by(input_label.id).changed();
                    ui.end_row();
                } else if matches!(st.auth.as_str(), "header" | "query") {
                    let query = st.auth == "query";
                    let input_label = ui.label(if query { tl!("Parameter name") } else { tl!("Header name") });
                    changed |= ui
                        .add(
                            egui::TextEdit::singleline(&mut st.key_name)
                                .hint_text(if query { "api_key" } else { "X-API-Key" })
                                .desired_width(360.0),
                        )
                        .labelled_by(input_label.id)
                        .changed();
                    ui.end_row();
                }
                if !st.auth.is_empty() && st.auth == "arcgis" {
                    let input_label = ui.label(tl!("Token from"));
                    let mut mode_changed = false;
                    ui.horizontal(|ui| {
                        mode_changed |= ui.radio_value(&mut st.token_generate, false, tl!("Paste token")).changed();
                        mode_changed |= ui.radio_value(&mut st.token_generate, true, tl!("Username and password")).changed();
                    })
                    .response
                    .labelled_by(input_label.id);
                    ui.end_row();
                    if mode_changed {
                        clear_secrets(st);
                        changed = true;
                    }
                    if st.token_generate {
                        let input_label = ui.label(tl!("Token endpoint"));
                        changed |= ui
                            .add(
                                egui::TextEdit::singleline(&mut st.token_endpoint)
                                    .hint_text("https://host/arcgis/tokens/generateToken")
                                    .desired_width(360.0),
                            )
                            .labelled_by(input_label.id)
                            .changed();
                        ui.end_row();
                        let input_label = ui.label(tl!("Password"));
                        changed |= ui
                            .add(egui::TextEdit::singleline(&mut st.password).password(true).desired_width(360.0))
                            .labelled_by(input_label.id)
                            .changed();
                        ui.end_row();
                        ui.label("");
                        ui.label(
                            egui::RichText::new(tl!(
                                "Asks this server for a token, over https:// only, and keeps the token and its expiry, never the password."
                            ))
                            .small()
                            .weak(),
                        );
                        ui.end_row();
                    } else {
                        let input_label = ui.label(tl!("Token"));
                        changed |= ui
                            .add(egui::TextEdit::singleline(&mut st.token).password(true).desired_width(360.0))
                            .labelled_by(input_label.id)
                            .changed();
                        ui.end_row();
                        let input_label = ui.label(tl!("Expires at (Unix milliseconds)"));
                        changed |= ui
                            .add(egui::TextEdit::singleline(&mut st.token_expires).hint_text(tl!("Optional")).desired_width(360.0))
                            .labelled_by(input_label.id)
                            .changed();
                        ui.end_row();
                        ui.label("");
                        ui.label(
                            egui::RichText::new(tl!(
                                "The token is added to each request to this address as its token parameter. With no expiry it is used until the server refuses it."
                            ))
                            .small()
                            .weak(),
                        );
                        ui.end_row();
                    }
                }
                if !st.auth.is_empty() {
                    if st.auth != "arcgis" {
                        let basic = st.auth == "basic";
                        let input_label = ui.label(if basic { tl!("Password") } else { tl!("Key") });
                        let field = if basic { &mut st.password } else { &mut st.api_key };
                        changed |= ui
                            .add(egui::TextEdit::singleline(field).password(true).desired_width(360.0))
                            .labelled_by(input_label.id)
                            .changed();
                        ui.end_row();
                        if st.auth == "query" {
                            ui.label("");
                            ui.label(
                                egui::RichText::new(tl!(
                                    "The key is added to each request to this address as this parameter. It is never saved in the connection or project."
                                ))
                                .small()
                                .weak(),
                            );
                            ui.end_row();
                        }
                    }
                    if store {
                        ui.label("");
                        changed |= ui.checkbox(&mut st.remember, tl!("Remember on this computer")).changed();
                        ui.end_row();
                    }
                    if insecure(st) && !generating_form(st) {
                        ui.label("");
                        changed |= ui.checkbox(&mut st.allow_insecure, tl!("Send the login over unencrypted http://")).changed();
                        ui.end_row();
                    } else {
                        st.allow_insecure = false;
                    }
                    if logged_in || remembered {
                        ui.label(tl!("Status"));
                        ui.vertical(|ui| {
                            if logged_in {
                                ui.label(tl!("Logged in"));
                                logout = ui.button(tl!("Log Out")).clicked();
                            }
                            if remembered {
                                ui.label(if store { tl!("Saved on this computer") } else { tl!("Saved login unavailable") });
                                forget = ui.add_enabled(store, egui::Button::new(tl!("Forget Saved Login"))).clicked();
                            }
                        });
                        ui.end_row();
                    }
                }
            }
        }
    });
    });
    if form_mode {
        ui.horizontal(|ui| {
            if !cfg!(target_arch = "wasm32")
                && ui.button(tl!("From .sde file…")).on_hover_text(tl!("Fill the form from an ArcGIS Pro connection file")).clicked()
            {
                from_sde = true;
            }
            toggle_form = ui.link(tl!("Enter an address instead")).clicked();
        });
    }
    changed |= toggle_form;
    // A credential-source switch never carries opt-in to the newly chosen source.
    if source_changed || !(database || service && !st.auth.is_empty() && store || st.kind == "s3" && st.credentials == "keys") {
        st.remember = false;
    }
    if changed {
        // A late download may finish, but its detached slot cannot report on the edited form.
        st.test = None;
        // Edited again: back to saving.
        st.warned = None;
        app.dialogs.info = None;
        app.dialogs.error = None;
    }
    if forget && let Some(id) = app.ui.connection.id.clone() {
        let result = if service {
            app.run("connection.forgetLogin", json!({"id":id})).map(|_| ()).map_err(|e| translated(&e))
        } else {
            forget_keys(app, &id)
        };
        match result {
            Ok(()) => {
                app.dialogs.error = None;
                app.dialogs.info = Some(if service {
                    tl!("The saved login was forgotten; a login in this session keeps working.").to_string()
                } else {
                    tl!("The keys saved on this computer were forgotten; a login in this session keeps working.").to_string()
                });
            }
            Err(e) => {
                app.dialogs.info = None;
                app.dialogs.error = Some(e);
            }
        }
    }
    if toggle_form {
        match swap_form(&mut app.ui.connection) {
            Ok(()) => app.dialogs.error = None,
            Err(e) => {
                app.dialogs.info = None;
                app.dialogs.error = Some(e);
            }
        }
    }
    if from_sde
        && let Some(path) = crate::dialogs::pick_open(&["sde"], tl!("ArcGIS Pro connection file"))
        && let Err(e) = fill_from_sde(app, &path)
    {
        app.dialogs.info = None;
        app.dialogs.error = Some(e);
    }
    if logout && let Some(id) = app.ui.connection.id.clone() {
        match app.run("connection.logout", json!({"id":id})) {
            Ok(_) => app.dialogs.info = Some(tl!("Logged out. Log in explicitly to use this connection again.").into()),
            Err(e) => app.dialogs.error = Some(translated(&e)),
        }
    }
    // A token being generated: said, with a way to stop it (closing the form stops it too).
    let mut stop = false;
    if app.ui.connection.generating.is_some() {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(tl!("Generating token…"));
            stop = ui.button(tl!("Cancel token request")).clicked();
        });
    }
    if stop {
        cancel_generating(app);
        app.dialogs.info = Some(tl!("The token request was cancelled.").into());
    }
    // A login that worked but couldn't do all it was asked: said until closed (or edited again).
    if let Some(warning) = app.ui.connection.warned.clone() {
        ui.add_space(4.0);
        ui.label(egui::RichText::new(tl!("Logged in for this session")).strong());
        ui.label(egui::RichText::new(warning).color(ui.visuals().warn_fg_color));
        return ui
            .with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                let accent = crate::theme::Tokens::get(ui.ctx()).accent;
                crate::widgets::dialog_button(ui, tl!("Close"), Some(accent), vec2(84.0, 28.0)).clicked()
            })
            .inner;
    }
    let st = &app.ui.connection;
    let testing = st.test.is_some() || st.generating.is_some();
    let unavailable = database && cfg!(target_arch = "wasm32");
    let blocked = test_blocked(app);
    let clicked = ui
        .horizontal(|ui| {
            let response = ui
                .add_enabled_ui(!testing && !unavailable && blocked.is_none(), |ui| {
                    crate::widgets::dialog_button(ui, tl!("Test"), None, vec2(84.0, 28.0))
                })
                .inner;
            let response = if unavailable {
                response.on_disabled_hover_text(tl!("Database connections are used by the desktop app"))
            } else if let Some(reason) = blocked {
                response.on_disabled_hover_text(reason)
            } else {
                response
            };
            if st.test.is_some() {
                ui.spinner();
            }
            response.clicked()
        })
        .inner;
    if clicked {
        start_test(app);
    }
    let was_generating = app.ui.connection.generating.is_some();
    let close = crate::dialogs::buttons_enabled(app, ui, "Save", !was_generating, save);
    // A Save that started a token request stays, to say how it goes (`poll_generating` lands it).
    let started = !was_generating && app.ui.connection.generating.is_some();
    // A warning to read keeps it open.
    let close = close && app.ui.connection.warned.is_none() && !started;
    if close && app.ui.connection.test.take().is_some() {
        app.dialogs.info = None;
        app.dialogs.error = None;
    }
    close
}

pub fn open_connection(app: &mut RustGisApp, c: &Connection) -> Result<(), String> {
    if web::is_service(c)
        && !c.auth.is_empty()
        && let Err(e) = web::open_connection(&mut app.session, c)
    {
        password_prompt(app, &c.id, password_request(app, &c.id), Some(e));
        return Ok(());
    }
    let (kind, extra) = match c.kind.as_str() {
        "postgres" | "mysql" | "sqlserver" => {
            if cfg!(target_arch = "wasm32") {
                return Err(tl!("Database connections are used by the desktop app").into());
            }
            if app.session.database_logins.has_session_password(&c.id) || c.remembered {
                let result = app.run("connection.login", json!({"id":c.id}));
                crate::connectiontree::refresh(app, Some(&c.id));
                let refused = password_request(app, &c.id);
                if result.is_err() || refused.is_some() {
                    password_prompt(app, &c.id, refused, result.err());
                    return Ok(());
                }
            }
            #[cfg(not(target_arch = "wasm32"))]
            if !app.session.database_logins.has_session_password(&c.id)
                && !c.remembered
                && let Some((_, key)) = database_key(&c.address)
            {
                // Picking an ID without credentials must not inherit another ID's activation.
                // An unowned, explicitly typed address password remains highest priority.
                rustgis_engine::data::source::duckdb::set_connection_password(&key, &c.id, None);
                app.session.database_logins.drop_activation(&key);
            }
            ("database", Value::Null)
        }
        "service" => ("service", json!({"type":"auto"})),
        "wms" | "wmts" | "wfs" | "wcs" | "ogcapi" | "arcgis" | "csw" => ("service", json!({"type":c.kind})),
        "stac" => ("stac", json!({"api":"true"})),
        "s3" => {
            if !app.session.has_s3_login(&c.id) {
                let keys = c.options.get("credentials").is_some_and(|v| v == "keys");
                // Keys typed for the session: the form, to type them. Keys saved on this computer
                // are read by this click's login (the system may ask to unlock its store).
                if keys && !c.saved_keys {
                    open_edit(app, &c.id)?;
                    app.ui.connection.login = true;
                    app.dialogs.info = Some(tl!("Type the keys to log in for this session, then Save.").into());
                    return Ok(());
                }
                if let Err(e) = app.run("connection.login", json!({"id":c.id})) {
                    if keys && needs_keys(&e) {
                        open_edit(app, &c.id)?;
                        app.ui.connection.login = true;
                        app.dialogs.error = Some(translated(&e));
                        app.dialogs.info = Some(tl!("Type the keys to log in for this session, then Save.").into());
                        return Ok(());
                    }
                    return Err(translated(&e));
                }
            }
            crate::s3ui::fill(app, c);
            let r = crate::sourcespane::open(
                app,
                &Source { kind: "s3".into(), name: c.name.clone(), address: c.name.clone(), extra: Value::Null },
            );
            if r.is_ok() {
                app.ui.s3.connection = Some(c.id.clone());
            }
            return r;
        }
        _ => return Err(format!("unknown kind '{}'", c.kind)),
    };
    if kind == "service" {
        return app.run("ui.addService", json!({"url":c.address, "type":extra["type"], "connection":c.id})).map(|_| ());
    }
    let r = crate::sourcespane::open(app, &Source { kind: kind.into(), name: c.name.clone(), address: c.address.clone(), extra });
    if r.is_ok() {
        if kind == "stac" {
            app.ui.stac.connection = Some(c.id.clone());
        } else {
            app.dialogs.connection = Some(c.id.clone());
        }
    }
    r
}

/// After a login from the form: the project's S3 sources that waited for that connection (the
/// chooser asked for its login) are given to it now.
fn resolve_waiting(app: &mut RustGisApp, id: &str) {
    let Some((source, waiting)) = app.ui.connection.resolving.take() else { return };
    if waiting == id
        && let Err(e) = choose_s3(app, &source, id)
    {
        app.session.notify(translated(&e), true);
    }
}

/// The user's choice of the saved connection `id` for the project's S3 sources waiting under
/// `source`: the engine logs it in if needed and opens them; nothing is read or signed here.
fn choose_s3(app: &mut RustGisApp, source: &str, id: &str) -> Result<(), String> {
    let v = app.run("connection.resolveS3", json!({"source": source, "id": id}))?;
    let name = app.session.connections.find(id).map(|c| c.name.clone()).unwrap_or_else(|| id.to_string());
    let opened = v["reopened"].as_u64().unwrap_or(0);
    let failed = v["failed"].as_u64().unwrap_or(0);
    // Rasters read in place whose overview is on its way: the connection reads them, but they
    // aren't in yet (the desktop doesn't wait; they land as the map draws).
    let reading = v["reading"].as_u64().unwrap_or(0);
    let n = opened.saturating_add(reading).to_string();
    let (failed_n, reading_n) = (failed.to_string(), reading.to_string());
    let args: &[(&str, &str)] = &[("name", &name), ("n", &n), ("failed", &failed_n), ("reading", &reading_n)];
    let text = match (failed, reading) {
        (0, 0) => i18n::fmt("{name} now reads {n} project sources", args),
        (0, _) => i18n::fmt("{name} now reads {n} project sources; {reading} still being read", args),
        (_, 0) => i18n::fmt("{name} now reads {n} project sources; {failed} could not be read", args),
        _ => i18n::fmt("{name} now reads {n} project sources; {reading} still being read and {failed} could not be read", args),
    };
    app.session.notify(text, failed > 0);
    Ok(())
}

/// Opens the chooser of the saved S3 connections that read the project's waiting S3 sources
/// (`ui.s3Connections`), with nothing picked.
pub fn open_chooser(app: &mut RustGisApp) -> Result<(), String> {
    if app.session.s3_pending.is_empty() {
        return Err(tl!("No project sources are waiting for an S3 connection.").into());
    }
    let ch = &mut app.ui.s3_chooser;
    ch.picked.clear();
    ch.error = None;
    ch.auto = false;
    app.dialogs = crate::dialogs::Dialogs { open: Some("s3Connections"), ..Default::default() };
    Ok(())
}

/// Once a frame: a project replacing the open one drops what was picked for the old one (and a
/// source waiting for a login), and the chooser opens by itself once for a project with S3
/// sources waiting for a choice (not again after Cancel).
pub fn follow_project(app: &mut RustGisApp) {
    let opened = app.session.opened;
    if app.ui.s3_chooser.project != Some(opened) {
        app.ui.s3_chooser = Chooser { project: Some(opened), auto: true, ..Default::default() };
        app.ui.connection.resolving = None;
        if app.dialogs.open == Some("s3Connections") {
            app.dialogs.open = None;
        }
    }
    if app.ui.s3_chooser.auto && app.dialogs.open.is_none() {
        app.ui.s3_chooser.auto = false;
        if !app.session.s3_pending.is_empty() {
            let _ = open_chooser(app);
        }
    }
}

/// Where a waiting resource's bucket is, as the chooser says it.
fn endpoint_label(endpoint: Option<&str>) -> String {
    match endpoint {
        None => tl!("unknown (an older project)").to_string(),
        Some("") => tl!("Amazon S3").to_string(),
        Some(e) => e.to_string(),
    }
}

/// The chooser (dialog `s3Connections`): each bucket the project's waiting sources read, and the
/// saved connections that could read it. True when it should close.
pub fn s3_chooser(app: &mut RustGisApp, ui: &mut egui::Ui) -> bool {
    let pending = app.session.s3_pending.clone();
    app.ui.s3_chooser.picked.retain(|source, _| pending.iter().any(|p| &p.source == source));
    ui.label(
        egui::RichText::new(tl!(
            "More than one saved connection could read these buckets. Choose the one that reads each; the project's sources that read it stay closed until you do."
        ))
        .weak(),
    );
    if pending.is_empty() {
        ui.label(tl!("No project sources are waiting for an S3 connection."));
    }
    for p in &pending {
        ui.group(|ui| {
            ui.label(egui::RichText::new(i18n::fmt(
                "Bucket {bucket} at {endpoint}",
                &[("bucket", &p.bucket), ("endpoint", &endpoint_label(p.endpoint.as_deref()))],
            )));
            for id in &p.candidates {
                let Some(c) = app.session.connections.connections.iter().find(|c| &c.id == id) else { continue };
                let name = if c.group.is_empty() { c.name.clone() } else { format!("{} › {}", c.group, c.name) };
                let at = if c.address.is_empty() { tl!("Amazon S3") } else { c.address.as_str() };
                let selected = app.ui.s3_chooser.picked.get(&p.source) == Some(id);
                if ui.radio(selected, format!("{name} — {at}")).clicked() {
                    app.ui.s3_chooser.picked.insert(p.source.clone(), id.clone());
                }
            }
        });
    }
    if let Some(e) = &app.ui.s3_chooser.error {
        ui.colored_label(ui.visuals().error_fg_color, e);
    }
    let mut close = false;
    let mut open = false;
    ui.add_space(4.0);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
        let accent = crate::theme::Tokens::get(ui.ctx()).accent;
        let any = !app.ui.s3_chooser.picked.is_empty();
        open = ui.add_enabled_ui(any, |ui| crate::widgets::dialog_button(ui, tl!("Open"), Some(accent), vec2(84.0, 28.0))).inner.clicked();
        // The sources stay closed.
        close = crate::widgets::dialog_button(ui, tl!("Cancel"), None, vec2(84.0, 28.0)).clicked();
    });
    if open {
        close = open_chosen(app);
    }
    close
}

/// Gives each resource picked to its connection. One whose keys must be typed opens that
/// connection's Log In, and the choice is made again once it has logged in. True when nothing
/// waits any more.
fn open_chosen(app: &mut RustGisApp) -> bool {
    app.ui.s3_chooser.error = None;
    let picks: Vec<(String, String)> = app.ui.s3_chooser.picked.iter().map(|(s, id)| (s.clone(), id.clone())).collect();
    for (source, id) in picks {
        match choose_s3(app, &source, &id) {
            Ok(()) => {
                app.ui.s3_chooser.picked.remove(&source);
            }
            Err(e) if needs_keys(&e) => {
                app.ui.s3_chooser.picked.remove(&source);
                if log_in(app, &id) {
                    app.ui.connection.resolving = Some((source, id));
                    app.dialogs.error = Some(translated(&e));
                }
                // Log In is open in its place.
                return false;
            }
            Err(e) => {
                app.ui.s3_chooser.error = Some(translated(&e));
                return false;
            }
        }
    }
    app.session.s3_pending.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A geodatabase form filled in by hand.
    fn gdb_form(sign_in: &str) -> State {
        State {
            kind: "sqlserver".into(),
            form: true,
            server: "gis-db\\GIS".into(),
            user: "sde".into(),
            database: "utility".into(),
            sign_in: sign_in.into(),
            ..Default::default()
        }
    }

    /// One of the data crate's synthetic `.sde` files (invented servers, no password).
    fn sde_fixture(name: &str) -> String {
        format!("{}/../data/testdata/sde/{name}", env!("CARGO_MANIFEST_DIR"))
    }

    #[test]
    fn the_geodatabase_form_composes_the_address() {
        let p = definition(&gdb_form("sql"));
        assert_eq!(p["address"], "mssql://sde@gis-db/utility?instance=GIS");
        assert_eq!(p["name"], "Geodatabase on gis-db\\GIS");
        // Windows sign-in names no user, whatever was typed in the User box.
        let windows = State { server: "gis-db".into(), ..gdb_form("windows") };
        assert_eq!(definition(&windows)["address"], "mssql://gis-db/utility");
        assert_eq!(definition(&gdb_form("windows"))["address"], "mssql://gis-db/utility?instance=GIS");
        // A name typed wins over the default.
        let named = State { name: "Utility GDB".into(), ..gdb_form("sql") };
        assert_eq!(definition(&named)["name"], "Utility GDB");
    }

    #[test]
    fn a_geodatabase_form_that_cannot_make_an_address_says_why() {
        let mut st = gdb_form("sql");
        st.server = "bad host!".into();
        assert!(form_error(&st).is_some());
        assert_eq!(definition(&st)["address"], "");
        st.server = "  ".into();
        assert!(form_error(&st).is_some());
        assert!(form_error(&gdb_form("sql")).is_none());
        // Database user with no user: no user is Windows sign-in, so it is refused, not composed.
        let no_user = State { user: "  ".into(), ..gdb_form("sql") };
        assert_eq!(form_error(&no_user).as_deref(), Some("Type the user name, or choose Windows sign-in"));
        assert_eq!(definition(&no_user)["address"], "");
        let windows = State { user: String::new(), ..gdb_form("windows") };
        assert!(form_error(&windows).is_none());
        // A password, token or key typed in Options belongs in the Password box, and an
        // instance in Server: the form says so rather than compose them.
        for typed in [
            "pwd=hunter2",
            "encrypt=strict&PASSWORD=x",
            "token=abc",
            "instance=OTHER",
            "encrypt=strict&Instance=X",
            "trust-server-certificate=true",
            "encrypt=strict&TrustServerCertificate=yes",
            "trust_server_certificate=maybe",
        ] {
            let refused = State { form_options: typed.into(), ..gdb_form("sql") };
            assert!(form_error(&refused).is_some(), "{typed}");
        }
        let fine = State { form_options: "encrypt=strict&connect-timeout=30".into(), ..gdb_form("sql") };
        assert!(form_error(&fine).is_none());
        // Address mode is never checked against the form.
        st.form = false;
        assert!(form_error(&st).is_none());
    }

    #[test]
    fn an_address_fills_the_geodatabase_form_back_in() {
        let f = |a: &str| parse_mssql(a).map(|p| (p.server, p.user, p.database, p.trust_cert));
        assert_eq!(f("mssql://sde@gis-db/utility?instance=GIS"), Some(("gis-db\\GIS".into(), Some("sde".into()), "utility".into(), false)));
        assert_eq!(f("mssql://gis-db:1444/util%20ity"), Some(("gis-db,1444".into(), None, "util ity".into(), false)));
        assert_eq!(f("mssql://gis-db:1433/u"), Some(("gis-db".into(), None, "u".into(), false)));
        // With an instance, a port given is the route and is kept, even 1433; the last
        // `instance` is the one, as the engine reads it.
        assert_eq!(f("mssql://gis-db:1433/u?instance=GIS"), Some((r"gis-db\GIS,1433".into(), None, "u".into(), false)));
        assert_eq!(f("mssql://gis-db/u?instance=OLD&instance=GIS"), Some((r"gis-db\GIS".into(), None, "u".into(), false)));
        let st = State { server: r"gis-db\GIS,1433".into(), ..gdb_form("windows") };
        let composed = form_address(&st).unwrap();
        assert_eq!(composed, "mssql://gis-db:1433/utility?instance=GIS");
        assert_eq!(parse_mssql(&composed).unwrap().server, r"gis-db\GIS,1433");
        assert_eq!(f("postgres:host=x"), None);
        // The database is read as the engine reads it: boundary slashes aren't part of it, an
        // encoded one is.
        assert_eq!(f("mssql://reader@gis-db/utility/"), Some(("gis-db".into(), Some("reader".into()), "utility".into(), false)));
        assert_eq!(f("mssql://gis-db/util%2Fity"), Some(("gis-db".into(), None, "util/ity".into(), false)));
        // The scheme is read without case, as the engine reads it.
        assert_eq!(f(" MSSQL://gis-db/utility "), Some(("gis-db".into(), None, "utility".into(), false)));
        // What the form composes comes back as it was typed.
        let composed = form_address(&gdb_form("sql")).unwrap();
        let back = parse_mssql(&composed).unwrap();
        assert_eq!((back.server.as_str(), back.user.as_deref(), back.database.as_str()), ("gis-db\\GIS", Some("sde"), "utility"));
    }

    #[test]
    fn trusting_the_certificate_goes_into_the_address_and_back() {
        let mut st = gdb_form("sql");
        assert!(!form_address(&st).unwrap().contains("trust"));
        st.trust_cert = true;
        st.server = "gis-db".into();
        let address = form_address(&st).unwrap();
        assert_eq!(address, "mssql://sde@gis-db/utility?trust-server-certificate=true");
        assert!(parse_mssql(&address).unwrap().trust_cert);
        // After an instance, the option joins the query.
        st.server = r"gis-db\GIS".into();
        let address = form_address(&st).unwrap();
        assert!(address.ends_with("?instance=GIS&trust-server-certificate=true"), "{address}");
        let back = parse_mssql(&address).unwrap();
        assert!(back.trust_cert);
        assert_eq!(back.server, r"gis-db\GIS");
        assert!(parse_mssql("mssql://h/d?TrustServerCertificate=yes").unwrap().trust_cert);
        assert!(!parse_mssql("mssql://h/d?trust-server-certificate=false").unwrap().trust_cert);
        // Every value the engine reads is read the same way; one it refuses stays in the options,
        // as written, for Test to say why, and isn't dropped.
        let read = |a: &str| parse_mssql(a).map(|m| (m.trust_cert, m.options)).unwrap();
        assert_eq!(read("mssql://h/d?trust-server-certificate=1"), (true, String::new()));
        assert_eq!(read("mssql://h/d?trust_server_certificate=0"), (false, String::new()));
        assert_eq!(read("mssql://h/d?trust-server-certificate=maybe"), (false, "trust-server-certificate=maybe".into()));
        assert_eq!(read("mssql://h/d?trust-server-certificate"), (false, "trust-server-certificate".into()));
    }

    #[test]
    fn a_geodatabase_test_names_its_release() {
        let r = json!({"count": 12, "layers": [], "geodatabase": {"owner": "sde", "release": "10.9"}});
        assert_eq!(summary("sqlserver", &r), "Enterprise geodatabase 10.9, 12 tables with geometry");
        let r = json!({"count": 3, "geodatabase": {"owner": "sde", "release": null}});
        assert_eq!(summary("sqlserver", &r), "Enterprise geodatabase, 3 tables with geometry");
        let r = json!({"count": 3, "geodatabaseProblem": "no access"});
        assert_eq!(summary("sqlserver", &r), "3 tables with geometry · the geodatabase catalog couldn't be read: no access");
        let r = json!({"count": 3, "geodatabase": {"owner": "sde", "release": "11.3.0", "note": "only the first 10000 are shown"}});
        assert_eq!(summary("sqlserver", &r), "Enterprise geodatabase 11.3.0, 3 tables with geometry · only the first 10000 are shown");
        assert_eq!(summary("sqlserver", &json!({"count": 3})), "3 tables with geometry");
    }

    #[test]
    fn a_sde_file_fills_the_form_and_saves_nothing_until_save() {
        let mut app = RustGisApp::with_sample();
        app.run("ui.connection", json!({"kind": "sqlserver", "form": true, "sdeFile": sde_fixture("sqlserver-os.sde")})).unwrap();
        let st = &app.ui.connection;
        assert!(st.form);
        assert_eq!(
            (st.server.as_str(), st.database.as_str(), st.user.as_str(), st.sign_in.as_str()),
            ("gisdb.example.invalid", "gis", "", "windows")
        );
        assert_eq!(st.name, "sqlserver-os");
        assert!(st.id.is_none());
        assert!(app.session.connections.connections.is_empty(), "reading the file saves nothing");
        assert!(app.dialogs.info.as_deref().is_some_and(|i| i.contains("Windows user")), "{:?}", app.dialogs.info);
        save(&mut app).unwrap();
        let list = &app.session.connections.connections;
        assert_eq!(list.len(), 1);
        assert_eq!((list[0].name.as_str(), list[0].address.as_str()), ("sqlserver-os", "mssql://gisdb.example.invalid/gis"));
        // A saved connection edited from a file keeps its name: it was typed when it was made.
        let id = list[0].id.clone();
        app.run("connection.edit", json!({"id": id, "name": "Utility GDB"})).unwrap();
        app.run("ui.connection", json!({"id": id})).unwrap();
        fill_from_sde(&mut app, &sde_fixture("sqlserver-dbauth-nosave.sde")).unwrap();
        assert_eq!(app.ui.connection.name, "Utility GDB");
    }

    #[test]
    fn a_sde_file_with_a_database_login_asks_for_the_user_and_password() {
        let mut app = RustGisApp::with_sample();
        app.run("ui.connection", json!({"kind": "sqlserver", "form": true, "sdeFile": sde_fixture("sqlserver-dbauth-nosave.sde")}))
            .unwrap();
        let st = &app.ui.connection;
        assert_eq!(st.sign_in, "sql");
        assert!(st.password.is_empty() && !st.remember);
        assert!(app.dialogs.info.as_deref().is_some_and(|i| i.contains("password")), "{:?}", app.dialogs.info);
    }

    #[test]
    fn a_second_sde_file_replaces_the_first_and_a_typed_name_stays() {
        let mut app = RustGisApp::with_sample();
        app.run("ui.connection", json!({"kind": "sqlserver", "form": true, "sdeFile": sde_fixture("sqlserver-dbauth-nosave.sde")}))
            .unwrap();
        fill_from_sde(&mut app, &sde_fixture("sqlserver-os.sde")).unwrap();
        assert_eq!((app.ui.connection.name.as_str(), app.ui.connection.sign_in.as_str()), ("sqlserver-os", "windows"));
        app.ui.connection.name = "Utility".into();
        app.ui.connection.typed_name = true;
        fill_from_sde(&mut app, &sde_fixture("sqlserver-dbauth-nosave.sde")).unwrap();
        assert_eq!(app.ui.connection.name, "Utility");
        assert!(app.session.connections.connections.is_empty());
        // The certificate choice and the options were for the server before: a file for
        // another server leaves them behind, one for the same server keeps them.
        let server = app.ui.connection.server.clone();
        app.ui.connection.trust_cert = true;
        app.ui.connection.form_options = "ca=C:/certs/ca.pem".into();
        fill_from_sde(&mut app, &sde_fixture("sqlserver-dbauth-nosave.sde")).unwrap();
        assert!(app.ui.connection.trust_cert);
        assert_eq!(app.ui.connection.form_options, "ca=C:/certs/ca.pem");
        assert!(!app.dialogs.info.as_deref().unwrap_or("").contains("previous server"));
        app.ui.connection.server = "elsewhere".into();
        fill_from_sde(&mut app, &sde_fixture("sqlserver-dbauth-nosave.sde")).unwrap();
        assert_eq!(app.ui.connection.server, server);
        assert!(!app.ui.connection.trust_cert);
        assert!(app.ui.connection.form_options.is_empty());
        // … and says so.
        assert!(app.dialogs.info.as_deref().is_some_and(|i| i.contains("previous server")), "{:?}", app.dialogs.info);
        // With no server typed there was no previous server: what was ticked or typed stays.
        app.ui.connection.server.clear();
        app.ui.connection.trust_cert = true;
        app.ui.connection.form_options = "encrypt=strict".into();
        fill_from_sde(&mut app, &sde_fixture("sqlserver-dbauth-nosave.sde")).unwrap();
        assert!(app.ui.connection.trust_cert);
        assert_eq!(app.ui.connection.form_options, "encrypt=strict");
        assert!(!app.dialogs.info.as_deref().unwrap_or("").contains("previous server"));
    }

    #[test]
    fn a_sde_file_that_is_refused_says_why_and_leaves_the_form_as_it_was() {
        let mut app = RustGisApp::with_sample();
        app.run("ui.connection", json!({"kind": "sqlserver", "form": true, "server": "typed-db"})).unwrap();
        // The form still opens, to enter the server by hand, with the reason in it.
        let e = fill_from_sde(&mut app, &sde_fixture("postgres-os.sde")).unwrap_err();
        assert!(e.contains("PostgreSQL"), "{e}");
        assert_eq!(app.ui.connection.server, "typed-db");
        assert!(app.session.connections.connections.is_empty());
        let mut app = RustGisApp::with_sample();
        app.run("ui.connection", json!({"kind": "sqlserver", "form": true, "sdeFile": sde_fixture("postgres-os.sde")})).unwrap();
        assert_eq!(app.dialogs.open, Some("newConnection"));
        assert!(app.dialogs.error.as_deref().is_some_and(|e| e.contains("PostgreSQL")), "{:?}", app.dialogs.error);
    }

    #[test]
    fn the_from_file_button_picks_a_file_and_cancelling_changes_nothing() {
        use egui_kittest::kittest::Queryable;
        let mut app = RustGisApp::with_sample();
        app.run("ui.connection", json!({"kind": "sqlserver", "form": true, "server": "typed-db"})).unwrap();
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(700.0, 760.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                dialog(app, ui);
            },
            app,
        );
        h.run_steps(2);
        // Cancelled: the form is as it was.
        crate::dialogs::PICKED.with(|p| *p.borrow_mut() = None);
        h.get_by_label("From .sde file…").click();
        h.run_steps(2);
        assert_eq!(h.state().ui.connection.server, "typed-db");
        assert!(h.state().session.connections.connections.is_empty());
        // A file picked fills the form from it, and saves nothing.
        crate::dialogs::PICKED.with(|p| *p.borrow_mut() = Some(sde_fixture("sqlserver-os.sde")));
        h.get_by_label("From .sde file…").click();
        h.run_steps(2);
        let st = &h.state().ui.connection;
        assert_eq!((st.server.as_str(), st.database.as_str()), ("gisdb.example.invalid", "gis"));
        assert!(h.state().session.connections.connections.is_empty());
    }

    #[test]
    fn a_geodatabase_form_save_makes_the_connection_and_a_bad_form_makes_none() {
        let mut app = RustGisApp::with_sample();
        app.run("ui.connection", json!({"kind": "sqlserver", "form": true, "server": "gis-db\\GIS", "database": "utility"})).unwrap();
        assert_eq!((app.ui.connection.server.as_str(), app.ui.connection.database.as_str()), ("gis-db\\GIS", "utility"));
        app.ui.connection.user = "sde".into();
        app.ui.connection.server = "bad host!".into();
        assert!(save(&mut app).is_err());
        assert!(app.session.connections.connections.is_empty());
        app.ui.connection.server = "gis-db\\GIS".into();
        save(&mut app).unwrap();
        let list = &app.session.connections.connections;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].address, "mssql://sde@gis-db/utility?instance=GIS");
        assert_eq!(list[0].name, "Geodatabase on gis-db\\GIS");
    }

    #[test]
    fn the_geodatabase_form_and_the_address_swap_without_losing_what_was_typed() {
        let mut st = State { address: "mssql://old@elsewhere/db".into(), ..gdb_form("sql") };
        swap_form(&mut st).unwrap();
        assert!(!st.form);
        assert_eq!(definition(&st)["address"], "mssql://sde@gis-db/utility?instance=GIS");
        // Back to the form: the address fills it as it was typed.
        swap_form(&mut st).unwrap();
        assert!(st.form);
        assert_eq!((st.server.as_str(), st.user.as_str(), st.database.as_str()), ("gis-db\\GIS", "sde", "utility"));
        // A form that can't make an address stays the form, says why, and leaves the address.
        st.server = "bad host!".into();
        assert!(swap_form(&mut st).is_err());
        assert!(st.form);
        assert_eq!(st.address, "mssql://sde@gis-db/utility?instance=GIS");
        assert_eq!(st.server, "bad host!");
        // A form with no server but something else typed stays, so nothing typed is lost.
        let mut half =
            State { kind: "sqlserver".into(), form: true, user: "gis".into(), address: "mssql://h/d".into(), ..Default::default() };
        assert!(swap_form(&mut half).is_err());
        assert!(half.form);
        assert_eq!(half.user, "gis");
        // An empty form goes to the address box as it was: nothing was typed to lose.
        let mut empty = State { kind: "sqlserver".into(), form: true, address: "mssql://h/d".into(), ..Default::default() };
        swap_form(&mut empty).unwrap();
        assert!(!empty.form);
        assert_eq!(empty.address, "mssql://h/d");
        // An address edited in the box is what the form shows next.
        st.form = false;
        st.address = "mssql://gis@other/assets".into();
        swap_form(&mut st).unwrap();
        assert_eq!((st.server.as_str(), st.user.as_str(), st.database.as_str()), ("other", "gis", "assets"));
        // An address the form can't read stays in the box, and why is said.
        st.form = false;
        st.address = "mssl://gis-db/utility".into();
        let why = swap_form(&mut st).unwrap_err();
        assert!(why.contains("mssql://"), "{why}");
        // A server the form can't write back (an IPv6 address) stays in the address box.
        let mut v6 = State { kind: "sqlserver".into(), address: "mssql://reader@[::1]:1500/gis".into(), ..Default::default() };
        assert!(swap_form(&mut v6).is_err());
        assert!(!v6.form && v6.server.is_empty());
        assert_eq!(v6.address, "mssql://reader@[::1]:1500/gis");
        // A database the form would write back as another (a leading space it trims) stays too.
        let mut spaced = State { kind: "sqlserver".into(), address: "mssql://reader@gis-db/%20utility".into(), ..Default::default() };
        assert!(swap_form(&mut spaced).is_err());
        assert!(!spaced.form && spaced.server.is_empty());
        // An empty instance is a route of its own to the engine (no default port): the form
        // can't show it, so the address stays as typed and why is said.
        for empty in
            ["mssql://gis-db/utility?instance=", "mssql://gis-db/utility?instance", "mssql://gis-db/utility?instance=GIS&instance="]
        {
            let mut kept = State { kind: "sqlserver".into(), address: empty.into(), ..Default::default() };
            let why = swap_form(&mut kept).unwrap_err();
            assert!(why.contains("instance"), "{empty}: {why}");
            assert!(!kept.form && kept.server.is_empty(), "{empty}");
        }
        assert!(!st.form);
        assert_eq!(st.address, "mssl://gis-db/utility");
        assert_eq!(st.server, "other");
        // An empty form is filled from the address: a saved connection edited in the form.
        let mut edit = State {
            kind: "sqlserver".into(),
            address: "mssql://gis-db/utility?trust-server-certificate=true".into(),
            ..Default::default()
        };
        swap_form(&mut edit).unwrap();
        assert_eq!(
            (edit.server.as_str(), edit.database.as_str(), edit.sign_in.as_str(), edit.trust_cert),
            ("gis-db", "utility", "windows", true)
        );
    }

    /// The address's options the form has no field for are kept and shown, not dropped (review:
    /// `encrypt` and `ca` lost on Save); an address with a password stays in the box.
    #[test]
    fn an_address_with_options_or_a_password_goes_through_the_form_whole() {
        let mut st = State {
            kind: "sqlserver".into(),
            address: r"mssql://sde@gis-db/utility?instance=GIS&encrypt=strict&ca=C:\certs\ca.pem&trust-server-certificate=true".into(),
            ..Default::default()
        };
        swap_form(&mut st).unwrap();
        assert_eq!(st.form_options, r"encrypt=strict&ca=C:\certs\ca.pem");
        assert!(st.trust_cert);
        assert_eq!(
            definition(&st)["address"],
            r"mssql://sde@gis-db/utility?instance=GIS&encrypt=strict&ca=C:\certs\ca.pem&trust-server-certificate=true"
        );
        swap_form(&mut st).unwrap();
        assert_eq!(st.address, r"mssql://sde@gis-db/utility?instance=GIS&encrypt=strict&ca=C:\certs\ca.pem&trust-server-certificate=true");
        // Options with no instance start the query.
        let mut plain =
            State { kind: "sqlserver".into(), address: "mssql://gis-db/utility?connect-timeout=30".into(), ..Default::default() };
        swap_form(&mut plain).unwrap();
        assert_eq!(definition(&plain)["address"], "mssql://gis-db/utility?connect-timeout=30");
        // A password in the address: the form isn't used, and why is said.
        let mut secret = State { kind: "sqlserver".into(), address: "mssql://sde:hunter2@gis-db/utility".into(), ..Default::default() };
        let why = swap_form(&mut secret).unwrap_err();
        assert!(why.contains("password"), "{why}");
        assert!(!secret.form);
        assert!(secret.server.is_empty());
        // However the address carries it: an unencoded `/` or `?` in the password, or a
        // password, token or key in the query (the engine reads them all).
        for address in [
            "mssql://sde:pa/ss@gis-db/utility",
            "mssql://sde:pa?ss@gis-db/utility",
            "mssql://sde@gis-db/utility?pwd=hunter2",
            "mssql://sde@gis-db/utility?encrypt=strict&PASSWD=hunter2",
            "mssql://sde@gis-db/utility?password=hunter2",
            "mssql://gis-db/utility?token=abc",
        ] {
            let mut st = State { kind: "sqlserver".into(), address: address.into(), ..Default::default() };
            assert!(swap_form(&mut st).is_err(), "{address}");
            assert!(!st.form && st.form_options.is_empty(), "{address}");
        }
        // The rest of such an address is still read as the engine reads it.
        let m = parse_mssql("mssql://sde:pa/ss@gis-db/utility").unwrap();
        assert_eq!((m.server.as_str(), m.user.as_deref(), m.database.as_str(), m.password), ("gis-db", Some("sde"), "utility", true));
    }

    #[test]
    fn the_geodatabase_form_draws_its_rows_and_swaps_with_the_address() {
        use egui_kittest::kittest::Queryable;
        let mut app = RustGisApp::with_sample();
        app.run("ui.connection", json!({"kind": "sqlserver", "form": true})).unwrap();
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(700.0, 760.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                dialog(app, ui);
            },
            app,
        );
        h.run_steps(2);
        for label in [
            "Platform",
            "Server",
            "Sign in",
            "User",
            "Password",
            "Database",
            "Name",
            "Group",
            "From .sde file…",
            "Trust the server's certificate",
        ] {
            assert!(h.query_by_label(label).is_some(), "{label}");
        }
        assert!(h.query_by_label("Connection").is_none());
        h.get_by_label("Enter an address instead").click();
        h.run_steps(2);
        assert!(!h.state().ui.connection.form);
        assert!(h.query_by_label("Connection").is_some());
        assert!(h.query_by_label("Server").is_none());
        h.get_by_label("Use the form instead").click();
        h.run_steps(2);
        assert!(h.state().ui.connection.form);
        // Windows sign-in has no user or password to type.
        h.state_mut().ui.connection.sign_in = "windows".into();
        h.run_steps(2);
        assert!(h.query_by_label("User").is_none());
        assert!(h.query_by_label("Password").is_none());
    }

    #[test]
    fn failed_unsaved_service_probe_clears_credentials() {
        let mut app = RustGisApp::with_sample();
        for auth in ["basic", "header", "query"] {
            open_new(&mut app, "wmts").unwrap();
            let st = &mut app.ui.connection;
            st.address = String::new();
            st.auth = auth.into();
            st.user = "reader".into();
            st.key_name = "X-Key".into();
            st.password = "probe-failure-password".into();
            st.api_key = "probe-failure-key".into();
            st.allow_insecure = true;
            start_test(&mut app);
            assert!(app.dialogs.error.is_some());
            assert!(app.ui.connection.test.is_none());
            assert!(app.ui.connection.password.is_empty());
            assert!(app.ui.connection.api_key.is_empty());
            assert!(!app.ui.connection.allow_insecure);
        }
    }

    #[test]
    fn unsaved_query_probe_never_records_a_pasted_custom_key() {
        let _turn = crate::fetching::turn();
        struct Fetcher;
        impl Drop for Fetcher {
            fn drop(&mut self) {
                cache::clear_fetcher();
            }
        }
        cache::set_fetcher(|_, done| done(Ok(b"{}".to_vec())));
        let _fetcher = Fetcher;
        let mut app = RustGisApp::with_sample();
        open_new(&mut app, "wmts").unwrap();
        let clean = "https://probe-log757.invalid/wmts?layer=a";
        let pasted = "RGSYNTH_unsaved_pasted_query";
        let st = &mut app.ui.connection;
        st.address = format!("{clean}&zq_probe_custom757={pasted}");
        st.auth = "query".into();
        st.key_name = "zq_probe_custom757".into();
        st.api_key = "RGSYNTH_unsaved_typed_query".into();
        start_test(&mut app);
        assert!(app.ui.connection.test.is_some(), "{:?}", app.dialogs.error);
        let initial = app.session.log.entries().find(|e| e.command == "connection.test").unwrap();
        assert_eq!(initial.params["address"], clean);
        assert!(!serde_json::to_string(initial).unwrap().contains(pasted));
        poll(&mut app, &egui::Context::default());
        assert!(app.ui.connection.test.is_none());
        let probes: Vec<_> = app.session.log.entries().filter(|e| e.command == "connection.test").collect();
        assert_eq!(probes.len(), 2, "both request and response paths were logged");
        assert!(probes.iter().all(|e| e.params["address"] == clean));
        let mut exported = Vec::new();
        app.session.log.write(&mut exported, false).unwrap();
        assert!(!String::from_utf8(exported).unwrap().contains(pasted));
        assert!(app.session.connections.connections.is_empty(), "the probe saved no definition");
    }

    #[test]
    fn invalid_query_probe_names_never_log_an_address() {
        for name in ["", "bad name", "zq+custom757", "_"] {
            let mut app = RustGisApp::with_sample();
            open_new(&mut app, "wmts").unwrap();
            let st = &mut app.ui.connection;
            st.address = "https://probe-invalid757.invalid/wmts?zq_unknown757=RGSYNTH_invalid_probe".into();
            st.auth = "query".into();
            st.key_name = name.into();
            st.api_key = "RGSYNTH_invalid_typed_probe".into();
            start_test(&mut app);
            assert!(app.dialogs.error.is_some(), "{name:?}");
            assert!(app.ui.connection.api_key.is_empty());
            assert!(app.session.log.entries().all(|e| e.command != "connection.test"), "{name:?}");
        }
    }

    #[test]
    fn abandoned_probe_clears_only_its_own_status_in_a_replacement_dialog() {
        // A captured translation differs from the current language without changing global state.
        let status = "Prüfung läuft…";
        for (dialog, info, expected_info, expected_error) in [
            (Some("addDelimited"), Some(status), None, Some("replacement file read failed")),
            (Some("addDelimited"), Some("replacement file loaded"), Some("replacement file loaded"), Some("replacement file read failed")),
            (Some("addDelimited"), None, None, Some("replacement file read failed")),
            (None, Some(status), None, None),
            (None, Some("previous info"), None, None),
        ] {
            let mut app = RustGisApp::with_sample();
            open_new(&mut app, "wmts").unwrap();
            let context = TestContext { opened: app.session.opened, form: definition(&app.ui.connection), status, _login: None };
            app.ui.connection.test = Some((json!({}), Slot::default(), context));
            app.dialogs.open = dialog;
            app.dialogs.info = info.map(str::to_owned);
            app.dialogs.error = Some("replacement file read failed".into());
            poll(&mut app, &egui::Context::default());
            assert!(app.ui.connection.test.is_none());
            assert_eq!(app.dialogs.open, dialog);
            assert_eq!(app.dialogs.info.as_deref(), expected_info);
            assert_eq!(app.dialogs.error.as_deref(), expected_error);
        }
    }

    #[test]
    fn unsaved_service_test_requires_valid_typed_credentials_not_a_saved_login() {
        let mut app = RustGisApp::with_sample();
        let address = "https://unsaved-test-validation.example/wmts";
        let made = app.run("connection.new", json!({"kind":"wmts","address":address,"auth":"header","keyName":"X-Api-Key"})).unwrap();
        let id = made["id"].as_str().unwrap();
        app.run("connection.login", json!({"id":id,"apiKey":"saved-validation-key"})).unwrap();
        let saved = app.session.connections.find(id).unwrap().clone();
        for auth in ["basic", "header", "query"] {
            for secret in ["", "invalid\ncredential"] {
                open_new(&mut app, "wmts").unwrap();
                let st = &mut app.ui.connection;
                st.address = address.into();
                st.auth = auth.into();
                st.user = "reader".into();
                st.key_name = "X-Api-Key".into();
                if auth == "basic" {
                    st.password = secret.into();
                } else {
                    st.api_key = secret.into();
                }
                assert!(test_blocked(&app).is_none());
                start_test(&mut app);
                let reason = if secret.is_empty() { web::REQUIRED } else { web::BAD_SECRET };
                assert_eq!(app.dialogs.error, Some(translated(reason)), "{auth}");
                assert!(app.ui.connection.test.is_none());
                assert!(app.ui.connection.id.is_none());
                assert_eq!(app.session.connections.connections, vec![saved.clone()]);
                assert_eq!(web::known_secret(&mut app.session, &saved).unwrap().expose(), "saved-validation-key");
                for entry in app.session.log.entries().filter(|e| e.command == "connection.test") {
                    assert!(entry.params.get("apiKey").is_none());
                    assert!(entry.params.get("password").is_none());
                }
            }
        }
    }

    #[test]
    fn edited_service_test_is_blocked_without_sending_or_saving_the_key() {
        let mut app = RustGisApp::with_sample();
        let c = app
            .run(
                "connection.new",
                json!({
                    "kind":"wms", "address":"https://test-form.example/wms", "auth":"header", "keyName":"X-Api-Key"
                }),
            )
            .unwrap();
        let id = c["id"].as_str().unwrap();
        let saved = app.session.connections.find(id).unwrap().clone();
        for field in ["address", "auth", "user", "keyName", "kind", "non-web kind"] {
            open_edit(&mut app, id).unwrap();
            let st = &mut app.ui.connection;
            st.api_key = "never-sent-form-key".into();
            match field {
                "address" => st.address = "https://other-test-form.example/wms".into(),
                "auth" => st.auth = "basic".into(),
                "user" => st.user = "unsaved-user".into(),
                "keyName" => st.key_name = "X-Other-Key".into(),
                "kind" => st.kind = "wmts".into(),
                "non-web kind" => st.kind = "postgres".into(),
                _ => {}
            }
            assert!(test_blocked(&app).is_some(), "{field}");
            start_test(&mut app);
            assert_eq!(app.dialogs.error.as_deref(), Some(web::SAVE_BEFORE_TEST), "{field}");
            assert!(app.ui.connection.test.is_none());
            assert_eq!(app.ui.connection.api_key, "never-sent-form-key");
            assert!(!app.session.database_logins.has_session_password(id));
            assert!(!web::logged_in(&app.session, &saved));
            assert_eq!(app.session.connections.find(id), Some(&saved));
        }
        open_edit(&mut app, id).unwrap();
        app.ui.connection.name = "An unsaved display name".into();
        assert!(test_blocked(&app).is_none(), "display-only edits don't change the login");
    }

    #[test]
    fn invalid_service_replacement_preserves_session_login_and_definition() {
        let _turn = crate::fetching::turn();
        let mut app = RustGisApp::with_sample();
        let c = app
            .run(
                "connection.new",
                json!({
                    "kind":"wms", "address":"https://replacement-form.example/wms", "auth":"basic", "user":"reader"
                }),
            )
            .unwrap();
        let id = c["id"].as_str().unwrap();
        app.run("connection.login", json!({"id":id, "password":"original-password"})).unwrap();
        let saved = app.session.connections.find(id).unwrap().clone();
        for password in ["bad\npassword".to_string(), "x".repeat(4097), "x".repeat(3070)] {
            open_edit(&mut app, id).unwrap();
            app.ui.connection.address = "https://unsaved-replacement-form.example/wms".into();
            app.ui.connection.password = password;
            assert!(save(&mut app).is_err());
            assert_eq!(app.session.connections.find(id), Some(&saved));
            assert_eq!(web::known_secret(&mut app.session, &saved).unwrap().expose(), "original-password");
        }
    }

    #[test]
    fn opening_a_remembered_connection_adopts_a_missing_recorded_project_choice() {
        use rustgis_engine::data::secrets::MemorySecrets;
        use std::sync::Arc;

        let mut app = RustGisApp::with_sample();
        let origin = "https://open-remembered-adoption.example";
        app.run("map.addTiles", json!({"url":format!("{origin}/tiles/{{z}}/{{x}}/{{y}}.png")})).unwrap();
        app.session.map_mut().tiles[0].connection = "deleted-connection".into();
        let made = app
            .run("connection.new", json!({"kind":"wms", "address":format!("{origin}/wms"), "auth":"header", "keyName":"X-Api-Key"}))
            .unwrap();
        let id = made["id"].as_str().unwrap();
        let store = Arc::new(MemorySecrets::new());
        app.session.database_logins.set_store(Some(store.clone()));
        app.session.connections.set_remembered(id, true);
        let c = app.session.connections.find(id).unwrap().clone();
        assert!(web::save(app.session.database_logins.secret_store(), &c, "remembered-adoption-key"));
        app.session.database_logins.request(LoginRequest {
            reason: LoginReason::Connection,
            kind: WEB,
            key: origin.into(),
            connections: Vec::new(),
            layers: Vec::new(),
            tiles: 1,
            opened: app.session.opened,
            prompted: false,
        });
        assert!(!web::logged_in(&app.session, &c));

        open_connection(&mut app, &c).unwrap();

        assert!(web::logged_in(&app.session, &c));
        assert_eq!(app.session.map().tiles[0].connection, c.id);
        assert!(!app.session.database_logins.requests(app.session.opened).any(|r| r.kind == WEB && r.key == origin));
    }

    #[test]
    fn opening_an_active_connection_resolves_a_choice_after_the_selected_peer_logs_out() {
        let mut app = RustGisApp::with_sample();
        let origin = "https://active-choice.example";
        let mut ids = Vec::new();
        for name in ["A", "B"] {
            let made = app
                .run(
                    "connection.new",
                    json!({
                        "kind":"wms", "name":name, "address":format!("{origin}/wms"),
                        "auth":"header", "keyName":"X-Key"
                    }),
                )
                .unwrap();
            let id = made["id"].as_str().unwrap().to_string();
            app.run("connection.login", json!({"id":id,"apiKey":format!("secret-{name}")})).unwrap();
            ids.push(id);
        }
        app.run("connection.logout", json!({"id":ids[1]})).unwrap();
        let a = app.session.connections.find(&ids[0]).unwrap().clone();
        assert!(web::logged_in(&app.session, &a));
        assert_eq!(app.session.database_logins.selection(WEB, origin), None);
        let mut project = rustgis_engine::Project::default();
        project.map_mut().tiles.push(rustgis_engine::map::TileLayer::new(
            "Unassigned",
            rustgis_engine::map::TileKind::Raster,
            format!("{origin}/tiles/{{z}}/{{x}}/{{y}}.png"),
        ));
        app.run("project.openText", json!({"text":serde_json::to_string(&project).unwrap()})).unwrap();
        assert!(app.session.database_logins.requests(app.session.opened).any(|r| r.kind == WEB && r.reason == LoginReason::Choose));
        open_connection(&mut app, &a).unwrap();
        assert_eq!(app.session.database_logins.selection(WEB, origin), Some(a.id.as_str()));
        assert!(!app.session.database_logins.requests(app.session.opened).any(|r| r.kind == WEB && r.key == origin));
        assert_eq!(app.session.map().tiles[0].connection, a.id);
        let applied = rustgis_engine::render::rustgis_tiles::logins::apply(&app.session.map().tiles[0].url).unwrap().unwrap();
        assert_eq!(applied.header.unwrap().1, "secret-A");
    }

    #[test]
    fn opening_a_connection_without_a_login_method_logs_nothing_in() {
        let mut app = RustGisApp::with_sample();
        let made = app.run("connection.new", json!({"kind":"wms", "address":"http://127.0.0.1:9/wms"})).unwrap();
        let c = app.session.connections.find(made["id"].as_str().unwrap()).unwrap().clone();
        let origin = login_origin(&c).unwrap();
        let _ = open_connection(&mut app, &c);
        // A login would choose this connection for its origin (and drop the tile cache).
        assert_eq!(app.session.database_logins.selection(WEB, &origin), None);
    }

    #[test]
    fn a_refused_s3_login_keeps_the_typed_keys_in_the_form() {
        let mut app = RustGisApp::with_sample();
        open_new(&mut app, "s3").unwrap();
        app.ui.connection.name = "Refused keys".into();
        app.ui.connection.access_key = "AKIAEXAMPLE".into();
        assert!(save(&mut app).is_err(), "a key ID without its secret is refused");
        assert_eq!(app.ui.connection.access_key, "AKIAEXAMPLE");
    }

    #[test]
    fn picked_requires_identity_and_the_unchanged_public_address() {
        let list = vec![
            Connection { id: "c1".into(), address: "https://example.com/wms".into(), ..Default::default() },
            Connection { id: "c2".into(), address: "https://example.com/wms".into(), ..Default::default() },
        ];
        assert!(picked(&list, None, &list[0].address).is_none());
        assert_eq!(picked(&list, Some("c2"), " https://example.com/wms ").map(|c| c.id.as_str()), Some("c2"));
        assert!(picked(&list, Some("removed"), &list[0].address).is_none());
        assert!(picked(&list, Some("c1"), "https://example.com/other").is_none());
        assert_eq!(picked(&list, Some("c1"), "https://example.com/wms?token=secret").map(|c| c.id.as_str()), Some("c1"));
    }

    #[test]
    fn remember_needs_a_store_and_no_session_token() {
        assert!(remember_blocked(true, "").is_none());
        assert!(remember_blocked(true, "  ").is_none());
        assert!(remember_blocked(false, "").is_some());
        assert!(remember_blocked(true, "token").is_some());
    }

    fn new_s3_form(app: &mut RustGisApp, name: &str) {
        app.run("ui.connection", json!({"kind": "s3"})).unwrap();
        let st = &mut app.ui.connection;
        assert!(!st.remember, "Remember starts off");
        st.name = name.into();
        st.access_key = "AKIDREMEMBERUI".into();
        st.secret_key = "remember-ui-secret".into();
        st.remember = true;
    }

    #[test]
    fn remember_without_a_database_password_changes_nothing() {
        let mut app = RustGisApp::with_sample();
        app.session.database_logins.set_store(Some(std::sync::Arc::new(rustgis_engine::data::secrets::MemorySecrets::new())));
        app.run("ui.connection", json!({"kind": "postgres"})).unwrap();
        app.ui.connection.remember = true;
        assert!(save(&mut app).is_err());
        assert!(app.session.connections.connections.is_empty());
    }

    #[test]
    fn remember_without_a_typed_pair_changes_nothing() {
        let mut app = RustGisApp::with_sample();
        app.session.database_logins.set_store(Some(std::sync::Arc::new(rustgis_engine::data::secrets::MemorySecrets::new())));
        new_s3_form(&mut app, "remember-ui-partial-test");
        app.ui.connection.secret_key.clear();
        assert!(save(&mut app).is_err());
        assert!(app.session.connections.connections.is_empty(), "nothing saved");
    }

    #[test]
    fn a_query_login_saves_its_parameter_and_logs_in_without_keeping_the_key() {
        let mut app = RustGisApp::with_sample();
        open_new(&mut app, "wmts").unwrap();
        let st = &mut app.ui.connection;
        st.name = "Query form".into();
        st.address = "https://query-form-1979.example/basemaps/wmts?ZQForm1979=typed-in-address&layer=a".into();
        st.auth = "query".into();
        st.key_name = "zqform1979".into();
        st.api_key = "query-form-secret-1979".into();
        save(&mut app).unwrap();
        let saved = app.session.connections.connections.last().unwrap().clone();
        assert_eq!((saved.auth.as_str(), saved.key_name.as_str()), ("query", "zqform1979"));
        assert_eq!(saved.address, "https://query-form-1979.example/basemaps/wmts?layer=a");
        assert!(web::logged_in(&app.session, &saved));
        let kept = app.session.connections.to_json();
        assert!(!kept.contains("query-form-secret-1979") && !kept.contains("typed-in-address"), "{kept}");
    }

    #[test]
    fn saving_an_address_with_a_key_names_its_parameter_but_not_the_key() {
        let mut app = RustGisApp::with_sample();
        let mark = app.session.notice_mark();
        keep(&mut app, json!({"kind":"wmts","address":"https://keep-key-1979.example/wmts?api_key=keep-key-secret-1979&layer=a"})).unwrap();
        let said = format!("{:?}", app.session.notices_since(mark));
        assert!(said.contains("api_key") && said.contains("API key in the query"), "{said}");
        assert!(!said.contains("keep-key-secret-1979"), "{said}");
        let mark = app.session.notice_mark();
        keep(&mut app, json!({"kind":"wmts","name":"No key","address":"https://keep-key-1979.example/clean?layer=a"})).unwrap();
        assert!(!format!("{:?}", app.session.notices_since(mark)).contains("left out"));
        assert!(!app.session.connections.to_json().contains("keep-key-secret-1979"));
    }
}
