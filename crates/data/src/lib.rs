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
pub mod date;
pub mod date_format;
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
pub mod loss;
pub mod mapinfo;
pub mod mbtiles;
pub mod memory;
pub mod ogc;
pub mod osm;
pub mod photo;
pub mod progress;
pub mod provenance;
#[cfg(not(target_arch = "wasm32"))]
pub mod publish;
pub mod query;
pub mod raster;
mod row_access;
pub mod rows;
pub mod s3_address;
pub mod secrets;
pub mod sheets;
pub mod source;
pub mod source_layers;
pub mod sqlite;
pub mod sync;
pub mod threads;
pub mod time;
pub mod topojson;
pub mod value;
pub mod wkb;
pub mod xml;
pub mod zip;

pub use date::{Date, DateTime};
pub use geometry::{Geometry, GeometryType, Measure, VertexId, ambiguous_area_message, pole_loop_count, pole_loop_message};
pub use row_access::RowRef;
pub use rustgis_geom::{Coord, Extent};
pub use value::{Field, FieldType, Value};

use serde::{Deserialize, Serialize};
use std::borrow::Cow;

/// Whether this build runs other programs: GDAL's (for the formats read through it), DuckDB (SQL
/// queries and DuckDB databases) and the `geolibre` runner. Not in the browser, which runs none,
/// nor on a phone (Android, iOS), whose apps can't start them (#1902): there, what would run one
/// says so (see [`no_programs_here`]) instead of asking for an install that can't be made.
pub const RUNS_PROGRAMS: bool = !cfg!(any(target_arch = "wasm32", target_os = "android", target_os = "ios"));

/// What runs RustGIS where [`RUNS_PROGRAMS`] is false, for a message that says a program can't run
/// there: "a phone" or "the browser". Empty where programs run.
pub fn no_programs_here() -> &'static str {
    if RUNS_PROGRAMS {
        ""
    } else if cfg!(target_arch = "wasm32") {
        "the browser"
    } else {
        "a phone"
    }
}

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

impl DataError {
    /// A coordinate conversion's error ([`Dataset::in_lonlat`], [`Dataset::hold_in`], …, which
    /// say it in words): [`DataError::Cancelled`] when it stopped for Cancel, else a format error.
    pub fn of_conversion(e: String) -> Self {
        if e == DataError::Cancelled.to_string() { DataError::Cancelled } else { DataError::Format(e) }
    }
}

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
mod unplaced_out_tests;

#[cfg(test)]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Feature {
    #[cfg_attr(feature = "count-row-uses", deprecated(note = "read rows through `Features::row`/`rows` (`RowRef`); see cargo xtask rows"))]
    pub id: u64,
    #[cfg_attr(feature = "count-row-uses", deprecated(note = "read rows through `Features::row`/`rows` (`RowRef`); see cargo xtask rows"))]
    pub geometry: Option<Geometry>,
    #[cfg_attr(feature = "count-row-uses", deprecated(note = "read rows through `Features::row`/`rows` (`RowRef`); see cargo xtask rows"))]
    pub attrs: Vec<Value>,
}

mod rows_copy {
    use std::cell::RefCell;

    thread_local! {
        static HOOK: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
    }

    /// Takes the hook out before calling it, so it runs once and may itself change rows.
    pub(super) fn fire() {
        if let Some(hook) = HOOK.try_with(|h| h.borrow_mut().take()).ok().flatten() {
            hook();
        }
    }

    /// Puts back the hook that was set before, also when `f` unwinds.
    struct Restore(Option<Option<Box<dyn FnOnce()>>>);

    impl Drop for Restore {
        fn drop(&mut self) {
            if let Some(previous) = self.0.take() {
                let _ = HOOK.try_with(|h| *h.borrow_mut() = previous);
            }
        }
    }

    pub(super) fn with<R>(hook: Box<dyn FnOnce()>, f: impl FnOnce() -> R) -> (R, bool) {
        let previous = HOOK.try_with(|h| h.borrow_mut().replace(hook)).unwrap_or(None);
        let restore = Restore(Some(previous));
        let result = f();
        let fired = HOOK.try_with(|h| h.borrow().is_none()).unwrap_or(true);
        drop(restore);
        (result, fired)
    }
}

/// Runs `f`, calling `hook` once on this thread just before the first change in it copies rows
/// that another owner shares (an Undo snapshot), and says whether it was called. The session
/// frees older Undo steps there, only once an edit is about to copy (#2431); an edit that fails
/// before writing keeps them. Any shared rows `f` copies call it, not only those it set out to
/// change. A hook set before is put back afterwards.
pub fn before_rows_copy<R>(hook: Box<dyn FnOnce()>, f: impl FnOnce() -> R) -> (R, bool) {
    rows_copy::with(hook, f)
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
    /// An id new rows are always given one above, whatever rows are removed: the highest of the
    /// rows a field package gave, so a feature added on the phone never takes one of theirs
    /// ([`Features::keep_ids_above`]). Not saved with the rows; 0 for none.
    id_floor: u64,
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
    #[cfg_attr(feature = "count-row-uses", deprecated(note = "use `Features::to_rows` or the row accessors; see cargo xtask rows"))]
    pub fn into_vec(self) -> Vec<Feature> {
        self.before_copy();
        std::sync::Arc::unwrap_or_clone(self.rows)
    }

    /// Calls the hook [`before_rows_copy`] set, when a change is about to copy rows another
    /// owner shares.
    fn before_copy(&self) {
        if std::sync::Arc::strong_count(&self.rows) > 1 {
            rows_copy::fire();
        }
    }

    /// Appends a row with the id one past the largest. `max_id` stays right because every change
    /// that removes or reorders rows clears it (`DerefMut`, `remove_row`, `retain_rows`,
    /// `truncate_rows`), and the pushes keep it.
    fn push_with_new_id(&mut self, geometry: Option<Geometry>, attrs: Vec<Value>) {
        let max = self.max_id.unwrap_or_else(|| self.rows.iter().map(|f| f.id).max().unwrap_or(0)).max(self.id_floor);
        let id = max.saturating_add(1);
        self.before_copy();
        std::sync::Arc::make_mut(&mut self.rows).push(Feature { id, geometry, attrs });
        self.max_id = Some(id);
    }

    /// Appends a row with the id one past the last row's, keeping the largest id if it is known.
    fn push_after_last(&mut self, geometry: Option<Geometry>, attrs: Vec<Value>) {
        let id = self.rows.last().map(|f| f.id.saturating_add(1)).unwrap_or(1);
        self.before_copy();
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
        self.before_copy();
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
        Features { rows: std::sync::Arc::new(v), max_id: None, id_floor: 0 }
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

/// Features are saved as a list of [`Feature`]s.
#[cfg(feature = "schema")]
impl schemars::JsonSchema for Features {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Features".into()
    }
    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        <Vec<Feature>>::json_schema(generator)
    }
}

impl<'de> Deserialize<'de> for Features {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Features, D::Error> {
        Vec::<Feature>::deserialize(d).map(Features::from)
    }
}

/// A feature class: a named table of features sharing a schema and geometry type.
/// Its geometry is in the system it holds its coordinates in ([`Dataset::held_in`]): a file's
/// own system for a file read in one (and a saved project keeps it), WGS 84 longitude/latitude
/// for one without, for features drawn or made by tools and plugins, and for a project saved by
/// an older release.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Dataset {
    pub name: String,
    pub geometry_type: GeometryType,
    pub fields: Vec<Field>,
    pub features: Features,
    /// Where it was read from, if anywhere.
    #[serde(default)]
    pub source: Option<String>,
    /// The coordinate system it was read in, when that isn't longitude/latitude: what an export
    /// writes by default. Its geometry is in [`Dataset::held_in`], which is this system for a file
    /// read now or a project saved by this release, and longitude/latitude for one a project saved
    /// by an older release holds.
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
    /// The system its geometry is in ([`Dataset::coord_crs`]): longitude/latitude unless it was
    /// put in another with [`Dataset::hold_in`]. A project saves its features in it, exactly,
    /// with the system beside them (`held_in`, absent for longitude/latitude, as every release
    /// saved them before), so a saved dataset reads back held as it was; a project with one
    /// requires `native-coordinates`.
    #[serde(default = "held_in_lonlat")]
    pub held_in: raster::Crs,
    /// The system of the map it is shown on, when that map draws it straight into its own system
    /// (the map's `direct_system`, set by the map): its places are then given in
    /// longitude/latitude for edits and drawing as that map shows them ([`Dataset::shown_conversion`]),
    /// so a vertex handle or a new vertex lands on the line the map draws. `None` when it is shown
    /// on no such map.
    #[serde(skip)]
    pub shown_in: Option<raster::Crs>,
}

/// What a dataset saved without a system of its own holds its coordinates in
/// ([`Dataset::held_in`]): longitude/latitude, as every release saved them before.
fn held_in_lonlat() -> raster::Crs {
    raster::Crs::LonLat
}

impl Dataset {
    /// The coordinate system its geometry is in ([`Dataset::held_in`]): the system a file was read
    /// in, else longitude/latitude (WGS 84). Drawing converts geometry from it
    /// ([`raster::Crs::to`]). Longitude/latitude never fails to convert, and [`Dataset::hold_in`]
    /// only holds a dataset where every place converts. A place that can't be converted isn't
    /// drawn, and the session says so (the map counts them as it draws, `Layer::unplaced`).
    pub fn coord_crs(&self) -> raster::Crs {
        self.held_in
    }

    /// Holds a dataset made in longitude/latitude from another (a command's output: a route,
    /// simulated points, annotation) as that one is: in `held`, the system it holds its
    /// coordinates in ([`Dataset::coord_crs`]), with `crs`, the one it was read in, which exports
    /// use by default. Refused as [`Dataset::hold_in`] is, leaving it as it was: in
    /// longitude/latitude with no system of its own, so Define Projection never takes its numbers
    /// for another system's.
    pub fn hold_as(&mut self, held: raster::Crs, crs: Option<raster::Crs>) -> std::result::Result<(), String> {
        self.hold_in(held)?;
        self.crs = crs;
        Ok(())
    }

    /// Converts its geometry into `crs` and holds it there ([`Dataset::held_in`]); a missing
    /// coordinate (NaN, infinite) stays as it is. Refused, naming the first feature, when its
    /// system can't be converted to `crs` or one of its places can't be held there (in Web
    /// Mercator, one past 85.05°, which it would hold at 85.05°); and for a window onto a source
    /// read in place, whose rows are read again in longitude/latitude. Nothing changes then.
    pub fn hold_in(&mut self, crs: raster::Crs) -> std::result::Result<(), String> {
        if self.windowed && crs != self.held_in {
            return Err(format!("{}: a layer read in place is held as its source is read", self.name));
        }
        let to = self.held_in.to(crs)?;
        if !to.is_identity() {
            let lonlat = self.lonlat_conversion()?;
            let past = |g: &Geometry| {
                let mut past = false;
                if crs == raster::Crs::WebMercator {
                    g.for_each_coord(|c| past |= c.is_finite() && lonlat.point(c).is_some_and(|l| l.y.abs() > rustgis_geom::MAX_LAT));
                }
                past
            };
            let held = |g: &Geometry| (!past(g)).then(|| convert_keeping_missing(&to, g, &HeldPlaces::default())).flatten();
            // Each place is converted once, and what goes in is what was checked (a second pass
            // could convert otherwise, as grids arriving meanwhile change the operations): the
            // converted geometries are kept beside the rows, then put in their place, so a refusal
            // or a Cancel changes nothing. Only geometry is kept twice meanwhile, never the
            // attributes, and rows no one else holds aren't copied.
            let mut converted = Vec::with_capacity(self.features.len());
            for (i, f) in self.features.iter().enumerate() {
                stop_when_asked(i)?;
                converted.push(match &f.geometry {
                    Some(g) => Some(
                        held(g)
                            .ok_or_else(|| format!("{}: feature {} has a place that can't be held in {}", self.name, f.id, crs.label()))?,
                    ),
                    None => None,
                });
            }
            for (f, g) in self.features.iter_mut().zip(converted) {
                f.geometry = g;
            }
        }
        self.held_in = crs;
        Ok(())
    }

    /// One of its geometries in longitude/latitude, the space edits work in (a vertex dragged to a
    /// place, a move on the ground): borrowed as it is while its coordinates are
    /// longitude/latitude ([`Dataset::coord_crs`]). A missing coordinate (NaN, infinite) stays as
    /// it is, for the edit to judge; refused,
    /// naming its system, when its system can't be converted or one of its places can't be.
    /// On a map that draws it straight into its own system ([`Dataset::shown_in`]), as that map
    /// shows it ([`Dataset::shown_conversion`]).
    pub fn geometry_to_lonlat<'a>(&self, g: &'a Geometry) -> std::result::Result<Cow<'a, Geometry>, String> {
        Self::geometry_to_lonlat_by(&self.shown_conversion()?, g)
    }

    /// [`Dataset::geometry_to_lonlat`] of `g`, feature `id`'s geometry, for an edit of that one
    /// feature: refused naming the layer and the feature when one of its places can't be given in
    /// longitude/latitude, so it can't be edited there.
    pub fn feature_to_lonlat<'a>(&self, id: u64, g: &'a Geometry) -> std::result::Result<Cow<'a, Geometry>, String> {
        let to = self.shown_conversion()?;
        Self::converted_to_lonlat(&to, g).ok_or_else(|| {
            format!(
                "{}: feature {id} has a place in {} that can't be given in longitude/latitude, so it can't be edited there",
                self.name,
                to.source().label()
            )
        })
    }

    /// [`Dataset::geometry_to_lonlat`] by `to`, its [`Dataset::lonlat_conversion`] made once for
    /// many geometries: refused, naming its system, when one of the geometry's places can't be
    /// converted.
    pub fn geometry_to_lonlat_by<'a>(to: &raster::Reprojection, g: &'a Geometry) -> std::result::Result<Cow<'a, Geometry>, String> {
        Self::converted_to_lonlat(to, g)
            .ok_or_else(|| format!("a place of this feature in {} can't be given in longitude/latitude", to.source().label()))
    }

    /// One of its geometries in longitude/latitude by `to`, its [`Dataset::lonlat_conversion`]
    /// made once for many geometries (as a frame draws them), as [`Dataset::geometry_to_lonlat`]
    /// converts it: borrowed when `to` is the identity, a missing coordinate kept, `None` when one
    /// of its places can't be converted.
    pub fn converted_to_lonlat<'a>(to: &raster::Reprojection, g: &'a Geometry) -> Option<Cow<'a, Geometry>> {
        if to.is_identity() {
            return Some(Cow::Borrowed(g));
        }
        convert_keeping_missing(to, g, &HeldPlaces::default()).map(Cow::Owned)
    }

    /// An edited geometry in longitude/latitude, written back in its own coordinates
    /// ([`Dataset::coord_crs`]): as it is while they are longitude/latitude. A missing coordinate
    /// stays as it is; refused, naming its system, when one of its places can't be held there. On
    /// a map that draws it straight into its own system ([`Dataset::shown_in`]), from
    /// longitude/latitude as that map shows them ([`Dataset::shown_conversion`]). A place reading
    /// exactly as a held one in longitude/latitude ([`Dataset::hold_places`]) is written as it is
    /// held, not converted back. An edit then moves only what it moved: the places
    /// it left, or snapped to, stay exactly as stored, whatever a round trip through
    /// longitude/latitude would do to them (about a nanometre in UTM, but up to metres where a
    /// projection's inverse is approximate, as Robinson's is).
    pub fn edit_from_lonlat(&self, g: Geometry, held: &HeldPlaces) -> std::result::Result<Geometry, String> {
        Self::edit_from_lonlat_by(&self.shown_from_lonlat()?, g, held)
    }

    /// [`Dataset::edit_from_lonlat`] by `to`, a conversion into its own coordinates: exact through
    /// WGS 84 (`Crs::LonLat.to(coord_crs)`) for places that come from data rather than a map, such
    /// as a tool's results ([`Dataset::results_from_lonlat`]) or a photo's position, or from
    /// another system for pasted features. A place past 85.05° is refused when `to` is from
    /// longitude/latitude into Web Mercator.
    pub fn edit_from_lonlat_by(to: &raster::Reprojection, g: Geometry, held: &HeldPlaces) -> std::result::Result<Geometry, String> {
        if to.is_identity() {
            return Ok(g);
        }
        edit_into_own(to, &g, held).ok_or_else(|| format!("the edit reaches a place {} can't hold", to.target().label()))
    }

    /// Whether an edit's places, given in longitude/latitude, are written as they are, with
    /// nothing to hold ([`Dataset::hold_places`]): its coordinates are longitude/latitude.
    pub fn edits_in_lonlat(&self) -> bool {
        self.lonlat_conversion().is_ok_and(|to| to.is_identity())
    }

    /// Adds the places of `g`, a geometry in its coordinates (or in a dataset's of the same
    /// system), to `held`, each by how it reads in longitude/latitude, for an edit to keep
    /// ([`Dataset::edit_from_lonlat`]): those `within` a box in its coordinates, or all. Nothing
    /// while its coordinates are longitude/latitude; a place that can't be converted adds nothing.
    pub fn hold_places(&self, g: &Geometry, within: Option<&Extent>, held: &mut HeldPlaces) {
        if let Ok(to) = self.shown_conversion() {
            Self::hold_places_by(&to, g, within, held);
        }
    }

    /// [`Dataset::hold_places`], each place read in longitude/latitude by `to`.
    fn hold_places_by(to: &raster::Reprojection, g: &Geometry, within: Option<&Extent>, held: &mut HeldPlaces) {
        if !to.is_identity() {
            held.add(to, g, within);
        }
    }

    /// The map system its places are shown through ([`Dataset::shown_in`]), when the map draws it
    /// there by a direct operation: `None` when it is shown on no such map, is in that system
    /// itself or in longitude/latitude, or there is no direct operation between the two (the way
    /// through WGS 84 then gives what the map draws).
    pub fn shown_through(&self) -> Option<raster::Crs> {
        let (own, map) = (self.coord_crs(), self.shown_in?);
        let plain = |c: raster::Crs| matches!(c, raster::Crs::LonLat | raster::Crs::WebMercator);
        (own != map && !plain(own) && !plain(map) && own.to(map).is_ok_and(|t| t.is_direct())).then_some(map)
    }

    /// The conversion of its own coordinates to longitude/latitude for edits and drawing: as the
    /// map it is shown on gives them ([`Dataset::shown_in`]: into the map's system by the direct
    /// operation the map draws it with, then by the map's own inverse), else
    /// [`Dataset::lonlat_conversion`]. Analysis and exports take [`Dataset::lonlat_conversion`],
    /// which doesn't depend on a map.
    pub fn shown_conversion(&self) -> std::result::Result<raster::Reprojection, String> {
        match self.shown_through() {
            Some(map) => self.coord_crs().to_via(map, raster::Crs::LonLat),
            None => self.lonlat_conversion(),
        }
    }

    /// [`Dataset::shown_conversion`] the other way: longitude/latitude into its own coordinates.
    pub fn shown_from_lonlat(&self) -> std::result::Result<raster::Reprojection, String> {
        match self.shown_through() {
            Some(map) => raster::Crs::LonLat.to_via(map, self.coord_crs()),
            None => raster::Crs::LonLat.to(self.coord_crs()),
        }
    }

    /// How many of its values a saved project holds as null, not as they are: numbers that aren't
    /// finite (NaN, infinity), which JSON can't write ([`Value`]'s `Serialize`). Zero for a window
    /// onto a source, whose rows aren't saved.
    pub fn unsaved_numbers(&self) -> usize {
        if self.windowed {
            return 0;
        }
        self.features.rows().flat_map(RowRef::values).filter(|v| v.is_non_finite()).count()
    }

    /// How many of its features have a coordinate that isn't a number (NaN, infinity), which a
    /// saved project would hold as null and couldn't open again ([`Geometry::is_finite`], #1985).
    /// Zero for a window onto a source, whose rows aren't saved.
    pub fn unsaved_coordinates(&self) -> usize {
        if self.windowed {
            return 0;
        }
        self.features.rows().filter(|r| r.geometry().is_some_and(|g| !g.is_finite())).count()
    }
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
        // In the system it holds them in, exactly, as it reads them back (`held_in`, below).
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
        // Absent for longitude/latitude, as every release saved a dataset before.
        if self.held_in != raster::Crs::LonLat && !self.windowed {
            st.serialize_field("held_in", &self.held_in)?;
        } else {
            st.skip_field("held_in")?;
        }
        st.end()
    }
}

/// What a reader skipped.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReadReport {
    pub skipped: usize,
    /// What the read says, each loss with its code ([`loss::Note`]).
    pub messages: loss::Notes,
    /// The files read and the rows read from them, when the read recorded them
    /// ([`sync::FileSync::around`]): what a layer kept in the project compares with its source
    /// before writing edits back.
    pub synced: Option<sync::Synced>,
    /// What a read of a source says of values in date fields that weren't dates
    /// ([`source::unread_dates_note`]), also in `messages`: for a caller that says only this.
    pub unread_dates: Option<String>,
    /// What it says of date-times it rounded to the microsecond ([`source::rounded_dates_note`]),
    /// also in `messages`.
    pub rounded_dates: Option<String>,
    /// The extrusion the reader suggests for the layer (a city model's volumes): applied when the
    /// layer is added, and only then; a reload keeps the layer's own.
    pub extrusion: Option<SuggestedExtrusion>,
}

/// An extrusion a reader suggests for a layer of polygons: heights from one field, on a base from
/// another.
#[derive(Clone, Debug, PartialEq)]
pub struct SuggestedExtrusion {
    pub height_field: String,
    pub base_field: String,
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
pub fn zm_export_note<'a>(layers: impl IntoIterator<Item = &'a Dataset>, target: &str) -> Option<loss::Note> {
    let names: Vec<&str> = layers.into_iter().filter(|ds| ds.zm_dropped).map(|ds| ds.name.as_str()).collect();
    let text = match names.as_slice() {
        [] => return None,
        [one] => format!("{target}: its geometry has no Z or M values, which were dropped when {one} was read"),
        _ => format!("{target}: its geometry has no Z or M values for {}, which were dropped when they were read", layer_list(&names)),
    };
    Some(loss::Note::loss(loss::LossCode::DroppedZm, text))
}

/// `3 layers (a, b, c)`: the count, and the first five names.
fn layer_list(names: &[&str]) -> String {
    let shown = names.iter().take(5).copied().collect::<Vec<_>>().join(", ");
    let more = if names.len() > 5 { format!(" and {} more", names.len() - 5) } else { String::new() };
    format!("{} layers ({shown}{more})", names.len())
}

/// What `what` (an export to a file, a copy, a package: "{target}") says of the `n` features of
/// `layer` it took without their geometry, as a place of each can't be given in
/// longitude/latitude ([`Dataset::in_lonlat_leaving_out`]). `None` for none.
pub fn unplaced_note(target: &str, layer: &str, n: usize) -> Option<String> {
    (n > 0).then(|| {
        format!("{target}: {n} feature(s) of {layer} have a place that can't be given in longitude/latitude, so their geometry is left out (and the feature, where only features with geometry are kept)")
    })
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
/// - Latitudes beyond 85.05° north or south, which an export in Web Mercator writes at 85.05°,
///   where it ends ([`rustgis_geom::lonlat_to_merc`]).
pub fn export_notes<'a>(
    layers: impl IntoIterator<Item = (&'a Dataset, rustgis_geom::Extent)>,
    target: &str,
    crs: raster::Crs,
) -> loss::Notes {
    use loss::{LossCode, Note};
    let layers: Vec<_> = layers.into_iter().collect();
    let mut notes: loss::Notes = zm_export_note(layers.iter().map(|(ds, _)| *ds), target).into_iter().collect();
    let approximate: Vec<&Dataset> =
        layers.iter().map(|(ds, _)| *ds).filter(|ds| ds.crs.is_some_and(|c| c != crs && c.lacks_datum_shift())).collect();
    match approximate.as_slice() {
        [] => {}
        [one] => notes.push(Note::loss(LossCode::ApproximatedDatum, format!(
            "{target}: the positions of {} are approximate, possibly a few hundred metres off: its coordinate system ({}) has no shift to WGS 84",
            one.name,
            one.crs.map(raster::Crs::label).unwrap_or_default()
        ))),
        many => notes.push(Note::loss(LossCode::ApproximatedDatum, format!(
            "{target}: the positions of {} are approximate, possibly a few hundred metres off: their coordinate systems have no shift to WGS 84",
            layer_list(&many.iter().map(|ds| ds.name.as_str()).collect::<Vec<_>>())
        ))),
    }
    // Another body's layers. Written in an Earth system, their longitude/latitude become the
    // Earth's. Written in another body's system, its WKT gives the datum only by the ellipsoid's
    // axes (`raster::wkt::to_wkt`), unless the system was read from a WKT that WKT 1 can hold,
    // which is written as it was read, names and all (`raster::wkt::writes_its_own_wkt`).
    let other_bodies: Vec<&Dataset> = layers.iter().map(|(ds, _)| *ds).filter(|ds| ds.crs.is_some_and(raster::Crs::is_planetary)).collect();
    let named = raster::wkt::writes_its_own_wkt(crs);
    let (in_planetary_system, as_earth) = match (crs.is_planetary(), named) {
        // The file names its body: only a layer on another body says its longitude/latitude are
        // written as the file's.
        (true, true) => {
            // From its PROJ string, else (a system PROJ writes none for) its WKT.
            let axis = |c: raster::Crs| {
                c.custom_def().and_then(|d| {
                    raster::planetary::proj4_axes(&d.proj4).map(|(a, _)| a).or_else(|| d.wkt.as_deref().and_then(raster::wkt::semi_major))
                })
            };
            // Another body: its semi-major axis more than 1% from the target's.
            let other = |ds: &&Dataset| match (ds.crs.and_then(axis), axis(crs)) {
                (Some(a), Some(b)) => ((a - b) / b).abs() > 0.01,
                _ => ds.crs != Some(crs),
            };
            (Vec::new(), other_bodies.into_iter().filter(other).collect())
        }
        (true, false) => (other_bodies, Vec::new()),
        (false, _) => (Vec::new(), other_bodies),
    };
    let written = if crs == raster::Crs::LonLat { "WGS 84 (EPSG:4326)".to_string() } else { crs.label() };
    match as_earth.as_slice() {
        [] => {}
        [one] => notes.push(Note::loss(
            LossCode::ChangedBody,
            format!(
                "{target}: {} is on another body than the Earth ({}), but its longitude/latitude are written as {written}",
                one.name,
                one.crs.map(raster::Crs::label).unwrap_or_default()
            ),
        )),
        many => notes.push(Note::loss(
            LossCode::ChangedBody,
            format!(
                "{target}: {} are on other bodies than the Earth, but their longitude/latitude are written as {written}",
                layer_list(&many.iter().map(|ds| ds.name.as_str()).collect::<Vec<_>>())
            ),
        )),
    }
    match in_planetary_system.as_slice() {
        [] => {}
        [one] => notes.push(Note::loss(LossCode::DroppedCrsName, format!(
            "{target}: the coordinate system of {} ({}) is another body's, and the file gives its datum only by the ellipsoid's axes, without the body's or the datum's name",
            one.name,
            one.crs.map(raster::Crs::label).unwrap_or_default()
        ))),
        many => notes.push(Note::loss(LossCode::DroppedCrsName, format!(
            "{target}: the coordinate systems of {} are other bodies', and the file gives their datums only by the ellipsoids' axes, without the bodies' or the datums' names",
            layer_list(&many.iter().map(|ds| ds.name.as_str()).collect::<Vec<_>>())
        ))),
    }
    let names: Vec<&str> = layers.iter().filter(|(_, e)| e.beyond_lonlat()).map(|(ds, _)| ds.name.as_str()).collect();
    match names.as_slice() {
        [] => {}
        // Told by the extent alone, which also takes in longitudes past ±180° on purpose (a layer
        // crossing the antimeridian): said, without a code, as it may have lost nothing.
        [one] => notes.push(Note::plain(format!(
            "{target}: the coordinates of {one} aren't longitude/latitude, so they're out of place: define its projection and export again"
        ))),
        _ => notes.push(Note::plain(format!(
            "{target}: the coordinates of {} aren't longitude/latitude, so they're out of place: define their projection and export again",
            layer_list(&names)
        ))),
    }
    if crs == raster::Crs::WebMercator {
        notes.extend(web_mercator_note(&layers, target));
    }
    notes
}

/// What an export in Web Mercator says of the layers it writes past 85.05° north or south, where
/// Web Mercator ends: [`rustgis_geom::lonlat_to_merc`] writes a latitude beyond (an infinite one
/// too) at 85.05°. The vertices moved are counted from a layer's features, those the file holds:
/// a Shapefile target (`.shp`, or zipped to `.zip`, as the exports name it) holds only geometry of
/// the layer's kind, and of it only parts it can draw ([`io::shapefile_vertices`]). A missing
/// coordinate, or an infinite longitude, isn't counted: it can't be projected, so the export
/// refuses it. A layer whose features are only a window onto its source (`windowed`) has its
/// extent tell instead, without a count; as a source can declare a wider extent than its data's,
/// that notice says only that the extent reaches beyond. A streamed layer written a chunk at a
/// time comes with no features, and its export counts them as it writes them
/// ([`export::Exported::beyond_web_mercator`], said by [`web_mercator_moved_note`]). The count is
/// one pass over the coordinates, without a Cancel point, as the other notices' passes are.
fn web_mercator_note(layers: &[(&Dataset, rustgis_geom::Extent)], target: &str) -> Option<loss::Note> {
    use rustgis_geom::MAX_LAT;
    let shapefile = std::path::Path::new(target)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("shp") || e.eq_ignore_ascii_case("zip"));
    let beyond: Vec<(&str, Option<usize>)> = layers
        .iter()
        .filter_map(|(ds, e)| {
            let mut moved = 0usize;
            if !ds.windowed {
                // By latitude, from the system the layer holds its coordinates in: none counted
                // when that can't be converted (its export can't be written either).
                let to = match ds.lonlat_conversion() {
                    Ok(t) => (!t.is_identity()).then_some(t),
                    Err(_) => return None,
                };
                let mut count = |c: Coord| {
                    let c = match &to {
                        Some(t) => t.point(c).unwrap_or(Coord::new(f64::NAN, f64::NAN)),
                        None => c,
                    };
                    moved += usize::from(c.x.is_finite() && c.y.abs() > MAX_LAT)
                };
                for g in ds.features.iter().filter_map(|f| f.geometry.as_ref()) {
                    if shapefile {
                        crate::io::shapefile_vertices(g, ds.geometry_type, &mut count);
                    } else {
                        g.for_each_coord(&mut count);
                    }
                }
            }
            let past = ds.windowed && !e.is_empty() && (e.min_y < -MAX_LAT || e.max_y > MAX_LAT);
            if moved > 0 { Some((ds.name.as_str(), Some(moved))) } else { past.then_some((ds.name.as_str(), None)) }
        })
        .collect();
    web_mercator_said(&beyond, target)
}

/// What an export in Web Mercator written a chunk at a time says of the `moved` vertices of
/// `layer` it wrote at 85.05° north or south, where Web Mercator ends, as it counted them
/// ([`export::Exported::beyond_web_mercator`]): as [`export_notes`] says it of a layer it has
/// whole. `None` for none.
pub fn web_mercator_moved_note(target: &str, layer: &str, moved: usize) -> Option<loss::Note> {
    if moved == 0 {
        return None;
    }
    web_mercator_said(&[(layer, Some(moved))], target)
}

/// The 85.05° notice for the layers `beyond` (each with its count of vertices moved, or `None`
/// when only its extent tells), or `None` for none. Layers counted and layers told by their
/// extent are said in one notice, each in its own clause, so neither hides the other.
fn web_mercator_said(beyond: &[(&str, Option<usize>)], target: &str) -> Option<loss::Note> {
    let counted: Vec<&str> = beyond.iter().filter(|(_, n)| n.is_some()).map(|(name, _)| *name).collect();
    let total: usize = beyond.iter().filter_map(|(_, n)| *n).sum();
    let extents: Vec<&str> = beyond.iter().filter(|(_, n)| n.is_none()).map(|(name, _)| *name).collect();
    let mut said = Vec::new();
    match (counted.as_slice(), total) {
        ([], _) => {}
        (_, 1) => said.push(format!("1 vertex of {} is beyond 85.05° north or south, and written at 85.05°", counted.join(", "))),
        ([one], n) => said.push(format!("{n} vertices of {one} are beyond 85.05° north or south, and written at 85.05°")),
        (_, n) => said.push(format!("{n} vertices of {} are beyond 85.05° north or south, and written at 85.05°", layer_list(&counted))),
    }
    match extents.as_slice() {
        [] => {}
        [one] => said.push(format!("the extent of {one} reaches beyond 85.05° north or south, and what lies beyond is written at 85.05°")),
        _ => said.push(format!(
            "the extents of {} reach beyond 85.05° north or south, and what lies beyond is written at 85.05°",
            layer_list(&extents)
        )),
    }
    if said.is_empty() {
        return None;
    }
    let text = format!("{target}: {}, where Web Mercator ends", said.join("; "));
    // Vertices counted are moved; an extent alone may reach further than the features do.
    Some(if counted.is_empty() { loss::Note::plain(text) } else { loss::Note::loss(loss::LossCode::ChangedPosition, text) })
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
            report.messages.push(loss::Note::loss(loss::LossCode::DroppedZm, zm_note(Some(features as u64))));
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
            held_in: raster::Crs::LonLat,
            shown_in: None,
        }
    }

    /// Index of a field by case-insensitive name.
    pub fn field_index(&self, name: &str) -> Option<usize> {
        self.fields.iter().position(|f| f.name.eq_ignore_ascii_case(name))
    }

    /// Value of `field` for feature `row` (Null when out of range).
    pub fn value(&self, row: usize, field: usize) -> &Value {
        self.features.value(row, field)
    }

    /// The box around its features' geometry, in its own coordinates ([`Dataset::coord_crs`]), as
    /// a file in that system would hold it. [`Dataset::lonlat_extent`] gives it in
    /// longitude/latitude, for zooming and for what the map and the notices take.
    pub fn extent(&self) -> Extent {
        self.features.rows().filter_map(RowRef::geometry).fold(Extent::EMPTY, |e, g| e.union(&g.extent()))
    }

    /// The box around its features' geometry in longitude/latitude: [`Dataset::extent`] through
    /// [`Dataset::extent_to_lonlat`].
    pub fn lonlat_extent(&self) -> Extent {
        self.lonlat_extent_of(self.features.rows().filter_map(RowRef::geometry))
    }

    /// The box in longitude/latitude around `geometries` (its own): exactly that of their places
    /// there, each converted, as the readers of every release before #2032 C2 converted them,
    /// not the converted box of their box (which a projection's grid, turned and curved against
    /// longitude/latitude, makes larger). Their box itself while they are longitude/latitude;
    /// a place that can't be converted is left out; empty when its system can't be converted.
    pub fn lonlat_extent_of<'a>(&self, geometries: impl Iterator<Item = &'a Geometry>) -> Extent {
        let Ok(to) = self.lonlat_conversion() else { return Extent::EMPTY };
        if to.is_identity() {
            return geometries.fold(Extent::EMPTY, |e, g| e.union(&g.extent()));
        }
        let mut out = Extent::EMPTY;
        for g in geometries {
            g.for_each_coord(|c| {
                if let Some(l) = to.point(c) {
                    out.expand(l);
                }
            });
        }
        out
    }

    /// A box in its own coordinates ([`Dataset::coord_crs`]) in longitude/latitude, as
    /// [`raster::Reprojection::extent`] converts it. The box itself while they are
    /// longitude/latitude. Empty when its system can't be converted (the map then draws none of
    /// it, and the session says so).
    pub fn extent_to_lonlat(&self, e: &Extent) -> Extent {
        match self.coord_crs().to(raster::Crs::LonLat) {
            Ok(to) => to.extent(e),
            Err(_) => Extent::EMPTY,
        }
    }

    /// The conversion of its own coordinates ([`Dataset::coord_crs`]) to longitude/latitude, for
    /// places taken from its geometry one by one, exact through WGS 84 whatever map it is shown
    /// on: the identity while they are longitude/latitude. Fails as [`raster::Crs::to`] does,
    /// which longitude/latitude never does.
    pub fn lonlat_conversion(&self) -> std::result::Result<raster::Reprojection, String> {
        self.coord_crs().to(raster::Crs::LonLat)
    }

    /// `m` of one of its geometries on the WGS 84 ellipsoid, measured in longitude/latitude
    /// ([`Geometry::wgs84_measure`]) from its own coordinates; refused as
    /// [`Dataset::geometry_to_lonlat`] refuses.
    pub fn wgs84_measure(&self, g: &Geometry, m: Measure) -> std::result::Result<Option<f64>, String> {
        Ok(Self::geometry_to_lonlat_by(&self.lonlat_conversion()?, g)?.wgs84_measure(m))
    }

    /// [`ambiguous_area_message`] for these of its geometries, read in longitude/latitude, where a
    /// ring through both poles is one; it also counts those that can't be given there, whose area
    /// can't be measured either.
    pub fn ambiguous_area_message<'a>(&self, geometries: impl Iterator<Item = &'a Geometry>) -> Option<String> {
        let unplaced = std::cell::Cell::new(0usize);
        let to = self.lonlat_conversion();
        let ambiguous = ambiguous_area_message(geometries.filter_map(|g| {
            match to.as_ref().map_err(String::clone).and_then(|to| Self::geometry_to_lonlat_by(to, g)) {
                Ok(g) => Some(g),
                Err(_) => {
                    unplaced.set(unplaced.get() + 1);
                    None
                }
            }
        }));
        let unplaced = (unplaced.get() > 0).then(|| {
            format!("area can't be measured for {} feature(s) with a place that can't be given in longitude/latitude", unplaced.get())
        });
        match (ambiguous, unplaced) {
            (Some(a), Some(u)) => Some(format!("{a}; {u}")),
            (a, u) => a.or(u),
        }
    }

    /// The dataset in longitude/latitude, the space most analysis tools work in: borrowed while its
    /// coordinates are longitude/latitude ([`Dataset::coord_crs`]); otherwise a copy with every
    /// geometry converted, missing coordinates kept. Refused, naming the first feature that can't
    /// be, when one of its places can't be converted. The copy is held in longitude/latitude
    /// ([`Dataset::held_in`]), so it isn't converted again.
    pub fn in_lonlat(&self) -> std::result::Result<Cow<'_, Dataset>, String> {
        let to = self.lonlat_conversion()?;
        if to.is_identity() {
            return Ok(Cow::Borrowed(self));
        }
        stop_when_asked(0)?;
        let mut out = self.clone();
        out.held_in = raster::Crs::LonLat;
        for (i, f) in out.features.iter_mut().enumerate() {
            stop_when_asked(i)?;
            if let Some(g) = &mut f.geometry {
                *g = convert_keeping_missing(&to, g, &HeldPlaces::default()).ok_or_else(|| {
                    format!(
                        "{}: feature {} has a place in {} that can't be given in longitude/latitude",
                        self.name,
                        f.id,
                        to.source().label()
                    )
                })?;
            }
        }
        Ok(Cow::Owned(out))
    }

    /// [`Dataset::in_lonlat`] for what leaves the session (an export, a copy, a package, a
    /// published map): a feature with a place that can't be given in longitude/latitude keeps its
    /// attributes and loses its geometry in the copy, rather than failing; returns how many did
    /// ([`unplaced_note`]). Refused only when its system can't be converted at all.
    pub fn in_lonlat_leaving_out(&self) -> std::result::Result<(Cow<'_, Dataset>, usize), String> {
        let to = self.lonlat_conversion()?;
        if to.is_identity() {
            return Ok((Cow::Borrowed(self), 0));
        }
        stop_when_asked(0)?;
        let mut out = self.clone();
        out.held_in = raster::Crs::LonLat;
        let mut unplaced = 0usize;
        for (i, f) in out.features.iter_mut().enumerate() {
            stop_when_asked(i)?;
            if let Some(g) = f.geometry.take() {
                f.geometry = convert_keeping_missing(&to, &g, &HeldPlaces::default());
                unplaced += usize::from(f.geometry.is_none());
            }
        }
        Ok((Cow::Owned(out), unplaced))
    }

    /// The dataset in `crs`, the system a tool works in when it works in its first input's own
    /// ([`Dataset::in_lonlat`] for longitude/latitude): borrowed while its coordinates are already
    /// there; otherwise a copy held there ([`Dataset::hold_in`]), missing coordinates kept.
    /// Refused as [`Dataset::hold_in`] refuses, naming the first feature with a place `crs` can't
    /// hold.
    pub fn in_crs(&self, crs: raster::Crs) -> std::result::Result<Cow<'_, Dataset>, String> {
        if crs == raster::Crs::LonLat {
            return self.in_lonlat();
        }
        if self.coord_crs().to(crs)?.is_identity() {
            return Ok(Cow::Borrowed(self));
        }
        let mut out = self.clone();
        // A copy for one run, not the layer: a window's rows are converted as they are.
        let windowed = std::mem::take(&mut out.windowed);
        out.hold_in(crs)?;
        out.windowed = windowed;
        Ok(Cow::Owned(out))
    }

    /// A longitude/latitude box in its own coordinates ([`Dataset::coord_crs`]), as
    /// [`raster::Reprojection::extent`] converts it: for asking which features a box reaches, once
    /// for the box rather than once a feature. The box itself while they are
    /// longitude/latitude. A box partly outside what its system holds gives the box of
    /// the part that converts, as [`raster::Reprojection::extent`] does; unlike a streamed source's
    /// `extent_from_lonlat`, it isn't refused. Empty when its system can't be converted, which
    /// longitude/latitude never fails to be (see [`Dataset::extent_to_lonlat`]).
    pub fn extent_from_lonlat(&self, e: &Extent) -> Extent {
        match raster::Crs::LonLat.to(self.coord_crs()) {
            Ok(to) => to.extent(e),
            Err(_) => Extent::EMPTY,
        }
    }

    /// `results`, a dataset in longitude/latitude made from this one (a tool's update of it), in
    /// its own coordinates: as it is while they are longitude/latitude; otherwise each geometry
    /// written back as an edit is ([`Dataset::edit_from_lonlat`]), its stored places kept as
    /// stored. Every stored place of `self` is read (for a streamed layer, its window: a place
    /// outside it is converted back, exact to the conversion's round trip, not kept as stored),
    /// but only those the results read as are held, so what is held is as large as the results,
    /// not the layer. Refused, naming the first feature, when one
    /// of its places can't be held in its system. A feature of the results without geometry whose
    /// stored one has a place that can't be given in longitude/latitude, so it was read without it
    /// ([`Dataset::in_lonlat_leaving_out`]), is left as stored, a field the results add empty:
    /// every result field of it is taken from the stored feature (or empty), which assumes the
    /// update's fields are worked out from geometry, as Calculate Geometry Attributes' are, the
    /// only in-place update today. A tool updating attributes alone would need its own values kept.
    pub fn results_from_lonlat(&self, mut results: Dataset) -> std::result::Result<Dataset, String> {
        // Exact through WGS 84 both ways, as the results were read (`in_lonlat`), whatever map
        // the layer is shown on.
        let (read, write) = (self.lonlat_conversion()?, raster::Crs::LonLat.to(self.coord_crs())?);
        if read.is_identity() {
            return Ok(results);
        }
        let mut held = HeldPlaces::default();
        let edits: Vec<&Geometry> = results.features.rows().filter_map(|r| r.geometry()).collect();
        if !edits.is_empty() {
            held.want_only_checking(&edits, stop_when_asked)?;
            for (i, g) in self.features.rows().filter_map(|r| r.geometry()).enumerate() {
                stop_when_asked(i)?;
                Self::hold_places_by(&read, g, None, &mut held);
            }
        }
        // The stored features of the results without geometry whose stored geometry has a place
        // that can't be given in longitude/latitude, by id: only those are converted again. The
        // ids of the results without geometry, sorted (8 bytes each, at most the results' count;
        // nothing when every result has geometry), and of the unplaced among them.
        let mut bare: Vec<u64> = results.features.rows().filter(|r| r.geometry().is_none()).map(|r| r.id()).collect();
        bare.sort_unstable();
        let mut unplaced = std::collections::HashMap::new();
        if !bare.is_empty() {
            for (i, f) in self.features.iter().enumerate() {
                stop_when_asked(i)?;
                if bare.binary_search(&f.id).is_ok()
                    && let Some(g) = &f.geometry
                    && convert_keeping_missing(&read, g, &HeldPlaces::default()).is_none()
                {
                    unplaced.insert(f.id, f);
                }
            }
        }
        // Before the rows are copied out of what shares them.
        stop_when_asked(0)?;
        for (i, f) in results.features.iter_mut().enumerate() {
            stop_when_asked(i)?;
            match f.geometry.take() {
                Some(g) => {
                    f.geometry =
                        Some(Self::edit_from_lonlat_by(&write, g, &held).map_err(|e| format!("{}: feature {}: {e}", self.name, f.id))?);
                }
                // Read without its geometry: left as it was, what the results add to it empty, as
                // nothing was worked out from a geometry it wasn't given.
                None => {
                    if let Some(stored) = unplaced.get(&f.id) {
                        f.geometry.clone_from(&stored.geometry);
                        for (k, field) in results.fields.iter().enumerate() {
                            if let Some(a) = f.attrs.get_mut(k) {
                                *a = self.field_index(&field.name).and_then(|j| stored.attrs.get(j)).cloned().unwrap_or(Value::Null);
                            }
                        }
                    }
                }
            }
        }
        results.held_in = self.held_in;
        Ok(results)
    }

    /// `merged`, its features in longitude/latitude as [`Dataset::in_lonlat`] gives them with some
    /// updated, added or left out (an offline area's sync), in its own coordinates: a feature whose
    /// id isn't in `changed` is taken as it holds it, not converted back; a changed one is written
    /// back as [`Dataset::results_from_lonlat`] writes it, holding only the stored places the
    /// changes read as. As it is while its coordinates are longitude/latitude.
    pub fn merged_from_lonlat(
        &self,
        mut merged: Dataset,
        changed: &std::collections::HashSet<u64>,
    ) -> std::result::Result<Dataset, String> {
        let (read, write) = (self.lonlat_conversion()?, raster::Crs::LonLat.to(self.coord_crs())?);
        if read.is_identity() {
            return Ok(merged);
        }
        let own: std::collections::HashMap<u64, &Feature> = self.features.iter().map(|f| (f.id, f)).collect();
        let is_changed = |id: u64| changed.contains(&id) || !own.contains_key(&id);
        let edits: Vec<&Geometry> = merged.features.iter().filter(|f| is_changed(f.id)).filter_map(|f| f.geometry.as_ref()).collect();
        // Every stored place is read, but only those an edit reads as are kept: what is held is
        // as large as the changes, and a place is found however far its system's inverse is from
        // exact (about a metre and a half in places for Robinson), which no box around the
        // edits would bound.
        let mut held = HeldPlaces::default();
        if !edits.is_empty() {
            held.want_only_checking(&edits, stop_when_asked)?;
            for (i, g) in self.features.iter().filter_map(|f| f.geometry.as_ref()).enumerate() {
                stop_when_asked(i)?;
                Self::hold_places_by(&read, g, None, &mut held);
            }
        }
        // Before the rows are copied out of what shares them.
        stop_when_asked(0)?;
        for (i, f) in merged.features.iter_mut().enumerate() {
            stop_when_asked(i)?;
            if is_changed(f.id) {
                if let Some(g) = f.geometry.take() {
                    f.geometry =
                        Some(Self::edit_from_lonlat_by(&write, g, &held).map_err(|e| format!("{}: feature {}: {e}", self.name, f.id))?);
                }
            } else if let Some(o) = own.get(&f.id) {
                f.geometry = o.geometry.clone();
            }
        }
        merged.held_in = self.held_in;
        Ok(merged)
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
        self.features.add_column(Value::Null);
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

/// Places a dataset already holds, in its own coordinates, by the exact longitude/latitude each
/// reads as: what an edit written back keeps as stored ([`Dataset::edit_from_lonlat`]).
#[derive(Clone, Debug, Default)]
pub struct HeldPlaces {
    places: std::collections::HashMap<(u64, u64), Coord>,
    /// When set, the only readings held from now on ([`HeldPlaces::want_only`]).
    wanted: Option<std::collections::HashSet<(u64, u64)>>,
}

impl HeldPlaces {
    fn key(c: Coord) -> (u64, u64) {
        (c.x.to_bits(), c.y.to_bits())
    }

    /// Adds the places of `g` (those `within` a box, or all), read in longitude/latitude through
    /// `to`.
    fn add(&mut self, to: &raster::Reprojection, g: &Geometry, within: Option<&Extent>) {
        g.for_each_coord(|c| {
            if c.is_finite()
                && within.is_none_or(|e| e.contains(c))
                && let Some(read) = to.point(c)
            {
                let key = Self::key(read);
                if self.wanted.as_ref().is_none_or(|w| w.contains(&key)) {
                    self.places.entry(key).or_insert(c);
                }
            }
        });
    }

    /// From now on, holds only places that one of `edited`'s (in longitude/latitude) reads as and
    /// that isn't held yet: a search of many stored places keeps only those an edit asks for.
    pub fn want_only(&mut self, edited: &[&Geometry]) {
        self.want_only_checking(edited, |_| Ok(())).unwrap_or_default();
    }

    /// [`HeldPlaces::want_only`], stopping when `check` (given each edit's index) says so, the
    /// places wanted left as they were.
    fn want_only_checking(
        &mut self,
        edited: &[&Geometry],
        mut check: impl FnMut(usize) -> std::result::Result<(), String>,
    ) -> std::result::Result<(), String> {
        let mut wanted = std::collections::HashSet::new();
        for (i, g) in edited.iter().enumerate() {
            check(i)?;
            g.for_each_coord(|c| {
                if c.is_finite() && self.get(c).is_none() {
                    wanted.insert(Self::key(c));
                }
            });
        }
        self.wanted = Some(wanted);
        Ok(())
    }

    /// The held place that reads exactly as `c` in longitude/latitude.
    pub fn get(&self, c: Coord) -> Option<Coord> {
        self.places.get(&Self::key(c)).copied()
    }

    pub fn len(&self) -> usize {
        self.places.len()
    }

    pub fn is_empty(&self) -> bool {
        self.places.is_empty()
    }
}

/// Stops a conversion of a whole layer once the command running it was asked to (looked at every
/// 256 features): a layer's conversion takes as long as the layer is large. A single feature
/// converts whole between looks, however many vertices it has (a long coastline is not stopped
/// partway).
fn stop_when_asked(i: usize) -> std::result::Result<(), String> {
    if i.is_multiple_of(256) {
        progress::check().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// An edit `g` in longitude/latitude in the target system, as [`convert_keeping_missing`], a held
/// place written as held; `None` also when it reaches past 85.05° for Web Mercator, which would
/// hold the place at 85.05°, where it ends, rather than refuse it.
fn edit_into_own(to: &raster::Reprojection, g: &Geometry, held: &HeldPlaces) -> Option<Geometry> {
    let mut clamped = false;
    if to.target() == raster::Crs::WebMercator {
        g.for_each_coord(|c| clamped |= c.is_finite() && c.y.abs() > rustgis_geom::MAX_LAT && held.get(c).is_none());
    }
    if clamped { None } else { convert_keeping_missing(to, g, held) }
}

/// `g` converted, its missing coordinates (NaN, infinite) kept as they are and a held place
/// written as held; `None` when a place that is one can't be converted.
fn convert_keeping_missing(to: &raster::Reprojection, g: &Geometry, held: &HeldPlaces) -> Option<Geometry> {
    let lost = std::cell::Cell::new(false);
    let out = g.map_coords(&|c| {
        if !c.is_finite() {
            return c;
        }
        if let Some(own) = held.get(c) {
            return own;
        }
        to.point(c).unwrap_or_else(|| {
            lost.set(true);
            c
        })
    });
    (!lost.get()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A dataset held in another system saves its features there, exactly, with the system
    /// beside them, and reads back held as it was, bit for bit, with no conversion (#2032 C2d):
    /// in UTM, in Robinson and in a system given by its PROJ definition. A window onto a source
    /// saves no rows, and one in longitude/latitude no system, as every release saved them.
    #[test]
    fn a_held_dataset_reads_back_bit_for_bit() {
        let defined =
            raster::Crs::parse("+proj=tmerc +lat_0=0 +lon_0=9 +k=0.9996 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs").unwrap();
        let bits = |ds: &Dataset| -> Vec<Vec<(u64, u64)>> {
            ds.features
                .rows()
                .map(|r| {
                    let mut out = Vec::new();
                    if let Some(g) = r.geometry() {
                        g.for_each_coord(|c| out.push((c.x.to_bits(), c.y.to_bits())));
                    }
                    out
                })
                .collect()
        };
        for crs in [raster::Crs::Epsg(32632), raster::Crs::parse("ESRI:54030").unwrap(), defined] {
            let mut ds = Dataset::new("held", GeometryType::Polyline, vec![Field::new("n", FieldType::Integer)]);
            let line = |x: f64| vec![Coord::new(x, 48.7), Coord::new(x + 0.013, 48.71), Coord::new(x + 0.031, 48.69)];
            ds.push(Some(Geometry::Line(vec![line(11.5), line(11.6)])), vec![Value::Int(1)]);
            ds.push(None, vec![Value::Int(2)]);
            ds.push(Some(Geometry::Line(vec![line(8.1)])), vec![Value::Int(3)]);
            ds.hold_in(crs).unwrap();
            // Through JSON text, as a project saves it.
            let saved = serde_json::to_string(&ds).unwrap();
            let held_in = serde_json::from_str::<serde_json::Value>(&saved).unwrap()["held_in"].clone();
            assert_eq!(held_in, serde_json::to_value(crs).unwrap(), "{}", crs.label());
            let back: Dataset = serde_json::from_str(&saved).unwrap();
            assert_eq!(back.coord_crs(), crs, "{}", crs.label());
            assert_eq!(bits(&back), bits(&ds), "{}", crs.label());
            assert_eq!(back.features.len(), 3);
        }
        let mut window = Dataset::new("window", GeometryType::Point, vec![]);
        window.push(Some(Geometry::Point(Coord::new(11.5, 48.7))), vec![]);
        window.hold_in(raster::Crs::Epsg(32632)).unwrap();
        window.windowed = true;
        let v = serde_json::to_value(&window).unwrap();
        assert!(v.get("held_in").is_none() && v["features"] == serde_json::json!([]), "{v}");
        let mut plain = Dataset::new("plain", GeometryType::Point, vec![]);
        plain.push(Some(Geometry::Point(Coord::new(11.5, 48.7))), vec![]);
        let v = serde_json::to_value(&plain).unwrap();
        assert!(v.get("held_in").is_none(), "{v}");
        assert_eq!(serde_json::from_value::<Dataset>(v).unwrap().coord_crs(), raster::Crs::LonLat);
    }

    /// A layer made from another is held as that one is, with its system for exports; one with a
    /// place the system can't hold stays in longitude/latitude with no system of its own.
    #[test]
    fn a_layer_made_from_another_is_held_as_it_is() {
        let utm = raster::Crs::Epsg(32632);
        let mut ds = Dataset::new("made", GeometryType::Point, vec![]);
        ds.push(Some(Geometry::Point(Coord::new(11.5, 48.7))), vec![]);
        ds.hold_as(utm, Some(utm)).unwrap();
        assert_eq!((ds.coord_crs(), ds.crs), (utm, Some(utm)));
        let mut polar = Dataset::new("polar", GeometryType::Point, vec![]);
        polar.push(Some(Geometry::Point(Coord::new(11.5, 89.0))), vec![]);
        assert!(polar.hold_as(raster::Crs::WebMercator, Some(raster::Crs::WebMercator)).is_err());
        assert_eq!((polar.coord_crs(), polar.crs), (raster::Crs::LonLat, None));
    }

    /// Holding converts the rows where they are, with no copy of rows no one else holds; a Cancel,
    /// or a place the system can't hold, found among the last rows changes none of them.
    #[test]
    fn holding_converts_in_place_and_a_cancel_or_refusal_changes_nothing() {
        use crate::progress::tests::Counted;
        let utm = raster::Crs::Epsg(32632);
        let made = |last_lat: f64| {
            let mut ds = Dataset::new("made", GeometryType::Point, vec![]);
            for i in 0..600 {
                let lat = if i == 599 { last_lat } else { 48.0 };
                ds.push(Some(Geometry::Point(Coord::new(11.5, lat))), vec![]);
            }
            ds
        };
        let mut ds = made(48.0);
        let (held, copied) = rows_copy::with(Box::new(|| {}), || ds.hold_in(utm));
        held.unwrap();
        assert!(!copied && ds.coord_crs() == utm, "converted where the rows are");
        let Some(Geometry::Point(c)) = ds.features[0].geometry else { panic!("no point") };
        let expected = raster::Crs::LonLat.to(utm).unwrap().point(Coord::new(11.5, 48.0)).unwrap();
        assert_eq!((c.x.to_bits(), c.y.to_bits()), (expected.x.to_bits(), expected.y.to_bits()));
        // Cancelled at the third check (row 512), after the first rows were looked at.
        let mut ds = made(48.0);
        let before = ds.features.clone();
        let probe = std::sync::Arc::new(Counted { after_checks: Some(2), ..Default::default() });
        {
            let _g = crate::progress::install(Some(probe));
            assert_eq!(ds.hold_in(utm), Err(DataError::Cancelled.to_string()));
        }
        assert!(ds.features.shares(&before) && ds.coord_crs() == raster::Crs::LonLat);
        let mut polar = made(89.0);
        let before = polar.features.clone();
        assert!(polar.hold_in(raster::Crs::WebMercator).unwrap_err().contains("feature 600"));
        assert!(polar.features.shares(&before) && polar.coord_crs() == raster::Crs::LonLat);
    }

    /// A dataset held in another system converts into it, and back, at the same places; one
    /// reaching past where Web Mercator ends, or a window onto a source read in place, is refused
    /// and left as it was.
    #[test]
    fn a_dataset_is_held_where_its_places_convert() {
        let utm = raster::Crs::Epsg(32632);
        let mut ds = Dataset::new("held", GeometryType::Point, vec![]);
        ds.push(Some(Geometry::Point(Coord::new(11.5, 48.7))), vec![]);
        ds.push(Some(Geometry::Point(Coord::new(f64::NAN, 48.7))), vec![]);
        let plain = ds.clone();
        ds.hold_in(utm).unwrap();
        assert_eq!(ds.coord_crs(), utm);
        let Some(Geometry::Point(p)) = ds.features[0].geometry else { panic!("a point") };
        assert_eq!(Some(p), utm.transformer().unwrap().from_lonlat(Coord::new(11.5, 48.7)));
        assert!(matches!(ds.features[1].geometry, Some(Geometry::Point(c)) if c.x.is_nan()));
        ds.hold_in(raster::Crs::LonLat).unwrap();
        let Some(Geometry::Point(back)) = ds.features[0].geometry else { panic!("a point") };
        assert!((back.x - 11.5).abs() < 1e-9 && (back.y - 48.7).abs() < 1e-9, "{back:?}");
        // Past 85.05°: refused, nothing changed.
        let mut polar = plain.clone();
        polar.push(Some(Geometry::Point(Coord::new(0.0, 89.0))), vec![]);
        let before = polar.clone();
        let err = polar.hold_in(raster::Crs::WebMercator).unwrap_err();
        assert!(err.contains("feature 3"), "{err}");
        assert_eq!(polar, before);
        // A window onto a source is read again in longitude/latitude.
        let mut window = plain;
        window.windowed = true;
        assert!(window.hold_in(utm).is_err());
        assert_eq!(window.coord_crs(), raster::Crs::LonLat);
    }

    /// A whole layer's conversion stops once its command is cancelled, part-way through as well as
    /// before it starts, leaving the dataset as it was; without a cancel it runs to the end.
    #[test]
    fn a_whole_layer_conversion_stops_when_cancelled() {
        use std::sync::atomic::Ordering;
        let utm = raster::Crs::Epsg(32632);
        let mut ds = Dataset::new("held", GeometryType::Point, vec![]);
        for i in 0..600 {
            ds.push(Some(Geometry::Point(Coord::new(11.0 + f64::from(i) * 1e-4, 48.7))), vec![]);
        }
        let plain = ds.clone();
        // Cancelled before it starts, or once asked `n` times.
        let before = || {
            let probe = std::sync::Arc::new(progress::tests::Counted::default());
            probe.cancel.store(true, Ordering::Relaxed);
            progress::install(Some(probe))
        };
        let at =
            |n: u64| progress::install(Some(std::sync::Arc::new(progress::tests::Counted { after_checks: Some(n), ..Default::default() })));
        // A conversion checks before its loop, then at features 0, 256 and 512: the third check
        // stops it at feature 256, part-way.
        let midway = || at(2);
        // A write-back first looks over its edits (checks at edits 0, 256 and 512 of 600; at the
        // one edit of a merge changing one feature), reads the held places (checks at features
        // 0, 256 and 512), then checks before its copy and at features 0 and 256 of it: the copy
        // stops at feature 256, part-way.
        let results_midway = || at(8);
        let merge_midway = || at(6);
        for cancel in [&before as &dyn Fn() -> progress::Installed, &midway] {
            let _cancelled = cancel();
            assert!(ds.hold_in(utm).is_err());
            assert_eq!(ds, plain, "a cancelled hold leaves the dataset as it was");
        }
        ds.hold_in(utm).unwrap();
        let held = ds.clone();
        let merged = ds.in_lonlat().unwrap().into_owned();
        type Cancel<'a> = &'a dyn Fn() -> progress::Installed;
        for (cancel, results, merge) in
            [(&before as Cancel, &before as Cancel, &before as Cancel), (&midway, &results_midway, &merge_midway)]
        {
            {
                let _cancelled = cancel();
                assert!(ds.in_lonlat().is_err());
            }
            {
                let _cancelled = results();
                assert!(ds.results_from_lonlat(merged.clone()).is_err());
            }
            {
                let _cancelled = merge();
                assert!(ds.merged_from_lonlat(merged.clone(), &std::collections::HashSet::from([1])).is_err());
            }
            assert_eq!(ds, held);
        }
        let _cancelled = before();
        // Said in words, read back as a cancel by those that wrap it.
        assert!(matches!(ds.in_lonlat().map_err(DataError::of_conversion), Err(DataError::Cancelled)));
        assert!(matches!(DataError::of_conversion("no such system".into()), DataError::Format(_)));
    }

    /// A merge in longitude/latitude written back to a held dataset: what it left as it was stays
    /// as stored, a changed feature keeps the places it didn't move, and only a moved or new place
    /// is converted.
    #[test]
    fn a_merge_converts_back_only_what_it_changed() {
        // UTM, and Robinson, whose inverse is a metre or so from exact in places.
        for crs in [raster::Crs::Epsg(32632), raster::Crs::parse("ESRI:54030").unwrap()] {
            let mut ds = Dataset::new("held", GeometryType::Point, vec![Field::new("NAME", FieldType::Text)]);
            for (i, x) in [11.1, 11.3, 11.5].into_iter().enumerate() {
                ds.push(Some(Geometry::Point(Coord::new(x, 48.7 + 0.01 * i as f64))), vec![Value::Text(format!("p{i}"))]);
            }
            ds.hold_in(crs).unwrap();
            let mut merged = ds.in_lonlat().unwrap().into_owned();
            let mut rows: Vec<Feature> = merged.features.iter().cloned().collect();
            let ids: Vec<u64> = rows.iter().map(|f| f.id).collect();
            rows[1].attrs[0] = Value::Text("renamed".into());
            let Some(Geometry::Point(moved)) = &mut rows[2].geometry else { panic!("a point") };
            moved.y += 0.001;
            let moved = *moved;
            let new = Coord::new(11.4, 48.75);
            rows.push(Feature { id: 99, geometry: Some(Geometry::Point(new)), ..rows[0].clone() });
            merged.features = rows.into();
            let changed = std::collections::HashSet::from([ids[1], ids[2], 99]);
            let back = ds.merged_from_lonlat(merged, &changed).unwrap();
            assert_eq!(back.coord_crs(), crs);
            let to = crs.transformer().unwrap();
            let stored: Vec<_> = ds.features.iter().map(|f| f.geometry.clone()).collect();
            let got: Vec<_> = back.features.iter().map(|f| f.geometry.clone()).collect();
            assert_eq!(got.len(), 4);
            assert_eq!(got[0], stored[0]);
            assert_eq!(got[1], stored[1]);
            assert_eq!(back.features.get(1).unwrap().attrs[0], Value::Text("renamed".into()));
            assert_eq!(got[2], Some(Geometry::Point(to.from_lonlat(moved).unwrap())));
            assert_eq!(got[3], Some(Geometry::Point(to.from_lonlat(new).unwrap())));
        }
    }

    /// An export in Web Mercator counts the places past where it ends by their latitude, from the
    /// system the layer holds its coordinates in (UTM 32N here): one at 86° N, not every northing.
    #[test]
    fn web_mercators_end_is_counted_by_latitude() {
        let utm = raster::Crs::Epsg(32632);
        let mut ds = Dataset::new("held", GeometryType::Point, vec![]);
        for lat in [48.7, 86.0] {
            ds.push(Some(Geometry::Point(Coord::new(9.0, lat))), vec![]);
        }
        ds.hold_in(utm).unwrap();
        assert_eq!(ds.coord_crs(), utm);
        let note = web_mercator_note(&[(&ds, ds.extent())], "out.gpkg");
        assert_eq!(note, web_mercator_said(&[("held", Some(1))], "out.gpkg"));
        assert_eq!(note.and_then(|n| n.loss), Some(loss::LossCode::ChangedPosition));
    }

    /// Analysis and measurement read a longitude/latitude dataset as it is: no copy, and the
    /// measure and the area check of the geometry itself.
    #[test]
    fn a_lonlat_dataset_is_read_as_it_is() {
        let ring = vec![Coord::new(0.0, 0.0), Coord::new(1.0, 0.0), Coord::new(1.0, 1.0), Coord::new(0.0, 1.0), Coord::new(0.0, 0.0)];
        let g = Geometry::Polygon(vec![vec![ring]]);
        let mut d = Dataset::new("t", GeometryType::Polygon, Vec::new());
        d.push(Some(g.clone()), Vec::new());
        assert!(matches!(d.in_lonlat(), Ok(Cow::Borrowed(b)) if std::ptr::eq(b, &d)));
        assert_eq!(d.wgs84_measure(&g, Measure::Area), Ok(g.wgs84_measure(Measure::Area)));
        assert!(d.wgs84_measure(&g, Measure::Area).unwrap().is_some_and(|a| a > 1.2e10));
        assert_eq!(d.ambiguous_area_message(std::iter::once(&g)), None);
        let results = d.clone();
        assert_eq!(d.results_from_lonlat(results.clone()).map(|r| r.features), Ok(results.features));
    }

    /// An edit's conversion keeps a missing coordinate (NaN) as it is, as edits in
    /// longitude/latitude do, converts the rest, and refuses a place that can't be converted:
    /// UTM 32N can't bring back one on the equator 90° from its central meridian.
    #[test]
    fn an_edits_conversion_keeps_missing_coordinates() {
        let to = raster::Crs::LonLat.to(raster::Crs::Epsg(32632)).unwrap();
        let t = raster::Crs::Epsg(32632).transformer().unwrap();
        let g = Geometry::Line(vec![vec![Coord::new(9.0, 47.0), Coord::new(f64::NAN, 47.0)]]);
        let Some(Geometry::Line(parts)) = convert_keeping_missing(&to, &g, &HeldPlaces::default()) else { panic!("converted") };
        assert_eq!(parts[0][0], t.from_lonlat(Coord::new(9.0, 47.0)).unwrap());
        assert!(parts[0][1].x.is_nan() && parts[0][1].y == 47.0, "{:?}", parts[0][1]);
        let lost = Geometry::Line(vec![vec![Coord::new(9.0, 47.0), Coord::new(99.0, 0.0)]]);
        assert!(convert_keeping_missing(&to, &lost, &HeldPlaces::default()).is_none());
        // Web Mercator would hold 89° at 85.05°: an edit reaching there is refused; one within
        // 85.05°, or missing a coordinate, is written.
        let merc = raster::Crs::LonLat.to(raster::Crs::WebMercator).unwrap();
        assert!(edit_into_own(&merc, &Geometry::Point(Coord::new(10.0, 89.0)), &HeldPlaces::default()).is_none());
        assert!(edit_into_own(&merc, &Geometry::Point(Coord::new(10.0, 80.0)), &HeldPlaces::default()).is_some());
        assert!(edit_into_own(&merc, &Geometry::Point(Coord::new(10.0, f64::NAN)), &HeldPlaces::default()).is_some());
        assert!(edit_into_own(&merc, &Geometry::Point(Coord::new(f64::NAN, 89.0)), &HeldPlaces::default()).is_some());
    }

    /// An edit written back keeps every place it didn't move exactly as stored, even through
    /// Robinson, whose inverse is approximate; a moved or inserted place is converted, and one
    /// snapped to another feature's place takes that place as stored. Moved places checked with
    /// PROJ 9.6.0: `printf "21 61\n25 65\n" | cs2cs -f %.6f +proj=longlat +datum=WGS84 +to ESRI:54030`.
    #[test]
    fn an_edit_written_back_keeps_what_it_did_not_move() {
        let rob = raster::Crs::parse("ESRI:54030").unwrap();
        let (to_own, to_lonlat) = (raster::Crs::LonLat.to(rob).unwrap(), rob.to(raster::Crs::LonLat).unwrap());
        let own = |c: Coord| to_own.point(c).unwrap();
        let stored: Vec<Coord> = [(10.0, 50.0), (20.0, 60.0), (30.0, 70.0), (170.0, 80.0)].map(|(x, y)| own(Coord::new(x, y))).to_vec();
        let other = own(Coord::new(-40.0, 30.5));
        let (feature, neighbour) = (Geometry::Line(vec![stored.clone()]), Geometry::Point(other));
        let mut held = HeldPlaces::default();
        held.add(&to_lonlat, &feature, None);
        held.add(&to_lonlat, &neighbour, None);
        assert_eq!(held.len(), 5);
        // Wanting only one reading, a search holds no other.
        let mut only = HeldPlaces::default();
        only.want_only(&[&Geometry::Point(to_lonlat.point(other).unwrap())]);
        only.add(&to_lonlat, &feature, None);
        only.add(&to_lonlat, &neighbour, None);
        assert_eq!(only.len(), 1);
        // As the edit reads them: the feature's places in longitude/latitude.
        let read: Vec<Coord> = stored.iter().map(|c| to_lonlat.point(*c).unwrap()).collect();
        let snapped = to_lonlat.point(other).unwrap();
        // The second place moved, one inserted after it, and the last snapped to the neighbour.
        let edited = Geometry::Line(vec![vec![read[0], Coord::new(21.0, 61.0), Coord::new(25.0, 65.0), read[2], read[3], snapped]]);
        let Some(Geometry::Line(parts)) = edit_into_own(&to_own, &edited, &held) else { panic!("written") };
        let out = &parts[0];
        for (i, s) in [(0, 0), (3, 2), (4, 3)] {
            assert_eq!((out[i].x.to_bits(), out[i].y.to_bits()), (stored[s].x.to_bits(), stored[s].y.to_bits()), "place {i}");
        }
        assert_eq!((out[5].x.to_bits(), out[5].y.to_bits()), (other.x.to_bits(), other.y.to_bits()));
        for (c, proj) in [(out[1], (1569350.169588, 6433621.288413)), (out[2], (1794351.609176, 6816459.832181))] {
            assert!((c.x - proj.0).abs() < 1e-3 && (c.y - proj.1).abs() < 1e-3, "{c:?} vs PROJ {proj:?}");
        }
        // Without them held, the same edit is converted whole, as a fresh drawing is.
        let Some(Geometry::Line(whole)) = edit_into_own(&to_own, &edited, &HeldPlaces::default()) else { panic!("written") };
        assert_eq!(whole[0][1], out[1]);
    }

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
