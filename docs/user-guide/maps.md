# Maps and Basemaps

A project holds one or more maps (**Home › Project › New Map**), each with its own layers, basemap,
projection, bookmarks and view settings. Maps open as tabs above the map view. In the Browser,
right-click a map to rename, duplicate or delete it; the project always keeps at least one map.
For selecting, grouping, reordering and copying layers between maps, see
[The window](index.md#the-window).

## Navigating

Drag to pan and scroll to zoom; ++home++ (or **Full Extent**) shows all the data. **Home › Navigate**
has fixed zoom steps, **Previous** and **Next** extents, **Bookmarks** (the World Atlas has World,
Europe, Africa, Americas and East Asia), and **Locate**: **By Coordinates…** pans to a longitude
(X) and latitude (Y), each in decimal degrees or degrees, minutes and seconds (`-74.006` or
`74°0'22"W`; `40.7128` or `40°42'46"N`; an N or S in the longitude is refused, not swapped), or on
the Earth's map to a UTM or MGRS grid reference in place of both (`18north 583959 4507351`,
`18TWL8395907350`). `map.goToXY` takes the same: `lon` and `lat` as numbers or text, or a
`position` read as the search box reads it, latitude first; two bare numbers there are said back
as the latitude and longitude they were read as. A reference that names no place says why; an
MGRS reference coarser than a meter (`18TWL`) goes to the center of its square and says so.
**By Place or Address…** finds a place or address with a geocoder.
**New Bookmark…**
saves the current view under a name, with an optional description of the place. The Bookmarks menu
removes a bookmark with the button beside it, and exports or imports them as a `.rgisbookmarks`
file. Imported bookmarks show the same place on a map in another projection; places that projection
can't show are skipped. Type a scale in the status bar to jump to it: the map zooms from the whole
world in past 2:1, close enough for site plans at 1:20 and finds drawings at 1:1 (type `2:1` for twice
life size). Beside it are the zoom level, bearing and pitch; click them to face north and look
straight down again. More navigation tools are under [View effects](#view-effects) below.

**Measurement units.** In **Home › Inspect › Measure**, choose **Metric** or **Imperial**.
The choice is remembered and updates existing distance and area measurements, the live
map scale bar, elevation profiles, and distance labels and presets for label visibility
and view clipping. Imperial readouts use feet or miles for length, and square feet, acres
or square miles for area, according to size. Profile elevations and gain/loss use feet.
Feet are international feet (0.3048 m).
Scripts can set `ui.measurementUnits={"units":"imperial"}` or `{"units":"metric"}`;
calling it without `units` toggles the choice. Distance presets still send metre values
to commands. Editing tools, geoprocessing tools, directions and layout scale bars have
their own explicit unit controls.

**Zoom To Layer** in a layer's right-click menu shows that layer's extent instead of the whole
map. It works for feature and raster data, PMTiles archives, cloud-optimised GeoTIFFs, and service
tile layers whose capabilities advertise geographic bounds, including WMS. WMS 1.3 geographic
bounds and WMS 1.1 latitude/longitude bounds are inherited from parent layers when needed and
saved with the project. Re-add a service layer saved by an older version to retain its bounds.
Tile layers without known bounds (such as plain XYZ/TMS or MVT templates, styles and MBTiles
archives) have no **Zoom To Layer** action unless bounds were supplied when adding them. A zoom to a
place the map's projection doesn't reach (wholly past Web Mercator's 85°, say) doesn't move the
view to it, and says so.

**Rotating and tilting the map.** Drag with the right mouse button, or hold ++ctrl++ and drag
with the Pan tool, to turn the map about the centre of the view (to the right turns it clockwise)
and to tilt it (up tilts it further), as in MapLibre; or hold ++shift++ and press ++arrow-left++ or ++arrow-right++ to turn it, ++arrow-up++ or ++arrow-down++
to tilt it. The north arrow turns with the map to point north and leans back as it tilts; click
it to face north and look straight down again. The bearing (the compass direction the top of the
view faces, in degrees clockwise from north) and the pitch (the tilt from looking straight down,
0 to 85 degrees) work as in MapLibre. They are saved with the project, its bookmarks and bookmark
files, and exported maps are drawn turned and tilted. A tilted map keeps the scale at the centre
of the view: nearer ground is larger, farther ground smaller and drawn coarser, fading into fog
and, past about 72 degrees, a sky above the horizon. Labels stay upright, and those too far away
to read are left out. Point markers stand upright, facing you, as MapLibre's circles do, smaller
with distance (aggregated, masked, blended or diagrammed point layers, and layers of more than 50,000 points,
keep their markers on the ground). Extruded layers (Style › Extrusion) stand up in perspective on a tilted
map, as in MapLibre: walls lit by the way they face and darker toward their foot, roofs at their
height, with the labels of extruded features on their roofs. While a tilted map moves, the
picture drawn last is laid over the ground and the extrusions and markers are moved with it, and the map is
drawn again once it settles. `map.setBearing` sets the bearing (`{"bearing": 90}`
puts east at the top; `{"by": 15}` turns it further), `map.setPitch` the pitch (`{"pitch": 60}`,
or `{"by": 10}`), `map.resetNorth` faces north and looks straight down, and `map.goToXY` takes a
`bearing` and a `pitch` too. Page layout map frames take their own bearing and pitch (see
[Layouts](layouts.md)), and the globe turns and tilts the same way: tilted, its camera swings
back about the centre of view, so the horizon comes into sight past the curve of the Earth.

<figure markdown="span">
  ![Countries extruded by population on a map tilted 55° and turned 20°, lit and shaded, with labels on their roofs](https://assets.rustgis.app/images/extrusion.webp)
  <figcaption>Countries extruded by population on a map tilted 55°.</figcaption>
</figure>

**Terrain** gives a tilted map relief, as MapLibre's terrain does: the map is draped over the
ground's heights, labels and point markers stand on it, and nearer slopes hide what lies behind
them. While the map moves, the relief is laid on a coarse grid, to keep up; once it settles, and
in exported maps, on a grid four times finer each way, so a ridge's outline follows the ridge
(the settled map fades in over the moving one in a quarter of a second). The heights come from a
DEM raster layer of the project (heights in metres; the layer can stay hidden) or from elevation
tiles: Terrarium (`terrarium`, as the free
`https://s3.amazonaws.com/elevation-tiles-prod/terrarium/{z}/{x}/{y}.png`) or Mapbox
Terrain-RGB (`mapbox`, also MapTiler's). **View › Scene › Terrain** turns it on from the active
layer (make the DEM the active layer first) and off again; a notice names the layer and says to
tilt the map. `map.terrain` sets it: `{"layer": "DEM"}`, or `{}` for the active raster layer when
no terrain is on, or `{"tiles": "https://…/{z}/{x}/{y}.png", "encoding": "terrarium"}`, with `"exaggeration"`
(0 to 20, default 1) to stretch the heights; `{"exaggeration": 2}` alone changes only that, and
`{"off": true}` flattens the map again. Terrain is saved with the project and shows in exported
maps; a flat (untilted) view, the globe and other projections stay flat, extruded layers are
not lifted onto the relief yet, and identify and selection still pick on the flat ground.

A **block diagram** cuts the relief to a box, as geomorphology and soil science figures do:
`map.terrain` with `"block": {}` draws only the ground inside the DEM layer's extent (for a DEM
in a projection such as UTM, a north-up box between its corners, close to the largest inside
it for one turned a little; one turned a lot can leave a sliver past its edges), with the map's sky and fog
around it, and the box's sides facing the eye as walls from the ground's profile down to a flat
base. `"extent": [xmin, ymin, xmax, ymax]` gives another box, in the map's units (Web Mercator
metres, at least 10 a side, not all within ±180 by ±90; needed over elevation tiles, which have no extent of their own; a box
in degrees, off the world or off the DEM is refused, and the result gives the box cut to as
numbers, or `"layer"` while the DEM's data isn't read); `"depth"` puts the base
that many metres below the lowest ground in the box (ground with no height, drawn at 0, counts as 0, and a notice says how much of the box has none; default half the box's relief, at least
100 m; both are stretched by the exaggeration), and `"walls": "#RRGGBB"` colours the walls
(default `#C8B89A`, a sandy brown). The walls are lit by the way they face the light, as extruded
walls are, darker toward their foot, and outlined. Left out, the block stays as it was, and
a `"block"` object changes only the keys it gives: keys left out stay as they were, and `null` puts one back to its
default (`"extent": null` the DEM's extent, `"depth": null` half the relief, `"walls": null` `#C8B89A` with no layer,
`"water": null` or `false` no water in the walls, `"shape": null` a box, `"radius": null` the circle's largest);
the block's `"water"` is given whole (`{depth, field}`) each time: `{"water": {"depth": 5}}` replaces a water that had a field.
Without a block, the keys left out take the defaults, and `"block": true` keeps a block as it is, or draws one with them;
`"block": false` takes it off. It shows in the settled view and in exported maps (over elevation tiles, the walls once the
box's tiles are read); while the map
moves the ground is drawn whole, and labels, markers and extruded features outside the box still
draw (hide those layers, or clip them to the box). The walls are schematic: say so in the
figure's caption.

`"walls": {"color": "#C8B89A", "layer": "Geology"}` colours the walls by a polygon layer, such as
a geology or soil map: along each wall, the colour the layer's symbology gives the polygon under
the box's edge at that point (its Unique Values or Graduated Colors classes, so the legend
matches; the topmost polygon where they overlap, among the features its definition query keeps;
a fill colour partly transparent is laid over `color`). The contacts go straight down to the base,
outlined: only where they cross the box's edge comes from the map, and the walls show no dip or
thickness. Off every polygon the walls are `color`, and so is the background of a polygon with
no solid fill. The hatches and markers of the class's fill layers (Fill Layers) over its topmost
solid fill, or over `color` when it has none, are drawn on the wall too, in its plane and in
perspective, as a geologic block diagram shows its rock units: a hatch's angle is from the
horizontal (0° gives bedding lines), and a pattern is as big on the wall as on the ground at the
view's centre (a reference scale grows it as it does on the ground). A pattern too fine to draw
on a wall shows as its colour mixed over the fill. Gradient and shapeburst fills aren't drawn on
walls, and a notice says how many of the layer's polygons along them show without theirs. The layer may be hidden, so the walls can show geology while the ground shows soils. It
must be a polygon layer that holds all its
features (not one read a view at a time), with a name of its own; it follows the layer's renames,
and when it is removed, or a project opens without it, the walls are `color` (said) until a
polygon layer of that name comes back. A plain `"walls": "#RRGGBB"` takes the layer off.
`{"walls": {"layer": "Geology"}}` keeps the walls' colour, `{"walls": {"color": "#806040"}}` keeps the layer, and
`{"walls": {"layer": null}}` takes the layer off and keeps the colour.

`"shape": "circle"` cuts a round block: the largest circle inside the box (the DEM's extent or
`"extent"`), centred in it, or a smaller one with `"radius"` (map units; one larger than the
box's circle takes the box's). It reads the same at any bearing, which suits a figure that
turns or an orbit. Only the ground inside the circle is drawn and sets the base, and the wall
runs round its near half, with everything a box's walls have: colour, a walls layer's units and
their patterns (which run on round the wall), light and fog. `"shape": "box"` (the default)
takes it back to the box. A block kept from before stays round when another key changes, its radius too.

`"shape": {"layer": "Bacias", "where": "nome = 'Rio Doce'"}` cuts the block to a study area: the
polygons of a polygon layer that the expression picks (written as a definition query, among the
rows the layer's own definition query keeps; all of them without `"where"`). Only the ground
inside their outer rings is drawn and sets the base, the block's box is the one round them (so
no `"extent"`), and it works over elevation tiles too. The walls stand along the outline where
it faces you, a concave outline's hidden where nearer ground or walls stand in front of them,
with everything a box's walls have; a wall's corner is outlined where the outline turns by more
than 45°. A `"where"` that picks no polygon is refused. Holes are not cut: the ground inside
them is drawn, and a notice says how many. Several polygons each stand as their own block on the
same base (dissolve touching ones first for one outline). The outline follows the polygons to about a third of the
finest mesh cell (a ring narrower than that is left out, and said), and is read again when the
layer is edited. A box kept from before is dropped when the block is cut to polygons.

With water on the terrain (below), `"block": {"water": {"depth": 12}}` shows it in the walls'
section: where the block's edge (a round block's circle too) runs inside a water polygon (the first in the layer's rows where
they overlap, among the features its definition query keeps), the wall shows a band of water from
the water's level down to its floor, over the walls' colours, in the water layer's solid fill (the
topmost of its fill layers; one partly transparent laid over a light blue, as a section is opaque),
lit as the wall is and darker with depth, half as bright at the full
depth, with a line at the level and one along the floor as dark as the shore band. A DEM holds the
water's surface, not its bed, so the depth is given: `"depth"` in metres for every water body, or
`"field": "prof"` names a numeric field of the water layer holding each one's depth (`"depth"` then
serves the rows with no value, empty or 0). Both are stretched by the exaggeration, and the floor
goes no lower than the base. The floor follows the shore: no depth at the shoreline, the full depth
from about the shore band's width in (a few pixels at the view's centre), so the section shows a
basin, not a cliff. A water body with no depth, or no level, shows none, and setting the block or
the water says how many of those its definition query keeps have no value in the field. These depths
are schematic unless they come from a field: say so in the figure's caption. `"water": false` in the
block takes the water out of the walls (the rest of the block stays as it was); while it is there, taking the terrain's water off, or giving
it a layer without the depth field, is refused. Deleting the depth field keeps the water's own
`depth` for all the water, or takes the water out of the walls when there is none, and says so.

**Water** on the terrain lies flat: `map.terrain` with `{"water": {"layer": "Lakes"}}` names a
polygon layer of water bodies, and inside each polygon the ground is level, at the lowest height
of the DEM along its shore, instead of rising and falling with a surface model that is noisy or
stepped over water (Copernicus GLO-30 is one). `"level": "field"` takes each polygon's level
(metres) from a numeric field instead; a row with no value there keeps its shore's. Each part of
a multipolygon is a water body of its own. A polygon whose shore lies off the DEM, with no level
given, stays on the ground, and a notice says how many. Shores are read every half DEM cell, up
to about 4 million heights in all (and one a ring); when the water layer's shores together are
longer, all of them are read at one wider step, and a notice says how
many polygons were. A layer read a view at a time (in chunks, by range) can't be the water. `{"water": false}` takes the water off;
left out (or `null`), it stays as it was. The water layer is kept by name, follows its renames, and can stay
hidden; its own symbology still draws it. A project that opens with a water layer that no longer holds
its `"level"` field as a number (the layer's data replaced outside RustGIS, a field renamed other than
only in case, or turned to text) clears it, as deleting the field does, and says so: its lakes lie at
their shores' lowest heights.
So does the block's water `"field"`: its walls show all the water at the block water's `"depth"`, or no
water without one. Rivers stay lines. Over elevation tiles, the levels are
read again until the tiles along the shores have come. A Shaded Relief layer draws the water's
cells at their level with the even light of flat ground, flat or tilted: no hillshade, cast
shadow, sky, fill light or rock on the water, so a semi-transparent water fill over it shows one
tone. The relief's ramp colours them at the water's level. A tilted view adds a soft sun glint:
where the flat water would reflect the sun toward the eye, it goes toward white, broad and faint,
so it shines a little on the sun's side. `"glint"` sets how much, 0 (none) to 1 (white at the
brightest point), 0.3 by default. The sun is the first Shaded Relief layer drawn at the view's centre (its azimuth and altitude; shown, within its scale range and its groups'), or from the north-west at 45° without one. A flat view, seen from straight
above, has no glint. The water is darker along its shore, flat or tilted: a band a few pixels
wide inside each lake's outline and around its islands, drawn over the water layer's fill,
toward black at the outline and fading inward, as relief artists draw the edge of a lake.
`"shore"` sets how dark it is at the outline, 0 (none) to 1 (black), 0.35 by default. It is
drawn with the water layer, so it shows only where that layer is drawn.

<figure markdown="span">
  ![Mount Rainier in USGS imagery draped over Terrarium elevation tiles, tilted 65°](https://assets.rustgis.app/images/terrain.webp)
  <figcaption>Mount Rainier: USGS imagery draped over Terrarium elevation tiles.</figcaption>
</figure>

**Export Orbit** (`map.exportOrbit`, on desktop) writes a turn of the view about its centre as an
animation: a tilted map with relief reads best when it moves. It turns the bearing by `"turn"`
degrees (default 360, once round; negative turns the other way) over `"frames"` frames (default
60), keeping the centre, the scale and the pitch, and draws each frame as Export Map draws the
view, at `"width"` × `"height"` points (default 640 × 400) and `"dpi"` (default 96). A path
ending in `.gif` gives an animated GIF that loops, each frame shown for `"speed"` milliseconds
(default 50; a GIF keeps times to the hundredth of a second, so 55 shows as 50 or 60); one ending in `.zip` gives the frames as PNGs (`frame_0001.png`…), to build a video
with another tool, since a GIF's 256 colours can band a smooth relief. A turn of 360° stops one
step short of where it began, so the loop has no frame twice; a part of a turn ends on its last
bearing. The light of a shaded relief stays with the ground, not the camera. The frames are drawn
from a copy of the map, so the view doesn't move. The frames count against a limit of 300 million
pixels in all (170 frames at 1600 × 1100, say), and a size past it is refused.

In the desktop app, **View › Scene › Export Orbit…** opens a dialog with the frames, the turn,
the size and a GIF's frame time (an empty box keeps the default); **Export…** checks them and
only then asks where to write the file. The frames are then drawn in the background, so the window stays free: the status bar
shows the export, how many frames it has drawn, and **Cancel**, which stops it between frames
and writes nothing. From a script or the command line the command runs to its end before it
answers:

```bash
rustgis-cli run --cmd 'project.open={"path":"massif.rgis"}' --cmd 'map.setPitch={"pitch":60}' --cmd 'map.exportOrbit={"path":"massif.gif","frames":72,"width":800,"height":500}'
```

**Export Block** (`map.exportBlock`, on desktop) writes a map's block diagram as a 3D model, for
Blender, a web viewer or a 3D printer's slicer: `{"path": "block.glb"}` gives binary glTF, one
file; a path ending in `.gltf` gives the same model as glTF's JSON with everything embedded. The
model has three meshes: **Ground**, the ground inside the block's box on the grid the settled view
lays it on, with the map drawn top down over the box as its texture (its layers, symbology and
labels, without the scale bar or the credits, at `"texture"` pixels on the box's longer side,
default 4096, 256 to 8192); **Walls**, the box's four sides from the ground's edge down to the
base, in the walls' colour; and **Base**, flat at the block's base. The texture holds no light:
the 3D viewer lights the model, so a hillshade layer shown on the map is the one light baked in.
Units are metres east (x), up (y) and south (z) of the box's south-west corner, scaled as at the
box's centre, with heights above sea level times the exaggeration; the file's `extras` give the
corner's longitude and latitude. The view's sky, fog and shadows aren't in the model, and walls
coloured by a polygon layer come in their plain colour for now (said). In the desktop app it
runs in the background, with the export and **Cancel** in the status bar (elevation tiles are
waited for, up to a minute); from a script or the command line it runs to its end before it
answers:

```bash
rustgis-cli run --cmd 'project.open={"path":"massif.rgis"}' --cmd 'map.exportBlock={"path":"massif.glb","texture":4096}'
```

MapLibre map links carry a view in their address, as
`#zoom/latitude/longitude/bearing/pitch` (`https://…/#15.5/40.7484/-73.9857/30/60`). **Go To Map
Link** (`map.goToLink {"url": …}`) takes such a link, or the part after its `#`, to that place,
zoom, bearing and pitch, and `map.link` gives the current view the same way. The web app reads
one from its own address (see [Installation](../installation.md)).

**Manage Bookmarks** (at the foot of the Bookmarks menu, or **View › Windows › Bookmarks**) opens
the Bookmarks pane: every map's bookmarks, grouped by map (the map in view in the accent colour),
with a box that filters them by name or description. On each bookmark, **Zoom To** shows its place
at its scale and **Pan To** centres it at the current scale; a bookmark of another map takes the map
in view to the same place. **Update** saves the current view into it (only on its own map, in
view), and **Delete** removes it. The chevron shows its description: click it to write or edit it.
Double-click a name to rename it, and right-click it to rename, describe, update, move it up or
down, or delete it. Bookmark edits run `bookmark.*` commands and undo with ++ctrl+z++;
**Zoom To** and **Pan To** change the view without adding an undo step.

**Home › Navigate › Locate › By Place or Address…** (`map.findPlace`) looks up a place or an
address by name with OpenStreetMap's [Nominatim](https://nominatim.org) (or a
[Photon](https://photon.komoot.io) server, PDOK's
[Locatieserver](data.md#geocoding-addresses) for Dutch addresses and places, the U.S. Census
Bureau for U.S. street addresses (desktop/CLI, or a CORS-enabled proxy on the web), or a geocoder of your
own given by its address) and zooms to the one picked. The search goes to that server over the network; see
[Geocoding addresses](data.md#geocoding-addresses) for whole tables.

The **search box** at the foot of the left pane searches the map as you type: a latitude and
longitude (`40.7, -74.0`, `40.7N 74W` or `40°42'46"N 74°0'21"W`; latitude first unless the
letters or a number past ±90 say otherwise), a UTM position or an MGRS reference (see
[Coordinate readouts](#coordinate-readouts)), the map's layers and bookmarks by name, and the
values in its layers' attribute tables (the best few per layer, by the feature's name where it has
one). Places come from Nominatim when you press ++enter++ on **Search places**, since the public
servers don't allow searching as you type. The arrow keys move through the list and ++enter++
picks: a layer is zoomed to, a feature selected and zoomed to, and a coordinate or a place is
marked with a pin until the box is cleared (✕ or ++esc++). `map.search` runs the same search from
the command line or a script, and goes to a hit with `"go"`:

```
rustgis-cli run --sample --cmd 'map.search={"query":"Tokyo","go":true}'
```

**Right-click the map** for a menu about that place. Its first row shows the place in the status
bar's coordinate format (latitude and longitude as `40.712800, -74.006000` for decimal degrees);
click it to copy it. North of 84° N and south of 80° S it gives the polar grid, UPS (see
[Coordinate readouts](#coordinate-readouts)). On the map of another body than the Earth it shows
and copies the latitude and longitude, and its hover text says why. Below it, **Copy As GeoJSON**
copies the place as a point feature, **Center Map Here** and **Zoom In Here** move the view, and **Directions To Here** and
**Directions From Here** open the [Directions](#directions) pane. **Quick Analysis** runs a tool
at the place: a buffer of 500 m, 1 km or 5 km (¼, 1 or 5 miles when the Directions pane is set
to miles; `gp.bufferPoint`), a viewshed of 2, 5 or 15 km or a line of sight on an elevation
raster in the map, and **Open In Geoprocessing…** opens the Buffer tool. **View In Google Maps**
and **View In Google Earth** open the place in your browser at about the same zoom.

### Coordinate readouts

The status bar shows the pointer's coordinates while it is over the map, and keeps the last
position (dimmed) when it leaves, so you can move to the status bar and click it. It reads `—`
until the pointer has been over the map, and again after you switch to another map or open another
project. Click the coordinates to step through four formats; the choice is remembered and also
sets the right-click menu's first row. The same place in each:

| Format | Example | Notes |
| --- | --- | --- |
| Decimal degrees | `15.3000° E  49.8500° N` | four decimal places |
| DMS | `15°18′00.0″ E  49°51′00.0″ N` | degrees, minutes and seconds |
| UTM | `33north 521567 5521996` | zone and hemisphere (`33south …` south of the equator), easting and northing in metres, rounded to the metre |
| MGRS | `33U WR 21566 21996` | zone (two digits: `04Q FJ …`) and band, the 100 km square, easting and northing within it, truncated to the metre |

UTM states the hemisphere, as GeographicLib's `GeoConvert -u -l` writes it, not the latitude band:
in plain UTM a letter after the zone is commonly read as the hemisphere, so `38S` (band S, 32° N
to 40° N) would be taken for the southern hemisphere, 10,000 km away. Latitude bands are MGRS's.
UTM and MGRS are on WGS 84, with the zone exceptions for southern Norway (32V) and Svalbard (31X,
33X, 35X and 37X), and cover 80° S up to 84° N. North of 84° N and south of 80° S the readouts
use the Universal Polar Stereographic grid (UPS), as NGA defines it (NGA.SIG.0012): the UTM
readout gives the zone spelled out and the easting and northing rounded to the meter,
`north 2077146 1562481`, as GeographicLib writes it, and the MGRS readout gives the polar
reference, which has no zone number: a letter for the half (Y for western longitudes and Z for
eastern ones in the north, A and B in the south, with the 0° and 180° meridians in Z and B),
the 100 km square and the digits, truncated, `Z AC 77146 62481`. 84° N itself is UPS, 80° S
itself is UTM. On the map of another body than the Earth the readout shows *No UTM here*
(or *No MGRS here*) rather than a wrong reference, and degrees still read. Scripts set the format with
`ui.coordinateFormat={"format":"utm"}` (`decimal`, `dms`, `utm` or `mgrs`); without `format`
it steps to the next one. A saved format this version doesn't know, say from a newer RustGIS,
reads as decimal degrees with a notice, and stays saved until you pick a format.

`map.convertPosition` gives a place in all of these forms at once, without moving the map, for
scripts, Python and the AI Assistant. It takes the place as `map.goToXY` does: `lon` and `lat`
(numbers, or degrees, minutes and seconds), or a `position` written in any form the search box
reads, and says what it assumed the same way. It answers with `lon` and `lat`, `decimal` and
`dms` as the readout writes them, `grid` (`utm`, or `ups` in the polar areas), the reference in
`utm` or `ups` (whichever `grid` names; the other is `null`), the UTM `zone` and latitude `band`
(`null` in UPS), `hemisphere`, `easting` and `northing` in meters, and `mgrs` spaced as the
readout writes it and `mgrsCompact` in one word. `digits` (0 to 5, 5 by default) sets the MGRS
precision, from 100 km to 1 m. As in the readouts, the `utm` and `ups` text round to the meter
while MGRS truncates, so the two can differ by a meter; `easting` and `northing` give the place
to the millimeter (truncated). Every key is always there, `null` where it has no value; on
another body's map the grids are `null`, and `note` says why.

```
rustgis-cli run --cmd 'map.convertPosition={"position":"18TWL8395907350"}'
```

```python
s.run("map.convertPosition", {"lon": -74.006, "lat": 40.7128})["utm"]  # '18north 583959 4507351'
```

The [search box](#navigating) takes the same grid references, in any case (polar references have
their own rules, below):

- **MGRS** with or without spaces: `33UWR2156621996`, `33U WR 21566 21996`, `33UWR 2156 2199`,
  or a 100 km square alone, `33UWR`; a zone below 10 with or without its leading zero (`04QFJ`,
  `4QFJ`). The digits are an even count from 0 to 10, half for the
  easting and half for the northing, and the search goes to the middle of the square they name.
  A 100 km square is also looked for in the attribute tables, since Sentinel-2 tiles are named
  after them.
- **UTM** as the readout writes it, zone and hemisphere: `33north 521567 5521996`,
  `21south 371625 6170423` (also with a space, `33 north …`); or zone and latitude band:
  `33U 521567 5521996`. `33N` reads as the northern hemisphere when the northing isn't in band
  N, since both readings agree.
  `S` after the zone is always band S (32° N to 40° N), as in `11S 384629 3768405` (Los
  Angeles); with a northing outside band S it could mean the southern hemisphere, so it finds
  nothing: write `south` instead.
- **UPS and polar MGRS**, north of 84° N and south of 80° S, which have no zone number: UPS as
  the readout writes it, `north 2077146 1562481` or `south 2096454 1452981`, or with the half's
  letter, `Z 2077146 1562481` or `Y, 1735147, 2243078`. Polar MGRS has no zone number, so three
  letters and a few digits look like a word or a code (`ATL`, `Bay 12`, the flight `BAW1234`);
  search reads one only as a reference is written: as the readout writes it, with a space after
  the half's letter and the square in capitals, then no digits or two equal groups (`Z AC`,
  `Z AC 77 62`, `Z AC 77146 62481`; `A ST 12` is an address), as
  one word with at least six digits (`ZAC7714662481`, 100 m or finer), or in capitals before
  two equal groups of at least three digits (`ZAC 771 624`, `ZAC 77146 62481`). A UPS position
  (`north 2077146 1562481`, `Z 2077146 1562481`) and a reference written exactly as the readout
  writes it (`Z AC 77146 62481`) are plainly places. Any
  other polar text may also be a name or a code, so the attribute tables are still searched,
  their hits come before the polar place, and **Search places** is still offered; with room
  for one hit (`limit: 1`), a matching feature is the hit. A UTM zone's position past 84° N or
  80° S, or a polar letter after a zone number (`38ZMB`), finds nothing and the list says how UPS
  is written.

A reference that doesn't exist (square letters the zone doesn't use, a northing or a square
outside the band, an odd number of digits, a place outside the zone) finds nothing rather than a
place somewhere else, and the list says why; `map.search` returns the reason as `gridError`, and
adds `"coordinateMayBeText": true` when a polar place among its hits may as well be a name or a
code. A 100 km square written with spaces, such as `5 Ave`, `4K TV` or `A TO`, is as likely
ordinary text, so one that names no square gives no reason; written as one word, `38TMJ`, it does. Polar
MGRS written compactly (`ZDA1234512345`) may be a code too, so one that names no place gives no
reason either; written as the readout writes it (`Z DA 12345 12345`), it does. A UPS position
after `n` or `s` may be a State Plane pair (`N 1350000 1350000`), so it gives no reason; after
`north`, `south` or the half's letter (`Y 2392768 1607232`), it does. On the map of
another body, grid references find nothing.

### Directions

**Directions** (Home › Navigate) plans a route between places and follows it turn by turn, with
the public [Valhalla](https://valhalla.github.io/valhalla/) server run by
[FOSSGIS](https://www.fossgis.de) on OpenStreetMap data. Pick **Drive**, **Bike** or **Walk**, then
set the start and the destination: type an address (++enter++ looks it up with Nominatim) or a
latitude and longitude, press the ⌖ button and click the map, or right-click the map and choose
**Directions From Here** or **Directions To Here**. **+ Add Stop** adds places on the way (up to
20 in all), **Swap** reverses the trip, and a drive can avoid tolls, highways or ferries; **Miles**
gives distances in miles and feet.

Routes are drawn as soon as every place is set. Between two places the server offers up to two
other ways, drawn grey: pick one in the list or click it on the map. The steps list each maneuver
with its arrow, instruction and distance; click one to zoom to it. **Simulate** replays the route
as a drive the map follows, with the next maneuver and its distance in a banner across the top of
the map, at 1× to 16× speed (pause, drag the slider to jump along, **End** to stop). **Save as
Layer** adds the route as a line layer, and its maneuvers as a point layer, to style, export
(GPX, GeoJSON…) or keep with the project.

The first time, the pane asks before anything is sent: planning a route sends its places to the
routing server, and looking a place up sends its name to Nominatim. Travel times don't include
live traffic, and the public server is shared, so it answers light use only.

`map.route` plans a route from the command line, a script or the AI Assistant, with places given
as `[lon, lat]`, `"lat, lon"` or a name to look up, and adds it and its steps as layers;
`ui.pane.directions` with `from`, `to` (and `via`, `mode`) opens the pane on a route. `map.route`
sends its places to the same public servers (names to Nominatim, at most five a call, a second
apart; coordinates to FOSSGIS's Valhalla), so the AI Assistant asks before running it:

```
rustgis-cli run --sample --cmd 'map.route={"from":"Brandenburg Gate, Berlin","to":"Alexanderplatz, Berlin","mode":"bike"}'
```

On the web, plan routes in the Directions pane; `map.route` cannot fetch a route or look up
place names there. On the desktop, its optional `url` selects another Valhalla server.

## Basemaps

<figure markdown="span">
  ![A MapLibre style from OpenFreeMap drawn over Paris, with the Basemap gallery listing the built-in basemaps and the web tile layer](https://assets.rustgis.app/images/tiles.webp)
  <figcaption>An OpenFreeMap MapLibre style over Paris, and the Basemap gallery.</figcaption>
</figure>

**Home › Add › Basemap** offers five built-in basemaps drawn from a world outline (Paper, Midnight,
Ocean, Atlas, Blank), which need no network, and web tile layers over them. On Blank, **Paper
color** under the basemaps sets the colour the map draws where it would draw white, a tilted view's
sky included, so a map frame sits on a coloured layout page without a white box around it
(`map.basemap {"name": "blank", "color": "#F1EADF"}`). Choosing another basemap takes the paper
colour off, as that basemap draws its own colours:

- **Raster tiles** from an XYZ or TMS template (`https://…/{z}/{x}/{y}.png`; `{-y}` for rows
  from the south, `{s}` for subdomains, `{q}` for Bing-style quadkeys) or a WMS through
  `{bbox-epsg-3857}`. PNG, JPEG and WebP.
- **Vector tiles** (Mapbox Vector Tiles, `.pbf`) from a template or a TileJSON address, drawn in
  a built-in style that knows the OpenMapTiles, Protomaps and Shortbread schemas.
- **PMTiles archives** (`.pmtiles`) of raster or vector tiles: a web address, read a tile at a
  time by byte ranges (the server must allow range requests, and CORS for the web app), or on the
  desktop a file (**Tile Archive File…**, Add Data, or drop it on the window). Version 3
  archives are read, uncompressed or with gzip, Brotli or Zstandard. The map moves to what the
  archive covers once its header is read, unless it covers the whole world, and right-clicking
  its row in Layers gives
  **Zoom To Layer**. A vector archive that is a basemap is drawn in the built-in style; any other
  is drawn with each of its layers in a colour of its own.
- **MBTiles files** (`.mbtiles`) of raster or vector tiles, drawn as PMTiles archives are: on
  the desktop read in place a tile at a time (**Tile Archive File…**, Add Data, or drop it on the
  window); in the web app added with Add Data and held in memory for the session (a project keeps
  the layer, and asks for the file again).
- **Map styles**: a MapLibre style document (`style.json`), with its vector, raster and
  elevation (`raster-dem`: Terrarium, Mapbox or custom encoding) sources.
  RustGIS draws background, fill, line, circle and raster layers, labels with their icons from
  the style's sprites (SDF icons in their `icon-color`; the icons of a sprite in the style's
  list are named `<sprite id>:<name>`, those of a sprite with the id `default` just `<name>`), and
  fill patterns; it evaluates the style's expressions and zoom functions, `image` among them (an
  icon's name when a sprite has it, so `coalesce` falls back to the next). When the style draws
  images, a notice says why any are missing: a sprite at an address RustGIS won't fetch, an entry
  of the list that can't be read, or a sprite whose download failed, which loses only its own
  icons and patterns and is tried again. From elevation it draws `color-relief` layers (a
  `color-relief-color` that is an `interpolate` or `step` on `["elevation"]`) and `hillshade`
  layers (each `hillshade-method`, lit as the map is drawn, north up; slopes are steepened at
  small scales so relief stays visible). Its shading follows the style's colours, exaggeration
  and light rather than matching MapLibre's pixel for pixel. Other `color-relief-color`
  expressions aren't drawn, and a notice names the layers skipped. If a style's elevation source can't be read, its relief and
  hillshade layers are left out with a notice saying why, and the rest is drawn. Labels are placed
  as MapLibre places them: at points,
  in the middle of areas, and along lines (street names every `symbol-spacing`, turned to the
  street, where it bends less than `text-max-angle`; shields and one-way arrows), wrapped to
  `text-max-width`, the style's later layers first and then by `symbol-sort-key`, kept apart by
  `text-padding` and the `*-optional`, `*-allow-overlap` and `*-ignore-placement` rules. Text
  along a line is drawn straight rather than curved round its bends. Line patterns and 3D
  extrusion are not drawn (extrusions draw flat). A label shows the lines of its text
  that the map's font can draw: a style's "Syria / سوريا" shows as "Syria".
  A style's root `center`, `zoom`, `bearing` and `pitch` say where it opens: `map.addTiles` with
  `"view": true` takes the map there once the style has loaded.

Present PMTiles metadata must be complete, decodable and a JSON object; malformed metadata or a
stored metadata section over **16 MiB** refuses ordinary loading, repair and reload rather than
silently losing credits or source-layer information. RustGIS's PMTiles writer enforces the same
metadata limit. This is not a limit on the archive's total size; absent metadata remains supported.
MBTiles metadata names and values must be text: unreadable or NULL metadata cells also produce an
error rather than being silently skipped. Native MBTiles loading, repair and reload refuse more
than **1,000 metadata rows** rather than silently omitting later rows.

HTTPS tiles use [platform certificate trust](data.md#https-certificates) on desktop and the
command line, including private CAs trusted by the operating system. The web app uses the browser.

Tile addresses are checked after filling their placeholders, before cached bytes or network
requests are used. In the native app, a document on the internet cannot select a local
destination IP. Explicitly selected self-hosted servers remain supported, including ordinary
hostnames resolved by local DNS, with local fallback when the same hostname also returns
public addresses. Native fetchers filter the resolved IPs before connecting,
using the supplying document's actual connection reach. Native requests refuse
link-local and unspecified addresses, including those represented by IPv4-mapped IPv6
addresses or the well-known NAT64 prefix. Public IPv4 destinations under that prefix can pass.
Document redirects also change the base for relative references.
IPv4 decoding covers mapped addresses and the well-known `64:ff9b::/96` prefix. It does not
inspect destinations behind 6to4 tunnels or custom translation gateways, including the
[RFC 8215](https://www.rfc-editor.org/rfc/rfc8215#section-5) `64:ff9b:1::/48` prefix, whose
translation is network-specific. Such routes can hide a private or link-local IPv4 destination
behind a public-looking IPv6 address; restrictions on those routes require network firewall policy.
Native redirects are checked at each hop. In the web app, use the final service address:
tile, style, TileJSON and sprite redirects, including HTTP-to-HTTPS upgrades and CDN hops,
are refused because the browser hides their destinations from the app. The same browser
transport refuses redirects for downloads, fonts, archive range reads and HEAD requests for
file metadata; see
[network downloads](data.md#network-downloads).
The web app checks the URL's spelling and normalized address, but cannot inspect resolved IPs.
A public-looking hostname can reach a private or link-local server if the browser permits it;
DNS resolution, local-network permissions and certificate trust are controlled by the browser.
Native tile and document requests, and file downloads through **From URL…**,
connect directly without an environment HTTP proxy, so a proxy cannot resolve destinations
beyond these checks. See [network downloads](data.md#network-downloads) if your network requires a proxy.

The gallery's **Web Basemaps** need no API key: the OpenFreeMap vector maps (Liberty, Bright,
Positron, Dark, Fiord) and USGS Imagery. Choosing one replaces the other web basemaps; tick
**Stack on the current basemaps** above them to add it over the others instead (clicking one
that already shows then takes it off). Toggling the checkbox keeps the gallery open so you can
choose a basemap; choosing one closes it. For many
more (OpenStreetMap, Esri, NASA and others), follow **Install the Basemaps plugin** under them,
which opens the Plugin Manager at the
[Basemaps plugin](https://plugins.rustgis.app/catalog/basemaps/); it adds a tab of public
basemaps. Add any other with **Add Tile Layer…** at the bottom of the Basemap gallery.
The gallery lists the map's tile layers: show or hide them, change their opacity and order, or
remove them. The Layers pane lists them too, at their place in the drawing order: a basemap
under the map's layers and above the built-in basemap; an overlay (a WMS or WMTS layer, a vector
tile service, **Overlay** ticked in Add Tile Layer) on top of the map's layers when added, as
every new layer goes. Drag one, or right-click it and choose **Move up** or **Move down**, to put it
between the map's layers (above a layer group, never inside one). Tick
one to show or hide it, and right-click it for **Zoom To Layer** (when its bounds are known), opacity,
**Blend** mode, order and **Remove**; a WMS or map service layer also has **Draw as**: one picture
of the view or tiles of a size (see
[One picture of the view, or tiles](data-formats.md#one-picture-of-the-view-or-tiles)).
A blend mode (Multiply, Screen, Overlay, Darken, Lighten, Difference) combines a tile layer with
what is drawn beneath it (tile layers, the basemap, and map layers when it's placed over them): stack imagery over a hillshade and set it to Multiply to
let the terrain show through. Overlays stay over the basemaps; choosing a built-in basemap
removes the web basemaps but keeps the overlays. Tile layers are saved with the project, and
their credits are shown in the corner of the map and drawn on exports. A layer's own credit
(`attribution`) stands in place of its tiles' (a TileJSON's or an archive's), which show when it
names none; a MapLibre style keeps its sources' credits beside it.

Use Explore to click features in vector PMTiles, MBTiles or MVT layers added as data and see
their source-layer names and attributes in a pop-up. This works with **Topmost Layer** or
**Visible Layers** pop-up mode, not with catalog basemaps or MapLibre styles; with **Selected in
Layers**, click a tile layer's row to select it and a click on the map identifies that layer
alone, whichever it is (a WMS layer too). Tiles are not
selectable or editable; a tile not yet loaded may need another click once it arrives.
**Reload** in the Basemap gallery (`map.reloadWebLayers`) clears cached tiles and styles and
fetches them again.

### Reloading a local tile archive (desktop)

After changing a PMTiles or MBTiles file on disk,
right-click its row in Layers and choose **Reload Data** (**Reload Tile Archive** in Command
Search). CLI, Python, MCP and the Assistant use
`map.reloadTiles={"index":0}`; `"name"` or catalog `"id"` can select the archive instead.
No replacement path is accepted. The desktop app also watches the archive (and an MBTiles file's
`-wal` and `-journal`) and marks its row **(changed on disk)** with a notification offering
**Reload Data**; see [Changed on disk](index.md#reload-data). RustGIS doesn't reload it until you choose.
Reload rereads that archive's metadata and tiles without clearing other archives or web layers.
The layer keeps its name, visibility, opacity, blend, drawing order, source-layer visibility,
style and other presentation settings; the map's view stays where it is. Undo cannot restore
file contents that another program has overwritten.

On the desktop, menu actions and direct attached MCP `run_command` calls validate reload and
repair on a background worker; the status bar shows the work and **Cancel**. Only one read runs
at a time. If its project, map, archive or presentation changes before it lands, the result is
left unapplied: retry on the intended target. A paused attached batch also rejects its remaining
commands if the project or selected map changes. Notebook cells, task steps, scripts (the Script
pane and attached MCP `run`) and file-bridge scripts validate inline on the window thread, without
reload status or cancellation during validation.
Cancelling MBTiles validation also interrupts SQLite queries used to infer missing zoom metadata.
It cannot preempt a filesystem operation blocked inside the operating system.

Approval binds the selected archive's owning map, stack position and source route, not just its
name or index. Retargeting or reordering it requires reviewing the operation again.

Each operation observes file size and modification time before validation, checks them again
after validation and immediately before applying the result, and rejects observable changes.
It also checks file identity and change time where the platform exposes them, plus MBTiles
`-wal` and `-journal` existence and revisions (not the reader-coordination `-shm` file).
Metadata that cannot be read is an error, not permission to apply an unchecked result.
These checks are not an immutable snapshot: a rewrite that leaves the same observations, or a
write after the final check, may escape detection. Finish writing before reloading or repairing.
If an archive is reloaded during a map or layout export, the export refuses a mixed-version
result. Wait for the reload to finish and retry the export.

Reload first checks that the file can be read. If it is missing, unreadable or invalid, a notice
explains the failure and the existing layer and retained cached data are left unchanged.
Finish writing or restore the file, then choose **Reload Data** again. If the file has moved,
choose **Repair Data Source…** instead and select its new location. Repair also checks the
replacement before changing the source; cancelling or choosing an unreadable or invalid archive
leaves the existing source and layer unchanged, so you can retry with a valid file.

Validation reads one size-bounded raster sample from a fresh MBTiles connection and decodes it
before dropping retained caches. PMTiles validation checks all reachable index directories, not
every tile payload. It allows at most **64 MiB stored and 64 MiB decoded index bytes**, **1,048,576
index entries**, **4,096 unique directories** (including the root), and **four leaf levels**;
each stored directory read is limited to **16 MiB**. An invalid index or an exceeded validation
budget is an explicit error without eviction, even if an unusually large index is otherwise valid.
Validation also refuses a stored tile range over **16 MiB**, or one outside the header's declared
tile-data section (including a zero-length section with nonempty tile entries). Ordinary loading
can still draw other tiles from such an archive; reload and repair require the replacement's
entire index to pass these checks before changing the source or dropping cached data.

Neither action is available for local archives in the web app, or for remote tile services.
On Windows, an open MBTiles file may not be replaceable while RustGIS holds it open: reload
does not bypass that operating-system restriction. Close RustGIS before replacing the file,
then reopen the project, or write to a different path and use **Repair Data Source…**.

**Source layers.** A vector PMTiles or MBTiles archive lists its source layers (the
`vector_layers` its metadata names: `water`, `roads`, `pois`…) under its row in the Layers pane:
the arrow beside it opens them, each with its own checkbox. Untick one to hide what it draws
(fills, lines, points and labels, in the default look and in a style given with `styleLayers`
alike); Explore's pop-ups then skip its features too. From scripts, `map.tiles` lists a vector
archive's `sourceLayers` once its header is read, and `map.setTiles` takes `"hiddenLayers":
["roads", …]`, the source layers switched off (`[]` or `null` shows them all again). Switching one
off is an edit to undo, and a project keeps which are off.

Tiles load in the background and the map fills in as they arrive, with coarser tiles standing in
meanwhile; exports and `rustgis-cli` wait for them. On the globe and in other projections the
tiles are reprojected. From scripts: `map.addTiles`, `map.removeTiles`, `map.setTiles`,
`map.zoomToTiles` and `map.tiles`. `map.addTiles` accepts optional `"bounds": [west, south, east,
north]` in longitude/latitude; `service.add` retains the bounds advertised by map and tile services.
Explicit bounds must form a finite, ordered geographic rectangle; an invalid value rejects the
command without adding or replacing a layer. Invalid or malformed bounds in a saved project
are instead discarded, leaving the project and tile layer usable without that coverage metadata.

**Legends.** A WMS or WMTS layer added from its service has its legend in the Layers pane, as
ArcGIS Pro's Contents pane shows a map service layer's: the arrow beside it opens the legend under
the layer, indented as a feature layer's classes are, at the size the service draws it (one wider
than the pane scrolls across). A WMS layer's legend is the one its service draws from the layer's
styling (GetLegendGraphic, when the service offers it in PNG or JPEG), else the picture of its
default style (its `LegendURL`: the style named `default`, else its first, inherited from the
layers above it as WMS has it); when one can't be shown the next is. A WMTS layer's comes from
its default style, which may give one per range of scales: the one for the map's scale shows,
and "No legend at this scale" where none is for it. The pictures are fetched when first shown
and kept (one that couldn't be fetched is asked for again a minute later). Any tile layer can be given one: `map.addTiles` and `map.setTiles` take
`"legend"` (an address, or a list of addresses and `{"url", "minScale", "maxScale"}`; `null` takes
it away), and `map.expandTiles` shows or hides it (as `layer.expand` does a layer's classes: not
an edit to undo). A project keeps a layer's legends and whether they show.

```sh
rustgis-cli run --cmd 'service.add={"url":"https://wms.geo.admin.ch/","layer":"ch.swisstopo.geologie-swissgeocover2d_lines"}' --cmd 'map.tiles={}'
```

### Other worlds

<figure markdown="span">
  ![Mars on the 3D globe, on the Viking mosaic, with Valles Marineris across the middle](https://assets.rustgis.app/images/mars-globe.webp)
  <figcaption>Mars on the globe, on the Viking mosaic: Valles Marineris crosses the middle.</figcaption>
</figure>

A map can be on the Moon, Mars or another body instead of the Earth. Pick the body from the
planet button at the top of the Layers pane, or under **Celestial Bodies** in the Basemap
gallery: Earth, Moon, Mars, Callisto, Charon, Europa, Ganymede, Io, Mercury, Pluto, Titan and
Venus. Choosing one closes the gallery and puts one of its basemaps under the map (reopen the
gallery for the others: Viking, MOLA elevation and hillshade mosaics for Mars, LOLA albedo for
the Moon, and the USGS Astrogeology mosaics of the rest) and measures on that body: the Measure tools, the scale bar,
the map scale and layout scale bars use its radius. Coordinates stay longitude and latitude, and
the map stays a Web Mercator sphere, the size of the body. Off the Earth there is no atmosphere:
the globe has no glow around it, and a tilted view's sky is the black of space, with no haze
toward the horizon. Choosing the Earth replaces all non-overlay web tile layers, including
planetary and custom basemaps, with **USGS Imagery**, keeping overlay tile layers. The body is
saved with the map.

The basemaps come from [OpenPlanetaryMap](https://www.openplanetary.org/opm) and
[USGS Astrogeology](https://astrogeology.usgs.gov/). OpenPlanetaryMap's mosaics sit in storage
that browsers can't read from another site, and USGS serves its mosaics in lon/lat only, so both
are read through the opengeos tile service (`tiles.geolibre.app`, shared with GeoLibre), which
adds what browsers need, reprojects the USGS mosaics and caches the tiles. From scripts:
`map.body={"name": "mars"}` (`"basemap"` picks another of its basemaps, or `false` adds none, still
taking other bodies' basemaps off) and `map.bodies`.

<figure markdown="span">
  ![The Moon's near side on the LOLA hillshaded albedo basemap, its maria dark among the cratered highlands](https://assets.rustgis.app/images/moon.webp)
  <figcaption>The Moon's near side on LOLA's hillshaded albedo.</figcaption>
</figure>

### Worldview

The world outline and the World Atlas sample's countries show Natural Earth's default (de facto)
borders, which are built in, or the borders as one of 31 countries sees them: Natural Earth's
[points of view](https://www.naturalearthdata.com/blog/admin-0-countries-point-of-views/), from
Argentina to Vietnam. Choose one in **Settings › Language › Worldview**, set `RUSTGIS_WORLDVIEW`
to its code (such as `chn`, `ind` or `usa`), or pass `--worldview` to `rustgis-cli`.

A point of view isn't part of the app: it is downloaded from
[source.coop](https://source.coop/opengeos/data/rustgis/worldviews) when you choose it, and the
desktop app and the command line keep it in the cache folder, so it is downloaded once; the web
app downloads it again in each session. **Detail**, at the top of the same menu, sets how much:

- **Simplified** (the default): borders at 1:110m, like the built-in ones; about 250 KB.
- **Original**: Natural Earth's 1:10m borders with every island and territory; a 3 MB download,
  10 MB once unpacked, so maps with it draw more slowly. On a flat Web Mercator map the
  basemap's land outline is drawn simplified to within a pixel at each zoom, which keeps most of
  that cost away from it; the globe and other projections draw it in full. With the default
  worldview, this downloads the detailed version of the built-in borders.

`rustgis-cli` takes `--worldview-detail original`. The map keeps the borders it shows until the
download arrives, and says so if it fails. Until
you choose one, RustGIS shows China's view when the system's time zone is mainland China's
(such as `Asia/Shanghai`) or its first locale is `zh-CN`, downloading it the same way, and the
default borders everywhere else. Your choice is kept for the next start. An untouched sample
project switches its countries when the new borders arrive; others keep the layer they have.

## Projections

The flat map is drawn in Web Mercator by default. **View › Projection** switches it to another
projection, and clicking the coordinate system in the status bar opens the **Coordinate
System** dialog, which also takes a projected EPSG code (such as `3035`, `32633` for UTM zone
33N or `2056` for Switzerland) or an ESRI code, the one `.prj` files and ArcGIS name (such as
`ESRI:102033` for South America Albers Equal Area Conic, or `ESRI:54030` for World Robinson). A
code whose projection method RustGIS can't compute is refused with the reason, rather than drawn
in another projection. The codes are those of the EPSG registry v13.102 and PROJ 9.9.0's ESRI
codes, so a system added to EPSG recently (such as `11476`, SIRGAS-Chile 2025 / UTM zone 19S) is
there too. Projections and datum transformations are computed by
[terrio-crs](https://github.com/opengeos/terrio), as PROJ 9.9.0 computes them. On the desktop, the
command line, Python and the phone apps, datum shift grids are read from RustGIS's own grid folder
(`datum-grids` in its app data), then from those PROJ and terrio use on the computer
(`TERRIO_GRID_DIR`, `PROJ_DATA`, `PROJ_LIB`, PROJ's user folder, terrio's download cache), so grids
installed for PROJ count; a grid a definition names by its path is never read. A datum whose most
accurate transformation needs a grid that isn't there takes the next PROJ lists (a NAD27 code
without `us_noaa_conus.tif`: up to about 10 m off), and says so once.

**Settings › Datum Grids › Download Datum Grids** (off by default, in the desktop and web apps)
downloads such grids from PROJ's grid server (`cdn.proj.org`; only PROJ's openly licensed GeoTIFF
grids, not the server's other files such as deformation models):

- On the desktop, a file read then waits for its systems' grids before its features are placed, up
  to two minutes in all, and stops waiting when you cancel it; the download goes on. In the web
  app a read doesn't wait: a layer read before its grid arrives keeps its places until it is
  opened again.
- Rasters, streamed layers and the map follow a grid as soon as it arrives. A layer opened before
  keeps its places until it is opened again.
- Each download is said, with its size, and so is each that fails (not tried again until the
  setting is turned off and on). An answer that isn't a grid file is refused.
- Turned off, grids already downloaded stay in use, and downloads already under way finish. On a
  computer, delete the `datum-grids` folder and start RustGIS again to stop using them (grids
  installed for PROJ are still used). The web app keeps a grid for the session only.
- Grids are downloaded three at a time, those a file read waits for first: at most 16 each time
  the setting is turned on, and none is started once 2 GB of grids have been kept; past that, the grid is said
  to be not downloaded. Turning the setting off drops those not started yet.

RustGIS supports a subset of Esri's projected systems, checked against
PROJ at representative points; unsupported codes such as World Miller (`ESRI:54003`) and the
two-point oblique Mercators (`ESRI:53025`, `ESRI:54025`) are refused as unknown codes. The presets:

| Preset | Kind |
| --- | --- |
| Equirectangular, Miller | World, cylindrical |
| Mollweide, Lambert Cylindrical Equal Area, Equal Earth, Eckert IV | World, equal-area |
| Robinson, Natural Earth, Winkel Tripel | World, compromise |
| North / South Polar Stereographic | Polar |
| Europe Equal Area (ETRS89 LAEA), USA Contiguous Albers Equal Area | Regional, equal-area |
| Albers Equal Area Conic, Lambert Conformal Conic | Conic, for a mid-latitude region |
| VN-2000 / TM-3 (Vietnam), VN-2000 / UTM zones 48N and 49N | Vietnam, transverse Mercator |

The world and polar presets take a central meridian, and the two conics also their standard
parallels: pick the preset in the **Coordinate System** dialog and set **Central meridian** (and
**First** and **Second standard parallel**) beside it. A Pacific-centred Robinson is
`map.projection {"type": "robinson", "lon_0": 150}`, and an Albers for South America
`{"type": "albers", "lon_0": -55, "lat_1": -5, "lat_2": -35}`. Without `type`, the values go to the
map's current preset. The map saves them in its code (`robinson +lon_0=150`), which can also be
typed as it is. **View › Projection** picks a preset in its standard form, and marks the one in
use whatever its parameters. Meridians run from -180° to 180° and parallels from -89° to 89°.
Parallels that average within 0.5° of the equator (mirrored, such as -30° and 30°, or both next
to it) make no cone and are refused, as is a value a preset doesn't take. The two regional grids
keep their standard definitions. A conic's area of use runs 35° beyond its parallels and 90°
either side of its meridian.

For Vietnam, **VN-2000 / TM-3** takes a central meridian (105°E by default), with province
shortcut buttons in the Coordinate System dialog. A province alias such as `vn2000:hanoi`
sets its meridian; scripts can use
`map.projection={"type":"vn2000tm3","lon_0":105.75}`. The UTM presets use fixed zones:
`vn2000Utm48` (`EPSG:3405`) and `vn2000Utm49` (`EPSG:3406`). These systems include the
seven-parameter datum shift to WGS 84. A bare `VN-2000` is ambiguous and is refused: name TM-3
or a UTM zone.

Layers are projected on the fly. A feature that crosses the edge of a world projection (the
meridian opposite its central one, such as 90°E in Equal Earth Americas) is cut there and drawn on
both sides of the map. Each projection has an area of use: polar projections show
their hemisphere poleward of 30°, regional ones their region, transverse Mercator (UTM) zones
30° either side of their meridian, and data outside it isn't drawn. Switching keeps the centre
and scale, and the choice is saved with the map (`map.projection` from the command line). A
raster already in the map's coordinate system draws without being reprojected, and so does a
vector layer read from a file in it. On a map in another EPSG or ESRI system limited to an area of
use, a layer is drawn into it by the direct transformation between the two systems (the datum
shift PROJ picks for the pair, not one through WGS 84), and its vertices are edited and snapped
where they are drawn; Web Mercator, the globe and world projections draw through
longitude/latitude. To open every new
session in a projection, set it in **Settings › Start-up › Projection**. Robinson, Equal
Earth, Natural Earth, Winkel Tripel and Eckert IV are computed by RustGIS itself on a sphere,
so EPSG:8857 (Equal Earth) works too. So is the sinusoidal projection, on a sphere or an
ellipsoid: MODIS rasters, whose GeoTIFFs give it as their own projection, open and draw, and
World, Sphere and Africa Sinusoidal (`ESRI:54008`, `53008`, `102011`) can be the map's.

### Finding a coordinate system

The coordinate system boxes of the Coordinate System dialog, Project Properties, Define Projection,
the Shapefile, DXF and CSV imports, the measured grid, **Settings › Start-up › Projection** and the analysis
environment search as you type. A box finds a system by its code (`3035`, `EPSG:3035`,
`esri:102033`), by words of its name (`lv95`, `utm 32n wgs`), by its projection method (`albers`,
`oblique mercator`) or by its area of use (`switzerland`), and lists the matches under the box: the
systems you used last first (the ten last applied, kept between sessions), the map's own
projections by name where the box sets the map (`robinson`), then the EPSG and ESRI systems. Among
equal matches, names whose words begin with the words typed come first (`lambert 93` finds
Lambert-93 before ISN93 / Lambert 1993), then WGS 84 and ETRS89 systems, and a plain system before
its axis-order variants.

↑/↓ move through the list, Enter or a click takes a system, and Escape closes the list. In a
dialog, taking a system fills the box, and Apply or OK confirms the dialog; Enter on a code typed in
full, such as `2263` or `EPSG:2263`, takes it and confirms the dialog at once. In the graticule and
start-up menus a system taken applies at once; in the analysis environment, **Set Coordinate
System** applies it.

Enter in an empty box leaves it empty (no system, or the map's own for the measured grid) unless ↑
or ↓ chose a row. A box for the map, the measured grid or the analysis environment lists projected
systems only. Text the list doesn't hold, such as WKT or a PROJ string, is used as typed. A system
is remembered once it is applied or its import starts; **Clear** beside **Recently used** in the
Coordinate System dialog forgets them all.

### The project's coordinate system

A project can have a coordinate system of its own, set in **Project Properties** (the project
page's **Project Properties…** button, or `project.coordinateSystem` from the command line, Python
or the Assistant). New maps are drawn in it, starting where the active map is looking, and new
empty layers from **Layer Templates** hold their coordinates in it, so features drawn in them are
stored in its units. Maps and layers already in the project keep their own systems. A system in
degrees, or one without an EPSG or ESRI code, can't be a map's projection: new maps then stay in
Web Mercator, and RustGIS says why. A project without one works as before.

**While it has none, take it from the first layer added** gives the project the system of the next
layer added from data (**Home › Add**: a file, a URL, a service, a database; or a map imported, an
offline map opened, layers pasted) that says what system
it is in (a raster's, or a file's such as a GeoPackage's or a Shapefile's `.prj`), and says so; Undo
takes it back with the layer. A tool's output gives none. A layer that names no system, taken for
longitude/latitude, gives none, nor does a raster read as WGS 84 because its coordinates look like
degrees; one that names WGS 84 or another system in degrees gives that, and new maps then stay in
Web Mercator. A raster read in place gives its system once its pixels have arrived, at the next
change to the project. The setting and the option are saved with the project;
an older RustGIS refuses a project that has them (it needs `project-crs`) rather than saving it
again without them.

In the **Coordinate System** dialog, **Use the project's** and **Use the first layer's** pick the
project's system or that of the map's first layer with one, and Apply draws the map in it.

## Globe

<figure markdown="span">
  ![The World Atlas on the 3D globe, with the View tab open](https://assets.rustgis.app/images/globe.webp)
  <figcaption>The World Atlas on the 3D globe.</figcaption>
</figure>

**View › Globe** shows the map as a 3D globe seen from space instead of the flat Web Mercator
map, and back again. Drag to spin the globe, right-drag or Ctrl+drag to turn and tilt it (as
on the flat map), scroll to zoom, and use **Full Extent** to see the
whole Earth. Every layer draws on the globe: vectors are clipped at the horizon and follow great
circles, rasters are draped onto the sphere, and labels, selection, identify, measuring and
editing work as on the flat map. The scale stays the same when you switch, and the choice is
saved with each map in the project (`map.projection` from the command line).

While the globe turns or zooms, the map is drawn flat once and laid over the sphere by the
graphics card, so it moves smoothly whatever the basemap; labels come back, and the map is
drawn exactly, as soon as it stops.

The globe hangs in dark space, in the light theme too, with stars around it and now and then a
shooting star passing behind it. **View › Visual Effect › Stars** turns them off. Exported maps
and layout map frames print the globe in a light space instead, without stars, so a globe inset
suits a light page (on the Midnight basemap the space stays dark).

## View effects

<figure markdown="span">
  ![Dark mode over Europe on the Midnight basemap, with the View tab open](https://assets.rustgis.app/images/dark.webp)
  <figcaption>Dark mode on the Midnight basemap.</figcaption>
</figure>

The View tab adds effects drawn over the map: **Illumination** (day and night shading for a time,
with twilight), **Weather** (haze, fog, rain, snow), **Visual Effect** (vignette, edges, the stars around the globe), **Camera
Effect** (tilt-shift blur, film grain) and **Postprocessing** (brightness, contrast, saturation,
sepia). The **Color Vision** simulator shows the map as people with protanopia, deuteranopia,
tritanopia or achromatopsia see it (on screen only; exports are untouched). **View › Display ›
Graticule** (`map.graticule`) draws a graticule over the basemap; its arrow picks the type, and
picking one shows it:

- **Latitude and Longitude** (the default): parallels and meridians, more of them as you zoom in.
- **Measured Grid**: eastings and northings in a projected coordinate system, at a round interval
  that suits the scale (about eight lines across the view), as a layout's measured grid draws them.
  The system is the one chosen under **Measured grid's coordinate system** (found by name or
  code, see [Finding a coordinate system](#finding-a-coordinate-system); an EPSG code such as
  `2056`, WKT or a PROJ string; `"crs"` in the command); left empty, it is the map's own when the
  map is projected, else the WGS 84 UTM zone at the centre of the view. A system that can't be read
  or is geographic is refused, with a message naming it.
- **UTM Zones**: the WGS 84 UTM zones and their latitude bands (C to X, 80° S to 84° N), with
  southern Norway's and Svalbard's wider zones. Once no more than eight zone-and-band areas are in
  view, each also gets its own eastings and northings, at the same interval in all of them and
  cut at the zone's edges.
- **MGRS**: the Military Grid Reference System's grid (`"mgrs"` in the command): the same zones
  and bands as grid zone designations, drawn heavier. Once no more than eight are in view, each gets
  its own grid at a power of ten that suits the scale, between three and thirty lines across the
  view: 100 km, then 10 km, 1 km and, zoomed in closest, 100 m (100 km while more than four grid
  zone designations show). The 100 km lines, the edges of the 100 km squares, stay heavier than the
  finer ones. Zoomed out, each grid zone designation is named (`33U`); once the 100 km squares are
  large enough on the screen, each is named by its letters (`VP`), with its grid zone designation
  (`33U VP`) when more than one is in view. A zone or square cut too small for its name isn't
  named, and the names aren't drawn on a tilted view. Nothing is drawn yet north of 84° N or south
  of 80° S, where MGRS uses UPS.

Only the MGRS grid is labeled on the map; the status bar reads coordinates in UTM or MGRS. Like the
latitudes and longitudes, they aren't drawn on the copies of the world a flat map shows past
±180°. The type and system are saved with the map; a project whose measured grid names a system
this version can't use (one in degrees, or a PROJ string the web app can't read) opens without it,
and says so.

Two more settings in the same menu apply to every type, and are saved with the map:

- **Draw on top of layers** (`"onTop"` in the command; off by default, and `null` goes back to
  it): by default the grid lines are drawn with the basemap, under the map's layers and web tile
  layers, so a filled layer hides them. With it on, they are drawn over every layer and tile
  layer, still under the labels. They then also draw over the highlights of selected features.
  The MGRS names are labels, so they're drawn over the layers either way, as the layers' own
  labels are: a filled layer can hide the lines under a name but not the name. They go with the
  lines when the grid's opacity is 0, and pause with the labels.
- **Grid color** (`"color"`: `"#RRGGBB"` or `"#RRGGBBAA"`, with its opacity): the lines' color. By
  default it is the basemap's own grid color, which is faint; **Default** (`null` or `""` in the
  command) goes back to it. The MGRS names take the same color, fully opaque. Major and minor
  lines keep their widths.

Neither setting shows or hides the grid. A project that uses either needs a RustGIS that knows
it: an older one refuses to open it rather than drop the setting.

**Global** shows a globe; **Local** a flat view centred where you are (azimuthal equidistant;
not in the web app, which doesn't include that projection).
**Wireframe** draws polygons as outlines; **Raise Priority** and **Lower Priority** change a
layer's drawing priority independently of its order. **View Clipping** hides features beyond a
distance from the centre, and **Speed Display** draws without antialiasing.
The **Navigator** adds a compass, zoom buttons and a pan
pad; **Camera** shows and sets the position and scale; the **Magnifier** shows twice the map under
the pointer. On the Home tab, **First-Person** navigation walks with W A S D and flies up and down
with Q and E. The interface follows the system's light or dark setting; **View › Display › Dark
Mode** switches it by hand.

**View › Display › GPU Rendering** (beta) draws supported parts of the flat map on the GPU
for smoother navigation on large datasets. Unsupported geometry and layer effects fall back
to the CPU. Raster tiles and opaque vector tile styles can be drawn directly on the GPU on a
north-up Web Mercator map; translucent or blended tiles use cached pictures during navigation.

GPU Rendering is off by default and remembered across sessions (`ui.map.gpu` is the command;
`RUSTGIS_GPU=1` forces it on for development). The globe, view effects and colour-vision
simulation stay on the CPU, and exports are unaffected. A rotated or tilted map uses the GPU
only when every visible layer can be drawn there: tiles, rasters, terrain, transparency,
masking, swipe, blending, aggregation, graphics, stacked strokes, fills or labels with icons keep it
on the CPU. Extrusions use the GPU only on a tilted map. In the desktop app, if the graphics
card runs out of memory (a card with 2 GB at a large window size, with other programs using it
too), the map switches to the CPU, with a notice, instead of the app closing; clicking **CPU**
in the status bar tries the GPU again, as does the next start (the setting stays on); errors
that follow from running out in the same moment aren't said on their own. Any other error the
graphics card reports is said once, as the map (or, with the map on the CPU, the window) may
then be drawn wrongly; so is running out of memory with the map already on the CPU.
