//! Explicit numeric date formats for field conversion. No locale or date order is inferred.

use crate::{Date, DateTime, FieldType, Value};

/// A validated reader, shared by the command and the table's format previews.
#[derive(Clone, Debug)]
pub struct DateFormat {
    format: String,
    pivot: Option<i64>,
}

impl DateFormat {
    /// Validates a pattern once. `pivot` is the first year of the 100-year window for `%y`.
    /// Numeric codes are deliberately limited; month names require a data language.
    pub fn new(format: &str, pivot: Option<i64>) -> Result<Self, String> {
        if format.len() > 128 {
            return Err("date format is too long (maximum 128 bytes)".into());
        }
        if matches!(format, "iso" | "excel1900" | "excel1904") {
            if pivot.is_some() {
                return Err("pivotYear requires a pattern containing %y".into());
            }
            return Ok(Self { format: format.into(), pivot });
        }
        let mut codes = Vec::new();
        let mut previous_variable = false;
        let mut chars = format.chars();
        while let Some(c) = chars.next() {
            if c != '%' {
                previous_variable = false;
                continue;
            }
            let mut code = chars.next().ok_or("unfinished % in date format")?;
            let unpadded = code == '-';
            if unpadded {
                code = chars.next().ok_or("unfinished %- in date format")?;
                if !matches!(code, 'm' | 'd' | 'H') {
                    return Err("%- is supported only for month (%-m), day (%-d) and 24-hour clock (%-H)".into());
                }
            }
            if code == '%' {
                previous_variable = false;
                continue;
            }
            if !matches!(code, 'Y' | 'y' | 'm' | 'd' | 'H' | 'I' | 'p' | 'M' | 'S' | 'f' | 'z') {
                return Err(format!(
                    "unsupported date format code %{code}: use %Y, %y, %m, %d, %H, %-m, %-d, %-H, %I, %p, %M, %S, %f, %z or %%"
                ));
            }
            if previous_variable && matches!(code, 'Y' | 'y' | 'm' | 'd' | 'H' | 'I' | 'M' | 'S' | 'f') {
                return Err("put a separator between a variable-width date code and the following digit code".into());
            }
            previous_variable = unpadded || code == 'I';
            if codes.contains(&code) {
                return Err(format!("date format repeats %{code}"));
            }
            codes.push(code);
        }
        if codes.contains(&'Y') == codes.contains(&'y') || !codes.contains(&'m') || !codes.contains(&'d') {
            return Err("date format needs one year (%Y or %y), month (%m) and day (%d)".into());
        }
        if codes.contains(&'y') {
            if !pivot.is_some_and(|p| (1..=9900).contains(&p)) {
                return Err("%y requires pivotYear (1 to 9900), the first year of its 100-year window".into());
            }
        } else if pivot.is_some() {
            return Err("pivotYear requires a pattern containing %y".into());
        }
        let clock = codes.contains(&'H') || codes.contains(&'I');
        if codes.contains(&'I') != codes.contains(&'p') || (codes.contains(&'I') && codes.contains(&'H')) {
            return Err("12-hour time needs both %I and %p, without %H or %-H".into());
        }
        if clock != codes.contains(&'M')
            || (codes.contains(&'S') && !clock)
            || (codes.contains(&'f') && !codes.contains(&'S'))
            || (codes.contains(&'z') && !clock)
        {
            return Err("time needs an hour (%H, %-H or %I with %p) and %M; %S needs a time, %f needs %S, and %z needs a time".into());
        }
        Ok(Self { format: format.into(), pivot })
    }

    /// The chosen reader, included in a refusal so the caller knows what was attempted.
    pub fn description(&self) -> &str {
        if self.format == "iso" { "ISO 8601, like 2026-03-05" } else { &self.format }
    }

    /// Reads one value without discarding a time or zone for a date field. The boolean reports
    /// microsecond rounding. A mismatch is null; callers count and report it before committing.
    pub fn parse(&self, text: &str, ty: FieldType) -> (Value, bool) {
        let parsed = self.datetime(text.trim());
        match parsed {
            Some((t, rounded)) if ty == FieldType::DateTime => (Value::DateTime(t), rounded),
            Some((t, rounded)) if ty == FieldType::Date && !rounded && t.is_date() => (Value::Date(t.date()), rounded),
            _ => (Value::Null, false),
        }
    }

    /// Normalizes an explicitly matched pattern through the existing ISO reader, retaining its
    /// range, offset and rounding rules. Input and pattern are consumed completely.
    fn datetime(&self, text: &str) -> Option<(DateTime, bool)> {
        match self.format.as_str() {
            "iso" => return DateTime::parse_rounding(text),
            "excel1900" | "excel1904" => {
                let (whole, micros, rounded) = decimal_days(text)?;
                let epoch = if self.format == "excel1904" {
                    i64::from(Date::from_ymd(1904, 1, 1)?.days())
                } else {
                    if whole == 60 {
                        return None;
                    }
                    -25_569 + i64::from(whole < 60)
                };
                return DateTime::local(epoch.checked_mul(DAY_US)?.checked_add(micros)?).map(|t| (t, rounded));
            }
            _ => {}
        }
        let (mut year, mut month, mut day) = (0, 0, 0);
        let (mut hour, mut minute, mut second) = (0, 0, 0);
        let (mut fraction, mut zone) = ("", "");
        let mut pm = None;
        let mut rest = text;
        let mut pattern = self.format.chars();
        while let Some(c) = pattern.next() {
            if c != '%' {
                rest = rest.strip_prefix(c)?;
                continue;
            }
            let mut code = pattern.next()?;
            let unpadded = code == '-';
            if unpadded {
                code = pattern.next()?;
            }
            if code == '%' {
                rest = rest.strip_prefix('%')?;
                continue;
            }
            if code == 'p' {
                let marker = rest.get(..2)?;
                pm = Some(if marker.eq_ignore_ascii_case("am") {
                    false
                } else if marker.eq_ignore_ascii_case("pm") {
                    true
                } else {
                    return None;
                });
                rest = rest.get(2..)?;
                continue;
            }
            if code == 'f' {
                let len = rest.bytes().take_while(u8::is_ascii_digit).count();
                if len == 0 {
                    return None;
                }
                fraction = rest.get(..len)?;
                rest = rest.get(len..)?;
                continue;
            }
            if code == 'z' {
                let len = if rest.starts_with(['Z', 'z']) {
                    1
                } else if rest.as_bytes().get(3) == Some(&b':') {
                    6
                } else {
                    5
                };
                zone = rest.get(..len)?;
                if !zone.starts_with(['Z', 'z', '+', '-']) {
                    return None;
                }
                rest = rest.get(len..)?;
                continue;
            }
            let len = if unpadded || code == 'I' {
                rest.bytes().take(2).take_while(u8::is_ascii_digit).count()
            } else if code == 'Y' {
                4
            } else {
                2
            };
            let digits = rest.get(..len)?;
            if !digits.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let n: i64 = digits.parse().ok()?;
            rest = rest.get(len..)?;
            match code {
                'Y' => year = n,
                'y' => {
                    let pivot = self.pivot?;
                    year = pivot + (n - pivot.rem_euclid(100)).rem_euclid(100);
                }
                'm' => month = n,
                'd' => day = n,
                'H' | 'I' => hour = n,
                'M' => minute = n,
                'S' => second = n,
                _ => return None,
            }
        }
        if !rest.is_empty() {
            return None;
        }
        if let Some(pm) = pm {
            if !(1..=12).contains(&hour) {
                return None;
            }
            hour = hour % 12 + if pm { 12 } else { 0 };
        }
        let date = Date::from_ymd(year, month, day)?;
        let frac = if fraction.is_empty() { String::new() } else { format!(".{fraction}") };
        DateTime::parse_rounding(&format!("{date}T{hour:02}:{minute:02}:{second:02}{frac}{zone}"))
    }
}

const DAY_US: i64 = 86_400_000_000;

/// Multiply decimal digits exactly before rounding to microseconds. Storage grows only with
/// the input, never with its exponent. Negative half-microseconds round toward later time,
/// matching the existing serial-date reader. The original floor identifies the fictitious day 60.
fn decimal_days(text: &str) -> Option<(i64, i64, bool)> {
    let (mantissa, exponent) = match text.split_once(['e', 'E']) {
        Some((m, e)) => (m, e.parse::<i64>().ok()?),
        None => (text, 0),
    };
    let negative = mantissa.starts_with('-');
    let mantissa = mantissa.strip_prefix(['-', '+']).unwrap_or(mantissa);
    let mut dot = false;
    let mut decimals = 0i128;
    let mut digits = Vec::new();
    for b in mantissa.bytes() {
        if b == b'.' && !dot {
            dot = true;
            continue;
        }
        if !b.is_ascii_digit() {
            return None;
        }
        decimals += i128::from(dot);
        digits.push(b - b'0');
    }
    if digits.is_empty() {
        return None;
    }
    digits.reverse();
    let mut carry = 0i64;
    for digit in &mut digits {
        let product = i64::from(*digit) * DAY_US + carry;
        *digit = (product % 10) as u8;
        carry = product / 10;
    }
    while carry > 0 {
        digits.push((carry % 10) as u8);
        carry /= 10;
    }
    while digits.last() == Some(&0) {
        digits.pop();
    }
    if digits.is_empty() {
        return Some((0, 0, false));
    }
    let scale = decimals - i128::from(exponent);
    // More than 18 integer digits cannot fit a supported date, including negative dates.
    if i128::try_from(digits.len()).ok()? - scale > 18 {
        return None;
    }
    let split = usize::try_from(scale.max(0)).unwrap_or(usize::MAX).min(digits.len());
    let mut micros = 0i64;
    for &digit in digits.get(split..)?.iter().rev() {
        micros = micros.checked_mul(10)?.checked_add(i64::from(digit))?;
    }
    if scale < 0 {
        micros = micros.checked_mul(10i64.checked_pow(u32::try_from(-scale).ok()?)?)?;
    }
    let remainder = digits.get(..split)?;
    let rounded = remainder.iter().any(|&d| d != 0);
    let half = if scale > i128::try_from(digits.len()).ok()? { 0 } else { remainder.last().copied().unwrap_or(0) };
    let above_half = half > 5 || (half == 5 && remainder.iter().rev().skip(1).any(|&d| d != 0));
    let whole = if negative { -micros / DAY_US - i64::from(micros % DAY_US != 0 || rounded) } else { micros / DAY_US };
    let up = above_half || (!negative && half == 5);
    micros = micros.checked_add(i64::from(up))?;
    Some((whole, if negative { -micros } else { micros }, rounded))
}

/// Dialog presets: human-readable labels and command patterns. Both date orders stay choices.
pub const PRESETS: &[(&str, &str)] = &[
    ("ISO 8601", "iso"),
    ("MM/DD/YYYY", "%m/%d/%Y"),
    ("DD/MM/YYYY", "%d/%m/%Y"),
    ("YYYY-MM-DD", "%Y-%m-%d"),
    ("YYYYMMDD", "%Y%m%d"),
    ("DD.MM.YYYY", "%d.%m.%Y"),
    ("MM/DD/YYYY HH:MM", "%m/%d/%Y %H:%M"),
    ("DD/MM/YYYY HH:MM", "%d/%m/%Y %H:%M"),
    ("YYYY-MM-DD HH:MM", "%Y-%m-%d %H:%M"),
    ("YYYYMMDD HH:MM", "%Y%m%d %H:%M"),
    ("DD.MM.YYYY HH:MM", "%d.%m.%Y %H:%M"),
    ("MM/DD/YYYY HH:MM:SS", "%m/%d/%Y %H:%M:%S"),
    ("DD/MM/YYYY HH:MM:SS", "%d/%m/%Y %H:%M:%S"),
    ("YYYY-MM-DD HH:MM:SS", "%Y-%m-%d %H:%M:%S"),
    ("YYYYMMDD HH:MM:SS", "%Y%m%d %H:%M:%S"),
    ("DD.MM.YYYY HH:MM:SS", "%d.%m.%Y %H:%M:%S"),
    ("M/D/YYYY h:MM:SS AM/PM", "%-m/%-d/%Y %I:%M:%S %p"),
    ("MM/DD/YY", "%m/%d/%y"),
    ("DD/MM/YY", "%d/%m/%y"),
    ("YY-MM-DD", "%y-%m-%d"),
    ("YYMMDD", "%y%m%d"),
    ("DD.MM.YY", "%d.%m.%y"),
    ("MM/DD/YY HH:MM", "%m/%d/%y %H:%M"),
    ("DD/MM/YY HH:MM", "%d/%m/%y %H:%M"),
    ("YY-MM-DD HH:MM", "%y-%m-%d %H:%M"),
    ("YYMMDD HH:MM", "%y%m%d %H:%M"),
    ("DD.MM.YY HH:MM", "%d.%m.%y %H:%M"),
    ("MM/DD/YY HH:MM:SS", "%m/%d/%y %H:%M:%S"),
    ("DD/MM/YY HH:MM:SS", "%d/%m/%y %H:%M:%S"),
    ("YY-MM-DD HH:MM:SS", "%y-%m-%d %H:%M:%S"),
    ("YYMMDD HH:MM:SS", "%y%m%d %H:%M:%S"),
    ("DD.MM.YY HH:MM:SS", "%d.%m.%y %H:%M:%S"),
    ("Spreadsheet days (1900)", "excel1900"),
    ("Spreadsheet days (1904)", "excel1904"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_patterns_preserve_order_and_require_a_century() {
        let read = |p, s| DateFormat::new(p, None).unwrap().parse(s, FieldType::Date).0.to_string();
        assert_eq!(read("%m/%d/%Y", "03/05/2026"), "2026-03-05");
        assert_eq!(read("%d/%m/%Y", "03/05/2026"), "2026-05-03");
        assert_eq!(read("%Y%m%d", "20260305"), "2026-03-05");
        assert_eq!(read("%d.%m.%Y", "05.03.2026"), "2026-03-05");
        assert_eq!(read("%m%d%Y", "01232021"), "2021-01-23");
        assert!(DateFormat::new("%m/%d/%Y", None).unwrap().parse("6/3/2021", FieldType::Date).0.is_null());
        for bad in ["%m/%d/%y", "%Y-%m", "%Y-%m-%d %Q", "%Y-%m-%d %S", "%Y%y%m%d", "%Y%m%d%", "%Y%m%d%m"] {
            assert!(DateFormat::new(bad, None).is_err(), "{bad}");
        }
        let f = DateFormat::new("%d/%m/%y", Some(1926)).unwrap();
        assert_eq!(f.parse("05/03/26", FieldType::Date).0.to_string(), "1926-03-05");
        assert_eq!(f.parse("05/03/25", FieldType::Date).0.to_string(), "2025-03-05");
        assert!(DateFormat::new("%y%m%d", Some(i64::MAX)).is_err());
        assert!(DateFormat::new("iso", Some(1926)).is_err());
        for s in ["20260229", "202603050", "2026年03月05", "00000305"] {
            assert!(DateFormat::new("%Y%m%d", None).unwrap().parse(s, FieldType::Date).0.is_null());
        }
    }

    #[test]
    fn unpadded_dates_and_twelve_hour_clocks_are_explicit() {
        for bad in ["%-m%-d-%Y", "%-m%d%Y", "%-d%Y-%m", "%m/%d/%Y %-H%M", "%m/%d/%Y %I%M %p", "%m/%d/%Y %I%f:%M:%S %p"] {
            assert!(DateFormat::new(bad, None).unwrap_err().contains("separator"), "{bad}");
        }
        let f = DateFormat::new("%-m/%d-%Y", None).unwrap();
        assert_eq!(f.parse("1/12-2021", FieldType::Date).0.to_string(), "2021-01-12");
        let pattern = "%-m/%-d/%Y %I:%M:%S %p";
        assert!(PRESETS.iter().any(|(_, p)| *p == pattern));
        let f = DateFormat::new(pattern, None).unwrap();
        for (text, expected) in [
            ("6/23/2021 5:00:00 AM", "2021-06-23T05:00:00"),
            ("6/3/2021 12:30:00 aM", "2021-06-03T00:30:00"),
            ("12/03/2021 12:00:00 PM", "2021-12-03T12:00:00"),
            ("06/23/2021 05:00:00 pm", "2021-06-23T17:00:00"),
        ] {
            assert_eq!(f.parse(text, FieldType::DateTime), (Value::DateTime(DateTime::parse(expected).unwrap()), false));
        }
        for bad in [
            "6/23/2021 13:00:00 PM",
            "6/23/2021 0:00:00 AM",
            "6/23/2021 5:60:00 AM",
            "6/23/2021 5:00:60 AM",
            "6/23/2021 5:00:00 XM",
            "6/23/2021 5:00:00 AM trailing",
            "13/23/2021 5:00:00 AM",
            "6/31/2021 5:00:00 AM",
            "006/23/2021 5:00:00 AM",
        ] {
            assert_eq!(f.parse(bad, FieldType::DateTime), (Value::Null, false), "{bad}");
        }
        assert!(f.parse("6/23/2021 5:00:00 AM", FieldType::Date).0.is_null());
        assert_eq!(f.parse("6/23/2021 12:00:00 AM", FieldType::Date).0.to_string(), "2021-06-23");
        let f = DateFormat::new("%-m/%-d/%Y %-H:%M:%S", None).unwrap();
        assert_eq!(f.parse("6/3/2021 5:00:00", FieldType::DateTime).0.to_string(), "2021-06-03T05:00:00");
        assert_eq!(f.parse("06/03/2021 23:00:00", FieldType::DateTime).0.to_string(), "2021-06-03T23:00:00");
        assert!(f.parse("6/3/2021 24:00:00", FieldType::DateTime).0.is_null());
        let f = DateFormat::new("%m/%d/%Y %H:%M:%S", None).unwrap();
        assert_eq!(f.parse("06/23/2021 05:00:00", FieldType::DateTime).0.to_string(), "2021-06-23T05:00:00");
        assert!(f.parse("06/23/2021 5:00:00", FieldType::DateTime).0.is_null());
        for bad in [
            "%-m/%m/%-d/%Y",
            "%m/%-d/%d/%Y",
            "%m/%d/%Y %H:%-H:%M",
            "%m/%d/%Y %I:%M",
            "%m/%d/%Y %H:%M %p",
            "%m/%d/%Y %I:%M %p %H",
            "%m/%d/%Y %-M",
            "%m/%d/%Y %-",
        ] {
            assert!(DateFormat::new(bad, None).is_err(), "{bad}");
        }
    }

    #[test]
    fn times_and_spreadsheet_days_keep_precision_and_refuse_data_loss() {
        for format in ["iso", "%Y-%m-%dT%H:%M:%S.%f"] {
            let reader = DateFormat::new(format, None).unwrap();
            for text in ["2026-03-05T23:59:59.9999999", "2026-03-05T00:00:00.0000001"] {
                assert!(reader.parse(text, FieldType::Date).0.is_null(), "{format}: {text}");
                assert!(reader.parse(text, FieldType::DateTime).1);
            }
            assert_eq!(reader.parse("2026-03-05T00:00:00.0000000", FieldType::Date).0.to_string(), "2026-03-05");
        }
        let f = DateFormat::new("%d/%m/%Y %H:%M:%S.%f%z", None).unwrap();
        let (v, rounded) = f.parse("05/03/2026 14:30:00.1234567+02:00", FieldType::DateTime);
        assert_eq!(v.to_string(), "2026-03-05T14:30:00.123457+02:00");
        assert!(rounded);
        assert!(f.parse("05/03/2026 14:30:00.1234567+02:00", FieldType::Date).0.is_null());
        for (format, serial, expected) in [
            ("excel1900", "1", "1900-01-01T00:00:00"),
            ("excel1900", "59", "1900-02-28T00:00:00"),
            ("excel1900", "61", "1900-03-01T00:00:00"),
            ("excel1900", "45000.1", "2023-03-15T02:24:00"),
            ("excel1900", "25569.5", "1970-01-01T12:00:00"),
            ("excel1904", "0", "1904-01-01T00:00:00"),
            ("excel1904", "24107.5", "1970-01-01T12:00:00"),
        ] {
            let f = DateFormat::new(format, None).unwrap();
            let (value, rounded) = f.parse(serial, FieldType::DateTime);
            assert_eq!(value.to_string(), expected);
            assert!(!rounded, "{serial}");
        }
        let f = DateFormat::new("excel1900", None).unwrap();
        for bad in ["60", "60.5", "NaN", "inf", "1e100", "2958466"] {
            assert!(f.parse(bad, FieldType::DateTime).0.is_null(), "{bad}");
        }
        assert!(f.parse("25569.5", FieldType::Date).0.is_null());
        for (format, text) in [
            ("excel1900", "45000.5000000000001"),
            ("excel1900", "25569.0000000000000001"),
            ("excel1900", "0.00000000000000000001"),
            ("excel1904", "1e-400"),
        ] {
            let reader = DateFormat::new(format, None).unwrap();
            assert!(reader.parse(text, FieldType::Date).0.is_null(), "{text} wasn't exact midnight");
            let (value, rounded) = reader.parse(text, FieldType::DateTime);
            assert!(!value.is_null() && rounded, "{text}: rounding must be counted");
        }
        for text in ["25569.0000000000000000", "2.5569e4"] {
            assert_eq!(f.parse(text, FieldType::Date), (Value::Date(Date::parse("1970-01-01").unwrap()), false));
        }
    }
}
