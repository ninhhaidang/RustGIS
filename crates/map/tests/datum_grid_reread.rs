//! A streamed layer's window is read again when a datum grid arrives (#2032 A4): its features are
//! held in longitude/latitude, placed with the transformation of the time they were read, and the
//! next read places them with the grid. The selection is kept, as after a change of projection.
//! Its own test binary: the grid generation is process-wide.

use rustgis_data::raster::crs;
use rustgis_data::{Coord, Dataset, Extent, Field, FieldType, Geometry, GeometryType, Value};
use rustgis_map::Map;
use rustgis_map::source::SourceLayer;

fn source() -> Box<dyn rustgis_data::source::FeatureSource> {
    let mut ds = Dataset::new("points", GeometryType::Point, vec![Field::new("n", FieldType::Integer)]);
    for i in 0..20 {
        ds.push(Some(Geometry::Point(Coord::new(i as f64 * 0.1, 1.0))), vec![Value::Int(i)]);
    }
    let (bytes, _) = rustgis_data::io::write_bytes(&ds, "fgb").unwrap();
    rustgis_data::source::open_bytes("points.fgb", bytes, None).unwrap()
}

fn settle(map: &mut Map, extent: &Extent) {
    map.sync_sources(extent, 1000.0, false);
    for layer in map.layers.iter_mut().filter(|l| l.visible) {
        assert!(!layer.sync_source(extent, true));
    }
}

#[test]
fn a_streamed_window_is_read_again_when_a_grid_arrives_and_keeps_its_selection() {
    let mut map = Map::new("grids");
    let id = map.add_streamed("points", SourceLayer::from_path("points.fgb", None, source()));
    let extent = Extent::new(-10.0, -10.0, 10.0, 10.0);
    settle(&mut map, &extent);
    let layer = map.layer_mut(id).unwrap();
    let selected = layer.dataset.features[3].id;
    layer.selection.push(3);
    let reads = layer.source.as_ref().unwrap().reads;

    // Nothing new: not read again.
    settle(&mut map, &extent);
    assert_eq!(map.layer(id).unwrap().source.as_ref().unwrap().reads, reads);

    crs::grids_changed();
    settle(&mut map, &extent);
    let layer = map.layer(id).unwrap();
    assert!(layer.source.as_ref().unwrap().reads > reads, "read again with the grid");
    assert_eq!(layer.selection.len(), 1);
    assert_eq!(layer.dataset.features[layer.selection[0]].id, selected);
}
