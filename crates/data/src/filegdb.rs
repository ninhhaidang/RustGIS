//! Esri File Geodatabases (a `.gdb` folder), read without GDAL: every feature class and table, with
//! their fields and coordinate system.
//!
//! The format is ESRI's own and undocumented; this follows the description published by the GDAL
//! project (`dump_gdbtable`'s FGDB-Spec). A geodatabase is a folder of tables, `aXXXXXXXX.gdbtable`
//! (fields and rows) with `aXXXXXXXX.gdbtablx` (where each row starts), numbered by their row in
//! the system catalog, `a00000001.gdbtable`.
//!
//! Geometry is read in 2D. A multipatch (3D surfaces, such as the buildings of swissBUILDINGS3D)
//! becomes its footprint: the union of its faces seen from above, with its lowest and highest Z in
//! the fields `z_min` and `z_max`, and the difference in `height` (an extrusion stands it up by it).
//! In the browser, where a panic in the union can't be caught, a multipatch is the hull of its
//! points. Curves are drawn as straight segments between their ends.
//! Compressed tables, raster fields and the newer sparse row layouts aren't read: they give
//! [`DataError::Unsupported`], and the caller can read the file through GDAL instead.
//!
//! Files are hostile input: every count and offset is checked, rows that can't be read are skipped
//! and counted.

use crate::footprint::{Hull, P3, UNIONS};
use crate::raster::{Crs, Transformer};
use crate::{DataError, Dataset, Field, FieldType, Geometry, GeometryType, ReadReport, Result, Value};
use rustgis_geom::Coord;
use std::path::Path;

/// The largest table read (its `.gdbtable` is read whole).
pub const MAX_TABLE_BYTES: usize = if cfg!(target_arch = "wasm32") { 512 << 20 } else { 2 << 30 };
/// Most tables read from one geodatabase.
const MAX_TABLES: usize = 512;
/// Most fields a table may have.
const MAX_FIELDS: usize = 4096;
/// Most parts and points one geometry may have (its blob's length bounds them first).
const MAX_PARTS: u64 = 1 << 24;
/// Most faces of one multipatch merged into its footprint; beyond, its outline is its points' hull.
const MAX_FACES: usize = 200_000;
/// Most faces merged for one table, whatever its rows ask: past it, multipatches are hulls, so a
/// crafted table can't make loading take hours.
const MAX_TABLE_FACES: usize = if cfg!(target_arch = "wasm32") { 500_000 } else { 4_000_000 };

/// Whether a folder is a File Geodatabase: named `.gdb`, holding a system catalog.
pub fn is_geodatabase(dir: &Path) -> bool {
    crate::source::is_gdb_name(&dir.to_string_lossy()) && dir.join("a00000001.gdbtable").is_file()
}

/// What a geodatabase gave: its layers, and the tables that couldn't be read (by name, with why),
/// which GDAL may read instead.
#[derive(Debug, Default)]
pub struct Read {
    pub layers: Vec<(Dataset, ReadReport)>,
    pub unread: Vec<(String, String)>,
}

/// Reads the feature classes and tables of a geodatabase folder; `layer` picks one by name
/// (ignoring case).
pub fn read_dir(dir: &Path, layer: Option<&str>) -> Result<Read> {
    let name = dir.file_stem().and_then(|s| s.to_str()).unwrap_or("geodatabase").to_string();
    let mut out = read(&name, &|file: &str| read_file(dir, file), layer)?;
    for (ds, _) in &mut out.layers {
        ds.source = Some(dir.display().to_string());
    }
    Ok(out)
}

/// The names of a geodatabase folder's feature classes and tables, from its catalog alone.
pub fn layers(dir: &Path) -> Result<Vec<String>> {
    let name = dir.file_stem().and_then(|s| s.to_str()).unwrap_or("geodatabase");
    let read = |file: &str| -> Result<Vec<u8>> {
        read_file(dir, file)?.ok_or_else(|| DataError::Format(format!("{name}: not a File Geodatabase (no {file})")))
    };
    Ok(own_tables(catalog(name, &read("a00000001.gdbtable")?, &read("a00000001.gdbtablx")?)?).map(|(_, n, _)| n).collect())
}

/// One file of a geodatabase folder, `None` when it isn't there; one past [`MAX_TABLE_BYTES`] is
/// refused before it is read.
fn read_file(dir: &Path, file: &str) -> Result<Option<Vec<u8>>> {
    let path = dir.join(file);
    match std::fs::metadata(&path) {
        Ok(m) if m.len() > MAX_TABLE_BYTES as u64 => Err(DataError::Unsupported(format!("{file} is too large to read here"))),
        Ok(_) => Ok(Some(std::fs::read(&path)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// The tables the catalog lists that hold the geodatabase's data, not its own bookkeeping
/// (`GDB_…`) nor nothing at all: their number, name and whether they are compressed. What
/// [`layers`] lists and [`read`] reads (or says it can't).
fn own_tables(listed: Vec<(u64, String, Option<f64>)>) -> impl Iterator<Item = (u64, String, bool)> {
    listed
        .into_iter()
        .filter(|(_, n, _)| !n.starts_with("GDB_"))
        .filter_map(|(row, n, format)| match format {
            None | Some(0.0) => Some((row, n, false)),
            Some(1.0) => Some((row, n, true)),
            _ => None,
        })
        .take(MAX_TABLES)
}

/// Each table the system catalog lists: its number, name and storage (0: a table of its own;
/// 1: compressed; 2: none).
fn catalog(name: &str, t: &[u8], x: &[u8]) -> Result<Vec<(u64, String, Option<f64>)>> {
    let table = Table::parse(t)?;
    let rows = row_offsets(x)?;
    let format_at = table.field("FileFormat");
    let Some(name_at) = table.field("Name") else {
        return Err(DataError::Format(format!("{name}: the system catalog has no Name field")));
    };
    Ok(rows
        .into_iter()
        .filter_map(|(row, offset)| {
            let values = table.row(t, offset)?;
            let Some(Value::Text(name)) = values.get(name_at).map(|v| v.value.clone()) else { return None };
            Some((row, name, format_at.and_then(|i| values.get(i)).and_then(|v| v.value.as_f64())))
        })
        .collect())
}

/// Reads the feature classes and tables of a geodatabase whose files `open` gives by name
/// (`a00000001.gdbtable`; `None` for a file it doesn't have).
pub fn read(name: &str, open: &dyn Fn(&str) -> Result<Option<Vec<u8>>>, layer: Option<&str>) -> Result<Read> {
    let t = open("a00000001.gdbtable")?.ok_or_else(|| DataError::Format(format!("{name}: not a File Geodatabase (no system catalog)")))?;
    let x = open("a00000001.gdbtablx")?.ok_or_else(|| DataError::Format(format!("{name}: the system catalog has no row index")))?;
    let listed = catalog(name, &t, &x)?;
    // The coordinate system codes the item definitions give, by table name.
    let wkids = listed
        .iter()
        .find(|(_, n, _)| n == "GDB_Items")
        .and_then(|(row, _, _)| {
            let file = format!("a{row:08x}");
            Some(wkids(&open(&format!("{file}.gdbtable")).ok()??, &open(&format!("{file}.gdbtablx")).ok()??))
        })
        .unwrap_or_default();
    let mut out = Read::default();
    for (row, table_name, compressed) in own_tables(listed) {
        if layer.is_some_and(|l| !l.eq_ignore_ascii_case(&table_name)) {
            continue;
        }
        if compressed {
            out.unread.push((table_name, "it is compressed".into()));
            continue;
        }
        // A table that can't be opened (too large, not readable) is said; the others are read.
        let file = format!("a{row:08x}");
        let files = open(&format!("{file}.gdbtable")).and_then(|t| Ok((t, open(&format!("{file}.gdbtablx"))?)));
        let (t, x) = match files {
            Ok((Some(t), Some(x))) => (t, x),
            // Listed, but its files aren't there: said by name, as `layers` lists it.
            Ok((t, x)) => {
                let missing: Vec<String> = [(t.is_none(), "gdbtable"), (x.is_none(), "gdbtablx")]
                    .iter()
                    .filter(|(gone, _)| *gone)
                    .map(|(_, e)| format!("{file}.{e}"))
                    .collect();
                let why = match missing.as_slice() {
                    [one] => format!("its file {one} is missing"),
                    _ => format!("its files {} are missing", missing.join(" and ")),
                };
                out.unread.push((table_name, why));
                continue;
            }
            Err(e) => {
                out.unread.push((table_name, e.to_string()));
                continue;
            }
        };
        // Items that share the table's name but disagree on the code give none: the WKT decides.
        let mut codes = wkids.iter().filter(|(n, _)| n.eq_ignore_ascii_case(&table_name)).map(|&(_, c)| c);
        let wkid = codes.next().filter(|&c| codes.all(|d| d == c));
        match read_table(&table_name, &t, &x, wkid) {
            Ok(Some(read)) => out.layers.push(read),
            Ok(None) => {}
            Err(e) => out.unread.push((table_name, e.to_string())),
        }
    }
    if out.layers.is_empty() {
        return Err(match (out.unread.first(), layer) {
            (Some((table, why)), _) => DataError::Unsupported(format!("{name}: {table}: {why}")),
            (None, Some(l)) => DataError::Format(format!("{name}: no layer '{l}'")),
            (None, None) => DataError::Format(format!("{name}: no feature classes or tables")),
        });
    }
    // A note several layers share is said once.
    let mut said: Vec<String> = Vec::new();
    for (_, report) in &mut out.layers {
        report.messages.retain(|m| !said.contains(m));
        said.extend(report.messages.iter().cloned());
    }
    Ok(out)
}

/// The coordinate system code (`LatestWKID`, else `WKID`) of each item `GDB_Items` defines, by
/// name: the WKT a table holds is Esri's, which names some projections differently.
fn wkids(t: &[u8], x: &[u8]) -> Vec<(String, u32)> {
    let (Ok(table), Ok(rows)) = (Table::parse(t), row_offsets(x)) else { return Vec::new() };
    let (Some(name_at), Some(def_at)) = (table.field("Name"), table.field("Definition")) else { return Vec::new() };
    let tag = |xml: &str, tag: &str| -> Option<u32> {
        let open = format!("<{tag}>");
        let start = xml.find(&open)? + open.len();
        xml.get(start..)?.split('<').next()?.trim().parse().ok()
    };
    rows.into_iter()
        .filter_map(|(_, offset)| {
            let values = table.row(t, offset)?;
            let Some(Value::Text(name)) = values.get(name_at).map(|v| v.value.clone()) else { return None };
            let Some(Value::Text(def)) = values.get(def_at).map(|v| &v.value) else { return None };
            Some((name, tag(def, "LatestWKID").or_else(|| tag(def, "WKID"))?))
        })
        .collect()
}

/// The coordinate system Esri data names: by its EPSG or Esri `code` (a WKID), else from its `wkt`,
/// with the transformer to longitude/latitude. `(None, None)` for longitude/latitude, and for a
/// system that can't be read, which `report` says; coordinates are then read as longitude/latitude.
/// The File Geodatabase and ArcGIS protocol buffer readers share it.
pub(crate) fn esri_crs(code: Option<u32>, wkt: Option<&str>, report: &mut ReadReport) -> (Option<Crs>, Option<Transformer>) {
    // By its code, else from the WKT.
    let by_code = code.and_then(crs_of_code).and_then(|c| match c.transformer() {
        Ok(t) if c != Crs::LonLat => Some((Some(c), Some(t))),
        Ok(_) => Some((None, None)),
        Err(_) => None,
    });
    match (by_code, wkt) {
        (Some(by_code), _) => by_code,
        (None, Some(wkt)) => match crate::raster::wkt::read_wkt(wkt) {
            Ok((c, note)) => {
                if let Some(n) = note {
                    report.messages.push(format!("coordinate system: {n}"));
                }
                match c.transformer() {
                    Ok(t) if c != Crs::LonLat => (Some(c), Some(t)),
                    Ok(_) => (None, None),
                    Err(e) => {
                        report.messages.push(format!("coordinate system: {e}; coordinates are read as longitude/latitude"));
                        (None, None)
                    }
                }
            }
            Err(e) => {
                report.messages.push(format!("coordinate system: {e}; coordinates are read as longitude/latitude"));
                (None, None)
            }
        },
        (None, None) => (None, None),
    }
}

/// A coordinate system by its EPSG or Esri code.
pub(crate) fn crs_of_code(code: u32) -> Option<Crs> {
    Crs::from_epsg(code).or_else(|| {
        let proj4 = crate::raster::crs::esri_proj4(code)?;
        Crs::custom(crate::raster::crs::CustomCrs {
            proj4: proj4.to_string(),
            label: format!("ESRI:{code}"),
            geokeys: Vec::new(),
            doubles: Vec::new(),
            ascii: String::new(),
        })
        .ok()
    })
}

/// One table as a dataset: `None` for a table with no rows and no fields worth showing. `wkid` is
/// its coordinate system's code, when the item definitions give one.
fn read_table(name: &str, t: &[u8], x: &[u8], wkid: Option<u32>) -> Result<Option<(Dataset, ReadReport)>> {
    let table = Table::parse(t)?;
    let rows = row_offsets(x)?;
    let mut report = ReadReport::default();
    let layer_type = match table.shape {
        _ if table.geometry.is_none() => GeometryType::None,
        Shape::Point | Shape::Multipoint => GeometryType::Point,
        Shape::Polyline => GeometryType::Polyline,
        Shape::Polygon | Shape::Multipatch => GeometryType::Polygon,
        Shape::None => GeometryType::None,
        Shape::Other(code) => return Err(DataError::Unsupported(format!("geometry type {code}"))),
    };
    let wkt = table.geometry.as_ref().map(|g| g.wkt.as_str()).filter(|w| !w.is_empty() && !w.starts_with('{'));
    let (crs, to_lonlat) = esri_crs(wkid.filter(|_| table.geometry.is_some()), wkt, &mut report);
    // The attribute fields: every field but the geometry, and the object ID first.
    let mut fields: Vec<Field> = Vec::new();
    let mut columns: Vec<usize> = Vec::new();
    for (i, f) in table.fields.iter().enumerate() {
        let ty = match f.kind {
            Kind::Geometry | Kind::Binary | Kind::Raster => continue,
            Kind::Int16 | Kind::Int32 | Kind::Int64 | Kind::ObjectId => FieldType::Integer,
            Kind::Float32 | Kind::Float64 => FieldType::Double,
            Kind::Text | Kind::Xml | Kind::Guid | Kind::GlobalId | Kind::Date | Kind::DateOnly | Kind::TimeOnly | Kind::DateTimeOffset => {
                FieldType::Text
            }
        };
        let alias = (!f.alias.is_empty() && f.alias != f.name).then(|| f.alias.clone());
        fields.push(Field { name: f.name.clone(), ty, alias });
        columns.push(i);
    }
    let patches = table.shape == Shape::Multipatch;
    // A multipatch's lowest and highest Z, and the difference (what an extrusion stands it up by),
    // in three fields after the table's own (named apart from them).
    let z_at = patches.then(|| {
        let free = |base: &str| -> String {
            let taken = |n: &str| fields.iter().any(|f| f.name.eq_ignore_ascii_case(n));
            (0..100)
                .map(|k| if k == 0 { base.to_string() } else { format!("{base}_{k}") })
                .find(|n| !taken(n))
                .unwrap_or_else(|| base.to_string())
        };
        let names = [free("z_min"), free("z_max"), free("height")];
        let at = fields.len();
        fields.extend(names.into_iter().map(|name| Field { name, ty: FieldType::Double, alias: None }));
        at
    });
    if rows.is_empty() && fields.is_empty() {
        return Ok(None);
    }
    let mut ds = Dataset::new(name, layer_type, fields);
    // A multipatch's heights are kept as fields and noted below; its measures aren't.
    let zm = table.geometry.is_some() && (table.has_m || (table.has_z && table.shape != Shape::Multipatch));
    ds.crs = crs;
    let geometry_at = table.fields.iter().position(|f| f.kind == Kind::Geometry);
    let (mut curves, mut wall_hulls, mut unmerged, mut unprojected) = (0usize, 0usize, 0usize, 0usize);
    // Multipatches with points but no footprint: too few, all on one line, or not finite numbers.
    let mut no_footprint = 0usize;
    // Shapes decoded, a multipatch's too (its M values go even when it has no footprint).
    let mut decoded = 0usize;
    let mut budget = MAX_TABLE_FACES;
    for (row, offset) in rows {
        let Some(mut values) = table.row(t, offset) else {
            report.skipped += 1;
            continue;
        };
        let mut attrs: Vec<Value> = Vec::with_capacity(ds.fields.len());
        for &c in &columns {
            attrs.push(match table.fields.get(c).map(|f| f.kind) {
                Some(Kind::ObjectId) => Value::Int(i64::try_from(row).unwrap_or(i64::MAX)),
                // Taken, not cloned: each column is read once, and the geometry's blob stays.
                _ => values.get_mut(c).map(|v| std::mem::replace(&mut v.value, Value::Null)).unwrap_or(Value::Null),
            });
        }
        let mut geometry = None;
        if let (Some(g), Some(def)) = (geometry_at.and_then(|i| values.get(i)).and_then(|v| v.blob.as_ref()), table.geometry.as_ref()) {
            match decode_geometry(g, def, &mut budget) {
                Ok(Decoded::Geometry(geom, curved)) => {
                    decoded += 1;
                    curves += curved;
                    geometry = Some(geom);
                }
                Ok(Decoded::Patch(patch)) => {
                    decoded += 1;
                    if let (Some((lo, hi)), Some(at)) = (patch.z, z_at) {
                        attrs.resize(at, Value::Null);
                        attrs.extend([Value::Float(lo), Value::Float(hi), Value::Float(hi - lo)]);
                    }
                    wall_hulls += usize::from(patch.hull == Some(Hull::NoFaces));
                    unmerged += usize::from(patch.hull == Some(Hull::Unmerged));
                    no_footprint += usize::from(patch.footprint.is_none() && patch.has_points);
                    geometry = patch.footprint;
                }
                Ok(Decoded::Empty) => {}
                Err(()) => report.skipped += 1,
            }
        }
        let geometry = match (geometry, &to_lonlat) {
            (Some(g), Some(t)) => {
                let g = to_lonlat_geometry(g, t);
                unprojected += usize::from(g.is_none());
                g
            }
            (g, _) => g,
        };
        ds.push_next(geometry, attrs);
    }
    if curves > 0 {
        report.messages.push(format!("{curves} curved segments are drawn as straight lines"));
    }
    if unprojected > 0 {
        report
            .messages
            .push(format!("{unprojected} features have coordinates outside their coordinate system: they are kept without geometry"));
    }
    if zm {
        // The table declares them for every shape.
        ds.drop_zm(&mut report, decoded);
    }
    if patches {
        report.messages.push("3D multipatches are drawn as their footprints, with their lowest and highest Z in z_min and z_max, and the difference in height".into());
    }
    if no_footprint > 0 {
        report.messages.push(format!(
            "{no_footprint} multipatches have no footprint (their points don't outline an area: fewer than three, all on one line, or not finite numbers): they are kept without geometry"
        ));
    }
    if wall_hulls > 0 {
        report.messages.push(format!("{wall_hulls} multipatches with no faces seen from above are drawn as the outline of their points"));
    }
    if unmerged > 0 {
        report.messages.push(if cfg!(target_arch = "wasm32") {
            format!("{unmerged} multipatches are drawn as the outline of their points (the desktop app draws their footprints)")
        } else {
            format!("{unmerged} multipatches whose faces couldn't be merged are drawn as the outline of their points")
        });
    }
    Ok(Some((ds, report)))
}

/// Projects a geometry to longitude/latitude; `None` when a point can't be.
fn to_lonlat_geometry(g: Geometry, t: &Transformer) -> Option<Geometry> {
    let mut ok = true;
    let out = g.map_coords(&|c| t.to_lonlat(c).unwrap_or(Coord::new(f64::NAN, f64::NAN)));
    out.for_each_coord(|c: Coord| ok &= c.is_finite());
    ok.then_some(out)
}

// ---------------------------------------------------------------------------------------------
// Bytes

/// A reader over a byte slice that never reads past its end.
struct Cur<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Cur<'a> {
    fn new(b: &'a [u8], i: usize) -> Self {
        Cur { b, i }
    }
    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let out = self.b.get(self.i..self.i.checked_add(n)?)?;
        self.i += n;
        Some(out)
    }
    fn skip(&mut self, n: usize) -> Option<()> {
        self.bytes(n).map(|_| ())
    }
    fn u8(&mut self) -> Option<u8> {
        self.bytes(1).and_then(|b| b.first().copied())
    }
    fn arr<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.bytes(N).and_then(|b| b.try_into().ok())
    }
    fn i16(&mut self) -> Option<i16> {
        self.arr().map(i16::from_le_bytes)
    }
    fn i32(&mut self) -> Option<i32> {
        self.arr().map(i32::from_le_bytes)
    }
    fn u32(&mut self) -> Option<u32> {
        self.arr().map(u32::from_le_bytes)
    }
    fn i64(&mut self) -> Option<i64> {
        self.arr().map(i64::from_le_bytes)
    }
    fn f32(&mut self) -> Option<f32> {
        self.arr().map(f32::from_le_bytes)
    }
    fn f64(&mut self) -> Option<f64> {
        self.arr().map(f64::from_le_bytes)
    }
    /// An unsigned little-endian base-128 number (at most 64 bits).
    fn varuint(&mut self) -> Option<u64> {
        let mut v: u64 = 0;
        for shift in (0..64).step_by(7) {
            let part = u64::from(self.u8()?);
            // Bits past the 64th are refused, not dropped.
            if shift > 57 && (part & 0x7F) >> (64 - shift) != 0 {
                return None;
            }
            v |= (part & 0x7F) << shift;
            if part & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    }
    /// A signed base-128 number: the first byte's 0x40 bit is the sign.
    fn varint(&mut self) -> Option<i64> {
        let c = self.u8()?;
        let negative = c & 0x40 != 0;
        let mut v: u64 = u64::from(c & 0x3F);
        let mut more = c & 0x80 != 0;
        let mut shift = 6u32;
        while more {
            let part = u64::from(self.u8()?);
            // Bits past the 64th are refused, not dropped.
            if shift >= 64 || (shift > 57 && (part & 0x7F) >> (64 - shift) != 0) {
                return None;
            }
            v |= (part & 0x7F) << shift;
            shift += 7;
            more = part & 0x80 != 0;
        }
        // A magnitude past `i64::MAX` (which `-i64::MIN` would overflow) is refused.
        let v = i64::try_from(v).ok()?;
        Some(if negative { -v } else { v })
    }
    /// A count, bounded by `max` and by the bytes left (each item takes at least `min_bytes`).
    fn count(&mut self, max: u64, min_bytes: usize) -> Option<usize> {
        let n = self.varuint()?;
        let left = (self.b.len().saturating_sub(self.i) / min_bytes.max(1)) as u64;
        (n <= max && n <= left.max(1)).then(|| usize::try_from(n).ok()).flatten()
    }
}

fn utf16(b: &[u8]) -> String {
    let units: Vec<u16> = b.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
    String::from_utf16_lossy(&units)
}

// ---------------------------------------------------------------------------------------------
// Tables

/// A field's type.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Int16,
    Int32,
    Float32,
    Float64,
    Text,
    Date,
    ObjectId,
    Geometry,
    Binary,
    Raster,
    Guid,
    GlobalId,
    Xml,
    Int64,
    DateOnly,
    TimeOnly,
    DateTimeOffset,
}

impl Kind {
    fn of(code: u8) -> Option<Kind> {
        Some(match code {
            0 => Kind::Int16,
            1 => Kind::Int32,
            2 => Kind::Float32,
            3 => Kind::Float64,
            4 => Kind::Text,
            5 => Kind::Date,
            6 => Kind::ObjectId,
            7 => Kind::Geometry,
            8 => Kind::Binary,
            9 => Kind::Raster,
            10 => Kind::Guid,
            11 => Kind::GlobalId,
            12 => Kind::Xml,
            13 => Kind::Int64,
            14 => Kind::DateOnly,
            15 => Kind::TimeOnly,
            16 => Kind::DateTimeOffset,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug)]
struct FieldDef {
    name: String,
    alias: String,
    kind: Kind,
    /// Its bit in each row's null flags, for a field that can be null.
    null_bit: Option<usize>,
}

/// How a geometry field's coordinates are stored.
#[derive(Clone, Debug)]
struct GeomDef {
    wkt: String,
    x0: f64,
    y0: f64,
    xy_scale: f64,
    z0: f64,
    z_scale: f64,
}

/// A table's kind of geometry.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Shape {
    None,
    Point,
    Multipoint,
    Polyline,
    Polygon,
    Multipatch,
    Other(u8),
}

struct Table {
    fields: Vec<FieldDef>,
    geometry: Option<GeomDef>,
    shape: Shape,
    /// Strings are UTF-8 (else UTF-16).
    utf8: bool,
    /// How many fields have a bit in each row's null flags.
    nullable: usize,
    /// The geometries carry Z values (which reading drops, but a multipatch keeps as fields).
    has_z: bool,
    /// The geometries carry M values (which reading drops).
    has_m: bool,
}

/// One field's value in a row: a value, or a geometry's bytes.
struct Cell<'a> {
    value: Value,
    blob: Option<&'a [u8]>,
}

impl Table {
    /// Reads a table's header: its fields, geometry kind and definition, and whether geometries
    /// carry Z or M values.
    fn parse(t: &[u8]) -> Result<Table> {
        let bad = |what: &str| DataError::Format(format!("unreadable table ({what})"));
        let mut c = Cur::new(t, 32);
        let at = c.i64().and_then(|o| usize::try_from(o).ok()).ok_or_else(|| bad("header"))?;
        let mut c = Cur::new(t, at);
        c.skip(8).ok_or_else(|| bad("field header"))?;
        let flags = c.u32().ok_or_else(|| bad("field header"))?;
        let count = c.i16().and_then(|n| usize::try_from(n).ok()).filter(|&n| n <= MAX_FIELDS).ok_or_else(|| bad("field count"))?;
        let has_z = flags & 0x8000_0000 != 0;
        let has_m = flags & 0x4000_0000 != 0;
        let shape = match flags & 0xFF {
            0 => Shape::None,
            1 => Shape::Point,
            2 => Shape::Multipoint,
            3 => Shape::Polyline,
            4 => Shape::Polygon,
            9 => Shape::Multipatch,
            other => Shape::Other(other as u8),
        };
        let mut fields = Vec::with_capacity(count);
        let mut geometry = None;
        for _ in 0..count {
            let field = (|| -> Option<(FieldDef, Option<GeomDef>)> {
                let n = usize::from(c.u8()?);
                let name = utf16(c.bytes(n * 2)?);
                let n = usize::from(c.u8()?);
                let alias = utf16(c.bytes(n * 2)?);
                let code = c.u8()?;
                let kind = Kind::of(code)?;
                let mut geom = None;
                let flag = match kind {
                    Kind::Text => {
                        c.skip(4)?;
                        let flag = c.u8()?;
                        let default = usize::try_from(c.varuint()?).ok()?;
                        if flag & 4 != 0 {
                            c.skip(default)?;
                        }
                        flag
                    }
                    Kind::ObjectId => {
                        c.skip(1)?;
                        c.u8()?
                    }
                    Kind::Geometry => {
                        c.skip(1)?;
                        let flag = c.u8()?;
                        let n = usize::try_from(c.i16()?).ok()?;
                        let wkt = utf16(c.bytes(n)?);
                        let g = c.u8()?;
                        let (z, m) = (g & 2 != 0, g & 4 != 0);
                        let (x0, y0, xy_scale) = (c.f64()?, c.f64()?, c.f64()?);
                        if m {
                            c.skip(16)?;
                        }
                        let (z0, z_scale) = if z { (c.f64()?, c.f64()?) } else { (0.0, 1.0) };
                        // Tolerances, then the extent (and its Z and M ranges).
                        c.skip(8 + if m { 8 } else { 0 } + if z { 8 } else { 0 } + 32)?;
                        if has_z {
                            c.skip(16)?;
                        }
                        if has_m {
                            c.skip(16)?;
                        }
                        // The spatial index's grid sizes.
                        c.skip(1)?;
                        let grids = usize::try_from(c.u32()?).ok().filter(|&n| n <= 3)?;
                        c.skip(grids * 8)?;
                        let usable = |s: f64| s.is_finite() && s > 0.0;
                        if !usable(xy_scale) || !x0.is_finite() || !y0.is_finite() || (z && (!usable(z_scale) || !z0.is_finite())) {
                            return None;
                        }
                        geom = Some(GeomDef { wkt, x0, y0, xy_scale, z0, z_scale });
                        flag
                    }
                    Kind::Binary | Kind::Guid | Kind::GlobalId | Kind::Xml => {
                        c.skip(1)?;
                        c.u8()?
                    }
                    // Raster fields have a layout of their own; such tables are left to GDAL.
                    Kind::Raster => return None,
                    _ => {
                        c.skip(1)?;
                        let flag = c.u8()?;
                        let default = usize::from(c.u8()?);
                        if flag & 4 != 0 {
                            c.skip(default)?;
                        }
                        flag
                    }
                };
                let nullable = kind != Kind::ObjectId && flag & 1 != 0;
                Some((FieldDef { name, alias, kind, null_bit: nullable.then_some(0) }, geom))
            })();
            let Some((f, g)) = field else {
                return Err(DataError::Unsupported("a field this reader doesn't know (a raster field?)".into()));
            };
            if g.is_some() && geometry.is_none() {
                geometry = g;
            }
            fields.push(f);
        }
        // The null flags have a bit for each field that can be null, in order.
        let mut nullable = 0usize;
        for f in &mut fields {
            if f.null_bit.is_some() {
                f.null_bit = Some(nullable);
                nullable += 1;
            }
        }
        Ok(Table { fields, geometry, shape, utf8: flags & 0x100 != 0, nullable, has_z, has_m })
    }

    fn field(&self, name: &str) -> Option<usize> {
        self.fields.iter().position(|f| f.name.eq_ignore_ascii_case(name))
    }

    /// A row's values, by field (the object ID's is `Null`: it isn't stored). `None` when the row
    /// can't be read.
    fn row<'a>(&self, t: &'a [u8], offset: usize) -> Option<Vec<Cell<'a>>> {
        let mut c = Cur::new(t, offset);
        let size = usize::try_from(c.i32()?).ok()?;
        let body = c.bytes(size)?;
        let mut c = Cur::new(body, 0);
        let nulls = c.bytes(self.nullable.div_ceil(8))?;
        let mut out = Vec::with_capacity(self.fields.len());
        for f in &self.fields {
            let null = f.null_bit.is_some_and(|bit| nulls.get(bit / 8).is_some_and(|b| b >> (bit % 8) & 1 == 1));
            if null || f.kind == Kind::ObjectId {
                out.push(Cell { value: Value::Null, blob: None });
                continue;
            }
            let text = |b: &[u8]| if self.utf8 { String::from_utf8_lossy(b).into_owned() } else { utf16(b) };
            let cell = |value: Value| Cell { value, blob: None };
            out.push(match f.kind {
                Kind::Int16 => cell(Value::Int(i64::from(c.i16()?))),
                Kind::Int32 => cell(Value::Int(i64::from(c.i32()?))),
                Kind::Int64 => cell(Value::Int(c.i64()?)),
                Kind::Float32 => cell(float(f64::from(c.f32()?))),
                Kind::Float64 => cell(float(c.f64()?)),
                Kind::Text | Kind::Xml => {
                    let n = usize::try_from(c.varuint()?).ok()?;
                    cell(Value::Text(text(c.bytes(n)?)))
                }
                Kind::Date => cell(date(c.f64()?, "YYYY-MM-DDTHH:mm:ss")),
                Kind::DateOnly => cell(date(c.f64()?, "YYYY-MM-DD")),
                Kind::TimeOnly => cell(date(c.f64()?, "HH:mm:ss")),
                Kind::DateTimeOffset => {
                    let days = c.f64()?;
                    let minutes = c.i16()?;
                    cell(match date(days, "YYYY-MM-DDTHH:mm:ss") {
                        Value::Text(s) => Value::Text(format!(
                            "{s}{}{:02}:{:02}",
                            if minutes < 0 { '-' } else { '+' },
                            minutes.unsigned_abs() / 60,
                            minutes.unsigned_abs() % 60
                        )),
                        v => v,
                    })
                }
                Kind::Guid | Kind::GlobalId => cell(Value::Text(guid(c.bytes(16)?))),
                Kind::Geometry => {
                    let n = usize::try_from(c.varuint()?).ok()?;
                    Cell { value: Value::Null, blob: Some(c.bytes(n)?) }
                }
                Kind::Binary => {
                    let n = usize::try_from(c.varuint()?).ok()?;
                    c.skip(n)?;
                    cell(Value::Null)
                }
                Kind::ObjectId | Kind::Raster => cell(Value::Null),
            });
        }
        Some(out)
    }
}

fn float(v: f64) -> Value {
    if v.is_finite() { Value::Float(v) } else { Value::Null }
}

/// Days since 1899-12-30 as text in `fmt`.
fn date(days: f64, fmt: &str) -> Value {
    // 1970-01-01 is day 25,569.
    let ms = (days - 25_569.0) * 86_400_000.0;
    if ms.is_finite() && ms.abs() < 1e16 { Value::Text(crate::time::format(ms, fmt)) } else { Value::Null }
}

/// A GUID as `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}` (its first three groups little-endian).
fn guid(b: &[u8]) -> String {
    let h = |r: &[usize]| r.iter().filter_map(|&i| b.get(i)).map(|x| format!("{x:02X}")).collect::<String>();
    format!("{{{}-{}-{}-{}-{}}}", h(&[3, 2, 1, 0]), h(&[5, 4]), h(&[7, 6]), h(&[8, 9]), h(&[10, 11, 12, 13, 14, 15]))
}

/// Where each row starts in its table, from the `.gdbtablx`: `(object ID, offset)` for every row
/// not deleted.
fn row_offsets(x: &[u8]) -> Result<Vec<(u64, usize)>> {
    let bad = |what: &str| DataError::Format(format!("unreadable row index ({what})"));
    let mut c = Cur::new(x, 0);
    let version = c.i32().ok_or_else(|| bad("header"))?;
    let blocks = c.i32().and_then(|n| usize::try_from(n).ok()).ok_or_else(|| bad("header"))?;
    let rows_v3 = c.i32().ok_or_else(|| bad("header"))?;
    let width = c.i32().and_then(|n| usize::try_from(n).ok()).filter(|w| (4..=6).contains(w)).ok_or_else(|| bad("offset size"))?;
    let stored = blocks.checked_mul(1024).ok_or_else(|| bad("block count"))?;
    let trailer = stored.checked_mul(width).and_then(|n| n.checked_add(16)).filter(|&n| n <= x.len()).ok_or_else(|| bad("block count"))?;
    let mut c = Cur::new(x, trailer);
    // Which 1024-row blocks are stored, when some are left out; and how many rows there are.
    let (bitmap, total): (Option<&[u8]>, usize) = match version {
        3 => {
            let words = c.i32().and_then(|n| usize::try_from(n).ok()).unwrap_or(0);
            let total = usize::try_from(rows_v3).map_err(|_| bad("row count"))?;
            if words == 0 {
                (None, total)
            } else {
                c.skip(12).ok_or_else(|| bad("bitmap"))?;
                (Some(c.bytes(words.checked_mul(4).ok_or_else(|| bad("bitmap"))?).ok_or_else(|| bad("bitmap"))?), total)
            }
        }
        4 => {
            let total = c.i64().and_then(|n| usize::try_from(n).ok()).ok_or_else(|| bad("row count"))?;
            let varying = c.i32().ok_or_else(|| bad("trailer"))?;
            if varying != 0 {
                return Err(DataError::Unsupported("a row index with missing blocks".into()));
            }
            (None, total)
        }
        _ => return Err(DataError::Unsupported(format!("row index version {version}"))),
    };
    // Block by block (1024 rows each): a block the bitmap leaves out is passed over whole, and no
    // more blocks are looked at than are stored or the bitmap has bits for, whatever the row count
    // in the header says.
    let in_blocks = match bitmap {
        None => total.min(stored).div_ceil(1024),
        Some(bits) => total.div_ceil(1024).min(bits.len().saturating_mul(8)),
    };
    let mut out = Vec::new();
    let mut stored_before = 0usize;
    for block in 0..in_blocks {
        let present = bitmap.is_none_or(|bits| bits.get(block / 8).is_some_and(|b| b >> (block % 8) & 1 == 1));
        if !present {
            continue;
        }
        if stored_before >= blocks {
            break;
        }
        let first = stored_before * 1024;
        stored_before += 1;
        for k in 0..1024 {
            let row = block * 1024 + k;
            if row >= total {
                break;
            }
            let at = 16 + (first + k) * width;
            let Some(bytes) = x.get(at..at + width) else { break };
            let offset = bytes.iter().rev().fold(0u64, |v, &b| v << 8 | u64::from(b));
            if offset == 0 {
                continue;
            }
            if let Ok(offset) = usize::try_from(offset) {
                out.push((row as u64 + 1, offset));
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Geometry

enum Decoded {
    /// A geometry, and how many curved segments it draws straight.
    Geometry(Geometry, usize),
    Patch(Patch),
    Empty,
}

/// A multipatch seen from above.
struct Patch {
    footprint: Option<Geometry>,
    /// Its lowest and highest Z.
    z: Option<(f64, f64)>,
    /// Why the footprint is the hull of its points, when it is.
    hull: Option<Hull>,
    /// Whether it has any points. One with none has nothing to outline: it isn't counted as lacking a footprint.
    has_points: bool,
}

const HAS_Z: u64 = 0x8000_0000;
const HAS_CURVES: u64 = 0x2000_0000;

/// `budget` is how many more faces the table's multipatches may merge (see [`MAX_TABLE_FACES`]).
fn decode_geometry(b: &[u8], def: &GeomDef, budget: &mut usize) -> std::result::Result<Decoded, ()> {
    let mut c = Cur::new(b, 0);
    let ty = c.varuint().ok_or(())?;
    let x = |v: f64| v / def.xy_scale + def.x0;
    let y = |v: f64| v / def.xy_scale + def.y0;
    match ty & 0xFF {
        0 => Ok(Decoded::Empty),
        // Points (2D, Z, M, ZM, and the general type).
        1 | 9 | 11 | 21 | 52 => {
            let (vx, vy) = (c.varuint().ok_or(())?, c.varuint().ok_or(())?);
            if vx == 0 || vy == 0 {
                return Ok(Decoded::Empty);
            }
            Ok(Decoded::Geometry(Geometry::Point(Coord::new(x((vx - 1) as f64), y((vy - 1) as f64))), 0))
        }
        // Multipoints.
        8 | 18 | 20 | 28 | 53 => {
            let n = c.count(MAX_PARTS, 2).ok_or(())?;
            for _ in 0..4 {
                c.varuint().ok_or(())?;
            }
            let pts = deltas(&mut c, n, def)?;
            if pts.is_empty() { Ok(Decoded::Empty) } else { Ok(Decoded::Geometry(Geometry::MultiPoint(pts), 0)) }
        }
        // Lines and polygons (and the general types, with curves).
        code @ (3 | 10 | 13 | 23 | 50 | 5 | 15 | 19 | 25 | 51) => {
            let general = code == 50 || code == 51;
            let polygon = matches!(code, 5 | 15 | 19 | 25 | 51);
            let n = c.count(MAX_PARTS, 2).ok_or(())?;
            let parts = c.count(MAX_PARTS, 1).ok_or(())?;
            let curves = if general && ty & HAS_CURVES != 0 { c.count(MAX_PARTS, 1).ok_or(())? } else { 0 };
            for _ in 0..4 {
                c.varuint().ok_or(())?;
            }
            let sizes = part_sizes(&mut c, n, parts)?;
            let pts = deltas(&mut c, n, def)?;
            let rings = split(&pts, &sizes);
            if rings.is_empty() {
                return Ok(Decoded::Empty);
            }
            let geom = if polygon {
                // Clockwise rings are outer; each counterclockwise one is a hole in the one before.
                // A ring with no area (its points on a line) is neither, and is left out.
                let rings: Vec<(Vec<Coord>, bool)> = rings
                    .into_iter()
                    .filter_map(|r| {
                        let a = signed_area(&r);
                        (a != 0.0 && a.is_finite()).then_some((r, a < 0.0))
                    })
                    .collect();
                if rings.is_empty() {
                    return Ok(Decoded::Empty);
                }
                crate::io::group_rings(rings)
            } else {
                Geometry::Line(rings)
            };
            Ok(Decoded::Geometry(geom, curves))
        }
        // Multipatches: 3D surfaces (the general type, or the Shapefile ones GDAL writes, which
        // always have Z).
        code @ (31 | 32 | 54) => {
            let has_z = code != 54 || ty & HAS_Z != 0;
            let n = c.count(MAX_PARTS, 2).ok_or(())?;
            c.varuint().ok_or(())?;
            let parts = c.count(MAX_PARTS, 1).ok_or(())?;
            for _ in 0..4 {
                c.varuint().ok_or(())?;
            }
            let sizes = part_sizes(&mut c, n, parts)?;
            let mut kinds = Vec::with_capacity(parts);
            for _ in 0..parts {
                kinds.push(c.varuint().ok_or(())? & 0xF);
            }
            let pts = deltas(&mut c, n, def)?;
            // Heights, when stored (a multipatch without them lies flat).
            let mut zs = Vec::with_capacity(if has_z { n } else { 0 });
            if has_z {
                let mut dz: i64 = 0;
                for _ in 0..n {
                    dz = dz.checked_add(c.varint().ok_or(())?).ok_or(())?;
                    zs.push(dz as f64 / def.z_scale + def.z0);
                }
            }
            let z = zs.iter().fold(None, |r: Option<(f64, f64)>, &v| match r {
                _ if !v.is_finite() => r,
                None => Some((v, v)),
                Some((lo, hi)) => Some((lo.min(v), hi.max(v))),
            });
            let pts: Vec<P3> = pts.iter().enumerate().map(|(i, p)| [p.x, p.y, zs.get(i).copied().unwrap_or(0.0)]).collect();
            let (footprint, hull) = footprint(&split(&pts, &sizes), &kinds, budget);
            Ok(Decoded::Patch(Patch { footprint, z, hull, has_points: !pts.is_empty() }))
        }
        _ => Err(()),
    }
}

/// The point counts of `parts` parts holding `n` points: all but the last are stored.
fn part_sizes(c: &mut Cur, n: usize, parts: usize) -> std::result::Result<Vec<usize>, ()> {
    if parts == 0 {
        return Ok(Vec::new());
    }
    let mut sizes = Vec::with_capacity(parts);
    let mut sum = 0usize;
    for _ in 1..parts {
        let s = c.count(MAX_PARTS, 1).ok_or(())?;
        sum = sum.checked_add(s).ok_or(())?;
        sizes.push(s);
    }
    sizes.push(n.checked_sub(sum).ok_or(())?);
    Ok(sizes)
}

/// `n` points stored as deltas from the one before (across parts).
fn deltas(c: &mut Cur, n: usize, def: &GeomDef) -> std::result::Result<Vec<Coord>, ()> {
    let (mut dx, mut dy) = (0i64, 0i64);
    let mut pts = Vec::with_capacity(n);
    for _ in 0..n {
        dx = dx.checked_add(c.varint().ok_or(())?).ok_or(())?;
        dy = dy.checked_add(c.varint().ok_or(())?).ok_or(())?;
        pts.push(Coord::new(dx as f64 / def.xy_scale + def.x0, dy as f64 / def.xy_scale + def.y0));
    }
    Ok(pts)
}

/// Points cut into parts of the given sizes (sizes past the points are cut short).
fn split<T: Clone>(pts: &[T], sizes: &[usize]) -> Vec<Vec<T>> {
    let mut out = Vec::with_capacity(sizes.len());
    let mut at = 0usize;
    for &s in sizes {
        let end = at.saturating_add(s).min(pts.len());
        if let Some(part) = pts.get(at..end).filter(|p| !p.is_empty()) {
            out.push(part.to_vec());
        }
        at = end;
    }
    out
}

/// Twice the signed area of a ring (positive counterclockwise).
pub(crate) fn signed_area(ring: &[Coord]) -> f64 {
    // From the first point: products of projected coordinates in the millions (or more) would round
    // a small ring's area away.
    let o = ring.first().copied().unwrap_or(Coord::new(0.0, 0.0));
    let local = |c: &Coord| (c.x - o.x, c.y - o.y);
    ring.iter()
        .zip(ring.iter().cycle().skip(1))
        .map(|(a, b)| {
            let ((ax, ay), (bx, by)) = (local(a), local(b));
            ax * by - bx * ay
        })
        .sum()
}

/// A multipatch's faces, each its outer ring and then its holes: triangles from strips, fans and
/// triangle lists, and rings. At most [`MAX_FACES`]; the flag says some were left out.
fn faces(parts: &[Vec<P3>], kinds: &[u64]) -> (Vec<Vec<Vec<P3>>>, bool) {
    let mut faces: Vec<Vec<Vec<P3>>> = Vec::new();
    let mut truncated = false;
    for (part, &kind) in parts.iter().zip(kinds) {
        // Faces past the cap are left out, within a part too (one strip can hold millions).
        let room = MAX_FACES.saturating_sub(faces.len());
        let has = match kind {
            0 | 1 => part.len().saturating_sub(2),
            6 => part.len() / 3,
            3 => 0,
            _ => 1,
        };
        if has > room {
            truncated = true;
        }
        if room == 0 {
            break;
        }
        match kind {
            // Triangle strip, fan, triangles.
            0 => faces.extend(part.windows(3).take(room).map(|w| vec![w.to_vec()])),
            1 => {
                if let Some((first, rest)) = part.split_first() {
                    faces.extend(
                        rest.windows(2)
                            .filter_map(|w| match w {
                                [a, b] => Some(vec![vec![*first, *a, *b]]),
                                _ => None,
                            })
                            .take(room),
                    );
                }
            }
            6 => faces.extend(part.as_chunks::<3>().0.iter().take(room).map(|w| vec![w.to_vec()])),
            // An inner ring is a hole in the face before.
            3 => match faces.last_mut() {
                Some(face) => face.push(part.clone()),
                None => faces.push(vec![part.clone()]),
            },
            // Outer, first and plain rings.
            _ => faces.push(vec![part.clone()]),
        }
    }
    (faces, truncated)
}

/// A multipatch's footprint (see [`crate::footprint::footprint`]): the union of its faces seen
/// from above, or the hull of its points. More than [`MAX_FACES`] faces are never merged.
fn footprint(parts: &[Vec<P3>], kinds: &[u64], budget: &mut usize) -> (Option<Geometry>, Option<Hull>) {
    // Where no union runs (the browser), the faces aren't built at all: the hull stands in.
    let (faces, truncated) = if UNIONS { faces(parts, kinds) } else { (Vec::new(), false) };
    crate::footprint::footprint(&faces, truncated, parts, budget)
}

#[cfg(test)]
mod tests;
