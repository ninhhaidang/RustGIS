//! MySQL and MariaDB: the spatial columns of a database's tables, read live in place (desktop
//! only).
//!
//! RustGIS talks to the server itself (the pure-Rust `mysql` client, TLS through rustls), so
//! nothing else needs installing. Extent queries use the column's spatial index
//! (`MBRIntersects`); geometry comes as WKB, longitude first (MySQL's geographic coordinate
//! systems are latitude first unless asked), and RustGIS converts it to longitude/latitude
//! itself, since MariaDB has no `ST_Transform`. A capped read returns an evenly spread sample,
//! numbered on the server (window functions: MySQL 8 or MariaDB 10.2 and later), so only the rows
//! kept cross the network. The session is read only, and a server asking for a local file
//! (`LOAD DATA LOCAL INFILE`) is refused.
//!
//! What a server sends is hostile input: geometry is decoded by [`wkb`], reads are bounded by
//! `LIMIT`, and statements time out.

use super::{Batch, Family, FeatureSource, Reproject, Request, SourceInfo, clamp_limit, stride};
use crate::{DataError, Feature, Field, FieldType, GeometryType, Result, Value, wkb};
use mysql::prelude::Queryable;
use rustgis_geom::Extent;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

/// How long a statement may run on the server, in seconds.
const STATEMENT_TIMEOUT: u64 = 120;

/// How long counting a table or listing its geometry types when it opens may take, in seconds:
/// past it the count is left unknown.
const PROBE_TIMEOUT: u64 = 10;

/// How long connecting may take, when the connection doesn't say.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Tables with an estimated row count above this aren't counted exactly when opened.
const EXACT_COUNT_BELOW: u64 = 2_000_000;

/// The rows read to measure a table's extent when it opens (an evenly spread sample of a larger
/// table): MySQL has no extent aggregate, and its envelopes fail on geographic coordinates.
const EXTENT_SAMPLE: usize = 20_000;

/// Most geometry columns listed.
const LIST_LIMIT: usize = 10_000;

/// The column types that hold geometry.
const GEOMETRY_TYPES: &[&str] = &[
    "geometry",
    "point",
    "linestring",
    "polygon",
    "multipoint",
    "multilinestring",
    "multipolygon",
    "geometrycollection",
    "geomcollection",
];

/// Schemas of the server's own.
const SYSTEM_SCHEMAS: &[&str] = &["mysql", "information_schema", "performance_schema", "sys"];

// ---------------------------------------------------------------------------------------------
// Connection addresses

pub use super::myconn::{database_name, is_address, split_password};

/// How the connection is protected (the MySQL client's `ssl-mode`).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Tls {
    /// No TLS (`DISABLED`).
    Disable,
    /// TLS when the server has it, else none (`PREFERRED`, the default), the certificate
    /// unchecked as the MySQL client does: encrypted, but not proof against a server in the
    /// middle.
    Prefer,
    /// TLS always, the certificate unchecked (`REQUIRED`).
    Require,
    /// TLS with the server's certificate checked against `ssl-ca` (a private authority) or the
    /// web's roots: its host name too with `VERIFY_IDENTITY`, not with `VERIFY_CA`.
    Verified { ca: Option<std::path::PathBuf>, identity: bool },
}

/// Where and how to connect, from an address.
#[derive(Clone, Debug, PartialEq)]
struct Config {
    host: String,
    port: u16,
    user: String,
    database: Option<String>,
    tls: Tls,
    connect_timeout: Duration,
}

/// An address's configuration (`mysql://[user@]host[:port][/database][?ssl-mode=…&ssl-ca=…]`),
/// with the MySQL client's defaults (`localhost`, port 3306, the login name) for what it leaves
/// out.
fn config(address: &str) -> Result<Config> {
    let bad = |m: &str| DataError::Format(format!("the connection: {m}"));
    let (public, _) = split_password(address).ok_or_else(|| bad("not a mysql:// address"))?;
    let (_, rest) = public.split_once("://").ok_or_else(|| bad("not a mysql:// address"))?;
    let (rest, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (user, hostport) = match super::pgconn::userinfo_end(rest) {
        Some(at) => (rest.get(..at).map(super::pgconn::percent_decode), rest.get(at + 1..).unwrap_or("")),
        None => (None, rest),
    };
    let hostport = hostport.split('/').next().unwrap_or("");
    let (host, port) = if let Some(v6) = hostport.strip_prefix('[') {
        let (h, after) = v6.split_once(']').ok_or_else(|| bad("an unterminated [address]"))?;
        (h.to_string(), after.strip_prefix(':'))
    } else {
        match hostport.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), Some(p)),
            None => (hostport.to_string(), None),
        }
    };
    let port = match port.filter(|p| !p.is_empty()) {
        Some(p) => p.parse().map_err(|_| bad(&format!("port '{p}'")))?,
        None => 3306,
    };
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let mut mode = None;
    let mut ca = None;
    let mut connect_timeout = CONNECT_TIMEOUT;
    for kv in query.split('&').filter(|kv| !kv.is_empty()) {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        let v = super::pgconn::percent_decode(v);
        match k.to_ascii_lowercase().replace('_', "-").as_str() {
            "ssl-mode" | "sslmode" => mode = Some(v.to_ascii_lowercase().replace('-', "_")),
            "ssl-ca" | "sslrootcert" => ca = Some(v),
            "connect-timeout" => {
                // Zero or less is no limit of its own, so the default applies.
                if let Ok(s) = v.trim().parse::<u64>()
                    && s > 0
                {
                    connect_timeout = Duration::from_secs(s);
                }
            }
            _ => {}
        }
    }
    let tls = match mode.as_deref() {
        Some("disabled" | "disable") => Tls::Disable,
        Some("preferred" | "prefer") | None => Tls::Prefer,
        Some("required" | "require") => Tls::Require,
        Some(m @ ("verify_ca" | "verify_identity" | "verify_full")) => {
            Tls::Verified { ca: ca.filter(|c| !c.is_empty() && c != "system").map(std::path::PathBuf::from), identity: m != "verify_ca" }
        }
        Some(other) => return Err(bad(&format!("ssl-mode '{other}'"))),
    };
    Ok(Config {
        host: if host.is_empty() { "localhost".into() } else { host },
        port,
        user: user.filter(|u| !u.is_empty()).or_else(|| env("USER")).or_else(|| env("USERNAME")).unwrap_or_else(|| "root".into()),
        database: database_name(&public),
        tls,
        connect_timeout,
    })
}

// ---------------------------------------------------------------------------------------------
// Connections

/// Which server: their SQL differs in timeouts and axis order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Server {
    mariadb: bool,
    major: u32,
    minor: u32,
}

impl Server {
    fn from_version(v: &str) -> Server {
        let mut parts = v.split(['.', '-']).map(|p| p.trim().parse().unwrap_or(0));
        let (major, minor) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0));
        Server { mariadb: v.to_ascii_lowercase().contains("mariadb"), major, minor }
    }

    /// Whether it has window functions (MySQL 8, MariaDB 10.2), which number samples and keyless
    /// tables.
    fn windows(self) -> bool {
        if self.mariadb { (self.major, self.minor) >= (10, 2) } else { self.major >= 8 }
    }

    /// MySQL 8 reads and writes geographic coordinates latitude first unless asked otherwise.
    fn axis_option(self) -> &'static str {
        if !self.mariadb && self.major >= 8 { ", 'axis-order=long-lat'" } else { "" }
    }

    /// A `SELECT` that times out after `secs` on the server.
    fn timed(self, select: &str, secs: u64) -> String {
        if self.mariadb {
            format!("SET STATEMENT max_statement_time = {secs} FOR {select}")
        } else {
            let rest = select.strip_prefix("SELECT ").unwrap_or(select);
            format!("SELECT /*+ MAX_EXECUTION_TIME({}) */ {rest}", secs.saturating_mul(1000))
        }
    }

    /// Whether an error is a statement timing out.
    fn timed_out(e: &mysql::Error) -> bool {
        // MySQL's ER_QUERY_TIMEOUT, MariaDB's ER_STATEMENT_TIMEOUT.
        matches!(e, mysql::Error::MySqlError(m) if m.code == 3024 || m.code == 1969)
    }
}

/// A connection to a server, shared by every layer read from it. Its statements run one at a
/// time: a slow read holds up the other layers of that server until it ends or times out.
pub struct Conn {
    config: Config,
    password: Option<String>,
    server: Server,
    /// Whether the connection isn't encrypted (`DISABLED`, or `PREFERRED` when the server offered
    /// no TLS).
    plain: std::sync::atomic::AtomicBool,
    client: Mutex<Option<mysql::Conn>>,
}

fn err(e: mysql::Error) -> DataError {
    // The server's own message (a missing table, a refused password) rather than its code.
    match e {
        mysql::Error::MySqlError(m) if matches!(m.code, 1045 | 1698) => DataError::Login(m.message),
        mysql::Error::MySqlError(m) => DataError::Format(m.message),
        e => DataError::Format(e.to_string()),
    }
}

/// Whether an error means the connection went away while it was idle (a server restart, its idle
/// timeout, a killed session), so the statement is worth running once more on a new one. A
/// timeout isn't: the statement would only be slow again.
fn dropped(e: &mysql::Error) -> bool {
    use std::io::ErrorKind as K;
    let gone = |io: &std::io::Error| {
        matches!(io.kind(), K::BrokenPipe | K::ConnectionReset | K::ConnectionAborted | K::UnexpectedEof | K::NotConnected)
    };
    match e {
        mysql::Error::IoError(io) => gone(io),
        // The packet codec's own wraps the I/O error.
        mysql::Error::CodecError(c) => std::error::Error::source(c).and_then(|s| s.downcast_ref::<std::io::Error>()).is_some_and(gone),
        // MySQL's idle disconnect (ER_CLIENT_INTERACTION_TIMEOUT), MariaDB's killed connection
        // and a server shutting down.
        mysql::Error::MySqlError(m) => matches!(m.code, 4031 | 1927 | 1053),
        _ => false,
    }
}

/// Whether a connection can't be used after an error: anything but the server's own answer
/// leaves the protocol in an unknown state.
fn broken(e: &mysql::Error) -> bool {
    dropped(e) || !matches!(e, mysql::Error::MySqlError(_) | mysql::Error::FromValueError(_) | mysql::Error::FromRowError(_))
}

impl Conn {
    fn is_plain(&self) -> bool {
        self.plain.load(std::sync::atomic::Ordering::Acquire)
    }

    fn has_open_client(&self) -> bool {
        match self.client.try_lock() {
            Ok(guard) => guard.is_some(),
            // A connection actively running a query holds `client`; treat it as open rather than
            // blocking the global `live()` pool lock.
            Err(std::sync::TryLockError::WouldBlock) => true,
            Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner().is_some(),
        }
    }

    fn reconnect_cfg(&self) -> Config {
        let mut cfg = self.config.clone();
        if !self.is_plain() && cfg.tls == Tls::Prefer {
            cfg.tls = Tls::Require;
        }
        cfg
    }

    /// Runs `f` with the client, connecting again when the connection dropped: a drop shows only
    /// when it is used, so `f` runs once more on a new one.
    fn with<T>(&self, mut f: impl FnMut(&mut mysql::Conn) -> std::result::Result<T, mysql::Error>) -> Result<T> {
        let mut guard = self.client.lock().unwrap_or_else(|e| e.into_inner());
        for attempt in 0..2 {
            if guard.is_none() {
                // Reconnect an encrypted `Prefer` session as `Require` so the driver never falls
                // back to sending the password or session statements in cleartext.
                let cfg = self.reconnect_cfg();
                let ((client, _), plain) = open_client(&cfg, self.password.as_deref())?;
                if plain && !self.is_plain() {
                    return Err(DataError::Format(
                        "the server no longer offers TLS: connect again with Home › Add › Add Data › Database… to accept that".into(),
                    ));
                }
                self.plain.store(plain, std::sync::atomic::Ordering::Release);
                *guard = Some(client);
            }
            let client = guard.as_mut().ok_or_else(|| DataError::Format("not connected".into()))?;
            match f(client) {
                Ok(v) => return Ok(v),
                Err(e) if attempt == 0 && dropped(&e) => *guard = None,
                Err(e) => {
                    if broken(&e) {
                        *guard = None;
                    }
                    return Err(err(e).redact_secret(self.password.as_deref().unwrap_or("")));
                }
            }
        }
        Err(DataError::Format("not connected".into()))
    }
}

/// Connects, with TLS as the configuration says (`Prefer`: TLS, and without only when the server
/// offers none, said in the second value), and sets the session read only with a statement
/// timeout.
fn open_client(config: &Config, password: Option<&str>) -> Result<((mysql::Conn, Server), bool)> {
    connect_client(config, password).map_err(|e| e.redact_secret(password.unwrap_or("")))
}

fn connect_client(config: &Config, password: Option<&str>) -> Result<((mysql::Conn, Server), bool)> {
    super::tls_provider();
    let base = mysql::OptsBuilder::new()
        .ip_or_hostname(Some(config.host.clone()))
        .tcp_port(config.port)
        .user(Some(config.user.clone()))
        .pass(password.map(str::to_string))
        .db_name(config.database.clone())
        .tcp_connect_timeout(Some(config.connect_timeout))
        .read_timeout(Some(Duration::from_secs(STATEMENT_TIMEOUT + 30)))
        .write_timeout(Some(Duration::from_secs(STATEMENT_TIMEOUT + 30)))
        // A host named `localhost` would otherwise be reached through the server's socket.
        .prefer_socket(false)
        // A server asking for a local file (`LOAD DATA LOCAL INFILE`) is refused, whatever the
        // client's default.
        .local_infile_handler(Some(mysql::LocalInfileHandler::new(|_, _| {
            Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "RustGIS sends the server no local file"))
        })));
    let unchecked = mysql::SslOpts::default().with_danger_accept_invalid_certs(true);
    let attempts: Vec<Option<mysql::SslOpts>> = match &config.tls {
        Tls::Disable => vec![None],
        Tls::Prefer => vec![Some(unchecked), None],
        Tls::Require => vec![Some(unchecked)],
        Tls::Verified { ca, identity } => {
            if let Some(file) = ca {
                super::read_ca_roots("ssl-ca", file, super::CaDer::Fallback)?;
            }
            let ssl = mysql::SslOpts::default().with_danger_skip_domain_validation(!identity);
            vec![Some(match ca {
                Some(file) => ssl.with_root_cert_path(Some(file.clone())),
                None => ssl,
            })]
        }
    };
    let mut last = None;
    for ssl in attempts {
        let plain = ssl.is_none();
        match mysql::Conn::new(base.clone().ssl_opts(ssl)) {
            Ok(c) => return Ok((session(c)?, plain)),
            // Without TLS only when the server says it has none: any other failure (a refused or
            // broken handshake, which someone in the middle could cause) doesn't fall back to
            // sending the password in the clear.
            Err(e @ mysql::Error::DriverError(mysql::DriverError::TlsNotSupported)) => last = Some(e),
            Err(e) => return Err(err(e)),
        }
    }
    Err(last.map(err).unwrap_or_else(|| DataError::Format("not connected".into())))
}

/// What opening a table notes when the connection isn't encrypted or doesn't check the server's
/// certificate.
fn tls_note(tls: &Tls, plain: bool) -> Option<&'static str> {
    match tls {
        Tls::Disable => Some("the connection isn't encrypted (ssl-mode=DISABLED)"),
        Tls::Prefer if plain => Some("the connection isn't encrypted: the server offers no TLS (ssl-mode=REQUIRED refuses such a server)"),
        Tls::Prefer | Tls::Require => Some("the server's certificate isn't checked (ssl-mode=VERIFY_IDENTITY checks it)"),
        Tls::Verified { identity: true, .. } => None,
        Tls::Verified { identity: false, .. } => {
            Some("the server's host name isn't checked against its certificate (ssl-mode=VERIFY_IDENTITY checks it)")
        }
    }
}

/// Sets up a new connection's session: read only, `TIMESTAMP` values in UTC, and a statement
/// timeout where the server has one; with which server it is.
fn session(mut c: mysql::Conn) -> Result<(mysql::Conn, Server)> {
    let version: Option<String> = c.query_first("SELECT VERSION()").map_err(err)?;
    let server = Server::from_version(version.as_deref().unwrap_or(""));
    let timeout = if server.mariadb {
        format!("SET SESSION max_statement_time = {STATEMENT_TIMEOUT}")
    } else {
        format!("SET SESSION max_execution_time = {}", STATEMENT_TIMEOUT * 1000)
    };
    c.query_drop("SET SESSION TRANSACTION READ ONLY").map_err(err)?;
    // `TIMESTAMP` values in UTC, so they read as instants whatever the server's zone. Unlike the
    // timeout, required: without it they'd be read in the server's zone and labelled UTC.
    c.query_drop("SET SESSION time_zone = '+00:00'").map_err(err)?;
    // MySQL 5.7 and older MariaDB have neither: their statements end with the read timeout.
    let _ = c.query_drop(timeout);
    Ok((c, server))
}

/// A live connection: its address and the password it was opened with.
type Live = (String, Option<String>, Weak<Conn>);

/// Live connections, so layers of one server share one.
fn live() -> &'static Mutex<Vec<Live>> {
    static L: OnceLock<Mutex<Vec<Live>>> = OnceLock::new();
    L.get_or_init(Default::default)
}

fn find_live(live: &mut Vec<Live>, address: &str, password: &Option<String>) -> Option<Arc<Conn>> {
    live.retain(|(_, _, w)| w.strong_count() > 0);
    live.iter().filter(|(a, p, _)| a == address && p == password).find_map(|(_, _, w)| w.upgrade().filter(|c| c.has_open_client()))
}

fn finish_live(live: &mut Vec<Live>, address: &str, password: Option<String>, conn: Arc<Conn>) -> Arc<Conn> {
    let want_plain = conn.is_plain();
    live.retain(|(_, _, w)| w.strong_count() > 0);
    if let Some(c) = live
        .iter()
        .filter(|(a, p, _)| a == address && *p == password)
        .find_map(|(_, _, w)| w.upgrade().filter(|c| c.has_open_client() && c.is_plain() == want_plain))
    {
        return c;
    }
    live.retain(|(a, p, w)| {
        if a != address || *p != password {
            return true;
        }
        w.upgrade().is_some_and(|c| c.has_open_client() && c.is_plain() == want_plain)
    });
    live.push((address.to_string(), password, Arc::downgrade(&conn)));
    conn
}

/// Connects to a database (`address`: a `mysql://` or `mariadb://` address as
/// [`super::duckdb::Database::Mysql`] holds it, without its password), or shares a connection
/// already open to it.
///
/// The password is the one remembered for it this session. When `ask` (the user is adding the
/// database), the MySQL client's `MYSQL_PWD` serves too; a project opening never hands it to the
/// server it names.
pub fn connect(address: &str, ask: bool) -> Result<Arc<Conn>> {
    let config = config(address)?;
    let password =
        super::duckdb::remembered_password(address).or_else(|| if ask { super::duckdb::ambient_password("mysql") } else { None });
    if let Some(c) = find_live(&mut live().lock().unwrap_or_else(|e| e.into_inner()), address, &password) {
        return Ok(c);
    }
    // Connecting can take as long as the timeout: other connections don't wait for it.
    let ((client, server), plain) = open_client(&config, password.as_deref())?;
    let conn = Arc::new(Conn {
        config,
        password: password.clone(),
        server,
        plain: std::sync::atomic::AtomicBool::new(plain),
        client: Mutex::new(Some(client)),
    });
    let mut live = live().lock().unwrap_or_else(|e| e.into_inner());
    Ok(finish_live(&mut live, address, password, conn))
}

// ---------------------------------------------------------------------------------------------
// Tables

/// A geometry column of a table or view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Table {
    /// Its database (MySQL's schema).
    pub schema: String,
    pub name: String,
    pub column: String,
    /// The column's type (`point`, `multipolygon`, `geometry`…).
    pub kind: String,
    /// Whether the schema is the one the connection names (its label then leaves it out).
    pub current: bool,
    /// Whether the table has other geometry columns (its label then names the column).
    pub several: bool,
}

impl Table {
    /// Its name: with its schema unless that is the connection's, and `:column` when it has
    /// several.
    pub fn label(&self) -> String {
        let base = if self.current { self.name.clone() } else { format!("{}.{}", self.schema, self.name) };
        if self.several { format!("{base}:{}", self.column) } else { base }
    }

    /// Whether `name` (`table`, `schema.table`, either with `:column`, any case) names it.
    pub fn is_named(&self, name: &str) -> bool {
        let (table, column) = match name.rsplit_once(':') {
            Some((t, c)) => (t, Some(c)),
            None => (name, None),
        };
        let full = format!("{}.{}", self.schema, self.name);
        let table_ok = table.eq_ignore_ascii_case(&self.name) || table.eq_ignore_ascii_case(&full);
        table_ok && column.is_none_or(|c| c.eq_ignore_ascii_case(&self.column))
    }

    fn sql_from(&self) -> String {
        format!("{}.{}", ident(&self.schema), ident(&self.name))
    }

    /// The geometry families its type holds; `None` for `geometry` and collections (any).
    fn declared_family(&self) -> Option<Family> {
        family_of(&self.kind)
    }
}

/// The family of a column type (`multipolygon`, `point`…).
fn family_of(kind: &str) -> Option<Family> {
    let k = kind.to_ascii_lowercase();
    match k.strip_prefix("multi").unwrap_or(&k) {
        "point" => Some(Family::Points),
        "linestring" => Some(Family::Lines),
        "polygon" => Some(Family::Polygons),
        _ => None,
    }
}

fn geometry_type(f: Option<Family>) -> GeometryType {
    match f {
        Some(Family::Points) => GeometryType::Point,
        Some(Family::Lines) => GeometryType::Polyline,
        Some(Family::Polygons) => GeometryType::Polygon,
        None => GeometryType::None,
    }
}

/// An SQL identifier.
fn ident(s: &str) -> String {
    format!("`{}`", s.replace('`', "``"))
}

/// Text of a value as the text protocol sends it (or a number, as the binary one does).
fn text_of(v: &mysql::Value) -> Option<String> {
    match v {
        mysql::Value::NULL => None,
        mysql::Value::Bytes(b) => Some(String::from_utf8_lossy(b).into_owned()),
        mysql::Value::Int(i) => Some(i.to_string()),
        mysql::Value::UInt(u) => Some(u.to_string()),
        mysql::Value::Float(f) => Some(f.to_string()),
        mysql::Value::Double(f) => Some(f.to_string()),
        other => Some(other.as_sql(true).trim_matches('\'').to_string()),
    }
}

fn int_of(v: &mysql::Value) -> Option<i64> {
    match v {
        mysql::Value::Int(i) => Some(*i),
        mysql::Value::UInt(u) => i64::try_from(*u).ok(),
        mysql::Value::Double(f) if f.fract() == 0.0 && f.abs() < 9.0e18 => Some(*f as i64),
        other => text_of(other)?.trim().parse().ok(),
    }
}

/// A row's id: a key's bits, unsigned (a negative key and one past `i64::MAX` alike).
fn id_of(v: &mysql::Value) -> Option<u64> {
    match v {
        mysql::Value::Int(i) => Some(i.cast_unsigned()),
        mysql::Value::UInt(u) => Some(*u),
        other => {
            let t = text_of(other)?;
            let t = t.trim();
            t.parse::<u64>().ok().or_else(|| t.parse::<i64>().ok().map(i64::cast_unsigned))
        }
    }
}

fn float_of(v: &mysql::Value) -> Option<f64> {
    match v {
        mysql::Value::Float(f) => Some(f64::from(*f)),
        mysql::Value::Double(f) => Some(*f),
        mysql::Value::Int(i) => Some(*i as f64),
        mysql::Value::UInt(u) => Some(*u as f64),
        other => text_of(other)?.trim().parse().ok(),
    }
    .filter(|f: &f64| f.is_finite())
}

fn at(r: &mysql::Row, i: usize) -> &mysql::Value {
    r.as_ref(i).unwrap_or(&mysql::Value::NULL)
}

/// The geometry columns of the connection's database (of every database but the server's own
/// when it names none).
pub fn tables(conn: &Conn) -> Result<Vec<Table>> {
    let types: Vec<String> = GEOMETRY_TYPES.iter().map(|t| format!("'{t}'")).collect();
    let system: Vec<String> = SYSTEM_SCHEMAS.iter().map(|t| format!("'{t}'")).collect();
    let sql = format!(
        "SELECT TABLE_SCHEMA, TABLE_NAME, COLUMN_NAME, DATA_TYPE, TABLE_SCHEMA = DATABASE() FROM information_schema.COLUMNS \
         WHERE LOWER(DATA_TYPE) IN ({}) AND TABLE_SCHEMA NOT IN ({}) AND (DATABASE() IS NULL OR TABLE_SCHEMA = DATABASE()) \
         ORDER BY TABLE_SCHEMA, TABLE_NAME, ORDINAL_POSITION LIMIT {LIST_LIMIT}",
        types.join(", "),
        system.join(", ")
    );
    let rows: Vec<mysql::Row> = conn.with(|c| c.query(sql.as_str()))?;
    let mut out: Vec<Table> = rows
        .iter()
        .filter_map(|r| {
            Some(Table {
                schema: text_of(at(r, 0))?,
                name: text_of(at(r, 1))?,
                column: text_of(at(r, 2))?,
                kind: text_of(at(r, 3))?.to_ascii_lowercase(),
                current: int_of(at(r, 4)) == Some(1),
                several: false,
            })
        })
        .collect();
    let mut columns: std::collections::HashMap<(String, String), usize> = std::collections::HashMap::new();
    for t in &out {
        *columns.entry((t.schema.clone(), t.name.clone())).or_default() += 1;
    }
    for t in &mut out {
        t.several = columns.get(&(t.schema.clone(), t.name.clone())).is_some_and(|n| *n > 1);
    }
    Ok(out)
}

/// The table named `name` (`table`, `schema.table`, `…:column`).
pub fn find_table(conn: &Conn, name: &str) -> Result<Table> {
    let all = tables(conn)?;
    all.iter()
        .find(|t| t.label().eq_ignore_ascii_case(name))
        .or_else(|| all.iter().find(|t| t.is_named(name)))
        .cloned()
        .ok_or_else(|| DataError::Format(format!("no table '{name}' with a geometry column")))
}

/// The geometry families a table holds: its column type's, or for a `geometry` column those its
/// rows have (a layer each).
pub fn families(conn: &Conn, table: &Table) -> Result<Vec<Family>> {
    if let Some(f) = table.declared_family() {
        return Ok(vec![f]);
    }
    let g = ident(&table.column);
    let sql =
        conn.server.timed(&format!("SELECT DISTINCT ST_Dimension({g}) FROM {} WHERE {g} IS NOT NULL", table.sql_from()), PROBE_TIMEOUT);
    // A scan of the table, bounded like the other probes. Past that, the rows may hold any type:
    // a layer each, those without rows left out when they open (by their count).
    let rows: Option<Vec<mysql::Row>> = conn.with(|c| match c.query(sql.as_str()) {
        Err(e) if Server::timed_out(&e) => Ok(None),
        r => r.map(Some),
    })?;
    let Some(rows) = rows else {
        return Ok(vec![Family::Points, Family::Lines, Family::Polygons]);
    };
    let mut out: Vec<Family> = rows
        .iter()
        .filter_map(|r| match int_of(at(r, 0))? {
            0 => Some(Family::Points),
            1 => Some(Family::Lines),
            2 => Some(Family::Polygons),
            _ => None,
        })
        .collect();
    out.sort_by_key(|f| *f as u8);
    out.dedup();
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// The source

/// One geometry column of a table (one family of it, for a column of mixed types), read live.
pub struct MysqlSource {
    info: SourceInfo,
    conn: Arc<Conn>,
    table: Table,
    /// The family read, for a column of mixed types.
    family: Option<Family>,
    /// The SRID the column's values carry.
    raw_srid: u32,
    /// Whether the column fixes its SRID (else only rows in `raw_srid` are read).
    constrained: bool,
    /// The expression of each column read: the id, the geometry (as WKB), then the fields.
    select: Vec<String>,
    /// Whether the id is the rows' number as read (no key).
    numbered: bool,
    /// The order rows are numbered in for a sample: the key's, so a sample keeps the same rows
    /// from one read to the next (none without one).
    order: String,
    /// The table's columns a sample numbers and reads (`t.`-qualified): the geometry and the
    /// fields, not every column of every matching row.
    numbered_cols: String,
    reproject: Reproject,
}

impl std::fmt::Debug for MysqlSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MysqlSource").field("table", &self.table).field("family", &self.family).finish_non_exhaustive()
    }
}

/// How a column of a MySQL type is read: as itself or cast. `None` leaves it out (geometry,
/// binary).
fn field_cast(data_type: &str, column_type: &str, col: &str) -> Option<(FieldType, String)> {
    let t = data_type.to_ascii_lowercase();
    if GEOMETRY_TYPES.contains(&t.as_str()) || t.contains("blob") || t.contains("binary") {
        return None;
    }
    Some(match t.as_str() {
        // MySQL's `BOOLEAN` is `tinyint(1)`.
        "tinyint" if column_type.to_ascii_lowercase().starts_with("tinyint(1)") => (FieldType::Boolean, col.to_string()),
        "tinyint" | "smallint" | "mediumint" | "int" | "integer" | "bigint" | "year" => (FieldType::Integer, col.to_string()),
        "bit" => (FieldType::Integer, format!("CAST({col} AS UNSIGNED)")),
        "float" | "double" | "real" | "decimal" | "numeric" => (FieldType::Double, col.to_string()),
        // As ISO text, which a date field parses: a `DATETIME` is a wall-clock time, a
        // `TIMESTAMP` an instant, given in UTC (the session's zone).
        "date" => (FieldType::Date, format!("CAST({col} AS CHAR)")),
        "datetime" => (FieldType::DateTime, format!("CAST({col} AS CHAR)")),
        "timestamp" => (FieldType::DateTime, format!("CONCAT(CAST({col} AS CHAR), 'Z')")),
        _ => (FieldType::Text, format!("CAST({col} AS CHAR)")),
    })
}

impl MysqlSource {
    /// Opens a table's geometry column (`family`: the one family to read of a column of mixed
    /// types). `crs` (`EPSG:…`) is the coordinate system of a geometry whose SRID is 0.
    pub fn open(conn: Arc<Conn>, table: Table, family: Option<Family>, crs: Option<&str>) -> Result<Self> {
        let mut notes = Vec::new();
        let g = ident(&table.column);
        let from = table.sql_from();
        let server = conn.server;
        // Samples and keyless tables are numbered with window functions.
        if !server.windows() {
            return Err(DataError::Unsupported("MySQL tables are read from MySQL 8 or MariaDB 10.2 and later".into()));
        }
        // The SRID: the column's (MySQL 8's per column; MariaDB's `REF_SYSTEM_ID`, its 0 for
        // none), else its rows'.
        let declared_sql = if server.mariadb {
            "SELECT NULLIF(SRID, 0) FROM information_schema.GEOMETRY_COLUMNS WHERE G_TABLE_SCHEMA = ? AND G_TABLE_NAME = ? AND G_GEOMETRY_COLUMN = ?"
        } else {
            "SELECT SRS_ID FROM information_schema.ST_GEOMETRY_COLUMNS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND COLUMN_NAME = ?"
        };
        // (MySQL 5.7 has neither: the rows' then.)
        let declared: Option<u32> =
            match conn.with(|c| c.exec_first::<Option<u32>, _, _>(declared_sql, (&table.schema, &table.name, &table.column))) {
                Ok(d) => d.flatten(),
                Err(e) => {
                    notes.push(format!("the column's coordinate system couldn't be read ({e}): the first row's is used"));
                    None
                }
            };
        let raw_srid = match declared {
            Some(s) => s,
            None => conn
                .with(|c| c.query_first::<Option<u32>, _>(format!("SELECT ST_SRID({g}) FROM {from} WHERE {g} IS NOT NULL LIMIT 1")))?
                .flatten()
                .unwrap_or(0),
        };
        let constrained = declared.is_some();
        if !constrained && raw_srid != 0 {
            notes.push(format!("the column doesn't fix a coordinate system: rows in one other than EPSG:{raw_srid} are left out"));
        }
        let mut srid = raw_srid;
        if srid == 0 {
            match crs.map(str::trim).filter(|c| !c.is_empty()) {
                Some(given) => {
                    let (code, note) = super::given_epsg(given)?;
                    srid = u32::from(code);
                    notes.extend(note);
                }
                None => notes.push(
                    "the geometry doesn't say its coordinate system; read as longitude/latitude (add it again with \"crs\", such as EPSG:2263)".into(),
                ),
            }
        }
        if let Some(note) = tls_note(&conn.config.tls, conn.is_plain()) {
            notes.push(note.into());
        }
        let reproject = Reproject::from_epsg(Some(srid), &mut notes);
        let wkb = format!("ST_AsBinary({g}{})", server.axis_option());

        // The columns: a single integer primary key gives the ids.
        let cols: Vec<mysql::Row> = conn.with(|c| {
            c.exec(
                "SELECT COLUMN_NAME, DATA_TYPE, COLUMN_TYPE, COLUMN_KEY FROM information_schema.COLUMNS \
                 WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? ORDER BY ORDINAL_POSITION",
                (&table.schema, &table.name),
            )
        })?;
        let mut fields = Vec::new();
        let mut casts = Vec::new();
        // (The key, an integer, is among the fields.)
        let mut raw = vec![format!("t.{}", ident(&table.column))];
        let mut keys = Vec::new();
        for r in &cols {
            let (Some(name), Some(ty), Some(column_type), key) =
                (text_of(at(r, 0)), text_of(at(r, 1)), text_of(at(r, 2)), text_of(at(r, 3)))
            else {
                continue;
            };
            if key.as_deref() == Some("PRI") {
                keys.push((name.clone(), ty.to_ascii_lowercase()));
            }
            if name == table.column {
                continue;
            }
            if let Some((ft, cast)) = field_cast(&ty, &column_type, &ident(&name)) {
                casts.push(cast);
                raw.push(format!("t.{}", ident(&name)));
                fields.push(Field::new(name, ft));
            }
        }
        let pk = match keys.as_slice() {
            [(k, ty)] if matches!(ty.as_str(), "tinyint" | "smallint" | "mediumint" | "int" | "integer" | "bigint") => Some(k.clone()),
            _ => None,
        };
        // Ids stable between reads: the key; without one, the rows are numbered as read.
        let numbered = pk.is_none();
        if numbered {
            notes.push(
                "no single-column integer key: features are numbered as read, so their ids can change from one read to the next".into(),
            );
        }
        let order = pk.as_deref().map(|k| format!("ORDER BY t.{}", ident(k))).unwrap_or_default();
        let id = pk.map(|k| ident(&k)).unwrap_or_else(|| "`rustgis rn`".into());
        let mut select = vec![id, wkb];
        select.extend(casts);

        let family = family.or(table.declared_family());
        let mut src = MysqlSource {
            info: SourceInfo {
                name: table.label(),
                format: if server.mariadb { "MariaDB" } else { "MySQL" },
                geometry_type: geometry_type(family),
                fields,
                extent: Extent::EMPTY,
                count: None,
                crs: if srid == 0 { "EPSG:4326".into() } else { format!("EPSG:{srid}") },
                indexed: false,
                notes,
                zm_dropped: false,
            },
            conn,
            table,
            family,
            raw_srid,
            constrained,
            select,
            numbered,
            order,
            numbered_cols: raw.join(", "),
            reproject,
        };
        src.info.indexed = src.has_index();
        src.info.count = src.count_rows();
        if src.info.count != Some(0) {
            src.info.extent = src.measure();
            if src.info.extent.is_empty() {
                src.info.notes.push("the table's extent couldn't be measured in time: zooming to the layer does nothing".into());
            }
        }
        Ok(src)
    }

    /// The table's spatial index on the column, if it has one (a view has none of its own).
    fn has_index(&self) -> bool {
        let t = &self.table;
        self.conn
            .with(|c| {
                c.exec_first::<u8, _, _>(
                    "SELECT 1 FROM information_schema.STATISTICS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND COLUMN_NAME = ? \
                     AND INDEX_TYPE = 'SPATIAL' LIMIT 1",
                    (&t.schema, &t.name, &t.column),
                )
            })
            .ok()
            .flatten()
            .is_some()
    }

    /// The rows with a geometry (of the family read), exactly unless the table is large.
    fn count_rows(&self) -> Option<u64> {
        let t = &self.table;
        let estimate = self
            .conn
            .with(|c| {
                c.exec_first::<Option<u64>, _, _>(
                    "SELECT TABLE_ROWS FROM information_schema.TABLES WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?",
                    (&t.schema, &t.name),
                )
            })
            .ok()
            .flatten()
            .flatten();
        match estimate {
            Some(n) if n >= EXACT_COUNT_BELOW => Some(n),
            // No estimate (a view) or a small one: counted, within the probe's time.
            _ => {
                let sql =
                    self.conn.server.timed(&format!("SELECT COUNT(*) FROM {} WHERE {}", t.sql_from(), self.filter(None)), PROBE_TIMEOUT);
                self.conn.with(|c| c.query_first::<Option<u64>, _>(sql.as_str())).ok().flatten().flatten()
            }
        }
    }

    /// The table's extent in longitude/latitude, from an evenly spread sample of its rows (all of
    /// them for a table that isn't large).
    fn measure(&self) -> Extent {
        let Ok(b) = self.read_within(&Request { extent: None, limit: EXTENT_SAMPLE }, Some(PROBE_TIMEOUT)) else {
            return Extent::EMPTY;
        };
        // Read in the table's own system: each placed in longitude/latitude, vertex by vertex.
        b.features
            .into_iter()
            .filter_map(|f| f.geometry)
            .filter_map(|g| self.reproject.read(g))
            .fold(Extent::EMPTY, |e, p| e.union(&p.lonlat))
    }

    /// The `WHERE` condition: a geometry, of the family read, and in `window` (a polygon's text
    /// from [`box_text`], in the column's coordinates) when given.
    fn filter(&self, window: Option<&str>) -> String {
        let g = ident(&self.table.column);
        let mut w = format!("{g} IS NOT NULL");
        // A column of no fixed SRID: only the rows in the one read (others would fail the read).
        if !self.constrained && self.raw_srid != 0 {
            w.push_str(&format!(" AND ST_SRID({g}) = {}", self.raw_srid));
        }
        if let Some(window) = window {
            // The column itself, so its spatial index serves. The polygon is a literal (numbers
            // RustGIS wrote): as a statement parameter, MySQL parses it again for every row,
            // several times slower.
            w.push_str(&format!(
                " AND MBRIntersects({g}, ST_GeomFromText('{window}', {}{}))",
                self.raw_srid,
                self.conn.server.axis_option()
            ));
        }
        if let Some(f) = self.family.filter(|_| self.table.declared_family().is_none()) {
            let dim = match f {
                Family::Points => 0,
                Family::Lines => 1,
                Family::Polygons => 2,
            };
            w.push_str(&format!(" AND ST_Dimension({g}) = {dim}"));
        }
        w
    }

    /// A row read as a feature: the id, the geometry (as the table holds it, in its own system)
    /// and the fields, in the order of the select, with the geometry's box in longitude/latitude.
    /// `None` without an id or a geometry RustGIS reads and places. A value of a date field that isn't a date
    /// RustGIS holds is null, counted in `unread` ([`Batch::unread_dates`]).
    fn row_feature(&self, r: &mysql::Row, unread: &mut Vec<usize>) -> Option<(Feature, Extent)> {
        let id = id_of(at(r, 0))?;
        let geometry = match at(r, 1) {
            mysql::Value::Bytes(b) => wkb::decode(b),
            _ => None,
        }?;
        let super::Placed { geometry, lonlat } = self.reproject.read(geometry)?;
        let attrs = self
            .info
            .fields
            .iter()
            .enumerate()
            .map(|(k, f)| {
                let v = at(r, k + 2);
                match f.ty {
                    // An unsigned value past `i64::MAX` as a number still, if inexact.
                    FieldType::Integer => int_of(v).map(Value::Int).or_else(|| float_of(v).map(Value::Float)),
                    FieldType::Double => float_of(v).map(Value::Float),
                    FieldType::Boolean => int_of(v).map(|i| Value::Bool(i != 0)),
                    FieldType::Text => text_of(v).map(Value::Text),
                    FieldType::Date | FieldType::DateTime => Some(super::server_date(text_of(v).as_deref(), f.ty, unread, k)),
                }
                .unwrap_or(Value::Null)
            })
            .collect();
        // A negative key keeps its bits: still one id per row, and the same on every read.
        Some((Feature { id, geometry: Some(geometry), attrs }, lonlat))
    }
}

/// A request's native query polygon, or no predicate for an intentional whole-table read.
/// Refuse a box the projection cannot bound before considering the whole-table shortcut.
fn native_window(req: &Request, all: Extent, reproject: &Reproject) -> Result<Option<String>> {
    let Some(e) = req.extent else { return Ok(None) };
    let native = reproject.database_extent(&e)?;
    // The table extent can be sampled; retain the existing shortcut for a view containing it.
    if !all.is_empty() && e.min_x <= all.min_x && e.min_y <= all.min_y && e.max_x >= all.max_x && e.max_y >= all.max_y {
        return Ok(None);
    }
    Ok(box_text(&native, reproject.is_lonlat()))
}

/// A box in a column's coordinates as a polygon's text, only finite numbers (it goes into the SQL
/// as a literal; `lonlat`: geographic, held in range and kept from MySQL's geodesic edges);
/// `None` for an empty or unbounded one.
fn box_text(n: &Extent, lonlat: bool) -> Option<String> {
    if n.is_empty() || ![n.min_x, n.min_y, n.max_x, n.max_y].iter().all(|v| v.is_finite()) {
        return None;
    }
    // MySQL takes a geographic polygon's edges as geodesics, which leave a parallel toward
    // its pole, into the box on one side: a point every 10 degrees keeps them within a
    // tenth of a degree (the sagitta, at most Δλ²/16 radians), and the box is padded by
    // that. (A point every degree is ten times slower to query.)
    let steps = if lonlat { ((n.width() / 10.0).ceil() as usize).clamp(1, 36) } else { 1 };
    let pad = if lonlat { (n.width() / steps as f64).to_radians().powi(2).to_degrees() / 16.0 } else { 0.0 };
    // Geographic coordinates out of range fail on MySQL.
    let (a, b, c, d) = if lonlat {
        (n.min_x.max(-180.0), (n.min_y - pad).max(-90.0), n.max_x.min(180.0), (n.max_y + pad).min(90.0))
    } else {
        (n.min_x, n.min_y, n.max_x, n.max_y)
    };
    // A box clamped to nothing (wholly past ±180°) has no ring; one of no width or height is
    // given a little, so its ring is a polygon.
    if a > c || b > d {
        return None;
    }
    let (c, d) = (if c > a { c } else { a + 1e-9 }, if d > b { d } else { b + 1e-9 });
    let x = |i: usize| a + (c - a) * i as f64 / steps as f64;
    let mut ring: Vec<String> = (0..=steps).map(|i| format!("{} {b}", x(i))).collect();
    ring.extend((0..=steps).map(|i| format!("{} {d}", x(steps - i))));
    ring.push(format!("{a} {b}"));
    Some(format!("POLYGON(({}))", ring.join(", ")))
}

impl MysqlSource {
    /// Reads what a request asks for, each statement timing out after `timeout` seconds when
    /// given (else the session's). Refuses unbounded views before sampling on the server.
    fn read_within(&self, req: &Request, timeout: Option<u64>) -> Result<Batch> {
        let timed = |sql: String| match timeout {
            Some(secs) => self.conn.server.timed(&sql, secs),
            None => sql,
        };
        let limit = clamp_limit(req.limit);
        let mut out = Batch::default();
        // A geographic table has nothing past ±180°/±90° (a view panned off the world).
        if self.reproject.is_lonlat()
            && let Some(e) = req.extent
            && !e.intersects(&Extent::new(-180.0, -90.0, 180.0, 90.0))
        {
            return Ok(out);
        }
        let window = native_window(req, self.info.extent, &self.reproject)?;
        if limit == 0 {
            return Ok(out);
        }
        let filter = self.filter(window.as_deref());
        let from = self.table.sql_from();
        let cols: Vec<String> = self.select.iter().enumerate().map(|(i, e)| format!("{e} AS c{i}")).collect();
        // A sample keeps every `step`-th row, numbered on the server, so only those are converted
        // (the expressions name the table's columns, which the numbered rows carry) and cross the
        // network.
        let numbered = |every: &str| {
            format!(
                "SELECT {}, `rustgis total` FROM (SELECT {}, ROW_NUMBER() OVER ({}) AS `rustgis rn`, {} AS `rustgis total` \
                 FROM {from} t WHERE {filter}) s WHERE MOD(`rustgis rn` - 1, {every}) = 0 LIMIT {limit}",
                cols.join(", "),
                self.numbered_cols,
                self.order,
                if window.is_some() { "COUNT(*) OVER ()" } else { "0" },
            )
        };
        let (matched, rows) = if window.is_some() {
            // A view: counted and numbered in one pass (counting first would scan the view twice,
            // which doubles its time on MySQL's geographic columns).
            let sql = numbered(&format!("GREATEST(CEIL(`rustgis total` / {limit}), 1)"));
            let sql = timed(sql);
            let rows: Vec<mysql::Row> = self.conn.with(|c| c.query(sql.as_str()))?;
            let total = rows.first().and_then(|r| int_of(at(r, self.select.len()))).and_then(|n| u64::try_from(n).ok());
            (total.unwrap_or(0), rows)
        } else {
            // The whole table: its count is cheap, and a read that keeps every row needs no
            // numbering. Both in one snapshot, so the count is of the rows read.
            self.conn.with(|c| {
                let opts = mysql::TxOpts::default().set_with_consistent_snapshot(true).set_access_mode(Some(mysql::AccessMode::ReadOnly));
                let mut t = c.start_transaction(opts)?;
                let n: Option<u64> = t.query_first(timed(format!("SELECT COUNT(*) FROM {from} WHERE {filter}")))?;
                let matched = n.unwrap_or(0);
                let step = stride(matched, limit);
                let sql = if step <= 1 && !self.numbered {
                    format!("SELECT {} FROM {from} WHERE {filter} LIMIT {limit}", cols.join(", "))
                } else {
                    numbered(&step.to_string())
                };
                let rows: Vec<mysql::Row> = t.query(timed(sql))?;
                t.commit()?;
                Ok((matched, rows))
            })?
        };
        let step = stride(matched, limit);
        out.matched = matched;
        for r in &rows {
            // Dates that aren't ones are counted in the rows kept.
            let mut unread = Vec::new();
            let Some((f, lonlat)) = self.row_feature(r, &mut unread) else {
                out.skipped += 1;
                continue;
            };
            // The index matched the request's box in the table's coordinates, which holds more
            // than the request when they are projected: placed in longitude/latitude to compare.
            if req.extent.is_some_and(|e| !lonlat.intersects(&e)) {
                continue;
            }
            super::add_unread_dates(&mut out.unread_dates, &unread);
            out.features.push(f);
        }
        out.complete = step <= 1 && (rows.len() as u64) >= out.matched;
        Ok(out)
    }
}

impl FeatureSource for MysqlSource {
    fn info(&self) -> &SourceInfo {
        &self.info
    }

    /// The table's own system, which its rows are read in (as a file's are kept).
    fn coord_crs(&self) -> crate::raster::Crs {
        self.reproject.crs()
    }

    fn read(&self, req: &Request) -> Result<Batch> {
        self.read_within(req, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refused_login_is_not_a_missing_table() {
        for code in [1045, 1698, 1146] {
            let e = err(mysql::Error::MySqlError(mysql::MySqlError { state: "28000".into(), message: "Access denied".into(), code }));
            if code == 1146 {
                assert!(matches!(e, DataError::Format(_)));
            } else {
                assert!(matches!(e, DataError::Login(_)));
            }
        }
    }

    #[test]
    fn addresses_take_client_defaults_and_modes() {
        let c = config("mysql://me:pw@db.example:3307/gis?ssl-mode=VERIFY_IDENTITY").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!((c.host.as_str(), c.port, c.user.as_str(), c.database.as_deref()), ("db.example", 3307, "me", Some("gis")));
        assert_eq!(c.tls, Tls::Verified { ca: None, identity: true });
        let c = config("mariadb://h?ssl_mode=verify_ca&ssl-ca=/x%20y.pem").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(c.tls, Tls::Verified { ca: Some("/x y.pem".into()), identity: false });
        assert_eq!((c.port, c.database), (3306, None));
        assert_eq!(config("mysql://h?ssl-mode=DISABLED").map(|c| c.tls).ok(), Some(Tls::Disable));
        assert_eq!(config("mysql://h").map(|c| c.tls).ok(), Some(Tls::Prefer));
        assert_eq!(config("mysql://").map(|c| c.host).ok().as_deref(), Some("localhost"));
        let c = config("mysql://u@[::1]:3310/g").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!((c.host.as_str(), c.port), ("::1", 3310));
        assert!(config("mysql://h?ssl-mode=bogus").is_err());
        assert!(config("mysql://h:port/g").is_err());
        assert!(config("mysql://[::1/g").is_err());
        assert!(config("postgres:host=h").is_err());
        let c = config("mysql://h?connect-timeout=0").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(c.connect_timeout, CONNECT_TIMEOUT);
        // A user is always set (the client's default is the login name).
        assert!(!config("mysql://h").map(|c| c.user).unwrap_or_default().is_empty());
    }

    #[test]
    fn tls_notes_and_ca_certificates() {
        assert_eq!(tls_note(&Tls::Disable, true), Some("the connection isn't encrypted (ssl-mode=DISABLED)"));
        assert_eq!(
            tls_note(&Tls::Prefer, true),
            Some("the connection isn't encrypted: the server offers no TLS (ssl-mode=REQUIRED refuses such a server)")
        );
        assert_eq!(tls_note(&Tls::Prefer, false), Some("the server's certificate isn't checked (ssl-mode=VERIFY_IDENTITY checks it)"));
        assert_eq!(tls_note(&Tls::Require, false), Some("the server's certificate isn't checked (ssl-mode=VERIFY_IDENTITY checks it)"));
        assert_eq!(tls_note(&Tls::Verified { ca: None, identity: true }, false), None);
        assert_eq!(
            tls_note(&Tls::Verified { ca: None, identity: false }, false),
            Some("the server's host name isn't checked against its certificate (ssl-mode=VERIFY_IDENTITY checks it)")
        );

        let base_cfg = config("mysql://me@db.example:3306/gis?ssl-mode=PREFERRED").unwrap();
        assert_eq!(base_cfg.tls, Tls::Prefer);
        let dropped_tls = Arc::new(Conn {
            config: base_cfg.clone(),
            password: None,
            server: Server::from_version("8.4.11"),
            plain: std::sync::atomic::AtomicBool::new(false),
            client: Mutex::new(None),
        });
        assert!(!dropped_tls.has_open_client());
        assert_eq!(dropped_tls.reconnect_cfg().tls, Tls::Require);
        let dropped_plain = Arc::new(Conn {
            config: base_cfg,
            password: None,
            server: Server::from_version("8.4.11"),
            plain: std::sync::atomic::AtomicBool::new(true),
            client: Mutex::new(None),
        });
        assert_eq!(dropped_plain.reconnect_cfg().tls, Tls::Prefer);

        let addr = "mysql://me@db.example:3306/gis";
        let pw = None;
        let mut pool: Vec<Live> = vec![(addr.to_string(), pw.clone(), Arc::downgrade(&dropped_tls))];
        assert!(find_live(&mut pool, addr, &pw).is_none());
        let replaced = finish_live(&mut pool, addr, pw.clone(), dropped_plain.clone());
        assert!(Arc::ptr_eq(&replaced, &dropped_plain));
        assert_eq!(pool.len(), 1);

        let ready = std::sync::Barrier::new(2);
        let done = std::sync::Barrier::new(2);
        std::thread::scope(|s| {
            s.spawn(|| {
                let _held = dropped_plain.client.lock().unwrap();
                ready.wait();
                done.wait();
            });
            ready.wait();
            assert!(dropped_plain.has_open_client());
            let found = find_live(&mut pool, addr, &pw).expect("busy connection treated as open");
            assert!(Arc::ptr_eq(&found, &dropped_plain));
            done.wait();
        });

        let dir = tempfile::tempdir().unwrap();
        let bad_cfg = |p: &std::path::Path| {
            let mut c = config("mysql://me@127.0.0.1:1/gis?ssl-mode=VERIFY_IDENTITY").unwrap();
            c.tls = Tls::Verified { ca: Some(p.to_path_buf()), identity: true };
            c.connect_timeout = Duration::from_millis(50);
            c
        };
        assert!(connect_client(&bad_cfg(&dir.path().join("missing.pem")), None).is_err());
        assert!(connect_client(&bad_cfg(dir.path()), None).is_err());

        let huge = dir.path().join("huge.pem");
        std::fs::write(&huge, vec![b'A'; (super::super::MAX_CA_BYTES as usize) + 1]).unwrap();
        let err = connect_client(&bad_cfg(&huge), None).unwrap_err().to_string();
        assert!(err.contains("larger than 1 MB"), "{err}");

        let (der, pem) = super::super::tests::synthetic_ca_cert();
        let pem_path = dir.path().join("ca.pem");
        std::fs::write(&pem_path, &pem).unwrap();
        let der_path = dir.path().join("ca.crt");
        std::fs::write(&der_path, &der).unwrap();
        for valid in [&pem_path, &der_path] {
            assert!(super::super::read_ca_roots("ssl-ca", valid, super::super::CaDer::Fallback).is_ok());
            let err = connect_client(&bad_cfg(valid), None).unwrap_err().to_string();
            assert!(!err.contains("no certificates") && !err.contains("invalid certificate"), "{err}");
        }

        let marker = concat!("SENTINEL_MYSQL_CA_", "LINE_9921");
        for (name, body) in [
            ("broken.pem", format!("-----BEGIN CERTIFICATE-----\n{marker}\n-----END CERTIFICATE-----\n")),
            ("plain.crt", format!("{marker}\nnot a certificate\n")),
        ] {
            let p = dir.path().join(name);
            std::fs::write(&p, body).unwrap();
            let err = connect_client(&bad_cfg(&p), None).unwrap_err().to_string();
            assert!(!err.contains(marker), "error leaked file contents: {err}");
        }
    }

    #[test]
    fn servers_and_their_sql() {
        let my = Server::from_version("8.4.11");
        let maria = Server::from_version("11.8.9-MariaDB-ubu2404");
        assert_eq!((my.mariadb, my.major, maria.mariadb, maria.major), (false, 8, true, 11));
        assert_eq!(my.axis_option(), ", 'axis-order=long-lat'");
        assert_eq!(maria.axis_option(), "");
        assert_eq!(Server::from_version("5.7.44").axis_option(), "");
        assert!(my.windows() && maria.windows() && Server::from_version("10.2.44-MariaDB").windows());
        assert!(!Server::from_version("5.7.44").windows() && !Server::from_version("10.1.48-MariaDB").windows());
        assert_eq!(my.timed("SELECT 1", 10), "SELECT /*+ MAX_EXECUTION_TIME(10000) */ 1");
        assert_eq!(maria.timed("SELECT 1", 10), "SET STATEMENT max_statement_time = 10 FOR SELECT 1");
    }

    #[test]
    fn tables_and_types() {
        let t = |current, several| Table {
            schema: "gis".into(),
            name: "Roads".into(),
            column: "geom".into(),
            kind: "linestring".into(),
            current,
            several,
        };
        assert_eq!(t(true, false).label(), "Roads");
        assert_eq!(t(false, false).label(), "gis.Roads");
        assert_eq!(t(true, true).label(), "Roads:geom");
        assert!(t(true, false).is_named("GIS.roads"));
        assert!(t(true, true).is_named("roads:GEOM"));
        assert!(!t(true, true).is_named("roads:other"));
        assert_eq!(ident("a`b"), "`a``b`");
        assert_eq!(family_of("multipolygon"), Some(Family::Polygons));
        assert_eq!(family_of("POINT"), Some(Family::Points));
        assert_eq!(family_of("geometry"), None);
        assert_eq!(family_of("geomcollection"), None);
        assert_eq!(field_cast("tinyint", "tinyint(1)", "`b`"), Some((FieldType::Boolean, "`b`".into())));
        assert_eq!(field_cast("bigint", "bigint unsigned", "`n`"), Some((FieldType::Integer, "`n`".into())));
        assert_eq!(field_cast("decimal", "decimal(10,2)", "`d`"), Some((FieldType::Double, "`d`".into())));
        assert_eq!(field_cast("date", "date", "`d`"), Some((FieldType::Date, "CAST(`d` AS CHAR)".into())));
        assert_eq!(field_cast("datetime", "datetime(6)", "`t`"), Some((FieldType::DateTime, "CAST(`t` AS CHAR)".into())));
        assert_eq!(field_cast("timestamp", "timestamp", "`t`"), Some((FieldType::DateTime, "CONCAT(CAST(`t` AS CHAR), 'Z')".into())));
        assert_eq!(field_cast("time", "time", "`t`"), Some((FieldType::Text, "CAST(`t` AS CHAR)".into())));
        assert_eq!(field_cast("longblob", "longblob", "`b`"), None);
        assert_eq!(field_cast("point", "point", "`p`"), None);
    }

    #[test]
    fn values_convert() {
        use mysql::Value as V;
        assert_eq!(int_of(&V::Bytes(b"42".to_vec())), Some(42));
        assert_eq!(int_of(&V::UInt(u64::MAX)), None);
        // Ids keep a key's bits, unsigned past `i64::MAX` or negative.
        assert_eq!(id_of(&V::UInt(u64::MAX)), Some(u64::MAX));
        assert_eq!(id_of(&V::Int(-1)), Some(u64::MAX));
        assert_eq!(id_of(&V::Bytes(b"18446744073709551615".to_vec())), Some(u64::MAX));
        assert_eq!(id_of(&V::Bytes(b"-2".to_vec())), Some(u64::MAX - 1));
        assert_eq!(float_of(&V::Bytes(b"12.50".to_vec())), Some(12.5));
        assert_eq!(float_of(&V::Bytes(b"nan".to_vec())), None);
        assert_eq!(text_of(&V::NULL), None);
        assert_eq!(text_of(&V::Bytes(vec![0xff, b'a'])).as_deref(), Some("\u{fffd}a"));
    }

    #[test]
    fn refused_database_view_is_not_a_whole_table_sample() {
        let r = Reproject::from_crs(crate::raster::Crs::from_epsg(32617).unwrap(), &mut Vec::new());
        let req = Request { extent: Some(Extent::new(-120.0, -30.0, 10.0, 30.0)), limit: 10 };
        let all = Extent::new(-84.0, 0.0, -78.0, 25.0);
        assert!(native_window(&req, all, &r).unwrap_err().to_string().contains("can't be bounded"));
        let clean = Request { extent: Some(Extent::new(-84.0, 30.0, -78.0, 40.0)), limit: 10 };
        assert!(native_window(&clean, Extent::EMPTY, &r).unwrap().is_some());
        assert!(native_window(&Request::all(), all, &r).unwrap().is_none());
    }

    #[test]
    fn boxes_keep_clear_of_geodesic_edges() {
        let projected = box_text(&Extent::new(0.0, 0.0, 1000.0, 500.0), false);
        assert_eq!(projected.as_deref(), Some("POLYGON((0 0, 1000 0, 1000 500, 0 500, 0 0))"));
        // A quarter of the world: a point every 10 degrees, padded north and south, in range.
        let quarter = box_text(&Extent::new(-180.0, -60.0, 0.0, 0.0), true).unwrap_or_default();
        let points: Vec<(f64, f64)> = quarter
            .trim_start_matches("POLYGON((")
            .trim_end_matches("))")
            .split(", ")
            .filter_map(|p| p.split_once(' ').and_then(|(x, y)| Some((x.parse().ok()?, y.parse().ok()?))))
            .collect();
        assert_eq!(points.len(), 2 * 19 + 1, "{quarter}");
        assert_eq!(points.first(), points.last());
        let (min_y, max_y) = points.iter().fold((f64::MAX, f64::MIN), |(a, b), p| (a.min(p.1), b.max(p.1)));
        assert!(min_y < -60.0 && min_y > -60.2 && max_y > 0.0 && max_y < 0.2, "{min_y} {max_y}");
        let polar = box_text(&Extent::new(-200.0, -95.0, 10.0, 95.0), true).unwrap_or_default();
        assert!(polar.starts_with("POLYGON((-180 -90,") && polar.contains(" 90,"), "{polar}");
        assert_eq!(box_text(&Extent::EMPTY, true), None);
        assert_eq!(box_text(&Extent::new(190.0, 0.0, 200.0, 10.0), true), None);
        let point = box_text(&Extent::new(5.0, 5.0, 5.0, 5.0), false).unwrap_or_default();
        assert!(point.starts_with("POLYGON((5 5, 5.000000001 5,"), "{point}");
        assert_eq!(box_text(&Extent::new(0.0, 0.0, f64::INFINITY, 1.0), false), None);
    }

    /// Reads every table of a live database: `RUSTGIS_TEST_MYSQL="mysql://user:password@host/db"
    /// cargo test -p rustgis-data -- --ignored mysql_live --nocapture`.
    #[test]
    #[ignore = "needs a live MySQL or MariaDB database in RUSTGIS_TEST_MYSQL"]
    fn mysql_live() {
        let Ok(address) = std::env::var("RUSTGIS_TEST_MYSQL") else { return };
        let super::super::duckdb::Database::Mysql(address) = super::super::duckdb::Database::parse(&address) else {
            panic!("not a connection")
        };
        let conn = connect(&address, true).unwrap_or_else(|e| panic!("{e}"));
        // A connection the server drops is opened again on the next use, which still works.
        let _ = conn.with(|c| c.query_drop("KILL CONNECTION CONNECTION_ID()"));
        let all = tables(&conn).unwrap_or_else(|e| panic!("after a dropped connection: {e}"));
        assert!(!all.is_empty());
        for t in &all {
            let fams = families(&conn, t).unwrap_or_else(|e| panic!("{}: {e}", t.label()));
            for f in fams {
                let start = std::time::Instant::now();
                let src = MysqlSource::open(conn.clone(), t.clone(), Some(f), None).unwrap_or_else(|e| panic!("{}: {e}", t.label()));
                let opened = start.elapsed();
                let i = src.info();
                let whole = src.read(&Request { extent: None, limit: 250_000 }).unwrap_or_else(|e| panic!("{}: {e}", t.label()));
                let read = start.elapsed() - opened;
                let e = i.extent;
                let q = Extent::new(e.min_x, e.min_y, e.min_x + e.width() / 2.0, e.min_y + e.height() / 2.0);
                let started = std::time::Instant::now();
                let part = src.read(&Request { extent: Some(q), limit: 1000 }).unwrap_or_else(|e| panic!("{}: {e}", t.label()));
                let quarter = started.elapsed();
                // In the table's own system: compared in longitude/latitude, as the request is.
                let to = src.coord_crs().to(crate::raster::Crs::LonLat).unwrap_or_else(|e| panic!("{e}"));
                for ft in &part.features {
                    let g = ft.geometry.as_ref().map(|g| to.geometry(g).extent()).unwrap_or(Extent::EMPTY);
                    assert!(g.intersects(&q.padded(0.01, 0.0)), "{}: feature {} outside the request", t.label(), ft.id);
                }
                let mut ids: Vec<u64> = whole.features.iter().map(|f| f.id).collect();
                ids.sort_unstable();
                ids.dedup();
                assert_eq!(ids.len(), whole.features.len(), "{}: ids repeat", t.label());
                println!(
                    "{:<12} {:<9} {:?} {:>9} rows idx={} crs={} extent=[{:.2} {:.2} {:.2} {:.2}] fields={} all: {} of {} complete={} skipped={} | quarter: {} of {} in {:?} | open {:?} read {:?} notes={:?}",
                    t.label(),
                    f.label(),
                    i.geometry_type,
                    i.count.unwrap_or(0),
                    i.indexed,
                    i.crs,
                    e.min_x,
                    e.min_y,
                    e.max_x,
                    e.max_y,
                    i.fields.len(),
                    whole.features.len(),
                    whole.matched,
                    whole.complete,
                    whole.skipped,
                    part.features.len(),
                    part.matched,
                    quarter,
                    opened,
                    read,
                    i.notes
                );
            }
        }
    }
}
