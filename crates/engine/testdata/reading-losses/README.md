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
| `book-error.xlsx` | `POINT(1 2)` | error cell (`t="e"`, value `#DIV/0!`) | Imports with a counted notice naming `note`; the error becomes Null |

The clean workbook is shared by all three pairs. All four must retain exactly one point
at (1, 2), integer `id=7` and text `note=Null`, through both `sheets::read` and
`layer.addData`. Thus refusal, losing the whole geometry, retaining the second member or
dropping an attribute cannot pass. The error cell is valid spreadsheet content, not a
malformed archive; formulas, date conversion and ODS nesting are outside these fixtures.

## ODS

Three independently encoded OpenDocument 1.2 spreadsheets, written offline for RustGIS with
Python 3.13's standard-library `zipfile` and literal XML. No spreadsheet application,
RustGIS exporter, third-party code or data was used. Each ZIP has an uncompressed first
`mimetype` entry, `META-INF/manifest.xml` declaring the document and `content.xml`, and
Deflate-compressed `content.xml`. Timestamps are fixed at 1980-01-01; there are no
preambles, offset adjustments, nested cells or annotations.

All have one `Places` sheet with string headers `wkt`, `id`, `note`, one data row,
numeric `id=7` and an empty note cell. The WKT is ordinary string content:

| File | WKT | Current loss classification |
| --- | --- | --- |
| `book-clean.ods` | `POINT(1 2)` | None; no native or command notice |
| `book-z.ods` | `POINT Z(1 2 3)` | Imports with a Z/M-loss notice |
| `book-collection.ods` | `GEOMETRYCOLLECTION(POINT(1 2),POINT(3 4))` | Silent: only the first member remains |

The clean workbook serves both pairs. The same retained-data assertions as XLSX require
exactly one point at (1, 2), integer `id=7` and text `note=Null` through the native
reader and `layer.addData`; native reports must also have zero skipped rows and the
expected height-loss flag and notice. Valid lossy files must import. WKT uses the
existing reader convention without testing or changing CRS assumptions. This slice does
not certify ODS completeness or cover malformed archives, formulas, dates or error cells.

## XLS and XLSB

Seven made-up workbooks written offline with SheetJS Community Edition `xlsx` 0.18.5:
`utils.aoa_to_sheet([['wkt','id','note'],[wkt,7]])`, appended as `Places`, then
`writeFile` with `bookType: 'biff8'` (XLS) or `'xlsb'` and compression enabled.
The writer is independent of RustGIS and is not needed by tests. No writer code or
third-party data is included. XLS exercises the compound-file/BIFF reader; XLSB
exercises the binary workbook/sheet parser inside ZIP, rather than XLSX's XML parser.

XLS files retain the writer's small compound container. XLSB archives were minimized
offline with Python's standard-library `zipfile`: retain workbook, worksheet and style
binary parts, replace package relationships/content types with only those parts, and
remove the unused theme, document properties and sheet metadata. ZIP timestamps are
1980-01-01. Except in `book-short.xlsb`, the generated XLSB short string/RK cell
records were re-encoded as
full `BrtCellSt`/`BrtCellRk` records with explicit column indexes, following the
published [MS-XLSB Cell layout](https://learn.microsoft.com/en-us/openspecs/office_file_formats/ms-xlsb/1e3e0c46-6175-4123-b02b-903643a5cc07).
`book-short.xlsb` preserves the original worksheet records and tests the short-cell loss.
As a check independent of the handwritten full-cell rewrite, SheetJS CE 0.18.5's `readFile`
and `sheet_to_json` read all seven committed XLS/XLSB workbooks back to the documented
headers, WKT and numeric ID. This check uses a parser independent of RustGIS.
No macros, formulas or external links are included.

The original SheetJS 0.18.5 short-cell worksheet, retained in `book-short.xlsb`,
pins a known reader gap: native
`sheets::read` retains `POINT(1 2)` but returns no attribute fields or values,
including the source `id=7`, with zero skipped rows and no messages. Its short
cell records are paired with the full-record `book-clean.xlsb`; both native and
command imports must retain exactly the point with no attribute fields/values or notices.

| Files (`.xls` and `.xlsb` unless stated) | WKT | Current loss classification |
| --- | --- | --- |
| `book-clean` | `POINT(1 2)` | None; no native or command notice |
| `book-z` | `POINT Z(1 2 3)` | Imports with a Z/M-loss notice |
| `book-collection` | `GEOMETRYCOLLECTION(POINT(1 2),POINT(3 4))` | Silent: only the first member remains |
| `book-short.xlsb` (paired with `book-clean.xlsb`) | `POINT(1 2)` | Silent: all attribute fields and values are lost |

The height/member pairs must retain exactly one point at (1, 2), integer `id=7`,
text `note=Null` and both field names/types, through `sheets::read` and `layer.addData`.
Native checks require zero skipped rows, the expected height flag and exactly one
Z/M message for the height variant (none otherwise). Valid lossy fixtures must import.
Dates, error cells, other sheets and broader type/encoding fidelity are not certified.

## Feature-service PBF

Three tiny, independently encoded feature-service query answers, written offline with
Python's standard-library `struct` and Protocol Buffers wire encoding, following
[FeatureCollection.proto](https://github.com/Esri/arcgis-pbf/blob/master/proto/FeatureCollection/FeatureCollection.proto).
No service data, exporter or third-party code is included. These exercise the sniffed
feature-service reader, distinct from the OSM PBF reader despite the shared extension.

All declare WKID 4326, point geometry, lower-left origin, XY scale 0.5 and translation
(10, 20). One point's quantized XY (3, -5) becomes (11.5, 17.5); its attributes are
OID `id=7` and string `name=Place`. `feature-xy.pbf` (100 bytes) is clean.
`feature-xyz.pbf` and `feature-xym.pbf` (121 bytes each) add respectively `hasZ` or
`hasM`, a third coordinate 37, and that component's unit scale/zero translation.
The clean file serves both pairs. This tests separate dimension flags, not renamed files.

The corpus checks exact geometry, feature ID, field names/types and attributes through
`esripbf::read` and `layer.addData`, with no skipped features. The native and command
messages must be exactly the one-feature Z/M drop message for XYZ/XYM and absent for XY.
Other geometry and attribute losses and network paging are outside these fixtures.

## Coverage boundaries

The corpus is a list of specific losses, not a format-completeness certificate.
Dispatch is defined by `data::io::{read_layers,read_bytes,read_path}`, `source::EXTENSIONS`
and `sheets::open_checked`. Aliases and sniffed formats share rows below; the table does
not claim that every alias or container has its own paired test. “Gap” means applicable
but not covered here, not necessarily a known bug. CRS assumptions remain separate work
(#2032). The exact tested loss and `Silent`/reported status live in the test case table.

| Reader dispatch | Paired corpus coverage | Remaining applicable families |
| --- | --- | --- |
| GeoJSON | Z, unreadable geometry | M, members/holes, attributes |
| TopoJSON | Z, missing arcs, bad quantized positions, nested members | M, other parts, attributes |
| Delimited text (CSV/TSV/TXT/DAT, gzip wrapper) | CSV WKT Z, bad WKT/XY, later WKT members, extra cells | M, holes/curves, types/names/encoding, wrapper parity |
| Shapefile | Point Z/M, line Z/ZM | Other shapes/parts, attributes; DBF encoding tracked in #2325 |
| Standalone DBF | None | Names/types/encoding; geometry and Z/M inapplicable |
| KML/KMZ | Z, missing geometry, bad coordinate/hole/part, excess point coordinates | Other parts, attributes |
| GPX | Bad waypoint, short track segment | Elevation attributes, other parts, attributes |
| GeoRSS (RSS/Atom/GeoRSS, sniffed XML) | Unreadable place/hole, excess coordinate pairs | GML dimensions, other members, attributes |
| LandXML (including sniffed XML) | Unreadable point, parcel without geometry | Elevations, curves/parts, attributes |
| GML | Z | M, members/holes/curves, attributes |
| CityGML / CityJSON (distinct parsers) | Varying line Z in each | Other dimensions, surfaces/members/holes, attributes |
| DXF | Varying line Z, unreadable entity/vertex | Other entities/curves/parts, attributes |
| MapInfo MIF/MID | Unreadable object, arc, short part/ring | Other geometry and attribute conversion |
| OSM PBF | Missing member/node, unclosed/outside inner ring | Other relations, attributes; native coordinates are XY |
| Feature-service PBF (distinct sniffed PBF parser) | Point Z and M, separately | Other geometries/dimensions, geometry members, attributes |
| GeoPackage / FlatGeobuf / GeoParquet / FileGDB (distinct readers) | Z in each | M, members/parts/curves where supported, attributes |
| LAS/LAZ | None in this paired corpus | Sampling, dimensions/attributes; geometry members/holes inapplicable |
| XLSX | WKT Z/members, error-valued attribute | M, other geometry, dates/formulas/names/encoding |
| ODS / XLS (distinct workbook parsers) | WKT Z/members in each | M, other geometry, dates/errors/names/encoding |
| XLSB | WKT Z/members with full cell records; short-cell attribute loss | Pinned silent short-cell loss; M, other geometry, dates/errors/names/encoding remain gaps |
| XLSM | Shared XLSX OOXML implementation only | No distinct fixture; macros and macro fidelity untested |
| Generic ZIP | Delegates to inner readers; KMZ covered above | Other wrapper parity |

XLSM uses the same OOXML parser as XLSX; merely renaming a workbook would not establish
another parser's coverage. Raster/image/grid/PDF fidelity and external program, database
and network-provider losses are outside this local vector/table corpus. Existing tests
elsewhere (including LAS whole-read fidelity) do not count as paired coverage here.
