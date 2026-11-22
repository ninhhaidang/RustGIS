//! Dates and times for the time slider: reading attribute values as instants, calendar steps, and
//! formatting. Instants are milliseconds since 1970-01-01 UTC (as `f64`, exact well beyond any
//! date a map shows); everything is UTC.

use crate::date::{civil_from_days, days_from_civil, days_in_month};
use crate::value::Value;

/// The years an instant may fall in: far past any data, small enough that day counts can't
/// overflow.
const MAX_YEAR: i64 = 1_000_000;
const DAY_MS: f64 = 86_400_000.0;
const HOUR_MS: f64 = 3_600_000.0;

/// A calendar unit the slider steps by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Unit {
    Hour,
    #[default]
    Day,
    Month,
    Year,
}

impl Unit {
    pub const ALL: [Unit; 4] = [Unit::Hour, Unit::Day, Unit::Month, Unit::Year];

    pub fn code(self) -> &'static str {
        match self {
            Unit::Hour => "hour",
            Unit::Day => "day",
            Unit::Month => "month",
            Unit::Year => "year",
        }
    }

    pub fn from_code(s: &str) -> Option<Unit> {
        let s = s.trim();
        let s = s.strip_suffix(['s', 'S']).unwrap_or(s);
        Unit::ALL.into_iter().find(|u| u.code().eq_ignore_ascii_case(s))
    }

    /// The unit that steps through a span of `ms` in a sensible number of steps: hours up to two
    /// days, days up to four months, months up to three years, years beyond.
    pub fn for_span(ms: f64) -> Unit {
        let days = ms / DAY_MS;
        if days <= 2.0 {
            Unit::Hour
        } else if days <= 120.0 {
            Unit::Day
        } else if days <= 3.0 * 366.0 {
            Unit::Month
        } else {
            Unit::Year
        }
    }

    /// The date format that shows this unit's steps apart.
    pub fn format(self) -> &'static str {
        match self {
            Unit::Hour => "YYYY-MM-DD HH:mm",
            Unit::Day => "YYYY-MM-DD",
            Unit::Month => "YYYY-MM",
            Unit::Year => "YYYY",
        }
    }
}

/// `t` moved by `n` units on the calendar (a month after Jan 31 is the last day of February).
pub fn add(t: f64, unit: Unit, n: i64) -> f64 {
    if !t.is_finite() {
        return t;
    }
    match unit {
        Unit::Hour => t + n as f64 * HOUR_MS,
        Unit::Day => t + n as f64 * DAY_MS,
        Unit::Month | Unit::Year => {
            let months = if unit == Unit::Year { n.saturating_mul(12) } else { n };
            let c = Civil::of(t);
            let total = (c.year * 12 + c.month - 1).saturating_add(months);
            let (y, m) = (total.div_euclid(12).clamp(-MAX_YEAR, MAX_YEAR), total.rem_euclid(12) + 1);
            let day = c.day.min(days_in_month(y, m));
            Civil { year: y, month: m, day, ..c }.ms()
        }
    }
}

/// `t` rounded down to the start of its unit (its hour, day, month or year).
pub fn floor(t: f64, unit: Unit) -> f64 {
    if !t.is_finite() {
        return t;
    }
    let c = Civil::of(t);
    let c = match unit {
        Unit::Hour => Civil { minute: 0, second: 0.0, ..c },
        Unit::Day => Civil { hour: 0, minute: 0, second: 0.0, ..c },
        Unit::Month => Civil { day: 1, hour: 0, minute: 0, second: 0.0, ..c },
        Unit::Year => Civil { month: 1, day: 1, hour: 0, minute: 0, second: 0.0, ..c },
    };
    c.ms()
}

/// The steps from `start` to `end` (both included when `end` falls on a step), `interval` units
/// apart; at most `cap` of them.
pub fn steps(start: f64, end: f64, unit: Unit, interval: u32, cap: usize) -> Vec<f64> {
    let mut out = Vec::new();
    if !(start.is_finite() && end.is_finite()) {
        return out;
    }
    let k = i64::from(interval.max(1));
    let mut i = 0i64;
    while out.len() < cap {
        let t = add(start, unit, i * k);
        if t > end || !t.is_finite() {
            break;
        }
        out.push(t);
        i += 1;
    }
    out
}

/// How a field stores its times.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Numbers of milliseconds since 1970.
    EpochMs,
    /// Numbers of seconds since 1970.
    EpochS,
    /// Whole years (`1958`).
    Year,
    /// Dates: values of date and date-time fields, and text dates (`2015-03-01`,
    /// `2015-03-01T12:00:00Z`, `2015/3/1`).
    Text,
}

/// What kind of time a value is, if it reads as one: numbers from 1e11 are milliseconds since
/// 1970 and from 1e8 seconds; whole numbers from 1000 to 9999 are years; text is a date, as are
/// dates and date-times.
pub fn kind_of(v: &Value) -> Option<Kind> {
    match v {
        Value::Date(_) | Value::DateTime(_) => Some(Kind::Text),
        Value::Int(i) => number_kind(*i as f64),
        Value::Float(x) => number_kind(*x),
        Value::Text(s) => {
            let t = s.trim();
            match plain_number(t) {
                Some(x) => number_kind(x),
                None => parse_text(t).map(|_| Kind::Text),
            }
        }
        Value::Null | Value::Bool(_) => None,
    }
}

/// Epoch times from 1900 to 2100, in seconds: a plausible span for data (larger numbers are
/// more likely counts or ids).
const EPOCH_S: std::ops::RangeInclusive<f64> = -2.21e9..=4.11e9;

fn number_kind(x: f64) -> Option<Kind> {
    if !x.is_finite() {
        None
    } else if x.abs() >= 1e11 {
        EPOCH_S.contains(&(x / 1000.0)).then_some(Kind::EpochMs)
    } else if x.abs() >= 1e8 {
        EPOCH_S.contains(&x).then_some(Kind::EpochS)
    } else if x.fract() == 0.0 && (1000.0..=9999.0).contains(&x) {
        Some(Kind::Year)
    } else {
        None
    }
}

/// The instant a value names, read as `kind` (`None` for a value that isn't one). Dates and
/// date-times name theirs whatever the kind, a time without a zone read as UTC.
pub fn instant(v: &Value, kind: Kind) -> Option<f64> {
    if let Some(us) = v.temporal_key() {
        return Some(us as f64 / 1000.0);
    }
    let x = match v {
        Value::Int(i) => Some(*i as f64),
        Value::Float(x) => Some(*x),
        Value::Text(s) => plain_number(s.trim()),
        Value::Null | Value::Bool(_) | Value::Date(_) | Value::DateTime(_) => return None,
    };
    let t = match (kind, x) {
        // Epoch times only as the field's were recognised (1900 to 2100, at their scale): a stray
        // 0 or 42 isn't 1970.
        (Kind::EpochMs, Some(x)) if number_kind(x) == Some(Kind::EpochMs) => x,
        (Kind::EpochS, Some(x)) if number_kind(x) == Some(Kind::EpochS) => x * 1000.0,
        (Kind::Year, Some(x)) if x.fract() == 0.0 && (1000.0..=9999.0).contains(&x) => Civil::date(x as i64, 1, 1).ms(),
        (Kind::Text, None) => parse_text(text(v)?)?,
        // A text field's bare year, or a number in a field of dates.
        (Kind::Text, Some(x)) => match number_kind(x)? {
            Kind::EpochMs => x,
            Kind::EpochS => x * 1000.0,
            _ => Civil::date(x as i64, 1, 1).ms(),
        },
        _ => return None,
    };
    // Past any calendar a map shows (and where day counts would overflow): not a date.
    (t.is_finite() && t.abs() <= LIMIT_MS).then_some(t)
}

/// The latest instant read, either side of 1970: about 950,000 years.
const LIMIT_MS: f64 = 3.0e16;

/// The kind most of a field's values read as, and the share of the non-empty values that do
/// (looking at up to `sample` rows).
pub fn detect<'a>(values: impl Iterator<Item = &'a Value>, sample: usize) -> Option<(Kind, f64)> {
    const KINDS: [Kind; 4] = [Kind::EpochMs, Kind::EpochS, Kind::Year, Kind::Text];
    let mut counts = [0usize; KINDS.len()];
    let count = |counts: &[usize], k: Kind| KINDS.iter().position(|x| *x == k).and_then(|i| counts.get(i)).copied().unwrap_or(0);
    let mut seen = 0usize;
    for v in values.filter(|v| !matches!(v, Value::Null) && !text(v).is_some_and(|s| s.trim().is_empty())).take(sample) {
        seen += 1;
        if let Some(c) = kind_of(v).and_then(|k| KINDS.iter().position(|x| *x == k)).and_then(|i| counts.get_mut(i)) {
            *c += 1;
        }
    }
    let (i, n) = counts.iter().enumerate().max_by_key(|(_, n)| **n)?;
    let kind = KINDS.get(i).copied()?;
    // Years and text dates mix (`1958`, `1958-03-01`): counted together as text, which reads both.
    let (text, years) = (count(&counts, Kind::Text), count(&counts, Kind::Year));
    let (kind, n) = if matches!(kind, Kind::Text | Kind::Year) && text > 0 { (Kind::Text, text + years) } else { (kind, *n) };
    (n > 0 && seen > 0).then(|| (kind, n as f64 / seen as f64))
}

/// A number written plainly (`2015`, `-12`, `1451070887.19`): text such as `1e3`, `+2015` or
/// `inf` is no number here, so codes don't read as years.
fn plain_number(s: &str) -> Option<f64> {
    let digits = s.strip_prefix('-').unwrap_or(s);
    let ok = !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        && digits.bytes().filter(|b| *b == b'.').count() <= 1
        && digits.bytes().any(|b| b.is_ascii_digit());
    if ok { s.parse::<f64>().ok() } else { None }
}

/// A text value's text.
fn text(v: &Value) -> Option<&str> {
    match v {
        Value::Text(s) => Some(s.trim()),
        _ => None,
    }
}

/// Reads a text date or date and time: `YYYY`, `YYYY-MM`, `YYYY-MM-DD` (or with `/`), then
/// optionally `T` or a space and `HH:MM[:SS[.fff]]`, then `Z` or an offset (`+05:30`).
pub fn parse_text(s: &str) -> Option<f64> {
    let s = s.trim();
    let (date, time) = match s.find(['T', 't', ' ']) {
        Some(i) => (s.get(..i)?, Some(s.get(i + 1..)?.trim()).filter(|t| !t.is_empty())?),
        None => (s, ""),
    };
    let neg = date.starts_with('-');
    let mut parts = date.trim_start_matches('-').split(['-', '/']);
    let y = digits(parts.next()?, 4, 6)?;
    let y = if neg { -y } else { y };
    let m = parts.next().map(|p| digits(p, 1, 2)).unwrap_or(Some(1))?;
    let d = parts.next().map(|p| digits(p, 1, 2)).unwrap_or(Some(1))?;
    if parts.next().is_some() || !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m) || y.abs() > MAX_YEAR {
        return None;
    }
    let mut c = Civil::date(y, m, d);
    let mut offset_min = 0i64;
    if !time.is_empty() {
        // Split off the zone: `Z`, or a sign after the clock.
        let (clock, zone) = match time.find(['Z', 'z', '+']).or_else(|| time.get(1..).and_then(|r| r.find('-')).map(|i| i + 1)) {
            Some(i) => (time.get(..i)?, time.get(i..)?),
            None => (time, ""),
        };
        let mut hms = clock.trim().split(':');
        c.hour = digits(hms.next()?, 1, 2)?;
        c.minute = hms.next().map(|p| digits(p, 1, 2)).unwrap_or(Some(0))?;
        c.second = match hms.next() {
            // Up to 60.999: a leap second is accepted on purpose (it reads as the next minute's
            // start).
            Some(p) => p.parse::<f64>().ok().filter(|x| (0.0..61.0).contains(x))?,
            None => 0.0,
        };
        if hms.next().is_some() || c.hour > 24 || (c.hour == 24 && (c.minute != 0 || c.second != 0.0)) || c.minute > 59 {
            return None;
        }
        let zone = zone.trim();
        if !(zone.is_empty() || zone.eq_ignore_ascii_case("z")) {
            let sign = if zone.starts_with('-') { -1 } else { 1 };
            let z = zone.get(1..)?.replace(':', "");
            let (hh, mm) = (digits(z.get(..2)?, 2, 2)?, z.get(2..).filter(|r| !r.is_empty()).map(|r| digits(r, 2, 2)).unwrap_or(Some(0))?);
            offset_min = sign * (hh * 60 + mm);
        }
    }
    Some(c.ms() - offset_min as f64 * 60_000.0)
}

/// A run of `min` to `max` ASCII digits as a number.
fn digits(s: &str, min: usize, max: usize) -> Option<i64> {
    (s.len() >= min && s.len() <= max && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok()).flatten()
}

/// Formats an instant with tokens `YYYY`, `YY`, `MMMM` (January), `MMM` (Jan), `MM`, `M`, `DD`,
/// `D`, `HH`, `H`, `mm`, `ss`; other characters are kept, and so is text in brackets (`[Day] D`).
pub fn format(t: f64, fmt: &str) -> String {
    if !t.is_finite() {
        return String::new();
    }
    const MONTHS: [&str; 12] =
        ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
    let c = Civil::of(t);
    let month = MONTHS.get((c.month - 1).clamp(0, 11) as usize).copied().unwrap_or("");
    let tokens: [(&str, String); 12] = [
        ("YYYY", format!("{:04}", c.year)),
        ("YY", format!("{:02}", c.year.rem_euclid(100))),
        ("MMMM", month.to_string()),
        ("MMM", month.chars().take(3).collect()),
        ("MM", format!("{:02}", c.month)),
        ("M", c.month.to_string()),
        ("DD", format!("{:02}", c.day)),
        ("D", c.day.to_string()),
        ("HH", format!("{:02}", c.hour)),
        ("H", c.hour.to_string()),
        ("mm", format!("{:02}", c.minute)),
        ("ss", format!("{:02}", c.second.floor() as i64)),
    ];
    let mut out = String::new();
    let mut rest = fmt;
    'outer: while !rest.is_empty() {
        // `[text]` is kept as it is (`YYYY-MM-DD[T]HH`).
        if let Some(r) = rest.strip_prefix('[')
            && let Some(end) = r.find(']')
        {
            out.push_str(r.get(..end).unwrap_or(""));
            rest = r.get(end + 1..).unwrap_or("");
            continue;
        }
        for (tok, val) in &tokens {
            if let Some(r) = rest.strip_prefix(tok) {
                out.push_str(val);
                rest = r;
                continue 'outer;
            }
        }
        let mut ch = rest.chars();
        out.extend(ch.next());
        rest = ch.as_str();
    }
    out
}

/// Fills a URL's date placeholders with an instant: `{date:FORMAT}` (any [`format`] tokens) and
/// the bare `{YYYY}`, `{MM}`, `{DD}`, `{HH}`, `{YYYYMMDD}`; tile placeholders like `{z}` are kept.
pub fn fill_template(template: &str, t: f64) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(i) = rest.find('{') {
        out.push_str(rest.get(..i).unwrap_or(""));
        let after = rest.get(i..).unwrap_or("");
        let Some(j) = after.find('}') else {
            out.push_str(after);
            return out;
        };
        let inner = after.get(1..j).unwrap_or("");
        match inner.strip_prefix("date:") {
            Some(f) => out.push_str(&format(t, f)),
            None if matches!(inner, "YYYY" | "MM" | "DD" | "HH" | "YYYYMMDD" | "YYYY-MM-DD") => out.push_str(&format(t, inner)),
            None => out.push_str(after.get(..=j).unwrap_or("")),
        }
        rest = after.get(j + 1..).unwrap_or("");
    }
    out.push_str(rest);
    out
}

/// The finest unit a dated URL names (an hour, a day, a month or a year), which is what it
/// changes by; `None` for a URL without dates.
pub fn template_unit(template: &str) -> Option<Unit> {
    if !is_template(template) {
        return None;
    }
    // Placeholders differ between two instants exactly when a unit apart.
    let t = parse_text("2001-02-03T04:00:00Z")?;
    let base = fill_template(template, t);
    Unit::ALL.into_iter().find(|u| fill_template(template, add(t, *u, 1)) != base)
}

/// Whether a URL has date placeholders [`fill_template`] fills.
pub fn is_template(url: &str) -> bool {
    fill_template(url, 0.0) != url
}

/// Reads a date a person typed or a command was given: a text date, a year, or epoch
/// milliseconds.
pub fn parse_input(v: &serde_json::Value) -> Option<f64> {
    let t = match v {
        serde_json::Value::Number(n) => {
            let x = n.as_f64()?;
            // A whole number from 1000 to 9999 is a year, eight digits a compact date (20150301);
            // anything else is milliseconds since 1970.
            if x.fract() == 0.0 && (1000.0..=9999.0).contains(&x) {
                Civil::date(x as i64, 1, 1).ms()
            } else if x.fract() == 0.0 && (10_000_101.0..=99_991_231.0).contains(&x) {
                // Not a valid date: the milliseconds it also reads as.
                parse_text(&format!("{:04}-{:02}-{:02}", (x / 1e4) as i64, (x / 100.0) as i64 % 100, x as i64 % 100)).unwrap_or(x)
            } else {
                x
            }
        }
        // Numbers written plainly, as in a field (`1e3` and `+2015` are no dates).
        serde_json::Value::String(s) => match plain_number(s.trim()) {
            Some(x) => return parse_input(&serde_json::json!(x)),
            None => parse_text(s)?,
        },
        _ => return None,
    };
    (t.is_finite() && t.abs() <= Civil::date(MAX_YEAR, 1, 1).ms()).then_some(t)
}

/// A UTC calendar date and clock time.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Civil {
    year: i64,
    month: i64,
    day: i64,
    hour: i64,
    minute: i64,
    second: f64,
}

impl Civil {
    fn date(year: i64, month: i64, day: i64) -> Civil {
        Civil { year, month, day, hour: 0, minute: 0, second: 0.0 }
    }

    fn of(t: f64) -> Civil {
        let t = t.clamp(-1e17, 1e17);
        let days = (t / DAY_MS).floor();
        let ms = t - days * DAY_MS;
        let (year, month, day) = civil_from_days(days as i64);
        let secs = ms / 1000.0;
        let hour = (secs / 3600.0).floor() as i64;
        let minute = ((secs - hour as f64 * 3600.0) / 60.0).floor() as i64;
        Civil { year, month, day, hour, minute, second: secs - hour as f64 * 3600.0 - minute as f64 * 60.0 }
    }

    fn ms(&self) -> f64 {
        let days = days_from_civil(self.year, self.month, self.day);
        days as f64 * DAY_MS + (self.hour * 3600 + self.minute * 60) as f64 * 1000.0 + self.second * 1000.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> f64 {
        parse_text(s).unwrap_or(f64::NAN)
    }

    #[test]
    fn reads_text_dates() {
        assert_eq!(d("1970-01-01"), 0.0);
        assert_eq!(d("1970-01-02T00:00:00Z"), DAY_MS);
        assert_eq!(d("2015-03-01"), 1_425_168_000_000.0);
        assert_eq!(d("2015/3/1"), d("2015-03-01"));
        assert_eq!(d("2015-03"), d("2015-03-01"));
        assert_eq!(d("2015"), d("2015-01-01"));
        assert_eq!(d("2015-03-01 12:30"), d("2015-03-01") + 12.5 * HOUR_MS);
        assert_eq!(d("2015-03-01T12:00:00+02:00"), d("2015-03-01") + 10.0 * HOUR_MS);
        assert_eq!(d("2015-03-01T12:00:00-0130"), d("2015-03-01") + 13.5 * HOUR_MS);
        assert_eq!(d("2015-03-01T00:00:00.500Z"), d("2015-03-01") + 500.0);
        assert_eq!(d("2015-03-01T24:00"), d("2015-03-02"));
        for bad in [
            "",
            "x",
            "2015-13-01",
            "2015-02-30",
            "2015-02-01T25:00",
            "2015-02-01T24:30",
            "12",
            "2015-01-01-01",
            "Ω",
            "2015-01-01T",
            "99999999999-01-01",
        ] {
            assert!(parse_text(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn detects_kinds() {
        assert_eq!(kind_of(&Value::Int(1_451_070_887_190)), Some(Kind::EpochMs));
        assert_eq!(kind_of(&Value::Int(1_451_070_887)), Some(Kind::EpochS));
        assert_eq!(kind_of(&Value::Int(1958)), Some(Kind::Year));
        assert_eq!(kind_of(&Value::Float(1958.5)), None);
        assert_eq!(kind_of(&Value::Int(42)), None);
        // Numbers past 1900–2100 as epoch times are more likely counts or ids.
        assert_eq!(kind_of(&Value::Int(9_000_000_000)), None);
        assert_eq!(kind_of(&Value::Float(5e14)), None);
        assert_eq!(kind_of(&Value::Text("1958".into())), Some(Kind::Year));
        assert_eq!(kind_of(&Value::Text("1958-03-01".into())), Some(Kind::Text));
        for code in ["1e3", "+2015", "inf", "NaN", "2.015e3"] {
            assert_eq!(kind_of(&Value::Text(code.into())), None, "{code}");
        }
        let vals = [Value::Text("1958".into()), Value::Text("1960-01-01".into()), Value::Null, Value::Text("n/a".into())];
        let (k, share) = detect(vals.iter(), 500).unwrap_or((Kind::EpochMs, 0.0));
        assert_eq!(k, Kind::Text);
        assert!((share - 2.0 / 3.0).abs() < 1e-9, "{share}");
        assert_eq!(instant(&Value::Int(1958), Kind::Year), Some(d("1958-01-01")));
        assert_eq!(instant(&Value::Text("1958".into()), Kind::Text), Some(d("1958-01-01")));
        assert_eq!(instant(&Value::Int(1_425_168_000), Kind::EpochS), Some(d("2015-03-01")));
        assert_eq!(instant(&Value::Bool(true), Kind::Text), None);
        // Outliers in a field of years or epoch times read as no date, not a far-off one.
        for v in [Value::Int(0), Value::Int(20_150_301), Value::Float(1e18)] {
            assert_eq!(instant(&v, Kind::Year), None, "{v:?}");
        }
        assert_eq!(instant(&Value::Float(1e300), Kind::EpochMs), None);
        assert_eq!(instant(&Value::Int(42), Kind::EpochMs), None);
        assert_eq!(instant(&Value::Int(0), Kind::EpochS), None);
    }

    #[test]
    fn date_fields_are_dates() {
        let vals =
            [Value::parse("2015-03-01", crate::FieldType::Date), Value::parse("2015-03-01T12:00:00+02:00", crate::FieldType::DateTime)];
        assert_eq!(detect(vals.iter(), 500), Some((Kind::Text, 1.0)));
        assert_eq!(instant(&vals[0], Kind::Year), Some(d("2015-03-01")));
        assert_eq!(instant(&vals[1], Kind::Text), Some(d("2015-03-01T10:00:00Z")));
    }

    #[test]
    fn steps_by_the_calendar() {
        assert_eq!(add(d("2015-01-31"), Unit::Month, 1), d("2015-02-28"));
        assert_eq!(add(d("2016-02-29"), Unit::Year, 1), d("2017-02-28"));
        assert_eq!(add(d("2015-01-01"), Unit::Month, -1), d("2014-12-01"));
        assert_eq!(floor(d("2015-07-19T13:45:00Z"), Unit::Month), d("2015-07-01"));
        assert_eq!(floor(d("2015-07-19T13:45:00Z"), Unit::Year), d("2015-01-01"));
        assert_eq!(floor(d("-0001-07-19"), Unit::Year), d("-0001-01-01"));
        let s = steps(d("2015-01-01"), d("2015-12-31"), Unit::Month, 1, 1000);
        assert_eq!(s.len(), 12);
        assert_eq!(steps(d("2015-01-01"), d("2015-12-31"), Unit::Month, 3, 1000).len(), 4);
        assert_eq!(steps(0.0, 1e15, Unit::Hour, 1, 50).len(), 50);
        assert!(steps(f64::NAN, 1.0, Unit::Day, 1, 50).is_empty());
        // A step that overflows the calendar stops at its last year instead of wrapping.
        assert_eq!(Civil::of(add(f64::MAX, Unit::Year, i64::MAX)).year, MAX_YEAR);
        assert_eq!(Civil::of(add(0.0, Unit::Year, i64::MIN)).year, -MAX_YEAR);
        assert_eq!(Unit::for_span(365.0 * DAY_MS), Unit::Month);
        assert_eq!(Unit::from_code("Months"), Some(Unit::Month));
        assert_eq!(Unit::from_code("dayss"), None);
        assert_eq!(Unit::from_code("s"), None);
    }

    #[test]
    fn formats_and_fills_templates() {
        let t = d("2023-08-05T09:07:03Z");
        assert_eq!(format(t, "YYYY-MM-DD HH:mm:ss"), "2023-08-05 09:07:03");
        assert_eq!(format(t, "MMM D, YYYY"), "Aug 5, 2023");
        assert_eq!(format(t, "MMMM YY"), "August 23");
        assert_eq!(format(t, "YYYY-MM-DD[T]HH [Day] D"), "2023-08-05T09 Day 5");
        assert_eq!(fill_template("https://x/{date:YYYY-MM-DD}/{z}/{y}/{x}.jpg", t), "https://x/2023-08-05/{z}/{y}/{x}.jpg");
        assert_eq!(fill_template("https://x/{YYYY}{MM}{DD}_{YYYYMMDD}.tif", t), "https://x/20230805_20230805.tif");
        assert_eq!(fill_template("open {brace", t), "open {brace");
        assert!(is_template("a/{date:YYYY}.tif"));
        assert_eq!(template_unit("a/{date:YYYY}.tif"), Some(Unit::Year));
        assert_eq!(template_unit("a/{YYYY}{MM}.tif"), Some(Unit::Month));
        assert_eq!(template_unit("a/{date:YYYY-MM-DD}/{z}/{x}/{y}.png"), Some(Unit::Day));
        assert_eq!(template_unit("a/{date:YYYY-MM-DDTHH}.png"), Some(Unit::Hour));
        assert_eq!(template_unit("a/{z}/{x}/{y}.png"), None);
        assert!(!is_template("a/{z}/{x}/{y}.png"));
        assert_eq!(parse_input(&serde_json::json!(2015)), Some(d("2015-01-01")));
        assert_eq!(parse_input(&serde_json::json!("2015-06")), Some(d("2015-06-01")));
        assert_eq!(parse_input(&serde_json::json!(1_425_168_000_000_i64)), Some(d("2015-03-01")));
        assert_eq!(parse_input(&serde_json::json!(1e300)), None);
        assert_eq!(parse_input(&serde_json::json!("20150301")), Some(d("2015-03-01")));
        assert_eq!(parse_input(&serde_json::json!("1e3")), None);
        assert_eq!(parse_input(&serde_json::json!(" 2015 ")), Some(d("2015-01-01")));
        assert_eq!(parse_input(&serde_json::json!(0)), Some(0.0));
        assert_eq!(parse_input(&serde_json::json!(500)), Some(500.0));
        assert_eq!(parse_input(&serde_json::json!(20_150_301)), Some(d("2015-03-01")));
        assert_eq!(parse_input(&serde_json::json!(20_151_301)), Some(20_151_301.0));
        assert_eq!(parse_input(&serde_json::json!(86_400_000)), Some(DAY_MS));
        assert_eq!(parse_input(&serde_json::json!([1])), None);
    }
}
