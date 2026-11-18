# Shapefile Z/M pairs

Made-up data, independently encoded as bytes for RustGIS (Apache-2.0), following
[Shapefile Technical Description](https://content.esri.com/support/whitepapers/ao_/shapefile.pdf),
file headers and Tables 4, 6, 8, 12 and 14. No production writer or third-party
code generated these fixtures; no generator is needed to run the tests.

Every `.shp` contains one record. Point XY is `(1, 2)`; the single-part line's XY
vertices are `(1, 2), (3, 4)`. Both clean and lossy inputs must import this geometry
and the one `ID=1` attribute row unchanged. All four lossy inputs currently import
and report dropping Z or M; clean inputs do not report that loss.

| Stem (`shp-`) | Shape type | Extra ordinates | `.shp` bytes |
| --- | --- | --- | --- |
| `point-xy` | 1, Point | None; clean partner for both points | 128 |
| `point-z` | 11, PointZ | Z=3; M=-1e39 (no-data sentinel) | 144 |
| `point-m` | 21, PointM | M=7 | 136 |
| `line-xy` | 3, PolyLine | None; clean partner for both lines | 188 |
| `line-z` | 13, PolyLineZ | Z=[5,9]; optional M section absent | 220 |
| `line-zm` | 13, PolyLineZ | Z=[5,9], M=[7,11] | 252 |

The first 100 bytes are the format's header: file code 9994 and file length in
big-endian 16-bit words, version 1000 and shape type in little endian, then XY,
Z and M bounds as little-endian float64. Record number 1 and content length are
big endian; record contents are little endian. Each matching `.shx` is 108 bytes:
the same header with file length 54 words, then offset 50 words and the record's
content length. Line contents have one part starting at vertex 0, the XY array,
and (for type 13) the Z bounds and array, followed by M bounds and array only in
`line-zm`. The PointZ M sentinel is below the specification's -1e38 no-data limit,
so this case exercises a height without a real measure.

The shared `shp-row.dbf` is 68 bytes: dBASE III, date 2026-11-18, one row, a
65-byte header and two-byte record; one numeric field `ID`, width 1, no decimals,
header terminator 0x0d, undeleted row ` 1`, EOF 0x1a. The harness supplies identical
WGS 84 `.prj` and UTF-8 `.cpg` text beside both files. This isolates ordinate loss
from coordinate-system assumptions and DBF encoding behavior. The corpus checks
the native reader first and then `layer.addData`, so external fallbacks or empty
imports cannot stand in for valid XY geometry.
