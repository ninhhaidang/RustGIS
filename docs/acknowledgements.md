# Acknowledgements

RustGIS stands on the work of many projects and their authors. Thank you to all of them. If your
work is missing here, please [open an issue](https://github.com/opengeos/RustGIS/issues).

!!! note "Disclaimer"

    RustGIS is primarily built with [Claude Opus 5.5](https://www.anthropic.com/claude), an AI
    model by Anthropic, working under human direction and review. Its results are tested, but check
    them for your own use, and please [report](https://github.com/opengeos/RustGIS/issues) anything
    that looks wrong.

## The GIS that came before

[QGIS](https://qgis.org) and [ArcGIS Pro](https://www.esri.com/en-us/arcgis/products/arcgis-pro/overview)
set the expectations of a desktop GIS that RustGIS is measured against, and [GDAL](https://gdal.org)
those of reading geospatial formats. [MapLibre](https://maplibre.org) defines the style
specification RustGIS implements a subset of for vector tiles, and [PROJ](https://proj.org)
the coordinate operations it follows. [Zarr](https://zarr.dev) defines the chunked-array format
read by RustGIS's raster provider.

## Geoprocessing

The optional GeoLibre tool suite ([geolibre-rust](https://github.com/opengeos/geolibre-rust),
built on [whitebox-wasm](https://github.com/opengeos/whitebox-wasm)) brings more than 1,000 raster,
vector, LiDAR and hydrology tools, after John Lindsay's
[WhiteboxTools](https://www.whiteboxgeo.com).

The suite runs separately from RustGIS: through an optional downloaded `geolibre` program on
the desktop and command line, or a WebAssembly runner loaded on demand in the browser.

## Data

- [Natural Earth](https://www.naturalearthdata.com) (public domain) for the sample data.
- The [EPSG Geodetic Parameter Dataset](https://epsg.org) of IOGP, PROJ's database and Esri's
  [projection-engine-db-doc](https://github.com/Esri/projection-engine-db-doc) for coordinate
  systems.
- [OpenFreeMap](https://openfreemap.org), whose free vector tiles and styles (such as Liberty)
  draw RustGIS's vector basemaps.
- [OpenStreetMap](https://www.openstreetmap.org) and its contributors,
  [Overture Maps](https://overturemaps.org) and [Protomaps](https://protomaps.com) for basemaps
  and open map data.
- The scientific colour ramps of matplotlib (Viridis and family, Cividis), Google (Turbo), Fabio
  Crameri's [Scientific colour maps](https://www.fabiocrameri.ch/colourmaps/) and
  [cmocean](https://matplotlib.org/cmocean/).

[ATTRIBUTION.md](https://github.com/opengeos/RustGIS/blob/main/ATTRIBUTION.md) lists every asset
with its source and licence.

## Rust crates

RustGIS is written in Rust, built on these crates, among others:

| Crate | For |
|---|---|
| [egui and eframe](https://github.com/emilk/egui), [wgpu](https://wgpu.rs) | The interface, natively and in the browser, and the GPU map |
| [tiny-skia](https://github.com/linebender/tiny-skia), [lyon](https://github.com/nical/lyon), [ab_glyph](https://crates.io/crates/ab_glyph), [image](https://github.com/image-rs/image), [epaint_default_fonts](https://crates.io/crates/epaint_default_fonts) | CPU drawing, tessellation, text, images and fallback interface fonts |
| [geo](https://github.com/georust/geo), [geozero](https://github.com/georust/geozero), [shapefile](https://crates.io/crates/shapefile), [a5](https://crates.io/crates/a5) | Geometry, formats and the A5 grid |
| [terrio-crs](https://github.com/opengeos/terrio) | Coordinate systems |
| [tiff](https://github.com/image-rs/image-tiff), [laz](https://crates.io/crates/laz) | GeoTIFF and LAZ |
| [Apache Parquet and Arrow](https://github.com/apache/arrow-rs), [flatgeobuf](https://crates.io/crates/flatgeobuf), [rusqlite](https://github.com/rusqlite/rusqlite) ([SQLite](https://sqlite.org)), [calamine](https://crates.io/crates/calamine), [csv](https://crates.io/crates/csv), [roxmltree](https://crates.io/crates/roxmltree) | GeoParquet (Arrow writes test fixtures), FlatGeobuf, GeoPackage, spreadsheets, CSV and XML |
| [postgres](https://crates.io/crates/postgres), [mysql](https://crates.io/crates/mysql), [tiberius](https://crates.io/crates/tiberius), [rustls](https://github.com/rustls/rustls) | PostgreSQL/PostGIS, MySQL/MariaDB, SQL Server and TLS |
| [miniz_oxide](https://crates.io/crates/miniz_oxide), [brotli-decompressor](https://crates.io/crates/brotli-decompressor), [ruzstd](https://crates.io/crates/ruzstd), [zstd](https://crates.io/crates/zstd), [lz4_flex](https://crates.io/crates/lz4_flex), [weezl](https://crates.io/crates/weezl), [lerc-reader](https://crates.io/crates/lerc-reader), [lerc-core](https://crates.io/crates/lerc-core), [zip](https://crates.io/crates/zip) | Compression for rasters, Zarr chunks, map tiles, GeoParquet and plugin packages |
| [wasmi](https://github.com/wasmi-labs/wasmi) | Sandboxed WebAssembly plugins |
| [ureq](https://crates.io/crates/ureq), [serde](https://serde.rs) | HTTP and JSON |
| [PyO3](https://pyo3.rs), [wasm-bindgen](https://github.com/rustwasm/wasm-bindgen), [rfd](https://crates.io/crates/rfd) | The Python package, the web app and file dialogs |

The full dependency list is in [Cargo.toml](https://github.com/opengeos/RustGIS/blob/main/Cargo.toml),
the crate and application manifests, and
[Cargo.lock](https://github.com/opengeos/RustGIS/blob/main/Cargo.lock). Dependency licences and
copyright notices are collected in
[THIRD-PARTY-LICENSES.txt](https://github.com/opengeos/RustGIS/blob/main/THIRD-PARTY-LICENSES.txt),
with a [separate list for Python](https://github.com/opengeos/RustGIS/blob/main/apps/rustgis-python/THIRD-PARTY-LICENSES.txt).
