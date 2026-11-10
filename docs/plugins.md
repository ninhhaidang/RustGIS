# Plugins

Plugins add commands, geoprocessing tools and assistant skills to RustGIS. A plugin has a
`plugin.json` manifest and is packaged as a `.rgplugin` file. WebAssembly plugins run sandboxed
in the desktop app, on the command line and in the web app. [Process plugins](#process-plugins)
run a local program instead and need the desktop app or command line.

Browse the [plugin catalog](https://plugins.rustgis.app/catalog/).
[opengeos/rustgis-plugins](https://github.com/opengeos/rustgis-plugins) holds the registry,
blocklist and [`rustgis-plugin` SDK](https://github.com/opengeos/rustgis-plugins/tree/main/sdk),
not plugin source. Plugin sources live in their own repositories; start one from the
[Rust WebAssembly](https://github.com/opengeos/rustgis-plugin-template),
[Rust process](https://github.com/opengeos/rustgis-rust-process-plugin-template) or
[Python process](https://github.com/opengeos/rustgis-python-plugin-template) template.

## Using plugins

Open **Plugins › Plugin Manager**. The **Plugins** tab also has **Install From File** and
**Browse Plugins** buttons.

- **Browse** lists the plugins in the registry, with search and a category filter. **Install**
  downloads the plugin, checks its SHA-256 and id against the registry entry, and shows what
  the plugin may do before it loads.
  Descriptions stay on one line; hover over one to read its full text and, for a withdrawn
  plugin, the withdrawal reason in a single tooltip.
- **Installed** lists your plugins: turn them on or off, update them, or uninstall them.
- **Settings** has **Install From File…** for a `.rgplugin` file; you can also drop the file on
  the window. It also sets the registry address (empty means the RustGIS registry). On the
  desktop, **Add Plugin Folder…** loads an unpacked plugin while you develop it.

A plugin's commands and tools appear on the **Plugins** tab, in a group named after the
plugin, or on the plugin's own tab if it has one. Its commands are also in command search, and
its tools are in the **Tools** pane under **Plugins ›**.

The desktop app keeps installed packages in `plugins/` in its data folder (overridden by
`$RUSTGIS_PLUGINS_DIR`) and loads them at startup. The web app remembers the
plugins you installed from the registry and downloads them again on your next visit. Packages
installed from files are remembered when they are at most 1.5 MiB; larger ones last for the
current session only. When one of them can't be loaded on a visit (you're offline, the download
is gone or has changed), the web app says so once and lists it on the **Installed** tab as *Not
loaded*, with why. **Retry** downloads a plugin from the registry again; **Remove** forgets it, so
the web app stops trying at start. A plugin that fails to load is never forgotten on its own.

On the command line:

```sh
rustgis-cli plugins                          # installed plugins
rustgis-cli plugins browse                   # the registry
rustgis-cli plugins get vector-toolkit       # install from the registry
rustgis-cli plugins install my.rgplugin      # install a file (or a plugin folder)
rustgis-cli plugins disable vector-toolkit   # enable / disable / uninstall <id>

rustgis-cli run --sample \
  --cmd 'gp.run={"tool":"plugin:vector-toolkit/hexagon-grid","params":{"input":"Countries","size":300}}' \
  --cmd 'share.exportMap={"path":"hex.png"}'
rustgis-cli run --plugin ./dev-plugin/ --sample --cmd 'plugin:my-plugin/hello'   # load for one run
```

Use `--no-plugins` to skip loading installed plugins. `plugins browse` and `plugins get` accept
`--registry https://…/registry.json` for another registry.

## Safety

Plugins run in a sandbox: an interpreter (`wasmi`) with no access to files, the network, the
clock or anything else outside the module. A plugin reaches RustGIS only through the requests
described below, and RustGIS checks each one:

- Every plugin can **read the map**: its layers, their features and the view.
- With the **`edit`** permission, it can change the project. It can run commands that edit it and
  add layers. A plugin command that changes the project is one undo step, and it is rolled back
  if the command fails.
- With the **`files`** permission, it can run commands that read or write files the user names,
  and its tools can take a file the user picks (a [`file` parameter](#file-parameters)).
  SQL, database and Zarr commands require both `files` and `network`, since they can reach
  local files or remote servers.
- With the **`network`** permission, it can reach the web: read a small document from a web
  address ([`host.fetch`](#fetching-a-document)) and run the commands that fetch (adding data
  or services by their address). With `edit` too, it can add web tile layers (`map.addTiles`).
  The addresses are the plugin's choice and can carry what it read from the project, so this is
  a permission of its own. A command that would bring in a tile layer some other way (importing
  a map file) has it taken out again without `network`.
- No plugin may run the user's scripts, tasks or models, create or save models, install or
  locate other software, manage plugins or connections and logins, replace the project, or
  step through undo history. It cannot call another plugin's command or tool. It may run its
  own tools through `gp.run`, with the usual `edit` and file requirements.

A [process plugin](#process-plugins) is the exception: it is a program on your computer, not
sandboxed, and the install prompt says so.

Commands, tool schemas and deferred results stay bound to the exact enabled plugin load that
started them. Disabling, removing or reloading it prevents further stages and result application.
Development process programs remain unsandboxed; this load binding does not attest their
executable bytes or prevent a program from changing on disk.

The install prompt lists these permissions. Each call has a fuel budget, so a plugin stuck in a
loop is stopped. Memory is capped at 512 MB, and requests and replies are size-limited. A plugin
that traps, panics or returns garbage produces an error message, never a crash.

The registry is curated: plugins are reviewed before they are listed, and packages are
hash-pinned. A `blocklist.json` beside a registry withdraws a plugin (every version, or one
package). RustGIS checks it when you browse that registry. Desktop and command-line startup
apply the cached blocklist; the web app fetches it when restoring plugins.

## Writing a plugin

Start from the [Rust WebAssembly template](https://github.com/opengeos/rustgis-plugin-template),
or the [Rust](https://github.com/opengeos/rustgis-rust-process-plugin-template) or
[Python](https://github.com/opengeos/rustgis-python-plugin-template) template for a
[process plugin](#process-plugins), and read the
[official plugins](https://official-plugins.rustgis.app) for examples, such as
[Hello RustGIS](https://github.com/opengeos/rustgis-official-plugins/tree/main/plugins/hello).
A WebAssembly plugin uses the `rustgis-plugin` SDK crate:

```rust
use rustgis_plugin::{Kind, Request, ToolOutput, Value, geojson, host, json};

fn handle(req: Request) -> Result<Value, String> {
    match (req.kind, req.id.as_str()) {
        (Kind::Command, "layer-report") => {
            let n = host::layers()?.as_array().map_or(0, Vec::len);
            host::notify(&format!("{n} layers"));
            Ok(json!({"layers": n}))
        }
        (Kind::Tool, "bounding-boxes") => {
            let boxes: Vec<Value> = req.features("input")?.iter().filter_map(|f| {
                let b = geojson::bbox(&geojson::positions(&f["geometry"]))?;
                Some(geojson::feature(geojson::polygon(&[[b[0], b[1]], [b[2], b[1]], [b[2], b[3]], [b[0], b[3]]]), json!({})))
            }).collect();
            Ok(ToolOutput::new().layer("Boxes", geojson::collection(boxes)).into())
        }
        (_, other) => Err(format!("unknown '{other}'")),
    }
}

rustgis_plugin::export!(handle);
```

```sh
rustup target add wasm32-unknown-unknown
cargo build --release --target wasm32-unknown-unknown
```

### `plugin.json`

```json
{
  "id": "hello-rustgis",
  "name": "Hello RustGIS",
  "version": "0.1.0",
  "description": "What it does, in a sentence or two.",
  "author": "You",
  "homepage": "https://github.com/you/your-plugin",
  "license": "MIT",
  "entry": "plugin.wasm",
  "abi": 1,
  "minRustgisVersion": "0.1.0",
  "permissions": ["edit"],
  "commands": [
    { "id": "layer-report", "label": "Layer\nReport", "help": "Shown on hover.", "icon": "stats", "params": "{}" }
  ],
  "tools": [
    {
      "id": "bounding-boxes", "name": "Bounding Boxes", "summary": "One line.", "toolbox": "Hello",
      "params": [
        { "name": "input", "label": "Input features", "kind": "vectorLayer", "required": true },
        { "name": "whole", "label": "One box for the whole layer", "kind": "bool", "default": false }
      ]
    }
  ],
  "i18n": { "de": { "Layer\nReport": "Ebenen-\nbericht" } }
}
```

- `id`: 1 to 64 lower-case letters, digits and hyphens, starting with a letter. Command and tool
  ids follow the same rules. Their full ids are `plugin:<id>/<command-or-tool>`; run commands
  directly and tools through `gp.run`.
- `runtime`: `wasm` (the default), `process` or `skills`. For WebAssembly plugins, `entry`
  defaults to `plugin.wasm` and must name a `.wasm` file relative to the manifest, using
  forward slashes with no `.` or `..` path segments. `abi` defaults to `1` and must equal `1`.
- `version` and optional `minRustgisVersion` are numeric versions such as `1.2.3` or `1.2`.
  RustGIS compares their numeric components, ignoring prerelease and build suffixes, and
  refuses a plugin that needs a newer RustGIS.
- `permissions`: `edit`, `files` and `network` (see [Safety](#safety)); empty by default.
  Ask only for what you use.
- `commands[].params`: a string describing the command's parameters for scripts and agents,
  not a JSON object or a form definition; it defaults to `"{}"`.
- `commands[].icon`: the name of one of RustGIS's own icons (`stats`, `zoom_full`, `bookmark`,
  `buffer`…), or a puzzle piece when it isn't one.
- Tool parameter `kind`s: `layer`, `vectorLayer`, `polygonLayer`, `pointLayer`, `field`,
  `numericField`, `number`, `text`, `choice` (with 1 to 64 `choices`), `bool` and `file` (see
  [File parameters](#file-parameters)). Each has `name`, `label`, optional `help`, `required`
  (default `false`) and an optional JSON `default`. Parameter names start with a lower-case
  letter and may also contain upper-case letters and underscores after it. In the tool's form, a
  required layer parameter starts at the active layer, or, when that isn't of the kind it takes (a
  `vectorLayer`, `polygonLayer` or `pointLayer`), at the first layer that is (empty when none
  is); an optional one stays empty until it's picked (its picker offers `—` to leave it out
  again), or shows the layer its `default` names, which a run without one uses; its picker then
  offers no `—`, which would leave the parameter out and run with that layer too.
- `panel` (optional) gives the plugin a pane of its own, with widgets bound to its commands. See
  [A panel](#a-panel).
- `i18n` translates the plugin's own text into the interface language, by language code (`de`,
  `zh-CN`…). See [Translations](#translations).
- `skills` (optional) lists skills for the AI Assistant that the package carries. See
  [Skills](#skills).

A manifest can declare at most 256 commands, 64 tools and 32 parameters per tool.

A package is a zip archive, conventionally named `<id>-<version>.rgplugin`. It holds
`plugin.json` at its root or inside one top-level folder; `entry` is relative to that manifest.
Packages are at most 64 MiB with at most 4,096 entries, modules at most 48 MiB, and the manifest
at most 512 KiB.

### File parameters

A tool can read a file the user picks, such as a NetCDF model run, with a `file` parameter.
The plugin needs the `files` permission:

```json
{ "name": "model", "label": "Model output", "kind": "file", "required": true, "extensions": ["nc", "nc4"] }
```

- The Tools pane shows a path box with a **Browse** button. `extensions` filters the picker:
  at most 16 extensions, each 1 to 16 letters or digits without the dot. Leave it out for any file.
- On the desktop and the command line the value is a path, and RustGIS reads the file. In the
  web app, which has no paths, the user picks the file and its contents go with the run. A
  script can do the same with `{"name": "run.nc", "data": base64}`.
- The tool gets the file in the request's `files`, by parameter name, as
  `{"name": "run.nc", "data": base64}`. The parameter itself holds just the file name.
- Files are at most 64 MB. The Geoprocessing history keeps the file's name, not its contents.
- RustGIS versions before file parameters refuse a plugin that has one, so set
  `minRustgisVersion` to the first version with them.

### Translations

A plugin speaks the interface language through its `i18n` table: for each language code, its
English strings and their translations, written exactly as they appear in `plugin.json`
(`\n` included):

```json
"i18n": {
  "de": { "Bounding Boxes": "Begrenzungsrahmen", "Input features": "Eingabe-Features" },
  "zh-CN": { "Bounding Boxes": "边界框", "Input features": "输入要素" }
}
```

- It covers the plugin's name and description, command labels and help, tool names, summaries
  and toolboxes, parameter labels and `choice` values, the tab, group and menu labels, and the
  panel's title, help and widget labels. A string with no translation shows in English.
- Alongside English (`en`), RustGIS's interface languages are `de`, `es`, `fr`, `it`, `nl`,
  `pt`, `ru`, `tr`, `az`, `id`, `vi`, `fa`, `zh`, `ja`, `ko` and `ka`. A code with a region
  matches its language: `zh-CN` serves `zh`.
- The registry copies the name and description translations into `registry.json`, so the
  Plugin Manager lists plugins in the interface language before they are installed.
- Messages a plugin builds while it runs can follow the interface language too: every request
  carries its code as `lang`. In the SDK that is `req.lang`, and `req.tr` looks a message up in
  `i18n`.
- Up to 64 languages and 20,000 translated strings in all.

### A tab of its own

By default a plugin's commands and tools get a group on the **Plugins** tab. A plugin can have a
ribbon tab of its own instead. The tab sits at the end of the tab strip, and **Settings › Customize › Ribbon**
can hide it like any other. Declare it in `plugin.json`:

```json
"tab": {
  "label": "Explorer",
  "groups": [
    {
      "label": "Go To",
      "items": [
        {
          "menu": "Continent",
          "icon": "zoom_world",
          "help": "Go to a continent.",
          "layout": "small",
          "items": [
            "go-africa",
            { "menu": "Europe", "items": ["go-europe", "go-france"] }
          ]
        },
        "random-place"
      ]
    },
    {
      "label": "Inspect",
      "layout": "small",
      "items": [
        "layer-report",
        "tool:field-summary",
        { "menu": "More", "items": ["feature-report", "tool:statistics"] },
        { "menu": "Overview", "layout": "large", "items": ["map-report"] }
      ]
    }
  ]
}
```

- `items` lists the buttons. An item is a command id, `tool:<id>` (which opens that tool in the
  Tools pane), `panel` (which opens the plugin's [panel](#a-panel)), or a dropdown `menu` whose
  `items` may also contain nested menus.
- A group's `layout` is `"large"` by default: full-height ribbon buttons, as in existing
  manifests. Use `"layout": "small"` for compact controls, stacked three per column.
  A menu's optional `layout` overrides its ribbon trigger's size; when omitted (or `null`),
  it inherits the group's layout. Consecutive small controls share three-row columns,
  including small menu triggers in a large group. A large trigger starts a new column.
  Column widths follow the translated labels; manifests neither need nor support pixel widths.
  Layout affects ribbon triggers only: all entries and nested submenu triggers inside a
  popup remain standard menu rows, regardless of any nested `layout`.
  Popup labels stay on one line: line breaks in labels or their translations become spaces.
- A tab has 1 to 8 groups. A large group has 1 to 16 direct items; a small group has 1 to 48.
  Each menu has 1 to 32 direct items. Menus may nest up to 4 levels, counting the outer menu
  as level 1. Across the entire tab there may be at most 16,384 items, counting every
  command/tool/panel entry and every menu at every level. Every entry must name one of the
  plugin's own commands or tools, or its panel.
- The label is one line of 1 to 32 characters and can't be the name of one of RustGIS's tabs
  (Home, Data, Style, Analyze, Plugins…), including their older names. The `i18n` translations
  apply to the tab, group and menu labels too.

[Map Explorer](https://github.com/opengeos/rustgis-official-plugins/tree/main/plugins/map-explorer) is
an example.

### A panel

A plugin can have a panel: a pane on the right of the map with texts, checkboxes, sliders,
buttons, trees, dropdowns, date pickers, range sliders and picks on the map bound to its
commands. Declare it in `plugin.json`:

```json
"panel": {
  "title": "Historical Imagery",
  "icon": "history",
  "help": "Step through the archived releases of World Imagery.",
  "state": "panel-state",
  "items": [
    { "kind": "text", "id": "selected", "label": "Selected release" },
    { "kind": "checkbox", "id": "local", "label": "Only versions with local changes", "command": "panel-state" },
    { "kind": "slider", "id": "release", "label": "Release timeline", "steps": true, "command": "go-to-release" },
    { "kind": "button", "command": "older" },
    { "kind": "button", "command": "newer" },
    { "kind": "text", "id": "metadata", "label": "Location metadata" }
  ]
}
```

- `title` is the pane's tab and the label of the button that opens it: one line of 1 to 48
  characters. RustGIS puts the button first in the plugin's group on the **Plugins** tab.
  On a tab of its own, put `panel` among a group's items. Command search lists the panel too.
  `icon` and `help` are for that button.
- `items` lists the widgets, top to bottom (1 to 32). Buttons that follow each other share a
  row. Each has an `id` (lower-case letters, digits and hyphens; a button's is its command's
  unless given), and `label` and `help` (on hover) where they apply:
    - `text`: a caption (`label`) over text the plugin sets (`text` is what it shows first).
    - `checkbox`: a box with a `label`, `default` `false` unless given.
    - `slider`: whole numbers from `min` to `max`, both within ±1,000,000,000 (both default
      to `0`; `default` is `min` unless given), with a caption, the value's label, and its two
      ends' labels. `steps: true` adds ‹ and › buttons that move it by one.
    - `button`: runs its `command`; its `label` is the command's unless given.
    - `choice`: a dropdown. Its `options` (`[{"id": "dswx-hls", "label": "DSWx-HLS"}]`, at most
      1,000, each `id` unique) come from `plugin.json` or the plugin; `default` is one of their
      ids. Its value is the chosen option's id, or `null`. An option whose id and label are
      those of one declared in `plugin.json` shows its label translated with its `i18n` (whether
      the declaration or the plugin's answer gave it); any other label is shown as the plugin
      words it. Only the first 1,000 options given are looked at.
    - `date`: a box for a date written `YYYY-MM-DD`, with a calendar button beside it. `default`
      is a date. Its value is the date, or `null`. A date typed runs the command when it is
      confirmed (Enter, or leaving the box); one that isn't a real date is put back.
    - `range`: a slider with two handles, low and high, over whole numbers from `min` to `max`,
      as a slider's (with its `labels` and `steps`, which move both handles). Each handle also
      takes the focus and moves with the arrow keys. `default` is `[low, high]` (both ends unless
      given). Its value is `[low, high]`, low first.
    - `tree`: nodes the plugin gives (themes, their datasets, say), each branch opened or closed
      with its triangle or a double click. `search: true` adds a box above it that keeps the
      nodes whose label holds every word typed (case and accents aside), with the branches
      above them open; a branch found with nothing found below it opens to all its children.
    - `pick`: a button that asks for a place on the map ([below](#picking-a-place-on-the-map)):
      a point, or with `"shape": "box"` a box. Its `label` is its command's unless given.
- A checkbox runs its `command` when it changes, a slider or a range when it is let go, or
  stepped (never while it is dragged, while the pane shows the label of the value it would take),
  a tree or a dropdown when a node or an option is chosen, a date when one is picked, and a pick
  when the place is picked. Without a `command`, a widget's value just goes with the plugin's
  next request.
- `state` (optional) is a command RustGIS runs each time the panel is opened, to answer its
  state.
- Every `command` reference, including `state`, must name one of the plugin's own commands.
  Widget ids are independent of command ids. A plugin with a panel can't have a command
  called `panel`.

A plugin keeps nothing between calls, so RustGIS keeps the panel's state. Every request of a
plugin with a panel carries the widgets' values, whatever ran the command (the panel, a ribbon
button, a script or the command line):

```json
"panel": { "values": { "local": false, "release": 196 }, "changed": ["release"] }
```

`values` has each checkbox (`true` or `false`), slider (a whole number), range (`[low, high]`),
tree and dropdown (the chosen node's or option's id, or `null`), date (`"YYYY-MM-DD"`, or
`null`) and pick (the place picked last, or `null`), and `changed` the ones this call set: the
widget the user just moved. A
widget's command is run with `{"panel": {"release": 196}}` as its parameters, which sets the
values before the call and is taken out of the request's `params`, so a script or `rustgis-cli`
can do the same:

```sh
rustgis-cli run --sample --cmd 'plugin:historical-imagery/go-to-release={"panel": {"release": 40}}'
```

Any command can answer the panel's new state, beside the rest of its answer:

```json
{
  "panel": {
    "values": { "release": 196 },
    "items": {
      "selected": { "text": "2025-12-18\nRelease 13192" },
      "release": { "min": 0, "max": 196, "labels": ["2014-02-20", "…", "2025-12-18"] },
      "newer": { "enabled": false },
      "metadata": { "text": "Source: Maxar (Vivid)\nResolution: 0.3 m" }
    }
  }
}
```

- An item can set `label` (`null` brings back the declared one), `enabled` and `visible`; a text
  its `text`; a slider or a range its `min`, `max` and `labels`, one per value from `min` (the
  numbers when there are none); a tree its `nodes`; a dropdown its `options`. What an answer leaves out stays as it was.
  Fields belonging to another widget kind are ignored and reported: `text` outside a text widget,
  `min`/`max`/`labels` outside a slider or range, `nodes` outside a tree, and `options` outside a
  dropdown. Unknown item ids and field names are also ignored and reported. Unknown panel-answer keys are reported too; echoed `changed` request metadata is accepted.
- A tree's `nodes` are `[{"id": "env", "label": "Environment", "count": 67, "children": [...]}]`:
  `id` is the value the node gives when chosen, unique in the tree; `label` defaults to the id;
  `count` (a whole number) shows at the right of the row. The nodes come before the values in an
  answer, so one can give a tree and choose a node of it; a chosen node no longer among them
  leaves nothing chosen.
- RustGIS checks the answer like any other: unknown ids and fields and values of the wrong type
  are ignored with a warning, a slider's or a range's value stays within its ends, a tree's among its nodes, a
  dropdown's among its options (a repeated or missing id is left out) and a date is a real date, texts are
  clipped to 4,000 characters, labels to 200, and a slider has at most 2,000 labels of 80
  characters. A tree holds at most 5,000 nodes, 8 levels deep; a node without an id, or with one
  longer than 200 characters or taken already, is left out with its children.
- Malformed panel, `items` or `values` objects, invalid numeric bounds and non-string node or
  option labels produce counted warnings. So do values clamped or reordered to fit a widget,
  choices cleared after options change, text cut to 4,000 characters, labels cut to 200,
  slider labels cut to 80 characters or 2,000 entries, and wrong-type applicable item fields.
  A malformed bound leaves that bound unchanged while a valid companion bound is applied.
  Warnings name the
  plugin and affected widgets or fields; clean input stays quiet. Diagnostic field and rejected-value names retain only ten samples of at most 200 characters, while counts include every field and value. Null entries in `items` mean no update; null values for selectable widgets still clear their selection. Control characters, line separators and bidi formatting controls in diagnostic names become spaces. A manifest's dropdown labels
  must already be strings of at most 200 characters without control characters. Option objects allow only `id` and an optional `label`; declarations requiring fields to be discarded or labels to be changed are refused with the affected fields in the error.
- A plugin words what it sets in the request's `lang`; RustGIS translates only what the
  manifest declares.
- A command that fails leaves the values as they were. The panel's state command is no undo
  step when it changes nothing in the project (it only reads the map); other commands keep
  theirs.
- RustGIS asks for the state only when the panel opens, and takes in what the plugin's own
  commands answer. It doesn't run the plugin when the map changes in other ways: undo, another
  plugin, a project opened. A state command that reads what it shows from the map (which
  release is on it) puts the panel right when it is opened again.
- The values are kept while RustGIS runs, not saved with the project.
- `plugin.panel {"id": "<plugin>"}` answers a panel's state (`values` and `items`), and sets
  `values` or, with `"refresh": true`, runs the state command first. On the command line, which
  has no panes, the commands still run with the values given.
- RustGIS versions before panels ignore `panel` (and refuse a tab that names it): keep the
  commands usable from the ribbon too, or set `minRustgisVersion`. Versions before dropdowns,
  dates and ranges refuse a panel that has one, so set `minRustgisVersion` to the first version
  with them.

#### Picking a place on the map

A `pick` lets the user show the plugin a place: where to grow a training sample from, a
site to report on, an area to download.

```json
{ "kind": "pick", "id": "seed", "label": "Grow a Sample", "command": "grow" },
{ "kind": "pick", "id": "area", "label": "Download Inside a Box", "shape": "box", "command": "download" }
```

- Clicked, the pick waits for the map, and the map's hint and the status bar say what for. The
  next click on the map is the point (dragging still pans the map); for a box, the next box
  dragged on it. Esc, choosing another tool, clicking the pick again or closing the panel stops
  it waiting, with nothing run. A pick turns the Explore tool on.
- The place is the pick's value, and its `command` runs with it, as any widget's:

  ```json
  "panel": {
    "values": {
      "seed": { "lon": -42.8, "lat": -5.09, "x": -4764474.2, "y": -567363.0, "crs": "EPSG:3857" },
      "area": null
    },
    "changed": ["seed"]
  }
  ```

  A point has `lon` and `lat` in degrees (WGS 84; on another body's map, that body's) and `x` and
  `y` in the map's coordinate system, `crs`: its EPSG or ESRI code (`EPSG:3857` for Web Mercator
  and the globe), or the PROJ definition of another projection (`+proj=robin …`). A box has `bbox`,
  `[west, south, east, north]` in degrees (a `bbox` whose west longitude is greater than its east
  crosses the antimeridian), and `extent`, `[xmin, ymin, xmax, ymax]`, the surrounding box in the
  map's coordinates, with `crs`. A box across the antimeridian is measured through it on Web
  Mercator and the globe, unwrapped: its `xmax` goes past the world's edge (20037508.34 m). On a
  projection whose edge it crosses (Robinson's at 180°, for one) its two parts sit at the map's two
  sides, and `extent` is the box around both, as wide as the map. A plugin that needs boxes inside
  the world splits `bbox` at the antimeridian. A place on another body's map, or one the projection
  can't show, has only its degrees, and on Web Mercator and the globe one past ±85.05° of latitude
  has its map coordinates at that edge, each with a warning. A place keeps the map coordinates of
  the map it was picked on, which its `crs` names, until it is picked again.
- The pane shows the place picked last under the pick, and the map marks it (a box's outline)
  while the panel is open.
- A plugin can clear a pick, `"values": {"seed": null}`, but not pick a place itself: another
  place in its answer is refused with a warning, and the place the pick holds, given back (an
  answer with the whole panel), is kept quietly. A script gives the degrees, and RustGIS works
  out the rest from the active map: `x`, `y`, `extent` or `crs` given other than the map's are
  replaced, with a warning. A place it can't read (degrees out of range, a `bbox` that isn't
  four numbers, south above north) is not taken, and a field it doesn't know is left out, each
  with a warning:

  ```sh
  rustgis-cli run --sample --cmd 'plugin:my-plugin/grow={"panel": {"seed": {"lon": -42.8, "lat": -5.09}}}'
  ```

- It works the same in the desktop app and the web app. On the command line, which has no map
  to click, the command still runs with the place given.
- RustGIS versions before picks refuse a manifest with one (its kind is unknown to them), so
  set `minRustgisVersion` to the first version with picks.

### What a plugin can ask for

Inside a request, `host::call(op, params)` sends one of these to RustGIS:

| Request | Answer |
|---|---|
| `host.info` | `{rustgis, abi, plugin, permissions}`: version, ABI, plugin id and permissions |
| `host.layers` | `[{id, name, raster, visible, geometry, features, selected, active, extent, transparency, group, fields: [{name, type}]}]`, top first: `extent` is `[west, south, east, north]` in longitude/latitude (`null` when empty or a raster not yet read), `transparency` the layer's own, 0 (opaque) to 100 (its groups' transparency applies on top), `group` the name of the group it is in or `null`. The web tile layers, groups, tables and layouts (with their page size) are in `info.project` |
| `host.features` `{layer, selected?, limit?}` | the layer's features as GeoJSON; each feature has its `row` |
| `host.view` | `{center: [lon, lat], scale, projection, bearing, pitch}` (bearing and pitch in degrees, 0 for a flat north-up map) |
| `host.notify` `{text, error?}` | shows a message in the status bar |
| `host.addLayer` `{name, geojson}` | adds a layer (`edit`) |
| `host.fetch` `{url}` | `{url, text, json?}`: the document at a web address (`network`; see [Fetching a document](#fetching-a-document)) |
| `map.addTiles` `{url, kind?, name?, attribution?, maxZoom?, overlay?, replace?, blend?, …}` | adds a tile layer (`edit` and `network`); accepts the built-in command's parameters |
| any command id (`layer.zoomTo`, `select.byAttributes`, `layer.symbology`…) | what the command returns, within the plugin's permissions |

In the apps a plugin's command (from the ribbon, command search or a panel's widget; not its
panel's state command) doesn't wait for a download, so where `layer.addUrl` has to
download a file whole it answers `{"pending": true, "url": …}`: RustGIS downloads the data off the
frame, saying so in the status bar, and adds the layer when it lands (its own undo step), as
**Home › Add › Add Data › From URL** does. On the desktop that goes for reading a GeoTIFF's,
FlatGeobuf's or GeoParquet's header too; in the web app a PMTiles archive, a cloud-optimised
GeoTIFF or a FlatGeobuf or GeoParquet whose header was fetched already is added at once. A window
of a cloud-optimised GeoTIFF (`"bbox"`, on the desktop) is read and added the same way. Up to 16
downloads wait at a time: a request past 16 fails, and if the command then fails too, none of its
downloads start. Run by
`rustgis-cli`, a script or a task, the command waits and answers what was added.

The command then runs again, as for a [fetch](#fetching-a-document): RustGIS undoes what the call
did to the project and, once every download it left has been added (or failed), runs it again with
the same parameters. In that run the same `layer.addUrl` call answers what adding the download
answered (`{"layer", …}`, or its error) and adds nothing, so the command goes on with the layer:
clips it, renames it or styles it. Everything the command did before the call runs again as well,
and only what it did to the project is undone: keep side effects outside the project (files
written, services called) after the call, or make them safe to repeat. The added layer stays its
own undo step, and what the command
does with it is another. So a command can be written as if `layer.addUrl` waited:

```rust
let added = host::call("layer.addUrl", json!({"url": url, "name": "Rivers"}))?;
if added.get("pending").is_some() {
    return Ok(json!({}));           // runs again once the layer is in
}
host::call("gp.run", json!({"tool": "clip", "params": {"input": "Rivers", "clip": "Area"}}))?;
```

A call asked again while its download is under way (the button clicked twice) waits for that
download, not another. A command runs again at most 64 times, and a download not added within 10
minutes runs it again with an error.

`service.add` does the same on the desktop: it reads the service's documents off the frame,
answering `{"pending": true, "url": …}`, and leaves a feature layer or a coverage to download as
`layer.addUrl` does, so the same call answers what adding it answered once the command runs again
(a feature layer's with `"area"`, when asked for over one). The command may run several times on
the way, once for each document at most, and a map layer is added by the run that has them. Up to
16 documents are read at a time, and asking for more fails. In the web app
`service.add` still refuses to download.

While a command waits the window answers, so the user can work and the project can change. The
status bar says the command is running, and asking for it again with the same parameters does
nothing. A run again sees the view the command first ran with, and the window's size then, unless
it moves the view itself; the layers, the selection and the rest are as they are by then. With
another map shown by then, or another project open, it isn't run again: it stops with an error. A failed answer is given once a run, in
the order it came, so a call made again after it is a new try. Because everything before the call
runs again, a command replacing a layer removes the old one after the call: removed before it, the
run again finds its new layer gone, and says so. With the old name still in use the new layer came
in as `Name (2)`, so rename it once the old one is gone.

Other commands that wait for a server (`service.layers`, `host.fetch`, say) still wait with the
window held. A command the user starts from the ribbon, command search or a panel first shows
"Running …" in the status bar with a busy cursor, and clicks made during a long run are dropped,
so they don't run it again.

`rustgis-cli commands` lists every command id and its parameters.

#### Fetching a document

`host.fetch {"url": "https://…"}` reads a small text document, such as a web API's JSON
answer, with a GET request. It needs the `network` permission. The answer is
`{"url", "text", "json"?}`: `url` is the address fetched, `text` the document, and `json` the
document parsed when it is a JSON object or array.

- Only `http://` and `https://` addresses on the internet: not this computer, the local network
  or cloud metadata addresses, and no redirect may lead there or from `https` to `http`. Names
  are checked as written and again where they resolve, on every redirect too, so a name that
  resolves to this computer or the local network is refused on the desktop and the command line
  (in the web app, the browser decides what it may reach). Requests carry no credentials (an
  S3 connection's included) and no headers of the plugin's. On the desktop and the command line
  they never go through a proxy (`HTTPS_PROXY` and the like are ignored), since a proxy would
  reach addresses RustGIS can't check.
- At most 2 MB, in UTF-8 (a byte-order mark is dropped). An answer that isn't a success (404,
  500…) is an error ending in `HTTP <status>`.
- Answers are kept for 10 minutes (up to 256 documents and 16 MB in all) while RustGIS runs,
  so asking for the same address again costs nothing. Each plugin has answers of its own: one
  never reads what another fetched. Errors are kept for 30 seconds,
  so every command waiting for an address sees its error; after that it is fetched again.
- On the desktop and the command line the request waits for the answer: at most 20 seconds for
  one, and 60 seconds for all the fetches of one plugin call. A call makes at most 64 fetches.
- In the web app nothing may wait. A fetch that isn't kept yet answers
  `{"pending": true, "url"}` and starts in the background. The command should then return
  straight away, before doing anything outside the project (writing a file): RustGIS undoes
  what the call did to the project (its error notices stay, and the next run doesn't repeat them;
  anything done outside the project stays too), and runs the command
  again with the same parameters once every pending answer has landed (up to 8 per call), when
  `host.fetch` answers from what was kept. A command that walks a chain of documents, each
  address taken from the one before, runs once per step, up to 64 times. The browser follows
  redirects itself, and the server must allow requests from web pages (CORS).

Use `host::call("host.fetch", json!({"url": url}))` in the Rust SDK.

Tools work differently. A tool receives its layer parameters' features as GeoJSON (only the
selected features when a layer has a selection), and its `file` parameters' files. It returns
new layers and standalone tables, or a selection, plus messages for the Geoprocessing history.
RustGIS adds the results as it does for built-in tools, with undo; returning these results
does not require `edit`. Tools can log, but they make no host requests.

In the desktop app a plugin tool runs in the background, so the map and the rest of the window
stay usable: the Tools pane shows the line the tool last logged, or a process plugin's last
`progress` message, and a **Cancel** button. Cancel stops a process plugin's program; a WebAssembly tool runs on to its
end and its result is dropped. One plugin tool runs at a time. `gp.run`, scripts and
`rustgis-cli` still wait for the result.

### The ABI (version 1)

The SDK implements this; it is described here for other languages.

The module is `wasm32` with no imports except these three, in module `rustgis`:

- `call(ptr: i32, len: i32) -> i32` runs the host request at `ptr`
  (`{"op": "...", "params": {...}}`). It returns the length of the reply, which is
  `{"ok": value}` or `{"error": "text"}`, or `-1` when there is none. A new call replaces any
  reply not yet copied.
- `result(ptr: i32)` copies that reply to `ptr` and consumes it. Allocate the length `call`
  returned first.
- `log(ptr: i32, len: i32)` logs a UTF-8 line.

The module exports:

- `memory`.
- `rustgis_alloc(len: i32) -> i32`, which returns room for `len` bytes. RustGIS writes the
  request there.
- `rustgis_call(ptr: i32, len: i32) -> i64`, which handles the request and returns
  `reply_ptr << 32 | reply_len`. The reply is `{"ok": value}` or `{"error": "text"}`.

Command requests are `{"abi": 1, "kind": "command", "id": "...", "params": {...}, "lang": "de"}`.
`id` is the local command id from the manifest, not its `plugin:…/…` id. `lang` is the interface
language's code (`en` for English). Commands of a plugin with a [panel](#a-panel) also get
`panel`, and may answer `panel` beside the rest of their `ok` value.

Tools get `"kind": "tool"` and their local tool id, plus `layers`, which maps layer names
(not parameter names) to FeatureCollections. When they have file inputs, `files` maps each
parameter name to `{"name": "...", "data": "..."}`, with the contents in standard padded base64.
A tool's `ok` value can contain:

- `layers`: `[{"name": "...", "geojson": {...}}]`.
- `tables`: the same shape, using FeatureCollections whose features have no geometry.
- `selection`: `{"layer": "<layer name>", "rows": [0, 2, ...]}`. If present, it takes precedence
  over `layers` and `tables`.
- `messages`: an array of strings.

RustGIS takes at most 64 layers, 64 tables and 200 messages, with at most 5,000,000 features
in each output dataset.

Each call gets a fresh instance, so a module keeps no state between calls and need not free
memory. Host requests and module replies are limited to 128 MiB.

## Process plugins

A process plugin is a program on your computer instead of a WebAssembly module: a Python script,
or an executable in any language. It can use the file system, threads and any library, which
lets existing analysis code (numpy, xarray, GDAL…) become a plugin without a rewrite. It is **not
sandboxed**: it can do anything you can. The install prompt says so, and the Plugin Manager
marks it *Not sandboxed*, in **Browse** too, before it is installed (with the Python version
its environment downloads, if it has one). Process plugins run on the desktop and the command
line, not in the web app: its **Browse** list shows them as *Desktop and command line only*,
with no **Install** button.

```json
{
  "id": "hello-python",
  "name": "Hello Python",
  "version": "0.1.0",
  "runtime": "process",
  "run": { "windows": ["python", "main.py"], "default": ["python3", "main.py"] },
  "permissions": [],
  "commands": [{ "id": "layer-count", "label": "Count\nLayers" }],
  "tools": [{
    "id": "feature-count", "name": "Count Features", "summary": "Counts a layer's features.",
    "params": [{ "name": "input", "label": "Input layer", "kind": "vectorLayer", "required": true }]
  }]
}
```

- `"runtime": "process"` and `run`: the program and its arguments, started in the plugin's
  folder. A bare name (`python3`) is looked up on `PATH`; a relative path (`bin/tool`) is in the
  plugin's folder. `run` can name a program per platform (`windows`, `macos`, `linux`, and
  `default` when the platform has no entry). Omit `entry`; process plugins use `run` instead.
  Each argument list contains the program and at most 63 arguments.
- Commands, tools, tabs, translations and permissions work as for WebAssembly plugins.
  Permissions still decide which host requests RustGIS answers, but they don't limit what the
  program itself can do.
- A `.rgplugin` holds the plugin's files (the manifest, scripts, executables). Installing it
  unpacks them to `unpacked/<id>/` in the plugins folder, keeping Unix execute bits and
  rejecting unsafe paths or more than 1 GiB of unpacked files. A plugin folder installed for
  development (Plugin Manager › Settings, or `rustgis-cli plugins install ./folder`) runs where
  it is.

### The protocol

The [Rust process template](https://github.com/opengeos/rustgis-rust-process-plugin-template)
provides a command, a tool and a reusable JSON-line transport module, with packaging and release
builds for Linux, Windows and universal macOS. Users run the packaged executable without Rust or
Python installed. Native plugins are not sandboxed and do not run in the web app.

Each command or tool call starts the program once. It speaks the [ABI](#the-abi-version-1)'s
requests and replies over its standard input and output, one JSON object per line:

1. RustGIS writes the [request](#the-abi-version-1) as one JSON line, including `panel` for a
   command of a plugin with a panel, and `layers` and optional `files` for a tool.
2. The program writes lines:
    - `{"call": {"op": "...", "params": {...}}}`: a [host request](#what-a-plugin-can-ask-for).
      RustGIS answers with one line, `{"ok": value}` or `{"error": "text"}`.
    - `{"log": "text"}`: a line for the log (and a tool's run messages).
    - `{"progress": "text"}`: what a tool is doing now, shown while it runs and not kept in its
      messages: a line per step of a long run, say.
    - Last, `{"ok": value}` or `{"error": "text"}`: the result.
3. RustGIS then closes the program's input; a program still running a few seconds later is
   stopped.

Output lines that aren't protocol messages are logged and skipped, since libraries print (even
JSON), and so is the program's error output. A command is stopped after 2 minutes; a tool runs
until it finishes or is cancelled (the desktop app runs it in the background: see
[What a plugin can ask for](#what-a-plugin-can-ask-for)).

The plugin above, in Python with no SDK:

```python
import json
import sys


def call(op, params):
    """Sends a request to RustGIS and returns its answer."""
    print(json.dumps({"call": {"op": op, "params": params}}), flush=True)
    reply = json.loads(sys.stdin.readline())
    if "error" in reply:
        raise RuntimeError(reply["error"])
    return reply["ok"]


req = json.loads(sys.stdin.readline())
try:
    if req["id"] == "layer-count":
        n = len(call("host.layers", {}))
        call("host.notify", {"text": f"The map has {n} layers."})
        result = {"ok": {"layers": n}}
    elif req["id"] == "feature-count":
        layer = req["params"]["input"]
        n = len(req["layers"][layer]["features"])
        result = {"ok": {"layers": [], "messages": [f"{layer} has {n} features."]}}
    else:
        result = {"error": f"unknown command '{req['id']}'"}
except Exception as e:
    result = {"error": str(e)}
print(json.dumps(result), flush=True)
```

### Session plugins

A process plugin with `"session": true` keeps one program running between its commands, so it
can ask RustGIS for things whenever it likes and hear when the map changes: a view of the map in
a window of its own (a 3D globe linked with it) is one. The program talks to RustGIS only over
its standard input and output: no port, no token, nothing another program can join.

```json
"runtime": "process",
"session": true,
"run": ["bin/globe"]
```

- RustGIS starts the program the first time one of the plugin's commands runs and writes a first
  line `{"session": {"abi": 1, "plugin": "<id>", "rustgis": "<version>", "lang": "<code>"}}`.
- Each command comes as `{"command": request, "id": n}`, where `request` is what a per-call
  program gets. The program answers `{"ok": value, "id": n}` or `{"error": "text", "id": n}`,
  within 2 minutes.
- At any time, during a command or between commands, the program can send a host request
  `{"call": {"op": "...", "params": {...}}, "id": "any"}`. RustGIS answers
  `{"ok": value, "id": "any"}` or `{"error": "text", "id": "any"}`; the `id` is optional and comes
  back as given. Requests go through the same checks as a command's, within the plugin's
  permissions. Between commands, each request that can change the project is an undo step of its
  own, named after the plugin, and an entry in the session log; `host.fetch` is refused: it would make the app wait, and the
  program can fetch for itself. Whether a request is part of a command depends on when RustGIS handles
  it, not when the program sent it: one handled before the command starts (by a poll, or as the
  command is about to run) is answered as between commands; one handled while the command runs,
  until the program answers, is part of that command (its undo step), even if it was sent just
  before. Sending a request meant to stand alone after answering the command, and
  waiting for its reply, makes that likely but not certain: if another of its commands starts
  before RustGIS handles the request, the request is part of that command. The desktop app answers requests as they come; `rustgis-cli` and
  the Python package only while one of the plugin's commands runs (or as the next one starts);
  `rustgis-cli` ends the program after its last command, and what it asks after that isn't
  answered.
- When the map changes, the desktop app sends `{"event": "view", "view": {center, scale,
  projection, bearing, pitch}}` (the view, as `host.view` answers it), `{"event": "layers"}` (layers added,
  removed, reordered, made active, renamed, shown or hidden, restyled, made more or less transparent, their
  fields or features changed, and web tile layers changed), `{"event": "selection"}` or `{"event": "project"}` (another project
  opened). Ask for what changed with host requests. Changes the program made itself are told too: a view it set comes back as
  a `view` event, so compare before following it. An event can arrive while the program waits
  for an answer: match answers by their `id`. While the program is behind in reading its input,
  events wait, and only the latest of each kind is sent.
- `log` and `progress` lines work as for a per-call program.
- The program RustGIS started is the session: keep it running, rather than starting another
  program and exiting. RustGIS stops it, with what it started, when the plugin is turned off,
  removed or updated and when the desktop app closes. Its input closing means it is being
  stopped (RustGIS closing or gone, however it ended; the plugin turned off, removed or updated;
  the program failed or stopped answering): the program should then exit, and stop what it
  started. A program may also
  exit by itself (its window closed); the plugin's next command starts it again. One that fails
  or stops answering a command is stopped, and the error says so.
- On Linux and macOS, while RustGIS runs, what a program left running when it exited is stopped
  with it. Windows can't find what an exited program started, so there the program must stop it
  itself, as every program must once RustGIS has gone.
- The plugin's tools still start the program once per run, with the request as its first line.
- RustGIS versions before sessions run the program once per command instead, so set
  `minRustgisVersion` to the first version with them.

### Python environments

A Python plugin can have RustGIS make its environment, so its users don't install Python or
any package themselves:

```json
"runtime": "process",
"python": { "version": "3.12", "requirements": "requirements.txt" },
"run": ["python", "main.py"]
```

- `python`, `python3` or `python.exe` as the program in `run` becomes that environment's
  interpreter, on every platform. The Python version defaults to `3.12`; omit `requirements`
  for an environment with no extra packages.
- RustGIS prepares the environment when the plugin is installed: in the background in the desktop
  app (the Plugin Manager shows how it's going, with a **Prepare** button if it is not ready), and
  before the command returns in `rustgis-cli plugins install` and `get`.
  `rustgis-cli plugins prepare <id>` and the `plugin.prepare` command make it on demand.
- It uses [uv](https://github.com/astral-sh/uv), which RustGIS downloads once (a pinned version,
  checked against its pinned SHA-256; `RUSTGIS_UV` names one to use instead, which is then
  trusted as it is, unchecked). uv installs a standalone Python of the version asked for and
  the packages in the requirements file, from PyPI. Pythons and uv's cache are shared in
  `tools/` in the plugins folder; each plugin's environment is in `envs/<id>/`. One
  environment is made at a time per plugin, even across the app and `rustgis-cli`.
- The environment is made again when the Python version or the requirements change (a plugin
  update), and removed with the plugin. Until it's ready, the plugin's calls fail with a
  message saying how to prepare it.
- The first install downloads Python and the packages: tens to hundreds of MB, depending on
  them. The install prompt says so.

## Skills

A plugin can carry [skills](user-guide/assistant.md#skills) for the AI Assistant: instructions
for a kind of task, which the assistant reads when a request needs them. Put each in
`skills/<name>/SKILL.md` in the package and list the names in `plugin.json`:

```json
"skills": ["flood-map"]
```

A `SKILL.md` starts with front matter giving the same `name` and a `description` that says when
to use it, then the instructions in Markdown (the
[Agent Skills](https://agentskills.io) format). The assistant knows it as `<plugin id>:<name>`
(`hello-rustgis:flood-map`) while the plugin is enabled. A `SKILL.md` is at most 64 KB, and a
plugin has at most 32 skills.

A package of skills alone, with no code, says `"runtime": "skills"`. It has no module, commands,
tools, tab, panel or permissions, and must list at least one skill. Nothing in it runs, and it
installs from a file in the desktop app, on the command line and in the web app:

```json
{
  "id": "flood-skills",
  "name": "Flood mapping skills",
  "version": "1.0.0",
  "runtime": "skills",
  "skills": ["flood-map", "gauge-report"]
}
```

## Publishing a plugin

1. Write the plugin in your own repository, starting from the
   [Rust WebAssembly](https://github.com/opengeos/rustgis-plugin-template),
   [Rust process](https://github.com/opengeos/rustgis-rust-process-plugin-template) or
   [Python process](https://github.com/opengeos/rustgis-python-plugin-template) template. See
   [Write a plugin](https://plugins.rustgis.app/develop/).
2. Release it: the templates' **Release** workflow attaches the `.rgplugin` and its registry
   entry to a GitHub release.
3. Open a pull request on
   [opengeos/rustgis-plugins](https://github.com/opengeos/rustgis-plugins) adding that entry as
   `registry/<id>.json`:

   ```json
   {"id": "...", "source": {"url": "https://…/<id>-<version>.rgplugin", "sha256": "…"}, "categories": ["Analysis"]}
   ```

   Every plugin is listed by `source`; the registry build refuses `path` entries.
4. CI downloads the package, checks its hash, validates its manifest and module (or a process
   plugin's files), and runs it in RustGIS. A maintainer reviews it before merging.
5. Once merged, the registry lists the plugin and serves its package at
   `https://packages.rustgis.app/<id>/<id>-<version>.rgplugin`. Published packages never change,
   and every version stays available. To update, release a new version and submit a pull
   request updating the entry's `source.url` and `source.sha256`.

Pull requests get a preview with an **Open in RustGIS** link; process plugins must be tried
in the desktop app instead. The nightly workflow runs every published plugin in the latest
RustGIS. See the registry's [publishing instructions](https://plugins.rustgis.app/registry/)
for details.

The file you submit is only a pointer to the release. The registry build turns it into the
published `registry.json` that RustGIS reads, whose entries carry `package` (the package's address) and
`sha256` in place of `source`. A registry entry contains `id`, `name`, `version`, `description`, `author`, `homepage`,
`license`, `permissions`, `minRustgisVersion`, `i18n` (the name and description translated),
`categories`, `package`, lower-case hexadecimal `sha256`, and `size` in bytes.
`package` is an HTTPS address or a path relative to `registry.json`;
plain `http://localhost` and `http://127.0.0.1` addresses are accepted for local test
registries. Entries without a valid id, name, version or SHA-256 are skipped. A process
plugin's entry also has `"runtime": "process"` and, when it has a managed Python environment,
`"python": "3.12"` (the version); a WebAssembly plugin can omit both. Registries can list
skills packages with `"runtime": "skills"`. The Plugin Manager checks the package's hash
and id before installing; `rustgis-cli plugins get` also checks its runtime.
`rustgis-cli plugins browse` prints `runtime` for every entry (`wasm` when the registry leaves
it out).
