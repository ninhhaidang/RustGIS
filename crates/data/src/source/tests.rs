use super::*;
use crate::{Dataset, Field, FieldType, Geometry, Value, wkb};
use rustgis_geom::{Coord, lonlat_to_merc};
use std::path::PathBuf;
use std::sync::Arc;

#[test]
fn file_geodatabase_names() {
    assert!(is_gdb_name("/x/roads.gdb") && is_gdb_name("Roads.GDB/") && is_gdb_name(r"C:\x\roads.gdb\"));
    assert!(!is_gdb_name("roads.gdbtable") && !is_gdb_name("gdb") && !is_gdb_name("roads.shp"));
    // A file picked inside a geodatabase stands for the geodatabase.
    assert_eq!(gdb_of(Path::new("/x/roads.gdb/a00000001.gdbtable")), Some(Path::new("/x/roads.gdb")));
    assert_eq!(gdb_of(Path::new("/x/roads.gdb/gdb")), Some(Path::new("/x/roads.gdb")));
    assert_eq!(gdb_of(Path::new("/x/roads.gdb/a0000000A.spx")), Some(Path::new("/x/roads.gdb")));
    assert_eq!(gdb_of(Path::new("/x/roads.gdb/a00000004.CatItemsByType.atx")), Some(Path::new("/x/roads.gdb")));
    assert_eq!(gdb_of(Path::new("/x/roads.gdb/timestamps")), Some(Path::new("/x/roads.gdb")));
    // A file named like a table, but of another kind, is itself.
    assert_eq!(gdb_of(Path::new("/x/roads.gdb/a00000001.csv")), None);
    assert_eq!(gdb_of(Path::new("/x/roads.gdb/a0000000b.bak")), None);
    // Other files kept in the folder are themselves.
    assert_eq!(gdb_of(Path::new("/x/roads.gdb/readme.txt")), None);
    assert_eq!(gdb_of(Path::new("/x/roads.gdb/export.csv")), None);
    assert_eq!(gdb_of(Path::new("/x/roads.gdb/a1.gdbtable")), None);
    assert_eq!(gdb_of(Path::new("/x/roads.gdb/x0000000a.gdbtable")), None);
    assert_eq!(gdb_of(Path::new("/x/roads.gdb")), None);
    assert_eq!(gdb_of(Path::new("/x/roads.gdb/")), None);
    assert_eq!(gdb_of(Path::new("/x/roads.shp")), None);
    assert_eq!(gdb_of(Path::new("a00000001.gdbtable")), None);
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn tls_provider_builds_client_config() {
    tls_provider();
    let _ = rustls::ClientConfig::builder();
}

/// 30 × 20 points over the world, named and numbered.
fn points() -> Vec<(Coord, String, i64)> {
    let mut v = Vec::new();
    for i in 0..30 {
        for j in 0..20 {
            let c = Coord::new(-174.0 + 12.0 * i as f64, -76.0 + 8.0 * j as f64);
            v.push((c, format!("p{i}_{j}"), (i * 20 + j) as i64));
        }
    }
    v
}

fn in_box(e: &Extent) -> usize {
    points().iter().filter(|(c, ..)| e.contains(*c)).count()
}

fn dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("rustgis-source-tests-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write_parquet(path: &Path, covering: bool) {
    write_parquet_typed(path, covering, r#"["Point"]"#);
}

/// [`write_parquet`], with the `geo` metadata's `geometry_types` (a JSON list) given.
fn write_parquet_typed(path: &Path, covering: bool, types: &str) {
    use arrow_array::{BinaryArray, Float32Array, Int64Array, RecordBatch, StringArray, StructArray};
    use arrow_schema::{DataType, Field as AField, Schema};
    let pts = points();
    let wkbs: Vec<Vec<u8>> = pts.iter().map(|(c, ..)| wkb::encode(&Geometry::Point(*c))).collect();
    let bbox_fields = ["xmin", "ymin", "xmax", "ymax"].map(|n| Arc::new(AField::new(n, DataType::Float32, false)));
    let coord = |f: fn(&Coord) -> f64| Arc::new(Float32Array::from(pts.iter().map(|(c, ..)| f(c) as f32).collect::<Vec<_>>()));
    let bbox =
        StructArray::new(bbox_fields.iter().cloned().collect(), vec![coord(|c| c.x), coord(|c| c.y), coord(|c| c.x), coord(|c| c.y)], None);
    let mut fields = vec![
        AField::new("name", DataType::Utf8, false),
        AField::new("value", DataType::Int64, false),
        AField::new("geometry", DataType::Binary, false),
    ];
    let mut cols: Vec<arrow_array::ArrayRef> = vec![
        Arc::new(StringArray::from(pts.iter().map(|p| p.1.clone()).collect::<Vec<_>>())),
        Arc::new(Int64Array::from(pts.iter().map(|p| p.2).collect::<Vec<_>>())),
        Arc::new(BinaryArray::from(wkbs.iter().map(|w| w.as_slice()).collect::<Vec<_>>())),
    ];
    let geo = if covering {
        fields.push(AField::new("geometry_bbox", DataType::Struct(bbox_fields.iter().cloned().collect()), false));
        cols.push(Arc::new(bbox));
        format!(
            r#"{{"version":"1.1.0","primary_column":"geometry","columns":{{"geometry":{{"encoding":"WKB","geometry_types":{types},
            "covering":{{"bbox":{{"xmin":["geometry_bbox","xmin"],"ymin":["geometry_bbox","ymin"],"xmax":["geometry_bbox","xmax"],"ymax":["geometry_bbox","ymax"]}}}}}}}}}}"#
        )
    } else {
        format!(
            r#"{{"version":"1.0.0","primary_column":"geometry","columns":{{"geometry":{{"encoding":"WKB","geometry_types":{types}}}}}}}"#
        )
    };
    let schema = Arc::new(Schema::new(fields));
    let batch = RecordBatch::try_new(schema.clone(), cols).unwrap();
    let props = ::parquet::file::properties::WriterProperties::builder()
        .set_max_row_group_row_count(Some(50))
        .set_key_value_metadata(Some(vec![::parquet::file::metadata::KeyValue::new("geo".to_string(), geo.to_string())]))
        .build();
    let mut w = ::parquet::arrow::ArrowWriter::try_new(std::fs::File::create(path).unwrap(), schema, Some(props)).unwrap();
    w.write(&batch).unwrap();
    w.close().unwrap();
}

fn write_fgb(path: &Path) {
    use flatgeobuf::geozero::{ColumnValue, PropertyProcessor};
    use flatgeobuf::{ColumnType, FgbWriter, GeometryType as G};
    let mut w = FgbWriter::create("grid", G::Point).unwrap();
    w.add_column("name", ColumnType::String, |_, _| {});
    w.add_column("value", ColumnType::Long, |_, _| {});
    for (c, name, v) in points() {
        let g = flatgeobuf::geozero::wkb::Wkb(wkb::encode(&Geometry::Point(c)));
        w.add_feature_geom(g, |f| {
            f.property(0, "name", &ColumnValue::String(&name)).unwrap();
            f.property(1, "value", &ColumnValue::Long(v)).unwrap();
        })
        .unwrap();
    }
    w.write(std::fs::File::create(path).unwrap()).unwrap();
}

/// A GeoPackage of the points in Web Mercator, with an R-tree index (or without).
fn write_gpkg(path: &Path, rtree: bool) {
    let _ = std::fs::remove_file(path);
    let c = rusqlite::Connection::open(path).unwrap();
    c.execute_batch(
        "CREATE TABLE gpkg_spatial_ref_sys (srs_name TEXT, srs_id INTEGER PRIMARY KEY, organization TEXT, organization_coordsys_id INTEGER, definition TEXT, description TEXT);
         INSERT INTO gpkg_spatial_ref_sys VALUES ('Web Mercator', 3857, 'EPSG', 3857, '', '');
         CREATE TABLE gpkg_contents (table_name TEXT PRIMARY KEY, data_type TEXT, identifier TEXT, description TEXT, last_change TEXT, min_x DOUBLE, min_y DOUBLE, max_x DOUBLE, max_y DOUBLE, srs_id INTEGER);
         CREATE TABLE gpkg_geometry_columns (table_name TEXT, column_name TEXT, geometry_type_name TEXT, srs_id INTEGER, z TINYINT, m TINYINT);
         INSERT INTO gpkg_contents (table_name, data_type, srs_id) VALUES ('grid \"q\"', 'features', 3857);
         INSERT INTO gpkg_geometry_columns VALUES ('grid \"q\"', 'geom', 'POINT', 3857, 0, 0);
         CREATE TABLE \"grid \"\"q\"\"\" (fid INTEGER PRIMARY KEY AUTOINCREMENT, geom BLOB, name TEXT, value MEDIUMINT, ok BOOLEAN);",
    )
    .unwrap();
    if rtree {
        c.execute_batch("CREATE VIRTUAL TABLE \"rtree_grid \"\"q\"\"_geom\" USING rtree(id, minx, maxx, miny, maxy);").unwrap();
    }
    // One transaction: a commit per row is slow on Windows (minutes for the fixture).
    let tx = c.unchecked_transaction().unwrap();
    for (i, (p, name, v)) in points().into_iter().enumerate() {
        let m = lonlat_to_merc(p);
        // A header with an xy envelope, then WKB.
        let mut blob = b"GP\x00\x03".to_vec();
        blob.extend_from_slice(&3857i32.to_le_bytes());
        for x in [m.x, m.x, m.y, m.y] {
            blob.extend_from_slice(&x.to_le_bytes());
        }
        blob.extend(wkb::encode(&Geometry::Point(m)));
        let fid = i as i64 + 1;
        tx.execute("INSERT INTO \"grid \"\"q\"\"\" VALUES (?1, ?2, ?3, ?4, ?5)", rusqlite::params![fid, blob, name, v, v % 2]).unwrap();
        if rtree {
            tx.execute("INSERT INTO \"rtree_grid \"\"q\"\"_geom\" VALUES (?1, ?2, ?3, ?4, ?5)", rusqlite::params![fid, m.x, m.x, m.y, m.y])
                .unwrap();
        }
    }
    tx.commit().unwrap();
}

/// The same checks for every provider.
fn check(src: &dyn FeatureSource, expect_indexed: bool) {
    let info = src.info();
    assert_eq!(info.geometry_type, GeometryType::Point, "{}", info.format);
    assert_eq!(info.count, Some(600), "{}", info.format);
    assert_eq!(info.indexed, expect_indexed, "{}", info.format);
    assert!(info.fields.iter().any(|f| f.name == "name" && f.ty == FieldType::Text), "{:?}", info.fields);
    let vi = info.fields.iter().position(|f| f.name == "value").unwrap();
    assert_eq!(info.fields[vi].ty, FieldType::Integer);

    // Everything.
    let all = src.read(&Request::all()).unwrap();
    assert_eq!(all.features.len(), 600, "{}", info.format);
    assert!(all.complete);
    let f = all.features.iter().find(|f| f.attrs.contains(&Value::Text("p3_4".into()))).unwrap();
    let c = match f.geometry {
        Some(Geometry::Point(c)) => c,
        _ => panic!("not a point"),
    };
    assert!((c.x - (-138.0)).abs() < 1e-4 && (c.y - (-44.0)).abs() < 1e-4, "{c:?} in {}", info.format);
    assert_eq!(f.attrs[vi], Value::Int(64));
    // Ids stay put between reads.
    let near = src.read(&Request { extent: Some(Extent::new(-140.0, -46.0, -130.0, -40.0)), limit: 100 }).unwrap();
    let g = near.features.iter().find(|g| g.attrs.contains(&Value::Text("p3_4".into()))).unwrap();
    assert_eq!(g.id, f.id, "{}", info.format);

    // An extent: exactly the points in it.
    let e = Extent::new(-60.0, -10.0, 40.0, 50.0);
    let got = src.read(&Request { extent: Some(e), limit: 10_000 }).unwrap();
    assert_eq!(got.features.len(), in_box(&e), "{}", info.format);
    assert!(got.complete);
    assert!(got.features.iter().all(|f| f.geometry.as_ref().is_some_and(|g| e.contains(g.extent().center()))));
    // Every feature keeps the id a whole read gives it.
    let id_of: std::collections::HashMap<String, u64> =
        all.features.iter().map(|f| (f.attrs[0].to_string() + &f.attrs[1].to_string(), f.id)).collect();
    for f in &got.features {
        assert_eq!(id_of.get(&(f.attrs[0].to_string() + &f.attrs[1].to_string())), Some(&f.id), "{}", info.format);
    }

    // A limit: a sample spread over the whole extent, not the first rows.
    let s = src.read(&Request { extent: None, limit: 60 }).unwrap();
    assert!(!s.complete);
    assert!(s.features.len() <= 60 && s.features.len() >= 50, "{} of {}", s.features.len(), info.format);
    let xs: Vec<f64> = s.features.iter().filter_map(|f| f.geometry.as_ref()).map(|g| g.extent().min_x).collect();
    assert!(xs.iter().any(|x| *x < -100.0) && xs.iter().any(|x| *x > 100.0), "{} sample isn't spread", info.format);
    assert!(s.matched >= 600);

    // An extent outside the data.
    let none = src.read(&Request { extent: Some(Extent::new(-179.9, 80.0, -179.0, 84.0)), limit: 100 }).unwrap();
    assert!(none.features.is_empty());

    // A whole read into a dataset.
    let (ds, report) = materialize(src).unwrap();
    assert_eq!(ds.features.len(), 600);
    assert!(report.messages.is_empty());
}

#[test]
fn materialize_refuses_oversized_and_incomplete_sources() {
    use std::sync::atomic::{AtomicBool, Ordering};

    struct Source {
        info: SourceInfo,
        batch: Option<Batch>,
        read: AtomicBool,
    }
    impl FeatureSource for Source {
        fn info(&self) -> &SourceInfo {
            &self.info
        }

        fn read(&self, _: &Request) -> Result<Batch> {
            self.read.store(true, Ordering::Relaxed);
            self.batch.clone().ok_or_else(|| DataError::Format("read reached".into()))
        }
    }
    let mut src = Source {
        info: SourceInfo {
            name: "large".into(),
            format: "Test",
            geometry_type: GeometryType::None,
            fields: vec![],
            extent: Extent::EMPTY,
            count: Some(MAX_LIMIT as u64 + 1),
            crs: "EPSG:4326".into(),
            indexed: false,
            notes: vec![],
            zm_dropped: false,
        },
        batch: None,
        read: AtomicBool::new(false),
    };
    let error = materialize(&src).unwrap_err().to_string();
    assert!(error.contains(&(MAX_LIMIT + 1).to_string()) && error.contains(&MAX_LIMIT.to_string()), "{error}");
    assert!(!src.read.load(Ordering::Relaxed), "an oversized source must be refused before reading");

    // Exactly the limit is allowed through to the provider; no huge allocation needed to check.
    src.info.count = Some(MAX_LIMIT as u64);
    assert!(materialize(&src).unwrap_err().to_string().contains("read reached"));
    assert!(src.read.load(Ordering::Relaxed));

    // Unknown or stale metadata must not allow an incomplete batch to become a dataset.
    for count in [None, Some(1)] {
        src.info.count = count;
        src.batch = Some(Batch { matched: MAX_LIMIT as u64 + 1, complete: false, ..Batch::default() });
        let error = materialize(&src).unwrap_err().to_string();
        assert!(error.contains(&(MAX_LIMIT + 1).to_string()) && error.contains(&MAX_LIMIT.to_string()), "{error}");
    }
    // Even an incomplete answer below the cap isn't a whole read.
    src.batch = Some(Batch { matched: 1, complete: false, ..Batch::default() });
    let error = materialize(&src).unwrap_err().to_string();
    assert!(error.contains("large") && error.contains(&MAX_LIMIT.to_string()), "{error}");

    // Unreadable rows reported by a complete scan still follow ReadReport's skip contract.
    src.batch = Some(Batch { matched: 1, complete: true, skipped: 1, ..Batch::default() });
    let (ds, report) = materialize(&src).unwrap();
    assert!(ds.features.is_empty());
    assert_eq!(report.skipped, 1);
}

/// A whole read of a source that can be read a chunk at a time holds what one read gives, and is
/// refused once its features outgrow the memory budget (#1111).
#[test]
fn whole_reads_by_chunk_stay_within_the_memory_budget() {
    let d = dir().join("budget");
    std::fs::create_dir_all(&d).unwrap();
    let mut ds = Dataset::new("lines", GeometryType::Polyline, vec![Field::new("name", FieldType::Text)]);
    for i in 0..200 {
        let x = f64::from(i) * 0.01;
        ds.push_next(Some(Geometry::Line(vec![vec![Coord::new(x, 1.0), Coord::new(x, 2.0)]])), vec![Value::Text(format!("line {i}"))]);
    }
    for ext in ["gpkg", "fgb", "parquet", "shp"] {
        let p = d.join(format!("lines.{ext}"));
        let _ = std::fs::remove_file(&p);
        crate::io::write_path(&ds, &p).unwrap();
        let src = open_path(&p, None).unwrap();
        assert!(src.visits_chunks(), "{ext}");
        let once = src.read(&Request::all()).unwrap().features;
        let (whole, report) = materialize_within(src.as_ref(), MAX_READ_BYTES).unwrap();
        assert_eq!(whole.features.to_vec(), once, "{ext}");
        assert_eq!(report.skipped, 0, "{ext}");
        let half = chunk_memory::features_bytes(&once) as u64 / 2;
        let error = materialize_within(src.as_ref(), half).unwrap_err().to_string();
        assert!(error.contains("clip or filter it first"), "{ext}: {error}");
    }
}

#[test]
fn geoparquet_with_and_without_a_covering() {
    let d = dir();
    let p = d.join("grid.parquet");
    write_parquet(&p, true);
    let src = open_path(&p, None).unwrap();
    check(src.as_ref(), true);
    // The covering's row-group statistics give the extent.
    assert!(src.info().extent.min_x <= -174.0 && src.info().extent.max_x >= 174.0);
    let bytes = std::fs::read(&p).unwrap();
    check(open_bytes("grid.parquet", bytes, None).unwrap().as_ref(), true);

    let p2 = d.join("plain.parquet");
    write_parquet(&p2, false);
    check(open_path(&p2, None).unwrap().as_ref(), false);
}

/// Rewrites a valid file's footer, preserving its pages and schema.
fn parquet_with_bad_chunk(original: &[u8], column: usize, kind: usize) -> Vec<u8> {
    use ::parquet::file::reader::{FileReader, SerializedFileReader};
    let reader = SerializedFileReader::new(bytes::Bytes::copy_from_slice(original)).unwrap();
    let md = reader.metadata().clone();
    let mut groups = md.row_groups().to_vec();
    let mut cols = groups[0].columns().to_vec();
    let data = cols[column].data_page_offset();
    let builder = cols[column].clone().into_builder();
    cols[column] = match kind {
        0 => builder.set_dictionary_page_offset(Some(-1)),
        1 => builder.set_dictionary_page_offset(None).set_data_page_offset(-1),
        2 => builder.set_total_compressed_size(-1),
        3 => builder.set_dictionary_page_offset(Some(data + 1)),
        _ => builder.set_dictionary_page_offset(None).set_data_page_offset(i64::MAX).set_total_compressed_size(1),
    }
    .build()
    .unwrap();
    groups[0] = groups[0].clone().into_builder().set_column_metadata(cols).build().unwrap();
    let md = md.into_builder().set_row_groups(groups).build();
    let end = original.len() - 8;
    let len = u32::from_le_bytes(original[end..end + 4].try_into().unwrap()) as usize;
    let mut bytes = original[..end - len].to_vec();
    ::parquet::file::metadata::ParquetMetaDataWriter::new(&mut bytes, &md).finish().unwrap();
    bytes
}

#[test]
fn geoparquet_refuses_invalid_column_ranges() {
    let path = dir().join("bad-chunk.parquet");
    write_parquet(&path, true);
    let original = std::fs::read(&path).unwrap();
    // Attribute, geometry and bbox columns all pass through the same metadata check.
    for col in [0, 2, 3] {
        for kind in 0..5 {
            let bytes = parquet_with_bad_chunk(&original, col, kind);
            let error = open_bytes("bad.parquet", bytes.clone(), None).err().unwrap().to_string();
            assert!(error.contains("column chunk"), "{col}/{kind}: {error}");
            assert!(crate::io::read_layers("bad.parquet", &bytes).is_err());
            std::fs::write(&path, &bytes).unwrap();
            assert!(matches!(open_path(&path, None), Err(DataError::Format(_))));
            let url = format!("https://test.example/bad-chunk-{col}-{kind}.parquet");
            remote::testing::serve(&url, bytes);
            assert!(matches!(open_url_blocking(&url), Err(DataError::Format(_))));
        }
    }
}

/// A dictionary page whose header says it holds two billion values is refused before the `parquet`
/// crate makes room for them (64 GB for a binary column; fuzzing, #1498). The page patched is the
/// file's last column chunk, so no other chunk moves.
#[test]
fn geoparquet_refuses_dictionary_counts_its_page_cannot_hold() {
    use ::parquet::file::reader::{FileReader, SerializedFileReader};
    let path = dir().join("dictionary-count.parquet");
    write_parquet(&path, false);
    let original = std::fs::read(&path).unwrap();
    assert!(crate::io::read_layers("x.parquet", &original).is_ok());
    let md = SerializedFileReader::new(bytes::Bytes::copy_from_slice(&original)).unwrap().metadata().clone();
    let mut groups = md.row_groups().to_vec();
    let last = groups.len() - 1;
    let mut cols = groups[last].columns().to_vec();
    let geometry = cols.len() - 1;
    let start = cols[geometry].dictionary_page_offset().unwrap() as usize;
    // In the page header, the dictionary page header (field 7, a struct) and its first field, the
    // count (an i32 varint).
    let at = start + original[start..].windows(2).position(|w| w == [0x4c, 0x15]).unwrap() + 2;
    let end = at + original[at..].iter().position(|b| b & 0x80 == 0).unwrap() + 1;
    let count = [0x80, 0xd0, 0xac, 0xf3, 0x0e]; // zigzag(2_000_000_000)
    let grown = (count.len() - (end - at)) as i64;
    let mut bytes = [&original[..at], &count, &original[end..start + cols[geometry].compressed_size() as usize]].concat();
    let size = cols[geometry].compressed_size() + grown;
    cols[geometry] = cols[geometry].clone().into_builder().set_total_compressed_size(size).build().unwrap();
    groups[last] = groups[last].clone().into_builder().set_column_metadata(cols).build().unwrap();
    let md = md.into_builder().set_row_groups(groups).build();
    ::parquet::file::metadata::ParquetMetaDataWriter::new(&mut bytes, &md).finish().unwrap();
    let error = crate::io::read_layers("x.parquet", &bytes).err().unwrap().to_string();
    assert!(error.contains("dictionary page says it holds 2000000000 values"), "{error}");
}

#[test]
fn flatgeobuf_refuses_feature_sizes_before_allocating() {
    let path = dir().join("bad-feature-size.fgb");
    write_fgb(&path);
    let original = std::fs::read(&path).unwrap();
    let h = flatgeobuf::size_prefixed_root_as_header(&original[8..]).unwrap();
    let index = flatgeobuf::packed_r_tree::PackedRTree::index_size(h.features_count() as usize, h.index_node_size());
    let start = 12 + u32::from_le_bytes(original[8..12].try_into().unwrap()) as usize + index;
    // Both the first prefix (read by selection) and a later prefix (read by next()).
    let second = start + 4 + u32::from_le_bytes(original[start..start + 4].try_into().unwrap()) as usize;
    for (which, offset) in [start, second].into_iter().enumerate() {
        let mut bytes = original.clone();
        bytes[offset..offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        std::fs::write(&path, &original).unwrap();
        let src = open_path(&path, None).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        for extent in [None, Some(Extent::WORLD_LONLAT)] {
            let error = src.read(&Request { extent, ..Request::all() }).unwrap_err().to_string();
            assert!(error.contains("feature size"), "{error}");
            let error = src.visit_chunks(extent, 1 << 20, &mut |_| Ok(())).unwrap_err().to_string();
            assert!(error.contains("feature size"), "{error}");
        }
        let src = open_bytes("bad.fgb", bytes.clone(), None).unwrap();
        assert!(src.read(&Request::all()).is_err());
        assert!(src.visit_chunks(None, 1 << 20, &mut |_| Ok(())).is_err());
        assert!(crate::io::read_layers("bad.fgb", &bytes).is_err());
        let url = format!("https://test.example/bad-feature-{which}.fgb");
        remote::testing::serve(&url, bytes);
        let src = open_driven(&url);
        assert!(src.read(&Request::all()).is_err());
    }
}

#[test]
fn flatgeobuf_refuses_impossible_index_counts() {
    let path = dir().join("impossible-index.fgb");
    write_fgb(&path);
    let original = std::fs::read(&path).unwrap();
    let header = flatgeobuf::size_prefixed_root_as_header(&original[8..]).unwrap();
    let slot = header._tab.vtable().get(flatgeobuf::Header::VT_FEATURES_COUNT) as usize;
    assert_ne!(slot, 0);
    let offset = 8 + header._tab.loc() + slot;
    for count in [1u64 << 63, u64::MAX, (1u64 << 40) + 1] {
        let mut bytes = original.clone();
        bytes[offset..offset + 8].copy_from_slice(&count.to_le_bytes());
        assert!(matches!(open_bytes("impossible.fgb", bytes.clone(), None), Err(DataError::Format(_))));
        assert!(crate::io::read_layers("impossible.fgb", &bytes).is_err());
        std::fs::write(&path, &original).unwrap();
        let opened = open_path(&path, None).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        assert!(matches!(open_path(&path, None), Err(DataError::Format(_))));
        // A path is reopened on every request: validate again if its file changed.
        for extent in [None, Some(Extent::WORLD_LONLAT)] {
            assert!(matches!(opened.read(&Request { extent, ..Request::all() }), Err(DataError::Format(_))));
        }
        let url = format!("https://test.example/impossible-index-{count}.fgb");
        remote::testing::serve(&url, bytes);
        assert!(matches!(open_url_blocking(&url), Err(DataError::Format(_))));
    }
}

#[test]
fn flatgeobuf() {
    let p = dir().join("grid.fgb");
    write_fgb(&p);
    let src = open_path(&p, None).unwrap();
    check(src.as_ref(), true);
    assert_eq!(src.info().name, "grid");
    check(open_bytes("grid.fgb", std::fs::read(&p).unwrap(), None).unwrap().as_ref(), true);
}

#[test]
fn geopackage_reprojects_and_uses_its_rtree() {
    let d = dir();
    let p = d.join("grid.gpkg");
    write_gpkg(&p, true);
    assert_eq!(layers(&p).unwrap(), vec!["grid \"q\"".to_string()]);
    let src = open_path(&p, Some("GRID \"Q\"")).unwrap();
    assert_eq!(src.info().crs, "EPSG:3857");
    check(src.as_ref(), true);
    // Booleans from integers.
    let all = src.read(&Request::all()).unwrap();
    let ok = src.info().fields.iter().position(|f| f.name == "ok").unwrap();
    assert!(all.features.iter().any(|f| f.attrs[ok] == Value::Bool(true)));
    check(open_bytes("grid.gpkg", std::fs::read(&p).unwrap(), None).unwrap().as_ref(), true);

    let p2 = d.join("scan.gpkg");
    write_gpkg(&p2, false);
    check(open_path(&p2, None).unwrap().as_ref(), false);
    assert!(open_path(&p2, Some("missing")).is_err());
}

/// The browser's GeoPackage reader (pure Rust) gives what SQLite gives, without the R-tree.
#[test]
fn geopackage_in_memory() {
    let p = dir().join("mem.gpkg");
    write_gpkg(&p, true);
    let bytes = Shared(Arc::new(std::fs::read(&p).unwrap()));
    assert_eq!(gpkg_mem::layers(bytes.clone()).unwrap(), vec!["grid \"q\"".to_string()]);
    let src = gpkg_mem::GpkgMemSource::open_shared(bytes.clone(), Some("GRID \"Q\"")).unwrap();
    assert_eq!(src.info().crs, "EPSG:3857");
    check(&src, false);
    // The same features, ids and values as SQLite reads.
    let sql = open_path(&p, None).unwrap();
    let (mut a, mut b) = (src.read(&Request::all()).unwrap().features, sql.read(&Request::all()).unwrap().features);
    a.sort_by_key(|f| f.id);
    b.sort_by_key(|f| f.id);
    assert_eq!(a, b);
    // SQLite's R-tree keeps 32-bit floats; the headers' envelopes are exact.
    let (e, f) = (src.info().extent, sql.info().extent);
    assert!([e.min_x - f.min_x, e.min_y - f.min_y, e.max_x - f.max_x, e.max_y - f.max_y].iter().all(|d| d.abs() < 1e-4), "{e:?} {f:?}");
    assert!(gpkg_mem::GpkgMemSource::open_shared(bytes.clone(), Some("missing")).is_err());
    // Every truncation is an error or a short read, never a panic.
    for n in (0..bytes.0.len()).step_by(131) {
        if let Ok(s) = gpkg_mem::GpkgMemSource::open_shared(Shared(Arc::new(bytes.0[..n].to_vec())), None) {
            let _ = s.read(&Request::all());
        }
    }
}

#[test]
fn geopackage_in_memory_handles_mutated_headers() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("mutated.gpkg");
    write_gpkg(&path, false);
    let original = std::fs::read(path).unwrap();
    let mut offsets: Vec<usize> = (0..100).collect();
    // SQLite pages have a short header; exercise the beginning of each one.
    for page in (4096..original.len()).step_by(4096) {
        offsets.extend(page..(page + 12).min(original.len()));
    }
    // Exercise flags and envelope metadata in actual GeoPackage geometry blobs.
    let geometries: Vec<_> =
        original.windows(4).enumerate().filter_map(|(i, bytes)| (bytes == b"GP\x00\x03").then_some(i)).take(3).collect();
    assert_eq!(geometries.len(), 3);
    for i in geometries {
        offsets.extend(i..i + 8);
    }
    offsets.sort_unstable();
    offsets.dedup();
    for offset in offsets {
        let mut bytes = original.clone();
        bytes[offset] ^= 0xff;
        if let Ok(source) = gpkg_mem::GpkgMemSource::open_shared(Shared(Arc::new(bytes)), None) {
            let _ = source.read(&Request::all());
        }
    }
}

#[test]
fn an_unknown_coordinate_system_is_reported() {
    let p = dir().join("unknown_crs.gpkg");
    write_gpkg(&p, true);
    {
        let c = rusqlite::Connection::open(&p).unwrap();
        c.execute("UPDATE gpkg_spatial_ref_sys SET organization_coordsys_id = 9999999, srs_id = 3857", []).unwrap();
    }
    let src = open_path(&p, None).unwrap();
    assert!(src.info().notes.iter().any(|n| n.contains("EPSG:9999999")), "{:?}", src.info().notes);
    let (_, report) = materialize(src.as_ref()).unwrap();
    assert!(report.messages.iter().any(|m| m.contains("read as longitude/latitude")), "{:?}", report.messages);
}

/// MAGNA-SIRGAS / Origen-Nacional (EPSG:9377) as a GeoPackage writer may define it: no authority.
const ORIGEN_NACIONAL: &str = r#"PROJCS["MAGNA-SIRGAS / Origen-Nacional",GEOGCS["MAGNA-SIRGAS",DATUM["Marco_Geocentrico_Nacional_de_Referencia",SPHEROID["GRS 1980",6378137.0,298.257222101]],PRIMEM["Greenwich",0.0],UNIT["degree",0.0174532925199433]],PROJECTION["Transverse_Mercator"],PARAMETER["false_easting",5000000.0],PARAMETER["false_northing",2000000.0],PARAMETER["central_meridian",-73.0],PARAMETER["scale_factor",0.9992],PARAMETER["latitude_of_origin",4.0],UNIT["Metre",1.0]]"#;

/// A GeoPackage with one point in srs_id 300001, defined by `organization`, `code` and
/// `definition`, at Origen-Nacional metres (4 874 000, 2 580 000).
fn write_custom_srs_gpkg(path: &Path, organization: &str, code: i64, definition: &str) {
    let _ = std::fs::remove_file(path);
    let c = rusqlite::Connection::open(path).unwrap();
    c.execute_batch(
        "CREATE TABLE gpkg_spatial_ref_sys (srs_name TEXT, srs_id INTEGER PRIMARY KEY, organization TEXT, organization_coordsys_id INTEGER, definition TEXT, description TEXT);
         CREATE TABLE gpkg_contents (table_name TEXT PRIMARY KEY, data_type TEXT, identifier TEXT, description TEXT, last_change TEXT, min_x DOUBLE, min_y DOUBLE, max_x DOUBLE, max_y DOUBLE, srs_id INTEGER);
         CREATE TABLE gpkg_geometry_columns (table_name TEXT, column_name TEXT, geometry_type_name TEXT, srs_id INTEGER, z TINYINT, m TINYINT);
         INSERT INTO gpkg_contents (table_name, data_type, srs_id) VALUES ('via', 'features', 300001);
         INSERT INTO gpkg_geometry_columns VALUES ('via', 'geom', 'POINT', 300001, 0, 0);
         CREATE TABLE via (fid INTEGER PRIMARY KEY AUTOINCREMENT, geom BLOB);",
    )
    .unwrap();
    c.execute(
        "INSERT INTO gpkg_spatial_ref_sys VALUES ('Custom', 300001, ?1, ?2, ?3, '')",
        rusqlite::params![organization, code, definition],
    )
    .unwrap();
    let p = Coord::new(4_874_000.0, 2_580_000.0);
    let mut blob = b"GP\x00\x01".to_vec();
    blob.extend_from_slice(&300_001i32.to_le_bytes());
    blob.extend(wkb::encode(&Geometry::Point(p)));
    c.execute("INSERT INTO via (geom) VALUES (?1)", [blob]).unwrap();
}

/// The point of [`write_custom_srs_gpkg`] and the notes, from the desktop and the browser readers.
fn custom_srs_reads(path: &Path) -> Vec<(Option<Geometry>, Vec<String>)> {
    let desktop = open_path(path, None).unwrap();
    let browser = gpkg_mem::GpkgMemSource::open_shared(Shared(Arc::new(std::fs::read(path).unwrap())), None).unwrap();
    [desktop.as_ref(), &browser as &dyn FeatureSource]
        .into_iter()
        .map(|src| {
            let batch = src.read(&Request::all()).unwrap();
            (batch.features.first().and_then(|f| f.geometry.clone()), src.info().notes.clone())
        })
        .collect()
}

#[test]
fn a_gpkg_system_without_an_epsg_code_is_read_from_its_definition() {
    // #1476: a custom row (organization CUSTOM, code 0) with the WKT of Origen-Nacional.
    let p = dir().join("origen_nacional.gpkg");
    write_custom_srs_gpkg(&p, "CUSTOM", 0, ORIGEN_NACIONAL);
    for (geometry, notes) in custom_srs_reads(&p) {
        let Some(Geometry::Point(c)) = geometry else { panic!("{geometry:?}") };
        // Northern Colombia, where EPSG:9377 puts those metres.
        let expected =
            crate::raster::Crs::from_epsg(9377).unwrap().transformer().unwrap().to_lonlat(Coord::new(4_874_000.0, 2_580_000.0)).unwrap();
        assert!((c.x - expected.x).abs() < 1e-6 && (c.y - expected.y).abs() < 1e-6, "{c:?} vs {expected:?}");
        assert!((-74.3..-74.0).contains(&c.x) && (9.1..9.4).contains(&c.y), "{c:?}");
        // Nothing to say: MAGNA-SIRGAS needs no datum shift.
        assert!(notes.is_empty(), "{notes:?}");
    }
    // The layer keeps that system as its own (Layer Properties, Define Projection, exports), in a
    // whole read too, on both readers.
    let desktop = open_path(&p, None).unwrap();
    let browser = gpkg_mem::GpkgMemSource::open_shared(Shared(Arc::new(std::fs::read(&p).unwrap())), None).unwrap();
    for src in [desktop.as_ref(), &browser as &dyn FeatureSource] {
        let crs = src.info().coordinate_system();
        assert!(crs.is_some_and(|c| c.custom_def().is_some_and(|d| d.proj4.contains("+proj=tmerc"))), "{:?}", src.info().crs);
        assert_eq!(materialize(src).unwrap().0.crs, crs);
    }
    drop((desktop, browser));
    // A row without a definition is still said to be read as longitude/latitude.
    write_custom_srs_gpkg(&p, "NONE", 300001, "undefined");
    for (_, notes) in custom_srs_reads(&p) {
        assert!(notes.iter().any(|n| n.contains("srs_id 300001") && n.contains("has no EPSG code")), "{notes:?}");
    }
    // No row for the id at all: said too.
    rusqlite::Connection::open(&p).unwrap().execute("DELETE FROM gpkg_spatial_ref_sys WHERE srs_id = 300001", []).unwrap();
    for (_, notes) in custom_srs_reads(&p) {
        assert!(notes.iter().any(|n| n.contains("srs_id 300001") && n.contains("has no EPSG code")), "{notes:?}");
    }
    // A writer's "undefined …" text defines nothing: the same note, not a parse failure.
    write_custom_srs_gpkg(&p, "NONE", 300001, "undefined geographic coordinate system");
    for (_, notes) in custom_srs_reads(&p) {
        assert!(notes.iter().any(|n| n.contains("has no EPSG code")) && !notes.iter().any(|n| n.contains("can't be read")), "{notes:?}");
    }
    // A definition that can't be read says so, and why.
    write_custom_srs_gpkg(&p, "CUSTOM", 0, "PROJCS[");
    for (_, notes) in custom_srs_reads(&p) {
        assert!(notes.iter().any(|n| n.contains("srs_id 300001") && n.contains("can't be read")), "{notes:?}");
    }
    // The spec's undefined ids: -1 (cartesian) is assumed longitude/latitude, said; 0
    // (geographic) is longitude/latitude, nothing to say.
    for (id, said) in [(-1, true), (0, false)] {
        write_custom_srs_gpkg(&p, "NONE", 300001, "undefined");
        rusqlite::Connection::open(&p)
            .unwrap()
            .execute_batch(&format!(
                "UPDATE gpkg_spatial_ref_sys SET srs_id = {id}, organization_coordsys_id = {id};
                 UPDATE gpkg_contents SET srs_id = {id};
                 UPDATE gpkg_geometry_columns SET srs_id = {id};"
            ))
            .unwrap();
        for (_, notes) in custom_srs_reads(&p) {
            assert_eq!(
                notes.iter().any(|n| n.contains(&format!("srs_id {id})")) && n.contains("has no EPSG code")),
                said,
                "{id}: {notes:?}"
            );
        }
    }
    // A row naming neither organization nor code but defining the system is read by its
    // definition, not as the system its id happens to be.
    write_custom_srs_gpkg(&p, "NONE", 0, ORIGEN_NACIONAL);
    rusqlite::Connection::open(&p)
        .unwrap()
        .execute_batch(
            "UPDATE gpkg_spatial_ref_sys SET srs_id = 3857, organization = NULL, organization_coordsys_id = NULL;
             UPDATE gpkg_contents SET srs_id = 3857;
             UPDATE gpkg_geometry_columns SET srs_id = 3857;",
        )
        .unwrap();
    for (geometry, notes) in custom_srs_reads(&p) {
        assert!(matches!(geometry, Some(Geometry::Point(c)) if (-74.3..-74.0).contains(&c.x)), "{geometry:?}");
        assert!(notes.is_empty(), "{notes:?}");
    }
    // MAGNA-SIRGAS on an ellipsoid other than its own isn't taken for it: the datum note stays.
    write_custom_srs_gpkg(
        &p,
        "CUSTOM",
        0,
        &ORIGEN_NACIONAL.replace(r#"SPHEROID["GRS 1980",6378137.0,298.257222101]"#, r#"SPHEROID["International 1924",6378388.0,297.0]"#),
    );
    for (_, notes) in custom_srs_reads(&p) {
        assert!(notes.iter().any(|n| n.contains("srs_id 300001") && n.contains("few hundred metres")), "{notes:?}");
    }
    // EPSG code 0 is no code: said, not read as longitude/latitude in silence.
    write_custom_srs_gpkg(&p, "EPSG", 0, "undefined");
    for (_, notes) in custom_srs_reads(&p) {
        assert!(notes.iter().any(|n| n.contains("srs_id 300001") && n.contains("has no EPSG code")), "{notes:?}");
    }
    // A row naming neither organization nor code leaves Web Mercator to its id, as a missing row.
    write_custom_srs_gpkg(&p, "NONE", 0, "undefined");
    rusqlite::Connection::open(&p)
        .unwrap()
        .execute_batch(
            "UPDATE gpkg_spatial_ref_sys SET srs_id = 3857, organization = NULL, organization_coordsys_id = NULL;
             UPDATE gpkg_contents SET srs_id = 3857;
             UPDATE gpkg_geometry_columns SET srs_id = 3857;",
        )
        .unwrap();
    for (geometry, notes) in custom_srs_reads(&p) {
        // Those metres in Web Mercator: about 43.8° E, 22.6° N.
        assert!(matches!(geometry, Some(Geometry::Point(c)) if (43.0..45.0).contains(&c.x) && (22.0..23.0).contains(&c.y)), "{geometry:?}");
        assert!(notes.is_empty(), "{notes:?}");
    }
    // An EPSG code stored as a REAL (9377.0, in a column a writer declared DOUBLE) is that code on
    // both readers, with no definition.
    write_custom_srs_gpkg(&p, "EPSG", 0, "undefined");
    rusqlite::Connection::open(&p)
        .unwrap()
        .execute_batch(
            "DROP TABLE gpkg_spatial_ref_sys;
             CREATE TABLE gpkg_spatial_ref_sys (srs_name TEXT, srs_id INTEGER PRIMARY KEY, organization TEXT, organization_coordsys_id DOUBLE, definition TEXT, description TEXT);
             INSERT INTO gpkg_spatial_ref_sys VALUES ('Origen-Nacional', 300001, 'EPSG', 9377.0, 'undefined', '');",
        )
        .unwrap();
    for (geometry, notes) in custom_srs_reads(&p) {
        assert!(matches!(geometry, Some(Geometry::Point(c)) if (-74.3..-74.0).contains(&c.x)), "{geometry:?}");
        assert!(notes.is_empty(), "{notes:?}");
    }
    // An EPSG code this build doesn't know falls back to the definition.
    write_custom_srs_gpkg(&p, "EPSG", 9_999_999, ORIGEN_NACIONAL);
    for (geometry, notes) in custom_srs_reads(&p) {
        assert!(matches!(geometry, Some(Geometry::Point(c)) if (-74.3..-74.0).contains(&c.x)), "{geometry:?}");
        assert!(notes.is_empty(), "{notes:?}");
    }
}

#[test]
fn flatgeobuf_round_trips_a_dataset() {
    let mut ds = Dataset::new("mixed", GeometryType::Polygon, vec![Field::new("n", FieldType::Integer), Field::new("s", FieldType::Text)]);
    ds.fields.push(Field::new("ok", FieldType::Boolean));
    ds.fields.push(Field::new("x", FieldType::Double));
    let sq = |x: f64| vec![vec![Coord::new(x, 0.0), Coord::new(x + 1.0, 0.0), Coord::new(x + 1.0, 1.0), Coord::new(x, 0.0)]];
    ds.push(Some(Geometry::Polygon(vec![sq(0.0)])), vec![Value::Int(7), Value::Text("a".into()), Value::Bool(true), Value::Float(1.5)]);
    ds.push(Some(Geometry::Polygon(vec![sq(5.0), sq(9.0)])), vec![Value::Null, Value::Text("b".into()), Value::Null, Value::Null]);
    let p = dir().join("rt.fgb");
    crate::io::write_path(&ds, &p).unwrap();
    let (back, _) = crate::io::read_path(&p).unwrap();
    assert_eq!(back.features.len(), 2);
    assert_eq!(back.fields.iter().map(|f| f.ty).collect::<Vec<_>>(), ds.fields.iter().map(|f| f.ty).collect::<Vec<_>>());
    let a = back.features.iter().find(|f| f.attrs[1] == Value::Text("a".into())).unwrap();
    assert_eq!(a.attrs, ds.features[0].attrs);
    let b = back.features.iter().find(|f| f.attrs[1] == Value::Text("b".into())).unwrap();
    assert_eq!(b.attrs[0], Value::Null);
    assert!(matches!(&b.geometry, Some(Geometry::Polygon(p)) if p.len() == 2));
}

#[test]
fn hostile_files_are_errors_not_panics() {
    let mut junk = vec![b"PAR1".to_vec(), b"fgb\x03fgb\x00".to_vec(), b"SQLite format 3\0".to_vec(), vec![0xff; 4096], Vec::new()];
    // Every truncation of a real file.
    let d = dir();
    let p = d.join("t.parquet");
    write_parquet(&p, true);
    let pq = std::fs::read(&p).unwrap();
    let f = d.join("t.fgb");
    write_fgb(&f);
    let fgb = std::fs::read(&f).unwrap();
    for n in (0..pq.len()).step_by(97) {
        junk.push(pq[..n].to_vec());
    }
    for n in (0..fgb.len()).step_by(89) {
        junk.push(fgb[..n].to_vec());
    }
    for bytes in junk {
        for name in ["x.parquet", "x.fgb", "x.gpkg"] {
            if let Ok(s) = open_bytes(name, bytes.clone(), None) {
                let _ = s.read(&Request::all());
                let _ = s.read(&Request { extent: Some(Extent::new(-10.0, -10.0, 10.0, 10.0)), limit: 3 });
            }
        }
    }
    assert!(open_path(Path::new("nope.shp"), None).is_err());
}

/// Reads every vector file in `RUSTGIS_SAMPLE_VECTORS` (the opengeos sample data, e.g.
/// <https://source.coop/giswqs/opengeos>): `cargo test -p rustgis-data -- --ignored vector_samples --nocapture`.
#[test]
#[ignore]
fn vector_samples() {
    let Ok(dir) = std::env::var("RUSTGIS_SAMPLE_VECTORS") else { return };
    let mut paths: Vec<_> =
        std::fs::read_dir(dir).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| is_source_name(&p.to_string_lossy())).collect();
    paths.sort();
    assert!(!paths.is_empty());
    for p in paths {
        let names = match layers(&p) {
            Ok(n) => n,
            Err(e) => {
                println!("{}: {e}", p.display());
                continue;
            }
        };
        for layer in names {
            let t = std::time::Instant::now();
            let src = match open_path(&p, Some(&layer)) {
                Ok(s) => s,
                Err(e) => {
                    println!("{} [{layer}]: {e}", p.display());
                    continue;
                }
            };
            let i = src.info();
            let open = t.elapsed();
            let e = i.extent;
            let view = Extent::new(
                e.center().x - e.width() / 8.0,
                e.center().y - e.height() / 8.0,
                e.center().x + e.width() / 8.0,
                e.center().y + e.height() / 8.0,
            );
            let t2 = std::time::Instant::now();
            let b = match src.read(&Request { extent: Some(view), limit: 200_000 }) {
                Ok(b) => b,
                Err(e) => {
                    println!("{} [{layer}]: {e}", p.display());
                    continue;
                }
            };
            println!(
                "{} [{}] {} {:?} {} fields, {:?} features, {} indexed={} extent={:?} open {:?}; view: {} of {} (complete={}, skipped {}) in {:?}",
                p.file_name().unwrap().to_string_lossy(),
                i.name,
                i.format,
                i.geometry_type,
                i.fields.len(),
                i.count,
                i.crs,
                i.indexed,
                i.extent,
                open,
                b.features.len(),
                b.matched,
                b.complete,
                b.skipped,
                t2.elapsed()
            );
        }
    }
}

/// A polygon layer with every field type, a hole, a multipolygon, a feature without geometry,
/// long and clashing field names and non-ASCII text.
fn export_sample() -> Dataset {
    let mut ds = Dataset::new(
        "Parcels",
        GeometryType::Polygon,
        vec![
            Field::new("population_total", FieldType::Integer),
            Field::new("population_density", FieldType::Double),
            Field::new("name", FieldType::Text),
            Field::new("ok", FieldType::Boolean),
            Field::new("geometry", FieldType::Text),
        ],
    );
    let sq = |x: f64, y: f64, s: f64| {
        vec![Coord::new(x, y), Coord::new(x + s, y), Coord::new(x + s, y + s), Coord::new(x, y + s), Coord::new(x, y)]
    };
    ds.push(
        Some(Geometry::Polygon(vec![vec![sq(0.0, 0.0, 10.0), sq(2.0, 2.0, 2.0)]])),
        vec![Value::Int(1_234_567), Value::Float(12.5), Value::Text("Zürich — 東京".into()), Value::Bool(true), Value::Text("g".into())],
    );
    ds.push(
        Some(Geometry::Polygon(vec![vec![sq(20.0, 20.0, 1.0)], vec![sq(-30.0, -40.0, 5.0)]])),
        vec![Value::Int(-3), Value::Float(-0.25), Value::Null, Value::Bool(false), Value::Null],
    );
    ds.push(None, vec![Value::Null, Value::Null, Value::Text("no shape".into()), Value::Null, Value::Null]);
    ds
}

#[test]
fn writes_geopackage_geoparquet_and_shapefile() {
    let ds = export_sample();
    let workspace = tempfile::tempdir().unwrap();
    let d = workspace.path();
    for ext in ["gpkg", "parquet", "shp", "fgb"] {
        let p = d.join(format!("parcels.{ext}"));
        let left_out = crate::io::write_path(&ds, &p).unwrap();
        let holds_all = matches!(ext, "gpkg" | "parquet");
        assert_eq!(left_out, if holds_all { 0 } else { 1 }, "{ext}");
        let (back, _) = crate::io::read_path(&p).unwrap();
        // GeoParquet retains nullable geometry; the other readers omit rows without a shape.
        assert_eq!(back.features.len(), if ext == "parquet" { 3 } else { 2 }, "{ext}");
        assert_eq!(back.geometry_type, GeometryType::Polygon, "{ext}");
        let e = back.extent();
        assert_eq!((e.min_x, e.min_y, e.max_x, e.max_y), (-30.0, -40.0, 21.0, 21.0), "{ext}");
        let field = |name: &str| back.field_index(name).unwrap_or_else(|| panic!("{ext}: no field {name} in {:?}", back.fields));
        let pop = if ext == "shp" { field("population") } else { field("population_total") };
        let dens = if ext == "shp" { field("populati_1") } else { field("population_density") };
        let name = field("name");
        let first = back.features.iter().find(|f| f.attrs[name] == Value::Text("Zürich — 東京".into())).unwrap();
        assert_eq!(first.attrs[pop].as_f64(), Some(1_234_567.0), "{ext}");
        assert_eq!(first.attrs[dens].as_f64(), Some(12.5), "{ext}");
        assert!(matches!(&first.geometry, Some(Geometry::Polygon(p)) if p.len() == 1 && p[0].len() == 2), "{ext}: the hole is kept");
        let second = back.features.iter().find(|f| f.attrs[pop].as_f64() == Some(-3.0)).unwrap();
        assert!(matches!(&second.geometry, Some(Geometry::Polygon(p)) if p.len() == 2), "{ext}: both parts are kept");
        assert_eq!(second.attrs[name], Value::Null, "{ext}");
        if holds_all {
            // The clashing field keeps its name; the geometry column took another.
            assert_eq!(back.value(0, field("geometry")), &Value::Text("g".into()), "{ext}");
        }
    }
    // The Shapefile's companion files.
    for e in ["shx", "dbf", "prj", "cpg"] {
        assert!(d.join(format!("parcels.{e}")).exists(), "{e}");
    }
    // GeoPackage and GeoParquet keep feature ids and answer extent queries by their index.
    for ext in ["gpkg", "parquet"] {
        let s = open_path(&d.join(format!("parcels.{ext}")), None).unwrap();
        assert_eq!(s.info().count, Some(3), "{ext}");
        let b = s.read(&Request { extent: Some(Extent::new(15.0, 15.0, 25.0, 25.0)), limit: 100 }).unwrap();
        assert_eq!(b.features.len(), 1, "{ext}");
        if ext == "gpkg" {
            assert_eq!(b.features[0].id, 2);
        }
    }
    // Single-file formats can replace their old exports, but GeoPackages may hold other tables.
    let mut fewer = ds.clone();
    fewer.features.truncate(1);
    for ext in ["parquet", "shp"] {
        let p = d.join(format!("parcels.{ext}"));
        crate::io::write_path(&fewer, &p).unwrap();
        assert_eq!(crate::io::read_path(&p).unwrap().0.features.len(), 1, "{ext}");
    }
}

/// Whole reads retain null geometry and its attributes; spatial queries require a location.
#[test]
fn geoparquet_exports_keep_exact_values_and_remote_extent_reads() {
    let mut ds =
        Dataset::new("points", GeometryType::Point, vec![Field::new("bbox", FieldType::Text), Field::new("value", FieldType::Double)]);
    ds.push(None, vec![Value::Text("no shape".into()), Value::Float(-0.25)]);
    ds.push(
        Some(Geometry::Point(Coord::new(-89.12345678901234, 36.181582500000005))),
        vec![Value::Text("東京".into()), Value::Float(0.12345678901234568)],
    );
    ds.push(Some(Geometry::Point(Coord::new(1.2345678901234567, -12.345678901234567))), vec![Value::Null, Value::Null]);
    let bytes = parquet::write(&ds).unwrap();
    let local = open_bytes("exact.parquet", bytes.clone(), None).unwrap();
    assert_eq!(local.info().count, Some(3));
    assert_eq!(local.info().fields, ds.fields);
    assert!(local.info().notes.is_empty());
    let back = local.read(&Request::all()).unwrap();
    assert_eq!(back.features.len(), 3);
    assert!(back.complete && back.skipped == 0);
    for (got, original) in back.features.iter().zip(&ds.features) {
        // Comparing WKB bytes also checks the coordinate bits, rather than numeric equality.
        assert_eq!(got.geometry.as_ref().map(wkb::encode), original.geometry.as_ref().map(wkb::encode));
        assert_eq!(got.attrs, original.attrs);
    }
    assert_eq!(back.features.iter().map(|f| f.id).collect::<Vec<_>>(), [1, 2, 3]);
    let (rows, summary, _) = visit_all(local.as_ref(), None, 256);
    assert_eq!(rows, back.features);
    assert_eq!((summary.matched, summary.skipped), (3, 0));
    let (whole, report) = materialize(local.as_ref()).unwrap();
    assert_eq!(whole.features.to_vec(), back.features);
    assert!(report.skipped == 0 && report.messages.is_empty());
    let url = "https://test.example/exact-export.parquet";
    remote::testing::serve(url, bytes);
    let source = open_driven(url);
    assert!(source.info().indexed);
    assert_eq!(source.read(&Request::all()).unwrap().features, back.features);
    // Null rows must not consume a sample slot, whether the group is partly or wholly inside.
    for (extent, matched) in [(Extent::new(-90.0, 36.0, -89.0, 37.0), 1), (Extent::new(-180.0, -90.0, 180.0, 90.0), 2)] {
        for limit in [1, 10] {
            let req = Request { extent: Some(extent), limit };
            let remote = source.read(&req).unwrap();
            let local_batch = local.read(&req).unwrap();
            assert_eq!(remote.features.len(), matched.min(limit));
            assert_eq!((remote.matched, local_batch.matched), (matched as u64, matched as u64));
            assert_eq!(remote.features, local_batch.features);
            assert_eq!(remote.complete, matched <= limit);
            if remote.complete {
                let (rows, summary, _) = visit_all(local.as_ref(), req.extent, 256);
                assert_eq!(rows, remote.features);
                assert_eq!(summary.matched, remote.matched);
            }
        }
    }
}

/// Inferred pre-1.1 boxes may be absent independently of geometry; sample actual spatial matches.
#[test]
fn geoparquet_inferred_covering_nulls_fall_back_to_geometry() {
    use arrow_array::{BinaryArray, Float64Array, RecordBatch, StructArray};
    use arrow_schema::{DataType, Field as AField, Schema};
    let point = Coord::new(-89.5, 36.5);
    let wkb = wkb::encode(&Geometry::Point(point));
    let bbox_fields = ["xmin", "ymin", "xmax", "ymax"].map(|name| Arc::new(AField::new(name, DataType::Float64, true)));
    let coords: Vec<arrow_array::ArrayRef> = [0.0, 0.0, 0.0, 0.0]
        .into_iter()
        .map(|value| Arc::new(Float64Array::from(vec![Some(value), None])) as arrow_array::ArrayRef)
        .collect();
    let bbox = StructArray::new(bbox_fields.iter().cloned().collect(), coords, None);
    let schema = Arc::new(Schema::new(vec![
        AField::new("geometry", DataType::Binary, true),
        AField::new("bbox", DataType::Struct(bbox_fields.iter().cloned().collect()), false),
    ]));
    // This is an ordinary struct column in 1.0, with no declared covering/null-presence contract.
    let geo = r#"{"version":"1.0.0","primary_column":"geometry","columns":{"geometry":{"encoding":"WKB","geometry_types":["Point"]}}}"#;
    let props = ::parquet::file::properties::WriterProperties::builder()
        .set_key_value_metadata(Some(vec![::parquet::file::metadata::KeyValue::new("geo".to_string(), geo.to_string())]))
        .build();
    let cols: Vec<arrow_array::ArrayRef> = vec![Arc::new(BinaryArray::from(vec![None, Some(wkb.as_slice())])), Arc::new(bbox)];
    let batch = RecordBatch::try_new(schema.clone(), cols).unwrap();
    let mut writer = ::parquet::arrow::ArrowWriter::try_new(Vec::new(), schema, Some(props)).unwrap();
    writer.write(&batch).unwrap();
    let bytes = writer.into_inner().unwrap();
    let local = open_bytes("inferred.parquet", bytes.clone(), None).unwrap();
    assert!(local.info().indexed);
    assert_eq!(local.read(&Request::all()).unwrap().features.len(), 2);
    let url = "https://test.example/inferred-covering.parquet";
    remote::testing::serve(url, bytes);
    let remote = open_driven(url);
    for extent in [Extent::new(-90.0, 36.0, -89.0, 37.0), Extent::new(-180.0, -90.0, 180.0, 90.0)] {
        for limit in [1, 10] {
            let req = Request { extent: Some(extent), limit };
            let expected = local.read(&req).unwrap();
            assert_eq!(expected.features.len(), 1);
            assert_eq!((expected.matched, expected.complete, expected.skipped), (1, true, 0));
            assert_eq!(expected.features[0].id, 2);
            let by_address = remote.read(&req).unwrap();
            assert_eq!(by_address.features, expected.features);
            assert_eq!((by_address.matched, by_address.complete, by_address.skipped), (1, true, 0));
            assert_eq!(visit_all(local.as_ref(), req.extent, 256).0, expected.features);
        }
    }
}

#[test]
fn geopackage_export_preserves_existing_database_and_wal() {
    let workspace = tempfile::tempdir().unwrap();
    let path = workspace.path().join("county.gpkg");
    let mut parcels = Dataset::new("parcels", GeometryType::Point, vec![]);
    parcels.push(Some(Geometry::Point(Coord::new(1.0, 2.0))), vec![]);
    crate::io::write_path(&parcels, &path).unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         CREATE TABLE zoning (name TEXT);
         INSERT INTO zoning VALUES ('residential');
         INSERT INTO gpkg_contents (table_name, data_type, identifier) VALUES ('zoning', 'attributes', 'zoning');",
    )
    .unwrap();
    let wal = workspace.path().join("county.gpkg-wal");
    let before = std::fs::read(&path).unwrap();
    let wal_before = std::fs::read(&wal).unwrap();
    assert!(!wal_before.is_empty());
    let mut permits = parcels.clone();
    permits.name = "permits".into();
    let result = crate::io::write_path(&permits, &path);
    assert!(result.is_err(), "export must refuse to replace an existing GeoPackage");
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(std::fs::read(&wal).unwrap(), wal_before);
    let names: Vec<String> = conn
        .prepare("SELECT table_name FROM gpkg_contents ORDER BY table_name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    assert_eq!(names, ["parcels", "zoning"]);
    assert_eq!(conn.query_row("SELECT name FROM zoning", [], |r| r.get::<_, String>(0)).unwrap(), "residential");
    // A fresh filename still exports normally, including a round trip through the reader.
    let fresh = workspace.path().join("permits.gpkg");
    crate::io::write_path(&permits, &fresh).unwrap();
    assert_eq!(crate::io::read_path(&fresh).unwrap().0.features.len(), 1);
}

#[test]
fn geopackage_export_refuses_existing_entries_and_orphaned_journals() {
    let workspace = tempfile::tempdir().unwrap();
    let ds = Dataset::new("permits", GeometryType::Point, vec![]);
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let path = workspace.path().join("county.gpkg");
        let entry = workspace.path().join(format!("county.gpkg{suffix}"));
        std::fs::write(&entry, b"existing data").unwrap();
        assert!(crate::io::write_path(&ds, &path).is_err());
        assert_eq!(std::fs::read(&entry).unwrap(), b"existing data");
        std::fs::remove_file(entry).unwrap();
        assert!(!path.exists());
        assert_eq!(std::fs::read_dir(workspace.path()).unwrap().count(), 0);
    }
    let path = workspace.path().join("directory.gpkg");
    std::fs::create_dir(&path).unwrap();
    assert!(crate::io::write_path(&ds, &path).is_err());
    assert!(path.is_dir());
}

#[cfg(unix)]
#[test]
fn geopackage_export_uses_regular_file_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let workspace = tempfile::tempdir().unwrap();
    let regular = workspace.path().join("regular");
    std::fs::write(&regular, b"control").unwrap();
    let path = workspace.path().join("permits.gpkg");
    crate::io::write_path(&Dataset::new("permits", GeometryType::Point, vec![]), &path).unwrap();
    // Compare with an ordinary file under the current umask, without changing process-wide state.
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&path), mode(&regular));
}

#[cfg(unix)]
#[test]
fn geopackage_export_preserves_destination_symlinks() {
    let workspace = tempfile::tempdir().unwrap();
    let ds = Dataset::new("permits", GeometryType::Point, vec![]);
    let target = workspace.path().join("target.gpkg");
    let path = workspace.path().join("county.gpkg");
    std::os::unix::fs::symlink(&target, &path).unwrap();
    for exists in [false, true] {
        if exists {
            std::fs::write(&target, b"existing data").unwrap();
        }
        assert!(crate::io::write_path(&ds, &path).is_err());
        assert_eq!(std::fs::read_link(&path).unwrap(), target);
        if exists {
            assert_eq!(std::fs::read(&target).unwrap(), b"existing data");
        } else {
            assert!(!target.exists());
        }
    }
}

#[test]
fn writes_points_lines_kmz_and_zipped_shapefiles() {
    let mut pts = Dataset::new("Cities", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
    pts.push(Some(Geometry::Point(Coord::new(2.35, 48.85))), vec![Value::Text("Paris".into())]);
    pts.push(Some(Geometry::MultiPoint(vec![Coord::new(1.0, 1.0), Coord::new(2.0, 2.0)])), vec![Value::Text("Pair".into())]);
    let mut lines = Dataset::new("Roads", GeometryType::Polyline, vec![Field::new("len", FieldType::Double)]);
    lines.push(
        Some(Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(1.0, 1.0)], vec![Coord::new(5.0, 5.0), Coord::new(6.0, 5.0)]])),
        vec![Value::Float(1e30)],
    );
    lines.push(Some(Geometry::Line(vec![vec![Coord::new(3.0, 3.0)]])), vec![Value::Float(2.0)]);
    let d = dir().join("export2");
    std::fs::create_dir_all(&d).unwrap();
    for ds in [&pts, &lines] {
        for ext in ["gpkg", "parquet", "shp"] {
            let p = d.join(format!("{}.{ext}", ds.name));
            let left_out = crate::io::write_path(ds, &p).unwrap();
            let (back, _) = crate::io::read_path(&p).unwrap();
            assert_eq!(back.geometry_type, ds.geometry_type, "{ext}");
            // A one-point line can't be a Shapefile polyline (and isn't read back from the others).
            let lost = usize::from(ds.name == "Roads");
            assert_eq!((left_out, back.features.len()), (if ext == "shp" { lost } else { 0 }, 2 - lost), "{} {ext}", ds.name);
            assert_eq!(
                back.extent(),
                ds.features.iter().take(2 - lost).filter_map(|f| f.geometry.as_ref()).fold(Extent::EMPTY, |e, g| e.union(&g.extent()))
            );
        }
    }
    // A number too large for a dBase field is null rather than cut short.
    let (roads, _) = crate::io::read_path(&d.join("Roads.shp")).unwrap();
    assert_eq!(roads.value(0, 0), &Value::Null);
    // A whole number past 2^53 would only be stored rounded, so it's null too.
    let mut ids = Dataset::new("ids", GeometryType::Point, vec![Field::new("id", FieldType::Integer)]);
    ids.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![Value::Int((1 << 53) + 1)]);
    ids.push(Some(Geometry::Point(Coord::new(1.0, 1.0))), vec![Value::Int(1 << 52)]);
    crate::io::write_path(&ids, &d.join("ids.shp")).unwrap();
    let (back, _) = crate::io::read_path(&d.join("ids.shp")).unwrap();
    assert_eq!((back.value(0, 0), back.value(1, 0)), (&Value::Null, &Value::Int(1 << 52)));
    // An upper-case .SHP keeps its name, so the path given is the file written.
    crate::io::write_path(&ids, &d.join("Upper.SHP")).unwrap();
    assert!(crate::io::read_path(&d.join("Upper.SHP")).is_ok());
    // No temporary files are left beside the outputs.
    assert!(std::fs::read_dir(&d).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().ends_with(".tmp")));
    // KMZ: the KML zipped as doc.kml.
    let (kmz, left_out) = crate::io::write_bytes(&pts, "kmz").unwrap();
    assert_eq!(left_out, 0);
    let entries = crate::zip::read(&kmz).unwrap();
    assert_eq!(entries[0].0, "doc.kml");
    assert!(String::from_utf8_lossy(&entries[0].1).contains("<name>Paris</name>"));
    // A zipped Shapefile opens as one.
    let (zipped, _) = crate::io::write_bytes(&pts, "shp").unwrap();
    let entries = crate::zip::read(&zipped).unwrap();
    let names: Vec<&str> = entries.iter().map(|e| e.0.as_str()).collect();
    assert_eq!(names, ["Cities.shp", "Cities.shx", "Cities.dbf", "Cities.prj", "Cities.cpg"]);
    let unzipped = d.join("unzipped");
    std::fs::create_dir_all(&unzipped).unwrap();
    for (n, b) in &entries {
        std::fs::write(unzipped.join(n), b).unwrap();
    }
    let (back, _) = crate::io::read_path(&unzipped.join("Cities.shp")).unwrap();
    assert_eq!(back.features.len(), 2);
    // Tables have no Shapefile or FlatGeobuf form, but GeoPackage and GeoParquet hold them.
    let mut table = Dataset::new("t", GeometryType::None, vec![Field::new("a", FieldType::Integer)]);
    table.push(None, vec![Value::Int(1)]);
    assert!(crate::io::write_bytes(&table, "shp").is_err());
    assert!(crate::io::write_bytes(&table, "parquet").is_ok());
    crate::io::write_path(&table, &d.join("t.gpkg")).unwrap();
    // Field names differing only in case are distinct in a dataset but not in SQLite.
    let mut cased = Dataset::new("c", GeometryType::Point, vec![Field::new("A", FieldType::Integer), Field::new("a", FieldType::Text)]);
    cased.push(Some(Geometry::Point(Coord::new(1.0, 2.0))), vec![Value::Int(1), Value::Text("x".into())]);
    crate::io::write_path(&cased, &d.join("c.gpkg")).unwrap();
    let (back, _) = crate::io::read_path(&d.join("c.gpkg")).unwrap();
    assert_eq!(back.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["A", "a_1"]);
    assert_eq!(back.features[0].attrs, [Value::Int(1), Value::Text("x".into())]);
    // An empty layer writes too.
    let empty = Dataset::new("e", GeometryType::Point, vec![]);
    assert!(crate::io::write_bytes(&empty, "parquet").is_ok());
    crate::io::write_path(&empty, &d.join("e.gpkg")).unwrap();
    assert!(crate::io::write_bytes(&empty, "shp").is_err());
}

/// Compares the browser's GeoPackage reader with SQLite on every table of every `.gpkg` in
/// `RUSTGIS_SAMPLE_VECTORS`: `cargo test -p rustgis-data --release -- --ignored gpkg_mem_matches --nocapture`.
#[test]
#[ignore]
fn gpkg_mem_matches_sqlite() {
    let Ok(dir) = std::env::var("RUSTGIS_SAMPLE_VECTORS") else { return };
    let mut paths: Vec<_> = std::fs::read_dir(dir).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| ext(p) == "gpkg").collect();
    paths.sort();
    let (mut tables, mut differ) = (0, 0);
    for p in paths {
        let bytes = Shared(Arc::new(std::fs::read(&p).unwrap()));
        let names = match layers(&p) {
            Ok(n) => n,
            Err(e) => {
                println!("{}: {e}", p.display());
                continue;
            }
        };
        for name in names {
            tables += 1;
            let t = std::time::Instant::now();
            let mem = gpkg_mem::GpkgMemSource::open_shared(bytes.clone(), Some(&name))
                .and_then(|s| s.read(&Request::all()).map(|b| (s.info().clone(), b)));
            let ms = t.elapsed().as_millis();
            let sql = open_path(&p, Some(&name)).and_then(|s| s.read(&Request::all()).map(|b| (s.info().clone(), b)));
            match (mem, sql) {
                (Ok((mi, mut m)), Ok((si, mut s))) => {
                    m.features.sort_by_key(|f| f.id);
                    s.features.sort_by_key(|f| f.id);
                    let same =
                        m.features == s.features && mi.fields == si.fields && mi.count == si.count && mi.geometry_type == si.geometry_type;
                    differ += usize::from(!same);
                    println!("{} {name}: {} features, {ms} ms{}", p.display(), m.features.len(), if same { "" } else { "  DIFFERENT" });
                }
                (m, s) => {
                    differ += usize::from(m.is_ok() != s.is_ok());
                    println!("{} {name}: mem {:?} / sqlite {:?}", p.display(), m.err(), s.err());
                }
            }
        }
    }
    println!("{tables} tables, {differ} different");
    assert_eq!(differ, 0);
}

fn open_driven(url: &str) -> Fetching {
    Fetching(std::sync::Arc::from(open_url_blocking(url).unwrap()))
}

/// `n` × `n` points over the world, a FlatGeobuf or a GeoParquet (row groups of 2 000), larger
/// than one fetch.
fn big(n: usize, parquet: bool) -> Vec<u8> {
    let mut ds =
        Dataset::new("big", GeometryType::Point, vec![Field::new("name", FieldType::Text), Field::new("value", FieldType::Integer)]);
    // Built directly: `push` per feature is slow for this many.
    for i in 0..n {
        for j in 0..n {
            let c = Coord::new(-179.0 + 358.0 * i as f64 / n as f64, -80.0 + 160.0 * j as f64 / n as f64);
            let id = (i * n + j) as u64;
            ds.features.push(crate::Feature {
                id: id + 1,
                geometry: Some(Geometry::Point(c)),
                attrs: vec![Value::Text(format!("p{i}_{j}")), Value::Int(id as i64)],
            });
        }
    }
    if !parquet {
        return fgb::write(&ds).unwrap();
    }
    // GeoParquet in row groups of 2 000 rows (with a bbox covering), as large files are written.
    use arrow_array::{BinaryArray, Float64Array, Int64Array, RecordBatch, StringArray, StructArray};
    use arrow_schema::{DataType, Field as AField, Schema};
    let pts: Vec<Coord> = ds
        .features
        .iter()
        .filter_map(|f| match f.geometry {
            Some(Geometry::Point(c)) => Some(c),
            _ => None,
        })
        .collect();
    let bbox_fields = ["xmin", "ymin", "xmax", "ymax"].map(|n| Arc::new(AField::new(n, DataType::Float64, false)));
    let coord = |f: fn(&Coord) -> f64| Arc::new(Float64Array::from(pts.iter().map(f).collect::<Vec<_>>()));
    let bbox =
        StructArray::new(bbox_fields.iter().cloned().collect(), vec![coord(|c| c.x), coord(|c| c.y), coord(|c| c.x), coord(|c| c.y)], None);
    let schema = Arc::new(Schema::new(vec![
        AField::new("name", DataType::Utf8, false),
        AField::new("value", DataType::Int64, false),
        AField::new("geometry", DataType::Binary, false),
        AField::new("bbox", DataType::Struct(bbox_fields.iter().cloned().collect()), false),
    ]));
    let wkbs: Vec<Vec<u8>> = pts.iter().map(|c| wkb::encode(&Geometry::Point(*c))).collect();
    let cols: Vec<arrow_array::ArrayRef> = vec![
        Arc::new(StringArray::from(ds.features.iter().map(|f| f.attrs[0].to_string()).collect::<Vec<_>>())),
        Arc::new(Int64Array::from(ds.features.iter().map(|f| f.attrs[1].as_f64().unwrap_or(0.0) as i64).collect::<Vec<_>>())),
        Arc::new(BinaryArray::from(wkbs.iter().map(|w| w.as_slice()).collect::<Vec<_>>())),
        Arc::new(bbox),
    ];
    let geo = r#"{"version":"1.1.0","primary_column":"geometry","columns":{"geometry":{"encoding":"WKB","geometry_types":["Point"],
        "covering":{"bbox":{"xmin":["bbox","xmin"],"ymin":["bbox","ymin"],"xmax":["bbox","xmax"],"ymax":["bbox","ymax"]}}}}}"#;
    let props = ::parquet::file::properties::WriterProperties::builder()
        .set_max_row_group_row_count(Some(2_000))
        .set_key_value_metadata(Some(vec![::parquet::file::metadata::KeyValue::new("geo".to_string(), geo.to_string())]))
        .build();
    let mut out = Vec::new();
    let mut w = ::parquet::arrow::ArrowWriter::try_new(&mut out, schema.clone(), Some(props)).unwrap();
    w.write(&RecordBatch::try_new(schema, cols).unwrap()).unwrap();
    w.close().unwrap();
    out
}

#[test]
fn flatgeobuf_and_geoparquet_by_address() {
    let d = dir();
    // The same checks as for a file on disk, read by range.
    let f = d.join("r.fgb");
    write_fgb(&f);
    remote::testing::serve("https://test.example/grid.fgb", std::fs::read(&f).unwrap());
    let src = open_driven("https://test.example/grid.fgb");
    check(&src, true);
    assert_eq!(src.info().name, "grid");
    let p = d.join("r.parquet");
    write_parquet(&p, true);
    remote::testing::serve("https://test.example/grid.parquet", std::fs::read(&p).unwrap());
    check(&open_driven("https://test.example/grid.parquet"), true);

    // Opening keeps reading the file it fetches for, even when other files push it out of those
    // kept while its bytes are on their way.
    let busy = "https://test.example/busy.fgb#busy";
    remote::testing::serve(busy, std::fs::read(&f).unwrap());
    assert_eq!(open_driven(busy).info().count, src.info().count);

    // A view of a large file fetches a small part of it, and the same features as the file on disk.
    for (name, parquet) in [("big.fgb", false), ("big.parquet", true)] {
        let bytes = big(400, parquet);
        let total = bytes.len();
        let url = format!("https://test.example/{name}");
        let sent = remote::testing::serve(&url, bytes.clone());
        let src = open_driven(&url);
        assert_eq!(src.info().count, Some(160_000), "{name}");
        let e = Extent::new(10.0, 10.0, 12.0, 12.0);
        let req = Request { extent: Some(e), limit: 10_000 };
        let got = src.read(&req).unwrap();
        let local = open_bytes(name, bytes, None).unwrap().read(&req).unwrap();
        let ids = |b: &Batch| b.features.iter().map(|f| f.id).collect::<std::collections::BTreeSet<_>>();
        assert!(!got.features.is_empty() && ids(&got) == ids(&local), "{name}: {} vs {}", got.features.len(), local.features.len());
        assert_eq!(
            got.features.first().map(|f| &f.attrs),
            local.features.iter().find(|f| Some(f.id) == got.features.first().map(|g| g.id)).map(|f| &f.attrs)
        );
        let fetched = sent.load(std::sync::atomic::Ordering::Relaxed);
        assert!(fetched < total / 4, "{name}: fetched {fetched} of {total} bytes");
        // A sample of the whole file, spread over it.
        let s = src.read(&Request { extent: None, limit: 500 }).unwrap();
        assert!(!s.complete && s.features.len() <= 500 && s.features.len() >= 250, "{name}: {}", s.features.len());
    }

    // Files without an index or a covering are read whole instead.
    let p = d.join("plain.parquet");
    write_parquet(&p, false);
    remote::testing::serve("https://test.example/plain.parquet", std::fs::read(&p).unwrap());
    let e = open_url_blocking("https://test.example/plain.parquet").err().map(|e| e.to_string()).unwrap_or_default();
    assert!(e.contains(remote::READ_WHOLE), "{e}");
    let mut ds = Dataset::new("x", GeometryType::Point, vec![]);
    ds.push(Some(Geometry::Point(Coord::new(1.0, 2.0))), vec![]);
    let unindexed = {
        use flatgeobuf::{FgbWriter, FgbWriterOptions, GeometryType as G};
        let mut w = FgbWriter::create_with_options("x", G::Point, FgbWriterOptions { write_index: false, ..Default::default() }).unwrap();
        w.add_feature_geom(flatgeobuf::geozero::wkb::Wkb(wkb::encode(&Geometry::Point(Coord::new(1.0, 2.0)))), |_| {}).unwrap();
        let mut out = Vec::new();
        w.write(&mut out).unwrap();
        out
    };
    remote::testing::serve("https://test.example/noindex.fgb", unindexed);
    let e = open_url_blocking("https://test.example/noindex.fgb").err().map(|e| e.to_string()).unwrap_or_default();
    assert!(e.contains(remote::READ_WHOLE), "{e}");

    // Cut short and junk: errors, never a panic or an endless read.
    let fgb = big(40, false);
    let pq = big(40, true);
    for (k, n) in (0..fgb.len()).step_by(997).enumerate() {
        let url = format!("https://test.example/cut{k}.fgb");
        remote::testing::serve(&url, fgb[..n].to_vec());
        if let Ok(s) = open_url_blocking(&url) {
            let s: std::sync::Arc<dyn FeatureSource> = std::sync::Arc::from(s);
            let _ = read_blocking(&s, &Request::all());
        }
    }
    for (k, n) in (0..pq.len()).step_by(991).enumerate() {
        let url = format!("https://test.example/cut{k}.parquet");
        remote::testing::serve(&url, pq[pq.len() - n..].to_vec());
        if let Ok(s) = open_url_blocking(&url) {
            let s: std::sync::Arc<dyn FeatureSource> = std::sync::Arc::from(s);
            let _ = read_blocking(&s, &Request::all());
        }
    }
    assert!(open_url_blocking("https://test.example/missing.fgb").is_err());
    assert!(is_remote_name("https://x.org/a/b.fgb?sig=1") && is_remote_name("http://x.org/b.geoparquet") && !is_remote_name("/tmp/a.fgb"));
    assert!(is_remote_name("s3://bucket/a/b.parquet") && !is_remote_name("s3://bucket/a/b.tif") && !is_remote_name("ftp://x.org/a.fgb"));
}

#[test]
fn login_bound_remote_files_keep_their_request_address() {
    crate::login_address::set_validator(remote::testing::validate_login);
    for (name, parquet) in [("bound-grid.fgb", false), ("bound-grid.geoparquet", true)] {
        let inner = format!("https://test.example/{name}?signature=a%2Fb#view");
        let address = crate::login_address::bind("0123456789abcdef", "https://test.example", &inner).unwrap();
        assert!(is_remote_name(&address));
        assert_eq!(url_name(&address), "bound-grid");
        remote::testing::serve(&address, big(4, parquet));
        let file = remote::file(&address);
        assert_eq!(file.url(), address);
        let source = open_driven(&address);
        assert_eq!(source.info().count, Some(16));
    }
    let image = crate::login_address::bind("0123456789abcdef", "https://test.example", "https://test.example/a.tif").unwrap();
    assert!(!is_remote_name(&image));
}

#[test]
fn flatgeobuf_stand_in_is_a_feature() {
    // The stand-in a sample passes over must pass the flatbuffers verifier, as real features do.
    let body = &fgb::STAND_IN_FOR_TESTS[4..];
    assert!(flatgeobuf::size_prefixed_root_as_feature(fgb::STAND_IN_FOR_TESTS.as_slice()).is_ok(), "{body:?}");
}

/// Writes a dataset as a Shapefile at `path` (its `.shp`), with the other files beside it.
fn write_shapefile(ds: &Dataset, path: &Path) {
    let (parts, _) = crate::io::shapefile_parts(ds).unwrap();
    for (ext, bytes) in parts {
        std::fs::write(path.with_extension(ext), bytes).unwrap();
    }
}

/// The points as a Shapefile.
fn points_shapefile(path: &Path) {
    let mut ds =
        Dataset::new("grid", GeometryType::Point, vec![Field::new("name", FieldType::Text), Field::new("value", FieldType::Integer)]);
    for (c, name, v) in points() {
        ds.push(Some(Geometry::Point(c)), vec![Value::Text(name), Value::Int(v)]);
    }
    write_shapefile(&ds, path);
}

#[test]
fn shapefile_read_in_place() {
    let p = dir().join("grid.shp");
    points_shapefile(&p);
    let src = open_path(&p, None).unwrap();
    assert_eq!(src.info().format, "Shapefile");
    check(src.as_ref(), true);
    assert_eq!(shp::record_count(&p), Some(600));
}

#[test]
fn shapefile_in_place_matches_one_read_whole() {
    let d = dir().join("shp-whole");
    std::fs::create_dir_all(&d).unwrap();
    let mut lines =
        Dataset::new("roads", GeometryType::Polyline, vec![Field::new("len", FieldType::Double), Field::new("ok", FieldType::Boolean)]);
    lines.push(
        Some(Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(1.5, 2.25)], vec![Coord::new(3.0, 3.0), Coord::new(4.0, 5.0)]])),
        vec![Value::Float(2.5), Value::Bool(true)],
    );
    lines.push(Some(Geometry::Line(vec![vec![Coord::new(-7.0, 8.0), Coord::new(9.0, -10.0)]])), vec![Value::Null, Value::Bool(false)]);
    let mut multi = Dataset::new("pairs", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
    multi.push(Some(Geometry::MultiPoint(vec![Coord::new(1.0, 1.0), Coord::new(2.0, 2.0)])), vec![Value::Text("pair".into())]);
    multi.push(Some(Geometry::MultiPoint(vec![Coord::new(-3.0, 4.0)])), vec![Value::Text("one".into())]);
    for ds in [export_sample(), lines, multi] {
        let p = d.join(format!("{}.shp", ds.name));
        write_shapefile(&ds, &p);
        let (whole, _) = crate::io::read_path(&p).unwrap();
        let (inplace, report) = materialize(open_path(&p, None).unwrap().as_ref()).unwrap();
        assert!(report.messages.is_empty(), "{:?}", report.messages);
        assert_eq!(inplace.geometry_type, whole.geometry_type, "{}", ds.name);
        assert_eq!(inplace.fields, whole.fields, "{}", ds.name);
        assert_eq!(inplace.features, whole.features, "{}", ds.name);
    }
}

#[test]
fn shapefile_in_place_reprojects_from_its_prj() {
    let d = dir().join("shp-utm");
    std::fs::create_dir_all(&d).unwrap();
    let p = d.join("paris.shp");
    let mut ds = Dataset::new("paris", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
    // Paris in UTM zone 31N.
    ds.push(Some(Geometry::Point(Coord::new(451_219.33, 5_411_550.20))), vec![Value::Text("paris".into())]);
    write_shapefile(&ds, &p);
    let utm = r#"PROJCS["WGS_1984_UTM_Zone_31N",GEOGCS["GCS_WGS_1984",DATUM["D_WGS_1984",SPHEROID["WGS_1984",6378137.0,298.257223563]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Transverse_Mercator"],PARAMETER["False_Easting",500000.0],PARAMETER["False_Northing",0.0],PARAMETER["Central_Meridian",3.0],PARAMETER["Scale_Factor",0.9996],PARAMETER["Latitude_Of_Origin",0.0],UNIT["Meter",1.0]]"#;
    std::fs::write(p.with_extension("prj"), utm).unwrap();
    let src = open_path(&p, None).unwrap();
    let e = src.info().extent;
    assert!((e.min_x - 2.335).abs() < 1e-6 && (e.min_y - 48.855).abs() < 1e-6, "{e:?}");
    assert!(src.info().notes.is_empty(), "{:?}", src.info().notes);
    // A longitude/latitude extent finds it; one beside it doesn't.
    let near = src.read(&Request { extent: Some(Extent::new(2.0, 48.5, 2.5, 49.0)), limit: 10 }).unwrap();
    let c = near.features.first().and_then(|f| f.geometry.as_ref()).and_then(Geometry::label_point).unwrap();
    assert!((c.x - 2.335).abs() < 1e-6 && (c.y - 48.855).abs() < 1e-6, "{c:?}");
    assert!(src.read(&Request { extent: Some(Extent::new(3.0, 48.5, 3.5, 49.0)), limit: 10 }).unwrap().features.is_empty());
    // Without the .prj the coordinates are taken as they are, and that's said.
    std::fs::remove_file(p.with_extension("prj")).unwrap();
    let src = open_path(&p, None).unwrap();
    assert!(src.info().notes.iter().any(|n| n.contains("not longitude/latitude")), "{:?}", src.info().notes);
}

#[test]
fn damaged_shapefiles_are_errors_or_skips_not_panics() {
    let d = dir().join("shp-junk");
    std::fs::create_dir_all(&d).unwrap();
    let good = d.join("good.shp");
    write_shapefile(&export_sample(), &good);
    let (shp, dbf) = (std::fs::read(&good).unwrap(), std::fs::read(good.with_extension("dbf")).unwrap());
    let p = d.join("cut.shp");
    let read_all = |p: &Path| {
        let read = || {
            if let Ok(s) = open_path(p, None) {
                let _ = s.read(&Request::all());
                let _ = s.read(&Request { extent: Some(Extent::new(-10.0, -10.0, 10.0, 10.0)), limit: 1 });
            }
        };
        #[cfg(not(target_arch = "wasm32"))]
        shp::with_test_cache(d.join("indexes"), read);
        #[cfg(target_arch = "wasm32")]
        read();
    };
    std::fs::write(p.with_extension("dbf"), &dbf).unwrap();
    for n in 0..shp.len() {
        std::fs::write(&p, &shp[..n]).unwrap();
        read_all(&p);
    }
    // Every byte of the shapes flipped in turn.
    for i in 100..shp.len() {
        let mut bad = shp.clone();
        bad[i] ^= 0xA5;
        std::fs::write(&p, &bad).unwrap();
        read_all(&p);
    }
    std::fs::write(&p, &shp).unwrap();
    for n in 0..dbf.len() {
        std::fs::write(p.with_extension("dbf"), &dbf[..n]).unwrap();
        read_all(&p);
    }
    std::fs::remove_file(p.with_extension("dbf")).unwrap();
    assert!(open_path(&p, None).is_err());
}

/// Reads every Shapefile under `RUSTGIS_SAMPLE_SHAPEFILES` both ways, in place and whole, and
/// compares them: `cargo test --release -p rustgis-data -- --ignored shapefile_samples --nocapture`.
#[test]
#[ignore]
fn shapefile_samples() {
    let Ok(dir) = std::env::var("RUSTGIS_SAMPLE_SHAPEFILES") else { return };
    fn walk(d: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("shp")) {
                out.push(p);
            }
        }
    }
    let mut paths = Vec::new();
    walk(Path::new(&dir), &mut paths);
    paths.sort();
    let mut differ = Vec::new();
    for p in paths {
        let t = std::time::Instant::now();
        let whole = crate::io::read_path(&p);
        let whole_t = t.elapsed();
        let t = std::time::Instant::now();
        let inplace = open_path(&p, None).and_then(|s| materialize(s.as_ref()));
        let inplace_t = t.elapsed();
        let (Ok((whole, _)), Ok((inplace, _))) = (&whole, &inplace) else {
            println!("{}: whole {:?} / in place {:?}", p.display(), whole.as_ref().err(), inplace.as_ref().err());
            continue;
        };
        let same_geometry = whole.features.iter().zip(&inplace.features).all(|(a, b)| a.geometry == b.geometry);
        let same = whole.features == inplace.features && whole.fields == inplace.fields;
        println!(
            "{}: {} features, whole {whole_t:?}, in place {inplace_t:?}; same={same} (geometry {same_geometry}, fields {})",
            p.display(),
            whole.features.len(),
            whole.fields == inplace.fields
        );
        if !same_geometry || whole.features.len() != inplace.features.len() {
            differ.push(p.display().to_string());
        }
    }
    assert!(differ.is_empty(), "{differ:?}");
}

#[test]
fn shapefile_in_place_keeps_large_whole_numbers_exact() {
    let d = dir().join("shp-ids");
    std::fs::create_dir_all(&d).unwrap();
    let p = d.join("ids.shp");
    let mut ds = Dataset::new("ids", GeometryType::Point, vec![Field::new("id", FieldType::Integer)]);
    for i in 0..5 {
        ds.push(Some(Geometry::Point(Coord::new(i as f64, 0.0))), vec![Value::Int(i)]);
    }
    write_shapefile(&ds, &p);
    // The id column (18 digits, no decimals) as another program writes it: values past f64's
    // exact integers, whole ones written with a point or exponent, and one that isn't whole.
    let mut dbf = std::fs::read(p.with_extension("dbf")).unwrap();
    let (header, row) = (u16::from_le_bytes([dbf[8], dbf[9]]) as usize, u16::from_le_bytes([dbf[10], dbf[11]]) as usize);
    for (i, text) in ["123456789012345678", "-98765432109876543", "12.0", "1e3", "12.5"].iter().enumerate() {
        let at = header + i * row + 1;
        dbf[at..at + 18].copy_from_slice(format!("{text:>18}").as_bytes());
    }
    std::fs::write(p.with_extension("dbf"), &dbf).unwrap();
    let src = open_path(&p, None).unwrap();
    assert_eq!(src.info().fields[0].ty, FieldType::Integer);
    let got: Vec<Value> = src.read(&Request::all()).unwrap().features.into_iter().map(|f| f.attrs[0].clone()).collect();
    assert_eq!(
        got,
        vec![
            Value::Int(123_456_789_012_345_678),
            Value::Int(-98_765_432_109_876_543),
            Value::Int(12),
            Value::Int(1000),
            Value::Float(12.5)
        ]
    );
}

#[test]
fn geojson_read_in_place() {
    let mut ds =
        Dataset::new("grid", GeometryType::Point, vec![Field::new("name", FieldType::Text), Field::new("value", FieldType::Integer)]);
    for (c, name, v) in points() {
        ds.push(Some(Geometry::Point(c)), vec![Value::Text(name), Value::Int(v)]);
    }
    let p = dir().join("grid.geojson");
    crate::io::write_path(&ds, &p).unwrap();
    let src = open_path(&p, None).unwrap();
    assert_eq!(src.info().format, "GeoJSON");
    check(src.as_ref(), false);
}

#[test]
fn geojson_in_place_rejects_partial_collections_with_a_notice() {
    let text = r#"{"type":"FeatureCollection","features":[
        {"type":"Feature","properties":{},"geometry":{"type":"GeometryCollection","geometries":[
            {"type":"Point","coordinates":[1,2]},
            {"type":"Polygon","coordinates":[[[0,0],[4,0],[4,4],[0,0]]]}
        ]}},
        {"type":"Feature","properties":{},"geometry":{"type":"GeometryCollection","geometries":[
            {"type":"Point","coordinates":[8,9]}
        ]}}
    ]}"#;
    let p = dir().join("collections.geojson");
    std::fs::write(&p, text).unwrap();
    let (whole, report) = crate::io::read_geojson(text, "x").unwrap();
    assert_eq!(whole.features.len(), 1);
    assert_eq!(report.skipped, 1);
    for in_memory in [crate::io::MAX_BYTES, 0] {
        let src = geojson::GeoJsonSource::open_with(&p, "x", in_memory).unwrap();
        assert_eq!(src.info().count, Some(1));
        assert_eq!(src.skipped_at_open(), 1);
        assert_eq!(src.info().notes, ["1 features had unreadable geometry"]);
        let (ds, streamed_report) = materialize(&src).unwrap();
        assert_eq!(ds.features, whole.features);
        assert_eq!(streamed_report.skipped, report.skipped);
        assert_eq!(streamed_report.messages, report.messages.iter().map(|m| format!("x: {m}")).collect::<Vec<_>>());
    }
    std::fs::remove_file(p).unwrap();
}

#[test]
fn geojson_in_place_matches_one_read_whole() {
    let d = dir().join("geojson-whole");
    std::fs::create_dir_all(&d).unwrap();
    // Types widened over features, a feature with geometry that can't be read (its properties
    // still count), one without geometry, mixed kinds, nested values and repeated keys.
    let mixed = r#"{"features":[
        {"type":"Feature","properties":{"a":1,"b":null,"n":{"x":[1,2]}},"geometry":{"type":"Point","coordinates":[1,2]}},
        {"type":"Feature","properties":{"a":2.5,"c":"x"},"geometry":{"type":"Nope"}},
        {"type":"Feature","properties":{"c":true,"d":null},"geometry":null},
        {"type":"Feature","properties":{"a":3},"geometry":{"type":"Polygon","coordinates":[[[0,0],[4,0],[4,4],[0,0]],[[1,1],[2,1],[2,2],[1,1]]]}},
        {"type":"Feature","properties":{"b":"t"},"geometry":{"type":"MultiLineString","coordinates":[[[0,0],[1,1]],[[5,5],[6,7]]]}},
        null,
        {"type":"Feature","properties":{},"geometry":{"type":"MultiPoint","coordinates":[[9,9],[8,8]]}}
    ],"type":"FeatureCollection","name":"m"}"#;
    let docs = [("mixed.geojson", mixed.to_string()), ("parcels.json", crate::io::to_geojson(&export_sample()).to_string())];
    for ((file, text), in_memory) in docs.iter().flat_map(|d| [(d, crate::io::MAX_BYTES), (d, 0)]) {
        let p = d.join(file);
        std::fs::write(&p, text).unwrap();
        let (whole, whole_report) = crate::io::read_geojson(text, "x").unwrap();
        // Scanned in memory, and through a buffer, as a file too large to hold is.
        let src = geojson::GeoJsonSource::open_with(&p, "x", in_memory).unwrap();
        let (inplace, report) = materialize(&src).unwrap();
        assert_eq!(inplace.fields, whole.fields, "{file}");
        assert_eq!(inplace.geometry_type, whole.geometry_type, "{file}");
        assert_eq!(inplace.features, whole.features, "{file}");
        assert_eq!(report.messages.len(), whole_report.messages.len(), "{file}: {:?}", report.messages);
        // Extent reads skip features without geometry, and find each by its box.
        let b = src.read(&Request { extent: Some(Extent::new(0.5, 0.5, 1.5, 2.5)), limit: 100 }).unwrap();
        assert!(b.features.iter().all(|f| f.geometry.is_some()), "{file}");
    }
    // Not a FeatureCollection, or not GeoJSON: not read in place.
    for (file, text) in [
        ("one.geojson", r#"{"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[1,2]}}"#),
        ("bare.geojson", r#"{"type":"Point","coordinates":[1,2]}"#),
        ("topo.json", r#"{"type":"Topology","objects":{},"arcs":[]}"#),
        ("list.json", "[1, 2]"),
        ("trailing.geojson", r#"{"type":"FeatureCollection","features":[]} {"#),
    ] {
        let p = d.join(file);
        std::fs::write(&p, text).unwrap();
        assert!(open_path(&p, None).is_err() && geojson::GeoJsonSource::open_with(&p, "x", 0).is_err(), "{file}");
    }
}

/// A GeoJSON FeatureCollection of Arnhem's municipal outline's box and a point in RD New
/// (EPSG:28992), `crs` being what the file declares (left out for `None`).
fn rd_geojson(crs: Option<&str>) -> String {
    let crs = crs.map(|c| format!(r#""crs":{c},"#)).unwrap_or_default();
    format!(
        r#"{{"type":"FeatureCollection","name":"arnhem",{crs}"features":[
        {{"type":"Feature","properties":{{"n":"area"}},"geometry":{{"type":"Polygon","coordinates":[[[183548,438459],[196442,438459],[196442,454549],[183548,454549],[183548,438459]]]}}}},
        {{"type":"Feature","properties":{{"n":"point"}},"geometry":{{"type":"Point","coordinates":[188000,445000]}}}}]}}"#
    )
}

fn named(name: &str) -> String {
    format!(r#"{{"type":"name","properties":{{"name":"{name}"}}}}"#)
}

/// What a GeoJSON text reads as through every path: whole, and in place scanned in memory and
/// through a buffer; the paths must agree. Returns the dataset, its messages and the source's
/// `info.crs`.
fn geojson_paths(file: &str, text: &str) -> (Dataset, Vec<String>, String) {
    let d = dir().join("geojson-crs");
    std::fs::create_dir_all(&d).unwrap();
    let p = d.join(file);
    std::fs::write(&p, text).unwrap();
    let (whole, report) = crate::io::read_geojson(text, "x").unwrap();
    for in_memory in [crate::io::MAX_BYTES, 0] {
        let src = geojson::GeoJsonSource::open_with(&p, "x", in_memory).unwrap();
        let (inplace, in_report) = materialize(&src).unwrap();
        assert_eq!(inplace.features, whole.features, "{file}");
        assert_eq!(inplace.crs, whole.crs, "{file}");
        assert_eq!(inplace.crs_incomplete, whole.crs_incomplete, "{file}");
        assert_eq!(inplace.geometry_type, whole.geometry_type, "{file}");
        assert_eq!(report.skipped, in_report.skipped, "{file}");
        let strip = |m: &Vec<String>| m.iter().map(|m| m.trim_start_matches("x: ").to_string()).collect::<Vec<_>>();
        assert_eq!(strip(&report.messages), strip(&in_report.messages), "{file}");
        // The extent a source says is that of the features it reads (those that convert).
        assert_eq!(src.info().extent, whole.extent(), "{file}");
        assert_eq!(src.info().count, Some(whole.features.len() as u64), "{file}");
        assert_eq!(src.info().coordinate_system(), whole.crs, "{file}");
    }
    let src = geojson::GeoJsonSource::open_with(&p, "x", crate::io::MAX_BYTES).unwrap();
    (whole, report.messages, src.info().crs.clone())
}

fn in_arnhem(e: &Extent) -> bool {
    (5.8..6.0).contains(&e.min_x) && (5.8..6.0).contains(&e.max_x) && (51.9..52.1).contains(&e.min_y) && (51.9..52.1).contains(&e.max_y)
}

#[test]
fn geojson_legacy_crs_is_honoured_by_both_paths() {
    // The issue #930 declaration, and the other ways EPSG:28992 is named.
    for name in [
        "urn:ogc:def:crs:EPSG::28992",
        "urn:ogc:def:crs:EPSG:6.6:28992",
        "EPSG:28992",
        "epsg:28992",
        "http://www.opengis.net/def/crs/EPSG/0/28992",
    ] {
        let (ds, messages, info_crs) = geojson_paths("rd.geojson", &rd_geojson(Some(&named(name))));
        assert!(in_arnhem(&ds.extent()), "{name}: {:?}", ds.extent());
        assert_eq!(ds.crs, Some(crate::raster::Crs::Epsg(28992)), "{name}");
        assert_eq!(info_crs, "EPSG:28992", "{name}");
        assert!(messages.is_empty(), "{name}: {messages:?}");
        assert!(!ds.crs_incomplete);
        assert_eq!(ds.geometry_type, GeometryType::Polygon);
    }
    // Reads in place go by the layer's own coordinates: a window of longitude/latitude finds the
    // features, one elsewhere finds none.
    let p = dir().join("geojson-crs").join("rd.geojson");
    for in_memory in [crate::io::MAX_BYTES, 0] {
        let src = geojson::GeoJsonSource::open_with(&p, "x", in_memory).unwrap();
        let at = |e: Extent| src.read(&Request { extent: Some(e), limit: 100 }).unwrap().features.len();
        assert_eq!(at(Extent::new(5.85, 51.95, 5.86, 51.96)), 1, "the polygon only");
        assert_eq!(at(Extent::new(5.95, 52.02, 5.96, 52.03)), 1, "the polygon only");
        assert_eq!(at(Extent::new(5.8, 51.9, 6.0, 52.1)), 2);
        assert_eq!(at(Extent::new(0.0, 0.0, 1.0, 1.0)), 0);
        assert_eq!(at(Extent::new(500000.0, 438459.0, 600000.0, 454549.0)), 0, "the file's coordinates aren't lon/lat");
    }
}

#[test]
fn geojson_without_a_crs_or_in_lonlat_is_unchanged() {
    let fc = |crs: &str| {
        format!(
            r#"{{"type":"FeatureCollection",{crs}"features":[{{"type":"Feature","properties":{{}},"geometry":{{"type":"Polygon","coordinates":[[[5.8,51.9],[6.0,51.9],[6.0,52.1],[5.8,52.1],[5.8,51.9]]]}}}},
            {{"type":"Feature","properties":{{}},"geometry":{{"type":"Point","coordinates":[-122.5,37.75]}}}}]}}"#
        )
    };
    let wgs84 = [
        String::new(),
        r#""crs":null,"#.to_string(),
        format!(r#""crs":{},"#, named("urn:ogc:def:crs:OGC:1.3:CRS84")),
        format!(r#""crs":{},"#, named("urn:ogc:def:crs:OGC::CRS84")),
        format!(r#""crs":{},"#, named("OGC::CRS84")),
        format!(r#""crs":{},"#, named("OGC:1.3:CRS84")),
        format!(r#""crs":{},"#, named("OGC:CRS84")),
        format!(r#""crs":{},"#, named("CRS84")),
        format!(r#""crs":{},"#, named("http://www.opengis.net/def/crs/OGC/1.3/CRS84")),
        format!(r#""crs":{},"#, named("EPSG:4326")),
        format!(r#""crs":{},"#, named("urn:ogc:def:crs:EPSG::4326")),
    ];
    let (base, _, _) = geojson_paths("plain.geojson", &fc(""));
    assert_eq!(base.extent(), Extent::new(-122.5, 37.75, 6.0, 52.1));
    for crs in &wgs84 {
        let (ds, messages, info_crs) = geojson_paths("wgs84.geojson", &fc(crs));
        assert_eq!(ds.features, base.features, "{crs}: not a coordinate moved");
        assert_eq!(ds.crs, None, "{crs}");
        assert_eq!(info_crs, "EPSG:4326", "{crs}");
        assert!(messages.is_empty(), "{crs}: {messages:?}");
        assert!(!ds.crs_incomplete);
    }
}

#[test]
fn geojson_geographic_crs_keeps_x_y_order() {
    // ETRS89 (EPSG:4258) is latitude/longitude in EPSG, but a legacy GeoJSON crs still gives
    // longitude, latitude: the point stays in the Netherlands, not at 52 E 5.9 N.
    let text = r#"{"type":"FeatureCollection","crs":{"type":"name","properties":{"name":"urn:ogc:def:crs:EPSG::4258"}},
        "features":[{"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[5.9,52.0]}}]}"#;
    let (ds, messages, info_crs) = geojson_paths("etrs89.geojson", text);
    assert!(messages.is_empty(), "{messages:?}");
    assert_eq!(ds.crs, Some(crate::raster::Crs::Epsg(4258)));
    assert_eq!(info_crs, "EPSG:4258");
    let Some(Geometry::Point(c)) = ds.features.first().and_then(|f| f.geometry.clone()) else { panic!("a point") };
    assert!((c.x - 5.9).abs() < 1e-3 && (c.y - 52.0).abs() < 1e-3, "{c:?}");
}

#[test]
fn geojson_crs_not_understood_is_said_and_coordinates_kept() {
    let long = "x".repeat(100_000);
    let declared = [
        (r#"{"type":"link","properties":{"href":"http://example.com/crs","type":"proj4"}}"#.to_string(), "of type \"link\""),
        (named("urn:ogc:def:crs:OGC:1.3:CRS83"), "CRS83"),
        (named("EPSG:1"), "EPSG:1"),
        (named("EPSG:99999999999"), "EPSG:99999999999"),
        (named("EPSG:abc"), "EPSG:abc"),
        (named("Amersfoort / RD New"), "Amersfoort"),
        (named("VN-2000 / UTM zone 48N EPSG:28992"), "VN-2000"),
        (named("OGC:CRS84:EPSG"), "OGC:CRS84:EPSG"),
        (named("x:EPSG:28992"), "x:EPSG:28992"),
        (named("urn:ogc:def:crs:EPSG::28992:extra"), "extra"),
        (named("urn:x:EPSG:6.6:28992"), "urn:x"),
        (named("http://example.com/def/crs/EPSG/0/28992"), "example.com"),
        (named("EPSG:28992:28992"), "EPSG:28992:28992"),
        (named("+proj=utm +zone=31 +datum=WGS84 EPSG"), "+proj=utm"),
        (named("PROJCS[\\\"RD\\\",EPSG]"), "PROJCS"),
        (named(&format!("EPSG:28992{long}")), "…"),
        (r#"{"type":"name","properties":{"name":28992}}"#.to_string(), "of type \"name\""),
        (r#"{"type":"name","properties":{}}"#.to_string(), "of type \"name\""),
        (r#"{"type":"name"}"#.to_string(), "of type \"name\""),
        (r#"{"type":5}"#.to_string(), "of an unknown shape"),
        (r#"{}"#.to_string(), "of an unknown shape"),
        (r#""EPSG:28992""#.to_string(), "of an unknown shape"),
        (r#"28992"#.to_string(), "of an unknown shape"),
        (r#"true"#.to_string(), "of an unknown shape"),
        (r#"["EPSG:28992"]"#.to_string(), "of an unknown shape"),
        (r#"[[[[[[[[[[[[[[[[[[[[1]]]]]]]]]]]]]]]]]]]]"#.to_string(), "of an unknown shape"),
        (format!(r#"{{"type":"{long}"}}"#), "…"),
    ];
    for (crs, shown) in &declared {
        let (ds, messages, info_crs) = geojson_paths("odd.geojson", &rd_geojson(Some(crs)));
        // Not taken as WGS 84 without saying so: one message naming the declaration.
        assert_eq!(messages.len(), 1, "{crs:.60}: {messages:?}");
        assert!(messages[0].starts_with("GeoJSON crs ") && messages[0].contains(shown), "{crs:.60}: {}", messages[0]);
        assert!(messages[0].contains("longitude/latitude"), "{}", messages[0]);
        assert!(messages[0].len() < 400, "a declaration is clipped in a message: {}", messages[0].len());
        // Read as they are, nothing converted or lost.
        assert_eq!(ds.features.len(), 2, "{crs:.60}");
        assert_eq!(ds.extent(), Extent::new(183548.0, 438459.0, 196442.0, 454549.0), "{crs:.60}");
        assert_eq!(ds.crs, None);
        assert_eq!(info_crs, "EPSG:4326");
    }
}

#[test]
fn geojson_crs_is_read_only_from_the_feature_collection() {
    // Not part of the legacy text: on a feature, a geometry or a lone Feature. Ignored, quietly.
    let crs = named("EPSG:28992");
    let docs = [
        format!(
            r#"{{"type":"FeatureCollection","features":[{{"type":"Feature","crs":{crs},"properties":{{}},"geometry":{{"type":"Point","coordinates":[188000,445000]}}}}]}}"#
        ),
        format!(
            r#"{{"type":"FeatureCollection","features":[{{"type":"Feature","properties":{{}},"geometry":{{"type":"Point","coordinates":[188000,445000],"crs":{crs}}}}}]}}"#
        ),
        format!(r#"{{"type":"Feature","crs":{crs},"properties":{{}},"geometry":{{"type":"Point","coordinates":[188000,445000]}}}}"#),
        format!(r#"{{"type":"Point","coordinates":[188000,445000],"crs":{crs}}}"#),
    ];
    for text in &docs {
        let (ds, report) = crate::io::read_geojson(text, "x").unwrap();
        assert!(report.messages.is_empty(), "{text}");
        assert_eq!(ds.crs, None, "{text}");
        assert_eq!(ds.extent(), Extent::new(188000.0, 445000.0, 188000.0, 445000.0), "{text}");
    }
    // A FeatureCollection whose features aren't an array is read whole, its crs still honoured.
    let (ds, report) = crate::io::read_geojson(&format!(r#"{{"type":"FeatureCollection","crs":{crs},"features":7}}"#), "x").unwrap();
    assert!(ds.features.is_empty() && report.messages.is_empty());
}

#[test]
fn geojson_features_that_cant_be_converted_are_skipped_and_counted() {
    // A coordinate the declared system can't convert (UTM 1e9 m east), or converts to beyond
    // longitude/latitude (Web Mercator 1e9 m east is longitude 8983°): the feature is dropped, and
    // its height with it (no Z notice for the 2D features kept).
    for (crs, good, bad) in [("EPSG:32617", "[500000,4649776]", "[1e9,0,30]"), ("EPSG:3857", "[1000000,6000000]", "[1e9,0,30]")] {
        let text = format!(
            r#"{{"type":"FeatureCollection","crs":{},"features":[
            {{"type":"Feature","properties":{{"n":1}},"geometry":{{"type":"Point","coordinates":{good}}}}},
            {{"type":"Feature","properties":{{"n":2}},"geometry":{{"type":"Point","coordinates":{bad}}}}},
            {{"type":"Feature","properties":{{"n":3}},"geometry":{{"type":"Point","coordinates":{good}}}}}]}}"#,
            named(crs)
        );
        let (ds, messages, info_crs) = geojson_paths("far.geojson", &text);
        assert_eq!(ds.features.len(), 2, "{crs}");
        assert_eq!(ds.features.iter().map(|f| f.id).collect::<Vec<_>>(), [1, 3], "{crs}: ids are the features' places in the file");
        assert_eq!(messages.len(), 1, "{crs}: {messages:?}");
        assert!(messages[0].contains(&format!("1 features couldn't be converted from {crs}")), "{}", messages[0]);
        assert_eq!(info_crs, crs);
        assert!(ds.crs_incomplete, "{crs}: a projection can't be defined again over lost features");
        assert!(!ds.zm_dropped, "{crs}");
        let Some(Geometry::Point(c)) = ds.features.first().and_then(|f| f.geometry.clone()) else { panic!("a point") };
        assert!(c.x.abs() < 180.0 && c.y.abs() < 90.0, "{crs}: {c:?}");
        if crs == "EPSG:32617" {
            assert!((c.x + 81.0).abs() < 1e-6 && (c.y - 42.0).abs() < 1e-3, "{c:?}");
        }
    }
}

/// A point in Web Mercator that converts, one that doesn't (longitude 8983°), and a polygon.
const MERC_OK: &str = r#"{"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[1000000,6000000]}}"#;
const MERC_BAD: &str = r#"{"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[1e9,0]}}"#;
const MERC_POLY: &str = r#"{"type":"Feature","properties":{},"geometry":{"type":"Polygon","coordinates":[[[1000000,6000000],[1100000,6000000],[1100000,6100000],[1000000,6000000]]]}}"#;

fn merc_collection(features: &[&str]) -> String {
    format!(r#"{{"type":"FeatureCollection","crs":{},"features":[{}]}}"#, named("EPSG:3857"), features.join(","))
}

#[test]
fn geojson_dropped_features_do_not_set_the_geometry_type() {
    // Two points that can't be placed and one polygon that can: a polygon layer, as read whole.
    let (ds, _, _) = geojson_paths("kinds.geojson", &merc_collection(&[MERC_BAD, MERC_POLY, MERC_BAD]));
    assert_eq!(ds.geometry_type, GeometryType::Polygon);
    assert_eq!(ds.features.len(), 1);
    // Nothing that can be placed: no geometry type at all, both ways.
    let (ds, messages, _) = geojson_paths("none.geojson", &merc_collection(&[MERC_BAD, MERC_BAD]));
    assert_eq!(ds.geometry_type, GeometryType::None);
    assert!(ds.features.is_empty() && ds.crs_incomplete);
    assert_eq!(messages.len(), 1, "{messages:?}");
}

#[test]
fn geojson_sampling_skips_dropped_features() {
    // Unplaceable features at every other place must not use up the sample: a read asking for
    // two of the three placeable ones gets two, and says it matched three.
    let text = merc_collection(&[MERC_BAD, MERC_OK, MERC_BAD, MERC_OK, MERC_BAD, MERC_OK]);
    let p = dir().join("geojson-crs").join("alternating.geojson");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, &text).unwrap();
    for in_memory in [crate::io::MAX_BYTES, 0] {
        let src = geojson::GeoJsonSource::open_with(&p, "x", in_memory).unwrap();
        assert_eq!(src.info().count, Some(3));
        let all = src.read(&Request { extent: None, limit: 100 }).unwrap();
        assert_eq!((all.features.len(), all.matched, all.skipped, all.complete), (3, 3, 0, true));
        let some = src.read(&Request { extent: None, limit: 2 }).unwrap();
        assert_eq!((some.features.len(), some.matched, some.skipped, some.complete), (2, 3, 0, false));
        assert!(some.features.iter().all(|f| f.geometry.is_some()));
        let ids: Vec<u64> = all.features.iter().map(|f| f.id).collect();
        assert_eq!(ids, [2, 4, 6]);
    }
}

#[test]
fn geojson_unreadable_feature_alone_does_not_make_the_layer_incomplete() {
    // A declared system, one feature with geometry that can't be read, and all the others
    // placed: nothing was lost to the system, so Define Projection stays available, both ways.
    let bad = r#"{"type":"Feature","properties":{},"geometry":{"type":"Nope"}}"#;
    let (ds, messages, _) = geojson_paths("unreadable.geojson", &merc_collection(&[MERC_OK, bad]));
    assert_eq!(ds.features.len(), 1);
    assert!(!ds.crs_incomplete);
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(messages[0].contains("unreadable geometry"), "{}", messages[0]);
}

#[test]
fn damaged_geojson_is_an_error_not_a_panic() {
    let d = dir().join("geojson-junk");
    std::fs::create_dir_all(&d).unwrap();
    let text = crate::io::to_geojson(&export_sample()).to_string();
    let p = d.join("cut.geojson");
    for n in 0..text.len() {
        std::fs::write(&p, &text.as_bytes()[..n]).unwrap();
        for in_memory in [crate::io::MAX_BYTES, 0] {
            if let Ok(s) = geojson::GeoJsonSource::open_with(&p, "x", in_memory) {
                let _ = s.read(&Request::all());
            }
        }
    }
    // A file that changes after it's opened: features are skipped, not misread or a panic.
    std::fs::write(&p, &text).unwrap();
    let src = open_path(&p, None).unwrap();
    std::fs::write(&p, &text.as_bytes()[..text.len() / 2]).unwrap();
    let b = src.read(&Request::all()).unwrap();
    assert!(b.skipped > 0);
}

/// The issue #514 squares: ETRS89 / UTM zone 32N (EPSG:25832), or the same with the zone prefixed
/// to the easting (EPSG:4647).
fn utm_squares(code: u32) -> Dataset {
    let e0 = if code == 4647 { 32_500_000.0 } else { 500_000.0 };
    let text: String = std::iter::once("side;wkt\n".to_string())
        .chain([50.0, 100.0, 1000.0].iter().map(|s| {
            let (x0, y0, x1, y1) = (e0, 5_700_000.0, e0 + s, 5_700_000.0 + s);
            format!("{s} m;POLYGON (({x0} {y0}, {x1} {y0}, {x1} {y1}, {x0} {y1}, {x0} {y0}))\n")
        }))
        .collect();
    let opts = crate::io::CsvOptions {
        wkt: Some("wkt".into()),
        crs: Some(Crs::from_epsg(code).unwrap()),
        delimiter: Some(b';'),
        ..Default::default()
    };
    crate::io::read_csv_with(&text, "squares", &opts).unwrap().0
}

#[test]
fn exports_are_written_in_the_system_the_layer_was_read_in() {
    let d = dir().join("export-crs");
    std::fs::create_dir_all(&d).unwrap();
    for code in [25832u32, 4647] {
        let ds = utm_squares(code);
        let crs = Crs::from_epsg(code).unwrap();
        assert_eq!(ds.crs, Some(crs));
        let e0 = if code == 4647 { 32_500_000.0 } else { 500_000.0 };
        for ext in ["gpkg", "shp", "fgb"] {
            let p = d.join(format!("squares-{code}.{ext}"));
            crate::io::write_path_in(&ds, &p, crs).unwrap();
            // The file says the system, and reading it back gives the same places.
            let src = open_path(&p, None).unwrap();
            assert_eq!(src.info().crs, format!("EPSG:{code}"), "{ext}");
            let (back, _) = materialize(src.as_ref()).unwrap();
            assert_eq!(back.crs, Some(crs), "{ext}");
            // (FlatGeobuf orders features along its index.)
            for b in &back.features {
                let a = ds.features.iter().find(|a| a.attrs == b.attrs).unwrap();
                let (a, b) = (a.geometry.as_ref().unwrap().extent(), b.geometry.as_ref().unwrap().extent());
                assert!((a.min_x - b.min_x).abs() < 1e-9 && (a.max_y - b.max_y).abs() < 1e-9, "{ext}: {a:?} {b:?}");
            }
        }
        // The Shapefile holds the system's own coordinates, and a .prj GDAL reads by its code.
        let shp = d.join(format!("squares-{code}.shp"));
        let shapes = shapefile::read_shapes_as::<_, shapefile::Polygon>(&shp).unwrap();
        let b = shapes.last().unwrap().bbox();
        assert!((b.min.x - e0).abs() < 1e-6 && (b.max.x - e0 - 1000.0).abs() < 1e-6, "{b:?}");
        assert!((b.min.y - 5_700_000.0).abs() < 1e-6 && (b.max.y - 5_701_000.0).abs() < 1e-6, "{b:?}");
        let prj = std::fs::read_to_string(shp.with_extension("prj")).unwrap();
        assert!(prj.starts_with("PROJCS[") && prj.ends_with(&format!("AUTHORITY[\"EPSG\",\"{code}\"]]")), "{prj}");
    }
    // By default an export keeps the layer's system where the format can say it; a system without
    // an EPSG code only where its WKT is read back (a Shapefile's .prj).
    let mut ds = utm_squares(25832);
    let utm = Crs::from_epsg(25832).unwrap();
    assert_eq!(crate::io::default_export_crs(&ds, "GPKG"), utm);
    assert_eq!(crate::io::default_export_crs(&ds, "geojson"), Crs::LonLat);
    let wkt = crate::raster::wkt::to_wkt(utm).unwrap().replace(",AUTHORITY[\"EPSG\",\"25832\"]", "").replace("EPSG:25832", "My grid");
    let custom = crate::raster::wkt::crs_from_wkt(&wkt).unwrap();
    assert_eq!(custom.epsg(), 0);
    ds.crs = Some(custom);
    assert_eq!(crate::io::default_export_crs(&ds, "shp"), custom);
    assert_eq!(crate::io::default_export_crs(&ds, "fgb"), Crs::LonLat);
    // Asked for, it goes into a Shapefile, but not a file that would read back as longitude/latitude.
    assert!(crate::io::write_path_in(&ds, &d.join("custom.shp"), custom).is_ok());
    assert!(crate::io::write_path_in(&ds, &d.join("custom.gpkg"), custom).is_err());
    assert!(crate::io::write_bytes_in(&ds, "fgb", custom).is_err());
    // GeoJSON is longitude/latitude only.
    let ds = utm_squares(25832);
    assert!(crate::io::write_path_in(&ds, &d.join("squares.geojson"), Crs::from_epsg(25832).unwrap()).is_err());
}

/// A catalog index as AlphaEarth's is (a `bbox` struct, a nested `assets.data.href`), and patch
/// embeddings as Earth Index's are (a fixed-size list of floats a point), in row groups of 50.
fn write_index_and_vectors() -> Vec<u8> {
    use arrow_array::builder::{FixedSizeListBuilder, Float32Builder};
    use arrow_array::{BinaryArray, Float64Array, RecordBatch, StringArray, StructArray};
    use arrow_schema::{DataType, Field as AField, Fields, Schema};
    let pts = points();
    let wkbs: Vec<Vec<u8>> = pts.iter().map(|(c, ..)| wkb::encode(&Geometry::Point(*c))).collect();
    let bbox_fields: Fields =
        ["xmin", "ymin", "xmax", "ymax"].map(|n| Arc::new(AField::new(n, DataType::Float64, false))).into_iter().collect();
    let coord =
        |f: fn(&Coord) -> f64| Arc::new(Float64Array::from(pts.iter().map(|(c, ..)| f(c)).collect::<Vec<_>>())) as arrow_array::ArrayRef;
    let bbox = StructArray::new(bbox_fields.clone(), vec![coord(|c| c.x), coord(|c| c.y), coord(|c| c.x), coord(|c| c.y)], None);
    let href =
        Arc::new(StringArray::from(pts.iter().map(|p| format!("s3://bucket/{}.tiff", p.1)).collect::<Vec<_>>())) as arrow_array::ArrayRef;
    let data_fields: Fields = vec![Arc::new(AField::new("href", DataType::Utf8, true))].into_iter().collect();
    let data = Arc::new(StructArray::new(data_fields.clone(), vec![href], None)) as arrow_array::ArrayRef;
    let asset_fields: Fields = vec![Arc::new(AField::new("data", DataType::Struct(data_fields), true))].into_iter().collect();
    let assets = StructArray::new(asset_fields.clone(), vec![data], None);
    let mut emb = FixedSizeListBuilder::new(Float32Builder::new(), 4);
    for (_, _, v) in &pts {
        for k in 0..4 {
            emb.values().append_value((*v * 10 + k) as f32);
        }
        emb.append(true);
    }
    let emb = emb.finish();
    let schema = Arc::new(Schema::new(vec![
        AField::new("geometry", DataType::Binary, false),
        AField::new("bbox", DataType::Struct(bbox_fields), false),
        AField::new("assets", DataType::Struct(asset_fields), true),
        AField::new("embedding", arrow_array::Array::data_type(&emb).clone(), true),
    ]));
    let cols: Vec<arrow_array::ArrayRef> = vec![
        Arc::new(BinaryArray::from(wkbs.iter().map(|w| w.as_slice()).collect::<Vec<_>>())),
        Arc::new(bbox),
        Arc::new(assets),
        Arc::new(emb),
    ];
    let geo = r#"{"version":"1.0.0","primary_column":"geometry","columns":{"geometry":{"encoding":"WKB","geometry_types":["Point"]}}}"#;
    let props = ::parquet::file::properties::WriterProperties::builder()
        .set_max_row_group_row_count(Some(50))
        .set_key_value_metadata(Some(vec![::parquet::file::metadata::KeyValue::new("geo".to_string(), geo.to_string())]))
        .build();
    let mut out = Vec::new();
    let mut w = ::parquet::arrow::ArrowWriter::try_new(&mut out, schema.clone(), Some(props)).unwrap();
    w.write(&RecordBatch::try_new(schema, cols).unwrap()).unwrap();
    w.close().unwrap();
    out
}

#[test]
fn nested_columns_and_vectors_by_address() {
    let bytes = write_index_and_vectors();
    let url = "https://test.example/index.parquet";
    remote::testing::serve(url, bytes.clone());
    let e = Extent::new(10.0, 10.0, 40.0, 30.0);
    // The rows in the extent, with the nested column as a field.
    let (fields, batch) =
        query_url_blocking(url, vec!["assets.data.href".into(), "no.such".into()], Request { extent: Some(e), limit: 1000 }).unwrap();
    let k = fields.iter().position(|f| f.name == "assets.data.href").unwrap();
    assert!(!fields.iter().any(|f| f.name == "no.such"));
    assert_eq!(batch.features.len(), in_box(&e));
    let names: Vec<String> = points().iter().filter(|(c, ..)| e.contains(*c)).map(|p| format!("s3://bucket/{}.tiff", p.1)).collect();
    for f in &batch.features {
        assert!(names.contains(&f.attrs[k].to_string()), "{:?}", f.attrs[k]);
    }
    // The points in the extent with their vectors: four values each, as written.
    let v = vectors_url_blocking(url, e, "embedding".into(), (100, 10_000)).unwrap();
    assert_eq!((v.points.len(), v.dims, v.values.len()), (in_box(&e), 4, in_box(&e) * 4));
    for (i, p) in v.points.iter().enumerate() {
        let (_, _, id) = points().into_iter().find(|(c, ..)| (c.x, c.y) == *p).unwrap();
        assert_eq!(&v.values[i * 4..i * 4 + 4], &[(id * 10) as f32, (id * 10 + 1) as f32, (id * 10 + 2) as f32, (id * 10 + 3) as f32]);
    }
    // Past the limits: said, not read.
    let err = vectors_url_blocking(url, Extent::new(-180.0, -90.0, 180.0, 90.0), "embedding".into(), (2, 10_000)).unwrap_err();
    assert!(err.to_string().contains("zoom in"), "{err}");
    assert!(vectors_url_blocking(url, e, "nothing".into(), (100, 10_000)).is_err());
    // A cut file: errors, never a panic.
    for n in [0, 10, 100, bytes.len() / 2] {
        let cut = format!("https://test.example/cut-{n}.parquet");
        remote::testing::serve(&cut, bytes[bytes.len() - n..].to_vec());
        let _ = vectors_url_blocking(&cut, e, "embedding".into(), (100, 10_000));
    }
}

#[test]
fn shapefile_complete_chunks_keep_all_rows_order_attributes_and_budget() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("chunks.shp");
    let mut ds = Dataset::new("chunks", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
    for i in 0..523 {
        ds.push(Some(Geometry::Point(Coord::new(i as f64 * 0.01, 1.0))), vec![Value::Text(format!("point {i}"))]);
    }
    write_shapefile(&ds, &path);
    let source = shp::ShpSource::open_path(&path, "chunks").unwrap();
    let expected = source.read(&Request::all()).unwrap();
    assert!(source.streams_chunks());
    for budget in [256, 1024, 8192] {
        let mut rows = Vec::new();
        let mut calls = 0;
        let summary = source
            .visit_chunks(None, budget, &mut |chunk| {
                calls += 1;
                assert!(chunk_memory::features_bytes(&chunk) <= budget);
                rows.extend(chunk);
                Ok(())
            })
            .unwrap();
        assert!(calls > 1);
        assert!(summary.complete);
        assert!(summary.features.is_empty());
        assert_eq!(summary.matched, 523);
        assert_eq!(rows, expected.features);
    }
    let mut oversized = Vec::new();
    source
        .visit_chunks(None, 1, &mut |chunk| {
            assert_eq!(chunk.len(), 1);
            oversized.extend(chunk);
            Ok(())
        })
        .unwrap();
    assert_eq!(oversized, expected.features, "indivisible rows must remain complete");
    let mut calls = 0;
    assert!(
        source
            .visit_chunks(None, 256, &mut |_| {
                calls += 1;
                Err(DataError::Format("cancelled".into()))
            })
            .is_err()
    );
    assert_eq!(calls, 1);
    assert!(source.visit_chunks(None, 0, &mut |_| { panic!("zero-budget traversal must not emit a row") }).is_err());
    assert!(source.visit_chunks(Some(Extent::EMPTY), 0, &mut |_| panic!("zero budget must not emit")).is_err());
    let empty_path = dir.path().join("empty.shp");
    // The production exporter deliberately refuses an empty layer. Build a valid zero-record
    // fixture from its headers rather than changing that existing export contract.
    for ext in ["shp", "shx"] {
        let mut header = std::fs::read(path.with_extension(ext)).unwrap();
        header.truncate(100);
        header[24..28].copy_from_slice(&50i32.to_be_bytes());
        std::fs::write(empty_path.with_extension(ext), header).unwrap();
    }
    let mut header = std::fs::read(path.with_extension("dbf")).unwrap();
    let len = u16::from_le_bytes([header[8], header[9]]) as usize;
    header.truncate(len);
    header[4..8].copy_from_slice(&0u32.to_le_bytes());
    header.push(0x1a);
    std::fs::write(empty_path.with_extension("dbf"), header).unwrap();
    let empty = shp::ShpSource::open_path(&empty_path, "empty").unwrap();
    assert!(empty.visit_chunks(None, 0, &mut |_| panic!("zero budget must not emit")).is_err());
    let extent = Some(Extent::new(0.02, 0.0, 0.1, 2.0));
    let expected = source.read(&Request { extent, limit: 1000 }).unwrap();
    let mut rows = Vec::new();
    source
        .visit_chunks(extent, 256, &mut |chunk| {
            rows.extend(chunk);
            Ok(())
        })
        .unwrap();
    assert_eq!(rows, expected.features);
}

/// Every feature of a complete traversal, in order, checking each chunk against the budget.
fn visit_all(src: &dyn FeatureSource, extent: Option<Extent>, budget: usize) -> (Vec<Feature>, Batch, usize) {
    let (mut rows, mut calls) = (Vec::new(), 0);
    let summary = src
        .visit_chunks(extent, budget, &mut |chunk| {
            calls += 1;
            // Only a row that alone exceeds the budget comes over it, by itself.
            let alone_too_big = |c: &Vec<Feature>| {
                c.len() == 1 && c.iter().all(|f| chunk_memory::feature_bytes(f) + std::mem::size_of::<Feature>() > budget)
            };
            assert!(chunk_memory::features_bytes(&chunk) <= budget || alone_too_big(&chunk), "{}", src.info().format);
            rows.extend(chunk);
            Ok(())
        })
        .unwrap();
    (rows, summary, calls)
}

/// GeoPackage, FlatGeobuf and GeoParquet visit, a chunk at a time, exactly the features a whole
/// read gives (ids, attributes, order and skipped rows), for exports. They still draw from a
/// view window: a complete traversal on every render would be far slower.
#[test]
fn geopackage_flatgeobuf_and_geoparquet_visit_what_a_whole_read_gives() {
    let d = dir().join("visit");
    std::fs::create_dir_all(&d).unwrap();
    let (gpkg, gpkg_scan, fgb, pq, pq_plain) =
        (d.join("grid.gpkg"), d.join("scan.gpkg"), d.join("grid.fgb"), d.join("grid.parquet"), d.join("plain.parquet"));
    write_gpkg(&gpkg, true);
    write_gpkg(&gpkg_scan, false);
    write_fgb(&fgb);
    write_parquet(&pq, true);
    write_parquet(&pq_plain, false);
    let mut sources: Vec<Box<dyn FeatureSource>> = Vec::new();
    for p in [&gpkg, &gpkg_scan, &fgb, &pq, &pq_plain] {
        sources.push(open_path(p, None).unwrap());
    }
    for p in [&gpkg, &fgb, &pq] {
        sources.push(open_bytes(&p.file_name().unwrap().to_string_lossy(), std::fs::read(p).unwrap(), None).unwrap());
    }
    // The browser's GeoPackage reader.
    sources.push(Box::new(gpkg_mem::GpkgMemSource::open_shared(Shared(Arc::new(std::fs::read(&gpkg_scan).unwrap())), None).unwrap()));
    let extent = Extent::new(-100.0, -30.0, 20.0, 45.0);
    let by_id = |mut v: Vec<Feature>| {
        v.sort_by_key(|f| f.id);
        v
    };
    for src in &sources {
        let format = src.info().format;
        assert!(src.visits_chunks() && !src.streams_chunks(), "{format}");
        let whole = src.read(&Request::all()).unwrap();
        assert!(whole.complete && whole.features.len() == 600, "{format}");
        for budget in [256, 4096, 1 << 20] {
            let (rows, summary, calls) = visit_all(src.as_ref(), None, budget);
            assert_eq!(rows, whole.features, "{format} at {budget} bytes");
            assert!(summary.complete && summary.features.is_empty(), "{format}");
            assert_eq!((summary.matched, summary.skipped), (600, whole.skipped), "{format}");
            assert!(budget > 4096 || calls > 1, "{format} at {budget} bytes came in one chunk");
        }
        // Each row alone when none fits the budget.
        assert_eq!(visit_all(src.as_ref(), None, 1).0, whole.features, "{format}");
        // An extent: the features a complete read of it gives (the R-tree's order may differ).
        let part = src.read(&Request { extent: Some(extent), limit: 10_000 }).unwrap();
        assert!(part.complete && part.features.len() == in_box(&extent), "{format}");
        let (rows, summary, _) = visit_all(src.as_ref(), Some(extent), 1024);
        assert!(summary.complete && summary.features.is_empty() && summary.matched == rows.len() as u64, "{format}");
        if src.info().indexed {
            assert_eq!(by_id(rows), by_id(part.features), "{format}");
        } else {
            assert_eq!(rows, part.features, "{format}");
        }
        assert!(visit_all(src.as_ref(), Some(Extent::new(1.0, 1.0, 2.0, 2.0)), 1024).0.is_empty(), "{format}");
        // A visitor's error stops the traversal; a zero budget is refused before any row.
        let mut calls = 0;
        let stopped = src.visit_chunks(None, 256, &mut |_| {
            calls += 1;
            Err(DataError::Format("cancelled".into()))
        });
        assert!(stopped.is_err() && calls == 1, "{format}");
        assert!(src.visit_chunks(None, 0, &mut |_| panic!("a zero budget must not emit")).is_err(), "{format}");
        // The map's reads of the layer go on while a traversal waits on its visitor.
        src.visit_chunks(None, 256, &mut |_| {
            assert_eq!(src.read(&Request { extent: None, limit: 1 }).unwrap().features.len(), 1, "{format}");
            Ok(())
        })
        .unwrap();
    }
    // A row group longer than a read takes from one: a traversal refuses it rather than leave
    // its last rows out.
    let capped = parquet::ParquetSource::open_path(&pq_plain, "plain").unwrap().with_group_rows(40);
    let error = capped.visit_chunks(None, 4096, &mut |_| Ok(())).unwrap_err().to_string();
    assert!(error.contains("row group 0 has 50 rows, more than the 40"), "{error}");
    // A capped row group outside the extent is passed over, not refused.
    let capped = parquet::ParquetSource::open_path(&pq, "grid").unwrap().with_group_rows(40);
    assert!(visit_all(&capped, Some(Extent::new(178.0, 0.0, 179.0, 1.0)), 4096).0.is_empty());
    // A GeoPackage put in place of the one added is refused, not read as the layer.
    #[cfg(unix)]
    {
        let src = open_path(&gpkg, None).unwrap();
        let other = d.join("other.gpkg");
        write_gpkg(&other, false);
        std::fs::rename(&other, &gpkg).unwrap();
        let error = src.visit_chunks(None, 4096, &mut |_| Ok(())).unwrap_err().to_string();
        assert!(error.contains("replaced"), "{error}");
        assert_eq!(src.read(&Request::all()).unwrap().features.len(), 600);
        // A file removed is missing, not replaced.
        std::fs::remove_file(&gpkg).unwrap();
        let error = src.visit_chunks(None, 4096, &mut |_| Ok(())).unwrap_err();
        assert!(matches!(&error, crate::DataError::Io(e) if e.kind() == std::io::ErrorKind::NotFound), "{error}");
    }
    // An unreadable geometry is skipped and counted, and a row without one left out, as a read does.
    drop(sources);
    let c = rusqlite::Connection::open(&gpkg_scan).unwrap();
    c.execute_batch("INSERT INTO \"grid \"\"q\"\"\" (geom, name) VALUES (X'4750000300000000DEAD', 'bad'), (NULL, 'none');").unwrap();
    drop(c);
    let src = open_path(&gpkg_scan, None).unwrap();
    let whole = src.read(&Request::all()).unwrap();
    assert_eq!((whole.features.len(), whole.skipped), (600, 1));
    let (rows, summary, _) = visit_all(src.as_ref(), None, 4096);
    assert_eq!((rows, summary.skipped), (whole.features, 1));
    // The browser's reader of the same file skips and leaves out the same rows.
    let mem = gpkg_mem::GpkgMemSource::open_shared(Shared(Arc::new(std::fs::read(&gpkg_scan).unwrap())), None).unwrap();
    let whole = mem.read(&Request::all()).unwrap();
    assert_eq!((whole.features.len(), whole.skipped), (600, 1));
    let (rows, summary, _) = visit_all(&mem, None, 4096);
    assert_eq!((rows, summary.skipped), (whole.features, 1));
}

/// GeoPackage (desktop and web readers) and GeoParquet tables that declare Z or M for every
/// geometry say they lose them, counting the features; the same tables declaring 2D say nothing.
#[test]
fn declared_z_and_m_are_said_and_counted() {
    let d = tempfile::tempdir().unwrap();
    let says = |info: &SourceInfo| info.zm_dropped && info.notes.contains(&crate::zm_note(Some(600)));
    let quiet = |info: &SourceInfo| !info.zm_dropped && !info.notes.iter().any(|n| n.contains("Z or M"));

    let gpkg = d.path().join("grid.gpkg");
    write_gpkg(&gpkg, false);
    assert!(quiet(open_path(&gpkg, None).unwrap().info()));
    let mem = |path: &Path| gpkg_mem::GpkgMemSource::open_shared(Shared(Arc::new(std::fs::read(path).unwrap())), None).unwrap();
    assert!(quiet(mem(&gpkg).info()));
    rusqlite::Connection::open(&gpkg).unwrap().execute("UPDATE gpkg_geometry_columns SET z = 1", []).unwrap();
    let info = open_path(&gpkg, None).unwrap().info().clone();
    assert!(says(&info), "{:?}", info.notes);
    // The web reader doesn't scan for the count, and says so without one.
    let info = mem(&gpkg).info().clone();
    assert!(info.zm_dropped && info.notes.contains(&crate::zm_note(None)), "{:?}", info.notes);

    let parquet = d.path().join("grid.parquet");
    write_parquet_typed(&parquet, false, r#"["Point"]"#);
    assert!(quiet(open_path(&parquet, None).unwrap().info()));
    write_parquet_typed(&parquet, false, r#"["Point Z"]"#);
    let info = open_path(&parquet, None).unwrap().info().clone();
    assert!(says(&info), "{:?}", info.notes);
    // Some types with Z, some without: said, without a count.
    write_parquet_typed(&parquet, false, r#"["Point", "Point Z"]"#);
    let info = open_path(&parquet, None).unwrap().info().clone();
    assert!(info.zm_dropped && info.notes.contains(&crate::zm_note(None)), "{:?}", info.notes);
}

/// A GeoParquet whose columns RustGIS has no field type for (a decimal too wide for a 64-bit
/// integer, binary, a list, a struct, a leaf beside the bbox covering's box) are said and left
/// out, and whose dates and times, read as whole numbers, are said with their unit; the box's
/// zmin and zmax aren't, and a file of ordinary columns says nothing.
#[test]
fn geoparquet_columns_not_kept_or_read_as_numbers_are_said() {
    use arrow_array::builder::{Int32Builder, ListBuilder};
    use arrow_array::{
        BinaryArray, Date32Array, Decimal128Array, Float64Array, RecordBatch, StringArray, StructArray, TimestampMicrosecondArray,
        TimestampMillisecondArray,
    };
    use arrow_schema::{DataType, Field as AField, Fields, Schema, TimeUnit};
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("columns.parquet");
    let wkb = wkb::encode(&Geometry::Point(Coord::new(1.0, 2.0)));
    let mut tags = ListBuilder::new(Int32Builder::new());
    tags.values().append_value(7);
    tags.append(true);
    let tags = tags.finish();
    let inner: Fields = vec![Arc::new(AField::new("a", DataType::Int64, true))].into_iter().collect();
    let extra = StructArray::new(inner.clone(), vec![Arc::new(arrow_array::Int64Array::from(vec![1]))], None);
    // The covering's box, with z bounds (part of the box) and a label (not).
    let mut box_fields: Vec<Arc<AField>> =
        ["xmin", "ymin", "xmax", "ymax", "zmin", "zmax"].map(|n| Arc::new(AField::new(n, DataType::Float64, false))).into();
    box_fields.push(Arc::new(AField::new("label", DataType::Utf8, true)));
    let mut box_cols: Vec<arrow_array::ArrayRef> =
        [1.0, 2.0, 1.0, 2.0, 0.0, 0.0].map(|v| Arc::new(Float64Array::from(vec![v])) as _).into();
    box_cols.push(Arc::new(StringArray::from(vec!["a"])));
    let box_fields: Fields = box_fields.into_iter().collect();
    let bbox = StructArray::new(box_fields.clone(), box_cols, None);
    let schema = Arc::new(Schema::new(vec![
        AField::new("name", DataType::Utf8, true),
        AField::new("price", DataType::Decimal128(30, 2), true),
        AField::new("photo", DataType::Binary, true),
        AField::new("tags", arrow_array::Array::data_type(&tags).clone(), true),
        AField::new("extra", DataType::Struct(inner), true),
        AField::new("seen", DataType::Timestamp(TimeUnit::Millisecond, Some("UTC".into())), true),
        AField::new("logged", DataType::Timestamp(TimeUnit::Microsecond, None), true),
        AField::new("day", DataType::Date32, true),
        AField::new("geometry", DataType::Binary, false),
        AField::new("geometry_bbox", DataType::Struct(box_fields), false),
    ]));
    let cols: Vec<arrow_array::ArrayRef> = vec![
        Arc::new(StringArray::from(vec!["a"])),
        Arc::new(Decimal128Array::from(vec![12345]).with_precision_and_scale(30, 2).unwrap()),
        Arc::new(BinaryArray::from(vec![&b"\x89PNG"[..]])),
        Arc::new(tags),
        Arc::new(extra),
        Arc::new(TimestampMillisecondArray::from(vec![1_790_856_000_000]).with_timezone("UTC")),
        Arc::new(TimestampMicrosecondArray::from(vec![1_790_856_000_000_000])),
        Arc::new(Date32Array::from(vec![20_727])),
        Arc::new(BinaryArray::from(vec![wkb.as_slice()])),
        Arc::new(bbox),
    ];
    let geo = r#"{"version":"1.1.0","primary_column":"geometry","columns":{"geometry":{"encoding":"WKB","geometry_types":["Point"],
        "covering":{"bbox":{"xmin":["geometry_bbox","xmin"],"ymin":["geometry_bbox","ymin"],"xmax":["geometry_bbox","xmax"],"ymax":["geometry_bbox","ymax"]}}}}}"#;
    let props = ::parquet::file::properties::WriterProperties::builder()
        .set_key_value_metadata(Some(vec![::parquet::file::metadata::KeyValue::new("geo".to_string(), geo.to_string())]))
        .build();
    let mut w = ::parquet::arrow::ArrowWriter::try_new(std::fs::File::create(&path).unwrap(), schema.clone(), Some(props)).unwrap();
    w.write(&RecordBatch::try_new(schema, cols).unwrap()).unwrap();
    w.close().unwrap();

    let left_out = "5 columns of types RustGIS has no field for were left out: \
                    price (decimal), photo (binary), tags (list), extra (struct), geometry_bbox (struct)";
    let as_numbers = "3 columns of dates or times were read as whole numbers: seen (milliseconds since 1970-01-01), \
                      logged (microseconds since 1970-01-01), day (days since 1970-01-01)";
    // Read in place: the source's notes, which adding the layer gives as notices.
    let src = open_path(&path, None).unwrap();
    let info = src.info();
    assert_eq!(info.notes, vec![left_out.to_string(), as_numbers.to_string()]);
    let names: Vec<&str> = info.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["name", "seen", "logged", "day"]);
    // Read whole: the same, in the read's report, and the values as said.
    let (ds, report) = materialize(src.as_ref()).unwrap();
    assert_eq!(report.messages, vec![format!("columns: {left_out}"), format!("columns: {as_numbers}")]);
    assert_eq!(
        ds.features[0].attrs,
        vec![Value::Text("a".into()), Value::Int(1_790_856_000_000), Value::Int(1_790_856_000_000_000), Value::Int(20_727)]
    );

    // Ordinary columns, with a bbox covering: nothing to say.
    let clean = d.path().join("clean.parquet");
    write_parquet(&clean, true);
    let src = open_path(&clean, None).unwrap();
    assert!(src.info().notes.is_empty(), "{:?}", src.info().notes);
    assert!(materialize(src.as_ref()).unwrap().1.messages.is_empty());
}

/// A FlatGeobuf's binary column, which RustGIS has no field type for, is said and left out, the
/// columns after it keeping their values; a file without one says nothing.
#[test]
fn flatgeobuf_binary_columns_are_said_and_left_out() {
    use flatgeobuf::geozero::{ColumnValue, PropertyProcessor};
    use flatgeobuf::{ColumnType, FgbWriter, GeometryType as G};
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("blobs.fgb");
    let mut w = FgbWriter::create("blobs", G::Point).unwrap();
    w.add_column("name", ColumnType::String, |_, _| {});
    w.add_column("blob", ColumnType::Binary, |_, _| {});
    w.add_column("value", ColumnType::Long, |_, _| {});
    let g = flatgeobuf::geozero::wkb::Wkb(wkb::encode(&Geometry::Point(Coord::new(1.0, 2.0))));
    w.add_feature_geom(g, |f| {
        f.property(0, "name", &ColumnValue::String("a")).unwrap();
        f.property(1, "blob", &ColumnValue::Binary(b"\x89PNG")).unwrap();
        f.property(2, "value", &ColumnValue::Long(42)).unwrap();
    })
    .unwrap();
    w.write(std::fs::File::create(&path).unwrap()).unwrap();

    let note = "1 column of a type RustGIS has no field for was left out: blob (binary)";
    let src = open_path(&path, None).unwrap();
    assert_eq!(src.info().notes, vec![note.to_string()]);
    let names: Vec<&str> = src.info().fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["name", "value"]);
    let (ds, report) = materialize(src.as_ref()).unwrap();
    assert_eq!(report.messages, vec![format!("blobs: {note}")]);
    assert_eq!(ds.features[0].attrs, vec![Value::Text("a".into()), Value::Int(42)]);

    let clean = d.path().join("clean.fgb");
    write_fgb(&clean);
    let src = open_path(&clean, None).unwrap();
    assert!(src.info().notes.is_empty(), "{:?}", src.info().notes);
}

/// The tables of a GeoPackage, by name.
fn gpkg_tables(path: &Path) -> Vec<String> {
    let c = rusqlite::Connection::open(path).unwrap();
    let mut st = c.prepare("SELECT table_name FROM gpkg_contents ORDER BY table_name").unwrap();
    st.query_map([], |r| r.get(0)).unwrap().collect::<std::result::Result<_, _>>().unwrap()
}

/// An export into an existing GeoPackage adds a table beside the others, refuses one whose name
/// is taken, and replaces a feature or attribute table (its spatial index too) when asked; the
/// other tables, an attribute table among them, are as they were.
#[test]
fn geopackage_export_adds_and_replaces_tables_and_keeps_the_others() {
    use crate::io::{GpkgMode, GpkgTable, GpkgWritten, write_path_to};
    let workspace = tempfile::tempdir().unwrap();
    let path = workspace.path().join("county.gpkg");
    let mut parcels = Dataset::new("parcels", GeometryType::Point, vec![Field::new("apn", FieldType::Text)]);
    parcels.push(Some(Geometry::Point(Coord::new(1.0, 2.0))), vec![Value::Text("001".into())]);
    crate::io::write_path(&parcels, &path).unwrap();
    // An attribute table, and the feature counts GDAL keeps.
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch(
            "CREATE TABLE owners (name TEXT); INSERT INTO owners VALUES ('Shelby');
             INSERT INTO gpkg_contents (table_name, data_type, identifier) VALUES ('owners', 'attributes', 'owners');
             CREATE TABLE gpkg_ogr_contents (table_name TEXT NOT NULL PRIMARY KEY, feature_count INTEGER DEFAULT NULL);
             INSERT INTO gpkg_ogr_contents VALUES ('parcels', 1);
             CREATE TRIGGER wipe AFTER INSERT ON gpkg_contents BEGIN DELETE FROM owners; END;",
        )
        .unwrap();
    let mut permits = Dataset::new("permits", GeometryType::Point, vec![Field::new("n", FieldType::Integer)]);
    for i in 0..3 {
        permits.push(Some(Geometry::Point(Coord::new(10.0 + i as f64, 20.0))), vec![Value::Int(i)]);
    }
    let to = |mode, name: Option<&str>| GpkgTable { mode, name: name.map(String::from) };
    let written = write_path_to(&permits, &path, Crs::LonLat, &to(GpkgMode::AddTable, None)).unwrap();
    assert_eq!(written, (0, Some(GpkgWritten { table: "permits".into(), replaced: false, dependents: vec![] })));
    rusqlite::Connection::open(&path).unwrap().execute_batch("INSERT INTO gpkg_ogr_contents VALUES ('permits', 3)").unwrap();
    assert_eq!(gpkg_tables(&path), ["owners", "parcels", "permits"]);
    // The file's own trigger on its contents didn't run.
    let owners =
        |p: &Path| rusqlite::Connection::open(p).unwrap().query_row("SELECT count(*) FROM owners", [], |r| r.get::<_, i64>(0)).unwrap();
    assert_eq!(owners(&path), 1);
    let read = |t: &str, extent| open_path(&path, Some(t)).unwrap().read(&Request { extent, limit: 100 }).unwrap().features;
    assert_eq!(read("permits", None).len(), 3);
    // Its spatial index answers an extent query.
    assert_eq!(read("permits", Some(Extent::new(10.5, 19.0, 11.5, 21.0))).len(), 1);
    assert_eq!(read("parcels", None)[0].attrs, [Value::Text("001".into())]);

    // A name taken (whatever its case) is refused unless replacing, and the file is unchanged.
    let before = std::fs::read(&path).unwrap();
    let mut before_index = before.clone();
    for name in [None, Some("PERMITS"), Some("owners_idx")] {
        if name == Some("owners_idx") {
            assert_eq!(std::fs::read(&path).unwrap(), before, "the refusals changed nothing");
            rusqlite::Connection::open(&path).unwrap().execute_batch("CREATE INDEX owners_idx ON owners (name)").unwrap();
            before_index = std::fs::read(&path).unwrap();
        }
        let e = write_path_to(&permits, &path, Crs::LonLat, &to(GpkgMode::AddTable, name)).unwrap_err().to_string();
        assert!(e.contains("already has"), "{name:?}: {e}");
    }
    assert_eq!(std::fs::read(&path).unwrap(), before_index, "the index refusal changed nothing");
    assert_ne!(before, before_index, "the index was added");

    // Replacing: fewer features, in another system, and its old spatial index gone. A view on the
    // table is named, as it reads the new one.
    rusqlite::Connection::open(&path).unwrap().execute_batch("CREATE VIEW recent_permits AS SELECT * FROM permits").unwrap();
    let mut fewer = permits.clone();
    fewer.features.truncate(1);
    let replaced = write_path_to(&fewer, &path, Crs::WebMercator, &to(GpkgMode::ReplaceTable, None)).unwrap();
    assert_eq!(replaced.1, Some(GpkgWritten { table: "permits".into(), replaced: true, dependents: vec!["recent_permits".into()] }));
    assert_eq!(gpkg_tables(&path), ["owners", "parcels", "permits"]);
    let s = open_path(&path, Some("permits")).unwrap();
    assert_eq!(s.info().count, Some(1));
    assert_eq!(s.info().coordinate_system(), Some(Crs::WebMercator));
    let c = rusqlite::Connection::open(&path).unwrap();
    let count = |sql: &str| c.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
    assert_eq!(count("SELECT count(*) FROM rtree_permits_geom"), 1);
    assert_eq!(count("SELECT count(*) FROM gpkg_geometry_columns WHERE table_name = 'permits'"), 1);
    assert_eq!(count("SELECT count(*) FROM gpkg_extensions WHERE table_name = 'permits'"), 1);
    assert_eq!(count("SELECT count(*) FROM gpkg_spatial_ref_sys WHERE srs_id = 3857"), 1);
    // The old table's cached count is gone with it; the others' are kept.
    assert_eq!(count("SELECT count(*) FROM gpkg_ogr_contents WHERE table_name = 'permits'"), 0);
    assert_eq!(count("SELECT count(*) FROM gpkg_ogr_contents WHERE table_name = 'parcels'"), 1);
    assert_eq!(c.query_row("SELECT name FROM owners", [], |r| r.get::<_, String>(0)).unwrap(), "Shelby");
    drop(c);
    // A second table in that system reuses its entry.
    let more = write_path_to(&fewer, &path, Crs::WebMercator, &to(GpkgMode::AddTable, Some("permits_2027"))).unwrap();
    assert_eq!(more.1, Some(GpkgWritten { table: "permits_2027".into(), replaced: false, dependents: vec![] }));
    // Replacing a table not there (a misspelt name) is refused rather than adding one.
    let e = write_path_to(&fewer, &path, Crs::LonLat, &to(GpkgMode::ReplaceTable, Some("permit"))).unwrap_err().to_string();
    assert!(e.contains("no table permit to replace"), "{e}");
    let c = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(c.query_row("SELECT count(*) FROM gpkg_spatial_ref_sys WHERE srs_id = 3857", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    // A table without geometry replaces an attribute table, adding no spatial reference system.
    let systems = || {
        let c = rusqlite::Connection::open(&path).unwrap();
        c.query_row("SELECT count(*) FROM gpkg_spatial_ref_sys", [], |r| r.get::<_, i64>(0)).unwrap()
    };
    let before_systems = systems();
    let mut owners = Dataset::new("owners", GeometryType::None, vec![Field::new("name", FieldType::Text)]);
    owners.push(None, vec![Value::Text("Memphis".into())]);
    owners.push(None, vec![Value::Text("Germantown".into())]);
    drop(c);
    write_path_to(&owners, &path, Crs::from_epsg(32616).unwrap(), &to(GpkgMode::ReplaceTable, None)).unwrap();
    assert_eq!(systems(), before_systems);
    let c = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(c.query_row("SELECT count(*) FROM owners", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
    drop(c);
    assert_eq!(read("parcels", None).len(), 1, "untouched throughout");
}

/// An export into an existing GeoPackage never replaces what isn't a feature or attribute table
/// (a tile table, a view, an unregistered table) or a file that isn't there, and refuses a file
/// that isn't a GeoPackage; adding a table where there is no file makes a new one.
#[test]
fn geopackage_export_into_a_file_refuses_what_it_cannot_replace() {
    use crate::io::{GpkgMode, GpkgTable, write_path_to};
    let workspace = tempfile::tempdir().unwrap();
    let path = workspace.path().join("county.gpkg");
    let replace = |name: &str| GpkgTable { mode: GpkgMode::ReplaceTable, name: Some(name.into()) };
    let mut ds = Dataset::new("permits", GeometryType::Point, vec![]);
    ds.push(Some(Geometry::Point(Coord::new(1.0, 2.0))), vec![]);
    // No file: nothing to replace in, but a table can be added to a new GeoPackage.
    assert!(write_path_to(&ds, &path, Crs::LonLat, &replace("permits")).unwrap_err().to_string().contains("no GeoPackage there"));
    assert!(!path.exists());
    let add = GpkgTable { mode: GpkgMode::AddTable, name: None };
    assert!(write_path_to(&ds, &path, Crs::LonLat, &add).unwrap().1.is_some_and(|t| !t.replaced));
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch(
            "CREATE TABLE ortho (id INTEGER PRIMARY KEY, tile_data BLOB);
             INSERT INTO gpkg_contents (table_name, data_type, identifier) VALUES ('ortho', 'tiles', 'ortho');
             CREATE VIEW recent AS SELECT * FROM permits; CREATE TABLE scratch (x);",
        )
        .unwrap();
    let before = std::fs::read(&path).unwrap();
    for name in ["ortho", "recent", "scratch"] {
        let e = write_path_to(&ds, &path, Crs::LonLat, &replace(name)).unwrap_err().to_string();
        assert!(e.contains("doesn't replace"), "{name}: {e}");
        assert_eq!(std::fs::read(&path).unwrap(), before, "{name}");
    }
    // A related-tables relation's table isn't replaced; another application's database isn't
    // written to.
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch(
            "CREATE TABLE gpkgext_relations (id INTEGER PRIMARY KEY, base_table_name TEXT, base_primary_column TEXT,
                related_table_name TEXT, related_primary_column TEXT, relation_name TEXT, mapping_table_name TEXT);
             INSERT INTO gpkgext_relations VALUES (1, 'permits', 'fid', 'owners', 'id', 'attributes', 'permits_owners');",
        )
        .unwrap();
    let e = write_path_to(&ds, &path, Crs::LonLat, &replace("permits")).unwrap_err().to_string();
    assert!(e.contains("related-tables relation"), "{e}");
    // Nor is a table others refer to by foreign key: their rows would be deleted with it.
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch(
            "DELETE FROM gpkgext_relations;
             CREATE TABLE inspections (id INTEGER PRIMARY KEY, permit INTEGER REFERENCES permits (fid) ON DELETE CASCADE);
             INSERT INTO inspections VALUES (1, 1);",
        )
        .unwrap();
    let e = write_path_to(&ds, &path, Crs::LonLat, &replace("permits")).unwrap_err().to_string();
    assert!(e.contains("inspections refer to permits"), "{e}");
    let kept: i64 = rusqlite::Connection::open(&path).unwrap().query_row("SELECT count(*) FROM inspections", [], |r| r.get(0)).unwrap();
    assert_eq!(kept, 1);
    // A table referring to the GeoPackage's own records keeps its rows when a replace deletes them.
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch(
            "DROP TABLE inspections;
             CREATE TABLE notes (layer TEXT REFERENCES gpkg_contents (table_name) ON DELETE CASCADE);
             INSERT INTO notes VALUES ('permits');",
        )
        .unwrap();
    write_path_to(&ds, &path, Crs::LonLat, &replace("permits")).unwrap();
    let notes: i64 = rusqlite::Connection::open(&path).unwrap().query_row("SELECT count(*) FROM notes", [], |r| r.get(0)).unwrap();
    assert_eq!(notes, 1);
    // An ordinary table named as the spatial index is kept, and the replace refused.
    let squatted = workspace.path().join("squatted.gpkg");
    write_path_to(&ds, &squatted, Crs::LonLat, &GpkgTable { mode: GpkgMode::AddTable, name: Some("sites".into()) }).unwrap();
    rusqlite::Connection::open(&squatted)
        .unwrap()
        .execute_batch(
            "DROP TABLE rtree_sites_geom; CREATE TABLE rtree_sites_geom (note TEXT); INSERT INTO rtree_sites_geom VALUES ('keep');",
        )
        .unwrap();
    let before = std::fs::read(&squatted).unwrap();
    let e = write_path_to(&ds, &squatted, Crs::LonLat, &replace("sites")).unwrap_err().to_string();
    assert!(e.contains("rtree_sites_geom"), "{e}");
    assert_eq!(std::fs::read(&squatted).unwrap(), before, "the failed replace changed nothing");
    let kept: String =
        rusqlite::Connection::open(&squatted).unwrap().query_row("SELECT note FROM rtree_sites_geom", [], |r| r.get(0)).unwrap();
    assert_eq!(kept, "keep");
    // A spatial index declared over lines, with the module quoted, is still the index; an
    // `rtree_i32` table of that name isn't.
    for (module, replaced) in [("\n  \"rtree\" (", true), ("[rtree](", true), ("rtree_i32(", false)] {
        let file = workspace.path().join(format!("index-{}.gpkg", module.len()));
        write_path_to(&ds, &file, Crs::LonLat, &GpkgTable { mode: GpkgMode::AddTable, name: Some("sites".into()) }).unwrap();
        rusqlite::Connection::open(&file)
            .unwrap()
            .execute_batch(&format!(
                "DROP TABLE rtree_sites_geom; CREATE VIRTUAL TABLE rtree_sites_geom USING {module}id, minx, maxx, miny, maxy);"
            ))
            .unwrap();
        assert_eq!(write_path_to(&ds, &file, Crs::LonLat, &replace("sites")).is_ok(), replaced, "{module:?}");
    }
    let other = workspace.path().join("other.gpkg");
    std::fs::copy(&path, &other).unwrap();
    rusqlite::Connection::open(&other).unwrap().execute_batch("PRAGMA application_id = 1297105496").unwrap();
    let add = GpkgTable { mode: GpkgMode::AddTable, name: Some("more".into()) };
    assert!(write_path_to(&ds, &other, Crs::LonLat, &add).unwrap_err().to_string().contains("application id"));
    let plain = workspace.path().join("plain.gpkg");
    rusqlite::Connection::open(&plain).unwrap().execute_batch("CREATE TABLE t (x)").unwrap();
    let e = write_path_to(&ds, &plain, Crs::LonLat, &replace("permits")).unwrap_err().to_string();
    assert!(e.contains("not a GeoPackage"), "{e}");
    let text = workspace.path().join("text.gpkg");
    std::fs::write(&text, b"not a database").unwrap();
    assert!(write_path_to(&ds, &text, Crs::LonLat, &replace("permits")).is_err());
    assert_eq!(std::fs::read(&text).unwrap(), b"not a database");
}
