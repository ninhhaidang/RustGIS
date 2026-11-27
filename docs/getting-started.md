# Getting Started

## Open the app

- **In the browser**: open [web.rustgis.app](https://web.rustgis.app). Nothing to install.
- **On the desktop**: [install](installation.md) RustGIS and start it. On first launch it opens
  the World Atlas sample: countries, cities, rivers and lakes from Natural Earth.
- **From source**:

```sh
cargo run --release -p rustgis -- --sample    # always opens the World Atlas sample
cargo run --release -p rustgis -- places.geojson
cargo run --release -p rustgis -- atlas.rgis   # a saved project
cargo run --release -p rustgis -- lesson.rgispkg   # a project package, opened unsaved
```

## Five minutes with the World Atlas

If you already changed the start-up settings, use **Project › New › World Atlas** to open the
sample again. **Settings › Start-up › Start With** chooses what opens next time.

1. **Explore.** Drag to pan, scroll to zoom, and click a country to see its attributes in a
   pop-up. Press ++home++ for the full extent, or pick a place from **Home › Bookmarks**.
2. **Style a layer.** Click *Countries* in the Layers pane, then open **Style › Symbology**.
   In the Symbology pane, choose *Graduated Colors*, set *Field* to `pop_est` and pick a colour
   scheme. *Unique Values* instead colours categories such as `continent`.
3. **Select.** Open **Home › Select By Attributes**, choose *Countries* under *Input Rows*,
   enter `continent = 'Asia'` under *Where*, and press *Apply*. Open the attribute table with
   ++ctrl+t++. Rows and map features stay in sync.
4. **Analyze.** Open **Analyze › Buffer**. Set *Input Features* to *Cities*, *Distance* to
   `300`, *Units* to *Kilometers* and *Dissolve* to *All*, then press *Run*. The result is added
   as a new layer, and the run is listed under **Analyze › History**.
5. **Change the view.** **View › Globe** turns the map into a 3D globe; **View › Projection**
   offers Equal Earth, Robinson and other projections. **Home › Add › Basemap** switches the basemap or
   adds a web tile layer, and its **Celestial Bodies** take the map to the Moon or Mars.
   Right-drag (or ++ctrl++-drag) turns and tilts the map; click the north arrow to reset it.
6. **Share.** **Publish › Export Map** saves a PNG with labels and a scale bar. **Home › New
   Layout** offers page sizes and **Publication Layout** presets for sheets (in one of seven styles) and figures.
   Choose a page size for a page with a map frame, legend, north arrow and scale bar.
   **Project › Save As** writes a `.rgis` project.

!!! tip "Command search"
    Press ++ctrl+k++ to find any command or geoprocessing tool by name.
    On macOS, use ++cmd++ in place of ++ctrl++ for shortcuts.

## Your own data

**Home › Add › Add Data** opens data files directly; its arrow offers other ways to add data,
including delimited text, URLs and services. Shapefiles work in the browser too: pick or drop
the `.shp` and `.dbf` together, with `.shx` and `.prj` if available, or use a ZIP containing
them. **Home › Add › Data Sources** opens saved data connections and recent sources.

**Project › New › Map** starts a blank project. **Home › Add › Web Data** finds public satellite imagery,
Overture Maps, OpenStreetMap, NASA Earthdata, open-data portals and more
(see [Web Data](user-guide/web-data.md)). Desktop database connections include PostGIS,
MySQL/MariaDB and SQL Server/Azure SQL. See [Data](user-guide/data.md) and
[Supported Data Formats](user-guide/data-formats.md) for format and platform limits.

## Ask the AI Assistant

**Analyze › AI Assistant** opens a chat pane. Choose a model provider (Anthropic, OpenAI,
your ChatGPT account, Gemini, OpenRouter, a local Ollama, or any OpenAI-compatible server) and ask, for example,
"Which 5 countries have the largest GDP?" or "Label the cities and zoom to Tokyo".
By default, the assistant asks before file, network and plugin operations. Undo restores
project changes, not files already written. See [AI Assistant](user-guide/assistant.md).

## Next steps

- The [User Guide](user-guide/index.md) walks through every part of the app.
- [Command Line](cli.md) shows how to script RustGIS with `rustgis-cli`.
- [Plugins](plugins.md) shows how to add tools from the plugin registry, or write your own.
- [Python](python.md) covers sessions, rendering and the interactive notebook map.
- [MCP](mcp.md) connects external AI agents to RustGIS.
