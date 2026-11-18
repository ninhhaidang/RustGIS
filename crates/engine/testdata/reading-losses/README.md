# Paired reader-loss fixtures

Each format holds one point and no user attributes: `xy` has `(1, 2)` and `xyz` has
`(1, 2, 3)`, in WGS 84 longitude/latitude. The reading corpus opens each pair from the
same path and checks that only the XYZ file reports dropping Z or M. These test the
shared engine command, not just a reader's internal report.

The data is made up for RustGIS, licensed Apache-2.0. No third-party data or code is
included. The writers below were used offline; tests need neither writer installed.

## FlatGeobuf and FileGDB

Written with GDAL 3.13.3, from a GeoJSON FeatureCollection holding the one point with
an empty `properties` object. For each input `xy.geojson` or `xyz.geojson`:

```sh
ogr2ogr -f FlatGeobuf xy.fgb xy.geojson -nln points -lco SPATIAL_INDEX=NO
ogr2ogr -f OpenFileGDB xy.gdb xy.geojson -nln points
```

Use `xyz` in place of `xy` for the 3D input. FlatGeobuf has no spatial index, since
it would add nothing to a one-feature fixture.

Each FileGDB fixture retains only the system catalog (`a00000001`), item definitions
(`a00000004`) and the point feature class (`a00000009`), with each table's `.gdbtable`
and `.gdbtablx`. Other system tables and indexes are not consumed by this reader and
were removed before compression. The six files are stored at the root of a ZIP to
avoid committing mostly empty row-index blocks. The harness extracts them to a
`.gdb` folder; importing a ZIP containing a FileGDB is not supported and is not what
this test exercises.

## GeoParquet

Written with PyArrow 26.0.1, with one binary `geometry` column, no compression and no
statistics. The value is little-endian ISO WKB: byte order `1`, type `1` (Point) or
`1001` (PointZ), followed by the coordinates as float64 values. The `geo` schema
metadata declares version `1.0.0`, primary column `geometry`, encoding `WKB` and
geometry type `Point` or `Point Z`. CRS is omitted, meaning the format's default
OGC:CRS84. This is not a fixture for an unknown CRS or for CRS fallback behavior.
