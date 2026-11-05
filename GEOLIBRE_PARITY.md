# Feature parity with GeoLibre

_Last reviewed: 2026-10-23 (v0.3.0)._

What [GeoLibre v3.4.0](https://github.com/opengeos/GeoLibre/tree/v3.4.0) offers and where RustGIS stands:
**missing** or **partial**, or ticked when done. It moved here from [#77](https://github.com/opengeos/RustGIS/issues/77) so that anyone can keep it
current: **tick or update an item in the pull request that lands it**. Where an item is planned,
_Roadmap_ names its theme in [ROADMAP.md](ROADMAP.md). The GeoLibre and Whitebox geoprocessing
suite RustGIS runs is not counted either way.

Sources: GeoLibre [v3.4.0](https://github.com/opengeos/GeoLibre/tree/v3.4.0) `docs/features.md`,
`docs/data-formats.md` and the other GeoLibre docs named per
section; RustGIS [ROADMAP.md](ROADMAP.md), [docs/](docs/) and the command registry
(`crates/engine/src/cmd`). Some items may be out of scope for a pure-Rust GIS (noted inline);
strike those through with a reason rather than leaving them open.

This list is a reference, not a plan: not every GeoLibre feature suits RustGIS. Some are left
to GeoLibre by decision ([#951](https://github.com/opengeos/RustGIS/discussions/951)): mobile
apps and field work, real-time collaboration and sharing, and AI segmentation are struck through
below with their reasons, and the Spatial SQL item notes that an SQL engine in the browser is
GeoLibre's too.

## Data formats & sources
_GeoLibre: docs/data-formats.md, docs/features.md › Adding data, SQL and databases_

- [x] **GeoPackage, GeoParquet, Shapefile (and KMZ) export** ([#92](https://github.com/opengeos/RustGIS/issues/92)): GeoLibre writes GeoJSON,
  GeoParquet, Shapefile, GeoPackage, CSV, KML and KMZ; RustGIS writes all of them, plus
  FlatGeobuf, GPX and WKT.
- [x] **Shapefile and GeoPackage in the web app** ([#94](https://github.com/opengeos/RustGIS/issues/94), [#95](https://github.com/opengeos/RustGIS/issues/95)).
- [ ] **Native KML/KMZ, GPX, GML, DXF/DWG, File Geodatabase, MapInfo and OSM PBF readers**:
  partial. KML/KMZ (styles and folders) and GPX (split into layers) are native on the desktop and
  the web since [#97](https://github.com/opengeos/RustGIS/issues/97); GML, MapInfo MIF/MID, OSM PBF and TopoJSON since [#122](https://github.com/opengeos/RustGIS/issues/122); KML GroundOverlays as
  raster layers since [#123](https://github.com/opengeos/RustGIS/issues/123); DXF (blocks, hatches, splines, text; a CAD layer picked with
  "layer") since [#159](https://github.com/opengeos/RustGIS/issues/159); Esri File Geodatabases without GDAL on the desktop (3D multipatches as
  footprints). Still missing: DWG (GDAL only) and File Geodatabase in the web app.
- [x] **Excel, LandXML, encoded polylines, GeoRSS** ([#118](https://github.com/opengeos/RustGIS/issues/118)).
- [x] **Delimited text with a projected CRS or address geocoding**: x/y columns in any coordinate
  system, WKT columns and any separator ([#115](https://github.com/opengeos/RustGIS/issues/115)); place search and address-table geocoding ([#127](https://github.com/opengeos/RustGIS/issues/127)).
- [x] **Remote vector and COG by URL**: Home › Add › Add Data › From URL ([#116](https://github.com/opengeos/RustGIS/issues/116)); COGs ([#132](https://github.com/opengeos/RustGIS/issues/132)), FlatGeobuf and
  GeoParquet ([#152](https://github.com/opengeos/RustGIS/issues/152)) read in place by HTTP range on the desktop and the web, from `s3://` too
  ([#150](https://github.com/opengeos/RustGIS/issues/150)).
- [x] **PMTiles and MBTiles** ([#74](https://github.com/opengeos/RustGIS/issues/74), [#113](https://github.com/opengeos/RustGIS/issues/113)): raster and vector archives, desktop and web.
- [ ] **Multidimensional rasters**: Zarr/Icechunk, NetCDF-4/HDF5 and kerchunk, with variable and
  dimension pickers, hyperspectral RGB and a 3D image cube. Partial: Zarr 2 and 3 stores with
  variable and dimension pickers and CF dates ([#147](https://github.com/opengeos/RustGIS/issues/147)), NetCDF/HDF variables through GDAL on the
  desktop ([#117](https://github.com/opengeos/RustGIS/issues/117)), data cubes on the time slider with trends. Still missing: native
  NetCDF-4/HDF5 ([#396](https://github.com/opengeos/RustGIS/issues/396), _Exploratory_), kerchunk, Icechunk, sharded Zarr 3, hyperspectral RGB and
  the 3D cube.
- [x] **GDAL raster formats beyond GeoTIFF and ASCII grid** ([#117](https://github.com/opengeos/RustGIS/issues/117), desktop).
- [ ] **OGC services**: partial. WMS, WMTS, WFS and OGC API Features from their capabilities
  ([#120](https://github.com/opengeos/RustGIS/issues/120)), OGC API Tiles ([#144](https://github.com/opengeos/RustGIS/issues/144)), WCS and CSW ([#178](https://github.com/opengeos/RustGIS/issues/178)). Still missing: auto-refresh (_Roadmap: any
  time_).
- [ ] **ArcGIS services**: FeatureServer, MapServer, ImageServer and VectorTileServer, with OAuth
  and a Portal browser. Partial: the four layer types through From Service… ([#120](https://github.com/opengeos/RustGIS/issues/120)); a token in
  the service address is sent with its requests (saved sources drop it); sign-in with OAuth to
  ArcGIS Online and Enterprise. Missing: a Portal browser (_Roadmap: data at the source_).
- [ ] **STAC browser and cloud catalogs**: STAC, Planetary Computer, Earth Engine, Overture,
  Source Cooperative, NASA Earthdata, Planet and Hugging Face. Partial: STAC Search ([#128](https://github.com/opengeos/RustGIS/issues/128)),
  Planetary Computer with signed assets ([#144](https://github.com/opengeos/RustGIS/issues/144)), Overture Maps and Source Cooperative ([#158](https://github.com/opengeos/RustGIS/issues/158)), NASA
  Earthdata (desktop) and Hugging Face. Missing: Earth Engine and Planet.
- [ ] **Open-data portal browsers**: partial. Overpass and geoBoundaries ([#139](https://github.com/opengeos/RustGIS/issues/139)), CKAN, Socrata and
  ArcGIS Hub ([#176](https://github.com/opengeos/RustGIS/issues/176)), four Natural Earth sample layers. Missing: more Natural Earth themes; a Web
  Data menu of global and per-country services ([#528](https://github.com/opengeos/RustGIS/issues/528)).
- [ ] **Database connections**: PostgreSQL/PostGIS, SQL Server/Azure SQL with write-back, Apache
  Iceberg. Partial: PostGIS ([#125](https://github.com/opengeos/RustGIS/issues/125)), MySQL, MariaDB and SQL Server tables read live on the desktop.
  Missing: write-back (_Roadmap: data at the source_) and Iceberg.
- [x] **Spatial SQL workspace** ([#131](https://github.com/opengeos/RustGIS/issues/131)): an SQL pane over DuckDB Spatial on the desktop. The web app
  has no in-browser engine: SQL in the browser is left to GeoLibre, which runs DuckDB, PGlite and
  Sedona there.
- [ ] **Private S3 buckets**: partial. `s3://` addresses, SigV4-signed connections, saved
  connections without their secrets and an S3 browser, desktop and web ([#150](https://github.com/opengeos/RustGIS/issues/150)). Missing: SSO, IAM
  roles and secrets kept in the keychain (_Roadmap: solid ground_).
- [ ] **3D and media data**: LiDAR (LAS/LAZ/COPC/EPT) as layers, 3D Tiles, I3S, Gaussian splats,
  placed glTF models, georeferenced video, geotagged photos, ICESat-2/GEDI. Partial: LAS, LAZ and
  COPC as point layers ([#137](https://github.com/opengeos/RustGIS/issues/137)), geotagged photos with thumbnails ([#175](https://github.com/opengeos/RustGIS/issues/175)); the rest is missing.
- [x] **Browser / Data Source Manager** ([#157](https://github.com/opengeos/RustGIS/issues/157)): a Data Sources pane with favourites, recents,
  folders, saved services, databases, S3 connections, STAC APIs and a library of public services.
- [x] **Data deep links** (`?data=`, `?style=`, `?tiles=` in [#121](https://github.com/opengeos/RustGIS/issues/121), `?stac=` in [#144](https://github.com/opengeos/RustGIS/issues/144)).

## Map rendering & 3D
_GeoLibre: docs/features.md › Map workspace, Rendering engines and 3D globe; docs/*-renderer.md_

- [ ] **3D terrain**: exaggeration, keyless Terrarium tiles, a custom DEM/COG terrain source,
  Cesium World Terrain. Partial: a globe, 2.5D extrusion, and tilt, rotation and terrain (from a
  DEM or elevation tiles, [#395](https://github.com/opengeos/RustGIS/issues/395)) in the GPU map.
- [ ] **Full 3D scene content**: 3D Tiles, I3S, point clouds, splats, CZML time-dynamic scenes,
  true-Z vectors. Missing (_Roadmap: 3D scenes_, [#726](https://github.com/opengeos/RustGIS/issues/726)).
- [ ] **MapLibre style fidelity**: line labels and road shields, line patterns, hillshade and
  fill-extrusion in styles. Partial; vector tiles and labels on the GPU map are underway ([#635](https://github.com/opengeos/RustGIS/issues/635),
  [#596](https://github.com/opengeos/RustGIS/issues/596)).
- [ ] **Basemap catalog**: OpenFreeMap, Protomaps, EOX S2 cloudless, regional basemaps, planetary
  bodies, thumbnail previews. Partial: a basemap gallery (OpenFreeMap, USGS Imagery), custom tile
  layers and a Basemaps plugin.
- [ ] **Multi-map grid**: N synchronized panes. Partial: Side by Side with two maps and linked
  views and cursors (_Roadmap: any time_).
- [ ] **Coordinate grids and readouts**: UTM and MGRS/USNG grids, UTM/MGRS/DDM readouts,
  grid-reference search, pointer elevation. Partial: graticule, decimal/DMS and coordinate navigation
  (_Roadmap: any time_).
- [ ] **Live data overlays**: RainViewer radar, a Timelapse of annual cloudless imagery, live feeds
  (quakes, satellites, fires, AIS). Missing.
- [ ] **Wikipedia cards, Street View/Mapillary, OpenAerialMap, historical imagery**. Missing.
- [ ] **On-map right-click menu with quick analysis**: copy the coordinate, open it elsewhere, run
  a buffer, isochrone or viewshed from the point. Missing (_Roadmap: any time_).
- [ ] **Flight simulator, camera tours, atmosphere**. Partial: a First-Person walk and animation
  keyframes.
- [ ] ~~Multiple rendering engines (MapLibre, Mapbox, Cesium, ArcGIS)~~: out of scope by design;
  RustGIS has its own renderer.

## Cartography & styling
_GeoLibre: docs/features.md › Layers, styling, and labels; docs/user-guide/styling.md_

- [x] **Layer groups**.
- [ ] **Rule-based and expression renderers**: nested rules with scale ranges. Partial: a symbol's
  size, colours, outline width and marker angle by expression (Symbology › By Expression,
  `layer.symbolBy`); label text, size, colour and angle by expression (Labeling pane › Content ›
  By Expression, `labels.by`); Unique Values by expression (`layer.symbology` `expression`), as
  which QGIS's rule-based renderers import, scale ranges as `@map_scale`; display filters are
  per-scale clauses; a rule renderer edited as rules is missing
  (_Roadmap: expressions and cartography_).
- [ ] **Heatmap and point-clustering renderers**. Partial: hexagon/square aggregation; kernel density
  is implemented as an analysis tool, while dynamic heatmap and point-clustering renderers remain outstanding
  (_Roadmap: expressions and cartography_).
- [ ] **More classification methods**: standard deviation, geometric interval, manual breaks.
  Partial: Jenks, equal interval, quantile (_Roadmap: fixes and quick wins_).
- [ ] **Symbol library**: a marker library, custom SVG/PNG markers, fill patterns, pie/donut/bar
  diagrams, arrow lines, inverted-polygon masks, geometry generators. Partial: 5 marker shapes,
  pie/bar diagrams on features from several fields (`layer.diagram`), each slice or bar labelled
  with its value, percent or both, and the categories listed in layout legends (_Roadmap:
  expressions and cartography_ for markers, patterns and a library).
- [ ] **Style interchange**: import/export SLD, QGIS QML, Mapbox GL JSON. Partial: MapLibre style
  export and `.rgisstyle` (_Roadmap: expressions and cartography_ for QML and SLD import).
- [ ] **Smart styling**: automatic palette on add, one-click style suggestions. Missing.
- [ ] **Label engine depth**: expression labels, offset, rotation, wrap, case, priority,
  de-duplication, number formats. Partial: label classes with SQL queries, field labels with
  placement and scale ranges; a labeling pane is underway ([#618](https://github.com/opengeos/RustGIS/issues/618), [#722](https://github.com/opengeos/RustGIS/issues/722)). Expression labels:
  _Roadmap: expressions and cartography_.
- [ ] **On-map legend and colorbar controls**. Partial: the legend is in Contents and layouts only
  (_Roadmap: expressions and cartography_).
- [ ] **Popup and tooltip designer**: field picking, formats (date, link, image), an expression
  title, hover tooltips. Partial: popup modes only (_Roadmap: expressions and cartography_).
- [ ] **Raster histogram stretch UI and Raster Attribute Table**. Partial: stretch types and DRA
  (_Roadmap: expressions and cartography_).
- [ ] **Colour-ramp library and style presets manager**. Partial: custom colour ramps and QGIS,
  GDAL and GMT palette files.

## Attribute data & editing
_GeoLibre: docs/features.md › Attribute data and expressions; docs/arcgis-editing.md_

- [x] **Vertex editing**: move, add and delete vertices.
- [ ] **Topological digitizing and CAD dimensions**: shared-edge drawing, dimension lines,
  building massing. Partial: snapping, topology validation, traverse.
- [ ] **Write-back to sources**: GeoPackage, GeoJSON, PostGIS, SQL Server, ArcGIS FeatureService.
  Missing; edits stay in the project (_Roadmap: data at the source_ for GeoPackage and PostGIS).
- [ ] **Expression builder and engine**: one engine shared by filters, labels, styles and Calculate
  Field, with a function reference. Partial: one expression language serves queries, filters,
  label classes, attribute rules, Calculate Field and symbols (docs: Expressions); labels by
  expression and a builder are missing (_Roadmap: expressions and cartography_).
- [ ] **Virtual fields and persistent attribute joins**. Missing; relationships exist (_Roadmap:
  data at the source_; a one-off join field tool under solid ground).
- [ ] **Attribute form designer**: widgets, constraints, conditional visibility. Partial: domains,
  subtypes, attribute rules (_Roadmap: data at the source_).
- [ ] **Editor tracking fields**: created/edited by and at. Partial: archiving keeps edit times
  (_Roadmap: data at the source_).
- [ ] **Table column management**: rename, hide and reorder columns, a column explorer. Partial:
  sort, filter, add/delete field (_Roadmap: data at the source_).
- [ ] **Quick filters**: per-field checkboxes and range/date controls. Partial: a definition query
  (_Roadmap: expressions and cartography_).
- [ ] **Selection gestures**: polygon, freehand, radius. Partial: click and box; Neighborhood
  Explorer selects by radius (_Roadmap: fixes and quick wins_).
- [ ] **Charts panel and dashboards**: histogram, scatter, bar, line, box and pie charts,
  indicators, cross-filtering. Partial: Exploratory Analysis, Infographics, layout/report charts
  (_Roadmap: analysis_ for charts linked to the selection).

## Analysis
_GeoLibre: docs/features.md › Processing and analysis; docs/user-guide/processing.md,
model-builder.md, segmentation.md, obia.md_

- [x] **Spatial join, intersect, union** ([#959](https://github.com/opengeos/RustGIS/issues/959)):
  spatial join, clip, erase, split, intersect, identity, union, symmetrical difference, update
  and near are built in.
- [ ] **More built-in vector tools**: attribute join, extract vertices, points along lines,
  fix/check geometry, random extract, buffer options. Partial: Merge (append layers), extract
  vertices and points along lines are built in, as Merge, Feature Vertices To Points and Points
  Along Lines (_Roadmap: solid ground_).
- [ ] **Routing services**: isochrones, service areas, OD matrix, multi-stop routing,
  turn-by-turn (Valhalla/OSRM). Partial: shortest path over a line layer (_Roadmap: analysis_).
- [ ] **Geocoding**: forward, batch and reverse, with multiple providers and a place search box.
  Partial: place search and address tables (Nominatim, Photon or your own server), a local locator,
  a search box. Missing: reverse geocoding (_Roadmap: analysis_).
- [ ] **Spatial statistics**: Emerging Hot Spot (Getis-Ord Gi\*), composite score builder.
  Partial: Suitability Modeler (_Roadmap: analysis_ for hot spots).
- [ ] **H3 and DGGS grids**: H3, A5, DGGRID, DGGAL. Partial: A5 grids (_Roadmap: any time_ for
  H3).
- [ ] ~~AI segmentation and detection~~ (SAM/SamGeo, ONNX/YOLO detection, OBIA with eCognition
  import): left to GeoLibre, which has SAM and ONNX models built in and an OBIA workbench with
  eCognition import. RustGIS has pixel classification.
- [ ] **Visual Model Builder canvas**: a node graph with validation and export to code. Partial:
  ModelBuilder with a node canvas and models from history.
- [ ] **Re-run or copy history as code; batch runs over a folder**. Partial: history copied as
  code; batch runs are missing (_Roadmap: solid ground_).
- [ ] **Viewshed from public terrain tiles (no DEM needed)**. Partial: needs a loaded elevation
  raster (_Roadmap: analysis_ for a DEM downloader).
- [x] **Conversion**: vector to PMTiles/GeoParquet, raster to COG: GeoParquet, COG and vector and
  raster PMTiles out ([#557](https://github.com/opengeos/RustGIS/issues/557)).
- [x] **Time Slider**: vector layers by a date field (`time.bind`), data cubes, trends (Sen's
  slope, Mann–Kendall), GIF export.
- [ ] **Point cloud annotation**: ASPRS classes, 3D boxes. Missing.
- [ ] **Hydrology and elevation downloaders**: NLDI navigation, USGS 3DEP and LiDAR, a global DEM
  downloader. Missing (_Roadmap: analysis_ for a DEM downloader).

## Layouts & printing
_GeoLibre: docs/features.md › Map tools, printing, and media; docs/user-guide/print-layout.md_

- [x] **SVG layout export**: each element a named group, map frames and surrounds as vectors.
- [ ] **Atlas along a line, page-filtered tables and charts, copy to clipboard**. Partial:
  per-feature map series (_Roadmap: any time_ for the clipboard).
- [ ] **Map and video recording**: canvas recording, a marker along a line to MP4, camera tours.
  Partial: keyframe PNG frames, time slider GIFs.
- [ ] **STL 3D export**. Partial: OBJ and glTF (_Roadmap: any time_).

## Sharing, collaboration & publishing
_GeoLibre: docs/collaboration.md, docs/server-api.md, docs/user-guide/embedding.md, projects.md,
storymaps.md_

- [ ] ~~Real-time multi-user collaboration~~ (live sessions, permissions, roster, chat):
  left to GeoLibre, the shared, web-first app.
- [ ] ~~Review comments~~ (anchored, threaded, resolvable): left to GeoLibre, the shared,
  web-first app.
- [ ] ~~Share links, project gallery, accounts~~ (roles, expiry, passwords, a server API):
  left to GeoLibre, the shared, web-first app. Share As writes static files.
- [ ] **Story maps**: chapters that fly the live map, a presenter view, HTML export. Partial:
  Presentations.
- [ ] **Embed API**: a `postMessage` protocol, an npm client, viewer-only and map-only modes, deep
  links. Partial: the web app embeddable with `?embed`, exchanging the project through
  `postMessage` (`apps/rustgis-web/src/embed.rs`); no npm client.
- [ ] **Import QGIS (`.qgs/.qgz`) and ArcGIS Pro (`.aprx/.mapx`) projects**. Partial: supported
  QGIS projects open as maps (`project.importQgis`). Missing: ArcGIS Pro (_Exploratory_).
- [ ] **Autosave, project history, crash recovery, snapshot compare**. Partial: undo, per-layer
  archive restore, and autosave with crash recovery in the desktop app
  ([#873](https://github.com/opengeos/RustGIS/issues/873)); the browser has neither yet.
- [ ] **Startup preferences and shareable interface settings**. Partial: start-up preferences,
  Pane Sets and ribbon/QAT customization.
- [ ] **Layer metadata with STAC Item export**. Missing (_Roadmap: data at the source_).
- [ ] **Browser extension ("Open data in …")**. Missing.

## Scripting & integrations
_GeoLibre: docs/python.md, docs/r.md, docs/notebook.md, docs/mcp.md, docs/plugin-api.md,
docs/agent-skill.md_

- [x] **Python package / Jupyter widget**: the `rustgis` package on PyPI runs the engine
  in-process, and `rustgis.Map` is a two-way Jupyter widget (anywidget). No leafmap-style API.
- [ ] **In-app Python console**. Partial: the "Python Window" runs command scripts, not Python
  (_Exploratory_).
- [ ] **Live notebook kernel**: JupyterLite on the web, JupyterLab on the desktop, driving the live
  map. Partial: built-in command notebooks with `.ipynb` in and out.
- [ ] **R package**: RStudio, Quarto, Shiny, `sf`. Missing (_Exploratory_).
- [x] **MCP: drive the running desktop app**: the loopback relay lets external AI clients drive the
  app; `rustgis-cli mcp` ([#75](https://github.com/opengeos/RustGIS/issues/75)) runs on its own project.
- [ ] **MCP Apps interactive map preview**. Partial: `view_map` returns a static PNG.
- [x] **A shipped agent skill (SKILL.md)**: [skills/rustgis](skills/rustgis/SKILL.md) installs and
  manages the app; the assistant has built-in skills too.
- [ ] **Plugin API breadth**: side and floating panels, menus, custom layers, map-click tools,
  assistant tools. Partial: commands, tools, a ribbon tab, assistant skills, process plugins.
- [ ] ~~Python FastAPI / GeoPandas processing sidecar~~: out of scope for pure Rust; process
  plugins can run Python.

## Platforms & distribution
_GeoLibre: docs/android.md, ios.md, mac-app-store.md, downloads.md, self-hosting.md,
deployment-capabilities.md, ui-profiles.md_

- [ ] ~~Android app~~: left to GeoLibre, which has the mobile apps.
- [ ] ~~iOS/iPadOS app~~: left to GeoLibre, which has the mobile apps.
- [ ] ~~Touch and responsive layout~~: left to GeoLibre, which has the mobile apps. RustGIS's
  layout is built for desktop screens, though the web app installs on phones and tablets.
- [ ] ~~Field data collection~~ (forms, photos, GPS or tap placement): left to GeoLibre, which
  has the mobile apps. A QFieldCloud connection, requested in
  [#665](https://github.com/opengeos/RustGIS/issues/665), is not on the roadmap either.
- [ ] ~~GPS~~ (live position, track logs, digitizing from GPS, NMEA receivers): left to
  GeoLibre, which covers field work on phones.
- [ ] **PWA and offline basemap tile pre-caching**. Partial: the web app is installable with
  offline start; offline data areas with sync. Tile pre-caching unverified.
- [ ] **Docker self-hosting image with runtime config**. Partial: a Dockerfile for the web app and
  published images (ghcr.io/opengeos/rustgis, and rustgis-cli for the command line); runtime config
  (settings from the container's environment) is missing.
- [ ] **Deployment capabilities, policies and UI profiles**: restrict edit, add, run and export per
  deployment. Missing.
- [ ] **Package-manager and store channels**: winget, a Homebrew Cask, the Mac App Store. Partial:
  winget, Homebrew, the AUR, Fedora COPR and the Microsoft Store; the Mac App Store is missing.
- [ ] **Windows code signing**. Missing (_Roadmap: solid ground_).
- [ ] **Auto-update**. Partial: Check for Updates (_Roadmap: solid ground_ for in-place updates).
- [ ] **Desktop network features**: OS trust store, mTLS client certificates, network log,
  reloading files changed on disk. Missing (_Roadmap: solid ground_ for reloading files).

## AI
_GeoLibre: docs/user-guide/ai-assistant.md, docs/features.md › AI, Python, and automation_

- [ ] **Voice commands and spoken answers**. Missing.
- [ ] **Provider profiles and per-feature providers; AWS Bedrock, Vercel AI Gateway**. Partial:
  one provider and model at a time.
- [ ] **AI-authored processing models**. Missing.

## Other
_GeoLibre: docs/features.md › Platforms and interface; docs/i18n.md_

- [ ] **More languages and right-to-left layout**: GeoLibre has 20 languages including Arabic,
  Persian and Hebrew. Partial: 20 languages; Persian text is drawn right to left, while the panes
  keep their left-to-right layout.
- [ ] **Translated geoprocessing tool names**. Missing (possibly deliberate).
- [ ] **Keyboard-shortcut cheat sheet**. Partial: Command Search; rebindable shortcuts are on the
  roadmap (_any time_, [#562](https://github.com/opengeos/RustGIS/issues/562)).

## Already at or beyond parity
- A native single-binary app, with every action a command shared by the ribbon, CLI, notebooks,
  models, the assistant and MCP
- Map projections on the main map (presets and projected EPSG codes)
- Reports: grouped, with statistics, charts and subreports
- Data design: domains, subtypes, attribute rules, contingent values, relationships, archiving,
  topology rules, traverse/COGO
- Large GeoPackage, GeoParquet and FlatGeobuf files streamed a view at a time
- Layouts: map frames, grids, extent indicators, map series and publication sheets to PDF
- Sandboxed WASM plugins with permissions and a hash-pinned registry, the same file on desktop,
  CLI and web
- AI Assistant with local models and per-command approval; `rustgis-cli ask`; an MCP server with
  built-in docs
- Imagery: mosaics, mensuration, pixel editor, a trainable classifier, hosted imagery tiles
- Offline areas with two-way sync of edited features

## To verify
- Whether exported COGs (written today) pass a COG validator: overviews and tile layout
- GeoLibre's language count: [#77](https://github.com/opengeos/RustGIS/issues/77) said 20,
  `docs/comparison/geolibre.md` says 21
- Accessibility compared with GeoLibre's axe-checked screens
- Whether offline areas or the tile cache pre-fetch basemap tiles
- Whether the shortcuts view is a full cheat sheet
- Some GeoLibre features above are taken from its docs and live in plugins or the Cesium engine
  only
