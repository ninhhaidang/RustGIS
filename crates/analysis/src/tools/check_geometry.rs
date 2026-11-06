//! Check Geometry: a new layer of the features whose geometry breaks the OGC Simple Features rules,
//! each with what is wrong ([`crate::validity`]), in stored coordinates.
use super::*;
use crate::validity::{self, Checked, Code, Limit, Problem};

const TOOL: &str = "Check Geometry";
/// The most problems one feature's `detail` describes; the rest are counted.
const MAX_DETAILS: usize = 20;

/// Writes the checked features (the selected ones, or all) that have problems, as stored, with
/// their attributes and two text fields: the problem codes and their description. A geometry
/// with non-finite coordinates is written without geometry, so it can't spoil the layer's extent.
pub(super) fn run(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    run_with_room(inp, p, web_has_room, |g| validity::check(g, validity::MAX_WORK))
}

/// A checked feature with problems, before it is copied to the output.
struct Found {
    row: usize,
    /// Its `problem` and `detail` values.
    problem: String,
    detail: String,
    /// Written without geometry.
    non_finite: bool,
}

/// What the checks found, by feature, for the notices.
#[derive(Default)]
struct Tally {
    checked: usize,
    found: usize,
    /// Features with each code, in `Code::ALL` order.
    counts: [usize; Code::ALL.len()],
    /// Features not checked fully because they needed more than the work limit.
    work_limit: usize,
    /// Features not checked fully because the memory for their checks couldn't be had.
    memory: usize,
    /// Features not checked fully because geo's relate failed on some of their rings or parts.
    relate_failed: usize,
    /// Features whose rings or parts weren't all checked against each other.
    unrelated: usize,
    /// Features with more attribute values than the layer has fields.
    extra: usize,
}

/// [`run`], asking `has_room` whether the browser can give the output's memory (see
/// [`web_has_room`]) before any feature is copied, and checking each geometry with `check`.
fn run_with_room(
    inp: &dyn Inputs,
    p: &Json,
    has_room: impl FnOnce(u64) -> bool,
    check: impl Fn(Option<&Geometry>) -> Checked,
) -> Result<ToolResult, String> {
    let ds = input(inp, p, "input")?;
    if ds.geometry_type == GeometryType::None {
        return Err(format!("{TOOL} needs a layer with geometry: '{}' is a table", ds.name));
    }
    let mut rows = rows(inp, p, "input", ds);
    rows.sort_unstable();
    rows.dedup();
    let width = ds.fields.len();
    let mut out = copy_schema(ds, out_name(p, format!("{}_check", ds.name)), ds.geometry_type);
    out.crs = ds.crs;
    out.crs_incomplete = ds.crs_incomplete;
    out.zm_dropped = ds.zm_dropped;
    let mut notes = Vec::new();
    let mut problem_field = String::new();
    for base in ["problem", "detail"] {
        let name = unique_field(&out, base);
        if name != base {
            notes.push(format!("{TOOL}: '{}' already has a field named {base}, so the output's {base} field is {name}", ds.name));
        }
        if base == "problem" {
            problem_field.clone_from(&name);
        }
        out.fields.push(Field::new(&name, FieldType::Text));
    }

    // The checks first, keeping only what the output needs of the features with problems, in
    // memory allocated fallibly as the copies below are.
    let mut tally = Tally { checked: rows.len(), ..Tally::default() };
    let mut found: Vec<Found> = Vec::new();
    crate::control::begin(rows.len());
    for (i, row) in rows.iter().enumerate() {
        crate::control::check()?;
        crate::control::reached(i);
        let Some(f) = ds.features.get(*row) else { continue };
        let checked = check(f.geometry.as_ref());
        tally.unrelated += usize::from(checked.unrelated);
        let codes = checked.codes();
        if codes.is_empty() {
            continue;
        }
        tally.found += 1;
        for code in &codes {
            if let Some(n) = Code::ALL.iter().position(|c| c == code).and_then(|k| tally.counts.get_mut(k)) {
                *n += 1;
            }
        }
        let limit = |limit: Limit| checked.problems.contains(&Problem::TooComplex { limit });
        tally.work_limit += usize::from(limit(Limit::Work));
        tally.memory += usize::from(limit(Limit::Memory));
        tally.relate_failed += usize::from(limit(Limit::Relate));
        tally.extra += usize::from(f.attrs.len() > width);
        let no_memory =
            |e: TryReserveError| format!("{TOOL}: not enough memory for the {} with problems found so far: {e}", features(tally.found));
        let names: Vec<&str> = codes.iter().map(|c| c.as_str()).collect();
        let problem = joined(&names, ", ").map_err(no_memory)?;
        let detail = details(&checked.problems).map_err(no_memory)?;
        found.try_reserve(1).map_err(no_memory)?;
        found.push(Found { row: *row, problem, detail, non_finite: codes.contains(&Code::NonFiniteCoordinate) });
    }

    // Then the copies, once the memory for them is known to be there: in the browser, whose memory
    // has a ceiling, it is asked for before anything is copied, and every copy is allocated
    // fallibly, so running out of memory stops the tool with a message.
    let n = found.len();
    if n > 0 {
        let need = found
            .iter()
            .filter_map(|item| {
                let f = ds.features.get(item.row)?;
                let geometry = f.geometry.as_ref().filter(|_| !item.non_finite);
                Some(row_bytes(&f.attrs, width, out.fields.len(), &[&item.problem, &item.detail], geometry))
            })
            .fold(0u64, u64::saturating_add);
        if !has_room(need) {
            return Err(format!(
                "{TOOL}: the {} with problems would need about {} MiB, more than the browser can give now: select fewer features",
                features(n),
                need.div_ceil(1 << 20)
            ));
        }
    }
    let no_memory = |e: TryReserveError| format!("{TOOL}: not enough memory for the {} with problems: {e}", features(n));
    let mut list: Vec<Feature> = Vec::new();
    list.try_reserve_exact(n).map_err(no_memory)?;
    for item in found {
        // The copies can take a while for many large features: a cancel still stops them.
        crate::control::check()?;
        let Some(f) = ds.features.get(item.row) else { continue };
        let mut attrs = Vec::new();
        attrs.try_reserve_exact(width.saturating_add(2)).map_err(no_memory)?;
        for v in f.attrs.iter().take(width) {
            attrs.push(try_clone_value(v).map_err(no_memory)?);
        }
        attrs.resize(width, Value::Null);
        attrs.push(Value::Text(item.problem));
        attrs.push(Value::Text(item.detail));
        let geometry =
            if item.non_finite { None } else { f.geometry.as_ref().map(crate::merge::clone_geometry).transpose().map_err(no_memory)? };
        // The input's id, so a row leads back to its feature.
        list.push(Feature { id: f.id, geometry, attrs });
    }
    out.features = list.into();

    let mut messages = notices(&tally, &problem_field);
    messages.extend(notes);
    Ok(ToolResult { output: Output::Layer(out), messages })
}

/// The memory an output row copying `attrs` (`width` of them, of `out_width` values in the
/// output) takes, with the text fields `texts` added and `geometry`: what [`point_bytes`] counts for
/// a row, the texts, and the geometry, held twice (the layer's copy and the map's projected one for
/// drawing), each allocation with its overhead.
pub(super) fn row_bytes(attrs: &[Value], width: usize, out_width: usize, texts: &[&str], geometry: Option<&Geometry>) -> u64 {
    let u = |x: usize| u64::try_from(x).unwrap_or(u64::MAX);
    let own = attrs.get(..width).unwrap_or(attrs);
    let text = texts.iter().fold(0u64, |n, t| n.saturating_add(u(t.len())).saturating_add(ALLOCATION_OVERHEAD));
    let geometry = match geometry {
        None => 0,
        Some(g) => {
            let lists = match g {
                Geometry::Point(_) => 0,
                Geometry::MultiPoint(_) => 1,
                Geometry::Line(parts) => parts.len().saturating_add(1),
                Geometry::Polygon(polys) => polys.iter().map(Vec::len).fold(polys.len().saturating_add(1), usize::saturating_add),
            };
            u(rustgis_data::source::chunk_memory::geometry_bytes(g)).saturating_add(u(lists).saturating_mul(ALLOCATION_OVERHEAD))
        }
    };
    point_bytes(own, out_width).saturating_add(text).saturating_add(geometry.saturating_mul(2))
}

/// The run's notices: the result in one line, then one notice per kind of feature not checked
/// fully or not copied whole. `problem` is the output's problem field.
fn notices(t: &Tally, problem: &str) -> Vec<String> {
    let mut messages = vec![summary(t.found, t.checked, &t.counts)];
    let they = |n: usize| if n == 1 { "it is" } else { "they are" };
    let non_finite = Code::ALL.iter().position(|c| *c == Code::NonFiniteCoordinate).and_then(|k| t.counts.get(k)).copied().unwrap_or(0);
    if non_finite > 0 {
        messages.push(format!(
            "{TOOL}: {} with non-finite coordinates {} written without geometry",
            features(non_finite),
            if non_finite == 1 { "is" } else { "are" }
        ));
    }
    if t.work_limit > 0 {
        messages.push(format!(
            "{TOOL}: {} could not be checked fully (too complex for the work limit); {} in the output as too_complex",
            features(t.work_limit),
            they(t.work_limit)
        ));
    }
    if t.memory > 0 {
        messages.push(format!(
            "{TOOL}: {} could not be checked fully, because there wasn't enough memory to check {}; {} in the output as too_complex",
            features(t.memory),
            if t.memory == 1 { "it" } else { "them" },
            they(t.memory)
        ));
    }
    if t.relate_failed > 0 {
        messages.push(format!(
            "{TOOL}: some rings or parts of {} could not be checked against each other, because geo's relate operation failed on them; {} in the output as too_complex",
            features(t.relate_failed),
            they(t.relate_failed)
        ));
    }
    if t.unrelated > 0 {
        messages.push(format!(
            "{TOOL}: the rings or parts of {} weren't all checked against each other, because some of their rings or parts already have problems of their own (see {problem})",
            features(t.unrelated)
        ));
    }
    if t.extra > 0 {
        messages.push(format!(
            "{TOOL}: {} had more attribute values than the layer has fields; the extra values aren't in the output",
            features(t.extra)
        ));
    }
    messages
}

/// "1 feature", "2 features".
pub(super) fn features(n: usize) -> String {
    format!("{n} feature{}", if n == 1 { "" } else { "s" })
}

/// The run's result in one line, with the features having each code.
fn summary(found: usize, checked: usize, counts: &[usize]) -> String {
    if checked == 0 {
        return format!("{TOOL}: no features to check");
    }
    if found == 0 {
        return format!("{TOOL}: no problems found in {}", features(checked));
    }
    let by_code: Vec<String> = Code::ALL.iter().zip(counts).filter(|(_, n)| **n > 0).map(|(c, n)| format!("{}: {n}", c.as_str())).collect();
    format!("{TOOL}: {found} of {} {} problems ({})", features(checked), if found == 1 { "has" } else { "have" }, by_code.join(", "))
}

/// The descriptions of a feature's problems, joined with "; ": the first [`MAX_DETAILS`], then how
/// many more there are.
fn details(problems: &[validity::Problem]) -> Result<String, TryReserveError> {
    let mut parts: Vec<String> = problems.iter().take(MAX_DETAILS).map(validity::Problem::describe).collect();
    let more = problems.len().saturating_sub(MAX_DETAILS);
    if more > 0 {
        parts.push(format!("and {more} more problem{}", if more == 1 { "" } else { "s" }));
    }
    joined(&parts, "; ")
}

/// `parts` joined with `sep`, in a string allocated fallibly at its exact length: it is kept until
/// the output is copied.
pub(super) fn joined<S: AsRef<str>>(parts: &[S], sep: &str) -> Result<String, TryReserveError> {
    let seps = sep.len().saturating_mul(parts.len().saturating_sub(1));
    let len = parts.iter().map(|p| p.as_ref().len()).fold(seps, usize::saturating_add);
    let mut s = String::new();
    s.try_reserve_exact(len)?;
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            s.push_str(sep);
        }
        s.push_str(part.as_ref());
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(g: Option<&Geometry>) -> Checked {
        validity::check(g, validity::MAX_WORK)
    }

    /// The inputs: one layer, with nothing selected.
    struct One(Dataset);
    impl Inputs for One {
        fn dataset(&self, _: &str) -> Option<&Dataset> {
            Some(&self.0)
        }
        fn selection(&self, _: &str) -> &[usize] {
            &[]
        }
    }

    fn too_complex(work_limit: usize, relate_failed: usize, both: usize) -> Tally {
        let mut counts = [0; Code::ALL.len()];
        if let Some(n) = Code::ALL.iter().position(|c| *c == Code::TooComplex).and_then(|k| counts.get_mut(k)) {
            *n = work_limit + relate_failed - both;
        }
        Tally { checked: 9, found: work_limit + relate_failed - both, counts, work_limit, relate_failed, ..Tally::default() }
    }

    /// Each reason a feature wasn't checked fully has a notice of its own, with its count: geo's
    /// relate failing can't be brought about on purpose, so its notice is checked here.
    #[test]
    fn features_not_checked_fully_are_counted_by_reason() {
        assert_eq!(
            notices(&too_complex(0, 1, 0), "problem"),
            [
                "Check Geometry: 1 of 9 features has problems (too_complex: 1)",
                "Check Geometry: some rings or parts of 1 feature could not be checked against each other, because geo's relate operation failed on them; it is in the output as too_complex",
            ]
        );
        assert_eq!(
            notices(&too_complex(2, 3, 1), "problem"),
            [
                "Check Geometry: 4 of 9 features have problems (too_complex: 4)",
                "Check Geometry: 2 features could not be checked fully (too complex for the work limit); they are in the output as too_complex",
                "Check Geometry: some rings or parts of 3 features could not be checked against each other, because geo's relate operation failed on them; they are in the output as too_complex",
            ]
        );
        let mut memory = too_complex(0, 0, 0);
        for n in [1, 2] {
            memory.memory = n;
            assert_eq!(
                notices(&memory, "problem").get(1).map(String::as_str),
                Some(if n == 1 {
                    "Check Geometry: 1 feature could not be checked fully, because there wasn't enough memory to check it; it is in the output as too_complex"
                } else {
                    "Check Geometry: 2 features could not be checked fully, because there wasn't enough memory to check them; they are in the output as too_complex"
                })
            );
        }
        let t = Tally { checked: 2, found: 2, unrelated: 2, ..Tally::default() };
        assert_eq!(
            notices(&t, "problem_2").last().map(String::as_str),
            Some(
                "Check Geometry: the rings or parts of 2 features weren't all checked against each other, because some of their rings or parts already have problems of their own (see problem_2)"
            )
        );
    }

    /// The output's memory is asked for before any feature is copied, and a browser that can't
    /// give it refuses the run with how much it needed.
    #[test]
    fn the_output_is_refused_when_the_browser_has_no_room_for_it() {
        let mut ds = Dataset::new("input", GeometryType::Polygon, vec![Field::new("name", FieldType::Text)]);
        let bowtie = vec![Coord::new(0., 0.), Coord::new(2., 2.), Coord::new(2., 0.), Coord::new(0., 2.), Coord::new(0., 0.)];
        ds.push_next(Some(Geometry::Polygon(vec![vec![bowtie]])), vec![Value::Text("x".repeat(1 << 21))]);
        ds.push_next(Some(Geometry::Point(Coord::new(1., 1.))), vec![Value::Text("fine".into())]);
        let inp = One(ds);
        let p = serde_json::json!({"input": "input"});
        let mut asked = 0;
        let r = run_with_room(
            &inp,
            &p,
            |need| {
                asked = need;
                true
            },
            check,
        )
        .unwrap();
        let Output::Layer(out) = &r.output else { panic!("a layer") };
        assert_eq!(out.features.len(), 1);
        // The row with its copied text, the codes and the detail, and the geometry twice: its five
        // coordinates and three lists, each with an allocation's overhead.
        let row = point_bytes(&inp.0.features[0].attrs, 3);
        let added = ("self_intersection".len() + "exterior ring of part 1 crosses itself".len()) as u64 + 2 * ALLOCATION_OVERHEAD;
        let geometry = rustgis_data::source::chunk_memory::geometry_bytes(inp.0.features[0].geometry.as_ref().unwrap()) as u64
            + 3 * ALLOCATION_OVERHEAD;
        assert!(row > 1 << 21);
        assert_eq!(asked, row + added + 2 * geometry);
        let e = run_with_room(&inp, &p, |_| false, check).err().unwrap();
        assert_eq!(
            e,
            "Check Geometry: the 1 feature with problems would need about 3 MiB, more than the browser can give now: select fewer features"
        );
    }

    /// A feature whose checks couldn't have the memory they need is in the output as too_complex,
    /// with that reason and its own notice, and with what the checks that copy nothing found.
    #[test]
    fn features_without_memory_for_their_checks_are_too_complex_never_valid() {
        let ring = |pts: &[(f64, f64)]| pts.iter().map(|&(x, y)| Coord::new(x, y)).collect::<Vec<_>>();
        let mut ds = Dataset::new("input", GeometryType::Polygon, vec![]);
        let square = ring(&[(0., 0.), (1., 0.), (1., 1.), (0., 1.), (0., 0.)]);
        let open = ring(&[(0., 0.), (1., 0.), (1., 0.), (1., 1.)]);
        ds.push_next(Some(Geometry::Polygon(vec![vec![square.clone()]])), vec![]);
        ds.push_next(Some(Geometry::Polygon(vec![vec![open]])), vec![]);
        ds.push_next(Some(Geometry::Point(Coord::new(1., 1.))), vec![]);
        let inp = One(ds);
        let p = serde_json::json!({"input": "input"});
        let refused = |g: Option<&Geometry>| validity::check_with(g, validity::MAX_WORK, &|_| false);
        let r = run_with_room(&inp, &p, |_| true, refused).unwrap();
        let Output::Layer(out) = &r.output else { panic!("a layer") };
        let rows: Vec<(String, String)> = out.features.iter().map(|f| (f.attrs[0].to_string(), f.attrs[1].to_string())).collect();
        assert_eq!(
            rows,
            [
                ("too_complex".to_string(), "not checked fully: not enough memory to check it".to_string()),
                (
                    "ring_not_closed, repeated_vertex, too_complex".to_string(),
                    "exterior ring of part 1 is not closed; 1 repeated vertex in exterior ring of part 1; not checked fully: not enough memory to check it".to_string()
                ),
            ]
        );
        assert_eq!(
            r.messages,
            [
                "Check Geometry: 2 of 3 features have problems (ring_not_closed: 1, repeated_vertex: 1, too_complex: 2)",
                "Check Geometry: 2 features could not be checked fully, because there wasn't enough memory to check them; they are in the output as too_complex",
            ]
        );
        // With the memory, the square is fine.
        let r = run_with_room(&inp, &p, |_| true, check).unwrap();
        let Output::Layer(out) = &r.output else { panic!("a layer") };
        assert_eq!(out.features.len(), 1);
    }

    /// The strings kept for the output are joined at their exact length.
    #[test]
    fn joined_strings_are_reserved_at_their_length() {
        let s = joined(&["self_intersection", "too_complex"], ", ").unwrap();
        assert_eq!(s, "self_intersection, too_complex");
        assert_eq!(s.capacity(), s.len());
        assert_eq!(joined::<&str>(&[], "; ").unwrap(), "");
        assert_eq!(joined(&["one".to_string()], "; ").unwrap(), "one");
    }
}
