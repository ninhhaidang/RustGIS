//! Disk-backed record directory and R-tree. Neither grows the provider's resident heap.

use super::{BUFFER, Place, bad, be_i32, le_i32, record_box};
use crate::Result;
use rusqlite::{Connection, OpenFlags, params};
use std::io::{BufReader, Read};
use std::path::Path;
use std::sync::Mutex;

struct DiskIndex {
    db: Mutex<Connection>,
    pub count: u64,
    covered: u32,
    bounds: Option<[f32; 4]>,
    /// Records with no usable box, stored or from their points: left out of `bounds` and of the
    /// R-tree.
    unboxed: u32,
    _lease: super::cache::Lease,
}

fn sql(e: rusqlite::Error) -> crate::DataError {
    bad(format!("spatial index: {e}"))
}

fn connect(path: &Path, write: bool) -> Result<Connection> {
    let flags = if write { OpenFlags::SQLITE_OPEN_READ_WRITE } else { OpenFlags::SQLITE_OPEN_READ_ONLY };
    let db = Connection::open_with_flags(path, flags | OpenFlags::SQLITE_OPEN_NO_MUTEX).map_err(sql)?;
    db.execute_batch("PRAGMA trusted_schema=OFF; PRAGMA cache_size=-8192; PRAGMA mmap_size=0; PRAGMA temp_store=FILE;").map_err(sql)?;
    Ok(db)
}

impl DiskIndex {
    pub fn open(path: &Path, len: u64, notes: &mut Vec<String>) -> Result<Self> {
        use super::cache;
        use crate::source::external::{fnv1a, stamp};
        let canonical = path.canonicalize()?;
        let signature = format!("shp-index-v7:{canonical:?}:{:?}", stamp(path)?);
        let mut dir = cache::index_dir();
        let budget = cache::index_budget();
        let key = format!("{:016x}-{:016x}", fnv1a(format!("shp-index-v7:{canonical:?}").as_bytes()), fnv1a(signature.as_bytes()));
        let pinned = cache::pin_index(&dir, &key, budget).or_else(|_| {
            dir = cache::index_fallback_dir();
            cache::pin_index(&dir, &key, budget)
        });
        if let Ok(Some(lease)) = pinned
            && let Ok(db) = connect(lease.path(), false)
            && let Ok((saved, count, note, covered, bounds, unboxed)) =
                db.query_row("SELECT signature, count, note, covered, bounds, unboxed FROM metadata", [], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, u32>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, u32>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, u32>(5)?,
                    ))
                })
            && saved == signature
            && covered.checked_add(unboxed).is_some_and(|n| n <= count)
            && let Ok(bounds) = serde_json::from_str::<Option<[f32; 4]>>(&bounds)
            // Held to the rule it was built by: a damaged or planted entry is built again rather
            // than giving the layer a box that isn't one (#1985).
            && bounds.is_none_or(usable)
            && bounds.is_some() == (covered > 0)
            && db.prepare("SELECT id, offset, len FROM records LIMIT 0").is_ok()
            && db.prepare("SELECT id FROM bounds LIMIT 0").is_ok()
        {
            if !note.is_empty() {
                notes.push(note);
            }
            // A reused index adds nothing to the cache, so there is nothing to sweep (#1230).
            return Ok(Self { db: Mutex::new(db), count: u64::from(count), covered, bounds, unboxed, _lease: lease });
        }
        // Building one sweeps each cache folder it tries once: when the index is committed or, if
        // it fails, when it is released or its reservation fails. Abandoned builds and indexes past
        // the budget go then, and so do this file's earlier versions and indexes unused too long.
        let fallback = cache::index_fallback_dir();
        let reserved = cache::reserve_index(&dir, &key, budget);
        // Already the fallback folder: trying it again would only sweep it again. A failed
        // reservation there scans the records instead, as a failed fallback does.
        let mut lease = if dir == fallback { reserved? } else { reserved.or_else(|_| cache::reserve_index(&fallback, &key, budget))? };
        let mut db = connect(lease.path(), true)?;
        // A disposable cache: no journal allocation proportional to the source is necessary.
        db.execute_batch(
            "PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF;
            CREATE TABLE records(id INTEGER PRIMARY KEY, offset INTEGER NOT NULL, len INTEGER NOT NULL);
            CREATE VIRTUAL TABLE bounds USING rtree(id, x0, x1, y0, y1);
            CREATE TABLE metadata(signature TEXT, count INTEGER, note TEXT, covered INTEGER, bounds TEXT, unboxed INTEGER);",
        )
        .map_err(sql)?;
        let tx = db.transaction().map_err(sql)?;
        let (mut covered, mut unboxed) = (0u32, 0u32);
        let mut all_bounds: Option<[f32; 4]> = None;
        let (count, note) = {
            let mut record = tx.prepare("INSERT INTO records VALUES(?1,?2,?3)").map_err(sql)?;
            let mut bounds = tx.prepare("INSERT INTO bounds VALUES(?1,?2,?3,?4,?5)").map_err(sql)?;
            scan_boxed(path, len, |id, place, b| {
                record.execute(params![id, place.offset, place.len]).map_err(sql)?;
                if union(&mut all_bounds, b) {
                    let [x0, y0, x1, y1] = b;
                    bounds.execute(params![id, x0, x1, y0, y1]).map_err(sql)?;
                    covered += 1;
                } else if not_a_box(b) {
                    unboxed += 1;
                }
                Ok(())
            })?
        };
        // Do not retain an index built while the original file was being replaced/edited.
        if signature != format!("shp-index-v7:{canonical:?}:{:?}", stamp(path)?) {
            return Err(bad("the .shp changed while its spatial index was being built"));
        }
        let bounds_json = serde_json::to_string(&all_bounds).map_err(|e| bad(e.to_string()))?;
        tx.execute("INSERT INTO metadata VALUES(?1,?2,?3,?4,?5,?6)", params![signature, count, note, covered, bounds_json, unboxed])
            .map_err(sql)?;
        tx.commit().map_err(sql)?;
        drop(db);
        lease.commit()?;
        let db = connect(lease.path(), false)?;
        if !note.is_empty() {
            notes.push(note);
        }
        Ok(Self { db: Mutex::new(db), count: u64::from(count), covered, bounds: all_bounds, unboxed, _lease: lease })
    }

    pub fn covers_bbox(&self, extent: [f32; 4]) -> bool {
        self.bounds.is_some_and(|b| extent[0] <= b[0] && extent[1] <= b[1] && extent[2] >= b[2] && extent[3] >= b[3])
    }

    /// Visit in file order without allocating a list of all matches. SQLite's page cache is
    /// budgeted at 8 MiB; a large ordering operation spills to disk.
    pub fn visit(&self, extent: Option<[f32; 4]>, limit: usize, visit: impl FnMut(u32, Place) -> Result<()>) -> Result<(u64, bool)> {
        let db = self.db.lock().map_err(|_| bad("spatial index lock is poisoned"))?;
        self.visit_on(&db, extent, limit, visit)
    }

    /// A complete render owns its read-only connection, so its callbacks never hold the
    /// interaction connection's mutex. The index lease keeps this immutable file alive.
    fn visit_complete(&self, extent: Option<[f32; 4]>, visit: impl FnMut(u32, Place) -> Result<()>) -> Result<(u64, bool)> {
        let db = connect(self._lease.path(), false)?;
        self.visit_on(&db, extent, u32::MAX as usize, visit)
    }

    fn visit_on(
        &self,
        db: &Connection,
        extent: Option<[f32; 4]>,
        limit: usize,
        mut visit: impl FnMut(u32, Place) -> Result<()>,
    ) -> Result<(u64, bool)> {
        // The index's own envelope (not the untrusted file header) proves that every box
        // matches. Skip the R-tree and its global ordering operation for overview requests.
        let extent = extent.filter(|e| u64::from(self.covered) != self.count || !self.covers_bbox(*e));
        let (from, args) = if let Some([x0, y0, x1, y1]) = extent {
            ("bounds CROSS JOIN records USING(id) WHERE x0<=?1 AND x1>=?2 AND y0<=?3 AND y1>=?4", vec![x1, x0, y1, y0])
        } else {
            ("records", Vec::new())
        };
        let matched = if extent.is_none() {
            self.count
        } else {
            let n: u32 =
                db.query_row(&format!("SELECT count(*) FROM {from}"), rusqlite::params_from_iter(&args), |r| r.get(0)).map_err(sql)?;
            u64::from(n)
        };
        if limit == 0 {
            return Ok((matched, false));
        }
        let step = crate::source::stride(matched, limit).max(1);
        // A thin overview sample must not scan the whole on-disk directory either. Its
        // contiguous ids let us seek the exact same sample directly, with bounded work.
        if extent.is_none() && step >= 16 {
            let mut stmt = db.prepare("SELECT offset, len FROM records WHERE id=?1").map_err(sql)?;
            for id in (0..matched).step_by(step as usize).take(limit) {
                let place = stmt.query_row([id as i64], |r| Ok(Place { offset: r.get(0)?, len: r.get(1)? })).map_err(sql)?;
                visit(id as u32, place)?;
            }
            return Ok((matched, false));
        }
        // Record ids are contiguous and zero based. Apply the overview sample in SQL rather
        // than transferring every skipped record through the SQLite/Rust boundary.
        let query = if extent.is_none() {
            format!("SELECT id, offset, len FROM records WHERE id % {step}=0 ORDER BY id LIMIT {limit}")
        } else {
            format!("SELECT id, offset, len FROM {from} ORDER BY id")
        };
        let mut stmt = db.prepare(&query).map_err(sql)?;
        let mut rows = stmt.query(rusqlite::params_from_iter(&args)).map_err(sql)?;
        let (mut ordinal, mut kept) = (0u64, 0usize);
        while let Some(r) = rows.next().map_err(sql)? {
            if extent.is_none() || ordinal.is_multiple_of(step) {
                visit(r.get(0).map_err(sql)?, Place { offset: r.get(1).map_err(sql)?, len: r.get(2).map_err(sql)? })?;
                kept += 1;
                if kept >= limit {
                    break;
                }
            }
            ordinal += 1;
        }
        Ok((matched, step == 1))
    }
}

/// Keep read-only deployments working when neither cache location is writable.
/// The fallback scans record headers in constant space; it never changes source files.
pub(super) struct Index {
    storage: Storage,
    pub count: u64,
    /// The union of the records' usable boxes (rounded outward to f32): `None` when no record has
    /// one (every shape null or empty, or every box not one even from its points: [`not_a_box`]).
    pub bounds: Option<[f32; 4]>,
    /// Records with no usable box, stored or from their points ([`not_a_box`],
    /// [`box_from_points`]): left out of `bounds` and of every read by area (the R-tree and the
    /// scan alike), and said.
    pub unboxed: u32,
}

/// Whether `b` is a record's box that isn't one: a record there (not a null or empty shape, whose
/// box is NaN), but a corner that isn't a number or is past f32's range, a box that's backwards,
/// or a record that can't be read (`record_box`).
fn not_a_box(b: [f32; 4]) -> bool {
    !usable(b) && !b.iter().all(|v| v.is_nan())
}

/// Whether `b` is a box: its corners numbers, in order.
fn usable(b: [f32; 4]) -> bool {
    let [x0, y0, x1, y1] = b;
    b.iter().all(|v| v.is_finite()) && x0 <= x1 && y0 <= y1
}

/// `b` added to `all` when it's a box ([`usable`]); whether it was. The index's R-tree and its
/// `bounds` take the same records by it, so a covered index's bounds frame every record.
fn union(all: &mut Option<[f32; 4]>, b: [f32; 4]) -> bool {
    if !usable(b) {
        return false;
    }
    let [x0, y0, x1, y1] = b;
    *all = Some(match *all {
        Some([a, b, x, y]) => [a.min(x0), b.min(y0), x.max(x1), y.max(y1)],
        None => [x0, y0, x1, y1],
    });
    true
}

/// What scanning the records' headers gathers, as a disk index holds it, for when none can be
/// written.
pub(super) struct Scanned {
    pub count: u32,
    /// A note on a truncated file; empty when there's none.
    pub note: String,
    pub bounds: Option<[f32; 4]>,
    pub unboxed: u32,
}

/// The records counted by scanning their headers, their boxes' union, and the shapes whose box
/// isn't one.
pub(super) fn scanned(path: &Path, len: u64) -> Result<Scanned> {
    let (mut bounds, mut unboxed) = (None, 0u32);
    let (count, note) = scan_boxed(path, len, |_, _, b| {
        if !union(&mut bounds, b) && not_a_box(b) {
            unboxed += 1;
        }
        Ok(())
    })?;
    Ok(Scanned { count, note, bounds, unboxed })
}
enum Storage {
    Disk(DiskIndex),
    Scan { path: std::path::PathBuf, len: u64 },
}
impl Index {
    pub fn open(path: &Path, len: u64, notes: &mut Vec<String>) -> Result<Self> {
        match DiskIndex::open(path, len, notes) {
            Ok(disk) => Ok(Self { count: disk.count, bounds: disk.bounds, unboxed: disk.unboxed, storage: Storage::Disk(disk) }),
            // Canceled while it was built: the run stops, not a scan in its place.
            Err(e @ crate::DataError::Cancelled) => Err(e),
            Err(error) => {
                notes.push(format!("spatial index unavailable ({error}); reading the .shp by scanning record headers"));
                Self::scanning(path, len, notes)
            }
        }
    }
    /// The index without a cache: the records' headers scanned now, and again for each read.
    fn scanning(path: &Path, len: u64, notes: &mut Vec<String>) -> Result<Self> {
        let Scanned { count, note, bounds, unboxed } = scanned(path, len)?;
        if !note.is_empty() {
            notes.push(note);
        }
        Ok(Self { count: u64::from(count), bounds, unboxed, storage: Storage::Scan { path: path.to_path_buf(), len } })
    }
    /// Complete file-order traversal without holding the shared interaction connection.
    pub fn visit_complete(&self, extent: Option<[f32; 4]>, visit: impl FnMut(u32, Place) -> Result<()>) -> Result<(u64, bool)> {
        match &self.storage {
            Storage::Disk(disk) => disk.visit_complete(extent, visit),
            Storage::Scan { .. } => self.visit(extent, u32::MAX as usize, visit),
        }
    }
    pub fn indexed(&self) -> bool {
        matches!(self.storage, Storage::Disk(_))
    }
    pub fn visit(&self, extent: Option<[f32; 4]>, limit: usize, mut visit: impl FnMut(u32, Place) -> Result<()>) -> Result<(u64, bool)> {
        match &self.storage {
            Storage::Disk(disk) => disk.visit(extent, limit, visit),
            Storage::Scan { path, len } => {
                // By area, only a record whose box is one ([`usable`], after [`scan_boxed`] has put
                // a backwards box right from its points), as the disk index's R-tree takes it.
                let matches = |b: [f32; 4]| {
                    extent.is_none_or(|[x0, y0, x1, y1]| {
                        let [a, by, x, y] = b;
                        usable(b) && a <= x1 && x >= x0 && by <= y1 && y >= y0
                    })
                };
                let mut matched = 0u64;
                scan_for(path, *len, extent.is_some(), |_, _, b| {
                    matched += u64::from(matches(b));
                    Ok(())
                })?;
                let step = crate::source::stride(matched, limit);
                let mut ordinal = 0u64;
                let mut kept = 0usize;
                scan_for(path, *len, extent.is_some(), |id, place, b| {
                    if matches(b) {
                        if ordinal.is_multiple_of(step) && kept < limit {
                            visit(id, place)?;
                            kept += 1;
                        }
                        ordinal += 1;
                    }
                    Ok(())
                })?;
                Ok((matched, step == 1))
            }
        }
    }
}

/// [`scan_boxed`] for a read by area; plain [`scan`] for a whole read, where no box is looked at,
/// so no record is read for its points.
fn scan_for(path: &Path, len: u64, by_area: bool, visit: impl FnMut(u32, Place, [f32; 4]) -> Result<()>) -> Result<(u32, String)> {
    if by_area { scan_boxed(path, len, visit) } else { scan(path, len, visit) }
}

/// [`scan`], a record whose stored box is backwards (numbers, but out of order: a writer's slip)
/// given the box of its own points instead where they read and are all numbers
/// ([`box_from_points`]): what every path (the disk index, its bounds, a scan by area) takes a
/// record's box to be, so such a shape is still framed and found. Only a backwards box is read
/// for: a NaN or infinite stored box is taken as it is, counted (most come from points that aren't
/// numbers, as #1985's files have by the thousand, and reading each would cost a whole record),
/// and a record that can't be read, or a point (whose box is the point), has nothing more to give.
fn scan_boxed(path: &Path, len: u64, mut visit: impl FnMut(u32, Place, [f32; 4]) -> Result<()>) -> Result<(u32, String)> {
    let (mut shp, mut buf) = (None, Vec::new());
    scan(path, len, |id, place, b| {
        let backwards = b.iter().all(|v| v.is_finite()) && !usable(b);
        let b = if backwards {
            // A whole record read: Cancel is looked at for each, as there may be many.
            crate::progress::check()?;
            let framed = box_from_points(&mut shp, &mut buf, path, place)?;
            // A huge record's buffer isn't kept for the rest of the scan.
            if buf.capacity() > 1 << 20 {
                buf = Vec::new();
            }
            framed.unwrap_or(b)
        } else {
            b
        };
        visit(id, place, b)
    })
}

/// The box of a record's own points, rounded outward as the stored ones are: `None` when it doesn't
/// parse, or a point isn't a number (a read leaves it out then too); an error when the file won't
/// open or read. Read through `shp`, opened on first use as few records need it, into `buf`, kept
/// from record to record.
fn box_from_points(shp: &mut Option<super::Cursor>, buf: &mut Vec<u8>, path: &Path, place: Place) -> Result<Option<[f32; 4]>> {
    if u64::from(place.len) > super::MAX_RECORD {
        return Ok(None);
    }
    // A file that won't open or read again is an error, not a record without a box: the index
    // isn't built (nor kept) on it.
    let cursor = match shp {
        Some(c) => c,
        None => shp.insert(super::Cursor::open(path)?),
    };
    buf.resize(place.len as usize, 0);
    cursor.read_at(u64::from(place.offset), buf)?;
    let Ok(Some(g)) = super::parse_shape(buf) else { return Ok(None) };
    if !g.is_finite() {
        return Ok(None);
    }
    let e = g.extent();
    // Read, but no points in any part (as part starts can leave it): an empty shape, as reads
    // give it, with nothing to frame (NaN, not counted as left out), not a box that's wrong.
    if e.is_empty() {
        return Ok(Some([f32::NAN; 4]));
    }
    let b = [super::f32_down(e.min_x), super::f32_down(e.min_y), super::f32_up(e.max_x), super::f32_up(e.max_y)];
    Ok(usable(b).then_some(b))
}

fn scan(path: &Path, len: u64, visit: impl FnMut(u32, Place, [f32; 4]) -> Result<()>) -> Result<(u32, String)> {
    scan_from(BufReader::with_capacity(BUFFER, crate::provenance::open(path)?), len, visit)
}

/// [`scan`] of a reader `len` bytes long. A file that ends before then (cut, or changed since its
/// length was taken) keeps the records read before it, with a note saying from which record the
/// rest are left out; any other error reading it is an error, so no index is built (nor kept) on
/// what was read before it.
fn scan_from<R: Read + std::io::Seek>(
    mut f: BufReader<R>,
    len: u64,
    mut visit: impl FnMut(u32, Place, [f32; 4]) -> Result<()>,
) -> Result<(u32, String)> {
    f.seek_relative(100)?;
    let mut pos = 100u64;
    let mut count = 0u32;
    // A record's header (8 bytes), its shape type and box, and its counts of parts and points, with
    // its first two parts' starts; `starts`, the content with the rest of them, for a line or polygon
    // of more.
    let mut head = [0u8; 60];
    let mut starts: Vec<u8> = Vec::new();
    let mut note = String::new();
    while pos + 8 <= len {
        // Cancel is looked at every so many records, on every path (an index built, a scan read).
        if count.is_multiple_of(4096) {
            crate::progress::check()?;
        }
        let want = (len - pos).min(head.len() as u64) as usize;
        // The file ending early (cut, or changed since its length was taken) leaves out the rest,
        // said; another error is one.
        let unread = |e: std::io::Error, n: u32| -> Result<String> {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                Ok(format!("the .shp couldn't be read from record {n}; the rest are left out"))
            } else {
                Err(e.into())
            }
        };
        let Some(buf) = head.get_mut(..want) else { break };
        if let Err(e) = f.read_exact(buf) {
            note = unread(e, count + 1)?;
            break;
        }
        let bytes = u64::try_from(be_i32(&head, 4).unwrap_or(-1)).unwrap_or(u64::MAX).saturating_mul(2);
        let content = pos + 8;
        if bytes < 4 || content.saturating_add(bytes) > len {
            note = format!("the .shp ends partway through record {}; the rest are left out", count + 1);
            break;
        }
        let c = head.get(8..(want as u64).min(8 + bytes) as usize).unwrap_or(&[]);
        // A line or polygon of more than two parts: the rest of its parts' starts, which the record
        // has room for, read on (each 4 bytes, against 16 for each point) so they're checked too.
        // Counts past what a read takes (MAX_PARTS), or a record too short for its points too,
        // aren't read for: the record is refused anyway.
        let more = match (le_i32(c, 0), le_i32(c, 36), le_i32(c, 40)) {
            (Some(3 | 5 | 13 | 15 | 23 | 25), Some(parts), Some(points))
                if parts > 2
                    && points >= 0
                    && parts as usize <= super::MAX_PARTS
                    && points as usize <= super::MAX_PARTS
                    && 44 + 4 * u64::from(parts.unsigned_abs()) + 16 * u64::from(points.unsigned_abs()) <= bytes =>
            {
                4 * (parts.unsigned_abs() as usize - 2)
            }
            _ => 0,
        };
        let b = if more > 0 {
            starts.clear();
            starts.extend_from_slice(c);
            starts.resize(c.len() + more, 0);
            if let Err(e) = f.read_exact(starts.get_mut(c.len()..).unwrap_or(&mut [])) {
                note = unread(e, count + 1)?;
                break;
            }
            let b = record_box(&starts, bytes);
            // A huge array isn't kept for the rest of the scan.
            if starts.capacity() > 1 << 20 {
                starts = Vec::new();
            }
            b
        } else {
            record_box(c, bytes)
        };
        visit(count, Place { offset: content as u32, len: bytes as u32 }, b)?;
        count += 1;
        pos = content + bytes;
        let read = (want as u64).saturating_sub(8) + more as u64;
        // A seek that fails is no file ending early: an error.
        f.seek_relative(bytes as i64 - read as i64)?;
    }
    Ok((count, note))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Coord, Dataset, Geometry, GeometryType};

    /// Scanning record headers, when no index can be written, gathers the records' boxes as an
    /// index does (`union`): a NaN shape's box is left out, the others framed (#1985).
    #[test]
    fn scanning_gathers_the_records_boxes_as_an_index_does() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("pts.shp");
        let mut ds = Dataset::new("pts", GeometryType::Point, vec![]);
        for (x, y) in [(10.0, 20.0), (11.0, 21.0), (12.0, 22.0)] {
            ds.push(Some(Geometry::Point(Coord::new(x, y))), vec![]);
        }
        crate::io::write_path(&ds, &path).unwrap();
        // The first record's point x, at 112.
        let mut b = std::fs::read(&path).unwrap();
        b[112..120].copy_from_slice(&f64::NAN.to_le_bytes());
        std::fs::write(&path, &b).unwrap();
        let s = scanned(&path, b.len() as u64).unwrap();
        assert_eq!((s.count, s.note.as_str()), (3, ""));
        assert_eq!(s.bounds, Some([11.0, 21.0, 12.0, 22.0]));
        assert_eq!(s.unboxed, 1, "the NaN shape is counted as left out");
        assert!(not_a_box([f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY]) && !not_a_box([f32::NAN; 4]));
        assert!(usable([0.0, 0.0, 1.0, 1.0]));
        assert!(!usable([f32::NAN, 0.0, 1.0, 1.0]) && !usable([2.0, 0.0, 1.0, 1.0]) && !usable([0.0, 0.0, f32::INFINITY, 1.0]));
    }

    /// A record whose stored box is wrong (backwards) over good points is framed by its points on
    /// every path: in the bounds, not counted, and found by a read by area through the disk index
    /// and through a scan without one (built here directly) alike.
    #[test]
    fn a_wrong_stored_box_is_framed_by_its_points_on_every_path() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("multi.shp");
        let mut ds = Dataset::new("pts", GeometryType::Point, vec![]);
        for (x, y) in [(10.0, 20.0), (12.0, 22.0)] {
            ds.push(Some(Geometry::MultiPoint(vec![Coord::new(x, y), Coord::new(x + 0.5, y + 0.5)])), vec![]);
        }
        crate::io::write_path(&ds, &path).unwrap();
        // The first record's stored box (at 112) made backwards: min x 10.5 past max x 10.
        let mut b = std::fs::read(&path).unwrap();
        assert_eq!(i32::from_le_bytes(b[108..112].try_into().unwrap()), 8, "a multipoint file");
        for (k, v) in [10.5f64, 20.0, 10.0, 20.5].iter().enumerate() {
            b[112 + 8 * k..120 + 8 * k].copy_from_slice(&v.to_le_bytes());
        }
        std::fs::write(&path, &b).unwrap();
        let len = b.len() as u64;
        let whole = Some([10.0, 20.0, 12.5, 22.5]);
        let s = scanned(&path, len).unwrap();
        assert_eq!((s.bounds, s.unboxed), (whole, 0));
        // Around the first record only.
        let area = Some([9.0f32, 19.0, 11.0, 21.0]);
        let found = |index: &Index| {
            let mut ids = Vec::new();
            index
                .visit(area, usize::MAX, |id, _| {
                    ids.push(id);
                    Ok(())
                })
                .unwrap();
            ids
        };
        let mut notes = Vec::new();
        let scan = Index::scanning(&path, len, &mut notes).unwrap();
        assert!(!scan.indexed());
        assert_eq!((scan.bounds, scan.unboxed), (whole, 0));
        assert_eq!(found(&scan), [0], "the scan finds it by area");
        super::super::cache::with_index_dir(tmp.path().join("cache"), || {
            let disk = Index::open(&path, len, &mut notes).unwrap();
            assert!(disk.indexed());
            assert_eq!((disk.bounds, disk.unboxed), (whole, 0));
            assert_eq!(found(&disk), [0], "the index finds it by area");
        });
        // A backwards box over a point that isn't a number (the second record: its box at 192, its
        // first point's x at 228) can't be put right: left out, and counted, on both paths.
        let bad = tmp.path().join("bad.shp");
        for ext in ["shx", "dbf"] {
            std::fs::copy(path.with_extension(ext), bad.with_extension(ext)).unwrap();
        }
        for (k, v) in [12.5f64, 22.0, 12.0, 22.5].iter().enumerate() {
            b[192 + 8 * k..200 + 8 * k].copy_from_slice(&v.to_le_bytes());
        }
        b[228..236].copy_from_slice(&f64::NAN.to_le_bytes());
        std::fs::write(&bad, &b).unwrap();
        let first = Some([10.0, 20.0, 10.5, 20.5]);
        let scan = Index::scanning(&bad, len, &mut notes).unwrap();
        assert_eq!((scan.bounds, scan.unboxed), (first, 1));
        super::super::cache::with_index_dir(tmp.path().join("cache"), || {
            let disk = Index::open(&bad, len, &mut notes).unwrap();
            assert_eq!((disk.bounds, disk.unboxed), (first, 1));
        });
    }

    /// A backwards box over a line whose part starts leave it no points (it reads as an empty
    /// line) has nothing to frame: not counted as left out, as reads give it, on both paths.
    #[test]
    fn a_backwards_box_over_a_shape_of_no_points_is_nothing_to_frame() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("lines.shp");
        let mut ds = Dataset::new("lines", GeometryType::Polyline, vec![]);
        for (x, y) in [(10.0, 20.0), (12.0, 22.0)] {
            ds.push(Some(Geometry::Line(vec![vec![Coord::new(x, y), Coord::new(x + 0.5, y + 0.5)]])), vec![]);
        }
        crate::io::write_path(&ds, &path).unwrap();
        // The first record: its box (at 112) backwards, its one part (counts at 144 and 148)
        // starting at 2, past its 2 points (the part's start at 152).
        let mut b = std::fs::read(&path).unwrap();
        assert_eq!(i32::from_le_bytes(b[108..112].try_into().unwrap()), 3, "a line file");
        assert_eq!(i32::from_le_bytes(b[144..148].try_into().unwrap()), 1, "one part");
        assert_eq!(i32::from_le_bytes(b[148..152].try_into().unwrap()), 2, "two points");
        for (k, v) in [10.5f64, 20.0, 10.0, 20.5].iter().enumerate() {
            b[112 + 8 * k..120 + 8 * k].copy_from_slice(&v.to_le_bytes());
        }
        b[152..156].copy_from_slice(&2i32.to_le_bytes());
        std::fs::write(&path, &b).unwrap();
        let len = b.len() as u64;
        let second = Some([12.0, 22.0, 12.5, 22.5]);
        let mut notes = Vec::new();
        let scan = Index::scanning(&path, len, &mut notes).unwrap();
        assert_eq!((scan.bounds, scan.unboxed), (second, 0));
        super::super::cache::with_index_dir(tmp.path().join("cache"), || {
            let disk = Index::open(&path, len, &mut notes).unwrap();
            assert_eq!((disk.bounds, disk.unboxed), (second, 0));
        });
    }

    /// A line of more than two parts has the rest of its parts' starts read on and checked: one
    /// that starts past its points frames nothing, counted, and the record after it is still read
    /// in its place, on both paths; a clean file of the same shapes frames them all.
    #[test]
    fn every_parts_start_of_a_record_is_checked() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("parts.shp");
        let mut ds = Dataset::new("lines", GeometryType::Polyline, vec![]);
        let part = |x: f64, y: f64| vec![Coord::new(x, y), Coord::new(x + 0.5, y + 0.5)];
        ds.push(Some(Geometry::Line(vec![part(10.0, 20.0), part(11.0, 21.0), part(12.0, 22.0)])), vec![]);
        ds.push(Some(Geometry::Line(vec![part(30.0, 40.0)])), vec![]);
        crate::io::write_path(&ds, &path).unwrap();
        let mut b = std::fs::read(&path).unwrap();
        let len = b.len() as u64;
        let mut notes = Vec::new();
        let both = |p: &Path, notes: &mut Vec<String>, cache: &str| {
            let scan = Index::scanning(p, len, notes).unwrap();
            let disk = super::super::cache::with_index_dir(tmp.path().join(cache), || Index::open(p, len, notes).unwrap());
            assert!(disk.indexed());
            [(scan.count, scan.bounds, scan.unboxed), (disk.count, disk.bounds, disk.unboxed)]
        };
        for got in both(&path, &mut notes, "clean") {
            assert_eq!(got, (2, Some([10.0, 20.0, 30.5, 40.5]), 0), "clean");
        }
        // The first record: 3 parts (count at 144) of 6 points, its third part's start (at 160) 7.
        assert_eq!(i32::from_le_bytes(b[144..148].try_into().unwrap()), 3, "three parts");
        assert_eq!(i32::from_le_bytes(b[160..164].try_into().unwrap()), 4, "the third part's start");
        b[160..164].copy_from_slice(&7i32.to_le_bytes());
        let bad = tmp.path().join("bad.shp");
        for ext in ["shx", "dbf"] {
            std::fs::copy(path.with_extension(ext), bad.with_extension(ext)).unwrap();
        }
        std::fs::write(&bad, &b).unwrap();
        for got in both(&bad, &mut notes, "bad") {
            assert_eq!(got, (2, Some([30.0, 40.0, 30.5, 40.5]), 1), "a start past the points");
        }
    }

    /// A file that can't be read as far as its length said (cut, or changed, after the length was
    /// taken) keeps the records read before, and says the rest are left out, from which record.
    #[test]
    fn a_read_that_fails_partway_says_what_it_left_out() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("pts.shp");
        let mut ds = Dataset::new("pts", GeometryType::Point, vec![]);
        for i in 0..5 {
            ds.push(Some(Geometry::Point(Coord::new(10.0 + f64::from(i), 20.0))), vec![]);
        }
        crate::io::write_path(&ds, &path).unwrap();
        // Records of 28 bytes from 100 to 240, each read 60 bytes ahead: past the real end (taken as
        // 100 bytes further) from the fourth.
        let len = std::fs::metadata(&path).unwrap().len() + 100;
        let mut notes = Vec::new();
        let scan = Index::scanning(&path, len, &mut notes).unwrap();
        assert_eq!(scan.count, 3);
        assert!(notes.iter().any(|n| n.contains("couldn't be read from record 4; the rest are left out")), "{notes:?}");
    }

    /// An error reading the records that isn't the file ending (a network drive gone, EIO) is an
    /// error, not a cut file: nothing is built on what was read before it, nor cached.
    #[test]
    fn an_io_error_partway_is_an_error_not_a_cut_file() {
        use std::io::{Cursor, Seek, SeekFrom};
        struct Failing(Cursor<Vec<u8>>, u64);
        impl Read for Failing {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.0.position() >= self.1 {
                    return Err(std::io::Error::other("the drive went away"));
                }
                let n = buf.len().min((self.1 - self.0.position()) as usize);
                self.0.read(&mut buf[..n])
            }
        }
        impl Seek for Failing {
            fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
                self.0.seek(to)
            }
        }
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("pts.shp");
        let mut ds = Dataset::new("pts", GeometryType::Point, vec![]);
        for i in 0..5 {
            ds.push(Some(Geometry::Point(Coord::new(10.0 + f64::from(i), 20.0))), vec![]);
        }
        crate::io::write_path(&ds, &path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let len = bytes.len() as u64;
        // Failing from the third record on (records of 28 bytes from 100).
        let reader = BufReader::with_capacity(64, Failing(Cursor::new(bytes.clone()), 160));
        let got = scan_from(reader, len, |_, _, _| Ok(()));
        assert!(matches!(got, Err(crate::DataError::Io(_))), "{got:?}");
        // The same bytes, read whole: every record.
        let reader = BufReader::with_capacity(64, Failing(Cursor::new(bytes), u64::MAX));
        assert_eq!(scan_from(reader, len, |_, _, _| Ok(())).unwrap(), (5, String::new()));
    }

    /// Canceled while the records are scanned, an index build stops the open: no scan in its
    /// place, nor a note that the index is unavailable; a scan without one stops too.
    #[test]
    fn cancel_stops_an_index_build_and_a_scan() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("pts.shp");
        let mut ds = Dataset::new("pts", GeometryType::Point, vec![]);
        ds.push(Some(Geometry::Point(Coord::new(10.0, 20.0))), vec![]);
        crate::io::write_path(&ds, &path).unwrap();
        let len = std::fs::metadata(&path).unwrap().len();
        let probe = std::sync::Arc::new(crate::progress::tests::Counted::default());
        probe.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        let _installed = crate::progress::install(Some(probe));
        let mut notes = Vec::new();
        super::super::cache::with_index_dir(tmp.path().join("cache"), || {
            assert!(matches!(Index::open(&path, len, &mut notes), Err(crate::DataError::Cancelled)));
        });
        assert!(notes.is_empty(), "{notes:?}");
        assert!(matches!(Index::scanning(&path, len, &mut notes), Err(crate::DataError::Cancelled)));
    }
}
