//! SQL Server and Azure SQL spatial tables and views, read live on the desktop.
//!
//! The pure-Rust TDS client uses verified TLS by default. A small private runtime serializes
//! bounded reads; geometry and geography arrive as linearized WKB, longitude first. Sampling
//! and keyless ids use server-side row numbers, so only the kept rows cross the network.

use super::{Batch, Family, FeatureSource, Reproject, Request, SourceInfo, clamp_limit, in_request, stride};
use crate::raster::Crs;
use crate::{DataError, Feature, Field, FieldType, Geometry, GeometryType, Result, Value, wkb};
use rustgis_geom::Extent;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex, Weak};
use std::time::Duration;
use tiberius::{ColumnData, Row};
use tokio_util::compat::TokioAsyncWriteCompatExt;

const STATEMENT_TIMEOUT: u64 = 120;
const PROBE_TIMEOUT: u64 = 10;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_CONNECT_TIMEOUT_SECS: u64 = 300;
const EXACT_COUNT_BELOW: u64 = 2_000_000;
const EXTENT_SAMPLE: usize = 20_000;
const LIST_LIMIT: usize = 10_000;
const DEFAULT_PORT: u16 = 1433;

// ---------------------------------------------------------------------------------------------
// Connection addresses

pub use super::msconn::{database_name, is_address, split_password};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Login {
    Sql(String),
    Windows(String),
    Integrated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Encrypt {
    Required,
    Strict,
    Off,
}

#[derive(Clone, Debug, PartialEq)]
struct Config {
    host: String,
    port: Option<u16>,
    instance: Option<String>,
    login: Login,
    database: Option<String>,
    encrypt: Encrypt,
    trust_cert: bool,
    ca: Option<PathBuf>,
    hostname_in_certificate: Option<String>,
    connect_timeout: Duration,
}

fn config(address: &str) -> Result<Config> {
    let bad = |m: &str| DataError::Format(format!("the connection: {m}"));
    let (public, _) = split_password(address).ok_or_else(|| bad("not a mssql:// address"))?;
    let (_, rest) = public.split_once("://").ok_or_else(|| bad("not a mssql:// address"))?;
    let (rest, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (user, hostport) = match super::pgconn::userinfo_end(rest) {
        Some(at) => (super::pgconn::percent_decode(rest.get(..at).unwrap_or("")), rest.get(at + 1..).unwrap_or("")),
        None => (String::new(), rest),
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
    let port = port.filter(|p| !p.is_empty()).map(|p| p.parse::<u16>().map_err(|_| bad(&format!("port '{p}'")))).transpose()?;
    let mut c = Config {
        host: if host.is_empty() { "localhost".into() } else { host },
        port,
        instance: None,
        login: if user.is_empty() {
            Login::Integrated
        } else if user.contains('\\') {
            Login::Windows(user)
        } else {
            Login::Sql(user)
        },
        database: database_name(&public),
        encrypt: Encrypt::Required,
        trust_cert: false,
        ca: None,
        hostname_in_certificate: None,
        connect_timeout: CONNECT_TIMEOUT,
    };
    for kv in query.split('&').filter(|kv| !kv.is_empty()) {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        let v = super::pgconn::percent_decode(v);
        match k.to_ascii_lowercase().replace('_', "-").as_str() {
            "encrypt" => {
                c.encrypt = match v.to_ascii_lowercase().as_str() {
                    "true" | "yes" | "mandatory" | "required" => Encrypt::Required,
                    "strict" => Encrypt::Strict,
                    "false" | "no" | "optional" => Encrypt::Off,
                    _ => return Err(bad(&format!("encrypt '{v}'"))),
                }
            }
            "trust-server-certificate" | "trustservercertificate" => {
                c.trust_cert = match v.to_ascii_lowercase().as_str() {
                    "true" | "yes" | "1" => true,
                    "false" | "no" | "0" => false,
                    _ => return Err(bad(&format!("trust-server-certificate '{v}'"))),
                }
            }
            "ca" | "ssl-ca" | "trust-server-certificate-ca" => c.ca = Some(v.into()),
            "hostname-in-certificate" => c.hostname_in_certificate = Some(v),
            "instance" => c.instance = Some(v),
            "connect-timeout" => {
                if let Ok(s) = v.trim().parse::<u64>()
                    && s > 0
                {
                    c.connect_timeout = Duration::from_secs(s.min(MAX_CONNECT_TIMEOUT_SECS));
                }
            }
            _ => {}
        }
    }
    if c.port.is_none() && c.instance.is_none() {
        c.port = Some(DEFAULT_PORT);
    }
    Ok(c)
}

// ---------------------------------------------------------------------------------------------
// Connections

type Client = tiberius::Client<tokio_util::compat::Compat<tokio::net::TcpStream>>;

/// A shared connection; statements run one at a time on its private runtime.
pub struct Conn {
    config: Config,
    password: Option<String>,
    major: u32,
    schema: String,
    compatibility: u32,
    rt: tokio::runtime::Runtime,
    client: Mutex<Option<Client>>,
}

fn err(e: tiberius::error::Error) -> DataError {
    match e {
        tiberius::error::Error::Server(t) if t.code() == 18456 => DataError::Login(t.message().to_string()),
        tiberius::error::Error::Server(t) => DataError::Format(t.message().to_string()),
        e => DataError::Format(e.to_string()),
    }
}

fn dropped(e: &tiberius::error::Error) -> bool {
    use std::io::ErrorKind as K;
    match e {
        tiberius::error::Error::Io { kind, .. } => {
            matches!(kind, K::BrokenPipe | K::ConnectionReset | K::ConnectionAborted | K::UnexpectedEof | K::NotConnected)
        }
        tiberius::error::Error::Server(t) => matches!(t.code(), 233 | 10054 | 10053),
        _ => false,
    }
}

fn broken(e: &tiberius::error::Error) -> bool {
    dropped(e) || !matches!(e, tiberius::error::Error::Server(_) | tiberius::error::Error::Conversion(_))
}

fn auth_method(login: &Login, password: Option<&str>) -> Result<tiberius::AuthMethod> {
    match login {
        Login::Sql(u) => Ok(tiberius::AuthMethod::sql_server(u, password.unwrap_or(""))),
        #[cfg(windows)]
        Login::Windows(u) => Ok(tiberius::AuthMethod::windows(u, password.unwrap_or(""))),
        #[cfg(windows)]
        Login::Integrated => Ok(tiberius::AuthMethod::Integrated),
        #[cfg(not(windows))]
        Login::Windows(_) => Err(DataError::Unsupported(
            "Windows logins (DOMAIN\\user) are supported on Windows: give a SQL Server login (mssql://user@host/database)".into(),
        )),
        #[cfg(not(windows))]
        Login::Integrated => Err(DataError::Format(
            "give a SQL Server login: mssql://user@host/database (signing in as the Windows user is supported on Windows)".into(),
        )),
    }
}

async fn open_client(config: &Config, password: Option<&str>) -> std::result::Result<Client, tiberius::error::Error> {
    let mut cfg = tiberius::Config::new();
    cfg.host(&config.host);
    if let Some(p) = config.port {
        cfg.port(p);
    }
    if let Some(i) = &config.instance {
        cfg.instance_name(i);
    }
    if let Some(d) = &config.database {
        cfg.database(d);
    }
    cfg.application_name("RustGIS");
    cfg.readonly(true);
    cfg.encryption(match config.encrypt {
        Encrypt::Required => tiberius::EncryptionLevel::Required,
        Encrypt::Strict => tiberius::EncryptionLevel::Strict,
        Encrypt::Off => tiberius::EncryptionLevel::Off,
    });
    if config.trust_cert {
        cfg.trust_cert();
    } else if let Some(ca) = &config.ca {
        cfg.trust_cert_ca(ca.display());
    }
    if let Some(h) = &config.hostname_in_certificate {
        cfg.hostname_in_certificate(h);
    }
    cfg.handshake_timeout(Some(config.connect_timeout));
    cfg.command_timeout(Some(Duration::from_secs(STATEMENT_TIMEOUT)));
    cfg.lossy_utf16_decoding(true);
    cfg.authentication(auth_method(&config.login, password).map_err(|e| tiberius::error::Error::Conversion(e.to_string().into()))?);
    let tcp = if config.instance.is_some() && config.port.is_none() {
        <tokio::net::TcpStream as tiberius::SqlBrowser>::connect_named(&cfg).await?
    } else {
        tokio::net::TcpStream::connect(cfg.get_addr()).await?
    };
    tcp.set_nodelay(true)?;
    match Client::connect(cfg.clone(), tcp.compat_write()).await {
        Err(tiberius::error::Error::Routing { host, port }) => {
            cfg.host(&host);
            cfg.port(port);
            let tcp = tokio::net::TcpStream::connect((host.as_str(), port)).await?;
            tcp.set_nodelay(true)?;
            Client::connect(cfg, tcp.compat_write()).await
        }
        result => result,
    }
}

async fn session(client: &mut Client) -> std::result::Result<(u32, String, u32), tiberius::error::Error> {
    client.simple_query("SET NOCOUNT ON; SET LOCK_TIMEOUT 30000;").await?.into_results().await?;
    let rows = client.simple_query("SELECT CAST(SERVERPROPERTY('ProductMajorVersion') AS INT), SCHEMA_NAME(), (SELECT CAST(compatibility_level AS INT) FROM sys.databases WHERE database_id = DB_ID())").await?.into_first_result().await?;
    let major = rows.first().and_then(|r| int_of(at(r, 0))).and_then(|n| u32::try_from(n).ok()).unwrap_or(0);
    let schema = rows.first().and_then(|r| text_of(at(r, 1))).unwrap_or_else(|| "dbo".into());
    // Missing metadata must not enable geography windows on a legacy database.
    let compatibility = rows.first().and_then(|r| int_of(at(r, 2))).and_then(|n| u32::try_from(n).ok()).unwrap_or(0);
    Ok((major, schema, compatibility))
}

/// The deadline includes TCP, TLS, authentication and session setup, also after a dropped client.
fn open_on(rt: &tokio::runtime::Runtime, config: &Config, password: Option<&str>) -> Result<(Client, u32, String, u32)> {
    rt.block_on(async {
        tokio::time::timeout(config.connect_timeout, async {
            let mut client = open_client(config, password).await?;
            let (major, schema, compatibility) = session(&mut client).await?;
            Ok((client, major, schema, compatibility))
        })
        .await
    })
    .map_err(|_| DataError::Format(format!("couldn't connect within {} s", config.connect_timeout.as_secs())))?
    .map_err(|e| err(e).redact_secret(password.unwrap_or("")))
}

fn connect_blocking(config: &Config, password: Option<&str>) -> Result<(tokio::runtime::Runtime, Client, u32, String, u32)> {
    super::tls_provider();
    let rt =
        tokio::runtime::Builder::new_current_thread().enable_io().enable_time().build().map_err(|e| DataError::Format(e.to_string()))?;
    let (client, major, schema, compatibility) = open_on(&rt, config, password)?;
    Ok((rt, client, major, schema, compatibility))
}

impl Conn {
    fn query_within(&self, sql: &str, params: &[&str], secs: u64) -> Result<Option<Vec<Row>>> {
        let mut guard = self.client.lock().unwrap_or_else(|e| e.into_inner());
        for attempt in 0..2 {
            if guard.is_none() {
                *guard = Some(open_on(&self.rt, &self.config, self.password.as_deref())?.0);
            }
            let client = guard.as_mut().ok_or_else(|| DataError::Format("not connected".into()))?;
            let result = self.rt.block_on(async {
                tokio::time::timeout(Duration::from_secs(secs), async {
                    let mut q = tiberius::Query::new(sql);
                    for p in params {
                        q.bind(*p);
                    }
                    q.query(client).await?.into_first_result().await
                })
                .await
            });
            match result {
                Err(_) => {
                    *guard = None;
                    return Ok(None);
                }
                Ok(Ok(rows)) => return Ok(Some(rows)),
                Ok(Err(e)) if attempt == 0 && dropped(&e) => *guard = None,
                Ok(Err(e)) => {
                    if broken(&e) {
                        *guard = None;
                    }
                    return Err(err(e).redact_secret(self.password.as_deref().unwrap_or("")));
                }
            }
        }
        Err(DataError::Format("not connected".into()))
    }

    fn query(&self, sql: &str, params: &[&str]) -> Result<Vec<Row>> {
        self.query_within(sql, params, STATEMENT_TIMEOUT)?.ok_or_else(|| timed_out(STATEMENT_TIMEOUT))
    }
}

fn timed_out(secs: u64) -> DataError {
    DataError::Format(format!("the server took longer than {secs} s"))
}

type Live = (String, Option<String>, Weak<Conn>);
fn live() -> &'static Mutex<Vec<Live>> {
    static L: LazyLock<Mutex<Vec<Live>>> = LazyLock::new(Default::default);
    &L
}
fn integrated_ok() -> &'static Mutex<HashSet<String>> {
    static I: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);
    &I
}

/// Shares a live connection. Project reopening sends only credentials supplied this session,
/// never SQLCMDPASSWORD or the Windows identity to an address not explicitly added this session.
pub fn connect(address: &str, ask: bool) -> Result<Arc<Conn>> {
    let config = config(address)?;
    let password =
        super::duckdb::remembered_password(address).or_else(|| if ask { super::duckdb::ambient_password("sqlserver") } else { None });
    let _ = auth_method(&config.login, password.as_deref())?;
    if config.login == Login::Integrated && !ask && !integrated_ok().lock().unwrap_or_else(|e| e.into_inner()).contains(address) {
        return Err(DataError::Format("signing in as the Windows user needs the database added this session".into()));
    }
    let find = |live: &mut Vec<Live>| {
        live.retain(|(_, _, w)| w.strong_count() > 0);
        live.iter().find(|(a, p, _)| a == address && *p == password).and_then(|(_, _, w)| w.upgrade())
    };
    if let Some(c) = find(&mut live().lock().unwrap_or_else(|e| e.into_inner())) {
        return Ok(c);
    }
    let (rt, client, major, schema, compatibility) = connect_blocking(&config, password.as_deref())?;
    if config.login == Login::Integrated && ask {
        integrated_ok().lock().unwrap_or_else(|e| e.into_inner()).insert(address.to_string());
    }
    let mut live = live().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(c) = find(&mut live) {
        return Ok(c);
    }
    let conn = Arc::new(Conn { config, password: password.clone(), major, schema, compatibility, rt, client: Mutex::new(Some(client)) });
    live.push((address.to_string(), password, Arc::downgrade(&conn)));
    Ok(conn)
}

// ---------------------------------------------------------------------------------------------
// Tables

/// One spatial column of a table or view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Table {
    pub schema: String,
    pub name: String,
    pub column: String,
    /// `geometry` or `geography`.
    pub kind: String,
    pub current: bool,
    pub several: bool,
}

impl Table {
    pub fn label(&self) -> String {
        let base = if self.current { self.name.clone() } else { format!("{}.{}", self.schema, self.name) };
        if self.several { format!("{base}:{}", self.column) } else { base }
    }

    pub fn is_named(&self, name: &str) -> bool {
        let (table, column) = match name.rsplit_once(':') {
            Some((t, c)) => (t, Some(c)),
            None => (name, None),
        };
        let full = format!("{}.{}", self.schema, self.name);
        (table.eq_ignore_ascii_case(&self.name) || table.eq_ignore_ascii_case(&full))
            && column.is_none_or(|c| c.eq_ignore_ascii_case(&self.column))
    }

    fn sql_from(&self) -> String {
        format!("{}.{}", ident(&self.schema), ident(&self.name))
    }
    fn geography(&self) -> bool {
        self.kind == "geography"
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

fn ident(s: &str) -> String {
    format!("[{}]", s.replace(']', "]]"))
}

fn at(r: &Row, i: usize) -> &ColumnData<'static> {
    r.get_column_data(i).unwrap_or(&ColumnData::I32(None))
}
fn int_of(v: &ColumnData<'static>) -> Option<i64> {
    match v {
        ColumnData::U8(v) => v.map(i64::from),
        ColumnData::I16(v) => v.map(i64::from),
        ColumnData::I32(v) => v.map(i64::from),
        ColumnData::I64(v) => *v,
        ColumnData::F32(Some(v)) => integral(f64::from(*v)),
        ColumnData::F64(Some(v)) => integral(*v),
        ColumnData::String(Some(v)) => v.trim().parse().ok(),
        _ => None,
    }
}
fn integral(v: f64) -> Option<i64> {
    (v.fract() == 0.0 && v.abs() < 9e18).then_some(v as i64)
}
fn id_of(v: &ColumnData<'static>) -> Option<u64> {
    int_of(v).map(i64::cast_unsigned)
}
fn float_of(v: &ColumnData<'static>) -> Option<f64> {
    match v {
        ColumnData::F32(v) => v.map(f64::from),
        ColumnData::F64(v) => *v,
        ColumnData::String(Some(v)) => v.trim().parse().ok(),
        _ => int_of(v).map(|v| v as f64),
    }
    .filter(|v| v.is_finite())
}
fn bool_of(v: &ColumnData<'static>) -> Option<bool> {
    if let ColumnData::Bit(v) = v { *v } else { None }
}
fn text_of(v: &ColumnData<'static>) -> Option<String> {
    match v {
        ColumnData::String(Some(v)) => Some(v.to_string()),
        ColumnData::Guid(Some(v)) => Some(v.to_string()),
        _ => None,
    }
}
fn bytes_of<'a>(v: &'a ColumnData<'static>) -> Option<&'a [u8]> {
    if let ColumnData::Binary(Some(v)) = v { Some(v) } else { None }
}

/// Lists spatial columns in the selected database, including views.
pub fn tables(conn: &Conn) -> Result<Vec<Table>> {
    let sql = format!(
        "SELECT TOP ({LIST_LIMIT}) TABLE_SCHEMA, TABLE_NAME, COLUMN_NAME, LOWER(DATA_TYPE), CASE WHEN TABLE_SCHEMA = SCHEMA_NAME() THEN 1 ELSE 0 END FROM INFORMATION_SCHEMA.COLUMNS WHERE DATA_TYPE IN ('geometry', 'geography') ORDER BY TABLE_SCHEMA, TABLE_NAME, ORDINAL_POSITION"
    );
    let rows = conn.query(&sql, &[])?;
    let mut out: Vec<Table> = rows
        .iter()
        .filter_map(|r| {
            Some(Table {
                schema: text_of(at(r, 0))?,
                name: text_of(at(r, 1))?,
                column: text_of(at(r, 2))?,
                kind: text_of(at(r, 3))?,
                current: text_of(at(r, 0)).is_some_and(|s| s == conn.schema),
                several: false,
            })
        })
        .collect();
    let mut columns: HashMap<(&str, &str), usize> = HashMap::new();
    for t in &out {
        *columns.entry((&t.schema, &t.name)).or_default() += 1;
    }
    let several: Vec<bool> = out.iter().map(|t| columns.get(&(t.schema.as_str(), t.name.as_str())).is_some_and(|n| *n > 1)).collect();
    for (t, several) in out.iter_mut().zip(several) {
        t.several = several;
    }
    Ok(out)
}

pub fn find_table(conn: &Conn, name: &str) -> Result<Table> {
    let all = tables(conn)?;
    all.iter()
        .find(|t| t.label().eq_ignore_ascii_case(name))
        .or_else(|| all.iter().find(|t| t.is_named(name)))
        .cloned()
        .ok_or_else(|| DataError::Format(format!("no table '{name}' with a geometry column")))
}

pub fn families(conn: &Conn, table: &Table) -> Result<Vec<Family>> {
    let g = ident(&table.column);
    let sql = format!("SELECT DISTINCT {g}.STDimension() FROM {} WHERE {g} IS NOT NULL", table.sql_from());
    let Some(rows) = conn.query_within(&sql, &[], PROBE_TIMEOUT)? else {
        return Ok(Family::ALL.to_vec());
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

pub struct SqlServerSource {
    info: SourceInfo,
    conn: Arc<Conn>,
    table: Table,
    family: Option<Family>,
    raw_srid: u32,
    geography: bool,
    projection: Projection,
    reproject: Reproject,
}
impl std::fmt::Debug for SqlServerSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqlServerSource").field("table", &self.table).field("family", &self.family).finish_non_exhaustive()
    }
}

fn field_cast(data_type: &str, col: &str) -> Option<(FieldType, String)> {
    Some(match data_type {
        "geometry" | "geography" | "image" | "binary" | "varbinary" | "timestamp" | "rowversion" => return None,
        "bit" => (FieldType::Boolean, col.to_string()),
        "tinyint" | "smallint" | "int" | "bigint" => (FieldType::Integer, format!("CAST({col} AS BIGINT)")),
        "real" | "float" | "decimal" | "numeric" | "money" | "smallmoney" => (FieldType::Double, format!("CAST({col} AS FLOAT)")),
        "date" | "datetime" | "datetime2" | "smalldatetime" | "datetimeoffset" | "time" => {
            (FieldType::Text, format!("CONVERT(NVARCHAR(40), {col}, 126)"))
        }
        _ => (FieldType::Text, format!("CAST({col} AS NVARCHAR(MAX))")),
    })
}

struct Projection {
    select: Vec<String>,
    numbered: bool,
    order: String,
    raw: String,
}

/// Gives every source column a positional name before adding row-number and count columns.
/// Original names remain only in the table projection and field metadata.
fn projection(geometry: &str, columns: impl IntoIterator<Item = (String, String)>, key: Option<&str>) -> (Vec<Field>, Projection) {
    let mut pk = None;
    let mut fields = Vec::new();
    let mut casts = Vec::new();
    let mut raw = vec![format!("t.{} AS i0", ident(geometry))];
    for (name, ty) in columns {
        if name == geometry {
            continue;
        }
        let alias = format!("i{}", raw.len());
        if let Some((ft, cast)) = field_cast(&ty, &alias) {
            if key == Some(name.as_str()) && matches!(ty.as_str(), "tinyint" | "smallint" | "int" | "bigint") {
                pk = Some((ident(&name), alias.clone()));
            }
            casts.push(cast);
            raw.push(format!("t.{} AS {alias}", ident(&name)));
            fields.push(Field::new(name, ft));
        }
    }
    let numbered = pk.is_none();
    let order = pk.as_ref().map(|(name, _)| format!("ORDER BY t.{name}")).unwrap_or_else(|| "ORDER BY (SELECT NULL)".into());
    let id = pk.map(|(_, alias)| format!("CAST({alias} AS BIGINT)")).unwrap_or_else(|| "[rustgis rn]".into());
    let mut select = vec![id, "i0.STCurveToLine().STAsBinary()".into()];
    select.extend(casts);
    (fields, Projection { select, numbered, order, raw: raw.join(", ") })
}

impl SqlServerSource {
    /// Opens one family of a spatial column. `crs` supplies the coordinate system for SRID 0.
    pub fn open(conn: Arc<Conn>, table: Table, family: Option<Family>, crs: Option<&str>) -> Result<Self> {
        if conn.major < 11 {
            return Err(DataError::Unsupported("SQL Server tables are read from SQL Server 2012 and later".into()));
        }
        let mut notes = Vec::new();
        let g = ident(&table.column);
        let from = table.sql_from();
        let geography = table.geography();
        let rows = conn.query(&format!("SELECT TOP (1) {g}.STSrid FROM {from} WHERE {g} IS NOT NULL"), &[])?;
        let raw_srid =
            rows.first().and_then(|r| int_of(at(r, 0))).and_then(|n| u32::try_from(n).ok()).unwrap_or(if geography { 4326 } else { 0 });
        if raw_srid != 0 {
            notes.push(format!("the column doesn't fix a coordinate system: rows in one other than EPSG:{raw_srid} are left out"));
        }
        let mut srid = raw_srid;
        if srid == 0 {
            match crs.map(str::trim).filter(|c| !c.is_empty()) {
                Some(given) => match Crs::parse(given) {
                    Ok(c) if c.epsg() > 0 => srid = u32::from(c.epsg()),
                    _ => return Err(DataError::Format(format!("'{given}': give the coordinate system as EPSG:<code>"))),
                },
                None => notes.push("the geometry doesn't say its coordinate system; read as longitude/latitude (add it again with \"crs\", such as EPSG:2263)".into()),
            }
        }
        if conn.config.encrypt == Encrypt::Off {
            notes.push("the connection isn't encrypted after login (encrypt=false)".into());
        }
        if conn.config.trust_cert {
            notes.push("the server's certificate isn't checked (trust-server-certificate=true)".into());
        }
        let reproject = Reproject::from_epsg(Some(srid), &mut notes);
        let cols = conn.query("SELECT COLUMN_NAME, LOWER(DATA_TYPE) FROM INFORMATION_SCHEMA.COLUMNS WHERE TABLE_SCHEMA = @P1 AND TABLE_NAME = @P2 ORDER BY ORDINAL_POSITION", &[&table.schema, &table.name])?;
        let keys = conn.query("SELECT k.COLUMN_NAME FROM INFORMATION_SCHEMA.TABLE_CONSTRAINTS c JOIN INFORMATION_SCHEMA.KEY_COLUMN_USAGE k ON k.CONSTRAINT_SCHEMA = c.CONSTRAINT_SCHEMA AND k.CONSTRAINT_NAME = c.CONSTRAINT_NAME WHERE c.CONSTRAINT_TYPE = 'PRIMARY KEY' AND c.TABLE_SCHEMA = @P1 AND c.TABLE_NAME = @P2", &[&table.schema, &table.name])?;
        let key = if keys.len() == 1 { keys.first().and_then(|r| text_of(at(r, 0))) } else { None };
        let columns = cols.iter().filter_map(|r| Some((text_of(at(r, 0))?, text_of(at(r, 1))?)));
        let (fields, projection) = projection(&table.column, columns, key.as_deref());
        if projection.numbered {
            notes.push(
                "no single-column integer key: features are numbered as read, so their ids can change from one read to the next".into(),
            );
        }
        let mut src = SqlServerSource {
            info: SourceInfo {
                name: table.label(),
                format: "SQL Server",
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
            geography,
            projection,
            reproject,
        };
        src.info.indexed = src.has_index();
        src.info.count = src.count_rows();
        if src.info.count != Some(0) {
            src.info.extent = src.measure();
            if src.info.extent.is_empty() {
                src.info.notes.push("the table's extent couldn't be measured in time: zooming to the layer does nothing".into());
            }
            src.check_zm();
        }
        Ok(src)
    }

    /// Notes the Z and M values reading drops: `STAsBinary` writes x and y only. Counts the rows
    /// with them within the probe timeout, and says so when that count fails or runs out of time.
    fn check_zm(&mut self) {
        let sql = zm_probe(&self.table.sql_from(), &ident(&self.table.column), &self.filter(None));
        let probe = self.conn.query_within(&sql, &[], PROBE_TIMEOUT).map(|rows| rows.map(|r| first_count(&r)));
        note_zm(&mut self.info, probe);
    }

    fn has_index(&self) -> bool {
        self.conn.query("SELECT TOP (1) 1 FROM sys.spatial_indexes si JOIN sys.index_columns ic ON ic.object_id = si.object_id AND ic.index_id = si.index_id JOIN sys.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id WHERE si.object_id = OBJECT_ID(@P1) AND c.name = @P2", &[&self.table.sql_from(), &self.table.column]).ok().is_some_and(|r| !r.is_empty())
    }

    fn count_rows(&self) -> Option<u64> {
        let estimate = self
            .conn
            .query(
                "SELECT SUM(p.rows) FROM sys.partitions p WHERE p.object_id = OBJECT_ID(@P1) AND p.index_id IN (0, 1)",
                &[&self.table.sql_from()],
            )
            .ok()
            .and_then(|r| first_count(&r));
        match estimate {
            Some(n) if n >= EXACT_COUNT_BELOW => Some(n),
            _ => self
                .conn
                .query_within(
                    &format!("SELECT COUNT_BIG(*) FROM {} WHERE {}", self.table.sql_from(), self.filter(None)),
                    &[],
                    PROBE_TIMEOUT,
                )
                .ok()
                .flatten()
                .and_then(|r| first_count(&r)),
        }
    }

    fn measure(&self) -> Extent {
        let Ok(b) = self.read_within(&Request { extent: None, limit: EXTENT_SAMPLE }, Some(PROBE_TIMEOUT)) else {
            return Extent::EMPTY;
        };
        b.features.iter().filter_map(|f| f.geometry.as_ref()).fold(Extent::EMPTY, |e, g| e.union(&g.extent()))
    }

    fn filter(&self, window: Option<&str>) -> String {
        let g = ident(&self.table.column);
        let mut w = format!("{g} IS NOT NULL AND {g}.STSrid = {}", self.raw_srid);
        if let Some(window) = window {
            let ty = if self.geography { "geography" } else { "geometry" };
            w.push_str(&format!(" AND {g}.Filter({ty}::STGeomFromText('{window}', {})) = 1", self.raw_srid));
        }
        if let Some(f) = self.family {
            let dim = match f {
                Family::Points => 0,
                Family::Lines => 1,
                Family::Polygons => 2,
            };
            w.push_str(&format!(" AND {g}.STDimension() = {dim}"));
        }
        w
    }

    fn row_feature(&self, r: &Row) -> Option<Feature> {
        let id = id_of(at(r, 0))?;
        let geometry: Geometry = self.reproject.geometry(wkb::decode(bytes_of(at(r, 1))?)?)?;
        let attrs = self
            .info
            .fields
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let v = at(r, i + 2);
                match f.ty {
                    FieldType::Integer => int_of(v).map(Value::Int),
                    FieldType::Double => float_of(v).map(Value::Float),
                    FieldType::Boolean => bool_of(v).map(Value::Bool),
                    FieldType::Text => text_of(v).map(Value::Text),
                }
                .unwrap_or(Value::Null)
            })
            .collect();
        Some(Feature { id, geometry: Some(geometry), attrs })
    }

    fn read_within(&self, req: &Request, timeout: Option<u64>) -> Result<Batch> {
        let limit = clamp_limit(req.limit);
        let mut out = Batch::default();
        if limit == 0 {
            return Ok(out);
        }
        if self.reproject.is_lonlat()
            && let Some(e) = req.extent
            && !e.intersects(&Extent::new(-180.0, -90.0, 180.0, 90.0))
        {
            return Ok(out);
        }
        let secs = timeout.unwrap_or(STATEMENT_TIMEOUT);
        let query = |sql: &str| self.conn.query_within(sql, &[], secs)?.ok_or_else(|| timed_out(secs));
        let window = native_window(req, self.info.extent, &self.reproject, self.geography, self.conn.compatibility);
        let filter = self.filter(window.as_deref());
        let from = self.table.sql_from();
        let cols = self.projection.select.iter().enumerate().map(|(i, e)| format!("{e} AS c{i}")).collect::<Vec<_>>().join(", ");
        let numbered = |every: &str| {
            format!(
                "SELECT TOP ({limit}) {cols}, [rustgis total] FROM (SELECT {}, ROW_NUMBER() OVER ({}) AS [rustgis rn], {} AS [rustgis total] FROM {from} t WHERE {filter}) s WHERE ([rustgis rn] - 1) % {every} = 0 ORDER BY [rustgis rn]",
                self.projection.raw,
                self.projection.order,
                if window.is_some() { "COUNT_BIG(*) OVER ()" } else { "0" }
            )
        };
        let (matched, rows) = if window.is_some() {
            let rows = query(&numbered(&format!("CAST(CEILING([rustgis total] / {limit}.0) AS BIGINT)")))?;
            let matched =
                rows.first().and_then(|r| int_of(at(r, self.projection.select.len()))).and_then(|n| u64::try_from(n).ok()).unwrap_or(0);
            (matched, rows)
        } else {
            // Concurrent edits may change the rows between this count and the read.
            let matched = first_count(&query(&format!("SELECT COUNT_BIG(*) FROM {from} WHERE {filter}"))?).unwrap_or(0);
            let step = stride(matched, limit);
            let sql = if step <= 1 && !self.projection.numbered {
                format!("SELECT TOP ({limit}) {cols} FROM (SELECT {} FROM {from} t WHERE {filter}) s", self.projection.raw)
            } else {
                numbered(&step.to_string())
            };
            (matched, query(&sql)?)
        };
        out.matched = matched;
        for r in &rows {
            let Some(f) = self.row_feature(r) else {
                out.skipped += 1;
                continue;
            };
            if f.geometry.as_ref().is_some_and(|g| in_request(g, req)) {
                out.features.push(f);
            }
        }
        out.complete = stride(matched, limit) <= 1 && rows.len() as u64 >= matched;
        Ok(out)
    }
}

/// The query that counts the rows of `from` matching `filter` whose geometry column `g` has Z or M
/// values.
fn zm_probe(from: &str, g: &str, filter: &str) -> String {
    format!("SELECT COUNT_BIG(*) FROM {from} WHERE {filter} AND ({g}.HasZ = 1 OR {g}.HasM = 1)")
}

/// Records what the Z/M count found: rows with Z or M values mark the source (with the count when
/// the server gave one), none says nothing, and a count that failed (`Err`) or ran out of time
/// (`Ok(None)`) gets a note saying the table wasn't checked and why.
fn note_zm(info: &mut SourceInfo, probe: Result<Option<Option<u64>>>) {
    match probe {
        Ok(Some(Some(0))) => {}
        Ok(Some(n)) => info.drop_zm(n),
        Ok(None) => info.notes.push(format!(
            "the table couldn't be checked for Z or M values (heights or measures) within {PROBE_TIMEOUT} s: any it has are dropped, as RustGIS keeps x and y only"
        )),
        Err(e) => info.notes.push(format!(
            "the table couldn't be checked for Z or M values (heights or measures): {e}; any it has are dropped, as RustGIS keeps x and y only"
        )),
    }
}

fn first_count(rows: &[Row]) -> Option<u64> {
    rows.first().and_then(|r| int_of(at(r, 0))).and_then(|n| u64::try_from(n).ok())
}

fn native_window(req: &Request, all: Extent, reproject: &Reproject, geography: bool, compatibility: u32) -> Option<String> {
    // At level 100 geography polygons cannot exceed a hemisphere; use local filtering instead.
    if geography && compatibility <= 100 {
        return None;
    }
    let e = req.extent?;
    if !all.is_empty() && e.min_x <= all.min_x && e.min_y <= all.min_y && e.max_x >= all.max_x && e.max_y >= all.max_y {
        return None;
    }
    box_text(&reproject.extent_from_lonlat(&e), geography)
}

/// A finite polygon literal. Geography needs a counter-clockwise ring, clear of poles and the
/// antimeridian, with subdivided geodesic edges and latitude padding as in the MySQL provider.
fn box_text(n: &Extent, geographic: bool) -> Option<String> {
    if n.is_empty() || ![n.min_x, n.min_y, n.max_x, n.max_y].iter().all(|v| v.is_finite()) {
        return None;
    }
    let steps = if geographic { ((n.width() / 10.0).ceil() as usize).clamp(1, 36) } else { 1 };
    let pad = if geographic { (n.width() / steps as f64).to_radians().powi(2).to_degrees() / 16.0 } else { 0.0 };
    let (a, b, c, d) = if geographic {
        (n.min_x.max(-179.9999), (n.min_y - pad).max(-89.9999), n.max_x.min(179.9999), (n.max_y + pad).min(89.9999))
    } else {
        (n.min_x, n.min_y, n.max_x, n.max_y)
    };
    if a > c || b > d {
        return None;
    }
    let (a, b, c, d) = (
        if geographic && a == c { a - 1e-9 } else { a },
        if geographic && b == d { b - 1e-9 } else { b },
        if c > a || geographic { c } else { a + 1e-9 },
        if d > b || geographic { d } else { b + 1e-9 },
    );
    let x = |i: usize| a + (c - a) * i as f64 / steps as f64;
    let mut ring: Vec<String> = (0..=steps).map(|i| format!("{} {b}", x(i))).collect();
    ring.extend((0..=steps).map(|i| format!("{} {d}", x(steps - i))));
    ring.push(format!("{a} {b}"));
    Some(format!("POLYGON(({}))", ring.join(", ")))
}

impl FeatureSource for SqlServerSource {
    fn info(&self) -> &SourceInfo {
        &self.info
    }
    fn read(&self, req: &Request) -> Result<Batch> {
        self.read_within(req, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_columns_cannot_shadow_numbering_or_count() {
        // The projection and window syntax is shared with SQLite; exercise real name resolution
        // locally without needing a SQL Server or evaluating its spatial methods.
        let db = rusqlite::Connection::open_in_memory().unwrap();
        let columns = [("rustgis rn", "geometry"), ("rustgis total", "int"), ("i0", "int"), ("c0", "int")]
            .map(|(name, ty)| (name.to_string(), ty.to_string()));
        let (_, projection) = projection("rustgis rn", columns, Some("rustgis total"));
        let id = projection.select.first().unwrap();
        let sql = format!(
            "SELECT {id}, i0, i1, i2, i3, [rustgis rn], [rustgis total] FROM \
             (SELECT {}, ROW_NUMBER() OVER ({}) AS [rustgis rn], COUNT(*) OVER () AS [rustgis total] \
             FROM (SELECT 101 AS [rustgis rn], 37 AS [rustgis total], 55 AS i0, 77 AS c0 \
                   UNION ALL SELECT 202, 19, 66, 88) t) s ORDER BY [rustgis rn]",
            projection.raw, projection.order
        );
        let mut statement = db.prepare(&sql).unwrap();
        let rows: Vec<[i64; 7]> = statement
            .query_map([], |r| Ok([r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?]))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(rows, [[19, 202, 19, 66, 88, 1, 2], [37, 101, 37, 55, 77, 2, 2]]);
    }

    #[test]
    fn addresses_take_secure_defaults_and_logins() {
        let c = config("mssql://sa@h/gis").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!((c.port, c.encrypt, c.trust_cert, c.login), (Some(1433), Encrypt::Required, false, Login::Sql("sa".into())));
        assert_eq!(config("mssql://CORP%5Cana@h").map(|c| c.login).ok(), Some(Login::Windows("CORP\\ana".into())));
        assert_eq!(config(r"mssql://CORP\ana@h").map(|c| c.login).ok(), Some(Login::Windows("CORP\\ana".into())));
        assert_eq!(config("mssql://h").map(|c| c.login).ok(), Some(Login::Integrated));
        assert_eq!(config("mssql://u@h?instance=SQLEXPRESS").map(|c| c.port).ok(), Some(None));
        let c = config("MSSQL://u@[::1]:1500/my%20db?ENCRYPT=strict&trust_server_certificate=yes&ca=/x%20y.pem&hostname-in-certificate=db&connect-timeout=2").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            (c.host.as_str(), c.port, c.database.as_deref(), c.encrypt, c.trust_cert),
            ("::1", Some(1500), Some("my db"), Encrypt::Strict, true)
        );
        assert_eq!(
            (c.ca, c.hostname_in_certificate, c.connect_timeout),
            (Some("/x y.pem".into()), Some("db".into()), Duration::from_secs(2))
        );
        assert_eq!(
            config("mssql://?connect-timeout=0&encrypt=optional").map(|c| (c.host, c.connect_timeout, c.encrypt)).ok(),
            Some(("localhost".into(), CONNECT_TIMEOUT, Encrypt::Off))
        );
        for a in ["mssql://h?encrypt=nope", "mssql://h:port", "mssql://[::1", "postgres:host=h", "mssql://h?trust-server-certificate=maybe"]
        {
            assert!(config(a).is_err(), "{a}");
        }
    }

    #[test]
    fn connection_deadlines_are_bounded() {
        for (given, expected) in [("1", 1), ("300", 300), ("301", 300), ("18446744073709551615", 300), ("0", 10), ("invalid", 10)] {
            let c = config(&format!("mssql://sa@h/gis?connect-timeout={given}")).unwrap();
            assert_eq!(c.connect_timeout, Duration::from_secs(expected), "{given}");
        }
    }

    #[test]
    fn values_convert() {
        assert_eq!(int_of(&ColumnData::I32(Some(42))), Some(42));
        assert_eq!(id_of(&ColumnData::I64(Some(-1))), Some(u64::MAX));
        assert_eq!(float_of(&ColumnData::F64(Some(f64::NAN))), None);
        assert_eq!(int_of(&ColumnData::F64(Some(2.5))), None);
        assert_eq!(int_of(&ColumnData::String(Some("42".into()))), Some(42));
        assert_eq!(text_of(&ColumnData::String(Some("Tōkyō".into()))).as_deref(), Some("Tōkyō"));
        assert_eq!(bool_of(&ColumnData::Bit(Some(true))), Some(true));
        assert_eq!(bytes_of(&ColumnData::Binary(Some((&b"wkb"[..]).into()))), Some(&b"wkb"[..]));
    }

    #[test]
    fn types_and_identifiers() {
        assert_eq!(ident("a]b"), "[a]]b]");
        for t in ["geometry", "geography", "image", "binary", "varbinary", "timestamp", "rowversion"] {
            assert_eq!(field_cast(t, "[c]"), None);
        }
        assert_eq!(field_cast("bit", "[c]"), Some((FieldType::Boolean, "[c]".into())));
        for t in ["tinyint", "smallint", "int", "bigint"] {
            assert_eq!(field_cast(t, "[c]"), Some((FieldType::Integer, "CAST([c] AS BIGINT)".into())));
        }
        for t in ["real", "float", "decimal", "numeric", "money", "smallmoney"] {
            assert_eq!(field_cast(t, "[c]"), Some((FieldType::Double, "CAST([c] AS FLOAT)".into())));
        }
        for t in ["date", "datetime", "datetime2", "smalldatetime", "datetimeoffset", "time"] {
            assert_eq!(field_cast(t, "[c]"), Some((FieldType::Text, "CONVERT(NVARCHAR(40), [c], 126)".into())));
        }
        assert_eq!(field_cast("uniqueidentifier", "[c]"), Some((FieldType::Text, "CAST([c] AS NVARCHAR(MAX))".into())));
    }

    #[test]
    fn native_windows_respect_database_compatibility() {
        let reproject = Reproject::from_crs(Crs::LonLat, &mut Vec::new());
        let req = Request { extent: Some(Extent::new(-180.0, -90.0, 180.0, 90.0)), limit: 100 };
        for compatibility in [0, 80, 90, 100] {
            assert_eq!(native_window(&req, Extent::EMPTY, &reproject, true, compatibility), None);
        }
        for compatibility in [110, 120, 160] {
            let window = native_window(&req, Extent::EMPTY, &reproject, true, compatibility).unwrap_or_default();
            assert!(window.starts_with("POLYGON((-179.9999 -89.9999, "), "{window}");
            assert!(window.ends_with(", -179.9999 -89.9999))"), "{window}");
        }
        for compatibility in [0, 80, 90, 100, 110, 120, 160] {
            assert_eq!(
                native_window(&req, Extent::EMPTY, &reproject, false, compatibility).as_deref(),
                Some("POLYGON((-180 -90, 180 -90, 180 90, -180 90, -180 -90))"),
            );
        }
        assert_eq!(native_window(&req, Extent::new(-10.0, -10.0, 10.0, 10.0), &reproject, true, 110), None);
        assert_eq!(native_window(&Request { extent: None, limit: 100 }, Extent::EMPTY, &reproject, true, 110), None);
    }

    #[test]
    fn boxes_keep_clear_of_geodesic_edges() {
        assert_eq!(box_text(&Extent::new(0.0, 0.0, 1000.0, 500.0), false).as_deref(), Some("POLYGON((0 0, 1000 0, 1000 500, 0 500, 0 0))"));
        for e in [Extent::new(-200.0, -95.0, 200.0, 95.0), Extent::new(-180.0, -60.0, 0.0, 0.0)] {
            let polygon = box_text(&e, true).unwrap_or_default();
            let points: Vec<(f64, f64)> = polygon
                .trim_start_matches("POLYGON((")
                .trim_end_matches("))")
                .split(", ")
                .filter_map(|p| p.split_once(' ').and_then(|(x, y)| Some((x.parse().ok()?, y.parse().ok()?))))
                .collect();
            assert_eq!(points.first(), points.last());
            assert!(points.iter().all(|(x, y)| x.abs() <= 179.9999 && y.abs() <= 89.9999));
            let area: f64 = points.windows(2).map(|p| p[0].0 * p[1].1 - p[1].0 * p[0].1).sum();
            assert!(area > 0.0, "ring must be counter-clockwise: {polygon}");
        }
        assert_eq!(box_text(&Extent::EMPTY, true), None);
        assert_eq!(box_text(&Extent::new(190.0, 0.0, 200.0, 10.0), true), None);
        assert_eq!(box_text(&Extent::new(0.0, 0.0, f64::INFINITY, 1.0), false), None);
    }

    #[test]
    fn zm_probe_counts_heights_and_measures() {
        assert_eq!(
            zm_probe("[dbo].[t]", "[g]", "[g] IS NOT NULL AND [g].STSrid = 4326"),
            "SELECT COUNT_BIG(*) FROM [dbo].[t] WHERE [g] IS NOT NULL AND [g].STSrid = 4326 AND ([g].HasZ = 1 OR [g].HasM = 1)"
        );
    }

    #[test]
    fn zm_counts_become_notes() {
        let noted = |probe: Result<Option<Option<u64>>>| {
            let mut info = SourceInfo {
                name: "t".into(),
                format: "SQL Server",
                geometry_type: GeometryType::Point,
                fields: vec![],
                extent: Extent::EMPTY,
                count: Some(3),
                crs: "EPSG:4326".into(),
                indexed: false,
                notes: vec![],
                zm_dropped: false,
            };
            note_zm(&mut info, probe);
            info
        };
        let none = noted(Ok(Some(Some(0))));
        assert!(!none.zm_dropped && none.notes.is_empty(), "{:?}", none.notes);
        let two = noted(Ok(Some(Some(2))));
        assert!(two.zm_dropped && two.notes == [crate::zm_note(Some(2))], "{:?}", two.notes);
        let uncounted = noted(Ok(Some(None)));
        assert!(uncounted.zm_dropped && uncounted.notes == [crate::zm_note(None)], "{:?}", uncounted.notes);
        let slow = noted(Ok(None));
        assert!(!slow.zm_dropped && slow.notes.len() == 1 && slow.notes[0].contains("within 10 s"), "{:?}", slow.notes);
        let failed = noted(Err(DataError::Format("permission denied".into())));
        assert!(!failed.zm_dropped && failed.notes.len() == 1 && failed.notes[0].contains("permission denied"), "{:?}", failed.notes);
    }

    #[cfg(not(windows))]
    #[test]
    fn windows_auth_requires_windows() {
        assert!(auth_method(&Login::Windows("CORP\\ana".into()), Some("pw")).is_err());
        assert!(auth_method(&Login::Integrated, None).is_err());
    }

    /// `RUSTGIS_TEST_MSSQL=mssql://user:password@host/database?... cargo test -- --ignored sqlserver_live --nocapture`.
    #[test]
    #[ignore]
    fn sqlserver_live() {
        let Ok(address) = std::env::var("RUSTGIS_TEST_MSSQL") else { return };
        let super::super::duckdb::Database::Sqlserver(address) = super::super::duckdb::Database::parse(&address) else {
            panic!("not a connection")
        };
        let conn = connect(&address, true).unwrap_or_else(|e| panic!("{e}"));
        *conn.client.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let all = tables(&conn).unwrap_or_else(|e| panic!("after a dropped connection: {e}"));
        assert!(!all.is_empty());
        for t in &all {
            let fams = families(&conn, t).unwrap_or_else(|e| panic!("{}: {e}", t.label()));
            for f in fams {
                let src = SqlServerSource::open(conn.clone(), t.clone(), Some(f), None).unwrap_or_else(|e| panic!("{}: {e}", t.label()));
                let i = src.info();
                let whole = src.read(&Request { extent: None, limit: 250_000 }).unwrap_or_else(|e| panic!("{}: {e}", t.label()));
                let e = i.extent;
                let q = Extent::new(e.min_x, e.min_y, e.min_x + e.width() / 2.0, e.min_y + e.height() / 2.0);
                let part = src.read(&Request { extent: Some(q), limit: 1000 }).unwrap_or_else(|e| panic!("{}: {e}", t.label()));
                for ft in &part.features {
                    let g = ft.geometry.as_ref().map(|g| g.extent()).unwrap_or(Extent::EMPTY);
                    assert!(g.intersects(&q.padded(0.01, 0.0)), "{}: feature {} outside the request", t.label(), ft.id);
                }
                let mut ids: Vec<u64> = whole.features.iter().map(|f| f.id).collect();
                ids.sort_unstable();
                ids.dedup();
                assert_eq!(ids.len(), whole.features.len(), "{}: ids repeat", t.label());
                let sample = src.read(&Request { extent: None, limit: 1 }).unwrap_or_else(|e| panic!("{e}"));
                assert!(sample.features.len() <= 1);
                assert_eq!(sample.matched, whole.matched);
                println!(
                    "{} {} rows={:?} idx={} crs={} extent={:?} all={} matched={} complete={} skipped={} quarter={} notes={:?} features={:?}",
                    t.label(),
                    f.label(),
                    i.count,
                    i.indexed,
                    i.crs,
                    e,
                    whole.features.len(),
                    whole.matched,
                    whole.complete,
                    whole.skipped,
                    part.features.len(),
                    i.notes,
                    whole.features
                );
            }
        }
    }

    /// `RUSTGIS_TEST_MSSQL=… cargo test -- --ignored sqlserver_live_zm`: creates and drops two
    /// tables in the database.
    #[test]
    #[ignore]
    fn sqlserver_live_zm() {
        let Ok(address) = std::env::var("RUSTGIS_TEST_MSSQL") else { return };
        let super::super::duckdb::Database::Sqlserver(address) = super::super::duckdb::Database::parse(&address) else {
            panic!("not a connection")
        };
        let conn = connect(&address, true).unwrap_or_else(|e| panic!("{e}"));
        let run = |sql: &str| conn.query(sql, &[]).unwrap_or_else(|e| panic!("{sql}: {e}"));
        // Names of their own, so the test never touches an existing table: CREATE TABLE fails on one.
        let tag = std::process::id();
        let (zm_name, flat_name) = (format!("rustgis_zm_test_{tag}"), format!("rustgis_flat_test_{tag}"));
        for (name, rows) in
            [(&zm_name, &["POINT (1 2 3)", "POINT (1 2 NULL 4)", "POINT (5 6)"][..]), (&flat_name, &["POINT (1 2)", "POINT (5 6)"])]
        {
            run(&format!("CREATE TABLE dbo.{name} (id INT PRIMARY KEY IDENTITY, g geometry)"));
            let values = rows.iter().map(|w| format!("(geometry::STGeomFromText('{w}', 4326))")).collect::<Vec<_>>().join(", ");
            run(&format!("INSERT INTO dbo.{name} (g) VALUES {values}"));
        }
        let open = |name: &str| {
            let t = find_table(&conn, name).unwrap_or_else(|e| panic!("{e}"));
            SqlServerSource::open(conn.clone(), t, Some(Family::Points), None).unwrap_or_else(|e| panic!("{e}")).info
        };
        let zm = open(&zm_name);
        let flat = open(&flat_name);
        run(&format!("DROP TABLE dbo.{zm_name}; DROP TABLE dbo.{flat_name}"));
        assert!(zm.zm_dropped && zm.notes.contains(&crate::zm_note(Some(2))), "{:?}", zm.notes);
        assert!(!flat.zm_dropped && !flat.notes.iter().any(|n| n.contains("Z or M")), "{:?}", flat.notes);
    }
}
