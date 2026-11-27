# Layouts and Reports

## Layouts

<figure markdown="span">
  ![A landscape A4 page layout with a map frame, title, legend, north arrow and scale bar, and the Publish tab open](https://assets.rustgis.app/images/layout.webp)
  <figcaption>A page layout with a map frame, title, legend, north arrow and scale bar.</figcaption>
</figure>

Home › New Layout adds a page layout (Letter, Legal, Tabloid or A5 to A0, landscape or portrait)
with a map frame, title, legend, north arrow, scale bar and a dynamic credits line. The title is
the dynamic text `{map}`, so it follows a rename of the map in the first map frame; replace it
with your own text for a title that stays. The command
`layout.new` also accepts custom `width` and `height` in millimetres and `"template": "blank"`.
Both `layout.new` and each layout in `info` include an `elements` array with each element's
`element` ID, `name` and `kind`, ready to use in `layout.update` or `layout.removeElement`.
The existing layout index, name and `info` map-frame summaries keep their meanings.
Layouts open as tabs next to the maps; while one is open, its groups lead the Publish tab:

- **Publication layouts**: Home › New Layout › **Publication Layout** builds a finished sheet or
  figure of the map view in one step, with `layout.publication` behind it:
  - **Sheets** (on every page size New Layout lists: Letter, Legal, Tabloid and A5 to A0,
    portrait or landscape; the menu lists the sizes, each with its styles under Portrait and
    under Landscape): a frame and margin, the title and subtitle,
    the map, and a footer of two boxed columns (sources, and the cartographic reference; one
    with nothing in it is left out, the other taking the width) with a note and the authorship
    under them. When the legend sits at the right of the map (its right edge in the frame's
    right tenth), starting between 45 % and 70 % of the frame's width, the right column starts
    where the legend does, unless its texts would then need a taller box. Text sizes follow the
    sheet's short side: an A3's are 1.41 times an A4's, an A0's four times. So do the legend's
    text size, the scale bar's and the graticule's (9, 8 and 8 points on A4) and the strips the
    graticule's labels take inside the frame. Given its size alone
    (`"a4"`, the default, or any other: `"letter"`, `"a0"`…), a sheet is turned to
    portrait or landscape, whichever the map comes out larger on: the area's extent east to west
    against north to south, on the map's projection, with the room the style leaves the map. The
    result names the profile chosen (`"profile": "a4-landscape"`); give it with its orientation
    (`"a4-portrait"`) to have the other.
  - **Figures** for a journal (90, 140 or 190 mm wide, as tall as the map's shape asks) or a
    thesis (160 × 225 or 160 × 110 mm): the map edge to edge with a line of its projection and
    datum under it, at 7 pt. A figure's title and sources belong in its caption, so they are
    left out, and the result says so.
  - **Step by step**: **Publication Layout Wizard…**, at the end of the same menu, asks for the
    size (a sheet turned Auto, Portrait or Landscape, or a figure), the style, the texts (title,
    the active map's name until one is typed; subtitle, note, sources and credit line, or none)
    and the accent colour, then shows what was picked; **Create** makes the layout as the menu
    does. A figure goes from its size to the review, as it takes no style, texts or accent. A
    field left empty is left out (for the credit line, the dated default applies); the title is
    the exception: empty, it means no title. A layout the command refuses is said, and the wizard
    stays open.
  - **The map**: the `extent` given (or the view) fitted north-up, without tilt, at a round scale,
    two significant figures, rounded up so it still fits within the strips the graticule's labels
    take inside the frame. A graticule's step follows the extent (2° for a state, 15′ for a city),
    with labels on every edge of a sheet.
    The north arrow, the legend (sized to what it lists), the scale bar with the numeric scale
    under it (in a box the size of the bar) and the locator sit in the frame's corners, clear of
    the graticule's labels. The **arrangement** says which corner is each one's. `classic` (the default favourite) puts the
    locator at the top left, the arrow at the top right, the legend at the bottom left and the
    scale at the bottom right. `mirrored` swaps left and right, `legendRight` swaps the legend and
    the scale, and `legendTop` swaps the legend and the locator. `"arrangement"` also takes a
    corner for each (`{"locator": "top-left", "northArrow": "top-right", "legend": "bottom-left",
    "scaleBar": "bottom-right"}`).
    A surround over what the map draws in its corner (its layers and labels, not the basemap; more
    than 0.5 % of the surround) moves off the corner, to the freest place in the frame. It keeps
    2 mm clear of the others, stays near its corner, and lines up with the frame's edges or centre or
    with another surround. The largest is placed first (the legend, as a rule), so it takes the
    freest place, and a smaller one whose corner it took moves out of its way.
    Your favourite arrangement, set with **`layout.arrangement {"favourite": …}`** and kept with
    the app's preferences, is the one tried first. Arrangements of your own are saved by name with
    **`layout.arrangement {"save": "Atlas", "arrangement": {…}}`** (up to 20, kept with the
    preferences too) and removed with `{"remove": "Atlas"}`; `"arrangement": "Atlas"` then takes
    one as it takes a built-in one, and a favourite can name one. `"arrangement": "best"`, the
    default, lays out the favourite, every arrangement by name and every saved one, and keeps the
    one that moves the least
    (by area, so moving the north arrow costs less than moving the legend), then the one leaving
    the least over the data; the favourite wins a tie. `"favourite"` always takes the favourite.
    The answer names the arrangement taken (`"arrangement"`) and the surrounds
    moved off their corners (`"moved"`). Where nowhere is freer, a surround stays. One warning
    names those left with more than a quarter of their area on the data while a corner of their
    size had more than a quarter less, to move by hand if that matters (data under every corner
    alike, such as a context layer filling the frame, isn't worth a warning); two that overlap (a
    frame too small) are said too. To see why a surround moved or stayed,
    **`"coverage": "grid.png"`** writes the grid the placement searched: what the map draws dark,
    the free cells white, the strips the graticule's labels keep tinted blue, and the surrounds
    placed over the map outlined in red. It shows the map's frame only: what sits outside it (a
    side column's locator and legend, as `"style": "panel"` places them, the titles and the
    footer) isn't on it.
    The top visible raster with a colour scale, not drawn by classes (Stretch, a tinted hillshade),
    gets a colorbar beneath the legend; RGB and untinted hillshade do not, so an RGB composite over
    a DEM leaves it to the DEM. A raster drawn by classes (Colormap, Unique Values, Classify) has
    its classes in the legend's rows instead, so a map whose only theme is a land cover gets a
    filled legend and no colorbar; a colour table over more values than a legend lists (over 256, or
    not whole) has neither, and a warning says so. A raster on a ramp on top that is still reading
    gets no colorbar either, rather than the one of a raster under it, and a warning says so too.
    The colorbar's title is the raster's name: the legend's box widens for it, up to its cap (45% of
    the frame's width, or the side column's), and a longer name wraps there, the box taller by its lines.
    `"colorbar": "none"` leaves it out, and the legend's box is sized for the legend alone: hiding
    the colorbar afterwards would leave its room empty in the box. The legend takes the fewest columns, up to three,
    that fit the largest rectangle the map leaves free; when none fits, a long one is laid in more
    when that shows more of it, or as much while covering at least a quarter less of what the map
    draws (the answer's `legendColumns`); in a side column it stays in one. Polygon layers with no
    fill and no labels, such as neighbouring borders, don't count as what the map draws here, unless
    nothing else is shown, nor do polygon layers in one colour with no labels that run on past the
    frame, such as neighbouring states, beside a layer drawn by classes or a raster. When no shape
    of the legend fits, the map is first moved towards an edge, by a quarter, a half, three quarters
    or all of the way the asked extent can go and stay in the frame, so the room on both its sides
    joins (the answer's `mapShift`, in mm right and down, and a warning). Of the moves that need the
    fewest legend columns, the one that leaves the extent's centre nearest the page's visual centre
    (in the middle across, 45% of the way down) is kept: a move away from that centre goes only as
    far as makes room, one towards it as far as keeps it nearest. Of the moves within 1 mm of the
    nearest, the one with the most room is kept. Where the projection stretches, as Web Mercator
    does away from the equator, the
    moved frame is drawn at its new centre's ground scale, and the extent has to stay whole at that
    scale. Only when no move makes room for it, a continuous map with a long legend is veiled: at
    the asked scale, at least 80% of the extent's part of the frame and 30% of the rest are drawn
    (not counting the layers just named), and the legend's box in one column, its colorbar included,
    is taller than a quarter of the frame; a sparser map stays unveiled. The veil covers what is
    outside the extent's bounding box in the map's projection, but for the strips under the
    graticule's labels (so an extent whose edges curve there, as in a conic or polar projection,
    keeps the box's corners outside it unveiled), and the surrounds group in its widest band (the
    answer's `veiled`, `arrangement` `"veil"` and `moved`, and a warning), at up to about half the
    scale on a sheet (each scale tried is rounded up to two significant figures) when the frame
    leaves too little of it, which the result says too. When neither makes room, the map stays
    centred and unveiled, and the legend is laid out as above. The veil and the surrounds are
    ordinary elements, to move or delete as any other; `"veil": false` leaves the map unveiled and
    at its scale. If the legend or colorbar cannot fit, the result warns about clipping or resizing.
    A streamed layer still loading gets a fixed-size legend to adjust by hand.
  - **Context and north**: the layers the placement takes as context around the data (polygons
    only outlined, and polygons in one colour that run on past the frame beside a layer drawn by
    classes or a raster) are left out of the legend too, which a warning names;
    `"contextInLegend": true` lists them. A name is left out only when every layer shown under it
    is context. When the extent spans 20° or more on its longer side (its width taken at its middle
    latitude), the layout has no north arrow: the graticule gives north there, and one arrow can't
    point north across the whole frame. A warning says so; `"northArrow": "always"` draws one, and
    `"none"` leaves it out at any span.
  - **On the globe**: with the map on the Globe projection and no `extent`, the sheet shows the
    globe whole. The globe nearly fills the frame, centred where the view looks, and the
    surrounds go in the room around it. It has no scale bar, since a globe's scale holds only at
    its centre. A sheet has no locator by default either, since it would only show the same globe
    again, smaller; `"locator": "globe"` or `"inset"` still adds one. Warnings say both. Near a
    pole the frame can't draw the globe that small at the view's latitude, so the globe is
    centred as near it as fits, and a warning says where. With an `extent`, a sheet on the globe
    is laid out as on a flat map. In any frame on the globe, the light space around the globe is
    room for the surrounds, as the blank page is.
  - **Legend box**: over the map, the legend's box is left clear where the map shows nothing under
    it but its light plain paper or water: no layer, label, context layer, basemap shape or graticule
    line, no web tiles, and no veil. Dark basemaps keep a white backing so the dark legend text
    stays readable. Over light water its text sits on the page as on open sea. The answer's
    `legendBox` says whether it has one; the clear box is still an element, to fill by hand, and
    `"legendBox": "always"` keeps it white. A legend in a side column or on a card keeps its box
    (`legendBox` is `true`).
  - **Decimal comma**: `"decimal": ","` prints the graticule, scale bar and colorbar numbers and
    `{scale}` with a decimal comma and points between thousands (`0,05`, `1:6.300.000`).
    `layout.page={"decimal": ","}` sets it on any layout; `"."` restores decimal points.
    This changes generated numbers, not numbers typed into text.
  - **Texts**: the title, subtitle, footer lines and authorship are measured with the layout's
    font (the title and headings bold, as drawn) and wrapped to their box, so none runs past it,
    and each box grows to hold them. `{projection}`, `{datum}` and
    `{grid}` are filled from the map when the layout is made. `{date}` and the other dynamic text
    tokens stay live. The reference defaults to the projection, datum and graticule, and the
    authorship to "Made with RustGIS · {date}" (`"author": ""` leaves it out). `headings` renames the two columns.
  - **The locator** (at the frame's top left unless the data is there): `"locator": "globe"` (a
    sheet's default) adds a separate globe map with simplified copies of the map's visible polygon
    layers held in the project. Streamed layers, rasters, points, lines and labels are left out.
    What simplifying changes or leaves alone there (a vertex past a pole taken to the pole, a
    polygon left as it was for a missing coordinate, or too intricate to simplify whole) is said
    in the layout's warnings; one kept as it was because its ring would collapse is not. The
    globe is whole in its circle: near a pole, where it can't be drawn that small at the area's
    latitude, it is centred as near the area as fits, the area still outlined on it, and the
    warnings say so. A locator too small for the whole globe anywhere (a figure's, under about
    20 mm), or one that would have to move so far that the area went behind the globe's rim,
    stays centred on the area, its circle clipping the globe, which the warnings also say.
    It uses the map's basemap, not its web tiles, in a round frame, the globe
    nearly filling it and the area outlined in red; `"inset"` shows the surrounding region (about
    four times the area across) on the map itself, outlined the same way; `"none"` (a figure's
    default) leaves it out. The legend stops short of it; a frame with no room for a legend beside
    it leaves the legend out, with a warning. Undo takes the globe's map away with the layout; a
    globe map no layout shows any more (its layout deleted) is replaced by the next sheet's rather
    than left beside it, which a warning says.
  - **The style** (`"style"`, on any sheet, portrait or landscape): `"classic"` (the default) sets
    the title over the map and the footer's boxes under it; `"banner"` puts the title and subtitle
    in a band of the accent colour across the top, the title in white; `"panel"` sets the map at
    the left and a pale panel at the right with the title, subtitle, locator and legend, and the
    sources and reference at its foot, the note across the bottom; `"editorial"` sets the title
    large in serif and a column at the right under a rule, with a heading (the third of
    `headings`), the note, the locator and the legend; `"modular"` sets cards with rounded
    corners on a light page: the map in one, the locator and the legend in one each beside it,
    the sources and reference in tinted ones under them; `"poster"` colours the whole page, sets
    the title and subtitle in light text across the top, the map in a light card and the footer
    in darker blocks with amber headings. `"accent"` (`#RRGGBB`) colours the band, the panel's
    tint, the headings and rules, and a poster's page (a deep brown, green or blue by default).
    A figure leaves the style to its caption. In the menu, each sheet opens a list of the six
    styles, each saying what it looks like, in its default accent (`"accent"` in the command
    picks another).
  - Every element is an ordinary one, to move or restyle afterwards. The command returns element
    IDs for `layout.update`, and `warnings`. Running it again with another `profile` makes
    another layout of the same map.

  ```
  layout.publication={"profile": "a4-portrait", "extent": [-46.8, -11.6, -38.5, -2.2],
    "title": "PIAUÍ", "subtitle": "Greening and browning of vegetation: NDVI trend, 2001–2025",
    "sources": ["NDVI: MOD13Q1 v6.1 (NASA LP DAAC), via the Brazil Data Cube (INPE)", "Boundaries: IBGE 2022"],
    "note": "Only cells with a significant trend (p < 0.05) are coloured."}
  ```

- **Page colour**: Publish › Page Size › **Page color** paints the whole page a colour under
  every element, in the view and in PNG, PDF and SVG exports; **White** gives the white page back.
  `layout.page={"color": "#0B1B2B"}` sets it, `null` or `"#FFFFFF"` restores white. It is
  opaque: a page has nothing under it to show through. The elements keep their own colours: on a
  dark page, give texts a light colour, and legends, colorbars, tables and charts a light **Ink**
  (below). A map frame keeps its own map's background; give the frame the blank basemap with a
  paper colour to match a coloured page.

- **Map frames**: rectangle, circle and ellipse frames, or polygon and lasso frames drawn on the
  page. Each frame shows a map of the project at its own centre and scale. Turn on **Pan/Zoom
  Map** (Map Frames group, or a frame's properties) to drag inside frames to pan their maps and
  turn the wheel to zoom them about the pointer; Esc turns it off. Without it, Shift-drag a frame
  to pan its map. Double-click a frame to match the map view, or set the scale in its properties:
  from the whole world down to site plans at 1:20, finds at 1:1 and 2:1 (a scale of 1:0.5).
  A frame can be turned and tilted like the map view: a new frame, or one matched to the view,
  takes the view's bearing and pitch, and **Bearing** and **Pitch** in its properties (or
  `layout.update` with `bearing` and `pitch`) set them. Its north arrows turn with it. **Grid** draws a labelled graticule over a frame; **Extent Indicator** outlines another frame's
  extent (an overview map).
- **Frame borders**: the **Border** section of a frame's properties sets the line drawn round it,
  along its shape for a round or polygon frame: its **Color** and **Line width** (dark grey and
  0.8 pt to begin with, up to 20 pt). The line is centred on the frame's edge, as a rectangle's
  outline is, so a wide border reaches half its width past the frame. A width of 0 draws no
  border, so a frame on a coloured page (a blank
  basemap with a paper colour) blends into it; **Default Border** brings the dark grey line back.
  Scripts set the same with `layout.update` and `"border": {"color": "#C8B89A", "width": 0}`
  (the keys not given keep theirs; `null` gives the default back).
- **Extent indicators**: the **Extent Indicator** section of an overview frame's properties sets
  the outline's **Color** and **Line width** (red and 2 pt to begin with), so it can sit quietly on a
  muted page, and **Remove Extent Indicator** takes it away. Scripts set the same with `color`
  and `width` on `layout.addElement` (`kind` `extentIndicator`), or `layout.update` with
  `"extentIndicator": {"color": "#555555", "width": 1}` (`false` removes it). The outline breaks
  where the other frame's edge leaves the Earth (off a globe's disc, or past the edge of its
  projection's world) and runs up to it there, and where the overview's own projection can't show
  the place. An outline across the overview map's edge (on a world projection, the meridian
  opposite its central one) is cut there, each piece on its side of the map, and so is a frame on
  the globe around a pole outlined on Web Mercator, at ±180°.
- **Grids**: the **Grid** section of a frame's properties turns the grid on and sets its
  **Interval** in degrees (**Auto** picks one from the frame's extent), its **Style** (full lines
  across the frame, ticks along its neatline only, or both, with the **Tick length**), its colour
  and **Line width**, and its labels: decimal degrees (`112.5°W`, as many decimals as the
  interval needs) or degrees, minutes and seconds (`112°30′W`), with a hemisphere letter or a
  sign (`-112.5°`), on any of the four edges (longitudes along the top and bottom, latitudes along
  the left and right; the bottom and left to start with), the side labels horizontal or
  running along the edge, and their **Label size** (`"labelSize"`, 4 to 100 points, 8 by default;
  their room from the frame's edge and a measured grid's MGRS square names follow in proportion,
  labels that would then touch, or that reach further in than the frame is across, are left out,
  and `null` gives the default back). From a command:
  `layout.addElement={"kind":"grid","step":0.5,"style":"ticks","format":"dms","edges":["top","bottom","left","right"],"labelOrientation":"vertical"}`
  (given again, it changes only the options it names), or `layout.update` with
  `"grid": {…}` (`false` removes the grid). On a map on the globe the lines are drawn on the
  globe, and stop at its horizon. A frame that shows the whole globe has nothing beyond its rim,
  and so no labels along its edges.
- **Measured grids**: a grid's **Type** is a **Graticule** (latitudes and longitudes, as above)
  or a **Measured grid**, as ArcGIS Pro has them: eastings and northings in a projected coordinate
  system, for maps read with UTM or national grid coordinates (a hiking map at 1:25,000 with a
  line every kilometre). Its **Coordinate system** is the one typed (`EPSG:25832`, WKT or a PROJ
  string); left empty, the map's own when the map is in a projected system, else the UTM zone
  (WGS 84) at the frame's centre. Its **Interval** is in that system's units, metres for UTM
  (**Auto** picks 1, 2 or 5 times a power of ten that suits the frame; one too fine for the frame
  is widened to about 50 lines). Its lines are drawn through the coordinate system onto the map,
  so on a map in another projection they lean or bend as they do there; eastings are labelled
  along the top and bottom and northings along the sides, whole (`543000`) or, with **Short
  labels**, as topographic maps print them, the kilometres' last two digits (`43`). Style, ticks,
  colour, width, edges and side label orientation are as for a graticule. A system that isn't
  projected (a latitude/longitude one) is refused: that is a graticule's job. From a command:
  `layout.update={"element":1,"grid":{"type":"measured","crs":"EPSG:25832","step":1000,"short":true}}`;
  a change of type without a step picks one, as degrees and metres don't carry over.
- **MGRS labels**: a measured grid's **MGRS labels** name each 100 km square that shows with
  its grid zone designation and square letters (`32T MT`), in the middle of what shows of it, and
  label the lines by their kilometres' last two digits (`05`; whole, at an interval of 100 km or
  more), as military and rescue maps print them; **Auto** picks an interval of at most 100 km, so
  the squares' edges are drawn (on a frame over 10,000 km across, where a 100 km grid would be a
  blur, it is widened as for any measured grid). The grid is then in a WGS 84 UTM zone: the one typed as
  its EPSG code (`EPSG:32632`, or 32601–32660 north and 32701–32760 south), else the map's own
  when the map is in one, else MGRS's zone at the frame's centre (southern Norway's 32V and
  Svalbard's wider zones included). Another system is refused while MGRS labels are on, and a UTM
  zone typed as WKT or a PROJ string isn't read as one; a switch to a graticule turns them off. A
  square is named where all of its label fits in it, clear of the edge labels: a frame too small
  for one names none, and nor does a frame north of 84° N or south of 80° S, where MGRS leaves
  UTM for the polar grids (UPS), though its lines are drawn. Across the Equator, the squares on the other side are named as MGRS names
  them there. On a frame across a zone's edge, only the zone the grid is in has its squares named,
  as the lines past its edge are that zone's eastings and northings carried on, not the next
  zone's. From a command: `layout.update={"element":1,"grid":{"type":"measured","mgrs":true}}`. A
  project with MGRS labels on a graticule or naming another system (a hand-edited file) opens with
  them off, and says so.
- **Map surrounds**: north arrow, scale bar, legend, colorbar, table frame (a layer's attribute table) and
  chart frame (a bar chart of a numeric field), plus a neatline or backdrop. A north arrow's
  properties pick its **Style** (classic arrow, compass rose, triangle or star) and its **Fill**
  and **Outline** colours (`"style":"compass","fill":"#B91C1C"`). A scale bar's pick its
  **Style** (alternating bar, stepped line or single line), its **Units** (metres or kilometres
  to suit the scale, or kilometres, metres, miles or feet), its **Divisions** (1 to 10), each
  labelled where there is room, its **Color** (the lines, the dark divisions and the labels), its
  **Fill** (the alternating bar's light divisions) and its **Text size** (`"textSize"`, 4 to 100
  points, 8 by default; the room over the bar and between the labels follows, the bar keeps its
  length and height and moves down, and right, in its box to make room for larger labels, which
  reach no further past its top and left than the default size's, and `null` gives the default
  back) (`"style":"line","units":"miles","divisions":2,"color":"#1F4E79","textSize":12`). A box too
  small for its labels and bar (too short, which draws the bar below it, too narrow for its last
  label, or so narrow that a larger last label runs into the first), or a label that runs off the
  page, is said when its place, size, text size, units, divisions or details change or its frame is
  drawn at another scale, and an export reports it as cut off, as it does a text. A locked scale bar
  refuses a new text size, `null` included. It also takes the details printed maps commonly use: **Subdivisions** (0 to 10: the first
  division after zero cut into unlabelled parts), **Divisions before zero** (0 to 2, counted toward the ten in all, labelled with
  negatives; it and **Divisions** are at most ten together, and more is refused), **Label position** (above or below the bar), a **Unit label** (the unit's name as
  printed: its abbreviation unless set; untick **Show unit label** for none) and its **Unit label
  position** (after the labels, after the bar, or on a line of its own above or below),
  **Decimals** (Automatic, or 0 to 3 fixed with the thousands set apart; Automatic gives each label
  the decimals it needs, as before; the layout's decimal comma applies; `"auto"` or a number, as a
  chart's) and a **Fit** for when round divisions don't fill the box: *Adjust division value* (the
  default) divides the roundest length that fits, so a division may not be round; *Adjust number of
  divisions* keeps a round division and adds or drops divisions, at most ten in all, the divisions
  before zero included, to fill the width;
  *Adjust width* keeps the divisions and draws the bar only as wide as they need
  (`"subdivisions":4,"divisionsBeforeZero":1,"labelPosition":"below","unitLabel":"Kilometres","unitLabelPosition":"afterBar","decimals":1,"fit":"adjustWidth"`).
  Their labels and unit label are drawn at its **Text size**, the lines above the bar reaching no
  higher than its box's top and those below it no lower than its foot, the bar moving to make
  room; a box too short for them all is said, and reported by an export, as for a larger text size.
  A unit label too long for its room is shortened with an ellipsis, or left out when not even its
  first letter fits; that is said, and reported by an export, too.
  Each change is one undo step, and a value set back to its default is no longer saved. A project that uses them can't be opened by an
  older RustGIS, and a layout file (`.rgislayout`) with such a bar lists them in its header, so an
  older RustGIS refuses it as well. A legend's
  parts:
  - **What a legend lists**: what its frame draws: only layers with a feature at the frame's
    scale (through their definition query, display filters and scale range) and, for Unique
    Values, only the classes those features have, with `<all other values>` only when some
    feature falls in it and its symbol is not fully transparent. Graduated Colors and
    Proportional Symbols list their classes as they are (they are taken from the kept features
    when the symbology is applied). A layer that draws nothing there, such as an empty one, is
    left out, in layouts saved before this too. Tick **All classes** in its properties
    (`"allClasses": true`) to list every class of every visible layer instead, and **Hide other
    values** (`"hideOther": true`) to leave `<all other values>` out of the legend in either
    mode (the features it stands for are still drawn).
  - **Show no data** (`"showNoData": true` on `layout.addElement` or `layout.update`) adds a
    **No data** row for a Graduated Colors layer's missing numeric values, using the same
    symbol as the map. It follows the frame's definition query and display filters; with
    **All classes**, it lists the symbol even when unused. It is off by default, including in
    older saved layouts. It changes only the legend; other renderer kinds keep their entries.
  - **Rasters drawn by classes** (Colormap, Unique Values, Classify) are listed under their name
    with the rows the Layers pane shows for them: a swatch of each class's colour and its label,
    the class name in the file when it has one, else the value or the range. The legend lists the
    classes the raster holds (the values in its statistics, without those its mask hides). On a
    raster with too many values to list (over 256, or not whole) it lists every Unique Values
    class, and for Classify the classes from its lowest value to its highest. With **All
    classes** it lists every class of a Unique Values or Classify renderer. A
    colour table lists the values the raster holds either way: its 256 entries have no
    meaningful "all". The rows are the Layers pane's, and the legend also lists them under a mask
    where the pane doesn't yet: a colour table over too many values lists those its mask leaves
    when they are few enough. Rasters on a ramp (Stretch, a tinted hillshade) and RGB composites are left out: a
    colorbar keys the ramp. **Exclude** leaves a classified raster out by its name. A
    cloud-optimised GeoTIFF read in place keeps an overview, and its classes are those of the
    overview, which can miss a rare one the map draws at full resolution; so can the classes of a
    raster under a mask with more than about four million cells, whose masked statistics read a
    sample of them: adding the legend, and `layout.publication`, say so.
  - **Columns** (`"columns"`, 1 to 3): its rows side by side under its title, as even as they go,
    each column as wide as its widest row, with its own column of symbols, and a layer's name
    staying with the row under it. The box keeps its size, so widen it for the columns: in a box
    too narrow they close up and long labels run into the next or past the box, and a box too
    short fills each column in turn and leaves out the last rows.
    A legend says when its box loses rows or closes its columns up when it is given `"columns"`,
    `"textSize"` (`null` included), `"showNoData"` or `"labels"`, and, in columns or at a text
    size of its own, when its box or what it lists changes. A legend in one column at the default
    size is told so only when given one of those.
  - **Text size** (`"textSize"`, 4 to 100 points, 9 by default; a size outside that range is
    refused): the size of its layers' names and classes' labels. Its title, rows, indents and the
    gap between columns follow in proportion, and so do a web layer's legend pictures and the length
    and height of line and polygon swatches, whose lines keep the width the map draws them at (a
    swatch's line or outline that reaches past its row is cut at the row's top and bottom, so it
    doesn't cover the rows beside it, and at its sides where its text starts, the column before it
    or the legend's edge begins, so it doesn't cover its label, another column or what lies past
    the legend). A
    point symbol keeps the size the map draws it at, its row growing only when the symbol needs it,
    so a legend of proportional symbols still matches the map. `"textSize": null` gives the default
    back. A size its box no longer holds says what the legend loses, and so does a box made too
    small for a legend with a size of its own (see Columns for when a legend says so).
  - **Point symbols**: a point layer's symbol is drawn as the map draws it: its marker or the
    stacked marks of its class or layer, its size and outline, the transparency of the layer and
    its groups, and its reference scale at the frame's scale, unturned. A row has one symbol, so
    it leaves out what changes from feature to feature (a symbol by expression, a marker's
    rotation by a field), and it doesn't take the layer's blend mode. The row and the column of
    symbols grow to hold a large one, up to 2048 points, past which the symbol is cut. A legend
    already on the page keeps its size, so with a larger symbol its last rows may be cut and its
    labels run past its right edge: make it larger to fit.
  - **Line and polygon symbols**: a line's swatch is drawn with the stacked strokes of its class or
    layer (casings, offsets, dashes, markers and images along it, by symbol level when the layer
    draws by levels), and a polygon's with its stacked fills (hatches, dots, gradients,
    shapeburst), as the map draws them, with the transparency of the layer and its groups and its
    reference scale at the frame's scale. A stroke wider than the row, or shifted off it, is cut at
    the row; the row keeps its height. A diagram's categories keep their own colour. A layer with
    no stacked strokes or fills keeps its plain swatch, see-through with the layer.
  - **Web layers**: a visible WMS or WMTS layer with a legend is listed by its name over its
    service's legend picture, the one for the frame's scale (as the Layers pane shows it), a
    pixel a point and narrowed to fit the legend's width, in the order the map draws its layers
    (as the Layers pane lists them); one whose picture can't be loaded (or is wider or taller
    than 2048 pixels) is left out. The Layout view does not wait for pictures on either desktop
    or browser: it draws what has arrived, then redraws once the pending legend pictures finish,
    not on every unrelated download. Desktop exports wait up to half a minute in all for each
    page's legend pictures (each page of a map series has its own wait), as they wait for the map's
    tiles. A page drawn while a legend picture is still on its way stops the export, even if the
    picture lands afterwards, rather than leave the file silently incomplete. The browser cannot wait:
    export again once the pictures have arrived.
  - **Its Layers section**: a checkbox for each layer of the frame's map (and each WMS or WMTS
    layer with a legend): untick one to leave it out of the legend while the map still draws it
    (a mask, a highlight, a basemap-like layer). Scripts give the same with
    `"exclude": ["Graticule"]` on `layout.addElement` or `layout.update` (`[]` lists every layer
    again); a renamed layer stays left out.
  - **Labels**: a legend can show a layer under a label instead of its name, so a layer named for
    a plugin or a script (`BrasilData: Setores censitários 2022 (MG)`, `setores_mg_2022_clip`)
    reads well on the page while it keeps its name everywhere else. Scripts give
    `"labels": {"BrasilData: Setores censitários 2022 (MG)": "Setores censitários"}` on
    `layout.addElement` or `layout.update`: the layer by its name (any case), as `exclude` names
    it, and a layer the map hasn't is refused. Each entry replaces that layer's label and leaves
    the others; `null` or a blank label shows the layer's name again, and `{}` every layer's. The
    label goes in the layer's row, or over its classes; a WMS or WMTS layer's goes over its
    picture. It is up to 200 characters (a longer one is refused), and the legend measures it, not
    the name, so sizing a legend to its content and the warnings on its fit follow the label. Two
    legends can label a layer differently (in two languages, say). A renamed layer keeps its label
    (not when another layer has its new name, which is said); a removed one's is dropped, so a later
    layer of that name isn't labelled unasked. Labels are saved with the layout, in the project and
    in a `.rgislayout` file.
- **Colorbar**: the colour scale of a raster layer in the frame's map, as the map draws it (its
  stretch, colour ramp and enhancement): a gradient bar for a stretch or a tinted hillshade,
  steps labelled at their breaks for Classify, and a block per value for Unique Values and colour
  tables. Its properties pick the **Layer** (unless one is picked, the top visible raster with a
  colour scale, not drawn by classes: a legend lists a land cover's classes, and a colorbar
  added over a map with only such rasters draws an empty placeholder and says so), the
  **Title** (the units, such as `Elevation (m)`; the layer's name to start with; a title wider than the colorbar
  wraps at spaces onto more lines, with the bar under them), the
  **Orientation**, which side the **Labels** go on, how many **Ticks** a gradient labels (2 to
  21, the ends included) and **Reverse** (high values first). An RGB raster or a hillshade
  without a tint has no colour scale and shows a placeholder, as does a picked layer that was
  renamed or removed (it is kept by name, like a table or chart frame's layer) until another is
  picked. From a command:
  `layout.addElement={"kind":"colorbar","layer":"DEM","title":"Elevation (m)","orientation":"vertical","ticks":3}`.
- **Shapes**: **Rectangle**, **Circle**, **Ellipse** and **Line** add a shape in the middle of
  the page, to frame, highlight or divide parts of it. The Line menu starts a horizontal, vertical
  or diagonal line; turn it to any angle with its rotation handle or properties.
  A circle is an ellipse in a square box. The properties set a shape's fill, outline and
  outline width (0 for none; a fill with no opacity leaves only the outline), a rectangle's
  rounded corners (`"radius"`, in mm), and a line's colour, width, arrowheads and angle. From a
  command:
  `layout.addElement={"kind":"line","dir":"falling","color":"#C0392B","width":2}`.
  A colour goes only to the elements that draw it, and is refused for the others. `color` is a
  text's, a line's, an arrow's or a scale bar's, and sets a shape's outline unless `outline` is
  also given; when adding, it is also an extent indicator's or a grid's. `fill` is a north
  arrow's, a shape's or a scale bar's, and `outline` a north arrow's or a shape's. A legend,
  picture, table and the like take none.
  A line and the outline of a rectangle, circle or ellipse can be dashed: **Dash** in their
  properties offers the Symbology pane's patterns (Solid, Dashed, Dotted, Dash-dot, Long dash),
  scaled by the width as there, for a dashed leader, a box marking a study area or an inset's
  extent, or a dotted neatline. On an arrow only the shaft is dashed; its heads stay solid. A
  command gives the pattern as a line layer's stroke takes it, lengths in points, dash and gap in
  turn (at most 8, each up to 500): `layout.update={"element":7,"dash":[4,2]}`, with `[]` or
  `null` for solid again. Every export draws it, SVG as a `stroke-dasharray`.
- **Arrows**: **Arrow** adds an arrow pointing right. Drag the round handle above a selected arrow
  or line to turn it about its centre: it snaps to level, upright and diagonal within 3°, and
  **Shift** turns it in 15° steps. **Angle** in its properties sets it exactly (0° points right,
  90° up), and **Arrowheads** puts heads at its end, its start, both or neither, so any line can
  be an arrow. From a command:
  `layout.addElement={"kind":"arrow","angle":30,"length":60}`, or
  `layout.update={"element":7,"angle":-90}` to turn one.
- **Text clipping**: the layout view and PNG, PDF and SVG exports warn when a text element or
  legend title runs off the page, or a text element has more lines than its height allows.
  The notice counts and names the affected elements with their IDs. A line may extend beyond
  its element's width while remaining on the page; that alone is not clipped. Reduce the size,
  add line breaks, turn on **Wrap to the box**, or resize or move the element to keep all its text;
  when every text it names is already wrapped, the notice suggests only the size and the box, and
  a legend's title, which has no wrap, is told to add line breaks. Lines left out below a text's
  box are said as soon as they are: `layout.addElement` and `layout.update` warn how many lines
  don't fit when they set a text, its size, font or fallback font, bold, italic, alignment,
  spacing, line spacing, case, wrap or box, or show it, and the Format pane shows the count under
  **Wrap to the box** (a text wrapped past the 200 lines a text draws counts them as "or more"). A
  text draws 200 lines at most, whatever its box: lines past them are said apart, with the advice
  to split the text into more than one, since a taller box doesn't draw them. A text whose ink
  crosses the page's edge is said in a notice of its own when one of these commands puts it there,
  so a text both cut below its box and past the edge is told each: `layout.addElement` and
  `layout.update` add a text whose ink crosses it, set what its lines or box are, show it, move it
  (`x`, `y`), turn it (`rotation`) or set its vertical alignment (`valign`), and
  `layout.duplicateElement` copies it past the edge, and each advises moving it inside the page,
  resizing it or reducing its size. A move, a turn, an alignment or a copy alone says only that, not
  the lines again; a text turned within the page, or with no letter or ink to show, says nothing.
  Resizing the page (`layout.page`) measures no text, so a text a smaller page cuts is told only
  by the export and the layout view.
- **Text** and **Dynamic Text**: `{date}`, `{time}`, `{scale}`, `{map}`, `{crs}`, `{page}`,
  `{pages}`, `{pageName}` and `{layout}` fill in when the page is drawn; the Format pane's
  **Insert Dynamic Text** menu, under the text, lists each with what it shows. A layer's figures
  fill in too (#2853): `{count:Cities}` is how many features the layer has, and `{sum:Cities:pop}`,
  `{mean:Cities:pop}`, `{min:Cities:pop}` and `{max:Cities:pop}` add up, average or pick the least
  or greatest number of a field, read as a chart reads its values. **Layer statistic** writes them,
  in a layout text's **Insert Dynamic Text** menu and in **Publish › Text › Dynamic Text** (which
  adds a text holding one): a submenu for each layer of the first map frame's map, with **Count**
  and, under each numeric field, **Sum**, **Mean**, **Least** and **Greatest** (raster layers,
  graphics layers and names a token can't hold are not listed). They follow the layer's
  definition query (one that no longer compiles keeps every feature, as the map draws it), take the
  map of the layout's first map frame (in a series, the whole layer, not the page's part of it, so
  a definition query that reads the series page's values keeps nothing there), and are written as a
  chart writes its values: thousands apart, up to two decimals with the zeros at the end dropped,
  and the decimal comma when the layout uses one. Renaming the layer renames it in these tokens, as
  it does for a chart's or table's layer (in the layouts whose first map frame shows its map, and
  in a presentation's map pages of it); a text that a longer name would take past the characters a
  text holds, or that a name with a brace (which a token can't hold) would break, keeps the old
  name, and the rename says so; deleting it leaves them as written. A layer read in place covers
  what is loaded, as a chart does, and a warning says so. A token with no figure (no such layer or
  field, a field with no numbers, a raster layer, or a sum, mean, least or greatest written without
  its field) stays as written, and a warning names what is missing. Both warnings come when the
  text is added or changed, or its layout is made or imported, or when another map frame becomes
  the first (it is given another map, the one before it is deleted or drawn over it, or its map is
  deleted), not when the page is exported. A presentation map page's title is checked against the
  page's map when the page is added with it, when it is changed or when the presentation is
  imported. The text, its size (2 to 400 points, as text graphics on
  the map), colour, bold, **Italic** (slanted, as water and ocean names are; the built-in fonts
  have no italic face, so the letters are slanted 12°), **Alignment** (left, center or right within
  the element) and **Letter spacing** in points (spaced-out names such as O C E A N O, or a
  negative value to tighten) are the same form a text on the map has in the pane; Enter ends the
  text and Shift+Enter starts a new line. Scripts set them with `italic`, `align` and
  `letterSpacing` on `layout.addElement` and `layout.update`.
  A layout's text also takes a **Font**:
  Sans-serif (the default) or Serif, for a title or a classic atlas look. The serif is one installed
  on the computer (Georgia or Times New Roman on Windows and macOS, Noto, DejaVu or Liberation Serif
  on Linux), with its own bold and italic faces, so italic there is a true italic rather than
  slanted letters (a family with no bold or italic file draws that style from its regular face:
  italic slanted, bold as the regular); RustGIS bundles no font. The project saves only the family,
  so it opens anywhere. Where no serif is installed (the web app, a minimal Linux), the text is
  drawn in the sans-serif and the pane says so, as `layout.addElement` and `layout.update` do in a
  `warning`. Scripts set it with `"font": "serif"` or `"font": "sans"`.

  The Font list also has **every font family installed on the computer**, after Sans-serif and
  Serif, so a figure can match the typeface a journal or a series names (Times New Roman, Arial,
  Garamond, a condensed face for long names). Type part of a name to find one. Its bold and italic
  come from the family's own faces, as for the serif, and characters it lacks from the sans. A
  named family keeps a **Fallback**, Sans-serif or Serif, for a computer that doesn't have it (a
  project opened elsewhere, the web app): there the text is drawn in the fallback, and the pane
  and the commands say so. A family that is installed but whose file can't be read (too large,
  damaged) is drawn in the fallback too, and the command that names it says so. The fallback
  starts as the family's own class (a serif's is Serif).
  A text turns with **Rotation**, in degrees clockwise about the centre of its box, as a map
  frame's bearing turns: 90 reads top to bottom and -90 bottom to top, for a source line up the
  margin, a vertical coordinate or a name along a river. The 0°, 90° and -90° buttons beside it set
  those. The element keeps its unturned box (X, Y, Width, Height), so moving and aligning work as
  before; its selection and handles follow the turned box. Every export draws it turned (PNG, PDF,
  GeoPDF, SVG). Scripts set `"rotation": 90` on `layout.addElement` and `layout.update`; an angle
  outside -180 to 180 is kept as the same turn within it (450 is 90).
  **Wrap to the box** breaks a text into lines at spaces to fit the width of its box, for a note,
  a source line or a caption: the lines follow the box when it is resized and the text when it is
  edited or its size changes, so there are no line breaks to put in by hand. It is off by default,
  so a text breaks lines only where it has a line break, as before. Dynamic text wraps once filled
  in, the text's own line breaks are kept, a word wider than the box (a long address) is broken
  across lines, a no-break space keeps its words on one line, and a turned text wraps to the
  width of its unturned box. The layout view and every export (PNG, PDF, SVG) break the same
  lines. Spaces running together in a wrapped text are drawn as one, and no ordinary space is
  kept at a line's start or end. Lines that don't fit the box's height are left out, and the
  commands, the Format pane and the export notice above say so. Scripts set `"wrap": true` on
  `layout.addElement` and `layout.update`. A project with a wrapped text needs this release or a
  later one to open.
  Scripts give the family's name: `"font": "Garamond"`, with `"fallback": "serif"` if wanted;
  `labels.fonts` lists the installed families. The project keeps the name, never a file. A PDF
  sets its text in the letters' own outlines, so it shows the family wherever it is opened, and
  the text can still be searched and copied; no font file goes into it.
  **Vertical alignment** puts a text's block of lines at the **Top** of its box (the default, as
  texts always were), in its **Middle**, or at its **Bottom**, its last line on the box's lower
  edge: a caption under a picture, a note at a panel's foot. A block taller than its box is drawn
  from the top whatever its alignment, so its first lines are kept and the last ones left out, as
  for a top-aligned text, and said. **Line spacing** sets the distance between lines as a multiple
  of the usual (0.5 to 3, default 1): tighter for a long address, looser for a title over two
  lines. **Case** draws a text in **Upper case** or **Lower case** whatever was typed, once its
  dynamic text is filled in (`{map}` and `{date}` too), before it wraps; the text itself is kept
  as typed, so it can be set back to **As typed**. The layout view and every export (PNG, PDF,
  GeoPDF, SVG) draw them alike. Scripts set `"valign": "middle"`, `"lineSpacing": 1.5` and
  `"case": "upper"` on `layout.addElement` and `layout.update`; a name not listed or a spacing
  outside 0.5 to 3 is refused, and `null` leaves each as it is. Each change is one undo step, and
  a setting put back to its default is no longer saved. A project that uses them can't be opened
  by an older RustGIS, and a layout file (`.rgislayout`) with such a text lists them in its
  header, so an older RustGIS refuses it as well.
  **Picture** places a PNG or JPEG.
- **Map Series**: one page per feature of an index layer; see [Map series](#map-series) for setup,
  page names, scales and a locator.
- Click an element to select it, drag to move, drag its handles to resize, and edit it in the
  **Format** pane, which opens as a tab beside the other right-hand panes. Closing the pane keeps
  the element selected; selecting an element, or clicking the selected one, opens it again. The
  wheel zooms the page, and each layout keeps its own zoom and pan while the project is open.
  **Delete** (or **Backspace**, the delete key on a
  Mac) deletes the selected element, and **Ctrl+D** (**⌘D** on a Mac) duplicates it: the copy is
  placed 5 mm down and to the right, on top, and selected. The **arrow keys** nudge it 1 mm, or
  10 mm with **Shift**, and keep it moving while held; a run of nudges is one step to undo.
  **Right-click** an element to select it and open a menu of Duplicate, Delete, the four Arrange
  steps and, on a map frame, **Zoom Frame** (the map view's extent, as a double-click does).
- **Arrange**: elements later in the layout are drawn over earlier ones, and a click picks the one
  on top. Under Duplicate and Delete, **Bring to Front** and **Send to Back** put the selected
  element over or under every other, and **Bring Forward** and **Send Backward** move it one
  place, to put a legend over a map frame, say. A locked element keeps its place. From a command:
  `layout.arrange={"element":4,"to":"front"}` (`front`, `forward`, `backward` or `back`).
- **Locked**, at the top of the Format pane, keeps a finished element from being changed by
  mistake: it can still be selected (a lock marks its corner, and it has no handles), but not
  moved, resized, deleted or edited, and a locked map frame's map isn't panned or zoomed. A map
  series still turns its pages. **Duplicate** makes an unlocked copy, and a map frame isn't
  deleted while one of its surrounds is locked. Scripts lock and unlock with `layout.update`
  `{"locked": true}` or `{"locked": false}`.
- **Background**: every element but a map frame can have a box drawn behind it, which moves,
  resizes, hides and turns with it, in the app and in PDF, PNG and SVG exports: a light box under
  a legend or a scale bar over a busy map, or under a text on a dark page. Turn on **Background**
  in the Format pane, then pick its **Fill** and **Outline** colours, the **Outline width** (0 for
  none), the **Corner radius** and the **Padding**, how far the box reaches past the element on
  every side; the element itself is drawn as before. It starts white with no outline, and a
  duplicate keeps it. From a command:
  `layout.update={"element":4,"background":{"fill":"#FFFFFFCC","radius":1,"padding":1.5}}`; the keys
  not given keep theirs, and `"background":null` removes it. `layout.addElement` takes it too. A
  map frame draws its own map and paper, so a background on one is refused.
- **Ink**: a legend, a colorbar, a table and a chart draw their text and lines in dark grey; turn
  on **Ink** in the Format pane and pick its colour to read them on a dark page or over imagery.
  It starts white over a dark background and dark grey otherwise, and turned off it is the dark
  grey again. A legend's title, layer names and
  class labels, a colorbar's title, outline, ticks and labels, a table's text and a chart's title
  and labels take it; a chart's bars and a colorbar's colours keep theirs. A table with an ink
  draws no white cells of its own, only its header and every other row in a light tint of the ink,
  so it reads on whatever is behind it: give it a **Background** for a box. From a command:
  `layout.update={"element":5,"color":"#FFFFFF"}`, and `"color":null` for the default;
  `layout.addElement` takes it too.
- **Chart frame**: a bar chart of a layer of the frame's map. Its properties pick the **Layer**
  and the numeric **Field**, and by default draw a bar per feature, the largest first, each
  labelled by the **Label** field. **Group by** a field draws a bar per value of that field
  instead, its **Statistic** of the field over that value's features: sum, count (features,
  numbers or not, so a count can chart a text field), mean, minimum or maximum. Values are
  grouped as they are stored, capitals and spaces included: "North" and "north " are two bars.
  Features with no value, or one that is only spaces, are one "(blank)" bar. A text field
  charted that way gives way to the layer's first numeric field when the chart stops grouping
  or counting, and the chart says so. **Order** puts the largest first, the smallest first, the
  labels A to Z or the bars in the layer's order. **Bars** sets how many are drawn (1 to 100,
  20 by default); **Past the limit** leaves the rest out, which the chart says, or gathers them
  in a last **Other bar**, its statistic over their features (their sum when not grouped), last
  whatever the order. A negative value is drawn the other way from a line at zero. Features
  with no number in the field are left out, and the chart says how many; a count counts them,
  since it reads no number. From a command:
  `layout.update={"element":6,"group":"region","statistic":"mean","order":"category","bars":8,"other":true}`,
  and `"group":""` for a bar per feature again; `layout.addElement` takes them too.
- **Chart look**: **Orientation** draws bars running across (the default) or **Vertical**
  columns standing on the bottom, each label centred under its column and cut to its width.
  **Title** is the one the chart is given (its field, or the statistic of its field by the
  group), **None**, which leaves its room to the bars, or **Own**, typed in **Title text**.
  **Values** writes each bar's value past its end, its thousands apart with commas (points
  with a layout's decimal comma), with up to two **Decimals** by default, the zeros they end
  in dropped, or a fixed 0 to 6; on narrow columns, or beside bars they would leave less than
  half the chart, they are written smaller to fit. **Bar color** sets the bars' colour.
  **Color by class** draws each bar in the colour its features have on the map, by the layer's
  symbology: a bar per feature in its feature's, a grouped bar in the colour its features share,
  so grouping by the field the layer's Unique Values class by gives each bar its class's colour.
  The colour is a symbol's fill (an outline's where the fill is clear, a line's stroke), by
  each feature's own geometry, as the chart's map frame draws it (at its scale, and on its
  series page), not its patterns or stacked strokes, nor the layer's transparency: the bars
  are drawn whole. Features of a hidden class, or points drawn as an image, show none and
  leave the bar to the others. A bar whose features have several colours or show none,
  and an Other bar, keep the bar colour, and the chart says how many bars do, and why, when it
  is added or changed. The bars follow the symbology when it changes; those counts are not said
  again then. The bars take the layer's colours whether the frame draws the layer or not: a
  layer turned off, or outside its visible scale range, still colours them.
  **Labels** puts the labels of bars running across at the left of their column, or **By the
  bars**, against them. Keep a title out of the chart to style it as you like with a text
  element. From a command:
  `layout.update={"element":6,"orientation":"vertical","values":true,"decimals":0,"title":"","barColor":"#C2185B","labelAlign":"right"}`,
  `"title":null` for the title it is given again and `"barColor":null` for the default blue;
  `"classes":true` colours the bars by class;
  `layout.addElement` takes them too.
- **Stacked bars**: a chart that groups and sums or counts can **Stack by** another field. Each
  bar is then split by that field's values, a stretch per value, and a key under the title names
  them: the population of each continent split by income group, for example. The stretches add
  up to the bar, so the row is shown only for a sum or a count. The key lists the values the
  bars drawn show, by their total over them, the largest first (A to Z when the bars go by
  label); past 8 values, it keeps the 7 with the largest totals and gathers the rest in a grey
  **Other**, and the chart says how many it gathers. The stretches take the colours diagrams
  take, or, with **Color by class**, the colour each value's features share on the map, so
  stacking by the field the layer's Unique Values class by gives each stretch its class's
  colour; a value whose features have several colours, or show none, keeps its palette colour,
  and the chart says how many do. Stretches below zero run the other way from the zero line, and
  **Values** writes the bar's total. An Other bar is stacked too, from the bars it gathers. A
  box too short to leave the bars room under the title and key shows the chart's placeholder,
  and the chart says so when it is added, resized, retitled or turned (not when the layer's data
  changes): make it taller. A stack field deleted from the layer leaves the bars whole, and the
  chart's next change lets the stack go, saying so. Turning grouping off, taking a mean, minimum
  or maximum, or moving the chart to a layer without the field lets the stack go, and the chart
  says so. From a command:
  `layout.update={"element":6,"group":"continent","stack":"income_grp"}`, and `"stack":""` for
  whole bars again; `layout.addElement` takes it too.
- **Facing charts**: two charts side by side compare two fields per category back to back, such
  as the share of businesses against the share of people in each district. **Mirror** draws a
  chart as its mirror image, its text reading as ever: bars running across start from the right
  with their labels at the right, and columns hang from the top with their labels over them.
  **Maximum** fixes the value at the far end of the bars' room (**Automatic**, the default, takes
  the largest bar, where a fixed one starts), so two charts given the same one share a scale; a
  bar past it runs to that end, and the chart says how many do. **Hide labels** leaves the labels
  out and gives their room to the bars. Mirror the left chart, leave the right one as it is and
  hide its labels: the categories are then named once, between the two. From a command:
  `layout.update={"element":6,"mirror":true,"max":40,"hideLabels":true}`, `"max":"auto"` (or
  `null`) for the bars' own again; `layout.addElement` takes them too.
- **Bar outline**: a chart's **Bar outline** draws a line round each bar, and between the
  stretches of a stacked one, in the colour and width beside it: dark grey and half a point when
  first turned on, up to 5 points. Light bars on a white page, or bars a shade apart, then keep
  their edges. The line runs just inside each bar, so it never covers the values, labels or title,
  and it narrows on a bar or a stretch thinner than three times its width, so their colours still
  show. From a command:
  `layout.update={"element":6,"barOutline":{"color":"#333333","width":1}}`, the keys not given
  keeping the outline's, and `"barOutline":null` (or a width of 0) for none; `layout.addElement`
  takes it too.
- **Rulers and guides**: rulers start on along the top and left and show millimetres from the page's corner.
  Drag out of a ruler to add a guide, drag a guide to move it, and drag it back onto a ruler to
  remove it. Guides are saved with the layout but never printed or exported. **Clear Guides**
  removes them all, and **Rulers** in the Page group hides the rulers.
- **Snap** starts on: a dragged or resized element snaps to the guides, the page's edges and centre, and
  the other elements' edges and centres, and the line it snapped to is highlighted. Hold Alt to
  place an element freely, or turn **Snap** off in the Page group.
- **Export** writes PDF (every map series page by default, up to 500), or PNG or SVG of the current page.
  On desktop, **Cancel** also stops an export while it waits for legend pictures or map tiles;
  a cancelled export does not replace the destination file.
  A PDF keeps the page as vectors: text, shapes, surrounds and map frames (their features, labels
  and grids, turned with the frame) stay sharp at any zoom or print size, and linear and radial
  gradient fills are PDF gradients. What is pixels by nature is a picture in it at the export's
  `dpi`, keeping its layer's opacity and blend mode: rasters, web tile layers, pictures, legend
  images from map services, a masked layer, and a frame that is tilted, on the globe or with view
  effects. Hatch and dot fills, shapeburst fills, and sweeping, repeating or reflected gradients
  are pictures cut to their shape. The `dpi` also sets how fine the vectors' detail is kept, as a
  PNG at that resolution would show it. A frame or page whose vectors would pass 256 MB (64 MB
  in the browser) is drawn as a picture instead, and the export says which.
  Its text is set in the fonts it is drawn in, each embedded as a subset of its glyphs, so a vector
  editor that reads embedded fonts (VectorCraft, where it was checked) opens it as text that can be
  edited; it can be searched and copied in any PDF reader. A face is set as its letters' outlines
  instead, and the export names it with why, when its licence forbids embedding or subsetting or
  allows embedding bitmaps only; when it is drawn at another weight or width than its variable font
  file's default; when its file doesn't hold it as drawn; when its file can't be subset (a damaged
  file, or a CFF2 variable font); when its subset would pass 8 MB; or when the document's
  embedded faces would pass 32 MB.
  A label's halo is a copy of its text under it, filled and stroked as wide as the halo, so it is
  text too (marked as decoration, an artifact, for readers that leave those out of what they copy).
  `"pdf": {"text": "outlines"}` on `layout.export` sets all the text, halos too, as outlines, as a
  print shop asking for outlined type wants.
  Its page has layers (PDF optional content groups), which a vector editor that reads them opens
  as layers (VectorCraft, where it was checked), and PDF readers list in their layers panel:
  one for each element, named after it, and inside a map frame's, one for each map layer and one
  for each layer's labels (`Labels: <layer>`), and one for the frame's grid. A map series' pages
  share them. `"pdf": {"layers": false}` on `layout.export` leaves them out. The map view's PDF
  (Publish › Print Map) has its map layers and labels as layers too.
  A PDF is also a GeoPDF: each map frame is georeferenced (an ISO 32000 geospatial viewport, the
  form GDAL, QGIS, Acrobat and Avenza read), with its map's coordinate system as WKT and its EPSG
  code when it has one, so a reader shows coordinates and measures in it, and GDAL or QGIS open the
  page as a georeferenced raster. It carries coordinates only: no layers and no feature
  attributes. A tilted frame, one on the globe, and one with a corner off the Earth aren't
  georeferenced, and the export says which (in a map series, on which pages when not on all: each
  page moves the frame). Where a frame is drawn over another, only the one on top carries
  coordinates there (none when it isn't georeferenced), so a reader doesn't show the
  other's through it. A viewport is a rectangle, so a shaped frame (circle, ellipse, polygon)
  drawn over another is left out, and the frame under it has no coordinates in its box. A frame in a projection with no WKT form is
  georeferenced by its corners in longitude/latitude when that puts the places on a 9 × 9 grid
  over it within 0.5 mm of where they are drawn (measured there, not proven between them), and
  left out otherwise. **Georeference PDF** in the Publish tab's
  Export group (`"georeference": false` for `layout.export`) leaves the coordinates out.
  In SVG each element is a group (an Inkscape layer) named after it: text stays text and rectangles,
  ellipses and lines stay shapes, and map frames and the other surrounds are kept as vectors as in a
  PDF (their labels as the letters' shapes, each run labelled with its text), so the page can be
  finished in a vector editor. What a PDF keeps as pictures is a picture in the SVG too, at the
  export's `dpi`, and a frame whose vectors would pass the budget is drawn as a picture, with a
  notice. A picture marker's image is written once and placed with `<use>` wherever drawn, so a
  vector editor shows each place as a clone of it. A text element uses the viewer's
  font (a named family where the viewer has it, else its serif or sans-serif), so its alignment
  and line widths can differ from PNG/PDF and can
  change when edited. A picture standing for a whole element is clipped to its box;
  `layout.export` accepts `dpi` and `"allPages": false` for just the current PDF
  page. **Print** opens the PDF in the system viewer on desktop; **Save Layout File** and
  **Import Layout** move layouts between projects (`.rgislayout`).
- **Capture To Clipboard** copies an image of the current page at 192 dpi, with any text still being
  typed. A page that would pass 16 million pixels, or 12,000 pixels on a side, is copied at a lower
  dpi, and the notice says so; a page that would need less than 36 dpi is too large to copy, so
  export it instead.

## Map series

Select the main map frame, then **Publish › Map Frames › Map Series** and choose its index layer
(tracts, for example). The series follows the layer's feature order. **Previous Page** and
**Next Page** step through it; **Refresh** refits the current page after changes to the index layer.

Use `layout.series` to set a page-name field and margin; for example, on the active layout:

```text
layout.series={"layer":"Tracts","nameField":"GEOID","margin":0.1}
```

Without `frame` (the frame's element id), the command uses the layout's first map frame, so
name the frame when the series is on another one. Running `layout.series` again starts the series
over at its first page. `nameField` supplies
`{pageName}` in dynamic text; if omitted, it uses the index layer's label field, or the row number
(starting at 1) when that field is unavailable or its value is null, empty or whitespace-only.
Add a text element such as
`Tract {pageName} · {page} of {pages}`. In commands, `layout.seriesPage={"page":0}` is the
first page; the displayed `{page}` starts at 1.

The frame fits each feature's extent anew, so its scale varies by page. `margin` is a fraction
of the feature's longitude/latitude extent added on **each side**: `0.1` (the default) adds
10% of its width on the left and right, and 10% of its height above and below, with at least
0.01° on each side. Bind a scale bar to the series frame; `{scale}` text uses the layout's first
map frame, so keep the series on that frame when using it. A feature without geometry or with
an empty extent leaves the frame at the preceding view.

For a locator, add a second map frame and pan/zoom it to the whole study area. Select that
locator frame, then **Publish › Map Frames › Extent Indicator** and choose the main frame.
The locator outlines the main frame's changing extent as the series turns its pages.

**Export Layout** writes all pages to PDF by default, but only the **first 500** when there are
more, with a warning. Split longer books' index data into layers of at most 500 features.
`layout.export` with `"allPages":false` writes only the current PDF page; PNG and SVG always
write the current page.

### Mark the page's own feature

Set [Symbols by Expression](cartography.md#symbols-by-expression) on the index layer. Use
`$id = @page_feature_id` to give the current feature a heavier outline and draw the rest in gray.
For an index layer called **Tracts**, the command is:

```text
layer.symbolBy={"layer":"Tracts","strokeWidth":"if($id = @page_feature_id, 3, 0.6)","stroke":"if($id = @page_feature_id, '#174a7e', '#9ca3af')","fill":"if($id = @page_feature_id, '#8bbce8', '#e5e7eb')"}
```

Use a style rather than a definition query: a definition query such as `$id = @page_feature_id`
leaves the layer empty in the map view, where the variables are null. Step through the pages to see the
style follow the index feature. The expression also works in a
locator frame that shows the index layer. Feature IDs are local to each layer, so use the ID
comparison on the index layer. Outside a series page the comparison never matches, and every
feature gets the gray style.

## Reports

**Publish › New Report** lists the selected layer's records page after page; the **Report** pane
previews it and sets it up.

- **Data**: pick the detail **Fields** (or **Autofill Fields**), **Group** records by up to four
  fields (each group gets a heading and its own statistics), add **Statistics** (count, sum,
  mean, minimum, maximum), a **Dynamic Picture** per record (a field holding a PNG/JPEG path relative to the saved
  project's folder, or base64 image data), **Attachments** (a field listing each record's files,
  separated by `;`; image files show when they're inside the project's folder), and a **Chart Frame**
  (records, or a field's sum, per category).
- **Report Section**: **Supplemental Pages** of text before or after the records, **New
  Template** and **Existing Template** (a page layout drawn as a report page), a **Related
  Report** appended after this one, and **Subreports** (each record's related records through a
  relationship, listed beneath it).
- A where clause or the layer's selection limits the records; a sort field orders them within
  their groups.
- **Publish › Report**: **Export Report** writes a PDF, kept as vectors as a layout's is (or
  the first page as PNG); **Save As Report
  File** keeps the report, and **Report Template** its design without the data source, to start
  reports on other layers (**Publish › Import Report**).

## Presentations and notebooks

**Publish › New Presentation** starts a presentation with a title page; the **Presentation** pane
previews, edits and plays it.

- **Page**: a **Map** page shows the map view as it is now, a **Blank** page shows text on a
  coloured page, an **Image** page a picture with a caption, and a **Video** page a video file or
  URL with an optional poster picture.
- Each page has a title, speaker notes and a duration (for **Play**), and can move up or down.
- **Publish › Output › Presentation › Export Presentation** writes an HTML slideshow (arrow
  keys, `N` for notes, `P` to play; videos play in place), a PDF (kept as vectors as a layout's
  is), or the first page as PNG.
  Video sources remain file paths or URLs, so keep them accessible beside the exported slideshow.
  **Save As Presentation File** in the same menu keeps it to import into other projects (`.rgispres`).

**Analyze › New Notebook** starts a notebook of text and command cells; **Add and Open** opens a
RustGIS (`.rgisnb`) or Jupyter (`.ipynb`) notebook. A command cell runs one command per line
(`id {json}` or `id={json}`, as in scripts) and shows each command's result beneath it. Notebooks save in
either format. The Notebook pane opens beside the map, on half of the room right of the left
pane (at least 320 points where the window allows); drag its edge to give the notebook or the
map more room.
The cells of a RustGIS notebook contain RustGIS commands, not Python, even in an `.ipynb` file.
A Jupyter notebook in another language, such as Python, or one that names no language, opens with
its cells and outputs (text, results, errors and PNG images) but its code cells don't run here;
saving it as `.ipynb` keeps its kernel, metadata, cell ids and outputs, so it still opens and runs
in Jupyter. RustGIS marks the `.ipynb` files it saves with the `rustgis` language, so their command
cells run when opened again. A notebook holds up to 1,000 cells of up to 200,000 characters each;
a larger one is refused rather than opened in part. Notebooks opened from a file or an existing
project cannot run commands that read or write files or run other scripts.
