//! Grids of cells with global ids (opengeos/RustGIS#551): Generate Tessellation and Aggregate
//! Points to Grid, in four systems.
//!
//! - **A5** ([a5geo.org](https://a5geo.org)): pentagons, all of one area at a resolution, so a
//!   count per cell is a density with no per-cell normalisation. Ids are A5's 64-bit ids in hex.
//! - **H3** (the `h3o` crate): hexagons and twelve pentagons a resolution, whose areas vary by
//!   about two to one. Ids are H3's own hex text.
//! - **S2** (the `s2` crate): quadrilaterals from a cube's faces. Ids are S2 tokens.
//! - **Hexagon**: a flat hexagon grid on the active map's projection plane (Web Mercator for a
//!   globe), anchored at the plane's origin, not at a layer's corner, so a place falls in the
//!   same cell whatever layer it's gridded with. Ids are its axial coordinates, `q,r`, and only
//!   mean something in the plane the layer names (`plane`). A cell at resolution r has the area
//!   of an A5 cell at r on that plane, so the two compare at one resolution.
//!
//! Ids are text, so they survive CSV and JSON and join with data from other tools of the system.
//! A5's tools (`a5Grid`, `a5Aggregate`) are this module's: they take a system, A5 unless one is named.

use std::collections::{HashMap, HashSet};
use std::f64::consts::{PI, TAU};

use geo::{PreparedGeometry, Relate};
use rustgis_data::{Dataset, Field, FieldType, Geometry, GeometryType, Value};
use rustgis_geom::{Coord, Extent};
use s2::region::Region;
use serde_json::Value as Json;

use crate::control::{self, Pace};
use crate::{Inputs, Output, ToolResult};

/// The most cells one run makes.
pub const MAX_CELLS: u64 = 200_000;
/// A5's finest resolution.
pub const MAX_RESOLUTION: i32 = 30;
/// How far over [`MAX_CELLS`] an area's [`estimate`] may go before it's refused without being
/// covered: the estimate is rough, and the exact count still applies after.
const ESTIMATE_MARGIN: f64 = 2.0;
/// The most cells a run's coverings test, and rows an edge crosses past its own band, before keeping those in the area
/// (S2's descent, the hexagon grid's rows), over all its polygons ([`Work`]): past it the run is
/// refused, as one over the cap. S2's coarser cells on the way down are paced, not counted: there
/// are a third as many of them as of the cells tested.
const MAX_WORK: u64 = 4 * MAX_CELLS;

/// A grid system.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum System {
    A5,
    H3,
    S2,
    Hexagon,
}

impl System {
    /// The names a `system` parameter takes, as the tool's choice lists them.
    pub const NAMES: [&'static str; 4] = ["H3", "S2", "A5", "Hexagon"];

    fn parse(s: &str) -> Result<System, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "a5" => Ok(System::A5),
            "h3" => Ok(System::H3),
            "s2" => Ok(System::S2),
            "hexagon" => Ok(System::Hexagon),
            _ => Err(System::unknown(s)),
        }
    }

    /// Whether a `system` names A5, as a run that leaves it out or blank does: the one rule for
    /// the tools and for the `dggs-system` capability.
    pub fn is_a5(given: &str) -> bool {
        matches!(System::parse(given), Ok(System::A5))
    }

    fn unknown(given: &str) -> String {
        format!("System is H3, S2, A5 or Hexagon, not {}", given.chars().take(40).collect::<String>())
    }

    fn name(self) -> &'static str {
        match self {
            System::A5 => "A5",
            System::H3 => "H3",
            System::S2 => "S2",
            System::Hexagon => "Hexagon",
        }
    }

    /// The id field's name: the system's, as A5's layers have always had (`a5`).
    fn field(self) -> &'static str {
        match self {
            System::A5 => "a5",
            System::H3 => "h3",
            System::S2 => "s2",
            System::Hexagon => "hex",
        }
    }

    /// The finest resolution. The hexagon grid stops at 24 (cells of about 0.12 m², sides of
    /// about 22 cm), which keeps a cell's axial coordinates anywhere on a world-wide plane well within
    /// the 32 bits each has in its id ([`pack`]).
    fn finest(self) -> i32 {
        match self {
            System::A5 | System::S2 => 30,
            System::H3 => 15,
            System::Hexagon => 24,
        }
    }

    /// A cell's mean area on the ground at a resolution, in square metres, for the [`estimate`].
    fn mean_area_m2(self, res: i32) -> f64 {
        let sphere = 4.0 * PI * radius() * radius();
        match self {
            System::A5 | System::Hexagon => a5::cell_area(res),
            System::H3 => h3_res(res).map_or(f64::NAN, |r| sphere / r.cell_count() as f64),
            System::S2 => sphere / (6.0 * 4f64.powi(res)),
        }
    }
}

/// A5's authalic sphere, in metres; S2's areas on the unit sphere are scaled to it, and the
/// hexagon grid's cells are measured on it.
fn radius() -> f64 {
    (a5::cell_area(-1) / (4.0 * PI)).sqrt()
}

fn h3_res(res: i32) -> Option<h3o::Resolution> {
    u8::try_from(res).ok().and_then(|r| h3o::Resolution::try_from(r).ok())
}

fn num(p: &Json, k: &str) -> Option<f64> {
    p.get(k).and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))).filter(|f: &f64| f.is_finite())
}

fn text<'a>(p: &'a Json, k: &str) -> &'a str {
    p.get(k).and_then(Json::as_str).unwrap_or("")
}

/// The resolution asked for: a whole number from 0 to the system's finest.
fn resolution(p: &Json, default: f64, system: System) -> Result<i32, String> {
    let r = num(p, "resolution").unwrap_or(default);
    let finest = system.finest();
    if r.fract() != 0.0 || !(0.0..=f64::from(finest)).contains(&r) {
        return Err(match system {
            System::A5 => format!("Resolution is a whole number from 0 to {finest}"),
            s => format!("{} resolution is a whole number from 0 to {finest}", s.name()),
        });
    }
    Ok(r as i32)
}

fn output_name(p: &Json, default: &str) -> String {
    match text(p, "output").trim() {
        "" => default.to_string(),
        n => n.chars().take(200).collect(),
    }
}

/// A ring of longitudes and latitudes made one the map can draw: closed, its longitudes run on
/// from one vertex to the next (past ±180° for a cell across the antimeridian), and, for a cell
/// around a pole, closed along the pole.
fn drawable(points: impl IntoIterator<Item = Coord>) -> Option<Vec<Coord>> {
    let mut open: Vec<Coord> = points.into_iter().collect();
    if open.len() > 1 && open.first() == open.last() {
        open.pop();
    }
    let mut out = pole_as_line(open);
    let start = *out.first()?;
    if out.last() != Some(&start) {
        out.push(start);
    }
    let mut prev: Option<f64> = None;
    for c in &mut out {
        if let Some(p) = prev {
            while c.x - p > 180.0 {
                c.x -= 360.0;
            }
            while c.x - p < -180.0 {
                c.x += 360.0;
            }
        }
        prev = Some(c.x);
    }
    let (first, last) = (*out.first()?, *out.last()?);
    // Around a pole the longitudes turn a whole circle: close the ring along the pole.
    let turn = last.x - first.x;
    if turn.abs() > 180.0 {
        let pole = if out.iter().map(|c| c.y).sum::<f64>() > 0.0 { 90.0 } else { -90.0 };
        out.push(Coord::new(last.x, pole));
        out.push(Coord::new(first.x, pole));
        out.push(first);
    }
    (out.len() >= 4).then_some(out)
}

/// An open ring with corners on a pole (S2's cells that meet at a cube face's centre, from level 1
/// on, have them, and so do A5's cells round a pole) as a map of longitudes and latitudes draws it.
/// Such a corner has no longitude of its own, so without this a point near the pole fell outside
/// the outline of its cell: the pole becomes a line along it, from the longitude of the corner
/// before to that of the one after.
fn pole_as_line(open: Vec<Coord>) -> Vec<Coord> {
    let n = open.len();
    if n < 3 || open.iter().all(|c| c.y.abs() < 90.0 - rustgis_geom::POLE_TOLERANCE) {
        return open;
    }
    let mut out: Vec<Coord> = Vec::with_capacity(n + 4);
    for (i, c) in open.iter().enumerate() {
        if c.y.abs() >= 90.0 - rustgis_geom::POLE_TOLERANCE {
            let around = |k: usize| open.get(k % n).copied().unwrap_or(*c);
            let (prev, next) = (around(i + n - 1), around(i + 1));
            out.push(Coord::new(prev.x, c.y.signum() * 90.0));
            out.push(Coord::new(next.x, c.y.signum() * 90.0));
        } else {
            out.push(*c);
        }
    }
    out
}

/// An A5 cell's outline as a [`drawable`] longitude/latitude ring.
pub fn ring(cell: u64) -> Option<Vec<Coord>> {
    let b = a5::cell_to_boundary(cell, None).ok()?;
    drawable(b.iter().map(|ll| Coord::new(ll.longitude(), ll.latitude())))
}

/// The part of a closed ring west (`west`) or east of the meridian at `x`, closed; the points
/// where it crosses the meridian are put on it.
fn clip(ring: &[Coord], x: f64, west: bool) -> Vec<Coord> {
    let inside = |c: &Coord| if west { c.x <= x } else { c.x >= x };
    let mut out: Vec<Coord> = Vec::with_capacity(ring.len() + 4);
    for w in ring.windows(2) {
        let (a, b) = (w[0], w[1]);
        if inside(&a) {
            out.push(a);
        }
        if inside(&a) != inside(&b) {
            let t = (x - a.x) / (b.x - a.x);
            out.push(Coord::new(x, a.y + t * (b.y - a.y)));
        }
    }
    if let Some(first) = out.first().copied()
        && out.last() != Some(&first)
    {
        out.push(first);
    }
    out
}

/// The part of a closed ring south (`south`) or north of the parallel at `y`, closed: [`clip`]
/// across the other axis.
fn clip_lat(ring: &[Coord], y: f64, south: bool) -> Vec<Coord> {
    let flipped: Vec<Coord> = ring.iter().map(|c| Coord::new(c.y, c.x)).collect();
    clip(&flipped, y, south).into_iter().map(|c| Coord::new(c.y, c.x)).collect()
}

/// A [`drawable`] ring as a polygon for the map: itself, or, for one across the antimeridian
/// (and one round a pole), its two sides of ±180°, each within −180° to 180°, as the parts of one
/// polygon, the way Natural Earth splits a country there.
fn split(r: Vec<Coord>) -> Option<Vec<Vec<Vec<Coord>>>> {
    let out = on_the_globe_sides(vec![r]);
    (!out.is_empty()).then_some(out)
}

/// An A5 cell as a polygon for the map ([`split`]).
pub fn parts(cell: u64) -> Option<Vec<Vec<Vec<Coord>>>> {
    split(ring(cell)?)
}

/// [`on_the_globe_sides`] at the meridian `edge` (and a turn either side of it) in place of ±180°:
/// the sides of a shape on a map that wraps there, each within `edge - 360 ..= edge`. A corner on
/// the edge is moved 1e-6° (about 11 cm) into its side: a projection takes the edge's longitude
/// to one side of the map or the other, not to the side it closes.
fn sides_at(shape: Vec<Vec<Coord>>, edge: f64) -> Vec<Vec<Vec<Coord>>> {
    let shift = edge - 180.0;
    let inside = |x: f64| x.clamp(-180.0 + 1e-6, 180.0 - 1e-6);
    let moved = |rings: Vec<Vec<Coord>>, by: f64, keep: &dyn Fn(f64) -> f64| -> Vec<Vec<Coord>> {
        rings.into_iter().map(|r| r.into_iter().map(|c| Coord::new(keep(c.x) + by, c.y)).collect()).collect()
    };
    let sides = on_the_globe_sides(moved(shape, -shift, &|x| x));
    sides.into_iter().map(|side| moved(side, shift, &inside)).collect()
}

/// A shape (its outer ring, then its holes) as shapes within −180° to 180°: itself, or, for one
/// whose ring runs on past ±180°, its two sides of the meridian, the far one moved back a turn.
/// Every ring comes back closed. H3's tiler and S2's descent take longitudes on the globe only.
fn on_the_globe_sides(shape: Vec<Vec<Coord>>) -> Vec<Vec<Vec<Coord>>> {
    let rings: Vec<Vec<Coord>> = shape
        .into_iter()
        .filter_map(|mut r| {
            let first = *r.first()?;
            if r.last() != Some(&first) {
                r.push(first);
            }
            Some(r)
        })
        .collect();
    let (lo, hi) = rings.iter().flatten().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), c| (lo.min(c.x), hi.max(c.x)));
    if lo >= -180.0 && hi <= 180.0 {
        return vec![rings];
    }
    let (edge, turn) = if hi > 180.0 { (180.0, -360.0) } else { (-180.0, 360.0) };
    let near: Vec<Vec<Coord>> = rings.iter().map(|r| clip(r, edge, edge > 0.0)).collect();
    let far: Vec<Vec<Coord>> =
        rings.iter().map(|r| clip(r, edge, edge < 0.0).into_iter().map(|c| Coord::new(c.x + turn, c.y)).collect()).collect();
    // A side whose outer ring has no area is left out, and so is a hole wholly on the other side.
    [near, far]
        .into_iter()
        .filter(|side| side.first().is_some_and(|outer| outer.len() >= 4))
        .map(|side| side.into_iter().filter(|r| r.len() >= 4).collect())
        .collect()
}

/// Whether a polygon's ring can be covered: three corners or more, each a longitude and latitude
/// (within ±90°, and ±360° so a ring that runs on past the antimeridian is kept).
fn usable(ring: &[Coord]) -> bool {
    let closed = ring.len() > 1 && ring.first() == ring.last();
    let (lo, hi) = ring.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), c| (lo.min(c.x), hi.max(c.x)));
    // Wider than a turn, it isn't a ring on the globe: its sides past ±180° would overlap.
    ring.len() - usize::from(closed) >= 3
        && hi - lo <= 360.0
        && ring.iter().all(|c| c.is_finite() && c.y.abs() <= 90.0 && c.x.abs() <= 360.0)
}

/// A longitude folded into −180° to 180°, for a point.
fn fold(lon: f64) -> f64 {
    if (-180.0..=180.0).contains(&lon) { lon } else { (lon + 180.0).rem_euclid(360.0) - 180.0 }
}

/// An extent as a ring, its edges followed every degree, so a parallel isn't taken for a great
/// circle.
fn extent_ring(e: Extent) -> Vec<Coord> {
    let steps = |a: f64, b: f64| ((b - a).abs().ceil() as usize).clamp(1, 720);
    let mut ring = Vec::new();
    let n = steps(e.min_x, e.max_x);
    ring.extend((0..n).map(|i| Coord::new(e.min_x + (e.max_x - e.min_x) * i as f64 / n as f64, e.min_y)));
    let m = steps(e.min_y, e.max_y);
    ring.extend((0..m).map(|i| Coord::new(e.max_x, e.min_y + (e.max_y - e.min_y) * i as f64 / m as f64)));
    ring.extend((0..n).map(|i| Coord::new(e.max_x - (e.max_x - e.min_x) * i as f64 / n as f64, e.max_y)));
    ring.extend((0..m).map(|i| Coord::new(e.min_x, e.max_y - (e.max_y - e.min_y) * i as f64 / m as f64)));
    ring
}

/// An extent within the globe's longitudes and latitudes, for a view panned past ±180° or zoomed
/// out past the poles; `None` when nothing of it is on the globe. A side with no width (one point,
/// or a line along a parallel or meridian) is widened by a hair, so the cells holding it are found.
fn on_the_globe(e: Extent) -> Option<Extent> {
    const HAIR: f64 = 1e-7;
    let side = |lo: f64, hi: f64, limit: f64| {
        let (lo, hi) = (lo.max(-limit), hi.min(limit));
        if lo == hi { ((lo - HAIR).max(-limit), (hi + HAIR).min(limit)) } else { (lo, hi) }
    };
    let ((x0, x1), (y0, y1)) = (side(e.min_x, e.max_x, 180.0), side(e.min_y, e.max_y, 90.0));
    (x0 < x1 && y0 < y1).then(|| Extent::new(x0, y0, x1, y1))
}

fn too_many(n: impl std::fmt::Display, res: i32) -> String {
    format!("{n} cells at resolution {res} is too many (at most {MAX_CELLS}): use a coarser resolution or a smaller area")
}

/// A ring's area (absolute) and length on the authalic sphere, its edges taken as great circles,
/// in square metres and metres.
///
/// The sphere, not the WGS 84 ellipsoid, for a comparison it can't change: H3, S2 and A5 give
/// their cells' areas on the authalic sphere (from `h3o`, `s2` and `a5`), so the hexagon grid's
/// `area_km2`, and the estimate before any covering, are measured on it too, and a cell in one
/// system compares with a cell in another. On the Earth it's under 1% from the ellipsoid's area.
fn on_the_sphere(ring: &[Coord]) -> (f64, f64) {
    let r = radius();
    let (mut twice, mut arc) = (0.0, 0.0);
    for (k, a) in ring.iter().enumerate() {
        let Some(b) = ring.get((k + 1) % ring.len()) else { break };
        let (l1, p1, l2, p2) = (a.x.to_radians(), a.y.to_radians(), b.x.to_radians(), b.y.to_radians());
        let mut dl = l2 - l1;
        if dl > PI {
            dl -= TAU;
        } else if dl < -PI {
            dl += TAU;
        }
        twice += dl * (2.0 + p1.sin() + p2.sin());
        let h = ((p2 - p1) / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
        arc += 2.0 * h.clamp(0.0, 1.0).sqrt().asin();
    }
    ((twice / 2.0).abs() * r * r, arc * r)
}

/// About how many cells of `cell` square metres cover a polygon (its outer ring, then its
/// holes), worked out before it's covered: its area over a cell's, and a cell per cell's width
/// along its rings, where a covering adds the cells its edges cross. Edges are taken as great
/// circles.
fn estimate(rings: &[Vec<Coord>], cell: f64) -> f64 {
    let (mut area, mut length) = (0.0, 0.0);
    for (i, ring) in rings.iter().enumerate() {
        let (a, l) = on_the_sphere(ring);
        area += if i == 0 { a } else { -a };
        length += l;
    }
    if !area.is_finite() || !length.is_finite() || cell.is_nan() || cell <= 0.0 {
        return f64::INFINITY;
    }
    area.max(0.0) / cell + length / cell.sqrt()
}

/// The cells at a resolution of A5 coverings (each compacted, as A5 gives them), each once and in
/// id order; more than [`MAX_CELLS`] is an error. A covering's cells don't overlap, so it's
/// counted exactly before any is made. Coverings can overlap one another (neighbouring parcels,
/// buffers), so they're merged into a set as they come and a cell they share counts once.
fn cells_of(coverings: impl IntoIterator<Item = Result<Vec<u64>, String>>, res: i32) -> Result<Vec<u64>, String> {
    // A coarser cell holds the ratio of the two resolutions' cell counts.
    let total = a5::get_num_cells(res);
    let mut cells: HashSet<u64> = HashSet::new();
    // Covered one at a time, so a run over the cap stops at the covering that takes it there.
    for covering in coverings {
        let covering = covering?;
        let parts = || covering.iter().copied().filter(|c| !a5::is_compaction_marker(*c));
        let n = parts().fold(0u64, |n, c| n.saturating_add(total / a5::get_num_cells(a5::get_resolution(c)).max(1)));
        if n > MAX_CELLS {
            return Err(too_many(n, res));
        }
        for c in parts() {
            if a5::get_resolution(c) == res {
                cells.insert(c);
            } else {
                cells.extend(a5::cell_to_children(c, Some(res))?);
            }
        }
        if cells.len() as u64 > MAX_CELLS {
            return Err(too_many(format!("At least {}", cells.len()), res));
        }
    }
    let mut cells: Vec<u64> = cells.into_iter().collect();
    cells.sort_unstable();
    Ok(cells)
}

/// Adds a covering's cells to a run's, refusing once they pass [`MAX_CELLS`].
fn gather(cells: &mut HashSet<u64>, more: impl IntoIterator<Item = u64>, res: i32) -> Result<(), String> {
    for c in more {
        cells.insert(c);
        if cells.len() as u64 > MAX_CELLS {
            return Err(too_many(format!("At least {}", cells.len()), res));
        }
    }
    Ok(())
}

fn sorted(cells: HashSet<u64>) -> Vec<u64> {
    let mut cells: Vec<u64> = cells.into_iter().collect();
    cells.sort_unstable();
    cells
}

/// A shape's rings as a `geo` polygon (in whatever plane its coordinates are).
fn geo_polygon(shape: &[Vec<Coord>]) -> Option<geo::Polygon<f64>> {
    let ls = |r: &Vec<Coord>| geo::LineString::from(r.iter().map(|c| (c.x, c.y)).collect::<Vec<_>>());
    let (outer, holes) = shape.split_first()?;
    Some(geo::Polygon::new(ls(outer), holes.iter().map(ls).collect()))
}

fn geo_multi(parts: &[Vec<Vec<Coord>>]) -> geo::MultiPolygon<f64> {
    geo::MultiPolygon::new(parts.iter().filter_map(|p| geo_polygon(p)).collect())
}

/// "1 polygon part" or "2 polygon parts" (and points) without a usable outline (or position).
fn left_out(n: usize, what: &str, lacking: &str) -> String {
    match n {
        1 => format!("1 {what} without a usable {lacking} was left out"),
        n => format!("{n} {what}s without a usable {lacking} were left out"),
    }
}

/// Cells left out because their outline couldn't be drawn: for the hexagon grid, corners the
/// map's projection can't take back to longitudes and latitudes.
fn undrawable(n: usize, g: &Grid<'_>) -> String {
    let (one, many) = match g.system {
        System::Hexagon => {
            let plane = g.plane.name();
            (format!("its corners are outside what {plane} reaches"), format!("their corners are outside what {plane} reaches"))
        }
        _ => ("its outline couldn't be worked out".to_string(), "their outlines couldn't be worked out".to_string()),
    };
    match n {
        1 => format!("1 cell was left out: {one}"),
        n => format!("{n} cells were left out: {many}"),
    }
}

/// The plane the anchored hexagon grid is laid on: the active map's flat projection, or Web
/// Mercator for a globe (or no map). Its origin, (0, 0), anchors the grid.
struct Plane<'a> {
    inp: &'a dyn Inputs,
    /// The map's projection, when the map is flat.
    map: Option<String>,
}

impl<'a> Plane<'a> {
    fn of(inp: &'a dyn Inputs) -> Self {
        Plane { inp, map: inp.projection_name() }
    }

    /// How far north and south the plane reaches, when it stops short of the poles: Web
    /// Mercator's 85.05°, past which it clamps latitudes rather than refusing them.
    /// Told by the plane itself, not its name: a point near a pole that doesn't come back where it
    /// was, where one at Web Mercator's limit does.
    fn lat_limit(&self) -> Option<f64> {
        let limit = rustgis_geom::MAX_LAT;
        let back = |lat: f64| self.forward(Coord::new(0.0, lat)).and_then(|m| self.inverse(m)).is_some_and(|c| (c.y - lat).abs() < 1e-6);
        (!back(89.9) && back(limit - 1e-6)).then_some(limit)
    }

    /// A point on the plane, only where the plane takes it back to the same place: a
    /// projection that clamps or folds a point (Web Mercator past 85.05°) would put it in a cell
    /// that doesn't hold it.
    fn holding_point(&self, c: Coord) -> Option<Coord> {
        let m = self.forward(c)?;
        // A pole is one place whatever its longitude, which a plane that draws it as a point
        // (Mollweide) can't give back: any finite place for it holds it, on a plane that reaches
        // the poles (Web Mercator's would be its edge, 85.05°).
        if c.y.abs() >= 90.0 - rustgis_geom::POLE_TOLERANCE && self.lat_limit().is_none() {
            return Some(m);
        }
        let back = self.inverse(m)?;
        ((back.y - c.y).abs() < 1e-6 && ((back.x - c.x + 180.0).rem_euclid(360.0) - 180.0).abs() < 1e-6).then_some(m)
    }

    fn name(&self) -> String {
        self.map.clone().unwrap_or_else(|| "Web Mercator".into())
    }

    /// The meridian where the plane wraps from one side to the other: a shape is cut there, not at
    /// ±180°, before its corners are put on the plane. 180 for Web Mercator, and for a plane
    /// limited to an area of use, which never reaches its edge.
    fn edge(&self) -> f64 {
        self.map.as_ref().and_then(|_| self.inp.map_edge()).filter(|e| e.is_finite()).unwrap_or(180.0)
    }

    fn forward(&self, c: Coord) -> Option<Coord> {
        match self.map {
            Some(_) => self.inp.project(c),
            None => c.is_finite().then(|| rustgis_geom::lonlat_to_merc(c)),
        }
        .filter(|c| c.is_finite())
    }

    fn inverse(&self, m: Coord) -> Option<Coord> {
        match self.map {
            Some(_) => self.inp.unproject(m),
            None => m.is_finite().then(|| rustgis_geom::merc_to_lonlat(m)),
        }
        .filter(|c| c.is_finite())
    }
}

const SQRT3: f64 = 1.732_050_807_568_877_2;

/// Pointy-top hexagons on a plane, of side (and circumradius) `size`, the one at axial `(0, 0)`
/// centred on the origin.
#[derive(Clone, Copy)]
struct Hex {
    size: f64,
}

impl Hex {
    /// A resolution's hexagon: the area of an A5 cell at that resolution, on the plane.
    fn at(res: i32) -> Self {
        Hex { size: (2.0 * a5::cell_area(res) / (3.0 * SQRT3)).sqrt() }
    }

    fn centre(self, q: i64, r: i64) -> Coord {
        Coord::new(self.size * SQRT3 * (q as f64 + r as f64 / 2.0), self.size * 1.5 * r as f64)
    }

    /// The hexagon holding a point of the plane, by rounding its cube coordinates.
    fn holding(self, m: Coord) -> Option<(i64, i64)> {
        let q = (SQRT3 / 3.0 * m.x - m.y / 3.0) / self.size;
        let r = (2.0 / 3.0 * m.y) / self.size;
        let s = -q - r;
        let (mut rq, mut rr, rs) = (q.round(), r.round(), s.round());
        let (dq, dr, ds) = ((rq - q).abs(), (rr - r).abs(), (rs - s).abs());
        if dq > dr && dq > ds {
            rq = -rr - rs;
        } else if dr > ds {
            rr = -rq - rs;
        }
        let fits = |v: f64| v.is_finite() && v.abs() <= f64::from(i32::MAX);
        (fits(rq) && fits(rr)).then_some((rq as i64, rr as i64))
    }

    /// Its corners on the plane, closed, each side cut in `per_side` so it bends with the
    /// projection once taken back to longitudes and latitudes.
    fn corners(self, q: i64, r: i64, per_side: usize) -> Vec<Coord> {
        let c = self.centre(q, r);
        let corner = |k: usize| {
            let a = (30.0 + 60.0 * k as f64).to_radians();
            Coord::new(c.x + self.size * a.cos(), c.y + self.size * a.sin())
        };
        let mut out = Vec::with_capacity(6 * per_side + 1);
        for k in 0..6 {
            let (a, b) = (corner(k), corner(k + 1));
            out.extend((0..per_side).map(|i| {
                let t = i as f64 / per_side as f64;
                Coord::new(a.x + t * (b.x - a.x), a.y + t * (b.y - a.y))
            }));
        }
        out.push(corner(0));
        out
    }
}

/// A hexagon's axial coordinates in one id: `q` in the high half, `r` in the low one.
fn pack(q: i64, r: i64) -> u64 {
    (u64::from(q as i32 as u32) << 32) | u64::from(r as i32 as u32)
}

fn unpack(id: u64) -> (i64, i64) {
    (i64::from((id >> 32) as u32 as i32), i64::from(id as u32 as i32))
}

/// A grid: a system at a resolution, and for the hexagon grid, its plane.
struct Grid<'a> {
    system: System,
    res: i32,
    plane: Plane<'a>,
    hex: Hex,
    /// Whether a covering cut its area at the plane's [`Plane::lat_limit`], in the north (1) or
    /// the south (2), to say so.
    cut: std::cell::Cell<u8>,
}

impl<'a> Grid<'a> {
    fn new(system: System, res: i32, inp: &'a dyn Inputs) -> Self {
        Grid { system, res, plane: Plane::of(inp), hex: Hex::at(res), cut: std::cell::Cell::new(0) }
    }

    /// Where a covering was cut at the plane's limit, in words.
    fn cut_sides(&self) -> Option<&'static str> {
        match self.cut.get() {
            0 => None,
            1 => Some("north"),
            2 => Some("south"),
            _ => Some("north and south"),
        }
    }

    /// The cell holding a longitude/latitude point.
    fn cell_of(&self, c: Coord) -> Option<u64> {
        if !c.is_finite() || c.y.abs() > 90.0 {
            return None;
        }
        let lon = fold(c.x);
        match self.system {
            System::A5 => a5::lonlat_to_cell(a5::LonLat::new(c.x, c.y), self.res).ok(),
            System::H3 => Some(u64::from(h3o::LatLng::new(c.y, lon).ok()?.to_cell(h3_res(self.res)?))),
            System::S2 => {
                let id = s2::cellid::CellID::from(s2::latlng::LatLng::from_degrees(c.y, lon));
                Some(id.parent(u64::try_from(self.res).ok()?).0)
            }
            System::Hexagon => {
                let (q, r) = self.hex.holding(self.plane.holding_point(Coord::new(lon, c.y))?)?;
                Some(pack(q, r))
            }
        }
    }

    /// A cell's outline, [`drawable`].
    fn ring(&self, id: u64) -> Option<Vec<Coord>> {
        match self.system {
            System::A5 => self::ring(id),
            System::H3 => {
                let cell = h3o::CellIndex::try_from(id).ok()?;
                drawable(cell.boundary().iter().map(|ll| Coord::new(ll.lng(), ll.lat())))
            }
            System::S2 => {
                let cell = s2::cell::Cell::from(s2::cellid::CellID(id));
                // Coarse cells' sides are long great circles: followed, so they're drawn bent.
                let per_side = 1usize << (8 - self.res.clamp(0, 8));
                let corners: [[f64; 3]; 4] = std::array::from_fn(|k| {
                    let v = cell.vertex(k).0;
                    [v.x, v.y, v.z]
                });
                let mut pts = Vec::with_capacity(4 * per_side);
                for (k, a) in corners.iter().enumerate() {
                    let (a, b) = (*a, corners.get((k + 1) % 4).copied().unwrap_or(*a));
                    for i in 0..per_side {
                        let t = i as f64 / per_side as f64;
                        let v = [a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1]), a[2] + t * (b[2] - a[2])];
                        let n = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
                        pts.push(Coord::new(v[1].atan2(v[0]).to_degrees(), (v[2] / n).clamp(-1.0, 1.0).asin().to_degrees()));
                    }
                }
                drawable(pts)
            }
            System::Hexagon => {
                let (q, r) = unpack(id);
                let per_side = if self.res < 10 { 4 } else { 1 };
                let pts: Option<Vec<Coord>> = self.hex.corners(q, r, per_side).into_iter().map(|m| self.plane.inverse(m)).collect();
                drawable(pts?)
            }
        }
    }

    /// A cell's id as text, or none for an index the system doesn't take (the cell is then
    /// left out and counted, never written with a blank id).
    fn id_text(&self, id: u64) -> Option<String> {
        Some(match self.system {
            System::A5 => a5::u64_to_hex(id),
            System::H3 => h3o::CellIndex::try_from(id).ok()?.to_string(),
            System::S2 => s2::cellid::CellID(id).to_token(),
            System::Hexagon => {
                let (q, r) = unpack(id);
                format!("{q},{r}")
            }
        })
    }

    /// A cell's area on the ground, in square kilometres.
    fn area_km2(&self, id: u64, ring: &[Coord]) -> f64 {
        match self.system {
            System::A5 => a5::cell_area(self.res) / 1e6,
            System::H3 => h3o::CellIndex::try_from(id).map_or(f64::NAN, |c| c.area_km2()),
            System::S2 => s2::cell::Cell::from(s2::cellid::CellID(id)).exact_area() * radius() * radius() / 1e6,
            // On the sphere, as the other three systems' areas are (see `on_the_sphere`).
            System::Hexagon => on_the_sphere(ring).0 / 1e6,
        }
    }

    /// The fields every layer of the grid starts with: the cell's id, resolution and area, and
    /// for the hexagon grid, the plane its ids are in.
    fn fields(&self) -> Vec<Field> {
        let mut f = vec![
            Field::new(self.system.field(), FieldType::Text),
            Field::new("resolution", FieldType::Integer),
            Field::new("area_km2", FieldType::Double),
        ];
        if self.system == System::Hexagon {
            f.push(Field::new("plane", FieldType::Text));
        }
        f
    }

    /// A cell as a polygon for the map and its first attributes ([`Grid::fields`]).
    fn feature(&self, id: u64) -> Option<(Geometry, Vec<Value>, f64)> {
        let ring = self.ring(id)?;
        let area = self.area_km2(id, &ring);
        let polygon = split(ring)?;
        let mut attrs = vec![Value::Text(self.id_text(id)?), Value::Int(i64::from(self.res)), Value::Float(area)];
        if self.system == System::Hexagon {
            attrs.push(Value::Text(self.plane.name()));
        }
        Some((Geometry::Polygon(polygon), attrs, area))
    }

    /// Every cell of the globe (the plane's world, for the hexagon grid).
    fn world(&self, center: bool) -> Result<Vec<u64>, String> {
        let res = self.res;
        match self.system {
            System::A5 => cells_of([a5::get_res0_cells()], res),
            System::H3 => {
                let r = h3_res(res).ok_or("No such H3 resolution")?;
                if r.cell_count() > MAX_CELLS {
                    return Err(too_many(r.cell_count(), res));
                }
                Ok(sorted(h3o::CellIndex::base_cells().flat_map(|c| c.children(r)).map(u64::from).collect()))
            }
            System::S2 => {
                let level = u64::try_from(res).map_err(|_| "No such S2 level".to_string())?;
                let n = 6u64.saturating_mul(4u64.saturating_pow(res as u32));
                if n > MAX_CELLS {
                    return Err(too_many(n, res));
                }
                let mut cells = Vec::new();
                for face in 0..6 {
                    let f = s2::cellid::CellID::from_face(face);
                    let (mut id, end) = (f.child_begin_at_level(level), f.child_end_at_level(level));
                    while id != end {
                        cells.push(id.0);
                        id = id.next();
                    }
                }
                cells.sort_unstable();
                Ok(cells)
            }
            System::Hexagon => {
                // The whole globe: a plane that stops short of the poles cuts it there and says so.
                self.cover(vec![vec![extent_ring(Extent::new(-180.0, -90.0, 180.0, 90.0))]], center)
            }
        }
    }

    /// The cells covering polygons of longitudes and latitudes (each its outer ring, then its
    /// holes), each once and in id order: those that overlap them, or with `center`, those whose
    /// centre is in them. Polygons can overlap; a cell they share counts once.
    fn cover(&self, shapes: Vec<Vec<Vec<Coord>>>, center: bool) -> Result<Vec<u64>, String> {
        control::check()?;
        let res = self.res;
        if self.system == System::A5 {
            let options =
                a5::PolygonToCellsOptions { containment: if center { a5::Containment::Center } else { a5::Containment::Overlapping } };
            let lonlat = |s: &Vec<Vec<Coord>>| -> Vec<Vec<a5::LonLat>> {
                s.iter().map(|r| r.iter().map(|c| a5::LonLat::new(c.x, c.y)).collect()).collect()
            };
            let mut pace = Pace::default();
            return cells_of(
                shapes.iter().map(|s| {
                    if pace.stop(s.iter().map(Vec::len).sum::<usize>().saturating_mul(64)) {
                        return Err(control::Cancelled.into());
                    }
                    a5::polygon_to_cells(&lonlat(s), res, Some(options))
                }),
                res,
            );
        }
        let mut cells: HashSet<u64> = HashSet::new();
        let mut work = Work::default();
        // H3 and S2 take longitudes on the globe; the hexagon grid, a run the plane draws unbroken.
        let sides = |s| if self.system == System::Hexagon { sides_at(s, self.plane.edge()) } else { on_the_globe_sides(s) };
        for side in shapes.into_iter().flat_map(sides) {
            // Weighted as A5's polygons are: H3's tiler is one step per polygon.
            work.spend(0, side.iter().map(Vec::len).sum::<usize>().saturating_mul(64), res)?;
            match self.system {
                System::H3 => self.h3_cover(&side, center, &mut cells)?,
                System::S2 => self.s2_cover(&side, center, &mut cells, &mut work)?,
                System::Hexagon => self.hex_cover(&side, center, &mut cells, &mut work)?,
                System::A5 => {}
            }
        }
        control::check()?;
        Ok(sorted(cells))
    }

    /// H3's own tiler, in one step: it's bounded by the [`estimate`] made before.
    fn h3_cover(&self, side: &[Vec<Coord>], center: bool, cells: &mut HashSet<u64>) -> Result<(), String> {
        use h3o::geom::{ContainmentMode, TilerBuilder};
        let r = h3_res(self.res).ok_or("No such H3 resolution")?;
        // The sides are within ±180° already: the tiler's guess at polygons across the
        // antimeridian would take a world-wide view for one.
        let mut tiler = TilerBuilder::new(r)
            .containment_mode(if center { ContainmentMode::ContainsCentroid } else { ContainmentMode::Covers })
            .disable_transmeridian_heuristic()
            .build();
        let Some(polygon) = geo_polygon(side) else { return Ok(()) };
        tiler.add(polygon).map_err(|e| format!("H3 can't cover a polygon: {e}"))?;
        gather(cells, tiler.into_coverage().map(u64::from), self.res)
    }

    /// S2 has no polygons: the cells of the level that meet the polygon's bounding box, found
    /// down from the cube's faces, then those that overlap the polygon (or hold its centre).
    fn s2_cover(&self, side: &[Vec<Coord>], center: bool, cells: &mut HashSet<u64>, work: &mut Work) -> Result<(), String> {
        let level = u8::try_from(self.res).map_err(|_| "No such S2 level".to_string())?;
        let Some(polygon) = geo_polygon(side) else { return Ok(()) };
        let Some(outer) = side.first() else { return Ok(()) };
        let e = outer.iter().fold(Extent::EMPTY, |e, c| e.union(&Extent::new(c.x, c.y, c.x, c.y)));
        // The descent looks at every cell of the bounding box, which for a thin, slanted polygon
        // can be many more than it covers: counted first, and refused as work, not as cells.
        let boxed = estimate(&[extent_ring(e)], self.system.mean_area_m2(self.res));
        if boxed > MAX_WORK as f64 {
            return Err(too_much_work(self.res));
        }
        let rect = s2::rect::Rect::from_degrees(e.min_y.max(-90.0), e.min_x, e.max_y.min(90.0), e.max_x);
        let prepared = PreparedGeometry::from(polygon);
        let mut stack: Vec<s2::cell::Cell> = (0..6).map(|f| s2::cell::Cell::from(s2::cellid::CellID::from_face(f))).collect();
        while let Some(cell) = stack.pop() {
            if !rect.intersects_cell(&cell) {
                continue;
            }
            if cell.level() < level {
                work.spend(0, 16, self.res)?;
                stack.extend(cell.children().into_iter().flatten());
                continue;
            }
            work.spend(1, 64, self.res)?;
            let keep = if center {
                let c = s2::latlng::LatLng::from(cell.center());
                prepared.relate(&geo::Point::new(c.lng.deg(), c.lat.deg())).is_intersects()
            } else {
                // A cell whose outline can't be worked out is kept: it's counted as left out
                // when the layer is made, never dropped here unsaid.
                self.ring(cell.id.0).and_then(split).is_none_or(|p| overlaps(&prepared.relate(&geo_multi(&p))))
            };
            if keep {
                gather(cells, [cell.id.0], self.res)?;
            }
        }
        Ok(())
    }

    /// The hexagons over a polygon, on the plane: each row's from the polygon's leftmost to its
    /// rightmost point within the row, then those that overlap it (or hold their centre in it).
    /// The polygon's edges are taken as straight on the plane, between its vertices.
    fn hex_cover(&self, side: &[Vec<Coord>], center: bool, cells: &mut HashSet<u64>, work: &mut Work) -> Result<(), String> {
        let res = self.res;
        let reach = || format!("The map's projection ({}) doesn't reach the whole area: choose a smaller area", self.plane.name());
        // Web Mercator stops at 85.05°: the area past it is cut off there, and said to be.
        let cut: Vec<Vec<Coord>>;
        let side: &[Vec<Coord>] = match self.plane.lat_limit() {
            Some(limit) if side.iter().flatten().any(|c| c.y.abs() > limit) => {
                let (north, south) = (side.iter().flatten().any(|c| c.y > limit), side.iter().flatten().any(|c| c.y < -limit));
                self.cut.set(self.cut.get() | u8::from(north) | (u8::from(south) << 1));
                let mut rings = side.iter().map(|r| clip_lat(&clip_lat(r, limit, true), -limit, false));
                // Nothing of the outer ring within reach, nothing to cover: its holes aren't one.
                let Some(outer) = rings.next().filter(|r| r.len() >= 4) else { return Ok(()) };
                cut = std::iter::once(outer).chain(rings.filter(|r| r.len() >= 4)).collect();
                &cut
            }
            _ => side,
        };
        let rings: Vec<Vec<Coord>> = side
            .iter()
            // Only where the plane takes a corner back where it was: one it folds (a transverse
            // Mercator far from its meridian) would cover the wrong cells.
            .map(|r| r.iter().map(|c| self.plane.holding_point(*c)).collect::<Option<Vec<Coord>>>())
            .collect::<Option<_>>()
            .ok_or_else(reach)?;
        let Some(outer) = rings.first() else { return Ok(()) };
        let size = self.hex.size;
        let (row_h, width) = (1.5 * size, SQRT3 * size);
        let (ymin, ymax) = outer.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), c| (lo.min(c.y), hi.max(c.y)));
        let (r0, r1) = (((ymin - size) / row_h).floor(), ((ymax + size) / row_h).ceil());
        if !(r1 - r0).is_finite() || r1 - r0 > MAX_WORK as f64 {
            return Err(too_much_work(res));
        }
        let first_row = r0 as i64;
        // Row j's hexagons reach from j·row_h − size to j·row_h + size: the outer ring's
        // leftmost and rightmost point there (a hole is inside it).
        let mut span = vec![(f64::INFINITY, f64::NEG_INFINITY); (r1 - r0) as usize + 1];
        for w in outer.windows(2) {
            let (a, b) = (w[0], w[1]);
            let (lo, hi) = (a.y.min(b.y), a.y.max(b.y));
            let (j0, j1) = (((lo - size) / row_h).ceil() as i64, ((hi + size) / row_h).floor() as i64);
            // An edge's band touches two rows, or more for an edge taller than two thirds of a
            // row: two per edge are linear in the outline, as reading it is, so only the rows
            // past them count.
            work.spend(u64::try_from(j1 - j0 - 1).unwrap_or(0), 4, res)?;
            for j in j0..=j1 {
                let (b0, b1) = (j as f64 * row_h - size, j as f64 * row_h + size);
                let at = |y: f64| a.x + (y.clamp(lo, hi) - a.y) / (b.y - a.y) * (b.x - a.x);
                let (x0, x1) = if b.y == a.y { (a.x.min(b.x), a.x.max(b.x)) } else { (at(b0).min(at(b1)), at(b0).max(at(b1))) };
                if let Some(s) = usize::try_from(j - first_row).ok().and_then(|i| span.get_mut(i)) {
                    *s = (s.0.min(x0), s.1.max(x1));
                }
            }
        }
        // The hexagons of each row within its span, counted before any is tested: each is looked
        // at, so past the work limit it's refused as work (a concave polygon takes fewer).
        let range = |j: i64, (x0, x1): (f64, f64)| {
            let half = j as f64 / 2.0;
            (((x0 - width / 2.0) / width - half).ceil() as i64, ((x1 + width / 2.0) / width - half).floor() as i64)
        };
        let rows: Vec<(i64, i64, i64)> = span
            .iter()
            .enumerate()
            .filter(|(_, s)| s.0 <= s.1)
            .map(|(i, s)| {
                let j = first_row + i as i64;
                let (q0, q1) = range(j, *s);
                (j, q0, q1)
            })
            .collect();
        let n: u64 = rows
            .iter()
            .map(|(_, q0, q1)| u64::try_from(q1.saturating_sub(*q0).saturating_add(1)).unwrap_or(0))
            .fold(0, u64::saturating_add);
        if n > MAX_WORK {
            return Err(too_much_work(res));
        }
        let Some(polygon) = geo_polygon(&rings) else { return Ok(()) };
        let prepared = PreparedGeometry::from(polygon);
        // Each test locates the cell against the whole outline, so it's paced by its vertices.
        let per_test = rings.iter().map(Vec::len).fold(64usize, usize::saturating_add);
        let fits = |v: i64| i32::try_from(v).is_ok();
        for (j, q0, q1) in rows {
            for q in q0..=q1 {
                if !fits(q) || !fits(j) {
                    return Err(reach());
                }
                work.spend(1, per_test, res)?;
                let keep = if center {
                    let c = self.hex.centre(q, j);
                    prepared.relate(&geo::Point::new(c.x, c.y)).is_intersects()
                } else {
                    let corners: Vec<(f64, f64)> = self.hex.corners(q, j, 1).iter().map(|c| (c.x, c.y)).collect();
                    overlaps(&prepared.relate(&geo::Polygon::new(corners.into(), vec![])))
                };
                if keep {
                    gather(cells, [pack(q, j)], res)?;
                }
            }
        }
        Ok(())
    }
}

/// Whether a cell overlaps an area: they share more than an edge or a corner, so a cell beside the
/// area, touching it, isn't taken.
fn overlaps(m: &geo::relate::IntersectionMatrix) -> bool {
    m.is_intersects() && !m.is_touches()
}

/// What a run's coverings look at, over all its polygons: the cells tested (S2's and the hexagon
/// grid's) and the rows an edge crosses past its own band, capped at [`MAX_WORK`] so many polygons that each stay under
/// the cap don't add up to hours; and its pace, so Cancel stops it soon.
#[derive(Default)]
struct Work {
    looked: u64,
    pace: Pace,
}

impl Work {
    /// Counts `looked` more cells or rows, and `units` of work for the pace.
    fn spend(&mut self, looked: u64, units: usize, res: i32) -> Result<(), String> {
        self.looked = self.looked.saturating_add(looked);
        if self.looked > MAX_WORK {
            return Err(too_much_work(res));
        }
        if self.pace.stop(units) {
            return Err(control::Cancelled.into());
        }
        Ok(())
    }
}

fn too_much_work(res: i32) -> String {
    format!("Covering the area at resolution {res} means looking at more than {MAX_WORK} cells: use a coarser resolution or a smaller area")
}

/// The system a tool's `system` parameter names: A5 when it's left out, as the tools were before.
fn system_of(p: &Json) -> Result<System, String> {
    match p.get("system") {
        // A5 when it names none: the tools were A5's before they took a system.
        None | Some(Json::Null) => Ok(System::A5),
        Some(Json::String(s)) => System::parse(s),
        Some(other) => Err(System::unknown(&other.to_string())),
    }
}

/// The Generate Tessellation tool (`a5Grid`): [`tessellate`] in the system the `system`
/// parameter names, A5 when it names none.
pub fn grid(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    tessellate(inp, p, system_of(p)?)
}

/// The cells at a resolution over the map's extent, a layer's polygons (or its extent, for
/// points and lines), or the whole world.
fn tessellate(inp: &dyn Inputs, p: &Json, system: System) -> Result<ToolResult, String> {
    let res = resolution(p, 5.0, system)?;
    let center = text(p, "containment") == "Center";
    let area = match text(p, "area") {
        "" | "Map Extent" => "Map Extent",
        a @ ("Input Layer" | "World") => a,
        a => return Err(format!("Area is Map Extent, Input Layer or World, not {}", a.chars().take(40).collect::<String>())),
    };
    let g = Grid::new(system, res, inp);
    // Polygons left out for want of a usable outline, and holes for want of one: the area a
    // hole left out would have kept out is covered.
    let (mut skipped, mut holes) = (0usize, 0usize);
    let off = "is outside the globe's longitudes and latitudes (−180° to 180°, −90° to 90°)";
    let cells: Vec<u64> = if area == "World" {
        g.world(center)?
    } else {
        // The polygons to cover, each its rings.
        let shapes: Vec<Vec<Vec<Coord>>> = if area == "Input Layer" {
            let name = text(p, "input");
            let ds = inp.dataset(name).ok_or_else(|| format!("No layer named {name}"))?;
            let sel = inp.selection(name);
            let rows: Vec<usize> = if sel.is_empty() { (0..ds.features.len()).collect() } else { sel.to_vec() };
            let mut shapes = Vec::new();
            let mut extent = Extent::EMPTY;
            let mut pace = Pace::default();
            for r in rows {
                if pace.stop(16) {
                    return Err(control::Cancelled.into());
                }
                match ds.features.get(r).and_then(|f| f.geometry.as_ref()) {
                    Some(Geometry::Polygon(polys)) => {
                        // A polygon whose outline isn't a ring is left out; so is a hole that isn't one.
                        for poly in polys {
                            if !poly.first().is_some_and(|r| usable(r)) {
                                skipped += 1;
                                continue;
                            }
                            holes += poly.iter().skip(1).filter(|r| !usable(r)).count();
                            shapes.push(poly.iter().filter(|r| usable(r)).cloned().collect());
                        }
                    }
                    Some(g) => extent = extent.union(&g.extent()),
                    None => {}
                }
            }
            if !extent.is_empty() {
                shapes.push(vec![extent_ring(on_the_globe(extent).ok_or_else(|| format!("The extent of {name} {off}"))?)]);
            }
            if shapes.is_empty() {
                return Err(match skipped {
                    0 => format!("{name} has no features to cover"),
                    n => format!("{name} has nothing to cover: {}", left_out(n, "polygon part", "outline")),
                });
            }
            shapes
        } else {
            let e = inp.view_extent().ok_or("There's no map view to fill; choose World or Input Layer")?;
            vec![vec![extent_ring(on_the_globe(e).ok_or_else(|| format!("The map view {off}"))?)]]
        };
        // Far too many is refused before the system works on it; the count after is exact. The
        // hexagon grid counts its cells on its plane instead, where they're of one size.
        if system != System::Hexagon {
            let guess: f64 = shapes.iter().map(|s| estimate(s, system.mean_area_m2(res))).sum();
            if guess > ESTIMATE_MARGIN * MAX_CELLS as f64 {
                return Err(too_many(if guess.is_finite() { format!("About {guess:.0}") } else { "Too many".into() }, res));
            }
        }
        let cells = g.cover(shapes, center)?;
        if cells.is_empty() {
            if let Some(sides) = g.cut_sides() {
                return Err(format!(
                    "The area is beyond {:.2}° {sides}, which {} doesn't reach: choose an area within it",
                    rustgis_geom::MAX_LAT,
                    g.plane.name()
                ));
            }
            return Err(format!("No cell at resolution {res} has its centre in the area: choose Cells: Overlapping or a finer resolution"));
        }
        cells
    };
    let mut out = Dataset::new(output_name(p, &format!("{} r{res}", system.name())), GeometryType::Polygon, g.fields());
    let (mut total, mut undrawn) = (0.0, 0usize);
    let mut pace = Pace::default();
    control::begin(cells.len());
    for (i, c) in cells.iter().enumerate() {
        control::reached(i);
        if pace.stop(64) {
            return Err(control::Cancelled.into());
        }
        match g.feature(*c) {
            Some((polygon, attrs, area)) => {
                total += area;
                out.push(Some(polygon), attrs);
            }
            None => undrawn += 1,
        }
    }
    control::finish();
    if out.features.is_empty() && undrawn > 0 {
        return Err(format!("No cell at resolution {res} could be drawn: {}", undrawable(undrawn, &g)));
    }
    let n = out.features.len();
    let mut msg = match system {
        System::A5 => format!("A5 grid: {n} cells at resolution {res}, each {:.6} km²", a5::cell_area(res) / 1e6),
        System::Hexagon => format!(
            "Hexagon grid on {}: {n} cells at resolution {res}, {:.6} km² each on average on the ground",
            g.plane.name(),
            total / n.max(1) as f64
        ),
        s => format!("{} grid: {n} cells at resolution {res}, {:.6} km² each on average", s.name(), total / n.max(1) as f64),
    };
    if skipped > 0 {
        msg.push_str(&format!("; {}", left_out(skipped, "polygon part", "outline")));
    }
    if holes > 0 {
        msg.push_str(&format!("; {}, so the area inside is covered too", left_out(holes, "hole", "outline")));
    }
    if let Some(sides) = g.cut_sides() {
        msg.push_str(&format!(
            "; the area beyond {:.2}° {sides}, which {} doesn't reach, was left out",
            rustgis_geom::MAX_LAT,
            g.plane.name()
        ));
    }
    if undrawn > 0 {
        msg.push_str(&format!("; {}", undrawable(undrawn, &g)));
    }
    Ok(ToolResult { losses: Vec::new(), output: Output::Layer(out), messages: vec![msg] })
}

/// The Aggregate Points to Grid tool (`a5Aggregate`): [`aggregate_in`] the system the `system`
/// parameter names, A5 when it names none.
pub fn aggregate(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    aggregate_in(inp, p, system_of(p)?)
}

/// A layer's points counted by the cell holding them (and a numeric field summed and averaged),
/// as the cells that hold any.
fn aggregate_in(inp: &dyn Inputs, p: &Json, system: System) -> Result<ToolResult, String> {
    let res = resolution(p, 9.0, system)?;
    let name = text(p, "input");
    let ds = inp.dataset(name).ok_or_else(|| format!("No layer named {name}"))?;
    if ds.geometry_type != GeometryType::Point {
        return Err(format!("{name} isn't a point layer"));
    }
    let field = match text(p, "field").trim() {
        "" => None,
        f => Some(ds.field_index(f).ok_or_else(|| format!("{name} has no field {f}"))?),
    };
    let g = Grid::new(system, res, inp);
    let sel = inp.selection(name);
    let rows: Vec<usize> = if sel.is_empty() { (0..ds.features.len()).collect() } else { sel.to_vec() };
    // Per cell: its points, and the field's sum and how many values it had (a feature's value
    // once in each cell, however many of a multipoint's points fall in it).
    let mut by: HashMap<u64, (u64, f64, u64)> = HashMap::new();
    let mut outside = 0usize;
    control::check()?;
    let mut pace = Pace::default();
    control::begin(rows.len());
    for (i, r) in rows.into_iter().enumerate() {
        control::reached(i);
        // One unit per feature: its points are charged below, so a point layer keeps its pace.
        if pace.stop(1) {
            return Err(control::Cancelled.into());
        }
        let Some(f) = ds.features.get(r) else { continue };
        let pts: Vec<Coord> = match &f.geometry {
            Some(Geometry::Point(c)) => vec![*c],
            Some(Geometry::MultiPoint(cs)) => cs.clone(),
            _ => continue,
        };
        let v = field.and_then(|i| ds.value(r, i).as_f64()).filter(|v| v.is_finite());
        let mut credited: HashSet<u64> = HashSet::new();
        for c in pts {
            // A multipoint's points can be many: Cancel is looked at between them too.
            if pace.stop(16) {
                return Err(control::Cancelled.into());
            }
            if !c.is_finite() || c.y.abs() > 90.0 || c.x.abs() > 360.0 {
                outside += 1;
                continue;
            }
            let Some(cell) = g.cell_of(c) else {
                outside += 1;
                continue;
            };
            let e = by.entry(cell).or_insert((0, 0.0, 0));
            e.0 += 1;
            if let Some(v) = v
                && credited.insert(cell)
            {
                e.1 += v;
                e.2 += 1;
            }
            if by.len() as u64 > MAX_CELLS {
                return Err(format!("The points fall in more than {MAX_CELLS} cells at resolution {res}: use a coarser resolution"));
            }
        }
    }
    control::finish();
    if by.is_empty() {
        let why = if outside > 0 { format!(": {}", left_out(outside, "point", "position")) } else { String::new() };
        return Err(format!("{name} has no points to count{why}"));
    }
    let mut fields = g.fields();
    fields.extend([Field::new("count", FieldType::Integer), Field::new("density_per_km2", FieldType::Double)]);
    let fname = field.and_then(|i| ds.fields.get(i)).map(|f| f.name.clone());
    if let Some(f) = &fname {
        fields.extend([Field::new(format!("{f}_sum"), FieldType::Double), Field::new(format!("{f}_mean"), FieldType::Double)]);
    }
    let mut out = Dataset::new(output_name(p, &format!("{name} {} r{res}", system.name())), GeometryType::Polygon, fields);
    let mut cells: Vec<(&u64, &(u64, f64, u64))> = by.iter().collect();
    cells.sort_unstable_by_key(|(c, _)| **c);
    let (mut total, mut undrawn, mut unplaced) = (0.0, 0usize, 0u64);
    control::check()?;
    for (c, (n, sum, nv)) in cells {
        if pace.stop(64) {
            return Err(control::Cancelled.into());
        }
        let Some((polygon, mut attrs, area)) = g.feature(*c) else {
            undrawn += 1;
            unplaced += n;
            continue;
        };
        total += area;
        attrs.extend([Value::Int(i64::try_from(*n).unwrap_or(i64::MAX)), Value::Float(*n as f64 / area)]);
        if fname.is_some() {
            attrs.extend([
                if *nv > 0 { Value::Float(*sum) } else { Value::Null },
                if *nv > 0 { Value::Float(sum / *nv as f64) } else { Value::Null },
            ]);
        }
        out.push(Some(polygon), attrs);
    }
    if out.features.is_empty() && undrawn > 0 {
        return Err(format!(
            "No cell holding {name}'s points could be drawn: {}, and so were the {unplaced} points in them",
            undrawable(undrawn, &g)
        ));
    }
    let n = out.features.len();
    let mut msg = match system {
        System::A5 => format!("{n} cells at resolution {res} ({:.6} km² each) hold the points", a5::cell_area(res) / 1e6),
        s => format!("{n} {} cells at resolution {res} ({:.6} km² each on average) hold the points", s.name(), total / n.max(1) as f64),
    };
    if outside > 0 {
        msg.push_str(&format!("; {}", left_out(outside, "point", "position")));
    }
    if undrawn > 0 {
        msg.push_str(&format!("; {}, and so were the {unplaced} points in them", undrawable(undrawn, &g)));
    }
    Ok(ToolResult { losses: Vec::new(), output: Output::Layer(out), messages: vec![msg] })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Inputs with one layer and a view.
    struct One(Dataset, Option<Extent>);
    impl Inputs for One {
        fn dataset(&self, n: &str) -> Option<&Dataset> {
            (n == self.0.name).then_some(&self.0)
        }
        fn selection(&self, _: &str) -> &[usize] {
            &[]
        }
        fn view_extent(&self) -> Option<Extent> {
            self.1
        }
    }

    fn points(cs: &[(f64, f64, f64)]) -> Dataset {
        let mut d = Dataset::new("pts", GeometryType::Point, vec![Field::new("v", FieldType::Double)]);
        for (x, y, v) in cs {
            d.push(Some(Geometry::Point(Coord::new(*x, *y))), vec![Value::Float(*v)]);
        }
        d
    }

    fn layer(r: ToolResult) -> Dataset {
        match r.output {
            Output::Layer(d) => d,
            _ => panic!("a layer"),
        }
    }

    /// Equal areas at a resolution, against A5's own count of the Earth's cells; every cell's
    /// ring closes and holds its centre.
    #[test]
    fn a_grid_of_equal_cells() {
        let inp = One(points(&[]), Some(Extent::new(-56.5, -13.5, -55.0, -12.0)));
        let d = layer(grid(&inp, &serde_json::json!({"resolution": 7})).unwrap());
        assert!(d.features.len() > 4, "{}", d.features.len());
        let areas: Vec<f64> = (0..d.features.len()).map(|r| d.value(r, 2).as_f64().unwrap()).collect();
        assert!(areas.windows(2).all(|w| w[0] == w[1]));
        let earth = 4.0 * std::f64::consts::PI * 6_371_007.180_918_475f64.powi(2) / 1e6;
        assert!((areas[0] * a5::get_num_cells(7) as f64 / earth - 1.0).abs() < 1e-6, "{}", areas[0]);
        for f in d.features.iter() {
            let Some(Geometry::Polygon(p)) = &f.geometry else { panic!("a polygon") };
            assert_eq!(p[0][0].first(), p[0][0].last());
        }
        // The whole world at resolution 1, and the cap.
        let w = layer(grid(&inp, &serde_json::json!({"resolution": 1, "area": "World"})).unwrap());
        assert_eq!(w.features.len() as u64, a5::get_num_cells(1));
        assert!(grid(&inp, &serde_json::json!({"resolution": 12, "area": "World"})).unwrap_err().contains("too many"));
        // The finest resolution's cell count (15·4³⁰, about 1.7e19) fits in a u64: the world there is
        // counted, not overflowed, and refused.
        let e = grid(&inp, &serde_json::json!({"resolution": 30, "area": "World"})).unwrap_err();
        assert!(e.starts_with(&a5::get_num_cells(30).to_string()) && e.contains("too many"), "{e}");
        // A view far too big for the resolution is refused by the estimate, before it's covered
        // (a covering whose edges alone cross millions of cells); one just over by the exact count.
        let wide = One(points(&[]), Some(Extent::new(-60.0, -30.0, -40.0, -10.0)));
        let e = grid(&wide, &serde_json::json!({"resolution": 30})).unwrap_err();
        assert!(e.starts_with("About ") && e.contains("too many"), "{e}");
        let e = grid(&One(points(&[]), Some(Extent::new(-60.0, -30.0, -46.0, -16.0))), &serde_json::json!({"resolution": 11})).unwrap_err();
        assert!(!e.starts_with("About ") && e.contains("too many"), "{e}");
        // A view panned past the antimeridian or the poles is cut to the globe; one off it is refused.
        let panned = layer(
            grid(&One(points(&[]), Some(Extent::new(-200.0, -100.0, -170.0, -80.0))), &serde_json::json!({"resolution": 3})).unwrap(),
        );
        assert!(!panned.features.is_empty());
        assert!(
            grid(&One(points(&[]), Some(Extent::new(190.0, 0.0, 200.0, 10.0))), &serde_json::json!({})).unwrap_err().contains("outside")
        );
        // A layer of one point, or of points along a parallel, has an extent with no width or
        // height: it's covered by the cells holding it, or none has its centre there.
        let one = One(points(&[(-55.72, -12.55, 1.0)]), None);
        let d = layer(grid(&one, &serde_json::json!({"resolution": 9, "area": "Input Layer", "input": "pts"})).unwrap());
        let here = a5::u64_to_hex(a5::lonlat_to_cell(a5::LonLat::new(-55.72, -12.55), 9).unwrap());
        assert!((0..d.features.len()).any(|r| d.value(r, 0) == &Value::Text(here.clone())), "{here}");
        let row = One(points(&[(-56.0, -12.55, 1.0), (-55.0, -12.55, 1.0)]), None);
        assert!(
            !layer(grid(&row, &serde_json::json!({"resolution": 9, "area": "Input Layer", "input": "pts"})).unwrap()).features.is_empty()
        );
        let e =
            grid(&one, &serde_json::json!({"resolution": 9, "area": "Input Layer", "input": "pts", "containment": "Center"})).unwrap_err();
        assert!(e.contains("Overlapping"), "{e}");
        // A polygon without a usable outline is left out, and said to be; an unknown area is refused.
        let mut shapes = Dataset::new("pts", GeometryType::Polygon, vec![]);
        let square = vec![
            Coord::new(-56.0, -13.0),
            Coord::new(-55.0, -13.0),
            Coord::new(-55.0, -12.0),
            Coord::new(-56.0, -12.0),
            Coord::new(-56.0, -13.0),
        ];
        shapes.push(Some(Geometry::Polygon(vec![vec![square], vec![vec![Coord::new(0.0, 0.0), Coord::new(1.0, 1.0)]]])), vec![]);
        shapes.push(
            Some(Geometry::Polygon(vec![vec![vec![
                Coord::new(f64::NAN, 0.0),
                Coord::new(1.0, 0.0),
                Coord::new(1.0, 1.0),
                Coord::new(0.0, 0.0),
            ]]])),
            vec![],
        );
        let r = grid(&One(shapes, None), &serde_json::json!({"resolution": 7, "area": "Input Layer", "input": "pts"})).unwrap();
        assert!(r.messages[0].contains("2 polygon parts without a usable outline"), "{:?}", r.messages);
        assert!(!layer(r).features.is_empty());
        // Corners past the poles aren't a longitude and latitude: with nothing else, the error says
        // why. A ring that runs on past the antimeridian is covered.
        let mut off = Dataset::new("pts", GeometryType::Polygon, vec![]);
        let tall =
            vec![Coord::new(0.0, 80.0), Coord::new(10.0, 80.0), Coord::new(10.0, 95.0), Coord::new(0.0, 95.0), Coord::new(0.0, 80.0)];
        off.push(Some(Geometry::Polygon(vec![vec![tall]])), vec![]);
        let e = grid(&One(off, None), &serde_json::json!({"resolution": 3, "area": "Input Layer", "input": "pts"})).unwrap_err();
        assert!(e.contains("nothing to cover: 1 polygon part without a usable outline was left out"), "{e}");
        // Overlapping polygons count the cells they share once: three copies of one, together
        // over the cap but not once merged, make that one's cells.
        let block = vec![
            Coord::new(-60.0, -30.0),
            Coord::new(-46.0, -30.0),
            Coord::new(-46.0, -16.0),
            Coord::new(-60.0, -16.0),
            Coord::new(-60.0, -30.0),
        ];
        let mut copies = Dataset::new("pts", GeometryType::Polygon, vec![]);
        for _ in 0..3 {
            copies.push(Some(Geometry::Polygon(vec![vec![block.clone()]])), vec![]);
        }
        let p10 = serde_json::json!({"resolution": 10, "area": "Input Layer", "input": "pts"});
        let three = layer(grid(&One(copies, None), &p10).unwrap()).features.len();
        let mut single = Dataset::new("pts", GeometryType::Polygon, vec![]);
        single.push(Some(Geometry::Polygon(vec![vec![block]])), vec![]);
        let alone = layer(grid(&One(single, None), &p10).unwrap()).features.len();
        assert!(3 * alone as u64 > MAX_CELLS && three == alone, "{alone} cells, {three} from three copies");
        let mut across = Dataset::new("pts", GeometryType::Polygon, vec![]);
        let wide =
            vec![Coord::new(170.0, 0.0), Coord::new(190.0, 0.0), Coord::new(190.0, 10.0), Coord::new(170.0, 10.0), Coord::new(170.0, 0.0)];
        across.push(Some(Geometry::Polygon(vec![vec![wide]])), vec![]);
        let d = layer(grid(&One(across, None), &serde_json::json!({"resolution": 3, "area": "Input Layer", "input": "pts"})).unwrap());
        let at = |lon: f64| a5::u64_to_hex(a5::lonlat_to_cell(a5::LonLat::new(lon, 5.0), 3).unwrap());
        for lon in [175.0, -175.0] {
            assert!((0..d.features.len()).any(|r| d.value(r, 0) == &Value::Text(at(lon))), "{lon}");
        }
        assert!(grid(&inp, &serde_json::json!({"area": "Mars"})).unwrap_err().contains("Map Extent, Input Layer or World"));
        for bad in [serde_json::json!({"resolution": 31}), serde_json::json!({"resolution": 2.5}), serde_json::json!({"resolution": -1})] {
            assert!(grid(&inp, &bad).is_err(), "{bad}");
        }
        assert!(grid(&One(points(&[]), None), &serde_json::json!({})).is_err(), "no view");
    }

    /// The estimate before covering against the exact count after: it's of the same order, so
    /// a margin of [`ESTIMATE_MARGIN`] refuses only what's well over the cap.
    #[test]
    fn the_estimate_follows_the_count() {
        for (e, res) in [
            (Extent::new(-56.5, -13.5, -55.0, -12.0), 9),
            (Extent::new(-75.0, -35.0, -35.0, 5.0), 6),
            (Extent::new(-180.0, -90.0, 180.0, 90.0), 3),
        ] {
            let shape = vec![extent_ring(e)];
            let lonlat: Vec<Vec<a5::LonLat>> = vec![shape[0].iter().map(|c| a5::LonLat::new(c.x, c.y)).collect()];
            let options = a5::PolygonToCellsOptions { containment: a5::Containment::Overlapping };
            let n = cells_of([a5::polygon_to_cells(&lonlat, res, Some(options))], res).unwrap().len() as f64;
            let guess = estimate(&shape, a5::cell_area(res));
            assert!(guess > n / ESTIMATE_MARGIN && guess < n * ESTIMATE_MARGIN, "{e:?} r{res}: {guess} for {n}");
        }
        // The whole globe's area is the authalic sphere's: every cell of a resolution.
        let world = estimate(&[extent_ring(Extent::new(-180.0, -90.0, 180.0, 90.0))], a5::cell_area(5));
        assert!(world > a5::get_num_cells(5) as f64);
    }

    /// Rings across the antimeridian run on past ±180°; those round a pole close along it.
    #[test]
    fn rings_across_the_antimeridian_and_round_the_poles() {
        // A corner that rounding left a hair short of the pole is still taken as the pole.
        let near = pole_as_line(vec![Coord::new(-10.0, 89.0), Coord::new(10.0, 89.0), Coord::new(0.0, 90.0 - 5e-8)]);
        assert_eq!(near.iter().filter(|c| c.y == 90.0).map(|c| c.x).collect::<Vec<_>>(), vec![10.0, -10.0], "{near:?}");
        for (lon, lat) in [(179.99, 10.0), (-179.99, -40.0), (0.0, 89.99), (120.0, -89.99)] {
            let c = a5::lonlat_to_cell(a5::LonLat::new(lon, lat), 3).unwrap();
            let r = ring(c).unwrap();
            assert!(r.windows(2).all(|w| (w[1].x - w[0].x).abs() <= 180.0), "{lon},{lat}: {r:?}");
            assert_eq!(r.first(), r.last());
        }
        // For the map, a cell across the antimeridian is split there: its parts lie within ±180°
        // and cover what its ring did, as a pole's do. One elsewhere stays whole.
        let shoelace = |r: &[Coord]| r.windows(2).map(|w| w[0].x * w[1].y - w[1].x * w[0].y).sum::<f64>().abs() / 2.0;
        for (lon, lat) in [(179.99, 10.0), (-179.99, -40.0), (0.0, 89.99), (120.0, -89.99)] {
            let c = a5::lonlat_to_cell(a5::LonLat::new(lon, lat), 3).unwrap();
            let p = parts(c).unwrap();
            // Across the antimeridian, two parts; round a pole, two when its ring turns past ±180°.
            assert!(p.len() == 2 || lon.abs() < 179.0, "{lon},{lat}: {p:?}");
            assert!(p.iter().flatten().flatten().all(|c| (-180.0..=180.0).contains(&c.x)), "{lon},{lat}: {p:?}");
            assert!(p.iter().all(|poly| poly[0].first() == poly[0].last()));
            let whole = shoelace(&ring(c).unwrap());
            let split: f64 = p.iter().map(|poly| shoelace(&poly[0])).sum();
            assert!((split / whole - 1.0).abs() < 1e-9, "{lon},{lat}: {split} for {whole}");
        }
        let sorriso = a5::lonlat_to_cell(a5::LonLat::new(-55.72, -12.55), 3).unwrap();
        assert_eq!(parts(sorriso).unwrap(), vec![vec![ring(sorriso).unwrap()]]);
    }

    /// Points counted by cell: the count, the density by the cell's area, and a field's sum and mean.
    #[test]
    fn points_counted_by_cell() {
        let near = [
            (-55.72, -12.55, 1.0),
            (-55.7201, -12.5501, 3.0),
            (-55.7202, -12.5499, f64::NAN),
            (10.75, 59.91, 5.0),
            // A longitude no wrap makes sense of is left out.
            (1e12, 0.0, 1.0),
        ];
        let inp = One(points(&near), None);
        let d = layer(aggregate(&inp, &serde_json::json!({"input": "pts", "resolution": 12, "field": "v"})).unwrap());
        assert_eq!(d.features.len(), 2);
        let left = aggregate(&inp, &serde_json::json!({"input": "pts", "resolution": 12})).unwrap();
        assert!(left.messages[0].contains("1 point without a usable position was left out"), "{:?}", left.messages);
        let row = (0..2).find(|r| d.value(*r, 3).as_f64() == Some(3.0)).expect("three points in Sorriso's cell");
        let area = d.value(row, 2).as_f64().unwrap();
        assert!((d.value(row, 4).as_f64().unwrap() - 3.0 / area).abs() < 1e-9);
        assert_eq!((d.value(row, 5).as_f64(), d.value(row, 6).as_f64()), (Some(4.0), Some(2.0)));
        let Value::Text(id) = d.value(row, 0) else { panic!("a hex id") };
        assert_eq!(a5::hex_to_u64(id).ok(), a5::lonlat_to_cell(a5::LonLat::new(-55.72, -12.55), 12).ok());
        // A multipoint's value counts once in its cell, however many of its points are there.
        let mut multi = points(&[(-55.72, -12.55, 1.0)]);
        multi.push(Some(Geometry::MultiPoint(vec![Coord::new(-55.72, -12.55), Coord::new(-55.7201, -12.5501)])), vec![Value::Float(5.0)]);
        let d = layer(aggregate(&One(multi, None), &serde_json::json!({"input": "pts", "resolution": 12, "field": "v"})).unwrap());
        assert_eq!((d.value(0, 3).as_f64(), d.value(0, 5).as_f64(), d.value(0, 6).as_f64()), (Some(3.0), Some(6.0), Some(3.0)));
        // Cancel is looked at between a multipoint's points, not only between features.
        let looks = |n: usize| {
            let mut d = points(&[]);
            d.push(Some(Geometry::MultiPoint(vec![Coord::new(-55.72, -12.55); n])), vec![Value::Float(1.0)]);
            let cancel = control::Control::cancelled_at_look(u64::MAX);
            let json = serde_json::json!({"system": "H3", "input": "pts", "resolution": 5});
            crate::run_under("Aggregate", &cancel, || aggregate(&One(d, None), &json)).unwrap();
            cancel.looked()
        };
        // Three paces' worth of points: three looks within the feature, where none was before.
        assert!(looks(3 * control::PACE_WORK / 16) >= looks(1) + 2);
        for bad in [
            serde_json::json!({"input": "nope"}),
            serde_json::json!({"input": "pts", "field": "nope"}),
            serde_json::json!({"input": "pts", "resolution": 40}),
        ] {
            assert!(aggregate(&inp, &bad).is_err(), "{bad}");
        }
    }

    /// Inputs with one layer, a view and a flat map projection: a plain plate carrée in metres,
    /// so the hexagon grid's plane is one the test can check by hand.
    struct Flat(Dataset, Option<Extent>);
    const M: f64 = 111_320.0;
    impl Inputs for Flat {
        fn dataset(&self, n: &str) -> Option<&Dataset> {
            (n == self.0.name).then_some(&self.0)
        }
        fn selection(&self, _: &str) -> &[usize] {
            &[]
        }
        fn view_extent(&self) -> Option<Extent> {
            self.1
        }
        fn projection_name(&self) -> Option<String> {
            Some("Plate Carrée".into())
        }
        // It reaches 175° east and west, as a projection of part of the world does, and folds
        // past it, as a transverse Mercator far from its meridian does: a finite point, but not
        // where the place is.
        fn project(&self, c: Coord) -> Option<Coord> {
            let x = if c.x.abs() <= 175.0 { c.x } else { c.x.signum() * (350.0 - c.x.abs()) };
            c.is_finite().then(|| Coord::new(x * M, c.y * M))
        }
        fn unproject(&self, m: Coord) -> Option<Coord> {
            (m.x.abs() <= 175.0 * M).then(|| Coord::new(m.x / M, m.y / M))
        }
    }

    /// A plate carrée centred on `.0` east, which wraps opposite it, as the map's projections do:
    /// a longitude is taken within half a turn of the centre, and the edge's own to the west side.
    struct Centred(f64, Dataset);
    impl Inputs for Centred {
        fn dataset(&self, n: &str) -> Option<&Dataset> {
            (n == self.1.name).then_some(&self.1)
        }
        fn selection(&self, _: &str) -> &[usize] {
            &[]
        }
        fn projection_name(&self) -> Option<String> {
            Some(format!("Plate Carrée on {}°", self.0))
        }
        fn project(&self, c: Coord) -> Option<Coord> {
            let x = (c.x - self.0 + 180.0).rem_euclid(360.0) - 180.0;
            c.is_finite().then(|| Coord::new(x * M, c.y * M))
        }
        fn unproject(&self, m: Coord) -> Option<Coord> {
            (m.x.abs() <= 180.0 * M).then(|| Coord::new(((m.x / M + self.0 + 540.0).rem_euclid(360.0)) - 180.0, m.y / M))
        }
        fn map_edge(&self) -> Option<f64> {
            Some(self.0 + 180.0)
        }
    }

    /// On a map centred off Greenwich the hexagon grid cuts an area at the map's edge, not at
    /// ±180°: an area across the edge is covered at both sides of the map, and the world is the
    /// same grid as on a map centred on 0°.
    #[test]
    fn the_hexagon_grid_cuts_at_the_maps_edge() {
        let mut d = Dataset::new("pts", GeometryType::Polygon, vec![]);
        let across =
            vec![Coord::new(-30.5, 0.0), Coord::new(-29.5, 0.0), Coord::new(-29.5, 1.0), Coord::new(-30.5, 1.0), Coord::new(-30.5, 0.0)];
        d.push(Some(Geometry::Polygon(vec![vec![across]])), vec![]);
        let p = serde_json::json!({"system": "Hexagon", "resolution": 9, "area": "Input Layer", "input": "pts"});
        let cells = layer(grid(&Centred(150.0, d), &p).unwrap());
        assert!(!cells.features.is_empty());
        // Every corner is near the area, not across the middle of the map.
        for f in &cells.features {
            let Some(Geometry::Polygon(poly)) = &f.geometry else { panic!("a polygon") };
            for c in poly.iter().flatten().flatten() {
                let from = ((c.x + 30.0 + 540.0).rem_euclid(360.0) - 180.0).abs();
                assert!(from < 1.5 && c.y > -1.0 && c.y < 2.0, "{c:?}");
            }
        }
        let world = |lon0: f64| {
            let p = serde_json::json!({"system": "Hexagon", "resolution": 2, "area": "World"});
            layer(grid(&Centred(lon0, points(&[])), &p).unwrap()).features.len()
        };
        assert_eq!(world(150.0), world(0.0));
    }

    const SYSTEMS: [(&str, i32); 4] = [("H3", 7), ("S2", 12), ("A5", 11), ("Hexagon", 13)];

    fn holds(d: &Dataset, row: usize, c: Coord) -> bool {
        use geo::Intersects;
        let Some(Geometry::Polygon(p)) = &d.features[row].geometry else { return false };
        geo_multi(p).intersects(&geo::Point::new(c.x, c.y))
    }

    /// Every system covers an area with cells of its own ids, closed and each once, and a point
    /// counted by cell falls in the cell whose outline holds it, which the grid has too.
    #[test]
    fn every_system_covers_and_counts() {
        let bogota = Extent::new(-74.2, 4.5, -74.0, 4.8);
        let pts = [(-74.08, 4.6, 1.0), (-74.15, 4.71, 2.0), (-74.03, 4.52, 3.0), (-74.19, 4.79, 4.0)];
        for (system, res) in SYSTEMS {
            let inp = One(points(&pts), Some(bogota));
            let p = serde_json::json!({"system": system, "resolution": res});
            let d = layer(grid(&inp, &p).unwrap());
            let key = System::parse(system).unwrap().field();
            assert_eq!(d.fields[0].name, key);
            assert!(d.features.len() > 4, "{system}: {}", d.features.len());
            let ids: HashSet<String> = (0..d.features.len()).map(|r| d.value(r, 0).to_string()).collect();
            assert_eq!(ids.len(), d.features.len(), "{system}: ids once each");
            for f in d.features.iter() {
                let Some(Geometry::Polygon(p)) = &f.geometry else { panic!("a polygon") };
                assert!(p.iter().all(|part| part[0].first() == part[0].last()), "{system}");
            }
            assert!((0..d.features.len()).all(|r| d.value(r, 2).as_f64().is_some_and(|a| a > 0.0)), "{system}: areas");
            let by =
                layer(aggregate(&inp, &serde_json::json!({"system": system, "resolution": res, "input": "pts", "field": "v"})).unwrap());
            let count = by.field_index("count").unwrap();
            let total: i64 = (0..by.features.len()).map(|r| by.value(r, count).as_f64().unwrap() as i64).sum();
            assert_eq!(total, 4, "{system}");
            for (x, y, _) in pts {
                let c = Coord::new(x, y);
                let row = (0..by.features.len()).find(|r| holds(&by, *r, c)).unwrap_or_else(|| panic!("{system}: {x},{y} in no cell"));
                let id = by.value(row, 0).clone();
                assert!((0..d.features.len()).any(|r| d.value(r, 0) == &id), "{system}: {id:?} not in the grid");
            }
        }
    }

    /// A point's cell holds it, everywhere: across the antimeridian, far north and south, and
    /// round the poles (the hexagon grid's Web Mercator stops at 85.05°).
    #[test]
    fn a_point_falls_in_the_cell_holding_it() {
        let places: [(f64, f64); 8] =
            [(179.9999, 10.0), (-179.9999, -40.0), (10.75, 59.91), (-74.08, 4.6), (0.0, 0.0), (120.0, -89.99), (30.0, 89.99), (45.0, 90.0)];
        for (system, res) in SYSTEMS {
            for (x, y) in places {
                let polar = y.abs() > 85.0;
                // The pole itself is a corner of the cells round it, on their outlines, not in one.
                if y.abs() >= 90.0 && system != "Hexagon" {
                    continue;
                }
                if polar && system == "Hexagon" {
                    // Web Mercator doesn't reach it: the point is left out, and said to be,
                    // never put in the edge cell its latitude would be clamped to.
                    let inp = One(points(&[(x, y, 1.0), (-74.08, 4.6, 1.0)]), None);
                    let r = aggregate(&inp, &serde_json::json!({"system": system, "resolution": res, "input": "pts"})).unwrap();
                    assert!(r.messages[0].contains("1 point without a usable position was left out"), "{x},{y}: {:?}", r.messages);
                    assert_eq!(layer(r).features.len(), 1);
                    continue;
                }
                let inp = One(points(&[(x, y, 1.0)]), None);
                let r = if polar { res.min(5) } else { res };
                let by = layer(aggregate(&inp, &serde_json::json!({"system": system, "resolution": r, "input": "pts"})).unwrap());
                assert_eq!(by.features.len(), 1, "{system} {x},{y}");
                assert!(holds(&by, 0, Coord::new(x, y)), "{system} {x},{y}: {:?}", by.features[0].geometry);
            }
        }
    }

    /// A polygon across the antimeridian is covered on both sides, and its cells drawn within ±180°.
    #[test]
    fn cells_across_the_antimeridian() {
        let mut across = Dataset::new("pts", GeometryType::Polygon, vec![]);
        let wide =
            vec![Coord::new(170.0, 0.0), Coord::new(190.0, 0.0), Coord::new(190.0, 10.0), Coord::new(170.0, 10.0), Coord::new(170.0, 0.0)];
        across.push(Some(Geometry::Polygon(vec![vec![wide]])), vec![]);
        for (system, res) in [("H3", 3), ("S2", 6), ("Hexagon", 5)] {
            let inp = One(across.clone(), None);
            let d = layer(
                grid(&inp, &serde_json::json!({"system": system, "resolution": res, "area": "Input Layer", "input": "pts"})).unwrap(),
            );
            for lon in [175.0, -175.0] {
                assert!((0..d.features.len()).any(|r| holds(&d, r, Coord::new(lon, 5.0))), "{system} {lon}");
            }
            assert!(
                d.features.iter().all(|f| match &f.geometry {
                    Some(Geometry::Polygon(p)) => p.iter().flatten().flatten().all(|c| (-180.0..=180.0).contains(&c.x)),
                    _ => false,
                }),
                "{system}"
            );
        }
    }

    /// What a covering looks at is held to the work limit, not the cell cap: a thin S2 strip
    /// across a wide box, and a hexagon U whose rows span its gap, look at between two and four
    /// times the cap's cells but take far fewer, and are covered.
    #[test]
    fn looked_at_past_the_cap_but_within_the_work_limit() {
        let res = 14;
        for (system, shape) in [("S2", "strip"), ("Hexagon", "U")] {
            let side = System::parse(system).unwrap().mean_area_m2(res).sqrt() / 111_320.0;
            let ring: Vec<Coord> = if shape == "strip" {
                // A box of about 520,000 cells, the strip its diagonal, a few cells wide.
                let (w, t) = (720.0 * side, 3.0 * side);
                vec![(0.0, 0.0), (t, 0.0), (w, w - t), (w, w), (w - t, w), (0.0, t), (0.0, 0.0)]
            } else {
                // Rows of about 930 hexagons each, 640 of them: some 600,000 candidates.
                let (w, h, t) = (1000.0 * side, 600.0 * side, 4.0 * side);
                vec![(0.0, 0.0), (w, 0.0), (w, h), (w - t, h), (w - t, t), (t, t), (t, h), (0.0, h), (0.0, 0.0)]
            }
            .into_iter()
            .map(|(x, y)| Coord::new(-74.0 + x, 1.0 + y))
            .collect();
            let mut d = Dataset::new("pts", GeometryType::Polygon, vec![]);
            d.push(Some(Geometry::Polygon(vec![vec![ring]])), vec![]);
            let p = serde_json::json!({"system": system, "resolution": res, "area": "Input Layer", "input": "pts"});
            let cells = layer(grid(&One(d, None), &p).unwrap_or_else(|e| panic!("{system}: {e}"))).features.len() as u64;
            assert!(cells > 0 && cells < MAX_CELLS / 4, "{system}: {cells}");
        }
    }

    /// A detailed outline isn't refused as work: an edge's own band of rows is linear in the
    /// input, as reading it is, so only the rows an edge crosses past it count.
    #[test]
    fn a_detailed_outline_is_within_the_work_limit() {
        let n = MAX_WORK as usize + 10_000;
        let ring: Vec<Coord> = (0..=n)
            .map(|i| {
                let a = i as f64 / n as f64 * 2.0 * PI;
                Coord::new(10.0 * a.cos(), 10.0 * a.sin())
            })
            .collect();
        let mut d = Dataset::new("pts", GeometryType::Polygon, vec![]);
        d.push(Some(Geometry::Polygon(vec![vec![ring]])), vec![]);
        let p = serde_json::json!({"system": "Hexagon", "resolution": 2, "area": "Input Layer", "input": "pts"});
        assert!(!layer(grid(&One(d, None), &p).unwrap()).features.is_empty());
        // Each cell's test grows with the outline, so Cancel is looked at by its vertices too.
        let looks = |n: usize| {
            let ring: Vec<Coord> = (0..=n)
                .map(|i| {
                    let a = i as f64 / n as f64 * 2.0 * PI;
                    Coord::new(10.0 * a.cos(), 10.0 * a.sin())
                })
                .collect();
            let mut d = Dataset::new("pts", GeometryType::Polygon, vec![]);
            d.push(Some(Geometry::Polygon(vec![vec![ring]])), vec![]);
            let p = serde_json::json!({"system": "Hexagon", "resolution": 5, "area": "Input Layer", "input": "pts"});
            let cancel = control::Control::cancelled_at_look(u64::MAX);
            crate::run_under("Grid", &cancel, || grid(&One(d, None), &p)).unwrap();
            cancel.looked()
        };
        assert!(looks(100_000) >= looks(1_000) + 2);
    }

    /// A zigzag outline whose edges each cross many rows is still held to the work limit, with
    /// few candidate cells: 10,000 edges across about 100 rows, 50 hexagons wide.
    #[test]
    fn a_zigzag_outline_is_held_to_the_work_limit() {
        let res = 14;
        let side = System::Hexagon.mean_area_m2(res).sqrt() / 111_320.0;
        let (teeth, w, h) = (5_000, 50.0 * side, 100.0 * side);
        let mut ring: Vec<Coord> = (0..=2 * teeth)
            .map(|i| Coord::new(-74.0 + i as f64 / (2 * teeth) as f64 * w, 1.0 + if i % 2 == 0 { 0.0 } else { h }))
            .collect();
        ring.extend([Coord::new(-74.0 + w, 1.0 - side), Coord::new(-74.0, 1.0 - side), Coord::new(-74.0, 1.0)]);
        let mut d = Dataset::new("pts", GeometryType::Polygon, vec![]);
        d.push(Some(Geometry::Polygon(vec![vec![ring]])), vec![]);
        let p = serde_json::json!({"system": "Hexagon", "resolution": res, "area": "Input Layer", "input": "pts"});
        let e = grid(&One(d, None), &p).unwrap_err();
        assert!(e.contains(&format!("looking at more than {MAX_WORK} cells")), "{e}");
    }

    /// The world in each system, counted against the system's own count, and the cap.
    #[test]
    fn the_world_and_the_cap() {
        let inp = One(points(&[]), Some(Extent::new(-75.0, -35.0, -35.0, 5.0)));
        let world = |system: &str, res: i32| grid(&inp, &serde_json::json!({"system": system, "resolution": res, "area": "World"}));
        assert_eq!(layer(world("H3", 0).unwrap()).features.len(), 122);
        assert_eq!(layer(world("S2", 2).unwrap()).features.len(), 96);
        let h3 = layer(world("H3", 1).unwrap());
        let sum: f64 = (0..h3.features.len()).map(|r| h3.value(r, 2).as_f64().unwrap()).sum();
        let sphere = 4.0 * PI * radius() * radius() / 1e6;
        assert!((sum / sphere - 1.0).abs() < 1e-3, "{sum} for {sphere}");
        let s2 = layer(world("S2", 3).unwrap());
        let sum: f64 = (0..s2.features.len()).map(|r| s2.value(r, 2).as_f64().unwrap()).sum();
        assert!((sum / sphere - 1.0).abs() < 1e-6, "{sum} for {sphere}");
        assert!(!layer(world("Hexagon", 1).unwrap()).features.is_empty());
        for (system, res) in [("H3", 6), ("S2", 9)] {
            assert!(world(system, res).unwrap_err().contains("too many"), "{system}");
        }
        assert!(world("Hexagon", 9).unwrap_err().contains(&format!("looking at more than {MAX_WORK} cells")));
        // Refused before any covering: far too many for the view, as cells, or (S2's descent, the
        // hexagon grid's rows) as cells to look at.
        for (system, res) in [("H3", 15), ("S2", 30), ("Hexagon", 24)] {
            let e = grid(&inp, &serde_json::json!({"system": system, "resolution": res})).unwrap_err();
            assert!(e.contains("too many") || e.contains(&format!("looking at more than {MAX_WORK} cells")), "{system}: {e}");
        }
        // What a run looks at is counted over all its polygons: one block is covered, but many
        // copies of it, each under the cap, add up past it and are refused.
        let block =
            vec![Coord::new(-74.5, 4.0), Coord::new(-73.5, 4.0), Coord::new(-73.5, 5.0), Coord::new(-74.5, 5.0), Coord::new(-74.5, 4.0)];
        for (system, res) in [("Hexagon", 13), ("S2", 14)] {
            let copies = |n: usize| {
                let mut d = Dataset::new("pts", GeometryType::Polygon, vec![]);
                for _ in 0..n {
                    d.push(Some(Geometry::Polygon(vec![vec![block.clone()]])), vec![]);
                }
                One(d, None)
            };
            let p = serde_json::json!({"system": system, "resolution": res, "area": "Input Layer", "input": "pts"});
            let one = layer(grid(&copies(1), &p).unwrap()).features.len() as u64;
            let n = (MAX_WORK / one + 2) as usize;
            let e = grid(&copies(n), &p).unwrap_err();
            // S2's polygons are summed by the estimate made before any is covered, so its run is
            // refused there; the hexagon grid's, by what its rows look at.
            let expected = if system == "S2" { "too many" } else { "looking at more than" };
            assert!(e.contains(expected), "{system}: {one} cells, {n} copies: {e}");
        }
        // Past 85.05° Web Mercator doesn't reach: the hexagon grid's area is cut there, and said to be.
        let north =
            grid(&One(points(&[]), Some(Extent::new(-10.0, 80.0, 10.0, 89.0))), &serde_json::json!({"system": "Hexagon", "resolution": 6}))
                .unwrap();
        assert!(
            north.messages[0].contains("the area beyond 85.05° north, which Web Mercator doesn't reach, was left out"),
            "{:?}",
            north.messages
        );
        let cells = layer(north);
        assert!(!cells.features.is_empty());
        assert!(cells.features.iter().all(|f| match &f.geometry {
            Some(Geometry::Polygon(p)) => p.iter().flatten().flatten().all(|c| c.y < 86.0),
            _ => false,
        }));
        // Wholly past it, the area is refused, and the side named.
        let e = grid(
            &One(points(&[]), Some(Extent::new(-10.0, -89.0, 10.0, -86.0))),
            &serde_json::json!({"system": "Hexagon", "resolution": 6}),
        )
        .unwrap_err();
        assert!(e.contains("beyond 85.05° south, which Web Mercator doesn't reach"), "{e}");
        // Every point left out: the error says why.
        let e = aggregate(&One(points(&[(0.0, 89.0, 1.0)]), None), &serde_json::json!({"system": "Hexagon", "input": "pts"})).unwrap_err();
        assert!(e.contains("no points to count: 1 point without a usable position was left out"), "{e}");
        // A hole without a usable outline is left out, and said to be: its area is covered.
        let square =
            vec![Coord::new(-74.2, 4.5), Coord::new(-74.0, 4.5), Coord::new(-74.0, 4.7), Coord::new(-74.2, 4.7), Coord::new(-74.2, 4.5)];
        let mut holed = Dataset::new("pts", GeometryType::Polygon, vec![]);
        holed.push(Some(Geometry::Polygon(vec![vec![square.clone(), vec![Coord::new(-74.1, 4.6), Coord::new(-74.05, 4.6)]]])), vec![]);
        let p = serde_json::json!({"system": "H3", "resolution": 7, "area": "Input Layer", "input": "pts"});
        let r = grid(&One(holed, None), &p).unwrap();
        assert!(
            r.messages[0].contains("1 hole without a usable outline was left out, so the area inside is covered too"),
            "{:?}",
            r.messages
        );
        let mut plain = Dataset::new("pts", GeometryType::Polygon, vec![]);
        plain.push(Some(Geometry::Polygon(vec![vec![square]])), vec![]);
        assert!(!grid(&One(plain, None), &p).unwrap().messages[0].contains("hole"));
        for bad in [
            serde_json::json!({"system": 5}),
            serde_json::json!({"system": "H3", "resolution": 16}),
            serde_json::json!({"system": "Hexagon", "resolution": 25}),
            serde_json::json!({"system": "S2", "resolution": 31}),
            serde_json::json!({"system": "S2", "resolution": -1}),
            serde_json::json!({"system": "ISEA3H"}),
        ] {
            assert!(grid(&inp, &bad).is_err(), "{bad}");
            assert!(aggregate(&One(points(&[(0.0, 0.0, 1.0)]), None), &bad).is_err(), "{bad}");
        }
    }

    /// Ids are the systems' own: H3's text, S2's token, and the hexagon grid's axial coordinates.
    #[test]
    fn ids_are_the_systems_own() {
        let inp = One(points(&[(-74.08, 4.6, 1.0)]), None);
        let at = |system: &str, res: i32| {
            let d = layer(aggregate(&inp, &serde_json::json!({"system": system, "resolution": res, "input": "pts"})).unwrap());
            d.value(0, 0).to_string()
        };
        let h3 = h3o::LatLng::new(4.6, -74.08).unwrap().to_cell(h3o::Resolution::Nine).to_string();
        assert_eq!(at("H3", 9), h3);
        let s2 = s2::cellid::CellID::from(s2::latlng::LatLng::from_degrees(4.6, -74.08)).parent(14);
        assert_eq!(at("S2", 14), s2.to_token());
        assert_eq!(s2::cellid::CellID::from_token(&at("S2", 14)), s2);
        assert_eq!(at("A5", 9), a5::u64_to_hex(a5::lonlat_to_cell(a5::LonLat::new(-74.08, 4.6), 9).unwrap()));
        // The origin's hexagon is 0,0, and its neighbours' coordinates are a step away.
        let origin = One(points(&[(0.0, 0.0, 1.0)]), None);
        let d = layer(aggregate(&origin, &serde_json::json!({"system": "Hexagon", "resolution": 10, "input": "pts"})).unwrap());
        assert_eq!(d.value(0, 0).to_string(), "0,0");
        assert_eq!(d.value(0, 3).to_string(), "Web Mercator");
        for (q, r) in [(3, -2), (-7, 11), (0, 1)] {
            let hex = Hex::at(10);
            let c = rustgis_geom::merc_to_lonlat(hex.centre(q, r));
            let one = One(points(&[(c.x, c.y, 1.0)]), None);
            let d = layer(aggregate(&one, &serde_json::json!({"system": "Hexagon", "resolution": 10, "input": "pts"})).unwrap());
            assert_eq!(d.value(0, 0).to_string(), format!("{q},{r}"));
        }
    }

    /// The hexagon grid is anchored: two areas that overlap make the same cells where they do, and
    /// it's laid on the map's plane, which the layer names, its hexagons regular there.
    #[test]
    fn the_hexagon_grid_is_anchored_on_the_maps_plane() {
        let p = serde_json::json!({"system": "Hexagon", "resolution": 12});
        let cells = |e: Extent| {
            let d = layer(grid(&Flat(points(&[]), Some(e)), &p).unwrap());
            (0..d.features.len()).map(|r| (d.value(r, 0).to_string(), format!("{:?}", d.features[r].geometry))).collect::<HashMap<_, _>>()
        };
        let (a, b) = (cells(Extent::new(-74.2, 4.5, -74.0, 4.7)), cells(Extent::new(-74.1, 4.6, -73.9, 4.8)));
        let shared: Vec<&String> = a.keys().filter(|k| b.contains_key(*k)).collect();
        assert!(!shared.is_empty());
        assert!(shared.iter().all(|k| a[*k] == b[*k]), "the same cell, the same outline");
        let d = layer(grid(&Flat(points(&[]), Some(Extent::new(-74.2, 4.5, -74.0, 4.7))), &p).unwrap());
        assert_eq!(d.value(0, 3).to_string(), "Plate Carrée");
        // On the plate carrée plane its corners are a side's length from its centre.
        let Some(Geometry::Polygon(poly)) = &d.features[0].geometry else { panic!("a polygon") };
        let ring: Vec<Coord> = poly[0][0].iter().map(|c| Coord::new(c.x * M, c.y * M)).collect();
        let n = (ring.len() - 1) as f64;
        let centre = ring.iter().take(ring.len() - 1).fold(Coord::new(0.0, 0.0), |s, c| Coord::new(s.x + c.x / n, s.y + c.y / n));
        let size = Hex::at(12).size;
        assert!(ring.iter().all(|c| ((c.x - centre.x).hypot(c.y - centre.y) / size - 1.0).abs() < 1e-6), "{ring:?}");
        // An area the plane doesn't reach is refused, and said to be; a cell whose corners it
        // can't take back is left out, and counted.
        let mut far = Dataset::new("pts", GeometryType::Polygon, vec![]);
        let wide =
            vec![Coord::new(170.0, 0.0), Coord::new(190.0, 0.0), Coord::new(190.0, 1.0), Coord::new(170.0, 1.0), Coord::new(170.0, 0.0)];
        far.push(Some(Geometry::Polygon(vec![vec![wide]])), vec![]);
        let e = grid(&Flat(far, None), &serde_json::json!({"system": "Hexagon", "resolution": 5, "area": "Input Layer", "input": "pts"}))
            .unwrap_err();
        assert!(e.contains("(Plate Carrée) doesn't reach the whole area"), "{e}");
        let edge = grid(
            &Flat(points(&[]), Some(Extent::new(172.0, 0.0, 174.99, 2.0))),
            &serde_json::json!({"system": "Hexagon", "resolution": 9}),
        )
        .unwrap();
        assert!(
            edge.messages[0].contains("cells were left out: their corners are outside what Plate Carrée reaches"),
            "{:?}",
            edge.messages
        );
        // A view whose every cell is such a cell makes no layer: it's refused, and said why.
        let e = grid(
            &Flat(points(&[]), Some(Extent::new(174.97, 0.0, 174.99, 0.02))),
            &serde_json::json!({"system": "Hexagon", "resolution": 5}),
        )
        .unwrap_err();
        assert!(
            e.contains("No cell at resolution 5 could be drawn: 1 cell was left out: its corners are outside what Plate Carrée reaches"),
            "{e}"
        );
        // Points in such a cell are left out with it, and said to be.
        let at_edge = Flat(points(&[(174.999, 1.0, 1.0), (174.998, 1.2, 1.0), (172.5, 1.0, 1.0)]), None);
        let by = aggregate(&at_edge, &serde_json::json!({"system": "Hexagon", "input": "pts", "resolution": 9})).unwrap();
        assert!(by.messages[0].contains("reaches, and so were the 2 points in them"), "{:?}", by.messages);
        assert_eq!(layer(by).features.len(), 1);
        // With every point in such a cell there's no layer: refused, and said why.
        let e = aggregate(
            &Flat(points(&[(174.999, 1.0, 1.0), (174.998, 1.2, 1.0)]), None),
            &serde_json::json!({"system": "Hexagon", "input": "pts", "resolution": 9}),
        )
        .unwrap_err();
        assert!(
            e.contains("No cell holding pts's points could be drawn: 2 cells were left out") && e.contains("so were the 2 points in them"),
            "{e}"
        );
        let kept = layer(edge);
        assert!(!kept.features.is_empty());
        assert!(kept.features.iter().all(|f| match &f.geometry {
            Some(Geometry::Polygon(p)) => p.iter().flatten().flatten().all(|c| c.x <= 175.0),
            _ => false,
        }));
    }

    /// A model or script saved before the tools took a system names none, and gets A5, as it did.
    #[test]
    fn a5_through_the_shared_tools_is_unchanged() {
        let inp = One(points(&[(-55.72, -12.55, 1.0), (10.75, 59.91, 2.0)]), Some(Extent::new(-56.5, -13.5, -55.0, -12.0)));
        let (old, a5) = (serde_json::json!({"resolution": 7}), serde_json::json!({"resolution": 7, "system": "A5"}));
        let (a, b) = (grid(&inp, &old).unwrap(), grid(&inp, &a5).unwrap());
        assert!(a.messages[0].starts_with("A5 grid: "), "{:?}", a.messages);
        assert_eq!(a.messages, b.messages);
        assert_eq!(format!("{:?}", layer(a)), format!("{:?}", layer(b)));
        let old = serde_json::json!({"resolution": 9, "input": "pts", "field": "v"});
        let a5 = serde_json::json!({"resolution": 9, "system": "A5", "input": "pts", "field": "v"});
        let (a, b) = (aggregate(&inp, &old).unwrap(), aggregate(&inp, &a5).unwrap());
        assert_eq!(a.messages, b.messages);
        assert_eq!(format!("{:?}", layer(a)), format!("{:?}", layer(b)));
    }
}
