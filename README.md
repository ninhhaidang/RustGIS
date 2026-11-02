<p align="center">
  <img src="https://assets.rustgis.app/images/logo.svg" alt="RustGIS logo: a rust-coloured cog framing a segmented globe with an orbit" width="160">
</p>

<h1 align="center">RustGIS</h1>

<p align="center">
  <b>A fast, free and open-source GIS in pure Rust for mapping, analyzing and sharing geospatial data.</b><br>
  Native on Windows, macOS and Linux, and in your web browser. Your data stays on your machine
  unless you send it to an AI model you choose, and every action is a command you can script.
</p>

<p align="center">
  <img alt="Written in Rust" src="https://img.shields.io/badge/written%20in-Rust-b84a2c?style=flat-square&logo=rust&logoColor=white">
  <img alt="License: Apache-2.0" src="https://img.shields.io/badge/license-Apache--2.0-2563eb?style=flat-square">
  <img alt="19 languages" src="https://img.shields.io/badge/languages-19-0e9384?style=flat-square">
  <a href="https://plugins.rustgis.app"><img alt="Plugins" src="https://img.shields.io/badge/plugins-registry-7c3aed?style=flat-square"></a>
  <a href="https://pypi.org/project/rustgis/"><img alt="PyPI" src="https://img.shields.io/pypi/v/rustgis?style=flat-square&logo=python&logoColor=white&label=pypi"></a>
  <a href="https://colab.research.google.com/github/opengeos/RustGIS/blob/main/apps/rustgis-python/examples/getting-started.ipynb"><img alt="Open in Colab" src="https://colab.research.google.com/assets/colab-badge.svg"></a>
  <a href="https://doi.org/10.5281/zenodo.23633281"><img alt="DOI" src="https://img.shields.io/badge/DOI-10.5281%2Fzenodo.23633281-1682d4?style=flat-square"></a>
</p>

<p align="center">
  <a href="https://web.rustgis.app"><img alt="Launch the RustGIS web app" src="https://img.shields.io/badge/Launch-web%20app-b84a2c?style=for-the-badge&logo=webassembly&logoColor=white" height="36"></a>
  <a href="https://github.com/opengeos/RustGIS/releases/latest"><img alt="Download the desktop app" src="https://img.shields.io/badge/Download-desktop%20app-2563eb?style=for-the-badge&logo=github&logoColor=white" height="36"></a>
  <a href="https://rustgis.app"><img alt="Read the documentation" src="https://img.shields.io/badge/Read-the%20docs-0e9384?style=for-the-badge&logo=materialformkdocs&logoColor=white" height="36"></a>
  <a href="https://apps.microsoft.com/detail/9ppjw8h561lq?mode=direct"><img alt="Get it from Microsoft" src="https://get.microsoft.com/images/en-us%20dark.svg" height="36"></a>
</p>

<p align="center">
  <img src="https://assets.rustgis.app/images/hero.webp" alt="RustGIS with the Home tab of the ribbon open. The Layers pane lists Cities, Rivers, Lakes and Countries with their legends; the map shows the world in the Equal Earth projection, countries shaded by population, sized city dots and country labels." width="100%">
</p>

## Why RustGIS

- **Fast and small.** One native program, drawn on the GPU, with no runtime to install and no
  server to run.
- **Runs everywhere.** The same app on Windows, macOS and Linux, and compiled to WebAssembly at
  [web.rustgis.app](https://web.rustgis.app) with nothing to install.
- **Local and private.** Your files are read and written on your machine, and nothing is uploaded
  on its own. The network is used only when you ask for it: web basemaps, plugins, the GeoLibre
  tools and the AI model you pick, which receives your questions and the data it reads to answer
  them.
- **Everything is a command.** Every button in the ribbon is a command with an id. Script it from
  `rustgis-cli`, chain it in notebooks and models, use completion in the Script pane, or ask the
  AI Assistant to run it for you.
- **Familiar.** A ribbon laid out in the order of the work (Home, Data, Style, Edit, Analyze,
  View, Publish), Layers and Browser panes, an attribute table, and undo for every change.
- **Extensible.** Over 1,000 GeoLibre and Whitebox tools for terrain, hydrology, LiDAR and remote sensing, and
  sandboxed WebAssembly [plugins](docs/plugins.md) that add commands, tools and ribbon tabs.

## A tour

<table>
<tr>
<td width="50%" valign="top"><img src="https://assets.rustgis.app/images/symbology-table.webp" alt="The Style tab with the Symbology pane showing graduated colours and colour schemes; the larger African countries are selected in cyan on the map and highlighted in the attribute table below" width="100%"><p align="center"><sub><b>Symbology and tables.</b> Graduated colours, with the selection synced to the table.</sub></p></td>
<td width="50%" valign="top"><img src="https://assets.rustgis.app/images/assistant.webp" alt="The AI Assistant pane: asked for the five most populous African countries, it read the Countries table, selected them and zoomed to them" width="100%"><p align="center"><sub><b>AI Assistant.</b> Ask about your data or tell the map what to do.</sub></p></td>
</tr>
<tr>
<td width="50%" valign="top"><img src="https://assets.rustgis.app/images/buffer-history.webp" alt="The Analyze tab with a gallery of geoprocessing tools and the Tools pane showing Buffer; dissolved 300 km buffers around African cities" width="100%"><p align="center"><sub><b>Analysis.</b> A tool gallery, the Tools pane and 1,000+ GeoLibre tools.</sub></p></td>
<td width="50%" valign="top"><img src="https://assets.rustgis.app/images/raster.webp" alt="A shaded-relief elevation model of the Sierra Nevada with the Style tab open" width="100%"><p align="center"><sub><b>Rasters.</b> GeoTIFF and COG, with stretch, RGB and shaded-relief styles.</sub></p></td>
</tr>
<tr>
<td width="50%" valign="top"><img src="https://assets.rustgis.app/images/globe.webp" alt="The World Atlas drawn on a 3D globe with the View tab open" width="100%"><p align="center"><sub><b>Globe and projections.</b> A 3D globe, Equal Earth, Robinson or any EPSG code.</sub></p></td>
<td width="50%" valign="top"><img src="https://assets.rustgis.app/images/tiles.webp" alt="An OpenFreeMap vector style over Paris, with the Basemap gallery listing the built-in basemaps and the web tile layer" width="100%"><p align="center"><sub><b>Web basemaps.</b> XYZ, WMS and vector tiles, and MapLibre styles.</sub></p></td>
</tr>
<tr>
<td width="50%" valign="top"><img src="https://assets.rustgis.app/images/layout.webp" alt="A landscape A4 page layout with a map frame, title, legend, north arrow and scale bar, and the Publish tab open" width="100%"><p align="center"><sub><b>Layouts.</b> Map frames, legends, scale bars, map series and PDF export.</sub></p></td>
<td width="50%" valign="top"><img src="https://assets.rustgis.app/images/plugins.webp" alt="The Plugin Manager browsing the plugin registry, listing Basemaps, Hello RustGIS, Map Explorer and Vector Toolkit with their permissions" width="100%"><p align="center"><sub><b>Plugins.</b> Install sandboxed plugins from <a href="https://plugins.rustgis.app">plugins.rustgis.app</a>.</sub></p></td>
</tr>
<tr>
<td width="50%" valign="top"><img src="https://assets.rustgis.app/images/modelbuilder.webp" alt="ModelBuilder after a run: Cities buffered by 300 km clip the Rivers, and Countries are dissolved by continent; every node is ticked and the results are on the map" width="100%"><p align="center"><sub><b>ModelBuilder.</b> Chain tools on a node canvas and run them in one go.</sub></p></td>
<td width="50%" valign="top"><img src="https://assets.rustgis.app/images/web-data.webp" alt="The Sentinel-2 Explorer in the Web Data pane over Bern: the clearest recent scenes as cards with previews, their footprints drawn on the map" width="100%"><p align="center"><sub><b>Web Data.</b> STAC, Overture Maps, NASA and open-data portals, where the map is.</sub></p></td>
</tr>
<tr>
<td width="50%" valign="top"><img src="https://assets.rustgis.app/images/terrain.webp" alt="Mount Rainier in USGS imagery draped over elevation tiles, the map tilted 65°" width="100%"><p align="center"><sub><b>Terrain.</b> Tilt the map and drape it over elevation.</sub></p></td>
<td width="50%" valign="top"><img src="https://assets.rustgis.app/images/mars-globe.webp" alt="Mars on the 3D globe, on the Viking mosaic, with Valles Marineris across the middle" width="100%"><p align="center"><sub><b>Other worlds.</b> The Moon, Mars and nine more bodies, on their own basemaps.</sub></p></td>
</tr>
<tr>
<td width="50%" valign="top"><img src="https://assets.rustgis.app/images/extrusion.webp" alt="Countries extruded by population on a map tilted 55° and turned 20°, lit and shaded, with labels on their roofs" width="100%"><p align="center"><sub><b>Extrusion.</b> Stand features up by a field, lit and shaded.</sub></p></td>
<td width="50%" valign="top"><img src="https://assets.rustgis.app/images/time-slider.webp" alt="The Time Slider under the map at 2018, showing a layer of dated events cumulatively, with a bar for each year" width="100%"><p align="center"><sub><b>Time Slider.</b> Step dated features and imagery through time.</sub></p></td>
</tr>
<tr>
<td width="50%" valign="top"><img src="https://assets.rustgis.app/images/dark.webp" alt="RustGIS in dark mode with the View tab open over a map of Europe on the Midnight basemap" width="100%"><p align="center"><sub><b>Dark mode</b> and the View tab's effects, panes and linked views.</sub></p></td>
<td width="50%" valign="top"><img src="https://assets.rustgis.app/images/backstage.webp" alt="The Project backstage with cards to start a blank map, the World Atlas sample, or open a project" width="100%"><p align="center"><sub><b>Projects.</b> Start blank, from the World Atlas sample, or open a project.</sub></p></td>
</tr>
</table>

Every screenshot is RustGIS itself, rendered offscreen by `cargo run -p rustgis-ui-egui --example ui_shot`.

## Features

| Area | Highlights |
|---|---|
| **Data** | GeoJSON, CSV/TSV with x/y columns in any coordinate system or WKT geometry, Excel and OpenDocument spreadsheets, GeoRSS, LandXML, encoded polylines, Shapefile (zipped too, in the browser as well), GeoPackage, GeoParquet, FlatGeobuf, KML/KMZ (with styles), GPX, GML, MapInfo MIF/MID, OpenStreetMap PBF, TopoJSON, AutoCAD DXF, LiDAR point clouds (LAS, LAZ, COPC) and geotagged photos (large files are read in place), files by their web address, every vector format GDAL reads when it is installed, Esri File Geodatabases without GDAL (desktop), PostGIS, MySQL, MariaDB and SQL Server tables read live (desktop), and SQL queries and DuckDB databases through DuckDB Spatial when DuckDB is installed; PMTiles archives (web or file) and MBTiles files; GeoJSON, CSV, GeoPackage, GeoParquet, Shapefile, FlatGeobuf, KML/KMZ, GPX and WKT out; standalone tables; reusable connections; supported QGIS projects and layer styles; layer and map files, styles, templates and packages |
| **Rasters** | GeoTIFF and cloud-optimised GeoTIFF (any pixel type, tiled or striped, compressed), ASCII grids, Zarr 2 and 3 stores (a variable at a time step, by folder or web address), and JPEG 2000, NetCDF, HDF, ERDAS Imagine and the other rasters GDAL reads when it is installed (desktop); reprojection from any EPSG code; Stretch, RGB, Classify, Unique Values, Colormap and Shaded Relief; DRA, band combinations, brightness/contrast/gamma; raster functions, indices, change detection, classification, georeferencing; cloud-optimised GeoTIFF export; data cubes, point time-series charts, temporal summaries and trends |
| **Maps** | Multiple maps and layouts per project, renamed and duplicated from the Browser pane; tilt, rotation, terrain and a 3D globe; maps of the Moon, Mars and other celestial bodies; Web Mercator or any projection (Equal Earth, Robinson, Winkel Tripel, polar and regional presets, or an EPSG code); bookmarks, extent history, coordinate navigation; five offline basemaps, with borders in two worldviews |
| **Web services** | WMS, WMTS, WFS, OGC API Features and ArcGIS REST (FeatureServer, MapServer, ImageServer, VectorTileServer) from their capabilities |
| **Web tiles** | XYZ/TMS/WMS raster tiles, Mapbox Vector Tiles and MapLibre styles (sprites, patterns, expressions), cached and fetched in the background |
| **Cartography** | Single symbol, unique values, graduated colours, proportional symbols; label classes with SQL queries, straight and curved line placement, installed font families, scale ranges and annotation; stacked and dashed line strokes, symbol levels, reference scales and polygons without fill; masking, display filters, aggregation bins, 2.5D extrusion; graphics and dynamic text |
| **Editing** | Create, move, rotate, scale, merge, split and explode features; snapping, templates, elevation, traverse, topology rules; domains, subtypes, attribute rules, relationships, archiving; undo for everything |
| **Analysis** | Selection by attributes (SQL) and location; Buffer, Near, Clip, Erase, Split, Intersect, Identity, Union, Symmetrical Difference, Update, Dissolve, Summarize Within, Summary Statistics, Pivot Table, Tabulate Intersection, Spatial Join, Zonal Statistics, A5 grids and a geometry calculator; viewshed, line of sight, IDW, ordinary kriging, shortest path, suitability modelling; ModelBuilder on a node canvas and scripts with command, parameter and value completion; 1,000+ [GeoLibre](https://github.com/opengeos/geolibre-rust) and Whitebox raster, vector, LiDAR, hydrology and terrain tools |
| **Imagery** | Ortho and reality mapping, pixel classification (k-means, minimum distance, a trainable neural network), mensuration, hosted imagery tiles |
| **Layouts and reports** | Page layouts with map frames, legends, scale bars, grids and map series; publication sheets and figures in one command, format panes, guides, snapping and element locks; reports with groups, statistics, charts and subreports; presentations (HTML slideshow, PDF); notebooks (Jupyter in and out) |
| **Sharing** | PNG export at any DPI; PDF; web map pages, OGC API Features collections, MapLibre styles, glTF scenes, locators; project, map and mobile map packages; offline areas with sync |
| **AI Assistant** | Ask about your data or tell the map what to do in plain language, with Anthropic, OpenAI (a key, or your ChatGPT plan by signing in), Gemini, OpenRouter, Ollama or any OpenAI-compatible model; also `rustgis-cli ask`, and an [MCP server](docs/mcp.md) (`rustgis-cli mcp`) for Claude Code, Cursor, VS Code and other agents |
| **Plugins** | Sandboxed WebAssembly [plugins](docs/plugins.md) add commands, tools and ribbon tabs; install them from [plugins.rustgis.app](https://plugins.rustgis.app) on the desktop, command line and web |
| **Interface** | Layers, Browser and Data Sources panes as tabs, command search (Ctrl+K), keyboard shortcuts, dark mode, a customizable ribbon and Quick Access Toolbar, linked side-by-side maps, 19 languages |

The [user guide](https://rustgis.app/user-guide/) covers each area in detail,
[Comparison](https://rustgis.app/comparison/) sets RustGIS beside GeoLibre, QGIS and ArcGIS, and
[ROADMAP.md](ROADMAP.md) lists what's next.

## Get RustGIS

- **Web app**: open **[web.rustgis.app](https://web.rustgis.app)**. The same ribbon and tools,
  with the browser's file picker for data and projects, and downloads for Save and Export.
- **Desktop**: download the app for Windows, macOS or Linux from
  [downloads.rustgis.app](https://downloads.rustgis.app) or the
  [latest release](https://github.com/opengeos/RustGIS/releases/latest) (see
  [Installation](docs/installation.md) for the files and first-run notes).
- **Microsoft Store**: get RustGIS from the
  [Microsoft Store](https://apps.microsoft.com/detail/9ppjw8h561lq) on Windows 10 and 11, which
  installs it in one click and keeps it up to date.
- **Package managers**: `winget install OpenGeos.RustGIS`,
  `brew install --cask opengeos/rustgis/rustgis`, `yay -S rustgis-bin` (AUR), or
  `sudo dnf copr enable giswqs/rustgis && sudo dnf install rustgis` (Fedora).
- **From source**: `cargo run --release -p rustgis` (Rust 1.90 or later).

## Quick start

```sh
cargo run --release -p rustgis                       # the desktop app with the World Atlas sample
cargo run --release -p rustgis -- data.geojson       # open data (or a .rgis project)
```

Command line: every ribbon action is a command with an id.

```sh
rustgis-cli commands                                 # all command ids and parameters
rustgis-cli render countries.geojson map.png --basemap ocean
rustgis-cli run --sample \
  --cmd 'gp.run={"tool":"countPoints","params":{"polygons":"Countries","points":"Cities"}}' \
  --cmd 'layer.symbology={"layer":"Countries_Summarize","type":"graduated","field":"point_count"}' \
  --cmd 'share.exportMap={"path":"out.png","dpi":192}' --save atlas.rgis
rustgis-cli ask --sample "Which 5 countries have the largest GDP?"
claude mcp add rustgis -- rustgis-cli mcp --sample   # RustGIS as an MCP server for AI agents
```

See [Command Line](docs/cli.md) for more.

Python: the same engine and commands in-process (`pip install rustgis`), and the app itself in a
Jupyter notebook.

```python
import rustgis
s = rustgis.Session(sample=True)
s.run("gp.run", tool="buffer", params={"input": "Cities", "distance": 100})
s.to_png("map.png")
gdf = s.layer("Cities_Buffer").to_geopandas()
```

See [Python](docs/python.md) and the [getting-started notebook](apps/rustgis-python/examples/getting-started.ipynb)
([open in Colab](https://colab.research.google.com/github/opengeos/RustGIS/blob/main/apps/rustgis-python/examples/getting-started.ipynb)).

## Architecture

Layered crates, each depending only on the ones below it:

| Layer | Crate | Job |
|---|---|---|
| L0 | `rustgis-geom` | coordinates, extents, Web Mercator, geodesic distance/area, scale |
| L1 | `rustgis-data` | features and typed attributes; GeoJSON, CSV, Shapefile, KML, GPX and more; GeoPackage, GeoParquet, FlatGeobuf, LiDAR, PostGIS and MySQL providers; GDAL formats and DuckDB queries through their programs; SQL where clauses; rasters (GeoTIFF, ASCII grid, Zarr) and their coordinate systems |
| L2 | `rustgis-map`, `rustgis-analysis` | layers, symbology, labeling, camera, basemaps; geoprocessing (on `geo`) |
| L2 | `rustgis-tiles` | web map tiles: XYZ/TMS/WMS, Mapbox Vector Tiles, PMTiles and MBTiles archives, MapLibre styles, the tile cache |
| L3 | `rustgis-render` | tiny-skia renderer, label placement, PNG export, fonts |
| L3 | `rustgis-plugins` | sandboxed WebAssembly plugins and the plugin registry |
| L3 | `rustgis-geolibre` | the GeoLibre tool suite as geoprocessing tools |
| L4 | `rustgis-engine` | session, undo, command registry, sample project |
| L5 | `rustgis-assistant` | the AI Assistant: a chat agent that acts through commands |
| L6 | `rustgis-ui-egui` | the ribbon front end (swappable) |
| apps | `rustgis`, `rustgis-cli`, `rustgis-web`, `rustgis-python` | desktop, command line, browser (WebAssembly), the `rustgis` Python package |

`cargo xtask ci` runs formatting, clippy, the tests and the layering check. Contributor and
agent notes: [AGENTS.md](AGENTS.md).

## Sponsor

RustGIS is free and open source, and stays that way. If it is useful to you or your team, sponsorship is the most direct way to keep development, hosting and cross-platform distribution going.

- [**GitHub Sponsors**](https://github.com/sponsors/giswqs): monthly or one-time, billed through your GitHub account.
- [**Buy Me a Coffee**](https://buymeacoffee.com/giswqs): a quick one-off contribution, no account required.

See the [Become a Sponsor](https://rustgis.app/sponsor/) page for what sponsorship supports and for other, free ways to help.

## Citation

If you use RustGIS in your work, please cite it. RustGIS is archived on [Zenodo](https://zenodo.org/), which mints a DOI for every release. The concept DOI below always resolves to the latest version.

[![DOI](https://zenodo.org/badge/DOI/10.5281/zenodo.23633281.svg)](https://doi.org/10.5281/zenodo.23633281)

> Wu, Q., Rincón Albarracín, J. G., Filho, E., Hofbauer, D., Fitzgerald, M., & Strebel, D. (2026). RustGIS: A fast, free and open-source GIS in pure Rust for mapping, analyzing and sharing geospatial data. Zenodo. <https://doi.org/10.5281/zenodo.23633281>

You can also use GitHub's **"Cite this repository"** button (which reads [`CITATION.cff`](CITATION.cff)) to copy a ready-made APA or BibTeX entry. See the [How to Cite](https://rustgis.app/citation/) page for more formats.

## Star History

<a href="https://www.star-history.com/?repos=opengeos%2Frustgis&type=date&legend=top-left">
 <picture>
   <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/chart?repos=opengeos/rustgis&type=date&theme=dark&legend=top-left" />
   <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/chart?repos=opengeos/rustgis&type=date&legend=top-left" />
   <img alt="Star History Chart" src="https://api.star-history.com/chart?repos=opengeos/rustgis&type=date&legend=top-left" />
 </picture>
</a>

## Acknowledgements

RustGIS builds on the work of many projects and their authors: [QGIS](https://qgis.org), ArcGIS
Pro and [GDAL](https://gdal.org), the GIS it is measured against; [MapLibre](https://maplibre.org)
and [PROJ](https://proj.org); the GeoLibre tool suite, after John Lindsay's
[WhiteboxTools](https://www.whiteboxgeo.com); [OpenFreeMap](https://openfreemap.org) for vector
basemaps; [Natural Earth](https://www.naturalearthdata.com),
[OpenStreetMap](https://www.openstreetmap.org), [Overture Maps](https://overturemaps.org) and the
EPSG dataset; and the Rust crates it is made of, among them [egui](https://github.com/emilk/egui),
[wgpu](https://wgpu.rs), [tiny-skia](https://github.com/linebender/tiny-skia),
[geo](https://github.com/georust/geo), [proj4rs](https://crates.io/crates/proj4rs),
[tiff](https://github.com/image-rs/image-tiff), Apache Arrow and Parquet,
[rusqlite](https://github.com/rusqlite/rusqlite) and [wasmi](https://github.com/wasmi-labs/wasmi).
See the [acknowledgements](https://rustgis.app/acknowledgements/) for the full list, and
[ATTRIBUTION.md](ATTRIBUTION.md) for every asset's source and licence.

## Disclaimer

RustGIS is primarily built with [Claude Opus 5.5](https://www.anthropic.com/claude), an AI model by
Anthropic, working under human direction and review. Its results are tested, but check them for
your own use, and please [report](https://github.com/opengeos/RustGIS/issues) anything that looks
wrong.

## License and credits

Licensed under the [Apache License 2.0](LICENSE). Sample data from
[Natural Earth](https://www.naturalearthdata.com/) (public domain).
