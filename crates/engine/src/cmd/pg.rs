//! PostGIS: connect to a PostgreSQL database with PostGIS, list its tables and add them as layers
//! that can be edited (desktop only). Home › Add Data › Database… (Load for editing) and
//! Edit › Manage Edits run these.
//!
//! The connection is the session's (`Session::postgis`), never the project's, and never with its
//! password, which is remembered for the session only. A table added with `pg.addLayer` is loaded
//! whole, its feature ids its integer primary key, and the layer keeps each row's version
//! (`xmin`) as read, so saving can tell a row someone else changed since from one to overwrite.
//!
//! `pg.saveEdits` writes what changed in such a layer back to its table in one transaction, and
//! saves nothing when a row changed or deleted there was changed or deleted by someone else
//! since, or a new row's key is taken. `pg.discardEdits` loads the rows again as they are now.

use serde_json::Value;
#[cfg(not(target_arch = "wasm32"))]
use serde_json::json;

#[cfg(not(target_arch = "wasm32"))]
use super::{f, ps};
use crate::{CmdResult, CommandSpec as C, Session};

/// Why a command needs a connection first.
const NOT_CONNECTED: &str = "connect to a PostGIS database first (Home › Add Data › Database…, Load for editing)";

/// `sslmode`s a connection may name, as libpq's.
#[cfg(not(target_arch = "wasm32"))]
const SSL_MODES: &[&str] = &["disable", "allow", "prefer", "require", "verify-ca", "verify-full"];

/// The most tables `pg.tables` lists.
#[cfg(not(target_arch = "wasm32"))]
const MAX_LISTED: usize = 5_000;

pub fn commands() -> Vec<C> {
    vec![
        C::new("pg.connect", "Connect PostGIS", "Home › Add › Add Data", connect).view().params(
            r#"{"connection"?: "host=… port=… dbname=… user=…, or a postgresql:// address (instead of the parts below)", "host"?: string, "port"?: number, "database"?: string, "user"?: string, "password"?: "kept for this session only, never saved", "sslmode"?: "disable"|"prefer"|"require"|"verify-ca"|"verify-full"}"#,
        ),
        C::new("pg.disconnect", "Disconnect PostGIS", "Home › Add › Add Data", disconnect).view().when(need_connection),
        C::new("pg.status", "PostGIS Connection", "Home › Add › Add Data", status).view(),
        C::new("pg.tables", "List PostGIS Tables", "Home › Add › Add Data", tables).view().when(need_connection).params(r#"{"schema"?: string}"#),
        C::new("pg.pendingEdits", "PostGIS Edits", "Edit › Manage Edits", pending_edits).view().params(r#"{"layer"?: "a layer added with pg.addLayer (default: the active one)"}"#),
        C::new("pg.saveEdits", "Save to PostGIS", "Edit › Manage Edits", save_edits)
            .params(r#"{"layer"?: "a layer added with pg.addLayer (default: the active one)"}"#),
        C::new("pg.discardEdits", "Discard PostGIS Edits", "Edit › Manage Edits", discard_edits)
            .params(r#"{"layer"?: "a layer added with pg.addLayer (default: the active one)"}"#),
        C::new("pg.addLayer", "Add PostGIS Layer", "Home › Add › Add Data", add_layer).when(need_connection).params(
            r#"{"table": "table, schema.table, or table:column for one of several geometry columns", "where"?: "an SQL condition on the table's columns", "limit"?: "the most rows of each layer (default and most: 100000)", "name"?: string, "crs"?: "EPSG:… of a geometry that doesn't say"}"#,
        ),
    ]
}

pub(crate) fn need_connection(s: &Session) -> Option<&'static str> {
    s.postgis.is_none().then_some(NOT_CONNECTED)
}

/// The layer `p` names (or the active one), which must have been added from PostGIS: its id,
/// name and table.
fn postgis_layer(s: &Session, p: &Value) -> Result<(u32, String, rustgis_map::source::PostgisEdit), String> {
    let id = s.layer_id(p)?;
    let l = s.map().layer(id).ok_or("no such layer")?;
    let edit = l
        .postgis
        .clone()
        .ok_or_else(|| format!("{}: not a PostGIS layer loaded for editing (Home › Add Data › Database…, Load for editing)", l.name))?;
    Ok((id, l.name.clone(), edit))
}

fn pending_edits(s: &mut Session, p: &Value) -> CmdResult {
    let (id, name, edit) = postgis_layer(s, p)?;
    let l = s.map().layer(id).ok_or("no such layer")?;
    let c = edit.versions.changes(&l.dataset.features);
    Ok(serde_json::json!({
        "layer": name,
        "table": edit.origin.table,
        "inserts": c.inserts.len(),
        "updates": c.updates.len(),
        "deletes": c.deletes.len(),
        "writable": edit.writable,
    }))
}

fn save_edits(s: &mut Session, p: &Value) -> CmdResult {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (s, p);
        Err("PostGIS databases are edited in the desktop app".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use rustgis_data::source::{duckdb::Database, postgis};
        let (id, name, edit) = postgis_layer(s, p)?;
        let l = s.map().layer(id).ok_or("no such layer")?;
        let changes = edit.versions.changes(&l.dataset.features);
        let fields = l.dataset.fields.clone();
        if changes.is_empty() {
            s.notify(format!("{name}: no edits to save"), false);
            return Ok(json!({"saved": false, "inserted": 0, "updated": 0, "deleted": 0}));
        }
        let table = edit.origin.table.clone();
        // Undone past a save: the database holds those edits already, and its new rows would be
        // inserted again.
        if s.saved_over.iter().any(|v| v.shares(&edit.versions)) {
            return Err(format!(
                "{name}: the layer is back before an edit saved to {table} (undone), so nothing was saved. Discard Edits loads the rows as they are now"
            ));
        }
        if !edit.writable {
            return Err(format!("{name}: the database's user can't insert, update or delete the rows of {table}"));
        }
        let Database::Postgres(dsn) = Database::parse(&edit.origin.database) else {
            return Err(format!("{name}: not a PostgreSQL connection"));
        };
        let label = Database::Postgres(dsn.clone()).short();
        let err = |e: rustgis_data::DataError| format!("{label}: {e}");
        // The user's own credentials (PGPASSWORD, the password file) go only to the database
        // they connected to this session, never to one a project file names.
        let ask = s.postgis.as_deref() == Some(dsn.as_str());
        let conn = postgis::connect_writable(&dsn, ask)
            .map_err(|e| format!("{label}: {e} (connect again: Home › Add Data › Database…, Load for editing)"))?;
        let t = postgis::find_table(&conn, &table).map_err(err)?;
        let src = postgis::PostgisSource::open(conn, t, edit.origin.family, edit.origin.crs.as_deref()).map_err(err)?;
        let rows = changes.insert_rows.clone();
        let edits = rustgis_data::source::edits::Edits { updates: changes.updates, deletes: changes.deletes, inserts: changes.inserts };
        let saved = match src.save(&edits, &fields).map_err(err)? {
            Ok(saved) => saved,
            Err(conflicts) => {
                let mut said: Vec<String> = conflicts.iter().take(20).map(ToString::to_string).collect();
                if conflicts.len() > 20 {
                    said.push(format!("{} more", conflicts.len() - 20));
                }
                return Err(format!(
                    "{name}: nothing was saved to {table}, {} conflicts: {}. Discard Edits loads the rows as they are now",
                    conflicts.len(),
                    said.join("; ")
                ));
            }
        };
        // Saved: from here on nothing fails, so the layer matches the database. New features
        // take the keys they were saved with.
        let key_field = fields.iter().position(|f| f.name == edit.key);
        let versions: std::collections::HashMap<u64, i64> = saved.versions.iter().copied().collect();
        if let Some(l) = s.map_mut().layer_mut(id) {
            for (row, (_, key)) in rows.iter().zip(&saved.inserted) {
                if let Some(f) = l.dataset.features.get_mut(*row) {
                    f.id = *key;
                    if let Some(a) = key_field.and_then(|i| f.attrs.get_mut(i)) {
                        *a = rustgis_data::Value::Int(key.cast_signed());
                    }
                }
            }
            let baseline = rustgis_map::source::versions_of(&l.dataset.features, &edit.versions, &versions);
            if let Some(pg) = l.postgis.as_mut() {
                pg.versions = baseline;
            }
        }
        // The versions saved over, while an undo step can bring them back.
        s.saved_over.retain(rustgis_map::source::Versions::held_elsewhere);
        s.saved_over.push(edit.versions);
        s.notify(
            format!("{name}: saved to {table}: {} new, {} changed, {} deleted", saved.inserted.len(), saved.updated, saved.deleted),
            false,
        );
        if !saved.skipped_fields.is_empty() {
            s.notify(format!("{name}: {table} has no column for {}, so it wasn't saved", saved.skipped_fields.join(", ")), false);
        }
        Ok(json!({
            "saved": true,
            "inserted": saved.inserted.len(),
            "updated": saved.updated,
            "deleted": saved.deleted,
            "unchanged": saved.unchanged,
            "keys": saved.inserted.iter().map(|(_, k)| k.cast_signed()).collect::<Vec<_>>(),
            "skippedFields": saved.skipped_fields,
        }))
    }
}

fn discard_edits(s: &mut Session, p: &Value) -> CmdResult {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (s, p);
        Err("PostGIS databases are read in the desktop app".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use rustgis_data::source::{FeatureSource, duckdb::Database, postgis};
        let (id, name, edit) = postgis_layer(s, p)?;
        let Database::Postgres(dsn) = Database::parse(&edit.origin.database) else {
            return Err(format!("{name}: not a PostgreSQL connection"));
        };
        let label = Database::Postgres(dsn.clone()).short();
        let err = |e: rustgis_data::DataError| format!("{label}: {e}");
        let ask = s.postgis.as_deref() == Some(dsn.as_str());
        let conn = postgis::connect(&dsn, ask)
            .map_err(|e| format!("{label}: {e} (connect again: Home › Add Data › Database…, Load for editing)"))?;
        let t = postgis::find_table(&conn, &edit.origin.table).map_err(err)?;
        let src = postgis::PostgisSource::open(conn, t, edit.origin.family, edit.origin.crs.as_deref()).map_err(err)?;
        let limit = if edit.limit == 0 { postgis::MAX_EDITABLE } else { edit.limit };
        let editable = src.read_editable(edit.condition.as_deref(), limit).map_err(err)?;
        let read: std::collections::HashMap<u64, i64> = editable.versions.iter().copied().collect();
        let n = editable.features.len();
        let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
        l.dataset.fields = src.info().fields.clone();
        l.dataset.features = editable.features.into();
        l.selection.clear();
        l.touch_geometry();
        let baseline = rustgis_map::source::versions_of(&l.dataset.features, &Default::default(), &read);
        if let Some(pg) = l.postgis.as_mut() {
            pg.versions = baseline;
            pg.writable = editable.writable;
        }
        s.notify(format!("{name}: edits discarded; {n} rows loaded again from {}", edit.origin.table), false);
        Ok(json!({"features": n, "matched": editable.matched}))
    }
}

fn status(s: &mut Session, _: &Value) -> CmdResult {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = s;
        Ok(Value::Null)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use rustgis_data::source::duckdb::Database;
        Ok(match &s.postgis {
            Some(dsn) => {
                let db = Database::Postgres(dsn.clone());
                json!({"connected": true, "connection": db.label(), "database": db.short()})
            }
            None => json!({"connected": false}),
        })
    }
}

fn disconnect(s: &mut Session, _: &Value) -> CmdResult {
    let was = s.postgis.take();
    Ok(Value::Bool(was.is_some()))
}

/// The connection the parameters give, without its password (which is remembered for the
/// session): a whole connection string, or its parts.
#[cfg(not(target_arch = "wasm32"))]
fn connection_param(p: &Value) -> Result<String, String> {
    use rustgis_data::source::duckdb::{Database, remember_password};
    let text = |k: &str| -> Option<String> {
        match p.get(k)? {
            Value::String(v) => Some(v.trim().to_string()).filter(|v| !v.is_empty()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        }
    };
    let spec = match text("connection") {
        // A bare keyword string is a PostgreSQL one (as Add From Database takes it).
        Some(c) if c.contains('=') && !c.contains("://") && !c.to_ascii_lowercase().starts_with("postgres:") => format!("postgres:{c}"),
        Some(c) => c,
        None => {
            if text("host").is_none() {
                return Err("give the host, or a whole connection string".into());
            }
            if let Some(m) = text("sslmode")
                && !SSL_MODES.contains(&m.to_ascii_lowercase().as_str())
            {
                return Err(format!("sslmode '{}': one of {}", clip(&m), SSL_MODES.join(", ")));
            }
            let quote = |v: &str| format!("'{}'", v.replace('\\', "\\\\").replace('\'', "\\'"));
            let parts: Vec<String> = [("host", "host"), ("port", "port"), ("dbname", "database"), ("user", "user"), ("sslmode", "sslmode")]
                .iter()
                .filter_map(|(key, param)| text(param).map(|v| format!("{key}={}", quote(&v))))
                .collect();
            format!("postgres:{}", parts.join(" "))
        }
    };
    let Database::Postgres(dsn) = Database::parse(&spec) else {
        return Err("not a PostgreSQL connection: host=… dbname=… user=…, or a postgresql:// address".into());
    };
    if let Some(pw) = ps(p, "password").filter(|pw| !pw.is_empty()) {
        remember_password(&dsn, pw);
    }
    Ok(dsn)
}

/// Text from a parameter, shortened for a message.
#[cfg(not(target_arch = "wasm32"))]
fn clip(s: &str) -> String {
    s.chars().take(40).collect()
}

fn connect(s: &mut Session, p: &Value) -> CmdResult {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (s, p);
        Err("PostGIS databases are read in the desktop app".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use rustgis_data::source::{duckdb::Database, postgis};
        let dsn = connection_param(p)?;
        let db = Database::Postgres(dsn.clone());
        let label = db.short();
        let conn = postgis::connect(&dsn, true).map_err(|e| format!("{label}: {e}"))?;
        // Listing the tables checks the database has PostGIS.
        let tables = postgis::tables(&conn).map_err(|e| format!("{label}: {e}"))?;
        s.postgis = Some(dsn);
        s.notify(format!("Connected to {label}: {} tables with geometry", tables.len()), false);
        Ok(json!({"connected": true, "connection": db.label(), "database": label, "tables": tables.len()}))
    }
}

fn tables(s: &mut Session, p: &Value) -> CmdResult {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (s, p);
        Err("PostGIS databases are read in the desktop app".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use rustgis_data::source::{duckdb::Database, postgis};
        let dsn = s.postgis.clone().ok_or(NOT_CONNECTED)?;
        let label = Database::Postgres(dsn.clone()).short();
        let conn = postgis::connect(&dsn, true).map_err(|e| format!("{label}: {e}"))?;
        let all = postgis::tables(&conn).map_err(|e| format!("{label}: {e}"))?;
        let schema = ps(p, "schema").map(str::trim).filter(|v| !v.is_empty());
        let listed: Vec<Value> = all
            .iter()
            .filter(|t| schema.is_none_or(|v| t.schema.eq_ignore_ascii_case(v)))
            .take(MAX_LISTED)
            .map(|t| {
                json!({"name": t.label(), "schema": t.schema, "table": t.name, "column": t.column, "type": t.kind, "srid": t.srid, "geography": t.geography})
            })
            .collect();
        Ok(json!({"database": label, "tables": listed}))
    }
}

/// Adds a PostGIS table as a layer (one per geometry family when it mixes them), framed on a map
/// that shows nothing yet.
fn add_layer(s: &mut Session, p: &Value) -> CmdResult {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (s, p);
        Err("PostGIS databases are read in the desktop app".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use rustgis_data::source::{FeatureSource, duckdb::Database, postgis};
        use rustgis_map::source::{PostgisEdit, PostgisOrigin};
        let dsn = s.postgis.clone().ok_or(NOT_CONNECTED)?;
        let name = ps(p, "table").map(str::trim).filter(|t| !t.is_empty()).ok_or("table is required")?;
        let db = Database::Postgres(dsn.clone());
        let label = db.short();
        let err = |e: rustgis_data::DataError| format!("{label}: {e}");
        let crs = ps(p, "crs").map(str::trim).filter(|c| !c.is_empty());
        let condition = ps(p, "where");
        let limit = f(p, "limit").map_or(postgis::MAX_EDITABLE, |n| n.clamp(1.0, postgis::MAX_EDITABLE as f64) as usize);
        let conn = postgis::connect(&dsn, true).map_err(err)?;
        let table = postgis::find_table(&conn, name).map_err(err)?;
        let families = postgis::families(&conn, &table).map_err(err)?;
        if families.is_empty() {
            return Err(format!("{label}: {}: the table has no rows with a geometry", table.label()));
        }
        let mixed = families.len() > 1;
        let empty_before = super::map_is_empty(s);
        let (mut ids, mut loaded, mut matched, mut writable) = (Vec::new(), 0u64, 0u64, true);
        for family in families {
            let src = postgis::PostgisSource::open(conn.clone(), table.clone(), Some(family), crs).map_err(err)?;
            let key = src.key().map(str::to_string);
            let editable = src.read_editable(condition, limit).map_err(err)?;
            let layer_name = match ps(p, "name").map(str::trim).filter(|n| !n.is_empty() && !mixed) {
                Some(n) => n.to_string(),
                None if mixed => format!("{} ({})", table.label(), family.label()),
                None => table.label(),
            };
            let info = src.info();
            let mut ds = rustgis_data::Dataset::new(layer_name.clone(), info.geometry_type, info.fields.clone());
            ds.crs = u32::try_from(src.srid()).ok().filter(|&c| c != 0 && c != 4326).and_then(rustgis_data::raster::Crs::from_epsg);
            let n = editable.features.len() as u64;
            let read: std::collections::HashMap<u64, i64> = editable.versions.iter().copied().collect();
            let versions = rustgis_map::source::versions_of(&editable.features, &Default::default(), &read);
            ds.features = editable.features.into();
            if n < editable.matched {
                s.notify(
                    format!(
                        "{layer_name}: loaded {n} of {} rows; narrow them with a condition (where) to edit the others",
                        editable.matched
                    ),
                    false,
                );
            }
            if !editable.writable {
                s.notify(format!("{layer_name}: this user can't insert, update or delete its rows, so edits can't be saved to it"), false);
            }
            let id = super::layer::add_dataset(s, ds);
            if let Some(l) = s.map_mut().layer_mut(id) {
                l.postgis = Some(PostgisEdit {
                    origin: PostgisOrigin {
                        database: db.label(),
                        table: table.label(),
                        family: Some(family),
                        crs: crs.map(str::to_string),
                    },
                    key: key.unwrap_or_default(),
                    srid: src.srid(),
                    writable: editable.writable,
                    condition: condition.map(str::trim).filter(|c| !c.is_empty()).map(str::to_string),
                    limit,
                    versions,
                });
            }
            ids.push(id);
            loaded += n;
            matched += editable.matched;
            writable &= editable.writable;
        }
        if empty_before {
            let e = ids.iter().filter_map(|id| s.map().layer(*id)).fold(rustgis_geom::Extent::EMPTY, |e, l| e.union(&l.extent()));
            if !e.is_empty() {
                s.zoom_to(&e);
            }
        }
        Ok(json!({"layers": ids, "features": loaded, "matched": matched, "writable": writable}))
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use serde_json::json;

    #[test]
    fn connections_are_built_from_their_parts_without_the_password() {
        let dsn = super::connection_param(&json!({"host": "db.example", "port": 5433, "database": "my gis", "user": "o'neil", "password": "s3cret", "sslmode": "require"}))
            .unwrap_or_else(|e| panic!("{e}"));
        for part in ["host=db.example", "port=5433", "dbname='my gis'", r"user='o\'neil'", "sslmode=require"] {
            assert!(dsn.contains(part), "{part} in {dsn}");
        }
        assert!(!dsn.contains("s3cret"), "{dsn}");
        // A whole connection string wins over the parts, and loses its password.
        let dsn = super::connection_param(&json!({"connection": "host=h dbname=g password=pw", "host": "other"}))
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(dsn, "host=h dbname=g");
        let dsn = super::connection_param(&json!({"connection": "postgresql://u:pw@h:5432/g"})).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(dsn, "postgresql://u@h:5432/g");
        // What isn't a PostgreSQL connection is refused before anything is sent.
        for bad in [json!({}), json!({"host": " "}), json!({"host": "h", "sslmode": "bogus"}), json!({"connection": "mysql://u@h/db"})] {
            assert!(super::connection_param(&bad).is_err(), "{bad}");
        }
    }
}
