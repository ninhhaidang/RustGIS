//! Calendar dates and date-times held in fields: [`Date`] (a day) and [`DateTime`] (a moment).
//!
//! Both are plain integers, so nothing reads the clock and the browser build is unaffected. They
//! cover the years 1 to 9999 (the range every format with dates can hold) and print as ISO 8601.
//! A date-time with a zone is stored as that instant and keeps its offset; one without a zone
//! stays the wall-clock time it says and isn't relabelled as UTC. For comparison, a date is its
//! midnight, and a date-time without a zone reads as if it were UTC.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

const DAY_US: i64 = 86_400_000_000;
const MINUTE_US: i64 = 60_000_000;
/// The days from 1970-01-01 to 0001-01-01 and to 9999-12-31.
const MIN_DAYS: i32 = -719_162;
const MAX_DAYS: i32 = 2_932_896;
/// The largest offset from UTC a date-time may carry, in minutes (±18:00, as ISO 8601 and most
/// databases allow).
const MAX_OFFSET_MIN: i16 = 18 * 60;

/// A calendar day: days since 1970-01-01, from year 1 to 9999.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date(i32);

impl Date {
    /// The day `year-month-day`, if it exists and falls in years 1 to 9999.
    pub fn from_ymd(year: i64, month: i64, day: i64) -> Option<Date> {
        if !(1..=9999).contains(&year) || !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
            return None;
        }
        i32::try_from(days_from_civil(year, month, day)).ok().map(Date)
    }

    /// The day `days` after 1970-01-01, if it falls in years 1 to 9999.
    pub fn from_days(days: i64) -> Option<Date> {
        i32::try_from(days).ok().filter(|d| (MIN_DAYS..=MAX_DAYS).contains(d)).map(Date)
    }

    /// Days since 1970-01-01.
    pub fn days(self) -> i32 {
        self.0
    }

    /// The year, month and day.
    pub fn ymd(self) -> (i64, i64, i64) {
        civil_from_days(i64::from(self.0))
    }

    /// Reads `YYYY-MM-DD`, and nothing else: no time, no zone, no other separators.
    pub fn parse(s: &str) -> Option<Date> {
        let b = s.trim().as_bytes();
        if b.len() != 10 || b.get(4) != Some(&b'-') || b.get(7) != Some(&b'-') {
            return None;
        }
        Date::from_ymd(digits(b.get(0..4)?)?, digits(b.get(5..7)?)?, digits(b.get(8..10)?)?)
    }

    /// Its midnight, as microseconds since 1970 (how dates and date-times compare).
    pub fn sort_key(self) -> i64 {
        i64::from(self.0) * DAY_US
    }
}

impl std::fmt::Display for Date {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (y, m, d) = self.ymd();
        write!(f, "{y:04}-{m:02}-{d:02}")
    }
}

/// A moment: microseconds since 1970-01-01, and the offset from UTC in minutes when it has a
/// zone. With an offset, `micros` is the instant in UTC; without one, it's the wall-clock time
/// as if it were UTC.
///
/// `==` and hashing compare what is stored, offset included, so `12:00Z` and `13:00+01:00` are
/// different values: changing one to the other is an edit that undo and saving must see.
/// Ordering ([`sort_key`](Self::sort_key), and `Value`'s `partial_cmp`) compares the instant,
/// where the two are equal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DateTime {
    micros: i64,
    offset: Option<i16>,
}

impl DateTime {
    /// A wall-clock time without a zone, in years 1 to 9999.
    pub fn local(micros: i64) -> Option<DateTime> {
        in_range(micros).then_some(DateTime { micros, offset: None })
    }

    /// The instant `micros` (UTC), shown at `offset_min` minutes from UTC (`0` is `Z`).
    pub fn with_offset(micros: i64, offset_min: i16) -> Option<DateTime> {
        let shown = micros.checked_add(i64::from(offset_min) * MINUTE_US)?;
        (offset_min.unsigned_abs() <= MAX_OFFSET_MIN.unsigned_abs() && in_range(micros) && in_range(shown))
            .then_some(DateTime { micros, offset: Some(offset_min) })
    }

    /// Microseconds since 1970: the UTC instant with a zone, the wall-clock time without one.
    pub fn micros(self) -> i64 {
        self.micros
    }

    /// The offset from UTC in minutes, when it has a zone.
    pub fn offset(self) -> Option<i16> {
        self.offset
    }

    /// The midnight starting a day, without a zone.
    pub fn from_date(d: Date) -> DateTime {
        DateTime { micros: d.sort_key(), offset: None }
    }

    /// Reads `YYYY-MM-DD`, then optionally `T` or a space and `HH:MM[:SS[.ffffff]]`, then
    /// optionally `Z` or an offset (`+05:30`, `-0800`, `+02`). A date alone is its midnight.
    /// Digits past microseconds are refused unless they're zeros, so nothing is cut; a reader
    /// that says how many it rounded uses [`parse_rounding`](Self::parse_rounding).
    pub fn parse(s: &str) -> Option<DateTime> {
        DateTime::parse_rounding(s).filter(|(_, rounded)| !rounded).map(|(t, _)| t)
    }

    /// [`parse`](Self::parse), but digits past microseconds round to the nearest microsecond (halves
    /// up, cut instead at the end of year 9999, where rounding up would leave the range). The
    /// boolean says that nonzero digits were lost, for the reader to count.
    pub fn parse_rounding(s: &str) -> Option<(DateTime, bool)> {
        let s = s.trim();
        let date = Date::parse(s.get(..10)?)?;
        let rest = s.get(10..)?;
        if rest.is_empty() {
            return Some((DateTime::from_date(date), false));
        }
        let rest = rest.strip_prefix(['T', 't', ' '])?;
        // The zone starts at `Z` or at a sign after the clock.
        let (clock, zone) = match rest.find(['Z', 'z', '+', '-']) {
            Some(i) => (rest.get(..i)?, Some(rest.get(i..)?)),
            None => (rest, None),
        };
        let b = clock.as_bytes();
        if b.len() < 5 || b.get(2) != Some(&b':') {
            return None;
        }
        let (h, m) = (digits(b.get(0..2)?)?, digits(b.get(3..5)?)?);
        let mut us = 0i64;
        let mut sec = 0i64;
        // The first digit past the microsecond, and whether any of them isn't zero.
        let (mut next, mut cut) = (0u8, false);
        if b.len() > 5 {
            if b.get(5) != Some(&b':') || b.len() < 8 {
                return None;
            }
            sec = digits(b.get(6..8)?)?;
            if b.len() > 8 {
                let frac = b.get(8..)?.strip_prefix(b".")?;
                if frac.is_empty() || !frac.iter().all(u8::is_ascii_digit) {
                    return None;
                }
                for i in 0..6 {
                    us = us * 10 + frac.get(i).map(|c| i64::from(c - b'0')).unwrap_or(0);
                }
                next = frac.get(6).map_or(0, |c| c - b'0');
                cut = frac.iter().skip(6).any(|&c| c != b'0');
            }
        }
        if h > 23 || m > 59 || sec > 59 {
            return None;
        }
        let wall = date.sort_key() + ((h * 60 + m) * 60 + sec) * 1_000_000 + us;
        let at = |wall: i64| -> Option<DateTime> {
            match zone {
                None => DateTime::local(wall),
                Some(z) if z.eq_ignore_ascii_case("z") => DateTime::with_offset(wall, 0),
                // `Z` followed by more (`Z0200`) is no zone: refused, not read as an offset.
                Some(z) if !z.starts_with(['+', '-']) => None,
                Some(z) => {
                    let sign = if z.starts_with('-') { -1 } else { 1 };
                    let z = z.get(1..)?.as_bytes();
                    let (hh, mm) = match z.len() {
                        2 => (digits(z)?, 0),
                        4 => (digits(z.get(0..2)?)?, digits(z.get(2..4)?)?),
                        5 if z.get(2) == Some(&b':') => (digits(z.get(0..2)?)?, digits(z.get(3..5)?)?),
                        _ => return None,
                    };
                    if mm > 59 {
                        return None;
                    }
                    let off = i16::try_from(sign * (hh * 60 + mm)).ok()?;
                    DateTime::with_offset(wall.checked_sub(i64::from(off) * MINUTE_US)?, off)
                }
            }
        };
        // Rounded up when it fits; cut when that would leave the range.
        let rounded = if next >= 5 { at(wall + 1).or_else(|| at(wall)) } else { at(wall) };
        rounded.map(|t| (t, cut))
    }

    /// Whether its day says all of it: midnight, without a zone. Otherwise [`date`](Self::date)
    /// drops its time or its zone.
    pub fn is_date(self) -> bool {
        self == DateTime::from_date(self.date())
    }

    /// The day it falls on, as it's shown (in its own offset).
    pub fn date(self) -> Date {
        Date(self.shown().div_euclid(DAY_US) as i32)
    }

    /// How it compares with dates and other date-times: the instant, with a time without a
    /// zone read as UTC.
    pub fn sort_key(self) -> i64 {
        self.micros
    }

    /// The wall-clock time it shows, as microseconds since 1970.
    fn shown(self) -> i64 {
        self.micros + i64::from(self.offset.unwrap_or(0)) * MINUTE_US
    }
}

impl std::fmt::Display for DateTime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let shown = self.shown();
        let us = shown.rem_euclid(DAY_US);
        let secs = us / 1_000_000;
        write!(f, "{}T{:02}:{:02}:{:02}", self.date(), secs / 3600, secs / 60 % 60, secs % 60)?;
        let frac = us % 1_000_000;
        if frac % 1000 == 0 && frac != 0 {
            write!(f, ".{:03}", frac / 1000)?;
        } else if frac != 0 {
            write!(f, ".{frac:06}")?;
        }
        match self.offset {
            None => Ok(()),
            Some(0) => write!(f, "Z"),
            Some(o) => write!(f, "{}{:02}:{:02}", if o < 0 { '-' } else { '+' }, o.unsigned_abs() / 60, o.unsigned_abs() % 60),
        }
    }
}

impl Serialize for Date {
    /// As its ISO 8601 text (`2026-03-05`), the form a project file holds.
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Date {
    /// From its ISO 8601 text; a day that doesn't exist (`2026-02-30`) or lies outside years 1 to
    /// 9999 is an error naming it, so a damaged project is refused rather than read as null.
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Date::parse(&s).ok_or_else(|| serde::de::Error::custom(format!("'{s}' isn't a date (YYYY-MM-DD, years 1 to 9999)")))
    }
}

/// A date is saved as its ISO 8601 text (`2026-03-05`), from year 1 to 9999.
#[cfg(feature = "schema")]
impl schemars::JsonSchema for Date {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Date".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "A calendar day as ISO 8601 text (YYYY-MM-DD), years 1 to 9999, maybe with spaces around it. The pattern can't tell a day that doesn't exist (2026-02-30); the reader refuses it.",
            "pattern": "^\\s*([1-9][0-9]{3}|0[1-9][0-9]{2}|00[1-9][0-9]|000[1-9])-(0[1-9]|1[0-2])-(0[1-9]|[12][0-9]|3[01])\\s*$"
        })
    }
}

/// A date-time is saved as its ISO 8601 text, with its offset when it has one.
#[cfg(feature = "schema")]
impl schemars::JsonSchema for DateTime {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "DateTime".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        // The day as for `Date`, then maybe a clock after `T`, `t` or a space (HH:MM, seconds and
        // up to six fraction digits optional, zeros past them), then maybe `Z` or an offset
        // (+02, +0200, +02:00) of at most 18 hours, as `MAX_OFFSET_MIN`.
        schemars::json_schema!({
            "type": "string",
            "description": "A date and time as ISO 8601 text, years 1 to 9999, with its offset (Z, +02:00) when it has one, without one when it has none, maybe with spaces around it. The pattern can't tell a day that doesn't exist (2026-02-30) or an instant before year 1 or past year 9999 once its offset is applied (0001-01-01T00:00:00+01:00); the reader refuses them.",
            "pattern": "^\\s*([1-9][0-9]{3}|0[1-9][0-9]{2}|00[1-9][0-9]|000[1-9])-(0[1-9]|1[0-2])-(0[1-9]|[12][0-9]|3[01])([Tt ]([01][0-9]|2[0-3]):[0-5][0-9](:[0-5][0-9](\\.[0-9]{1,6}0*)?)?([Zz]|[+-]((0[0-9]|1[0-7])(:?[0-5][0-9])?|18(:?00)?))?)?\\s*$"
        })
    }
}

impl Serialize for DateTime {
    /// As its ISO 8601 text, with its offset when it has one (`2026-03-05T14:30:00+02:00`).
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for DateTime {
    /// From its ISO 8601 text; one that isn't a valid date-time is an error naming it, as for
    /// [`Date`].
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        DateTime::parse(&s).ok_or_else(|| serde::de::Error::custom(format!("'{s}' isn't a date-time (ISO 8601, years 1 to 9999)")))
    }
}

/// A binary floating-point day count from an epoch, rounded to microseconds. Split whole
/// days before scaling: scaling an absolute date as f64 loses low time bits in distant years.
/// The boolean says that fractional microseconds were rounded, for the reader's notice.
pub(crate) fn serial_datetime(days: f64, epoch: i64) -> Option<(DateTime, bool)> {
    if !days.is_finite() || days.abs() > 4_000_000.0 {
        return None;
    }
    let whole = days.floor() as i64;
    let day = Date::from_days(whole.checked_add(epoch)?)?;
    let fraction = (days - whole as f64) * DAY_US as f64;
    let micros = day.sort_key().checked_add(fraction.round() as i64)?;
    DateTime::local(micros).map(|t| (t, fraction.fract() != 0.0))
}

/// Whether microseconds since 1970 fall in years 1 to 9999.
fn in_range(micros: i64) -> bool {
    (i64::from(MIN_DAYS) * DAY_US..(i64::from(MAX_DAYS) + 1) * DAY_US).contains(&micros)
}

/// A run of ASCII digits as a number.
fn digits(b: &[u8]) -> Option<i64> {
    (!b.is_empty() && b.len() <= 9 && b.iter().all(u8::is_ascii_digit)).then(|| b.iter().fold(0, |n, c| n * 10 + i64::from(c - b'0')))
}

/// Whether `y` is a leap year of the proleptic Gregorian calendar.
fn leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// The days in a month of the proleptic Gregorian calendar.
pub(crate) fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        2 if leap(y) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's `days_from_civil`,
/// <https://howardhinnant.github.io/date_algorithms.html>).
pub(crate) fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The proleptic Gregorian date `days` after 1970-01-01 (Hinnant's `civil_from_days`).
pub(crate) fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A date-time from ISO text; one that doesn't read is far outside every range.
    fn dt(s: &str) -> DateTime {
        DateTime::parse(s).unwrap_or(DateTime { micros: i64::MIN, offset: None })
    }

    #[test]
    fn digits_past_microseconds_round_only_where_asked_and_are_refused_otherwise() {
        let at = |s: &str| DateTime::parse_rounding(s).map(|(t, rounded)| (t.to_string(), rounded));
        let said = |s: &str, t: &str| assert_eq!(at(s), Some((t.to_string(), true)), "{s}");
        said("2026-03-05T14:30:00.123456789Z", "2026-03-05T14:30:00.123457Z");
        said("2026-03-05T14:30:00.1234564Z", "2026-03-05T14:30:00.123456Z");
        said("2026-03-05T14:30:00.1234565+02:00", "2026-03-05T14:30:00.123457+02:00");
        said("2026-03-05 14:30:00.0000005", "2026-03-05T14:30:00.000001");
        said("2026-03-05T14:30:59.9999996", "2026-03-05T14:31:00");
        said("2026-03-05T00:00:00.0000001", "2026-03-05T00:00:00");
        said("1969-12-31T23:59:59.9999995Z", "1970-01-01T00:00:00Z");
        // Zeros past the microsecond, or none, lose nothing.
        assert_eq!(at("2026-03-05T14:30:00.123456000Z"), Some(("2026-03-05T14:30:00.123456Z".into(), false)));
        assert_eq!(at("2026-03-05T14:30:00.123Z"), Some(("2026-03-05T14:30:00.123Z".into(), false)));
        // Rounding up past the last microsecond of year 9999 (in UTC, or as shown) cuts instead.
        said("9999-12-31T23:59:59.9999995", "9999-12-31T23:59:59.999999");
        said("9999-12-31T23:59:59.9999995Z", "9999-12-31T23:59:59.999999Z");
        said("9999-12-31T23:59:59.9999995+02:00", "9999-12-31T23:59:59.999999+02:00");
        // `parse` itself refuses what it would round.
        assert!(DateTime::parse("2026-03-05T14:30:00.123456789Z").is_none(), "nothing is cut");
        assert!(DateTime::parse("2026-03-05T14:30:00.0000001").is_none());
        assert!(DateTime::parse("9999-12-31T23:59:59.9999995").is_none());
        assert!(DateTime::parse("2026-03-05T14:30:00.Z").is_none());
        assert!(DateTime::parse("2026-03-05T14:30:00.12x").is_none());
        assert!(DateTime::parse("2026-03-05T14:30:00Z0200").is_none(), "Z then an offset is no zone");
        assert_eq!(
            DateTime::parse("2026-03-05T14:30:00.123456000Z").map(|t| t.to_string()),
            DateTime::parse("2026-03-05T14:30:00.123456Z").map(|t| t.to_string())
        );
        assert!(DateTime::parse("2026-03-05T00:00:00").is_some_and(DateTime::is_date));
        assert!(!DateTime::parse("2026-03-05T00:00:00Z").is_some_and(DateTime::is_date), "a zone isn't kept by a date");
        assert!(!DateTime::parse("2026-03-05T14:30:00").is_some_and(DateTime::is_date));
    }

    #[test]
    fn dates_read_and_print_iso() {
        for s in ["1970-01-01", "2026-03-05", "2024-02-29", "0001-01-01", "9999-12-31"] {
            assert_eq!(Date::parse(s).map(|d| d.to_string()).as_deref(), Some(s));
        }
        assert_eq!(Date::parse("1970-01-02").map(Date::days), Some(1));
        assert_eq!(Date::parse("1969-12-31").map(Date::days), Some(-1));
        for bad in [
            "",
            "2026",
            "2026-03",
            "2026-3-5",
            "2026/03/05",
            "2026-02-30",
            "2025-02-29",
            "0000-01-01",
            "2026-13-01",
            "2026-03-05T00:00",
            "١٩٧٠-٠١-٠١",
        ] {
            assert_eq!(Date::parse(bad), None, "{bad}");
        }
        assert_eq!(Date::from_days(i64::MAX), None);
        assert_eq!(Date::from_days(i64::from(MAX_DAYS) + 1), None);
    }

    #[test]
    fn date_times_keep_their_zone_or_its_absence() {
        assert_eq!(dt("2026-03-05T14:30:00").to_string(), "2026-03-05T14:30:00");
        assert_eq!(dt("2026-03-05 14:30").to_string(), "2026-03-05T14:30:00");
        assert_eq!(dt("2026-03-05").to_string(), "2026-03-05T00:00:00");
        assert_eq!(dt("2026-03-05T14:30:00z").to_string(), "2026-03-05T14:30:00Z");
        assert_eq!(dt("2026-03-05T14:30:00+0200").to_string(), "2026-03-05T14:30:00+02:00");
        assert_eq!(dt("2026-03-05T14:30:00-03").to_string(), "2026-03-05T14:30:00-03:00");
        assert_eq!(dt("2026-03-05T14:30:00.5Z").to_string(), "2026-03-05T14:30:00.500Z");
        assert_eq!(dt("2026-03-05T14:30:00.1234560").to_string(), "2026-03-05T14:30:00.123456");
        // The instant, compared across offsets; a time without a zone compares as UTC.
        assert_eq!(dt("2026-03-05T14:30:00+02:00").sort_key(), dt("2026-03-05T12:30:00Z").sort_key());
        assert_eq!(dt("2026-03-05T12:30:00").sort_key(), dt("2026-03-05T12:30:00Z").sort_key());
        assert_eq!(dt("2026-03-05T23:30:00-05:00").date().to_string(), "2026-03-05");
        assert!(DateTime::parse("0001-01-01T00:30:00+01:00").is_none(), "before year 1 in UTC");
        assert_eq!(dt("9999-12-31T23:59:59.999999").date().to_string(), "9999-12-31");
        for bad in [
            "2026-03-05T",
            "2026-03-05T24:00",
            "2026-03-05T12",
            "2026-03-05T12:60",
            "2026-03-05T12:00:60",
            "2026-03-05T12:00+19:00",
            "2026-03-05T12:00+02:5",
            "2026-03-05T12:00:00.",
            "2026-03-05X12:00",
        ] {
            assert_eq!(DateTime::parse(bad), None, "{bad}");
        }
        // An offset past ±18 hours is refused, the most negative one too.
        assert_eq!(DateTime::with_offset(0, i16::MIN), None);
        assert_eq!(DateTime::with_offset(0, i16::MAX), None);
    }

    #[test]
    fn binary_serial_dates_keep_fractional_precision_in_distant_years() {
        // The next representable fractional day adds about 40.233 microseconds here. Scaling
        // the whole date as f64 first would quantize the clock to 32-microsecond steps.
        let serial = 2_958_465.25 + 2f64.powi(-31);
        let (t, rounded) = serial_datetime(serial, -25_569).unwrap();
        assert_eq!(t.to_string(), "9999-12-31T06:00:00.000040");
        assert!(rounded);
        assert_eq!(serial_datetime(25_569.5, -25_569).unwrap(), (dt("1970-01-01T12:00:00"), false));
        for bad in [f64::NAN, f64::INFINITY, -1e100, 2_958_466.0] {
            assert!(serial_datetime(bad, -25_569).is_none());
        }
    }

    #[test]
    fn civil_days_round_trip() {
        for days in [MIN_DAYS, -1, 0, 59, 11_016, 20_517, MAX_DAYS] {
            let (y, m, d) = civil_from_days(i64::from(days));
            assert_eq!(days_from_civil(y, m, d), i64::from(days));
        }
    }
}
