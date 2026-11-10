# QGIS file corpus

By Dominik Strebel ([@pick2510](https://github.com/pick2510)), for the RustGIS project.

QGIS projects (`.qgs`, `.qgz`) and layer styles (`.qml`) for testing the QGIS importer
(`crates/map/src/qgis/`, the `project.importQgis` and `style.importQgis` commands). QGIS publishes
no specification of these formats, so the corpus stands in for one: `every_corpus_file_reads` in
`src/qgis/tests.rs` reads each file, and the files show what real projects contain.

| Folder | What | Licence |
|---|---|---|
| `generated/qml/` | 97 styles written by QGIS 4.2.3 through its API, each varying one thing: every symbol layer type with its defaults, every renderer (categorized and graduated by expression, rule-based with nested and else rules), every paint effect, data-defined size, angle and colour, labels (every placement, buffer, background, shadow, font, wrap, expression text, offsets, data-defined settings, scale ranges, curved labels, each callout, rule-based labels), layer settings (opacity, blend mode, scale range, reference scale, subset), diagrams (pie, by fields and expressions, a histogram, a stacked bar and a text diagram, two of them sized between two values) and raster renderers (grey, pseudocolour interpolated, discrete and exact, multiband, paletted, hillshade, contour) | Apache-2.0, like RustGIS: made by Dominik Strebel ([@pick2510](https://github.com/pick2510)) for the RustGIS project |
| `generated/qgs/` | 21 projects: one per provider (OGR, GDAL, memory, delimited text by x/y and WKT, SpatiaLite, virtual, WMS, XYZ, WFS, WCS, ArcGIS feature and map server, vector tiles, PostGIS, SQL Server, GPX); project structure (groups, a mutually exclusive group, a map theme, a bookmark, a variable, a relation, a join, a virtual field, time from a field, two styles on a layer); a layout with every item and an atlas; the annotation layer with every item; `all-styles.qgz`, every style above on its own layer (written before the diagrams were added) | Apache-2.0 |
| `generated/data/` | The data those files use: `sample.gpkg` (layers `points`, `lines` and `polygons`, 9 made-up features each, fields `id`, `name`, `kind`, `value`, `angle`, `day`), `dem.tif` (3 bands of a linear surface), two CSVs, a GPX file | Apache-2.0 |
| `generated/generate.py`, `scrub.py` | The generator, and the step that removes the user's name and absolute paths from what QGIS writes | Apache-2.0 |
| `generated/types.txt` | The element types QGIS 4.2.3 knows (symbol layers, renderers, providers, label placements, callouts, annotation and layout items, paint effects): the checklist the corpus was made from. A few have no file yet: the single-colour raster renderer, and the text table, fixed table, elevation profile and chart layout items | Apache-2.0 |

## Not included

- **The SMV25 project and its data.** The repository does not bundle third-party vendor styles.
  For a large real-world project to test manually, download the GeoPackage version and its
  QGIS project from
  [swisstopo](https://www.swisstopo.admin.ch/en/national-map-swiss-map-vector-25) (also on
  [opendata.swiss](https://opendata.swiss/en/dataset/swiss-map-vector-25-vektorielle-landeskarte-der-schweiz-1-25-000))
  and place the project beside `SMV25_CHLV95LN02.gpkg` and the four rasters in
  `SMV25_CHLV95LN02_RASTER/` (about 760 MB) to open the whole map. These external files are
  outside the automated corpus test.
- **The QGIS Training Manual's projects and styles**, which were part of the audit: they are
  GPL-2.0, so they live in [qgis/QGIS-Training-Data](https://github.com/qgis/QGIS-Training-Data)
  (`exercise_data/`, 36 `.qgs`, `.qgz` and `.qml` files from QGIS 1.8 to 3.x).
- **`sample.sqlite`**, the SpatiaLite copy of `sample.gpkg` the SpatiaLite project reads (7 MB,
  mostly SpatiaLite's table of coordinate systems): `generate.py` writes it.
- The service and database layers (WFS, WCS, ArcGIS, PostGIS, SQL Server) point at
  `example.invalid` or `db.invalid`; QGIS writes them as configured, without data. The PostGIS
  and SQL Server sources carry a made-up password (`secret`), to test that the importer drops it.

## Regenerating

QGIS 4.2.3 (here the Flathub Flatpak, whose `/tmp` is private, so it writes under `~/.cache`):

```sh
mkdir -p ~/.cache/qgis-corpus
flatpak run --env=PYTHONPATH=/app/share/qgis/python --env=QT_QPA_PLATFORM=offscreen \
  --command=python3 org.qgis.qgis generated/generate.py ~/.cache/qgis-corpus/out
```

It writes `data/`, `qml/`, `qgs/`, `types.txt` and `log.txt` (what was written, and what this
QGIS couldn't write) into the output folder, then runs `scrub.py` on the projects. QGIS may crash
on exit, after writing everything. Copy the files here without `log.txt`, `sample.sqlite`,
`dem.tif.aux.xml`, `all-styles.qgs` (the `.qgz` holds it) and the `_attachments.zip` files QGIS
writes beside each project.
