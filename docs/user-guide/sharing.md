# Sharing

## Exporting

**Publish › Output › Export Map** writes the map view as a PNG with labels, the scale bar and
tile credits. The ribbon exports at 192 DPI; `share.exportMap` accepts `width` and `height`
in pixels and a `dpi` value (96 by default). **Print Map** opens a PDF of the map view, kept as
vectors as a layout's PDF is (see Layouts), in the system viewer on desktop; **Capture To
Clipboard** copies a map image. **Export To 3D** writes a layer as a Wavefront OBJ model with
polygons extruded by a field.

**Data › Export** (or `share.exportLayer`, **Export Features**) writes a layer to GeoJSON, CSV,
GeoPackage, GeoParquet, Shapefile (including a zip), FlatGeobuf, KML, KMZ or GPX (see
[Writing](data-formats.md#writing)). GeoPackage, Shapefile and FlatGeobuf use the layer's source
coordinate system by default; the `crs` parameter chooses another. A streamed layer exports its
whole source, not just the features loaded for the view. The web app downloads every format but
GeoPackage and FlatGeobuf. `share.exportTable` exports a standalone
table. **Data › Export** also writes rasters to GeoTIFF, and **Data Interop** converts layers to
KML, GPX or CSV with WKT. Layouts, reports and presentations export from their own tabs and panes
(see [Layouts and Reports](layouts.md)).

GeoPackage exports require a new filename. Exporting to an existing `.gpkg` is refused to
protect its tables and data, even if the save dialog offers to replace the file. Export does
not append a table to an existing GeoPackage or replace one of its tables.

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
