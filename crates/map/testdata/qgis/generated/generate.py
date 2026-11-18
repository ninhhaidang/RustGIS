"""Generates a corpus of QGIS project (.qgs) and style (.qml) files, each varying one thing, with
the QGIS API (run in the QGIS Flatpak). Output: OUT (argv[1]).

  data/      a GeoPackage (points, lines, polygons with a few fields) and a small GeoTIFF
  qml/       one style per symbol layer type, renderer, paint effect, label setting,
             data-defined property and raster renderer
  qgs/       projects: every provider, project structure (groups, themes, bookmarks, variables,
             relations, joins, virtual fields, time), a layout with every item, annotations
  types.txt  the element types this QGIS knows (the inventory to cover)
  log.txt    what was written, and what failed

The projects are then scrubbed of the user's name and absolute paths (scrub.py).

By Dominik Strebel (@pick2510), for the RustGIS project. Apache-2.0.
"""
import os
import sys
import traceback

from qgis.core import (
    Qgis, QgsApplication, QgsProject, QgsVectorLayer, QgsRasterLayer, QgsField, QgsFeature,
    QgsGeometry, QgsPointXY, QgsVectorFileWriter, QgsCoordinateReferenceSystem, QgsSymbol,
    QgsSingleSymbolRenderer, QgsCategorizedSymbolRenderer, QgsRendererCategory,
    QgsGraduatedSymbolRenderer, QgsRendererRange, QgsRuleBasedRenderer, QgsPalLayerSettings,
    QgsVectorLayerSimpleLabeling, QgsRuleBasedLabeling, QgsTextFormat, QgsTextBufferSettings,
    QgsTextBackgroundSettings, QgsTextShadowSettings, QgsProperty, QgsSymbolLayer,
    QgsReadWriteContext, QgsMapLayerStyle, QgsLayerTreeGroup, QgsMapThemeCollection,
    QgsBookmark, QgsReferencedRectangle, QgsRectangle, QgsExpressionContextUtils, QgsRelation,
    QgsVectorLayerJoinInfo, QgsPrintLayout, QgsLayoutItemMap, QgsLayoutItemLabel,
    QgsLayoutItemLegend, QgsLayoutItemScaleBar, QgsLayoutItemPicture, QgsLayoutItemShape,
    QgsLayoutItemPolygon, QgsLayoutItemPolyline, QgsLayoutItemAttributeTable, QgsLayoutFrame,
    QgsLayoutItemHtml, QgsLayoutItemMarker, QgsLayoutPoint, QgsLayoutSize, QgsUnitTypes,
    QgsAnnotationLayer, QgsAnnotationPointTextItem, QgsAnnotationMarkerItem,
    QgsAnnotationLineItem, QgsAnnotationPolygonItem, QgsAnnotationPictureItem,
    QgsAnnotationRectangleTextItem, QgsAnnotationLineTextItem, QgsPoint, QgsLineString,
    QgsPolygon, QgsSingleBandPseudoColorRenderer, QgsRasterShader, QgsColorRampShader,
    QgsSingleBandGrayRenderer, QgsMultiBandColorRenderer, QgsPalettedRasterRenderer,
    QgsHillshadeRenderer, QgsRasterContourRenderer, QgsHeatmapRenderer,
    QgsInvertedPolygonRenderer, Qgs25DRenderer, QgsMergedFeatureRenderer,
    QgsPointClusterRenderer, QgsPointDisplacementRenderer, QgsNullSymbolRenderer,
    QgsEmbeddedSymbolRenderer, QgsEffectStack, QgsVectorLayerTemporalProperties,
    QgsDateTimeRange, QgsStyle, QgsGradientColorRamp, QgsLayerTreeLayer,
    QgsDiagramSettings, QgsDiagramLayerSettings, QgsSingleCategoryDiagramRenderer,
    QgsLinearlyInterpolatedDiagramRenderer, QgsPieDiagram, QgsHistogramDiagram,
    QgsStackedBarDiagram, QgsTextDiagram, QgsGroupLayer, QgsPolymorphicRelation,
)
from qgis.PyQt.QtCore import QVariant, QMetaType, QDateTime, QDate, QTime, QSizeF
from qgis.PyQt.QtGui import QColor, QPainter

OUT = os.path.abspath(sys.argv[1])
LOG = []


def log(msg):
    LOG.append(msg)
    print(msg, flush=True)


def attempt(name, fn):
    try:
        fn()
        log(f"ok      {name}")
    except Exception as e:  # noqa: BLE001 - the corpus keeps going; the log says what failed
        log(f"FAILED  {name}: {type(e).__name__}: {e}")
        LOG.append(traceback.format_exc())


QgsApplication.setPrefixPath("/app", True)
app = QgsApplication([], False)
app.initQgis()
for d in ("data", "qml", "qgs"):
    os.makedirs(os.path.join(OUT, d), exist_ok=True)
CRS = QgsCoordinateReferenceSystem("EPSG:2056")
GPKG = os.path.join(OUT, "data", "sample.gpkg")
TIF = os.path.join(OUT, "data", "dem.tif")


# ---------------------------------------------------------------------------------------------
# Data

def field(name, t):
    return QgsField(name, t)


def make_data():
    if os.path.exists(GPKG):
        os.remove(GPKG)
    specs = {
        "points": ("Point", [(2696500 + i * 150, 1261500 + (i % 3) * 120) for i in range(9)]),
        "lines": ("LineString", None),
        "polygons": ("Polygon", None),
    }
    first = True
    for name, (geom, coords) in specs.items():
        vl = QgsVectorLayer(f"{geom}?crs=EPSG:2056", name, "memory")
        pr = vl.dataProvider()
        pr.addAttributes([
            field("id", QMetaType.Type.Int), field("name", QMetaType.Type.QString),
            field("kind", QMetaType.Type.QString), field("value", QMetaType.Type.Double),
            field("angle", QMetaType.Type.Double), field("day", QMetaType.Type.QDate),
        ])
        vl.updateFields()
        feats = []
        for i in range(9):
            f = QgsFeature(vl.fields())
            x, y = 2696500 + i * 150, 1261500 + (i % 3) * 120
            if geom == "Point":
                g = QgsGeometry.fromPointXY(QgsPointXY(x, y))
            elif geom == "LineString":
                g = QgsGeometry.fromPolylineXY([QgsPointXY(x, y), QgsPointXY(x + 100, y + 80), QgsPointXY(x + 220, y + 40)])
            else:
                g = QgsGeometry.fromPolygonXY([[QgsPointXY(x, y), QgsPointXY(x + 120, y), QgsPointXY(x + 120, y + 90), QgsPointXY(x, y + 90), QgsPointXY(x, y)]])
            f.setGeometry(g)
            f.setAttributes([i, f"Feature {i}", ["a", "b", "c"][i % 3], float(i * 10), float(i * 40), QDate(2024, 1 + i, 1)])
            feats.append(f)
        pr.addFeatures(feats)
        opts = QgsVectorFileWriter.SaveVectorOptions()
        opts.driverName = "GPKG"
        opts.layerName = name
        opts.actionOnExistingFile = QgsVectorFileWriter.ActionOnExistingFile.CreateOrOverwriteFile if first else QgsVectorFileWriter.ActionOnExistingFile.CreateOrOverwriteLayer
        err = QgsVectorFileWriter.writeAsVectorFormatV3(vl, GPKG, QgsProject.instance().transformContext(), opts)
        if err[0] != QgsVectorFileWriter.WriterError.NoError:
            raise RuntimeError(str(err))
        first = False
    from osgeo import gdal, osr
    ds = gdal.GetDriverByName("GTiff").Create(TIF, 40, 30, 3, gdal.GDT_Float32)
    ds.SetGeoTransform((2696400, 25, 0, 1262350, 0, -25))
    srs = osr.SpatialReference()
    srs.ImportFromEPSG(2056)
    ds.SetProjection(srs.ExportToWkt())
    for b in (1, 2, 3):
        band = ds.GetRasterBand(b)
        band.WriteArray(__import__("numpy").fromfunction(lambda r, c: 400 + 3 * r + 2 * c + 10 * b, (30, 40)))
    ds = None


attempt("data", make_data)


def layer(name):
    vl = QgsVectorLayer(f"{GPKG}|layername={name}", name, "ogr")
    if not vl.isValid():
        raise RuntimeError(f"{name} not valid")
    return vl


def raster():
    rl = QgsRasterLayer(TIF, "dem", "gdal")
    if not rl.isValid():
        raise RuntimeError("raster not valid")
    return rl


def save_qml(vl, name):
    path = os.path.join(OUT, "qml", f"{name}.qml")
    msg, ok = vl.saveNamedStyle(path)
    if not ok:
        raise RuntimeError(msg)


# ---------------------------------------------------------------------------------------------
# Symbol layers: one style per type, its defaults, on the geometry it draws

GEOM_OF = {Qgis.SymbolType.Marker: "points", Qgis.SymbolType.Line: "lines", Qgis.SymbolType.Fill: "polygons"}
reg = QgsApplication.symbolLayerRegistry()
for stype, lname in GEOM_OF.items():
    for t in sorted(reg.symbolLayersForType(stype)):
        def one(stype=stype, lname=lname, t=t):
            vl = layer(lname)
            sl = reg.symbolLayerMetadata(t).createSymbolLayer({})
            if sl is None:
                raise RuntimeError("no symbol layer")
            sym = QgsSymbol.defaultSymbol(vl.geometryType())
            sym.changeSymbolLayer(0, sl)
            vl.setRenderer(QgsSingleSymbolRenderer(sym))
            save_qml(vl, f"symbol-{stype.name.lower()}-{t}")
        attempt(f"symbol {stype.name} {t}", one)


# Symbol-level details on a simple symbol: dashes, units, offsets, several layers, levels.
def symbol_details():
    vl = layer("lines")
    sym = QgsSymbol.defaultSymbol(vl.geometryType())
    sl = sym.symbolLayer(0)
    sl.setUseCustomDashPattern(True)
    sl.setCustomDashVector([3.0, 1.5, 0.5, 1.5])
    sl.setWidthUnit(Qgis.RenderUnit.MapUnits)
    sl.setOffset(0.4)
    second = sl.clone()
    second.setUseCustomDashPattern(False)
    second.setColor(QColor("#ffffff"))
    second.setWidth(0.2)
    second.setRenderingPass(1)
    sym.appendSymbolLayer(second)
    sym.setOpacity(0.6)
    vl.setRenderer(QgsSingleSymbolRenderer(sym))
    vl.renderer().setUsingSymbolLevels(True)
    save_qml(vl, "symbol-details-dash-units-offset-levels")


attempt("symbol details", symbol_details)


# Paint effects: each on a simple fill.
for eff in sorted(QgsApplication.paintEffectRegistry().effects()):
    def one(eff=eff):
        vl = layer("polygons")
        sym = QgsSymbol.defaultSymbol(vl.geometryType())
        stack = QgsEffectStack()
        e = QgsApplication.paintEffectRegistry().createEffect(eff)
        stack.appendEffect(e)
        stack.setEnabled(True)
        sym.symbolLayer(0).setPaintEffect(stack)
        vl.setRenderer(QgsSingleSymbolRenderer(sym))
        save_qml(vl, f"effect-{eff}")
    attempt(f"effect {eff}", one)


# ---------------------------------------------------------------------------------------------
# Renderers

def renderers():
    out = {}
    pts, polys = layer("points"), layer("polygons")
    ms = QgsSymbol.defaultSymbol(pts.geometryType())
    fs = QgsSymbol.defaultSymbol(polys.geometryType())
    out["singleSymbol"] = ("points", QgsSingleSymbolRenderer(ms.clone()))
    cats = [QgsRendererCategory(v, ms.clone(), v) for v in ("a", "b")] + [QgsRendererCategory(["c", "d"], ms.clone(), "c or d"), QgsRendererCategory("", ms.clone(), "other")]
    out["categorizedSymbol"] = ("points", QgsCategorizedSymbolRenderer("kind", cats))
    out["categorizedSymbol-expression"] = ("points", QgsCategorizedSymbolRenderer("upper(\"kind\")", [QgsRendererCategory("A", ms.clone(), "A")]))
    ranges = [QgsRendererRange(0, 30, ms.clone(), "0-30"), QgsRendererRange(30, 80, ms.clone(), "30-80")]
    g = QgsGraduatedSymbolRenderer("value", ranges)
    out["graduatedSymbol"] = ("points", g)
    g2 = QgsGraduatedSymbolRenderer("value * 2", [QgsRendererRange(0, 100, ms.clone(), "x")])
    g2.setGraduatedMethod(Qgis.GraduatedMethod.Size)
    out["graduatedSymbol-size-expression"] = ("points", g2)
    root = QgsRuleBasedRenderer.Rule(None)
    for label, flt, mn, mx in [("a", "\"kind\" = 'a'", 0, 0), ("big", "\"value\" > 40 AND \"kind\" IN ('b','c')", 1000, 50000), ("fn", "length(\"name\") > 3", 0, 0)]:
        r = QgsRuleBasedRenderer.Rule(ms.clone(), mx, mn, flt, label)
        root.appendChild(r)
    nested = QgsRuleBasedRenderer.Rule(None, 0, 0, "\"value\" < 50", "nested")
    nested.appendChild(QgsRuleBasedRenderer.Rule(ms.clone(), 0, 0, "\"kind\" = 'c'", "inner"))
    root.appendChild(nested)
    else_rule = QgsRuleBasedRenderer.Rule(ms.clone(), 0, 0, "ELSE", "else")
    root.appendChild(else_rule)
    out["RuleRenderer"] = ("points", QgsRuleBasedRenderer(root))
    out["nullSymbol"] = ("points", QgsNullSymbolRenderer())
    out["heatmapRenderer"] = ("points", QgsHeatmapRenderer())
    out["pointCluster"] = ("points", QgsPointClusterRenderer())
    out["pointDisplacement"] = ("points", QgsPointDisplacementRenderer())
    out["invertedPolygonRenderer"] = ("polygons", QgsInvertedPolygonRenderer(QgsSingleSymbolRenderer(fs.clone())))
    out["mergedFeatureRenderer"] = ("polygons", QgsMergedFeatureRenderer(QgsSingleSymbolRenderer(fs.clone())))
    out["25dRenderer"] = ("polygons", Qgs25DRenderer())
    out["embeddedSymbol"] = ("points", QgsEmbeddedSymbolRenderer(ms.clone()))
    return out


def write_renderers():
    for name, (lname, r) in renderers().items():
        def one(name=name, lname=lname, r=r):
            vl = layer(lname)
            vl.setRenderer(r)
            save_qml(vl, f"renderer-{name}")
        attempt(f"renderer {name}", one)


attempt("renderers", write_renderers)


# ---------------------------------------------------------------------------------------------
# Data-defined properties

def data_defined():
    vl = layer("points")
    sym = QgsSymbol.defaultSymbol(vl.geometryType())
    sl = sym.symbolLayer(0)
    sl.setDataDefinedProperty(QgsSymbolLayer.Property.Size, QgsProperty.fromField("value"))
    sl.setDataDefinedProperty(QgsSymbolLayer.Property.Angle, QgsProperty.fromField("angle"))
    sl.setDataDefinedProperty(QgsSymbolLayer.Property.FillColor, QgsProperty.fromExpression("CASE WHEN \"kind\" = 'a' THEN '#ff0000' ELSE '#0000ff' END"))
    vl.setRenderer(QgsSingleSymbolRenderer(sym))
    save_qml(vl, "datadefined-size-angle-color")


attempt("data-defined", data_defined)


# ---------------------------------------------------------------------------------------------
# Diagrams: pie by fields and by expressions, a histogram, a stacked bar and a text diagram; a pie
# and a histogram sized between two values (by an expression, and by a field).

def diagrams():
    def settings(attrs):
        s = QgsDiagramSettings()
        s.enabled = True
        s.categoryAttributes = attrs
        s.categoryColors = [QColor(c) for c in ("#e41a1c", "#377eb8", "#4daf4a", "#984ea3")[:len(attrs)]]
        s.categoryLabels = [a.strip('"') for a in attrs]
        s.size = QSizeF(12, 12)
        s.sizeType = Qgis.RenderUnit.Millimeters
        s.backgroundColor = QColor(255, 255, 255, 255)
        s.penColor = QColor("#333333")
        s.penWidth = 0.3
        s.barWidth = 3
        return s

    for name, kind, attrs, scaled in [
        ("pie", QgsPieDiagram, ['"value"', '"angle"'], False),
        ("pie-expression", QgsPieDiagram, ['"value"', '"angle" / 2', '"id"'], False),
        ("pie-scaled", QgsPieDiagram, ['"value"', '"angle"'], True),
        ("histogram-scaled", QgsHistogramDiagram, ['"value"', '"angle"'], True),
        ("histogram", QgsHistogramDiagram, ['"value"', '"angle"', '"id"'], False),
        ("stackedbar", QgsStackedBarDiagram, ['"value"', '"angle"'], False),
        ("text", QgsTextDiagram, ['"value"', '"angle"'], False),
    ]:
        vl = layer("polygons")
        s = settings(attrs)
        if scaled:
            r = QgsLinearlyInterpolatedDiagramRenderer()
            r.setLowerValue(0)
            r.setUpperValue(400)
            r.setLowerSize(QSizeF(0, 0))
            r.setUpperSize(QSizeF(20, 20))
            if name == "pie-scaled":
                r.setClassificationAttributeIsExpression(True)
                r.setClassificationAttributeExpression('"value" + "angle"')
            else:
                r.setClassificationField("value")
            s.scaleByArea = True
        else:
            r = QgsSingleCategoryDiagramRenderer()
        r.setDiagram(kind())
        r.setDiagramSettings(s)
        vl.setDiagramRenderer(r)
        vl.setDiagramLayerSettings(QgsDiagramLayerSettings())
        save_qml(vl, f"diagram-{name}")


attempt("diagrams", diagrams)


# ---------------------------------------------------------------------------------------------
# Labels

def label_settings(**kw):
    s = QgsPalLayerSettings()
    s.fieldName = "name"
    s.enabled = True
    fmt = QgsTextFormat()
    fmt.setSize(9)
    s.setFormat(fmt)
    for k, v in kw.items():
        setattr(s, k, v)
    return s


def save_labels(lname, name, s):
    vl = layer(lname)
    vl.setLabeling(QgsVectorLayerSimpleLabeling(s))
    vl.setLabelsEnabled(True)
    save_qml(vl, f"label-{name}")


def labels():
    attempt("label plain", lambda: save_labels("points", "plain", label_settings()))
    for p in Qgis.LabelPlacement:
        lname = {"Line": "lines", "Curved": "lines", "PerimeterCurved": "polygons", "Horizontal": "polygons", "Free": "polygons", "OutsidePolygons": "polygons"}.get(p.name, "points")
        attempt(f"label placement {p.name}", lambda p=p, lname=lname: save_labels(lname, f"placement-{p.name}", label_settings(placement=p)))

    def fmt_variant(name, edit, lname="points"):
        s = label_settings()
        f = s.format()
        edit(s, f)
        s.setFormat(f)
        attempt(f"label {name}", lambda: save_labels(lname, name, s))

    def buffer(s, f):
        b = QgsTextBufferSettings()
        b.setEnabled(True)
        b.setSize(1.5)
        b.setColor(QColor("#ffee00"))
        f.setBuffer(b)
    fmt_variant("buffer", buffer)

    def background(s, f):
        b = QgsTextBackgroundSettings()
        b.setEnabled(True)
        b.setFillColor(QColor("#ddeeff"))
        f.setBackground(b)
    fmt_variant("background", background)

    def shadow(s, f):
        sh = QgsTextShadowSettings()
        sh.setEnabled(True)
        f.setShadow(sh)
    fmt_variant("shadow", shadow)

    def font(s, f):
        from qgis.PyQt.QtGui import QFont
        q = QFont("Liberation Sans Narrow")
        q.setItalic(True)
        q.setBold(True)
        q.setLetterSpacing(QFont.SpacingType.AbsoluteSpacing, 1.5)
        q.setWordSpacing(3)
        f.setFont(q)
        f.setNamedStyle("Bold Italic")
        f.setSizeUnit(Qgis.RenderUnit.MapUnits)
        f.setSize(20)
        f.setColor(QColor("#336699"))
        f.setOpacity(0.8)
    fmt_variant("font-spacing-units", font)

    def wrap(s, f):
        s.wrapChar = " "
        s.autoWrapLength = 10
        s.multilineAlign = Qgis.LabelMultiLineAlignment.Center
    fmt_variant("wrap", wrap)

    def expression(s, f):
        s.fieldName = "\"name\" || ' (' || to_string(\"value\") || ')'"
        s.isExpression = True
    fmt_variant("expression-text", expression)

    def offsets(s, f):
        s.placement = Qgis.LabelPlacement.OverPoint
        s.quadOffset = Qgis.LabelQuadrantPosition.AboveRight
        s.xOffset = 2
        s.yOffset = -1
        s.dist = 3
    fmt_variant("offset-quadrant", offsets)

    def dd(s, f):
        p = s.dataDefinedProperties()
        p.setProperty(QgsPalLayerSettings.Property.LabelRotation, QgsProperty.fromField("angle"))
        p.setProperty(QgsPalLayerSettings.Property.Size, QgsProperty.fromExpression("\"value\" / 5 + 6"))
        p.setProperty(QgsPalLayerSettings.Property.Color, QgsProperty.fromField("kind"))
        s.setDataDefinedProperties(p)
    fmt_variant("data-defined", dd)

    def scale(s, f):
        s.scaleVisibility = True
        s.minimumScale = 50000
        s.maximumScale = 1000
        s.priority = 8
        s.obstacle = True
    fmt_variant("scale-priority-obstacle", scale)

    def repeat(s, f):
        s.placement = Qgis.LabelPlacement.Curved
        s.repeatDistance = 30
        s.maxCurvedCharAngleIn = 20
        s.maxCurvedCharAngleOut = -20
        s.lineSettings().setOverrunDistance(5)
    fmt_variant("line-repeat-overrun-angles", repeat, "lines")

    for c in QgsApplication.calloutRegistry().calloutTypes():
        def callout(s, f, c=c):
            co = QgsApplication.calloutRegistry().createCallout(c)
            co.setEnabled(True)
            s.setCallout(co)
        fmt_variant(f"callout-{c}", callout)

    def rules():
        vl = layer("points")
        root = QgsRuleBasedLabeling.Rule(None)
        r1 = QgsRuleBasedLabeling.Rule(label_settings(), 0, 0, "\"kind\" = 'a'", "a")
        r2 = QgsRuleBasedLabeling.Rule(label_settings(), 50000, 1000, "\"value\" > 20", "big")
        r3 = QgsRuleBasedLabeling.Rule(label_settings(), 0, 0, "ELSE", "else")
        group = QgsRuleBasedLabeling.Rule(None, 0, 0, "\"value\" < 70", "group")
        group.appendChild(QgsRuleBasedLabeling.Rule(label_settings(), 0, 0, "\"kind\" = 'b'", "inner"))
        for r in (r1, r2, r3, group):
            root.appendChild(r)
        vl.setLabeling(QgsRuleBasedLabeling(root))
        vl.setLabelsEnabled(True)
        save_qml(vl, "label-rule-based")
    attempt("label rule-based", rules)


attempt("labels", labels)


# Layer settings in a style: opacity, blending, scale range, reference scale, subset.
def layer_settings():
    vl = layer("polygons")
    vl.setOpacity(0.5)
    from qgis.PyQt.QtGui import QPainter
    vl.setBlendMode(QPainter.CompositionMode.CompositionMode_Multiply)
    vl.setScaleBasedVisibility(True)
    vl.setMinimumScale(100000)
    vl.setMaximumScale(500)
    vl.renderer().setReferenceScale(25000)
    vl.setSubsetString("\"value\" > 10")
    save_qml(vl, "layer-opacity-blend-scale-reference-subset")


attempt("layer settings", layer_settings)


# ---------------------------------------------------------------------------------------------
# Raster renderers

def raster_styles():
    def save(rl, name):
        msg, ok = rl.saveNamedStyle(os.path.join(OUT, "qml", f"raster-{name}.qml"))
        if not ok:
            raise RuntimeError(msg)

    def gray():
        rl = raster()
        rl.setRenderer(QgsSingleBandGrayRenderer(rl.dataProvider(), 1))
        save(rl, "singlebandgray")
    attempt("raster gray", gray)
    for kind in ("Interpolated", "Discrete", "Exact"):
        def pseudo(kind=kind):
            rl = raster()
            sh = QgsRasterShader()
            crs = QgsColorRampShader(400, 600, QgsGradientColorRamp(QColor("#2b83ba"), QColor("#d7191c")), getattr(QgsColorRampShader.Type, kind))
            crs.setColorRampItemList([QgsColorRampShader.ColorRampItem(400, QColor("#2b83ba"), "low"), QgsColorRampShader.ColorRampItem(500, QColor("#ffffbf"), "mid"), QgsColorRampShader.ColorRampItem(600, QColor("#d7191c"), "high")])
            sh.setRasterShaderFunction(crs)
            rl.setRenderer(QgsSingleBandPseudoColorRenderer(rl.dataProvider(), 1, sh))
            save(rl, f"singlebandpseudocolor-{kind.lower()}")
        attempt(f"raster pseudocolor {kind}", pseudo)

    def multi():
        rl = raster()
        rl.setRenderer(QgsMultiBandColorRenderer(rl.dataProvider(), 1, 2, 3))
        rl.brightnessFilter().setBrightness(20)
        rl.brightnessFilter().setContrast(10)
        rl.hueSaturationFilter().setSaturation(-30)
        rl.resampleFilter().setZoomedInResampler(None)
        save(rl, "multibandcolor-brightness-saturation")
    attempt("raster multiband", multi)

    def paletted():
        rl = raster()
        classes = [QgsPalettedRasterRenderer.Class(400, QColor("#ff0000"), "a"), QgsPalettedRasterRenderer.Class(450, QColor("#00ff00"), "b")]
        rl.setRenderer(QgsPalettedRasterRenderer(rl.dataProvider(), 1, classes))
        save(rl, "paletted")
    attempt("raster paletted", paletted)

    def hill():
        rl = raster()
        rl.setRenderer(QgsHillshadeRenderer(rl.dataProvider(), 1, 315, 45))
        save(rl, "hillshade")
    attempt("raster hillshade", hill)

    def contour():
        rl = raster()
        rl.setRenderer(QgsRasterContourRenderer(rl.dataProvider()))
        save(rl, "contour")
    attempt("raster contour", contour)

    def single_color():
        rl = raster()
        rl.setRenderer(__import__("qgis.core", fromlist=["x"]).QgsSingleColorRasterRenderer(rl.dataProvider(), 1, QColor("#884400")) if hasattr(__import__("qgis.core", fromlist=["x"]), "QgsSingleColorRasterRenderer") else None)
        save(rl, "singlecolor")
    attempt("raster single color", single_color)

    def opacity():
        rl = raster()
        rl.renderer().setOpacity(0.4)
        rl.setBlendMode(__import__("qgis.PyQt.QtGui", fromlist=["QPainter"]).QPainter.CompositionMode.CompositionMode_Multiply)
        save(rl, "opacity-blend")
    attempt("raster opacity", opacity)


attempt("raster styles", raster_styles)


# ---------------------------------------------------------------------------------------------
# Projects

def new_project():
    p = QgsProject.instance()
    p.clear()
    p.setCrs(CRS)
    p.setTitle("corpus")
    return p


def save_project(p, name):
    path = os.path.join(OUT, "qgs", f"{name}.qgs")
    if not p.write(path):
        raise RuntimeError(p.error())


# Each provider: one layer (offline ones valid, services and databases written as configured).
PROVIDERS = [
    ("ogr-gpkg", lambda: QgsVectorLayer(f"{GPKG}|layername=points", "ogr gpkg", "ogr")),
    ("gdal-geotiff", lambda: QgsRasterLayer(TIF, "gdal tif", "gdal")),
    ("memory", lambda: memory_layer()),
    ("delimitedtext-xy", lambda: QgsVectorLayer(f"file://{csv('xy')}?type=csv&xField=x&yField=y&crs=EPSG:2056", "csv xy", "delimitedtext")),
    ("delimitedtext-wkt", lambda: QgsVectorLayer(f"file://{csv('wkt')}?type=csv&wktField=wkt&crs=EPSG:2056", "csv wkt", "delimitedtext")),
    ("spatialite", lambda: spatialite_layer()),
    ("virtual", lambda: QgsVectorLayer(f"?layer=ogr:{GPKG}|layername=points:pts&query=SELECT * FROM pts WHERE value > 20", "virtual", "virtual")),
    ("wms", lambda: QgsRasterLayer("contextualWMSLegend=0&crs=EPSG:2056&dpiMode=7&format=image/png&layers=ch.swisstopo.pixelkarte-farbe&styles&url=https://wms.geo.admin.ch/", "wms", "wms")),
    ("xyz", lambda: QgsRasterLayer("type=xyz&url=https://tile.openstreetmap.org/%7Bz%7D/%7Bx%7D/%7By%7D.png&zmax=19&zmin=0", "xyz", "wms")),
    ("wfs", lambda: QgsVectorLayer("pagingEnabled='true' srsname='EPSG:2056' typename='ms:roads' url='https://example.invalid/wfs' version='2.0.0'", "wfs", "WFS")),
    ("wcs", lambda: QgsRasterLayer("cache=PreferNetwork&crs=EPSG:2056&format=GTiff&identifier=dem&url=https://example.invalid/wcs", "wcs", "wcs")),
    ("arcgisfeatureserver", lambda: QgsVectorLayer("crs='EPSG:2056' url='https://example.invalid/arcgis/rest/services/Roads/FeatureServer/0'", "afs", "arcgisfeatureserver")),
    ("arcgismapserver", lambda: QgsRasterLayer("crs='EPSG:2056' format='PNG32' layer='0' url='https://example.invalid/arcgis/rest/services/Topo/MapServer'", "ams", "arcgismapserver")),
    ("vectortile", lambda: __import__("qgis.core", fromlist=["QgsVectorTileLayer"]).QgsVectorTileLayer("type=xyz&url=https://example.invalid/tiles/%7Bz%7D/%7Bx%7D/%7By%7D.pbf&zmax=14&zmin=0", "vector tiles")),
    ("postgres", lambda: QgsVectorLayer("dbname='gis' host=db.invalid port=5432 user='me' password='secret' sslmode=disable key='id' srid=2056 type=Point table=\"public\".\"towns\" (geom)", "postgis", "postgres")),
    ("mssql", lambda: QgsVectorLayer("dbname='gis' host=db.invalid user='me' password='secret' srid=2056 type=Point table=\"dbo\".\"towns\" (geom)", "mssql", "mssql")),
    ("gpx", lambda: QgsVectorLayer(f"{gpx()}?type=waypoint", "gpx", "gpx")),
    # Vector tiles from an MBTiles file, a PMTiles archive and with a MapLibre style; raster tiles
    # from an MBTiles file (the files needn't be there: QGIS writes the layers as configured).
    ("mbtilesvectortiles", lambda: vector_tiles(f"type=mbtiles&url={os.path.join(OUT, 'data', 'tiles.mbtiles')}", "mbtiles")),
    ("pmtiles", lambda: vector_tiles(f"type=xyz&url=file://{os.path.join(OUT, 'data', 'tiles.pmtiles')}", "pmtiles")),
    ("vectortile-style", lambda: vector_tiles("styleUrl=https://example.invalid/style.json&type=xyz&url=https://example.invalid/tiles/%7Bz%7D/%7Bx%7D/%7By%7D.pbf&zmax=14&zmin=0", "styled")),
    ("mbtiles-raster", lambda: QgsRasterLayer(f"type=mbtiles&url={os.path.join(OUT, 'data', 'raster.mbtiles')}", "mbtiles raster", "wms")),
    ("gpx-track", lambda: QgsVectorLayer(f"{gpx()}?type=track", "gpx track", "gpx")),
    # A virtual layer on another of the project's layers, and one RustGIS doesn't read as a layer.
    ("virtual-ref", lambda: virtual_ref()),
    ("virtual-grouped", lambda: QgsVectorLayer(f"?layer=ogr:{GPKG}|layername=points:pts&query=SELECT kind, count(*) AS n FROM pts GROUP BY kind", "virtual grouped", "virtual")),
]


def vector_tiles(uri, name):
    from qgis.core import QgsVectorTileLayer
    return QgsVectorTileLayer(uri, name)


def virtual_ref():
    base = QgsVectorLayer(f"{GPKG}|layername=points", "points", "ogr")
    QgsProject.instance().addMapLayer(base)
    return [base, QgsVectorLayer(f"?layer_ref={base.id()}:pts&query=SELECT * FROM pts WHERE value > 20", "virtual ref", "virtual")]


def memory_layer():
    vl = QgsVectorLayer("Point?crs=EPSG:2056&field=name:string(20)&field=n:integer", "memory", "memory")
    f = QgsFeature(vl.fields())
    f.setGeometry(QgsGeometry.fromPointXY(QgsPointXY(2697000, 1261800)))
    f.setAttributes(["one", 1])
    vl.dataProvider().addFeatures([f])
    return vl


def csv(kind):
    path = os.path.join(OUT, "data", f"points-{kind}.csv")
    with open(path, "w") as fh:
        if kind == "xy":
            fh.write("name,x,y\nA,2697000,1261800\nB,2697100,1261900\n")
        else:
            fh.write("name,wkt\nA,POINT(2697000 1261800)\nB,\"LINESTRING(2697000 1261800, 2697100 1261900)\"\n")
    return path


def gpx():
    path = os.path.join(OUT, "data", "waypoints.gpx")
    with open(path, "w") as fh:
        fh.write('<?xml version="1.0"?><gpx version="1.1" creator="corpus" xmlns="http://www.topografix.com/GPX/1/1"><wpt lat="47.5" lon="8.73"><name>A</name></wpt></gpx>\n')
    return path


def spatialite_layer():
    path = os.path.join(OUT, "data", "sample.sqlite")
    src = layer("points")
    opts = QgsVectorFileWriter.SaveVectorOptions()
    opts.driverName = "SQLite"
    opts.datasourceOptions = ["SPATIALITE=YES"]
    opts.layerName = "points"
    QgsVectorFileWriter.writeAsVectorFormatV3(src, path, QgsProject.instance().transformContext(), opts)
    return QgsVectorLayer(f"dbname='{path}' table=\"points\" (GEOMETRY)", "spatialite", "spatialite")


def providers():
    for name, make in PROVIDERS:
        def one(name=name, make=make):
            p = new_project()
            made = make()
            for lyr in made if isinstance(made, list) else [made]:
                p.addMapLayer(lyr)
                if not lyr.isValid():
                    log(f"        (provider {name}: layer {lyr.name()} written, not valid here)")
            save_project(p, f"provider-{name}")
        attempt(f"provider {name}", one)


attempt("providers", providers)


# Project structure: groups, a mutually exclusive group, themes, bookmarks, variables, relations,
# joins, a virtual field, time settings, a raster, a layer's style with several styles.
def structure():
    p = new_project()
    pts, lines, polys, dem = layer("points"), layer("lines"), layer("polygons"), raster()
    for lyr in (pts, lines, polys, dem):
        p.addMapLayer(lyr, False)
    root = p.layerTreeRoot()
    g = root.addGroup("Group")
    g.addLayer(pts)
    sub = g.addGroup("Exclusive")
    sub.setIsMutuallyExclusive(True)
    sub.addLayer(lines)
    sub.addLayer(polys)
    root.addLayer(dem).setItemVisibilityChecked(False)
    g.setCustomProperty("note", "a group property")
    themes = p.mapThemeCollection()
    themes.insert("Only points", QgsMapThemeCollection.createThemeFromCurrentState(root, __import__("qgis.core", fromlist=["QgsLayerTreeModel"]).QgsLayerTreeModel(root)))
    bm = QgsBookmark()
    bm.setId("bm")
    bm.setName("Old town")
    bm.setExtent(QgsReferencedRectangle(QgsRectangle(2696400, 1261250, 2698200, 1262350), CRS))
    p.bookmarkManager().addBookmark(bm)
    QgsExpressionContextUtils.setProjectVariable(p, "sheet", "1072")
    rel = QgsRelation()
    rel.setId("lines_points")
    rel.setName("lines to points")
    rel.setReferencingLayer(lines.id())
    rel.setReferencedLayer(pts.id())
    rel.addFieldPair("id", "id")
    p.relationManager().addRelation(rel)
    join = QgsVectorLayerJoinInfo()
    join.setJoinLayer(pts)
    join.setJoinFieldName("id")
    join.setTargetFieldName("id")
    join.setPrefix("pt_")
    polys.addJoin(join)
    polys.addExpressionField("\"value\" * 2", QgsField("double_value", QMetaType.Type.Double))
    tp = pts.temporalProperties()
    tp.setIsActive(True)
    tp.setMode(Qgis.VectorTemporalMode.FeatureDateTimeInstantFromField)
    tp.setStartField("day")
    pts.styleManager().addStyleFromLayer("second style")
    p.viewSettings().setDefaultViewExtent(QgsReferencedRectangle(QgsRectangle(2696400, 1261250, 2698200, 1262350), CRS))
    md = p.metadata()
    md.setAuthor("corpus")
    md.setAbstract("Project structure variants")
    p.setMetadata(md)
    save_project(p, "structure-groups-themes-bookmarks-variables-relations-joins-time")


attempt("structure", structure)


# A print layout with every item type, and an atlas.
def layouts():
    p = new_project()
    pts, polys = layer("points"), layer("polygons")
    p.addMapLayers([pts, polys])
    lay = QgsPrintLayout(p)
    lay.initializeDefaults()
    lay.setName("Every item")
    m = QgsLayoutItemMap(lay)
    m.attemptMove(QgsLayoutPoint(10, 10, Qgis.LayoutUnit.Millimeters))
    m.attemptResize(QgsLayoutSize(180, 120, Qgis.LayoutUnit.Millimeters))
    m.setExtent(QgsRectangle(2696400, 1261250, 2698200, 1262350))
    lay.addLayoutItem(m)
    lbl = QgsLayoutItemLabel(lay)
    lbl.setText("Title [% @project_title %]")
    lay.addLayoutItem(lbl)
    leg = QgsLayoutItemLegend(lay)
    leg.setLinkedMap(m)
    lay.addLayoutItem(leg)
    sb = QgsLayoutItemScaleBar(lay)
    sb.setLinkedMap(m)
    sb.applyDefaultSize()
    lay.addLayoutItem(sb)
    north = QgsLayoutItemPicture(lay)
    north.setLinkedMap(m)
    lay.addLayoutItem(north)
    shape = QgsLayoutItemShape(lay)
    shape.setShapeType(QgsLayoutItemShape.Shape.Ellipse)
    lay.addLayoutItem(shape)
    from qgis.PyQt.QtGui import QPolygonF
    from qgis.PyQt.QtCore import QPointF
    lay.addLayoutItem(QgsLayoutItemPolygon(QPolygonF([QPointF(0, 0), QPointF(10, 0), QPointF(5, 8)]), lay))
    lay.addLayoutItem(QgsLayoutItemPolyline(QPolygonF([QPointF(0, 0), QPointF(10, 5)]), lay))
    lay.addLayoutItem(QgsLayoutItemMarker(lay))
    table = QgsLayoutItemAttributeTable.create(lay)
    table.setVectorLayer(pts)
    lay.addMultiFrame(table)
    frame = QgsLayoutFrame(lay, table)
    frame.attemptResize(QgsLayoutSize(80, 40, Qgis.LayoutUnit.Millimeters))
    table.addFrame(frame)
    html = QgsLayoutItemHtml.create(lay)
    html.setContentMode(QgsLayoutItemHtml.ContentMode.ManualHtml)
    html.setHtml("<b>html</b>")
    lay.addMultiFrame(html)
    hframe = QgsLayoutFrame(lay, html)
    html.addFrame(hframe)
    atlas = lay.atlas()
    atlas.setCoverageLayer(polys)
    atlas.setEnabled(True)
    m.setAtlasDriven(True)
    p.layoutManager().addLayout(lay)
    save_project(p, "layout-every-item-atlas")


attempt("layouts", layouts)


# The annotation layer with each item type.
def annotations():
    p = new_project()
    al = QgsAnnotationLayer("annotations", QgsAnnotationLayer.LayerOptions(p.transformContext()))
    al.addItem(QgsAnnotationPointTextItem("point text", QgsPointXY(2697000, 1261800)))
    al.addItem(QgsAnnotationMarkerItem(QgsPoint(2697100, 1261800)))
    al.addItem(QgsAnnotationLineItem(QgsLineString([QgsPoint(2697000, 1261700), QgsPoint(2697200, 1261750)])))
    poly = QgsPolygon(QgsLineString([QgsPoint(2697000, 1261500), QgsPoint(2697100, 1261500), QgsPoint(2697100, 1261600), QgsPoint(2697000, 1261500)]))
    al.addItem(QgsAnnotationPolygonItem(poly))
    al.addItem(QgsAnnotationLineTextItem("along a line", QgsLineString([QgsPoint(2697000, 1261400), QgsPoint(2697300, 1261450)])))
    al.addItem(QgsAnnotationRectangleTextItem("rect text", QgsRectangle(2697300, 1261300, 2697500, 1261400)))
    al.addItem(QgsAnnotationPictureItem(Qgis.PictureFormat.SVG, "", QgsRectangle(2697500, 1261300, 2697600, 1261400)))
    p.addMapLayer(al)
    p.mainAnnotationLayer().addItem(QgsAnnotationPointTextItem("main layer text", QgsPointXY(2697050, 1261850)))
    save_project(p, "annotations-every-item")


attempt("annotations", annotations)


# Annotations with their settings: styled marks, lines and polygons (one with a hole), text
# aligned, turned, in millimetres, as HTML, with a buffer; rectangle text with a background and a
# frame, of a fixed size; a picture file and an SVG; text along a line; one switched off, one sized
# at a reference scale; drawn in an order of their own (z index). The layer has its own
# coordinate system (the project's is the same).
def annotations_styled():
    from qgis.core import QgsTextBufferSettings, QgsMarkerSymbol, QgsLineSymbol, QgsFillSymbol
    from qgis.PyQt.QtCore import Qt, QSizeF
    from qgis.PyQt.QtGui import QImage, QFont
    png = os.path.join(OUT, "data", "picture.png")
    img = QImage(40, 20, QImage.Format.Format_RGB32)
    img.fill(QColor(200, 30, 30))
    img.save(png)
    p = new_project()
    al = QgsAnnotationLayer("styled annotations", QgsAnnotationLayer.LayerOptions(p.transformContext()))
    al.setCrs(CRS)

    def fmt(size=12, unit=Qgis.RenderUnit.Points, color=QColor(0, 0, 0), bold=False, italic=False, buffer=False):
        f = QgsTextFormat()
        font = QFont("DejaVu Sans")
        font.setBold(bold)
        font.setItalic(italic)
        f.setFont(font)
        f.setSize(size)
        f.setSizeUnit(unit)
        f.setColor(color)
        if buffer:
            b = QgsTextBufferSettings()
            b.setEnabled(True)
            b.setSize(1)
            b.setColor(QColor(255, 255, 0))
            f.setBuffer(b)
        return f

    def add(item, z=0):
        item.setZIndex(z)
        al.addItem(item)
        return item

    t = add(QgsAnnotationPointTextItem("right, bold, red", QgsPointXY(2697000, 1261800)), 5)
    t.setFormat(fmt(14, color=QColor(200, 0, 0), bold=True, buffer=True))
    t.setAlignment(Qt.AlignmentFlag.AlignRight)
    t = add(QgsAnnotationPointTextItem("turned, italic", QgsPointXY(2697000, 1261750)))
    t.setFormat(fmt(4, Qgis.RenderUnit.Millimeters, italic=True))
    t.setAngle(30)
    t = add(QgsAnnotationPointTextItem("<b>html</b> text", QgsPointXY(2697000, 1261700)))
    f = fmt()
    f.setAllowHtmlFormatting(True)
    t.setFormat(f)
    t = add(QgsAnnotationPointTextItem("switched off", QgsPointXY(2697000, 1261650)))
    t.setEnabled(False)
    t = add(QgsAnnotationPointTextItem("at a reference scale", QgsPointXY(2697000, 1261600)))
    t.setUseSymbologyReferenceScale(True)
    t.setSymbologyReferenceScale(5000)
    m = add(QgsAnnotationMarkerItem(QgsPoint(2697100, 1261800)), 2)
    m.setSymbol(QgsMarkerSymbol.createSimple({"name": "square", "color": "255,0,0,255", "outline_color": "0,0,128,255", "outline_width": "0.5", "size": "4"}))
    m = add(QgsAnnotationMarkerItem(QgsPoint(2697150, 1261800)))
    m.setSymbol(QgsMarkerSymbol.createSimple({"name": "circle", "color": "0,160,0,128", "size": "3"}))
    ln = add(QgsAnnotationLineItem(QgsLineString([QgsPoint(2697000, 1261550), QgsPoint(2697200, 1261580), QgsPoint(2697300, 1261520)])), 1)
    ln.setSymbol(QgsLineSymbol.createSimple({"line_color": "0,0,255,255", "line_width": "1"}))
    ln = add(QgsAnnotationLineItem(QgsLineString([QgsPoint(2697000, 1261500), QgsPoint(2697200, 1261500)])))
    ln.setSymbol(QgsLineSymbol.createSimple({"line_color": "0,0,0,255", "line_width": "0.5", "line_style": "dash"}))
    outer = QgsLineString([QgsPoint(2697400, 1261500), QgsPoint(2697600, 1261500), QgsPoint(2697600, 1261700), QgsPoint(2697400, 1261700), QgsPoint(2697400, 1261500)])
    hole = QgsLineString([QgsPoint(2697450, 1261550), QgsPoint(2697550, 1261550), QgsPoint(2697550, 1261650), QgsPoint(2697450, 1261550)])
    poly = QgsPolygon(outer)
    poly.addInteriorRing(hole)
    pg = add(QgsAnnotationPolygonItem(poly), -1)
    pg.setSymbol(QgsFillSymbol.createSimple({"color": "0,200,0,100", "outline_color": "0,100,0,255", "outline_width": "0.8"}))
    r = add(QgsAnnotationRectangleTextItem("centred in a box", QgsRectangle(2697300, 1261750, 2697500, 1261850)), 3)
    r.setFormat(fmt(10))
    r.setAlignment(Qt.AlignmentFlag.AlignHCenter | Qt.AlignmentFlag.AlignVCenter)
    r.setBackgroundSymbol(QgsFillSymbol.createSimple({"color": "255,255,180,255", "outline_style": "no"}))
    r.setFrameSymbol(QgsFillSymbol.createSimple({"color": "0,0,0,0", "outline_color": "200,0,0,255", "outline_width": "0.6"}))
    r = add(QgsAnnotationRectangleTextItem("plain, top left", QgsRectangle(2697300, 1261600, 2697500, 1261700)))
    r.setFormat(fmt(10))
    r.setBackgroundEnabled(False)
    r.setFrameEnabled(False)
    r = add(QgsAnnotationRectangleTextItem("fixed size", QgsRectangle(2697300, 1261400, 2697500, 1261500)))
    r.setFormat(fmt(10))
    r.setPlacementMode(Qgis.AnnotationPlacementMode.FixedSize)
    r.setFixedSize(QSizeF(30, 10))
    r.setFixedSizeUnit(Qgis.RenderUnit.Millimeters)
    pic = add(QgsAnnotationPictureItem(Qgis.PictureFormat.Raster, png, QgsRectangle(2697700, 1261700, 2697800, 1261800)), 4)
    pic.setFrameEnabled(True)
    pic = add(QgsAnnotationPictureItem(Qgis.PictureFormat.SVG, "/usr/share/qgis/svg/arrows/Arrow_01.svg", QgsRectangle(2697700, 1261550, 2697800, 1261650)))
    lt = add(QgsAnnotationLineTextItem("along a line", QgsLineString([QgsPoint(2697000, 1261400), QgsPoint(2697300, 1261450)])))
    lt.setFormat(fmt(10))
    p.addMapLayer(al)
    t = QgsAnnotationPointTextItem("on the main layer", QgsPointXY(2697050, 1261850))
    t.setFormat(fmt(10, color=QColor(0, 0, 160)))
    p.mainAnnotationLayer().addItem(t)
    save_project(p, "annotations-styled")


attempt("annotations styled", annotations_styled)


# Groups: nested, hidden, collapsed, a layer unchecked in a shown group, and a group drawn as one
# (a group layer) with its opacity and blending.
def groups():
    p = new_project()
    pts, lines, polys, dem = layer("points"), layer("lines"), layer("polygons"), raster()
    for lyr in (pts, lines, polys, dem):
        p.addMapLayer(lyr, False)
    root = p.layerTreeRoot()
    outer = root.addGroup("Outer")
    inner = outer.addGroup("Inner")
    inner.addLayer(pts)
    outer.addLayer(lines).setItemVisibilityChecked(False)
    hidden = root.addGroup("Hidden")
    hidden.setItemVisibilityChecked(False)
    hidden.addLayer(dem)
    collapsed = root.addGroup("Drawn as one")
    collapsed.setExpanded(False)
    collapsed.addLayer(polys)
    gl = collapsed.convertToGroupLayer(QgsGroupLayer.LayerOptions(p.transformContext()))
    p.addMapLayer(gl, False)
    gl.setOpacity(0.6)
    gl.setBlendMode(QPainter.CompositionMode.CompositionMode_Multiply)
    root.addGroup("Empty")
    save_project(p, "groups-nested-hidden-collapsed-drawn-as-one")


attempt("groups", groups)


# Bookmarks (in two coordinate systems, one turned, one in a group), relations (on one pair of
# fields, a composition, on two pairs, polymorphic) and time settings (a span with steps, each
# way a layer takes its time).
def bookmarks_relations_time():
    p = new_project()
    # In UTC, so the file is the same wherever it is written.
    UTC = __import__("qgis.PyQt.QtCore", fromlist=["Qt"]).Qt.TimeSpec.UTC
    pts, lines, polys = layer("points"), layer("lines"), layer("polygons")
    for lyr in (pts, lines, polys):
        p.addMapLayer(lyr)
    bm = p.bookmarkManager()
    old = QgsBookmark()
    old.setId("old-town")
    old.setName("Old town")
    old.setGroup("Places")
    old.setExtent(QgsReferencedRectangle(QgsRectangle(2696400, 1261250, 2698200, 1262350), CRS))
    old.setRotation(30)
    bm.addBookmark(old)
    wide = QgsBookmark()
    wide.setId("region")
    wide.setName("Region")
    wide.setExtent(QgsReferencedRectangle(QgsRectangle(8.6, 47.4, 8.9, 47.6), QgsCoordinateReferenceSystem("EPSG:4326")))
    bm.addBookmark(wide)
    rm = p.relationManager()
    for rid, name, strength, pairs in [
        ("lines_points", "lines to points", Qgis.RelationshipStrength.Association, [("id", "id")]),
        ("polygons_points", "polygons of points", Qgis.RelationshipStrength.Composition, [("id", "id")]),
        ("lines_points_kind", "lines to points by kind", Qgis.RelationshipStrength.Association, [("id", "id"), ("kind", "kind")]),
    ]:
        rel = QgsRelation()
        rel.setId(rid)
        rel.setName(name)
        rel.setReferencingLayer(lines.id() if rid.startswith("lines") else polys.id())
        rel.setReferencedLayer(pts.id())
        for referencing, referenced in pairs:
            rel.addFieldPair(referencing, referenced)
        rel.setStrength(strength)
        rm.addRelation(rel)
    poly = QgsPolymorphicRelation()
    poly.setId("by_layer")
    poly.setName("by layer")
    poly.setReferencingLayer(polys.id())
    poly.addFieldPair("id", "id")
    poly.setReferencedLayerField("name")
    poly.setReferencedLayerExpression("@layer_name")
    poly.setReferencedLayerIds([pts.id(), lines.id()])
    rm.addPolymorphicRelation(poly)
    ts = p.timeSettings()
    ts.setTemporalRange(QgsDateTimeRange(QDateTime(QDate(2024, 1, 1), QTime(0, 0), UTC), QDateTime(QDate(2024, 12, 31), QTime(0, 0), UTC)))
    ts.setTimeStep(1)
    ts.setTimeStepUnit(Qgis.TemporalUnit.Months)
    ts.setIsTemporalRangeCumulative(True)
    ts.setFramesPerSecond(2)
    tp = pts.temporalProperties()
    tp.setIsActive(True)
    tp.setMode(Qgis.VectorTemporalMode.FeatureDateTimeInstantFromField)
    tp.setStartField("day")
    tp.setAccumulateFeatures(True)
    tl = lines.temporalProperties()
    tl.setIsActive(True)
    tl.setMode(Qgis.VectorTemporalMode.FeatureDateTimeStartAndDurationFromFields)
    tl.setStartField("day")
    tl.setDurationField("value")
    tl.setDurationUnits(Qgis.TemporalUnit.Days)
    tg = polys.temporalProperties()
    tg.setIsActive(True)
    tg.setMode(Qgis.VectorTemporalMode.FixedTemporalRange)
    tg.setFixedTemporalRange(QgsDateTimeRange(QDateTime(QDate(2024, 3, 1), QTime(0, 0), UTC), QDateTime(QDate(2024, 6, 1), QTime(0, 0), UTC)))
    save_project(p, "bookmarks-relations-time")


attempt("bookmarks relations time", bookmarks_relations_time)


# Every style above as one project, each on its own layer: a single file covering them all.
def all_styles():
    p = new_project()
    qml_dir = os.path.join(OUT, "qml")
    for f in sorted(os.listdir(qml_dir)):
        name = f[:-4]
        if name.startswith("raster-"):
            lyr = raster()
        elif "marker" in name or name.startswith(("renderer-", "label-", "datadefined")) and "polygon" not in name and "line" not in name:
            lyr = layer("points")
        elif "-line-" in name or "placement-Line" in name or "placement-Curved" in name or "line-repeat" in name or "symbol-details" in name:
            lyr = layer("lines")
        else:
            lyr = layer("polygons")
        msg, ok = lyr.loadNamedStyle(os.path.join(qml_dir, f))
        lyr.setName(name)
        p.addMapLayer(lyr)
    save_project(p, "all-styles")
    with zipfile_qgz(p):
        pass


class zipfile_qgz:
    """Also writes the all-styles project as a .qgz."""
    def __init__(self, p):
        self.p = p

    def __enter__(self):
        return self

    def __exit__(self, *a):
        if not self.p.write(os.path.join(OUT, "qgs", "all-styles.qgz")):
            raise RuntimeError(self.p.error())
        return False


attempt("all styles project", all_styles)


# The inventory: the types this QGIS can write.
def inventory():
    from qgis.core import QgsProviderRegistry
    lines = [f"QGIS {Qgis.version()}"]
    for t in (Qgis.SymbolType.Marker, Qgis.SymbolType.Line, Qgis.SymbolType.Fill):
        lines.append(f"{t.name} symbol layers: {', '.join(sorted(reg.symbolLayersForType(t)))}")
    lines.append("renderers: " + ", ".join(sorted(QgsApplication.rendererRegistry().renderersList())))
    lines.append("raster renderers: singlebandgray, singlebandpseudocolor, multibandcolor, paletted, hillshade, contour, singlecolor")
    lines.append("providers: " + ", ".join(sorted(QgsProviderRegistry.instance().providerList())))
    lines.append("label placements: " + ", ".join(p.name for p in Qgis.LabelPlacement))
    lines.append("callouts: " + ", ".join(QgsApplication.calloutRegistry().calloutTypes()))
    lines.append("annotation items: " + ", ".join(QgsApplication.annotationItemRegistry().itemTypes().keys()))
    lines.append("layout items: " + ", ".join(QgsApplication.layoutItemRegistry().itemTypes().values()))
    lines.append("paint effects: " + ", ".join(sorted(QgsApplication.paintEffectRegistry().effects())))
    with open(os.path.join(OUT, "types.txt"), "w") as fh:
        fh.write("\n".join(lines) + "\n")


attempt("inventory", inventory)

with open(os.path.join(OUT, "log.txt"), "w") as fh:
    fh.write("\n".join(LOG) + "\n")

# Before exiting: QGIS may crash on exit.
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import scrub  # noqa: E402 - beside this script

scrub.scrub(OUT)
app.exitQgis()
