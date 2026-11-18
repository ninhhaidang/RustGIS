use super::*;
use crate::Dataset;
use crate::source::open_path;

fn dir() -> PathBuf {
    static SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "rustgis-shp-index-test-{}-{}",
        std::process::id(),
        SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&path).unwrap();
    path
}
fn write_shapefile(ds: &Dataset, path: &Path) {
    crate::io::write_path(ds, path).unwrap();
}
/// A streamed Shapefile is framed by its records' own boxes (#1985): the header's box stands for
/// them only when it is their box (the specification's minimum bounding rectangle), exact at any
/// size. One with a corner that isn't a number (an infinity, a NaN), too small for the shapes
/// (stale), or inverted is set aside with a note, so a NaN shape doesn't lose the layer its
/// extent, nor does a false "not longitude/latitude" note come with it; one only larger than the
/// shapes (editors that only grow it, or junk) is set aside without one. A shape whose box isn't
/// one is counted, said, also from a cached index. A file whose records have no finite box (every
/// shape null), or no records at all, has no extent rather than Null Island; the null one says so
/// when its header claimed a box. A clean header is used as it is, without a note. A cached index
/// whose stored box isn't one is built again, not believed.
#[test]
fn a_streamed_shapefile_is_framed_by_its_records() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().to_path_buf();
    let write = |name: &str, points: &[Option<(f64, f64)>]| {
        let mut ds = Dataset::new("pts", GeometryType::Point, vec![]);
        for p in points {
            ds.push(p.map(|(x, y)| Geometry::Point(Coord::new(x, y))), vec![]);
        }
        let path = folder.join(name);
        write_shapefile(&ds, &path);
        path
    };
    // Little-endian f64s: the header's box at 36, 44, 52, 60; the first record's point x at 112.
    let patch = |path: &Path, at: usize, v: f64| {
        let mut b = std::fs::read(path).unwrap();
        b[at..at + 8].copy_from_slice(&v.to_le_bytes());
        std::fs::write(path, b).unwrap();
    };
    let three = [Some((10.0, 20.0)), Some((11.0, 21.0)), Some((12.0, 22.0))];
    let whole = Extent::new(10.0, 20.0, 12.0, 22.0);
    let has = |s: &ShpSource, what: &str| s.info().notes.iter().any(|n| n.contains(what));
    const SET_ASIDE: &str = "header's bounding box doesn't hold its shapes";
    const UNBOXED: &str = "have no usable bounding box (a coordinate";
    const NO_SHAPE_BOX: &str = "has no shapes to frame: its header's box isn't used";
    let quiet = |s: &ShpSource| [SET_ASIDE, UNBOXED, NO_SHAPE_BOX, "not longitude/latitude"].iter().all(|w| !has(s, w));
    cache::with_index_dir(folder.join("cache"), || {
        let open = |p: &Path| {
            let s = ShpSource::open_path(p, "pts").unwrap();
            assert!(s.info().indexed);
            s
        };
        let clean = open(&write("clean.shp", &three));
        assert_eq!(clean.info().extent, whole, "a clean header is used as it is");
        assert!(quiet(&clean), "{:?}", clean.info().notes);
        // Coordinates f32 can't hold, the records' boxes rounded outward past them: the header,
        // exact, still matches them, with nothing said, at any size (UTM, a southern northing
        // near 10,000 km where an f32 step is a meter).
        let odd = [Some((10.1, 20.3)), Some((12.7, 22.9))];
        let s = open(&write("odd.shp", &odd));
        assert_eq!(s.info().extent, Extent::new(10.1, 20.3, 12.7, 22.9));
        assert!(!has(&s, SET_ASIDE), "{:?}", s.info().notes);
        let utm = [Some((500_000.123, 4_500_000.789)), Some((512_345.678, 9_999_999.3))];
        let s = open(&write("utm.shp", &utm));
        assert_eq!(s.info().extent.max_y, 9_999_999.3, "the header, exact");
        assert!(!has(&s, SET_ASIDE), "{:?}", s.info().notes);
        // A corner that isn't a number, one too small for the shapes (stale), or an inverted
        // header: the records' boxes, said. One larger than the shapes but holding them (left so
        // by editors that only grow it, or junk): the records' boxes, with nothing to say.
        let corners = [
            (44, f64::INFINITY, true),
            (52, f64::NEG_INFINITY, true),
            (60, f64::NAN, true),
            (52, 11.0, true),
            (36, 13.0, true),
            (36, 5.0, false),
            (60, 1e300, false),
        ];
        for (k, (at, v, said)) in corners.into_iter().enumerate() {
            let p = write(&format!("corner-{k}.shp"), &three);
            patch(&p, at, v);
            let s = open(&p);
            assert_eq!(s.info().extent, whole, "corner at {at} = {v}");
            assert_eq!(has(&s, SET_ASIDE), said, "corner at {at} = {v}: {:?}", s.info().notes);
            assert!(!has(&s, "not longitude/latitude"), "corner at {at} = {v}: {:?}", s.info().notes);
        }
        // A 0, 0, 0, 0 header over shapes (a writer that keeps no box): the shapes' box, and
        // nothing wrong to say.
        let p = write("zero-header.shp", &three);
        for at in [36, 44, 52, 60] {
            patch(&p, at, 0.0);
        }
        let s = open(&p);
        assert_eq!(s.info().extent, whole);
        assert!(quiet(&s), "{:?}", s.info().notes);
        // A NaN shape and the box its writer made from it: the other shapes frame the layer, and
        // the shape left out is counted.
        let p = write("nan-shape.shp", &three);
        patch(&p, 112, f64::NAN);
        patch(&p, 36, f64::NAN);
        let s = open(&p);
        assert_eq!(s.info().extent, Extent::new(11.0, 21.0, 12.0, 22.0));
        assert!(has(&s, SET_ASIDE) && !has(&s, "not longitude/latitude"), "{:?}", s.info().notes);
        assert!(has(&s, &format!("1 shape(s) {UNBOXED}")), "{:?}", s.info().notes);
        // Opened again from its cached index, it says the same: a probe table written into every
        // index there is still there after, so the index was reused, not built again.
        let indexes = || {
            std::fs::read_dir(folder.join("cache"))
                .unwrap()
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "sqlite"))
                .collect::<Vec<_>>()
        };
        drop(s);
        for index in indexes() {
            rusqlite::Connection::open(&index).unwrap().execute_batch("CREATE TABLE reuse_probe(value INTEGER)").unwrap();
        }
        let s = open(&p);
        let probed = |index: &Path| {
            let db = rusqlite::Connection::open(index).unwrap();
            db.query_row("SELECT count(*) FROM sqlite_master WHERE name='reuse_probe'", [], |r| r.get::<_, i64>(0)).unwrap() == 1
        };
        assert!(indexes().iter().all(|i| probed(i)), "the cached index reused");
        assert!(has(&s, &format!("1 shape(s) {UNBOXED}")), "from the cache: {:?}", s.info().notes);
        // Every record null (its shape type, at 108 + 28 per record, set to 0): no extent, not
        // Null Island. Under the 0, 0, 0, 0 box writers give such a file, nothing to say; under a
        // header claiming a box, said.
        let nulled = |name: &str, header: [f64; 4]| {
            let p = write(name, &three);
            let mut b = std::fs::read(&p).unwrap();
            for at in [108, 136, 164] {
                b[at..at + 4].copy_from_slice(&0i32.to_le_bytes());
            }
            for (k, v) in header.iter().enumerate() {
                b[36 + 8 * k..44 + 8 * k].copy_from_slice(&v.to_le_bytes());
            }
            std::fs::write(&p, b).unwrap();
            open(&p)
        };
        let s = nulled("null.shp", [0.0; 4]);
        assert!(s.info().extent.is_empty(), "{:?}", s.info().extent);
        assert!(quiet(&s), "{:?}", s.info().notes);
        let s = nulled("null-boxed.shp", [10.0, 20.0, 12.0, 22.0]);
        assert!(s.info().extent.is_empty(), "{:?}", s.info().extent);
        assert!(has(&s, NO_SHAPE_BOX) && !has(&s, UNBOXED), "{:?}", s.info().notes);
        // Under a header that isn't a box (a corner that isn't a number), said too.
        let s = nulled("null-nan.shp", [10.0, f64::NAN, 12.0, 22.0]);
        assert!(s.info().extent.is_empty(), "{:?}", s.info().extent);
        assert!(has(&s, NO_SHAPE_BOX) && !has(&s, UNBOXED), "{:?}", s.info().notes);
        // A file of no records (its length, at 24, in 16-bit words), under a 0, 0, 0, 0 header box or
        // one left from the records it had.
        for (name, header) in [("empty.shp", [0.0f64; 4]), ("emptied.shp", [10.0, 20.0, 12.0, 22.0])] {
            let p = write(name, &three);
            for file in [p.clone(), p.with_extension("shx")] {
                let mut b = std::fs::read(&file).unwrap();
                b.truncate(100);
                b[24..28].copy_from_slice(&50i32.to_be_bytes());
                for (k, v) in header.iter().enumerate() {
                    b[36 + 8 * k..44 + 8 * k].copy_from_slice(&v.to_le_bytes());
                }
                std::fs::write(&file, b).unwrap();
            }
            let s = open(&p);
            assert_eq!(s.info().count, Some(0));
            assert!(s.info().extent.is_empty(), "{name}: {:?}", s.info().extent);
            // An empty layer loses nothing by its header's box: nothing said, whatever it holds.
            assert!(quiet(&s), "{name}: {:?}", s.info().notes);
        }
    });
    // A cached index whose stored box isn't one (inverted, as damage or a planted file could
    // leave it) is built again.
    let planted = folder.join("planted-cache");
    cache::with_index_dir(planted.clone(), || {
        let p = write("planted.shp", &three);
        patch(&p, 52, f64::NAN);
        drop(ShpSource::open_path(&p, "pts").unwrap());
        let db_path = std::fs::read_dir(&planted)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|e| e == "sqlite"))
            .unwrap();
        // Inverted, or none while its records are covered: either way not the rule it was built by.
        for stored in ["'[12,22,10,20]'", "'null'"] {
            let db = rusqlite::Connection::open(&db_path).unwrap();
            assert_eq!(db.execute(&format!("UPDATE metadata SET bounds = {stored}"), []).unwrap(), 1);
            drop(db);
            let s = ShpSource::open_path(&p, "pts").unwrap();
            assert!(s.info().indexed, "{stored}: built again, not scanned: {:?}", s.info().notes);
            assert_eq!(s.info().extent, whole, "{stored}: {:?}", s.info().extent);
        }
    });
}
/// A record of no points (an empty geometry, which some writers give a 0, 0, 0, 0 box), or too
/// short to say, has no box, so it doesn't stretch the layer's extent to 0, 0 and set aside a
/// header that's right, nor is it counted as a shape left out.
#[test]
fn an_empty_shape_has_no_box() {
    // Shape type, the box, then a polygon's counts of parts and points, or a multipoint's points.
    let record = |shape: i32, counts: &[i32]| {
        let mut raw = shape.to_le_bytes().to_vec();
        for v in [0.0f64, 0.0, 0.0, 0.0] {
            raw.extend_from_slice(&v.to_le_bytes());
        }
        for c in counts {
            raw.extend_from_slice(&c.to_le_bytes());
        }
        raw
    };
    // Nothing to frame (a null or empty shape): NaN, not counted. A record whose box isn't one
    // (unreadable, an unknown type): infinite, counted as left out. A backwards box is kept as
    // stored here; the index puts it right from the points (`index::scan_boxed`).
    let none = |b: [f32; 4]| b.iter().all(|v| v.is_nan());
    let unread = |b: [f32; 4]| b == [f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY];
    // The record as given, its length as long as it is.
    let whole = |c: &[u8]| record_box(c, c.len() as u64);
    assert!(none(whole(&0i32.to_le_bytes())), "a null shape");
    assert!(none(whole(&record(5, &[0, 0]))), "an empty polygon");
    assert!(none(whole(&record(3, &[0, 0]))), "an empty line");
    assert!(none(whole(&record(8, &[0]))), "an empty multipoint");
    assert!(none(record_box(&record(5, &[0, 4]), 108)), "a polygon of no parts");
    assert!(unread(whole(&record(5, &[0, 4]))), "a polygon of no parts, too short for its 4 points");
    assert!(unread(whole(&record(5, &[1, -1]))), "a count that can't be");
    assert!(unread(whole(&record(31, &[1, 4]))), "a multipatch, which isn't read");
    assert!(unread(whole(&record(5, &[]))), "a polygon too short for its counts");
    assert!(unread(whole(&record(5, &[1]))), "a polygon too short for its points' count");
    assert!(unread(whole(&record(8, &[]))), "a multipoint too short for its count");
    assert!(unread(whole(&record(99, &[1, 4]))), "a shape type that isn't one");
    assert!(unread(whole(&1i32.to_le_bytes())), "a point too short for its coordinates");
    // Counts that say more points than the record holds: a read refuses it, so it frames nothing.
    assert!(unread(whole(&record(8, &[1]))), "a multipoint of one point and no coordinates");
    assert!(unread(record_box(&record(5, &[1, 4]), 111)), "a polygon a byte short of its 4 points");
    assert!(unread(record_box(&record(8, &[2]), 71)), "a multipoint a byte short of its 2 points");
    assert!(unread(record_box(&record(8, &[1 << 25]), u64::MAX)), "more points than a shape may have");
    // A point's record shorter than its x and y, though its bytes run on into the next record.
    let mut point = 1i32.to_le_bytes().to_vec();
    point.extend_from_slice(&[0; 16]);
    assert!(unread(record_box(&point, 19)), "a point a byte short of its coordinates");
    assert_eq!(record_box(&point, 20), [0.0; 4], "a point with its coordinates");
    // Long enough for its parts' starts and its points: 44 + 4 + 4 × 16, and 40 + 2 × 16.
    assert_eq!(record_box(&record(5, &[1, 4, 0]), 112), [0.0; 4], "a polygon with points keeps its box");
    // Parts' starts a read refuses (one past the points, past the next part's, below 0, or not
    // there at all): it frames nothing. Starts in order, within the points, keep the box. A line
    // of 1 part and 2 points is 44 + 4 + 32 bytes; of 2 parts and 4 points, 44 + 8 + 64.
    assert!(unread(record_box(&record(3, &[1, 2, 3]), 80)), "a part starting past the points");
    assert!(unread(record_box(&record(3, &[2, 4, 3, 1]), 116)), "a part starting past the next");
    assert!(unread(record_box(&record(3, &[1, 2, -1]), 80)), "a part starting below 0");
    assert!(unread(record_box(&record(3, &[1, 2]), 80)), "a start that isn't there");
    assert_eq!(record_box(&record(3, &[2, 4, 0, 2]), 116), [0.0; 4], "two parts in order");
    assert_eq!(record_box(&record(3, &[2, 4, 0, 4]), 116), [0.0; 4], "a last part of no points still reads");
    // Every part empty (the first starts at the end of the points): read as an empty shape, it has
    // nothing to frame, as a shape of no points hasn't, whatever its stored box.
    assert!(none(record_box(&record(3, &[1, 2, 2]), 80)), "one part starting at the end of its points");
    assert!(none(record_box(&record(5, &[2, 4, 4, 4]), 116)), "two parts, both empty");
    assert_eq!(record_box(&record(8, &[2]), 72), [0.0; 4], "a multipoint with points keeps its box");
    // A box that's backwards (min x 5, max x 1): kept as stored, and not a box where it's taken.
    let mut backwards = 5i32.to_le_bytes().to_vec();
    for v in [5.0f64, 0.0, 1.0, 1.0] {
        backwards.extend_from_slice(&v.to_le_bytes());
    }
    backwards.extend_from_slice(&1i32.to_le_bytes());
    backwards.extend_from_slice(&4i32.to_le_bytes());
    backwards.extend_from_slice(&0i32.to_le_bytes());
    let b = record_box(&backwards, 112);
    assert_eq!(b, [5.0, 0.0, 1.0, 1.0]);
    assert!(b[0] > b[2] && !none(b), "backwards, for the index to put right, not nothing");
    // Backwards by less than an f32 step on both sides: rounded outward each side would come out
    // in order, and be taken as a box; it stays backwards.
    let mut slight = 5i32.to_le_bytes().to_vec();
    for v in [1.000_000_02f64, 2.000_000_04, 1.000_000_01, 2.000_000_02] {
        slight.extend_from_slice(&v.to_le_bytes());
    }
    slight.extend_from_slice(&1i32.to_le_bytes());
    slight.extend_from_slice(&4i32.to_le_bytes());
    slight.extend_from_slice(&0i32.to_le_bytes());
    let [x0, y0, x1, y1] = record_box(&slight, 112);
    assert!(x0 > x1 && y0 > y1, "still backwards: {x0} > {x1}, {y0} > {y1}");
    // A whole file: a multipoint emptied (its box, at 112, made 0, 0, 0, 0 and its count of
    // points, at 144, 0) under a header that's the others' box. The header stands, with nothing
    // said, as the index and a scan of the records both read it.
    let tmp = tempfile::tempdir().unwrap();
    let mut ds = Dataset::new("pts", GeometryType::Point, vec![]);
    for (x, y) in [(10.0, 20.0), (12.0, 22.0)] {
        ds.push(Some(Geometry::MultiPoint(vec![Coord::new(x, y), Coord::new(x + 0.5, y + 0.5)])), vec![]);
    }
    let path = tmp.path().join("multi.shp");
    write_shapefile(&ds, &path);
    let mut b = std::fs::read(&path).unwrap();
    assert_eq!(i32::from_le_bytes(b[108..112].try_into().unwrap()), 8, "a multipoint file");
    b[112..144].fill(0);
    b[144..148].copy_from_slice(&0i32.to_le_bytes());
    for (k, v) in [12.0f64, 22.0, 12.5, 22.5].iter().enumerate() {
        b[36 + 8 * k..44 + 8 * k].copy_from_slice(&v.to_le_bytes());
    }
    std::fs::write(&path, &b).unwrap();
    let scanned = index::scanned(&path, b.len() as u64).unwrap();
    assert_eq!((scanned.count, scanned.bounds, scanned.unboxed), (2, Some([12.0, 22.0, 12.5, 22.5]), 0));
    cache::with_index_dir(tmp.path().join("cache"), || {
        let s = ShpSource::open_path(&path, "pts").unwrap();
        assert!(s.info().indexed);
        assert_eq!(s.info().extent, Extent::new(12.0, 22.0, 12.5, 22.5), "the header, as written");
        assert!(!s.info().notes.iter().any(|n| n.contains("bounding box")), "{:?}", s.info().notes);
    });
}
fn points_shapefile(path: &Path) {
    let mut ds = Dataset::new("grid", GeometryType::Point, vec![]);
    for x in 0..30 {
        for y in 0..20 {
            ds.push(Some(Geometry::Point(Coord::new(x as f64, y as f64))), vec![]);
        }
    }
    write_shapefile(&ds, path);
}
#[test]
fn shapefile_disk_index_reopens_and_invalidates_without_changing_source() {
    let fixture = dir();
    cache::with_index_dir(fixture.join("cache"), || {
        let p = fixture.join("indexed-grid.shp");
        points_shapefile(&p);
        let original = std::fs::read(&p).unwrap();
        let req = Request { extent: Some(Extent::new(10.0, 10.0, 40.0, 30.0)), limit: 1000 };
        let src = open_path(&p, None).unwrap();
        let expected = src.read(&req).unwrap();
        let reopened = open_path(&p, None).unwrap();
        assert!(reopened.info().indexed);
        assert_eq!(reopened.read(&req).unwrap().features, expected.features);
        assert_eq!(std::fs::read(&p).unwrap(), original);
        drop((src, reopened));
        // Replacing the same path must never reuse stale offsets, boxes or record counts.
        let mut replacement = Dataset::new("new", GeometryType::Point, vec![]);
        replacement.push(Some(Geometry::Point(Coord::new(-75.0, -30.0))), vec![]);
        write_shapefile(&replacement, &p);
        let changed = open_path(&p, None).unwrap();
        assert_eq!(changed.info().count, Some(1));
        assert!(changed.read(&req).unwrap().features.is_empty());
        assert_eq!(changed.read(&Request::all()).unwrap().features.len(), 1);
    });
    std::fs::remove_dir_all(fixture).unwrap();
}

#[test]
fn shapefile_disk_index_keeps_file_order_and_query_boundaries() {
    let fixture = dir();
    cache::with_index_dir(fixture.join("cache"), || {
        let p = fixture.join("indexed-order.shp");
        let mut ds = Dataset::new("ordered", GeometryType::Point, vec![]);
        // Deliberately reverse spatial order. The index must not change painter/attribute order.
        for x in (0..1000).rev() {
            ds.push(Some(Geometry::Point(Coord::new(x as f64 / 100.0, 0.0))), vec![]);
        }
        write_shapefile(&ds, &p);
        // A stale file-header envelope cannot prove that a view contains the whole source.
        let mut bytes = std::fs::read(&p).unwrap();
        for (at, value) in [(36, 1.5f64), (44, 0.0), (52, 1.5), (60, 0.0)] {
            bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
        }
        std::fs::write(&p, bytes).unwrap();
        let src = open_path(&p, None).unwrap();
        let batch = src.read(&Request { extent: Some(Extent::new(1.0, -1.0, 2.0, 1.0)), limit: 1000 }).unwrap();
        assert_eq!(batch.matched, 101);
        assert_eq!(batch.features.len(), 101);
        assert!(batch.complete);
        assert!(batch.features.windows(2).all(|p| p[0].id < p[1].id));
        assert_eq!(batch.features.first().unwrap().geometry, Some(Geometry::Point(Coord::new(2.0, 0.0))));
        assert_eq!(batch.features.last().unwrap().geometry, Some(Geometry::Point(Coord::new(1.0, 0.0))));
        let sample = src.read(&Request { extent: None, limit: 10 }).unwrap();
        assert_eq!(sample.matched, 1000);
        assert_eq!(sample.features.len(), 10);
        assert!(!sample.complete);
        assert_eq!(sample.features.iter().map(|f| f.id).collect::<Vec<_>>(), (0..10).map(|i| i * 100 + 1).collect::<Vec<_>>());
    });
    std::fs::remove_dir_all(fixture).unwrap();
}

#[test]
fn shapefile_overview_index_shortcut_does_not_include_null_shapes() {
    let fixture = dir();
    cache::with_index_dir(fixture.join("cache"), || {
        let p = fixture.join("indexed-null.shp");
        let mut ds = Dataset::new("point", GeometryType::Point, vec![]);
        ds.push(Some(Geometry::Point(Coord::new(1.0, 1.0))), vec![]);
        write_shapefile(&ds, &p);
        let mut bytes = std::fs::read(&p).unwrap();
        bytes.extend_from_slice(&2i32.to_be_bytes());
        bytes.extend_from_slice(&2i32.to_be_bytes());
        bytes.extend_from_slice(&0i32.to_le_bytes());
        let words = bytes.len() as i32 / 2;
        bytes[24..28].copy_from_slice(&words.to_be_bytes());
        std::fs::write(&p, bytes).unwrap();
        let src = open_path(&p, None).unwrap();
        assert_eq!(src.read(&Request::all()).unwrap().features.len(), 2);
        let view = src.read(&Request { extent: Some(Extent::new(-10.0, -10.0, 10.0, 10.0)), limit: 100 }).unwrap();
        assert_eq!(view.matched, 1);
        assert_eq!(view.features.len(), 1);
        assert!(view.features.iter().all(|f| f.geometry.is_some()));
    });
    std::fs::remove_dir_all(fixture).unwrap();
}

#[test]
fn disk_index_is_reused_and_corrupt_cache_is_rebuilt() {
    let folder = dir();
    let source = folder.join("points.shp");
    points_shapefile(&source);
    let cache_root = folder.join("cache");
    cache::with_index_dir(cache_root.clone(), || {
        drop(ShpSource::open_path(&source, "points").unwrap());
        let path = std::fs::read_dir(&cache_root)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|e| e == "sqlite"))
            .unwrap();
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE reuse_probe(value INTEGER)").unwrap();
        drop(db);
        drop(ShpSource::open_path(&source, "points").unwrap());
        let db = rusqlite::Connection::open(&path).unwrap();
        assert_eq!(db.query_row("SELECT count(*) FROM sqlite_master WHERE name='reuse_probe'", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        drop(db);
        std::fs::write(&path, b"interrupted or corrupted cache").unwrap();
        let reopened = ShpSource::open_path(&source, "points").unwrap();
        assert_eq!(reopened.read(&Request::all()).unwrap().features.len(), 600);
        drop(reopened);
        cache::maintain(&cache_root, 0).unwrap();
    });
    std::fs::remove_dir_all(folder).unwrap();
}

/// Opening costs the same whatever the cache holds: building an index sweeps the cache folder
/// once, reusing one sweeps it not at all, and neither release does (#1230).
#[test]
fn opening_sweeps_the_cache_only_to_build_an_index() {
    let folder = dir();
    let source = folder.join("points.shp");
    points_shapefile(&source);
    let cache_root = folder.join("cache");
    cache::with_index_dir(cache_root.clone(), || {
        let before = cache::sweeps();
        let built = ShpSource::open_path(&source, "points").unwrap();
        assert!(built.info().indexed);
        assert_eq!(cache::sweeps(), before + 1, "building an index sweeps once");
        drop(built);
        let reused = ShpSource::open_path(&source, "points").unwrap();
        assert!(reused.info().indexed);
        assert_eq!(reused.read(&Request::all()).unwrap().features.len(), 600);
        drop(reused);
        assert_eq!(cache::sweeps(), before + 1, "reusing an index and releasing either sweeps nothing");
        cache::maintain(&cache_root, 0).unwrap();
    });
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
fn disk_queries_match_legacy_scan_sampling_and_edges() {
    let folder = dir();
    let source = folder.join("points.shp");
    let mut ds = Dataset::new("points", GeometryType::Point, vec![]);
    for i in 0..401 {
        let x = ((i * 71) % 401) as f64 / 13.0 - 15.0;
        let y = ((i * 37) % 401) as f64 / 17.0 - 12.0;
        ds.push(Some(Geometry::Point(Coord::new(x, y))), vec![]);
    }
    write_shapefile(&ds, &source);
    cache::with_index_dir(folder.join("cache"), || {
        let src = ShpSource::open_path(&source, "points").unwrap();
        for extent in [
            None,
            Some(Extent::new(-180.0, -90.0, 180.0, 90.0)),
            Some(Extent::new(-4.5, -3.0, 7.5, 4.0)),
            Some(Extent::new(80.0, 50.0, 90.0, 60.0)),
        ] {
            let ids: Vec<u64> = ds
                .features
                .iter()
                .enumerate()
                .filter_map(|(i, f)| {
                    let Some(Geometry::Point(p)) = &f.geometry else { return None };
                    let mut raw = Vec::new();
                    raw.extend_from_slice(&1i32.to_le_bytes());
                    raw.extend_from_slice(&p.x.to_le_bytes());
                    raw.extend_from_slice(&p.y.to_le_bytes());
                    let b = record_box(&raw, raw.len() as u64);
                    extent
                        .is_none_or(|e| {
                            b[0] <= e.max_x as f32 && b[2] >= e.min_x as f32 && b[1] <= e.max_y as f32 && b[3] >= e.min_y as f32
                        })
                        .then_some(i as u64 + 1)
                })
                .collect();
            for limit in [0, 1, 7, 29, 1000] {
                let step = crate::source::stride(ids.len() as u64, limit).max(1) as usize;
                let expected: Vec<_> = ids.iter().copied().step_by(step).take(limit).collect();
                let batch = src.read(&Request { extent, limit }).unwrap();
                assert_eq!(batch.matched, ids.len() as u64);
                assert_eq!(batch.complete, step == 1);
                assert_eq!(batch.features.iter().map(|f| f.id).collect::<Vec<_>>(), expected);
            }
        }
    });
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
#[ignore = "manual cold/warm index benchmark; creates 100,000 synthetic records"]
fn shapefile_disk_index_benchmark() {
    let folder = dir();
    let path = folder.join("benchmark.shp");
    let mut ds = Dataset::new("benchmark", GeometryType::Point, vec![]);
    for i in 0..100_000 {
        ds.push(Some(Geometry::Point(Coord::new((i % 1000) as f64 / 10.0 - 50.0, (i / 1000) as f64 / 10.0 - 5.0))), vec![]);
    }
    write_shapefile(&ds, &path);
    cache::with_index_dir(folder.join("cache"), || {
        let clock = std::time::Instant::now();
        let first = ShpSource::open_path(&path, "benchmark").unwrap();
        let cold = clock.elapsed();
        drop(first);
        let clock = std::time::Instant::now();
        let source = ShpSource::open_path(&path, "benchmark").unwrap();
        let warm = clock.elapsed();
        let request = Request { extent: Some(Extent::new(-1.0, -1.0, 1.0, 1.0)), limit: 1000 };
        let clock = std::time::Instant::now();
        let indexed = source.read(&request).unwrap();
        let query = clock.elapsed();
        let clock = std::time::Instant::now();
        let mut matched = 0u64;
        let bytes = std::fs::read(&path).unwrap();
        for raw in bytes.get(100..).unwrap().as_chunks::<28>().0 {
            let [x0, y0, x1, y1] = record_box(raw.get(8..).unwrap(), 20);
            if x0 <= 1.0 && x1 >= -1.0 && y0 <= 1.0 && y1 >= -1.0 {
                matched += 1;
            }
        }
        let scan = clock.elapsed();
        assert_eq!(indexed.matched, matched);
        eprintln!(
            "100000 points: cold={cold:?}, warm={warm:?}, indexed-query={query:?}, record-header scan including file read={scan:?}, matches={matched}"
        );
    });
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
fn shapefile_disk_index_complete_chunks_preserve_full_reads_and_file_order() {
    let fixture = dir();
    cache::with_index_dir(fixture.join("cache"), || {
        let path = fixture.join("complete-indexed.shp");
        points_shapefile(&path);
        let source = open_path(&path, None).unwrap();
        assert!(source.info().indexed);
        assert_eq!(source.read(&Request { extent: None, limit: 1 }).unwrap().features.len(), 1);
        for extent in [None, Some(Extent::new(10.0, 5.0, 15.0, 10.0)), Some(Extent::EMPTY)] {
            let expected = source.read(&Request { extent, ..Request::all() }).unwrap();
            for budget in [1, 256, 8192] {
                let mut rows = Vec::new();
                let summary = source
                    .visit_chunks(extent, budget, &mut |chunk| {
                        assert!(chunk.len() == 1 || crate::source::chunk_memory::features_bytes(&chunk) <= budget);
                        rows.extend(chunk);
                        Ok(())
                    })
                    .unwrap();
                assert!(summary.complete);
                assert!(summary.features.is_empty());
                assert_eq!(summary.matched, expected.matched);
                assert_eq!(summary.skipped, expected.skipped);
                assert_eq!(rows, expected.features);
            }
        }
        for extent in [None, Some(Extent::EMPTY), Some(Extent::new(-180.0, -90.0, -179.0, -89.0))] {
            assert!(source.visit_chunks(extent, 0, &mut |_| panic!("zero budget must not emit")).is_err());
        }
        let mut reentrant = false;
        source
            .visit_chunks(None, 256, &mut |_| {
                if !reentrant {
                    assert_eq!(source.read(&Request { extent: None, limit: 1 })?.features.len(), 1);
                    reentrant = true;
                }
                Ok(())
            })
            .unwrap();
        assert!(reentrant);
        let mut calls = 0;
        assert!(
            source
                .visit_chunks(None, 1, &mut |_| {
                    calls += 1;
                    Err(bad("cancelled indexed traversal"))
                })
                .is_err()
        );
        assert_eq!(calls, 1);
        assert!(source.read(&Request::all()).unwrap().complete, "cancellation must release the index lock");
    });
    std::fs::remove_dir_all(fixture).unwrap();
}

#[test]
fn shapefile_record_past_the_cap_is_skipped_not_allocated() {
    // A record whose header claims more than MAX_RECORD bytes, inside a file grown (sparse where
    // the file system allows) to hold it: skipped and counted, never given a buffer that size.
    let fixture = dir();
    cache::with_index_dir(fixture.join("cache"), || {
        let p = fixture.join("huge-record.shp");
        let mut ds = Dataset::new("two", GeometryType::Point, vec![]);
        ds.push(Some(Geometry::Point(Coord::new(1.0, 2.0))), vec![]);
        ds.push(Some(Geometry::Point(Coord::new(3.0, 4.0))), vec![]);
        write_shapefile(&ds, &p);
        // The second record's header is at 100 + 28; its content length (16-bit words) at +4.
        let bytes = MAX_RECORD + 2;
        let mut shp = std::fs::read(&p).unwrap();
        shp.splice(132..136, i32::try_from(bytes / 2).unwrap().to_be_bytes());
        std::fs::write(&p, &shp).unwrap();
        std::fs::OpenOptions::new().write(true).open(&p).unwrap().set_len(136 + bytes).unwrap();
        let src = open_path(&p, None).unwrap();
        let all = src.read(&Request::all()).unwrap();
        assert_eq!((all.features.len(), all.skipped), (1, 1));
        let mut chunked = 0;
        let batch = src
            .visit_chunks(None, 1 << 20, &mut |f| {
                chunked += f.len();
                Ok(())
            })
            .unwrap();
        assert_eq!((chunked, batch.skipped), (1, 1));
    });
    std::fs::remove_dir_all(fixture).unwrap();
}
