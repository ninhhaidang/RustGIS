//! The files a layer was read from, as they were when it was read ([`FileSync`]): what a layer
//! kept in the project compares with its source before its edits are written back to it, so
//! that writing them never overwrites a change made to the file since (#1385).

use serde::{Deserialize, Serialize};

/// The files of a source as they were at a moment: each one's name, size and SHA-256 hash, and
/// the files that weren't there. A Shapefile is its `.shp`, `.shx`, `.dbf`, `.prj` and `.cpg`
/// (the ones its reader opens); a GeoJSON file is itself. Only the contents count: a file
/// copied, touched or moved to another disk with the same bytes is the same file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSync {
    /// Each file by its name beside the source (`parcels.dbf`), with its stamp, or `None` for
    /// one that wasn't there, so a `.prj` added since counts as a change too.
    pub files: Vec<(String, Option<FileStamp>)>,
}

/// One file as it was.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStamp {
    pub len: u64,
    /// The SHA-256 of its bytes, in hexadecimal.
    pub sha256: String,
}

/// A layer kept in the project that was read whole from a file it can be written back to (a
/// Shapefile or GeoJSON file), as it and the file were at their last sync, when it was read: so
/// it can tell whether it has edits the file lacks and whether the file changed since.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Synced {
    /// The files, as they were.
    pub files: FileSync,
    /// The layer's [`rows_digest`] then: the file's as it read then too.
    pub rows: u64,
    /// For a layer held in a system of its own, its [`native_digest`] then, which is compared
    /// instead of [`Synced::rows`] when there is one: it hashes the coordinates as held, so it
    /// takes no conversion of the whole layer, and it still matches once the conversion to
    /// longitude/latitude changes (a datum grid downloaded). `rows` is kept beside it for the
    /// releases before it, which compare that. Such a release saves the project again without
    /// it, which loses nothing it needs: the record is then compared by `rows`, as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<u64>,
}

impl Synced {
    /// The record of `ds` as read from `files`: its [`rows_digest`], and its [`native_digest`]
    /// when it is held in a system other than longitude/latitude (else the two are the same).
    pub fn record(files: FileSync, ds: &crate::Dataset) -> crate::Result<Self> {
        let (rows, native) = digests(ds)?;
        Ok(Self { files, rows, native })
    }

    /// The digest recorded, to compare with [`Synced::digest_of`] a layer.
    pub fn digest(&self) -> u64 {
        self.native.unwrap_or(self.rows)
    }

    /// The digest of `ds` of the kind recorded: [`native_digest`] when the record has one, else
    /// [`rows_digest`] (a record saved by an older release, or of a layer in longitude/latitude).
    pub fn digest_of(&self, ds: &crate::Dataset) -> crate::Result<u64> {
        if self.native.is_some() { native_digest(ds) } else { rows_digest(ds) }
    }
}

/// A hash of what a dataset holds: its fields, coordinate system, and each row's geometry and
/// attributes in order (FNV-1a over their JSON, written to the hash as it is made). Not its name
/// or where it came from, or display aliases, which are no edit to the file. A number that isn't
/// finite, which JSON writes as null, is told from an empty cell by its position and bits, as the GeoPackage edit
/// rows are: clearing one is an edit. A project saves one as null (and says so), so a layer that
/// held one reads as edited once its project is opened again: it no longer holds what its file
/// does. Its work grows with the rows: each is counted as progress ([`crate::progress`]), and it
/// stops once the run is cancelled; the desktop app runs it off the UI thread.
pub fn rows_digest(ds: &crate::Dataset) -> crate::Result<u64> {
    Ok(hashes(ds, true, false)?.0.unwrap_or_default())
}

/// The digests [`Synced::record`] keeps of `ds`: its [`rows_digest`], and its [`native_digest`]
/// when it is held in a system other than longitude/latitude.
pub fn digests(ds: &crate::Dataset) -> crate::Result<(u64, Option<u64>)> {
    if ds.coord_crs() == crate::raster::Crs::LonLat {
        return Ok((rows_digest(ds)?, None));
    }
    let (rows, native) = hashes(ds, true, true)?;
    Ok((rows.unwrap_or_default(), native))
}

/// [`rows_digest`] over the geometry as the dataset holds it, in its own system, without
/// converting it to longitude/latitude; the same as [`rows_digest`] for a dataset held in
/// longitude/latitude.
pub fn native_digest(ds: &crate::Dataset) -> crate::Result<u64> {
    if ds.coord_crs() == crate::raster::Crs::LonLat {
        return rows_digest(ds);
    }
    Ok(hashes(ds, false, true)?.1.unwrap_or_default())
}

/// The digests of `ds`: its [`rows_digest`] (its geometry in longitude/latitude) when `lonlat`,
/// and its [`native_digest`] (as held) when `native`, in one pass over the rows.
fn hashes(ds: &crate::Dataset, lonlat: bool, native: bool) -> crate::Result<(Option<u64>, Option<u64>)> {
    /// FNV-1a over the bytes written to it.
    struct Fnv(u64);
    impl std::io::Write for Fnv {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0 = buf.iter().fold(self.0, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3));
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    /// Writes `value`'s JSON to the hash (nothing, should it not write), then a byte JSON never
    /// writes, so values can't run into each other.
    fn put(h: &mut Fnv, value: &impl serde::Serialize) {
        let _ = serde_json::to_writer(&mut *h, value);
        h.0 = (h.0 ^ 0xff).wrapping_mul(0x0100_0000_01b3);
    }
    /// The rows' values past their geometry: a number that isn't finite by its bits.
    fn non_finite(h: &mut Fnv, attrs: &[crate::Value]) {
        for (index, value) in attrs.iter().enumerate() {
            if let crate::Value::Float(number) = value
                && !number.is_finite()
            {
                put(h, &("non-finite", index, number.to_bits()));
            }
        }
    }
    // Whole Shapefile/GeoJSON reads record fields with no aliases. Clear display-only aliases
    // when comparing a layer, keeping the serialized form and existing saved digests unchanged.
    let mut fields = ds.fields.clone();
    for field in &mut fields {
        field.alias = None;
    }
    let start = |h: &mut Fnv| put(h, &(&fields, &ds.crs));
    let (mut a, mut b) = (Fnv(0xcbf2_9ce4_8422_2325), Fnv(0xcbf2_9ce4_8422_2325));
    start(&mut a);
    start(&mut b);
    // Its geometry in longitude/latitude, as every release before #2032 C2 held a file's rows
    // (converted by the same transformer the readers converted them with), so a digest saved
    // then still matches the file read now in its own coordinates. Or as held, with the system
    // it is held in (as a project saves it, not its display label), so the same numbers held in
    // another system differ.
    let held = ds.coord_crs();
    if held != crate::raster::Crs::LonLat {
        put(&mut b, &("held in", &held));
    }
    let to = if lonlat { Some(ds.lonlat_conversion().map_err(crate::DataError::Format)?) } else { None };
    let converts = to.as_ref().filter(|t| !t.is_identity());
    crate::progress::begin(ds.features.len());
    for f in ds.features.iter() {
        if lonlat {
            match converts {
                Some(to) => put(&mut a, &(&f.geometry.as_ref().map(|g| to.geometry(g)), &f.attrs)),
                None => put(&mut a, &(&f.geometry, &f.attrs)),
            }
            non_finite(&mut a, &f.attrs);
        }
        if native {
            put(&mut b, &(&f.geometry, &f.attrs));
            non_finite(&mut b, &f.attrs);
        }
        crate::progress::step(1)?;
    }
    Ok((lonlat.then_some(a.0), native.then_some(b.0)))
}

/// Whether `path` names a source whose files a [`FileSync`] records: a Shapefile or a
/// `.geojson` file. A `.json` file isn't one yet: it may hold a city model or a topology, which
/// can't be written back as they were read.
pub fn is_syncable(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    [".shp", ".geojson"].iter().any(|e| lower.ends_with(e))
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::{FileStamp, FileSync};
    use crate::{DataError, Result};
    use std::path::{Path, PathBuf};

    /// The files of the source at `path`, each as a name beside it and its path: for a
    /// Shapefile the companions its reader opens (the `.cpg` in whichever case is there, else
    /// `.cpg`), otherwise the file alone.
    fn parts(path: &Path) -> Vec<(String, PathBuf)> {
        let mut paths = vec![path.to_path_buf()];
        if path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("shp")) {
            // The table in whichever case the reader finds it.
            paths.extend([path.with_extension("shx"), crate::io::dbf_sidecar(path), path.with_extension("prj")]);
            let cpg = crate::dbf::CPG_SPELLINGS
                .iter()
                .map(|e| path.with_extension(e))
                .find(|p| p.is_file())
                .unwrap_or_else(|| path.with_extension("cpg"));
            paths.push(cpg);
        }
        paths.into_iter().map(|p| (p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), p)).collect()
    }

    /// The file's size, or `None` when it isn't there.
    fn len(path: &Path) -> Result<Option<u64>> {
        match std::fs::metadata(path) {
            Ok(m) if m.is_file() => Ok(Some(m.len())),
            Ok(_) => Err(DataError::Format(format!("{} isn't a file", path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// The SHA-256 of the file's bytes, read a block at a time, each counted as progress
    /// ([`crate::progress`]) and stopping once the run is cancelled.
    fn sha256(path: &Path) -> Result<String> {
        use sha2::Digest;
        use std::io::Read;
        let mut file = crate::provenance::open(path)?;
        let metadata = file.metadata().ok();
        let mut hasher = sha2::Sha256::new();
        let mut buf = vec![0u8; 1 << 16];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(buf.get(..n).unwrap_or_default());
            crate::progress::step(n)?;
        }
        let hash: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
        crate::provenance::hashed(path, metadata.as_ref(), hash.clone());
        Ok(hash)
    }

    /// Bytes as a progress count.
    fn count(bytes: u64) -> usize {
        usize::try_from(bytes).unwrap_or(usize::MAX)
    }

    impl FileSync {
        /// The files of the source at `path` as they are now, each read whole to hash it.
        pub fn read(path: &Path) -> Result<FileSync> {
            let parts = parts(path);
            let total = parts.iter().filter_map(|(_, p)| std::fs::metadata(p).ok()).map(|m| m.len()).sum();
            crate::progress::begin(count(total));
            let mut files = Vec::new();
            for (name, p) in parts {
                let stamp = match len(&p)? {
                    None => None,
                    Some(len) => Some(FileStamp { len, sha256: sha256(&p)? }),
                };
                files.push((name, stamp));
            }
            Ok(FileSync { files })
        }

        /// The files of the source at `path` as just written: each of `written` (a path and
        /// the bytes written to it) by those bytes, so a write made since by someone else isn't
        /// taken for this one; any other part as it is on the disk.
        pub fn of_written(path: &Path, written: &[(PathBuf, Vec<u8>)]) -> Result<FileSync> {
            use sha2::Digest;
            let mut files = Vec::new();
            for (name, p) in parts(path) {
                let stamp = match written.iter().find(|(w, _)| *w == p) {
                    Some((_, bytes)) => Some(FileStamp {
                        len: bytes.len() as u64,
                        sha256: sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect(),
                    }),
                    None => match len(&p)? {
                        None => None,
                        Some(len) => Some(FileStamp { len, sha256: sha256(&p)? }),
                    },
                };
                files.push((name, stamp));
            }
            Ok(FileSync { files })
        }

        /// Runs `read`, which reads the source at `path`, and returns with its result the files
        /// as they were just before it ran (`None` when a part couldn't be read). Hashed before
        /// the read, the record can only be older than what was read: a write while it ran, or
        /// after, leaves the file unlike the record, so it shows as changed and is never written
        /// over. That costs one more pass over the files than the read, about a second for every
        /// 250 MB of them where SHA-256 runs at 250 MB/s: about 17 for a part of the largest a reader
        /// takes ([`crate::io::max_bytes`], 4 GiB at most). It runs on the thread that reads: off the UI
        /// thread for a file read ahead, on the calling thread for one `layer.addData` reads
        /// whole there (a GeoJSON file that isn't a FeatureCollection). Errs,
        /// without reading, when the run is cancelled while the files are hashed.
        pub fn around<R>(path: &Path, read: impl FnOnce() -> R) -> Result<(R, Option<FileSync>)> {
            let before = match FileSync::read(path) {
                Ok(sync) => Some(sync),
                Err(DataError::Cancelled) => return Err(DataError::Cancelled),
                Err(_) => None,
            };
            Ok((read(), before))
        }

        /// The names of the files that differ now from this record: changed, added, removed or
        /// unreadable. Empty when the source is as it was. A file of the size recorded is read
        /// whole and hashed, so its work grows with the file: a caller on the desktop's UI
        /// thread does it on a thread of its own. Errs only when the run is cancelled.
        pub fn changed(&self, path: &Path) -> Result<Vec<String>> {
            let now = parts(path);
            let mut changed = Vec::new();
            // Files there now that the record lacks (a `.cpg` added in another case); not the
            // `.cpg` name `parts` falls back to when there is none.
            for (name, p) in &now {
                if !self.files.iter().any(|(n, _)| n == name) && p.exists() {
                    changed.push(name.clone());
                }
            }
            let total = self.files.iter().filter_map(|(_, s)| s.as_ref()).map(|s| s.len).sum();
            crate::progress::begin(count(total));
            for (name, was) in &self.files {
                let Some((_, p)) = now.iter().find(|(n, _)| n == name) else {
                    // Out of the set now: changed if it held anything.
                    if was.is_some() {
                        changed.push(name.clone());
                    }
                    continue;
                };
                let same = match (was, len(p)) {
                    (None, Ok(None)) => true,
                    (Some(was), Ok(Some(len))) if len == was.len => match sha256(p) {
                        Ok(hash) => hash == was.sha256,
                        Err(DataError::Cancelled) => return Err(DataError::Cancelled),
                        Err(_) => false,
                    },
                    _ => false,
                };
                if !same {
                    changed.push(name.clone());
                }
            }
            Ok(changed)
        }

        /// The names of the files that differ from this record by what metadata alone shows:
        /// added, removed, or another length. Reads no file, so a change that keeps every length
        /// isn't seen; [`FileSync::changed`] hashes them. An unreadable file counts as differing.
        pub fn resized(&self, path: &Path) -> Vec<String> {
            let now = parts(path);
            let mut differ: Vec<String> =
                now.iter().filter(|(name, p)| !self.files.iter().any(|(n, _)| n == name) && p.exists()).map(|(n, _)| n.clone()).collect();
            for (name, was) in &self.files {
                let is = now.iter().find(|(n, _)| n == name).map(|(_, p)| len(p));
                let same = match (was, is) {
                    (None, None | Some(Ok(None))) => true,
                    (Some(was), Some(Ok(Some(len)))) => len == was.len,
                    _ => false,
                };
                if !same {
                    differ.push(name.clone());
                }
            }
            differ
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn resized_sees_lengths_and_presence_without_reading() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.geojson");
        std::fs::write(&path, "{}").unwrap();
        let record = FileSync::read(&path).unwrap();
        assert!(record.resized(&path).is_empty());
        std::fs::write(&path, "[]").unwrap();
        assert!(record.resized(&path).is_empty(), "same length: only a hash tells");
        std::fs::write(&path, "{ }").unwrap();
        assert_eq!(record.resized(&path), ["a.geojson"]);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(record.resized(&path), ["a.geojson"]);
    }

    #[test]
    fn a_shapefile_set_records_its_companions_and_the_missing_ones() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        for ext in ["shp", "shx", "dbf"] {
            std::fs::write(d.join(format!("a.{ext}")), ext).unwrap();
        }
        std::fs::write(d.join("a.cpg"), "UTF-8").unwrap();
        let path = d.join("a.shp");
        let sync = FileSync::read(&path).unwrap();
        let names: Vec<(&str, bool)> = sync.files.iter().map(|(n, s)| (n.as_str(), s.is_some())).collect();
        assert_eq!(names, [("a.shp", true), ("a.shx", true), ("a.dbf", true), ("a.prj", false), ("a.cpg", true)]);
        assert!(sync.changed(&path).unwrap().is_empty());
    }

    #[test]
    fn changes_are_named_and_a_touch_is_not_one() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let path = d.join("a.shp");
        for ext in ["shp", "shx", "dbf"] {
            std::fs::write(d.join(format!("a.{ext}")), ext).unwrap();
        }
        let sync = FileSync::read(&path).unwrap();
        // The same bytes written again: not a change.
        std::fs::write(d.join("a.dbf"), "dbf").unwrap();
        assert!(sync.changed(&path).unwrap().is_empty());
        // Other bytes of the same length, a file added and one removed.
        std::fs::write(d.join("a.dbf"), "DBF").unwrap();
        std::fs::write(d.join("a.prj"), "GEOGCS").unwrap();
        std::fs::remove_file(d.join("a.shx")).unwrap();
        assert_eq!(sync.changed(&path).unwrap(), ["a.shx", "a.dbf", "a.prj"]);
    }

    /// A copy that keeps times (`cp -p`, `rsync -t`) can put back the size and time a file had
    /// with other bytes: the hash still tells.
    #[test]
    fn a_rewrite_that_puts_the_time_back_is_a_change() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.geojson");
        std::fs::write(&path, "{}").unwrap();
        let sync = FileSync::read(&path).unwrap();
        let time = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::fs::write(&path, "[]").unwrap();
        std::fs::File::options().write(true).open(&path).unwrap().set_modified(time).unwrap();
        assert_eq!(sync.changed(&path).unwrap(), ["a.geojson"]);
    }

    #[test]
    fn a_removed_cpg_is_named_as_it_was_spelled() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        for ext in ["shp", "shx", "dbf"] {
            std::fs::write(d.join(format!("a.{ext}")), ext).unwrap();
        }
        // Recorded under a spelling no file has now: as `a.CPG` is once removed on a
        // case-sensitive file system (one that ignores case would find `a.cpg` there).
        let path = d.join("a.shp");
        let mut sync = FileSync::read(&path).unwrap();
        let cpg = sync.files.iter_mut().find(|(n, _)| n == "a.cpg").unwrap();
        *cpg = ("a.CPG".into(), Some(FileStamp { len: 5, sha256: "00".into() }));
        assert_eq!(sync.changed(&path).unwrap(), ["a.CPG"]);
    }

    #[test]
    fn a_write_while_reading_shows_as_a_change() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.geojson");
        std::fs::write(&path, "{}").unwrap();
        let ((), sync) = FileSync::around(&path, || std::fs::write(&path, "[]").unwrap()).unwrap();
        assert_eq!(sync.unwrap().changed(&path).unwrap(), ["a.geojson"]);
        let ((), sync) = FileSync::around(&path, || ()).unwrap();
        assert!(sync.unwrap().changed(&path).unwrap().is_empty());
    }

    #[test]
    fn a_rows_digest_tells_a_non_finite_number_from_an_empty_cell() {
        use crate::{Dataset, Feature, Field, FieldType, GeometryType, Value};
        let row = |v: Value| vec![Feature { id: 1, geometry: None, attrs: vec![v] }].into();
        let mut ds = Dataset::new("a".to_string(), GeometryType::None, vec![Field::new("v", FieldType::Double)]);
        ds.features = row(Value::Float(f64::NAN));
        let digest = |v: Value| {
            let mut d = ds.clone();
            d.features = row(v);
            rows_digest(&d).unwrap()
        };
        // Clearing it is an edit, and so is a project's saving it as null.
        let reopened: Dataset = serde_json::from_value(serde_json::to_value(&ds).unwrap()).unwrap();
        assert_ne!(rows_digest(&reopened).unwrap(), rows_digest(&ds).unwrap());
        assert_eq!(rows_digest(&reopened).unwrap(), digest(Value::Null));
        assert_ne!(digest(Value::Float(f64::INFINITY)), digest(Value::Float(f64::NAN)));
        assert_eq!(digest(Value::Float(f64::NAN)), rows_digest(&ds).unwrap());
    }

    /// The digest is saved in projects with no version: a change in how fields, coordinate
    /// systems, geometry or values serialize changes every saved one, and each layer of an older
    /// project would then show as edited. This pins it; a change needs a way to read the old one.
    #[test]
    fn a_rows_digest_is_pinned() {
        use crate::{Coord, Dataset, Feature, Field, FieldType, Geometry, GeometryType, Value};
        let mut ds = Dataset::new(
            "a".to_string(),
            GeometryType::Point,
            vec![Field::new("name", FieldType::Text), Field::new("n", FieldType::Integer)],
        );
        ds.features = vec![Feature {
            id: 1,
            geometry: Some(Geometry::Point(Coord { x: -90.05, y: 35.15 })),
            attrs: vec![Value::Text("Main St".into()), Value::Int(4)],
        }]
        .into();
        assert_eq!(rows_digest(&ds).unwrap(), 1_724_049_670_388_433_092);
        ds.fields[0].alias = Some("Street name".into());
        assert_eq!(rows_digest(&ds).unwrap(), 1_724_049_670_388_433_092, "an alias changes display, not file contents");
    }

    /// A layer held in a system of its own is recorded by its own coordinates too, compared in
    /// their place, while the longitude/latitude digest older releases compare is kept beside
    /// them. A layer in longitude/latitude is recorded as before, with no second digest, and a
    /// record saved without one is compared as before.
    #[test]
    fn a_held_layer_is_recorded_by_its_own_coordinates_too() {
        use crate::{Coord, Dataset, Feature, Field, FieldType, Geometry, GeometryType, Value};
        let mut lonlat = Dataset::new("a".to_string(), GeometryType::Point, vec![Field::new("n", FieldType::Integer)]);
        lonlat.features =
            vec![Feature { id: 1, geometry: Some(Geometry::Point(Coord { x: 7.4, y: 46.9 })), attrs: vec![Value::Int(4)] }].into();
        let files = FileSync { files: Vec::new() };
        let plain = Synced::record(files.clone(), &lonlat).unwrap();
        assert_eq!(plain, Synced { files: files.clone(), rows: rows_digest(&lonlat).unwrap(), native: None });
        assert_eq!(serde_json::to_string(&plain).unwrap(), r#"{"files":{"files":[]},"rows":"#.to_string() + &plain.rows.to_string() + "}");
        assert_eq!(native_digest(&lonlat).unwrap(), rows_digest(&lonlat).unwrap());

        let lv95 = crate::raster::Crs::from_epsg(2056).unwrap();
        let mut held = lonlat.clone();
        held.crs = Some(lv95);
        held.held_in = lv95;
        held.features =
            vec![Feature { id: 1, geometry: Some(Geometry::Point(Coord { x: 2_600_000.0, y: 1_200_000.0 })), attrs: vec![Value::Int(4)] }]
                .into();
        let record = Synced::record(files.clone(), &held).unwrap();
        assert_eq!(record.rows, rows_digest(&held).unwrap());
        assert_eq!(record.native, Some(native_digest(&held).unwrap()));
        assert_ne!(record.native, Some(record.rows));
        assert_eq!(record.digest_of(&held).unwrap(), record.digest());
        // Read back, and as an older release saved it: compared by the longitude/latitude one.
        let text = serde_json::to_string(&record).unwrap();
        assert_eq!(serde_json::from_str::<Synced>(&text).unwrap(), record);
        let older = Synced { native: None, ..record.clone() };
        assert_eq!(serde_json::from_str::<Synced>(&serde_json::to_string(&older).unwrap()).unwrap(), older);
        assert_eq!(older.digest_of(&held).unwrap(), older.digest());
        // An edit shows by either.
        let mut edited = held.clone();
        let mut rows: Vec<_> = edited.features.iter().cloned().collect();
        rows[0].attrs[0] = Value::Int(5);
        edited.features = rows.into();
        assert_ne!(record.digest_of(&edited).unwrap(), record.digest());
        assert_ne!(older.digest_of(&edited).unwrap(), older.digest());
        // The same numbers held in another system are another layer.
        let mut other = held.clone();
        other.held_in = crate::raster::Crs::from_epsg(21781).unwrap();
        assert_ne!(native_digest(&other).unwrap(), native_digest(&held).unwrap());
    }

    #[test]
    fn a_record_round_trips() {
        let sync = FileSync { files: vec![("a.geojson".into(), Some(FileStamp { len: 2, sha256: "ab".into() })), ("a.prj".into(), None)] };
        let text = serde_json::to_string(&sync).unwrap();
        assert_eq!(text, r#"{"files":[["a.geojson",{"len":2,"sha256":"ab"}],["a.prj",null]]}"#);
        assert_eq!(serde_json::from_str::<FileSync>(&text).unwrap(), sync);
    }
}
