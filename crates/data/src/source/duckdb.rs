//! DuckDB, optionally: SQL queries (with DuckDB Spatial's `ST_` functions, over files, web
//! addresses and the tables of a DuckDB database) and the tables of DuckDB database files, run
//! through the `duckdb` program when it is installed.
//!
//! A query's result is written as GeoParquet in the cache folder, in longitude/latitude, sorted
//! along a Hilbert curve and with a bounding-box covering, so the GeoParquet provider reads a large
//! one a view at a time. A result mixing geometry types becomes one file per type. Nothing links to
//! DuckDB: without the program, queries report how to get it.
//!
//! The SQL reaches DuckDB only as a string literal handed to its `query()` table function, which
//! runs a single SELECT: it can't end the statement, run other statements (`COPY`, `ATTACH`,
//! `INSTALL`) or the shell's dot commands. It can still read any file and web address, as it is
//! meant to: the AI Assistant asks before running a query, and plugins need both the files and
//! network permissions (`rustgis_engine::cmd::plugins::SQL`).

use super::Family;
pub use super::external::ReloadWorkspace;
use super::external::{self, TempFile, fnv1a, stamp, touch};
use crate::secrets::Secret;
use crate::{DataError, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// DuckDB database files.
pub const EXTENSIONS: &[&str] = &["duckdb", "ddb"];

/// Longest SQL accepted (the command line it travels on is limited, on Windows to 32K characters).
pub const MAX_SQL: usize = 12_000;

/// Most tables of a database added at once.
pub const MAX_TABLES: usize = 64;

/// Most tables of a database listed.
const LIST_LIMIT: usize = 10_000;

/// The longest a query may run, and the longest a table listing may.
const QUERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30 * 60);
const LIST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// Results not used for this long are removed from the cache.
const KEEP: std::time::Duration = std::time::Duration::from_secs(30 * 24 * 3600);

/// The column added to sort rows into geometry families; never in a result.
const DIM: &str = "__rustgis_dim";
/// The column added to count the rows whose geometry had Z or M values, which the conversion
/// drops; never in a result.
const ZM: &str = "__rustgis_zm";

/// What a converted table is written as: raised when that changes (dates as dates in 2, nanosecond
/// timestamps as timestamps in 3, the rows that lost Z or M values counted in 4, kept in its own
/// coordinate system in 5), so a table converted before is converted again rather than kept as it
/// was.
const CONVERSION: u32 = 5;

/// A database named by commands: DuckDB files, PostgreSQL connections (also queryable through
/// DuckDB's `postgres` extension), or live-only MySQL, MariaDB and SQL Server connections.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Database {
    File(PathBuf),
    /// A libpq connection string (`host=… dbname=… user=…`) or `postgresql://` URI, without its
    /// password (see [`remember_password`]).
    Postgres(String),
    /// A `mysql://` or `mariadb://` address, without its password: its tables are read live by
    /// [`super::mysql`], not through DuckDB.
    Mysql(String),
    /// An `mssql://` address, without its password: read live by [`super::sqlserver`].
    Sqlserver(String),
}

impl Database {
    /// A database by how projects and commands name it: `postgres:<connection string>`, a
    /// `postgresql://`, `mysql://`, `mariadb://` or `mssql://` address, or a file path.
    /// A password in it is taken out and remembered for the session.
    pub fn parse(s: &str) -> Database {
        let t = s.trim();
        if let Some((public, password)) = super::msconn::split_password(t) {
            if let Some(p) = password {
                remember_password(&public, &p);
            }
            return Database::Sqlserver(public);
        }
        if let Some((public, password)) = super::myconn::split_password(t) {
            if let Some(p) = password {
                remember_password(&public, &p);
            }
            return Database::Mysql(public);
        }
        let lower = t.to_ascii_lowercase();
        let dsn = if let Some(rest) =
            t.get(..9).filter(|p| p.eq_ignore_ascii_case("postgres:")).and_then(|_| t.get(9..)).filter(|r| !r.starts_with("//"))
        {
            Some(rest.trim().to_string())
        } else if lower.starts_with("postgresql://") || lower.starts_with("postgres://") {
            Some(t.to_string())
        } else {
            None
        };
        match dsn {
            Some(d) => {
                let (public, password) = split_password(&d);
                if let Some(p) = password {
                    remember_password(&public, &p);
                }
                Database::Postgres(public)
            }
            None => Database::File(PathBuf::from(t)),
        }
    }

    /// How projects save it (never with a password).
    pub fn label(&self) -> String {
        match self {
            Database::File(p) => p.display().to_string(),
            Database::Postgres(d) if d.contains("://") => d.clone(),
            Database::Postgres(d) => format!("postgres:{d}"),
            Database::Mysql(a) | Database::Sqlserver(a) => a.clone(),
        }
    }

    /// What a layer's name and messages call it: the file, or the database and host.
    pub fn short(&self) -> String {
        match self {
            Database::File(p) => p.file_stem().and_then(|s| s.to_str()).unwrap_or("database").to_string(),
            Database::Postgres(d) => {
                // Quoted values (`dbname='my gis'`) as libpq reads them.
                super::pgconn::keyword_pairs(d)
                    .into_iter()
                    .find_map(|(k, v)| (k == "dbname").then_some(v))
                    .unwrap_or_else(|| d.rsplit('/').next().unwrap_or("postgres").split('?').next().unwrap_or("postgres").to_string())
            }
            Database::Mysql(a) => super::myconn::database_name(a).unwrap_or_else(|| "mysql".into()),
            Database::Sqlserver(a) => super::msconn::database_name(a).unwrap_or_else(|| "sqlserver".into()),
        }
    }
}

/// A connection string without its password, and the password.
fn split_password(dsn: &str) -> (String, Option<String>) {
    super::pgconn::split_secrets(dsn, &["password"])
}

pub use super::pgconn::public_label;

/// A password remembered for a connection's address. `connection_id` names the saved connection
/// it was activated for (see [`set_connection_password`]); `None` is one typed for the address.
struct Entry {
    password: Secret,
    connection_id: Option<String>,
}

type Passwords = std::collections::HashMap<String, Entry>;

static PASSWORDS: std::sync::LazyLock<std::sync::Mutex<Passwords>> = std::sync::LazyLock::new(Default::default);

fn lock() -> std::sync::MutexGuard<'static, Passwords> {
    PASSWORDS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Remembers a connection's password for this session, as typed for its address (the saved
/// connections' own are activated by [`set_connection_password`]): projects name connections
/// without them, and libpq's `PGPASSWORD` or `~/.pgpass` serve when it isn't remembered.
pub fn remember_password(dsn: &str, password: &str) {
    lock().insert(dsn.to_string(), Entry { password: Secret::new(password), connection_id: None });
}

/// Makes saved connection `id`'s password the one connections to `dsn` use (`Some`), replacing
/// whatever was remembered for the address; or takes away a password a saved connection
/// activated there, whichever one (`None`), leaving one typed for the address.
pub fn set_connection_password(dsn: &str, id: &str, password: Option<&str>) {
    let mut all = lock();
    match password {
        Some(p) => {
            all.insert(dsn.to_string(), Entry { password: Secret::new(p), connection_id: Some(id.to_string()) });
        }
        None => {
            if all.get(dsn).is_some_and(|e| e.connection_id.is_some()) {
                all.remove(dsn);
            }
        }
    }
}

/// Whether this driver's exact address slot is currently owned by saved connection `id`.
/// Unlike runtime selection, this also accounts for a later explicitly typed address password.
pub fn has_connection_password(dsn: &str, id: &str) -> bool {
    lock().get(dsn).is_some_and(|entry| entry.connection_id.as_deref() == Some(id))
}

/// Tries an explicitly supplied password, restoring the previous address slot and its ownership
/// when `refused` classifies the outcome as an authentication refusal. Other failures retain
/// the new password. Never holds the cache lock during network I/O.
pub fn try_connection_password<T>(dsn: &str, id: &str, password: &str, run: impl FnOnce() -> T, refused: impl FnOnce(&T) -> bool) -> T {
    let previous = lock().insert(dsn.to_string(), Entry { password: Secret::new(password), connection_id: Some(id.to_string()) });
    let result = run();
    if refused(&result) {
        let mut all = lock();
        // A concurrent login or Forget supersedes this attempt, even if the server refused it.
        if all.get(dsn).is_some_and(|entry| entry.connection_id.as_deref() == Some(id) && entry.password.expose() == password) {
            if let Some(previous) = previous {
                all.insert(dsn.to_string(), previous);
            } else {
                all.remove(dsn);
            }
        }
    }
    result
}

/// Takes away every password saved connection `id` activated.
pub fn forget_connection_password(id: &str) {
    lock().retain(|_, e| e.connection_id.as_deref() != Some(id));
}

/// Takes away every password a saved connection activated, leaving the typed ones.
pub fn forget_connection_passwords() {
    lock().retain(|_, e| e.connection_id.is_none());
}

/// The password remembered for a connection this session, if any.
pub(crate) fn remembered_password(dsn: &str) -> Option<String> {
    lock().get(dsn).map(|e| e.password.expose().to_string())
}

/// The password a database client's own environment variable gives for `kind` (`postgres`:
/// `PGPASSWORD`, `mysql`: `MYSQL_PWD`, `sqlserver`: `SQLCMDPASSWORD`); an empty one is none. The
/// one rule the drivers' explicit connections and the saved connections share, so an ambient
/// password ranks the same wherever it is asked for.
pub fn ambient_password(kind: &str) -> Option<String> {
    ambient_password_in(kind, |name| std::env::var(name).ok())
}

fn ambient_password_in(kind: &str, get: impl Fn(&str) -> Option<String>) -> Option<String> {
    let variable = match kind {
        "postgres" => "PGPASSWORD",
        "mysql" => "MYSQL_PWD",
        "sqlserver" => "SQLCMDPASSWORD",
        _ => return None,
    };
    get(variable).filter(|p| !p.is_empty())
}

/// Whether a password is remembered for a connection this session, typed or activated.
pub fn has_password(dsn: &str) -> bool {
    lock().contains_key(dsn)
}

/// Whether a password typed for this address (not one a saved connection activated) is remembered.
pub fn has_explicit_password(dsn: &str) -> bool {
    lock().get(dsn).is_some_and(|e| e.connection_id.is_none())
}

/// The remembered password of a query's database, handed to DuckDB's PostgreSQL client in its
/// environment (`PGPASSWORD`): never in the script, where the process list and DuckDB's errors
/// would show it.
fn password(database: Option<&Database>) -> Option<String> {
    match database? {
        Database::Postgres(dsn) => remembered_password(dsn),
        Database::File(_) | Database::Mysql(_) | Database::Sqlserver(_) => None,
    }
}

/// The error of a query (or table) without rows.
pub const NO_ROWS: &str = "the query returned no rows";

/// Whether a file name is a DuckDB database.
pub fn is_duckdb_name(name: &str) -> bool {
    let ext = Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    EXTENSIONS.contains(&ext.as_str())
}

/// The `duckdb` program: in the folder `RUSTGIS_DUCKDB_BIN` names, else on the `PATH`.
pub fn program() -> Option<PathBuf> {
    external::program("RUSTGIS_DUCKDB_BIN", "duckdb")
}

/// Whether DuckDB is installed.
pub fn available() -> bool {
    program().is_some()
}

/// Whether a test that needs DuckDB runs: skipped without it, unless `RUSTGIS_REQUIRE_DUCKDB` is
/// set, when it fails on the `Err` instead.
pub fn test_runs() -> std::result::Result<bool, String> {
    external::test_runs("RUSTGIS_REQUIRE_DUCKDB", "DuckDB (the duckdb program)", available())
}

/// A live-only database named for a query: add its tables instead.
fn live_not_here() -> DataError {
    DataError::Unsupported(
        "SQL queries don't run on MySQL, MariaDB or SQL Server databases: add their tables with Home › Add › Add Data › Database…".into(),
    )
}

fn missing() -> DataError {
    DataError::Unsupported(missing_text(crate::no_programs_here()))
}

/// Why a query or a DuckDB database can't be read: DuckDB isn't installed, or (`here` names it)
/// this build runs no programs, where installing it wouldn't help.
fn missing_text(here: &str) -> String {
    if here.is_empty() {
        "SQL queries and DuckDB databases need DuckDB: install it (the duckdb program on the PATH, or RUSTGIS_DUCKDB_BIN naming its folder) and try again"
            .into()
    } else {
        format!(
            "SQL queries and DuckDB databases run through DuckDB, a program {here} can't run: open this project on a computer to use them"
        )
    }
}

/// Where results are kept: the platform's cache folder, or `RUSTGIS_DUCKDB_CACHE`.
pub fn cache_dir() -> Option<PathBuf> {
    external::cache_dir("RUSTGIS_DUCKDB_CACHE", "duckdb")
}

/// A query to run.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Query<'a> {
    /// One SELECT statement (or `FROM …`, `WITH …`, `VALUES …`).
    pub sql: &'a str,
    /// A database opened (read only) for the query, so its tables are named plainly.
    pub database: Option<&'a Database>,
    /// The coordinate system of a geometry column that doesn't say its own (`EPSG:2263`).
    pub crs: Option<&'a str>,
}

/// One geometry family of a result, as GeoParquet.
#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    pub path: PathBuf,
    pub family: Family,
    pub count: u64,
    /// Its rows whose geometry had Z or M values, written with x and y only.
    pub zm_dropped: u64,
}

/// A result: a part per geometry family it holds.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Output {
    pub parts: Vec<Part>,
    /// Rows without a geometry, left out.
    pub skipped: u64,
    /// Rows of unselected geometry families, left out of a manual replacement.
    pub other_family: u64,
}

/// A table of a database with a geometry column.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Table {
    pub schema: String,
    pub name: String,
}

impl Table {
    /// Its name, with its schema unless that is `main`.
    pub fn label(&self) -> String {
        if self.schema == "main" { self.name.clone() } else { format!("{}.{}", self.schema, self.name) }
    }

    fn sql(&self) -> String {
        format!("FROM {}.{}", ident(&self.schema), ident(&self.name))
    }
}

/// An SQL string literal.
fn literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// An SQL identifier.
fn ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// Checks a coordinate system given by name.
fn check_crs(crs: Option<&str>) -> Result<()> {
    match crs {
        Some(c) if c.is_empty() || c.len() > 64 || !c.chars().all(|ch| ch.is_ascii_alphanumeric() || ":_.".contains(ch)) => {
            Err(DataError::Format(format!("crs '{c}': give a code such as EPSG:2263")))
        }
        _ => Ok(()),
    }
}

/// libpq parameters that read or write local files or inject server start-up options, refused when
/// handing a connection string to DuckDB's `postgres` extension.
const REFUSED_PG_PARAMS: &[&str] =
    &["sslcert", "sslkey", "sslkeylogfile", "sslrootcert", "sslcrl", "sslcrldir", "passfile", "service", "servicefile", "options"];

fn pg_param_named(raw_key: &str) -> bool {
    let trimmed = raw_key.trim();
    REFUSED_PG_PARAMS.iter().any(|p| trimmed.eq_ignore_ascii_case(p))
        || crate::address::normalized_name(trimmed).is_some_and(|norm| REFUSED_PG_PARAMS.contains(&norm.as_str()))
}

fn pg_param_refused(raw_key: &str) -> bool {
    pg_param_named(raw_key) || crate::address::normalized_name(raw_key.trim()).is_none()
}

fn check_pg_query(query: &str, strict: bool) -> Result<()> {
    let refused = if strict { pg_param_refused } else { pg_param_named };
    for kv in query.split(['&', ';']).filter(|kv| !kv.is_empty()) {
        let (k, _) = kv.split_once('=').unwrap_or((kv, ""));
        let decoded_kv = super::pgconn::percent_decode(kv);
        let (dk, _) = decoded_kv.split_once('=').unwrap_or((&decoded_kv, ""));
        if refused(k) || refused(dk) {
            let shown = dk.trim();
            return Err(DataError::Format(format!("the connection: parameter '{shown}' isn't allowed")));
        }
    }
    Ok(())
}

/// Validates the host list of a PostgreSQL URI (including `[ipv6]` and comma-separated hosts) and
/// returns the remainder starting at `/`, `?`, or the end of `s`.
fn after_uri_hosts(mut s: &str) -> Result<&str> {
    loop {
        if let Some(after_open) = s.strip_prefix('[') {
            let Some((inside, after_close)) = after_open.split_once(']') else {
                return Err(DataError::Format("the connection: invalid connection string".into()));
            };
            if inside.is_empty() || inside.contains(['?', '/', '#', '&', '=', ';', '@', '[']) {
                return Err(DataError::Format("the connection: invalid connection string".into()));
            }
            s = after_close;
        }
        let next = s.find([',', '/', '?', '[']).unwrap_or(s.len());
        if s.get(..next).unwrap_or("").contains('@') {
            return Err(DataError::Format("the connection: invalid connection string".into()));
        }
        match s.as_bytes().get(next).copied() {
            Some(b'[') => {
                return Err(DataError::Format("the connection: invalid connection string".into()));
            }
            Some(b',') => {
                s = s.get(next + 1..).unwrap_or("");
            }
            _ => return Ok(s.get(next..).unwrap_or("")),
        }
    }
}

/// Checks a PostgreSQL connection string before handing it to DuckDB's `ATTACH … (TYPE postgres)`.
fn check_postgres_attach(dsn: &str) -> Result<()> {
    if dsn.contains('\0') {
        return Err(DataError::Format("the connection: NUL character isn't allowed".into()));
    }
    let t = dsn.trim();
    let lower = t.to_ascii_lowercase();
    if lower.starts_with("postgresql://") || lower.starts_with("postgres://") {
        let Some((_, rest)) = t.split_once("://") else {
            return Err(DataError::Format("the connection: invalid connection string".into()));
        };
        let before_slash = rest.split('/').next().unwrap_or(rest);
        if let Some(at) = before_slash.find('@') {
            let user_part = before_slash.get(..at).unwrap_or("");
            let after_at = before_slash.get(at + 1..).unwrap_or("");
            let host_part = after_at.split('?').next().unwrap_or("");
            if user_part.contains(['[', ']']) || host_part.contains('@') || (rest.contains('/') && after_at.contains('@')) {
                return Err(DataError::Format("the connection: invalid connection string".into()));
            }
            if !rest.contains('/')
                && let Some((_, after_last_at)) = after_at.rsplit_once('@')
                && let Some((_, q2)) = after_last_at.split_once('?')
            {
                check_pg_query(q2, true)?;
            }
            let tail = after_uri_hosts(rest.get(at + 1..).unwrap_or(""))?;
            if let Some((_, query)) = tail.split_once('?') {
                check_pg_query(query, true)?;
            }
            if let Some((_, pre_q)) = user_part.split_once('?') {
                check_pg_query(pre_q, false)?;
                if !rest.contains('/')
                    && let Some((_, full_q)) = rest.split_once('?')
                {
                    check_pg_query(full_q, false)?;
                }
            }
        } else {
            let tail = after_uri_hosts(rest)?;
            if let Some((_, query)) = tail.split_once('?') {
                check_pg_query(query, true)?;
            }
        }
        return Ok(());
    }
    let mut chars = t.chars().peekable();
    loop {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        if chars.peek().is_none() {
            break;
        }
        let mut key = String::new();
        while let Some(c) = chars.next_if(|c| !c.is_whitespace() && *c != '=') {
            key.push(c);
        }
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        if key.is_empty() || chars.next() != Some('=') {
            return Err(DataError::Format("the connection: invalid connection string".into()));
        }
        if pg_param_refused(&key) {
            return Err(DataError::Format(format!("the connection: parameter '{key}' isn't allowed")));
        }
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        let mut val = String::new();
        if chars.next_if_eq(&'\'').is_some() {
            let mut closed = false;
            while let Some(c) = chars.next() {
                match c {
                    '\\' => match chars.next() {
                        Some(esc) => val.push(esc),
                        None => return Err(DataError::Format("the connection: unterminated quote".into())),
                    },
                    '\'' => {
                        closed = true;
                        break;
                    }
                    other => val.push(other),
                }
            }
            if !closed {
                return Err(DataError::Format("the connection: unterminated quote".into()));
            }
        } else {
            while let Some(c) = chars.next_if(|c| !c.is_whitespace()) {
                if c == '\\' {
                    if let Some(esc) = chars.next() {
                        val.push(esc);
                    }
                } else {
                    val.push(c);
                }
            }
        }
        if key.eq_ignore_ascii_case("dbname") {
            let vt = val.trim();
            let vl = vt.to_ascii_lowercase();
            if vl.starts_with("postgresql://") || vl.starts_with("postgres://") || vt.contains('=') {
                return Err(DataError::Format("the connection: dbname may not be a connection string".into()));
            }
        }
    }
    Ok(())
}

/// The statements every run starts with: DuckDB Spatial (installed the first time), x/y axis
/// order whatever the coordinate system, and the database, when there is one.
///
/// `confined` shuts DuckDB out of every file and web address but the cache folder once the
/// database is open: a database file is hostile input, and its views can hold any SQL. Tables are
/// read that way, so adding a database (or a project naming one) reads nothing else.
fn prelude(dir: &Path, database: Option<&Database>, confined: bool) -> Result<String> {
    let tmp = dir.join("spill");
    let mut s = format!(
        "INSTALL spatial; LOAD spatial; SET geometry_always_xy = true; SET enable_progress_bar = false; SET temp_directory = {};",
        literal(&tmp.display().to_string())
    );
    match database {
        Some(Database::File(db)) => {
            let full = std::path::absolute(db).unwrap_or_else(|_| db.to_path_buf());
            if !full.is_file() {
                return Err(DataError::Format(format!("{}: no such database", db.display())));
            }
            crate::provenance::duckdb(&full);
            s.push_str(&format!(" ATTACH {} AS src (READ_ONLY); USE src;", literal(&full.display().to_string())));
        }
        // PostGIS geometry columns come through as DuckDB geometries with their coordinate system.
        Some(Database::Postgres(dsn)) => {
            check_postgres_attach(dsn)?;
            s.push_str(&format!(" INSTALL postgres; LOAD postgres; ATTACH {} AS src (TYPE postgres, READ_ONLY); USE src;", literal(dsn)));
        }
        Some(Database::Mysql(_) | Database::Sqlserver(_)) => return Err(live_not_here()),
        None => {}
    }
    if confined {
        let allowed = format!("{}{}", dir.display(), std::path::MAIN_SEPARATOR);
        s.push_str(&format!(
            " SET allowed_directories = [{}]; SET enable_external_access = false; SET lock_configuration = true;",
            literal(&allowed)
        ));
    }
    Ok(s)
}

/// Whether `script` breaks a line only inside a quoted string or name. DuckDB reads a script given
/// on its standard input line by line, and a line that starts a statement with `.` is one of its
/// own commands (`.shell` among them): the user's SQL and every path and name are quoted
/// ([`literal`], [`ident`]), so none can start one. A doubled quote closes and reopens the quote.
/// What DuckDB would read as a quote or a comment that this doesn't follow (`--`, `/*`, `$$`, a
/// backtick, `E'…'`) is refused outside quotes too: the scripts RustGIS writes use none of them.
fn breaks_only_in_quotes(script: &str) -> bool {
    let mut quote = None;
    let mut before = ' ';
    for c in script.chars() {
        match (quote, c) {
            (None, '\'' | '"') if before.is_alphanumeric() || before == '_' => return false,
            (None, '\'' | '"') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, '\n' | '\r' | '$' | '`') => return false,
            (None, '-') if before == '-' => return false,
            (None, '*') if before == '/' => return false,
            _ => {}
        }
        before = if quote.is_some() { ' ' } else { c };
    }
    true
}

/// Runs a script.
fn run_script(dir: &Path, script: &str, database: Option<&Database>, timeout: std::time::Duration) -> Result<()> {
    run_script_with(&program().ok_or_else(missing)?, dir, script, database, timeout)
}

fn run_script_with(duckdb: &Path, dir: &Path, script: &str, database: Option<&Database>, timeout: std::time::Duration) -> Result<()> {
    if !breaks_only_in_quotes(script) {
        return Err(DataError::Format("DuckDB: the script breaks a line outside a quoted text".into()));
    }
    // An empty start-up file, so the user's own (~/.duckdbrc) can't change what runs.
    let init = dir.join("init.sql");
    if !init.is_file() {
        std::fs::write(&init, b"")?;
    }
    let mut c = Command::new(duckdb);
    if let Some(p) = password(database) {
        c.env("PGPASSWORD", p);
    }
    // The script goes to its standard input, never its arguments: the user's SQL can hold a signed
    // address, a secret's keys or a password, and any user of a Unix machine can list a program's
    // arguments while it runs. `-bail` stops at the first error, as `-c` does. Windows doesn't show
    // one user's arguments to another, and reads a pipe as text there (a CR LF in a quoted value
    // would become LF), so it keeps `-c`.
    #[cfg(not(windows))]
    let out = {
        c.arg("-init").arg(&init).arg("-bail");
        external::output_with_input(c, timeout, "DuckDB", script.as_bytes().to_vec())?
    };
    #[cfg(windows)]
    let out = {
        c.arg("-init").arg(&init).arg("-c").arg(script);
        external::output(c, timeout, "DuckDB")?
    };
    if !out.status.success() {
        return Err(DataError::Format(format!("DuckDB: {}", message(&String::from_utf8_lossy(&out.stderr), &out.status))));
    }
    Ok(())
}

/// Runs `script`, then `select`, whose rows DuckDB writes to a file that is read back: not taken
/// from what it prints, which warnings share.
fn fetch(dir: &Path, script: &str, database: Option<&Database>, select: &str, timeout: std::time::Duration) -> Result<serde_json::Value> {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let file = dir.join(format!("{:016x}.{}.{n}.rows.tmp.json", fnv1a(select.as_bytes()), std::process::id()));
    let _guard = TempFile(Some(file.clone()));
    let copy = format!("{script} COPY ({select}) TO {} (FORMAT json, ARRAY true);", literal(&file.display().to_string()));
    run_script(dir, &copy, database, timeout)?;
    serde_json::from_slice(&std::fs::read(&file)?).map_err(|e| DataError::Format(format!("DuckDB: unreadable result ({e})")))
}

/// What went wrong, from DuckDB's error output: its first lines, without colours, warnings or
/// the excerpt of the statement (which shows the query wrapped, not as it was written).
fn message(stderr: &str, status: &std::process::ExitStatus) -> String {
    let plain = strip_ansi(stderr);
    let lines: Vec<&str> = plain.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    // An error is reported after any warnings.
    let from = lines.iter().position(|l| l.contains("Error")).unwrap_or(0);
    let msg: Vec<&str> = lines.iter().skip(from).take_while(|l| !l.starts_with("LINE ")).take(3).copied().collect();
    if msg.is_empty() { format!("duckdb failed ({status})") } else { msg.join(" ") }
}

/// Text without terminal colour codes.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // ESC [ … letter
            for d in chars.by_ref() {
                if d.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// A column of a result.
#[derive(Clone, Debug, PartialEq)]
struct Column {
    name: String,
    ty: String,
}

/// Column types the GeoParquet provider reads as they are; others are written as text. Dates and
/// timestamps are written as Parquet's own (a `TIMESTAMPTZ` as a UTC instant); a `TIMESTAMP_NS`
/// too, which is read rounded to the microsecond and said.
fn kept_as_is(ty: &str) -> bool {
    matches!(
        ty,
        "BOOLEAN"
            | "TINYINT"
            | "SMALLINT"
            | "INTEGER"
            | "BIGINT"
            | "UTINYINT"
            | "USMALLINT"
            | "UINTEGER"
            | "FLOAT"
            | "DOUBLE"
            | "VARCHAR"
            | "DATE"
            | "TIMESTAMP"
            | "TIMESTAMP_MS"
            | "TIMESTAMP_NS"
            | "TIMESTAMP WITH TIME ZONE"
    ) || ty.starts_with("DECIMAL(")
}

fn is_geometry(ty: &str) -> bool {
    ty == "GEOMETRY" || ty.starts_with("GEOMETRY(")
}

/// The coordinate system a geometry type says (`GEOMETRY('EPSG:3857')`).
fn type_crs(ty: &str) -> Option<&str> {
    ty.strip_prefix("GEOMETRY('").and_then(|r| r.strip_suffix("')")).filter(|c| !c.is_empty())
}

/// The system a result's geometry is kept in rather than converted to longitude/latitude: the one
/// given or the one its type says, when RustGIS reads its EPSG code (as the GeoParquet DuckDB
/// writes says it, [`select`]). `None` for longitude/latitude or one it can't read.
fn own_system(columns: &[Column], crs: Option<&str>) -> Option<crate::raster::Crs> {
    let ty = columns.iter().find(|c| is_geometry(&c.ty)).and_then(|c| type_crs(&c.ty));
    let code = crs.or(ty)?;
    let (authority, number) = code.split_once(':')?;
    let crs = crate::raster::Crs::from_epsg(number.trim().parse().ok()?)
        .filter(|_| authority.trim().eq_ignore_ascii_case("EPSG"))
        .filter(|c| *c != crate::raster::Crs::LonLat)?;
    crs.transformer().is_ok().then_some(crs)
}

/// The SELECT that turns a result into what is written: the first geometry column in
/// longitude/latitude, or kept in its own system when `own` ([`own_system`]), x and y only, other
/// geometry columns as WKT, types the provider doesn't read as text, each row's geometry family and
/// whether its geometry had Z or M values; and the geometry column's name.
fn select(columns: &[Column], crs: Option<&str>, from: &str, own: bool) -> Result<(String, String)> {
    let geom = columns
        .iter()
        .find(|c| is_geometry(&c.ty))
        .ok_or_else(|| DataError::Format("the query returns no geometry column: select one (such as ST_Point(lon, lat) AS geom)".into()))?;
    let g = ident(&geom.name);
    // A geometry that says no coordinate system (the usual case for longitude/latitude) is taken
    // as it is. One kept in its own is given it, so the GeoParquet says it.
    let geom_expr = match (crs, type_crs(&geom.ty)) {
        (Some(c), _) if own => format!("ST_SetCRS({g}::GEOMETRY, {})", literal(c)),
        (None, Some(_)) if own => g.clone(),
        (Some(c), _) => format!("ST_Transform({g}::GEOMETRY, {}, 'EPSG:4326')", literal(c)),
        (None, Some(t)) if t.eq_ignore_ascii_case("EPSG:4326") || t.eq_ignore_ascii_case("OGC:CRS84") => g.clone(),
        (None, Some(_)) => format!("ST_Transform({g}, 'EPSG:4326')"),
        (None, None) => g.clone(),
    };
    let mut list: Vec<String> = columns
        .iter()
        .map(|c| {
            let n = ident(&c.name);
            if std::ptr::eq(c, geom) {
                format!("ST_Force2D({geom_expr}) AS {n}")
            } else if is_geometry(&c.ty) {
                format!("ST_AsText({n}) AS {n}")
            } else if kept_as_is(&c.ty) {
                n
            } else if c.ty == "TIMESTAMP_S" {
                // Parquet has no seconds: an infinite one can't be written as one, but can as microseconds.
                format!("CAST({n} AS TIMESTAMP) AS {n}")
            } else {
                format!("CAST({n} AS VARCHAR) AS {n}")
            }
        })
        .collect();
    list.push(format!("(ST_HasZ({g}::GEOMETRY) OR ST_HasM({g}::GEOMETRY)) AS {ZM}"));
    Ok((format!("SELECT *, ST_Dimension({g}) AS {DIM} FROM (SELECT {} FROM {from})", list.join(", ")), geom.name.clone()))
}

/// Runs a query, writing its result into the cache (always afresh: the data it reads may have
/// changed).
pub fn run(q: &Query<'_>) -> Result<Output> {
    let dir = cache_dir().ok_or_else(|| DataError::Format("no cache folder for DuckDB results".into()))?;
    run_in(&dir, q, false)
}

/// The most rows a preview returns.
pub const MAX_PREVIEW: usize = 1000;
/// The longest value a preview shows (text and geometry as WKT are cut there).
const PREVIEW_TEXT: usize = 300;

/// A query's first rows, as text: what an SQL workspace shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Preview {
    /// Each column's name and DuckDB type.
    pub columns: Vec<(String, String)>,
    /// The rows, each value as JSON (numbers, booleans and text; null; geometries as WKT).
    pub rows: Vec<Vec<serde_json::Value>>,
    /// Whether the result has a geometry column (it can be added to the map).
    pub geometry: bool,
    /// Whether there are rows past those returned.
    pub more: bool,
}

/// Checks a query before it runs: one statement, not empty, not too long.
fn checked(sql: &str) -> Result<&str> {
    let sql = sql.trim().trim_end_matches(';').trim_end();
    if sql.is_empty() {
        return Err(DataError::Format("the query is empty".into()));
    }
    if sql.len() > MAX_SQL {
        return Err(DataError::Format(format!("the query is longer than {MAX_SQL} characters")));
    }
    if sql.contains('\0') {
        return Err(DataError::Format("the query holds a NUL character".into()));
    }
    Ok(sql)
}

/// Runs a query and returns its first `limit` rows (at most [`MAX_PREVIEW`]), without writing a
/// result: an SQL workspace's preview. Geometries come back as WKT, other values DuckDB doesn't
/// give as JSON scalars as text, and long values cut.
pub fn preview(q: &Query<'_>, limit: usize) -> Result<Preview> {
    let dir = cache_dir().ok_or_else(|| DataError::Format("no cache folder for DuckDB results".into()))?;
    preview_in(&dir, q, limit)
}

fn preview_in(dir: &Path, q: &Query<'_>, limit: usize) -> Result<Preview> {
    let sql = checked(q.sql)?;
    if !available() {
        return Err(missing());
    }
    std::fs::create_dir_all(dir)?;
    let limit = limit.clamp(1, MAX_PREVIEW);
    let prelude = prelude(dir, q.database, false)?;
    let from = format!("query({})", literal(sql));
    let described =
        fetch(dir, &prelude, q.database, &format!("SELECT column_name, column_type FROM (DESCRIBE FROM {from})"), QUERY_TIMEOUT)?;
    let columns: Vec<Column> = described
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| Some(Column { name: c.get("column_name")?.as_str()?.to_string(), ty: c.get("column_type")?.as_str()?.to_string() }))
        .take(500)
        .collect();
    if columns.is_empty() {
        return Err(DataError::Format("the query returns no columns".into()));
    }
    // Columns by position (`c0`, `c1`…), so names that repeat stay apart.
    let list: Vec<String> = columns
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let n = ident(&c.name);
            let expr = if is_geometry(&c.ty) {
                format!("left(ST_AsText({n}), {PREVIEW_TEXT})")
            } else if matches!(c.ty.as_str(), "FLOAT" | "DOUBLE") {
                // NaN and infinities aren't JSON numbers: shown empty.
                format!("CASE WHEN isfinite({n}) THEN {n} END")
            } else if matches!(
                c.ty.as_str(),
                "BOOLEAN" | "TINYINT" | "SMALLINT" | "INTEGER" | "BIGINT" | "UTINYINT" | "USMALLINT" | "UINTEGER"
            ) {
                n
            } else {
                format!("left(CAST({n} AS VARCHAR), {PREVIEW_TEXT})")
            };
            format!("{expr} AS c{i}")
        })
        .collect();
    // The columns come from DESCRIBE, run against the query itself.
    let select = format!("SELECT {} FROM {from} LIMIT {}", list.join(", "), limit + 1);
    let fetched = fetch(dir, &prelude, q.database, &select, QUERY_TIMEOUT)?;
    let mut rows: Vec<Vec<serde_json::Value>> = fetched
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| (0..columns.len()).map(|i| r.get(format!("c{i}")).cloned().unwrap_or(serde_json::Value::Null)).collect())
        .collect();
    let more = rows.len() > limit;
    rows.truncate(limit);
    Ok(Preview {
        geometry: columns.iter().any(|c| is_geometry(&c.ty)),
        columns: columns.into_iter().map(|c| (c.name, c.ty)).collect(),
        rows,
        more,
    })
}

/// The tables of a database and their columns (`table` or `schema.table`, column), for completing
/// names in an SQL workspace. Read confined, as [`tables_of`] is.
pub fn schema_of(database: &Database) -> Result<Vec<(String, String)>> {
    let dir = cache_dir().ok_or_else(|| DataError::Format("no cache folder for DuckDB results".into()))?;
    if !available() {
        return Err(missing());
    }
    std::fs::create_dir_all(&dir)?;
    let select = format!(
        "SELECT schema_name, table_name, column_name FROM duckdb_columns() WHERE database_name = 'src' ORDER BY schema_name, table_name, column_index LIMIT {LIST_LIMIT}"
    );
    let rows = fetch(&dir, &prelude(&dir, Some(database), true)?, Some(database), &select, LIST_TIMEOUT)?;
    Ok(rows
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| {
            let t = Table { schema: r.get("schema_name")?.as_str()?.to_string(), name: r.get("table_name")?.as_str()?.to_string() };
            Some((t.label(), r.get("column_name")?.as_str()?.to_string()))
        })
        .collect())
}

/// Runs a query; `confined` (for a table of a database) as [`prelude`] says. The result's parts are
/// named after this run alone, so a run never replaces a file a layer may be reading.
fn run_in(dir: &Path, q: &Query<'_>, confined: bool) -> Result<Output> {
    run_selected_in(dir, q, confined, None)
}

/// A manual table replacement keeps only its original family and permits a zero-row geometry
/// column. Ordinary adds retain their existing nonempty-result behavior.
fn run_selected_in(dir: &Path, q: &Query<'_>, confined: bool, selected: Option<Family>) -> Result<Output> {
    run_kept_in(dir, q, confined, selected, true)
}

/// As [`run_selected_in`]; with `keep`, a result in a system RustGIS reads is kept in it, and run
/// again in longitude/latitude, as before, when its file doesn't read back in it or the run fails
/// on a DuckDB too old to give a geometry its system.
fn run_kept_in(dir: &Path, q: &Query<'_>, confined: bool, selected: Option<Family>, keep: bool) -> Result<Output> {
    let sql = checked(q.sql)?;
    check_crs(q.crs)?;
    if !available() {
        return Err(missing());
    }
    std::fs::create_dir_all(dir)?;
    if selected.is_none() {
        external::prune(dir, ours, KEEP);
    }
    let prelude = prelude(dir, q.database, confined)?;
    let from = format!("query({})", literal(sql));

    // The result's columns, from DuckDB binding the query (not running it).
    let described =
        fetch(dir, &prelude, q.database, &format!("SELECT column_name, column_type FROM (DESCRIBE FROM {from})"), QUERY_TIMEOUT)?;
    let columns: Vec<Column> = described
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| Some(Column { name: c.get("column_name")?.as_str()?.to_string(), ty: c.get("column_type")?.as_str()?.to_string() }))
        .collect();
    if let Some(c) = columns.iter().find(|c| c.name.eq_ignore_ascii_case(DIM) || c.name.eq_ignore_ascii_case(ZM)) {
        return Err(DataError::Format(format!("the query returns a column named {}", c.name)));
    }
    let own = if keep { own_system(&columns, q.crs) } else { None };
    let (select, geom) = select(&columns, q.crs, &from, own.is_some())?;
    let g = ident(&geom);
    // The bounding-box covering, unless the result already has a column of its name.
    let bbox_name = format!("{geom}_bbox");
    let bbox = if columns.iter().any(|c| c.name.eq_ignore_ascii_case(&bbox_name)) {
        String::new()
    } else {
        format!(
            ", struct_pack(xmin := ST_XMin({g}), ymin := ST_YMin({g}), xmax := ST_XMax({g}), ymax := ST_YMax({g})) AS {}",
            ident(&bbox_name)
        )
    };

    // Written under a name of its own, so a failed run leaves nothing behind and runs don't meet.
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let run_tag = format!("{:016x}", fnv1a(format!("{sql}\n{:?}\n{:?}\n{now}\n{}\n{n}", q.database, q.crs, std::process::id()).as_bytes()));
    let tmp = |f: Family| dir.join(format!("{run_tag}.{}.{n}.{}.tmp.parquet", std::process::id(), f.label()));
    let mut guards: Vec<TempFile> = Family::ALL.iter().map(|f| TempFile(Some(tmp(*f)))).collect();
    let mut script = format!("{prelude} CREATE TEMP TABLE r AS {select};");
    // Rows in order along a curve over the world, or over the result's own extent in its system.
    let hilbert = if own.is_some() {
        format!("(SELECT ST_Extent(ST_Extent_Agg({g})) FROM r)")
    } else {
        "{'min_x': -180, 'min_y': -90, 'max_x': 180, 'max_y': 90}::BOX_2D".to_string()
    };
    for f in Family::ALL {
        script.push_str(&format!(
            " COPY (SELECT * EXCLUDE ({DIM}, {ZM}){bbox} FROM r WHERE {DIM} = {} ORDER BY ST_Hilbert({g}, {hilbert})) TO {} (FORMAT parquet, ROW_GROUP_SIZE 16384);",
            dimension(f),
            literal(&tmp(f).display().to_string())
        ));
    }
    let counts = match fetch(
        dir,
        &script,
        q.database,
        &format!("SELECT {DIM} AS dim, count(*)::BIGINT AS n, count_if({ZM})::BIGINT AS zm FROM r GROUP BY {DIM}"),
        QUERY_TIMEOUT,
    ) {
        Ok(counts) => counts,
        // A DuckDB without `ST_SetCRS` (its error names the function): in longitude/latitude, as
        // before. Any other failure is the query's own.
        Err(e) if own.is_some() && q.crs.is_some() && e.to_string().to_ascii_lowercase().contains("st_setcrs") => {
            drop(guards);
            crate::progress::check()?;
            return run_kept_in(dir, q, confined, selected, false);
        }
        Err(e) => return Err(e),
    };

    let mut out = Output::default();
    for row in counts.as_array().into_iter().flatten() {
        let count = row.get("n").and_then(|v| v.as_u64()).unwrap_or(0);
        let zm_dropped = row.get("zm").and_then(|v| v.as_u64()).unwrap_or(0);
        match row.get("dim").and_then(|v| v.as_i64()).and_then(|d| Family::ALL.into_iter().find(|f| dimension(*f) == d)) {
            Some(family) if count > 0 => {
                let path = dir.join(format!("{run_tag}.{}.parquet", family.label()));
                out.parts.push(Part { path, family, count, zm_dropped });
            }
            Some(_) => {}
            None => out.skipped = out.skipped.saturating_add(count),
        }
    }
    if let Some(family) = selected {
        out.parts.retain(|p| {
            if p.family == family {
                true
            } else {
                out.other_family = out.other_family.saturating_add(p.count);
                false
            }
        });
        if out.parts.is_empty() && (out.other_family > 0 || out.skipped == 0) {
            // COPY wrote a valid empty GeoParquet for every family, even when the selected
            // family disappeared while rows of another family remain.
            out.parts.push(Part { path: dir.join(format!("{run_tag}.{}.parquet", family.label())), family, count: 0, zm_dropped: 0 });
        }
    }
    if out.parts.is_empty() {
        return Err(DataError::Format(if out.skipped > 0 {
            format!("none of the query's {} rows has a geometry", out.skipped)
        } else {
            NO_ROWS.into()
        }));
    }
    // Kept in its own system only when each part reads back in it: else in longitude/latitude.
    if let Some(crs) = own {
        let read_back = |p: &Part| {
            crate::provenance::without(|| super::parquet::ParquetSource::open_path(&tmp(p.family), "check"))
                .is_ok_and(|s| super::FeatureSource::coord_crs(&s) == crs)
        };
        if !out.parts.iter().filter(|p| p.count > 0).all(read_back) {
            drop(guards);
            crate::progress::check()?;
            return run_kept_in(dir, q, confined, selected, false);
        }
    }
    // The parts take their final names; the families not in the result are removed with their
    // guards.
    for p in &out.parts {
        let from = tmp(p.family);
        std::fs::rename(&from, &p.path)?;
        if let Some(g) = guards.iter_mut().find(|g| g.0.as_deref() == Some(from.as_path())) {
            g.0 = None;
        }
    }
    Ok(out)
}

/// The `ST_Dimension` of a geometry family.
fn dimension(f: Family) -> i64 {
    match f {
        Family::Points => 0,
        Family::Lines => 1,
        Family::Polygons => 2,
    }
}

/// The tables (and views) of a database that have a geometry column. Read confined: a view
/// reaching outside the database fails when it is converted.
pub fn tables(database: &Path) -> Result<Vec<Table>> {
    tables_of(&Database::File(database.to_path_buf()))
}

/// The tables (and views) of a database, file or connection, that have a geometry column.
pub fn tables_of(database: &Database) -> Result<Vec<Table>> {
    let dir = cache_dir().ok_or_else(|| DataError::Format("no cache folder for DuckDB results".into()))?;
    tables_in(&dir, database)
}

fn tables_in(dir: &Path, database: &Database) -> Result<Vec<Table>> {
    if !available() {
        return Err(missing());
    }
    std::fs::create_dir_all(dir)?;
    let select = format!(
        "SELECT DISTINCT schema_name, table_name FROM duckdb_columns() WHERE database_name = 'src' AND starts_with(data_type, 'GEOMETRY') ORDER BY ALL LIMIT {LIST_LIMIT}"
    );
    let rows = fetch(dir, &prelude(dir, Some(database), true)?, Some(database), &select, LIST_TIMEOUT)?;
    Ok(rows
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| {
            Some(Table { schema: r.get("schema_name")?.as_str()?.to_string(), name: r.get("table_name")?.as_str()?.to_string() })
        })
        .collect())
}

/// The table of a database named `name` (`table` or `schema.table`, any case).
pub fn find_table(database: &Path, name: &str) -> Result<Table> {
    find_table_of(&Database::File(database.to_path_buf()), name)
}

/// The table of a database, file or connection, named `name`.
pub fn find_table_of(database: &Database, name: &str) -> Result<Table> {
    let all = tables_of(database)?;
    all.into_iter()
        .find(|t| t.label().eq_ignore_ascii_case(name) || t.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| DataError::Format(format!("no table '{name}' with a geometry column")))
}

/// Converts a table of a database, reusing an earlier conversion while the file is unchanged.
pub fn convert_table(database: &Path, table: &Table, crs: Option<&str>) -> Result<Output> {
    convert_table_of(&Database::File(database.to_path_buf()), table, crs)
}

/// Converts a table of a database: a file's as [`convert_table`] does; a connection's afresh each
/// time (its data may change at any time).
pub fn convert_table_of(database: &Database, table: &Table, crs: Option<&str>) -> Result<Output> {
    let dir = cache_dir().ok_or_else(|| DataError::Format("no cache folder for DuckDB results".into()))?;
    match database {
        Database::File(p) => convert_table_in(&dir, p, table, crs),
        Database::Mysql(_) | Database::Sqlserver(_) => Err(live_not_here()),
        Database::Postgres(_) => {
            if !available() {
                return Err(missing());
            }
            std::fs::create_dir_all(&dir)?;
            run_in(&dir, &Query { sql: &table.sql(), database: Some(database), crs }, true)
        }
    }
}

/// A manual replacement's output and private files, retained by its concrete reader.
#[derive(Debug)]
pub struct FreshOutput {
    pub output: Output,
    pub workspace: std::sync::Arc<ReloadWorkspace>,
}

/// Re-reads only a selected table/family, bypassing file-signature caches. The result has its own
/// path, so retained handles and snapshots continue to read their previous conversion.
pub fn convert_table_fresh(database: &Path, table: &Table, family: Family, crs: Option<&str>) -> Result<FreshOutput> {
    let dir = cache_dir().ok_or_else(|| DataError::Format("no cache folder for DuckDB results".into()))?;
    convert_table_fresh_in(&dir, database, table, family, crs)
}

fn convert_table_fresh_in(dir: &Path, database: &Path, table: &Table, family: Family, crs: Option<&str>) -> Result<FreshOutput> {
    let workspace = external::reload_workspace(dir)?;
    let output = run_selected_in(
        workspace.path(),
        &Query { sql: &table.sql(), database: Some(&Database::File(database.to_path_buf())), crs },
        true,
        Some(family),
    )?;
    Ok(FreshOutput { output, workspace })
}

fn convert_table_in(dir: &Path, database: &Path, table: &Table, crs: Option<&str>) -> Result<Output> {
    let full = std::path::absolute(database).unwrap_or_else(|_| database.to_path_buf());
    let (len, modified) = stamp(&full)?;
    crate::provenance::duckdb(&full);
    // Changes not yet checkpointed are in the write-ahead log beside the file.
    let mut wal = full.clone().into_os_string();
    wal.push(".wal");
    let (wal_len, wal_modified) = stamp(Path::new(&wal)).unwrap_or((0, 0));
    // The cache key, spelled out: kept beside the conversion and checked, so a hash collision
    // can't hand back another table.
    let key = format!(
        "{}\n{}\n{}\n{crs:?}\n{len}\n{modified}\n{wal_len}\n{wal_modified}\nconversion {CONVERSION}",
        full.display(),
        table.schema,
        table.name
    );
    let tag = format!("{:016x}", fnv1a(key.as_bytes()));
    let keyfile = dir.join(format!("{tag}.key"));
    if let Some(hit) = std::fs::read_to_string(&keyfile).ok().and_then(|s| cached(dir, &key, &s)) {
        touch(&keyfile);
        for p in &hit.parts {
            touch(&p.path);
        }
        return Ok(hit);
    }
    let _ = std::fs::remove_file(&keyfile);
    // A database is hostile input: its tables are read confined to the cache.
    let out = run_in(dir, &Query { sql: &table.sql(), database: Some(&Database::File(full.clone())), crs }, true)?;
    // The key goes last, vouching for the parts written before it: the key, then a line per part
    // (its family, rows, file and rows that lost Z or M values) and the rows left out.
    let mut text = key.clone();
    for p in &out.parts {
        let file = p.path.file_name().and_then(|f| f.to_str()).unwrap_or_default();
        text.push_str(&format!("\npart {} {} {file} {}", p.family.label(), p.count, p.zm_dropped));
    }
    text.push_str(&format!("\nskipped {}", out.skipped));
    std::fs::write(&keyfile, text)?;
    Ok(out)
}

/// A conversion in the cache, when its key file matches `key` and its parts are all there.
fn cached(dir: &Path, key: &str, stored: &str) -> Option<Output> {
    let rest = stored.strip_prefix(key)?.strip_prefix('\n')?;
    let mut out = Output::default();
    for line in rest.lines() {
        let mut w = line.split(' ');
        match (w.next(), w.next(), w.next(), w.next(), w.next(), w.next()) {
            // Only a file of this module's, in the cache folder.
            (Some("part"), Some(f), Some(n), Some(file), Some(zm), None) if ours(file) && file.ends_with(&format!(".{f}.parquet")) => {
                let family = Family::ALL.into_iter().find(|x| x.label() == f)?;
                let path = dir.join(file);
                if !path.is_file() {
                    return None;
                }
                out.parts.push(Part { path, family, count: n.parse().ok()?, zm_dropped: zm.parse().ok()? });
            }
            (Some("skipped"), Some(n), None, None, None, None) => out.skipped = n.parse().ok()?,
            _ => return None,
        }
    }
    (!out.parts.is_empty()).then_some(out)
}

/// Whether a file name is one this module writes: `<16 hex>.<family>.parquet`, `<16 hex>.key`, or
/// a `<16 hex>.<pid>.<n>.<family>.tmp.parquet` or `<16 hex>.<pid>.<n>.rows.tmp.json` left by a
/// run that didn't finish.
fn ours(name: &str) -> bool {
    let Some((tag, rest)) = name.split_once('.') else { return false };
    if tag.len() != 16 || !tag.bytes().all(|b| b.is_ascii_hexdigit()) {
        return false;
    }
    let family = |f: &str| Family::ALL.iter().any(|x| x.label() == f);
    if rest == "key" || rest.strip_suffix(".parquet").is_some_and(family) {
        return true;
    }
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let parts = |suffix: &str| rest.strip_suffix(suffix).map(|r| r.split('.').map(str::to_string).collect::<Vec<_>>());
    match (parts(".tmp.parquet").as_deref(), parts(".tmp.json").as_deref()) {
        (Some([pid, n, f]), _) => digits(pid) && digits(n) && family(f),
        (_, Some([pid, n, c])) => digits(pid) && digits(n) && c == "rows",
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in `duckdb` that writes its arguments and its standard input to files beside it.
    #[cfg(unix)]
    fn recording_duckdb(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let exe = dir.join("duckdb");
        std::fs::write(&exe, "#!/bin/sh\nhere=$(dirname \"$0\")\nprintf '%s\\n' \"$@\" > \"$here/args.txt\"\ncat > \"$here/input.txt\"\n")
            .unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
        exe
    }

    /// Runs `script` with a stand-in just written, which can be "busy" a moment while another test
    /// starts a program.
    #[cfg(unix)]
    fn run_with_stand_in(exe: &Path, dir: &Path, script: &str) -> Result<()> {
        let mut ran = run_script_with(exe, dir, script, None, std::time::Duration::from_secs(30));
        for _ in 0..100 {
            if !matches!(&ran, Err(DataError::Io(e)) if e.kind() == std::io::ErrorKind::ExecutableFileBusy) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10)); // test-wait: a just-written stand-in program is briefly busy
            ran = run_script_with(exe, dir, script, None, std::time::Duration::from_secs(30));
        }
        ran
    }

    /// A query's text, credentials and all, reaches DuckDB on its standard input, never in its
    /// arguments, which every user of the machine can list while it runs.
    #[cfg(unix)]
    #[test]
    fn a_querys_credentials_stay_out_of_duckdbs_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let exe = recording_duckdb(dir.path());
        let secret = "dummy";
        let sql = format!("SELECT * FROM read_parquet('https://bucket.example/data.parquet?{secret}')\nWHERE 'line;\n.shell no' <> ''");
        let script =
            format!("{} CREATE TEMP TABLE r AS SELECT * FROM query({});", prelude(dir.path(), None, false).unwrap(), literal(&sql));
        run_with_stand_in(&exe, dir.path(), &script).unwrap();
        let args = std::fs::read_to_string(dir.path().join("args.txt")).unwrap();
        let input = std::fs::read_to_string(dir.path().join("input.txt")).unwrap();
        // A path may contain the dummy value too. Only these arguments are allowed; all SQL,
        // including its test credential, must arrive on standard input instead.
        assert_eq!(args.lines().collect::<Vec<_>>(), ["-init", dir.path().join("init.sql").to_str().unwrap(), "-bail"]);
        assert_eq!(input, script, "the whole script, as it was");
    }

    /// A script longer than Linux takes in one argument (128 KiB) reaches DuckDB.
    #[cfg(unix)]
    #[test]
    fn a_script_longer_than_an_argument_can_be_reaches_duckdb() {
        let dir = tempfile::tempdir().unwrap();
        let exe = recording_duckdb(dir.path());
        let script = format!("SELECT {};", literal(&"x".repeat(1 << 20)));
        run_with_stand_in(&exe, dir.path(), &script).unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("input.txt")).unwrap().len(), script.len());
    }

    /// Line breaks only inside quotes: outside, a line starting with `.` would be one of DuckDB's
    /// own commands.
    #[test]
    fn a_script_breaks_lines_only_inside_quotes() {
        assert!(breaks_only_in_quotes("SELECT * FROM query('SELECT 1;\n.shell x\n');"));
        assert!(breaks_only_in_quotes("SELECT \"a\nb\" FROM t WHERE c = 'it''s\n';"));
        assert!(!breaks_only_in_quotes("SELECT 1;\n.shell x"));
        assert!(!breaks_only_in_quotes("SELECT 'a''' ;\r.shell x"));
        // What DuckDB reads as a comment or a quote of another kind, outside quotes.
        for other in ["SELECT 1 -- '", "SELECT 1 /* ' */", "SELECT $$a'$$", "SELECT `'`", "SELECT E'\\''", "SELECT x'1'"] {
            assert!(!breaks_only_in_quotes(other), "{other}");
        }
        assert!(breaks_only_in_quotes(
            "SELECT 'a--b /* $ ` E''' AS \"x--\" FROM \"s\".\"t\" WHERE n = -1 AND m = 2 - -3 AND d IN ['a', ('b')];"
        ));
        let dir = tempfile::tempdir().unwrap();
        let e =
            run_script_with(Path::new("/nonexistent/duckdb"), dir.path(), "SELECT 1;\n.shell x", None, std::time::Duration::from_secs(1))
                .unwrap_err();
        assert!(e.to_string().contains("outside a quoted text"), "{e}");
    }

    #[test]
    fn where_programs_cant_run_the_message_says_so_instead_of_asking_for_an_install() {
        assert!(missing_text("").contains("install it"));
        let phone = missing_text("a phone");
        assert!(phone.contains("a phone can't run") && !phone.contains("install"), "{phone}");
    }

    /// Each kind reads only its own client's variable, and an empty one is none (a fake
    /// environment: the process's is never changed).
    #[test]
    fn ambient_password_follows_each_clients_variable() {
        let env = |name: &str| match name {
            "PGPASSWORD" => Some("pg-env".to_string()),
            "MYSQL_PWD" => Some("my-env".to_string()),
            "SQLCMDPASSWORD" => Some(String::new()),
            _ => None,
        };
        assert_eq!(ambient_password_in("postgres", env).as_deref(), Some("pg-env"));
        assert_eq!(ambient_password_in("mysql", env).as_deref(), Some("my-env"));
        assert_eq!(ambient_password_in("sqlserver", env), None);
        assert_eq!(ambient_password_in("s3", env), None);
    }

    #[test]
    fn refused_password_does_not_restore_over_a_newer_login_or_forget() {
        let dsn = "host=refused_password_newer_slot";
        remember_password(dsn, "original");
        try_connection_password(
            dsn,
            "refused-attempt",
            "bad",
            || {
                remember_password(dsn, "newer");
            },
            |_| true,
        );
        assert_eq!(remembered_password(dsn).as_deref(), Some("newer"));
        try_connection_password(
            dsn,
            "refused-attempt",
            "bad",
            || {
                forget_connection_password("refused-attempt");
            },
            |_| true,
        );
        assert!(!has_password(dsn));
    }

    #[test]
    fn remembered_database_cache_ownership_and_explicit_preservation() {
        let a = "host=remembered_cache_a user=one";
        let b = "mysql://remembered_cache_b/db";
        set_connection_password(a, "cache-c1", Some("one"));
        assert!(!has_explicit_password(a));
        assert_eq!(remembered_password(a).as_deref(), Some("one"));
        set_connection_password(a, "cache-c2", Some("two"));
        forget_connection_password("cache-c1");
        assert_eq!(remembered_password(a).as_deref(), Some("two"));
        set_connection_password(a, "cache-c1", None);
        assert!(!has_password(a));
        remember_password(a, "explicit");
        set_connection_password(a, "cache-c2", None);
        assert!(has_explicit_password(a));
        assert_eq!(remembered_password(a).as_deref(), Some("explicit"));
        set_connection_password(b, "cache-c2", Some("owned"));
        forget_connection_passwords();
        assert!(!has_password(b));
        assert_eq!(remembered_password(a).as_deref(), Some("explicit"));
        assert_eq!(password(Some(&Database::Postgres(a.into()))).as_deref(), Some("explicit"));
        lock().remove(a);
    }

    #[test]
    fn databases_by_name() {
        // Quoted passwords with spaces, spaced `=`, escapes; an `@` past a URI's authority.
        assert_eq!(
            split_password("host=h  password = 'a b\\'c' application_name='my app' dbname=g"),
            ("host=h application_name='my app' dbname=g".to_string(), Some("a b'c".to_string()))
        );
        assert_eq!(split_password("host=h password='unterminated"), ("host=h".to_string(), None));
        assert_eq!(
            split_password("postgresql://db.example:5432/gis?application_name=x@y"),
            ("postgresql://db.example:5432/gis?application_name=x@y".to_string(), None)
        );
        assert_eq!(
            split_password("postgresql://me:p@ss@db.example/gis?x=1"),
            ("postgresql://me@db.example/gis?x=1".to_string(), Some("p@ss".to_string()))
        );
        assert_eq!(Database::parse(" places.duckdb "), Database::File("places.duckdb".into()));
        // A password is taken out of the connection string, remembered and never in its label.
        let d = Database::parse("postgres:host=db.example dbname=gis user=me password=pw1");
        assert_eq!(d, Database::Postgres("host=db.example dbname=gis user=me".into()));
        assert_eq!(d.label(), "postgres:host=db.example dbname=gis user=me");
        assert_eq!(d.short(), "gis");
        // Quoted values (as `pg.connect` writes them) are named without their quotes, whole.
        assert_eq!(Database::Postgres("host='h' dbname='my gis' user='o\\'neil'".into()).short(), "my gis");
        assert_eq!(Database::Postgres("host='h' dbname='gis'".into()).short(), "gis");
        assert_eq!(password(Some(&d)).as_deref(), Some("pw1"));
        assert_eq!(Database::parse(&d.label()), d);
        let u = Database::parse("postgresql://me:pw2@db.example:5433/osm?sslmode=require");
        assert_eq!(u, Database::Postgres("postgresql://me@db.example:5433/osm?sslmode=require".into()));
        assert_eq!((u.label().as_str(), u.short().as_str()), ("postgresql://me@db.example:5433/osm?sslmode=require", "osm"));
        assert_eq!(password(Some(&u)).as_deref(), Some("pw2"));
        assert_eq!(Database::parse("POSTGRES:dbname=x"), Database::Postgres("dbname=x".into()));
        assert_eq!(password(Some(&Database::parse("postgres:dbname=other"))), None);
        // A connection is attached read only, its password never in the script.
        let dir = std::env::temp_dir();
        let p = prelude(&dir, Some(&d), true).unwrap();
        assert!(p.contains("ATTACH 'host=db.example dbname=gis user=me' AS src (TYPE postgres, READ_ONLY)") && !p.contains("pw1"), "{p}");
        let p = prelude(&dir, Some(&u), true).unwrap();
        assert!(p.contains("ATTACH 'postgresql://me@db.example:5433/osm?sslmode=require' AS src (TYPE postgres, READ_ONLY)"), "{p}");

        // libpq local-file and option parameters are refused before DuckDB `ATTACH` runs.
        for bad in [
            "postgres:host=db.example dbname=gis passfile=/tmp/pgpass",
            "postgres:host=db.example dbname=gis sslrootcert=/tmp/root.crt",
            "postgres:host=db.example dbname=gis sslcert=/tmp/c.pem",
            "postgres:host=db.example dbname=gis sslkey=/tmp/k.pem",
            "postgres:host=db.example dbname=gis sslkeylogfile=/tmp/keys.log",
            "postgres:host=db.example dbname=gis sslcrl=/tmp/r.pem",
            "postgres:host=db.example dbname=gis service=prod",
            "postgres:host=db.example dbname=gis servicefile=/tmp/pg_service.conf",
            "postgres:host=db.example dbname=gis options='-c log_statement=all'",
            "postgresql://me@db.example/gis?passfile=/tmp/pgpass",
            "postgresql://me@db.example/gis?sslkeylogfile=/tmp/keys.log",
            "postgresql://me@db.example/gis?SSLROOTCERT=/tmp/root.crt",
            "postgresql://me@db.example/gis?%70assfile=/tmp/pgpass",
            "postgresql://me@db.example/gis?sslmode=require;servicefile=/tmp/pg_service.conf",
            "postgres:host=db.example dbname='postgresql://me@db.example/gis?passfile=/tmp/pgpass'",
        ] {
            let db = Database::parse(bad);
            assert!(prelude(&dir, Some(&db), true).is_err(), "{bad}");
        }
        for bad in [
            "postgresql://me@db.example/gis?application_name=x#&sslrootcert=/tmp/root.crt",
            "postgresql://u?x@db.example/gis?sslrootcert=/tmp/root.crt",
            "postgresql://db.example?options=-c%20foo@bar?sslmode=require",
            "postgresql://me@db.example/gis?sslkeylogfile=/tmp/keys.log",
            "postgresql://[fe80::1?x]/gis?sslrootcert=/tmp/root.crt",
            "postgresql://[::1]?x@db.example/gis?sslrootcert=/tmp/root.crt",
            "postgresql://[::1]?x=1@db.example/gis?sslkeylogfile=/tmp/keys.log",
            "postgresql://[u]?x@db.example/gis?sslrootcert=/tmp/root.crt",
            "postgresql://u1@u2?x@db.example/gis?sslrootcert=/tmp/root.crt",
            "postgresql://u1@u2?x@db.example?sslrootcert=/tmp/root.crt",
            "postgresql://u@[fe80::1?x]/gis?sslrootcert=/tmp/root.crt",
            "postgresql://h1,[fe80::1?x]/gis?sslrootcert=/tmp/root.crt",
            "postgresql://[::1]:5432/gis?sslrootcert=/tmp/root.crt",
            "postgresql://[::1/gis",
            "postgresql://u@[::1/gis",
            "host=db.example dbname=gis sslkeylogfile=/tmp/keys.log",
            "host=db.example bare passfile=/tmp/pgpass",
            "host='unterminated",
        ] {
            assert!(prelude(&dir, Some(&Database::Postgres(bad.into())), true).is_err(), "{bad}");
        }
        for good in [
            "postgresql://u?x@db.example/gis?sslmode=require",
            "postgresql://u@db.example?application_name=alice@team",
            "postgresql://[::1]:5432/gis?sslmode=require",
            "postgresql://[::1]/gis",
            "postgresql://user@[fe80::1%25eth0]:5432/gis?sslmode=require",
            "postgresql://[::1]:5432,[fe80::1%25eth0]:5433/gis?sslmode=require",
        ] {
            assert!(prelude(&dir, Some(&Database::Postgres(good.into())), true).is_ok(), "{good}");
        }
    }

    #[test]
    fn sqlserver_names_keep_no_password() {
        let db = Database::parse("mssql://ana:p%40w@h/gis?password=other&pwd=query-secret&access_token=token-secret&encrypt=strict");
        assert_eq!(db, Database::Sqlserver("mssql://ana@h/gis?encrypt=strict".into()));
        assert_eq!(db.short(), "gis");
        assert_eq!(remembered_password(&db.label()).as_deref(), Some("p@w"));
        assert_eq!(password(Some(&db)), None);
        assert!(prelude(&std::env::temp_dir(), Some(&db), true).is_err());
        assert_eq!(Database::parse("mssql://h").short(), "sqlserver");
    }

    #[test]
    fn names_and_quoting() {
        assert!(is_duckdb_name("a.duckdb") && is_duckdb_name("B.DDB") && !is_duckdb_name("a.db") && !is_duckdb_name("duckdb"));
        assert_eq!(literal("it's"), "'it''s'");
        assert_eq!(ident("a\"b"), "\"a\"\"b\"");
        assert_eq!(Table { schema: "main".into(), name: "t".into() }.label(), "t");
        assert_eq!(Table { schema: "s".into(), name: "t".into() }.label(), "s.t");
        assert!(check_crs(Some("EPSG:2263")).is_ok() && check_crs(None).is_ok());
        assert!(check_crs(Some("EPSG:2263'; --")).is_err() && check_crs(Some("")).is_err());
        assert_eq!(type_crs("GEOMETRY('EPSG:3857')"), Some("EPSG:3857"));
        assert_eq!(type_crs("GEOMETRY"), None);
        assert_eq!(strip_ansi("\u{1b}[90mWARNING\u{1b}[0m x"), "WARNING x");
        let m = message(
            "WARNING: x\nBinder Error: no column \"b\"\n\nLINE 1: SELECT … query('SELECT b')\n        ^",
            &std::process::ExitStatus::default(),
        );
        assert_eq!(m, "Binder Error: no column \"b\"");
    }

    #[test]
    fn cache_files() {
        assert!(ours("0123456789abcdef.points.parquet") && ours("0123456789abcdef.key"));
        assert!(ours("0123456789abcdef.4242.7.lines.tmp.parquet") && ours("0123456789abcdef.4242.7.rows.tmp.json"));
        assert!(!ours("0123456789abcdef.4242.7.lines.tmp.json") && !ours("0123456789abcdef.4242.7.rows.tmp.parquet"));
        assert!(!ours("0123456789abcdef.parquet") && !ours("0123456789abcdef.other.parquet") && !ours("init.sql"));
        assert!(!ours("data.points.parquet") && !ours("0123456789abcdef.x.7.lines.tmp.parquet"));
    }

    #[test]
    fn select_list() {
        let cols = |v: &[(&str, &str)]| v.iter().map(|(n, t)| Column { name: n.to_string(), ty: t.to_string() }).collect::<Vec<_>>();
        // No geometry column.
        let e = select(&cols(&[("a", "INTEGER")]), None, "x", false).unwrap_err();
        assert!(e.to_string().contains("no geometry column"), "{e}");
        // Types the provider doesn't read become text; dates stay dates; a second geometry, WKT.
        let (s, g) = select(
            &cols(&[
                ("d", "DATE"),
                ("geom", "GEOMETRY"),
                ("n", "DECIMAL(10,2)"),
                ("g2", "GEOMETRY('EPSG:3857')"),
                ("t", "TIMESTAMP WITH TIME ZONE"),
                ("ns", "TIMESTAMP_NS"),
                ("s", "TIMESTAMP_S"),
                ("l", "INTEGER[]"),
            ]),
            None,
            "x",
            false,
        )
        .unwrap();
        assert_eq!(g, "geom");
        assert!(s.contains("(SELECT \"d\", ") && s.contains(", \"t\", ") && s.contains("ST_Force2D(\"geom\") AS \"geom\""), "{s}");
        assert!(s.contains("\"ns\",") && !s.contains("CAST(\"ns\""), "{s}");
        assert!(s.contains("CAST(\"l\" AS VARCHAR)"), "{s}");
        assert!(s.contains("CAST(\"s\" AS TIMESTAMP) AS \"s\""), "{s}");
        assert!(s.contains(", \"n\",") && s.contains("ST_AsText(\"g2\")"), "{s}");
        assert!(s.contains(&format!("(ST_HasZ(\"geom\"::GEOMETRY) OR ST_HasM(\"geom\"::GEOMETRY)) AS {ZM}")), "{s}");
        // A geometry that says its coordinate system is kept in it when RustGIS reads it, and
        // converted when not; one given is given to it, or converted from.
        let typed = cols(&[("g", "GEOMETRY('EPSG:3857')")]);
        assert_eq!(own_system(&typed, None), Some(crate::raster::Crs::WebMercator));
        let (s, _) = select(&typed, None, "x", true).unwrap();
        assert!(!s.contains("ST_Transform") && s.contains("ST_Force2D(\"g\")"), "{s}");
        let (s, _) = select(&typed, None, "x", false).unwrap();
        assert!(s.contains("ST_Transform(\"g\", 'EPSG:4326')"), "{s}");
        let (s, _) = select(&cols(&[("g", "GEOMETRY('OGC:CRS84')")]), None, "x", false).unwrap();
        assert!(!s.contains("ST_Transform"), "{s}");
        let plain = cols(&[("g", "GEOMETRY")]);
        assert_eq!(own_system(&plain, Some("EPSG:2263")), crate::raster::Crs::from_epsg(2263));
        let (s, _) = select(&plain, Some("EPSG:2263"), "x", true).unwrap();
        assert!(s.contains("ST_SetCRS(\"g\"::GEOMETRY, 'EPSG:2263')") && !s.contains("ST_Transform"), "{s}");
        let (s, _) = select(&plain, Some("EPSG:2263"), "x", false).unwrap();
        assert!(s.contains("ST_Transform(\"g\"::GEOMETRY, 'EPSG:2263', 'EPSG:4326')"), "{s}");
        // Longitude/latitude, no system, or one that isn't an EPSG code: converted as before.
        assert_eq!(own_system(&plain, None), None);
        assert_eq!(own_system(&cols(&[("g", "GEOMETRY('EPSG:4326')")]), None), None);
        assert_eq!(own_system(&cols(&[("g", "GEOMETRY('ESRI:54008')")]), None), None);
    }

    #[test]
    fn bad_queries_never_reach_duckdb() {
        let dir = std::env::temp_dir();
        for sql in ["", "  ;  ", "SELECT '\0'"] {
            assert!(run_in(&dir, &Query { sql, ..Query::default() }, false).is_err());
        }
        let long = "x".repeat(MAX_SQL + 1);
        assert!(run_in(&dir, &Query { sql: &long, ..Query::default() }, false).unwrap_err().to_string().contains("longer"));
        let e = run_in(&dir, &Query { sql: "SELECT 1", crs: Some("EPSG:1 OR"), ..Query::default() }, false).unwrap_err();
        assert!(e.to_string().contains("crs"), "{e}");
    }

    /// A query's first rows, as an SQL workspace shows them (skipped without DuckDB).
    #[test]
    fn previews_rows() {
        assert!(checked("  ;").is_err() && checked("SELECT 1\0").is_err());
        if !test_runs().unwrap() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("rustgis-duckdb-preview-{}", std::process::id()));
        let sql = "SELECT i AS n, i AS n, 'x' || i AS label, DATE '2020-01-02' AS day, ST_Point(i, 2) AS geom FROM range(5) t(i)";
        let p = preview_in(&dir, &Query { sql, ..Query::default() }, 3).unwrap();
        let names: Vec<&str> = p.columns.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names.len(), 5, "{names:?}");
        assert!(p.geometry && p.more);
        assert_eq!(p.rows.len(), 3);
        assert_eq!(p.rows[1][2], serde_json::json!("x1"));
        assert_eq!(p.rows[1][3], serde_json::json!("2020-01-02"));
        assert_eq!(p.rows[1][4], serde_json::json!("POINT (1 2)"));
        // A table without geometry, all of it.
        let p = preview_in(&dir, &Query { sql: "SELECT 42 AS answer", ..Query::default() }, 10).unwrap();
        assert!(!p.geometry && !p.more);
        assert_eq!(p.rows, vec![vec![serde_json::json!(42)]]);
        // NaN and infinities show empty; big integers stay numbers.
        let sql = "SELECT 'NaN'::DOUBLE AS a, 'inf'::DOUBLE AS b, 1.5::DOUBLE AS c, 9007199254740993::BIGINT AS d, 7::UINTEGER AS e";
        let p = preview_in(&dir, &Query { sql, ..Query::default() }, 10).unwrap();
        assert_eq!(
            p.rows,
            vec![vec![
                serde_json::Value::Null,
                serde_json::Value::Null,
                serde_json::json!(1.5),
                serde_json::json!(9007199254740993i64),
                serde_json::json!(7)
            ]]
        );
        assert!(preview_in(&dir, &Query { sql: "SELECT * FROM nowhere", ..Query::default() }, 10).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Queries and a database through DuckDB when it is installed (skipped otherwise).
    #[test]
    fn runs_queries_when_duckdb_is_installed() {
        if !test_runs().unwrap() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("rustgis-duckdb-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cache = dir.join("cache");
        let open = |p: &Path| crate::source::open_path(p, None).unwrap();

        // Mixed geometry, a row without one, a projected column and types written as text.
        // An infinite time is written, and read as empty with a count.
        let sql = "SELECT 1 AS id, 'a' AS name, DATE '2020-01-02' AS day, TIMESTAMP_S '2020-01-02 03:04:05' AS seen, ST_Point(10, 20) AS geom \
            UNION ALL SELECT 2, 'b', NULL, NULL, ST_GeomFromText('LINESTRING(0 0, 1 1)') \
            UNION ALL SELECT 3, 'c', NULL, TIMESTAMP_S 'infinity', ST_GeomFromText('POINT(11 21)') \
            UNION ALL SELECT 4, 'd', NULL, NULL, NULL;";
        let out = run_in(&cache, &Query { sql, ..Query::default() }, false).unwrap();
        assert_eq!(out.skipped, 1);
        assert_eq!(out.parts.len(), 2);
        let points = out.parts.iter().find(|p| p.family == Family::Points).unwrap();
        let lines = out.parts.iter().find(|p| p.family == Family::Lines).unwrap();
        assert_eq!(points.count, 2);
        assert_eq!(lines.count, 1);
        assert_eq!((points.zm_dropped, lines.zm_dropped), (0, 0), "x and y only: nothing dropped");
        // Rows with Z or M values are counted, as their values are dropped (#1505).
        let sql = "SELECT ST_GeomFromText('POINT Z (1 2 3)') AS geom UNION ALL SELECT ST_GeomFromText('POINT M (1 2 4)') \
            UNION ALL SELECT ST_Point(5, 6) UNION ALL SELECT ST_GeomFromText('LINESTRING Z (0 0 1, 1 1 2)')";
        let zm = run_in(&cache, &Query { sql, ..Query::default() }, false).unwrap();
        let dropped = |f: Family| zm.parts.iter().find(|p| p.family == f).map(|p| (p.count, p.zm_dropped));
        assert_eq!((dropped(Family::Points), dropped(Family::Lines)), (Some((3, 2)), Some((1, 1))));
        let line_source = open(&lines.path);
        let line_batch = line_source.read(&crate::source::Request { extent: None, limit: 10, own: None }).unwrap();
        assert_eq!(line_batch.features.len(), 1);
        assert_eq!(line_batch.features[0].geometry.as_ref().unwrap().kind(), crate::GeometryType::Polyline);
        assert_eq!(line_batch.features[0].geometry.as_ref().unwrap().extent(), rustgis_geom::Extent::new(0.0, 0.0, 1.0, 1.0));
        let pts = open(&points.path);
        let info = pts.info();
        assert_eq!(info.count, Some(2));
        assert!(info.indexed, "the bbox covering is found");
        assert_eq!(info.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), vec!["id", "name", "day", "seen"]);
        assert_eq!(info.fields[2].ty, crate::FieldType::Date);
        assert_eq!(info.fields[3].ty, crate::FieldType::DateTime);
        let b = pts
            .read(&crate::source::Request { extent: Some(rustgis_geom::Extent::new(10.5, 20.5, 12.0, 22.0)), limit: 10, own: None })
            .unwrap();
        assert_eq!(b.features.len(), 1, "the extent query reads the covering");
        assert_eq!(b.features[0].geometry.as_ref().unwrap().extent(), rustgis_geom::Extent::new(11.0, 21.0, 11.0, 21.0));
        let all_points = pts.read(&crate::source::Request { extent: None, limit: 10, own: None }).unwrap();
        assert_eq!(all_points.features.len(), 2);
        assert!(all_points.features.iter().all(|f| f.geometry.as_ref().unwrap().kind() == crate::GeometryType::Point));
        let days: Vec<String> = all_points.features.iter().map(|f| f.attrs[2].to_string()).collect();
        assert!(days.contains(&"2020-01-02".to_string()), "{days:?}");
        assert_eq!(all_points.unread_dates.iter().sum::<usize>(), 1);
        assert_eq!(all_points.unread_dates.get(3), Some(&1));
        // Each date type passed through writes infinity, which is read as empty and counted.
        let sql = "SELECT 1 AS id, DATE 'infinity' AS d, TIMESTAMP 'infinity' AS t, TIMESTAMP_S '-infinity' AS s, \
            TIMESTAMP_MS 'infinity' AS ms, TIMESTAMPTZ 'infinity' AS tz, ST_Point(1, 2) AS geom";
        let out = run_in(&cache, &Query { sql, ..Query::default() }, false).unwrap();
        let infinite = open(&out.parts[0].path).read(&crate::source::Request { extent: None, limit: 10, own: None }).unwrap();
        assert_eq!(infinite.features.len(), 1);
        assert_eq!(infinite.unread_dates, [0, 1, 1, 1, 1, 1]);
        // A nanosecond timestamp is a timestamp, rounded to the microsecond and counted.
        let sql = "SELECT 1 AS id, TIMESTAMP_NS '2026-03-05 12:34:56.123456789' AS n, TIMESTAMP_NS '2026-03-05 12:34:56.123456' AS m, \
            ST_Point(1, 2) AS geom";
        let out = run_in(&cache, &Query { sql, ..Query::default() }, false).unwrap();
        let nanos = open(&out.parts[0].path);
        assert_eq!(
            nanos.info().fields.iter().map(|f| f.ty).collect::<Vec<_>>(),
            [crate::FieldType::Integer, crate::FieldType::DateTime, crate::FieldType::DateTime]
        );
        let batch = nanos.read(&crate::source::Request { extent: None, limit: 10, own: None }).unwrap();
        assert!(batch.unread_dates.is_empty());
        assert_eq!(batch.rounded_dates, [0, 1]);
        assert_eq!(batch.features[0].attrs[1].to_string(), "2026-03-05T12:34:56.123457");
        assert_eq!(batch.features[0].attrs[2].to_string(), "2026-03-05T12:34:56.123456");
        // No temporary files are left.
        let names: Vec<String> =
            std::fs::read_dir(&cache).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert!(names.iter().all(|n| !n.contains(".tmp.")), "{names:?}");

        // A projected geometry that says its coordinate system is kept in it, as stored, and
        // placed where it is in longitude/latitude…
        let first = |path: &Path| {
            let source = open(path);
            let batch = source.read(&crate::source::Request { extent: None, limit: 1, own: None }).unwrap();
            let Some(crate::Geometry::Point(c)) = batch.features.into_iter().next().unwrap().geometry else { panic!() };
            (source.coord_crs(), c, source.info().extent)
        };
        let out =
            run_in(&cache, &Query { sql: "SELECT ST_Point(1113194.9, 0)::GEOMETRY('EPSG:3857') AS g", ..Query::default() }, false).unwrap();
        let (crs, c, e) = first(&out.parts[0].path);
        assert_eq!((crs, c), (crate::raster::Crs::WebMercator, rustgis_geom::Coord::new(1113194.9, 0.0)));
        assert!((e.min_x - 10.0).abs() < 1e-3 && e.min_y.abs() < 1e-6, "{e:?}");
        // …and so is one given with "crs".
        let q = Query { sql: "SELECT ST_Point(2600000, 1200000) AS g", crs: Some("EPSG:2056"), ..Query::default() };
        let (crs, c, e) = first(&run_in(&cache, &q, false).unwrap().parts[0].path);
        assert_eq!((crs, c), (crate::raster::Crs::from_epsg(2056).unwrap(), rustgis_geom::Coord::new(2_600_000.0, 1_200_000.0)));
        assert!((e.min_x - 7.4386).abs() < 1e-3 && (e.min_y - 46.9511).abs() < 1e-3, "{e:?}");
        // One given in longitude/latitude, or by a code RustGIS doesn't read, comes out in
        // longitude/latitude, as before.
        let q = Query { sql: "SELECT ST_Point(10, 20) AS g", crs: Some("EPSG:4326"), ..Query::default() };
        assert_eq!(first(&run_in(&cache, &q, false).unwrap().parts[0].path).0, crate::raster::Crs::LonLat);
        let q = Query { sql: "SELECT ST_Point(1000000, 2000000) AS g", crs: Some("ESRI:54008"), ..Query::default() };
        let (crs, c, _) = first(&run_in(&cache, &q, false).unwrap().parts[0].path);
        assert!(crs == crate::raster::Crs::LonLat && c.x.abs() <= 180.0 && c.y.abs() <= 90.0, "{crs:?} {c:?}");

        // Invalid SQL, empty query results and non-spatial results are errors. DuckDB versions
        // format their diagnostics differently; their prose is not the command contract.
        for sql in ["SELEC 1", "SELECT ST_Point(0, 0) AS g WHERE false", "SELECT 1 AS a"] {
            assert!(run_in(&cache, &Query { sql, ..Query::default() }, false).is_err(), "{sql}");
        }
        for sql in ["COPY (SELECT 1) TO 'x.csv'", "SELECT ST_Point(0, 0) AS g'); COPY (SELECT 1) TO ('x.csv", "SELECT 1; SELECT 2"] {
            assert!(run_in(&cache, &Query { sql, ..Query::default() }, false).is_err(), "{sql}");
        }
        assert!(!dir.join("x.csv").exists() && !Path::new("x.csv").exists());

        // A database: its geometry tables, converted once.
        let db = dir.join("t.duckdb");
        let secret = dir.join("secret.txt");
        std::fs::write(&secret, "do not read").unwrap();
        let duckdb = program().unwrap();
        let st = Command::new(duckdb)
            .arg(&db)
            .arg("-c")
            .arg("LOAD spatial; CREATE SCHEMA s2; CREATE TABLE pts AS SELECT i AS id, ST_Point(i % 360 - 180, i % 170 - 85) AS geom FROM range(1000) t(i); CREATE TABLE s2.polys AS SELECT 1 AS id, ST_GeomFromText('POLYGON((0 0, 1 0, 1 1, 0 0))') AS geom; CREATE TABLE plain(a INT);")
            .arg("-c")
            .arg(format!("CREATE VIEW leak AS SELECT content AS c, ST_Point(0, 0) AS geom FROM read_text({});", literal(&secret.display().to_string())))
            .status()
            .unwrap();
        assert!(st.success());
        let tables = tables_in(&cache, &Database::File(db.clone())).unwrap();
        assert_eq!(tables.iter().map(Table::label).collect::<Vec<_>>(), vec!["leak", "pts", "s2.polys"]);
        // A view reaching outside the database is refused: tables are read confined to the cache.
        assert!(convert_table_in(&cache, &db, &tables[0], None).is_err());
        let tables = &tables[1..];
        let a = convert_table_in(&cache, &db, &tables[0], None).unwrap();
        assert_eq!(a.parts.iter().map(|p| p.count).collect::<Vec<_>>(), vec![1000]);
        // The second time is a cache hit, read back from the key file.
        assert_eq!(convert_table_in(&cache, &db, &tables[0], None).unwrap(), a);
        // A selected family the table no longer holds is an empty branch, with the other family's
        // rows counted; its private workspace disappears with its owner even though ordinary
        // cached results remain reusable.
        let fresh = convert_table_fresh_in(&cache, &db, &tables[0], Family::Lines, None).unwrap();
        assert_eq!(fresh.output.parts.iter().map(|p| (p.family, p.count)).collect::<Vec<_>>(), vec![(Family::Lines, 0)]);
        assert_eq!(fresh.output.other_family, 1000);
        drop(fresh);
        assert!(
            std::fs::read_dir(&cache).unwrap().flatten().all(|entry| !entry.file_name().to_string_lossy().starts_with("reload-")),
            "dropped fresh conversions must not retain workspaces"
        );
        // Changes still in the write-ahead log are a different file.
        let mut wal = db.clone().into_os_string();
        wal.push(".wal");
        std::fs::write(&wal, b"pending").unwrap();
        let fresh = convert_table_in(&cache, &db, &tables[0], None).unwrap();
        assert_ne!(fresh.parts[0].path, a.parts[0].path);
        std::fs::remove_file(&wal).unwrap();
        let key = std::fs::read_dir(&cache).unwrap().flatten().find(|e| e.file_name().to_string_lossy().ends_with(".key")).unwrap();
        assert!(std::fs::read_to_string(key.path()).unwrap().contains("part points 1000 "));
        // A query naming the database's tables plainly.
        let q = Query { sql: "SELECT * FROM pts WHERE id < 10", database: Some(&Database::File(db.clone())), crs: None };
        assert_eq!(run_in(&cache, &q, false).unwrap().parts[0].count, 10);
        let _ = std::fs::remove_dir_all(dir);
    }
}
