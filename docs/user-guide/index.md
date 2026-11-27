# User Guide

This guide walks through RustGIS by task. Each page covers one part of the app:

| Page | What it covers |
| --- | --- |
| [Data](data.md) | Adding files, databases, saved connections, Data Sources, large files, rasters, Zarr and project items |
| [Web Data](web-data.md) | Public web services and catalogs: STAC, Overture Maps, OpenStreetMap, NASA, open-data portals, satellite embeddings |
| [Supported Data Formats](data-formats.md) | Every format RustGIS reads and writes, how to add it, and its limits |
| [Maps and Basemaps](maps.md) | Basemaps and web tile layers, projections, the 3D globe, view effects and navigation |
| [Cartography](cartography.md) | Symbology, labeling, layer effects and graphics |
| [Editing](editing.md) | Creating and modifying features, data design, the clipboard and pop-ups |
| [Analysis](analysis.md) | Selection and SQL queries, geoprocessing, the GeoLibre tools, guided workflows, imagery, data cubes and time series |
| [Expressions](expressions.md) | The expression language of queries, filters and Calculate Field: grammar, operators, functions |
| [Layouts and Reports](layouts.md) | Page layouts, map series, reports, presentations and notebooks |
| [Sharing](sharing.md) | Exports, packages and open-format web maps, layers, styles and scenes |
| [Workspace](workspace.md) | Linked views, the Browser view, animation, profiles, offline areas, customizing the ribbon |
| [Field Work](field-work.md) | The phone apps: capturing and editing by touch, the device's position and tracks, photos, offline basemaps and field packages |
| [AI Assistant](assistant.md) | A chat pane that answers questions and acts on the map through commands |

<figure markdown="span">
  [![RustGIS with the Home tab of the ribbon open over a world map in the Equal Earth projection, countries shaded by population](https://assets.rustgis.app/images/hero.webp)](https://assets.rustgis.app/images/hero.webp){ target="_blank" rel="noopener" title="Open the full-size image" }
  <figcaption>RustGIS with the World Atlas sample in the Equal Earth projection.</figcaption>
</figure>

## The window

- **Ribbon**: tabs of commands grouped by task. The **Project** button opens the backstage
  (Start, New, Open, Save, Save As and About).
- **Quick Access Toolbar** (top left): New, Open, Save, Undo, Redo and
  [Undo History](editing.md), plus any commands you add under **Settings › Customize › Quick Access**.
- **Layers pane** (left): the map's layers in drawing order, with their legends. A new layer goes
  on top, whatever its kind (a feature, raster or web layer, an analysis result). Tick a layer to
  show or hide it; drag it up or down to change the drawing order; right-click it to zoom to it,
  rename it (or press ++f2++) or remove it and more. The entire layer block, including its
  expanded legend and classes, is split 50/50: drop in its upper half to insert above, or its
  lower half to insert below. The insertion line shows the destination. Drop inside the first
  block's upper half to move to the top, or inside the final block's lower half to move to the
  bottom.
  Type in the box at the top to show only the layers and tables
  whose names contain the text (++escape++ clears it); a selected layer it hides is no longer
  selected, and layers can be reordered again once it is cleared. The buttons beside the list's
  title list the layers by **drawing order**, by **data source** (grouped under the file, web
  address or database each comes from) or by **selection** (selectable layers first, each with a
  box to make it selectable or not and its count of selected features); only the drawing order
  reorders layers. A layer that isn't selectable is skipped by the Select tool's clicks and boxes
  on the map; Select By Attributes, Select Layer By Location and the attribute table still select
  in it, since they name the layer.
  **Pick several layers** with ++ctrl++ (++cmd++ on a Mac) and click to add or drop one, or
  ++shift++ and click to take the rows from the last one clicked; the layer clicked last,
  outlined, is the one the Style and Data tabs act on. Right-click one of them to zoom to them
  all, show or hide them, allow or block selecting their features on the map, move them to the top
  or bottom of the drawing order (one in a group, to the top or bottom of its group), put them in
  a group or take them out of one, raise or flatten their polygons together
  ([Extrusion](cartography.md#layer-effects)), or remove them; a layer the filter hides is left out.
  A new group made from the menu asks for its name. In scripts, `layer.select` takes
  a list and a `mode` (`replace`, `toggle` or `extend`), and the commands that remove, show, move,
  group or zoom to a layer take a list in `layer`, or the picked layers when it is left out.
  **Group layers** to keep related ones together, such as one dataset drawn as several layers
  with their own visibility ranges: right-click a layer and pick **Group › New Group**, or move
  it into a group that already exists. Groups nest, up to eight levels (*Base map › Hydrography
  › Rivers*): a new group made from layers that are all in one group goes inside it. A group
  folds open and closed with its arrow; its box shows or hides all its layers at once, its
  subgroups' too (each keeps its own box), and its menu renames it, sets a transparency over its
  layers' own, ungroups it (its layers and subgroups go up one level) or removes it with its
  layers and subgroups. The visibility, transparency and visibility range of every group above a
  layer apply to it.
  Drag a layer or group onto a group's row to put it in that group, or between rows to put it
  in the group of the row below; drag it out of the group's rows to take it out. Drag a group's
  row to move all its layers in the drawing order. Like a word processor's outline, a layer's or
  group's menu has **Indent** (into the group just above it, as its last item) and **Outdent**
  (out of its group, just below it); ++alt+shift+right++ and ++alt+shift+left++ indent and
  outdent the active layer, ++alt+shift+up++ and ++alt+shift+down++ move it one step, past a
  subgroup at once. Scripts and the AI Assistant build groups with `group.add` (`parent` puts it
  in another), `group.set`, `group.scaleRange` (a visibility range on top of each layer's own),
  `group.move`, `group.setParent`, `group.indent`, `group.outdent`, `group.expand`,
  `group.ungroup`, `group.remove`, `layer.group`, `layer.indent` and `layer.outdent`.
  **Copy Layer** (or **Copy Group**, with its layers and subgroups) from its menu and **Paste
  Layer** (or **Paste Group**) it on a layer (above it), a group or the map's name above the
  list (at the top, also when the map has no layers yet), after switching to another map if you
  like, or on a map in the Browser (at the top). The copy keeps its source, symbology, labels,
  transparency, scale range, definition query, display filters and mask, but not its selection,
  and a name the map already has gets " copy". Pasting into the same map duplicates the layer. A
  mask whose layer the map lacks is cleared, and said. Pasted into a group (on it, or on one of
  its layers), the copies go in it at the top, copied groups too. Scripts use `layer.copy` and
  `layer.paste`.
  The Style and Data tabs act on the layer selected here. The tabs at the top of the left pane
  switch it between **Layers**, **Browser** and **Data Sources** (also in **View › Windows**).
  The planet button above the list switches the map between the Earth and other celestial bodies.
- **Map view**: one tab per map and layout. The buttons at the top right zoom in, zoom out and go
  to the full extent; the north arrow and scale bar sit in the corners (on a rotated or tilted
  map, click the north arrow to face north and look straight down); tile credits at the bottom right.
- **Panes** (right): Tools (geoprocessing), Symbology, Labeling, Layer Properties, History,
  Bookmarks, Editing, Workflows, Script, SQL, the AI Assistant and more. Open them from **View › Windows** or the commands that use
  them. Each pane opened gets a tab at the top, in the order opened: click a tab to show its pane,
  and its **✕** to close it (the next tab shows; closing the last hides the side). Right-click a
  tab to close it, the others or all; the **✕** at the header's end closes every pane. Tabs that
  don't fit are listed under **»**. A ribbon button for a pane shows it when it is open but not in view,
  and closes it when it is. **Pane Sets** keep the tabs open.
- **Status bar**: the scale (type one, or pick from the list), the zoom level, bearing and pitch
  (click them to face north and look straight down again), the pointer's coordinates (click them
  to step through map coordinates (the map's own system, the default), decimal degrees, DMS, UTM, MGRS and layer coordinates (the active layer's projected system); see [Coordinate readouts](maps.md#coordinate-readouts)),
  the coordinate system (click its button to
  change the projection), the selection count (with a button to clear it), the latest notice (click
  to open **Messages**), a **Messages** button, the last draw time, the frame rate and a **GPU**/**CPU**
  renderer switch. While agents control the desktop app, it also shows the MCP relay or file bridge.

**Command Search** (++ctrl+k++, or the box at the top right) finds commands and geoprocessing
tools by name. Commands with an interactive entry point open their dialog, file picker or pane
to collect input: for example, Basemap, Add Tile Layer, Rename, Select By Attributes and exports.
Workflow tools such as Georeference and Classification Wizard open in the Workflows pane.
Workflows can also be found by their translated titles or section ids, such as `deepLearning`.
A command that needs input but has no interactive entry point stays listed greyed out, with
the reason on hover, instead of running without its required parameters.
Desktop-only database connections are also disabled in the browser.

To give the map more room, collapse the ribbon to its tab row with the chevron at its right
end, by double-clicking a tab, or with ++ctrl+f1++ (++cmd+f1++ on macOS, where F1 may need the Fn
key). Click a tab to peek at its commands while collapsed. When the window is narrower than the
ribbon, arrows at its ends scroll it.

### Messages

Open **View › Windows › Messages**, click the status-bar notice or **Messages** button,
or search for **Messages** in Command Search (`ui.pane.diagnosticLog`). This troubleshooting
history is separate from **Help › Diagnostic Monitor**, which still shows performance metrics.

Messages are collected even while the pane is closed. The newest entries appear first, with
a UTC timestamp, severity, and source (such as the command id or Network). Expand **Technical
details** for failed requests: their address, method, observed HTTP status and duration when
available. Native connection failures and timeouts are identified when the transport knows
them; an opaque browser failure does not claim a status or a specific DNS, CORS, or connection cause.

Use **All**, **Errors**, **Warnings**, **Info**, or **Network failures** to filter the history
(**Network failures** also lists the notes below about omitted absences). **Copy diagnostics**
copies the filtered entries and their technical details, including folded details, as plain
text: oldest entry first, with UTC Unix-millisecond timestamps, where the pane shows the newest
first and a UTC date and time. **Clear** clears all entries, not just the selected filter. Closing
or reopening the pane does not clear it.

A map tile a server doesn't have (HTTP 404, or 410 where it is gone) is routine, as is a Zarr
store's absent chunk, so each isn't listed. One **Network** note per server says that such
absences are omitted, naming the first address. Past 64 servers one more note says that further
servers aren't listed. A note that has left the 500 entries, or a **Clear**, lets its server be
noted again.

The last 500 entries are kept in memory for this session, including across project changes;
older entries are discarded. Each message is limited to 2 KiB and technical details to 8 KiB.
Nothing is saved in `.rgis` projects or sent automatically. Credentials are removed before
retention: addresses omit user information, query strings and fragments; credential fields
and structured payloads are omitted conservatively, which can also remove surrounding context.
A credential field in the middle of a line is removed when its value is one word or quoted, but
a value of several words reads as prose and is kept (`… password: correct horse battery`).
Review copied diagnostics for private layer names or file paths before sharing them.

## The ribbon

| Tab | Groups |
| --- | --- |
| **Home** | Project (New Map, New Layout), Navigate (Pan, Bookmarks, Full Extent, Zoom In/Out, Previous/Next, Zoom Box, Planar/First-Person, Identify, Locate [By Coordinates, By Place or Address], Directions), Add (Basemap, Add Data, Data Sources, [Web Data](web-data.md)), Selection (Select, Select By Attributes, Select By Location, Clear, Zoom To, Attributes), Inspect (Measure, Infographics) |
| **Data** | Layer Templates, Data Source (Repair Source); for a feature layer: Definition Query, Table, Selection, Data Design, Archiving, Relationship and Export; for a raster layer: Information and Export; then Offline and Feature Cache |
| **Style** | For the layer selected in Layers. A feature layer: Visibility Range, Effects, Drawing, Drawing Effects, Extrusion for polygons (with Faces and Light and Shadow once on) and Layer. A raster layer: Visibility Range, Effects, Compare, Raster Effects, Rendering, Enhancement and Layer. Both: Styles |
| **Labeling** | For the feature layer selected in Layers: Labels (Label, Properties), Label Class (Class, Field), Text Symbol (Size, Color, Bold, Halo), Label Placement and Label Range. Whatever is selected: Map Labeling (Lock, Pause, View Unplaced, Convert) |
| **Edit** | Manage Edits (Edit, Save, Undo, Redo, Status, Save to PostGIS and Discard PostGIS Edits on the desktop while a [PostGIS layer is loaded for editing](data.md#editing-postgis-tables), Settings, Templates, Topology), Snapping, Features (Create, Delete, Edit Vertices, Modify), Tools, Elevation, Corrections, Graphics, Graphics and Text, Clipboard |
| **Analyze** | Geoprocessing (History, Tools, Model Builder, Script, SQL, Environments), Tasks (the Tasks pane; Task: Record Task, New Task From History, Import Task, run a task), AI (AI Assistant), Portal (Feature Analysis, Raster Analysis), Workflows, Raster (Raster Functions, Function Editor, Process), Tools (the tool gallery), Selection, Notebook |
| **View** | Display (Graticule, GPU Rendering), View (Globe, Projection, Global; Local on the desktop), Effects, Scene, Windows, Navigation, Link, Workspace, Animation, Time (Time Slider), Profile (Profile; Time Series on the desktop) |
| **Publish** | While a layout is open, its groups first (Page, Map Frames, Map Surrounds, Shapes, Text, Export); then Report, Presentation, Output (Export Map, Export Features, Print Map, Capture To Clipboard, Report, Presentation, Export To 3D), Package, Share As, Layout, Save As, Thumbnail |
| **Plugins** | Plugins (Plugin Manager, Install From File, Browse Plugins), Extensions (the [Extensions](#extensions) window), plus a group for each enabled [plugin](../plugins.md) with commands or tools; a plugin with its own tab adds that tab instead |
| **Settings** | Theme (the theme list, Follow the System, Interface Scale), Customize (Ribbon, Quick Access), Language (interface language and worldview), Start-up (Start With, Projection, Extent), Autosave on the desktop |
| **Help** | Performance (Diagnostic Monitor), Learn (Learning Resources), Help (About, Check for Updates on the desktop, Command Search, Shortcuts) |
| **Imagery** | The [Imagery extension](#extensions)'s tab, after Help, while it is on (it is off until you turn it on): Ortho Mapping, Reality Mapping, Alignment, Image Analysis, Image Classification, Imagery Sharing, Mensuration, Imagery Tools |

The tabs stay the same whatever is selected: Style, Labeling and Data act on the layer selected in Layers.

### Extensions

Extensions are optional parts of RustGIS that you turn on or off. Unlike a
[plugin](../plugins.md), an extension ships with RustGIS: there is nothing to download or install.
**Plugins › Extensions** opens the Extensions window, which lists each extension with a switch,
what it adds and where it shows. An extension with a tab of its own adds that tab after Help
(before plugins' own tabs) while it is on; one without a tab adds its buttons to RustGIS's own
tabs, or a group to the Plugins tab.

| Extension | Default | Adds |
| --- | --- | --- |
| **Imagery** (`imagery`) | Off | The Imagery tab: Ortho Mapping, Reality Mapping, Alignment (Georeference), Image Analysis, Image Classification (with Deep Learning Tools), Imagery Sharing, Mensuration and Imagery Tools (Indices, Pixel Editor), and their commands (`imagery.*`, `dl.*`, and the `raster.*` commands those groups run). Process (clip and resample a raster), Mosaic Rasters, Project Raster and Polygons to Raster aren't part of it and always run |
| **AI Assistant** (`assistant`) | On | The [AI Assistant](assistant.md) pane, with its buttons in Analyze › AI and View › Windows (`ui.pane.assistant`). The [MCP server](../mcp.md) (`rustgis-cli mcp`, and the desktop app's MCP Relay) and `rustgis-cli ask` aren't part of it and run either way |

While an extension is off, its tab, its pane, its buttons, its Workflows pane sections and its
commands in Command Search, the Keyboard Shortcuts editor, the Quick Access Toolbar and the AI
Assistant's command search are hidden. Its commands are refused with a message naming the
extension, whoever runs them: a shortcut, a script, a model, the command line, Python or the
assistant. Shortcuts and Quick Access buttons you gave them are kept and come back with it.
Hiding the extension's tab in Settings › Customize › Ribbon only hides the tab; the commands still
run.

The desktop and web apps start each extension at its default and remember your choices (the
`extensions` setting). A setting saved by a newer RustGIS that names extensions this version
doesn't have keeps them unchanged, and so do entries that can't be read (each kind is reported
in Messages). A setting that can't be read at all is reported, leaves every extension at its
default and is never written over. The command line and Python start with every extension
on, so scripts that use its commands keep working; models and scripts run in the apps follow the
apps' choices. Turn one off or on in a run with:

```bash
rustgis-cli run --sample --cmd extension.list \
  --cmd 'extension.disable={"id":"imagery"}' \
  --cmd 'extension.enable={"id":"imagery","enabled":true}'
```

`extension.enable` takes `"enabled": true` or `false` (default `true`) and refuses other values.

Menus with checkboxes stay open while you change options: stack or show web basemaps, combine
Visual and Camera Effects, or customize Ribbon tabs and Quick Access buttons. Click outside
the menu to dismiss it. The time slider's Layers menu likewise stays open while binding layers.

## Projects

A project (`.rgis`) holds maps, layouts, reports, presentations, notebooks, styles, tasks and
models. Vector data loaded whole is stored inside the project. On the desktop, linked local
files (large vector files read in place, rasters and tile archives) are saved relative to the
project's folder: moving the folder whole keeps the links. **Save As** recomputes those paths
for the new location. Files on another drive or a network share stay absolute. Web addresses,
databases and embedded data are unchanged; files uploaded in the web app are embedded. A
project opened as text, as in the web app, has no folder against which to resolve local links.
Saved web addresses drop passwords, keys and tokens. A project package (`.rgispkg`) also
embeds linked rasters within a size limit, while large files read in place, database and web
layers stay linked; it opens as a new, unsaved project.

Relative linked paths, including bare paths in older or hand-written files, are interpreted
against the project's folder, never the process's working directory. Resolution is lexical:
it does not follow symbolic links. Avoid `..` after a symlinked directory; choose the direct
data location instead.

On the backstage's **Start** or **New** page, choose a blank **Map**, the **World Atlas** sample or
**Open…**. **Save** and **Save As** write the project, named after its file; a project opened
from a file is named after that file too. See [Project items](data.md#project-items) for the
other files RustGIS reads and writes.

Changes to the project are commands, and each can be undone (++ctrl+z++) and redone
(++ctrl+y++). Undo keeps the last 40 changes; starting or opening another project clears it.
A number dragged or a colour picked in a picker is one change, however far it moves. A number
typed into a control that changes the map or a layout right away is one change too: it is set
when you press ++enter++ or click elsewhere, and ++esc++ brings back the value from before. A value
with its own button, such as the Editing pane's elevation, waits for that button.
The desktop app also keeps a copy of unsaved work and offers it back after a crash: see
[Autosave and recovery](workspace.md#autosave-and-recovery).
Exports and other files already written are not undone.

### Repair Data Source

When drawing cannot read a linked vector file, the error names the layer and its file path.
If the file was moved or deleted, restore it or repair the source. Newly failing layers share
one counted notice, listing up to five layers and their files. Screen redraws report each failure
once per layer; a successful read allows a later failure to be reported again. Every export that
succeeds without a layer reports the read failure for that output, including repeated exports.

If a linked file moved or was renamed, right-click its layer or local PMTiles/MBTiles tile
archive and choose **Repair Data Source…**. For the selected layer, use **Data › Data Source ›
Repair Source**, or find **Repair Data Source** (or **Repair Tile Archive**) in Command Search.
Choose the replacement file or folder. For a container such as a GeoPackage, **List Datasets**
reads its tables in the background; choose a table when there is more than one. The existing
table is retained when available. Repair keeps the layer's identity and configuration, marks
the project unsaved and can be undone or redone. A raster drawn with its own color table
requires a replacement with a color table; incompatible rasters are rejected without changing
the saved style.
Vector repair also refuses a table containing only a different geometry family; for example,
a Lines layer cannot be repaired to a points-only DuckDB table.
If a tile archive is removed or reordered out of its captured slot while the dialog is open,
the dialog closes rather than applying the replacement to another archive.
It does not change embedded data, databases or web links. The web app cannot link to files
on your computer: add the file again with **Add Data**.

CLI and Python use the same commands: `layer.repairSource` with
`{"layer":"Cities","path":"/data/towns.fgb"}` (and optional `"table"`), or `map.repairTiles`
with `{"index":0,"path":"/data/tiles.pmtiles"}`. Tile archives can also be selected by
`"name"` or catalog `"id"`.

### Reload Data

On the desktop, right-click a linked layer and choose **Reload Data**, beside **Repair Data
Source…**. For the active layer, use **Data › Data Source › Reload Data**, or **Reload Data**
in Command Search. Reload rereads the same local source, table or raster subdataset without
adding other tables from its container. It retains the layer's name, identity, drawing order,
group, visibility, opacity, styling, labels, filters and map camera. Local PMTiles and MBTiles
tile archives have their own **Reload Data** on their Layers row; see
[Maps › Reloading a local tile archive](maps.md#reloading-a-local-tile-archive-desktop).

Supported targets are linked vectors, rasters (including time-following Zarr variables),
GDAL/DuckDB file-backed layers and editable local GeoPackage tables. GDAL-linked File
Geodatabases and Zarr folders can be reloaded manually. Folder sources are not monitored
(**Folder source — changes are not monitored. Use Reload Data.** in the Layers row's tooltip): use
Reload Data even when the folder's timestamp has not changed.

**Changed on disk (desktop).** While the app is open, RustGIS looks at the files its linked
layers and local tile archives read every 5 seconds and when its window regains focus, in every
map of the project. It reads only metadata (size, modification time and the file's identity: on
Linux and macOS its status-change time too, on Windows its creation time), never the data: a Shapefile's `.dbf`, `.shx`, `.prj` and `.cpg` count with its
`.shp`, a MapInfo file's `.mid` with its `.mif`, a GeoPackage, MBTiles or `.sqlite` file's `-wal`
and `-journal` with it, and a DuckDB database's `.wal` with it (a `.db` file gets both). Other
sidecars (a raster's `.aux.xml` or `.prj`) aren't watched. A change is reported once two looks in a row agree:
a stability check, not proof the writer has finished (one that pauses for longer may be reported
mid-write, and again when it's done). The Layers pane marks the row **(changed on disk)**, **(missing on
disk)** or **(can't be checked)**, and a notification lists each affected file with **Reload Data**
for each layer or archive reading it (**Repair Data Source…** when a linked file is missing) and
**Not Now**. RustGIS doesn't reload anything until you choose, though a layer read in place
(streamed) reads its file as you pan and zoom, so it may already show some of the new data; Reload
Data reloads only that layer, opening its map
first, and asks before discarding GeoPackage edits (see below). **Not Now** hides the notification
for that version of the file; the row stays marked, and a later change notifies again. A reload
clears the mark for what it read; a change that came while it read stays marked. Saving edits, or
exporting into a watched file, from RustGIS's menus, ribbon and Command Search isn't reported as an
outside change, except for other layers reading the table that was written, or the file an export
replaced (a GeoJSON, a Shapefile): they still show the old rows and stay marked. (A layer read in
place from a GeoPackage table RustGIS wrote reads it again, so it isn't marked.) Watching for
changes, the marks and Not Now don't change the project or add undo steps.

Limits: a write that keeps the size, modification time and identity of every watched file isn't
seen; nor is one made while the layer is being opened, before its first look, which RustGIS takes
as what the layer read. Reload Data stays available whether or not a change was reported. A
reload, a GeoPackage save or an export run from a script, a model, the AI Assistant pane, the file
bridge or the command line doesn't clear the mark or count as RustGIS's own write (an archive
reload from an attached MCP client does clear it). Another program writing the same GeoPackage
while RustGIS saves or exports into it isn't told apart from RustGIS's own write. A file that
doesn't answer (a network drive gone away) pauses the checks of every file until it does; a
notice says so. When one map shows the same tile archive twice, removing or reordering those
two can move a mark from one to the other.

Saved GDAL/DuckDB links reread their original selected table when restoring a disposable
converted copy, including compatible empty tables. If the original cannot be read, RustGIS
does not borrow another session's temporary conversion. A durable saved conversion outside the
current cache can still open as a fallback, with one notice naming the original and its read
error (or the layer count and first failure when several layers fall back). Losses found while
converting a link again as a project opens (dropped Z or M values, clamped attributes) are
reported once per kind, with a layer count. Counts are summed only when every layer's count is
known and comparable; otherwise the notice identifies the first layer's count as its own, or
reports an unknown total. Temporary reload folders left by a crash are cleaned up later, only
when their lock file carries RustGIS's ownership marker and is unlocked after a 60-second grace
period; folders RustGIS did not create are never removed, even in a shared cache folder.
Reloading an existing DuckDB branch that has lost all its rows installs an empty layer of the
same geometry family, even when rows of another family remain; the omitted rows are reported.

Reload does not turn snapshots into live links. Embedded uploads, generated layers, ordinary
imports without linked provenance, arbitrary SQL results and layers detached by **Load All
Features** must be added again. A source name in a snapshot's history is not a live link.
Server databases and remote URLs use their separate discard/refresh actions. The web app
cannot reread a local file; add the file again with **Add Data**.

For an editable GeoPackage with pending inserts, updates or deletions, reload first shows
their counts. **Cancel** keeps the edits; **Discard Edits and Reload** explicitly abandons
them and rereads the selected table, without writing to the file. A successful reload updates
the baseline used to detect conflicts when subsequently saving edits.
Close an already-open dialog before opening the discard confirmation; Reload Data does not
discard another dialog's unfinished input.

The desktop UI prepares the read or conversion in the background, with running status and
Cancel in the status bar. Cancel installs nothing. If the source is repaired or the target is
replaced, or GeoPackage edits change after confirmation while a reload is waiting or running,
the reload is refused; review the current state and confirm again.

Reload checks a fresh candidate before replacing the layer. Incompatible geometry, missing
fields, incompatible numeric fields used by presentation settings, invalidated configured
expressions, missing tables/subdatasets and insufficient raster bands or palettes are rejected.
Saved labeling configuration is checked even when labels are disabled. A time-following Zarr
replacement whose dates cannot be reached by any step of the current slider is rejected: adjust
the slider and retry; reload does not silently change the shared slider for other layers.
An unreadable or missing source leaves the existing configuration and retained in-memory data
in place; correct the file and retry, or use **Repair Data Source…** if it moved.
For streamed sources these opening and compatibility checks are not a full-file integrity scan.
A streamed source may itself already be affected by an external overwrite: its retained handle
is not a complete last-good copy.

Replacing vector features resets selections and saved feature-specific label moves because
feature IDs may now identify different data. It also clears map-wide label locks and line
anchors: keeping a partial lock could hide the replacement's labels. Notices count reset saved
moves, locked placements and anchors, including placements belonging to other layers, and
report loader losses. Successful reload is one undo step; failure adds none.
Undo restores in-memory edits and configuration, including saved label moves and locks, but
cannot recover overwritten files or promise historical streamed features or pixels.

CLI, Python, MCP and the Assistant use `layer.reloadData` with
`{"layer":"Cities","discardEdits":false}`. Both parameters are optional: the active layer is
the default and discarding edits defaults to false. Only an explicit
`"discardEdits":true` permits losing pending GeoPackage edits. Success returns, for example,
`{"layer":42,"path":"/data/cities.fgb","table":null}`; `table` is a string for a selected table
or raster subdataset.

### Source file state

A layer read whole from a Shapefile or a `.geojson` file keeps its features in the project, so
edits change the project, not the file. It also keeps a record of what was read: each part of
the file's size and SHA-256 hash, and a hash of the layer's rows. `source.syncState` compares
the layer with the rows read, and the file with the parts read, and answers `inSync`, `edited`
(the layer has edits since it was read), `sourceChanged` (the file changed since it was read)
or `both`, with the names of the parts that changed. It doesn't compare the layer with the file
as it is now: the same edit made to both shows as `both`. Only the bytes count: writing the
same bytes again isn't a change, while a copy that puts back the file's size and time with other
bytes is one.
Undoing every edit makes the layer match its record again, so it reads `inSync` if the file
hasn't changed either. A number that isn't finite (NaN, infinity), which a
project saves as empty, shows the layer as `edited` once its project is opened again. On the desktop the file and the rows are hashed
in the background, with progress and Cancel in the status bar.

The record is made just before the file is read, so a file written to while it was being read
shows as changed, unless the write was undone before RustGIS looked again. A layer read in
place and a `.json` file have no record. The record is saved with the project; the web app
can't check it.

On the desktop, make the layer active and choose **Source File State** in Command Search; what it
found shows in the status bar. CLI and Python: `source.syncState` with `{"layer":"parcels"}`.

The desktop's Layers pane also marks such a layer as it goes: **(edits not published)** once it
holds rows its file lacks (its rows are hashed in the background while the pane shows the layer,
once after the project opens and again after each edit), and **(changed on disk)** when the file changes, by the same look at its
metadata as for linked files, never its data. A file whose length differs from the record, or
that gained or lost a part, is marked even when it changed while the project was closed; one
changed then without a change of length is found by Source File State or Publish. The row's
tooltip says what to do; no notification offers Reload Data, which can't read such a layer.

### Publish to source

`source.publish` writes a layer's edits back over the Shapefile or `.geojson` file it was read
from, so the file that other people and programs read holds them. Use **Source File State** in
Command Search (`source.syncState`) to compare the layer and its file before publishing.

#### Shapefile

A Shapefile can't change one row in place, so
the whole file is written again, and RustGIS first makes sure that only the edits change. It
refuses, with nothing written, when:

- the file changed since the layer was read (`source.syncState` would say `sourceChanged` or
  `both`): writing would overwrite that change;
- the layer was read otherwise than the file reads now (by an older RustGIS), so rows that
  weren't edited could change;
- the file, read and written again, wouldn't be the same file: shapes with Z or M values, records
  without a shape, multipatches, records marked deleted, text with spaces before it, numbers with
  more digits than RustGIS holds exactly, columns of a type RustGIS doesn't write (memo, binary),
  text in a code page RustGIS can't write. The message names what would change and where;
- the file can't hold the layer as it is: a feature without a shape, a field name longer than 10
  bytes, text in characters the file's code page lacks.

The file keeps its code page (its `.cpg` and the language driver in its `.dbf`), each column's
width, decimals and type where the values still fit, and its `.prj`. A column that can't hold
an edited value gets one that can, and a notice names it with its old and new form; a field
deleted or renamed in the layer leaves its column out of the file, and a notice names it too. A layer read in another
coordinate system than its `.prj` names, including one assigned with Define Projection, gets a
`.prj` for that system, said too. A shape that wasn't edited is
written with the coordinates the file holds, so a projected file's untouched shapes stay as they
were to the bit. An edited shape in a projected system is converted from longitude/latitude, as
RustGIS holds it, and read back: publishing refuses one that would move by more than one part in
10¹¹ of its coordinates (a tenth of a millimetre at ten thousand kilometres). A notice
names the file, counts shapes with projection round-off and identifies the first feature and
the largest coordinate difference in degrees; exact round trips give no such notice.

Publishing refuses symbolic links in any source part: add the actual source files instead.
This keeps each link and its target unchanged.

The new parts are written beside the old ones, with the old parts' file permissions, checked,
and then renamed into place, the `.shp` last. An OS lock prevents two RustGIS processes from
publishing the same source at once; a competing publish refuses immediately. The empty
`.filename.shp.rustgis-lock` sidecar stays beside the source for reuse; the OS releases its lock
when the publisher finishes or exits. On Unix, this empty file permits read/write access across
accounts so users sharing the source directory can reopen it for locking, including on NFS.
Other programs do not follow this locking protocol: changes they make after RustGIS's final
source check can be overwritten by the publish. Stop other writers before publishing.
If one part fails to land, the old parts are put back;
should putting one back fail too, the message names the temporary file that holds it, beside the
Shapefile, so it can be renamed back by hand. Sidecars RustGIS doesn't write, such as `.shp.xml`
metadata, are left alone. The spatial indexes that would no longer match (`.qix`, `.sbn`, `.sbx`)
are removed where they can be; one that can't be (held open by another program) is left, and
should be deleted before that program uses the file. Once the new parts have landed, the hidden
copies of the old ones are removed too; a copy that can't be removed is left beside the
Shapefile, and the publish names it so it can be deleted.

This rollback needs RustGIS to keep running. Replacing several Shapefile parts is not atomic:
a process crash or power loss between renames can leave a mixed or incomplete set. Keep an
independent backup before publishing. After such an interruption, preserve the whole directory,
including hidden `.<part-name>.<process-id>.<number>.tmp` files (for example,
`.parcels.dbf.1234.5.tmp`); these can contain both staged new parts and displaced old parts.
Restore a complete, consistent Shapefile from your backup before opening or publishing it again.
Do not restore an arbitrary temporary file or mix parts from different versions. Temporary
files may help with manual recovery, but are not a crash-safe backup.

Once published, the layer holds its rows as the file reads them, which differs only where a
Shapefile can't tell values apart (an empty text reads as no value, a column of whole numbers with no declared
decimals as integers, date-time fields as ISO 8601 text). A double field becoming integer or a date-time field
becoming text produces a notice. The layer reads `inSync`; the project has a change to save. Undo doesn't change the file:
undoing an edit made before publishing shows the layer as `edited` again. A layer with no edits writes nothing. Layers read in place remain read-only.

#### GeoJSON

For `.geojson`, publish also checks that the source is unchanged, that it still reads as it did
at the last sync, and that both the original and the edited layer survive a read/write. It keeps
the document's kind (FeatureCollection, Feature or bare geometry), a collection's name, and
feature ids. Ordinary integer coordinate text stays integer text when the value is unchanged.
Whitespace and member order may change. A legacy `crs` member is kept, and the layer is written
back in the system it names (Swiss LV95 coordinates stay LV95); a layer whose coordinate system
was defined again since is refused.

Publish refuses a rewrite that would change foreign members, Z values,
a geometry the reader simplifies, nested properties it reads as text, duplicate object keys,
number text the writer would spell differently (such as `1.00`, `1E3`, or a decimal below
0.00001 such as `0.000004`, which it writes `4e-6`), a property that mixes numbers and text
(read as a text column, its numbers would be written as text), or a file that doesn't read whole
(a feature the reader skips). The message says why; the original file stays unchanged. A file with feature ids requires the same internal row
ids in the same order: adding, deleting or reordering rows is refused when their external ids
couldn't be preserved, and the same holds for a layer without fields whose features mix absent,
null and object `properties`. Absent or null `properties` are kept only in a layer without fields:
a collection that mixes them with attributed features is refused. A `bbox` on the collection
or its features is recomputed from the coordinates written. A feature keeps a box only where it had one, unless
rows were added, removed or reordered: then every feature gets one when any did. A feature whose
geometry was cleared is written without one, having nothing to bound. A collection `bbox` with no geometry
left, one that doesn't match the file's own coordinates, or a `bbox` on a geometry object, is
refused. Use Export Features for a
different document structure. A single Feature can't become a collection, a bare geometry can't
gain attributes, and an empty collection can't retain a field schema.

The new file is written beside the source with its existing permissions, synced and read back.
The source is checked again immediately before the atomic replacement; cancellation or failure
before replacement leaves it unchanged. The same OS lock as for Shapefile prevents concurrent
RustGIS publishers; its empty `.filename.geojson.rustgis-lock` sidecar stays for reuse. Other
programs do not follow this lock: changes they make after RustGIS's final source check can be
overwritten by the publish. Stop other writers before publishing. A field removed or renamed in
the layer produces a notice naming the properties left out. Afterward the
layer takes the file's read-back values and its new sync point, without changing the project
format. Undo changes the layer, and shows it as edited; it does not undo a write to the file.

#### Running a publish

On the desktop, make the layer active and choose **Publish to Source** in Command Search; it runs
in the background, with progress and Cancel in the status bar. CLI and Python: `source.publish`
with `{"layer":"parcels"}`.

## Keyboard shortcuts

| Keys | Action |
| --- | --- |
| ++ctrl+k++ | Command search |
| ++ctrl+n++ / ++ctrl+o++ / ++ctrl+s++ | New / Open / Save project |
| ++ctrl+shift+s++ | Save project as |
| ++ctrl+z++ | Undo |
| ++ctrl+y++ / ++ctrl+shift+z++ | Redo |
| ++ctrl+t++ | Attribute table |
| ++ctrl+f1++ | Collapse or expand the ribbon |
| ++ctrl+equal++ / ++ctrl+minus++ | Zoom in / out |
| ++home++ | Full extent |
| ++esc++ | Back to Explore; cancel a sketch |
| ++delete++ | Delete selected features; with Edit Vertices, the chosen vertex; in a layout, the selected element |
| ++ctrl+d++ | Duplicate the selected layout element |
| Arrow keys / ++shift++ + arrow keys | Nudge the selected layout element / nudge it further |
| ++alt+shift+right++ / ++alt+shift+left++ | Indent the active layer into the group above it / outdent it out of its group |
| ++alt+shift+up++ / ++alt+shift+down++ | Move the active layer up / down one step in the Layers pane |
| ++f2++ | Rename the active layer |
| ++ctrl+c++ / ++ctrl+x++ / ++ctrl+v++ | Copy / Cut / Paste features |

On macOS, ++cmd++ takes the place of ++ctrl++. **Help › Shortcuts** lists them all in the app,
from the same table the app runs them from. While you type in a text field, undo and redo take
back its typing, not the project's last change.
