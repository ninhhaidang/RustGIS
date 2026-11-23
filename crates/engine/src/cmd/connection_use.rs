//! User-initiated database reads use the requested saved ID, never a same-address row's login.

use serde_json::Value;
#[cfg(not(target_arch = "wasm32"))]
use serde_json::json;

use crate::Session;
use crate::connections::Connection;

/// Geometry-table details shared by saved-ID Browse and Test. An explicitly typed Test password
/// replaces session state only if authentication wasn't refused; a later listing failure keeps it.
/// Call only after the probe return: a probe must neither read secrets nor connect.
///
/// Gives the rows, with what is known of an Esri enterprise geodatabase: `{owner, release}` when
/// the rows are a geodatabase's (each row then has an `item`), and why its catalog couldn't be
/// read when the database has one that is browsed as plain SQL Server.
pub(super) fn tables_and_geodatabase(
    s: &mut Session,
    c: &Connection,
    typed: Option<&str>,
) -> Result<(Vec<Value>, Option<Value>, Option<String>), String> {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (s, c, typed);
        Err(crate::connections::DESKTOP_ONLY.into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use rustgis_data::DataError;
        use rustgis_data::source::duckdb::{self, Database};

        let database = Database::parse(&c.address);
        let (kind, key) = match &database {
            Database::Postgres(key) => ("postgres", key),
            Database::Mysql(key) => ("mysql", key),
            Database::Sqlserver(key) => ("sqlserver", key),
            Database::File(_) => return Err("not a database server".into()),
        };
        let credential = if typed.is_some() {
            // Keep session, selection and activation bookkeeping unchanged during the attempt.
            None
        } else {
            // Another saved ID's activation is not authority to read this connection.
            // Passwords explicitly typed for the address remain intact.
            duckdb::set_connection_password(key, &c.id, None);
            s.database_logins.drop_activation(key);
            if !s.database_logins.has_session_password(&c.id) && duckdb::has_explicit_password(key) {
                None
            } else {
                super::credential(s, c)?
            }
        };
        if typed.is_none() && credential.is_none() && s.database_logins.is_rejected(&c.id) && !duckdb::has_explicit_password(key) {
            super::request_password(s, c);
            return Err("password is required".into());
        }
        let saved = credential.as_ref().is_some_and(|(_, saved)| *saved);
        let session = credential.as_ref().is_some_and(|(_, saved)| !*saved);
        if typed.is_none()
            && let Some((password, _)) = &credential
        {
            super::activate(s, c, password.expose());
            s.database_logins.select(kind, key, &c.id);
        }
        let result = if let Some(pw) = typed {
            duckdb::try_connection_password(key, &c.id, pw, || read_tables(&database), |result| matches!(result, Err(DataError::Login(_))))
        } else {
            read_tables(&database)
        };
        if let Some(pw) = typed
            && !matches!(&result, Err(DataError::Login(_)))
        {
            s.database_logins.set_session_password(&c.id, pw);
            s.database_logins.select(kind, key, &c.id);
            s.database_logins.note_activation(key, &c.id);
        }

        match result {
            Ok(listing) => {
                s.database_logins.clear_requests_for(kind, key);
                Ok(listing)
            }
            Err(error) => {
                let password = typed.or_else(|| credential.as_ref().map(|(pw, _)| pw.expose())).unwrap_or("");
                let error = error.redact_secret(password);
                if matches!(&error, DataError::Login(_)) {
                    if saved {
                        s.database_logins.reject(&c.id);
                    } else if session {
                        s.database_logins.reject_session_password(c);
                    }
                    if saved || session {
                        duckdb::forget_connection_password(&c.id);
                        s.database_logins.drop_activation(key);
                    }
                    super::request_password(s, c);
                }
                Err(format!("{}: {error}", database.short()))
            }
        }
    }
}

/// What a database lists: its rows, and a geodatabase's owner and release, or why its catalog
/// couldn't be read.
#[cfg(not(target_arch = "wasm32"))]
type Listing = (Vec<Value>, Option<Value>, Option<String>);

#[cfg(not(target_arch = "wasm32"))]
fn read_tables(database: &rustgis_data::source::duckdb::Database) -> rustgis_data::Result<Listing> {
    use rustgis_data::source::{duckdb::Database, mysql, postgis, sqlserver};

    // Keep DataError intact through both connect and table listing so only an authentication
    // refusal suppresses a saved credential. A transport or query error must not do that.
    match database {
        Database::Postgres(address) => {
            let connection = postgis::connect(address, true)?;
            Ok((
                postgis::tables(&connection)?
                    .into_iter()
                    .map(|t| json!({"label": t.label(), "schema": t.schema, "name": t.name, "column": t.column, "kind": t.kind}))
                    .collect(),
                None,
                None,
            ))
        }
        Database::Mysql(address) => {
            let connection = mysql::connect(address, true)?;
            Ok((
                mysql::tables(&connection)?
                    .into_iter()
                    .map(|t| json!({"label": t.label(), "schema": t.schema, "name": t.name, "column": t.column, "kind": t.kind}))
                    .collect(),
                None,
                None,
            ))
        }
        Database::Sqlserver(address) => {
            let connection = sqlserver::connect(address, true)?;
            sqlserver_listing(&connection)
        }
        Database::File(_) => Err(rustgis_data::DataError::Format("not a database server".into())),
    }
}

/// A geodatabase's listing as it was read, and why it wasn't when it failed. Only a login refusal
/// ends the listing; any other trouble is kept as the problem, and the database then browses as
/// plain SQL Server, saying why (the catalog may have been read, and only its tables' listing
/// failed: [`rustgis_data::source::sqlserver::geodatabase_problem`] then has nothing to say).
#[cfg(not(target_arch = "wasm32"))]
pub(in crate::cmd) fn listing_or_problem<T>(read: rustgis_data::Result<Option<T>>) -> rustgis_data::Result<(Option<T>, Option<String>)> {
    match read {
        Err(error @ rustgis_data::DataError::Login(_)) => Err(error),
        Ok(listing) => Ok((listing, None)),
        Err(error) => Ok((None, Some(error.to_string()))),
    }
}

/// A SQL Server database's rows: a geodatabase's classes, tables and other spatial tables, or
/// the plain spatial tables (with why the catalog was left unread, when it was).
#[cfg(not(target_arch = "wasm32"))]
pub(in crate::cmd) fn sqlserver_listing(connection: &rustgis_data::source::sqlserver::Conn) -> rustgis_data::Result<Listing> {
    use rustgis_data::source::sqlserver;

    let plain = |t: sqlserver::Table| json!({"label": t.label(), "schema": t.schema, "name": t.name, "column": t.column, "kind": t.kind});
    let (listing, problem) = listing_or_problem(sqlserver::geodatabase_tables(connection))?;
    let Some(listing) = listing else {
        let rows = sqlserver::tables(connection)?.into_iter().map(plain).collect();
        return Ok((rows, None, problem.or_else(|| sqlserver::geodatabase_problem(connection))));
    };
    let columns_cut = listing.cut_note();
    let rows = geodatabase_rows(listing);
    // A catalog, or a list of spatial columns, cut at its limit says so wherever the listing is
    // shown.
    let geodatabase = sqlserver::geodatabase(connection).map(|g| {
        let mut out = json!({"owner": g.owner, "release": g.release});
        let notes: Vec<String> = g.cut_note().into_iter().chain(columns_cut.clone()).collect();
        if !notes.is_empty() {
            out["note"] = json!(notes.join("; "));
        }
        out
    });
    Ok((rows, geodatabase, None))
}

/// A geodatabase listing's rows. A class without a spatial column (its table holds no native one)
/// is listed with `column` and `kind` null: it isn't a layer, and refuses at open. A spatial
/// table the catalog doesn't register has no dataset, alias or versioning (null), and is shown
/// by its label: its name with its schema, as two schemas may hold a table of one name.
#[cfg(not(target_arch = "wasm32"))]
pub(in crate::cmd) fn geodatabase_rows(listing: rustgis_data::source::sqlserver::GdbListing) -> Vec<Value> {
    let mut rows = Vec::new();
    for (item, table) in &listing.classes {
        let (schema, name, column, kind, label) = match table {
            Some(t) => (t.schema.clone(), t.name.clone(), json!(t.column), json!(t.kind), t.label()),
            None => (item.schema.clone(), item.table.clone(), Value::Null, Value::Null, format!("{}.{}", item.schema, item.table)),
        };
        rows.push(json!({
            "label": label, "schema": schema, "name": name, "column": column, "kind": kind,
            "item": "featureClass", "dataset": item.dataset, "alias": item.alias, "versioning": item.versioning,
            "geometry": item.family.map(|f| f.label()),
            "display": item.alias.as_deref().unwrap_or_else(|| item.short_name()),
        }));
    }
    for item in &listing.tables {
        rows.push(json!({
            "label": format!("{}.{}", item.schema, item.table), "schema": item.schema, "name": item.table,
            "item": "table", "dataset": item.dataset, "alias": item.alias, "versioning": item.versioning,
            "display": item.alias.as_deref().unwrap_or_else(|| item.short_name()),
        }));
    }
    for t in listing.other {
        rows.push(json!({
            "label": t.label(), "schema": t.schema, "name": t.name, "column": t.column, "kind": t.kind, "item": "other",
            "dataset": null, "alias": null, "versioning": null, "display": t.label(),
        }));
    }
    rows
}

/// The labels of the rows that add a layer: a plain database's, and a geodatabase's classes with
/// a spatial column and other spatial tables.
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn layer_labels(rows: &[Value]) -> Vec<&str> {
    let is_layer = |r: &Value| match r.get("item").and_then(Value::as_str) {
        Some("table") => false,
        Some("featureClass") => r.get("kind").is_some_and(|k| k.as_str().is_some_and(|k| !k.is_empty())),
        _ => true,
    };
    rows.iter().filter(|r| is_layer(r)).filter_map(|r| r.get("label").and_then(Value::as_str)).collect()
}

/// What a listing tells the caller: how many layers, which (and a geodatabase's owner and
/// release, or why its catalog went unread).
#[cfg(not(target_arch = "wasm32"))]
pub(in crate::cmd) fn listing_result(rows: &[Value], geodatabase: Option<Value>, problem: Option<String>) -> Value {
    let layers = layer_labels(rows);
    let mut out = json!({"count": layers.len(), "layers": layers});
    if let Some(g) = geodatabase {
        out["geodatabase"] = g;
    }
    if let Some(p) = problem {
        out["geodatabaseProblem"] = json!(p);
    }
    out
}
