# Layouts and Reports

## Layouts

<figure markdown="span">
  ![A landscape A4 page layout with a map frame, title, legend, north arrow and scale bar, and the Publish tab open](https://assets.rustgis.app/images/layout.webp)
  <figcaption>A page layout with a map frame, title, legend, north arrow and scale bar.</figcaption>
</figure>

Home › New Layout adds a page layout (Letter, Legal, Tabloid or A5 to A0, landscape or portrait)
with a map frame, title, legend, north arrow, scale bar and a dynamic credits line. The command
`layout.new` also accepts custom `width` and `height` in millimetres and `"template": "blank"`.
Layouts open as tabs next to the maps; while one is open, its groups lead the Publish tab:

- **Publication layouts**: Home › New Layout › **Publication Layout** builds a finished sheet or
  figure of the map view in one step, with `layout.publication` behind it:
  - **Sheets** (A4, A3 or A2, portrait or landscape): a frame and margin, the title and subtitle,
    the map, and a footer of two boxed columns (sources, and the cartographic reference; one
    with nothing in it is left out, the other taking the width) with a note and the authorship
    under them. Text sizes follow the sheet: an A3's are 1.41 times an
    A4's. Given its size alone (`"a4"`, the default, `"a3"` or `"a2"`), a sheet is turned to
    portrait or landscape, whichever the map comes out larger on: the area's extent east to west
    against north to south, on the map's projection, with the room the style leaves the map. The
    result names the profile chosen (`"profile": "a4-landscape"`); give it with its orientation
    (`"a4-portrait"`) to have the other.
  - **Figures** for a journal (90, 140 or 190 mm wide, as tall as the map's shape asks) or a
    thesis (160 × 225 or 160 × 110 mm): the map edge to edge with a line of its projection and
    datum under it, at 7 pt. A figure's title and sources belong in its caption, so they are
    left out, and the result says so.
  - **The map**: the `extent` given (or the view) fitted north-up, without tilt, at a round scale,
    two significant figures, rounded up so it still fits within the strips the graticule's labels
    take inside the frame. A graticule's step follows the extent (2° for a state, 15′ for a city),
    with labels on every edge of a sheet.
    The north arrow, the legend (sized to what it lists), the scale bar with the numeric scale
    under it and the locator sit in the frame's corners, clear of the graticule's
    labels. The **arrangement** says which corner is each one's. `classic` (the default favourite) puts the
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
    the app's preferences, is the one tried first. `"arrangement": "best"`, the default, lays out
    the favourite and every arrangement by name, and keeps the one that moves the least
    (by area, so moving the north arrow costs less than moving the legend), then the one leaving
    the least over the data; the favourite wins a tie. `"favourite"` always takes the favourite.
    The answer names the arrangement taken (`"arrangement"`) and the surrounds
    moved off their corners (`"moved"`). Where nowhere is freer, a surround stays. One warning
    names those left with more than a quarter of their area on the data while a corner of their
    size had more than a quarter less, to move by hand if that matters (data under every corner
    alike, such as a context layer filling the frame, isn't worth a warning); two that overlap (a
    frame too small) are said too.
    The top visible raster gets a colorbar beneath the vector legend if its renderer has a colour
    scale; RGB and untinted hillshade do not. `"colorbar": "none"` leaves it out, and the legend's
    box is sized for the legend alone: hiding the colorbar afterwards would leave its room empty
    in the box. The legend takes the fewest columns, up to three,
    that fit the largest rectangle the map leaves free; when none fits, a long one is laid in more
    when that shows more of it, or as much while covering at least a quarter less of what the map
    draws (the answer's `legendColumns`); in a side column it stays in one. Polygon layers with no
    fill and no labels, such as neighbouring borders, don't count as what the map draws here, unless
    nothing else is shown. If the legend or colorbar cannot fit, the result warns about clipping or
    resizing. A streamed layer still loading gets a fixed-size legend to adjust by hand.
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
- **Extent indicators**: the **Extent Indicator** section of an overview frame's properties sets
  the outline's **Color** and **Line width** (red and 2 pt to begin with), so it can sit quietly on a
  muted page, and **Remove Extent Indicator** takes it away. Scripts set the same with `color`
  and `width` on `layout.addElement` (`kind` `extentIndicator`), or `layout.update` with
  `"extentIndicator": {"color": "#555555", "width": 1}` (`false` removes it).
- **Grids**: the **Grid** section of a frame's properties turns the grid on and sets its
  **Interval** in degrees (**Auto** picks one from the frame's extent), its **Style** (full lines
  across the frame, ticks along its neatline only, or both, with the **Tick length**), its colour
  and **Line width**, and its labels: decimal degrees (`112.5°W`, as many decimals as the
  interval needs) or degrees, minutes and seconds (`112°30′W`), with a hemisphere letter or a
  sign (`-112.5°`), on any of the four edges (longitudes along the top and bottom, latitudes along
  the left and right; the bottom and left to start with), and the side labels horizontal or
  running along the edge. From a command:
  `layout.addElement={"kind":"grid","step":0.5,"style":"ticks","format":"dms","edges":["top","bottom","left","right"],"labelOrientation":"vertical"}`
  (given again, it changes only the options it names), or `layout.update` with
  `"grid": {…}` (`false` removes the grid).
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
- **Map surrounds**: north arrow, scale bar, legend, colorbar, table frame (a layer's attribute table) and
  chart frame (a bar chart of a numeric field), plus a neatline or backdrop. A north arrow's
  properties pick its **Style** (classic arrow, compass rose, triangle or star) and its **Fill**
  and **Outline** colours (`"style":"compass","fill":"#B91C1C"`). A scale bar's pick its
  **Style** (alternating bar, stepped line or single line), its **Units** (metres or kilometres
  to suit the scale, or kilometres, metres, miles or feet), its **Divisions** (1 to 10), each
  labelled where there is room, its **Color** (the lines, the dark divisions and the labels) and
  its **Fill** (the alternating bar's light divisions)
  (`"style":"line","units":"miles","divisions":2,"color":"#1F4E79"`). A legend's
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
  - **Columns** (`"columns"`, 1 to 3): its rows side by side under its title, as even as they go,
    each column as wide as its widest row, with its own column of symbols, and a layer's name
    staying with the row under it. The box keeps its size, so widen it for the columns: in a box
    too narrow they close up and long labels run into the next or past the box, and a box too
    short fills each column in turn and leaves out the last rows.
    Giving `"columns"`, or changing the box or what a legend in columns lists, says when its box
    loses rows or closes the columns up. A legend in one column is told so only when given
    `"columns"`.
  - **Point symbols**: a point layer's symbol is drawn as the map draws it: its marker or the
    stacked marks of its class or layer, its size and outline, the transparency of the layer and
    its groups, and its reference scale at the frame's scale, unturned. A row has one symbol, so
    it leaves out what changes from feature to feature (a symbol by expression, a marker's
    rotation by a field), and it doesn't take the layer's blend mode. The row and the column of
    symbols grow to hold a large one, up to 2048 points, past which the symbol is cut. A legend
    already on the page keeps its size, so with a larger symbol its last rows may be cut and its
    labels run past its right edge: make it larger to fit.
  - **Web layers**: a visible WMS or WMTS layer with a legend is listed by its name over its
    service's legend picture, the one for the frame's scale (as the Layers pane shows it), a
    pixel a point and narrowed to fit the legend's width, in the order the map draws its layers
    (as the Layers pane lists them); one whose picture can't be loaded (or is wider or taller
    than 2048 pixels) is left out. Exporting waits up to half a minute for the pictures, as it
    waits for the map's tiles; in the browser the page is drawn with those that have arrived,
    and again as the others arrive.
  - **Its Layers section**: a checkbox for each layer of the frame's map (and each WMS or WMTS
    layer with a legend): untick one to leave it out of the legend while the map still draws it
    (a mask, a highlight, a basemap-like layer). Scripts give the same with
    `"exclude": ["Graticule"]` on `layout.addElement` or `layout.update` (`[]` lists every layer
    again); a renamed layer stays left out.
- **Colorbar**: the colour scale of a raster layer in the frame's map, as the map draws it (its
  stretch, colour ramp and enhancement): a gradient bar for a stretch or a tinted hillshade,
  steps labelled at their breaks for Classify, and a block per value for Unique Values and colour
  tables. Its properties pick the **Layer** (the top visible raster unless one is picked), the
  **Title** (the units, such as `Elevation (m)`; the layer's name to start with), the
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
- **Arrows**: **Arrow** adds an arrow pointing right. Drag the round handle above a selected arrow
  or line to turn it about its centre: it snaps to level, upright and diagonal within 3°, and
  **Shift** turns it in 15° steps. **Angle** in its properties sets it exactly (0° points right,
  90° up), and **Arrowheads** puts heads at its end, its start, both or neither, so any line can
  be an arrow. From a command:
  `layout.addElement={"kind":"arrow","angle":30,"length":60}`, or
  `layout.update={"element":7,"angle":-90}` to turn one.
- **Text** and **Dynamic Text**: `{date}`, `{time}`, `{scale}`, `{map}`, `{crs}`, `{page}`,
  `{pages}`, `{pageName}` and `{layout}` fill in when the page is drawn; the Format pane's
  **Insert Dynamic Text** menu, under the text, lists each with what it shows. The text, its
  size (2 to 400 points, as text graphics on the map), colour, bold, **Italic** (slanted, as water
  and ocean names are; the built-in fonts have no italic face, so the letters are slanted 12°),
  **Alignment** (left, center or right within the element) and **Letter spacing** in points
  (spaced-out names such as O C E A N O, or a negative value to tighten) are the same form a text
  on the map has in the pane; Enter ends the text and Shift+Enter starts a new line. Scripts set them with `italic`, `align` and
  `letterSpacing` on `layout.addElement` and `layout.update`. A layout's text also takes a **Font**:
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
  Scripts give the family's name: `"font": "Garamond"`, with `"fallback": "serif"` if wanted;
  `labels.fonts` lists the installed families. The project keeps the name, never a file. A PDF
  sets its text in the letters' own outlines, so it shows the family wherever it is opened, and
  the text can still be searched and copied; no font file goes into it. **Picture** places a PNG
  or JPEG.
- **Map Series**: one page per feature of an index layer, with the frame zoomed to each feature;
  step through pages, refresh, and export every page into one PDF.
- Click an element to select it, drag to move, drag its handles to resize, and edit it in the
  **Format** pane, which opens as a tab beside the other right-hand panes. Closing the pane keeps
  the element selected; selecting an element, or clicking the selected one, opens it again. The
  wheel zooms the page, and each layout keeps its own zoom and pan while the project is open.
  **Delete** (or **Backspace**, the delete key on a
  Mac) deletes the selected element, and **Ctrl+D** (**⌘D** on a Mac) duplicates it: the copy is
  placed 5 mm down and to the right, on top, and selected. The **arrow keys** nudge it 1 mm, or
  10 mm with **Shift**, and keep it moving while held; a run of nudges is one step to undo.
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
- **Rulers and guides**: rulers start on along the top and left and show millimetres from the page's corner.
  Drag out of a ruler to add a guide, drag a guide to move it, and drag it back onto a ruler to
  remove it. Guides are saved with the layout but never printed or exported. **Clear Guides**
  removes them all, and **Rulers** in the Page group hides the rulers.
- **Snap** starts on: a dragged or resized element snaps to the guides, the page's edges and centre, and
  the other elements' edges and centres, and the line it snapped to is highlighted. Hold Alt to
  place an element freely, or turn **Snap** off in the Page group.
- **Export** writes PDF (every map series page by default, up to 500), or PNG or SVG of the current page.
  A PDF keeps the page as vectors: text, shapes, surrounds and map frames (their features, labels
  and grids, turned with the frame) stay sharp at any zoom or print size, and linear and radial
  gradient fills are PDF gradients. What is pixels by nature is a picture in it at the export's
  `dpi`, keeping its layer's opacity and blend mode: rasters, web tile layers, pictures, legend
  images from map services, a masked layer, and a frame that is tilted, on the globe or with view
  effects. Hatch and dot fills, shapeburst fills, and sweeping, repeating or reflected gradients
  are pictures cut to their shape. The `dpi` also sets how fine the vectors' detail is kept, as a
  PNG at that resolution would show it. A frame or page whose vectors would pass 256 MB (64 MB
  in the browser) is drawn as a picture instead, and the export says which.
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
  notice. A text element uses the viewer's
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
(`id {json}`, as in scripts) and shows each command's result beneath it. Notebooks save in
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
