# RustGIS — instructions for agents

RustGIS is a fast, free and open-source GIS in pure Rust for mapping, analyzing and sharing
geospatial data, on the desktop and in the browser. Its architecture: layered crates, everything
is a command, a thin egui front end, headless screenshots for UI work.

Before picking work, read [ROADMAP.md](ROADMAP.md) (check Underway) and
[CONTRIBUTING.md](CONTRIBUTING.md) (what to coordinate first).

## Never crash
- No `unwrap()`, `expect()`, `panic!`, `unreachable!`, `todo!` in non-test code; no `unsafe`
  (`unsafe_code = "forbid"`). Crate roots carry the `#![deny(clippy::unwrap_used, …)]` block.
- Input from files and command parameters is hostile: `get()` over indexing, clamp sizes,
  bound recursion. `Session::run` and `analysis::run` catch escaped panics as a last resort.
- `crates/engine/src/tests.rs::hostile_params_never_panic` and the analysis equivalent run every
  command/tool with junk parameters. Keep them green; every crash fix gets a regression test.

## Never silent
- A reader, writer, tool or command that drops, truncates or approximates data says what, how
  much and where: Z values dropped, field names cut, a CRS assumed, features skipped.
- Report through `Session::notify` (app, CLI, Python `take_notices`) or `ToolResult::messages`,
  one notice per kind with a count; `eprintln!` alone doesn't count. Unusable results are errors.
- Test both ways: lossy input gives the notice, clean input none. Existing silent losses are bugs.
- A file or setting from a newer RustGIS is refused or kept unchanged, never saved again without
  what this version couldn't read; a reader checks the version it is given
  ([saved files](docs/maintaining.md#saved-files-and-settings)).

## Never hold the window
On the desktop, a command whose work grows with its input (features, cells, tiles, files,
requests) runs off the UI thread, with progress and Cancel (#1562). The browser has one thread and
is out of scope.
- Use an existing background path: tools go through `gp::tool_start`; exports through
  `exports::runs_in_background`; commands that make a raster through
  `workflows::runs_in_background`; models and scripts through the copy (`workflows::ahead_on_copy`),
  with the command on `ON_A_COPY` when running it twice changes nothing outside the session.
- The work looks at Cancel between items of its long loops (`control::check`, `Pace`,
  `progress::check`) and counts its progress. Where progress can't be measured, a busy indicator
  without a count is fine; Cancel is not optional.
- Work that splits into independent parts runs on several threads where practical: the same
  output as one thread, bit for bit; one thread below a size threshold and in the browser; the
  shared helpers (`parallel::for_each_row`, `try_map`, `try_map_on`); the thread count from
  `rustgis_data::threads` (it follows the user's setting), never `available_parallelism` or a
  fixed number; scratch memory within `memory::thread_scratch`; a thread the OS refuses costs
  speed, not the run (`Builder::spawn_scoped`).
- A command that must run on the UI thread says why in its doc comment.
- `crates/engine/src/tests.rs::long_commands_stop_when_cancelled_while_they_run` cancels each
  background command as its work starts and fails one that never looks; its lists must match the
  background registries, so a new background command is added there. It also runs the long tools
  (Buffer, Dissolve, Spatial Join, Clip, Erase, Intersect, Identity, Union, Symmetrical
  Difference, Update, Split, Kernel Density, Kriging); a new tool
  whose work grows with its input is added to that list. Passing shows the work looks
  at Cancel, not that it looks often: a long step without a check (one union, one read) is said in
  its doc comment.

## Architecture
| Layer | Crate | Job |
|---|---|---|
| L0 | `geom` | coordinates, extents, Web Mercator, geodesic measurement, scale |
| L1 | `data` | features, typed attributes, GeoJSON/CSV/Shapefile/KML/GPX I/O, GeoPackage/GeoParquet/FlatGeobuf/LiDAR (LAS/LAZ)/PostGIS/MySQL providers (`source`; PostGIS through the pure-Rust `postgres` client, MySQL and MariaDB through the pure-Rust `mysql` client, desktop only; GeoPackages in the browser through the pure-Rust `sqlite` reader; GDAL formats and DuckDB queries through their programs), SQL where clauses, rasters (GeoTIFF/ASCII grid, Zarr stores, EPSG reprojection) |
| L2 | `map`, `analysis` | layers, symbology, labels, camera, basemaps, web tile layers; geoprocessing tools |
| L2 | `tiles` | web map tiles: XYZ/TMS/WMS rasters, Mapbox Vector Tiles, PMTiles and MBTiles archives, a MapLibre style subset; the tile cache and background fetching (see `docs/user-guide/maps.md` › Basemaps) |
| L2 | `keychain` | the system's secret store (macOS Keychain, Windows Credential Manager, Secret Service) behind `SecretStore`; the desktop app, the CLI's `--use-saved-secrets` and Python's `use_saved_secrets` |
| L3 | `render` | tiny-skia map renderer, label placement, PNG export, font discovery |
| L3 | `plugins` | plugins: sandboxed WebAssembly modules (`wasmi`) adding commands and tools; `.rgplugin` packages, the plugin registry (see `docs/plugins.md`) |
| L3 | `geolibre` | the GeoLibre/Whitebox tool suite as geoprocessing tools (engine feature `geolibre`): run by the optional `geolibre` program natively, by geolibre-rust's WASI runner in the browser |
| L4 | `engine` | `Session`, undo, command registry, sample project |
| L5 | `assistant` | the AI Assistant: a chat agent (sans I/O: `provider` builds and reads OpenAI Chat Completions/Responses, Anthropic and Gemini requests, `chatgpt` signs in with ChatGPT to use the user's plan, `agent` loops, `tools` run commands; file, network and plugin commands wait for the user's approval) |
| L6 | `ui-egui` | ribbon, panes, map view, attribute table, dialogs (swappable) |
| apps | `rustgis`, `rustgis-cli`, `rustgis-web`, `rustgis-python` | desktop app, command line, browser (WebAssembly), the `rustgis` Python package (PyO3, built with maturin) |

- **Everything is a command**: a `CommandSpec` in `crates/engine/src/cmd/*.rs`. UI-only
  commands (`ui.*`) live in `crates/ui-egui/src/lib.rs`. The ribbon, shortcuts, command search
  and CLI dispatch by id. Commands never open dialogs.
- **Command ingress**: root `Session::run` is trusted native User dispatch. Assistant, MCP,
  plugin, imported-item and continuation adapters carry their named opaque policy scope;
  scopes inherit or narrow, and `with_scope` rejects nested ingress. Peer project text uses
  `open_peer_text`. See [embedding-page limitations](SECURITY.md#embedding-page-command-boundary)
  when changing web message handlers.
- **Ribbon placement** follows the
  [ribbon guidelines](docs/contributing.md#ribbon-guidelines): the tab's purpose, one button per
  destination, labels that say what happens, and what a ribbon pull request states.
- **Never break wasm**: everything below `apps/` must build for `wasm32-unknown-unknown`
  (CI runs `cargo build --target wasm32-unknown-unknown -p rustgis-web`; the Zstandard decoder in
  `rustgis-data` is C, so this needs clang). Use `web_time`, not
  `std::time`; file access goes through `Services` on the web. `rustgis-geolibre` builds for
  both: natively it runs the `geolibre` program; in the browser the web app runs geolibre-rust's
  WASI runner asynchronously (`external_start` → `Services::geolibre_run` → `gp.finish`).
- **Interface text is translatable**: wrap user-visible literals in `tl!("…")`, use
  `i18n::fmt("… {name} …", &[("name", …)])` instead of `format!` for messages, and pass labels to
  the ribbon widgets (they translate). Pull requests don't need to add the new strings to the
  catalogs in `crates/ui-egui/src/i18n/*.tsv`: a missing entry shows in English, and the
  maintainer translates them before each release (format in `docs/i18n.md`). Command ids, data
  and the CLI are never translated.
- **Layering** is enforced by `cargo xtask layers`: nothing below L6 depends on egui/eframe/rfd.

## Assets
- No third-party vendor icons, images, styles, fonts, help text or code. Icons are drawn in
  code (`icons.rs`). Every asset file has a row in `ATTRIBUTION.md`.
- Never copy or translate code from GPL, LGPL or other copyleft projects (such as GRASS, QGIS or
  GEOS) into RustGIS, by hand or with an AI tool. Implementing a published algorithm is fine: cite
  the paper or standard. Name in the pull request any project whose code you followed (see
  [CONTRIBUTING.md](CONTRIBUTING.md#where-code-comes-from)).
- Screenshots and other images are not committed here: they live in
  [opengeos/rustgis-assets](https://github.com/opengeos/rustgis-assets) (WebP, served at
  `https://assets.rustgis.app/images/…`) and docs link to them by URL.

## Quality gates
`cargo xtask ci` (fmt, clippy -D warnings, tests, layers) before every commit.
A new dependency must have a licence `deny.toml` allows (CI runs `cargo deny check licenses`); after
changing dependencies, run `cargo xtask notices` to refresh the `THIRD-PARTY-LICENSES.txt` files the
packages ship (the release build fails on stale ones).
Pull requests run CI on Linux; pushes to `main` add macOS and Windows (x64 and ARM64). Label a
pull request `ci:windows`, `ci:macos` or `ci:full` to test there before merging (platform-specific
code, paths, file dialogs). The release build of the apps runs nightly
(`.github/workflows/nightly.yml`), with every platform's tests; a failure opens a
`Nightly build failed` issue. `ci:full` also runs the release build on the pull request.

## Review comments
- Fix in the pull request what a review finds wrong: bugs, crash paths, silent data loss, a broken
  rule from this file.
- A quality point (style, naming, wording, a refactor, an extra test for code that works) may
  wait for the **release quality pass**, where maintainers handle them together before each
  release. Reply that it's left for the release quality pass, end the reply with
  `<!-- quality-sweep -->`, and leave it out of the next push.

## Looking at the app
- `cargo run --release -p rustgis` (opens the World Atlas sample).
- Headless screenshots: `cargo run --release -p rustgis-ui-egui --example ui_shot -- script.jsonl`
  (see the example header). For UI work, render the actual changed UI and look at the PNG.
  For any visible change, include screenshots in the PR (before and after for changed UI, after
  only for new UI); see [attachment guidance](docs/contributing.md#documentation).
- CLI: `rustgis-cli run --sample --cmd 'gp.run={"tool":"buffer","params":{"input":"Cities","distance":100}}' --cmd 'share.exportMap={"path":"out.png"}'`.
