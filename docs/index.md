# RustGIS

**A fast, free and open-source GIS written in Rust for mapping, analyzing and sharing geospatial
data.**

RustGIS runs natively on Windows, macOS and Linux, and in your web browser. Local files are
processed on your machine; web data sources, AI providers and plugins can use the network.
Map operations are commands you can script from the command line, Python or the AI Assistant.

<div class="hero-actions" markdown>

[Try it in your browser](https://web.rustgis.app){ .md-button .md-button--primary }
[Download](installation.md){ .md-button }
[Get started](getting-started.md){ .md-button }
[Compare](comparison/index.md){ .md-button }
[![Get it from Microsoft](https://get.microsoft.com/images/en-us%20dark.svg)](https://apps.microsoft.com/detail/9ppjw8h561lq?mode=direct)

</div>

<figure markdown="span">
  [![RustGIS with the Home tab of the ribbon open over a world map in the Equal Earth projection, countries shaded by population](https://assets.rustgis.app/images/hero.webp)](https://assets.rustgis.app/images/hero.webp){ target="_blank" rel="noopener" title="Open the full-size image" }
  <figcaption>RustGIS with the World Atlas sample in the Equal Earth projection.</figcaption>
</figure>

## Highlights

<div class="grid cards" markdown>

-   **Your data, any source**

    GeoJSON, Shapefile, GeoPackage, GeoParquet, GeoTIFF, LiDAR and Zarr; on the desktop, File
    Geodatabases and live PostGIS, MySQL, MariaDB and SQL Server; STAC catalogs, Overture Maps and
    open-data portals; saved connections and QGIS projects.
    [Data](user-guide/data.md) · [Web Data](user-guide/web-data.md) · [Formats](user-guide/data-formats.md)

-   **Cartography and 3D**

    Graduated colours, symbols by expression, curved labels, stacked symbols, publication layouts
    and map series; a 3D globe, projection presets, terrain and extrusions, and maps of the Moon,
    Mars and other worlds.
    [Cartography](user-guide/cartography.md) · [Maps](user-guide/maps.md) · [Layouts](user-guide/layouts.md)

-   **Analysis**

    SQL selection, overlay and proximity tools, guided workflows, ModelBuilder, imagery tools,
    spatial join, kriging, zonal statistics, data-cube time series and trends, and over 1,000
    tools through the optional GeoLibre/Whitebox suite.
    [Analysis](user-guide/analysis.md) · [Expressions](user-guide/expressions.md)

-   **Editing**

    Create features and edit their vertices with snapping and templates; move, rotate, merge,
    split and explode them; keep attributes valid with domains, subtypes and rules; check
    topology, and undo project edits.
    [Editing](user-guide/editing.md) · [Data design](user-guide/editing.md#data-design)

-   **AI Assistant**

    Ask about your data or tell the map what to do. Choose a model provider or sign in with
    ChatGPT; add skills, or hand tasks to a coding agent on the desktop. File, network and
    plugin operations wait for your approval.
    [AI Assistant](user-guide/assistant.md) · [Skills](user-guide/assistant.md#skills)

-   **Scriptable and extensible**

    `rustgis-cli` and the `rustgis` Python package run commands and render maps; MCP connects AI
    agents to headless sessions or the open app. Plugins add tools, commands and assistant skills,
    sandboxed in WebAssembly or as unsandboxed desktop processes.
    [Command Line](cli.md) · [Python](python.md) · [MCP](mcp.md) · [Plugins](plugins.md)

</div>

## A tour

<div class="grid" markdown>

<figure markdown="span">
  [![The Style tab and the Symbology pane, with the larger African countries selected](https://assets.rustgis.app/images/symbology-table.webp)](https://assets.rustgis.app/images/symbology-table.webp){ target="_blank" rel="noopener" title="Open the full-size image" }
  <figcaption><strong>Symbology and tables.</strong> Graduated colours, with the selection synced to the table.</figcaption>
</figure>

<figure markdown="span">
  [![The AI Assistant selecting the five most populous African countries](https://assets.rustgis.app/images/assistant.webp)](https://assets.rustgis.app/images/assistant.webp){ target="_blank" rel="noopener" title="Open the full-size image" }
  <figcaption><strong>AI Assistant.</strong> Ask about your data or tell the map what to do.</figcaption>
</figure>

<figure markdown="span">
  [![The Analyze tab's tool gallery and dissolved buffers around African cities](https://assets.rustgis.app/images/buffer-history.webp)](https://assets.rustgis.app/images/buffer-history.webp){ target="_blank" rel="noopener" title="Open the full-size image" }
  <figcaption><strong>Analysis.</strong> A tool gallery, the Tools pane and 1,000+ GeoLibre tools.</figcaption>
</figure>

<figure markdown="span">
  [![ModelBuilder: a model of buffer, clip and dissolve after a run](https://assets.rustgis.app/images/modelbuilder.webp)](https://assets.rustgis.app/images/modelbuilder.webp){ target="_blank" rel="noopener" title="Open the full-size image" }
  <figcaption><strong>ModelBuilder.</strong> Chain tools on a node canvas and run them in one go.</figcaption>
</figure>

<figure markdown="span">
  [![A shaded-relief elevation model with the Style tab](https://assets.rustgis.app/images/raster.webp)](https://assets.rustgis.app/images/raster.webp){ target="_blank" rel="noopener" title="Open the full-size image" }
  <figcaption><strong>Rasters.</strong> GeoTIFF and COG, with stretch, RGB and shaded-relief styles.</figcaption>
</figure>

<figure markdown="span">
  [![The World Atlas on the 3D globe](https://assets.rustgis.app/images/globe.webp)](https://assets.rustgis.app/images/globe.webp){ target="_blank" rel="noopener" title="Open the full-size image" }
  <figcaption><strong>Globe and projections.</strong> A 3D globe, Equal Earth, Robinson and supported EPSG/ESRI systems.</figcaption>
</figure>

<figure markdown="span">
  [![Mount Rainier in USGS imagery draped over terrain, tilted](https://assets.rustgis.app/images/terrain.webp)](https://assets.rustgis.app/images/terrain.webp){ target="_blank" rel="noopener" title="Open the full-size image" }
  <figcaption><strong>Terrain.</strong> Tilt the map and drape it over elevation.</figcaption>
</figure>

<figure markdown="span">
  [![Mars on the 3D globe, on the Viking mosaic](https://assets.rustgis.app/images/mars-globe.webp)](https://assets.rustgis.app/images/mars-globe.webp){ target="_blank" rel="noopener" title="Open the full-size image" }
  <figcaption><strong>Other worlds.</strong> The Moon, Mars and nine more bodies, on their own basemaps.</figcaption>
</figure>

<figure markdown="span">
  [![Countries extruded by population on a tilted map](https://assets.rustgis.app/images/extrusion.webp)](https://assets.rustgis.app/images/extrusion.webp){ target="_blank" rel="noopener" title="Open the full-size image" }
  <figcaption><strong>Extrusion.</strong> Stand features up by a field, lit and shaded.</figcaption>
</figure>

<figure markdown="span">
  [![An OpenFreeMap vector style over Paris with the Basemap gallery](https://assets.rustgis.app/images/tiles.webp)](https://assets.rustgis.app/images/tiles.webp){ target="_blank" rel="noopener" title="Open the full-size image" }
  <figcaption><strong>Web basemaps.</strong> XYZ, WMS and vector tiles, and MapLibre styles.</figcaption>
</figure>

<figure markdown="span">
  [![The Sentinel-2 Explorer in the Web Data pane, with scene footprints on the map](https://assets.rustgis.app/images/web-data.webp)](https://assets.rustgis.app/images/web-data.webp){ target="_blank" rel="noopener" title="Open the full-size image" }
  <figcaption><strong>Web Data.</strong> STAC, Overture Maps, NASA and open-data portals, where the map is.</figcaption>
</figure>

<figure markdown="span">
  [![The Time Slider showing dated events through 2018](https://assets.rustgis.app/images/time-slider.webp)](https://assets.rustgis.app/images/time-slider.webp){ target="_blank" rel="noopener" title="Open the full-size image" }
  <figcaption><strong>Time Slider.</strong> Step dated features and imagery through time.</figcaption>
</figure>

<figure markdown="span">
  [![A page layout with a map frame, legend, north arrow and scale bar](https://assets.rustgis.app/images/layout.webp)](https://assets.rustgis.app/images/layout.webp){ target="_blank" rel="noopener" title="Open the full-size image" }
  <figcaption><strong>Layouts.</strong> Map frames, legends, scale bars, map series and PDF export.</figcaption>
</figure>

<figure markdown="span">
  [![The Plugin Manager browsing the plugin registry](https://assets.rustgis.app/images/plugins.webp)](https://assets.rustgis.app/images/plugins.webp){ target="_blank" rel="noopener" title="Open the full-size image" }
  <figcaption><strong>Plugins.</strong> Install tools, commands and assistant skills from <a href="https://plugins.rustgis.app">plugins.rustgis.app</a>.</figcaption>
</figure>

<figure markdown="span">
  [![Dark mode over Europe on the Midnight basemap](https://assets.rustgis.app/images/dark.webp)](https://assets.rustgis.app/images/dark.webp){ target="_blank" rel="noopener" title="Open the full-size image" }
  <figcaption><strong>Dark mode</strong> and the View tab's effects, panes and linked views.</figcaption>
</figure>

<figure markdown="span">
  [![The Project backstage](https://assets.rustgis.app/images/backstage.webp)](https://assets.rustgis.app/images/backstage.webp){ target="_blank" rel="noopener" title="Open the full-size image" }
  <figcaption><strong>Projects.</strong> Start blank, from the World Atlas sample, or open a project.</figcaption>
</figure>

</div>
