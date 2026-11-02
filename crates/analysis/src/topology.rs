//! Editing geometry: merging features, and topology checks (overlaps, gaps, self-intersections,
//! dangles, coverage). Computed in Web Mercator metres.

use geo::{Area, BooleanOps, BoundingRect, Contains, Intersects, Orient, orient::Direction};
use rustgis_data::{Dataset, Geometry};
use rustgis_geom::{Coord, lonlat_to_merc, merc_to_lonlat};

use crate::convert::{from_multipolygon, to_multipolygon};

/// A topology problem: the rows involved and where it is (longitude/latitude).
#[derive(Clone, Debug, PartialEq)]
pub struct TopologyError {
    pub rows: Vec<usize>,
    pub at: Coord,
}

/// A check's errors, and whether it finished (a check stops at an error or work limit).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Checked {
    pub errors: Vec<TopologyError>,
    pub complete: bool,
}

impl Checked {
    fn done(errors: Vec<TopologyError>) -> Checked {
        Checked { errors, complete: true }
    }
    fn partial(errors: Vec<TopologyError>) -> Checked {
        Checked { errors, complete: false }
    }
}

/// Most errors reported per check.
const MAX_ERRORS: usize = 10_000;
/// Most pair tests one check spends, so a huge layer can't stall the app.
const MAX_WORK: usize = 50_000_000;

/// One geometry from several: polygons are unioned, lines and points gathered.
pub fn merge(geoms: &[Geometry]) -> Option<Geometry> {
    match geoms.first()? {
        Geometry::Polygon(_) => {
            let polys: Vec<geo::MultiPolygon<f64>> =
                geoms.iter().filter_map(to_multipolygon).map(|m| m.orient(Direction::Default)).collect();
            let all: Vec<geo::Polygon<f64>> = polys.into_iter().flat_map(|m| m.0).collect();
            from_multipolygon(&geo::unary_union(all.iter()))
        }
        Geometry::Line(_) => {
            let parts: Vec<Vec<Coord>> =
                geoms.iter().filter_map(|g| if let Geometry::Line(p) = g { Some(p.clone()) } else { None }).flatten().collect();
            (!parts.is_empty()).then_some(Geometry::Line(parts))
        }
        Geometry::Point(_) | Geometry::MultiPoint(_) => {
            let pts: Vec<Coord> = geoms
                .iter()
                .flat_map(|g| match g {
                    Geometry::Point(c) => vec![*c],
                    Geometry::MultiPoint(v) => v.clone(),
                    _ => Vec::new(),
                })
                .collect();
            (!pts.is_empty()).then_some(Geometry::MultiPoint(pts))
        }
    }
}

/// The parts of a multipart geometry as separate geometries.
pub fn explode(g: &Geometry) -> Vec<Geometry> {
    match g {
        Geometry::MultiPoint(v) => v.iter().map(|c| Geometry::Point(*c)).collect(),
        Geometry::Line(parts) => parts.iter().map(|p| Geometry::Line(vec![p.clone()])).collect(),
        Geometry::Polygon(polys) => polys.iter().map(|p| Geometry::Polygon(vec![p.clone()])).collect(),
        Geometry::Point(_) => vec![g.clone()],
    }
}

fn polygons(ds: &Dataset, rows: &[usize]) -> Vec<(usize, geo::MultiPolygon<f64>, geo::Rect<f64>)> {
    rows.iter()
        .filter_map(|r| {
            let mp = ds.features.get(*r)?.geometry.as_ref().and_then(to_multipolygon)?.orient(Direction::Default);
            let bb = mp.bounding_rect()?;
            Some((*r, mp, bb))
        })
        .collect()
}

fn centre(mp: &geo::MultiPolygon<f64>) -> Coord {
    use geo::InteriorPoint;
    mp.interior_point().map(|p| merc_to_lonlat(Coord::new(p.x(), p.y()))).unwrap_or_default()
}

fn rects_touch(a: &geo::Rect<f64>, b: &geo::Rect<f64>) -> bool {
    a.min().x <= b.max().x && b.min().x <= a.max().x && a.min().y <= b.max().y && b.min().y <= a.max().y
}

/// Pairs of polygons that overlap (share area, not just an edge).
pub fn overlaps(ds: &Dataset, rows: &[usize]) -> Checked {
    let mut p = polygons(ds, rows);
    // Sweep from west to east: only polygons whose boxes overlap in x are compared.
    p.sort_by(|a, b| a.2.min().x.partial_cmp(&b.2.min().x).unwrap_or(std::cmp::Ordering::Equal));
    let mut out = Vec::new();
    let mut work = 0usize;
    crate::control::begin(p.len());
    for (i, (ra, a, ba)) in p.iter().enumerate() {
        if crate::control::cancelled() {
            return Checked::partial(out);
        }
        crate::control::reached(i);
        for (rb, b, bb) in p.iter().skip(i + 1) {
            if bb.min().x > ba.max().x {
                break;
            }
            work += 1;
            if work > MAX_WORK {
                return Checked::partial(out);
            }
            if !rects_touch(ba, bb) || !a.intersects(b) {
                continue;
            }
            let inter = a.intersection(b);
            // Shared edges leave slivers of floating-point area; a square metre is a real overlap.
            if inter.unsigned_area() > 1.0 {
                out.push(TopologyError { rows: vec![*ra, *rb], at: centre(&inter) });
                if out.len() >= MAX_ERRORS {
                    return Checked::partial(out);
                }
            }
        }
    }
    Checked::done(out)
}

/// Holes in the union of a layer's polygons that no polygon covers (the outer boundary isn't a
/// gap).
pub fn gaps(ds: &Dataset, rows: &[usize]) -> Checked {
    let all: Vec<geo::Polygon<f64>> = polygons(ds, rows).into_iter().flat_map(|(_, m, _)| m.0).collect();
    let u = geo::unary_union(all.iter());
    let mut out = Vec::new();
    for poly in &u.0 {
        for hole in poly.interiors() {
            let mp = geo::MultiPolygon::new(vec![geo::Polygon::new(hole.clone(), vec![])]);
            if mp.unsigned_area() > 1.0 {
                out.push(TopologyError { rows: Vec::new(), at: centre(&mp) });
                if out.len() >= MAX_ERRORS {
                    return Checked::partial(out);
                }
            }
        }
    }
    Checked::done(out)
}

fn segments_cross(a: (Coord, Coord), b: (Coord, Coord)) -> Option<Coord> {
    let d = (a.1.x - a.0.x) * (b.1.y - b.0.y) - (a.1.y - a.0.y) * (b.1.x - b.0.x);
    if d.abs() < 1e-12 {
        return None;
    }
    let t = ((b.0.x - a.0.x) * (b.1.y - b.0.y) - (b.0.y - a.0.y) * (b.1.x - b.0.x)) / d;
    let u = ((b.0.x - a.0.x) * (a.1.y - a.0.y) - (b.0.y - a.0.y) * (a.1.x - a.0.x)) / d;
    // Strictly inside both segments: touching at shared vertices isn't crossing.
    let e = 1e-9;
    (t > e && t < 1.0 - e && u > e && u < 1.0 - e).then(|| Coord::new(a.0.x + t * (a.1.x - a.0.x), a.0.y + t * (a.1.y - a.0.y)))
}

/// Where a path crosses itself (rings and lines; ring closures are allowed). Segments are swept
/// west to east, so only those overlapping in x are compared. A path too complex to finish
/// within the work budget is reported at its start, to be looked at, rather than passed.
fn self_crossing(path: &[Coord], closed: bool) -> Option<Coord> {
    let segs: Vec<(Coord, Coord)> = path.windows(2).filter_map(|w| Some((*w.first()?, *w.get(1)?))).collect();
    let n = segs.len();
    let minx = |s: &(Coord, Coord)| s.0.x.min(s.1.x);
    let maxx = |s: &(Coord, Coord)| s.0.x.max(s.1.x);
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|a, b| {
        let (sa, sb) = (segs.get(*a).map_or(0.0, minx), segs.get(*b).map_or(0.0, minx));
        sa.partial_cmp(&sb).unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut work = 0usize;
    for (k, &i) in order.iter().enumerate() {
        let Some(a) = segs.get(i) else { continue };
        for &j in order.iter().skip(k + 1) {
            let Some(b) = segs.get(j) else { continue };
            if minx(b) > maxx(a) {
                break;
            }
            work += 1;
            if work > MAX_WORK {
                return path.first().copied();
            }
            let (lo, hi) = (i.min(j), i.max(j));
            // Neighbouring segments share a vertex, as do a ring's first and last.
            if hi == lo + 1 || (closed && lo == 0 && hi == n - 1) {
                continue;
            }
            if let Some(c) = segments_cross(*a, *b) {
                return Some(c);
            }
        }
    }
    None
}

/// Lines and polygon rings that cross themselves.
pub fn self_intersections(ds: &Dataset, rows: &[usize]) -> Checked {
    let mut out = Vec::new();
    crate::control::begin(rows.len());
    for (i, r) in rows.iter().enumerate() {
        if crate::control::cancelled() {
            return Checked::partial(out);
        }
        crate::control::reached(i);
        let Some(g) = ds.features.get(*r).and_then(|f| f.geometry.as_ref()) else { continue };
        let paths: Vec<(Vec<Coord>, bool)> = match g {
            Geometry::Line(parts) => parts.iter().map(|p| (p.clone(), false)).collect(),
            Geometry::Polygon(polys) => polys.iter().flatten().map(|r| (r.clone(), true)).collect(),
            _ => Vec::new(),
        };
        for (p, closed) in paths {
            let m: Vec<Coord> = p.iter().map(|c| lonlat_to_merc(*c)).collect();
            if let Some(c) = self_crossing(&m, closed) {
                out.push(TopologyError { rows: vec![*r], at: merc_to_lonlat(c) });
                break;
            }
        }
        if out.len() >= MAX_ERRORS {
            return Checked::partial(out);
        }
    }
    Checked::done(out)
}

/// Line ends that touch no other line (within `tol_m` metres). Line parts are indexed on a grid,
/// so each end is measured only against parts near it.
pub fn dangles(ds: &Dataset, rows: &[usize], tol_m: f64) -> Checked {
    let lines: Vec<(usize, Vec<Vec<Coord>>)> = rows
        .iter()
        .filter_map(|r| match ds.features.get(*r)?.geometry.as_ref()? {
            Geometry::Line(parts) => Some((*r, parts.iter().map(|p| p.iter().map(|c| lonlat_to_merc(*c)).collect()).collect())),
            _ => None,
        })
        .collect();
    // Tolerance in Mercator metres grows with latitude; index with the largest it can be here.
    let max_tol = tol_m / 85f64.to_radians().cos();
    let mut ext = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for c in lines.iter().flat_map(|(_, ps)| ps.iter().flatten()) {
        ext = (ext.0.min(c.x), ext.1.min(c.y), ext.2.max(c.x), ext.3.max(c.y));
    }
    let span = (ext.2 - ext.0).max(ext.3 - ext.1).max(1.0);
    let cell = (span / 512.0).max(max_tol.min(span)).max(1e-6);
    let key = |x: f64, y: f64| (((x - ext.0) / cell).floor() as i64, ((y - ext.1) / cell).floor() as i64);
    // Each part's segments go into the cells their (widened) boxes cover.
    let mut grid: std::collections::HashMap<(i64, i64), Vec<(usize, usize)>> = std::collections::HashMap::new();
    let mut work = 0usize;
    crate::control::begin(lines.len());
    for (li, (_, parts)) in lines.iter().enumerate() {
        if crate::control::cancelled() {
            return Checked::partial(Vec::new());
        }
        crate::control::reached(li);
        for (pi, part) in parts.iter().enumerate() {
            for w in part.windows(2) {
                let (Some(a), Some(b)) = (w.first(), w.get(1)) else { continue };
                let (x0, y0) = key(a.x.min(b.x) - max_tol, a.y.min(b.y) - max_tol);
                let (x1, y1) = key(a.x.max(b.x) + max_tol, a.y.max(b.y) + max_tol);
                for gx in x0..=x1 {
                    for gy in y0..=y1 {
                        work += 1;
                        if work > MAX_WORK {
                            return Checked::partial(Vec::new());
                        }
                        let v = grid.entry((gx, gy)).or_default();
                        if v.last() != Some(&(li, pi)) {
                            v.push((li, pi));
                        }
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    crate::control::begin(lines.len());
    for (li, (r, parts)) in lines.iter().enumerate() {
        if crate::control::cancelled() {
            return Checked::partial(out);
        }
        crate::control::reached(li);
        for (pi, part) in parts.iter().enumerate() {
            // A closed part has no ends.
            if part.len() < 2 || part.first() == part.last() {
                continue;
            }
            for end in [part.first(), part.last()].into_iter().flatten() {
                let tol = tol_m / merc_to_lonlat(*end).y.to_radians().cos().max(0.01);
                let near = grid.get(&key(end.x, end.y)).map(Vec::as_slice).unwrap_or(&[]);
                // The end's own part counts too, past the stretch next to the end (a line curling
                // back onto itself, like a 6, doesn't dangle there).
                let at_start = part.first() == Some(end);
                let own_rest = rest_of_part(part, at_start, 2.0 * tol);
                let touches = !own_rest.is_empty() && rustgis_data::geometry::seg_dist2(&own_rest, *end) <= tol * tol
                    || near.iter().any(|&(oli, opi)| {
                        work += 1;
                        (oli != li || opi != pi)
                            && lines
                                .get(oli)
                                .and_then(|(_, ps)| ps.get(opi))
                                .is_some_and(|op| op.len() >= 2 && rustgis_data::geometry::seg_dist2(op, *end) <= tol * tol)
                    });
                if work > MAX_WORK {
                    return Checked::partial(out);
                }
                if !touches {
                    out.push(TopologyError { rows: vec![*r], at: merc_to_lonlat(*end) });
                }
            }
        }
        if out.len() >= MAX_ERRORS {
            return Checked::partial(out);
        }
    }
    Checked::done(out)
}

/// A part without the stretch within `skip` metres (along the line) of one end: from the first
/// vertex further than that, to the other end.
fn rest_of_part(part: &[Coord], from_start: bool, skip: f64) -> Vec<Coord> {
    let pts: Vec<Coord> = if from_start { part.to_vec() } else { part.iter().rev().copied().collect() };
    let mut walked = 0.0;
    for (i, w) in pts.windows(2).enumerate() {
        let (Some(a), Some(b)) = (w.first(), w.get(1)) else { break };
        walked += ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt();
        if walked > skip {
            return pts.get(i + 1..).map(<[Coord]>::to_vec).unwrap_or_default();
        }
    }
    Vec::new()
}

/// Points of `ds` that fall in no polygon of `by`.
pub fn not_covered(ds: &Dataset, rows: &[usize], by: &Dataset) -> Checked {
    let all: Vec<usize> = (0..by.features.len()).collect();
    let polys = polygons(by, &all);
    let mut out = Vec::new();
    crate::control::begin(rows.len());
    for (i, r) in rows.iter().enumerate() {
        if crate::control::cancelled() {
            return Checked::partial(out);
        }
        crate::control::reached(i);
        let pts: Vec<Coord> = match ds.features.get(*r).and_then(|f| f.geometry.as_ref()) {
            Some(Geometry::Point(c)) => vec![*c],
            Some(Geometry::MultiPoint(v)) => v.clone(),
            _ => continue,
        };
        for c in pts {
            let m = lonlat_to_merc(c);
            let p = geo::Point::new(m.x, m.y);
            if !polys.iter().any(|(_, mp, bb)| rects_touch(bb, &geo::Rect::new(p.0, p.0)) && mp.contains(&p)) {
                out.push(TopologyError { rows: vec![*r], at: c });
            }
        }
        if out.len() >= MAX_ERRORS {
            return Checked::partial(out);
        }
    }
    Checked::done(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_data::GeometryType;

    fn square(x: f64, y: f64, s: f64) -> Geometry {
        Geometry::Polygon(vec![vec![vec![
            Coord::new(x, y),
            Coord::new(x + s, y),
            Coord::new(x + s, y + s),
            Coord::new(x, y + s),
            Coord::new(x, y),
        ]]])
    }

    #[test]
    fn checks_find_problems() {
        let mut ds = Dataset::new("p", GeometryType::Polygon, vec![]);
        ds.push(Some(square(0.0, 0.0, 1.0)), vec![]);
        ds.push(Some(square(0.5, 0.5, 1.0)), vec![]); // overlaps the first
        ds.push(Some(square(1.0, 0.0, 1.0)), vec![]); // shares an edge with the first
        let rows: Vec<usize> = (0..3).collect();
        let o = overlaps(&ds, &rows).errors;
        assert!(o.iter().any(|e| e.rows == vec![0, 1]) && !o.iter().any(|e| e.rows == vec![0, 2]), "{o:?}");
        // A ring of four squares around a hole.
        let mut ring = Dataset::new("r", GeometryType::Polygon, vec![]);
        for (x, y) in [(0.0, 0.0), (1.0, 0.0), (2.0, 0.0), (0.0, 1.0), (2.0, 1.0), (0.0, 2.0), (1.0, 2.0), (2.0, 2.0)] {
            ring.push(Some(square(x, y, 1.0)), vec![]);
        }
        let g = gaps(&ring, &(0..8).collect::<Vec<_>>()).errors;
        assert_eq!(g.len(), 1);
        assert!((g[0].at.x - 1.5).abs() < 0.1 && (g[0].at.y - 1.5).abs() < 0.1, "{:?}", g[0].at);
        // A bow tie crosses itself.
        let mut lines = Dataset::new("l", GeometryType::Polyline, vec![]);
        lines.push(
            Some(Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(1.0, 1.0), Coord::new(1.0, 0.0), Coord::new(0.0, 1.0)]])),
            vec![],
        );
        lines.push(Some(Geometry::Line(vec![vec![Coord::new(5.0, 5.0), Coord::new(6.0, 5.0)]])), vec![]);
        lines.push(Some(Geometry::Line(vec![vec![Coord::new(6.0, 5.0), Coord::new(6.0, 6.0)]])), vec![]);
        assert_eq!(self_intersections(&lines, &[0, 1, 2]).errors.len(), 1);
        // Line 1 starts and line 2 ends in the open: two dangles (they meet at 6,5).
        assert_eq!(dangles(&lines, &[1, 2], 1.0).errors.len(), 2);
        let mut pts = Dataset::new("pt", GeometryType::Point, vec![]);
        pts.push(Some(Geometry::Point(Coord::new(0.5, 0.5))), vec![]);
        pts.push(Some(Geometry::Point(Coord::new(9.0, 9.0))), vec![]);
        assert_eq!(not_covered(&pts, &[0, 1], &ds).errors.iter().map(|e| e.rows.clone()).collect::<Vec<_>>(), vec![vec![1]]);
        // Merging two overlapping squares makes one polygon.
        let m = merge(&[square(0.0, 0.0, 1.0), square(0.5, 0.0, 1.0)]).unwrap_or(Geometry::Point(Coord::default()));
        assert!(matches!(&m, Geometry::Polygon(p) if p.len() == 1));
        assert_eq!(explode(&Geometry::MultiPoint(vec![Coord::new(0.0, 0.0), Coord::new(1.0, 1.0)])).len(), 2);
    }

    #[test]
    fn a_point_across_the_antimeridian_is_covered_by_its_polygon() {
        // A polygon kept unwrapped (179.9° to 180.1°) covers a point at 180.05°, which is -179.95° once
        // folded: the point is covered, not reported as outside the polygon.
        let ring =
            vec![Coord::new(179.9, 0.0), Coord::new(180.1, 0.0), Coord::new(180.1, 1.0), Coord::new(179.9, 1.0), Coord::new(179.9, 0.0)];
        let mut ds = Dataset::new("p", GeometryType::Polygon, vec![]);
        ds.push(Some(Geometry::Polygon(vec![vec![ring]])), vec![]);
        let mut pts = Dataset::new("pt", GeometryType::Point, vec![]);
        pts.push(Some(Geometry::Point(Coord::new(180.05, 0.5))), vec![]);
        assert!(not_covered(&pts, &[0], &ds).errors.is_empty());
    }

    #[test]
    fn long_paths_are_checked_not_skipped() {
        // 10,000 segments zigzagging east without crossing...
        let mut path: Vec<Coord> = (0..10_001).map(|i| Coord::new(i as f64 * 0.0001, (i % 2) as f64 * 0.0001)).collect();
        let mut ds = Dataset::new("l", GeometryType::Polyline, vec![]);
        ds.push(Some(Geometry::Line(vec![path.clone()])), vec![]);
        assert!(self_intersections(&ds, &[0]).errors.is_empty());
        // ...then back across the start.
        path.push(Coord::new(0.00015, -0.0001));
        path.push(Coord::new(0.00015, 0.0002));
        let mut ds = Dataset::new("l", GeometryType::Polyline, vec![]);
        ds.push(Some(Geometry::Line(vec![path])), vec![]);
        let e = self_intersections(&ds, &[0]).errors;
        assert_eq!(e.len(), 1);
        assert!((e[0].at.x - 0.00015).abs() < 1e-6);
    }

    #[test]
    fn a_line_curling_onto_itself_does_not_dangle_there() {
        // A "6": the end comes back onto the line's own earlier part.
        let six =
            vec![Coord::new(0.0, 0.003), Coord::new(0.0, 0.0), Coord::new(0.001, 0.0), Coord::new(0.001, 0.001), Coord::new(0.0, 0.001)];
        let mut ds = Dataset::new("l", GeometryType::Polyline, vec![]);
        ds.push(Some(Geometry::Line(vec![six])), vec![]);
        let d = dangles(&ds, &[0], 1.0);
        assert!(d.complete);
        assert_eq!(d.errors.len(), 1, "only the free top end dangles");
        assert!((d.errors[0].at.y - 0.003).abs() < 1e-9);
    }

    #[test]
    fn a_hairpin_ending_at_its_start_does_not_dangle() {
        // Three vertices: out, across and back to within a metre of the start.
        let hairpin = vec![Coord::new(0.0, 0.0), Coord::new(0.001, 0.0), Coord::new(0.000_005, 0.000_001)];
        let mut ds = Dataset::new("l", GeometryType::Polyline, vec![]);
        ds.push(Some(Geometry::Line(vec![hairpin])), vec![]);
        assert!(dangles(&ds, &[0], 1.0).errors.is_empty());
    }
}
