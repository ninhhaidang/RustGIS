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
use super::external::{self, TempFile, fnv1a, output, stamp, touch};
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

/// A live-only database named for a query: add its tables instead.
fn live_not_here() -> DataError {
    DataError::Unsupported(
        "SQL queries don't run on MySQL, MariaDB or SQL Server databases: add their tables with Home › Add › Add Data › Database…".into(),
    )
}

fn missing() -> DataError {
    DataError::Unsupported(
        "SQL queries and DuckDB databases need DuckDB: install it (the duckdb program on the PATH, or RUSTGIS_DUCKDB_BIN naming its folder) and try again"
            .into(),
    )
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
            s.push_str(&format!(" ATTACH {} AS src (READ_ONLY); USE src;", literal(&full.display().to_string())));
        }
        // PostGIS geometry columns come through as DuckDB geometries with their coordinate system.
        Some(Database::Postgres(dsn)) => {
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

/// Runs a script.
fn run_script(dir: &Path, script: &str, database: Option<&Database>, timeout: std::time::Duration) -> Result<()> {
    let duckdb = program().ok_or_else(missing)?;
    // An empty start-up file, so the user's own (~/.duckdbrc) can't change what runs.
    let init = dir.join("init.sql");
    if !init.is_file() {
        std::fs::write(&init, b"")?;
    }
    let mut c = Command::new(duckdb);
    c.arg("-init").arg(&init).arg("-c").arg(script);
    if let Some(p) = password(database) {
        c.env("PGPASSWORD", p);
    }
    let out = output(c, timeout, "DuckDB")?;
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

/// Column types the GeoParquet provider reads as they are; others are written as text.
fn kept_as_is(ty: &str) -> bool {
    matches!(
        ty,
        "BOOLEAN" | "TINYINT" | "SMALLINT" | "INTEGER" | "BIGINT" | "UTINYINT" | "USMALLINT" | "UINTEGER" | "FLOAT" | "DOUBLE" | "VARCHAR"
    ) || ty.starts_with("DECIMAL(")
}

fn is_geometry(ty: &str) -> bool {
    ty == "GEOMETRY" || ty.starts_with("GEOMETRY(")
}

/// The coordinate system a geometry type says (`GEOMETRY('EPSG:3857')`).
fn type_crs(ty: &str) -> Option<&str> {
    ty.strip_prefix("GEOMETRY('").and_then(|r| r.strip_suffix("')")).filter(|c| !c.is_empty())
}

/// The SELECT that turns a result into what is written: the first geometry column in
/// longitude/latitude, other geometry columns as WKT, types the provider doesn't read as text, and
/// each row's geometry family; and the geometry column's name.
fn select(columns: &[Column], crs: Option<&str>, from: &str) -> Result<(String, String)> {
    let geom = columns
        .iter()
        .find(|c| is_geometry(&c.ty))
        .ok_or_else(|| DataError::Format("the query returns no geometry column: select one (such as ST_Point(lon, lat) AS geom)".into()))?;
    let g = ident(&geom.name);
    // A geometry that says no coordinate system (the usual case for longitude/latitude) is taken
    // as it is.
    let geom_expr = match (crs, type_crs(&geom.ty)) {
        (Some(c), _) => format!("ST_Transform({g}::GEOMETRY, {}, 'EPSG:4326')", literal(c)),
        (None, Some(t)) if t.eq_ignore_ascii_case("EPSG:4326") || t.eq_ignore_ascii_case("OGC:CRS84") => g.clone(),
        (None, Some(_)) => format!("ST_Transform({g}, 'EPSG:4326')"),
        (None, None) => g.clone(),
    };
    let list: Vec<String> = columns
        .iter()
        .map(|c| {
            let n = ident(&c.name);
            if std::ptr::eq(c, geom) {
                format!("ST_Force2D({geom_expr}) AS {n}")
            } else if is_geometry(&c.ty) {
                format!("ST_AsText({n}) AS {n}")
            } else if kept_as_is(&c.ty) {
                n
            } else {
                format!("CAST({n} AS VARCHAR) AS {n}")
            }
        })
        .collect();
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
    if columns.iter().any(|c| c.name == DIM) {
        return Err(DataError::Format(format!("the query returns a column named {DIM}")));
    }
    let (select, geom) = select(&columns, q.crs, &from)?;
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
    for f in Family::ALL {
        script.push_str(&format!(
            " COPY (SELECT * EXCLUDE ({DIM}){bbox} FROM r WHERE {DIM} = {} ORDER BY ST_Hilbert({g}, {{'min_x': -180, 'min_y': -90, 'max_x': 180, 'max_y': 90}}::BOX_2D)) TO {} (FORMAT parquet, ROW_GROUP_SIZE 16384);",
            dimension(f),
            literal(&tmp(f).display().to_string())
        ));
    }
    let counts =
        fetch(dir, &script, q.database, &format!("SELECT {DIM} AS dim, count(*)::BIGINT AS n FROM r GROUP BY ALL"), QUERY_TIMEOUT)?;

    let mut out = Output::default();
    for row in counts.as_array().into_iter().flatten() {
        let count = row.get("n").and_then(|v| v.as_u64()).unwrap_or(0);
        match row.get("dim").and_then(|v| v.as_i64()).and_then(|d| Family::ALL.into_iter().find(|f| dimension(*f) == d)) {
            Some(family) if count > 0 => {
                let path = dir.join(format!("{run_tag}.{}.parquet", family.label()));
                out.parts.push(Part { path, family, count });
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
            out.parts.push(Part { path: dir.join(format!("{run_tag}.{}.parquet", family.label())), family, count: 0 });
        }
    }
    if out.parts.is_empty() {
        return Err(DataError::Format(if out.skipped > 0 {
            format!("none of the query's {} rows has a geometry", out.skipped)
        } else {
            NO_ROWS.into()
        }));
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
    // Changes not yet checkpointed are in the write-ahead log beside the file.
    let mut wal = full.clone().into_os_string();
    wal.push(".wal");
    let (wal_len, wal_modified) = stamp(Path::new(&wal)).unwrap_or((0, 0));
    // The cache key, spelled out: kept beside the conversion and checked, so a hash collision
    // can't hand back another table.
    let key = format!("{}\n{}\n{}\n{crs:?}\n{len}\n{modified}\n{wal_len}\n{wal_modified}", full.display(), table.schema, table.name);
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
    // (its family, rows and file) and the rows left out.
    let mut text = key.clone();
    for p in &out.parts {
        let file = p.path.file_name().and_then(|f| f.to_str()).unwrap_or_default();
        text.push_str(&format!("\npart {} {} {file}", p.family.label(), p.count));
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
        match (w.next(), w.next(), w.next(), w.next()) {
            // Only a file of this module's, in the cache folder.
            (Some("part"), Some(f), Some(n), Some(file)) if ours(file) && file.ends_with(&format!(".{f}.parquet")) => {
                let family = Family::ALL.into_iter().find(|x| x.label() == f)?;
                let path = dir.join(file);
                if !path.is_file() {
                    return None;
                }
                out.parts.push(Part { path, family, count: n.parse().ok()? });
            }
            (Some("skipped"), Some(n), None, None) => out.skipped = n.parse().ok()?,
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
        let e = select(&cols(&[("a", "INTEGER")]), None, "x").unwrap_err();
        assert!(e.to_string().contains("no geometry column"), "{e}");
        // Types the provider doesn't read become text; a second geometry, WKT.
        let (s, g) =
            select(&cols(&[("d", "DATE"), ("geom", "GEOMETRY"), ("n", "DECIMAL(10,2)"), ("g2", "GEOMETRY('EPSG:3857')")]), None, "x")
                .unwrap();
        assert_eq!(g, "geom");
        assert!(s.contains("CAST(\"d\" AS VARCHAR)") && s.contains("ST_Force2D(\"geom\") AS \"geom\""), "{s}");
        assert!(s.contains(", \"n\",") && s.contains("ST_AsText(\"g2\")"), "{s}");
        // A geometry that says its coordinate system is converted; one given is used.
        let (s, _) = select(&cols(&[("g", "GEOMETRY('EPSG:3857')")]), None, "x").unwrap();
        assert!(s.contains("ST_Transform(\"g\", 'EPSG:4326')"), "{s}");
        let (s, _) = select(&cols(&[("g", "GEOMETRY('OGC:CRS84')")]), None, "x").unwrap();
        assert!(!s.contains("ST_Transform"), "{s}");
        let (s, _) = select(&cols(&[("g", "GEOMETRY")]), Some("EPSG:2263"), "x").unwrap();
        assert!(s.contains("ST_Transform(\"g\"::GEOMETRY, 'EPSG:2263', 'EPSG:4326')"), "{s}");
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
        if !available() {
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
        if !available() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("rustgis-duckdb-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cache = dir.join("cache");
        let open = |p: &Path| crate::source::open_path(p, None).unwrap();

        // Mixed geometry, a row without one, a projected column and types written as text.
        let sql = "SELECT 1 AS id, 'a' AS name, DATE '2020-01-02' AS day, ST_Point(10, 20) AS geom \
            UNION ALL SELECT 2, 'b', NULL, ST_GeomFromText('LINESTRING(0 0, 1 1)') \
            UNION ALL SELECT 3, 'c', NULL, ST_GeomFromText('POINT(11 21)') \
            UNION ALL SELECT 4, 'd', NULL, NULL;";
        let out = run_in(&cache, &Query { sql, ..Query::default() }, false).unwrap();
        assert_eq!(out.skipped, 1);
        assert_eq!(out.parts.len(), 2);
        let points = out.parts.iter().find(|p| p.family == Family::Points).unwrap();
        let lines = out.parts.iter().find(|p| p.family == Family::Lines).unwrap();
        assert_eq!(points.count, 2);
        assert_eq!(lines.count, 1);
        let line_source = open(&lines.path);
        let line_batch = line_source.read(&crate::source::Request { extent: None, limit: 10 }).unwrap();
        assert_eq!(line_batch.features.len(), 1);
        assert_eq!(line_batch.features[0].geometry.as_ref().unwrap().kind(), crate::GeometryType::Polyline);
        assert_eq!(line_batch.features[0].geometry.as_ref().unwrap().extent(), rustgis_geom::Extent::new(0.0, 0.0, 1.0, 1.0));
        let pts = open(&points.path);
        let info = pts.info();
        assert_eq!(info.count, Some(2));
        assert!(info.indexed, "the bbox covering is found");
        assert_eq!(info.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), vec!["id", "name", "day"]);
        assert_eq!(info.fields[2].ty, crate::FieldType::Text);
        let b = pts.read(&crate::source::Request { extent: Some(rustgis_geom::Extent::new(10.5, 20.5, 12.0, 22.0)), limit: 10 }).unwrap();
        assert_eq!(b.features.len(), 1, "the extent query reads the covering");
        assert_eq!(b.features[0].geometry.as_ref().unwrap().extent(), rustgis_geom::Extent::new(11.0, 21.0, 11.0, 21.0));
        let all_points = pts.read(&crate::source::Request { extent: None, limit: 10 }).unwrap();
        assert_eq!(all_points.features.len(), 2);
        assert!(all_points.features.iter().all(|f| f.geometry.as_ref().unwrap().kind() == crate::GeometryType::Point));
        // No temporary files are left.
        let names: Vec<String> =
            std::fs::read_dir(&cache).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert!(names.iter().all(|n| !n.contains(".tmp.")), "{names:?}");

        // A projected geometry that says its coordinate system comes out in longitude/latitude.
        let out =
            run_in(&cache, &Query { sql: "SELECT ST_Point(1113194.9, 0)::GEOMETRY('EPSG:3857') AS g", ..Query::default() }, false).unwrap();
        let e = open(&out.parts[0].path).info().extent;
        assert!((e.min_x - 10.0).abs() < 1e-3 && e.min_y.abs() < 1e-6, "{e:?}");
        // …and so does one given with "crs".
        let q = Query { sql: "SELECT ST_Point(1113194.9, 0) AS g", crs: Some("EPSG:3857"), ..Query::default() };
        let e = open(&run_in(&cache, &q, false).unwrap().parts[0].path).info().extent;
        assert!((e.min_x - 10.0).abs() < 1e-3, "{e:?}");

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
