# Analysis

## Selection and queries

**Home › Select** picks features by clicking or dragging a box. Open **Select options** to choose
**Select by Polygon** (click vertices; click the first vertex or double-click to finish), **Select by
Lasso** (drag a freehand outline), or **Select by Radius** (drag from the centre to set a distance). Shift adds to the
selection; Ctrl/Cmd removes; Esc cancels an unfinished outline. Area tools use even-odd fill, so a
self-crossing outline selects its alternating filled regions. Radius uses the Neighborhood Explorer's
local Mercator distance and retained geographic prefilter; its outline and readout are approximate
and some features near the outline may be excluded. The main Select button remains click/rectangle
selection.
All selection tools take only features that are drawn, in selectable layers: a definition query or
a display filter hiding a feature keeps it out, and a click picks the feature drawn on top among the
selectable layers. **Select By Attributes** takes a SQL clause and checks it as you type; **Select By
Location** picks features that touch another layer's features or lie within a search distance of them (or,
inverted, those that don't). Selected features draw in
cyan and are highlighted in the attribute table, and the selection is synced both ways. Switch,
clear or zoom to the selection, or make a new layer from it (**Make Layer From Selected
Features**, `select.layerFromSelection`).

### The attribute table

++ctrl+t++ (or **Home › Selection › Attributes**) docks the active layer's table under the map.
Click a column header to sort by it (a second click reverses the order, a third unsorts), click a row to select it (++ctrl++ or
++shift++ toggles it without clearing the other rows), and double-click a cell to change its value. **Selected only**
keeps to the selected rows and **Filter rows…** keeps rows where any value contains the text typed,
ignoring letter case. The toolbar opens Select By Attributes and zooms to, switches, clears or
deletes the selection, adds or calculates a field, and calculates geometry attributes.
The first column, **Feature ID**, shows each feature's identifier, not its current row position
after sorting or filtering. It is not a dataset field: do not use it as a geoprocessing field name.

<figure markdown="span">
  ![The attribute table docked under the map: the 14 African countries of more than 30 million people, selected by attributes, highlighted on the map and in the table](https://assets.rustgis.app/images/attribute-table.webp)
  <figcaption>The attribute table, with a selection highlighted on the map and in the table.</figcaption>
</figure>

Select By Attributes and definition queries use SQL `WHERE` clauses, written in the
[expression language](expressions.md):

```sql
pop_est > 100000000 AND continent = 'Asia'
name LIKE 'S%'
iso_a3 IN ('FRA', 'DEU', 'ITA')
gdp_md IS NULL
"OR" = 1          -- double quotes name a field, even a keyword
```

## Geoprocessing tools

<figure markdown="span">
  ![The Analyze tab's tool gallery and the Tools pane with Buffer open; 300 km dissolved buffers around African cities](https://assets.rustgis.app/images/buffer-history.webp)
  <figcaption>The Analyze tab's tool gallery and the Tools pane running Buffer.</figcaption>
</figure>

Open a tool from **Analyze › Portal › Feature Analysis** or **Raster Analysis**, **Analyze › Tools** (the Tools pane,
which searches every tool) or Command Search. Fill in its parameters and click **Run**: new
layers or tables are added to the project; selection tools select existing rows and Calculate
Geometry Attributes updates the input layer. Each run is listed under **Analyze › History**.

In the desktop app a tool runs in the background, so the map and the rest of the window stay in
use. While it runs, the Tools pane shows its name, a **Cancel** button and, for the tools that
count their work (Viewshed, IDW Interpolation, Kriging, Kernel Density, Buffer, Select Layer By
Location and others), a progress bar. One tool runs at a time. A cancelled run adds nothing, and
for a tool that counts its work, its message says how far through its current step it got. Edits
made to its input layers while it runs are kept: a selection it makes selects the same features
where they now are (and says how many are gone), and a result that would replace an edited
layer, or one loaded whole meanwhile, is refused. Renaming the map while it runs is fine; after
switching to another map, a result that adds to or changes the map is refused (one made only of
tables or a file is still made). Opening another project drops the run. The Analysis pane's
**Zonal Statistics**, **Viewshed**, **Interpolate (IDW)**, **Suitability Modeler**, **Raster
Functions** and the **Function Editor**'s chains, **Change Detection**, **Classification Wizard**
(k-means), **Classification Tools** (from training polygons), **Indices**, **Process** (clip and resample), an ortho or
reality workspace's **Build** (its mosaic) and **Deep Learning** (training and classifying) run in
the background the same way, with their progress and **Cancel** under the pane's inputs, one at a
time; so does the map's right-click
**Viewshed … Here**, which opens the pane to show it. A model trained there is kept once its classes
are added: a cancelled run keeps none. Scripts, models, the AI
Assistant, the command line and the web app wait for each tool to finish.

**Viewshed**, **IDW Interpolation** and **Kriging** have no work limit when you run them from their
pane in the desktop app (the Tools or Analysis pane), nor does **Kernel Density** or **Deep
Learning**: a large run shows its progress, and **Cancel** stops it within a moment, however wide
its rows. Neither does `rustgis-cli run` typed at a terminal, which Ctrl-C stops. Where a run can't
be stopped while it goes (a script, a model, the AI Assistant, Python, the web app, and `rustgis-cli
run` called from another program, such as an agent or a CI job, which may wait with a timeout),
Viewshed, IDW Interpolation and Kriging refuse a run that would take more than a second or two of
work on one thread (a ray step, a cell-point pair), Kernel Density one whose points' search windows
(the square around each point's radius) cover more than 50 million cells, and Deep Learning a model
too large for its raster; its training fits a share of the sampled pixels, or fewer epochs, instead
(`dl.train` reports the epochs it ran). A task or model recorded from a pane run keeps these limits
when it runs again in the app, so a step the pane took on can be refused there, and a model trained
again can differ. On the desktop these limits grow with the cores the run takes (up to 16), as many
times more work as the threads it runs on. Threads share cores and memory, so a run at that limit
can take up to about twice as long as a one-thread run at its own (measured on one machine, a Xeon
Silver 4114; it can be longer on others); in the web app, on one thread, the limits stay as they
are. The error names the limit on this computer and the threads it counted. The limits on memory
(the cells a grid may have, and those on the following pages) hold everywhere, as does Viewshed's on
its window: a radius covering more than 25 million cells, or 100,000 cells across, is refused. A
tool's own default cell size still follows the one-thread limit, so it makes the same grid on any
computer and from any caller; a finer cell size given is what a larger limit, or none, lets through.

From the command line or a script (`gp.run`), a built-in tool's parameter with a list of choices, such as Buffer's
units, takes one of them in any letter case (`"meters"` for `Meters`); any other value is an error
that names the choices (Buffer's units no longer take the abbreviations `m`, `mi` or `metres`).
Buffer and Select Layer By Location accept **Kilometers**, **Meters**, **Miles** and **Feet**. Feet are international feet
(exactly 0.3048 metres): a 500-foot notification radius is 152.4 metres on the ground.

The History pane lists each run with when it ran (in UTC). Its **⋯** menu, or a right-click on
the tool's name, copies a run as code, with the parameters it ran with: **Copy as Python** (a
call for the [`rustgis` package](../python.md), one parameter a line), **Copy as Command** (a
line for the Script pane) and **Copy as CLI Command** (`rustgis-cli run --project … --cmd …`,
quoted for a POSIX shell: macOS, Linux, WSL or Git Bash; on Windows' own shells, use the Python
or the Script line). **Open in Tool** opens the tool's form with those values, to change and run
again. **Save as Python Script…** writes the whole history as a script that opens the project
and runs each tool in order; a run that failed is there as a comment. The CLI command and the
script open the project's file: for a project not saved yet, they name the file it would be
saved as, so save it there first; and a project changed since it was saved needs saving again,
or the script opens it without those changes (a layer added since, say). What gives access is
taken out of the code: addresses lose their keys, tokens and passwords, a connection its
password, a pasted private key goes, and a parameter named as a password, key, secret or token
is left empty (`gp.historyCode` and `gp.historyScript` do the same from a script). A secret
written some other way (a token typed into a SQL query, say) can still be there: read the code
before sharing it. A point buffered where the
map was clicked is copied as the command that buffered it, with the point.

These are the built-in tools; GeoLibre and plugin tools join them when available.
Use these ids in `gp.run`'s `tool` parameter (`gp.tools` lists the available tools and their parameters):

| Tool | Id | Toolbox |
| --- | --- | --- |
| Buffer | `buffer` | Analysis › Proximity |
| Near | `near` | Analysis › Proximity |
| Clip | `clip` | Analysis › Extract |
| Split | `split` | Analysis › Extract |
| Erase | `erase` | Analysis › Overlay |
| Intersect | `intersect` | Analysis › Overlay |
| Identity | `identity` | Analysis › Overlay |
| Union | `union` | Analysis › Overlay |
| Symmetrical Difference | `symmetricalDifference` | Analysis › Overlay |
| Update | `update` | Analysis › Overlay |
| Spatial Join | `spatialJoin` | Analysis › Overlay |
| Join Field | `joinField` | Data Management › Joins |
| Dissolve | `dissolve` | Data Management › Generalization |
| Minimum Bounding Geometry | `convexHull` | Data Management › Features |
| Feature To Point | `featureToPoint` | Data Management › Features |
| Merge | `merge` | Data Management › General |
| Feature Vertices To Points | `featureVerticesToPoints` | Data Management › Features |
| Points Along Lines | `pointsAlongLines` | Data Management › Features |
| Polygon To Line | `polygonToLine` | Data Management › Features |
| Split Line At Vertices | `splitLineAtVertices` | Data Management › Features |
| Feature Envelope To Polygon | `featureEnvelopeToPolygon` | Data Management › Features |
| Unsplit Line | `unsplitLine` | Data Management › Features |
| Check Geometry | `checkGeometry` | Data Management › Features |
| XY Table To Point | `xyTableToPoint` | Data Management › Features |
| Calculate Geometry Attributes | `calculateGeometry` | Data Management › Features |
| Simplify | `simplify` | Cartography › Generalization |
| Tissot's Indicatrix | `tissot` | Cartography › Projections |
| A5 Grid | `a5Grid` | Analysis › DGGS |
| Aggregate Points to A5 | `a5Aggregate` | Analysis › DGGS |
| Select Layer By Location | `selectByLocation` | Data Management › Layers and Table Views |
| Select Layer By Attribute | `selectByAttributes` | Data Management › Layers and Table Views |
| Summarize Within | `countPoints` | Analysis › Summarize |
| Summary Statistics | `summaryStatistics` | Analysis › Statistics |
| Pivot Table | `pivotTable` | Analysis › Statistics |
| Tabulate Intersection | `tabulateIntersection` | Analysis › Statistics |
| Export Features | `exportFeatures` | Conversion |
| Vector to PMTiles | `vectorToPmtiles` | Analyze › Tools |
| Raster to PMTiles | `rasterToPmtiles` | Analyze › Tools |
| IDW Interpolation | `idwInterpolation` | Analysis › Interpolation |
| Ordinary Kriging | `kriging` | Analysis › Interpolation |
| Zonal Statistics | `zonalStats` | Analysis › Raster |
| Kernel Density | `kernelDensity` | Analysis › Density |

**Raster to PMTiles** (`rasterToPmtiles`) writes Web Mercator PNG tiles using the raster's current
bands, colors, mask and resampling. Set the minimum and maximum zoom from 0 to 16 and choose a
`.pmtiles` output; without one it uses the last component of the layer name. Layer and group
transparency are included. The raster must be georeferenced and fit
within one world. Turn off dynamic range adjustment and display rotation first. The in-memory
export is limited to 512 tiles and 64 MiB of compressed tile data; reduce the extent or zoom range
if it exceeds a limit. Areas beyond the Web Mercator latitude limit are clipped.

**Vector to PMTiles** (`vectorToPmtiles`) writes a single-layer MVT archive in standard Web
Mercator. Choose `minZoom` and `maxZoom` from 0 to 16; `sourceLayer` defaults to the input
layer's name. The output file is saved atomically on desktop or downloaded in the browser.
Without `output`, CLI, script and task calls write `<layer>.pmtiles` in the process's working
directory, where `<layer>` is the layer's name without any folder part; an existing destination
is replaced atomically. The desktop tool pane instead asks for a destination with Save As.
Native exports refuse to replace the open/saved project file, the package this project was
opened from, or a file that a loaded layer, raster, standalone table or tile archive reads.
For directory-backed sources such as File Geodatabases, files inside the source directory are
protected as well.
This includes path aliases, ZIP archives holding loaded members, and Shapefile companion
files. Project/package paths stay anchored across changes to the working directory, and their
origins are restored when a project replacement is undone or redone. Browser downloads cannot
check the eventual save destination; choose a different file in the browser's save dialog to
avoid replacing an input.
An active selection is exported; otherwise the geoprocessing extent applies, and streamed
sources are read in full rather than limited to the visible map window. It exports points,
lines and polygons, including multipart geometry, attributes and feature IDs—not the map's
style or labels. Antimeridian paths are unwrapped and clipped with an 8/4096-tile buffer;
latitudes beyond Web Mercator's limit are clamped and reported. Geometry parts collapsed by
integer quantization and features without geometry are reported. Null properties are omitted;
integer attributes outside ±2^53 are rejected to avoid precision loss.

This is an in-memory export, limited to 10,000 distinct output tiles and a conservative
256 MiB working-memory budget, including the input, clipping/encoding temporaries and final
archive. This budget is checked after streamed-source materialization, not during provider
loading: the whole-read limits ([Large files](data.md#large-files)) apply first, and loading may
use more than 256 MiB before export refuses the input. Reduce a large source before loading it
for export. Geometry allocation estimates are linear in the number of vertices and rings.
Each geometry component's buffered candidate rectangle is limited to 40,000 tiles per zoom,
including candidates where clipping produces no output. A separate **100,000,000-unit
geometry-work budget** accumulates across all exported features, components and zooms:
each candidate tile costs the component's vertex count, and each polygon validity check
(before clipping and after integer quantization) costs `(vertices + rings)²`. The extra
vertex per ring allows for implicit ring closure. The work-limit error is distinct from
the memory-limit error: simplify polygon rings, reduce the input extent, or narrow the zoom
range. These limits reject the whole export rather than sampling or truncating it.
Browser task, model and script runs can queue at most eight files or 256 MiB of archive bytes
before downloads are submitted; split larger batches into separate runs. Browser success
means the download was submitted, not confirmation that the browser saved it. Example:

```text
gp.run={"tool":"vectorToPmtiles","params":{"input":"Roads","minZoom":0,"maxZoom":12,"sourceLayer":"roads","output":"roads.pmtiles"}}
```

**Simplify** uses Douglas–Peucker with a tolerance in kilometres. Each polygon ring keeps at
least three different, non-collinear corners (plus the closing vertex), so a tolerance larger
than a polygon produces a small ring instead of restoring its original vertices. Holes and
multipart rings are retained. This is not a topology-preserving simplifier: check the result
where holes or neighbouring polygons must stay aligned. The default tolerance remains 25 km;
use a smaller value for local boundaries.

**Kernel Density** (`kernelDensity`) calculates a continuous magnitude-per-unit-area raster
surface from point features using a Quartic (biweight, default) or Gaussian kernel (truncated at 3σ and
renormalised to integrate to 1).
Points can optionally be weighted by a numeric population field (invalid or non-positive weights
are excluded and reported in the tool messages; for multipoint features, each constituent point part
receives the feature's weight). Calculations are performed in the linear units
of a projected coordinate system (defaults to the layer's projected system; if the input layer lacks a
coordinate system or is in degrees, or to project into a specific system, supply a projected system via `crs`,
e.g. `EPSG:32618`).
Because bandwidth and cell size are measured in coordinate-space linear units, global cylindrical
projections such as Web Mercator (EPSG:3857) introduce scale distortion that increases with latitude;
use a local projected coordinate system (such as UTM or State Plane) when accurate ground distances
are required. Leaving `radius` empty or setting it to `0` automatically estimates the bandwidth
using Silverman's rule of thumb based on the points' spatial distribution (falling back to extent span / 30,
or 1000 coordinate-space linear units for a single point or coincident points). When `pixelSize` is
omitted, an adaptive cell size is calculated to span approximately 250 cells along the longer side
of the extent; the default search radius is automatically adjusted if needed to be at least the pixel
size so points are not missed between cell centres.
The search radius must be at least the pixel size. The extent can optionally be overridden via `extent`
as `xmin,ymin,xmax,ymax` text or a four-number array. The output is written as a single-band Float32 GeoTIFF.

Most feature tools process only the selected rows when an input has a selection. Selection tools
instead replace the input's selection; Select Layer By Location honours the selecting layer's
selection. Summarize Within counts the selected points against **all** input polygons, and
Calculate Geometry Attributes updates **all** rows. For tools that create a map layer, `output`
is the layer name; `vectorToPmtiles` uses it as a file destination.

**Buffer** distances, and the distance of **Select Layer By Location**, are measured on the Earth
(the WGS 84 ellipsoid, as PROJ and GeographicLib measure it), not in the map's display
projection. Each feature is buffered in an azimuthal equidistant projection on the ellipsoid
centred on it, so distances from a point are true; long lines and large polygon outlines are
buffered in stretches of a few hundred kilometres to limit projection error. Up to 85° of
latitude, a buffer's sides are within 0.012 % of the distance (or 1 cm), and within 0.05 % along
a winding line or outline buffered in stretches. Its curves are drawn with straight edges (128
to a point's circle, 64 to a semicircular line end). Between vertices, those edges cut up
to 0.031 % of the distance inside the true curve in the projection plane, and up to 0.125 % at
polygon corners and line joins, in addition to the projection error. Negative Buffer
distances shrink polygons. Buffer and Select Layer By Location limit distances to 5,000 km.
Ordinary Buffer limits each feature to 2,000,000 outline vertices. Its retained output has a
separate memory budget: on the desktop a quarter of the computer's memory, from 512 MiB up to 8 GiB
(`bufferBytes` in `gp.env`), and 128 MiB in the browser, including geometry
allocations, copied attributes and spare row capacity. Features are processed in bounded
batches, in input order. Many small buffers can therefore exceed 2,000,000 vertices together.
Dissolve All retains the shared 2,000,000-vertex limit before union and checks the dissolved
result too. Multipoint discs count before their union even when they overlap; for Dissolve All
their charges also accumulate across features.
Distance-based Select Layer By Location uses the same per-selector complexity limit and
memory budget for retained selector geometries, row capacity and a conservative spatial-index
storage allowance. With more than 16 distance selectors, selection stops with an error after
50,000,000 actual candidate checks across all target features, on both the grid and fallback
paths. Early matches stop their scan, so large input counts alone do not refuse a request.
Smaller selector sets keep their bounded-width linear scan. Selection at zero distance keeps
its existing behavior without these buffer limits. An excessive request fails with an error, never partial output; select fewer features
or split complex features. These budgets cover retained results, not total process memory:
the input layers, temporary geometry on active workers and polygon-overlay scratch are additional.
**Calculate Geometry Attributes** measures polygon areas and perimeters, and line lengths,
on the WGS 84 ellipsoid, independently of the map's display projection. Holes are subtracted
regardless of ring winding, and multipart measurements are summed. Ring directions are
normalized in continuous longitude space, so non-polar regions larger than half the Earth
keep their full area and small antimeridian crossings stay small. Rings winding around a pole
use the smaller polar region in either direction; reversing a ring does not select its global
complement. Edges follow the shortest geodesics between vertices. The output fields remain
`area_km2`, `perimeter_km` and `length_km`; point coordinates are unchanged.
Spatial Join's **Largest overlap** still measures area on a sphere of radius 6,371 km; it can
differ from the ellipsoid by up to about 0.9 % (a small area near a pole). The same WGS 84
measure backs the attribute table's **Calculate Geometry** (`edit.calculateGeometry`), the
[expressions](expressions.md#values)' `$area`, `$perimeter` and `$length`, the map's
**Measure** tools on the Earth, and every network tool's edge and route lengths, so all of these
read the same number for the same geometry, with two exceptions. A polygon ring with a full-turn
(-180° to 180°) edge, such as a globe-wide band, is measured on the sphere by the attribute table
and the Measure tools, while **Calculate Geometry Attributes** unwraps it, so it reads near zero
there. A ring winding around a pole is measured as the region it traces by the attribute table
and the Measure tools (a ring that goes all the way round at 60° N gives the larger region),
while **Calculate Geometry Attributes** gives the smaller polar cap. On the Moon or another body, the Measure tools measure on
that body's sphere. The Radius tools still measure a local Mercator distance, and **Mensuration
Tools** (`raster.measure`) still measure on the sphere, so those can differ from the numbers above.

**Merge** (`merge`, Data Management › General) appends loaded vector layers of the same geometry
type (points, lines or polygons) into a new layer. It needs no GeoLibre runner. Choose **Input
Layers** and **Preserve Attributes** in the tool pane, or pass names separated by `;` or a JSON
array. Desktop runs can also name local vector files without adding them to the map first.
In the browser, load the files as layers first. Each layer contributes its selection, or all its
features when it has no selection; read-in-place layers without a selection contribute their
complete source.

```json
{"tool":"merge","params":{"inputs":["Survey A","Survey B"],"attributes":true,"output":"Combined survey"}}
```

Matching field names (case-insensitive) and types share a column. Other fields are added and
missing values are empty. Incompatible types remain separate columns with unique suffixes,
such as `count_2`, preserving their original values and types. Turn off **Preserve Attributes**
for geometry only. Multipart geometry and holes are kept unchanged, new feature ids are assigned,
and export defaults to the first input's coordinate system; internal coordinates stay longitude
and latitude. The inputs stay unchanged, and the new layer can be undone.

**Feature Vertices To Points** (`featureVerticesToPoints`) writes a point at each vertex of the
`input` layer's features (a point layer's points are their own vertices, and a multipoint's points
its parts), with its feature's attributes and four fields. `orig_fid` is the feature's OID, linking
each point to its feature as ArcGIS's tool of the same name does. Three say where the point lies,
named as in QGIS's Extract Vertices: `vertex_part` (the part of a multipart feature),
`vertex_part_ring` (the polygon ring: 0 for the outer ring, and for lines and points) and
`vertex_part_index` (the place along the part or ring, from 0). Unlike QGIS's tool, it doesn't
write a polygon ring's closing point, which repeats its first. A new field whose name the input
already has gets a suffix (`vertex_part_2`). The points keep the input's coordinate system. In the
web app, the tool stops with a message if the browser can't give it the points' memory: select
fewer features, or simplify ones with many vertices.

```text
gp.run={"tool":"featureVerticesToPoints","params":{"input":"Rivers","output":"river_vertices"}}
```

**Points Along Lines** (`pointsAlongLines`) writes points at a set `distance` along each part of
the `input` layer's lines, or each ring of its polygons (holes too), measured on the WGS 84
ellipsoid as Buffer's distances are, in `units` (Kilometers by default): a point at `startOffset`
(0 by default), then one every `distance`, stopping `endOffset` before the part's end.
`includeEnd` adds a point where they stop when the distance doesn't land there (the first is
always at the start offset). Each part and ring is walked on its own from its first vertex, so the
gap between two parts never counts; a line's parts that cross the antimeridian, one ending on it
where the next begins on its other side, as a GeoJSON file splits a line that crosses it, are one
line. A ring ends where it starts, so a point there is made once, and a line that ends where it
starts (a contour) is walked as a ring too; a part shorter than its offsets gets none. A line that
passes through one place twice gets a point there each time the distance lands on it. Each point
has its feature's attributes and three fields: `orig_fid`, the feature's OID, as Feature Vertices
To Points writes it, and, named as in QGIS's Points along geometry, `distance` (how far along its
part, in `units`, the start offset included) and `angle` (the line's direction there, in degrees
clockwise from north; on a vertex, halfway between the segments either side). A new field whose
name the input already has gets a suffix (`distance_2`). QGIS's tool works differently: it
measures `distance` along the whole feature (a polygon's rings as one, its end offset left out) in
the layer's units, and walks a closed line as an open one, which gets a point at its end, on its
start again, when the distance lands there. Here each part and ring starts again from 0, and the
distance is on the ground in any coordinate system. Missing geometry, point features, and line
or polygon features with no measurable segments (including empty geometries) are skipped with
separate counted notices. A valid part that produces no points because of the offsets is not
reported as unusable geometry.

Each edge is the shortest path between its vertices on the ellipsoid, as Calculate Geometry
Attributes measures it and the globe draws it. A flat map draws an edge straight in its own
projection instead (in Web Mercator, along a constant bearing), so a point on a long edge can sit
off the drawn line: on an east–west edge 100 km long, by about 200 m at 45° of latitude, 340 m at
60° and 1.1 km at 80° (on one 10 km long, a hundredth of that; on the equator or along a meridian,
not at all). Add vertices to long edges first if the points must follow the drawn line. An edge
spanning more than 180° of longitude is taken the short way round, and one from −180° to 180° at
one latitude has no length; a polygon cut at the antimeridian is walked as it is stored, so it gets
points along the cut too (a polar cap whose parallel is one edge from −180° to 180°, only along the
cut). An edge between opposite sides of the Earth has no single shortest path, and the tool takes
the one GeographicLib does (over a pole for two points on the equator).

The angle is the bearing from true north, which near a pole turns quickly along even a straight
line (by 33° in 30 m, 11 m from the North Pole); at a pole itself, where every direction is south,
it follows GeographicLib's convention, from the meridian of the longitude the vertex is written
with. On a map in another projection than Web Mercator, north isn't straight up everywhere, so
there the angle differs from the line's direction on the map. In Web Mercator it differs from a long
edge's drawn direction by about half the edge's span of longitude times the sine of the latitude:
at the ends of an east–west edge 100 km long, 0.45° at 45° and 2.5° at 80°; and, as Web Mercator
isn't conformal on the ellipsoid, from a diagonal edge's by up to about 0.19° at the equator (0.1°
at 45°, less toward the poles). A vertex less than a millimeter from the one before is passed over,
so a near-duplicate doesn't turn a point's angle; a vertex past a pole (bad data) is taken as the
pole, and the tool says how many it took so; and a vertex with a coordinate missing or infinite is passed over (its neighbors joined, or a
line's end trimmed back to the last vertex it has), and the tool says how many it passed over. The output keeps what the input lost on reading, if
anything (features, or Z and M values), as its layer's notes say. The points
keep the input's coordinate system, with longitudes between -180 and 180, so a point past the
antimeridian is on the far side of the map. The points are counted first and may take up to about
4 GiB (some 20 million points with a few fields): a distance that would make more, or more than
the browser can give in the web app, stops the tool with a message before anything is built. Use a
larger distance, simplify lines with very many vertices, or select fewer features.

```text
gp.run={"tool":"pointsAlongLines","params":{"input":"Rivers","distance":500,"units":"Meters","includeEnd":true}}
```

**IDW Interpolation** works like the QGIS tool of the same name: every point weighs on every
cell by 1/distanceᴾ, where P is the distance coefficient (2 by default). Lines and polygons
contribute their vertices. Leave the extent empty to use the input's, or give it as
`xmin,ymin,xmax,ymax` in degrees. `pixelSize` sets the cell size in degrees; when omitted it
starts at 300 cells across the extent and coarsens for large inputs. The result is a Float32
GeoTIFF layer. The **Geostatistical Wizard** is a separate IDW workflow: it uses 12 nearest
neighbours by default (`neighbors`), rather than every point.

**Ordinary Kriging** (`kriging`) estimates both a value and its uncertainty. Set `input` and a
numeric `field`, then choose a `model`: **Spherical** (default), **Exponential** or **Gaussian**.
`neighbours` defaults to 12 (minimum 2, capped at 32). Optional `nugget`, `sill` and `range`
override the defaults: zero nugget, the sample variance (1 for near-constant values), and a
third of the extent's diagonal. Coordinates, `range`, `extent` and `pixelSize` are in degrees
(WGS 84). Points at the same location are averaged; multipoints contribute their points, while
lines and polygons contribute their bounding-box centres. At least two distinct locations with
numeric values are needed. It adds **two separate Float32 GeoTIFF layers**: the prediction,
named by `output`, and `<output>_variance`.

**Erase** (`erase`) removes from `input` features (points, lines or polygons) their parts inside
the `erase` polygons. An erase polygon's edge counts as inside it, as for Clip, so a point on the
edge or a stretch of line along it is erased; features left with nothing are left out, and one
no erase polygon overlaps is kept as it is.

**Intersect** (`intersect`) splits `input` features by the polygons of `overlay`: each part of an
input feature inside an overlay polygon becomes a feature with the attributes of both, the input's
first. A parcel crossed by two flood zones gives two features, one for each zone, and a point in two
overlapping polygons gives two. Input features outside every overlay polygon are left out. An
overlay polygon's edge counts as inside it, as for Clip: a point on the edge and a stretch of line
along it are kept, but polygons that only share an edge or a corner, and a line that only touches
one, give nothing. The input can be points, lines or polygons and keeps its geometry type; the
overlay must be polygons. An overlay field whose name the input already has gets a suffix (`id_2`).
Overlapping features within one layer are not split against each other. Like Clip and Erase, the
overlay works on the map's Web Mercator coordinates, so the parts' edges are straight on the map.
Selections in both layers are honoured. To measure each part, run Calculate Geometry Attributes on
the result:

```text
gp.run={"tool":"intersect","params":{"input":"parcels","overlay":"floodplain","output":"parcels_in_floodplain"}}
```

**Identity** (`identity`) takes the same parameters. Each input feature gives its Intersect
features, then its part outside every overlay polygon, with empty overlay attributes. Every parcel
is then in the result, in or out of the floodplain: one no overlay polygon meets, or without
geometry, as it is (Intersect leaves those out). **Union** (`union`) takes two polygon
layers and adds, after Identity's features, each overlay polygon's part outside every input polygon,
with empty input attributes: together the features cover everything either layer covers. Both
follow Intersect's rules for field names, touching parts and selections. Where overlay polygons
overlap each other, an input feature's part in both comes out once for each, and Union adds each
overlay polygon's own remainder, so the overlay's overlap outside the input comes out once for each
too.

**Symmetrical Difference** (`symmetricalDifference`) takes two polygon layers, as Union does, and
keeps only the pieces one layer covers: Union's features without Intersect's. Each input feature's
part outside every overlay polygon comes first, with empty overlay attributes, then each overlay
polygon's part outside every input polygon, with empty input attributes. As in Union, an input
feature without geometry is kept as it is, with empty overlay attributes, so no input row is lost.

**Update** (`update`) replaces part of a polygon layer with another: each `input` polygon's part
outside every `update` polygon keeps its attributes, and each update polygon is then added whole.
The output has the input's fields only. An update polygon's values go into the input's fields of the
same name (matched exactly, else ignoring case; converted to the field's type, so text that isn't a
number becomes empty in a numeric field); the input's other fields are left empty. An input field
that no update field matches exactly, but two match ignoring case, stops the tool rather than pick
one. An input polygon no update polygon overlaps, or one without geometry, is kept as it is. Update
polygons that overlap each other each come out whole, so their overlap comes out twice.

```text
gp.run={"tool":"update","params":{"input":"parcels","update":"resurveyed_parcels"}}
```

**Split** (`split`) clips `input` (points, lines or polygons) by each polygon of `split`, as Clip
does, and adds one layer for each. Give a `field` of the split layer to name the layers by its
values: polygons sharing a value make one layer together, and one without a value is named by its
id. Without a field, each polygon gives a layer named by its id. Names start with `output`, else
the input's name, and an underscore: `roads_North` (a name the map already has gets a number, as
for any new layer). A feature on the edge between two split
polygons goes into both layers. Layers that would be empty are left out. Split stops, before
clipping anything, when there are more than 1,000 split areas (split polygons, or values of the
field), empty ones included. Selections in both layers are honoured.

```text
gp.run={"tool":"split","params":{"input":"roads","split":"districts","field":"name"}}
```

**Near** (`near`) copies `input` and adds, for each feature, the nearest feature of `near`: its OID
as the attribute table shows it (`near_fid`), the distance to it (`near_dist`, in `units`,
Kilometers by default) and the point on it nearest the input feature (`near_x` and `near_y`, in
longitude from −180° up to 180° and latitude). Distances are measured as Spatial Join's **Closest**
measures them: on the ground, between the features' nearest points, and zero for features that
intersect, whose point is one they share. The point is found as the distance is, in a plane centred
between the features, so on a long edge it can sit off the geodesic by up to about 0.01 % of the
distance (5 m at 50 km). A `distance` limits how far it looks; features with nothing that near, or
without geometry, get empty values. Of equally near features, the first in the near layer wins.
With the same layer as both, each feature's nearest is another feature, not itself. A field name
the input already has gets a suffix (`near_dist_2`). Selections in both layers are honoured:

```text
gp.run={"tool":"near","params":{"input":"addresses","near":"hydrants","units":"Feet"}}
```

**Spatial Join** (`spatialJoin`, also `gp.spatialJoin`) copies attributes from `join` features
to `target` features. Layers may have different source coordinate systems: their geometry is
stored in longitude/latitude before the join. `match` is one of:

- **Intersects** (default), **Contains** (target contains join) or **Within** (target lies within
  join).
- **Have their center in**: the join features holding the target's interior point, the point
  Feature To Point gives. A parcel a zone boundary crosses goes with the zone its point falls in.
- **Largest overlap**: the one join polygon covering most of the target, by ground area for a
  polygon target and ground length for a line. Polygons that only touch don't overlap. Equal
  overlaps keep the join layer's order.
- **Closest**: the one nearest join feature. A `distance` (in `units`, as for Buffer; Kilometers
  by default) limits how far it looks, leaving targets with nothing that near unmatched; without
  one, every target gets its nearest feature. Ties keep the join layer's order.
- **Within a distance**: every join feature within `distance` (required), nearest first.

Distances are on the ground (the WGS 84 ellipsoid, as for Buffer), between the features' nearest
points (edges and vertices alike), and are zero for features that intersect. `distanceField`
names a field added for the distance to the joined feature, in `units`; it applies to
**Closest** and **Within a distance** only, as `distance` does, and the Tools pane shows them
only for those.

**Join one to one** (the default `operation`) copies the first matching feature's attributes;
**Join one to many** makes a row for each match. Both add `join_count`, the total matches for
that target (one at most for Largest overlap and Closest), and prefix joined fields with `join_`
(change `prefix` to override it). `keepAll` defaults to **Keep all**, leaving unmatched joined
values null; choose **Keep only matching** to drop those targets. Selections in both layers are
honoured.

In a one-to-one join, `merge` combines the values of all the matches instead of taking the
first: a list such as `[{"field": "units", "rule": "sum"}, {"field": "name", "rule": "join",
"delimiter": "; "}]` naming join fields. The rules are `first` (the default for fields without a
rule), `last`, `sum` and `mean` (numeric fields), `min` and `max` (numbers or text), `count` (the
matches with a value) and `join` (the values as text, separated by `delimiter`, `, ` by default).
Every rule but `first` and `last` skips empty values. A target without matches gets empty
values, except a `count` of 0. `mean` writes a decimal field, `count` a whole number and `join`
text. The `sum` of a whole-number field adds its whole numbers exactly, and is empty when too
large for a 64-bit integer; the tool says how many sums were left empty, and in which fields. A
decimal with a fraction, or a number past that range, isn't a whole number the field holds: the sum
leaves it out. The other numeric rules leave out a number too large for a decimal (text such as
`1e309`). The tool says how many matched numbers were left out, and in which fields. Whole numbers written as text count as the
numbers they are, however they are written (`12`, `12.0`, `1.2e1`). `min`
and `max` compare whole numbers exactly, past 2^53 (9,007,199,254,740,992) too, where their decimals
can be equal. The `mean` of whole numbers comes from their exact sum; a mean that is a whole number
past 2^53, which a decimal can't hold, is rounded, and the tool says how many were. For example, the
total units permitted in each district:

```text
gp.run={"tool":"spatialJoin","params":{"target":"districts","join":"permits","merge":[{"field":"units","rule":"sum"}]}}
```

In the Tools pane, use **Add rule** under **Merge Rules**, pick a field from Join Features
and its rule, and set **Delimiter** for `join`. Use **Move up**, **Move down** and **Remove** to
manage the list. Fields without a rule keep the first match.

**Join Field** (`joinField`) performs a one-off non-spatial attribute join,
copying fields from a source table or layer (`joinTable`, specified by name or numeric table ID)
into a `target` table or layer based on matching key values:

- **Key fields:** `targetField` (aliases: `target_field`, `key`) names the key field in the target.
  `joinField` (aliases: `join_field`) names the matching key in the source table; when omitted, empty,
  or whitespace-only, it defaults to `targetField`. Numeric keys match across integer and floating-point
  types when whole numbers (e.g. `10` matches `10.0`), while non-finite floats (`NaN`, infinity) and
  nulls never match. A text key matches an integer key when the text is the number written out in
  full (`"47157000100"` matches `47157000100`), so an all-digit ID typed as a number in one table and
  as text in the other (Census GEOIDs, FIPS codes, ZIP codes) joins when the codes match exactly as
  written. Leading zeros stay meaningful: a number that lost its zero (`1001` for `"01001"`) leaves its
  record unmatched, and a warning says how many records missed this way, with the first pair. Other
  incompatible key data types (e.g. text vs floating point or boolean) are rejected, and the message
  suggests converting one key with Add Field and Calculate Field.
- **Transferred fields:** `fields` selects specific source fields to transfer (a comma-separated list
  or a JSON array of strings, e.g. `["tax_rate", "zone_code"]`; non-string and non-array types are rejected). Repeated
  field names are deduplicated in order. When omitted or passed as a blank string (`""`), every source field except
  the join key is transferred; passing an explicitly empty array (`[]`) or an array of blank strings transfers zero fields.
  Transferred fields preserve their original source display aliases (retained without technical prefixes) and field types.
- **Prefix and collision handling:** `prefix` prepends text to all transferred field names. When a
  transferred field name conflicts with an existing target field, an incremental suffix is added
  (`name_2`, `name_3`) to maintain schema uniqueness without overwriting existing data.
- **Duplicate keys:** `duplicates` (`duplicateStrategy`) determines what happens when multiple source
  rows share the same join key: **First** (default: uses the first matching source record), **Last**
  (uses the last matching source record), or **Error** (fails the operation and reports the duplicate key).
- **Selections and streamed inputs:** selections on the target layer or source table are honoured;
  when a target selection is active, the output contains the selected target rows, while unselected
  datasets are processed in full. Streamed layers that are only partially loaded in the view window are
  rejected with a clear message (`the target layer is streamed; load all features first`) so incomplete
  results are never committed as a whole layer. Rows with attribute counts exceeding the schema definition
  are rejected with a clear error rather than silently truncated. Unmatched target records receive null values.

```text
gp.run={"tool":"joinField","params":{"target":"parcels","targetField":"owner_id","joinTable":"owners","fields":["name","phone"],"duplicates":"First"}}
```

**Summary Statistics** (`summaryStatistics`) makes a standalone table of statistics: one row for
the whole table (none for an empty one), or one for each combination of values of the `case`
fields (one field, or several as a list such as `["county", "year"]`). Rows are sorted by those
values; records with no value form a group of their own, first. Statistics follow record order
(`FIRST`, `LAST`, `CONCATENATE`) and use only the selected records when the table has a
selection. Each row has the case fields, then `FREQUENCY` (the group's records), then a field
`<STAT>_<field>` for each statistic asked for. Name the statistics in `statistics`, a list of up
to 256 (field, statistic) pairs in the order the fields should appear:

```text
gp.run={"tool":"summaryStatistics","params":{"input":"parcels","case":["county","year"],"statistics":[{"field":"income","statistic":"MEAN"},{"field":"income","statistic":"MEDIAN"},{"field":"zoning","statistic":"MODE"}]}}
```

In the Tools pane, use **Add statistic** to pick each field and statistic. The statistic picker
offers the operations supported by that field's type, including percentiles `P1` through `P99`
for numeric fields. Use **Add field** under **Case Fields** to group by several fields. Move
entries up or down to choose their output order, or remove them. Leaving Statistics empty keeps
the single **Statistics Field** shortcut. These lists are also available in ModelBuilder; for an
input produced by another tool, type its field names before running the model.

| Statistic | What it is |
| --- | --- |
| `COUNT` | Values that aren't empty |
| `SUM`, `MEAN`, `MIN`, `MAX` | The usual; `MIN` and `MAX` of text compare by character code, so ISO dates order as dates |
| `RANGE` | `MAX` − `MIN` |
| `STD`, `VARIANCE` | Population standard deviation and variance (divided by the count) |
| `MEDIAN` | The middle value; for an even count, the mean of the middle two |
| `P1` to `P99` | Percentiles, interpolated linearly between ranks (as a spreadsheet's `PERCENTILE.INC`), so `P50` is the median |
| `IQR` | `P75` − `P25` |
| `CV` | Coefficient of variation, `STD` / `MEAN`; empty when the mean is 0 |
| `MODE`, `MINORITY` | The most and least common value; on a tie, the lowest |
| `UNIQUE` | The number of distinct values |
| `FIRST`, `LAST` | The first and last value, in record order |
| `CONCATENATE` | The values in record order, joined by `separator` (`, ` by default; 256 bytes at most) |
| `NULLS` | Records with no value |

Every statistic but `NULLS` leaves out empty values. A text field takes `COUNT`, `MIN`, `MAX`,
`FIRST`, `LAST`, `UNIQUE`, `CONCATENATE`, `MODE`, `MINORITY` and `NULLS`; the rest need a numeric
field. A group with no values gets empty statistics, except a `COUNT` and `UNIQUE` of 0 (its
`NULLS` is its `FREQUENCY`). Two output fields can't share a name, so a case field can't be called
`FREQUENCY` or `<STAT>_<field>` for a statistic asked for. Counts are whole numbers, `CONCATENATE`
and the statistics of a text field are text, and the others are decimal. A decimal holds whole
numbers exactly only up to 2^53 (9,007,199,254,740,992): a statistic that should be a whole number
past it (a `SUM`, `RANGE` or evenly dividing `MEAN` of whole numbers, the `MIN`, `MAX`, `FIRST`,
`LAST`, `MODE` or `MINORITY` picked, or a whole `MEDIAN`, percentile or `IQR`) is rounded, and the
tool says how many results were rounded and in which fields. The whole numbers are an Integer
field's values and whole numbers written as text in a numeric field, which these checks and
`UNIQUE`, `MODE`, `MINORITY` and `CONCATENATE` take exactly up to the 64-bit integer range. A
longer one written in digits is read as a decimal, and its statistics aren't checked: the tool
says how many such values it read and in which fields, as statistics of them may be rounded. A
number written with a decimal point or an exponent (`6.02e23`) asks for a decimal and gets no
notice. A Double field's values are decimals already, rounded as decimal arithmetic rounds, and
get no notice. Without `statistics`, `field` gives that field's `COUNT`, `SUM`, `MEAN`, `MIN`,
`MAX` and `STD`, reading its values as numbers.

**Dissolve** (`dissolve`) merges the polygons sharing a value of `field` into one feature for
each value, or all of them into one without a field. Each feature has the value and `count`, the
polygons merged; records with no value form a feature of their own. **Summarize Within**
(`countPoints`) copies the polygons and adds `point_count`, the points inside each polygon or on
its edge; a point where polygons overlap counts in each of them.

Both tools take `statistics` to add summary fields: the list Summary Statistics takes, with the
same statistics, rules for text fields, empty values and output types (above), and `separator`
for `CONCATENATE`. Each pair adds a field `<STAT>_<field>` after the tool's own fields:

```text
gp.run={"tool":"dissolve","params":{"input":"parcels","field":"county","statistics":[{"field":"pop","statistic":"SUM"},{"field":"owner","statistic":"FIRST"}]}}
gp.run={"tool":"countPoints","params":{"polygons":"districts","points":"schools","statistics":[{"field":"pupils","statistic":"MEAN"}]}}
```

The first adds `SUM_pop` and `FIRST_owner` after `county` and `count`; the second adds
`MEAN_pupils` after the districts' fields and `point_count`. Dissolve's statistics are of the
input's fields, over exactly the records merged into each feature: a record without a polygon is
left out of them, as it is of `count`. Summarize Within's are of the point layer's fields, over
the points `point_count` counts; a polygon with no points (or no geometry) gets a `COUNT`,
`UNIQUE` and `NULLS` of 0 and empty other statistics. Both use only the selected records or
points when there is a selection, taken in record order for `FIRST`, `LAST` and `CONCATENATE`. A
summary field can't share a name, in any case, with another output field: Dissolve's dissolve
field, a field of Summarize Within's polygons, or another summary field. In the Tools pane, use
**Add statistic** as in Summary Statistics. Without `statistics`, neither tool adds fields.

**Pivot Table** (`pivotTable`) turns the values of one field into fields of their own. It makes a
standalone table with a row for each combination of values of the row `fields` (one field, or
several as a list); with no row fields there is one row for the whole table, or none for an empty
table. Then it adds a field for each value of the `pivot` field, sorted by value. Each cell holds
the `value` field's value for that row and pivot value, or is empty when no record has them. A
table with a record per county and year becomes one with a row per county and a field per year:

```text
gp.run={"tool":"pivotTable","params":{"input":"incomes","fields":"county","pivot":"year","value":"income"}}
```

| county | year2020 | year2021 | year2022 |
| --- | --- | --- | --- |
| A | 10 | 11 | |
| B | 20 | | 22 |

Use **Add field** under **Row Fields** in the Tools pane or ModelBuilder to choose several row
fields and order them with **Move up** and **Move down**.

Without `statistic`, each row and pivot value can have one record at most; the tool names the
first pair with more, and the cells keep the value field's type. Give a `statistic` to combine the
records instead, such as the mean income by county and year from a table of households. It
takes any statistic from Summary Statistics (above), with the same rules for text fields, empty
values, output types and whole numbers past 2^53: the tool says how many cells were rounded and in
which pivot fields. `separator` joins `CONCATENATE`'s values. A cell with no records is
empty even for `COUNT`, which tells it apart from records that have no value (a `COUNT` of 0):

```text
gp.run={"tool":"pivotTable","params":{"input":"households","fields":"county","pivot":"year","value":"income","statistic":"MEAN"}}
```

Field names come from the pivot values. A number, or true and false, follows the pivot field's name
(`year2020`, `rate2_5` for 2.5, `temp_3` for −3). Text keeps its letters, digits and underscores,
each run of other characters becoming one underscore (none right after an underscore) and
underscores at its ends dropped (`North east` and `North_-east` give `North_east`), and follows the
field's name when it would start with a digit or have nothing left. At most 64 characters of a
value are kept; a name already taken, ignoring case, gets `_2`, `_3` and so on in the order of the
pivot values. A record with no pivot value (or a number that isn't one, NaN) is an error: fill
those in, or select the other records. The pivot field can have at most 1,000 values, and the table
at most 10 million cells. Only selected records are used when the table has a selection, and the
output is named `<input>_Pivot` unless `output` names it.

The AI Assistant uses Pivot Table for questions such as "mean household income by county and year":
it runs the tool with `county` as the row field, `year` as the pivot field and `MEAN`, then reads
the table it names with `read_features`, a row per county.

**Tabulate Intersection** (`tabulateIntersection`) measures how much of each zone each class of
polygons covers, and shares the classes' totals out to the zones by area. Both layers are polygon
layers. Zone features sharing the values of the `zoneFields` (one field, or several as a list) make
one zone, and class features sharing the values of the `classFields` make one class; with no class
fields, all the class features are one class. The tool makes a standalone table with a row for each
zone and class that overlap (pairs that only touch get none), sorted by the zone fields' values,
then the class fields', with empty values first, as Summary Statistics sorts its rows:

| Field | Holds |
| --- | --- |
| The zone fields | The zone's values |
| The class fields | The class's values |
| `area_km2` | The ground area of the class in the zone, in km², where class features overlap each other counted once |
| `percentage` | The class's area in the zone as a percentage of the zone's area, both measured as the map draws them (below; empty for a zone with no area) |
| Each of `sumFields` | The sum, over the class's features, of the field's value times the part of the feature's area inside the zone |

A field name already used in the table, ignoring case, gets `_2` (`_3` and so on), as when a
class field and a zone field are both called `name`.

The sum fields share a total out by area, as for population counted by census block and wanted by
school district: a block half inside a district gives it half its population. They must be numeric,
and can't also be class fields. Zones that overlap each other each get their share, so a value can
be shared out more than once; the value of a feature's part outside every zone isn't shared out.

`area_km2` is measured on the ellipsoid, as Calculate Geometry Attributes measures it. The
percentages and shares compare areas measured on a sphere with each edge as the map draws it (a
straight line on the map), the lines the tool cuts along, so the shares of a feature that zones
cover without overlapping each other add up to its whole value, up to the overlay's rounding of the
cut. The overlay rounds coordinates to a grid (the size of the zones near a feature over about a
billion), so a part that leaves nothing outside its zone wider than that grid is the whole feature,
and only parts outside every zone wider than the grid are reported. A feature so narrow next to its
zones that the rounding can move its area by more than 1% is measured only roughly, unless it lies
wholly inside a zone, and the tool says how many there are. The percentages of classes that overlap
each other can add up to more than 100. Like the other overlay tools, it works on the map, which
ends at 85.05° north and south: parts of polygons beyond that are taken as lying along it, so
overlaps there have no area ([#1033](https://github.com/opengeos/RustGIS/issues/1033)); the tool
says how many features reach beyond it.

The tool says how many zone and class features have no polygon (no geometry, an empty one, a line or
point, or a ring collapsed to no area; they are left out), how many cross the antimeridian without
being split there (the map, and with it the percentages and shares, takes them the long way round,
while area_km2 takes their ground shape), how many zones overlap no class feature and so have no
rows, how many class features have no value in a sum field (they add nothing to its shares) and,
with sum fields, how many class features lie partly or wholly outside the zones. Only selected
features are used when a layer has a selection, and the table is named `<zones>_Tabulate` unless
`output` names it.

```text
gp.run={"tool":"tabulateIntersection","params":{"zones":"districts","zoneFields":"district","classes":"blocks","sumFields":["population"]}}
gp.run={"tool":"tabulateIntersection","params":{"zones":"counties","zoneFields":"county","classes":"landcover","classFields":"class"}}
```

<figure markdown="span">
  ![Tissot's indicatrix on the Robinson projection: circles of 800 km every 30°, stretched and sheared toward the poles](https://assets.rustgis.app/images/tissot.webp)
  <figcaption>Tissot's indicatrix on Robinson: equal circles, stretched and sheared by the projection.</figcaption>
</figure>

**Tissot's Indicatrix** shows what the map's projection does to the Earth. It draws the same small circle repeated over a grid: by default circles of 500 km radius every 30° over the whole world. With **Area: Map Extent** it fills the view instead (the whole Earth on the globe), for a country or a state with a smaller spacing and radius.

- **The circles are geodesic,** in longitude/latitude. The map draws each one as the ellipse its projection makes of it, and redraws them when you change the projection: growing circles in Web Mercator, flattened ellipses of equal area in Mollweide.
- **On a flat map, each circle also records Tissot's parameters at its centre** for the projection the map had when the tool ran:
    - `h` and `k`: scale along the meridian and the parallel;
    - `a` and `b`: the ellipse's semi-axes;
    - `area_scale`: `a·b`, which is 1 on an equal-area projection;
    - `max_angle`: the largest angular distortion ω in degrees, 0 on a conformal projection.

    They are measured against the WGS 84 ellipsoid. For a world projection the map draws on a sphere (Mollweide, Robinson…), they can be off by a few tenths of a percent.
- **Making a distortion map:** color the circles by `area_scale` or `max_angle`.

**A5 Grid** and **Aggregate Points to A5** use [A5](https://a5geo.org), a discrete global grid of
pentagonal cells that all have the same area at a given resolution. A count per cell is therefore
already a density, and stays comparable between regions, unlike a grid of cells that change size
with latitude or the map's projection.

- **A5 Grid** makes the cells at a resolution (0–30; each step divides a cell into four: 5 is
  about 33,000 km², 9 about 130 km², 12 about 2 km²) over the map's extent, an input layer's
  polygons (or the extent of its points and lines), or the whole world. **Cells: Overlapping**
  covers the area without gaps; **Center** keeps the cells whose centre is inside. A run makes at
  most 200,000 cells.
- **Aggregate Points to A5** counts a point layer's points (its selection, when there is one) by the
  cell holding them, and keeps only the cells that hold any. It adds `count` and
  `density_per_km2`, and a numeric field's `_sum` and `_mean`, where a feature's value counts
  once in each cell (a multipoint's, however many of its points fall there).
- Both write `a5` (the cell's 64-bit id in hex, as other A5 tools read it: DuckDB, PostgreSQL,
  Python's `pya5`, R), `resolution` and `area_km2`. A cell across the antimeridian, or round a pole, is
  split at ±180° into two parts of one polygon, each within −180° to 180°.

### Geometry conversions

These tools write new layers, honor selected features, and retain source fields, attributes and
CRS metadata. Coordinates are used directly, without projection or snapping.

- **Polygon To Line** (`polygonToLine`) makes one multipart line feature per polygon feature,
  including every outer and inner ring. Open rings are closed. Shared boundaries remain in both
  features; the tool does not build a shared-edge topology.
- **Split Line At Vertices** (`splitLineAtVertices`) writes one two-vertex feature for every
  consecutive pair in every line part, with the source attributes. Repeated vertices produce
  zero-length segments; parts are never connected across gaps. The browser checks the estimated
  output memory, including repeated text attributes, before building the segments; unavailable
  memory returns an error.
- **Feature Envelope To Polygon** (`featureEnvelopeToPolygon`) writes one axis-aligned rectangle
  around all coordinates of each feature in stored longitude/latitude axes. Point features and
  horizontal or vertical lines have zero-area envelopes and are skipped with a count. Longitude
  bounds are literal, including for features spanning the antimeridian.
- **Unsplit Line** (`unsplitLine`) joins line parts at exactly equal endpoints where precisely
  two part ends meet and all attribute values agree. Reversed parts and closed cycles are
  supported. Branches, attribute changes, and intersections away from endpoints stay separate.
  Every output chain retains its common attributes; no field values are discarded.

Missing geometries and empty or too-short parts are skipped with a count. If skipping leaves
no usable output, the tool returns an error. Non-finite coordinates and malformed rows with
more attribute values than schema fields are errors; missing attribute values become null.

```text
gp.run={"tool":"polygonToLine","params":{"input":"Regions","output":"Boundaries"}}
gp.run={"tool":"splitLineAtVertices","params":{"input":"Rivers"}}
gp.run={"tool":"featureEnvelopeToPolygon","params":{"input":"Regions"}}
gp.run={"tool":"unsplitLine","params":{"input":"Rivers"}}
```

### Checking geometry

**Check Geometry** (`checkGeometry`) finds the features whose geometry breaks the OGC Simple
Features rules and writes them to a new layer, `<input>_check` unless you name it. Only the
selected features are checked when some are selected. Each output row is its input feature as
stored, with the same feature id, fields, attributes and CRS, plus two text fields: `problem`,
the codes of what is wrong separated by `, `, and `detail`, a short description of each problem
separated by `; ` that names the part and ring (counted from 1; ring 1 is the exterior ring),
such as `exterior ring of part 1 crosses itself; 3 repeated vertices in ring 2 of part 1`. A
feature with more than 20 problems lists the first 20 and counts the rest. If the input already
has a `problem` or `detail` field, the new one gets the next free name (`problem_2`) and the
tool says so. When nothing is wrong, the output layer is empty and the message says how many
features were checked.

| Code | What is wrong |
| --- | --- |
| `null_geometry` | No geometry, or one without coordinates |
| `non_finite_coordinate` | A coordinate is not a number or infinite. The row is written without geometry, so it can't spoil the layer's extent |
| `too_few_points` | A line part with fewer than 2 distinct points, a ring with fewer than 3, or a polygon part without rings |
| `ring_not_closed` | A ring's last point is not its first |
| `repeated_vertex` | The same vertex twice in a row (a closed ring's last point doesn't count) |
| `self_intersection` | A ring crosses itself, touches itself, or runs back over itself |
| `hole_outside_shell` | An interior ring (hole) is not inside its exterior ring |
| `rings_intersect` | Two rings of one polygon overlap or touch along a line |
| `disconnected_interior` | Rings of one polygon touch at points so that they cut its interior into pieces, such as a hole touching the exterior ring at two points, or three holes each touching the next |
| `parts_overlap` | Two polygons of one multipart feature overlap or touch along a line |
| `too_complex` | Some of the feature's checks were left out: it needs more work than the tool spends on one feature, there wasn't enough memory to check it, or the geo library's relate operation failed on some of its rings or parts (`detail` says which of the three, not which rings or parts). The checks of each ring's points still run. The tool says how many features each reason left out |

Codes are listed in this order, and are never translated. The checks work in stored
coordinates, without projection. Lines may cross themselves, so only polygon rings are checked
for self-intersection; points are checked for missing and non-finite coordinates only. Rings may
touch each other at single points, as long as the polygon's interior stays in one piece (checked
once its rings have no other problem with each other); any number of rings may meet at the same
point. An unclosed ring is reported, then checked as if it
were closed. Ring orientation is not checked, since GeoJSON and Shapefiles wind rings in
opposite directions. The rings of a polygon are only checked against each other when each of them is
usable (enough points, finite coordinates, and not crossing, touching or running back over
itself), and the parts of a feature only when each part's rings are usable and have no problem
with each other; the tool says how many features were not fully checked this way, because some
of their rings or parts already have problems of their own. Rings and parts that touch are compared with the
[geo](https://github.com/georust/geo) library's relate operation.

```text
gp.run={"tool":"checkGeometry","params":{"input":"Parcels"}}
```

## GeoLibre tools

The desktop app and the command line can also run the
[GeoLibre](https://github.com/opengeos/geolibre-rust) tool suite: over 1,000 tools from
Whitebox and GeoLibre for terrain analysis (slope, aspect, hillshade, curvature), hydrology
(fill depressions, flow direction and accumulation, watersheds), image processing,
interpolation and vector work. They run on the map's layers and add their outputs as new
layers.

The suite is an optional download, so RustGIS itself stays small: the **GeoLibre runner**, a
single `geolibre` program (about 20 MB to download). In the Tools pane, click
**Install Whitebox and GeoLibre tools** under *Whitebox and GeoLibre tools*: RustGIS downloads the latest runner for your
platform from the [geolibre-rust releases](https://github.com/opengeos/geolibre-rust/releases/latest),
checks it against its published SHA-256 checksum (which catches a corrupted download; the
release itself is trusted as when you download it by hand) and installs it in its data folder
(by default `~/.local/share/rustgis/geolibre` on Linux, `~/Library/Application Support/RustGIS/geolibre`
on macOS, `%LOCALAPPDATA%\RustGIS\geolibre` on Windows). `RUSTGIS_APP_DATA_DIR` overrides the
data folder; Linux also honours `XDG_DATA_HOME`. From the command line, `gp.installGeolibre`
does the same.

To remove it, click **Uninstall** beside *GeoLibre* at the top of the Tools pane, then
**Confirm uninstall** (`gp.uninstallGeolibre` from the command line): RustGIS deletes the runner
it installed and removes the `geolibre` folder only if it is empty. Other files there, a runner
you located yourself, and one on your `PATH` are left alone; RustGIS looks for a runner again.
You can install the tools again at any time. In the browser,
**Remove** unloads the tools and stops loading them when RustGIS starts.

To install it yourself instead, download the archive for your platform
(`geolibre-<version>-<platform>.tar.gz`, or `.zip` on Windows), unpack it anywhere, and click
**Locate…** to pick the `geolibre` program; RustGIS remembers it.

RustGIS also finds the runner without asking when it is next to the RustGIS program (or in a
`geolibre` folder beside it), on your `PATH`, or named by the `RUSTGIS_GEOLIBRE` environment
variable (the command line uses these). The tools then appear under **GeoLibre** in the Tools
pane and the Browser's toolboxes, grouped by the category and subcategory the runner gives
each tool (Conversion, Hydrology, LiDAR, Projection, Raster, Remote Sensing, Terrain, Vector,
then e.g. **GeoLibre › Terrain › Derivatives**). A runner too old to list subcategories groups
them by category only. Find Tools searches them.

Field-name parameters (`field`, `*_field`, `fields` and `*_fields`) have a **Fields** menu beside
the text box. Pick an input layer or table first, then choose one of its fields. For tools with
several inputs, the longest matching input-name prefix selects the source: for example,
`denominator_weight_field` uses `denominator`, not `input`. Without a matching prefix, suggestions
come from the first vector or table input. Weight and numeric field suggestions show numeric
columns; a value or class field can also use text columns.

The text box stays editable for new column names or fields not yet known before a model runs.
For plural field parameters, each choice appends a name, separated by a comma (or by `;` if the
text already uses that separator). Follow the parameter's help if its runner needs different
list syntax, and check the tool's own help for what a weight field left blank means.

- Raster inputs are passed as their own files; vector layers (or their selection) are written
  in the first raster input's coordinate system, or, with no raster input, the UTM zone around
  the data. The latter uses metres; data spanning more than 40° of longitude stays in degrees.
- Tools that take several layers (a composite's bands, a mosaic's tiles) list them in order:
  **Add** picks a layer, and ▲ ▼ change its place (the order of the bands, for a composite). From
  the command line, name them separated by `;`: `"inputs":"red;green;blue"`. In the desktop app
  a file not open as a layer can be added too: type its path below the list and press Enter.
- Raster outputs are kept inside the project, so they save and reopen with it. Table outputs
  become standalone tables; text reports appear in the tool's messages.
- LiDAR and other inputs RustGIS doesn't open as layers take a file path, read like Add Data
  reads one. Outputs are written to a fresh scratch folder and loaded from there.
- Each run is a separate `geolibre` process, so a tool that fails can't take RustGIS down.
- From the command line: `gp.run={"tool":"geolibre:slope","params":{"input":"dem"}}`;
  `gp.tools` lists every tool's parameters, `gp.geolibre` shows the runner in use and
  `gp.setGeolibre={"path":"…"}` picks one.

In the web app, click **Load Whitebox and GeoLibre tools** in the Tools pane instead. RustGIS
loads geolibre-rust's WebAssembly runner (the `geolibre-wasm` package, from jsDelivr, about
25 MB the first time, then from the browser's cache) and the tools run in the page, in an
in-memory folder: nothing is uploaded. The web app remembers this and loads them again on your
next visit. The runner's code comes from jsDelivr, pinned to one geolibre-wasm release, the
same trust as installing that package. Tools that read files by path (LiDAR files, for instance) need the desktop app,
since a web page has no file system.

In the desktop app the Tools pane runs a GeoLibre tool in the background like any other; its
**Cancel** takes effect when the GeoLibre program ends, and the result is then dropped. On the
command line RustGIS waits while a tool runs. In the web app the run
is started in the background and the result is added when it ends, but the tool itself runs
in the page, so the page is busy while it computes.

## Analysis workflows

The Analyze tab's Workflows pane holds guided analyses. Where one needs points, it asks you to
click them on the map:

| Workflow | What it does |
| --- | --- |
| Raster Functions | Slope, aspect, hillshade, NDVI, band math (`(b4-b3)/(b4+b3)`), remapping, thresholds, resampling |
| Function Editor | Chains raster functions, each on the previous output |
| Suitability Modeler | Scores rasters 1–10 and adds them with weights |
| Visibility Analysis | A viewshed from a point on an elevation raster, with the Earth's curvature and the refraction of visible light (1/7, as `gdal_viewshed`) |
| Exploratory 3D Analysis | A line of sight between two points, split into seen and hidden parts, with the same curvature and refraction |
| Geostatistical Wizard | An IDW surface from feature values, using 12 nearest neighbours by default |
| Network Analysis | The shortest (or cheapest, by a cost field) route over a line layer |
| Neighborhood Explorer | What lies within a distance of a point, with sums of numeric fields |
| Simulation | Random points in polygons (a fixed number, or from a field × rate) |
| Data Engineering | Counts, nulls, distinct values, min, max and mean of every field |
| Exploratory Analysis | Histograms and correlations of numeric fields |
| Embeddings | The features most like the selected one, by their numeric attributes |
| Data Interop | KML, GPX, CSV with WKT, GeoJSON or CSV |

Network routing rebuilds its graph for each request and solves each edge length on WGS 84.
Large line networks can therefore take longer to route than with spherical lengths; repeated
routes do not currently share a cached graph.

Rasters these make are kept in the project. **Environments** (`gp.env`) set an extent from the map
view, full extent, a layer or a longitude/latitude box (or `null` to clear it). For tools that
honour it, this restricts an unselected main feature input by bounding-box intersection, not by
clipping its geometry; an existing selection takes precedence and overlay inputs stay whole.
The Geostatistical Wizard uses the environment's extent and raster cell size (degrees).
IDW Interpolation and Ordinary Kriging instead use their own `extent` and `pixelSize` parameters;
the environment does not set their output grids.

`gp.env` also returns the memory limits on this computer (`limits`). On the desktop they follow the
computer's memory, or a container's limit when it is lower: each plans for a quarter of it, never
below the web app's fixed limits and never above a ceiling.

| Limit | Fixed (web app, smallest) | Desktop: a quarter of memory at | Ceiling |
|---|---|---|---|
| `rasterSamples`: samples a raster read holds (cells times bands) | 120 million (32 million in the web app) | 8 bytes a sample | 1 billion |
| `outputPixels`: pixels of a raster a tool makes | 64 million | 16 bytes a pixel | 512 million |
| `pagedFeatures`: features read from a service page by page | 100,000 | 20 KB a feature (about 100,000 for every 8 GB) | 1 million |
| `densityCells`: cells of a Kernel Density raster | 16 million | 16 bytes a cell | 512 million |
| `bufferBytes`: memory for Buffer's output and Select Layer By Location's buffered selectors | 512 MiB (128 MiB in the web app) | the bytes themselves | 8 GiB |
| `threadScratchBytes`: working memory a tool's threads take together, beyond one thread's | 256 MiB | a sixteenth of memory, on top of the quarter | 4 GiB |

So a computer with 16 GB reads rasters of up to about 500 million samples. A run that passes on one
computer can be refused on a smaller one; the error names the limit there. Project Raster (`raster.project`) puts a raster
on a grid of any coordinate system, cell size and extent; Raster Functions › Resample matches a target
raster's grid.

The **Script** pane runs commands from text, one per line (`id {json}`); its box grows with the script down to the bottom of the pane,
short of the buttons, and then scrolls, so **Run** stays in view; Ctrl+Enter (⌘+Enter on macOS)
in the box runs the script too. Above a run's results, a line tells its number, how long it took
and when it started (`Run #2 (0.84 s)`, and `14:04 UTC` on the right), so a run made again by
mistake shows (Undo takes a whole run back). It colours the commands as you type, in both
themes: `#` comments in green italics, the command id, and the JSON's strings, numbers and
`true`/`false`/`null`; an id no command has, or one that can't run in a script (`ui.*`, `script.*`,
`notebook.run`), and parameters that aren't valid JSON are red and underlined. A script is read to
its 10,000th line; any further are dimmed, as Run doesn't read them. Typing a line's first word
opens a list of the commands it starts, under the cursor: ↑/↓ to move, Enter or Tab to accept,
Escape to close, or a click. Beside it, the highlighted command's help: its signature (keys with `?`
may be left out), its name, its parameters one per line, and where it sits in the interface.
Accepting a command on a line of its own adds its braces: with its required keys ready for their
values, the cursor at the first (`map.goToXY {"lon": , "lat": }`), or empty with the cursor inside
when every key may be left out; a command with no parameters gets none, and a line that has
parameters keeps them. While you type a command's parameters (the cursor inside its `{…}`), the same help stays by the cursor;
Escape closes it. A quote that starts a key in a command's braces opens the list of its keys not
given yet, beside the highlighted key's description; accepting one closes it with its colon
(`"size": `). The same goes in a key's own object, or in each object of its list, for the keys the
command's description gives it (`topology.rules {"add": {"rule": …}}`). In a `gp.run` line, a
quote after `"tool": ` opens the list of the tools, beside the highlighted tool's help; accepting
one closes it. In its `"params": {…}`, the keys offered are those of the tool its `"tool"` names
(by id or name, before or after `params`), beside the highlighted key's kind, name, default and
help; there the help by the cursor is the tool's. A `task.addStep` line works the same way: its
`"command": "` offers the commands, and its `"params"` that command's keys and help. For a
command that takes them, `"layer": "` offers the map's layers, top first, each with its kind's
icon and beside it what it holds; `"field": "` offers the fields of the layer the line names
(before or after it), or of the active layer when it names none, with each field's type beside it.
So do the other keys a command describes as taking a field or fields, also inside their arrays
(without the fields the array already lists):
`"fields": ["` in `data.explore` (only the numeric ones, as it says), `"sort": "` in `report.new`,
`"address": ["` in `layer.geocode` (the fields of its `"table"` when it names one).
`"map": "`, `"layout": "` and `"table": "` offer the project's maps, layouts and standalone tables,
with their layer counts, page sizes and row counts. `"class": "` offers the line's layer's label
classes (or the active layer's), with each class's SQL clause, or **All features**. Default is
included under its current name, except for commands that cannot remove or move it.
In a `gp.run` line's `"params"`, a value offers what the tool's parameter takes, as the tool's
form does: the layers of its kind (polygon, point, raster, vector; any layer, then the tables),
the fields of the tool's input that it takes (numeric, holding coordinates), or its choices
(`"units": "` offers `Kilometers`, `Meters`, …). Accepting one writes its name
as the line's JSON needs it (`"Big \"lakes\""`). A `{` or `[`
typed closes itself, the cursor inside, unless text follows it or it's in a string or a comment;
so does a `"` where a string can start (after a space, `{`, `[`, `,` or `:`). A `}` or `]` typed
before the same one steps over it, as does a `"` typed before the quote that ends its string, and
Backspace in an empty `{}`, `[]` or `""` deletes both. The lists open in a quote that closed
itself, and accepting keeps that quote. Pasting adds nothing.

## ModelBuilder

<figure markdown="span">
  ![ModelBuilder after a run: Cities buffered by 300 km clip the Rivers into "Rivers near cities", and Countries are dissolved by continent; every node is ticked and the results are on the map](https://assets.rustgis.app/images/modelbuilder.webp)
  <figcaption>A model after its run: each node is ticked, and its outputs are layers on the map.</figcaption>
</figure>

**Analyze › Model Builder** opens a model in a view of its own, a tab beside the maps and
layouts. A model is a graph of three kinds of node: **inputs** (blue) name layers of the map,
**tools** (amber) are geoprocessing tools, and **outputs** (green) keep a result as a layer of
their name.

- Drag a tool from the palette on the left onto the canvas, or click it; **+ Input** and
  **+ Output** add the other nodes.
- Drag from a node's right port to another node's left port to connect them. A tool's left
  ports are its layer parameters; a parameter that isn't connected can name a layer of the map
  instead, in the settings on the right.
- Select a node to set its parameters there; **Keep this result** adds an output for it.
- Drag the background to pan, Ctrl+wheel (or pinch) to zoom; **Arrange** lays the nodes out
  along the flow and **Fit** shows the whole model. Delete removes the selected node or wire,
  Ctrl+D copies a node, Ctrl+Z and Ctrl+Y undo and redo.
- Problems are listed below the canvas as you build (an input without a layer, a required
  parameter left empty, a loop) and the nodes concerned are marked; **Run** waits until there
  are none. The tools then run in flow order, and each node shows how it fared. A result that
  only feeds other tools is removed once the model has run; the outputs, and tools at the end
  of the flow, stay on the map. A run that fails is undone as a whole.
- **Save** keeps the model in the project; **Export** writes it as a `*.model.json` file, which
  **Import** (or dropping the file on the window) opens again. A model from a file may not read
  or write files. **Copy Command** copies a `model.run` command that runs the model from the
  Script pane, the command line or Python.

The Workflows pane's Models section lists the project's models, and New Model From History turns
the geoprocessing you've run into one. Models made as steps (`model.new` with `steps`, where `%1`
stands for step 1's output) run as they always have until they are saved from ModelBuilder, which
shows them as a graph and saves them as one. The `model.*` commands build, check (`model.validate`),
save and run models without the canvas:

```json
{"graph": {"nodes": [
   {"id": "i1", "kind": "input", "layer": "Cities"},
   {"id": "t1", "kind": "tool", "tool": "buffer", "params": {"distance": 50}},
   {"id": "o1", "kind": "output", "name": "City zones"}],
 "edges": [
   {"from": "i1", "fromPort": "out", "to": "t1", "toPort": "input"},
   {"from": "t1", "fromPort": "out", "to": "o1", "toPort": "in"}]}}
```

## Imagery

The Analyze tab's imagery groups open their tools in the Workflows pane:

- **Ortho Mapping** and **Reality Mapping** workspaces mosaic georeferenced images into an
  orthomosaic, or elevation rasters into a surface model that exports as a 3D mesh (OBJ).
- **Georeference** places a raster from control points (column, row, longitude, latitude) or
  stretches it over the map view. Those only shift and scale a north-up grid, as RustGIS's
  geotransform has no rotation. With `transform` (`raster.georeference`) the points are fitted by any
  of the Control Point Fit transforms and the raster is warped onto a new north-up grid covering the
  warped image: `{"layer": "scan", "transform": "spline", "points": [[column, row, x, y], …],
  "pointsCrs": "EPSG:4326", "crs": 32613, "resampling": "bilinear", "cellSize": 5, "name": "warped",
  "path": "warped.tif"}`. `pointsCrs` is the coordinate system the points' x and y are in (default
  EPSG:4326) and `crs` the output's (default `pointsCrs`, so nothing is reprojected unless asked).
  Each output cell is taken back through the fit, after reprojection, to the image and sampled there
  with `nearest`, `bilinear` (the default) or `cubic`; cells that fall outside the image are no
  data, or see-through for a colour image, which gains an alpha band for it (a grey byte image
  becomes grey with alpha; alpha is 0–255 in every pixel type). An image warped into a geographic
  `crs` gets square cells in degrees, and one that would span more than 180° of longitude is
  refused: give a projected `crs`. The cell is about the source pixel's size on the ground unless
  `cellSize` (output units) says otherwise. The result replaces the layer's raster, or becomes a new
  layer with `name`; `path` also writes it as a cloud-optimised GeoTIFF, which then is the layer's
  file. A raster opened from an overview is warped at that reduced resolution, and the result says
  so.
- **Control Point Fit** (`raster.gcpFit`) fits a similarity, affine, projective, second- or
  third-order polynomial, or thin plate spline transform to control points and reports how well
  it holds: each point's residual in pixels and map units, the RMSE, and each point's error when
  it is left out of the fit. With `compare` it fits them all and recommends the simplest whose
  leave-one-out error is close to the best.
- **Change Detection** (difference, ratio, percent), **Spectral Analysis** (every band at a
  pixel), **Indices** (NDVI, NDWI, SAVI, NBR, NDBI, EVI), **Process** (clip to the view, to a
  box, or to a polygon layer's selected or all polygons, with the cells outside them no-data;
  resample) and the **Pixel Editor** (set the pixels inside a polygon).
- **Mosaic Rasters** (`raster.mosaic`) merges tiles of one grid, such as the 1 km tiles of a state
  elevation model, into one raster in their own coordinate system and cell size, optionally
  written to a GeoTIFF: `{"inputs": ["dgm_1.tif", "dgm_2.tif", …], "path": "dem.tif"}`. Inputs
  are layers or file paths; where they overlap, the first one with data wins. Tiles in different
  coordinate systems or cell sizes, or opened from an overview, are refused rather than quietly
  resampled, as are tiles whose cells don't line up; the mosaic has to fit in memory (120 million
  values, cells times bands, or more on a computer with more memory: see Environments).
- **Project Raster** (`raster.project`) puts a raster on a grid of your choosing, so rasters from
  different sources line up cell for cell (for map algebra, the Suitability Modeler, a series for
  Trend Analysis, or the inputs of a model): `{"layer": "ndvi", "crs": 31983, "cellSize": 100,
  "extent": "Study area", "resampling": "average", "path": "ndvi_100m.tif"}`. It has no pane yet:
  run it from the Script pane, the AI Assistant, the command line or Python. The result is a new
  layer (`name`, by default the layer's name with "projected"), and with `path` a cloud-optimised
  GeoTIFF too.
  - **The grid:** `crs` (by default the layer's), `cellSize` in its units (one number, or
    `[x, y]`; by default about the layer's pixel there), `origin`, a point a cell corner falls on
    (`[0, 0]` by default, so cell corners are whole multiples of the cell size and any two rasters
    projected with the same `crs` and `cellSize` line up), and `extent`: a polygon layer (its
    selected features' bounds, or all of them), `[west, south, east, north]` in degrees or
    `"view"`, by default the layer's footprint. The extent is widened to whole cells.
  - **Another raster's grid:** `"grid": "dem_100m"` takes its coordinate system, cells and extent
    exactly, instead of `crs`, `cellSize`, `origin` and `extent`.
  - **Resampling:** `nearest` (the default: it never invents a value, which is right for classes),
    `bilinear` or `cubic` sample each cell at its centre. A cell whose centre falls on a no-data
    pixel (or on NaN, with or without a no-data value; in a colour image, no-data in every
    channel) is no-data; otherwise they interpolate from the pixels around it that have a value,
    as gdalwarp does. Going coarser, they skip the detail between cell centres and can give a jagged
    result, where gdalwarp widens its kernel to smooth it. `average` and `mode`
    take every pixel whose centre falls in the cell (no-data skipped), its mean or its most frequent
    value (of those tied, the one that reaches the top count first, the pixels taken row by row, as
    in GDAL), which is what a coarser grid of elevation or land-cover classes should hold. A
    cell no pixel centre falls in, on a grid finer than the raster, takes the pixel under its
    centre. Classes take `nearest` or `mode`, continuous values `bilinear` or `cubic`, or `average`
    going coarser. A whole-number pixel type keeps its type, so its mean is rounded. On a colour
    image the colours count as much as their pixels are opaque, and `mode` is taken band by band
    (each channel's most frequent value among the pixels that show).
  - Cells off the raster are its no-data value. A raster without one keeps its bands as they are,
    with NaN there in a floating-point type and 0 in a whole-number one, as GDAL leaves them; a
    colour image gains an alpha band instead, so they are see-through.
  - Datums are shifted as everywhere in RustGIS, through each system's transformation to WGS 84
    (a Helmert step for SAD69 or Córrego Alegre, a few metres from the IBGE's grid shifts).
  - The result has to fit in memory (120 million values, cells times bands, or more on a computer
    with more memory; 32 million in the web app): a grid past that is refused, naming the limit. A raster read from its overview is
    projected at that resolution, and the result says so.
- **Classification Wizard** (unsupervised k-means) and **Classification Tools** (minimum distance
  from training polygons).
- **Mensuration**: distances along the surface, heights between two points, areas, and volumes
  above or below a base height, with a list of results.

### Trend analysis

**Trend Analysis** (`cube.trend`, in the Workflows pane after Change Detection) asks of a series of
rasters whether each cell is changing, how fast, and how surely: an NDVI series greening or
browning, a temperature series warming. Each cell gets **Sen's slope**, the median of the slopes
between every two dates, which holds up against outliers such as a cloud left in an NDVI scene,
and a **Mann–Kendall** test of whether the trend is significant.

- **The series** (a data cube) is any of:
  - raster layers, a date each, dated by their names or their files' (`ndvi_2024-06-01`, MODIS's
    `A2024153`, `2024-06`, a year) or by the `dates` given: `{"layers": ["ndvi_2015", "ndvi_2016",
    …]}`. A layer on another grid than the first one listed is resampled onto it (nearest
    neighbour, as Raster Functions › Resample does) as it is read, so a scene with another cell size
    or coordinate system takes part. The first layer sets the cell size: a finer layer gives one of
    its pixels to each cell, not their mean. Cells a layer doesn't cover are empty that date. The
    Workflows pane's Dates field takes the dates separated by commas;
  - a Zarr variable along its time dimension, by its layer or its store, as for Summarise over time
    (see [Data](data.md)): `{"path": "air.zarr", "variable": "t2m"}`;
  - a raster of several bands, a date each, as Earth Engine and openEO export, dated by the bands'
    names (the descriptions GDAL writes in the file, or in an `.aux.xml` next to it: `NDVI
    2024-06-01`) or `dates`: `{"layer": "ndvi_2015_2024", "dates": ["2015", "2016", …]}`;
  - a STAC collection's band (desktop), read in place from its cloud-optimised GeoTIFFs over an
    area, as Add as Time Series finds them (see [Web data](web-data.md)): `{"url":
    "https://data.inpe.br/bdc/stac/v1", "collection": "mod13q1-6.1", "asset": "NDVI", "bbox": [-46,
    -10.95, -40.35, -2.7], "start": "2001", "end": "2025"}`. Each date's files (a grid tile each,
    MODIS's h13v09 and h13v10 for Piauí) are put together on one grid, each scaled as its own
    metadata says (NDVI's 0.0001). `"cellSize"` reads the level of the files' overviews nearest
    it (`1000` reads MODIS's 926 m one), which reads far fewer bytes; by default the full images.
    An overview is often one pixel taken from each block (the Brazil Data Cube's are), so a cell is
    then one 250 m pixel, which can differ much from the place's mean; `"resampling": "average"`
    makes each cell the mean of the full images' pixels in it, reading those (16 times the bytes
    at 1 km from 250 m). It needs every file's corner on a cell's corner, as MODIS's tiles are; a
    file on a grid shifted by part of a cell is refused: set `"resampling": "nearest"` and run it
    again. A file's no-data value is skipped, from its header or from its asset's metadata
    (`raster:bands` `nodata`). Where two tiles' files overlap on a date (items without a tile
    name are told apart by footprint), a cell is the mean of their values, while the data cube on
    the map shows one of them. This source is given as parameters (a script, the assistant, the
    command line); `cube.trend` waits for the search and the reads with no Cancel, the app with
    it, as for a Zarr store's address, so keep the area and period small there, or run a large
    one from the command line (see **Long runs** below).
  - an entry of the catalog of data cubes (see [Web data](web-data.md)), by its id: `{"cube":
    "bdc-mod13q1-ndvi", "bbox": [-46, -10.95, -40.35, -2.7], "start": "2001", "end": "2025"}`
    fills in the collection's API, band, cell size and cloud cap, or a Zarr entry's store and
    variable. In the app, **Trend Analysis…** on a cube of Home › Add › Web Data › Data Cubes opens
    this section on it: a STAC cube is read where the map is, a Zarr cube whole.

  For the mean of a finer layer's pixels in each cell, or for a grid of your own, put the rasters
  on one grid first with Project Raster (`"grid"`, the first one's, and `"resampling":
  "average"`).
- **The test:** NDVI and temperature series are autocorrelated, which makes the plain test report
  too many significant cells, so the default is the **modified Mann–Kendall** test of **Hamed &
  Rao (1998)**: the variance of S is corrected by the autocorrelation of the detrended series'
  ranks, at the lags significant at the test's level. The others:
  - **Yue & Wang (2004)**, from the lag-1 autocorrelation of the detrended series;
  - **Seasonal** (Hirsch et al. 1982), for a series with a yearly cycle: the test is made within
    each season (`"seasons": 12` for months, 23 for 16-day composites) and summed, and Sen's slope
    comes from pairs within a season;
  - **Mann–Kendall** for independent values.

  The definitions follow `pymannkendall`'s, ties included.
- **Options:** the significance level (`"alpha"`, 0.05), the slope per year or per decade (from the
  real dates, so uneven spacing is handled), a period (`start`, `end`; a whole number from 1000 to
  9999 is a year, and an end given as a year or a month takes all of it), a season across years
  (`"months": [12, 1, 2]`), and the dates a cell needs (`"minValid"`, by default half of them, at
  least 4). Missing values and no-data are skipped cell by cell.
- **Outputs:** four 32-bit layers, kept in the project as GeoTIFFs: Sen's slope, the test's Z, the
  p-value, and the slope where p < alpha (no-data elsewhere), the one a map usually shows on a
  diverging ramp centred on 0.
- **Size:** the cube is read into memory a slice at a time, up to 64 million values (cells times
  dates; 16 million in the web app) and 2,000 dates. A STAC collection's area is read in **pieces**
  instead, bands of rows each within that block (a whole number of the files' tile rows where they fit),
  eight dates at a time, and the outputs are put together: Piauí at about 1 km over 25 years of
  16-day composites (645 × 984 cells × 576 dates) is six pieces. Its area is up to 25 million
  cells. The seasonal test compares only pairs within a season, so its time limit counts those.
  It runs in the background with progress and Cancel.
- **Long runs:** in the app, a trend run from the Script pane or the assistant waits with no
  Cancel, so it takes a tenth of the panes' limit on pairs compared (2 billion), as Python does.
  `rustgis-cli run`, where nothing but a shell waits on it, takes the panes' limit (20 billion):
  Piauí's 576 composites at about 1 km with the seasonal test (about 4.4 billion pairs)
  run in one call there, and `rustgis-cli run` prints its progress (`cube.trend: 42%`). There, a
  STAC collection's search also takes up to 10,000 items for a trend or a summary (a state's four
  MODIS tiles over 25 years are 2,304). In the app, whose Script pane and assistant wait for the
  search with no Cancel, and in Python, it takes 2,000, as a data cube on the time slider does.

```sh
rustgis-cli run --cmd 'cube.trend={"layers":["ndvi_2015","ndvi_2016","ndvi_2017","ndvi_2018"],"per":"decade"}'
rustgis-cli run --cmd 'cube.trend={"url":"https://data.inpe.br/bdc/stac/v1","collection":"mod13q1-6.1","asset":"NDVI","bbox":[-46,-10.95,-40.35,-2.7],"start":"2001","end":"2025","cellSize":1000,"test":"seasonal","seasons":23,"per":"decade"}' --save piaui.rgis
```

### Summarising a data cube over time

**Summarise a Data Cube over Time** (`cube.reduce`) makes rasters of one statistic of each cell
over the dates of any of the series Trend Analysis takes (layers, a raster's bands, a Zarr
variable, a STAC collection's band): **mean**, **min**, **max**, **sum**, **std** (the population
standard deviation), **count** of values, or **countAbove** a threshold (`"above"`), as Summarise
over time does for a Zarr variable (see [Data](data.md)).

- **Classes:** a series of categorical rasters, such as yearly land-cover maps (MapBiomas, ESA
  WorldCover), is summarised as classes with eight more statistics:
  - **mode**, each cell's most frequent class, the least of those tied (`"majority"` too);
  - **modeCount**, at how many dates it had that class;
  - **variety**, how many classes it had;
  - **changes**, how many times its class changed from one date with a class to the next, in date
    order. A date with no data is skipped, so A, no data, B is one change. Within each group, so by
    year a change is one within the year;
  - **longestRun**, the class it kept for the most dates in a row, in date order, the least of those
    tied. A date with no data neither breaks a run nor adds to it, so A, no data, A is a run of two;
  - **longestRunLength**, how many dates that run lasted;
  - **frequency**, a layer for each class met (`… frequency 1985–2023 class 3`), each cell's count
    of dates in that class. By year or month, a layer for each class of each group;
  - **countIn**, at how many dates it was one of the classes given (`"classes": [3, 4, 5]`, whole
    numbers from 0 to 255), such as the years a cell was forest.

  They take the values as classes: a value that isn't a whole number (an NDVI) is refused with the
  value, as is a run with more than 1,000 classes, or more than 64 million cells × classes in the
  groups being read at once (16 million in the web app; for frequency, whose classes are each a
  layer, the outputs' 32 million values). These are the data cube summary's only: Summarise over time for a
  Zarr variable refuses them and points here. Mode, modeCount, variety and frequency keep a count
  of each class in each cell while they read, 2 bytes a class, for each group being read: by
  month, every month's at once, so a large area by month may need a smaller area or a group by
  year. **changes**,
  **longestRun** and **longestRunLength** read a STAC collection one date at a time, in order, so
  they are slower there than the other statistics, which read several dates at once.

- **Groups:** `"by": "all"` (the default) makes one layer of the whole period, `"year"` one for
  each calendar year (an annual mean NDVI from 16-day composites), `"month"` one for each month of
  the year across the years (a climatology). `"start"`, `"end"` and `"months"` pick the dates as for
  a trend.
- **Missing values:** no-data and missing values are skipped cell by cell. A cell keeps no value
  where it had fewer values than `"minValid"` (a share of its group's dates, 0 by default: 0.5
  wants half of a year's composites); with the default, **count**, **countAbove**, **countIn**,
  **modeCount**, **variety**, **changes**, **longestRunLength** and **frequency** give 0 where a cell
  never had a value, and **mode** and **longestRun** give none.
- **Size:** the slices are folded into running totals as they are read, so a long period takes no
  more memory than a short one; a STAC collection's area is read in pieces, as for a trend. The
  outputs hold up to 32 million values (cells times groups; 8 million in the web app). A summary by
  month keeps every month's totals while it reads, up to five times the outputs' memory for
  **std** (three for mean, min, max, sum and countAbove), so a large area by month may need a
  coarser cell size or a smaller area. `rustgis-cli run` takes a STAC collection's search up to
  10,000 items, as for a trend (the app and Python 2,000), and prints a summary's progress
  (`cube.reduce: 42%`). The layers
  are named after what they hold (`NDVI mean 2001`, `NDVI mean 2001–2025 month 03`, or the
  `"name"` given and the group) and kept in the project as GeoTIFFs.

```sh
rustgis-cli run --cmd 'cube.reduce={"layers":["ndvi_2015","ndvi_2016","ndvi_2017","ndvi_2018"],"stat":"max"}'
rustgis-cli run --data cobertura_1985.tif --data cobertura_1986.tif --data cobertura_1987.tif --cmd 'cube.reduce={"layers":["cobertura_1985","cobertura_1986","cobertura_1987"],"stat":"changes"}'
rustgis-cli run --cmd 'cube.reduce={"url":"https://data.inpe.br/bdc/stac/v1","collection":"mod13q1-6.1","asset":"NDVI","bbox":[-44.8,-7.5,-44.3,-7.0],"start":"2001","end":"2025","cellSize":1000,"stat":"mean","by":"year","minValid":0.5,"name":"NDVI"}' --save urucui.rgis
```

## Zonal statistics and polygon clipping

Open **Zonal Statistics** through Command Search, or choose it in the Workflows pane.

Zonal Statistics uses the selected polygon features when the layer has a selection. It keeps
original geometries and fields, and reports features with missing or rejected polygon parts.
If any polygon part fails validation or transformation to WGS 84, the feature receives count 0 and null
statistics. If a part subsequently fails projection onto the raster grid, it is omitted and reported;
statistics cover the remaining valid parts. Large
overlapping zones each require a separate pass over their raster window.

For `raster.process` polygon clipping, `selectedOnly: true` requires a selection,
`selectedOnly: false` uses every polygon, and omitting it uses the selection when present.
The Process pane initially checks Selected only when the chosen layer has a selection, and sends
the choice explicitly. Clipping and Zonal Statistics share
polygon transformation rules so a failed hole cannot silently enlarge the sampled area.
A malformed ring or a failed projection rejects its complete polygon part in all raster masking
operations. Clipping skips invalid features and reports their count in the command result;
its output bounds cover the valid polygons only.

An empty band field or `bands: ""` selects band 1. The pane disables calculation when raster
data is unavailable, shows the available band range for invalid band input, and indicates when
only selected polygons will appear in the output. Both tools report parts omitted during raster-grid
projection while retaining the valid parts.

Field parameters in Dissolve, Summarize Within, XY Table to Point, IDW, Kriging, Summary
Statistics and Pivot Table accept case-insensitive names. An exact spelling takes precedence;
otherwise a name matching several fields is rejected as ambiguous. This prevents silently using
the wrong field.

## Deep learning and hosted imagery

**Analyze › Deep Learning Tools** trains a neural network (a multilayer perceptron) on the pixels
under training polygons and classifies every pixel, adding a class raster with a confidence band
and reporting the accuracy on held-back pixels. Run from its pane in the desktop app, it trains
on every sampled pixel for all the epochs asked, however long that takes, and can be cancelled;
elsewhere it keeps to a few seconds of work (see the limits above). Trained models stay in the project; **Publish ›
Share As › Deep Learning** saves one as a package to apply in other projects. **Analyze › Create
Hosted Imagery** cuts a raster into XYZ web map tiles (PNG, transparent outside the raster) with
a TileJSON file and a viewer page.
