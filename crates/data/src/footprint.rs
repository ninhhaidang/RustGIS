//! A 3D shape's footprint: the union of its faces seen from above, the hull of its points when they
//! can't be merged. Shared by the readers of 3D surfaces (File Geodatabase multipatches, CityJSON),
//! which turn their own faces into rings and keep their own counts and notices.
//!
//! In the browser no union runs (see [`MAX_UNION_FACES`]): a footprint is always the hull.

use crate::{Coord, Geometry};

/// A point with its height.
pub(crate) type P3 = [f64; 3];

/// Whether faces are merged here: not in the browser, where a panic in the union can't be caught.
/// Readers can skip building faces when they aren't.
pub(crate) const UNIONS: bool = !cfg!(target_arch = "wasm32");

/// Most faces of one shape merged by a union; past it, the hull (a union's time grows faster than
/// its faces, so one crafted shape mustn't stall loading). The browser merges none: a panic in the
/// union library can't be caught there, and hostile input mustn't end the page, so shapes are always
/// the hulls of their points. On the desktop the catch relies on the workspace's default
/// `panic = "unwind"`.
const MAX_UNION_FACES: usize = 50_000;

/// The corners of a closed four-sided face, which count as one face: a long ring costs what the
/// faces with its corners would, or two crafted faces of many vertices could cross each other billions
/// of times for the price of two.
const VERTICES_PER_FACE: usize = 5;

/// The largest X or Y taken: the hull and the union square differences of coordinates, and past this
/// they overflow to a NaN, on which the hull panics (in the browser, where it can't be caught).
const MAX_COORDINATE: f64 = 1e150;

/// Why a footprint is the hull of its points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Hull {
    /// No face shows from above (only walls).
    NoFaces,
    /// Its faces weren't merged: too many, past the budget, left out by the reader, or the union failed.
    Unmerged,
}

/// A shape's footprint: the union of its `faces` (each its outer ring and then its holes) seen from
/// above. Faces standing on edge, such as walls, show nothing and are left out. The footprint is the
/// hull of `points` when no face shows, or when its faces aren't merged: `truncated` (the reader left
/// some out, or can't vouch for them, as with a height that isn't a finite number), more seen from
/// above than [`MAX_UNION_FACES`] or than `budget` has left (which they then take from it). `points` are all the shape's points, in any grouping: they also set the scale
/// below which a face is too narrow to show. A shape with a point or a face whose X or Y isn't a
/// finite number, or passes [`MAX_COORDINATE`], has no footprint: the hull, and orienting a face,
/// panic on NaN. A height that isn't a finite number (or passes the limit) can't say which way a face looks: all of them
/// go, not only that one (which would drop it silently), and the hull stands in.
pub(crate) fn footprint(
    faces: &[Vec<Vec<P3>>],
    truncated: bool,
    points: &[Vec<P3>],
    budget: &mut usize,
) -> (Option<Geometry>, Option<Hull>) {
    use geo::{ConvexHull, Orient, orient::Direction};
    let usable = |p: &P3| p[0].abs() <= MAX_COORDINATE && p[1].abs() <= MAX_COORDINATE;
    if !points.iter().chain(faces.iter().flatten()).flatten().all(usable) {
        return (None, None);
    }
    // A height past the limit too: the tilt of a face, measured below, overflows on it.
    let truncated = truncated || points.iter().flatten().any(|p| p[2].is_nan() || p[2].abs() > MAX_COORDINATE);
    let all = || points.iter().flatten();
    let (x0, x1) = all().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), p| (a.min(p[0]), b.max(p[0])));
    let (y0, y1) = all().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), p| (a.min(p[1]), b.max(p[1])));
    let size = (x1 - x0).max(y1 - y0);
    if !size.is_finite() {
        return (None, None);
    }
    // A face shows from above by the cosine of its tilt (its projected area over its area): walls,
    // even leaning ones, show as slivers, which cover nothing and which the union can't always
    // take. Faces narrower than a ten-thousandth of the shape are left out too (a flat shape).
    let shows = |ring: &[P3]| {
        let (mut nx, mut ny, mut nz) = (0.0, 0.0, 0.0);
        let (mut longest, mut area2) = (0.0f64, 0.0);
        // From the ring's first point: projected coordinates run to millions, whose products would
        // lose the millimetres a small face is measured in.
        let o = ring.first().copied().unwrap_or([0.0; 3]);
        let local: Vec<P3> = ring.iter().map(|p| [p[0] - o[0], p[1] - o[1], p[2] - o[2]]).collect();
        for (a, b) in local.iter().zip(local.iter().cycle().skip(1)) {
            nx += (a[1] - b[1]) * (a[2] + b[2]);
            ny += (a[2] - b[2]) * (a[0] + b[0]);
            nz += (a[0] - b[0]) * (a[1] + b[1]);
            longest = longest.max((b[0] - a[0]).hypot(b[1] - a[1]));
            area2 += a[0] * b[1] - b[0] * a[1];
        }
        let n = nx.hypot(ny).hypot(nz);
        n > 0.0 && nz.abs() / n > 0.05 && longest > 0.0 && area2.abs() / longest > size * 1e-4
    };
    let ring2 = |r: &[P3]| geo::LineString::from(r.iter().map(|p| (p[0], p[1])).collect::<Vec<_>>());
    // Past the cap, a partial union would be wrong: the hull below stands in.
    let seen: Vec<geo::Polygon<f64>> = faces
        .iter()
        .filter(|_| !truncated)
        .filter_map(|face| {
            let (outer, holes) = face.split_first()?;
            (outer.len() >= 3 && shows(outer))
                .then(|| geo::Polygon::new(ring2(outer), holes.iter().map(|h| ring2(h)).collect()).orient(Direction::Default))
        })
        .collect();
    let vertices: usize = seen.iter().map(|f| f.exterior().0.len() + f.interiors().iter().map(|h| h.0.len()).sum::<usize>()).sum();
    let cost = seen.len().max(vertices / VERTICES_PER_FACE);
    let merge = UNIONS && !truncated && cost <= MAX_UNION_FACES && cost <= *budget;
    // The union library may still trip over faces it can't place: the hull stands in then. (In the
    // browser, where a panic can't be caught, nothing is merged: see `MAX_UNION_FACES`.)
    let union = if seen.is_empty() || !merge {
        None
    } else {
        // Spent before the union runs, so faces whose union panics (and become the hull) count too:
        // the budget bounds the work tried, not the footprints made.
        *budget -= cost;
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| geo::unary_union(seen.iter()))).ok().filter(|u| !u.0.is_empty())
    };
    let (shape, hull) = match union {
        Some(u) => (u, None),
        None => {
            let points: Vec<geo::Point<f64>> = all().map(|p| geo::Point::new(p[0], p[1])).collect();
            if points.len() < 3 {
                return (None, None);
            }
            let why = if UNIONS && seen.is_empty() && !truncated { Hull::NoFaces } else { Hull::Unmerged };
            (geo::MultiPolygon::new(vec![geo::MultiPoint::from(points).convex_hull()]), Some(why))
        }
    };
    let ring = |ls: &geo::LineString<f64>| ls.0.iter().map(|c| Coord::new(c.x, c.y)).collect::<Vec<_>>();
    let polys: Vec<Vec<Vec<Coord>>> = shape
        .0
        .iter()
        .filter(|p| p.exterior().0.len() >= 4)
        .map(|p| std::iter::once(ring(p.exterior())).chain(p.interiors().iter().map(ring)).collect())
        .collect();
    if polys.is_empty() { (None, None) } else { (Some(Geometry::Polygon(polys)), hull) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The area of a footprint's outer rings.
    fn area(g: &Option<Geometry>) -> f64 {
        let Some(Geometry::Polygon(polys)) = g else { return 0.0 };
        polys.iter().filter_map(|p| p.first()).map(|r| crate::geometry::ring_area(r).abs()).sum()
    }

    /// A box 10 m wide: its roof and floor show from above, its walls don't. Its footprint is the
    /// square, merged from faces given as rings, the way a reader that has them passes them.
    #[test]
    fn a_box_is_the_square_it_stands_on() {
        let square = |z: f64| vec![[0.0, 0.0, z], [10.0, 0.0, z], [10.0, 10.0, z], [0.0, 10.0, z], [0.0, 0.0, z]];
        let wall = vec![[0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [10.0, 0.0, 5.0], [0.0, 0.0, 5.0], [0.0, 0.0, 0.0]];
        let faces = vec![vec![square(0.0)], vec![square(5.0)], vec![wall.clone()]];
        let points = vec![square(0.0), square(5.0), wall];
        let mut budget = 10;
        let (shape, hull) = footprint(&faces, false, &points, &mut budget);
        assert_eq!((hull, budget), (None, 8), "the two flat faces merged: {shape:?}");
        assert!((area(&shape) - 100.0).abs() < 1e-9, "{shape:?}");
    }

    /// A courtyard stays open: a face's hole is kept in the footprint, not filled.
    #[test]
    fn a_hole_in_a_face_stays_in_the_footprint() {
        let ring = |a: f64, b: f64| vec![[a, a, 0.0], [b, a, 0.0], [b, b, 0.0], [a, b, 0.0], [a, a, 0.0]];
        let mut budget = 10;
        let (shape, hull) = footprint(&[vec![ring(0.0, 10.0), ring(3.0, 7.0)]], false, &[ring(0.0, 10.0), ring(3.0, 7.0)], &mut budget);
        assert_eq!(hull, None);
        let Some(Geometry::Polygon(polys)) = &shape else { panic!("{shape:?}") };
        assert_eq!(polys.iter().map(Vec::len).collect::<Vec<_>>(), [2], "one polygon with its hole: {shape:?}");
        assert!((area(&shape) - 100.0).abs() < 1e-9, "{shape:?}");
    }

    /// Walls alone show nothing from above: the hull of their points stands in, and says why.
    /// Left out by the reader, faces aren't merged either: the hull, as unmerged.
    #[test]
    fn walls_alone_or_faces_left_out_are_the_hull() {
        let wall = |x: f64| vec![[x, 0.0, 0.0], [x, 10.0, 0.0], [x, 10.0, 5.0], [x, 0.0, 5.0], [x, 0.0, 0.0]];
        let points = vec![wall(0.0), wall(10.0)];
        let faces = vec![vec![wall(0.0)], vec![wall(10.0)]];
        let mut budget = 10;
        let (shape, hull) = footprint(&faces, false, &points, &mut budget);
        assert_eq!((hull, budget), (Some(Hull::NoFaces), 10));
        assert!((area(&shape) - 100.0).abs() < 1e-9, "{shape:?}");
        let (shape, hull) = footprint(&faces, true, &points, &mut budget);
        assert_eq!((hull, budget), (Some(Hull::Unmerged), 10));
        assert!(shape.is_some());
    }

    /// Finite but huge coordinates overflow in the hull, which then panics on the NaN: none, with no
    /// face merged. Past the limit only: a shape just under it still has its hull.
    #[test]
    fn coordinates_too_large_to_measure_make_none() {
        let mut budget = 10;
        let huge = vec![[1e200, 1e200, 0.0], [0.0, 0.0, 0.0], [1e99, -1e99, 0.0], [2e99, -2e99, 0.0]];
        assert_eq!(footprint(&[vec![huge.clone()]], false, std::slice::from_ref(&huge), &mut budget), (None, None));
        let at = MAX_COORDINATE;
        let large = vec![[at, at, 0.0], [-at, at, 0.0], [-at, -at, 0.0], [at, -at, 0.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]];
        let (shape, hull) = footprint(&[], false, std::slice::from_ref(&large), &mut budget);
        assert!(shape.is_some() && hull == Some(Hull::NoFaces), "{shape:?}");
        assert_eq!(budget, 10);
    }

    /// A height that isn't a finite number, or is too large to measure a tilt with, makes the hull
    /// stand in, for every reader.
    #[test]
    fn a_height_that_isnt_a_number_makes_the_hull() {
        for bad in [f64::NAN, 1e200] {
            let ring = vec![[0.0, 0.0, 1.0], [10.0, 0.0, bad], [10.0, 10.0, 1.0], [0.0, 10.0, 1.0], [0.0, 0.0, 1.0]];
            let mut budget = 10;
            let (shape, hull) = footprint(&[vec![ring.clone()]], false, std::slice::from_ref(&ring), &mut budget);
            assert_eq!((hull, budget), (Some(Hull::Unmerged), 10), "{bad}");
            assert!(shape.is_some());
        }
    }

    /// A long ring costs what the faces with its corners would: past the budget, the hull.
    #[test]
    fn a_face_of_many_vertices_costs_as_many_faces() {
        let ring: Vec<P3> = (0..=200)
            .map(|i| {
                let a = f64::from(i) / 200.0 * std::f64::consts::TAU;
                [a.cos() * 100.0, a.sin() * 100.0, 0.0]
            })
            .collect();
        let mut budget = 10;
        let (shape, hull) = footprint(&[vec![ring.clone()]], false, std::slice::from_ref(&ring), &mut budget);
        assert_eq!((hull, budget), (Some(Hull::Unmerged), 10), "201 vertices cost 40");
        assert!(shape.is_some());
        let mut budget = 50;
        let (_, hull) = footprint(&[vec![ring.clone()]], false, std::slice::from_ref(&ring), &mut budget);
        assert_eq!((hull, budget), (None, 10));
    }

    /// Fewer than three points, or none finite, make no footprint.
    #[test]
    fn too_few_or_unusable_points_make_none() {
        let mut budget = 10;
        assert_eq!(footprint(&[], false, &[vec![[0.0, 0.0, 0.0], [1.0, 1.0, 0.0]]], &mut budget), (None, None));
        assert_eq!(footprint(&[], false, &[vec![[f64::NAN, 0.0, 0.0]; 4]], &mut budget), (None, None));
        assert_eq!(footprint(&[], false, &[], &mut budget), (None, None));
    }

    /// A point or a face that isn't a finite number makes no footprint rather than reaching the
    /// hull or the orienting of a face, which panic on NaN; a part of the shape isn't drawn as if it
    /// were the whole. A hole counts too: it is oriented with a face that shows.
    #[test]
    fn a_shape_with_points_that_arent_numbers_has_none() {
        let square = vec![[0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [10.0, 10.0, 0.0], [0.0, 10.0, 0.0], [0.0, 0.0, 0.0]];
        let mut budget = 10;
        for bad in [f64::NAN, f64::INFINITY] {
            let mut odd = square.clone();
            odd.insert(2, [bad, 5.0, 0.0]);
            assert_eq!(footprint(&[], false, &[square.clone(), odd.clone()], &mut budget), (None, None), "point {bad}");
            assert_eq!(footprint(&[vec![odd.clone()]], false, std::slice::from_ref(&square), &mut budget), (None, None), "face {bad}");
            let hole = vec![[2.0, 2.0, 0.0], [2.0, 4.0, 0.0], [4.0, bad, 0.0], [4.0, 2.0, 0.0], [2.0, 2.0, 0.0]];
            let face = vec![square.clone(), hole];
            assert_eq!(footprint(&[face], false, std::slice::from_ref(&square), &mut budget), (None, None), "hole {bad}");
            odd[2] = [5.0, bad, 0.0];
            assert_eq!(footprint(&[], false, &[odd], &mut budget), (None, None), "y {bad}");
        }
        assert_eq!(budget, 10, "nothing was merged");
    }
}
