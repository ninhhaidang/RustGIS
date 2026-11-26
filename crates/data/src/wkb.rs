//! Well-known binary (WKB) geometry: the encoding GeoParquet and GeoPackage store geometry in.
//!
//! Reads ISO WKB (2D, Z, M, ZM) and PostGIS extended WKB (Z/M/SRID flags); Z and M are dropped.
//! The input is hostile: every count is checked against the bytes left, nesting is bounded and a
//! malformed geometry is `None`, never a panic.

use crate::Geometry;
use rustgis_geom::Coord;

/// Deepest nesting accepted (a collection of collections of …).
const MAX_DEPTH: usize = 8;

/// Decodes a WKB geometry. `None` when it is malformed, empty or of an unsupported type.
pub fn decode(bytes: &[u8]) -> Option<Geometry> {
    let mut r = Reader { b: bytes, pos: 0 };
    r.geometry(0).flatten()
}

/// Encodes a geometry as ISO WKB (2D, little-endian).
pub fn encode(g: &Geometry) -> Vec<u8> {
    let mut out = Vec::with_capacity(wkb_size(g));
    let head = |out: &mut Vec<u8>, ty: u32| {
        out.push(1);
        out.extend_from_slice(&ty.to_le_bytes());
    };
    let count = |out: &mut Vec<u8>, n: usize| out.extend_from_slice(&(u32::try_from(n).unwrap_or(u32::MAX)).to_le_bytes());
    let coord = |out: &mut Vec<u8>, c: &Coord| {
        out.extend_from_slice(&c.x.to_le_bytes());
        out.extend_from_slice(&c.y.to_le_bytes());
    };
    let ring = |out: &mut Vec<u8>, r: &[Coord]| {
        count(out, r.len());
        r.iter().for_each(|c| coord(out, c));
    };
    match g {
        Geometry::Point(c) => {
            head(&mut out, 1);
            coord(&mut out, c);
        }
        Geometry::MultiPoint(v) => {
            head(&mut out, 4);
            count(&mut out, v.len());
            for c in v {
                head(&mut out, 1);
                coord(&mut out, c);
            }
        }
        Geometry::Line(parts) => {
            head(&mut out, 5);
            count(&mut out, parts.len());
            for p in parts {
                head(&mut out, 2);
                ring(&mut out, p);
            }
        }
        Geometry::Polygon(polys) => {
            head(&mut out, 6);
            count(&mut out, polys.len());
            for rings in polys {
                head(&mut out, 3);
                count(&mut out, rings.len());
                rings.iter().for_each(|r| ring(&mut out, r));
            }
        }
    }
    out
}

/// Encodes a geometry only when it fits a single-type ISO WKB column.
pub fn encode_single(g: &Geometry) -> Option<Vec<u8>> {
    let (ty, count) = match g {
        Geometry::Point(_) => return Some(encode(g)),
        Geometry::MultiPoint(v) if v.len() == 1 => return v.first().map(|c| encode(&Geometry::Point(*c))),
        Geometry::Line(p) if p.len() == 1 => (2_u32, p.first()?.len()),
        Geometry::Polygon(p) if p.len() == 1 => (3_u32, p.first()?.len()),
        _ => return None,
    };
    let mut out = Vec::with_capacity(wkb_size(g).checked_sub(9)?);
    out.push(1);
    out.extend_from_slice(&ty.to_le_bytes());
    out.extend_from_slice(&u32::try_from(count).ok()?.to_le_bytes());
    match g {
        Geometry::Line(p) => {
            for c in p.first()? {
                out.extend_from_slice(&c.x.to_le_bytes());
                out.extend_from_slice(&c.y.to_le_bytes());
            }
        }
        Geometry::Polygon(p) => {
            for r in p.first()? {
                out.extend_from_slice(&u32::try_from(r.len()).ok()?.to_le_bytes());
                for c in r {
                    out.extend_from_slice(&c.x.to_le_bytes());
                    out.extend_from_slice(&c.y.to_le_bytes());
                }
            }
        }
        _ => return None,
    }
    Some(out)
}

/// Whether a WKB header carries Z or M dimensions.
pub fn has_zm(bytes: &[u8]) -> bool {
    let Some(raw) = bytes.get(1..5).and_then(|b| <[u8; 4]>::try_from(b).ok()) else { return false };
    let ty = match bytes.first() {
        Some(0) => u32::from_be_bytes(raw),
        Some(1) => u32::from_le_bytes(raw),
        _ => return false,
    };
    ty & 0xC000_0000 != 0 || (ty & 0x0FFF_FFFF) / 1000 > 0
}

/// Encoded width: 16 bytes per coordinate, 4 per count and 5 per geometry head.
pub(crate) fn wkb_size(g: &Geometry) -> usize {
    let ring = |r: &[Coord]| 4 + 16 * r.len();
    match g {
        Geometry::Point(_) => 5 + 16,
        Geometry::MultiPoint(v) => 5 + 4 + v.len() * (5 + 16),
        Geometry::Line(parts) => 5 + 4 + parts.iter().map(|p| 5 + ring(p)).sum::<usize>(),
        Geometry::Polygon(polys) => 5 + 4 + polys.iter().map(|p| 5 + 4 + p.iter().map(|r| ring(r)).sum::<usize>()).sum::<usize>(),
    }
}

/// One field of a coordinate chunk; `None` when the chunk is too short.
#[inline]
fn f64_at(ch: &[u8], off: usize, little: bool) -> Option<f64> {
    let a: [u8; 8] = ch.get(off..off + 8)?.try_into().ok()?;
    Some(if little { f64::from_le_bytes(a) } else { f64::from_be_bytes(a) })
}

struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

#[derive(Clone, Copy)]
struct Head {
    little: bool,
    ty: u32,
    dims: usize,
}

impl Reader<'_> {
    fn left(&self) -> usize {
        self.b.len().saturating_sub(self.pos)
    }

    fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
        let s = self.b.get(self.pos..self.pos.checked_add(N)?)?;
        self.pos += N;
        s.try_into().ok()
    }

    fn u32(&mut self, little: bool) -> Option<u32> {
        let a = self.take::<4>()?;
        Some(if little { u32::from_le_bytes(a) } else { u32::from_be_bytes(a) })
    }

    fn f64(&mut self, little: bool) -> Option<f64> {
        let a = self.take::<8>()?;
        Some(if little { f64::from_le_bytes(a) } else { f64::from_be_bytes(a) })
    }

    fn head(&mut self) -> Option<Head> {
        let little = match self.take::<1>()? {
            [0] => false,
            [1] => true,
            _ => return None,
        };
        let raw = self.u32(little)?;
        // Extended WKB: flags in the high bits, then an optional SRID.
        let (mut z, mut m) = (raw & 0x8000_0000 != 0, raw & 0x4000_0000 != 0);
        if raw & 0x2000_0000 != 0 {
            self.u32(little)?;
        }
        let mut ty = raw & 0x0FFF_FFFF;
        // ISO WKB: thousands carry the dimensions.
        match ty / 1000 {
            0 => {}
            1 => z = true,
            2 => m = true,
            3 => (z, m) = (true, true),
            _ => return None,
        }
        ty %= 1000;
        Some(Head { little, ty, dims: 2 + usize::from(z) + usize::from(m) })
    }

    /// A count of items each at least `min_bytes` long, rejected when it can't fit.
    fn count(&mut self, h: Head, min_bytes: usize) -> Option<usize> {
        let n = usize::try_from(self.u32(h.little)?).ok()?;
        (n.checked_mul(min_bytes)? <= self.left()).then_some(n)
    }

    fn coord(&mut self, h: Head) -> Option<Coord> {
        let x = self.f64(h.little)?;
        let y = self.f64(h.little)?;
        for _ in 2..h.dims {
            self.f64(h.little)?;
        }
        Some(Coord::new(x, y))
    }

    /// A coordinate sequence, and whether every coordinate is finite. `None` when malformed.
    fn coords(&mut self, h: Head) -> Option<(Vec<Coord>, bool)> {
        let stride = h.dims * 8;
        let n = self.count(h, stride)?;
        let end = self.pos + n * stride;
        let run = self.b.get(self.pos..end)?;
        self.pos = end;
        let mut v = Vec::with_capacity(n);
        let mut finite = true;
        // The run is bounds-checked once; each chunk is exactly one coordinate.
        for ch in run.chunks_exact(stride) {
            let c = Coord::new(f64_at(ch, 0, h.little)?, f64_at(ch, 8, h.little)?);
            finite &= c.is_finite();
            v.push(c);
        }
        Some((v, finite))
    }

    /// A polygon's rings: `Some(None)` when its exterior is unusable (fewer than three points or a
    /// non-finite one), so a hole is never taken for the exterior. Unusable holes are dropped.
    fn rings(&mut self, h: Head) -> Option<Option<Vec<Vec<Coord>>>> {
        let n = self.count(h, 4)?;
        let mut rings = Vec::with_capacity(n);
        let mut exterior_ok = true;
        // Every ring is read whatever it holds, so the reader ends up past the polygon.
        for i in 0..n {
            let (r, finite) = self.coords(h)?;
            let usable = finite && r.len() >= 3;
            if i == 0 {
                exterior_ok = usable;
            }
            if usable {
                rings.push(r);
            }
        }
        Some((exterior_ok && !rings.is_empty()).then_some(rings))
    }

    /// The next geometry: `None` when malformed (where the bytes after it start is unknown),
    /// `Some(None)` when it is well formed but empty or unusable.
    fn geometry(&mut self, depth: usize) -> Option<Option<Geometry>> {
        if depth > MAX_DEPTH {
            return None;
        }
        let h = self.head()?;
        match h.ty {
            1 => {
                let c = self.coord(h)?;
                // An empty point is NaN, NaN.
                Some(c.is_finite().then_some(Geometry::Point(c)))
            }
            2 => {
                let (line, finite) = self.coords(h)?;
                Some((finite && line.len() >= 2).then(|| Geometry::Line(vec![line])))
            }
            3 => Some(self.rings(h)?.map(|rings| Geometry::Polygon(vec![rings]))),
            4..=7 => {
                let n = self.count(h, 5)?;
                let mut parts = Vec::with_capacity(n.min(1 << 16));
                for _ in 0..n {
                    // An empty member is skipped; a malformed one makes the whole geometry
                    // malformed, since where the next member starts is unknown.
                    if let Some(g) = self.geometry(depth + 1)? {
                        parts.push(g);
                    }
                }
                Some(merge(parts))
            }
            _ => None,
        }
    }
}

/// Merges the members of a multi-geometry or collection into one geometry of the first member's
/// kind; members of other kinds are dropped.
fn merge(mut parts: Vec<Geometry>) -> Option<Geometry> {
    if parts.len() == 1 {
        // A lone member needs no regrouping; a one-point multipoint still collapses.
        return match parts.pop()? {
            Geometry::MultiPoint(v) if v.len() == 1 => v.first().copied().map(Geometry::Point),
            g => Some(g),
        };
    }
    let first = parts.first()?.kind();
    let mut points = Vec::new();
    let mut lines = Vec::new();
    let mut polys = Vec::new();
    for g in parts.into_iter().filter(|g| g.kind() == first) {
        match g {
            Geometry::Point(c) => points.push(c),
            Geometry::MultiPoint(v) => points.extend(v),
            Geometry::Line(p) => lines.extend(p),
            Geometry::Polygon(p) => polys.extend(p),
        }
    }
    match first {
        crate::GeometryType::Point if points.len() == 1 => points.first().copied().map(Geometry::Point),
        crate::GeometryType::Point => Some(Geometry::MultiPoint(points)),
        crate::GeometryType::Polyline => Some(Geometry::Line(lines)),
        crate::GeometryType::Polygon => Some(Geometry::Polygon(polys)),
        crate::GeometryType::None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square() -> Geometry {
        let r = vec![Coord::new(0.0, 0.0), Coord::new(1.0, 0.0), Coord::new(1.0, 1.0), Coord::new(0.0, 0.0)];
        Geometry::Polygon(vec![vec![r]])
    }

    #[test]
    fn single_columns_keep_their_type_and_reject_multiple_parts() {
        let line = vec![Coord::new(0.0, 0.0), Coord::new(1.0, 1.0)];
        for (g, ty) in [(Geometry::Point(Coord::new(2.0, 3.0)), 1_u32), (Geometry::Line(vec![line.clone()]), 2), (square(), 3)] {
            let bytes = encode_single(&g).unwrap();
            assert_eq!(bytes.get(1..5), Some(ty.to_le_bytes().as_slice()));
            assert_eq!(decode(&bytes), Some(g));
        }
        let point = Coord::new(2.0, 3.0);
        assert_eq!(encode_single(&Geometry::MultiPoint(vec![point])), Some(encode(&Geometry::Point(point))));
        assert_eq!(encode_single(&Geometry::MultiPoint(vec![point, point])), None);
        assert_eq!(encode_single(&Geometry::Line(vec![line.clone(), line])), None);
        assert_eq!(encode_single(&Geometry::Polygon(vec![vec![], vec![]])), None);
    }

    #[test]
    fn dimension_headers_detect_iso_and_ewkb_in_both_byte_orders() {
        for (ty, expected) in
            [(1_u32, false), (1002, true), (2003, true), (3001, true), (0x8000_0001, true), (0x4000_0001, true), (0x2000_0001, false)]
        {
            for little in [false, true] {
                let mut bytes = vec![u8::from(little)];
                bytes.extend_from_slice(&if little { ty.to_le_bytes() } else { ty.to_be_bytes() });
                assert_eq!(has_zm(&bytes), expected);
                for n in 0..5 {
                    assert!(!has_zm(&bytes[..n]));
                }
            }
        }
        assert!(!has_zm(&[7, 255, 255, 255, 255]));
    }

    #[test]
    fn round_trips() {
        for g in [
            Geometry::Point(Coord::new(1.5, -2.0)),
            Geometry::MultiPoint(vec![Coord::new(1.0, 2.0), Coord::new(3.0, 4.0)]),
            Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(1.0, 1.0)]]),
            Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(1.0, 1.0)], vec![Coord::new(2.0, 2.0), Coord::new(3.0, 3.0)]]),
            square(),
            Geometry::Polygon(vec![vec![
                vec![Coord::new(0.0, 0.0), Coord::new(9.0, 0.0), Coord::new(9.0, 9.0), Coord::new(0.0, 0.0)],
                vec![Coord::new(1.0, 1.0), Coord::new(2.0, 1.0), Coord::new(2.0, 2.0), Coord::new(1.0, 1.0)],
            ]]),
            Geometry::Polygon(vec![
                vec![vec![Coord::new(0.0, 0.0), Coord::new(1.0, 0.0), Coord::new(1.0, 1.0), Coord::new(0.0, 0.0)]],
                vec![vec![Coord::new(5.0, 5.0), Coord::new(6.0, 5.0), Coord::new(6.0, 6.0), Coord::new(5.0, 5.0)]],
            ]),
        ] {
            assert_eq!(encode(&g).len(), wkb_size(&g));
            assert_eq!(decode(&encode(&g)), Some(g));
        }
    }

    #[test]
    fn reads_big_endian_z_and_ewkb() {
        // Big-endian ISO PointZ (1001).
        let mut b = vec![0u8];
        b.extend_from_slice(&1001u32.to_be_bytes());
        for v in [3.0f64, 4.0, 99.0] {
            b.extend_from_slice(&v.to_be_bytes());
        }
        assert_eq!(decode(&b), Some(Geometry::Point(Coord::new(3.0, 4.0))));
        // EWKB point with Z and an SRID.
        let mut e = vec![1u8];
        e.extend_from_slice(&(1u32 | 0x8000_0000 | 0x2000_0000).to_le_bytes());
        e.extend_from_slice(&4326u32.to_le_bytes());
        for v in [5.0f64, 6.0, 7.0] {
            e.extend_from_slice(&v.to_le_bytes());
        }
        assert_eq!(decode(&e), Some(Geometry::Point(Coord::new(5.0, 6.0))));
    }

    #[test]
    fn a_bad_exterior_ring_drops_the_polygon_not_promotes_a_hole() {
        let ring = |pts: &[(f64, f64)]| pts.iter().map(|&(x, y)| Coord::new(x, y)).collect::<Vec<_>>();
        let hole = ring(&[(1.0, 1.0), (2.0, 1.0), (2.0, 2.0), (1.0, 1.0)]);
        // An exterior of two points, then a hole.
        let g = Geometry::Polygon(vec![vec![ring(&[(0.0, 0.0), (9.0, 0.0)]), hole.clone()]]);
        assert_eq!(decode(&encode(&g)), None);
        // A NaN vertex in the exterior.
        let g = Geometry::Polygon(vec![vec![ring(&[(0.0, 0.0), (f64::NAN, 0.0), (9.0, 9.0), (0.0, 0.0)]), hole.clone()]]);
        assert_eq!(decode(&encode(&g)), None);
        // A short hole is dropped, the polygon kept.
        let outer = ring(&[(0.0, 0.0), (9.0, 0.0), (9.0, 9.0), (0.0, 0.0)]);
        let g = Geometry::Polygon(vec![vec![outer.clone(), ring(&[(1.0, 1.0)]), hole.clone()]]);
        assert_eq!(decode(&encode(&g)), Some(Geometry::Polygon(vec![vec![outer, hole]])));
    }

    #[test]
    fn hostile_input_is_none() {
        assert_eq!(decode(&[]), None);
        assert_eq!(decode(&[7]), None);
        // A line claiming four billion points.
        let mut b = vec![1u8];
        b.extend_from_slice(&2u32.to_le_bytes());
        b.extend_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(decode(&b), None);
        // Deep nesting of collections.
        let mut deep = Vec::new();
        for _ in 0..64 {
            deep.push(1u8);
            deep.extend_from_slice(&7u32.to_le_bytes());
            deep.extend_from_slice(&1u32.to_le_bytes());
        }
        assert_eq!(decode(&deep), None);
        // Every truncation of a valid polygon.
        let full = encode(&square());
        for n in 0..full.len() {
            let _ = decode(&full[..n]);
        }
        // A multipolygon whose second member is truncated mid-ring is malformed as a whole.
        let two = encode(&Geometry::Polygon(vec![
            vec![vec![Coord::new(0.0, 0.0), Coord::new(1.0, 0.0), Coord::new(1.0, 1.0), Coord::new(0.0, 0.0)]],
            vec![vec![Coord::new(5.0, 5.0), Coord::new(6.0, 5.0), Coord::new(6.0, 6.0), Coord::new(5.0, 5.0)]],
        ]));
        assert!(decode(&two).is_some());
        assert_eq!(decode(&two[..two.len() - 20]), None);
        // An empty point.
        let mut p = vec![1u8];
        p.extend_from_slice(&1u32.to_le_bytes());
        p.extend_from_slice(&f64::NAN.to_le_bytes());
        p.extend_from_slice(&f64::NAN.to_le_bytes());
        assert_eq!(decode(&p), None);
    }
}
