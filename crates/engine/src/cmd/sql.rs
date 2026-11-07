//! The SQL workspace's commands: a query's rows without adding a layer, and a database's tables
//! and columns (for completing names). Queries run through DuckDB (`layer.addSql` adds a result to
//! the map).

use serde_json::Value;
#[cfg(not(target_arch = "wasm32"))]
use serde_json::json;

#[cfg(not(target_arch = "wasm32"))]
use super::f;
use crate::{CmdResult, CommandSpec as C, Session};

pub fn commands() -> Vec<C> {
    vec![
        C::new("sql.query", "Run SQL Query", "Analyze › SQL", query).roles(&[crate::ParamRole { key: "sql", role: crate::Role::Sql }, crate::ParamRole { key: "query", role: crate::Role::Sql }]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network).with(crate::policy::Effect::Files)).view().params(
            r#"{"sql": "one DuckDB SELECT (DuckDB Spatial's ST_ functions, read_parquet('…'), web addresses)", "database"?: "a .duckdb file or a PostgreSQL connection (postgres:host=… dbname=…) whose tables the query names", "password"?: "the connection's, kept for this session only", "limit"?: "rows returned, up to 1000 (default 100)"}"#,
        ),
        C::new("sql.schema", "List Database Columns", "Analyze › SQL", schema).roles(&[crate::ParamRole { key: "sql", role: crate::Role::Sql }, crate::ParamRole { key: "query", role: crate::Role::Sql }]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network).with(crate::policy::Effect::Files))
            .view()
            .params(r#"{"database": "a .duckdb file or a PostgreSQL connection", "password"?: string}"#),
    ]
}

/// Runs a query and returns its first rows: `{columns: [{name, type}], rows, geometry, more}`.
fn query(_s: &mut Session, p: &Value) -> CmdResult {
    let sql = super::ps(p, "sql").ok_or("sql is required")?;
    #[cfg(target_arch = "wasm32")]
    {
        let _ = sql;
        Err("SQL queries run through DuckDB in the desktop app".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use rustgis_data::source::duckdb;
        let database = super::layer::sql_database_param(p)?;
        let limit = f(p, "limit").filter(|l| l.is_finite()).map(|l| l.clamp(1.0, duckdb::MAX_PREVIEW as f64) as usize).unwrap_or(100);
        let r = duckdb::preview(&duckdb::Query { sql, database: database.as_ref(), crs: None }, limit).map_err(|e| e.to_string())?;
        Ok(preview_json(&r))
    }
}

/// A preview as the command returns it.
#[cfg(not(target_arch = "wasm32"))]
pub fn preview_json(r: &rustgis_data::source::duckdb::Preview) -> Value {
    json!({
        "columns": r.columns.iter().map(|(n, t)| json!({"name": n, "type": t})).collect::<Vec<_>>(),
        "rows": r.rows,
        "geometry": r.geometry,
        "more": r.more,
    })
}

/// A database's tables and their columns: `{tables: {name: [columns]}}`.
fn schema(_s: &mut Session, p: &Value) -> CmdResult {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = p;
        Err("databases are read through DuckDB in the desktop app".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let db = super::layer::sql_database_param(p)?.ok_or("database is required")?;
        let cols = rustgis_data::source::duckdb::schema_of(&db).map_err(|e| format!("{}: {e}", db.short()))?;
        let mut tables = serde_json::Map::new();
        for (t, c) in cols {
            if let Some(a) = tables.entry(t).or_insert_with(|| json!([])).as_array_mut() {
                a.push(json!(c));
            }
        }
        Ok(json!({"tables": tables}))
    }
}
