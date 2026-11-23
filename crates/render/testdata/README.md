# Test data for rustgis-render

- `test-cff.otf`: a CFF (OpenType .otf) face made for the PDF font tests (#1815), so that they need
  no third-party font. It holds `.notdef` and three glyphs of distinct shapes and widths: `A` a
  square (600 units wide), `B` a triangle (700) and `C` two bars (800), 1000 units per em,
  `fsType` 0. It was made with fontTools 4 by the script below, and is part of RustGIS under its
  licence.

```python
from fontTools.fontBuilder import FontBuilder
from fontTools.pens.t2CharStringPen import T2CharStringPen

fb = FontBuilder(1000, isTTF=False)
order = [".notdef", "A", "B", "C"]
fb.setupGlyphOrder(order)
fb.setupCharacterMap({ord("A"): "A", ord("B"): "B", ord("C"): "C"})
adv = {".notdef": 500, "A": 600, "B": 700, "C": 800}

def draw(name):
    p = T2CharStringPen(adv[name], None)
    if name == "A":
        p.moveTo((50, 0)); p.lineTo((550, 0)); p.lineTo((550, 500)); p.lineTo((50, 500)); p.closePath()
    elif name == "B":
        p.moveTo((50, 0)); p.lineTo((650, 0)); p.lineTo((350, 700)); p.closePath()
    elif name == "C":
        p.moveTo((50, 0)); p.lineTo((250, 0)); p.lineTo((250, 600)); p.lineTo((50, 600)); p.closePath()
        p.moveTo((500, 0)); p.lineTo((750, 0)); p.lineTo((750, 300)); p.lineTo((500, 300)); p.closePath()
    return p.getCharString()

fb.setupCFF("RustGISTestCFF", {"FullName": "RustGIS Test CFF"}, {g: draw(g) for g in order}, {})
fb.setupHorizontalMetrics({g: (adv[g], 50) for g in order})
fb.setupHorizontalHeader(ascent=800, descent=-200)
fb.setupNameTable({"familyName": "RustGIS Test CFF", "styleName": "Regular"})
fb.setupOS2(sTypoAscender=800, sTypoDescender=-200, usWinAscent=800, usWinDescent=200, fsType=0)
fb.setupPost()
fb.save("test-cff.otf")
```

- `test-script-only.otf`: the same RustGIS-drawn outlines as `test-cff.otf`, with its Unicode
  cmap replaced by Bengali, Persian and Lao characters and no Latin letters. It reproduces
  Android's script-only fonts without bundling third-party fonts. Generated with fontTools 4:

```python
from fontTools.ttLib import TTFont
font = TTFont("test-cff.otf")
for table in font["cmap"].tables:
    if table.isUnicode():
        table.cmap = {0x0985: "A", 0x09be: "B", 0x06cc: "A", 0x06af: "C",
                      0x0e81: "A", 0x0e82: "B", 0x0eb2: "B", 0x0ec8: "C"}
font.save("test-script-only.otf")
```

- `test-fallback.otf`: one square outline per character in `Azəệ中體あ한অাیگກຂາ່`, 600 units
  wide and 1000 units per em. The glyph ids are distinct, so PDF Unicode maps can round-trip
  each character. It tests shared fallback loading/export routing, not language typography.
  Generated with fontTools 4:

```python
from fontTools.fontBuilder import FontBuilder
from fontTools.pens.t2CharStringPen import T2CharStringPen
chars = sorted(set("Azəệ中體あ한অাیگກຂາ່"))
fb = FontBuilder(1000, isTTF=False)
order = [".notdef"] + [f"g{ord(c):04X}" for c in chars]
fb.setupGlyphOrder(order)
fb.setupCharacterMap({ord(c): f"g{ord(c):04X}" for c in chars})
glyphs = {}
for name in order:
    pen = T2CharStringPen(600, None)
    if name != ".notdef":
        pen.moveTo((50, 0)); pen.lineTo((550, 0)); pen.lineTo((550, 600)); pen.lineTo((50, 600)); pen.closePath()
    glyphs[name] = pen.getCharString()
fb.setupCFF("RustGISFallbackTest", {"FullName": "RustGIS Fallback Test"}, glyphs, {})
fb.setupHorizontalMetrics({g: (600, 50) for g in order})
fb.setupHorizontalHeader(ascent=800, descent=-200)
fb.setupNameTable({"familyName": "RustGIS Fallback Test", "styleName": "Regular"})
fb.setupOS2(sTypoAscender=800, sTypoDescender=-200, usWinAscent=800, usWinDescent=200, fsType=0)
fb.setupPost()
fb.save("test-fallback.otf")
```

- `test-latin-catalog.otf`: the same original square outline, mapped to the 96 extended Latin
  letters used by the Azerbaijani and Vietnamese catalogs. It tests successful UI activation
  after a complete font arrives, not typography. Two additional kana (`あか`) exercise a
  downloaded face filling a partial installed CJK font. Generated from `test-fallback.otf`:

```python
from fontTools.ttLib import TTFont
font = TTFont("test-fallback.otf")
chars = "ÀÁÂÇÍÔÖÚÜÝàáâãçèéêìíòóôõöùúüýăĐđğĩİıŞşũƏơƯưəạẢảẤấẦầẨẩẫậắằẳẵặẹẻẽếềểễệỉỊịọỏốồổỗộớờỞởỡợỤụủỨứừửữựỳỷỹ"
for table in font["cmap"].tables:
    if table.isUnicode():
        table.cmap = {ord(c): "g0259" for c in chars}
        table.cmap.update({ord(c): "g3042" for c in "あか"})
font.save("test-latin-catalog.otf")
```
