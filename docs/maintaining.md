# Maintaining RustGIS

This page is for the [maintainers](https://github.com/orgs/opengeos/teams/rustgis-maintainers):
who approves a coordinate-first change, what happens when a proposal gets no reply, and what
reviewers check on saved files and dependencies. Contributors need only
[CONTRIBUTING.md](https://github.com/opengeos/RustGIS/blob/main/CONTRIBUTING.md).

## Who approves a coordinate-first change

CONTRIBUTING.md asks some changes to wait for a maintainer's reply. Who gives it depends on how
hard the change is to undo.

**Any maintainer**, for changes that only add. A maintainer can open the pull request without
proposing first; propose first when you're unsure which kind of change it is, or when the work is
big enough that a "no" would waste it. Say in the pull request which kind it is.

- an optional command parameter, whose default keeps today's behaviour;
- a field in a command's output;
- an optional field in the project file or another saved file, whose absence means today's
  behaviour, and whose loss when an older RustGIS saves the file costs a setting rather than data
  or a different result;
- a feature of an existing dependency, a dev-dependency or an xtask-only crate, unless it brings in
  a large set of new dependencies.

**Two maintainers**, for changes that break something that can still be put right, and for most
new dependencies. A maintainer who proposes the change counts as one of them.

- removing or renaming a command or parameter, or changing what one means, returns or defaults to;
- a new renderer, symbology or layer kind;
- a new project format version for a feature, following [saved files](#saved-files-and-settings);
- an addition to the plugin interface that plugins declare through `minRustgisVersion`;
- any other new dependency, answering the questions under [new dependencies](#new-dependencies).

**The project lead, [@giswqs](https://github.com/giswqs)**, has the final say on:

- the shape of the core types: `Coord`, `Feature`, geometries and field types;
- how a project stores its data;
- a dependency that is a C, C++ or system library, brings in a large tree such as `tokio`, or
  replaces or overlaps code RustGIS already has;
- a new crate in the workspace;
- a plugin ABI change, or removing something from the plugin interface;
- a new top-level ribbon tab;
- _Exploratory_ roadmap items, and anything that changes what RustGIS is for.

The lead also settles a disagreement between maintainers. When you approve, say which kind of
change it is, so the reply can be found later.

## When a proposal gets no reply

A contributor's proposal waits for the approval its kind needs. If a week passes without a reply,
mention @opengeos/rustgis-maintainers on the issue once.

If a maintainer proposes a change any maintainer could approve and nobody objects within a week of
the proposal or its last change, they can go ahead. An objection says what would go wrong, so that
it can be answered or the plan changed. Silence doesn't approve the other two kinds.

For a change the lead decides, ask once, on the issue: the question in a sentence, the options and
your recommendation. Link that comment from other threads rather than asking again.

## Saved files and settings

Projects, map, layer and style files, models, saved connections and sources, plugin state and
preferences outlive the release that wrote them, and people run an older and a newer RustGIS on the
same files. New and changed code follows these rules; not every existing reader does yet.

- A reader checks the version it is given.
- An older RustGIS refuses a file or setting newer than it understands, or keeps it unchanged. It
  doesn't save it again without what it couldn't read.
- A format's version goes up only when a file uses something an older reader would lose or
  misread.
- Where a change allows it, reading a new version ships at least one release before anything
  writes it.
- A project, map or layer file, a model or a web tool lists in `requires` each capability it needs
  that an older reader would lose or misread: a reader refuses one it doesn't know, naming it.
  Task, presentation and notebook readers, and models inside a task's steps, don't check
  `requires` yet. A release reads
  `.rgis` format 2 before any writes it (`READ_FORMAT` and `CAPABILITIES` in
  `crates/engine/src/project.rs`), so what is added to format 2 after that comes with a capability
  name, added to `CAPABILITIES` where it is read.
  Independent sizes on classified point layers use `classified-point-size`; only documents
  containing a `size_by` mapping list that requirement.
- `schema/project.schema.json` is the JSON Schema of the project file, generated from the types
  it is saved from by `cargo xtask schema`. A pull request that changes what a project saves
  regenerates it and commits it in the same pull request; CI's `cargo xtask schema --check` fails
  otherwise. A pull request that changes how it is read adds or updates a fixture project in
  `schema/fixtures/`, which must open and validate against the schema. A schema change alone
  doesn't raise the format: the rules above say when that happens.
- RustGIS releases, file formats, the plugin ABI and the GeoLibre runner protocol are versioned
  separately.
- A program RustGIS installs from elsewhere, such as the native GeoLibre runner, is pinned to a
  known release and moved forward by a pull request.

Keep historical projects and preferences in the
[release fixtures](../crates/engine/testdata/releases/README.md). These are the original bytes
written by the release named in each filename: never regenerate, reformat or resave them.
The [Release fixtures workflow](../.github/workflows/release-fixtures.yml) captures a small project
and preferences file from each published release once its Linux archive is available, then opens
a pull request. Actions must be allowed to create pull requests in the repository settings. If
that setting is disabled, open a pull request from the `release-fixtures/<tag>` branch or use the
workflow's artifact to submit the fixtures. Approve the CI runs on the generated pull request.
The `released_projects_and_preferences_still_open` test discovers
the files by version and checks that the current reader still opens them correctly. Record a skip
in the fixtures' README and in the test's `SKIPS` when a release has no such file format.

New project saves and map/layer exports write format 2. Their `requires` header names only
features present in the saved payload: `terrain-block`, `terrain-block-shape` (a block
diagram cut to a circle), `terrain-water`,
`terrain-water-shore` (a terrain water's shore band other than the default),
`terrain-block-water` (water in a block diagram's walls),
`shaded-relief-effects` (cast shadows, sky visibility, fill light or rock colour),
`shaded-relief-light` (a Shaded Relief's ambient or balanced light) and
`normalized-legend-urls` for normalized legend addresses. Project
requirements include offline map snapshots. `table-export-order` protects saved task commands,
command notebook cells and replayable history using Export Table sort or limit options.
Earlier format-2 readers refuse unknown required
capabilities instead of opening and later saving away those settings; legacy format-1 files
still open.

Projects, map files and layer files whose symbology draws an image as a marker (`Marker::Picture`:
a symbol's, a stacked stroke's or a dot fill's, saved styles included), or whose replayable
commands set one (an `image` in `layer.marker`, a `layer.lineStrokes` stroke's markers or a
`layer.fillLayers` fill), require `picture-markers`:
an older reader must refuse them rather than fail on the marker or lose it.

Projects, model files and web tools that save a percent-slope function require `slope-percent`:
an older reader must refuse them rather than run the saved function in degrees. Project
requirements also cover executable task steps, replayable history and notebook command cells.
The same saved containers require `tabulate-intersection-unit` for Tabulate Intersection
with a non-default `areaUnit`, so older readers refuse instead of silently using km².
Omitted units and explicit `km2` require nothing extra; omitted zone fields need no
capability because older tools refuse them.
The same containers require `tabulate-intersection-empty-zones` when
`includeEmptyZones` is on, so older readers refuse the file instead of omitting the zero rows.

Projects with a background on a layout element, or a saved task step, script, notebook command
cell or history entry that gives one (`layout.addElement` or `layout.update` with a
`background` other than `null`), require `layout-element-background`, so an older reader
refuses them instead of opening the layout and saving it again, or running the command,
without the background. Layout files (`.rgislayout`) carry no format header yet and are not
covered. `layout.import` already reads one, ahead of the release that writes it: it refuses a
newer format or an unknown `requires` entry by name (#2277).

Projects, map files and layer files with a window of a Zarr variable that follows the time slider
(`layer.addZarr` with `"time": true` and `"cells"` or `"bbox"`) require `zarr-window`, so an older
reader refuses them instead of reading the whole map at each step. A saved `layer.addZarr` command
with both needs nothing: an older RustGIS refuses it when it runs.

Projects with an ink on a legend, colorbar, table or chart (`color` on those elements) require
`layout-element-ink`, so an older reader refuses them instead of drawing those elements in dark
grey and saving them again without it. Saved `layout.addElement` and `layout.update` commands
that set one need nothing: an older reader refuses `color` on those elements, so the command
fails there instead of losing the ink. A saved `layout.import` whose `text` gives an element an
ink requires `layout-element-ink`, and one whose `text` gives an element a background requires
`layout-element-background`, since an older reader would read that layout without them. Layout
files are not covered, as for backgrounds (#2277).

Projects with a layout text that wraps to its box (`wrap` on a text element) require
`layout-element-wrap`, so an older reader refuses them instead of drawing the text on its own
line breaks and saving it again without the option. Saved `layout.addElement` and `layout.update`
commands with `"wrap": true` require it too, since an older reader warns that it ignores the key
and adds the text unwrapped; `"wrap": false` needs nothing. A saved `layout.import` whose `text`
has a wrapped text requires it as well. Layout files are not covered, as for backgrounds (#2277).

Projects with a layout chart grouped by a field, or with a statistic, an order or a number of bars
other than the defaults, or with an Other bar (`group`, `statistic`, `order`, `bars`, `other` on a
chart element) require `layout-chart-data`, so an older reader refuses them instead of drawing a
bar per feature, the 20 largest, and saving the chart again without them. A statistic counts
grouped or not: one kept while a chart is ungrouped would be lost too. Saved `layout.addElement`
and `layout.update` commands that set one of them require it too, and so does a `layout.update`
that gives a chart's `label` (`""` too), which an older reader's update doesn't take; giving the
defaults, or `"group": ""`, needs nothing. A saved `layout.import` whose `text` has such a chart
requires it as well. Layout files are not covered, as for backgrounds (#2277).

Projects with a layout chart drawn as columns, with its values or their decimals, its own title
or none, a bar colour or labels against its bars (`orientation`, `values`, `decimals`, `title`,
`bar_color`, `label_align` on a chart element) require `layout-chart-look`, so an older reader
refuses them instead of drawing bars running across in the default blue, with the title the
chart is given, and saving it again without them. Saved `layout.addElement` commands for a chart
that set one of them require it too. A saved `layout.update` requires it for `values`,
`decimals`, `barColor` and `labelAlign` only: its `title` and `orientation` may be a legend's or
a colorbar's, which the command alone doesn't tell, and the project the update ran on requires it
for its chart anyway. Giving the defaults needs nothing. A saved `layout.import` whose `text` has
such a chart requires it as well. Layout files are not covered, as for backgrounds (#2277).

A coordinate system read from WKT is saved with its PROJ definition, which older readers read,
and its WKT beside it (`wkt`), which this release reads first. Projects, map files and layer
files require `crs-wkt` when a saved system's definition doesn't place points as its WKT does
(within a centimetre, at its centre and a few degrees around it): angles in grads, a datum
transformation the definition can't hold, or no definition. An older reader refuses those
instead of misplacing the layer. Others need nothing: an older reader opens them by the
definition and, saving them again, drops only the WKT.

## New dependencies

A proposal for a dependency answers:

- Does it build for `wasm32-unknown-unknown`, or does it sit behind a native-only feature?
- Does `deny.toml` allow its licence?
- What does it add to build time and package size, measured, with only the features we need?
- What does it do better than the code we have, shown by tests (accuracy, malformed input, the
  browser)?

## The release quality pass

One maintainer takes the [release quality pass](contributing.md#releases) for each release and
says so in the version-bump pull request. Each point left for the pass is fixed, filed as an issue,
or dropped with a reply on its comment; none carries into a second release unrecorded.
