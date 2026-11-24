use super::*;
use crate::raster::wkt::tests::{PlacementCase, metres_apart, placement_cases};

fn varuint(mut v: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let b = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return out;
        }
        out.push(b | 0x80);
    }
}

fn varint(v: i64) -> Vec<u8> {
    let (neg, mut m) = (v < 0, v.unsigned_abs());
    let mut first = (m & 0x3F) as u8 | if neg { 0x40 } else { 0 };
    m >>= 6;
    if m == 0 {
        return vec![first];
    }
    first |= 0x80;
    let mut out = vec![first];
    out.extend(varuint(m));
    out
}

fn utf16_bytes(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// A field for the test writer.
enum F {
    Oid,
    Text(&'static str),
    Int(&'static str),
    Date(&'static str, u8),
    Shape { z: bool, wkt: &'static str },
}

/// One row's stored values (the object ID stores nothing): `None` is null.
enum V {
    Text(Option<&'static str>),
    Int(Option<i32>),
    Blob(Vec<u8>),
    Date(Option<(f64, Option<i16>)>),
}

/// A minimal table (version 3) and its row index, as ArcGIS lays them out.
fn table(layer_flags: u32, fields: &[F], rows: &[Vec<V>]) -> (Vec<u8>, Vec<u8>) {
    let mut f = Vec::new();
    f.extend(4i32.to_le_bytes());
    f.extend((layer_flags | 0x100).to_le_bytes());
    f.extend((fields.len() as i16).to_le_bytes());
    let name = |f: &mut Vec<u8>, n: &str| {
        f.push(n.encode_utf16().count() as u8);
        f.extend(utf16_bytes(n));
        f.push(0);
    };
    for field in fields {
        match field {
            F::Oid => {
                name(&mut f, "OBJECTID");
                f.extend([6, 4, 2]);
            }
            F::Text(n) => {
                name(&mut f, n);
                f.push(4);
                f.extend(255i32.to_le_bytes());
                f.extend([5, 0]);
            }
            F::Int(n) => {
                name(&mut f, n);
                f.extend([1, 4, 5, 0]);
            }
            F::Date(n, kind) => {
                name(&mut f, n);
                f.extend([*kind, 8, 5, 0]);
            }
            F::Shape { z, wkt } => {
                name(&mut f, "SHAPE");
                f.extend([7, 0, 7]);
                let w = utf16_bytes(wkt);
                f.extend((w.len() as i16).to_le_bytes());
                f.extend(w);
                f.push(if *z { 3 } else { 1 });
                for v in [0.0f64, 0.0, 1000.0] {
                    f.extend(v.to_le_bytes());
                }
                if *z {
                    f.extend(0.0f64.to_le_bytes());
                    f.extend(1000.0f64.to_le_bytes());
                }
                f.extend(0.001f64.to_le_bytes());
                if *z {
                    f.extend(0.001f64.to_le_bytes());
                }
                for v in [0.0f64, 0.0, 100.0, 100.0] {
                    f.extend(v.to_le_bytes());
                }
                if layer_flags & 0x8000_0000 != 0 {
                    f.extend([0.0f64, 100.0].iter().flat_map(|v| v.to_le_bytes()));
                }
                f.push(0);
                f.extend(1u32.to_le_bytes());
                f.extend(10.0f64.to_le_bytes());
            }
        }
    }
    // Every field but the object ID can be null here (the shape's flag 7 says so), so each has a
    // bit in the null flags.
    let nullable = fields.iter().filter(|f| !matches!(f, F::Oid)).count();
    let mut t = vec![0u8; 40];
    t.extend((f.len() as i32).to_le_bytes());
    t.extend(&f);
    let mut offsets = Vec::new();
    for row in rows {
        let mut nulls = vec![0u8; nullable.div_ceil(8)];
        let mut body = Vec::new();
        for (bit, v) in row.iter().enumerate() {
            match v {
                V::Text(None) | V::Int(None) | V::Date(None) => {
                    if let Some(b) = nulls.get_mut(bit / 8) {
                        *b |= 1 << (bit % 8);
                    }
                }
                V::Text(Some(s)) => {
                    body.extend(varuint(s.len() as u64));
                    body.extend(s.as_bytes());
                }
                V::Int(Some(i)) => body.extend(i.to_le_bytes()),
                V::Date(Some((days, offset))) => {
                    body.extend(days.to_le_bytes());
                    if let Some(o) = offset {
                        body.extend(o.to_le_bytes());
                    }
                }
                V::Blob(b) => {
                    body.extend(varuint(b.len() as u64));
                    body.extend(b);
                }
            }
        }
        offsets.push(t.len() as u32);
        t.extend(((nulls.len() + body.len()) as i32).to_le_bytes());
        t.extend(nulls);
        t.extend(body);
    }
    let size = t.len() as i64;
    t[0..4].copy_from_slice(&3i32.to_le_bytes());
    t[4..8].copy_from_slice(&(rows.len() as i32).to_le_bytes());
    t[12..16].copy_from_slice(&5i32.to_le_bytes());
    t[24..32].copy_from_slice(&size.to_le_bytes());
    t[32..40].copy_from_slice(&40i64.to_le_bytes());
    let mut x = Vec::new();
    for v in [3i32, 1, rows.len() as i32, 4] {
        x.extend(v.to_le_bytes());
    }
    for i in 0..1024 {
        x.extend(offsets.get(i).copied().unwrap_or(0).to_le_bytes());
    }
    x.extend([0u8; 16]);
    (t, x)
}

/// A polygon blob (type 5): one ring, scale 1000 from 0.
fn polygon(ring: &[(f64, f64)]) -> Vec<u8> {
    let mut b = varuint(5);
    b.extend(varuint(ring.len() as u64));
    b.extend(varuint(1));
    b.extend([0u8; 4]);
    let (mut px, mut py) = (0i64, 0i64);
    for &(x, y) in ring {
        let (ix, iy) = ((x * 1000.0).round() as i64, (y * 1000.0).round() as i64);
        b.extend(varint(ix - px));
        b.extend(varint(iy - py));
        (px, py) = (ix, iy);
    }
    b
}

/// A multipatch blob (type 54 with Z): its parts' types and points.
fn multipatch(parts: &[(u64, Vec<(f64, f64, f64)>)]) -> Vec<u8> {
    let n: usize = parts.iter().map(|(_, p)| p.len()).sum();
    let mut b = varuint(54 | HAS_Z);
    b.extend(varuint(n as u64));
    b.extend(varuint(0));
    b.extend(varuint(parts.len() as u64));
    b.extend([0u8; 4]);
    for (_, p) in parts.iter().take(parts.len().saturating_sub(1)) {
        b.extend(varuint(p.len() as u64));
    }
    for (kind, _) in parts {
        b.extend(varuint(*kind));
    }
    let (mut px, mut py) = (0i64, 0i64);
    for (_, p) in parts {
        for &(x, y, _) in p {
            let (ix, iy) = ((x * 1000.0).round() as i64, (y * 1000.0).round() as i64);
            b.extend(varint(ix - px));
            b.extend(varint(iy - py));
            (px, py) = (ix, iy);
        }
    }
    let mut pz = 0i64;
    for (_, p) in parts {
        for &(_, _, z) in p {
            let iz = (z * 1000.0).round() as i64;
            b.extend(varint(iz - pz));
            pz = iz;
        }
    }
    b
}

const LONLAT: &str = r#"GEOGCS["GCS_WGS_1984",DATUM["D_WGS_1984",SPHEROID["WGS_1984",6378137.0,298.257223563]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]]"#;

/// A geodatabase's files: a catalog naming the given tables, which follow as a00000002….
fn geodatabase(tables: Vec<(&'static str, (Vec<u8>, Vec<u8>))>) -> Vec<(String, Vec<u8>)> {
    let rows: Vec<Vec<V>> = std::iter::once("GDB_SystemCatalog")
        .chain(tables.iter().map(|(n, _)| *n))
        .map(|n| vec![V::Text(Some(n)), V::Int(Some(0))])
        .collect();
    let (t, x) = table(0, &[F::Oid, F::Text("Name"), F::Int("FileFormat")], &rows);
    let mut files = vec![("a00000001.gdbtable".to_string(), t), ("a00000001.gdbtablx".to_string(), x)];
    for (i, (_, (t, x))) in tables.into_iter().enumerate() {
        files.push((format!("a{:08x}.gdbtable", i + 2), t));
        files.push((format!("a{:08x}.gdbtablx", i + 2), x));
    }
    files
}

fn read_files(files: &[(String, Vec<u8>)]) -> Result<Vec<(Dataset, ReadReport)>> {
    read("test", &|name: &str| Ok(files.iter().find(|(n, _)| n == name).map(|(_, b)| b.clone())), None).map(|r| r.layers)
}

/// A square (clockwise, as ArcGIS stores outer rings) from (x, y) with side `s`.
fn square(x: f64, y: f64, s: f64) -> Vec<(f64, f64)> {
    vec![(x, y), (x, y + s), (x + s, y + s), (x + s, y), (x, y)]
}

#[test]
fn numbers_decode() {
    for v in [0u64, 1, 63, 64, 127, 128, 300, 1 << 40, u64::MAX >> 1] {
        assert_eq!(Cur::new(&varuint(v), 0).varuint(), Some(v), "{v}");
    }
    for v in [0i64, 1, -1, 63, -63, 64, -64, -65, 1_000_000, -1_000_000, 1 << 50] {
        assert_eq!(Cur::new(&varint(v), 0).varint(), Some(v), "{v}");
    }
    // Endless continuation bits and cut-short numbers are refused.
    assert_eq!(Cur::new(&[0xFF; 12], 0).varuint(), None);
    assert_eq!(Cur::new(&[0xFF; 12], 0).varint(), None);
    assert_eq!(Cur::new(&[0x80], 0).varuint(), None);
    assert_eq!(guid(&[0x67, 0x45, 0x23, 0x01, 0xAB, 0x89, 0xEF, 0xCD, 1, 2, 3, 4, 5, 6, 7, 8]), "{01234567-89AB-CDEF-0102-030405060708}");
    // 2000-01-01 is day 36,526 after 1899-12-30.
    assert_eq!(date(36_526.5, "YYYY-MM-DDTHH:mm:ss"), Value::Text("2000-01-01T12:00:00".into()));
}

#[test]
fn reads_a_feature_class_with_fields_nulls_and_holes() {
    let outer = square(0.0, 0.0, 10.0);
    // A hole: counterclockwise, in the same blob as a second part.
    let hole: Vec<(f64, f64)> = square(2.0, 2.0, 2.0).into_iter().rev().collect();
    let mut blob = varuint(5);
    blob.extend(varuint((outer.len() + hole.len()) as u64));
    blob.extend(varuint(2));
    blob.extend([0u8; 4]);
    blob.extend(varuint(outer.len() as u64));
    let (mut px, mut py) = (0i64, 0i64);
    for &(x, y) in outer.iter().chain(&hole) {
        let (ix, iy) = ((x * 1000.0) as i64, (y * 1000.0) as i64);
        blob.extend(varint(ix - px));
        blob.extend(varint(iy - py));
        (px, py) = (ix, iy);
    }
    let fields = [F::Oid, F::Shape { z: false, wkt: LONLAT }, F::Text("name"), F::Int("count")];
    let rows = vec![
        vec![V::Blob(blob), V::Text(Some("Gebäude")), V::Int(Some(7))],
        vec![V::Blob(polygon(&square(20.0, 20.0, 5.0))), V::Text(None), V::Int(None)],
    ];
    let files = geodatabase(vec![("Parcels", table(4, &fields, &rows))]);
    let layers = read_files(&files).unwrap();
    assert_eq!(layers.len(), 1);
    let (ds, report) = &layers[0];
    assert_eq!(ds.name, "Parcels");
    assert_eq!(ds.geometry_type, GeometryType::Polygon);
    assert_eq!(report.skipped, 0, "{report:?}");
    let names: Vec<&str> = ds.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["OBJECTID", "name", "count"]);
    let rows: Vec<&crate::Feature> = ds.features.iter().collect();
    assert_eq!(rows[0].attrs, vec![Value::Int(1), Value::Text("Gebäude".into()), Value::Int(7)]);
    assert_eq!(rows[1].attrs, vec![Value::Int(2), Value::Null, Value::Null]);
    match &rows[0].geometry {
        Some(Geometry::Polygon(p)) => {
            assert_eq!(p.len(), 1, "one polygon");
            assert_eq!(p[0].len(), 2, "with its hole");
            assert_eq!(p[0][0][2], Coord::new(10.0, 10.0));
        }
        g => panic!("{g:?}"),
    }
}

#[test]
fn a_multipatch_is_its_footprint_with_its_heights() {
    // A 10 m box from z 400 to 410: its roof as two triangles (a strip), a floor ring, and walls,
    // which show nothing from above.
    let roof = (0, vec![(0.0, 0.0, 410.0), (0.0, 10.0, 410.0), (10.0, 0.0, 410.0), (10.0, 10.0, 410.0)]);
    let floor = (2, vec![(0.0, 0.0, 400.0), (0.0, 10.0, 400.0), (10.0, 10.0, 400.0), (10.0, 0.0, 400.0), (0.0, 0.0, 400.0)]);
    let wall = (2, vec![(0.0, 0.0, 400.0), (0.0, 0.0, 410.0), (10.0, 0.0, 410.0), (10.0, 0.0, 400.0), (0.0, 0.0, 400.0)]);
    // A leaning wall: a sliver from above, which is left out too.
    let leaning = (6, vec![(0.0, 10.0, 400.0), (5.0, 10.003, 410.0), (10.0, 10.0, 400.0)]);
    // Only walls: the outline of its points stands in.
    let walls_only = vec![
        (2, vec![(30.0, 30.0, 0.0), (30.0, 30.0, 5.0), (40.0, 30.0, 5.0), (40.0, 30.0, 0.0), (30.0, 30.0, 0.0)]),
        (2, vec![(40.0, 30.0, 0.0), (40.0, 30.0, 5.0), (40.0, 40.0, 5.0), (40.0, 40.0, 0.0), (40.0, 30.0, 0.0)]),
    ];
    let fields = [F::Oid, F::Shape { z: true, wkt: LONLAT }, F::Text("name")];
    let rows = vec![
        vec![V::Blob(multipatch(&[roof, floor, wall, leaning])), V::Text(Some("box"))],
        vec![V::Blob(multipatch(&walls_only)), V::Text(Some("walls"))],
    ];
    let files = geodatabase(vec![("Buildings", table(9 | 0x8000_0000, &fields, &rows))]);
    let layers = read_files(&files).unwrap();
    let (ds, report) = &layers[0];
    assert_eq!(ds.geometry_type, GeometryType::Polygon);
    assert_eq!(ds.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["OBJECTID", "name", "z_min", "z_max", "height"]);
    let rows: Vec<&crate::Feature> = ds.features.iter().collect();
    assert_eq!(rows[0].attrs[2..], [Value::Float(400.0), Value::Float(410.0), Value::Float(10.0)]);
    let area = |g: &Option<Geometry>| match g {
        Some(Geometry::Polygon(p)) => p.iter().map(|poly| poly.first().map_or(0.0, |r| signed_area(r).abs() / 2.0)).sum::<f64>(),
        _ => 0.0,
    };
    assert!((area(&rows[0].geometry) - 100.0).abs() < 1e-6, "{:?}", rows[0].geometry);
    assert!((area(&rows[1].geometry) - 50.0).abs() < 1e-6, "the hull of an L of walls: {:?}", rows[1].geometry);
    assert!(report.messages.iter().any(|m| m.contains("footprints")), "{report:?}");
    assert!(report.messages.iter().any(|m| m.contains("1 multipatches with no faces")), "{report:?}");
    assert!(!report.messages.iter().any(|m| m.contains("no footprint")), "every multipatch has one: {report:?}");
}

/// A multipatch whose points outline no area (too few, or a wall standing in a straight line) is
/// kept without geometry, and the reader says so. One with no points has nothing to lose: not counted.
#[test]
fn a_multipatch_without_a_footprint_is_counted() {
    let floor = (2, vec![(0.0, 0.0, 0.0), (0.0, 10.0, 0.0), (10.0, 10.0, 0.0), (10.0, 0.0, 0.0), (0.0, 0.0, 0.0)]);
    let stub = (2, vec![(20.0, 20.0, 0.0), (21.0, 21.0, 0.0)]);
    let wall = (2, vec![(30.0, 30.0, 0.0), (30.0, 30.0, 5.0), (40.0, 30.0, 5.0), (40.0, 30.0, 0.0), (30.0, 30.0, 0.0)]);
    let fields = [F::Oid, F::Shape { z: true, wkt: LONLAT }];
    let rows = vec![
        vec![V::Blob(multipatch(&[floor]))],
        vec![V::Blob(multipatch(&[stub]))],
        vec![V::Blob(multipatch(&[wall]))],
        vec![V::Blob(multipatch(&[]))],
    ];
    let files = geodatabase(vec![("Buildings", table(9 | 0x8000_0000, &fields, &rows))]);
    let layers = read_files(&files).unwrap();
    let (ds, report) = &layers[0];
    let rows: Vec<&crate::Feature> = ds.features.iter().collect();
    assert_eq!(rows.len(), 4, "all kept");
    assert!(rows[0].geometry.is_some() && rows[1..].iter().all(|f| f.geometry.is_none()), "{rows:?}");
    assert!(report.messages.iter().any(|m| m.contains("2 multipatches have no footprint")), "{report:?}");
}

#[test]
fn row_index_with_left_out_blocks() {
    // Rows 1025 and 1026 stored in the one block present (the second of two).
    let mut x = Vec::new();
    for v in [3i32, 1, 2048, 4] {
        x.extend(v.to_le_bytes());
    }
    for i in 0..1024u32 {
        x.extend(
            match i {
                0 => 100u32,
                1 => 0,
                2 => 200,
                _ => 0,
            }
            .to_le_bytes(),
        );
    }
    for v in [1i32, 2, 1, 1] {
        x.extend(v.to_le_bytes());
    }
    x.extend(0b10u32.to_le_bytes());
    assert_eq!(row_offsets(&x).unwrap(), vec![(1025, 100), (1027, 200)]);
    // A version 4 index with missing blocks isn't read here.
    let mut x = Vec::new();
    for v in [4i32, 0, 0, 4] {
        x.extend(v.to_le_bytes());
    }
    x.extend(10i64.to_le_bytes());
    x.extend(30i32.to_le_bytes());
    assert!(matches!(row_offsets(&x), Err(DataError::Unsupported(_))));
}

/// A row index claiming billions of rows in blocks its bitmap leaves out is passed over block by
/// block, not row by row (it hung before).
#[test]
fn a_huge_row_count_with_absent_blocks_is_quick() {
    let mut x = Vec::new();
    for v in [3i32, 0, i32::MAX, 4] {
        x.extend(v.to_le_bytes());
    }
    for v in [1i32, (i32::MAX / 1024) + 1, 0, 1] {
        x.extend(v.to_le_bytes());
    }
    x.extend(0u32.to_le_bytes());
    let start = std::time::Instant::now();
    assert_eq!(row_offsets(&x).unwrap(), Vec::new());
    assert!(start.elapsed() < std::time::Duration::from_secs(1), "{:?}", start.elapsed());
}

/// One strip of a million points makes no more faces than the cap (each is an allocation).
#[test]
fn a_huge_strip_stops_at_the_face_cap() {
    let strip: Vec<P3> = (0..1_000_000).map(|i| [f64::from(i), f64::from(i % 2), 0.0]).collect();
    let n = strip.len();
    for kind in [0, 1, 6] {
        let (f, truncated) = faces(std::slice::from_ref(&strip), &[kind]);
        assert_eq!((f.len(), truncated), (MAX_FACES, true), "kind {kind}");
    }
    // Later parts get only the room left.
    assert_eq!(faces(&[strip[..10].to_vec(), strip.clone()], &[0, 0]).0.len(), MAX_FACES);
    assert!(n / 3 > MAX_FACES);
    // Within the cap, nothing is left out.
    assert_eq!(faces(&[strip[..10].to_vec()], &[0]), (strip[..10].windows(3).map(|w| vec![w.to_vec()]).collect(), false));
    // A truncated patch is drawn as its points' hull, not as part of its faces.
    let mut budget = MAX_TABLE_FACES;
    let (shape, hull) = footprint(std::slice::from_ref(&strip), &[0], &mut budget);
    assert!(hull == Some(Hull::Unmerged) && shape.is_some());
    assert_eq!(budget, MAX_TABLE_FACES, "nothing was merged");
}

/// A ring with no area (its points on a line) is neither an outer ring nor a hole: no sliver.
#[test]
fn a_ring_with_no_area_is_left_out() {
    let fields = [F::Oid, F::Shape { z: false, wkt: LONLAT }];
    let line = [(0.0, 0.0), (5.0, 0.0), (10.0, 0.0), (0.0, 0.0)];
    let rows = vec![vec![V::Blob(polygon(&line))], vec![V::Blob(polygon(&square(0.0, 0.0, 10.0)))]];
    let files = geodatabase(vec![("Flat", table(4, &fields, &rows))]);
    let layers = read_files(&files).unwrap();
    let (ds, report) = &layers[0];
    let rows: Vec<&crate::Feature> = ds.features.iter().collect();
    assert!(!matches!(rows.first().and_then(|f| f.geometry.as_ref()), Some(Geometry::Polygon(_))), "{report:?}");
    assert!(matches!(rows.get(1).and_then(|f| f.geometry.as_ref()), Some(Geometry::Polygon(p)) if p.len() == 1));
}

/// A small ring far from the origin keeps its area: products of its coordinates would round it away.
#[test]
fn a_small_ring_far_from_the_origin_has_an_area() {
    let ring: Vec<Coord> =
        [(0.0, 0.0), (0.0, 1.0), (1.0, 1.0), (1.0, 0.0), (0.0, 0.0)].iter().map(|&(x, y)| Coord::new(1e9 + x, 1e9 + y)).collect();
    assert_eq!(signed_area(&ring), -2.0, "clockwise, twice its area");
}

/// A height that isn't a finite number can't say which way a face looks: the hull stands in, rather
/// than that face going without a word.
#[test]
fn a_multipatch_with_an_infinite_height_is_its_hull() {
    let roof = vec![[0.0, 0.0, 5.0], [10.0, 0.0, f64::INFINITY], [10.0, 10.0, 5.0], [0.0, 10.0, 5.0], [0.0, 0.0, 5.0]];
    let mut budget = 10;
    let (shape, hull) = footprint(std::slice::from_ref(&roof), &[2], &mut budget);
    assert_eq!((hull, budget), (Some(Hull::Unmerged), 10));
    assert!(shape.is_some());
}

/// A table's multipatches merge no more faces than its budget: past it, they are hulls.
#[test]
fn the_face_budget_bounds_a_tables_unions() {
    // A 10 m square roof as a strip of two triangles, and a wall.
    let parts = vec![
        vec![[0.0, 0.0, 5.0], [0.0, 10.0, 5.0], [10.0, 0.0, 5.0], [10.0, 10.0, 5.0]],
        vec![[0.0, 0.0, 0.0], [0.0, 0.0, 5.0], [10.0, 0.0, 5.0], [10.0, 0.0, 0.0], [0.0, 0.0, 0.0]],
    ];
    let kinds = [0, 2];
    let mut budget = 3;
    let (shape, hull) = footprint(&parts, &kinds, &mut budget);
    assert_eq!((hull, budget), (None, 1), "two faces show and are merged: {shape:?}");
    let (shape, hull) = footprint(&parts, &kinds, &mut budget);
    assert_eq!((hull, budget), (Some(Hull::Unmerged), 1), "past the budget: {shape:?}");
    assert!(shape.is_some());
}

/// Numbers whose bits run past 64 are refused: one decoding to `i64::MIN` panicked when negated.
#[test]
fn numbers_past_64_bits_are_refused() {
    // Sign set, magnitude 2^63: eight continuation bytes reach bit 62, then 0x02.
    let mut b = vec![0xC0];
    b.extend([0x80; 8]);
    b.push(0x02);
    assert_eq!(Cur::new(&b, 0).varint(), None);
    // The largest magnitude still reads.
    let mut b = vec![0xFF];
    b.extend([0xFF; 8]);
    b.push(0x01);
    assert_eq!(Cur::new(&b, 0).varint(), Some(-i64::MAX));
    // Unsigned: bit 63 reads, a bit past it doesn't.
    let mut b = vec![0x80; 9];
    b.push(0x01);
    assert_eq!(Cur::new(&b, 0).varuint(), Some(1 << 63));
    let last = b.len() - 1;
    b[last] = 0x02;
    assert_eq!(Cur::new(&b, 0).varuint(), None);
}

/// A table that can't be opened (too large, not readable) is said by name; the others are read.
#[test]
fn an_unreadable_table_leaves_the_others() {
    let fields = [F::Oid, F::Text("name")];
    let rows = vec![vec![V::Text(Some("a"))]];
    let files = geodatabase(vec![("Good", table(0, &fields, &rows)), ("Huge", table(0, &fields, &rows))]);
    let open = |name: &str| -> Result<Option<Vec<u8>>> {
        if name == "a00000003.gdbtable" {
            return Err(DataError::Unsupported(format!("{name} is too large to read here")));
        }
        Ok(files.iter().find(|(n, _)| n == name).map(|(_, b)| b.clone()))
    };
    let read = read("test", &open, None).unwrap();
    assert_eq!(read.layers.iter().map(|(d, _)| d.name.as_str()).collect::<Vec<_>>(), ["Good"]);
    assert_eq!(read.unread.len(), 1);
    assert_eq!(read.unread[0].0, "Huge");
    assert!(read.unread[0].1.contains("too large"), "{:?}", read.unread);
}

/// A table the catalog lists whose files are missing is said by name, not passed over.
#[test]
fn a_table_with_missing_files_is_said() {
    let fields = [F::Oid, F::Text("name")];
    let rows = vec![vec![V::Text(Some("a"))]];
    let mut files = geodatabase(vec![("Good", table(0, &fields, &rows)), ("Gone", table(0, &fields, &rows))]);
    files.retain(|(n, _)| n != "a00000003.gdbtablx");
    let open = |name: &str| -> Result<Option<Vec<u8>>> { Ok(files.iter().find(|(n, _)| n == name).map(|(_, b)| b.clone())) };
    let read = read("test", &open, None).unwrap();
    assert_eq!(read.layers.len(), 1);
    assert_eq!(read.unread.len(), 1, "{:?}", read.unread);
    assert_eq!(read.unread[0].0, "Gone");
    assert_eq!(read.unread[0].1, "its file a00000003.gdbtablx is missing");
}

/// A feature whose coordinates its coordinate system can't place keeps its row, and is said.
#[test]
fn coordinates_that_cant_be_converted_are_said() {
    const LAEA: &str = r#"PROJCS["ETRS89 / LAEA Europe",GEOGCS["ETRS89",DATUM["European_Terrestrial_Reference_System_1989",SPHEROID["GRS 1980",6378137,298.257222101]],PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433]],PROJECTION["Lambert_Azimuthal_Equal_Area"],PARAMETER["latitude_of_center",52],PARAMETER["longitude_of_center",10],PARAMETER["false_easting",4321000],PARAMETER["false_northing",3210000],UNIT["metre",1],AUTHORITY["EPSG","3035"]]"#;
    let fields = [F::Oid, F::Shape { z: false, wkt: LAEA }, F::Text("name")];
    let rows = vec![
        vec![V::Blob(polygon(&square(4_321_000.0, 3_210_000.0, 10.0))), V::Text(Some("in"))],
        // Far past the far side of the Earth.
        vec![V::Blob(polygon(&square(9e8, 9e8, 10.0))), V::Text(Some("out"))],
    ];
    let files = geodatabase(vec![("T", table(4, &fields, &rows))]);
    let layers = read_files(&files).unwrap();
    let (ds, report) = &layers[0];
    let rows: Vec<&crate::Feature> = ds.features.iter().collect();
    assert_eq!(rows.len(), 2);
    assert!(rows[0].geometry.is_some() && rows[1].geometry.is_none(), "{:?}", rows[1].geometry);
    assert!(report.messages.iter().any(|m| m.starts_with("1 features have coordinates outside")), "{report:?}");
}

#[test]
fn compressed_tables_and_unknown_files_are_said() {
    let (t, x) = table(
        0,
        &[F::Oid, F::Text("Name"), F::Int("FileFormat")],
        &[vec![V::Text(Some("GDB_SystemCatalog")), V::Int(Some(0))], vec![V::Text(Some("Packed")), V::Int(Some(1))]],
    );
    let files = vec![("a00000001.gdbtable".to_string(), t), ("a00000001.gdbtablx".to_string(), x)];
    assert!(matches!(read_files(&files), Err(DataError::Unsupported(m)) if m.contains("compressed")));
    assert!(matches!(read_files(&[]), Err(DataError::Format(m)) if m.contains("not a File Geodatabase")));
}

/// Damaged files are refused or read in part, never a panic.
#[test]
fn hostile_bytes_never_panic() {
    let fields = [F::Oid, F::Shape { z: true, wkt: LONLAT }, F::Text("name"), F::Int("count")];
    let rows = vec![
        vec![V::Blob(multipatch(&[(0, vec![(0.0, 0.0, 1.0), (0.0, 5.0, 1.0), (5.0, 0.0, 2.0)])])), V::Text(Some("a")), V::Int(Some(1))],
        vec![V::Blob(polygon(&square(1.0, 1.0, 3.0))), V::Text(None), V::Int(Some(2))],
    ];
    let files = geodatabase(vec![("T", table(9 | 0x8000_0000, &fields, &rows))]);
    assert!(read_files(&files).is_ok());
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for _ in 0..3000 {
        let mut damaged = files.clone();
        let which = (next() % damaged.len() as u64) as usize;
        let bytes = &mut damaged[which].1;
        match next() % 3 {
            // Flipped bytes.
            0 => {
                for _ in 0..1 + next() % 8 {
                    let at = (next() % bytes.len().max(1) as u64) as usize;
                    if let Some(b) = bytes.get_mut(at) {
                        *b = next() as u8;
                    }
                }
            }
            // Cut short.
            1 => bytes.truncate((next() % (bytes.len() as u64 + 1)) as usize),
            // Counts and sizes at their extremes.
            _ => {
                let at = (next() % bytes.len().max(1) as u64) as usize;
                for (k, b) in [0xFFu8, 0xFF, 0xFF, 0x7F].into_iter().enumerate() {
                    if let Some(x) = bytes.get_mut(at + k) {
                        *x = b;
                    }
                }
            }
        }
        let _ = read_files(&damaged);
    }
}

/// A geodatabase GDAL writes (when GDAL is installed), read back without it: points with typed
/// fields, and a TIN, which GDAL stores as a multipatch (a triangle fan).
#[test]
fn reads_what_gdal_writes_when_gdal_is_installed() {
    if !crate::source::gdal::available() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("rustgis-filegdb-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let gdb = dir.join("written.gdb");
    let run = |args: &[&str]| {
        let ok = std::process::Command::new("ogr2ogr").args(args).status().is_ok_and(|s| s.success());
        assert!(ok, "ogr2ogr {args:?}");
    };
    let points = dir.join("points.csv");
    std::fs::write(&points, "WKT,name,count,height,day\n\"POINT (8.5 47.4)\",Zürich,3,1.5,2024-05-06\n\"POINT (7.4 46.9)\",,,,\n").unwrap();
    let tins = dir.join("tins.csv");
    std::fs::write(
        &tins,
        "WKT,name\n\"TIN Z (((0 0 400,0 10 400,10 10 400,0 0 400)),((0 0 400,10 10 400,10 0 400,0 0 400)),((0 0 400,10 0 400,10 0 410,0 0 400)))\",block\n",
    )
    .unwrap();
    let (gdb_s, points_s, tins_s) = (gdb.display().to_string(), points.display().to_string(), tins.display().to_string());
    run(&["-f", "OpenFileGDB", "-a_srs", "EPSG:4326", "-nlt", "POINT", "-nln", "places", "-oo", "AUTODETECT_TYPE=YES", &gdb_s, &points_s]);
    run(&["-f", "OpenFileGDB", "-update", "-a_srs", "EPSG:2056", "-nlt", "TINZ", "-nln", "blocks", &gdb_s, &tins_s]);
    assert!(is_geodatabase(&gdb));
    assert_eq!(layers(&gdb).unwrap(), ["places", "blocks"]);
    let read = read_dir(&gdb, None).unwrap().layers;
    let (places, report) = &read[0];
    assert_eq!(report.skipped, 0, "{report:?}");
    let rows: Vec<&crate::Feature> = places.features.iter().collect();
    assert_eq!(rows.len(), 2);
    assert!(
        matches!(rows[0].geometry, Some(Geometry::Point(c)) if (c.x - 8.5).abs() < 1e-7 && (c.y - 47.4).abs() < 1e-7),
        "{:?}",
        rows[0].geometry
    );
    let at = |n: &str| places.field_index(n).unwrap();
    assert_eq!(rows[0].attrs[at("name")], Value::Text("Zürich".into()));
    assert_eq!(rows[0].attrs[at("count")], Value::Int(3));
    assert_eq!(rows[0].attrs[at("height")], Value::Float(1.5));
    // GDAL's CSV autodetect makes the date an ESRI `Date` field, which is read as a native date
    // and time (midnight here), as every FileGDB date is (#1894).
    assert!(matches!(&rows[0].attrs[at("day")], Value::DateTime(d) if d.to_string() == "2024-05-06T00:00:00"), "{:?}", rows[0].attrs);
    assert_eq!(rows[1].attrs[at("count")], Value::Null);
    let (blocks, _) = &read[1];
    assert_eq!(blocks.crs, Some(Crs::Epsg(2056)), "the coordinate system, by its code");
    let block = blocks.features.iter().next().unwrap();
    let z = |n: &str| block.attrs[blocks.field_index(n).unwrap()].clone();
    assert_eq!((z("z_min"), z("z_max"), z("height")), (Value::Float(400.0), Value::Float(410.0), Value::Float(10.0)));
    assert!(matches!(&block.geometry, Some(Geometry::Polygon(p)) if p.len() == 1), "{:?}", block.geometry);
    // One layer by name, ignoring case.
    assert_eq!(read_dir(&gdb, Some("BLOCKS")).unwrap().layers.len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Another body's system in a File Geodatabase or an ArcGIS page (Mars, IAU 2015) is read as its
/// longitude/latitude with one notice, refused when it can't be converted, and an Earth system
/// that can't be converted is read as longitude/latitude, as before.
#[test]
fn planetary_systems_are_said_or_refused() {
    let gcs = r#"GEOGCS["GCS_Mars_2015_-_Sphere_Ocentric",DATUM["D_Mars_2015_-_Sphere",SPHEROID["Mars_2015_-_Sphere",3396190.0,0.0]],PRIMEM["Reference_Meridian",0.0],UNIT["Degree",0.0174532925199433]]"#;
    // A method no one knows (Bonne is read, as PROJ 9.8.1 reads it, on any body).
    let unread = |gcs: &str| {
        format!(
            r#"PROJCS["Unheard_Of",{gcs},PROJECTION["Unheard_Of"],PARAMETER["Central_Meridian",0.0],PARAMETER["Standard_Parallel_1",30.0],UNIT["Meter",1.0]]"#
        )
    };
    let body = |m: &String| m.contains("belongs to another body than the Earth");
    let mut report = ReadReport::default();
    let (crs, t) = esri_crs(None, Some(gcs), &mut report).unwrap();
    assert!(crs.is_some_and(Crs::is_planetary) && t.is_some());
    assert_eq!(report.messages.iter().filter(|m| body(&m.text)).count(), 1, "{:?}", report.messages);
    let e = esri_crs(None, Some(&unread(gcs)), &mut ReadReport::default()).err().unwrap().to_string();
    assert!(e.contains("belongs to another body than the Earth") && e.contains("Unheard_Of"), "{e}");
    let earth = r#"GEOGCS["GCS_WGS_1984",DATUM["D_WGS_1984",SPHEROID["WGS_1984",6378137.0,298.257223563]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]]"#;
    let mut report = ReadReport::default();
    let (crs, t) = esri_crs(None, Some(&unread(earth)), &mut report).unwrap();
    assert!(crs.is_none() && t.is_none());
    assert!(
        report.messages.iter().any(|m| m.contains("Unheard_Of")) && !report.messages.iter().any(|m| body(&m.text)),
        "{:?}",
        report.messages
    );
}

/// #1677: an EPSG code is used as a `.prj`'s own `AUTHORITY` is: when the WKT gives no shift to WGS
/// 84, as Esri's doesn't, or the code's own. Another shift is read from the WKT. An Esri code is
/// used whatever shift its WKT gives, and another shift is said to be set aside. Neither kind of code, both the Earth's, is used for another
/// body's WKT.
#[test]
fn a_code_gives_way_to_its_wkts_shift_or_body() {
    let bng = crate::raster::wkt::tests::bng_wkt1();
    let own = "TOWGS84[446.448,-125.157,542.06,0.15,0.247,0.842,-20.489]";
    let other = bng.replace(own, "TOWGS84[375,-111,431,0,0,0,0]");
    assert_ne!(other, bng);
    let esri_bng = r#"PROJCS["British_National_Grid",GEOGCS["GCS_OSGB_1936",DATUM["D_OSGB_1936",SPHEROID["Airy_1830",6377563.396,299.3249646]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Transverse_Mercator"],PARAMETER["False_Easting",400000.0],PARAMETER["False_Northing",-100000.0],PARAMETER["Central_Meridian",-2.0],PARAMETER["Scale_Factor",0.9996012717],PARAMETER["Latitude_Of_Origin",49.0],UNIT["Meter",1.0]]"#;
    for wkt in [esri_bng, &bng] {
        let mut report = ReadReport::default();
        let (crs, t) = esri_crs(Some(27700), Some(wkt), &mut report).unwrap();
        assert_eq!(crs, Some(Crs::Epsg(27700)));
        assert!(t.is_some() && report.messages.is_empty(), "{:?}", report.messages);
    }
    let mut report = ReadReport::default();
    let (crs, t) = esri_crs(Some(27700), Some(&other), &mut report).unwrap();
    assert_eq!(crs, crate::raster::wkt::crs_from_wkt(&other).ok());
    assert!(crs.is_some_and(|c| c.custom_def().is_some_and(|d| d.proj4.contains("+towgs84=375,-111,431"))), "{crs:?}");
    assert!(t.is_some() && report.messages.is_empty(), "{:?}", report.messages);
    // A shift of zero where the code has one is the WKT's, said.
    let mut report = ReadReport::default();
    let (crs, _) = esri_crs(Some(27700), Some(&bng.replace(own, "TOWGS84[0,0,0,0,0,0,0]")), &mut report).unwrap();
    assert!(matches!(crs, Some(Crs::Custom(_))), "{crs:?}");
    assert_eq!(
        report.messages,
        ["coordinate system: the file gives a shift of zero to WGS 84, which is used in place of EPSG:27700's transformation"]
    );
    // A WKT bound to a system whose shift to WGS 84 isn't known (NAD 27's is a grid) can't be
    // read: the code, and the WKT's shift is said to be set aside.
    let nad27 = r#"GEOGCS["NAD27",DATUM["North_American_Datum_1927",SPHEROID["Clarke 1866",6378206.4,294.978698213898]],PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433],AUTHORITY["EPSG","4267"]]"#;
    let unknown = crate::raster::wkt::tests::bound_crs(&bng, nad27, &[375.0, -111.0, 431.0]);
    let mut report = ReadReport::default();
    let (crs, t) = esri_crs(Some(27700), Some(&unknown), &mut report).unwrap();
    assert_eq!(crs, Some(Crs::Epsg(27700)));
    assert!(t.is_some());
    assert_eq!(
        report.messages,
        [
            "coordinate system: the file's own shift to WGS 84 is set aside for EPSG:27700's, because the shift from NAD27 to WGS 84 isn't known"
        ]
    );
    // An Esri code (South America Albers) with a WKT giving another shift: the code, and the
    // WKT's shift is said to be set aside. With no shift, or the code's own, nothing is said.
    let albers = crs_of_code(102033).unwrap();
    let shifted = r#"PROJCS["South_America_Albers_Equal_Area_Conic",GEOGCS["GCS_South_American_1969",DATUM["D_South_American_1969",SPHEROID["GRS_1967_Truncated",6378160.0,298.25],TOWGS84[1,2,3,0,0,0,0]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Albers"],PARAMETER["False_Easting",0.0],PARAMETER["False_Northing",0.0],PARAMETER["Central_Meridian",-60.0],PARAMETER["Standard_Parallel_1",-5.0],PARAMETER["Standard_Parallel_2",-42.0],PARAMETER["Latitude_Of_Origin",-32.0],UNIT["Meter",1.0]]"#;
    let set_aside = "coordinate system: the file's own shift to WGS 84 is set aside for ESRI:102033's, because ArcGIS keeps transformations apart from the system";
    for (wkt, said) in [
        (shifted.to_string(), Some(set_aside)),
        (shifted.replace("TOWGS84[1,2,3,0,0,0,0]", "TOWGS84[0,0,0]"), Some(set_aside)),
        // A bound system's transformation, even one that can't be read.
        (crate::raster::wkt::tests::bound_crs(&shifted.replace(",TOWGS84[1,2,3,0,0,0,0]", ""), nad27, &[1.0, 2.0, 3.0]), Some(set_aside)),
        (shifted.replace(",TOWGS84[1,2,3,0,0,0,0]", ""), None),
        (shifted.replace("TOWGS84[1,2,3,0,0,0,0]", "TOWGS84[-57,1,-41,0,0,0,0]"), None),
    ] {
        let mut report = ReadReport::default();
        assert_eq!(esri_crs(Some(102033), Some(&wkt), &mut report).unwrap().0, Some(albers), "{wkt}");
        assert_eq!(report.messages, Vec::from_iter(said), "{wkt}");
    }
    // A code whose datum is taken for WGS 84 (USA Contiguous Albers, on NAD 83) has no shift: the
    // WKT's is said to be left out, and a shift of zero is the code's own.
    let usa = r#"PROJCS["USA_Contiguous_Albers_Equal_Area_Conic",GEOGCS["GCS_North_American_1983",DATUM["D_North_American_1983",SPHEROID["GRS_1980",6378137.0,298.257222101],TOWGS84[0,0,0]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Albers"],PARAMETER["False_Easting",0.0],PARAMETER["False_Northing",0.0],PARAMETER["Central_Meridian",-96.0],PARAMETER["Standard_Parallel_1",29.5],PARAMETER["Standard_Parallel_2",45.5],PARAMETER["Latitude_Of_Origin",37.5],UNIT["Meter",1.0]]"#;
    for (wkt, said) in [
        (usa.to_string(), None),
        (
            usa.replace("TOWGS84[0,0,0]", "TOWGS84[1,2,3]"),
            Some(
                "coordinate system: the file's own shift to WGS 84 is set aside, because ArcGIS keeps transformations apart from the system, and ESRI:102003 is read without it, so positions may be off by as much as that shift",
            ),
        ),
    ] {
        let mut report = ReadReport::default();
        assert_eq!(esri_crs(Some(102003), Some(&wkt), &mut report).unwrap().0, crs_of_code(102003), "{wkt}");
        assert_eq!(report.messages, Vec::from_iter(said), "{wkt}");
    }
    // On a sphere, a code without a shift changes no datum, not even the ellipsoid, which any shift
    // would: the notice says the ellipsoid, and the shift when it isn't zero, also one that can't be
    // read.
    let mollweide = r#"PROJCS["Sphere_Mollweide",GEOGCS["GCS_Sphere",DATUM["D_Sphere",SPHEROID["Sphere",6371000.0,0.0],TOWGS84[0,0,0]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Mollweide"],PARAMETER["False_Easting",0.0],PARAMETER["False_Northing",0.0],PARAMETER["Central_Meridian",0.0],UNIT["Meter",1.0]]"#;
    let mut report = ReadReport::default();
    assert_eq!(esri_crs(Some(53009), Some(mollweide), &mut report).unwrap().0, crs_of_code(53009));
    assert_eq!(
        report.messages,
        [
            "coordinate system: the file's shift of zero to WGS 84 is set aside, because ArcGIS keeps transformations apart from the system, and ESRI:53009 is read on its own ellipsoid without it, so positions may be off by the difference between that ellipsoid and WGS 84's"
        ]
    );
    let unshifted_mollweide = mollweide.replace(",TOWGS84[0,0,0]", "");
    for wkt in [
        mollweide.replace("TOWGS84[0,0,0]", "TOWGS84[1,2,3]"),
        crate::raster::wkt::tests::bound_crs(&unshifted_mollweide, nad27, &[1.0, 2.0, 3.0]),
    ] {
        let mut report = ReadReport::default();
        assert_eq!(esri_crs(Some(53009), Some(&wkt), &mut report).unwrap().0, crs_of_code(53009), "{wkt}");
        assert_eq!(
            report.messages,
            [
                "coordinate system: the file's own shift to WGS 84 is set aside, because ArcGIS keeps transformations apart from the system, and ESRI:53009 is read on its own ellipsoid without it, so positions may be off by as much as that shift and the difference between that ellipsoid and WGS 84's"
            ],
            "{wkt}"
        );
    }
    let mut report = ReadReport::default();
    esri_crs(Some(53009), Some(&unshifted_mollweide), &mut report).unwrap();
    assert!(report.messages.is_empty(), "{:?}", report.messages);
    // Esri's Web Mercator code is EPSG's too, whose shift is none: a shift of zero is its own.
    let web_mercator = r#"PROJCS["WGS_1984_Web_Mercator_Auxiliary_Sphere",GEOGCS["GCS_WGS_1984",DATUM["D_WGS_1984",SPHEROID["WGS_1984",6378137.0,298.257223563],TOWGS84[0,0,0,0,0,0,0]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Mercator_Auxiliary_Sphere"],PARAMETER["False_Easting",0.0],PARAMETER["False_Northing",0.0],PARAMETER["Central_Meridian",0.0],PARAMETER["Standard_Parallel_1",0.0],PARAMETER["Auxiliary_Sphere_Type",0.0],UNIT["Meter",1.0]]"#;
    assert!(crate::raster::crs::esri_proj4(102100).is_some());
    let mut report = ReadReport::default();
    assert_eq!(esri_crs(Some(102100), Some(web_mercator), &mut report).unwrap().0, Some(Crs::WebMercator));
    assert!(report.messages.is_empty(), "{:?}", report.messages);
    // Mars, under an EPSG code and an Esri one: read as Mars, said, or refused.
    let mars = r#"GEOGCS["GCS_Mars_2015_-_Sphere_Ocentric",DATUM["D_Mars_2015_-_Sphere",SPHEROID["Mars_2015_-_Sphere",3396190.0,0.0]],PRIMEM["Reference_Meridian",0.0],UNIT["Degree",0.0174532925199433]]"#;
    let body = |m: &String| m.contains("belongs to another body than the Earth");
    for code in [4326, 102033] {
        let mut report = ReadReport::default();
        let (crs, _) = esri_crs(Some(code), Some(mars), &mut report).unwrap();
        assert!(crs.is_some_and(Crs::is_planetary), "{code}: {crs:?}");
        assert_eq!(report.messages.iter().filter(|m| body(&m.text)).count(), 1, "{code}: {:?}", report.messages);
        // A method no one knows (Bonne is read, as PROJ 9.8.1 reads it).
        let unread = format!(
            r#"PROJCS["Unheard_Of",{mars},PROJECTION["Unheard_Of"],PARAMETER["Central_Meridian",0.0],PARAMETER["Standard_Parallel_1",30.0],UNIT["Meter",1.0]]"#
        );
        let e = esri_crs(Some(code), Some(&unread), &mut ReadReport::default()).err().unwrap().to_string();
        assert!(e.contains("belongs to another body than the Earth") && e.contains("Unheard_Of"), "{code}: {e}");
    }
}

/// #1678: a grid shift the WKT gives (a WKT2 `BOUNDCRS`'s NTv2 file, or GDAL's WKT1
/// `PROJ4_GRIDS`) isn't applied when the code is read, for an EPSG code, longitude/latitude's and
/// an Esri one alike, and is said once, with the code's own shift used in its place when it has
/// one. Without the grid nothing is said. (An EPSG code without a shift of its own, such as
/// NAD 27's, gives way to the WKT, whose grid `read_wkt` says.)
#[test]
fn a_code_says_a_grid_its_wkt_gives() {
    use crate::raster::wkt::tests::BOUND_BY_A_GRID;
    use crate::source::tests::{ED50_UTM31, ed50_utm31_bound};
    let conus = "coordinate system: the definition's NTv2 transformation to WGS 84 (conus) isn't applied; ESRI:102033's own shift to WGS 84 is used in its place, so positions may be off by the difference between the two";
    let spain = "coordinate system: the definition's NTv2 transformation to WGS 84 (SPED2ETV2.gsb) isn't applied; EPSG:23031's own shift to WGS 84 is used in its place, so positions may be off by the difference between the two";
    let ntv2 = "coordinate system: the definition's grid shift (ntv2_0.gsb) isn't applied; ESRI:102033's own shift to WGS 84 is used in its place, so positions may be off by the difference between the two";
    let unshifted =
        "coordinate system: the definition's grid shift (ntv2_0.gsb) isn't applied, so positions may be off by as much as that shift";
    // South America Albers (an Esri code), plain and with GDAL's WKT1 grid on its datum; with a
    // Helmert shift beside the grid only the grid is said.
    let albers = r#"PROJCS["South_America_Albers_Equal_Area_Conic",GEOGCS["GCS_South_American_1969",DATUM["D_South_American_1969",SPHEROID["GRS_1967_Truncated",6378160.0,298.25]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Albers"],PARAMETER["False_Easting",0.0],PARAMETER["False_Northing",0.0],PARAMETER["Central_Meridian",-60.0],PARAMETER["Standard_Parallel_1",-5.0],PARAMETER["Standard_Parallel_2",-42.0],PARAMETER["Latitude_Of_Origin",-32.0],UNIT["Meter",1.0]]"#;
    let gridded = albers.replace("298.25]]", r#"298.25],EXTENSION["PROJ4_GRIDS","ntv2_0.gsb"]]"#);
    let both = albers.replace("298.25]]", r#"298.25],TOWGS84[1,2,3,0,0,0,0],EXTENSION["PROJ4_GRIDS","ntv2_0.gsb"]]"#);
    assert!(gridded != albers && both != albers);
    // USA Contiguous Albers (an Esri code on NAD 83, without a shift of its own) and
    // longitude/latitude (EPSG:4326) with a grid on their datum.
    let usa = r#"PROJCS["USA_Contiguous_Albers_Equal_Area_Conic",GEOGCS["GCS_North_American_1983",DATUM["D_North_American_1983",SPHEROID["GRS_1980",6378137.0,298.257222101]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Albers"],PARAMETER["False_Easting",0.0],PARAMETER["False_Northing",0.0],PARAMETER["Central_Meridian",-96.0],PARAMETER["Standard_Parallel_1",29.5],PARAMETER["Standard_Parallel_2",45.5],PARAMETER["Latitude_Of_Origin",37.5],UNIT["Meter",1.0]]"#;
    let usa_gridded = usa.replace("298.257222101]]", r#"298.257222101],EXTENSION["PROJ4_GRIDS","ntv2_0.gsb"]]"#);
    let wgs84 = r#"GEOGCS["GCS_WGS_1984",DATUM["D_WGS_1984",SPHEROID["WGS_1984",6378137.0,298.257223563]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]]"#;
    let wgs84_gridded = wgs84.replace("298.257223563]]", r#"298.257223563],EXTENSION["PROJ4_GRIDS","ntv2_0.gsb"]]"#);
    assert!(usa_gridded != usa && wgs84_gridded != wgs84);
    for (code, wkt, said) in [
        // ED50 / UTM zone 31N, an EPSG code with a Helmert shift of its own, bound by Spain's
        // grid from ED50.
        (23031, ed50_utm31_bound().as_str(), Some(spain)),
        (23031, ED50_UTM31, None),
        (102033, BOUND_BY_A_GRID, Some(conus)),
        (102033, gridded.as_str(), Some(ntv2)),
        (102033, both.as_str(), Some(ntv2)),
        (102033, albers, None),
        (102003, usa_gridded.as_str(), Some(unshifted)),
        (102003, usa, None),
        (4326, wgs84_gridded.as_str(), Some(unshifted)),
        (4326, wgs84, None),
    ] {
        let mut report = ReadReport::default();
        // Longitude/latitude is `None`.
        let read = crs_of_code(code).filter(|c| *c != Crs::LonLat);
        assert_eq!(esri_crs(Some(code), Some(wkt), &mut report).unwrap().0, read, "{code} {wkt}");
        assert_eq!(report.messages, Vec::from_iter(said), "{code} {wkt}");
    }
}

/// #1486: the projected placement cases, as a feature class's WKT. (A File Geodatabase holds Esri
/// WKT1, which has no bound systems.)
#[test]
fn placement_cases_are_placed() {
    for PlacementCase { what, wkt, xy, lonlat } in placement_cases() {
        if !wkt.starts_with("PROJCS[") {
            continue;
        }
        // `F::Shape` takes a `&'static str`, as its other uses are constants: the few cases' text
        // is leaked for the test's run.
        let fields = [F::Oid, F::Shape { z: false, wkt: wkt.leak() }];
        let rows = vec![vec![V::Blob(polygon(&square(xy.x, xy.y, 1.0)))]];
        let files = geodatabase(vec![("T", table(4, &fields, &rows))]);
        let layers = read_files(&files).unwrap();
        let (ds, report) = &layers[0];
        assert!(report.messages.is_empty(), "{what}: {:?}", report.messages);
        let ds = ds.in_lonlat().unwrap().into_owned();
        let Some(Geometry::Polygon(p)) = &ds.features[0].geometry else { panic!("{what}: {:?}", ds.features[0].geometry) };
        // The corner at the point (stored to the millimetre) is within a centimetre of it.
        let off = p[0][0].iter().map(|c| metres_apart(*c, lonlat)).fold(f64::INFINITY, f64::min);
        assert!(off < 0.01, "{what}: {off} m off");
    }
}

/// All four FileGDB temporal wire kinds go through the table reader, including a stored invalid
/// date and a true null. Local times and explicit offsets retain different semantics.
#[test]
fn filegdb_temporal_fields_keep_days_offsets_and_null_notices() {
    let fields = [F::Date("day", 14), F::Date("local", 5), F::Date("offset", 16), F::Date("clock", 15)];
    let (t, x) = table(
        0,
        &fields,
        &[
            vec![
                V::Date(Some((36_526.0, None))),
                V::Date(Some((36_526.5, None))),
                V::Date(Some((36_526.5, Some(-300)))),
                V::Date(Some((0.5, None))),
            ],
            vec![V::Date(Some((f64::NAN, None))), V::Date(None), V::Date(None), V::Date(None)],
        ],
    );
    let (ds, report) = read_table("dates", &t, &x, None).unwrap().unwrap();
    assert_eq!(
        ds.fields.iter().map(|f| f.ty).collect::<Vec<_>>(),
        [FieldType::Date, FieldType::DateTime, FieldType::DateTime, FieldType::Text]
    );
    assert_eq!(
        ds.features[0].attrs.iter().map(ToString::to_string).collect::<Vec<_>>(),
        ["2000-01-01", "2000-01-01T12:00:00", "2000-01-01T12:00:00-05:00", "12:00:00"]
    );
    assert!(report.unread_dates.unwrap().starts_with("1 value"));
    assert!(ds.features[1].attrs.iter().all(Value::is_null));
    assert_eq!(gdb_datetime(25_569.5, None).to_string(), "1970-01-01T12:00:00");
    assert!(gdb_datetime(25_569.0, Some(i16::MIN)).is_null());
    assert!(gdb_datetime(1e100, None).is_null());
    // Exercise the reader/report boundary as well as the shared serial helper.
    for (serial, rounded) in [(45_000.5, false), (45_000.1, true)] {
        let (t, x) = table(0, &[F::Date("moment", 5)], &[vec![V::Date(Some((serial, None)))]]);
        let (_, report) = read_table("dates", &t, &x, None).unwrap().unwrap();
        let notices: Vec<_> = report.messages.iter().filter(|n| n.contains("rounded to the nearest microsecond")).collect();
        assert_eq!(notices.len(), usize::from(rounded), "{serial}: {report:?}");
        if rounded {
            assert!(notices[0].contains("1 serial date") && notices[0].contains("moment"));
            assert_eq!(notices[0].loss, Some(crate::loss::LossCode::ChangedRounded));
        }
    }
}
