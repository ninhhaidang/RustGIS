# Sharing

## Exporting

**Publish › Output › Export Map** writes the map view as a PNG with labels, the scale bar and
tile credits. The ribbon exports at 192 DPI; `share.exportMap` accepts `width` and `height`
in pixels and a `dpi` value (96 by default). **Print Map** opens a PDF of the map view, kept as
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
