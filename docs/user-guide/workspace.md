# Workspace

Tools for working across maps and views, and for setting up the app the way you like it.

## Projects

**Project › New**, **Open**, **Save** and **Save As** manage `.rgis` project files; older
`.rgisx` projects still open. A project keeps its maps, layers, styles, layouts, tables, tasks,
models and other project items together. In-memory feature data is saved in the project;
streamed layers reopen their sources, and file-backed rasters reopen their files. Keep those
sources accessible when moving a project. See [Project items](data.md#project-items) for
templates, packages and files for individual items.

**Open** also accepts a project package (`.rgispkg`), as do drag-and-drop and the desktop
command line. A package opens as a new, unsaved project; Save writes a plain `.rgis` file.
Opening or creating a project clears undo history; saving does not.

A `•` after the project's name in the title bar means it has unsaved changes. Undoing or redoing
back to the state you last saved clears it. Before the desktop app discards unsaved changes, it
asks **Save changes to…?** That happens when you quit or close the window, and when New, Open, the
World Atlas sample, a template or a project dropped on the window would replace the project:

- **Save** saves first and then goes on. A project that was never saved asks where to write it.
  If you cancel that, or the save fails, nothing is discarded.
- **Don't Save** goes on without saving.
- **Cancel** (or Escape) goes back to the project.

Commands don't ask: `project.new`, `project.open` and the rest replace the project as they always
have when a script, a task, the AI Assistant, the command line or an attached agent runs them.
The browser app doesn't ask yet; it warns before you leave a page with unsaved changes. On macOS,
**Quit RustGIS** (⌘Q) and the Dock's **Quit** don't ask yet either. With autosave on (the
default), RustGIS keeps your changes as a [recovery copy](#autosave-and-recovery) and offers them
back the next time it starts; with autosave off, they are lost, so save before you quit.

Saved layer addresses omit passwords, access keys, tokens and signatures. The open session
keeps them, but after restarting RustGIS a protected source may need you to sign in to its
connection again or add it again with credentials.

### Autosave and recovery

The desktop app keeps a recovery copy of unsaved work. While the project has changes that aren't
saved, it writes a copy every few minutes (5 by default), and only when something changed since
the last one. Choose how often, or turn it off, in **Settings › Autosave**
(`ui.autosave={"minutes": 10}`; `0` is off). The copy is written in the background, so a large
project doesn't hold up the app. It leaves out layer passwords, keys and tokens, as a saved project
does. It is kept in the `recovery` folder of the app's
[data folder](../installation.md#settings-data-and-cache-folders), not beside your project.
Saving, opening or starting another project, and choosing **Don't Save** when you quit remove it.

If RustGIS crashes, is closed by the system or loses power, the next launch asks **Recover unsaved
work?** and says how old the copy is.

- **Recover** opens the work unsaved, under its file's name, so **Save** writes it back to that
  file. RustGIS warns you if the file was changed after the copy was made. A project that was never
  saved stays untitled, and **Save** asks where to write it.
- **Discard** deletes the copy.
- **Later** keeps the copy for the next launch.

If RustGIS crashes again before you decide, neither copy replaces the other. Up to five are kept
and offered newest first, one per launch once you recover one. Recovering never replaces a project
that has unsaved changes of its own: save it first.

The same choices are commands: `ui.recover` opens the copy, and `ui.recover={"discard": true}`
deletes it. `ui.autosave` reports the interval and any copy waiting.

Quitting asks about unsaved changes first (see [Projects](#projects)). If RustGIS is quit without
that answer (on macOS, **Quit** doesn't ask yet), it keeps a last copy of the work instead, and
the next launch offers it back, saying RustGIS was quit without saving. With autosave off, nothing
is kept. A second copy of RustGIS running alongside the first doesn't autosave, so it can't
replace the first one's copy; it says so as it starts. Nor does RustGIS autosave when its settings
folder can't lock files (some network or synced folders), where it can't tell a second copy apart.
The browser app doesn't autosave yet; it warns before you leave a page with unsaved changes.

### Session log

RustGIS keeps a **log** of the commands run in the session: what was done, when, with what, and
whether it worked. It is an audit trail of the session, for reviewing work or recalling what was
run; it runs nothing again (History's Copy as code and recorded tasks do that). Every
front end goes into it alike: the app, scripts, the AI Assistant, the command line, Python and MCP.

In the app, the **History** pane has two views: **Tool runs**, the tools run on the project (saved
with it, and each can run again), and **Session log**, which shows the log newest first: each
command with whether it worked, when it started (UTC), how long it took, its error and its
parameters (on one line; all of them on hover, and a right-click copies them). It lists the newest
200 that match and says how many there are; type part of a command's id (`layer.`, `export`) to
filter, or tick **Failed only**. **Save Log As…** writes the whole log as CSV or JSON Lines, as
the name's extension says (CSV without one; in the browser it is downloaded as CSV, and the
download isn't itself an entry in the log, as a saved file is). **Clear Log** empties it and
**Log Commands** turns it off or on.

- **What goes in:** each command that changes the project, and each that names a file, folder,
  address or connection, reads included (an export, a download, a save, opening a file), plus
  undo and redo, with the step they took back or brought back. Reading and looking around within
  the project (listing layers, describing a tool, reading features, moving the view) is left
  out. A command run by another (a script's lines, a model's steps) is inside its parent's
  entry. A plugin can't empty the log or turn it off, and reading it (`project.log`) takes the
  file permission, since it names the files and addresses the user's commands used.
- **Each entry:** `at` (when it started, in milliseconds since 1970, UTC; for a plugin's command
  that waited for its downloads, when its last run started), `command`, `params`, `ok`, `error`
  when it failed, and `millis`, how long it took.
- **For the session only:** the log is kept in memory, across the projects opened or made in the
  session, until RustGIS closes or the log is cleared. A saved project, its recovery copy, a
  package or a template carries none of it, so sharing a project shares no log. Reading, writing
  out, clearing or switching the log doesn't change the project and is no undo step; undoing a
  command doesn't take its entry back. Steps that undo takes back as one (a held arrow key
  nudging an element) are one entry, with the last one's parameters. Each copy of RustGIS (the
  desktop app, a browser tab, a Python program) keeps its own.
- **No secrets:** the parameters lose passwords, keys and tokens as History's Copy as code does, and
  so does an error's text; a document given as text (a map, a project) or a file's contents in
  base64 (any text made only of base64's characters, so a long path with no dot too), past 200
  characters, is kept as its size only (a shorter one is kept, its secrets taken
  out as any other text's), and an SQL `CREATE SECRET`'s options as `(…)`. A secret written
  as plain words, with no name or address form (a password typed into a note), isn't recognised:
  look at an exported log before sharing it. Each entry's parameters are clipped to about 4 KB of
  JSON (a long text is cut, a list keeps its first 100 items), its error to 500 characters and a
  `…`, and the log keeps its last 10,000 entries, the oldest dropped first (the pane and an
  export say how many were dropped, and which: the oldest, or older plugin entries). Plugins' commands take
  at most 1,000 of those places: past that, a plugin's new entry pushes out an older plugin entry,
  not one of the user's.

From scripts: `project.log {"since"?, "until"?, "command"?, "failed"?, "limit"?}` lists entries,
newest last (`command` is an id, or the start of one ending in a dot, `layer.`; `limit` is a whole
number, 100 by default and at most 1,000, with `count` saying how many match), and
`project.exportLog {"path": "log.csv"}` writes it out as CSV (one row an entry, the parameters as a
JSON column) or JSON Lines (`.jsonl` or `.ndjson`), as the path's extension says; any other
extension (a `.json`, a project) is refused, whatever `format` is given. `project.clearLog` empties
it (leaving its own entry) and `project.logCommands {"on": false}` stops it (what is there stays).

### GeoLibre projects

**Open** on desktop or **Project › New › GeoLibre Project…** opens a `.geolibre` or legacy
`.geolibre.json` file as a new map in the current project. Existing maps stay in place.
The same import is available to scripts:

```text
project.importGeolibre={"path":"watershed.geolibre"}
project.importGeolibre={"url":"https://example.org/watershed.geolibre"}
```

The command also accepts JSON `text`, base64 `data`, and `dir` for moved local data files.
An absolute data path is tried first, then its filename under `dir` (the project's folder by
default). A desktop project may reference files outside that folder; import projects you trust
to select those files. Browser imports can use inline GeoJSON and web addresses; local paths need desktop.
A web app link such as `?project=https://example.org/watershed.geolibre` imports the same map.
This startup link takes precedence over other data/style links so download timing cannot change
which map receives them.

The map keeps its view, bookmarks, background, layer visibility and opacity, and supported vector
styles and field labels. The result includes `map`, `layers` and per-layer `notes`, distinguishing
approximated presentation from skipped sources. Newer format versions are read as far as possible
with a note. Unsupported layer kinds and project sections are reported rather than silently lost.

Colour classes and proportional sizes are supported separately. Combined colour-and-size styles
keep colour and use a fixed size; graduated colour class boundaries are retained; proportional
sizes use RustGIS's area scaling. Label halos use RustGIS defaults. Tile overlays draw above feature
layers, and nested groups become flat folders with inherited visibility and opacity retained.
Local data paths require a locally opened project or an explicit `dir`; URL and pasted-text imports cannot read local files without that folder.
URL data downloaded in the browser fills its reserved layer position when it arrives; wait for
those downloads before saving. Saving is refused while imported layers are still downloading. Recovery copies retain completed work and omit unfinished layers. A browser import can queue at most 16 whole-file downloads
(fewer if other downloads are queued, active, or awaiting completion); a larger batch is refused before creating its map.
Split that project into smaller imports or use the desktop app. Streaming tiles and COGs do
not consume those download slots. Desktop GeoLibre URL imports wait for each download on the calling thread, which can pause the interface.
Each request, including a remote MBTiles archive, has a 60-second timeout; download larger archives locally before importing. Zarr sources use their saved variable and dimension indices,
or the first supported variable when those choices are absent. Imported Zarr stores are read
one at a time with a 128 MiB download-buffer limit; larger slices must be reduced or opened
through the desktop data workflow.

### QGIS projects and styles

A QGIS project (`.qgs`, or a `.qgz` archive) opens as a new map of the current project: **Open**
on the desktop, or **Project › New › QGIS Project…** (`project.importQgis={"path": "…/towns.qgs"}`).
The QGIS Project picker filters for `.qgs` and `.qgz` files on both desktop and web.
Its layers come in the layer tree's order, each from its file (GeoPackage, Shapefile, GeoJSON,
GeoTIFF and the other formats RustGIS reads), a CSV of
points, XYZ tiles or a WMS layer, unchecked layers and groups hidden. The map takes the project's
coordinate system and its saved view (or its layers' extent, when that view shows none of them),
with no basemap and no graticule, as QGIS draws it. A project in degrees (EPSG:4326 and other supported
longitude/latitude systems) opens in the Equirectangular projection, the plate carrée QGIS draws
it in. The import is one undo step.
Relative layer paths resolve from the project's folder on desktop. In the browser, relative
file sources cannot be resolved and are listed in the import notes.

Each layer keeps its look:

- **Renderers**: single symbol, categorized (as Unique Values, a category's merged values each a
  class, its "all other values" category the rest; by a field, or by an expression as Unique
  Values by that [expression](cartography.md), its boolean categories matching true and false;
  one that can't be read on the layer draws every feature with the first category's symbol
  that's switched on, or nothing when no category is, noted),
  graduated (as Graduated Colors, by a field: ranges by an expression are noted and drawn with
  the first range's symbol), and
  rule-based: as Unique Values on a field where every rule picks values of that field at every
  scale (an else rule the rest), else as Unique Values by an
  [expression](cartography.md) that gives each feature the first rule it meets, each rule a class
  named by its label. A rule's condition is its filter and its parents' (an else rule's: that
  none of its siblings draws the feature, at their scales), and its scale range, as `@map_scale`
  (a rule inside another within both ranges; a switched-off rule takes the rules under it). A
  top-level else rule draws the rest. QGIS draws a feature that several rules pick with each of
  their symbols, RustGIS with the first's: the notes say how many such features the layer has. A
  rule whose filter can't be read on the layer is left out, noted by its label; when none can be,
  or there are more than 200 rules, or they are too long for one expression, every feature is
  drawn with the first rule's symbol, noted. Point cluster, displacement and
  other renderers that wrap one are drawn by the one inside.
- **Symbols**: simple fills, lines and markers with their colours, widths in any unit, offsets,
  caps, joins, dashes and the symbol's opacity; a line's simple lines as stacked strokes, its
  rendering passes as their levels when the renderer draws in symbol levels; marker and hashed
  lines as strokes of markers or hash marks, at their places (intervals, vertices, the ends, the
  middle), turned with the line, their markers' offsets and angles kept; font markers as
  characters of their font, on points and along lines; a marker symbol of several layers, or one
  shifted, turned or anchored off its centre, as a point's marks; a marker angle taken from a
  field (`"field"` or `- "field"`) as the layer's rotation field; a fill symbol of several layers
  as stacked fills, its brush patterns (horizontal, vertical, diagonal, cross) and line pattern
  fills as hatch lines, point pattern fills as markers on their grid, gradient and shapeburst
  fills with their colours or colour ramp, and the fills not drawn yet (dense brushes, SVG, raster
  and random fills as a tint of their colour).
- **Layer settings**: opacity, blending, visibility range, filter (subset string), and the
  reference scale.
- **Rasters**: singleband grey as a stretch from black to white, singleband pseudocolour as a
  stretch along its colour map (interpolated), as Classify (discrete) or as Unique Values (exact),
  paletted as Unique Values, multiband colour as RGB and hillshade as Shaded Relief; with the
  contrast enhancement's bounds as the stretch (following the view when QGIS works them out from
  the canvas), brightness, contrast and gamma, bilinear resampling, and the values its
  transparency, clipping and no-data settings hide as the raster's mask.
- **Labels**: simple and rule-based, each rule a label class whose SQL clause is its filter and
  its parents' (an else rule's: none of its siblings'), with the text's field, size, colour,
  bold, italic and letter spacing, its font family (drawn in it where it is installed, else in
  RustGIS's label font), its buffer as a halo of its colour, opacity and size, its background as
  a rectangle (filled and outlined as its fill symbol says, padded by its size, its corners
  rounded), its wrapping (its wrap character and length, and how its lines line up, "follow
  placement", QGIS's default, as the side the label sits on: centred over a point, so a value
  of several lines comes in centred as QGIS draws it), its placement (Parallel and Curved along lines,
  around and over points), its repeat distance along lines, its offset from a point (and on a
  point layer, its distance around the point, as an offset upwards) and its scale range.

Projects and styles from QGIS 1 and 2 read too: their legend gives the layers' order, and the
older names of the same settings are read.

A label's text that is an expression (not only a field) carries over as its class's text
expression, and its data-defined size (turned from the text's unit into points), colour and
rotation as its [expressions](cartography.md#labeling); QGIS 2's counterclockwise rotation is
turned clockwise. Other data-defined label settings are noted by name, and a colour expression
on see-through text is drawn opaque, noted.

A symbol's data-defined size, fill and outline colours, outline width and marker angle carry over
as the layer's [symbol expressions](cartography.md#symbols-by-expression), sizes turned from the
symbol's unit (millimetres, points, pixels, inches; map units at the reference scale) into points, where RustGIS's [expression language](expressions.md) reads them. A marker of several layers
scales them all with the size one of its layers takes; a marker drawn as marks (several layers,
or one shifted or turned) leaves its colours and outline widths by expression out, noted, and so
does a line of several strokes. They apply to every feature, so a layer whose classes set different
ones (or one with a class switched off) leaves them out, noted. One using a
function or variable it lacks, or a field the layer hasn't, is noted and left out.

QGIS filter expressions (a layer's filter, label and renderer rules) carry over: those that are
SQL (fields compared with values, `LIKE`, `IN`, `IS NULL`, joined by `AND`, `OR` and `NOT`) as
before, the rest (functions, arithmetic, `CASE`, `@map_scale`) as they are, for the
[expression language](expressions.md), QGIS's map scale (`@map_scale`, `$scale`) read as RustGIS's
(see below), in filters and in data-defined symbol and label settings. One it can't read on the layer (a function or variable it
lacks, a field the layer hasn't) is noted and left out, as is one measuring with `$area`,
`$perimeter` or `$length`, which QGIS gives in the project's units. What RustGIS doesn't draw as QGIS does is drawn as
near as it can, or left out, and said: the import's notes list, layer by layer, what was
approximated (gradient and shapeburst fills' offsets left out and colour ramps other than gradients drawn from their first colour to their last, SVG, raster and random fills and dense brushes as a tint of their colour, SVG markers as circles,
dashed polygon outlines solid, label halos with sharp corners drawn round and blended ones drawn
over the map, label backgrounds of other shapes drawn as rectangles, label distances from lines
and polygons, a distance around points moved straight up, labels wrapped at a shortest line
length or justified, values outside a graduated renderer's ranges in its first or last class, file options
such as `layerid` that RustGIS doesn't read) and what was skipped (arrow lines, other data-defined
settings, draw effects, label callouts, contours and other raster
renderers RustGIS has no match for, PostGIS, WFS
and other sources RustGIS doesn't read, diagrams, joins, virtual fields, a layer's other styles,
and the project's print layouts, annotations, bookmarks, map themes and relations). A database
password in a project never reaches the notes.

A QGIS layer style (`.qml`) styles the selected layer the same way, a feature layer or a raster:
**Import QGIS Style…** in the Symbology pane (`style.importQgis={"layer": "Roads", "path":
"…/roads.qml"}`). A raster style that reads a band the raster hasn't is noted and leaves the
raster as it was. On a raster still loading (a remote file), the bands can't be checked yet: the
style is applied, noted as unchecked, and a band the file hasn't is drawn from its last band.

RustGIS works out a map's scale with the OGC's 0.28 mm screen pixel, QGIS at 96 pixels to the
inch, so the same view is a larger scale in QGIS; scales from QGIS (reference scales, visibility
ranges) are multiplied by 0.945 on the way in, so a layer shows at the same zoom in both.

## Panes and command search

The left pane has tabs for **Layers**, **Browser** and **Data Sources**. Right-hand panes
such as **Tools**, **Symbology**, **Labeling**, **Format** and **AI Assistant** remain open as
tabs: select a tab to bring it forward, or its × to close it. Right-click a tab to close it,
the other panes or all panes; an overflow menu lists tabs that do not fit.

Right-click a map or layout tab (or its entry in Browser) to **Rename**, **Duplicate** or
**Delete** it. Deletion asks for confirmation; a project must keep at least one map.

Click **Command Search** at the right of the title bar or on the Help tab, or press ++ctrl+k++
(++cmd+k++ on macOS). Search by command name, ID or ribbon location; analysis tools, workflow
names, plugin commands and plugin panels are searchable too. Click a result, or press Enter to
choose the first available one. Commands that need inputs open their dialog, pane or workflow when one
is available; unavailable commands stay visible but disabled, with the reason on hover.
Use scripts or the assistant for parameterized commands without an interactive entry point.

## Keyboard shortcuts

**Settings › Customize › Keyboard Shortcuts** assigns multiple keys to commands and map tools.
Click **Add…** and press the desired key combination. Escape cancels; **Assign Escape** binds Escape.
If the combination is already in use, choose **Replace** or **Cancel**.
Tab, Enter, Space and arrow key bindings can override their usual keyboard navigation actions.
Click a displayed key to remove it, **Reset {key}** (for example, **Reset Ctrl+T**) to restore a default key, or **Reset All** to restore
all defaults. These changes are saved as a user preference on this device. Copy, Cut and Paste stay
fixed. While typing, shortcuts without Ctrl/⌘ are paused, including Shift and Alt combinations. During
first-person navigation, unmodified shortcuts are paused except Escape, Home and Delete. Browsers may
reserve some shortcuts. In Command Search, right-click a command and choose **Keyboard Shortcuts**
to open it in the editor. If saved preferences are unreadable or exceed the supported limits, the
original value is preserved and edits apply only to the current session. **Reset All** explicitly
replaces the saved shortcuts with the defaults.

Use **Import…** and **Export…** in the editor to back up shortcuts or share them between desktop,
macOS and browser sessions. Export writes `rustgis-shortcuts.json`, containing only differences
from the current defaults. `Mod` means Ctrl, or ⌘ on macOS, so the same file works across platforms.
Import merges with your current shortcuts: review the counts of bindings, conflicts and unknown
commands, then choose **Import** or **Cancel**. The file wins on conflicting keys; unrelated
bindings stay unchanged. Unknown command ids are retained and become usable when their plugin is
installed. Import only files you trust: pressing an imported key runs its command with the supplied
parameters, which can include file writes or preference changes. Inspect shared JSON before importing.
Invalid files are rejected without changing shortcuts. Files are limited to 100,100
bytes (including version metadata) and 1,000 records. Merged preference data must fit 100,000 bytes. If saved preferences
are unreadable, use **Reset All** before importing so they are not silently overwritten.

The version 1 file format is:

```json
{
  "rustgis_shortcuts": 1,
  "bindings": [
    { "keys": "D", "command": "tool.set", "params": { "tool": "select" } },
    { "keys": "T", "command": "select.zoomTo" },
    { "keys": "Mod+T", "command": "table.open", "remove": true }
  ]
}
```

Conflicts count distinct keys whose final imported assignment replaces a different current binding.
Unknown-command counts cover distinct unknown command IDs among retained imported assignments, excluding
removals and overwritten records.
Records apply in file order; the last assignment to a key wins. A removal removes only the named
command and parameters at that key, so a different user binding is preserved.

Scripts and UI automation can use these UI-layer commands (the AI Assistant still refuses `ui.*`):

| Command | Parameters and result |
| --- | --- |
| `ui.shortcuts.set` | `keys`, `command`, optional `params`; replaces the key's binding. With `remove: true`, removes only that command and parameters at the key. |
| `ui.shortcuts.reset` | `keys` restores that key's default; no parameters resets all shortcuts. |
| `ui.shortcuts.export` | No parameters opens Save/download; `path` writes a desktop file; `text: true` returns the document as `text` without a file dialog. |
| `ui.shortcuts.import` | No parameters opens a file picker. `text`, base64 `data`, or desktop `path` validates and opens the preview, returning `bindings`, `conflicts`, `unknown_commands`. Pass the same document with `confirm: true` to merge and save it. |

See the [keyboard shortcut reference](index.md#keyboard-shortcuts) for project, map and editing
shortcuts, macOS bindings and text-field undo behavior. **Help › Shortcuts** lists the active
bindings in the app. Feature editing is described in [Editing](editing.md); layout deletion,
duplication and nudging have their own [layout shortcuts](layouts.md#layouts).

## Views and tools

- **Side by Side** shows a second project map beside the active one (add another map first);
  **Link Views** keeps maps at the same place and scale, and **Link Cursors** marks the
  pointer's place on the other map.
- **Browser View** shows the project's maps, layouts, tables, styles, tasks, models, folders and
  offline areas as cards (double-click to open); **Item Pop-ups** add hover details.
- **Pane Sets** (View › Workspace) save the left pane, open right-hand pane tabs, active pane
  and ribbon tab and bring them back. The **Workflow Manager** tracks jobs (new, in progress,
  on hold, done) with the area each covers.
- **Data Sources** (Home › Add › **Data Sources**) opens the Data Sources pane; **View › Windows ›
  Data Sources** still shows or hides it. **Home › Add › Add Data › New Connection** lists each
  kind of connection (PostgreSQL/PostGIS, MySQL/MariaDB and SQL Server are left out in the web
  app) and opens its form. Saved connections are listed in the pane, not in the Add Data menu.
  Command Search finds the pane as **Data Sources**.
- **Tasks** (Analyze › Tasks, or View › Tasks) steps through a task's instructions: read the current step and do it
  yourself, with **Show Me** to find where, or click **Run** to have it done.
    - **Show Me** points at the step's command:
        - a ribbon button gets its tab opened and is outlined for a few seconds;
        - a tool is outlined in the Analyze gallery, or opened, empty, in the Tools pane;
        - a symbology step opens the Symbology pane on its layer;
        - a command no button runs opens Command Search on its name.
    - **Record Task** (in the pane, or Analyze › Task) turns an exercise into a task: do it once, and
      each command that changes the project becomes a step. Panes, dialogs and moving the view
      aren't recorded, and a colour adjusted several times stays one step. **Stop Recording** makes
      the task, named as typed beside it, and opens it for editing. Opening or creating another
      project ends a recording without making a task.
        - Undo doesn't remove a recorded step; delete it with Edit Steps.
        - A task holds at most 200 steps, and a step's parameters at most 4 MB: add large data to
          the project before recording.
        - Steps keep their parameters as they ran, file paths included (a file you added, say), so
          check them before sharing a task. A command run on the selected layer names it, so the
          step replays on that layer whichever is selected then; where two layers share a name it
          replays on the first, so rename one first (the recording says so). A command holding a password, key or token (a database
          connection, a signed address) isn't recorded, nor is data added from a download, which is
          kept for the session only. A plain `key` parameter that doesn't read as an access key (a
          field or a lookup key, say) is recorded, and the recording warns that it might be one:
          delete that step before sharing the task if it is.
    - **Edit Steps** gives each step ▲ ▼ to reorder it, **Edit** for its title and instructions, and
      **Delete**. A recorded tool's step is named after the tool.
    - In the Browser pane, a click on a task opens its steps; **Run All Steps** is in its context
      menu.
    - A task from a file (`.rgistask`, or inside a project or a package) can't read or write files
      or run scripts. Its built-in analysis tools may still name their output layers with a plain
      name: one with `/`, `\`, `.` or `:` (a folder, an extension or a drive) makes the task count
      as writing files, so write `Buffer 1500 m` rather than `Buffer 1.5 km`. GeoLibre and plugin
      tools' outputs are refused.
- **Thumbnails** for the project and for each map, from the view or an image.
- **Animation**: add keyframes (views), play them, and export the moves as a zip of PNG frames
  on desktop.
- **Time Slider** (View › Time) shows data through time, in a timeline docked under the map.

    <figure markdown="span">
      ![The Time Slider under the map at 2018, showing a layer of dated events cumulatively, with a bar for each year](https://assets.rustgis.app/images/time-slider.webp)
      <figcaption>The Time Slider in cumulative mode: every event up to 2018 is drawn.</figcaption>
    </figure>

    - **Bind To Time Slider** (a layer's context menu, or the bar's **Layers** menu) picks the
      layer's date field: the one most of whose values read as dates. Text dates (`2015-03-01`,
      `2015-03-01T12:00:00Z`, `2015/3/1`), epoch milliseconds or seconds, and years (`1958`) all
      read; times are UTC. Only the features whose date falls in the slider's window draw (and
      show in the legend). The bar's bars count each step's features.
    - **Step** is an hour, a day, a month or a year, times a number; the slider starts in steps
      that suit the data's span. **Show** sets the window: **This step**, **Wide** (a step either
      side), **Wider** (three either side), or **Cumulative** (everything up to the step).
    - Play, pause, step, or click and drag the timeline; **Loop** starts over at the end, and the
      speed runs from 0.5× to 4× (0.8 s a step at 1×).
    - Imagery by date: a tile address with date placeholders (`{date:YYYY-MM-DD}`, `{YYYY}`,
      `{MM}`, `{DD}`, `{YYYYMMDD}`) follows the slider. Add it with `map.addTiles`, or a
      cloud-optimised GeoTIFF a file per date with `layer.addUrl`, giving its `start` and `end`.
      For example NASA GIBS daily MODIS imagery:
      `https://gibs.earthdata.nasa.gov/wmts/epsg3857/best/MODIS_Terra_CorrectedReflectance_TrueColor/default/{date:YYYY-MM-DD}/GoogleMapsCompatible_Level9/{z}/{y}/{x}.jpg`.
    - A Zarr variable added with **Follow the time slider** (Home › Add › Add Data › Zarr…) shows
      the step the slider is at; see [Zarr stores](data.md#zarr-stores).
    - So does a WMS or WMTS layer with a time dimension, as it is added by default (**Follow the
      time slider** in Web Data's dialogs; `service.add` without a `"time"`): its requests take
      the step's time, and a slider that is off starts with the layer's times; see
      [Layers that change with time](web-data.md#layers-that-change-with-time).
    - From scripts: `time.bind` (`"layer"`, `"field"`, `"start"`, `"end"`, `"unit"`, `"interval"`, `"show"`), `time.set` (the
      window: `"start"`, `"end"`, `"unit"`, `"interval"`, `"show"`, `"speed"`, `"loop"`,
      `"format"`…), `time.goTo` (`"date"`, or `"step"` from 0), `time.step` (`"by"`, negative goes
      back), `time.fields` (a layer's date fields) and `time.unbind`:
      `rustgis-cli run --cmd 'layer.addData={"path":"quakes.csv"}' --cmd 'time.bind={"layer":"quakes","unit":"year","show":"cumulative"}' --cmd 'time.goTo={"date":"2018"}'`.
    - **Export** on desktop writes the steps as an animated GIF (`time.export` also writes a zip of PNG
      frames). The slider, its window and the bindings are saved with the project; the off button
      turns it off and every feature shows again.
- **Time Series**: a Zarr variable's values through time at points clicked on the map, as lines on
  one chart; see [Zarr stores](data.md#zarr-stores).
- **Profile**: elevation profiles along clicked points or selected lines, auto-completed to the
  raster's edge, or across a line (a cross section), with gain, loss and a chart.
- **Infographics** (Home tab) summarise what's in view: counts, totals and top categories.
- **Offline**: Download Map takes the data in view into an offline area; open it to edit, then
  Sync sends changed, new and deleted features back and refreshes it. A feature changed in the
  map since the last sync and also edited or deleted offline takes the offline version, with a
  warning naming it (Undo reverts the whole sync, restoring the map's version); an offline edit
  to a feature since deleted from the map is dropped, with a warning too. **Feature Cache** prepares
  every layer's geometry for the current projection (Fill, Empty, Cancel, Auto Cache).
- **Settings › Customize** chooses the ribbon's tabs and the Quick Access Toolbar's extra
  buttons. Settings and Help always remain visible. **Help** opens the user guide and the
  **Diagnostic Monitor** (frame rate, draw time, data size).
- **Plugins**: **Plugins › Plugin Manager** installs, updates and removes [plugins](../plugins.md)
  from the registry at [plugins.rustgis.app](https://plugins.rustgis.app) or from a file.
- **Language**: **Settings › Language** switches the interface among
  [20 languages](../i18n.md) and chooses the [worldview](maps.md#worldview) of the built-in
  borders.
- **Autosave**: **Settings › Autosave** sets how often the desktop app keeps a
  [recovery copy](#autosave-and-recovery) of unsaved work, or turns it off.
- **Start-up**: **Settings › Start-up** sets what RustGIS opens with: **Start With** the World Atlas
  sample or a blank map, or, in the desktop app, **Last Project** (the `.rgis` project last opened
  or saved) or **Choose Project…** (one you pick once), the **Projection** the map opens in (a
  preset or a supported projected EPSG or ESRI code, such as `3010` or `ESRI:102033`), and the **Extent** it opens on (the
  full extent, or **Use Current View** to keep the view you are looking at). A project opened at
  start keeps its own projection and view; the Projection and Extent settings apply to the sample
  and a blank map. If the project no longer opens (moved or deleted), RustGIS starts with the sample
  and says why. After a crash it starts with the sample too, in case the project is what crashed, and offers
  back the [recovery copy](#autosave-and-recovery) autosave kept of unsaved work, if any;
  starting `rustgis --sample` also skips the start-up project. The choices are remembered on this
  computer, or in this browser for the web app. Opening files from the command line starts a blank
  map.
