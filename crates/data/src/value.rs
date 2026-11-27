//! Attribute values and field definitions.

use crate::date::{Date, DateTime};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum FieldType {
    Integer,
    Double,
    Text,
    Boolean,
    /// A calendar day.
    Date,
    /// A moment: a date and time, with or without a zone.
    DateTime,
}

impl FieldType {
    pub fn label(self) -> &'static str {
        match self {
            FieldType::Integer => "Long",
            FieldType::Double => "Double",
            FieldType::Text => "Text",
            FieldType::Boolean => "Boolean",
            FieldType::Date => "Date",
            FieldType::DateTime => "DateTime",
        }
    }
    pub fn is_numeric(self) -> bool {
        matches!(self, FieldType::Integer | FieldType::Double)
    }
    /// Whether it holds dates or date-times.
    pub fn is_temporal(self) -> bool {
        matches!(self, FieldType::Date | FieldType::DateTime)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Field {
    pub name: String,
    pub ty: FieldType,
    /// Display name (the field alias).
    #[serde(default)]
    pub alias: Option<String>,
}

impl Field {
    pub fn new(name: impl Into<String>, ty: FieldType) -> Self {
        Field { name: name.into(), ty, alias: None }
    }
    pub fn display(&self) -> &str {
        self.alias.as_deref().unwrap_or(&self.name)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Value {
    #[default]
    Null,
    Int(i64),
    Float(f64),
    Text(String),
    Bool(bool),
    Date(Date),
    DateTime(DateTime),
}

impl Serialize for Value {
    /// As its variant, except that a number that isn't finite (NaN, infinity), which JSON can't
    /// write, is written as null: JSON writes it as `null` in its place, which a number can't be
    /// read from, so the file holding it wouldn't open. Saving says how many
    /// ([`crate::Dataset::unsaved_numbers`]).
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Value::Float(f) if !f.is_finite() && s.is_human_readable() => s.serialize_unit_variant("Value", 0, "Null"),
            Value::Null => s.serialize_unit_variant("Value", 0, "Null"),
            Value::Int(i) => s.serialize_newtype_variant("Value", 1, "Int", i),
            Value::Float(f) => s.serialize_newtype_variant("Value", 2, "Float", f),
            Value::Text(t) => s.serialize_newtype_variant("Value", 3, "Text", t),
            Value::Bool(b) => s.serialize_newtype_variant("Value", 4, "Bool", b),
            Value::Date(d) => s.serialize_newtype_variant("Value", 5, "Date", d),
            Value::DateTime(t) => s.serialize_newtype_variant("Value", 6, "DateTime", t),
        }
    }
}

impl Value {
    /// Whether human-readable output must substitute Null for this numeric value.
    pub fn is_non_finite(&self) -> bool {
        matches!(self, Value::Float(value) if !value.is_finite())
    }

    /// Parses a typed numeric source field without truncating or clamping its numbers. Integer
    /// text is kept as an integer when exact; other numeric text is kept as a float, as native
    /// real source cells are. Non-finite values stay available until counted output conversion.
    /// Interactive edits use [`Self::parse`] and refuse non-finite input at command ingress.
    pub fn parse_source(text: &str, ty: FieldType) -> Value {
        if ty.is_numeric()
            && let Ok(number) = text.trim().parse::<f64>()
            && (ty == FieldType::Double || text.trim().parse::<i64>().is_err())
        {
            return Value::Float(number);
        }
        Self::parse(text, ty)
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::Float(f) if f.is_finite() => Some(*f),
            Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            Value::Text(s) => s.trim().parse::<f64>().ok().filter(|f| f.is_finite()),
            _ => None,
        }
    }
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }
    /// The value as a file writes it in text: a number in full, as the shortest text that reads
    /// back as the same number, and nothing for null. [`Display`](std::fmt::Display) is for
    /// showing a value, and rounds numbers to 6 decimals.
    pub fn to_file_text(&self) -> String {
        match self {
            Value::Null => String::new(),
            Value::Float(f) => f.to_string(),
            other => other.to_string(),
        }
    }
    /// How a date or date-time compares: microseconds since 1970, a date at its midnight and a
    /// time without a zone as if it were UTC. `None` for other values.
    pub fn temporal_key(&self) -> Option<i64> {
        match self {
            Value::Date(d) => Some(d.sort_key()),
            Value::DateTime(t) => Some(t.sort_key()),
            _ => None,
        }
    }
    /// The comparison keys of two values when one is a date or date-time and the other is one
    /// too, or text that reads as one (`'2026-01-01'`, `'2026-01-01T12:00Z'`): how
    /// `issued >= '2026-01-01'` compares dates. `None` otherwise, so text that isn't a full date
    /// (`'2026'`, `''`) compares as text.
    pub fn temporal_keys(&self, other: &Value) -> Option<(i64, i64)> {
        let key = |v: &Value| match v {
            Value::Text(s) => DateTime::parse(s).map(DateTime::sort_key),
            v => v.temporal_key(),
        };
        if self.temporal_key().is_none() && other.temporal_key().is_none() {
            return None;
        }
        Some((key(self)?, key(other)?))
    }
    /// Converts a JSON value to an attribute value.
    pub fn from_json(v: &serde_json::Value) -> Value {
        match v {
            serde_json::Value::Null => Value::Null,
            serde_json::Value::Bool(b) => Value::Bool(*b),
            serde_json::Value::Number(n) => match n.as_i64() {
                Some(i) => Value::Int(i),
                None => n.as_f64().map(Value::Float).unwrap_or(Value::Null),
            },
            serde_json::Value::String(s) => Value::Text(s.clone()),
            other => Value::Text(other.to_string()),
        }
    }
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Value::Null => serde_json::Value::Null,
            Value::Int(i) => (*i).into(),
            Value::Float(f) => serde_json::Number::from_f64(*f).map(serde_json::Value::Number).unwrap_or(serde_json::Value::Null),
            Value::Text(s) => s.clone().into(),
            Value::Bool(b) => (*b).into(),
            Value::Date(_) | Value::DateTime(_) => self.to_string().into(),
        }
    }
    /// Whether it asks for null: null itself, or text that is empty or `<null>`, as
    /// [`parse`](Value::parse) and the table take it. Such a value becoming null is no loss, so
    /// nothing reports or refuses it.
    pub fn asks_null(&self) -> bool {
        match self {
            Value::Null => true,
            Value::Text(t) => t.trim().is_empty() || t.trim().eq_ignore_ascii_case("<null>"),
            _ => false,
        }
    }
    /// [`parse`](Value::parse), for dates held as text in a file or a text field: a date field also
    /// takes a midnight without a zone (`2026-03-05T00:00:00`), which says only its day.
    pub fn parse_stored(text: &str, ty: FieldType) -> Value {
        match (ty, Value::parse(text, ty)) {
            (FieldType::Date, Value::Null) => DateTime::parse(text).filter(|t| t.is_date()).map_or(Value::Null, |t| Value::Date(t.date())),
            (_, v) => v,
        }
    }

    /// [`parse_stored`](Value::parse_stored), but digits past the microsecond of a date-time are
    /// rounded off, and the boolean says they were ([`DateTime::parse_rounding`]), for a reader to count.
    pub fn parse_stored_counted(text: &str, ty: FieldType) -> (Value, bool) {
        let t = text.trim();
        match ty {
            FieldType::DateTime => DateTime::parse_rounding(t).map_or((Value::Null, false), |(d, r)| (Value::DateTime(d), r)),
            FieldType::Date => match Date::parse(t) {
                Some(d) => (Value::Date(d), false),
                // Only a midnight as written is a date: digits past the microsecond never round
                // into the next day.
                None => DateTime::parse(t).filter(|d| d.is_date()).map_or((Value::Null, false), |d| (Value::Date(d.date()), false)),
            },
            _ => (Value::parse(text, ty), false),
        }
    }

    /// Whether a file's text is `<Null>` (any case, spaces around it allowed), which files exported
    /// from other programs hold for a missing value.
    pub fn is_null_marker(text: &str) -> bool {
        text.trim().eq_ignore_ascii_case("<null>")
    }

    /// A value read from a file's text, as [`parse_source`](Value::parse_source) but for a text
    /// field, which keeps the text as the file has it: spaces around it are kept, and all-space
    /// text is text. Empty text and [`<Null>`](Value::is_null_marker) are null, in any field; a
    /// reader counts the `<Null>`s it reads ([`crate::source::null_text_note`]). Text typed into the
    /// table goes through [`parse`](Value::parse), which trims.
    pub fn parse_file(text: &str, ty: FieldType) -> Value {
        match ty {
            _ if text.is_empty() || Self::is_null_marker(text) => Value::Null,
            FieldType::Text => Value::Text(text.to_string()),
            _ => Self::parse_source(text, ty),
        }
    }

    /// Whether this value, put in a decimal (Double) field, comes out rounded: a whole number past
    /// 2^53, written as digits (with or without `.0`) and held only exactly, or a number written as
    /// text with more significant digits than a decimal holds ([`double_holds`](crate::io::double_holds)):
    /// `9007199254740993.0` and `12345678901234567000.0` are; `0.1`, `1e3` and
    /// `1.2345678901234567e19`, which says only its 17 digits, aren't.
    pub fn rounds_as_double(&self) -> bool {
        match self {
            // i128 holds both exactly.
            Value::Int(i) => (*i as f64) as i128 != i128::from(*i),
            Value::Text(t) => {
                let t = t.trim();
                if !t.parse::<f64>().is_ok_and(f64::is_finite) {
                    return false;
                }
                // A whole number written with a decimal point (`12345678901234567000.0`) is held
                // only exactly, as one written without it is.
                match t.split_once('.') {
                    Some((whole, zeros)) if whole.bytes().any(|b| b.is_ascii_digit()) && zeros.bytes().all(|b| b == b'0') => {
                        !crate::io::double_holds(whole)
                    }
                    _ => !crate::io::double_holds(t),
                }
            }
            _ => false,
        }
    }

    /// Whether a number written as text has a fraction, read from its digits (not from the
    /// decimal it parses to, which can round one away): `2.5`, `2.0000000000000001` and
    /// `2.0000000000000000001e0` have; `2`, `2.000` and `1e3` haven't. Text that isn't a number
    /// written out in digits (`inf`, `x`) has none.
    pub fn text_has_fraction(text: &str) -> bool {
        let t = text.trim().trim_start_matches(['-', '+']);
        let (mantissa, exp) = match t.split_once(['e', 'E']) {
            Some((m, e)) => (m, e.parse::<i64>().ok()),
            None => (t, Some(0)),
        };
        let Some(exp) = exp else { return false };
        let (whole, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
        if !whole.bytes().chain(frac.bytes()).all(|b| b.is_ascii_digit()) || whole.len() + frac.len() == 0 {
            return false;
        }
        // The digits past the point once the exponent moves it: a fraction when any isn't 0.
        let digits = format!("{whole}{frac}");
        let past = i64::try_from(frac.len()).unwrap_or(i64::MAX).saturating_sub(exp);
        let kept = i64::try_from(digits.len()).unwrap_or(0).saturating_sub(past.max(0));
        match usize::try_from(kept) {
            Ok(kept) => digits.get(kept..).is_some_and(|f| f.bytes().any(|b| b != b'0')),
            // The point moves past every digit: all of them are fraction.
            Err(_) => digits.bytes().any(|b| b != b'0'),
        }
    }

    /// The yes/no value this value is, as a yes/no field holds it: a yes/no as it is, a whole
    /// number yes unless it is 0, text one of the [`yes_no`](Value::yes_no) words; anything else
    /// (`0.5`, `maybe`, a date, null) is neither.
    pub fn as_yes_no(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            Value::Int(i) => Some(*i != 0),
            Value::Float(f) if f.is_finite() && f.fract() == 0.0 => Some(*f != 0.0),
            Value::Text(t) => Value::yes_no(t),
            _ => None,
        }
    }

    /// The yes/no value text says, ignoring case and spaces around it: `true`, `t`, `yes`, `y` and
    /// `1` are true; `false`, `f`, `no`, `n` and `0` are false; other text is neither.
    pub fn yes_no(text: &str) -> Option<bool> {
        match text.trim().to_ascii_lowercase().as_str() {
            "true" | "t" | "yes" | "y" | "1" => Some(true),
            "false" | "f" | "no" | "n" | "0" => Some(false),
            _ => None,
        }
    }

    /// Parses user text into a value of `ty` (empty → Null). Text a number field can't hold,
    /// including a non-finite Double (`NaN`, `inf`), is Null. Dates are ISO 8601: `YYYY-MM-DD`
    /// for a date; that, a time and an optional zone for a date-time. Text that isn't one is Null.
    /// A yes/no field takes the words of [`yes_no`](Value::yes_no); other text is Null, not false.
    pub fn parse(text: &str, ty: FieldType) -> Value {
        let t = text.trim();
        if t.is_empty() || t.eq_ignore_ascii_case("<null>") {
            return Value::Null;
        }
        match ty {
            FieldType::Integer => t
                .parse::<i64>()
                .ok()
                .or_else(|| t.parse::<f64>().ok().filter(|f| f.is_finite()).map(|f| f as i64))
                .map_or(Value::Null, Value::Int),
            FieldType::Double => t.parse::<f64>().ok().filter(|f| f.is_finite()).map_or(Value::Null, Value::Float),
            FieldType::Boolean => Value::yes_no(t).map_or(Value::Null, Value::Bool),
            FieldType::Text => Value::Text(t.to_string()),
            FieldType::Date => Date::parse(t).map(Value::Date).unwrap_or(Value::Null),
            FieldType::DateTime => DateTime::parse(t).map(Value::DateTime).unwrap_or(Value::Null),
        }
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Null => write!(f, "<Null>"),
            Value::Int(i) => write!(f, "{i}"),
            Value::Float(v) => {
                if v.fract() == 0.0 && v.abs() < 1e15 {
                    write!(f, "{v:.0}")
                } else {
                    write!(f, "{}", (v * 1e6).round() / 1e6)
                }
            }
            Value::Text(s) => write!(f, "{s}"),
            Value::Bool(b) => write!(f, "{}", if *b { "True" } else { "False" }),
            Value::Date(d) => write!(f, "{d}"),
            Value::DateTime(t) => write!(f, "{t}"),
        }
    }
}

impl PartialOrd for Value {
    /// Dates by instant (also against date text), numbers as numbers, nulls first, then text
    /// ignoring case. Two date-times at one instant with different offsets are `Equal` here but
    /// not `==`, which compares what is stored (see [`DateTime`]).
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        use std::cmp::Ordering;
        if let Some((a, b)) = self.temporal_keys(other) {
            return Some(a.cmp(&b));
        }
        // Whole numbers order exactly: through f64, neighbours above 2^53 would tie. A whole
        // number against any other number is exact too, so equal values stay transitive and a
        // sort by this order stays total.
        if let (Value::Int(a), Value::Int(b)) = (self, other) {
            return Some(a.cmp(b));
        }
        match (self.as_f64(), other.as_f64()) {
            (Some(a), Some(b)) if !matches!(self, Value::Text(_)) || !matches!(other, Value::Text(_)) => match (self, other) {
                // Whole-number text against a whole number reads as the integer it spells.
                (Value::Int(i), Value::Text(t)) => match t.trim().parse::<i64>() {
                    Ok(j) => Some(i.cmp(&j)),
                    Err(_) => int_cmp_f64(*i, b),
                },
                (Value::Text(t), Value::Int(i)) => match t.trim().parse::<i64>() {
                    Ok(j) => Some(j.cmp(i)),
                    Err(_) => int_cmp_f64(*i, a).map(Ordering::reverse),
                },
                (Value::Int(i), _) => int_cmp_f64(*i, b),
                (_, Value::Int(i)) => int_cmp_f64(*i, a).map(Ordering::reverse),
                _ => a.partial_cmp(&b),
            },
            _ => match (self, other) {
                (Value::Null, Value::Null) => Some(Ordering::Equal),
                (Value::Null, _) => Some(Ordering::Less),
                (_, Value::Null) => Some(Ordering::Greater),
                _ => Some(self.to_string().to_lowercase().cmp(&other.to_string().to_lowercase())),
            },
        }
    }
}

/// `i` against `f` exactly, without rounding `i` to the nearest f64: unknown for NaN.
fn int_cmp_f64(i: i64, f: f64) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering;
    if f.is_nan() {
        return None;
    }
    // 2^63 is exactly representable; every whole f64 below it in magnitude fits in an i64.
    const TWO_63: f64 = 9_223_372_036_854_775_808.0;
    if f >= TWO_63 {
        return Some(Ordering::Less);
    }
    if f < -TWO_63 {
        return Some(Ordering::Greater);
    }
    let whole = f.trunc();
    let w = whole as i64;
    Some(match i.cmp(&w) {
        Ordering::Equal => 0.0_f64.partial_cmp(&(f - whole)).unwrap_or(Ordering::Equal),
        o => o,
    })
}

/// A Unique Values class's value, given as its text, read as a number exactly as written: any
/// text Rust reads as a decimal (`9`, `-2.5`, `1e300`, `.5`, past 2^53 or i128), kept as its
/// significant digits and the place of its point rather than rounded to an `f64`; or one of the
/// words a decimal can be (infinity, NaN).
#[derive(Clone, Copy, Debug)]
enum ClassNumber {
    /// `0.d…d × 10^point`: below zero or not, where its significant digits start and end in the
    /// text (a point between them is skipped when they're read; none for zero), and `point`.
    Finite {
        negative: bool,
        start: usize,
        end: usize,
        point: i64,
    },
    Infinite {
        negative: bool,
    },
    NaN,
}

/// The largest exponent kept, so sums with it can't overflow: an exponent past it (past 2^61, as
/// no value's display text has, only a text typed so) is taken as it, and numbers with two such
/// exponents of one sign are ordered by their digits.
const MAX_EXPONENT: i64 = i64::MAX / 4;

fn class_number(text: &str) -> Option<ClassNumber> {
    // Only what Rust reads as a decimal is a number.
    let f = text.parse::<f64>().ok()?;
    let (negative, body) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let offset = text.len() - body.len();
    let (mantissa, exponent) = match body.find(['e', 'E']) {
        Some(i) => (body.get(..i).unwrap_or_default(), body.get(i + 1..).unwrap_or_default()),
        None => (body, ""),
    };
    if !mantissa.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        // A word: a number only as a float's display text writes it (`inf`, `-inf`, `NaN`), so a
        // text value such as `Nan` (a name) or `Infinity` stays text.
        return match text {
            "NaN" => Some(ClassNumber::NaN),
            "inf" | "-inf" => Some(ClassNumber::Infinite { negative: f < 0.0 }),
            _ => None,
        };
    }
    // (Rust read the text, so an exponent given has digits: one that doesn't parse is too large.)
    let size = |digits: &str| digits.parse::<i64>().unwrap_or(MAX_EXPONENT).min(MAX_EXPONENT);
    let exponent = match exponent.strip_prefix('-') {
        _ if exponent.is_empty() => 0,
        Some(digits) => -size(digits),
        None => size(exponent.strip_prefix('+').unwrap_or(exponent)),
    };
    let dot = mantissa.find('.').unwrap_or(mantissa.len());
    let significant = |b: &u8| b.is_ascii_digit() && *b != b'0';
    let (Some(first), Some(last)) = (mantissa.bytes().position(|b| significant(&b)), mantissa.bytes().rposition(|b| significant(&b)))
    else {
        return Some(ClassNumber::Finite { negative: false, start: offset, end: offset, point: 0 });
    };
    // Digits from the first significant one to the point, or minus the zeros between them.
    let point = if first < dot { (dot - first) as i64 } else { -((first - dot - 1) as i64) };
    Some(ClassNumber::Finite { negative, start: offset + first, end: offset + last + 1, point: point.saturating_add(exponent) })
}

/// The significant digits of a number between `start` and `end` in `text`, past any point.
fn significant_digits(text: &str, start: usize, end: usize) -> impl Iterator<Item = u8> + '_ {
    text.get(start..end).unwrap_or_default().bytes().filter(|b| *b != b'.')
}

/// Orders two class numbers, read from the texts `a` and `b`, exactly by value: NaN after every
/// other number. Numbers equal in value (`1`, `1.0`, `1e0`) compare equal here, and then by text
/// in [`cmp_numeric_classes`].
fn cmp_class_numbers(a: &str, x: ClassNumber, b: &str, y: ClassNumber) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    // -1, 0 or 1, and for a finite number other than 0, its size against another of its sign.
    let sign = |n: ClassNumber| match n {
        ClassNumber::Finite { start, end, .. } if start == end => 0,
        ClassNumber::Finite { negative, .. } | ClassNumber::Infinite { negative } => {
            if negative {
                -1
            } else {
                1
            }
        }
        ClassNumber::NaN => 2,
    };
    match (x, y) {
        (ClassNumber::NaN, ClassNumber::NaN) => Ordering::Equal,
        (ClassNumber::NaN, _) => Ordering::Greater,
        (_, ClassNumber::NaN) => Ordering::Less,
        (ClassNumber::Infinite { negative: n }, ClassNumber::Infinite { negative: m }) => m.cmp(&n),
        (ClassNumber::Infinite { negative }, _) => {
            if negative {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        }
        (_, ClassNumber::Infinite { negative }) => {
            if negative {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        }
        (ClassNumber::Finite { negative, start: sa, end: ea, point: pa }, ClassNumber::Finite { start: sb, end: eb, point: pb, .. }) => {
            sign(x).cmp(&sign(y)).then_with(|| {
                if sign(x) == 0 {
                    return Ordering::Equal;
                }
                // The same sign: the larger size has its point further on, or the same and larger
                // digits (shorter ones as if ended with zeros).
                let size = pa.cmp(&pb).then_with(|| significant_digits(a, sa, ea).cmp(significant_digits(b, sb, eb)));
                if negative { size.reverse() } else { size }
            })
        }
    }
}

/// Orders two class values, each a text and the number it reads as, as [`cmp_numeric_classes`].
fn cmp_read_classes(a: &str, x: Option<ClassNumber>, b: &str, y: Option<ClassNumber>) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (x, y) {
        (Some(x), Some(y)) => cmp_class_numbers(a, x, b, y).then_with(|| a.cmp(b)),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => a.cmp(b),
    }
}

/// Whether a value's text is a missing value's: the empty text or `<Null>`.
fn missing(text: &str) -> bool {
    text.is_empty() || text == "<Null>"
}

/// Whether a class value's text keeps Unique Values classes from being ordered by number: text
/// that reads as no number, other than the empty text and `<Null>` (a missing value's).
pub fn orders_as_text(text: &str) -> bool {
    !missing(text) && class_number(text).is_none()
}

/// [`numeric_classes`] over texts already read as numbers.
fn numeric_read<'a>(values: impl IntoIterator<Item = (&'a str, Option<ClassNumber>)>) -> bool {
    let mut number = false;
    for (text, n) in values {
        if missing(text) {
            continue;
        }
        if n.is_none() {
            return false;
        }
        number = true;
    }
    number
}

/// Whether Unique Values classes with these values, given as their text, are ordered by number:
/// when one reads as a number and every other does too, or is the empty text or `<Null>`. A
/// numeric field's values always are; a text field's when it holds only numbers, such as codes.
pub fn numeric_classes<'a>(values: impl IntoIterator<Item = &'a str>) -> bool {
    numeric_read(values.into_iter().map(|v| (v, class_number(v))))
}

/// Orders two Unique Values classes' values, given as their text, when [`numeric_classes`]: by
/// number (9 before 10), then the rest (the empty text, `<Null>`) after them by text. Values equal
/// in number are ordered by text (1 before 1.0), so this is a total order on texts.
pub fn cmp_numeric_classes(a: &str, b: &str) -> std::cmp::Ordering {
    cmp_read_classes(a, class_number(a), b, class_number(b))
}

/// A class value's text, read as a number once: ordered as [`cmp_numeric_classes`] orders texts,
/// without reading them again on every comparison.
#[derive(Clone, Debug)]
pub struct ClassText {
    text: String,
    number: Option<ClassNumber>,
}

impl ClassText {
    pub fn new(text: String) -> Self {
        let number = class_number(&text);
        ClassText { text, number }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn into_text(self) -> String {
        self.text
    }

    /// [`orders_as_text`] for this value.
    pub fn orders_as_text(&self) -> bool {
        !missing(&self.text) && self.number.is_none()
    }

    /// [`numeric_classes`] over these values.
    pub fn numeric<'a>(values: impl IntoIterator<Item = &'a ClassText>) -> bool {
        numeric_read(values.into_iter().map(|c| (c.text.as_str(), c.number)))
    }
}

impl PartialEq for ClassText {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text
    }
}

impl Eq for ClassText {}

impl Ord for ClassText {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        cmp_read_classes(&self.text, self.number, &other.text, other.number)
    }
}

impl PartialOrd for ClassText {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// What Unique Values leaves out of its classes: how many kept features with a geometry have a
/// value past the first it keeps (drawn as `<all other values>`), and whether those first were
/// taken by number ([`numeric_classes`] over every value) or by text. The classes kept can all be
/// numbers though they were taken by text, past a text value that sorts after them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PastClasses {
    pub features: usize,
    pub by_number: bool,
}

/// Sorts Unique Values classes' values, given as their text: by number when
/// [`numeric_classes`], else by text.
pub fn sort_class_values(values: &mut Vec<String>) {
    let mut read: Vec<ClassText> = std::mem::take(values).into_iter().map(ClassText::new).collect();
    if ClassText::numeric(&read) {
        read.sort();
    } else {
        read.sort_by(|a, b| a.text.cmp(&b.text));
    }
    values.extend(read.into_iter().map(ClassText::into_text));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whole numbers order exactly against whole numbers and against decimals, so values equal to
    /// one another stay equal to the same things: a sort by this order never sees a contradiction.
    #[test]
    fn whole_numbers_order_exactly_and_transitively() {
        use std::cmp::Ordering;
        let two_53 = 1_i64 << 53;
        let (a, f, b) = (Value::Int(two_53), Value::Float(9_007_199_254_740_992.0), Value::Int(two_53 + 1));
        assert_eq!(a.partial_cmp(&f), Some(Ordering::Equal));
        assert_eq!(b.partial_cmp(&f), Some(Ordering::Greater));
        assert_eq!(f.partial_cmp(&b), Some(Ordering::Less));
        assert_eq!(a.partial_cmp(&b), Some(Ordering::Less));
        assert_eq!(Value::Int(2).partial_cmp(&Value::Float(2.5)), Some(Ordering::Less));
        assert_eq!(Value::Int(-2).partial_cmp(&Value::Float(-2.5)), Some(Ordering::Greater));
        assert_eq!(Value::Int(i64::MAX).partial_cmp(&Value::Float(9.3e18)), Some(Ordering::Less));
        // The same integer written as text compares as that integer, both ways round.
        let text = Value::Text(" 9007199254740993".into());
        assert_eq!(b.partial_cmp(&text), Some(Ordering::Equal));
        assert_eq!(text.partial_cmp(&a), Some(Ordering::Greater));
        let mut v = vec![b.clone(), f.clone(), a.clone(), Value::Float(1.5), Value::Int(1)];
        v.sort_by(|x, y| x.partial_cmp(y).unwrap_or(Ordering::Equal));
        assert_eq!(v, [Value::Int(1), Value::Float(1.5), f, a, b]);
    }

    #[test]
    fn values_stay_small() {
        assert_eq!(std::mem::size_of::<Value>(), 24);
    }

    #[test]
    fn dates_compare_with_dates_and_date_text() {
        let d = Value::parse("2026-03-05", FieldType::Date);
        let t = Value::parse("2026-03-05T00:00:00+01:00", FieldType::DateTime);
        assert_eq!(d.to_string(), "2026-03-05");
        assert!(t < d, "an hour before UTC midnight");
        assert!(d > Value::Text("2026-01-01".into()));
        assert!(d < Value::Text("2026-03-05T00:00:01".into()));
        // Text that isn't a full date compares as text.
        assert_eq!(d.temporal_keys(&Value::Text("2026".into())), None);
        assert!(d > Value::Text("2026".into()));
        assert!(d > Value::Null);
        assert_eq!(Value::parse("2026-02-30", FieldType::Date), Value::Null);
        assert_eq!(d.to_json(), serde_json::json!("2026-03-05"));
    }

    /// A date field takes a date-time only when it is a midnight as written: digits past the
    /// microsecond never round it into the next day.
    #[test]
    fn a_stored_date_is_not_rounded_into_the_next_day() {
        assert_eq!(Value::parse_stored_counted("2026-03-05T23:59:59.9999999", FieldType::Date), (Value::Null, false));
        assert_eq!(Value::parse_stored_counted("2026-03-05T00:00:00.0000001", FieldType::Date), (Value::Null, false));
        let midnight = Value::parse_stored_counted("2026-03-05T00:00:00.000000000", FieldType::Date);
        assert_eq!(midnight, (Value::parse("2026-03-05", FieldType::Date), false));
        let (moment, rounded) = Value::parse_stored_counted("2026-03-05T23:59:59.9999999", FieldType::DateTime);
        assert_eq!((moment.to_string(), rounded), ("2026-03-06T00:00:00".to_string(), true));
    }

    #[test]
    fn project_files_hold_dates_as_iso_text() {
        let vals = vec![Value::parse("2026-03-05", FieldType::Date), Value::parse("2026-03-05T14:30:00+02:00", FieldType::DateTime)];
        let json = serde_json::to_string(&vals).unwrap_or_default();
        assert_eq!(json, r#"[{"Date":"2026-03-05"},{"DateTime":"2026-03-05T14:30:00+02:00"}]"#);
        assert_eq!(serde_json::from_str::<Vec<Value>>(&json).ok(), Some(vals));
        assert!(serde_json::from_str::<Value>(r#"{"Date":"2026-02-30"}"#).is_err());
    }

    /// File text keeps its spaces in a text field (#2059); empty and `<Null>` are null in any
    /// field, numbers read past spaces, and text typed into the table is still trimmed.
    #[test]
    fn file_text_is_kept_as_it_is_and_typed_text_is_trimmed() {
        for (text, kept) in
            [(" 2535", Value::Text(" 2535".into())), ("   ", Value::Text("   ".into())), ("", Value::Null), (" <null> ", Value::Null)]
        {
            assert_eq!(Value::parse_file(text, FieldType::Text), kept, "{text:?}");
        }
        assert_eq!(Value::parse_file(" 2535 ", FieldType::Integer), Value::Int(2535));
        assert_eq!(Value::parse_file("<Null>", FieldType::Double), Value::Null);
        assert_eq!(Value::parse(" 2535", FieldType::Text), Value::Text("2535".into()), "typed text is trimmed");
    }

    /// Interactive Double text rejects non-finite numbers, while source parsing preserves them
    /// for the counted output notice. Exact integer text stays integer; source decimals stay real.
    #[test]
    fn a_number_field_reads_text_that_is_not_a_finite_number_as_null() {
        for text in ["NaN", "inf", "-inf", "infinity", "1e400"] {
            assert_eq!(Value::parse(text, FieldType::Double), Value::Null, "{text}");
            assert_eq!(Value::parse(text, FieldType::Integer), Value::Null, "{text}");
            assert!(matches!(Value::parse_source(text, FieldType::Integer), Value::Float(f) if !f.is_finite()));
            assert!(matches!(Value::parse_source(text, FieldType::Double), Value::Float(f) if !f.is_finite()));
        }
        for (text, said) in [(" Yes ", Some(true)), ("T", Some(true)), ("1", Some(true)), ("n", Some(false)), ("FALSE", Some(false))] {
            assert_eq!(Value::yes_no(text), said, "{text}");
            assert_eq!(Value::parse(text, FieldType::Boolean), Value::Bool(said.unwrap_or_default()), "{text}");
        }
        for (text, rounded) in [
            ("9007199254740993.0", true),
            ("12345678901234567000.0", true),
            ("12345678901234567890", true),
            ("9007199254740992.0", false),
            ("0.1", false),
            ("1e3", false),
            ("1.2345678901234567e19", false),
        ] {
            assert_eq!(Value::Text(text.into()).rounds_as_double(), rounded, "{text}");
        }
        assert!(Value::Int((1 << 53) + 1).rounds_as_double() && !Value::Int(1 << 53).rounds_as_double());
        for (text, fraction) in [
            ("2.5", true),
            ("-2.7", true),
            ("2.0000000000000001", true),
            ("2.0000000000000000001e0", true),
            ("25e-1", true),
            ("5e-1", true),
            (".5", true),
            ("2", false),
            ("2.000", false),
            ("1e3", false),
            ("1.5e1", false),
            ("2.50e1", false),
            ("inf", false),
            ("x", false),
        ] {
            assert_eq!(Value::text_has_fraction(text), fraction, "{text}");
        }
        for text in ["maybe", "2", "on", "yess"] {
            assert_eq!(Value::yes_no(text), None, "{text}");
            assert_eq!(Value::parse(text, FieldType::Boolean), Value::Null, "{text} is neither, not no");
        }
        assert_eq!(Value::parse("2.5", FieldType::Double), Value::Float(2.5));
        assert_eq!(Value::parse("2.5", FieldType::Integer), Value::Int(2));
        assert_eq!(Value::parse("1e3", FieldType::Integer), Value::Int(1000));
        assert_eq!(Value::parse_source("3", FieldType::Integer), Value::Int(3));
        assert_eq!(Value::parse_source("2.7", FieldType::Integer), Value::Float(2.7));
        assert_eq!(Value::parse_source("1e30", FieldType::Integer), Value::Float(1e30));
    }

    /// JSON cannot represent non-finite numbers; output uses a null value with the save report.
    /// Finite values keep their established encoding.
    #[test]
    fn a_value_that_is_not_finite_is_written_and_read_as_null() {
        for f in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(serde_json::to_string(&Value::Float(f)).ok().as_deref(), Some(r#""Null""#));
        }
        for v in [Value::Null, Value::Int(-3), Value::Float(0.1), Value::Text("NaN".into()), Value::Bool(true)] {
            let text = serde_json::to_string(&v).unwrap_or_default();
            assert_eq!(serde_json::from_str::<Value>(&text).ok(), Some(v.clone()), "{text}");
        }
        assert_eq!(serde_json::to_string(&Value::Float(0.1)).ok().as_deref(), Some(r#"{"Float":0.1}"#), "the format is unchanged");
        // A variant this version doesn't know is refused by name, which is how a release before
        // dates refuses a project holding them.
        assert!(serde_json::from_str::<Value>(r#"{"Time":"12:00"}"#).is_err_and(|e| e.to_string().contains("unknown variant")));
    }

    fn sorted(values: &[&str]) -> Vec<String> {
        let mut v: Vec<String> = values.iter().map(|s| s.to_string()).collect();
        sort_class_values(&mut v);
        v
    }

    #[test]
    fn classes_are_ordered_by_number_when_every_value_reads_as_one() {
        assert_eq!(sorted(&["100", "9", "10"]), ["9", "10", "100"]);
        assert_eq!(sorted(&["10", "2.5", "-1"]), ["-1", "2.5", "10"]);
        // As a field's display text writes them: a large decimal without an exponent.
        assert_eq!(sorted(&["100000000000000000000", "3e2", "250"]), ["250", "3e2", "100000000000000000000"]);
        assert_eq!(sorted(&[&i64::MAX.to_string(), &i64::MIN.to_string(), "0"]), [i64::MIN.to_string(), "0".into(), i64::MAX.to_string()]);
    }

    #[test]
    fn a_text_among_the_values_keeps_text_order() {
        assert_eq!(sorted(&["b", "A", "10", "9"]), ["10", "9", "A", "b"]);
        assert_eq!(sorted(&["10", "9", "1 0"]), ["1 0", "10", "9"], "a number with a space is text");
        assert!(!numeric_classes(["9", "x"]));
    }

    #[test]
    fn missing_values_go_after_the_numbers() {
        assert_eq!(sorted(&["<Null>", "10", "", "9"]), ["9", "10", "", "<Null>"]);
        assert!(numeric_classes(["<Null>", "5"]));
        // With no number, nothing to order by number.
        assert!(!numeric_classes(["<Null>", ""]));
        assert!(!numeric_classes([]));
    }

    #[test]
    fn whole_numbers_past_2_53_are_ordered_exactly() {
        // 2^53 + 1 and 2^53 are one decimal; as whole numbers they differ.
        assert_eq!(sorted(&["9007199254740993", "9007199254740992"]), ["9007199254740992", "9007199254740993"]);
        assert_eq!(sorted(&["-9007199254740992", "-9007199254740993"]), ["-9007199254740993", "-9007199254740992"]);
        // A decimal that reads as 2^53 is between them.
        assert_eq!(
            sorted(&["9007199254740993", "9007199254740992.5", "9007199254740991"]),
            ["9007199254740991", "9007199254740992.5", "9007199254740993"]
        );
        // Below zero, where their texts order the other way.
        assert_eq!(sorted(&["-9007199254740992.5", "-9007199254740993"]), ["-9007199254740993", "-9007199254740992.5"]);
        assert_eq!(cmp_numeric_classes("9007199254740992.5", "9007199254740993"), std::cmp::Ordering::Less);
        assert_eq!(cmp_numeric_classes("9007199254740993", "9007199254740992.5"), std::cmp::Ordering::Greater);
    }

    /// Whole numbers of any length are ordered by their digits: past i128 too, and against a
    /// decimal at i128's limit.
    #[test]
    fn whole_numbers_past_i128_are_ordered_exactly() {
        let below_min = "-170141183460469231731687303715884105729";
        let further_below = "-170141183460469231731687303715884105730";
        assert_eq!(sorted(&[below_min, further_below]), [further_below, below_min]);
        let past_max = "170141183460469231731687303715884105729";
        let further_past = "170141183460469231731687303715884105730";
        assert_eq!(sorted(&[further_past, past_max]), [past_max, further_past]);
        // i128::MAX against decimals that read as one f64 with it (2^127): by what is written.
        let max = i128::MAX.to_string();
        assert_eq!(sorted(&["1.7014118346046923e38", &max]), ["1.7014118346046923e38", max.as_str()]);
        assert_eq!(
            sorted(&["170141183460469231731687303715884105728.0", &max]),
            [max.as_str(), "170141183460469231731687303715884105728.0"]
        );
        assert_eq!(
            sorted(&["170141183460469231731687303715884105728.0", past_max]),
            ["170141183460469231731687303715884105728.0", past_max]
        );
        // Hundreds of digits: above every decimal but infinity.
        let huge = format!("1{}", "0".repeat(400));
        let below_huge = format!("-{huge}");
        assert_eq!(
            sorted(&["inf", &huge, "1e308", &below_huge, "-inf", "-1e308"]),
            ["-inf", below_huge.as_str(), "-1e308", "1e308", huge.as_str(), "inf"]
        );
        // A large float's display text is a whole number in full (its shortest digits, then
        // zeros): neighbouring floats' texts are in the floats' order.
        for v in [1e300, -1e300, 1.2345678901234568e20, 9.007199254740994e15] {
            let (a, b) = (Value::Float(v).to_string(), Value::Float(v.next_up()).to_string());
            assert!(!a.contains(['.', 'e']), "{a}");
            assert_eq!(cmp_numeric_classes(&a, &b), std::cmp::Ordering::Less, "{a} {b}");
        }
        // Signs and leading zeros: one number, ordered by text.
        assert_eq!(sorted(&["7", "007", "+7", "-0", "0", "-00", "+0"]), ["+0", "-0", "-00", "0", "+7", "007", "7"]);
    }

    /// Decimals are ordered by what is written, not by the f64 they read as: two that read as
    /// one f64, a decimal against a whole number it rounds to, and exponents.
    #[test]
    fn decimals_are_ordered_exactly_as_written() {
        assert_eq!(sorted(&["10000000000000000.5", "9999999999999999.9"]), ["9999999999999999.9", "10000000000000000.5"]);
        assert_eq!(cmp_numeric_classes("-9007199254740992", "-9007199254740991.9"), std::cmp::Ordering::Less);
        assert_eq!(cmp_numeric_classes("9007199254740992", "9007199254740992.1"), std::cmp::Ordering::Less);
        assert_eq!(sorted(&["0.30000000000000001", "0.3", "0.29999999999999999"]), ["0.29999999999999999", "0.3", "0.30000000000000001"]);
        // One value written five ways: equal, then by text.
        assert_eq!(sorted(&["1e2", "100", "100.0", "0.1e3", "+1E+2"]), ["+1E+2", "0.1e3", "100", "100.0", "1e2"]);
        // Small and large exponents, and ones past what an i64 holds.
        let tiny = format!("1e-{}", "9".repeat(30));
        let huge = format!("1e{}", "9".repeat(30));
        assert_eq!(
            sorted(&[&huge, "1e400", "0", &tiny, "1e-400", "-1e-400", ".5", "5."]),
            ["-1e-400", "0", tiny.as_str(), "1e-400", ".5", "5.", "1e400", huge.as_str()]
        );
        assert_eq!(sorted(&["-0.0", "0e5", "-0", "0.000"]), ["-0", "-0.0", "0.000", "0e5"], "zeros are one value");
    }

    /// Words are numbers only as a float's display text writes them: `Nan` (a name) and
    /// `Infinity` are text, and keep text order.
    #[test]
    fn only_a_floats_words_are_numbers() {
        for word in ["Nan", "nan", "NAN", "Inf", "INF", "infinity", "Infinity", "+inf", "-Infinity"] {
            assert!(orders_as_text(word), "{word}");
            let mut text_order = vec![word, "10", "9"];
            text_order.sort();
            assert_eq!(sorted(&[word, "10", "9"]), text_order, "{word}");
        }
        for word in ["NaN", "inf", "-inf"] {
            assert!(!orders_as_text(word), "{word}");
        }
        assert_eq!(sorted(&["NaN", "10", "-inf", "9", "inf"]), ["-inf", "9", "10", "inf", "NaN"]);
        let shown: Vec<String> = [f64::NAN, f64::INFINITY, f64::NEG_INFINITY].iter().map(|f| Value::Float(*f).to_string()).collect();
        assert_eq!(shown, ["NaN", "inf", "-inf"], "as a float field's values are written");
    }

    #[test]
    fn numbers_equal_in_value_and_not_a_number_have_a_place() {
        // Equal in number: by text, so the order is total and repeatable.
        assert_eq!(sorted(&["1.0", "1"]), ["1", "1.0"]);
        assert_eq!(sorted(&["0", "-0"]), ["-0", "0"]);
        // Infinities in their place, NaN after every number and before missing values.
        assert_eq!(sorted(&["NaN", "inf", "<Null>", "0", "-inf"]), ["-inf", "0", "inf", "NaN", "<Null>"]);
    }

    #[test]
    fn the_numeric_order_is_total() {
        let values = [
            "NaN",
            "-inf",
            "inf",
            "1",
            "1.0",
            "+1",
            "01",
            "-0",
            "0",
            "9007199254740993",
            "9007199254740992.5",
            "-9007199254740993",
            "-9007199254740992.5",
            "170141183460469231731687303715884105727",
            "1.7014118346046923e38",
            "170141183460469231731687303715884105729",
            "-170141183460469231731687303715884105730",
            "10000000000000000.5",
            "9999999999999999.9",
            "-9007199254740991.9",
            "1e2",
            "0.1e3",
            "1e-400",
            "-1e-400",
            "2.5",
            "",
            "<Null>",
        ];
        for a in values {
            assert_eq!(cmp_numeric_classes(a, a), std::cmp::Ordering::Equal, "{a}");
            for b in values {
                assert_eq!(cmp_numeric_classes(a, b), cmp_numeric_classes(b, a).reverse(), "{a} {b}");
                if a != b {
                    assert_ne!(cmp_numeric_classes(a, b), std::cmp::Ordering::Equal, "{a} {b}");
                }
                for c in values {
                    if cmp_numeric_classes(a, b).is_lt() && cmp_numeric_classes(b, c).is_lt() {
                        assert!(cmp_numeric_classes(a, c).is_lt(), "{a} < {b} < {c}");
                    }
                }
            }
        }
    }
}
