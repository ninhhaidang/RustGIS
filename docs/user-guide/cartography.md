# Cartography

## Symbology

<figure markdown="span">
  ![The Style tab and the Symbology pane with graduated colours; the larger African countries are selected on the map and in the attribute table](https://assets.rustgis.app/images/symbology-table.webp)
  <figcaption>Graduated colours in the Symbology pane, with a selection synced between map and table.</figcaption>
</figure>

Select a layer in Layers and open **Style › Symbology** (or the Symbology pane):

- **Single Symbol**: one fill, outline and marker for every feature.
- **Unique Values**: one colour per distinct value of a field. If no field is chosen, the current
  renderer's field is kept when available; otherwise the first available field is used, including
  text fields. There are up to 64 classes: the first values in numeric order when every value is
  a number (9, 10, 100, as on a numeric field or a text field of codes), otherwise in text order
  (by character, so `B` before `a`). Features with a value past them draw as
  `<all other values>`, and a notice says how many and in which order the classes were taken. The
  legend lists the classes by number when every class is a number, a class renamed by hand in its
  value's place, and otherwise by label without regard to case. The Layers pane and layout
  legends list only the classes that drawn features use (after the definition query and display filters);
  `<all other values>` appears only when a drawn feature falls outside the classes and its symbol
  is not fully transparent. A layout legend can leave it out with **Hide other values** (see
  [Layouts](layouts.md)). Instead of a field, Unique Values can class by an
  [expression](expressions.md) typed in the **Expression** box under the field: one colour per
  distinct value it gives (`CASE WHEN pop_max > 1e7 THEN 'large' ELSE 'small' END`, `upper(kind)`),
  a feature where it gives null in `<all other values>`. The classes are the values it gives
  without a map; it can read `@map_scale`, so a feature can move between those classes as the map
  zooms (a value it gives only at some scales draws as `<all other values>`), and the Layers pane
  and layout legends list the classes drawn at the map's scale (on a layer too large to class
  every feature for a legend, past 2,000,000 features × classes, all its classes). Clearing the
  box goes back to a field. A layer classed by expression is drawn on the CPU, and a web style
  shared from it draws every feature with one symbol, which a notice says
  (`layer.symbology={"type": "unique", "expression": "…"}`).
- **Graduated Colors**: 1 to 12 classes of a numeric field by natural breaks (Jenks), equal
  interval, quantile, geometric interval, standard deviation, arithmetic progression or head/tail
  breaks (the Method list is in alphabetical order). Geometric interval makes class
  widths grow by one ratio, for values piled up at the low end; for a field that reaches zero or
  below, the minimum is taken as if it sat at a hundredth of the range, so the widths grow by
  101^(1/classes). Standard deviation makes classes one deviation wide about the mean, and leaves
  out a class no value falls in, so a skewed field can get fewer classes than asked. Arithmetic
  progression makes class widths grow by one step (1, 2, 3… units), for more detail among the low
  values. Head/tail breaks splits at the mean, then at the mean of the values above it, and so on
  while those stay under 40% of the values they were taken from: for a few very large values among
  many small ones (city sizes, incomes). It sets its own number of classes, at most the number
  asked. Manual keeps the bounds you type: each class's upper bound is a number to edit or take out, **Split the widest
  class** adds one, and the field's largest value closes the last class. If no field is
  chosen, the current renderer's field is kept when it is numeric; otherwise the first numeric
  field is used. If the layer has no numeric field, or the
  switch fails for another reason, the pane shows why and keeps the current renderer. When numeric
  fields exist, a field picker lets you try another one; the error stays visible if a retry fails.
  A problem no field can fix, such as a definition query that keeps no features, needs fixing
  first. Closing the pane or selecting another layer drops the retry.
- **Proportional Symbols**: point size grows with a numeric field, chosen as for Graduated Colors.

A point's **Shape** is a circle, square, triangle, diamond or star, or a **Character**: any
character of an installed font, picked under **Font**, as map signs often are (a symbol font's
church, tree or mine). Its **Size** is the font's size, and the character sits as QGIS draws a
font marker: centred across the point, its baseline half the font's ascent below it. A font the
machine lacks draws the character in the label font, and a character no font here has
draws a circle, so the point still shows. By command:

```text
layer.marker={"layer":"Peaks","char":"▲","font":"DejaVu Sans","size":12}
```

**Rotation** turns each point's marker by the angle in a number field, in degrees clockwise
(**Counterclockwise** for angles as mathematics counts them), as wind arrows or the signs of a
topographic map turn. **Marks** stack several markers at a point, bottom first, in place of the
symbol's: a white disc under a sign, say, so it reads on any background. Each mark has its own
colour, outline, shape, size and angle, and is shifted right (**Offset right**) and down
(**Offset down**); its offset turns with its angle, as QGIS turns it. **With the field** turns a
mark by the rotation field too, or keeps it still. A layer with classes can have marks for one
class of its own, as line strokes can. By command, `layer.markerRotation` and the point layer's
strokes of markers:

```text
layer.markerRotation={"layer":"Signs","field":"angle","counterclockwise":true}
layer.lineStrokes={"layer":"Signs","strokes":[{"color":"#FFFFFF","markers":{"mark":"circle","size":14}},{"color":"#000000","markers":{"mark":"character","char":"▲","size":10,"offsetAlong":0}}]}
```

### Symbols by expression

**By Expression**, under the symbol in the Symbology pane, sets parts of the symbol per feature
from an [expression](expressions.md): a point's **Size** (points), the **Fill** and **Outline**
colours (a line's **Color**), the **Outline width** (a line's **Width**, in points) and a point's
**Angle** (degrees clockwise, in place of the **Rotation** field; where it gives null, the field's). A colour is text: `'#d7301f'`,
a CSS colour name such as `'navy'`, or `'r,g,b[,a]'` as `color_rgb()` and `color_rgba()` give it.
A point with **Marks** scales them all with its **Size**, their sizes and offsets in proportion to
the symbol's own size (each mark keeps its colours); a size of 0 or less draws nothing. Where an
expression gives null (or, for a colour, text that isn't one), the feature takes the symbol's own, so a `CASE` without `ELSE` changes only the features it names. `@map_scale` is the
scale the map is drawn at, so a width can change with the zoom (in a shared web map, whose
reader zooms freely, it is null). They apply on top of the
renderer: a Unique Values class's colour, then the expression's. An expression applies when its
box loses focus; one that doesn't read shows why and isn't applied. A layer styled by expression
is drawn on the CPU; points drawn as counted bins (aggregation) keep the bins' own symbol, and
a line's stacked strokes keep the colour and width each sets. A shared web map draws each
feature's colours, outline width and size as styled (points as circles, without their angle or
marks); a layer's MapLibre style (a web layer's, or one exported alone) carries the renderer's
symbol, without the expressions. By command:

```text
layer.symbolBy={"layer":"Cities","size":"scale_linear(pop_max, 0, 2e7, 4, 24)","fill":"CASE WHEN pop_max > 1e7 THEN '#d7301f' END"}
layer.symbolBy={"layer":"Rivers","strokeWidth":"if(@map_scale < 5e6, 2, 0.5)"}
layer.symbolBy={"layer":"Cities","fill":null}
```

Pick a colour scheme from the pane's swatches, listed in groups: **Perceptually Uniform** (Viridis,
Plasma, Inferno, Magma, Cividis, Batlow and Aurora: even steps in lightness, so they read in
greyscale and for colour-blind viewers), **Sequential**, **Diverging** (Red–Blue, Vik, Roma,
Balance), **Topographic and Bathymetric** (Terrain, Oleron, Topo and Deep; Oleron and Topo change
colour sharply at the middle of the range, sea level for data centred on 0), **Qualitative**,
**Matplotlib** (the whole matplotlib colour map library as GeoLibre lists it, cmocean's maps
included, by name from Accent to YlOrRd) and the project's own **Custom** ramps. Commands take a
scheme by name; matplotlib's names are case-sensitive where they meet ours (`terrain` is
matplotlib's, `Terrain` ours). A project's own ramp named like a built-in one (an older
project's `jet`, say) is renamed with ` (custom)` when the project opens; layers drawn with it
keep their colours, as each holds its own copy. Click a group's name to show or hide it. The Style tab sets the fill **Color**, the
**Outline** width, **Transparency** and a **Visibility Range** (the scales between which the layer
draws; a large file read in place is not read at all outside them); points can be circles, squares,
triangles, diamonds or stars. **Data › Definition Query** draws only the features matching a SQL
clause; Unique Values, Graduated Colors and Proportional Symbols take their values, classes and
sizes from those features when you apply them (apply the symbology again after changing the query),
and **Full Extent** zooms to them. A layer with [subtypes](editing.md#data-design) can be symbolized
by them. **Style › Styles** saves symbology as styles to reuse on other layers and in other
projects.

For polygons, **No Fill** in the Symbology pane or **Style › Drawing** draws only the outlines.
It keeps each symbol's outline colour and width, including in Unique Values and Graduated Colors,
without changing the classes. Change the outline with **Outline color** and **Outline width** in
the Single Symbol editor, or `layer.outline` for all classes. Undo restores the previous fills;
the colour picker's opacity control can also make a fill visible again. By command:

```text
layer.noFill={"layer":"Countries"}
```

A GeoJSON with mixed geometry keeps its points, lines and polygons in one layer, classified by its
most common geometry family. **No Fill** and the **Fills** editor are also available when that
layer contains polygons. No Fill leaves point and line symbols unchanged; use the Fills editor
to change the polygon fill's colour or remove its transparent fill to restore the symbol's fill.
For layers read in place, these controls recognise polygon features in the currently loaded view
as well as the layer's declared geometry family.

The ribbon's **Color** control changes Unique Values or Graduated Colors back to Single Symbol.
To keep the classification, edit a class's colour in the Symbology pane instead. A polygon's
fill and outline are independent: changing its fill leaves its outline intact.
For raster renderers and band stretches, see [Rasters](data.md#rasters).

### Edit a class

With Unique Values or Graduated Colors, each class in the Symbology pane's list has a colour button
and a legend label. Pick a colour and opacity to give that class its own. Type a label to show in the Layers
pane and in layout legends instead of the value or range (for example `High income` for
`1. High income: OECD`); clear it to show the value again. `<all other values>` can be edited the
same way. Applying the symbology again on the same field keeps the colours, outlines and labels
you set by hand on the classes that are still there (the others take the scheme's colours again);
picking another colour scheme recolours every class but keeps the labels. A Graduated Colors
class keeps its colour and label while its range stays the same. By command:

```text
layer.classStyle={"layer":"Countries","value":"1. High income: OECD","color":"#2E6B4F","label":"High income"}
layer.classStyle={"layer":"Countries","index":0,"label":"Under 1 million"}
layer.classStyle={"layer":"Countries","index":0,"color":"#2E6B4FFF"}
layer.classStyle={"layer":"Countries","other":true,"label":"No data"}
```

`value` names a Unique Values class, `index` a Graduated Colors class (0 is the lowest) and
`other` the `<all other values>` class; `outline` sets a class's outline colour.
`#RRGGBBAA` sets the fill opacity exactly for every geometry type: `FF` is fully opaque for
print choropleths, `00` fully transparent, and intermediate values partly transparent. The class
colour picker includes opacity and sends all eight digits, including `FF`. Changing opacity
keeps the class's RGB colour, even when making a fully transparent class visible again.
For fully transparent fills, the numeric controls temporarily use linear 0–1 values so you can
edit the hidden RGB colour without losing it. Other colour pickers keep their numeric settings.

For polygons, six-digit `#RRGGBB` retains the existing translucency, with the legacy minimum
alpha of 150; new polygon layers still default to 170. Colour schemes preserve a polygon's
fill opacity when classes are first created or recoloured with `layer.ramp`. Reapplying the
same field, ranges and scheme also preserves opacity, including edits that change only alpha.
As with other class colour edits, changing the field or graduated ranges starts fresh; choosing
a different scheme through `layer.symbology` resets polygon opacity to its default.
Layer/group transparency still applies on top of the symbol's opacity in maps and exports.

### Line strokes

A line layer draws one stroke by default. The **Strokes** section of the Symbology pane stacks
up to 16 in one symbol, for road casings, parallel lines and offsets: **Add Stroke** puts one on
top, and each stroke has its own **Color**, **Width**, **Offset**, **Cap** (round, flat or square)
and **Join** (round, miter or bevel), with buttons to move it up or down or remove it. The list
shows the top stroke first, and the preview above it shows the symbol along a line drawn left to
right. A colour or width set to **From symbol** follows the renderer, so the strokes take each
Unique Values or Graduated Colors class's colour. **Use a Single Stroke** goes back to one.

Widths and offsets are in points: they keep their size on screen at every scale, and the features
keep their coordinates. A positive offset shifts the stroke to the right of the line's direction
(the order of its vertices) and a negative one to the left, so two copies of an edge drawn in
opposite directions land on opposite sides of it. Strokes are saved with the project and drawn in
exports. By command, bottom stroke first (`[]` goes back to the single stroke):

```text
layer.lineStrokes={"layer":"Rivers","strokes":[{"color":"#000000","width":11.5,"offset":8.5},{"color":"#FF8C00","width":7,"offset":8.5,"cap":"round","join":"round"}]}
```

A stroke without `color` or `width` takes the renderer's.

A Unique Values or Graduated Colors class can stack strokes of its own, so a motorway gets a
casing and a centre line while a path stays a single thin line on the same layer. Pick the class
under **Strokes for** at the top of the Strokes section (**All classes** edits the layer's
strokes); a class with strokes of its own is marked with •. A class without its own is drawn with
the layer's strokes, and changing them while it is picked gives it its own copy. **Use the
Layer's Strokes** gives the class back to the layer's. A class keeps its strokes when the same
symbology is applied again on the same field (a Graduated Colors class, while its range stays the
same). By command, name the class as `layer.classStyle` does, with `value`, `index` or
`"other": true`:

```text
layer.lineStrokes={"layer":"Roads","value":"Motorway","strokes":[{"color":"#5A3A1E","width":7},{"color":"#F28C28","width":5},{"color":"#FFFFFF","width":0.6}]}
```

A stroke can be dashed: **Dash** takes the lengths in points, dash and gap in turn (`4 2`, or
`4 2 1 2` for dash-dot; an odd list is repeated, as in SVG), and its menu offers dashed,
dotted, dash-dot and long-dash patterns sized to the stroke's width. Empty is a solid line. With
flat caps the dashes are as long as given; round and square caps add half the width at each end.
By command, `"dash"` on a stroke (`null` for solid):

```text
layer.lineStrokes={"layer":"Paths","strokes":[{"color":"#000000","width":1.2,"cap":"flat","dash":[6,3]}]}
```

By default, each feature draws its whole stack in turn, so where two roads meet, the second one's
casing cuts across the first one's core. **Draw strokes in levels** (symbol levels) draws all
stacked strokes at a lower level before those at a higher one, so the casings join underneath
and the cores run through. A stroke's level is its place in the stack (the bottom one 0) unless
it is given one: a **Level** row shows on each stroke while levels are on; turn off **Auto** to
set it from 0 to 255. Strokes of different classes that share a level go down together, such as
the casings of all road classes before any core. A layer read in place from its file draws
its levels a batch of features at a time. By command (`"level"` on a stroke, `null` for its
place in the stack):

```text
layer.lineStrokes={"layer":"Roads","value":"Motorway","strokes":[{"color":"#5A3A1E","width":7,"level":0},{"color":"#F28C28","width":5,"level":1}]}
layer.strokeLevels={"layer":"Roads","on":true}
```

A stroke can draw markers or hash marks along the line instead of a line, as railways, boundaries
and directions of flow are drawn: its **Draw** menu picks **Line**, **Markers** or **Hash marks**.
Markers take a **Shape** (a character of a font too, as for points) and a **Size**; hash marks a **Length**, crossing the line square at an
**Angle** of 0°. **Place** puts them at intervals along the line (**Interval** apart, counted from
its start), at its vertices or its inner ones, at either end or both, or in its middle. **With the
line** turns each mark with the line's direction; the **Angle** turns it further, clockwise. Marks
take the stroke's colour, its width as a hash mark's line (or a marker's outline, by command), and
its offset to either side. Stack them with line strokes: a casing, a core and ticks across it. By
command, `"markers"` on a stroke:

```text
layer.lineStrokes={"layer":"Railways","strokes":[{"color":"#333333","width":1.5},{"color":"#333333","width":1,"markers":{"mark":"hash","size":6,"interval":10}}]}
layer.lineStrokes={"layer":"Rivers","strokes":[{"width":1},{"markers":{"mark":"triangle","size":7,"at":"middle"}}]}
layer.lineStrokes={"layer":"Trails","strokes":[{"markers":{"mark":"character","char":"»","font":"DejaVu Sans","size":8,"interval":16}}]}
```

`"at"` takes `"interval"`, `"vertices"`, `"innerVertices"`, `"first"`, `"last"`, `"middle"` and
`"segmentMiddles"`, or a list of them; `"offsetAlong"` moves the marks along the line (from its
start, back from its end, on from its middle), `"averageAngle"` turns a mark at an interval with
the line over that stretch of it (smoother on a line of many short bends), `"rotate": false`
keeps marks upright, and `"outline"` gives markers an outline as wide as the stroke.

### Fills

A polygon layer fills with one colour by default. The **Fills** section of the Symbology pane
stacks fills inside the outline, as a wetland gets a pale green under blue hatch lines: **Add
Fill** puts one on top (the first is the symbol's colour, the next hatch lines over it), the
arrows move one up or down and the cross removes it; the top fill is listed first. **Draw** picks
**Solid**, **Hatch lines** (parallel lines at an **Angle**, anticlockwise from east, **Spacing**
apart, of a **Line width**, shifted across by an **Offset**), **Markers** (a **Marker** shape
of a **Size** on a grid **Spacing across** and **Spacing down** apart, **Rows shifted** to
stagger them), **Gradient** or **Shapeburst**. A gradient shades each feature from its colour to
a **Second color**: by default top to bottom, of a **Type** that runs **Linear** (straight
across), **Radial** (in rings from its first point) or **Conical** (round its first point),
turned by an **Angle** (anticlockwise), and past its points a **Spread** of **Pad** (its end
colours), **Reflect** or **Repeat**. A shapeburst shades from its colour at the polygon's edge to
the second colour inside, a **Distance** in, or over the **Whole shape** to the point deepest
inside; **Blur** softens it and **Ignore holes** shades from the outer edge alone. Each part of a multi-part
feature is shaded on its own, in its own box (an island its own gradient). Zoomed far into a
polygon shaded over the whole shape, its deepest point is found on a coarser picture of it. A colour set to **From symbol** follows the renderer, so the fills take each
class's colour. The polygon's outline is drawn over the fills. Patterns are fixed to the map, so
neighbouring polygons' hatch lines meet, and keep their size in points as the map zooms. A Unique
Values or Graduated Colors class can stack fills of its own, picked under **Fills for** as for
strokes. By command, `layer.fillLayers`:

```text
layer.fillLayers={"layer":"Wetlands","fills":[{"color":"#DDEEDD"},{"color":"#3366CC","pattern":"hatch","angle":0,"spacing":6,"width":0.8}]}
layer.fillLayers={"layer":"Orchards","fills":[{"color":"#F2F5E6"},{"color":"#4E7A2E","pattern":"dots","marker":"circle","size":3,"spacing":[8,8],"displacement":[4,0]}]}
layer.fillLayers={"layer":"Land use","value":"Forest","fills":[{"color":null},{"color":"#2D5A27","pattern":"hatch","angle":45}]}
layer.fillLayers={"layer":"Lakes","fills":[{"color":"#1F5FA8","pattern":"shapeburst","color2":"#CFE3F7","distance":12,"blur":4}]}
layer.fillLayers={"layer":"Countries","fills":[{"color":"#FFFFFF","pattern":"gradient","shape":"radial","from":"centroid","to":[1,0.5],"color2":"#88AA66"}]}
```

A gradient's `from` and `to` are places in the feature's box (`[across, down]`, 0 to 1) or
`"centroid"`, or in the map view's box with `"inView": true`; `stops` adds colours between the two
(`[[0.5, "#RRGGBB"], …]`), held rather than blended with `"discrete": true`.

`[]` goes back to the single fill (for a class, to the layer's fills). Fills draw on the CPU (on the
globe, patterns keep to the screen rather than the map as it turns); the
legend shows the symbol's colour.

### Reference scale

Symbol and label sizes are set in points, and normally keep that size on screen at every scale.
A layer with a **Reference scale** (Symbology pane, under **Layer**) has its sizes set for that scale
instead, as on a printed map: zoomed in to half of it, its lines, markers and labels are drawn
twice as large; zoomed out to twice it, half as large (within 1/100 and 32 times). Pick a scale,
**Current scale** to keep the sizes as they are now, or **None**. A topographic map designed for
1:25,000 then keeps its proportions as you zoom. By command (a scale, `"current"` or `null`):

```text
layer.referenceScale={"layer":"Roads","scale":25000}
```

Scales in RustGIS take a screen pixel to be 0.28 mm (the OGC standard pixel). Programs that take
96 pixels to the inch (0.2646 mm) call the same view a larger scale; to match their sizes,
multiply their reference scale by 0.2646 / 0.28 (about 0.945).

### Custom color ramps

Besides the built-in color schemes, a project can hold your own color ramps: an organization's
palette, a published scheme such as a ColorBrewer or cmocean ramp not built in, or one designed in a
tool like [Phosphor](https://www.joshuastevens.net/phosphor/). Add them from a color ramp file
with **Style › Styles › Add Color Ramp File** (or **Add Color Ramp File** under the swatches in
the Symbology pane). They appear under **Custom** in the Symbology pane, for feature layers and
rasters (Stretch, Classify, Unique Values and Shaded Relief) alike, and under **Color Ramps** in
the Browser, where right-click deletes one. **Publish › Save As › Color Ramps** saves the
project's custom ramps to a file.

A color ramp file (`.rgisramp`) is JSON: a list of ramps (or one ramp on its own), each with a
`name`, a `kind` (`sequential`, the default, `diverging` or `qualitative`) and its `stops`:

```json
[{
  "name": "Phosphor Frostfire",
  "kind": "diverging",
  "stops": ["#eff6ff", "#abc5e8", "#87a0d7", "#7b5ca6", "#ca7482", "#f5b88e", "#fff5da"]
}]
```

Stops are `#RRGGBB` or `#RRGGBBAA` colors, evenly spaced from the first to the last. For uneven
spacing, give a stop as `{"pos": 0.25, "color": "#87a0d7"}`, with `pos` from 0 to 1; plain
stops between two positioned ones are spaced evenly between them. A ramp has 2 to 256 stops (a
qualitative one can have 1), and can't take the name of a built-in scheme; adding a ramp with
the name of one already in the project replaces it. Scripts, the CLI and the AI Assistant add
ramps with the `ramp.add` command (`{"path": "ramps.rgisramp"}`, or the JSON itself as
`{"text": …}`), and then name them like built-in schemes, for example
`layer.ramp={"ramp":"Phosphor Frostfire"}`.

### Color ramps from other tools

**Add Color Ramp File** also reads the color ramp files other tools write, so a palette from
QGIS, GDAL, GMT or a tool like Phosphor can be added without converting it first:

| Format | Extension | What RustGIS reads |
| --- | --- | --- |
| QGIS style | `.xml` | Every gradient ramp (discrete ones as steps) and preset ramp (as a qualitative ramp) in a Style Manager export, with its name. ColorBrewer, cpt-city and random ramps hold no colors and are skipped. |
| QGIS layer style | `.qml` | The layer's color ramp, or a raster's color map. |
| GDAL color-relief text | `.txt` | `value R G B [A]` lines (as `gdaldem color-relief` takes them), separated by spaces, tabs, commas or colons; values may be percentages and colors may be names; `nv` lines are skipped. A QGIS color map export reads the same way. |
| GMT palette | `.cpt` | `z0 color0 z1 color1` lines, with colors as `r/g/b`, `r g b`, `#RRGGBB`, grey levels, names or HSV; `B`, `F` and `N` lines are skipped. |

These formats tie colors to data values; the ramp keeps their spacing, from the lowest value
to the highest, and a sharp change between two colors stays sharp. A ramp from a GDAL or GMT
file is named after the file and is sequential; the `ramp.add` command takes a `name` and a
`kind` to change that (`{"path": "relief.cpt", "kind": "diverging"}`), and a `format`
(`rgisramp`, `qgis`, `gdal` or `cpt`) for text without a file name. A file with more than 256
colors is resampled to 256 evenly spaced stops.

Sequential and diverging ramps blend between their stops; when there are as many classes as
stops, each class takes a stop's color exactly, so a stepped palette keeps its colors. A
qualitative ramp gives each class the next stop, starting again after the last. A layer keeps
its own copy of the ramp it uses, so layer files, map files and packages keep their colors in
projects that don't have the ramp, and deleting a ramp from the project leaves layers that use
it unchanged.

## Labeling

The **Labeling pane** brings together the label classes and the chosen class's field, SQL
clause, text symbol, placement and visibility limits. Under its header (the layer, labels on or
off, and the class chosen), a tab per topic shows one part at a time: **Content** (the label field,
with a search in its list, the class's expressions under **By Expression**, and the SQL query), **Symbol**, **Position** and
**Range**. Open it for a feature layer with Layers › right-click › **Labeling Properties…**,
**View › Windows › Labeling**, or **Labeling › Labels › Properties**. It runs the same commands as
the ribbon, so each setting change undoes on its own (a dragged size or a colour pick is one step).
Label classes are described below.

Turn labels on per layer (Labeling › Labels › **Label**), then:

- **Label field and text**: the field the labels show
  (`labels.field={"layer": "Cities", "field": "name"}`), and the text symbol's size (4–72 points),
  colour, halo, bold and italic (`labels.style={"layer": "Cities", "size": 12, "color": "#5A1E0A", "halo": true}`).
- **By expression**: a class's **Text**, **Size**, **Color** and **Angle** per feature from an
  [expression](expressions.md) (Labeling pane › Content › **By Expression**). The text replaces
  the field's, as in `"name" || ' (' || round(pop_max / 1e6, 1) || ' M)'`, so a class with a
  text expression needs no field; a feature whose text is null or blank isn't labelled (in the
  example, one with no name). The size is in points (kept within 4–72), the colour is text as
  for [symbols](#symbols-by-expression) (`'#d7301f'`, `'navy'`, `'r,g,b[,a]'`), and the angle
  turns a point's or polygon's label about its centre, in degrees clockwise, kept upright (one
  past 90° reads from its other end, as labels along lines do; those keep the line's angle). Where an expression gives null, or text that isn't a colour,
  the class's own setting is used; `@map_scale` is the scale the map is drawn at. An expression
  applies when its box loses focus; one that doesn't read shows why and isn't applied, and one
  that stops reading (a field removed) is drawn as if it weren't set. **Convert** keeps each
  label's text, and its size, colour and angle by expression (at the map's scale) in `SIZE`,
  `COLOR` and `ANGLE` fields the annotation's classes draw by. A Web Style or Web Layer export labels with the
  field, size and colour of the Default class, without its expressions (and says so). By command
  (a key left out keeps its expression, `null` or `""` removes it; `class` as for the other label
  commands):

  ```text
  labels.by={"layer":"Cities","text":"upper(name)","size":"scale_linear(pop_max, 0, 2e7, 8, 20)"}
  labels.by={"layer":"Cities","color":"CASE WHEN pop_max > 1e7 THEN '#d7301f' END","angle":"15"}
  labels.by={"layer":"Cities","text":null}
  ```

- **Halo and letter spacing**: a halo is white behind dark text and dark behind light, and thin,
  unless it is given a colour and a width of its own (Labeling pane › Symbol › **Halo color**,
  **Halo width**: how far it reaches beyond the letters, 0–10 points). The letters can be set
  apart, or closer with a negative spacing (**Letter spacing**, −5 to 50 points), along lines
  too. The width and the spacing grow with the labels at a reference scale
  (`labels.style={"layer": "Rivers", "haloColor": "#FFFFFF", "haloWidth": 1.4, "spacing": 0.5}`;
  `"haloColor": ""` and `"haloWidth": null` go back to the automatic colour and the thin halo).
- **Background**: a rectangle behind the text (Labeling pane › Symbol › **Background**), white
  with a thin dark outline when turned on: its **Fill** and **Outline** (each can be none), the
  outline's width, its padding across and up and down (how far it reaches past the text, 0–50
  points) and its **Corner radius**. Other labels keep off it as they keep off a halo, and it
  turns with a label along a line; a label curved along a line, character by character, has
  none. Its sizes grow with the labels at a reference scale
  (`labels.style={"layer": "Roads", "background": {"fill": "#FFFFFF", "stroke": "#262626", "strokeWidth": 0.3, "padding": [1.2, 0.3], "radius": 0.7}}`;
  `"background": true` turns on the default one, `false` takes it away, and what an object leaves
  out is kept).
- **Offset**: a label moved from where its position puts it, right and down in points
  (Labeling pane › Position › **Offset right**, **Offset down**, −200 to 200; negative moves it
  left or up), growing with the labels at a reference scale. A label along a line isn't moved
  (`labels.placement={"layer": "Cities", "position": "aboveRight", "offset": [5.7, -2.8]}`; an
  offset can be given without a position).
- **Wrapping**: a label's text broken into lines (Labeling pane › Symbol): **Wrap at** a
  character (or a few, up to 8), a line break wherever it occurs, left out (a space puts each
  word on its own line, `/` breaks `Zürich/Winterthur`), and **Wrap length**, the longest line in characters, broken
  between words (a longer word keeps a line of its own). **Line alignment** lines the lines up
  left (as labels always have), centred or right. A label curved along a line, character by
  character, isn't wrapped
  (`labels.style={"layer": "Cities", "wrapChar": " ", "wrapLength": 12, "lineAlign": "center"}`;
  `""` and `0` wrap none).
- **Repeat along lines**: a label placed along a line (Parallel or Curved) repeated every so many
  points (Labeling pane › Position › **Repeat**, 10 to 10,000): the one at the line's middle where
  it always is, and more either side of it, nearest first, at most 32 a line. Each is a label of
  its own: it collides like any other (one that doesn't fit is left out, the others stay), and
  Lock keeps the ones placed. The distance grows with the labels at a reference scale
  (`labels.placement={"layer": "Rivers", "position": "curved", "repeat": 144}`; `null` or `0` is
  the one at the middle).
- **Font**: the labels' font family, any installed on the computer (Labeling pane › Symbol
  › **Font**; type part of a name to find it), or the label font the interface uses. Bold and
  italic take the family's own bold and italic faces where it has them. The family is saved by
  its name, so a project keeps it on a computer that lacks it, drawing its labels in the label
  font there (the pane shows it as not installed), and in the browser, which reads no installed
  fonts (`labels.style={"layer": "Rivers", "font": "Liberation Serif"}`; `labels.fonts` lists the
  installed families, `{"font": ""}` goes back to the label font).
- **SQL**: label only the features that match a SQL clause (the language of definition
  queries), such as `pop_max > 5000000`. In the Labeling pane, Class › **SQL** › **New
  expression**: write the clause (the field and operator buttons add to it), **Check** says
  whether it reads and how many of the features the layer draws at the map's scale match
  (after its definition query, display filters and time window), and **Apply** sets it in one
  undo step; **Clear** removes the chosen class's clause, leaving other classes unchanged
  (`labels.class.update={"layer": "Cities", "where": "pop_max > 5000000"}`).
- **Visibility Range**: draw labels only between two scales (Out Beyond / In Beyond), or between
  two viewing distances (Closest / Farthest). Clear Limits removes all four.
- **Placement**: automatic (above points, centred on lines and polygons), centred, or one of
  eight positions around the feature (`labels.placement={"layer": "Cities", "position":
  "aboveRight"}`). Lines can be labelled **along** them (Position › Along lines): **Parallel**
  sets the text straight, turned to the line's direction across it; **Curved** turns each
  character with the line where it bends. Both sit at the middle of the line's longest stretch
  in view, centred on the line and kept upright, and may run past its ends by about a letter; a
  line too short for its text, or (curved) one bending more than 25° from one character to the
  next, is left unlabelled. A text over 200 characters long is set straight rather than
  curved. Points and polygons with these placements are labelled as automatic
  (`labels.placement={"layer": "Rivers", "position": "curved"}`).
- **Other places round points**: a point's label whose place another label takes tries the
  other places round the point, corners first (above right, above left, below right, below left,
  right, left, above, below, leaving out its own), and goes to the first where
  all of it shows; where none does, it stays at its own place and shows what fits there (an icon
  without its text, say), or is left out if nothing fits (Labeling pane › Position ›
  **Try other places round the point**, for point layers). Off by default: a label that moves can
  take the place a less important one had. The class's offset applies to every place, and Lock
  keeps the place each label took
  (`labels.placement={"layer": "Cities", "otherPositions": true}`).
- **Other places along lines**: a Parallel or Curved label whose place at the middle is taken, or
  (curved) where the line bends too sharply, tries up to seven others along the same stretch,
  nearest the middle first, and goes to the first that will do (Labeling pane › Position › Along
  lines › **Try other places along the line**). If none will, it is left out, as with this off. A
  line with a repeat keeps its repeats where they are, so the box is hidden while Repeat is on, and
  the setting does nothing there or at a position off the line. Off by default, for the same reason
  as round points, and Lock keeps the place each label took
  (`labels.placement={"layer": "Rivers", "position": "curved", "otherPositions": true}`).
- **Lock** keeps the labelled features fixed while you pan and zoom; **Pause** stops drawing
  labels for faster navigation; **View Unplaced** draws the labels that didn't fit in red. They
  apply to the whole map, so they are on the ribbon (Labeling › Map Labeling), not in the pane.
- **Convert** (Labeling › Map Labeling, or Layers › right-click › **Convert To Annotation**) turns a
  layer's labels into an annotation layer: one point per label, carrying its
  text in a `TEXT` field, which you can then edit, move or delete like any other features.
- **Label classes** label different features differently, such as large cities in larger text.
  Each class has a SQL clause choosing its features, and its own field, text symbol, placement
  and visibility range. Each class labels every feature its clause matches, the **Default**
  class too (with no clause, every feature). A feature matched by two classes produces a label
  candidate for each; collision rules still determine which labels fit. To avoid duplicate
  labels, use clauses that do not overlap: `pop_max > 5000000` for Large cities and
  `pop_max <= 5000000` for Default. Classes are placed in list order, Default last; a hidden
  class or one outside its visibility range labels nothing without excluding features from
  other classes. There can be up to 64 classes besides Default. In the Labeling pane, the
  **Class** list at the top, in a card with the class's actions, chooses the class every tab
  shows and edits, and **Visible** under it hides or shows that class. The row under the list has **New**, which adds a class
  with the chosen class's field, text symbol, placement and range, listed before Default;
  **Rename** and **Move Up** / **Move Down** (the arrows) for the chosen class; and, at its far
  end, **Remove** (Default can be renamed or hidden, not moved or removed). The ribbon's label groups edit the
  same chosen class, and **Labeling › Label Class › Class** chooses it as the pane's list does. By
  command, make them with
  `labels.class.add` (a new class copies Default's field, text symbol, placement and range, or
  those of the class named by `from`), then `labels.class.rename`, `labels.class.update` (`where` or `visible`),
  `labels.class.move` (`to`: 0 first), `labels.class.remove` and `labels.class.clear` (removes
  every class but Default, even when it has been renamed; no error when there are none).
  Field, text symbol, placement and range commands take a `class`; without one they change
  Default, whatever its name. For example:

  ```text
  labels.class.add={"layer":"Cities","name":"Large cities","where":"pop_max > 5000000"}
  labels.class.update={"layer":"Cities","where":"pop_max <= 5000000"}
  labels.style={"layer":"Cities","class":"Large cities","size":14,"bold":true}
  ```

  Convert keeps the classes, with a point for each matching class and a `CLASS` field identifying
  it. Web Style and Web Layer exports (**Publish › Share As**) use Default only, and have no labels
  when Default is hidden.

<figure markdown="span">
  ![Europe with the Countries and Cities labelled; the city names sit above and to the right of their points](https://assets.rustgis.app/images/labels.webp)
  <figcaption>City labels placed above and to the right of their points, beside the country labels.</figcaption>
</figure>

## Layer effects

On the **Style** tab, **Masking** shows a layer only inside another layer's polygons;
**Display Filters** show different features at different scales (a SQL condition per scale
range); **Aggregation** draws points as hexagon or square bins coloured by count.
**Extrusion** raises polygons by a constant height or numeric field (in metres), multiplied by
**Factor**, with **Face Culling**, **Lighting** and **Cast Shadow** controls. It draws in an
oblique view on an untilted flat map and in perspective on a tilted map; the globe draws the
polygons flat. See [Rotating and tilting the map](maps.md#navigating).

For a raster layer, the **Style** tab also has **Layer Blend** (multiply, screen, overlay,
darken, lighten, difference), **Swipe** (the layer shows only left of a divider you slide, to
compare it with what's beneath), **Masking** and **Rotation**.

## Graphics

**Edit › Graphics** draws shapes in a graphics layer (**Home › Add › Add Data › Graphics Layer**, or
one is made with the first graphic). Click once for a Point. Line, Curve and Polygon are clicked
vertex by vertex (double-click to finish); Freehand, Lasso, Rectangle, Circle and Ellipse are
dragged. **Edit › Graphics and Text › Text** places a text where you click, as a layout's text
is placed: it says "Text", is selected, and opens in the **Format** pane (beside the other
right-hand panes) with its text selected, so you type over
it. Shift+Enter starts a new line, and Enter ends the text. Dynamic Text is a menu of values that
update as the map changes (map name, scale, date, time, coordinate system, number of layers, map
centre): pick one, then click the map, and the text starts with its token (`{map}`, `{scale}`,
`{date}`, `{time}`, `{crs}`, `{layers}` or `{center}`); the pane's **Insert Dynamic Text** menu adds
more. Picture: drag a rectangle, then pick a PNG or JPEG image to stretch over it. Tools return
to Explore after each graphic unless Keep Tool is on. Each graphic keeps its own fill, outline,
width, text size, bold, italic, alignment and letter spacing in the layer's attributes, so you can
edit them in the attribute table or with `graphics.style`. To change a text later, click anywhere
on it (with Explore or Select): selected alone, it opens in the Format pane, with the same form a
layout's text has there: text, size, colour, bold, **Italic** (slanted), **Alignment** (how the
lines of a text of several lines line up; the text stays centred on its point) and **Letter
spacing** in points (spaced-out names such as O C E A N O, or a negative value to tighten). A text
looks the same on the map and in an exported map. Each change is one undo step on that text only
(`graphics.edit` does the same by layer and row, with `text`, `fontSize`, `fill` for the colour,
`bold`, `italic`, `align` and `letterSpacing`). Once a text is selected, drag it by
its text to move it, as one undo step (`points` on `graphics.edit`); a drag anywhere else, or on a
text that isn't selected, pans the map as usual. A graphics layer that isn't **Selectable** keeps
its texts from being picked, moved or edited. Closing the
pane keeps the text selected; double-clicking it or right-clicking it › **Edit Text…** opens the
pane again. Text sizes go from 2 to 400 points, on the map as on a layout page.
