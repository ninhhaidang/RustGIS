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
- A change to what a project saves runs `cargo xtask schema` and commits
  `schema/project.schema.json` with it; a change to how a project is read adds or updates a fixture
  in `schema/fixtures/`.

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
- A command that must run on the UI thread says why in its doc comment.
- A new background raster command or export, or a tool whose work grows with its input, goes in
  the lists in `long_commands` (`crates/engine/src/tests/cancellation.rs`), which both cancel tests
  run; the raster and export lists must match the background registries. A read-ahead command
  goes in `reads::IN_BACKGROUND` (a table edit in `edit::TABLE_EDITS`), and `long_commands`
  fails until it runs every one there or names why not in its exemptions.
  `long_commands_stop_when_cancelled_while_they_run` fails one that never looks at Cancel;
  `cancelling_at_each_check_leaves_nothing_behind` cancels at every check in turn until the command
  finishes before reaching it, on small fixtures, failing if one needs 256 or more checks. It
  checks project data, Undo/Redo, files and an immediate retry. Network-bound cases keep only
  first-check coverage.
  Passing shows the work looks, not that it looks often: a long step without a check (one union,
  one read) is said in its doc comment.

## Use the threads
Work that splits into independent parts runs on several threads where practical:
- the same output as one thread, bit for bit; one thread below a size threshold and in the browser;
- in `analysis`, the shared helpers (`parallel::for_each_row`, `try_map`, `try_map_on`);
  everywhere, the thread count from `rustgis_data::threads` (it follows the user's setting), never
  `available_parallelism` or a fixed number, and scratch memory within `memory::thread_scratch`;
- a thread the OS refuses costs speed, not the run (`Builder::spawn_scoped`), and a job's thread
  it refuses ends that job with an error (`Builder::spawn`; `work_off_frame` in `ui-egui`).
  `clippy.toml` refuses `available_parallelism`, and `std::thread::spawn` and `Scope::spawn`, which
  panic then; an exception says why in its `#[expect(clippy::disallowed_methods, reason = "…")]`.

## Size for real data
Real data dwarfs the sample project: a parcel layer of half a million rows and a hundred fields
loads at about 2.5 GB, national layers run to tens of millions of rows and more, and a 1 m
elevation model of 1,000 km² is a billion cells. When a change stores, copies or compares layer
data, give in the pull request what it holds as a multiple of the layer, as `cargo xtask memory`
measures.
- Memory follows what an operation changes or produces: an Undo step, a version, a sync base or a
  cache holds only the changed rows or shares storage with the original. New work adds no second
  full copy of a layer or raster; #2431 measured what one costs.
- Work that grows with its input goes in pieces (chunks, tiles, slices) that it doesn't keep, or
  sizes its limit with `memory::planned` and refuses past it with a way out, such as filtering or
  clipping first. Running out of memory partway counts as a crash.

## Architecture
| Layer | Crate | Job |
|---|---|---|
| L0 | `geom` | coordinates, extents, projections, geodesic measurement |
| L1 | `data` | features and attributes, file formats, database providers (PostGIS and MySQL through pure-Rust clients, desktop only; GeoPackage in the browser through a pure-Rust `sqlite` reader), queries, rasters; GDAL and DuckDB through their programs |
| L2 | `map`, `analysis`, `tiles`, `keychain` | layers and symbology; geoprocessing tools; web map tiles; the system's secret store |
| L3 | `render`, `plugins`, `geolibre` | map rendering and export; WebAssembly plugins; the GeoLibre tool suite (its program natively, its WASI runner in the browser) |
| L4 | `engine` | `Session`, undo, the command registry, the sample project |
| L5 | `assistant` | the AI Assistant (sans I/O); file, network and plugin commands wait for approval |
| L6 | `ui-egui` | ribbon, panes, map view, attribute table, dialogs |
| L7 | `ui-touch` | the phone interface around `ui-egui`'s map view |
| apps | `rustgis`, `rustgis-cli`, `rustgis-web`, `rustgis-python`, `rustgis-mobile` | desktop, command line, browser, Python package, Android and iOS |

- **Everything is a command**: a `CommandSpec` in `crates/engine/src/cmd/*.rs`. UI-only
  commands (`ui.*`) live in `crates/ui-egui/src/lib.rs`. The ribbon, shortcuts, command search
  and CLI dispatch by id. Commands never open dialogs.
- **`.view()` leaves the layers as they are**: a command that adds, removes or reorders layers
  mutates, even when it fails part way. Declared `.view()`, it leaves no Undo step when it runs by
  itself. Only Undo, Redo (`Effect::History`) and a project replaced (`Effect::Replaces`) may.
  The engine's unit and integration tests fail a registered command declared wrong (the check in
  `invoke`, `policy/dispatch.rs`, feature `declaration-check`), but only on the paths they run.
- **Command ingress**: root `Session::run` is trusted native User dispatch. Assistant, MCP,
  plugin, imported-item and continuation adapters carry their named opaque policy scope;
  scopes inherit or narrow, and `with_scope` rejects nested ingress. Peer project text uses
  `open_peer_text`. See [embedding-page limitations](SECURITY.md#embedding-page-command-boundary)
  when changing web message handlers.
- **Ribbon placement** follows the
  [ribbon guidelines](docs/contributing.md#ribbon-guidelines): the tab's purpose, one button per
  destination, labels that say what happens, and what a ribbon pull request states.
- **Never break wasm**: everything below `apps/` must build for `wasm32-unknown-unknown`
  (CI runs `cargo build --target wasm32-unknown-unknown -p rustgis-web` with warnings as errors;
  the Zstandard decoder in `rustgis-data` is C, so this needs clang). Use `web_time`, not
  `std::time`; file access goes through `Services` on the web. In the browser, GeoLibre tools run
  asynchronously (`external_start` → `Services::geolibre_run` → `gp.finish`), never as a blocking
  call. Code only the desktop calls is
  dead code in the browser build and fails it: put it behind `#[cfg(not(target_arch = "wasm32"))]`
  and check as CI does, with
  `RUSTFLAGS='-D warnings' cargo build --locked --target wasm32-unknown-unknown -p rustgis-web`.
- **Interface text is translatable**: wrap user-visible literals in `tl!("…")`, use
  `i18n::fmt("… {name} …", &[("name", …)])` instead of `format!` for messages, and pass labels to
  the ribbon widgets (they translate). Pull requests don't need to add the new strings to the
  catalogs in `crates/ui-egui/src/i18n/*.tsv`: a missing entry shows in English, and the
  maintainer translates them before each release (format in `docs/i18n.md`). Command ids, data
  and the CLI are never translated.
- **Layering** is enforced by `cargo xtask layers`: nothing below L6 depends on egui/eframe/rfd, a crate
  depends only on lower layers (`[target.…]` dependencies count, dev- and build-dependencies don't), and a
  new crate under `crates/` gets its layer in `LAYERS` (`xtask/src/main.rs`).
- **The expression language has one vocabulary**: a new function, variable or keyword goes in
  the catalogs in `crates/data/src/query.rs` (`Func::spec`, `VARS`, `Kw`), which the parser
  reads, and in `docs/user-guide/expressions.md`; the `query` tests fail when the guide misses
  a signature, unit, variable or keyword, or names a function that doesn't exist.
- **A parameter that takes an expression says so**: a command's key gives its type as `sql` or
  `expression` in its params (`"where": sql`, `"size"?: expression|null`), and a tool's
  parameter is `ParamKind::Expression`, so it can be told from plain text. Another language (a
  database's, a service's) is described in quoted prose instead.

## Reach every surface
A change that works where you tried it is often missing elsewhere. Before calling it done, say in
the pull request which of these it reaches and why the rest don't apply:
- the desktop app, the browser (file access through `Services`), the command line, Python, the
  phone (the touch interface, `ui-touch`);
- the ribbon, shortcuts and command search; the Assistant and MCP (do their tool descriptions
  show the new option?); plugins;
- the way back: what you add can be undone, removed or reopened.

## Assets
- No third-party vendor icons, images, styles, fonts, help text or code. Icons are drawn in
  code (`icons.rs`). Every asset file has a row in `ATTRIBUTION.md`; `cargo xtask attribution` (in CI) fails an image or
  font file that no row names, by path, directory or pattern.
- Never copy or translate code from GPL, LGPL or other copyleft projects (such as GRASS, QGIS or
  GEOS) into RustGIS, by hand or with an AI tool. Implementing a published algorithm is fine: cite
  the paper or standard. Name in the pull request any project whose code you followed (see
  [CONTRIBUTING.md](CONTRIBUTING.md#where-code-comes-from)).
- Screenshots and other images are not committed here: they live in
  [opengeos/rustgis-assets](https://github.com/opengeos/rustgis-assets) (WebP, served at
  `https://assets.rustgis.app/images/…`) and docs link to them by URL.

## Quality gates
`cargo xtask ci` (fmt, clippy -D warnings, tests, layers) before every commit.
`cargo xtask test-hygiene` (also in CI) requires `// test-wait: reason` on timer waits in tests and
`#[ignore = "reason"]` on ignored tests, and refuses an `assert_eq!` or `assert_ne!` (or `debug_assert_*`)
whose two sides are the same tokens; prefer completion or progress signals, and a fixed expected value.
`clippy.toml` refuses the four sphere measures in `rustgis-geom` (`haversine_m`, `geodesic_length_m`,
`geodesic_ring_perimeter_m`, `geodesic_ring_area_m2`): the Earth measures on the WGS 84 ellipsoid
(`wgs84_distance_m`, `wgs84_length_m`, `wgs84_step_m`, `wgs84_ring_perimeter_area_m2`), and a call kept on
the sphere, for another body or a comparison it can't change, says why in its
`#[expect(clippy::disallowed_methods, reason = "…")]`.
The engine test `command_surface_matches_the_snapshot` fails on any difference between the registered
commands and `crates/engine/command-surface.txt` (each command id with its parameter keys and optional
marks, not the help text): a pull request that adds, removes or renames a command or a parameter runs
`cargo xtask commands` and commits the file. A removal or rename is a coordinate-first change.
`cargo xtask memory` (in CI on Linux) fails when a command line workflow's peak memory grows past
its limit in `xtask/memory-limits.txt`; a change that lowers one commits the lower limit with it.
`cargo xtask rows` (also in CI) fails when code outside `rustgis-data` uses a layer's rows directly
(`Feature` fields, the row vector) more often than `xtask/row-uses.txt` allows: use the accessors on
`Features` (`row`, `rows`, `value`, `set_value`, …), and `--update` when you lower the count (#2431).
It counts only the libraries and programs built for the host, not tests, examples, benches or code
built only for other targets, nor a row indexed without reading a field (`features[i]` alone); use
the accessors there too.
Both limit files only go down: CI's `Limits only go down` job fails a pull request that raises or
removes a limit against its base (a new memory scenario is fine). A maintainer approves a deliberate
raise by commenting on the pull request with the line the failure prints for it, on a line of its
own (`<!-- ratchet-raise-approved: xtask/row-uses.txt count=790 -->`), then re-runs the job.
The test `command_ids_in_hand_kept_lists_are_registered` (`crates/ui-egui/tests/command_ids.rs`) fails when
non-test source holds a command id that no command has (a dotted literal in a registered namespace, such
as `layer.add`, or a `ui.*` id with no match arm in `run_ui`, in `crates/ui-egui/src/lib.rs`): a renamed or removed
command leaves the lists that name it, such as `ON_A_COPY`, the policy tables and Command Search, pointing
at nothing. A literal that isn't a command goes in its `EXEMPT` list with a reason (file and host names
ending in a `NOT_IDS` word need none), and an `EXEMPT` entry the source no longer
holds fails it too.
While you work, `cargo xtask changed` runs the same checks on only the packages your branch changes
and those that depend on them (and `test-hygiene` on the whole workspace, and formatting, `layers`,
`attribution` and `rows` when packages are checked; `attribution` also when only non-code files change), or everything when a
changed file such as `Cargo.lock` is outside every package and nothing includes it.
A new dependency must have a licence `deny.toml` allows (CI runs `cargo deny check licenses`); after
changing dependencies, run `cargo xtask notices` to refresh the `THIRD-PARTY-LICENSES.txt` files the
packages ship (the release build fails on stale ones).
Pull requests run CI on Linux (plus the Android and iOS builds when they touch
`apps/rustgis-mobile`); pushes to `main` test every platform. When a pull request touches
platform-specific code, paths or file dialogs, label it `ci:windows` or `ci:macos` to test there,
or `ci:full` for every platform and the release build. Label-triggered runs start the tests
immediately; label-gated build jobs in `ci.yml` start from the next push.

## Review comments
- Fix in the pull request what a review finds wrong: bugs, crash paths, silent data loss, a broken
  rule from this file.
- A quality point (style, naming, wording, a refactor, an extra test for code that works) may
  wait for the **release quality pass**, where maintainers handle them together before each
  release. Reply that it's left for the release quality pass, end the reply with
  `<!-- quality-sweep -->`, and leave it out of the next push.
- The Claude review ends each inline finding with its category (a last line such as `Category: bug`).
  CI's `Review deferrals` job fails, on each push and in the merge queue, when the marker answers
  a `bug`, `security` or `agents-md` finding, or one edited since it was posted. A maintainer
  overrides it for one finding by ending a reply in its thread with `<!-- deferral-approved -->`.

## Looking at the app
- `cargo run --release -p rustgis` (opens the World Atlas sample).
- Headless screenshots: `cargo run --release -p rustgis-ui-egui --example ui_shot -- script.jsonl`
  (see the example header). For UI work, render the actual changed UI and look at the PNG.
  For any visible change, include screenshots in the PR (before and after for changed UI, after
  only for new UI); see [attachment guidance](docs/contributing.md#documentation).
- CLI: `rustgis-cli run --sample --cmd 'gp.run={"tool":"buffer","params":{"input":"Cities","distance":100}}' --cmd 'share.exportMap={"path":"out.png"}'`.

## This file
A rule here names a trap, not a map of the code: agents can read the code. A new rule earns its
place when someone who knows the code would still get it wrong, it has happened more than once, and
it says exactly what to do. A pull request that adds a check or a test list updates its line here;
any other new rule goes under "Suggested AGENTS.md additions" in the pull request description, and a
maintainer decides.
