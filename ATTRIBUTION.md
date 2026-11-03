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
| Sample data (`countries`, `countries_chn`, `cities`, `rivers`, `lakes` GeoJSON), embedded at build time from [rustgis-assets](https://github.com/opengeos/rustgis-assets/tree/main/data) | Natural Earth 1:110m, attributes trimmed; `countries_chn` is Natural Earth's China point of view (1:10m, simplified) | Public domain |
| QGIS screenshot (`qgis-world-atlas.webp` in [rustgis-assets](https://github.com/opengeos/rustgis-assets/tree/main/images)), on the [RustGIS and QGIS](docs/comparison/qgis.md) page | Taken by the RustGIS project: [QGIS](https://qgis.org) 4.2 showing the Natural Earth sample data above | QGIS is GPL-2.0-or-later; the data is public domain |
| QGIS test corpus (`crates/map/testdata/qgis/generated/`: styles, projects and their sample data) | Dominik Strebel ([@pick2510](https://github.com/pick2510)) for the RustGIS project: written by [QGIS](https://qgis.org) 4.2.3 through its API, from `generate.py` there; the data is made up | Apache-2.0 |
| Analysis reference corpus (`crates/analysis/tests/reference/`: layers and expected results) | Written for the RustGIS project from `generate.py` there: the layers are made up; the expected results were computed by [GEOS](https://libgeos.org) 3.13.1 through [Shapely](https://shapely.readthedocs.io) 2.1.2, and distances, buffers, areas and lengths by GeographicLib in [PROJ](https://proj.org) 9.5.1 through [pyproj](https://pyproj4.github.io/pyproj/) 3.7.2, run only to write them (GEOS is LGPL-2.1, Shapely BSD-3-Clause, PROJ and pyproj MIT; none is included in or linked into RustGIS) | Apache-2.0 |

Raster coordinate systems are looked up by EPSG code in the PROJ definitions of the
[crs-definitions](https://crates.io/crates/crs-definitions) crate (CC0-1.0), compiled into the
app. Those definitions come from PostGIS's `spatial_ref_sys`, itself derived from the EPSG Geodetic
Parameter Dataset (© IOGP). The codes it lacks (EPSG 9020–20003,
`crates/data/src/raster/epsg_gap.rs`) are derived from the
[EPSG Geodetic Parameter Dataset](https://epsg.org) v12.029 (owned by IOGP, under its
[terms of use](https://epsg.org/terms-of-use.html)): converted to PROJ definitions by PROJ 9.8,
some of them approximately, so they are not the dataset itself. The Equi7 grids' codes
(EPSG 27701–27707, in the same file) are derived from the dataset's v13.106, written as PROJ
definitions of its Azimuthal Equidistant method.
The axis order of those systems (`crates/data/src/raster/epsg_axes.rs`: which are written north
or latitude first) is from the EPSG Geodetic Parameter Dataset v11.022 (© IOGP, under its terms
of use), read from the PROJ 9.5.1 database through pyproj 3.7.2.
The map's ESRI codes (`crates/data/src/raster/esri.rs`) are Esri's projected coordinate systems
from Esri's [projection-engine-db-doc](https://github.com/Esri/projection-engine-db-doc)
(Apache-2.0), as imported into the [PROJ](https://proj.org) 9.7.1 database (MIT) and written as
PROJ definitions by PROJ 9.7.1.
The WKT2 test corpus (`crates/data/src/raster/wkt2_corpus.tsv`) is 60 of those EPSG and Esri
coordinate systems written as WKT2 by PROJ 9.7.1, through pyproj 3.7.2.
The ESRI datum test corpus (`crates/data/src/raster/esri_datum_corpus.tsv`) contains 13 EPSG
coordinate systems written as ESRI WKT1 through pyproj 3.7.2; reference coordinates use PROJ
with the bundled `crs-definitions` Helmert definitions. The OSGB36, Amersfoort and GGRS87 WKT2
reference coordinates use those same transformations.

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

Four pieces of code follow other projects' code, and their licences are in
THIRD-PARTY-LICENSES.txt too: the reader of ArcGIS feature-service answers as protocol buffers
(`crates/data/src/esripbf.rs`) follows [Terrio](https://github.com/opengeos/terrio)'s ESRIPBF reader
(Copyright (c) 2026 Qiusheng Wu and Terrio contributors, MIT), and both follow Esri's published
[`FeatureCollection.proto`](https://github.com/Esri/arcgis-pbf) (Apache-2.0); where a label goes in an area (`pole_of_inaccessibility` in
`crates/tiles/src/symbols.rs`) follows [polylabel](https://github.com/mapbox/polylabel)
(Copyright (c) 2016 Mapbox, ISC); telling expression filters from legacy ones
(`is_expression_filter` in `crates/tiles/src/style.rs`) follows the
[MapLibre style specification](https://github.com/maplibre/maplibre-style-spec) (Copyright (c)
2020 MapLibre contributors and Mapbox, BSD-3-Clause); and the sinusoidal projection's meridian distance
(`crates/geom/src/sinusoidal.rs`) follows [PROJ](https://proj.org)'s `pj_enfn`, `pj_mlfn` and
`pj_inv_mlfn` (MIT).

Icons are original vector drawings in code (`crates/ui-egui/src/icons.rs`). Fonts are not
bundled: the interface uses an installed system sans-serif when one is found, otherwise the
Ubuntu Light face that ships inside `epaint_default_fonts` (Ubuntu Font Licence 1.0). For an
interface language in a script those faces lack, the web app downloads one from
[rustgis-assets](https://github.com/opengeos/rustgis-assets/tree/main/fonts), where each font's
licence is kept beside it: Noto Sans SC, JP or KR for Chinese, Japanese or Korean, and Vazirmatn
for Arabic script (both SIL Open Font License 1.1).
