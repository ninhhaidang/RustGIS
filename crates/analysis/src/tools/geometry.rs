//! Geometry walks in stored coordinates: no projection, snapping or topology repair.
use super::*;

/// The supported per-feature walks, kept exhaustive at their internal dispatch boundary.
#[derive(Clone, Copy)]
pub(super) enum Conversion {
    PolygonToLine,
    FeatureEnvelopeToPolygon,
}

/// Runs the boundary and envelope conversions; skipped empty or degenerate pieces are counted.
pub(super) fn convert(inp: &dyn Inputs, p: &Json, mode: Conversion) -> Result<ToolResult, String> {
    let ds = input(inp, p, "input")?;
    let (tool, expected, kind, suffix) = match mode {
        Conversion::PolygonToLine => ("Polygon To Line", Some(GeometryType::Polygon), GeometryType::Polyline, "Lines"),
        Conversion::FeatureEnvelopeToPolygon => ("Feature Envelope To Polygon", None, GeometryType::Polygon, "Envelopes"),
    };
    if expected.is_some_and(|t| ds.geometry_type != t) || ds.geometry_type == GeometryType::None {
        return Err(format!("{tool}: incompatible input geometry type"));
    }
    let mut out = converted_dataset(ds, p, suffix, kind);
    let mut skipped = 0;
    let row_ids = rows(inp, p, "input", ds);
    crate::control::begin(row_ids.len());
    crate::control::check()?;
    let mut pace = crate::control::Pace::default();
    for row in row_ids {
        crate::control::check()?;
        convert_row(ds, row, mode, tool, &mut out, &mut skipped, &mut pace)?;
        crate::control::advance(1);
    }
    finish(out, skipped, tool)
}

use crate::control::COORD_PACE_CHUNK;

const PART_PACE_WORK: usize = crate::control::PACE_WORK / 65_536;

/// Copies `ring` (closing it if needed) in [`COORD_PACE_CHUNK`]-vertex slices with fallible
/// reservation so a single huge ring checks Cancel via `pace` mid-copy.
fn copy_ring_paced(ring: &[Coord], tool: &str, pace: &mut crate::control::Pace) -> Result<Vec<Coord>, String> {
    let closed = ring.first() == ring.last();
    let needed = ring.len().checked_add(usize::from(!closed)).ok_or_else(|| format!("{tool}: ring has too many vertices"))?;
    let mut line = Vec::new();
    line.try_reserve_exact(needed).map_err(|e| format!("{tool}: not enough memory for {needed} vertices: {e}"))?;
    for chunk in ring.chunks(COORD_PACE_CHUNK) {
        if pace.stop(chunk.len()) {
            return Err(crate::control::Cancelled.into());
        }
        line.extend_from_slice(chunk);
    }
    if !closed && let Some(first) = line.first().copied() {
        line.push(first);
    }
    Ok(line)
}

fn convert_row(
    ds: &Dataset,
    row: usize,
    mode: Conversion,
    tool: &str,
    out: &mut Dataset,
    skipped: &mut usize,
    pace: &mut crate::control::Pace,
) -> Result<(), String> {
    let Some(f) = ds.features.get(row) else { return Ok(()) };
    require_schema(f, ds.fields.len())?;
    let Some(g) = &f.geometry else {
        *skipped += 1;
        return Ok(());
    };
    match (mode, g) {
        (Conversion::PolygonToLine, Geometry::Polygon(polys)) => {
            require_finite_paced(g, pace)?;
            let mut parts = Vec::new();
            if polys.is_empty() {
                *skipped += 1;
            }
            for rings in polys {
                if rings.is_empty() {
                    *skipped += 1;
                }
                for ring in rings {
                    if pace.stop(crate::control::PACE_WORK / 1024) {
                        return Err(crate::control::Cancelled.into());
                    }
                    if ring.len() < 2 {
                        *skipped += 1;
                        continue;
                    }
                    parts.push(copy_ring_paced(ring, tool, pace)?);
                }
            }
            if !parts.is_empty() {
                out.push_next(Some(Geometry::Line(parts)), f.attrs.clone());
            }
        }
        (Conversion::PolygonToLine, _) => {
            require_finite_paced(g, pace)?;
            return Err(format!("{tool}: feature {} has an incompatible geometry", f.id));
        }
        _ => {
            let e = finite_extent_paced(g, pace)?;
            if e.is_empty() || e.width() == 0.0 || e.height() == 0.0 {
                *skipped += 1;
                return Ok(());
            }
            let a = Coord::new(e.min_x, e.min_y);
            let ring = vec![a, Coord::new(e.max_x, e.min_y), Coord::new(e.max_x, e.max_y), Coord::new(e.min_x, e.max_y), a];
            out.push_next(Some(Geometry::Polygon(vec![vec![ring]])), f.attrs.clone());
        }
    }
    Ok(())
}

/// A generated dataset retains schema and loss provenance, without a reloadable source/window.
fn converted_dataset(ds: &Dataset, p: &Json, suffix: &str, kind: GeometryType) -> Dataset {
    let mut out = copy_schema(ds, out_name(p, format!("{}_{suffix}", ds.name)), kind);
    out.crs = ds.crs;
    out.crs_incomplete = ds.crs_incomplete;
    out.zm_dropped = ds.zm_dropped;
    out
}

/// Extra heap storage over a point: one line part and two coordinates, in both the output
/// and its projected drawing cache, including both allocation headers in each copy.
const SEGMENT_EXTRA_BYTES: u64 = 2 * (size_of::<Vec<Coord>>() as u64 + 2 * size_of::<Coord>() as u64 + 2 * ALLOCATION_OVERHEAD);

/// Counts segments and skipped pieces before allocation. Text attributes are repeated for
/// every segment; the browser estimate includes that expansion, not just coordinate storage.
fn size_segments<'a>(
    features: impl IntoIterator<Item = &'a Feature>,
    width: usize,
    estimate: bool,
) -> Result<(usize, Option<u64>, usize), String> {
    let (mut count, mut bytes, mut skipped) = (0usize, 0u64, 0usize);
    let mut pace = crate::control::Pace::default();
    for f in features {
        crate::control::check()?;
        require_schema(f, width)?;
        let Some(g) = &f.geometry else {
            skipped += 1;
            continue;
        };
        require_finite_paced(g, &mut pace)?;
        let Geometry::Line(parts) = g else { return Err(format!("Split Line At Vertices: feature {} is not a line", f.id)) };
        if parts.is_empty() {
            skipped += 1;
        }
        let per_segment =
            if estimate { point_bytes(f.attrs.get(..width).unwrap_or(&f.attrs), width).saturating_add(SEGMENT_EXTRA_BYTES) } else { 0 };
        for part in parts {
            if pace.stop(crate::control::PACE_WORK / 1024) {
                return Err(crate::control::Cancelled.into());
            }
            if part.len() < 2 {
                skipped += 1;
            }
            let n = part.len().saturating_sub(1);
            count = count.checked_add(n).ok_or("Split Line At Vertices: too many segments")?;
            bytes = bytes.saturating_add(u64::try_from(n).unwrap_or(u64::MAX).saturating_mul(per_segment));
        }
    }
    Ok((count, estimate.then_some(bytes), skipped))
}

/// Refuses a browser output before cloning any segment attributes if its estimated memory
/// (plus the shared probe's safety margin) is unavailable.
fn check_segment_room(n: usize, need: u64, has_room: impl FnOnce(u64) -> bool) -> Result<(), String> {
    if has_room(need) {
        return Ok(());
    }
    Err(format!(
        "Split Line At Vertices: {n} segments would need about {} MiB, more than the browser can give now: select fewer features",
        need.div_ceil(1 << 20)
    ))
}

/// Allocates each segment and its attributes fallibly, including long text values.
fn segment_feature(id: u64, pair: &[Coord], own: &[Value], width: usize) -> Result<Feature, TryReserveError> {
    let mut coords = Vec::new();
    coords.try_reserve_exact(pair.len())?;
    coords.extend_from_slice(pair);
    let mut parts = Vec::new();
    parts.try_reserve_exact(1)?;
    parts.push(coords);
    let attrs = clone_attrs(own, width)?;
    Ok(Feature { id, geometry: Some(Geometry::Line(parts)), attrs })
}

/// Copies schema-width attributes fallibly, including text, and pads missing values with null.
fn clone_attrs(own: &[Value], width: usize) -> Result<Vec<Value>, TryReserveError> {
    let mut attrs = Vec::new();
    attrs.try_reserve_exact(width)?;
    for v in own.iter().take(width) {
        attrs.push(try_clone_value(v)?);
    }
    attrs.resize(width, Value::Null);
    Ok(attrs)
}

/// Splits without duplicating an unbounded attribute payload blindly: count once, probe the
/// browser's output memory, and reserve the final list once. Native overcommit remains OS-managed.
pub(super) fn split(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    let ds = input(inp, p, "input")?;
    if ds.geometry_type != GeometryType::Polyline {
        return Err("Split Line At Vertices needs a line layer".into());
    }
    let selected = inp.selection(s(p, "input"));
    let all = 0..if selected.is_empty() { ds.features.len() } else { 0 };
    let features = || selected.iter().copied().chain(all.clone()).filter_map(|r| ds.features.get(r));
    let (n, need, skipped) = size_segments(features(), ds.fields.len(), cfg!(target_arch = "wasm32"))?;
    if let Some(need) = need {
        check_segment_room(n, need, web_has_room)?;
    }
    let no_memory = |e: TryReserveError| format!("Split Line At Vertices: not enough memory for {n} segments: {e}");
    crate::control::begin(n);
    crate::control::check()?;
    let mut segments = Vec::new();
    segments.try_reserve_exact(n).map_err(no_memory)?;
    let mut pace = crate::control::Pace::default();
    for f in features() {
        crate::control::check()?;
        let Some(Geometry::Line(parts)) = &f.geometry else { continue };
        for pair in parts.iter().flat_map(|part| part.windows(2)) {
            if pace.stop(crate::control::PACE_WORK / 1024) {
                return Err(crate::control::Cancelled.into());
            }
            let id =
                u64::try_from(segments.len()).ok().and_then(|n| n.checked_add(1)).ok_or("Split Line At Vertices: too many segments")?;
            match segment_feature(id, pair, &f.attrs, ds.fields.len()) {
                Ok(feature) => {
                    segments.push(feature);
                    crate::control::advance(1);
                }
                Err(e) => {
                    drop(segments);
                    return Err(no_memory(e));
                }
            }
        }
    }
    let mut out = converted_dataset(ds, p, "Segments", GeometryType::Polyline);
    out.features = segments.into();
    finish(out, skipped, "Split Line At Vertices")
}

/// Rejects values without field definitions before output schema normalization could drop them.
fn require_schema(f: &Feature, width: usize) -> Result<(), String> {
    if f.attrs.len() > width {
        return Err(format!("Geometry conversion: feature {} has attribute count {} exceeding schema width {width}", f.id, f.attrs.len()));
    }
    Ok(())
}

/// Walks `g` in [`COORD_PACE_CHUNK`]-coordinate slices, rejecting non-finite coordinates while
/// checking Cancel via `pace` and forwarding each finite coordinate to `visit`.
fn visit_finite_coords_paced(g: &Geometry, pace: &mut crate::control::Pace, mut visit: impl FnMut(Coord)) -> Result<(), String> {
    fn visit_slice(coords: &[Coord], pace: &mut crate::control::Pace, visit: &mut impl FnMut(Coord)) -> Result<(), String> {
        if pace.stop(PART_PACE_WORK) {
            return Err(crate::control::Cancelled.into());
        }
        for chunk in coords.chunks(COORD_PACE_CHUNK) {
            if pace.stop(chunk.len()) {
                return Err(crate::control::Cancelled.into());
            }
            for &c in chunk {
                if !c.is_finite() {
                    return Err("Geometry conversion requires finite coordinates".into());
                }
                visit(c);
            }
        }
        Ok(())
    }
    match g {
        Geometry::Point(c) => {
            if !c.is_finite() {
                return Err("Geometry conversion requires finite coordinates".into());
            }
            visit(*c);
        }
        Geometry::MultiPoint(pts) => visit_slice(pts, pace, &mut visit)?,
        Geometry::Line(parts) => {
            for part in parts {
                visit_slice(part, pace, &mut visit)?;
            }
        }
        Geometry::Polygon(polys) => {
            for rings in polys {
                if rings.is_empty() && pace.stop(PART_PACE_WORK) {
                    return Err(crate::control::Cancelled.into());
                }
                for ring in rings {
                    visit_slice(ring, pace, &mut visit)?;
                }
            }
        }
    }
    Ok(())
}

/// Walks `g` in [`COORD_PACE_CHUNK`]-coordinate slices, rejecting non-finite coordinates and
/// computing its bounding [`Extent`] while checking Cancel via `pace`.
fn finite_extent_paced(g: &Geometry, pace: &mut crate::control::Pace) -> Result<Extent, String> {
    let mut extent = Extent::EMPTY;
    visit_finite_coords_paced(g, pace, |c| extent.expand(c))?;
    Ok(extent)
}

/// Rejects invalid coordinates before extent calculation or endpoint hashing can hide them,
/// checking Cancel via `pace` inside large geometries without computing an unused extent.
fn require_finite_paced(g: &Geometry, pace: &mut crate::control::Pace) -> Result<(), String> {
    visit_finite_coords_paced(g, pace, |_| {})
}

/// Returns counted loss notices, refusing a result in which every usable geometry was lost.
fn finish(out: Dataset, skipped: usize, tool: &str) -> Result<ToolResult, String> {
    if skipped > 0 && out.features.is_empty() {
        return Err(format!("{tool}: no usable output; skipped {skipped} empty or degenerate features/parts"));
    }
    let messages = if skipped == 0 { vec![] } else { vec![format!("{tool}: skipped {skipped} empty or degenerate features/parts")] };
    Ok(ToolResult { losses: Vec::new(), output: Output::Layer(out), messages })
}

/// Hashes exact coordinates, normalizing signed zero to match coordinate equality.
fn endpoint(c: &Coord) -> (u64, u64) {
    let bits = |x: f64| if x == 0.0 { 0 } else { x.to_bits() };
    (bits(c.x), bits(c.y))
}

/// Pre-counts valid line parts (`len >= 2`) across `row_ids` so [`unsplit`] reports one monotonic
/// progress total (`rows + edges`) across both edge collection and chain walking.
fn count_unsplit_edges(ds: &Dataset, row_ids: &[usize], pace: &mut crate::control::Pace) -> Result<usize, String> {
    let mut edges = 0usize;
    for &row in row_ids {
        if pace.stop(PART_PACE_WORK) {
            return Err(crate::control::Cancelled.into());
        }
        let Some(f) = ds.features.row(row) else { continue };
        let Some(Geometry::Line(parts)) = f.geometry() else { continue };
        for part in parts {
            if pace.stop(PART_PACE_WORK) {
                return Err(crate::control::Cancelled.into());
            }
            if part.len() >= 2 {
                edges = edges.saturating_add(1);
            }
        }
    }
    Ok(edges)
}

/// Appends `part` (forward or reversed, optionally skipping the shared junction vertex) in
/// [`COORD_PACE_CHUNK`]-vertex slices with fallible reservation so a single huge edge checks
/// Cancel via `pace` mid-copy.
fn extend_part_paced(
    line: &mut Vec<Coord>,
    part: &[Coord],
    reverse: bool,
    skip_first: bool,
    pace: &mut crate::control::Pace,
) -> Result<(), String> {
    let slice = if !skip_first {
        part
    } else if reverse {
        part.get(..part.len().saturating_sub(1)).unwrap_or(&[])
    } else {
        part.get(1..).unwrap_or(&[])
    };
    line.try_reserve(slice.len()).map_err(|e| format!("Unsplit Line: not enough memory for vertices: {e}"))?;
    if reverse {
        for chunk in slice.rchunks(COORD_PACE_CHUNK) {
            if pace.stop(chunk.len()) {
                return Err(crate::control::Cancelled.into());
            }
            line.extend(chunk.iter().rev().copied());
        }
    } else {
        for chunk in slice.chunks(COORD_PACE_CHUNK) {
            if pace.stop(chunk.len()) {
                return Err(crate::control::Cancelled.into());
            }
            line.extend_from_slice(chunk);
        }
    }
    Ok(())
}

/// Joins endpoint-connected parts only through degree-two nodes with identical attributes.
/// Crossings without endpoint vertices are untouched. Two passes start open chains before cycles.
pub(super) fn unsplit(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    let ds = input(inp, p, "input")?;
    if ds.geometry_type != GeometryType::Polyline {
        return Err("Unsplit Line needs a line layer".into());
    }
    let mut out = converted_dataset(ds, p, "Unsplit", GeometryType::Polyline);
    let mut edges = Vec::new();
    let mut nodes: HashMap<_, Vec<(usize, bool)>> = HashMap::new();
    let mut skipped = 0;
    let row_ids = rows(inp, p, "input", ds);
    let mut pace = crate::control::Pace::default();
    let edge_count = count_unsplit_edges(ds, &row_ids, &mut pace)?;
    crate::control::begin(row_ids.len().saturating_add(edge_count));
    crate::control::check()?;
    for row in row_ids {
        crate::control::check()?;
        unsplit_collect_row(ds, row, &mut edges, &mut nodes, &mut skipped, &mut pace)?;
        crate::control::advance(1);
    }
    // An endpoint is traversable only when both incident edges have the same attributes.
    let joins = |c: &Coord| -> Option<&Vec<(usize, bool)>> {
        let incident = nodes.get(&endpoint(c))?;
        let [(a, _), (b, _)] = incident.as_slice() else { return None };
        let (a, b) = (edges.get(*a)?.1, edges.get(*b)?.1);
        (0..ds.fields.len()).all(|i| a.get(i).unwrap_or(&Value::Null) == b.get(i).unwrap_or(&Value::Null)).then_some(incident)
    };
    let mut used = vec![false; edges.len()];
    for cycles in [false, true] {
        for (seed, (part, attrs)) in edges.iter().enumerate() {
            if pace.stop(crate::control::PACE_WORK / 1024) {
                return Err(crate::control::Cancelled.into());
            }
            if used.get(seed).copied().unwrap_or(true) {
                continue;
            }
            let (Some(first), Some(last)) = (part.first(), part.last()) else { continue };
            let reverse = if joins(first).is_none() {
                false
            } else if joins(last).is_none() {
                true
            } else if cycles {
                false
            } else {
                continue;
            };
            let mut line = Vec::new();
            let (mut id, mut reverse) = (seed, reverse);
            while let Some((part, _)) = edges.get(id) {
                let Some(seen) = used.get_mut(id) else { break };
                if *seen {
                    break;
                }
                crate::control::check()?;
                *seen = true;
                let skip_first = !line.is_empty();
                extend_part_paced(&mut line, part, reverse, skip_first, &mut pace)?;
                crate::control::advance(1);
                let next = line.last().and_then(joins).and_then(|incident| incident.iter().find(|(other, _)| *other != id));
                let Some(&(other, at_end)) = next else { break };
                id = other;
                reverse = at_end;
            }
            let attrs = clone_attrs(attrs, ds.fields.len()).map_err(|e| format!("Unsplit Line: not enough memory for attributes: {e}"))?;
            out.features.try_reserve(1).map_err(|e| format!("Unsplit Line: not enough memory for output rows: {e}"))?;
            out.push_next(Some(Geometry::Line(vec![line])), attrs);
        }
    }
    finish(out, skipped, "Unsplit Line")
}

fn unsplit_collect_row<'a>(
    ds: &'a Dataset,
    row: usize,
    edges: &mut Vec<(&'a Vec<Coord>, &'a Vec<Value>)>,
    nodes: &mut HashMap<(u64, u64), Vec<(usize, bool)>>,
    skipped: &mut usize,
    pace: &mut crate::control::Pace,
) -> Result<(), String> {
    let Some(f) = ds.features.get(row) else { return Ok(()) };
    require_schema(f, ds.fields.len())?;
    let Some(g) = &f.geometry else {
        *skipped += 1;
        return Ok(());
    };
    require_finite_paced(g, pace)?;
    let Geometry::Line(parts) = g else {
        return Err(format!("Unsplit Line: feature {} is not a line", f.id));
    };
    if parts.is_empty() {
        *skipped += 1;
    }
    for part in parts {
        if pace.stop(crate::control::PACE_WORK / 1024) {
            return Err(crate::control::Cancelled.into());
        }
        let (Some(first), Some(last)) = (part.first(), part.last()) else {
            *skipped += 1;
            continue;
        };
        if part.len() < 2 {
            *skipped += 1;
            continue;
        }
        let id = edges.len();
        nodes.entry(endpoint(first)).or_default().push((id, false));
        nodes.entry(endpoint(last)).or_default().push((id, true));
        edges.push((part, &f.attrs));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A short input's text must be charged for every output segment; capacity refusal is
    /// tested without allocating a huge output or relying on a real browser OOM.
    #[test]
    fn split_estimates_repeated_text_and_refuses_unavailable_memory() {
        let features = [Feature {
            id: 1,
            geometry: Some(Geometry::Line(vec![vec![
                Coord::new(0.0, 0.0),
                Coord::new(1.0, 0.0),
                Coord::new(2.0, 0.0),
                Coord::new(3.0, 0.0),
            ]])),
            attrs: vec![Value::Text("x".repeat(4096))],
        }];
        let (n, need, skipped) = size_segments(&features, 1, true).unwrap();
        assert_eq!((n, skipped), (3, 0));
        let need = need.unwrap();
        assert!(need >= 3 * (4096 + SEGMENT_EXTRA_BYTES));
        assert!(
            check_segment_room(n, need, |asked| {
                assert_eq!(asked, need);
                true
            })
            .is_ok()
        );
        assert!(check_segment_room(n, need, |_| false).unwrap_err().contains("3 segments"));
        assert_eq!(size_segments(&features, 1, false).unwrap(), (3, None, 0));
    }

    struct One(Dataset);

    impl Inputs for One {
        fn dataset(&self, name: &str) -> Option<&Dataset> {
            (name == "input").then_some(&self.0)
        }
        fn selection(&self, _: &str) -> &[usize] {
            &[]
        }
    }

    fn poly_square(x: f64) -> Geometry {
        let ring = vec![Coord::new(x, 0.0), Coord::new(x + 1.0, 0.0), Coord::new(x + 1.0, 1.0), Coord::new(x, 0.0)];
        Geometry::Polygon(vec![vec![ring]])
    }

    #[test]
    fn convert_checks_cancel_and_reports_progress() {
        use crate::control::Control;

        let mut ds = Dataset::new("input", GeometryType::Polygon, vec![Field::new("id", FieldType::Integer)]);
        for i in 0..4 {
            ds.push_next(Some(poly_square(i as f64 * 2.0)), vec![Value::Int(i)]);
        }
        ds.push_next(None, vec![Value::Int(4)]);
        let inp = One(ds);
        let params = serde_json::json!({"input": "input"});

        for tool in ["polygonToLine", "featureEnvelopeToPolygon"] {
            let counting = Control::cancelled_at_look(u64::MAX);
            crate::run_with(tool, &inp, &params, &counting).unwrap();
            assert_eq!(counting.progress(), Some((5, 5)), "{tool}");
            assert_eq!(counting.looked(), 6, "{tool}");

            let cancelled = Control::cancelled_at_look(4);
            let err = crate::run_with(tool, &inp, &params, &cancelled).unwrap_err();
            assert!(err.contains("cancelled"), "{tool}: {err}");
            assert_eq!(cancelled.looked(), 4, "{tool}");
            assert_eq!(cancelled.progress(), Some((2, 5)), "{tool}");
        }

        // Single multipolygon with 1,500 rings cancels inside the ring loop via Pace.
        let ring = vec![Coord::new(0.0, 0.0), Coord::new(1.0, 0.0), Coord::new(1.0, 1.0), Coord::new(0.0, 0.0)];
        let mut rings = Vec::with_capacity(1500);
        for _ in 0..1500 {
            rings.push(ring.clone());
        }
        let mut poly_ds = Dataset::new("input", GeometryType::Polygon, vec![]);
        poly_ds.push_next(Some(Geometry::Polygon(vec![rings])), vec![]);
        let poly_inp = One(poly_ds);

        let counting_rings = Control::cancelled_at_look(u64::MAX);
        crate::run_with("polygonToLine", &poly_inp, &params, &counting_rings).unwrap();
        assert_eq!(counting_rings.looked(), 3);

        let cancel_ring = Control::cancelled_at_look(3);
        let err = crate::run_with("polygonToLine", &poly_inp, &params, &cancel_ring).unwrap_err();
        assert!(err.contains("cancelled"), "{err}");
        assert_eq!(cancel_ring.looked(), 3);

        // 800 rings of 1,200 vertices: per-ring and per-chunk units together cross PACE_WORK, so Cancel is looked at inside the ring loop.
        let make_heavy_rings = |verts: usize| {
            let mut ring = Vec::with_capacity(verts);
            for i in 0..verts {
                ring.push(Coord::new(i as f64, 0.0));
            }
            let rings = vec![ring; 800];
            let mut d = Dataset::new("input", GeometryType::Polygon, vec![]);
            d.push_next(Some(Geometry::Polygon(vec![rings])), vec![]);
            One(d)
        };
        let counting_small = Control::cancelled_at_look(u64::MAX);
        crate::run_with("polygonToLine", &make_heavy_rings(4), &params, &counting_small).unwrap();
        assert_eq!(counting_small.looked(), 2);

        let cancel_verts = Control::cancelled_at_look(3);
        let err = crate::run_with("polygonToLine", &make_heavy_rings(1_200), &params, &cancel_verts).unwrap_err();
        assert!(err.contains("cancelled"), "{err}");
        assert_eq!(cancel_verts.looked(), 3);
    }

    #[test]
    fn copy_ring_paced_cancels_mid_ring_across_chunks() {
        use crate::control::{Control, PACE_WORK, Pace};

        let verts = COORD_PACE_CHUNK + 4_464;
        let big_ring: Vec<Coord> = (0..verts).map(|i| Coord::new(i as f64, 0.0)).collect();

        // Prime Pace so chunk 1 (COORD_PACE_CHUNK) stays below PACE_WORK and chunk 2 crosses it mid-ring.
        let cancel_mid = Control::cancelled_at_look(1);
        let err = crate::run_under("copy_ring_paced", &cancel_mid, || {
            let mut pace = Pace::default();
            assert!(!pace.stop(PACE_WORK - verts));
            copy_ring_paced(&big_ring, "Polygon To Line", &mut pace)
        })
        .unwrap_err();
        assert!(err.contains("cancelled"), "{err}");
        assert_eq!(cancel_mid.looked(), 1);

        let counting = Control::cancelled_at_look(u64::MAX);
        let copied = crate::run_under("copy_ring_paced", &counting, || {
            let mut pace = Pace::default();
            assert!(!pace.stop(PACE_WORK - verts));
            copy_ring_paced(&big_ring, "Polygon To Line", &mut pace)
        })
        .unwrap();
        assert_eq!(counting.looked(), 1);
        assert_eq!(copied.len(), verts + 1);
        assert_eq!(copied.first(), copied.last());
    }

    #[test]
    fn finite_extent_paced_cancels_mid_ring_and_rejects_non_finite() {
        use crate::control::{Control, PACE_WORK, Pace};

        let verts = COORD_PACE_CHUNK + 1_024;
        let big_ring: Vec<Coord> = (0..verts).map(|i| Coord::new(i as f64, -(i as f64))).collect();
        let geom = Geometry::Polygon(vec![vec![big_ring]]);

        // Prime Pace so chunk 1 stays below PACE_WORK and chunk 2 triggers Cancel mid-scan.
        let cancel_mid = Control::cancelled_at_look(1);
        let err = crate::run_under("finite_extent_paced", &cancel_mid, || {
            let mut pace = Pace::default();
            assert!(!pace.stop(PACE_WORK - PART_PACE_WORK - verts));
            finite_extent_paced(&geom, &mut pace)
        })
        .unwrap_err();
        assert!(err.contains("cancelled"), "{err}");
        assert_eq!(cancel_mid.looked(), 1);

        let counting = Control::cancelled_at_look(u64::MAX);
        let ext = crate::run_under("finite_extent_paced", &counting, || {
            let mut pace = Pace::default();
            assert!(!pace.stop(PACE_WORK - PART_PACE_WORK - verts));
            finite_extent_paced(&geom, &mut pace)
        })
        .unwrap();
        assert_eq!(counting.looked(), 1);
        assert_eq!(ext, geom.extent());

        let bad = Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(f64::NAN, 1.0)]]);
        let mut pace = Pace::default();
        assert!(finite_extent_paced(&bad, &mut pace).unwrap_err().contains("finite"));
    }

    #[test]
    fn extend_part_paced_cancels_mid_part_forward_and_reversed() {
        use crate::control::{Control, PACE_WORK, Pace};

        let verts = COORD_PACE_CHUNK + 2_048;
        let part: Vec<Coord> = (0..verts).map(|i| Coord::new(i as f64, 0.0)).collect();

        for reverse in [false, true] {
            let cancel_mid = Control::cancelled_at_look(1);
            let err = crate::run_under("extend_part_paced", &cancel_mid, || {
                let mut pace = Pace::default();
                let copied = verts - 1;
                assert!(!pace.stop(PACE_WORK - copied));
                let mut line = vec![Coord::new(-1.0, 0.0)];
                extend_part_paced(&mut line, &part, reverse, true, &mut pace)
            })
            .unwrap_err();
            assert!(err.contains("cancelled"), "reverse={reverse}: {err}");
            assert_eq!(cancel_mid.looked(), 1);

            let counting = Control::cancelled_at_look(u64::MAX);
            let out = crate::run_under("extend_part_paced", &counting, || {
                let mut pace = Pace::default();
                let copied = verts - 1;
                assert!(!pace.stop(PACE_WORK - copied));
                let mut line = vec![Coord::new(-1.0, 0.0)];
                extend_part_paced(&mut line, &part, reverse, true, &mut pace)?;
                Ok::<_, String>(line)
            })
            .unwrap();
            assert_eq!(counting.looked(), 1);
            assert_eq!(out.len(), verts);
            assert_eq!(out[0], Coord::new(-1.0, 0.0));
            if reverse {
                assert_eq!(out[1], Coord::new((verts - 2) as f64, 0.0));
                assert_eq!(out[verts - 1], Coord::new(0.0, 0.0));
            } else {
                assert_eq!(out[1], Coord::new(1.0, 0.0));
                assert_eq!(out[verts - 1], Coord::new((verts - 1) as f64, 0.0));
            }
        }
    }

    #[test]
    fn split_checks_cancel_and_reports_progress() {
        use crate::control::Control;

        // 4 polyline features, 3 vertices each (2 segments each => n = 8).
        let mut ds = Dataset::new("input", GeometryType::Polyline, vec![Field::new("id", FieldType::Integer)]);
        for i in 0..4 {
            let x = i as f64 * 10.0;
            ds.push_next(
                Some(Geometry::Line(vec![vec![Coord::new(x, 0.0), Coord::new(x + 1.0, 0.0), Coord::new(x + 2.0, 0.0)]])),
                vec![Value::Int(i)],
            );
        }
        let inp = One(ds);
        let params = serde_json::json!({"input": "input"});

        let counting = Control::cancelled_at_look(u64::MAX);
        crate::run_with("splitLineAtVertices", &inp, &params, &counting).unwrap();
        assert_eq!(counting.progress(), Some((8, 8)));
        assert_eq!(counting.looked(), 9);

        let cancel_sizing = Control::cancelled_at_look(2);
        let err = crate::run_with("splitLineAtVertices", &inp, &params, &cancel_sizing).unwrap_err();
        assert!(err.contains("cancelled"), "{err}");
        assert_eq!(cancel_sizing.looked(), 2);
        assert_eq!(cancel_sizing.progress(), None);

        let cancel_reserve = Control::cancelled_at_look(5);
        let err = crate::run_with("splitLineAtVertices", &inp, &params, &cancel_reserve).unwrap_err();
        assert!(err.contains("cancelled"), "{err}");
        assert_eq!(cancel_reserve.looked(), 5);
        assert_eq!(cancel_reserve.progress(), Some((0, 8)));

        let cancel_emit = Control::cancelled_at_look(7);
        let err = crate::run_with("splitLineAtVertices", &inp, &params, &cancel_emit).unwrap_err();
        assert!(err.contains("cancelled"), "{err}");
        assert_eq!(cancel_emit.looked(), 7);
        assert_eq!(cancel_emit.progress(), Some((2, 8)));

        // Single long line with 2,500 vertices (2,499 segments) cancels mid-line via Pace.
        let mut long_ds = Dataset::new("input", GeometryType::Polyline, vec![]);
        let coords: Vec<Coord> = (0..2500).map(|i| Coord::new(i as f64, 0.0)).collect();
        long_ds.push_next(Some(Geometry::Line(vec![coords])), vec![]);
        let long_inp = One(long_ds);

        let counting_long = Control::cancelled_at_look(u64::MAX);
        crate::run_with("splitLineAtVertices", &long_inp, &params, &counting_long).unwrap();
        assert_eq!(counting_long.progress(), Some((2499, 2499)));
        assert_eq!(counting_long.looked(), 5);

        let cancel_long = Control::cancelled_at_look(4);
        let err = crate::run_with("splitLineAtVertices", &long_inp, &params, &cancel_long).unwrap_err();
        assert!(err.contains("cancelled"), "{err}");
        assert_eq!(cancel_long.looked(), 4);
        assert_eq!(cancel_long.progress(), Some((1023, 2499)));
    }

    #[test]
    fn unsplit_checks_cancel_and_reports_progress() {
        use crate::control::Control;

        // 4 connected 2-vertex line features forming a 4-edge chain with identical attribute.
        let mut ds = Dataset::new("input", GeometryType::Polyline, vec![Field::new("road", FieldType::Text)]);
        for i in 0..4 {
            let x = i as f64;
            ds.push_next(Some(Geometry::Line(vec![vec![Coord::new(x, 0.0), Coord::new(x + 1.0, 0.0)]])), vec![Value::Text("main".into())]);
        }
        let inp = One(ds);
        let params = serde_json::json!({"input": "input"});

        // Shared monotonic progress across 4 input rows + 4 collected edges = 8.
        let counting = Control::cancelled_at_look(u64::MAX);
        let res = crate::run_with("unsplitLine", &inp, &params, &counting).unwrap();
        let Output::Layer(out) = res.output else { panic!("expected layer") };
        assert_eq!(out.features.len(), 1);
        assert_eq!(counting.progress(), Some((8, 8)));
        assert_eq!(counting.looked(), 9);

        let cancel_input = Control::cancelled_at_look(3);
        let err = crate::run_with("unsplitLine", &inp, &params, &cancel_input).unwrap_err();
        assert!(err.contains("cancelled"), "{err}");
        assert_eq!(cancel_input.looked(), 3);
        assert_eq!(cancel_input.progress(), Some((1, 8)));

        let cancel_chain = Control::cancelled_at_look(7);
        let err = crate::run_with("unsplitLine", &inp, &params, &cancel_chain).unwrap_err();
        assert!(err.contains("cancelled"), "{err}");
        assert_eq!(cancel_chain.looked(), 7);
        assert_eq!(cancel_chain.progress(), Some((5, 8)));
    }
}
