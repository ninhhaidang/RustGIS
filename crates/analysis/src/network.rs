//! Vector workflows: shortest routes over a line network, random points, field profiles,
//! attribute similarity and neighbourhood summaries.

use std::collections::{BinaryHeap, HashMap};
use std::sync::{Arc, Mutex, Weak};

use rustgis_data::{Dataset, Feature, Geometry};
use rustgis_geom::{Coord, EARTH_RADIUS_M, WGS84_MIN_RADIUS_M, haversine_m, wgs84_distance_m};

/// A shortest route: its path, length in metres, cost, and the line rows it uses.
#[derive(Clone, Debug, PartialEq)]
pub struct Route {
    pub path: Vec<Coord>,
    pub length_m: f64,
    pub cost: f64,
    pub rows: Vec<usize>,
    /// Segments omitted because an endpoint has non-finite coordinates or an invalid latitude.
    pub skipped_segments: usize,
}

#[derive(PartialEq)]
struct State(f64, usize);
impl Eq for State {}
impl PartialOrd for State {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for State {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        o.0.partial_cmp(&self.0).unwrap_or(std::cmp::Ordering::Equal)
    }
}

/// Nodes join where line vertices coincide (to about a centimetre).
fn key(c: Coord) -> (i64, i64) {
    ((c.x * 1e7).round() as i64, (c.y * 1e7).round() as i64)
}

type Edge = (usize, f64, f64, usize); // (to, cost, length, row)
type Segment = (usize, usize, f64, usize); // (from, to, weight, row)

/// Immutable topology: snapped route ends belong only to each query's working copy.
struct Network {
    nodes: Vec<Coord>,
    adj: Vec<Vec<Edge>>,
    segs: Vec<Segment>,
    skipped_segments: usize,
}

impl Network {
    /// Builds edges in source order, preserving shortest-path tie breaking and arithmetic.
    fn build(ds: &Dataset, rows: &[usize], cost_field: Option<usize>) -> Result<Self, String> {
        let route_error = |reason, skipped| route_error(ds, reason, skipped);
        let mut skipped_segments = 0;
        let mut ids: HashMap<(i64, i64), usize> = HashMap::new();
        let mut nodes: Vec<Coord> = Vec::new();
        let mut adj: Vec<Vec<Edge>> = Vec::new();
        let mut segs: Vec<Segment> = Vec::new();
        let node = |c: Coord, ids: &mut HashMap<(i64, i64), usize>, nodes: &mut Vec<Coord>, adj: &mut Vec<Vec<Edge>>| {
            *ids.entry(key(c)).or_insert_with(|| {
                nodes.push(c);
                adj.push(Vec::new());
                nodes.len() - 1
            })
        };
        let mut pace = crate::control::Pace::default();
        crate::control::begin(rows.len());
        for (i, r) in rows.iter().enumerate() {
            crate::control::check()?;
            crate::control::reached(i);
            let Some(Geometry::Line(parts)) = ds.features.get(*r).and_then(|f| f.geometry.as_ref()) else { continue };
            // A cost multiplies each edge's length; no value means 1, and zero or less closes the
            // line (impassable).
            let weight = match cost_field.map(|fi| ds.value(*r, fi).as_f64()) {
                Some(Some(w)) if w.is_finite() && w > 0.0 => w,
                Some(Some(_)) => continue,
                _ => 1.0,
            };
            for part in parts {
                for w in part.windows(2) {
                    if pace.stop(4096) {
                        return Err(crate::control::Cancelled.into());
                    }
                    let (Some(a), Some(b)) = (w.first(), w.get(1)) else { continue };
                    // Non-finite coordinates cast to ordinary integer keys (NaN becomes zero).
                    // Keep unroutable segments out before they can corrupt a valid shared node.
                    if !a.is_finite() || !b.is_finite() || a.y.abs() > 90.0 || b.y.abs() > 90.0 {
                        skipped_segments += 1;
                        continue;
                    }
                    let (ia, ib) = (node(*a, &mut ids, &mut nodes, &mut adj), node(*b, &mut ids, &mut nodes, &mut adj));
                    let len = wgs84_distance_m(*a, *b);
                    if let Some(v) = adj.get_mut(ia) {
                        v.push((ib, len * weight, len, *r));
                    }
                    if let Some(v) = adj.get_mut(ib) {
                        v.push((ia, len * weight, len, *r));
                    }
                    segs.push((ia, ib, weight, *r));
                    // Checked per segment, so one huge feature can't grow the network unbounded.
                    if segs.len() > 10_000_000 || nodes.len() > 5_000_000 {
                        return Err(route_error("the network is too large".into(), skipped_segments));
                    }
                }
            }
        }
        if segs.is_empty() && skipped_segments > 0 {
            return Err(route_error("no usable network remains".into(), skipped_segments));
        }
        crate::control::finish();
        Ok(Self { nodes, adj, segs, skipped_segments })
    }

    /// Heap storage retained by the cache, including spare capacity in every adjacency list.
    fn bytes(&self) -> usize {
        size_of::<Network>()
            .saturating_add(2 * size_of::<usize>())
            .saturating_add(self.nodes.capacity().saturating_mul(size_of::<Coord>()))
            .saturating_add(self.adj.capacity().saturating_mul(size_of::<Vec<Edge>>()))
            .saturating_add(self.segs.capacity().saturating_mul(size_of::<Segment>()))
            .saturating_add(self.adj.iter().fold(0usize, |n, edges| n.saturating_add(edges.capacity().saturating_mul(size_of::<Edge>()))))
    }
}

const GRAPH_CACHE_BYTES: usize = 256 << 20;

struct CachedNetwork {
    features: Weak<Vec<Feature>>,
    rows: Vec<usize>,
    cost_field: Option<usize>,
    graph: Arc<Network>,
}

static NETWORK: Mutex<Option<CachedNetwork>> = Mutex::new(None);

/// Reuses one bounded graph across calls and workers, without retaining the source features.
/// Mutable feature access invalidates the weak allocation identity, including in-place edits.
fn network_for(
    cache: &Mutex<Option<CachedNetwork>>,
    ds: &Dataset,
    rows: &[usize],
    cost_field: Option<usize>,
) -> Result<Arc<Network>, String> {
    crate::control::check()?;
    let features = ds.features.allocation_key();
    {
        let cached = cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(cached) = cached.as_ref().filter(|c| c.features.ptr_eq(&features) && c.rows == rows && c.cost_field == cost_field) {
            return Ok(cached.graph.clone());
        }
    }
    // Build outside the lock: another run can check Cancel while this one builds its graph.
    let graph = Arc::new(Network::build(ds, rows, cost_field)?);
    let bytes = graph.bytes().saturating_add(rows.len().saturating_mul(size_of::<usize>()));
    crate::control::check()?;
    let mut cached = cache.lock().unwrap_or_else(|e| e.into_inner());
    *cached = (bytes <= GRAPH_CACHE_BYTES).then(|| CachedNetwork { features, rows: rows.to_vec(), cost_field, graph: graph.clone() });
    Ok(graph)
}

/// Preserve clean-network errors; failed partial routes must still disclose omissions.
fn route_error(ds: &Dataset, reason: String, skipped_segments: usize) -> String {
    if skipped_segments == 0 {
        reason
    } else {
        format!("{reason}; {skipped_segments} segments with invalid coordinates were omitted from '{}'", ds.name)
    }
}

/// The shortest route between two points over the lines of `rows`, joining where vertices
/// coincide. Each edge costs its length (on the WGS 84 ellipsoid) times the row's `cost_field`
/// value (1 when empty; lines costing zero or less are closed). Each
/// end snaps to the nearest point on a line (within `snap_m`), where the route starts or stops.
/// Repeated calls share one graph up to 256 MiB; edits, row order/visibility and cost field changes
/// rebuild it. Snapping and endpoint attachments remain specific to each call.
pub fn shortest_route(
    ds: &Dataset,
    rows: &[usize],
    from: Coord,
    to: Coord,
    cost_field: Option<usize>,
    snap_m: f64,
) -> Result<Route, String> {
    shortest_route_with(&NETWORK, ds, rows, from, to, cost_field, snap_m)
}

/// Keeps endpoint validation, graph reuse and search on the same path for any cache owner.
fn shortest_route_with(
    cache: &Mutex<Option<CachedNetwork>>,
    ds: &Dataset,
    rows: &[usize],
    from: Coord,
    to: Coord,
    cost_field: Option<usize>,
    snap_m: f64,
) -> Result<Route, String> {
    if !snap_m.is_finite() {
        return Err("the snap distance must be finite".into());
    }
    if [from, to].iter().any(|c| !c.is_finite() || c.y.abs() > 90.0) {
        return Err("a route end has invalid coordinates".into());
    }
    let graph = network_for(cache, ds, rows, cost_field)?;
    route_on(&graph, ds, from, to, snap_m)
}

/// The closest point on any of `segs` to `p`: (segment, fraction along it, the point, metres
/// away). With `within`, only segments whose box comes within about that many metres of `p` are
/// measured; one farther can't be within it, so a nearer one is never missed.
fn nearest_segment(segs: &[Segment], nodes: &[Coord], p: Coord, within: Option<f64>) -> Result<Option<(usize, f64, Coord, f64)>, String> {
    crate::control::check()?;
    let window = within.and_then(|m| snap_window(p, m));
    let k = p.y.to_radians().cos().max(0.01);
    let mut stopped = false;
    let mut pace = crate::control::Pace::default();
    let nearest = segs
        .iter()
        .enumerate()
        .take_while(|_| {
            stopped = pace.stop(4096);
            !stopped
        })
        .filter_map(|(i, (a, b, _, _))| {
            let (a, b) = (*nodes.get(*a)?, *nodes.get(*b)?);
            // A segment runs the short way, as it is measured: an end stored across ±180° from the
            // other is moved by a whole turn beside it. Unchanged within 180° of each other.
            let gap = b.x - a.x;
            let b = if gap.abs() > 180.0 { Coord::new(a.x + (gap + 180.0).rem_euclid(360.0) - 180.0, b.y) } else { b };
            // The whole turns between the point and the segment's middle: none within 180° of it,
            // so only a segment near the antimeridian (or stored past ±180°) sees the point moved
            // to its side, for the window test as for the projection.
            let turn = 360.0 * ((0.5 * (a.x + b.x) - p.x) / 360.0).round();
            if let Some([x0, x1, y0, y1]) = window
                && (a.x.max(b.x) - turn < x0 || a.x.min(b.x) - turn > x1 || a.y.max(b.y) < y0 || a.y.min(b.y) > y1)
            {
                return None;
            }
            let px = p.x + turn;
            let (dx, dy) = ((b.x - a.x) * k, b.y - a.y);
            let len2 = dx * dx + dy * dy;
            let t = if len2 > 0.0 { (((px - a.x) * k * dx + (p.y - a.y) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
            let x = a.x + t * (b.x - a.x);
            // Written within −180° to 180°, as a line's vertices are; unchanged when it is.
            let x = if (-180.0..=180.0).contains(&x) { x } else { (x + 180.0).rem_euclid(360.0) - 180.0 };
            let q = Coord::new(x, a.y + t * (b.y - a.y));
            Some((i, t, q, haversine_m(q, p)))
        })
        .fold(None, |best: Option<(usize, f64, Coord, f64)>, (i, t, q, sphere)| {
            // The minimum WGS 84 radius gives a lower bound, so a farther spherical
            // candidate cannot be discarded merely because the two metrics rank it differently.
            let lower = if (-90.0..=90.0).contains(&p.y) && (-90.0..=90.0).contains(&q.y) {
                sphere * WGS84_MIN_RADIUS_M / EARTH_RADIUS_M
            } else {
                0.0 // The inverse solver clamps latitudes; the spherical bound would not apply.
            };
            if best.as_ref().is_some_and(|b| lower > b.3) {
                return best;
            }
            let distance = wgs84_distance_m(q, p);
            if distance.is_finite() && best.as_ref().is_none_or(|b| distance < b.3) { Some((i, t, q, distance)) } else { best }
        });
    if stopped {
        return Err(crate::control::Cancelled.into());
    }
    Ok(nearest)
}

/// The longitudes and latitudes `[west, east, south, north]` holding every place within `m`
/// metres of `p`, with room to spare: a degree of latitude is at least 110.5 km long, and a degree
/// of longitude at least that times the cosine of the latitude. None near a pole or ±180°, where a
/// box doesn't hold them; then every segment is measured.
fn snap_window(p: Coord, m: f64) -> Option<[f64; 4]> {
    let dlat = m / 110_000.0 * 1.5;
    let top = p.y.abs() + dlat;
    if !dlat.is_finite() || top.is_nan() || top >= 80.0 {
        return None;
    }
    let dlon = dlat / top.to_radians().cos();
    (p.x - dlon > -180.0 && p.x + dlon < 180.0).then_some([p.x - dlon, p.x + dlon, p.y - dlat, p.y + dlat])
}

/// Runs the original search on a private copy, so endpoint attachments never leak across calls.
/// Cloning the node and adjacency vectors is one step without a Cancel check inside it.
fn route_on(graph: &Network, ds: &Dataset, from: Coord, to: Coord, snap_m: f64) -> Result<Route, String> {
    crate::control::check()?;
    let mut nodes = graph.nodes.clone();
    let mut adj = graph.adj.clone();
    let segs = &graph.segs;
    let skipped_segments = graph.skipped_segments;
    let route_error = |reason, skipped| route_error(ds, reason, skipped);
    let nearest = |p: Coord, nodes: &[Coord]| nearest_segment(segs, nodes, p, None);
    // Each end becomes a node on its segment, joined to the segment's two ends.
    let attach = |p: Coord, nodes: &mut Vec<Coord>, adj: &mut Vec<Vec<Edge>>| -> Result<(usize, usize, f64), String> {
        let (si, t, q, d) = nearest(p, nodes)?.ok_or_else(|| route_error("the layer has no lines".into(), skipped_segments))?;
        if d > snap_m {
            return Err(route_error(
                format!("a route end is more than {} from the network", rustgis_geom::format_distance(snap_m)),
                skipped_segments,
            ));
        }
        let &(a, b, weight, row) = segs.get(si).ok_or_else(|| route_error("the layer has no lines".into(), skipped_segments))?;
        if t <= 0.0 {
            return Ok((a, si, t));
        }
        if t >= 1.0 {
            return Ok((b, si, t));
        }
        nodes.push(q);
        adj.push(Vec::new());
        let n = nodes.len() - 1;
        for end in [a, b] {
            let len = wgs84_distance_m(q, nodes.get(end).copied().unwrap_or(q));
            if let Some(v) = adj.get_mut(n) {
                v.push((end, len * weight, len, row));
            }
            if let Some(v) = adj.get_mut(end) {
                v.push((n, len * weight, len, row));
            }
        }
        Ok((n, si, t))
    };
    let (s, s_seg, s_t) = attach(from, &mut nodes, &mut adj)?;
    let (t, t_seg, t_t) = attach(to, &mut nodes, &mut adj)?;
    // Both ends on one segment: they're joined directly along it.
    if s_seg == t_seg && s != t && (0.0..=1.0).contains(&s_t) && (0.0..=1.0).contains(&t_t) {
        let (Some(&(_, _, weight, row)), Some(a), Some(b)) = (segs.get(s_seg), nodes.get(s).copied(), nodes.get(t).copied()) else {
            return Err(route_error("the layer has no lines".into(), skipped_segments));
        };
        let len = wgs84_distance_m(a, b);
        if let Some(v) = adj.get_mut(s) {
            v.push((t, len * weight, len, row));
        }
        if let Some(v) = adj.get_mut(t) {
            v.push((s, len * weight, len, row));
        }
    }
    let n = nodes.len();
    let mut dist = vec![f64::INFINITY; n];
    let mut prev: Vec<Option<(usize, f64, usize)>> = vec![None; n];
    let mut heap = BinaryHeap::new();
    if let Some(d) = dist.get_mut(s) {
        *d = 0.0;
    }
    heap.push(State(0.0, s));
    crate::control::begin(nodes.len());
    let mut visited = 0;
    let mut pace = crate::control::Pace::default();
    while let Some(State(d, u)) = heap.pop() {
        crate::control::check()?;
        visited += 1;
        crate::control::reached(visited);
        if u == t {
            break;
        }
        if d > dist.get(u).copied().unwrap_or(f64::INFINITY) {
            continue;
        }
        for &(v, c, len, row) in adj.get(u).map(Vec::as_slice).unwrap_or(&[]) {
            if pace.stop(4096) {
                return Err(crate::control::Cancelled.into());
            }
            let nd = d + c;
            if nd < dist.get(v).copied().unwrap_or(f64::INFINITY) {
                if let Some(x) = dist.get_mut(v) {
                    *x = nd;
                }
                if let Some(x) = prev.get_mut(v) {
                    *x = Some((u, len, row));
                }
                heap.push(State(nd, v));
            }
        }
    }
    let cost = dist.get(t).copied().unwrap_or(f64::INFINITY);
    if !cost.is_finite() {
        return Err(route_error("the two points aren't connected by the network".into(), skipped_segments));
    }
    let mut path = Vec::new();
    let mut length = 0.0;
    let mut used = Vec::new();
    let mut at = t;
    path.push(nodes.get(t).copied().unwrap_or(to));
    while let Some(Some((p, len, row))) = prev.get(at) {
        crate::control::check()?;
        path.push(nodes.get(*p).copied().unwrap_or(from));
        length += len;
        if used.last() != Some(row) {
            used.push(*row);
        }
        at = *p;
    }
    path.reverse();
    used.reverse();
    used.dedup();
    crate::control::finish();
    Ok(Route { path, length_m: length, cost, rows: used, skipped_segments })
}

// ---------------------------------------------------------------------------------------------
// Searches from several places at once, for service areas and cost matrices (#2344)

/// A line network for searches from many places: the route's graph, shared through the same
/// cache, so a route, a service area and a cost matrix over unchanged lines build it once.
pub(crate) struct Graph(Arc<Network>);

/// A point moved onto the network: onto segment `seg`, at `at`, `to_a` and `to_b` metres along
/// the segment from its two ends.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Placed {
    pub(crate) seg: usize,
    pub(crate) at: Coord,
    pub(crate) to_a: f64,
    to_b: f64,
}

/// One segment of the network: its two ends (nodes), their places, the weight its length is
/// multiplied by, and the row of the line it belongs to.
pub(crate) struct SegmentInfo {
    pub(crate) a: usize,
    pub(crate) b: usize,
    pub(crate) from: Coord,
    pub(crate) to: Coord,
    pub(crate) weight: f64,
    pub(crate) row: usize,
}

impl Graph {
    /// The network over `rows` of `ds` (in that order), each line's length multiplied by its
    /// `cost_field` value as for [`shortest_route`]. Reused while the lines don't change.
    pub(crate) fn of(ds: &Dataset, rows: &[usize], cost_field: Option<usize>) -> Result<Self, String> {
        network_for(&NETWORK, ds, rows, cost_field).map(Self)
    }

    pub(crate) fn nodes(&self) -> usize {
        self.0.nodes.len()
    }

    pub(crate) fn segments(&self) -> usize {
        self.0.segs.len()
    }

    /// Segments left out of the network for invalid coordinates.
    pub(crate) fn skipped_segments(&self) -> usize {
        self.0.skipped_segments
    }

    /// Segment `i`, in the order the lines and their vertices were read; None past the last.
    pub(crate) fn segment(&self, i: usize) -> Option<SegmentInfo> {
        let &(a, b, weight, row) = self.0.segs.get(i)?;
        Some(SegmentInfo { a, b, from: *self.0.nodes.get(a)?, to: *self.0.nodes.get(b)?, weight, row })
    }

    /// `p` moved onto the nearest segment, or None when none is within `snap_m` metres (or there
    /// are no segments). Measured as a route's ends are.
    pub(crate) fn place(&self, p: Coord, snap_m: f64) -> Result<Option<Placed>, String> {
        if !p.is_finite() || p.y.abs() > 90.0 {
            return Ok(None);
        }
        let Some((seg, _, at, moved_m)) = nearest_segment(&self.0.segs, &self.0.nodes, p, Some(snap_m))? else { return Ok(None) };
        if moved_m > snap_m {
            return Ok(None);
        }
        let Some(s) = self.segment(seg) else { return Ok(None) };
        Ok(Some(Placed { seg, at, to_a: wgs84_distance_m(at, s.from), to_b: wgs84_distance_m(at, s.to) }))
    }

    /// Where a search from `p` starts: both ends of its segment, at the cost and length of the
    /// way along it, labelled `source`. None for a place not on this graph, so it reaches nothing.
    pub(crate) fn starts(&self, p: &Placed, source: usize) -> Vec<Start> {
        let Some(&(a, b, w, _)) = self.0.segs.get(p.seg) else { return Vec::new() };
        vec![Start { node: a, cost: p.to_a * w, length: p.to_a, source }, Start { node: b, cost: p.to_b * w, length: p.to_b, source }]
    }

    /// The cheapest way to `to` found by `search`, as (cost, length): through either end of its
    /// segment, or straight along it from `from` when both lie on the same segment. Infinite when
    /// the search didn't reach it.
    pub(crate) fn reach(&self, search: &Search, from: &Placed, to: &Placed) -> (f64, f64) {
        let Some(&(a, b, w, _)) = self.0.segs.get(to.seg) else { return (f64::INFINITY, f64::INFINITY) };
        let mut best = (f64::INFINITY, f64::INFINITY);
        for (node, along) in [(a, to.to_a), (b, to.to_b)] {
            let cost = search.cost(node) + along * w;
            if cost < best.0 {
                best = (cost, search.length(node) + along);
            }
        }
        if from.seg == to.seg {
            let along = wgs84_distance_m(from.at, to.at);
            if along * w <= best.0 {
                best = (along * w, along);
            }
        }
        best
    }
}

/// Where a search starts: a node, the cost and length of reaching it from the start's own place,
/// and the start's label (a facility's or an origin's number).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Start {
    pub(crate) node: usize,
    pub(crate) cost: f64,
    pub(crate) length: f64,
    pub(crate) source: usize,
}

/// A search's working state, made once per thread and reused: each search resets only the nodes
/// the last one reached, so many short searches over a large network don't each clear it all.
pub(crate) struct Search {
    cost: Vec<f64>,
    length: Vec<f64>,
    source: Vec<usize>,
    settled: Vec<bool>,
    touched: Vec<usize>,
    heap: BinaryHeap<State>,
}

impl Search {
    /// The most bytes a search over `graph` holds, for sharing memory between threads: its arrays
    /// by node, and its queue, which may take an entry for each way along each segment and for
    /// each start, kept between searches at up to twice that as it grows.
    pub(crate) fn bytes(graph: &Graph) -> usize {
        let per_node = graph.nodes().saturating_mul(3 * size_of::<f64>() + 1 + 2 * size_of::<usize>());
        let queue = graph.segments().saturating_mul(2).saturating_add(2).saturating_mul(2 * size_of::<State>());
        per_node.saturating_add(queue)
    }

    pub(crate) fn new(graph: &Graph) -> Self {
        let n = graph.nodes();
        Self {
            cost: vec![f64::INFINITY; n],
            length: vec![f64::INFINITY; n],
            source: vec![usize::MAX; n],
            settled: vec![false; n],
            touched: Vec::new(),
            heap: BinaryHeap::new(),
        }
    }

    /// The least cost of reaching `node` in the last search: exact for every node it settled. When
    /// `settled` ended the search early, the others keep tentative costs; otherwise they are above
    /// `limit` (or infinite).
    pub(crate) fn cost(&self, node: usize) -> f64 {
        self.cost.get(node).copied().unwrap_or(f64::INFINITY)
    }

    /// The length of the way to `node` that [`Search::cost`] costs.
    pub(crate) fn length(&self, node: usize) -> f64 {
        self.length.get(node).copied().unwrap_or(f64::INFINITY)
    }

    /// The nodes the last search gave a cost: every node it settled, and some beyond `limit`.
    pub(crate) fn reached(&self) -> impl Iterator<Item = usize> + '_ {
        self.touched.iter().copied()
    }

    /// The start `node` is reached from most cheaply: of equally cheap ones, the first to reach it,
    /// which depends on the network, and for starts at one node on the order they were given.
    pub(crate) fn source(&self, node: usize) -> Option<usize> {
        self.source.get(node).copied().filter(|s| *s != usize::MAX)
    }

    /// Settles nodes in order of their cost from the nearest of `starts`, every node costing up to
    /// `limit`, and calls `settled` with each; the search ends early when it returns true. With
    /// `progress`, counts the nodes settled for the run's progress (a search on its own, not one
    /// of many made on several threads, which count theirs).
    pub(crate) fn run(
        &mut self,
        graph: &Graph,
        starts: &[Start],
        limit: f64,
        progress: bool,
        mut settled: impl FnMut(usize) -> bool,
    ) -> Result<(), String> {
        for &n in &self.touched {
            if let Some(c) = self.cost.get_mut(n) {
                *c = f64::INFINITY;
            }
            if let Some(l) = self.length.get_mut(n) {
                *l = f64::INFINITY;
            }
            if let Some(s) = self.source.get_mut(n) {
                *s = usize::MAX;
            }
            if let Some(s) = self.settled.get_mut(n) {
                *s = false;
            }
        }
        self.touched.clear();
        self.heap.clear();
        crate::control::check()?;
        for s in starts {
            self.improve(s.node, s.cost, s.length, s.source);
        }
        let adj = &graph.0.adj;
        if progress {
            crate::control::begin(adj.len());
        }
        let mut visited = 0;
        let mut pace = crate::control::Pace::default();
        while let Some(State(d, u)) = self.heap.pop() {
            if pace.stop(1) {
                return Err(crate::control::Cancelled.into());
            }
            if d > self.cost(u) || self.settled.get(u).copied().unwrap_or(true) {
                continue;
            }
            if d > limit {
                break;
            }
            if let Some(s) = self.settled.get_mut(u) {
                *s = true;
            }
            visited += 1;
            if progress {
                crate::control::reached(visited);
            }
            if settled(u) {
                break;
            }
            let (length, source) = (self.length(u), self.source.get(u).copied().unwrap_or(usize::MAX));
            for &(v, c, len, _) in adj.get(u).map(Vec::as_slice).unwrap_or(&[]) {
                self.improve(v, d + c, length + len, source);
            }
        }
        if progress {
            crate::control::finish();
        }
        Ok(())
    }

    /// Records a cheaper way to `node`, if this is one.
    fn improve(&mut self, node: usize, cost: f64, length: f64, source: usize) {
        let Some(old) = self.cost.get_mut(node) else { return };
        if cost.is_nan() || cost >= *old {
            return;
        }
        if old.is_infinite() {
            self.touched.push(node);
        }
        *old = cost;
        if let Some(l) = self.length.get_mut(node) {
            *l = length;
        }
        if let Some(s) = self.source.get_mut(node) {
            *s = source;
        }
        self.heap.push(State(cost, node));
    }
}

/// Most random points made in one call, and point-in-polygon tries spent making them.
pub const MAX_POINTS: usize = 1_000_000;
const MAX_TRIES: usize = 50_000_000;

/// Points spread at random (but reproducibly, from `seed`) inside polygons: `per` in each, up to
/// [`MAX_POINTS`] in all; thin polygons may get fewer (tries are bounded too).
pub fn random_points(ds: &Dataset, rows: &[usize], per: usize, seed: u64) -> Vec<(usize, Coord)> {
    let mut budget = MAX_TRIES;
    let mut state = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407) | 1;
    let mut rnd = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 11) as f64 / (1u64 << 53) as f64
    };
    let mut out = Vec::new();
    crate::control::begin(rows.len());
    for (i, r) in rows.iter().enumerate() {
        if crate::control::cancelled() {
            break;
        }
        crate::control::reached(i);
        let Some(Geometry::Polygon(polys)) = ds.features.get(*r).and_then(|f| f.geometry.as_ref()) else { continue };
        let e = ds.features.get(*r).and_then(|f| f.geometry.as_ref()).map(|g| g.extent()).unwrap_or_default();
        let mut placed = 0;
        let mut tries = 0usize;
        while placed < per && tries < per.saturating_mul(200) && budget > 0 && out.len() < MAX_POINTS {
            tries += 1;
            budget -= 1;
            let c = Coord::new(e.min_x + rnd() * e.width(), e.min_y + rnd() * e.height());
            if polys.iter().any(|rings| rustgis_data::geometry::polygon_contains(rings, c)) {
                out.push((*r, c));
                placed += 1;
            }
        }
    }
    out
}

/// A summary of one field: (name, type, count, nulls, distinct, min, max, mean, top values).
#[derive(Clone, Debug, PartialEq)]
pub struct FieldProfile {
    pub name: String,
    pub ty: String,
    pub count: usize,
    pub nulls: usize,
    /// Distinct values, counted until 100,000 are seen (`truncated` says counting stopped there,
    /// so `distinct` and `top` cover only the rows before).
    pub distinct: usize,
    pub truncated: bool,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub mean: Option<f64>,
    pub top: Vec<(String, usize)>,
}

/// Profiles every field of a dataset.
pub fn profile_fields(ds: &Dataset) -> Vec<FieldProfile> {
    ds.fields
        .iter()
        .enumerate()
        .map(|(fi, f)| {
            let mut counts: HashMap<String, usize> = HashMap::new();
            let (mut nulls, mut sum, mut n_num) = (0usize, 0.0, 0usize);
            let (mut min, mut max) = (f64::INFINITY, f64::NEG_INFINITY);
            let mut truncated = false;
            for r in 0..ds.features.len() {
                let v = ds.value(r, fi);
                if v.is_null() {
                    nulls += 1;
                    continue;
                }
                if let Some(x) = v.as_f64().filter(|_| f.ty.is_numeric()) {
                    sum += x;
                    n_num += 1;
                    min = min.min(x);
                    max = max.max(x);
                }
                // Past the cap, counting stops (no text is made for the remaining rows).
                if truncated {
                    continue;
                }
                if counts.len() >= 100_000 {
                    truncated = true;
                    continue;
                }
                *counts.entry(v.to_string()).or_insert(0) += 1;
            }
            let mut top: Vec<(String, usize)> = counts.iter().map(|(k, v)| (k.clone(), *v)).collect();
            top.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            top.truncate(5);
            FieldProfile {
                name: f.name.clone(),
                ty: f.ty.label().to_string(),
                count: ds.features.len(),
                nulls,
                distinct: counts.len(),
                truncated,
                min: (n_num > 0).then_some(min),
                max: (n_num > 0).then_some(max),
                mean: (n_num > 0).then(|| sum / n_num as f64),
                top,
            }
        })
        .collect()
}

/// Rows ranked by how close their numeric attributes are to a row's (z-scored Euclidean
/// distance over `fields`), nearest first, excluding the row itself.
pub fn similar(ds: &Dataset, row: usize, fields: &[usize], n: usize) -> Vec<(usize, f64)> {
    let stats: Vec<(f64, f64)> = fields
        .iter()
        .map(|fi| {
            let vals: Vec<f64> = (0..ds.features.len()).filter_map(|r| ds.value(r, *fi).as_f64()).filter(|v| v.is_finite()).collect();
            let m = vals.iter().sum::<f64>() / vals.len().max(1) as f64;
            let sd = (vals.iter().map(|v| (v - m).powi(2)).sum::<f64>() / vals.len().max(1) as f64).sqrt();
            (m, if sd > 0.0 { sd } else { 1.0 })
        })
        .collect();
    let vec_of = |r: usize| -> Option<Vec<f64>> {
        fields.iter().zip(&stats).map(|(fi, (m, sd))| ds.value(r, *fi).as_f64().filter(|v| v.is_finite()).map(|v| (v - m) / sd)).collect()
    };
    let Some(target) = vec_of(row) else { return Vec::new() };
    let mut out: Vec<(usize, f64)> = (0..ds.features.len())
        .filter(|r| *r != row)
        .filter_map(|r| vec_of(r).map(|v| (r, v.iter().zip(&target).map(|(a, b)| (a - b).powi(2)).sum::<f64>().sqrt())))
        .collect();
    out.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    out.truncate(n);
    out
}

fn radius_scale(at: Coord) -> f64 {
    1.0 / at.y.to_radians().cos().max(0.01)
}

fn radius_coord_supported(point: Coord) -> bool {
    point.is_finite() && (-180.0..=180.0).contains(&point.x) && (-rustgis_geom::MAX_LAT..=rustgis_geom::MAX_LAT).contains(&point.y)
}

/// Local Mercator distance in metres, using the same centre-latitude correction as [`within`].
///
/// This is not a geodesic distance and does not include `within`'s legacy geographic
/// bounding-box prefilter. Inputs must be canonical lon/lat within the Mercator domain. ([`within`]
/// measures a shape reaching beyond 85.05° on the ground instead.)
pub fn radius_distance_m(at: Coord, point: Coord) -> Option<f64> {
    if !radius_coord_supported(at) || !radius_coord_supported(point) {
        return None;
    }
    let center = rustgis_geom::lonlat_to_merc(at);
    let point = rustgis_geom::lonlat_to_merc(point);
    let dx = point.x - center.x;
    let dy = point.y - center.y;
    let distance = (dx.powi(2) + dy.powi(2)).sqrt() / radius_scale(at);
    distance.is_finite().then_some(distance)
}

/// A point on the local distance-threshold outline, at an angle counterclockwise from east.
///
/// Radii must be positive and at most 5,000 km. Points outside the canonical Mercator
/// domain are unsupported rather than wrapped or clamped. The outline is approximate:
/// [`within`] additionally preserves its historical geographic bounding-box prefilter, and it
/// measures a shape reaching beyond 85.05° on the ground, which may then count outside the outline.
pub fn radius_boundary_point(at: Coord, radius_m: f64, angle_rad: f64) -> Option<Coord> {
    if !radius_coord_supported(at) || !radius_m.is_finite() || radius_m <= 0.0 || radius_m > 5_000_000.0 || !angle_rad.is_finite() {
        return None;
    }
    let center = rustgis_geom::lonlat_to_merc(at);
    let radius = radius_m * radius_scale(at);
    let (sin, cos) = angle_rad.sin_cos();
    let point = Coord::new(center.x + radius * cos, center.y + radius * sin);
    let point = rustgis_geom::merc_to_lonlat(point);
    radius_coord_supported(point).then_some(point)
}

/// Rows of `ds` whose geometry comes within `radius_m` of a point, with their distances. A shape
/// is measured in local Web Mercator about the point, as [`radius_distance_m`] is; where the point
/// lies beyond 85.05°, where Web Mercator stops, or the shape's box reaches beyond it, it's
/// measured on the ground instead (`polar_reach`) (#1033), to within its steps' sag (up to about
/// 125 m within 5,000 km; farther, more, but only ever nearer). Whether a shape is measured in Web
/// Mercator or on the ground doesn't depend on the radius, and a larger radius never loses a row,
/// to within that sag (though past the search's `REACH_STEPS_TOTAL` steps, which a larger radius
/// may spend sooner, a shape counts at its box's distance; [`within_counted`] says how many did).
/// A center that's no place (a latitude past ±90°, a coordinate missing or infinite), or a radius
/// missing or negative, reaches no row: callers check them first, to say why.
pub fn within(ds: &Dataset, rows: &[usize], at: Coord, radius_m: f64) -> Vec<(usize, f64)> {
    within_counted(ds, rows, at, radius_m).0
}

/// [`within`], and how many of its rows have a distance that's a bound rather than measured: a
/// point or edge as near as its box ([`least_distance_to_box`]), past the search's
/// `REACH_STEPS_TOTAL` steps or along an edge longer than [`REACH_STEPS_MAX`] steps, or a polygon
/// counted as holding the center (at 0) when its turns around the pole outrun the steps or are too
/// many to count. They may lie farther, even beyond the radius.
pub fn within_counted(ds: &Dataset, rows: &[usize], at: Coord, radius_m: f64) -> (Vec<(usize, f64)>, usize) {
    let report = within_report(ds, rows, at, radius_m);
    (report.rows, report.bounded)
}

/// Radius results and counts of approximations or invalid input, for callers to report.
#[derive(Default)]
pub struct WithinReport {
    pub rows: Vec<(usize, f64)>,
    pub bounded: usize,
    pub invalid_latitudes: usize,
    pub missing_coordinates: usize,
}

/// Searches like [`within_counted`], also counting shapes refused for invalid latitudes and
/// shapes measured without vertices whose coordinates are missing or infinite. Shapes are measured
/// in longitude/latitude, converted from the dataset's own coordinates ([`Dataset::coord_crs`]),
/// which they are already today; a place that doesn't convert counts as missing.
pub fn within_report(ds: &Dataset, rows: &[usize], at: Coord, radius_m: f64) -> WithinReport {
    use rustgis_geom::MAX_LAT;
    // A center that's no place (a latitude past ±90°, a coordinate missing) reaches nothing.
    if !(at.x.is_finite() && at.y.abs() <= 90.0 && radius_m >= 0.0) {
        return WithinReport::default();
    }
    // Nothing reached when its system can't be converted, which longitude/latitude never fails to
    // be (`Dataset::coord_crs`).
    let Ok(lonlat) = ds.lonlat_conversion() else { return WithinReport::default() };
    let m = rustgis_geom::lonlat_to_merc(at);
    let k = radius_scale(at);
    let polar_point = at.y.abs() > MAX_LAT;
    // On the ground, its longitude within ±180° (to within a rounding, however far around it's
    // written), so every measure there sees the same place; Web Mercator takes it as written.
    let lon = at.x.rem_euclid(360.0);
    let center = Coord::new(if lon >= 180.0 { lon - 360.0 } else { lon }, at.y);
    let ground = rustgis_geom::Aeqd::new(center);
    // The steps along polar shapes' edges this search may take, shared by them all.
    let mut steps = Steps::new(REACH_STEPS_TOTAL);
    let mut bounded = 0;
    let (mut invalid_latitudes, mut missing_coordinates) = (0, 0);
    let mut out: Vec<(usize, f64)> = rows
        .iter()
        .filter_map(|r| {
            let own = ds.features.get(*r)?.geometry.as_ref()?;
            let converted;
            let g = if lonlat.is_identity() {
                own
            } else {
                converted = lonlat.geometry(own);
                &converted
            };
            let (mut invalid, mut missing) = (false, false);
            g.for_each_coord(|c| {
                invalid |= c.y.is_finite() && c.y.abs() > 90.0;
                missing |= !c.is_finite();
            });
            if invalid {
                invalid_latitudes += 1;
                return None;
            }
            missing_coordinates += usize::from(missing);
            // Skip shapes whose box is farther than the radius before projecting them.
            let e = g.extent();
            if polar_point || e.min_y < -MAX_LAT || e.max_y > MAX_LAT {
                if least_distance_to_box(center, (e.min_x, e.max_x), (e.min_y, e.max_y)) > radius_m {
                    return None;
                }
                steps.bound = f64::INFINITY;
                let d = polar_reach(g, &e, center, &ground, radius_m, &mut steps);
                // A shape with nothing to measure is no distance away, however far the radius.
                let found = d <= radius_m && d.is_finite();
                // Its distance a bound: it may lie farther, even beyond the radius.
                bounded += usize::from(found && steps.bound <= d);
                return found.then_some((*r, d));
            }
            let dlat = radius_m / 111_320.0;
            let dlon = dlat * k;
            if at.x < e.min_x - dlon || at.x > e.max_x + dlon || at.y < e.min_y - dlat || at.y > e.max_y + dlat {
                return None;
            }
            // Keep missing vertices unmeasurable instead of projecting infinite latitudes onto
            // the Mercator edge. A broken ring cannot establish containment.
            let pg = g.map_coords(&|c| if c.is_finite() { rustgis_geom::lonlat_to_merc(c) } else { Coord::new(f64::NAN, f64::NAN) });
            let d = if !missing && pg.hit(m, 0.0) { 0.0 } else { pg_dist(&pg, m) / k };
            (d.is_finite() && d <= radius_m).then_some((*r, d))
        })
        .collect();
    out.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    WithinReport { rows: out, bounded, invalid_latitudes, missing_coordinates }
}

/// The steps a search ([`within`]) has left to take along its shapes; and, for the shape being
/// measured, the least distance it counted at a bound rather than measured: a point or edge as
/// near as its box, once the steps it needed weren't there or were more than one edge takes.
struct Steps {
    left: usize,
    bound: f64,
}

impl Steps {
    fn new(left: usize) -> Self {
        Self { left, bound: f64::INFINITY }
    }

    /// Notes a distance counted at a bound.
    fn bound(&mut self, d: f64) -> f64 {
        self.bound = self.bound.min(d);
        d
    }

    /// Takes `n` steps if that many are left.
    fn take(&mut self, n: usize) -> bool {
        let enough = n <= self.left;
        if enough {
            self.left -= n;
        }
        enough
    }
}

/// The longest step, in degrees of longitude or latitude, that [`polar_reach`] follows an edge by.
pub const REACH_STEP_DEG: f64 = 0.5;
/// The most steps it takes along one edge; one needing more (over 512° long, as only a hostile
/// edge is) counts as near as its box.
pub const REACH_STEPS_MAX: f64 = 1024.0;
/// The most steps one search ([`within`]) takes along all its shapes' edges, a point measured
/// counting as one; past them, a point or edge counts as near as its box
/// ([`least_distance_to_box`]), so a shape may be counted in that's farther than the radius, never
/// left out for want of steps.
pub const REACH_STEPS_TOTAL: usize = 250_000;
/// More than a degree of longitude or latitude is long anywhere on the ground (meters): a step's
/// length is at most this times the degrees it spans in each.
const DEGREE_M_MAX: f64 = 111_700.0;

/// The least distance on the ground (meters) from `p` to a point with longitude in `lon` (west,
/// then east) and latitude in `lat` (degrees; a longitude range 360° wide holds every longitude),
/// never more than the true one: the central angle (from its tangent, which rounds little at any
/// distance) on a sphere of WGS 84's least radius of curvature, less a centimeter. An edge
/// straight in the analysis plane stays within its ends' box, so this bounds it too.
fn least_distance_to_box(p: Coord, (lon0, lon1): (f64, f64), (lat0, lat1): (f64, f64)) -> f64 {
    if !(p.is_finite() && lon0.is_finite() && lon1.is_finite() && lat0.is_finite() && lat1.is_finite()) {
        return 0.0;
    }
    let (lat0, lat1) = (lat0.min(lat1).clamp(-90.0, 90.0), lat0.max(lat1).clamp(-90.0, 90.0));
    let phi = p.y.clamp(-90.0, 90.0);
    // How far `p`'s longitude is from the range, the short way around (none within it).
    let (width, past) = (lon1 - lon0, (p.x - lon0).rem_euclid(360.0));
    let dlon = if width >= 360.0 || past <= width { 0.0 } else { (past - width).min(360.0 - past) };
    let angle = if dlon <= 0.0 {
        // Along `p`'s own meridian, to the nearer latitude.
        (lat0 - phi).max(phi - lat1).max(0.0).to_radians()
    } else {
        // On the range's nearer edge meridian. Along it, from pole to pole, the distance from `p`
        // has one turn: while Δλ is under 90°, a least one at atan(tan φ / cos Δλ), so the nearest
        // in range is that latitude clamped; from 90° on, a greatest one, so the nearest in range
        // is one of its ends.
        let (s, c) = dlon.to_radians().sin_cos();
        let (sin, cos) = phi.to_radians().sin_cos();
        let angle_to = |q: f64| {
            let (q_sin, q_cos) = q.to_radians().sin_cos();
            (q_cos * s).hypot(cos * q_sin - sin * q_cos * c).atan2(sin * q_sin + cos * q_cos * c)
        };
        if c > 0.0 { angle_to((sin / cos / c).atan().to_degrees().clamp(lat0, lat1)) } else { angle_to(lat0).min(angle_to(lat1)) }
    };
    (angle * rustgis_geom::WGS84_MIN_RADIUS_M - 0.01).max(0.0)
}

/// How far `g` comes from `at` on the ground (meters), as [`within`] measures it near a pole: zero
/// when `at` (its longitude within ±180°, as [`within`] gives it, moved by whole turns into `g`'s
/// box `e`) lies in a polygon in the analysis plane, as the other tools see it; otherwise the
/// least distance to its points or edges, each edge followed as the plane draws it (straight in
/// the plane: in longitude and latitude beyond 85.05°) in steps of at most half a degree, in
/// `ground`, an azimuthal equidistant plane at `at`. Its vertices alone would draw a long edge as
/// a chord, or one along a parallel from −180° to 180° as a point. A point or edge whose box can't
/// come within `radius_m`, or nearer than one already measured, isn't measured; each point
/// measured and each step takes one of the search's `steps`, and once they run out, a point or
/// edge counts as near as its box ([`least_distance_to_box`]).
fn polar_reach(g: &Geometry, e: &rustgis_geom::Extent, at: Coord, ground: &rustgis_geom::Aeqd, radius_m: f64, steps: &mut Steps) -> f64 {
    use rustgis_geom::{lonlat_to_plane, plane_to_lonlat};
    let from = |c: Coord| {
        let p = ground.forward(c);
        (p, p.x.hypot(p.y))
    };
    if let Geometry::Polygon(_) = g {
        // Incomplete rings are measured only along their finite edges, without inferring
        // containment from a missing vertex or clamping an infinite latitude onto a pole.
        let mut finite = true;
        g.for_each_coord(|c| finite &= c.is_finite());
        // The center at its longitude moved by whole turns into the polygon's box: a polygon
        // written across ±180°, or from 0° to 360°, holds a center written a turn or more away; one
        // whose box holds none of them holds no center. Every turn the box spans is tried: the
        // first four (all an ordinary polygon has) freely, each further one taking as many steps as
        // the polygon has vertices. When, past the first four, the steps left can't pay for the
        // rest, or the turns are too many to count one by one (past 2⁵³), the polygon counts as
        // holding the center, a bound, at once, taking none (as an edge too long to step along
        // does), so one polygon written a billion degrees long doesn't spend the search's steps.
        if finite && e.min_y <= at.y && at.y <= e.max_y {
            let (first, last) = (((e.min_x - at.x) / 360.0).ceil(), ((e.max_x - at.x) / 360.0).floor());
            let mut plane = None;
            let mut tried = 0_u32;
            let mut turn = first;
            // The steps each turn past the first four takes.
            let mut cost = 1;
            while turn <= last {
                if tried == 4 {
                    let mut n = 0_usize;
                    g.for_each_coord(|_| n += 1);
                    cost = n.max(1);
                    // (Finite, or infinite for a box billions of turns wide: never missing.)
                    if (last - turn + 1.0) * cost as f64 > steps.left as f64 {
                        return steps.bound(0.0);
                    }
                }
                if tried >= 4 && !steps.take(cost) {
                    return steps.bound(0.0);
                }
                let c = Coord::new(at.x + 360.0 * turn, at.y);
                if plane.get_or_insert_with(|| g.map_coords(&lonlat_to_plane)).hit(lonlat_to_plane(c), 0.0) {
                    return 0.0;
                }
                tried = tried.saturating_add(1);
                let next = first + f64::from(tried);
                if next <= turn {
                    return steps.bound(0.0);
                }
                turn = next;
            }
        }
    }
    let mut best = f64::INFINITY;
    let mut point = |c: Coord, best: &mut f64| {
        let near = if c.is_finite() { least_distance_to_box(at, (c.x, c.x), (c.y, c.y)) } else { f64::INFINITY };
        if near <= radius_m && near < *best {
            *best = if steps.take(1) { best.min(from(c).1) } else { steps.bound(near) };
        }
    };
    // A line's part of one point, or a ring of one point (closed or not), has no edges: it's
    // measured as that point.
    match g {
        Geometry::Point(_) | Geometry::MultiPoint(_) => {
            g.for_each_coord(|c| point(c, &mut best));
            return best;
        }
        Geometry::Line(parts) => parts.iter().for_each(|p| {
            if let [c] = p.as_slice() {
                point(*c, &mut best);
            }
        }),
        Geometry::Polygon(polys) => polys.iter().flatten().for_each(|r| match r.as_slice() {
            [c] => point(*c, &mut best),
            [c, d] if c == d => point(*c, &mut best),
            _ => {}
        }),
    }
    let origin = Coord::new(0.0, 0.0);
    // The last vertex measured: the next edge starts there when they're joined.
    let mut joined: Option<(Coord, (Coord, f64))> = None;
    g.for_each_edge(|_, a, b| {
        // A coordinate missing can't be measured.
        if !(a.is_finite() && b.is_finite()) {
            return;
        }
        let near = least_distance_to_box(at, (a.x.min(b.x), a.x.max(b.x)), (a.y.min(b.y), a.y.max(b.y)));
        if near > radius_m || near >= best {
            return;
        }
        // Its start, measured already when the edge before ended there, or one more step.
        let measured = joined.filter(|(c, _)| *c == a).map(|(_, measured)| measured);
        // An edge of no length, or along a pole (as rings cut at ±180° are closed: both ends at the
        // North Pole, or both at the South), is one place on the ground, measured once, as a point
        // is.
        let tolerance = rustgis_geom::POLE_TOLERANCE;
        let along = a.y.min(b.y) >= 90.0 - tolerance || a.y.max(b.y) <= -90.0 + tolerance;
        if a == b || along {
            let start = measured.or_else(|| steps.take(1).then(|| from(a)));
            match start {
                Some(start) => {
                    best = best.min(start.1);
                    joined = Some((b, start));
                }
                None => best = best.min(steps.bound(near)),
            }
            return;
        }
        // Steps equal in the plane: at most half a degree of longitude, and of latitude where the
        // plane stretches latitude least along the edge (at its lowest latitude, or 85.05° beyond).
        let (pa, pb) = (lonlat_to_plane(a), lonlat_to_plane(b));
        let low = if a.y * b.y <= 0.0 { 0.0 } else { a.y.abs().min(b.y.abs()).min(rustgis_geom::MAX_LAT) };
        let dlat = ((pb.y - pa.y).abs() / rustgis_geom::MERCATOR_R * low.to_radians().cos()).to_degrees();
        let span = (b.x - a.x).abs().max(dlat) / REACH_STEP_DEG;
        let n = if span > 1.0 { span.ceil().min(REACH_STEPS_MAX) as usize } else { 1 };
        let cost = n + usize::from(measured.is_none());
        // An edge needing more steps than one edge takes (over 512° long), or more than are left,
        // counts as near as its box: longer steps' chords could pass farther than the edge does.
        if span > REACH_STEPS_MAX || !steps.take(cost) {
            best = best.min(steps.bound(near));
            return;
        }
        let start = measured.unwrap_or_else(|| from(a));
        let (mut last, (mut last_p, mut last_d)) = (a, start);
        for k in 1..=n {
            let t = k as f64 / n as f64;
            let q = if k == n { b } else { plane_to_lonlat(Coord::new(pa.x + (pb.x - pa.x) * t, pa.y + (pb.y - pa.y) * t)) };
            let (q_p, q_d) = from(q);
            // A step is no nearer than its nearer end less its length: near `at`'s antipode, where
            // the plane tears, its chord can pass close to `at` though the step is far.
            let chord = rustgis_data::geometry::seg_dist2(&[last_p, q_p], origin).sqrt();
            let length = ((q.x - last.x).abs() + (q.y - last.y).abs()) * DEGREE_M_MAX;
            best = best.min(chord.max(last_d.min(q_d) - length));
            (last, last_p, last_d) = (q, q_p, q_d);
        }
        joined = Some((b, (last_p, last_d)));
    });
    best
}

fn pg_dist(g: &Geometry, p: Coord) -> f64 {
    let d2 = match g {
        Geometry::Point(c) => (c.x - p.x).powi(2) + (c.y - p.y).powi(2),
        Geometry::MultiPoint(v) => v.iter().map(|c| (c.x - p.x).powi(2) + (c.y - p.y).powi(2)).fold(f64::INFINITY, f64::min),
        Geometry::Line(parts) => parts.iter().map(|l| rustgis_data::geometry::seg_dist2(l, p)).fold(f64::INFINITY, f64::min),
        Geometry::Polygon(polys) => polys.iter().flatten().map(|r| rustgis_data::geometry::seg_dist2(r, p)).fold(f64::INFINITY, f64::min),
    };
    d2.sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_data::{Field, FieldType, GeometryType, Value};

    #[test]
    fn polar_reach_spends_its_budget_then_bounds_edges_by_their_boxes() {
        // A line around 89.95°N five times, 5.58 km from a center at 89.9°N: with room for two
        // edges' 718 steps and the first's start (the second starts where the first ended), it
        // follows two and counts the third as near as its box, 5.53 km, which is never more than
        // the line really is.
        let c = Coord::new;
        let line = Geometry::Line(vec![(0..=5).map(|i| c(if i % 2 == 0 { -179.5 } else { 179.5 }, 89.95)).collect()]);
        let at = c(0.0, 89.9);
        let mut steps = Steps::new(2 * 718 + 1);
        let d = polar_reach(&line, &line.extent(), at, &rustgis_geom::Aeqd::new(at), 10_000.0, &mut steps);
        assert!(steps.left == 0 && steps.bound == d);
        assert!((5_520.0..5_540.0).contains(&d), "{d}");
        // With room enough, every edge is followed: the line where it is.
        let mut steps = Steps::new(10 * 718);
        let d = polar_reach(&line, &line.extent(), at, &rustgis_geom::Aeqd::new(at), 10_000.0, &mut steps);
        assert!((5_560.0..5_600.0).contains(&d) && steps.left > 0 && steps.bound.is_infinite(), "{d}");
        // A box bound is never more than the distance: the box of an edge along a parallel.
        let bound = least_distance_to_box(at, (-179.5, 179.5), (89.95, 89.95));
        assert!(bound <= d && bound > 5_500.0, "{bound}");
    }

    #[test]
    fn a_box_is_never_farther_than_its_nearest_point() {
        // From 1°N, a box from 150°E to 160°E reaching the South Pole: its nearest point is the
        // pole, 91° away (past 90° of longitude away, distances along a meridian peak in between);
        // and one at 30°E from 10°N to 20°N, nearest at its corner at 10°N.
        let c = Coord::new;
        let p = c(0.0, 1.0);
        for (lon, lat, nearest) in [((150.0, 160.0), (-90.0, -80.0), c(150.0, -90.0)), ((30.0, 40.0), (10.0, 20.0), c(30.0, 10.0))] {
            let (bound, d) = (least_distance_to_box(p, lon, lat), rustgis_geom::wgs84_distance_m(p, nearest));
            assert!(bound <= d && bound > 0.99 * d, "{lon:?} {lat:?}: {bound} vs {d}");
        }
        // Micrometers away, where an arccosine would round to centimeters.
        let (p, q) = (c(-135.34187, -75.78973), c(-135.34187 + 4e-11, -75.78973));
        assert!(least_distance_to_box(p, (q.x, q.x), (q.y, q.y)) <= rustgis_geom::wgs84_distance_m(p, q));
    }

    #[test]
    fn a_center_is_where_its_longitude_falls_however_it_is_written() {
        // 1e300° is a whole number of turns: from 87°N there, as from 0°, a polygon from 175°E to
        // 185°E is 556 km away over the pole, not around the center.
        let c = Coord::new;
        let mut ds = Dataset::new("caps", GeometryType::Polygon, vec![]);
        ds.push(
            Some(Geometry::Polygon(vec![vec![vec![c(175.0, 86.0), c(185.0, 86.0), c(185.0, 88.0), c(175.0, 88.0), c(175.0, 86.0)]]])),
            vec![],
        );
        let (far, near) = (within(&ds, &[0], c(1e300, 87.0), 600_000.0), within(&ds, &[0], c(0.0, 87.0), 600_000.0));
        assert!(far.len() == 1 && far == near && (550_000.0..560_000.0).contains(&far[0].1), "{far:?} {near:?}");
        assert!(within(&ds, &[0], c(1e300, 87.0), 1.0).is_empty());
        // A center at 180.1° lies in a polygon written from 179.6° to 180.2°, as on the map.
        let mut across = Dataset::new("across", GeometryType::Polygon, vec![]);
        let ring = vec![c(179.6, 10.0), c(180.2, 10.0), c(180.2, 10.4), c(179.6, 10.4), c(179.6, 10.0)];
        across.push(Some(Geometry::Polygon(vec![vec![ring]])), vec![]);
        assert_eq!(within(&across, &[0], c(180.1, 10.2), 1_000.0), vec![(0, 0.0)]);
        // Within ±85.05°, Web Mercator takes a center as written, as before: one at 180° finds a
        // point at 179.9°, and one at 190° a point written 190.1°.
        let mut pts = Dataset::new("pts", GeometryType::Point, vec![]);
        pts.push(Some(Geometry::Point(c(179.9, 0.0))), vec![]);
        pts.push(Some(Geometry::Point(c(190.1, 0.0))), vec![]);
        let found = |at: Coord| within(&pts, &[0, 1], at, 50_000.0).into_iter().map(|(r, _)| r).collect::<Vec<_>>();
        assert_eq!((found(c(180.0, 0.0)), found(c(190.0, 0.0))), (vec![0], vec![1]));
    }

    #[test]
    fn an_edge_along_a_pole_is_measured_once() {
        // From 180° to −180° along the South Pole, as rings cut at ±180° are closed: one place, one
        // step of the budget, 1° (111.7 km) from 89°S.
        let c = Coord::new;
        let (at, line) = (c(0.0, -89.0), Geometry::Line(vec![vec![c(180.0, -90.0), c(-180.0, -90.0)]]));
        let mut steps = Steps::new(10);
        let d = polar_reach(&line, &line.extent(), at, &rustgis_geom::Aeqd::new(at), 200_000.0, &mut steps);
        assert!(steps.left == 9 && (d - rustgis_geom::wgs84_distance_m(at, c(0.0, -90.0))).abs() < 1e-3, "{} {d}", steps.left);
        // Going on up 180°W from there, the next edge starts where that one ended: one step more.
        let line = Geometry::Line(vec![vec![c(180.0, -90.0), c(-180.0, -90.0), c(-180.0, -89.75)]]);
        let mut steps = Steps::new(10);
        polar_reach(&line, &line.extent(), at, &rustgis_geom::Aeqd::new(at), 200_000.0, &mut steps);
        assert_eq!(steps.left, 8);
        // A ring whose vertices are all one point is that point, once, as is a vertex repeated
        // beside a coordinate missing: 10 km off, it's found.
        let p = c(0.0, -89.09);
        let ring = Geometry::Polygon(vec![vec![vec![p, p, p, p]]]);
        let mut steps = Steps::new(10);
        let d = polar_reach(&ring, &ring.extent(), at, &rustgis_geom::Aeqd::new(at), 200_000.0, &mut steps);
        assert!(steps.left == 9 && (d - rustgis_geom::wgs84_distance_m(at, p)).abs() < 1e-3, "{} {d}", steps.left);
        let mut beside = Dataset::new("beside", GeometryType::Polyline, vec![]);
        beside.push(Some(Geometry::Line(vec![vec![p, p, c(f64::NAN, -89.5)]])), vec![]);
        let found = within(&beside, &[0], at, 20_000.0);
        assert!(found.len() == 1 && found.iter().all(|(_, d)| (d - rustgis_geom::wgs84_distance_m(at, p)).abs() < 1e-3), "{found:?}");
        // A shape with nothing to measure isn't found, however far the radius reaches.
        let mut empty = Dataset::new("empty", GeometryType::Polyline, vec![]);
        empty.push(Some(Geometry::Line(vec![])), vec![]);
        assert!(within(&empty, &[0], at, f64::INFINITY).is_empty());
    }

    #[test]
    fn polar_reach_counts_points_against_its_budget() {
        // Points at 89.95°N, 16.7, 12.5 and 5.6 km from a center at 89.9°N, nearest last: each
        // measured takes one of the budget; with none left, a point counts as near as its box,
        // never more than it is; and one out of reach takes none.
        let c = Coord::new;
        let at = c(0.0, 89.9);
        let (ground, pts) = (rustgis_geom::Aeqd::new(at), Geometry::MultiPoint(vec![c(180.0, 89.95), c(90.0, 89.95), c(0.0, 89.95)]));
        let mut steps = Steps::new(10);
        let d = polar_reach(&pts, &pts.extent(), at, &ground, 50_000.0, &mut steps);
        assert!(steps.left == 7 && steps.bound.is_infinite() && (5_570.0..5_600.0).contains(&d), "{} {d}", steps.left);
        let mut steps = Steps::new(1);
        let bound = polar_reach(&pts, &pts.extent(), at, &ground, 50_000.0, &mut steps);
        assert!(steps.left == 0 && steps.bound == bound && bound <= d && bound > 5_500.0, "{bound}");
        let mut steps = Steps::new(10);
        let out_of_reach = polar_reach(&pts, &pts.extent(), at, &ground, 1_000.0, &mut steps);
        assert!(out_of_reach.is_infinite() && steps.left == 10 && steps.bound.is_infinite());
    }

    #[test]
    fn a_center_is_tried_at_every_turn_a_polygon_spans() {
        // A strip along 86°N from 0° to 1441°, rising into a block from 1430° to 1441°: a center at
        // 86.5°N on 0° lies in the block, 1440° on, its fifth turn. A sliver a billion degrees long,
        // rising from 86°N to 87°N along it, reaches 86.5°N only some 1.4 million turns on: it isn't
        // tried at every turn for ever, but past the steps counts as holding the center, a bound,
        // and says so.
        let c = Coord::new;
        let mut ds = Dataset::new("strips", GeometryType::Polygon, vec![]);
        let block = vec![c(0.0, 86.0), c(1441.0, 86.0), c(1441.0, 87.0), c(1430.0, 87.0), c(1430.0, 86.1), c(0.0, 86.1), c(0.0, 86.0)];
        ds.push(Some(Geometry::Polygon(vec![vec![block]])), vec![]);
        let long = vec![c(0.0, 86.0), c(1e9, 86.0), c(1e9, 87.0), c(0.0, 86.0)];
        ds.push(Some(Geometry::Polygon(vec![vec![long]])), vec![]);
        assert_eq!(within_counted(&ds, &[0], c(0.0, 86.5), 1_000.0), (vec![(0, 0.0)], 0));
        assert_eq!(within_counted(&ds, &[1], c(0.0, 86.5), 50_000.0), (vec![(1, 0.0)], 1));
        // Past its first four turns, the sliver counts at its bound at once, taking no steps: a
        // line after it, about 160 km off, is still measured, and left out of 50 km.
        ds.push(Some(Geometry::Line(vec![vec![c(-170.0, 86.0), c(170.0, 89.9)]])), vec![]);
        assert_eq!(within_counted(&ds, &[1, 2], c(0.0, 86.5), 50_000.0), (vec![(1, 0.0)], 1));
        // A center on the sliver's corner is found there, at its first turn, exactly.
        assert_eq!(within_counted(&ds, &[1], c(0.0, 86.0), 50_000.0), (vec![(1, 0.0)], 0));
        // A wedge written 1e19° on, its turns some 2.8e16, past 2⁵³, where a turn and the next
        // round to one number: the center isn't in it at the first, which can't be counted on from,
        // so it's a bound, at once, taking no steps.
        let wedge = Geometry::Polygon(vec![vec![vec![c(1e19, 86.0), c(1e19 + 1e6, 86.0), c(1e19 + 1e6, 86.6), c(1e19, 86.0)]]]);
        let mut steps = Steps::new(REACH_STEPS_TOTAL);
        let at = c(0.0, 86.5);
        assert!((1e19 / 360.0f64).ceil() + 1.0 == (1e19 / 360.0f64).ceil());
        let d = polar_reach(&wedge, &wedge.extent(), at, &rustgis_geom::Aeqd::new(at), 50_000.0, &mut steps);
        assert!(d == 0.0 && steps.bound == 0.0 && steps.left == REACH_STEPS_TOTAL, "{d} {} {}", steps.bound, steps.left);
        // A center in the polygon at its first turn is found there, exactly, however many turns
        // its box spans after.
        let mut wide = Dataset::new("wide", GeometryType::Polygon, vec![]);
        let ring = vec![c(-10.0, 86.0), c(1e9, 86.0), c(1e9, 87.0), c(-10.0, 87.0), c(-10.0, 86.0)];
        wide.push(Some(Geometry::Polygon(vec![vec![ring]])), vec![]);
        assert_eq!(within_counted(&wide, &[0], c(0.0, 86.5), 1_000.0), (vec![(0, 0.0)], 0));
    }

    #[test]
    fn a_distance_counted_at_a_bound_says_so() {
        // From 89.9°N, a line along 89.95°N: its first, short edge is measured 5.75 km away; its
        // next, 180° long, needs more steps than are left and counts at its box's distance, nearer:
        // the line's distance is that bound. With steps enough, it's measured, and no bound. An
        // edge 600° long, needing more steps than one edge takes, counts at its box, taking none
        // (in longer steps, chords could pass farther than the edge does).
        let c = Coord::new;
        let (at, ground) = (c(0.0, 89.9), rustgis_geom::Aeqd::new(c(0.0, 89.9)));
        let line = Geometry::Line(vec![vec![c(10.0, 89.95), c(10.1, 89.95), c(-170.0, 89.95)]]);
        let mut steps = Steps::new(5);
        let d = polar_reach(&line, &line.extent(), at, &ground, 10_000.0, &mut steps);
        assert!(steps.bound == d && d < 5_700.0, "{d}");
        let mut steps = Steps::new(REACH_STEPS_TOTAL);
        let d = polar_reach(&line, &line.extent(), at, &ground, 10_000.0, &mut steps);
        assert!(steps.bound.is_infinite() && d > 5_500.0, "{d}");
        let long = Geometry::Line(vec![vec![c(0.0, 89.95), c(600.0, 89.95)]]);
        let mut steps = Steps::new(REACH_STEPS_TOTAL);
        let d = polar_reach(&long, &long.extent(), at, &ground, 10_000.0, &mut steps);
        let bound = least_distance_to_box(at, (0.0, 600.0), (89.95, 89.95));
        assert!(steps.bound == d && d == bound && steps.left == REACH_STEPS_TOTAL, "{d} {}", steps.bound);
    }

    #[test]
    fn a_search_counts_the_rows_it_found_past_its_steps() {
        // 300 lines each going 348 times around 89.95°N, 5.6 km from a center at 89.9°N: more
        // steps than one search has, so the last rows count at their boxes' distance, and say so;
        // a search at 85°N, measured in Web Mercator, takes no steps.
        let c = Coord::new;
        let mut ds = Dataset::new("lines", GeometryType::Polyline, vec![]);
        for _ in 0..300 {
            ds.push(Some(Geometry::Line(vec![(0..=348).map(|i| c(if i % 2 == 0 { -179.5 } else { 179.5 }, 89.95)).collect()])), vec![]);
        }
        let rows: Vec<usize> = (0..300).collect();
        let (found, bounded) = within_counted(&ds, &rows, c(0.0, 89.9), 10_000.0);
        assert!(found.len() == 300 && bounded > 0 && bounded < 300, "{} {bounded}", found.len());
        let mut near = Dataset::new("near", GeometryType::Point, vec![]);
        near.push(Some(Geometry::Point(c(0.0, 85.0))), vec![]);
        let (found, bounded) = within_counted(&near, &[0], c(0.01, 85.0), 10_000.0);
        assert!(found.len() == 1 && bounded == 0, "{found:?} {bounded}");
    }

    /// A segment across ±180° runs the short way: a point on it is placed there, and a point on
    /// the far side of the world isn't placed on it at all.
    #[test]
    fn a_segment_across_the_antimeridian_is_measured_the_short_way() {
        let mut ds = Dataset::new("roads", GeometryType::Polyline, vec![]);
        ds.push(Some(Geometry::Line(vec![vec![Coord::new(179.99, 0.0), Coord::new(-179.99, 0.0)]])), vec![]);
        let graph = Graph(Arc::new(Network::build(&ds, &[0], None).unwrap()));
        assert!(graph.place(Coord::new(0.0, 0.0), 1000.0).unwrap().is_none(), "nowhere near it");
        let on = graph.place(Coord::new(179.995, 0.0), 10.0).unwrap();
        assert!(on.is_some_and(|p| (p.at.x - 179.995).abs() < 1e-9), "{on:?}");
        let across = graph.place(Coord::new(-179.995, 0.0001), 50.0).unwrap();
        assert!(across.is_some_and(|p| (p.at.x + 179.995).abs() < 1e-9), "{across:?}");
        let middle = graph.place(Coord::new(180.0, 0.0), 1000.0).unwrap();
        assert!(middle.is_some_and(|p| p.at.x.abs() > 179.999 && p.at.y.abs() < 1e-9), "{middle:?}");
    }

    /// A point is placed on a segment stored past ±180° (at 370°, the same meridian as 10°), as a
    /// route's end is, not passed over by the snap window.
    #[test]
    fn a_segment_stored_past_180_degrees_is_found_by_the_window() {
        let mut ds = Dataset::new("roads", GeometryType::Polyline, vec![]);
        ds.push(Some(Geometry::Line(vec![vec![Coord::new(370.0, 0.0), Coord::new(370.01, 0.0)]])), vec![]);
        let graph = Graph(Arc::new(Network::build(&ds, &[0], None).unwrap()));
        let placed = graph.place(Coord::new(10.005, 0.0001), 100.0).unwrap();
        assert!(placed.is_some_and(|p| p.seg == 0), "{placed:?}");
    }

    /// Repeated pairs reuse topology without retaining endpoint edits; every graph input
    /// invalidates it, even an attribute/geometry mutation with no other strong feature owner.
    #[test]
    fn route_graph_reuse_preserves_results_and_follows_network_edits() {
        let cache = Mutex::new(None);
        let mut ds = Dataset::new(
            "roads",
            GeometryType::Polyline,
            vec![Field::new("cost", FieldType::Double), Field::new("other", FieldType::Double)],
        );
        ds.push(Some(Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(0.09, 0.0)]])), vec![Value::Float(1.0), Value::Float(2.0)]);
        let from = Coord::new(0.03, 0.0001);
        let to = Coord::new(0.06, -0.0001);
        let before = shortest_route_with(&cache, &ds, &[0], from, to, None, 50.0).unwrap();
        let graph = cache.lock().unwrap().as_ref().unwrap().graph.clone();
        assert_eq!(ds.features.allocation_key().strong_count(), 1, "the cache must not retain feature data");
        let shared = network_for(&cache, &ds.clone(), &[0], None).unwrap();
        assert!(Arc::ptr_eq(&graph, &shared), "another pair must not rebuild unchanged topology");
        // A different pair attaches different interior nodes, then the original pair repeats.
        shortest_route_with(&cache, &ds, &[0], Coord::new(0.02, 0.0), Coord::new(0.08, 0.0), None, 1.0).unwrap();
        let after = shortest_route_with(&cache, &ds, &[0], from, to, None, 50.0).unwrap();
        assert!(Arc::ptr_eq(&graph, &cache.lock().unwrap().as_ref().unwrap().graph), "queries must keep the original graph");
        assert_eq!(before, after);
        assert_eq!(before.length_m.to_bits(), after.length_m.to_bits());
        assert_eq!(before.cost.to_bits(), after.cost.to_bits());
        assert!(before.path.iter().zip(&after.path).all(|(a, b)| a.x.to_bits() == b.x.to_bits() && a.y.to_bits() == b.y.to_bits()));
        assert!(shortest_route_with(&cache, &ds, &[0], from, to, None, 1.0).is_err(), "a cache hit still obeys this pair's snap distance");
        assert_eq!((shared.nodes.len(), shared.segs.len()), (2, 1), "query nodes stay out of the graph");
        let cost = network_for(&cache, &ds, &[0], Some(0)).unwrap();
        assert!(!Arc::ptr_eq(&shared, &cost));
        let other = network_for(&cache, &ds, &[0], Some(1)).unwrap();
        assert!(!Arc::ptr_eq(&cost, &other));
        assert_eq!(route_on(&other, &ds, from, to, 50.0).unwrap().cost.to_bits(), (before.cost * 2.0).to_bits());
        ds.features[0].attrs[1] = Value::Float(3.0);
        let edited = network_for(&cache, &ds, &[0], Some(1)).unwrap();
        assert!(!Arc::ptr_eq(&other, &edited));
        assert_eq!(edited.adj[0][0].1.to_bits(), (edited.adj[0][0].2 * 3.0).to_bits());
        ds.features[0].geometry = Some(Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(0.18, 0.0)]]));
        let moved = network_for(&cache, &ds, &[0], Some(1)).unwrap();
        assert!(!Arc::ptr_eq(&edited, &moved));
        assert_eq!(moved.nodes[1], Coord::new(0.18, 0.0));
        ds.push(Some(Geometry::Line(vec![vec![Coord::new(0.18, 0.0), Coord::new(0.20, 0.0)]])), vec![Value::Float(1.0), Value::Float(1.0)]);
        let added = network_for(&cache, &ds, &[0, 1], Some(1)).unwrap();
        assert!(!Arc::ptr_eq(&moved, &added));
        let reversed = network_for(&cache, &ds, &[1, 0], Some(1)).unwrap();
        assert!(!Arc::ptr_eq(&added, &reversed), "row order participates in tie breaking");
        let filtered = network_for(&cache, &ds, &[1], Some(1)).unwrap();
        assert!(!Arc::ptr_eq(&reversed, &filtered));
        assert_eq!(filtered.segs.len(), 1);
        let replaced = Dataset::new("roads", GeometryType::Polyline, ds.fields.clone());
        let empty = network_for(&cache, &replaced, &[1], Some(1)).unwrap();
        assert!(!Arc::ptr_eq(&filtered, &empty), "a replacement with the same name is a different network");
        assert!(empty.segs.is_empty());
        // Cancellation reaches cold construction and cached queries, without publishing partial graphs.
        let control = crate::control::Control::cancelled_once_started();
        assert!(crate::run_under("Route", &control, || Network::build(&ds, &[0, 1], None)).is_err());
        assert!(control.looked() > 0);
        let control = crate::control::Control::cancelled_once_started();
        assert!(crate::run_under("Route", &control, || route_on(&filtered, &ds, from, to, 50.0)).is_err());
        assert!(control.looked() > 0);
    }

    #[test]
    fn routes_points_profiles_and_similarity() {
        // A square of roads with a slow diagonal.
        let mut ds = Dataset::new("roads", GeometryType::Polyline, vec![Field::new("cost", FieldType::Double)]);
        let l = |a: (f64, f64), b: (f64, f64)| Some(Geometry::Line(vec![vec![Coord::new(a.0, a.1), Coord::new(b.0, b.1)]]));
        ds.push(l((0.0, 0.0), (1.0, 0.0)), vec![Value::Float(1.0)]);
        ds.push(l((1.0, 0.0), (1.0, 1.0)), vec![Value::Float(1.0)]);
        ds.push(l((0.0, 0.0), (1.0, 1.0)), vec![Value::Float(5.0)]);
        let rows = [0, 1, 2];
        let short = shortest_route(&ds, &rows, Coord::new(0.0, 0.0), Coord::new(1.0, 1.0), None, 1000.0).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(short.rows, vec![2], "by length the diagonal wins");
        let cheap =
            shortest_route(&ds, &rows, Coord::new(0.0, 0.0), Coord::new(1.0, 1.0), Some(0), 1000.0).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(cheap.rows, vec![0, 1], "by cost the two sides win");
        assert!(shortest_route(&ds, &rows, Coord::new(5.0, 5.0), Coord::new(1.0, 1.0), None, 1000.0).is_err());
        let mut poly = Dataset::new("p", GeometryType::Polygon, vec![]);
        poly.push(
            Some(Geometry::Polygon(vec![vec![vec![
                Coord::new(0.0, 0.0),
                Coord::new(1.0, 0.0),
                Coord::new(1.0, 1.0),
                Coord::new(0.0, 1.0),
                Coord::new(0.0, 0.0),
            ]]])),
            vec![],
        );
        let pts = random_points(&poly, &[0], 25, 7);
        assert_eq!(pts.len(), 25);
        assert!(pts.iter().all(|(_, c)| c.x > 0.0 && c.x < 1.0 && c.y > 0.0 && c.y < 1.0));
        assert_eq!(random_points(&poly, &[0], 25, 7), pts, "reproducible");
        let p = profile_fields(&ds);
        assert_eq!((p[0].min, p[0].max, p[0].distinct), (Some(1.0), Some(5.0), 2));
        let sim = similar(&ds, 0, &[0], 2);
        assert_eq!(sim[0].0, 1);
        let near = within(&ds, &rows, Coord::new(0.5, 0.001), 500.0);
        assert_eq!(near.first().map(|x| x.0), Some(0));
    }

    #[test]
    fn route_ends_snap_onto_long_segments() {
        // One straight road 10 km long, with vertices only at its ends.
        let mut ds = Dataset::new("roads", GeometryType::Polyline, vec![]);
        ds.push(Some(Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(0.09, 0.0)]])), vec![]);
        // Clicks beside its middle, far from both vertices but close to the road.
        let r = shortest_route(&ds, &[0], Coord::new(0.03, 0.0001), Coord::new(0.06, -0.0001), None, 50.0).unwrap();
        assert!((r.length_m - 3339.6).abs() < 1.0, "{}", r.length_m);
        assert!((r.path[0].x - 0.03).abs() < 1e-9 && (r.path.last().unwrap().x - 0.06).abs() < 1e-9);
        assert!(shortest_route(&ds, &[0], Coord::new(0.03, 0.01), Coord::new(0.06, 0.0), None, 50.0).is_err(), "1 km away");
        let error = shortest_route(&ds, &[0], Coord::new(f64::NAN, 0.0), Coord::new(0.06, 0.0), None, 50.0).unwrap_err();
        assert!(error.contains("route end"), "{error}");
        let error = shortest_route(&ds, &[0], Coord::new(0.0, 95.0), Coord::new(0.06, 0.0), None, 50.0).unwrap_err();
        assert!(error.contains("route end"), "{error}");
        assert!(shortest_route(&ds, &[0], Coord::new(0.0, 1.0), Coord::new(0.06, 1.0), None, f64::NAN).is_err());
    }

    #[test]
    fn a_broken_segment_listed_first_does_not_hide_a_valid_one() {
        // The invalid segment is excluded before graph construction and cannot hide the road.
        let mut ds = Dataset::new("roads", GeometryType::Polyline, vec![]);
        ds.push(Some(Geometry::Line(vec![vec![Coord::new(0.5, 0.5), Coord::new(f64::NAN, 0.5)]])), vec![]);
        ds.push(Some(Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(0.09, 0.0)]])), vec![]);
        let r = shortest_route(&ds, &[0, 1], Coord::new(0.03, 0.0001), Coord::new(0.06, -0.0001), None, 50.0).unwrap();
        assert!((r.length_m - 3339.6).abs() < 1.0, "{}", r.length_m);
    }

    #[test]
    fn nonfinite_vertices_do_not_alias_valid_network_nodes() {
        let mut ds = Dataset::new("roads", GeometryType::Polyline, vec![]);
        ds.push(Some(Geometry::Line(vec![vec![Coord::new(f64::NAN, 0.5), Coord::new(0.5, 0.5)]])), vec![]);
        ds.push(Some(Geometry::Line(vec![vec![Coord::new(0.0, 0.5), Coord::new(0.09, 0.5)]])), vec![]);
        let r = shortest_route(&ds, &[0, 1], Coord::new(0.0, 0.5), Coord::new(0.09, 0.5), None, 1.0).unwrap();
        assert!(r.path.iter().all(|c| c.is_finite()), "{:?}", r.path);
        assert_eq!(r.rows, vec![1]);
        assert!((r.length_m - 10_018.375).abs() < 1.0, "{}", r.length_m);
    }

    #[test]
    fn snap_acceptance_uses_the_nearest_ellipsoidal_candidate() {
        let mut ds = Dataset::new("roads", GeometryType::Polyline, vec![]);
        // At the equator, 0.001 degrees east is nearer on the sphere than 0.001003
        // degrees north. On WGS 84 only the northern segment is within 111 metres.
        ds.push(Some(Geometry::Line(vec![vec![Coord::new(0.001, 0.0), Coord::new(0.002, 0.0)]])), vec![]);
        ds.push(Some(Geometry::Line(vec![vec![Coord::new(0.0, 0.001003), Coord::new(0.0, 0.003)]])), vec![]);
        let from = Coord::new(0.0, 0.0);
        assert!(haversine_m(from, Coord::new(0.001, 0.0)) < haversine_m(from, Coord::new(0.0, 0.001003)));
        let r = shortest_route(&ds, &[0, 1], from, Coord::new(0.0, 0.003), None, 111.0).unwrap();
        assert_eq!(r.rows, vec![1]);
        assert_eq!(r.path.first(), Some(&Coord::new(0.0, 0.001003)));
    }

    fn offset(at: Coord, x: f64, y: f64) -> Coord {
        let m = rustgis_geom::lonlat_to_merc(at);
        let scale = radius_scale(at);
        rustgis_geom::merc_to_lonlat(Coord::new(m.x + x * scale, m.y + y * scale))
    }

    #[test]
    fn radius_helpers_match_local_mercator_metric() {
        let at = Coord::new(0.0, 60.0);
        let m = rustgis_geom::lonlat_to_merc(at);
        let east = rustgis_geom::merc_to_lonlat(Coord::new(m.x + 2000.0, m.y));
        assert!((radius_distance_m(at, east).unwrap() - 1000.0).abs() < 1e-8);
        assert_eq!(radius_distance_m(at, at), Some(0.0));
        let boundary = radius_boundary_point(at, 1000.0, 0.0).unwrap();
        assert!((rustgis_geom::lonlat_to_merc(boundary).x - m.x - 2000.0).abs() < 1e-8);
        for (longitude, latitude) in [(3.0, 0.0), (3.0, 60.0), (-73.98, 40.75)] {
            let center = Coord::new(longitude, latitude);
            for angle in [0.0, 0.5, 1.0, 2.0, 3.0, 4.5] {
                let point = radius_boundary_point(center, 1000.0, angle).unwrap();
                assert!((radius_distance_m(center, point).unwrap() - 1000.0).abs() < 1e-7);
            }
            let diagonal = offset(center, 600.0, 800.0);
            let radius = radius_distance_m(center, diagonal).unwrap();
            assert!((radius - 1000.0).abs() < 1e-7);
            let mut ds = Dataset::new("boundary", GeometryType::Point, vec![]);
            ds.push(Some(Geometry::Point(diagonal)), vec![]);
            assert_eq!(within(&ds, &[0], center, radius), vec![(0, radius)]);
        }
    }

    #[test]
    fn radius_helpers_reject_unsupported_inputs_without_clamping() {
        let center = Coord::new(0.0, 0.0);
        for invalid in [
            Coord::new(f64::NAN, 0.0),
            Coord::new(0.0, f64::INFINITY),
            Coord::new(181.0, 0.0),
            Coord::new(-181.0, 0.0),
            Coord::new(0.0, 86.0),
            Coord::new(0.0, -86.0),
        ] {
            assert!(radius_distance_m(center, invalid).is_none());
            assert!(radius_distance_m(invalid, center).is_none());
            assert!(radius_boundary_point(invalid, 1000.0, 0.0).is_none());
        }
        for radius in [0.0, -1.0, 5_000_001.0, f64::NAN, f64::INFINITY] {
            assert!(radius_boundary_point(center, radius, 0.0).is_none());
        }
        for angle in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(radius_boundary_point(center, 1000.0, angle).is_none());
        }
        assert!(radius_boundary_point(Coord::new(179.999, 0.0), 1000.0, 0.0).is_none());
        assert!(radius_boundary_point(Coord::new(-179.999, 0.0), 1000.0, std::f64::consts::PI).is_none());
        assert!(radius_boundary_point(Coord::new(0.0, 85.05), 1000.0, std::f64::consts::FRAC_PI_2).is_none());
        assert!(radius_boundary_point(Coord::new(0.0, -85.05), 1000.0, -std::f64::consts::FRAC_PI_2).is_none());
        assert!(radius_boundary_point(center, 5_000_000.0, 0.0).is_some());
    }

    #[test]
    fn radius_geometry_distances_at_equator_and_high_latitude() {
        for latitude in [0.0, 60.0] {
            let center = Coord::new(0.0, latitude);
            let point = |x, y| offset(center, x, y);
            let square =
                |half: f64| vec![point(-half, -half), point(half, -half), point(half, half), point(-half, half), point(-half, -half)];
            let mut ds = Dataset::new("radius", GeometryType::Point, vec![]);
            for geometry in [
                Geometry::Point(center),
                Geometry::Point(point(300.0, 400.0)),
                Geometry::Point(point(660.0, 880.0)),
                Geometry::Line(vec![vec![point(-2000.0, 0.0), point(2000.0, 0.0)]]),
                Geometry::Polygon(vec![vec![square(2000.0)]]),
                Geometry::Polygon(vec![vec![square(2000.0), square(1500.0)]]),
                Geometry::MultiPoint(vec![point(4000.0, 4000.0), point(300.0, 400.0)]),
                Geometry::Line(vec![vec![point(-2000.0, 1500.0), point(2000.0, 1500.0)]]),
                Geometry::Polygon(vec![vec![vec![
                    point(600.0, 600.0),
                    point(800.0, 600.0),
                    point(800.0, 800.0),
                    point(600.0, 800.0),
                    point(600.0, 600.0),
                ]]]),
            ] {
                ds.push(Some(geometry), vec![]);
            }
            ds.push(None, vec![]);
            let rows: Vec<_> = (0..ds.features.len()).collect();
            let hits = within(&ds, &rows, center, 1000.0);
            let mut selected: Vec<_> = hits.iter().map(|hit| hit.0).collect();
            selected.sort_unstable();
            assert_eq!(selected, vec![0, 1, 3, 4, 6, 8]);
            for (row, distance) in hits {
                let expected = match row {
                    0 | 3 | 4 => 0.0,
                    1 | 6 => 500.0,
                    8 => (600.0_f64.powi(2) + 600.0_f64.powi(2)).sqrt(),
                    _ => panic!("unexpected radius hit"),
                };
                assert!((distance - expected).abs() < 1e-7, "{latitude}: {row}: {distance}");
            }
        }
    }

    #[test]
    fn radius_preserves_legacy_geographic_prefilter() {
        let center = Coord::new(0.0, 60.0);
        let point = Coord::new(0.0, 59.1);
        assert!(radius_distance_m(center, point).unwrap() < 100_000.0);
        let mut ds = Dataset::new("legacy prefilter", GeometryType::Point, vec![]);
        ds.push(Some(Geometry::Point(point)), vec![]);
        assert!(within(&ds, &[0], center, 100_000.0).is_empty());
    }
}
