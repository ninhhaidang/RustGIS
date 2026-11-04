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
    for row in rows(inp, p, "input", ds) {
        let Some(f) = ds.features.get(row) else { continue };
        require_schema(f, ds.fields.len())?;
        let Some(g) = &f.geometry else {
            skipped += 1;
            continue;
        };
        require_finite(g)?;
        match (mode, g) {
            (Conversion::PolygonToLine, Geometry::Polygon(polys)) => {
                let mut parts = Vec::new();
                if polys.is_empty() {
                    skipped += 1;
                }
                for rings in polys {
                    if rings.is_empty() {
                        skipped += 1;
                    }
                    for ring in rings {
                        if ring.len() < 2 {
                            skipped += 1;
                            continue;
                        }
                        let mut line = ring.clone();
                        if let Some(first) = line.first().copied()
                            && line.last() != Some(&first)
                        {
                            line.push(first);
                        }
                        parts.push(line);
                    }
                }
                if !parts.is_empty() {
                    out.push_next(Some(Geometry::Line(parts)), f.attrs.clone());
                }
            }
            (Conversion::PolygonToLine, _) => {
                return Err(format!("{tool}: feature {} has an incompatible geometry", f.id));
            }
            _ => {
                let e = g.extent();
                if e.is_empty() || e.width() == 0.0 || e.height() == 0.0 {
                    skipped += 1;
                    continue;
                }
                let a = Coord::new(e.min_x, e.min_y);
                let ring = vec![a, Coord::new(e.max_x, e.min_y), Coord::new(e.max_x, e.max_y), Coord::new(e.min_x, e.max_y), a];
                out.push_next(Some(Geometry::Polygon(vec![vec![ring]])), f.attrs.clone());
            }
        }
    }
    finish(out, skipped, tool)
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
    for f in features {
        require_schema(f, width)?;
        let Some(g) = &f.geometry else {
            skipped += 1;
            continue;
        };
        require_finite(g)?;
        let Geometry::Line(parts) = g else { return Err(format!("Split Line At Vertices: feature {} is not a line", f.id)) };
        if parts.is_empty() {
            skipped += 1;
        }
        let per_segment =
            if estimate { point_bytes(f.attrs.get(..width).unwrap_or(&f.attrs), width).saturating_add(SEGMENT_EXTRA_BYTES) } else { 0 };
        for part in parts {
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
    let mut segments = Vec::new();
    segments.try_reserve_exact(n).map_err(no_memory)?;
    for f in features() {
        let Some(Geometry::Line(parts)) = &f.geometry else { continue };
        for pair in parts.iter().flat_map(|part| part.windows(2)) {
            let id =
                u64::try_from(segments.len()).ok().and_then(|n| n.checked_add(1)).ok_or("Split Line At Vertices: too many segments")?;
            match segment_feature(id, pair, &f.attrs, ds.fields.len()) {
                Ok(feature) => segments.push(feature),
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

/// Rejects invalid coordinates before extent calculation or endpoint hashing can hide them.
fn require_finite(g: &Geometry) -> Result<(), String> {
    let mut finite = true;
    g.for_each_coord(|c| finite &= c.is_finite());
    if finite { Ok(()) } else { Err("Geometry conversion requires finite coordinates".into()) }
}

/// Returns counted loss notices, refusing a result in which every usable geometry was lost.
fn finish(out: Dataset, skipped: usize, tool: &str) -> Result<ToolResult, String> {
    if skipped > 0 && out.features.is_empty() {
        return Err(format!("{tool}: no usable output; skipped {skipped} empty or degenerate features/parts"));
    }
    let messages = if skipped == 0 { vec![] } else { vec![format!("{tool}: skipped {skipped} empty or degenerate features/parts")] };
    Ok(ToolResult { output: Output::Layer(out), messages })
}

/// Hashes exact coordinates, normalizing signed zero to match coordinate equality.
fn endpoint(c: &Coord) -> (u64, u64) {
    let bits = |x: f64| if x == 0.0 { 0 } else { x.to_bits() };
    (bits(c.x), bits(c.y))
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
    for row in rows(inp, p, "input", ds) {
        let Some(f) = ds.features.get(row) else { continue };
        require_schema(f, ds.fields.len())?;
        let Some(g) = &f.geometry else {
            skipped += 1;
            continue;
        };
        require_finite(g)?;
        let Geometry::Line(parts) = g else {
            return Err(format!("Unsplit Line: feature {} is not a line", f.id));
        };
        if parts.is_empty() {
            skipped += 1;
        }
        for part in parts {
            let (Some(first), Some(last)) = (part.first(), part.last()) else {
                skipped += 1;
                continue;
            };
            if part.len() < 2 {
                skipped += 1;
                continue;
            }
            let id = edges.len();
            nodes.entry(endpoint(first)).or_default().push((id, false));
            nodes.entry(endpoint(last)).or_default().push((id, true));
            edges.push((part, &f.attrs));
        }
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
                *seen = true;
                let skip = usize::from(!line.is_empty());
                if reverse {
                    line.extend(part.iter().rev().skip(skip).copied());
                } else {
                    line.extend(part.iter().skip(skip).copied());
                }
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
}
