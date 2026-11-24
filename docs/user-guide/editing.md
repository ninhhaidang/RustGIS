# Editing

## Creating features

**Edit › Create** draws new points, lines and polygons into the layer selected in Layers:
click once for a point, or click each vertex and double-click to finish a line or polygon
(++esc++ cancels). **Delete** (++delete++) removes the selected features across the map's layers.
Edit attributes in the attribute table (++ctrl+t++): double-click a cell
to change it. **Data › Data Design** (and **Data › Fields**) adds and deletes fields and calculates a
field from an expression; **Calculate Geometry** (in the attribute table's toolbar) fills a new or
existing numeric field with each feature's area, perimeter, length or X/Y coordinate, in the units
you choose, for all features or the selected ones. Length and perimeter include feet and miles;
area includes square feet (`ft2`), acres (`ac`) and square miles (`mi2`). An unrecognized
`edit.calculateGeometry` unit is refused with the valid unit keys for the chosen property. A feature
with a place that can't be given in longitude/latitude (one far past where its layer's own system
reaches) is left empty, and a notice counts them.
Every edit is one undo step. Areas, perimeters and lengths measure on the WGS 84 ellipsoid, the same measure as the network tools and the Measure panel (Spatial Join still
measures on the sphere). A ring with a full-turn edge, -180° to 180° along a parallel that the
ring reaches down one meridian and leaves back up the other (a globe-wide band, or a cap closed
along the pole), counts that whole parallel, as long as the ring's turns leave it closed around
the globe; an edge from 180° to -180° anywhere else is a seam along the antimeridian (a polygon
split there), so a 1° cell written that way is the cell. A ring winding around a pole is measured as the region it
traces, where [Geoprocessing](analysis.md#geoprocessing-tools) gives the smaller polar cap.

Undo (++ctrl+z++) and Redo (++ctrl+y++ or ++ctrl+shift+z++) restore project changes, not just
feature edits; use ++cmd++ instead of ++ctrl++ on macOS. While typing in a text field, those
shortcuts undo or redo the typing instead. Saving keeps the undo history; opening or creating
a project clears it. Undo does not reverse files already written by an export.

In the desktop and browser toolbar, open **Undo History** next to Undo and Redo to see the
current project state and the available steps. Select a row to go directly to the state after
that change, or **Earliest available state** to undo every retained step. You can select a newer
row to redo those changes. On a narrow window, Undo History may be under **More Quick Access
commands**. For longer jumps, **Cancel** in the status bar stops at the state reached so far.
Making a new edit after going back discards the redo steps.

## Which parcel area to report

**Calculate Geometry** and `area('ac')` measure the polygon on the WGS 84 ellipsoid, regardless
of the display projection, with holes subtracted. Acres use 4,046.8564224 square metres per acre.
This is a measurement of the mapped boundary, not a deed acreage or a terrain-surface area.

A State Plane area is measured on that projection's flat grid, so its scale distortion and units
(often US survey square feet rather than international ones) make it differ slightly: on Shelby
County, Tennessee parcels, by about 0.01% from the same boundaries in EPSG:2274.

A stored `Shape__Area` field is the area when the file was made, in that file's units; it isn't
updated when a boundary is edited, so check both before comparing. A deed acreage describes the
recorded parcel, which can differ from the mapped boundary.

## Modifying features

<figure markdown="span">
  ![Edit Vertices on Lake Victoria: square vertex handles and round midpoint handles around the selected lake](https://assets.rustgis.app/images/editing.webp)
  <figcaption>Edit Vertices: drag a square handle to move a vertex, a round one to add a vertex.</figcaption>
</figure>

The Edit tab's **Edit** button turns feature editing on or off for the map; with it off,
feature creation, geometry changes and attribute edits are disabled. The Editing pane
(Settings, Templates, Status, Modify) holds the rest. Its **Distance units**
selector sets the units used for Move and constant elevation. The choice is remembered independently
of the live map's measurement units. The entered numbers are interpreted in the selected units when
you click **Move**, **Constant** or **Apply elevation**. The current elevation stays in effect while
you edit the value or choose units; click **Apply elevation** to apply the new inputs.

- **Modify**: move the selected features by distances east and north, rotate them, scale them,
  merge them (polygons are unioned), explode multipart features, split a line where you click,
  and flip line direction. A single point turns and scales on itself, so Rotate and Scale leave
  points (text graphics included) as they are: with only points selected they change nothing,
  say so, and add no undo step. Edit › Tools lists every editing tool.
  In a layer held in a projected system (UTM, a national grid; not Web Mercator, another world
  projection or a geographic system), Move, Rotate and Scale work in the layer's own coordinates,
  as the analysis tools do: Move steps the distances east and north along its grid, measured flat
  in its unit (so in UTM a kilometre on the grid is between about 0.999 and 1.0004 km on the
  ground, and its north is grid north; Ground to grid below corrects ground distances and
  directions to it), following the system's axes when they point west, south or north first; Rotate turns clockwise
  with grid north up, and Scale scales, about the middle of the feature's extent there. A Move
  whose selection spans layers in more than one system (or some in one and some in
  longitude/latitude; graphics don't count) moves them all as below, so places they share stay
  together, and says so. The rest of this paragraph, about Web Mercator and the ground, is for
  the other layers.
  A selected feature with a place that can't be given in longitude/latitude (one far past where
  its layer's own system reaches) is left as it is by Move, Rotate and Scale (except in a layer
  edited in its own grid, where it changes with the rest), Split and Cut Polygon,
  and a notice counts them by layer; Flip Line flips it in the layer's own coordinates. An edit
  of that one feature (a vertex, Reshape, Trace, Merge with it, a shape conversion) is refused,
  naming the layer and the feature; Set Geometry gives it a new shape, and Delete removes it.
  Beyond 85.05°, where Web Mercator stops, Move, Rotate and Scale work on the ground: a feature
  that reaches beyond it, or that the edit would take beyond it, moves rigidly, together with the
  selected features sharing a vertex with it (their center goes the distance and direction asked,
  along the ground and across a pole if that's the way, and the rest turn with the world around
  it), and turns or scales about its own center. Within ±85.05° an edit is made in Web Mercator, as
  before (where a long move north covers less ground than asked, and Rotate and Scale turn about
  the middle of the feature's extent), so an edit that just stays within 85.05° and one that just
  passes it can land far apart. Only the vertices move rigidly: edges stay straight in longitude
  and latitude between them, so long edges bend, the more so the nearer the pole one end is than
  the other, and far less for moves north–south than east–west: an edge from 87° to 89° bends
  333 m for each kilometer moved east; one from 89.5° to 89.99° bends much more. Add vertices along them first to keep their shape. A moved
  feature's longitudes run on from where they were written, past ±180° if it crosses there (to
  188°, say, rather than −172°), as within ±85.05°; one carried more than halfway around a pole
  is written within half a turn of where it was.
  Rotate and Scale use each feature's own center, so selected neighbors sharing a corner can
  separate at that corner.
  Right by a pole a move's directions turn quickly with its center's place: a center at
  the pole takes "east" from the prime meridian (toward 90°E), one a meter away from its own
  longitude. The whole edit is refused, and nothing changes, when a selected line or polygon
  edited on the ground reaches a pole (so moving every feature of a world layer fails while
  Antarctica is selected), spans 180° of longitude or more in one edge, or would be carried onto
  or around a pole, or when the edit would move a feature more than 10,000 km on the ground or
  carry a vertex more than 10,000 km from the center it's edited about. A move of no distance
  leaves features beyond 85.05° as they are. Moving neighbors one at a time, one within ±85.05°
  and one beyond, can leave their shared corners apart, by about the distance moved times
  2·sin(Δλ/2), Δλ being the corner's longitude from the center of the one moved on the ground (a
  tenth of it 6° away, half of it 30° away): select them together. Split cuts beyond 85.05° where
  it's asked: where the point or the line reaches beyond it, at the line's point nearest on the
  ground (once a split has worked out a million distances along segments near the point, a
  segment counts as near as the nearest point found on it so far, its nearer end at first, and the
  split says how many lines it cut that way, or left whole when that point was an end).
- **Cut Polygon** (Edit › Tools, or the Editing pane): select polygons, then click a line across
  them and double-click to finish. Each polygon the line crosses from edge to edge is cut into
  pieces; a zigzag line can make several. The pieces keep the original's attributes. The
  original row keeps the largest piece, together with any parts of a multipart polygon that the
  line missed; every other piece becomes a feature of its own, and the pieces end up selected.
  Holes the line misses stay in the piece around them. A line that stops inside a polygon cuts
  nothing there; a line end within a centimetre of an edge counts as on it. When the cut
  succeeds, a notice says how many selected features the line didn't cut (`missed` in the
  result). A line that cuts none of them gives an error instead.
  `edit.cutPolygon={"line": {"type": "LineString", "coordinates": [[0.3, -1], [0.3, 2]]}}`.
- **Reshape** (Edit › Tools, or the Editing pane): select one line or polygon, then draw a line
  that leaves its edge and comes back to it. For a polygon, the stretch of boundary between the
  line's first and last crossings is replaced by the line, keeping whichever of the two possible
  shapes is larger. A hole left wholly outside the new boundary goes with the part reshaped away,
  and a notice says how many (`holesRemoved` in the result). A reshape that would make the
  boundary cross itself, or a hole cross its outer ring, is refused. For a line, the stretch between two crossings is replaced. A drawn
  line that crosses only once replaces the shorter end, so start on the line and draw out to
  extend or redirect an end (`edit.reshape={"line": …}`).
- **Trace** (Edit › Features): while drawing a feature, a cut or a reshape, click a point on a
  line or polygon edge, then another point on the same edge. The sketch follows the edge between
  them, going the shorter way round a ring; click a point partway to go the other way. Turn on
  snapping so clicks land exactly on the edge. From scripts,
  `edit.trace={"from": [lon, lat], "to": [lon, lat]}` returns the path along the nearest edge
  passing within the snapping tolerance of both points (`"layer"` keeps the search to one layer;
  add `"row"` to trace a given feature), and how far each point was from the edge (`fromOffM`, `toOffM`, in Web Mercator metres, which
  run longer than ground metres away from the equator).
- **Edit Vertices** (Edit › Features): reshape the selected features on the map. Their vertices
  show as square handles and each edge has a round midpoint handle. Drag a vertex to move it;
  drag a midpoint, or click an edge, to insert a vertex; click a vertex and press ++delete++ to
  remove it (a line keeps at least two vertices, a polygon ring three). A selected feature with
  a place that can't be given in longitude/latitude (see [Places with no longitude/latitude](data-formats.md#places-with-no-longitudelatitude))
  gets no handles, and a notice says so once, naming it or counting them. Clicking elsewhere
  selects the feature there (++shift++ adds it). Multipart features and polygon holes work the
  same way, dragged vertices snap when snapping is on, and each move, insertion or deletion is
  one undo step. ++esc++ finishes. From scripts and the command line, `edit.vertex` does the
  same to one vertex and `edit.setGeometry` (Set Geometry) replaces a feature's whole shape.
- **Feature templates**: named default values for new features, made from a selected feature;
  the Create tool uses the chosen template.
- **Snapping** (**Edit › Snapping**): new and dragged vertices snap to nearby vertices, edges and
  line ends of visible feature layers at the current scale (magenta marker; graphics and rasters
  are not targets). Vertices and ends take priority over edges within the tolerance. Snapping
  starts off; its vertex, edge and end options start on. Both Point features and individual
  MultiPoint members use the vertex option; they supply neither edges nor line ends. Dragging
  a Point or MultiPoint member excludes its entire source feature. The tolerance is how near, in screen
  pixels, a vertex must come to snap (10 by default, 1 to 100):
  `edit.snapping={"on": true, "tolerancePx": 12, "edge": false}` (without `"on"` it toggles
  snapping, even when other options are supplied). `edit.snap={"lon":12.5,"lat":41.9}` queries
  the current snap target without editing a feature.
- **Settings**: **Show Vertices** marks the vertices of the selected features even when Edit
  Vertices isn't on (`edit.settings={"showVertices": true}`).
- **Elevation**: new features get a `z` attribute, constant or sampled from a raster surface at
  the feature's label point, as the layer holds it, placed on the surface by the transformation
  between the two systems; this is not a change to its geometry's coordinates. An edit whose
  label point the surface's system can't give is refused.
- **Ground to grid**: a scale factor and rotation applied to entered distances and directions
  (Move, and `edit.traverse`, which draws a line or polygon from distances and bearings). Into a
  layer held in a projected system, a traverse starts at the given longitude/latitude converted
  into that system and steps each leg on its grid, flat in its unit from grid north, as Move does;
  into any other layer, each leg goes east and north in Web Mercator stretched to the latitude.
- **Topology**: rules (must not overlap, must not have gaps, must not self-intersect, must not
  have dangles, must be covered by); Validate lists the errors, and clicking one zooms to it.
  Primary rows follow the layer's definition query, independently of selection, time windows
  and display filters. For **must be covered by**, every resident row of the covering layer
  supplies the reference coverage; its map filters do not narrow that reference set.
  A layer held in a projected system is checked in its own coordinates, as the analysis tools
  work in it. The dangle tolerance (`toleranceM`, 1 m by default) is measured flat there in its
  unit, an overlap or gap counts from a square metre of it, and a covering layer is converted
  into it, with a notice when its long edges, taken straight there, run more than a metre from
  where a check in longitude/latitude takes them. A rule whose covering layer that system can't
  hold is checked in longitude/latitude instead, and says so. A layer in Web Mercator, another
  world projection or a geographic system is checked in longitude/latitude, its dangle tolerance
  on the ground; there, a feature with a place that can't be given in longitude/latitude (one
  far past where its layer's own system reaches) is left out of the check, and a notice names
  the rules and counts them. An error at a place that can't be given in longitude/latitude is
  listed with its features but has no place for the Error Inspector to zoom to (in
  `topology.validate`'s answer, `located` is false and `lon` and `lat` are null), and a notice
  counts them. A rule that left a feature out is said to be checked incompletely, since its
  neighbours' issues there (a gap where it lay, a dangle where it joined) may not be real.
  On the desktop the check runs in the background, so the map stays in use: the status bar
  shows **Error Inspector** with its progress and **Cancel**, and a check that can be cancelled
  takes any amount of work (a gaps rule first joins all its polygons in one step, which
  **Cancel** stops only once it is done). A layer edited while it is checked makes the check say
  so: run Validate again. Headless script, assistant and command-line calls check on their
  caller's thread; when they cannot be cancelled, they stop after a fixed amount of work,
  saying which rules weren't checked completely (a gaps rule's join has no such limit).
  A desktop window requires the background queue instead of running the check inline. A line or
  ring too complex to check for self-intersections within the limit is listed at its first point,
  to be looked at, and its rule is said to be incomplete.
  Beyond 85.05° they follow the geometry tools (see
  [Geoprocessing tools](analysis.md#geoprocessing-tools), **Near the poles**).
- **Status**: features added, changed and deleted per layer since the last save.

### Annotating an image

**Analyze → Annotate** opens the annotation pane. It is also available from **View → Windows**
and Command Search. Add a GeoTIFF and a polygon feature layer first, then choose the **Image**
and **Target layer** in the pane, enable editing, and choose Circle or Rotated Box.
The ordinary tools remain available under Edit and share these settings.

Selecting an image defaults the **New shape grid** to **Original image pixels**. RustGIS reads
its original TIFF header, including rotation and shear, independently of any displayed overview.
New box widths/heights and circle radii are then expressed in source-image pixels. A circle in
image coordinates may appear elliptical on the map; a rectangle may appear skewed. The image is
not resampled by this choice, and shapes are not clipped to its boundary.

Choose **Web Mercator grid metres** explicitly to draw in map coordinates instead; those are not
ground-distance metres. Choosing **No image** restores that grid. Under Edit, the same choices
are in Editing → New shape grid when a circle or rotated box tool is active. Grid changes affect
new shapes; existing shapes retain their own construction grid when moved, resized or rotated.
Undo/Redo retain those constructions within the session.

On desktop, header reads run in the background with progress and Cancel. Changing images,
switching to Web Mercator or using **Cancel Grid Read** stops only that lookup and preserves
unrelated queued work. The status-bar Cancel retains its queue-wide behaviour. Pixel-based
drawing waits for a valid grid; a failed or cancelled read never silently substitutes map metres.
After changing a source file externally, reload the image layer first, then use **Reload Grid**.
Original-pixel drawing and conversion share the same [source checks](#image-source-checks).
Removing/replacing an image invalidates its cached grid. Local and embedded GeoTIFFs are supported;
download remote originals first. In the browser, upload the original GeoTIFF. Other local formats
(including PNG, JPEG, ASCII grids and GDAL-converted images) are refused with guidance to export
the image as a GeoTIFF and add that file; reloading the unsupported source cannot supply its grid.
Construction grids require a supported EPSG coordinate system. Unsupported or custom systems
are refused during the grid read; choose a supported image or **Web Mercator grid metres**.
Changing projects or maps clears the old image binding, keeps pixel mode selected and reports a
warning. Drawing stays blocked until you choose an image again or explicitly choose Web Mercator
grid metres; opening the pane does not clear the warning.
An image removed from the map keeps its last name as **Unavailable image**. Pixel drawing stays
blocked and **Reload Grid** is disabled until the image is restored or another image is chosen.
Choose **No image** to return explicitly to Web Mercator grid metres.
**Original Image Grid** in Command Search reports the active image's original dimensions and
EPSG coordinate system in a notice, while keeping the Annotate pane open.

Space or middle-drag pans; releasing Space keeps the drawing tool. Use the selected object's
existing handles, Undo/Redo, and **Export GeoJSON…**. **Shape details** is collapsed by default;
expand it for numeric dimensions and additional editing controls. Map handles remain available
while the section is hidden. Refused drawing gestures show an error notification even when
**Shape details** is collapsed. The reason stays visible in the pane, and repeated refusals with
the same message do not add duplicate notices. Changing the image/grid or invalidating its source
cancels an unfinished new shape with an explanation; releasing afterwards does not save that
preview or create an Undo step. Exports contain
ordinary geographic polygons and feature properties; circles are polygon approximations.
Check **Use image filename** to suggest `img1.geojson` for an image named `img1.tiff`.
The preview shows the suggested basename; the save dialog still chooses the folder and lets
you change the name. The option starts unchecked and applies only to this pane’s GeoJSON
export; ordinary layer exports keep their layer-based names.

Image selection, grid choices and exact constructions are session-only. Reopened polygons can
be explicitly converted again. No image-binding metadata or palette sidecars are added.

For CLI, Python, Assistant/MCP and permitted plugins, `raster.imageGrid={"layer":"Image"}`
returns the original `epsg`, `width`, `height` and six affine coefficients in the same format
accepted by `edit.addBox.box.frame` and `edit.addCircle.circle.frame`. The query is read-only,
requires Files permission, and uses the shared read worker on desktop/MCP. Headless synchronous
callers must run file reads off their own UI thread. The desktop Script pane prepares grid queries
on its worker and shows the actual frame in each line's result, rather than a queue acknowledgement.
Queries may follow replayable selection/edit steps or query embedded rasters produced by
`gp.run` earlier in the same script. Accepted tool outputs retain their prepared raster data;
repeated queries and tools with multiple raster outputs use the matching output's grid without
rereading headers on the UI thread. Live output IDs and display styles are preserved. If a tool's
inputs change while the script is being prepared, all dependent grid results are discarded and
the script must be run again. File-opening or project-changing setup must run in a separate
script first. Scoped callers should use the individually approved query.

### Image source checks

Local GeoTIFF loading records the file revision with its decoded pixels. If a verified file changes
or becomes unverifiable during decoding, **Add Data** and **Reload Data** refuse that load rather
than accepting potentially inconsistent pixels. Wait for writes/cloud synchronization to finish,
then retry **Add Data**, or **Reload Data** for an existing layer. Or copy the image to a stable
local location: for a new layer, add the copy with **Add Data**; for an existing layer, point it at
the copy with **Repair Data Source** (**Reload Data** reads only the layer's current source, so it
does not pick up the copy). This load-time check also applies outside annotation workflows.

Original image grids are not offered for an image opened as a scan or laid over the view, which is
drawn where it was placed rather than where its file puts it, nor for an image read in place a view
at a time; choose map metres, or add the georeferenced GeoTIFF (clipped, if it is too large to open
whole).

Grid reads require loaded pixels and a matching verified revision. After known external edits,
reload the image layer and retry the operation. Detection compares metadata, not content hashes:
length and modification time, plus device/inode/change time on Unix or creation time on Windows.
Rewrites preserving every compared field go undetected, so reload after known edits even without
a warning. A cached grid remains aligned with the pixels retained in the session.

If the initial revision capture fails, viewing can still load, but grid reads report the original
metadata error. Check filesystem metadata support/access, or export the image as a GeoTIFF to
another location and add it; repeated reloads cannot repair unsupported metadata.

### Rotated boxes

Choose a loaded, visible polygon layer, turn **Edit › Edit** on, and select **Edit › Features ›
Rotated Box**. The existing **Editing** pane opens with precise controls for the selected box.
These are ordinary polygon features: the layer's fields, feature templates, symbology and edit
rules continue to apply. Box tools do not create annotation or class fields.

- Drag to create a box. Drag inside one to move it, or drag a corner or side handle to resize it.
  The opposite corner or side stays fixed. Hold ++shift++ to draw a square or preserve the
  aspect ratio while resizing a corner.
- Drag the round paddle outside the top edge to rotate; ++shift++ snaps rotation to 15°.
  Width, height and angle are regenerated from the construction, avoiding accumulated vertex
  rotation error. The pane also edits centre X/Y, width, height and angle numerically.
- Click a box to select it. ++alt++-click cycles overlapping features; ++shift++ adds to the
  selection. ++ctrl++/++cmd++-drag starts a new box even over an existing box.
- Hold ++space++ and drag, or drag with the middle mouse button, to pan. Releasing ++space++
  keeps Rotated Box active so the next click selects an object. The wheel still zooms.
- Right-click a box for **Edit Box Dimensions** or **Delete Selected Box**. The delete action
  removes only that object; the ordinary ++delete++ shortcut removes the selection. Each
  completed move, resize, rotation or deletion is undoable. ++esc++ cancels a held gesture;
  press it again to leave the tool.

The interactive tool starts in Web Mercator grid metres, **not ground metres**; angles run
clockwise from grid east. Activating the interactive tool, including through `tool.set`, requires
an untilted, non-globe Earth view. The geometry commands (`edit.addBox`, `edit.updateBox` and
`edit.deleteBox`) work independently of the map's projection and pitch, using the construction
grid on an Earth map. Boxes crossing the antimeridian, the polar editing limit, and unsupported
projection or datum transformations are refused. The grid is frozen for an existing box.
Scripts can supply an original image grid
from `raster::geotiff::read_image_grid` through `rustgis_engine::box_edit::Frame::from_grid`;
then the box dimensions and centre are original image pixels, including for rotated/skewed
GeoTIFFs. This does not bind a layer to an image or save image metadata.

Construction is kept only in the current session. An independent vertex edit disables the
box handles rather than silently fitting a new box; undo restores them. Saving/reopening or
sharing GeoJSON leaves **ordinary polygons** and ordinary feature properties. In-session edits
preserve feature identity; import uses the normal GeoJSON reader. Copy/paste transfers ordinary polygon geometry and attributes, not construction parameters;
the original keeps its constrained handles. There is no automatic recognition of imported polygons. Use **Edit Vertices** to edit those polygons.
**Export Features** (`share.exportLayer`) includes one counted note explaining that box
construction does not round-trip. Explicit project saves/downloads, map/layer files, project
and map packages, data conversions and feature-bearing web shares report the same loss.
Source polygons contain four transformed corners joined by straight edges; curved grid edges
are approximated. The target format may further approximate or omit geometry: CSV writes
representative points and GPX does not retain polygon areas, with their own format-specific notes.

Commands use the same editing and undo paths. The follow-up examples assume `edit.addBox`
returned `{"row":0,...}`; replace `0` with the returned row when the layer already has features.
Pass that feature's ID as `featureId` when updating or deleting after other edits:

```text
edit.addBox={"layer":"Training polygons","box":{"center":[1000,1000],"width":200,"height":100,"angle":30},"attributes":{"class":"vehicle"}}
edit.updateBox={"layer":"Training polygons","row":0,"box":{"center":[1000,1000],"width":240,"height":100,"angle":45}}
edit.boxInfo={"layer":"Training polygons","row":0}
edit.deleteBox={"layer":"Training polygons","row":0}
```

`attributes` names must already exist in the layer. `edit.updateBox` and `edit.deleteBox` also
accept `featureId` to refuse a stale row. `edit.boxInfo` returns the current construction;
updates keep its frame when `box.frame` is omitted, and preserve the feature's identity/attributes.
`edit.updateBox` changes only the shape and rejects an `attributes` parameter; use `edit.setValue`
to change attribute values.
An optional `box.frame` holds `epsg`, original `width`/`height` and the full GDAL-order `affine` array. Only recognised EPSG
frames with supported datum transformations are accepted. These parameters are command input,
not GeoJSON properties or a new saved-file representation.

### Circles

On a loaded polygon layer, turn **Edit › Edit** on and choose **Edit › Features › Circle**.
This creates ordinary feature polygons, independently of the annotation workflow or graphics.
The existing **Editing** pane opens with centre X/Y, radius and polygon tolerance controls.

- Drag from the centre to the edge to create a circle. Drag inside an existing circle to select and move
  it, or drag its round radius handle to resize with the centre fixed.
- Click to select, ++alt++-click to cycle overlaps, ++shift++-click to extend the selection.
  ++ctrl++/++cmd++-drag forces a new circle over existing features.
- Hold ++space++ and drag, or middle-drag, to pan; releasing ++space++ keeps Circle active.
  Right-click for **Edit Circle Dimensions** or **Delete Selected Circle** (just that object).
- Each completed gesture is one undo step. ++esc++ cancels the preview before another press
  leaves the tool. Templates, ordinary attributes and edit rules work as for other features.

The interactive tool uses the same flat Earth view and frozen construction grids as Rotated
Box. Its default radius is in **Web Mercator grid metres, not ground metres**. Scripts may
supply `circle.frame` from an original GeoTIFF grid via `box_edit::Frame::from_grid`; centre,
radius and tolerance then use original image pixels, including rotated or skewed grids.
A pixel circle may look elliptical on the map. Geometry commands remain view-independent.
Antimeridian crossings, polar editing limits and unsupported transformations are refused.

The exact centre/radius are session-only. Geometry is regenerated with 32–2048 straight edges;
`tolerance` (command default **0.25 grid units**) bounds the local-circle sagitta **before projection**,
not error in geographic coordinates or ground distance. New mouse-drawn circles choose the
larger of 0.25 grid units and radius/10000; this value is shown in the pane and preserved by
moves and resizes. Requests needing more than 2048 edges
are refused; increase tolerance or reduce radius. Commands report the edge count and approximation.
Changing a radius does not repeatedly rescale old vertices.

Exported GeoJSON holds only ordinary polygons and feature properties. **Export Features**
reports a counted circle-construction/approximation note, separately from any box or format
losses. Explicit project saves/downloads, map/layer files, project and map packages, data
conversions and feature-bearing web shares report this loss too. Reopening a project or
importing GeoJSON leaves polygons; use **Edit Vertices** for those. There is no automatic
circle detection, image binding or custom metadata. Independent geometry edits disable the
circle handles; undo restores them when the original geometry returns. Copy/paste produces
ordinary polygons in the target; the original retains its circle construction.

```text
edit.addCircle={"layer":"Training polygons","circle":{"center":[1000,1000],"radius":100,"tolerance":0.25},"attributes":{"class":"tree"}}
edit.updateCircle={"layer":"Training polygons","row":0,"circle":{"center":[1010,1000],"radius":120,"tolerance":0.25}}
edit.circleInfo={"layer":"Training polygons","row":0}
edit.deleteCircle={"layer":"Training polygons","row":0}
```

Replace `row:0` with the row returned by `edit.addCircle`. Update/delete accept `featureId`
to refuse stale rows. Updates preserve IDs and attributes, inherit an omitted frame and refuse
an explicitly changed frame. `edit.updateCircle` changes shape only; use `edit.setValue` for
attributes. Attribute names must already exist. `edit.circleInfo` returns the current construction.
`circle.frame` has the same EPSG, dimensions and affine input fields as `box.frame`, with no
new saved-file representation.

### Moving shared polygon vertices

**Edit › Features › Shared Vertices** opts polygon moves into linked editing. It starts off,
lasts only for the current app session, and is not saved in projects or preferences. With it on,
dragging a polygon vertex moves every polygon vertex in that layer stored at exactly the same
original coordinate, including unselected and filtered-out rows, point-only contacts, other
polygon parts and holes. Unrelated polygons touching by chance at that point move together too;
turn the switch off to move just the source feature. Inserting and deleting vertices, and moving
line or point vertices, keep their existing behaviour. Changing the switch cancels a held drag.

The command uses row positions and the same vertex addresses as `edit.vertex`:

```json
{"layer":"Parcels","row":0,"part":0,"ring":0,"index":3,"lon":12.5,"lat":41.9}
```

Run these parameters with `edit.moveSharedVertex`. It returns the source `part`, `ring` and
`index`, plus `modified`: the number of features whose geometry or attributes actually changed.
Every matched feature runs through attribute rules and archiving in one transaction; a rejection
rolls back the whole edit. A successful command is one undo step. As with `edit.vertex`, a direct
same-position command still runs the rules and records an edit, even if `modified` is zero.
Undo retains cumulative edit counts. Streamed layers require **Load All Features** first.

This moves coincident **vertices**, not every shared boundary. For a boundary stored as P–Q
on one polygon and P–R–Q on its neighbour, moving Q moves the two stored Q vertices but leaves
R in place. It does not remap intermediate vertices, insert missing ones or refuse the edit
because vertex densities differ; the two boundaries can separate. There is no tolerance or
nearby-coordinate matching. Use the `mustNotOverlap` and `mustNotHaveGaps` topology rules, then
**Edit › Manage Edits › Topology › Error Inspector (validate)** (`topology.validate`) to inspect a coverage.

### Distance units in commands

`edit.move`, `edit.traverse` and constant `edit.elevation` accept `"units": "Feet"`,
`"Meters"`, `"Kilometers"` or `"Miles"` (case insensitive). Omitting `units` keeps the
existing metre default. All traverse legs use the command's units. Conversion to metres
happens before ground-to-grid corrections and distance limits; stored elevation stays in
metres. Feet mean international feet (0.3048 m), independently of the source CRS's units.

For example, `edit.move={"dx":500,"dy":0,"units":"Feet"}` moves selected features
152.4 metres east.

## Data design

The Data tab's Data Design pane keeps attributes valid, on every edit (creating, pasting,
attribute edits, Calculate Field):

- **Domains**: coded values (`code=description`) or a numeric range for a field.
- **Subtypes**: an integer field's codes, each with a name and default values for new features;
  the layer is symbolized by subtype.
- **Attribute rules**: calculations (a field set from an expression on every insert and update)
  and constraints (a SQL condition every feature must meet).
- **Contingent values**: the combinations of values allowed across fields.
- **Archiving**: keeps every edit with its time; restore the layer as it was minutes ago.
- **Relationships**: link two layers by key fields; **Related Data** selects the rows related to
  the selection.

## Clipboard and pop-ups

Copy (++ctrl+c++) and Cut (++ctrl+x++) take selected features from one layer: the active layer
if it has a selection, otherwise the first layer with selected features. Paste (++ctrl+v++)
adds them to the layer selected in Layers, matching fields by name and converting values to the
destination field types; unmatched fields stay empty. Geometry types must match. A feature with a
place the destination layer's coordinate system can't hold is left out, and a notice counts them.
**Paste Special** adds them as a new layer instead. Copied features also go on the system clipboard as GeoJSON, so
they can be pasted into another RustGIS window or a text editor, and GeoJSON copied elsewhere
pastes into RustGIS. **Copy Path** copies the selected layer's data source path.

Whole-layer copying is separate: use **Copy Layer** or **Copy Group** and **Paste Layer** in the
Layers pane's context menus to preserve symbology and other layer settings, including when
moving layers between maps.

Pop-ups (Home › Navigate) choose which layers a click with Explore identifies: the topmost layer
under the click, every visible layer, only selectable layers, only the layer selected in Layers, or
nothing. The pop-up lists every feature under the click in those layers, overlapping ones included
(up to 100): step through them with ‹ ›. The feature shown is the one selected; when the pop-up
shows a raster's pixel or a web tile layer's feature, nothing is selected. The topmost and
visible modes take a raster's pixel values too, placed among the features by the drawing order: a
raster drawn over a polygon (rain clipped to an area whose outline stays on the map, say) shows its
pixel, and a polygon drawn over a raster shows its feature. The other two modes show the topmost
raster's pixel only when no feature is under the click. The magnifier in
the pop-up's header zooms to it. Long feature titles and layer names are shortened with an
ellipsis to leave room for the header controls; hover over either name to read it in full.

A value that is a web address is a link: click it to open the page in your browser (a new tab in the
web app), or right-click it for **Copy Link**. It must be the whole value: `http://` or `https://`
(in any case), a host name or IP address with no user name before it
(`https://bank.example@other.example/` stays text), at most 2048 bytes, and no spaces, control
characters, quotes or backticks, angle brackets, braces, backslashes or `^`. A picture kept with a
feature (a geotagged photo's thumbnail, a `data:image/jpeg`, `png` or `webp` value) shows at the top
of the pop-up. Without one, the pop-up can show a picture from the web: the `https://` address in a
field named `image`, `image_url`, `photo`, `photo_url`, `picture`, `picture_url`, `thumbnail` or
`thumbnail_url` (in any case, with or without `_`, `-` or spaces), or else the first address whose
path ends in `.jpg`, `.jpeg`, `.png` or `.webp`, with fields taken in the order of their names. An
address that can be fetched comes before a plain `http://` one or one on the local network. Fields
such as `image_page_url` or `image_credit` stay links or text in the list.

A picture from RustGIS's own asset server (`https://assets.rustgis.app/…`) is fetched as the pop-up
opens. A picture from any other server isn't: the pop-up shows **Show Picture** and the server's
name, and fetches nothing until you click it, because the request tells that server that someone
opened the data (see [Privacy](../privacy.md)). A spinner shows while a picture is fetched. PNG, JPEG,
WebP and GIF pictures of up to 4 MB and 2048 × 2048 pixels are shown (on every platform; any other
format, such as BMP or TIFF, shows "Picture unavailable: … pictures aren't supported"); the last 16 addresses' pictures are kept for the
session, so opening the pop-up again shows the picture without asking again; they are forgotten with
the web layers' cached tiles (signing out of ArcGIS, a login that ends, **Reload Web Layers**). When
one can't be shown, the pop-up says why ("Picture unavailable: …": the server's answer, a network
error, a server that stops sending it, not a picture, a picture format that isn't supported, too
large) and doesn't ask again; its address stays a link. A plain `http://` picture, or one on this
computer or the local network, is never fetched. In the web app the picture's server must allow it
to be read from another site (CORS); many photo sites do, some don't. It must also give the picture
at that address: the web app refuses a redirect, as it does for every download.

The topmost and visible modes also identify the features of vector tile layers under the click:
PMTiles and MBTiles archives and vector tiles added as data (not catalog basemaps or MapLibre
styles), beneath the map's own layers. Each shows its tile layer and source layer (`firenze ›
buildings`) and its attributes; tile features can't be selected or zoomed to.

They ask WMS layers too (WMS tile layers that are shown and, as far as their service's
capabilities say, queryable): RustGIS sends the service a standard GetFeatureInfo request for the
point (WMS 1.3.0 or 1.1.1, as in the layer's address), asking for GeoJSON when it offers it, else
GML, other XML, plain text or HTML. While it asks, the pop-up says "Asking the map service…".
When something else is under the click, it waits up to 4 seconds for every layer asked to answer,
then shows the features found and answered together, in drawing order; answers that come later
join it as they arrive. When nothing else is, it waits until an answer has features, and closes
once every layer asked has answered (or failed) without any.
When the service answers with an error in that format (swisstopo's MapServer can't write some of
its layers as GeoJSON, but answers them in GML), RustGIS asks again in the next format it offers,
and keeps asking that layer in it for the rest of the session.
Topmost asks only the highest WMS layer that can be asked at the point (one that covers it and is
queryable), and only when it is drawn over the feature found, or nothing was found; its features
then take that feature's place. A project doesn't save the request: a WMS layer from a project reads its service's
capabilities again on its first click (once for all the map's layers from that service), so until
then Topmost takes it as one that can be asked.
`identify` lists the requests in `featureInfo`, and `identify.featureInfo` reads an answer (or, on
the command line, asks the service and waits).

A tile layer's row in Layers can be clicked to select it, as a map layer's can (`map.selectTiles`;
no map layer is active then, and clicking one ends it). With **Selected in Layers**, a click on
the map then identifies that tile layer alone: a WMS layer is asked as above, and a vector tile
layer, archive or MapLibre style lists its features. This works for catalog basemaps and MapLibre
styles too, which the topmost and visible modes leave out.

### Manually converting imported polygons

Circles and rotated boxes shared as GeoJSON reopen as ordinary polygons. To edit one with
constrained handles again, turn **Edit › Edit** on, select its loaded polygon layer, right-click
the object and choose **Convert Polygon…**. Choose Circle or Rotated Box once, in the conversion
pane. The **Editing** pane also has
**Convert Selected Polygon…**, and Command Search has **Convert Polygon to Shape…**. These tools
are available independently of any annotation workflow. A right-click keeps the selected object
under the pointer; Alt-right-click cycles overlapping objects. In Explore, selection tools and
Edit Vertices, right-clicking a polygon selects that object for the menu. Right-clicking empty
space preserves the selection but offers no conversion target. Circle and Rotated Box instead
clear the selection on empty space, ready to draw another shape. Drawing and measurement tools
keep their current selection when opening the map menu.

1. Choose Circle or Rotated Box and a construction grid. The shape defaults to the active
   Circle or Rotated Box tool when opening conversion; other tools default to Circle. The default is **Web Mercator grid
   metres**, not ground metres. Choose an open GeoTIFF to fit in its **original image pixels**,
   preserving the source affine transform, including rotation and shear, rather than using a
   display overview. Local GeoTIFFs and embedded/browser-uploaded GeoTIFFs are supported; remote
   original grids must be downloaded and added first. An unreadable or unsupported source gives
   an error, without falling back to a different grid.
   The same [image source checks](#image-source-checks) apply. If asked to reload, use
   **Reload Data** on the image, then retry **Preview Conversion** or **Convert Now**.
2. For a quick conversion, click **Convert Now** to fit and apply using the chosen shape and
   grid, without a preview or another confirmation. Alternatively, click **Preview Conversion**
   to show an orange outline without changing the feature or undo history, review or adjust the
   centre and dimensions, then **Apply Conversion** or **Close**. Apply replaces Convert Now
   in the same button position; the actions and Construction Grid stay in place after preview.
3. Both routes replace only this feature, preserve its ID and attributes, and enable the existing
   Circle or Rotated Box editor. One Undo restores the original polygon and construction state;
   Redo restores the conversion. The desktop performs fitting and header reads in the background,
   with progress and Cancel in the status bar. Cancel or a changed source/selection before fitting
   finishes leaves the geometry and history unchanged. **Close** abandons an uncommitted
   preview; **Cancel Fitting** stops an active fit. After applying, use the regular **Undo**
   action to restore the original polygon. Drag the map to pan while fitting or
   reviewing a preview.

Conversion is not captured by **Record Task**: the proposal belongs to the current session and
cannot safely be replayed after reopening a project. A notice explains when a conversion is
omitted from the recording.

A rotated box uses the minimum-area enclosing rectangle of the input vertices in the chosen
planar grid (longer side as width, clockwise angle in `[0,180)`). A circle uses the centre of
the vertices' axis-aligned bounds and the maximum vertex distance as radius. This deterministic
circle encloses the vertices; it is not a least-squares or minimum-enclosing-circle fit.
The tolerance starts at the larger of 0.25 grid units and radius/10000, before projection.
Both operations can change the boundary; neither promises recovery of the original construction.
Transformed edges are still represented by straight polygon edges, with the limits described above.

Conversion accepts a single closed polygon without holes, with at most 4096 vertices (excluding
its closing duplicate). Multipart features, holes, degenerate grids/polygons and unsupported
projection boundaries are refused, never silently discarded. A source or selection changed while
reviewing the preview invalidates it; preview again. Edit rules that change the proposed geometry,
attributes or identity cause the whole conversion to be refused. Larger polygons must be simplified
explicitly before fitting.

The result remains an ordinary polygon on disk. Centre, radius and box dimensions live only in
this session; saves and exports retain the existing counted construction-loss notes. No custom
GeoJSON metadata, image bindings, class palette format or saved-file changes are introduced.

Scripts, CLI, Python, the Assistant/MCP and permitted plugins use the same commands:

```text
edit.previewShapeConversion={"layer":"Annotations","row":0,"kind":"rotatedBox","image":"Original TIFF"}
```

Pass that command's complete answer as `preview` to
`edit.applyShapeConversion={"preview":…,"confirm":true}` in the **same live session**. Preview is
read-only and does not grant consent to Apply. Preview tokens cannot be reused after reopening
a project; saved workflows must obtain a fresh preview. Omit `image` to use Web Mercator, or supply `frame`
with the same EPSG, dimensions and affine fields accepted by `edit.addBox` (image and frame are
mutually exclusive). Preview may read a source file and therefore declares the Files permission.
Synchronous API callers must invoke fitting off their own UI thread. On the phone the commands
are usable through its existing command interface; the desktop-style right-click menu is not a
new touch-bar destination.
