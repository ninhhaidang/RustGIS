# User Guide

This guide walks through RustGIS by task. Each page covers one part of the app:

| Page | What it covers |
| --- | --- |
| [Data](data.md) | Vector and raster formats, adding data, large files, raster display, project items |
| [Web Data](web-data.md) | Public web services and catalogs: STAC, Overture Maps, OpenStreetMap, NASA, open-data portals, satellite embeddings |
| [Supported Data Formats](data-formats.md) | Every format RustGIS reads and writes, how to add it, and its limits |
| [Maps and Basemaps](maps.md) | Basemaps and web tile layers, projections, the 3D globe, view effects and navigation |
| [Cartography](cartography.md) | Symbology, labeling, layer effects and graphics |
| [Editing](editing.md) | Creating and modifying features, data design, the clipboard and pop-ups |
| [Analysis](analysis.md) | Selection and SQL queries, geoprocessing, the GeoLibre tools, guided workflows, imagery |
| [Layouts and Reports](layouts.md) | Page layouts, map series, reports, presentations and notebooks |
| [Sharing](sharing.md) | Exports, packages and open-format web maps, layers, styles and scenes |
| [Workspace](workspace.md) | Linked views, the Browser view, animation, profiles, offline areas, customizing the ribbon |
| [AI Assistant](assistant.md) | A chat pane that answers questions and acts on the map through commands |

<figure markdown="span">
  [![RustGIS with the Home tab of the ribbon open over a world map in the Equal Earth projection, countries shaded by population](https://assets.rustgis.app/images/hero.webp)](https://assets.rustgis.app/images/hero.webp){ target="_blank" rel="noopener" title="Open the full-size image" }
  <figcaption>RustGIS with the World Atlas sample in the Equal Earth projection.</figcaption>
</figure>

## The window

- **Ribbon**: tabs of commands grouped by task. The **Project** button opens the backstage (new,
  open, save, About).
- **Quick Access Toolbar** (top left): New, Open, Save, Undo and Redo, plus any commands you add
  under **Settings › Customize › Quick Access**.
- **Layers pane** (left): the map's layers in drawing order, with their legends. Tick a layer to
  show or hide it; drag it up or down to change the drawing order; right-click it to zoom to it,
  rename or remove it and more. Type in the box at the top to show only the layers and tables
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
  a group or take them out of one, or remove them (++delete++ does too); a layer the filter hides
  is left out. A new group made from the menu asks for its name. In scripts, `layer.select` takes
  a list and a `mode` (`replace`, `toggle` or `extend`), and the commands that remove, show, move,
  group or zoom to a layer take a list in `layer`, or the picked layers when it is left out.
  **Group layers** to keep related ones together, such as one dataset drawn as several layers
  with their own visibility ranges: right-click a layer and pick **Group › New Group**, or move
  it into a group that already exists. A group folds open and closed with its arrow; its box
  shows or hides all its layers at once (each keeps its own box), and its menu renames it, sets a
  transparency over its layers' own, ungroups it (its layers stay) or removes it with its layers.
  Drag a layer onto a group's row, or between its layers, to put it in the group; drag it out
  of the group's rows to take it out. Drag the group's row to move all its layers in the drawing
  order. Scripts and the AI Assistant build groups with `group.add`, `group.set`,
  `group.scaleRange` (a visibility range on top of each layer's own), `group.move`,
  `group.expand`, `group.ungroup`, `group.remove` and `layer.group`.
  **Copy Layer** (or **Copy Group**, with its layers) from its menu and **Paste Layer** (or
  **Paste Group**) it on a layer (above it), a group or the map's name above the list
  (at the top, also when the map has no layers yet), after switching to another map if you like,
  or on a map in the Browser (at the top). The copy keeps its source, symbology, labels,
  transparency, scale range, definition query, display filters and mask, but not its selection,
  and a name the map already has gets " copy". Pasting into the same map duplicates the layer. A
  mask whose layer the map lacks is cleared, and said. Groups don't nest: pasted into a group (on
  it, or on one of its layers), a copied group's layers join it at the top. Scripts use
  `layer.copy` and `layer.paste`.
  The Style and Data tabs act on the layer selected here. The tabs at the top of the left pane
  switch it between **Layers**, **Browser** and **Data Sources** (also in **View › Windows**).
- **Map view**: one tab per map and layout. The buttons at the top right zoom in, zoom out and go
  to the full extent; the north arrow and scale bar sit in the corners (on a rotated or tilted
  map, click the north arrow to face north and look straight down); tile credits at the bottom right.
- **Panes** (right): Tools (geoprocessing), Symbology, History, Details, Editing, Workflows,
  Script, the AI Assistant and more. Open them from **View › Windows** or the commands that use
  them. Each pane opened gets a tab at the top, in the order opened: click a tab to show its pane,
  and its **✕** to close it (the next tab shows; closing the last hides the side). Right-click a
  tab to close it, the others or all; the **✕** at the header's end closes every pane. Tabs that
  don't fit are listed under **»**. A ribbon button for a pane shows it when it is open but not in view,
  and closes it when it is. **Pane Sets** keep the tabs open.
- **Status bar**: the scale (type one, or pick from the list), the zoom level, bearing and pitch
  (click them to face north and look straight down again), the coordinate system (click it to
  change the projection), the pointer's coordinates (click them to switch between decimal
  degrees and DMS), the selection count and the draw time.

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

## The ribbon

| Tab | Groups |
| --- | --- |
| **Home** | Project (New Map, New Layout, Toolbox, Import, Add Folder, Templates), Navigate (Pan, Bookmarks, extents, Zoom In/Out, Go To XY, Identify), Add (Basemap, Add Data, [Web Data](web-data.md)), Selection, Inspect (Measure, Locate, Zoom Box, Infographics) |
| **Data** | Layer Templates, Definition Query, Table, Selection, Data Design, Archiving, Relationship, Export (for a raster layer: Information, Export), Offline, Feature Cache |
| **Style** | For the layer selected in Layers: visibility range, transparency, symbology, drawing effects, extrusion and labels (labels, label range, placement, label class, text symbol) for a vector layer; visibility range, transparency, swipe, raster effects, rendering and enhancement for a raster layer; Styles |
| **Edit** | Manage Edits (Edit, Save, Undo), Snapping, Features (Create, Modify, Delete), Tools, Elevation, Corrections, Graphics, Graphics and Text, Clipboard |
| **Analyze** | Geoprocessing (History, Tools, ModelBuilder, Script, SQL, Environments), Tasks (the Tasks pane; Task: Record Task, New Task From History, Import Task, run a task), AI (AI Assistant), Portal (Feature and Raster Analysis), Workflows, Raster (Raster Functions, Function Editor), the tool gallery, Selection, Notebook, the imagery groups (Ortho Mapping, Reality Mapping, Alignment, Image Analysis, Image Classification, Imagery Sharing, Mensuration, Imagery Tools) |
| **View** | Display, View (Globe, Projection, Global, Local), Effects, Scene, Windows, Labeling, Navigation, Link, Workspace, Animation, Time, Profile |
| **Publish** | While a layout is open, its groups first (Page, Map Frames, Map Surrounds, Text, Export); then Report, Presentation, Output (Export Map, Print, Capture), Package, Share As, Layout, Save As, Thumbnail |
| **Plugins** | The Plugin Manager, plus a tab for each [plugin](../plugins.md) that adds one |
| **Settings** | Customize (ribbon tabs, Quick Access Toolbar), Language (interface language and worldview), Start-up (start with the sample, a blank map, the last project or one you choose, projection, extent) |
| **Help** | Performance (Diagnostic Monitor), Learn, Help (About, Check for Updates on the desktop, Command Search, Shortcuts) |

The tabs stay the same whatever is selected: Style and Data act on the layer selected in Layers.

## Projects

A project (`.rgis`) holds maps, layouts, reports, presentations, notebooks, styles, tasks and
models. Vector data added from files is stored inside the project, so a project file is
self-contained; rasters are referenced by path on the desktop and stored inside the project in
the web app. **Project › New** starts a blank map or the World Atlas sample; **Save** and **Save
As** write the project, named after its file; a project opened from a file is named after that
file too. See [Project items](data.md#project-items) for the other files
RustGIS reads and writes.

Every change is a command, and every command can be undone (++ctrl+z++) and redone
(++ctrl+y++).

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
| ++delete++ | Delete selected features |
| ++ctrl+d++ | Duplicate the selected layout element |
| ++ctrl+c++ / ++ctrl+x++ / ++ctrl+v++ | Copy / Cut / Paste features |

On macOS, ++cmd++ takes the place of ++ctrl++. **Help › Shortcuts** lists them all in the app,
from the same table the app runs them from. While you type in a text field, undo and redo take
back its typing, not the project's last change.
