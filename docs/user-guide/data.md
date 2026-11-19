# Data

## Formats

Every format, with how to add it and its limits, is listed on [Supported Data
Formats](data-formats.md).

| Format | Read | Write |
| --- | --- | --- |
| GeoJSON (`.geojson`, `.json`) | Yes | Yes |
| Delimited text (`.csv`, `.tsv`, `.txt`, recognised `.dat`; also UTF-8 gzip-compressed text) with x/y columns in a supported coordinate system, or WKT geometry | Yes | CSV |
| Shapefile (`.shp` + `.dbf`, or zipped in a `.zip`) | Yes (the web app also accepts sidecar files picked or dropped together) | Yes (`share.exportLayer`) |
| GeoPackage (`.gpkg`, feature tables) | Yes | Yes (desktop) |
| GeoParquet (`.parquet`, `.geoparquet`, WKB geometry) | Yes | Yes |
| FlatGeobuf (`.fgb`) | Yes | Yes (`share.exportLayer`) |
| KML/KMZ (with their styles), GPX | Yes | Yes |
| Spreadsheets (`.xlsx`, `.xls`, `.ods`), GeoRSS feeds, LandXML, encoded polylines | Yes | No |
| GML, MapInfo MIF/MID, OpenStreetMap PBF, TopoJSON, CityJSON (footprints with heights), AutoCAD DXF (ASCII) | Yes | No |
| LiDAR point clouds: LAS, LAZ, COPC (an evenly spread sample) | Yes | No |
| Geotagged photos (JPEG with EXIF GPS): points with their thumbnails | Yes | No |
| Esri File Geodatabase (`.gdb` folder; 3D multipatches as footprints) | Yes (desktop) | No |
| Through GDAL: AutoCAD DWG, MapInfo TAB, OpenStreetMap XML, File Geodatabases RustGIS can't read itself… | Yes (desktop, when GDAL is installed) | No |
| PostgreSQL/PostGIS databases, read live (large tables a view at a time) | Yes (desktop and phones) | No |
| MySQL and MariaDB databases, read live (large tables a view at a time) | Yes (desktop and phones) | No |
| SQL Server and Azure SQL databases, read live (large tables a view at a time) | Yes (desktop and phones) | No |
| DuckDB databases (`.duckdb`, `.ddb`) and SQL queries | Yes (desktop, when DuckDB is installed) | No |
| GeoTIFF and cloud-optimised GeoTIFF (`.tif`, `.tiff`) | Yes | Yes |
| ASCII grid (`.asc`, with an optional `.prj`) | Yes | No |
| Unreferenced TIFF and PNG scans; JPEG with `"as": "scan"` | Yes | GeoTIFF after georeferencing |
| PDF pages: GeoPDFs placed by their coordinates, other pages as scans | Yes (desktop, command line and Python) | GeoTIFF after georeferencing |
| Through GDAL: JPEG 2000, ERDAS Imagine, SRTM HGT, DTED, NetCDF, HDF, ENVI, SAGA… | Yes (desktop, when GDAL is installed) | No |
| Web tiles: XYZ/TMS/WMS rasters, Mapbox Vector Tiles, MapLibre styles, PMTiles and MBTiles archives | Yes | Yes (Imagery › Create Hosted Imagery) |
| RustGIS project (`.rgis`; older `.rgisx` files open too) | Yes | Yes |

Add data with **Home › Add › Add Data**: its main button opens the file picker, and its menu offers
other ways to add data, including **Delimited Text…**, **From URL…** and **From Service…**. Files
can also be dropped on the window or passed on the command line (`rustgis places.geojson`). The
file picker opens formats listed on [Supported Data Formats](data-formats.md), including
cloud-optimised GeoTIFF, GeoParquet and FlatGeobuf (read in place, a view at a time; see [Files
on the web](data-formats.md#files-on-the-web)). **Home › Add › Data Sources** opens the pane with
saved connections, favourites, recent data and the public-source library.
On the desktop, a file that holds several layers (a File Geodatabase, a GeoPackage, a DuckDB
database or a file read through GDAL) asks which to add when it is picked, dropped or
double-clicked in the Browser: **Choose Layers** lists them, all ticked, with **Select All**,
**Select None** and a filter for a long list. Turn **Ask Which Layers to Add** off in the Add Data
menu to add every layer, as before. Scripts pick one with `layer.addData`'s `"layer"`;
`info.fileLayers` lists them.
A DXF drawing opens **Import DXF** when it is picked, dropped or double-clicked: it asks for the
coordinate system the drawing's coordinates are in (the map's, to start with; blank keeps them as
they are) and whether its blocks (图块) are drawn. Drawings dropped together ask one after the
other, so each keeps its own answers.
A KML, KMZ, GPX, OpenStreetMap, DXF, LandXML or GeoRSS file, a zip of Shapefiles or a File
Geodatabase that RustGIS reads itself, with layers of several geometry types, adds its polygons
first and its points last, so its points are drawn on top, and a KML's ground overlays go under
them. A name given goes to the first map layer added, the bottom one (a standalone table only gets
it when the file has no map layer). A GeoPackage adds its tables in name order, and a file read
through GDAL keeps its layers in the file's order and orders only the types within each layer.
GeoPackage attribute-only tables are not supported. Adding a whole file reports their count in one
notice. Tile and other unsupported GeoPackage contents are also counted by type. The omission counts
cover catalog entries without a registered geometry column; unknown content types are combined into
one counted "other" group. Only contents registered in `gpkg_contents` are considered; arbitrary
unregistered SQLite tables are ignored. Desktop uploads add their first feature table, with the
existing note naming how many feature tables the file contains. Browser uploads with virtual
generated `table_name` or `data_type` catalog columns cannot be evaluated and are refused with an
explanation; materialize these columns or select a feature table explicitly. A file containing only
attribute or tile tables is refused with the count. Selecting a feature table explicitly adds only
that table.

On the desktop, **Home › Add › Add Data › Add Folder…** connects a folder to the Browser; the web
app does not offer folder connections.
On the desktop, a file Add Data reads whole (a GeoTIFF or another raster, one GDAL converts, a
GeoJSON of up to 32 MB, delimited text, a ZIP archive or a spreadsheet) is read in the background, so the map
and the rest of the window stay in use: the status bar shows **Add Data**, how many files wait
after it, and **Cancel**. Files dropped or added together are read one at a time and added in
that order; one that asks which layers to add joins them once you choose. Cancel drops the file
being read and those waiting; opening another project, or switching to another map, drops them
too, and a message says so (the files waiting are counted, not named). A file read in place, a web
service or a database adds as before, and scripts read the file where the command runs. So does
**Delimited Text…**: it reads the file with the columns picked as you press **Add**, so a read
that fails (a column that isn't in the file, a coordinate system it doesn't know) keeps the
dialog open for another try.

Published pipe-delimited `.dat` files open as tables when their header and rows reveal a
separator; otherwise give `"delimiter":"|"` to `layer.addData`, or rename a text file to `.txt`.
Gzip-compressed UTF-8 `.csv.gz`, `.tsv.gz`, `.txt.gz` and `.dat.gz` open directly, including by
URL or upload. Decompression streams records, checks the gzip checksum and stops at the size read into memory
(256 MiB in the web app; on the desktop it follows the computer's memory) of expanded text. Table rows are still kept in memory for type inference; CSV imports also refuse
a retained-table estimate above a quarter of physical memory, between 256 MiB (the browser
budget) and 4 GiB. This estimate includes cell objects and the dataset built alongside the rows. Delimited members
in one ZIP share this allowance, including gzip-compressed members.
For another encoding, decompress the file and add the `.txt` file, whose reader detects the encoding. Delimited text
is exported as `.csv`; gzip is an input format.

A GTFS zip adds its delimited `.txt` members as standalone tables; `stop_lon` and `stop_lat`
place stops as points. This imports the tables, not a transit routing network. The archive's
usual 32-layer/table limit applies, and skipped readable members are named in a notice. Text
members that do not resemble tables, including single-column `README.txt` documentation, are
skipped with a notice. Give `"crs"` when the archive's text tables use projected coordinates.
A zipped File Geodatabase must be unzipped first: on the desktop, add the
`.gdb` folder. The archive reader says this when it finds one, including beside readable files.

**Home › Add › Add Data › Delimited Text…** asks for a CSV or a spreadsheet first, then lists its columns to pick the x and y (or WKT)
ones from, with the coordinate system (such as `EPSG:2263`) and the separator (changing it reads
the columns again); `info.fileColumns` lists them for scripts. Add Data finds them itself
(columns named like longitude/latitude, including GTFS `stop_lon`/`stop_lat`, easting/northing or
geometry, and EWKT's `SRID=`). With no projected coordinate system or explicit column choices,
RustGIS compares complete longitude/latitude and X/Y pairs regardless of column order.
X/Y includes planar aliases such as easting/northing. RustGIS counts in-range, non-0,0 positions
for each pair, preferring longitude/latitude when the counts are equal and nonzero.
That pair is used for the whole file. Automatic detection keeps X/Y and longitude/latitude
separate. If it finds candidate names for both axes but no matching pair or WKT column,
the file opens as a table with a notice naming the candidates so you can choose x and y.
The published aliases `Longitude Decimal Degrees`/`Latitude Decimal Degrees` and
`Longitutde`, `Longtitude` and `Lattitude` are recognised too, and so are longitude and latitude
in other languages and scripts: Chinese (`經度`/`緯度`, `座標經度`/`座標緯度`, `经度`/`纬度`), Japanese
(`経度`/`緯度`), Korean (`경도`/`위도`), and the words for them in Vietnamese, Thai, Indonesian, Hindi,
Arabic, Persian, Hebrew, Russian, Ukrainian, Greek, German (`Längengrad`/`Breitengrad`), Dutch,
Spanish (`longitud`/`latitud`), Italian, the Nordic languages, Polish, Czech, Hungarian, Turkish
and others, in any case. A note in brackets after a name (`經度(X)`, `Longitude (WGS84)`,
`lat [deg]`) is set aside; a name as written comes before one with a note, in English and in
other languages. A word that is also an object's length or width (German `Länge` and `Breite`)
isn't taken as a coordinate, nor named in the notice; Spanish `longitud`, which is also a length,
is taken only as written and beside `latitud`. English names come first when a file has both,
and export writes English names. A name whose accents a file saved as separate marks (as some
Macs save Vietnamese) reads as one typed with them. The dialog checks the first 1,000 rows
of delimited text, or every row of a spreadsheet, to pick defaults. It recomputes unchanged defaults when you
leave the coordinate-system box or press Enter; columns you picked stay picked. With a projected
coordinate system, defaults prefer X/Y or easting/northing columns when the file also has
longitude/latitude columns.
If you leave the suggested columns untouched, Add Data checks all rows before choosing the pair.
Coordinates still outside degree ranges are counted in a
notice. Points placed at exactly 0,0 are also counted in a notice: these often mean missing
coordinates, but the points are kept. A CSV without coordinate columns, or a layer without geometry, opens as a
standalone table, listed under the map in Layers; right-click it to open or remove it. Tables
can share a name: commands and tools take a table's id (`info.project` lists them) as well as its
name, which stands for the first table of that name. An id given as a number is never taken for
a name. The Tools pane and Geocode Addresses send the id of the table picked; a tool's run keeps
the table's name beside it (`input_name`). The **XY Table To Point** tool
([Geoprocessing](analysis.md), and a table's right-click menu) makes points of a table already in
the project, from two fields of coordinates and their coordinate system. In a text field a single comma is the decimal separator
(`1,234` is 1.234); thousands separators are not read.

**Use a polygon's published interior point.** To place TIGER interior points, run **XY Table
To Point** with the polygon layer as **Input Table**. Set **X Field** to `INTPTLON` and **Y Field**
to `INTPTLAT` (use `INTPTLON20` and `INTPTLAT20` when the fields have that suffix). The tool
reads signed, zero-padded text directly, such as `-093.1445780` and `+44.9500000`.
Set the tool's **Coordinate System** (`crs`) to the fields' system: NAD83, `EPSG:4269`, for
TIGER/Line. Left empty, it places them as WGS 84. The new layer
keeps the attributes and places points at the published coordinates. Check the tool's messages
for any rows kept without a point.

Excel `.xls` records, formula operands and sheet bounds are checked before opening, including
sheets that were not selected. The workbook shares a memory budget across its sheets and retained
metadata. Value and formula grids are counted separately, with conservative allocation estimates
for formula strings, defined names and sheet state. Formula-heavy workbooks can reach this budget
before their value-grid limit. A file with damaged records or excessive allocations is refused
with the reason. Normal cached formula values are read. A formula's direct cell reference to
row 65,536 is currently refused because the XLS reader cannot handle that reference safely.

**Home › Add › Add Data › Graphics Layer** adds a layer for [drawn graphics](cartography.md#graphics),
and **Data › Layer Templates** adds empty layers with ready-made schemas to draw into.

Dates and date-times remain typed when reading Shapefile, GeoPackage, GeoParquet, local
FlatGeobuf, FileGDB, workbook date cells and GPX timestamps. Ordinary spreadsheet text that
looks like a date stays text. A workbook column mixing date cells with ordinary text is read
entirely as text; clean that column in the workbook before using date operations.
Invalid dates become empty values with a notice naming the fields
and counting the values; dates cover years 1–9999. Fractional seconds are kept to microseconds:
digits past the sixth (nanosecond timestamps in Parquet, text read from a GeoPackage or FlatGeobuf, or
converted by Convert Field) are rounded to the nearest microsecond, and a read says how many values it
rounded and in which fields. Text typed in, kept in a project, read from GPX or compared in a
where clause isn't rounded: with more digits than that it isn't a date. Floating-point serial dates in workbooks and FileGDB are rounded
the same way, with a notice when rounding is needed. A workbook's 1900-02-29 (serial 60, a day
that never existed) is read as empty and counted, in a column of dates or any other.
Times without a day stay text; Parquet clocks retain `Z` when declared UTC.

GeoParquet exports use native dates and microsecond timestamps. Zoned date-times are written
as UTC instants, with a notice when their displayed offsets change; unzoned times stay unzoned.
A column mixing zoned and unzoned times is written as ISO text, with a notice, to preserve both.
The writer makes a cancellable pass over timestamp columns before writing to choose that policy;
a streamed export reads its source twice.
If newly available date columns no longer match a saved layer's field positions (for example,
a formerly unsupported INT96 column), reading or exporting that layer is refused. Add the file
again to use its current schema; the saved fields are kept unchanged. Saved GeoParquet layers that still list dates as epoch integers are also refused: add the source
again. Their saved fields, filters and styles are kept unchanged rather than partially migrated. An old integer field that
now reads as time-only text is refused in the same way.
FlatGeobuf exports use its native DateTime column type for both dates and date-times. Local
files are scanned to distinguish date-only columns, date-times and time-only text. With no value
to scan, a date field comes back as a date-time field, and the export says which fields. Reopening
a saved desktop layer runs that scan off the window thread, with progress and Cancel in the
status bar. Remote
FlatGeobuf date/time columns stay ISO text with a notice so opening them can keep reading by
range without downloading every feature.

Vector geometry is stored as WGS 84 longitude/latitude. Data in another supported coordinate
system is converted as it is read; Shapefiles use their `.prj`, and formats such as GeoPackage,
GeoParquet and FlatGeobuf carry their coordinate system in the file. Rasters keep their own
coordinate system and are reprojected as they draw. The map uses Web Mercator by default, but
**View › Projection** can change it; see [Maps](maps.md).

Coordinate systems are read and transformed as PROJ 9.9.0 does, through terrio-crs. A few EPSG
and ESRI codes PROJ 9.9.0 dropped or can't compute (EPSG:3752 and 22700, ESRI:10820 and 65162)
keep the definitions RustGIS read them by before, so they don't follow PROJ exactly. A project
saved by an earlier release keeps the definition it saved for a layer in an ESRI code, so that
layer is placed as it was then (ESRI:102144 246 m from PROJ 9.9.0, for one); the same code read
from a file now is placed as PROJ places it.

ESRI `.prj` files without an EPSG code use their projection parameters, and their datum is recognized by
the name Esri gives it (ED50, OSGB36, DHDN, `D_Philippine_Reference_System_1992`,
`D_Pulkovo_1942`, `D_Tokyo` and others): it takes the transformation PROJ uses for that datum
and place. In a `.prj` or other WKT:

- A transformation in the file (a `TOWGS84`, or a WKT2 `BOUNDCRS`'s) takes precedence, also over
  the built-in one of an EPSG code in the WKT, as in PROJ and GDAL. It also takes precedence over
  the EPSG code given beside the WKT in a GeoPackage system, a FlatGeobuf header or a File
  Geodatabase. A system
  RustGIS can read only by that code keeps the code's transformation, with a notice. Web Mercator
  is on WGS 84, so a transformation its file gives is set aside, with a notice.
- A `BOUNDCRS` whose transformation goes to another datum than WGS 84 continues with that
  datum's own transformation to WGS 84: the one the file gives it, else none for a datum treated
  as WGS 84 (such as RDN2008, IGM95 or NAD 83), else the built-in one. It is refused when there is
  none of these, when the target gives a zero transformation where its datum has another (a
  writer's placeholder, as for ED50 or NAD 27), or when combining the two transformations'
  rotations could be off by a centimetre. When it is refused beside the EPSG code of a GeoPackage
  system, a FlatGeobuf header or a File Geodatabase, the code is used instead, with a notice.
- A grid transformation (NTv2 or NADCON, a `BOUNDCRS`'s, or GDAL's WKT1 `PROJ4_GRIDS` or
  `+nadgrids` in its `PROJ4` extension), or another without Helmert translations, isn't applied:
  the system is read without it, with a notice naming it and its grid file, as positions may be
  off by as much as that shift. A system read by an EPSG or Esri code that has its own shift to
  WGS 84 uses that shift instead, and the notice says so. A `BOUNDCRS` without a transformation, which
  PROJ refuses, is read as its source system, with a notice unless the source is on WGS 84. A
  ballpark offset is no known shift (the datum's own notice says so), and a null offset is none.
- The notices come with WKT read from a Shapefile's or ASCII grid's `.prj`, a GeoPackage's,
  FlatGeobuf's or FileGDB's system, a LAS file or a Zarr store. WKT given as a command's `crs`,
  and a GeoPackage or FlatGeobuf system read by its EPSG code, say of these only the two above: a transformation that isn't
  applied, and a `BOUNDCRS` without one (typed WKT also says its projection method's formula, as
  below). A Zarr store's `crs` says them only when the slice uses it: a store that gives its own
  system, or a longitude/latitude grid, doesn't. A Zarr store whose axes are longitude/latitude is
  read in the geographic system it gives (a datum such as ED50, or another body's, said once).
  With no system, or a projected system on degree axes, it uses WGS 84 and says so once for a
  slice or a point series. When the store gives no system, the notice names a caller's
  non-WGS84 `crs` that is ignored on those axes; explicit WGS 84 is not an assumption.
  A declared store system takes precedence over the caller's fallback. A PROJ definition that names an ellipsoid
  and no datum shift says so, as a WKT's datum without one does. Point time series use the
  geographic system too when locating a WGS 84 query. A web store's slice or a directory window
  is kept in a project as a GeoTIFF; if its keys cannot preserve the declared system (such as
  another body's or a custom datum shift), adding it is refused rather than relabelling it
  WGS 84. Imported slices retain the read notices that the GeoTIFF cannot carry.
  A PostGIS, MySQL or SQL Server layer's `crs` is
  read only for a geometry with no SRID of its own, by its EPSG code, with the same two notices; a
  geometry that has an SRID ignores it, so nothing is said of it. A DuckDB query's `crs` isn't
  read by RustGIS: it goes to DuckDB, which transforms the geometry from it even when the
  geometry's type gives a system, so none of these notices come with it.
- A UTM name (`WGS_1984_UTM_Zone_31N`) gives its EPSG code only when the parameters are that
  zone's.
- Transverse Mercator (South Orientated), the method GDAL and Esri write South Africa's Lo grids
  with, counts westings and southings, and is read so, its false easting and northing as a false
  westing and southing. Read from its parameters, a system declaring other axes is refused; one
  with an EPSG code (EPSG:2048, say) is read by the code, as the code defines it, whatever axes
  its WKT declares.
- Projected axes pointing west or south, as in South Africa's Lo grids, are read as such, or
  refused where they can't be (Web Mercator, Krovak, sinusoidal, or such axes declared northing
  first, as in Austria's Ferro grids).
- A WKT1 or Esri projection's angles (its latitude of origin, central meridian, standard
  parallels, azimuth) and its prime meridian are in its geographic system's unit, such as grads
  for NTF (Paris)'s Lambert zones, as PROJ reads them. The Paris meridian written as 2.33722917,
  in degrees as GDAL and Esri write it whatever the unit, is read in degrees.

RDN2008 and IGM95 are treated as WGS 84-like datums. These transformations do not replace
regional grid corrections for survey accuracy. An unrecognized datum keeps its ellipsoid and
produces a notice when no transformation is available. Swiss oblique Mercator, the standard
Krovak East/North variant, the oblique Mercator in its azimuth forms (the two-point form, with
no azimuth, is refused), and the Cassini-Soldner, polyconic and Equal Earth methods are
supported from their parameters, as are sinusoidal (MODIS), Robinson, orthographic and Mercator
(variant A), in `.prj` files and in GeoTIFF keys. Natural Earth, Winkel Tripel and Eckert IV are
read from WKT as GDAL, Esri and PROJ write it, and by their ESRI codes (Natural Earth and Eckert
IV, on WGS 84 and on a sphere: `ESRI:54077`, `53077`, `54012`, `53012`). They're computed on a
sphere of the datum's semi-major axis, as PROJ computes them. Winkel Tripel's standard parallel
is the file's; a definition or WKT without one is at 0, as PROJ and GDAL take a coordinate
system without one (PROJ's `proj` program, which runs the bare projection, takes Winkel's own
instead). The map's Winkel Tripel is at Winkel's own, acos(2/π) (about 50.46°), and its
definition and WKT say so. A parameter that isn't a finite number, or a scale factor other than
1, is refused rather than left out. Esri's orthographic is a spherical projection:
on an ellipsoid it is refused, with a notice, rather than computed as EPSG's ellipsoidal one.
The equidistant cylindrical projection (EPSG:4087, World Equidistant Cylindrical) is computed on its
datum's ellipsoid (EPSG method 1028), as [PROJ 9.8](https://proj.org/en/stable/operations/projections/eqc.html)
computes it, both for data and for a map drawn in it (including the Equirectangular preset); on
a sphere it is the spherical formula. PROJ 9.7 and earlier, and so files they wrote, used a
sphere of the semi-major axis on an ellipsoid too: such coordinates are placed up to tens of
kilometres away from where that software put them (about 25 km at 55°N). Shapefile and ASCII-grid
`.prj` files and GeoPackage WKT definitions that name EPSG method 1028 on an ellipsoid say so;
code-only identifiers and GeoTIFF's coordinate transformation 17 don't name the method and say
nothing. The datum's ellipsoid is retained for datum transformations and
exported coordinate system definitions.

Shapefile text uses a supported `.cpg` encoding, otherwise the `.dbf` code page, with UTF-8 as
the fallback. Keep the sidecar files together. A shape with a coordinate that isn't a number (NaN,
which some programs write for an empty point, or an infinity) has no place: its feature keeps its
attributes without geometry, and a notice says how many (a large Shapefile read in place on the
desktop leaves the whole record out, counted with its unreadable shapes). A project, project package or map package is never saved with such a
coordinate, since it couldn't be opened again: the save is refused, saying which layer, and a file
already there is kept as it was.

## Attribute tables and fields

Open a feature layer's table with ++ctrl+t++ (++cmd+t++ on macOS), **Home › Selection › Attributes**
or **Attribute Table** on a feature layer's **Data** tab (`table.open`). Rows and map selections stay in sync.
Standalone tables open from their right-click menu and are edited the same way
([editing a standalone table](analysis.md#editing-a-standalone-table)). See
[The attribute table](analysis.md#the-attribute-table) for sorting, filtering, selection and the SQL `WHERE` clause syntax used by **Select By Attributes**
and **Definition Query** (the full grammar is in [Expressions](expressions.md)). These clauses filter a layer; they are not DuckDB queries.

For an editable feature layer, double-click a cell to change it. The table's toolbar also adds
fields, calculates field [expressions](expressions.md) and opens **Calculate Geometry** (`edit.calculateGeometry`).
The geometry calculator fills a new double field or an existing numeric field with area,
perimeter, length or point X/Y, for all or selected features. Point coordinates can be WGS 84
degrees or a supported projected EPSG system's units. Areas, perimeters and lengths measure on
the WGS 84 ellipsoid; how full-turn rings (globe-wide bands) and rings split at the antimeridian
are measured, and the exception for polar rings, are under
[Geoprocessing](analysis.md#geoprocessing-tools).
See [Editing](editing.md) for field
calculations and data-design rules. Streamed layers must first be loaded with **Load All
Features**.

## Formats through GDAL

When [GDAL](https://gdal.org) is installed (its `ogr2ogr` and `ogrinfo` programs on the `PATH`,
or the folder holding them in `RUSTGIS_GDAL_BIN`; GDAL 3.8 or later), the desktop app also opens
the vector formats GDAL reads: DWG, MapInfo TAB, OpenStreetMap XML, File Geodatabases RustGIS
can't read itself (compressed ones, or ones with raster fields)
and more. RustGIS doesn't link to GDAL: each layer is converted once, by `ogr2ogr`, into a
FlatGeobuf with a spatial index in WGS 84 (GDAL reprojects it), kept in the cache folder
(`~/.cache/rustgis/gdal` on Linux, `~/Library/Caches/RustGIS/gdal` on macOS,
`%LOCALAPPDATA%\RustGIS\cache\gdal` on Windows, or `RUSTGIS_GDAL_CACHE`). Adding the file again
reuses the conversion and its layer listing until the file's path, size or modification time
changes (for a File Geodatabase folder, RustGIS checks its files too). Missing or unreadable
cached listings are rebuilt; **Reload** always reads the source afresh. The first listing skips
spatial extents but keeps feature counts to check for conversion losses. Large layers stream
like any large FlatGeobuf (below). Every layer of the file is added; a layer mixing geometry
types is added as one layer per type, its points on top and its polygons at the bottom. Curves become straight segments, and
3D surfaces (an Esri multipatch, such as the buildings of swissBUILDINGS3D, is read as a TIN)
become flat polygons, one part per face, without their heights. The conversion leaves out a
feature it can't index, such as a polygon with no coordinates; when GDAL gives the layer's feature
count, adding the file says how many were left out, also when it reuses a conversion. Projects
remember the original file and convert it again if the cached copy is gone. A listing that runs
past five minutes, or a conversion past thirty minutes, stops with an error naming the source
file and the operation that exceeded its limit.

## SQL queries and DuckDB databases

When [DuckDB](https://duckdb.org) is installed (the `duckdb` program on the `PATH`, or the folder
holding it in `RUSTGIS_DUCKDB_BIN`; DuckDB 1.5 or later), the desktop app adds the result of an SQL
query as a layer, with the `layer.addSql` command (Add Data From SQL in command search):

```json
{"sql": "SELECT name, geometry FROM read_parquet('places.parquet') WHERE country = 'FR'", "name": "French places"}
```

**Analyze › SQL** opens the SQL pane, a workspace for these queries: an editor (Ctrl+Enter runs
the query) that completes table and column names of the database named above it and DuckDB
Spatial's common functions, the result's first 200 rows as a table, **Add to Map** for a result
with a geometry column, and a history of the queries run (kept by the app with its preferences,
without the database's password; a query that seems to hold a credential, such as `user:password@`
in an address, a signed URL or a secret, isn't kept). Queries run in the background, so the app keeps drawing while DuckDB works. The
commands behind it are `sql.query` (a query's first rows, up to 1,000, without adding a layer) and
`sql.schema` (a database's tables and their columns); both take `"database"` as `layer.addSql`
does. The web app has no DuckDB, so the pane says to open the project in the desktop app.

The query is one DuckDB `SELECT` (or `FROM …`, `WITH …`), with [DuckDB
Spatial](https://duckdb.org/docs/stable/core_extensions/spatial/overview)'s `ST_` functions: it can
read Parquet, CSV, JSON and GeoParquet files, anything `ST_Read` reads, and web addresses (`https://`,
`s3://`), so a bounding box of [Overture Maps](https://overturemaps.org) is one query:

```sql
SELECT id, names.primary AS name, geometry
FROM read_parquet('s3://overturemaps-us-west-2/release/2026-09-23.1/theme=places/type=place/*', hive_partitioning = 1)
WHERE bbox.xmin BETWEEN -83.95 AND -83.89 AND bbox.ymin BETWEEN 35.94 AND 35.98
```

The first geometry column becomes the layer's geometry; other columns become its attributes (types
RustGIS doesn't read, such as lists, as text). `DATE` columns become dates, `TIMESTAMP` columns
date-times without a zone and `TIMESTAMPTZ` columns instants in UTC; a `TIMESTAMP_NS` is a
date-time too, rounded to the microsecond, with a notice. A geometry that says its coordinate system
is converted to longitude/latitude; give `"crs"` (such as `EPSG:2263`) for one that doesn't. A
result mixing geometry types becomes a layer per type (points on top, polygons at the bottom), and
rows without a geometry are left out.
`"database"` opens a DuckDB database (read only) so the query names its tables plainly; the
database is trusted like the query, since its views run as part of it.

**Home › Add › Add Data** opens a DuckDB database file (`.duckdb`, `.ddb`): every table with a geometry
column becomes a layer (`"layer"` picks one).

### Count GTFS departures per stop

To count scheduled departures from 07:00 up to but excluding 09:00 on Tuesday, 8 December 2026,
unzip the feed and run this in **Analyze › SQL** with DuckDB installed. Use the extracted files'
full paths in `read_csv`. Reading the fields as text keeps IDs such as `006` distinct from `6`.

```sql
WITH exceptions AS (
  SELECT service_id, exception_type
  FROM read_csv('calendar_dates.txt', all_varchar = true)
  WHERE date = '20261208'
), active AS (
  SELECT service_id
  FROM read_csv('calendar.txt', all_varchar = true)
  WHERE tuesday = '1' AND '20261208' BETWEEN start_date AND end_date
    AND service_id NOT IN (SELECT service_id FROM exceptions WHERE exception_type = '2')
  UNION
  SELECT service_id FROM exceptions WHERE exception_type = '1'
)
SELECT s.stop_id, count(*) AS departures
FROM read_csv('stop_times.txt', all_varchar = true) AS s
JOIN read_csv('trips.txt', all_varchar = true) AS t USING (trip_id)
JOIN active AS a ON a.service_id = t.service_id
WHERE lpad(s.departure_time, 8, '0') >= '07:00:00'
  AND lpad(s.departure_time, 8, '0') < '09:00:00'
  AND s.stop_id IS NOT NULL
  -- With frequencies.txt in the feed, also leave out its template trips:
  -- AND t.trip_id NOT IN (SELECT trip_id FROM read_csv('frequencies.txt', all_varchar = true))
GROUP BY s.stop_id
ORDER BY s.stop_id
```

Match date exceptions to the weekly calendar by `service_id`. Type 2 removes service and
type 1 adds it, even when the ID has no weekly row. Then match active services to trips by
`service_id` and trips to stop times by `trip_id`. For another date, change both date literals
and the weekday column. If the feed uses only `calendar_dates.txt`, keep just the additions
in `active`. If it uses only `calendar.txt`, keep the weekly rule and omit the exception checks.

The time filter pads one-digit hours and uses the feed's local service-day times, where
`24:30:00` is after midnight on that service day. The result counts timed rows per `stop_id`,
including repeat visits by a trip. Stops with no departures in the window have no row. The SQL
pane previews the first 200 rows. This query uses the times in `stop_times.txt`. Frequency-based trips need a
separate calculation using [`frequencies.txt`](https://gtfs.org/documentation/schedule/reference/#frequenciestxt),
and their `trip_id`s left out of this query: their stop times are a template, which it would
count as one departure. When the feed has that file, remove the `-- ` before the
`AND t.trip_id NOT IN …` line.

### PostgreSQL and PostGIS

**Home › Add › Add Data › Database…** (`layer.addDatabase`, desktop) connects to a PostgreSQL database with
PostGIS and reads its tables live. The dialog takes a saved connection or a typed one, a password,
a table (**List Tables**, or all tables with geometry) and a coordinate system for geometry that
doesn't say its own. RustGIS talks to the server itself (nothing else to install;
TLS as libpq does it). `sslmode=prefer` (the default) uses TLS only when the server offers it, and otherwise connects
without encryption, so a server in the middle can force a plaintext connection. `require` always
uses TLS but doesn't check the server's certificate. On networks you don't trust, use
`verify-full` or `verify-ca`; both check the host name here. The certificate is checked against `sslrootcert` (a private authority's PEM file) or the
web's roots. `disable` turns TLS off. The connection is a libpq connection string
(`host=localhost port=5432 dbname=gis user=postgres`) or a `postgresql://user@host/dbname`
address; what it leaves out comes from `PGHOST`, `PGPORT`, `PGUSER` and `PGDATABASE`, as with
libpq. **List Tables** (`info.databaseTables`) shows the tables and views with a geometry or
geography column, and every one of them becomes a layer unless one is chosen (`table`,
`schema.table`, or `table:column` for one of several geometry columns). The connection is read
only.

A table of more than 100,000 rows (or any, with `"stream": true`) is read in place a view at a
time, like a large GeoParquet: the view's box is matched against the column's spatial index, and
when more rows match than a view draws, an evenly spread sample of them is numbered on the server,
so only those cross the network. Smaller tables are loaded whole, so they can be edited. The
server converts geometry to longitude/latitude (`ST_Transform`, so every coordinate system in its
`spatial_ref_sys` works) and curves to lines; give `"crs"` (such as `EPSG:2263`) for a column whose
geometry doesn't say its coordinate system. A column of mixed geometry types becomes a layer per
type, points on top, and empty tables (such as the TIGER geocoder's) are left out. Features keep
their ids between views: the table's integer primary key, else the row's place in the table (the
rows of a view or a partitioned table without a key are numbered as read, so theirs can change
between views).

`date` columns are read as dates, and `timestamp` and `timestamptz` columns as date-times:
a `timestamp` stays the wall-clock time it holds, and a `timestamptz` is the instant, shown in UTC
whatever the server's or database's time zone. Values a date field can't hold (`infinity`, a
year BC or after 9999) are read as empty, and the layer counts them by field. `time` and
`interval` columns stay text.

Passwords are never written to the project. By default, an entered password lasts until
RustGIS closes. For a named desktop connection, **Remember on this computer** is an unchecked
opt-in to the operating system's password store; see [Named connections](#named-connections)
for reconnect, selection and forgetting rules. Opening a project can use a remembered password
only for a unique, exact saved-address match, or an explicit session selection. Otherwise a
waiting layer offers **Log In…**, or a chooser when several remembered connections match.
Without a matching connection, connect again with **Home › Add › Add Data › Database…**.
`PGPASSWORD` or libpq's password file (`~/.pgpass`, or
`%APPDATA%\postgresql\pgpass.conf`) can serve when you explicitly add a table without a password;
opening a project never newly authorizes these ambient credentials. A small table's features
are kept in the project as they were when added.

`layer.addSql` takes a connection as its `"database"` too, through DuckDB's `postgres` extension
(installed the first time, like DuckDB Spatial), so a query names its tables plainly; the password
reaches DuckDB in its environment (`PGPASSWORD`), never on its command line:

```sh
rustgis-cli run --cmd 'layer.addSql={"sql":"SELECT name, geom FROM roads WHERE lanes > 2","database":"postgres:host=localhost dbname=gis user=postgres","password":"…"}'
```

RustGIS doesn't link to DuckDB: it runs `duckdb`, which writes the result as GeoParquet in the
cache folder (`~/.cache/rustgis/duckdb` on Linux, `~/Library/Caches/RustGIS/duckdb` on macOS,
`%LOCALAPPDATA%\RustGIS\cache\duckdb` on Windows, or `RUSTGIS_DUCKDB_CACHE`), sorted along a
Hilbert curve with a bounding-box covering, so large results stream like any large GeoParquet
(below). DuckDB Spatial is installed the first time (a download from DuckDB's extension
repository). A database table is converted once and reused until the file changes; a query runs
afresh each time. Projects convert a table again if the cached result is gone, but never run a
query when they open: run it again. A database is read on its own, shut out of other files and web
addresses, so a view in it that reaches elsewhere fails rather than reading them.

A query can read any file the app can and fetch any web address, and a database is a file or a
server, so the [AI Assistant](assistant.md) asks before it runs a query or reads a database, and
plugins need both the files and network permissions.

### Editing PostGIS tables

On the desktop, a PostgreSQL table with PostGIS can be loaded to be edited and its edits saved
back to it. In **Home › Add › Add Data › Database…**, give a PostgreSQL connection, choose a table
(**List Tables**) and tick **Load for editing**.

- The connection (`pg.connect`) lasts until RustGIS closes, or until **Disconnect** in the same
  form (`pg.disconnect`). It isn't saved with the project, and neither is the password. Without a
  password, `PGPASSWORD` or libpq's password file serve.
- The table (`pg.addLayer`; `pg.tables` lists those with a geometry or geography column) loads
  whole, up to 100,000 rows a layer, in the order of its key.
  A table whose geometries are of several types loads each type as its own layer (points on top), each up to that many.
  A **condition** (an SQL expression on its columns, such as `population > 10000`) loads only the
  rows it keeps. The layer is an ordinary layer in memory, so every editing tool works on it.

- **Edit › Manage Edits › Save to PostGIS…** (`pg.saveEdits`), there while a layer is loaded for
  editing, shows how many of the layer's rows are new, changed and deleted, and writes them to its
  table in one transaction. **Discard PostGIS Edits…** (`pg.discardEdits`)
  loads the rows again as they are in the database, dropping the edits (Undo brings them back).

A table needs a single-column integer primary key to be added this way. The key becomes each
feature's id, and the layer keeps each row's version (PostgreSQL's `xmin`) and what it held as it
was read. Saving locks the rows you changed or deleted and checks them first: when one was changed
or deleted in the database since you loaded it, or a new row's key is already taken, **nothing is
saved**, and the message lists those rows. Discard PostGIS Edits then loads them as they are now. A row
someone else changed that you didn't edit is no conflict, and stays as they left it.

Undo doesn't reach into the database: undoing a save brings back the layer as it was before, but
the rows stay saved. Saving that layer again is refused, since it would write the same edits
twice (its new rows as more new rows); Redo, or Discard PostGIS Edits, brings the layer back in step.

Only what you changed is written: the columns whose values differ, and the geometry when it moved
(converted back to the table's coordinate system, single or multi type and dimensions). A new row
takes the value of its key field, else the table's default key (`serial`, identity), else its
feature id, and its feature takes the key it was saved with. A new row's empty fields take their
column's default, NULL when it has none. Fields the table has no column for
aren't saved, and the message names them. A date-time with an offset (`+02:00`) saved to a
`timestamptz` column keeps its moment, held in UTC; saved to a `timestamp` column, it keeps the
clock time it shows. Either way its offset isn't kept. A date-time without a zone saved to a
`timestamptz` column is taken as UTC. The message counts each of these by field. The layer says
when the database's user can't insert, update or delete the table's rows; saving is then refused.

```sh
rustgis-cli run --cmd 'pg.connect={"host":"localhost","database":"gis","user":"postgres"}' \
  --cmd 'pg.addLayer={"table":"public.roads","where":"lanes > 2","name":"Roads"}' \
  --cmd 'edit.enable={"on":true}' \
  --cmd 'edit.setValue={"layer":"Roads","row":0,"field":"name","value":"Main St"}' \
  --cmd 'pg.saveEdits={"layer":"Roads"}'
```

Connecting and adding a layer read the database, so the [AI Assistant](assistant.md) asks
before it connects or adds a table, and asks every time before it saves edits. Plugins can't
connect or save at all.

### Editing GeoPackage tables

On the desktop, **Home › Add › Add Data › Edit GeoPackage Table…** (`gpkg.addLayer`) loads one
feature table of an existing local `.gpkg` for editing. Choose its table with **List Tables**
when the file has several. The table needs one `INTEGER PRIMARY KEY`. At most 100,000 rows
are loaded; **Most rows** can lower that limit. Whole numbers may use comma-grouped thousands
(for example, `1,000`); fractional values are rejected. RustGIS does not
directly edit rows outside that load, including rows with unreadable geometry. File-owned
triggers can change other rows.

**Edit › Manage Edits › Save to GeoPackage…** (`gpkg.saveEdits`) writes pending inserts, updates
and deletes to that table in one SQLite transaction. If any row you changed or deleted has
changed or disappeared in the file since it was loaded, nothing is saved. A constraint or
trigger failure also rolls everything back; pending edits remain in the layer. New features
get source-assigned keys that cannot alias loaded rows, and unrelated columns and tables remain
intact. The saved rows are read back within the transaction, so the layer keeps values assigned
by defaults and triggers before it starts tracking its next edits. Dates are saved as a
GeoPackage export writes them: a date-time with another offset than UTC's is saved in UTC, and one
with digits past the millisecond (a GeoPackage DATETIME holds milliseconds, `YYYY-MM-DDTHH:MM:SS.SSSZ`)
is rounded to the nearest millisecond, and a notice says each. A stored date that isn't one loads as empty, with a notice, and is left as it is
unless that value is edited. If a DELETE trigger recreates
a target row, that stored row reappears in the layer and is not counted as deleted. If triggers
change or remove other rows already loaded in the layer, those rows are refreshed or removed
before the next edits; rows outside the load are not added.

New-feature saves also check the table's persisted `last_change` revision. If it changed since
the layer was loaded, the save is refused rather than risking duplicate inserts from an older
project or an undone save reopened in another session. Pending edits remain available to export;
Discard Edits reloads the source and drops those pending edits. Identical feature values are not
used to guess whether a new row was saved before.

Spatial indexes use the final stored geometry after table triggers run. Bounds are refreshed
from the whole table's readable native extents after inserts, geometry edits or deletes; they
can shrink, and become NULL when no readable nonempty geometry remains. Attribute-only saves
keep known bounds unless a trigger writes feature rows. Such trigger writes require a whole-table
spatial rebuild in the transaction; unreadable nonempty geometry, including a body hidden behind
a valid header envelope, makes that rebuild refuse the entire save rather than guess its index
coverage. Metadata or index-repair triggers that would change features after the final snapshot
are also refused, with the transaction rolled back. A geometry type, Z/M declaration or SRS that
changed since the layer was loaded also refuses the save until Discard Edits reloads it.

**Discard GeoPackage Edits…** (`gpkg.discardEdits`) reloads the scoped rows as they are in the
file now, including renamed columns and its current geometry type and coordinate system; Undo
restores the discarded layer edits. A changed geometry family resets its renderer to a compatible
default. Saving refuses a changed source SRS until the layer is reloaded.
Undoing a save does not undo file writes: Redo or Discard Edits is needed before saving again.
Saving the project keeps pending edits in the project only; exporting writes a new file rather
than changing this table.
Save and Discard dialogs remain bound to the project, map and specific layer instance where they
opened. Switching maps or projects, or undoing and re-adding a layer, requires reopening or
choosing again; a reused layer ID cannot retarget another file.
Feature-less project templates retain the layer's schema and styling, but no source-edit binding:
opening a template cannot turn its absent features into source-row deletions. The same holds for
the simplified layers a publication layout's globe locator map keeps: they can't be saved back.

Geometry is converted back to the table's coordinate system and must match its declared family.
Single-part columns reject multipart features; collection, curve and surface columns cannot
receive geometry writes. Geometry writes that would lose existing Z/M dimensions, or insert
geometry into a table requiring them, are refused; attribute-only edits can still be saved.
Read-only files cannot be saved. These editing commands are desktop-only; normal GeoPackage
reading remains available in the browser.

```sh
rustgis-cli run --cmd 'gpkg.addLayer={"path":"/data/roads.gpkg","table":"roads","name":"Roads"}' \
  --cmd 'edit.enable={"on":true}' \
  --cmd 'edit.setValue={"layer":"Roads","row":0,"field":"name","value":"Main St"}' \
  --cmd 'gpkg.saveEdits={"layer":"Roads"}'
```

The AI Assistant asks before reading the file and every time before saving edits. Plugins
need file permission to load or discard rows and cannot save edits back to the table.


## MySQL and MariaDB

**Home › Add › Add Data › Database…** (`layer.addDatabase`) also connects to a MySQL (8 or later) or
MariaDB (10.2 or later) database and reads its spatial columns live, as it does PostGIS tables:
RustGIS talks to the server itself, with nothing else to install. The connection is an address,
`mysql://user@host:3306/database` (or `mariadb://…`); what it leaves out is, as with the
`mysql` client, `localhost`, port 3306 and your login name. **List Tables**
(`info.databaseTables`) shows the database's tables and views with a geometry column, and every
one becomes a layer unless one is chosen (`table`, or `table:column` for one of several geometry
columns). An address without a database lists the tables of every database but the server's own,
as `database.table`. The session is read only, and a server asking for a local file (`LOAD DATA
LOCAL INFILE`) is sent none.

`DATE` columns are read as dates and `DATETIME` columns as date-times without a zone. `TIMESTAMP`
columns, which MySQL holds as instants, are read in UTC whatever the server's time zone. Zero
and partial dates (`0000-00-00`, `2026-03-00`) are read as empty, and the layer counts them by
field. `TIME` and `YEAR` columns stay as they were: text and numbers.

`ssl-mode` in the address's query sets the encryption, as the `mysql` client names it:
`PREFERRED` (the default) encrypts, connecting unencrypted only when the server offers no
encryption (the layer then says so), and `REQUIRED` always encrypts; neither checks the server's
certificate, so neither protects the password from a server in the middle. On networks you
don't trust, use `VERIFY_IDENTITY`, which checks the certificate and the host name (against
`ssl-ca`, a private authority's PEM file, or the web's roots); `VERIFY_CA` checks the
certificate only. `DISABLED` turns encryption off:

```sh
rustgis-cli run --cmd 'layer.addDatabase={"database":"mysql://gis@db.example:3306/city?ssl-mode=VERIFY_IDENTITY","password":"…"}' \
  --cmd 'share.exportMap={"path":"city.png"}'
```

Large tables are read a view at a time through the column's spatial index (`MBRIntersects`), and
a capped view is an evenly spread sample numbered on the server; smaller ones are loaded whole.
RustGIS converts the geometry to longitude/latitude itself (MariaDB has no `ST_Transform`), from
the column's SRID (MySQL's per-column SRID, MariaDB's `REF_SYSTEM_ID`) or else its rows'; give
`"crs"` (such as `EPSG:2263`) for a column whose SRID is 0. A `GEOMETRY` column of mixed types
becomes a layer per type, points on top. Features keep their ids between views from the table's
integer primary key; a table without one has its rows numbered as read, so theirs can change
between views.

Passwords are never written to the project and last for the session unless you opt in with
**Remember on this computer** for a named desktop connection. The same exact-address reconnect,
chooser and Forget rules in [Named connections](#named-connections) apply. `MYSQL_PWD` can serve when you explicitly
add a table without a password, but opening a project never newly authorizes it.
SQL queries (`layer.addSql`, the SQL pane) don't run on MySQL or MariaDB databases.

## SQL Server and Azure SQL

**Home › Add › Add Data › Database…** (`layer.addDatabase`) reads the `geometry` and `geography`
columns of SQL Server 2012 or later and Azure SQL tables and views directly, on the desktop.
Use `mssql://user@host:1433/database`; an empty host means `localhost`, and omitting the database
uses the login's default database. **List Tables** (`info.databaseTables`) lists spatial columns.
Choose a table as `table`, `schema.table` or `table:column`, or leave the choice empty to add all.
For a named instance, use `?instance=SQLEXPRESS` without a port so SQL Browser can discover it.

The connection is encrypted and its certificate checked against the platform's trust store by
default (`encrypt=mandatory`, also `required`, `true` or `yes`). `encrypt=strict` selects TDS 8.0
strict encryption on servers that support it. A private authority can be supplied as
`?ca=/path/to/ca.pem`, and `hostname-in-certificate` overrides the expected certificate name.
For a self-signed server, `?trust-server-certificate=true` disables certificate checking; the
layer reports that it is unchecked. `?encrypt=false` (also `optional` or `no`) requests encryption
for login only, not subsequent traffic; certificate validation still applies during login unless
explicitly disabled with `trust-server-certificate=true`. `connect-timeout` sets a positive
connection deadline in seconds (default: 10, maximum: 300).

```sh
rustgis-cli run --cmd 'layer.addDatabase={"database":"mssql://gis@db.example:1433/city","password":"…"}' \
  --cmd 'share.exportMap={"path":"city.png"}'
```

SQL Server logins work on every desktop platform. On Windows, `DOMAIN%5Cuser` with a password
uses a Windows login: `%5C` is the URL encoding of `\`, and a literal `DOMAIN\user` is also accepted
in the address (escape it as `DOMAIN\\user` inside JSON). Leaving the user out signs in as the
current Windows user. These Windows logins are not supported on macOS or Linux: give a SQL Server
login instead. Passwords can be supplied in user-info or the `password`, `pass`, `pwd`, or `passwd`
query parameters; the separate password field overrides an address's password.

Large tables read a view at a time using their spatial index, with a server-numbered sample when
more rows match than a view draws; small tables are loaded whole. Curves are linearized and
coordinates converted to longitude/latitude. Only rows with the first non-null geometry's SRID
are read; give `"crs"` for SRID 0. Mixed geometry families become separate layers. An integer
primary key provides stable feature ids; without one (including views), ids are numbered as read.

`date` columns are read as dates, and `datetime`, `datetime2` and `smalldatetime` columns as
date-times without a zone. A `datetimeoffset` keeps its offset. RustGIS keeps times to the
microsecond, so a seventh digit of a second (100 ns) is left out. Looking at up to 100,000 rows,
the layer names the columns where those rows hold one, and says a larger table's may when they
hold none (or all of them may, when they couldn't be looked at). `time` columns stay text.

At database compatibility level 100 or lower, streamed `geography` layers filter their whole-table
read or sample locally instead of sending a viewport polygon to the server. A small view can
therefore miss features in a large sampled table. `geometry` layers and higher compatibility
levels retain server-side viewport filtering.

The provider only reads data; edits are not written back to SQL Server. Passwords are never
written to the project and last for the session unless **Remember on this computer** is
checked for a named desktop connection. The [named-connection rules](#named-connections) govern
remembered reconnects and explicit selection. Secret-named query values such as `pwd` and
`access_token` are removed from saved connection addresses. `SQLCMDPASSWORD` serves only when
explicitly adding a database, not when opening a project. Windows integrated sign-in still
requires that same address to have been added this session: opening a project does not
authorize Windows identity. Reconnect with **Home › Add › Add Data › Database…** when needed. SQL queries
(`layer.addSql`, the SQL pane) do not run on SQL Server connections.

## Web Data

**Home › Add › Web Data** gathers public web services and catalogs: STAC imagery catalogs, Overture
Maps, OpenStreetMap, NASA Earthdata, open-data portals, satellite embeddings and more. They have a
page of their own: [Web Data](web-data.md).

## Geocoding addresses

**Home › Add › Add Data › Geocode Addresses…** (`layer.geocode`) turns a standalone table's or a
layer's addresses into points: a CSV of addresses without coordinates is added as a standalone table, then each row's address
fields (`"address"`: a field, or several such as street, city and country, joined with commas)
are looked up, each distinct address once. The result is a new point layer holding every field
plus `geocoded`, what the geocoder called the place, and a text field `match_status`:

- `matched`: the geocoder returned a usable point for the row's address.
- `unmatched`: the row has no usable point, including an empty address or a failed lookup.

Rows that weren't found stay in its table without a point, and a notice counts them; filter
`match_status = 'unmatched'` to review them. A field of the same name in the input is kept, and
the generated field gets underscores appended until its name is unique (`match_status_`,
`match_status__`, and so on); filter the generated field. `matched` means the geocoder returned a point, not that
it is the exact address: it may be placed along the street or at a broader place. The geocoders
don't give a match score, so there is none in the output.

The geocoder is OpenStreetMap's public [Nominatim](https://nominatim.org) by default, or
[Photon](https://photon.komoot.io) (`"provider": "photon"`). Their usage policies allow light
use, so RustGIS sends them at most one request a second and 100 addresses a run; give `"url"` of
a Nominatim or Photon server of your own for more (up to 10,000, with no wait between requests).

For addresses in the Netherlands, `"provider": "locatieserver"` (**Locatieserver (the
Netherlands)** in the dialog) uses PDOK's
[Locatieserver](https://www.pdok.nl/introductie/-/article/pdok-locatieserver-1), the free
geocoder built on the Dutch government's address and place registers (BAG and others): it finds
addresses, postcodes, streets, towns, municipalities and provinces. Its answers are open data from
those registers; Find Place credits PDOK Locatieserver (Kadaster) under the results. PDOK
publishes no request limit for it, so RustGIS sends it four requests a second and up to 10,000
addresses a run. An address is matched to the house before its street (`Damrak 1, Amsterdam` is
the building, not the middle of Damrak). Its places have a point but no extent, so Find Place
zooms in close around them.

For U.S. street addresses, `"provider": "census"` (**U.S. Census (addresses)**) uses the
[Census Bureau geocoder](https://geocoding.geo.census.gov/geocoder/Geocoding_Services_API.html),
with the `Public_AR_Current` benchmark. Give a house number and street plus city/state or ZIP;
it does not search general place names. Coordinates are calculated along address ranges, rather
than surveyed building positions. RustGIS sends one single-record request at a time, at most one
a second, including through a proxy, with up to 10,000 distinct addresses a run; this pacing is a
RustGIS precaution, not a published Census rate limit. This uses the single-line endpoint, not
Census's file-upload service. Puerto Rico addresses requiring Urbanization or Municipio are not
supported by that endpoint. No Census geography attributes are added. Find Place credits the
U.S. Census Bureau and zooms around the returned point. Direct access works in the desktop app
and CLI; Census does not support CORS, so the web app requires a CORS-enabled proxy in **Server**
(`"url"`), exposing the same `/locations/onelineaddress` endpoint.

Find Place shows up to eight candidates; `map.findPlace` honours its `limit` (1–20). When
the server returns additional candidates, a notice counts those not shown, including candidates
not inspected past the parser's 20-result safety cap. Supplied `places` answers need no network
access and can be applied without a proxy in the browser.

An answer's places without a usable position (no coordinate, or one that can't be read) are left
out, and a notice says how many; an answer with places but none usable counts as a failed
request, not as "not found". An address longer than the geocoder takes (400 characters for the
Locatieserver, 500 for the others) isn't sent cut short: its request fails. In the app the requests go out in the background, one at a time, and the layer is added when the
last answer lands. The status bar shows progress and **Cancel**; cancelling discards the run
without adding a layer, even if its current request later finishes. `layer.geocode` on the command
line waits for the answers.

```sh
rustgis-cli run --cmd 'layer.addData={"path":"stores.csv"}' --cmd 'layer.geocode={"table":"stores","address":["street","city","country"]}'
```

The addresses are sent to the geocoder's server, so the [AI Assistant](assistant.md) asks before
it geocodes, and plugins need the network permission.

## S3 buckets

An `s3://bucket/key` address works wherever data is added by its address: **Home › Add › Add Data ›
From URL…** and `layer.addUrl` (a cloud-optimised GeoTIFF is read in place by range, other files are
downloaded), PMTiles archives, `?data=` links, and STAC assets whose address is an `s3://` one.
A public bucket needs nothing more: it is read anonymously from AWS.

**Home › Add › Add Data › S3…** (`s3.connect`) connects to a private bucket, or to an S3-compatible
service (MinIO, Cloudflare R2, Wasabi, source.coop…) by its endpoint, and browses a bucket a
folder at a time (`s3.list`), adding an object with **Add**. A connection names its endpoint (AWS
when empty), its region and the buckets it serves: the ones it is given (`"buckets"`) and the ones
browsed through it this session. An `s3://` address of one of them is read through that
connection, signed (AWS Signature Version 4) with its keys; an address of any other bucket (a
project's, a link's) is read anonymously from AWS, never with your keys or from your endpoint.
Keys are typed, or, on the desktop and only when chosen, read from an AWS profile
(`"profile"`: `~/.aws/credentials` and `~/.aws/config`) or from the environment
(`"environment": true`: `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`,
`AWS_REGION`); without any, the connection is anonymous. **AWS, anonymous** in the dialog browses
with no keys at all.

```sh
rustgis-cli run --cmd 's3.connect={"name":"minio","endpoint":"https://minio.example.org","buckets":["my-bucket"],"accessKeyId":"…","secretAccessKey":"…"}' \
  --cmd 's3.list={"url":"s3://my-bucket/rasters/"}' --cmd 'layer.addUrl={"url":"s3://my-bucket/rasters/dem.tif"}'
```

Keys are kept until RustGIS closes unless you ask otherwise: never in a project (which keeps the
address), a command's result, a notice or the log. After **Connect**, **Save as Connection** keeps
its name, endpoint, region and credential source under **Data Sources › Connections**, never its
keys. Use **Saved connection** to pick it again.

On the desktop, a saved connection that uses typed keys can also keep them in your computer's
credential store (the macOS Keychain, the Windows Credential Manager, or the Secret Service):
tick **Remember on this computer** in the S3 dialog or the connection's form when you log in
(it is off every time; `connection.login` takes `"remember": true`). Both keys go to the store,
with the endpoint they are for, and the connection only gets a **Saved keys** mark; the
connection's definition, the preferences and projects never contain a key. Displaying that mark
does not read the store or guarantee it is available. In the next session, choosing, browsing,
testing or logging in to the connection can read its keys from the store with nothing typed
(the system may ask you to unlock it). Typed keys win over saved ones for that login.
Temporary credentials (with a session token) are never kept: they work for the session,
and keys saved earlier stay as they were. If the store is missing or refuses, the login still
works for this session and says the keys weren't remembered; without keys typed, a connection
with nothing available says so, and is not logged in. The web app has no system keychain; the
command line and Python use it only with `--use-saved-secrets` / `use_saved_secrets=True`.
**Forget Saved Keys** (`connection.forgetSavedKeys`; the connection's right-click menu or its
form) deletes them from the store and keeps the session's login. Removing a connection deletes
its saved keys first, and changing its endpoint or switching away from typed keys does too.
With a credential store available, these actions check it even when the **Saved keys** indication
is absent: the indication may be stale. If the store refuses deletion, the forget, removal or
edit doesn't happen. Without a store, a connection marked as holding saved keys cannot be
removed or have its endpoint changed. Renaming, grouping, buckets and region keep the keys,
and a copy of a connection has none. A profile, the environment or anonymous access never
read saved keys.

A saved connection's login reads only what is added through it: its listings, and the objects
**Add** takes from them, are kept as addresses that name that connection (and its endpoint), so a
project reads a bucket through the connection it was added with, never with another connection's
keys because the endpoint or bucket is the same. Opening a project (or importing a map or layer)
finds each bucket its S3 sources read. A single matching connection whose bucket list explicitly
includes that bucket logs in and opens the sources when its credentials are available.
Connections with an empty bucket list require an explicit choice, even when only one matches.
When only one such connection matches, the notice names it and asks you to confirm it in
**Choose S3 connection…**; it is not automatically authorized to read an unlisted bucket.
Missing or unavailable keys leave the sources blocked until you log in.
If several connections fit, **Choose S3 connection…** (`connection.resolveS3`) asks which one,
with no connection selected initially; nothing is read until you choose, and **Cancel** leaves
the sources blocked. If none fits, sources carrying saved-connection provenance stay unread
(an older project's bare `s3://` address is read as before). Addresses of plain `s3.connect`
sessions keep reading as they did.
A saved login belonging to another open session is not reused merely because its name matches:
each session logs in through its selected saved connection.
Resolving a bucket retries only that bucket's deferred sources, keeping layer styles and ordering.
An older source with an unknown endpoint stays separate from an explicitly AWS-backed source,
even when both name the same bucket and object. Saving and reopening while a choice is pending
preserves that distinction; choosing a connection for one resource does not choose for the other.
Raster reloads use the same authorized address and load their pixels in the background.
Keys go only to the endpoint they're for: a signed request that is redirected isn't followed, and
keys from a profile or the environment are used only with AWS or the profile's own
`endpoint_url`. In the web app, the bucket's CORS rules must allow the page's origin (and, for a
private bucket, the `Authorization` and `x-amz-*` headers). The [AI Assistant](assistant.md) asks
before it connects or lists; plugins can't connect, and need the network permission to list.

## Data Sources

The **Data Sources** pane (**View › Windows › Data Sources**, `ui.pane.sources`; **Home › Add ›
Data Sources** also opens it) gathers the places data comes from:

- **Favourites**: any source, right-clicked and added.
- **Recent**: the last 20 sources added (files, web addresses, tile layers, services, database
  tables, Zarr variables, STAC assets, sample data), newest first. A dataset added from a
  connection in the tree is listed under that connection's name rather than one made from its
  address. Only the name changes: the entry keeps the kind, address and options of the dataset
  as it was added, and the layer keeps its own name. Clicking it opens it as any Recent entry
  of that kind opens (a service entry reopens the service's layer list). A source is one entry
  per kind and address, so every dataset added through one service shares an entry, as do two
  connections with one address. The name is the connection's when the dataset was added:
  renaming a connection changes it only when something is added from it again. An add that
  fails, or is cancelled before it lands, isn't listed.
- **Folders**: the folders connected to the Browser (**Add Folder…**; desktop).
- **Connections**: named services, databases, STAC APIs and S3 endpoints, organised by optional
  groups. **New Connection ▾** opens a kind-specific form. Click a connection to open it: its
  datasets are listed under it, a level at a time as you open its folders, with **Refresh**
  (a connection's or a folder's right-click menu; **⟳** refreshes every connection) and an error
  with **Retry** (and **Log In…** for databases and keyed S3) when it can't be read. Click a
  dataset to select it (click again to clear it); double-click, or right-click › **Add**, adds
  one, and right-click › **Add Selected (N)** adds them all (**Clear Selection** empties the
  selection). Each dataset is added by one command, so each is one undo step. On a table, a
  service layer or a STAC collection, **Add With Options…** opens the database, service or STAC
  add dialog on it; on a connection, it opens the kind's add dialog. Right-click a connection to
  edit, test, duplicate, copy its address or remove it.
- **Library**: well-known public sources: OpenStreetMap tiles, EOX Sentinel-2 cloudless and
  NASA GIBS (WMTS), USGS imagery, topographic and hydrography maps (ArcGIS), GEBCO bathymetry
  (WMS), swisstopo's maps and imagery (geo.admin.ch) as WMTS tiles and as a WMS (most of its
  layers answer Identify), Earth Search and Planetary Computer (STAC), and the Natural Earth
  sample layers.

A click on a file, a web address, a tile layer or a sample adds it at once. The lists are kept
with the app's preferences, never in a project. They hold no secrets: a web address loses its
password and any key, token or signature in its query (`token=`, `sig=`, `X-Amz-*`…), and a
database is kept without its password. The commands `sources.list`, `sources.save`,
`sources.favourite` and `sources.remove` read and change the lists, for the command line and the
[AI Assistant](assistant.md). On the command line they last for the run.

### Network downloads

On desktop and the command line, web tiles, service documents and file downloads through
**From URL…** connect directly, including `layer.addUrl`. These requests do not use
`HTTP_PROXY`, `HTTPS_PROXY` or `ALL_PROXY`: a proxy could resolve a destination beyond the
app's IP checks. There is no proxy override for these requests. If your network requires a
proxy, download a file with a trusted external tool and open the local copy in RustGIS.
Live tile and service access requires direct connectivity. The web app uses the browser's
network settings. Browser GET and HEAD requests refuse redirects, including HTTP-to-HTTPS upgrades
and CDN hops, because the app cannot inspect their destinations. This includes **From URL…**
file downloads, fonts, service documents, tiles and sprites, and PMTiles/COG archive range
reads. Supply the final address; for a redirecting file download, download it with a trusted
external tool and open the local copy. The browser also controls DNS and local-network access;
see [tile request limits](maps.md#basemaps).

### HTTPS certificates

On desktop and the command line, RustGIS's HTTPS clients use platform certificate verification:
OGC services (WMS, WMTS, WFS and WCS), web tiles and downloads, ArcGIS portal sign-in's token
requests, the AI Assistant's model requests, the plugin registry, the uv download that makes
Python environments, and the GeoLibre runner installer. A private or enterprise CA (or a TLS
inspection proxy's) must be trusted by the operating system for the account running RustGIS:

- **Windows:** install the CA in the appropriate Windows Trusted Root Certification
  Authorities store. Windows evaluates the certificate chain and its trust/distrust policies.
- **macOS:** install and trust the CA for SSL in Keychain Access.
- **Linux and the BSDs:** install the CA in the system CA bundle, or set `SSL_CERT_FILE` to a PEM
  CA bundle and/or `SSL_CERT_DIR` to CA certificate directories before starting RustGIS. These
  variables select the trust sources instead of the default system locations (an empty
  `SSL_CERT_DIR` counts as unset).
  If no default system roots are found and neither variable is set, this client falls back to
  its bundled Mozilla roots, so public HTTPS still works in minimal containers, and says so
  once a session (in the status bar and the troubleshooting history). It does not
  add Mozilla roots to an existing CA store (even one it can't read) or an explicitly configured one.

`SSL_CERT_FILE` and `SSL_CERT_DIR` do **not** override Windows or macOS trust for this client.
Restart RustGIS after changing certificate trust. The service must still send a valid chain,
use a certificate matching its hostname, and have unexpired certificates; there is no option
to skip verification. Database connections (PostgreSQL/PostGIS, MySQL, SQL Server) have their
own TLS settings; PostGIS with verification on and no root certificate given still trusts only
the bundled roots.

Two steps use other trust:

- **ArcGIS sign-in** opens the portal's page in your browser, which checks the portal's
  certificate with its own trust (some browsers keep their own certificate store). If it doesn't
  trust the CA, sign-in stops before RustGIS gets a token.
- **uv**, making a plugin's Python environment, downloads Python and packages with its bundled
  Mozilla roots. To have it use the operating system's trust, set `UV_SYSTEM_CERTS=true` in the
  environment (or `system-certs = true` in a `uv.toml`) before starting RustGIS. A non-empty
  `SSL_CERT_FILE` or `SSL_CERT_DIR` replaces uv's roots on every platform.

The web app continues to use the browser's certificate verification and fetch behavior; its
server must also allow browser requests (CORS).

### Named connections

In the pane, **New Connection ▾** and **Edit…** let you set the name, group and address;
**Home › Add › Add Data › New Connection** opens the same form for a chosen kind. Leave a new
connection's name blank to derive it from the address. **Test** checks the form without saving
it: web-service, catalogue, STAC and S3 requests run in the background on both desktop and web;
database tests run synchronously on desktop. The web app doesn't offer database kinds for new
connections; saved database rows remain visible but cannot be opened or tested there. Earth
Search and Planetary Computer remain available under **Library**.

For a new web-service connection, enter its Basic password or header/query API key to test it before
saving. The test uses only the entered login, not a saved service login or an S3 session login
at the same address.
For an `http://` address, tick the confirmation before sending the login unencrypted. After a test
starts, RustGIS clears the password or key and the confirmation from the form; enter them again
if you want to save a session login. The test does not save the connection or retain its login.
Test status masks echoed login credentials, and the command log keeps response sizes rather than
response bodies.
For an existing service connection, save changes to its address or login definition before testing.

**From Service…** (including **Catalogue search (CSW)**), **Database…**, **STAC Search…** and
**S3…** can use saved connections directly. Service, database and S3 dialogs offer a **Saved
connection** picker; STAC includes saved connections in its existing API/catalog list.
Picking one keeps its identity, not just its address: two connections can point to the same
server with different names or users.

To keep an endpoint entered by hand, click **Save as Connection** after it connects: after
**List Tables** for a database, **Collections** or **Browse** for STAC, a catalogue search with
results, or **Connect** for a service or S3. Nothing is saved automatically. The new connection
is selected and the button disappears. An endpoint typed by hand can be saved even when another
connection has the same address; its default name is made unique. S3 uses the session name, so
choose a different name and connect again if that name is already saved. DuckDB files aren't
saved database connections. Within an ArcGIS directory, the button keeps the root directory.

Changing a form field clears pending or completed Test feedback; a late response from the
previous inputs is ignored. PostgreSQL accepts a bare `host=… dbname=… user=…` connection
string, but a blank address cannot be saved or tested.

The `connection.*` commands keep named, reusable endpoints independently of the project.
Each has a stable id (`c1`, `c2`, …), a case-insensitively unique name and an optional group.
Different names may use the same address. `connection.new`, `connection.edit`,
`connection.duplicate` and `connection.remove` manage them; commands that take `id` accept
either the id or the name. `connection.list` can filter by `kind` or `group`, and reports
availability, session login state and the commands to use to add data.
Names cannot match a stored id or the reserved `c` + digits id pattern (case-insensitive).
Loaded names that conflict get a numeric suffix; ids and endpoints stay unchanged. Clearing
a name with `connection.edit` chooses a unique default from the address.

```sh
rustgis-cli run \
  --cmd 'connection.new={"kind":"wms","address":"https://ows.terrestris.de/osm/service","name":"OSM WMS","group":"Public"}' \
  --cmd 'connection.test={"id":"OSM WMS"}' \
  --cmd 'connection.list={"group":"public"}'
```

Kinds are `postgres`, `mysql` (including MariaDB), `sqlserver`, `service` (detected on
connect), `wms`, `wmts`, `wfs`, `wcs`, `ogcapi`, `arcgis`, `csw`, `stac` and `s3`.
Database connections are used on the desktop only. S3 options are `region`, `buckets`
(comma-separated), `credentials` (`keys`, `profile`, `environment` or `none`) and `profile`.
`connection.test` lists the endpoint's tables, layers, records, collections or buckets.
It also accepts an unsaved definition (`kind`, `address`, `name`, `group`, `options`) instead
of `id`. With `probe: true`, it returns the secret-free address to fetch (`fetch: null` for
databases). For non-database kinds, pass that response text as `answer` with the same definition
to parse it without a network request. Database kinds ignore `answer` and test through their
driver. An anonymous S3 test needs at least one named bucket.

**Test** does not send service requests to XYZ URL templates with tile coordinates
(`{z}/{x}/{y}`, also `{zoom}` or `{-y}`). To add one to the map, use
**Home › Add › Basemap › Add Tile Layer…** (`map.addTiles`) instead.

`connection.browse` lists what a connection offers, a level at a time: database roots list the
schemas returned by their provider; opening a schema lists its spatial tables. Services list their
layers, catalogues their records with the services and files they link to, STAC APIs their
collections, items and assets (for a static STAC catalog, its catalogs, collections and items,
each item opened for its assets), and S3 connections their buckets, folders and files. Each child
has a `key`, a `name` and what it is (`what`); a child with a `node` is browsed by asking again with
that `node`; one with `add` names the command and parameters that add it (`service.add`,
`layer.addDatabase`, `stac.addAsset`, `layer.addUrl`). Database browsing adds no table-count cap
of its own. MySQL/MariaDB and SQL Server providers list at most 10,000 spatial-column rows per
catalog read, so larger catalogs can omit later tables or schemas; PostGIS has no such provider
limit.
Other levels list at most 500 children and say `truncated` when they leave some out. Like
`connection.test`, it reads the address itself on the desktop and the command line; with
`probe: true` it returns the address to fetch, and the app passes what it fetched back as `answer`
(and further `documents` when a service asks for them, as `next`).
Browsing never changes the project. When a saved database password turns out to be missing, or
to have been saved for another address, the connection loses its **Saved password** marker; the
stored entry itself is left for **Forget Saved Password**, an address edit or a removal to delete.

Addresses are saved without passwords or tokens. Database passwords entered through
`connection.login` are kept separately by connection ID for this session.
On desktop, **Remember on this computer** in the new, edit and login forms is always unchecked
initially. Checking it opts that submitted password into the operating system's password store;
**Test** never saves it. The command equivalent is
`connection.login {"id":"c1","password":"…","remember":true}`; `remember` defaults to false.
If the store is missing, unavailable or denies access, the login remains usable for the
session and a visible warning says it was not remembered (`passwordSaved:false` in the result).
The browser has no OS-store hook and remains session-only. The command line and Python also
remain session-only unless run with `--use-saved-secrets` (Python: `use_saved_secrets=True`).

The stored database password envelope is bound to the connection's ID, kind and cleaned address,
including its username. Renaming or regrouping preserves it. Changing the address or username,
or removing the connection, deletes the old stored password before committing the change; if
deletion fails, the definition remains unchanged. Invalid edits do not delete a password.
Duplicating a connection copies neither its stored password nor its session login.
**Saved password** marks a remembered database connection without reading the store just to
display it; it is separate from an S3 connection's **Saved keys** marker.
**Forget Saved Password** (`connection.forgetPassword {"id":"c1"}`) deletes the stored value,
but leaves an explicitly entered session password and already-open layers alone. Forgetting an
inactive connection never selects it or replaces another connection's active password.

Database layer origins remain address-only: projects contain neither database passwords nor saved
database connection IDs. On opening a project, one exact matching remembered database connection
may reconnect automatically.
Matching strips passwords and outer wrappers/whitespace, but does not infer that URI and
libpq forms, reordered parameters, host aliases or different usernames mean the same server.
Several eligible remembered matches open a chooser with no default; cancel leaves the layers
unavailable. Choose a connection explicitly, or use `connection.login {"id":"c2"}` to select
its existing session, environment or remembered password without exposing it. Selection lasts
for this session, not in the project. `connection.list` reports secret-free `loginRequests` with
the reason, database label, candidate IDs and affected-layer count; ambiguous layers are not
attributed to the first saved row. `loggedIn` means this ID has a session password, an
environment password selected for it, or an already-loaded remembered password available; it
does not certify successful server authentication.

Explicit command, address and session passwords take precedence over saved credentials. On the
command line and in Python with `--use-saved-secrets` / `use_saved_secrets=True`, an eligible
remembered connection's `PGPASSWORD`, `MYSQL_PWD` or `SQLCMDPASSWORD` (according to the database
kind) wins over the keychain password without reading the store; the desktop app always uses the
saved password. Project opening may use it only through the same exact-address
remembered-connection selection; it never newly authorizes an unrelated address, password-file
credentials or Windows identity. A positively refused saved or environment password is not
retried automatically for the rest of the session, and no other saved password is tried
automatically. Its old OS-store value is retained.
The fresh login form has an empty password and Remember unchecked: an unchecked replacement is
session-only and does not overwrite or delete the old value; checking Remember and successfully
saving replaces it. A password the server refuses is not kept as this session's, and is not sent
to the database's other layers. A refused typed login restores the previous active password,
selection and activation; with no previous password, the attempted value is removed.
If a password reused from this ID's session cache is refused, that cached value, its owned
driver entry and activation are cleared, and a password is requested. An identical saved value
is also suppressed for this session; a different saved value is not marked refused by this
attempt. The OS-store entry is retained in either case.
A transport error or missing table is not a wrong-password prompt. Use Forget to delete a
previously saved password.

A database login reopens waiting layers at the same password-free address;
`connection.list` reports them as `waiting`, and `connection.login` returns `reopened` and
`failed` counts. Adding, editing or removing a saved connection, or adding a layer, resolves
only the databases that change: an unrelated database that is down is not connected to again.
`connection.test` never persists a password. For a named connection, if the
server positively refuses a password typed for that Test, the previous per-ID session credential,
selection and active address password are restored (or the attempted credential is removed if
none existed).
A failure after authentication, such as a table-listing error, keeps the new session login.
S3 `accessKeyId` and `secretAccessKey` likewise stay session-only unless a typed-key login opts
into remembering them; a nonblank `sessionToken` always keeps the complete temporary credential
set session-only, leaving previously saved keys unchanged (see [S3 buckets](#s3-buckets)).
With login parameters, an S3 `connection.test` logs in first, never remembers keys, and a failed
listing does not undo that session login.
S3 profiles and environment credentials remain subject to the existing endpoint restrictions;
editing an S3 name, endpoint or options disconnects its session login. S3 shares the name-based
session registry used by the S3 dialog and `s3.connect`, so editing or removing a saved
connection also affects a session entry of the same name created there.
Renaming clears runtime entries under both the old and new names, so the renamed
connection cannot inherit an unrelated login at its destination name.
Plugins may list connections but cannot manage, log in to, log out of, forget passwords, logins
or S3 keys for, choose an S3 connection for, test or browse them. The in-app
assistant asks for fresh approval each time for those operations, including Forget, S3 source
selection and scripted calls; external assistants using MCP cannot run them. Even `probe: true`
needs approval: connection operations cannot be approved for the whole conversation.

Desktop and web apps save connections with their preferences (`connections` in desktop
`prefs.json`, `rustgis.connections` in web localStorage). The first load imports saved services,
databases other than DuckDB files, STAC APIs and remembered S3 connections once, without
changing the old lists. Legacy saved endpoint lists are no longer shown in the pane or the
S3 dialog; use the imported named connections instead. Testing S3 can register or replace the
session endpoint and credentials under the form's connection name, including a same-name
session created in the S3 dialog. Cancel does not disconnect that login or restore the
previous one. CLI and Python connections last only for the run.

#### Web-service logins

A WMS, WMTS, WFS, WCS, OGC API, ArcGIS REST, CSW or STAC connection can log in: its form's
**Login** picks **None**, **Basic authentication** (a user name and a password), **API key in a
header** (the header's name, such as `X-API-Key`, and the key) or **API key in the query** (the
parameter's name, such as `api_key`, and the key). The connection keeps only the method, the user
name and the header or parameter name, never the password or key.
The password or key goes to `connection.login {"id":"c1","password":"…"}` (basic) or
`{"id":"c1","apiKey":"…"}` and lasts only for this session by default.
On desktop, **Remember on this computer** is unchecked initially. Checking it
(`"remember": true` in `connection.login`) saves the Basic password, header key or query key in
the operating system's credential store: macOS Keychain, Windows Credential Manager or Secret
Service. **Test** never remembers a login. If the store is absent, unavailable or refuses access,
the entered login still works for this session and a warning says it was not remembered.
An unchecked replacement stays session-only and leaves any previously saved login unchanged.
The web app has no OS-store hook and remains session-only.

A remembered login is read only when needed, for the same connection ID, exact origin (scheme,
host and port), method, user and header or query parameter name. It is never borrowed by another
connection. A missing or unavailable saved login leaves the connection waiting for an entered
login; it does not prevent session-only sign-in. The credential envelope is versioned; unknown
versions are refused without rewriting the saved value. A failed store read is not retried merely
by opening the source again: choose **Log In** or run `connection.login` explicitly to retry.
Deleting or replacing a saved login, by **Remember**, **Forget Saved Login**, or an edit or
removal, first checks whose it is. Connection IDs are numbered per computer, so another
connection, or a newer RustGIS, can have saved a login under the same ID. A login saved for this
connection's origin, method, user and header or parameter name is replaced or deleted, and so is
a damaged value that no version can read. A login saved for another connection or by a newer
RustGIS is left unchanged: **Remember** then keeps the entered login for the session only, with a
warning, and **Forget Saved Login** says the entry was left.
Editing the origin, method, user or header or
parameter name ends the login and deletes the saved one (a connection's kind doesn't change:
make a new one); renaming or changing its path at the same origin keeps it. Removing a connection
also deletes its saved login; duplicating it copies neither its session nor its saved login.
If the store refuses the deletion for a connection marked remembered, the edit or removal does
not happen. A connection that isn't marked can still have a login saved by another session, so a
store is checked for it too; if that fails, the change goes on with a warning that a saved login
may remain. This check can ask to unlock the credential store even for a connection that isn't
marked.

**Forget Saved Login** (`connection.forgetLogin {"id":"c1"}`) deletes only the stored login,
leaving the session's login usable. **Log Out** (`connection.logout`) ends the session login,
but keeps the stored one. Logout prevents automatic reuse for the rest of that session until
an explicit login, including `connection.login {"id":"c1"}` to use the saved login again.

An API key in the query suits services such as Planet's basemaps, whose address is
`https://api.planet.com/basemaps/v1/mosaics/wmts?api_key=…`: save the connection as
`connection.new {"kind":"wmts","address":"https://api.planet.com/basemaps/v1/mosaics/wmts","auth":"query","keyName":"api_key"}`,
then log in with the key. The parameter, percent-encoded, is added to each request to the
connection's origin (capabilities, tiles, feature pages and refreshes), replacing a parameter of
the same name (whatever its case or escaping) that an address already holds, so it is sent once.
The key is in no address that is saved, shown or recorded: a key typed into the address is left
out of the saved connection. When the connection does not already use a query login, the result
of `connection.new` or `connection.edit` offers up to four plain parameter names as
`strippedParameters`, never their values; the notice explains how to use them for a query login.
A parameter that a query login names is also
left out of any other address that carries it, saved or recorded, for as long as RustGIS runs.
A run holds up to 256 different parameter names; a connection or login with yet another one is
refused (restart RustGIS, or reuse a name) rather than sent without that protection.

Creating a login requires secure random numbers: the operating system's generator on desktop,
or the browser's `crypto.getRandomValues` in the web app. If that source is missing or fails,
RustGIS refuses the login rather than using a predictable fallback. A failed replacement leaves
the previous active login usable.

**ArcGIS token** (`"auth": "arcgis"`, on any web-service kind: an ArcGIS-hosted WMS or WMTS needs it
too) logs in to ArcGIS Server or ArcGIS Enterprise with a token, sent as each request's one `token`
parameter (a parameter of that name in an address is replaced, whatever its case or escaping, and
an explicit connection token outranks a portal sign-in's, which is then neither sent nor renewed for
that request). The connection keeps the method and an optional user; the token is given in one of
two ways.

- **Paste token**: `connection.login {"id":"c1","token":"…","expiresAt":1893456000000}`. The token is
  ASCII letters, digits and `- _ . ~ + / =`, at most 4096 bytes. `expiresAt` is optional, in Unix
  milliseconds and in the future; without it the token is used until the server refuses it, and
  nothing is renewed or guessed. **Test** with a pasted token uses a temporary login that ends with
  the test: the saved connection, its session login and its saved login are not used or replaced.
- **Username and password**: `connection.generateToken {"id":"c1","tokenEndpoint":"https://host/arcgis/tokens/generateToken","password":"…"}`
  asks the server's `generateToken` for a token for the connection's user (`client=requestip`, 60
  minutes) and answers `{"id","pending":true}`. The endpoint must be an `https://` address at the
  connection's own origin, with no user information, query or fragment, and is never taken from a
  service's answer; a federated portal's sign-in uses the portal sign-in instead. The request runs
  in the background on the desktop and in the web app (30 seconds at most, answers up to 1 MiB, no
  redirects, cookies or other login). `connection.tokenStatus {"id":"c1"}` answers
  `{"pending":true}` until it lands, then `connection.login`'s result with `"pending":false`;
  `connection.cancelToken {"id":"c1"}` stops it, leaving any earlier login. Only the caller that
  started it can ask or cancel, and a replaced project, a changed login (user, method or origin), a
  logout, a removal, a pasted token, a closed form, or logging in or out of another connection ends
  it. Failures say a fixed sentence, never the
  server's text. The password is in that one request and is kept nowhere: not in the form once sent,
  the connection, the session, the saved login, History, recordings or notices.
  **Test** requires a saved connection, generates through the same commands with Remember off, and
  leaves the resulting session-only token logged in before testing the service. Cancelling a native
  request never waits for the worker; one blocking network operation can finish at its 30-second
  deadline before the worker notices cancellation.

With **Remember on this computer** (`"remember": true`, on `connection.login` or
`connection.generateToken`) the token and its expiry, never a password, go to the credential store as
the same versioned login described above. A saved or session token that has expired is not used: the
login is revoked, cached authenticated responses are not read, nothing falls back to anonymous
access or a portal sign-in, and the connection asks to log in again (`connection.login` with a new
token, or `connection.generateToken`). An expired saved token is neither deleted nor rewritten until
it is forgotten or replaced, and is not read again that session. A token is never renewed
automatically, since that would need the password. From the command line and Python, poll
`connection.tokenStatus` until `pending` is false.

A login is sent only to the connection's origin: the same scheme, host and port, for its
capabilities, listings, tiles, feature pages and refreshes. On the desktop a redirect within that
origin keeps the login, and a redirect to another origin is followed without it, never getting
another connection's login either. In the web app a request carrying a login isn't redirected at
all, since the browser can't take the login off on the way: a service that redirects (to
`https://` or to another path) fails there, so save the connection with the address it ends at.
Service documents cannot supply a login binding. While resolving their links, RustGIS inherits
only the login used to read the document, and only at that same origin. A runtime wrapper in a
document link contributes its ordinary URL, never its login.
Relative links, including parent paths, resolve against the document's address without rewriting
template placeholders or query values, including proxy parameters containing `https://` URLs.
KVP requests replace capabilities operation parameters, keeping unrelated parameters such as
MapServer's `map=`.
A same-origin link whose path or query holds a space, backslash or control character keeps the
login: tabs, line breaks and trailing spaces are dropped, a backslash in the path becomes `/` and
other spaces, control characters and backslashes are percent-encoded (a space as `%20`).
If a service supplies a same-origin link too long to carry its login, the request is refused
instead of being fetched anonymously. Addresses that cannot be rebound to a login are left
unchanged and reported.
A plain `http://` address sends the login unencrypted, so the form asks for confirmation
and the command needs `"allowInsecure": true`. Error messages, notices, the preferences,
projects, History and what the [AI Assistant](assistant.md) sees never hold the password or key.
On the desktop, a failed HEAD request that carried a web-service login records the HTTP status
without response headers, including after logout or a redirect to another origin: a server could
echo credentials in those headers. HEAD requests that never carried a login keep their selected
diagnostic headers on refusal.
Runtime login bindings are also removed from recorded command parameters, including addresses
embedded in text, when saving, recovering or exporting them. The live session keeps its originals.

A project's layers use the connection they record; layers recording none use an explicit session
choice or the only saved connection for their origin. Different connections and sessions can use
different logins at the same origin without sharing credentials or authenticated cached responses.
When several saved connections match the address, RustGIS asks which one to use (**Choose
Connection…**, `connection.login {id}` for the one chosen) and never picks one itself.
When an origin has exactly one saved connection and it has a login method, an address at that
origin is read through that connection, so it needs that login: log in, or save a public
connection for the address too.
A layer that has no login yet asks for one, and a layer whose recorded connection isn't on this computer offers
to make one for its address. `connection.list` reports these as `loginRequests` with the `origin`.
Logging in retries waiting remote rasters without changing their style. A dated layer also asks
for the login covering dates at its recorded connection's origin, even when it reopens on a date
hosted elsewhere. Those foreign-origin dates stay anonymous.
CLI and Python remain session-only unless `--use-saved-secrets` / `use_saved_secrets=True`
opts into remembered logins, as for database passwords and S3 keys. An active login still ends
with its session; only an explicitly remembered one can be loaded again.
Earthdata Login and ArcGIS OAuth sign-in and tokens keep their own
[sign-in rules](web-data.md); these web-service login controls do not change them.
Unbound fetches never adopt another session's service login.
Layers added by plugins stay anonymous, including after saving and reopening a project or adding
a saved connection later. To use a connection's login, add the layer yourself through that connection.
A plugin cannot set a same-origin legend on a layer tied to a saved connection, even when that
connection is currently public. If it changes the legends of an unconnected tile layer, that
layer stays anonymous too.
Refreshing a plugin-added service group also stays anonymous: its capabilities, linked documents,
feature downloads and newly discovered collections do not borrow a saved login. The group keeps
that provenance after saving and reopening, even if a refresh temporarily leaves it empty.

## Large files

GeoPackage, GeoParquet, FlatGeobuf and (on the desktop) Shapefile files with more than 100,000
features are read in place rather than loaded whole, avoiding an in-memory copy of the entire
dataset. The layer reads features in view in the background as the map moves, using spatial
indexes: the GeoPackage R-tree, the FlatGeobuf packed R-tree, GeoParquet row-group bounding
boxes, or a cached Shapefile R-tree built from the shapes' bounding boxes. When a
local file's view holds more than 250,000 features (100,000 in the web app), the layer draws an
evenly spread sample of them and its tooltip in Layers says so: zoom in to see them all. Remote files have lower limits (see
[Data formats](data-formats.md)). Smaller files load
whole; `layer.addData` with `"stream": true` or `false` overrides the choice.

On the desktop, **Load All Features** from the layer's menu in Layers reads its features in the
background, with **Cancel** in the status bar; the layer stays streamed until they are all read.

On the desktop, opening a large Shapefile for the first time scans its record headers to build
a disposable SQLite index outside the dataset directory. Reopening an unchanged file reuses
that index; its canonical path, size and modification time identify the cached generation.
The record directory and R-tree stay on disk, with an 8 MiB SQLite page-cache target per open
index and memory mapping disabled. Query results retain file order and the same sampling
rules. Attributes and the `.prj` are still read from the source files.

The indexes are kept in the `shp-index` folder of the RustGIS cache directory:
`~/Library/Caches/RustGIS/shp-index` on macOS, `~/.cache/rustgis/shp-index` on Linux
(`$XDG_CACHE_HOME`), `%LOCALAPPDATA%\RustGIS\cache\shp-index` on Windows, or under
`RUSTGIS_CACHE_DIR` when it is set. Set
`RUSTGIS_SHP_CACHE` to choose another directory, `RUSTGIS_SHP_CACHE_MIB` to choose the
retained index budget (2048 MiB by default, clamped to 64–65536 MiB), and
`RUSTGIS_SHP_CACHE_DAYS` to choose how long an unused index is kept (30 days by default,
clamped to 1–3650). The cache is disposable: each index is rebuilt the next time its Shapefile
is opened. **Empty Index Cache** (`cache.emptyIndexes`, from command search, a script or the
CLI) removes every index not in use, in the cache folder and its fallback, and reports how many
indexes and bytes it freed. A notice names what it kept: the indexes of open layers, here or in
another RustGIS window, and files it couldn't remove, without a readable lease or refused by the
system; and a folder it couldn't empty, without stopping at it. With RustGIS closed, the folder
can also be deleted by hand.

Idle indexes are evicted in least-recently-used order. Open readers and builders hold
operating-system leases, so another instance cannot evict their indexes; active files can
temporarily exceed the budget. An index larger than the budget is session-only. Building an
index, even one that fails, reclaims abandoned builds and temporary indexes, evicts idle ones
past the budget, and drops idle indexes unused for longer than the limit (of a file deleted or
moved, for example). A build that is kept also drops the idle indexes of the same Shapefile
from before it changed. Reopening a cached index leaves the cache as it is, and releasing one
removes only its own temporary index. Otherwise a release sweeps the folder only while indexes
in use keep the cache over budget and releasing them can bring it back within budget. A
corrupt cache is rebuilt, and an unavailable cache
directory falls back to the system temporary directory (a per-user directory on Unix).
New Unix cache directories are created with mode `0700`. Existing roots must belong to
the effective user, forbid group/other writes, and have trusted parents; symlink roots
are rejected. These checks also apply to configured locations. Unsafe locations fall
back rather than changing their permissions. If neither location works, the
Shapefile remains readable by scanning record headers without retaining a full in-memory
index. Deleting idle cache files does not modify the dataset.

On the desktop, a GeoJSON FeatureCollection larger than 32 MB is read the same way, with no size
limit: opening reads the file once, keeping each feature's place in it and its bounding box (a
file over 256 MB through a buffer, so it is never held whole), and each view parses just the
features it shows. A single feature, a bare geometry, TopoJSON or CityJSON is loaded whole.

A streamed layer works like any other for symbology, labels, pop-ups, selection and the
attribute table (which lists the features read for the view). On the desktop, **Export Features**
(`share.exportLayer`) of a GeoPackage, FlatGeobuf, GeoParquet or Shapefile read in place to
GeoJSON, CSV, FlatGeobuf, GeoPackage or GeoParquet reads the source a chunk at a time, putting
each chunk in the output's coordinate system and writing it before reading the next: the layer
is never held whole. What stays in memory is a chunk and what the source and the output keep: a
GeoParquet source one row group at a time (memory grows with its largest row group), a FlatGeobuf
output a small index entry for each feature. A source read by its web address is exported whole,
as below.
Geoprocessing tools, other exports and
**Load All Features** read the whole source. A GeoPackage, FlatGeobuf or GeoParquet file, or a
Shapefile read in place, is read a chunk at a time, with no limit on its feature count, up to
4 GiB of features in memory (1 GiB in the web app). Other sources (a large GeoJSON read in
place, LAS/LAZ, databases, a file by its web address) are read at once, up to 2 million features
(500,000 in the web app; a source that doesn't report its feature count must have fewer than
that, since a read that reaches the limit is refused). A source past its limit, memory or
count, is refused rather than processed as a sample, and these limits also apply to a file added
without streaming (`layer.addData` with `"stream": false`) or given to a geoprocessing tool as a
file path (a Shapefile so added goes through the Shapefile reader and its file-size limit). An
incomplete read is an error, not a successful sample. In the web app, a GeoParquet or FlatGeobuf
file read in place by its web address isn't read whole: download it to load it. A tool given a
streamed layer with a selection uses the selected features read for the view.

Edits are refused, since the features are read again as the view moves: right-click the
layer in Layers and choose **Load All Features** (`layer.loadAll`) to load it into memory and
edit it. Projects save the layer's file path, not its features, and read the file again when they
open; a browser upload is kept in the project.

LiDAR (LAS/LAZ, including COPC) keeps at most 2 million points on the desktop or 500,000 in the
browser as a display sample, evenly spread across the points read. LAZ (including COPC) reads
only the first 400 million points on the desktop or 40 million in the browser; later points
are not read. The layer retains the file's original point count; zooming does not recover
points omitted from the sample. Whole-layer tools, exports and **Load All Features** refuse
clouds over 2 million points on the desktop or 500,000 in the browser. Use a smaller extract
to process all its points.

| File | Features | A view read |
| --- | --- | --- |
| GeoParquet, 0.1° grid (50 MB) | 1.6 million polygons | 183,000 in 0.15 s |
| FlatGeobuf, the same grid (335 MB) | 1.6 million polygons | 183,000 in 0.11 s |
| GeoPackage, the same grid (344 MB) | 1.6 million polygons | 183,000 in 0.18 s |
| GeoParquet, global grid (190 MB) | 6.5 million polygons | a sample of 136,000 in 0.27 s |

### Streamed view memory

Interaction windows share a retained-memory budget across all source layers in a map,
including hidden layers: 128 MiB on the desktop and 48 MiB in the browser. The budget
counts allocated row capacity, attribute strings, nested geometry buffers, projected
geometry, extent arrays and optional display geometry. Complex records can therefore
reduce the interaction sample even below the existing feature-count limit. Source ids,
attributes and file order are retained; the sample is marked as incomplete. An individual
feature that cannot fit produces a layer error instead of silently disappearing.

Projection is prepared with the window on the source worker. Optional thinned geometry
uses the remaining budget, up to 64 MiB per layer; when it cannot fit, drawing uses the
original projected geometry. Explicitly loaded layers retain their full source data and
limit only their optional display caches.

This is a budget for installed interaction caches, not a process-RSS or GPU-memory cap.
Provider decoding scratch, in-flight reads, shared snapshots and explicit full reads are
outside it. The generic provider adapter probes a small sample and trims the returned
window; it does not bound the provider's transient decoding allocations. Blocking map
exports retain their original row limits and reread instead of reusing a budget-reduced
interaction window. Full-source loading, analysis, editing, data exports and project
serialization continue to use the original source data and existing read limits.

## Rasters

<figure markdown="span">
  ![A shaded-relief elevation raster of the Sierra Nevada with the Style tab open](https://assets.rustgis.app/images/raster.webp)
  <figcaption>An elevation raster of the Sierra Nevada with shaded-relief symbology.</figcaption>
</figure>

Add a GeoTIFF or an ASCII grid with **Add Data**, by dropping it on the window, or in the web app
by picking it. Local ASCII grids can be up to 1 GiB; ASCII-grid uploads are limited to 256 MiB in the web app.
A TIFF or PNG without coordinates opens as a scan to place with **Georeference**; see
[Supported Data Formats](data-formats.md). An ASCII grid takes its coordinate system from a `.prj`
file next to it (as a Shapefile does); otherwise, and always for uploads, it must be in
longitude/latitude degrees, and is read as WGS 84 with a notice. On the desktop, a GDAL `.aux.xml` file next to a raster (`dem.tif.aux.xml`
or `dem.aux.xml`) names its bands from their descriptions, gives the no-data value when the file
has none, and supplies the band statistics when only an overview of the raster is read. Its
category names (GDAL's `SetCategoryNames`, one per value) label the classes in the legend of a
Colormap or Unique Values layer; a value without a name shows the value itself. Without category
names, the name column of a raster attribute table in the sidecar labels them the same way (a table
of value ranges isn't read, and is said). The band
descriptions GDAL and rasterio write inside a GeoTIFF (its `GDAL_METADATA` tag) name its bands
too, a sidecar's taking precedence; a table's names don't replace class names the GeoTIFF itself
gives, or the sidecar's category names, and a table left unread that way is said.
Exporting a raster opened from an overview writes that overview; the results of adding and of
exporting it (`layer.addData`, `share.exportRaster`) give the full size as `overviewOf`, so scripts
and the MCP server see it too. A GeoTIFF read in place that is added before its overview is in
(the desktop app, the browser) answers `"reading": true`, sized by its header when that is in, and
with `null` for its size and bands until then.
`share.exportRaster` writes an untiled GeoTIFF, in strips of rows, with an 8-bit grey band (a
hillshade, say) as bytes and another single band as 32-bit floats; with `"cog": true` it writes a
cloud-optimised one: 512-pixel tiles, overviews (each half the size of the last, down to one tile),
and every directory ahead of the tiles, so a reader fetches the header once and then only the tiles
it needs. Both compress with Deflate after a predictor: the floating-point one for decimal values,
differences along the row for whole numbers, none for classes. Overviews are averaged, except for a
single band of classes (a palette raster's, a band with class names, or a band written as 8- or
16-bit whole numbers with a few distinct values), whose overviews take a pixel so that no class is
invented; colour and grey-with-alpha files are always averaged. A palette keeps its colour table,
though not the table's transparency, which a TIFF colour table can't hold (GDAL drops it too).
Either file is classic TIFF when it would fit in 4 GiB even uncompressed and BigTIFF when it might
not.
Exported to a file, a cloud-optimised one is written tile by tile, never held whole in memory (the
browser builds it in memory to download it).
GeoTIFFs may be regular or cloud-optimised: grey, RGB(A), palette (coloured by the file's colour
table, as land cover such as the NLCD is) or multiband, of any pixel type, tiled or striped, and
compressed with Deflate, LZW, PackBits, JPEG, Zstandard or LERC (alone, or wrapped in Deflate or
Zstandard, as GDAL writes it; samples LERC marks invalid are no-data, and an alpha GDAL kept as
LERC's mask is put back). JPEG-compressed YCbCr images are shown
as RGB; other YCbCr and CMYK images aren't read. A rotated or skewed image is resampled (nearest pixel)
to a north-up grid as it is read, and a notice gives the grid's size and cell, and says when the cell had
to be coarser than the file's original full-resolution cells to stay within the sample limit,
including when an overview or every few pixels of the image were read. A GeoTIFF with no coordinate system whose
coordinates fit in degrees is read as WGS 84 with a notice, as is one whose own projection can't be read
(the notice says why); metres of a system that can't be read are refused. The coordinate system is an EPSG code,
or the file's own projection described in its GeoKeys (Albers, Lambert conformal conic, transverse
Mercator, Mercator, Lambert azimuthal, stereographic, equirectangular, sinusoidal and others), which an
exported GeoTIFF keeps. A raster too large to hold in memory (more than 120 million samples, or
more on a computer with more memory, see Environments in the analysis guide; 32 million in the
web app) opens from its largest overview that fits; one without overviews opens as
an overview made as it is read, a strip or tile at a time (every second, fourth… pixel each way),
so reading it takes as long as decoding the whole file once. A cloud-optimised GeoTIFF added by its
web address is read in place instead, a tile at a time (see [Files on the
web](data-formats.md#files-on-the-web)).

The web app opens a picked or dropped file of up to 256 MB whole. A larger GeoTIFF is read a part
at a time instead: its header, then only the tiles of the overview it opens from. A
cloud-optimised file opens however large it is, as long as those parts fit in 256 MB (a 436 MB
EnMAP hyperspectral scene reads about 20 MB, the 1.6 GB NLCD land cover about 14 MB).
The parts read are what a saved project keeps. A file that isn't cloud-optimised (in strips, or
without overviews) needs most of itself, and past 256 MB the web app says to open it in the
desktop app.

With a raster layer selected in Layers, the **Style** and **Data** tabs show its groups:

| Tab › Group | Controls |
| --- | --- |
| Style › Visibility Range, Effects | Scale range, transparency |
| Style › Rendering | **Symbology**: Stretch, RGB, Classify, Unique Values, Colormap, Shaded Relief; **Stretch Type**: None, Minimum Maximum, Custom Min/Max (your own bounds, one pair per colour channel on an RGB layer), Percent Clip, Standard Deviation, Histogram Equalize; **DRA** (stretch the pixels in view); **Resampling** (nearest or bilinear); **Band Combination** (natural colour, colour infrared, or any bands) |
| Style › Enhancement | Brightness, contrast, gamma |
| Data | Raster information (size, bands, statistics, coordinate system) and Export Raster (GeoTIFF) |

**See-through cells** of an image with an alpha band (RGBA, or grey and alpha) are left out of
its colour bands' statistics. These are the cells the map draws fully see-through: an alpha of no
value, or one under half a 256th of opaque. That is alpha 0 in an 8-bit image, 128 or less of
65535 in a 16-bit one, and under about 0.002 of 1 in a Float32 one. Leaving them out changes
Raster information's counts and ranges, and the stretches taken from them: Minimum Maximum,
Percent Clip, Standard Deviation, Histogram Equalize, a style mask's, and a cloud-optimised
GeoTIFF's own. So a value those cells hold, such as −3.4e38 in a Float32 image, doesn't set the
stretch, and an opaque black cell still counts. The alpha band's own statistics count every
alpha with a value, see-through ones included.
**Change Detection** gives no-data where either image is see-through.

**Cast shadows**, on a Shaded Relief layer (`raster.symbology` `"shadow"`, 0 to 1), darkens the
slopes the terrain hides from the sun as well as those facing away from it: a valley behind a ridge
is in shadow, with a soft edge. Only the direct light is darkened: a slope in full shadow keeps the
relief's ambient light. 0, the default, draws the relief as before. The shadows are worked out once
for the layer's sun, band and Z factor, off the map's frame on the desktop: the map and the Layout
view draw without them for the moment that takes (under a second for a few million cells on a
multi-core computer, several seconds on one thread; a sun under about 5° traces much farther, some
seconds on a multi-core computer and twenty or so on one thread), then with them. An export
waits for them; what the window draws at once (Copy Map or Copy Layout to the clipboard, printing,
the presentation and report previews, an agent's snapshot) doesn't, and says the layers it drew
without them.
The strength is drawn, not worked out, so changing it is immediate. The browser works them out for
rasters of at most 500,000 cells, and a cloud-optimised GeoTIFF read in place has none unless its
whole image is held: either way the layer is drawn without them, and a notice says so. Terrain
outside the raster casts nothing on it, and a grid spanning 0 to 360° casts nothing across its
seam.

**Sky visibility**, on a Shaded Relief layer (`raster.symbology` `"sky"`, 0 to 1), dims the
relief's ambient light where the terrain around a cell hides part of the sky: valley floors, pits
and the foot of a cliff get less of it than a plain or a ridge, whatever the sun. Each cell's sky is
the share of the half sphere above its horizon, looked for in 16 directions up to 100 cells away
(past 32 cells, in steps of a sixteenth of the distance, so a peak narrower than that far away can
be missed); 1 scales the ambient light by it, 0, the default, draws the relief as before, and the
direct light (and its cast shadows) is left as it is. It is worked out once for the layer's band,
Z factor and mask, not for the sun, so moving the sun doesn't work it out again: off the map's
frame on the desktop, about a second for two million cells on a multi-core computer, nine or so on
one thread. It is drawn, waited for by
exports, and said when it can't be drawn (in the browser over 500,000 cells, or a cloud-optimised
GeoTIFF read in place) as cast shadows are. Terrain outside the raster, or farther than 100 cells,
hides none of its sky.

**Fill light**, on a Shaded Relief layer (`raster.symbology` `"fill"`, 0 to 1), opens the slopes
turned away from the sun without lifting the rest of the map: a second, weaker light from the
opposite side (the sun's azimuth plus 180°), 55° up, adds `fill × max(0, L − sin 55°)` of the
colour to each cell, where `L` is how squarely the cell faces that light (the same slopes as the
hillshade). Flat ground gets exactly `sin 55°` of it and so gains nothing, nor does a slope turned
to the sun; the more a slope turns toward the fill, the more it gains, up to 0.18 of its colour
with a fill of 1. Nothing shadows it, and it is drawn at once (nothing to work out). 0, the
default, draws the relief as before, and it is saved with the project only when it isn't 0.

**Rock on steep slopes**, on a Shaded Relief layer (`raster.symbology`
`"rock": {"from": 25, "to": 40, "color": "#B2ACA4"}`), tints steep ground toward a rock colour,
so cliffs and scarps stand out from the gentle slopes of the same height. Below **From** degrees
of slope the ramp's colour is kept, above **To** the rock colour is used, and between them the two
are blended on a smooth step. The lights (hillshade, cast shadows, sky visibility and fill light)
then shade that colour as they shade the ramp. The slope is the ground's own, worked out from the
same 3 × 3 window as the hillshade but without the Z factor, so 35° means 35° whatever the
exaggeration (metres at the cell's latitude for a geographic raster). A 30 m DEM averages a cliff
with its foot, so its walls come out gentler than they are: 25° to 40° suits it, and a finer DEM
can start higher. Missing keys keep the current rock's (or those defaults), and `"rock": false`
removes it; it is off by default and saved with the project only when set.

**Ambient** and **Balanced light**, on a Shaded Relief layer (`raster.symbology` `"ambient"`, 0 to
1, and `"balanced"`, true or false), set how the sun's light shades the colour. Each cell is drawn
at `ambient × sky + (1 − ambient) × h × direct` of its colour, where `h` is how squarely it faces
the sun (0 to 1), `sky` the share of the sky it sees (1 without sky visibility) and `direct` what
cast shadows leave of the sun (1 without them); the fill light is added to that. So `ambient` is the
share a slope turned fully from the sun keeps: lower draws deeper shadows. With the balanced light,
`h` is divided by its value on flat ground (`sin altitude`), and the sum is held to at most 1.3
before the fill light is added: flat ground that sees the whole sky is drawn in the ramp's own colour, the colour the legend
shows, slopes turned to the sun brighten (each channel clamped to 255) and slopes turned away fall
toward `ambient`. A sun within about 0.1° of the horizon lights no flat ground, so there the
balanced light is drawn as the plain one, with its own ambient; below about 1° the picture changes
sharply with small changes of altitude, so keep the sun higher with the balanced light. A layout's colour bar shows the
colour of flat open ground, so with the balanced light it is the ramp itself; where sky visibility
hides part of the sky, the ground is drawn darker than the bar. Left unset, `ambient` is
0.25, as the relief was always drawn, or 0.5 with the balanced light, which keeps about the same
contrast; `"ambient": null` goes back to that default. A Z factor of 1.5 to 2 suits the balanced
light on 30 m data. Both are saved with the project only when set.

With **Resampling** set to bilinear, a Shaded Relief layer's light (its hillshade, cast shadows,
sky visibility and fill light) and its rock tint are blended between the cells' centres, as its
values are, so a cell drawn over many pixels shades smoothly instead of in squares; nearest keeps
one light per cell, as before. A cell with no data, or masked, is left out of the blend, so it
doesn't darken the relief next to it.

**Masking**, at the foot of the Symbology pane (`raster.mask`), hides pixel values from view without
changing the data: values to draw as no data (a background of 0 or −9999 the file doesn't flag, QA
flags, land cover classes such as `0, 254, 255`), and values outside, or inside, a range. The
stretch, class breaks and legend then use only the values shown. **Clear mask** shows every value
again; the mask is saved with the project.

A single-band raster of continuous values (an elevation model, a temperature grid, an index such
as NDVI) is first drawn as a grey stretch, black to white (matplotlib's `gray`), clipping 0.5 % at each end. To draw such rasters
your own way, style one with the Stretch symbology, then click **Save as Default** under
**Resampling**: rasters added from then on, opened files and analysis results (a slope, an NDVI)
alike, take its color scheme, inversion, stretch type and
resampling (a Custom Min/Max stretch is in one raster's own units, so it can't be saved). **Reset
Default** goes back to the grey stretch. The app keeps the setting with its preferences, for every
project; colour images, rasters with their own color table and rasters of a few categories keep
their own look. Scripts use `raster.setDefaultStyle`, e.g.
`{"ramp": "Viridis", "invert": false, "stretch": "standardDeviation", "n": 2, "resampling": "bilinear"}`,
`{"fromLayer": true}` or `{"reset": true}`; with no parameters it returns the current setting.

Tiles of one dataset drawn as separate layers each stretch over their own values, which shows as
steps where they meet. `raster.stretch` with `{"like": "<layer>"}` gives a layer the value range
of another layer's stretch as fixed bounds, so the tiles match; `{"type": "custom", "min": …,
"max": …}` sets the bounds yourself.

Clicking a raster with the Explore tool shows its pixel values. On a raster of two bands or more,
the pop-up's **Spectral Profile** button charts every band at that pixel and opens the profile, where
more points are compared (see [Spectral Analysis](analysis.md)). A band's wavelength is read from the
GeoTIFF's GDAL metadata or its `.aux.xml` sidecar: GDAL's `CENTRAL_WAVELENGTH_UM` (its `IMAGERY`
domain), or a `wavelength` or `central_wavelength` item (as rasterio, STAC exports and ENVI files
through GDAL write them) in the unit a `wavelength_units` item gives. With no unit, a value under 100
is read as micrometres and any other as nanometres: imaging sensors see from about 0.3 to 15 µm, so
the two never meet. One that isn't a length from 1 nm to 1 m, or another length for a band that has
one, is said and left out. A GeoTIFF export writes the wavelengths of the bands it writes (one
band, or a raster's RGB) back as `CENTRAL_WAVELENGTH_UM`. Projects remember a raster's file
path relative to the project and reopen it; rasters added in the browser are stored inside the project.

### Zarr stores

**Home › Add › Add Data › Zarr…** (`layer.addZarr`) adds a variable of a [Zarr](https://zarr.dev) store
(format 2 or 3), the multidimensional arrays climate and Earth-observation data are kept in: give
the store's web address, or (on the desktop) its folder, and **Connect** lists its variables
(`raster.zarrInfo`). A variable's map is its latitude/longitude (or y/x) dimensions; its other
dimensions, such as time or pressure level, are picked a step at a time with a slider, labelled by
their coordinates (CF time units as dates, the `noleap` and `360_day` model calendars included).
A coordinate of whole years from 1000 to 9999 with no CF units (integers named `time` or `year`,
or marked `axis: "T"` or `standard_name: "time"`, with no units or units of years, as the
AlphaEarth mosaic's `2017…2025`) is a time axis too, each year on its 1 January, for the time
slider and a series at a point. Step counts in that range stored the same way (no units, named
`time`) would be read as years too: give such a coordinate CF units.
**Add to Map** adds that step as a raster layer named after the variable and step (`tas
2014-07-15`), and the dialog stays open to add others. Only the chunks the step needs are fetched,
a few at a time, in the background.

Stores are read in pure Rust, in the web app as well as on the desktop: Zarr 2 with consolidated
metadata (`.zmetadata`, as xarray writes) and Zarr 3 (`zarr.json`, sharded arrays too), chunks
compressed with blosc (LZ4, zlib or zstd, byte- or bit-shuffled), zlib, gzip, zstd or LZ4, of
integer or float values, in C or Fortran order. A sharded array's shards are fetched whole, except in a window (below) and in a series at a point (`zarr.series`), which reads each shard's index and then only the chunk holding the cell, by range.
`_FillValue`, `missing_value` and a Zarr 2 array's fill value are no data; a Zarr 3 array's fill
value isn't: chunks never written are left empty, but chunks missing from inside a shard show the
fill value. `scale_factor` and `add_offset` are applied. The coordinates must be evenly spaced (a curvilinear grid isn't read);
longitude/latitude grids of 0–360° are turned to -180–180°, and projected ones take their
coordinate system from the variable's grid mapping (`crs_wkt`, `spatial_ref`, an EPSG code) or
`"crs"`. A grid placed by the zarr-conventions `spatial:transform` (an affine transform with no
rotation, pixel- or node-registered) reads no coordinates, and the transform wins over coordinates
the store also holds. It is the variable's own, or that of the nearest group above it (up to 64
levels up) that gives one, taken with that group's `spatial:shape`, which must be the variable's map's. Its map
dimensions are those `spatial:dimensions` names. `proj:code`, `proj:wkt2` or `proj:epsg` give
the coordinate system, after the variable's own attributes and its grid mapping, and before a
`spatial_ref` or `crs` array beside it. A variable opened by its own address doesn't see its
groups' attributes: open the store's address with `"variable"` instead.
A web store whose root lists no arrays (a Zarr 3 root `zarr.json` with no consolidated metadata,
or a Zarr 2 `.zgroup` with no `.zmetadata`) needs `"variable"`: its array, the groups above it,
the root's `spatial:dimensions` and the coordinates its dimensions name are then found by name
(`raster.zarrInfo` without one says so). The Add Data › Zarr dialog can't name a variable before
it connects yet: give it the address of one array.
Blosclz-compressed and transposed chunks and shards inside shards aren't read yet, and say so. A
blosc or LZ4 chunk whose header claims more bytes than its data can make, or whose blocks lie
outside it, is refused, naming the array. A store by web address must allow the browser to fetch it
(CORS) for the web app; Google Cloud's CMIP6 bucket does, ARCO-ERA5's doesn't (the desktop reads both).

```sh
rustgis-cli run --cmd 'layer.addZarr={"url":"https://storage.googleapis.com/gcp-public-data-arco-era5/ar/1959-2022-full_37-1h-0p25deg-chunk-1.zarr-v2","variable":"2m_temperature","index":{"time":539100}}'
```

**A window** of the map (`layer.addZarr` with `"cells": [first column, first row, end column, end
row]`, the ends not included, or `"bbox": [west, south, east, north]` in the grid's coordinates,
degrees for a longitude/latitude grid) reads only the chunks it touches, and only its part of the
coordinates (by byte range when they are stored uncompressed). In a sharded array it reads each
shard's index by range, then only the shard's chunks the window needs, one range each; a window
over more than 4,096 of the shards' chunks reads its shards whole instead. A box is placed by the variable's
coordinates, read whole, so it takes grids of up to 4,000,000 cells a side; a longer one takes
`"cells"`. A grid placed by its `spatial:transform` places a box by arithmetic, whatever its
size. A box reaching past the grid is read within it, and says so; one over a longitude grid's
edge (0°/360°, or ±180°) is refused: give a box on each side (one round the whole globe is the
whole grid); `raster.zarrInfo` with
`"bbox"` gives the cells a box covers (`window.cells`). A window is at least 2 × 2 cells (a box within one cell is widened to two, and says so). A window
is kept in the project as a GeoTIFF, as a web store's slice is, also from a folder; with
`"time": true` it follows the time slider instead, each step reading only the window, and the
project keeps the window's cells with the store. A grid whose axes aren't named or measured as
longitude, but whose store gives a geographic system, takes a box or a point written either way
round the globe too, as it is drawn.

**Follow the time slider** (desktop; `layer.addZarr` with `"time": true`) adds the variable as one
layer whose step is the time slider's. The slider turns on over the variable's dates when it is
off, and the layer shows the last of its steps in the slider's window, or nothing when none falls
in it. Moving or playing the slider reads that step's slice (only its chunks), keeping the last
eight read. A web store's next two steps are read ahead in the background. In the desktop app the
slider doesn't wait for a step not read yet: the layer shows the step before until the new one is
read, and a step that can't be read empties the layer and says why. Playing waits for each step to
be read before it moves on, so every step shows. Scripts, models, notebooks, AI agents, the command
line and **Export** wait for each step. The stretch is fixed at the first slice's range, so a
colour means the same value at every step, and **Export** writes an animation of it. An axis longer
than the slider's 20,000 steps (hourly ERA5) takes a period with `"start"` and `"end"`; given
neither (as the dialog adds it), the layer takes 20,000 steps from the one picked. The project
keeps such a layer by its store (a folder or a web address) and selection, not as a slice, and
reads the slider's step again when it opens.

```sh
rustgis-cli run --cmd 'layer.addZarr={"path":"air.zarr","variable":"t","time":true}' --cmd 'time.export={"path":"t.gif"}'
```

**View › Time Series** (desktop; `zarr.series`) reads a Zarr variable's values through time at a
point. Pick a Zarr layer, then **Pick a Point** on the map. A layer that follows the time slider
gives its period, a slice the whole axis, and `"start"`/`"end"` any other. Only the chunks holding
that point's cell are read. A store chunked one step at a time, such as ARCO-ERA5's map-oriented
copy, reads a chunk per step: about 4,000 steps at most, so a long axis needs a shorter period.
A sharded variable counts the chunks inside its shards the same way, since each one is its own
request; each shard read also takes one request for its index, which the cap doesn't count, so a
series with one step per shard makes about twice as many requests as it has steps.
The chart draws up to six points as lines and marks the time slider's step, and a click on it
takes the slider to that date. **Export CSV…** writes the date and value of each point.

```sh
rustgis-cli run --cmd 'zarr.series={"path":"air.zarr","variable":"t","lon":-55.7,"lat":-12.5}'
```

**Summarise over time** (in the Time Series pane; `zarr.reduce`) makes a raster of one statistic of
a Zarr variable's steps: **mean**, **min**, **max**, **sum**, **std** (the population standard
deviation), **count** of values, or **countAbove** a threshold (days above 30 °C, say). It reads a
layer's period by default, else the `"start"`/`"end"` given, and `"months"` keeps one season across
years (`[12, 1, 2]`). Slices are read one at a time and folded into running totals (12 to 20 bytes a
cell, for grids of up to 32 million cells), so memory depends on the grid, not on the period: at most
20,000 steps, with progress and **Cancel** in the app. A cell keeps no value when it had fewer values
than `"minValid"` (a share, 0 by default) of the steps read; with the default, **count** and
**countAbove** give 0 where a cell never had a value (land in an ocean variable, say), and a
`"minValid"` above 0 leaves such cells empty. The layer is named after what it holds (`tas mean 1991-01-01–2020-12-31`) and kept in the
project as a GeoTIFF.

```sh
rustgis-cli run --cmd 'zarr.reduce={"path":"air.zarr","variable":"t","stat":"mean","months":[12,1,2]}'
```

A single step (a layer that doesn't follow the slider) from a store in a folder is saved in the
project by its folder and slice (`2m_temperature[time=539100]`) and read again when the project
opens, as Add Data on the folder does (its first variable); a single step from a store by web
address is kept in the project as the slice read. `?zarr=<address>` in a web app link
opens the dialog on a store.

## Project items

RustGIS saves pieces of a project as files you can share and reuse:

| Item | Extension | Made with | Opened with |
| --- | --- | --- | --- |
| Map file (a map with its data) | `.rgismap` | Publish › Save As › Map File | Command Search (`map.import`), or drop it on the window |
| Layer file (a layer with its symbology) | `.rgislyr` | Publish › Save As › Layer File | Add Data |
| Styles (saved symbology) | `.rgisstyle` | Style › Styles › New Style, then Publish › Save As › Style Items | Style › Styles › Add Style File |
| [Color ramps](cartography.md#custom-color-ramps) | `.rgisramp` (also reads QGIS `.xml`/`.qml`, GDAL color `.txt` and GMT `.cpt`) | Publish › Save As › Color Ramps, or by hand | Style › Styles › Add Color Ramp File |
| Project template (the setup without features) | `.rgistemplate` | Publish › Save As, or Command Search (`project.saveTemplate`) | Command Search (`project.fromTemplate`) |
| Project package (the project, with linked raster files embedded while they fit) | `.rgispkg` | Publish › Package › Project | Project › Open, or drop it on the window |
| Map package / mobile map package | `.rgismpk` | Publish › Package › Map or Mobile Map | Open the package as a project, or drop it on the window |
| Results package (geoprocessing history and outputs) | `.rgisgpk` | Publish › Package › Results | any zip tool |
| Task (guided command steps) | `.rgistask` | Analyze › Task, Publish › Save As › Task Item | Analyze › Task › Import Task |
| Bookmarks (as longitude, latitude and scale) | `.rgisbookmarks` | Home › Navigate › Bookmarks › Export Bookmarks | Home › Navigate › Bookmarks › Import Bookmarks |
| Layout | `.rgislayout` | Publish › Save Layout File | Home › New Layout › Import Layout |

Packages are zip files. Project and map packages can newly embed a combined 512 MiB of linked
raster files on the desktop (128 MiB in the web app); a raster that doesn't fit stays linked.
Already embedded uploads are retained. Local streamed vector files, databases and web layers
stay linked to their sources. A mobile map package keeps only the visible layers and simplifies their
geometry, for phones and tablets; what simplifying changes or leaves alone (a latitude past a pole
taken as the pole, a one-vertex line part left out, a feature left as it was or too intricate to
simplify whole) is said, as Simplify says it. Styles can also be imported from the layers of a
layer file, map file or project. Data › Layer Templates adds empty layers with ready-made schemas
(places, routes, areas, observations…) to draw into. Add Folder connects a folder to the Browser,
which also lists the project's layouts, styles and tasks. A task's steps each run a command; New Task
From History turns the geoprocessing history into a task you can replay.

Saved projects, templates, map and layer files, and packages remove recognized credentials from
supported addresses and recognized executable parameters. The open project keeps its credentials. Removal
notices describe what a shared workflow or video will need again; an unchecked-content notice
means opaque content was kept and needs your review. Unrecognized credentials can remain in
addresses without a notice, so review addresses and preserved content before sharing.

RustGIS remembers up to 32 distinct saved snapshots for explicit project reopening, with at most
1 MiB of restoration data per snapshot and 8 MiB in total. A snapshot must match, and only its
recorded, unchanged fields can regain credentials. Larger restoration records are not remembered;
a notice says so. Imports, packages and peer exchanges do not use this memory. After eviction or
closing RustGIS, sign in again or add the protected sources with their credentials. Feature data
and user text remain data, so review them before sharing.

A layer added from a saved web-service connection records the connection's id (`"connection"`
in the `.rgis` file; never its login), so the project needs no token in its addresses. See
[Web-service logins](#web-service-logins) for what happens when it opens where that connection or
its login is missing.

### Complete streamed Shapefile rendering

The CPU renderer traverses a streamed Shapefile in source order instead of painting only
its interaction sample. Shapes and attributes arrive in chunks targeting 8 MiB of owned
feature storage; projection and display geometry are temporary copies of one chunk.
Whole-file imports, loading for editing and other providers retain their existing read APIs.
An indivisible row exceeding the chunk target is emitted alone, preserving its full geometry.
A zero budget reports an error.

Styles, definition queries, display filters and label classes use the same painter as an
in-memory layer. The renderer chooses geometry simplification from statistics over the whole
layer, accumulates point bins across chunks, applies transparency and blend once per layer,
and draws selection highlights after the whole layer. Labels share the existing global
priority, stable tie ordering, collision rules and 20,000-candidate placement limit.
Screen rendering and PNG/layout exports share this traversal. Live redraws of a streamed
Shapefile therefore also read the complete source; line/polygon layers currently make an
additional statistics pass. Interaction windows remain byte-budgeted, but they are not a
shortcut for this complete painter, so redraws can be I/O intensive on large files.

Native flat extrusions use a private disposable SQLite ordering workspace (2 MiB page-cache
target, memory mapping disabled, 4 GiB database ceiling), with a global shadow pass followed
by globally sorted solids. It is removed when rendering finishes. Failure or incomplete
traversal does not publish a partial successful frame. This workspace is not a reusable index.

This is not a fixed bound on total renderer memory: raster targets, label candidates and the
existing combined polygon-mask path have separate costs. Upright perspective point markers
and polygon extrusions retain the complete upstream depth renderer through an isolated
full-source snapshot. Upright points require a known source total of at most 50,000 rows;
larger or unknown totals use complete ground-marker traversal, independent of the current
interaction-window size. Plain fills, lines and mask sources continue complete chunk traversal
per band, including terrain bands; they do not consume that snapshot's feature budget.
Retained complete-source feature storage across the snapshot has a separate budget (512 MiB natively, 192 MiB in the browser).
Exceeding it fails the whole frame with a source diagnostic, without sampling or truncating
features; raster targets, derived geometry caches and decoding scratch are outside that budget. Flat line/polygon rendering
makes a statistics pass before painting and currently reads the entire source, so it trades
additional I/O for complete rendering and predictable chunk storage. Geometry preprocessing
and GPU preparation are separate optimizations.
