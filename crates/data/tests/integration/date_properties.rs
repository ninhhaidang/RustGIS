//! Date and DateTime fields against an independent calendar, as properties: reading and showing
//! days and moments, how they order, and how a where clause compares them with a literal. The
//! oracle below counts days from a closed-form year table, not from the code under test.
//!
//! Pull requests run a fixed seed; the nightly a new one with more cases. A failure gives its seed
//! and the smallest input found. To run a seed again, or deeper:
//! `PROPERTY_SEED=7 PROPERTY_CASES=20000 cargo test -p rustgis-data --test date_properties`.
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestRng, TestRunner};
use rustgis_data::query;
use rustgis_data::{Coord, Dataset, Date, DateTime, Field, FieldType, GeometryType, Value};
use std::cmp::Ordering;

const DAY_US: i64 = 86_400_000_000;
const MINUTE_US: i64 = 60_000_000;
/// The largest offset the data model accepts, in minutes (documented in `date.rs`).
const MAX_OFFSET: i64 = 18 * 60;

/// A number from the environment, else `default`.
fn env(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// Runs `test` over `strategy` with the convention's seeded runner.
fn check<S: Strategy>(strategy: S, default_cases: u32, test: impl Fn(S::Value) -> Result<(), TestCaseError>) {
    let seed = env("PROPERTY_SEED", 1);
    let mut bytes = [0u8; 32];
    bytes[..8].copy_from_slice(&seed.to_le_bytes());
    let cases = u32::try_from(env("PROPERTY_CASES", u64::from(default_cases))).unwrap_or(default_cases);
    let config = Config { cases, failure_persistence: None, ..Config::default() };
    let mut runner = TestRunner::new_with_rng(config, TestRng::from_seed(RngAlgorithm::ChaCha, &bytes));
    if let Err(e) = runner.run(&strategy, test) {
        panic!("PROPERTY_SEED={seed}: {e}");
    }
}

// ---- The oracle: proleptic Gregorian days from a closed-form count of years. ----

/// Whether `y` is a leap year in the proleptic Gregorian calendar (the century rules, from the definition).
fn leap(y: i64) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

/// Days in month `m` of year `y`, from the calendar's definition, independent of the code under test.
fn month_len(y: i64, m: i64) -> i64 {
    [31, if leap(y) { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31].get((m - 1) as usize).copied().unwrap_or(0)
}

/// Days from 0001-01-01 to January 1 of year `y`.
fn year_start(y: i64) -> i64 {
    365 * (y - 1) + (y - 1) / 4 - (y - 1) / 100 + (y - 1) / 400
}

/// Days from 1970-01-01, None when the day doesn't exist or is outside years 1 to 9999.
fn days(y: i64, m: i64, d: i64) -> Option<i64> {
    if !(1..=9999).contains(&y) || !(1..=12).contains(&m) || d < 1 || d > month_len(y, m) {
        return None;
    }
    let before: i64 = (1..m).map(|k| month_len(y, k)).sum();
    Some(year_start(y) + before + d - 1 - year_start(1970))
}

/// The year, month and day of a day count from 1970 (years 1 to 9999 only).
fn civil(n: i64) -> (i64, i64, i64) {
    let from_year_one = n + year_start(1970);
    let mut y = (from_year_one / 366 + 1).clamp(1, 9999);
    while y < 9999 && year_start(y + 1) <= from_year_one {
        y += 1;
    }
    let mut left = from_year_one - year_start(y);
    let mut m = 1;
    while left >= month_len(y, m) {
        left -= month_len(y, m);
        m += 1;
    }
    (y, m, left + 1)
}

/// The first microsecond of year 1 since 1970, the oracle's lower range end.
fn min_us() -> i64 {
    days(1, 1, 1).unwrap_or(0) * DAY_US
}

/// One past the last microsecond of year 9999 since 1970, the oracle's upper range end.
fn max_us() -> i64 {
    (days(9999, 12, 31).unwrap_or(0) + 1) * DAY_US
}

/// Whether `us` microseconds since 1970 fall in years 1 to 9999.
fn in_range(us: i64) -> bool {
    (min_us()..max_us()).contains(&us)
}

/// The text a moment shows: `micros` (UTC when `offset` is set) in its own offset, with the
/// fraction as the docs say (none, milliseconds, or six digits).
fn show(micros: i64, offset: Option<i64>) -> String {
    let shown = micros + offset.unwrap_or(0) * MINUTE_US;
    let (n, in_day) = (shown.div_euclid(DAY_US), shown.rem_euclid(DAY_US));
    let (y, m, d) = civil(n);
    let secs = in_day / 1_000_000;
    let frac = in_day % 1_000_000;
    let mut s = format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60);
    if frac != 0 {
        s += &if frac % 1000 == 0 { format!(".{:03}", frac / 1000) } else { format!(".{frac:06}") };
    }
    match offset {
        None => s,
        Some(0) => s + "Z",
        Some(o) => format!("{s}{}{:02}:{:02}", if o < 0 { '-' } else { '+' }, o.abs() / 60, o.abs() % 60),
    }
}

// ---- Generated texts, each with the answer the oracle gives. ----

/// The clock and zone of a date-time text.
#[derive(Clone, Debug)]
struct Clock {
    hour: i64,
    minute: i64,
    /// Seconds, when written; the fraction digits only with them.
    second: Option<i64>,
    frac: String,
    /// The zone: None (zoneless) or the offset in minutes.
    offset: Option<i64>,
    /// How the zone is written: 0 `Z`, 1 `z`, 2 `+hh:mm`, 3 `+hhmm`, 4 `+hh` (when minutes are 0, else `+hh:mm`).
    zone_form: u8,
    /// `T`, `t` or a space.
    sep: char,
}

#[derive(Clone, Debug)]
struct Spec {
    y: i64,
    m: i64,
    d: i64,
    clock: Option<Clock>,
}

impl Spec {
    /// The text this spec writes: date, clock, fraction and zone in one of the accepted shapes.
    fn text(&self) -> String {
        let mut s = format!("{:04}-{:02}-{:02}", self.y, self.m, self.d);
        let Some(c) = &self.clock else { return s };
        s.push(c.sep);
        s += &format!("{:02}:{:02}", c.hour, c.minute);
        if let Some(sec) = c.second {
            s += &format!(":{sec:02}");
            if !c.frac.is_empty() {
                s += &format!(".{}", c.frac);
            }
        }
        if let Some(o) = c.offset {
            let (sign, a) = (if o < 0 { '-' } else { '+' }, o.abs());
            s += &match c.zone_form {
                0 | 1 if o == 0 => (if c.zone_form == 0 { "Z" } else { "z" }).to_string(),
                3 => format!("{sign}{:02}{:02}", a / 60, a % 60),
                4 if a % 60 == 0 => format!("{sign}{:02}", a / 60),
                _ => format!("{sign}{:02}:{:02}", a / 60, a % 60),
            };
        }
        s
    }

    /// What reading the text must give: (micros, offset), or None when it must be refused.
    fn expected(&self) -> Option<(i64, Option<i64>)> {
        let day = days(self.y, self.m, self.d)?;
        let Some(c) = &self.clock else { return Some((day * DAY_US, None)) };
        if c.hour > 23 || c.minute > 59 || c.second.is_some_and(|s| s > 59) {
            return None;
        }
        // Digits past the sixth must be zeros: nothing is cut.
        if c.frac.bytes().skip(6).any(|b| b != b'0') {
            return None;
        }
        let first6: String = c.frac.chars().chain(std::iter::repeat('0')).take(6).collect();
        let us: i64 = first6.parse().ok()?;
        let wall = day * DAY_US + ((c.hour * 60 + c.minute) * 60 + c.second.unwrap_or(0)) * 1_000_000 + us;
        match c.offset {
            None => in_range(wall).then_some((wall, None)),
            Some(o) => {
                let utc = wall - o * MINUTE_US;
                (o.abs() <= MAX_OFFSET && in_range(wall) && in_range(utc)).then_some((utc, Some(o)))
            }
        }
    }
}

/// Years to try: the whole range, calendar edge cases, and a few outside it.
fn year() -> impl Strategy<Value = i64> {
    prop_oneof![
        3 => 1..=9999i64,
        2 => prop::sample::select(vec![1i64, 2, 4, 100, 400, 1582, 1899, 1900, 1969, 1970, 2000, 2024, 2100, 9996, 9998, 9999]),
        // Outside the years the model holds.
        1 => prop::sample::select(vec![0i64, 10000, 12345]),
    ]
}

/// Zone offsets in minutes: ordinary ones, the extremes, and a few past the accepted range.
fn offset() -> impl Strategy<Value = i64> {
    prop_oneof![
        2 => -MAX_OFFSET..=MAX_OFFSET,
        3 => prop::sample::select(vec![0i64, 345, -570, 840, -720, 60, -60, 330, MAX_OFFSET, -MAX_OFFSET, 1081, -1200]),
    ]
}

/// A clock with edge hours, minutes and seconds (24, 60), a fraction of up to nine digits and an offset.
fn clock() -> impl Strategy<Value = Clock> {
    let fraction = prop_oneof![
        1 => Just(String::new()),
        2 => "[0-9]{1,6}",
        2 => "[0-9]{6}0{1,3}",
        1 => "[0-9]{6}[0-9]{1,3}",
    ];
    (
        prop_oneof![9 => 0..=23i64, 1 => Just(24i64)],
        prop_oneof![9 => 0..=59i64, 1 => Just(60i64)],
        prop::option::weighted(0.8, prop_oneof![9 => 0..=59i64, 1 => Just(60i64)]),
        fraction,
        prop::option::weighted(0.7, offset()),
        0u8..5,
        prop::sample::select(vec!['T', 'T', 't', ' ']),
    )
        .prop_map(|(hour, minute, second, frac, offset, zone_form, sep)| Clock {
            hour,
            minute,
            second,
            frac: if second.is_some() { frac } else { String::new() },
            offset,
            zone_form,
            sep,
        })
}

/// A date or date-time text spec built from the generators above.
fn spec() -> impl Strategy<Value = Spec> {
    (
        year(),
        prop_oneof![9 => 1..=12i64, 1 => Just(13i64)],
        prop_oneof![9 => 1..=31i64, 1 => Just(0i64)],
        prop::option::weighted(0.8, clock()),
    )
        .prop_map(|(y, m, d, clock)| Spec { y, m, d, clock })
}

/// A spec that is a valid date-time more often: for ordering and clauses.
fn valid_spec() -> impl Strategy<Value = Spec> {
    spec().prop_filter("a moment the model holds", |s| s.expected().is_some())
}

/// Turns a failed property into the test runner's failure with its message.
fn fail(msg: String) -> Result<(), TestCaseError> {
    Err(TestCaseError::fail(msg))
}

// ---- Date ----

#[test]
fn a_date_is_the_proleptic_gregorian_day() {
    check((-2i64..10_003, -1i64..15, -1i64..33), 4096, |(y, m, d)| {
        let want = days(y, m, d);
        let got = Date::from_ymd(y, m, d);
        if got.map(|g| i64::from(g.days())) != want {
            return fail(format!("from_ymd({y},{m},{d}) = {got:?}, want {want:?}"));
        }
        let text = format!("{y:04}-{m:02}-{d:02}");
        // Only a four-digit year with two-digit month and day is the ISO form.
        let shaped = (0..=9999).contains(&y) && (0..=99).contains(&m) && (0..=99).contains(&d);
        let parsed = Date::parse(&text);
        if shaped && parsed.map(|p| i64::from(p.days())) != want {
            return fail(format!("parse({text}) = {parsed:?}, want {want:?}"));
        }
        if !shaped && parsed.is_some() {
            return fail(format!("parse({text}) = {parsed:?}, want none"));
        }
        if let (Some(g), Some(n)) = (got, want) {
            if g.ymd() != (y, m, d) || g.to_string() != text || Date::parse(&g.to_string()) != Some(g) {
                return fail(format!("{y}-{m}-{d}: ymd {:?}, display {g}", g.ymd()));
            }
            if civil(n) != (y, m, d) {
                return fail(format!("the oracle's own civil({n}) disagrees"));
            }
            if Date::from_days(n) != Some(g) {
                return fail(format!("from_days({n}) != from_ymd"));
            }
        }
        Ok(())
    });
}

#[test]
fn every_day_count_in_range_round_trips_and_the_rest_is_refused() {
    let last = days(9999, 12, 31).unwrap_or(0);
    let first = days(1, 1, 1).unwrap_or(0);
    check(
        prop_oneof![8 => first..=last, 2 => prop::sample::select(vec![
            first - 1, first, last, last + 1, i64::from(i32::MAX), i64::from(i32::MIN), i64::MAX, i64::MIN, 0, -1, 1, 11_016, 11_017,
        ])],
        4096,
        |n| {
            let got = Date::from_days(n);
            if (first..=last).contains(&n) {
                let Some(g) = got else { return fail(format!("from_days({n}) refused a day in range")) };
                let (y, m, d) = civil(n);
                if g.ymd() != (y, m, d) || g.to_string() != format!("{y:04}-{m:02}-{d:02}") || Date::parse(&g.to_string()) != Some(g) {
                    return fail(format!("day {n}: ymd {:?}, display {g}, oracle {:?}", g.ymd(), (y, m, d)));
                }
                if g.sort_key() != n * DAY_US {
                    return fail(format!("day {n}: sort_key {}", g.sort_key()));
                }
            } else if got.is_some() {
                return fail(format!("from_days({n}) = {got:?} outside years 1 to 9999"));
            }
            Ok(())
        },
    );
}

#[test]
fn dates_the_calendar_rejects_are_refused() {
    for bad in [
        "0000-01-01",
        "10000-01-01",
        "2026-13-01",
        "2026-00-10",
        "2026-01-00",
        "2026-02-29",
        "1900-02-29",
        "2100-02-29",
        "2026-04-31",
        "2026-1-01",
        "2026-01-1",
        "-001-01-01",
        "+026-01-01",
        "2026/01/01",
        "20260101",
        "2026-01-01T",
        "２０２６-01-01",
        "",
    ] {
        assert_eq!(Date::parse(bad), None, "{bad:?} is no date");
    }
    for good in ["2000-02-29", "2024-02-29", "1600-02-29", "0001-01-01", "9999-12-31", "1900-02-28", "1969-12-31", "1970-01-01"] {
        let d = Date::parse(good).unwrap_or_else(|| panic!("{good} is a date"));
        assert_eq!(d.to_string(), good);
    }
    assert_eq!(Date::parse("1969-12-31").map(Date::days), Some(-1));
    assert_eq!(Date::parse("1970-01-01").map(Date::days), Some(0));
}

// ---- DateTime ----

#[test]
fn a_date_time_reads_as_the_oracle_says_and_shows_back_the_same_moment() {
    check(spec(), 4096, |s| {
        let text = s.text();
        let got = DateTime::parse(&text);
        let want = s.expected();
        let got_pair = got.map(|t| (t.micros(), t.offset().map(i64::from)));
        if got_pair != want {
            return fail(format!("parse({text:?}) = {got_pair:?}, want {want:?}"));
        }
        if let (Some(t), Some((us, off))) = (got, want) {
            let shown = t.to_string();
            if shown != show(us, off) {
                return fail(format!("{text:?} shows as {shown}, the oracle says {}", show(us, off)));
            }
            if DateTime::parse(&shown) != Some(t) {
                return fail(format!("{shown} doesn't read back as {t:?}"));
            }
            // The date part is the day it shows.
            let day = (us + off.unwrap_or(0) * MINUTE_US).div_euclid(DAY_US);
            if i64::from(t.date().days()) != day {
                return fail(format!("{text:?}: date() is day {}, shown day {day}", t.date().days()));
            }
            // As a Value, the same.
            if Value::parse(&text, FieldType::DateTime) != Value::DateTime(t) {
                return fail(format!("Value::parse disagrees on {text:?}"));
            }
        }
        Ok(())
    });
}

#[test]
fn moments_at_the_ends_of_the_range() {
    // The shown time and the UTC instant must both fall in years 1 to 9999.
    for (text, ok) in [
        ("9999-12-31T23:59:59.999999Z", true),
        ("9999-12-31T23:59:59-14:00", false),
        ("9999-12-31T23:59:59+14:00", true),
        ("0001-01-01T00:00+14:00", false),
        ("0001-01-01T00:00-14:00", true),
        ("0001-01-01T00:00:00Z", true),
        ("0001-01-01T00:00:00", true),
        ("9999-12-31T24:00:00", false),
        ("2016-12-31T23:59:60Z", false),
        ("2026-03-05T14:30:00.1234567Z", false),
        ("2026-03-05T14:30:00.1234560Z", true),
        ("2026-03-05t14:30:00z", true),
        ("2026-03-05 14:30", true),
        ("2026-03-05T14:30:00+18:00", true),
        ("2026-03-05T14:30:00+18:01", false),
        ("2026-03-05T14:30:00-00:00", true),
    ] {
        assert_eq!(DateTime::parse(text).is_some(), ok, "{text}");
    }
}

#[test]
fn moments_order_by_instant_and_a_zoneless_time_reads_as_utc() {
    check((valid_spec(), valid_spec()), 4096, |(a, b)| {
        let ((ua, _), (ub, _)) = (a.expected().unwrap_or_default(), b.expected().unwrap_or_default());
        let ty = |s: &Spec| if s.clock.is_some() { FieldType::DateTime } else { FieldType::Date };
        let (va, vb) = (Value::parse(&a.text(), ty(&a)), Value::parse(&b.text(), ty(&b)));
        if va.is_null() || vb.is_null() {
            return fail(format!("{:?} or {:?} read as null", a.text(), b.text()));
        }
        if va.temporal_key() != Some(ua) || vb.temporal_key() != Some(ub) {
            return fail(format!("keys {:?}, {:?}; oracle {ua}, {ub} ({} / {})", va.temporal_key(), vb.temporal_key(), a.text(), b.text()));
        }
        if va.partial_cmp(&vb) != Some(ua.cmp(&ub)) {
            return fail(format!("{} vs {}: {:?}, oracle {:?}", a.text(), b.text(), va.partial_cmp(&vb), ua.cmp(&ub)));
        }
        // Against the other's text, as a where clause does.
        if va.temporal_keys(&Value::Text(b.text())) != Some((ua, ub)) {
            return fail(format!("{} vs the text {}: {:?}", a.text(), b.text(), va.temporal_keys(&Value::Text(b.text()))));
        }
        Ok(())
    });
}

#[test]
fn a_date_is_its_midnight_and_equal_to_a_zoneless_midnight() {
    check((1..=9999i64, 1..=12i64, 1..=31i64), 2048, |(y, m, d)| {
        let Some(n) = days(y, m, d) else { return Ok(()) };
        let text = format!("{y:04}-{m:02}-{d:02}");
        let date = Value::parse(&text, FieldType::Date);
        let local = Value::parse(&format!("{text}T00:00:00"), FieldType::DateTime);
        let utc = Value::parse(&format!("{text}T00:00:00Z"), FieldType::DateTime);
        let shifted = Value::parse(&format!("{text}T05:45:00+05:45"), FieldType::DateTime);
        for other in [&local, &utc, &shifted] {
            if other.is_null() {
                // A shifted moment near year 1 can leave the range.
                if y == 1 && std::ptr::eq(other, &shifted) {
                    continue;
                }
                return fail(format!("{text} midnight read as null"));
            }
            if date.partial_cmp(other) != Some(Ordering::Equal) || date.temporal_key() != Some(n * DAY_US) {
                return fail(format!("{text}: date {:?} vs {other:?}", date.temporal_key()));
            }
        }
        // `==` compares what is stored, so a zoned and a zoneless midnight differ.
        if local == utc {
            return fail(format!("{text}: zoneless and Z compare == though they store different things"));
        }
        // A date-time converted back to a date keeps its shown day.
        if let Value::DateTime(t) = shifted
            && t.date().to_string() != text
        {
            return fail(format!("{text}: the shifted moment's day is {}", t.date()));
        }
        Ok(())
    });
}

// ---- Where clauses ----

/// A literal in a clause: a full date or date-time (compares as a moment) or partial text.
#[derive(Clone, Debug)]
enum Lit {
    Moment(Spec),
    Text(&'static str),
}

/// A where-clause literal: a date, a date-time, or text that isn't a full date.
fn lit() -> impl Strategy<Value = Lit> {
    prop_oneof![
        4 => valid_spec().prop_map(Lit::Moment),
        1 => prop::sample::select(vec!["2026", "2026-03", "1969", "1969-12", "0001", "9999", "2000-02", "1970-01-01 "])
            .prop_map(Lit::Text),
    ]
}

impl Lit {
    /// The literal as it is written in a clause, quoted.
    fn sql(&self) -> String {
        match self {
            Lit::Moment(s) => format!("'{}'", s.text()),
            Lit::Text(t) => format!("'{t}'"),
        }
    }
}

/// A cell of the field under test: what the field holds, the answer of the oracle.
#[derive(Clone, Debug)]
struct Cell {
    value: Value,
    /// The instant it compares as, and the text it shows (None for null).
    shown: Option<(i64, String)>,
}

/// A cell: null, or a moment from `spec` read as `ty`.
fn cell(spec: &Spec, ty: FieldType) -> Cell {
    // A Date field holds only the day; a DateTime field the whole moment.
    let spec = if ty == FieldType::Date { Spec { clock: None, ..spec.clone() } } else { spec.clone() };
    let Some((us, off)) = spec.expected() else { return Cell { value: Value::Null, shown: None } };
    let value = Value::parse(&spec.text(), ty);
    Cell { value, shown: Some((us, if ty == FieldType::Date { spec.text() } else { show(us, off) })) }
}

/// SQL three-valued comparison of a cell with a literal: None when unknown.
fn compare(c: &Cell, op: &str, l: &Lit) -> Option<bool> {
    let (us, shown) = c.shown.as_ref()?;
    let ord = match l {
        Lit::Moment(s) => us.cmp(&s.expected()?.0),
        // Text that isn't a full date compares as text with the field's ISO text, ignoring case.
        Lit::Text(t) => shown.to_lowercase().cmp(&t.to_lowercase()),
    };
    Some(match op {
        "=" => ord.is_eq(),
        "<>" => ord.is_ne(),
        "<" => ord.is_lt(),
        "<=" => ord.is_le(),
        ">" => ord.is_gt(),
        _ => ord.is_ge(),
    })
}

/// A one-column-pair dataset of Date and DateTime fields holding `rows`, for the where-clause tests.
fn dataset(rows: &[(Cell, Cell)]) -> Dataset {
    let mut ds = Dataset::new("t", GeometryType::Point, vec![Field::new("d", FieldType::Date), Field::new("t", FieldType::DateTime)]);
    for (d, t) in rows {
        ds.push(Some(rustgis_data::Geometry::Point(Coord::new(0.0, 0.0))), vec![d.value.clone(), t.value.clone()]);
    }
    ds
}

#[test]
fn where_clauses_on_dates_agree_with_the_oracle() {
    let rows = prop::collection::vec((spec(), spec()), 1..6);
    let ops = prop::sample::select(vec!["=", "<>", "<", "<=", ">", ">="]);
    check((rows, ops, lit(), lit(), lit(), 0usize..6), 1024, |(specs, op, a, b, c, kind)| {
        let rows: Vec<(Cell, Cell)> = specs.iter().map(|(d, t)| (cell(d, FieldType::Date), cell(t, FieldType::DateTime))).collect();
        let ds = dataset(&rows);
        for (field, pick) in [("d", 0usize), ("t", 1)] {
            let cells: Vec<&Cell> = rows.iter().map(|r| if pick == 0 { &r.0 } else { &r.1 }).collect();
            let (clause, want): (String, Vec<Option<bool>>) = match kind {
                0 => (format!("{field} {op} {}", a.sql()), cells.iter().map(|c| compare(c, op, &a)).collect()),
                // The literal on the left reads as the mirrored comparison.
                1 => {
                    let mirrored = match op {
                        "<" => ">",
                        "<=" => ">=",
                        ">" => "<",
                        ">=" => "<=",
                        o => o,
                    };
                    (format!("{} {op} {field}", a.sql()), cells.iter().map(|c| compare(c, mirrored, &a)).collect())
                }
                2 => (
                    format!("{field} BETWEEN {} AND {}", a.sql(), b.sql()),
                    cells.iter().map(|c| compare(c, ">=", &a).zip(compare(c, "<=", &b)).map(|(x, y)| x && y)).collect(),
                ),
                3 => (
                    format!("{field} NOT BETWEEN {} AND {}", a.sql(), b.sql()),
                    cells.iter().map(|c| compare(c, ">=", &a).zip(compare(c, "<=", &b)).map(|(x, y)| !(x && y))).collect(),
                ),
                4 => (
                    format!("{field} IN ({}, {}, {})", a.sql(), b.sql(), c.sql()),
                    cells
                        .iter()
                        .map(|x| {
                            x.shown.as_ref()?;
                            Some([&a, &b, &c].iter().any(|l| compare(x, "=", l) == Some(true)))
                        })
                        .collect(),
                ),
                _ => (
                    format!("{field} NOT IN ({}, {})", a.sql(), b.sql()),
                    cells
                        .iter()
                        .map(|x| {
                            x.shown.as_ref()?;
                            Some(![&a, &b].iter().any(|l| compare(x, "=", l) == Some(true)))
                        })
                        .collect(),
                ),
            };
            let got = match query::select(&clause, &ds) {
                Ok(rows) => rows,
                Err(e) => return fail(format!("{clause} refused: {e}")),
            };
            let expect: Vec<usize> = want.iter().enumerate().filter(|(_, w)| **w == Some(true)).map(|(i, _)| i).collect();
            if got != expect {
                let shown: Vec<_> = cells.iter().map(|c| c.shown.as_ref().map(|s| s.1.clone())).collect();
                return fail(format!("{clause} on {shown:?}: rows {got:?}, oracle {expect:?}"));
            }
        }
        Ok(())
    });
}

#[test]
fn null_dates_never_match_and_is_null_finds_them() {
    let rows = vec![(
        cell(&Spec { y: 2026, m: 3, d: 5, clock: None }, FieldType::Date),
        cell(&Spec { y: 1, m: 1, d: 0, clock: None }, FieldType::DateTime),
    )];
    let ds = dataset(&rows);
    for clause in [
        "t = '2026-03-05'",
        "t <> '2026-03-05'",
        "t < '9999-01-01'",
        "t >= '0001-01-01'",
        "t BETWEEN '0001-01-01' AND '9999-12-31'",
        "t IN ('2026-03-05')",
        "t NOT IN ('2026-03-05')",
        "NOT (t = '2026-03-05')",
    ] {
        assert_eq!(query::select(clause, &ds), Ok(vec![]), "{clause}");
    }
    assert_eq!(query::select("t IS NULL", &ds), Ok(vec![0]));
    assert_eq!(query::select("d IS NOT NULL", &ds), Ok(vec![0]));
}

/// A FlatGeobuf writes a Date column as its native date-time column, and the reader types it again
/// from the values: a Date column with no value to look at (every cell null, or the only valued
/// features left out for having no geometry) comes back as DateTime, and the export says so.
#[test]
fn a_date_column_of_nulls_in_a_flatgeobuf_comes_back_as_a_date_time_and_is_said() {
    let mut ds = Dataset::new("layer", GeometryType::Point, vec![Field::new("a", FieldType::Date), Field::new("b", FieldType::Date)]);
    ds.push(Some(rustgis_data::Geometry::Point(Coord::new(1.0, 2.0))), vec![Value::Null, Value::parse("2026-03-05", FieldType::Date)]);
    let note = |ds: &Dataset| {
        let written = rustgis_data::io::written_values(ds.features.iter(), &ds.fields);
        rustgis_data::io::fgb_empty_dates_note(&ds.fields, &written, "out.fgb")
    };
    let said = note(&ds).unwrap_or_default();
    assert!(said.starts_with("out.fgb: 1 date field has no values") && said.ends_with("in 'a'"), "{said}");
    let path = std::env::temp_dir().join(format!("date-props-{}.fgb", std::process::id()));
    std::fs::write(&path, rustgis_data::source::fgb::write(&ds).unwrap_or_default()).unwrap_or_default();
    let back = rustgis_data::io::read_path(&path);
    let _ = std::fs::remove_file(&path);
    let (back, _) = back.unwrap_or_else(|e| panic!("read back: {e}"));
    let types: Vec<_> = back.fields.iter().map(|f| f.ty).collect();
    assert_eq!(types, [FieldType::DateTime, FieldType::Date]);
    // A value in every date field: nothing to say.
    ds.features[0].attrs[0] = Value::parse("2026-03-06", FieldType::Date);
    assert_eq!(note(&ds), None);
}
