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
| [AI Assistant](assistant.md) | A chat pane that answers questions and acts on the map through commands |

<figure markdown="span">
  [![RustGIS with the Home tab of the ribbon open over a world map in the Equal Earth projection, countries shaded by population](https://assets.rustgis.app/images/hero.webp)](https://assets.rustgis.app/images/hero.webp){ target="_blank" rel="noopener" title="Open the full-size image" }
  <figcaption>RustGIS with the World Atlas sample in the Equal Earth projection.</figcaption>
</figure>

## The window

- **Ribbon**: tabs of commands grouped by task. The **Project** button opens the backstage
  (Start, New, Open, Save, Save As and About).
- **Quick Access Toolbar** (top left): New, Open, Save, Undo and Redo, plus any commands you add
  under **Settings › Customize › Quick Access**.
- **Layers pane** (left): the map's layers in drawing order, with their legends. A new layer goes
  on top, whatever its kind (a feature, raster or web layer, an analysis result). Tick a layer to
  show or hide it; drag it up or down to change the drawing order; right-click it to zoom to it,
  rename or remove it and more. The entire layer block, including its expanded legend and
  classes, is split 50/50: drop in its upper half to insert above, or its lower half to insert
  below. The insertion line shows the destination. Drop inside the first block's upper half
  to move to the top, or inside the final block's lower half to move to the bottom.
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
  a group or take them out of one, or remove them; a layer the filter hides is left out.
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
- **Panes** (right): Tools (geoprocessing), Symbology, Labeling, History, Details, Bookmarks,
  Editing, Workflows, Script, SQL, the AI Assistant and more. Open them from **View › Windows** or the commands that use
  them. Each pane opened gets a tab at the top, in the order opened: click a tab to show its pane,
  and its **✕** to close it (the next tab shows; closing the last hides the side). Right-click a
  tab to close it, the others or all; the **✕** at the header's end closes every pane. Tabs that
  don't fit are listed under **»**. A ribbon button for a pane shows it when it is open but not in view,
  and closes it when it is. **Pane Sets** keep the tabs open.
- **Status bar**: the scale (type one, or pick from the list), the zoom level, bearing and pitch
  (click them to face north and look straight down again), the pointer's coordinates (click them
  to switch between decimal degrees and DMS), the coordinate system (click it to change the
  projection), the selection count (with a button to clear it), the latest notice (click to open
  **Diagnostics**), a **Diagnostics** button, the last draw time, the frame rate and a **GPU**/**CPU**
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

### Diagnostics

Open **View › Windows › Diagnostics**, click the status-bar notice or **Diagnostics** button,
or search for **Diagnostics** in Command Search (`ui.pane.diagnosticLog`). This troubleshooting
history is separate from **Help › Diagnostic Monitor**, which still shows performance metrics.

Messages are collected even while the pane is closed. The newest entries appear first, with
a UTC timestamp, severity, and source (such as the command id or Network). Expand **Technical
details** for failed requests: their address, method, observed HTTP status and duration when
available. Native connection failures and timeouts are identified when the transport knows
them; an opaque browser failure does not claim a status or a specific DNS, CORS, or connection cause.

Use **All**, **Errors**, **Warnings**, **Info**, or **Network failures** to filter the history.
**Copy diagnostics** copies the filtered entries and their technical details, including folded
details. **Clear** clears all entries, not just the selected filter. Closing or reopening the
pane does not clear it.

The last 500 entries are kept in memory for this session, including across project changes;
older entries are discarded. Each message is limited to 2 KiB and technical details to 8 KiB.
Nothing is saved in `.rgis` projects or sent automatically. Credentials are removed before
retention: addresses omit user information, query strings and fragments; credential fields
and structured payloads are omitted conservatively, which can also remove surrounding context.
Review copied diagnostics for private layer names or file paths before sharing them.


## The ribbon

| Tab | Groups |
| --- | --- |
| **Home** | Project (New Map, New Layout), Navigate (Pan, Bookmarks, Full Extent, Zoom In/Out, Previous/Next, Zoom Box, Planar/First-Person, Identify, Locate [By Coordinates, By Place or Address], Directions), Add (Basemap, Add Data, Data Sources, [Web Data](web-data.md)), Selection (Select, Select By Attributes, Select By Location, Clear, Zoom To, Attributes), Inspect (Measure, Infographics) |
| **Data** | Layer Templates, Data Source (Repair Source); for a feature layer: Definition Query, Table, Selection, Data Design, Archiving, Relationship and Export; for a raster layer: Information and Export; then Offline and Feature Cache |
| **Style** | For the layer selected in Layers. A feature layer: Visibility Range, Effects, Drawing, Drawing Effects, Extrusion for polygons (with Faces and Light and Shadow once on) and Layer. A raster layer: Visibility Range, Effects, Compare, Raster Effects, Rendering, Enhancement and Layer. Both: Styles |
| **Labeling** | For the feature layer selected in Layers: Labels (Label, Properties), Label Class (Class, Field), Text Symbol (Size, Color, Bold, Halo), Label Placement and Label Range. Whatever is selected: Map Labeling (Lock, Pause, View Unplaced, Convert) |
| **Edit** | Manage Edits (Edit, Save, Undo, Redo, Status, Save to PostGIS and Discard PostGIS Edits on the desktop while a [PostGIS layer is loaded for editing](data.md#editing-postgis-tables), Settings, Templates, Topology), Snapping, Features (Create, Delete, Edit Vertices, Modify), Tools, Elevation, Corrections, Graphics, Graphics and Text, Clipboard |
| **Analyze** | Geoprocessing (History, Tools, Model Builder, Script, SQL, Environments), Tasks (the Tasks pane; Task: Record Task, New Task From History, Import Task, run a task), AI (AI Assistant), Portal (Feature Analysis, Raster Analysis), Workflows, Raster (Raster Functions, Function Editor), Tools (the tool gallery), Selection, Notebook, the imagery groups (Ortho Mapping, Reality Mapping, Alignment, Image Analysis, Image Classification, Imagery Sharing, Mensuration, Imagery Tools) |
| **View** | Display (Graticule, Dark Mode, GPU Rendering), View (Globe, Projection, Global; Local on the desktop), Effects, Scene, Windows, Navigation, Link, Workspace, Animation, Time (Time Slider), Profile (Profile; Time Series on the desktop) |
| **Publish** | While a layout is open, its groups first (Page, Map Frames, Map Surrounds, Shapes, Text, Export); then Report, Presentation, Output (Export Map, Export Features, Print Map, Capture To Clipboard, Report, Presentation, Export To 3D), Package, Share As, Layout, Save As, Thumbnail |
| **Plugins** | Plugins (Plugin Manager, Install From File, Browse Plugins), plus a group for each enabled [plugin](../plugins.md) with commands or tools; a plugin with its own tab adds that tab instead |
| **Settings** | Customize (Ribbon, Quick Access), Language (interface language and worldview), Start-up (Start With, Projection, Extent), Autosave on the desktop |
| **Help** | Performance (Diagnostic Monitor), Learn (Learning Resources), Help (About, Check for Updates on the desktop, Command Search, Shortcuts) |

The tabs stay the same whatever is selected: Style, Labeling and Data act on the layer selected in Layers.

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
The desktop app also keeps a copy of unsaved work and offers it back after a crash: see
[Autosave and recovery](workspace.md#autosave-and-recovery).
Exports and other files already written are not undone.

### Repair Data Source

If a linked file moved or was renamed, right-click its layer or local PMTiles/MBTiles tile
archive and choose **Repair Data Source…**. For the selected layer, use **Data › Data Source ›
Repair Source**, or find **Repair Data Source** (or **Repair Tile Archive**) in Command Search.
Choose the replacement file or folder. For a container such as a GeoPackage, **List Datasets**
reads its tables in the background; choose a table when there is more than one. The existing
table is retained when available. Repair keeps the layer's identity and configuration, marks
the project unsaved and can be undone or redone. A raster drawn with its own color table
requires a replacement with a color table; incompatible rasters are rejected without changing
the saved style.
If a tile archive is removed or reordered out of its captured slot while the dialog is open,
the dialog closes rather than applying the replacement to another archive.
It does not change embedded data, databases or web links. The web app cannot link to files
on your computer: add the file again with **Add Data**.

CLI and Python use the same commands: `layer.repairSource` with
`{"layer":"Cities","path":"/data/towns.fgb"}` (and optional `"table"`), or `map.repairTiles`
with `{"index":0,"path":"/data/tiles.pmtiles"}`. Tile archives can also be selected by
`"name"` or catalog `"id"`.

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
| ++ctrl+c++ / ++ctrl+x++ / ++ctrl+v++ | Copy / Cut / Paste features |

On macOS, ++cmd++ takes the place of ++ctrl++. **Help › Shortcuts** lists them all in the app,
from the same table the app runs them from. While you type in a text field, undo and redo take
back its typing, not the project's last change.
