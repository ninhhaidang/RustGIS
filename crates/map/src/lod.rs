//! Thinned display copies of a layer's geometry, for drawing it zoomed out.
//!
//! A detailed layer seen whole puts many vertices in each pixel: walking them all on every frame
//! makes panning and zooming stall. A thinned copy is simplified with Douglas–Peucker, keeping
//! only the vertices its shape needs to stay within a set distance of the original (#1750), so
//! the map draws from far fewer vertices and looks the same. Copies are made per level (a
//! tolerance of 2^level projection units) on first use and kept, a few at a time; the renderer
//! picks the level just under a device pixel at the view's zoom. A level is built on several
//! threads ([`rustgis_data::threads`]), each row on its own, so it is the same on any number.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use rustgis_data::{Coord, Geometry};

/// The tolerance, in device pixels, a thinned copy keeps within.
pub const THIN_PX: f64 = 1.0;

/// Extrusion footprint tolerance in logical pixels, kept tighter than flat display thinning.
pub const EXTRUSION_THIN_PX: f64 = 0.3;

/// Layers with fewer line and ring vertices than this are always drawn from the full geometry.
pub const THIN_ABOVE: usize = 200_000;

/// A level is kept as a copy only when it drops at least this share of the vertices; otherwise
/// it draws from the full geometry (zoomed in past the vertex spacing, or many tiny features).
pub const MIN_SAVING: f64 = 0.25;

/// How many levels a layer keeps.
pub const LEVELS_KEPT: usize = 3;

/// Additional geometry retained for display levels per layer, excluding original/projected data.
pub const CACHE_BYTES: usize = 64 * 1024 * 1024;

/// The most rows a thread takes at a time while it builds a level ([`map_budgeted`]); fewer, so
/// each thread takes several turns, in a layer of few rows.
const CHUNK_ROWS: usize = 256;

/// A level of fewer vertices than this is built on the calling thread alone.
const THREADS_FROM_VERTICES: usize = 50_000;

/// What building a row takes beside its result, per vertex: each run projected (16 bytes a vertex),
/// which vertices to keep, and the kept ones, before the row is counted in the budget.
const SCRATCH_PER_VERTEX: usize = 48;

/// And per part (a line's run, a polygon's ring or polygon), with or without vertices: the list
/// the row's copy holds it in.
const SCRATCH_PER_PART: usize = std::mem::size_of::<Vec<Coord>>();

/// Inputs of an unsuccessful display attempt, so unchanged frames do not repeat the work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Rejected {
    pub revision: u64,
    pub level: i32,
    pub rows: usize,
    pub budget: usize,
    pub whole_layer_eligible: bool,
}

/// Why [`display`] made no level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refused {
    /// Too few vertices to thin, or past the budget.
    Unfit,
    /// Built from rows on the map as `Rows` has them, and found to drop fewer than
    /// [`MIN_SAVING`] of the vertices: a copy built the same way, by [`project_budgeted`], would
    /// too.
    SavesTooLittle(Rows),
}

#[cfg(test)]
thread_local! {
    /// Counts display construction on this test thread, including candidates later rejected.
    pub(crate) static DISPLAY_BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Counts the levels [`project_budgeted`] builds on this test thread, kept or not.
    pub(crate) static LEVEL_BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// One bounded display level, independent of the full geometry used for interaction.
#[derive(Debug)]
pub(crate) struct Display {
    pub revision: u64,
    pub level: i32,
    pub whole_layer_eligible: bool,
    pub geometries: Vec<Option<Geometry>>,
    pub extents: Vec<rustgis_geom::Extent>,
    pub bytes: usize,
}

/// A display copy in `flat`, simplified to `tol` map units. In Web Mercator each line and ring is
/// projected on its own and then simplified, so the full projected geometry is never built; other
/// projections are projected whole first (a seam can cut a ring). Points the projection can't
/// show (not finite) are kept and still break paths.
pub fn project_thinned(g: &Geometry, flat: &crate::projection::Flat, tol: f64) -> Geometry {
    if !tol.is_finite() || tol <= 0.0 {
        return flat.project(g);
    }
    if !matches!(flat, crate::projection::Flat::Mercator) {
        return thin(&flat.project(g), tol);
    }
    // Each line and ring is projected on its own and simplified in map units, so the error bound
    // holds on the map exactly, and the whole projected geometry is never built.
    simplify_mapped(g, tol, Some(&|c: Coord| flat.forward_checked(c).unwrap_or(Coord::new(f64::NAN, f64::NAN))))
}

/// Where a display level's rows are on the map ([`display`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Rows {
    /// In longitude/latitude, on a Web Mercator map: projected as they are simplified.
    LonLat,
    /// In the map's own coordinates (a layer held in the system the map is drawn in): simplified
    /// as they are, with nothing converted.
    MapUnits,
}

/// Builds a display level without ever constructing the full projected geometry: from rows in
/// longitude/latitude on a Web Mercator map, or from rows already in the map's coordinates
/// ([`Rows`]). Original extents are transformed separately so thinning cannot change culling or
/// splats. Both the geometry and its extent table fit in the optional display-cache budget.
pub(crate) fn display(
    features: &[rustgis_data::Feature],
    rows: Rows,
    revision: u64,
    level: i32,
    budget: usize,
    whole_layer_eligible: bool,
) -> Result<Display, Refused> {
    #[cfg(test)]
    DISPLAY_BUILDS.set(DISPLAY_BUILDS.get() + 1);
    let eligible: usize = features.iter().filter_map(|f| f.geometry.as_ref()).map(thinnable).sum();
    if !whole_layer_eligible && eligible <= THIN_ABOVE {
        return Err(Refused::Unfit);
    }
    let extent_bytes = features.len().checked_mul(std::mem::size_of::<rustgis_geom::Extent>()).ok_or(Refused::Unfit)?;
    let budget = budget.checked_sub(extent_bytes).ok_or(Refused::Unfit)?;
    let geometries = project_budgeted(features, rows, level, budget).ok_or(Refused::Unfit)?;
    let all: usize = features.iter().filter_map(|f| f.geometry.as_ref()).map(vertices).sum();
    let kept: usize = geometries.iter().flatten().map(vertices).sum();
    if !whole_layer_eligible && kept as f64 > all as f64 * (1.0 - MIN_SAVING) {
        return Err(Refused::SavesTooLittle(rows));
    }
    // Mercator is monotone in both axes; two opposite corners bound every source vertex. Rows in
    // map coordinates already have their extents there.
    let extents = features
        .iter()
        .map(|f| {
            let e = f.geometry.as_ref().map_or(rustgis_geom::Extent::EMPTY, Geometry::extent);
            if e.is_empty() || rows == Rows::MapUnits {
                e
            } else {
                rustgis_geom::Extent::from_points(
                    rustgis_geom::lonlat_to_merc(Coord::new(e.min_x, e.min_y)),
                    rustgis_geom::lonlat_to_merc(Coord::new(e.max_x, e.max_y)),
                )
            }
        })
        .collect();
    let bytes = cache_bytes(&geometries).checked_add(extent_bytes).ok_or(Refused::Unfit)?;
    Ok(Display { revision, level, whole_layer_eligible, geometries, extents, bytes })
}

/// Builds only display geometry within `budget`: Mercator from rows in longitude/latitude, the
/// rows simplified as they are when they are in the map's coordinates ([`Rows`]). Callers apply
/// their own eligibility and saving rules; exact consumers already have extents and need no second
/// extent table.
pub(crate) fn project_budgeted(features: &[rustgis_data::Feature], rows: Rows, level: i32, budget: usize) -> Option<Vec<Option<Geometry>>> {
    #[cfg(test)]
    LEVEL_BUILDS.set(LEVEL_BUILDS.get() + 1);
    let tol = tolerance(level);
    map_budgeted(
        features,
        budget,
        |f| f.geometry.as_ref(),
        |g| match rows {
            Rows::LonLat => project_thinned(g, &crate::projection::Flat::Mercator, tol),
            Rows::MapUnits => simplify(g, tol),
        },
    )
}

/// Heap allocation held by a geometry's vectors (the enum itself is in its containing array).
fn heap_bytes(g: &Geometry) -> usize {
    rustgis_data::source::memory::geometry_bytes(g)
}

pub(crate) fn cache_bytes(geoms: &Vec<Option<Geometry>>) -> usize {
    geoms
        .iter()
        .flatten()
        .fold(geoms.capacity().saturating_mul(std::mem::size_of::<Option<Geometry>>()), |n, g| n.saturating_add(heap_bytes(g)))
}

/// Build one level without first allocating a whole window's extra geometry. Running out of
/// budget discards only this optional display cache; drawing still uses the full projection.
pub(crate) fn thin_budgeted(geoms: crate::shapes::Shapes<'_>, tol: f64, budget: usize) -> Option<Vec<Option<Geometry>>> {
    match geoms {
        // A layer's own rows, read in place: no list of them beside the level.
        crate::shapes::Shapes::Rows(_) => map_budgeted_at(geoms.len(), budget, |row| geoms.get(row), |g| thin(g, tol)),
        crate::shapes::Shapes::Copies(c) => map_budgeted(c, budget, Option::as_ref, |g| thin(g, tol)),
    }
}

/// `build` of each item's geometry (`geometry`), in order ([`map_budgeted_rows`]), on up to the
/// user's thread count ([`rustgis_data::threads`]): one below [`THREADS_FROM_VERTICES`], and no
/// more than [`rustgis_data::memory::thread_scratch`] holds, beyond one thread's, with each thread
/// building the biggest row at once ([`row_scratch`]).
fn map_budgeted<T: Sync>(
    items: &[T],
    budget: usize,
    geometry: impl Fn(&T) -> Option<&Geometry> + Sync,
    build: impl Fn(&Geometry) -> Geometry + Sync,
) -> Option<Vec<Option<Geometry>>> {
    map_budgeted_at(items.len(), budget, |i| geometry(&items[i]), build)
}

/// [`map_budgeted`] of rows `0..len`, each row's geometry read by `geometry` where it is.
fn map_budgeted_at<'g>(
    len: usize,
    budget: usize,
    geometry: impl Fn(usize) -> Option<&'g Geometry> + Sync,
    build: impl Fn(&Geometry) -> Geometry + Sync,
) -> Option<Vec<Option<Geometry>>> {
    let (total, biggest) =
        (0..len).filter_map(&geometry).fold((0usize, 0usize), |(t, b), g| (t.saturating_add(vertices(g)), b.max(row_scratch(g))));
    let threads = threads_for(total, biggest, rustgis_data::threads::for_items(len), rustgis_data::memory::thread_scratch());
    map_budgeted_rows(len, budget, threads, |row| geometry(row).map(&build))
}

/// The threads to build a level of `total` vertices on, the biggest row taking `biggest` bytes to
/// build ([`row_scratch`]): at most `most`, one below [`THREADS_FROM_VERTICES`], and as many as
/// `scratch` bytes hold beyond one thread's, each building a row as big as the biggest.
fn threads_for(total: usize, biggest: usize, most: usize, scratch: usize) -> usize {
    if total < THREADS_FROM_VERTICES {
        return 1;
    }
    let affordable = (scratch / biggest.max(1)).saturating_add(1);
    most.min(affordable).max(1)
}

/// What building a level's copy of `g` takes before the row is counted in the budget: its
/// vertices ([`SCRATCH_PER_VERTEX`]) and its parts, empty ones too ([`SCRATCH_PER_PART`]).
fn row_scratch(g: &Geometry) -> usize {
    let parts = match g {
        Geometry::Point(_) | Geometry::MultiPoint(_) => 1,
        Geometry::Line(parts) => parts.len(),
        Geometry::Polygon(polys) => polys.iter().map(|rings| rings.len().saturating_add(1)).fold(0, usize::saturating_add),
    };
    vertices(g).saturating_mul(SCRATCH_PER_VERTEX).saturating_add(parts.saturating_mul(SCRATCH_PER_PART))
}

/// [`map_budgeted_rows`] of `items`.
#[cfg(test)]
fn map_budgeted_on<T: Sync>(
    items: &[T],
    budget: usize,
    threads: usize,
    build: impl Fn(&T) -> Option<Geometry> + Sync,
) -> Option<Vec<Option<Geometry>>> {
    map_budgeted_rows(items.len(), budget, threads, |i| build(&items[i]))
}

/// `build` of each row of `0..len`, given its number, in order, on up to `threads` threads
/// taking turns at chunks of rows (at most [`CHUNK_ROWS`], and eight turns a thread where the
/// rows allow);
/// `None` when the copy (a slot a row, and each geometry's heap) takes more than `budget` bytes,
/// as one thread would find it. Each row is built on its own, so the copy is the same on any
/// number of threads. Past the budget the threads stop after the row they are on: at most one row
/// each beyond it is made, and dropped. A thread the system refuses costs speed: the others, and
/// this one, take its rows.
fn map_budgeted_rows(
    len: usize,
    budget: usize,
    threads: usize,
    build: impl Fn(usize) -> Option<Geometry> + Sync,
) -> Option<Vec<Option<Geometry>>> {
    let slots = len.checked_mul(std::mem::size_of::<Option<Geometry>>())?;
    let left = budget.checked_sub(slots)?;
    let mut copy: Vec<Option<Geometry>> = Vec::with_capacity(len);
    copy.resize_with(len, || None);
    let (used, over, next) = (AtomicUsize::new(0), AtomicBool::new(false), AtomicUsize::new(0));
    type Chunk<'a> = Mutex<Option<(usize, &'a mut [Option<Geometry>])>>;
    let rows = len.div_ceil(threads.max(1).saturating_mul(8)).clamp(1, CHUNK_ROWS);
    let chunks: Vec<Chunk> = copy.chunks_mut(rows).enumerate().map(|(i, out)| Mutex::new(Some((i * rows, out)))).collect();
    let work = || {
        while !over.load(Ordering::Relaxed) {
            let Some(chunk) = chunks.get(next.fetch_add(1, Ordering::Relaxed)) else { break };
            // Never held while a row is built, so never poisoned; if it were, its rows still are.
            let Some((first, out)) = chunk.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() else { continue };
            for (row, out) in (first..).zip(out) {
                let g = build(row);
                let bytes = g.as_ref().map_or(0, heap_bytes);
                // Bytes held in memory: their sum fits.
                if used.fetch_add(bytes, Ordering::Relaxed).saturating_add(bytes) > left {
                    over.store(true, Ordering::Relaxed);
                    return;
                }
                *out = g;
            }
        }
    };
    if threads <= 1 || chunks.len() <= 1 {
        work();
    } else {
        std::thread::scope(|scope| {
            for _ in 1..threads.min(chunks.len()) {
                let _ = std::thread::Builder::new().name("rustgis-lod".into()).spawn_scoped(scope, work);
            }
            work();
        });
    }
    drop(chunks);
    (!over.load(Ordering::Relaxed)).then_some(copy)
}

/// The level for a tolerance in projection units: the largest whose tolerance (2^level) is at
/// most `tol`. `None` for a tolerance that is not a positive number.
pub fn level(tol: f64) -> Option<i32> {
    (tol.is_finite() && tol > 0.0).then(|| tol.log2().floor().clamp(-60.0, 60.0) as i32)
}

/// A level's tolerance in projection units.
pub fn tolerance(level: i32) -> f64 {
    2f64.powi(level)
}

/// A display copy of `g` within `tol` projection units: [`simplify`] (Douglas–Peucker).
pub fn thin(g: &Geometry, tol: f64) -> Geometry {
    simplify(g, tol)
}

/// A copy of `g` simplified by Douglas–Peucker (Douglas and Peucker, "Algorithms for the
/// reduction of the number of points required to represent a digitized line or its caricature",
/// The Canadian Cartographer 10(2), 1973): along each line and ring, every dropped vertex is
/// within `tol` (projection units, Euclidean) of the simplified line, and the first and last are
/// kept. Unlike distance thinning, which keeps a vertex every `tol` along a shape, it keeps only
/// the ones its shape needs, so a detailed coastline keeps far fewer at the same error. A ring
/// keeps its extreme vertices (so its extent) and at least a triangle (or stays whole when it has
/// no more; a ring with no area keeps two of its vertices, one repeated), so a small polygon still
/// has an area to fill; a point the projection couldn't show (not finite) is kept, and the
/// stretches either side of it are simplified apart. A tolerance that is not a positive number
/// keeps `g` as it is.
pub fn simplify(g: &Geometry, tol: f64) -> Geometry {
    if !tol.is_finite() || tol <= 0.0 {
        return g.clone();
    }
    simplify_mapped(g, tol, None)
}

/// [`simplify`] after mapping each coordinate through `f` (a projection), one line or ring at a
/// time; `None` keeps them as they are. `tol` must be a positive number.
fn simplify_mapped(g: &Geometry, tol: f64, f: Option<&dyn Fn(Coord) -> Coord>) -> Geometry {
    fn mapped<'a>(run: &'a [Coord], f: Option<&dyn Fn(Coord) -> Coord>) -> std::borrow::Cow<'a, [Coord]> {
        match f {
            Some(f) => run.iter().map(|c| f(*c)).collect::<Vec<_>>().into(),
            None => run.into(),
        }
    }
    match g {
        Geometry::Point(_) | Geometry::MultiPoint(_) => match f {
            Some(f) => g.map_coords(&f),
            None => g.clone(),
        },
        Geometry::Line(parts) => Geometry::Line(parts.iter().map(|p| simplify_run(&mapped(p, f), tol)).collect()),
        Geometry::Polygon(polys) => {
            Geometry::Polygon(polys.iter().map(|rings| rings.iter().map(|r| simplify_ring(&mapped(r, f), tol)).collect()).collect())
        }
    }
}

/// One ring simplified, kept to at least a closed triangle.
fn simplify_ring(ring: &[Coord], tol: f64) -> Vec<Coord> {
    let keep = keep_mask(ring, tol, true);
    let kept = keep.iter().filter(|k| **k).count();
    let n = ring.len();
    if kept >= 4 || n <= 4 {
        return if kept >= 4 { collect_kept(ring, &keep) } else { ring.to_vec() };
    }
    // Too few kept (all of the ring is within `tol` of them): grow them into a triangle, each time
    // adding the vertex farthest from what is kept. A triangle has every pair of its corners as an
    // edge, so the vertices are still within `tol` of it.
    let mut corners: Vec<usize> = (0..n - 1).filter(|&i| keep.get(i).copied().unwrap_or(false)).collect();
    while corners.len() < 3 {
        let from = |c: Coord| match corners.as_slice() {
            [a] => ring.get(*a).map_or(f64::NAN, |a| (c.x - a.x).hypot(c.y - a.y)),
            [a, b] => match (ring.get(*a), ring.get(*b)) {
                (Some(a), Some(b)) => segment_distance(c, *a, *b),
                _ => f64::NAN,
            },
            _ => f64::NAN,
        };
        let far = (0..n - 1)
            .filter(|i| !corners.contains(i))
            .filter_map(|i| ring.get(i).filter(|c| c.is_finite()).map(|c| (i, from(*c))))
            .fold(None, |best: Option<(usize, f64)>, (i, d)| if d > best.map_or(0.0, |b| b.1) { Some((i, d)) } else { best });
        // Every vertex on what is kept already (a ring with no area, such as an out-and-back
        // sliver): the kept vertices, the last repeated, rather than the whole ring.
        let Some((i, _)) = far else {
            let Some(&last) = corners.last() else { return ring.to_vec() };
            corners.resize(3, last);
            break;
        };
        corners.push(i);
    }
    corners.sort_unstable();
    let mut out: Vec<Coord> = corners.iter().filter_map(|&i| ring.get(i).copied()).collect();
    out.extend(ring.last().copied());
    out
}

/// The kept vertices of `run`, in order.
fn collect_kept(run: &[Coord], keep: &[bool]) -> Vec<Coord> {
    run.iter().zip(keep).filter_map(|(c, k)| k.then_some(*c)).collect()
}

/// The vertices of one line kept by Douglas–Peucker ([`keep_mask`]).
fn simplify_run(run: &[Coord], tol: f64) -> Vec<Coord> {
    collect_kept(run, &keep_mask(run, tol, false))
}

/// Which vertices of one line or ring Douglas–Peucker keeps, each finite stretch on its own. With
/// `extremes`, its leftmost, rightmost, lowest and highest vertices are kept too, so a simplified
/// ring has the same extent as the original: culling, and drawing a tiny polygon as its box, go
/// by the extent of the full geometry.
fn keep_mask(run: &[Coord], tol: f64, extremes: bool) -> Vec<bool> {
    let mut keep = vec![false; run.len()];
    let mark = |keep: &mut [bool], i: usize| {
        if let Some(k) = keep.get_mut(i) {
            *k = true;
        }
    };
    if extremes {
        let mut most: [Option<(usize, f64)>; 4] = [None; 4];
        for (i, c) in run.iter().enumerate().filter(|(_, c)| c.is_finite()) {
            for (slot, v) in most.iter_mut().zip([-c.x, c.x, -c.y, c.y]) {
                if slot.is_none_or(|(_, best)| v > best) {
                    *slot = Some((i, v));
                }
            }
        }
        for (i, _) in most.into_iter().flatten() {
            mark(&mut keep, i);
        }
    }
    // Douglas–Peucker runs between the vertices kept already, within each finite stretch.
    let mut start: Option<usize> = None;
    for (i, c) in run.iter().enumerate() {
        if !c.is_finite() {
            mark(&mut keep, i);
            start = None;
            continue;
        }
        let forced = keep.get(i).copied().unwrap_or(false);
        match start {
            None => start = Some(i),
            Some(s) if forced => {
                douglas_peucker(run, s, i, tol, &mut keep);
                start = Some(i);
            }
            Some(_) => {}
        }
        if run.get(i + 1).is_none_or(|n| !n.is_finite()) {
            match start {
                Some(s) if s < i => douglas_peucker(run, s, i, tol, &mut keep),
                _ => mark(&mut keep, i),
            }
            start = None;
        }
    }
    keep
}

/// Marks the vertices of `run[first..=last]` (all finite) Douglas–Peucker keeps at `tol`. It works
/// from a stack, not by recursion, so a long run can't overflow the thread's stack, and its work is
/// bounded: Douglas–Peucker is quadratic on a shape that splits unevenly at every step (a zigzag),
/// so once it has worked out [`DP_WORK`] distances for each vertex of the run (on average), the
/// spans still open are thinned by distance instead (a vertex every `tol` along them), which
/// keeps every dropped vertex within `tol` too.
fn douglas_peucker(run: &[Coord], first: usize, last: usize, tol: f64, keep: &mut [bool]) {
    for i in [first, last] {
        if let Some(k) = keep.get_mut(i) {
            *k = true;
        }
    }
    let mut stack = vec![(first, last)];
    let mut budget = (last - first + 1).saturating_mul(DP_WORK);
    while let Some((a, b)) = stack.pop() {
        if b <= a + 1 {
            continue;
        }
        let (Some(&pa), Some(&pb)) = (run.get(a), run.get(b)) else { continue };
        if budget < b - a {
            // Out of work: keep a vertex each `tol` (|dx| + |dy|) from the last kept, the distance
            // rule thinning used before #1750.
            let mut kept = pa;
            for i in a + 1..b {
                let Some(&c) = run.get(i) else { break };
                if (c.x - kept.x).abs() + (c.y - kept.y).abs() >= tol {
                    if let Some(k) = keep.get_mut(i) {
                        *k = true;
                    }
                    kept = c;
                }
            }
            continue;
        }
        budget -= b - a;
        let mut far = (a, tol);
        for (i, c) in run.iter().enumerate().take(b).skip(a + 1) {
            let d = segment_distance(*c, pa, pb);
            if d > far.1 {
                far = (i, d);
            }
        }
        if far.0 != a {
            if let Some(k) = keep.get_mut(far.0) {
                *k = true;
            }
            stack.push((a, far.0));
            stack.push((far.0, b));
        }
    }
}

/// How many distances Douglas–Peucker may work out for each vertex of a run, on average, before it
/// thins the rest of the run by distance: an even split takes about log2 of the run's length
/// (under 32).
const DP_WORK: usize = 48;

/// The distance from `p` to the segment `a`–`b` (to `a` when they are the same point, as a
/// closed ring's ends are).
fn segment_distance(p: Coord, a: Coord, b: Coord) -> f64 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 { (((p.x - a.x) * dx + (p.y - a.y) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
    (p.x - (a.x + t * dx)).hypot(p.y - (a.y + t * dy))
}

/// The number of vertices thinning can drop: those of its lines and rings (points are drawn
/// as they are).
pub fn thinnable(g: &Geometry) -> usize {
    match g {
        Geometry::Point(_) | Geometry::MultiPoint(_) => 0,
        _ => vertices(g),
    }
}

/// The number of vertices in a geometry.
pub fn vertices(g: &Geometry) -> usize {
    match g {
        Geometry::Point(_) => 1,
        Geometry::MultiPoint(p) => p.len(),
        Geometry::Line(parts) => parts.iter().map(Vec::len).sum(),
        Geometry::Polygon(polys) => polys.iter().flatten().map(Vec::len).sum(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A layer of every kind of row, longitude/latitude: none, points, wavy lines and rings of a
    /// few to thousands of vertices.
    fn rows(n: usize) -> Vec<Option<Geometry>> {
        (0..n)
            .map(|i| {
                let (x, y) = ((i % 97) as f64 * 0.3 - 14.0, (i % 61) as f64 * 0.2 + 40.0);
                let len = 3 + (i * 7919) % if i % 50 == 0 { 5000 } else { 200 };
                let wave = |k: usize| {
                    let t = k as f64 / len as f64;
                    Coord::new(x + t * 0.2, y + (t * 40.0).sin() * 0.01 + ((k * 31) % 7) as f64 * 1e-5)
                };
                match i % 5 {
                    0 => None,
                    1 => Some(Geometry::Point(Coord::new(x, y))),
                    2 | 3 => Some(Geometry::Line(vec![(0..len).map(wave).collect(), vec![Coord::new(x, y), Coord::new(x + 0.1, y)]])),
                    _ => {
                        let mut ring: Vec<Coord> = (0..len)
                            .map(|k| {
                                let a = k as f64 / len as f64 * std::f64::consts::TAU;
                                Coord::new(x + a.cos() * 0.05 + ((k * 13) % 5) as f64 * 1e-5, y + a.sin() * 0.05)
                            })
                            .collect();
                        ring.push(ring[0]);
                        Some(Geometry::Polygon(vec![vec![ring]]))
                    }
                }
            })
            .collect()
    }

    /// What one thread builds, row by row, as levels were built before they took threads: the
    /// running bytes checked after each row.
    fn one_by_one(
        items: &[Option<Geometry>],
        budget: usize,
        build: impl Fn(&Option<Geometry>) -> Option<Geometry>,
    ) -> Option<Vec<Option<Geometry>>> {
        let mut bytes = items.len().checked_mul(std::mem::size_of::<Option<Geometry>>())?;
        let mut copy = Vec::with_capacity(items.len());
        for g in items {
            let g = build(g);
            bytes = bytes.checked_add(g.as_ref().map_or(0, heap_bytes))?;
            if bytes > budget {
                return None;
            }
            copy.push(g);
        }
        (bytes <= budget).then_some(copy)
    }

    /// A level is the same, row for row, on one thread or many, projected and thinned or thinned
    /// alone, and as levels were built one row at a time.
    #[test]
    fn a_level_is_the_same_on_one_thread_or_many() {
        let rows = rows(3000);
        let flat = crate::projection::Flat::Mercator;
        let project = |g: &Option<Geometry>| g.as_ref().map(|g| project_thinned(g, &flat, 40.0));
        let projected: Vec<Option<Geometry>> = rows.iter().map(|g| g.as_ref().map(|g| flat.project(g))).collect();
        let thinned = |g: &Option<Geometry>| g.as_ref().map(|g| thin(g, 40.0));
        for (items, build) in [(&rows, &project as &(dyn Fn(&Option<Geometry>) -> Option<Geometry> + Sync)), (&projected, &thinned)] {
            let one = map_budgeted_on(items, usize::MAX, 1, build).unwrap();
            assert_eq!(Some(&one), one_by_one(items, usize::MAX, build).as_ref());
            assert!(one.iter().flatten().map(vertices).sum::<usize>() < items.iter().flatten().map(vertices).sum::<usize>() / 2, "thinned");
            for threads in [2, 6] {
                assert!(map_budgeted_on(items, usize::MAX, threads, build).unwrap() == one, "{threads} threads");
            }
        }
    }

    /// A small level is built on one thread; a big one on as many as the scratch memory holds, each
    /// building a row as big as its biggest.
    #[test]
    fn threads_follow_the_vertices_and_the_scratch() {
        let line = |n: usize| Geometry::Line(vec![(0..n).map(|i| Coord::new(i as f64, 0.0)).collect()]);
        let row = row_scratch(&line(1_000_000));
        assert_eq!(row, 1_000_000 * SCRATCH_PER_VERTEX + SCRATCH_PER_PART);
        assert_eq!(threads_for(THREADS_FROM_VERTICES - 1, 480, 16, usize::MAX), 1);
        assert_eq!(threads_for(THREADS_FROM_VERTICES, 480, 16, usize::MAX), 16);
        assert_eq!(threads_for(10_000_000, row, 16, 3 * row), 4);
        assert_eq!(threads_for(10_000_000, row, 16, row / 2), 1);
        assert_eq!(threads_for(10_000_000, 0, 16, 0), 1);
        assert_eq!(threads_for(10_000_000, row, 1, usize::MAX), 1);
        // Parts count without vertices: a row of 700,000 empty runs or rings takes as much to
        // build as one of about 350,000 vertices, and starts as few threads.
        let empty_runs = Geometry::Line(vec![Vec::new(); 700_000]);
        let empty_rings = Geometry::Polygon(vec![vec![Vec::new(); 700_000]]);
        assert_eq!(vertices(&empty_runs), 0);
        for g in [&empty_runs, &empty_rings] {
            assert!(row_scratch(g) >= 700_000 * SCRATCH_PER_PART);
            assert_eq!(threads_for(10_000_000, row_scratch(g), 16, 4 * row_scratch(g)), 5);
        }
    }

    /// A level over its budget is refused on many threads exactly where one thread refuses it: a
    /// byte short of what it takes.
    #[test]
    fn a_level_over_budget_is_refused_on_any_number_of_threads() {
        let rows = rows(3000);
        let thinned = |g: &Option<Geometry>| g.as_ref().map(|g| thin(g, 1e-4));
        let full = map_budgeted_on(&rows, usize::MAX, 1, thinned).unwrap();
        let takes = cache_bytes(&full);
        for threads in [1, 2, 6] {
            assert!(map_budgeted_on(&rows, takes, threads, thinned).is_some(), "{threads} threads");
            assert!(map_budgeted_on(&rows, takes - 1, threads, thinned).is_none(), "{threads} threads");
            assert!(map_budgeted_on(&rows, takes / 3, threads, thinned).is_none(), "{threads} threads");
            assert!(map_budgeted_on(&rows, 10, threads, thinned).is_none(), "{threads} threads");
        }
        assert!(one_by_one(&rows, takes, thinned).is_some() && one_by_one(&rows, takes - 1, thinned).is_none());
    }

    #[test]
    fn preprojection_thinning_bounds_mercator_error_at_all_latitudes() {
        let flat = crate::projection::Flat::Mercator;
        for (south, north) in [(-1.5, -1.0), (70.0, 85.0), (-90.0, 90.0), (89.0, 95.0)] {
            let run: Vec<_> = (0..10_000)
                .map(|i| {
                    let t = i as f64 / 9999.0;
                    Coord::new(36.0 + t * 0.5, south + (north - south) * t)
                })
                .collect();
            let original = Geometry::Line(vec![run]);
            let Geometry::Line(full) = flat.project(&original) else { panic!("line") };
            let Geometry::Line(kept) = project_thinned(&original, &flat, 10_000.0) else { panic!("line") };
            // Require savings only near the equator; a pole-spanning geometry may keep every
            // vertex under the larger polar bound. All runs must satisfy the error bound.
            if south == -1.5 {
                assert!(kept[0].len() < full[0].len() / 10);
            }
            assert_eq!(kept[0].first(), full[0].first());
            assert_eq!(kept[0].last(), full[0].last());
            // Each dropped vertex is within tolerance of the simplified line, on the map.
            assert!(within(&full[0], &kept[0], 10_000.0 + 1e-6), "{south}..{north}");
        }
    }

    /// A polar part must not force unrelated equatorial parts to retain their dense vertices.
    #[test]
    fn multipart_mercator_thinning_uses_each_runs_latitude_bound() {
        let equator: Vec<_> = (0..10_000).map(|i| Coord::new(i as f64 * 0.00001, 0.0)).collect();
        let polar = vec![Coord::new(1.0, 85.0), Coord::new(1.01, 85.0)];
        let flat = crate::projection::Flat::Mercator;
        let single = project_thinned(&Geometry::Line(vec![equator.clone()]), &flat, 100.0);
        let mixed = project_thinned(&Geometry::Line(vec![equator, polar]), &flat, 100.0);
        let (Geometry::Line(single), Geometry::Line(mixed)) = (single, mixed) else { panic!("lines") };
        assert_eq!(single[0], mixed[0]);
        assert!(mixed[0].len() < 200);
    }

    #[test]
    fn preprojection_preserves_breaks_holes_and_custom_projection_seams() {
        let flat = crate::projection::Flat::Mercator;
        let ring = vec![Coord::new(0.0, 0.0), Coord::new(0.01, 0.0), Coord::new(0.01, 0.01), Coord::new(0.0, 0.0)];
        let g = Geometry::Polygon(vec![vec![ring.clone(), ring]]);
        assert_eq!(project_thinned(&g, &flat, 1e6), flat.project(&g), "small rings and holes keep their area");
        let line = Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(f64::NAN, 1.0), Coord::new(0.001, 0.0)]]);
        let Geometry::Line(parts) = project_thinned(&line, &flat, 1e6) else { panic!("line") };
        assert_eq!(parts[0].len(), 3);
        assert!(parts[0][1].x.is_nan());
        for name in ["equalEarth", "northPolar", "EPSG:32633"] {
            let custom = crate::projection::Flat::Custom(std::sync::Arc::new(crate::projection::Projector::new(name).unwrap()));
            let g = Geometry::Line(vec![vec![Coord::new(170.0, 30.0), Coord::new(-170.0, 40.0)]]);
            let actual = project_thinned(&g, &custom, 1000.0);
            let expected = thin(&custom.project(&g), 1000.0);
            // Invalid coordinates compare through their serialized debug form, since NaN != NaN.
            assert_eq!(format!("{actual:?}"), format!("{expected:?}"), "{name}");
        }
        for tolerance in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(project_thinned(&g, &flat, tolerance), flat.project(&g));
        }
    }

    #[test]
    fn optional_levels_respect_heap_budget_and_leave_original_geometry_intact() {
        let original = vec![Some(Geometry::Polygon(vec![vec![vec![
            Coord::new(0.0, 0.0),
            Coord::new(10.0, 0.0),
            Coord::new(10.0, 10.0),
            Coord::new(0.0, 0.0),
        ]]]))];
        let before = original.clone();
        assert!(thin_budgeted(crate::Shapes::Copies(&original), 0.1, 0).is_none());
        assert!(thin_budgeted(crate::Shapes::Copies(&original), 0.1, std::mem::size_of::<Option<Geometry>>()).is_none());
        let copy = thin_budgeted(crate::Shapes::Copies(&original), 0.1, 1024).unwrap();
        assert!(cache_bytes(&copy) <= 1024);
        assert_eq!(copy, before);
        assert_eq!(original, before);
    }

    /// A layer's own rows are thinned in place: within the same budget as copies of them, with
    /// nothing beside the level counted, so a layer thinned from its copies is thinned from its
    /// rows too, at the smallest budget either takes.
    #[test]
    fn a_level_of_a_layers_own_rows_takes_the_budget_its_copies_take() {
        let square =
            Geometry::Polygon(vec![vec![vec![Coord::new(0.0, 0.0), Coord::new(10.0, 0.0), Coord::new(10.0, 10.0), Coord::new(0.0, 0.0)]]]);
        let mut ds = rustgis_data::Dataset::new("squares", rustgis_data::GeometryType::Polygon, vec![]);
        for _ in 0..3 {
            ds.push(Some(square.clone()), vec![]);
        }
        ds.push(None, vec![]);
        let copies: Vec<_> = (0..ds.features.len()).map(|row| ds.features.geometry(row).cloned()).collect();
        let takes = cache_bytes(&thin_budgeted(crate::Shapes::Copies(&copies), 0.1, usize::MAX).unwrap());
        let rows = crate::Shapes::Rows(&ds.features);
        assert_eq!(thin_budgeted(rows, 0.1, takes).unwrap(), thin_budgeted(crate::Shapes::Copies(&copies), 0.1, takes).unwrap());
        assert!(thin_budgeted(rows, 0.1, takes - 1).is_none());
    }

    #[test]
    fn thinning_keeps_ends_and_the_vertices_the_shape_needs() {
        // A straight run keeps its ends only, however long; a corner is kept.
        let run: Vec<Coord> = (0..=100).map(|i| Coord::new(i as f64 * 0.1, 0.0)).collect();
        let Geometry::Line(parts) = thin(&Geometry::Line(vec![run.clone()]), 1.0) else { panic!("a line") };
        assert_eq!(parts[0], vec![Coord::new(0.0, 0.0), Coord::new(10.0, 0.0)]);
        let mut bent = run;
        bent.push(Coord::new(10.0, 5.0));
        let Geometry::Line(parts) = thin(&Geometry::Line(vec![bent]), 1.0) else { panic!("a line") };
        assert_eq!(parts[0], vec![Coord::new(0.0, 0.0), Coord::new(10.0, 0.0), Coord::new(10.0, 5.0)]);
    }

    #[test]
    fn thinning_breaks_at_unprojectable_points() {
        let run =
            vec![Coord::new(0.0, 0.0), Coord::new(0.01, 0.0), Coord::new(f64::NAN, f64::NAN), Coord::new(0.02, 0.0), Coord::new(5.0, 0.0)];
        let Geometry::Line(parts) = thin(&Geometry::Line(vec![run]), 1.0) else { panic!("a line") };
        let kept = &parts[0];
        // Each side of the break keeps its own ends.
        assert_eq!(kept.len(), 5);
        assert!(kept[2].x.is_nan());
        assert_eq!((kept[1], kept[3]), (Coord::new(0.01, 0.0), Coord::new(0.02, 0.0)));
    }

    #[test]
    fn a_small_ring_keeps_an_area_to_fill() {
        // A ring under the tolerance keeps a triangle; one that is already a triangle stays whole.
        let ring = vec![Coord::new(0.0, 0.0), Coord::new(0.1, 0.0), Coord::new(0.1, 0.1), Coord::new(0.0, 0.1), Coord::new(0.0, 0.0)];
        let Geometry::Polygon(p) = thin(&Geometry::Polygon(vec![vec![ring.clone()]]), 1.0) else { panic!("a polygon") };
        assert_eq!(p[0][0], vec![Coord::new(0.0, 0.0), Coord::new(0.1, 0.0), Coord::new(0.1, 0.1), Coord::new(0.0, 0.0)]);
        let triangle = Geometry::Polygon(vec![p[0].clone()]);
        assert_eq!(thin(&triangle, 1.0), triangle);
        assert_eq!(thinnable(&Geometry::MultiPoint(ring)), 0);
    }

    /// Every dropped vertex of `full` is within `tol` of the simplified run `kept`.
    fn within(full: &[Coord], kept: &[Coord], tol: f64) -> bool {
        full.iter().all(|c| kept.windows(2).any(|w| segment_distance(*c, w[0], w[1]) <= tol + 1e-9))
    }

    #[test]
    fn simplifying_keeps_the_shape_within_tolerance_with_fewer_vertices_than_thinning() {
        // A wiggly coastline: a sine wave with a little noise, finely sampled.
        let run: Vec<Coord> =
            (0..20_000).map(|i| i as f64 * 0.01).map(|x| Coord::new(x, (x * 0.3).sin() * 20.0 + ((x * 37.0).sin() * 0.05))).collect();
        let g = Geometry::Line(vec![run.clone()]);
        let Geometry::Line(dp) = simplify(&g, 0.5) else { panic!("a line") };
        // What distance thinning (#1066, a vertex kept every 0.5 along the run) used to keep.
        let mut last = run[0];
        let thinned = 1 + run
            .iter()
            .filter(|c| {
                (c.x - last.x).abs() + (c.y - last.y).abs() >= 0.5 && {
                    last = **c;
                    true
                }
            })
            .count();
        assert_eq!(dp[0].first(), run.first());
        assert_eq!(dp[0].last(), run.last());
        assert!(within(&run, &dp[0], 0.5));
        assert!(dp[0].len() * 4 < thinned, "{} against {thinned}", dp[0].len());
        // A closed ring keeps its shape, and a tiny ring still a triangle to fill.
        let ring: Vec<Coord> = (0..=1000)
            .map(|i| f64::from(i) * std::f64::consts::TAU / 1000.0)
            .map(|t| Coord::new(t.cos() * 100.0, t.sin() * 100.0))
            .collect();
        let ring = [&ring[..1000], &ring[..1]].concat();
        let Geometry::Polygon(p) = simplify(&Geometry::Polygon(vec![vec![ring.clone()]]), 1.0) else { panic!("a polygon") };
        let kept = &p[0][0];
        assert!(kept.len() >= 4 && kept.len() < 100 && kept.first() == kept.last(), "{}", kept.len());
        assert!(within(&ring, kept, 1.0));
        let Geometry::Polygon(p) = simplify(&Geometry::Polygon(vec![vec![ring.clone()]]), 1e6) else { panic!("a polygon") };
        // Simplified past its size, the circle keeps its four extremes: its extent stays its own.
        let diamond = &p[0][0];
        assert_eq!(diamond.len(), 5);
        assert_eq!(diamond.first(), diamond.last());
        let twice: f64 = diamond.windows(2).map(|w| w[0].x * w[1].y - w[1].x * w[0].y).sum();
        assert!((twice.abs() / 2.0 - 20_000.0).abs() < 1.0, "{}", twice.abs() / 2.0);
        let extent = |r: &[Coord]| {
            r.iter().fold(rustgis_geom::Extent::EMPTY, |mut e, c| {
                e.expand(*c);
                e
            })
        };
        assert_eq!(extent(diamond), extent(&ring));
    }

    /// A ring whose extremes are two vertices grows from them into a triangle, still within the
    /// tolerance of every vertex.
    #[test]
    fn a_ring_with_two_extremes_grows_into_a_triangle() {
        let ring = vec![Coord::new(0.0, 0.0), Coord::new(5.0, 4.9), Coord::new(10.0, 10.0), Coord::new(5.0, 5.1), Coord::new(0.0, 0.0)];
        let Geometry::Polygon(p) = simplify(&Geometry::Polygon(vec![vec![ring.clone()]]), 1.0) else { panic!("a polygon") };
        let kept = &p[0][0];
        assert_eq!(kept, &vec![Coord::new(0.0, 0.0), Coord::new(5.0, 4.9), Coord::new(10.0, 10.0), Coord::new(0.0, 0.0)]);
        assert!(within(&ring, kept, 1.0));
    }

    /// A long sliver keeps its extremes (four vertices) and stays within the tolerance.
    #[test]
    fn a_sliver_ring_keeps_its_extremes() {
        let ring: Vec<Coord> = (0..=20)
            .map(|i| Coord::new(f64::from(i) * 0.5, f64::from(i) * 0.5 + 0.01))
            .chain((1..20).rev().map(|i| Coord::new(f64::from(i) * 0.5, f64::from(i) * 0.5 - 0.01)))
            .chain([Coord::new(0.0, 0.01)])
            .collect();
        let Geometry::Polygon(p) = simplify(&Geometry::Polygon(vec![vec![ring.clone()]]), 1.0) else { panic!("a polygon") };
        let kept = &p[0][0];
        assert_eq!(kept.len(), 4, "{kept:?}");
        assert_eq!(kept.first(), kept.last());
        assert!(kept.contains(&Coord::new(10.0, 10.01)), "the far corner stays: {kept:?}");
        assert!(within(&ring, kept, 1.0));
    }

    /// A ring with no area (out along a line and back) keeps four of its vertices, not all of them.
    #[test]
    fn an_out_and_back_ring_keeps_four_vertices() {
        let out: Vec<Coord> = (0..=50).map(|i| Coord::new(f64::from(i), 0.0)).collect();
        let ring: Vec<Coord> = out.iter().chain(out.iter().rev().skip(1)).copied().collect();
        let Geometry::Polygon(p) = simplify(&Geometry::Polygon(vec![vec![ring.clone()]]), 1.0) else { panic!("a polygon") };
        let kept = &p[0][0];
        assert_eq!(kept.len(), 4, "{kept:?}");
        assert_eq!(kept.first(), kept.last());
        assert!(kept.contains(&Coord::new(50.0, 0.0)));
        assert!(within(&ring, kept, 1.0));
    }

    #[test]
    fn simplifying_breaks_at_unprojectable_points_and_ignores_bad_tolerances() {
        let run = vec![
            Coord::new(0.0, 0.0),
            Coord::new(1.0, 0.0),
            Coord::new(2.0, 0.0),
            Coord::new(f64::NAN, f64::NAN),
            Coord::new(3.0, 0.0),
            Coord::new(4.0, 0.0),
            Coord::new(5.0, 0.0),
        ];
        let g = Geometry::Line(vec![run]);
        let Geometry::Line(parts) = simplify(&g, 0.1) else { panic!("a line") };
        let kept = &parts[0];
        assert_eq!(kept.len(), 5);
        assert!(kept[2].x.is_nan());
        assert_eq!((kept[1], kept[3]), (Coord::new(2.0, 0.0), Coord::new(3.0, 0.0)));
        for tol in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(format!("{:?}", simplify(&g, tol)), format!("{g:?}"));
        }
        let points = Geometry::MultiPoint(vec![Coord::new(0.0, 0.0), Coord::new(0.0, 0.0)]);
        assert_eq!(simplify(&points, 1.0), points);
        assert_eq!(
            simplify(&Geometry::Line(vec![vec![], vec![Coord::new(1.0, 1.0)]]), 1.0),
            Geometry::Line(vec![vec![], vec![Coord::new(1.0, 1.0)]])
        );
    }

    /// A long run is simplified without recursion: a zigzag that keeps every vertex.
    #[test]
    fn simplifying_a_long_run_does_not_overflow_the_stack() {
        let run: Vec<Coord> = (0..200_000).map(|i| Coord::new(f64::from(i), if i % 2 == 0 { 0.0 } else { 10.0 })).collect();
        let Geometry::Line(parts) = simplify(&Geometry::Line(vec![run.clone()]), 1.0) else { panic!("a line") };
        assert_eq!(parts[0].len(), 200_000);
        // One that splits as unevenly as it can at every step (each vertex a little farther out)
        // takes bounded work, and still keeps within the tolerance.
        let run: Vec<Coord> =
            (0..200_000).map(|i| Coord::new(f64::from(i), f64::from(i).powi(2) * 1e-6 * if i % 2 == 0 { 1.0 } else { -1.0 })).collect();
        let started = std::time::Instant::now();
        let Geometry::Line(parts) = simplify(&Geometry::Line(vec![run.clone()]), 0.5) else { panic!("a line") };
        assert!(started.elapsed().as_secs() < 20, "{:?}", started.elapsed());
        assert!(parts[0].first() == run.first() && parts[0].last() == run.last());
        // Every dropped vertex (one in a hundred checked, against the kept vertices either side of
        // it) is within the tolerance of the simplified line.
        let mut at = 0;
        for (i, c) in run.iter().enumerate() {
            if parts[0].get(at + 1) == Some(c) {
                at += 1;
            }
            if i % 100 == 0 {
                let next = parts[0].get(at + 1).unwrap_or(&parts[0][at]);
                assert!(segment_distance(*c, parts[0][at], *next) <= 0.5 + 1e-9, "vertex {i}");
            }
        }
    }

    #[test]
    fn levels_are_powers_of_two_under_the_tolerance() {
        assert_eq!(level(1.0), Some(0));
        assert_eq!(level(3.9), Some(1));
        assert_eq!(level(0.3), Some(-2));
        assert!(tolerance(level(1234.5).unwrap_or(0)) <= 1234.5);
        assert_eq!(level(0.0), None);
        assert_eq!(level(f64::NAN), None);
        assert_eq!(level(-1.0), None);
    }
}
