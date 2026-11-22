//! Repair Geometry: a new layer of the features (the selected ones, or all) with their geometry
//! made valid after the OGC Simple Features rules ([`crate::repair`]), each saying what was
//! repaired.
use super::check_geometry::{features, joined, row_bytes};
use super::*;
use crate::repair::{self, Gone, Kept, Outcome, Rule};
use crate::validity::Code;

const TOOL: &str = "Repair Geometry";

/// Writes every processed feature with its id, attributes and a text field of what was repaired:
/// valid ones as stored, repaired ones with their new geometry, those that can't be repaired
/// without geometry, and those left as they were (too complex, or the repair failed) as stored.
pub(super) fn run(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    run_with_room(inp, p, &web_has_room, |g, rule| repair::repair_by(g, rule, &web_has_room))
}

/// The rule `p` names (#2003), Shells Minus Holes when it names none.
fn rule_of(p: &Json) -> Result<Rule, String> {
    match p.get("rule") {
        None | Some(Json::Null) => Ok(Rule::default()),
        Some(v) => v
            .as_str()
            .and_then(Rule::named)
            .ok_or_else(|| format!("{TOOL}: rule is one of {}", repair::RULES.map(|r| format!("\"{r}\"")).join(", "))),
    }
}

/// What a feature rebuilt under the Even-Odd rule has in its repairs field, after the codes: data,
/// never translated.
const EVEN_ODD: &str = "even_odd";

/// A feature's geometry in the output.
enum Written {
    /// As stored.
    Same,
    /// None: it can't be repaired.
    Without,
    Repaired(Geometry),
}

/// A processed feature, before it is copied to the output.
struct Item {
    row: usize,
    /// Its `repairs` value.
    repairs: String,
    geometry: Written,
}

/// What the repairs did, by feature, for the notices.
#[derive(Default)]
struct Tally {
    processed: usize,
    repaired: usize,
    /// Repaired features with each code, in `Code::ALL` order.
    counts: [usize; Code::ALL.len()],
    /// Features written without geometry, by reason, in `Gone::ALL` order.
    gone: [usize; Gone::ALL.len()],
    /// Features left as they were, because they needed more than the work bound, because the
    /// memory for their repair couldn't be had, and because geo failed on them.
    work: usize,
    memory: usize,
    failed: usize,
    /// Rings and parts dropped (too few points, no area), and from how many features.
    dropped: usize,
    dropped_features: usize,
    /// Holes and parts dropped as narrower than a step of the overlay's grid in any direction, and
    /// from how many features (each with `no_area` in its repairs).
    narrow: usize,
    narrow_features: usize,
    /// Features rebuilt by the overlay, and how far their coordinates may have moved at most.
    overlaid: usize,
    moved: f64,
    /// Features with more attribute values than the layer has fields.
    extra: usize,
    /// Under the Even-Odd rule: features it rebuilt, and duplicate rings left out first, from how
    /// many features.
    even_odd: usize,
    duplicates: usize,
    duplicate_features: usize,
}

impl Tally {
    fn count(&self, code: Code) -> usize {
        Code::ALL.iter().position(|c| *c == code).and_then(|k| self.counts.get(k)).copied().unwrap_or(0)
    }
}

/// [`run`], asking `has_room` whether the browser can give the output's memory (see
/// [`web_has_room`]) before any feature is copied, and repairing each geometry with `repair`.
fn run_with_room(
    inp: &dyn Inputs,
    p: &Json,
    has_room: &dyn Fn(u64) -> bool,
    repair: impl Fn(Option<&Geometry>, Rule) -> Outcome,
) -> Result<ToolResult, String> {
    let rule = rule_of(p)?;
    let ds = input(inp, p, "input")?;
    if ds.geometry_type == GeometryType::None {
        return Err(format!("{TOOL} needs a layer with geometry: '{}' is a table", ds.name));
    }
    let mut rows = rows(inp, p, "input", ds);
    rows.sort_unstable();
    rows.dedup();
    let width = ds.fields.len();
    let mut out = copy_schema(ds, out_name(p, format!("{}_repaired", ds.name)), ds.geometry_type);
    out.crs = ds.crs;
    out.crs_incomplete = ds.crs_incomplete;
    out.zm_dropped = ds.zm_dropped;
    let mut notes = Vec::new();
    let field = unique_field(&out, "repairs");
    if field != "repairs" {
        notes.push(format!("{TOOL}: '{}' already has a field named repairs, so the output's repairs field is {field}", ds.name));
    }
    out.fields.push(Field::new(&field, FieldType::Text));

    // The repairs first, keeping what the output needs of each feature in memory allocated
    // fallibly, as the copies below are.
    let mut tally = Tally { processed: rows.len(), ..Tally::default() };
    let no_memory = |e: TryReserveError| format!("{TOOL}: not enough memory for the repairs of {}: {e}", features(rows.len()));
    let mut items: Vec<Item> = Vec::new();
    items.try_reserve_exact(rows.len()).map_err(no_memory)?;
    crate::control::begin(rows.len());
    for (i, row) in rows.iter().enumerate() {
        crate::control::check()?;
        crate::control::reached(i);
        let Some(f) = ds.features.get(*row) else { continue };
        tally.extra += usize::from(f.attrs.len() > width);
        let (repairs, geometry) = match repair(f.geometry.as_ref(), rule) {
            Outcome::Valid => (String::new(), Written::Same),
            Outcome::Repaired(r) => {
                tally.repaired += 1;
                for code in &r.codes {
                    if let Some(n) = Code::ALL.iter().position(|c| c == code).and_then(|k| tally.counts.get_mut(k)) {
                        *n += 1;
                    }
                }
                tally.dropped += r.dropped;
                tally.dropped_features += usize::from(r.dropped > 0);
                tally.narrow += r.narrow;
                tally.narrow_features += usize::from(r.narrow > 0);
                if let Some(moved) = r.moved {
                    tally.overlaid += 1;
                    tally.moved = tally.moved.max(moved);
                }
                // The Even-Odd rule decides only what the overlay rebuilds: said for those features.
                let even_odd = rule == Rule::EvenOdd && r.moved.is_some();
                tally.even_odd += usize::from(even_odd);
                tally.duplicates += r.duplicates;
                tally.duplicate_features += usize::from(r.duplicates > 0);
                // A hole or part with no area on the overlay's grid is said after the codes, then the
                // rule when it isn't the default.
                let narrow = (r.narrow > 0).then_some(Gone::NoArea.as_str());
                let names: Vec<&str> = r.codes.iter().map(|c| c.as_str()).chain(narrow).chain(even_odd.then_some(EVEN_ODD)).collect();
                (joined(&names, ", ").map_err(no_memory)?, Written::Repaired(r.geometry))
            }
            Outcome::Gone(why) => {
                if let Some(n) = Gone::ALL.iter().position(|g| *g == why).and_then(|k| tally.gone.get_mut(k)) {
                    *n += 1;
                }
                (joined(&[why.as_str()], "").map_err(no_memory)?, Written::Without)
            }
            // No area left once duplicates were left out: counted as both.
            Outcome::GoneAfterDuplicates { duplicates } => {
                let why = Gone::NoArea;
                if let Some(n) = Gone::ALL.iter().position(|g| *g == why).and_then(|k| tally.gone.get_mut(k)) {
                    *n += 1;
                }
                tally.duplicates += duplicates;
                tally.duplicate_features += 1;
                (joined(&[why.as_str()], "").map_err(no_memory)?, Written::Without)
            }
            Outcome::Kept(why) => {
                match why {
                    Kept::Work => tally.work += 1,
                    Kept::Memory => tally.memory += 1,
                    Kept::Failed => tally.failed += 1,
                }
                (joined(&[why.as_str()], "").map_err(no_memory)?, Written::Same)
            }
        };
        // Within the room reserved for one item a row.
        items.push(Item { row: *row, repairs, geometry });
    }

    // Then the copies, once the memory for them is known to be there: in the browser, whose memory
    // has a ceiling, it is asked for before anything is copied, and every copy is allocated
    // fallibly, so running out of memory stops the tool with a message.
    let n = items.len();
    if n > 0 {
        let need = items
            .iter()
            .filter_map(|item| {
                let f = ds.features.get(item.row)?;
                let geometry = match &item.geometry {
                    Written::Same => f.geometry.as_ref(),
                    Written::Without => None,
                    Written::Repaired(g) => Some(g),
                };
                Some(row_bytes(&f.attrs, width, out.fields.len(), &[&item.repairs], geometry))
            })
            .fold(0u64, u64::saturating_add);
        if !has_room(need) {
            return Err(format!(
                "{TOOL}: the {} would need about {} MiB, more than the browser can give now: select fewer features",
                features(n),
                need.div_ceil(1 << 20)
            ));
        }
    }
    let no_memory = |e: TryReserveError| format!("{TOOL}: not enough memory for the {}: {e}", features(n));
    let mut list: Vec<Feature> = Vec::new();
    list.try_reserve_exact(n).map_err(no_memory)?;
    for item in items {
        // The copies can take a while for many large features: a cancel still stops them.
        crate::control::check()?;
        let Some(f) = ds.features.get(item.row) else { continue };
        let mut attrs = Vec::new();
        attrs.try_reserve_exact(width.saturating_add(1)).map_err(no_memory)?;
        for v in f.attrs.iter().take(width) {
            attrs.push(try_clone_value(v).map_err(no_memory)?);
        }
        attrs.resize(width, Value::Null);
        attrs.push(Value::Text(item.repairs));
        let geometry = match item.geometry {
            Written::Same => f.geometry.as_ref().map(crate::merge::clone_geometry).transpose().map_err(no_memory)?,
            Written::Without => None,
            Written::Repaired(g) => Some(g),
        };
        // The input's id, so a row leads back to its feature.
        list.push(Feature { id: f.id, geometry, attrs });
    }
    out.features = list.into();

    // The units its coordinates are in, which the overlay's rounding moves them by.
    let crs = ds.coord_crs();
    let units = if crs.is_geographic() { "degrees of longitude or latitude".to_string() } else { format!("units of {}", crs.label()) };
    let mut messages = notices(&tally, &field, rule, &units);
    messages.extend(notes);
    let losses = (tally.extra > 0)
        .then(|| rustgis_data::loss::LossMessage {
            code: rustgis_data::loss::LossCode::DroppedExtraAttributes,
            text: format!(
                "{TOOL}: {} had more attribute values than the layer has fields; the extra values aren't in the output",
                features(tally.extra)
            ),
        })
        .into_iter()
        .collect();
    Ok(ToolResult { output: Output::Layer(out), messages, losses })
}

/// The run's notices: the result in one line, then one notice per kind of feature not repaired,
/// and per kind of change beyond the codes: pieces dropped (with too few points or no area, or
/// narrower than a step of the overlay's grid in any direction), holes taken out, parts merged or
/// split, coordinates moved. `field` is the output's repairs field.
fn notices(t: &Tally, field: &str, rule: Rule, units: &str) -> Vec<String> {
    let mut messages = vec![summary(t)];
    let is = |n: usize| if n == 1 { "is" } else { "are" };
    let was = |n: usize| if n == 1 { "was" } else { "were" };
    let gone: usize = t.gone.iter().sum();
    if gone > 0 {
        let by: Vec<String> = Gone::ALL.iter().zip(t.gone).filter(|(_, n)| *n > 0).map(|(g, n)| format!("{}: {n}", g.as_str())).collect();
        messages.push(format!(
            "{TOOL}: {} couldn't be repaired and {} written without geometry ({})",
            features(gone),
            is(gone),
            by.join(", ")
        ));
    }
    if t.work > 0 {
        messages.push(format!(
            "{TOOL}: {} {} too complex to repair within the work limit and {} written unchanged, with too_complex in {field}",
            features(t.work),
            was(t.work),
            is(t.work)
        ));
    }
    if t.memory > 0 {
        messages.push(format!(
            "{TOOL}: {} couldn't be repaired, because there wasn't enough memory to repair {}, and {} written unchanged, with too_complex in {field}",
            features(t.memory),
            if t.memory == 1 { "it" } else { "them" },
            is(t.memory)
        ));
    }
    if t.failed > 0 {
        messages.push(format!(
            "{TOOL}: {} couldn't be repaired, because geo's geometry operations failed on {}, and {} written unchanged, with repair_failed in {field}",
            features(t.failed),
            if t.failed == 1 { "it" } else { "them" },
            is(t.failed)
        ));
    }
    if t.dropped > 0 {
        messages.push(format!(
            "{TOOL}: {} {} with too few distinct points or no area {} dropped from {}",
            t.dropped,
            if t.dropped == 1 { "ring or part" } else { "rings or parts" },
            was(t.dropped),
            features(t.dropped_features)
        ));
    }
    if t.narrow > 0 {
        messages.push(format!(
            "{TOOL}: {} {} narrower than a step of the overlay's grid, which would leave {} no area, {} dropped from {}, with {} in {field}",
            t.narrow,
            if t.narrow == 1 { "hole or part" } else { "holes or parts" },
            if t.narrow == 1 { "it" } else { "them" },
            was(t.narrow),
            features(t.narrow_features),
            Gone::NoArea.as_str()
        ));
    }
    // What each rule does with a hole outside its ring and with overlapping parts.
    let holes = t.count(Code::HoleOutsideShell);
    if holes > 0 && rule == Rule::ShellsMinusHoles {
        messages.push(format!(
            "{TOOL}: holes not inside their exterior ring were taken out of {}; what of a hole is inside its exterior ring stays a hole",
            features(holes)
        ));
    }
    if holes > 0 && rule == Rule::EvenOdd {
        messages.push(format!(
            "{TOOL}: holes not inside their exterior ring in {} were combined by the Even-Odd rule: what of a hole lies outside its exterior ring is area, as a part, where no other ring covers it too; what lies inside stays a hole",
            features(holes)
        ));
    }
    let merged = t.count(Code::PartsOverlap);
    if merged > 0 && rule == Rule::ShellsMinusHoles {
        messages.push(format!("{TOOL}: the parts that overlapped or touched along a line were merged in {}", features(merged)));
    }
    if merged > 0 && rule == Rule::EvenOdd {
        messages.push(format!(
            "{TOOL}: the parts that overlapped or touched along a line in {} were combined by the Even-Odd rule, which keeps what an odd number of parts cover and leaves out what an even number cover, keeps a part repeated whole once, and joins parts that meet along a line",
            features(merged)
        ));
    }
    let split = t.count(Code::DisconnectedInterior);
    if split > 0 {
        messages.push(format!(
            "{TOOL}: {} whose interior was cut into pieces by rings touching at points {} written with each piece as a part of its own",
            features(split),
            is(split)
        ));
    }
    if t.overlaid > 0 {
        messages.push(format!(
            "{TOOL}: {} {} rebuilt by geo's boolean overlay, which rounds coordinates to a fine grid: they may have moved by up to {} {units}, and vertices in the middle of straight edges were left out",
            features(t.overlaid),
            was(t.overlaid),
            rounded_up(t.moved)
        ));
    }
    if t.even_odd > 0 {
        messages.push(format!(
            "{TOOL}: {} {} rebuilt by the Even-Odd rule, with {EVEN_ODD} in {field}: each part keeps what lies inside an odd number of its rings, whatever their roles, so where two holes overlap is area again, a ring inside another is a hole, and what of a hole lies outside its exterior ring is a part; then the parts are combined the same way",
            features(t.even_odd),
            was(t.even_odd)
        ));
    }
    if t.duplicates > 0 {
        messages.push(format!(
            "{TOOL}: {} duplicate {} left out of {} before the Even-Odd rule, which would have canceled each pair out",
            t.duplicates,
            if t.duplicates == 1 { "ring was" } else { "rings were" },
            features(t.duplicate_features)
        ));
    }
    messages
}

/// `v`, positive, to two significant digits rounded up, so a bound stays one: "7.5e-8".
fn rounded_up(v: f64) -> String {
    let mut e = v.log10().floor();
    let mut m = (v / 10f64.powf(e) * 10.0 - 1e-9).ceil() / 10.0;
    if m >= 10.0 {
        m /= 10.0;
        e += 1.0;
    }
    format!("{m:.1}e{e}")
}

/// The run's result in one line, with the repaired features having each code.
fn summary(t: &Tally) -> String {
    if t.processed == 0 {
        return format!("{TOOL}: no features to repair");
    }
    let others = t.gone.iter().sum::<usize>() + t.work + t.memory + t.failed;
    if t.repaired == 0 && others == 0 {
        return format!("{TOOL}: no repairs needed in {}", features(t.processed));
    }
    let mut by_code: Vec<String> =
        Code::ALL.iter().zip(t.counts).filter(|(_, n)| *n > 0).map(|(c, n)| format!("{}: {n}", c.as_str())).collect();
    if t.narrow_features > 0 {
        by_code.push(format!("{}: {}", Gone::NoArea.as_str(), t.narrow_features));
    }
    let codes = if by_code.is_empty() { String::new() } else { format!(" ({})", by_code.join(", ")) };
    format!("{TOOL}: repaired {} of {}{codes}", t.repaired, features(t.processed))
}

#[cfg(test)]
mod tests {
    use super::*;
    /// The units a layer in longitude/latitude has its coordinates in.
    const DEGREES: &str = "degrees of longitude or latitude";

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

    fn ring(pts: &[(f64, f64)]) -> Vec<Coord> {
        pts.iter().map(|&(x, y)| Coord::new(x, y)).collect()
    }

    /// The output's memory is asked for before any feature is copied, and a browser that can't
    /// give it refuses the run with how much it needed.
    #[test]
    fn the_output_is_refused_when_the_browser_has_no_room_for_it() {
        let mut ds = Dataset::new("input", GeometryType::Polygon, vec![Field::new("name", FieldType::Text)]);
        let open = ring(&[(0., 0.), (2., 0.), (2., 2.), (0., 2.)]);
        ds.push_next(Some(Geometry::Polygon(vec![vec![open]])), vec![Value::Text("x".repeat(1 << 21))]);
        ds.push_next(None, vec![Value::Text("none".into())]);
        let inp = One(ds);
        let p = serde_json::json!({"input": "input"});
        let asked = std::cell::Cell::new(0);
        let r = run_with_room(
            &inp,
            &p,
            &|need| {
                asked.set(need);
                true
            },
            |g, rule| repair::repair_by(g, rule, &|_| true),
        )
        .unwrap();
        let Output::Layer(out) = &r.output else { panic!("a layer") };
        assert_eq!(out.features.len(), 2);
        // The rows with their copied text and codes, and the repaired geometry twice: its five
        // coordinates and two lists, each with an allocation's overhead. The row without geometry
        // has none.
        let closed = Geometry::Polygon(vec![vec![ring(&[(0., 0.), (2., 0.), (2., 2.), (0., 2.), (0., 0.)])]]);
        let geometry = rustgis_data::source::chunk_memory::geometry_bytes(&closed) as u64 + 3 * ALLOCATION_OVERHEAD;
        let first = point_bytes(&inp.0.features[0].attrs, 2) + "ring_not_closed".len() as u64 + ALLOCATION_OVERHEAD + 2 * geometry;
        let second = point_bytes(&inp.0.features[1].attrs, 2) + "null_geometry".len() as u64 + ALLOCATION_OVERHEAD;
        assert_eq!(asked.get(), first + second);
        let e = run_with_room(&inp, &p, &|_| false, |g, rule| repair::repair_by(g, rule, &|_| true)).err().unwrap();
        assert_eq!(e, "Repair Geometry: the 2 features would need about 3 MiB, more than the browser can give now: select fewer features");
    }

    /// Each reason a feature is left as it was has a notice of its own, with its count: geo
    /// failing natively can't be brought about on purpose, so its notice is checked here.
    #[test]
    fn features_left_as_they_were_are_counted_by_reason() {
        let t = Tally { processed: 9, failed: 1, ..Tally::default() };
        assert_eq!(
            notices(&t, "repairs", Rule::default(), DEGREES),
            [
                "Repair Geometry: repaired 0 of 9 features",
                "Repair Geometry: 1 feature couldn't be repaired, because geo's geometry operations failed on it, and is written unchanged, with repair_failed in repairs",
            ]
        );
        let t = Tally { processed: 9, work: 2, memory: 3, failed: 2, ..Tally::default() };
        assert_eq!(
            notices(&t, "repairs_2", Rule::default(), DEGREES)[1..],
            [
                "Repair Geometry: 2 features were too complex to repair within the work limit and are written unchanged, with too_complex in repairs_2",
                "Repair Geometry: 3 features couldn't be repaired, because there wasn't enough memory to repair them, and are written unchanged, with too_complex in repairs_2",
                "Repair Geometry: 2 features couldn't be repaired, because geo's geometry operations failed on them, and are written unchanged, with repair_failed in repairs_2",
            ]
        );
        assert_eq!(notices(&Tally::default(), "repairs", Rule::default(), DEGREES), ["Repair Geometry: no features to repair"]);
        // A layer held in a projected system is told its bound in that system's units.
        let rebuilt = Tally { overlaid: 1, moved: 0.01, ..Tally::default() };
        let said = notices(&rebuilt, "repairs", Rule::default(), "units of EPSG:32632").join("\n");
        assert!(said.contains("may have moved by up to 1.0e-2 units of EPSG:32632, and vertices"), "{said}");
    }

    /// A cancel stops the repairs at the next feature, and the copies at the next row.
    #[test]
    fn a_cancel_stops_the_repairs_and_the_copies() {
        let mut ds = Dataset::new("input", GeometryType::Polygon, vec![]);
        for _ in 0..3 {
            ds.push_next(Some(Geometry::Polygon(vec![vec![ring(&[(0., 0.), (2., 0.), (2., 2.), (0., 2.)])]])), vec![]);
        }
        let inp = One(ds);
        let p = serde_json::json!({"input": "input"});
        let control = crate::control::Control::new();
        let _installed = crate::control::install(Some(control.clone()));
        let repaired = std::cell::Cell::new(0);
        let r = run_with_room(&inp, &p, &|_| true, |g, rule| {
            repaired.set(repaired.get() + 1);
            control.cancel();
            repair::repair_by(g, rule, &|_| true)
        });
        assert_eq!(r.err().as_deref(), Some(crate::control::CANCELLED));
        assert_eq!(repaired.get(), 1, "stopped before the second feature");

        let control = crate::control::Control::new();
        let _installed = crate::control::install(Some(control.clone()));
        let r = run_with_room(
            &inp,
            &p,
            &|_| {
                control.cancel();
                true
            },
            |g, rule| repair::repair_by(g, rule, &|_| true),
        );
        assert_eq!(r.err().as_deref(), Some(crate::control::CANCELLED), "stopped in the copies");
    }

    #[test]
    fn bounds_are_rounded_up() {
        assert_eq!(rounded_up(5.0 * (-26f64).exp2()), "7.5e-8");
        assert_eq!(rounded_up(5.0 * (-29f64).exp2()), "9.4e-9");
        assert_eq!(rounded_up(2.5e-3), "2.5e-3");
        assert_eq!(rounded_up(9.96), "1.0e1");
        assert_eq!(rounded_up(1.0), "1.0e0");
    }
}
