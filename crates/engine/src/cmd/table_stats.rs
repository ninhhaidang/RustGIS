//! `table.statistics`: the quick numbers of one column of a layer's attribute table, without an
//! output table (Summary Statistics writes one).
//!
//! One pass over the rows. The sum, mean, minimum and maximum are the ones Summary Statistics
//! defines, except that a column of whole numbers is added up exactly (an i128 sum, i64 minimum and
//! maximum), so nothing past 2^53 is rounded; the standard deviation is the population one
//! (divided by n), as its STD is. A text or date column keeps its distinct texts to count them, up
//! to [`MAX_DISTINCT`] entries and [`MAX_DISTINCT_BYTES`] of UTF-8 text: past either the count stops and says so, so a column of identifiers is not
//! copied whole into memory.
//!
//! The work grows with the rows, so the desktop app does it on a thread of its own first
//! ([`stats_ahead`]), with progress and **Cancel**, and the command takes that result as it lands;
//! a layer changed meanwhile is told to ask again rather than counted again here, which would
//! hold the window. Direct desktop MCP relay requests use the same read queue. Synchronous
//! Session adapters (including scripts and the embedded assistant) count on their caller's thread;
//! they must arrange a worker when that thread owns a window. A registered window owner refuses
//! an unprepared inline count instead of holding the window. The browser has one thread.

use std::borrow::Cow;
use std::collections::HashSet;

use rustgis_data::loss::{LossCode, Note};
use rustgis_data::{Dataset, Value as Cell};
use serde_json::{Value, json};

use super::{b, ps};
use crate::{CmdResult, Session};

/// How many distinct texts a text or date column is counted to. Past it the count stops, and the
/// answer and a notice say it is "more than" this.
pub const MAX_DISTINCT: usize = 100_000;

/// Most UTF-8 bytes copied into the distinct-value set (in addition to its entry bound).
pub const MAX_DISTINCT_BYTES: usize = 8 << 20;

/// Exact binary sum, used only when the ordinary sum overflows. Each bin holds signed
/// significands at one binary exponent, in units of the smallest subnormal. 2098 bits hold
/// any finite f64; usize::BITS more hold a whole column's sum. Each i128 bin holds at most
/// usize::MAX 53-bit significands without overflowing, on either supported word size.
#[derive(Clone, Default)]
struct WideSum(Vec<i128>);

impl WideSum {
    fn add(&mut self, x: f64) {
        if self.0.is_empty() {
            self.0.resize(2099 + usize::BITS as usize, 0);
        }
        let bits = x.to_bits();
        let exponent = ((bits >> 52) & 0x7ff) as usize;
        let significand = (bits & ((1u64 << 52) - 1)) | if exponent > 0 { 1u64 << 52 } else { 0 };
        let signed = if x.is_sign_negative() { -i128::from(significand) } else { i128::from(significand) };
        if let Some(bin) = self.0.get_mut(exponent.saturating_sub(1)) {
            *bin += signed;
        }
    }

    fn total(&self) -> f64 {
        let normalize = |digits: &mut [i128]| {
            let mut carry = 0;
            for d in digits {
                let v = *d + carry;
                *d = v.rem_euclid(2);
                carry = v.div_euclid(2);
            }
            carry
        };
        let mut digits = self.0.clone();
        let negative = normalize(&mut digits) < 0;
        if negative {
            digits.clone_from(&self.0);
            for d in &mut digits {
                *d = -*d;
            }
            normalize(&mut digits);
        }
        let Some(high) = digits.iter().rposition(|&d| d != 0) else { return 0.0 };
        let cut = high.saturating_sub(52);
        let mut significand = 0u64;
        for &d in digits.get(cut..=high).unwrap_or(&[]).iter().rev() {
            significand = (significand << 1) | d as u64;
        }
        // Round once, ties to even, before restoring the binary scale.
        if cut > 0 && digits.get(cut - 1) == Some(&1) && (significand & 1 != 0 || digits.iter().take(cut - 1).any(|&d| d != 0)) {
            significand += 1;
        }
        let scale = if cut < 52 { f64::from_bits(1u64 << cut) } else { 2.0f64.powi(cut as i32 - 1074) };
        let total = significand as f64 * scale;
        if negative { -total } else { total }
    }

    /// How far `t` is from the sum: what is left once `t` is taken away, rounded once.
    fn off_by(&self, t: f64) -> f64 {
        let mut rest = self.clone();
        rest.add(-t);
        rest.total()
    }
}

/// Whole numbers past this (2^53) are not all held by a decimal.
const EXACT_IN_F64: u64 = 1 << 53;

/// Welford's running mean and sum of squares (B. P. Welford, "Note on a method for calculating
/// corrected sums of squares and products", Technometrics 4(3), 1962), so one pass gives the variance.
#[derive(Default)]
struct Welford {
    n: usize,
    mean: f64,
    m2: f64,
    /// Population deviation updated without squaring a value (the squares may overflow or
    /// underflow even when the deviation fits). The ordinary result is retained when usable.
    deviation: f64,
    max_abs: f64,
}

impl Welford {
    fn add(&mut self, x: f64) {
        self.n += 1;
        let n = self.n as f64;
        let d = x - self.mean;
        // `x - mean` overflows for values of opposite sign near the largest float: then each part is
        // divided first, which can't.
        let step = if d.is_finite() { d / n } else { x / n - self.mean / n };
        self.mean += step;
        self.m2 += d * (x - self.mean);
        let previous = (self.n - 1) as f64;
        self.deviation = (self.deviation * (previous / n).sqrt()).hypot(step * previous.sqrt());
        self.max_abs = self.max_abs.max(x.abs());
    }
    fn std_dev(&self) -> f64 {
        let variance = self.m2 / self.n.max(1) as f64;
        // Even a positive subnormal square has lost significant bits: taking its square root
        // cannot recover them. Use the square-free result for partial underflow too.
        if variance.is_normal() && variance > 0.0 { variance.sqrt() } else { self.deviation.min(self.max_abs) }
    }
}

/// The statistics of a numeric column, gathered as the rows go by. Whole numbers are also kept
/// exactly, so a column of only whole numbers needs no decimal at all.
#[derive(Default)]
struct Numbers {
    count: usize,
    nulls: usize,
    skipped: usize,
    /// How many of `count` are whole numbers, and how many of those are past 2^53.
    ints: usize,
    big: usize,
    /// Of those, the ones a decimal can't hold, so rounded however the others add up.
    inexact: usize,
    sum: f64,
    wide_sum: WideSum,
    all: Welford,
    min: f64,
    max: f64,
    isum: i128,
    imin: Option<i64>,
    imax: Option<i64>,
    /// The first whole number; the others' spread is measured from it, exactly, so values of a
    /// large size that differ little keep their variance.
    first: Option<i64>,
    spread: Welford,
}

impl Numbers {
    fn add_float(&mut self, x: f64) {
        self.count += 1;
        self.sum += x;
        self.wide_sum.add(x);
        self.all.add(x);
        if self.count == 1 {
            self.min = x;
            self.max = x;
        } else {
            self.min = self.min.min(x);
            self.max = self.max.max(x);
        }
    }

    /// Whether the sum the answer gives is off by half a unit or more: rounded at the scale of the
    /// whole numbers past 2^53, as decimals alone never round a sum this far (0.1 + 0.2 isn't).
    /// The sum given is the running one, or the exact one where that overflowed.
    fn sum_rounded(&self) -> bool {
        let given = if self.sum.is_finite() { self.sum } else { self.wide_sum.total() };
        given.is_finite() && self.wide_sum.off_by(given).abs() >= 0.5
    }

    fn add_whole(&mut self, i: i64) {
        self.add_float(i as f64);
        self.ints += 1;
        if i.unsigned_abs() > EXACT_IN_F64 {
            self.big += 1;
            // One a decimal still holds exactly (2^54) is rounded only as the sum adds it up.
            self.inexact += usize::from((i as f64) as i128 != i128::from(i));
        }
        self.isum += i128::from(i);
        self.imin = Some(self.imin.map_or(i, |m| m.min(i)));
        self.imax = Some(self.imax.map_or(i, |m| m.max(i)));
        let first = *self.first.get_or_insert(i);
        self.spread.add((i128::from(i) - i128::from(first)) as f64);
    }
}

/// A whole number for the answer: a plain number while a decimal holds it, its integer when it is
/// past 2^53 (and fits an i64), its digits as text beyond that, so none is rounded.
fn exact(v: i128) -> Value {
    if v.unsigned_abs() <= u128::from(EXACT_IN_F64) {
        json!(v as f64)
    } else if let Ok(i) = i64::try_from(v) {
        json!(i)
    } else {
        Value::String(v.to_string())
    }
}

/// The statistics of a text, date or other column.
#[derive(Default)]
struct Texts {
    count: usize,
    nulls: usize,
    distinct: HashSet<String>,
    /// More distinct texts than [`MAX_DISTINCT`]: the set stopped growing.
    capped: bool,
    distinct_bytes: usize,
    byte_capped: bool,
    invalid_dates: usize,
    min: Option<String>,
    max: Option<String>,
    /// For a date column: the earliest and latest by instant.
    earliest: Option<(i64, String)>,
    latest: Option<(i64, String)>,
}

/// The selected rows, each once, sorted, leaving out numbers past the layer's last row. Sorting
/// is O(n log n), so it is done where the count is, not at the request on the UI thread.
fn covered(ds: &Dataset, selected: &[usize]) -> Vec<usize> {
    let mut rows: Vec<usize> = selected.iter().copied().filter(|&r| r < ds.features.len()).collect();
    rows.sort_unstable();
    rows.dedup();
    rows
}

fn is_empty_text(v: &Cell) -> bool {
    matches!(v, Cell::Text(t) if t.trim().is_empty())
}

/// What a request asks for, checked against the layer: which layer and column, over which rows.
#[derive(PartialEq)]
struct Target {
    id: u32,
    layer: String,
    /// The field index in dataset order, independent of the table's displayed order.
    column: usize,
    selected: bool,
    /// The selection as the layer had it when asked (empty when all are counted); [`gather`] sorts
    /// it and keeps each row once.
    rows: Vec<usize>,
}

/// The column of `p`: its `index` (dataset field order, which tells apart columns of the same
/// name, as the attribute table's menu gives it), else its `field` name, looked up as given
/// before the name without surrounding spaces.
fn column_of(ds: &Dataset, layer: &str, p: &Value) -> Result<usize, String> {
    if let Some(i) = p.get("index").filter(|i| !i.is_null()) {
        let i = i.as_u64().and_then(|i| usize::try_from(i).ok()).ok_or("\"index\" must be a field index in dataset order, from 0")?;
        return if ds.fields.get(i).is_some() { Ok(i) } else { Err(format!("{layer}: no column {i}")) };
    }
    let field = ps(p, "field").filter(|f| !f.trim().is_empty()).ok_or("give the field to describe (\"field\")")?;
    ds.field_index(field).or_else(|| ds.field_index(field.trim())).ok_or_else(|| format!("{layer}: no field named '{}'", field.trim()))
}

fn resolve(s: &Session, p: &Value) -> Result<Target, String> {
    let id = s.layer_id(p)?;
    let selected = b(p, "selected").unwrap_or(false);
    let l = s.map().layer(id).ok_or("no such layer")?;
    if l.is_raster() || l.graphics {
        return Err(format!("{}: statistics are for the fields of a feature layer", l.name));
    }
    let ds = &l.dataset;
    if ds.windowed {
        return Err(format!("{}: load all its features first (Layers › Load All Features), so the statistics cover them all", l.name));
    }
    let column = column_of(ds, &l.name, p)?;
    // Copied as it is: the sort and the dedup wait for `gather`, on the thread that counts.
    let rows = if selected { l.selection.clone() } else { Vec::new() };
    Ok(Target { id, layer: l.name.clone(), column, selected, rows })
}

/// An answer, and what it says to the user.
struct Done {
    answer: Value,
    /// What the count left out or rounded, each with its code: every one is a real loss.
    notices: Vec<rustgis_data::loss::Note>,
}

/// Counts column `t.column` of `ds` over its rows, looking at Cancel as it goes (see
/// [`rustgis_data::progress`]).
fn gather(ds: &Dataset, t: &Target) -> Result<Done, String> {
    let cancelled = |_| "the column statistics were cancelled".to_string();
    let fi = t.column;
    let f = ds.fields.get(fi).ok_or_else(|| format!("{}: no column {fi}", t.layer))?;
    let picked = if t.selected { covered(ds, &t.rows) } else { Vec::new() };
    let rows = if t.selected { picked.len() } else { ds.features.len() };
    let rows_at = |i: usize| if t.selected { picked.get(i).copied() } else { Some(i) };
    let mut out = json!({"layer": t.layer, "layerId": t.id, "field": f.name, "index": fi, "selected": t.selected, "rows": rows});
    let mut notices = Vec::new();
    let map = out.as_object_mut().ok_or("no answer")?;
    rustgis_data::progress::begin(rows);
    if f.ty.is_numeric() {
        let mut n = Numbers::default();
        for i in 0..rows {
            if i % 1024 == 0 {
                rustgis_data::progress::step((rows - i).min(1024)).map_err(cancelled)?;
            }
            let Some(r) = rows_at(i) else { continue };
            match ds.value(r, fi) {
                Cell::Null => n.nulls += 1,
                v if is_empty_text(v) => n.nulls += 1,
                Cell::Int(w) => n.add_whole(*w),
                v => match v.as_f64() {
                    Some(x) => n.add_float(x),
                    None => n.skipped += 1,
                },
            }
        }
        let some = n.count > 0;
        let whole = some && n.ints == n.count;
        let (sum, mean, min, max, std_dev) = match (whole, n.imin, n.imax) {
            (true, Some(lo), Some(hi)) => {
                (exact(n.isum), json!(n.isum as f64 / n.count as f64), exact(i128::from(lo)), exact(i128::from(hi)), n.spread.std_dev())
            }
            _ => (
                json!(if n.sum.is_finite() { n.sum } else { n.wide_sum.total() }),
                json!(n.all.mean),
                json!(n.min),
                json!(n.max),
                n.all.std_dev(),
            ),
        };
        // `json!` turns a number past what a decimal holds (infinity) into null: said, not silent.
        let mut overflowed = Vec::new();
        if some && sum.is_null() {
            overflowed.push("sum");
        }
        if some && mean.is_null() {
            overflowed.push("mean");
        }
        if some && !std_dev.is_finite() {
            overflowed.push("standard deviation");
        }
        let pick = |x: Value| if some { x } else { Value::Null };
        map.insert("kind".into(), json!("number"));
        map.insert("count".into(), json!(n.count));
        map.insert("nulls".into(), json!(n.nulls));
        map.insert("skipped".into(), json!(n.skipped));
        map.insert("sum".into(), sum);
        map.insert("mean".into(), pick(mean));
        map.insert("min".into(), pick(min));
        map.insert("max".into(), pick(max));
        map.insert("stdDev".into(), pick(json!(std_dev)));
        if n.skipped > 0 {
            notices.push(Note::loss(
                LossCode::DroppedValue,
                format!(
                    "{}: {} value(s) in '{}' that aren't finite numbers (NaN, infinity or other text) left out of the statistics",
                    t.layer, n.skipped, f.name
                ),
            ));
        }
        // Whole numbers added up exactly unless the column also holds decimals, which add them as
        // decimals: the whole numbers past 2^53 are then rounded.
        if !whole && n.inexact > 0 {
            notices.push(Note::loss(
                LossCode::ChangedRounded,
                format!(
                    "{}: {} whole number(s) in '{}' past 2^53 are rounded in the statistics, because the column also holds decimals",
                    t.layer, n.inexact, f.name
                ),
            ));
        } else if !whole && n.sum_rounded() {
            // A sum past 2^53 with a fraction (5 + 0.5 + 2^54, or decimals alone): a decimal holds
            // no fraction there, so it is rounded by half a unit or more. Checked against the exact
            // sum, so one that comes out exact, or rounds only as decimals do, says nothing.
            notices.push(Note::loss(
                LossCode::ChangedRounded,
                format!(
                    "{}: the sum and mean of '{}' are rounded by half a unit or more, as a decimal number holds no fraction past 2^53",
                    t.layer, f.name
                ),
            ));
        }
        if !overflowed.is_empty() {
            notices.push(Note::loss(
                LossCode::DroppedCalculationResult,
                format!(
                    "{}: the {} of '{}' is too large to hold as a decimal number (past about 1.8e308) and is left out",
                    t.layer,
                    overflowed.join(" and the "),
                    f.name
                ),
            ));
        }
    } else {
        let temporal = f.ty.is_temporal();
        let mut t2 = Texts::default();
        for i in 0..rows {
            if i % 1024 == 0 {
                rustgis_data::progress::step((rows - i).min(1024)).map_err(cancelled)?;
            }
            let Some(r) = rows_at(i) else { continue };
            let v = ds.value(r, fi);
            if v.is_null() || is_empty_text(v) {
                t2.nulls += 1;
                continue;
            }
            t2.count += 1;
            let text: Cow<str> = match v {
                Cell::Text(x) => Cow::Borrowed(x.as_str()),
                other => Cow::Owned(other.to_file_text()),
            };
            if !t2.capped && !t2.distinct.contains(text.as_ref()) {
                if t2.distinct.len() >= MAX_DISTINCT {
                    t2.capped = true;
                } else if text.len() > MAX_DISTINCT_BYTES.saturating_sub(t2.distinct_bytes) {
                    t2.capped = true;
                    t2.byte_capped = true;
                } else {
                    t2.distinct_bytes += text.len();
                    t2.distinct.insert(text.to_string());
                }
            }
            if temporal && let Some(k) = v.temporal_key() {
                if t2.earliest.as_ref().is_none_or(|(e, _)| k < *e) {
                    t2.earliest = Some((k, text.to_string()));
                }
                if t2.latest.as_ref().is_none_or(|(e, _)| k > *e) {
                    t2.latest = Some((k, text.to_string()));
                }
            } else if temporal {
                t2.invalid_dates += 1;
            } else {
                if t2.min.as_deref().is_none_or(|m| text.as_ref() < m) {
                    t2.min = Some(text.to_string());
                }
                if t2.max.as_deref().is_none_or(|m| text.as_ref() > m) {
                    t2.max = Some(text.to_string());
                }
            }
        }
        let (min, max) = if temporal { (t2.earliest.map(|e| e.1), t2.latest.map(|e| e.1)) } else { (t2.min, t2.max) };
        map.insert("kind".into(), json!(if temporal { "date" } else { "text" }));
        map.insert("count".into(), json!(t2.count));
        map.insert("nulls".into(), json!(t2.nulls));
        map.insert("distinct".into(), json!(t2.distinct.len()));
        if t2.capped {
            map.insert("distinctCapped".into(), json!(true));
            let text = if t2.byte_capped {
                format!(
                    "{}: '{}' reached the {} MiB distinct-text budget; it has more than {} distinct values and the distinct count stops at {}",
                    t.layer,
                    f.name,
                    MAX_DISTINCT_BYTES >> 20,
                    t2.distinct.len(),
                    t2.distinct.len()
                )
            } else {
                format!(
                    "{}: '{}' has more than {MAX_DISTINCT} distinct values; the distinct count stops at {MAX_DISTINCT}",
                    t.layer, f.name
                )
            };
            notices.push(Note::loss(LossCode::DroppedOverLimit, text));
        }
        if t2.invalid_dates > 0 {
            notices.push(Note::loss(
                LossCode::DroppedValue,
                format!(
                    "{}: {} value(s) in '{}' aren't dates and were left out of the date minimum and maximum",
                    t.layer, t2.invalid_dates, f.name
                ),
            ));
        }
        map.insert("min".into(), json!(min));
        map.insert("max".into(), json!(max));
    }
    Ok(Done { answer: out, notices })
}

/// The key a count made ahead is taken by (see [`super::reads`]).
fn ahead_key(id: u32) -> String {
    format!("table.statistics:{id}")
}

/// A count made ahead: the rows it counted (shared with the layer, not copied), what it was asked
/// and what it found.
struct StatsAhead {
    dataset: Dataset,
    target: Target,
    done: Result<Done, String>,
}

impl StatsAhead {
    /// Whether it counted `l` as it is: the same rows (the same allocation: an edit copies them),
    /// fields, selection and request.
    fn of(&self, l: &rustgis_map::Layer, t: &Target) -> bool {
        self.target == *t && l.dataset.features.shares(&self.dataset.features) && l.dataset.fields == self.dataset.fields
    }
}

/// The count `table.statistics` makes before it runs in the desktop app: on a thread of its own
/// with progress and Cancel. A request that is wrong gives nothing: the command says why.
pub(crate) fn stats_ahead(s: &Session, p: &Value) -> Option<super::reads::ReadWork> {
    let target = resolve(s, p).ok()?;
    let dataset = s.map().layer(target.id)?.dataset.clone();
    Some(super::reads::ahead("Column Statistics", ahead_key(target.id), move || {
        let done = gather(&dataset, &target);
        StatsAhead { dataset, target, done }
    }))
}

/// Synchronous dispatch cannot move a borrowed Session or its active policy stack to a worker,
/// nor yield a deferred reply. Desktop UI and direct relay adapters prepare the count before
/// dispatch; other synchronous adapters must run off their window thread. Spawning and joining
/// here would still hold that thread and would not provide its progress or Cancel controls.
pub(super) fn run(s: &mut Session, p: &Value) -> CmdResult {
    let target = resolve(s, p)?;
    let ahead = super::reads::take::<StatsAhead>(s, &ahead_key(target.id));
    #[cfg(not(target_arch = "wasm32"))]
    if ahead.is_none() && s.statistics_window_thread == Some(std::thread::current().id()) {
        return Err("column statistics need the desktop background read queue; use the column's Statistics menu or a direct MCP table.statistics request".into());
    }
    let l = s.map().layer(target.id).ok_or("no such layer")?;
    let done = match ahead {
        Some(a) if a.of(l, &target) => a.done?,
        // Counting again here would hold the window: the count made on its thread is dropped.
        Some(_) => return Err(format!("{}: the layer or its selection changed while the statistics were counted: ask again", l.name)),
        None => gather(&l.dataset, &target)?,
    };
    for n in done.notices {
        s.say(n);
    }
    Ok(done.answer)
}
