//! Service areas and origin–destination cost matrices over a line layer (#2344), searched on the
//! network a route uses ([`crate::network::Graph`]): lines join where their vertices coincide, and
//! each line's length, on the WGS 84 ellipsoid, is multiplied by its cost field's value.
use super::*;
use crate::network::{Graph, Placed, Search, Start};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The most breaks a service area takes.
const MAX_BREAKS: usize = 32;
/// The most rows a cost matrix may hold.
const MAX_PAIRS: usize = 10_000_000;
/// How far a point may be moved onto the network when no snap distance is given, in metres.
const SNAP_M: f64 = 1000.0;

/// What both tools read first: the network's layer and graph, the unit costs are given and
/// written in, how far points may be moved onto the network, and the notices about the network.
struct Network<'a> {
    ds: &'a Dataset,
    graph: Graph,
    unit_m: f64,
    snap_m: f64,
    notes: Vec<String>,
}

/// Reads the `network`, `costField`, `units` and `snapDistance` parameters and builds (or reuses)
/// the graph over the network's selected lines, counting lines with no cost or closed by it, and
/// segments with invalid coordinates, in notices. An error when no line is left to search.
fn network<'a>(inp: &'a dyn Inputs, p: &Json, tool: &str) -> Result<Network<'a>, String> {
    let ds = input(inp, p, "network")?;
    if ds.geometry_type != GeometryType::Polyline {
        return Err(format!("{tool} searches a line layer; '{}' holds no lines", ds.name));
    }
    let cost_field = match s(p, "costField").trim() {
        "" => None,
        name => Some(ds.field_index(name).ok_or_else(|| format!("'{}' has no field named '{name}'", ds.name))?),
    };
    let unit_m = to_metres(1.0, s(p, "units"))?;
    let snap_m = match p.get("snapDistance").filter(|v| !v.is_null() && v.as_str().is_none_or(|t| !t.trim().is_empty())) {
        None => SNAP_M,
        Some(_) => num(p, "snapDistance").filter(|d| *d >= 0.0).ok_or("Snap Distance must be a number of metres, 0 or more")?,
    };
    let mut rows = rows(inp, p, "network", ds);
    rows.sort_unstable();
    rows.dedup();
    let mut notes = Vec::new();
    // A line joins the network through its segments: one with no part of two vertices has none.
    let has_segment = |r: usize| match ds.features.get(r).and_then(|f| f.geometry.as_ref()) {
        Some(Geometry::Line(parts)) => parts.iter().any(|part| part.len() >= 2),
        _ => false,
    };
    let no_line = rows.iter().filter(|&&r| !has_segment(r)).count();
    if no_line > 0 {
        notes.push(format!("{tool}: {no_line} feature(s) of '{}' have no line, and were left out of the network", ds.name));
    }
    if let Some(fi) = cost_field {
        // As a route reads them: no number costs the length alone, zero or less closes the line.
        let (mut missing, mut closed) = (0usize, 0usize);
        for &r in &rows {
            crate::control::check()?;
            // Only lines join the network; what has no line is counted once, above.
            if !has_segment(r) {
                continue;
            }
            match ds.value(r, fi).as_f64() {
                Some(w) if w.is_finite() && w > 0.0 => {}
                Some(_) => closed += 1,
                None => missing += 1,
            }
        }
        let field = ds.fields.get(fi).map_or("", |f| f.name.as_str());
        if missing > 0 {
            notes.push(format!("{tool}: {missing} line(s) of '{}' have no number in {field}, and cost their length", ds.name));
        }
        if closed > 0 {
            notes.push(format!(
                "{tool}: {closed} line(s) of '{}' cost zero or less (or no finite number) in {field}, and are closed",
                ds.name
            ));
        }
    }
    let graph = Graph::of(ds, &rows, cost_field)?;
    if graph.overflowing() > 0 {
        let field = cost_field.and_then(|fi| ds.fields.get(fi)).map_or("", |f| f.name.as_str());
        notes.push(format!(
            "{tool}: {} line(s) of '{}' cost so much in {field} that a segment's cost is past the largest number, and are closed",
            graph.overflowing(),
            ds.name
        ));
    }
    if graph.skipped_segments() > 0 {
        notes.push(format!(
            "{tool}: {} segment(s) of '{}' with invalid coordinates were left out of the network",
            graph.skipped_segments(),
            ds.name
        ));
    }
    if graph.segments() == 0 {
        // What left no lines is said with it: the notes would be lost with the run.
        let why = if notes.is_empty() { String::new() } else { format!(" ({})", notes.join("; ")) };
        return Err(format!("'{}' has no lines to search{why}", ds.name));
    }
    Ok(Network { ds, graph, unit_m, snap_m, notes })
}

/// The features of a point layer moved onto the network, in layer order, as (feature id, place),
/// with a notice for each kind left out: no point, several points, or too far from the network.
/// An error when none is left.
fn place(inp: &dyn Inputs, p: &Json, key: &str, net: &mut Network<'_>, tool: &str) -> Result<Vec<(u64, Placed)>, String> {
    let ds = input(inp, p, key)?;
    if ds.geometry_type != GeometryType::Point {
        return Err(format!("{tool} places points; '{}' holds no points", ds.name));
    }
    let mut rows = rows(inp, p, key, ds);
    rows.sort_unstable();
    rows.dedup();
    enum Got {
        Placed(u64, Placed),
        NoPoint,
        Several,
        Far,
    }
    let (graph, snap_m) = (&net.graph, net.snap_m);
    // The graph is in longitude/latitude: each point is given there from the system its layer
    // holds it in (the one the run works in, [`Inputs::work_crs`]); one that can't be has no
    // usable point.
    let to = ds.lonlat_conversion()?;
    crate::control::begin(rows.len());
    let got = crate::parallel::try_map(
        &rows,
        64,
        || (),
        |_, _, &r| {
            let Some(f) = ds.features.get(r) else { return Ok(Got::NoPoint) };
            let at = match &f.geometry {
                Some(Geometry::Point(c)) => *c,
                Some(Geometry::MultiPoint(v)) => match v.as_slice() {
                    [] => return Ok(Got::NoPoint),
                    [c] => *c,
                    _ => return Ok(Got::Several),
                },
                _ => return Ok(Got::NoPoint),
            };
            let Some(at) = to.point(at).filter(|c| c.is_finite() && c.y.abs() <= 90.0) else { return Ok(Got::NoPoint) };
            Ok::<_, String>(graph.place(at, snap_m)?.map_or(Got::Far, |placed| Got::Placed(f.id, placed)))
        },
    )?;
    let (mut placed, mut no_point, mut several, mut far) = (Vec::new(), 0usize, 0usize, 0usize);
    for g in got {
        match g {
            Got::Placed(id, at) => placed.push((id, at)),
            Got::NoPoint => no_point += 1,
            Got::Several => several += 1,
            Got::Far => far += 1,
        }
    }
    let name = &ds.name;
    if no_point > 0 {
        net.notes.push(format!("{tool}: {no_point} feature(s) of '{name}' have no usable point, and were left out"));
    }
    if several > 0 {
        net.notes.push(format!("{tool}: {several} feature(s) of '{name}' hold several points, and were left out"));
    }
    if far > 0 {
        let snap = rustgis_geom::format_distance(snap_m);
        net.notes.push(format!("{tool}: {far} feature(s) of '{name}' lie more than {snap} from '{}', and were left out", net.ds.name));
    }
    if placed.is_empty() {
        let mut why = format!("none of the {} feature(s) of '{name}' could be placed on '{}'", rows.len(), net.ds.name);
        if far > 0 {
            why.push_str("; a larger Snap Distance may reach them");
        }
        return Err(why);
    }
    Ok(placed)
}

/// The service area's breaks, ascending, in the tool's units: numbers separated by commas or
/// spaces, a number, or a list of them.
fn breaks(p: &Json) -> Result<Vec<f64>, String> {
    let bad = || "Breaks must be numbers above 0, such as 5, 10, 15".to_string();
    let mut out = match p.get("breaks") {
        Some(Json::Number(n)) => vec![n.as_f64().ok_or_else(bad)?],
        Some(Json::String(t)) => t
            .split([',', ' ', ';'])
            .filter(|v| !v.trim().is_empty())
            .map(|v| v.trim().parse::<f64>().map_err(|_| bad()))
            .collect::<Result<_, _>>()?,
        Some(Json::Array(a)) => a.iter().map(|v| v.as_f64().ok_or_else(bad)).collect::<Result<_, _>>()?,
        _ => return Err("Breaks are required, such as 5, 10, 15".into()),
    };
    if out.is_empty() || out.iter().any(|b| !b.is_finite() || *b <= 0.0) {
        return Err(bad());
    }
    out.sort_by(f64::total_cmp);
    out.dedup();
    if out.len() > MAX_BREAKS {
        return Err(format!("Service Area takes at most {MAX_BREAKS} breaks"));
    }
    Ok(out)
}

/// The parts of a stretch of network from `p` (reached at cost `cp`, from start `lp`) to `q`
/// (`cq`, `lq`), costing `c` from end to end, that each band of `limits` reaches: (band, start,
/// from, to), `from` and `to` being fractions of the way from `p` to `q`. A place on it costs the
/// lesser of reaching it from either end, so the cost rises from each end to a peak between them;
/// a part is reached from the end it lies on the side of.
fn bands(cp: f64, cq: f64, c: f64, lp: Option<usize>, lq: Option<usize>, limits: &[f64]) -> Vec<(usize, usize, f64, f64)> {
    let peak = match (cp.is_finite(), cq.is_finite()) {
        (false, false) => return Vec::new(),
        (true, false) => 1.0,
        (false, true) => 0.0,
        (true, true) => ((cq - cp + c) / (2.0 * c)).clamp(0.0, 1.0),
    };
    let mut out = Vec::new();
    let mut lo = 0.0;
    for (k, &hi) in limits.iter().enumerate() {
        if let Some(lp) = lp.filter(|_| cp.is_finite()) {
            let (from, to) = (((lo - cp) / c).max(0.0), ((hi - cp) / c).min(peak));
            if to > from {
                out.push((k, lp, from, to));
            }
        }
        if let Some(lq) = lq.filter(|_| cq.is_finite()) {
            let (from, to) = ((1.0 - (hi - cq) / c).max(peak), (1.0 - (lo - cq) / c).min(1.0));
            if to > from {
                out.push((k, lq, from, to));
            }
        }
        lo = hi;
    }
    out.sort_by(|a, b| a.2.total_cmp(&b.2));
    out
}

/// The place a fraction `t` of the way from `a` to `b`, exactly `a` and `b` at the ends. A segment
/// across ±180° is followed the short way, as it is measured, and each place written within
/// −180° to 180°, as the line's own vertices are.
fn along(a: Coord, b: Coord, t: f64) -> Coord {
    if t <= 0.0 {
        a
    } else if t >= 1.0 {
        b
    } else {
        let dx = b.x - a.x;
        let dx = if dx.abs() > 180.0 { (dx + 180.0).rem_euclid(360.0) - 180.0 } else { dx };
        let x = a.x + t * dx;
        let x = if (-180.0..=180.0).contains(&x) { x } else { (x + 180.0).rem_euclid(360.0) - 180.0 };
        Coord::new(x, a.y + t * (b.y - a.y))
    }
}

/// Service Area: the lines reached within each break from the nearest facility, each line cut
/// where a break or the facility it is nearest changes. One search from every facility at once,
/// then one pass over the network's segments.
pub(super) fn service_area(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    const TOOL: &str = "Service Area";
    let mut net = network(inp, p, TOOL)?;
    let breaks = breaks(p)?;
    let limits: Vec<f64> = breaks.iter().map(|b| b * net.unit_m).collect();
    let facilities = place(inp, p, "facilities", &mut net, TOOL)?;
    let graph = &net.graph;
    let starts: Vec<Start> = facilities.iter().enumerate().flat_map(|(i, (_, at))| graph.starts(at, i)).collect();
    let mut search = Search::new(graph);
    search.run(graph, &starts, limits.last().copied().unwrap_or(0.0), true, |_| false)?;

    // A facility's own segment is reached from the facility itself, at no cost, as well as from
    // its ends: the segment is cut there.
    let mut on: HashMap<usize, Vec<(f64, Coord, usize)>> = HashMap::new();
    for (i, (_, at)) in facilities.iter().enumerate() {
        on.entry(at.seg).or_default().push((at.to_a, at.at, i));
    }
    for v in on.values_mut() {
        v.sort_by(|a, b| a.0.total_cmp(&b.0));
    }
    // Each line's parts in each band, from each facility, joined where they meet.
    let mut parts: BTreeMap<(usize, usize, usize), Vec<Vec<Coord>>> = BTreeMap::new();
    crate::control::begin(graph.segments());
    for i in 0..graph.segments() {
        crate::control::check()?;
        crate::control::reached(i);
        let Some(sg) = graph.segment(i) else { continue };
        let mut stops = vec![(sg.from, search.cost(sg.a), search.source(sg.a))];
        stops.extend(on.get(&i).into_iter().flatten().map(|&(_, at, f)| (at, 0.0, Some(f))));
        // Facilities at one place: the first of them (in layer order, which the sort keeps) has it.
        stops.dedup_by(|later, kept| later.0 == kept.0 && later.2.is_some() && kept.2.is_some() && later.1 == 0.0 && kept.1 == 0.0);
        stops.push((sg.to, search.cost(sg.b), search.source(sg.b)));
        for w in stops.windows(2) {
            let &[(a, ca, la), (b, cb, lb)] = w else { continue };
            let c = rustgis_geom::wgs84_distance_m(a, b) * sg.weight;
            if c.is_nan() || c <= 0.0 {
                continue;
            }
            for (band, facility, from, to) in bands(ca, cb, c, la, lb, &limits) {
                let (start, end) = (along(a, b, from), along(a, b, to));
                let lines = parts.entry((sg.row, band, facility)).or_default();
                match lines.last_mut() {
                    Some(line) if line.last() == Some(&start) => line.push(end),
                    _ => lines.push(vec![start, end]),
                }
            }
        }
    }
    crate::control::finish();

    let ds = net.ds;
    let mut out = copy_schema(ds, out_name(p, format!("{}_ServiceArea", ds.name)), GeometryType::Polyline);
    for (name, ty) in [("facility_fid", FieldType::Integer), ("from_break", FieldType::Double), ("to_break", FieldType::Double)] {
        let name = unique_field(&out, name);
        out.fields.push(Field::new(name, ty));
    }
    let mut reached = HashSet::new();
    // Made in longitude/latitude, on the graph; each line held as the network is, in the system
    // the run works in, as it is added (the output is never made twice).
    let to_own = Crs::LonLat.to(ds.coord_crs())?;
    let held = |lines: Vec<Vec<Coord>>| -> Result<Vec<Vec<Coord>>, String> {
        if to_own.is_identity() {
            return Ok(lines);
        }
        lines
            .into_iter()
            .map(|part| {
                part.into_iter()
                    .map(|c| {
                        to_own.point(c).filter(|c| c.is_finite()).ok_or_else(|| {
                            format!("{TOOL}: a place it reached can't be held in {}, the system of '{}'", ds.coord_crs().label(), ds.name)
                        })
                    })
                    .collect()
            })
            .collect()
    };
    for ((row, band, facility), lines) in parts {
        crate::control::check()?;
        let Some(f) = ds.features.get(row) else { continue };
        reached.insert(facility);
        let mut attrs: Vec<Value> = (0..ds.fields.len()).map(|i| f.attrs.get(i).cloned().unwrap_or(Value::Null)).collect();
        let fid = facilities.get(facility).map_or(Value::Null, |(id, _)| Value::Int(id.cast_signed()));
        let from = if band == 0 { 0.0 } else { breaks.get(band - 1).copied().unwrap_or(0.0) };
        attrs.extend([fid, Value::Float(from), Value::Float(breaks.get(band).copied().unwrap_or(0.0))]);
        out.push(Some(Geometry::Line(held(lines)?)), attrs);
    }
    out.held_in = ds.coord_crs();
    out.crs = ds.crs;
    let units = if s(p, "units").is_empty() { "Kilometers" } else { s(p, "units") };
    let mut messages = vec![format!(
        "{TOOL}: {} line part(s) reached from {} of {} facilities, in {units}",
        out.features.len(),
        reached.len(),
        facilities.len()
    )];
    messages.append(&mut net.notes);
    Ok(ToolResult { losses: Vec::new(), output: Output::Layer(out), messages })
}

/// OD Cost Matrix: the cost and length of the cheapest way from each origin to each destination,
/// one search per origin, the origins on several threads. A search ends once it has settled both
/// ends of every destination's segment, or passed the cutoff.
pub(super) fn od_cost_matrix(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    od_cost_matrix_on(inp, p, None)
}

/// [`od_cost_matrix`] on `threads` threads (none: as many as the memory and origins allow).
pub(super) fn od_cost_matrix_on(inp: &dyn Inputs, p: &Json, threads: Option<usize>) -> Result<ToolResult, String> {
    const TOOL: &str = "OD Cost Matrix";
    let mut net = network(inp, p, TOOL)?;
    let given = |k: &str| p.get(k).filter(|v| !v.is_null() && v.as_str().is_none_or(|t| !t.trim().is_empty())).is_some();
    let cutoff = match given("cutoff") {
        false => None,
        true => Some(num(p, "cutoff").filter(|c| *c >= 0.0).ok_or("Cutoff must be a number, 0 or more")?),
    };
    let count = match given("count") {
        false => None,
        true => Some(
            num(p, "count")
                .filter(|n| *n >= 1.0 && n.fract() == 0.0)
                .map(|n| n.min(MAX_PAIRS as f64) as usize)
                .ok_or("Destinations to Find must be a whole number, 1 or more")?,
        ),
    };
    let origins = place(inp, p, "origins", &mut net, TOOL)?;
    let destinations = place(inp, p, "destinations", &mut net, TOOL)?;
    let per_origin = count.map_or(destinations.len(), |n| n.min(destinations.len()));
    let (graph, unit_m) = (&net.graph, net.unit_m);
    let limit = cutoff.map_or(f64::INFINITY, |c| c * unit_m);
    // The destinations at each end of their segments, and on each segment: a search visits only
    // those it reached, whatever the number of destinations. Once both ends of every destination's
    // segment are settled, nothing nearer is left.
    let mut at_node: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut on_segment: HashMap<usize, Vec<usize>> = HashMap::new();
    for (j, (_, d)) in destinations.iter().enumerate() {
        let Some(sg) = graph.segment(d.seg) else { continue };
        at_node.entry(sg.a).or_default().push(j);
        if sg.b != sg.a {
            at_node.entry(sg.b).or_default().push(j);
        }
        on_segment.entry(d.seg).or_default().push(j);
    }
    let targets = at_node.len();
    let rows = AtomicUsize::new(0);
    // Each thread's search, and at most its rows, candidates and marks over every destination:
    // twice the rows and candidates for the room a growing vector takes, and half the rows again
    // for the sort's scratch.
    let per_destination = 5 * size_of::<(usize, f64, f64)>() / 2 + 2 * size_of::<usize>() + 1;
    let scratch = Search::bytes(graph).saturating_add(destinations.len().saturating_mul(per_destination));
    let threads = threads.unwrap_or_else(|| crate::parallel::threads_for_each(origins.len(), 2, scratch));
    crate::control::begin(origins.len());
    let found = crate::parallel::try_map_each_on(
        &origins,
        threads,
        || (Search::new(graph), vec![false; destinations.len()]),
        |(search, seen), _, (_, o)| {
            let mut settled = 0usize;
            search.run(graph, &graph.starts(o, 0), limit, false, |u| {
                if at_node.contains_key(&u) {
                    settled += 1;
                }
                settled == targets
            })?;
            // A destination within the limit is reached through an end of its segment that costs
            // no more, or lies on the origin's own segment.
            let near = search.reached().filter(|&n| search.cost(n) <= limit).filter_map(|n| at_node.get(&n));
            let mut candidates = Vec::new();
            for &j in near.chain(on_segment.get(&o.seg)).flatten() {
                if let Some(s) = seen.get_mut(j).filter(|s| !**s) {
                    *s = true;
                    candidates.push(j);
                }
            }
            for &j in &candidates {
                if let Some(s) = seen.get_mut(j) {
                    *s = false;
                }
            }
            // Millions of destinations may be reached: Cancel is looked at between them.
            let mut row: Vec<(usize, f64, f64)> = Vec::new();
            for j in candidates {
                crate::control::check()?;
                let Some((_, d)) = destinations.get(j) else { continue };
                let (cost, length) = graph.reach(search, o, d);
                if cost.is_finite() && cost <= limit {
                    row.push((j, cost, length));
                }
            }
            // Counted before the cheapest few are kept: those beyond them are reached.
            let unreached = destinations.len() - row.len();
            row.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
            row.truncate(per_origin);
            // Kept until the table is made: only the rows, not the room every destination took.
            row.shrink_to_fit();
            if rows.fetch_add(row.len(), Ordering::Relaxed).saturating_add(row.len()) > MAX_PAIRS {
                return Err(format!(
                    "{TOOL}: the matrix would hold more than {MAX_PAIRS} rows; set a lower Cutoff or Destinations to Find"
                ));
            }
            Ok((row, unreached))
        },
    )?;
    crate::control::finish();

    let fields = ["origin_fid", "destination_fid", "rank"].map(|n| Field::new(n, FieldType::Integer));
    let mut out = Dataset::new(
        out_name(p, format!("{}_ODCostMatrix", net.ds.name)),
        GeometryType::None,
        fields.into_iter().chain(["cost", "length"].map(|n| Field::new(n, FieldType::Double))).collect(),
    );
    // Ids stored wrapped (a GeoPackage's negative ones) read back as they were written.
    let id = |v: u64| Value::Int(v.cast_signed());
    // Summed wide: origins by destinations can pass a 32-bit count in the browser.
    let mut unreached = 0u64;
    // One origin may have millions of rows: Cancel is looked at between them too.
    for ((oid, _), (row, missed)) in origins.iter().zip(&found) {
        crate::control::check()?;
        unreached = unreached.saturating_add(u64::try_from(*missed).unwrap_or(u64::MAX));
        for (rank, &(j, cost, length)) in row.iter().enumerate() {
            crate::control::check()?;
            let did = destinations.get(j).map_or(Value::Null, |(d, _)| id(*d));
            out.push(None, vec![id(*oid), did, Value::Int(rank as i64 + 1), Value::Float(cost / unit_m), Value::Float(length / unit_m)]);
        }
    }
    let units = if s(p, "units").is_empty() { "Kilometers" } else { s(p, "units") };
    let mut messages = vec![format!(
        "{TOOL}: {} pair(s) from {} origin(s) to {} destination(s), in {units}",
        out.features.len(),
        origins.len(),
        destinations.len()
    )];
    if unreached > 0 {
        messages.push(match cutoff {
            None => format!("{TOOL}: {unreached} origin–destination pair(s) aren't connected by '{}', and have no row", net.ds.name),
            Some(c) => {
                format!("{TOOL}: {unreached} origin–destination pair(s) cost more than {c} {units} or aren't connected, and have no row")
            }
        });
    }
    messages.append(&mut net.notes);
    Ok(ToolResult { losses: Vec::new(), output: Output::Table(out), messages })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_geom::wgs84_distance_m as dist;
    use serde_json::json;

    struct Map(HashMap<String, Dataset>);
    impl Inputs for Map {
        fn dataset(&self, layer: &str) -> Option<&Dataset> {
            self.0.get(layer)
        }
        fn selection(&self, _: &str) -> &[usize] {
            &[]
        }
    }

    fn c(x: f64, y: f64) -> Coord {
        Coord::new(x, y)
    }

    /// Roads near the equator, about 1.1 km a hundredth of a degree: a line east from (0, 0) to
    /// (0.02, 0) with a vertex at 0.01, a line north from that vertex, a line on east to 0.03
    /// costing twice its length, and a line far away that touches none of them.
    fn roads() -> Dataset {
        let mut ds = Dataset::new("roads", GeometryType::Polyline, vec![Field::new("cost", FieldType::Double)]);
        for (line, cost) in [
            (vec![c(0.0, 0.0), c(0.01, 0.0), c(0.02, 0.0)], 1.0),
            (vec![c(0.01, 0.0), c(0.01, 0.01)], 1.0),
            (vec![c(0.02, 0.0), c(0.03, 0.0)], 2.0),
            (vec![c(1.0, 1.0), c(1.01, 1.0)], 1.0),
        ] {
            ds.push(Some(Geometry::Line(vec![line])), vec![Value::Float(cost)]);
        }
        ds
    }

    /// A point layer with a feature for each place, one with no geometry for each `None`.
    fn points(name: &str, at: &[Option<Coord>]) -> Dataset {
        let mut ds = Dataset::new(name, GeometryType::Point, vec![]);
        for p in at {
            ds.push(p.map(Geometry::Point), vec![]);
        }
        ds
    }

    fn map(layers: Vec<Dataset>) -> Map {
        Map(layers.into_iter().map(|d| (d.name.clone(), d)).collect())
    }

    /// The layer or table a tool made.
    fn dataset(r: &ToolResult) -> &Dataset {
        match &r.output {
            Output::Layer(d) | Output::Table(d) => d,
            _ => panic!("expected a layer or table"),
        }
    }

    /// Metres along every part of a feature's lines.
    fn length(f: &Feature) -> f64 {
        let Some(Geometry::Line(parts)) = &f.geometry else { return 0.0 };
        parts.iter().flat_map(|p| p.windows(2)).map(|w| dist(w[0], w[1])).sum()
    }

    fn field(ds: &Dataset, f: &Feature, name: &str) -> f64 {
        ds.value(ds.features.iter().position(|g| std::ptr::eq(g, f)).unwrap(), ds.field_index(name).unwrap()).as_f64().unwrap()
    }

    /// A facility in the middle of a line reaches both ways along it from where it stands, not
    /// from the line's ends (the fault opengeos/whitebox-wasm#41 found elsewhere), and each break's
    /// band holds the length expected of it, the far line none.
    #[test]
    fn service_area_measures_each_band_from_the_facility_itself() {
        let shops = points("shops", &[Some(c(0.005, 0.0001))]);
        let shop = shops.features[0].id as f64;
        let inputs = map(vec![roads(), shops]);
        let r = service_area(&inputs, &json!({"network": "roads", "facilities": "shops", "breaks": "0.3, 0.8"})).unwrap();
        assert_eq!(r.messages.len(), 1, "clean input, no notices: {:?}", r.messages);
        let out = dataset(&r);
        let band = |to: f64| -> f64 { out.features.iter().filter(|f| field(out, f, "to_break") == to).map(length).sum() };
        // Half the first segment lies on each side of the shop; 300 m each way, then 800 m: west
        // to the dead end, east to the vertex and on along both lines that leave it.
        let half = dist(c(0.005, 0.0), c(0.0, 0.0));
        assert!((band(0.3) - 600.0).abs() < 0.5, "{}", band(0.3));
        let second = 2.0 * (half - 300.0) + 2.0 * (800.0 - half);
        assert!((band(0.8) - second).abs() < 0.5, "{} vs {second}", band(0.8));
        // West of the shop, on its own segment, within the first band.
        let west = out.features.iter().any(|f| {
            field(out, f, "to_break") == 0.3 && matches!(&f.geometry, Some(Geometry::Line(p)) if p.iter().flatten().any(|q| q.x < 0.004))
        });
        assert!(west, "the facility's own segment is reached westward");
        assert!(
            out.features.iter().all(|f| field(out, f, "facility_fid") == shop && field(out, f, "from_break") < field(out, f, "to_break"))
        );
        assert!(out.features.iter().all(|f| matches!(&f.geometry, Some(Geometry::Line(p)) if p.iter().flatten().all(|q| q.x < 0.5))));
        assert!(out.fields.iter().any(|f| f.name == "cost"), "the network's own fields are kept");
    }

    /// Each part of a line goes to the facility nearest along the network, cut where they are
    /// equally near.
    #[test]
    fn service_area_gives_each_part_to_the_nearest_facility() {
        let shops = points("shops", &[Some(c(0.0, 0.0)), Some(c(0.016, 0.0))]);
        let (first, second) = (shops.features[0].id as f64, shops.features[1].id as f64);
        let inputs = map(vec![roads(), shops]);
        let r = service_area(&inputs, &json!({"network": "roads", "facilities": "shops", "breaks": 5})).unwrap();
        let out = dataset(&r);
        let east_end = |fid: f64| -> f64 {
            out.features
                .iter()
                .filter(|f| field(out, f, "facility_fid") == fid)
                .filter_map(|f| match &f.geometry {
                    Some(Geometry::Line(p)) => p.iter().flatten().filter(|q| q.y == 0.0).map(|q| q.x).reduce(f64::max),
                    _ => None,
                })
                .fold(f64::MIN, f64::max)
        };
        assert!((east_end(first) - 0.008).abs() < 1e-6, "{}", east_end(first));
        assert!(east_end(second) > 0.029, "the second shop reaches the end of the costly line: {}", east_end(second));
    }

    /// A part of a segment across ±180° runs the short way, near the antimeridian, not round the
    /// world; segments elsewhere are interpolated as before.
    #[test]
    fn a_segment_across_the_antimeridian_is_cut_the_short_way() {
        let mid = along(c(179.99, 0.0), c(-179.99, 0.0), 0.25);
        assert!((mid.x - 179.995).abs() < 1e-9, "{mid:?}");
        let over = along(c(179.99, 0.0), c(-179.99, 0.0), 0.75);
        assert!((over.x + 179.995).abs() < 1e-9, "{over:?}");
        assert_eq!(along(c(1.0, 2.0), c(3.0, 4.0), 0.5), c(2.0, 3.0));
        let mut lines = Dataset::new("roads", GeometryType::Polyline, vec![]);
        lines.push(Some(Geometry::Line(vec![vec![c(179.99, 0.0), c(-179.99, 0.0)]])), vec![]);
        let inputs = map(vec![lines, points("shops", &[Some(c(179.99, 0.0))])]);
        let r = service_area(&inputs, &json!({"network": "roads", "facilities": "shops", "breaks": "0.5"})).unwrap();
        let xs: Vec<f64> = dataset(&r)
            .features
            .iter()
            .flat_map(|f| match &f.geometry {
                Some(Geometry::Line(p)) => p.iter().flatten().map(|q| q.x).collect(),
                _ => Vec::new(),
            })
            .collect();
        assert!(!xs.is_empty() && xs.iter().all(|x| x.abs() > 179.98), "{xs:?}");
    }

    /// Two facilities at one place in the middle of a line: the first has every part reached from
    /// there, on both sides, not one side each.
    #[test]
    fn coincident_facilities_go_to_the_first() {
        let shops = points("shops", &[Some(c(0.005, 0.0)), Some(c(0.005, 0.0))]);
        let first = shops.features[0].id as f64;
        let inputs = map(vec![roads(), shops]);
        let r = service_area(&inputs, &json!({"network": "roads", "facilities": "shops", "breaks": "5"})).unwrap();
        let out = dataset(&r);
        assert!(
            out.features.iter().all(|f| field(out, f, "facility_fid") == first),
            "{:?}",
            out.features.iter().map(|f| field(out, f, "facility_fid")).collect::<Vec<_>>()
        );
    }

    /// Costs and lengths through the network, with the cost field doubling one line; along one
    /// segment directly; nothing to a separate network, said with a count; the nearest only, and
    /// within a cutoff.
    #[test]
    fn od_cost_matrix_measures_the_cheapest_way() {
        let origins = points("from", &[Some(c(0.0, 0.0)), Some(c(1.005, 1.0)), Some(c(0.002, 0.0))]);
        let destinations = points("to", &[Some(c(0.03, 0.0)), Some(c(0.01, 0.01)), Some(c(0.004, 0.0))]);
        let o: Vec<f64> = origins.features.iter().map(|f| f.id as f64).collect();
        let d: Vec<f64> = destinations.features.iter().map(|f| f.id as f64).collect();
        let inputs = map(vec![roads(), origins, destinations]);
        let run = |extra: Json| {
            let mut p = json!({"network": "roads", "origins": "from", "destinations": "to", "costField": "cost", "units": "Meters"});
            p.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            od_cost_matrix(&inputs, &p).unwrap()
        };
        let r = run(json!({}));
        let out = dataset(&r);
        let row = |i: usize, j: usize| {
            out.features
                .iter()
                .find(|f| field(out, f, "origin_fid") == o[i] && field(out, f, "destination_fid") == d[j])
                .map(|f| (field(out, f, "cost"), field(out, f, "length")))
        };
        let (a, b, cc) = (dist(c(0.0, 0.0), c(0.01, 0.0)), dist(c(0.01, 0.0), c(0.02, 0.0)), dist(c(0.02, 0.0), c(0.03, 0.0)));
        let (cost, len) = row(0, 0).unwrap();
        assert!((cost - (a + b + 2.0 * cc)).abs() < 1e-6 && (len - (a + b + cc)).abs() < 1e-6, "{cost} {len}");
        let (cost, _) = row(0, 1).unwrap();
        assert!((cost - (a + dist(c(0.01, 0.0), c(0.01, 0.01)))).abs() < 1e-6);
        let (cost, _) = row(2, 2).unwrap();
        assert!((cost - dist(c(0.002, 0.0), c(0.004, 0.0))).abs() < 1e-6, "straight along the segment: {cost}");
        assert!(row(1, 0).is_none());
        assert_eq!(out.features.len(), 6);
        assert!(r.messages.iter().any(|m| m.contains("3 origin–destination pair(s) aren't connected")), "{:?}", r.messages);

        let nearest = run(json!({"count": 1}));
        let out = dataset(&nearest);
        assert_eq!(out.features.len(), 2);
        assert!(out.features.iter().all(|f| field(out, f, "rank") == 1.0));
        let within = run(json!({"cutoff": 1500}));
        assert!(dataset(&within).features.iter().all(|f| field(dataset(&within), f, "cost") <= 1500.0));
        assert_eq!(dataset(&within).features.len(), 2, "each of the two connected origins to its nearest destination");
    }

    /// What's left out is said, one notice per kind with a count; when nothing is left, an error.
    #[test]
    fn points_left_out_are_counted() {
        let shops = points("shops", &[Some(c(0.005, 0.0)), None, Some(c(0.5, 0.5))]);
        let inputs = map(vec![roads(), shops, points("nowhere", &[Some(c(0.5, 0.5))])]);
        let r = service_area(&inputs, &json!({"network": "roads", "facilities": "shops", "breaks": "1"})).unwrap();
        assert!(r.messages.iter().any(|m| m.contains("1 feature(s) of 'shops' have no usable point")), "{:?}", r.messages);
        assert!(r.messages.iter().any(|m| m.contains("1 feature(s) of 'shops' lie more than 1.0 km")), "{:?}", r.messages);
        let e = service_area(&inputs, &json!({"network": "roads", "facilities": "nowhere", "breaks": "1"})).unwrap_err();
        assert!(e.contains("Snap Distance"), "{e}");
        for bad in [json!("0"), json!("-1, 2"), json!("a"), json!([])] {
            assert!(service_area(&inputs, &json!({"network": "roads", "facilities": "shops", "breaks": bad})).is_err(), "{bad}");
        }
        let mut gaps = roads();
        gaps.push(None, vec![Value::Float(1.0)]);
        gaps.push(Some(Geometry::Point(c(0.0, 0.0))), vec![Value::Null]);
        gaps.push(Some(Geometry::Line(vec![vec![c(0.5, 0.5)]])), vec![Value::Null]);
        let inputs = map(vec![gaps, points("shops", &[Some(c(0.005, 0.0))])]);
        let r = service_area(&inputs, &json!({"network": "roads", "facilities": "shops", "breaks": "1", "costField": "cost"})).unwrap();
        assert!(r.messages.iter().any(|m| m.contains("3 feature(s) of 'roads' have no line, and were left out")), "{:?}", r.messages);
        assert!(!r.messages.iter().any(|m| m.contains("no number in cost")), "a feature with no line is counted once: {:?}", r.messages);
        let mut costs = roads();
        costs.features[3].attrs[0] = Value::Null;
        costs.features[2].attrs[0] = Value::Float(0.0);
        let inputs = map(vec![costs, points("shops", &[Some(c(0.005, 0.0))])]);
        let r = service_area(&inputs, &json!({"network": "roads", "facilities": "shops", "breaks": "5", "costField": "cost"})).unwrap();
        assert!(r.messages.iter().any(|m| m.contains("1 line(s) of 'roads' have no number in cost")), "{:?}", r.messages);
        assert!(
            r.messages.iter().any(|m| m.contains("1 line(s) of 'roads' cost zero or less (or no finite number) in cost, and are closed")),
            "{:?}",
            r.messages
        );
        assert!(!r.messages.iter().any(|m| m.contains("past the largest number")), "{:?}", r.messages);
        // A cost so large its segments' costs overflow closes the line too, counted apart.
        let mut huge = roads();
        huge.features[3].attrs[0] = Value::Float(1e306);
        let inputs = map(vec![huge, points("shops", &[Some(c(0.005, 0.0))])]);
        let r = service_area(&inputs, &json!({"network": "roads", "facilities": "shops", "breaks": "5", "costField": "cost"})).unwrap();
        assert!(
            r.messages.iter().any(|m| m
                .contains("1 line(s) of 'roads' cost so much in cost that a segment's cost is past the largest number, and are closed")),
            "{:?}",
            r.messages
        );
        // Every line closed so: the refusal says why.
        let mut all = roads();
        for f in all.features.iter_mut() {
            f.attrs[0] = Value::Float(1e306);
        }
        let inputs = map(vec![all, points("shops", &[Some(c(0.005, 0.0))])]);
        let e = service_area(&inputs, &json!({"network": "roads", "facilities": "shops", "breaks": "5", "costField": "cost"})).unwrap_err();
        assert!(e.contains("has no lines to search") && e.contains("4 line(s) of 'roads' cost so much"), "{e}");
    }

    /// A point just across ±180° from a line is moved onto it, about 110 m away, not left out as
    /// if it were the whole Earth's width away.
    #[test]
    fn a_point_across_the_antimeridian_snaps_to_the_line_beside_it() {
        let mut lines = Dataset::new("roads", GeometryType::Polyline, vec![]);
        lines.push(Some(Geometry::Line(vec![vec![c(-180.0, 0.0), c(-179.99, 0.0)]])), vec![]);
        let inputs = map(vec![lines, points("shops", &[Some(c(179.999, 0.0))])]);
        let r = service_area(&inputs, &json!({"network": "roads", "facilities": "shops", "breaks": "0.5"})).unwrap();
        assert_eq!(r.messages.len(), 1, "{:?}", r.messages);
        assert!(!dataset(&r).features.is_empty());
    }

    /// Ids stored wrapped, as a GeoPackage's negative ones are, are written back as they were;
    /// and an origin that reaches one destination but not another says so even when only the
    /// nearest is kept.
    #[test]
    fn od_rows_keep_negative_ids_and_count_pairs_past_the_kept_ones() {
        let mut from = points("from", &[Some(c(0.0, 0.0))]);
        from.features[0].id = (-2i64).cast_unsigned();
        let to = points("to", &[Some(c(0.01, 0.0)), Some(c(1.005, 1.0))]);
        let inputs = map(vec![roads(), from, to]);
        let r = od_cost_matrix(&inputs, &json!({"network": "roads", "origins": "from", "destinations": "to", "count": 1})).unwrap();
        let out = dataset(&r);
        assert_eq!(out.features.len(), 1);
        assert_eq!(field(out, &out.features[0], "origin_fid"), -2.0);
        assert!(r.messages.iter().any(|m| m.contains("1 origin–destination pair(s) aren't connected")), "{:?}", r.messages);
        let r = service_area(
            &map(vec![roads(), {
                let mut shops = points("shops", &[Some(c(0.005, 0.0))]);
                shops.features[0].id = (-7i64).cast_unsigned();
                shops
            }]),
            &json!({"network": "roads", "facilities": "shops", "breaks": "1"}),
        )
        .unwrap();
        assert!(dataset(&r).features.iter().all(|f| field(dataset(&r), f, "facility_fid") == -7.0));
    }

    /// Every origin's row the same, bit for bit, on one thread or several.
    #[test]
    fn od_cost_matrix_is_the_same_on_any_thread_count() {
        let at: Vec<Option<Coord>> = (0..40).map(|i| Some(c(f64::from(i) * 0.00075, if i % 3 == 0 { 0.0 } else { 0.001 }))).collect();
        let inputs = map(vec![roads(), points("pts", &at)]);
        let p = json!({"network": "roads", "origins": "pts", "destinations": "pts", "snapDistance": 200});
        let rows = |threads| {
            let r = od_cost_matrix_on(&inputs, &p, Some(threads)).unwrap();
            dataset(&r).features.iter().map(|f| f.attrs.iter().map(|v| format!("{v:?}")).collect::<Vec<_>>()).collect::<Vec<_>>()
        };
        let one = rows(1);
        assert!(one.len() > 100);
        assert_eq!(one, rows(4));
    }
}
