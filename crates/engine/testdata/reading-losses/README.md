# Paired reader-loss fixtures

The `xy.*` and `xyz.*` pairs hold one point and no user attributes: `xy` has `(1, 2)` and `xyz` has
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

The independently encoded [Shapefile pairs](SHAPEFILE.md) cover point and line Z/M loss.

## XLSX

Four independently encoded Office Open XML workbooks, written offline with Python 3.13's
standard-library `zipfile` (Deflate) and literal XML. No spreadsheet application, RustGIS
exporter, third-party code or data was used. Each ZIP holds only `[Content_Types].xml`,
`_rels/.rels`, `xl/workbook.xml`, `xl/_rels/workbook.xml.rels` and
`xl/worksheets/sheet1.xml`. ZIP timestamps are fixed at 1980-01-01.

Each workbook has one sheet, `Places`, with headers `wkt`, `id`, `note` in A1:C1 and one
row. B2 is the number 7. A2 is an inline string; C2 is absent except in the error case.
WKT uses the reader's existing default longitude/latitude convention; these cases do not
exercise coordinate-system assumptions.

| File | A2 (WKT) | C2 | Current loss classification |
| --- | --- | --- | --- |
| `book-clean.xlsx` | `POINT(1 2)` | absent | None; no command notice |
| `book-z.xlsx` | `POINT Z(1 2 3)` | absent | Imports with a Z/M-loss notice |
| `book-collection.xlsx` | `GEOMETRYCOLLECTION(POINT(1 2),POINT(3 4))` | absent | Silent: only the first member remains |
| `book-error.xlsx` | `POINT(1 2)` | error cell (`t="e"`, value `#DIV/0!`) | Silent: the error becomes Null |

The clean workbook is shared by all three pairs. All four must retain exactly one point
at (1, 2), integer `id=7` and text `note=Null`, through both `sheets::read` and
`layer.addData`. Thus refusal, losing the whole geometry, retaining the second member or
dropping an attribute cannot pass. The error cell is valid spreadsheet content, not a
malformed archive; formulas, date conversion and ODS nesting are outside these fixtures.
