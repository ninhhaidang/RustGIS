//! Statistics over groups of records, shared by the tools that summarise a table: grouping rows by
//! the values of key fields, and the statistics computed over each group's values of a field.
//!
//! Null values are left out of every statistic except NULLS, which counts them. A statistic over no
//! values is null, except COUNT and UNIQUE (0) and NULLS. Definitions:
//!
//! - STD and VARIANCE are the population ones (divided by n).
//! - MEDIAN of an even count is the mean of the middle two. A percentile `Pk` interpolates linearly
//!   between ranks: at rank (n − 1)·k/100 counted from 0, so P50 is the median.
//! - IQR is P75 − P25; CV is STD / MEAN, and null when MEAN is 0.
//! - MODE and MINORITY are the most and least common values, the lowest of those tied.
//! - FIRST and LAST are the first and last values in record order.
//! - Text compares by character code, so ISO dates sort as dates.

use std::cell::OnceCell;
use std::cmp::Ordering;
use std::collections::HashMap;

use rustgis_data::{Dataset, FieldType, Value};
use serde_json::Value as Json;

/// A statistic of a field's values within a group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stat {
    Count,
    Sum,
    Mean,
    Min,
    Max,
    Range,
    Std,
    Variance,
    Median,
    /// A percentile, 1 to 99.
    Percentile(u8),
    Iqr,
    Cv,
    Mode,
    Minority,
    Unique,
    First,
    Last,
    Concatenate,
    Nulls,
}

/// The statistics with a fixed name, with those names. Percentiles are `P1` to `P99`.
pub(crate) const NAMED: [(&str, Stat); 18] = [
    ("COUNT", Stat::Count),
    ("SUM", Stat::Sum),
    ("MEAN", Stat::Mean),
    ("MIN", Stat::Min),
    ("MAX", Stat::Max),
    ("RANGE", Stat::Range),
    ("STD", Stat::Std),
    ("VARIANCE", Stat::Variance),
    ("MEDIAN", Stat::Median),
    ("IQR", Stat::Iqr),
    ("CV", Stat::Cv),
    ("MODE", Stat::Mode),
    ("MINORITY", Stat::Minority),
    ("UNIQUE", Stat::Unique),
    ("FIRST", Stat::First),
    ("LAST", Stat::Last),
    ("CONCATENATE", Stat::Concatenate),
    ("NULLS", Stat::Nulls),
];

/// The names a statistic can be given, for messages and help.
pub const STAT_NAMES: &str =
    "COUNT, SUM, MEAN, MIN, MAX, RANGE, STD, VARIANCE, MEDIAN, P1 to P99, IQR, CV, MODE, MINORITY, UNIQUE, FIRST, LAST, CONCATENATE, NULLS";

impl Stat {
    /// The statistic a name stands for, in any case: one of [`STAT_NAMES`].
    pub fn parse(name: &str) -> Option<Stat> {
        let name = name.trim();
        if let Some((_, s)) = NAMED.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)) {
            return Some(*s);
        }
        let digits = name.strip_prefix(['P', 'p'])?;
        // Plain digits only: `parse` would also take a sign.
        if digits.is_empty() || digits.len() > 2 || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        digits.parse::<u8>().ok().filter(|k| (1..=99).contains(k)).map(Stat::Percentile)
    }

    /// The statistic's name, upper case, as output field names start with it.
    pub fn name(self) -> String {
        match self {
            Stat::Percentile(k) => format!("P{k}"),
            s => NAMED.iter().find(|(_, n)| *n == s).map(|(n, _)| (*n).to_string()).unwrap_or_default(),
        }
    }

    /// Whether the statistic applies to text values (and to values that aren't numbers, such as
    /// true or false), as well as to numbers.
    pub fn fits_text(self) -> bool {
        matches!(
            self,
            Stat::Count
                | Stat::Min
                | Stat::Max
                | Stat::First
                | Stat::Last
                | Stat::Unique
                | Stat::Concatenate
                | Stat::Mode
                | Stat::Minority
                | Stat::Nulls
        )
    }

    /// The type of the statistic's output field, for a numeric field or another.
    pub fn output_type(self, numeric: bool) -> FieldType {
        match self {
            Stat::Count | Stat::Unique | Stat::Nulls => FieldType::Integer,
            Stat::Concatenate => FieldType::Text,
            _ if numeric => FieldType::Double,
            _ => FieldType::Text,
        }
    }
}

/// The most (field, statistic) pairs a statistics list may hold: each adds an output field and a
/// computation for every group.
pub const MAX_REQUESTS: usize = 256;

/// A field's statistic, as a statistics list names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// The field's index in the input.
    pub field: usize,
    pub stat: Stat,
}

/// Reads a list of (field, statistic) pairs, `[{"field": "income", "statistic": "MEAN"}]`, given as
/// JSON or as its text. Rejects an unknown field or statistic, a statistic that doesn't fit the
/// field's type, and a pair given twice. Nothing given is an empty list.
pub fn parse_requests(ds: &Dataset, v: Option<&Json>) -> Result<Vec<Request>, String> {
    let parsed;
    let list = match v {
        None | Some(Json::Null) => return Ok(Vec::new()),
        Some(Json::String(t)) if t.trim().is_empty() => return Ok(Vec::new()),
        Some(Json::String(t)) => {
            parsed = serde_json::from_str::<Json>(t).map_err(|e| format!("Statistics: {e}"))?;
            &parsed
        }
        Some(other) => other,
    };
    let items = list.as_array().ok_or("Statistics must be a list such as [{\"field\": \"income\", \"statistic\": \"MEAN\"}]")?;
    if items.len() > MAX_REQUESTS {
        return Err(format!("Statistics can list at most {MAX_REQUESTS} pairs (not {})", items.len()));
    }
    let mut out: Vec<Request> = Vec::with_capacity(items.len());
    for item in items {
        let text = |k: &str| item.get(k).and_then(Json::as_str).map(str::trim);
        let name = text("field").filter(|n| !n.is_empty()).ok_or("each statistic needs a \"field\"")?;
        let field = crate::find_field_case_insensitive(ds, name)?;
        let stat_name = text("statistic").unwrap_or("");
        let stat =
            Stat::parse(stat_name).ok_or_else(|| format!("the statistic for '{name}' must be one of {STAT_NAMES} (not '{stat_name}')"))?;
        let numeric = ds.fields.get(field).is_some_and(|f| f.ty.is_numeric());
        if !numeric && !stat.fits_text() {
            return Err(format!("{} needs a numeric field ('{name}' is not)", stat.name()));
        }
        let req = Request { field, stat };
        if out.contains(&req) {
            return Err(format!("{} of '{name}' is listed twice", stat.name()));
        }
        out.push(req);
    }
    Ok(out)
}

/// A group of records sharing the values of the key fields.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    /// The key fields' values, in the order of the key fields.
    pub key: Vec<Value>,
    /// The group's rows, in record order.
    pub rows: Vec<usize>,
}

/// A value as a group key: equal keys are equal values. Numbers compare by their bits, with −0 as
/// 0, so every NaN is one key. Text is borrowed from the dataset.
#[derive(Hash, PartialEq, Eq)]
enum KeyPart<'a> {
    Null,
    Int(i64),
    Float(u64),
    Text(&'a str),
    Bool(bool),
}

/// A value's group key.
fn key_part(v: &Value) -> KeyPart<'_> {
    match v {
        Value::Null => KeyPart::Null,
        Value::Int(i) => KeyPart::Int(*i),
        Value::Float(f) if f.is_nan() => KeyPart::Float(f64::NAN.to_bits()),
        Value::Float(f) => KeyPart::Float(if *f == 0.0 { 0.0f64.to_bits() } else { f.to_bits() }),
        Value::Text(t) => KeyPart::Text(t),
        Value::Bool(b) => KeyPart::Bool(*b),
    }
}

/// Groups `rows` by the values of the `keys` fields, sorted by those values (nulls first). Null
/// values form their own group, and −0 is 0. With no key fields, every row is one group, and no
/// rows are no groups.
pub fn group_rows(ds: &Dataset, rows: &[usize], keys: &[usize]) -> Vec<Group> {
    let mut index: HashMap<Vec<KeyPart<'_>>, usize> = HashMap::new();
    let mut groups: Vec<Group> = Vec::new();
    for &r in rows {
        let hashed: Vec<KeyPart> = keys.iter().map(|&k| key_part(ds.value(r, k))).collect();
        // The key's values are copied only for a new group (its hash borrows them).
        let g = match index.get(&hashed) {
            Some(&g) => g,
            None => {
                // −0 as 0 and one NaN, as they group.
                let key: Vec<Value> = keys
                    .iter()
                    .map(|&k| match ds.value(r, k) {
                        Value::Float(f) if f.is_nan() => Value::Float(f64::NAN),
                        Value::Float(f) => Value::Float(f + 0.0),
                        v => v.clone(),
                    })
                    .collect();
                groups.push(Group { key, rows: Vec::new() });
                index.insert(hashed, groups.len() - 1);
                groups.len() - 1
            }
        };
        if let Some(g) = groups.get_mut(g) {
            g.rows.push(r);
        }
    }
    groups.sort_by(|a, b| cmp_keys(&a.key, &b.key));
    groups
}

/// Orders group keys value by value: nulls, then numbers (true and false as 1 and 0) by value,
/// then text by its lower case and then by character code. Numbers equal in value are ordered by
/// kind (1 before 1.0). This is a total order, whatever mix of kinds a field holds (text that reads
/// as a number is still text), so the order never depends on the records'.
fn cmp_keys(a: &[Value], b: &[Value]) -> Ordering {
    a.iter().zip(b).map(|(x, y)| cmp_key(x, y)).find(|o| o.is_ne()).unwrap_or(Ordering::Equal)
}

/// Orders two group key values (see [`cmp_keys`]).
fn cmp_key(x: &Value, y: &Value) -> Ordering {
    /// A number's kind, the tie-break between equal values: true/false, then whole, then decimal.
    fn kind(v: &Value) -> u8 {
        match v {
            Value::Bool(_) => 0,
            Value::Int(_) => 1,
            _ => 2,
        }
    }
    let rank = |v: &Value| match v {
        Value::Null => 0,
        Value::Text(_) => 2,
        _ => 1,
    };
    rank(x).cmp(&rank(y)).then_with(|| match (x, y) {
        (Value::Text(a), Value::Text(b)) => {
            a.chars().flat_map(char::to_lowercase).cmp(b.chars().flat_map(char::to_lowercase)).then_with(|| a.cmp(b))
        }
        (Value::Null, Value::Null) => Ordering::Equal,
        _ => cmp_number(x, y).then_with(|| kind(x).cmp(&kind(y))),
    })
}

/// Orders two numbers (true and false as 1 and 0) by value, exactly: a whole number against a
/// decimal without rounding it to f64. Every NaN is one value, above all others; −0 is 0.
fn cmp_number(x: &Value, y: &Value) -> Ordering {
    /// A number as a whole number or a decimal.
    enum N {
        I(i64),
        F(f64),
    }
    let n = |v: &Value| match v {
        Value::Bool(b) => N::I(i64::from(*b)),
        Value::Int(i) => N::I(*i),
        Value::Float(f) if f.is_nan() => N::F(f64::NAN),
        Value::Float(f) => N::F(f + 0.0),
        Value::Null | Value::Text(_) => N::F(f64::NAN),
    };
    match (n(x), n(y)) {
        (N::I(a), N::I(b)) => a.cmp(&b),
        (N::F(a), N::F(b)) => a.total_cmp(&b),
        (N::I(a), N::F(b)) => cmp_int_float(a, b),
        (N::F(a), N::I(b)) => cmp_int_float(b, a).reverse(),
    }
}

/// Orders a whole number against a decimal exactly; NaN is above every number.
fn cmp_int_float(i: i64, f: f64) -> Ordering {
    // 2^63: no i64 reaches it, every i64 is at least its negative.
    const TWO_63: f64 = 9_223_372_036_854_775_808.0;
    if f.is_nan() || f >= TWO_63 {
        return Ordering::Less;
    }
    if f < -TWO_63 {
        return Ordering::Greater;
    }
    let t = f.trunc();
    // In range, so the truncated decimal is a whole number an i64 holds exactly.
    i.cmp(&(t as i64)).then_with(|| {
        if f > t {
            Ordering::Less
        } else if f < t {
            Ordering::Greater
        } else {
            Ordering::Equal
        }
    })
}

/// One field's values within a group, without its nulls, in record order.
#[derive(Clone, Debug, PartialEq)]
pub enum Values {
    Numbers(Vec<f64>),
    Texts(Vec<String>),
}

/// A field's values within a group, and how many of its records have none.
#[derive(Clone, Debug)]
pub struct Column {
    values: Values,
    nulls: usize,
    /// Beside each number, in the same order, the whole number it was read from, exactly: past
    /// 2^53 whole numbers can differ where their f64s don't. Empty when there are none.
    whole: Vec<Option<i64>>,
    /// How many of the numbers were read from a whole number written as text, in digits, past the
    /// i64 range, as a decimal.
    rounded_texts: usize,
    /// The values in ascending order, sorted once, and only for a statistic that needs them so.
    sorted: OnceCell<Values>,
    /// With whole numbers among the values, all of them in ascending order compared exactly.
    exact: OnceCell<Vec<Value>>,
}

/// Columns are equal when their values are: the sorted caches don't count.
impl PartialEq for Column {
    fn eq(&self, other: &Column) -> bool {
        self.values == other.values && self.nulls == other.nulls && self.whole == other.whole
    }
}

impl Column {
    /// A column of `values`, in record order, and `nulls` records without one.
    pub fn new(values: Values, nulls: usize) -> Column {
        Column { values, nulls, whole: Vec::new(), rounded_texts: 0, sorted: OnceCell::new(), exact: OnceCell::new() }
    }

    /// How many values were read from a whole number written as text, in digits, past the i64
    /// range, as a decimal: such a value is past what the checks take exactly, so the statistics
    /// of it aren't checked and the tool reports it instead. (One written with a decimal point or
    /// an exponent asks for a decimal, and is one.)
    pub fn rounded_texts(&self) -> usize {
        self.rounded_texts
    }

    /// The values in ascending order.
    fn sorted(&self) -> &Values {
        self.sorted.get_or_init(|| match &self.values {
            Values::Numbers(v) => {
                let mut s = v.clone();
                s.sort_by(f64::total_cmp);
                Values::Numbers(s)
            }
            Values::Texts(v) => {
                let mut s = v.clone();
                s.sort_unstable();
                Values::Texts(s)
            }
        })
    }

    /// With whole numbers among the values, all of them sorted and compared exactly: whole numbers
    /// as themselves, decimals against them without rounding. Past 2^53 whole numbers can differ
    /// where their f64s don't.
    fn exact(&self) -> Option<&[Value]> {
        let Values::Numbers(v) = &self.values else { return None };
        if self.whole.is_empty() {
            return None;
        }
        let exact = self.exact.get_or_init(|| {
            let mut e: Vec<Value> = v.iter().zip(&self.whole).map(|(x, w)| w.map_or(Value::Float(*x), Value::Int)).collect();
            e.sort_by(cmp_number);
            e
        });
        Some(exact)
    }

    /// How many distinct values there are.
    fn distinct(&self) -> usize {
        if let Some(e) = self.exact() {
            return e.windows(2).filter(|w| matches!(w, [a, b] if cmp_number(a, b).is_ne())).count() + usize::from(!e.is_empty());
        }
        match self.sorted() {
            Values::Numbers(s) => distinct(s),
            Values::Texts(s) => distinct(s),
        }
    }
}

/// A field's values in `rows`. A numeric field, or any field read `as_numbers`, gives numbers (a
/// value that isn't a finite number counts as null); another gives each value's text.
pub fn column(ds: &Dataset, rows: &[usize], field: usize, as_numbers: bool) -> Column {
    let numeric = as_numbers || ds.fields.get(field).is_some_and(|f| f.ty.is_numeric());
    let mut nulls = 0;
    if numeric {
        let mut out = Vec::with_capacity(rows.len());
        let mut whole = Vec::with_capacity(rows.len());
        let mut rounded_texts = 0;
        for &r in rows {
            let v = ds.value(r, field);
            match v.as_f64() {
                Some(x) => {
                    out.push(x);
                    // A whole number written as text, read as a number, is kept exactly too; one
                    // past the i64 range is the decimal, counted when it was written as digits
                    // (asking for a whole number), whether or not the decimal holds it: the
                    // statistics of it aren't checked.
                    whole.push(match v {
                        Value::Int(i) => Some(*i),
                        Value::Text(t) => {
                            let written = whole_text(t);
                            let exact = written.as_ref().and_then(WholeText::to_i64);
                            if exact.is_none() && written.is_some_and(|w| w.plain) {
                                rounded_texts += 1;
                            }
                            exact
                        }
                        _ => None,
                    });
                }
                None => nulls += 1,
            }
        }
        let mut column = Column::new(Values::Numbers(out), nulls);
        if whole.iter().any(Option::is_some) {
            column.whole = whole;
        }
        column.rounded_texts = rounded_texts;
        column
    } else {
        let mut out = Vec::with_capacity(rows.len());
        for &r in rows {
            match ds.value(r, field) {
                Value::Null => nulls += 1,
                v => out.push(v.to_string()),
            }
        }
        Column::new(Values::Texts(out), nulls)
    }
}

/// A count as a value.
pub fn count(n: usize) -> Value {
    Value::Int(i64::try_from(n).unwrap_or(i64::MAX))
}

/// Computes `stat` over a column. `separator` joins CONCATENATE's values. A statistic that doesn't
/// fit text is null over text.
pub fn compute(stat: Stat, column: &Column, separator: &str) -> Value {
    match (stat, &column.values) {
        (Stat::Nulls, _) => count(column.nulls),
        (Stat::Count, Values::Numbers(v)) => count(v.len()),
        (Stat::Count, Values::Texts(v)) => count(v.len()),
        (Stat::Concatenate, Values::Numbers(v)) if !v.is_empty() => {
            let text = |(i, x): (usize, &f64)| match column.whole.get(i) {
                Some(Some(w)) => w.to_string(),
                _ => number_text(*x),
            };
            Value::Text(v.iter().enumerate().map(text).collect::<Vec<_>>().join(separator))
        }
        (Stat::Concatenate, Values::Texts(v)) if !v.is_empty() => Value::Text(v.join(separator)),
        (Stat::Unique, _) => count(column.distinct()),
        (_, Values::Numbers(v)) => {
            // MODE and MINORITY tell whole numbers apart exactly, as UNIQUE does.
            if let (Stat::Mode | Stat::Minority, Some(e)) = (stat, column.exact()) {
                return number(pick_run_by(e, stat == Stat::Mode, |a, b| cmp_number(a, b).is_eq()).and_then(Value::as_f64));
            }
            let sorted = || match column.sorted() {
                Values::Numbers(s) => s.as_slice(),
                Values::Texts(_) => &[],
            };
            number(numeric(stat, v, sorted))
        }
        (_, Values::Texts(v)) => {
            let sorted = || match column.sorted() {
                Values::Texts(s) => s.as_slice(),
                Values::Numbers(_) => &[],
            };
            textual(stat, v, sorted).map_or(Value::Null, |t| Value::Text(t.to_string()))
        }
    }
}

/// Whether `value`, [`compute`]'s result for `stat` over `column`, is rounded from a whole number
/// it should be exactly: a decimal holds every whole number only up to 2^53. That is a statistic
/// whose exact value is whole, worked out from the whole numbers the column was read from: SUM and
/// RANGE of whole numbers, a MEAN of them that divides evenly, the MIN, MAX, FIRST, LAST, MODE or
/// MINORITY picked, and a whole MEDIAN, percentile or IQR of values past 2^53. Other means, STD,
/// VARIANCE, CV and percentiles that aren't whole are decimals anyway, and counts and CONCATENATE
/// (which writes whole numbers as they are) aren't rounded.
pub fn rounded(stat: Stat, column: &Column, value: &Value) -> bool {
    let Value::Float(x) = value else { return false };
    let Values::Numbers(numbers) = &column.values else { return false };
    if column.whole.is_empty() {
        return false;
    }
    let differs = |w: i128| !(x.fract() == 0.0 && *x as i128 == w);
    // Each value as the whole number it is exactly: one read from a whole number, or a decimal (or
    // true or false) with no fraction, which holds its whole number exactly.
    let exact_ints = || numbers.iter().zip(&column.whole).map(|(n, w)| w.map(i128::from).or_else(|| decimal_whole(*n)));
    let wholes = || exact_ints().flatten();
    let all_whole = || exact_ints().all(|w| w.is_some());
    // Percentiles are checked when a value they lie on or between is past 2^53, where the limit
    // is: within it, interpolating has rounding of its own, as any decimal arithmetic does.
    let past = |v: i128| v.unsigned_abs() > 1 << 53;
    // The `k`th percentile worked out exactly, as [`percentile`] interpolates it between ranks of
    // the whole numbers: its value times 100, and whether a value it lies on or between is past
    // 2^53. None over decimals.
    let exact_percentile = |k: i128| -> Option<(i128, bool)> {
        let e = column.exact()?;
        let whole = |i: i128| match e.get(usize::try_from(i).ok()?)? {
            Value::Int(w) => Some(i128::from(*w)),
            Value::Float(f) => decimal_whole(*f),
            _ => None,
        };
        let at = (i128::try_from(e.len()).ok()? - 1) * k;
        let (lo, rem) = (at / 100, at % 100);
        let a = whole(lo)?;
        if rem == 0 {
            return Some((a * 100, past(a)));
        }
        let b = whole(lo + 1)?;
        Some((a * (100 - rem) + b * rem, past(a) || past(b)))
    };
    // A whole value times 100, compared with the result.
    let whole_differs = |hundredfold: i128| hundredfold % 100 == 0 && differs(hundredfold / 100);
    // Without a value past 2^53 no percentile is checked, and the column needn't be sorted.
    let any_past = || wholes().any(past);
    let percentile_rounded = |k: i128| any_past() && exact_percentile(k).is_some_and(|(v, near)| near && whole_differs(v));
    // Rounding keeps order, so a value within 2^53 was picked from one within it, exactly: no need
    // to sort the column.
    let picked_within = x.abs() < TWO_53;
    match stat {
        Stat::Sum => all_whole() && differs(wholes().sum()),
        Stat::Range => all_whole() && wholes().min().zip(wholes().max()).is_some_and(|(lo, hi)| differs(hi - lo)),
        Stat::First | Stat::Last => {
            let w = if stat == Stat::First { column.whole.first() } else { column.whole.last() };
            matches!(w, Some(Some(w)) if differs(i128::from(*w)))
        }
        Stat::Min | Stat::Max | Stat::Mode | Stat::Minority => {
            if picked_within {
                return false;
            }
            let picked = match stat {
                // One pass, compared exactly, as [`Column::exact`] would sort them.
                Stat::Min | Stat::Max => {
                    let exact = numbers.iter().zip(&column.whole).map(|(n, w)| w.map_or(Value::Float(*n), Value::Int));
                    if stat == Stat::Min { exact.min_by(cmp_number) } else { exact.max_by(cmp_number) }
                }
                // MODE and MINORITY have sorted the column already.
                _ => column.exact().and_then(|e| pick_run_by(e, stat == Stat::Mode, |a, b| cmp_number(a, b).is_eq())).cloned(),
            };
            matches!(picked, Some(Value::Int(w)) if differs(i128::from(w)))
        }
        Stat::Median => percentile_rounded(50),
        Stat::Percentile(k) => percentile_rounded(i128::from(k)),
        // A difference of percentiles, whole even when they aren't.
        Stat::Iqr => {
            any_past()
                && exact_percentile(75)
                    .zip(exact_percentile(25))
                    .is_some_and(|((hi, near_hi), (lo, near_lo))| (near_hi || near_lo) && whole_differs(hi - lo))
        }
        // A MEAN of whole numbers is whole when their sum divides by their count.
        Stat::Mean => {
            let n = i128::try_from(column.whole.len()).unwrap_or(i128::MAX);
            all_whole() && n > 0 && {
                let sum: i128 = wholes().sum();
                sum % n == 0 && differs(sum / n)
            }
        }
        Stat::Std | Stat::Variance | Stat::Cv | Stat::Count | Stat::Unique | Stat::Nulls | Stat::Concatenate => false,
    }
}

/// The whole number a decimal with no fraction is, exactly; None for one with a fraction, or past
/// the i64 range the whole numbers are read in. Bounded so, sums and differences of any count of
/// values stay far within an i128.
fn decimal_whole(x: f64) -> Option<i128> {
    const I64_LIMIT: f64 = 9_223_372_036_854_775_808.0;
    (x.fract() == 0.0 && (-I64_LIMIT..I64_LIMIT).contains(&x)).then_some(x as i128)
}

/// 2^53, up to which a decimal holds every whole number.
const TWO_53: f64 = 9_007_199_254_740_992.0;

/// A whole number written as text, as its digits.
#[derive(Debug, PartialEq)]
struct WholeText {
    /// The number's decimal digits, with its sign when negative and no leading zero: what the
    /// text stands for, exactly, however it is written.
    digits: String,
    /// Whether it is written as digits alone (a sign apart), as a whole number is, rather than
    /// with a decimal point or an exponent, as a decimal is.
    plain: bool,
}

impl WholeText {
    /// The number, when it fits an i64.
    fn to_i64(&self) -> Option<i64> {
        self.digits.parse().ok()
    }
}

/// The most digits a whole number written as text is read with. Any finite decimal has fewer (the
/// largest has 309), so the bound only keeps a text with a huge exponent, which the decimal reader
/// rejects anyway, from allocating for its zeros.
const MAX_WHOLE_DIGITS: usize = 400;

/// The whole number a number written as text stands for, in the forms the decimal reader takes
/// ("12", "+12", "12.0", "1.2e1", "1200e-2"); None for text that isn't a whole number.
fn whole_text(t: &str) -> Option<WholeText> {
    let t = t.trim();
    let (negative, t) = match t.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let (mantissa, exponent) = t.split_once(['e', 'E']).unwrap_or((t, "0"));
    let (int, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let exponent_digits = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
    let all_digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if (int.is_empty() && frac.is_empty())
        || !all_digits(int)
        || !all_digits(frac)
        || exponent_digits.is_empty()
        || !all_digits(exponent_digits)
    {
        return None;
    }
    let joined = format!("{int}{frac}");
    let digits = joined.trim_start_matches('0');
    let plain = frac.is_empty() && !mantissa.contains('.') && !t.contains(['e', 'E']);
    // Zero is zero whatever power of ten it is written with.
    if digits.is_empty() {
        return Some(WholeText { digits: "0".to_string(), plain });
    }
    let exponent: i32 = exponent.parse().ok()?;
    // The power of ten the digits are multiplied by.
    let scale = i64::from(exponent) - i64::try_from(frac.len()).ok()?;
    let whole: String = if scale < 0 {
        // The last digits are the fraction: all zeros for a whole number.
        let cut = digits.len().saturating_sub(usize::try_from(scale.unsigned_abs()).ok()?);
        if !digits.get(cut..)?.bytes().all(|b| b == b'0') {
            return None;
        }
        digits.get(..cut)?.to_string()
    } else {
        let zeros = usize::try_from(scale).ok().filter(|z| digits.len() + z <= MAX_WHOLE_DIGITS)?;
        format!("{digits}{}", "0".repeat(zeros))
    };
    // Never empty: the digits start with one that isn't 0, which the fraction test keeps.
    Some(WholeText { digits: if negative { format!("-{whole}") } else { whole }, plain })
}

/// How many distinct values sorted `s` holds.
fn distinct<T: PartialEq>(s: &[T]) -> usize {
    s.windows(2).filter(|w| matches!(w, [a, b] if a != b)).count() + usize::from(!s.is_empty())
}

/// A number as an output value: null unless finite, and −0 as 0.
fn number(x: Option<f64>) -> Value {
    x.filter(|x| x.is_finite()).map_or(Value::Null, |x| Value::Float(x + 0.0))
}

/// A number's text in a concatenation: whole numbers without a decimal point, others in full; −0
/// as 0, as grouping treats it.
fn number_text(x: f64) -> String {
    let x = x + 0.0;
    if x.fract() == 0.0 && x.abs() < 1e15 { format!("{x:.0}") } else { x.to_string() }
}

/// A numeric statistic over numbers `v`, in record order; `sorted` gives them sorted, for the
/// statistics that need it. `None` over no numbers.
fn numeric<'a>(stat: Stat, v: &[f64], sorted: impl Fn() -> &'a [f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    let n = v.len() as f64;
    let mean = || v.iter().sum::<f64>() / n;
    let variance = || {
        let m = mean();
        v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / n
    };
    let min = || v.iter().copied().fold(f64::INFINITY, f64::min);
    let max = || v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    match stat {
        Stat::Sum => Some(v.iter().sum()),
        Stat::Mean => Some(mean()),
        Stat::Min => Some(min()),
        Stat::Max => Some(max()),
        Stat::Range => Some(max() - min()),
        Stat::Std => Some(variance().sqrt()),
        Stat::Variance => Some(variance()),
        Stat::Median => percentile(sorted(), 50.0),
        Stat::Percentile(k) => percentile(sorted(), f64::from(k)),
        Stat::Iqr => Some(percentile(sorted(), 75.0)? - percentile(sorted(), 25.0)?),
        Stat::Cv => {
            let m = mean();
            (m != 0.0).then(|| variance().sqrt() / m)
        }
        Stat::Mode | Stat::Minority => pick_run(sorted(), stat == Stat::Mode).copied(),
        Stat::First => v.first().copied(),
        Stat::Last => v.last().copied(),
        Stat::Count | Stat::Unique | Stat::Concatenate | Stat::Nulls => None,
    }
}

/// A text statistic over texts `v`, in record order; `sorted` gives them sorted, for the
/// statistics that need it. `None` over no texts, or for a statistic that needs numbers.
fn textual<'a>(stat: Stat, v: &'a [String], sorted: impl Fn() -> &'a [String]) -> Option<&'a str> {
    match stat {
        Stat::Min => v.iter().map(String::as_str).min(),
        Stat::Max => v.iter().map(String::as_str).max(),
        Stat::First => v.first().map(String::as_str),
        Stat::Last => v.last().map(String::as_str),
        Stat::Mode | Stat::Minority => pick_run(sorted(), stat == Stat::Mode).map(String::as_str),
        _ => None,
    }
}

/// The value of the longest run of equal values in sorted `s` (`most`), or of the shortest: the
/// first such run, so the lowest value on a tie.
fn pick_run<T: PartialEq>(s: &[T], most: bool) -> Option<&T> {
    pick_run_by(s, most, |a, b| a == b)
}

/// [`pick_run`] with the values' equality given by `eq`.
fn pick_run_by<T>(s: &[T], most: bool, eq: impl Fn(&T, &T) -> bool) -> Option<&T> {
    let mut best: Option<(&T, usize)> = None;
    let mut i = 0;
    while let Some(first) = s.get(i) {
        let len = s.get(i..).map_or(0, |rest| rest.iter().take_while(|x| eq(x, first)).count()).max(1);
        let better = best.is_none_or(|(_, n)| if most { len > n } else { len < n });
        if better {
            best = Some((first, len));
        }
        i += len;
    }
    best.map(|(v, _)| v)
}

/// The `k`th percentile of sorted numbers, interpolating linearly between ranks.
fn percentile(s: &[f64], k: f64) -> Option<f64> {
    let last = s.len().checked_sub(1)?;
    let rank = last as f64 * k / 100.0;
    let lo = rank.floor() as usize;
    let a = *s.get(lo)?;
    let b = *s.get(lo + 1).unwrap_or(&a);
    // Weighted rather than a + (b − a)·t, whose b − a overflows for extremes of opposite sign.
    // Equal neighbours are the answer as they are: the weights' products could round a subnormal
    // to 0.
    if a == b {
        return Some(a);
    }
    let t = rank - lo as f64;
    Some(a * (1.0 - t) + b * t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_data::{Field, GeometryType};

    /// Numbers as a group's values.
    fn nums(v: &[f64]) -> Values {
        Values::Numbers(v.to_vec())
    }

    /// Texts as a group's values.
    fn texts(v: &[&str]) -> Values {
        Values::Texts(v.iter().map(|t| (*t).to_string()).collect())
    }

    /// The named statistic over values with no nulls, joined by ", ".
    fn f(stat: &str, v: &Values) -> Value {
        compute(Stat::parse(stat).unwrap(), &Column::new(v.clone(), 0), ", ")
    }

    /// Every named statistic and the percentiles read back under their own names, in any case, and
    /// are listed in [`STAT_NAMES`]; out-of-range percentiles and other text are not statistics.
    #[test]
    fn names_round_trip() {
        for (name, stat) in NAMED {
            assert!(STAT_NAMES.split(", ").any(|n| n == name), "{name} is missing from STAT_NAMES");
            assert_eq!(Stat::parse(&name.to_lowercase()), Some(stat));
            assert_eq!(stat.name(), name);
        }
        assert_eq!(Stat::parse("p90"), Some(Stat::Percentile(90)));
        assert_eq!(Stat::Percentile(5).name(), "P5");
        for bad in ["P0", "P100", "P", "P+5", "P05x", "PERCENTILE", "", "avg"] {
            assert_eq!(Stat::parse(bad), None, "{bad}");
        }
    }

    /// The numeric statistics over a small known sample, checked against hand-computed values.
    #[test]
    fn numeric_statistics() {
        let v = nums(&[4.0, 1.0, 3.0, 1.0, 6.0]);
        let expect = [
            ("COUNT", Value::Int(5)),
            ("SUM", Value::Float(15.0)),
            ("MEAN", Value::Float(3.0)),
            ("MIN", Value::Float(1.0)),
            ("MAX", Value::Float(6.0)),
            ("RANGE", Value::Float(5.0)),
            ("VARIANCE", Value::Float(3.6)),
            ("MEDIAN", Value::Float(3.0)),
            ("P25", Value::Float(1.0)),
            ("P75", Value::Float(4.0)),
            ("P90", Value::Float(5.2)),
            ("IQR", Value::Float(3.0)),
            ("MODE", Value::Float(1.0)),
            ("MINORITY", Value::Float(3.0)),
            ("UNIQUE", Value::Int(4)),
            ("FIRST", Value::Float(4.0)),
            ("LAST", Value::Float(6.0)),
            ("CONCATENATE", Value::Text("4, 1, 3, 1, 6".into())),
        ];
        for (name, want) in expect {
            match (f(name, &v), &want) {
                (Value::Float(a), Value::Float(b)) => assert!((a - b).abs() < 1e-12, "{name}: {a} ≠ {b}"),
                (got, _) => assert_eq!(got, want, "{name}"),
            }
        }
        let Value::Float(sd) = f("STD", &v) else { panic!("STD is a number") };
        assert!((sd - 3.6f64.sqrt()).abs() < 1e-12);
        let Value::Float(cv) = f("CV", &v) else { panic!("CV is a number") };
        assert!((cv - 3.6f64.sqrt() / 3.0).abs() < 1e-12);
    }

    /// An even count's median is the mean of the middle two, and P50 agrees with it; one value is
    /// every percentile.
    #[test]
    fn median_and_percentiles_interpolate() {
        let v = nums(&[10.0, 40.0, 20.0, 30.0]);
        assert_eq!(f("MEDIAN", &v), Value::Float(25.0));
        assert_eq!(f("P50", &v), Value::Float(25.0));
        assert_eq!(f("P1", &nums(&[7.0])), Value::Float(7.0));
        assert_eq!(f("P99", &nums(&[7.0])), Value::Float(7.0));
    }

    /// MODE and MINORITY break a tie with the lowest value, for numbers and for text.
    #[test]
    fn ties_take_the_lowest_value() {
        assert_eq!(f("MODE", &nums(&[3.0, 2.0, 3.0, 2.0])), Value::Float(2.0));
        assert_eq!(f("MINORITY", &nums(&[5.0, 9.0, 9.0, 7.0])), Value::Float(5.0));
        let t = texts(&["b", "a", "b", "a", "c"]);
        assert_eq!(f("MODE", &t), Value::Text("a".into()));
        assert_eq!(f("MINORITY", &t), Value::Text("c".into()));
    }

    /// Text statistics compare by character code, so ISO dates order as dates; the numeric ones are
    /// null over text.
    #[test]
    fn text_statistics() {
        let t = texts(&["2024-03-01", "2023-12-31", "2024-01-15"]);
        assert_eq!(f("MIN", &t), Value::Text("2023-12-31".into()));
        assert_eq!(f("MAX", &t), Value::Text("2024-03-01".into()));
        assert_eq!(f("FIRST", &t), Value::Text("2024-03-01".into()));
        assert_eq!(f("LAST", &t), Value::Text("2024-01-15".into()));
        assert_eq!(f("UNIQUE", &texts(&["a", "b", "a"])), Value::Int(2));
        assert_eq!(compute(Stat::Concatenate, &Column::new(texts(&["a", "b"]), 0), "|"), Value::Text("a|b".into()));
        assert_eq!(f("MEAN", &t), Value::Null);
    }

    /// Over no values a statistic is null, except the counts; NULLS counts the nulls; CV is null
    /// when the mean is 0.
    #[test]
    fn empty_groups_are_null_not_zero() {
        for v in [nums(&[]), texts(&[])] {
            for (name, _) in NAMED {
                let got = compute(Stat::parse(name).unwrap(), &Column::new(v.clone(), 3), ", ");
                let want = match name {
                    "COUNT" | "UNIQUE" => Value::Int(0),
                    "NULLS" => Value::Int(3),
                    _ => Value::Null,
                };
                assert_eq!(got, want, "{name} over {v:?}");
            }
        }
        assert_eq!(f("CV", &nums(&[-1.0, 1.0])), Value::Null);
    }

    /// A table of numbers `v` and texts `k`.
    fn table(values: &[(Value, &str)]) -> Dataset {
        let mut ds = Dataset::new("t", GeometryType::None, vec![Field::new("v", FieldType::Double), Field::new("k", FieldType::Text)]);
        for (v, k) in values {
            ds.push(None, vec![v.clone(), Value::Text((*k).into())]);
        }
        ds
    }

    /// Groups are sorted by key with nulls first, keep their rows in record order, and treat −0 as
    /// 0; no key fields put every row in one group, and no rows give no groups.
    #[test]
    fn grouping() {
        let ds = table(&[
            (Value::Float(2.0), "b"),
            (Value::Null, "a"),
            (Value::Float(0.0), "b"),
            (Value::Float(-0.0), "a"),
            (Value::Float(2.0), "a"),
        ]);
        let all: Vec<usize> = (0..ds.features.len()).collect();
        let g = group_rows(&ds, &all, &[0]);
        assert_eq!(
            g.iter().map(|g| g.key.clone()).collect::<Vec<_>>(),
            [vec![Value::Null], vec![Value::Float(0.0)], vec![Value::Float(2.0)]]
        );
        assert_eq!(g.iter().map(|g| g.rows.clone()).collect::<Vec<_>>(), [vec![1], vec![2, 3], vec![0, 4]]);
        let two = group_rows(&ds, &all, &[1, 0]);
        assert_eq!(two.first().map(|g| g.key.clone()), Some(vec![Value::Text("a".into()), Value::Null]));
        assert_eq!(two.len(), 5);
        assert_eq!(group_rows(&ds, &all, &[]).len(), 1);
        assert!(group_rows(&ds, &[], &[]).is_empty());
        assert_eq!(column(&ds, &all, 0, false), Column::new(nums(&[2.0, 0.0, -0.0, 2.0]), 1));
        // Text read as numbers: what isn't a number is null.
        assert_eq!(column(&ds, &all, 1, true), Column::new(nums(&[]), 5));
        // Keys equal but for case are ordered by character code, whatever the record order.
        let mut cased = table(&[(Value::Null, "b"), (Value::Null, "B")]);
        let both = [0, 1];
        let order = |ds: &Dataset| group_rows(ds, &both, &[1]).into_iter().map(|g| g.key).collect::<Vec<_>>();
        let want = vec![vec![Value::Text("B".into())], vec![Value::Text("b".into())]];
        assert_eq!(order(&cased), want);
        cased.features.reverse();
        assert_eq!(order(&cased), want);
        // So are 1 and 1.0, which display alike: the whole number first.
        let mut mixed = table(&[(Value::Float(1.0), ""), (Value::Int(1), "")]);
        let want = vec![vec![Value::Int(1)], vec![Value::Float(1.0)]];
        let order = |ds: &Dataset| group_rows(ds, &both, &[0]).into_iter().map(|g| g.key).collect::<Vec<_>>();
        assert_eq!(order(&mixed), want);
        mixed.features.reverse();
        assert_eq!(order(&mixed), want);
        // Text and numbers mixed in one field still sort totally: numbers by value, then text.
        let mut mix = Dataset::new("t", GeometryType::None, vec![Field::new("k", FieldType::Text)]);
        for v in [Value::Text("9".into()), Value::Float(9.5), Value::Text("10".into()), Value::Int(9), Value::Bool(true)] {
            mix.push(None, vec![v]);
        }
        let all5 = [0, 1, 2, 3, 4];
        let order = |ds: &Dataset| group_rows(ds, &all5, &[0]).into_iter().map(|g| g.key).collect::<Vec<_>>();
        let want: Vec<Vec<Value>> =
            [Value::Bool(true), Value::Int(9), Value::Float(9.5), Value::Text("10".into()), Value::Text("9".into())]
                .into_iter()
                .map(|v| vec![v])
                .collect();
        assert_eq!(order(&mix), want);
        mix.features.reverse();
        assert_eq!(order(&mix), want);
        // A group first met as −0 is keyed 0.
        let neg = table(&[(Value::Float(-0.0), ""), (Value::Float(0.0), "")]);
        let key = group_rows(&neg, &[0, 1], &[0]).into_iter().next().and_then(|g| g.key.into_iter().next());
        assert!(matches!(key, Some(Value::Float(z)) if z.to_bits() == 0), "{key:?}");
        // Whole numbers and decimals compare exactly past 2^53, and NaNs of either sign are one
        // group, after the numbers.
        let big = (1i64 << 53) + 1;
        assert_eq!(cmp_key(&Value::Int(big), &Value::Float(9_007_199_254_740_992.0)), Ordering::Greater);
        let mixed_big = table(&[(Value::Int(big), ""), (Value::Float(9_007_199_254_740_992.0), "")]);
        let order: Vec<Vec<Value>> = group_rows(&mixed_big, &[0, 1], &[0]).into_iter().map(|g| g.key).collect();
        assert_eq!(order, vec![vec![Value::Float(9_007_199_254_740_992.0)], vec![Value::Int(big)]]);
        assert_eq!(cmp_key(&Value::Int(i64::MAX), &Value::Float(9.3e18)), Ordering::Less);
        assert_eq!(cmp_key(&Value::Int(i64::MIN), &Value::Float(f64::NEG_INFINITY)), Ordering::Greater);
        assert_eq!(cmp_key(&Value::Int(3), &Value::Float(3.5)), Ordering::Less);
        assert_eq!(cmp_key(&Value::Int(-3), &Value::Float(-3.5)), Ordering::Greater);
        let nans = table(&[(Value::Float(-f64::NAN), ""), (Value::Float(f64::INFINITY), ""), (Value::Float(f64::NAN), "")]);
        let keys: Vec<Vec<Value>> = group_rows(&nans, &[0, 1, 2], &[0]).into_iter().map(|g| g.key).collect();
        assert_eq!(keys.len(), 2);
        assert_eq!(keys.first(), Some(&vec![Value::Float(f64::INFINITY)]));
        assert!(matches!(keys.get(1).and_then(|k| k.first()), Some(Value::Float(n)) if n.to_bits() == f64::NAN.to_bits()));
        // Every NaN is one key, whatever its sign and payload.
        let nan = table(&[
            (Value::Float(f64::NAN), ""),
            (Value::Float(-f64::NAN), ""),
            (Value::Float(f64::from_bits(0x7ff8_0000_0000_0001)), ""),
        ]);
        assert_eq!(group_rows(&nan, &[0, 1, 2], &[0]).len(), 1);
    }

    /// UNIQUE counts whole numbers exactly, past where f64 tells them apart; percentiles between
    /// extremes of opposite sign don't overflow.
    #[test]
    fn large_values() {
        let mut ds = Dataset::new("t", GeometryType::None, vec![Field::new("n", FieldType::Integer)]);
        for i in [1i64 << 53, (1 << 53) + 1, (1 << 53) + 1] {
            ds.push(None, vec![Value::Int(i)]);
        }
        assert_eq!(compute(Stat::Unique, &column(&ds, &[0, 1, 2], 0, false), ", "), Value::Int(2));
        assert_eq!(f("MEDIAN", &nums(&[-1e308, 1e308])), Value::Float(0.0));
        assert_eq!(f("P75", &nums(&[-1e308, 1e308])), Value::Float(5e307));
    }

    /// CONCATENATE writes whole numbers exactly, past 2^53; a percentile between equal subnormals
    /// is that subnormal.
    #[test]
    fn exact_extremes() {
        let mut ds = Dataset::new("t", GeometryType::None, vec![Field::new("n", FieldType::Integer)]);
        for i in [1i64 << 53, (1 << 53) + 1, (1 << 53) + 1] {
            ds.push(None, vec![Value::Int(i)]);
        }
        let want = Value::Text("9007199254740992, 9007199254740993, 9007199254740993".into());
        assert_eq!(compute(Stat::Concatenate, &column(&ds, &[0, 1, 2], 0, false), ", "), want);
        // A column mixing whole numbers and decimals writes its whole numbers exactly too.
        let mut mixed = Dataset::new("t", GeometryType::None, vec![Field::new("n", FieldType::Double)]);
        for v in [Value::Int((1 << 53) + 1), Value::Float(0.5)] {
            mixed.push(None, vec![v]);
        }
        assert_eq!(compute(Stat::Concatenate, &column(&mixed, &[0, 1], 0, false), ", "), Value::Text("9007199254740993, 0.5".into()));
        // UNIQUE is exact in a mixed column too: 2^53 and 2^53 + 1 (whole) are two values beside a
        // decimal, 2^53 + 1 and 2^53 (a decimal) are two, and 2 (whole) and 2.0 (decimal) one.
        let mut mixed = Dataset::new("t", GeometryType::None, vec![Field::new("n", FieldType::Double)]);
        for v in [
            Value::Int(1 << 53),
            Value::Int((1 << 53) + 1),
            Value::Float(0.5),
            Value::Float(9_007_199_254_740_992.0),
            Value::Int(2),
            Value::Float(2.0),
        ] {
            mixed.push(None, vec![v]);
        }
        let unique = |rows: &[usize]| compute(Stat::Unique, &column(&mixed, rows, 0, false), ", ");
        assert_eq!(unique(&[0, 1, 2]), Value::Int(3));
        assert_eq!(unique(&[1, 3, 4, 5]), Value::Int(3));
        // On that exact path, −0 and 0 are one value with the whole number 0.
        let mut signed_zero = Dataset::new("t", GeometryType::None, vec![Field::new("n", FieldType::Double)]);
        for v in [Value::Int(0), Value::Float(-0.0), Value::Float(0.0)] {
            signed_zero.push(None, vec![v]);
        }
        assert_eq!(compute(Stat::Unique, &column(&signed_zero, &[0, 1, 2], 0, false), ", "), Value::Int(1));
        // MODE and MINORITY tell whole numbers past 2^53 apart: 2^53 + 2 twice is the mode, not
        // 2^53 and 2^53 + 1, which share an f64.
        let mut modes = Dataset::new("t", GeometryType::None, vec![Field::new("n", FieldType::Integer)]);
        for i in [1i64 << 53, (1 << 53) + 1, (1 << 53) + 2, (1 << 53) + 2] {
            modes.push(None, vec![Value::Int(i)]);
        }
        let col = column(&modes, &[0, 1, 2, 3], 0, false);
        assert_eq!(compute(Stat::Mode, &col, ", "), Value::Float(9_007_199_254_740_994.0));
        assert_eq!(compute(Stat::Minority, &col, ", "), Value::Float(9_007_199_254_740_992.0));
        // A column's equality ignores whether its values were sorted yet.
        let sorted = column(&modes, &[0, 1, 2, 3], 0, false);
        assert_eq!(compute(Stat::Median, &sorted, ", "), compute(Stat::Median, &col, ", "));
        assert_eq!(sorted, column(&modes, &[0, 1, 2, 3], 0, false));
        // MODE of −0 and 0 is 0.
        assert!(matches!(f("MODE", &nums(&[-0.0, 0.0])), Value::Float(z) if z.to_bits() == 0));
        let tiny = f64::from_bits(1);
        assert_eq!(f("MEDIAN", &nums(&[tiny, tiny])), Value::Float(tiny));
    }

    /// A statistic whose exact value is a whole number is rounded when a decimal can't hold it, and
    /// not otherwise; statistics that are decimals anyway never are.
    #[test]
    fn rounded_marks_whole_numbers_past_2_53() {
        const BIG: i64 = (1 << 53) + 1;
        let ints = |v: &[Value]| {
            let mut ds = Dataset::new("t", GeometryType::None, vec![Field::new("n", FieldType::Integer)]);
            for x in v {
                ds.push(None, vec![x.clone()]);
            }
            let rows: Vec<usize> = (0..v.len()).collect();
            column(&ds, &rows, 0, false)
        };
        let check = |stat: &str, c: &Column| {
            let stat = Stat::parse(stat).unwrap();
            rounded(stat, c, &compute(stat, c, ", "))
        };
        // In range: nothing is rounded, a sum reaching exactly 2^53 included.
        let small = ints(&[Value::Int(1 << 52), Value::Int(1 << 52), Value::Int(-3)]);
        for stat in ["SUM", "MEAN", "MIN", "MAX", "RANGE", "MEDIAN", "STD", "FIRST", "LAST", "MODE", "P90", "COUNT", "UNIQUE"] {
            assert!(!check(stat, &small), "{stat}");
        }
        assert!(!check("SUM", &ints(&[Value::Int(1 << 52), Value::Int(1 << 52)])));
        // A sum past 2^53 of whole numbers each in range is rounded; the mean of the same is a
        // decimal anyway and isn't counted.
        let sum_past = ints(&[Value::Int(1 << 53), Value::Int(1)]);
        assert!(check("SUM", &sum_past));
        assert!(!check("MEAN", &sum_past));
        assert!(!check("MAX", &sum_past), "2^53 itself fits");
        // A range past 2^53, odd, is rounded.
        assert!(check("RANGE", &ints(&[Value::Int(1 << 53), Value::Int(-1)])));
        // One whole number past 2^53: the statistics that pick it, or sum or subtract it, are
        // rounded; those that pick another value, fall between values or are decimals anyway aren't.
        let one_big = ints(&[Value::Int(2), Value::Int(2), Value::Int(BIG)]);
        for stat in ["MAX", "LAST", "SUM", "RANGE", "MINORITY", "P99"] {
            assert!(check(stat, &one_big) == (stat != "P99"), "{stat}");
        }
        for stat in ["MIN", "FIRST", "MODE", "MEDIAN", "P99", "MEAN", "STD", "VARIANCE", "CV", "COUNT", "UNIQUE", "NULLS", "CONCATENATE"] {
            // (2 + 2 + BIG) / 3 isn't whole: the MEAN is a decimal anyway.
            assert!(!check(stat, &one_big), "{stat}");
        }
        // A MEAN that is whole is checked: BIG alone, and (BIG + BIG + 4) / 3; not (2 + BIG) / 2.
        assert!(check("MEAN", &ints(&[Value::Int(BIG)])));
        assert!(check("MEAN", &ints(&[Value::Int(BIG), Value::Int(BIG), Value::Int(4)])) == ((2 * BIG + 4) % 3 == 0));
        assert!(!check("MEAN", &ints(&[Value::Int(1 << 53), Value::Int(-4)])), "whole, and it fits");
        assert!(!check("MEAN", &ints(&[Value::Int((1 << 53) + 2), Value::Int((1 << 53) + 6)])), "2^53 + 4, as the decimals give it");
        assert!(check("MEAN", &ints(&[Value::Int((1 << 53) + 2), Value::Int((1 << 53) + 4)])), "2^53 + 3 doesn't fit");
        // A MEDIAN or percentile landing on it is rounded; one halfway between it and another, not a
        // whole number, isn't.
        let two_big = ints(&[Value::Int(2), Value::Int(BIG), Value::Int(BIG)]);
        assert!(check("MEDIAN", &two_big));
        assert!(check("P50", &two_big));
        assert!(!check("P25", &two_big), "halfway between 2 and BIG");
        assert!(!check("MEDIAN", &ints(&[Value::Int(2), Value::Int(BIG)])), "halfway");
        // Between two values, a whole percentile past 2^53 is checked: 2^53 + 1 halfway between
        // 2^53 and 2^53 + 2 comes out 2^53, but 2^53 + 2 between 2^53 and 2^53 + 4 fits.
        assert!(check("MEDIAN", &ints(&[Value::Int(1 << 53), Value::Int((1 << 53) + 2)])));
        assert!(!check("MEDIAN", &ints(&[Value::Int(1 << 53), Value::Int((1 << 53) + 4)])));
        assert!(check("P75", &ints(&[Value::Int(1 << 53), Value::Int((1 << 53) + 4)])), "2^53 + 3");
        // Within 2^53 the interpolation's own rounding isn't reported: P29 of 0 and 100 is 29,
        // computed as 28.999999999999996.
        let hundred = ints(&[Value::Int(0), Value::Int(100), Value::Int(BIG)]);
        assert_ne!(compute(Stat::Percentile(29), &ints(&[Value::Int(0), Value::Int(100)]), ", "), Value::Float(29.0), "premise");
        assert!(!check("P29", &ints(&[Value::Int(0), Value::Int(100)])));
        assert!(!check("P14", &hundred), "with a value past 2^53 beside them too");
        // But a whole percentile or IQR of values past 2^53 is checked wherever it lies: the median
        // of −(2^53 + 1) and 2^53 + 3 is 1, computed as 2; the IQR of 2^53 + 1 and 2^53 + 3 is 1,
        // from quartiles that aren't whole, computed as 4.
        let straddle = ints(&[Value::Int(-BIG), Value::Int(BIG + 2)]);
        assert_eq!(compute(Stat::Median, &straddle, ", "), Value::Float(2.0), "premise");
        assert!(check("MEDIAN", &straddle));
        let close = ints(&[Value::Int(BIG), Value::Int(BIG + 2)]);
        assert_eq!(compute(Stat::Iqr, &close, ", "), Value::Float(4.0), "premise");
        assert!(check("IQR", &close));
        // A value past 2^53 elsewhere in the column doesn't make it checked: these quartiles lie
        // between values within 2^53, and the IQR, 3, comes out 2.
        let (m, n) = (Value::Int(6_755_399_441_055_746), Value::Int(6_755_399_441_055_750));
        let beside = ints(&[m.clone(), m, n.clone(), n.clone(), n, Value::Int(BIG)]);
        assert_eq!(compute(Stat::Iqr, &beside, ", "), Value::Float(2.0), "premise");
        assert!(!check("IQR", &beside));
        // Interpolation weighs the nearer value more: P25 of 2^53 and 2^53 + 8 is 2^53 + 2, P75
        // 2^53 + 6, and both fit.
        let eight = ints(&[Value::Int(1 << 53), Value::Int((1 << 53) + 8)]);
        assert_eq!(compute(Stat::Percentile(25), &eight, ", "), Value::Float(9_007_199_254_740_994.0), "premise");
        assert!(!check("P25", &eight));
        assert!(!check("P75", &eight));
        // Past 2^53 the check follows the result: marked exactly when the IQR, 2, comes out otherwise.
        let wide = ints(&[Value::Int((1 << 53) + 2), Value::Int((1 << 53) + 6)]);
        assert_eq!(check("IQR", &wide), compute(Stat::Iqr, &wide, ", ") != Value::Float(2.0));
        // Within 2^53 an IQR's own rounding isn't reported either: 2 comes out 0 here.
        let within = ints(&[Value::Int(6_755_399_441_055_746), Value::Int(6_755_399_441_055_750)]);
        assert_eq!(compute(Stat::Iqr, &within, ", "), Value::Float(0.0), "premise");
        assert!(!check("IQR", &within));
        // Halfway between two values past 2^53 that differ by an odd number isn't a whole number.
        assert!(!check("MEDIAN", &ints(&[Value::Int((1 << 53) + 2), Value::Int((1 << 53) + 5)])));
        // IQR is checked as the difference of the values it lands on: BIG − 2 is 2^53 − 1, but the
        // decimals give 2^53 − 2.
        let spread = ints(&[Value::Int(2), Value::Int(2), Value::Int(2), Value::Int(BIG), Value::Int(BIG)]);
        assert!(check("IQR", &spread));
        assert!(!check("IQR", &ints(&[Value::Int(1 << 52), Value::Int(-(1 << 52)), Value::Int(3)])));
        assert!(!check("IQR", &ints(&[Value::Int(BIG), Value::Int(BIG), Value::Int(BIG)])), "BIG − BIG is 0");
        let exact_spread = ints(&[Value::Int(BIG), Value::Int(BIG), Value::Int(BIG), Value::Int(BIG + 4), Value::Int(BIG + 4)]);
        assert_eq!(compute(Stat::Iqr, &exact_spread, ", "), Value::Float(4.0), "premise: rounded alike, 4 apart");
        assert!(!check("IQR", &exact_spread));
        // A sum is checked against its exact value: BIG − (BIG − 1) is 1, but BIG reads as 2^53 and
        // the decimal sum is 0. A rounded input whose sum comes out right isn't marked.
        let cancel = ints(&[Value::Int(BIG), Value::Int(-(BIG - 1))]);
        assert_eq!(compute(Stat::Sum, &cancel, ", "), Value::Float(0.0), "premise: BIG reads as 2^53");
        assert!(check("SUM", &cancel));
        let even = ints(&[Value::Int((1 << 53) + 2), Value::Int(-2)]);
        assert!(!check("SUM", &even), "2^53 + 2 fits a decimal, and so does the sum");
        // Past 2^53 only the whole numbers a decimal can't hold count: 2^53 + 2 can.
        let fits = ints(&[Value::Int((1 << 53) + 2), Value::Int(4), Value::Int((1 << 53) + 2)]);
        for stat in ["MEDIAN", "MAX", "MODE", "FIRST", "IQR"] {
            assert!(!check(stat, &fits), "{stat}");
        }
        // MIN and MAX each check the value they pick: here 2^53 + 2 fits, 2^53 + 3 doesn't.
        let apart = ints(&[Value::Int((1 << 53) + 3), Value::Int((1 << 53) + 2)]);
        assert!(!check("MIN", &apart));
        assert!(check("MAX", &apart));
        let apart = ints(&[Value::Int(-(1 << 53) - 2), Value::Int(-(1 << 53) - 3)]);
        assert!(!check("MAX", &apart));
        assert!(check("MIN", &apart));
        // Decimals and texts are never marked: there are no whole numbers to round.
        assert!(!check("SUM", &Column::new(nums(&[1e300, 1.0]), 0)));
        assert!(!check("MAX", &Column::new(texts(&["a"]), 0)));
        // Whole numbers written as text and read as numbers are checked as well, in any form.
        let mut written = Dataset::new("t", GeometryType::None, vec![Field::new("n", FieldType::Text)]);
        for t in ["9.007199254740993e15", " 2 ", "1e3"] {
            written.push(None, vec![Value::Text(t.into())]);
        }
        let as_text = column(&written, &[0, 1, 2], 0, true);
        assert!(rounded(Stat::Max, &as_text, &compute(Stat::Max, &as_text, ", ")));
        assert!(!rounded(Stat::Min, &as_text, &compute(Stat::Min, &as_text, ", ")));
        let small_text = column(&written, &[1, 2], 0, true);
        assert!(!rounded(Stat::Sum, &small_text, &compute(Stat::Sum, &small_text, ", ")));
        // A decimal with a fraction among whole numbers: the SUM is a decimal, not checked.
        let mixed = |v: [Value; 2]| {
            let mut ds = Dataset::new("t", GeometryType::None, vec![Field::new("n", FieldType::Integer)]);
            for x in v {
                ds.push(None, vec![x]);
            }
            column(&ds, &[0, 1], 0, false)
        };
        assert!(!rounded(Stat::Sum, &mixed([Value::Int(BIG), Value::Float(0.5)]), &Value::Float(1.0)));
        // A decimal with no fraction, or true or false, among whole numbers is a whole number too.
        let marked = |stat: Stat, c: &Column| rounded(stat, c, &compute(stat, c, ", "));
        // BIG + 0.0 and BIG + true come out 2^53 and 2^53, not 2^53 + 1 and 2^53 + 2.
        assert!(marked(Stat::Sum, &mixed([Value::Int(BIG), Value::Float(0.0)])));
        assert!(marked(Stat::Sum, &mixed([Value::Int(BIG), Value::Bool(true)])));
        assert!(marked(Stat::Mean, &mixed([Value::Int(BIG), Value::Float(1.0)])), "(2^53 + 2) / 2");
        // A MEAN over a decimal with a fraction is a decimal, however the whole numbers alone divide:
        // (BIG + 1.5 + 1.5) / 3 is whole and comes out right, while BIG / 3 is whole and differs.
        assert!(!marked(Stat::Mean, &ints(&[Value::Int(BIG), Value::Float(1.5), Value::Float(1.5)])));
        assert!(!marked(Stat::Sum, &mixed([Value::Int(1 << 53), Value::Float(2.0)])), "2^53 + 2 fits");
        // −2^63 is an i64 too, so checked as one: BIG − 2^63 comes out 2^53 − 2^63.
        assert!(marked(Stat::Sum, &mixed([Value::Int(BIG), Value::Float(-9_223_372_036_854_775_808.0)])));
        assert!(!marked(Stat::Sum, &mixed([Value::Int(1), Value::Float(9_223_372_036_854_775_808.0)])), "2^63 isn't");
        // P75 of 1.0 and BIG is 3·2^51 + 1, computed as 3·2^51.
        assert!(marked(Stat::Percentile(75), &mixed([Value::Float(1.0), Value::Int(BIG)])));
        let max = mixed([Value::Int(BIG), Value::Float(0.5)]);
        assert!(rounded(Stat::Max, &max, &compute(Stat::Max, &max, ", ")), "but the whole number it picks is");
    }

    /// Whole numbers written as text are read exactly in every form the decimal reader takes, and
    /// other text is not a whole number.
    #[test]
    fn whole_numbers_in_text() {
        let whole_text = |t: &str| whole_text(t).as_ref().and_then(WholeText::to_i64);
        for (t, want) in [
            ("12", Some(12)),
            (" +12 ", Some(12)),
            ("-12", Some(-12)),
            ("0012", Some(12)),
            ("12.", Some(12)),
            ("12.000", Some(12)),
            ("1.2e1", Some(12)),
            ("1200E-2", Some(12)),
            ("9007199254740993.0", Some((1 << 53) + 1)),
            ("9.007199254740993e15", Some((1 << 53) + 1)),
            ("9223372036854775807", Some(i64::MAX)),
            ("-9223372036854775808", Some(i64::MIN)),
            ("-0", Some(0)),
            ("0.0e5", Some(0)),
            (".0", Some(0)),
            ("0e2147483647", Some(0)),
            ("0e2147483648", Some(0)),
            ("-0.000e-99999999999", Some(0)),
            ("12.5", None),
            ("1.25e1", None),
            ("1e-1", None),
            ("9223372036854775808", None),
            ("1e19", None),
            ("1e2147483647", None),
            ("1e99999999999", None),
            ("", None),
            (".", None),
            ("-", None),
            ("e5", None),
            ("1e", None),
            ("1e+", None),
            ("1e5.0", None),
            ("0e", None),
            ("inf", None),
            ("NaN", None),
            ("1_000", None),
            ("--1", None),
            ("1.2.3", None),
            ("١٢", None),
        ] {
            assert_eq!(whole_text(t), want, "{t:?}");
            if let Some(w) = want {
                assert_eq!(t.trim().parse::<f64>().ok(), Some(w as f64), "premise: the decimal reader takes {t:?}");
            }
        }
    }

    /// A whole number written as text past the i64 range is its digits, however it is written;
    /// one written as digits alone is counted in a column read as numbers, whether or not the
    /// decimal it is read as holds it.
    #[test]
    fn whole_text_past_i64_is_counted_when_written_in_digits() {
        let digits = |t: &str| {
            let w = whole_text(t).unwrap_or_else(|| panic!("{t:?} is a whole number"));
            assert_eq!(w.to_i64(), None, "{t:?} is past the i64 range");
            w.digits.clone()
        };
        // Written as digits alone, or with a point or an exponent, which ask for a decimal.
        let plain = |t: &str| whole_text(t).map(|w| w.plain);
        for t in ["123456789012345678901", " -9223372036854775809 ", "9223372036854775808", "+12", "0", "-0", "007"] {
            assert_eq!(plain(t), Some(true), "{t:?}");
        }
        for t in ["1e20", "12.", "12.0", "1200e-2", "0.0", "0e5", "9e37"] {
            assert_eq!(plain(t), Some(false), "{t:?}");
        }
        assert_eq!(digits("123456789012345678901"), "123456789012345678901");
        assert_eq!(digits("-123456789012345678901"), "-123456789012345678901");
        assert_eq!(digits("9223372036854775808"), "9223372036854775808", "2^63, which is a decimal");
        assert_eq!(digits("-9223372036854775809"), "-9223372036854775809");
        assert_eq!(digits("100000000000000000000"), "100000000000000000000");
        assert_eq!(digits("1e20"), "100000000000000000000");
        assert_eq!(digits("12345678901234567890000e-3"), "12345678901234567890");
        assert_eq!(digits("1.7976931348623157e308").len(), 309, "the largest decimal");
        assert_eq!(whole_text("1e2147483647"), None, "more digits than any decimal has");
        // Zero and a decimal with a fraction are never past the range.
        assert_eq!(whole_text("-0").as_ref().and_then(WholeText::to_i64), Some(0));
        assert_eq!(whole_text("12.5"), None);
        // In a column read as numbers, each value written in digits past the range is counted,
        // whether the decimal holds it (2^63, 10^20) or not; whole numbers within the range,
        // decimals and numbers written with an exponent aren't.
        let mut ds = Dataset::new("t", GeometryType::None, vec![Field::new("code", FieldType::Text)]);
        for t in [
            "123456789012345678901",
            "-9223372036854775809",
            "100000000000000000000",
            "9007199254740993",
            "12.5",
            "1e400",
            "n/a",
            "9e37",
            "9223372036854775808",
            "9223372036854775807",
            "1e20",
        ] {
            ds.push(None, vec![Value::Text(t.into())]);
        }
        let all: Vec<usize> = (0..11).collect();
        let c = column(&ds, &all, 0, true);
        assert_eq!(c.rounded_texts(), 4);
        assert_eq!(c.nulls, 2, "premise: an infinite decimal and a word are null");
        assert_eq!(c.whole, [None, None, None, Some(9_007_199_254_740_993), None, None, None, Some(i64::MAX), None]);
        assert_eq!(column(&ds, &[2, 8], 0, true).rounded_texts(), 2, "10^20 and 2^63, which their decimals hold");
        assert_eq!(column(&ds, &[3, 4, 7, 9, 10], 0, true).rounded_texts(), 0, "within the range, a decimal, an exponent");
        assert_eq!(column(&ds, &[0], 0, false).rounded_texts(), 0, "read as text, nothing is a number");
        // Over whole numbers past the range the per-statistic check says nothing: the count is the
        // notice.
        assert!(!rounded(Stat::Sum, &c, &compute(Stat::Sum, &c, ", ")));
        assert!(!rounded(Stat::Max, &c, &compute(Stat::Max, &c, ", ")));
    }

    /// CONCATENATE writes −0 as 0, as grouping treats it.
    #[test]
    fn concatenate_writes_negative_zero_as_zero() {
        assert_eq!(f("CONCATENATE", &nums(&[-0.0, 1.5, -2.0])), Value::Text("0, 1.5, -2".into()));
    }

    /// A statistics list is read from JSON or its text; bad fields, statistics and pairs are named
    /// in the error.
    #[test]
    fn parsing_requests() {
        let ds = table(&[]);
        let ok = parse_requests(&ds, Some(&serde_json::json!([{"field": "V", "statistic": "p90"}, {"field": "k", "statistic": "mode"}])));
        assert_eq!(ok, Ok(vec![Request { field: 0, stat: Stat::Percentile(90) }, Request { field: 1, stat: Stat::Mode }]));
        let text = parse_requests(&ds, Some(&Json::String(r#"[{"field": "v", "statistic": "SUM"}]"#.into())));
        assert_eq!(text, Ok(vec![Request { field: 0, stat: Stat::Sum }]));
        // The list's length is bounded.
        let long: Vec<Json> =
            (1..=99).flat_map(|k| ["v", "k", "v"].map(|f| serde_json::json!({"field": f, "statistic": format!("P{k}")}))).collect();
        let err = parse_requests(&ds, Some(&Json::Array(long))).unwrap_err();
        assert!(err.contains("at most 256"), "{err}");
        assert_eq!(parse_requests(&ds, Some(&Json::String(" ".into()))), Ok(Vec::new()));
        for (bad, says) in [
            (serde_json::json!([{"field": "k", "statistic": "MEAN"}]), "needs a numeric field"),
            (serde_json::json!([{"field": "v", "statistic": "AVG"}]), "must be one of"),
            (serde_json::json!([{"field": "nope", "statistic": "SUM"}]), "no field"),
            (serde_json::json!([{"statistic": "SUM"}]), "needs a \"field\""),
            (serde_json::json!([{"field": "v", "statistic": "SUM"}, {"field": "V", "statistic": "sum"}]), "listed twice"),
            (serde_json::json!({"field": "v"}), "must be a list"),
            (Json::String("[".into()), "Statistics:"),
        ] {
            let err = parse_requests(&ds, Some(&bad)).unwrap_err();
            assert!(err.contains(says), "{bad}: {err}");
        }
    }
}
