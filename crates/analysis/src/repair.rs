//! Geometry repair in stored coordinates: a valid geometry, after the OGC Simple Features rules
//! (OGC 06-103r4, ISO 19125-1: 6.1.11.1 Polygon, 6.1.12 MultiPolygon), for one in which
//! [`validity::check`] finds problems. Repair Geometry writes the results.
//!
//! - Vertex-level problems get fixes that move no coordinate: an unclosed ring is closed,
//!   repeated vertices are dropped, and a ring with fewer than 3 distinct points (a line part with
//!   fewer than 2) is dropped, with its part's holes when it is the part's exterior ring.
//! - Polygon-level problems (a ring crossing, touching or running back over itself, a hole not
//!   inside its exterior ring, rings or parts that overlap or touch along a line, an interior split
//!   by rings touching at points) are repaired by area. Each ring stands for the area it winds
//!   around (non-zero winding: both lobes of a bowtie count, and the direction a ring is wound in
//!   doesn't matter); a part is its exterior ring's area less the union of its holes' areas, so a
//!   hole outside its exterior ring disappears and holes that overlap merge; and the feature is
//!   the union of its parts, so parts that overlap merge. geo's boolean overlay (i_overlay) does
//!   the arithmetic on an integer grid, which moves coordinates a little ([`grid_step`],
//!   [`GRID_STEPS_MOVED`]), and writes each piece of a split interior as a part of its own. A hole
//!   or part narrower than a step of that grid in any direction (up to √2 steps across a diagonal,
//!   [`narrower_than`]) may be left no area by it, so it is dropped before the overlay and counted
//!   ([`Repaired::narrow`]).
//! - A repaired geometry is checked again and written only when it is valid. A feature too busy
//!   to check or overlay within the work bound, or without the memory for it, or one the overlay
//!   fails on, is left as it was ([`Kept`]): never dropped.
//! - Lines may cross themselves (OGC doesn't require a simple line), and points have nothing to
//!   repair. Missing geometry and non-finite coordinates can't be repaired ([`Gone`]).
//!
//! In the browser a panic in the overlay can't be caught: it stops the app there, as it would in
//! the other tools that run the overlay (Clip, Dissolve, Buffer). So its input is bounded by the
//! work charge, has finite coordinates on a grid i_overlay can represent, and its memory is asked
//! of the browser before it runs. Natively a panic is caught, and the feature is left as it was.

use crate::validity::{self, At, Checked, Code, Limit, Problem, Room};
use geo::algorithm::bool_ops::{BooleanOps, FillRule, OpType};
use geo::orient::{Direction, Orient};
use geo::{Area, Euclidean, Length};
use rustgis_data::{Coord, Geometry};

/// Work per vertex the overlay takes in or makes, against the same bound as a check
/// ([`validity::MAX_WORK`]): at 25, up to 2 million vertices a feature, as Buffer allows.
const OVERLAY_COST: u64 = 25;
/// The memory the overlay holds per vertex, as estimated: geo's and i_overlay's copies of the
/// rings in floats and on the grid, their segments and sort, the overlay's graph and its output.
const OVERLAY_BYTES: u64 = 160;
/// How far a coordinate may move through the overlays of one repair, in steps of the grid: a
/// vertex is rounded to the grid in each of up to three overlays (a ring's own area, a part's holes
/// taken out, the union of the parts), each time by at most half a step on each axis, and a vertex
/// the overlay adds where rings cross lies on the rounded segments, so within as much again of the
/// input's boundary. Measured in the tests, the largest move is well under one step.
pub(crate) const GRID_STEPS_MOVED: f64 = 5.0;

/// Why a feature is left as it was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kept {
    /// Checking or overlaying it needs more work than the bound.
    Work,
    /// The memory for it couldn't be had.
    Memory,
    /// geo failed on it: its relate when checking, or the overlay (a panic, natively;
    /// coordinates on no grid it can represent; or a result that isn't valid).
    Failed,
}

impl Kept {
    /// The code written for it (data: never translated).
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Kept::Work | Kept::Memory => Code::TooComplex.as_str(),
            Kept::Failed => "repair_failed",
        }
    }
}

/// Why a feature can't be repaired: it is written without geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Gone {
    Null,
    NonFinite,
    /// Every part has too few distinct points.
    TooFewPoints,
    /// Nothing with an area is left once repaired: rings that run back over themselves, or holes
    /// covering their whole part.
    NoArea,
}

impl Gone {
    pub(crate) const ALL: [Gone; 4] = [Gone::Null, Gone::NonFinite, Gone::TooFewPoints, Gone::NoArea];

    /// The code written for it (data: never translated).
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Gone::Null => Code::NullGeometry.as_str(),
            Gone::NonFinite => Code::NonFiniteCoordinate.as_str(),
            Gone::TooFewPoints => Code::TooFewPoints.as_str(),
            Gone::NoArea => "no_area",
        }
    }
}

/// A repaired geometry, and what was done to it.
#[derive(Debug, PartialEq)]
pub(crate) struct Repaired {
    pub geometry: Geometry,
    /// What was wrong with it, each once, in [`Code`] order.
    pub codes: Vec<Code>,
    /// Rings and parts left out: too few distinct points, or no area.
    pub dropped: usize,
    /// Holes and parts left out of the overlay as narrower than a step of its grid in any
    /// direction, where they would have no area ([`narrower_than`]); written as `no_area` after
    /// the codes.
    pub narrow: usize,
    /// When the overlay rebuilt it, how far its coordinates may have moved, in their units (degrees
    /// for a layer in longitude/latitude, a projected system's own units for one held in it).
    pub moved: Option<f64>,
    /// Under [`Rule::EvenOdd`], rings dropped as duplicates before the rings were combined, since a
    /// pair would cancel out: a hole repeated in its part, and every ring of a part repeated whole
    /// (the same corners, [`normal_form`]).
    pub duplicates: usize,
}

/// What [`repair`] made of one geometry.
#[derive(Debug, PartialEq)]
pub(crate) enum Outcome {
    /// Nothing is wrong: written as it is.
    Valid,
    Repaired(Repaired),
    Gone(Gone),
    /// No area left ([`Gone::NoArea`]) once `duplicates` duplicate rings were left out
    /// ([`Rule::EvenOdd`]), which the tool still counts.
    GoneAfterDuplicates {
        duplicates: usize,
    },
    Kept(Kept),
}

/// How the rebuild by area decides what is inside a polygon whose rings cross, overlap or sit in
/// the wrong place (#2003). Both give a valid geometry; they differ in what the rings are taken to
/// mean, and only for such polygons: one valid as it is, or fixed without moving a coordinate, is
/// written the same either way.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Rule {
    /// Each part is its exterior ring's area less the union of its holes', and the feature is the
    /// union of its parts: the rings' roles as OGC gives them (GEOS's "structure"). Parts that
    /// overlap merge, a hole outside its exterior ring disappears, and a ring listed as a part of
    /// its own is filled even when it lies inside another part.
    #[default]
    ShellsMinusHoles,
    /// Each ring's own area as [`Rule::ShellsMinusHoles`] reads it (its loops by winding, so a
    /// ring traced twice or a bowtie keeps its area), then the rings combined by parity whatever
    /// their roles, first within each part and then the parts together: what lies inside an odd
    /// number of rings is in, close to GEOS's "linework" method. Where two parts overlap is left
    /// out, where two holes of a part overlap is area again, a ring listed as a part inside another
    /// is a hole, and what of a hole lies outside its exterior ring is a part. A hole repeated in
    /// its part and a part repeated whole are dropped first, since a pair would cancel out
    /// ([`Repaired::duplicates`]); linework would cancel them.
    EvenOdd,
}

/// Each rule by the name Repair Geometry's `rule` parameter gives it, in the order it offers them.
const NAMED: &[(&str, Rule)] = &[("Shells Minus Holes", Rule::ShellsMinusHoles), ("Even-Odd", Rule::EvenOdd)];
/// The names of [`NAMED`], in its order, as the parameter's choices.
pub(crate) const RULES: [&str; NAMED.len()] = {
    let mut names = [""; NAMED.len()];
    let mut i = 0;
    while i < NAMED.len() {
        names[i] = NAMED[i].0;
        i += 1;
    }
    names
};

impl Rule {
    /// The rule a tool parameter names, as [`NAMED`] spells it, in any case.
    pub(crate) fn named(name: &str) -> Option<Rule> {
        NAMED.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, rule)| *rule)
    }
}

/// The memory, per vertex, that [`Rule::EvenOdd`] holds besides the overlay's
/// ([`OVERLAY_BYTES`]), at most, while [`distinct_rings`] finds duplicates: each ring's normal form
/// kept (16), and while one is made its two readings (32), Booth's failure table (16), a rotation
/// (16) and the least so far (16; the table and the rotation are never held together); and, held
/// with them, a ring's place in `keep` (16), a part's holes listed (24 a ring), with their corner
/// counts (32), the ids kept (8) and their keys (32, up to twice that as a growing list reserves),
/// a ring at least 4 vertices. At most about 116 a vertex for large rings, about 48 for many small
/// ones; 128 leaves room.
const EVEN_ODD_BYTES: u64 = 128;

/// Repairs one geometry under `rule`, spending at most [`validity::MAX_WORK`] on each check and on
/// the overlay, and asking `has_room` whether the browser can give the overlay's memory before it
/// runs (see `web_has_room` in the tools).
pub(crate) fn repair_by(g: Option<&Geometry>, rule: Rule, has_room: &dyn Fn(u64) -> bool) -> Outcome {
    repair_by_with(g, rule, validity::MAX_WORK, &|_| true, has_room)
}

/// [`repair_by`] with a work `budget`, asking `allow` before each reservation of a buffer whether to
/// try it, as [`validity::check_with`] does: tests refuse some to stand in for running out of
/// memory.
pub(crate) fn repair_by_with(
    g: Option<&Geometry>,
    rule: Rule,
    budget: u64,
    allow: &dyn Fn(usize) -> bool,
    has_room: &dyn Fn(u64) -> bool,
) -> Outcome {
    let first = validity::check_with(g, budget, allow);
    if first.problems.is_empty() {
        return Outcome::Valid;
    }
    let Some(g) = g else { return Outcome::Gone(Gone::Null) };
    if let Some(gone) = gone(&first) {
        return Outcome::Gone(gone);
    }
    if let Some(kept) = kept(&first) {
        return Outcome::Kept(kept);
    }
    let room = Room(allow);
    match g {
        Geometry::Line(parts) => match lines(parts, room) {
            Err(limit) => Outcome::Kept(kept_for(limit)),
            Ok((parts, _)) if parts.is_empty() => Outcome::Gone(Gone::TooFewPoints),
            Ok((parts, dropped)) => verified(
                Repaired { geometry: Geometry::Line(parts), codes: first.codes(), dropped, narrow: 0, moved: None, duplicates: 0 },
                budget,
                allow,
            ),
        },
        Geometry::Polygon(polys) => polygon(polys, &first, rule, budget, allow, has_room),
        // A point's only problems, a missing or non-finite coordinate, are handled above.
        Geometry::Point(_) | Geometry::MultiPoint(_) => Outcome::Kept(Kept::Failed),
    }
}

/// Why a checked feature can't be repaired, if it can't.
fn gone(c: &Checked) -> Option<Gone> {
    c.problems.iter().find_map(|p| match p {
        Problem::Null { .. } => Some(Gone::Null),
        Problem::NonFinite { .. } => Some(Gone::NonFinite),
        _ => None,
    })
}

/// Why a feature that wasn't checked fully is left as it was, if it wasn't.
fn kept(c: &Checked) -> Option<Kept> {
    c.problems.iter().find_map(|p| match p {
        Problem::TooComplex { limit } => Some(kept_for(*limit)),
        _ => None,
    })
}

fn kept_for(limit: Limit) -> Kept {
    match limit {
        Limit::Work => Kept::Work,
        Limit::Memory => Kept::Memory,
        Limit::Relate => Kept::Failed,
    }
}

/// `r`, once a check finds nothing wrong with its geometry; left as it was otherwise.
fn verified(r: Repaired, budget: u64, allow: &dyn Fn(usize) -> bool) -> Outcome {
    let c = validity::check_with(Some(&r.geometry), budget, allow);
    if let Some(k) = kept(&c) {
        return Outcome::Kept(k);
    }
    if c.problems.is_empty() { Outcome::Repaired(r) } else { Outcome::Kept(Kept::Failed) }
}

/// `pts` without repeated consecutive vertices, closed when `ring`, in a vector reserved fallibly.
fn cleaned(pts: &[Coord], ring: bool, room: Room) -> Result<Vec<Coord>, Limit> {
    let mut v: Vec<Coord> = room.vec(pts.len().saturating_add(usize::from(ring)))?;
    for c in pts {
        if v.last() != Some(c) {
            v.push(*c);
        }
    }
    if ring
        && let (Some(first), Some(last)) = (v.first().copied(), v.last())
        && first != *last
    {
        v.push(first);
    }
    Ok(v)
}

/// A line's parts without repeated vertices, and without the parts that have fewer than 2
/// distinct points; how many were dropped.
fn lines(parts: &[Vec<Coord>], room: Room) -> Result<(Vec<Vec<Coord>>, usize), Limit> {
    let mut out = room.vec(parts.len())?;
    let mut dropped = 0usize;
    for part in parts {
        let part = cleaned(part, false, room)?;
        // Consecutive vertices differ now: two of them are two distinct points.
        if part.len() < 2 {
            dropped += 1;
        } else {
            out.push(part);
        }
    }
    Ok((out, dropped))
}

/// A polygon's rings closed and without repeated vertices, and without the rings that have fewer
/// than 3 distinct points (an exterior ring's part goes with it, holes and all), nor parts without
/// rings; how many rings and parts were dropped, and whether anything changed.
fn rings(polys: &[Vec<Vec<Coord>>], room: Room) -> Result<(Vec<Vec<Vec<Coord>>>, usize, bool), Limit> {
    let mut out = room.vec(polys.len())?;
    let (mut dropped, mut changed) = (0usize, false);
    for part in polys {
        let mut kept: Vec<Vec<Coord>> = room.vec(part.len())?;
        for (r, ring) in part.iter().enumerate() {
            let fixed = cleaned(ring, true, room)?;
            if validity::distinct_up_to(&fixed, 3) < 3 {
                dropped += 1;
                changed = true;
                if r == 0 {
                    break;
                }
                continue;
            }
            changed |= fixed != *ring;
            kept.push(fixed);
        }
        if part.is_empty() {
            dropped += 1;
            changed = true;
        }
        // An exterior ring dropped leaves nothing: its holes went with it.
        if !kept.is_empty() {
            out.push(kept);
        }
    }
    Ok((out, dropped, changed))
}

/// A polygon feature: the fixes that move no coordinate, then, when the polygon-level checks
/// still find something, the rebuild by area.
fn polygon(
    polys: &[Vec<Vec<Coord>>],
    first: &Checked,
    rule: Rule,
    budget: u64,
    allow: &dyn Fn(usize) -> bool,
    has_room: &dyn Fn(u64) -> bool,
) -> Outcome {
    let room = Room(allow);
    let (fixed, dropped, changed) = match rings(polys, room) {
        Ok(fixed) => fixed,
        Err(limit) => return Outcome::Kept(kept_for(limit)),
    };
    if fixed.is_empty() {
        return Outcome::Gone(Gone::TooFewPoints);
    }
    let fixed = Geometry::Polygon(fixed);
    // Checked again when something changed: rings and parts left unrelated by a ring's problem
    // (see `Checked::unrelated`) are related now.
    let rechecked;
    let second = if changed {
        rechecked = validity::check_with(Some(&fixed), budget, allow);
        &rechecked
    } else {
        first
    };
    if let Some(kept) = kept(second) {
        return Outcome::Kept(kept);
    }
    let mut codes = first.codes();
    for code in second.codes() {
        if !codes.contains(&code) {
            codes.push(code);
        }
    }
    codes.sort_unstable();
    if second.problems.is_empty() {
        return Outcome::Repaired(Repaired { geometry: fixed, codes, dropped, narrow: 0, moved: None, duplicates: 0 });
    }
    let Geometry::Polygon(parts) = &fixed else { return Outcome::Kept(Kept::Failed) };
    if gone(second).is_some() {
        return Outcome::Kept(Kept::Failed);
    }

    let coords = parts.iter().flatten().map(Vec::len).fold(0usize, usize::saturating_add);
    let size = u64::try_from(coords).unwrap_or(u64::MAX).saturating_add(second.contacts.saturating_mul(2));
    if size.saturating_mul(OVERLAY_COST) > budget {
        return Outcome::Kept(Kept::Work);
    }
    // Even-Odd's duplicate keys aren't charged for one part with one hole or none, which can have
    // no duplicate ([`distinct_rings`] makes none). Otherwise they are, an upper bound: rings and
    // parts of sizes no other shares get no key either.
    let keyed = !matches!(parts.as_slice(), [rings] if rings.len() < 3);
    let per_vertex = if rule == Rule::EvenOdd && keyed { OVERLAY_BYTES + EVEN_ODD_BYTES } else { OVERLAY_BYTES };
    if !has_room(size.saturating_mul(per_vertex)) {
        return Outcome::Kept(Kept::Memory);
    }
    let Some(step) = grid_step(parts.iter().flatten().flatten().map(|c| (c.x, c.y))) else { return Outcome::Kept(Kept::Failed) };
    // The rings crossing themselves, by part and ring: in order, as the problems are.
    let crossing = second.problems.iter().filter_map(|p| match p {
        Problem::SelfIntersection { at: At::Ring(p, r), .. } => Some((*p, *r)),
        _ => None,
    });
    let crossing = match room.collect(crossing.clone().count(), crossing) {
        Ok(c) => c,
        Err(limit) => return Outcome::Kept(kept_for(limit)),
    };
    let rebuilt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match rule {
        Rule::ShellsMinusHoles => by_area(parts, &crossing, room),
        Rule::EvenOdd => by_parity(parts, &crossing, room),
    }));
    let Rebuilt { polys, no_area, narrow, found, duplicates } = match rebuilt {
        Ok(Ok(rebuilt)) => rebuilt,
        Ok(Err(kept)) => return Outcome::Kept(kept),
        Err(_) => return Outcome::Kept(Kept::Failed),
    };
    if polys.is_empty() {
        // The duplicates left out on the way are still said.
        return if duplicates > 0 { Outcome::GoneAfterDuplicates { duplicates } } else { Outcome::Gone(Gone::NoArea) };
    }
    for code in found {
        if !codes.contains(&code) {
            codes.push(code);
        }
    }
    codes.sort_unstable();
    let moved = Some(step * GRID_STEPS_MOVED);
    verified(
        Repaired { geometry: Geometry::Polygon(polys), codes, dropped: dropped.saturating_add(no_area), narrow, moved, duplicates },
        budget,
        allow,
    )
}

/// The step of the integer grid i_overlay rounds the points `pts` to: it takes half the larger
/// side of their extent, `h`, to about 2^29 grid units (2^(29 - round(log2 h)) units a unit of
/// the coordinates). `None` when there is no such grid: an extent that is empty, not finite, or
/// so large or so small that the scale or the step isn't a finite number.
pub(crate) fn grid_step(pts: impl Iterator<Item = (f64, f64)>) -> Option<f64> {
    let (mut lo_x, mut lo_y, mut hi_x, mut hi_y) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    for (x, y) in pts {
        (lo_x, lo_y, hi_x, hi_y) = (lo_x.min(x), lo_y.min(y), hi_x.max(x), hi_y.max(y));
    }
    let half = (hi_x - lo_x).max(hi_y - lo_y) * 0.5;
    if !(half.is_finite() && half > 0.0) {
        return None;
    }
    let e = 29.0 - half.log2().round();
    let (scale, step) = (e.exp2(), (-e).exp2());
    (scale.is_normal() && step.is_normal()).then_some(step)
}

/// The step of the grid the overlay rounds `rings` to ([`grid_step`]); an error when there is
/// none, and the overlay can't run on them.
fn on_grid<'a>(rings: impl Iterator<Item = &'a geo::LineString<f64>>) -> Result<f64, Kept> {
    grid_step(rings.flat_map(|r| r.0.iter().map(|c| (c.x, c.y)))).ok_or(Kept::Failed)
}

/// Every ring of `polys`, as the overlay takes them in.
fn rings_of(polys: &[geo::Polygon<f64>]) -> impl Iterator<Item = &geo::LineString<f64>> {
    polys.iter().flat_map(|p| std::iter::once(p.exterior()).chain(p.interiors()))
}

/// Whether `poly` is narrower than `step`, the step of the overlay's grid, in some direction:
/// rounded to that grid, its points may then all fall on one line, which leaves it no area.
/// Whether they do depends on where it lies between the grid's lines, so such a polygon is dropped
/// before the overlay, and counted, rather than left to the overlay to drop unsaid.
///
/// Rounding moves a point by at most half a step on each axis, so across a line with unit normal
/// `(nx, ny)` by at most `(|nx| + |ny|) / 2` steps. Points rounded onto one such line were within
/// `|nx| + |ny|` steps of each other across it: one step across the axes, up to √2 steps at 45°. So
/// `poly` is narrower when, in some direction, its width is less than `|nx| + |ny|` steps. Between
/// the axes and the normals of its convex hull's edges, width and allowance are each a sinusoid of
/// the direction, and their ratio is monotonic, so the least ratio is found in one of those
/// directions: the axes, by the extent, and each hull edge, by the hull vertex farthest from its
/// line, which rotating calipers find in one turn around the hull (M. I. Shamos, "Computational
/// Geometry", PhD thesis, Yale University, 1978; G. T. Toussaint, "Solving geometric problems with
/// the rotating calipers", Proc. IEEE MELECON '83, 1983). Only the exterior ring makes the hull.
///
/// Only thin polygons pay for the hull. A polygon of least width `w` lies in a strip `w` wide and
/// as long as its diameter, at most the diagonal of its extent, so its area is at most `w` times
/// that diagonal; its rings here are simple (the overlay's, or ones the check found not to cross
/// themselves), so the exterior ring's area is the area it covers. An area of twice the step times
/// the diagonal or more means a width of more than √2 steps, wide enough in every direction (twice,
/// so that the rounding of the area's sum can't tip it). The hull sorts the vertices the overlay
/// is charged for ([`OVERLAY_COST`]), in buffers reserved fallibly.
fn narrower_than(poly: &geo::Polygon<f64>, step: f64, room: Room) -> Result<bool, Limit> {
    let ring = &poly.exterior().0;
    let (mut lo_x, mut lo_y, mut hi_x, mut hi_y) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    for c in ring {
        (lo_x, lo_y, hi_x, hi_y) = (lo_x.min(c.x), lo_y.min(c.y), hi_x.max(c.x), hi_y.max(c.y));
    }
    let (wide, high) = (hi_x - lo_x, hi_y - lo_y);
    if wide.min(high) < step {
        return Ok(true);
    }
    // Twice the area, from the first vertex, so that far from the origin no precision is lost.
    let Some(o) = ring.first() else { return Ok(true) };
    let twice = ring.windows(2).fold(0.0, |s, w| match w {
        [a, b] => s + (a.x - o.x) * (b.y - o.y) - (b.x - o.x) * (a.y - o.y),
        _ => s,
    });
    if twice.abs() >= 4.0 * step * wide.hypot(high) {
        return Ok(false);
    }
    let hull = hull(ring, room)?;
    let n = hull.len();
    if n < 3 {
        // All on one line.
        return Ok(true);
    }
    // Twice the area of the triangle of edge `i` and vertex `k`: the vertex's distance from the
    // edge's line times the edge's length; positive inside, as the hull turns anticlockwise.
    let across = |i: usize, k: usize| -> Option<f64> {
        let (a, b, p) = (hull.get(i % n)?, hull.get((i + 1) % n)?, hull.get(k % n)?);
        Some((b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x))
    };
    // The calipers turn once around the hull; the bound on their steps only guards the loop.
    let (mut far, mut turns) = (1usize, 0usize);
    for i in 0..n {
        while turns < n.saturating_mul(3) {
            match (across(i, far), across(i, far + 1)) {
                (Some(here), Some(next)) if next > here => {
                    far = (far + 1) % n;
                    turns += 1;
                }
                _ => break,
            }
        }
        let (Some(a), Some(b), Some(d)) = (hull.get(i), hull.get((i + 1) % n), across(i, far)) else { return Ok(false) };
        // The farthest vertex's distance, `d / |e|`, against the allowance `(|ex| + |ey|) / |e|` steps.
        if d < step * ((b.x - a.x).abs() + (b.y - a.y).abs()) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The convex hull of `pts`, anticlockwise, without collinear points or a closing repeat, by
/// Andrew's monotone chain (A. M. Andrew, "Another efficient algorithm for convex hulls in two
/// dimensions", Information Processing Letters 9(5), 1979), in buffers reserved fallibly.
fn hull(pts: &[geo::Coord<f64>], room: Room) -> Result<Vec<geo::Coord<f64>>, Limit> {
    let mut pts = room.collect(pts.len(), pts.iter().copied())?;
    pts.sort_unstable_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    pts.dedup();
    let turns_left = |h: &[geo::Coord<f64>], p: geo::Coord<f64>| match h {
        [.., a, b] => (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x) > 0.0,
        _ => true,
    };
    let mut hull = room.vec(pts.len().saturating_add(1))?;
    // The lower chain, left to right, then the upper, right to left, on top of it.
    for &p in &pts {
        while !turns_left(&hull, p) {
            hull.pop();
        }
        room.grow(&mut hull)?;
        hull.push(p);
    }
    let lower = hull.len();
    for &p in pts.iter().rev().skip(1) {
        while hull.len() > lower && !turns_left(&hull, p) {
            hull.pop();
        }
        room.grow(&mut hull)?;
        hull.push(p);
    }
    // The upper chain ends where the lower began.
    if hull.len() > 1 {
        hull.pop();
    }
    Ok(hull)
}

/// `polys` without those narrower than `step` ([`narrower_than`]), adding how many were left out
/// to `narrow`.
fn wider(mut polys: Vec<geo::Polygon<f64>>, step: f64, narrow: &mut usize, room: Room) -> Result<Vec<geo::Polygon<f64>>, Kept> {
    let before = polys.len();
    let mut failed = None;
    polys.retain(|p| match narrower_than(p, step, room) {
        Ok(thin) => !thin,
        Err(limit) => {
            failed.get_or_insert(limit);
            true
        }
    });
    if let Some(limit) = failed {
        return Err(kept_for(limit));
    }
    *narrow = narrow.saturating_add(before - polys.len());
    Ok(polys)
}

/// What the rebuild by area found that the checks may have missed: a part with a ring crossing
/// itself isn't related to its other rings or to other parts (see `Checked::unrelated`).
type Found = Vec<Code>;

/// What [`by_area`] or [`by_parity`] made of a feature's parts.
struct Rebuilt {
    /// The rings of the result.
    polys: Vec<Vec<Vec<Coord>>>,
    /// Rings and parts left without area.
    no_area: usize,
    /// Holes and parts dropped as narrower than a step of the overlay's grid in any direction.
    narrow: usize,
    found: Found,
    /// Rings dropped first as duplicates, a hole repeated in its part or every ring of a part
    /// repeated whole ([`Rule::EvenOdd`] only).
    duplicates: usize,
}

/// The area `ring` stands for, as both rules read one ring: as it is when simple (it isn't in
/// `crossing`, the rings that cross, touch or run back over themselves, by part and ring, sorted),
/// otherwise its loops by non-zero winding, so both lobes of a bowtie count and a ring traced twice
/// counts once.
fn ring_area(ring: &[Coord], at: (usize, usize), crossing: &[(usize, usize)], room: Room) -> Result<geo::MultiPolygon<f64>, Kept> {
    let ring = geo::LineString(room.collect(ring.len(), ring.iter().map(|c| geo::Coord { x: c.x, y: c.y })).map_err(kept_for)?);
    let poly = geo::Polygon::new(ring, vec![]);
    if crossing.binary_search(&at).is_err() {
        // A simple ring: its area as it is, wound as the overlay expects.
        return Ok(geo::MultiPolygon::new(vec![poly.orient(Direction::Default)]));
    }
    on_grid(std::iter::once(poly.exterior()))?;
    Ok(poly.boolean_op_with_fill_rule(&geo::MultiPolygon::new(vec![]), OpType::Union, FillRule::NonZero))
}

/// The rebuild under [`Rule::EvenOdd`] of `parts` (closed rings, each with 3 distinct points or
/// more; `crossing` as in [`ring_area`]), built as [`by_area`] builds its own, part by part and then
/// the parts together, with parity in place of shells minus holes:
/// - duplicates (the same corners, [`normal_form`]) left out first ([`distinct_rings`]): a hole repeated in its part, and a part
///   repeated whole, which parity would otherwise cancel out;
/// - each ring's own area ([`ring_area`]); a part whose exterior ring has none is left out with its
///   holes, as [`by_area`] leaves it;
/// - a part's holes narrower than a step of the overlay's grid dropped, then the part's rings
///   combined by parity, so its holes' overlaps come back and a hole partly outside its ring adds
///   that part as area;
/// - the parts, the narrow ones dropped, combined by parity, so where parts overlap is left out and
///   parts meeting along a line are joined.
///
/// Besides the checks' codes it finds those the checks can't tell of a part with a ring crossing
/// itself ([`unrelated_hole_codes`]), and [`Code::PartsOverlap`] when the parts' combination makes fewer polygons than it was given (parts joined)
/// or less area than they had, by more than rounding to the grid could take: it moves each edge by
/// at most half a step on each axis, so changes the area by less than the rings' length times a
/// step.
///
/// It has no Cancel check of its own: its passes over the rings are bounded by the work budget
/// `polygon` checks before calling it, and the tool looks at Cancel between features.
fn by_parity(parts: &[Vec<Vec<Coord>>], crossing: &[(usize, usize)], room: Room) -> Result<Rebuilt, Kept> {
    let empty = geo::MultiPolygon::<f64>::new(vec![]);
    let Distinct { keep, duplicates } = distinct_rings(parts, room).map_err(kept_for)?;
    let mut all: Vec<geo::Polygon<f64>> = Vec::new();
    let mut no_area = 0usize;
    let mut narrow = 0usize;
    let mut found = Found::new();
    // `keep` is by part, then ring, and keeps every exterior ring of a part it keeps.
    for kept in keep.chunk_by(|a, b| a.0 == b.0) {
        let Some(&(p, 0)) = kept.first() else { continue };
        let Some(rings) = parts.get(p) else { continue };
        let Some(shell) = rings.first() else { continue };
        let shell = ring_area(shell, (p, 0), crossing, room)?;
        if shell.0.is_empty() {
            // Its holes go with it.
            no_area += 1;
            continue;
        }
        let mut cut: Vec<geo::Polygon<f64>> = Vec::new();
        for &(_, r) in kept.get(1..).unwrap_or_default() {
            let Some(hole) = rings.get(r) else { continue };
            let hole = ring_area(hole, (p, r), crossing, room)?;
            if hole.0.is_empty() {
                no_area += 1;
            }
            for piece in hole.0 {
                room.grow(&mut cut).map_err(kept_for)?;
                cut.push(piece);
            }
        }
        if !cut.is_empty() {
            let step = on_grid(rings_of(&shell.0).chain(rings_of(&cut)))?;
            cut = wider(cut, step, &mut narrow, room)?;
            // What is left is on that grid too; checked again all the same, since a panic in the
            // overlay can't be caught in the browser.
            on_grid(rings_of(&shell.0).chain(rings_of(&cut)))?;
        }
        let part = if cut.is_empty() {
            shell
        } else {
            let cut = geo::MultiPolygon::new(cut);
            let related = (0..rings.len()).all(|r| crossing.binary_search(&(p, r)).is_err());
            if !related {
                unrelated_hole_codes(&shell, &cut, &mut found);
            }
            let mut pieces: Vec<geo::Polygon<f64>> = room.vec(shell.0.len().saturating_add(cut.0.len())).map_err(kept_for)?;
            pieces.extend(shell.0);
            pieces.extend(cut.0);
            geo::MultiPolygon::new(pieces).boolean_op_with_fill_rule(&empty, OpType::Union, FillRule::EvenOdd)
        };
        if part.0.is_empty() {
            no_area += 1;
        }
        for piece in part.0 {
            room.grow(&mut all).map_err(kept_for)?;
            all.push(piece);
        }
    }
    let combined = if parts.len() > 1 && all.len() > 1 {
        // Parts narrower than a step of the grid, in any direction, are dropped first; finding the
        // step checks the grid can take every vertex, which holds for those left.
        let step = on_grid(rings_of(&all))?;
        let all = wider(all, step, &mut narrow, room)?;
        // Every part narrower than a step (two thin rectangles overlapping): none left, so no area,
        // as a part with none is, not a grid with no coordinates to check.
        if all.is_empty() {
            return Ok(Rebuilt { polys: Vec::new(), no_area, narrow, found, duplicates });
        }
        // The overlay rounds the parts left to their own grid, finer where a distant sliver was
        // dropped: that step, not the first, bounds what rounding can take from their area.
        let step = on_grid(rings_of(&all))?;
        let given = all.len();
        let each: f64 = all.iter().map(Area::unsigned_area).sum();
        let length: f64 = rings_of(&all).map(|r| Euclidean.length(r)).sum();
        let combined = geo::MultiPolygon::new(all).boolean_op_with_fill_rule(&empty, OpType::Union, FillRule::EvenOdd);
        if combined.0.len() < given || each - combined.unsigned_area() > length * step {
            found.push(Code::PartsOverlap);
        }
        combined
    } else {
        geo::MultiPolygon::new(all)
    };
    Ok(Rebuilt { polys: polygons(&combined, room).map_err(kept_for)?, no_area, narrow, found, duplicates })
}

/// What the checks don't tell of the holes of a part with a ring crossing itself, which they
/// can't relate to the rest: [`Code::HoleOutsideShell`] when some of `cut`, its holes' areas, lies
/// outside `shell`, its exterior ring's, and [`Code::RingsIntersect`] when holes overlap or touch
/// along a line, which their union tells by making fewer polygons than it was given.
fn unrelated_hole_codes(shell: &geo::MultiPolygon<f64>, cut: &geo::MultiPolygon<f64>, found: &mut Found) {
    if !found.contains(&Code::HoleOutsideShell) && !cut.boolean_op_with_fill_rule(shell, OpType::Difference, FillRule::NonZero).0.is_empty()
    {
        found.push(Code::HoleOutsideShell);
    }
    if cut.0.len() > 1
        && !found.contains(&Code::RingsIntersect)
        && on_grid(rings_of(&cut.0)).is_ok()
        && cut.boolean_op_with_fill_rule(&geo::MultiPolygon::new(vec![]), OpType::Union, FillRule::NonZero).0.len() < cut.0.len()
    {
        found.push(Code::RingsIntersect);
    }
}

/// What [`distinct_rings`] keeps of a polygon's rings.
struct Distinct {
    /// The rings to combine, by part and then ring.
    keep: Vec<(usize, usize)>,
    /// How many rings were left out as duplicates.
    duplicates: usize,
}

/// The rings of `parts` to combine under [`Rule::EvenOdd`], without duplicates whose
/// canceling out no one means: a hole repeated in its own part, and a part repeated whole (its
/// exterior ring and its holes, after the first). A hole repeated in another part is kept: each of
/// the parts has it, and leaving one out would fill it. The same ring is the same corners in the
/// same cyclic order, from any vertex and in either direction, a vertex in the middle of a straight
/// edge aside ([`normal_form`]): a copy with one more such vertex would cancel its twin out as
/// surely. Only rings, and parts, of as many corners are compared, so a polygon that can have no
/// duplicate costs no normal form. In buffers reserved fallibly.
fn distinct_rings(parts: &[Vec<Vec<Coord>>], room: Room) -> Result<Distinct, Limit> {
    let n = parts.iter().map(Vec::len).fold(0usize, usize::saturating_add);
    let mut keep: Vec<(usize, usize)> = room.vec(n)?;
    // One part with one hole or none can have no duplicate.
    if let [rings] = parts
        && rings.len() < 3
    {
        keep.extend((0..rings.len()).map(|r| (0, r)));
        return Ok(Distinct { keep, duplicates: 0 });
    }
    let mut duplicates = 0usize;
    for (p, rings) in parts.iter().enumerate() {
        let Some((_, rest)) = rings.split_first() else { continue };
        keep.push((p, 0));
        let mut holes = room.collect(rest.len(), rest.iter().enumerate().map(|(h, ring)| (h + 1, ring.as_slice())))?;
        let (kept, left_out) = first_of_each(&mut holes, room)?;
        duplicates += left_out;
        keep.extend(kept.into_iter().map(|r| (p, r)));
    }
    // Parts that may be the same: the same number of rings kept, of the same number of points.
    let mut sizes: Vec<(usize, usize, usize)> = room.vec(parts.len())?;
    for kept in keep.chunk_by(|a, b| a.0 == b.0) {
        let Some(&(p, _)) = kept.first() else { continue };
        let points = kept.iter().filter_map(|&(p, r)| parts.get(p)?.get(r)).map(|r| corner_count(r)).fold(0usize, usize::saturating_add);
        sizes.push((kept.len(), points, p));
    }
    sizes.sort_unstable();
    let mut repeated: Vec<usize> = Vec::new();
    let mut keyed: Vec<(Vec<Vec<(u64, u64)>>, usize)> = Vec::new();
    for group in sizes.chunk_by(|a, b| (a.0, a.1) == (b.0, b.1)) {
        if group.len() < 2 {
            continue;
        }
        keyed.clear();
        for &(count, _, p) in group {
            let from = keep.partition_point(|k| k.0 < p);
            let mut forms = room.vec(count)?;
            for &(p, r) in keep.get(from..from.saturating_add(count)).unwrap_or_default() {
                let Some(ring) = parts.get(p).and_then(|rings| rings.get(r)) else { continue };
                forms.push(normal_form(ring, room)?);
            }
            // The exterior ring first, then the holes in any order.
            if let Some((_, holes)) = forms.split_first_mut() {
                holes.sort_unstable();
            }
            room.grow(&mut keyed)?;
            keyed.push((forms, p));
        }
        keyed.sort_unstable();
        for pair in keyed.windows(2) {
            if let [first, then] = pair
                && first.0 == then.0
            {
                room.grow(&mut repeated)?;
                repeated.push(then.1);
            }
        }
    }
    repeated.sort_unstable();
    let before = keep.len();
    keep.retain(|(p, _)| repeated.binary_search(p).is_err());
    Ok(Distinct { duplicates: duplicates + (before - keep.len()), keep })
}

/// Of `rings` (an id and the ring), the ids of those that aren't a duplicate (the same corners, [`normal_form`]) of another with
/// a lower id, in order, and how many are. Only rings with as many corners ([`corner_count`]) get
/// a normal form.
fn first_of_each(rings: &mut [(usize, &[Coord])], room: Room) -> Result<(Vec<usize>, usize), Limit> {
    // One ring or none: no duplicate, and no corners to count.
    if let [] | [_] = rings {
        return Ok((room.collect(rings.len(), rings.iter().map(|&(id, _)| id))?, 0));
    }
    let mut sized = room.collect(rings.len(), rings.iter().map(|&(id, ring)| (corner_count(ring), id, ring)))?;
    sized.sort_unstable_by_key(|&(n, id, _)| (n, id));
    let mut kept: Vec<usize> = room.vec(rings.len())?;
    let mut keyed: Vec<(Vec<(u64, u64)>, usize)> = Vec::new();
    for group in sized.chunk_by(|a, b| a.0 == b.0) {
        if let [(_, id, _)] = group {
            kept.push(*id);
            continue;
        }
        for &(_, id, ring) in group {
            room.grow(&mut keyed)?;
            keyed.push((normal_form(ring, room)?, id));
        }
    }
    keyed.sort_unstable();
    let mut duplicates = 0usize;
    for (i, (form, id)) in keyed.iter().enumerate() {
        if i > 0 && keyed.get(i - 1).is_some_and(|prev| prev.0 == *form) {
            duplicates += 1;
        } else {
            kept.push(*id);
        }
    }
    kept.sort_unstable();
    Ok((kept, duplicates))
}

/// A ring without its closing repeat.
fn open(ring: &[Coord]) -> &[Coord] {
    match ring.split_last() {
        Some((last, rest)) if Some(last) == rest.first() => rest,
        _ => ring,
    }
}

/// How far from 0, how near it, and how close together coordinates may be for
/// [`straight_through`]'s test to be exact: each 0 or between `1 / EXACT_SPAN` and `EXACT_SPAN` in
/// size, and any two on one axis equal or at least `1 / EXACT_SPAN` apart. The differences'
/// products then run from 1e-200 to 4e200, and the rounding tails the test's later stages
/// multiply together (each at least about 1e-116, from a coordinate or difference of 1e-100) stay
/// within an f64's normal range too.
const EXACT_SPAN: f64 = 1e100;

/// Whether `b`, between `a` and `c` in a ring, lies on the straight line from `a` to `c`, exactly
/// (by Shewchuk's adaptive orientation test, as geo's robust kernel gives it: J. R. Shewchuk,
/// "Adaptive precision floating-point arithmetic and fast robust geometric predicates", Discrete &
/// Computational Geometry 18(3), 1997, 305–363), and strictly between them: a vertex in the middle
/// of a straight edge, which doesn't change the ring. A spike, going back the way it came, isn't.
///
/// The test is exact only where its products neither overflow nor underflow (Shewchuk's own
/// condition; past it, an overflow's NaN reads as collinear and an underflow's 0 as on the line):
/// so only within [`EXACT_SPAN`] of 0, and with no two coordinates closer than its inverse but
/// equal, is a vertex on a slanted edge taken for one mid-edge. Elsewhere it stays a corner. Real
/// coordinates, in degrees, meters or feet, are well inside. A horizontal or vertical edge needs no
/// such test: its vertices share one coordinate exactly, at any size.
fn straight_through(a: Coord, b: Coord, c: Coord) -> bool {
    use geo::kernels::{Kernel, Orientation, RobustKernel};
    let g = |p: Coord| geo::Coord { x: p.x, y: p.y };
    let within = |lo: f64, v: f64, hi: f64| lo.min(hi) <= v && v <= lo.max(hi);
    let sized = |v: f64| v == 0.0 || (1.0 / EXACT_SPAN..=EXACT_SPAN).contains(&v.abs());
    let small = |p: Coord| sized(p.x) && sized(p.y);
    let apart = |u: f64, v: f64| u == v || (u - v).abs() >= 1.0 / EXACT_SPAN;
    let exact = small(a)
        && small(b)
        && small(c)
        && [(a.x, b.x), (a.x, c.x), (b.x, c.x), (a.y, b.y), (a.y, c.y), (b.y, c.y)].iter().all(|&(u, v)| apart(u, v));
    let between = b != a && b != c && within(a.x, b.x, c.x) && within(a.y, b.y, c.y);
    // On a horizontal or vertical edge, being on the line is the shared coordinate being equal:
    // exact at any size, so the range above isn't needed there.
    let level = (a.x == b.x && b.x == c.x) || (a.y == b.y && b.y == c.y);
    between && (level || (exact && RobustKernel::orient2d(g(a), g(b), g(c)) == Orientation::Collinear))
}

/// The corners of an open ring: its vertices but those in the middle of a straight edge
/// ([`straight_through`]), each tested against its neighbors as written, so a straight run of
/// several loses them all. A ring of fewer than three vertices keeps them.
fn corners(pts: &[Coord]) -> impl Iterator<Item = &Coord> {
    let n = pts.len();
    pts.iter()
        .enumerate()
        .filter(move |&(i, &b)| {
            n < 3 || {
                let (a, c) = (pts.get((i + n - 1) % n), pts.get((i + 1) % n));
                !matches!((a, c), (Some(&a), Some(&c)) if straight_through(a, b, c))
            }
        })
        .map(|(_, c)| c)
}

/// How many corners a closed ring has ([`corners`]): the size its [`normal_form`] has.
fn corner_count(ring: &[Coord]) -> usize {
    corners(open(ring)).count()
}

/// A closed ring's corners ([`corners`]) as bits, without the closing repeat: its least rotation
/// (as a sequence of points) in whichever direction reads less, the same for the same ring whatever
/// vertex it starts at, whichever way it is wound, and whatever vertices it has in the middle of a
/// straight edge. In time and memory linear in the ring, however often it passes one vertex
/// ([`least_rotation`]).
fn normal_form(ring: &[Coord], room: Room) -> Result<Vec<(u64, u64)>, Limit> {
    let pts = open(ring);
    let n = corners(pts).count();
    // Plus 0.0 makes -0.0 the same as 0.0: the same point, which writers and reprojection give either way.
    let forward = room.collect(n, corners(pts).map(|c| ((c.x + 0.0).to_bits(), (c.y + 0.0).to_bits())))?;
    let backward = room.collect(n, forward.iter().rev().copied())?;
    let mut best: Option<Vec<(u64, u64)>> = None;
    for seq in [forward, backward] {
        let start = least_rotation(&seq, room)?;
        let form = room.collect(n, seq.iter().skip(start).chain(seq.iter().take(start)).copied())?;
        if best.as_ref().is_none_or(|b| form < *b) {
            best = Some(form);
        }
    }
    Ok(best.unwrap_or_default())
}

/// Where the lexicographically least rotation of `s` starts, by Booth's algorithm in linear time
/// (K. S. Booth, "Lexicographically least circular substrings", Information Processing Letters
/// 10(4–5), 1980, 240–242): a failure function over `s` read twice round, as Knuth–Morris–Pratt
/// matching uses one. 0 for an empty `s`.
fn least_rotation(s: &[(u64, u64)], room: Room) -> Result<usize, Limit> {
    let n = s.len();
    let len = n.saturating_mul(2);
    // f[j]: the failure function's value at j, one more than the index it points to (0 for none),
    // so it stays unsigned.
    let mut f: Vec<usize> = room.collect(len, std::iter::repeat_n(0, len))?;
    let at = |i: usize| s.get(i % n.max(1)).copied().unwrap_or_default();
    let mut k = 0usize;
    for j in 1..len {
        let sj = at(j);
        let mut i = f.get(j.saturating_sub(k + 1)).copied().unwrap_or(0);
        while i != 0 && sj != at(k + i) {
            if sj < at(k + i) {
                k = j.saturating_sub(i);
            }
            i = f.get(i.saturating_sub(1)).copied().unwrap_or(0);
        }
        if sj != at(k + i) {
            // i == 0 here: no proper prefix matches.
            if sj < at(k) {
                k = j;
            }
            if let Some(v) = f.get_mut(j.saturating_sub(k)) {
                *v = 0;
            }
        } else if let Some(v) = f.get_mut(j.saturating_sub(k)) {
            *v = i + 1;
        }
    }
    Ok(k % n.max(1))
}

/// The rebuild by area (see the module doc) of `parts`, closed rings each with 3 distinct points
/// or more, of which those in `crossing` (part, ring; sorted) cross, touch or run back over
/// themselves. The rings of the result, how many rings and parts were left without area, how many
/// holes and parts were dropped as narrower, in any direction, than a step of the grid of the
/// overlay that takes them out or merges them, and what it found: [`Code::HoleOutsideShell`] when some of the holes of a
/// part with such a ring lie outside its exterior ring's area, [`Code::RingsIntersect`] when holes
/// of such a part merged, and [`Code::PartsOverlap`] when parts merged, which each union tells by
/// making fewer polygons than it was given (pieces that only touch at points stay apart; the narrow
/// ones are dropped first, so their vanishing isn't taken for a merge).
fn by_area(parts: &[Vec<Vec<Coord>>], crossing: &[(usize, usize)], room: Room) -> Result<Rebuilt, Kept> {
    let empty = geo::MultiPolygon::<f64>::new(vec![]);
    let area = |ring: &Vec<Coord>, at: (usize, usize)| ring_area(ring, at, crossing, room);
    let mut all: Vec<geo::Polygon<f64>> = Vec::new();
    let mut no_area = 0usize;
    let mut narrow = 0usize;
    let mut found = Found::new();
    for (p, rings) in parts.iter().enumerate() {
        let Some((shell, holes)) = rings.split_first() else { continue };
        let shell = area(shell, (p, 0))?;
        if shell.0.is_empty() {
            // Its holes go with it.
            no_area += 1;
            continue;
        }
        let mut cut: Vec<geo::Polygon<f64>> = Vec::new();
        for (h, hole) in holes.iter().enumerate() {
            let hole = area(hole, (p, h + 1))?;
            if hole.0.is_empty() {
                no_area += 1;
            }
            for piece in hole.0 {
                room.grow(&mut cut).map_err(kept_for)?;
                cut.push(piece);
            }
        }
        // Holes narrower than a step of the grid the difference below rounds to, in any direction,
        // are dropped first.
        if !cut.is_empty() {
            let step = on_grid(rings_of(&shell.0).chain(rings_of(&cut)))?;
            cut = wider(cut, step, &mut narrow, room)?;
        }
        let part = if cut.is_empty() {
            shell
        } else {
            // The holes' areas, each wound the same way, make their union under non-zero winding.
            let cut = geo::MultiPolygon::new(cut);
            let related = (0..rings.len()).all(|r| crossing.binary_search(&(p, r)).is_err());
            if !related {
                unrelated_hole_codes(&shell, &cut, &mut found);
            }
            shell.boolean_op_with_fill_rule(&cut, OpType::Difference, FillRule::NonZero)
        };
        if part.0.is_empty() {
            no_area += 1;
        }
        for piece in part.0 {
            room.grow(&mut all).map_err(kept_for)?;
            all.push(piece);
        }
    }
    if parts.len() > 1 && all.len() > 1 {
        // Parts narrower than a step of the union's grid, in any direction, are dropped first.
        let step = on_grid(rings_of(&all))?;
        all = wider(all, step, &mut narrow, room)?;
    }
    let all = geo::MultiPolygon::new(all);
    let merged = if parts.len() > 1 && all.0.len() > 1 {
        let merged = all.boolean_op_with_fill_rule(&empty, OpType::Union, FillRule::NonZero);
        if merged.0.len() < all.0.len() {
            found.push(Code::PartsOverlap);
        }
        merged
    } else {
        all
    };
    Ok(Rebuilt { polys: polygons(&merged, room).map_err(kept_for)?, no_area, narrow, found, duplicates: 0 })
}

/// The overlay's polygons as rings, copied fallibly.
fn polygons(m: &geo::MultiPolygon<f64>, room: Room) -> Result<Vec<Vec<Vec<Coord>>>, Limit> {
    let mut out = room.vec(m.0.len())?;
    for poly in &m.0 {
        // The overlay writes closed rings of 3 points or more; anything shorter has no area.
        if poly.exterior().0.len() < 4 {
            continue;
        }
        let mut rings = room.vec(poly.interiors().len().saturating_add(1))?;
        for ring in std::iter::once(poly.exterior()).chain(poly.interiors()).filter(|r| r.0.len() >= 4) {
            rings.push(room.collect(ring.0.len(), ring.0.iter().map(|c| Coord::new(c.x, c.y)))?);
        }
        out.push(rings);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`repair_by`] under the default rule.
    fn repair(g: Option<&Geometry>, has_room: &dyn Fn(u64) -> bool) -> Outcome {
        repair_by(g, Rule::default(), has_room)
    }

    /// [`repair_by_with`] under the default rule.
    fn repair_with(g: Option<&Geometry>, budget: u64, allow: &dyn Fn(usize) -> bool, has_room: &dyn Fn(u64) -> bool) -> Outcome {
        repair_by_with(g, Rule::default(), budget, allow, has_room)
    }

    /// Booth's least rotation agrees with trying every rotation, on every sequence of up to 8
    /// symbols from 3, repeats and all.
    #[test]
    fn least_rotation_is_the_least_of_all_rotations() {
        let room = Room(&|_| true);
        for n in 1..=8usize {
            for code in 0..3usize.pow(n as u32) {
                let s: Vec<(u64, u64)> = (0..n).map(|k| ((code / 3usize.pow(k as u32) % 3) as u64, 0)).collect();
                let rotation = |start: usize| -> Vec<(u64, u64)> { s[start..].iter().chain(&s[..start]).copied().collect() };
                let least = (0..n).map(rotation).min().unwrap();
                assert_eq!(rotation(least_rotation(&s, room).unwrap()), least, "{s:?}");
            }
        }
        assert_eq!(least_rotation(&[], room).unwrap(), 0);
    }

    /// The same ring has one normal form from any vertex, in either direction, and a different ring
    /// another; a ring passing one vertex 100,000 times takes linear time, where trying each pass as
    /// a start would take about 10^10 steps.
    #[test]
    fn a_ring_has_one_normal_form() {
        let room = Room(&|_| true);
        let square: Vec<Coord> = ring(SQUARE);
        let want = normal_form(&square, room).unwrap();
        let open = &square[..square.len() - 1];
        for start in 0..open.len() {
            for reversed in [false, true] {
                let mut r: Vec<Coord> = open[start..].iter().chain(&open[..start]).copied().collect();
                if reversed {
                    r.reverse();
                }
                r.push(r[0]);
                assert_eq!(normal_form(&r, room).unwrap(), want, "from {start}, reversed {reversed}");
            }
        }
        assert_ne!(normal_form(&ring(&[(0., 0.), (10., 0.), (10., 11.), (0., 10.), (0., 0.)]), room).unwrap(), want);
        // A vertex in the middle of a straight edge, or a straight run of them, doesn't change the
        // ring; one past the edge's end (a spike out and back along it) does.
        let mid = ring(&[(0., 0.), (2.5, 0.), (5., 0.), (10., 0.), (10., 10.), (0., 10.), (0., 0.)]);
        assert_eq!(normal_form(&mid, room).unwrap(), want);
        assert_eq!(corner_count(&mid), 4);
        let past = ring(&[(0., 0.), (12., 0.), (10., 0.), (10., 10.), (0., 10.), (0., 0.)]);
        assert_ne!(normal_form(&past, room).unwrap(), want);
        // Where the orientation test isn't exact (its products overflowing or underflowing), a
        // vertex is a corner, never taken for one on the line, on it or not: these two triangles
        // aren't one ring. Within its range, one on the line is mid-edge.
        let c = Coord::new;
        assert!(!straight_through(c(-1e200, -1e200), c(0., 0.5e200), c(1e200, 1e200)));
        assert!(!straight_through(c(0., 0.), c(0., 1.), c(1e200, 1e200)), "off the line, overflowing");
        assert!(!straight_through(c(0., 0.), c(1e-200, 1.5e-200), c(2e-200, 2e-200)), "off the line, underflowing");
        // A tiny coordinate beside ordinary ones, where the test's later stages would underflow: off
        // the line by 1e-400, which the test would read as 0.
        assert!(!straight_through(c(1e-200, -1.), c(0.5, -1e-200), c(1., 1.)), "a tiny coordinate");
        // A horizontal or vertical edge is exact at any size: a midpoint at y = 1e-101, or far out.
        assert!(straight_through(c(0., 1e-101), c(5., 1e-101), c(10., 1e-101)), "a tiny level edge");
        assert!(straight_through(c(1e200, 0.), c(1e200, 5.), c(1e200, 10.)), "a far vertical edge");
        assert!(!straight_through(c(0., 1e-101), c(12., 1e-101), c(10., 1e-101)), "past the end still counts");
        // Ordinary data, floating-point noise near 0 included, is within the test's range.
        assert!(straight_through(c(0., 0.), c(6e-17, 6e-17), c(1., 1.)));
        assert!(straight_through(c(-1e100, -1e100), c(0., 0.), c(1e100, 1e100)));
        assert!(straight_through(c(0., 0.), c(1e-90, 1e-90), c(2e-90, 2e-90)));
        let t1 = ring(&[(-1e200, -1e200), (1e200, 1e200), (1e200, -1e200), (-1e200, -1e200)]);
        let t2 = ring(&[(-1e200, -1e200), (0., 0.5e200), (1e200, 1e200), (1e200, -1e200), (-1e200, -1e200)]);
        assert_ne!(normal_form(&t1, room).unwrap(), normal_form(&t2, room).unwrap());
        // At the closing vertex too: a square starting mid-edge.
        let from_mid = ring(&[(5., 0.), (10., 0.), (10., 10.), (0., 10.), (0., 0.), (5., 0.)]);
        assert_eq!(normal_form(&from_mid, room).unwrap(), want);
        let mut star = vec![Coord::new(0., 0.)];
        for k in 0..100_000 {
            let a = f64::from(k) * 1e-5;
            star.extend([Coord::new(a.cos(), a.sin()), Coord::new(0., 0.)]);
        }
        assert_eq!(normal_form(&star, room).unwrap().len(), star.len() - 1);
    }

    type Ring<'a> = &'a [(f64, f64)];

    /// Every reservation tried.
    const ANY: Room<'static> = Room(&|_| true);

    fn ring(pts: Ring) -> Vec<Coord> {
        pts.iter().map(|&(x, y)| Coord::new(x, y)).collect()
    }
    fn multi(parts: &[&[Ring]]) -> Geometry {
        Geometry::Polygon(parts.iter().map(|rings| rings.iter().map(|r| ring(r)).collect()).collect())
    }
    fn poly(rings: &[Ring]) -> Geometry {
        multi(&[rings])
    }
    fn fix(g: &Geometry) -> Outcome {
        repair(Some(g), &|_| true)
    }
    fn repaired(g: &Geometry) -> Repaired {
        match fix(g) {
            Outcome::Repaired(r) => r,
            other => panic!("{g:?} gave {other:?}"),
        }
    }
    /// The area of a polygon geometry, by the shoelace formula over every ring (holes wound
    /// against their shell count negative, so only the magnitude per ring is used: shell minus holes).
    fn area(g: &Geometry) -> f64 {
        let ring_area = |r: &[Coord]| r.windows(2).map(|w| w[0].x * w[1].y - w[1].x * w[0].y).sum::<f64>().abs() / 2.0;
        let Geometry::Polygon(parts) = g else { panic!("a polygon") };
        parts.iter().map(|rings| ring_area(&rings[0]) - rings[1..].iter().map(|h| ring_area(h)).sum::<f64>()).sum()
    }

    const SQUARE: Ring = &[(0., 0.), (10., 0.), (10., 10.), (0., 10.), (0., 0.)];
    const BOWTIE: Ring = &[(0., 0.), (2., 2.), (2., 0.), (0., 2.), (0., 0.)];

    #[test]
    fn cleaned_drops_repeats_and_closes_rings() {
        let pts = ring(&[(0., 0.), (0., 0.), (1., 0.), (1., 0.), (1., 1.)]);
        assert_eq!(cleaned(&pts, true, ANY).unwrap(), ring(&[(0., 0.), (1., 0.), (1., 1.), (0., 0.)]));
        assert_eq!(cleaned(&pts, false, ANY).unwrap(), ring(&[(0., 0.), (1., 0.), (1., 1.)]));
        // A duplicated closing point is one repeat; a closed ring isn't closed again.
        assert_eq!(cleaned(&ring(&[(0., 0.), (1., 0.), (1., 1.), (0., 0.), (0., 0.)]), true, ANY).unwrap().len(), 4);
        assert_eq!(cleaned(&[], true, ANY).unwrap(), []);
    }

    #[test]
    fn degenerate_rings_and_parts_are_dropped_and_counted() {
        let flat: Ring = &[(0., 0.), (1., 0.), (0., 0.)];
        let Geometry::Polygon(polys) = multi(&[&[SQUARE, flat], &[flat, SQUARE], &[], &[SQUARE]]) else { panic!("a polygon") };
        let (out, dropped, changed) = rings(&polys, ANY).unwrap();
        // The hole of part 1; part 2, whose exterior ring goes with its hole; part 3, without rings.
        assert_eq!(dropped, 3);
        assert!(changed);
        assert_eq!(out, vec![vec![ring(SQUARE)], vec![ring(SQUARE)]]);
        let Geometry::Polygon(clean) = poly(&[SQUARE]) else { panic!("a polygon") };
        assert_eq!(rings(&clean, ANY).unwrap(), (clean.clone(), 0, false));
        // The same length as before but not the same ring: a repeat dropped and the ring closed.
        let Geometry::Polygon(open) = poly(&[&[(0., 0.), (10., 0.), (10., 0.), (10., 10.)]]) else { panic!("a polygon") };
        assert!(rings(&open, ANY).unwrap().2);

        let (out, dropped) = lines(&[ring(&[(0., 0.), (0., 0.)]), ring(&[(0., 0.), (1., 1.), (1., 1.)]), vec![]], ANY).unwrap();
        assert_eq!((out, dropped), (vec![ring(&[(0., 0.), (1., 1.)])], 2));
    }

    #[test]
    fn a_ring_stands_for_the_area_it_winds_around() {
        // A bowtie keeps both lobes, whichever way it is wound.
        for bowtie in [BOWTIE.to_vec(), BOWTIE.iter().rev().copied().collect()] {
            let r = repaired(&poly(&[&bowtie]));
            let Geometry::Polygon(parts) = &r.geometry else { panic!() };
            assert_eq!(parts.len(), 2);
            assert_eq!(area(&r.geometry), 2.0);
            assert_eq!(r.codes, [Code::SelfIntersection]);
        }
        // A loop inside the ring, touching it at a vertex: wound against the ring, it is outside
        // (an "inverted" hole, as some formats write one); wound with it, it is inside.
        let against: Ring = &[(0., 0.), (10., 0.), (10., 10.), (0., 10.), (0., 0.), (5., 8.), (8., 5.), (5., 2.), (0., 0.)];
        let with: Ring = &[(0., 0.), (10., 0.), (10., 10.), (0., 10.), (0., 0.), (5., 2.), (8., 5.), (5., 8.), (0., 0.)];
        let r = repaired(&poly(&[against]));
        assert_eq!(area(&r.geometry), 100.0 - 24.0);
        let Geometry::Polygon(parts) = &r.geometry else { panic!() };
        assert_eq!((parts.len(), parts[0].len()), (1, 2), "one polygon with a hole touching its shell at a point");
        assert_eq!(area(&repaired(&poly(&[with])).geometry), 100.0);
    }

    #[test]
    fn grid_steps_follow_i_overlays_scale() {
        let step = |pts: &[(f64, f64)]| grid_step(pts.iter().copied());
        // Half of 10 is 5: log2 5 rounds to 2, so 2^27 units a unit.
        assert_eq!(step(&[(0., 0.), (10., 3.)]), Some((-27f64).exp2()));
        assert_eq!(step(&[(1e5, 0.), (1e5 + 10.7, 10.)]), Some((-27f64).exp2()));
        assert_eq!(step(&[(-180., -90.), (180., 90.)]), Some((-22f64).exp2()));
        assert_eq!(step(&[(1., 1.), (1., 1.)]), None, "no extent");
        assert_eq!(step(&[(-1e308, 0.), (1e308, 0.)]), None, "an extent past the largest float");
        assert_eq!(step(&[(0., 0.), (1e-300, 1e-300)]), None, "a scale past the largest float");
        assert_eq!(step(&[]), None);
        // The overlay runs only on rings with a grid: in the browser, where its failures can't be
        // caught, it never sees one it can't scale.
        let ls = |pts: &[(f64, f64)]| geo::LineString::from(pts.to_vec());
        assert_eq!(on_grid([ls(&[(0., 0.), (1., 0.), (0., 1.), (0., 0.)])].iter()), Ok((-30f64).exp2()));
        assert_eq!(on_grid([ls(&[(0., 0.), (1e-310, 0.), (0., 1e-310), (0., 0.)])].iter()), Err(Kept::Failed));
        assert_eq!(on_grid([ls(&[(-1e308, 0.), (1e308, 0.), (0., 1.), (-1e308, 0.)])].iter()), Err(Kept::Failed));
    }

    #[test]
    fn widths_are_measured_in_every_direction() {
        let p = |pts: &[(f64, f64)]| geo::Polygon::new(geo::LineString::from(pts.to_vec()), vec![]);
        let thin = |pts: &[(f64, f64)], step: f64| narrower_than(&p(pts), step, ANY);
        // A strip at 45°, 100 long and 1 wide: 70 across and up, but within √2 steps across only
        // at a step of more than 1 / √2.
        let a = 0.5f64.sqrt();
        let strip = [(0., 0.), (70.7, 70.7), (70.7 - a, 70.7 + a), (-a, a), (0., 0.)];
        assert_eq!(thin(&strip, 0.70), Ok(false));
        assert_eq!(thin(&strip, 0.71), Ok(true));
        // On the axes, a step is enough, and the bound is strict.
        let wide = [(0., 0.), (100., 0.), (100., 1.), (0., 1.), (0., 0.)];
        assert_eq!(thin(&wide, 1.0), Ok(false));
        assert_eq!(thin(&wide, 1.0 + 1e-9), Ok(true));
        // A triangle's least width is its smallest height: here 4.8, from the right angle to the
        // hypotenuse of a 6, 8, 10 triangle at 36.87°, where rounding moves a point across by up
        // to 0.7 steps (cos + sin over 2); its legs, 6 and 8, lie on the axes.
        let triangle = [(0., 0.), (8., 0.), (0., 6.), (0., 0.)];
        assert_eq!(thin(&triangle, 4.8 / 1.4 - 1e-9), Ok(false));
        assert_eq!(thin(&triangle, 4.8 / 1.4 + 1e-9), Ok(true));
        // Many vertices on a thin lens, far from the origin, wound either way.
        let lens: Vec<(f64, f64)> = (0..=2000)
            .map(|k| {
                let t = std::f64::consts::TAU * f64::from(k) / 2000.0;
                (1e6 + 50.0 * t.cos() - 0.2 * t.sin(), 1e6 + 50.0 * t.cos() + 0.2 * t.sin())
            })
            .collect();
        let back: Vec<(f64, f64)> = lens.iter().rev().copied().collect();
        // 0.4 √2 wide across its diagonal: thin at a step over 0.4.
        for l in [&lens, &back] {
            assert_eq!(thin(l, 0.399), Ok(false));
            assert_eq!(thin(l, 0.401), Ok(true));
        }
        // Points in a row, all one point, nothing, or not numbers: thin or not, but never a panic.
        assert_eq!(thin(&[(0., 0.), (5., 5.), (10., 10.), (0., 0.)], 1e-9), Ok(true));
        assert_eq!(thin(&[(3., 3.), (3., 3.), (3., 3.), (3., 3.)], 1e-9), Ok(true));
        assert_eq!(thin(&[], 1e-9), Ok(true));
        let _ = thin(&[(f64::NAN, 0.), (1., f64::NAN), (2., 2.), (f64::NAN, 0.)], 1e-9);
        let _ = thin(&[(f64::INFINITY, 0.), (1., 1.), (0., f64::NEG_INFINITY), (f64::INFINITY, 0.)], 1e-9);
        // The hull's buffers are asked for first.
        assert_eq!(narrower_than(&p(&strip), 0.70, Room(&|_| false)), Err(Limit::Memory));
        // The hull: anticlockwise, without the points inside or on its edges, or a repeat.
        let hull_of = |pts: &[(f64, f64)]| -> Vec<(f64, f64)> {
            let pts: Vec<geo::Coord<f64>> = pts.iter().map(|&(x, y)| geo::Coord { x, y }).collect();
            hull(&pts, ANY).unwrap().iter().map(|c| (c.x, c.y)).collect()
        };
        assert_eq!(
            hull_of(&[(0., 0.), (2., 0.), (1., 1.), (2., 2.), (1., 0.), (0., 2.), (0., 0.)]),
            [(0., 0.), (2., 0.), (2., 2.), (0., 2.)]
        );
        assert_eq!(hull_of(&[(0., 0.), (1., 1.), (2., 2.)]), [(0., 0.), (2., 2.)]);
        assert_eq!(hull_of(&[(1., 1.)]), [(1., 1.)]);
        assert_eq!(hull_of(&[]), []);
    }

    /// Under Even-Odd, a square with two holes covering it, one with a vertex mid-edge: one hole is
    /// a duplicate, left out, and the other cancels the square, so no area is left, and the
    /// duplicate left out on the way is still given to the tool to say.
    #[test]
    fn duplicates_left_out_before_no_area_are_still_counted() {
        let mid: Ring = &[(0., 0.), (5., 0.), (10., 0.), (10., 10.), (0., 10.), (0., 0.)];
        let g = poly(&[SQUARE, SQUARE, mid]);
        assert_eq!(repair_by(Some(&g), Rule::EvenOdd, &|_| true), Outcome::GoneAfterDuplicates { duplicates: 1 });
        // With no duplicate, no area is as it was.
        assert_eq!(repair_by(Some(&poly(&[SQUARE, SQUARE])), Rule::EvenOdd, &|_| true), Outcome::Gone(Gone::NoArea));
    }

    /// Under Even-Odd, a part repeated with a vertex in the middle of a straight edge is the same
    /// part, left out as a duplicate, not a copy that cancels its twin out.
    #[test]
    fn a_part_repeated_with_a_vertex_mid_edge_is_a_duplicate_under_even_odd() {
        // The same square twice, once with a vertex in the middle of its bottom edge: without
        // seeing them as one, parity cancels them out and the feature has no area.
        let mid: Ring = &[(0., 0.), (5., 0.), (10., 0.), (10., 10.), (0., 10.), (0., 0.)];
        let g = multi(&[&[SQUARE], &[mid]]);
        match repair_by(Some(&g), Rule::EvenOdd, &|_| true) {
            Outcome::Repaired(r) => {
                assert_eq!(r.duplicates, 1);
                assert!((area(&r.geometry) - 100.0).abs() < 1e-9, "{:?}", r.geometry);
            }
            other => panic!("{other:?}"),
        }
    }

    /// Under Even-Odd, parts all narrower than a step of the grid (two thin overlapping rectangles,
    /// a billionth wide, where a step is 2⁻²⁷) are dropped, leaving no area: the feature has none,
    /// not a repair that failed.
    #[test]
    fn parts_all_too_narrow_leave_no_area_under_even_odd() {
        let a: Ring = &[(0., 0.), (1e-9, 0.), (1e-9, 10.), (0., 10.), (0., 0.)];
        let b: Ring = &[(0.5e-9, 0.), (1.5e-9, 0.), (1.5e-9, 10.), (0.5e-9, 10.), (0.5e-9, 0.)];
        let g = multi(&[&[a], &[b]]);
        assert_eq!(repair_by(Some(&g), Rule::EvenOdd, &|_| true), Outcome::Gone(Gone::NoArea));
    }

    #[test]
    fn what_can_not_be_repaired_has_its_reason() {
        assert_eq!(repair(None, &|_| true), Outcome::Gone(Gone::Null));
        assert_eq!(fix(&Geometry::Polygon(vec![])), Outcome::Gone(Gone::Null));
        assert_eq!(fix(&Geometry::Point(Coord::new(f64::NAN, 0.))), Outcome::Gone(Gone::NonFinite));
        assert_eq!(fix(&poly(&[SQUARE, &[(1., 1.), (f64::INFINITY, 1.), (2., 2.), (1., 1.)]])), Outcome::Gone(Gone::NonFinite));
        assert_eq!(fix(&poly(&[&[(0., 0.), (1., 0.), (0., 0.)]])), Outcome::Gone(Gone::TooFewPoints));
        assert_eq!(fix(&Geometry::Line(vec![ring(&[(0., 0.), (0., 0.)])])), Outcome::Gone(Gone::TooFewPoints));
        // Three distinct points in a row: a ring with no area.
        assert_eq!(fix(&poly(&[&[(0., 0.), (1., 0.), (2., 0.), (0., 0.)]])), Outcome::Gone(Gone::NoArea));
        // A hole covering its whole shell.
        assert_eq!(fix(&poly(&[SQUARE, SQUARE])), Outcome::Gone(Gone::NoArea));
        assert_eq!(fix(&Geometry::Point(Coord::new(1., 0.))), Outcome::Valid);
    }

    /// A pentagram: 5 segments, each crossing two others.
    fn pentagram() -> Geometry {
        let at = |k: f64| {
            let a = (90.0 + 144.0 * k).to_radians();
            Coord::new((10.0 * a.cos() * 1e6).round() / 1e6, (10.0 * a.sin() * 1e6).round() / 1e6)
        };
        Geometry::Polygon(vec![vec![(0..=5).map(|k| if k == 5 { at(0.0) } else { at(f64::from(k)) }).collect()]])
    }

    #[test]
    fn the_work_bound_and_the_overlays_memory_leave_a_feature_as_it_was() {
        let star = pentagram();
        let first = validity::check(Some(&star), validity::MAX_WORK);
        assert_eq!(first.contacts, 5);
        // Its 6 coordinates and up to 2 vertices a contact, at 25 each: more than its checks take.
        let need = (6 + 2 * 5) * OVERLAY_COST;
        assert_eq!(kept(&validity::check(Some(&star), need - 1)), None, "the checks fit");
        let r = repaired(&star);
        let Geometry::Polygon(parts) = &r.geometry else { panic!("a polygon") };
        assert_eq!((parts.len(), parts[0].len(), parts[0][0].len()), (1, 1, 11), "the star's outline, its centre filled");
        assert!(matches!(repair_with(Some(&star), need, &|_| true, &|_| true), Outcome::Repaired(_)));
        assert_eq!(repair_with(Some(&star), need - 1, &|_| true, &|_| true), Outcome::Kept(Kept::Work));
        // The browser is asked for the overlay's memory, and refusing it leaves the feature as it was.
        let asked = std::cell::Cell::new(0);
        let r = repair_with(Some(&star), validity::MAX_WORK, &|_| true, &|need| {
            asked.set(need);
            false
        });
        assert_eq!(r, Outcome::Kept(Kept::Memory));
        assert_eq!(asked.get(), (6 + 2 * 5) * OVERLAY_BYTES);
        // Under Even-Odd, one part of one ring makes no duplicate keys, so it's asked the same; two
        // parts, which can, are asked for the keys too.
        let ask = |g: &Geometry| {
            let asked = std::cell::Cell::new(0);
            let _ = repair_by_with(Some(g), Rule::EvenOdd, validity::MAX_WORK, &|_| true, &|need| {
                asked.set(need);
                false
            });
            asked.get()
        };
        assert_eq!(ask(&star), (6 + 2 * 5) * OVERLAY_BYTES);
        let Geometry::Polygon(one) = &star else { panic!("a polygon") };
        let two = Geometry::Polygon(vec![one[0].clone(), one[0].clone()]);
        let size = 12 + 2 * validity::check(Some(&two), validity::MAX_WORK).contacts;
        assert_eq!(ask(&two), size * (OVERLAY_BYTES + EVEN_ODD_BYTES));
        // Without an overlay nothing is asked of the browser.
        let unclosed = poly(&[&[(0., 0.), (10., 0.), (10., 10.), (0., 10.)]]);
        assert!(matches!(repair(Some(&unclosed), &|_| false), Outcome::Repaired(_)));
    }

    /// A star of 2001 points, each joined to the one 1000 on: about 2 million crossings, which the
    /// checks find within the bound (they count a ring's crossings once), but which the overlay
    /// would cut into 4 million vertices.
    #[test]
    fn crossings_are_charged_to_the_overlay() {
        let (k, m) = (2001u32, 1000u32);
        let star: Vec<Coord> = (0..=k)
            .map(|i| {
                let a = std::f64::consts::TAU * f64::from((i * m) % k) / f64::from(k);
                Coord::new(a.cos(), a.sin())
            })
            .collect();
        let g = Geometry::Polygon(vec![vec![star]]);
        let c = validity::check(Some(&g), validity::MAX_WORK);
        assert_eq!(c.codes(), [Code::SelfIntersection], "checked fully");
        assert!(c.contacts > 1_900_000, "{}", c.contacts);
        assert_eq!(fix(&g), Outcome::Kept(Kept::Work));
    }

    /// Every reservation refused in turn: the feature is then left as it was, never dropped,
    /// taken for valid, or written half repaired.
    #[test]
    fn a_reservation_refused_leaves_the_feature_as_it_was() {
        let hole_out: Ring = &[(20., 20.), (21., 20.), (21., 21.), (20., 20.)];
        // A strip at 45°, 1.45 steps of the union's grid wide: only its hull tells it is wide enough.
        let a = 1.45 * (-20f64).exp2() / 2f64.sqrt();
        let strip: Ring = &[(1000., 1000.), (1005., 1005.), (1005. - a, 1005. + a), (1000. - a, 1000. + a), (1000., 1000.)];
        for g in [
            poly(&[BOWTIE]),
            multi(&[&[SQUARE], &[&[(5., 5.), (15., 5.), (15., 15.), (5., 15.), (5., 5.)]], &[strip]]),
            multi(&[&[SQUARE, hole_out], &[&[(5., 5.), (15., 5.), (15., 15.), (5., 15.), (5., 5.)]]]),
            poly(&[&[(0., 0.), (10., 0.), (10., 0.), (10., 10.)]]),
            Geometry::Line(vec![ring(&[(0., 0.), (1., 1.), (1., 1.)])]),
        ] {
            let full = repaired(&g);
            let tried = std::cell::Cell::new(0usize);
            let _ = repair_with(Some(&g), validity::MAX_WORK, &|_| tried.replace(tried.get() + 1) < usize::MAX, &|_| true);
            let n = tried.get();
            assert!(n >= 2, "{g:?}");
            for k in 0..n {
                let tried = std::cell::Cell::new(0usize);
                let allow = |_| {
                    let i = tried.get();
                    tried.set(i + 1);
                    i != k
                };
                assert_eq!(
                    repair_with(Some(&g), validity::MAX_WORK, &allow, &|_| true),
                    Outcome::Kept(Kept::Memory),
                    "{g:?}, refusing {k}"
                );
            }
            assert_eq!(repair_with(Some(&g), validity::MAX_WORK, &|_| true, &|_| true), Outcome::Repaired(full));
        }
    }

    #[test]
    fn a_feature_the_overlay_can_not_represent_is_left_as_it_was() {
        // A bowtie a hundred-billionth of the smallest normal float across: no grid holds it.
        let tiny: Vec<(f64, f64)> = BOWTIE.iter().map(|&(x, y)| (x * 1e-310, y * 1e-310)).collect();
        let g = poly(&[&tiny]);
        assert_eq!(validity::check(Some(&g), validity::MAX_WORK).codes(), [Code::SelfIntersection]);
        assert_eq!(fix(&g), Outcome::Kept(Kept::Failed));
    }

    #[test]
    fn a_repair_that_is_not_valid_is_not_kept() {
        let bad =
            Repaired { geometry: poly(&[BOWTIE]), codes: vec![Code::SelfIntersection], dropped: 0, narrow: 0, moved: None, duplicates: 0 };
        assert_eq!(verified(bad, validity::MAX_WORK, &|_| true), Outcome::Kept(Kept::Failed));
        let busy = Repaired { geometry: poly(&[BOWTIE]), codes: vec![], dropped: 0, narrow: 0, moved: None, duplicates: 0 };
        assert_eq!(verified(busy, 0, &|_| true), Outcome::Kept(Kept::Work));
    }
}
