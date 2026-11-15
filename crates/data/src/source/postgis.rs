//! PostGIS: the geometry (and geography) tables of a PostgreSQL database, read live in place
//! (desktop only).
//!
//! RustGIS talks to the server itself (the pure-Rust `postgres` client, TLS through rustls), so
//! nothing else needs installing. Extent queries use the column's GiST index (`&&`); the server
//! converts geometry to longitude/latitude (`ST_Transform`, with the server's own PROJ, so every
//! coordinate system in its `spatial_ref_sys` works) and curves to lines. A capped read returns an
//! evenly spread sample, numbered on the server, so only the rows kept cross the network. The
//! connection is read only.
//!
//! What a server sends is hostile input: geometry is decoded by [`wkb`], reads are bounded by
//! `LIMIT`, and statements time out.

use super::edits::{Conflict, Edits, Saved};
use super::{Batch, Family, FeatureSource, Reproject, Request, SourceInfo, clamp_limit, in_request, stride};
use crate::raster::Crs;
use crate::{DataError, Feature, Field, FieldType, GeometryType, Result, Value, wkb};
use postgres::types::ToSql;
use rustgis_geom::Extent;
use std::sync::{
    Arc, Mutex, OnceLock, Weak,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

/// How long a statement may run on the server.
const STATEMENT_TIMEOUT: &str = "120s";

/// How long connecting may take, when the connection doesn't say.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Tables with an estimated row count above this aren't counted exactly when opened.
const EXACT_COUNT_BELOW: f64 = 2_000_000.0;

/// The most rows a layer loaded to be edited holds ([`PostgisSource::read_editable`]); a table of
/// several geometry types loads each as its own layer.
pub const MAX_EDITABLE: usize = 100_000;

/// How long counting or measuring a table when it opens may take (one never analyzed has no
/// estimate, and could be large): past it the count or extent is left unknown.
const PROBE_TIMEOUT: &str = "10s";

/// A geometry (or geography) column of a table or view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Table {
    pub schema: String,
    pub name: String,
    pub column: String,
    /// The SRID the column is declared with (0: unconstrained).
    pub srid: i32,
    /// The declared geometry type (`POINT`, `MULTIPOLYGON`, `GEOMETRY`…).
    pub kind: String,
    pub geography: bool,
    /// Whether the table has other geometry columns (its label then names the column).
    pub several: bool,
}

impl Table {
    /// Its name: with its schema unless that is `public`, and `:column` when it has several.
    pub fn label(&self) -> String {
        let base = if self.schema == "public" { self.name.clone() } else { format!("{}.{}", self.schema, self.name) };
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

    /// The column as a geometry.
    fn geom(&self) -> String {
        if self.geography { format!("{}::geometry", ident(&self.column)) } else { ident(&self.column) }
    }

    /// The geometry families a declared type holds; `None` for `GEOMETRY` (any).
    fn declared_family(&self) -> Option<Family> {
        family_of(&self.kind)
    }
}

/// The family of a PostGIS geometry type name (`MULTIPOLYGON`, `POINTM`, `CURVEPOLYGON`…).
fn family_of(kind: &str) -> Option<Family> {
    let k = kind.to_ascii_uppercase();
    let k = k.strip_prefix("MULTI").unwrap_or(&k);
    if k.starts_with("POINT") {
        Some(Family::Points)
    } else if k.starts_with("CURVEPOLYGON") {
        Some(Family::Polygons)
    } else if k.starts_with("LINESTRING") || k.starts_with("CIRCULARSTRING") || k.starts_with("COMPOUNDCURVE") || k.starts_with("CURVE") {
        Some(Family::Lines)
    } else if k.starts_with("POLYGON")
        || k.starts_with("SURFACE")
        || k.starts_with("TRIANGLE")
        || k.starts_with("TIN")
        || k.starts_with("POLYHEDRALSURFACE")
    {
        Some(Family::Polygons)
    } else {
        None
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
    format!("\"{}\"", s.replace('"', "\"\""))
}

fn refused_login(e: &postgres::Error) -> bool {
    // 28000 also covers access-policy and role failures that a password cannot fix.
    matches!(e.code(), Some(&postgres::error::SqlState::INVALID_PASSWORD))
        // tokio-postgres exposes no typed discriminator for this client-side config error.
        // The no-password handshake in the engine regression guards its exact source message.
        || std::error::Error::source(e).is_some_and(|s| s.to_string() == "password missing")
}

fn err(e: postgres::Error) -> DataError {
    // The server's own message (a missing table, a failed password) rather than "db error".
    let message = e.as_db_error().map(|db| db.message().to_string()).unwrap_or_else(|| e.to_string());
    if refused_login(&e) { DataError::Login(message) } else { DataError::Format(message) }
}

// ---------------------------------------------------------------------------------------------
// Connections

/// How the connection is protected (libpq's `sslmode`).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Tls {
    /// No TLS (`disable`).
    Disable,
    /// TLS when the server has it (`prefer`, libpq's default) or always (`require`), without
    /// checking the server's certificate, as libpq does: encrypted, but not proof against a
    /// server in the middle. `verify-full` is.
    Unverified,
    /// TLS with the server's certificate checked (`verify-ca`, `verify-full`): against the
    /// certificates of `sslrootcert` (a private authority), else the web's roots.
    Verified(Option<std::path::PathBuf>),
}

/// A connection to a server, shared by every layer read from it (a database of many tables
/// shouldn't take as many of its connections). Its statements run one at a time: a slow read
/// holds up the other layers of that server until it ends or times out.
pub struct Conn {
    config: postgres::Config,
    tls: Tls,
    /// Whether the connection isn't encrypted (`sslmode=disable`, or `prefer` when the server
    /// offered no TLS).
    plain: AtomicBool,
    /// A connection that saves edits ([`connect_writable`]); every other one is read only.
    writable: bool,
    client: Mutex<Option<postgres::Client>>,
}

impl Conn {
    fn is_plain(&self) -> bool {
        self.plain.load(Ordering::Acquire)
    }

    fn has_open_client(&self) -> bool {
        match self.client.try_lock() {
            Ok(guard) => guard.as_ref().is_some_and(|c| !c.is_closed()),
            // A connection actively running a query or save holds `client`; treat it as open
            // rather than blocking the global `live()` pool lock.
            Err(std::sync::TryLockError::WouldBlock) => true,
            Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner().as_ref().is_some_and(|c| !c.is_closed()),
        }
    }

    fn reconnect_cfg(&self) -> postgres::Config {
        let mut cfg = self.config.clone();
        if !self.is_plain() {
            cfg.ssl_mode(postgres::config::SslMode::Require);
        }
        cfg
    }

    fn reconnect_client(&self) -> Result<postgres::Client> {
        let cfg = self.reconnect_cfg();
        let (client, plain) = open_client(&cfg, &self.tls, self.writable)?;
        // A connection that was encrypted isn't continued in the clear.
        if plain && !self.is_plain() {
            return Err(DataError::Format(
                "the server no longer offers TLS: connect again with Home › Add › Add Data › Database… to accept that".into(),
            ));
        }
        self.plain.store(plain, Ordering::Release);
        Ok(client)
    }

    /// Runs `f` with the client, connecting again when the connection dropped: a drop (a server
    /// restart, an idle timeout) shows only when it is used, so `f` runs once more on a new one.
    fn with<T>(&self, mut f: impl FnMut(&mut postgres::Client) -> std::result::Result<T, postgres::Error>) -> Result<T> {
        let mut guard = self.client.lock().unwrap_or_else(|e| e.into_inner());
        for attempt in 0..2 {
            if guard.as_ref().is_none_or(postgres::Client::is_closed) {
                *guard = Some(self.reconnect_client()?);
            }
            let client = guard.as_mut().ok_or_else(|| DataError::Format("not connected".into()))?;
            let r = f(client);
            if !client.is_closed() {
                return r.map_err(|e| redacted(&self.config, err(e)));
            }
            *guard = None;
            if attempt == 1 || r.is_ok() {
                return r.map_err(|e| redacted(&self.config, err(e)));
            }
        }
        Err(DataError::Format("not connected".into()))
    }

    /// Runs `f` once, without the reconnect of [`Conn::with`]: a write that failed when the connection
    /// dropped may have been committed, so it must not run again on a new one.
    fn once<T>(&self, f: impl FnOnce(&mut postgres::Client) -> std::result::Result<T, postgres::Error>) -> Result<T> {
        let mut guard = self.client.lock().unwrap_or_else(|e| e.into_inner());
        if guard.as_ref().is_none_or(postgres::Client::is_closed) {
            *guard = Some(self.reconnect_client()?);
        }
        let client = guard.as_mut().ok_or_else(|| DataError::Format("not connected".into()))?;
        let r = f(client);
        let dropped = client.is_closed();
        if dropped {
            *guard = None;
        }
        match r {
            Ok(v) => Ok(v),
            Err(_) if dropped => {
                Err(DataError::Format("the connection dropped during the save: it may have been saved, so check the table".into()))
            }
            Err(e) => Err(redacted(&self.config, err(e))),
        }
    }

    /// Runs `f` in a read-only transaction seeing one snapshot of the database (`REPEATABLE
    /// READ`), its statements timing out after `timeout` when given.
    fn snapshot<T>(
        &self,
        timeout: Option<&str>,
        mut f: impl FnMut(&mut postgres::Transaction<'_>) -> std::result::Result<T, postgres::Error>,
    ) -> Result<T> {
        self.with(|c| {
            let mut t = c.build_transaction().isolation_level(postgres::IsolationLevel::RepeatableRead).read_only(true).start()?;
            if let Some(timeout) = timeout {
                t.batch_execute(&format!("SET LOCAL statement_timeout = '{timeout}'"))?;
            }
            let r = f(&mut t)?;
            t.commit()?;
            Ok(r)
        })
    }
}

/// Connects, with the password taken out of any error: a server or the client may echo what it was given.
fn open_client(config: &postgres::Config, tls: &Tls, writable: bool) -> Result<(postgres::Client, bool)> {
    connect_client(config, tls, writable).map_err(|e| redacted(config, e))
}

fn redacted(config: &postgres::Config, error: DataError) -> DataError {
    match config.get_password().and_then(|p| std::str::from_utf8(p).ok()) {
        Some(password) => error.redact_secret(password),
        None => error,
    }
}

/// Connects with `config` over `tls`, setting up the session: read only unless `writable`, the
/// statement timeout, full float precision, and dates as ISO text in UTC. The second value says
/// whether the connection is unencrypted (`sslmode=disable`, or `prefer` when the server offered
/// no TLS).
fn connect_client(config: &postgres::Config, tls: &Tls, writable: bool) -> Result<(postgres::Client, bool)> {
    let handshake = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut client = match tls {
        Tls::Disable => config.connect(postgres::NoTls),
        Tls::Unverified | Tls::Verified(_) => config
            .connect(TrackTls { inner: tokio_postgres_rustls::MakeRustlsConnect::new(rustls_config(tls)?), handshake: handshake.clone() }),
    }
    .map_err(|e| {
        let message = match e.as_db_error() {
            Some(db) => db.message().to_string(),
            // The cause ("password authentication failed", "connection refused"), not just
            // "error connecting to server".
            None => std::error::Error::source(&e).map(|s| format!("{e}: {s}")).unwrap_or_else(|| e.to_string()),
        };
        if refused_login(&e) { DataError::Login(message) } else { DataError::Format(message) }
    })?;
    let plain = match tls {
        Tls::Disable => true,
        Tls::Unverified => !handshake.load(std::sync::atomic::Ordering::Relaxed),
        Tls::Verified(_) => false,
    };
    let read_only = if writable { "" } else { "SET default_transaction_read_only = on; " };
    // Dates as text in ISO 8601 whatever the server's own settings, and `timestamptz` in UTC
    // (`+00`), so a date-time with a zone reads as that instant.
    client
        .batch_execute(&format!(
            "{read_only}SET statement_timeout = '{STATEMENT_TIMEOUT}'; SET extra_float_digits = 3; SET DateStyle = 'ISO, YMD'; SET TimeZone = 'UTC';"
        ))
        .map_err(err)?;
    Ok((client, plain))
}

/// Tracks whether the final socket connected by `tokio-postgres` completed a TLS handshake,
/// resetting on each host attempt in a multi-host connection string.
struct TrackTls<M> {
    inner: M,
    handshake: Arc<std::sync::atomic::AtomicBool>,
}

struct TrackTlsConnect<C> {
    inner: C,
    handshake: Arc<std::sync::atomic::AtomicBool>,
}

impl<S, M> postgres::tls::MakeTlsConnect<S> for TrackTls<M>
where
    S: Send + 'static,
    M: postgres::tls::MakeTlsConnect<S>,
    M::TlsConnect: Send + 'static,
    <M::TlsConnect as postgres::tls::TlsConnect<S>>::Future: Send + 'static,
{
    type Stream = M::Stream;
    type TlsConnect = TrackTlsConnect<M::TlsConnect>;
    type Error = M::Error;

    fn make_tls_connect(&mut self, domain: &str) -> std::result::Result<Self::TlsConnect, Self::Error> {
        self.handshake.store(false, std::sync::atomic::Ordering::Relaxed);
        let inner = self.inner.make_tls_connect(domain)?;
        Ok(TrackTlsConnect { inner, handshake: self.handshake.clone() })
    }
}

impl<S, C> postgres::tls::TlsConnect<S> for TrackTlsConnect<C>
where
    S: Send + 'static,
    C: postgres::tls::TlsConnect<S> + Send + 'static,
    C::Future: Send + 'static,
{
    type Stream = C::Stream;
    type Error = C::Error;
    type Future = std::pin::Pin<Box<dyn std::future::Future<Output = std::result::Result<Self::Stream, Self::Error>> + Send>>;

    fn connect(self, stream: S) -> Self::Future {
        let handshake = self.handshake;
        let fut = self.inner.connect(stream);
        Box::pin(async move {
            let res = fut.await;
            if res.is_ok() {
                handshake.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            res
        })
    }
}

fn rustls_config(tls: &Tls) -> Result<rustls::ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| DataError::Format(format!("TLS: {e}")))?;
    Ok(match tls {
        Tls::Verified(Some(file)) => {
            let roots = super::read_ca_roots("sslrootcert", file, super::CaDer::None)?;
            builder.with_root_certificates(roots).with_no_client_auth()
        }
        Tls::Verified(None) => {
            let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
            builder.with_root_certificates(roots).with_no_client_auth()
        }
        _ => builder.dangerous().with_custom_certificate_verifier(Arc::new(Unchecked(provider))).with_no_client_auth(),
    })
}

/// libpq's `sslmode=prefer`/`require`: the connection is encrypted, but the server's certificate
/// isn't checked (its signatures are).
#[derive(Debug)]
struct Unchecked(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for Unchecked {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// What opening a table notes when the connection isn't encrypted or doesn't check the server's
/// certificate.
fn tls_note(tls: &Tls, plain: bool) -> Option<&'static str> {
    match tls {
        Tls::Disable => Some("the connection isn't encrypted (sslmode=disable)"),
        Tls::Unverified if plain => {
            Some("the connection isn't encrypted: the server offers no TLS (sslmode=require refuses such a server)")
        }
        Tls::Unverified => Some("the server's certificate isn't checked (sslmode=verify-full checks it)"),
        Tls::Verified(_) => None,
    }
}

/// A live connection: its connection string, the password it was opened with, and whether it
/// writes.
type Live = (String, Option<String>, bool, Weak<Conn>);

/// Live connections, so layers of one server share one.
fn live() -> &'static Mutex<Vec<Live>> {
    static L: OnceLock<Mutex<Vec<Live>>> = OnceLock::new();
    L.get_or_init(Default::default)
}

/// Connects to a database (`dsn`: a libpq connection string or `postgresql://` address, as
/// [`super::duckdb::Database::Postgres`] holds it, without its password), or shares a connection
/// already open to it.
///
/// The password is the one remembered for it this session. When `ask` (the user is adding the
/// database), libpq's `PGPASSWORD` and password file serve too; a project opening never hands
/// them to the server it names.
pub fn connect(dsn: &str, ask: bool) -> Result<Arc<Conn>> {
    connect_as(dsn, ask, false)
}

/// Connects to a database to save edits: as [`connect`], but the session isn't read only. Never
/// shared with the connections that read layers, which stay read only.
pub fn connect_writable(dsn: &str, ask: bool) -> Result<Arc<Conn>> {
    connect_as(dsn, ask, true)
}

fn find_live(live: &mut Vec<Live>, dsn: &str, password: &Option<String>, writable: bool) -> Option<Arc<Conn>> {
    live.retain(|(_, _, _, w)| w.strong_count() > 0);
    live.iter()
        .filter(|(d, p, wr, _)| d == dsn && p == password && *wr == writable)
        .find_map(|(_, _, _, w)| w.upgrade().filter(|c| c.has_open_client()))
}

fn finish_live(live: &mut Vec<Live>, dsn: &str, password: Option<String>, writable: bool, conn: Arc<Conn>) -> Arc<Conn> {
    let want_plain = conn.is_plain();
    live.retain(|(_, _, _, w)| w.strong_count() > 0);
    if let Some(c) = live
        .iter()
        .filter(|(d, p, wr, _)| d == dsn && *p == password && *wr == writable)
        .find_map(|(_, _, _, w)| w.upgrade().filter(|c| c.has_open_client() && c.is_plain() == want_plain))
    {
        return c;
    }
    live.retain(|(d, p, wr, w)| {
        if d != dsn || *p != password || *wr != writable {
            return true;
        }
        w.upgrade().is_some_and(|c| c.has_open_client() && c.is_plain() == want_plain)
    });
    live.push((dsn.to_string(), password, writable, Arc::downgrade(&conn)));
    conn
}

fn connect_as(dsn: &str, ask: bool, writable: bool) -> Result<Arc<Conn>> {
    let (mut config, tls) = config(dsn)?;
    let password = super::duckdb::remembered_password(dsn).or_else(|| {
        if !ask {
            return None;
        }
        super::duckdb::ambient_password("postgres").or_else(|| pgpass(&config))
    });
    // A connection opened with another password (one given since) or one whose client already
    // closed isn't shared.
    if let Some(c) = find_live(&mut live().lock().unwrap_or_else(|e| e.into_inner()), dsn, &password, writable) {
        return Ok(c);
    }
    if let Some(p) = &password {
        config.password(p);
    }
    // Connecting can take as long as the timeout: other connections don't wait for it.
    let (client, plain) = open_client(&config, &tls, writable)?;
    let conn = Arc::new(Conn { config, tls, plain: AtomicBool::new(plain), writable, client: Mutex::new(Some(client)) });
    let mut live = live().lock().unwrap_or_else(|e| e.into_inner());
    // Another layer may have connected meanwhile; stale closed or downgraded slots are replaced.
    Ok(finish_live(&mut live, dsn, password, writable, conn))
}

/// A connection string's client configuration, with libpq's defaults (`PGHOST`, `PGPORT`,
/// `PGUSER`, `PGDATABASE`, the user's own name) for what it leaves out.
fn config(dsn: &str) -> Result<(postgres::Config, Tls)> {
    let bad = |e: postgres::Error| DataError::Format(format!("the connection: {e}"));
    let t = dsn.trim();
    let lower = t.to_ascii_lowercase();
    let mut sslmode = None;
    let mut rootcert = None;
    let mut config = if lower.starts_with("postgresql://") || lower.starts_with("postgres://") {
        // `sslmode=verify-…` is libpq's, not the client's: taken out of the query here.
        let (base, query) = t.split_once('?').unwrap_or((t, ""));
        let mut kept = Vec::new();
        for kv in query.split('&').filter(|kv| !kv.is_empty()) {
            match kv.split_once('=') {
                Some((k, v)) if k.eq_ignore_ascii_case("sslmode") => sslmode = Some(v.to_ascii_lowercase()),
                Some((k, v)) if k.eq_ignore_ascii_case("sslrootcert") => rootcert = Some(super::pgconn::percent_decode(v)),
                // libpq options this client has no use for (certificates are checked against the
                // web's roots).
                Some((k, _)) if IGNORED.iter().any(|i| k.eq_ignore_ascii_case(i)) => {}
                _ => kept.push(kv),
            }
        }
        let uri = if kept.is_empty() { base.to_string() } else { format!("{base}?{}", kept.join("&")) };
        uri.parse::<postgres::Config>().map_err(bad)?
    } else {
        let mut c = postgres::Config::new();
        for (k, v) in super::pgconn::keyword_pairs(t) {
            match k.to_ascii_lowercase().as_str() {
                "host" => {
                    for h in v.split(',').filter(|h| !h.is_empty()) {
                        c.host(h);
                    }
                }
                "hostaddr" => {
                    for h in v.split(',').filter_map(|h| h.trim().parse().ok()) {
                        c.hostaddr(h);
                    }
                }
                "port" => {
                    // An empty one is the default port, as in libpq.
                    for p in v.split(',').map(str::trim) {
                        if p.is_empty() {
                            c.port(5432);
                            continue;
                        }
                        c.port(p.parse().map_err(|_| DataError::Format(format!("the connection: port '{p}'")))?);
                    }
                }
                "dbname" => {
                    c.dbname(&v);
                }
                "user" => {
                    c.user(&v);
                }
                "password" => {
                    c.password(v);
                }
                "options" => {
                    c.options(&v);
                }
                "application_name" => {
                    c.application_name(&v);
                }
                "connect_timeout" => {
                    // As libpq: zero or less is no limit of its own, so the default applies.
                    if let Ok(s) = v.trim().parse::<i64>()
                        && s > 0
                    {
                        c.connect_timeout(Duration::from_secs(s.unsigned_abs()));
                    }
                }
                "sslmode" => sslmode = Some(v.to_ascii_lowercase()),
                "sslrootcert" => rootcert = Some(v),
                _ => {}
            }
        }
        c
    };
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    if config.get_hosts().is_empty() && config.get_hostaddrs().is_empty() {
        config.host(&env("PGHOST").unwrap_or_else(|| "localhost".into()));
    }
    if config.get_ports().is_empty()
        && let Some(p) = env("PGPORT").and_then(|p| p.parse().ok())
    {
        config.port(p);
    }
    if config.get_user().is_none() {
        let user = env("PGUSER").or_else(|| env("USER")).or_else(|| env("USERNAME")).unwrap_or_else(|| "postgres".into());
        config.user(&user);
    }
    if config.get_dbname().is_none()
        && let Some(d) = env("PGDATABASE")
    {
        config.dbname(&d);
    }
    if config.get_connect_timeout().is_none() {
        config.connect_timeout(CONNECT_TIMEOUT);
    }
    if config.get_application_name().is_none() {
        config.application_name("RustGIS");
    }
    let mode = sslmode.or_else(|| env("PGSSLMODE").map(|m| m.to_ascii_lowercase()));
    let tls = match mode.as_deref() {
        Some("disable") => Tls::Disable,
        // The certificate unchecked, as libpq. For `allow` libpq tries without TLS first, then
        // with it; here TLS first, then without (`prefer`), so a server that only takes TLS
        // connects too.
        Some("allow" | "require" | "prefer") | None => Tls::Unverified,
        // `verify-ca` checks the host name too, stricter than libpq. libpq's
        // `sslrootcert=system` is the system's roots: the web's, here.
        Some("verify-ca") | Some("verify-full") => Tls::Verified(
            rootcert.or_else(|| env("PGSSLROOTCERT")).filter(|r| !r.is_empty() && r != "system").map(std::path::PathBuf::from),
        ),
        Some(other) => return Err(DataError::Format(format!("the connection: sslmode '{other}'"))),
    };
    config.ssl_mode(match (&tls, mode.as_deref()) {
        (Tls::Disable, _) => postgres::config::SslMode::Disable,
        (_, Some("prefer" | "allow") | None) => postgres::config::SslMode::Prefer,
        _ => postgres::config::SslMode::Require,
    });
    Ok((config, tls))
}

/// libpq connection options a `postgresql://` address may carry that this client leaves out.
const IGNORED: &[&str] = &["sslcert", "sslkey", "sslcrl", "gssencmode", "sslcompression", "passfile", "sslpassword"];

/// Where servers usually put their socket: a password file names a connection through one as
/// `localhost`.
#[cfg(unix)]
const DEFAULT_SOCKET_DIRS: &[&str] = &["/var/run/postgresql", "/run/postgresql", "/tmp"];

/// The password for a connection in libpq's password file (`PGPASSFILE`, else `~/.pgpass`, or
/// `%APPDATA%\postgresql\pgpass.conf`), which libpq ignores when others can read it.
fn pgpass(config: &postgres::Config) -> Option<String> {
    let path = std::env::var_os("PGPASSFILE").map(std::path::PathBuf::from).or_else(|| {
        if cfg!(windows) {
            std::env::var_os("APPDATA").map(|a| std::path::Path::new(&a).join("postgresql").join("pgpass.conf"))
        } else {
            std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".pgpass"))
        }
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if std::fs::metadata(&path).ok()?.permissions().mode() & 0o077 != 0 {
            return None;
        }
    }
    let text = std::fs::read_to_string(&path).ok()?;
    let host = pgpass_host(config);
    let port = config.get_ports().first().copied().unwrap_or(5432).to_string();
    let user = config.get_user().unwrap_or_default().to_string();
    let db = config.get_dbname().map(str::to_string).unwrap_or_else(|| user.clone());
    pgpass_find(&text, &[&host, &port, &db, &user])
}

/// The host a password file names a connection by.
fn pgpass_host(config: &postgres::Config) -> String {
    // As libpq: the host, else the address, else `localhost`; a socket in the default directory
    // is `localhost`, another its directory.
    match config.get_hosts().first() {
        Some(postgres::config::Host::Tcp(h)) => h.clone(),
        #[cfg(unix)]
        Some(postgres::config::Host::Unix(dir)) if !DEFAULT_SOCKET_DIRS.iter().any(|d| dir == std::path::Path::new(d)) => {
            dir.display().to_string()
        }
        #[cfg(unix)]
        Some(postgres::config::Host::Unix(_)) => "localhost".into(),
        None => config.get_hostaddrs().first().map_or_else(|| "localhost".into(), ToString::to_string),
    }
}

/// The password of the first line of a password file matching `want` (host, port, database,
/// user; `*` matches anything, `\:` and `\\` escape).
fn pgpass_find(text: &str, want: &[&str; 4]) -> Option<String> {
    for line in text.lines() {
        if line.trim_start().starts_with('#') {
            continue;
        }
        let mut fields = Vec::new();
        let mut cur = String::new();
        let mut chars = line.chars();
        while let Some(c) = chars.next() {
            match c {
                '\\' => cur.extend(chars.next()),
                ':' if fields.len() < 4 => fields.push(std::mem::take(&mut cur)),
                c => cur.push(c),
            }
        }
        fields.push(cur);
        if fields.len() != 5 {
            continue;
        }
        if fields.iter().zip(want).all(|(f, w)| f == "*" || f == w) {
            return fields.pop();
        }
    }
    None
}

// ---------------------------------------------------------------------------------------------
// Tables

/// The geometry and geography columns of a database's tables and views.
pub fn tables(conn: &Conn) -> Result<Vec<Table>> {
    let sql = "SELECT f_table_schema::text, f_table_name::text, f_geometry_column::text, srid, type::text, false \
               FROM geometry_columns \
               UNION ALL \
               SELECT f_table_schema::text, f_table_name::text, f_geography_column::text, srid, type::text, true \
               FROM geography_columns \
               ORDER BY 1, 2, 3";
    let rows = conn.with(|c| c.query(sql, &[])).map_err(|e| match e {
        DataError::Format(m) if m.contains("geometry_columns") => {
            DataError::Format("the database has no PostGIS (CREATE EXTENSION postgis adds it)".into())
        }
        e => e,
    })?;
    let mut out: Vec<Table> = rows
        .iter()
        .filter_map(|r| {
            Some(Table {
                schema: r.try_get(0).ok()?,
                name: r.try_get(1).ok()?,
                column: r.try_get(2).ok()?,
                srid: r.try_get(3).ok()?,
                kind: r.try_get(4).ok()?,
                geography: r.try_get(5).ok()?,
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
    // A name exactly as listed first, then by its parts.
    all.iter()
        .find(|t| t.label().eq_ignore_ascii_case(name))
        .or_else(|| all.iter().find(|t| t.is_named(name)))
        .cloned()
        .ok_or_else(|| DataError::Format(format!("no table '{name}' with a geometry column")))
}

/// The geometry families a table holds: its declared type's, or for a `GEOMETRY` column those its
/// rows have (a layer each).
pub fn families(conn: &Conn, table: &Table) -> Result<Vec<Family>> {
    if let Some(f) = table.declared_family() {
        return Ok(vec![f]);
    }
    let g = table.geom();
    let from = table.sql_from();
    let filter = format!("{g} IS NOT NULL AND NOT ST_IsEmpty({g})");
    let sql = format!("SELECT DISTINCT ST_Dimension({g}) FROM {from} WHERE {filter}");
    // A scan of the table, bounded like the other probes. Past that, the rows may hold any type:
    // a layer each, those without rows left out when they open (by their count). Other errors
    // (no permission, a dropped connection) are the table's.
    let rows = conn.snapshot(Some(PROBE_TIMEOUT), |t| match t.query(sql.as_str(), &[]) {
        Err(e) if e.code() == Some(&postgres::error::SqlState::QUERY_CANCELED) => Ok(None),
        r => r.map(Some),
    })?;
    let Some(rows) = rows else {
        return Ok(vec![Family::Points, Family::Lines, Family::Polygons]);
    };
    let mut out: Vec<Family> = rows
        .iter()
        .filter_map(|r| r.try_get::<_, Option<i32>>(0).ok().flatten())
        .filter_map(|d| match d {
            0 => Some(Family::Points),
            1 => Some(Family::Lines),
            2 | 3 => Some(Family::Polygons),
            _ => None,
        })
        .collect();
    out.sort_by_key(|f| *f as u8);
    out.dedup();
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// The source

/// A table's rows read to be edited ([`PostgisSource::read_editable`]).
#[derive(Debug, Default)]
pub struct Editable {
    /// The rows, in key order; each feature's id is its key.
    pub features: Vec<Feature>,
    /// Each row's key and version (`xmin`), as read.
    pub versions: Vec<(u64, i64)>,
    /// The rows the condition keeps, of which `features` holds the first.
    pub matched: u64,
    /// Whether this connection's user may insert, update and delete the table's rows.
    pub writable: bool,
    /// Values in date fields that weren't dates RustGIS holds (`infinity`, a year BC or past
    /// 9999), read as empty: their count for each field ([`super::unread_dates_note`]).
    pub unread_dates: Vec<usize>,
}

/// One geometry column of a table (one family of it, for a column of mixed types), read live.
pub struct PostgisSource {
    info: SourceInfo,
    conn: Arc<Conn>,
    table: Table,
    /// The family read, for a column of mixed types.
    family: Option<Family>,
    /// The SRID the geometry is in (the column's, the rows', or the one given).
    srid: i32,
    /// The SRID the column's values carry (0 when they say none).
    raw_srid: i32,
    /// The geometry, in its own coordinates.
    native: String,
    /// The table's single-column integer primary key, which gives the ids: rows can be told
    /// apart to be edited only with one.
    key: Option<String>,
    /// The expression of each column read: the id, the geometry (as WKB, longitude/latitude),
    /// then the fields.
    select: Vec<String>,
    /// Converts a request's extent to the table's coordinates, when RustGIS knows the coordinate
    /// system itself; else the server does.
    reproject: Option<Reproject>,
}

impl std::fmt::Debug for PostgisSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgisSource").field("table", &self.table).field("family", &self.family).finish_non_exhaustive()
    }
}

/// How a column of a PostgreSQL type is read: cast to a type RustGIS has, or as text. `None`
/// leaves it out (other geometry, rasters, binary).
fn field_cast(ty: &str) -> Option<(FieldType, &'static str)> {
    match ty {
        "int2" | "int4" | "int8" | "oid" => Some((FieldType::Integer, "int8")),
        "float4" | "float8" | "numeric" => Some((FieldType::Double, "float8")),
        // `money` has no cast to a number; as text it keeps its currency's format.
        "bool" => Some((FieldType::Boolean, "bool")),
        // Read as ISO text (the session's `DateStyle`), which a date field parses: a `timestamp`
        // without a zone stays its wall-clock time, a `timestamptz` comes in UTC.
        "date" => Some((FieldType::Date, "text")),
        "timestamp" | "timestamptz" => Some((FieldType::DateTime, "text")),
        "geometry" | "geography" | "raster" | "bytea" | "box2d" | "box3d" | "tsvector" => None,
        _ => Some((FieldType::Text, "text")),
    }
}

impl PostgisSource {
    /// Opens a table's geometry column (`family`: the one family to read of a column of mixed
    /// types). `crs` (`EPSG:…`) is the coordinate system of a geometry that doesn't say its own.
    pub fn open(conn: Arc<Conn>, table: Table, family: Option<Family>, crs: Option<&str>) -> Result<Self> {
        let mut notes = Vec::new();
        let g = table.geom();
        // The SRID: the column's, else its rows', else the one given.
        let mut srid = if table.geography && table.srid == 0 { 4326 } else { table.srid };
        if srid == 0 {
            let sql = format!("SELECT ST_SRID({g}) FROM {} WHERE {g} IS NOT NULL LIMIT 1", table.sql_from());
            srid = conn.with(|c| c.query_opt(sql.as_str(), &[]))?.and_then(|r| r.try_get::<_, i32>(0).ok()).unwrap_or(0);
        }
        let raw_srid = srid;
        if table.srid == 0 && !table.geography && raw_srid != 0 {
            notes.push(format!("the column doesn't fix a coordinate system: rows in one other than EPSG:{raw_srid} are left out"));
        }
        let given = crs.map(str::trim).filter(|c| !c.is_empty());
        let mut native = g.clone();
        if srid == 0 {
            match given {
                Some(given) => {
                    let (code, note) = super::given_epsg(given)?;
                    srid = i32::from(code);
                    native = format!("ST_SetSRID({g}, {srid})");
                    notes.extend(note);
                }
                None => notes.push(
                    "the geometry doesn't say its coordinate system; read as longitude/latitude (add it again with \"crs\", such as EPSG:2263)".into(),
                ),
            }
        }
        if let Some(note) = tls_note(&conn.tls, conn.is_plain()) {
            notes.push(note.into());
        }
        let lonlat = if srid == 0 || srid == 4326 { native.clone() } else { format!("ST_Transform({native}, 4326)") };
        // Curves become lines; Z and M are dropped by the reader (opening notes them, below).
        let wkb = format!("ST_AsBinary(ST_CurveToLine({lonlat}))");

        // The columns: a single integer primary key gives the ids.
        let rel = format!("{}.{}", ident(&table.schema), ident(&table.name));
        let cols = conn.with(|c| {
            c.query(
                "SELECT a.attname::text, t.typname::text, COALESCE(i.indisprimary, false), c.relkind::text \
                 FROM pg_attribute a \
                 JOIN pg_class c ON c.oid = a.attrelid \
                 JOIN pg_type t ON t.oid = a.atttypid \
                 LEFT JOIN pg_index i ON i.indrelid = a.attrelid AND i.indisprimary AND i.indnatts = 1 AND a.attnum = i.indkey[0] \
                 WHERE a.attrelid = $1::text::regclass AND a.attnum > 0 AND NOT a.attisdropped \
                 ORDER BY a.attnum",
                &[&rel],
            )
        })?;
        let mut fields = Vec::new();
        let mut casts = Vec::new();
        let mut pk = None;
        let mut relkind = String::new();
        for r in &cols {
            let (Ok(name), Ok(ty), Ok(is_pk), Ok(kind)) =
                (r.try_get::<_, String>(0), r.try_get::<_, String>(1), r.try_get::<_, bool>(2), r.try_get::<_, String>(3))
            else {
                continue;
            };
            relkind = kind;
            if is_pk && matches!(ty.as_str(), "int2" | "int4" | "int8") {
                pk = Some(name.clone());
            }
            if name == table.column {
                continue;
            }
            if let Some((ft, cast)) = field_cast(&ty) {
                casts.push(format!("{}::{cast}", ident(&name)));
                fields.push(Field::new(name, ft));
            }
        }
        // Ids stable between reads: the key, else the row's place in the table (`ctid`, which an
        // `UPDATE`, `VACUUM FULL` or `CLUSTER` changes); a view has neither, and a partitioned table's `ctid`s repeat across its partitions, so their
        // rows are numbered as read.
        let id = match (&pk, relkind.as_str()) {
            (Some(k), _) => format!("{}::int8", ident(k)),
            (None, "r" | "m") => "(((ctid::text::point)[0])::int8 * 65536 + ((ctid::text::point)[1])::int8)".to_string(),
            _ => "(row_number() OVER ())::int8".to_string(),
        };
        if pk.is_none() && !matches!(relkind.as_str(), "r" | "m") {
            notes.push("a view or partitioned table without a single-column key: its features are numbered as read, so their ids can change from one read to the next".into());
        } else if pk.is_none() {
            notes.push(
                "no single-column integer key: features are numbered by their place in the table, which changes when rows are updated"
                    .into(),
            );
        }
        let mut select = vec![id, wkb];
        select.extend(casts);
        let key = pk;

        let family = family.or(table.declared_family());
        let reproject = match srid {
            0 | 4326 => Some(Reproject::from_crs(Crs::LonLat, &mut Vec::new())),
            // A coordinate system RustGIS can't convert itself is left to the server.
            s => u32::try_from(s).ok().and_then(Crs::from_epsg).map(|c| Reproject::from_crs(c, &mut Vec::new())).filter(|r| !r.is_lonlat()),
        };
        let mut src = PostgisSource {
            info: SourceInfo {
                name: table.label(),
                format: "PostGIS",
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
            srid,
            raw_srid,
            native,
            key,
            select,
            reproject,
        };
        src.info.indexed = src.has_index();
        src.info.count = src.count_rows();
        src.info.extent = src.extent();
        // A column declared with Z or M, or (an unconstrained one) Z or M in the first rows this layer
        // reads, for a layer with a row to read: an empty one (or one its filter leaves empty, which an
        // estimated count can't tell) loses nothing. A check that fails (or times out) is said too.
        let (from, filter) = (src.table.sql_from(), src.filter(false));
        let sql = format!(
            "SELECT EXISTS (SELECT 1 FROM {from} WHERE {filter}) AND (COALESCE((SELECT coord_dimension FROM geometry_columns WHERE f_table_schema = $1 AND f_table_name = $2 AND f_geometry_column = $3 \
             UNION ALL SELECT coord_dimension FROM geography_columns WHERE f_table_schema = $1 AND f_table_name = $2 AND f_geography_column = $3 \
             LIMIT 1), 2) > 2 \
             OR COALESCE((SELECT bool_or(ST_NDims(g::geometry) > 2) FROM (SELECT {} AS g FROM {from} WHERE {filter} LIMIT 16) AS s), false))",
            src.table.geom(),
        );
        let zm = if src.info.count == Some(0) {
            Some(false)
        } else {
            let t = &src.table;
            let row = src.conn.snapshot(Some(PROBE_TIMEOUT), |c| c.query_one(sql.as_str(), &[&t.schema, &t.name, &t.column]));
            row.ok().and_then(|r| r.try_get::<_, bool>(0).ok())
        };
        match zm {
            Some(true) => src.info.drop_zm(None),
            Some(false) => {}
            None => src.info.notes.push("couldn't tell whether it has Z or M values, which RustGIS doesn't keep".into()),
        }
        Ok(src)
    }

    /// The table's single-column integer primary key, if it has one.
    pub fn key(&self) -> Option<&str> {
        self.key.as_deref()
    }

    /// The SRID of the geometry read (0 when it says none).
    pub fn srid(&self) -> i32 {
        self.srid
    }

    /// The table and geometry column read.
    pub fn table(&self) -> &Table {
        &self.table
    }

    /// Reads the rows to edit: at most `limit` of them (and [`MAX_EDITABLE`]) in key order, those
    /// `condition` (an SQL expression on the table's columns) keeps, each with its version
    /// (`xmin`), which saving checks the database against. One snapshot, read only: the
    /// condition can't change anything. Needs the table's integer primary key.
    pub fn read_editable(&self, condition: Option<&str>, limit: usize) -> Result<Editable> {
        let key = self.key.as_deref().ok_or_else(|| {
            DataError::Format(format!(
                "{}: no single-column integer primary key, so its rows can't be told apart to edit",
                self.table.label()
            ))
        })?;
        let limit = limit.clamp(1, MAX_EDITABLE);
        let from = self.table.sql_from();
        let mut filter = self.filter(false);
        if let Some(c) = condition.map(str::trim).filter(|c| !c.is_empty()) {
            if c.contains(';') {
                return Err(DataError::Format("the condition: one SQL expression, without ';'".into()));
            }
            filter = format!("{filter} AND ({c})");
        }
        let version = self.select.len();
        let mut cols: Vec<String> = self.select.iter().enumerate().map(|(i, e)| format!("{e} AS c{i}")).collect();
        cols.push(format!("xmin::text::int8 AS c{version}"));
        let sql = format!("SELECT {} FROM {from} WHERE {filter} ORDER BY {} LIMIT {limit}", cols.join(", "), ident(key));
        let rel = format!("{}.{}", ident(&self.table.schema), ident(&self.table.name));
        let (matched, rows, writable) = self.conn.snapshot(None, |t| {
            let n: i64 = t.query_one(format!("SELECT count(*) FROM {from} WHERE {filter}").as_str(), &[])?.try_get(0)?;
            let writable: bool = t
                .query_one(
                    "SELECT has_table_privilege($1::text::regclass, 'INSERT') AND has_table_privilege($1::text::regclass, 'UPDATE')                      AND has_table_privilege($1::text::regclass, 'DELETE')",
                    &[&rel],
                )?
                .try_get(0)?;
            Ok((u64::try_from(n).unwrap_or(0), t.query(sql.as_str(), &[])?, writable))
        })?;
        let mut out = Editable {
            features: Vec::with_capacity(rows.len()),
            versions: Vec::with_capacity(rows.len()),
            matched,
            writable,
            unread_dates: Vec::new(),
        };
        for r in &rows {
            // Dates that aren't ones are counted in the rows kept.
            let mut unread = Vec::new();
            let (Some(f), Some(v)) = (self.feature(r, &mut unread), r.try_get::<_, Option<i64>>(version).ok().flatten()) else {
                continue;
            };
            super::add_unread_dates(&mut out.unread_dates, &unread);
            out.versions.push((f.id, v));
            out.features.push(f);
        }
        Ok(out)
    }

    /// A row read as a feature: the id, the geometry (WKB) and the fields, in the order of
    /// `select`. `None` without an id or a geometry RustGIS reads. A value of a date field that
    /// isn't a date RustGIS holds is null, counted in `unread` ([`Batch::unread_dates`]).
    fn feature(&self, r: &postgres::Row, unread: &mut Vec<usize>) -> Option<Feature> {
        let id = r.try_get::<_, Option<i64>>(0).ok().flatten()?;
        let geometry = r.try_get::<_, Option<Vec<u8>>>(1).ok().flatten().and_then(|b| wkb::decode(&b))?;
        let attrs = self
            .info
            .fields
            .iter()
            .enumerate()
            .map(|(k, f)| {
                let i = k + 2;
                match f.ty {
                    FieldType::Integer => r.try_get::<_, Option<i64>>(i).ok().flatten().map(Value::Int),
                    FieldType::Double => r.try_get::<_, Option<f64>>(i).ok().flatten().map(Value::Float),
                    FieldType::Boolean => r.try_get::<_, Option<bool>>(i).ok().flatten().map(Value::Bool),
                    FieldType::Text => r.try_get::<_, Option<String>>(i).ok().flatten().map(Value::Text),
                    FieldType::Date | FieldType::DateTime => {
                        let text = r.try_get::<_, Option<String>>(i).ok().flatten();
                        Some(super::server_date(text.as_deref(), f.ty, unread, k))
                    }
                }
                .unwrap_or(Value::Null)
            })
            .collect();
        // A negative key keeps its bits: still one id per row, and the same on every read.
        Some(Feature { id: id.cast_unsigned(), geometry: Some(geometry), attrs })
    }

    /// The table's spatial index on the column, if it has one (a view has none of its own).
    fn has_index(&self) -> bool {
        let rel = format!("{}.{}", ident(&self.table.schema), ident(&self.table.name));
        self.conn
            .with(|c| {
                c.query_opt(
                    "SELECT 1 FROM pg_index i JOIN pg_class x ON x.oid = i.indexrelid JOIN pg_am am ON am.oid = x.relam \
                     JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = ANY(i.indkey) \
                     WHERE i.indrelid = $1::text::regclass AND am.amname IN ('gist', 'spgist', 'brin') AND a.attname = $2 LIMIT 1",
                    &[&rel, &self.table.column],
                )
            })
            .ok()
            .flatten()
            .is_some()
    }

    /// The rows with a geometry (of the family read), exactly unless the table is large.
    fn count_rows(&self) -> Option<u64> {
        let rel = format!("{}.{}", ident(&self.table.schema), ident(&self.table.name));
        let estimate = self
            .conn
            .with(|c| c.query_opt("SELECT reltuples::float8 FROM pg_class WHERE oid = $1::text::regclass", &[&rel]))
            .ok()
            .flatten()
            .and_then(|r| r.try_get::<_, f64>(0).ok());
        match estimate {
            Some(n) if n >= EXACT_COUNT_BELOW => Some(n as u64),
            // No estimate (`-1`: never analyzed) or a small one: counted, within the probe's time.
            _ => {
                let sql = format!("SELECT count(*) FROM {} WHERE {}", self.table.sql_from(), self.filter(false));
                self.conn
                    .snapshot(Some(PROBE_TIMEOUT), |t| t.query_one(sql.as_str(), &[]))
                    .ok()
                    .and_then(|r| r.try_get::<_, i64>(0).ok())
                    .map(|n| n.max(0) as u64)
            }
        }
    }

    /// The table's extent in longitude/latitude: the planner's estimate when it has one, else
    /// measured (only for a table that isn't large).
    fn extent(&self) -> Extent {
        let t = &self.table;
        let estimated = if t.geography {
            None
        } else {
            self.conn
                .with(|c| {
                    c.query_opt(
                        "SELECT ST_XMin(b), ST_YMin(b), ST_XMax(b), ST_YMax(b) FROM (SELECT ST_EstimatedExtent($1, $2, $3) AS b) s WHERE b IS NOT NULL",
                        &[&t.schema, &t.name, &t.column],
                    )
                })
                .ok()
                .flatten()
                .and_then(|r| row_extent(&r))
        };
        let native = estimated.or_else(|| {
            if self.info.count.is_none_or(|n| n as f64 >= EXACT_COUNT_BELOW) {
                return None;
            }
            let g = &self.native;
            let sql = format!(
                "SELECT ST_XMin(b), ST_YMin(b), ST_XMax(b), ST_YMax(b) FROM (SELECT ST_Extent({g}) AS b FROM {} WHERE {}) s WHERE b IS NOT NULL",
                t.sql_from(),
                self.filter(false)
            );
            self.conn.snapshot(Some(PROBE_TIMEOUT), |t| t.query_opt(sql.as_str(), &[])).ok().flatten().and_then(|r| row_extent(&r))
        });
        native.map(|e| self.to_lonlat(&e)).unwrap_or(Extent::EMPTY)
    }

    /// A native extent in longitude/latitude.
    fn to_lonlat(&self, e: &Extent) -> Extent {
        if let Some(r) = &self.reproject {
            return r.extent_to_lonlat(e);
        }
        self.server_transform(e, self.srid, 4326).unwrap_or(Extent::EMPTY)
    }

    /// A longitude/latitude extent in the table's coordinates; a refused box is an error,
    /// so sampling cannot discard view matches before local filtering.
    fn lonlat_to_native(&self, e: &Extent) -> Result<Extent> {
        if let Some(r) = &self.reproject {
            return r.database_extent(e);
        }
        let c = Extent::new(e.min_x.max(-180.0), e.min_y.max(-89.9), e.max_x.min(180.0), e.max_y.min(89.9));
        self.server_transform(&c, 4326, self.srid).ok_or_else(super::unbounded_database_view)
    }

    /// An extent converted by the server: its edges, densified, transformed.
    fn server_transform(&self, e: &Extent, from: i32, to: i32) -> Option<Extent> {
        if e.is_empty() {
            return None;
        }
        let step = (e.width().max(e.height()) / 16.0).max(1e-9);
        self.conn
            .with(|c| {
                c.query_opt(
                    "SELECT ST_XMin(b), ST_YMin(b), ST_XMax(b), ST_YMax(b) FROM \
                     (SELECT ST_Transform(ST_Segmentize(ST_MakeEnvelope($1, $2, $3, $4, $5), $6), $7)::box2d AS b) s",
                    &[&e.min_x, &e.min_y, &e.max_x, &e.max_y, &from, &step, &to],
                )
            })
            .ok()
            .flatten()
            .and_then(|r| row_extent(&r))
    }

    /// The `WHERE` condition: a geometry, of the family read, and with `in_extent`, in the extent
    /// given as `$1`–`$4` (in the column's coordinates).
    fn filter(&self, in_extent: bool) -> String {
        let g = self.table.geom();
        let mut w = format!("{g} IS NOT NULL AND NOT ST_IsEmpty({g})");
        // A column of no fixed SRID: only the rows in the one read (others would fail the read).
        if self.table.srid == 0 && !self.table.geography && self.raw_srid != 0 {
            w.push_str(&format!(" AND ST_SRID({g}) = {}", self.raw_srid));
        }
        if in_extent {
            let col = ident(&self.table.column);
            // The column itself, so its index serves (a geography's in longitude/latitude).
            if self.table.geography {
                w.push_str(&format!(" AND {col} && ST_MakeEnvelope($1, $2, $3, $4, 4326)::geography"));
            } else {
                w.push_str(&format!(" AND {col} && ST_MakeEnvelope($1, $2, $3, $4, {})", self.raw_srid));
            }
        }
        if let Some(f) = self.family.filter(|_| self.table.declared_family().is_none()) {
            let dims = match f {
                Family::Points => "0",
                Family::Lines => "1",
                Family::Polygons => "2, 3",
            };
            w.push_str(&format!(" AND ST_Dimension({g}) IN ({dims})"));
        }
        w
    }
}

// ---------------------------------------------------------------------------------------------
// Saving edits

/// A column a value is written to: the layer's field, its type in the table, and the field of
/// the rows as read that holds its value.
struct Target {
    field: usize,
    name: String,
    ty: String,
    read: usize,
}

/// A parameter of a statement.
type Param = Box<dyn ToSql + Sync>;

fn refs(params: &[Param]) -> Vec<&(dyn ToSql + Sync)> {
    params.iter().map(|p| p.as_ref() as &(dyn ToSql + Sync)).collect()
}

/// A value as text for the server to cast to its column's type (`NULL` for none).
fn text_value(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::Int(i) => Some(i.to_string()),
        Value::Float(x) if x.is_nan() => Some("NaN".into()),
        Value::Float(x) if x.is_infinite() => Some(if *x > 0.0 { "Infinity" } else { "-Infinity" }.into()),
        Value::Float(x) => Some(x.to_string()),
        Value::Text(t) => Some(t.clone()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Date(_) | Value::DateTime(_) => Some(v.to_string()),
    }
}

/// Counts a date-time written to a column (`column`, its type as `format_type` names it) that
/// doesn't keep what it says: a `timestamptz` holds the instant in UTC, so an offset isn't kept
/// ([`Saved::utc_dates`]) and a time without a zone is taken as UTC ([`Saved::assumed_utc`]); a
/// `timestamp` keeps the clock time, without the offset ([`Saved::zoneless_dates`]). Text in the
/// field counts when it reads as such a date-time.
fn count_offset(saved: &mut Saved, field: usize, column: &str, v: &Value) {
    let t = match v {
        Value::DateTime(t) => Some(*t),
        // A date is stored at its midnight, without a zone.
        Value::Date(d) => Some(crate::date::DateTime::from_date(*d)),
        Value::Text(s) => crate::date::DateTime::parse(s),
        _ => None,
    };
    let Some(t) = t.filter(|_| column.starts_with("timestamp")) else { return };
    let counts = match (column.ends_with("without time zone"), t.offset()) {
        (true, Some(_)) => &mut saved.zoneless_dates,
        (false, Some(o)) if o != 0 => &mut saved.utc_dates,
        (false, None) => &mut saved.assumed_utc,
        _ => return,
    };
    if counts.len() <= field {
        counts.resize(field + 1, 0);
    }
    if let Some(n) = counts.get_mut(field) {
        *n = n.saturating_add(1);
    }
}

/// What saving to `table` says of the date-times its columns didn't keep as they were
/// ([`Saved::utc_dates`], [`Saved::zoneless_dates`], [`Saved::assumed_utc`]), naming the fields
/// of `fields` they were in.
pub fn saved_dates_notes(fields: &[Field], saved: &Saved, table: &str) -> Vec<String> {
    let table: String = table.chars().map(crate::io::dbf_notice_char).collect();
    let kinds: [(&[usize], &str, &str); 3] = [
        (&saved.utc_dates, "1 date-time was saved in UTC, without its offset", "date-times were saved in UTC, without their offsets"),
        (
            &saved.zoneless_dates,
            "1 date-time was saved at the clock time shown, without its offset",
            "date-times were saved at the clock time shown, without their offsets",
        ),
        (&saved.assumed_utc, "1 date-time without a time zone was saved as UTC", "date-times without a time zone were saved as UTC"),
    ];
    let why =
        ["(a timestamptz column holds only the moment)", "(a timestamp column has no time zone)", "(a timestamptz column holds a moment)"];
    kinds
        .iter()
        .zip(why)
        .filter_map(|((counts, one, many), why)| {
            let total: usize = counts.iter().sum();
            let what = if total == 1 { (*one).to_string() } else { format!("{total} {many}") };
            (total > 0).then(|| format!("{table}: {what} {why}, in {}", crate::io::named_fields(fields, counts)))
        })
        .collect()
}

impl PostgisSource {
    /// The geometry of parameter `$n` (WKB in longitude/latitude) as the column holds it: in its
    /// coordinate system, its single or multi type, and its dimensions.
    fn geometry_in(&self, n: usize, dims: i32) -> String {
        let t = &self.table;
        let mut e = format!("ST_SetSRID(ST_GeomFromWKB(${n}::bytea), 4326)");
        if !t.geography {
            if self.srid != 0 && self.srid != 4326 {
                e = format!("ST_Transform({e}, {})", self.srid);
            }
            // A column without a coordinate system of its own: the rows' (or none).
            if self.raw_srid != self.srid || self.srid == 0 {
                e = format!("ST_SetSRID({e}, {})", self.raw_srid);
            }
        }
        let kind = t.kind.to_ascii_uppercase();
        let has_m = kind.ends_with('M') && !kind.ends_with("ZM") && dims == 3;
        let base = kind.trim_end_matches(['Z', 'M']);
        // RustGIS holds lines and polygons as multi-part: one part goes in a single-type column as
        // that type, more fail there as the server says. A column of any type keeps what it holds.
        if base.starts_with("MULTI") {
            e = format!("ST_Multi({e})");
        } else if base != "GEOMETRYCOLLECTION" && base != "GEOMETRY" {
            e = format!("(SELECT CASE WHEN ST_NumGeometries(g) = 1 THEN ST_GeometryN(g, 1) ELSE g END FROM (SELECT {e} AS g) s)");
        }
        e = match dims {
            4 => format!("ST_Force4D({e})"),
            3 if has_m => format!("ST_Force3DM({e})"),
            3 => format!("ST_Force3DZ({e})"),
            _ => e,
        };
        if t.geography { format!("({e})::geography") } else { e }
    }

    /// Saves edits to the table in one transaction: every row changed or deleted is locked and
    /// must be at the version it was read at, and every new row's key free; otherwise nothing is
    /// saved and the conflicts are the answer. Only the columns whose values changed are
    /// written (a value read through a cast, such as a `numeric` as a number, is left as it is
    /// unless it changed). `fields` are the layer's, matched to the table's columns by name.
    /// Needs a connection from [`connect_writable`].
    pub fn save(&self, edits: &Edits, fields: &[Field]) -> Result<std::result::Result<Saved, Vec<Conflict>>> {
        let key = self.key.as_deref().ok_or_else(|| {
            DataError::Format(format!(
                "{}: no single-column integer primary key, so its rows can't be told apart to save",
                self.table.label()
            ))
        })?;
        if !self.conn.writable {
            return Err(DataError::Format("this connection only reads".into()));
        }
        let k = ident(key);
        let from = self.table.sql_from();
        let rel = format!("{}.{}", ident(&self.table.schema), ident(&self.table.name));
        let column = ident(&self.table.column);
        let key_field = fields.iter().position(|f| f.name == key);
        self.conn.once(|c| {
            let mut t = c.build_transaction().isolation_level(postgres::IsolationLevel::ReadCommitted).start()?;
            // The table's columns: their types, whether they are computed, and whether they have
            // a default.
            let columns: std::collections::HashMap<String, (String, bool, bool)> = t
                .query(
                    "SELECT a.attname::text, format_type(a.atttypid, a.atttypmod), \
                     COALESCE(to_jsonb(a) ->> 'attgenerated', '') <> '', \
                     a.atthasdef OR COALESCE(to_jsonb(a) ->> 'attidentity', '') <> '' \
                     FROM pg_attribute a WHERE a.attrelid = $1::text::regclass AND a.attnum > 0 AND NOT a.attisdropped",
                    &[&rel],
                )?
                .iter()
                .filter_map(|r| Some((r.try_get(0).ok()?, (r.try_get(1).ok()?, r.try_get(2).ok()?, r.try_get(3).ok()?))))
                .collect();
            let key_default = columns.get(key).is_some_and(|c| c.2);
            let mut saved = Saved::default();
            let mut targets = Vec::new();
            for (i, f) in fields.iter().enumerate() {
                if f.name == key || f.name == self.table.column {
                    continue;
                }
                match (columns.get(&f.name), self.info.fields.iter().position(|g| g.name == f.name)) {
                    (Some((ty, false, _)), Some(read)) => targets.push(Target { field: i, name: f.name.clone(), ty: ty.clone(), read }),
                    // Computed by the database.
                    (Some((_, true, _)), _) => {}
                    _ => saved.skipped_fields.push(f.name.clone()),
                }
            }
            let dims: i32 = t
                .query_opt(
                    "SELECT coord_dimension FROM geometry_columns WHERE f_table_schema = $1 AND f_table_name = $2 AND f_geometry_column = $3 \
                     UNION ALL SELECT coord_dimension FROM geography_columns WHERE f_table_schema = $1 AND f_table_name = $2 AND f_geography_column = $3 \
                     LIMIT 1",
                    &[&self.table.schema, &self.table.name, &self.table.column],
                )?
                .and_then(|r| r.try_get(0).ok())
                .unwrap_or(2);

            // The rows read: locked, and still as they were.
            let read: Vec<i64> =
                edits.updates.iter().map(|(f, _)| f.id.cast_signed()).chain(edits.deletes.iter().map(|(k, _)| k.cast_signed())).collect();
            let now: std::collections::HashMap<i64, i64> = t
                .query(format!("SELECT {k}::int8, xmin::text::int8 FROM {from} WHERE {k} = ANY($1::int8[]) FOR UPDATE").as_str(), &[&read])?
                .iter()
                .filter_map(|r| Some((r.try_get(0).ok()?, r.try_get(1).ok()?)))
                .collect();
            let mut conflicts = Vec::new();
            for (id, version) in edits.updates.iter().map(|(f, v)| (f.id, *v)).chain(edits.deletes.iter().copied()) {
                match now.get(&id.cast_signed()) {
                    None => conflicts.push(Conflict::Deleted(id)),
                    Some(v) if *v != version => conflicts.push(Conflict::Changed(id)),
                    Some(_) => {}
                }
            }
            // New rows' keys: free, and each once.
            let new_key = |f: &Feature| -> Option<i64> {
                match key_field.and_then(|i| f.attrs.get(i)) {
                    Some(Value::Int(v)) => Some(*v),
                    _ if key_default => None,
                    _ => Some(f.id.cast_signed()),
                }
            };
            let given: Vec<i64> = edits.inserts.iter().filter_map(new_key).collect();
            let mut seen = std::collections::HashSet::new();
            for g in &given {
                if !seen.insert(*g) {
                    conflicts.push(Conflict::Exists(g.cast_unsigned()));
                }
            }
            for r in t.query(format!("SELECT {k}::int8 FROM {from} WHERE {k} = ANY($1::int8[])").as_str(), &[&given])? {
                if let Ok(g) = r.try_get::<_, i64>(0) {
                    conflicts.push(Conflict::Exists(g.cast_unsigned()));
                }
            }
            if !conflicts.is_empty() {
                // Dropped unsaved: rolled back, the locks let go.
                return Ok(Err(conflicts));
            }

            // Updates: the columns whose values differ from the rows as they are (as they were
            // read, since their versions match).
            let mut written: Vec<i64> = Vec::new();
            let ids: Vec<i64> = edits.updates.iter().map(|(f, _)| f.id.cast_signed()).collect();
            let cols: Vec<String> = self.select.iter().enumerate().map(|(i, e)| format!("{e} AS c{i}")).collect();
            let current: std::collections::HashMap<u64, Feature> = t
                .query(format!("SELECT {} FROM {from} WHERE {k} = ANY($1::int8[])", cols.join(", ")).as_str(), &[&ids])?
                .iter()
                .filter_map(|r| self.feature(r, &mut Vec::new()))
                .map(|f| (f.id, f))
                .collect();
            for (f, _) in &edits.updates {
                let was = current.get(&f.id);
                let mut sets = Vec::new();
                let mut params: Vec<Param> = Vec::new();
                for tg in &targets {
                    let value = f.attrs.get(tg.field).unwrap_or(&Value::Null);
                    if was.and_then(|w| w.attrs.get(tg.read)) == Some(value) {
                        continue;
                    }
                    count_offset(&mut saved, tg.field, &tg.ty, value);
                    params.push(Box::new(text_value(value)));
                    sets.push(format!("{} = CAST(${}::text AS {})", ident(&tg.name), params.len(), tg.ty));
                }
                if let Some(g) = &f.geometry
                    && was.and_then(|w| w.geometry.as_ref()) != Some(g)
                {
                    params.push(Box::new(wkb::encode(g)));
                    sets.push(format!("{column} = {}", self.geometry_in(params.len(), dims)));
                }
                if sets.is_empty() {
                    saved.unchanged += 1;
                    continue;
                }
                params.push(Box::new(f.id.cast_signed()));
                let sql = format!("UPDATE {from} SET {} WHERE {k} = ${}::int8", sets.join(", "), params.len());
                t.execute(sql.as_str(), &refs(&params))?;
                saved.updated += 1;
                written.push(f.id.cast_signed());
            }

            // New rows: their values, the defaults for the rest.
            for f in &edits.inserts {
                let (mut names, mut values, mut params): (Vec<String>, Vec<String>, Vec<Param>) = Default::default();
                if let Some(v) = new_key(f) {
                    params.push(Box::new(v));
                    names.push(k.clone());
                    values.push(format!("${}::int8", params.len()));
                }
                for tg in &targets {
                    let value = f.attrs.get(tg.field).unwrap_or(&Value::Null);
                    if *value == Value::Null {
                        continue;
                    }
                    count_offset(&mut saved, tg.field, &tg.ty, value);
                    params.push(Box::new(text_value(value)));
                    names.push(ident(&tg.name));
                    values.push(format!("CAST(${}::text AS {})", params.len(), tg.ty));
                }
                if let Some(g) = &f.geometry {
                    params.push(Box::new(wkb::encode(g)));
                    names.push(column.clone());
                    values.push(self.geometry_in(params.len(), dims));
                }
                let sql = if names.is_empty() {
                    format!("INSERT INTO {from} DEFAULT VALUES RETURNING {k}::int8")
                } else {
                    format!("INSERT INTO {from} ({}) VALUES ({}) RETURNING {k}::int8", names.join(", "), values.join(", "))
                };
                let id: i64 = t.query_one(sql.as_str(), &refs(&params))?.try_get(0)?;
                saved.inserted.push((f.id, id.cast_unsigned()));
                written.push(id);
            }

            let gone: Vec<i64> = edits.deletes.iter().map(|(k, _)| k.cast_signed()).collect();
            if !gone.is_empty() {
                saved.deleted = usize::try_from(t.execute(format!("DELETE FROM {from} WHERE {k} = ANY($1::int8[])").as_str(), &[&gone])?).unwrap_or(0);
            }
            saved.versions = t
                .query(format!("SELECT {k}::int8, xmin::text::int8 FROM {from} WHERE {k} = ANY($1::int8[])").as_str(), &[&written])?
                .iter()
                .filter_map(|r| Some((r.try_get::<_, i64>(0).ok()?.cast_unsigned(), r.try_get(1).ok()?)))
                .collect();
            t.commit()?;
            Ok(Ok(saved))
        })
    }
}

fn row_extent(r: &postgres::Row) -> Option<Extent> {
    let v: Vec<f64> = (0..4).filter_map(|i| r.try_get::<_, Option<f64>>(i).ok().flatten()).collect();
    match v.as_slice() {
        [a, b, c, d] if v.iter().all(|x| x.is_finite()) => Some(Extent::new(*a, *b, *c, *d)),
        _ => None,
    }
}

impl FeatureSource for PostgisSource {
    fn info(&self) -> &SourceInfo {
        &self.info
    }

    /// Refuses an unbounded view before the server counts or samples the query.
    fn read(&self, req: &Request) -> Result<Batch> {
        let limit = clamp_limit(req.limit);
        let mut out = Batch::default();
        let native = req.extent.map(|e| self.lonlat_to_native(&e)).transpose()?;
        if limit == 0 {
            return Ok(out);
        }
        let filter = self.filter(native.is_some());
        let args: Vec<f64> = native.map(|e| vec![e.min_x, e.min_y, e.max_x, e.max_y]).unwrap_or_default();
        let params: Vec<&(dyn ToSql + Sync)> = args.iter().map(|a| a as &(dyn ToSql + Sync)).collect();
        let from = self.table.sql_from();
        // Counted first, so a sample keeps every `step`-th row, numbered on the server; both in
        // one snapshot, so the count is of the rows read.
        let (matched, step, rows) = self.conn.snapshot(None, |t| {
            let n: i64 = t.query_one(format!("SELECT count(*) FROM {from} WHERE {filter}").as_str(), &params)?.try_get(0)?;
            let matched = u64::try_from(n).unwrap_or(0);
            let step = stride(matched, limit);
            let sql = if step <= 1 {
                let cols: Vec<String> = self.select.iter().enumerate().map(|(i, e)| format!("{e} AS c{i}")).collect();
                format!("SELECT {} FROM {from} WHERE {filter} LIMIT {limit}", cols.join(", "))
            } else {
                // Rows are numbered as they are, and only those kept are converted (the geometry,
                // the casts): the expressions name the table's columns, which the sample carries.
                let id = self.select.first().map(String::as_str).unwrap_or("0");
                let rest: Vec<String> = self.select.iter().enumerate().skip(1).map(|(i, e)| format!("{e} AS c{i}")).collect();
                format!(
                    "SELECT \"rustgis id\" AS c0, {} FROM (SELECT t.*, {id} AS \"rustgis id\", row_number() OVER () AS \"rustgis rn\" \
                     FROM {from} t WHERE {filter}) s WHERE (\"rustgis rn\" - 1) % {step} = 0 LIMIT {limit}",
                    rest.join(", ")
                )
            };
            Ok((matched, step, t.query(sql.as_str(), &params)?))
        })?;
        out.matched = matched;
        for r in &rows {
            // Dates that aren't ones are counted in the rows kept.
            let mut unread = Vec::new();
            let Some(feature) = self.feature(r, &mut unread) else {
                out.skipped += 1;
                continue;
            };
            // The index matched the request's box in the table's coordinates, which holds more
            // than the request when they are projected.
            if let Some(geometry) = &feature.geometry
                && !in_request(geometry, req)
            {
                continue;
            }
            super::add_unread_dates(&mut out.unread_dates, &unread);
            out.features.push(feature);
        }
        out.complete = step <= 1 && (rows.len() as u64) >= out.matched;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn families_of_declared_types() {
        assert_eq!(family_of("MULTIPOLYGON"), Some(Family::Polygons));
        assert_eq!(family_of("POINTM"), Some(Family::Points));
        assert_eq!(family_of("COMPOUNDCURVE"), Some(Family::Lines));
        assert_eq!(family_of("MULTICURVE"), Some(Family::Lines));
        assert_eq!(family_of("CURVEPOLYGON"), Some(Family::Polygons));
        assert_eq!(family_of("GEOMETRY"), None);
        assert_eq!(family_of("GEOMETRYCOLLECTION"), None);
    }

    /// A disconnected source for geometry metadata and local query-bound validation.
    fn source(kind: &str) -> PostgisSource {
        PostgisSource {
            info: SourceInfo {
                name: "t".into(),
                format: "PostGIS",
                geometry_type: GeometryType::Polygon,
                fields: Vec::new(),
                extent: Extent::EMPTY,
                count: None,
                crs: "EPSG:4326".into(),
                indexed: false,
                notes: Vec::new(),
                zm_dropped: false,
            },
            conn: Arc::new(Conn {
                config: postgres::Config::new(),
                tls: Tls::Disable,
                plain: AtomicBool::new(true),
                writable: false,
                client: Mutex::new(None),
            }),
            table: Table {
                schema: "public".into(),
                name: "t".into(),
                column: "geom".into(),
                srid: 0,
                kind: kind.into(),
                geography: false,
                several: false,
            },
            family: None,
            srid: 0,
            raw_srid: 0,
            native: String::new(),
            key: None,
            select: Vec::new(),
            reproject: None,
        }
    }

    #[test]
    fn refused_database_view_does_not_reach_the_connection() {
        let mut source = source("POINT");
        source.reproject = Some(Reproject::from_crs(Crs::from_epsg(32617).unwrap(), &mut Vec::new()));
        for limit in [0, 10] {
            let req = Request { extent: Some(Extent::new(-120.0, -30.0, 10.0, 30.0)), limit };
            let error = source.read(&req).unwrap_err().to_string();
            assert!(error.contains("can't be bounded in the database"), "{error}");
        }
        let clean = Request { extent: Some(Extent::new(-84.0, 30.0, -78.0, 40.0)), limit: 0 };
        assert!(source.read(&clean).unwrap().features.is_empty());
        assert!(source.read(&Request { extent: None, limit: 0 }).is_ok());
    }

    #[test]
    fn unconstrained_geometry_keeps_its_parts() {
        // A typed column takes one part as its type; an unconstrained one keeps the geometry as it is.
        assert!(source("POLYGON").geometry_in(1, 2).contains("ST_NumGeometries"));
        assert!(!source("GEOMETRY").geometry_in(1, 2).contains("ST_NumGeometries"));
    }

    #[test]
    fn table_names() {
        let t = |schema: &str, several| Table {
            schema: schema.into(),
            name: "Roads".into(),
            column: "geom".into(),
            srid: 4326,
            kind: "LINESTRING".into(),
            geography: false,
            several,
        };
        assert_eq!(t("public", false).label(), "Roads");
        assert_eq!(t("osm", false).label(), "osm.Roads");
        assert_eq!(t("osm", true).label(), "osm.Roads:geom");
        assert!(t("osm", false).is_named("roads"));
        assert!(t("osm", false).is_named("OSM.roads"));
        assert!(t("osm", true).is_named("osm.roads:GEOM"));
        assert!(!t("osm", true).is_named("osm.roads:other"));
        assert!(!t("osm", false).is_named("other.roads"));
        assert_eq!(ident("a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn connection_strings_take_libpq_defaults_and_modes() {
        let (c, tls) = config("host=db.example port=6543 dbname=gis user=me sslmode=verify-full").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(tls, Tls::Verified(None));
        assert_eq!(c.get_ports(), &[6543]);
        assert_eq!(c.get_dbname(), Some("gis"));
        assert_eq!(c.get_user(), Some("me"));
        let (_, tls) = config("host=h dbname=g sslmode=disable").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(tls, Tls::Disable);
        let (c, tls) = config("postgresql://me@h:5433/gis?sslmode=verify-ca&sslrootcert=/x%20y").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(tls, Tls::Verified(Some("/x y".into())));
        let (_, tls) = config("host=h sslmode=verify-full sslrootcert=system").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(tls, Tls::Verified(None));
        // A private authority's file that isn't there fails, rather than falling back.
        let (_, tls) = config("host=h sslmode=verify-full sslrootcert=/nonexistent.pem").unwrap_or_else(|e| panic!("{e}"));
        assert!(rustls_config(&tls).is_err());
        assert_eq!(c.get_ports(), &[5433]);
        assert_eq!(c.get_dbname(), Some("gis"));
        assert!(config("host=h sslmode=bogus").is_err());
        assert!(config("host=h port=x").is_err());
        let (c, _) = config("host=a,b port=,6000").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(c.get_ports(), &[5432, 6000]);
        // A user is always set (libpq's default is the login name).
        let (c, _) = config("host=h").unwrap_or_else(|e| panic!("{e}"));
        assert!(c.get_user().is_some());
        assert!(c.get_connect_timeout().is_some());
        // As libpq, a timeout of zero or less is no limit of its own: the default applies.
        for t in ["0", "-5"] {
            let (c, _) = config(&format!("host=h connect_timeout={t}")).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(c.get_connect_timeout(), Some(&CONNECT_TIMEOUT));
        }
        let (c, _) = config("host=h connect_timeout=30").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(c.get_connect_timeout(), Some(&Duration::from_secs(30)));
    }

    #[test]
    fn tls_notes_and_ca_certificates() {
        use postgres::tls::{MakeTlsConnect, TlsConnect};
        use std::sync::atomic::{AtomicBool, Ordering};

        struct DummyStream;
        impl tokio::io::AsyncRead for DummyStream {
            fn poll_read(
                self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
                _: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                std::task::Poll::Ready(Ok(()))
            }
        }
        impl tokio::io::AsyncWrite for DummyStream {
            fn poll_write(
                self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
                buf: &[u8],
            ) -> std::task::Poll<std::io::Result<usize>> {
                std::task::Poll::Ready(Ok(buf.len()))
            }
            fn poll_flush(self: std::pin::Pin<&mut Self>, _: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
                std::task::Poll::Ready(Ok(()))
            }
            fn poll_shutdown(self: std::pin::Pin<&mut Self>, _: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
                std::task::Poll::Ready(Ok(()))
            }
        }
        impl postgres::tls::TlsStream for DummyStream {
            fn channel_binding(&self) -> postgres::tls::ChannelBinding {
                postgres::tls::ChannelBinding::none()
            }
        }

        #[derive(Clone, Copy)]
        struct FakeTls(bool);
        impl MakeTlsConnect<()> for FakeTls {
            type Stream = DummyStream;
            type TlsConnect = FakeTls;
            type Error = std::io::Error;
            fn make_tls_connect(&mut self, _: &str) -> std::io::Result<FakeTls> {
                Ok(*self)
            }
        }
        impl TlsConnect<()> for FakeTls {
            type Stream = DummyStream;
            type Error = std::io::Error;
            type Future = std::future::Ready<std::io::Result<DummyStream>>;
            fn connect(self, _: ()) -> Self::Future {
                std::future::ready(if self.0 { Ok(DummyStream) } else { Err(std::io::Error::other("tls failed")) })
            }
        }

        assert_eq!(tls_note(&Tls::Disable, true), Some("the connection isn't encrypted (sslmode=disable)"));
        assert_eq!(
            tls_note(&Tls::Unverified, true),
            Some("the connection isn't encrypted: the server offers no TLS (sslmode=require refuses such a server)")
        );
        assert_eq!(tls_note(&Tls::Unverified, false), Some("the server's certificate isn't checked (sslmode=verify-full checks it)"));
        assert_eq!(tls_note(&Tls::Verified(None), false), None);

        // `TrackTls` records per-socket handshake completion and resets on each `make_tls_connect`
        // so a prior host's TLS handshake doesn't mask a subsequent host's cleartext fallback.
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let handshake = Arc::new(AtomicBool::new(false));
        let mut tracked = TrackTls { inner: FakeTls(true), handshake: handshake.clone() };
        let c1 = MakeTlsConnect::<()>::make_tls_connect(&mut tracked, "a.example").unwrap();
        assert!(!handshake.load(Ordering::Relaxed));
        assert!(rt.block_on(c1.connect(())).is_ok());
        assert!(handshake.load(Ordering::Relaxed));

        // Second host: `make_tls_connect` resets immediately even if `connect` is never called
        // (`sslmode=prefer` server replying `'N'`), and stays false when `connect` fails.
        let _unused = MakeTlsConnect::<()>::make_tls_connect(&mut tracked, "b.example").unwrap();
        assert!(!handshake.load(Ordering::Relaxed));
        tracked.inner = FakeTls(false);
        let c2 = MakeTlsConnect::<()>::make_tls_connect(&mut tracked, "c.example").unwrap();
        assert!(rt.block_on(c2.connect(())).is_err());
        assert!(!handshake.load(Ordering::Relaxed));

        // Pre-handshake reconnect upgrades encrypted `sslmode=prefer` to `Require`, and closed
        // `Conn` entries in `Live` don't block a fresh connection or linger in the pool.
        let (base_cfg, _) = config("host=db.example dbname=gis user=me sslmode=prefer").unwrap();
        assert_eq!(base_cfg.get_ssl_mode(), postgres::config::SslMode::Prefer);
        let dropped_tls = Arc::new(Conn {
            config: base_cfg.clone(),
            tls: Tls::Unverified,
            plain: AtomicBool::new(false),
            writable: false,
            client: Mutex::new(None),
        });
        assert!(!dropped_tls.has_open_client());
        assert_eq!(dropped_tls.reconnect_cfg().get_ssl_mode(), postgres::config::SslMode::Require);
        let dropped_plain = Arc::new(Conn {
            config: base_cfg,
            tls: Tls::Unverified,
            plain: AtomicBool::new(true),
            writable: false,
            client: Mutex::new(None),
        });
        assert_eq!(dropped_plain.reconnect_cfg().get_ssl_mode(), postgres::config::SslMode::Prefer);

        let dsn = "host=db.example dbname=gis user=me";
        let pw = None;
        let mut pool: Vec<Live> = vec![(dsn.to_string(), pw.clone(), false, Arc::downgrade(&dropped_tls))];
        assert!(find_live(&mut pool, dsn, &pw, false).is_none());
        let replaced = finish_live(&mut pool, dsn, pw.clone(), false, dropped_plain.clone());
        assert!(Arc::ptr_eq(&replaced, &dropped_plain));
        assert_eq!(pool.len(), 1);
        assert!(Arc::ptr_eq(&pool[0].3.upgrade().unwrap(), &dropped_plain));

        // `find_live` never blocks on a `Conn`'s `client` mutex while another thread holds it:
        // a blocking lock inside `has_open_client` would deadlock before `done.wait()`.
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
            let found = find_live(&mut pool, dsn, &pw, false).expect("busy connection treated as open");
            assert!(Arc::ptr_eq(&found, &dropped_plain));
            assert!(find_live(&mut pool, "host=other.example dbname=gis user=me", &pw, false).is_none());
            done.wait();
        });

        // Custom CA loader: accepts valid PEM <= 1 MiB, rejects folders, oversized files and
        // invalid files without echoing their contents.
        let dir = tempfile::tempdir().unwrap();
        let (_, pem) = super::super::tests::synthetic_ca_cert();
        let good = dir.path().join("ca.pem");
        std::fs::write(&good, &pem).unwrap();
        assert!(rustls_config(&Tls::Verified(Some(good))).is_ok());

        assert!(rustls_config(&Tls::Verified(Some(dir.path().to_path_buf()))).is_err());

        let huge = dir.path().join("huge.pem");
        std::fs::write(&huge, vec![b'A'; (super::super::MAX_CA_BYTES as usize) + 1]).unwrap();
        let err = rustls_config(&Tls::Verified(Some(huge))).unwrap_err().to_string();
        assert!(err.contains("larger than 1 MB"), "{err}");

        let marker = concat!("SENTINEL_CA_", "LINE_8472");
        for (name, body) in [
            ("plain.pem", format!("{marker}\nnot a certificate\n")),
            ("broken.pem", format!("-----BEGIN CERTIFICATE-----\n{marker}\n-----END CERTIFICATE-----\n")),
        ] {
            let p = dir.path().join(name);
            std::fs::write(&p, body).unwrap();
            let err = rustls_config(&Tls::Verified(Some(p))).unwrap_err().to_string();
            assert!(!err.contains(marker), "error leaked file contents: {err}");
        }
    }

    #[test]
    fn password_files() {
        let text = "# comment\nother:5432:*:me:nope\ndb.example:*:gis:me:p\\:w\\\\d\n*:*:*:*:fallback\n";
        assert_eq!(pgpass_find(text, &["db.example", "5432", "gis", "me"]).as_deref(), Some("p:w\\d"));
        assert_eq!(pgpass_find(text, &["x", "1", "y", "z"]).as_deref(), Some("fallback"));
        assert_eq!(pgpass_find("a:b:c", &["a", "b", "c", "d"]), None);
        // The host it names: the host, else the address, else `localhost`.
        let host = |dsn: &str| pgpass_host(&config(dsn).unwrap_or_else(|e| panic!("{e}")).0);
        assert_eq!(host("host=db.example hostaddr=10.0.0.1"), "db.example");
        assert_eq!(host("hostaddr=10.0.0.1"), "10.0.0.1");
        #[cfg(unix)]
        {
            assert_eq!(host("host=/var/run/postgresql"), "localhost");
            assert_eq!(host("host=/srv/pg"), "/srv/pg");
        }
    }

    #[test]
    fn field_casts() {
        assert_eq!(field_cast("int4"), Some((FieldType::Integer, "int8")));
        assert_eq!(field_cast("numeric"), Some((FieldType::Double, "float8")));
        assert_eq!(field_cast("date"), Some((FieldType::Date, "text")));
        assert_eq!(field_cast("timestamp"), Some((FieldType::DateTime, "text")));
        assert_eq!(field_cast("timestamptz"), Some((FieldType::DateTime, "text")));
        assert_eq!(field_cast("time"), Some((FieldType::Text, "text")));
        assert_eq!(field_cast("geometry"), None);
    }

    /// Saving counts what a column doesn't keep: a `timestamptz` keeps an offset's instant in UTC
    /// and takes a time without a zone as UTC, a `timestamp` keeps the clock time; UTC in a
    /// `timestamptz`, a time without a zone in a `timestamp` and other columns lose nothing.
    #[test]
    fn saved_offsets_are_counted() {
        let at = |s: &str| Value::parse(s, FieldType::DateTime);
        let mut saved = Saved::default();
        for (column, v) in [
            ("timestamp with time zone", at("2026-03-05T14:30:00+02:00")),
            ("timestamp(3) with time zone", at("2026-03-05T14:30:00Z")),
            ("timestamp with time zone", at("2026-03-05T14:30:00")),
            ("timestamp without time zone", at("2026-03-05T14:30:00Z")),
            ("timestamp(0) without time zone", Value::Text("2026-03-05 14:30+05:30".into())),
            ("timestamp without time zone", at("2026-03-05T14:30:00")),
            ("text", at("2026-03-05T14:30:00+02:00")),
            ("date", Value::parse("2026-03-05", FieldType::Date)),
            ("timestamp with time zone", Value::parse("2026-03-05", FieldType::Date)),
        ] {
            count_offset(&mut saved, 1, column, &v);
        }
        assert_eq!(
            (saved.utc_dates.as_slice(), saved.zoneless_dates.as_slice(), saved.assumed_utc.as_slice()),
            (&[0, 1][..], &[0, 2][..], &[0, 2][..])
        );
        let fields = [Field::new("id", FieldType::Integer), Field::new("seen", FieldType::DateTime)];
        assert_eq!(
            saved_dates_notes(&fields, &saved, "wells"),
            [
                "wells: 1 date-time was saved in UTC, without its offset (a timestamptz column holds only the moment), in 'seen'",
                "wells: 2 date-times were saved at the clock time shown, without their offsets (a timestamp column has no time zone), in 'seen'",
                "wells: 2 date-times without a time zone were saved as UTC (a timestamptz column holds a moment), in 'seen'"
            ]
        );
        assert!(saved_dates_notes(&fields, &Saved::default(), "wells").is_empty());
    }

    /// Reads every table of a live database: `RUSTGIS_TEST_POSTGIS="host=… dbname=… user=…
    /// password=…" cargo test -p rustgis-data -- --ignored postgis_live --nocapture`.
    #[test]
    #[ignore = "needs a live PostGIS database in RUSTGIS_TEST_POSTGIS"]
    fn postgis_live() {
        let Ok(dsn) = std::env::var("RUSTGIS_TEST_POSTGIS") else { return };
        let db = super::super::duckdb::Database::parse(&dsn);
        let super::super::duckdb::Database::Postgres(dsn) = db else { panic!("not a connection") };
        let conn = connect(&dsn, true).unwrap_or_else(|e| panic!("{e}"));
        // A connection the server drops is opened again on the next use, which still works.
        let _ = conn.with(|c| c.batch_execute("SELECT pg_terminate_backend(pg_backend_pid())"));
        let all = tables(&conn).unwrap_or_else(|e| panic!("after a dropped connection: {e}"));
        for t in all.iter().filter(|t| t.schema != "tiger") {
            let fams = families(&conn, t).unwrap_or_else(|e| panic!("{}: {e}", t.label()));
            for f in fams.iter().copied().map(Some).chain(fams.is_empty().then_some(None)) {
                let start = std::time::Instant::now();
                let src = match PostgisSource::open(conn.clone(), t.clone(), f, None) {
                    Ok(s) => s,
                    Err(e) => {
                        println!("{:<28} open failed: {e}", t.label());
                        continue;
                    }
                };
                let opened = start.elapsed();
                let i = src.info();
                let all = src.read(&Request { extent: None, limit: 250_000 }).unwrap_or_else(|e| panic!("{}: {e}", t.label()));
                let read = start.elapsed() - opened;
                // A view of a quarter of the extent.
                let e = i.extent;
                let q = Extent::new(e.min_x, e.min_y, e.min_x + e.width() / 2.0, e.min_y + e.height() / 2.0);
                let part = src.read(&Request { extent: Some(q), limit: 1000 }).unwrap_or_else(|e| panic!("{}: {e}", t.label()));
                for ft in &part.features {
                    let g = ft.geometry.as_ref().map(|g| g.extent()).unwrap_or(Extent::EMPTY);
                    assert!(g.intersects(&q.padded(0.01, 0.0)), "{}: feature {} outside the request", t.label(), ft.id);
                }
                let mut ids: Vec<u64> = all.features.iter().map(|f| f.id).collect();
                ids.sort_unstable();
                ids.dedup();
                assert_eq!(ids.len(), all.features.len(), "{}: ids repeat", t.label());
                println!(
                    "{:<28} {:<9} {:?} {:>9} rows ({:?}) idx={} crs={} extent=[{:.2} {:.2} {:.2} {:.2}] fields={} all: {} of {} complete={} skipped={} | quarter: {} of {} | open {:?} read {:?} notes={:?}",
                    t.label(),
                    f.map(|f| f.label()).unwrap_or("-"),
                    i.geometry_type,
                    i.count.unwrap_or(0),
                    t.kind,
                    i.indexed,
                    i.crs,
                    e.min_x,
                    e.min_y,
                    e.max_x,
                    e.max_y,
                    i.fields.len(),
                    all.features.len(),
                    all.matched,
                    all.complete,
                    all.skipped,
                    part.features.len(),
                    part.matched,
                    opened,
                    read,
                    i.notes
                );
            }
        }
    }
}
