//! Geometry validity in stored coordinates, after the OGC Simple Features rules (OGC 06-103r4,
//! ISO 19125-1: 6.1.11.1 Polygon, 6.1.12 MultiPolygon): what is wrong with one geometry, as a list
//! of [`Problem`]s. Check Geometry reports them; Repair Geometry starts from them ([`crate::repair`]).
//!
//! - Rings are read as stored: an unclosed ring is reported, and then checked as if it were closed.
//!   Repeated vertices are reported, and then left out of the other checks.
//! - Crossings are found by one sweep over every ring segment of a feature, with exact predicates
//!   (geo's robust [`line_intersection`]). Rings and parts that touch each other are related with
//!   geo's [`Relate`] (DE-9IM), as geo's `Validation` does; rings and parts that don't touch are
//!   either apart or one inside the other, which a point-in-ring test tells. Only rings that are
//!   simple themselves are related, so the relate graph never sees a ring that crosses itself.
//! - A polygon's interior must be connected (6.1.11.1). When its rings are simple and meet only at
//!   points, the rings and the points where two of them touch form a graph, with an edge from each
//!   ring to each point where it touches another ring of its part; the interior is in one piece
//!   exactly when that graph has no cycle, which union-find tells. Two rings touching at two
//!   points make a cycle; any number of rings meeting at one point make a star, which is fine.
//!   geo's `Validation` doesn't check this.
//! - Lines may cross themselves (a simple line isn't required by OGC), and ring orientation isn't
//!   checked: GeoJSON and Shapefile wind rings in opposite directions.
//!
//! The work for one feature is bounded ([`MAX_WORK`]): a feature that needs more is reported as
//! [`Code::TooComplex`], never taken for valid. So is one whose copies for the checks can't be
//! had: they are reserved fallibly, since in the browser a failed allocation stops the app.

use geo::Relate;
use geo::coordinate_position::{CoordPos, coord_pos_relative_to_ring};
use geo::dimensions::Dimensions;
use geo::line_intersection::{LineIntersection, line_intersection};
use rustgis_data::{Coord, Geometry};
use std::collections::{HashMap, HashSet};
use std::fmt;

/// The most work one feature's crossing and overlap checks may take, in segment pairs compared.
/// Each ring coordinate counts [`VERTEX_COST`] first, a point-in-ring test one per ring segment,
/// and relating two rings or parts [`RELATE_COST`] per vertex. The topology checks' bound: about a
/// quarter of a second natively, of which a polygon of a million vertices needs a small part. A
/// busier feature is too complex.
pub(crate) const MAX_WORK: u64 = 50_000_000;
/// Work per ring coordinate of a polygon, charged before its rings are copied for the sweep, so
/// what the sweep holds grows with the budget, not with the feature. Per coordinate it holds
/// about 104 bytes: the ring's copy (16), its segment (64), and the sweep's two orders and lower
/// bounds (24). At 16, a feature has at most `MAX_WORK / 16` (3,125,000) coordinates to sweep,
/// about 325 MB at once; a polygon of a million coordinates spends 16 million of its 50 million on
/// them, and keeps the rest for the comparisons.
const VERTEX_COST: u64 = 16;
/// Work per vertex of relating two rings or parts (building geo's relate graph and its index).
const RELATE_COST: u64 = 32;
/// Work per pair of rings or parts found in contact, and per point where a ring touches another
/// ring of its part: memory held until the sweep ends.
const CONTACT_COST: u64 = 64;

/// What is wrong, in the order the problems of a feature are listed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Code {
    NullGeometry,
    NonFiniteCoordinate,
    TooFewPoints,
    RingNotClosed,
    RepeatedVertex,
    SelfIntersection,
    HoleOutsideShell,
    RingsIntersect,
    DisconnectedInterior,
    PartsOverlap,
    TooComplex,
}

impl Code {
    pub(crate) const ALL: [Code; 11] = [
        Code::NullGeometry,
        Code::NonFiniteCoordinate,
        Code::TooFewPoints,
        Code::RingNotClosed,
        Code::RepeatedVertex,
        Code::SelfIntersection,
        Code::HoleOutsideShell,
        Code::RingsIntersect,
        Code::DisconnectedInterior,
        Code::PartsOverlap,
        Code::TooComplex,
    ];

    /// The stable machine code (data: never translated).
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Code::NullGeometry => "null_geometry",
            Code::NonFiniteCoordinate => "non_finite_coordinate",
            Code::TooFewPoints => "too_few_points",
            Code::RingNotClosed => "ring_not_closed",
            Code::RepeatedVertex => "repeated_vertex",
            Code::SelfIntersection => "self_intersection",
            Code::HoleOutsideShell => "hole_outside_shell",
            Code::RingsIntersect => "rings_intersect",
            Code::DisconnectedInterior => "disconnected_interior",
            Code::PartsOverlap => "parts_overlap",
            Code::TooComplex => "too_complex",
        }
    }
}

/// Where a problem is. Indices are 0-based here and 1-based in descriptions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum At {
    /// A single point.
    Point,
    /// A point of a multipoint.
    MultiPoint(usize),
    /// A part of a line.
    Line(usize),
    /// A polygon (part) of a polygon feature, as a whole.
    Part(usize),
    /// A ring of a polygon part; ring 0 is the exterior.
    Ring(usize, usize),
}

impl At {
    /// The part and ring, for ordering.
    fn key(self) -> (usize, usize) {
        match self {
            At::Point => (0, 0),
            At::MultiPoint(i) | At::Line(i) | At::Part(i) => (i, 0),
            At::Ring(p, r) => (p, r),
        }
    }
}

impl fmt::Display for At {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            At::Point => write!(f, "the point"),
            At::MultiPoint(i) => write!(f, "point {}", i.saturating_add(1)),
            At::Line(p) | At::Part(p) => write!(f, "part {}", p.saturating_add(1)),
            At::Ring(p, 0) => write!(f, "exterior ring of part {}", p.saturating_add(1)),
            At::Ring(p, r) => write!(f, "ring {} of part {}", r.saturating_add(1), p.saturating_add(1)),
        }
    }
}

/// How a ring meets itself, from least to most serious.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Contact {
    /// At a point, without crossing (a vertex on itself or on another of its segments).
    Touch,
    /// Along a stretch, running back over itself.
    Overlap,
    /// Crossing over itself.
    Cross,
}

/// Why a feature wasn't checked fully.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Limit {
    /// It needs more than the work bound.
    Work,
    /// The memory for its rings' copies, or for the sweep over them, couldn't be had.
    Memory,
    /// geo's relate failed on two of its rings or parts (only natively: in the browser a failure
    /// can't be caught, and only simple rings are related to keep it from failing).
    Relate,
}

/// One thing wrong with a geometry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Problem {
    /// No geometry (`missing`), or one without any coordinates.
    Null { missing: bool },
    /// NaN or infinite coordinates: how many, and where the first is.
    NonFinite { count: usize, first: At },
    /// A line part with fewer than 2 distinct points, a ring with fewer than 3, or a polygon part
    /// without rings ([`At::Part`]).
    TooFewPoints { at: At, distinct: usize },
    /// A ring whose last coordinate isn't its first.
    RingNotClosed { at: At },
    /// Consecutive identical vertices (a ring's closing point isn't one).
    RepeatedVertex { at: At, count: usize },
    /// A ring crossing or touching itself.
    SelfIntersection { at: At, contact: Contact },
    /// An interior ring not inside its part's exterior ring.
    HoleOutsideShell { at: At },
    /// Two rings of `part` sharing an area or touching along a line (`area` false).
    RingsIntersect { part: usize, a: usize, b: usize, area: bool },
    /// The interior of `part` split into pieces by its rings touching each other at points.
    DisconnectedInterior { part: usize },
    /// Two parts sharing an area or touching along a line (`area` false).
    PartsOverlap { a: usize, b: usize, area: bool },
    /// Not checked fully: some of the checks of its rings and parts were left out.
    TooComplex { limit: Limit },
}

impl Problem {
    pub(crate) fn code(&self) -> Code {
        match self {
            Problem::Null { .. } => Code::NullGeometry,
            Problem::NonFinite { .. } => Code::NonFiniteCoordinate,
            Problem::TooFewPoints { .. } => Code::TooFewPoints,
            Problem::RingNotClosed { .. } => Code::RingNotClosed,
            Problem::RepeatedVertex { .. } => Code::RepeatedVertex,
            Problem::SelfIntersection { .. } => Code::SelfIntersection,
            Problem::HoleOutsideShell { .. } => Code::HoleOutsideShell,
            Problem::RingsIntersect { .. } => Code::RingsIntersect,
            Problem::DisconnectedInterior { .. } => Code::DisconnectedInterior,
            Problem::PartsOverlap { .. } => Code::PartsOverlap,
            Problem::TooComplex { .. } => Code::TooComplex,
        }
    }

    /// The order of a feature's problems: by code, then by where.
    fn key(&self) -> (Code, usize, usize, usize) {
        let at = match self {
            Problem::Null { .. } | Problem::TooComplex { .. } => (0, 0, 0),
            Problem::NonFinite { first: at, .. }
            | Problem::TooFewPoints { at, .. }
            | Problem::RingNotClosed { at }
            | Problem::RepeatedVertex { at, .. }
            | Problem::SelfIntersection { at, .. }
            | Problem::HoleOutsideShell { at } => {
                let (p, r) = at.key();
                (p, r, 0)
            }
            Problem::RingsIntersect { part, a, b, .. } => (*part, *a, *b),
            Problem::DisconnectedInterior { part } => (*part, 0, 0),
            Problem::PartsOverlap { a, b, .. } => (*a, *b, 0),
        };
        (self.code(), at.0, at.1, at.2)
    }

    /// A short English description naming where (1-based part and ring numbers).
    pub(crate) fn describe(&self) -> String {
        let s = |n: usize| if n == 1 { "" } else { "s" };
        let meet = |area: bool| if area { "overlap" } else { "touch along a line" };
        match self {
            Problem::Null { missing: true } => "no geometry".into(),
            Problem::Null { missing: false } => "empty geometry".into(),
            Problem::NonFinite { count: 1, first } => format!("a non-finite coordinate in {first}; written without geometry"),
            Problem::NonFinite { count, first } => {
                format!("{count} non-finite coordinates, the first in {first}; written without geometry")
            }
            Problem::TooFewPoints { at: at @ At::Part(_), .. } => format!("{at} has no rings"),
            Problem::TooFewPoints { at, distinct: 0 } => format!("{at} has no points"),
            Problem::TooFewPoints { at, distinct } => format!("{at} has {distinct} distinct point{}", s(*distinct)),
            Problem::RingNotClosed { at } => format!("{at} is not closed"),
            Problem::RepeatedVertex { at, count } => {
                format!("{count} repeated {} in {at}", if *count == 1 { "vertex" } else { "vertices" })
            }
            Problem::SelfIntersection { at, contact } => match contact {
                Contact::Cross => format!("{at} crosses itself"),
                Contact::Overlap => format!("{at} runs back over itself"),
                Contact::Touch => format!("{at} touches itself"),
            },
            Problem::HoleOutsideShell { at } => format!("{at} is not inside the exterior ring"),
            Problem::RingsIntersect { part, a: 0, b, area } => {
                format!("exterior ring and ring {} of part {} {}", b.saturating_add(1), part.saturating_add(1), meet(*area))
            }
            Problem::RingsIntersect { part, a, b, area } => {
                format!("rings {} and {} of part {} {}", a.saturating_add(1), b.saturating_add(1), part.saturating_add(1), meet(*area))
            }
            Problem::DisconnectedInterior { part } => {
                format!("the interior of part {} is split into pieces by rings touching at points", part.saturating_add(1))
            }
            Problem::PartsOverlap { a, b, area } => format!("parts {} and {} {}", a.saturating_add(1), b.saturating_add(1), meet(*area)),
            Problem::TooComplex { limit: Limit::Work } => "not checked fully: too complex for the work limit".into(),
            Problem::TooComplex { limit: Limit::Memory } => "not checked fully: not enough memory to check it".into(),
            Problem::TooComplex { limit: Limit::Relate } => "some rings or parts couldn't be checked against each other".into(),
        }
    }
}

/// What [`check`] found in one geometry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Checked {
    /// The problems, by [`Code`] and then by where they are.
    pub problems: Vec<Problem>,
    /// Some rings or parts weren't checked against each other: one of them has too few points,
    /// non-finite coordinates or crosses itself, or (for parts) has a ring problem of its own.
    pub unrelated: bool,
    /// The pairs of ring segments the sweep found meeting, apart from neighbours at their shared
    /// vertex: each adds at most two vertices where an overlay cuts the rings (see
    /// [`crate::repair`]).
    pub contacts: u64,
}

impl Checked {
    /// The codes of the problems, each once, in order.
    pub(crate) fn codes(&self) -> Vec<Code> {
        let mut codes: Vec<Code> = self.problems.iter().map(Problem::code).collect();
        codes.dedup();
        codes
    }
}

/// Checks one geometry in stored coordinates, spending at most `budget` work (see [`MAX_WORK`])
/// on crossings and overlaps.
pub(crate) fn check(g: Option<&Geometry>, budget: u64) -> Checked {
    check_with(g, budget, &|_| true)
}

/// [`check`], asking `allow` before each reservation of a buffer (with its number of elements)
/// whether to try it: tests refuse some to stand in for running out of memory.
pub(crate) fn check_with(g: Option<&Geometry>, budget: u64, allow: &dyn Fn(usize) -> bool) -> Checked {
    let mut out = Checked::default();
    let Some(g) = g else {
        out.problems.push(Problem::Null { missing: true });
        return out;
    };
    if g.coord_count() == 0 {
        out.problems.push(Problem::Null { missing: false });
        return out;
    }
    match g {
        Geometry::Point(c) => {
            if !c.is_finite() {
                out.problems.push(Problem::NonFinite { count: 1, first: At::Point });
            }
        }
        Geometry::MultiPoint(pts) => {
            let mut bad = pts.iter().enumerate().filter(|(_, c)| !c.is_finite()).map(|(i, _)| i);
            if let Some(first) = bad.next() {
                out.problems.push(Problem::NonFinite { count: 1 + bad.count(), first: At::MultiPoint(first) });
            }
        }
        Geometry::Line(parts) => {
            let mut non_finite = NonFinite::default();
            for (p, part) in parts.iter().enumerate() {
                if non_finite.add(part, At::Line(p)) {
                    vertex_checks(&mut out.problems, At::Line(p), part, false);
                }
            }
            non_finite.report(&mut out.problems);
        }
        Geometry::Polygon(polys) => polygon(&mut out, polys, &mut Work { spent: 0, budget, room: Room(allow) }),
    }
    out.problems.sort_by_key(Problem::key);
    out.problems.dedup();
    out
}

/// Counts non-finite coordinates over a geometry's pieces.
#[derive(Default)]
struct NonFinite {
    count: usize,
    first: Option<At>,
}

impl NonFinite {
    /// Adds a piece's coordinates; whether they are all finite.
    fn add(&mut self, pts: &[Coord], at: At) -> bool {
        let bad = pts.iter().filter(|c| !c.is_finite()).count();
        if bad > 0 && self.first.is_none() {
            self.first = Some(at);
        }
        self.count = self.count.saturating_add(bad);
        bad == 0
    }

    fn report(self, problems: &mut Vec<Problem>) {
        if let Some(first) = self.first {
            problems.push(Problem::NonFinite { count: self.count, first });
        }
    }
}

/// The checks of one line part or ring that need no other piece: distinct points, closure and
/// repeated vertices. Whether it has enough distinct points to be checked further.
fn vertex_checks(problems: &mut Vec<Problem>, at: At, pts: &[Coord], ring: bool) -> bool {
    let need = if ring { 3 } else { 2 };
    let distinct = distinct_up_to(pts, need);
    if distinct < need {
        problems.push(Problem::TooFewPoints { at, distinct });
    }
    if ring && pts.first() != pts.last() {
        problems.push(Problem::RingNotClosed { at });
    }
    let repeated = repeated_vertices(pts);
    if repeated > 0 {
        problems.push(Problem::RepeatedVertex { at, count: repeated });
    }
    distinct >= need
}

/// How many distinct points `pts` has, counting up to `limit`.
pub(crate) fn distinct_up_to(pts: &[Coord], limit: usize) -> usize {
    let mut seen: Vec<Coord> = Vec::with_capacity(limit);
    for c in pts {
        if seen.len() >= limit {
            break;
        }
        if !seen.contains(c) {
            seen.push(*c);
        }
    }
    seen.len()
}

/// Vertices equal to the one before them.
fn repeated_vertices(pts: &[Coord]) -> usize {
    pts.windows(2).filter(|w| w.first() == w.get(1)).count()
}

/// Reserves a feature's buffers fallibly, asking first whether to try (see [`check_with`]).
#[derive(Clone, Copy)]
pub(crate) struct Room<'a>(pub(crate) &'a dyn Fn(usize) -> bool);

impl Room<'_> {
    /// An empty vector with room for `n` elements.
    pub(crate) fn vec<T>(self, n: usize) -> Result<Vec<T>, Limit> {
        let mut v = Vec::new();
        if !(self.0)(n) {
            return Err(Limit::Memory);
        }
        v.try_reserve_exact(n).map_err(|_| Limit::Memory)?;
        Ok(v)
    }

    /// At most `n` of `items`, in a vector reserved for `n`, so collecting them allocates nothing more.
    pub(crate) fn collect<T>(self, n: usize, items: impl Iterator<Item = T>) -> Result<Vec<T>, Limit> {
        let mut v = self.vec(n)?;
        v.extend(items.take(n));
        Ok(v)
    }

    /// Room for one more entry in the map `m`, growing it as an insert would.
    fn map<K: Eq + std::hash::Hash, V>(self, m: &mut HashMap<K, V>) -> Result<(), Limit> {
        if m.len() < m.capacity() {
            return Ok(());
        }
        if !(self.0)(m.len().saturating_add(1)) {
            return Err(Limit::Memory);
        }
        m.try_reserve(1).map_err(|_| Limit::Memory)
    }

    /// Room for one more element in the set `m`, growing it as an insert would.
    fn set<K: Eq + std::hash::Hash>(self, m: &mut HashSet<K>) -> Result<(), Limit> {
        if m.len() < m.capacity() {
            return Ok(());
        }
        if !(self.0)(m.len().saturating_add(1)) {
            return Err(Limit::Memory);
        }
        m.try_reserve(1).map_err(|_| Limit::Memory)
    }

    /// Room for one more element in `v`, growing it as `push` would.
    pub(crate) fn grow<T>(self, v: &mut Vec<T>) -> Result<(), Limit> {
        if v.len() < v.capacity() {
            return Ok(());
        }
        if !(self.0)(v.len().saturating_add(1)) {
            return Err(Limit::Memory);
        }
        v.try_reserve(1).map_err(|_| Limit::Memory)
    }
}

/// A ring as the other checks read it: repeated vertices left out, and closed.
fn closed_ring(pts: &[Coord], room: Room) -> Result<geo::LineString<f64>, Limit> {
    let mut v: Vec<geo::Coord<f64>> = room.vec(pts.len().saturating_add(1))?;
    for c in pts {
        let c = geo::Coord { x: c.x, y: c.y };
        if v.last() != Some(&c) {
            v.push(c);
        }
    }
    if let (Some(first), Some(last)) = (v.first().copied(), v.last())
        && first != *last
    {
        v.push(first);
    }
    Ok(geo::LineString(v))
}

/// A segment of a ring, for the sweep.
#[derive(Clone, Copy)]
struct Seg {
    a: geo::Coord<f64>,
    b: geo::Coord<f64>,
    part: usize,
    ring: usize,
    /// Its index in its ring, and how many segments the ring has.
    i: usize,
    n: usize,
}

impl Seg {
    fn lo(&self, y: bool) -> f64 {
        if y { self.a.y.min(self.b.y) } else { self.a.x.min(self.b.x) }
    }
    fn hi(&self, y: bool) -> f64 {
        if y { self.a.y.max(self.b.y) } else { self.a.x.max(self.b.x) }
    }
    /// Neighbours in the same ring, which share a vertex.
    fn next_to(&self, o: &Seg) -> bool {
        let (lo, hi) = (self.i.min(o.i), self.i.max(o.i));
        self.part == o.part && self.ring == o.ring && (hi == lo + 1 || (lo == 0 && hi + 1 == self.n))
    }
}

/// The segments in sweep order along the axis that compares fewer pairs (`true`: y), with how many
/// pairs the sweep compares. The count is exact, so a feature too busy to sweep is known before
/// any pair is compared. The orders are reserved fallibly and sorted in place (an unstable sort
/// allocates nothing): which of two equal lower bounds comes first doesn't change the pairs.
fn sweep_order(segs: &[Seg], room: Room) -> Result<(Vec<usize>, bool, u64), Limit> {
    let along = |y: bool| -> Result<(Vec<usize>, u64), Limit> {
        let mut order: Vec<usize> = room.collect(segs.len(), 0..segs.len())?;
        let lo = |i: &usize| segs.get(*i).map_or(0.0, |s| s.lo(y));
        order.sort_unstable_by(|a, b| lo(a).total_cmp(&lo(b)));
        let los: Vec<f64> = room.collect(order.len(), order.iter().map(lo))?;
        let mut pairs = 0u64;
        for (k, i) in order.iter().enumerate() {
            let hi = segs.get(*i).map_or(0.0, |s| s.hi(y));
            let rest = los.get(k + 1..).unwrap_or(&[]);
            pairs = pairs.saturating_add(rest.partition_point(|l| *l <= hi) as u64);
        }
        Ok((order, pairs))
    };
    let (x_order, x_pairs) = along(false)?;
    if x_pairs <= segs.len() as u64 {
        return Ok((x_order, false, x_pairs));
    }
    let (y_order, y_pairs) = along(true)?;
    Ok(if y_pairs < x_pairs { (y_order, true, y_pairs) } else { (x_order, false, x_pairs) })
}

/// How two different rings or parts meet, from the sweep.
#[derive(Clone, Copy, Default, Debug)]
struct Meet {
    /// Their boundaries cross somewhere.
    cross: bool,
    /// They share a stretch of boundary.
    line: bool,
}

/// Work spent on a feature, against its budget, and how its buffers are reserved.
struct Work<'a> {
    spent: u64,
    budget: u64,
    room: Room<'a>,
}

impl Work<'_> {
    /// Spends `n`; false when that is past the budget.
    fn spend(&mut self, n: u64) -> bool {
        self.spent = self.spent.saturating_add(n);
        self.spent <= self.budget
    }
}

/// A polygon part's rings, as the checks read them: `None` for a ring that can't be checked
/// further (non-finite coordinates, too few points).
struct Part {
    rings: Vec<Option<geo::LineString<f64>>>,
    /// Every ring is there and simple, so the part can be related to others.
    simple: bool,
    /// And its rings are related properly: no hole outside the shell, no rings meeting.
    clean: bool,
}

/// An empty vector with room for `n` elements, unless the checks have stopped (`stop`), or stop
/// now for want of memory.
fn reserved<T>(stop: &mut Option<Limit>, room: Room, n: usize) -> Vec<T> {
    if stop.is_some() {
        return Vec::new();
    }
    room.vec(n).unwrap_or_else(|limit| {
        *stop = Some(limit);
        Vec::new()
    })
}

fn polygon(out: &mut Checked, polys: &[Vec<Vec<Coord>>], work: &mut Work) {
    // Charged before any ring is copied. Past the budget, or without the memory for the copies,
    // only the checks that copy nothing run.
    let coords = polys.iter().flatten().map(Vec::len).fold(0usize, usize::saturating_add);
    let mut stop = (!work.spend(u64::try_from(coords).unwrap_or(u64::MAX).saturating_mul(VERTEX_COST))).then_some(Limit::Work);
    let room = work.room;
    let mut non_finite = NonFinite::default();
    let mut parts: Vec<Part> = reserved(&mut stop, room, polys.len());
    for (p, rings) in polys.iter().enumerate() {
        if rings.is_empty() {
            out.problems.push(Problem::TooFewPoints { at: At::Part(p), distinct: 0 });
        }
        let mut checked = reserved(&mut stop, room, rings.len());
        for (r, ring) in rings.iter().enumerate() {
            let at = At::Ring(p, r);
            let usable = non_finite.add(ring, at) && vertex_checks(&mut out.problems, at, ring, true);
            if stop.is_some() {
                continue;
            }
            match usable.then(|| closed_ring(ring, room)).transpose() {
                // Within the room reserved for the part's rings.
                Ok(copy) => checked.push(copy),
                Err(limit) => stop = Some(limit),
            }
        }
        if stop.is_none() {
            let simple = !checked.is_empty() && checked.iter().all(Option::is_some);
            parts.push(Part { rings: checked, simple, clean: simple });
        }
    }
    non_finite.report(&mut out.problems);
    if let Some(limit) = stop {
        out.problems.push(Problem::TooComplex { limit });
        return;
    }

    let n_segs = parts.iter().flat_map(|p| p.rings.iter().flatten()).map(|r| r.0.len().saturating_sub(1)).fold(0, usize::saturating_add);
    let Ok(mut segs) = room.vec(n_segs) else {
        out.problems.push(Problem::TooComplex { limit: Limit::Memory });
        return;
    };
    // Within the room reserved: each ring has as many segments as counted.
    for (p, part) in parts.iter().enumerate() {
        for (r, ring) in part.rings.iter().enumerate() {
            let Some(ring) = ring else { continue };
            let n = ring.0.len().saturating_sub(1);
            segs.extend(ring.lines().enumerate().map(|(i, l)| Seg { a: l.start, b: l.end, part: p, ring: r, i, n }));
        }
    }
    let (order, y, pairs) = match sweep_order(&segs, room) {
        Ok(sweep) => sweep,
        Err(limit) => {
            out.problems.push(Problem::TooComplex { limit });
            return;
        }
    };
    if !work.spend(pairs) {
        out.problems.push(Problem::TooComplex { limit: Limit::Work });
        return;
    }

    // Each ring's worst contact with itself, how pairs of rings (of a part) and parts meet, and
    // where rings touch other rings of their part.
    let mut selfx: HashMap<(usize, usize), Contact> = HashMap::new();
    let mut rings_meet: HashMap<(usize, usize, usize), Meet> = HashMap::new();
    let mut parts_meet: HashMap<(usize, usize), Meet> = HashMap::new();
    let mut touches: HashSet<Touch> = HashSet::new();
    for (k, i) in order.iter().enumerate() {
        let Some(s) = segs.get(*i) else { continue };
        let hi = s.hi(y);
        for j in order.get(k + 1..).unwrap_or(&[]) {
            let Some(t) = segs.get(*j) else { continue };
            if t.lo(y) > hi {
                break;
            }
            // A touch is at an endpoint of one of the segments, which geo returns exactly.
            let (contact, at) = match line_intersection(geo::Line::new(s.a, s.b), geo::Line::new(t.a, t.b)) {
                None => continue,
                Some(LineIntersection::SinglePoint { is_proper: true, .. }) => (Contact::Cross, None),
                Some(LineIntersection::SinglePoint { is_proper: false, intersection }) => (Contact::Touch, Some(intersection)),
                Some(LineIntersection::Collinear { .. }) => (Contact::Overlap, None),
            };
            let new = if (s.part, s.ring) == (t.part, t.ring) {
                // Neighbours meet at their shared vertex; running back over each other is a spike.
                if contact == Contact::Touch && s.next_to(t) {
                    continue;
                }
                out.contacts = out.contacts.saturating_add(1);
                if !selfx.contains_key(&(s.part, s.ring)) && room.map(&mut selfx).is_err() {
                    out.problems.push(Problem::TooComplex { limit: Limit::Memory });
                    return;
                }
                let e = selfx.entry((s.part, s.ring));
                let new = matches!(e, std::collections::hash_map::Entry::Vacant(_));
                let worst = e.or_insert(contact);
                *worst = (*worst).max(contact);
                new
            } else {
                out.contacts = out.contacts.saturating_add(1);
                let meet = if s.part == t.part {
                    let key = (s.part, s.ring.min(t.ring), s.ring.max(t.ring));
                    if !rings_meet.contains_key(&key) && room.map(&mut rings_meet).is_err() {
                        out.problems.push(Problem::TooComplex { limit: Limit::Memory });
                        return;
                    }
                    let new = !rings_meet.contains_key(&key);
                    (rings_meet.entry(key).or_default(), new)
                } else {
                    let key = (s.part.min(t.part), s.part.max(t.part));
                    if !parts_meet.contains_key(&key) && room.map(&mut parts_meet).is_err() {
                        out.problems.push(Problem::TooComplex { limit: Limit::Memory });
                        return;
                    }
                    let new = !parts_meet.contains_key(&key);
                    (parts_meet.entry(key).or_default(), new)
                };
                meet.0.cross |= contact == Contact::Cross;
                meet.0.line |= contact == Contact::Overlap;
                meet.1
            };
            if new && !work.spend(CONTACT_COST) {
                out.problems.push(Problem::TooComplex { limit: Limit::Work });
                return;
            }
            // One point is usually found by several pairs of segments (it is a vertex): kept once.
            if let Some(at) = at.filter(|_| s.part == t.part && s.ring != t.ring) {
                for ring in [s.ring, t.ring] {
                    if !touches.contains(&(s.part, ring, point_key(at))) && room.set(&mut touches).is_err() {
                        out.problems.push(Problem::TooComplex { limit: Limit::Memory });
                        return;
                    }
                    if touches.insert((s.part, ring, point_key(at))) && !work.spend(CONTACT_COST) {
                        out.problems.push(Problem::TooComplex { limit: Limit::Work });
                        return;
                    }
                }
            }
        }
    }
    drop(segs);
    for (&(p, r), &contact) in &selfx {
        out.problems.push(Problem::SelfIntersection { at: At::Ring(p, r), contact });
        if let Some(part) = parts.get_mut(p) {
            part.simple = false;
            part.clean = false;
        }
    }

    let related = relate_rings(out, &mut parts, &rings_meet, work).and_then(|()| {
        disconnected_interiors(out, &parts, touches, room)?;
        relate_parts(out, &parts, &parts_meet, work)
    });
    if let Err(limit) = related {
        out.problems.push(Problem::TooComplex { limit });
    }
}

/// Where a ring touches another ring of its part: the part, the ring, and the point.
type Touch = (usize, usize, (u64, u64));

/// A point as a key, exactly: -0 and 0 are one.
fn point_key(c: geo::Coord<f64>) -> (u64, u64) {
    let bits = |v: f64| if v == 0.0 { 0.0f64.to_bits() } else { v.to_bits() };
    (bits(c.x), bits(c.y))
}

/// Reports the clean parts whose interior is split into pieces: those whose rings and touch points
/// form a graph with a cycle (see the module doc). Each (ring, point) edge is there once.
fn disconnected_interiors(out: &mut Checked, parts: &[Part], touches: HashSet<Touch>, room: Room) -> Result<(), Limit> {
    let n = touches.len();
    if n == 0 {
        return Ok(());
    }
    let mut touches: Vec<Touch> = room.collect(n, touches.into_iter())?;
    touches.sort_unstable();
    for edges in touches.chunk_by(|a, b| a.0 == b.0) {
        let Some(&(p, ..)) = edges.first() else { continue };
        let Some(part) = parts.get(p).filter(|part| part.clean) else { continue };
        // Nodes: the part's rings, then its touch points as they come.
        let rings = part.rings.len();
        let mut points: HashMap<(u64, u64), usize> = HashMap::new();
        // Every node has its place reserved: the rings, and a point for each edge at most.
        let mut sets = Sets(room.collect(rings.saturating_add(edges.len()), 0..rings)?);
        for &(_, ring, at) in edges {
            let next = rings.saturating_add(points.len());
            if !points.contains_key(&at) {
                room.map(&mut points)?;
            }
            let point = *points.entry(at).or_insert(next);
            if !sets.join(ring, point) {
                out.problems.push(Problem::DisconnectedInterior { part: p });
                break;
            }
        }
    }
    Ok(())
}

/// Disjoint sets (union-find) of nodes `0..`, each pointing towards its set's root.
struct Sets(Vec<usize>);

impl Sets {
    fn root(&mut self, mut i: usize) -> usize {
        loop {
            let up = self.0.get(i).copied().unwrap_or(i);
            if up == i {
                return i;
            }
            // Path halving: point to the grandparent on the way up.
            let grand = self.0.get(up).copied().unwrap_or(up);
            if let Some(slot) = self.0.get_mut(i) {
                *slot = grand;
            }
            i = grand;
        }
    }

    /// Joins the sets of `a` and `b`, adding nodes up to them; false when they were one already.
    fn join(&mut self, a: usize, b: usize) -> bool {
        while self.0.len() <= a.max(b) {
            self.0.push(self.0.len());
        }
        let (ra, rb) = (self.root(a), self.root(b));
        if ra == rb {
            return false;
        }
        if let Some(slot) = self.0.get_mut(ra) {
            *slot = rb;
        }
        true
    }
}

/// The interior rings of each simple part against its exterior and each other.
fn relate_rings(out: &mut Checked, parts: &mut [Part], meets: &HashMap<(usize, usize, usize), Meet>, work: &mut Work) -> Result<(), Limit> {
    for (p, part) in parts.iter_mut().enumerate() {
        if part.rings.len() < 2 {
            continue;
        }
        if !part.simple {
            out.unrelated = true;
            continue;
        }
        let rings: Vec<&geo::LineString<f64>> = work.room.collect(part.rings.len(), part.rings.iter().flatten())?;
        let Some((shell, holes)) = rings.split_first() else { continue };
        let found = out.problems.len();
        for (h, hole) in holes.iter().enumerate() {
            let r = h + 1;
            let meet = meets.get(&(p, 0, r)).copied();
            let inside = match meet {
                None => {
                    spend(work, shell.0.len())?;
                    first_inside(hole, shell)
                }
                Some(m) if m.cross => false,
                Some(_) => {
                    spend_relate(work, shell, hole)?;
                    match relate(&[shell], &[hole]) {
                        Some((contains, _)) => contains,
                        None => {
                            out.problems.push(Problem::TooComplex { limit: Limit::Relate });
                            true
                        }
                    }
                }
            };
            if !inside {
                out.problems.push(Problem::HoleOutsideShell { at: At::Ring(p, r) });
            }
            if meet.is_some_and(|m| m.line) {
                out.problems.push(Problem::RingsIntersect { part: p, a: 0, b: r, area: false });
            }
        }
        for (a, b) in boxes_meet(holes.iter().map(|h| bounds(&h.0)), work)? {
            let (Some(ha), Some(hb)) = (holes.get(a), holes.get(b)) else { continue };
            let (ra, rb) = (a + 1, b + 1);
            let meet = meets.get(&(p, ra, rb)).copied();
            let nested = |work: &mut Work| -> Result<bool, Limit> {
                spend(work, ha.0.len().saturating_add(hb.0.len()))?;
                Ok(first_inside(ha, hb) || first_inside(hb, ha))
            };
            if let Some(area) = shared(out, meet, work, nested, |work| {
                spend_relate(work, ha, hb)?;
                Ok(relate(&[ha], &[hb]).map(|(_, area)| area))
            })? {
                out.problems.push(Problem::RingsIntersect { part: p, a: ra, b: rb, area });
            }
        }
        part.clean = out.problems.len() == found;
    }
    Ok(())
}

/// The clean parts of a multipart polygon against each other.
fn relate_parts(out: &mut Checked, parts: &[Part], meets: &HashMap<(usize, usize), Meet>, work: &mut Work) -> Result<(), Limit> {
    if parts.len() < 2 {
        return Ok(());
    }
    let shells: Vec<Option<&geo::LineString<f64>>> =
        work.room.collect(parts.len(), parts.iter().map(|p| p.rings.first().and_then(Option::as_ref).filter(|_| p.clean)))?;
    if shells.iter().any(Option::is_none) {
        out.unrelated = true;
    }
    let boxes = shells.iter().map(|s| s.map_or(EMPTY_BOX, |s| bounds(&s.0)));
    for (a, b) in boxes_meet(boxes, work)? {
        let (Some(pa), Some(pb)) = (parts.get(a), parts.get(b)) else { continue };
        if !(pa.clean && pb.clean) {
            continue;
        }
        let ra: Vec<&geo::LineString<f64>> = work.room.collect(pa.rings.len(), pa.rings.iter().flatten())?;
        let rb: Vec<&geo::LineString<f64>> = work.room.collect(pb.rings.len(), pb.rings.iter().flatten())?;
        let meet = meets.get(&(a, b)).copied();
        let all = vertices(&ra).saturating_add(vertices(&rb));
        let nested = |work: &mut Work| -> Result<bool, Limit> {
            spend(work, all)?;
            Ok(first_in_part(&ra, &rb) || first_in_part(&rb, &ra))
        };
        if let Some(area) = shared(out, meet, work, nested, |work| {
            spend(work, all.saturating_mul(RELATE_COST as usize))?;
            Ok(relate(&ra, &rb).map(|(_, area)| area))
        })? {
            out.problems.push(Problem::PartsOverlap { a, b, area });
        }
    }
    Ok(())
}

/// How two rings (as areas) or two parts share space: `Some(true)` an area, `Some(false)` only a
/// stretch of boundary, `None` neither. Without contact one may still lie inside the other
/// (`nested`); boundaries that cross always share an area; otherwise geo's relate tells (`related`:
/// whether the interiors share an area, `None` when it failed, which is reported).
fn shared(
    out: &mut Checked,
    meet: Option<Meet>,
    work: &mut Work,
    nested: impl FnOnce(&mut Work) -> Result<bool, Limit>,
    related: impl FnOnce(&mut Work) -> Result<Option<bool>, Limit>,
) -> Result<Option<bool>, Limit> {
    Ok(match meet {
        None => nested(work)?.then_some(true),
        Some(m) if m.cross => Some(true),
        Some(m) => match related(work)? {
            Some(true) => Some(true),
            Some(false) => m.line.then_some(false),
            None => {
                out.problems.push(Problem::TooComplex { limit: Limit::Relate });
                None
            }
        },
    })
}

fn spend(work: &mut Work, n: usize) -> Result<(), Limit> {
    if work.spend(n as u64) { Ok(()) } else { Err(Limit::Work) }
}

fn spend_relate(work: &mut Work, a: &geo::LineString<f64>, b: &geo::LineString<f64>) -> Result<(), Limit> {
    spend(work, a.0.len().saturating_add(b.0.len()).saturating_mul(RELATE_COST as usize))
}

fn vertices(rings: &[&geo::LineString<f64>]) -> usize {
    rings.iter().map(|r| r.0.len()).fold(0, usize::saturating_add)
}

/// Whether the first vertex of `ring` is inside `other` (which it doesn't touch: the sweep found
/// no contact between them, so the whole ring is on that side).
fn first_inside(ring: &geo::LineString<f64>, other: &geo::LineString<f64>) -> bool {
    ring.0.first().is_some_and(|c| coord_pos_relative_to_ring(*c, other) == CoordPos::Inside)
}

/// Whether the first vertex of the part `a` is inside the part `b` (inside its shell and not in
/// one of its holes); the parts' rings don't touch.
fn first_in_part(a: &[&geo::LineString<f64>], b: &[&geo::LineString<f64>]) -> bool {
    let (Some(shell), Some((b_shell, b_holes))) = (a.first(), b.split_first()) else { return false };
    let Some(c) = shell.0.first() else { return false };
    coord_pos_relative_to_ring(*c, b_shell) == CoordPos::Inside
        && b_holes.iter().all(|h| coord_pos_relative_to_ring(*c, h) != CoordPos::Inside)
}

/// geo's relate of two polygons (exterior ring first): whether `a` contains `b`, and whether their
/// interiors share an area. `None` when it failed (natively; see [`Limit::Relate`]).
fn relate(a: &[&geo::LineString<f64>], b: &[&geo::LineString<f64>]) -> Option<(bool, bool)> {
    let poly = |rings: &[&geo::LineString<f64>]| {
        let (shell, holes) = rings.split_first()?;
        Some(geo::Polygon::new((*shell).clone(), holes.iter().map(|h| (*h).clone()).collect()))
    };
    let (a, b) = (poly(a)?, poly(b)?);
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let m = a.relate(&b);
        (m.is_contains(), m.get(CoordPos::Inside, CoordPos::Inside) == Dimensions::TwoDimensional)
    }))
    .ok()
}

/// A bounding box: min x, min y, max x, max y.
type Box2 = (f64, f64, f64, f64);
/// A box no other meets.
const EMPTY_BOX: Box2 = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);

fn bounds(pts: &[geo::Coord<f64>]) -> Box2 {
    pts.iter().fold(EMPTY_BOX, |b, c| (b.0.min(c.x), b.1.min(c.y), b.2.max(c.x), b.3.max(c.y)))
}

/// The pairs `(a, b)`, `a < b`, of boxes that meet, swept along x; each pair compared counts as work.
fn boxes_meet(boxes: impl ExactSizeIterator<Item = Box2>, work: &mut Work) -> Result<Vec<(usize, usize)>, Limit> {
    let boxes: Vec<Box2> = work.room.collect(boxes.len(), boxes)?;
    let mut order: Vec<usize> = work.room.collect(boxes.len(), (0..boxes.len()).filter(|i| boxes.get(*i).is_some_and(|b| b.0 <= b.2)))?;
    // In place: an unstable sort allocates nothing, and equal boxes may come in either order.
    order.sort_unstable_by(|a, b| boxes.get(*a).map_or(0.0, |x| x.0).total_cmp(&boxes.get(*b).map_or(0.0, |x| x.0)));
    let mut pairs = Vec::new();
    for (k, i) in order.iter().enumerate() {
        let Some(a) = boxes.get(*i) else { continue };
        for j in order.get(k + 1..).unwrap_or(&[]) {
            let Some(b) = boxes.get(*j) else { continue };
            if b.0 > a.2 {
                break;
            }
            spend(work, 1)?;
            if b.1 <= a.3 && a.1 <= b.3 {
                // A pair kept costs as much as a contact: memory held until the pairs are checked.
                spend(work, CONTACT_COST as usize)?;
                work.room.grow(&mut pairs)?;
                pairs.push(((*i).min(*j), (*i).max(*j)));
            }
        }
    }
    pairs.sort_unstable();
    Ok(pairs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use geo::Validation;

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
    fn problems(g: &Geometry) -> Vec<Problem> {
        check(Some(g), MAX_WORK).problems
    }
    fn described(g: &Geometry) -> Vec<String> {
        problems(g).iter().map(Problem::describe).collect()
    }

    const SQUARE: Ring = &[(0., 0.), (10., 0.), (10., 10.), (0., 10.), (0., 0.)];
    const HOLE: Ring = &[(2., 2.), (4., 2.), (4., 4.), (2., 4.), (2., 2.)];
    const BOWTIE: Ring = &[(0., 0.), (2., 2.), (2., 0.), (0., 2.), (0., 0.)];

    #[test]
    fn vertex_helpers_count_what_they_say() {
        let pts = ring(&[(0., 0.), (0., 0.), (1., 0.), (1., 0.), (1., 0.), (0., 0.)]);
        assert_eq!(repeated_vertices(&pts), 3);
        assert_eq!(distinct_up_to(&pts, 3), 2);
        assert_eq!(distinct_up_to(&ring(&[(0., 0.), (1., 0.), (2., 0.), (3., 0.)]), 3), 3, "stops at the limit");
        assert_eq!(distinct_up_to(&ring(&[(0., 0.), (-0., 0.)]), 3), 1, "signed zeros are one point");
        assert_eq!(distinct_up_to(&[], 2), 0);
        // Repeats left out, and closed once.
        let closed = closed_ring(&ring(&[(0., 0.), (1., 0.), (1., 0.), (1., 1.)]), ANY).unwrap();
        assert_eq!(closed.0.len(), 4);
        assert_eq!(closed.0.first(), closed.0.last());
        assert_eq!(closed_ring(&ring(&[(0., 0.), (1., 0.), (1., 1.), (0., 0.), (0., 0.)]), ANY).unwrap().0.len(), 4);
    }

    #[test]
    fn valid_shapes_have_no_problems() {
        let triangle: Ring = &[(0., 0.), (1., 0.), (0., 1.), (0., 0.)];
        assert_eq!(problems(&poly(&[triangle])), []);
        assert_eq!(problems(&poly(&[SQUARE, HOLE])), []);
        // A hole touching the shell at one point, and two holes touching at one point.
        assert_eq!(problems(&poly(&[SQUARE, &[(0., 5.), (3., 4.), (3., 6.), (0., 5.)]])), []);
        assert_eq!(problems(&poly(&[SQUARE, HOLE, &[(4., 4.), (6., 4.), (6., 6.), (4., 4.)]])), []);
        // Parts apart, touching at a corner, and an island in another part's hole.
        let far: Ring = &[(20., 0.), (30., 0.), (30., 10.), (20., 0.)];
        let corner: Ring = &[(10., 10.), (20., 10.), (20., 20.), (10., 10.)];
        let island: Ring = &[(2.5, 2.5), (3.5, 2.5), (3.5, 3.5), (2.5, 2.5)];
        assert_eq!(problems(&multi(&[&[SQUARE], &[far], &[corner]])), []);
        assert_eq!(problems(&multi(&[&[SQUARE, HOLE], &[island]])), []);
        // Lines may cross themselves; points only need finite coordinates.
        assert_eq!(problems(&Geometry::Line(vec![ring(BOWTIE)])), []);
        assert_eq!(problems(&Geometry::Point(Coord::new(1., 2.))), []);
        assert_eq!(problems(&Geometry::MultiPoint(ring(&[(1., 2.), (1., 2.)]))), [], "repeated points of a multipoint are fine");
    }

    #[test]
    fn null_and_empty_geometries() {
        assert_eq!(check(None, MAX_WORK).problems, [Problem::Null { missing: true }]);
        for g in [
            Geometry::MultiPoint(vec![]),
            Geometry::Line(vec![]),
            Geometry::Line(vec![vec![], vec![]]),
            Geometry::Polygon(vec![]),
            Geometry::Polygon(vec![vec![]]),
            Geometry::Polygon(vec![vec![vec![]]]),
        ] {
            assert_eq!(problems(&g), [Problem::Null { missing: false }], "{g:?}");
        }
        assert_eq!(Problem::Null { missing: false }.describe(), "empty geometry");
    }

    #[test]
    fn non_finite_coordinates_are_counted_and_their_pieces_not_checked_further() {
        assert_eq!(
            described(&Geometry::Point(Coord::new(f64::NAN, 0.))),
            ["a non-finite coordinate in the point; written without geometry"]
        );
        let mp = Geometry::MultiPoint(ring(&[(0., 0.), (f64::INFINITY, 0.), (0., f64::NAN)]));
        assert_eq!(described(&mp), ["2 non-finite coordinates, the first in point 2; written without geometry"]);
        let line = Geometry::Line(vec![ring(&[(0., 0.), (0., 0.)]), ring(&[(f64::NAN, 0.), (f64::NAN, 0.)])]);
        assert_eq!(
            described(&line),
            [
                "2 non-finite coordinates, the first in part 2; written without geometry",
                "part 1 has 1 distinct point",
                "1 repeated vertex in part 1"
            ]
        );
        // A bowtie part is still found; the part with NaN is left out of the crossings and of the
        // relations, which the result says.
        let g = multi(&[&[BOWTIE], &[&[(5., 5.), (f64::NAN, 5.), (6., 6.), (5., 5.)]]]);
        let c = check(Some(&g), MAX_WORK);
        assert_eq!(c.codes(), [Code::NonFiniteCoordinate, Code::SelfIntersection]);
        assert!(c.unrelated);
    }

    #[test]
    fn rings_and_lines_with_too_few_points() {
        assert_eq!(
            described(&Geometry::Line(vec![ring(&[(0., 0.), (1., 1.)]), ring(&[(2., 2.)]), vec![]])),
            ["part 2 has 1 distinct point", "part 3 has no points"]
        );
        // Two distinct points closed is not a ring; nor is a part without rings.
        let g = multi(&[&[SQUARE], &[], &[&[(20., 0.), (21., 0.), (20., 0.)]]]);
        assert_eq!(described(&g), ["part 2 has no rings", "exterior ring of part 3 has 2 distinct points"]);
        let c = check(Some(&g), MAX_WORK);
        assert!(c.unrelated, "parts with unusable rings aren't related to the others");
        // Three distinct points closed, the smallest ring, is fine.
        assert_eq!(problems(&poly(&[&[(0., 0.), (1., 0.), (1., 1.), (0., 0.)]])), []);
        // A hole with too few points: the part's rings aren't related.
        let c = check(Some(&poly(&[SQUARE, &[(1., 1.), (2., 2.), (1., 1.)]])), MAX_WORK);
        assert_eq!(c.codes(), [Code::TooFewPoints]);
        assert!(c.unrelated);
    }

    #[test]
    fn unclosed_rings_are_reported_and_checked_as_closed() {
        let open: Ring = &[(0., 0.), (10., 0.), (10., 10.), (0., 10.)];
        assert_eq!(described(&poly(&[open])), ["exterior ring of part 1 is not closed"]);
        // An unclosed bowtie still crosses itself once closed.
        assert_eq!(
            described(&poly(&[&[(0., 0.), (2., 2.), (2., 0.), (0., 2.)]])),
            ["exterior ring of part 1 is not closed", "exterior ring of part 1 crosses itself"]
        );
        // An unclosed hole outside its unclosed shell.
        assert_eq!(
            described(&poly(&[open, &[(20., 20.), (21., 20.), (21., 21.)]])),
            ["exterior ring of part 1 is not closed", "ring 2 of part 1 is not closed", "ring 2 of part 1 is not inside the exterior ring"]
        );
    }

    #[test]
    fn repeated_vertices_ignore_the_closing_point() {
        let dup: Ring = &[(0., 0.), (10., 0.), (10., 0.), (10., 10.), (0., 10.), (0., 0.), (0., 0.)];
        assert_eq!(described(&poly(&[dup])), ["2 repeated vertices in exterior ring of part 1"]);
        assert_eq!(problems(&poly(&[SQUARE])), [], "a closed ring's last point repeats its first, which is no repeat");
    }

    #[test]
    fn self_intersections_cross_touch_or_run_back() {
        assert_eq!(problems(&poly(&[BOWTIE])), [Problem::SelfIntersection { at: At::Ring(0, 0), contact: Contact::Cross }]);
        // Two triangles touching at (2, 2): the ring touches itself at a vertex.
        let touch: Ring = &[(0., 0.), (4., 0.), (2., 2.), (4., 4.), (0., 4.), (2., 2.), (0., 0.)];
        assert_eq!(described(&poly(&[touch])), ["exterior ring of part 1 touches itself"]);
        // A vertex on another of its segments.
        let t: Ring = &[(0., 0.), (4., 0.), (4., 4.), (2., 0.5), (2., 0.), (0., 4.), (0., 0.)];
        assert_eq!(problems(&poly(&[t])).first().map(Problem::code), Some(Code::SelfIntersection));
        // A spike, and a degenerate ring of three collinear points.
        let spike: Ring = &[(0., 0.), (10., 0.), (10., 10.), (10., 15.), (10., 10.), (0., 10.), (0., 0.)];
        assert_eq!(described(&poly(&[spike])), ["exterior ring of part 1 runs back over itself"]);
        assert_eq!(described(&poly(&[&[(0., 0.), (1., 0.), (2., 0.), (0., 0.)]])), ["exterior ring of part 1 runs back over itself"]);
        // A hole crossing itself; its part's rings then aren't related.
        let c = check(Some(&poly(&[SQUARE, &[(2., 2.), (4., 4.), (4., 2.), (2., 4.), (2., 2.)]])), MAX_WORK);
        assert_eq!(c.problems.iter().map(Problem::describe).collect::<Vec<_>>(), ["ring 2 of part 1 crosses itself"]);
        assert!(c.unrelated);
        // Collinear neighbours meeting only at their shared vertex are fine.
        assert_eq!(problems(&poly(&[&[(0., 0.), (5., 0.), (10., 0.), (10., 10.), (0., 0.)]])), []);
    }

    #[test]
    fn holes_outside_or_meeting_other_rings() {
        let outside: Ring = &[(20., 20.), (21., 20.), (21., 21.), (20., 20.)];
        let crossing: Ring = &[(8., 2.), (12., 2.), (12., 4.), (8., 4.), (8., 2.)];
        assert_eq!(described(&poly(&[SQUARE, outside])), ["ring 2 of part 1 is not inside the exterior ring"]);
        assert_eq!(described(&poly(&[SQUARE, crossing])), ["ring 2 of part 1 is not inside the exterior ring"]);
        // A hole sharing an edge with the shell, inside it.
        let edge: Ring = &[(0., 2.), (3., 2.), (3., 4.), (0., 4.), (0., 2.)];
        assert_eq!(described(&poly(&[SQUARE, edge])), ["exterior ring and ring 2 of part 1 touch along a line"]);
        // A hole outside, sharing an edge with the shell from outside.
        let beside: Ring = &[(10., 2.), (12., 2.), (12., 4.), (10., 4.), (10., 2.)];
        assert_eq!(
            described(&poly(&[SQUARE, beside])),
            ["ring 2 of part 1 is not inside the exterior ring", "exterior ring and ring 2 of part 1 touch along a line"]
        );
        // Holes overlapping, nested, and sharing an edge.
        let over: Ring = &[(3., 3.), (5., 3.), (5., 5.), (3., 5.), (3., 3.)];
        let inner: Ring = &[(2.5, 2.5), (3.5, 2.5), (3.5, 3.5), (2.5, 2.5)];
        let side: Ring = &[(4., 2.), (6., 2.), (6., 4.), (4., 4.), (4., 2.)];
        assert_eq!(described(&poly(&[SQUARE, HOLE, over])), ["rings 2 and 3 of part 1 overlap"]);
        assert_eq!(described(&poly(&[SQUARE, HOLE, inner])), ["rings 2 and 3 of part 1 overlap"]);
        assert_eq!(described(&poly(&[SQUARE, HOLE, side])), ["rings 2 and 3 of part 1 touch along a line"]);
        // Several problems in order of code, then of place.
        assert_eq!(
            problems(&poly(&[SQUARE, over, outside, HOLE])),
            [Problem::HoleOutsideShell { at: At::Ring(0, 2) }, Problem::RingsIntersect { part: 0, a: 1, b: 3, area: true },]
        );
    }

    #[test]
    fn parts_overlapping_or_touching_along_a_line() {
        let shifted: Ring = &[(5., 5.), (15., 5.), (15., 15.), (5., 15.), (5., 5.)];
        let beside: Ring = &[(10., 0.), (20., 0.), (20., 10.), (10., 10.), (10., 0.)];
        let within: Ring = &[(1., 1.), (2., 1.), (2., 2.), (1., 1.)];
        let same_hole_island: Ring = &[(2., 2.), (4., 2.), (4., 4.), (2., 4.), (2., 2.)];
        assert_eq!(described(&multi(&[&[SQUARE], &[shifted]])), ["parts 1 and 2 overlap"]);
        assert_eq!(described(&multi(&[&[SQUARE], &[beside]])), ["parts 1 and 2 touch along a line"]);
        assert_eq!(described(&multi(&[&[SQUARE], &[within]])), ["parts 1 and 2 overlap"]);
        assert_eq!(described(&multi(&[&[within], &[SQUARE]])), ["parts 1 and 2 overlap"], "either way round");
        // An island filling a hole exactly shares the hole's boundary.
        assert_eq!(described(&multi(&[&[SQUARE, HOLE], &[same_hole_island]])), ["parts 1 and 2 touch along a line"]);
        // Parts with ring problems aren't related to others, and the result says so.
        let c = check(Some(&multi(&[&[SQUARE, &[(20., 20.), (21., 20.), (21., 21.), (20., 20.)]], &[shifted]])), MAX_WORK);
        assert_eq!(c.codes(), [Code::HoleOutsideShell]);
        assert!(c.unrelated);
    }

    #[test]
    fn the_work_bound_reports_a_feature_as_too_complex_never_valid() {
        let g = poly(&[BOWTIE]);
        let c = check(Some(&g), 1);
        assert_eq!(c.problems, [Problem::TooComplex { limit: Limit::Work }]);
        // Checks that need no sweep still run.
        let open = poly(&[&[(0., 0.), (2., 2.), (2., 0.), (0., 2.)]]);
        assert_eq!(check(Some(&open), 0).codes(), [Code::RingNotClosed, Code::TooComplex]);
        // The relations have their share of the bound too: a hole needs a point-in-ring test.
        let pairs = {
            let segs: Vec<Seg> = [SQUARE, HOLE]
                .iter()
                .enumerate()
                .flat_map(|(r, pts)| {
                    let l = closed_ring(&ring(pts), ANY).unwrap();
                    let n = l.0.len() - 1;
                    l.lines().enumerate().map(|(i, l)| Seg { a: l.start, b: l.end, part: 0, ring: r, i, n }).collect::<Vec<_>>()
                })
                .collect();
            sweep_order(&segs, ANY).unwrap().2
        };
        // Each of the 10 ring coordinates is charged too.
        let coords = 10 * VERTEX_COST;
        assert_eq!(check(Some(&poly(&[SQUARE, HOLE])), coords + pairs).codes(), [Code::TooComplex]);
        assert_eq!(check(Some(&poly(&[SQUARE, HOLE])), coords + pairs + 5).codes(), []);
    }

    /// The coordinates are charged before the rings are copied and swept: a feature whose
    /// coordinates alone are past the budget is too complex, however few pairs its sweep compares.
    #[test]
    fn coordinates_are_charged_before_the_sweep() {
        let pairs = |rings: &[Ring]| {
            let segs: Vec<Seg> = rings
                .iter()
                .enumerate()
                .flat_map(|(r, pts)| {
                    let l = closed_ring(&ring(pts), ANY).unwrap();
                    let n = l.0.len() - 1;
                    l.lines().enumerate().map(|(i, l)| Seg { a: l.start, b: l.end, part: 0, ring: r, i, n }).collect::<Vec<_>>()
                })
                .collect();
            sweep_order(&segs, ANY).unwrap().2
        };
        let charge = 5 * VERTEX_COST;
        assert!(pairs(&[SQUARE]) < charge - 1, "the sweep alone would fit");
        assert_eq!(check(Some(&poly(&[SQUARE])), charge - 1).problems, [Problem::TooComplex { limit: Limit::Work }]);
        assert_eq!(check(Some(&poly(&[SQUARE])), charge + pairs(&[SQUARE])).problems, []);
        // Checks that copy nothing still run: the open ring is reported.
        let open: Ring = &[(0., 0.), (10., 0.), (10., 10.), (0., 10.)];
        assert_eq!(check(Some(&poly(&[open])), 4 * VERTEX_COST - 1).codes(), [Code::RingNotClosed, Code::TooComplex]);
        // A ring past the budget by its coordinates alone: too complex, never valid, and quick.
        let circle = |n: u32| {
            let step = std::f64::consts::TAU / f64::from(n);
            let mut pts: Vec<Coord> =
                (0..n).map(|i| Coord::new((f64::from(i) * step).cos() * 1e3, (f64::from(i) * step).sin() * 1e3)).collect();
            pts.push(Coord::new(1e3, 0.));
            Geometry::Polygon(vec![vec![pts]])
        };
        let over = u32::try_from(MAX_WORK / VERTEX_COST).unwrap();
        let started = std::time::Instant::now();
        assert_eq!(check(Some(&circle(over)), MAX_WORK).problems, [Problem::TooComplex { limit: Limit::Work }]);
        assert!(started.elapsed().as_secs() < 10, "not swept");
        // A polygon of a million coordinates is still checked fully.
        assert_eq!(check(Some(&circle(1_000_000)), MAX_WORK).problems, []);
    }

    /// Counts the reservations in `tried`, allowing those whose number (from 0) `allow` takes.
    fn counted(tried: &std::cell::Cell<usize>, allow: impl Fn(usize) -> bool) -> impl Fn(usize) -> bool {
        move |_| {
            let n = tried.get();
            tried.set(n + 1);
            allow(n)
        }
    }

    /// A buffer whose memory can't be had makes the feature too complex, never valid, wherever the
    /// checks ask for it; with the memory, the feature is checked fully.
    #[test]
    fn a_reservation_refused_reports_the_feature_too_complex_never_valid() {
        // Two holes whose boxes meet, and an island in one of them: every buffer is asked for, and
        // nothing is wrong. Both axes are swept for their pairs.
        let a: Ring = &[(2., 2.), (6., 2.), (2., 6.), (2., 2.)];
        let b: Ring = &[(6., 6.), (6., 4.5), (4.5, 6.), (6., 6.)];
        let island: Ring = &[(2.5, 2.5), (3.5, 2.5), (2.5, 3.5), (2.5, 2.5)];
        let g = multi(&[&[SQUARE, a, b], &[island]]);
        let tried = std::cell::Cell::new(0);
        assert_eq!(check_with(Some(&g), MAX_WORK, &counted(&tried, |_| true)).problems, []);
        let asked = tried.get();
        // The parts, each part's rings, the four ring copies, the segments, the sweep's two orders
        // along each axis, the holes and their boxes and pairs, the parts' shells, boxes and pair,
        // and the two parts' rings.
        assert_eq!(asked, 1 + 2 + 4 + 1 + 4 + (1 + 3) + (1 + 3) + 2);
        // Each refused alone, so no later reservation can make up for one taken lightly.
        for k in 0..asked {
            let tried = std::cell::Cell::new(0);
            let c = check_with(Some(&g), MAX_WORK, &counted(&tried, |n| n != k));
            assert_eq!(c.problems, [Problem::TooComplex { limit: Limit::Memory }], "reservation {k} refused");
        }
        // Rings in contact: the maps of contacts, the touch points and the cycle test's nodes are
        // asked for too, each refused in turn.
        let g = poly(&[SQUARE, ACROSS]);
        let tried = std::cell::Cell::new(0);
        assert_eq!(check_with(Some(&g), MAX_WORK, &counted(&tried, |_| true)).problems, disconnected(0));
        let asked_in_contact = tried.get();
        assert!(asked_in_contact > 1 + 1 + 2 + 1 + 4 + 2, "the contacts' maps are asked for: {asked_in_contact}");
        for k in 0..asked_in_contact {
            let tried = std::cell::Cell::new(0);
            let c = check_with(Some(&g), MAX_WORK, &counted(&tried, |n| n != k));
            assert_eq!(c.problems, [Problem::TooComplex { limit: Limit::Memory }], "reservation {k} of the touching rings refused");
        }
        // A ring meeting itself: the map of self-contacts is asked for too.
        let g = poly(&[BOWTIE]);
        let tried = std::cell::Cell::new(0);
        assert_eq!(check_with(Some(&g), MAX_WORK, &counted(&tried, |_| true)).codes(), [Code::SelfIntersection]);
        for k in 0..tried.get() {
            let tried = std::cell::Cell::new(0);
            let c = check_with(Some(&g), MAX_WORK, &counted(&tried, |n| n != k));
            assert_eq!(c.problems, [Problem::TooComplex { limit: Limit::Memory }], "reservation {k} of the bowtie refused");
        }
        assert_eq!(Problem::TooComplex { limit: Limit::Memory }.describe(), "not checked fully: not enough memory to check it");
        assert_eq!(Problem::TooComplex { limit: Limit::Work }.describe(), "not checked fully: too complex for the work limit");
    }

    /// Without the memory for the copies, the checks that copy nothing still run, on every ring.
    #[test]
    fn without_memory_the_vertex_checks_still_run() {
        let open: Ring = &[(0., 0.), (10., 0.), (10., 0.), (10., 10.), (0., 10.)];
        let thin: Ring = &[(20., 0.), (21., 0.), (20., 0.)];
        let g = multi(&[&[SQUARE, open], &[thin], &[&[(30., 0.), (f64::NAN, 0.), (31., 1.), (30., 0.)]]]);
        let all = [
            Problem::NonFinite { count: 1, first: At::Ring(2, 0) },
            Problem::TooFewPoints { at: At::Ring(1, 0), distinct: 2 },
            Problem::RingNotClosed { at: At::Ring(0, 1) },
            Problem::RepeatedVertex { at: At::Ring(0, 1), count: 1 },
            Problem::TooComplex { limit: Limit::Memory },
        ];
        // Nothing reserved, or only the parts and the first part's rings (its shell's copy is refused).
        for k in [0, 2] {
            let tried = std::cell::Cell::new(0);
            assert_eq!(check_with(Some(&g), MAX_WORK, &counted(&tried, |n| n < k)).problems, all, "{k} allowed");
        }
        // Lines and points copy nothing.
        let line = Geometry::Line(vec![ring(&[(0., 0.), (0., 0.)])]);
        assert_eq!(check_with(Some(&line), MAX_WORK, &|_| false).codes(), [Code::TooFewPoints, Code::RepeatedVertex]);
    }

    /// The pairs the sweep is charged for are the ones it compares, along the axis with fewer.
    #[test]
    fn the_sweep_counts_its_pairs_before_comparing_them() {
        let seg = |x0: f64, y0: f64, x1: f64, y1: f64, i: usize| Seg {
            a: geo::Coord { x: x0, y: y0 },
            b: geo::Coord { x: x1, y: y1 },
            part: 0,
            ring: 0,
            i,
            n: 100,
        };
        // Ten segments spanning the same x range, stacked in y: every pair overlaps in x, none in y.
        let stacked: Vec<Seg> = (0..10).map(|i| seg(0., i as f64, 1., i as f64 + 0.5, i)).collect();
        let (order, y, pairs) = sweep_order(&stacked, ANY).unwrap();
        assert!(y);
        assert_eq!(pairs, 0);
        assert_eq!(order.len(), 10);
        // A zigzag across the same x range, overlapping in both: 45 pairs.
        let zigzag: Vec<Seg> = (0..10).map(|i| seg(0., i as f64, 1., i as f64 + 20.0, i)).collect();
        assert_eq!(sweep_order(&zigzag, ANY).unwrap().2, 45);
    }

    // Rings touching at points: the interior is split when the rings and touch points form a cycle.
    const ACROSS: Ring = &[(0., 5.), (5., 2.), (10., 5.), (5., 8.), (0., 5.)];
    const CORNERS: Ring = &[(0., 0.), (6., 4.), (10., 10.), (4., 6.), (0., 0.)];
    const AT_SHELL: Ring = &[(0., 5.), (3., 4.), (3., 6.), (0., 5.)];
    // Three holes around the triangle (3, 3), (7, 3), (5, 6), each touching the next at a corner.
    const LEFT: Ring = &[(3., 3.), (5., 6.), (1., 6.), (3., 3.)];
    const BELOW: Ring = &[(3., 3.), (5., 1.), (7., 3.), (3., 3.)];
    const RIGHT: Ring = &[(7., 3.), (9., 6.), (5., 6.), (7., 3.)];
    // Two holes touching the shell at (0, 5), one above and one below.
    const UP: Ring = &[(0., 5.), (3., 6.), (2., 8.), (0., 5.)];
    const DOWN: Ring = &[(0., 5.), (2., 2.), (3., 4.), (0., 5.)];
    // A hole touching the shell at (0, 5) and one touching the shell at (5, 0), touching at (4, 4).
    const WEST: Ring = &[(0., 5.), (4., 4.), (3., 7.), (0., 5.)];
    const SOUTH: Ring = &[(5., 0.), (7., 3.), (4., 4.), (5., 0.)];

    fn disconnected(part: usize) -> Vec<Problem> {
        vec![Problem::DisconnectedInterior { part }]
    }

    #[test]
    fn rings_touching_at_points_may_split_the_interior() {
        // A hole touching the shell at two points, on its edges or at its corners.
        assert_eq!(problems(&poly(&[SQUARE, ACROSS])), disconnected(0));
        assert_eq!(problems(&poly(&[SQUARE, CORNERS])), disconnected(0));
        // Touch points one above the other (same x), and one beside the other (same y): two points.
        let down_across: Ring = &[(5., 0.), (8., 5.), (5., 10.), (2., 5.), (5., 0.)];
        assert_eq!(problems(&poly(&[SQUARE, down_across])), disconnected(0));
        assert_eq!(described(&poly(&[SQUARE, ACROSS])), ["the interior of part 1 is split into pieces by rings touching at points"]);
        // Three holes pairwise touching around a piece of the interior.
        assert_eq!(problems(&poly(&[SQUARE, LEFT, BELOW, RIGHT])), disconnected(0));
        // Shell, hole, hole and back to the shell: the corner at (0, 0) is cut off.
        assert_eq!(problems(&poly(&[SQUARE, WEST, SOUTH])), disconnected(0));
        // Reported for the part it is in, and the other parts still checked.
        let far: Ring = &[(20., 0.), (30., 0.), (30., 10.), (20., 0.)];
        assert_eq!(problems(&multi(&[&[far], &[SQUARE, ACROSS]])), disconnected(1));
        let over: Ring = &[(5., 5.), (15., 5.), (15., 15.), (5., 15.), (5., 5.)];
        assert_eq!(
            problems(&multi(&[&[SQUARE, ACROSS], &[over]])),
            [Problem::DisconnectedInterior { part: 0 }, Problem::PartsOverlap { a: 0, b: 1, area: true }]
        );
        assert!(!check(Some(&poly(&[SQUARE, ACROSS])), MAX_WORK).unrelated);
    }

    /// A hole whose boundary runs along the shell's shares stretches of boundary with it: rings
    /// meeting on a line, as OGC and geo call it, even a hole that is the whole shell. (Their areas
    /// always share space: a hole is inside its shell. Overlap is for two holes, or two parts.)
    #[test]
    fn a_hole_along_the_shell_touches_it_along_a_line() {
        let along: Ring = &[(0., 0.), (5., 0.), (5., 5.), (0., 5.), (0., 0.)];
        for hole in [along, SQUARE] {
            let g = poly(&[SQUARE, hole]);
            assert_eq!(problems(&g), [Problem::RingsIntersect { part: 0, a: 0, b: 1, area: false }]);
            assert_eq!(described(&g), ["exterior ring and ring 2 of part 1 touch along a line"]);
            let geo_says: Vec<String> =
                geo::Polygon::new(closed_ring(&ring(SQUARE), ANY).unwrap(), vec![closed_ring(&ring(hole), ANY).unwrap()])
                    .validation_errors()
                    .iter()
                    .map(ToString::to_string)
                    .collect();
            assert_eq!(geo_says, ["exterior ring and interior ring at index 0 intersect on a line"], "{hole:?}");
        }
    }

    #[test]
    fn rings_touching_without_splitting_the_interior_are_fine() {
        // One hole touching the shell once; two holes touching each other once.
        assert_eq!(problems(&poly(&[SQUARE, AT_SHELL])), []);
        assert_eq!(problems(&poly(&[SQUARE, HOLE, &[(4., 4.), (6., 4.), (6., 6.), (4., 4.)]])), []);
        // Two of the three holes around the triangle: a chain, not a loop.
        assert_eq!(problems(&poly(&[SQUARE, LEFT, BELOW])), []);
        assert_eq!(problems(&poly(&[SQUARE, WEST])), []);
        assert_eq!(problems(&poly(&[SQUARE, WEST, &[(4., 4.), (7., 3.), (6., 6.), (4., 4.)]])), []);
        // Three rings meeting at one point, at the shell or inside it, are a star.
        assert_eq!(problems(&poly(&[SQUARE, UP, DOWN])), []);
        let around: [Ring; 3] = [
            &[(5., 5.), (8., 5.), (7., 8.), (5., 5.)],
            &[(5., 5.), (4., 8.), (2., 6.), (5., 5.)],
            &[(5., 5.), (3., 2.), (7., 2.), (5., 5.)],
        ];
        assert_eq!(problems(&poly(&[SQUARE, around[0], around[1], around[2]])), []);
        // A touch point found as 0 on the shell and -0 on the hole is one point.
        let shell: Ring = &[(0., 0.), (10., 0.), (10., 10.), (0., 10.), (0., 5.), (0., 0.)];
        assert_eq!(problems(&poly(&[shell, &[(-0., 5.), (3., 4.), (3., 6.), (-0., 5.)]])), []);
        // Parts touching each other at two points, and an island touching its hole at four: only
        // rings of the same part split its interior.
        let chevron: Ring = &[(10., 0.), (20., 5.), (10., 10.), (15., 5.), (10., 0.)];
        assert_eq!(problems(&multi(&[&[SQUARE], &[chevron]])), []);
        let diamond: Ring = &[(3., 2.), (4., 3.), (3., 4.), (2., 3.), (3., 2.)];
        assert_eq!(problems(&multi(&[&[SQUARE, HOLE], &[diamond]])), []);
        assert_eq!(problems(&multi(&[&[SQUARE, AT_SHELL], &[&[(3., 4.), (2., 5.), (3., 6.), (1., 5.), (3., 4.)]]])), []);
    }

    /// Each point where a ring touches another ring of its part is charged as work, once.
    #[test]
    fn touch_points_are_charged_as_work() {
        let g = poly(&[SQUARE, ACROSS]);
        let pairs = {
            let segs: Vec<Seg> = [SQUARE, ACROSS]
                .iter()
                .enumerate()
                .flat_map(|(r, pts)| {
                    let l = closed_ring(&ring(pts), ANY).unwrap();
                    let n = l.0.len() - 1;
                    l.lines().enumerate().map(|(i, l)| Seg { a: l.start, b: l.end, part: 0, ring: r, i, n }).collect::<Vec<_>>()
                })
                .collect();
            sweep_order(&segs, ANY).unwrap().2
        };
        // The coordinates, the sweep, the pair of rings in contact, both rings at both points, and
        // relating them.
        let coords = 10 * VERTEX_COST;
        let needed = coords + pairs + CONTACT_COST + 4 * CONTACT_COST + (5 + 5) * RELATE_COST;
        assert_eq!(check(Some(&g), needed).problems, disconnected(0));
        assert_eq!(check(Some(&g), needed - 1).codes(), [Code::TooComplex]);
        // Short of the touch points' share, too: never valid.
        assert_eq!(check(Some(&g), coords + pairs + CONTACT_COST + 3 * CONTACT_COST).codes(), [Code::TooComplex]);
    }

    /// Pairs of holes whose boxes meet are held until they are checked, so each one kept is charged
    /// like a contact: a feature with many such pairs is too complex before it fills memory.
    #[test]
    fn box_pairs_kept_are_charged_as_work() {
        // Twenty nested squares inside a shell: none touch, and all their boxes meet (190 pairs).
        let holes: Vec<Vec<(f64, f64)>> = (1..=20)
            .map(|k| {
                let (lo, hi) = (50. - f64::from(k), 50. + f64::from(k));
                vec![(lo, lo), (hi, lo), (hi, hi), (lo, hi), (lo, lo)]
            })
            .collect();
        let shell: Ring = &[(0., 0.), (100., 0.), (100., 100.), (0., 100.), (0., 0.)];
        let rings: Vec<Ring> = std::iter::once(shell).chain(holes.iter().map(Vec::as_slice)).collect();
        let g = poly(&rings);
        let complete = |budget: u64| !check(Some(&g), budget).codes().contains(&Code::TooComplex);
        assert!(complete(MAX_WORK));
        assert!(check(Some(&g), MAX_WORK).codes().contains(&Code::RingsIntersect), "nested holes overlap");
        // The least budget that checks it fully.
        let (mut lo, mut hi) = (0, MAX_WORK);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if complete(mid) { hi = mid } else { lo = mid + 1 }
        }
        assert!(lo >= 190 * CONTACT_COST, "needed {lo}");
    }

    /// The checks agree with geo's `Validation` on polygons without repeated vertices or rings
    /// meeting along a line (which geo also flags) — except where OGC rules are stricter than
    /// geo's: none of these.
    #[test]
    fn agrees_with_geos_validation() {
        let shapes: Vec<Vec<Vec<Ring>>> = vec![
            vec![vec![SQUARE]],
            vec![vec![BOWTIE]],
            vec![vec![SQUARE, HOLE]],
            vec![vec![SQUARE, &[(20., 20.), (21., 20.), (21., 21.), (20., 20.)]]],
            vec![vec![SQUARE, &[(8., 2.), (12., 2.), (12., 4.), (8., 4.), (8., 2.)]]],
            vec![vec![SQUARE, HOLE, &[(3., 3.), (5., 3.), (5., 5.), (3., 5.), (3., 3.)]]],
            vec![vec![SQUARE, HOLE, &[(4., 4.), (6., 4.), (6., 6.), (4., 4.)]]],
            vec![vec![&[(0., 0.), (4., 0.), (2., 2.), (4., 4.), (0., 4.), (2., 2.), (0., 0.)]]],
            vec![vec![SQUARE], vec![&[(5., 5.), (15., 5.), (15., 15.), (5., 15.), (5., 5.)]]],
            vec![vec![SQUARE], vec![&[(10., 0.), (20., 0.), (20., 10.), (10., 10.), (10., 0.)]]],
            vec![vec![SQUARE], vec![&[(10., 10.), (20., 10.), (20., 20.), (10., 10.)]]],
            vec![vec![SQUARE, HOLE], vec![&[(2.5, 2.5), (3.5, 2.5), (3.5, 3.5), (2.5, 2.5)]]],
            // Rings touching at points without splitting the interior.
            vec![vec![SQUARE, AT_SHELL]],
            vec![vec![SQUARE, UP, DOWN]],
            vec![vec![SQUARE, LEFT, BELOW]],
            vec![vec![SQUARE], vec![&[(10., 0.), (20., 5.), (10., 10.), (15., 5.), (10., 0.)]]],
            vec![vec![SQUARE, HOLE], vec![&[(3., 2.), (4., 3.), (3., 4.), (2., 3.), (3., 2.)]]],
        ];
        for shape in shapes {
            let g = multi(&shape.iter().map(Vec::as_slice).collect::<Vec<_>>());
            let mp = geo::MultiPolygon::new(
                shape
                    .iter()
                    .map(|rings| {
                        let mut rings = rings.iter().map(|r| geo::LineString::from(r.to_vec()));
                        geo::Polygon::new(rings.next().unwrap(), rings.collect())
                    })
                    .collect(),
            );
            assert_eq!(problems(&g).is_empty(), mp.is_valid(), "{shape:?}: {:?} vs {:?}", problems(&g), mp.validation_errors());
        }
    }

    /// geo's `Validation` takes a polygon whose interior is split by rings touching at points for
    /// valid, which OGC (6.1.11.1) doesn't: these are the cases this check adds.
    #[test]
    fn finds_split_interiors_geos_validation_misses() {
        for rings in [vec![SQUARE, ACROSS], vec![SQUARE, CORNERS], vec![SQUARE, LEFT, BELOW, RIGHT], vec![SQUARE, WEST, SOUTH]] {
            let mut geo_rings = rings.iter().map(|r| geo::LineString::from(r.to_vec()));
            let polygon = geo::Polygon::new(geo_rings.next().unwrap(), geo_rings.collect());
            assert!(polygon.is_valid(), "{rings:?}: {:?}", polygon.validation_errors());
            assert_eq!(problems(&poly(&rings)), disconnected(0), "{rings:?}");
        }
    }
}
