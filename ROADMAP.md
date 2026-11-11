# Roadmap

_Last reviewed: 2026-11-10 (v0.6.0). Reviewed at each release; if this date is old, trust the
linked issues over this page._

This page lists the work planned for RustGIS and where help is most useful; details of each item
live in its linked issue. Gaps nobody has planned are listed in
[GEOLIBRE_PARITY.md](GEOLIBRE_PARITY.md) without a theme.

## Goals
- **What exists works.** A feature that crashes, misreads data or is undocumented costs more
  than a missing one. Robustness and correct coordinates come before breadth, and a result that
  loses or approximates data says so ([AGENTS.md](AGENTS.md): Never crash, Never silent).
- **Finish the everyday workflow first.** Add data, style it, edit it, analyse it, print or share
  it: depth in these beats a long list of first versions.
- **Foundations before features.** Build the shared piece once (an expression engine, a secret
  store, Z coordinates) and let several features stand on it.
- **Work you saved keeps working.** A project, model, script or plugin made with one release opens
  in the next; a file from a newer release is refused or kept unchanged, never saved again with
  parts missing ([saved files](docs/maintaining.md#saved-files-and-settings)).
- **Free and self-contained.** Pure Rust with nothing else to install; GDAL and DuckDB are
  optional extras that add formats, not requirements.
- **One program everywhere.** Desktop, browser, CLI, Python and AI agents share the same commands;
  nothing below `apps/` breaks the WebAssembly build. On the desktop, long work never holds the
  window ([#1562](https://github.com/opengeos/RustGIS/issues/1562)).

## How to use this page
Themes are not releases and not in order; where an item needs another first, it says so. See
[CONTRIBUTING.md](CONTRIBUTING.md) for how to propose work, and
[docs/maintaining.md](docs/maintaining.md) for who approves what. Items carry one of three
markers, or none:
- _(open)_: self-contained; open a pull request directly.
- _Coordinate first_: propose on the linked issue and wait for a maintainer's reply before writing
  much code.
- _Decide first_: the project hasn't settled the design, and the decision is one
  [docs/maintaining.md](docs/maintaining.md#who-approves-a-coordinate-first-change) reserves for
  the project lead (core types, how a project stores its data, the plugin ABI). Join in
  where it is linked (an issue or a discussion); don't start code until the decision is recorded
  there.

An item marked _Coordinate first_ or _Decide first_ links the issue or discussion where it is
decided, or says an issue is needed; opening that issue is the first step. A theme's tracking issue
holds its details and progress; this page keeps one line per item. A pull request that lands an item
removes it from this page, or narrows it to what is left, in the same change.

Before proposing something, check that RustGIS doesn't already do it: the
[README's feature table](README.md#features), the [user guide](docs/user-guide/index.md), and the
live lists of commands and geoprocessing tools (`rustgis-cli commands`, `rustgis-cli tools`; over
MCP, `search_commands` and `search_tools`).

[GEOLIBRE_PARITY.md](GEOLIBRE_PARITY.md) compares RustGIS with GeoLibre feature by feature: a
source of ideas, not a plan ([#951](https://github.com/opengeos/RustGIS/discussions/951)). Issues
labelled [good first issue](https://github.com/opengeos/RustGIS/labels/good%20first%20issue) are a
place to start.

## Underway
An item is underway when it has an open pull request, or someone said on its issue or discussion
in the last week that they are working on it. The
[open pull requests](https://github.com/opengeos/RustGIS/pulls) are the live list; check them, and
the item's issue, before starting. To take an item, comment on its
issue if it has one, and open a draft pull request early. Design work without a pull request yet:
- How a project stores its data: a prototype (posted 2026-11-09) and the first steps
  ([discussion #1378](https://github.com/opengeos/RustGIS/discussions/1378),
  [poll #1660](https://github.com/opengeos/RustGIS/discussions/1660)).

## Foundations
The shared pieces the everyday workflow stands on.

### Correct results
A result is right, or RustGIS says what it lost or assumed
([AGENTS.md](AGENTS.md#never-silent)).
- Known silent losses: a polygon looping round a pole drops out of Clip and Spatial Join
  ([#1354](https://github.com/opengeos/RustGIS/issues/1354)); DuckDB queries (`ST_Force2D`) and
  the TopoJSON reader drop Z without a notice, and the DXF reader keeps one `elevation` per entity
  but not each vertex's Z (other readers, GDAL and SQL Server among them, say so since
  [#1214](https://github.com/opengeos/RustGIS/pull/1214)); the KML and OSM readers leave out
  unreadable coordinates, holes and parts without one, and GeoRSS its unreadable holes (GML says
  so since [#1686](https://github.com/opengeos/RustGIS/pull/1686)) _(open)_.
- A test that runs lossy sample files through every reader and writer and fails on any loss
  without a notice, beside `hostile_params_never_panic` in `crates/engine/src/tests.rs` _(open)_.
- A `--strict` option for `rustgis-cli run` that exits non-zero when a loss is reported, for
  scheduled jobs. Notices carry a severity but not a kind today (`Notice` in
  `crates/engine/src/lib.rs`): start with an issue on which notices count as a loss.
- Coordinate systems read wrongly from WKT: grad-based systems
  ([#1692](https://github.com/opengeos/RustGIS/issues/1692)), anything named Web Mercator
  ([#1699](https://github.com/opengeos/RustGIS/issues/1699)); systems not read at all: Natural
  Earth, Winkel Tripel and Eckert IV ([#1701](https://github.com/opengeos/RustGIS/issues/1701)).
- Grid datum shifts (NTv2 and GeoTIFF grids) for NAD27, NAD83(HARN), GDA94↔GDA2020 and OSGB36
  without the GDAL program, a notice whenever a layer is read without its shift, and the shifts
  named in GeoPackage, File Geodatabase and WKT2 definitions kept
  ([#1677](https://github.com/opengeos/RustGIS/issues/1677),
  [#1678](https://github.com/opengeos/RustGIS/issues/1678)). _Coordinate first._
- Data without a coordinate system: one written rule per format (assumed with a notice, or
  refused), tested. Custom coordinate systems saved by name
  ([#1702](https://github.com/opengeos/RustGIS/issues/1702)).
- A Known Limitations page that collects every limit in one place: memory budget, feature
  counts, sampled drawing, what the browser refuses, missing datum grids _(open)_.

### Projects, saved files and the data model
Projects that open whatever their size, saved work that keeps working from release to release
([#1405](https://github.com/opengeos/RustGIS/issues/1405),
[discussion #1378](https://github.com/opengeos/RustGIS/discussions/1378)), and the core types
features build on.
- No map, layer, layout or presentation file saved that RustGIS couldn't reopen (projects
  already are refused, [#1364](https://github.com/opengeos/RustGIS/pull/1364)); packages
  (`.rgispkg`) that hold everything or say what stayed linked; projects written to disk as they
  save, not built in memory first.
- A JSON Schema for `.rgis` with fixture files and a CI check that fails when the format drifts;
  a project saved by each release kept as a test that must keep opening; a snapshot test of
  command ids and parameters, so a removal is always deliberate.
- An older RustGIS keeps settings it can't read (connections, favourites, plugin and account
  data); the GeoLibre runner pinned to versions known to work, with a notice outside them
  ([#1405](https://github.com/opengeos/RustGIS/issues/1405) items 3 and 4).
- A compressed encoding of today's project JSON, read one release before it is written; a
  reference system: named connections, copies that remember their source, missing and changed
  states, a place for results ([#1653](https://github.com/opengeos/RustGIS/issues/1653)).
  _Coordinate first._
- What a project holds by default, its own data or links to it
  ([poll #1660](https://github.com/opengeos/RustGIS/discussions/1660)), and storage that opens a
  large project without reading it whole. _Decide first._
- Date and date-time field types ([#853](https://github.com/opengeos/RustGIS/issues/853); part 1 is
  a draft, [#1325](https://github.com/opengeos/RustGIS/pull/1325)), then read and written as dates
  by Shapefile and GeoPackage, the other files, and the databases, in that order; afterwards time
  zones (a time without a zone stays as written; where an instant is needed, an assumed zone is
  saved in the project, never written into the data) and recovering a project with an invalid date.
  _Decide first_ (field types).
- Z coordinates kept from the data and written back, Phase 1 of
  [#726](https://github.com/opengeos/RustGIS/issues/726). _Decide first_ (it changes `Coord`).
- Persistent attribute joins and virtual fields. _Decide first_ (they change `Feature`; needs an
  issue).

### Large data and speed
Big layers work, or say plainly where they stop
([#1111](https://github.com/opengeos/RustGIS/issues/1111),
[#1227](https://github.com/opengeos/RustGIS/issues/1227)).
- An adjustable memory budget, and the feature cap (2 million on the desktop, 500,000 in the
  browser) lifted for the sources that still have it: everything but local or uploaded GeoPackage,
  FlatGeobuf, GeoParquet and Shapefiles read in place, which are read a chunk at a time
  (`visits_chunks`). Remote FlatGeobuf and GeoParquet still stop at the cap when read whole.
- Shapefile, KML, KMZ and GPX exported a chunk at a time, as the other formats are.
- Faster raster writing: COG export at Deflate level 1 with a predictor (3 for floating-point
  samples, 2 for integers) and its tiles compressed on several cores (`cogwrite.rs` writes level
  6, no predictor, on one thread today); Hillshade written as Byte.
- Faster drawing: layers read in place read and projected once per draw, filtered by extent
  ([#1065](https://github.com/opengeos/RustGIS/issues/1065)); only the changed layer redrawn
  ([#1057](https://github.com/opengeos/RustGIS/issues/1057)); a heavy layer drawn on several
  cores ([#1067](https://github.com/opengeos/RustGIS/issues/1067)).
- More analysis tools on several cores
  ([#1252](https://github.com/opengeos/RustGIS/issues/1252)) and within the memory budget
  ([#1291](https://github.com/opengeos/RustGIS/issues/1291)), through the shared helpers named in
  [AGENTS.md](AGENTS.md#never-hold-the-window).
- Long work off the UI thread: what [#1562](https://github.com/opengeos/RustGIS/issues/1562)
  leaves open, such as a warning in debug builds when the UI thread is held too long.

### Platform, docs and security
- Signed Windows builds (no SmartScreen warning), then a signature check, by publisher name, in
  Check for Updates' Download and Install; in-place updates for the macOS app; settings an
  administrator can fix for a managed install (no AI Assistant, plugins or web services).
- Autosave and crash recovery in the browser, as the desktop app has since
  [#873](https://github.com/opengeos/RustGIS/issues/873) (`apps/rustgis-web/src/web.rs`; the
  browser half needs an issue of its own).
- The OS keychain for the remaining secrets: web-service basic-auth and API-key logins (for the
  session today), and a plugin `secrets` permission bound to one origin
  ([#760](https://github.com/opengeos/RustGIS/issues/760)); testing on each platform
  ([#891](https://github.com/opengeos/RustGIS/issues/891)).
- Waiting on upstream releases: Control-click menus on macOS
  ([#707](https://github.com/opengeos/RustGIS/issues/707)) and asking to save on ⌘Q
  ([#966](https://github.com/opengeos/RustGIS/issues/966)), both on winit; FlatGeobuf export in
  the web app, on a `flatgeobuf` crate release
  ([#1111](https://github.com/opengeos/RustGIS/issues/1111)).
- Docs: a pass over the user guide and the comparison pages against the app today, and short task
  tutorials for newcomers: buffer, select and export a list; edit a layer and save the result; a
  print layout _(open)_.

### Plugins
What plugin authors have asked for. An addition that plugins declare through
`minRustgisVersion` needs two maintainers ([docs/maintaining.md](docs/maintaining.md)).
- Panels: a text box ([#1260](https://github.com/opengeos/RustGIS/issues/1260)); trees past
  5,000 nodes, or a notice when cut ([#1546](https://github.com/opengeos/RustGIS/issues/1546)); a
  command that asks for its parameters in a dialog
  ([#1547](https://github.com/opengeos/RustGIS/issues/1547)).
- Host requests: settings kept between sessions
  ([#1261](https://github.com/opengeos/RustGIS/issues/1261)), `host.fetch` without the parsed
  copy ([#1265](https://github.com/opengeos/RustGIS/issues/1265)), a file or folder chosen by the
  user ([#1438](https://github.com/opengeos/RustGIS/issues/1438)); for session plugins,
  symbology and raster tile requests and a view in a page layout (asked on
  [#1518](https://github.com/opengeos/RustGIS/issues/1518), which session plugins closed).
- The WebAssembly plugin memory limit raised or made configurable
  ([#1357](https://github.com/opengeos/RustGIS/issues/1357)); commands that declare what they
  reach, instead of special cases by id ([#1316](https://github.com/opengeos/RustGIS/issues/1316)).
- Plugin versions ordered through prereleases, and an older RustGIS offered the newest release it
  can run ([#1405](https://github.com/opengeos/RustGIS/issues/1405) items 5 and 6). _Coordinate
  first_ (registry feed).

## The everyday workflow
In the order of the work: add data, style it, edit it, analyse it, lay it out and share it.

### Add data
- Georeferencer: side-by-side window, transform picker, more formats
  ([#609](https://github.com/opengeos/RustGIS/issues/609)).
- Noticing when a linked file or tile archive changes on disk, and offering to reload it (Reload
  Data does it by hand; [#1477](https://github.com/opengeos/RustGIS/issues/1477)).
- OGC API services that offer tilesets but no collections list them
  ([#1266](https://github.com/opengeos/RustGIS/issues/1266); waits on a maintainer's answer
  there).
- GeoTIFF transparency masks ([#1564](https://github.com/opengeos/RustGIS/issues/1564)).
- ArcGIS: browsing a signed-in portal's own content (Hub search, sign-in and adding by address
  work today); Esri JSON for layers that offer neither protocol buffers nor GeoJSON (older
  servers; GeoJSON is asked for whenever `supportedQueryFormats` lacks PBF, in
  `crates/data/src/ogc.rs`); large feature layers read a view at a time
  ([#829](https://github.com/opengeos/RustGIS/issues/829), to re-scope after the protocol buffer
  pages).
- Coded-value domains and subtypes read from File Geodatabases (their codes show raw today).
- Layer metadata, exported as STAC items.
- GeoLibre opening RustGIS `.rgis` projects, once the project format settles; combined colour and
  size rendering from GeoLibre projects ([#1017](https://github.com/opengeos/RustGIS/issues/1017)).
- Other worlds: higher-resolution planetary basemaps read directly
  ([#1447](https://github.com/opengeos/RustGIS/issues/1447)), the USGS planetary WMS layers drawn
  ([#1446](https://github.com/opengeos/RustGIS/issues/1446)), highlights and landing sites
  ([#1449](https://github.com/opengeos/RustGIS/issues/1449)); IAU coordinate systems beyond the
  first step ([#1448](https://github.com/opengeos/RustGIS/issues/1448)). _Decide first_ for
  #1448 (what dataset coordinates mean).

### Style and map
On top of the expression language:
- Dates in expressions: date functions and arithmetic, once the date types land
  ([#853](https://github.com/opengeos/RustGIS/issues/853)).
- Expression labels: case, priority and number formats (text, size, colour and angle are done).
- Rule-based renderer: ordered rules with an expression, symbol and scale range, edited as such
  (QGIS's rules import as Unique Values by expression).
- Quick filters in the attribute table; pop-up templates: chosen fields, a title and date formats
  (links and pictures are done).

And alongside it:
- Label placement: candidate positions, conflicts, obstacles and fitting
  ([#995](https://github.com/opengeos/RustGIS/issues/995)); line labels that try other places
  along the line ([#1187](https://github.com/opengeos/RustGIS/issues/1187)).
- Kernel density (heat map) as a renderer; point clustering.
- SVG and PNG markers, image fill patterns and a symbol library.
- A legend and colour bar on the map (layouts have both).
- Extend QGIS QML style import beyond the supported renderers
  ([#882](https://github.com/opengeos/RustGIS/issues/882)); add SLD styles.
- A histogram for raster stretches; raster attribute tables and legend labels from class names
  ([#889](https://github.com/opengeos/RustGIS/issues/889)).
- UTM and MGRS grids drawn on the map ([#1042](https://github.com/opengeos/RustGIS/issues/1042))
  _(open)_; more than two synced maps side by side _(open)_; auto-refresh for web service layers
  _(open)_.
- GPU map: vector tiles and labels on the GPU tile layer
  ([#635](https://github.com/opengeos/RustGIS/issues/635),
  [#596](https://github.com/opengeos/RustGIS/issues/596)).

### Edit
- Save edits back to Shapefile and GeoJSON layers, as GeoPackage and PostGIS do
  ([#1385](https://github.com/opengeos/RustGIS/issues/1385)). _Coordinate first._
- Editable standalone tables (they are read-only today).
- Shared edges, so that moving one polygon boundary moves its neighbour's too
  ([#872](https://github.com/opengeos/RustGIS/issues/872)). Coincident vertices already move
  together (`edit.moveSharedVertex`, with Shared Vertices on); boundaries with different vertex
  densities still separate, and linked insertion and deletion remain open.
- Traverse closure for `edit.traverse`: a misclosure report, an optional compass-rule adjustment,
  curve legs and a form.
- Editor tracking fields (created/edited by and at).
- Attribute table columns: rename (hide and reorder are done) _(open)_.
- Attribute forms: widgets, constraints and conditional visibility, on top of domains and
  subtypes.

### Analyse
Before adding a tool, check whether the GeoLibre suite already has it (`gp.tools`); a built-in
version must justify itself: no runner download, the selection honoured, geodesic distances and
attributes kept.
- Network analysis: service areas, isochrones (also from a point on the map, through its
  right-click menu) and an origin–destination matrix over a line layer, in a guided workflow over
  the GeoLibre suite's network tools, once their results are checked against known answers
  (today: shortest path over a line layer, and online Directions). Follow the
  [ribbon guidelines](docs/contributing.md#ribbon-guidelines) for where it goes.
- Hot-spot statistics (Getis-Ord Gi\*), and emerging hot spots on the time slider.
- Kriging with a fitted variogram and leave-one-out cross-validation (the variogram is a default
  estimated from the points and extent today, not fitted; `kriging.rs`).
- Charts linked to the selection: histogram, scatter, bar and box plots of a layer's fields.
- H3 grids built in, next to the A5 grids _(open)_.
- Reverse geocoding, and the US Census geocoder beside Nominatim and Photon; a DEM downloader so
  viewshed works without a loaded elevation raster _(open)_.
- Reference results for zonal statistics and the terrain tools, beside the existing analysis
  reference cases _(open)_.

### Lay out and share
- Layout legends: a label per layer ([#1654](https://github.com/opengeos/RustGIS/issues/1654)), a
  classified raster's classes ([#1679](https://github.com/opengeos/RustGIS/issues/1679)), entries
  reordered and renamed, several values grouped into one unique-value class.
- Map series at a fixed scale, with the current feature highlighted and its attributes in the
  page's text.

### 3D scenes
- A scene with a free camera and each layer's elevation mode (on the ground, relative, absolute),
  Phase 2 of [#726](https://github.com/opengeos/RustGIS/issues/726), after the Z design above.
- LiDAR point layers (LAS/LAZ/COPC, read today as points) drawn at their heights in the scene.
- Terrain from several layers ([#1310](https://github.com/opengeos/RustGIS/issues/1310)), and
  finer terrain once a tilted view settles
  ([#1523](https://github.com/opengeos/RustGIS/issues/1523)).
- 3D objects and 3D Tiles follow.

## Exploratory
Open questions rather than plans; discuss before building.
- Tools on layers bigger than memory: stream them, or cut the memory each feature takes
  ([#1111](https://github.com/opengeos/RustGIS/issues/1111)).
- Layers kept in their own coordinate system rather than WGS 84.
- Long work in the browser on Web Workers or WebAssembly threads (the desktop rule of
  [#1562](https://github.com/opengeos/RustGIS/issues/1562) leaves the browser out).
- Integrating [opengeos/terrio](https://github.com/opengeos/terrio), the pure-Rust raster, vector
  and point cloud I/O library: which of its drivers, coordinate transformations and GDAL-style
  operations could back or replace RustGIS's own readers, or stand in for the GDAL program, and
  which of them build for the web app ([#663](https://github.com/opengeos/RustGIS/issues/663)).
- Native NetCDF-4/HDF5 without GDAL ([#396](https://github.com/opengeos/RustGIS/issues/396)),
  kerchunk and Icechunk.
- Broader QGIS project compatibility and opening ArcGIS Pro projects (supported QGIS projects
  already open as maps).
- An in-app Python console, and notebooks whose Python cells run beside the map (`.ipynb` files
  open and save today, but their Python cells don't run;
  [#1056](https://github.com/opengeos/RustGIS/issues/1056)); an R package.
- Mesh layers for model results ([#1166](https://github.com/opengeos/RustGIS/issues/1166)).
- A server that GeoLibre and RustGIS can both use, once RustGIS is stable
  ([#951](https://github.com/opengeos/RustGIS/discussions/951)).

## Not planned for RustGIS
Left to GeoLibre, which is web and mobile first
([#951](https://github.com/opengeos/RustGIS/discussions/951)). Open a discussion if you think one
belongs here after all.
- Mobile and field data collection apps.
- Real-time collaborative editing.
- An SQL engine inside the web app (the desktop has DuckDB).
- Segmentation with SAM and other ONNX models.
