---
name: geoprocessing
description: Run geoprocessing tools and chain them — buffer, clip, erase, split, dissolve, join attributes, select by location or attributes, summarize within, summary statistics, simplify, export features, kernel density. Use for "within N km of", "inside", "merge by", "count per", "overlay", "join tables", "density surface", or any multi-step analysis.
---

# Geoprocessing

Based on the RustGIS user guide, Analysis › Geoprocessing tools
(`read_doc` `{"page": "user-guide/analysis.md", "section": "Geoprocessing tools"}`).

1. **Find the tool:** `search_tools` with words ("buffer", "overlay", "count points", "density"), then
   `describe_tool` for its parameters. Never guess parameter names. The built-in tools are:
   - proximity: `buffer` (`input`, `distance`, `units`, `dissolve`), `near` (`input`, `near`,
     `distance`, `units`: adds the nearest feature's id, its distance and the nearest point)
   - overlay: `clip` (`input`, `clip`), `erase` (`input`, `erase`), `intersect` (`input`,
     `overlay`: the parts in each overlay polygon, with both layers' attributes), `identity`
     (the same, plus the parts outside every overlay polygon), `union` (both polygon layers:
     every piece either covers), `symmetricalDifference` (both polygon layers: the pieces only
     one covers), `update` (`input`, `update`: the input's polygons with the update polygons
     laid over them), `split` (`input`, `split`, `field`: one clipped layer per split polygon
     or field value)
   - generalization: `dissolve` (`input`, `field`, `statistics`: summary fields over each
     group's records, as in `summaryStatistics`), `simplify` (`input`, `tolerance`)
   - geometry: `convexHull`, `featureToPoint`, `featureVerticesToPoints`, `pointsAlongLines`
     (`input`, `distance`, `units`, `startOffset`, `endOffset`, `includeEnd`), `xyTableToPoint`
     (`input`, `x`, `y`, `crs`), `calculateGeometry`; `polygonToLine`, `splitLineAtVertices`,
     `featureEnvelopeToPolygon`, `unsplitLine` (`input`, optional `output`; see the user guide
     for envelope axes and endpoint-joining rules); `checkGeometry` (`input`: a layer of the
     features with invalid geometry, saying what is wrong with each); `repairGeometry` (`input`:
     every feature with its geometry made valid where it can be, and a field saying what was
     repaired; features with a missing or non-finite coordinate, even in a hole, or left with no
     usable points or, for polygons, no area once repaired are written without geometry; valid
     points and lines are kept; other features too complex to check, or whose repair fails,
     are written unchanged)
   - selection: `selectByLocation` (`input`, `select`, `distance`, `invert`), `selectByAttributes`
     (`input`, `where`)
   - statistics: `countPoints` (Summarize Within: `polygons`, `points`, `statistics`: summary
     fields of the points in each polygon, as in `summaryStatistics`), `summaryStatistics`
     (`input`, `statistics`, `case`), `pivotTable` (`input`, `fields`, `pivot`, `value`, `statistic`),
     `tabulateIntersection` (`zones`, `zoneFields`, `classes`, `classFields`, `sumFields`: class
     areas and totals shared out to zones by overlap)
   - joins: `spatialJoin` (`target`, `join`, `match`), `joinField` (`target`, `targetField`,
     `joinTable`, `joinField`, `fields`, `prefix`, `duplicates`)
   - conversion: `exportFeatures` (`input`, `where`)
   - density: `kernelDensity` (`input`, `field`, `radius`, `pixelSize`, `kernel`, `crs`)
2. **Run it:** use `run_tool` with layers named in `params`. The output is usually one new layer,
   and the result names it: use that name as the next step's input. Give `output` a clear name
   when the chain is long. A tool with several outputs (`split`, one layer per split area) lists
   them all among the layers it added: pick the layer the next step needs, or run that step on
   each.
3. **The selection:** tools process only the selected features when a layer has a selection. Clear it
   with `select.clear` first unless the user meant the selection.
4. **Units:** `buffer` distances take `units`. Check `describe_tool` for the default and say which
   you used.
5. **Check:** read the output with `list_layers` (feature count) or `read_features` before going on,
   and report counts and the names of the layers made. Each run is one undo step.

For raster analysis (slope, NDVI, viewshed, suitability), the guided workflows are in
`user-guide/analysis.md` › Analysis workflows. The GeoLibre/Whitebox tools are in
`user-guide/analysis.md` › GeoLibre tools; find them with `search_tools` too.
