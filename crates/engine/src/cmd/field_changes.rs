//! What a field package brings back: the phone's changes, not its map (#2468).
//!
//! When a field package is opened, the rows of its feature layers as they were given are written to
//! a folder of this device's own, named by the package's identity ([`FieldOrigin::package`]): each
//! row once, in a file read a row at a time, and beside it an index of each row's id, place in the
//! file and fingerprint. Send Field Package fingerprints the rows the map holds then: a row whose
//! fingerprint differs has its given copy read from the file and is compared with it, attribute by
//! attribute; an id the map no longer has was deleted, one the copy hasn't was added. So every edit
//! is found, however it was made, with no second copy of a layer held in memory, and the copy is
//! still found after the field project is saved and opened again, or recovered.
//!
//! The returned package carries the list ([`ENTRY`]): per layer, the features added, those deleted
//! with their values and shape as given, and for each feature edited the attributes (by name) and
//! the shape it changed, with their values as given. The new values are the package's map's own.
//! Bring Back applies the list to the home map: a change whose value as given is still the home
//! map's goes in; one changed at home too is a conflict that the field's value wins, said by
//! feature. Nothing else is compared, so a field the office added, or a second phone's untouched
//! rows, are left alone.
//!
//! [`FieldOrigin::package`]: crate::project::FieldOrigin::package

use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use rustgis_data::{Geometry, RowRef, Value};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::Session;
use crate::project::{ConflictKind, Project};

/// The package entry holding the list of changes.
pub(crate) const ENTRY: &str = "changes.v1.json";
/// The entry of a package going out from home, naming its identity: only its rows are kept as
/// given. A returned package keeps the identity but never has it, so a device opening a return
/// never takes its edited rows for those home gave.
pub(crate) const GIVEN: &str = "given.v1.json";
/// The version of the list this RustGIS writes and reads.
const VERSION: u32 = 1;
/// The bytes of one index record: id, place in the rows file, length, fingerprint.
const RECORD: usize = 8 + 8 + 4 + 16;
/// The longest row read back from a given copy, in bytes.
const MAX_ROW: u32 = 256 << 20;
/// The file listing a given copy's layers, written last: a folder without it is incomplete.
const LAYERS: &str = "layers.json";

/// The changes of one returning package.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct Changes {
    pub version: u32,
    /// The package's identity ([`crate::project::FieldOrigin::package`]).
    pub package: String,
    pub layers: Vec<LayerChanges>,
    /// The layers the package gave that the field removed, by name: said on sending, as their
    /// features go back unchanged.
    #[serde(skip)]
    pub gone: Vec<String>,
}

/// The changes to one layer, named by its id.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct LayerChanges {
    pub layer: u32,
    /// The ids of the features added in the field: their rows are the package map's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added: Vec<u64>,
    /// The features deleted in the field, with every value and the shape they had as given.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deleted: Vec<Given>,
    /// The features edited in the field, with the values and shape they changed, as given. Their
    /// new values are the package map's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edited: Vec<Given>,
    /// The fields the package gave that the field removed, by name: their values aren't listed, so
    /// Bring Back refuses the list while the home layer still has them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<String>,
    /// The fields the field added that the package didn't give: the only fields Bring Back adds
    /// to the home layer, so a field removed at home meanwhile isn't added back.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<rustgis_data::Field>,
}

/// A feature's values as the package gave them.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Given {
    pub id: u64,
    /// Attribute values by field name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<(String, Value)>,
    /// The shape, when it is part of the change (`null` for none).
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    pub geometry: Option<Option<Geometry>>,
}

/// Reads a field that is there, even as `null`, as `Some`.
fn present<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    T::deserialize(d).map(Some)
}

impl Changes {
    /// Reads the list from a package entry, refusing one from a newer RustGIS, one of another
    /// package, and one naming a feature twice in a layer.
    pub(crate) fn read(bytes: &[u8], package: &str) -> Result<Changes, String> {
        #[derive(Deserialize)]
        struct Header {
            version: u32,
        }
        let damaged = |e: serde_json::Error| format!("the field package's list of changes is damaged: {e}");
        let header: Header = serde_json::from_slice(bytes).map_err(damaged)?;
        if header.version > VERSION {
            return Err("the field package's list of changes was made by a newer RustGIS: update RustGIS to bring it back".into());
        }
        if header.version != VERSION {
            return Err(format!(
                "the field package's list of changes is of version {}, which no RustGIS writes: it is damaged",
                header.version
            ));
        }
        let changes: Changes = serde_json::from_slice(bytes).map_err(damaged)?;
        if changes.package != package {
            return Err("the field package's list of changes is of another package: make a new field package".into());
        }
        let mut layers = HashSet::new();
        for l in &changes.layers {
            let mut ids = HashSet::new();
            let all = l.added.iter().copied().chain(l.deleted.iter().map(|g| g.id)).chain(l.edited.iter().map(|g| g.id));
            if !layers.insert(l.layer) || !all.into_iter().all(|id| ids.insert(id)) {
                return Err("the field package's list of changes names a layer or a feature twice: it is damaged".into());
            }
        }
        Ok(changes)
    }
}

// The copy of the rows a package gave, on the phone.

/// The folder the given copies are kept in: the app's own `field bases`.
fn bases() -> Option<PathBuf> {
    #[cfg(test)]
    {
        test_bases::root()
    }
    #[cfg(not(test))]
    {
        rustgis_data::dirs::app_data_dir().map(|d| d.join("field bases"))
    }
}

/// The given copy of package `package`'s rows, when this device has one.
fn base_dir(package: &str) -> Option<PathBuf> {
    crate::project::valid_field_id(package).then(|| bases().map(|b| b.join(package))).flatten()
}

/// The mark that package `package`'s return was sent, beside where its copy is kept: it outlasts a
/// restart, so the copy is never made again ([`forget`]).
fn spent_mark(package: &str) -> Option<PathBuf> {
    crate::project::valid_field_id(package).then(|| bases().map(|b| b.join(format!("{package}.spent")))).flatten()
}

/// One layer of a given copy, as [`LAYERS`] lists it.
#[derive(Serialize, Deserialize)]
struct BaseLayer {
    layer: u32,
    /// Its name, to say so should the field remove it.
    #[serde(default)]
    name: String,
    /// Its fields' names, in the order the rows hold their values.
    fields: Vec<String>,
    rows: usize,
}

/// A row as the rows file holds it.
#[derive(Deserialize)]
struct BaseRow {
    geometry: Option<Geometry>,
    values: Vec<Value>,
}

/// A row's shape and values as they are written and fingerprinted: `fields[i]` is where the row
/// holds the copy's field `i` (`None`: Null).
struct Print<'a> {
    row: RowRef<'a>,
    /// The row's shape in longitude/latitude (converted one row at a time, never the layer).
    geometry: Option<&'a Geometry>,
    fields: &'a [Option<usize>],
}

impl Serialize for Print<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        struct Values<'a>(&'a Print<'a>);
        impl Serialize for Values<'_> {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                let Print { row, fields, .. } = self.0;
                s.collect_seq(fields.iter().map(|j| j.map_or(&Value::Null, |j| row.value(j))))
            }
        }
        use serde::ser::SerializeStruct;
        let mut out = s.serialize_struct("BaseRow", 2)?;
        out.serialize_field("geometry", &self.geometry)?;
        out.serialize_field("values", &Values(self))?;
        out.end()
    }
}

/// A 128-bit FNV-1a hash of what is written to it.
struct Fnv(u128);

impl Write for Fnv {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        for b in bytes {
            self.0 = (self.0 ^ u128::from(*b)).wrapping_mul(0x0000_0000_0100_0000_0000_0000_0000_013B);
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// FNV-1a's 128-bit offset basis.
const FNV_BASIS: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;

/// The fingerprint of a row as [`Print`] writes it, in longitude/latitude. Rows are fingerprinted
/// as they are held once read, given and sent alike, so a value that reads back as it was saved
/// (as every one does) can't make an unedited row look edited.
fn fingerprint(print: &Print<'_>) -> Result<[u8; 16], String> {
    let mut hash = Fnv(FNV_BASIS);
    serde_json::to_writer(&mut hash, print).map_err(|e| e.to_string())?;
    Ok(hash.0.to_le_bytes())
}

/// One row of a layer's index: its id, where it is in the rows file, and its fingerprint (40
/// bytes held).
#[derive(Clone, Copy)]
struct Entry {
    id: u64,
    offset: u64,
    len: u32,
    print: [u8; 16],
}

/// The rows a package from home gave, taken when it opens: its feature layers, sharing their rows
/// with the open project, so taking them copies none.
#[derive(Debug)]
pub(crate) struct GivenRows {
    package: String,
    layers: Vec<(u32, String, rustgis_data::Dataset)>,
}

impl GivenRows {
    /// The rows `project`'s map holds, to keep as package `package` gave them.
    pub(crate) fn of(project: &Project, package: &str) -> GivenRows {
        let layers = project
            .maps
            .first()
            .into_iter()
            .flat_map(|m| &m.layers)
            .filter(|l| !l.is_raster() && !l.is_streamed())
            .map(|l| (l.id, l.name.clone(), l.dataset.clone()))
            .collect();
        GivenRows { package: package.to_string(), layers }
    }

    /// The rows `project`, just read from a package unpacked into `dir`, was given by home: when the
    /// package says it went out from home ([`GIVEN`]) under the identity `project` carries, and this
    /// device doesn't keep them already.
    pub(crate) fn in_package(dir: &Path, project: &Project) -> Option<GivenRows> {
        let package = project.field_origin.as_ref().map(|o| o.package.as_str()).filter(|p| !p.is_empty())?;
        let given = marks(dir, package).then(|| GivenRows::of(project, package))?;
        (!given.kept()).then_some(given)
    }

    /// Whether this device keeps them already.
    pub(crate) fn kept(&self) -> bool {
        base_dir(&self.package).is_some_and(|d| d.join(LAYERS).is_file())
    }
}

/// Removes the rows package `package` gave this device, once its return has been written: home
/// brings a package back once, so they are no longer needed. Whether there were any. The package
/// is marked spent first, on disk ([`spent_mark`]), so opening it again, after a restart too, keeps
/// nothing; a mark that can't be written is an error, and the copy is left.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn forget(package: &str) -> Result<bool, String> {
    // A keep of its rows still running lets its copy go once it is in place.
    SPENT.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(package.to_string());
    if let Some(mark) = spent_mark(package) {
        if let Some(parent) = mark.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        std::fs::write(&mark, b"").map_err(|e| format!("{}: {e}", mark.display()))?;
    }
    let Some(dir) = base_dir(package).filter(|d| d.exists()) else { return Ok(false) };
    // Moved out of its place first, whole, so a removal cut short never leaves half a copy there:
    // a later keep finds the place free. Moved, it is removed; one that can't be is named.
    let gone = dir.with_extension(format!("{}.gone", crate::project::field_id()?));
    std::fs::rename(&dir, &gone).map_err(|e| format!("{}: {e}", dir.display()))?;
    std::fs::remove_dir_all(&gone).map_err(|e| format!("{}: {e}; remove the folder by hand", gone.display()))?;
    Ok(true)
}

/// The packages whose returns this run of RustGIS has sent ([`forget`]), as well as marked on disk.
static SPENT: std::sync::Mutex<std::collections::BTreeSet<String>> = std::sync::Mutex::new(std::collections::BTreeSet::new());

/// Whether package `package`'s return has been sent, in this run or an earlier one.
fn is_spent(package: &str) -> bool {
    SPENT.lock().unwrap_or_else(std::sync::PoisonError::into_inner).contains(package) || spent_mark(package).is_some_and(|m| m.is_file())
}

/// What a keep says when its package has gone back already.
const ALREADY_SENT: &str =
    "The field package had already gone back, so the rows it gave weren't kept on this device: make a new field package for more edits";

/// Whether `dir`, a package unpacked, went out from home as package `package`: its [`GIVEN`] names
/// it.
pub(crate) fn marks(dir: &Path, package: &str) -> bool {
    marked(dir, package).unwrap_or(false)
}

/// [`marks`], telling a marker that isn't there (`Ok(false)`, a return or another kind of folder)
/// from one that is there but can't be read (`Err`, a damaged package).
pub(crate) fn marked(dir: &Path, package: &str) -> Result<bool, String> {
    #[derive(Deserialize)]
    struct Marker {
        package: String,
    }
    let file = dir.join(GIVEN);
    if package.is_empty() || matches!(std::fs::symlink_metadata(&file), Err(e) if e.kind() == std::io::ErrorKind::NotFound) {
        return Ok(false);
    }
    let bytes = crate::project::read_file(&file, 4096)?;
    let marker: Marker = serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", file.display()))?;
    Ok(marker.package == package)
}

/// What a package going out from home holds under [`GIVEN`].
pub(crate) fn given_entry(package: &str) -> Result<Vec<u8>, String> {
    serde_json::to_vec(&json!({"package": package})).map_err(|e| e.to_string())
}

/// What [`keep_base`] made: the notices of what was let go (text, and whether it is an error), or
/// why the rows couldn't be kept.
type Kept = Result<Vec<(String, bool)>, String>;

/// What [`keep_ahead`] made, for `offline.keepRows` to take: whether there were rows to keep, and
/// what keeping them made.
type KeptAhead = (bool, Kept);

/// The key `offline.keepRows`'s read is taken under.
const KEEP_KEY: &str = "offline.keepRows";

/// Keeping the rows the open field package gave, on a thread of its own: see [`keep_base`]. The
/// read takes them out of the project, so whatever ends it (done, failed, cancelled, or the read
/// left), they are let go with it, and an edit made meanwhile copies its layer only while the
/// read runs. With none waiting (kept already) it does nothing.
pub(crate) fn keep_ahead(s: &mut Session) -> Option<super::reads::ReadWork> {
    let given = s.project.given.take();
    Some(super::reads::ahead("Keep Field Package Rows", KEEP_KEY.into(), move || -> KeptAhead {
        match given {
            Some(given) => (true, keep_base(&given)),
            None => (false, rustgis_data::progress::check().map(|()| Vec::new()).map_err(|e| e.to_string())),
        }
    }))
}

/// `offline.keepRows`: keeps on this device the rows the open field package gave
/// ([`keep_base`]), so Send Field Package can tell what changed. Opening the package asks for it;
/// in the app it runs on a thread of its own, with progress and Cancel. Cancelled or failed, the
/// rows aren't kept, the next opening of the package keeps them, and meanwhile Send Field Package
/// sends the whole map and says so. `kept` says whether there were rows to keep and they are on
/// this device: not when the package has gone back already.
pub(crate) fn keep_rows(s: &mut Session, _p: &serde_json::Value) -> crate::CmdResult {
    let ahead = super::reads::take::<KeptAhead>(s, KEEP_KEY);
    #[cfg(not(target_arch = "wasm32"))]
    if ahead.is_none() && s.project.given.is_some() && s.statistics_window_thread == Some(std::thread::current().id()) {
        return Err("keeping a field package's rows needs the background read queue".into());
    }
    // A read's answer: its rows were taken out of the project when it started. Kept here, they are
    // taken out of the project, and let go once kept, however that ends. Only the path that keeps
    // them takes them, so rows another opening left waiting are never dropped unkept.
    let (had, done) = match ahead {
        Some(done) => done,
        None => match s.project.given.take() {
            Some(given) => (true, keep_base(&given)),
            None => return Ok(json!({"kept": false})),
        },
    };
    let notes = done.map_err(|e| {
        if e == rustgis_data::DataError::Cancelled.to_string() {
            e
        } else {
            format!(
                "The rows this field package gave couldn't be kept on this device ({e}): Send Field Package sends the whole map, which Bring Back refuses if its offline area has been brought back or synced since the package was made"
            )
        }
    })?;
    // A package gone back already keeps nothing, though its rows were waiting.
    let kept = had && notes.iter().all(|(text, _)| text != ALREADY_SENT);
    for (text, error) in notes {
        s.notify(text, error);
    }
    Ok(json!({"kept": kept}))
}

/// Keeps the rows `given` holds on this device under its package's identity, unless it keeps them
/// already. The files are written beside the folder and moved into place once complete, so a copy
/// cut short (cancelled, or failed) is never read and the next open of the package writes it anew.
/// Reads each row once and writes it once, in longitude/latitude as a returning map is compared;
/// Cancel is looked at between rows, and progress counts them.
pub(crate) fn keep_base(given: &GivenRows) -> Kept {
    rustgis_data::progress::check().map_err(|e| e.to_string())?;
    let dir = base_dir(&given.package).ok_or("this device gives RustGIS no folder of its own to keep the field package's rows in")?;
    if dir.join(LAYERS).is_file() {
        return Ok(Vec::new());
    }
    // Its return sent before it was opened again, maybe in an earlier run: nothing is kept.
    if is_spent(&given.package) {
        return Ok(vec![(ALREADY_SENT.into(), false)]);
    }
    let parent = dir.parent().ok_or("no folder for the field package's rows")?;
    std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    let part = parent.join(format!("{}.{}.part", given.package, crate::project::field_id()?));
    let written = write_base(given, &part).and_then(|()| publish(&part, &dir));
    let mut notes = Vec::new();
    if part.exists() {
        notes.extend(let_go(&part, "a copy of the field package's rows cut short"));
    }
    // Its return sent while it was kept: the copy just put in place is no longer needed.
    if written.is_ok() && is_spent(&given.package) {
        notes.extend(let_go(&dir, "the rows of a field package already sent back"));
        if notes.iter().all(|(_, error)| !error) {
            notes.push((ALREADY_SENT.into(), false));
        }
    }
    outcome(written, notes)
}

/// What a keep says: on success, the notices of what it let go; on a failure, the failure with any
/// folder it couldn't remove after it, so neither is lost.
fn outcome(written: Result<(), String>, notes: Vec<(String, bool)>) -> Kept {
    match written {
        Ok(()) => Ok(notes),
        Err(e) if notes.is_empty() => Err(e),
        Err(e) => Err(format!("{e}; {}", notes.into_iter().map(|(text, _)| text).collect::<Vec<_>>().join("; "))),
    }
}

/// Removes `dir`, `what`: nothing to say when it goes, or is gone already (a send that raced the
/// keep removed it), the folder and why when it can't.
fn let_go(dir: &Path, what: &str) -> Option<(String, bool)> {
    std::fs::remove_dir_all(dir)
        .err()
        .filter(|e| e.kind() != std::io::ErrorKind::NotFound)
        .map(|e| (format!("{}: {what} couldn't be removed from this device ({e}); remove the folder by hand", dir.display()), true))
}

/// Moves the complete copy `part` into place as `dir`, whole, so a folder there is always complete.
/// An empty folder there (left by a removal cut short) is removed first, on every system; one that
/// holds anything is another opening's copy of the same package, which wins and is left as it is.
fn publish(part: &Path, dir: &Path) -> Result<(), String> {
    // Removed only while empty, so a copy put there meanwhile is never touched.
    if dir.is_dir() {
        let _ = std::fs::remove_dir(dir);
    }
    match std::fs::rename(part, dir) {
        Ok(()) => Ok(()),
        Err(_) if dir.join(LAYERS).is_file() => Ok(()),
        Err(e) => Err(format!("{}: {e}", dir.display())),
    }
}

/// Writes the rows `given` holds into `dir`: see [`keep_base`].
fn write_base(given: &GivenRows, dir: &Path) -> Result<(), String> {
    use rustgis_data::progress;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    progress::begin(given.layers.iter().map(|(_, _, d)| d.features.len()).sum());
    let mut layers = Vec::new();
    for (id, name, dataset) in &given.layers {
        // Each row's shape is put in longitude/latitude as it is written: a layer held in another
        // system isn't copied whole.
        let to = dataset.lonlat_conversion().map_err(|e| format!("{name}: {e}"))?;
        let ds = dataset;
        let fields: Vec<Option<usize>> = (0..ds.fields.len()).map(Some).collect();
        let rows_path = dir.join(format!("{id}.rows"));
        let mut rows = std::io::BufWriter::new(std::fs::File::create(&rows_path).map_err(io(&rows_path))?);
        let mut index = Vec::with_capacity(ds.features.len());
        let (mut offset, mut line) = (0u64, Vec::new());
        for row in ds.features.rows() {
            progress::step(1).map_err(|e| e.to_string())?;
            line.clear();
            let geometry = in_lonlat(&to, row, name)?;
            serde_json::to_writer(&mut line, &Print { row, geometry: geometry.as_deref(), fields: &fields }).map_err(|e| e.to_string())?;
            let len = u32::try_from(line.len())
                .ok()
                .filter(|n| *n <= MAX_ROW)
                .ok_or_else(|| format!("{name}: feature {} is too large to keep a copy of", row.id()))?;
            rows.write_all(&line).map_err(io(&rows_path))?;
            // The fingerprint of the bytes just written, as [`fingerprint`] makes it.
            let mut hash = Fnv(FNV_BASIS);
            hash.write_all(&line).map_err(|e| e.to_string())?;
            index.push(Entry { id: row.id(), offset, len, print: hash.0.to_le_bytes() });
            offset += u64::from(len);
        }
        rows.flush().map_err(io(&rows_path))?;
        index.sort_unstable_by_key(|e| e.id);
        if index.windows(2).any(|w| w.first().map(|e| e.id) == w.get(1).map(|e| e.id)) {
            return Err(format!("{name}: two features have the same id"));
        }
        let mut bytes = Vec::with_capacity(index.len() * RECORD);
        for e in &index {
            bytes.extend_from_slice(&e.id.to_le_bytes());
            bytes.extend_from_slice(&e.offset.to_le_bytes());
            bytes.extend_from_slice(&e.len.to_le_bytes());
            bytes.extend_from_slice(&e.print);
        }
        let index_path = dir.join(format!("{id}.index"));
        std::fs::write(&index_path, bytes).map_err(io(&index_path))?;
        layers.push(BaseLayer {
            layer: *id,
            name: name.clone(),
            fields: ds.fields.iter().map(|f| f.name.clone()).collect(),
            rows: index.len(),
        });
    }
    progress::check().map_err(|e| e.to_string())?;
    let list = dir.join(LAYERS);
    std::fs::write(&list, serde_json::to_vec(&layers).map_err(|e| e.to_string())?).map_err(io(&list))
}

/// `row`'s shape in longitude/latitude by `to` (the layer's conversion, made once): borrowed when it
/// is held there already; refused, naming the feature, when one of its places can't be.
fn in_lonlat<'a>(
    to: &rustgis_data::raster::Reprojection,
    row: RowRef<'a>,
    layer: &str,
) -> Result<Option<std::borrow::Cow<'a, Geometry>>, String> {
    row.geometry()
        .map(|g| {
            rustgis_data::Dataset::converted_to_lonlat(to, g)
                .ok_or_else(|| format!("{layer}: feature {} has a place that can't be given in longitude/latitude", row.id()))
        })
        .transpose()
}

/// A file error, named by the file.
fn io(path: &Path) -> impl Fn(std::io::Error) -> String + '_ {
    move |e| format!("{}: {e}", path.display())
}

/// Reads a layer's index, refusing one that doesn't hold `rows` records in id order.
fn read_index(path: &Path, rows: usize) -> Result<Vec<Entry>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if bytes.len() != rows.saturating_mul(RECORD) {
        return Err(format!("{}: damaged", path.display()));
    }
    let mut out = Vec::with_capacity(rows);
    for r in bytes.as_chunks::<RECORD>().0 {
        let (id, rest) = r.split_at(8);
        let (offset, rest) = rest.split_at(8);
        let (len, print) = rest.split_at(4);
        let (Ok(id), Ok(offset), Ok(len), Ok(print)) = (id.try_into(), offset.try_into(), len.try_into(), print.try_into()) else {
            return Err(format!("{}: damaged", path.display()));
        };
        let e = Entry { id: u64::from_le_bytes(id), offset: u64::from_le_bytes(offset), len: u32::from_le_bytes(len), print };
        if out.last().is_some_and(|last: &Entry| last.id >= e.id) || e.len > MAX_ROW {
            return Err(format!("{}: damaged", path.display()));
        }
        out.push(e);
    }
    Ok(out)
}

/// Reads row `e` of a rows file, refusing one without a value for each of the copy's `fields`.
fn read_row(file: &mut std::fs::File, e: &Entry, fields: usize) -> Result<BaseRow, String> {
    let mut bytes = vec![0; e.len as usize];
    file.seek(SeekFrom::Start(e.offset)).and_then(|_| file.read_exact(&mut bytes)).map_err(|e| e.to_string())?;
    let row: BaseRow = serde_json::from_slice(&bytes).map_err(|e| format!("a kept row is damaged: {e}"))?;
    if row.values.len() != fields {
        return Err(format!("a kept row is damaged: feature {} has {} values for {fields} fields", e.id, row.values.len()));
    }
    Ok(row)
}

// Making the list, on the phone.

/// The changes `map` holds to the rows package `package` gave it, layer by layer; `None` when
/// this device doesn't keep the package's rows (opened elsewhere, or before it kept them). Only
/// rows whose fingerprint changed are read back and compared. A layer removed from the map is
/// left out, as a sync leaves it, and named in [`Changes::gone`]. Cancel is looked at between rows.
pub(crate) fn list(map: &rustgis_map::Map, package: &str) -> Result<Option<Changes>, String> {
    use rustgis_data::progress;
    let Some(dir) = base_dir(package).filter(|d| d.join(LAYERS).is_file()) else { return Ok(None) };
    let list_path = dir.join(LAYERS);
    let text = std::fs::read(&list_path).map_err(|e| format!("{}: {e}", list_path.display()))?;
    let given: Vec<BaseLayer> = serde_json::from_slice(&text).map_err(|e| format!("{}: {e}", list_path.display()))?;
    let mut changes = Changes { version: VERSION, package: package.to_string(), layers: Vec::new(), gone: Vec::new() };
    progress::begin(given.iter().map(|g| g.rows).sum());
    for g in given {
        let Some(layer) = map.layers.iter().find(|l| l.id == g.layer && !l.is_raster()) else {
            changes.gone.push(if g.name.is_empty() { format!("#{}", g.layer) } else { g.name.clone() });
            continue;
        };
        // Each row's shape is compared in longitude/latitude, converted one row at a time.
        let to = layer.dataset.lonlat_conversion()?;
        let ds = &layer.dataset;
        // Where the map holds each of the copy's fields, by name, and the fields added since.
        let fields: Vec<Option<usize>> = g.fields.iter().map(|n| ds.field_index(n)).collect();
        let added_fields: Vec<usize> = (0..ds.fields.len()).filter(|j| !fields.contains(&Some(*j))).collect();
        let index = read_index(&dir.join(format!("{}.index", g.layer)), g.rows)?;
        let mut seen = vec![false; index.len()];
        let (mut changed, mut added) = (Vec::new(), Vec::new());
        for row in ds.features.rows() {
            progress::step(1).map_err(|e| e.to_string())?;
            match index.binary_search_by_key(&row.id(), |e| e.id) {
                Ok(k) => {
                    if let Some(s) = seen.get_mut(k) {
                        *s = true;
                    }
                    let geometry = in_lonlat(&to, row, &layer.name)?;
                    let print = fingerprint(&Print { row, geometry: geometry.as_deref(), fields: &fields })?;
                    if index.get(k).is_none_or(|e| e.print != print) || added_fields.iter().any(|j| !row.value(*j).is_null()) {
                        changed.push((k, row));
                    }
                }
                Err(_) => added.push(row.id()),
            }
        }
        let rows_path = dir.join(format!("{}.rows", g.layer));
        let mut rows = std::fs::File::open(&rows_path).map_err(|e| format!("{}: {e}", rows_path.display()))?;
        let mut edited = Vec::new();
        for (k, row) in changed {
            progress::check().map_err(|e| e.to_string())?;
            let Some(e) = index.get(k) else { continue };
            let base = read_row(&mut rows, e, g.fields.len())?;
            let mut values = Vec::new();
            for ((name, j), was) in g.fields.iter().zip(&fields).zip(&base.values) {
                // A field removed in the field is listed as removed, not by value.
                if j.is_some_and(|j| row.value(j) != was) {
                    values.push((name.clone(), was.clone()));
                }
            }
            for j in &added_fields {
                if let Some(f) = ds.fields.get(*j).filter(|_| !row.value(*j).is_null()) {
                    values.push((f.name.clone(), Value::Null));
                }
            }
            let now = in_lonlat(&to, row, &layer.name)?;
            let geometry = (now.as_deref() != base.geometry.as_ref()).then_some(base.geometry);
            if !values.is_empty() || geometry.is_some() {
                edited.push(Given { id: e.id, values, geometry });
            }
        }
        let mut deleted = Vec::new();
        for (e, _) in index.iter().zip(&seen).filter(|(_, seen)| !**seen) {
            progress::check().map_err(|e| e.to_string())?;
            let base = read_row(&mut rows, e, g.fields.len())?;
            let values = g.fields.iter().cloned().zip(base.values).collect();
            deleted.push(Given { id: e.id, values, geometry: Some(base.geometry) });
        }
        let removed: Vec<String> = g.fields.iter().zip(&fields).filter(|(_, j)| j.is_none()).map(|(n, _)| n.clone()).collect();
        if !(added.is_empty() && deleted.is_empty() && edited.is_empty() && removed.is_empty() && added_fields.is_empty()) {
            let fields = added_fields.iter().filter_map(|j| ds.fields.get(*j)).cloned().collect();
            changes.layers.push(LayerChanges { layer: g.layer, added, deleted, edited, removed, fields });
        }
    }
    Ok(Some(changes))
}

// Applying the list, at home.

/// Applies `changes`, of a package of offline area `ai` whose map is `edited`, to the area's source
/// map, as the module describes, and renews the area from it. Refused, changing nothing, when a
/// change is to a field the source map hasn't, or has with another type. Says by feature which
/// changes replaced one made at home, which deletions were kept for one made at home, and which
/// edits were dropped for a deletion at home; and the layers no longer there. A feature added in the
/// field keeps its id unless the source map uses it, else takes the next free one (`renumbered`).
///
/// It runs on the window's thread, as Bring Back does: one pass over each changed layer of the
/// source and of the package's map, the rest as large as the changes.
pub(crate) fn apply(s: &mut Session, ai: usize, edited: &rustgis_map::Map, changes: &Changes) -> Result<serde_json::Value, String> {
    let area = s.project.offline_areas.get(ai).ok_or("no such area")?;
    let (area_name, source) = (area.name.clone(), area.map.clone());
    let offline_map = super::workspace::area_map_index(&s.project, ai);
    let si = s
        .project
        .maps
        .iter()
        .position(|m| m.name.eq_ignore_ascii_case(&source))
        .ok_or_else(|| format!("the source map '{source}' is gone"))?;
    crate::project::ensure_map_imports_complete(s.project.maps.get(si).ok_or("the source map is gone")?)?;
    let next_generation =
        s.project.offline_areas.get(ai).is_some_and(|a| !a.generation.is_empty()).then(crate::project::field_id).transpose()?;
    let src = s.project.maps.get(si).ok_or("the source map is gone")?;
    // Every change to a field the source can hold, checked before anything changes.
    let (mut misfits, mut removed) = (Vec::new(), Vec::new());
    for lc in &changes.layers {
        let (Some(ol), Some(sl)) = (
            edited.layers.iter().find(|l| l.id == lc.layer && !l.is_raster()),
            src.layers.iter().find(|l| l.id == lc.layer && !l.is_raster()),
        ) else {
            continue;
        };
        let mut names: HashSet<String> = lc.edited.iter().flat_map(|g| g.values.iter().map(|(n, _)| n.to_ascii_lowercase())).collect();
        if !lc.added.is_empty() {
            let added: HashSet<u64> = lc.added.iter().copied().collect();
            for row in ol.dataset.features.rows().filter(|r| added.contains(&r.id())) {
                for (f, v) in ol.dataset.fields.iter().zip(row.values()) {
                    if !v.is_null() {
                        names.insert(f.name.to_ascii_lowercase());
                    }
                }
            }
        }
        let mut cut: Vec<String> = names
            .into_iter()
            .filter(|n| {
                let theirs = ol.dataset.field_index(n).and_then(|i| ol.dataset.fields.get(i)).map(|f| f.ty);
                let ours = sl.dataset.field_index(n).and_then(|j| sl.dataset.fields.get(j)).map(|f| f.ty);
                // A field the package's map hasn't has no new value to give.
                ours.is_none() || theirs.is_none_or(|t| Some(t) != ours)
            })
            .collect();
        cut.sort();
        if !cut.is_empty() {
            misfits.push(format!("'{}' ({})", ol.name, cut.join(", ")));
        }
        let mut gone: Vec<&str> = lc.removed.iter().filter(|n| sl.dataset.field_index(n).is_some()).map(String::as_str).collect();
        gone.sort_unstable();
        if !gone.is_empty() {
            removed.push(format!("'{}' ({})", ol.name, gone.join(", ")));
        }
    }
    if !misfits.is_empty() {
        return Err(format!(
            "cannot bring back the field's changes: the map '{source}', or the field package's map, hasn't the field(s) they change, or has them with another type, in layer(s) {}. Nothing was changed; add the fields, then Bring Back again",
            misfits.join("; ")
        ));
    }
    // The field's values in a field it removed aren't known, so none can be brought back.
    if !removed.is_empty() {
        return Err(format!(
            "cannot bring back the field's changes: the field package removed field(s) the map '{source}' still has, in layer(s) {}, so their values in the field aren't known. Nothing was changed; if the field renamed one, rename it here too, or restore it on the phone and send the package again, then Bring Back again",
            removed.join("; ")
        ));
    }
    let mut done = Applied::default();
    let tag = |layer: &str, id: u64| format!("{layer} #{id}");
    let src = s.project.maps.get_mut(si).ok_or("the source map is gone")?;
    for lc in &changes.layers {
        let Some(ol) = edited.layers.iter().find(|l| l.id == lc.layer && !l.is_raster()) else {
            done.unknown += 1;
            continue;
        };
        let Some(sl) = src.layers.iter_mut().find(|l| l.id == lc.layer && !l.is_raster()) else {
            done.gone.push(ol.name.clone());
            continue;
        };
        let name = sl.name.clone();
        let error = |e: String| super::workspace::sync_error(&ol.name, e);
        let theirs = ol.dataset.in_lonlat().map_err(error)?;
        // The source is changed in longitude/latitude, as the package holds its features, and its
        // own coordinates put back after, those it left as they were as stored.
        let held = (sl.dataset.coord_crs() != rustgis_data::raster::Crs::LonLat).then(|| sl.dataset.clone());
        if held.is_some() {
            sl.dataset = sl.dataset.in_lonlat().map_err(error)?.into_owned();
        }
        // Every id the layer had, or kept new ones above, before this return.
        let home_top = sl.dataset.features.highest_id();
        let wanted: HashSet<u64> =
            lc.added.iter().copied().chain(lc.deleted.iter().map(|g| g.id)).chain(lc.edited.iter().map(|g| g.id)).collect();
        let at_field: HashMap<u64, usize> =
            theirs.features.rows().enumerate().filter(|(_, r)| wanted.contains(&r.id())).map(|(i, r)| (r.id(), i)).collect();
        let mut at: HashMap<u64, usize> = HashMap::new();
        // A feature added in the field keeps its id while home doesn't use it: a replacement for one
        // that home uses is taken above every id the field added, so it never takes a free one.
        let mut top = lc.added.iter().copied().max().unwrap_or(0).max(sl.dataset.features.highest_id());
        for (i, r) in sl.dataset.features.rows().enumerate() {
            top = top.max(r.id());
            if wanted.contains(&r.id()) {
                at.insert(r.id(), i);
            }
        }
        let field_value =
            |row: usize, field: &str| theirs.field_index(field).map_or(&Value::Null, |i| theirs.features.value(row, i)).clone();
        let mut written = HashSet::new();
        let lost = |kind, id, values, shape| super::workspace::conflict(&changes.package, lc.layer, &name, id, kind, values, shape);
        for g in &lc.edited {
            // An edit naming a feature the package's map hasn't has no values to bring back or keep.
            let Some(&p) = at_field.get(&g.id) else {
                done.unknown += 1;
                continue;
            };
            let Some(&r) = at.get(&g.id) else {
                done.skipped += 1;
                done.dropped.push(tag(&name, g.id));
                // The field's edits lost: the values and shape it changed.
                let values = g.values.iter().map(|(field, _)| (field.clone(), field_value(p, field))).collect();
                let shape = g.geometry.as_ref().map(|_| theirs.features.geometry(p).cloned());
                done.lost.push(lost(ConflictKind::Dropped, g.id, values, shape));
                continue;
            };
            let (mut changed, mut values, mut shape) = (false, Vec::new(), None);
            for (field, was) in &g.values {
                let Some(j) = sl.dataset.field_index(field) else { continue };
                let new = field_value(p, field);
                let now = sl.dataset.features.value(r, j);
                if now != &new {
                    // Changed at home too: home's value is replaced, and kept as the one that lost.
                    if now != was {
                        values.push((field.clone(), now.clone()));
                    }
                    sl.dataset.features.set_value(r, j, new);
                    changed = true;
                }
            }
            if let Some(was) = &g.geometry {
                let new = theirs.features.geometry(p);
                let now = sl.dataset.features.geometry(r);
                if now != new {
                    if now != was.as_ref() {
                        shape = Some(now.cloned());
                    }
                    sl.dataset.features.set_geometry(r, new.cloned());
                    changed = true;
                }
            }
            if changed {
                done.updated += 1;
                written.insert(g.id);
            }
            if !values.is_empty() || shape.is_some() {
                done.conflicts.push(tag(&name, g.id));
                done.lost.push(lost(ConflictKind::Replaced, g.id, values, shape));
            }
        }
        // A deletion goes in while the feature is as it was given; one changed at home stays.
        let mut gone = HashSet::new();
        // Home's fields by name, once for the layer: the first of a name, whatever its case.
        let mut home_fields: HashMap<String, usize> = HashMap::new();
        for (j, f) in sl.dataset.fields.iter().enumerate() {
            home_fields.entry(f.name.to_ascii_lowercase()).or_insert(j);
        }
        for g in &lc.deleted {
            let Some(row) = at.get(&g.id).and_then(|r| sl.dataset.features.row(*r)) else { continue };
            let same_place = g.geometry.as_ref().is_none_or(|was| row.geometry() == was.as_ref());
            let same_values =
                g.values.iter().all(|(field, was)| home_fields.get(&field.to_ascii_lowercase()).is_none_or(|j| row.value(*j) == was));
            // A field the package didn't give (added at home since, or by another phone's return)
            // holding a value is a change made at home too.
            let named: HashSet<String> = g.values.iter().map(|(n, _)| n.to_ascii_lowercase()).collect();
            let given = |f: &rustgis_data::Field| named.contains(&f.name.to_ascii_lowercase());
            let same_values = same_values && sl.dataset.fields.iter().enumerate().all(|(j, f)| given(f) || row.value(j).is_null());
            if same_place && same_values {
                gone.insert(g.id);
            } else {
                done.kept.push(tag(&name, g.id));
                done.lost.push(lost(ConflictKind::Kept, g.id, Vec::new(), None));
            }
        }
        if !gone.is_empty() {
            let before = sl.dataset.features.len();
            sl.dataset.features.retain_rows(|r| !gone.contains(&r.id()));
            done.deleted += before - sl.dataset.features.len();
        }
        for id in &lc.added {
            let Some(row) = at_field.get(id).and_then(|p| theirs.features.row(*p)) else {
                done.unknown += 1;
                continue;
            };
            let (own, renewed) = id_at_home(*id, home_top, at.contains_key(id), top, &name)?;
            done.renumbered += usize::from(renewed);
            top = top.max(own);
            at.insert(own, sl.dataset.features.len());
            let mut attrs = vec![Value::Null; sl.dataset.fields.len()];
            for (f, v) in theirs.fields.iter().zip(row.values()) {
                if let Some(cell) = sl.dataset.field_index(&f.name).and_then(|j| attrs.get_mut(j)) {
                    *cell = v.clone();
                }
            }
            sl.dataset.features.push_parts(own, row.geometry().cloned(), attrs);
            written.insert(own);
            done.added += 1;
        }
        if let Some(own) = held {
            let merged = std::mem::replace(&mut sl.dataset, own);
            sl.dataset = sl.dataset.merged_from_lonlat(merged, &written).map_err(error)?;
        }
        sl.selection.clear();
        sl.touch_geometry();
    }
    super::workspace::renew_area(s, ai, si, offline_map, next_generation)?;
    super::workspace::keep_conflicts(s, ai, std::mem::take(&mut done.lost));
    done.say(s, &area_name, &source);
    Ok(json!({
        "updated": done.updated,
        "added": done.added,
        "deleted": done.deleted,
        "skipped": done.skipped,
        "kept": done.kept.len(),
        "conflicts": done.conflicts.len() + done.kept.len(),
        "renumbered": done.renumbered,
    }))
}

/// Adds to offline area `ai`'s source layers, and to the area's own copy of them, the fields
/// `changes` says the field added that they haven't. Returns them as `layer: field`.
pub(crate) fn add_fields(s: &mut Session, ai: usize, changes: &Changes) -> Vec<String> {
    let Some(source) = s.project.offline_areas.get(ai).map(|a| a.map.clone()) else { return Vec::new() };
    let Some(si) = s.project.maps.iter().position(|m| m.name.eq_ignore_ascii_case(&source)) else { return Vec::new() };
    let mut added = Vec::new();
    for lc in changes.layers.iter().filter(|lc| !lc.fields.is_empty()) {
        let Some(sl) = s.project.maps.get_mut(si).and_then(|m| m.layers.iter_mut().find(|l| l.id == lc.layer && !l.is_raster())) else {
            continue;
        };
        let new: Vec<&rustgis_data::Field> = lc.fields.iter().filter(|f| sl.dataset.field_index(&f.name).is_none()).collect();
        for f in &new {
            sl.dataset.ensure_field(&f.name, f.ty);
        }
        let name = sl.name.clone();
        if let Some(b) = s.project.offline_areas.get_mut(ai).and_then(|a| a.data.layers.iter_mut().find(|l| l.id == lc.layer)) {
            for f in &new {
                b.dataset.ensure_field(&f.name, f.ty);
            }
        }
        added.extend(new.iter().map(|f| format!("{name}: {}", f.name)));
    }
    added
}

/// The photo entries (names under `photos/`) that the cells `changes` applies name in `edited`,
/// the package's map: the photos field (`field`) of the features it adds, and of those whose
/// photos it changes that `home`, the area's map, still has. `entries` gives the entries a cell's
/// text names.
pub(crate) fn photos_named(
    edited: &rustgis_map::Map,
    home: Option<&rustgis_map::Map>,
    changes: &Changes,
    fields: impl Fn(&rustgis_map::Layer) -> Vec<usize>,
    entries: impl Fn(&str) -> Vec<String>,
) -> HashSet<String> {
    let mut named = HashSet::new();
    for lc in &changes.layers {
        let Some(l) = edited.layers.iter().find(|l| l.id == lc.layer && !l.is_raster()) else { continue };
        // Every photos field of the layer (a layer can have both a `photos` and a `photo`).
        let fis = fields(l);
        let names: Vec<&str> = fis.iter().filter_map(|fi| l.dataset.fields.get(*fi).map(|f| f.name.as_str())).collect();
        if names.is_empty() {
            continue;
        }
        // A layer home no longer has takes none of the changes, nor their photos.
        let Some(home_layer) = home.and_then(|m| m.layers.iter().find(|h| h.id == lc.layer && !h.is_raster())) else { continue };
        // An edit to a feature home no longer has is dropped, its photo with it.
        let mut photo_edits: HashSet<u64> = lc
            .edited
            .iter()
            .filter(|g| g.values.iter().any(|(n, _)| names.iter().any(|name| n.eq_ignore_ascii_case(name))))
            .map(|g| g.id)
            .collect();
        let at_home: HashSet<u64> = if photo_edits.is_empty() {
            HashSet::new()
        } else {
            home_layer.dataset.features.rows().map(|r| r.id()).filter(|id| photo_edits.contains(id)).collect()
        };
        photo_edits.retain(|id| at_home.contains(id));
        let ids: HashSet<u64> = lc.added.iter().copied().chain(photo_edits).collect();
        for row in l.dataset.features.rows().filter(|r| ids.contains(&r.id())) {
            for fi in &fis {
                if let Value::Text(text) = row.value(*fi) {
                    named.extend(entries(text));
                }
            }
        }
    }
    named
}

/// The id a feature added in the field with id `own` takes in a home layer: its own while it is
/// above `home_top`, every id the layer had or kept new ones above before this return, and not
/// `taken`; else one above `top`, every id there is now. An id at or below `home_top` that no row
/// uses may still be one a package out holds (a feature deleted here since), so it isn't kept.
/// Whether it took a new one; refused when none is left.
pub(crate) fn id_at_home(own: u64, home_top: u64, taken: bool, top: u64, layer: &str) -> Result<(u64, bool), String> {
    let (id, renumbered) =
        if own > home_top && !taken { (own, false) } else { (top.checked_add(1).ok_or_else(|| exhausted(layer))?, true) };
    // Above `i64::MAX` no GeoPackage or PostGIS table holds it, and the area couldn't be packaged
    // again: refused, as an id run out is.
    if id >= i64::MAX.unsigned_abs() {
        return Err(exhausted(layer));
    }
    Ok((id, renumbered))
}

/// The refusal when a feature added in the field needs a new id and none is left above the
/// layer's highest (`u64::MAX`): the command fails, and the project is put back as it was.
pub(crate) fn exhausted(layer: &str) -> String {
    format!(
        "cannot bring back layer '{layer}': a feature added in the field needs an id below {}, the largest a GeoPackage or PostGIS feature can have, and none is left above the layer's highest. Nothing was changed",
        i64::MAX
    )
}

/// What [`apply`] did, and what it says.
#[derive(Default)]
struct Applied {
    updated: usize,
    added: usize,
    deleted: usize,
    /// Edits to features deleted at home meanwhile, left deleted.
    skipped: usize,
    renumbered: usize,
    /// Changes naming a layer or feature the package's map hasn't.
    unknown: usize,
    /// Features edited at home and in the field, as `layer #id`: the field's values went in.
    conflicts: Vec<String>,
    /// Features deleted in the field but changed at home, kept.
    kept: Vec<String>,
    /// Features edited in the field but deleted at home, whose edits were dropped.
    dropped: Vec<String>,
    /// Layers changed in the field that the source map no longer has.
    gone: Vec<String>,
    /// The versions that lost, of the conflicts, kept and dropped features above, kept on the area.
    lost: Vec<crate::project::AreaConflict>,
}

impl Applied {
    /// Says, one notice of each kind, what didn't go in as the field left it.
    fn say(&self, s: &mut Session, area: &str, source: &str) {
        let listed = |ids: &[String]| {
            let mut said: Vec<String> = ids.iter().take(20).cloned().collect();
            if ids.len() > 20 {
                said.push(format!("{} more", ids.len() - 20));
            }
            said.join(", ")
        };
        if !self.conflicts.is_empty() {
            s.notify(
                format!(
                    "Offline area '{area}': {} feature(s) were changed in the map '{source}' since the field package was made and also in the field, so the field's values replaced the map's changes: {}. Undo restores the map's versions, and Data › Offline › Conflicts keeps the versions that lost",
                    self.conflicts.len(),
                    listed(&self.conflicts)
                ),
                false,
            );
        }
        if !self.kept.is_empty() {
            s.notify(
                format!(
                    "Offline area '{area}': {} feature(s) deleted in the field were changed in the map '{source}' since the field package was made, so they were kept: {}. Data › Offline › Conflicts lists them",
                    self.kept.len(),
                    listed(&self.kept)
                ),
                false,
            );
        }
        if !self.dropped.is_empty() {
            s.notify(
                format!(
                    "Offline area '{area}': {} feature(s) edited in the field were deleted from the map '{source}' meanwhile, so those edits were dropped: {}. Data › Offline › Conflicts keeps the versions that lost",
                    self.dropped.len(),
                    listed(&self.dropped)
                ),
                false,
            );
        }
        if !self.gone.is_empty() {
            s.notify(
                format!(
                    "Offline area '{area}': {} layer(s) changed in the field are no longer in the map '{source}', so their changes weren't brought back: {}",
                    self.gone.len(),
                    self.gone.join(", ")
                ),
                true,
            );
        }
        if self.unknown > 0 {
            s.notify(
                format!(
                    "Offline area '{area}': {} change(s) in the field package name a layer or feature its map hasn't, so they were left out: the package may be damaged",
                    self.unknown
                ),
                true,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two openings of one package keeping its rows at once: the second to finish finds the first's
    /// complete copy in place, leaves it as it is, and succeeds.
    #[test]
    fn a_copy_kept_meanwhile_is_never_replaced() {
        let root = test_bases::root().unwrap();
        let (dir, part) = (root.join("winner"), root.join("loser.part"));
        for (d, text) in [(&dir, "first"), (&part, "second")] {
            std::fs::create_dir_all(d).unwrap();
            std::fs::write(d.join(LAYERS), text).unwrap();
        }
        publish(&part, &dir).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join(LAYERS)).unwrap(), "first");
        // An empty folder there is replaced, on every system.
        let (empty, part) = (root.join("empty"), root.join("fresh.part"));
        std::fs::create_dir_all(&empty).unwrap();
        std::fs::create_dir_all(&part).unwrap();
        std::fs::write(part.join(LAYERS), "fresh").unwrap();
        publish(&part, &empty).unwrap();
        assert_eq!(std::fs::read_to_string(empty.join(LAYERS)).unwrap(), "fresh");
    }

    /// A return sent while its package's rows are still being kept: once the keep puts its copy in
    /// place, it finds the package spent and removes it, so nothing is left behind.
    #[test]
    fn a_keep_that_finishes_after_its_return_was_sent_leaves_nothing() {
        let id = crate::project::field_id().unwrap();
        let mut project = Project::new("Field");
        project.field_origin = Some(crate::project::FieldOrigin {
            project: crate::project::field_id().unwrap(),
            area: crate::project::field_id().unwrap(),
            generation: String::new(),
            package: id.clone(),
            ids: Default::default(),
        });
        let given = GivenRows::of(&project, &id);
        // Sent before the keep finished: there was nothing to remove yet.
        assert!(!forget(&id).unwrap());
        let notes = keep_base(&given).unwrap();
        assert!(!base_dir(&id).unwrap().exists());
        assert!(notes.iter().any(|(text, error)| !error && text.contains("had already gone back")), "{notes:?}");
    }

    /// A kept row with fewer values than the copy has fields is damaged, not read short.
    #[test]
    fn a_kept_row_missing_a_value_is_refused_as_damaged() {
        let path = test_bases::root().unwrap().join("short.rows");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let line = serde_json::to_vec(&json!({"geometry": null, "values": [Value::Null]})).unwrap();
        std::fs::write(&path, &line).unwrap();
        let e = Entry { id: 7, offset: 0, len: line.len() as u32, print: [0; 16] };
        let mut file = std::fs::File::open(&path).unwrap();
        assert!(read_row(&mut file, &e, 1).is_ok());
        let refused = read_row(&mut file, &e, 2).err().unwrap();
        assert!(refused.contains("damaged") && refused.contains("feature 7"), "{refused}");
    }

    /// A folder a racing send removed first is gone, not a failure to remove it.
    #[test]
    fn letting_go_of_a_folder_already_gone_says_nothing() {
        let dir = test_bases::root().unwrap().join("already gone");
        assert!(!dir.exists());
        assert_eq!(let_go(&dir, "a copy"), None);
    }

    /// A package sent back, then opened again after a restart (which forgets what this run sent),
    /// keeps nothing, and says why: the mark on disk outlasts the run.
    #[test]
    fn a_package_sent_back_keeps_no_rows_when_opened_after_a_restart() {
        let id = crate::project::field_id().unwrap();
        let mut project = Project::new("Field");
        project.field_origin = Some(crate::project::FieldOrigin {
            project: crate::project::field_id().unwrap(),
            area: crate::project::field_id().unwrap(),
            generation: String::new(),
            package: id.clone(),
            ids: Default::default(),
        });
        let given = GivenRows::of(&project, &id);
        assert!(keep_base(&given).unwrap().is_empty());
        assert!(forget(&id).unwrap());
        // A restart: this run's memory of what was sent is gone, the mark isn't.
        SPENT.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&id);
        assert!(is_spent(&id));
        let notes = keep_base(&given).unwrap();
        assert!(!base_dir(&id).unwrap().exists());
        assert!(notes.iter().any(|(text, error)| !error && text.contains("had already gone back")), "{notes:?}");
        // The command says so: its rows were waiting, and none were kept.
        let mut s = Session::new(Project::new("Field"));
        s.project.given = Some(std::sync::Arc::new(GivenRows::of(&project, &id)));
        assert_eq!(keep_rows(&mut s, &json!({})).unwrap()["kept"], json!(false));
        assert!(s.notices.iter().any(|n| n.text.contains("had already gone back")));
        // A package not sent is kept as before.
        let other = crate::project::field_id().unwrap();
        assert!(!is_spent(&other) && keep_base(&GivenRows::of(&project, &other)).unwrap().is_empty());
        assert!(base_dir(&other).unwrap().join(LAYERS).is_file());
    }

    /// A read's answer lands without taking rows another opening left waiting: those are kept by
    /// the keep that follows, not dropped.
    #[test]
    fn a_landing_keep_leaves_rows_waiting_for_another() {
        let id = crate::project::field_id().unwrap();
        let mut project = Project::new("Field");
        project.field_origin = Some(crate::project::FieldOrigin {
            project: crate::project::field_id().unwrap(),
            area: crate::project::field_id().unwrap(),
            generation: String::new(),
            package: id.clone(),
            ids: Default::default(),
        });
        project.given = Some(std::sync::Arc::new(GivenRows::of(&project, &id)));
        let mut s = Session::new(project);
        let read = super::super::reads::read_ahead(&mut s, "offline.keepRows", &json!({})).unwrap();
        let done = read.run(&rustgis_analysis::control::Control::new()).unwrap();
        // Rows waiting again before the answer lands (another package's, say).
        let other = crate::project::field_id().unwrap();
        s.project.given = Some(std::sync::Arc::new(GivenRows::of(&s.project, &other)));
        super::super::reads::read_land(&mut s, done).unwrap();
        assert!(s.field_rows_waiting(), "the waiting rows were dropped");
        s.run("offline.keepRows", &json!({})).unwrap();
        assert!(!s.field_rows_waiting() && base_dir(&other).unwrap().join(LAYERS).is_file());
    }

    /// A keep with no rows waiting says it kept none, whether its read ran ahead or not.
    #[test]
    fn a_keep_with_nothing_waiting_says_it_kept_none() {
        let mut s = Session::new(Project::new("Field"));
        let read = super::super::reads::read_ahead(&mut s, "offline.keepRows", &json!({})).unwrap();
        let done = read.run(&rustgis_analysis::control::Control::new()).unwrap();
        assert_eq!(super::super::reads::read_land(&mut s, done).unwrap()["kept"], false);
        assert_eq!(s.run("offline.keepRows", &json!({})).unwrap()["kept"], false);
    }

    /// A feature added in the field keeps its id only above every id home had or kept, and no id
    /// from `i64::MAX` up is ever given: none a GeoPackage or PostGIS table can't hold.
    #[test]
    fn ids_at_home_stay_above_home_and_below_the_largest_a_table_holds() {
        assert_eq!(id_at_home(11, 10, false, 10, "L").unwrap(), (11, false));
        assert_eq!(id_at_home(9, 10, false, 12, "L").unwrap(), (13, true));
        assert_eq!(id_at_home(11, 10, true, 12, "L").unwrap(), (13, true));
        let largest = i64::MAX.unsigned_abs();
        assert!(id_at_home(largest, 10, false, 10, "L").is_err());
        assert!(id_at_home(5, 10, false, largest - 1, "L").is_err());
        assert_eq!(id_at_home(5, 10, false, largest - 2, "L").unwrap(), (largest - 1, true));
    }

    /// A keep that failed and then couldn't remove its partial copy says both.
    #[test]
    fn a_failed_keep_says_the_folder_it_left_behind() {
        let left = ("/kept/rows.part: a copy cut short couldn't be removed".to_string(), true);
        let said = outcome(Err("disk full".into()), vec![left.clone()]).unwrap_err();
        assert!(said.contains("disk full") && said.contains(&left.0), "{said}");
        assert_eq!(outcome(Err("disk full".into()), Vec::new()).unwrap_err(), "disk full");
        assert_eq!(outcome(Ok(()), vec![left.clone()]).unwrap(), vec![left]);
    }

    /// A folder that can't be removed is said, by name, as an error.
    #[cfg(unix)]
    #[test]
    fn a_folder_that_cant_be_removed_is_said() {
        use std::os::unix::fs::PermissionsExt;
        let dir = test_bases::root().unwrap().join("stuck");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(LAYERS), "rows").unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        let said = let_go(&dir, "the rows of a field package already sent back");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let (text, error) = said.unwrap();
        assert!(error && text.contains(&dir.display().to_string()) && text.contains("remove the folder by hand"), "{text}");
        assert!(let_go(&dir, "the rows").is_none());
    }

    /// A list of any version but this one is refused: a newer one asks for a newer RustGIS.
    #[test]
    fn a_list_of_another_version_is_refused() {
        let id = "0123456789abcdef0123456789abcdef";
        let list = |v: u32| format!(r#"{{"version":{v},"package":"{id}","layers":[]}}"#);
        assert!(Changes::read(list(1).as_bytes(), id).is_ok());
        assert!(Changes::read(list(2).as_bytes(), id).unwrap_err().contains("newer RustGIS"));
        assert!(Changes::read(list(0).as_bytes(), id).unwrap_err().contains("damaged"));
    }
}

/// In tests, given copies go in a folder of each test's own thread, removed when it ends.
#[cfg(test)]
pub(crate) mod test_bases {
    use std::path::PathBuf;

    struct Root(PathBuf);

    impl Drop for Root {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    thread_local! {
        static ROOT: Root = {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Root(std::env::temp_dir().join(format!("rustgis-field-bases-{}-{n}", std::process::id())))
        };
    }

    /// This thread's folder.
    pub(crate) fn root() -> Option<PathBuf> {
        ROOT.try_with(|r| r.0.clone()).ok()
    }
}
