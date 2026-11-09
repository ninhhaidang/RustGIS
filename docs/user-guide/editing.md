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
area includes square feet (`ft2`), acres (`ac`) and square miles (`mi2`). Every edit is one undo
step. Areas, perimeters and lengths measure on the WGS 84 ellipsoid, the same measure as the network tools and the Measure panel (Spatial Join still
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
  remove it (a line keeps at least two vertices, a polygon ring three). Clicking elsewhere
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
  the feature's label point; this is not a change to its geometry's coordinates.
- **Ground to grid**: a scale factor and rotation applied to entered distances and directions
  (Move, and `edit.traverse`, which draws a line or polygon from distances and bearings).
- **Topology**: rules (must not overlap, must not have gaps, must not self-intersect, must not
  have dangles, must be covered by); Validate lists the errors, and clicking one zooms to it.
  Beyond 85.05° they follow the geometry tools (see
  [Geoprocessing tools](analysis.md#geoprocessing-tools), **Near the poles**).
- **Status**: features added, changed and deleted per layer since the last save.

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
destination field types; unmatched fields stay empty. Geometry types must match. **Paste Special**
adds them as a new layer instead. Copied features also go on the system clipboard as GeoJSON, so
they can be pasted into another RustGIS window or a text editor, and GeoJSON copied elsewhere
pastes into RustGIS. **Copy Path** copies the selected layer's data source path.

Whole-layer copying is separate: use **Copy Layer** or **Copy Group** and **Paste Layer** in the
Layers pane's context menus to preserve symbology and other layer settings, including when
moving layers between maps.

Pop-ups (Home › Navigate) choose which layers a click with Explore identifies: the topmost layer
under the click, every visible layer, only selectable layers, only the layer selected in Layers, or
nothing. The pop-up lists every feature under the click in those layers, overlapping ones included
(up to 100): step through them with ‹ ›. The feature shown is the one selected. The magnifier in
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
format, such as BMP or TIFF, is "not supported"); the last 16 addresses' pictures are kept for the
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
