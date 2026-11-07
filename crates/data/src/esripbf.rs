//! Answers of ArcGIS feature-service queries as protocol buffers (`f=pbf`): one page of a layer,
//! with the fields, values and geometries the same query gives as JSON, in fewer bytes and faster
//! to decode.
//!
//! Written from Esri's published description of the format, `FeatureCollection.proto` in
//! <https://github.com/Esri/arcgis-pbf> (Apache-2.0), after Terrio's ESRIPBF reader
//! (<https://github.com/opengeos/terrio>, MIT; see ATTRIBUTION.md), adapted to RustGIS's features.
//!
//! - Coordinates are integers, scaled and translated by the answer's `transform`. X and Y are
//!   zigzag differences from the vertex before, starting over at each part; Z and M are each
//!   vertex's own. RustGIS keeps neither Z nor M, and says so.
//! - Polygon rings are clockwise when outer and counterclockwise when holes in the outer ring
//!   before them, as Esri writes them and as [`crate::filegdb`] reads them. A ring with no area is
//!   left out, and said.
//! - The coordinate system is the answer's (`latestWkid`, `wkid`, else its WKT), read as
//!   [`crate::filegdb`] reads one; geometry is turned to longitude/latitude.
//! - Fields: integers, object ids and dates are integers, dates in milliseconds since 1970 (UTC),
//!   as the same query gives them as GeoJSON; single and double precision numbers are doubles;
//!   strings, GUIDs, XML, date-only, time-only and offset timestamps are text, as the service
//!   sends them. Geometry, blob and raster fields are left out, and said; a type the format adds
//!   later is read as text, and said.
//! - Features are numbered by their place, as the GeoJSON reader numbers them; the object id stays
//!   a field ([`Page::object_id_field`]).
//!
//! Refused, by name: an empty answer (a service refuses a query that way), a JSON answer (the
//! service's error, or another format), a count, ids or an extent, curves, shape buffers,
//! multipatches, envelopes, a point with more than one vertex, coordinates without a transform,
//! fields and rows that would take more than 512 MB, geometries that would take more than 1 GB, and
//! text values (field names and the WKT included) that would take more than 512 MB. Answers are
//! hostile input: every length is checked, and nothing is read past the end.

use crate::{DataError, Dataset, Field, FieldType, Geometry, GeometryType, ReadReport, Result, Value};
use rustgis_geom::Coord;

fn invalid(message: impl std::fmt::Display) -> DataError {
    DataError::Format(format!("{PREFIX}{message}"))
}

/// What every error of this reader starts with.
const PREFIX: &str = "ArcGIS protocol buffer: ";

/// Most memory an answer's fields and rows may take, whatever its size: a few bytes can declare
/// a field, or a feature, and each field, each feature and each of its values (sent or not) takes
/// memory of its own. A page is bounded by its service's record limit, far below this.
const MAX_ROW_BYTES: usize = 512 << 20;

/// Most memory an answer's geometries may take, whatever its size: a vertex of two one-byte
/// numbers takes 16 bytes, and each part a list of its own.
const MAX_GEOMETRY_BYTES: usize = 1 << 30;

/// What a part takes of [`MAX_GEOMETRY_BYTES`] before its vertices: its slot in the list of parts,
/// which grows by doubling (so up to two), and a block for its vertices, which an allocator rounds
/// up to about 32 bytes at least.
const PART_BYTES: usize = 2 * std::mem::size_of::<Vec<Coord>>() + 32;

/// Most memory an answer's text values may take, whatever its size: a number of a few bytes
/// written as text can take hundreds (1e300 is 301 digits), and bytes that aren't UTF-8 three
/// times their size.
const MAX_TEXT_BYTES: usize = 512 << 20;

/// The memory `fields` declared fields and `features` rows of their values take, before their
/// text and geometries.
fn row_bytes(features: usize, fields: usize) -> usize {
    use std::mem::size_of;
    // Declared, kept or not, as a column, a field and a name to tell repeats by.
    let field = size_of::<Declared>() + size_of::<Option<(usize, Kind)>>() + size_of::<Field>() + size_of::<String>();
    let row = size_of::<crate::Feature>().saturating_add(fields.saturating_mul(size_of::<Value>()));
    features.saturating_mul(row).saturating_add(fields.saturating_mul(field))
}

fn too_many_rows(features: usize, fields: usize) -> DataError {
    invalid(format!("{features} features of {fields} fields would take more than {} MB", MAX_ROW_BYTES >> 20))
}

/// The format's geometry types (`GeometryType`).
const POINT: u64 = 0;
const MULTIPOINT: u64 = 1;
const POLYLINE: u64 = 2;
const POLYGON: u64 = 3;
const MULTIPATCH: u64 = 4;
const ENVELOPE: u64 = 5;

/// The format's object id field type (`esriFieldTypeOID`).
const OID: u64 = 6;

/// The last field type the format defines today (`esriFieldTypeTimestampOffset`); a later one is
/// read as text, and said.
const LAST_FIELD_TYPE: u64 = 16;

/// One page of a query's answer.
#[derive(Debug)]
pub struct Page {
    /// Its features, in longitude/latitude.
    pub dataset: Dataset,
    /// What was left out or assumed while reading it, but for what its features lost
    /// ([`losses`](Self::losses)).
    pub report: ReadReport,
    /// The service holds more features than it sent (`exceededTransferLimit`): the next page
    /// starts after these.
    pub exceeded_transfer_limit: bool,
    /// The field holding the features' object ids (`esriFieldTypeOID`), by its place in
    /// `dataset.fields`.
    pub object_id_field: Option<usize>,
    /// What its features lost, counted: a reader joining pages sums them and says each once
    /// ([`Losses::messages`]), as [`read`] says a single page's.
    pub losses: Losses,
    /// About the memory its fields, rows, text values and geometries take, in bytes, as counted
    /// against [`MAX_ROW_BYTES`], [`MAX_TEXT_BYTES`] and [`MAX_GEOMETRY_BYTES`]: a reader joining
    /// pages sums them against a budget of its own.
    pub memory: usize,
    /// Features it holds past the limit it was read within ([`read_page_within`]), not read: what
    /// they lost isn't counted.
    pub left_out: usize,
    /// A hash of how many features it holds and of its first and last features as they were sent,
    /// taken before any limit: a reader of pages tells a page sent again by it when the features
    /// have no object id.
    pub fingerprint: u64,
}

/// What a page's features lost or had assumed, counted.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Losses {
    /// Features with a geometry, in an answer naming no coordinate system.
    pub no_crs: bool,
    /// Values that don't fit their field's type.
    pub unreadable: usize,
    /// Values or field names that aren't valid UTF-8.
    pub not_utf8: usize,
    /// Polygon rings without area.
    pub flat_rings: usize,
    /// Features whose coordinates are outside their coordinate system.
    pub unprojected: usize,
    /// Features with Z or M values, dropped.
    pub with_zm: usize,
}

impl Losses {
    /// Adds another page's.
    pub fn add(&mut self, other: &Losses) {
        self.no_crs |= other.no_crs;
        self.unreadable = self.unreadable.saturating_add(other.unreadable);
        self.not_utf8 = self.not_utf8.saturating_add(other.not_utf8);
        self.flat_rings = self.flat_rings.saturating_add(other.flat_rings);
        self.unprojected = self.unprojected.saturating_add(other.unprojected);
        self.with_zm = self.with_zm.saturating_add(other.with_zm);
    }

    /// What they say, one message per kind (as [`read`] says them).
    pub fn messages(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.no_crs {
            out.push("the answer names no coordinate system; coordinates are read as longitude/latitude".into());
        }
        if self.unreadable > 0 {
            out.push(format!("{} values don't fit their field's type and are left empty", self.unreadable));
        }
        if self.not_utf8 > 0 {
            out.push(format!(
                "{} values or field names aren't valid UTF-8; the bytes that can't be read are replaced with U+FFFD",
                self.not_utf8
            ));
        }
        if self.flat_rings > 0 {
            out.push(format!("{} polygon rings have no area and are left out", self.flat_rings));
        }
        if self.unprojected > 0 {
            out.push(format!(
                "{} features have coordinates outside their coordinate system: they are kept without geometry",
                self.unprojected
            ));
        }
        if self.with_zm > 0 {
            out.push(crate::zm_note(Some(self.with_zm as u64)));
        }
        out
    }
}

/// Reads an answer as a layer named `name`, saying when the service held more features than it sent.
pub fn read(bytes: &[u8], name: &str) -> Result<(Dataset, ReadReport)> {
    let mut page = read_page(bytes, name)?;
    page.report.messages.extend(page.losses.messages());
    if page.exceeded_transfer_limit {
        page.report.messages.push(format!(
            "the service sent {} features only (its record limit); the rest aren't in this answer",
            page.dataset.features.len()
        ));
    }
    Ok((page.dataset, page.report))
}

/// Whether some bytes start as an answer: an optional version, then a query result holding a
/// feature result, a count, object ids or an extent, whose first field is one the format defines
/// for it. A count, ids or an extent are told so that reading them says why they're refused. An
/// OpenStreetMap or vector tile protocol buffer isn't an answer.
pub fn is_esripbf(bytes: &[u8]) -> bool {
    signs(bytes).unwrap_or(false)
}

/// Reads one page of an answer as a layer named `name`.
pub fn read_page(bytes: &[u8], name: &str) -> Result<Page> {
    read_page_within(bytes, name, usize::MAX)
}

/// Reads one page of an answer as a layer named `name`, its first `limit` features only (the
/// rest are counted in [`Page::left_out`]): a reader joining pages up to a number of features
/// counts what those it keeps lost, and no others.
pub fn read_page_within(bytes: &[u8], name: &str, limit: usize) -> Result<Page> {
    if bytes.is_empty() {
        return Err(invalid("the answer is empty: the service refused the query (ask for it with f=json to see why)"));
    }
    // A service may answer in JSON whatever was asked for: its error, or another format.
    if is_json(bytes) {
        return Err(json_service_error(bytes).unwrap_or_else(|| invalid("the answer is JSON, not a protocol buffer")));
    }
    if bytes.len() as u64 > crate::io::MAX_BYTES {
        return Err(invalid(format!("the answer is too large ({} bytes)", bytes.len())));
    }
    let result = feature_result(bytes)?;

    let (mut more, mut has_z, mut has_m) = (false, false, false);
    let mut geometry_type = None;
    let mut reference = None;
    let mut transform = Transform::default();
    let mut declared = Vec::new();
    let mut features = Vec::new();
    let mut tally = Tally::default();
    let mut r = Reader::new(result);
    while !r.done() {
        match r.key()? {
            (7, 0) => geometry_type = Some(r.varint()?),
            (8, 2) => reference = Some(r.message()?),
            (9, 0) => more = r.varint()? != 0,
            (10, 0) => has_z = r.varint()? != 0,
            (11, 0) => has_m = r.varint()? != 0,
            (12, 2) => transform = read_transform(r.message()?)?,
            // Checked as fields and features come, in any order, since even an empty one takes
            // memory, with or without the other.
            (13, 2) => declared.push(field(r.message()?, &mut tally)?),
            (15, 2) => features.push(r.message()?),
            (_, wire) => {
                r.skip(wire)?;
                continue;
            }
        }
        if row_bytes(features.len(), declared.len()) > MAX_ROW_BYTES {
            return Err(too_many_rows(features.len(), declared.len()));
        }
    }
    let fingerprint = {
        use std::hash::Hasher as _;
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        hash.write_usize(features.len());
        for f in [features.first(), features.last()].into_iter().flatten() {
            hash.write(f);
        }
        hash.finish()
    };
    let left_out = features.len().saturating_sub(limit);
    features.truncate(limit);
    let rows = row_bytes(features.len(), declared.len());
    // A point layer's type is the format's default, which an answer leaves out.
    let shape_kind = geometry_type.unwrap_or(POINT);
    // Refused whether or not a feature has coordinates, so such a layer isn't read as empty.
    match shape_kind {
        MULTIPATCH => return Err(invalid("multipatch geometries aren't read")),
        ENVELOPE => return Err(invalid("envelope geometries aren't read")),
        _ => {}
    }

    let mut report = ReadReport::default();
    // Values come by position, one per declared field; a column left out keeps its place.
    let mut columns: Vec<Option<(usize, Kind)>> = Vec::with_capacity(declared.len());
    let mut fields: Vec<Field> = Vec::new();
    let mut names = std::collections::HashSet::new();
    let (mut repeated, mut not_read, mut unknown) = (0usize, 0usize, 0usize);
    let mut object_id_field = None;
    for Declared { name, number, utf8, alias } in declared {
        tally.not_utf8 += usize::from(!utf8);
        let Some(read_as) = kind(number) else {
            not_read += 1;
            columns.push(None);
            continue;
        };
        unknown += usize::from(number > LAST_FIELD_TYPE);
        // Fields are found by name whatever its case, so the first of a name keeps it.
        if !names.insert(name.to_lowercase()) {
            repeated += 1;
            columns.push(None);
            continue;
        }
        if number == OID && object_id_field.is_none() {
            object_id_field = Some(fields.len());
        }
        columns.push(Some((fields.len(), read_as)));
        fields.push(Field { name, ty: read_as.field_type(), alias });
    }
    if repeated > 0 {
        report.messages.push(format!("{repeated} fields repeat an earlier field's name and are left out"));
    }
    if not_read > 0 {
        report.messages.push(format!("{not_read} fields hold geometries, blobs or rasters, which aren't read, and are left out"));
    }
    if unknown > 0 {
        report.messages.push(format!("{unknown} fields have a type RustGIS doesn't know; their values are read as text"));
    }
    let (code, wkt, wkt_utf8) = match reference {
        Some(bytes) => spatial_reference(bytes, &mut tally)?,
        None => (None, None, true),
    };
    // Only when the WKT is what names the system: a code RustGIS knows goes first.
    if !wkt_utf8 && code.and_then(crate::filegdb::crs_of_code).is_none() {
        report.messages.push("the coordinate system's WKT isn't valid UTF-8; the bytes that can't be read are replaced with U+FFFD".into());
    }
    let (crs, to_lonlat) = crate::filegdb::esri_crs(code, wkt.as_deref(), &mut report)?;
    if crs.is_none()
        && wkt.is_none()
        && let Some(code) = code
        && crate::filegdb::crs_of_code(code) != Some(crate::raster::Crs::LonLat)
    {
        report.messages.push(format!("coordinate system {code} can't be used; coordinates are read as longitude/latitude"));
    }

    let layer_type = match shape_kind {
        POINT | MULTIPOINT => GeometryType::Point,
        POLYLINE => GeometryType::Polyline,
        POLYGON => GeometryType::Polygon,
        _ => GeometryType::None,
    };
    let shape = Shape { kind: shape_kind, transform: &transform, has_z, has_m };
    let mut ds = Dataset::new(name, GeometryType::None, fields);
    let width = ds.fields.len();
    let (mut any_geometry, mut with_zm, mut unprojected) = (false, 0usize, 0usize);
    for (n, bytes) in features.into_iter().enumerate() {
        let (values, geometry) = feature(bytes, &columns, width, &shape, &mut tally).map_err(|e| match e {
            DataError::Format(m) => invalid(format!("feature {}: {}", n + 1, m.strip_prefix(PREFIX).unwrap_or(&m))),
            e => e,
        })?;
        any_geometry |= geometry.is_some();
        with_zm += usize::from(geometry.is_some() && (has_z || has_m));
        let geometry = match (geometry, &to_lonlat) {
            (Some(g), Some(t)) => {
                let g = crate::io::geometry_to_lonlat(&g, t);
                unprojected += usize::from(g.is_none());
                g
            }
            (g, _) => g,
        };
        ds.push_next(geometry, values);
    }
    if geometry_type.is_some() || any_geometry {
        ds.geometry_type = layer_type;
    }
    ds.crs = crs;
    // What couldn't be converted can't come back by defining the projection again.
    ds.crs_incomplete = unprojected > 0;
    let losses = Losses {
        // Assumed, so said: an answer that names no coordinate system.
        no_crs: any_geometry && code.is_none() && wkt.is_none(),
        unreadable: tally.unreadable,
        not_utf8: tally.not_utf8,
        flat_rings: tally.flat_rings,
        unprojected,
        with_zm,
    };
    ds.zm_dropped = with_zm > 0;
    let memory = rows
        .saturating_add(MAX_GEOMETRY_BYTES.saturating_sub(tally.geometry_room))
        .saturating_add(MAX_TEXT_BYTES.saturating_sub(tally.text_room));
    Ok(Page { dataset: ds, report, exceeded_transfer_limit: more, object_id_field, losses, memory, left_out, fingerprint })
}

/// A reader of protocol buffer fields, which refuses what runs past its bytes.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn done(&self) -> bool {
        self.at >= self.bytes.len()
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.at)
    }

    fn varint(&mut self) -> Result<u64> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = *self.bytes.get(self.at).ok_or_else(|| invalid("the data ends inside a number"))?;
            self.at += 1;
            // The tenth byte holds the 64th bit alone.
            if shift == 63 && byte & 0x7e != 0 {
                return Err(invalid("a number is too long"));
            }
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(invalid("a number is too long"))
    }

    /// A field's number and wire type.
    fn key(&mut self) -> Result<(u64, u8)> {
        let key = self.varint()?;
        Ok((key >> 3, (key & 7) as u8))
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.at.checked_add(n).ok_or_else(|| invalid("a field's length is too large"))?;
        let taken = self.bytes.get(self.at..end).ok_or_else(|| invalid("the data ends inside a field"))?;
        self.at = end;
        Ok(taken)
    }

    /// A length-delimited field's bytes.
    fn message(&mut self) -> Result<&'a [u8]> {
        let n = usize::try_from(self.varint()?).map_err(|_| invalid("a field's length is too large"))?;
        self.take(n)
    }

    fn fixed<const N: usize>(&mut self) -> Result<[u8; N]> {
        <[u8; N]>::try_from(self.take(N)?).map_err(|_| invalid("the data ends inside a field"))
    }

    fn double(&mut self) -> Result<f64> {
        Ok(f64::from_le_bytes(self.fixed()?))
    }

    fn skip(&mut self, wire: u8) -> Result<()> {
        match wire {
            0 => self.varint().map(|_| ()),
            1 => self.take(8).map(|_| ()),
            2 => self.message().map(|_| ()),
            5 => self.take(4).map(|_| ()),
            w => Err(invalid(format!("unknown wire type {w}"))),
        }
    }
}

fn zigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

/// A reader at an answer's query result, past its optional version: `None` when the bytes start
/// otherwise. The query result's length is left to read.
fn at_query_result(bytes: &[u8]) -> Result<Option<Reader<'_>>> {
    let mut r = Reader::new(bytes);
    let mut key = r.key()?;
    if key == (1, 2) {
        let version = r.message()?;
        if version.len() > 32 || !version.iter().all(u8::is_ascii_graphic) {
            return Ok(None);
        }
        key = r.key()?;
    }
    Ok((key == (2, 2)).then_some(r))
}

fn signs(bytes: &[u8]) -> Result<bool> {
    let Some(mut r) = at_query_result(bytes)? else { return Ok(false) };
    // These lengths may run past the bytes given: only the start of an answer may be.
    let query = r.varint()?;
    // A feature result (1), a count (2), object ids (3) or an extent and its count (4).
    let (holds, wire) = r.key()?;
    if wire != 2 || !(1..=4).contains(&holds) {
        return Ok(false);
    }
    let result = r.varint()?;
    if result >= query {
        return Ok(false);
    }
    // A count of zero sends nothing.
    if result == 0 {
        return Ok(holds == 2);
    }
    let (field, wire) = r.key()?;
    Ok(match (holds, field) {
        (1, 1..=6 | 8 | 12..=16) => wire == 2,
        (1, 7 | 9..=11) => wire == 0,
        // The count.
        (2, 1) => wire == 0,
        // The object id field's name, how the server makes them, the ids (packed or not).
        (3, 1 | 2) => wire == 2,
        (3, 3) => wire == 0 || wire == 2,
        // The extent, the count.
        (4, 1) => wire == 2,
        (4, 2) => wire == 0,
        _ => false,
    })
}

/// The bytes of an answer's feature result.
fn feature_result(bytes: &[u8]) -> Result<&[u8]> {
    let mut result = None;
    let mut top = Reader::new(bytes);
    while !top.done() {
        match top.key()? {
            (2, 2) => {
                let mut query = Reader::new(top.message()?);
                while !query.done() {
                    match query.key()? {
                        (1, 2) => result = Some(query.message()?),
                        (2..=4, 2) => return Err(invalid("the answer holds a count, ids or an extent, not features")),
                        (_, wire) => query.skip(wire)?,
                    }
                }
            }
            (_, wire) => top.skip(wire)?,
        }
    }
    result.ok_or_else(|| invalid("the answer holds no features"))
}

/// Whether an answer is JSON, as a service may answer whatever was asked for.
fn is_json(bytes: &[u8]) -> bool {
    bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes).trim_ascii_start().first() == Some(&b'{')
}

/// A service's error answer (`{"error": {"code": …, "message": …}}`, its members in any order),
/// found without reading the rest of the document.
fn json_service_error(bytes: &[u8]) -> Option<DataError> {
    use serde::de::{Error as _, MapAccess, Visitor};
    #[derive(Default, serde::Deserialize)]
    struct ServiceError {
        message: Option<String>,
        code: Option<i64>,
    }
    struct RootError<'a>(&'a mut Option<ServiceError>);
    impl<'de> Visitor<'de> for RootError<'_> {
        type Value = ();
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a JSON object")
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<(), A::Error> {
            while let Some(key) = map.next_key::<String>()? {
                if key == "error" {
                    *self.0 = Some(map.next_value::<Option<ServiceError>>()?.unwrap_or_default());
                    // Found: the rest isn't read.
                    return Err(A::Error::custom("service error found"));
                }
                map.next_value::<serde::de::IgnoredAny>()?;
            }
            Ok(())
        }
    }
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let mut found = None;
    let _ = serde::Deserializer::deserialize_map(&mut serde_json::Deserializer::from_slice(bytes), RootError(&mut found));
    found.map(|error| {
        let message = error.message.as_deref().unwrap_or("no message");
        let code = error.code.map(|c| format!(" ({c})")).unwrap_or_default();
        invalid(format!("the service answered with an error{code}: {message}"))
    })
}

/// How an answer's integers become coordinates.
struct Transform {
    /// The answer gave one: the format sends one with every answer that has coordinates.
    given: bool,
    upper_left: bool,
    /// X, Y, M and Z, as the format orders them.
    scale: [f64; 4],
    translate: [f64; 4],
}

impl Default for Transform {
    fn default() -> Self {
        Self { given: false, upper_left: true, scale: [1.0; 4], translate: [0.0; 4] }
    }
}

/// The four numbers of a `Scale` or `Translate`; those absent keep `absent`.
fn four(bytes: &[u8], absent: f64) -> Result<[f64; 4]> {
    let mut out = [absent; 4];
    let mut r = Reader::new(bytes);
    while !r.done() {
        match r.key()? {
            (field @ 1..=4, 1) => {
                let value = r.double()?;
                if let Some(slot) = usize::try_from(field).ok().and_then(|f| f.checked_sub(1)).and_then(|i| out.get_mut(i)) {
                    *slot = value;
                }
            }
            (_, wire) => r.skip(wire)?,
        }
    }
    Ok(out)
}

fn read_transform(bytes: &[u8]) -> Result<Transform> {
    let mut t = Transform { given: true, ..Transform::default() };
    let mut r = Reader::new(bytes);
    while !r.done() {
        match r.key()? {
            (1, 0) => t.upper_left = r.varint()? == 0,
            // A scale of zero is one the service left out.
            (2, 2) => t.scale = four(r.message()?, 1.0)?.map(|s| if s == 0.0 { 1.0 } else { s }),
            (3, 2) => t.translate = four(r.message()?, 0.0)?,
            (_, wire) => r.skip(wire)?,
        }
    }
    Ok(t)
}

/// A field as an answer declares it.
struct Declared {
    name: String,
    /// Its type's number (`FieldType`).
    number: u64,
    /// Whether its name and alias were valid UTF-8.
    utf8: bool,
    /// None when empty or the name itself.
    alias: Option<String>,
}

/// A field as declared. The name is taken from `tally`'s text room before it's made, twice: it's
/// kept lowercase too; the alias once.
fn field(bytes: &[u8], tally: &mut Tally) -> Result<Declared> {
    let (mut name, mut number, mut utf8) = (String::new(), 0, true);
    let (mut alias, mut alias_utf8) = (String::new(), true);
    let mut r = Reader::new(bytes);
    while !r.done() {
        match r.key()? {
            (1, 2) => {
                let raw = r.message()?;
                tally.spend_text(lossy_len(raw).saturating_mul(2))?;
                utf8 = std::str::from_utf8(raw).is_ok();
                name = String::from_utf8_lossy(raw).into_owned();
            }
            (2, 0) => number = r.varint()?,
            (3, 2) => {
                let raw = r.message()?;
                tally.spend_text(lossy_len(raw))?;
                alias_utf8 = std::str::from_utf8(raw).is_ok();
                alias = String::from_utf8_lossy(raw).into_owned();
            }
            (_, wire) => r.skip(wire)?,
        }
    }
    let alias = Some(alias).filter(|a| !a.is_empty() && *a != name);
    Ok(Declared { name, number, utf8: utf8 && alias_utf8, alias })
}

/// The code of an answer's coordinate system (its `latestWkid` when RustGIS knows it, else its
/// `wkid`), its WKT and whether that WKT was valid UTF-8. The WKT is taken from the text room
/// before it's made.
fn spatial_reference(bytes: &[u8], tally: &mut Tally) -> Result<(Option<u32>, Option<String>, bool)> {
    let (mut wkid, mut latest, mut wkt, mut utf8) = (None, None, None, true);
    let mut r = Reader::new(bytes);
    while !r.done() {
        match r.key()? {
            (1, 0) => wkid = u32::try_from(r.varint()?).ok().filter(|&c| c > 0),
            (2, 0) => latest = u32::try_from(r.varint()?).ok().filter(|&c| c > 0),
            (5, 2) => {
                let raw = r.message()?;
                utf8 = std::str::from_utf8(raw).is_ok();
                tally.spend_text(lossy_len(raw))?;
                wkt = Some(String::from_utf8_lossy(raw).into_owned()).filter(|w| !w.trim().is_empty());
            }
            (_, wire) => r.skip(wire)?,
        }
    }
    let code = latest.filter(|&c| crate::filegdb::crs_of_code(c).is_some()).or(wkid).or(latest);
    Ok((code, wkt, utf8))
}

/// How a field's values are read.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Integer,
    Double,
    Text,
}

impl Kind {
    fn field_type(self) -> FieldType {
        match self {
            Kind::Integer => FieldType::Integer,
            Kind::Double => FieldType::Double,
            Kind::Text => FieldType::Text,
        }
    }
}

/// How a field of the format's type `number` is read: `None` for geometry, blob and raster fields,
/// which aren't. A type the format adds later is read as text.
fn kind(number: u64) -> Option<Kind> {
    match number {
        // Small integer, integer, date (milliseconds since 1970), object id, big integer.
        0 | 1 | 5 | OID | 13 => Some(Kind::Integer),
        // Single and double precision.
        2 | 3 => Some(Kind::Double),
        // Geometry, blob, raster.
        7..=9 => None,
        // String, GUID, global id, XML, date only, time only, timestamp with offset, and a type
        // the format adds later (said by `read_page`).
        _ => Some(Kind::Text),
    }
}

/// The length `String::from_utf8_lossy` gives `bytes`, without making it: each piece that isn't
/// UTF-8 becomes U+FFFD, three bytes.
fn lossy_len(bytes: &[u8]) -> usize {
    bytes.utf8_chunks().map(|c| c.valid().len() + if c.invalid().is_empty() { 0 } else { 3 }).sum()
}

/// What a feature's values and geometry are read with.
struct Shape<'a> {
    kind: u64,
    transform: &'a Transform,
    has_z: bool,
    has_m: bool,
}

/// What was left out of an answer's features, to say once, and the memory their geometries and
/// text may still take.
struct Tally {
    /// Values that don't fit their field's type.
    unreadable: usize,
    /// Polygon rings without area.
    flat_rings: usize,
    /// Text values and field names that aren't valid UTF-8.
    not_utf8: usize,
    /// Bytes left of [`MAX_GEOMETRY_BYTES`].
    geometry_room: usize,
    /// Bytes left of [`MAX_TEXT_BYTES`].
    text_room: usize,
}

impl Default for Tally {
    fn default() -> Self {
        Self { unreadable: 0, flat_rings: 0, not_utf8: 0, geometry_room: MAX_GEOMETRY_BYTES, text_room: MAX_TEXT_BYTES }
    }
}

impl Tally {
    /// Takes `bytes` of the geometries' memory, refusing the answer when there's no more.
    fn spend(&mut self, bytes: usize) -> Result<()> {
        self.geometry_room = self
            .geometry_room
            .checked_sub(bytes)
            .ok_or_else(|| invalid(format!("the geometries would take more than {} MB", MAX_GEOMETRY_BYTES >> 20)))?;
        Ok(())
    }

    /// Takes `bytes` of the text values' memory, refusing the answer when there's no more.
    fn spend_text(&mut self, bytes: usize) -> Result<()> {
        self.text_room = self
            .text_room
            .checked_sub(bytes)
            .ok_or_else(|| invalid(format!("the text values would take more than {} MB", MAX_TEXT_BYTES >> 20)))?;
        Ok(())
    }
}

/// A feature's values, one per kept field, and its geometry.
fn feature(
    bytes: &[u8],
    columns: &[Option<(usize, Kind)>],
    width: usize,
    shape: &Shape,
    tally: &mut Tally,
) -> Result<(Vec<Value>, Option<Geometry>)> {
    let mut values = vec![Value::Null; width];
    let mut column = 0usize;
    let mut geometry = None;
    let mut r = Reader::new(bytes);
    while !r.done() {
        match r.key()? {
            (1, 2) => {
                let cell = cell(r.message()?)?;
                let slot = *columns.get(column).ok_or_else(|| invalid("it has more values than the answer has fields"))?;
                column += 1;
                if let Some((at, kind)) = slot
                    && let Some(v) = values.get_mut(at)
                {
                    // Text sent as text is taken from the room before it's made, at its size once
                    // read; a number written as text, a few hundred bytes at most, once made.
                    if kind == Kind::Text
                        && let Cell::Text(bytes) = &cell
                    {
                        tally.spend_text(lossy_len(bytes))?;
                    }
                    let (value, fits) = value(&cell, kind);
                    if let Value::Text(s) = &value
                        && !matches!(cell, Cell::Text(_))
                    {
                        tally.spend_text(s.len())?;
                    }
                    tally.unreadable += usize::from(!fits);
                    tally.not_utf8 += usize::from(kind == Kind::Text && matches!(cell, Cell::Text(b) if std::str::from_utf8(b).is_err()));
                    *v = value;
                }
            }
            (2, 2) => geometry = read_geometry(r.message()?, shape, tally)?,
            (3, 2) => return Err(invalid("shape-buffer geometries aren't read")),
            (7, 2) => return Err(invalid("curve geometries aren't read (GeoJSON gives them as straight segments)")),
            (_, wire) => r.skip(wire)?,
        }
    }
    Ok((values, geometry))
}

/// A `Value`, as the format encodes it.
enum Cell<'a> {
    Null,
    Text(&'a [u8]),
    Int(i64),
    UInt(u64),
    Real(f64),
    Bool(bool),
}

fn cell(bytes: &[u8]) -> Result<Cell<'_>> {
    let mut out = Cell::Null;
    let mut r = Reader::new(bytes);
    while !r.done() {
        match r.key()? {
            (1, 2) => out = Cell::Text(r.message()?),
            // Through its shortest text: 0.1, not 0.10000000149011612.
            (2, 5) => out = Cell::Real(f32::from_le_bytes(r.fixed()?).to_string().parse().unwrap_or(f64::NAN)),
            (3, 1) => out = Cell::Real(r.double()?),
            // sint32 and sint64.
            (4 | 8, 0) => out = Cell::Int(zigzag(r.varint()?)),
            // uint32 and uint64.
            (5 | 7, 0) => out = Cell::UInt(r.varint()?),
            // int64.
            (6, 0) => out = Cell::Int(r.varint()? as i64),
            (9, 0) => out = Cell::Bool(r.varint()? != 0),
            (10, 0) => {
                r.varint()?;
                out = Cell::Null;
            }
            (_, wire) => r.skip(wire)?,
        }
    }
    Ok(out)
}

impl Cell<'_> {
    /// Text that is UTF-8, to read a number from: text that isn't holds no number, and isn't copied.
    fn text(&self) -> Option<&str> {
        match self {
            Cell::Text(bytes) => std::str::from_utf8(bytes).ok(),
            _ => None,
        }
    }

    /// A whole number: a real one only when it has no fraction and fits.
    fn as_i64(&self) -> Option<i64> {
        match self {
            Cell::Int(i) => Some(*i),
            Cell::UInt(u) => i64::try_from(*u).ok(),
            Cell::Real(f) => (f.is_finite() && f.fract() == 0.0 && (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(f))
                .then_some(*f as i64),
            _ => None,
        }
    }

    fn as_f64(&self) -> Option<f64> {
        match self {
            Cell::Int(i) => Some(*i as f64),
            Cell::UInt(u) => Some(*u as f64),
            Cell::Real(f) => Some(*f),
            _ => None,
        }
    }

    fn to_text(&self) -> String {
        match self {
            Cell::Text(bytes) => String::from_utf8_lossy(bytes).into_owned(),
            Cell::Int(i) => i.to_string(),
            Cell::UInt(u) => u.to_string(),
            Cell::Real(f) => f.to_string(),
            Cell::Bool(b) => b.to_string(),
            Cell::Null => String::new(),
        }
    }
}

/// A value as its field's kind reads it, and whether it fits that kind (one that doesn't is null).
fn value(cell: &Cell, kind: Kind) -> (Value, bool) {
    let read = match (kind, cell) {
        (_, Cell::Null) => return (Value::Null, true),
        (Kind::Text, c) => Some(Value::Text(c.to_text())),
        (Kind::Integer, c) => c.as_i64().or_else(|| c.text().and_then(|s| s.trim().parse().ok())).map(Value::Int),
        (Kind::Double, c) => {
            c.as_f64().or_else(|| c.text().and_then(|s| s.trim().parse::<f64>().ok())).filter(|f| f.is_finite()).map(Value::Float)
        }
    };
    match read {
        Some(v) => (v, true),
        None => (Value::Null, false),
    }
}

/// The vertices of a part: `count` of them, or those left without one. X and Y start over from
/// zero at each part; Z and M are read and left out. The part and each vertex are taken from
/// `tally`'s room.
fn part(c: &mut Reader, count: Option<usize>, shape: &Shape, tally: &mut Tally) -> Result<Vec<Coord>> {
    const VERTEX: usize = std::mem::size_of::<Coord>();
    tally.spend(PART_BYTES)?;
    let dimensions = 2 + usize::from(shape.has_z) + usize::from(shape.has_m);
    let t = shape.transform;
    let ([scale_x, scale_y, ..], [translate_x, translate_y, ..]) = (t.scale, t.translate);
    // No more than the coordinates left, nor than the room left, can hold.
    let mut vertices = Vec::with_capacity(count.unwrap_or(0).min(c.remaining() / dimensions).min(tally.geometry_room / VERTEX));
    let (mut sum_x, mut sum_y) = (0i64, 0i64);
    while count.map_or(!c.done(), |n| vertices.len() < n) {
        tally.spend(VERTEX)?;
        sum_x = sum_x.checked_add(zigzag(c.varint()?)).ok_or_else(|| invalid("a coordinate is out of range"))?;
        sum_y = sum_y.checked_add(zigzag(c.varint()?)).ok_or_else(|| invalid("a coordinate is out of range"))?;
        for _ in 2..dimensions {
            c.varint()?;
        }
        let x = translate_x + scale_x * sum_x as f64;
        let dy = scale_y * sum_y as f64;
        let y = if t.upper_left { translate_y - dy } else { translate_y + dy };
        if !(x.is_finite() && y.is_finite()) {
            return Err(invalid("a coordinate is not a finite number"));
        }
        vertices.push(Coord::new(x, y));
    }
    Ok(vertices)
}

/// Hands `found` each length of a geometry's parts, in order, as they come in its fields: packed,
/// possibly in several blocks, or one number a field.
fn each_length(bytes: &[u8], mut found: impl FnMut(u64) -> Result<()>) -> Result<()> {
    let mut r = Reader::new(bytes);
    while !r.done() {
        match r.key()? {
            (2, 2) => {
                let mut packed = Reader::new(r.message()?);
                while !packed.done() {
                    found(packed.varint()?)?;
                }
            }
            (2, 0) => found(r.varint()?)?,
            (_, wire) => r.skip(wire)?,
        }
    }
    Ok(())
}

/// A geometry. One without coordinates is no geometry.
fn read_geometry(bytes: &[u8], shape: &Shape, tally: &mut Tally) -> Result<Option<Geometry>> {
    // Whether the geometry gives its parts' lengths, and whether one of them has vertices. They
    // aren't kept: a few bytes of them would take memory of their own, so the parts are read
    // from them in a second pass.
    let (mut counted, mut with_vertices) = (false, false);
    each_length(bytes, |n| {
        counted = true;
        with_vertices |= n > 0;
        Ok(())
    })?;
    // Packed, possibly in several blocks, or one number a field. One block, the usual, isn't copied.
    let mut coords: std::borrow::Cow<[u8]> = std::borrow::Cow::Borrowed(&[]);
    let mut r = Reader::new(bytes);
    while !r.done() {
        match r.key()? {
            (3, 2) => {
                let block = r.message()?;
                if coords.is_empty() {
                    coords = std::borrow::Cow::Borrowed(block);
                } else {
                    coords.to_mut().extend_from_slice(block);
                }
            }
            (3, 0) => put_varint(coords.to_mut(), r.varint()?),
            (_, wire) => r.skip(wire)?,
        }
    }
    if coords.is_empty() {
        if with_vertices {
            return Err(invalid("a part has vertices, but the geometry has no coordinates"));
        }
        return Ok(None);
    }
    if !shape.transform.given {
        return Err(invalid("it has coordinates, but the answer has no transform for them"));
    }
    let mut c = Reader::new(&coords);
    // Each part is taken from the room as it's read; an empty part adds nothing.
    let mut parts = Vec::new();
    if counted {
        each_length(bytes, |length| {
            if length > 0 {
                // Refused at its second part, before the rest is read.
                if shape.kind == POINT && !parts.is_empty() {
                    return Err(invalid("a point has more than one vertex"));
                }
                let count = usize::try_from(length).map_err(|_| invalid("a part's length is too large"))?;
                parts.push(part(&mut c, Some(count), shape, tally)?);
            }
            Ok(())
        })?;
    } else {
        parts.push(part(&mut c, None, shape, tally)?);
    }
    if !c.done() {
        return Err(invalid("it has more coordinates than its parts count"));
    }
    Ok(match shape.kind {
        POINT => {
            let mut vertices = parts.into_iter().flatten();
            let point = vertices.next();
            if vertices.next().is_some() {
                return Err(invalid("a point has more than one vertex"));
            }
            point.map(Geometry::Point)
        }
        MULTIPOINT => {
            // One part, the usual, is the points as they are; several are joined into a list sized
            // up front, as collecting would grow by doubling while the parts are still held.
            let points = if parts.len() == 1 {
                parts.into_iter().next().unwrap_or_default()
            } else {
                let total: usize = parts.iter().map(Vec::len).sum();
                // Taken from the room too, as the parts are still held while it's filled.
                tally.spend(total.saturating_mul(std::mem::size_of::<Coord>()))?;
                let mut points = Vec::with_capacity(total);
                for p in parts {
                    points.extend(p);
                }
                points
            };
            (!points.is_empty()).then_some(Geometry::MultiPoint(points))
        }
        POLYLINE => {
            let parts: Vec<Vec<Coord>> = parts.into_iter().filter(|p| !p.is_empty()).collect();
            (!parts.is_empty()).then_some(Geometry::Line(parts))
        }
        POLYGON => polygon(parts, tally),
        kind => return Err(invalid(format!("geometries of type {kind} aren't read"))),
    })
}

/// Rings grouped into polygons by their orientation, as Esri writes them: each clockwise ring
/// starts a polygon, and each counterclockwise one is a hole in the polygon before it. Rings
/// without area are counted and left out.
fn polygon(rings: Vec<Vec<Coord>>, tally: &mut Tally) -> Option<Geometry> {
    let rings: Vec<(Vec<Coord>, bool)> = rings
        .into_iter()
        .filter(|r| !r.is_empty())
        .filter_map(|r| {
            let area = crate::filegdb::signed_area(&r);
            if area != 0.0 && area.is_finite() {
                Some((r, area < 0.0))
            } else {
                tally.flat_rings += 1;
                None
            }
        })
        .collect();
    (!rings.is_empty()).then(|| crate::io::group_rings(rings))
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8 & 0x7f) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

#[cfg(test)]
pub(crate) mod tests;
