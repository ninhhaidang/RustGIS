# Attribution

Every asset file in this repository, with its source and licence. The licences of the Rust
crates RustGIS is built from, with their copyright notices, are in
[THIRD-PARTY-LICENSES.txt](THIRD-PARTY-LICENSES.txt) (the Python package's in
[apps/rustgis-python/THIRD-PARTY-LICENSES.txt](apps/rustgis-python/THIRD-PARTY-LICENSES.txt)),
which every package ships next to LICENSE. They are generated from `Cargo.lock` by
`cargo xtask notices` (it needs `cargo install --locked --features cli cargo-about@0.9.2`);
`deny.toml` lists the licences a dependency may have. The code and data RustGIS's own crates
take from other projects (the colour maps, coordinate-system tables and borrowed code below) are
listed in `INCLUDED` in [xtask/src/notices.rs](xtask/src/notices.rs), which adds them to the
same files: each with its licence texts, kept in [third-party/](third-party/) as published, and
the EPSG tables with the acknowledgement of IOGP's ownership and a link to the dataset's terms
of use.

| File | Author / source | Licence |
|---|---|---|
| Paired binary reader-loss fixtures (`crates/engine/testdata/reading-losses/xy.*`, `xyz.*`) | The RustGIS project: one made-up point, written offline with [GDAL](https://gdal.org) 3.13.3 (FlatGeobuf and FileGDB) and [PyArrow](https://arrow.apache.org/docs/python/) 26.0.1 (GeoParquet); [fixture notes](crates/engine/testdata/reading-losses/README.md) describe the minimal data and retained files | Apache-2.0 |
| Historical release projects and preferences (`crates/engine/testdata/releases/v*.rgis`, `v*.prefs.json`) | The RustGIS project: unchanged output of the published binaries, with made-up layer data. The [fixture README](crates/engine/testdata/releases/README.md) lists writers and archive checksums. | Apache-2.0 |
| Shapefile Z/M reader-loss fixtures (`crates/engine/testdata/reading-losses/shp-*`) | The RustGIS project: independently encoded, made-up point and line records following the published Shapefile specification; byte layout in [SHAPEFILE.md](crates/engine/testdata/reading-losses/SHAPEFILE.md) | Apache-2.0 |
| XLSX reader-loss fixtures (`crates/engine/testdata/reading-losses/book-*.xlsx`) | The RustGIS project: made-up one-row workbooks, encoded as Office Open XML with Python standard-library ZIP/XML writing; [fixture notes](crates/engine/testdata/reading-losses/README.md#xlsx) describe their contents | Apache-2.0 |
| ODS reader-loss fixtures (`crates/engine/testdata/reading-losses/book-*.ods`) | The RustGIS project: made-up one-row OpenDocument workbooks, independently encoded with Python standard-library ZIP/XML writing; [fixture notes](crates/engine/testdata/reading-losses/README.md#ods) describe their contents | Apache-2.0 |
| XLS/XLSB reader-loss fixtures (`crates/engine/testdata/reading-losses/book-*.xls`, `book-*.xlsb`) | The RustGIS project: made-up one-row workbooks generated offline with SheetJS CE 0.18.5; XLSB package parts were minimized; except in `book-short.xlsb`, short cell records were independently re-encoded to the published full-cell layout; [fixture notes](crates/engine/testdata/reading-losses/README.md#xls-and-xlsb) describe contents and checks | Apache-2.0 |
| Feature-service PBF reader-loss fixtures (`crates/engine/testdata/reading-losses/feature-*.pbf`) | The RustGIS project: independently encoded, made-up point answers following the published FeatureCollection protocol buffer schema; [fixture notes](crates/engine/testdata/reading-losses/README.md#feature-service-pbf) describe contents and checks | Apache-2.0 |
| OSM PBF ring-loss fixtures (`crates/engine/testdata/reading-losses/hole-*.osm.pbf`) | The RustGIS project: independently encoded, made-up nodes, ways and relation from the published OSM PBF schema; contents and checksums in [OSM.md](crates/engine/testdata/reading-losses/OSM.md) | Apache-2.0 |
| Interface themes (`crates/ui-egui/themes/light.json`, `dark.json`) | The RustGIS project | Apache-2.0 |
| Sample data (`countries`, `cities`, `rivers`, `lakes` GeoJSON), embedded at build time from [rustgis-assets](https://github.com/opengeos/rustgis-assets/tree/main/data) | Natural Earth 1:110m, attributes trimmed | Public domain |
| Worldviews (`countries_<code>.geojson` and `original/countries_<code>.geojson.gz` on [source.coop](https://source.coop/opengeos/data/rustgis/worldviews)), downloaded by the app when one is chosen, not part of it | Natural Earth's Admin 0 Countries (1:10m) and its points of view, with the sample countries' attributes by `scripts/worldviews.py` in [rustgis-assets](https://github.com/opengeos/rustgis-assets): simplified to 1:110m density, or at their original detail | Public domain |
| QGIS screenshot (`qgis-world-atlas.webp` in [rustgis-assets](https://github.com/opengeos/rustgis-assets/tree/main/images)), on the [RustGIS and QGIS](docs/comparison/qgis.md) page | Taken by the RustGIS project: [QGIS](https://qgis.org) 4.2 showing the Natural Earth sample data above | QGIS is GPL-2.0-or-later; the data is public domain |
| QGIS test corpus (`crates/map/testdata/qgis/generated/`: styles, projects and their sample data) | Dominik Strebel ([@pick2510](https://github.com/pick2510)) for the RustGIS project: written by [QGIS](https://qgis.org) 4.2.3 through its API, from `generate.py` there; the raster image marker styles and their two images (`marker.png`, `marker.jpg`) added the same way by the RustGIS project, as were the SVG marker styles and project (QGIS 4.4.0) and their two SVGs (`icon.svg`, `marker.svg`, written by `generate.py` itself); the data is made up | Apache-2.0 |
| CFF test font (`crates/render/testdata/test-cff.otf`), read only by the PDF font tests | The RustGIS project: generated for this test with [fontTools](https://github.com/fonttools/fonttools) by the script in `crates/render/testdata/README.md`; its glyphs are made up | Apache-2.0 |
| Script-only and fallback test fonts (`crates/render/testdata/test-script-only.otf`, `crates/render/testdata/test-fallback.otf`, `crates/render/testdata/test-latin-catalog.otf`) | The RustGIS project: original square outlines and script cmap variants ([generation](crates/render/testdata/README.md)) | Apache-2.0 |
| JAXA Earth catalog test copy (`crates/tiles/tests/data/jaxa_catalog.v2.md`), read only by tests | JAXA: the [JAXA Earth API](https://data.earth.jaxa.jp/) dataset catalog ([catalog.v2.md](https://data.earth.jaxa.jp/app/mcp/catalog.v2.md)), copied on 2026-11-08 | JAXA's [terms of use](https://earth.jaxa.jp/en/data/policy/): free to use and redistribute, with credit to JAXA |
| CityGML test mesh (`crates/data/tests/data/rocking-chair.gml`), read only by tests | 97 faces of the rocking chair in OGC's CityGML 3.0 sample `Building_LOD3-EPSG25832.gml` ([CityGML-3.0Encodings](https://github.com/opengeospatial/CityGML-3.0Encodings/blob/master/Moved_to_CITYGML-3.0Encoding_CityGML/Examples/Examples-from-CityGML-2.0/building/CityGMLv3/Building_LOD3-EPSG25832.gml)), copyright (c) 2013 3DXScape, copied on 2026-11-12 and rewritten as a CityGML 2.0 object, coordinates unchanged | MIT |
| Datum grid test files (`crates/data/tests/data/grids/us_noaa_conus.tif`, `us_noaa_ethpgn.tif`), read only by the datum grid tests | NOAA's NADCON and HARN grids as converted by [PROJ-data](https://github.com/OSGeo/PROJ-data) (`us_noaa`), copied unchanged from [cdn.proj.org](https://cdn.proj.org) | Public domain |
| LERC GeoTIFF test files (`crates/data/tests/data/lerc/*.tif`), read only by the LERC tests | Written for the RustGIS project by `make_lerc.py` there with [GDAL](https://gdal.org) 3.9.3 through [rasterio](https://rasterio.readthedocs.io) 1.4.3, run only to write them; the values are made up | Apache-2.0 |
| Analysis reference corpus (`crates/analysis/tests/reference/`: layers and expected results) | Written for the RustGIS project from `generate.py` there: the layers are made up; the expected results were computed by [GEOS](https://libgeos.org) 3.13.1 through [Shapely](https://shapely.readthedocs.io) 2.1.2, and distances, buffers, areas and lengths by GeographicLib in [PROJ](https://proj.org) 9.5.1 through [pyproj](https://pyproj4.github.io/pyproj/) 3.7.2, run only to write them (GEOS is LGPL-2.1, Shapely BSD-3-Clause, PROJ and pyproj MIT; none is included in or linked into RustGIS) | Apache-2.0 |

Coordinate systems are looked up by EPSG and ESRI code in the catalog of
[terrio-crs](https://github.com/opengeos/terrio) (see below), written as PROJ definitions as GDAL
3.13.3 writes them. Two EPSG codes it doesn't compute (EPSG:3752 and 22700, in
`crates/data/src/raster/crs.rs`) keep the definitions RustGIS read them by before, from PostGIS's
`spatial_ref_sys`, itself derived from the
[EPSG Geodetic Parameter Dataset](https://epsg.org) (owned by IOGP, under its
[terms of use](https://epsg.org/terms-of-use.html)): PROJ definitions that approximate it, not the
dataset itself. Two ESRI codes it lacks (ESRI:10820 and 65162, in the same file) are
Esri's projected coordinate systems from Esri's
[projection-engine-db-doc](https://github.com/Esri/projection-engine-db-doc)
(Apache-2.0), as imported into the [PROJ](https://proj.org) database (MIT) and written as
PROJ definitions by PROJ 9.7.1.
Coordinates are transformed by [terrio-crs](https://github.com/opengeos/terrio) (MIT), which
carries its own tables derived from the EPSG Geodetic Parameter Dataset (© IOGP, under its terms
of use) through PROJ's database; its NOTICE, with their sources, is in THIRD-PARTY-LICENSES.txt.
The WKT2 test corpus (`crates/data/src/raster/wkt2_corpus.tsv`) is 84 of those EPSG and Esri
coordinate systems written as WKT2 by PROJ 9.7.1 through pyproj 3.7.2 (the Italian and Swiss
rows by PROJ 9.8.1).
The ESRI datum test corpus (`crates/data/src/raster/esri_datum_corpus.tsv`) contains 42 EPSG
coordinate systems written as ESRI WKT1, 13 of them through pyproj 3.7.2, 17 Italian ones by
GDAL 3.13 and 12 on datums known by their Esri name by a PROJ 9.9.0 build. Reference coordinates
in both corpora are PROJ 9.8.1's (pyproj 3.8.0) for each row's WKT.

The GeoLibre tool suite ([geolibre-rust](https://github.com/opengeos/geolibre-rust), MIT, built
on [whitebox-wasm](https://github.com/opengeos/whitebox-wasm), MIT OR Apache-2.0) is not part of
RustGIS: the desktop app and command line run its separately downloaded `geolibre` runner, and
the web app loads its WebAssembly runner (the `geolibre-wasm` npm package, from jsDelivr, which
carries its own licence and third-party notices) when asked. Its tool names, descriptions and
categories come from the runner's manifests.

The scientific colour ramps (`crates/map/src/color/scientific.rs`) are sampled from published
colour maps: Viridis, Plasma, Inferno and Magma by Stéfan van der Walt and Nathaniel Smith (CC0),
and Cividis by Jamie R. Nuñez, Christopher R. Anderton and Ryan S. Renslow, as
[matplotlib](https://matplotlib.org) ships them (matplotlib licence); Turbo by Anton Mikhailov,
Copyright 2019 Google LLC (Apache-2.0); Batlow, Vik, Roma and Oleron from Fabio Crameri's
[Scientific colour maps](https://www.fabiocrameri.ch/colourmaps/), Copyright (c) 2020 Fabio
Crameri (MIT, through [cmcrameri](https://github.com/callumrollo/cmcrameri)); and Balance,
Haline, Deep and Topo from [cmocean](https://matplotlib.org/cmocean/), Copyright (c) 2015
Kristen M. Thyng (MIT). The Matplotlib group's colour maps (`crates/map/src/color/matplotlib.rs`)
are sampled from [rio-tiler](https://github.com/cogeotiff/rio-tiler)'s tables, Copyright (c) 2021
cogeotiff (BSD-3-Clause): matplotlib's own maps (matplotlib licence; its
ColorBrewer maps, such as Blues and RdYlBu, Copyright (c) 2002 Cynthia Brewer, Mark Harrower and
The Pennsylvania State University, Apache-2.0; its gist maps, such as gist_earth, from Yorick,
Copyright (c) 1996 The Regents of the University of California, under a BSD-style licence),
cmocean's (MIT, as above) and rio-tiler's cfastie, rplumbo and schwarzwald. The full licence
texts are in THIRD-PARTY-LICENSES.txt.

Three pieces of code follow other projects' code, and their licences are in
THIRD-PARTY-LICENSES.txt too: the reader of ArcGIS feature-service answers as protocol buffers
(`crates/data/src/esripbf.rs`) follows [Terrio](https://github.com/opengeos/terrio)'s ESRIPBF reader
(Copyright (c) 2026 Qiusheng Wu and Terrio contributors, MIT), and both follow Esri's published
[`FeatureCollection.proto`](https://github.com/Esri/arcgis-pbf) (Apache-2.0); where a label goes in an area (`pole_of_inaccessibility` in
`crates/tiles/src/symbols.rs`) follows [polylabel](https://github.com/mapbox/polylabel)
(Copyright (c) 2016 Mapbox, ISC); and telling expression filters from legacy ones
(`is_expression_filter` in `crates/tiles/src/style.rs`) follows the
[MapLibre style specification](https://github.com/maplibre/maplibre-style-spec) (Copyright (c)
2020 MapLibre contributors and Mapbox, BSD-3-Clause).

Icons are original vector drawings in code (`crates/ui-egui/src/icons.rs`). Fonts are not
bundled: the interface uses an installed system sans-serif when one is found, otherwise the
Ubuntu Light face that ships inside `epaint_default_fonts` (Ubuntu Font Licence 1.0). For an
interface language those faces cannot draw, the app downloads a fallback from
[rustgis-assets](https://github.com/opengeos/rustgis-assets/tree/main/fonts), where each font's
licence is kept beside it: Noto Sans SC, TC, JP or KR for Simplified Chinese, Traditional Chinese,
Japanese or Korean; Noto Sans for extended Latin, Noto Sans Bengali, Noto Sans Lao; and
Vazirmatn for Arabic script (all SIL Open Font License 1.1).
