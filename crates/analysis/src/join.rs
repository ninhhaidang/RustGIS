//! Spatial Join's and Near's measures, and Spatial Join's merge rules: the ground distance between
//! two features and their nearest points, a test that rules out features too far away to matter,
//! the overlap a feature has with a polygon, and the rules that roll the values of several matches
//! up into one.
//!
//! Distances are on the WGS 84 ellipsoid, as buffers' are. The nearest points of two features are
//! found in an azimuthal equidistant plane, centred first between the features and then between
//! the points found, and measured along the geodesic between them.

use rustgis_data::{Coord, Dataset, FieldType, Geometry, Value};
use rustgis_geom::{Aeqd, Extent, WGS84_MIN_RADIUS_M, geodesic_interpolate, geodesic_length_m, geodesic_ring_area_m2, wgs84_distance_m};
use serde_json::Value as Json;

/// Whether a coordinate is a place on the Earth in lon/lat: finite, with a latitude of ±90° at most
/// and a longitude within 540° of the meridian (a shape across the antimeridian goes past 180°).
/// Distances leave other coordinates out, as places they can't measure from.
fn on_earth(c: &Coord) -> bool {
    c.is_finite() && c.x.abs() <= 540.0 && c.y.abs() <= 90.0
}

/// The first coordinate of a geometry that is a place on the Earth.
pub fn first_coord(g: &Geometry) -> Option<Coord> {
    let mut first = None;
    g.for_each_coord(|c| {
        if first.is_none() && on_earth(&c) {
            first = Some(c);
        }
    });
    first
}

/// The ground distance (metres) between two features that don't intersect: between their nearest
/// points, edges and vertices alike. `None` for a geometry without a finite coordinate.
///
/// The nearest pair is found in a plane centred between the features' first vertices, then again
/// in one centred between the pair found, where the plane is true near the answer however large
/// the features are.
pub fn distance_m(a: &Geometry, b: &Geometry) -> Option<f64> {
    let (p, q) = nearest_points(a, b)?;
    let d = wgs84_distance_m(p, q);
    d.is_finite().then_some(d)
}

/// The nearest points (lon/lat) of two features that don't intersect, as [`distance_m`] finds
/// them: a point of `a`, then one of `b`. `None` for a geometry without a finite coordinate.
pub fn nearest_points(a: &Geometry, b: &Geometry) -> Option<(Coord, Coord)> {
    // The features' first vertices are a pair of their points: the search starts from them.
    let (fa, fb) = (first_coord(a)?, first_coord(b)?);
    let (p, q) = nearest_pair(a, b, &Aeqd::new(geodesic_interpolate(fa, fb, 0.5)), Some((fa, fb)))?;
    Some(nearest_pair(a, b, &Aeqd::new(geodesic_interpolate(p, q, 0.5)), Some((p, q))).unwrap_or((p, q)))
}

/// The nearest points of two geometries (lon/lat) as found in `plane`: a point of `a`, one of `b`.
/// `seed`, a pair already found, starts the search, so pairs no nearer than it are skipped.
fn nearest_pair(a: &Geometry, b: &Geometry, plane: &Aeqd, seed: Option<(Coord, Coord)>) -> Option<(Coord, Coord)> {
    let boxed = |segs: Vec<(Coord, Coord)>| -> Vec<((Coord, Coord), [f64; 4])> {
        segs.into_iter().map(|(s, e)| ((s, e), [s.x.min(e.x), s.y.min(e.y), s.x.max(e.x), s.y.max(e.y)])).collect()
    };
    let sa = boxed(segments(a, plane));
    // `b`'s edges by their west end, so the edges past one of `a`'s by more than the best distance
    // so far end the search for it.
    let mut sb = boxed(segments(b, plane));
    sb.sort_by(|x, y| x.1[0].total_cmp(&y.1[0]));
    let mut best: Option<(f64, Coord, Coord)> = seed
        .map(|(p, q)| {
            let (p, q) = (plane.forward(p), plane.forward(q));
            ((p.x - q.x).hypot(p.y - q.y), p, q)
        })
        .filter(|(d, _, _)| d.is_finite());
    // Two segments that don't cross are nearest at an end of one of them. A pair whose boxes are
    // already farther apart than the best pair so far can't hold a nearer one.
    for &((a0, a1), ba) in &sa {
        for &((b0, b1), bb) in &sb {
            if best.is_some_and(|(d, _, _)| bb[0] - ba[2] >= d) {
                break;
            }
            let gap = (bb[0] - ba[2]).max(ba[0] - bb[2]).max(0.0).hypot((bb[1] - ba[3]).max(ba[1] - bb[3]).max(0.0));
            if best.is_some_and(|(d, _, _)| gap >= d) {
                continue;
            }
            for (p, q) in
                [(a0, closest_on(a0, b0, b1)), (a1, closest_on(a1, b0, b1)), (closest_on(b0, a0, a1), b0), (closest_on(b1, a0, a1), b1)]
            {
                let d = (p.x - q.x).hypot(p.y - q.y);
                // An absurd coordinate can project to nothing finite: it never stands as the best.
                if d.is_finite() && best.is_none_or(|(b, _, _)| d < b) {
                    best = Some((d, p, q));
                }
            }
        }
    }
    let (_, p, q) = best?;
    Some((plane.inverse(p), plane.inverse(q)))
}

/// A geometry's edges in the plane; a point is an edge of no length.
fn segments(g: &Geometry, plane: &Aeqd) -> Vec<(Coord, Coord)> {
    let mut out = Vec::new();
    let mut path = |pts: &[Coord], closed: bool| {
        // A coordinate left out ends a run: no edge joins the runs on either side of it, and a
        // ring missing one isn't closed across the gap.
        let whole = pts.iter().all(on_earth);
        for run in pts.split(|c| !on_earth(c)).filter(|r| !r.is_empty()) {
            let v: Vec<Coord> = run.iter().map(|c| plane.forward(*c)).collect();
            match v.as_slice() {
                [] => {}
                [only] => out.push((*only, *only)),
                _ => {
                    out.extend(v.windows(2).filter_map(|w| Some((*w.first()?, *w.get(1)?))));
                    if let (true, Some(first), Some(last)) = (closed && whole, v.first(), v.last())
                        && first != last
                    {
                        out.push((*last, *first));
                    }
                }
            }
        }
    };
    match g {
        Geometry::Point(c) => path(std::slice::from_ref(c), false),
        Geometry::MultiPoint(v) => v.iter().for_each(|c| path(std::slice::from_ref(c), false)),
        Geometry::Line(parts) => parts.iter().for_each(|p| path(p, false)),
        Geometry::Polygon(polys) => polys.iter().flatten().for_each(|r| path(r, true)),
    }
    out
}

/// The point of the segment `a`–`b` nearest `p`.
fn closest_on(p: Coord, a: Coord, b: Coord) -> Coord {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 { ((p.x - a.x) * dx + (p.y - a.y) * dy) / len2 } else { 0.0 };
    let t = if t.is_finite() { t.clamp(0.0, 1.0) } else { 0.0 };
    Coord::new(a.x + t * dx, a.y + t * dy)
}

/// How far apart, in degrees of latitude and of longitude, two points within `metres` of each
/// other can be when one is no further than `lat` degrees from the equator. The longitude is
/// `None` when they can reach a pole, so any longitudes can be that close. Two points whose
/// longitudes differ by Δλ, both no further from the equator than φ, are at least
/// 2R·asin(cos φ·sin(Δλ/2)) apart, with R the ellipsoid's smallest radius of curvature, so the
/// ellipsoid's shorter distances near the equator are never ruled out.
fn spread(lat: f64, metres: f64) -> (f64, Option<f64>) {
    // A little slack, so a feature exactly at the distance isn't ruled out by rounding.
    let r = (metres.max(0.0) * (1.0 + 1e-9) + 1e-3) / WGS84_MIN_RADIUS_M;
    let dlat = r.to_degrees();
    let phi = (lat.abs() + dlat).to_radians();
    let reach = (r / 2.0).min(std::f64::consts::FRAC_PI_2).sin() / phi.cos();
    if phi >= std::f64::consts::FRAC_PI_2 || reach.is_nan() || reach >= 1.0 {
        return (dlat, None);
    }
    (dlat, Some((2.0 * reach.asin()).to_degrees()))
}

/// Whether a feature within the lon/lat extent `b` may lie within `metres` of one within `a`:
/// `false` only when every pair of points of the two extents is farther apart than that, at the
/// poles and across the antimeridian too.
pub fn may_be_within(a: &Extent, b: &Extent, metres: f64) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let (dlat, dlon) = spread(a.min_y.abs().max(a.max_y.abs()), metres);
    let lat_gap = (b.min_y - a.max_y).max(a.min_y - b.max_y).max(0.0);
    if lat_gap > dlat {
        return false;
    }
    dlon.is_none_or(|dlon| lon_gap(a, b) <= dlon)
}

/// The lon/lat extent that holds every point within `metres` of `ext`, for looking features up in
/// an index. `None` when that reaches a pole or crosses the antimeridian, where only looking at
/// every feature is sure to find them.
pub fn search_box(ext: &Extent, metres: f64) -> Option<Extent> {
    if ext.is_empty() || !metres.is_finite() {
        return None;
    }
    let (dlat, dlon) = spread(ext.min_y.abs().max(ext.max_y.abs()), metres);
    let dlon = dlon?;
    let (x0, x1) = (ext.min_x - dlon, ext.max_x + dlon);
    (x0 >= -180.0 && x1 <= 180.0).then(|| Extent::new(x0, ext.min_y - dlat, x1, ext.max_y + dlat))
}

/// The gap in longitude between two extents (degrees), the short way round.
fn lon_gap(a: &Extent, b: &Extent) -> f64 {
    if a.width() >= 360.0 || b.width() >= 360.0 {
        return 0.0;
    }
    [-360.0, 0.0, 360.0].iter().map(|k| (b.min_x + k - a.max_x).max(a.min_x - (b.max_x + k)).max(0.0)).fold(f64::INFINITY, f64::min)
}

/// The ground area (square metres) of a lon/lat polygon geometry, holes taken out.
pub fn area_m2(g: &Geometry) -> f64 {
    let Geometry::Polygon(polys) = g else { return 0.0 };
    polys
        .iter()
        .flat_map(|rings| rings.iter().enumerate())
        .map(|(i, r)| if i == 0 { geodesic_ring_area_m2(r) } else { -geodesic_ring_area_m2(r) })
        .sum()
}

/// The ground length (metres) of a lon/lat line geometry.
pub fn length_m(g: &Geometry) -> f64 {
    let Geometry::Line(parts) = g else { return 0.0 };
    parts.iter().map(|p| geodesic_length_m(p)).sum()
}

/// How the values of a join field's matches become one value in a one-to-one join.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    First,
    Last,
    Sum,
    Mean,
    Min,
    Max,
    Count,
    Join,
}

/// The rules' names, as the `merge` parameter spells them.
pub const RULES: &[(&str, Rule)] = &[
    ("first", Rule::First),
    ("last", Rule::Last),
    ("sum", Rule::Sum),
    ("mean", Rule::Mean),
    ("min", Rule::Min),
    ("max", Rule::Max),
    ("count", Rule::Count),
    ("join", Rule::Join),
];

impl Rule {
    /// Whether this rule can combine values of the given field type.
    pub fn fits(self, ty: FieldType) -> bool {
        match self {
            Self::Sum | Self::Mean => ty.is_numeric(),
            Self::Min | Self::Max => ty.is_numeric() || ty == FieldType::Text,
            _ => true,
        }
    }
}

/// A merge rule for one join field.
#[derive(Clone, Debug, PartialEq)]
pub struct Merge {
    /// The field's index in the join layer.
    pub field: usize,
    pub rule: Rule,
    /// What `join` puts between values.
    pub delimiter: String,
}

impl Merge {
    /// The type of the merged field, given the join field's.
    pub fn field_type(&self, ty: FieldType) -> FieldType {
        match self.rule {
            Rule::Mean => FieldType::Double,
            Rule::Count => FieldType::Integer,
            Rule::Join => FieldType::Text,
            _ => ty,
        }
    }

    /// The matches' values (in match order) merged into one, and what merging them lost. Every
    /// rule but `first` and `last` skips nulls; with nothing left a numeric rule gives null,
    /// `count` zero and `join` "". An integer field's sum adds its whole numbers exactly: a total
    /// too large for 64 bits is null, and said so, as is each number it leaves out (a decimal with
    /// a fraction, or one past the range). The other numeric rules leave out, and count, a number
    /// too large for a decimal (text such as "1e309", or an infinite decimal). `min` and `max`
    /// compare whole numbers exactly, past 2^53 too, where their decimals
    /// can be equal; a `mean` of whole numbers is worked out from their exact sum, and said to be
    /// rounded when it is a whole number a decimal can't hold.
    pub fn apply(&self, ty: FieldType, values: &[&Value]) -> (Value, Lost) {
        let present = || values.iter().copied().filter(|v| !matches!(v, Value::Null));
        let numbers: Vec<f64> = present().filter_map(Value::as_f64).collect();
        // The numbers no decimal holds, which `numbers` leaves out.
        let unread = present().filter(|v| is_number(v)).count().saturating_sub(numbers.len());
        let mut lost = Lost::default();
        let value = match self.rule {
            Rule::First => values.first().map(|v| (*v).clone()).unwrap_or(Value::Null),
            Rule::Last => values.last().map(|v| (*v).clone()).unwrap_or(Value::Null),
            Rule::Count => Value::Int(i64::try_from(present().count()).unwrap_or(i64::MAX)),
            Rule::Join => Value::Text(present().map(ToString::to_string).collect::<Vec<_>>().join(&self.delimiter)),
            Rule::Sum if ty == FieldType::Integer => {
                // A number that isn't a whole number within the 64-bit range can't be added to the
                // field's sum: it is left out, and counted. Text that isn't a number is no value.
                let numeric = || present().filter(|v| is_number(v));
                let ints: Vec<i64> = numeric().filter_map(whole_number).collect();
                lost.left_out = numeric().count().saturating_sub(ints.len());
                // The exact total, checked once: a running total may pass the range and come back.
                let total = ints.iter().try_fold(0i128, |s, i| s.checked_add(i128::from(*i)));
                match total.map(i64::try_from) {
                    _ if ints.is_empty() => Value::Null,
                    Some(Ok(sum)) => Value::Int(sum),
                    _ => {
                        lost.overflowed = true;
                        Value::Null
                    }
                }
            }
            Rule::Sum | Rule::Mean if numbers.is_empty() => {
                lost.left_out = unread;
                Value::Null
            }
            Rule::Sum => {
                lost.left_out = unread;
                Value::Float(numbers.iter().sum())
            }
            Rule::Mean => {
                lost.left_out = unread;
                // An integer field's values, when every one is a whole number, are summed exactly:
                // their decimals can't be, past 2^53. A decimal field's are decimals already.
                // Only the numbers a decimal holds count, as in `numbers`: text that isn't a number
                // is no value, and a number too large for a decimal is left out (counted above).
                let wholes: Option<Vec<i64>> =
                    (ty == FieldType::Integer).then(|| present().filter(|v| v.as_f64().is_some()).map(whole_number).collect()).flatten();
                match wholes {
                    Some(w) => {
                        let (sum, n) = (w.iter().map(|i| i128::from(*i)).sum::<i128>(), w.len() as i128);
                        // An evenly dividing mean is a whole number, rounded once to its decimal,
                        // and said to be rounded when the decimal isn't it.
                        let whole = sum % n == 0;
                        let mean = if whole { (sum / n) as f64 } else { sum as f64 / n as f64 };
                        lost.rounded = whole && mean as i128 != sum / n;
                        Value::Float(mean)
                    }
                    None => Value::Float(numbers.iter().sum::<f64>() / numbers.len() as f64),
                }
            }
            Rule::Min | Rule::Max => {
                // Numbers by value, exactly, text by its characters.
                let cmp = |a: &Value, b: &Value| match (number(a), number(b)) {
                    (Some(x), Some(y)) if ty.is_numeric() => crate::stats::cmp_number(&x, &y),
                    _ => a.to_string().cmp(&b.to_string()),
                };
                // A number no decimal holds is left out, and counted, as by `mean`.
                if ty.is_numeric() {
                    lost.left_out = unread;
                }
                let candidates = present().filter(|v| !ty.is_numeric() || v.as_f64().is_some());
                let best = if self.rule == Rule::Max {
                    // The first of equal values, as `min_by` keeps.
                    candidates.fold(None, |best: Option<&Value>, v| match best {
                        Some(b) if cmp(v, b).is_le() => Some(b),
                        _ => Some(v),
                    })
                } else {
                    candidates.min_by(|a, b| cmp(a, b))
                };
                best.cloned().unwrap_or(Value::Null)
            }
        };
        (value, lost)
    }
}

/// What merging one target's matches with a rule lost, for Spatial Join's notices.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Lost {
    /// An integer field's sum went past the 64-bit range, and is null.
    pub overflowed: bool,
    /// How many numbers the rule left out: for an integer field's sum, decimals with a fraction and
    /// numbers past the 64-bit range, which no whole number of the field holds; for the other
    /// numeric rules, numbers too large for a decimal.
    pub left_out: usize,
    /// A mean of whole numbers is a whole number past 2^53, which its decimal rounds.
    pub rounded: bool,
}

/// A value as the whole number it holds exactly: an integer, true or false, a decimal with no
/// fraction within the i64 range, or text written as one in any form a number is ("12", "12.0",
/// "1.2e1"), read from its digits. A number past the i64 range isn't one.
fn whole_number(v: &Value) -> Option<i64> {
    // 2^63: i64::MIN is exactly a float; i64::MAX rounds up to 2^63, which is out of range.
    const TWO_63: f64 = 9_223_372_036_854_775_808.0;
    match v {
        Value::Int(i) => Some(*i),
        Value::Text(t) => crate::stats::text_i64(t),
        _ => v.as_f64().filter(|f| f.fract() == 0.0 && (-TWO_63..TWO_63).contains(f)).map(|f| f as i64),
    }
}

/// Whether a value is a number: one a decimal holds, an infinite decimal, or text written as a
/// number too large for any ("1e309"). Not a number (NaN), and text that isn't one, "inf" included,
/// aren't.
fn is_number(v: &Value) -> bool {
    v.as_f64().is_some()
        || matches!(v, Value::Float(f) if f.is_infinite())
        || matches!(v, Value::Text(t) if t.bytes().any(|b| b.is_ascii_digit()) && t.trim().parse::<f64>().is_ok_and(f64::is_infinite))
}

/// A value as a number to compare exactly: a whole number as an integer (text written as one too,
/// read from its digits), any other number as a decimal. None for a value that isn't a number.
fn number(v: &Value) -> Option<Value> {
    match v {
        Value::Int(_) | Value::Float(_) => Some(v.clone()),
        Value::Bool(b) => Some(Value::Int(i64::from(*b))),
        Value::Text(t) => crate::stats::text_i64(t).map(Value::Int).or_else(|| v.as_f64().map(Value::Float)),
        Value::Null => None,
    }
}

/// The `merge` parameter: a list (or the text of one) such as
/// `[{"field": "units", "rule": "sum"}, {"field": "name", "rule": "join", "delimiter": "; "}]`,
/// naming fields of the join layer. Empty or absent is no rules.
pub fn parse_merge(join: &Dataset, v: Option<&Json>) -> Result<Vec<Merge>, String> {
    let parsed;
    let list = match v {
        None | Some(Json::Null) => return Ok(Vec::new()),
        Some(Json::String(t)) if t.trim().is_empty() => return Ok(Vec::new()),
        Some(Json::String(t)) => {
            parsed = serde_json::from_str::<Json>(t).map_err(|e| format!("Merge rules: {e}"))?;
            &parsed
        }
        Some(other) => other,
    };
    let items = list.as_array().ok_or("Merge rules must be a list such as [{\"field\": \"units\", \"rule\": \"sum\"}]")?;
    let mut out: Vec<Merge> = Vec::with_capacity(items.len());
    for item in items {
        let text = |k: &str| item.get(k).and_then(Json::as_str).map(str::trim);
        let name = text("field").filter(|n| !n.is_empty()).ok_or("each merge rule needs a \"field\"")?;
        let field = crate::find_field_case_insensitive(join, name)?;
        let rule_name = text("rule").unwrap_or("");
        let rule = RULES.iter().find(|(n, _)| n.eq_ignore_ascii_case(rule_name)).map(|(_, r)| *r).ok_or_else(|| {
            format!(
                "merge rule for '{name}' must be one of {} (not '{rule_name}')",
                RULES.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ")
            )
        })?;
        let ty = join.fields.get(field).map(|f| f.ty).unwrap_or(FieldType::Text);
        if !rule.fits(ty) {
            let needs = if matches!(rule, Rule::Min | Rule::Max) { "a numeric or text field" } else { "a numeric field" };
            return Err(format!("merge rule '{rule_name}' needs {needs} ('{name}' is not)"));
        }
        if out.iter().any(|m| m.field == field) {
            return Err(format!("'{name}' has more than one merge rule"));
        }
        let delimiter = match item.get("delimiter") {
            None | Some(Json::Null) => ", ".to_string(),
            Some(Json::String(d)) => d.clone(),
            Some(_) => return Err(format!("the delimiter for '{name}' must be text")),
        };
        out.push(Merge { field, rule, delimiter });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    use rustgis_geom::haversine_m;

    /// The ground distance from `p` to a line, by sampling each edge finely along its great circle.
    /// Only the samples within 1.2% of the nearest on the sphere are measured on the ellipsoid,
    /// whose distances are within 0.6% of the sphere's.
    fn brute(p: Coord, line: &[Coord]) -> f64 {
        let samples: Vec<(Coord, f64)> = line
            .windows(2)
            .flat_map(|w| (0..=400).map(move |k| geodesic_interpolate(w[0], w[1], f64::from(k) / 400.0)))
            .map(|c| (c, haversine_m(p, c)))
            .collect();
        let nearest = samples.iter().map(|(_, d)| *d).fold(f64::INFINITY, f64::min);
        samples.iter().filter(|(_, d)| *d <= nearest * 1.012 + 1.0).map(|(c, _)| wgs84_distance_m(p, *c)).fold(f64::INFINITY, f64::min)
    }

    #[test]
    fn distances_are_on_the_wgs84_ellipsoid() {
        // 0.0904369° north of the equator is 10,000 m on WGS 84 (GeographicLib) but 10,056 m on the
        // mean sphere: within 10 km in the measure, and not ruled out before it (#888).
        let (a, b) = (Coord::new(0.0, 0.0), Coord::new(0.0, 0.090_436_9));
        assert!(haversine_m(a, b) > 10_050.0);
        let d = distance_m(&Geometry::Point(a), &Geometry::Point(b)).unwrap();
        assert!((d - 9_999.994_808).abs() < 1e-5, "{d}");
        let (ea, eb) = (Extent::new(a.x, a.y, a.x, a.y), Extent::new(b.x, b.y, b.x, b.y));
        assert!(may_be_within(&ea, &eb, 10_000.0));
        assert!(search_box(&ea, 10_000.0).is_some_and(|e| e.max_y >= b.y));
    }

    #[test]
    fn an_integer_sum_takes_decimals_over_the_whole_i64_range() {
        let sum = Merge { field: 0, rule: Rule::Sum, delimiter: String::new() };
        let (min, past_max) = (Value::Float(i64::MIN as f64), Value::Float(9_223_372_036_854_775_808.0));
        assert_eq!(sum.apply(FieldType::Integer, &[&min]).0, Value::Int(i64::MIN));
        // 2^63 is one past i64::MAX: not a whole number the field holds, so it's left out.
        assert_eq!(sum.apply(FieldType::Integer, &[&past_max, &Value::Int(3)]).0, Value::Int(3));
        assert_eq!(sum.apply(FieldType::Integer, &[&Value::Float(2.5)]).0, Value::Null);
        // What a sum leaves out is counted: 2^63 and 2.5, not text that isn't a number.
        let skipped = sum.apply(FieldType::Integer, &[&past_max, &Value::Float(2.5), &Value::Text("n/a".into()), &Value::Int(3)]);
        assert_eq!(skipped, (Value::Int(3), Lost { left_out: 2, ..Lost::default() }));
        // Text written as a number too large for a decimal is a number left out too; "inf" isn't one.
        let huge = [Value::Text("1e309".into()), Value::Text("-1E400".into()), Value::Text("inf".into()), Value::Int(3)];
        let huge: Vec<&Value> = huge.iter().collect();
        assert_eq!(sum.apply(FieldType::Integer, &huge), (Value::Int(3), Lost { left_out: 2, ..Lost::default() }));
        // The other numeric rules leave out what no decimal holds, and count it.
        let mean = Merge { field: 0, rule: Rule::Mean, delimiter: String::new() };
        let left_out = |n| Lost { left_out: n, ..Lost::default() };
        assert_eq!(mean.apply(FieldType::Integer, &huge), (Value::Float(3.0), left_out(2)));
        assert_eq!(mean.apply(FieldType::Double, &huge), (Value::Float(3.0), left_out(2)));
        assert_eq!(sum.apply(FieldType::Double, &huge), (Value::Float(3.0), left_out(2)));
        assert_eq!(mean.apply(FieldType::Integer, &huge[..1]), (Value::Null, left_out(1)));
        let infinite = Value::Float(f64::INFINITY);
        // What is left out doesn't stop the exact mean of the rest: 2^53 + 1 twice is rounded.
        let big = Value::Int((1 << 53) + 1);
        let rounded = Lost { left_out: 1, rounded: true, ..Lost::default() };
        assert_eq!(mean.apply(FieldType::Integer, &[&infinite, &big, &big]), (Value::Float(9_007_199_254_740_992.0), rounded));
        assert_eq!(mean.apply(FieldType::Integer, &[&big, huge[0], &big]), (Value::Float(9_007_199_254_740_992.0), rounded));
        assert_eq!(sum.apply(FieldType::Integer, &[&infinite, &Value::Float(f64::NAN), &Value::Int(3)]), (Value::Int(3), left_out(1)));
        let max = Merge { field: 0, rule: Rule::Max, delimiter: String::new() };
        let min = Merge { field: 0, rule: Rule::Min, delimiter: String::new() };
        assert_eq!(max.apply(FieldType::Integer, &huge), (Value::Int(3), left_out(2)));
        assert_eq!(min.apply(FieldType::Integer, &huge), (Value::Int(3), left_out(2)));
        assert_eq!(max.apply(FieldType::Double, &[&Value::Int(3), &infinite]), (Value::Int(3), left_out(1)));
        // A text field's min and max take every value, as text.
        assert_eq!(max.apply(FieldType::Text, &huge), (Value::Text("inf".into()), Lost::default()));
        // The total is checked once: i64::MAX + 1 - 1 is i64::MAX; i64::MAX + 1 isn't a sum.
        let (max, one, minus) = (Value::Int(i64::MAX), Value::Int(1), Value::Int(-1));
        assert_eq!(sum.apply(FieldType::Integer, &[&max, &one, &minus]), (max.clone(), Lost::default()));
        assert_eq!(sum.apply(FieldType::Integer, &[&max, &one]), (Value::Null, Lost { overflowed: true, ..Lost::default() }));
    }

    #[test]
    fn a_left_out_coordinate_does_not_join_its_neighbours() {
        // The line goes north to a coordinate that is no place, and back: no edge runs straight
        // between the two real vertices, past the point between them.
        let line = Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(600.0, 80.0), Coord::new(10.0, 0.0)]]);
        let d = distance_m(&Geometry::Point(Coord::new(5.0, 0.0)), &line).unwrap();
        let truth = wgs84_distance_m(Coord::new(5.0, 0.0), Coord::new(0.0, 0.0));
        assert!((d - truth).abs() < 1e-6 * truth, "{d} vs {truth}");
        // Nor does a ring close across the gap.
        let ring = Geometry::Polygon(vec![vec![vec![
            Coord::new(0.0, 0.0),
            Coord::new(10.0, 0.0),
            Coord::new(f64::NAN, 5.0),
            Coord::new(0.0, 10.0),
            Coord::new(0.0, 0.0),
        ]]]);
        let d = distance_m(&Geometry::Point(Coord::new(5.0, 5.2)), &ring).unwrap();
        assert!(d > 400_000.0, "{d}");
    }

    #[test]
    fn an_absurd_coordinate_does_not_hide_a_real_one() {
        // A finite longitude far out of range projects to nothing finite; the real point beside it
        // is still measured, whichever comes first.
        // With the plane centred near f64::MAX, a longitude of -f64::MAX is infinitely far from it.
        let (origin, near) = (Coord::new(0.0, 0.0), Coord::new(0.001, 0.0));
        let a = Geometry::MultiPoint(vec![Coord::new(f64::MAX, 0.0), origin]);
        for pts in [vec![Coord::new(-f64::MAX, 0.0), near], vec![near, Coord::new(-f64::MAX, 0.0)]] {
            let d = distance_m(&a, &Geometry::MultiPoint(pts.clone()));
            let truth = wgs84_distance_m(origin, near);
            assert!(d.is_some_and(|d| (d - truth).abs() < 1e-6 * truth), "{pts:?}: {d:?} vs {truth}");
        }
    }

    #[test]
    fn distances_are_true_far_from_the_features_first_vertices() {
        // A line 90° of longitude long at 60°N, measured from beside its far end: the nearest
        // points are about 4,500 km from the line's first vertex.
        let line: Vec<Coord> = (0..=90).map(|x| Coord::new(f64::from(x), 60.0)).collect();
        for p in [Coord::new(89.5, 60.5), Coord::new(45.3, 59.2), Coord::new(91.0, 61.0)] {
            let d = distance_m(&Geometry::Point(p), &Geometry::Line(vec![line.clone()])).unwrap();
            let truth = brute(p, &line);
            assert!((d - truth).abs() < 1e-4 * truth, "{p:?}: {d} vs {truth}");
        }
        // Between two large features: lines 90° long, overlapping in longitude only at their far
        // ends, and a polygon's far side from a line beside it.
        let (a, b): (Vec<Coord>, Vec<Coord>) = (
            (0..=90).map(|x| Coord::new(f64::from(x), 60.0)).collect(),
            (80..=170).map(|x| Coord::new(f64::from(x) + 0.5, 61.0 + f64::from(x - 80) * 0.01)).collect(),
        );
        let d = distance_m(&Geometry::Line(vec![a.clone()]), &Geometry::Line(vec![b.clone()])).unwrap();
        let truth = a.iter().map(|p| brute(*p, &b)).chain(b.iter().map(|p| brute(*p, &a))).fold(f64::INFINITY, f64::min);
        assert!((d - truth).abs() < 1e-4 * truth, "lines: {d} vs {truth}");
        // A polygon's far side, from outside it.
        let mut ring: Vec<Coord> = (-80..=80).map(|x| Coord::new(f64::from(x), 50.0)).collect();
        ring.extend((-80..=80).rev().map(|x| Coord::new(f64::from(x), 70.0)));
        ring.push(Coord::new(-80.0, 50.0));
        let p = Coord::new(79.0, 49.0);
        let d = distance_m(&Geometry::Point(p), &Geometry::Polygon(vec![vec![ring.clone()]])).unwrap();
        let truth = brute(p, &ring);
        assert!((d - truth).abs() < 1e-4 * truth, "{d} vs {truth}");
        let road: Vec<Coord> = (40..=82).map(|x| Coord::new(f64::from(x), 48.5 + f64::from(x - 40) * 0.01)).collect();
        let d = distance_m(&Geometry::Line(vec![road.clone()]), &Geometry::Polygon(vec![vec![ring.clone()]])).unwrap();
        let truth = road.iter().map(|p| brute(*p, &ring)).chain(ring.iter().map(|p| brute(*p, &road))).fold(f64::INFINITY, f64::min);
        assert!((d - truth).abs() < 1e-4 * truth, "line to polygon: {d} vs {truth}");
    }
}
