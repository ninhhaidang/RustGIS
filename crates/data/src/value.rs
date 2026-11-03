//! Attribute values and field definitions.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldType {
    Integer,
    Double,
    Text,
    Boolean,
}

impl FieldType {
    pub fn label(self) -> &'static str {
        match self {
            FieldType::Integer => "Long",
            FieldType::Double => "Double",
            FieldType::Text => "Text",
            FieldType::Boolean => "Boolean",
        }
    }
    pub fn is_numeric(self) -> bool {
        matches!(self, FieldType::Integer | FieldType::Double)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum Value {
    #[default]
    Null,
    Int(i64),
    Float(f64),
    Text(String),
    Bool(bool),
}

impl Value {
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
        }
    }
    /// Parses user text into a value of `ty` (empty → Null).
    pub fn parse(text: &str, ty: FieldType) -> Value {
        let t = text.trim();
        if t.is_empty() || t.eq_ignore_ascii_case("<null>") {
            return Value::Null;
        }
        match ty {
            FieldType::Integer => {
                t.parse::<i64>().map(Value::Int).or_else(|_| t.parse::<f64>().map(|f| Value::Int(f as i64))).unwrap_or(Value::Null)
            }
            FieldType::Double => t.parse::<f64>().map(Value::Float).unwrap_or(Value::Null),
            FieldType::Boolean => Value::Bool(matches!(t.to_ascii_lowercase().as_str(), "true" | "yes" | "1" | "y")),
            FieldType::Text => Value::Text(t.to_string()),
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
        }
    }
}

impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        use std::cmp::Ordering;
        match (self.as_f64(), other.as_f64()) {
            (Some(a), Some(b)) if !matches!(self, Value::Text(_)) || !matches!(other, Value::Text(_)) => a.partial_cmp(&b),
            _ => match (self, other) {
                (Value::Null, Value::Null) => Some(Ordering::Equal),
                (Value::Null, _) => Some(Ordering::Less),
                (_, Value::Null) => Some(Ordering::Greater),
                _ => Some(self.to_string().to_lowercase().cmp(&other.to_string().to_lowercase())),
            },
        }
    }
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
