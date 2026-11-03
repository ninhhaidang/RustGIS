# Supported Data Formats

This page lists the data formats RustGIS reads and writes, how to add each one, and the main
limits. [Data](data.md) covers adding data, large files and rasters in more detail; [Maps and
Basemaps](maps.md#basemaps) covers web tile layers.

Most native readers work in the desktop app and the [web app](https://web.rustgis.app).
Formats read through external GDAL or DuckDB programs need the desktop app or `rustgis-cli`;
they aren't available in the web app. Database connections and folder-based File Geodatabases
are desktop-only too. Browser files are uploaded rather than opened by path; GeoPackages use a
pure-Rust SQLite file reader there. Reprojection supports the coordinate systems RustGIS knows,
not every EPSG code; azimuthal equidistant systems such as Equi7 need the desktop app.

## Vector and tabular files

| Format | Extensions or source | How to add it | Notes |
| --- | --- | --- | --- |
| GeoJSON | `.geojson`, `.json` | Home › Add › Add Data, or drop the file | Features with typed attributes; a single-member `GeometryCollection` is unwrapped (including nested single-member collections). Empty or multi-member collections are unsupported: the whole feature, including its attributes, is skipped and counted in an unreadable-geometry notice. A FeatureCollection's legacy `crs` member (`{"type": "name", "properties": {"name": "urn:ogc:def:crs:EPSG::28992"}}`) is honoured for EPSG codes (`EPSG:28992`, `urn:ogc:def:crs:EPSG::28992`, `urn:ogc:def:crs:EPSG:6.6:28992`) and for `CRS84`: a projected or other non-WGS 84 system converts the coordinates to longitude/latitude and the layer keeps it as its coordinate system (Layer properties, export in it, Define Projection); `CRS84` and `EPSG:4326` are longitude/latitude already, so the coordinates stay as they are and the layer has no other coordinate system. Coordinates are x/y (easting/northing, or longitude/latitude) whatever the EPSG axis order. A `crs` that isn't understood (a `link`, WKT or PROJ text, any other name, an unknown code) is named in a notice and the coordinates are read as longitude/latitude. A feature whose coordinates can't be converted, or convert to beyond ±180.5° longitude or ±90.5° latitude (the declared system doesn't fit the numbers), is left out and counted in a notice, and Define Projection then asks for the file to be added again. A `crs` on a feature, a geometry or a lone Feature is ignored. GeoJSON is always written in WGS 84 without a `crs`. Desktop FeatureCollections larger than 32 MiB use an in-place reader (which honours `crs` too, reading the file once more to check the features convert); layers with more than 100,000 features stream a view at a time. Other GeoJSON files load whole, up to 256 MiB. |
| Delimited text | `.csv`, `.tsv`, `.txt` | Add Data, or Home › Add › Add Data › Delimited Text… to choose the columns from the file's header | Comma, semicolon, tab or pipe separated (found from the header, or `"delimiter"`); `info.fileColumns` lists the columns and the ones found. Points from x/y columns (named like longitude/latitude or easting/northing, or chosen with `"x"` and `"y"`), or any geometry from a WKT/EWKT column (named like `wkt` or `geometry`, or `"wkt"`). Coordinates in another system are converted from `"crs"` (`EPSG:2263`, or WKT) or an EWKT `SRID=`; without either, coordinates outside longitude/latitude are pointed out. Text is UTF-8, or UTF-16 with its byte-order mark; other files are read as Windows-1252 (Latin-1, as many public sources still publish CSV), with a notice saying so. A file without coordinates opens as a standalone table. |
| Shapefile | `.shp` with `.dbf` (and `.shx`), or zipped (`.zip`) | Add Data or drop the file; in the web app, pick or drop the `.shp` and `.dbf` together, with the `.shx` and `.prj` if there are (they open as one layer), or the zip | A zip may hold several Shapefiles, in folders, and other vector files (GeoJSON, CSV, GeoParquet, FlatGeobuf); each becomes a layer, at most 32 for the whole archive (Shapefiles first, so other files are left out when Shapefiles fill it). `.txt` files and macOS `__MACOSX` entries are passed over, and a member that can't be read is skipped and named in a notice. Converted to longitude/latitude from the coordinate system in its `.prj` (EPSG, Esri or OGC WKT, or WKT2 as PROJ and QGIS write it; Web Mercator, UTM, State Plane and other common projections); without a `.prj` it must be in longitude/latitude. On the desktop, one with more than 100,000 shapes (counted from its `.shx`) is read in place, a view at a time. |
| KML / KMZ | `.kml`, `.kmz` | Add Data, drop the file, or upload it in the web app | A layer for each kind of geometry (points, lines, polygons), with each placemark's name, description, folder path and extended data. Styles are kept: the layer is drawn by unique values of its `style` field, with each KML style's line and fill colours, widths and icon colours (up to 64 styles). Multi-geometries and `gx:Track`s are read. Ground overlays (pictures on a `LatLonBox`, turned by its `rotation`) are raster layers kept in the project: the picture inside a KMZ, or a file beside a KML (not a web address, nor outside its folder); a `gx:LatLonQuad` isn't drawn. |
| GPX | `.gpx` | Add Data, drop the file, or upload it in the web app | Waypoints, routes, tracks (one line per track, its segments as parts) and track points (with elevation and time) as separate layers. |
| Spreadsheets | `.xlsx`, `.xlsm`, `.xlsb`, `.xls`, `.ods` | Add Data (or Delimited Text… to choose the columns), drop the file, or upload it in the web app | One sheet at a time, read like delimited text: the first non-empty row names the columns, and x/y or WKT columns (found, or given with `"x"`, `"y"`, `"wkt"` and `"crs"`) make the geometry; a sheet without them is a table. The first sheet is added and the others named in a notice; `"layer"` picks one, and `info.fileLayers` lists them. Dates read as ISO 8601. |
| GeoRSS | `.rss`, `.atom`, `.georss`, or `.xml` | Add Data, drop the file, upload, or From URL… | RSS 2.0, RSS 1.0 and Atom feeds with GeoRSS Simple, GeoRSS GML or W3C Basic Geo places: points, lines and polygons layers with each item's title, link, description, date, id and category. |
| LandXML | `.landxml`, or `.xml` | Add Data, drop the file, or upload | Coordinate geometry points, parcels (polygons), alignments (lines; curves as chords) and TIN surfaces (a triangle per face, with its mean elevation), converted from the file's `epsgCode` or `"crs"`. |
| Encoded polylines | text | `layer.addEncodedPolyline` with `"polyline"` (one or a list) | Google's encoding, precision 5, or 6 (`"precision"`) as OSRM and Valhalla can give: a line layer. |
| GML | `.gml` | Add Data, drop the file, or upload | Simple features in GML 2 and 3 (WFS answers, OGR's GML): a layer per feature type, its simple elements as fields. Converted from the `srsName` (EPSG codes, URNs and URLs; the URN and URL forms of geographic systems are latitude first). |
| MapInfo Interchange | `.mif` with its `.mid` (or both zipped) | Add Data, drop the file, or upload the zip | Points, lines, polylines, regions (holes found by nesting), rectangles and ellipses; text and arcs are rows without geometry. Converted from the `CoordSys`: longitude/latitude, Transverse Mercator, Lambert Conformal Conic, Albers or Mercator. A loose `.mif` upload has no `.mid` attributes; zip both files to keep them in the web app. |
| OpenStreetMap PBF | `.osm.pbf`, or `.pbf` containing OSM data | Add Data, drop the file, or upload | Points (tagged nodes), lines and multipolygons (closed ways that are areas, and multipolygon and boundary relations), with common tags as fields and the rest in `other_tags`, as GDAL gives them. Up to 40 million nodes (4 million in the web app); on the desktop, extracts over 256 MiB open through GDAL when it is installed. Vector-tile `.pbf` files use Add Tile Layer instead. |
| TopoJSON | `.topojson`, or `.json` | Add Data, drop the file, or upload | Shared arcs (quantized or not) are rebuilt into geometries. A `.topojson` adds a layer per object, up to 64; a topology named `.json` adds its first object and names the others in a notice. Use `.topojson` to add them all. |
| AutoCAD DXF | `.dxf` (ASCII) | Add Data, drop the file, or upload | Points, lines and polygons layers from the drawing's entities: POINT, LINE, LWPOLYLINE and POLYLINE (bulges drawn as arcs; closed ones stay lines; polyface meshes become polygons), CIRCLE, ARC, ELLIPSE, SPLINE, HELIX, LEADER, MLINE (its centre line), TEXT, MTEXT, ATTRIB and constant ATTDEF (points with their text, at the alignment point of justified text), HATCH boundaries (holes found by nesting), SOLID, TRACE and 3DFACE (polygons). Blocks (named in any case) are drawn where INSERTs place them (scaled, rotated, in arrays, nested up to 8 deep), and so are DIMENSIONs' blocks (a linear dimension without one draws its lines and measured text). Entities in an object's own coordinate system are turned into the drawing's. Fields: `layer`, `entity`, `color` (ACI, BYLAYER and BYBLOCK resolved), `linetype`, `text`, `height`, `rotation`, `block`, `elevation` and `paperspace`. The drawing's layouts (paper space: title blocks, viewport frames) are left out, with a notice, unless `"paperSpace": true`. A drawing never says its coordinate system: give `"crs"` (such as `EPSG:2263`), or its coordinates are kept as they are, with a notice. `"layer"` picks one of the drawing's own layers, and `info.fileLayers` lists them. Up to 2 million features (500,000 in the web app). Not read: binary DXF, MULTILEADER, WIPEOUT, IMAGE and ACIS solids (3DSOLID, REGION), named in a notice. |
| Geotagged photos | `.jpg`, `.jpeg` | Add Data (several at once in the web app), drop them, upload, or `layer.addPhotos` with `"paths"` or a `"folder"` | JPEGs with a GPS position in their EXIF become points in the **Photos** layer (photos added one at a time join it): each with its `name`, `file`, `time` (when it was taken), `altitude`, `direction` (where the camera faced), `camera` and a `thumbnail` of at most 160 pixels, kept with the feature so the project needs no other file. Identify shows the thumbnail. Photos without a position are named in a notice and left out; a layer holds up to 2,000 photos. A JPEG is always read as a photo unless `layer.addData` or `layer.addDataBytes` is given `"as": "scan"` (a phone photo of a paper map, say: see [Scans with no coordinates](#rasters)). |
| LiDAR point clouds | `.las`, `.laz` (COPC too) | Add Data, drop the file, upload, or From URL… | LAS 1.0–1.4 (point formats 0–10) and LAZ, on desktop and web: points with `z`, `intensity`, `classification`, `return` and colour (`red`, `green`, `blue`), coloured by ASPRS class or elevation. An evenly spread sample is read once, at most 2 million points (500,000 in the web app); views use that sample, not the COPC spatial index. LAZ decompression reads at most the first 400 million points (40 million on the web), with a notice when capped. Converted from the file's WKT or GeoTIFF-key record; without one, coordinates are kept with a notice. A local project keeps the path; uploads keep the file bytes. Remote files download whole. |
| GeoPackage | `.gpkg` | Add Data, or upload in the web app | Local files and browser uploads add every feature table, up to 32 (with the rest counted in `skippedCount` and up to 100 named in `skipped`); `"layer"` picks one, and `info.fileLayers` lists them. Desktop byte uploads and downloads add the first table unless `"layer"` names another. Large local tables use their R-tree and reproject supported EPSG systems. Browser tables are scanned, testing each geometry header's envelope first; UTF-16 databases aren't read there. Raster tile tables aren't imported. |
| GeoParquet | `.parquet`, `.geoparquet` | Add Data, or upload in the web app | WKB geometry, with the CRS from the file's `geo` metadata. A binary geometry column is required; plain non-spatial Parquet tables and GeoArrow-native geometry encodings aren't read by this importer. Row groups are pruned by the bbox covering. Snappy, gzip, LZ4 and zstd compression are read, in the browser too; Brotli isn't. Text, whole-number, floating-point and true/false columns become fields, as do decimals stored as 32- or 64-bit integers (up to 18 digits), read as floating-point numbers. Dates and times stored as 32- or 64-bit integers are read as those whole numbers (such as milliseconds since 1970). Other columns are left out: decimals stored as bytes (how many writers store every decimal), binary, lists, maps, structs, INT96 timestamps and further geometry columns. A notice gives how many were left out and names the first 12, each with its kind; another names the date and time columns with their unit. |
| FlatGeobuf | `.fgb` | Add Data, or upload | Read in place by its packed R-tree. Binary columns are left out, with a notice counting them and naming the first 12. |
| Esri File Geodatabase | `.gdb` folder (desktop) | In Add Data pick a `.gdbtable` file inside it; or double-click the `.gdb` in the Browser, or drop the folder | The native reader loads up to 512 feature classes and tables, up to 2 GiB per table. Feature classes become layers and non-spatial tables standalone tables; dates are text. Coordinates use the EPSG/Esri code, else WKT. `"layer"` picks one; `info.fileLayers` lists them. Multipatches become footprints with `z_min`, `z_max` and `height`; large or wall-only ones may use a convex hull. Curves become straight segments between their ends. Unreadable tables (compressed storage, raster fields or unsupported row indexes) are tried through GDAL when installed; otherwise they are named in a notice. |
| Standalone tables | CSV, or any layer without geometry | Add Data | Listed under the map in Layers; join or relate them to layers. |

Shapefile text uses the encoding in its `.cpg` sidecar first, then the language-driver code in
the `.dbf` header. Include the `.cpg` when picking or dropping loose files in the web app.
UTF-8 (including `65001`), DOS code pages such as CP437 and CP850 (numeric, `CP` or `IBM`
labels), Windows code pages such as `1252`, `CP1252` and `Windows-1252`, ISO-8859 encodings
and common East Asian encodings are supported. The Esri `0x57` DBF marker
is read as Windows-1252. An unreadable, empty, invalid, oversized or unsupported `.cpg` gives a notice and
falls back to the DBF code page; an unsupported DBF code page gives a notice and falls back to
UTF-8. Files without encoding metadata keep the UTF-8 default. Unsupported characters become
replacement characters. This applies to zipped files and large files read in place too.
See the [DBF language-driver support inventory](shapefile-encoding.md) for every recognized
marker and the legacy code pages that still use the UTF-8 fallback.

Files with more than 100,000 features (GeoPackage, GeoParquet, FlatGeobuf, and Shapefile on the
desktop) are read in place, a view
at a time, rather than loaded whole: see [Large files](data.md#large-files). They draw, label,
identify and run through tools like any layer; **Load All Features** loads one to edit it.

## Formats read through GDAL

With [GDAL](https://gdal.org) installed (its `ogr2ogr` and `ogrinfo` on the `PATH`, GDAL 3.7 or
later), the desktop app also reads the vector formats below. Each layer is converted once into a
cached, indexed FlatGeobuf, so large ones stream too: see [Formats through
GDAL](data.md#formats-through-gdal).

| Format | Extensions or source | Notes |
| --- | --- | --- |
| AutoCAD DWG | `.dwg` | Through GDAL's CAD driver, when it is built in. Give `"crs"` (such as `EPSG:2263`) when the drawing doesn't say its coordinate system. |
| MapInfo tables | `.tab` | Keep the companion files together. |
| OpenStreetMap XML | `.osm` | |
| GeoJSON Lines, GMT | `.geojsonl`, `.geojsons`, `.gmt` | |

## Files on the web

**Home › Add › Add Data › From URL…** (`layer.addUrl` with `"url"`) downloads a file by its `https://`
(or `http://` or `s3://`, see [S3 buckets](data.md#s3-buckets)) address and adds it like an
uploaded file. Native formats include GeoJSON, delimited text, GeoParquet, FlatGeobuf, GeoPackage,
zipped Shapefiles, KML/KMZ, GPX, spreadsheets, GeoRSS, GML, DXF, TopoJSON, LiDAR and rasters.
Downloads are limited to 512 MiB on desktop and 256 MiB on the web; individual readers may have
lower limits (the whole-file vector readers use 256 MiB). In the web app the server must allow
requests from web pages (CORS). On the desktop, with
[GDAL](https://gdal.org) installed, a JPEG 2000 file (`.jp2`, `.j2k`: Sentinel-2's imagery on S3)
is downloaded too, converted to a GeoTIFF that the project keeps, and added. Desktop byte uploads
(`layer.addDataBytes`) of `.jp2` and `.j2k` use the same conversion. The web app rejects both JPEG
2000 uploads and downloads with an explicit GDAL limitation; convert them to GeoTIFF first.
The format comes from the address, or from the contents when the address has no extension (an API
endpoint, say), JPEG 2000 included: its signature is recognised in the contents, so the desktop
converts it and the web app gives the same GDAL limitation. The
app downloads in the background and adds the layer when the file arrives (to the project it was
started in: opening another meanwhile drops it). A `.pmtiles` address is added as a tile layer
and read a tile at a time instead.

For JPEG 2000 uploads and downloads on the desktop, the converted GeoTIFF must fit within 512 MiB.
The private conversion workspace (source file, output and GDAL scratch files together) is monitored
every 50 ms against a budget of the input file's byte length plus three times that output limit
(1.5 GiB). Conversion stops when the workspace exceeds the budget, but files can grow between
checks: this is not a strict operating-system disk quota. Ordinary conversions of files opened
from disk do not use this workspace budget.

Conversion runs synchronously while the layer is added and can temporarily make the app
unresponsive. The original JPEG 2000 byte buffer is released once it has been staged on disk,
before GDAL runs or the GeoTIFF is read back. The converted GeoTIFF and decoded raster still
occupy memory together; the 512 MiB file limit is not a limit on total process memory.

A tiled GeoTIFF is read in place rather than downloaded: its header first, then, by HTTP range
requests, the tiles of the overview each part of the view needs. Overviews make zoomed-out reads
smaller. It is a tile layer (`map.addTiles` kind `cog`, saved by its address), drawn under the map's
own layers: RGB(A) 8-bit images in colour, other images stretched between the 2nd and 98th
percentiles of their coarsest overview. Planar files (bands stored apart), signed-byte data and
bottom-up images work too. `map.setTiles` selects one or three `"bands"` (from 1) and a fixed
`"range"`; see [Satellite embeddings](web-data.md#satellite-embeddings).
A GeoTIFF in strips, a rotated image or an unsupported tiled image downloads whole as a raster layer, as does a
GeoTIFF added with `"whole": true` (for raster tools and symbology). The web app needs the server
to allow range requests (CORS with the `Range` header).

A GeoParquet or FlatGeobuf file by its address is read in place too, a view at a time, however
large: the layer is streamed like a large local file, and the view's features are fetched in the
background by HTTP range requests and drawn as they land. A FlatGeobuf's header and packed R-tree
index are read first (a level of the tree per round of requests), then only the features in the
view. A GeoParquet's footer is read first, then the row groups the view meets, by their bounding
box covering column (`bbox`) and its statistics: the covering of groups on the view's edge, then
the geometry and attribute columns of the rows taken. A view holding more than a read takes (60,000
features on the desktop, 30,000 in the web app) shows an evenly spread sample of it, in patches;
zoom in for more detail. The project keeps the address and reads it the same way when it opens.
A FlatGeobuf without an
index, a GeoParquet without a covering column, or a server that doesn't answer range requests
means the file is downloaded whole instead, as is any file added
with `"whole": true`; Load All Features and the geoprocessing tools fetch the whole file on the
desktop, while in the web app they need the file downloaded first.

On desktop, `layer.addUrl` waits for downloads, up to a minute; the From URL… dialog (`ui.addUrl`)
and a plugin's `layer.addUrl` work in the background. In the web app the same dialog handles asynchronous downloads.
`layer.addUrl` can add PMTiles and COG tile layers directly, but an ordinary download returns
`{"pending": true, "url": "…"}` and is added later by the app. To open a remote GeoParquet or
FlatGeobuf whose header hasn't been fetched yet, use the From URL… dialog or a `?data=` link.

## Map and feature services

**Home › Add › Add Data › From Service…** connects to a service by its address, lists what it offers from
its capabilities. For OGC API feature collections, enter a group name (initially **My OGC Layer**)
and choose **Import All**. The group expands into ordinary point, line and polygon layers with
the same legends, styling and attribute tables as file imports. Other offerings have individual
**Add** buttons (`service.layers` and `service.add` from scripts, the command line and MCP).
An OGC API landing page works directly: RustGIS follows its advertised
JSON collections link, so you don't need to append `/collections`. If the landing page also
offers map tiles, they remain listed alongside the collections. A failed collections request
leaves any available map layers usable and shows the error.

When a service can't be reached or refuses the request, the dialog says what happened in plain
words and what to try next: a sign-in or token it needs (401), access refused (403), nothing at
that address (404), too many requests (429), a problem on the server (5xx), or a server that
couldn't be reached or took too long. When a firewall in front of the server (Cloudflare,
Akamai, Imperva, Sucuri or AWS WAF) blocked or challenged RustGIS itself, told by its own block
page or a header it sends only then, it says so: only the service's administrator can let
RustGIS through. A firewall that only passed on the service's own refusal (a private service
wanting a token) isn't blamed for it. **Details**, folded below, lists
what the server said for an expert or the administrator: the address (without keys or tokens),
the status, the server, the firewall and the request's id, the `User-Agent` RustGIS sent, and
the title or first words of the server's answer (as text). **Copy details** copies them to send.
In the web app the browser hides most of a refused answer, so it tells the status only.

Right-click the group and choose **Refresh** to rediscover its collections and update them all,
including newly added or removed collections. Right-click a child and choose **Refresh** to
update only that collection. Existing children keep their names, styles and visibility (a changed
geometry type gets a suitable default symbol); selections are cleared because row positions can
change. Refresh replaces local feature edits with the service's data. Failed imports or refreshes
leave the map intact; a successful operation is one undo step. Groups and their source connections
are saved in the project. Group downloads are limited to 2000 collections and 64 MB in total and
use the existing per-collection feature limits.

Scripts use `service.import {"url":"…","name":"My OGC Layer"}` and
`service.refresh {"group":id}` or `service.refresh {"layer":id}`. The browser uses
`service.groupPlan` to discover the requests, then supplies `text`, `documents`, and
`responses: [{"url":"…","text":"GeoJSON"}]` to import or refresh without blocking network calls.

| Service | What it lists | How a layer is added |
| --- | --- | --- |
| WMS 1.1.1 / 1.3.0 | Named layers offered in Web Mercator or in a coordinate system RustGIS can transform (an EPSG code, CRS:84) | A layer of GetMap images (PNG when the service offers it, else JPEG): one picture of the view, or tiles; see [One picture of the view, or tiles](#one-picture-of-the-view-or-tiles) and [Services in other coordinate systems](#services-in-other-coordinate-systems) |
| WMTS | Layers whose tile matrix set is Web Mercator's pyramid, or a tile matrix set in a coordinate system RustGIS can transform | A tile layer from the tile template (or KVP GetTile) |
| WFS 1.1 / 2.0 | Feature types | Their features downloaded as GeoJSON, following next links. Without a next link, WFS 1.x reads only the first response (requested limit: 50,000); reaching that limit produces an incomplete-read notice. A server may cap this response below the requested limit. WFS 2.0 verifies the hits total and uses offset requests when needed, up to 100,000 features; unknown totals, repeated offset pages and interrupted reads produce a notice |
| OGC API Features | Collections | Their items downloaded as GeoJSON, following the service's `next` pages up to 100,000 features (a command such as `service.add` waits a minute at most) |
| OGC API Tiles | Collections' map and vector tiles; a landing page's dataset map tiles | A tile layer in WebMercatorQuad, from the tile template the tileset advertises (its `item` link; `…/tiles/WebMercatorQuad/{z}/{y}/{x}`, the usual path, when that can't be read), keeping the tiles link's query (a token), map tiles in the server's image format, vector tiles drawn layer by layer in colours of their own |
| WCS 1.0 / 2.0 (a 1.1 service is asked in 1.0) | Coverages, with their bounds | A raster: GetCoverage of the map's view within the coverage's bounds (or its whole extent: `"bbox": "full"`), as a GeoTIFF in EPSG:4326, at most 2048 pixels on a side |
| ArcGIS REST | FeatureServer and MapServer layers; a MapServer's map; an ImageServer; a VectorTileServer | Layers queried as GeoJSON, page by page past the server's record limit (`resultOffset`) up to 100,000 features (a command such as `service.add` waits a minute at most), where the layer supports paging (ArcGIS Server 10.3 and later; otherwise up to the record limit); a map as its tile cache when it is Web Mercator's pyramid, else exported images; an ImageServer as exported images; a VectorTileServer as its MapLibre style |

A WMS or WMTS layer's tiles are asked for down to the detail its service gives, and stretched
only past it: a WMTS layer's over the zooms of its tile matrix set (or those its
`TileMatrixSetLimits` name; swisstopo's SWISSIMAGE goes to zoom 20), a WMS layer's between the
scales it is drawn at (WMS 1.3.0's `MinScaleDenominator` and `MaxScaleDenominator`, 1.1.1's
`ScaleHint`, its parent's when it gives none), else down to zoom 22 (about 4 cm a pixel at the
equator), since a WMS draws at any scale. `service.layers` lists these zooms, `minZoom` and
`maxZoom`, for each WMS and WMTS layer.

The type is guessed from the address (`?SERVICE=`, `/wmts`, `/wfs`, `/wcs`, `/collections`, `/rest/services`…) or
chosen (select **OGC API Features and Tiles** when a landing page's address doesn't identify
its type). A WebMercatorQuad tileset is found by its
tile matrix set, whatever the server calls it. A service that asks for a token says so. In the web app the server must allow requests
from web pages (CORS).

A coverage is read once, for the view when it was added (its DescribeCoverage first, for its axes,
format and bounds; `service.coverage` gives the GetCoverage address alone). Moving the map doesn't
ask for it again: add it again for another area. The server reprojects it to EPSG:4326 (WCS 2.0
through its CRS extension); one that can't answers with an error, which is shown.

A feature type is asked for in CRS84 when it offers it, else in `EPSG:4326`. Every server tried
answered those requests longitude first, but a server may answer latitude first (deegree does
when asked in the URN form, `urn:ogc:def:crs:EPSG::4326`). The `crs` a GeoJSON answer names
doesn't tell the order (GeoServer names its longitude-first answers `urn:ogc:def:crs:EPSG::4326`),
so the features' positions are turned round only when they miss the layer's longitude/latitude
bounds from the capabilities and fit them turned (give or take a tenth of the bounds' size, or
half a degree). An OGC API collection's extent counts as such bounds only in CRS84, the
standard's default. A layer listed without bounds, or whose features fit both ways, is read as it
comes.

A WFS, OGC API Features or ArcGIS feature layer comes whole, as in other GIS programs. A large
one (a country's municipalities, hundreds of megabytes) can be asked for over an area instead:
tick **Only the map's area** in the dialog, or give `service.add` a `"bbox"`
(`[west, south, east, north]` in longitude/latitude, or `"view"`). The service then sends only
the features that cross the area, whole, not clipped. `service.add` then zooms to the box given
(not to the whole layer's bounds), and leaves the view as it is for `"view"`. The box is asked in
each service's own terms:

- **WFS 1.1 and 2.0:** `BBOX`, in CRS84 when the feature type offers it, else in
  `urn:ogc:def:crs:EPSG::4326`, latitude first as that name fixes.
- **OGC API Features:** `bbox`, longitude first; the `next` pages keep it.
- **ArcGIS:** an envelope in EPSG:4326 the features intersect; the pages past the record limit
  keep it.

A WFS 1.0 service reads its box in the feature type's own coordinate system, so it is asked for
the whole layer, as is an area across the antimeridian; a note says so. When a whole layer takes
longer than `service.add` waits (a minute), the error suggests `"bbox"`:

```sh
rustgis-cli run --cmd 'service.add={"url":"https://geoservicos.ibge.gov.br/geoserver/wfs","layer":"CGMAT:qg_2024_030_munic","bbox":[-45.99,-10.93,-40.37,-2.74]}'
```

The layer holds the features of the area it was added for: moving the map doesn't ask for more.

For callers that fetch documents themselves (including the web app), `service.layers` accepts
`text` for the initial document and `documents: [{"url": "…", "text": "…"}]` for subsequent
responses. It returns `next` while another collections document is needed; supplied documents
never cause a blocking capabilities fetch. Each listed layer includes `source_url`; pass it
and the same documents to `service.add` to distinguish layers with the same name. Discovery
follows at most four documents and rejects loops and disallowed addresses. Collection lists
aren't paged. On the desktop, feature downloads follow their `next` pages (see the table above).

### One picture of the view, or tiles

A WMS layer and an ArcGIS map or image service's export draw their pictures on request, for any
area and size. Added from **Add Service**, the Library or the National Map (its drawn maps; its
imagery stays in tiles), such a layer is asked for **one picture of the view**: the service lays
out its symbols and labels over the whole view, so none is cut at a tile edge, and it is asked
once a view instead of for every tile (#1162). So is one added by its address (`map.addTiles`
with a GetMap or export address that has its size: `WIDTH` and `HEIGHT`, or `size`), unless
it's given `"singleImage": false` or a `"tileSize"`.

- The picture reaches past the view, its middle on a grid, so small pans and zooms (most of a
  quarter of a level) ask for nothing new; while the next picture loads, the last one stays in
  place.
- It is asked for in the map's logical pixels, as tiles are, so the service's symbols keep their
  size on a high-density screen; at most 4096 pixels a side (2048 for five minutes after a larger
  picture of the layer fails), coarser for a view larger than that. A picture the service fails
  to draw leaves the layer drawn in tiles there.
- Nothing is asked for zoomed out past the layer's scale range or outside its bounds, and closer
  in than its finest zoom its picture there is stretched, as its tiles are.
- It is drawn so on a flat map, north up or turned, in Web Mercator or in a projection with an
  EPSG code. The picture is asked for in the map's own system when the service offers it (a
  map service's export takes any: the service projects it); otherwise in one it does offer
  (Web Mercator, or the WMS layer's [own system](#services-in-other-coordinate-systems)), and
  warped into place piece by piece. On the globe, a tilted map, a projection without an EPSG
  code (the presets), or a Web Mercator or longitude/latitude picture across or near the
  antimeridian (a little larger than the view, it would cross it; tiles wrap), the layer is
  drawn in tiles. Exports
  and layouts ask for their own picture.

Right-click the layer in the Layers pane, **Draw as**, to switch to **Tiles** and back to
**Single image** (`map.setTiles` with `"singleImage"`). A layer asked for in Web Mercator takes
tiles of 256, 512 or 1024 pixels (`"tileSize"`): larger tiles cut fewer labels and ask fewer times.
Tiles are fetched while the map moves, and kept for panning back. A layer in a project saved
before keeps its tiles until you switch it.

### Services in other coordinate systems

National and regional services often draw their maps in a coordinate system of their own
(Switzerland's LV95, EPSG:2056; Austria's EPSG:31287) and sometimes not in Web Mercator at all. A
WMS layer offered in any system with an EPSG code that RustGIS can transform (or in CRS:84), and a
WMTS layer whose tile matrix set is in one, is listed and added like any other (#769):

- **On a Web Mercator map**, a layer the service offers in Web Mercator is asked for in it, as
  before. Otherwise a WMS layer drawn as [one picture of the view](#one-picture-of-the-view-or-tiles)
  is asked for one picture in its own system, warped into place; drawn as tiles (and a WMTS
  layer), each of the map's tiles is made from what the service gives for its area in the
  layer's own system, warped into place on your computer: one GetMap image, or the tiles of the
  WMTS matrix nearest the map's detail (a coarser one where that would take more than 16 tiles;
  of a set of more than 40 matrices, its 20 coarsest and 20 finest are kept). A WMS layer is
  asked for in the projected system it lists first, else in CRS:84, else in another
  longitude/latitude system.
- **On a map in another projection** (**View › Projection**, `map.projection`) that the layer is
  offered in, a WMS layer is asked for in that system, as
  [one picture of the view](#one-picture-of-the-view-or-tiles) or, drawn as tiles, in tiles of its
  own 256 pixels a side, and drawn as the service draws it: nothing is warped, so its lines and labels stay sharp. A WMTS
  layer is drawn so on a map in its tile matrix set's system. Change the map's projection and the
  layer follows: in Web Mercator again, it is asked for in Web Mercator (or warped), and in a
  projection the layer isn't offered in, it is drawn warped as before. A WMS layer is drawn so in
  up to 64 of the systems it lists that RustGIS can transform (projected ones first, then
  CRS:84, then other longitude/latitude ones, each kind in the order listed), and warped in the
  rest. A tilted map draws every layer warped.
- **Identify** asks a WMS layer drawn in a system of its own (GetFeatureInfo) in that system.
- **Axis order.** A WMS 1.3.0 bounding box and a WMTS `TopLeftCorner` are in the order of the
  system's axes as EPSG defines them, as the standards ask: latitude first in EPSG:4326 and other
  longitude/latitude systems (not in CRS:84), north first in EPSG:3035 (LAEA Europe), the German
  Gauss-Krüger zones, SWEREF99 TM, Poland's CS92, Austria's Lambert and the ETRS89 "UTM (N-E)"
  codes (EPSG:3040 and its neighbours), east first in Web Mercator, the other UTM codes and LV95.
  RustGIS carries that order for the systems it knows (from the EPSG dataset); for a system newer
  than its table, the layer's own `BoundingBox` is read the way that holds the layer, and a WMTS
  corner the way its matrix covers the layer. A longitude/latitude WMTS corner, which services
  often write longitude first, is read longitude first when its first number can't be a
  latitude, or when its matrix read in EPSG's order misses the layer and read the other way
  doesn't; otherwise in EPSG's order.

A layer offered in no system RustGIS can transform (an `AUTO:` system, a planet's) is still left
out, and the dialog says how many were.

```sh
rustgis-cli run --cmd 'service.add={"url":"https://wmts.geo.admin.ch/EPSG/2056/1.0.0/WMTSCapabilities.xml","layer":"ch.swisstopo.pixelkarte-farbe"}' --cmd 'map.projection={"type":"EPSG:2056"}' --cmd 'map.goToXY={"lon":7.4474,"lat":46.948,"scale":20000}' --cmd 'share.exportMap={"path":"bern-lv95.png"}'
```

### Secured ArcGIS services

A secured ArcGIS service answers error 499, "Token Required". There are two ways in:

- **Sign in to ArcGIS** (in Add From Service, shown for ArcGIS addresses; `arcgis.signIn`) signs in
  to ArcGIS Online, an ArcGIS Online organization (`https://myorg.maps.arcgis.com`) or an ArcGIS
  Enterprise portal (`https://gis.example.org/portal`) on the portal's own page, with its single
  sign-on and multi-factor checks: RustGIS never sees the password. It uses OAuth 2.0 with PKCE.
  ArcGIS Online needs no setup: leave **Client ID** blank and RustGIS's own app is used. An
  Enterprise portal needs the client ID of an app registered on it (**Content › New item ›
  Developer credentials › OAuth 2.0 credentials**) whose redirect URLs list RustGIS's callback:
  `http://127.0.0.1:7290/arcgis/callback` for the desktop app and the command line (give
  `"port"` to `arcgis.signIn` for another port, and register that address instead), and
  `oauth-callback.html` next to the web app's page for the browser (for example
  `https://web.rustgis.app/oauth-callback.html`), where the sign-in opens in a pop-up window.
  ArcGIS accepts only an address the app registers, port and path included. The portal and
  client ID are remembered; the session is not.
- **A token with the address**: a service address given with `?token=…` (from the portal, or
  `arcgis.token` with `"url"` and `"token"`) keeps the token for that ArcGIS Server and takes it
  out of the address the layer keeps.

A token is sent (in the `X-Esri-Authorization` header; in the browser, as the request's `token`
parameter, since ArcGIS's query endpoints refuse the header from web pages) only where its session
reaches: the portal
and, for ArcGIS Online, the organization's own hosted services (`services*.arcgis.com/<org id>/…`,
`tiles.arcgis.com/tiles/<org id>/…`), or, for ArcGIS Enterprise, the portal's host and its
federated servers; a token given with an address goes to that server alone. Its layers, tiles and
feature pages are then read with it. Tokens live in memory for the session, never in a project or
a connection, and a sign-in renews itself until the portal's session ends (two weeks at most).
**Sign out** (`arcgis.signOut`) forgets it (and, on the desktop, revokes it with the portal); `arcgis.status` lists
the sessions.

### Catalogues

The same dialog's **Catalogue search (CSW)** type searches a CSW 2.0.2 catalogue (GeoNetwork,
pycsw, geocat.ch…) by words anywhere in its records and, if you like, the map's area
(`catalog.search`). Its records are listed 20 at a time (**More** asks for the next), with their
titles, abstracts (on hover) and bounds (**Zoom**), and a button per link RustGIS can follow: a WMS,
WMTS, WFS, WCS, OGC API or ArcGIS service connects in the dialog, a download is added as From URL…
does. Links pointing where a document on the web may not send RustGIS (the local network) are left
out.

## PostgreSQL and PostGIS

The desktop app reads the geometry and geography columns of a PostGIS database live, talking to the
server itself: see [PostgreSQL and PostGIS](data.md#postgresql-and-postgis).

| Source | How to add it | Notes |
| --- | --- | --- |
| PostGIS table or view | Home › Add › Add Data › Database… (`layer.addDatabase`) with a libpq connection string or `postgresql://` address | Large tables read a view at a time by their spatial index, others loaded whole. Converted to longitude/latitude by the server; `"crs"` for geometry that doesn't say. A column of mixed types becomes a layer per type. The password is kept for the session only. |

## MySQL and MariaDB

The desktop app reads the spatial columns of a MySQL (8 or later) or MariaDB (10.2 or later)
database live, talking to the server itself: see [MySQL and MariaDB](data.md#mysql-and-mariadb).

| Source | How to add it | Notes |
| --- | --- | --- |
| MySQL or MariaDB table or view | Home › Add › Add Data › Database… (`layer.addDatabase`) with a `mysql://user@host:3306/database` or `mariadb://…` address | Large tables read a view at a time by their spatial index, others loaded whole. Converted to longitude/latitude by RustGIS from the column's SRID; `"crs"` for an SRID of 0. A column of mixed types becomes a layer per type. `ssl-mode` sets the encryption. The password is kept for the session only. |

## SQL Server and Azure SQL

The desktop app reads SQL Server 2012 or later and Azure SQL spatial tables live, without an
external client: see [SQL Server and Azure SQL](data.md#sql-server-and-azure-sql).

| Source | How to add it | Notes |
| --- | --- | --- |
| SQL Server table or view | Home › Add › Add Data › Database… (`layer.addDatabase`) with an `mssql://user@host:1433/database` address | Read only; large tables read a view at a time. Geometry and geography, including curves, are converted to longitude/latitude. `"crs"` supplies SRID 0; mixed types become separate layers. TLS and certificate checking are on by default. SQL Server logins on every desktop platform; Windows logins on Windows only. Passwords are kept for the session, never in projects. |

## SQL queries and DuckDB databases

With [DuckDB](https://duckdb.org) installed (the `duckdb` program on the `PATH`, DuckDB 1.5 or
later), the desktop app adds the result of an SQL query as a layer, and opens DuckDB database files.
Results are written as cached GeoParquet, so large ones stream too: see [SQL queries and DuckDB
databases](data.md#sql-queries-and-duckdb-databases).

| Source | How to add it | Notes |
| --- | --- | --- |
| SQL query | `layer.addSql` with `"sql"` | One DuckDB `SELECT` with DuckDB Spatial: Parquet, CSV, GeoParquet and `ST_Read` files, web addresses (`https://`, `s3://`). Give `"crs"` for a geometry that doesn't say its coordinate system. |
| DuckDB database | `.duckdb`, `.ddb`: Add Data | Every table with a geometry column becomes a layer (`"layer"` picks one). Converted once, until the file changes. |

## Rasters

| Format | Extensions or source | How to add it | Notes |
| --- | --- | --- | --- |
| GeoTIFF / Cloud-optimised GeoTIFF | `.tif`, `.tiff` | Add Data, or upload | Regular or cloud-optimised, striped or tiled; grey, RGB(A), palette (with its colour table) or multiband; Deflate, LZW, PackBits, JPEG or Zstandard compression. Supported EPSG systems or the file's own GeoKey projection are reprojected as it draws. Rotated or skewed images are resampled north-up. Large files open from an overview, or one made while reading. Browser uploads over 256 MiB read only the parts needed for a fitting overview, provided those parts fit the upload limit; the project keeps those parts, not the full-resolution file. |
| JPEG 2000 | `.jp2`, `.j2k` | Add Data, byte upload (`layer.addDataBytes`), or From URL… (desktop, with [GDAL](https://gdal.org) installed) | Converted through GDAL. Uploads and downloads keep the converted GeoTIFF in the project, so reopening it needs neither GDAL nor the original file or address; their output and workspace limits are described under [Files on the web](#files-on-the-web). The web app rejects JPEG 2000 uploads and downloads with an explicit GDAL limitation: use GeoTIFF instead. |
| Scans with no coordinates | an untagged `.tif` or `.png`; any `.tif`, `.png` or `.jpg`/`.jpeg` with `"as": "scan"` | Add Data, or upload (`layer.addData`, `layer.addDataBytes`) | Grey or RGB(A), 8- or 16-bit pixels. An untagged TIFF or PNG opens as a scan by default; a JPEG needs `"as": "scan"` (otherwise it is a photo). This also discards a GeoTIFF's coordinates. The scan initially covers the current view and is marked not georeferenced (`georeferenced: false`) until **Georeference** places it. Export and tools needing ground coordinates refuse it until then. PNG/JPEG decoding is bounded and may reduce the image; large TIFFs can be read at reduced resolution. |
| ASCII grid | `.asc` (with an optional `.prj` on desktop) | Add Data, or upload | A single numeric band with square `CELLSIZE` or separate `DX` and `DY` pixel sizes. Desktop files use the neighbouring `.prj`; uploaded and downloaded grids have no sidecar and must have coordinates that fit WGS 84 longitude/latitude. |
| Zarr | a store's folder (`.zarr`, desktop) or web address (desktop and browser), format 2 (consolidated) or 3 | Home › Add › Add Data › Zarr… (`layer.addZarr`), Add Data on a local folder, or a `?zarr=` link | A variable's latitude/longitude (or y/x) map at the steps picked of its other dimensions (time, level), read chunk by chunk in pure Rust (blosc, zlib, gzip, zstd, LZ4; sharded arrays too). See [Zarr stores](data.md#zarr-stores). |
| Rasters GDAL reads | `.img`, `.hgt`, `.dem`, `.dt0`–`.dt2`, `.nc`, `.nc4`, `.hdf`, `.hdf5`, `.h5`, `.he5`, `.bil`, `.bsq`, `.bip`, `.envi`, `.grd`, `.rst`, `.sdat`, `.kea`, `.ers`, `.ntf`, `.nitf`, `.ecw`, `.sid`, `.gsb`, `.bt`, `.rsw` | Add Data (desktop, with [GDAL](https://gdal.org): `gdal_translate`, `gdalinfo` and `gdalwarp`; the installed build must include the driver) | Converted once to a cached cloud-optimised GeoTIFF. NetCDF/HDF variables are subdatasets: the first is added, the others named in a notice; `"layer"` picks one. Without a declared coordinate system, longitude/latitude-like coordinates use WGS 84 (0–360° longitudes are moved to −180–180°); otherwise give `"crs"`. JPEG 2000 is listed separately above. |

Pixels are held as 32-bit floats: whole numbers are exact up to ±16,777,216 and decimals to about 7
significant digits. A file with values past that (Int32 or UInt32 IDs above 16,777,216, Int64,
Float64 with more digits, long decimals in an ASCII grid) opens with a notice counting the values that
were rounded, and Export Raster, which writes the rounded values, says so again.

## Tiles and web services

| Format | Extensions or source | How to add it | Notes |
| --- | --- | --- | --- |
| Raster tiles | XYZ/TMS template (`{z}/{x}/{y}`, `{-y}`, `{s}`, `{q}`), WMS (`{bbox-epsg-3857}`) | Home › Add › Basemap › Add Tile Layer | PNG, JPEG and WebP. |
| Vector tiles | Mapbox Vector Tiles (`.pbf`, `.mvt`) template, or TileJSON | Add Tile Layer | Drawn in a built-in style that knows the OpenMapTiles, Protomaps and Shortbread schemas. |
| MapLibre styles | `style.json` | Add Tile Layer | A supported subset of vector and raster sources, sprites, expressions and zoom functions; see [Maps and Basemaps](maps.md#basemaps) for styling limits. |
| PMTiles | `.pmtiles`: a web address, or a desktop file | Add Tile Layer (or **Tile Archive File…**), Add Data, or drop a local file on desktop | Version 3 archives only: PNG, JPEG or WebP rasters, or MVT vectors; uncompressed, gzip, Brotli or Zstandard. Read a tile at a time by byte ranges (CORS on the web). Browser local-file uploads aren't supported; use an archive's web address. The map moves to the archive's bounds, except a world archive. Vector archives use the built-in basemap style, or a colour per layer when added as an overlay. |
| MBTiles | `.mbtiles`: a file | Add Data, drop the file, Add Tile Layer's **Tile Archive File…** button, or upload in the web app | Raster (PNG, JPEG, WebP) or vector tiles, as its metadata says, drawn as PMTiles archives are. The desktop reads the file in place, a tile at a time. The web app reads an upload into memory (the plain `tiles` table and the deduplicated layouts of mbutil, TileMill and Planetiler) and keeps it for the session: a project saved there asks for the file again. |

## Writing

| Format | How to write it | Notes |
| --- | --- | --- |
| GeoJSON (`.geojson`, `.json`), CSV | Data › Export (`share.exportLayer`) | The Export Features dialog picks the format; the chosen extension is appended when needed. In the web app it downloads any vector format below except GeoPackage and FlatGeobuf (a Shapefile as one zip). A streamed layer exports its whole source: on the desktop, to GeoJSON, CSV, FlatGeobuf, GeoPackage or GeoParquet a chunk at a time with no limit on its size (a GeoParquet source's row groups are read whole, so on the desktop each holds at most 16,777,216 rows), otherwise loaded whole (see [Streamed layers](data.md)); remote sources on the web must be downloaded first. |
| GeoPackage | Data › Export to `.gpkg` (desktop) | A new file with one feature table (an attributes table for a layer without geometry) in the layer's coordinate system (see below), with an R-tree and GeoPackage 1.4 triggers to maintain it. An existing destination is refused; adding or replacing tables in an existing GeoPackage is not supported. Unique feature ids that fit signed 64-bit integers are kept; otherwise SQLite assigns new ids. |
| GeoParquet (`.parquet`, `.geoparquet`) | Data › Export | GeoParquet 1.1: WKB geometry in WGS 84, Snappy compression, row groups of up to 65,536 features and a `bbox` covering column. A standalone table without geometry writes plain Parquet. |
| Shapefile | Data › Export to `.shp` (or `.zip` for the files zipped) | Writes `.shp`, `.shx`, `.dbf`, `.prj` (the coordinate system, see below) and `.cpg` (UTF-8). Field names are cut to 10 bytes (made unique ignoring case, and reported), text to at most 254 bytes, and numbers that do not fit their dBase field are left empty. Export notices count shortened text, non-null values left empty (including incompatible types), and numbers rounded to the field's precision, and name the affected source fields; features without geometry, and lines of a single point, are left out (and reported). |
| FlatGeobuf | Data › Export to `.fgb` | In the layer's coordinate system (see below), with a spatial index; features without geometry are left out (and reported). Desktop only: the writer needs a temporary file, which the browser doesn't have. |
| KML, KMZ, GPX | Data › Export to `.kml`, `.kmz` or `.gpx` | Placemarks named by a `name` field, if there is one; features without geometry are left out. |
| GeoJSON, KML, GPX, CSV, CSV with WKT | Analyze › Workflows › Data Interop (`data.convert`; `"format": "geojson"`, `"kml"`, `"gpx"`, `"csv"` or `"wkt"`) | Returns text when `"path"` is omitted; writes a file on desktop when it is given. |
| GeoTIFF / Cloud-optimised GeoTIFF | Data › Export Raster (`share.exportRaster`) | Untiled GeoTIFF by default; `"cog": true` writes 512-pixel tiles, Deflate compression and overviews. Writes the loaded raster's resolution (an overview when that is what was opened), the selected band or its colour bands. The web app's Export Raster downloads an ordinary GeoTIFF. |
| PNG | Publish › Export Map (`share.exportMap`) | Configurable DPI, with labels, scale bar and tile credits. |
| PDF | Layouts, reports and presentations | |
| Wavefront OBJ, glTF | Publish › Export To 3D, Share As › Web Scene | Polygons extruded by a field. |
| Web map, OGC API Features, MapLibre style | Publish › Share As | Open-format files that need no server: see [Sharing](sharing.md). |
| Raster tiles | `imagery.hostedImagery` | A zip of `tiles/z/x/y.png` tiles, `tiles.json` and a viewer page; at most 5,000 tiles and 256 MiB of tile data. |

GeoPackage, Shapefile and FlatGeobuf exports default to the layer's original coordinate system
when it has an EPSG code. Shapefiles can also retain a custom WKT system; GeoPackage and FlatGeobuf
need an EPSG code and otherwise default to WGS 84. A layer made in RustGIS defaults to WGS 84.
`share.exportLayer` takes `"crs"` to choose
another, such as `{"layer": "Parcels", "path": "parcels.gpkg", "crs": "EPSG:4647"}`. GeoJSON, KML,
GPX, GeoParquet and CSV are always written in WGS 84.

RustGIS keeps x and y only. When a source has Z or M values (heights or measures), adding it says
they were dropped, and exporting or converting the layer (or exporting a standalone table that
holds geometry), or sharing it as a web map, web layer or OGC API Features, says what's written
lacks them.
Shapefile, GeoPackage, GeoParquet, FlatGeobuf, PostGIS, File Geodatabase, GeoJSON, KML (altitudes
other than 0), GML and CSV WKT columns are checked; sources read through GDAL or DuckDB, and SQL
Server tables, aren't yet. GPX elevations and LiDAR heights are kept as fields, and a File
Geodatabase multipatch's heights as `z_min` and `z_max`. Where a source doesn't declare its
dimensions, only a sample is checked: the first 16 rows of a GeoParquet file without geometry types
(in its first row group), and the first 16 rows with a geometry of a GeoPackage table with optional
Z or M (in the web app, its first 16 rows) or of an unconstrained PostGIS column.

Exporting, converting or sharing a layer also repeats what adding it said about its positions. A
coordinate system defined in a file whose datum has no shift to WGS 84 (a NAD 27 `.prj` without
`TOWGS84`, say) makes positions approximate, possibly a few hundred metres off, so an export in
another system says so; written back in that same system they are exact, and nothing is said.
Coordinates beyond ±180.5° longitude or ±90.5° latitude (a CSV of eastings and northings without its
`"crs"`, a Shapefile without its `.prj`) aren't longitude/latitude: an export says they're out of
place, and Define Projection, below, places them.

## Define Projection

When a Shapefile's `.prj` is missing or incorrect, right-click its layer and choose
**Define Projection…** (also in Command Search). Enter the **source** coordinate system, such
as `EPSG:2263` or a WKT/PROJ definition, then Apply. This reinterprets the native coordinate
numbers: it changes the layer's position, preserves attributes and selection, and can be undone
in one step. Use **Zoom To Layer** to see its corrected position. It does not modify the original
file. Saving the project keeps the definition. Shapefile exports use it by default;
GeoPackage and FlatGeobuf exports do so when the definition has an EPSG code. Custom
definitions without an EPSG code can be exported in their native coordinates as Shapefiles.

This differs from changing the map's coordinate system, which only changes the display, or
exporting to another CRS, which converts coordinate numbers while preserving locations.
RustGIS stores geometry in longitude/latitude, so Define Projection first reconstructs native
numbers through the previous CRS; small floating-point rounding differences are possible.
Edits already made to the layer are reinterpreted too. If the wrong CRS already discarded or
clamped coordinates, import the original file again to recover them accurately.

If the incorrect `.prj` prevented geometry from being read, Define Projection cannot restore
it. Choose **Import Original File…** in the dialog, or search for **Import Shapefile with
Projection…**. Enter the correct CRS before choosing the file. In the browser, pick the `.shp`
and `.dbf` together, with any sidecars, or choose a ZIP. The override replaces the `.prj` before
any conversion and applies to every Shapefile in a ZIP; other formats keep their own CRS.
This adds a new layer, so existing layer edits and styles are not transferred.

A streamed layer cannot be changed by Define Projection. Use **Load All Features** first (up
to the read limit), or import again with the override; streamed Shapefiles remember that
override when the project reopens. Layers whose import lost geometry must be imported again.

The same operations are available to scripts and the CLI:

```json
{"cmd":"layer.defineProjection","params":{"layer":"Parcels","crs":"EPSG:2263"}}
{"cmd":"layer.addData","params":{"path":"parcels.shp","crs":"EPSG:2263"}}
```

`layer.addDataBytes`, `layer.addUrl` and `layer.addDownload` also accept `crs` for zipped
Shapefiles. The source files and their `.prj` sidecars are never rewritten by either operation.

## RustGIS files

| File | Extension | Notes |
| --- | --- | --- |
| Project | `.rgis` (older `.rgisx` open too) | Saves streamed layers' local file paths relative to the project, not their features; a browser upload is kept inside. |
| Map, layer, style, color ramp, template, task, bookmark and layout files | `.rgismap`, `.rgislyr`, `.rgisstyle`, `.rgisramp`, `.rgistemplate`, `.rgistask`, `.rgisbookmarks`, `.rgislayout` | See [Project items](data.md#project-items). |
| Project package | `.rgispkg` | Everything, rasters included. |
| Plugin | `.rgplugin` | See [Plugins](../plugins.md). |
| Color ramps from other tools | QGIS style `.xml` and layer style `.qml`, GDAL color-relief `.txt`, GMT `.cpt` | Read as custom color ramps; see [Custom color ramps](cartography.md#color-ramps-from-other-tools). |
