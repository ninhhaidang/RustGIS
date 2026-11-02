# Architecture

RustGIS is a layered Cargo workspace. Lower layers never depend on the UI, which keeps the
engine scriptable and the front end swappable.

| Layer | Crate | Job |
| --- | --- | --- |
| L0 | `rustgis-geom` | Coordinates, extents, Web Mercator, geodesic distance/area, scale |
| L1 | `rustgis-data` | Features, typed attributes, vector and spreadsheet I/O, SQL where clauses, LiDAR, rasters and EPSG reprojection; indexed file and remote data providers; live PostGIS, MySQL/MariaDB and SQL Server providers on the desktop; optional GDAL and DuckDB programs (see [Data formats](user-guide/data-formats.md)) |
| L2 | `rustgis-map`, `rustgis-analysis` | Layers, symbology, labeling, camera, basemaps and web tile layers; geoprocessing tools |
| L2 | `rustgis-tiles` | Web map tiles: XYZ/TMS/WMS raster tiles, Mapbox Vector Tiles, PMTiles and MBTiles archives and a subset of MapLibre styles; a cache that fetches in the background (worker threads natively, the browser's `fetch` on the web) |
| L3 | `rustgis-render` | tiny-skia renderer, label placement, font discovery, map and layout export, and UI-independent GPU scene preparation |
| L3 | `rustgis-geolibre` | The [GeoLibre tools](user-guide/analysis.md#geolibre-tools) as geoprocessing tools: run by the optional `geolibre` program natively, by geolibre-rust's WASI runner in the browser |
| L3 | `rustgis-plugins` | [Plugins](plugins.md): sandboxed WebAssembly modules, desktop process plugins and assistant skills packages; `.rgplugin` packages and the plugin registry |
| L4 | `rustgis-engine` | Session, undo, command registry, projects, sample project |
| L5 | `rustgis-assistant` | The [AI Assistant](user-guide/assistant.md): provider request/response formats, conversation loop, command tools, skills and agent integration; native HTTP transport and local agent/MCP connections, with browser I/O supplied by the web app |
| L6 | `rustgis-ui-egui` | Ribbon, panes, map view, dialogs and the optional wgpu map backend |
| apps | `rustgis`, `rustgis-cli`, `rustgis-web`, `rustgis-python` | Desktop app, command line, browser (WebAssembly), and the PyO3-backed `rustgis` Python package |
| tooling | `xtask` | Workspace checks, third-party licence notices and web service-worker generation |

## Everything is a command

Engine actions are `CommandSpec`s with an id, label, ribbon location, shortcut, parameter
description, an `enabled` check and a `run` function. The ribbon, keyboard shortcuts, command
search, CLI and Python dispatch engine commands through `Session::run`. UI-only commands
(`ui.*`) live in `crates/ui-egui/src/lib.rs`; engine commands never open dialogs.

The UI exports `big_split_menu` for a large ribbon control with independent, accessible primary
and alternatives buttons. Pass the primary command id and parameters separately from the menu
closure; menu choices run their own commands and never replace the primary action. The primary's
enabled state comes from its command, while `menu_enabled` controls the arrow independently. Use
`ui.push_id` when placing repeated controls whose order can change. Keep whole-button dropdowns
for choices without a clear everyday default.

For tab purposes, pane shortcuts and choosing controls, follow the [ribbon guidelines](contributing.md#ribbon-guidelines).

Mutating commands snapshot the project for undo and restore it on an error or escaped panic.
View-only commands do not take undo snapshots. Undo and rollback cover project state, not
external effects such as files already written.

Plugin commands (`plugin:<plugin>/<command>`) dispatch through `Session::run` too; their project
changes form one undo step. WebAssembly plugins reach the session through permission-checked
requests. Process plugins use the same host protocol but are not sandboxed (see [Plugins](plugins.md)).

## Troubleshooting diagnostics

`rustgis_data::diagnostics` provides the bounded, sanitized session history without a UI
dependency. `Session::notify` and `Session::warn` retain messages immediately, independently
of pane visibility and project undo/persistence. Command sources contain registered ids,
never command parameters. Errors rejected before command execution are captured too.
The frontend also routes UI-only command failures through the notice path, without duplicating
errors already reported by an engine command.
Unavailable undo/redo keyboard shortcuts do not dispatch a command, matching their disabled
toolbar buttons; direct command failures still follow the same capture path.

HTTP failures are observed at the shared tile/data/service transport boundaries, not by
polling failed cache entries. A weak `Capture` travels through tile scheduling, native worker
queues, data/render workers, and browser fetch callbacks. Synchronous scopes restore the
previous owner; browser futures receive explicit captures and never hold a thread-local scope
across an await. Unowned requests are not broadcast to other sessions. Transport metadata
uses observed statuses and known timeout/connection causes only.

The egui Diagnostics pane (`ui.pane.diagnosticLog`) filters and copies these sanitized records.
The existing Diagnostic Monitor (`ui.pane.diagnostics`) remains the performance view.

## Never crash

Production code forbids unsafe Rust. Library crates deny Clippy's unwrap, expect, panic,
unimplemented, todo and unreachable lints. Readers treat file contents and command parameters as hostile.
`Session::run` and `analysis::run` catch escaped panics as a last resort. Hostile-parameter tests
exercise engine commands and built-in geoprocessing tools; some file-writing and network
commands are skipped.

## Never silent

When a reader, writer, tool or command can't represent something, approximates it or drops it, it
says so in a notice: what was lost, how much and where. Notices go through `Session::notify`,
which the app shows, the CLI prints and Python returns from `take_notices`. Commands pass on the
notes readers return, and geoprocessing tools report theirs in `ToolResult::messages`. A result
that would be unusable is an error instead. Older code doesn't yet report every loss; each one
found is treated as a bug.

## Pure Rust

RustGIS's engine and apps are written in Rust and run without GDAL, PROJ or any other GIS
library installed. GDAL, DuckDB and the `geolibre` program are optional extras: on the desktop,
when they are installed, RustGIS calls them to read [more formats](user-guide/data-formats.md#formats-read-through-gdal),
run DuckDB queries and run the [GeoLibre tools](user-guide/analysis.md#geolibre-tools). In the web
app the GeoLibre tools run in the browser instead, through geolibre-rust's WASI runner. Some
dependencies do compile C code into the build: the Zstandard decoder (`zstd-sys`,
desktop and browser), SQLite (`libsqlite3-sys` through `rusqlite`, desktop), the TLS cryptography
libraries (`aws-lc-sys` and `ring`, desktop) and GeographicLib's geodesic routines for the
azimuthal equidistant projection (`proj4rs-geodesic`, desktop). Building them needs a C
compiler; building the web app needs clang for Zstandard.

## Secrets

`Services::secrets` gives the desktop app its OS credential store: the login Keychain on macOS,
Windows Credential Manager (persistence Local), and the Secret Service (GNOME Keyring, KWallet)
over pure-Rust D-Bus on Linux and the BSDs. It is `None` in the browser, and the session's store
is `None` on the command line and in Python unless `--use-saved-secrets` / `use_saved_secrets`
asks for it. Only `rustgis-keychain` uses an OS API; other crates use only the
`rustgis_data::secrets` trait. CLI and Python call `rustgis_engine::prefs::opt_in` once to read
the desktop's preferences, load its saved connections without writing changes back, and install
the lazy store. The generic `rustgis_data::secrets::Reporting` wrapper warns once on failure and
stops retrying an unavailable backend. The preferences module also owns desktop writes, refuses
to overwrite unreadable or malformed preferences, and returns read errors for callers to report
without preference values. Assistant API keys additionally require the desktop's Remember
preference and matching provider/address; an environment key wins. The CLI uses shared assistant
preference names and only reads a saved API key for a prompt, never for account management or
model listing.

- **Keys.** The service is `RustGIS`. The account is `owner/name` with a lowercase owner (for
  example `assistant/<provider>` or `connection/<id>`); each feature defines its own. Values are
  opaque strings in `Secret`.
- **Errors, not fallbacks.** A missing or locked store, or a refused prompt, returns a
  `SecretError` to show the user. Never write the secret anywhere else instead.
- **No leaks.** `Secret`'s `Debug` is redacted and it has no `Display`. Never log a secret, and
  never put one in command parameters, History, workspace recordings or assistant tool output.
- **Blocking.** A call can wait on an OS prompt or D-Bus. Make it on a user action or a worker
  thread, never every frame, and keep the value in memory for the session. Don't access one
  account from two threads at once.
  Python releases the GIL for commands, rendering, data reads and lock waits. A process-wide
  native-operation gate preserves serialization across Python sessions because driver
  activations are process-global; a per-session mutex protects each session's state. Diagnostics
  are logged only after both locks are released. Non-throwing `rustgis` logging cannot replace
  an operation's result or original error.
  The Assistant queues legacy API-key migrations once at startup and serializes its keychain
  reads, writes and deletions on a worker. An existing keychain key takes precedence over a
  legacy preference. The preference is removed only after a successful read of that key or a
  successful migration write; new API keys are never written to desktop preferences.
- **S3 request snapshots.** The registry lock protects selection of the exact live login and
  the requested bucket's explicit-grant status. Copy only that login and grant boolean, not
  the grant list; parse and validate addresses and prepare signatures outside the lock.
  Keychain calls and network I/O never run under the registry lock.
- **Earthdata Login token.** `rustgis_tiles::earthdata::set_secret_store` gets `Services::secrets`
  from `RustGisApp::with_services`, and `prefs::opt_in`'s store for the command line and Python. `earthdata.login` with
  `remember` keeps the token at `earthdata/token`, and without it (or when the store refuses
  it) deletes the remembered one; `earthdata.logout` always deletes it. The token is read back
  once per store given, on the first
  request to an Earthdata data host or when the NASA Earthaccess pane opens (on a worker), never
  over a token typed this session. An expired token is deleted, not sent, and the pane and the
  refusal message ask for a new one. The token and its store are process-global, like the
  session token before them.
- **ChatGPT sign-in.** The desktop app gives its store to `chatgpt::set_secret_store` at start.
  The keychain keeps only each registration's refresh token, `chatgpt/<client_id>/<subject>`,
  with the `rev` its entry in `chatgpt-auth.json` names (any other is out of date and never
  used), without the access and id tokens, to stay within Windows Credential Manager's 2560 bytes
  of UTF-16 (a real refresh token's length is not verified yet). Access and id
  tokens stay with the process that got them; a process without one renews first. The keychain
  is read only to renew, sign out or remove, under the file's `store-lock` (and, to renew, the
  `renewal-lock`), so a rotating refresh token is never used twice. The entry's `keychain` flag is
  set before the first keychain write and cleared after a delete, so removal never strands an
  item. Tokens the keychain couldn't keep stay in memory for the session, with an empty `rev`; a
  new sign-in never replaces one the keychain holds. Tokens from an earlier version's file leave it
  only after the keychain has taken them. `Config::from_env` never reads the sign-in, and a
  configuration's key is a marker (`chatgpt::SIGNED_IN`), never a token: `chatgpt::send`
  authorizes each request. The Assistant reads the sign-in on a worker (startup, switching to the
  provider, after a failed request) bound to the selection that started it, and requests wait for
  it.
- **macOS.** The system asks again for access when the app's code signature changes, which
  happens with unsigned or re-signed builds.
- **Tests.** Use `MemorySecrets`, and `MemorySecrets::failing(..)` for error paths. The shared
  backend's smoke test is `#[ignore]`d:
  `cargo test -p rustgis-keychain keychain_round_trip -- --ignored`.

## Quality gates

```sh
cargo xtask ci        # fmt, clippy -D warnings, tests, layering check
```

`cargo xtask layers` checks that workspace dependencies point to lower layers and that crates
below L6 do not directly depend on UI crates such as egui, eframe, rfd, wgpu or winit.
The browser build is a separate CI gate:

```sh
cargo build --locked --target wasm32-unknown-unknown -p rustgis-web
```

Keep platform-specific I/O behind native target gates or the UI's `Services` callbacks. The web
app supplies asynchronous file pickers, downloads, HTTP requests and the GeoLibre WASI runner.
See [Contributing](contributing.md) for the remaining CI gates and developer tasks.
