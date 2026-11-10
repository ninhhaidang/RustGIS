//! Whole-table editing of an existing GeoPackage, with optimistic row versions and atomic saves.

use super::{CHUNK, GpkgSource, err, header_blob, is_empty_blob, quote, split_blob, sql_value, value};
use crate::raster::Crs;
use crate::source::SourceInfo;
use crate::source::edits::{Conflict, Edits, Saved};
use crate::{DataError, Feature, Field, Geometry, Result, Value, wkb};
use rusqlite::config::DbConfig;
use rusqlite::functions::FunctionFlags;
use rusqlite::types::{Value as Sql, ValueRef};
use rusqlite::{Connection, OpenFlags, TransactionBehavior, params, params_from_iter};
use rustgis_geom::Extent;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::Duration;

/// A whole-table editing session is bounded, unlike a streamed map source.
pub const MAX_EDITABLE: usize = 100_000;

/// Rows loaded for editing, including geometry-less rows but excluding unreadable geometries.
pub struct Editable {
    pub features: Vec<Feature>,
    pub versions: Vec<(u64, i64)>,
    /// The durable table marker from the same read transaction as the rows.
    pub revision: String,
    pub matched: u64,
    pub skipped: u64,
    pub writable: bool,
}

/// Stored row versions and snapshots, read in the write transaction before it commits.
#[derive(Debug)]
pub struct SavedRows {
    pub changes: Saved,
    /// Requested snapshots, followed by final snapshots of trigger-affected resident keys.
    /// Attributes follow supplied fields; absent columns retain submitted values when available.
    pub features: Vec<Feature>,
    /// Affected known resident keys absent after triggers, in unsigned key order.
    pub removed: Vec<u64>,
    /// Supplied field positions that have no stored column.
    pub unstored_fields: Vec<usize>,
    pub revision: String,
}

/// An existing feature table; its read connection is never reused for writes.
pub struct EditTable {
    src: GpkgSource,
    path: PathBuf,
    columns: Vec<String>,
    geometry_type: String,
    srs_id: i64,
    z: i64,
    m: i64,
}

impl EditTable {
    pub fn open(path: &Path, table: &str) -> Result<Self> {
        let src = GpkgSource::open(path, Some(table))?;
        let (columns, geometry_type, srs_id, z, m) = {
            let guard = src.conn.lock().map_err(|_| DataError::Format("GeoPackage connection poisoned".into()))?;
            let conn = guard.as_ref().ok_or_else(|| DataError::Format("GeoPackage connection closed".into()))?;
            let cols = table_columns(conn, &src.table)?;
            let mut keys = cols.iter().filter(|c| c.pk > 0);
            let key = keys.next();
            if keys.next().is_some()
                || !key.is_some_and(|c| c.ty.trim().eq_ignore_ascii_case("INTEGER") && src.pk.as_deref() == Some(c.name.as_str()))
            {
                return Err(DataError::Format(format!("{table}: no INTEGER PRIMARY KEY, so its rows can't be told apart to save")));
            }
            let (ty, srs, z, m) = conn
                .query_row("SELECT geometry_type_name, srs_id, z, m FROM gpkg_geometry_columns WHERE table_name = ?1", [&src.table], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?))
                })
                .map_err(err)?;
            (cols.into_iter().map(|c| c.name).collect(), ty.to_ascii_uppercase(), srs, z, m)
        };
        Ok(Self { src, path: path.to_path_buf(), columns, geometry_type, srs_id, z, m })
    }

    pub fn info(&self) -> &SourceInfo {
        &self.src.info
    }

    pub fn crs(&self) -> Option<Crs> {
        (!self.src.reproject.is_lonlat()).then_some(self.src.reproject.crs)
    }

    pub fn table(&self) -> &str {
        &self.src.table
    }

    pub fn key(&self) -> &str {
        self.src.pk.as_deref().unwrap_or("")
    }

    pub fn geometry_column(&self) -> &str {
        &self.src.geom
    }

    pub fn srs_id(&self) -> i64 {
        self.srs_id
    }

    /// The declared geometry type (uppercase), Z and M flags as read when opened.
    pub fn declaration(&self) -> (&str, i64, i64) {
        (&self.geometry_type, self.z, self.m)
    }

    pub fn read_editable(&self, limit: usize) -> Result<Editable> {
        let guard = self.src.conn.lock().map_err(|_| DataError::Format("GeoPackage connection poisoned".into()))?;
        let conn = guard.as_ref().ok_or_else(|| DataError::Format("GeoPackage connection closed".into()))?;
        // Count and baseline come from the same snapshot, even if another process is writing.
        let tx = conn.unchecked_transaction().map_err(err)?;
        let matched = tx.query_row(&format!("SELECT COUNT(*) FROM {}", quote(self.table())), [], |r| r.get::<_, i64>(0)).map_err(err)?;
        let matched = u64::try_from(matched).map_err(|_| DataError::Format("GeoPackage: negative row count".into()))?;
        let (key, geom) = self.positions(&self.columns)?;
        let attrs = self
            .info()
            .fields
            .iter()
            .map(|f| self.columns.iter().position(|c| c.eq_ignore_ascii_case(&f.name)).ok_or_else(|| self.shape_error()))
            .collect::<Result<Vec<_>>>()?;
        let mut out = Editable {
            features: Vec::new(),
            versions: Vec::new(),
            revision: self.revision(&tx)?,
            matched,
            skipped: 0,
            writable: std::fs::metadata(&self.path).is_ok_and(|m| !m.permissions().readonly()),
        };
        {
            let mut st = tx
                .prepare(&format!(
                    "SELECT {} FROM {} ORDER BY {} LIMIT ?1",
                    select_list(&self.columns),
                    quote(self.table()),
                    quote(self.key())
                ))
                .map_err(err)?;
            let mut rows = st.query([limit.clamp(1, MAX_EDITABLE) as i64]).map_err(err)?;
            while let Some(row) = rows.next().map_err(err)? {
                let id = row.get::<_, i64>(key).map_err(err)?.cast_unsigned();
                let Some(geometry) = self.geometry(row.get_ref(geom).map_err(err)?) else {
                    out.skipped += 1;
                    continue;
                };
                let mut hash = FNV_OFFSET;
                for i in 0..self.columns.len() {
                    hash_value(&mut hash, row.get_ref(i).map_err(err)?);
                }
                let attrs = self
                    .info()
                    .fields
                    .iter()
                    .zip(&attrs)
                    .map(|(f, &i)| row.get_ref(i).map(|v| value(v, f.ty)))
                    .collect::<rusqlite::Result<_>>()
                    .map_err(err)?;
                out.versions.push((id, hash.cast_signed()));
                out.features.push(Feature { id, geometry, attrs });
            }
        }
        tx.commit().map_err(err)?;
        Ok(out)
    }

    /// Returns conflicts without changing anything; every other failure also rolls back the save.
    /// `reserved` must include every loaded baseline key, including rows not changed by this save.
    /// Pending inserts also require `expected_revision` to match the durable loaded table marker.
    /// New positive keys exceed those keys, the table's current maximum and its AUTOINCREMENT
    /// history. Stored snapshots and their versions are decoded together before commit; an
    /// unreadable or trigger-removed row aborts the whole transaction.
    pub fn save(
        &self,
        edits: &Edits,
        fields: &[Field],
        reserved: impl Iterator<Item = u64>,
        expected_revision: &str,
    ) -> Result<std::result::Result<SavedRows, Vec<Conflict>>> {
        let mut conn =
            Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX).map_err(err)?;
        conn.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true).map_err(err)?;
        conn.set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false).map_err(err)?;
        conn.busy_timeout(Duration::from_secs(5)).map_err(err)?;
        register_functions(&conn)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(err)?;
        let revision = self.revision(&tx)?;
        if !edits.inserts.is_empty() && revision != expected_revision {
            return Err(DataError::Format(format!(
                "{}: the source table changed since it was loaded; refusing stale inserts, so nothing was saved. Discard Edits loads it again",
                self.table()
            )));
        }
        let (geometry_type, srs_id, z, m) = tx
            .query_row(
                "SELECT geometry_type_name, srs_id, z, m FROM gpkg_geometry_columns WHERE lower(table_name) = lower(?1)",
                [self.table()],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?)),
            )
            .map_err(err)?;
        if srs_id != self.srs_id {
            return Err(DataError::Format(format!(
                "{}: its SRS changed since it was loaded, so nothing was saved. Discard Edits loads it again",
                self.table()
            )));
        }
        if !geometry_type.eq_ignore_ascii_case(&self.geometry_type) || z != self.z || m != self.m {
            return Err(self.shape_error());
        }
        let columns: Vec<String> = table_columns(&tx, self.table())?.into_iter().map(|c| c.name).collect();
        let (key, geom) = self.positions(&columns)?;
        let residents: HashMap<i64, bool> = reserved
            .map(u64::cast_signed)
            .chain(edits.updates.iter().map(|(f, _)| f.id.cast_signed()))
            .chain(edits.deletes.iter().map(|(k, _)| k.cast_signed()))
            .map(|k| (k, false))
            .collect();
        let reserved_high_water = if edits.inserts.is_empty() { 0 } else { residents.keys().copied().max().unwrap_or(0).max(0) };
        // Residents already gone (deleted elsewhere since loading) aren't evicted by this save.
        let absent_before = self.missing_keys(&tx, &residents.keys().copied().collect::<Vec<_>>())?;
        let feature_writes = watch_feature_writes(&tx, self.table(), self.key(), residents)?;
        let mut direct_changes = 0_u64;
        let mut saved = Saved::default();
        let mut targets = Vec::new();
        let mut snapshot_columns = Vec::new();
        let mut unstored_fields = Vec::new();
        for (i, f) in fields.iter().enumerate() {
            if let Some(c) = columns.iter().position(|c| c.eq_ignore_ascii_case(&f.name)) {
                snapshot_columns.push((i, c));
                if c != key && c != geom {
                    targets.push((i, c));
                } else {
                    saved.skipped_fields.push(f.name.clone());
                }
            } else {
                saved.skipped_fields.push(f.name.clone());
                unstored_fields.push(i);
            }
        }
        let keys: Vec<i64> =
            edits.updates.iter().map(|(f, _)| f.id.cast_signed()).chain(edits.deletes.iter().map(|(k, _)| k.cast_signed())).collect();
        let current = self.fetch_rows(&tx, &columns, key, &keys)?;
        let mut conflicts = Vec::new();
        for (k, version) in edits.updates.iter().map(|(f, v)| (f.id, *v)).chain(edits.deletes.iter().copied()) {
            match current.get(&k.cast_signed()) {
                None => conflicts.push(Conflict::Deleted(k)),
                Some(row) if fingerprint(row.iter().map(ValueRef::from)) != version => conflicts.push(Conflict::Changed(k)),
                Some(_) => {}
            }
        }
        if !conflicts.is_empty() {
            return Ok(Err(conflicts));
        }

        let table = quote(self.table());
        let pk = quote(self.key());
        let mut spatial_changed = !edits.inserts.is_empty() || !edits.deletes.is_empty();
        for (f, _) in &edits.updates {
            let row = current.get(&f.id.cast_signed()).ok_or_else(|| self.shape_error())?;
            let mut sets = Vec::new();
            let mut args = Vec::new();
            for &(i, c) in &targets {
                let field = fields.get(i).ok_or_else(|| self.shape_error())?;
                let old = row.get(c).ok_or_else(|| self.shape_error())?;
                let attr = f.attrs.get(i).unwrap_or(&Value::Null);
                let previous = value(ValueRef::from(old), field.ty);
                let same_nan = matches!((&previous, attr), (Value::Float(a), Value::Float(b)) if a.is_nan() && b.is_nan());
                if previous != *attr && !same_nan {
                    sets.push(format!("{} = ?", quote(columns.get(c).ok_or_else(|| self.shape_error())?)));
                    args.push(sql_value(attr, field.ty));
                }
            }
            let old = ValueRef::from(row.get(geom).ok_or_else(|| self.shape_error())?);
            let geometry_changed = self.geometry(old).ok_or_else(|| self.shape_error())? != f.geometry;
            spatial_changed |= geometry_changed;
            if geometry_changed {
                let blob = self.geometry_blob(f.geometry.as_ref(), Some((f.id, old)))?;
                sets.push(format!("{} = ?", quote(self.geometry_column())));
                args.push(blob);
            }
            if sets.is_empty() {
                saved.unchanged += 1;
                continue;
            }
            args.push(Sql::Integer(f.id.cast_signed()));
            direct_changes +=
                tx.execute(&format!("UPDATE {table} SET {} WHERE {pk} = ?", sets.join(", ")), params_from_iter(args)).map_err(err)? as u64;
            saved.updated += 1;
        }
        drop(current);
        // Reserve the loaded baseline as well as live and historical keys. A deleted source row
        // can still be present in the layer, so SQLite's ordinary rowid allocation is not enough.
        let mut high_water = 0;
        if !edits.inserts.is_empty() {
            high_water = reserved_high_water;
            let current_max: Option<i64> = tx.query_row(&format!("SELECT max({pk}) FROM {table}"), [], |r| r.get(0)).map_err(err)?;
            high_water = high_water.max(current_max.unwrap_or(0));
            let has_sequence: bool = tx
                .query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'sqlite_sequence')", [], |r| r.get(0))
                .map_err(err)?;
            if has_sequence {
                let sequence: Option<i64> = tx
                    .query_row("SELECT max(seq) FROM sqlite_sequence WHERE lower(name) = lower(?1)", [self.table()], |r| r.get(0))
                    .map_err(err)?;
                high_water = high_water.max(sequence.unwrap_or(0));
            }
        }
        for f in &edits.inserts {
            let new_key = high_water.checked_add(1).ok_or_else(|| {
                DataError::Format(format!("{}: no positive INTEGER PRIMARY KEY remains above the reserved keys", self.table()))
            })?;
            high_water = new_key;
            let mut names = vec![pk.clone()];
            let mut args = vec![Sql::Integer(new_key)];
            if let Some(g) = &f.geometry {
                let blob = self.geometry_blob(Some(g), None)?;
                names.push(quote(self.geometry_column()));
                args.push(blob);
            }
            for &(i, c) in &targets {
                let attr = f.attrs.get(i).unwrap_or(&Value::Null);
                if *attr != Value::Null {
                    let field = fields.get(i).ok_or_else(|| self.shape_error())?;
                    names.push(quote(columns.get(c).ok_or_else(|| self.shape_error())?));
                    args.push(sql_value(attr, field.ty));
                }
            }
            let sql = format!("INSERT INTO {table} ({}) VALUES ({})", names.join(", "), vec!["?"; names.len()].join(", "));
            direct_changes += tx.execute(&sql, params_from_iter(args)).map_err(err)? as u64;
            let key = new_key;
            saved.inserted.push((f.id, key.cast_unsigned()));
        }
        for (k, _) in &edits.deletes {
            let deleted = tx.execute(&format!("DELETE FROM {table} WHERE {pk} = ?1"), [k.cast_signed()]).map_err(err)?;
            direct_changes += deleted as u64;
        }
        // Spatial/metadata repair must not itself trigger more feature writes after the snapshot.
        feature_writes.allowed.store(false, Ordering::Relaxed);
        let affected = feature_writes.affected()?;
        // A conflict-REPLACE delete fires no delete trigger (recursive_triggers is off), so a
        // resident row it evicted is found only by checking each unmarked resident still exists.
        let unmarked: Vec<i64> = feature_writes.unmarked()?.into_iter().filter(|k| absent_before.binary_search(k).is_err()).collect();
        let evicted = self.missing_keys(&tx, &unmarked)?;
        let mut keys: Vec<i64> = edits
            .updates
            .iter()
            .map(|(f, _)| f.id.cast_signed())
            .chain(saved.inserted.iter().map(|(_, k)| k.cast_signed()))
            .chain(edits.deletes.iter().map(|(k, _)| k.cast_signed()))
            .collect();
        keys.sort_unstable();
        keys.dedup();
        let additional: Vec<i64> = affected.iter().copied().filter(|k| keys.binary_search(k).is_err()).collect();
        keys.extend(&additional);
        let rows = self.fetch_rows(&tx, &columns, key, &keys)?;
        let mut removed: Vec<u64> = affected.iter().filter(|k| !rows.contains_key(*k)).chain(&evicted).map(|k| k.cast_unsigned()).collect();
        removed.sort_unstable();
        removed.dedup();
        saved.deleted = edits.deletes.iter().filter(|(k, _)| !rows.contains_key(&k.cast_signed())).count();
        let mut features = Vec::with_capacity(keys.len());
        let originals = edits
            .updates
            .iter()
            .map(|(f, _)| (Some(f), f.id.cast_signed()))
            .chain(edits.inserts.iter().zip(&saved.inserted).map(|(f, (_, k))| (Some(f), k.cast_signed())))
            .chain(edits.deletes.iter().filter(|(k, _)| rows.contains_key(&k.cast_signed())).map(|(k, _)| (None, k.cast_signed())))
            .chain(additional.iter().filter(|k| rows.contains_key(*k)).map(|k| (None, *k)));
        for (original, k) in originals {
            let row = rows.get(&k).ok_or_else(|| DataError::Format(format!("row {k}: it was removed while saving")))?;
            let geometry = self
                .geometry(ValueRef::from(row.get(geom).ok_or_else(|| self.shape_error())?))
                .ok_or_else(|| DataError::Format(format!("row {k}: its stored geometry can't be read, so nothing was saved")))?;
            let mut feature = Feature { id: k.cast_unsigned(), geometry, attrs: vec![Value::Null; fields.len()] };
            if let Some(original) = original {
                for &i in &unstored_fields {
                    if let (Some(target), Some(submitted)) = (feature.attrs.get_mut(i), original.attrs.get(i)) {
                        target.clone_from(submitted);
                    }
                }
            }
            for &(i, c) in &snapshot_columns {
                let field = fields.get(i).ok_or_else(|| self.shape_error())?;
                let attr = feature.attrs.get_mut(i).ok_or_else(|| self.shape_error())?;
                *attr = value(ValueRef::from(row.get(c).ok_or_else(|| self.shape_error())?), field.ty);
            }
            saved.versions.push((k.cast_unsigned(), fingerprint(row.iter().map(ValueRef::from))));
            features.push(feature);
        }
        // Standard spatial triggers only write the R-tree. A trigger that writes feature rows
        // can affect keys outside this edit batch, so those saves need a complete spatial scan.
        let rebuild = feature_writes.count.load(Ordering::Relaxed) != direct_changes || !evicted.is_empty();
        let mut written_extent = Extent::EMPTY;
        if rebuild {
            written_extent = rebuild_spatial(&tx, self.table(), self.key(), self.geometry_column(), self.src.rtree.as_deref())?;
        } else {
            for k in keys {
                let extent = rows
                    .get(&k)
                    .map(|row| stored_extent(ValueRef::from(row.get(geom).ok_or_else(|| self.shape_error())?), k, false))
                    .transpose()?
                    .unwrap_or(Extent::EMPTY);
                maintain_rtree(&tx, self.src.rtree.as_deref(), k, &extent)?;
            }
        }
        let unknown_bounds: bool = tx
            .query_row(
                "SELECT min_x IS NULL OR min_y IS NULL OR max_x IS NULL OR max_y IS NULL
             FROM gpkg_contents WHERE lower(table_name) = lower(?1)",
                [self.table()],
                |r| r.get(0),
            )
            .map_err(err)?;
        if rebuild || spatial_changed || unknown_bounds {
            // Ordinary saves keep unreadable rows and index entries untouched, while bounds
            // fold every readable native extent once (including untouched rows).
            let extent = if rebuild { written_extent } else { readable_bounds(&tx, self.table(), self.geometry_column())? };
            let extent = (!extent.is_empty()).then_some(extent);
            tx.execute(
                "UPDATE gpkg_contents SET min_x = ?2, min_y = ?3, max_x = ?4, max_y = ?5 WHERE lower(table_name) = lower(?1)",
                params![self.table(), extent.map(|e| e.min_x), extent.map(|e| e.min_y), extent.map(|e| e.max_x), extent.map(|e| e.max_y)],
            )
            .map_err(err)?;
        }
        tx.execute(
            "UPDATE gpkg_contents SET last_change = strftime('%Y-%m-%dT%H:%M:%fZ',
             CASE WHEN julianday(last_change) >= julianday('now') THEN last_change ELSE 'now' END, '+0.001 seconds')
             WHERE lower(table_name) = lower(?1)",
            [self.table()],
        )
        .map_err(err)?;
        let revision = self.revision(&tx)?;
        tx.commit().map_err(err)?;
        Ok(Ok(SavedRows { changes: saved, features, removed, unstored_fields, revision }))
    }

    fn revision(&self, conn: &Connection) -> Result<String> {
        conn.query_row(
            "SELECT last_change FROM gpkg_contents
             WHERE lower(table_name) = lower(?1) AND julianday(last_change) IS NOT NULL",
            [self.table()],
            |r| r.get(0),
        )
        .map_err(|e| DataError::Format(format!("{}: missing or invalid GeoPackage last_change: {e}", self.table())))
    }

    fn shape_error(&self) -> DataError {
        DataError::Format(format!("{} changed shape since it was loaded; Discard Edits loads it again", self.table()))
    }

    fn positions(&self, columns: &[String]) -> Result<(usize, usize)> {
        let key = columns.iter().position(|c| c.eq_ignore_ascii_case(self.key()));
        let geom = columns.iter().position(|c| c.eq_ignore_ascii_case(self.geometry_column()));
        key.zip(geom).ok_or_else(|| self.shape_error())
    }

    // Outer None is unreadable; inner None is a valid NULL or empty geometry.
    fn geometry(&self, v: ValueRef<'_>) -> Option<Option<Geometry>> {
        match v {
            ValueRef::Null => Some(None),
            ValueRef::Blob(b) if is_empty_blob(b) => Some(None),
            ValueRef::Blob(b) => {
                let (_, body) = split_blob(b)?;
                self.src.reproject.geometry(wkb::decode(body)?).map(Some)
            }
            _ => None,
        }
    }

    fn geometry_blob(&self, g: Option<&Geometry>, row: Option<(u64, ValueRef<'_>)>) -> Result<Sql> {
        // Check before returning NULL: removing geometry must not silently drop existing Z/M.
        let has_zm = row.is_some_and(|(_, v)| match v {
            ValueRef::Blob(b) => split_blob(b).is_some_and(|(_, body)| wkb::has_zm(body)),
            _ => false,
        });
        if self.z == 1 || self.m == 1 || has_zm {
            let message = match row {
                Some((k, _)) => format!(
                    "row {}: its geometry has Z or M values, which RustGIS doesn't keep, so its geometry can't be saved (its attributes can)",
                    k.cast_signed()
                ),
                None => format!(
                    "{} needs Z or M values, which RustGIS doesn't keep, so new features with a geometry can't be saved",
                    self.table()
                ),
            };
            return Err(DataError::Format(message));
        }
        let Some(g) = g else {
            return Ok(Sql::Null);
        };
        let native = self.src.reproject.to_native(g).ok_or_else(|| {
            let id = row.map(|(k, _)| k.cast_signed().to_string()).unwrap_or_else(|| "new".into());
            DataError::Format(format!("row {id}: its geometry can't be converted to {}", self.src.reproject.label()))
        })?;
        let body = match (self.geometry_type.as_str(), &native) {
            ("POINT", Geometry::Point(_) | Geometry::MultiPoint(_))
            | ("LINESTRING", Geometry::Line(_))
            | ("POLYGON", Geometry::Polygon(_)) => wkb::encode_single(&native).ok_or_else(|| {
                DataError::Format(format!("{}'s {} column holds one part; a feature has several", self.table(), self.geometry_type))
            })?,
            ("MULTIPOINT", Geometry::Point(c)) => wkb::encode(&Geometry::MultiPoint(vec![*c])),
            ("MULTIPOINT", Geometry::MultiPoint(_)) | ("MULTILINESTRING", Geometry::Line(_)) | ("MULTIPOLYGON", Geometry::Polygon(_)) => {
                wkb::encode(&native)
            }
            ("GEOMETRY", _) => wkb::encode_single(&native).unwrap_or_else(|| wkb::encode(&native)),
            ("POINT" | "LINESTRING" | "POLYGON" | "MULTIPOINT" | "MULTILINESTRING" | "MULTIPOLYGON", _) => {
                return Err(DataError::Format(format!(
                    "{}'s {} column can't hold this {:?} geometry",
                    self.table(),
                    self.geometry_type,
                    native.kind()
                )));
            }
            // The model has no collection, curve or surface variant to preserve these types.
            _ => return Err(DataError::Format(format!("{}'s {} geometry can't be written by RustGIS", self.table(), self.geometry_type))),
        };
        let srs = i32::try_from(self.srs_id)
            .map_err(|_| DataError::Format(format!("{}: srs_id {} doesn't fit a GeoPackage geometry header", self.table(), self.srs_id)))?;
        let extent = native.extent();
        Ok(Sql::Blob(header_blob(&body, &extent, srs)))
    }

    /// Keys among `keys` with no row now, read through the key column only.
    fn missing_keys(&self, conn: &Connection, keys: &[i64]) -> Result<Vec<i64>> {
        let mut present = std::collections::HashSet::with_capacity(keys.len());
        for chunk in keys.chunks(CHUNK) {
            let pk = quote(self.key());
            let mut st = conn
                .prepare(&format!("SELECT {pk} FROM {} WHERE {pk} IN ({})", quote(self.table()), vec!["?"; chunk.len()].join(",")))
                .map_err(err)?;
            let mut rows = st.query(params_from_iter(chunk.iter())).map_err(err)?;
            while let Some(row) = rows.next().map_err(err)? {
                present.insert(row.get::<_, i64>(0).map_err(err)?);
            }
        }
        let mut missing: Vec<i64> = keys.iter().copied().filter(|k| !present.contains(k)).collect();
        missing.sort_unstable();
        Ok(missing)
    }

    fn fetch_rows(&self, conn: &Connection, columns: &[String], key: usize, keys: &[i64]) -> Result<HashMap<i64, Vec<Sql>>> {
        let mut out = HashMap::new();
        for chunk in keys.chunks(CHUNK) {
            let mut st = conn
                .prepare(&format!(
                    "SELECT {} FROM {} WHERE {} IN ({})",
                    select_list(columns),
                    quote(self.table()),
                    quote(self.key()),
                    vec!["?"; chunk.len()].join(",")
                ))
                .map_err(err)?;
            let mut rows = st.query(params_from_iter(chunk.iter())).map_err(err)?;
            while let Some(row) = rows.next().map_err(err)? {
                let k = row.get::<_, i64>(key).map_err(err)?;
                let vals = (0..columns.len()).map(|i| row.get(i)).collect::<rusqlite::Result<Vec<Sql>>>().map_err(err)?;
                out.insert(k, vals);
            }
        }
        Ok(out)
    }
}

struct FeatureWrites {
    count: AtomicU64,
    allowed: AtomicBool,
    residents: Mutex<HashMap<i64, bool>>,
}

impl FeatureWrites {
    fn affected(&self) -> Result<Vec<i64>> {
        let residents = self.residents.lock().map_err(|_| DataError::Format("GeoPackage write observer poisoned".into()))?;
        let mut keys: Vec<i64> = residents.iter().filter_map(|(&k, &changed)| changed.then_some(k)).collect();
        keys.sort_unstable();
        Ok(keys)
    }

    /// Residents no write was observed on.
    fn unmarked(&self) -> Result<Vec<i64>> {
        let residents = self.residents.lock().map_err(|_| DataError::Format("GeoPackage write observer poisoned".into()))?;
        Ok(residents.iter().filter_map(|(&k, &changed)| (!changed).then_some(k)).collect())
    }
}

fn watch_feature_writes(conn: &Connection, table: &str, key: &str, residents: HashMap<i64, bool>) -> Result<Arc<FeatureWrites>> {
    let state = Arc::new(FeatureWrites { count: AtomicU64::new(0), allowed: AtomicBool::new(true), residents: Mutex::new(residents) });
    let observed = state.clone();
    conn.create_scalar_function("rustgis_feature_write", 2, FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_INNOCUOUS, move |ctx| {
        if !observed.allowed.load(Ordering::Relaxed) {
            return Err(rusqlite::Error::UserFunctionError(Box::new(std::io::Error::other(
                "metadata or spatial-index triggers cannot change feature rows after the saved snapshot",
            ))));
        }
        observed.count.fetch_add(1, Ordering::Relaxed);
        let old = ctx.get::<Option<i64>>(0)?;
        let new = ctx.get::<Option<i64>>(1)?;
        let mut residents = observed
            .residents
            .lock()
            .map_err(|_| rusqlite::Error::UserFunctionError(Box::new(std::io::Error::other("GeoPackage write observer poisoned"))))?;
        for key in old.into_iter().chain(new) {
            if let Some(changed) = residents.get_mut(&key) {
                *changed = true;
            }
        }
        Ok(0_i64)
    })
    .map_err(err)?;
    // Connection-local observers see file-owned triggers without growing beyond resident keys.
    let table = quote(table);
    let key = quote(key);
    for (event, old, new) in [
        ("INSERT", "NULL".to_string(), format!("NEW.{key}")),
        ("UPDATE", format!("OLD.{key}"), format!("NEW.{key}")),
        ("DELETE", format!("OLD.{key}"), "NULL".to_string()),
    ] {
        conn.execute_batch(&format!(
            "CREATE TEMP TRIGGER rustgis_observe_{event} AFTER {event} ON main.{table}
             BEGIN SELECT rustgis_feature_write({old}, {new}); END;"
        ))
        .map_err(err)?;
    }
    Ok(state)
}

fn stored_extent(value: ValueRef<'_>, key: i64, strict: bool) -> Result<Extent> {
    match value {
        ValueRef::Null => Ok(Extent::EMPTY),
        ValueRef::Blob(bytes) if is_empty_blob(bytes) => Ok(Extent::EMPTY),
        value => {
            let extent = if strict {
                match value {
                    ValueRef::Blob(bytes) => split_blob(bytes)
                        .and_then(|(envelope, body)| wkb::decode(body).map(|geometry| envelope.unwrap_or_else(|| geometry.extent()))),
                    _ => None,
                }
            } else {
                native_extent(value)
            };
            extent.ok_or_else(|| DataError::Format(format!("row {key}: its stored geometry can't be read, so nothing was saved")))
        }
    }
}

fn readable_bounds(conn: &Connection, table: &str, geometry: &str) -> Result<Extent> {
    let mut statement = conn.prepare(&format!("SELECT {} FROM {}", quote(geometry), quote(table))).map_err(err)?;
    let mut rows = statement.query([]).map_err(err)?;
    let mut bounds = Extent::EMPTY;
    while let Some(row) = rows.next().map_err(err)? {
        if let Some(extent) = native_extent(row.get_ref(0).map_err(err)?) {
            bounds = bounds.union(&extent);
        }
    }
    Ok(bounds)
}

fn rebuild_spatial(conn: &Connection, table: &str, key: &str, geometry: &str, rtree: Option<&str>) -> Result<Extent> {
    let mut index = if let Some(rtree) = rtree {
        conn.execute(&format!("DELETE FROM {}", quote(rtree)), []).map_err(err)?;
        Some(conn.prepare(&format!("INSERT INTO {} VALUES (?1, ?2, ?3, ?4, ?5)", quote(rtree))).map_err(err)?)
    } else {
        None
    };
    let mut statement = conn.prepare(&format!("SELECT {}, {} FROM {}", quote(key), quote(geometry), quote(table))).map_err(err)?;
    let mut rows = statement.query([]).map_err(err)?;
    let mut bounds = Extent::EMPTY;
    while let Some(row) = rows.next().map_err(err)? {
        let key = row.get::<_, i64>(0).map_err(err)?;
        let extent = stored_extent(row.get_ref(1).map_err(err)?, key, true)?;
        if !extent.is_empty() {
            if let Some(index) = &mut index {
                index.execute(params![key, extent.min_x, extent.max_x, extent.min_y, extent.max_y]).map_err(err)?;
            }
            bounds = bounds.union(&extent);
        }
    }
    Ok(bounds)
}

struct Column {
    name: String,
    ty: String,
    pk: i64,
}

fn table_columns(conn: &Connection, table: &str) -> Result<Vec<Column>> {
    let mut st = conn.prepare(&format!("PRAGMA table_info({})", quote(table))).map_err(err)?;
    st.query_map([], |r| Ok(Column { name: r.get(1)?, ty: r.get::<_, Option<String>>(2)?.unwrap_or_default(), pk: r.get(5)? }))
        .map_err(err)?
        .collect::<rusqlite::Result<_>>()
        .map_err(err)
}

fn select_list(columns: &[String]) -> String {
    columns.iter().map(|c| quote(c)).collect::<Vec<_>>().join(", ")
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x100_0000_01b3;

fn fingerprint<'a>(vals: impl Iterator<Item = ValueRef<'a>>) -> i64 {
    let mut hash = FNV_OFFSET;
    for v in vals {
        hash_value(&mut hash, v);
    }
    hash.cast_signed()
}

fn hash_value(hash: &mut u64, v: ValueRef<'_>) {
    let mut bytes = |b: &[u8]| {
        for x in b {
            *hash = (*hash ^ u64::from(*x)).wrapping_mul(FNV_PRIME);
        }
    };
    match v {
        ValueRef::Null => bytes(&[0]),
        ValueRef::Integer(v) => {
            bytes(&[1]);
            bytes(&v.to_le_bytes());
        }
        ValueRef::Real(v) => {
            bytes(&[2]);
            bytes(&v.to_bits().to_le_bytes());
        }
        ValueRef::Text(v) => {
            bytes(&[3]);
            bytes(&(v.len() as u64).to_le_bytes());
            bytes(v);
        }
        ValueRef::Blob(v) => {
            bytes(&[4]);
            bytes(&(v.len() as u64).to_le_bytes());
            bytes(v);
        }
    }
}

fn native_extent(v: ValueRef<'_>) -> Option<Extent> {
    let ValueRef::Blob(b) = v else {
        return None;
    };
    if is_empty_blob(b) {
        return None;
    }
    let (envelope, body) = split_blob(b)?;
    envelope.or_else(|| wkb::decode(body).map(|g| g.extent())).filter(|e| !e.is_empty())
}

fn register_functions(conn: &Connection) -> Result<()> {
    let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC | FunctionFlags::SQLITE_INNOCUOUS;
    conn.create_scalar_function("ST_IsEmpty", 1, flags, |ctx| {
        Ok(match ctx.get_raw(0) {
            ValueRef::Blob(b) => Some(i64::from(is_empty_blob(b))),
            _ => None,
        })
    })
    .map_err(err)?;
    for (name, index) in [("ST_MinX", 0), ("ST_MaxX", 1), ("ST_MinY", 2), ("ST_MaxY", 3)] {
        conn.create_scalar_function(name, 1, flags, move |ctx| {
            Ok(native_extent(ctx.get_raw(0)).map(|e| match index {
                0 => e.min_x,
                1 => e.max_x,
                2 => e.min_y,
                _ => e.max_y,
            }))
        })
        .map_err(err)?;
    }
    Ok(())
}

fn maintain_rtree(conn: &Connection, rtree: Option<&str>, key: i64, extent: &Extent) -> Result<()> {
    let Some(rt) = rtree else {
        return Ok(());
    };
    if extent.is_empty() {
        conn.execute(&format!("DELETE FROM {} WHERE id = ?1", quote(rt)), [key]).map_err(err)?;
    } else {
        conn.execute(
            &format!("INSERT OR REPLACE INTO {} VALUES (?1, ?2, ?3, ?4, ?5)", quote(rt)),
            params![key, extent.min_x, extent.max_x, extent.min_y, extent.max_y],
        )
        .map_err(err)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_geom::Coord;

    const REVISION: &str = "2026-01-01T00:00:00.000Z";

    #[test]
    fn gpkg_editing_another_field_preserves_an_unchanged_nan() {
        let (_dir, path) = fixture();
        let conn = Connection::open(&path).unwrap();
        conn.execute("UPDATE roads SET lanes = 'NaN'", []).unwrap();
        let table = EditTable::open(&path, "roads").unwrap();
        let read = table.read_editable(MAX_EDITABLE).unwrap();
        let lane = table.info().fields.iter().position(|f| f.name == "lanes").unwrap();
        let name = table.info().fields.iter().position(|f| f.name == "name").unwrap();
        assert!(matches!(read.features[0].attrs[lane], Value::Float(v) if v.is_nan()));
        let mut changed = read.features[0].clone();
        changed.attrs[name] = Value::Text("edited".into());
        let edits = Edits { updates: vec![(changed, read.versions[0].1)], ..Default::default() };
        let saved = table.save(&edits, &table.info().fields, std::iter::empty(), &read.revision).unwrap().unwrap();
        assert_eq!(saved.changes.updated, 1);
        let stored: (String, String) = conn.query_row("SELECT name, lanes FROM roads", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(stored, ("edited".into(), "NaN".into()));
        // An explicit clear still writes SQL NULL.
        let read = table.read_editable(MAX_EDITABLE).unwrap();
        let mut cleared = read.features[0].clone();
        cleared.attrs[lane] = Value::Null;
        let edits = Edits { updates: vec![(cleared, read.versions[0].1)], ..Default::default() };
        table.save(&edits, &table.info().fields, std::iter::empty(), &saved.revision).unwrap().unwrap();
        assert!(conn.query_row("SELECT lanes IS NULL FROM roads", [], |r| r.get::<_, bool>(0)).unwrap());
    }

    #[test]
    fn gpkg_insert_revision_is_durable_and_strictly_advances() {
        let (_dir, path) = fixture();
        let conn = Connection::open(&path).unwrap();
        // A future timestamp forces the strictly-monotonic branch even on rapid saves.
        conn.execute("UPDATE gpkg_contents SET last_change = '2099-01-01T00:00:00.000Z'", []).unwrap();
        let table = EditTable::open(&path, "roads").unwrap();
        let loaded = table.read_editable(MAX_EDITABLE).unwrap();
        let edits = Edits { inserts: vec![Feature { id: 100, geometry: Some(line(4.0)), attrs: vec![] }], ..Default::default() };
        let saved = table.save(&edits, &table.info().fields, [1].into_iter(), &loaded.revision).unwrap().unwrap();
        assert!(saved.revision > loaded.revision);
        drop(table);
        // An older serialized project cannot repeat an insert, even in a new editing session.
        let table = EditTable::open(&path, "roads").unwrap();
        let error = table.save(&edits, &table.info().fields, [1].into_iter(), &loaded.revision).unwrap_err();
        assert!(error.to_string().contains("refusing stale inserts"), "{error}");
        assert_eq!(conn.query_row("SELECT count(*) FROM roads", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
        assert_eq!(table.read_editable(MAX_EDITABLE).unwrap().revision, saved.revision);
        let mut revision = saved.revision;
        for _ in 0..8 {
            // Identical geometries/attributes are legitimate new features, not duplicate rows.
            let saved = table.save(&edits, &table.info().fields, [1].into_iter(), &revision).unwrap().unwrap();
            assert!(saved.revision > revision);
            assert_eq!(table.read_editable(MAX_EDITABLE).unwrap().revision, saved.revision);
            revision = saved.revision;
        }
        assert_eq!(conn.query_row("SELECT count(*) FROM roads", [], |r| r.get::<_, i64>(0)).unwrap(), 10);
        conn.execute_batch(
            "CREATE TRIGGER reject_marker BEFORE UPDATE OF last_change ON gpkg_contents
             BEGIN SELECT RAISE(ABORT, 'marker refused'); END;",
        )
        .unwrap();
        let before = table.read_editable(MAX_EDITABLE).unwrap();
        let error = table.save(&edits, &table.info().fields, [1].into_iter(), &revision).unwrap_err();
        assert!(error.to_string().contains("marker refused"), "{error}");
        let after = table.read_editable(MAX_EDITABLE).unwrap();
        assert_eq!(after.revision, before.revision);
        assert_eq!(after.features, before.features);
        assert_eq!(after.versions, before.versions);
    }

    #[test]
    fn gpkg_requires_valid_revision_metadata() {
        for mutation in
            ["UPDATE gpkg_contents SET last_change = NULL", "UPDATE gpkg_contents SET last_change = 'invalid'", "DELETE FROM gpkg_contents"]
        {
            let (_dir, path) = fixture();
            let table = EditTable::open(&path, "roads").unwrap();
            let loaded = table.read_editable(MAX_EDITABLE).unwrap();
            Connection::open(&path).unwrap().execute(mutation, []).unwrap();
            assert!(table.read_editable(MAX_EDITABLE).is_err(), "{mutation}");
            let edits = Edits { inserts: vec![Feature { id: 100, geometry: None, attrs: vec![] }], ..Default::default() };
            let error = table.save(&edits, &table.info().fields, [1].into_iter(), &loaded.revision).unwrap_err();
            assert!(error.to_string().contains("missing or invalid"), "{mutation}: {error}");
            assert_eq!(Connection::open(&path).unwrap().query_row("SELECT count(*) FROM roads", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        }
    }

    #[test]
    fn gpkg_rechecks_geometry_declaration_inside_write_transaction() {
        for (declaration, message) in [
            ("srs_id = 3857", "SRS changed"),
            ("geometry_type_name = 'POINT'", "changed shape"),
            ("z = 1", "changed shape"),
            ("m = 1", "changed shape"),
        ] {
            for spatial in [false, true] {
                let (_dir, path) = fixture();
                let table = EditTable::open(&path, "roads").unwrap();
                let before = table.read_editable(MAX_EDITABLE).unwrap();
                let mut changed = before.features[0].clone();
                changed.attrs[0] = Value::Text("must not save".into());
                if spatial {
                    changed.geometry = Some(line(8.0));
                }
                let edits = Edits { updates: vec![(changed, before.versions[0].1)], ..Default::default() };
                Connection::open(&path).unwrap().execute(&format!("UPDATE gpkg_geometry_columns SET {declaration}"), []).unwrap();
                let error = table.save(&edits, &table.info().fields, [1].into_iter(), &before.revision).unwrap_err();
                assert!(error.to_string().contains(message), "{declaration}: {error}");
                let after = table.read_editable(MAX_EDITABLE).unwrap();
                assert_eq!(after.features, before.features);
                assert_eq!(after.versions, before.versions);
                assert_eq!(after.revision, before.revision);
            }
        }
    }

    #[test]
    fn gpkg_unknown_bounds_include_untouched_native_geometries() {
        for untouched in [false, true] {
            for bounds in ["NULL, NULL, NULL, NULL", "NULL, 0, 2, 3"] {
                let (_dir, path) = fixture();
                let conn = Connection::open(&path).unwrap();
                conn.execute("UPDATE roads SET geom = NULL", []).unwrap();
                conn.execute(&format!("UPDATE gpkg_contents SET (min_x, min_y, max_x, max_y) = ({bounds})"), []).unwrap();
                if untouched {
                    let g = line(-30.0);
                    conn.execute(
                        "INSERT INTO roads (fid, geom) VALUES (2, ?1)",
                        [header_blob(&wkb::encode_single(&g).unwrap(), &g.extent(), 4326)],
                    )
                    .unwrap();
                }
                let table = EditTable::open(&path, "roads").unwrap();
                let read = table.read_editable(MAX_EDITABLE).unwrap();
                let edits = Edits { inserts: vec![Feature { id: 100, geometry: Some(line(10.0)), attrs: vec![] }], ..Default::default() };
                table.save(&edits, &table.info().fields, read.versions.iter().map(|(k, _)| *k), &read.revision).unwrap().unwrap();
                let bounds: (f64, f64, f64, f64) = conn
                    .query_row("SELECT min_x, min_y, max_x, max_y FROM gpkg_contents", [], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                    })
                    .unwrap();
                assert_eq!(bounds, (if untouched { -30.0 } else { 10.0 }, 2.0, 11.0, 3.0));
            }
        }
    }

    #[test]
    fn gpkg_final_trigger_geometries_repair_all_rtree_coverage() {
        for coverage in ["none", "partial", "full"] {
            let (_dir, path) = fixture();
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE VIRTUAL TABLE rtree_roads_geom USING rtree(id, minx, maxx, miny, maxy);
                 INSERT INTO rtree_roads_geom VALUES (1, 1, 2, 2, 3);
                 CREATE TABLE moves (kind TEXT, geom BLOB);",
            )
            .unwrap();
            for (kind, x) in [("insert", 60.0), ("update", 90.0), ("delete", 40.0)] {
                let g = line(x);
                conn.execute(
                    "INSERT INTO moves VALUES (?1, ?2)",
                    params![kind, header_blob(&wkb::encode_single(&g).unwrap(), &g.extent(), 4326)],
                )
                .unwrap();
            }
            if coverage == "full" {
                conn.execute_batch(
                    "CREATE TRIGGER spatial_update AFTER UPDATE OF geom ON roads BEGIN
                       DELETE FROM rtree_roads_geom WHERE id = OLD.fid;
                       INSERT INTO rtree_roads_geom SELECT NEW.fid, ST_MinX(NEW.geom), ST_MaxX(NEW.geom), ST_MinY(NEW.geom), ST_MaxY(NEW.geom)
                         WHERE NEW.geom IS NOT NULL AND NOT ST_IsEmpty(NEW.geom);
                     END;
                     CREATE TRIGGER spatial_insert AFTER INSERT ON roads BEGIN
                       INSERT OR REPLACE INTO rtree_roads_geom SELECT NEW.fid, ST_MinX(NEW.geom), ST_MaxX(NEW.geom), ST_MinY(NEW.geom), ST_MaxY(NEW.geom)
                         WHERE NEW.geom IS NOT NULL AND NOT ST_IsEmpty(NEW.geom);
                     END;"
                ).unwrap();
            }
            if coverage != "none" {
                conn.execute_batch(
                    "CREATE TRIGGER spatial_delete AFTER DELETE ON roads BEGIN
                       DELETE FROM rtree_roads_geom WHERE id = OLD.fid;
                     END;",
                )
                .unwrap();
            }
            conn.execute_batch(
                "CREATE TRIGGER move_insert AFTER INSERT ON roads WHEN NEW.name = 'move' BEGIN
                   UPDATE roads SET geom = (SELECT geom FROM moves WHERE kind = 'insert') WHERE fid = NEW.fid;
                 END;
                 CREATE TRIGGER move_update AFTER UPDATE OF name ON roads WHEN NEW.name = 'move' BEGIN
                   UPDATE roads SET geom = (SELECT geom FROM moves WHERE kind = 'update') WHERE fid = NEW.fid;
                 END;",
            )
            .unwrap();
            let table = EditTable::open(&path, "roads").unwrap();
            let read = table.read_editable(MAX_EDITABLE).unwrap();
            let mut changed = read.features[0].clone();
            // Attribute-only submitted update; its trigger moves the stored geometry.
            changed.attrs[0] = Value::Text("move".into());
            let edits = Edits {
                updates: vec![(changed, read.versions[0].1)],
                inserts: vec![Feature { id: 100, geometry: Some(line(4.0)), attrs: vec![Value::Text("move".into()), Value::Null] }],
                ..Default::default()
            };
            let saved = table.save(&edits, &table.info().fields, [1].into_iter(), &read.revision).unwrap().unwrap();
            assert_eq!(saved.features[0].geometry, Some(line(90.0)), "{coverage}");
            assert_eq!(saved.features[1].geometry, Some(line(60.0)), "{coverage}");
            for (x, expected) in [(90.5, vec![1_i64]), (60.5, vec![2]), (4.5, vec![]), (1.5, vec![])] {
                let mut st = conn.prepare("SELECT id FROM rtree_roads_geom WHERE minx <= ?1 AND maxx >= ?1 ORDER BY id").unwrap();
                let keys = st.query_map([x], |r| r.get::<_, i64>(0)).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
                assert_eq!(keys, expected, "{coverage}: {x}");
            }
            let bounds: (f64, f64, f64, f64) = conn
                .query_row("SELECT min_x, min_y, max_x, max_y FROM gpkg_contents", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .unwrap();
            assert_eq!(bounds, (60.0, 2.0, 91.0, 3.0), "{coverage}");
            conn.execute_batch(
                "CREATE TRIGGER keep_deleted AFTER DELETE ON roads WHEN OLD.fid = 1 BEGIN
                   INSERT INTO roads (fid, geom, name) SELECT OLD.fid, geom, 'kept' FROM moves WHERE kind = 'delete';
                 END;",
            )
            .unwrap();
            let edits = Edits { deletes: saved.changes.versions, ..Default::default() };
            let restored = table.save(&edits, &table.info().fields, [1, 2].into_iter(), &saved.revision).unwrap().unwrap();
            assert_eq!(restored.changes.deleted, 1);
            assert_eq!(restored.features[0].id, 1);
            assert_eq!(restored.features[0].geometry, Some(line(40.0)));
            assert_eq!(restored.features[0].attrs[0], Value::Text("kept".into()));
            assert_eq!(restored.changes.versions, table.read_editable(MAX_EDITABLE).unwrap().versions);
            let index: (i64, f64, f64) =
                conn.query_row("SELECT id, minx, maxx FROM rtree_roads_geom", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
            assert_eq!(index, (1, 40.0, 41.0), "{coverage}");
            assert_eq!(conn.query_row("SELECT count(*) FROM rtree_roads_geom", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
            let bounds: (f64, f64, f64, f64) = conn
                .query_row("SELECT min_x, min_y, max_x, max_y FROM gpkg_contents", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .unwrap();
            assert_eq!(bounds, (40.0, 2.0, 41.0, 3.0), "{coverage}");
        }
    }

    #[test]
    fn gpkg_cross_row_triggers_rebuild_spatial_state_transactionally() {
        use crate::source::{FeatureSource, Request};
        for indexed in [false, true] {
            let (_dir, path) = fixture();
            let conn = Connection::open(&path).unwrap();
            let other = line(4.0);
            conn.execute(
                "INSERT INTO roads (fid, geom, name) VALUES (2, ?1, 'other'), (3, ?1, 'deleted')",
                [header_blob(&wkb::encode_single(&other).unwrap(), &other.extent(), 4326)],
            )
            .unwrap();
            if indexed {
                conn.execute_batch(
                    "CREATE VIRTUAL TABLE rtree_roads_geom USING rtree(id,minx,maxx,miny,maxy);
                    INSERT INTO rtree_roads_geom VALUES(1,1,2,2,3),(2,4,5,2,3),(3,4,5,2,3);",
                )
                .unwrap();
            }
            let moved = line(30.0);
            let inserted = line(60.0);
            conn.execute_batch(&format!(
                "CREATE TRIGGER move_other AFTER UPDATE OF name ON roads WHEN NEW.fid = 1 BEGIN
                UPDATE roads SET geom = x'{}' WHERE fid = 2;
                INSERT INTO roads (fid,geom,name) VALUES (7,x'{}','trigger inserted');
                DELETE FROM roads WHERE fid = 3;
                END;",
                header_blob(&wkb::encode_single(&moved).unwrap(), &moved.extent(), 4326)
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>(),
                header_blob(&wkb::encode_single(&inserted).unwrap(), &inserted.extent(), 4326)
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            ))
            .unwrap();
            let table = EditTable::open(&path, "roads").unwrap();
            let loaded = table.read_editable(MAX_EDITABLE).unwrap();
            let mut feature = loaded.features[0].clone();
            feature.attrs[0] = Value::Text("move other rows".into());
            let edits = Edits { updates: vec![(feature, loaded.versions[0].1)], ..Default::default() };
            let saved = table.save(&edits, &table.info().fields, [1, 2, 3].into_iter(), &loaded.revision).unwrap().unwrap();
            assert_eq!(saved.features.iter().map(|f| f.id).collect::<Vec<_>>(), vec![1, 2]);
            assert_eq!(saved.features[1].geometry, Some(moved.clone()));
            assert_eq!(saved.changes.versions.iter().map(|(k, _)| *k).collect::<Vec<_>>(), vec![1, 2]);
            assert_eq!(saved.removed, vec![3]);
            assert_eq!(saved.changes.deleted, 0, "trigger-only deletion is not a submitted deletion");
            let source = GpkgSource::open(&path, Some("roads")).unwrap();
            for (x, expected) in [(30.5, vec![2]), (60.5, vec![7]), (4.5, vec![])] {
                let batch = source.read(&Request { extent: Some(Extent::new(x, 1.0, x + 0.1, 4.0)), limit: 100 }).unwrap();
                assert_eq!(batch.features.iter().map(|f| f.id).collect::<Vec<_>>(), expected, "indexed={indexed}");
            }
            let bounds: (f64, f64, f64, f64) = conn
                .query_row("SELECT min_x, min_y, max_x, max_y FROM gpkg_contents", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .unwrap();
            assert_eq!(bounds, (1.0, 2.0, 61.0, 3.0), "indexed={indexed}");
            if indexed {
                let keys = conn
                    .prepare("SELECT id FROM rtree_roads_geom ORDER BY id")
                    .unwrap()
                    .query_map([], |r| r.get::<_, i64>(0))
                    .unwrap()
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .unwrap();
                assert_eq!(keys, vec![1, 2, 7]);
            }
        }
    }

    #[test]
    fn gpkg_observes_both_primary_keys_without_duplicate_resident_snapshots() {
        let (_dir, path) = fixture();
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "INSERT INTO roads(fid,name) VALUES(-2,'negative'),(2,'positive');
             CREATE TRIGGER replace_resident_key AFTER UPDATE OF name ON roads WHEN NEW.fid=1 BEGIN
             DELETE FROM roads WHERE fid=-2;
             UPDATE roads SET fid=-2,name='trigger result',lanes=9 WHERE fid=2;
             UPDATE roads SET lanes=10 WHERE fid=-2;
             INSERT INTO roads(fid,name) VALUES(-3,'outside scope');
             END;",
        )
        .unwrap();
        let table = EditTable::open(&path, "roads").unwrap();
        let loaded = table.read_editable(MAX_EDITABLE).unwrap();
        let mut feature = loaded.features.iter().find(|f| f.id == 1).unwrap().clone();
        feature.attrs[0] = Value::Text("change residents".into());
        let version = loaded.versions.iter().find(|(k, _)| *k == 1).unwrap().1;
        let edits = Edits { updates: vec![(feature, version)], ..Default::default() };
        let saved = table.save(&edits, &table.info().fields, [(-2_i64).cast_unsigned(), 2].into_iter(), &loaded.revision).unwrap().unwrap();
        assert_eq!(saved.features.iter().map(|f| f.id.cast_signed()).collect::<Vec<_>>(), vec![1, -2]);
        assert_eq!(saved.changes.versions.iter().map(|(k, _)| k.cast_signed()).collect::<Vec<_>>(), vec![1, -2]);
        assert_eq!(saved.removed, vec![2]);
        let negative = saved.features[1].clone();
        let lanes = table.info().fields.iter().position(|f| f.name == "lanes").unwrap();
        assert_eq!(negative.attrs[lanes], Value::Int(10));
        let mut next = negative;
        next.attrs[0] = Value::Text("next".into());
        let edits = Edits { updates: vec![(next, saved.changes.versions[1].1)], ..Default::default() };
        let saved = table.save(&edits, &table.info().fields, [1, (-2_i64).cast_unsigned()].into_iter(), &saved.revision).unwrap().unwrap();
        assert_eq!(saved.changes.updated, 1);
        let stored: (String, i64) =
            conn.query_row("SELECT name,lanes FROM roads WHERE fid=-2", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(stored, ("next".into(), 10));
    }

    #[test]
    fn gpkg_bounds_shrink_on_geometry_edits_and_deletes_and_clear_when_empty() {
        let (_dir, path) = fixture();
        let conn = Connection::open(&path).unwrap();
        let g = line(30.0);
        conn.execute("INSERT INTO roads (fid, geom) VALUES (2, ?1)", [header_blob(&wkb::encode_single(&g).unwrap(), &g.extent(), 4326)])
            .unwrap();
        conn.execute("UPDATE gpkg_contents SET min_x=-100, min_y=-100, max_x=100, max_y=100", []).unwrap();
        let bounds = || {
            conn.query_row("SELECT min_x,min_y,max_x,max_y FROM gpkg_contents", [], |r| {
                Ok((r.get::<_, Option<f64>>(0)?, r.get::<_, Option<f64>>(1)?, r.get::<_, Option<f64>>(2)?, r.get::<_, Option<f64>>(3)?))
            })
            .unwrap()
        };
        let table = EditTable::open(&path, "roads").unwrap();
        let read = table.read_editable(MAX_EDITABLE).unwrap();
        let mut moved = read.features[1].clone();
        moved.geometry = Some(line(-10.0));
        let edits = Edits { updates: vec![(moved, read.versions[1].1)], ..Default::default() };
        table.save(&edits, &table.info().fields, [1, 2].into_iter(), &read.revision).unwrap().unwrap();
        assert_eq!(bounds(), (Some(-10.0), Some(2.0), Some(2.0), Some(3.0)));
        for (key, expected) in [(2, (Some(1.0), Some(2.0), Some(2.0), Some(3.0))), (1, (None, None, None, None))] {
            let read = table.read_editable(MAX_EDITABLE).unwrap();
            let version = read.versions.iter().find(|(id, _)| *id == key).unwrap().1;
            let edits = Edits { deletes: vec![(key, version)], ..Default::default() };
            let saved = table.save(&edits, &table.info().fields, [1, 2].into_iter(), &read.revision).unwrap().unwrap();
            assert_eq!(saved.changes.deleted, 1);
            assert_eq!(bounds(), expected);
        }
        assert_eq!(conn.query_row("SELECT count(*) FROM roads", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    }

    #[test]
    fn gpkg_unreadable_untouched_geometry_refuses_trigger_rebuild_without_partial_writes() {
        let (_dir, path) = fixture();
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "INSERT INTO roads (fid, geom, name, lanes) VALUES (2,x'00','unreadable',2);
             CREATE VIRTUAL TABLE rtree_roads_geom USING rtree(id,minx,maxx,miny,maxy);
             INSERT INTO rtree_roads_geom VALUES(1,1,2,2,3),(2,5,6,2,3);
             CREATE TRIGGER modify_other AFTER UPDATE OF name ON roads WHEN NEW.fid=1
             BEGIN UPDATE roads SET lanes=7 WHERE fid=2; END;",
        )
        .unwrap();
        // A valid XY envelope must not hide an unreadable WKB body during a strict rebuild.
        let unreadable = header_blob(&[0], &Extent::new(5.0, 2.0, 6.0, 3.0), 4326);
        conn.execute("UPDATE roads SET geom = ?1 WHERE fid = 2", [&unreadable]).unwrap();
        let index = || {
            conn.prepare("SELECT id,minx,maxx,miny,maxy FROM rtree_roads_geom ORDER BY id")
                .unwrap()
                .query_map([], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?, r.get::<_, f64>(2)?, r.get::<_, f64>(3)?, r.get::<_, f64>(4)?))
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        let before_index = index();
        let table = EditTable::open(&path, "roads").unwrap();
        let read = table.read_editable(MAX_EDITABLE).unwrap();
        assert_eq!(read.skipped, 1);
        let mut feature = read.features[0].clone();
        feature.attrs[0] = Value::Text("must roll back".into());
        let edits = Edits { updates: vec![(feature, read.versions[0].1)], ..Default::default() };
        let error = table.save(&edits, &table.info().fields, [1].into_iter(), &read.revision).unwrap_err();
        assert!(error.to_string().contains("stored geometry can't be read"), "{error}");
        let after = table.read_editable(MAX_EDITABLE).unwrap();
        assert_eq!(after.features, read.features);
        assert_eq!(after.versions, read.versions);
        assert_eq!(after.revision, read.revision);
        assert_eq!(index(), before_index);
        let bad: (Vec<u8>, i64) = conn.query_row("SELECT geom, lanes FROM roads WHERE fid=2", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(bad, (unreadable, 2));
        let bounds: (f64, f64, f64, f64) = conn
            .query_row("SELECT min_x,min_y,max_x,max_y FROM gpkg_contents", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap();
        assert_eq!(bounds, (0.0, 0.0, 2.0, 3.0));
    }

    #[test]
    fn gpkg_conflict_replace_evictions_are_removed_but_earlier_external_deletes_are_not() {
        let (_dir, path) = fixture();
        let conn = Connection::open(&path).unwrap();
        // INSERT OR REPLACE on a unique column deletes row 2 without firing a delete trigger.
        conn.execute_batch(
            "INSERT INTO roads (fid, name) VALUES (2, 'two'), (3, 'three');
             CREATE UNIQUE INDEX roads_name ON roads(name);
             CREATE TRIGGER evict AFTER UPDATE OF name ON roads WHEN NEW.fid = 1
             BEGIN INSERT OR REPLACE INTO roads (fid, name) VALUES (9, 'two'); END;",
        )
        .unwrap();
        let table = EditTable::open(&path, "roads").unwrap();
        let read = table.read_editable(MAX_EDITABLE).unwrap();
        assert_eq!(read.features.iter().map(|f| f.id).collect::<Vec<_>>(), vec![1, 2, 3]);
        // Deleted by someone else before this save: not this save's eviction.
        conn.execute("DELETE FROM roads WHERE fid = 3", []).unwrap();
        let mut changed = read.features[0].clone();
        changed.attrs[0] = Value::Text("changed".into());
        let edits = Edits { updates: vec![(changed, read.versions[0].1)], ..Default::default() };
        let saved = table.save(&edits, &table.info().fields, read.versions.iter().map(|(k, _)| *k), &read.revision).unwrap().unwrap();
        assert_eq!(conn.query_row("SELECT count(*) FROM roads WHERE fid = 2", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        assert_eq!(saved.removed, vec![2], "the evicted resident row is reported; the earlier external delete isn't");
        assert!(saved.features.iter().all(|f| f.id != 9), "an unknown trigger-inserted row must not join the loaded scope");
    }

    #[test]
    fn gpkg_metadata_triggers_cannot_change_features_after_spatial_repair() {
        let (_dir, path) = fixture();
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER alter_after_snapshot AFTER UPDATE OF last_change ON gpkg_contents
            BEGIN UPDATE roads SET name = 'late mutation'; END;",
        )
        .unwrap();
        let table = EditTable::open(&path, "roads").unwrap();
        let loaded = table.read_editable(MAX_EDITABLE).unwrap();
        let mut feature = loaded.features[0].clone();
        feature.attrs[0] = Value::Text("must roll back".into());
        let edits = Edits { updates: vec![(feature, loaded.versions[0].1)], ..Default::default() };
        assert!(table.save(&edits, &table.info().fields, [1].into_iter(), &loaded.revision).is_err());
        let after = table.read_editable(MAX_EDITABLE).unwrap();
        assert_eq!(after.features, loaded.features);
        assert_eq!(after.versions, loaded.versions);
        assert_eq!(after.revision, loaded.revision);
    }

    fn line(x: f64) -> Geometry {
        Geometry::Line(vec![vec![Coord::new(x, 2.0), Coord::new(x + 1.0, 3.0)]])
    }

    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("roads.gpkg");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE gpkg_spatial_ref_sys (srs_id INTEGER PRIMARY KEY, organization TEXT, organization_coordsys_id INTEGER);
             INSERT INTO gpkg_spatial_ref_sys VALUES (4326, 'EPSG', 4326);
             CREATE TABLE gpkg_contents (table_name TEXT PRIMARY KEY, last_change TEXT, min_x DOUBLE, min_y DOUBLE, max_x DOUBLE, max_y DOUBLE);
             INSERT INTO gpkg_contents VALUES ('roads', '2026-01-01T00:00:00.000Z', 0, 0, 2, 3);
             CREATE TABLE gpkg_geometry_columns (table_name TEXT, column_name TEXT, geometry_type_name TEXT, srs_id INTEGER, z INTEGER, m INTEGER);
             INSERT INTO gpkg_geometry_columns VALUES ('roads', 'geom', 'LINESTRING', 4326, 0, 0);
             CREATE TABLE roads (fid INTEGER PRIMARY KEY, geom LINESTRING, name TEXT DEFAULT 'default', lanes INTEGER DEFAULT 2, photo BLOB);
             CREATE TABLE other (v TEXT);
             INSERT INTO other VALUES ('keep');",
        ).unwrap();
        let g = line(1.0);
        conn.execute(
            "INSERT INTO roads VALUES (1, ?1, 'old', 2, x'00ff')",
            [header_blob(&wkb::encode_single(&g).unwrap(), &g.extent(), 4326)],
        )
        .unwrap();
        (dir, path)
    }

    fn blob_at(path: &Path, id: i64) -> Vec<u8> {
        Connection::open(path).unwrap().query_row("SELECT geom FROM roads WHERE fid = ?1", [id], |r| r.get(0)).unwrap()
    }

    fn update(table: &EditTable, geometry: Option<Geometry>) -> Edits {
        let read = table.read_editable(MAX_EDITABLE).unwrap();
        let mut f = read.features[0].clone();
        f.geometry = geometry;
        Edits { updates: vec![(f, read.versions[0].1)], ..Default::default() }
    }

    #[test]
    fn gpkg_single_lines_and_transaction_rollback() {
        let (_dir, path) = fixture();
        let table = EditTable::open(&path, "ROADS").unwrap();
        let saved = table.save(&update(&table, Some(line(4.0))), &table.info().fields, std::iter::empty(), REVISION).unwrap().unwrap();
        assert_eq!(saved.changes.updated, 1);
        let bytes = blob_at(&path, 1);
        let (_, body) = split_blob(&bytes).unwrap();
        assert_eq!(body.get(1..5), Some(2u32.to_le_bytes().as_slice()));
        assert_eq!(wkb::decode(body), Some(line(4.0)));
        assert_eq!(saved.changes.versions, table.read_editable(MAX_EDITABLE).unwrap().versions);
        let part = vec![Coord::new(5.0, 2.0), Coord::new(6.0, 3.0)];
        let multipart = Geometry::Line(vec![part.clone(), part]);
        assert!(
            table
                .save(&update(&table, Some(multipart)), &table.info().fields, std::iter::empty(), &saved.revision)
                .unwrap_err()
                .to_string()
                .contains("holds one part")
        );
        assert_eq!(blob_at(&path, 1), bytes);

        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER no_bad BEFORE INSERT ON roads WHEN NEW.name = 'bad' BEGIN SELECT RAISE(ABORT, 'bad name'); END;",
        )
        .unwrap();
        let read = table.read_editable(MAX_EDITABLE).unwrap();
        let mut changed = read.features[0].clone();
        changed.attrs[0] = Value::Text("partial".into());
        let new = Feature { id: 100, geometry: Some(line(7.0)), attrs: vec![Value::Text("bad".into()), Value::Null] };
        let edits = Edits { updates: vec![(changed, read.versions[0].1)], inserts: vec![new], ..Default::default() };
        let before: String = conn.query_row("SELECT last_change FROM gpkg_contents", [], |r| r.get(0)).unwrap();
        assert!(table.save(&edits, &table.info().fields, std::iter::empty(), &read.revision).unwrap_err().to_string().contains("bad name"));
        assert_eq!(conn.query_row("SELECT name FROM roads WHERE fid = 1", [], |r| r.get::<_, String>(0)).unwrap(), "old");
        assert_eq!(conn.query_row("SELECT count(*) FROM roads", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        assert_eq!(conn.query_row("SELECT last_change FROM gpkg_contents", [], |r| r.get::<_, String>(0)).unwrap(), before);
        assert_eq!(blob_at(&path, 1), bytes);
    }

    #[test]
    fn gpkg_conflicts_fingerprint_unexposed_columns() {
        let (_dir, path) = fixture();
        let table = EditTable::open(&path, "roads").unwrap();
        let edits = update(&table, Some(line(4.0)));
        let conn = Connection::open(&path).unwrap();
        conn.execute("UPDATE roads SET photo = x'01ff' WHERE fid = 1", []).unwrap();
        assert_eq!(
            table.save(&edits, &table.info().fields, std::iter::empty(), REVISION).unwrap().unwrap_err(),
            vec![Conflict::Changed(1)]
        );
        assert_eq!(conn.query_row("SELECT last_change FROM gpkg_contents", [], |r| r.get::<_, String>(0)).unwrap(), REVISION);
        conn.execute("DELETE FROM roads WHERE fid = 1", []).unwrap();
        assert_eq!(
            table.save(&edits, &table.info().fields, std::iter::empty(), REVISION).unwrap().unwrap_err(),
            vec![Conflict::Deleted(1)]
        );
        assert_ne!(
            fingerprint([ValueRef::Text(b"a"), ValueRef::Text(b"bc")].into_iter()),
            fingerprint([ValueRef::Text(b"ab"), ValueRef::Text(b"c")].into_iter())
        );
        assert_ne!(fingerprint([ValueRef::Text(b"1")].into_iter()), fingerprint([ValueRef::Blob(b"1")].into_iter()));
        assert_ne!(fingerprint([ValueRef::Integer(1)].into_iter()), fingerprint([ValueRef::Real(1.0)].into_iter()));
    }

    #[test]
    fn gpkg_editable_keeps_null_and_empty_but_skips_bad_rows() {
        let (_dir, path) = fixture();
        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "INSERT INTO roads (fid, geom) VALUES (2, NULL), (3, ?1), (4, x'47500003'), (5, 'not geometry')",
            [header_blob(&[], &Extent::EMPTY, 4326)],
        )
        .unwrap();
        let table = EditTable::open(&path, "roads").unwrap();
        let read = table.read_editable(MAX_EDITABLE).unwrap();
        assert_eq!((read.matched, read.skipped, read.features.len()), (5, 2, 3));
        assert_eq!(read.versions.iter().map(|(k, _)| *k).collect::<Vec<_>>(), vec![1, 2, 3]);
        assert!(read.features[1..].iter().all(|f| f.geometry.is_none()));
        assert_eq!(table.read_editable(0).unwrap().features.len(), 1);
        let edits = Edits { deletes: read.versions, ..Default::default() };
        assert_eq!(table.save(&edits, &table.info().fields, std::iter::empty(), &read.revision).unwrap().unwrap().changes.deleted, 3);
        assert_eq!(conn.query_row("SELECT count(*) FROM roads", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
        assert_eq!(conn.query_row("SELECT geom FROM roads WHERE fid = 4", [], |r| r.get::<_, Vec<u8>>(0)).unwrap(), b"GP\0\x03");
    }

    #[test]
    fn gpkg_zm_refuses_geometry_removal_but_allows_attributes() {
        let (_dir, path) = fixture();
        let conn = Connection::open(&path).unwrap();
        // Optional Z with an actual ISO XYZ line: the read geometry deliberately drops Z.
        conn.execute("UPDATE gpkg_geometry_columns SET z = 2", []).unwrap();
        let mut body = vec![1];
        body.extend_from_slice(&1002u32.to_le_bytes());
        body.extend_from_slice(&2u32.to_le_bytes());
        for v in [1.0_f64, 2.0, 9.0, 2.0, 3.0, 10.0] {
            body.extend_from_slice(&v.to_le_bytes());
        }
        let original = header_blob(&body, &line(1.0).extent(), 4326);
        conn.execute("UPDATE roads SET geom = ?1 WHERE fid = 1", [&original]).unwrap();
        let table = EditTable::open(&path, "roads").unwrap();
        // Reading drops that Z, and says so (the optional Z found in the first rows).
        assert!(table.info().zm_dropped);
        for g in [None, Some(line(6.0))] {
            assert!(
                table
                    .save(&update(&table, g), &table.info().fields, std::iter::empty(), REVISION)
                    .unwrap_err()
                    .to_string()
                    .contains("Z or M")
            );
            assert_eq!(blob_at(&path, 1), original);
        }
        let mut edits = update(&table, Some(line(1.0)));
        edits.updates[0].0.attrs[0] = Value::Text("attributes only".into());
        let saved = table.save(&edits, &table.info().fields, std::iter::empty(), REVISION).unwrap().unwrap();
        assert_eq!(saved.changes.updated, 1);
        assert_eq!(blob_at(&path, 1), original);
        conn.execute("UPDATE gpkg_geometry_columns SET z = 1", []).unwrap();
        let table = EditTable::open(&path, "roads").unwrap();
        let edits = Edits { inserts: vec![Feature { id: 100, geometry: Some(line(4.0)), attrs: vec![] }], ..Default::default() };
        assert!(
            table.save(&edits, &table.info().fields, std::iter::empty(), &saved.revision).unwrap_err().to_string().contains("needs Z or M")
        );
        let edits = Edits { inserts: vec![Feature { id: 100, geometry: None, attrs: vec![] }], ..Default::default() };
        assert_eq!(
            table.save(&edits, &table.info().fields, std::iter::empty(), &saved.revision).unwrap().unwrap().changes.inserted,
            vec![(100, 2)]
        );
    }

    #[test]
    fn gpkg_untriggered_rtree_metadata_and_generated_keys() {
        let (_dir, path) = fixture();
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE VIRTUAL TABLE rtree_roads_geom USING rtree(id, minx, maxx, miny, maxy);
            INSERT INTO rtree_roads_geom VALUES (1, 1, 2, 2, 3);",
        )
        .unwrap();
        let table = EditTable::open(&path, "roads").unwrap();
        let saved = table.save(&update(&table, Some(line(10.0))), &table.info().fields, std::iter::empty(), REVISION).unwrap().unwrap();
        let envelope: (f64, f64, f64, f64) = conn
            .query_row("SELECT minx, maxx, miny, maxy FROM rtree_roads_geom WHERE id = 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .unwrap();
        assert_eq!(envelope, (10.0, 11.0, 2.0, 3.0));
        assert_eq!(conn.query_row("SELECT max_x FROM gpkg_contents", [], |r| r.get::<_, f64>(0)).unwrap(), 11.0);
        assert_ne!(conn.query_row("SELECT last_change FROM gpkg_contents", [], |r| r.get::<_, String>(0)).unwrap(), REVISION);
        table.save(&update(&table, None), &table.info().fields, std::iter::empty(), &saved.revision).unwrap().unwrap();
        assert_eq!(conn.query_row("SELECT count(*) FROM rtree_roads_geom", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        let read = table.read_editable(MAX_EDITABLE).unwrap();
        let edits = Edits {
            deletes: read.versions,
            inserts: vec![Feature { id: 100, geometry: Some(line(20.0)), attrs: vec![] }],
            ..Default::default()
        };
        let saved = table.save(&edits, &table.info().fields, std::iter::empty(), &read.revision).unwrap().unwrap();
        assert_eq!(saved.changes.inserted, vec![(100, 2)]);
        assert_eq!(saved.changes.deleted, 1);
        assert_eq!(conn.query_row("SELECT id FROM rtree_roads_geom", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
        assert_eq!(conn.query_row("SELECT name FROM roads", [], |r| r.get::<_, String>(0)).unwrap(), "default");
        assert_eq!(conn.query_row("SELECT v FROM other", [], |r| r.get::<_, String>(0)).unwrap(), "keep");
    }

    #[test]
    fn gpkg_innocuous_functions_work_from_untrusted_triggers() {
        let (_dir, path) = fixture();
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE VIRTUAL TABLE rtree_roads_geom USING rtree(id, minx, maxx, miny, maxy);
             INSERT INTO rtree_roads_geom VALUES (1, 1, 2, 2, 3);
             CREATE TRIGGER spatial_update AFTER UPDATE OF geom ON roads
             BEGIN DELETE FROM rtree_roads_geom WHERE id = OLD.fid;
               INSERT INTO rtree_roads_geom SELECT NEW.fid, ST_MinX(NEW.geom), ST_MaxX(NEW.geom), ST_MinY(NEW.geom), ST_MaxY(NEW.geom)
               WHERE NEW.geom IS NOT NULL AND NOT ST_IsEmpty(NEW.geom);
             END;",
        )
        .unwrap();
        let table = EditTable::open(&path, "roads").unwrap();
        let saved = table.save(&update(&table, Some(line(8.0))), &table.info().fields, std::iter::empty(), REVISION).unwrap().unwrap();
        assert_eq!(conn.query_row("SELECT maxx FROM rtree_roads_geom", [], |r| r.get::<_, f64>(0)).unwrap(), 9.0);
        table.save(&update(&table, None), &table.info().fields, std::iter::empty(), &saved.revision).unwrap().unwrap();
        assert_eq!(conn.query_row("SELECT count(*) FROM rtree_roads_geom", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        register_functions(&conn).unwrap();
        for sql in ["SELECT ST_IsEmpty(NULL)", "SELECT ST_MinX('text')", "SELECT ST_MaxY(x'00')"] {
            assert_eq!(conn.query_row(sql, [], |r| r.get::<_, Option<f64>>(0)).unwrap(), None);
        }
        let raw = wkb::encode_single(&line(5.0)).unwrap();
        assert_eq!(conn.query_row("SELECT ST_MaxX(?1)", [raw], |r| r.get::<_, f64>(0)).unwrap(), 6.0);
    }

    #[test]
    fn gpkg_native_reprojection_writes_native_envelopes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mercator.gpkg");
        let crs = Crs::from_epsg(3857).unwrap();
        let mut ds = crate::Dataset::new("roads", crate::GeometryType::Polyline, vec![]);
        ds.push(Some(line(1_000_000.0)), vec![]);
        super::super::write_in(&ds, &path, crs).unwrap();
        let table = EditTable::open(&path, "roads").unwrap();
        assert_eq!(table.crs(), Some(crs));
        let mut edits = update(&table, None);
        let original = table.read_editable(MAX_EDITABLE).unwrap().features[0].geometry.clone().unwrap();
        let moved = original.map_coords(&|c| Coord::new(c.x + 1.0, c.y + 1.0));
        let expected = table.src.reproject.to_native(&moved).unwrap();
        edits.updates[0].0.geometry = Some(moved.clone());
        let revision = table.read_editable(MAX_EDITABLE).unwrap().revision;
        table.save(&edits, &[], std::iter::empty(), &revision).unwrap().unwrap();
        let stored = blob_at(&path, 1);
        let (envelope, body) = split_blob(&stored).unwrap();
        assert_eq!(envelope, Some(expected.extent()));
        assert_eq!(wkb::decode(body), Some(expected.clone()));
        let conn = Connection::open(&path).unwrap();
        let minx: f64 = conn.query_row("SELECT minx FROM rtree_roads_geom WHERE id = 1", [], |r| r.get(0)).unwrap();
        assert!((minx - expected.extent().min_x).abs() < 1.0);
        let reloaded = table.read_editable(MAX_EDITABLE).unwrap().features[0].geometry.clone().unwrap();
        let mut expected_coords = Vec::new();
        moved.for_each_coord(|c| expected_coords.push(c));
        let mut actual_coords = Vec::new();
        reloaded.for_each_coord(|c| actual_coords.push(c));
        assert!(expected_coords.iter().zip(actual_coords).all(|(a, b)| (a.x - b.x).abs() < 1e-6 && (a.y - b.y).abs() < 1e-6));
    }

    #[test]
    fn gpkg_fields_match_by_name_and_unstored_edits_are_unchanged() {
        let (_dir, path) = fixture();
        let table = EditTable::open(&path, "roads").unwrap();
        let read = table.read_editable(MAX_EDITABLE).unwrap();
        let fields = vec![
            Field::new("missing", crate::FieldType::Text),
            Field::new("NAME", crate::FieldType::Text),
            Field::new("fid", crate::FieldType::Integer),
        ];
        let mut f = read.features[0].clone();
        f.attrs = vec![Value::Text("not saved".into()), Value::Text("old".into()), Value::Int(999)];
        let mut edits = Edits { updates: vec![(f, read.versions[0].1)], ..Default::default() };
        let saved = table.save(&edits, &fields, std::iter::empty(), &read.revision).unwrap().unwrap();
        assert_eq!((saved.changes.unchanged, saved.changes.updated), (1, 0));
        assert_eq!(saved.changes.skipped_fields, vec!["missing", "fid"]);
        let mut expected = edits.updates[0].0.clone();
        expected.attrs[2] = Value::Int(1);
        assert_eq!(saved.features, vec![expected]);
        edits.updates[0].0.attrs[1] = Value::Text("new".into());
        assert_eq!(table.save(&edits, &fields, std::iter::empty(), &saved.revision).unwrap().unwrap().changes.updated, 1);
        let conn = Connection::open(&path).unwrap();
        let stored: (i64, String, i64, Vec<u8>) =
            conn.query_row("SELECT fid, name, lanes, photo FROM roads", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap();
        assert_eq!(stored, (1, "new".into(), 2, vec![0, 255]));
        conn.execute("ALTER TABLE roads RENAME COLUMN geom TO shape", []).unwrap();
        assert!(table.save(&Edits::default(), &fields, std::iter::empty(), REVISION).unwrap_err().to_string().contains("changed shape"));
    }

    #[test]
    fn gpkg_saved_snapshots_include_defaults_triggers_and_supplied_field_order() {
        let (_dir, path) = fixture();
        let table = EditTable::open(&path, "roads").unwrap();
        let conn = Connection::open(&path).unwrap();
        // This column appears after open: snapshots must use the transaction's column list.
        conn.execute_batch(
            "ALTER TABLE roads ADD COLUMN added TEXT DEFAULT 'added-default';
             CREATE TRIGGER stored_insert AFTER INSERT ON roads BEGIN
               UPDATE roads SET name = upper(NEW.name), lanes = NEW.lanes + 5,
                 geom = (SELECT geom FROM roads WHERE fid = 1) WHERE fid = NEW.fid;
             END;
             CREATE TRIGGER stored_update AFTER UPDATE OF name ON roads
             WHEN NEW.name != upper(NEW.name) BEGIN
               UPDATE roads SET name = upper(NEW.name), lanes = NEW.lanes + 1, geom = NULL WHERE fid = NEW.fid;
             END;",
        )
        .unwrap();
        let fields = vec![
            Field::new("lanes", crate::FieldType::Integer),
            Field::new("local", crate::FieldType::Text),
            Field::new("NAME", crate::FieldType::Text),
            Field::new("added", crate::FieldType::Text),
            Field::new("fid", crate::FieldType::Integer),
        ];
        let version = EditTable::open(&path, "roads").unwrap().read_editable(MAX_EDITABLE).unwrap().versions[0].1;
        let unchanged = Feature {
            id: 1,
            geometry: Some(line(1.0)),
            attrs: vec![
                Value::Int(2),
                Value::Text("kept".into()),
                Value::Text("old".into()),
                Value::Text("added-default".into()),
                Value::Int(999),
            ],
        };
        let inserted = Feature {
            id: 100,
            geometry: None,
            attrs: vec![Value::Null, Value::Text("insert-only".into()), Value::Null, Value::Null, Value::Int(999)],
        };
        let edits = Edits { updates: vec![(unchanged, version)], inserts: vec![inserted], ..Default::default() };
        let saved = table.save(&edits, &fields, [1].into_iter(), REVISION).unwrap().unwrap();
        assert_eq!((saved.changes.unchanged, saved.changes.updated), (1, 0));
        assert_eq!(saved.changes.inserted, vec![(100, 2)]);
        assert_eq!(saved.features.len(), 2);
        assert_eq!(saved.unstored_fields, vec![1]);
        assert_eq!(saved.features[0].attrs[1], Value::Text("kept".into()));
        assert_eq!(saved.features[0].attrs[4], Value::Int(1));
        assert_eq!(
            saved.features[1],
            Feature {
                id: 2,
                geometry: Some(line(1.0)),
                attrs: vec![
                    Value::Int(7),
                    Value::Text("insert-only".into()),
                    Value::Text("DEFAULT".into()),
                    Value::Text("added-default".into()),
                    Value::Int(2),
                ],
            }
        );
        let reopened = EditTable::open(&path, "roads").unwrap();
        assert_eq!(saved.changes.versions, reopened.read_editable(MAX_EDITABLE).unwrap().versions);

        let mut changed = saved.features[1].clone();
        changed.attrs[2] = Value::Text("edited".into());
        changed.geometry = Some(line(8.0));
        let edits = Edits { updates: vec![(changed, saved.changes.versions[1].1)], ..Default::default() };
        let saved = table.save(&edits, &fields, [1, 2].into_iter(), &saved.revision).unwrap().unwrap();
        assert_eq!(saved.changes.updated, 1);
        assert_eq!(saved.features[0].geometry, None);
        assert_eq!(saved.features[0].attrs[0], Value::Int(8));
        assert_eq!(saved.features[0].attrs[1], Value::Text("insert-only".into()));
        assert_eq!(saved.features[0].attrs[2], Value::Text("EDITED".into()));
        let read = reopened.read_editable(MAX_EDITABLE).unwrap();
        assert_eq!(saved.changes.versions, vec![read.versions[1]]);
        // Reusing the returned snapshot and version must not reset trigger-produced values.
        let edits = Edits { updates: vec![(saved.features[0].clone(), saved.changes.versions[0].1)], ..Default::default() };
        let saved_again = table.save(&edits, &fields, [1, 2].into_iter(), &saved.revision).unwrap().unwrap();
        assert_eq!((saved_again.changes.unchanged, saved_again.changes.updated), (1, 0));
        assert_eq!(saved_again.features, saved.features);
    }

    #[test]
    fn gpkg_unreadable_or_removed_stored_rows_roll_back() {
        for operation in ["INSERT", "UPDATE"] {
            for (action, message) in [
                ("UPDATE roads SET geom = x'00' WHERE fid = NEW.fid", "stored geometry can't be read"),
                ("DELETE FROM roads WHERE fid = NEW.fid", "removed while saving"),
            ] {
                let (_dir, path) = fixture();
                let conn = Connection::open(&path).unwrap();
                conn.execute_batch(&format!(
                    "CREATE TRIGGER break_stored AFTER {operation} ON roads WHEN NEW.name = 'broken'
                     BEGIN {action}; END;"
                ))
                .unwrap();
                let table = EditTable::open(&path, "roads").unwrap();
                let read = table.read_editable(MAX_EDITABLE).unwrap();
                let mut changed = read.features[0].clone();
                changed.attrs[0] = Value::Text(if operation == "UPDATE" { "broken" } else { "partial" }.into());
                let mut edits = Edits { updates: vec![(changed, read.versions[0].1)], ..Default::default() };
                if operation == "INSERT" {
                    edits.inserts.push(Feature {
                        id: 100,
                        geometry: Some(line(4.0)),
                        attrs: vec![Value::Text("broken".into()), Value::Null],
                    });
                }
                let error = table.save(&edits, &table.info().fields, [1].into_iter(), &read.revision).unwrap_err();
                assert!(error.to_string().contains(message), "{operation}: {error}");
                let after = table.read_editable(MAX_EDITABLE).unwrap();
                assert_eq!(after.features, read.features);
                assert_eq!(after.versions, read.versions);
                assert_eq!(after.matched, 1);
                assert_eq!(conn.query_row("SELECT last_change FROM gpkg_contents", [], |r| r.get::<_, String>(0)).unwrap(), read.revision);
            }
        }
    }

    #[test]
    fn gpkg_reserves_loaded_keys_after_external_highest_key_deletion() {
        let (_dir, path) = fixture();
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("INSERT INTO roads (fid) VALUES (-1), (2), (3)").unwrap();
        let table = EditTable::open(&path, "roads").unwrap();
        let read = table.read_editable(MAX_EDITABLE).unwrap();
        conn.execute("DELETE FROM roads WHERE fid = 3", []).unwrap();
        let edits = Edits {
            inserts: vec![Feature { id: 999, geometry: None, attrs: vec![Value::Text("new".into()), Value::Null] }],
            ..Default::default()
        };
        let saved = table.save(&edits, &table.info().fields, read.versions.iter().map(|(k, _)| *k), &read.revision).unwrap().unwrap();
        assert_eq!(saved.changes.inserted, vec![(999, 4)]);
        assert_eq!(saved.features[0].id, 4);
        assert_eq!(conn.query_row("SELECT count(*) FROM roads WHERE fid IN (3, 999)", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        assert_eq!(saved.changes.versions, vec![table.read_editable(MAX_EDITABLE).unwrap().versions[3]]);
    }

    #[test]
    fn gpkg_generated_keys_respect_autoincrement_history() {
        let (_dir, path) = fixture();
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "ALTER TABLE roads RENAME TO old_roads;
             CREATE TABLE roads (fid INTEGER PRIMARY KEY AUTOINCREMENT, geom LINESTRING, name TEXT DEFAULT 'default', lanes INTEGER DEFAULT 2, photo BLOB);
             INSERT INTO roads SELECT * FROM old_roads;
             DROP TABLE old_roads;
             INSERT INTO roads (fid) VALUES (100);
             DELETE FROM roads WHERE fid = 100;",
        )
        .unwrap();
        let table = EditTable::open(&path, "roads").unwrap();
        let edits = Edits { inserts: vec![Feature { id: 999, geometry: None, attrs: vec![] }], ..Default::default() };
        let saved = table.save(&edits, &table.info().fields, [70].into_iter(), REVISION).unwrap().unwrap();
        assert_eq!(saved.changes.inserted, vec![(999, 101)]);
        assert_eq!(saved.features[0].id, 101);
        assert_eq!(conn.query_row("SELECT seq FROM sqlite_sequence WHERE name = 'roads'", [], |r| r.get::<_, i64>(0)).unwrap(), 101);
    }

    #[test]
    fn gpkg_key_exhaustion_rolls_back_updates_and_earlier_inserts() {
        for high_water_source in ["table", "reserved", "sequence"] {
            let (_dir, path) = fixture();
            let conn = Connection::open(&path).unwrap();
            if high_water_source == "sequence" {
                conn.execute_batch(
                    "ALTER TABLE roads RENAME TO old_roads;
                     CREATE TABLE roads (fid INTEGER PRIMARY KEY AUTOINCREMENT, geom LINESTRING, name TEXT DEFAULT 'default', lanes INTEGER DEFAULT 2, photo BLOB);
                     INSERT INTO roads SELECT * FROM old_roads;
                     DROP TABLE old_roads;",
                )
                .unwrap();
            }
            if high_water_source != "reserved" {
                conn.execute("INSERT INTO roads (fid) VALUES (?1)", [i64::MAX - 1]).unwrap();
                if high_water_source == "sequence" {
                    conn.execute("DELETE FROM roads WHERE fid = ?1", [i64::MAX - 1]).unwrap();
                }
            }
            let table = EditTable::open(&path, "roads").unwrap();
            let before = table.read_editable(MAX_EDITABLE).unwrap();
            let mut changed = before.features[0].clone();
            changed.attrs[0] = Value::Text("partial".into());
            let edits = Edits {
                updates: vec![(changed, before.versions[0].1)],
                inserts: vec![Feature { id: 100, geometry: None, attrs: vec![] }, Feature { id: 101, geometry: None, attrs: vec![] }],
                ..Default::default()
            };
            let reserved = if high_water_source == "reserved" { (i64::MAX - 1).cast_unsigned() } else { 1 };
            let error = table.save(&edits, &table.info().fields, [reserved].into_iter(), &before.revision).unwrap_err();
            assert!(error.to_string().contains("no positive INTEGER PRIMARY KEY"), "{high_water_source}: {error}");
            let after = table.read_editable(MAX_EDITABLE).unwrap();
            assert_eq!(after.features, before.features, "{high_water_source}");
            assert_eq!(after.versions, before.versions, "{high_water_source}");
            assert_eq!(after.matched, before.matched, "{high_water_source}");
            assert_eq!(conn.query_row("SELECT last_change FROM gpkg_contents", [], |r| r.get::<_, String>(0)).unwrap(), before.revision);
            if high_water_source == "sequence" {
                assert_eq!(
                    conn.query_row("SELECT seq FROM sqlite_sequence WHERE name = 'roads'", [], |r| r.get::<_, i64>(0)).unwrap(),
                    i64::MAX - 1
                );
            }
        }
    }

    #[test]
    fn gpkg_declared_geometry_mismatches_roll_back_updates_and_inserts() {
        for (ty, geometry) in [
            ("POINT", line(4.0)),
            ("LINESTRING", Geometry::Point(Coord::new(4.0, 5.0))),
            ("POLYGON", line(4.0)),
            ("MULTIPOINT", line(4.0)),
            ("MULTILINESTRING", Geometry::Point(Coord::new(4.0, 5.0))),
            ("MULTIPOLYGON", line(4.0)),
            ("GEOMETRYCOLLECTION", Geometry::Point(Coord::new(4.0, 5.0))),
        ] {
            let (_dir, path) = fixture();
            let conn = Connection::open(&path).unwrap();
            conn.execute("UPDATE gpkg_geometry_columns SET geometry_type_name = ?1", [ty]).unwrap();
            let table = EditTable::open(&path, "roads").unwrap();
            let original = blob_at(&path, 1);
            assert!(
                table.save(&update(&table, Some(geometry.clone())), &table.info().fields, std::iter::empty(), REVISION).is_err(),
                "{ty}"
            );
            assert_eq!(blob_at(&path, 1), original, "{ty}");
            let read = table.read_editable(MAX_EDITABLE).unwrap();
            let mut changed = read.features[0].clone();
            changed.attrs[0] = Value::Text("must roll back".into());
            let inserted = Feature { id: 99, geometry: Some(geometry), attrs: vec![Value::Text("new".into()), Value::Null] };
            let edits = Edits { updates: vec![(changed, read.versions[0].1)], inserts: vec![inserted], ..Default::default() };
            assert!(table.save(&edits, &table.info().fields, std::iter::empty(), &read.revision).is_err(), "{ty}");
            let stored: (i64, String) = conn.query_row("SELECT count(*), name FROM roads", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
            assert_eq!(stored, (1, "old".into()), "{ty}");
            assert_eq!(blob_at(&path, 1), original, "{ty}");
        }
    }

    #[test]
    fn gpkg_geometry_writes_keep_the_declared_single_or_multi_type() {
        let point = Coord::new(4.0, 5.0);
        let polygon = Geometry::Polygon(vec![vec![vec![point, Coord::new(5.0, 5.0), Coord::new(5.0, 6.0), point]]]);
        for (ty, geometry, wkb_type) in [
            ("POINT", Geometry::MultiPoint(vec![point]), 1_u32),
            ("LINESTRING", line(4.0), 2),
            ("POLYGON", polygon.clone(), 3),
            ("MULTIPOINT", Geometry::Point(point), 4),
            ("MULTILINESTRING", line(4.0), 5),
            ("MULTIPOLYGON", polygon, 6),
            ("GEOMETRY", Geometry::Point(point), 1),
        ] {
            let (_dir, path) = fixture();
            Connection::open(&path).unwrap().execute("UPDATE gpkg_geometry_columns SET geometry_type_name = ?1", [ty]).unwrap();
            let table = EditTable::open(&path, "roads").unwrap();
            table.save(&update(&table, Some(geometry)), &table.info().fields, std::iter::empty(), REVISION).unwrap().unwrap();
            let bytes = blob_at(&path, 1);
            let (_, body) = split_blob(&bytes).unwrap();
            assert_eq!(body.get(1..5), Some(wkb_type.to_le_bytes().as_slice()), "{ty}");
        }
    }

    #[test]
    fn gpkg_rejects_non_integer_and_composite_primary_keys() {
        for schema in [
            "CREATE TABLE roads (fid INT PRIMARY KEY, geom BLOB, name TEXT)",
            "CREATE TABLE roads (fid INTEGER, geom BLOB, name TEXT, PRIMARY KEY (fid, name))",
            "CREATE TABLE roads (fid INTEGER, geom BLOB, name TEXT)",
        ] {
            let (_dir, path) = fixture();
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch("DROP TABLE roads").unwrap();
            conn.execute_batch(schema).unwrap();
            let result = EditTable::open(&path, "roads");
            assert!(matches!(result, Err(DataError::Format(e)) if e.contains("no INTEGER PRIMARY KEY")));
        }
    }
}
