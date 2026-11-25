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
- A key a project file has that this version doesn't read (a hand edit, another program) is left
  out on open and said: one notice per part of the project, with a count and the first few
  places. Keys inside a setting read leniently or flattened into another (a layer's labels) aren't
  seen, so they go unsaid (#2639).
- Where a change allows it, reading a new version ships at least one release before anything
  writes it.
- A project, map or layer file, a model or a web tool lists in `requires` each capability it needs
  that an older reader would lose or misread: a reader refuses one it doesn't know, naming it.
  `task.import` reads a task file this way (#2818), though `task.save` doesn't list anything in one
  yet. Presentation and notebook readers, and models inside a task's steps, don't check
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

New project saves, map/layer exports and layout files write format 2. Their `requires` header names only
features present in the saved payload: `terrain-block`, `terrain-block-shape` (a block
diagram cut to a circle or to polygons), `terrain-block-polygon` (one cut to polygons),
`terrain-water`,
`terrain-water-shore` (a terrain water's shore band other than the default),
`terrain-block-water` (water in a block diagram's walls),
`terrain-block-walls-layer` (the layer a block diagram's walls are coloured by),
`shaded-relief-effects` (cast shadows, sky visibility, fill light or rock colour),
`shaded-relief-light` (a Shaded Relief's ambient or balanced light),
`blank-paper` (a blank basemap's paper colour) and
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
an older reader must refuse them rather than fail on the marker or lose it. Where such an image
is an SVG, they also require `svg-markers`: a reader of PNG and JPEG markers only would draw it
as a circle and refuse the command that sets it.

Projects, model files and web tools that save a percent-slope function require `slope-percent`:
an older reader must refuse them rather than run the saved function in degrees. Project
requirements also cover executable task steps, replayable history and notebook command cells.
The same saved containers require `tabulate-intersection-unit` for Tabulate Intersection
with a non-default `areaUnit`, so older readers refuse instead of silently using km².
Omitted units and explicit `km2` require nothing extra; omitted zone fields need no
capability because older tools refuse them.
The same containers require `tabulate-intersection-empty-zones` when
`includeEmptyZones` is on, so older readers refuse the file instead of omitting the zero rows.
The same containers require `dggs-system` when Generate Tessellation or Aggregate Points to Grid
(`a5Grid`, `a5Aggregate`) is given a `system` other than A5: those were A5's tools, and an older
reader would make A5 cells, warning only that it ignored the key. A blank `system`, `A5` or none
needs nothing.

Projects with a background on a layout element, or a saved task step, script, notebook command
cell or history entry that gives one (`layout.addElement` or `layout.update` with a
`background` other than `null`), require `layout-element-background`, so an older reader
refuses them instead of opening the layout and saving it again, or running the command,
without the background. Layout files (`.rgislayout`) carry a header of their own, as map files
do, and require what their elements use, as the project does;
`layout.import` refuses a newer format or an unknown `requires` entry by name, and reads a file
without a header as a legacy layout (#2277).

Projects, map files and layer files with a window of a Zarr variable that follows the time slider
(`layer.addZarr` with `"time": true` and `"cells"` or `"bbox"`) require `zarr-window`, so an older
reader refuses them instead of reading the whole map at each step. A saved `layer.addZarr` command
with both needs nothing: an older RustGIS refuses it when it runs.

Projects, map files and layer files with a label class curved along a guide line layer
(`labels.placement` with a `guide`) require `label-guide`, so an older reader refuses them instead
of placing those labels as without it and saving them again without the guide. A saved
`labels.placement` command with a guide needs nothing: an older RustGIS refuses the key when it runs.

Projects, map files and layer files with a label class weighing other than 5 (`labels.placement`
with a `weight`) require `label-weight`, so an older reader refuses them instead of placing their
labels as without it and saving them again without the weights. So do projects with such a class in
a saved style, or a saved `labels.placement` step (in a task, a model or a script) with a weight
other than 5: an older reader would apply the style, or run the step, without the weight, saying
nothing.

Likewise a label class keeping other than 2 points free round its labels (`labels.placement` with a
`clearance`) requires `label-clearance`, in a layer, a saved style or a saved `labels.placement`
step: an older reader would place the labels 2 points apart and save them again without the
clearances, or run the step without it.

A label class removing duplicates (`labels.placement` with a `removeDuplicates` distance) requires
`label-duplicates`, in a layer, a saved style or a saved `labels.placement` step: an older reader
would place and save every label, or run the step without it.

Projects and map files with a tile layer drawn nearest (`map.setTiles` with `"resampling":
"nearest"`) require `tile-resampling`, so an older reader refuses them instead of drawing the layer
blended and saving it again without the setting. So do projects with a saved `map.addTiles` or
`map.setTiles` step (in a task, a model or a script) that sets it: an older reader would run the
step without it, saying nothing.

Projects with an ink on a legend, colorbar, table or chart (`color` on those elements) require
`layout-element-ink`, so an older reader refuses them instead of drawing those elements in dark
grey and saving them again without it. Saved `layout.addElement` and `layout.update` commands
that set one need nothing: an older reader refuses `color` on those elements, so the command
fails there instead of losing the ink. A saved `layout.import` whose `text` gives an element an
ink requires `layout-element-ink`, and one whose `text` gives an element a background requires
`layout-element-background`, since an older reader would read that layout without them.

Projects with a layout text that wraps to its box (`wrap` on a text element) require
`layout-element-wrap`, so an older reader refuses them instead of drawing the text on its own
line breaks and saving it again without the option. Saved `layout.addElement` and `layout.update`
commands with `"wrap": true` require it too, since an older reader warns that it ignores the key
and adds the text unwrapped; `"wrap": false` needs nothing. A saved `layout.import` whose `text`
has a wrapped text requires it as well.

Projects with a layout chart grouped by a field, or with a statistic, an order or a number of bars
other than the defaults, or with an Other bar (`group`, `statistic`, `order`, `bars`, `other` on a
chart element) require `layout-chart-data`, so an older reader refuses them instead of drawing a
bar per feature, the 20 largest, and saving the chart again without them. A statistic counts
grouped or not: one kept while a chart is ungrouped would be lost too. Saved `layout.addElement`
and `layout.update` commands that set one of them require it too, and so does a `layout.update`
that gives a chart's `label` (`""` too), which an older reader's update doesn't take; giving the
defaults, or `"group": ""`, needs nothing. A saved `layout.import` whose `text` has such a chart
requires it as well.

Projects with a layout chart drawn as columns, with its values or their decimals, its own title
or none, a bar colour or labels against its bars (`orientation`, `values`, `decimals`, `title`,
`bar_color`, `label_align` on a chart element) require `layout-chart-look`, so an older reader
refuses them instead of drawing bars running across in the default blue, with the title the
chart is given, and saving it again without them. Saved `layout.addElement` commands for a chart
that set one of them require it too. A saved `layout.update` requires it for `values`,
`decimals`, `barColor` and `labelAlign` only: its `title` and `orientation` may be a legend's or
a colorbar's, which the command alone doesn't tell, and the project the update ran on requires it
for its chart anyway (`decimals` may also be a scale bar's, so such an update requires
`layout-scale-bar-details` as well). Giving the defaults needs nothing. A saved `layout.import`
whose `text` has such a chart requires it as well.

Projects with a layout chart coloured by its layer's classes (`classes` on a chart element)
require `layout-chart-classes`, so an older reader refuses them instead of drawing every bar in
the chart's bar colour and saving it again without. Saved `layout.addElement` commands for a
chart and `layout.update` commands that set `"classes": true` require it too (only a chart takes
the key); `false` needs nothing. A saved `layout.import` whose `text` has such a chart requires it
as well.

Projects with a layout chart stacked by a second field (`stack` on a chart element) require
`layout-chart-stack`, so an older reader refuses them instead of drawing each bar whole and
saving it again without the field. It is required whenever the field is stored, even where the
chart doesn't draw it stacked (ungrouped, or not summing or counting, which commands refuse or
let go). Saved `layout.addElement` commands for a chart and `layout.update` commands that set a
`stack` field require it too (only a chart takes the key); `""` needs nothing. A saved
`layout.import` whose `text` has such a chart requires it as well.

Projects with a layout chart that faces another (`mirror`, a `max` or `hide_labels` on a chart
element; a `max` not above 0, only in a hand-edited file, is let go on read and said as a value out
of range) require `layout-chart-facing`, so an older reader refuses them instead of drawing
the chart the usual way round, on its bars' own scale and labelled, and saving it again without
(#2487 part 4). Saved `layout.addElement` commands for a chart and `layout.update` commands that
set `"mirror": true`, a `max` above 0 or `"hideLabels": true` require it too (only a chart takes
the keys); `false`, `null` and `"auto"` need nothing. A saved `layout.import` whose `text` has
such a chart requires it as well.

Projects with a layout chart that draws a line round its bars (`bar_outline` with a width above 0
on a chart element) require `layout-chart-outline`, so an older reader refuses them instead of
drawing the bars without and saving the chart again without (#2487 part 4b). In a hand-edited
file, a width past 5 is held to 5 and one below 0 let go on read, both said as values out of
range; a width of 0 draws none and is let go. Saved
`layout.addElement` commands for a chart and `layout.update` commands that give a `barOutline`
object without a width or with one above 0 require it too (only a chart takes the key); `null` and
a width of 0 need nothing. A saved `layout.import` whose `text` has such a chart requires it as
well.

Projects with a map frame whose border isn't the default dark grey 0.8 pt line (`border_color`,
`border_width` on a map frame, a width of 0 drawing none) require `layout-frame-border`, so an
older reader refuses them instead of drawing the default border and saving the frame again
without its own (#2557). A saved `layout.update` that gives a frame a `border` object requires it
too, since an older reader warns that it ignores the key; `"border": null` only gives the default
back and needs nothing. A saved `layout.import` whose `text` has such a frame requires it as well.

Projects with a legend, a scale bar or a grid whose text isn't its default size (`text_size` on
a legend, 9 points by default, or on a scale bar, 8; `label_size` on a frame's grid, 8) require
`layout-text-size`, so an older reader refuses them instead of drawing that text at the default
and saving it again without its size (#2581). A saved `layout.addElement` that gives a legend or
a scale bar `textSize`, or a grid `labelSize`, of another size requires it too, and so does a
`layout.update` with `"grid": {"labelSize": …}` of another size; `null` or the default need
nothing. A saved `layout.update` isn't told which element it changes, so any `textSize` but
`null` requires it there. A saved `layout.import` whose `text` has such an element requires it
as well.

Projects with a scale bar that has subdivisions, divisions before zero, labels below, a unit
label of its own or placed elsewhere, decimals or a fit other than the defaults
(`subdivisions`, `divisions_before_zero`, `label_position`, `unit_label`, `unit_label_position`,
`decimals`, `fit` on a scale bar element) require `layout-scale-bar-details`, so
an older reader refuses them instead of drawing a plain bar and saving it again without them.
Saved `layout.addElement` and `layout.update` commands that set one require it too, since an
older reader ignores the keys; giving the defaults, `"decimals": "auto"` or `"unitLabel": null`,
needs nothing. Decimals fixed at any number, 0 included, are a detail. A chart takes `decimals`
too: a saved `layout.update` can't tell whose they are, so a number there requires both this and
`layout-chart-look`, while a `layout.addElement` says which kind it adds. A saved
`layout.import` whose `text` has such a bar requires it as well, and a layout file
(`.rgislayout`) with such a bar lists it in its header, so an older reader refuses the file too.
A scale bar's text size isn't one of its details: it requires `layout-text-size`, as above.

Projects, map files and layer files with a layer or table held in a coordinate system of its own
(`Dataset::held_in`: a file's system, kept since #2580) save its features in that system, exactly,
with `held_in` beside them, and require `native-coordinates`: an older reader would read those
coordinates as longitude/latitude, and one that saved the project again would lose its system and
its exact coordinates. That holds for one with no features too (a template's layers), whose system
an older reader would also save away. A layer or table in longitude/latitude, or a window onto a
streamed source, is saved as before and requires nothing (#2032 C2d).

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
