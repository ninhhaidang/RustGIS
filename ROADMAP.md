# Roadmap

_Last reviewed: 2026-11-08 (v0.6.0). Reviewed weekly; if this date is old, trust the linked issues
over this page._

This page says where RustGIS is heading and where help is most useful. Details live in the linked
issues. RustGIS and GeoLibre complement each other
([#951](https://github.com/opengeos/RustGIS/discussions/951)): GeoLibre is web and mobile first,
RustGIS desktop first and built for automation and AI agents.
[GEOLIBRE_PARITY.md](GEOLIBRE_PARITY.md) compares the two feature by feature as a reference, not a
plan: not every GeoLibre feature suits RustGIS.

## Goals
- **What exists works.** A feature that crashes, misreads data or is undocumented costs more
  than a missing one. Robustness and correct coordinates come before breadth.
- **Finish the everyday workflow first.** Add data, style it, edit it, analyse it, print or share
  it: depth in these beats a long list of first versions.
- **Foundations before features.** Build the shared piece once (an expression engine, a secret
  store, Z coordinates) and let several features stand on it.
- **Free and self-contained.** Pure Rust with nothing else to install; GDAL and DuckDB are
  optional extras that add formats, not requirements.
- **One program everywhere.** Desktop, browser, CLI, Python and AI agents share the same commands;
  nothing below `apps/` breaks the WebAssembly build.

## Contributing
Most items below have no issue yet: propose one and open a pull request. See
[CONTRIBUTING.md](CONTRIBUTING.md) for which items to pick freely, which to coordinate first, and
how to write a proposal. Themes under Next are not releases and not in order; where an item
needs another first, it says so.

## Underway
An item is underway when it has an open pull request, or someone said on its issue in the last
week that they are working on it. Check here and the
[open pull requests](https://github.com/opengeos/RustGIS/pulls) before starting; to take an item,
comment on its issue or pull request and open a draft pull request early.
- GPU map: vector tiles and labels on the GPU tile layer
  ([#635](https://github.com/opengeos/RustGIS/issues/635),
  [#596](https://github.com/opengeos/RustGIS/issues/596)).
- Georeferencer: side-by-side window, transform picker, more formats
  ([#609](https://github.com/opengeos/RustGIS/issues/609)).

## Next

### Fixes and quick wins
The bugs users hit, the docs and a few quick wins: good first pull requests.
- Robustness: Control-click menus on macOS
  ([#707](https://github.com/opengeos/RustGIS/issues/707)), Load Off-Map Data
  ([#661](https://github.com/opengeos/RustGIS/issues/661)).
- A pass over the user guide, the comparison pages and the README against what the app does
  today, and short
  task tutorials for newcomers: buffer, select and export a list; edit a layer and save the
  result; a print layout _(open)_.

### Solid ground
Correct, robust and recoverable, with the foundations other themes stand on.
- Correct coordinates: grid datum shifts (NTv2 and GeoTIFF grids) for NAD27, NAD83(HARN),
  GDA94↔GDA2020 and OSGB36, and a notice whenever a layer is read without its shift; GDA94
  ([#730](https://github.com/opengeos/RustGIS/issues/730)) is one case. _Coordinate first._
- Reloading files changed on disk ([#1477](https://github.com/opengeos/RustGIS/issues/1477)):
  manual Reload Data for linked layers and local PMTiles/MBTiles archives; automatic detection and
  notifications remain open.
- Autosave and crash recovery in the browser: reopen unsaved work after a crash. The desktop app
  has them ([#873](https://github.com/opengeos/RustGIS/issues/873)).
- Signed Windows builds (no SmartScreen warning), then a signature check, by publisher name, in
  Check for Updates' Download and Install, which updates a `setup.exe` install in place
  ([#1290](https://github.com/opengeos/RustGIS/issues/1290)); in-place updates for the macOS app;
  settings an administrator can fix for a managed install (no AI Assistant, plugins or web
  services).
- Extend the OS keychain secret store to service connections (database passwords, S3 keys,
  AI provider keys and the ChatGPT sign-in's tokens are already kept there).

### Expressions and cartography
Next on top of the expression language:
- Expression labels: case, priority and number formats (text, size, colour and angle are done).
- Rule-based renderer: ordered rules with an expression, symbol and scale range, edited as such
  (QGIS's rules import as Unique Values by expression).
- Quick filters in the attribute table; pop-up templates (chosen fields, formats for dates, links
  and images, a title).

And alongside it:
- Kernel density (heat map) as a renderer; point clustering.
- SVG and PNG markers, image fill patterns and a symbol library.
- A legend and colour bar on the map. In layouts, legend entries reordered and renamed; several
  values grouped into one unique-value class.
- Map series at a fixed scale, with the current feature highlighted and its attributes in the
  page's text.
- Extend QGIS QML style import beyond the supported renderers; add SLD styles.
- A histogram for raster stretches; raster attribute tables.

### Data at the source
- GeoLibre opening RustGIS `.rgis` projects, once RustGIS's project format and supported
  data types settle. RustGIS imports GeoLibre projects; combined colour and size rendering
  is tracked in [#1017](https://github.com/opengeos/RustGIS/issues/1017).
- A date and date-time field type, read and written as dates (they are text today)
  ([#853](https://github.com/opengeos/RustGIS/issues/853)). _Coordinate first_ (it changes
  `FieldType`).
- Editable standalone tables (they are read-only today).
- Polygon editing: shared edges, where moving one boundary moves its neighbour's too.
  Coincident polygon vertices can already move together with Shared Vertices
  (`edit.moveSharedVertex`); different vertex densities and linked insertion/deletion remain
  open.
- Editor tracking fields (created/edited by and at).
- Attribute table columns: rename (hide and reorder are done) _(open)_.
- Persistent attribute joins and virtual fields. _Coordinate first_ (they change `Feature`).
- Attribute forms: widgets, constraints and conditional visibility, on top of domains and
  subtypes.
- Coded-value domains and subtypes read from File Geodatabases (their codes show raw today).
- Layer metadata, exported as STAC items.
- An ArcGIS Portal browser; Esri JSON for services that don't offer GeoJSON.

### Analysis
- Routing: isochrones, service areas and an origin–destination matrix (today shortest path over
  a line layer).
- Hot-spot statistics (Getis-Ord Gi\*), and emerging hot spots on the time slider.
- Charts linked to the selection: histogram, scatter, bar and box plots of a layer's fields.
- Reverse geocoding, and the US Census geocoder beside Nominatim and Photon; a DEM downloader so
  viewshed works without a loaded elevation raster _(open)_.

### 3D scenes, first steps
- Phases 1–2 of [#726](https://github.com/opengeos/RustGIS/issues/726): Z coordinates kept from
  the data and written back; a scene with a free camera and each layer's elevation mode (on the
  ground, relative, absolute).
- LiDAR point layers (LAS/LAZ/COPC, read today as points) drawn at their heights in the scene.
- 3D objects and 3D Tiles follow.

### Self-contained, any time _(open)_
Built in means no runner download, the selection honoured, geodesic distances and attributes
kept. Before adding a tool, check whether the GeoLibre suite already has it (`gp.tools`); a
built-in version must justify itself on one of those.
- UTM and MGRS grids drawn on the map (the readouts are done).
- H3 grids built in, next to the A5 grids (the GeoLibre suite has H3 tools only when its runner
  is installed).
- An on-map right-click menu: copy the coordinate, run a buffer or viewshed from the point.
- More than two synced maps side by side.
- Auto-refresh for web service layers.

## Exploratory
Open questions rather than plans; discuss before building.
- Layers kept in their own coordinate system rather than WGS 84.
- Integrating [opengeos/terrio](https://github.com/opengeos/terrio), the pure-Rust raster, vector
  and point cloud I/O library: which of its drivers, coordinate transformations and GDAL-style
  operations could back or replace RustGIS's own readers, or stand in for the GDAL program, and
  which of them build for the web app.
- Native NetCDF-4/HDF5 without GDAL ([#396](https://github.com/opengeos/RustGIS/issues/396)),
  kerchunk and Icechunk.
- Broader QGIS project compatibility and opening ArcGIS Pro projects (supported QGIS projects
  already open as maps).
- An in-app Python console; an R package.

## Done

- Customizable keyboard shortcuts with portable JSON import/export
  ([#562](https://github.com/opengeos/RustGIS/issues/562)).
- Save edits back to existing GeoPackage and PostGIS tables on desktop, with atomic saves,
  conflict detection and discard/reload
  ([#1041](https://github.com/opengeos/RustGIS/issues/1041)).
- Projects that survive a move: local data paths relative to the project and undoable Repair Data
  Source for layers and tile archives ([#879](https://github.com/opengeos/RustGIS/issues/879)).
- Layered workspace, command registry with undo, project files (`.rgis`), `rustgis-cli`, the web
  app (WebAssembly, installable with offline start, opened with a project or data by its web
  address, self-hostable with the `Dockerfile`), 21 interface languages, custom themes as JSON
  files and an interface scale; a ribbon laid out in the order of the work (Home, Data, Style,
  Edit, Analyze, View, Publish), start-up preferences, Check for Updates (with Download and
  Install for Windows `setup.exe` installs), autosave and crash recovery in the desktop app, a
  native macOS menu bar, a History pane of tool runs and the session log (filtered, saved as CSV
  or JSON Lines); attribute table columns hidden and reordered; files, rasters and projects too
  large for the machine refused with a message, the memory limits following the computer's
  memory.
- Data: files by their web address; GeoJSON, CSV/TSV (x/y in any EPSG system, or WKT), Excel and
  OpenDocument spreadsheets, GeoRSS, LandXML, encoded polylines, Shapefile (zipped too, with the
  coordinate system in its `.prj`), GeoPackage, GeoParquet, FlatGeobuf, KML/KMZ (styles
  included), GPX, GML, MapInfo MIF, OSM PBF, TopoJSON, DXF, CityJSON (footprints with their
  heights) and LiDAR (LAS/LAZ/COPC) in, a Shapefile by its `.shp` address, all of them in the web
  app too (large files read in place, a view at a time, and Shapefiles, GeoPackage, FlatGeobuf
  and GeoParquet a chunk at a time); Esri File Geodatabases without GDAL (desktop; 3D
  multipatches as footprints with their heights); the vector formats GDAL reads when it is
  installed, PostGIS, MySQL, MariaDB and SQL Server tables read live (desktop), and SQL queries
  and DuckDB databases (DuckDB Spatial) when DuckDB is; GeoJSON, CSV, GeoPackage (also as a table
  added to or replacing one in an existing file), GeoParquet, Shapefile, FlatGeobuf, KML/KMZ, GPX
  and WKT out; standalone tables; geotagged photos and KML ground overlays; geocoding (Find
  Place, address tables; PDOK Locatieserver); a Data Sources pane (favourites, recent, saved
  services, a library, reusable connections and dataset browsing) and an SQL pane (DuckDB
  Spatial); layer, map, style, template, task and package files; supported QGIS `.qgs`/`.qgz` and
  GeoLibre `.geolibre` projects opened as maps, with supported layer styles and notes on import
  differences.
- Open data: STAC search (Planetary Computer assets), NASA Earthdata (desktop), Hugging Face,
  Overture Maps, Source Cooperative, OpenStreetMap by area (Overpass), geoBoundaries, CKAN,
  Socrata and ArcGIS Hub portals, S3 buckets with signed connections and a bucket browser, and a
  Data Cubes pane with a catalog of cubes for the time slider, static collections laid out by
  date (JAXA Earth) among them.
- Rasters: GeoTIFF/COG (by web address, read in place by range as raster layers, or a window of
  one by bounding box; Zstandard too) and ASCII grids, Zarr 2 and 3 stores (a variable's step, by
  folder or web address), and JPEG 2000, NetCDF, HDF, ERDAS Imagine… through GDAL, EPSG
  reprojection, stretch/RGB/classified/shaded-relief symbology, raster functions, indices, change
  detection, classification, georeferencing; cloud-optimised GeoTIFFs out; data cubes on a time
  slider, point time-series charts, temporal and categorical summaries, and trends (Sen's slope,
  Mann–Kendall), including STAC bands read in place.
- Services: WMS, WMTS, WFS, WCS, OGC API Features and Tiles, and ArcGIS REST, listed from their
  capabilities (ArcGIS feature services in protocol buffer pages); CSW catalogue search; sign-in
  to ArcGIS Online and Enterprise (OAuth), basic-auth and API-key logins for a session; WMS and
  WMTS time on the time slider, legends, GetFeatureInfo and layers in other coordinate systems;
  feature services asked for an area only.
- Maps: multiple maps, a GPU renderer (beta) with smooth zoom and pan, tilt, rotation and
  terrain, a 3D globe with the flat map draped over it, maps of the Moon, Mars and other
  celestial bodies, projection presets and EPSG codes (sinusoidal, Robinson, orthographic and
  Mercator among them; features cut along the edge of world projections), a search box for
  layers, places and coordinates, UTM and MGRS coordinate readouts, bookmarks, web tile layers
  (XYZ/TMS/WMS, Mapbox Vector Tiles, MapLibre styles with their sprites and raster-dem hillshade
  and colour relief, PMTiles and MBTiles archives), a basemap gallery (OpenFreeMap, USGS
  Imagery), worldviews.
- Cartography: single, unique values, graduated colours (natural breaks, equal interval,
  quantile, geometric interval, standard deviation, manual, arithmetic progression, head/tail
  breaks), proportional symbols and pie and bar diagrams; Unique Values by expression; Invert
  Color Ramp; symbol size, colours, outline width and angle by expression; label text, size,
  colour and angle by expression; nested layer groups; label classes with SQL queries, managed in
  the Labeling pane; straight and curved line placement, offsets, backgrounds, halos, wrapping,
  repeats, other positions round a point and along a line, callouts, Show All Labels and a Move
  Label tool, installed font families and annotation; stacked and dashed line strokes, markers
  and hash marks along lines, stacked, shifted and turned point marks, font markers, hatch,
  marker, gradient and shapeburst polygon fills, symbol levels, reference scales, polygon
  outlines without fill; the full matplotlib colour maps; custom colour ramps (and QGIS, GDAL and
  GMT palette files); masking, display filters, blend modes, aggregation, extrusion; graphics.
- Merge vector layers: append matching geometry types, union attributes without type conversion,
  honor selections and complete streamed inputs
  ([#1061](https://github.com/opengeos/RustGIS/issues/1061)).
- Editing: create, move, rotate, scale, merge, split, explode, vertex editing, cut polygons,
  reshape and trace along an edge while drawing; snapping, templates, elevation, topology,
  coincident polygon vertices moved together; domains, subtypes, attribute rules, contingent
  values, relationships, archiving.
- Analysis: selection by attributes and location; polygon, lasso and radius on the map; overlay
  (clip, erase, split, intersect, identity, union, symmetrical difference, update), proximity
  (buffer, near) and summary tools (summary fields on Dissolve and Summarize Within), pivot
  tables, check and repair geometry, geometry conversions (polygon to line, split line at
  vertices, feature envelope to polygon, unsplit line), polygons burnt onto a grid, tabulate
  intersection (class areas and totals shared out to zones by overlap), spatial join by
  intersection, centre, largest overlap, closest and distance with merge rules, join field
  (non-spatial attribute join), feature vertices to points, points along lines, zonal statistics,
  kernel density, A5 grids, kriging, Calculate Field with an expression language, Calculate
  Geometry with metric or imperial units, Project Raster, and vector and raster PMTiles export
  (#557); guided workflows (viewshed, line of sight, IDW, shortest path, suitability); tools run
  in the background with progress and Cancel, many on several cores on the desktop, and in
  batches (`gp.batch`); measurements on the WGS 84 ellipsoid, in the attribute table, the measure tool
  and expressions;
  ModelBuilder on a node canvas, scripts with command, parameter and value completion, and
  history copied as code; the GeoLibre and Whitebox suite (1,000+ tools) natively and in the
  browser.
- Layouts (map frames, surrounds, map series, publication sheets and figures in one command,
  format panes, guides, snapping, element locking and order, measured grids, legends in up to
  three columns, publication arrangements, vector PDF and SVG, a page copied to the clipboard),
  reports, presentations and notebooks; the web app embeddable in a page (`?embed`).
- Sharing as open formats (web maps, OGC API Features collections, MapLibre styles, glTF and STL
  scenes, locators, the map view or a tile layer as a GeoTIFF placed on the ground), offline
  areas with sync, deep learning pixel classification, hosted imagery tiles.
- The AI Assistant (Anthropic, OpenAI, ChatGPT sign-in, Gemini, OpenRouter, Requesty,
  TrustedRouter, Ollama, OpenAI-compatible) in the app, the web app and `rustgis-cli ask`; an MCP
  server (`rustgis-cli mcp`) for AI agents such as Claude Code, Cursor and VS Code, with the
  documentation built in.
- The AI Assistant's skills, subagents and ACP clients; MCP over HTTP, an audited loopback relay
  and a file bridge, which let external AI clients drive the app. Database passwords, S3 keys, AI
  provider keys and the Earthdata Login token optionally remembered in the desktop OS keychain,
  where the ChatGPT sign-in keeps its tokens.
- Sandboxed WebAssembly plugins with a registry at plugins.rustgis.app; process plugins that run
  as programs (with Python environments RustGIS makes), in the background with progress and
  Cancel; panels with tree widgets, and compact ribbon columns and nested menus.
- The `rustgis` Python package on PyPI: the engine in-process, `rustgis.Map` in Jupyter and
  `rustgis.qt.MapWidget` in Qt applications.
- Desktop packages: Windows installers (setup.exe, .msi), a portable zip and a Store MSIX; a
  signed and notarized macOS DMG; Linux .deb, .rpm, AppImage and tarballs; Homebrew, the AUR,
  Fedora COPR and winget; a download mirror at downloads.rustgis.app; native Windows on ARM64
  builds; web app and command line images on ghcr.io and a static web app archive.
