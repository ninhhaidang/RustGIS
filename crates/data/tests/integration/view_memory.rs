//! The default provider adapter bounds retained views without changing full-source reads.

use rustgis_data::{
    Coord, Dataset, Field, FieldType, Geometry, GeometryType, Value,
    source::{Request, memory},
};

#[test]
fn budgeted_views_keep_full_reads_for_geopackage_geoparquet_and_geojson() {
    let mut data = Dataset::new("budget", GeometryType::Polyline, vec![Field::new("text", FieldType::Text)]);
    for i in 0..150 {
        data.push(
            Some(Geometry::Line(vec![(0..80).map(|n| Coord::new(i as f64 * 0.001, n as f64 * 0.001)).collect()])),
            vec![Value::Text("x".repeat(300))],
        );
    }
    let folder = tempfile::tempdir().unwrap();
    for extension in ["gpkg", "parquet", "geojson"] {
        let path = folder.path().join(format!("budget.{extension}"));
        rustgis_data::io::write_path(&data, &path).unwrap();
        let source = rustgis_data::source::open_path(&path, None).unwrap();
        let request = Request { extent: Some(rustgis_geom::Extent::new(-1.0, -1.0, 1.0, 1.0)), ..Request::all() };
        let full = source.read(&request).unwrap();
        let sample = source.read_budgeted(&request, 32 * 1024).unwrap();
        assert!(memory::features_bytes(&sample.features) <= 32 * 1024, "{extension}");
        assert!(!sample.complete, "{extension}");
        assert!(!sample.features.is_empty(), "{extension}");
        assert_eq!(sample.matched, full.matched, "{extension}");
        for feature in &sample.features {
            assert!(full.features.contains(feature), "{extension}: sampled feature was changed");
        }
        assert_eq!(source.read(&request).unwrap().features, full.features, "{extension}");
        assert_eq!(rustgis_data::source::materialize(source.as_ref()).unwrap().0.features.len(), 150);
    }
}
