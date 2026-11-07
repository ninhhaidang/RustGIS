# Contributing

Contributions are welcome. See [AGENTS.md](https://github.com/opengeos/RustGIS/blob/main/AGENTS.md)
for the conventions.

1. Fork the repository and create a branch.
2. Make your change with tests. Engine behaviour is a command in `crates/engine/src/cmd/`;
   UI-only commands (`ui.*`) live in `crates/ui-egui/src/lib.rs`.
3. Run `cargo xtask ci` and `pre-commit run --all-files`.
4. Open a pull request. Rust CI runs on Linux by default. A maintainer can label it `ci:windows`,
   `ci:macos` or `ci:full` to test those platforms before merging (`ci:windows` tests Windows x64
   and ARM64); pushes to `main` test them all. `ci:full` also builds the release apps. The nightly
   workflow tests and release-builds them all when there are new commits, and opens an issue on
   failure.
5. The docs workflow builds a site and web-app preview. A first-time fork contributor's preview
   needs a maintainer to add `preview` or re-run the workflow; returning contributors' previews
   publish automatically.

A maintainer's `automerge` label approves and squash-merges a pull request after its required
checks pass. Pushing again removes the label and disables auto-merge; removing the label also
disables it.

A maintainer's `claude-autofix` label has Claude bring a pull request up to date: it merges the base
branch and resolves any conflicts, then fixes the open review comments it agrees with, replies to
each and resolves the ones it fixed or found already fixed, and pushes the commits to the branch (a
fork's too, when edits by maintainers are allowed). The label comes off when it's done; add it again
for another round.

Not every review comment has to be settled before a pull request merges. A quality point (style,
naming, wording, a refactor, an extra test for code that works) can be left for the **release
quality pass**: before each release, maintainers gather these from the merged pull requests and fix
them together. Reply on the comment that it's left for the release quality pass (agents end the
reply with `<!-- quality-sweep -->`), so it can be found. Bugs, crashes, silent data loss and
breaks of the rules in AGENTS.md are fixed in the pull request.

Changes to saved or generated output must classify each new owning field: address, executable
parameters, generated text or user data. Use the shared durable-output transaction and keep
credential removal separate from execution and approval identity. Review every writer that
consumes the owner, including packages and peer exchange. Reuse a focused regression that checks
the written output, restoration of live fields on failure, counted notices and clean-input
silence. Opaque data needs an explicit preservation policy; recursive removal by key name can
destroy user data.

## Developer checks

`cargo xtask ci` runs `cargo fmt --all --check`,
`cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace -q` and
`cargo xtask layers`. The pre-commit hooks also check TOML, YAML, JSON, whitespace and large files.
CI separately builds `rustgis-web` for `wasm32-unknown-unknown` and runs
`cargo deny --locked check licenses sources` against `deny.toml`, including development
dependencies. The fuzz workflow applies the same source policy using `fuzz/deny.toml`, which
also permits NCSA for libFuzzer. Sources are limited to crates.io; an intentional git source needs an explicit
allowlist entry and a pinned revision. CI also runs `python3 .github/scripts/check-cargo-sources.py`
to reject repository Cargo configuration with `source`, `paths` or `patch` overrides, including
case variants of `.cargo/config` and `.cargo/config.toml` used on case-insensitive filesystems.
Tracked `.cargo` directories must be real directories.
Config `include` directives are also rejected so those checks cannot be bypassed by loading
another file; put intentional dependency patches in `Cargo.toml` for review.
User-level Cargo mirrors are outside this repository check.

Workflow actions are pinned to full commit SHAs with the previous version or branch in a comment.
Dependabot's existing GitHub Actions updates keep these pins current. When changing an action,
resolve the commit in that action's own repository and keep the version comment in step.

Other workspace tasks:

| Task | Purpose |
| --- | --- |
| `cargo xtask layers` | Check crate dependency direction and keep UI dependencies out of lower layers |
| `cargo xtask notices` | Regenerate the shipped `THIRD-PARTY-LICENSES.txt` files after changing dependencies, or after adding code or data from another project to `INCLUDED` in `xtask/src/notices.rs` (with its licence text in `third-party/`); requires `cargo-about` |
| `cargo xtask notices --check` | Check the notices without writing them; stale notices warn in CI but block releases |
| `cargo xtask web-pwa <dir>` | Generate the service worker in a Trunk build output directory, such as `dist/web` |

Interface strings must be translatable: use `tl!("…")` for literals and
`i18n::fmt("… {name} …", &[("name", value)])` for messages with values. New strings can fall back
to English until the catalogs are updated; see [Languages](i18n.md).

### Saved-file compatibility

`crates/engine/src/project.rs::FORMAT` is the saved-document compatibility version, separate
from the application release version. Project, map, layer and style imports check a top-level
`format` before interpreting their payloads. Missing metadata remains supported legacy content;
style files currently remain arrays. Current companion writers keep their existing output.
Every project serialization, including templates and packages, writes the current `FORMAT`,
even when the project was loaded from an older supported file.
These reader checks prepare for future writers; they do not protect already released readers
or detect new information in files that omit the required version.

When a future change would cause an older reader to misinterpret or lose meaningful saved
information on resaving, advance `FORMAT` and declare it in each affected document's top-level
`format`. This includes additive fields that parse successfully but disappear when an older
writer saves. For a style file that needs metadata, the future writer must use an object rather
than the legacy array; readers with this guard refuse its newer format before interpreting it.
A payload or wrapper for such a new format belongs in that future change. Keep existing legacy
readers and test that unsupported versions fail before typed deserialization, normalization or
source reopening, including alternate import and package paths.

### Faster local builds and test runs

All of this is optional and on your side: the repository's profiles stay as they are, and
`cargo xtask ci` is still the check before each commit and pull request.

**Less debug information.** Much of a debug build's size and link time is debug information,
most of it the dependencies'. In your user-level `~/.cargo/config.toml`:

```toml
[profile.dev]
debug = "line-tables-only"   # backtraces keep file:line

[profile.dev.package."*"]
debug = false
```

On Linux, `split-debuginfo = "unpacked"` under `[profile.dev]` also keeps the linker from copying
what is left of it, and [mold](https://github.com/rui314/mold) links faster still than the
`rust-lld` Rust already uses on x86-64 Linux. Install mold first (the link fails without it), then:

```toml
[target.x86_64-unknown-linux-gnu]
rustflags = ["-C", "link-arg=-fuse-ld=mold"]
```

The settings change every build's fingerprint, so the next build is a full one.

**Build only what you're testing.** `cargo test --workspace` builds every crate's library, test
binaries and examples; a change in `ui-egui` alone means its library twice (plain and for its unit
tests), fifteen integration test binaries and the examples. While you work, name what you need:
`cargo test -p rustgis-ui-egui --lib`, `cargo test -p rustgis-engine --test project_save`, or
`cargo clippy -p rustgis-ui-egui --all-targets` to check without linking.

**cargo-nextest.** [cargo-nextest](https://nexte.st) (`cargo install cargo-nextest --locked`, or
`brew install cargo-nextest` for a pre-built one) runs the test binaries side by side, each test in
its own process, and filters tests by expression:

```sh
cargo nextest run --workspace --no-fail-fast -E 'not (package(rustgis-engine) and test(hostile_params_never_panic))'
cargo test --workspace --doc   # nextest doesn't run doctests
```

The engine's `hostile_params_never_panic`, which runs every command with junk parameters, is by far
its slowest test; leaving it out while you iterate keeps a run short. Run it before you push, with
`cargo nextest run -p rustgis-engine -E 'test(hostile_params_never_panic)'` or `cargo xtask ci`.
A test that fails now and then can be retried with `--retries 1`.

**`opt-level` 0 for UI work.** The workspace crates build at `opt-level = 1`. With
`CARGO_PROFILE_DEV_OPT_LEVEL=0`, `ui-egui` rebuilds after an edit about five times faster, but the
engine's tests run about six times slower (rasters, geometry and the hostile-parameter test), so
it suits iterating on the interface only. Switching the level rebuilds the workspace crates.

**rust-analyzer in its own folder.** An editor's rust-analyzer and the terminal share `target/`:
each waits for the other's lock ("Blocking waiting for file lock"), and as they build with
different settings, each undoes the other's work. With `rust-analyzer.cargo.targetDir` set to
`true` it builds in `target/rust-analyzer` instead, at the cost of the disk space.

**On macOS.** No linker setting is needed: mold doesn't run there, Apple's own linker (Xcode 15
and later) is already fast, and `split-debuginfo = "unpacked"` is already the default for debug
builds.

- Leave `target/` out of backups and search, which otherwise go through every file a build
  changes: `tmutil addexclusion target` once it exists, and add the folder to Spotlight's privacy
  list in System Settings.
- macOS checks a program the first time it runs, and each build makes new test binaries. If even
  the simplest tests take more than 0.2 seconds under nextest, run
  `sudo spctl developer-mode enable-terminal`, turn your terminal on under System Settings ›
  Privacy & Security › Developer Tools, and restart it. From then on, nothing that terminal starts
  gets the first-run check, programs you download included, so do it only for a terminal you
  trust; turning its switch off there undoes it. Working inside tmux or zellij, add the
  multiplexer too, since its shells aren't the terminal's children, and add it again after each
  update of a Homebrew build (signed ad hoc, it gets a new identity each time). Measure before
  and after: on some machines it makes no difference (an M1 Max with macOS 15.7, for one).

**`cargo clean` now and then.** `target/` keeps the artifacts of every branch and setting you have
built, and can grow past a hundred gigabytes.

## Ribbon guidelines

These guidelines decide where a ribbon control goes and what kind of control it is. They guide
new features and small, tab-by-tab improvements; they are not a claim that the current ribbon
already follows every rule. Decisions about particular controls live in their issues.

### Start with the task and the tab's purpose

Use the tab's purpose to decide where a feature belongs, rather than putting it wherever there
is space. Agree on that purpose before reorganising a tab.

| Tab | Purpose |
| --- | --- |
| Home | The basics of working with a map: starting a map or layout, adding data, moving around, and selecting and inspecting features. |
| Data | Working with the content and structure of data: looking at records, changing fields and values, connecting tables, and bringing data in and out. Exports here produce data, such as features and rasters. |
| Style | Deciding how a layer looks on the map: how its features are drawn and shown at different scales. |
| Labeling | How the map is labelled: what gets a label, with which text, where, and at which scales, plus the label settings for the whole map. |
| Edit | Creating and changing features and graphics on the map, and managing those edits. |
| Analyze | Turning existing data into new results, from single tools to repeatable workflows, scripts and automation. |
| View | How the map and workspace are displayed: map appearance and view configuration, and which panes and windows are open. Basic navigation stays on Home. |
| Publish | Getting work out to others: printing, exporting or sharing products such as maps, layouts, reports and packages, including on the web. |
| Plugins | Finding, installing and using plugins that extend RustGIS. |
| Settings | Preferences that apply to every project, such as language, appearance and start-up. |
| Help | Learning RustGIS, finding commands and shortcuts, and getting information about the app. |

Keep Data, Style and Labeling fixed, rather than making the tab strip change with the selected
layer. Their target should be explicit: a layer chooser at the left of each layer-dependent tab
(for example, “Styling: Countries ▾”) is planned, with a useful hint when no suitable layer is
selected. Where a class chooser is needed, the layer comes first, then the class.

If a control seems to fit two tabs, or none, clarify its task and destination before placing
it. Home is not a catch-all for anything described as “frequently used”. Data exports data;
Publish exports products. A control kept on a tab its purpose doesn't cover, until another tab
is reorganised, is an interim exception: record it in the issue that tracks the move.

### Keep useful shortcuts, not duplicate destinations

- **One button per destination, not per pane.** Buttons that open the same pane at different
  sections are useful task shortcuts. Merge controls that land in the same place with the same
  context.
- **Keep View › Windows complete.** Every pane must be reachable there. Working tabs may repeat
  the panes their tasks need; do not replace those shortcuts with a generic Panes dropdown just
  to reduce the button count.
- **Keep modes with their tools.** Measurement units belong with Measure because they are chosen
  while measuring. Application-wide preferences such as Dark Mode belong in Settings.
- **One name and icon per thing.** Use the same label and icon for the same command wherever it
  appears: ribbon, panes and Command Search. Do not give one tool different names, or expose
  one setting as two differently named toggles.
- **Labels say what happens.** Name a control for what it does, not for a similar feature
  elsewhere. A coordinates-only action should say “By Coordinates”; the “Locate” menu covers
  both coordinates and place/address search. End a label with “…” when it opens a dialog or
  file picker that asks for input.
- **Keep a route to every feature.** Removing a ribbon control must not remove access to its
  action. Name and check the remaining route, including for UI-only actions.

### Choose controls for the workflow

- Put everyday actions and quick adjustments on the ribbon; keep deeper, less frequent options
  in a pane. A pane holds the full set of settings; its ribbon controls are shortcuts to the
  same settings and selected layer or class, not separate state.
- Use a split button only when there is a clear everyday default. The main action stays fixed;
  choosing an alternative never changes it to “last used”. The arrow opens related alternatives.
  Without an obvious default, use a plain dropdown. For example, Select's main action is click
  or box selection; Polygon, Lasso and Radius are in its arrow menu.

For split-button implementation and enabled-state handling, see
[Architecture](architecture.md#everything-is-a-command).

### Check workflows before shipping

Improve the ribbon in small pull requests, tab by tab, rather than replacing the whole layout
at once. Clear naming and duplicate-destination fixes can land independently of a larger
reorganisation.

Check everyday workflows such as adding data, selecting by location, buffering, symbolising
and exporting a map. When moving, removing or regrouping controls, record clicks and tab
switches for these before and after; fewer controls is not an improvement if those tasks become
harder. Check the tab at 1366 px wide with the [`ui_shot` example](#documentation) and look at
the image. Keep everyday controls reachable without horizontal scrolling; move deeper options to
their pane rather than packing more controls into the strip.

A pull request that adds a ribbon control should state:

- the task it serves, its tab and group, and how it fits the tab's purpose;
- whether it runs immediately, selects a tool, opens a dialog or opens a pane, including the
  pane section and layer/class context;
- the fixed default and alternatives if it uses a split button;
- a screenshot of the tab at 1366 px wide.

A pull request that moves, removes or regroups controls should also state:

- where each removed control's action remains reachable, and any Command Search location or
  label updates;
- why any repeated destination is useful in its workflow;
- the workflows checked, with their click and tab-switch counts before and after.

Changing ribbon placement does not require changing command ids or parameters: keep CLI,
shortcuts and scripts working.

## Test data

Generate small datasets during tests instead of committing fixture files. The shared generator
in `tests/support/ogc.rs` builds OGC landing pages, collection lists and GeoJSON in memory;
`tests/support/ogc_server.rs` serves them locally for engine and UI tests. The sorting smoke
test similarly generates its KML, KMZ, CSV and GeoJSON inputs. These run automatically with
`cargo test --workspace --locked` and `cargo xtask ci`, without a separate setup step or a
public service. Extend these generators when adding cases.

`crates/engine/tests/workflows.rs` runs everyday jobs from start to finish: join, overlay,
summarise, map, save, reopen and export, and the same saved job twice. Its real-data case,
Shelby County's census tracts checked against the areas the Census Bureau publishes, is
`#[ignore]`d: the nightly build downloads the public-domain files, pinned by checksum, and runs it
with `RUSTGIS_WORKFLOW_DATA` naming their folder (the list of files is in `nightly.yml`). To run
it locally, download the files listed there into one folder, check them with `sha256sum` against
the checksums beside them, and run
`RUSTGIS_WORKFLOW_DATA=<folder> cargo test -p rustgis-engine --test workflows -- --ignored`.

## Documentation

The site is built with [Zensical](https://zensical.org):

```sh
pip install -r requirements_docs.txt
zensical serve        # http://localhost:8000
```

Images live in [rustgis-assets](https://github.com/opengeos/rustgis-assets) and are linked by
URL; don't commit images to this repository.

For PR screenshots, GitHub web attachments are fine. The optional
[`gh image` extension](https://github.com/drogers0/gh-image) can automate uploads and return
image links; `gh` itself has no native standalone attachment-upload command.

The screenshots are rendered by the app itself. From the repository root, run
`crates/ui-egui/examples/shots/render.sh`; it needs ImageMagick (`magick`) and network access for
tiles, plugins and the sample DEM. It runs the `ui_shot` scripts beside it and writes WebP files
to `target/shots/`; copy them to `images/` in rustgis-assets. The `assistant` shot asks a real
model, so set `RUSTGIS_ASSISTANT_PROVIDER` and the provider's key first, or it shows an empty
pane (see [AI Assistant](user-guide/assistant.md#choosing-a-model)).

## Releases

1. Handle the review points left for the release quality pass since the last pass, in a few pull
   requests.
2. Bump `version` in the workspace `Cargo.toml` and merge it.
3. Refresh third-party notices if dependencies changed (`cargo xtask notices`).
4. Run the **Release dry run** workflow, which checks the version and builds the desktop/CLI
   packages without publishing anything:
   `gh workflow run release-dry-run.yml -R opengeos/RustGIS --ref main -f version=<version>`
5. When it passes, publish a GitHub release tagged `v<version>` (the tag must match `Cargo.toml`).

The **Release** workflow then builds and attaches the Windows installers, zip and MSIX, the
macOS DMGs, the Linux tarballs, `.deb` and `.rpm` packages and AppImages, and `SHA256SUMS`. To try the packaging locally:

```sh
cargo build --release --locked -p rustgis -p rustgis-cli
packaging/package.sh <version>  # writes dist/ for the host platform
```
