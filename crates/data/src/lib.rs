//! L1: the feature model (geometry + typed attributes) and vector I/O.
//!
//! Coordinates are stored as WGS 84 longitude/latitude. Readers never panic on malformed input:
//! bad features are skipped and counted in [`ReadReport`].
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod address;
pub mod citygml;
pub mod cityjson;
mod citymodel;
pub mod color;
mod dbf;
pub mod diagnostics;
pub mod dirs;
pub mod dxf;
pub mod esripbf;
pub mod export;
pub mod filegdb;
mod footprint;
pub mod geometry;
pub mod georss;
pub mod gml;
pub mod gpx;
#[cfg(not(target_arch = "wasm32"))]
pub mod http;
pub mod interop;
pub mod io;
pub mod kml;
pub mod landxml;
pub mod lidar;
pub mod login_address;
pub mod mapinfo;
pub mod mbtiles;
pub mod memory;
pub mod ogc;
pub mod osm;
pub mod photo;
pub mod progress;
pub mod query;
pub mod raster;
pub mod s3_address;
pub mod secrets;
pub mod sheets;
pub mod source;
pub mod source_layers;
pub mod sqlite;
pub mod threads;
pub mod time;
pub mod topojson;
pub mod value;
pub mod wkb;
pub mod xml;
pub mod zip;

pub use geometry::{Geometry, GeometryType, Measure, VertexId, ambiguous_area_message};
pub use rustgis_geom::{Coord, Extent};
pub use value::{Field, FieldType, Value};

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum DataError {
    #[error("i/o: {0}")]
    Io(#[source] std::io::Error),
    #[error("{0}")]
    Format(String),
    /// The server refused the login: no password, or a wrong one.
    #[error("{0}")]
    Login(String),
    #[error("unsupported file type: {0}")]
    Unsupported(String),
    /// A Zarr slice none of whose chunks is there (the array named): a step never written,
    /// which Zarr reads as its fill value, told apart from a slice that can't be read.
    #[error("{0}: none of the slice's chunks could be read")]
    NoChunks(String),
    /// Stopped because its run was cancelled ([`progress`]).
    #[error("cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, DataError>;

/// An I/O error, or [`DataError::Cancelled`] when it carries one: a write that only returns I/O
/// errors stops with it for a cancelled run ([`progress`]).
impl From<std::io::Error> for DataError {
    fn from(e: std::io::Error) -> Self {
        if e.get_ref().and_then(|inner| inner.downcast_ref::<DataError>()).is_some_and(|d| matches!(d, DataError::Cancelled)) {
            return DataError::Cancelled;
        }
        DataError::Io(e)
    }
}

/// Replaces `secret` in `message` with asterisks; the text itself when it holds none.
fn without_secret(message: String, secret: &str) -> String {
    if message.contains(secret) { message.replace(secret, "********") } else { message }
}

/// Whether an I/O error's message holds `secret`. Only one carrying text from elsewhere (a
/// driver's, a server's) can: the system's own errors are fixed text and are not even formatted,
/// so the common case allocates nothing.
fn io_mentions(e: &std::io::Error, secret: &str) -> bool {
    e.get_ref().is_some() && e.to_string().contains(secret)
}

impl DataError {
    /// The error with `secret` (a password the driver sent) taken out of its message, whatever a
    /// server or driver echoes, keeping the kind: a refused login stays [`DataError::Login`].
    /// An empty secret leaves the error as it is, and so does text without it (nothing is copied).
    /// Even one-character passwords must be redacted: diagnostics may lose readability,
    /// but an untrusted server can echo a secret of any length.
    pub fn redact_secret(self, secret: &str) -> Self {
        if secret.is_empty() {
            return self;
        }
        match self {
            DataError::Format(m) => DataError::Format(without_secret(m, secret)),
            DataError::Login(m) => DataError::Login(without_secret(m, secret)),
            DataError::Unsupported(m) => DataError::Unsupported(without_secret(m, secret)),
            DataError::NoChunks(m) => DataError::NoChunks(without_secret(m, secret)),
            DataError::Io(e) if io_mentions(&e, secret) => {
                DataError::Io(std::io::Error::new(e.kind(), without_secret(e.to_string(), secret)))
            }
            DataError::Io(e) => DataError::Io(e),
            DataError::Cancelled => DataError::Cancelled,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod secret_redaction_tests {
    use super::*;

    #[test]
    fn remembered_database_redaction_preserves_classification_and_owned_text() {
        let message = String::from("no credential here");
        let pointer = message.as_ptr();
        let DataError::Login(unchanged) = DataError::Login(message).redact_secret("sentinel") else { panic!("lost Login") };
        assert_eq!(unchanged.as_ptr(), pointer);
        let e = DataError::Login("server echoed sentinel twice sentinel".into()).redact_secret("sentinel");
        assert!(matches!(e, DataError::Login(_)));
        assert_eq!(e.to_string(), "server echoed ******** twice ********");
        let e = DataError::Format("sentinel".into()).redact_secret("sentinel");
        assert!(matches!(e, DataError::Format(_)));
        assert_eq!(DataError::Login("unchanged".into()).redact_secret("").to_string(), "unchanged");
    }

    #[derive(Debug)]
    struct Pieces;
    impl std::fmt::Display for Pieces {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("error: se")?;
            f.write_str("nti")?;
            f.write_str("nel echoed")
        }
    }
    impl std::error::Error for Pieces {}

    #[test]
    fn remembered_database_redaction_handles_fragmented_io_and_preserves_kind() {
        let e = DataError::Io(std::io::Error::new(std::io::ErrorKind::PermissionDenied, Pieces)).redact_secret("sentinel");
        let DataError::Io(e) = e else { panic!("lost Io") };
        assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(e.to_string(), "error: ******** echoed");
        let long = "x".repeat(300);
        let e = DataError::Io(std::io::Error::other(format!("before {long} after"))).redact_secret(&long);
        assert_eq!(e.to_string(), "i/o: before ******** after");
        let e = DataError::Io(std::io::Error::other("unchanged")).redact_secret(&long);
        assert_eq!(e.to_string(), "i/o: unchanged");
    }
}

/// One row: an optional geometry and one value per field.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Feature {
    pub id: u64,
    pub geometry: Option<Geometry>,
    pub attrs: Vec<Value>,
}

/// A dataset's features, shared until changed: cloning them (an undo snapshot, a copy of the
/// project) copies a pointer, and the first change through one of the copies copies the rows
/// for it. A large layer is then held once however many undo steps leave it alone.
#[derive(Clone, Default)]
pub struct Features {
    rows: std::sync::Arc<Vec<Feature>>,
    /// The largest id, kept by [`Dataset::push`] and [`Dataset::push_next`] so that building `n`
    /// rows doesn't search them `n` times; `None` (search once) after any other change to the rows.
    max_id: Option<u64>,
}

impl Features {
    /// Whether two are the same rows in memory (one cloned from the other, neither changed since).
    pub fn shares(&self, other: &Features) -> bool {
        std::sync::Arc::ptr_eq(&self.rows, &other.rows)
    }

    /// A non-owning allocation identity for derived caches. Mutable access dissociates weak
    /// references through `Arc::make_mut`, even without another strong owner, so an identity
    /// stops matching after any row, geometry or attribute edit. It does not retain the rows.
    pub fn allocation_key(&self) -> std::sync::Weak<Vec<Feature>> {
        std::sync::Arc::downgrade(&self.rows)
    }

    /// The rows, taking them out of the share (copying them only when another copy holds them).
    pub fn into_vec(self) -> Vec<Feature> {
        std::sync::Arc::unwrap_or_clone(self.rows)
    }

    /// Appends a row with the id one past the largest. `max_id` stays right only because every
    /// other change to `rows` goes through `DerefMut`, which clears it.
    fn push_with_new_id(&mut self, geometry: Option<Geometry>, attrs: Vec<Value>) {
        let max = self.max_id.unwrap_or_else(|| self.rows.iter().map(|f| f.id).max().unwrap_or(0));
        let id = max.saturating_add(1);
        std::sync::Arc::make_mut(&mut self.rows).push(Feature { id, geometry, attrs });
        self.max_id = Some(id);
    }

    /// Appends a row with the id one past the last row's, keeping the largest id if it is known.
    fn push_after_last(&mut self, geometry: Option<Geometry>, attrs: Vec<Value>) {
        let id = self.rows.last().map(|f| f.id.saturating_add(1)).unwrap_or(1);
        std::sync::Arc::make_mut(&mut self.rows).push(Feature { id, geometry, attrs });
        self.max_id = self.max_id.map(|m| m.max(id));
    }
}

impl std::ops::Deref for Features {
    type Target = Vec<Feature>;
    fn deref(&self) -> &Vec<Feature> {
        &self.rows
    }
}

impl std::ops::DerefMut for Features {
    fn deref_mut(&mut self) -> &mut Vec<Feature> {
        // The caller may change ids, so the largest is searched for again on the next push.
        self.max_id = None;
        std::sync::Arc::make_mut(&mut self.rows)
    }
}

impl PartialEq for Features {
    fn eq(&self, other: &Features) -> bool {
        self.shares(other) || *self.rows == *other.rows
    }
}

impl std::fmt::Debug for Features {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.rows.fmt(f)
    }
}

impl From<Vec<Feature>> for Features {
    fn from(v: Vec<Feature>) -> Features {
        Features { rows: std::sync::Arc::new(v), max_id: None }
    }
}

impl FromIterator<Feature> for Features {
    fn from_iter<I: IntoIterator<Item = Feature>>(it: I) -> Features {
        Features::from(it.into_iter().collect::<Vec<_>>())
    }
}

impl IntoIterator for Features {
    type Item = Feature;
    type IntoIter = std::vec::IntoIter<Feature>;
    fn into_iter(self) -> Self::IntoIter {
        self.into_vec().into_iter()
    }
}

impl<'a> IntoIterator for &'a Features {
    type Item = &'a Feature;
    type IntoIter = std::slice::Iter<'a, Feature>;
    fn into_iter(self) -> Self::IntoIter {
        self.rows.iter()
    }
}

impl<'a> IntoIterator for &'a mut Features {
    type Item = &'a mut Feature;
    type IntoIter = std::slice::IterMut<'a, Feature>;
    fn into_iter(self) -> Self::IntoIter {
        std::ops::DerefMut::deref_mut(self).iter_mut()
    }
}

impl Serialize for Features {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        self.rows.serialize(s)
    }
}

impl<'de> Deserialize<'de> for Features {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Features, D::Error> {
        Vec::<Feature>::deserialize(d).map(Features::from)
    }
}

/// A feature class: a named table of features sharing a schema and geometry type.
/// Geometry coordinates are always WGS 84 longitude/latitude, including datasets created by
/// tools and plugins. Projected source coordinates must be normalized before insertion.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Dataset {
    pub name: String,
    pub geometry_type: GeometryType,
    pub fields: Vec<Field>,
    pub features: Features,
    /// Where it was read from, if anywhere.
    #[serde(default)]
    pub source: Option<String>,
    /// The coordinate system it was read in, when that isn't longitude/latitude. Its geometry is
    /// always longitude/latitude; this is what an export writes by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crs: Option<raster::Crs>,
    /// Import lost geometry while interpreting source coordinates. Defining a projection cannot
    /// recover it; import the original file with a CRS override instead. Kept across project saves.
    #[serde(default)]
    pub crs_incomplete: bool,
    /// The source had Z or M values (heights or measures), which RustGIS doesn't keep: they were
    /// dropped on import, so an export writes x and y only. Kept across project saves.
    #[serde(default)]
    pub zm_dropped: bool,
    /// The features are a window onto a source read in place (the part of a large file in view),
    /// so they aren't saved: the source is read again instead.
    #[serde(skip)]
    pub windowed: bool,
}

impl Serialize for Dataset {
    /// Saves CRS recovery and dropped Z/M state with the dataset, but omits disposable streamed rows.
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("Dataset", 8)?;
        st.serialize_field("name", &self.name)?;
        st.serialize_field("geometry_type", &self.geometry_type)?;
        st.serialize_field("fields", &self.fields)?;
        let none: &[Feature] = &[];
        st.serialize_field("features", if self.windowed { none } else { self.features.as_slice() })?;
        st.serialize_field("source", &self.source)?;
        if let Some(crs) = &self.crs {
            st.serialize_field("crs", crs)?;
        } else {
            st.skip_field("crs")?;
        }
        if self.crs_incomplete {
            st.serialize_field("crs_incomplete", &true)?;
        } else {
            st.skip_field("crs_incomplete")?;
        }
        if self.zm_dropped {
            st.serialize_field("zm_dropped", &true)?;
        } else {
            st.skip_field("zm_dropped")?;
        }
        st.end()
    }
}

/// What a reader skipped.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReadReport {
    pub skipped: usize,
    pub messages: Vec<String>,
}

/// What a read says when it drops Z or M values (see [`Dataset::zm_dropped`]): from `features` of
/// them when counted, else from the source as a whole.
pub fn zm_note(features: Option<u64>) -> String {
    let what = match features {
        Some(1) => "1 feature had Z or M values (heights or measures), which were".to_string(),
        Some(n) => format!("{n} features had Z or M values (heights or measures), which were"),
        None => "its Z or M values (heights or measures) were".to_string(),
    };
    format!("{what} dropped: RustGIS keeps x and y only, so exports won't have them")
}

/// What an export of `layers` says when any lost Z or M values on import, or `None`: one notice,
/// naming them (the first few) and counting them. `target` names what was written (a file, or the
/// text returned).
pub fn zm_export_note<'a>(layers: impl IntoIterator<Item = &'a Dataset>, target: &str) -> Option<String> {
    let names: Vec<&str> = layers.into_iter().filter(|ds| ds.zm_dropped).map(|ds| ds.name.as_str()).collect();
    match names.as_slice() {
        [] => None,
        [one] => Some(format!("{target}: its geometry has no Z or M values, which were dropped when {one} was read")),
        _ => {
            Some(format!("{target}: its geometry has no Z or M values for {}, which were dropped when they were read", layer_list(&names)))
        }
    }
}

/// `3 layers (a, b, c)`: the count, and the first five names.
fn layer_list(names: &[&str]) -> String {
    let shown = names.iter().take(5).copied().collect::<Vec<_>>().join(", ");
    let more = if names.len() > 5 { format!(" and {} more", names.len() - 5) } else { String::new() };
    format!("{} layers ({shown}{more})", names.len())
}

/// What an export of `layers` written in `crs` says its geometry lacks, one notice per kind. Each
/// layer comes with its whole extent in longitude/latitude (for a layer read in place, its
/// source's). `target` names what was written. The notices:
/// - Z or M values dropped on import ([`zm_export_note`]).
/// - Positions only approximate, because their system had no shift to WGS 84
///   ([`raster::Crs::lacks_datum_shift`]). Nothing is said when they are written back in it.
/// - Another body's longitude/latitude written as the Earth's, or its system written without the
///   body's name ([`raster::Crs::is_planetary`]).
/// - Coordinates that aren't longitude/latitude (beyond ±180.5° or ±90.5°, as the readers check),
///   which an export writes as if they were.
pub fn export_notes<'a>(
    layers: impl IntoIterator<Item = (&'a Dataset, rustgis_geom::Extent)>,
    target: &str,
    crs: raster::Crs,
) -> Vec<String> {
    let layers: Vec<_> = layers.into_iter().collect();
    let mut notes: Vec<String> = zm_export_note(layers.iter().map(|(ds, _)| *ds), target).into_iter().collect();
    let approximate: Vec<&Dataset> =
        layers.iter().map(|(ds, _)| *ds).filter(|ds| ds.crs.is_some_and(|c| c != crs && c.lacks_datum_shift())).collect();
    match approximate.as_slice() {
        [] => {}
        [one] => notes.push(format!(
            "{target}: the positions of {} are approximate, possibly a few hundred metres off: its coordinate system ({}) has no shift to WGS 84",
            one.name,
            one.crs.map(raster::Crs::label).unwrap_or_default()
        )),
        many => notes.push(format!(
            "{target}: the positions of {} are approximate, possibly a few hundred metres off: their coordinate systems have no shift to WGS 84",
            layer_list(&many.iter().map(|ds| ds.name.as_str()).collect::<Vec<_>>())
        )),
    }
    // Another body's layers. Written in an Earth system, their longitude/latitude become the
    // Earth's. Written in another body's system, its WKT gives the datum only by the ellipsoid's
    // axes (`raster::wkt::to_wkt`).
    let other_bodies: Vec<&Dataset> = layers.iter().map(|(ds, _)| *ds).filter(|ds| ds.crs.is_some_and(raster::Crs::is_planetary)).collect();
    let (in_planetary_system, as_earth) = if crs.is_planetary() { (other_bodies, Vec::new()) } else { (Vec::new(), other_bodies) };
    let written = if crs == raster::Crs::LonLat { "WGS 84 (EPSG:4326)".to_string() } else { crs.label() };
    match as_earth.as_slice() {
        [] => {}
        [one] => notes.push(format!(
            "{target}: {} is on another body than the Earth ({}), but its longitude/latitude are written as {written}",
            one.name,
            one.crs.map(raster::Crs::label).unwrap_or_default()
        )),
        many => notes.push(format!(
            "{target}: {} are on other bodies than the Earth, but their longitude/latitude are written as {written}",
            layer_list(&many.iter().map(|ds| ds.name.as_str()).collect::<Vec<_>>())
        )),
    }
    match in_planetary_system.as_slice() {
        [] => {}
        [one] => notes.push(format!(
            "{target}: the coordinate system of {} ({}) is another body's, and the file gives its datum only by the ellipsoid's axes, without the body's or the datum's name",
            one.name,
            one.crs.map(raster::Crs::label).unwrap_or_default()
        )),
        many => notes.push(format!(
            "{target}: the coordinate systems of {} are other bodies', and the file gives their datums only by the ellipsoids' axes, without the bodies' or the datums' names",
            layer_list(&many.iter().map(|ds| ds.name.as_str()).collect::<Vec<_>>())
        )),
    }
    let outside = |e: &rustgis_geom::Extent| !e.is_empty() && (e.min_x < -180.5 || e.max_x > 180.5 || e.min_y < -90.5 || e.max_y > 90.5);
    let names: Vec<&str> = layers.iter().filter(|(_, e)| outside(e)).map(|(ds, _)| ds.name.as_str()).collect();
    match names.as_slice() {
        [] => {}
        [one] => notes.push(format!(
            "{target}: the coordinates of {one} aren't longitude/latitude, so they're out of place: define its projection and export again"
        )),
        _ => notes.push(format!(
            "{target}: the coordinates of {} aren't longitude/latitude, so they're out of place: define their projection and export again",
            layer_list(&names)
        )),
    }
    notes
}

/// Why exporting `ds` in `crs` would write other data than the layer's: a layer on another body
/// projected onto another ellipsoid ([`raster::planetary::export_error`]). `None` when the export
/// writes its numbers, which [`export_notes`] says when they go under another system's name.
pub fn export_error(ds: &Dataset, crs: raster::Crs) -> Option<String> {
    ds.crs.and_then(|from| raster::planetary::export_error(from, crs)).map(|e| format!("{}: {e}", ds.name))
}

impl Dataset {
    /// Marks the dataset as having lost the Z or M values of `features` features (none: nothing)
    /// and says so in `report`.
    pub(crate) fn drop_zm(&mut self, report: &mut ReadReport, features: usize) {
        if features > 0 {
            self.zm_dropped = true;
            report.messages.push(zm_note(Some(features as u64)));
        }
    }

    /// Creates an empty, complete dataset whose coordinates are assumed to be WGS 84.
    pub fn new(name: impl Into<String>, geometry_type: GeometryType, fields: Vec<Field>) -> Self {
        Dataset {
            name: name.into(),
            geometry_type,
            fields,
            features: Features::default(),
            source: None,
            crs: None,
            crs_incomplete: false,
            zm_dropped: false,
            windowed: false,
        }
    }

    /// Index of a field by case-insensitive name.
    pub fn field_index(&self, name: &str) -> Option<usize> {
        self.fields.iter().position(|f| f.name.eq_ignore_ascii_case(name))
    }

    /// Value of `field` for feature `row` (Null when out of range).
    pub fn value(&self, row: usize, field: usize) -> &Value {
        self.features.get(row).and_then(|f| f.attrs.get(field)).unwrap_or(&Value::Null)
    }

    pub fn extent(&self) -> Extent {
        self.features.iter().filter_map(|f| f.geometry.as_ref()).fold(Extent::EMPTY, |e, g| e.union(&g.extent()))
    }

    /// Pushes a feature with the next id, padding/truncating attributes to the schema.
    pub fn push(&mut self, geometry: Option<Geometry>, mut attrs: Vec<Value>) {
        attrs.resize(self.fields.len(), Value::Null);
        self.features.push_with_new_id(geometry, attrs);
    }

    /// Pushes a feature with the id one past the last feature's, padding/truncating attributes:
    /// for readers filling a new dataset. The ids are unique only while the rows are in id order;
    /// [`Dataset::push`] gives unique ids in any order, until an id reaches `u64::MAX`.
    pub fn push_next(&mut self, geometry: Option<Geometry>, mut attrs: Vec<Value>) {
        attrs.resize(self.fields.len(), Value::Null);
        self.features.push_after_last(geometry, attrs);
    }

    /// Adds a field (if missing) and returns its index.
    pub fn ensure_field(&mut self, name: &str, ty: FieldType) -> usize {
        if let Some(i) = self.field_index(name) {
            return i;
        }
        self.fields.push(Field::new(name, ty));
        for f in &mut self.features {
            f.attrs.push(Value::Null);
        }
        self.fields.len() - 1
    }

    /// The features `rows` keeps, by position; a row past its end counts, so `&[]` keeps them all.
    pub fn kept_features<'a>(&'a self, rows: &'a [bool]) -> impl Iterator<Item = &'a Feature> {
        self.features.iter().enumerate().filter(|(r, _)| rows.get(*r).copied().unwrap_or(true)).map(|(_, f)| f)
    }

    /// Numeric (min, max) of a field over the features `rows` keeps, ignoring non-numbers.
    pub fn numeric_range(&self, field: usize, rows: &[bool]) -> Option<(f64, f64)> {
        let mut r: Option<(f64, f64)> = None;
        for f in self.kept_features(rows) {
            if let Some(v) = f.attrs.get(field).and_then(Value::as_f64) {
                r = Some(match r {
                    Some((a, b)) => (a.min(v), b.max(v)),
                    None => (v, v),
                });
            }
        }
        r
    }

    /// Distinct display values of a field over the features `rows` keeps, the first `limit` in
    /// Unique Values' order ([`value::sort_class_values`]: by number when they read as numbers).
    pub fn unique_values(&self, field: usize, rows: &[bool], limit: usize) -> Vec<String> {
        self.unique_values_counted(field, rows, limit).0
    }

    /// [`Dataset::unique_values`], and what is past them: how many of the kept features with a
    /// geometry have a value past the first `limit`, and whether those were taken by number.
    pub fn unique_values_counted(&self, field: usize, rows: &[bool], limit: usize) -> (Vec<String>, value::PastClasses) {
        let mut drawn: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for f in self.kept_features(rows) {
            let text = f.attrs.get(field).map(|x| x.to_string()).unwrap_or_default();
            *drawn.entry(text).or_insert(0) += usize::from(f.geometry.is_some());
        }
        // Each value read as a number once, not on every comparison.
        let mut v: Vec<(value::ClassText, usize)> = drawn.into_iter().map(|(t, n)| (value::ClassText::new(t), n)).collect();
        let by_number = value::ClassText::numeric(v.iter().map(|(t, _)| t));
        let order = |a: &(value::ClassText, usize), b: &(value::ClassText, usize)| {
            if by_number { a.0.cmp(&b.0) } else { a.0.text().cmp(b.0.text()) }
        };
        // Only the first `limit` need sorting: the rest are set apart in linear time.
        if limit < v.len() {
            v.select_nth_unstable_by(limit, order);
        }
        let features = v.get(limit..).unwrap_or_default().iter().map(|(_, n)| n).sum();
        v.truncate(limit);
        v.sort_by(order);
        (v.into_iter().map(|(t, _)| t.into_text()).collect(), value::PastClasses { features, by_number })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_io_error_keeps_its_source() {
        let e = DataError::Io(std::io::Error::new(std::io::ErrorKind::NotFound, "gone"));
        let source = std::error::Error::source(&e).map(ToString::to_string);
        assert_eq!(source.as_deref(), Some("gone"));
    }

    #[test]
    fn features_are_shared_until_changed() {
        let mut d = Dataset::new("t", GeometryType::Point, vec![Field::new("a", FieldType::Integer)]);
        d.push(Some(Geometry::Point(Coord::new(1.0, 2.0))), vec![Value::Int(5)]);
        let mut copy = d.clone();
        assert!(copy.features.shares(&d.features));
        // Reading doesn't copy; the first change does, and only the changed copy sees it.
        assert_eq!(copy.features.len(), 1);
        assert!(copy.features.shares(&d.features));
        copy.push(Some(Geometry::Point(Coord::new(3.0, 4.0))), vec![Value::Int(6)]);
        assert!(!copy.features.shares(&d.features));
        assert_eq!((d.features.len(), copy.features.len()), (1, 2));
        for f in &mut copy.features {
            f.attrs[0] = Value::Int(0);
        }
        assert_eq!(d.value(0, 0), &Value::Int(5));
        assert_ne!(copy, d);
        let back: Features = serde_json::from_str(&serde_json::to_string(&copy.features).unwrap()).unwrap();
        assert_eq!(back, copy.features);
    }

    #[test]
    fn windowed_features_are_not_saved() {
        let mut d = Dataset::new("t", GeometryType::Point, vec![Field::new("a", FieldType::Integer)]);
        d.push(Some(Geometry::Point(Coord::new(1.0, 2.0))), vec![Value::Int(5)]);
        let back: Dataset = serde_json::from_str(&serde_json::to_string(&d).unwrap()).unwrap();
        assert_eq!(back, d);
        d.windowed = true;
        let back: Dataset = serde_json::from_str(&serde_json::to_string(&d).unwrap()).unwrap();
        assert!(back.features.is_empty() && !back.windowed);
        assert_eq!(back.fields, d.fields);
    }

    #[test]
    fn push_pads_attrs_and_assigns_ids() {
        let mut d = Dataset::new("t", GeometryType::Point, vec![Field::new("a", FieldType::Integer), Field::new("b", FieldType::Text)]);
        d.push(Some(Geometry::Point(Coord::new(1.0, 2.0))), vec![Value::Int(5)]);
        d.push(None, vec![Value::Int(1), Value::Null, Value::Int(9)]);
        assert_eq!(d.features[0].attrs.len(), 2);
        assert_eq!(d.features[1].attrs.len(), 2);
        assert_eq!(d.features[1].id, 2);
        assert_eq!(d.numeric_range(0, &[]), Some((1.0, 5.0)));
        assert_eq!(d.numeric_range(0, &[false]), Some((1.0, 1.0)));
        assert_eq!(d.numeric_range(0, &[false, false]), None);
        assert_eq!(d.unique_values(0, &[true, false], 10), vec!["5"]);
        assert_eq!(d.value(99, 99), &Value::Null);
    }

    /// #1340: a numeric field's distinct values are in numeric order, and the first `limit` kept
    /// are the smallest; a text field's keep text order.
    #[test]
    fn unique_values_of_a_numeric_field_are_in_numeric_order() {
        let point = || Some(Geometry::Point(Coord::new(0.0, 0.0)));
        let fields = vec![Field::new("i", FieldType::Integer), Field::new("r", FieldType::Double), Field::new("t", FieldType::Text)];
        let mut d = Dataset::new("t", GeometryType::Point, fields);
        for (i, r, t) in [(100, 10.0, "b"), (9, 2.5, "A"), (10, -1.0, "10"), (9, 10.0, "9")] {
            d.push(point(), vec![Value::Int(i), Value::Float(r), Value::Text(t.into())]);
        }
        d.push(point(), vec![Value::Null, Value::Null, Value::Null]);
        assert_eq!(d.unique_values(0, &[], 10), ["9", "10", "100", "<Null>"]);
        assert_eq!(d.unique_values(1, &[], 10), ["-1", "2.5", "10", "<Null>"]);
        assert_eq!(d.unique_values(2, &[], 10), ["10", "9", "<Null>", "A", "b"], "text keeps its order");
        // The first two are the two smallest, and the features past them are counted.
        let (first, past) = d.unique_values_counted(0, &[], 2);
        assert_eq!(first, ["9", "10"]);
        assert_eq!(past, value::PastClasses { features: 2, by_number: true });
        assert_eq!(d.unique_values_counted(0, &[], 10).1.features, 0);
        assert_eq!(d.unique_values_counted(2, &[], 10).1, value::PastClasses { features: 0, by_number: false });
        // Only kept features count, and only those with a geometry (one without draws nothing).
        d.push(None, vec![Value::Int(500)]);
        assert_eq!(d.unique_values_counted(0, &[], 2).1.features, 2);
        assert_eq!(d.unique_values_counted(0, &[true, true, true, true, false], 2).1.features, 1);
        // (A row past the end of `rows` is kept.)
        assert_eq!(d.unique_values(0, &[false, false, true, false, false, false], 10), ["10"]);
        // A text value in a numeric field (a hostile file) orders the field as text, and says so
        // though the values kept are numbers.
        d.push(point(), vec![Value::Text("x".into())]);
        let (first, past) = d.unique_values_counted(0, &[], 3);
        assert_eq!(first, ["10", "100", "500"]);
        assert_eq!(past, value::PastClasses { features: 4, by_number: false });
    }

    #[test]
    fn push_keeps_the_largest_id_instead_of_searching_for_it() {
        let mut d = Dataset::new("t", GeometryType::Point, vec![]);
        for _ in 0..3 {
            d.push(None, vec![]);
        }
        assert_eq!(d.features.max_id, Some(3));
        // A kept largest id is what the next push uses: it doesn't search the rows again,
        // so building a layer of n features is linear, not quadratic (#1227).
        d.features.max_id = Some(100);
        d.push(None, vec![]);
        assert_eq!(d.features.last().map(|f| f.id), Some(101));
        // Any other change may change ids, so the next push searches once.
        d.features[0].id = 500;
        assert_eq!(d.features.max_id, None);
        d.push(None, vec![]);
        assert_eq!(d.features.last().map(|f| f.id), Some(501));
        d.features.retain(|f| f.id != 501);
        d.push(None, vec![]);
        let ids: Vec<u64> = d.features.iter().map(|f| f.id).collect();
        assert_eq!(ids, vec![500, 2, 3, 101, 501]);
        // Rows built elsewhere start without one; a clone keeps it with the rows it shares.
        let mut from: Dataset = Dataset::new("u", GeometryType::Point, vec![]);
        from.features = vec![Feature { id: 7, geometry: None, attrs: vec![] }].into();
        assert_eq!(from.features.max_id, None);
        from.push(None, vec![]);
        let mut copy = from.clone();
        assert_eq!(copy.features.max_id, Some(8));
        copy.push(None, vec![]);
        assert_eq!(copy.features.last().map(|f| f.id), Some(9));
        assert_eq!(from.features.len(), 2);
        // push_next keeps a known largest id, so mixing it with push stays linear.
        copy.push_next(None, vec![]);
        assert_eq!(copy.features.max_id, Some(10));
        copy.push(None, vec![]);
        assert_eq!(copy.features.last().map(|f| f.id), Some(11));
    }
}
