//! Disk-backed record directory and R-tree. Neither grows the provider's resident heap.

use super::{BUFFER, Place, bad, be_i32, record_box};
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
        let signature = format!("shp-index-v3:{canonical:?}:{:?}", stamp(path)?);
        let mut dir = cache::index_dir();
        let budget = cache::index_budget();
        let key = format!("{:016x}-{:016x}", fnv1a(format!("shp-index-v3:{canonical:?}").as_bytes()), fnv1a(signature.as_bytes()));
        let pinned = cache::pin_index(&dir, &key, budget).or_else(|_| {
            dir = cache::index_fallback_dir();
            cache::pin_index(&dir, &key, budget)
        });
        if let Ok(Some(lease)) = pinned
            && let Ok(db) = connect(lease.path(), false)
            && let Ok((saved, count, note, covered, bounds)) =
                db.query_row("SELECT signature, count, note, covered, bounds FROM metadata", [], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, u32>(1)?, r.get::<_, String>(2)?, r.get::<_, u32>(3)?, r.get::<_, String>(4)?))
                })
            && saved == signature
            && covered <= count
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
            return Ok(Self { db: Mutex::new(db), count: u64::from(count), covered, bounds, _lease: lease });
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
            CREATE TABLE metadata(signature TEXT, count INTEGER, note TEXT, covered INTEGER, bounds TEXT);",
        )
        .map_err(sql)?;
        let tx = db.transaction().map_err(sql)?;
        let mut covered = 0u32;
        let mut all_bounds: Option<[f32; 4]> = None;
        let (count, note) = {
            let mut record = tx.prepare("INSERT INTO records VALUES(?1,?2,?3)").map_err(sql)?;
            let mut bounds = tx.prepare("INSERT INTO bounds VALUES(?1,?2,?3,?4,?5)").map_err(sql)?;
            scan(path, len, |id, place, b| {
                record.execute(params![id, place.offset, place.len]).map_err(sql)?;
                if union(&mut all_bounds, b) {
                    let [x0, y0, x1, y1] = b;
                    bounds.execute(params![id, x0, x1, y0, y1]).map_err(sql)?;
                    covered += 1;
                }
                Ok(())
            })?
        };
        // Do not retain an index built while the original file was being replaced/edited.
        if signature != format!("shp-index-v3:{canonical:?}:{:?}", stamp(path)?) {
            return Err(bad("the .shp changed while its spatial index was being built"));
        }
        let bounds_json = serde_json::to_string(&all_bounds).map_err(|e| bad(e.to_string()))?;
        tx.execute("INSERT INTO metadata VALUES(?1,?2,?3,?4,?5)", params![signature, count, note, covered, bounds_json]).map_err(sql)?;
        tx.commit().map_err(sql)?;
        drop(db);
        lease.commit()?;
        let db = connect(lease.path(), false)?;
        if !note.is_empty() {
            notes.push(note);
        }
        Ok(Self { db: Mutex::new(db), count: u64::from(count), covered, bounds: all_bounds, _lease: lease })
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
    /// The union of the records' finite boxes (rounded outward to f32): `None` when no record has
    /// one (every shape null or empty, or a coordinate that isn't a number).
    pub bounds: Option<[f32; 4]>,
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

/// The records counted by scanning their headers, with any note on a truncated file and their
/// boxes' union: what a disk index holds, for when none can be written.
pub(super) fn scanned(path: &Path, len: u64) -> Result<(u32, String, Option<[f32; 4]>)> {
    let mut bounds = None;
    let (count, note) = scan(path, len, |_, _, b| {
        union(&mut bounds, b);
        Ok(())
    })?;
    Ok((count, note, bounds))
}
enum Storage {
    Disk(DiskIndex),
    Scan { path: std::path::PathBuf, len: u64 },
}
impl Index {
    pub fn open(path: &Path, len: u64, notes: &mut Vec<String>) -> Result<Self> {
        match DiskIndex::open(path, len, notes) {
            Ok(disk) => Ok(Self { count: disk.count, bounds: disk.bounds, storage: Storage::Disk(disk) }),
            Err(error) => {
                let (count, note, bounds) = scanned(path, len)?;
                notes.push(format!("spatial index unavailable ({error}); reading the .shp by scanning record headers"));
                if !note.is_empty() {
                    notes.push(note);
                }
                Ok(Self { count: u64::from(count), bounds, storage: Storage::Scan { path: path.to_path_buf(), len } })
            }
        }
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
                let matches = |b: [f32; 4]| {
                    extent.is_none_or(|[x0, y0, x1, y1]| {
                        let [a, b, x, y] = b;
                        a <= x1 && x >= x0 && b <= y1 && y >= y0
                    })
                };
                let mut matched = 0u64;
                scan(path, *len, |_, _, b| {
                    matched += u64::from(matches(b));
                    Ok(())
                })?;
                let step = crate::source::stride(matched, limit);
                let mut ordinal = 0u64;
                let mut kept = 0usize;
                scan(path, *len, |id, place, b| {
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

fn scan(path: &Path, len: u64, mut visit: impl FnMut(u32, Place, [f32; 4]) -> Result<()>) -> Result<(u32, String)> {
    let mut f = BufReader::with_capacity(BUFFER, crate::provenance::open(path)?);
    f.seek_relative(100)?;
    let mut pos = 100u64;
    let mut count = 0u32;
    // A record's header, its shape type and box, and its counts of parts and points.
    let mut head = [0u8; 52];
    let mut note = String::new();
    while pos + 8 <= len {
        let want = (len - pos).min(head.len() as u64) as usize;
        let Some(buf) = head.get_mut(..want) else { break };
        if f.read_exact(buf).is_err() {
            break;
        }
        let bytes = u64::try_from(be_i32(&head, 4).unwrap_or(-1)).unwrap_or(u64::MAX).saturating_mul(2);
        let content = pos + 8;
        if bytes < 4 || content.saturating_add(bytes) > len {
            note = format!("the .shp ends partway through record {}; the rest are left out", count + 1);
            break;
        }
        let b = record_box(head.get(8..(want as u64).min(8 + bytes) as usize).unwrap_or(&[]));
        visit(count, Place { offset: content as u32, len: bytes as u32 }, b)?;
        count += 1;
        pos = content + bytes;
        if f.seek_relative(bytes as i64 - (want as u64).saturating_sub(8) as i64).is_err() {
            break;
        }
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
        let (count, note, bounds) = scanned(&path, b.len() as u64).unwrap();
        assert_eq!((count, note.as_str()), (3, ""));
        assert_eq!(bounds, Some([11.0, 21.0, 12.0, 22.0]));
        assert!(usable([0.0, 0.0, 1.0, 1.0]));
        assert!(!usable([f32::NAN, 0.0, 1.0, 1.0]) && !usable([2.0, 0.0, 1.0, 1.0]) && !usable([0.0, 0.0, f32::INFINITY, 1.0]));
    }
}
