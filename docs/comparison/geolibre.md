# RustGIS and GeoLibre

RustGIS and [GeoLibre](https://geolibre.app) are sibling [opengeos](https://github.com/opengeos)
projects with the same maintainer, so people often ask how they differ and which to use. This
page answers that: what each is built for, where each is stronger, and how to use them together.
For RustGIS beside QGIS, ArcGIS Pro and ArcGIS Online as well, see the
[full comparison](index.md).

!!! note "How to read this"
    This page is written by the RustGIS project. It reflects RustGIS as of **October 2026** and
    GeoLibre 3.4 as described in [its documentation](https://geolibre.app), not every
    plugin either one can load. Both projects move quickly, so details here age fast. Found
    something wrong or out of date? Please
    [open an issue](https://github.com/opengeos/RustGIS/issues).

## The short answer

GeoLibre and RustGIS serve similar purposes, but take different approaches and suit different
workflows. Neither is meant to replace the other
([discussion #951](https://github.com/opengeos/RustGIS/discussions/951)).

**GeoLibre is web and mobile first.** It is built with web technology (Tauri, React,
MapLibre GL JS, DuckDB-WASM, deck.gl) and runs in a browser tab with nothing installed, as a
desktop app, and as native Android and iOS apps. Its strengths are access from anywhere,
cloud-native data, interactive visualization and sharing. GeoLibre is mature, with a broad
set of features; as its maintainer put it in
[discussion #951](https://github.com/opengeos/RustGIS/discussions/951), its development now
centres on stability, performance and usability.

**RustGIS is desktop first, built for automation and AI agents.** It is a single program written
entirely in Rust that draws its own interface on the GPU, with no browser engine or JavaScript
runtime inside; the same program, compiled to WebAssembly, also runs in the browser. Its
interface is a ribbon laid out in the order of the work, and almost everything in it is a
command that scripts, the command line, Python and AI agents can run. It goes deeper in the
classic desktop GIS jobs of data design, reports and print layouts. RustGIS is young and
changing quickly: expect frequent releases, new capabilities and some significant changes.

The two complement each other. Both can run the 1,000+ GeoLibre/Whitebox geoprocessing suite,
both read and write common open formats, RustGIS imports `.geolibre` projects as new maps,
and GeoLibre opening `.rgis` projects is planned for later (see
[Using them together](#using-them-together)). Choose GeoLibre for web-first work, phones and
notebooks, choose RustGIS for a scriptable desktop GIS, or use both.

## Where each is stronger today

These lists compare features as of **October 2026**, and RustGIS's features change quickly. The
differences in the short answer, in platform, technology and focus, are the ones that last.

### Where GeoLibre is stronger

- **More platforms.** GeoLibre runs as a web app, an installable PWA, a desktop app on Windows,
  macOS and Linux, native [Android and iOS apps](https://geolibre.app), a Docker image, a Chrome
  extension and a Jupyter widget. RustGIS runs on the desktop and in the browser; it has no
  mobile app and no touch-first layout.
- **Data reach.** GeoLibre opens more formats in the app itself, including File Geodatabase,
  DWG, MapInfo TAB, NetCDF-4/HDF5, Kerchunk and Icechunk, ICESat-2 and GEDI. It connects to
  SQL Server and Apache Iceberg as well as PostGIS and DuckDB, and its plugins browse dozens of
  catalogs and portals, from NASA Earthdata and Planet to Hugging Face and about 150 US local
  agencies. RustGIS reads a focused set natively, including 2D File Geodatabase feature
  classes and tables, GeoRSS and LandXML. It also has NASA Earthdata discovery and live
  SQL Server/Azure SQL connections. Other formats and File Geodatabase tables the native
  reader cannot read need GDAL (see [Data formats](../user-guide/data-formats.md)).
- **SQL in the browser.** GeoLibre runs DuckDB Spatial, PGlite/PostGIS and Apache Sedona inside
  the browser. RustGIS's SQL pane uses DuckDB on the desktop; its web app has no SQL engine.
- **Cartography depth.** GeoLibre has rule-based and expression renderers, heat maps,
  clustering, diagrams, blend modes, layer groups, a data-defined label engine, and style
  interchange with SLD, QGIS QML and Mapbox GL JSON. RustGIS covers the common renderers and
  SQL-filtered label classes, stacked line symbols, symbol levels, blend modes and layer groups,
  symbol size, colours, outline width and angle by expression, and label text, size, colour and
  angle by expression, and Unique Values by expression (QGIS's rule-based renderers import as
  it), but no rule-based renderer edited as rules.
- **3D.** GeoLibre has a Cesium globe with terrain, 3D Tiles, I3S, Gaussian splats, CZML and
  Google Photorealistic 3D Tiles. RustGIS has a globe, terrain-draped tilted views, 2.5D
  extrusion and LiDAR point layers.
- **AI and imagery models.** Besides an AI assistant, GeoLibre has SAM segmentation, in-browser
  ONNX/YOLO object detection and an object-based image analysis workbench. RustGIS has pixel
  classification (k-means, minimum distance and a trainable neural network).
- **Sharing and collaboration.** GeoLibre shares projects through links with roles, expiry and
  passwords, has review comments anchored to the map, story maps, dashboards, and an
  experimental real-time collaboration mode. RustGIS shares by writing files: web map pages,
  packages and static exports.
- **Python, R and notebooks.** The `geolibre` Python package is a two-way Jupyter widget with a
  leafmap-style API, the `geolibre` R package works in RStudio, Quarto and Shiny, and the
  notebook panel runs JupyterLite or JupyterLab. RustGIS has a [Python package](../python.md)
  that runs commands headlessly and built-in notebooks of command cells, but no Python console
  in the app and no R package.
- **Opening other projects.** GeoLibre imports QGIS (`.qgs`/`.qgz`) and ArcGIS Pro
  (`.aprx`/`.mapx`) projects. RustGIS opens supported QGIS projects as maps (see
  [QGIS projects](../user-guide/workspace.md#qgis-projects-and-styles)) but not ArcGIS Pro ones.
- **Editing.** GeoLibre edits vertices and writes edits back to their sources (GeoPackage,
  PostGIS, ArcGIS feature services). RustGIS moves, inserts and deletes vertices, but
  edits stay in the project until exported rather than being written back to those sources.
- **Languages.** GeoLibre's interface comes in 21 languages, including right-to-left Arabic,
  Hebrew and Persian. RustGIS has [17](../i18n.md), including Persian text support;
  its widget layout is not right-to-left.

### Where RustGIS is stronger

- **A native program, not a web view.** RustGIS is memory-safe Rust compiled to one native
  program with no browser engine, JavaScript or Python inside the core app. Large
  GeoPackage, GeoParquet, FlatGeobuf and Shapefile files are read a view at a time rather
  than loaded whole; the cloud-native readers also work in the browser.
- **Commands for scripts and AI agents.** Ribbon actions dispatch commands with ids and JSON
  parameters. Engine commands run from [`rustgis-cli`](../cli.md), the
  [`rustgis` Python package](../python.md), notebooks, models, the Script pane, the
  [MCP server](../mcp.md) and the AI Assistant. UI-only pane/dialog commands stay in the app.
  A batch of maps can be rendered in CI with one
  command line. GeoLibre is driven through its interface, its Python and R packages and its MCP
  server; it has no general command-line tool.
- **Sandboxed WebAssembly plugins.** These RustGIS [plugins](../plugins.md) use declared,
  approved permissions and run on desktop, in `rustgis-cli` and in the browser.
  Plugins can add commands, tools, ribbon tabs, panels and assistant skills. Native process
  plugins, including Python environments managed by RustGIS, are also supported on desktop
  and CLI, but are not sandboxed. GeoLibre's TypeScript plugin API states that its plugins
  "run as trusted code in the app window" and that its policy "is not a security boundary".
- **Data design.** RustGIS defines domains, subtypes, attribute rules, contingent values,
  relationship classes, archiving, topology rules and traverse/COGO in the project itself, the
  geodatabase design work of ArcGIS Pro. GeoLibre has attribute forms with constraints and
  honours the domains and subtypes of the ArcGIS feature services it edits, but doesn't define
  its own.
- **Reports and layouts.** RustGIS builds grouped reports with statistics, charts and
  subreports, and layouts with publication presets, shaped map frames, legends, colorbars,
  grids, tables, charts, extent indicators and map series, exported to PDF or PNG. GeoLibre's
  print layout and atlas cover the common cases, and it has no report builder.
- **A ribbon interface.** Tabs follow the order of the work (Home, Data, Style, Labeling, Edit,
  Analyze, View, Publish), with Command Search (++ctrl+k++) to find any command or tool by name. People
  coming from ArcGIS Pro often find it familiar. GeoLibre uses menus and panels, with a command
  palette and beginner, intermediate and advanced interface levels.
- **Offline local workflows.** RustGIS's local-file readers, built-in tools and rendering work
  without a network or server (remote and cloud sources need the network); offline areas sync edits back to the corresponding map in the project,
  not to the underlying source data or a remote database. GeoLibre tools, GDAL, DuckDB and process plugins use separate programs.
  GeoLibre works
  offline too, but some features need a remote host (GDAL export, ONNX detection and story map
  HTML export among them), and on the desktop its raster tools and some vector tools use an
  optional Python sidecar.

## Side by side

| | **RustGIS** | **GeoLibre** |
|---|---|---|
| **Licence** | Apache-2.0 | MIT |
| **Written in** | Rust (egui, wgpu, tiny-skia) | TypeScript and Rust (Tauri, React, MapLibre GL JS, DuckDB-WASM, deck.gl) |
| **First release** | 2026 | 2026 (v1.0 in June) |
| **Platforms** | Windows, macOS, Linux, and the browser (WebAssembly) | Windows, macOS, Linux, the browser and PWA, Android, iOS, Docker |
| **Interface** | Ribbon with Command Search | Menus and panels with a command palette and interface levels |
| **Map engines** | One native renderer (2D map and globe) | MapLibre, Mapbox GL JS, CesiumJS or the ArcGIS Maps SDK |
| **Formats** | A focused set read natively; more through GDAL when installed | A wide set in the app, some desktop only (File Geodatabase) |
| **Databases** | PostgreSQL/PostGIS, MySQL/MariaDB and SQL Server/Azure SQL read live (read only); DuckDB through DuckDB; desktop only | DuckDB, PostGIS, SQL Server, Iceberg; PGlite and Sedona in the browser |
| **Geoprocessing** | Built-in tools plus optional 1,000+ GeoLibre/Whitebox suite; list depends on the runner | 1,000+ (GeoLibre and Whitebox) plus the GeoLibre Toolbox |
| **Editing** | Create, move, rotate, scale, split, merge, vertex move/insert/delete, snapping; export edits | Vertex editing and write-back to sources |
| **Data design** | Domains, subtypes, attribute rules, relationships, topology | Attribute forms; ArcGIS services' domains and subtypes |
| **Output** | Layouts, map series, reports, presentations, web map pages | Print layouts with atlas, story maps, dashboards, offline HTML |
| **Scripting** | Engine commands: CLI, Python package, notebooks, Script pane, MCP; UI-only commands stay in the app | Python and R packages, Jupyter widget, notebooks, MCP |
| **Plugins** | Sandboxed WebAssembly; unsandboxed desktop/CLI process and Python plugins; registry | TypeScript, trusted, small registry |
| **AI** | Assistant using engine commands and project undo; MCP server; pixel classification | Assistant with undoable operations and voice; SAM, ONNX; MCP server |
| **Collaboration** | — | Share links, review comments, real-time (experimental) |
| **Languages** | 17, including Persian text; no right-to-left widget layout | 21, including right-to-left |
| **Project file** | `.rgis` (open JSON) | `.geolibre` (open, documented JSON) |

## Using them together

The two read and write the same open formats, so data moves between them without conversion:
GeoPackage, GeoParquet, FlatGeobuf, Shapefile, GeoJSON, KML/KMZ, GPX, CSV and cloud-optimised
GeoTIFF (GeoPackage export is desktop only in RustGIS).

RustGIS imports `.geolibre` projects as new maps, keeping supported data, styles and views
and reporting differences. See [GeoLibre projects](../user-guide/workspace.md#geolibre-projects)
for supported content and limits. GeoLibre opening `.rgis` projects
comes later, once RustGIS's project format and the data types it supports settle: RustGIS is
still changing quickly, and GeoLibre would otherwise be reading a moving target.

They also share their geoprocessing. RustGIS's **Analyze › Tools** offers the GeoLibre and
Whitebox suite through the optional `geolibre` program on desktop and an on-demand WebAssembly
runner in the browser. The available tools come from that runner's manifests. See
[Analysis](../user-guide/analysis.md).

A common split is to explore, share and collaborate in GeoLibre, especially from a browser,
a phone or a notebook, and to use RustGIS for repeatable desktop work: a project scripted
command by command, maps rendered by `rustgis-cli` in CI, data with domains and attribute rules,
or reports and map series printed to PDF.

## Common questions

**Why are there two?**
To explore different ways of making geospatial tools accessible and powerful. GeoLibre uses the
web platform to reach as many places and data sources as possible. RustGIS explores what a
desktop GIS looks like as one native Rust program that scripts and AI agents can drive. Each
project borrows ideas from the other, and they are meant to be used together, not chosen
between.

**Which one should I start with?**
If you want to open a dataset in a browser tab or on a phone right now, or work in Jupyter or R,
start with GeoLibre. If you want a desktop GIS with a ribbon, scriptable engine commands,
or reports and data design, start with RustGIS.

**Do they run the same geoprocessing tools?**
RustGIS can load the GeoLibre/Whitebox suite of over 1,000 tools from
[geolibre-rust](https://github.com/opengeos/geolibre-rust), with the available list depending
on the runner. RustGIS also has built-in vector tools, A5 grids, interpolation and guided
workflows; GeoLibre adds its Toolbox (routing, H3 grids, spatial statistics).

**Can a GeoLibre plugin run in RustGIS, or the other way round?**
No. GeoLibre plugins are TypeScript modules for its web interface. RustGIS has its own
WebAssembly and native process-plugin interfaces, including Python process plugins.

**Can I open a GeoLibre project in RustGIS?**
Yes. **Open** or **Project › New › GeoLibre Project…** imports a `.geolibre` (or
`.geolibre.json`) as a new map, preserving supported data, styles and the saved view.
The command `project.importGeolibre` also accepts a URL, text or base64 JSON. It reports
unsupported sources and appearance differences per layer; see
[GeoLibre projects](../user-guide/workspace.md#geolibre-projects) for the limits.

## See also

- [Full comparison](index.md): RustGIS beside GeoLibre, QGIS, ArcGIS Pro and ArcGIS Online
- [RustGIS and QGIS](qgis.md): a closer look, for QGIS users
- [GeoLibre](https://geolibre.app) and [its comparison](https://geolibre.app/comparison/) with
  QGIS, ArcGIS, CARTO, Felt and kepler.gl
- [Analysis](../user-guide/analysis.md): the GeoLibre and Whitebox tools in RustGIS
