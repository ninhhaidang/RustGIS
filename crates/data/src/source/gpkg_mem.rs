//! GeoPackage held in memory, read with the pure-Rust SQLite reader (`crate::sqlite`): how the
//! browser opens GeoPackages, where SQLite (C) isn't available. The desktop opens files with
//! SQLite (`gpkg`), which uses the R-tree index; here a read scans the table, testing the envelope
//! in each geometry's header before decoding it.

use super::{Batch, FeatureSource, Reproject, Request, Sampler, Shared, SourceInfo, clamp_limit, in_request};
use crate::sqlite::{Db, Table, Val};
use crate::{DataError, Feature, Field, FieldType, GeometryType, Result, Value, wkb};
use rustgis_geom::Extent;
use std::ops::ControlFlow;

/// A feature table of a GeoPackage held in memory.
pub struct GpkgMemSource {
    info: SourceInfo,
    db: Db<Shared>,
    table: Table,
    geom_col: usize,
    /// Column index of each field, in `info.fields` order.
    field_cols: Vec<usize>,
    reproject: Reproject,
}

fn err(e: String) -> DataError {
    DataError::Format(format!("GeoPackage: {e}"))
}

/// The rows of a small metadata table, as (column name → value) lookups.
fn meta_rows(db: &Db<Shared>, tables: &[Table], name: &str) -> Result<Vec<Vec<(String, Val)>>> {
    let t = tables
        .iter()
        .find(|t| t.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| DataError::Format(format!("not a GeoPackage: no {name} table")))?;
    let mut out = Vec::new();
    let mut too_many = false;
    db.rows(t, |_, r| {
        out.push(t.columns.iter().map(|c| c.name.to_ascii_lowercase()).zip(r.iter().cloned()).collect());
        // Metadata tables are small; a huge one is not a GeoPackage's.
        too_many = out.len() > 100_000;
        if too_many { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
    })
    .map_err(err)?;
    if too_many {
        return Err(DataError::Format(format!("GeoPackage: the {name} table has more than 100,000 rows")));
    }
    Ok(out)
}

fn get<'a>(row: &'a [(String, Val)], col: &str) -> Option<&'a Val> {
    row.iter().find(|(c, _)| c == col).map(|(_, v)| v)
}

fn text(v: Option<&Val>) -> Option<String> {
    match v? {
        Val::Text(s) => Some(s.clone()),
        _ => None,
    }
}

/// An integer value, exactly (an id, a code).
fn int(v: Option<&Val>) -> Option<i64> {
    match v? {
        Val::Int(i) => Some(*i),
        Val::Real(x) if x.fract() == 0.0 && x.abs() < 9e15 => Some(*x as i64),
        _ => None,
    }
}

fn real(v: Option<&Val>) -> Option<f64> {
    match v? {
        Val::Real(x) => Some(*x),
        Val::Int(i) => Some(*i as f64),
        _ => None,
    }
}

/// The feature tables of a GeoPackage held in memory (sorted, as the desktop lists them).
pub fn layers(bytes: Shared) -> Result<Vec<String>> {
    let db = Db::open(bytes).map_err(err)?;
    let tables = db.tables().map_err(err)?;
    let mut names: Vec<String> =
        meta_rows(&db, &tables, "gpkg_geometry_columns")?.iter().filter_map(|r| text(get(r, "table_name"))).collect();
    names.sort();
    Ok(names)
}

impl GpkgMemSource {
    /// Opens a feature table (default: the first) of the GeoPackage in `bytes`.
    pub fn open_shared(bytes: Shared, layer: Option<&str>) -> Result<Self> {
        let db = Db::open(bytes).map_err(err)?;
        // The schema, read once.
        let schema = db.tables().map_err(err)?;
        let geometry_columns = meta_rows(&db, &schema, "gpkg_geometry_columns")?;
        let mut tables: Vec<String> = geometry_columns.iter().filter_map(|r| text(get(r, "table_name"))).collect();
        tables.sort();
        let table_name = match layer {
            Some(l) => tables
                .iter()
                .find(|t| t.eq_ignore_ascii_case(l))
                .cloned()
                .ok_or_else(|| DataError::Format(format!("no feature table '{l}' (tables: {})", tables.join(", "))))?,
            None => tables.first().cloned().ok_or_else(|| DataError::Format("the GeoPackage has no feature tables".into()))?,
        };
        let mut notes = Vec::new();
        if layer.is_none() && tables.len() > 1 {
            notes.push(format!("the first of {} feature tables ({})", tables.len(), tables.join(", ")));
        }
        let gc = geometry_columns
            .iter()
            .find(|r| text(get(r, "table_name")).is_some_and(|t| t == table_name))
            .ok_or_else(|| DataError::Format("GeoPackage: the table's geometry column is missing".into()))?;
        let geom = text(get(gc, "column_name")).unwrap_or_default();
        let geom_type = text(get(gc, "geometry_type_name")).unwrap_or_default();
        let srs_id = int(get(gc, "srs_id")).unwrap_or(0);
        // The coordinate system, as the desktop reader decides it.
        let srs = meta_rows(&db, &schema, "gpkg_spatial_ref_sys").unwrap_or_default();
        let row = srs.iter().find(|r| int(get(r, "srs_id")) == Some(srs_id)).map(|r| super::GpkgSrs {
            organization: text(get(r, "organization")),
            code: int(get(r, "organization_coordsys_id")),
            definition: text(get(r, "definition")),
        });
        let reproject = super::gpkg_reproject(srs_id, row, &mut notes)?;

        let table = schema
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case(&table_name))
            .cloned()
            .ok_or_else(|| DataError::Format(format!("GeoPackage: no table '{table_name}'")))?;
        let geom_col = table
            .columns
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(&geom))
            .ok_or_else(|| DataError::Format(format!("GeoPackage: no column '{geom}' in {table_name}")))?;
        let mut fields = Vec::new();
        let mut field_cols = Vec::new();
        for (i, c) in table.columns.iter().enumerate() {
            if i == geom_col || Some(i) == table.rowid_column {
                continue;
            }
            if let Some(ty) = super::gpkg_blob::field_type(&c.decl) {
                fields.push(Field::new(c.name.clone(), ty));
                field_cols.push(i);
            }
        }
        let contents = meta_rows(&db, &schema, "gpkg_contents").unwrap_or_default();
        let declared = contents.iter().find(|r| text(get(r, "table_name")).is_some_and(|t| t == table_name)).and_then(|r| {
            Some(Extent::new(real(get(r, "min_x"))?, real(get(r, "min_y"))?, real(get(r, "max_x"))?, real(get(r, "max_y"))?))
        });
        let count = db.count(table.root).map_err(err)?;
        let geometry_type = match geom_type.to_ascii_uppercase().as_str() {
            "POINT" | "MULTIPOINT" => GeometryType::Point,
            "LINESTRING" | "MULTILINESTRING" | "CURVE" | "MULTICURVE" => GeometryType::Polyline,
            "POLYGON" | "MULTIPOLYGON" | "SURFACE" | "MULTISURFACE" => GeometryType::Polygon,
            _ => GeometryType::None,
        };
        let info = SourceInfo {
            name: table_name,
            format: "GeoPackage",
            geometry_type,
            fields,
            extent: Extent::EMPTY,
            count: Some(count),
            // A system the definition gives, by its PROJ definition, so the layer keeps it
            // (`SourceInfo::coordinate_system`), as a Shapefile's .prj does (#1476).
            crs: reproject.info_crs(),
            indexed: false,
            notes,
            zm_dropped: false,
        };
        let mut src = GpkgMemSource { info, db, table, geom_col, field_cols, reproject };
        // The extent the file declares, else the envelopes in the geometry headers.
        let native = match declared.filter(|e| !e.is_empty()) {
            Some(e) => e,
            None => src.envelopes()?,
        };
        src.info.extent = src.reproject.extent_to_lonlat(&native);
        if src.info.geometry_type == GeometryType::None {
            src.info.geometry_type = src.peek_kind();
        }
        // Z and M are 0 (none), 1 (every geometry) or 2 (optional: looked for in the first rows).
        let (z, m) = (int(get(gc, "z")).unwrap_or(0), int(get(gc, "m")).unwrap_or(0));
        if (z == 1 || m == 1) && src.info.count != Some(0) {
            // Every geometry has them; how many rows have one isn't known without a scan (an empty
            // table has none).
            src.info.drop_zm(None);
        } else if z == 2 || m == 2 {
            match src.peek_zm() {
                Some(true) => src.info.drop_zm(None),
                Some(false) => {}
                None => src.info.notes.push("couldn't tell whether it has Z or M values, which RustGIS doesn't keep".into()),
            }
        }
        Ok(src)
    }

    /// Whether any of the first few geometries has Z or M values (the scan stops there); `None`
    /// when they can't be read.
    fn peek_zm(&self) -> Option<bool> {
        let mut found = false;
        let mut looked = 0;
        let read = self.db.rows(&self.table, |_, r| {
            // Every row counts, so a table of empty geometries isn't read to its end.
            looked += 1;
            if let Some(Val::Blob(b)) = r.get(self.geom_col) {
                found |= super::gpkg_blob::split_blob(b).is_some_and(|(_, body)| wkb::has_zm(body));
            }
            if found || looked >= 16 { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
        });
        read.ok().map(|()| found)
    }

    /// The geometry type of a table declared as plain `GEOMETRY`, from its first few geometries
    /// (the scan stops there).
    fn peek_kind(&self) -> GeometryType {
        let mut kind = GeometryType::None;
        let mut looked = 0;
        let _ = self.db.rows(&self.table, |_, r| {
            looked += 1;
            if let Some(Val::Blob(b)) = r.get(self.geom_col)
                && let Some((_, body)) = super::gpkg_blob::split_blob(b)
                && let Some(g) = wkb::decode(body)
            {
                kind = g.kind();
                return ControlFlow::Break(());
            }
            if looked >= 16 { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
        });
        kind
    }

    /// The union of the envelopes in the geometry headers (native coordinates).
    fn envelopes(&self) -> Result<Extent> {
        let mut e = Extent::EMPTY;
        self.db
            .rows(&self.table, |_, r| {
                if let Some(Val::Blob(b)) = r.get(self.geom_col)
                    && let Some((env, body)) = super::gpkg_blob::split_blob(b)
                {
                    match env {
                        Some(x) => e = e.union(&x),
                        None => {
                            if let Some(g) = wkb::decode(body) {
                                e = e.union(&g.extent());
                            }
                        }
                    }
                }
                ControlFlow::Continue(())
            })
            .map_err(err)?;
        Ok(e)
    }
}

/// An attribute value of a field's type.
fn value(v: &Val, ty: FieldType) -> Value {
    match (v, ty) {
        (Val::Null, _) | (Val::Blob(_), _) => Value::Null,
        (Val::Int(i), FieldType::Boolean) => Value::Bool(*i != 0),
        (Val::Int(i), FieldType::Double) => Value::Float(*i as f64),
        (Val::Int(i), _) => Value::Int(*i),
        (Val::Real(f), FieldType::Integer) if f.fract() == 0.0 && f.abs() < 9e15 => Value::Int(*f as i64),
        (Val::Real(f), _) => Value::Float(*f),
        (Val::Text(t), FieldType::Text) => Value::Text(t.clone()),
        (Val::Text(t), ty) => Value::parse(t, ty),
    }
}

impl FeatureSource for GpkgMemSource {
    fn info(&self) -> &SourceInfo {
        &self.info
    }

    fn read(&self, req: &Request) -> Result<Batch> {
        let limit = clamp_limit(req.limit);
        let native = req.extent.map(|e| self.reproject.extent_from_lonlat(&e));
        let mut sample = if native.is_none() { Sampler::with_total(limit, self.info.count.unwrap_or(0)) } else { Sampler::new(limit) };
        let mut skipped = 0;
        self.db
            .rows(&self.table, |rowid, r| {
                let blob = match r.get(self.geom_col) {
                    Some(Val::Blob(b)) => b,
                    Some(Val::Null) | None => return ControlFlow::Continue(()),
                    Some(_) => {
                        skipped += 1;
                        return ControlFlow::Continue(());
                    }
                };
                if super::gpkg_blob::is_empty_blob(blob) {
                    return ControlFlow::Continue(());
                }
                let Some((envelope, body)) = super::gpkg_blob::split_blob(blob) else {
                    skipped += 1;
                    return ControlFlow::Continue(());
                };
                // The envelope says whether the row matches before anything is decoded; without
                // one, only the decoded geometry can, so the match is counted after it.
                if let (Some(q), Some(e)) = (&native, envelope)
                    && !e.intersects(q)
                {
                    return ControlFlow::Continue(());
                }
                let known = native.is_none() || envelope.is_some();
                if known && !sample.wants() {
                    return ControlFlow::Continue(());
                }
                let Some(geometry) = wkb::decode(body).and_then(|g| self.reproject.geometry(g)) else {
                    skipped += 1;
                    return ControlFlow::Continue(());
                };
                if !in_request(&geometry, req) || (!known && !sample.wants()) {
                    return ControlFlow::Continue(());
                }
                // Integer keys, negative ones too, map to distinct ids.
                sample.keep(Feature { id: rowid as u64, geometry: Some(geometry), attrs: self.attrs(r) });
                ControlFlow::Continue(())
            })
            .map_err(err)?;
        Ok(Batch { matched: sample.seen(), complete: sample.complete(), features: sample.items, skipped })
    }

    /// The upload is held in memory already, so every table can be visited.
    fn visits_chunks(&self) -> bool {
        true
    }

    /// A scan in table order, keeping and skipping the rows a whole read does.
    fn visit_chunks(&self, extent: Option<Extent>, bytes: usize, visit: &mut dyn FnMut(Vec<Feature>) -> Result<()>) -> Result<Batch> {
        let mut chunks = super::chunk_memory::Chunker::new(bytes, visit)?;
        let req = Request { extent, limit: usize::MAX };
        let native = extent.map(|e| self.reproject.extent_from_lonlat(&e));
        let mut out = Batch { complete: true, ..Batch::default() };
        // The visitor's error, which stops the scan.
        let mut failed = None;
        self.db
            .rows(&self.table, |rowid, r| {
                let blob = match r.get(self.geom_col) {
                    Some(Val::Blob(b)) => b,
                    Some(Val::Null) | None => return ControlFlow::Continue(()),
                    Some(_) => {
                        out.skipped += 1;
                        return ControlFlow::Continue(());
                    }
                };
                if super::gpkg_blob::is_empty_blob(blob) {
                    return ControlFlow::Continue(());
                }
                let Some((envelope, body)) = super::gpkg_blob::split_blob(blob) else {
                    out.skipped += 1;
                    return ControlFlow::Continue(());
                };
                if let (Some(q), Some(e)) = (&native, envelope)
                    && !e.intersects(q)
                {
                    return ControlFlow::Continue(());
                }
                let Some(geometry) = wkb::decode(body).and_then(|g| self.reproject.geometry(g)) else {
                    out.skipped += 1;
                    return ControlFlow::Continue(());
                };
                if !in_request(&geometry, &req) {
                    return ControlFlow::Continue(());
                }
                out.matched += 1;
                match chunks.push(Feature { id: rowid as u64, geometry: Some(geometry), attrs: self.attrs(r) }) {
                    Ok(()) => ControlFlow::Continue(()),
                    Err(e) => {
                        failed = Some(e);
                        ControlFlow::Break(())
                    }
                }
            })
            .map_err(err)?;
        if let Some(e) = failed {
            return Err(e);
        }
        chunks.flush()?;
        Ok(out)
    }
}

impl GpkgMemSource {
    /// A row's field values, in `info.fields` order.
    fn attrs(&self, r: &[Val]) -> Vec<Value> {
        self.info.fields.iter().zip(&self.field_cols).map(|(f, &c)| r.get(c).map(|v| value(v, f.ty)).unwrap_or(Value::Null)).collect()
    }
}
