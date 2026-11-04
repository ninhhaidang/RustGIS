---
name: print-layout
description: Make a printable page layout — map frame, title, legend, north arrow, scale bar — and export it to PDF or PNG. Use when asked for a "layout", "print", "PDF", "poster" or a map "with a legend".
---

# Print layout

1. Get the map right first: symbology, labels and extent. A layout's map frame shows the map's current view.
2. For a finished sheet or a figure for a paper in one step, use `layout.publication` with a `"profile"`
   (`"a4"`, `"a3"` or `"a2"` for a sheet turned to the area's shape, or `"a4-portrait"`, `"a3-landscape"`, … to choose; `"figure-1col"`, `"thesis-full"`, … for a figure), the `"extent"`
   to fit, and the texts: `"title"`, `"subtitle"`, `"sources"`, `"note"`, a `"locator"` (`"globe"`: the visible polygon layers held in the project, not streamed ones, over the basemap; `"inset"` or `"none"`; when omitted, `"globe"` on a sheet and `"none"` on a figure), and `"decimal": ","` for a map in a language with a decimal comma. A sheet's `"style"` arranges it: `"classic"` (the default), `"banner"`, `"panel"`, `"editorial"`, `"modular"` or `"poster"` (for a poster-like sheet), with an `"accent"` colour as `"#RRGGBB"`. Its
   surrounds (locator, north arrow, legend, scale bar) start from an `"arrangement"` (`"classic"`, `"mirrored"`, `"legendRight"`, `"legendTop"`, or a corner for each; `"best"`, the default, tries the user's favourite, set with `layout.arrangement`, and the others), and one over the data moves to the freest place in the frame, the largest first (the legend, as a rule); read its `"arrangement"`, `"moved"` and `"warnings"`. It places the frame at a round scale, the
   graticule, legend, scale bar and north arrow, and wraps the footer. Its result lists every element's id and name
   (`"elements"`), so skip to step 5 to adjust them. Otherwise,
   `layout.new` `{"name": N, "page": "letter"|"a4"|…, "orientation": "landscape"|"portrait"}` creates the layout and
   opens it.
3. `layout.addFrame` for the map frame (`search_commands` "map frame" for its position and size parameters, in
   millimetres from the page's top-left).
4. `layout.addElement` for each element with a `kind`: `"text"` (the title, large and bold, at the top), `"legend"`,
   `"northArrow"`, `"scaleBar"`, and optionally `"dynamicText"` (the date or a source line). Keep a margin of about
   10 mm. The legend and scale bar usually go along the bottom or the right edge. A north arrow takes a `"style"`
   (`"classic"`, `"compass"`, `"triangle"`, `"star"`); a scale bar `"units"` (`"auto"`, `"kilometers"`, `"meters"`,
   `"miles"`, `"feet"`), `"divisions"` and a `"style"` (`"alternating"`, `"stepped"`, `"line"`). `"grid"` adds a
   graticule to the frame (`"style": "ticks"` for neatline ticks, `"format": "dms"`, `"edges"`).
5. Adjust with `layout.update` (`{"element": id, "x", "y", "w", "h", "text", "size", "bold"}`). The ids come from the
   results of the earlier calls.
6. `layout.export` `{"path": "map.pdf"}`, or `.png` with a `dpi` (300 for print). Exporting writes a file, so the
   user is asked first. `layout.close` goes back to the map.

Tell the user the page size and what's on it, and where the file is.
