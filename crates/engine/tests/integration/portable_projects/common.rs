use rustgis_engine::{Project, Session};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub struct Fixture {
    pub root: PathBuf,
}

impl Fixture {
    pub fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!(
            "rustgis-portable-{label}-{}-{nonce}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        Self { root }
    }

    pub fn file(&self, relative: &str) -> PathBuf {
        let path = native_join(&self.root, relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

pub fn native_join(root: &Path, relative: &str) -> PathBuf {
    let mut path = root.to_path_buf();
    path.extend(relative.split('/'));
    path
}

pub fn session() -> Session {
    Session::new(Project::default())
}

pub fn sample_file(path: &Path, layer: &str) {
    let mut s = session();
    s.run("project.sample", &Value::Null).unwrap();
    s.run("share.exportLayer", &json!({"layer": layer, "path": path})).unwrap();
}

pub fn raster_file(path: &Path, bands: usize) {
    use rustgis_engine::data::raster::{ColorHint, Crs, GeoTransform, Raster, geotiff};
    assert!(matches!(bands, 1 | 3 | 4));
    let transform = GeoTransform { origin_x: 10.0, origin_y: 45.04, pixel_width: 0.01, pixel_height: -0.01 };
    let values = (0..bands).map(|band| (0..16).map(|i| (i + band * 20) as f32).collect()).collect();
    let color = if bands == 1 { ColorHint::Gray } else { ColorHint::Rgb { alpha: bands == 4 } };
    let raster = Raster::new("dem", 4, 4, values, transform, Crs::LonLat, None, color, "Float32").unwrap();
    std::fs::write(path, geotiff::write(&raster, 0).unwrap()).unwrap();
}

pub fn archive_file(path: &Path) {
    use rustgis_engine::render::rustgis_tiles::{mvt, pmtiles};
    let feature = mvt::Feature {
        id: Some(1),
        kind: mvt::GeomKind::Point,
        geometry: vec![vec![[2048.0, 2048.0]]],
        props: vec![("name".into(), mvt::Value::String("Town".into()))],
    };
    let tile = mvt::encode(&mvt::Tile { layers: vec![mvt::Layer { name: "places".into(), extent: 4096, features: vec![feature] }] });
    let archive = pmtiles::NewArchive {
        tile_type: pmtiles::TileType::Mvt,
        gzipped_tiles: false,
        bounds: [-180.0, -85.0, 180.0, 85.0],
        metadata: json!({"vector_layers": [{"id": "places"}]}),
    };
    std::fs::write(path, pmtiles::write(vec![(0, 0, 0, tile)], &archive).unwrap()).unwrap();
}

pub fn gpkg_file(path: &Path, tables: &[&str]) {
    let c = rusqlite::Connection::open(path).unwrap();
    c.execute_batch(
        "CREATE TABLE gpkg_spatial_ref_sys (srs_name TEXT, srs_id INTEGER PRIMARY KEY, organization TEXT, organization_coordsys_id INTEGER, definition TEXT, description TEXT);
         INSERT INTO gpkg_spatial_ref_sys VALUES ('WGS 84', 4326, 'EPSG', 4326, '', '');
         CREATE TABLE gpkg_contents (table_name TEXT PRIMARY KEY, data_type TEXT, identifier TEXT, description TEXT, last_change TEXT, min_x DOUBLE, min_y DOUBLE, max_x DOUBLE, max_y DOUBLE, srs_id INTEGER);
         CREATE TABLE gpkg_geometry_columns (table_name TEXT, column_name TEXT, geometry_type_name TEXT, srs_id INTEGER, z TINYINT, m TINYINT);",
    )
    .unwrap();
    let mut blob = b"GP\x00\x01".to_vec();
    blob.extend_from_slice(&4326i32.to_le_bytes());
    blob.extend(rustgis_engine::data::wkb::encode(&rustgis_engine::data::Geometry::Point(rustgis_engine::data::Coord::new(2.35, 48.85))));
    for table in tables {
        assert!(!table.is_empty() && table.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'));
        c.execute_batch(&format!(
            "INSERT INTO gpkg_contents (table_name, data_type, srs_id) VALUES ('{table}', 'features', 4326);
             INSERT INTO gpkg_geometry_columns VALUES ('{table}', 'geom', 'POINT', 4326, 0, 0);
             CREATE TABLE {table} (fid INTEGER PRIMARY KEY AUTOINCREMENT, geom BLOB);"
        ))
        .unwrap();
        c.execute(&format!("INSERT INTO {table} (geom) VALUES (?1)"), [&blob]).unwrap();
    }
}

pub fn add_stream(s: &mut Session, path: &Path, table: Option<&str>) -> u32 {
    let mut params = json!({"path": path, "stream": true});
    if let Some(table) = table {
        params["layer"] = json!(table);
    }
    s.run("layer.addData", &params).unwrap()["layer"].as_u64().unwrap() as u32
}
