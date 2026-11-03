# Fuzzing the file readers

Coverage-guided fuzz targets for the readers that take files RustGIS doesn't control (#779). They
feed malformed bytes to each reader and the crates beneath it, looking for panics, out-of-memory
allocations and hangs that hand-written hostile-input tests don't anticipate.

| Target | Calls | Covers |
|---|---|---|
| `read_layers` | `io::read_layers`, the first byte picking the extension from `READ_EXTENSIONS` | GeoJSON, CSV, KML/KMZ, GPX, GML, DXF, OSM PBF, TopoJSON, MapInfo, zipped Shapefiles, GeoPackage, GeoParquet, FlatGeobuf, LAS/LAZ and the rest |
| `sniff` | `io::sniff_extension`, then `raster::read_bytes` for a raster or `read_layers` | files dropped without an extension |
| `geotiff` | `raster::geotiff::read` | GeoTIFF and the `tiff` crate |
| `wkt` | `raster::wkt::read_wkt` | `.prj` files, GeoPackage spatial reference rows |
| `wkb` | `wkb::decode` | GeoPackage, GeoParquet and PostGIS geometry |
| `mvt` | `rustgis_tiles::mvt::decode` | vector tiles |
| `esripbf` | `esripbf::is_esripbf`, then `esripbf::read` | ArcGIS feature-service answers as protocol buffers (`f=pbf`) |
| `project` | `Project::from_json` | `.rgis` projects |

This crate is its own workspace, outside RustGIS's: `cargo build`, `cargo xtask ci`, the wasm
build and the workspace's `cargo deny` never build it. It needs nightly Rust and
[cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz). `libfuzzer-sys` compiles libFuzzer (C++,
NCSA licence), which `deny.toml` here allows; nothing in this crate is shipped.

## Running it

From this folder:

```sh
rustup toolchain install nightly
cargo install cargo-fuzz
cargo +nightly fuzz build -O
${CARGO_TARGET_DIR:-target}/x86_64-unknown-linux-gnu/release/seeds   # writes corpus/<target>/
cargo +nightly fuzz run -O read_layers -- -max_total_time=600 -rss_limit_mb=2048 -timeout=10
```

The seeds are well-formed files written by RustGIS's own writers from the sample project, so
fuzzing starts from real files; the repository commits no data. `corpus/` and `artifacts/` are
ignored. Put `CARGO_TARGET_DIR` somewhere roomy: a sanitizer build of the engine is several GB.

CI runs `.github/workflows/fuzz.yml`: every target for ten minutes a night, and for a minute on a
pull request labelled `ci:fuzz`. It runs libFuzzer in fork mode with `-ignore_crashes`,
`-ignore_ooms` and `-ignore_timeouts`, so a known failure doesn't stop the rest of a target being
explored. To do the same locally, add `-fork=1 -ignore_crashes=1 -ignore_ooms=1 -ignore_timeouts=1`
(the ignore flags work only in fork mode). Failing inputs fail the target, are uploaded,
and at night open a "Fuzzing found a crash" issue.

## A failure

libFuzzer writes the input to `artifacts/<target>/` (`crash-…`, `oom-…` or `timeout-…`).
Reproduce it with `cargo +nightly fuzz run -O <target> artifacts/<target>/<file>`, and shrink it
with `cargo +nightly fuzz tmin -O <target> artifacts/<target>/<file>`. Fix the reader in RustGIS's
own code, then add the input as a regression test that runs in plain `cargo test`, next to the
reader's other hostile-input tests: generate it in the test when it's small, as those tests do.

When a dependency is at fault and its fix isn't released yet, guard the call in RustGIS if that
can be done cheaply. Otherwise leave the issue open, pointing at the dependency's fix.
