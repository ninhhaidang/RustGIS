//! Writing a layer kept in the project back over the source it was read from (#1385), so that
//! the file shows its edits and keeps everything else it held.
//!
//! A Shapefile can't change one row in place: the whole file is written again. That is only
//! safe when nothing but the edits changes, so [`shapefile`] proves it before it touches the
//! file, and refuses, saying why, when it can't:
//!
//! 1. The file is as it was when the layer was read ([`FileSync::changed`]): a change made since
//!    would be overwritten. And it reads now as the layer was read: rows that weren't edited are
//!    written as the file holds them, not as an older read held them.
//! 2. The file as it was read, written again the same way, holds what the file holds: the same
//!    shapes (no Z or M values dropped, none left out), the same columns and the same values,
//!    compared as the `.dbf` holds them, not as read: the column descriptors byte for byte, and
//!    text cells too but for the padding after them (text a read trims, a code page's
//!    characters); numbers by value (`003.50` is `3.5`, `-0` is `0`), logicals by meaning (`Y`,
//!    `1` and `T` are one true, `N`, `0` and `F` one false) and a zero date as a blank one.
//!    Whatever a read and write of the file loses shows here, before the edits.
//! 3. The layer, written, reads back as the layer: every value and shape the edits made fits.
//!
//! A shape that wasn't edited is written with the coordinates the file holds, not converted to
//! longitude/latitude and back, so a projected file's shapes stay as they were to the bit.
//!
//! Only then are the new parts written beside the old ones, checked again against the bytes
//! meant, and renamed into place, the `.shp` last; any failure puts the old parts back
//! ([`crate::io::place_shapefile`]). The file keeps its code page, column widths, decimals,
//! `.prj` and `.cpg` where the edits allow; sidecars RustGIS doesn't write (`.shp.xml`) are left.

use crate::dbf::{Column, RawTable};
use crate::io::{DbfStyle, ShapefileParts};
use crate::raster::Crs;
use crate::sync::{FileSync, Synced};
use crate::{DataError, Dataset, Feature, FieldType, Geometry, Result, Value};
use std::path::{Path, PathBuf};

mod json;
pub use json::geojson;

/// How far an edited shape may read back from where the layer has it, relative to the size of its
/// coordinates, in a file in another system than longitude/latitude: the layer's shape is
/// projected to the file's system and read back through the inverse, which isn't exact to the
/// last bit (a few parts in 10¹³ measured, in a state plane system in feet). One part in 10¹¹ is
/// a tenth of a millimetre at ten thousand kilometres; anything more is refused.
const PROJECTED_TOLERANCE: f64 = 1e-11;

/// A layer written back to its source: the files and rows as written, to record as the new sync
/// point. `read` holds the rows in file order, with any format normalization (such as Shapefile
/// empty text reading as null or date-times as text); the engine preserves layer feature ids.
/// `notes` describes field changes, projection round-off and other format-specific effects.
pub struct Published {
    pub files: FileSync,
    pub read: Dataset,
    /// Digest of `read`, computed before placement so cancellation cannot interrupt completion.
    pub rows: u64,
    pub features: usize,
    pub notes: Vec<String>,
}

/// A refusal: nothing was written.
fn refused(path: &Path, why: impl std::fmt::Display) -> DataError {
    DataError::Format(format!("{}: {why}; nothing was written", path.display()))
}

/// Placement replaces a directory entry, so following a linked part while reading is unsafe.
fn refuse_link(source: &Path, part: &Path) -> Result<()> {
    match std::fs::symlink_metadata(part) {
        Ok(m) if m.file_type().is_symlink() => Err(refused(
            source,
            format!(
                "{} is a symbolic link; add the actual source files instead so publishing keeps the link and its target unchanged",
                part.display()
            ),
        )),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// A publisher's lock, released before its descriptor closes so duplicates inherited during
/// process creation cannot extend its lifetime.
struct SourceLock(std::fs::File);

impl Drop for SourceLock {
    /// Unlock the shared file description, including any duplicate descriptors.
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Holds an exclusive OS lock on a persistent sidecar through publication. The sidecar is
/// never unlinked: deleting it could let a third process lock a different inode while another
/// still holds the old one. Dropping the guard explicitly unlocks before closing its handle;
/// contention refuses immediately. This coordinates RustGIS publishers, not other programs.
fn source_lock(path: &Path) -> Result<SourceLock> {
    refuse_link(path, path)?;
    let target = path.canonicalize()?;
    let mut name = std::ffi::OsString::from(".");
    name.push(target.file_name().ok_or_else(|| refused(path, "it has no file name"))?);
    name.push(".rustgis-lock");
    let lock_path = target.with_file_name(name);
    if std::fs::symlink_metadata(&lock_path).is_ok_and(|m| !m.file_type().is_file()) {
        return Err(refused(path, format!("{} isn't a regular lock file", lock_path.display())));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32);
    }
    let opened = match options.open(&lock_path) {
        Ok(file) => {
            // The empty sidecar contains no source data. Accounts sharing the directory need
            // read/write access regardless of source groups, ACLs or a private umask: NFS
            // exclusive locks require a writable handle. Set modes through the open handle.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                file.set_permissions(std::fs::Permissions::from_mode(0o666))?;
            }
            Ok(file)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            options.create_new(false);
            options.open(&lock_path)
        }
        Err(e) => Err(e),
    };
    let file = opened.map_err(|e| refused(path, format!("couldn't open {}: {e}", lock_path.display())))?;
    if !file.metadata()?.is_file() {
        return Err(refused(path, "its lock isn't a regular file"));
    }
    file.try_lock().map_err(|e| match e {
        std::fs::TryLockError::WouldBlock => refused(path, "another RustGIS publisher holds this source; try again after it finishes"),
        std::fs::TryLockError::Error(e) => refused(path, format!("couldn't lock {}: {e}", lock_path.display())),
    })?;
    Ok(SourceLock(file))
}

/// The Shapefile at `path`'s parts as they are, read into memory, with its `.cpg`'s path.
struct Original {
    shp: Vec<u8>,
    shx: Option<Vec<u8>>,
    dbf: Vec<u8>,
    /// Where its table is, in whichever case the reader found it, so it is written back there.
    dbf_path: PathBuf,
    prj: Option<Vec<u8>>,
    cpg: Option<(PathBuf, Vec<u8>)>,
}

/// Reads an original file at `p`, `None` when absent. The caller supplies format-specific size advice.
fn read_part(p: &Path, size_error: fn(&str, u64) -> String) -> Result<Option<Vec<u8>>> {
    match std::fs::metadata(p) {
        Ok(m) if m.len() > crate::io::max_bytes() => Err(DataError::Format(size_error(&p.to_string_lossy(), crate::io::max_bytes()))),
        Ok(_) => Ok(Some(std::fs::read(p)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

impl Original {
    /// Reads the parts of the Shapefile at `path`: its `.shp` and `.dbf` must be there; its
    /// `.shx`, `.prj` and `.cpg` (in whichever case it is spelled) may be.
    fn read(path: &Path) -> Result<Original> {
        let read = |part: &Path| read_part(part, crate::io::shapefile_size_error);
        let missing = |ext: &str| refused(path, format!("its .{ext} isn't there"));
        let shp = read(path)?.ok_or_else(|| missing("shp"))?;
        let dbf_path = crate::io::dbf_sidecar(path);
        let dbf = read(&dbf_path)?.ok_or_else(|| missing("dbf"))?;
        let shx = read(&path.with_extension("shx"))?;
        let prj = read(&path.with_extension("prj"))?;
        let mut cpg = None;
        for ext in crate::dbf::CPG_SPELLINGS {
            let p = path.with_extension(ext);
            if let Some(bytes) = read(&p)? {
                cpg = Some((p, bytes));
                break;
            }
        }
        Ok(Original { shp, shx, dbf, dbf_path, prj, cpg })
    }

    /// Its parts as the reader takes them.
    fn parts(&self) -> ShapefileParts {
        ShapefileParts {
            shp: self.shp.clone(),
            shx: self.shx.clone(),
            dbf: self.dbf.clone(),
            prj: self.prj.as_ref().and_then(|p| String::from_utf8(p.clone()).ok()),
            cpg: self.cpg.as_ref().map(|(_, b)| b.clone()),
        }
    }

    /// The coordinate system its `.prj` names, as the reader takes it (`None` for none, one it
    /// can't read).
    fn crs(&self) -> Option<Crs> {
        let text = std::str::from_utf8(self.prj.as_deref()?).ok()?;
        crate::raster::wkt::read_wkt(text).ok().map(|(c, _)| c)
    }
}

/// Writes `ds`, read whole from the Shapefile at `path` when it and its files were as `synced`
/// records, back over that Shapefile: see the module's documentation for what it checks first
/// and what it keeps. Its work grows with the file: it is hashed twice, read twice, written in
/// memory twice and read back, the hashing and writing counting progress, and it stops once the
/// run is cancelled (before the last part is placed, which puts the old file back). Reading the
/// file into memory and back, and comparing the shapes, table and rows (one pass each, without
/// a check), are steps that run to their end before Cancel is seen. Hashing the prepared bytes
/// and writing/syncing each staged part likewise run as single steps.
pub fn shapefile(ds: &Dataset, path: &Path, synced: &Synced) -> Result<Published> {
    let _lock = source_lock(path)?;
    let unchanged = |when: &str| -> Result<()> {
        refuse_link(path, path)?;
        for ext in ["shx", "dbf", "prj"].into_iter().chain(crate::dbf::CPG_SPELLINGS) {
            refuse_link(path, &path.with_extension(ext))?;
        }
        refuse_link(path, &crate::io::dbf_sidecar(path))?;
        let changed = synced.files.changed(path)?;
        if changed.is_empty() {
            return Ok(());
        }
        Err(refused(path, format!("{} changed {when}, and writing the layer would overwrite that", changed.join(", "))))
    };
    unchanged("since the layer was read")?;
    let original = Original::read(path)?;
    let crs = ds.crs.filter(|c| *c != Crs::LonLat);
    let written_crs = crs.unwrap_or(Crs::LonLat);
    let mut notes = Vec::new();
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();

    let Some(table) = crate::dbf::raw_table(&original.dbf) else {
        return Err(refused(path, "its .dbf's header doesn't describe its records"));
    };
    // A record marked deleted still has its shape in the `.shp`; a rewrite would drop both.
    if table.deleted > 0 {
        return Err(refused(
            path,
            format!("{} of its records are marked deleted, and writing it again would remove them from the file", table.deleted),
        ));
    }
    let mut encoding_notes = Vec::new();
    let ldid = original.dbf.get(29).copied().unwrap_or(0);
    let style = DbfStyle {
        encoding: crate::dbf::encoding(original.cpg.as_ref().map(|(_, b)| b.as_slice()), ldid, &mut encoding_notes),
        ldid,
        columns: table.columns.clone(),
    };
    // A code page RustGIS can't write (one it reads as UTF-8 for want of a decoder): edited text
    // would be written in UTF-8 under the file's language driver.
    if let Some(note) = encoding_notes.first() {
        return Err(refused(path, format!("{note}, so its text can't be written in its own code page")));
    }

    // 1. It reads as the layer was read: in the layer's system, and in the file's own
    // coordinates (the system taken for longitude/latitude, which converts nothing).
    let (as_read, _) = crate::io::read_shapefile_parts(original.parts(), &name, ds.crs)?;
    if crate::sync::rows_digest(&as_read)? != synced.rows {
        // Define Projection changes the layer's CRS, which is part of its digest. The
        // unedited file must still reproduce the saved baseline in its own CRS. Keep the
        // current-CRS read above for imports with an override and native-coordinate reuse.
        let (baseline, _) = crate::io::read_shapefile_parts(original.parts(), &name, None)?;
        if crate::sync::rows_digest(&baseline)? != synced.rows {
            return Err(refused(
                path,
                "it reads otherwise now than at the saved sync point, even in the file's own coordinate system; the import CRS may differ or RustGIS may read it more fully now: restore the original import CRS or add the file again with the intended CRS",
            ));
        }
    }
    let raw = match crs {
        Some(_) => Some(crate::io::read_shapefile_parts(original.parts(), &name, Some(Crs::LonLat))?.0),
        None => None,
    };
    let as_held = raw.as_ref().map(|raw| (&as_read, raw));

    // 2. The file as read, written again, is the file.
    crate::progress::begin(as_read.features.len());
    let (again, _, _) = crate::io::shapefile_parts_styled(&*in_file(&as_read, crs, as_held)?, written_crs, Some(&style))?;
    let part = |parts: &[(&str, Vec<u8>)], ext: &str| parts.iter().find(|(e, _)| *e == ext).map(|(_, b)| b.clone()).unwrap_or_default();
    if let Some(why) = same_shapes(&original.shp, &part(&again, "shp"), 0.0) {
        return Err(refused(path, format!("written again, its shapes wouldn't be the same: {why}")));
    }
    if let Some(why) = same_table(&table, &part(&again, "dbf")) {
        return Err(refused(path, format!("written again, its table wouldn't be the same: {why}")));
    }

    // 3. The layer, written, reads back as the layer.
    if let Some(note) = crate::io::shapefile_names_note(&ds.fields, &name) {
        return Err(refused(path, note));
    }
    crate::progress::begin(ds.features.len());
    let (mut parts, left_out, widened) = crate::io::shapefile_parts_styled(&*in_file(ds, crs, as_held)?, written_crs, Some(&style))?;
    if left_out > 0 {
        return Err(refused(
            path,
            format!("{left_out} of the layer's features have no shape a Shapefile of its kind holds (none, or one of another kind)"),
        ));
    }
    let back = ShapefileParts {
        shp: part(&parts, "shp"),
        shx: Some(part(&parts, "shx")),
        dbf: part(&parts, "dbf"),
        prj: None,
        cpg: original.cpg.as_ref().map(|(_, b)| b.clone()),
    };
    let (read_back, _) = crate::io::read_shapefile_parts(back, &name, crs)?;
    // Compared as they are when the layer holds its coordinates in the system the file is read
    // back in (its own): written unconverted, they read back exactly. Otherwise both in
    // longitude/latitude, so compared alike whatever systems the two hold them in.
    fn lonlat<'d>(d: &'d Dataset, path: &Path) -> Result<std::borrow::Cow<'d, Dataset>> {
        d.in_lonlat().map_err(|e| match DataError::of_conversion(e) {
            DataError::Format(e) => refused(path, e),
            cancelled => cancelled,
        })
    }
    let systems_differ = ds.coord_crs() != read_back.coord_crs();
    let (layer, back) = if systems_differ {
        (lonlat(ds, path)?, lonlat(&read_back, path)?)
    } else {
        (std::borrow::Cow::Borrowed(ds), std::borrow::Cow::Borrowed(&read_back))
    };
    let projected = crs.is_some() && systems_differ;
    if let Some(why) = same_rows(&layer, &back, if projected { PROJECTED_TOLERANCE } else { 0.0 }) {
        return Err(refused(path, format!("the file couldn't hold the layer as it is: {why}")));
    }

    if projected && let Some(note) = projection_note(&layer, &back)? {
        notes.push(format!("{}: {note}", path.display()));
    }

    // The file's columns no field of the layer has now (deleted or renamed): left out, and said.
    let kept_names = crate::io::shapefile_column_names(&ds.fields);
    let gone: Vec<String> =
        table.columns.iter().filter(|c| !kept_names.iter().any(|n| n.as_bytes() == c.name.as_slice())).map(column).collect();
    if !gone.is_empty() {
        let shown: Vec<&str> = gone.iter().take(5).map(String::as_str).collect();
        let more = if gone.len() > 5 { format!(" and {} more", gone.len() - 5) } else { String::new() };
        notes.push(format!(
            "{}: {} left out, as no field of the layer has its name now: {}{more}",
            path.display(),
            if gone.len() == 1 { "1 column of the file was".to_string() } else { format!("{} columns of the file were", gone.len()) },
            shown.join(", ")
        ));
    }
    if let Some(note) = crate::io::dates_as_text_note(&ds.fields, &path.display().to_string()) {
        notes.push(note);
    }
    // The reader infers integers from whole numeric values when no decimals are declared in the DBF header.
    let integral: Vec<String> = ds
        .fields
        .iter()
        .zip(&read_back.fields)
        .filter(|(a, b)| a.ty == FieldType::Double && b.ty == FieldType::Integer)
        .map(|(field, _)| field.name.clone())
        .collect();
    if !integral.is_empty() {
        notes.push(format!(
            "{}: {} double field{} read back as integer because all non-null values are whole numbers: {}",
            path.display(),
            integral.len(),
            if integral.len() == 1 { "" } else { "s" },
            names_list(&integral)
        ));
    }
    if !widened.is_empty() {
        let shown: Vec<&str> = widened.iter().take(5).map(String::as_str).collect();
        let more = if widened.len() > 5 { format!(" and {} more", widened.len() - 5) } else { String::new() };
        notes.push(format!(
            "{}: {} columns other than the file's, to hold their values: {}{more}",
            path.display(),
            if widened.len() == 1 { "1 field was given".to_string() } else { format!("{} fields were given", widened.len()) },
            shown.join(", ")
        ));
    }

    // The `.prj` and `.cpg` as the file had them, unless the layer is in another system.
    parts.retain(|(e, _)| !matches!(*e, "prj" | "cpg"));
    if Some(written_crs) == original.crs() || (ds.crs.is_none() && original.crs().is_none()) {
        if let Some(prj) = &original.prj {
            parts.push(("prj", prj.clone()));
        }
    } else {
        let Some(prj) = (match crs {
            None => Some(crate::io::WGS84_PRJ.to_string()),
            Some(c) => crate::raster::wkt::to_wkt(c),
        }) else {
            return Err(refused(path, format!("the layer is in {}, which a .prj can't describe", written_crs.label())));
        };
        notes.push(format!("{}: its .prj now says {}, the system the layer is in", path.display(), written_crs.label()));
        parts.push(("prj", prj.into_bytes()));
    }
    let mut targets: Vec<(PathBuf, Vec<u8>)> = parts
        .into_iter()
        .map(|(e, bytes)| (if e == "dbf" { original.dbf_path.clone() } else { crate::io::shapefile_part(path, e) }, bytes))
        .collect();
    if let Some((cpg, bytes)) = &original.cpg {
        targets.push((cpg.clone(), bytes.clone()));
    }
    let rows = crate::sync::rows_digest(&read_back)?;
    // The record of the files as they will be, made before any is placed: once they are, the
    // publish has happened, whatever comes after.
    let files = FileSync::of_written(path, &targets)?;

    // Written beside the old parts, checked, and placed.
    let staged = crate::io::stage_shapefile(&targets)?;
    let checked = (|| -> Result<()> {
        for ((tmp, _), (_, bytes)) in staged.iter().zip(&targets) {
            if std::fs::read(tmp)? != *bytes {
                return Err(refused(path, format!("{} didn't read back as written", tmp.display())));
            }
        }
        unchanged("while the layer was written")
    })();
    if let Err(e) = checked {
        crate::io::unstage(&staged);
        return Err(e);
    }
    let kept = crate::io::place_shapefile(path, &staged)?;
    notes.extend(crate::io::kept_backups_note(path, &kept));
    Ok(Published { files, read: read_back, rows, features: ds.features.len(), notes })
}

/// `ds` in the file's coordinate system `crs` (`None`: longitude/latitude), from the system it
/// holds its coordinates in ([`Dataset::coord_crs`]). Given the file as read and in its own
/// coordinates (`as_held`, the same rows), a feature of a layer held in longitude/latitude whose
/// shape is the one read takes the file's coordinates as they are; any other is projected. A native
/// shape that failed conversion is refused: missing geometry cannot identify its source shape.
fn in_file<'a>(ds: &'a Dataset, crs: Option<Crs>, as_held: Option<(&Dataset, &Dataset)>) -> Result<std::borrow::Cow<'a, Dataset>> {
    // A native shape the read couldn't place (left without geometry) can't be told apart from a
    // feature without one, whatever system the layer holds its coordinates in.
    if let Some((read, raw)) = as_held {
        for (i, (f, r)) in read.features.iter().zip(raw.features.iter()).enumerate() {
            crate::progress::check()?;
            if f.geometry.is_none() && r.geometry.is_some() {
                return Err(DataError::Format(format!(
                    "source shape {} couldn't be converted to longitude/latitude, so its native geometry can't be matched safely; check the source projection and add the file again; nothing was written",
                    i + 1
                )));
            }
        }
    }
    // From the system the layer holds its coordinates in: as they are when that is the file's.
    let (c, from) = (crs.unwrap_or(Crs::LonLat), ds.coord_crs());
    if c == from {
        return Ok(std::borrow::Cow::Borrowed(ds));
    }
    let t = from.to(c).map_err(DataError::Format)?;
    // The file as read is matched by its longitude/latitude: only a layer held in them is.
    let as_held = as_held.filter(|_| from == Crs::LonLat);
    // The file as read, in longitude/latitude as the layer reads it (converted from the file's
    // own system by the conversion that gave the layer its copy).
    let read_in_lonlat = match as_held {
        Some((read, _)) => Some(read.in_lonlat().map_err(DataError::of_conversion)?),
        None => None,
    };
    let (read, raw): (Vec<&Feature>, Vec<&Feature>) = match (&read_in_lonlat, as_held) {
        (Some(read), Some((_, raw))) => (read.features.iter().collect(), raw.features.iter().collect()),
        _ => (Vec::new(), Vec::new()),
    };
    // File reads number rows anew; persistent layer ids can have gaps after a deletion.
    // Exact geometry keys retain correspondence without trusting those regenerated ids.
    let key = |f: &Feature| serde_json::to_vec(&f.geometry).map_err(|e| DataError::Format(e.to_string()));
    let mut at: std::collections::HashMap<Vec<u8>, (usize, bool)> = std::collections::HashMap::new();
    for (i, f) in read.iter().enumerate() {
        crate::progress::check()?;
        at.entry(key(f)?)
            .and_modify(|(first, ambiguous)| *ambiguous |= raw.get(*first).zip(raw.get(i)).is_some_and(|(a, b)| a.geometry != b.geometry))
            .or_insert((i, false));
    }
    let mut rows = Vec::with_capacity(ds.features.len());
    for f in ds.features.iter() {
        // A shape taken as the file holds it is quick, but a layer of many is not.
        crate::progress::check()?;
        let matched = at.get(&key(f)?);
        if matched.is_some_and(|(_, ambiguous)| *ambiguous) {
            return Err(DataError::Format("several source shapes read as the same geometry but have different native coordinates; their correspondence is ambiguous, so nothing was written".into()));
        }
        let held = matched.and_then(|&(i, _)| raw.get(i));
        let mut row = f.clone();
        match held {
            Some(h) => row.geometry = h.geometry.clone(),
            // Looks at Cancel without counting: the write that follows counts the features.
            None => crate::io::project_each(std::slice::from_mut(&mut row), &t, false)?,
        }
        rows.push(row);
    }
    let mut out = ds.clone();
    out.features = rows.into();
    out.held_in = c;
    Ok(std::borrow::Cow::Owned(out))
}

/// The first few names, quoted, and how many more.
fn names_list(names: &[String]) -> String {
    let shown: Vec<String> = names.iter().take(5).map(|n| format!("'{}'", crate::io::dbf_notice_name(n))).collect();
    let more = if names.len() > 5 { format!(" and {} more", names.len() - 5) } else { String::new() };
    format!("{}{more}", shown.join(", "))
}

/// A value's bytes for a notice: decoded loosely, quoted, cut after 40 bytes, control characters as
/// spaces.
fn shown(bytes: &[u8]) -> String {
    format!("'{}'", crate::io::dbf_notice_name(&String::from_utf8_lossy(bytes)))
}

/// A shape as the file holds it: its kind, and its coordinates part by part.
fn shape_parts(shape: &shapefile::Shape) -> (&'static str, Vec<Vec<(f64, f64)>>) {
    use shapefile::Shape;
    let xy = |ps: &[shapefile::Point]| ps.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>();
    let xym = |ps: &[shapefile::PointM]| ps.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>();
    let xyz = |ps: &[shapefile::PointZ]| ps.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>();
    match shape {
        Shape::NullShape => ("no shape", Vec::new()),
        Shape::Point(p) => ("Point", vec![vec![(p.x, p.y)]]),
        Shape::PointM(p) => ("PointM", vec![vec![(p.x, p.y)]]),
        Shape::PointZ(p) => ("PointZ", vec![vec![(p.x, p.y)]]),
        Shape::Multipoint(m) => ("Multipoint", vec![xy(m.points())]),
        Shape::MultipointM(m) => ("MultipointM", vec![xym(m.points())]),
        Shape::MultipointZ(m) => ("MultipointZ", vec![xyz(m.points())]),
        Shape::Polyline(l) => ("Polyline", l.parts().iter().map(|p| xy(p)).collect()),
        Shape::PolylineM(l) => ("PolylineM", l.parts().iter().map(|p| xym(p)).collect()),
        Shape::PolylineZ(l) => ("PolylineZ", l.parts().iter().map(|p| xyz(p)).collect()),
        Shape::Polygon(p) => ("Polygon", p.rings().iter().map(|r| xy(r.points())).collect()),
        Shape::PolygonM(p) => ("PolygonM", p.rings().iter().map(|r| xym(r.points())).collect()),
        Shape::PolygonZ(p) => ("PolygonZ", p.rings().iter().map(|r| xyz(r.points())).collect()),
        Shape::Multipatch(_) => ("Multipatch", Vec::new()),
    }
}

/// Whether `a` and `b` are within `tolerance` of each other, relative to their size.
fn near(a: f64, b: f64, tolerance: f64) -> bool {
    a == b || (a - b).abs() <= tolerance * a.abs().max(b.abs()).max(1.0)
}

/// Whether two lists of parts hold the same coordinates, within `tolerance`.
fn same_coords(a: &[Vec<(f64, f64)>], b: &[Vec<(f64, f64)>], tolerance: f64) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(p, q)| p.len() == q.len() && p.iter().zip(q).all(|(s, t)| near(s.0, t.0, tolerance) && near(s.1, t.1, tolerance)))
}

/// Why the shapes of `.shp` file `b` aren't those of `a`, or `None` when they are: one shape for
/// each, of the same kind, with the same coordinates within `tolerance`. Counts each kind of
/// difference and names the first shape of it.
fn same_shapes(a: &[u8], b: &[u8], tolerance: f64) -> Option<String> {
    let read = |bytes: &[u8]| -> std::result::Result<Vec<shapefile::Shape>, String> {
        let mut reader = shapefile::ShapeReader::new(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
        reader.iter_shapes().collect::<std::result::Result<Vec<_>, _>>().map_err(|e| e.to_string())
    };
    let (a, b) = match (read(a), read(b)) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => return Some(format!("its shapes couldn't be read ({e})")),
    };
    let (a, b): (Vec<_>, Vec<_>) = (a.iter().map(shape_parts).collect(), b.iter().map(shape_parts).collect());
    // Each kind of shape a read leaves out, with the first record holding one.
    let mut out = Vec::new();
    for kind in ["no shape", "Multipatch"] {
        let held: Vec<usize> = a.iter().enumerate().filter(|(_, (k, _))| *k == kind).map(|(i, _)| i + 1).collect();
        if let Some(first) = held.first() {
            let what = if kind == "no shape" { "have no shape" } else { "are multipatches" };
            out.push(format!("{} records {what}, which RustGIS leaves out (the first, record {first})", held.len()));
        }
    }
    if !out.is_empty() {
        return Some(out.join("; "));
    }
    if a.len() != b.len() {
        return Some(format!("{} shapes would be {}", a.len(), b.len()));
    }
    let kinds: Vec<usize> = a.iter().zip(&b).enumerate().filter(|(_, (s, t))| s.0 != t.0).map(|(i, _)| i).collect();
    if let Some(&first) = kinds.first() {
        let (was, now) = (a.get(first).map_or("", |s| s.0), b.get(first).map_or("", |s| s.0));
        let zm = if was.ends_with('Z') || was.ends_with('M') { ", as RustGIS doesn't keep Z or M values" } else { "" };
        return Some(format!("{} shapes would change kind{zm} (the first, record {}: {was} to {now})", kinds.len(), first + 1));
    }
    let moved: Vec<usize> =
        a.iter().zip(&b).enumerate().filter(|(_, (s, t))| !same_coords(&s.1, &t.1, tolerance)).map(|(i, _)| i).collect();
    moved.first().map(|first| format!("{} shapes would change (the first, record {})", moved.len(), first + 1))
}

/// A `.dbf` value as a value, for comparing what two files hold: text without the spaces or NULs
/// that pad it, a number as the decimal it says exactly (no `f64`, which can't tell
/// 9007199254740993 from 9007199254740992), a date or yes/no without the forms of empty.
#[derive(PartialEq)]
enum Cell<'a> {
    Bytes(&'a [u8]),
    /// Negative, the whole digits without leading zeros, the decimals without trailing zeros.
    Number(bool, &'a [u8], &'a [u8]),
    Empty,
}

/// A number's text as its sign, whole digits and decimals with the zeros that don't count left
/// off (`-003.50` is `-3.5`; `-0` is `0`), or `None` when it isn't plain decimal digits.
fn decimal(text: &[u8]) -> Option<(bool, &[u8], &[u8])> {
    let (negative, digits) = match text.split_first() {
        Some((b'-', rest)) => (true, rest),
        Some((b'+', rest)) => (false, rest),
        _ => (false, text),
    };
    let (whole, decimals) = match digits.iter().position(|b| *b == b'.') {
        Some(at) => (digits.get(..at)?, digits.get(at + 1..)?),
        None => (digits, &b""[..]),
    };
    if whole.is_empty() && decimals.is_empty() || !whole.iter().chain(decimals).all(u8::is_ascii_digit) {
        return None;
    }
    let whole = whole.get(whole.iter().position(|b| *b != b'0').unwrap_or(whole.len())..)?;
    let decimals = decimals.get(..decimals.iter().rposition(|b| *b != b'0').map_or(0, |i| i + 1))?;
    Some((negative && !(whole.is_empty() && decimals.is_empty()), whole, decimals))
}

/// What value `bytes` of a column of type `ty` holds ([`Cell`]).
fn cell(ty: u8, bytes: &[u8]) -> Cell<'_> {
    let padding = |b: &u8| *b == b' ' || *b == 0;
    let end = bytes.iter().rposition(|b| !padding(b)).map_or(0, |i| i + 1);
    let right = bytes.get(..end).unwrap_or_default();
    let trimmed = right.get(right.iter().position(|b| !padding(b)).unwrap_or(right.len())..).unwrap_or_default();
    match ty.to_ascii_uppercase() {
        b'C' => {
            if right.is_empty() {
                Cell::Empty
            } else {
                Cell::Bytes(right)
            }
        }
        b'N' | b'F' => {
            if trimmed.is_empty() {
                return Cell::Empty;
            }
            match decimal(trimmed) {
                Some((negative, whole, decimals)) => Cell::Number(negative, whole, decimals),
                None => Cell::Bytes(trimmed),
            }
        }
        b'L' => match trimmed.first().map(u8::to_ascii_uppercase) {
            Some(b'T' | b'Y' | b'1') => Cell::Bytes(b"T"),
            Some(b'F' | b'N' | b'0') => Cell::Bytes(b"F"),
            _ => Cell::Empty,
        },
        b'D' if trimmed.is_empty() || trimmed == b"00000000" => Cell::Empty,
        _ => Cell::Bytes(trimmed),
    }
}

/// A column described for a notice: its name, type, width and decimals (`N(10,2)`).
fn column(c: &Column) -> String {
    let name = String::from_utf8_lossy(&c.name);
    let ty = char::from(c.ty);
    if matches!(c.ty.to_ascii_uppercase(), b'N' | b'F') {
        format!("'{}' {ty}({},{})", crate::io::dbf_notice_name(&name), c.len, c.decimals)
    } else {
        format!("'{}' {ty}({})", crate::io::dbf_notice_name(&name), c.len)
    }
}

/// Why the `.dbf` `b` doesn't hold what `a` holds, or `None` when it does: the same columns
/// (names, types, widths, decimals) and, record by record, the same values ([`cell`]). Counts the
/// values that differ and names the first.
fn same_table(a: &RawTable, b: &[u8]) -> Option<String> {
    let Some(b) = crate::dbf::raw_table(b) else { return Some("its .dbf couldn't be written".into()) };
    if a.columns.len() != b.columns.len() {
        return Some(format!("it has {} columns and would have {}", a.columns.len(), b.columns.len()));
    }
    let changed: Vec<String> = a
        .columns
        .iter()
        .zip(&b.columns)
        .filter(|(s, t)| s.name != t.name || !s.ty.eq_ignore_ascii_case(&t.ty) || s.len != t.len || s.decimals != t.decimals)
        .map(|(s, t)| format!("{} would be {}", column(s), column(t)))
        .collect();
    if !changed.is_empty() {
        return Some(format!("its columns would change: {}", changed.join(", ")));
    }
    if a.records.len() != b.records.len() {
        return Some(format!("{} records would be {}", a.records.len(), b.records.len()));
    }
    let mut first = None;
    let mut count = 0usize;
    for (row, (r, s)) in a.records.iter().zip(&b.records).enumerate() {
        for (c, (x, y)) in a.columns.iter().zip(r.iter().zip(s)) {
            if cell(c.ty, x) != cell(c.ty, y) {
                count += 1;
                first.get_or_insert((row, c, *x, *y));
            }
        }
    }
    first.map(|(row, c, x, y)| {
        format!(
            "{count} values would change (the first, record {} of '{}': {} would be {})",
            row + 1,
            crate::io::dbf_notice_name(&String::from_utf8_lossy(&c.name)),
            shown(x),
            shown(y)
        )
    })
}

/// Whether two values of a field of type `ty` are the same once written to a `.dbf` and read:
/// numbers by their value (a whole number reads back as an integer), empty text and no value
/// alike (a `.dbf` can't tell them apart), a date-time by its text.
fn same_value(ty: FieldType, a: &Value, b: &Value) -> bool {
    let text = |v: &Value| match v {
        Value::Null => String::new(),
        v => v.to_string(),
    };
    match (a, b) {
        _ if a == b => true,
        (Value::Int(_) | Value::Float(_), Value::Int(_) | Value::Float(_)) => a.as_f64() == b.as_f64(),
        _ if matches!(ty, FieldType::Text | FieldType::DateTime) => text(a) == text(b),
        _ => false,
    }
}

/// A geometry as its parts of coordinates: a point is a part of one, each polygon ring closed
/// and turned to run the same way whichever way it was drawn (a Shapefile orders a ring's
/// vertices by whether it is a hole).
fn geometry_parts(g: Option<&Geometry>) -> Vec<Vec<(f64, f64)>> {
    let xy = |cs: &[crate::Coord]| cs.iter().map(|c| (c.x, c.y)).collect::<Vec<_>>();
    match g {
        None => Vec::new(),
        Some(Geometry::Point(c)) => vec![vec![(c.x, c.y)]],
        Some(Geometry::MultiPoint(cs)) => cs.iter().map(|c| vec![(c.x, c.y)]).collect(),
        Some(Geometry::Line(parts)) => parts.iter().map(|p| xy(p)).collect(),
        Some(Geometry::Polygon(polys)) => polys
            .iter()
            .flatten()
            .map(|r| {
                let mut ring = xy(r);
                if ring.first() != ring.last() {
                    ring.extend(ring.first().copied());
                }
                // Twice the signed area: positive runs anticlockwise.
                let area: f64 = ring.windows(2).map(|w| w.first().zip(w.get(1)).map_or(0.0, |(p, q)| p.0 * q.1 - q.0 * p.1)).sum();
                if area < 0.0 {
                    ring.reverse();
                }
                ring
            })
            .collect(),
    }
}

/// Projection round-off actually present in the checked read-back: a count, the first feature
/// and the largest coordinate difference in degrees. Exact round-trips need no notice.
fn projection_note(ds: &Dataset, back: &Dataset) -> Result<Option<String>> {
    let (mut count, mut first, mut largest) = (0usize, 0usize, 0.0_f64);
    for (k, (f, g)) in ds.features.iter().zip(back.features.iter()).enumerate() {
        crate::progress::check()?;
        let (p, q) = (geometry_parts(f.geometry.as_ref()), geometry_parts(g.geometry.as_ref()));
        if same_coords(&p, &q, 0.0) {
            continue;
        }
        if count == 0 {
            first = k + 1;
        }
        count += 1;
        for (a, b) in p.iter().flatten().zip(q.iter().flatten()) {
            largest = largest.max((a.0 - b.0).abs()).max((a.1 - b.1).abs());
        }
    }
    Ok((count > 0).then(|| {
        format!(
            "shapes with projection round-off: {count} (the first, feature {first}); the largest coordinate difference is {largest:e} degrees"
        )
    }))
}

/// Why `back`, the layer `ds` written to a Shapefile and read again, isn't `ds`, or `None` when
/// it is: the same fields, features, values ([`same_value`]) and shapes within `tolerance`.
/// Counts the values and shapes that differ and names the first of each.
fn same_rows(ds: &Dataset, back: &Dataset, tolerance: f64) -> Option<String> {
    let names = |d: &Dataset| d.fields.iter().map(|f| f.name.clone()).collect::<Vec<_>>();
    if names(ds) != names(back) {
        return Some(format!("its fields would read back as {}", names_list(&names(back))));
    }
    if ds.features.len() != back.features.len() {
        return Some(format!("its {} features would read back as {}", ds.features.len(), back.features.len()));
    }
    let (mut values, mut first_value, mut shapes, mut first_shape) = (0usize, None, 0usize, None);
    for (k, (f, g)) in ds.features.iter().zip(back.features.iter()).enumerate() {
        for (field, (a, b)) in ds.fields.iter().zip(f.attrs.iter().zip(&g.attrs)) {
            if !same_value(field.ty, a, b) {
                values += 1;
                first_value.get_or_insert((k, field.name.clone(), a.clone(), b.clone()));
            }
        }
        // Points are compared as parts of one point each, so a point and a multipoint of one agree.
        let (p, q) = (geometry_parts(f.geometry.as_ref()), geometry_parts(g.geometry.as_ref()));
        let grouping = |g: Option<&Geometry>| match g {
            Some(Geometry::Polygon(polys)) => Some(polys.iter().map(Vec::len).collect::<Vec<_>>()),
            _ => None,
        };
        if grouping(f.geometry.as_ref()) != grouping(g.geometry.as_ref()) || !same_coords(&p, &q, tolerance) {
            shapes += 1;
            first_shape.get_or_insert(k);
        }
    }
    let mut out = Vec::new();
    if let Some((k, name, a, b)) = first_value {
        let show = |v: &Value| if v.is_null() { "empty".to_string() } else { shown(v.to_string().as_bytes()) };
        out.push(format!(
            "{values} values would read back otherwise (the first, feature {} of '{}': {} as {})",
            k + 1,
            crate::io::dbf_notice_name(&name),
            show(&a),
            show(&b)
        ));
    }
    if let Some(k) = first_shape {
        out.push(format!("{shapes} shapes would read back otherwise (the first, feature {})", k + 1));
    }
    (!out.is_empty()).then(|| out.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Coord, Field, GeometryType};

    /// A layer held in the file's own system (#2032 C2) is written as it is held; one held in
    /// another is converted from it, without matching the file as read in longitude/latitude.
    #[test]
    fn a_layer_held_in_the_files_system_is_written_as_it_is() {
        let (utm, etrs) = (Crs::Epsg(32632), Crs::Epsg(25832));
        let at = Coord::new(500_000.0, 5_400_000.0);
        let mut ds = Dataset::new("held", GeometryType::Point, vec![]);
        ds.push(Some(Geometry::Point(at)), vec![]);
        ds.held_in = utm;
        let same = in_file(&ds, Some(utm), None).unwrap();
        assert!(matches!(same, std::borrow::Cow::Borrowed(_)));
        let other = in_file(&ds, Some(etrs), Some((&ds, &ds))).unwrap();
        assert_eq!(other.features[0].geometry, Some(Geometry::Point(utm.to(etrs).unwrap().point(at).unwrap())));
        assert_eq!(other.coord_crs(), etrs);
    }

    /// A small polygon layer of parcels with an owner, an area with decimals and a count.
    fn parcels() -> Dataset {
        let mut ds = Dataset::new(
            "parcels",
            GeometryType::Polygon,
            vec![Field::new("OWNER", FieldType::Text), Field::new("ACRES", FieldType::Double), Field::new("UNITS", FieldType::Integer)],
        );
        let square = |x: f64| {
            Geometry::Polygon(vec![vec![vec![
                Coord::new(x, 35.0),
                Coord::new(x, 35.1),
                Coord::new(x + 0.1, 35.1),
                Coord::new(x + 0.1, 35.0),
                Coord::new(x, 35.0),
            ]]])
        };
        ds.push(Some(square(-90.0)), vec![Value::Text("Smith".into()), Value::Float(1.25), Value::Int(2)]);
        ds.push(Some(square(-89.8)), vec![Value::Text("Jones".into()), Value::Float(0.5), Value::Int(1)]);
        ds
    }

    /// Writes `ds` as a Shapefile at `path` and reads it back with its sync record.
    fn written(ds: &Dataset, path: &Path) -> (Dataset, Synced) {
        crate::io::write_path(ds, path).unwrap();
        read(path)
    }

    /// Reads the Shapefile at `path` with its sync record, as Add Data does.
    fn read(path: &Path) -> (Dataset, Synced) {
        let (read, files) = FileSync::around(path, || crate::io::read_path(path).unwrap()).unwrap();
        let rows = crate::sync::rows_digest(&read.0).unwrap();
        (read.0, Synced { files: files.unwrap(), rows })
    }

    /// An edit is written back: the file reads as the layer, keeps its columns and `.prj`, and
    /// its new record matches the files as they are.
    #[test]
    fn an_edit_is_written_back_and_the_file_keeps_its_columns() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parcels.shp");
        let (mut ds, sync) = written(&parcels(), &path);
        let prj = std::fs::read(path.with_extension("prj")).unwrap();
        let columns = crate::dbf::raw_table(&std::fs::read(path.with_extension("dbf")).unwrap()).unwrap().columns;
        let mut rows: Vec<_> = ds.features.iter().cloned().collect();
        rows[0].attrs[0] = Value::Text("Smyth".into());
        ds.features = rows.into();
        let published = shapefile(&ds, &path, &sync).unwrap();
        assert!(published.notes.is_empty(), "{:?}", published.notes);
        let (back, _) = crate::io::read_path(&path).unwrap();
        assert_eq!(back.features.iter().next().unwrap().attrs[0], Value::Text("Smyth".into()));
        assert_eq!(std::fs::read(path.with_extension("prj")).unwrap(), prj);
        assert_eq!(crate::dbf::raw_table(&std::fs::read(path.with_extension("dbf")).unwrap()).unwrap().columns, columns);
        assert!(published.files.changed(&path).unwrap().is_empty());
        // Only the reusable lock sidecar remains; no staged or backup parts.
        let names: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        assert!(names.iter().all(|n| !n.starts_with('.') || n == ".parcels.shp.rustgis-lock"), "{names:?}");
    }

    /// A table named in upper case (`parcels.DBF`) is written back under that name, and an edit
    /// made to it afterwards outside RustGIS is seen.
    #[test]
    fn an_upper_case_table_is_published_under_its_own_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parcels.shp");
        crate::io::write_path(&parcels(), &path).unwrap();
        let upper = path.with_extension("DBF");
        std::fs::rename(path.with_extension("dbf"), &upper).unwrap();
        // On a case-insensitive file system the two names are one file; nothing to check there.
        if path.with_extension("dbf").exists() {
            return;
        }
        let (mut ds, sync) = read(&path);
        let mut rows: Vec<_> = ds.features.iter().cloned().collect();
        rows[0].attrs[0] = Value::Text("Smyth".into());
        ds.features = rows.into();
        let published = shapefile(&ds, &path, &sync).unwrap();
        assert!(!path.with_extension("dbf").exists());
        let (back, _) = crate::io::read_path(&path).unwrap();
        assert_eq!(back.features.iter().next().unwrap().attrs[0], Value::Text("Smyth".into()));
        assert!(published.files.changed(&path).unwrap().is_empty());
        let mut bytes = std::fs::read(&upper).unwrap();
        let last = bytes.len() - 2;
        bytes[last] ^= 1;
        std::fs::write(&upper, bytes).unwrap();
        assert_eq!(published.files.changed(&path).unwrap(), ["parcels.DBF"]);
    }

    /// Reads follow symlinks, but replacement would sever them and leave the target stale.
    #[cfg(unix)]
    #[test]
    fn linked_shapefile_parts_are_refused_without_replacing_the_link_or_target() {
        for ext in ["shp", "shx", "dbf", "prj", "CPG"] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("parcels.shp");
            written(&parcels(), &path);
            if ext == "CPG" {
                std::fs::write(path.with_extension(ext), b"UTF-8").unwrap();
            }
            let part = path.with_extension(ext);
            let target = dir.path().join(format!("original.{ext}"));
            std::fs::rename(&part, &target).unwrap();
            std::os::unix::fs::symlink(&target, &part).unwrap();
            let before = std::fs::read(&target).unwrap();
            let (mut ds, sync) = read(&path);
            let mut rows: Vec<_> = ds.features.iter().cloned().collect();
            rows[0].attrs[0] = Value::Text("Smyth".into());
            ds.features = rows.into();
            let e = shapefile(&ds, &path, &sync).err().expect("linked part must be refused").to_string();
            assert!(e.contains("symbolic link") && e.contains("nothing was written"), "{ext}: {e}");
            assert_eq!(std::fs::read_link(&part).unwrap(), target);
            assert_eq!(std::fs::read(&target).unwrap(), before);
            assert!(sync.files.changed(&path).unwrap().is_empty());
        }
    }

    /// An independent handle represents a concurrent publisher. Its lock refuses a publish
    /// without touching the source, and is reusable after closing, with no stale-lock cleanup.
    #[test]
    fn a_concurrent_publisher_is_refused_until_the_lock_is_released() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parcels.shp");
        let (mut ds, sync) = written(&parcels(), &path);
        let mut rows: Vec<_> = ds.features.iter().cloned().collect();
        rows[0].attrs[0] = Value::Text("Smyth".into());
        ds.features = rows.into();
        let held = source_lock(&path).unwrap();
        let e = shapefile(&ds, &path, &sync).err().unwrap().to_string();
        assert!(e.contains("another RustGIS publisher") && e.contains("nothing was written"), "{e}");
        assert!(sync.files.changed(&path).unwrap().is_empty());
        drop(held);
        shapefile(&ds, &path, &sync).unwrap();
        // A second publisher that loaded the old baseline now sees the committed change.
        assert!(shapefile(&ds, &path, &sync).err().unwrap().to_string().contains("changed since the layer was read"));
    }

    /// The publisher's original-file read stops before loading an oversized attribute sidecar.
    #[test]
    fn oversized_shapefile_dbf_read_for_publish_gives_dataset_advice() {
        use crate::io::{FileSizeAdvice, file_size_error_args, max_bytes};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parcels.shp");
        let dbf = path.with_extension("dbf");
        written(&parcels(), &path);
        std::fs::OpenOptions::new().write(true).open(&dbf).unwrap().set_len(max_bytes() + 1).unwrap();

        let error = Original::read(&path).err().unwrap().to_string();
        let limit = (max_bytes() >> 20).to_string();
        assert_eq!(file_size_error_args(&error), Some((dbf.to_str().unwrap(), limit.as_str(), FileSizeAdvice::Shapefile)));
        assert_eq!(std::fs::metadata(&dbf).unwrap().len(), max_bytes() + 1);
    }

    /// A duplicated descriptor must not keep a finished publisher's lock alive.
    #[test]
    fn a_source_lock_releases_while_a_duplicate_descriptor_stays_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parcels.shp");
        written(&parcels(), &path);
        let held = source_lock(&path).unwrap();
        let duplicate = held.0.try_clone().unwrap();
        drop(held);
        let next = source_lock(&path).expect("a finished publisher must release its lock despite a duplicated descriptor");
        drop(duplicate);
        assert!(source_lock(&path).err().unwrap().to_string().contains("another RustGIS publisher"));
        drop(next);
    }

    /// A shared source's empty lock is writable across accounts, even under a private umask.
    /// Reopening it keeps a writable handle (required by NFS) and excludes a second publisher.
    #[cfg(unix)]
    #[test]
    fn shared_source_locks_are_writable_and_reusable() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parcels.shp");
        written(&parcels(), &path);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o660)).unwrap();
        let held = source_lock(&path).unwrap();
        let lock_path = dir.path().join(".parcels.shp.rustgis-lock");
        assert_eq!(std::fs::metadata(&lock_path).unwrap().permissions().mode() & 0o777, 0o666);
        drop(held);
        let mut held = source_lock(&path).unwrap();
        // A zero-byte write checks the descriptor's access mode without storing source data.
        assert_eq!(held.0.write(&[]).unwrap(), 0);
        assert!(source_lock(&path).err().unwrap().to_string().contains("another RustGIS publisher"));
        drop(held);
        source_lock(&path).unwrap();
    }

    /// A collapsed hole is inferred as an outer ring on read: identical flat coordinates
    /// must not let that topology change overwrite the source.
    #[test]
    fn a_polygon_hole_that_reads_as_an_outer_ring_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parcels.shp");
        let (mut ds, sync) = written(&parcels(), &path);
        let mut rows: Vec<_> = ds.features.iter().cloned().collect();
        if let Some(Geometry::Polygon(polys)) = &mut rows[0].geometry {
            polys[0].push(vec![Coord::new(-89.98, 35.02), Coord::new(-89.96, 35.02), Coord::new(-89.98, 35.02)]);
        }
        ds.features = rows.into();
        let e = shapefile(&ds, &path, &sync).err().expect("changed ring grouping must be refused").to_string();
        assert!(e.contains("shapes would read back otherwise") && e.contains("nothing was written"), "{e}");
        assert!(sync.files.changed(&path).unwrap().is_empty());
    }

    /// Geometry edits remove obsolete indexes regardless of their extension's letter case.
    #[test]
    fn publishing_removes_case_variant_indexes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parcels.shp");
        let (mut ds, sync) = written(&parcels(), &path);
        let indexes = ["qix", "QIX", "qIx", "sbn", "SBN", "SBX"];
        for extension in indexes {
            std::fs::write(path.with_extension(extension), b"old index").unwrap();
        }
        let mut rows: Vec<_> = ds.features.iter().cloned().collect();
        if let Some(Geometry::Polygon(polys)) = &mut rows[0].geometry {
            polys[0][0][1].x += 0.005;
        }
        ds.features = rows.into();
        shapefile(&ds, &path, &sync).unwrap();
        for extension in indexes {
            assert!(!path.with_extension(extension).exists(), "stale index {extension} survived");
        }
    }

    /// A DBF record beyond its declared count must not disappear during an unrelated edit.
    #[test]
    fn publishing_refuses_unaccounted_dbf_records() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parcels.shp");
        crate::io::write_path(&parcels(), &path).unwrap();
        let mut dbf = std::fs::read(path.with_extension("dbf")).unwrap();
        let header = usize::from(u16::from_le_bytes([dbf[8], dbf[9]]));
        let width = usize::from(u16::from_le_bytes([dbf[10], dbf[11]]));
        let count = u32::from_le_bytes(dbf[4..8].try_into().unwrap()) as usize;
        let extra = dbf[header..header + width].to_vec();
        dbf.truncate(header + count * width);
        dbf.extend(extra);
        dbf.push(0x1a);
        std::fs::write(path.with_extension("dbf"), &dbf).unwrap();
        let (mut ds, sync) = read(&path);
        assert_eq!(ds.features.len(), 2);
        let mut rows: Vec<_> = ds.features.iter().cloned().collect();
        rows[0].attrs[0] = Value::Text("Smyth".into());
        ds.features = rows.into();
        let error = shapefile(&ds, &path, &sync).err().expect("uncounted DBF records must be refused").to_string();
        assert!(error.contains("header") && error.contains("records") && error.contains("nothing was written"), "{error}");
        assert!(sync.files.changed(&path).unwrap().is_empty());
    }

    /// A layer read otherwise than the file reads now (by an older RustGIS, or saved in a
    /// project that couldn't hold a value) is refused: its unedited rows aren't the file's.
    #[test]
    fn a_layer_read_otherwise_than_the_file_reads_now_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parcels.shp");
        let (ds, mut sync) = written(&parcels(), &path);
        // As an older read had it: one value otherwise.
        let mut older = ds.clone();
        let mut rows: Vec<_> = older.features.iter().cloned().collect();
        rows[0].attrs[1] = Value::Float(1.2);
        older.features = rows.into();
        sync.rows = crate::sync::rows_digest(&older).unwrap();
        let e = shapefile(&older, &path, &sync).err().unwrap().to_string();
        assert!(e.contains("reads otherwise now") && e.contains("nothing was written"), "{e}");
    }

    /// Defining a missing or incorrect source CRS publishes the definition without moving
    /// its native coordinates, and records a sync point that permits the next publish.
    #[test]
    fn a_defined_projection_publishes_without_moving_native_coordinates() {
        for (source, crs) in [(None, Crs::WebMercator), (None, Crs::LonLat), (Some(Crs::WebMercator), Crs::LonLat)] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("parcels.shp");
            crate::io::write_path(&parcels(), &path).unwrap();
            match source {
                Some(c) => std::fs::write(path.with_extension("prj"), crate::raster::wkt::to_wkt(c).unwrap()).unwrap(),
                None => std::fs::remove_file(path.with_extension("prj")).unwrap(),
            }
            let before = std::fs::read(&path).unwrap();
            let (ds, sync) = read(&path);
            let defined = crate::io::define_projection(&ds, crs).unwrap();
            let published = shapefile(&defined, &path, &sync).unwrap();
            if source.is_none() {
                assert!(same_shapes(&before, &std::fs::read(&path).unwrap(), 0.0).is_none());
            }
            assert!(published.notes.iter().any(|n| n.contains(".prj now says")), "{:?}", published.notes);
            let (back, next) = read(&path);
            assert_eq!(Original::read(&path).unwrap().crs(), Some(crs));
            assert_eq!(back.crs, (crs != Crs::LonLat).then_some(crs));
            assert!(same_rows(&defined, &back, PROJECTED_TOLERANCE).is_none());
            assert_eq!(next.rows, published.rows);
            shapefile(&back, &path, &next).unwrap();
        }
    }

    /// An unedited numeric overflow marker must not silently become an empty cell.
    #[test]
    fn an_overflow_cell_is_refused_without_changing_the_source() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parcels.shp");
        crate::io::write_path(&parcels(), &path).unwrap();
        let mut dbf = std::fs::read(path.with_extension("dbf")).unwrap();
        let table = crate::dbf::raw_table(&dbf).unwrap();
        let header = usize::from(u16::from_le_bytes([dbf[8], dbf[9]]));
        let at = header + 1 + usize::from(table.columns[0].len) + usize::from(table.columns[1].len);
        dbf[at..at + 18].fill(b'*');
        std::fs::write(path.with_extension("dbf"), &dbf).unwrap();
        let (mut ds, sync) = read(&path);
        assert_eq!(ds.features.first().unwrap().attrs[2], Value::Null);
        let mut rows: Vec<_> = ds.features.iter().cloned().collect();
        rows[1].attrs[0] = Value::Text("Jonas".into());
        ds.features = rows.into();
        let error = shapefile(&ds, &path, &sync).err().expect("overflow marker must be refused").to_string();
        assert!(error.contains("table wouldn't be the same") && error.contains("UNITS") && error.contains("***"), "{error}");
        assert!(sync.files.changed(&path).unwrap().is_empty());
    }

    /// A number past what an `f64` holds exactly, in a column that wasn't edited, is refused
    /// rather than written back rounded.
    #[test]
    fn a_number_an_f64_rounds_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parcels.shp");
        crate::io::write_path(&parcels(), &path).unwrap();
        // UNITS, an N(18,0) column, holding 2⁵³ + 1 in the first record.
        let mut dbf = std::fs::read(path.with_extension("dbf")).unwrap();
        let table = crate::dbf::raw_table(&dbf).unwrap();
        let header = usize::from(u16::from_le_bytes([dbf[8], dbf[9]]));
        let at = header + 1 + usize::from(table.columns[0].len) + usize::from(table.columns[1].len);
        dbf[at..at + 18].copy_from_slice(b"  9007199254740993");
        std::fs::write(path.with_extension("dbf"), &dbf).unwrap();
        let (mut ds, sync) = read(&path);
        let mut rows: Vec<_> = ds.features.iter().cloned().collect();
        rows[1].attrs[0] = Value::Text("Jonas".into());
        ds.features = rows.into();
        let e = shapefile(&ds, &path, &sync).err().unwrap().to_string();
        assert!(e.contains("table wouldn't be the same") && e.contains("9007199254740993"), "{e}");
        // Written the same in other forms, a number is the same.
        assert!(cell(b'N', b"  003.50") == cell(b'N', b"3.5"));
        assert!(cell(b'N', b"-0.00") == cell(b'N', b"0"));
        assert!(cell(b'N', b"9007199254740993") != cell(b'N', b"9007199254740992"));
    }

    /// A code page RustGIS reads as UTF-8 for want of a decoder (DOS French Canadian, by its
    /// language driver) is refused: edited text would be written in another code page.
    #[test]
    fn a_code_page_rustgis_cant_write_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parcels.shp");
        crate::io::write_path(&parcels(), &path).unwrap();
        std::fs::remove_file(path.with_extension("cpg")).unwrap();
        let mut dbf = std::fs::read(path.with_extension("dbf")).unwrap();
        dbf[29] = 0x6C;
        std::fs::write(path.with_extension("dbf"), &dbf).unwrap();
        let (ds, sync) = read(&path);
        let e = shapefile(&ds, &path, &sync).err().unwrap().to_string();
        assert!(e.contains("code page 863") && e.contains("nothing was written"), "{e}");
    }

    /// Once published, the layer as the file reads it (an empty text as no value) is its record:
    /// it publishes again.
    #[test]
    fn a_layer_the_file_reads_otherwise_publishes_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parcels.shp");
        let (mut ds, sync) = written(&parcels(), &path);
        let mut rows: Vec<_> = ds.features.iter().cloned().collect();
        rows[0].attrs[0] = Value::Text(String::new());
        ds.features = rows.into();
        let published = shapefile(&ds, &path, &sync).unwrap();
        assert_eq!(published.read.features.iter().next().unwrap().attrs[0], Value::Null);
        let synced = Synced { files: published.files, rows: crate::sync::rows_digest(&published.read).unwrap() };
        let mut ds = published.read;
        let mut rows: Vec<_> = ds.features.iter().cloned().collect();
        rows[1].attrs[0] = Value::Text("Jonas".into());
        ds.features = rows.into();
        shapefile(&ds, &path, &synced).unwrap();
    }

    /// A field deleted from the layer leaves the file's column out, and says so.
    #[test]
    fn a_deleted_field_leaves_its_column_out_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parcels.shp");
        let (mut ds, sync) = written(&parcels(), &path);
        ds.fields.remove(2);
        let rows: Vec<_> = ds
            .features
            .iter()
            .cloned()
            .map(|mut f| {
                f.attrs.remove(2);
                f
            })
            .collect();
        ds.features = rows.into();
        let published = shapefile(&ds, &path, &sync).unwrap();
        assert!(
            published.notes.iter().any(|n| n.contains("1 column of the file was left out") && n.contains("'UNITS' N(18,0)")),
            "{:?}",
            published.notes
        );
        let columns = crate::dbf::raw_table(&std::fs::read(path.with_extension("dbf")).unwrap()).unwrap().columns;
        assert_eq!(columns.len(), 2);
    }

    /// A file with records marked deleted is refused: writing it again would drop them, shapes and
    /// all.
    #[test]
    fn a_file_with_deleted_records_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parcels.shp");
        crate::io::write_path(&parcels(), &path).unwrap();
        let mut dbf = std::fs::read(path.with_extension("dbf")).unwrap();
        let header = usize::from(u16::from_le_bytes([dbf[8], dbf[9]]));
        dbf[header] = b'*';
        std::fs::write(path.with_extension("dbf"), &dbf).unwrap();
        let (ds, sync) = read(&path);
        let e = shapefile(&ds, &path, &sync).err().unwrap().to_string();
        assert!(e.contains("1 of its records are marked deleted") && e.contains("nothing was written"), "{e}");
        assert_eq!(std::fs::read(path.with_extension("dbf")).unwrap(), dbf);
    }

    /// A file changed since it was read is never written over.
    #[test]
    fn a_changed_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parcels.shp");
        let (ds, sync) = written(&parcels(), &path);
        let other = Dataset { features: ds.features.iter().take(1).cloned().collect::<Vec<_>>().into(), ..ds.clone() };
        crate::io::write_path(&other, &path).unwrap();
        let before = std::fs::read(path.with_extension("dbf")).unwrap();
        let e = shapefile(&ds, &path, &sync).err().unwrap().to_string();
        assert!(e.contains("changed since the layer was read") && e.contains("nothing was written"), "{e}");
        assert_eq!(std::fs::read(path.with_extension("dbf")).unwrap(), before);
    }

    /// Text in a code page is written back in it, with the file's language driver; a character
    /// the code page can't hold is refused rather than written as something else.
    #[test]
    fn a_code_page_is_kept_and_what_it_cant_hold_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parcels.shp");
        crate::io::write_path(&parcels(), &path).unwrap();
        // The same file in Windows-1252: its `.cpg` says so.
        std::fs::write(path.with_extension("cpg"), "1252").unwrap();
        let (mut ds, sync) = read(&path);
        let mut rows: Vec<_> = ds.features.iter().cloned().collect();
        rows[0].attrs[0] = Value::Text("Muñoz".into());
        ds.features = rows.clone().into();
        let published = shapefile(&ds, &path, &sync).unwrap();
        // The record of what was published, as the layer takes it.
        let synced = Synced { files: published.files, rows: crate::sync::rows_digest(&published.read).unwrap() };
        let dbf = std::fs::read(path.with_extension("dbf")).unwrap();
        assert!(dbf.windows(5).any(|w| w == b"Mu\xf1oz"), "written in Windows-1252");
        assert_eq!(std::fs::read(path.with_extension("cpg")).unwrap(), b"1252");
        assert_eq!(crate::io::read_path(&path).unwrap().0.features.iter().next().unwrap().attrs[0], Value::Text("Muñoz".into()));
        // A character Windows-1252 doesn't have.
        rows[1].attrs[0] = Value::Text("Łukasz".into());
        ds.features = rows.into();
        let e = shapefile(&ds, &path, &synced).err().unwrap().to_string();
        assert!(e.contains("couldn't hold the layer") && e.contains("Łukasz"), "{e}");
        assert_eq!(std::fs::read(path.with_extension("dbf")).unwrap(), dbf);
    }

    /// What a read loses is refused before the edits are: text a read trims, a feature without a
    /// shape, a number too precise for the column.
    #[test]
    fn what_a_read_loses_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parcels.shp");
        let mut ds = parcels();
        let mut rows: Vec<_> = ds.features.iter().cloned().collect();
        rows[0].attrs[0] = Value::Text("Smiths".into());
        ds.features = rows.into();
        crate::io::write_path(&ds, &path).unwrap();
        // The first owner `<Null>`, as another program may have written a missing value: it reads
        // as empty, and would be written back blank.
        let mut dbf = std::fs::read(path.with_extension("dbf")).unwrap();
        let at = dbf.windows(6).position(|w| w == b"Smiths").unwrap();
        dbf.splice(at..at + 6, *b"<Null>");
        std::fs::write(path.with_extension("dbf"), &dbf).unwrap();
        let (ds, sync) = read(&path);
        let e = shapefile(&ds, &path, &sync).err().unwrap().to_string();
        assert!(e.contains("table wouldn't be the same") && e.contains("'<Null>'"), "{e}");
    }

    /// Text with a space before it reads as the file has it, so it is written back unchanged (#2059).
    #[test]
    fn a_leading_space_is_written_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parcels.shp");
        crate::io::write_path(&parcels(), &path).unwrap();
        let mut dbf = std::fs::read(path.with_extension("dbf")).unwrap();
        let at = dbf.windows(5).position(|w| w == b"Smith").unwrap();
        dbf.splice(at..at + 5, *b" Smit");
        std::fs::write(path.with_extension("dbf"), &dbf).unwrap();
        let (ds, sync) = read(&path);
        shapefile(&ds, &path, &sync).unwrap();
        let (back, _) = read(&path);
        assert!(back.features.iter().any(|f| f.attrs.contains(&Value::Text(" Smit".into()))), "{:?}", back.features);
    }

    /// A float (`F`) column reads every digit its text holds, not the seven of an `f32`, and is
    /// written back as an `F` column with its width and decimals.
    #[test]
    fn a_float_column_keeps_its_digits_and_its_type() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parcels.shp");
        let mut ds = parcels();
        let mut rows: Vec<_> = ds.features.iter().cloned().collect();
        rows[0].attrs[1] = Value::Float(123456.12345678);
        ds.features = rows.into();
        crate::io::write_path(&ds, &path).unwrap();
        // ACRES, the second column, made a float column as other programs write one.
        let mut dbf = std::fs::read(path.with_extension("dbf")).unwrap();
        dbf[32 + 32 + 11] = b'F';
        std::fs::write(path.with_extension("dbf"), &dbf).unwrap();
        let (mut ds, sync) = read(&path);
        assert_eq!(ds.features.iter().next().unwrap().attrs[1], Value::Float(123456.12345678));
        let mut rows: Vec<_> = ds.features.iter().cloned().collect();
        rows[1].attrs[0] = Value::Text("Jonas".into());
        ds.features = rows.into();
        shapefile(&ds, &path, &sync).unwrap();
        let columns = crate::dbf::raw_table(&std::fs::read(path.with_extension("dbf")).unwrap()).unwrap().columns;
        assert_eq!(columns[1].ty, b'F');
        assert_eq!(crate::io::read_path(&path).unwrap().0.features.iter().next().unwrap().attrs[1], Value::Float(123456.12345678));
    }

    /// Converting a text field to a date changes its physical column, which publishing says.
    #[test]
    fn a_converted_field_names_its_old_and_new_column_types() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parcels.shp");
        let mut source = parcels();
        let mut rows: Vec<_> = source.features.iter().cloned().collect();
        for row in &mut rows {
            row.attrs[0] = Value::Text("2025-01-02".into());
        }
        source.features = rows.into();
        let (mut ds, sync) = written(&source, &path);
        ds.fields[0].ty = FieldType::Date;
        let mut rows: Vec<_> = ds.features.iter().cloned().collect();
        for row in &mut rows {
            row.attrs[0] = Value::Date(crate::Date::parse("2025-01-02").unwrap());
        }
        ds.features = rows.into();
        let published = shapefile(&ds, &path, &sync).unwrap();
        assert_eq!(published.notes.len(), 1, "{:?}", published.notes);
        assert!(published.notes[0].contains("'OWNER' C(10) to D(8)"), "{:?}", published.notes);
        let bytes = std::fs::read(path.with_extension("dbf")).unwrap();
        let table = crate::dbf::raw_table(&bytes).unwrap();
        assert_eq!(table.columns[0].ty, b'D');
    }

    /// A date-time still fits its text column, but losing the field type must be said once.
    #[test]
    fn a_date_time_field_written_as_text_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parcels.shp");
        let stamp = "2025-01-02T12:30:00Z";
        let mut source = parcels();
        let mut rows: Vec<_> = source.features.iter().cloned().collect();
        for row in &mut rows {
            row.attrs[0] = Value::Text(stamp.into());
        }
        source.features = rows.into();
        let (mut ds, sync) = written(&source, &path);
        ds.fields[0].ty = FieldType::DateTime;
        let mut rows: Vec<_> = ds.features.iter().cloned().collect();
        for row in &mut rows {
            row.attrs[0] = Value::DateTime(crate::DateTime::parse(stamp).unwrap());
        }
        ds.features = rows.into();
        let published = shapefile(&ds, &path, &sync).unwrap();
        assert_eq!(published.notes.len(), 1, "{:?}", published.notes);
        let note = &published.notes[0];
        assert!(note.contains("1 date-time field was written as text (ISO 8601)") && note.contains("'OWNER'"), "{note}");
        assert!(note.contains(&path.display().to_string()), "{note}");
        assert_eq!(published.read.fields[0].ty, FieldType::Text);
        assert_eq!(published.read.features.first().unwrap().attrs[0], Value::Text(stamp.into()));
    }

    /// Editing values to whole numbers keeps the read-back field kind as Double when decimals are declared.
    #[test]
    fn a_double_field_with_declared_decimals_reads_back_as_double() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parcels.shp");
        let (mut ds, sync) = written(&parcels(), &path);
        assert_eq!(ds.fields[1].ty, FieldType::Double);
        let mut rows: Vec<_> = ds.features.iter().cloned().collect();
        rows[0].attrs[1] = Value::Float(100.0);
        rows[1].attrs[1] = Value::Float(200.0);
        ds.features = rows.into();
        let published = shapefile(&ds, &path, &sync).unwrap();
        assert_eq!(published.read.fields[1].ty, FieldType::Double);
        assert_eq!(published.read.features.first().unwrap().attrs[1], Value::Float(100.0));
        assert!(published.notes.is_empty(), "{:?}", published.notes);
        let again = shapefile(&published.read, &path, &Synced { files: published.files, rows: published.rows }).unwrap();
        assert!(again.notes.is_empty(), "{:?}", again.notes);
    }

    /// When an existing file column has no declared decimals (N(x,0)), whole-valued Double data
    /// reuses the column and reads back as Integer, producing an explanatory note.
    #[test]
    fn a_double_field_reused_from_an_integer_column_reads_back_as_integer_and_notes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("counts.shp");
        let mut ds = Dataset::new("counts", GeometryType::Point, vec![Field::new("ID", FieldType::Integer)]);
        ds.push(Some(Geometry::Point(Coord::new(10.0, 20.0))), vec![Value::Int(100)]);
        let (mut ds, sync) = written(&ds, &path);
        assert_eq!(ds.fields[0].ty, FieldType::Integer);

        ds.fields[0].ty = FieldType::Double;
        let mut rows: Vec<_> = ds.features.iter().cloned().collect();
        rows[0].attrs[0] = Value::Float(100.0);
        ds.features = rows.into();

        let published = shapefile(&ds, &path, &sync).unwrap();
        assert_eq!(published.read.fields[0].ty, FieldType::Integer);
        assert_eq!(published.read.features.first().unwrap().attrs[0], Value::Int(100));
        assert_eq!(published.notes.len(), 1, "{:?}", published.notes);
        let note = &published.notes[0];
        assert!(note.contains("1 double field read back as integer") && note.contains("'ID'"), "{note}");
        assert!(note.contains(&path.display().to_string()), "{note}");
    }

    /// When an existing file column has declared decimals (e.g. N(10,2)), an Integer field does not
    /// keep the decimal column; it remakes the column without decimals and stays Integer.
    #[test]
    fn an_integer_field_does_not_reuse_a_decimal_column() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parcels.shp");
        let (mut ds, sync) = written(&parcels(), &path);
        assert_eq!(ds.fields[1].ty, FieldType::Double);

        ds.fields[1].ty = FieldType::Integer;
        let mut rows: Vec<_> = ds.features.iter().cloned().collect();
        rows[0].attrs[1] = Value::Int(100);
        rows[1].attrs[1] = Value::Int(200);
        ds.features = rows.into();

        let published = shapefile(&ds, &path, &sync).unwrap();
        assert_eq!(published.read.fields[1].ty, FieldType::Integer);
        assert_eq!(published.read.features.first().unwrap().attrs[1], Value::Int(100));
        assert_eq!(published.notes.len(), 1, "{:?}", published.notes);
        let note = &published.notes[0];
        assert!(note.contains("'ACRES' N(24,15) to N(24,0)"), "{note}");
        assert!(note.contains(&path.display().to_string()), "{note}");
    }

    /// A whole-valued double reads as Double when decimals are declared, and its existing wide decimal column still fits.
    #[test]
    fn an_integral_double_keeps_its_wide_decimal_column() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parcels.shp");
        let mut ds = parcels();
        let mut rows: Vec<_> = ds.features.iter().cloned().collect();
        rows[0].attrs[1] = Value::Float(100.0);
        rows[1].attrs[1] = Value::Float(200.0);
        ds.features = rows.into();
        let (mut ds, sync) = written(&ds, &path);
        assert_eq!(ds.fields[1].ty, FieldType::Double);
        let before = std::fs::read(path.with_extension("dbf")).unwrap();
        let mut rows: Vec<_> = ds.features.iter().cloned().collect();
        rows[0].attrs[0] = Value::Text("Smyth".into());
        ds.features = rows.into();
        let published = shapefile(&ds, &path, &sync).unwrap();
        assert!(published.notes.is_empty(), "{:?}", published.notes);
        let after = std::fs::read(path.with_extension("dbf")).unwrap();
        let (a, b) = (crate::dbf::raw_table(&before).unwrap(), crate::dbf::raw_table(&after).unwrap());
        assert_eq!(a.columns, b.columns);
        for (a, b) in a.records.iter().zip(&b.records) {
            assert_eq!(a[1], b[1]);
        }
    }

    /// An unconverted source shape cannot supply geometry to a new geometry-less feature.
    #[test]
    fn an_unconverted_shape_cannot_be_copied_to_a_geometryless_feature() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parcels.shp");
        let mut native = Dataset::new("parcels", GeometryType::Point, vec![Field::new("OWNER", FieldType::Text)]);
        native.push(Some(Geometry::Point(Coord::new(1e30, 1e30))), vec![Value::Text("old".into())]);
        native.push(Some(Geometry::Point(Coord::new(500_000.0, 0.0))), vec![Value::Text("kept".into())]);
        crate::io::write_path(&native, &path).unwrap();
        let crs = Crs::parse("EPSG:32616").unwrap();
        std::fs::write(path.with_extension("prj"), crate::raster::wkt::to_wkt(crs).unwrap()).unwrap();
        let (mut ds, sync) = read(&path);
        assert_eq!(ds.features.iter().filter(|f| f.geometry.is_none()).count(), 1);
        ds.push(None, vec![Value::Text("new".into())]);
        let error = shapefile(&ds, &path, &sync).err().expect("unconverted shapes must not be assigned to new features").to_string();
        assert!(error.contains("source shape 1") && error.contains("converted") && error.contains("nothing was written"), "{error}");
        assert!(sync.files.changed(&path).unwrap().is_empty());
    }

    /// After deletion, persistent layer ids differ from file row numbers. An attribute-only
    /// second publish must still copy the untouched projected coordinates to the bit.
    #[test]
    fn repeated_publication_keeps_native_shapes_after_deleting_a_feature() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parcels.shp");
        let crs = Crs::parse("EPSG:2274").unwrap();
        crate::io::write_path_in(&parcels(), &path, crs).unwrap();
        let (mut ds, sync) = read(&path);
        let survivor = ds.features.get(1).unwrap().clone();
        let id = survivor.id;
        ds.features = vec![survivor].into();
        let published = shapefile(&ds, &path, &sync).unwrap();
        ds = published.read;
        let mut rows: Vec<_> = ds.features.iter().cloned().collect();
        rows[0].id = id;
        rows[0].attrs[0] = Value::Text("Smyth".into());
        ds.features = rows.into();
        let sync = Synced { files: published.files, rows: published.rows };
        let before = std::fs::read(&path).unwrap();
        shapefile(&ds, &path, &sync).unwrap();
        assert!(same_shapes(&before, &std::fs::read(&path).unwrap(), 0.0).is_none());
    }

    /// The writer uses UTF-8 name bytes even with a legacy code page: a rewrite is refused
    /// before the differing name or width can reach the source.
    #[test]
    fn a_legacy_non_ascii_column_name_refuses_the_rewrite_without_changing_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parcels.shp");
        written(&parcels(), &path);
        std::fs::write(path.with_extension("cpg"), "1252").unwrap();
        let mut bytes = std::fs::read(path.with_extension("dbf")).unwrap();
        bytes[32..43].fill(0);
        bytes[32..37].copy_from_slice(b"Se\xf1or");
        std::fs::write(path.with_extension("dbf"), &bytes).unwrap();
        let (mut ds, sync) = read(&path);
        assert_eq!(ds.fields[0].name, "Señor");
        let mut rows: Vec<_> = ds.features.iter().cloned().collect();
        rows[0].attrs[0] = Value::Text("Smyth".into());
        ds.features = rows.into();
        let e = shapefile(&ds, &path, &sync).err().unwrap().to_string();
        assert!(e.contains("its columns would change") && e.contains("nothing was written"), "{e}");
        assert_eq!(std::fs::read(path.with_extension("dbf")).unwrap(), bytes);
        assert!(sync.files.changed(&path).unwrap().is_empty());
    }

    /// A widened column is said; a layer in another system gets a `.prj` for it, said too.
    #[test]
    fn a_wider_value_widens_its_column_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parcels.shp");
        let (mut ds, sync) = written(&parcels(), &path);
        let mut rows: Vec<_> = ds.features.iter().cloned().collect();
        rows[0].attrs[0] = Value::Text("Smith and Jones Holdings".into());
        ds.features = rows.into();
        let published = shapefile(&ds, &path, &sync).unwrap();
        assert_eq!(published.notes.len(), 1, "{:?}", published.notes);
        assert!(published.notes[0].contains("1 field was given columns other than the file's"), "{:?}", published.notes);
        assert!(published.notes[0].contains("'OWNER' C(5) to C(24)"), "{:?}", published.notes);
        let (back, _) = crate::io::read_path(&path).unwrap();
        assert_eq!(back.features.iter().next().unwrap().attrs[0], Value::Text("Smith and Jones Holdings".into()));
    }

    /// A layer held in the file's system (#2032 C2) is compared with what the file reads back as
    /// in longitude/latitude, not its metres with degrees: an edit of its table is written.
    #[test]
    fn a_layer_held_in_the_files_system_publishes_an_edit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parcels.shp");
        let crs = Crs::parse("EPSG:32616").unwrap();
        crate::io::write_path_in(&parcels(), &path, crs).unwrap();
        let (mut ds, sync) = read(&path);
        ds.hold_in(crs).unwrap();
        let mut rows: Vec<_> = ds.features.iter().cloned().collect();
        rows[0].attrs[0] = Value::Text("Smyth".into());
        ds.features = rows.into();
        let published = shapefile(&ds, &path, &sync).unwrap();
        assert!(published.notes.is_empty(), "{:?}", published.notes);
        let (back, _) = crate::io::read_path(&path).unwrap();
        assert_eq!(back.features.iter().next().unwrap().attrs[0], Value::Text("Smyth".into()));
    }

    /// A projected file's unedited shapes stay where they were, to the tolerance.
    #[test]
    fn a_projected_file_keeps_its_unedited_shapes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parcels.shp");
        // UTM in metres, and a state plane system in US feet.
        for code in ["EPSG:32616", "EPSG:2274"] {
            let crs = Crs::parse(code).unwrap();
            let mut ds = parcels();
            ds.crs = Some(crs);
            crate::io::write_path_in(&ds, &path, crs).unwrap();
            let before = std::fs::read(&path).unwrap();
            let (mut ds, sync) = read(&path);
            let mut rows: Vec<_> = ds.features.iter().cloned().collect();
            rows[1].attrs[2] = Value::Int(3);
            ds.features = rows.into();
            let published = shapefile(&ds, &path, &sync).unwrap();
            assert!(published.notes.is_empty(), "{code}: {:?}", published.notes);
            // Every shape as it was, to the bit: none was edited.
            assert!(same_shapes(&before, &std::fs::read(&path).unwrap(), 0.0).is_none(), "{code}");
            // A shape moved is projected; the others stay as they were.
            let (mut ds, sync) = read(&path);
            let mut rows: Vec<_> = ds.features.iter().cloned().collect();
            if let Some(Geometry::Polygon(polys)) = &mut rows[1].geometry {
                polys[0][0][1].y += 0.01;
            }
            ds.features = rows.into();
            let published = shapefile(&ds, &path, &sync).unwrap();
            // Whether the moved shape's round trip through the projection is exact depends on
            // the platform's libm, so the note is checked against what the file reads back as.
            let (back, _) = read(&path);
            let off = differing(&ds, &back);
            // Only the moved shape may differ; whether it does is up to the libm (the test that
            // requires a publish giving the note on any platform:
            // `a_publish_with_projection_round_off_says_so`).
            assert!(off.is_empty() || off == [2], "{code}: unedited shapes changed: {off:?}");
            if !off.is_empty() {
                assert_eq!(published.notes.len(), 1, "{code}: {:?}", published.notes);
                let note = &published.notes[0];
                assert!(
                    note.contains("shapes with projection round-off: 1 (the first, feature 2)") && note.ends_with(" degrees"),
                    "{code}: {note}"
                );
            } else {
                assert!(published.notes.is_empty(), "{code}: {:?}", published.notes);
            }
            let shapes = |b: &[u8]| -> Vec<_> {
                let mut r = shapefile::ShapeReader::new(std::io::Cursor::new(b.to_vec())).unwrap();
                r.iter_shapes().map(|s| shape_parts(&s.unwrap()).1).collect()
            };
            let (was, now) = (shapes(&before), shapes(&std::fs::read(&path).unwrap()));
            assert_eq!(was[0], now[0], "{code}");
            assert_ne!(was[1], now[1], "{code}");
        }
    }

    /// The features of `back` whose shapes aren't those of `ds` to the bit, counted from 1. The
    /// read-back must have every feature.
    fn differing(ds: &Dataset, back: &Dataset) -> Vec<usize> {
        assert_eq!(back.features.len(), ds.features.len());
        ds.features
            .iter()
            .zip(back.features.iter())
            .enumerate()
            .filter(|(_, (f, g))| !same_coords(&geometry_parts(f.geometry.as_ref()), &geometry_parts(g.geometry.as_ref()), 0.0))
            .map(|(k, _)| k + 1)
            .collect()
    }

    /// Round-off in the read-back is counted, the first feature named and the largest difference
    /// given; an exact read-back gives no note. Independent of any platform's libm.
    #[test]
    fn projection_round_off_is_counted_and_named() {
        let mut ds = parcels();
        // The two again after them, so exact and off features alternate.
        let again: Vec<_> = ds.features.iter().cloned().collect();
        for f in again {
            ds.push(f.geometry, f.attrs);
        }
        assert_eq!(ds.features.len(), 4);
        assert!(projection_note(&ds, &ds).unwrap().is_none());
        // Features 1 and 3 exact, a y off in feature 2 and an x off in feature 4, each the larger
        // once, so the count skips the exact ones and both axes count.
        for (dy, dx) in [(3e-12, 1e-12), (1e-12, 3e-12)] {
            let mut rows: Vec<_> = ds.features.iter().cloned().collect();
            let (mut moved_y, mut moved_x) = (0.0, 0.0);
            if let Some(Geometry::Polygon(polys)) = &mut rows[1].geometry {
                let y = polys[0][0][1].y;
                polys[0][0][1].y = y + dy;
                // The steps as stored, which aren't exactly dy and dx.
                moved_y = polys[0][0][1].y - y;
            }
            if let Some(Geometry::Polygon(polys)) = &mut rows[3].geometry {
                let x = polys[0][0][2].x;
                polys[0][0][2].x = x + dx;
                moved_x = polys[0][0][2].x - x;
            }
            assert!(moved_y > 0.0 && moved_x > 0.0 && moved_y != moved_x);
            let largest = f64::max(moved_y, moved_x);
            let mut back = ds.clone();
            back.features = rows.into();
            let note = projection_note(&ds, &back).unwrap().unwrap();
            assert_eq!(
                note,
                format!(
                    "shapes with projection round-off: 2 (the first, feature 2); the largest coordinate difference is {largest:e} degrees"
                )
            );
        }
    }

    /// A publish whose read-back has projection round-off says so through `shapefile()`, on any
    /// platform: a layer held in longitude/latitude (as a project saved by an older release keeps
    /// its copy), whose moves are converted into the file's system. Whether a given move's round
    /// trip is exact is up to the libm, so several are tried in each system: each gives the note
    /// exactly when its read-back differs, and at least one must, or the noted publish went
    /// untested. A layer held in the file's system, as one read now is, writes its coordinates as
    /// they are.
    #[test]
    fn a_publish_with_projection_round_off_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parcels.shp");
        let (mut noted, mut exact) = (Vec::new(), Vec::new());
        for code in ["EPSG:32616", "EPSG:2274"] {
            let crs = Crs::parse(code).unwrap();
            for dy in [0.01, 0.013, 0.017, 0.019, 0.023, 0.029, 0.031, 0.037, 0.041, 0.043, 0.047, 0.053] {
                let mut ds = parcels();
                ds.crs = Some(crs);
                crate::io::write_path_in(&ds, &path, crs).unwrap();
                let (native, sync) = read(&path);
                assert_eq!(native.held_in, crs, "{code}: read in the file's own system");
                let mut ds = native.in_lonlat().unwrap().into_owned();
                let mut rows: Vec<_> = ds.features.iter().cloned().collect();
                if let Some(Geometry::Polygon(polys)) = &mut rows[1].geometry {
                    polys[0][0][1].y += dy;
                }
                ds.features = rows.into();
                let published = shapefile(&ds, &path, &sync).unwrap();
                let (back, _) = read(&path);
                let off = differing(&ds, &back.in_lonlat().unwrap());
                assert!(off.is_empty() || off == [2], "{code} {dy}: unedited shapes changed: {off:?}");
                assert_eq!(published.notes.len(), usize::from(!off.is_empty()), "{code} {dy}: {:?}", published.notes);
                if let Some(note) = published.notes.first() {
                    assert!(
                        note.contains("shapes with projection round-off: 1 (the first, feature 2)")
                            && note.ends_with(" degrees")
                            && !note.contains("difference is 0e0 "),
                        "{code} {dy}: {note}"
                    );
                    noted.push((code, dy));
                } else {
                    exact.push((code, dy));
                }
            }
        }
        assert!(
            !noted.is_empty(),
            "no move left projection round-off in either system ({} exact), so the noted publish went \
             untested; a more exact projection can do this: try other moves",
            exact.len()
        );
    }
}
