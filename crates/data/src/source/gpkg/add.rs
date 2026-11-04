//! Exports into an existing GeoPackage: one table added, or one of its feature or attribute
//! tables replaced, in a single transaction that leaves the file's other tables as they were.

use super::{FEATURE_METADATA, GpkgWritten, Redo, WGS84_WKT, create_table, err, fill, plan, quote, register};
use crate::raster::Crs;
use crate::{DataError, Field, GeometryType, Result};
use rusqlite::config::DbConfig;
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use std::cell::Cell;
use std::path::Path;
use std::time::Duration;

/// Writes the features `feed` hands over as table `name` of the GeoPackage at `path` (see
/// [`super::write_features_in`]). A table of that name is refused, or, with `replace`, required:
/// a feature or attribute table, dropped first; nothing else in the file is touched. Every
/// change is one transaction, so a failed export leaves the file as it was.
pub(super) fn write_features(
    path: &Path,
    name: &str,
    geometry_type: GeometryType,
    fields: &[Field],
    crs: Crs,
    replace: bool,
    feed: &mut dyn FnMut(&mut dyn FnMut(&[crate::Feature]) -> Result<()>) -> Result<()>,
) -> Result<GpkgWritten> {
    let file = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    // Never made here: the file must exist, as checked before.
    let mut conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX).map_err(err)?;
    // The file may be hostile: what it defines (triggers, views) may not call functions, nor write
    // to SQLite's own records.
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true).map_err(err)?;
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false).map_err(err)?;
    // Nor may its triggers run when the export writes its GeoPackage records: one could change
    // other tables. The new table's own triggers are made after its rows are in.
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER, false).map_err(err)?;
    // Nor may its foreign keys turn the export's deletes (of a replaced table's GeoPackage
    // records) into deletes elsewhere; a table referring to the replaced one is refused instead.
    conn.execute_batch("PRAGMA foreign_keys = OFF").map_err(err)?;
    conn.busy_timeout(Duration::from_secs(5)).map_err(err)?;
    let (mut multi_points, mut keep_ids) = (None, true);
    // Each choice changes at most once; an export starting again rolls back what it wrote.
    for _ in 0..3 {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(err)?;
        check_geopackage(&tx, &file)?;
        // An attribute table has no spatial reference system.
        let srs_id = if geometry_type == GeometryType::None { 0 } else { srs_id(&tx, crs)? };
        // The views and triggers naming a replaced table, when one is.
        let replaced = Cell::new(None);
        let mut make = |multi| {
            let layout = plan(name, geometry_type, fields, multi, srs_id);
            replaced.set(make_room(&tx, &file, &layout.table, replace)?);
            if layout.has_geom {
                tx.execute_batch(FEATURE_METADATA).map_err(err)?;
                let index = format!("rtree_{}_{}", layout.table, layout.geom);
                if let Some(kind) = object(&tx, &index)? {
                    return Err(DataError::Format(format!(
                        "{file} already has {} named {index}, which the table's spatial index needs; choose another table name",
                        with_article(&kind)
                    )));
                }
            }
            create_table(&tx, &layout)?;
            Ok(layout)
        };
        match fill(&tx, multi_points, keep_ids, &mut make, feed)? {
            Err(Redo::MultiPoints) => multi_points = Some(true),
            Err(Redo::Renumber) => keep_ids = false,
            Ok((layout, extent)) => {
                register(&tx, &layout, extent)?;
                // A cancel that came while it was written rolls it all back; the last look, as
                // committing writes the table, and a Cancel after it finds it written.
                crate::progress::check()?;
                tx.commit().map_err(err)?;
                let dependents = replaced.take();
                return Ok(GpkgWritten { table: layout.table, replaced: dependents.is_some(), dependents: dependents.unwrap_or_default() });
            }
        }
    }
    Err(DataError::Format("GeoPackage: the export didn't settle on a table".into()))
}

/// Refuses a database without a GeoPackage's contents and spatial reference systems, or marked
/// as another application's (an unmarked one, as older programs write, is taken).
fn check_geopackage(conn: &Connection, file: &str) -> Result<()> {
    // "GPKG", and GeoPackage 1.0's and 1.1's "GP10" and "GP11".
    let id: i64 = conn.query_row("PRAGMA application_id", [], |r| r.get(0)).map_err(err)?;
    if !matches!(id, 0 | 0x4750_4B47 | 0x4750_3130 | 0x4750_3131) {
        return Err(DataError::Format(format!("{file}: not a GeoPackage (its application id is {id:#010x})")));
    }
    for table in ["gpkg_contents", "gpkg_spatial_ref_sys"] {
        if object(conn, table)?.as_deref() != Some("table") {
            return Err(DataError::Format(format!("{file}: not a GeoPackage (it has no {table} table)")));
        }
    }
    Ok(())
}

/// What kind of SQLite object (`table`, `view`, `index`, `trigger`) is named `name`, without
/// regard to case as SQLite names are.
fn object(conn: &Connection, name: &str) -> Result<Option<String>> {
    conn.query_row("SELECT type FROM sqlite_master WHERE lower(name) = lower(?1) LIMIT 1", [name], |r| r.get(0)).optional().map_err(err)
}

/// The GeoPackage's id for `crs`: an entry it has for the EPSG code, or with the same definition
/// for a system without one; else one added, numbered by its EPSG code where that is free and
/// otherwise from 100000 up, as a new file's custom system is.
fn srs_id(conn: &Connection, crs: Crs) -> Result<i32> {
    let epsg = crs.epsg();
    let wkt = if epsg == 4326 { Some(WGS84_WKT.to_string()) } else { crate::raster::wkt::to_wkt(crs) };
    let found: Option<i64> = if epsg > 0 {
        conn.query_row(
            "SELECT srs_id FROM gpkg_spatial_ref_sys WHERE upper(organization) = 'EPSG' AND organization_coordsys_id = ?1 ORDER BY srs_id LIMIT 1",
            [i64::from(epsg)],
            |r| r.get(0),
        )
        .optional()
        .map_err(err)?
    } else {
        // A system without a WKT form is found by its name.
        let (sql, key) = match &wkt {
            Some(wkt) => ("SELECT srs_id FROM gpkg_spatial_ref_sys WHERE definition = ?1 ORDER BY srs_id LIMIT 1", wkt.clone()),
            None => (
                "SELECT srs_id FROM gpkg_spatial_ref_sys WHERE srs_name = ?1 AND definition = 'undefined' AND srs_id >= 100000 ORDER BY srs_id LIMIT 1",
                crs.label(),
            ),
        };
        conn.query_row(sql, [key], |r| r.get(0)).optional().map_err(err)?
    };
    let id = match found {
        Some(id) => id,
        None => {
            let preferred = if epsg > 0 { i64::from(epsg) } else { 100_000 };
            let taken: bool = conn
                .query_row("SELECT EXISTS (SELECT 1 FROM gpkg_spatial_ref_sys WHERE srs_id = ?1)", [preferred], |r| r.get(0))
                .map_err(err)?;
            let id = if taken {
                conn.query_row("SELECT max(100000, max(srs_id) + 1) FROM gpkg_spatial_ref_sys", [], |r| r.get(0)).map_err(err)?
            } else {
                preferred
            };
            let (org, code) = if epsg > 0 { ("EPSG", i64::from(epsg)) } else { ("NONE", id) };
            conn.execute(
                "INSERT INTO gpkg_spatial_ref_sys (srs_name, srs_id, organization, organization_coordsys_id, definition, description)
                    VALUES (?1, ?2, ?3, ?4, ?5, '')",
                rusqlite::params![crs.label(), id, org, code, wkt.as_deref().unwrap_or("undefined")],
            )
            .map_err(err)?;
            id
        }
    };
    i32::try_from(id).map_err(|_| DataError::Format(format!("GeoPackage: spatial reference system id {id} is out of range")))
}

/// Clears the way for table `table`: nothing of that name, or (with `replace`, which needs one) a
/// feature or attribute table of it, dropped with its spatial index and its GeoPackage records.
/// For a table replaced, returns the views and other tables' triggers whose SQL names it (they
/// read the new table, and break if a column they use is gone); anything else of the name is
/// refused.
fn make_room(conn: &Connection, file: &str, table: &str, replace: bool) -> Result<Option<Vec<String>>> {
    let registered: Option<(String, String)> = conn
        .query_row("SELECT table_name, data_type FROM gpkg_contents WHERE lower(table_name) = lower(?1)", [table], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()
        .map_err(err)?;
    let kind = object(conn, table)?;
    match (kind.as_deref(), registered) {
        // Replacing a table not there is most likely a misspelt name: the stale table would stay.
        (None, None) if replace => {
            Err(DataError::Format(format!("{file} has no table {table} to replace; check the name, or add the layer as a new table")))
        }
        (None, None) => Ok(None),
        (None, Some((old, _))) => {
            Err(DataError::Format(format!("{file} lists a table {old} in its contents that it doesn't have; choose another table name")))
        }
        (Some("table"), Some((old, data_type))) if matches!(data_type.as_str(), "features" | "attributes") => {
            if !replace {
                return Err(DataError::Format(format!(
                    "{file} already has a table {old}: choose another table name, or replace that table"
                )));
            }
            let children = referencing(conn, &old)?;
            if !children.is_empty() {
                return Err(DataError::Format(format!(
                    "{file}: {} refer to {old} by foreign key, whose rows a replaced table would delete or break; choose another table name",
                    children.join(", ")
                )));
            }
            if related(conn, &old)? {
                return Err(DataError::Format(format!(
                    "{file}: {old} is in a related-tables relation, whose links a replaced table would break; choose another table name"
                )));
            }
            let dependents = dependents(conn, &old)?;
            drop_table(conn, &old)?;
            Ok(Some(dependents))
        }
        (_, Some((old, data_type))) => Err(DataError::Format(format!(
            "{file}: {old} is a {data_type} table, which an export doesn't replace; choose another table name"
        ))),
        (Some(kind), None) => Err(DataError::Format(format!(
            "{file} already has {} named {table}, which an export doesn't replace; choose another table name",
            with_article(kind)
        ))),
    }
}

/// The other tables with a foreign key to `table`. Dropping it would delete or empty their rows
/// (SQLite enforces foreign keys here), or leave them pointing at rows that aren't the same.
fn referencing(conn: &Connection, table: &str) -> Result<Vec<String>> {
    let mut st = conn
        .prepare(
            "SELECT DISTINCT m.name FROM sqlite_master m, pragma_foreign_key_list(m.name) f
                WHERE m.type = 'table' AND lower(m.name) != lower(?1) AND lower(f.\"table\") = lower(?1) ORDER BY m.name",
        )
        .map_err(err)?;
    let names = st.query_map([table], |r| r.get::<_, String>(0)).map_err(err)?;
    names.collect::<rusqlite::Result<Vec<_>>>().map_err(err)
}

/// Whether the GeoPackage's related-tables extension names `table` as a base, related or mapping
/// table: its rows are linked by id, which a new table's rows don't keep.
fn related(conn: &Connection, table: &str) -> Result<bool> {
    if object(conn, "gpkgext_relations")?.as_deref() != Some("table") {
        return Ok(false);
    }
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM gpkgext_relations WHERE lower(?1) IN
            (lower(base_table_name), lower(related_table_name), lower(mapping_table_name)))",
        [table],
        |r| r.get(0),
    )
    .map_err(err)
}

/// The views, and triggers on other tables, whose SQL mentions `table` (by a case-blind search,
/// so a longer name containing it counts too), by name.
fn dependents(conn: &Connection, table: &str) -> Result<Vec<String>> {
    let mut st = conn
        .prepare(
            "SELECT name FROM sqlite_master WHERE type IN ('view', 'trigger') AND lower(tbl_name) != lower(?1)
                AND instr(lower(sql), lower(?1)) > 0 ORDER BY name",
        )
        .map_err(err)?;
    let names = st.query_map([table], |r| r.get::<_, String>(0)).map_err(err)?;
    names.collect::<rusqlite::Result<Vec<_>>>().map_err(err)
}

/// An SQLite object's kind with its article: "an index", "a view".
fn with_article(kind: &str) -> String {
    if kind.starts_with(['a', 'e', 'i', 'o', 'u']) { format!("an {kind}") } else { format!("a {kind}") }
}

/// Drops feature or attribute table `table`, its spatial indexes, and its rows in the GeoPackage
/// tables that describe it (the optional ones where the file has them). A table named as its
/// spatial index that isn't an R-tree is left, and the new table's index then refused.
fn drop_table(conn: &Connection, table: &str) -> Result<()> {
    let has = |t: &str| object(conn, t).map(|k| k.as_deref() == Some("table"));
    let mut sql = String::new();
    if has("gpkg_geometry_columns")? {
        let mut st = conn.prepare("SELECT column_name FROM gpkg_geometry_columns WHERE lower(table_name) = lower(?1)").map_err(err)?;
        let columns = st.query_map([table], |r| r.get::<_, String>(0)).map_err(err)?.collect::<rusqlite::Result<Vec<_>>>().map_err(err)?;
        for c in columns {
            let index = format!("rtree_{table}_{c}");
            let rtree: bool = conn
                .query_row(
                    // Its declaration without spaces, line breaks and quotes of any kind, so `USING "rtree" (`
                    // counts and `USING rtree_i32(` doesn't.
                    "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND lower(name) = lower(?1)
                        AND replace(replace(replace(replace(replace(replace(replace(replace(lower(sql), ' ', ''), char(9), ''), char(10), ''), char(13), ''), '\"', ''), '`', ''), '[', ''), ']', '')
                            LIKE 'createvirtualtable%usingrtree(%')",
                    [&index],
                    |r| r.get(0),
                )
                .map_err(err)?;
            if rtree {
                sql.push_str(&format!("DROP TABLE {};", quote(&index)));
            }
        }
    }
    sql.push_str(&format!("DROP TABLE {};", quote(table)));
    conn.execute_batch(&sql).map_err(err)?;
    // `gpkg_ogr_contents` is the feature count GDAL keeps, which would otherwise outlive the table.
    for records in
        ["gpkg_geometry_columns", "gpkg_extensions", "gpkg_data_columns", "gpkg_metadata_reference", "gpkg_ogr_contents", "gpkg_contents"]
    {
        if has(records)? {
            conn.execute(&format!("DELETE FROM {records} WHERE lower(table_name) = lower(?1)"), [table]).map_err(err)?;
        }
    }
    Ok(())
}
