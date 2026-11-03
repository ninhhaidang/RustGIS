# RustGIS vs. other GIS platforms

How RustGIS compares to its sibling project **[GeoLibre](https://geolibre.app)** and to the GIS
platforms people most often ask about: **QGIS**, **ArcGIS Pro** and **ArcGIS Online**.

RustGIS is not trying to replace any of these. It fills a narrower spot: a **free and
open-source desktop GIS compiled to a single native program**, with a ribbon interface familiar
from desktop GIS, almost every action available as a scriptable command, and the same app running
in a browser tab through WebAssembly.

!!! note "How to read this"
    Comparisons like this age quickly and are written by an interested party. The RustGIS
    column reflects what ships in the [current source](https://github.com/opengeos/RustGIS)
    as of **October 2026**; the other columns describe each product's mainstream,
    out-of-the-box behaviour, not what is reachable with every add-on or plugin. The GeoLibre,
    QGIS and ArcGIS columns draw on [GeoLibre's own comparison](https://geolibre.app/comparison/),
    where those products are described in more depth. Found something wrong or out of date?
    Please [open an issue](https://github.com/opengeos/RustGIS/issues).

Coming from QGIS? [RustGIS and QGIS](qgis.md) goes deeper: the pros and cons of each, how QGIS
concepts map onto RustGIS, and how to use the two together. Deciding between RustGIS and its
sibling? [RustGIS and GeoLibre](geolibre.md) compares the two in detail.

## RustGIS and GeoLibre

RustGIS and GeoLibre are both [opengeos](https://github.com/opengeos) projects, and they share
more than a maintainer: both can run the suite of over 1,000 GeoLibre and Whitebox
geoprocessing tools, through an optional desktop runner or WebAssembly in the browser.

They differ in how they are built and who they suit:

- **GeoLibre** is built on web technology (Tauri, React, MapLibre GL JS, DuckDB-WASM, deck.gl).
  It reaches further: cloud-native formats (GeoParquet, PMTiles, Zarr, STAC), databases, OGC
  and Esri services, native Android and iOS apps, a Jupyter widget, a Python package and
  real-time collaboration.
- **RustGIS** is written entirely in Rust and draws its own interface on the GPU (egui). It is
  one native program with no browser engine or JavaScript runtime inside, a ribbon laid
  out in the order of the work, and an engine whose commands run the
  same way from the ribbon, `rustgis-cli`, Python, notebooks, models and the AI Assistant.

They complement each other rather than compete: GeoLibre is web and mobile first, RustGIS
desktop first and built for automation and AI agents. If you want the widest data access and the
most platforms, choose GeoLibre. If you want a compact native desktop GIS with a classic ribbon
workflow and a scriptable command engine, choose RustGIS. Both work on local files on your
machine; remote features, such as web data and AI providers, send requests over the network. See
[RustGIS and GeoLibre](geolibre.md) for a closer look.

## At a glance

| | **RustGIS** | **GeoLibre** | **QGIS** | **ArcGIS Pro** | **ArcGIS Online** |
|---|---|---|---|---|---|
| **License** | Apache-2.0, open source | MIT, open source | GPL-2.0+, open source | Proprietary | Proprietary |
| **Cost** | Free | Free | Free | Paid subscription | Paid subscription (credit-metered analysis) |
| **Built with** | Rust (egui, wgpu, tiny-skia) | TypeScript and Rust (Tauri, React, MapLibre) | C++ and Python (Qt) | .NET and C++ | Web (JavaScript) |
| **Desktop** | Windows, macOS, Linux: one native program | Windows, macOS, Linux (Tauri) | Windows, macOS, Linux | Windows only | — |
| **Browser** | WebAssembly app; external programs and live database connections are desktop only | Full app, nothing to install | No official browser build | No | Yes (the product *is* the browser app) |
| **Mobile** | — | Native Android and iOS apps | Via QField / Mergin Maps (separate apps) | — | Via ArcGIS Field Maps (separate app) |
| **Interface** | Ribbon, tabs in the order of the work | Panels and toolbars | Toolbars and panels | Ribbon with contextual tab groups | Web map viewer and apps |
| **Works offline** | Local data and built-in tools work offline; offline areas sync edits back to the project's source map | Yes: PWA install, offline areas, desktop build | Yes | Yes | Limited (Field Maps offline areas) |
| **Where your data lives** | Your device | Your device, processed client-side | Your device | Your device / your enterprise geodatabase | Vendor cloud |
| **Project file** | `.rgis` (JSON, open) | `.geolibre` / `.geolibre.json` (open, documented) | `.qgs` / `.qgz` (open) | `.aprx` (proprietary) | Web map JSON (hosted) |

## Data and formats

| | **RustGIS** | **GeoLibre** | **QGIS** | **ArcGIS Pro** | **ArcGIS Online** |
|---|---|---|---|---|---|
| **Format breadth** | GeoJSON, CSV, Shapefile, GeoPackage (desktop export), GeoParquet, FlatGeobuf, KML/KMZ and GPX in and out; GML, MapInfo MIF, OSM PBF, TopoJSON, CityJSON, DXF, GeoRSS, LandXML, LiDAR and spreadsheets in; File Geodatabase feature classes and tables read natively, with limits; GeoTIFF/COG, ASCII grids and georeferenced PNG/JPEG; WKT out; optional GDAL and DuckDB programs extend access on desktop | Wide: DuckDB-WASM Spatial plus in-house readers | **Widest**: everything GDAL/OGR reads | Very wide, plus native Esri formats | Common upload formats |
| **Cloud-native data** | GeoParquet, FlatGeobuf, PMTiles and cloud-optimised GeoTIFF read in place, large files and web addresses alike (by address, over HTTP byte ranges: a COG reads only the tiles the view needs, a FlatGeobuf only its index and the features in view, a GeoParquet its footer and the row groups in view, desktop and web), Zarr 2 and 3 stores (the chunks of the step picked, desktop and web), from `s3://` buckets too (private ones signed with a connection's keys, S3-compatible services by endpoint) | GeoParquet, FlatGeobuf, PMTiles, COG, Zarr, streamed over HTTP | GeoParquet, FlatGeobuf, PMTiles, COG, Zarr | GeoParquet, COG, Zarr | Hosted layers |
| **Web maps and tiles** | XYZ/TMS/WMS raster tiles, WMTS, OGC API Tiles, Mapbox Vector Tiles, MapLibre styles | XYZ, WMS/WFS/WMTS, OGC API, vector tiles, MapLibre styles | Full OGC support, vector tiles | Full OGC support | Yes |
| **Coordinate systems** | Rasters in any EPSG code, reprojected on the fly; the map in presets or projected EPSG and ESRI codes | Reprojects on import, EPSG catalog | Full PROJ support | Full support | Web Mercator focus |
| **Geocoding** | Place/address search and address tables through Nominatim or Photon (OSM's public servers or your own) | Geocoding search and batch geocoding | Through plugins | ArcGIS World Geocoder (credits) | Yes |
| **Open-data portals** | OpenStreetMap by area (Overpass), administrative boundaries (geoBoundaries), and datasets searched by text on data.gov and CKAN sites, Socrata and ArcGIS Hub | ArcGIS Hub, Socrata, CKAN, US portals, geoBoundaries, Overpass, Natural Earth | Through plugins (QuickOSM) | Living Atlas | Living Atlas |
| **STAC, databases, Esri services** | STAC search (Earth Search, Planetary Computer or any STAC API, and `?stac=` links), footprints, assets and time-series cubes; Overture Maps, Source Cooperative and NASA Earthdata discovery; PostgreSQL/PostGIS, MySQL/MariaDB and SQL Server/Azure SQL read live (desktop), and DuckDB databases through DuckDB (desktop); WMS, WMTS, WFS, WCS, OGC API Features and Tiles, and ArcGIS REST services; CSW catalogue search; Planetary Computer assets signed; Data Sources with favourites, recent sources, grouped named connections and a public-service library | STAC browser, PostGIS, DuckDB, Iceberg, ArcGIS services, File Geodatabase (desktop) | STAC, PostGIS, SpatiaLite, Oracle, SQL Server, ArcGIS services | Enterprise geodatabases, Esri services | Hosted only |
| **3D** | A 3D globe; tilted views draped over terrain from a DEM layer or elevation tiles; 2.5D extrusion with lighting and shadows; glTF scene export; LiDAR (LAS/LAZ/COPC) as point layers; geotagged photos as points with thumbnails | Cesium globe, 3D Tiles, I3S, LiDAR, Gaussian splats | Point clouds, 3D map view | **Deepest 3D**: scenes, I3S, mesh, LAS | Scene Viewer |

## Analysis and processing

| | **RustGIS** | **GeoLibre** | **QGIS** | **ArcGIS Pro** | **ArcGIS Online** |
|---|---|---|---|---|---|
| **Geoprocessing tools** | Built-in tools plus the optional **1,000+** GeoLibre/Whitebox suite (the available list comes from the runner) | **1,000+** (GeoLibre and Whitebox) | **1,000+** (native, GDAL, GRASS, SAGA) | **Most mature and complete**, plus paid extensions | A useful subset, credit-metered |
| **Where analysis runs** | Your machine; in the browser on WebAssembly | Your browser (WASM); optional Python sidecar on desktop | Your machine | Your machine, or a server | Vendor cloud |
| **Vector analysis** | Geodesic buffer, clip, erase, intersect, dissolve, spatial join, summarize within, statistics, selection by location, A5 grids and aggregation; optional GeoLibre vector tools | Buffer, overlay, dissolve, joins, topology checks | Comprehensive | Comprehensive | Common tools |
| **Raster analysis** | Raster functions, band math, indices, change detection, classification, viewshed, IDW and ordinary kriging; time-series summaries and trends; optional GeoLibre terrain and hydrology tools | Hillshade, slope, zonal stats, raster calculator, reclassify | Comprehensive (GDAL/GRASS/SAGA) | Comprehensive; Spatial Analyst | Limited |
| **Query language** | SQL `WHERE` clauses for selection and definition queries; DuckDB Spatial queries over files, web addresses and databases in an SQL pane (desktop) | DuckDB Spatial, PGlite/PostGIS, Sedona, all in the browser | Virtual layers, PostGIS | SQL against geodatabases | — |
| **Model / batch chaining** | ModelBuilder (a node canvas; models from history), tasks, scripts | Visual Model Builder, batch runner | Graphical Model Designer | ModelBuilder | — |
| **Guided workflows** | Suitability, visibility, line of sight, IDW, shortest path on line networks, neighborhood, simulation; driving, cycling and walking directions through Valhalla | Isochrones, routing, Emerging Hot Spot Analysis | Plugins and processing providers | Extensions (Network, Spatial, Geostatistical Analyst) | Routing services (credits) |
| **AI / ML** | Pixel classification (k-means, minimum distance, a trainable neural network) | AI Segmentation (SAM), ONNX object detection | Via plugins | Deep-learning toolset | Pretrained models (credits) |

## Cartography, layout and output

| | **RustGIS** | **GeoLibre** | **QGIS** | **ArcGIS Pro** | **ArcGIS Online** |
|---|---|---|---|---|---|
| **Renderers** | Single, unique values, graduated, proportional; stacked line strokes and symbol levels; hexagon/square aggregation; blend modes and layer groups; raster stretch, RGB, classify, colormap, shaded relief | Single, categorized, graduated, rule-based, heatmap, cluster, proportional | **Richest** | Very rich | Smart-mapping styles |
| **Labeling** | Collision avoidance, SQL-filtered label classes, per-class placement and scale/distance ranges, annotation | Data-defined engine | Best-in-class placement | Maplex | Basic |
| **Print layout** | Publication presets, shaped map frames, legends, colorbars, scale bars, grids, extent indicators, tables, charts and map series; PDF and PNG | Print Layout with atlas, PNG, PDF, SVG | Full composer with atlas | Full layout view with map series | Basic print |
| **Reports** | Grouped reports with statistics, charts and subreports; PDF | — | Via atlas and plugins | Reports | — |
| **Presentations** | HTML slideshow and PDF from map, text, image and video pages | Story maps with presenter view | Via plugins | — | ArcGIS StoryMaps (separate product) |
| **Standalone export** | Web map page (one HTML file), OGC API Features collections, MapLibre styles, glTF | Whole project to one offline HTML file | Via qgis2web | — | — |

## Automation and extensibility

| | **RustGIS** | **GeoLibre** | **QGIS** | **ArcGIS Pro** | **ArcGIS Online** |
|---|---|---|---|---|---|
| **Scripting** | Engine commands run headlessly through `rustgis-cli` and the `rustgis` Python package; Script pane; UI-only pane/dialog commands stay in the app | Python console, `geolibre` Python package | PyQGIS, `qgis_process` | `arcpy` | ArcGIS API for Python / JavaScript |
| **Notebooks** | Built-in notebooks of text and command cells; Jupyter `.ipynb` in and out | The whole app as a Jupyter widget | Bindings, not the UI | Notebooks drive `arcpy` | Hosted notebooks |
| **Plugins** | Permission-controlled WebAssembly plugins on desktop, CLI and web; unsandboxed process/Python plugins on desktop and CLI; a registry, custom ribbon tabs and panels, assistant skills | TypeScript plugin API, a registry | **Largest**: thousands of Python plugins | .NET add-ins, Python toolboxes | App templates |
| **AI assistant** | Acts through engine commands, with undo for project edits; Anthropic, OpenAI, Gemini, OpenRouter, Ollama or any OpenAI-compatible model; bring your own key or sign in with ChatGPT; also on the command line; MCP for external agents | Natural-language assistant with undoable operations; bring your own key | Via plugins | Copilot features | AI assistants |
| **Collaboration** | — | Real-time (MVP), review comments | No | No | Shared hosted layers |
| **Self-hosting** | Dockerfile or static build of the web app | Docker image or static build | QGIS Server | ArcGIS Enterprise (paid) | — |

## Where each one is the right choice

**Choose RustGIS when** you want a compact, native desktop GIS that installs as one program, a
ribbon interface without a licence, engine commands scriptable from
the command line with the same ids the interface uses, sandboxed WebAssembly plugins, or an AI
assistant whose project edits use the same undo system as the interface.

**Choose GeoLibre when** you need the widest data access from a browser tab (cloud-native
formats, STAC, databases, OGC and Esri services), the same app on Android, iOS and in Jupyter,
real-time collaboration, or its Jupyter widget.

**Choose QGIS when** you need the widest format support through GDAL/OGR, the deepest open-source
cartography, a mature plugin for a niche task, or heavy local processing of large datasets.

**Choose ArcGIS Pro when** you are in the Esri ecosystem, need enterprise geodatabases and
versioned editing, the deepest 3D and imagery workflows, or organizational support.

**Choose ArcGIS Online when** the priority is hosted layers, organizational sharing and the
surrounding Esri app family.

They also compose: RustGIS reads and writes GeoJSON, CSV, GeoPackage, GeoParquet, Shapefile and GeoTIFF, which every tool above
opens, and writes MapLibre styles and OGC API Features collections for the web.

## Where RustGIS is not the strongest option

Stated plainly, so the tables above are worth trusting:

- **Format breadth.** MySQL/MariaDB and SQL Server/Azure SQL are read only;
  PostGIS and GeoPackage support table write-back on desktop. Live database connections and
  DuckDB files are desktop only. The web app doesn't write GeoPackages or FlatGeobufs.
  The native File Geodatabase reader is 2D and does not read compressed tables,
  raster fields or sparse row layouts; GDAL can fill these gaps on desktop. DWG and rasters
  such as JPEG 2000, NetCDF and HDF also need GDAL. SQL queries need DuckDB; both are external programs.
- **Coordinate systems.** Vector data is stored in longitude/latitude; there is no full PROJ.
- **Editing depth.** Vertex move, insert and delete are supported, but not versioned multi-user
  database editing. Desktop GeoPackage and PostGIS layers loaded for editing
  can save back to their source tables with row-conflict checks; other edits stay in the project
  until exported.
- **Specialized analysis.** Built-in spatial joins, overlays (intersect, identity, union,
  symmetrical difference, update) and shortest paths on line networks are available, but full
  network datasets and advanced spatial statistics depend on the external suite or another GIS.
- **Python.** The [`rustgis` package](../python.md) runs commands headlessly, but there is no
  Python console inside the app. Python process plugins are supported on desktop and CLI, without a sandbox.
- **Maturity.** RustGIS is young. Many features are first versions; see the
  [roadmap](https://github.com/opengeos/RustGIS/blob/main/ROADMAP.md).

## See also

- [User Guide](../user-guide/index.md): what RustGIS does, page by page
- [RustGIS and GeoLibre](geolibre.md): a closer look at the sibling project
- [RustGIS and QGIS](qgis.md): a closer look, for QGIS users
- [Command Line](../cli.md): scripting RustGIS
- [GeoLibre's comparison](https://geolibre.app/comparison/): GeoLibre against QGIS, ArcGIS,
  CARTO, Felt, kepler.gl and Google Earth
