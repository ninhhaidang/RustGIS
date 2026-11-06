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
- RustGIS releases, file formats, the plugin ABI and the GeoLibre runner protocol are versioned
  separately.
- A program RustGIS installs from elsewhere, such as the native GeoLibre runner, is pinned to a
  known release and moved forward by a pull request.

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
