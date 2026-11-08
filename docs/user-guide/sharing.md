# Sharing

## Exporting

**Publish › Output › Export Map** writes the map view as a PNG with labels, the scale bar and
tile credits. The ribbon exports at 192 DPI; `share.exportMap` accepts `width` and `height`
in points, with the whole view fitted in them, and a `dpi` value (96 by default). A point is a
pixel at 96 DPI, so `"width": 400` at 192 DPI is an image 800 pixels wide.

Export Map to a `.tif` path writes the view as a GeoTIFF instead: an RGB image placed on the
ground in the map's projection, so another GIS opens it where it belongs. It holds the layers,
tile and WMS layers included, and the labels, but not the scale bar or the credits. The credits go
into the file's Copyright tag, which holds plain ASCII: `©` and `®` are written as `(c)` and `(R)`,
other characters as `?` and control characters as spaces, and the export says which. Its answer
gives them in full as `copyright`. `"cog": true` writes a
cloud-optimised GeoTIFF. A turned map is drawn north up over the box around the view (and says
so). A tilted map, the globe and a map of another body are refused: export a PNG of those. One
side holds at most 8192 pixels, so a large `width` or `height` needs a lower `dpi`; this holds for a
PNG too, which is refused past it rather than cut off. With `"fit": true` an export too large for
its `dpi` is drawn at the most that fits, and says so; the ribbon exports that way, at 192 DPI.
`dpi` runs from 48 to 600 and a side from 16 points; a value outside is drawn at the limit, and
said. An export holds
at most 8000 points a side, PNG or GeoTIFF: with no `width` or `height`, a larger view is cut to
its middle; a `width` or `height` past 8000 is drawn at 8000, holding less ground than asked (the whole
view still fits). Either way the
export says so. In a GeoTIFF,
tiles still on their way when the export stops waiting, streamed layers still reading the view,
and tile layers that can't be drawn leave gaps that would pass for data, so its export warns
about them. The web app's Export Map downloads a PNG. A projection
whose parameters GeoTIFF's keys can't hold (Mollweide, for example) is refused too:
switch the map to an EPSG projection first.

**Export Raster** writes a tile layer alone too: right-click a WMS, WMTS or XYZ layer (or a
raster tile archive) in the Layers pane and choose **Export Raster…**, or pick it and use
**Data › Export Raster**. Its own tiles over the view, at the zoom of the view's scale, are put together
as an RGBA GeoTIFF in Web Mercator (EPSG:3857): a pixel of the file for a pixel of a tile, not
resampled, with the layer's transparency and its credits in the Copyright tag. In a script,
`share.exportRaster` takes `"tiles"` (the tile layer's name, or its index in the Layers pane, 0
at the top), `"bbox"` (`[west, south, east, north]` in degrees, the view's by default), `"zoom"`
(the tiles' zoom, 0 to 30) and `"cog"`. A zoom past the service's deepest enlarges its tiles, and
says so. An export fetches at most 1024 tiles and holds at most 8192 pixels a side: past either it
is refused, naming a zoom that fits, as many tile services' terms forbid bulk downloads
(OpenStreetMap's tile usage policy, for one). Tiles that haven't come after a wait (up to a minute for
each block of up to 2048 pixels), or can't be fetched or drawn (a server's "no tile here"
included), leave gaps, and the export warns with how many; so does a box where it drew nothing.
The layer's opacity is a display setting and isn't applied: the file keeps the tiles' own alpha. Vector tiles, and styles that draw them or draw tiles from more
than one source, are refused: Export Map to a `.tif` writes what they draw. A style drawing only
one source's raster tiles is written as those tiles. The web app exports the view with Export Map instead.

**Print Map** opens a PDF of the map view, kept as
vectors as a layout's PDF is (see Layouts), in the system viewer on desktop; **Capture To
Clipboard** copies a map image. **Export To 3D** writes a layer as a Wavefront OBJ or binary STL model
with polygons extruded by a field (`share.export3d` writes STL when `path` ends in `.stl`). STL
holds closed solids only: lines and zero-height features are left out, zero-area triangles are
skipped, and a cap that can't be triangulated is fanned over its outer ring, filling any holes;
the notices say how many. OBJ
uses metres east, up and south of the layer's south-west corner (Y up). STL uses metres east and
north of the model's own south-west corner, which includes the cubes drawn for points and so can
lie past the layer's, and up from the features' base (Z up, as slicers expect; a negative height
goes below 0). Neither has a CRS; STL stores them as unitless 32-bit floats, so set the unit
(metres) when you import it.

**Data › Export** (or `share.exportLayer`, **Export Features**) writes a layer to GeoJSON, CSV,
GeoPackage, GeoParquet, Shapefile (including a zip), FlatGeobuf, KML, KMZ or GPX (see
[Writing](data-formats.md#writing)). GeoPackage, Shapefile and FlatGeobuf use the layer's source
coordinate system by default; the `crs` parameter chooses another. A streamed layer exports its
whole source, not just the features loaded for the view. The web app downloads every format but
GeoPackage and FlatGeobuf. `share.exportTable` exports a standalone
table. **Data › Export** also writes rasters to GeoTIFF, and **Data Interop** converts layers to
KML, GPX or CSV with WKT. Layouts, reports and presentations export from their own tabs and panes
(see [Layouts and Reports](layouts.md)).

On the desktop, Export Features, Export Raster, Export Map and Export Layout write their file in
the background, so the map and the rest of the window stay in use. While one runs, the status bar
shows its name, a progress bar (Export Raster's untiled GeoTIFF, Export Map and a one-page layout
show none) and **Cancel**; when it ends its messages appear as before. One export runs at a time.
Export Map and Export Layout draw the map and layout as they were when the export started, so
moving the map or editing the layout meanwhile doesn't change the file. A cancelled export writes
nothing, and a file it was to replace stays as it was. Scripts, models, the AI Assistant and the
command line still wait for the file. Printing a layout, and the exports of reports, presentations
and animations, still run before the window goes on. So does Export Map while a style added with
its own view is still loading: it waits for the map to move there, without progress or **Cancel**.

A GeoPackage export writes a new file by default, and refuses an existing `.gpkg` to protect its
tables, even if the save dialog offers to replace the file. To put the layer into an existing
GeoPackage instead, choose GeoPackage in **Export Features**, then under **Into** either
**A new table in a GeoPackage** or **A table replaced in a GeoPackage**, name the table, and
pick the file (on the desktop). The GeoPackage's other tables stay as they were: one table is
added, or one feature or attribute table of that name is replaced (a feature table with its
spatial index; an attribute table has none), in a single transaction, so a failed export changes
nothing. A name already used is refused when
adding (with no file there, adding makes a new GeoPackage). Replacing needs a feature or
attribute table of that name, so a misspelt name changes nothing; tiles, views and other tables
are never replaced, nor is a table in a related-tables relation. A replaced table's own indexes,
triggers and column descriptions go with it, and the export names any views or triggers elsewhere
that use it. The GeoPackage's own triggers don't run while the export writes. A layer read in
place from the GeoPackage it is exported into is read whole first, rather than a chunk at a
time, as SQLite can't read and write one file in the same export.
From the command line, `share.exportLayer` and `share.exportTable` take `"mode": "addTable"` or
`"replaceTable"` and an optional `"tableName"` (default: the layer's or table's name). The name is
written as a GeoPackage allows: control characters are removed, surrounding spaces trimmed, an
empty name becomes `layer`, and one starting with `gpkg_`, `sqlite_` or `rtree_` (reserved for the
GeoPackage's own tables) gets `layer_` in front. Adding and replacing look for that name, and the
command's result gives it as `table`:

```sh
rustgis-cli run --data permits.geojson \
  --cmd 'share.exportLayer={"layer":"permits","path":"county.gpkg","mode":"addTable"}'
```

Packages and project item files are described under [Project items](data.md#project-items).

## Sharing as open formats

**Publish › Share As** writes open-format files to a chosen file on desktop, or downloads them
in the browser. The Share As commands accept a `path`; without one they use the first
**data store** (a folder added with **Add Data Store…** in the Shared Items pane).

- **Web Map** (`share.webMap`): an HTML page of visible layers (rasters as embedded images),
  pan and zoom, a layer list and pop-ups. It uses a flat Mercator view, not the project's
  projection, and does not include the basemap's web tiles.
- **Web Layer** (`share.webLayer`): the selected layer as a static OGC API – Features collection,
  with its style and a viewer page, in a zip. **Geodata Service** (`share.geodataService`) writes
  the map's non-raster, non-graphics layers and the project's standalone tables as collections.
  These are files, not running services with query or editing endpoints.
- **Web Style** (`share.webStyle`): the selected layer's symbology as a MapLibre style (unique
  values and classes as expressions). Labels use only the Default label class, without its SQL
  clause; a hidden Default class exports no labels. The same limit applies to a Web Layer's style.
- **Locator** (`share.locator`): the selected layer's names as a `.rgisloc` place-search file;
  text fields are searched by default, or specify `fields`. `locator.find` searches it and can
  zoom to the best match with `"zoom": true`.
- **Web Tool** (`share.webTool`): the last model as a `.rgistool` file, which `model.import` adds
  to another project; `model` chooses a model by name.
- **Web Scene** (`share.webScene`): the visible vector layers as glTF, polygons extruded by their
  layer's extrusion setting, with a camera that looks at the map's centre from its bearing and pitch.
- **Web Link Chart** (`share.linkChart`): the features joined by the map's relationships as a
  node-link HTML page; `relationship` limits it to one relationship.

**Publish › Share As › Jobs** (the Shared Items pane) lists desktop shares; **Save Web Map**,
**Replace Web Layer**, **Save Web Scene** and **Save Web Link Chart** update a share in place
(`share.saveWebMap`, `share.replaceWebLayer`, `share.saveWebScene`, `share.saveWebLinkChart`).
The `item` parameter chooses the share by ID or name; omitting it uses the latest of that kind.
**Upload File** (`share.upload`) copies a file into a data store; it does not upload to a remote server.

After opening a project, register its data stores again before writing to them, and use
**Share As** to choose each item's destination again before updating it. Saved share records do
not grant permission to overwrite their paths. Undo changes the project record, not files
already written.
