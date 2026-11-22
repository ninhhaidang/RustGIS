//! The time slider: a moving window of time that shows only the features of bound layers whose
//! date falls in it, and fills the dates of tile layers whose URL names one.

use std::sync::Arc;

use rustgis_data::Dataset;
use rustgis_data::time::{self, Unit};
use serde::{Deserialize, Serialize};

/// The most steps a slider lists (an hourly slider over centuries stops here).
pub const MAX_STEPS: usize = 20_000;

/// A map's time slider: the span it covers, where it is, and the window it shows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TimeSlider {
    /// The first step (milliseconds since 1970, UTC).
    pub start: f64,
    /// The last moment covered.
    pub end: f64,
    /// The current step.
    pub current: f64,
    /// What a step is: an hour, a day, a month or a year…
    pub unit: Unit,
    /// …times this many.
    pub interval: u32,
    /// Explicit dates to step through instead (irregular data); empty for regular steps.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dates: Vec<f64>,
    /// Steps before the current one the window takes in.
    pub before: u32,
    /// Steps from the current one the window takes in (1: just this step).
    pub after: u32,
    /// The window reaches back to the start: everything up to now shows.
    pub cumulative: bool,
    /// Milliseconds a step shows for while playing.
    pub speed_ms: u32,
    /// Playing starts over at the end.
    #[serde(rename = "loop")]
    pub looping: bool,
    /// The date format (empty: the unit's own, as `YYYY-MM`).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub format: String,
}

impl Default for TimeSlider {
    fn default() -> Self {
        TimeSlider {
            start: 0.0,
            end: 0.0,
            current: 0.0,
            unit: Unit::Day,
            interval: 1,
            dates: Vec::new(),
            before: 0,
            after: 1,
            cumulative: false,
            speed_ms: 800,
            looping: true,
            format: String::new(),
        }
    }
}

impl TimeSlider {
    /// A slider over `[start, end]` in steps that suit its length, at its start.
    pub fn spanning(start: f64, end: f64) -> TimeSlider {
        let (start, end) = if start <= end { (start, end) } else { (end, start) };
        let unit = Unit::for_span(end - start);
        let start = time::floor(start, unit);
        TimeSlider { start, end, current: start, unit, ..TimeSlider::default() }
    }

    /// The steps it moves through, in order.
    pub fn steps(&self) -> Vec<f64> {
        if !self.dates.is_empty() {
            return self.dates.clone();
        }
        let s = time::steps(self.start, self.end, self.unit, self.interval, MAX_STEPS);
        if s.is_empty() { vec![self.start] } else { s }
    }

    /// The index of the step at or before the current one.
    pub fn index(&self) -> usize {
        self.index_in(&self.steps())
    }

    /// [`index`](Self::index) in its steps, already listed.
    pub fn index_in(&self, steps: &[f64]) -> usize {
        steps.partition_point(|t| *t <= self.current).saturating_sub(1)
    }

    /// Moves `n` steps (back when negative); wraps around when `wrap`, else stops at the ends.
    /// Whether it moved.
    pub fn step(&mut self, n: i64, wrap: bool) -> bool {
        let s = self.steps();
        let len = s.len() as i64;
        if len == 0 {
            return false;
        }
        let i = self.index_in(&s) as i64 + n;
        let i = if wrap { i.rem_euclid(len) } else { i.clamp(0, len - 1) };
        let t = s.get(i as usize).copied().unwrap_or(self.start);
        let moved = t != self.current;
        self.current = t;
        moved
    }

    /// Moves to the step at or before `t` (clamped to the span).
    pub fn go_to(&mut self, t: f64) {
        let s = self.steps();
        self.current = s.iter().rev().find(|x| **x <= t).or(s.first()).copied().unwrap_or(self.start);
    }

    /// Whether it is at its last step.
    pub fn at_end(&self) -> bool {
        self.index() + 1 >= self.steps().len()
    }

    /// The window shown: `[from, to)`.
    pub fn window(&self) -> (f64, f64) {
        let k = i64::from(self.interval.max(1));
        let (from, to) = if self.dates.is_empty() {
            (time::add(self.current, self.unit, -i64::from(self.before) * k), time::add(self.current, self.unit, i64::from(self.after) * k))
        } else {
            // Irregular dates: the window runs from one listed date to another.
            let i = self.index();
            let at = |j: i64| self.dates.get(j.clamp(0, self.dates.len() as i64 - 1) as usize).copied();
            let from = at(i as i64 - i64::from(self.before)).unwrap_or(self.current);
            let last = i as i64 + i64::from(self.after);
            let to = match self.dates.get(last as usize) {
                Some(t) => *t,
                None => self.dates.last().map_or(self.current, |l| l + 1.0),
            };
            (from, to)
        };
        let from = if self.cumulative { f64::NEG_INFINITY } else { from };
        (from, to.max(from))
    }

    /// The format dates show in.
    pub fn date_format(&self) -> &str {
        if self.format.trim().is_empty() { self.unit.format() } else { &self.format }
    }

    /// A date in its format.
    pub fn label(&self, t: f64) -> String {
        time::format(t, self.date_format())
    }

    /// Keeps it usable whatever a file or command gave it: finite, ordered, bounded.
    pub fn normalize(&mut self) {
        let fix = |x: f64| if x.is_finite() { x.clamp(-3.0e16, 3.0e16) } else { 0.0 };
        self.start = fix(self.start);
        self.end = fix(self.end).max(self.start);
        self.interval = self.interval.clamp(1, 10_000);
        self.before = self.before.min(10_000);
        // The window always takes in the current step: `[from, to)` is never empty.
        self.after = self.after.clamp(1, 10_000);
        self.speed_ms = self.speed_ms.clamp(16, 60_000);
        self.dates.retain(|t| t.is_finite());
        self.dates.sort_by(f64::total_cmp);
        self.dates.dedup();
        self.dates.truncate(MAX_STEPS);
        let current = fix(self.current);
        self.go_to(current);
    }
}

/// A layer bound to the time slider: the field its dates are in, and the window it shows.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TimeBinding {
    pub field: String,
    /// The window last applied (`None`: the slider is off and every feature shows). Not saved:
    /// a cumulative one is unbounded, which JSON can't hold; opening a project applies it again.
    #[serde(skip)]
    pub window: Option<(f64, f64)>,
    /// The rows the window keeps, and the window, rows and fields they were worked out for:
    /// worked out again when a layer's features change outside a command (a streamed layer reading
    /// another view), once, not on every draw.
    #[serde(skip)]
    shown: Arc<std::sync::Mutex<Option<Shown>>>,
    /// Each row's moment, with the rows, fields and field it was read from: kept while they are
    /// the same, so moving the slider doesn't read the dates again.
    #[serde(skip)]
    moments: Option<Arc<Moments>>,
}

/// A binding's rows in its window, and what they were worked out for.
#[derive(Debug)]
struct Shown {
    window: (f64, f64),
    features: rustgis_data::Features,
    fields: Vec<rustgis_data::Field>,
    rows: Arc<Vec<bool>>,
}

/// A binding's moments and what they were read from.
#[derive(Debug)]
struct Moments {
    features: rustgis_data::Features,
    fields: Vec<rustgis_data::Field>,
    field: String,
    moments: Option<Vec<Option<f64>>>,
}

impl TimeBinding {
    pub fn new(field: impl Into<String>) -> TimeBinding {
        TimeBinding { field: field.into(), ..TimeBinding::default() }
    }

    /// Sets the window and works out the rows it keeps.
    pub fn apply(&mut self, ds: &Dataset, window: Option<(f64, f64)>) {
        self.window = window;
        let Some(w) = window else {
            // A new cache: copies of the binding (an undo step) keep theirs.
            self.shown = Arc::default();
            return;
        };
        let fresh =
            self.moments.as_ref().is_some_and(|m| m.features.shares(&ds.features) && m.fields == ds.fields && m.field == self.field);
        if !fresh {
            self.moments = Some(Arc::new(Moments {
                features: ds.features.clone(),
                fields: ds.fields.clone(),
                field: self.field.clone(),
                moments: dated(ds, &self.field),
            }));
        }
        let m = self.moments.as_ref().and_then(|m| m.moments.as_deref());
        let rows = Arc::new(keep(m, ds.features.len(), w));
        self.shown =
            Arc::new(std::sync::Mutex::new(Some(Shown { window: w, features: ds.features.clone(), fields: ds.fields.clone(), rows })));
    }

    /// The rows in the window (`None`: there is none, and every row shows): those worked out
    /// for these features and fields, or (when they changed outside a command) worked out now and
    /// kept.
    pub fn keeps(&self, ds: &Dataset) -> Option<Arc<Vec<bool>>> {
        let w = self.window?;
        let mut cache = self.shown.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = cache.as_ref().filter(|c| c.window == w && c.features.shares(&ds.features) && c.fields == ds.fields) {
            return Some(c.rows.clone());
        }
        let rows = Arc::new(rows_in(ds, &self.field, w));
        *cache = Some(Shown { window: w, features: ds.features.clone(), fields: ds.fields.clone(), rows: rows.clone() });
        Some(rows)
    }
}

/// The rows of `ds` whose `field` names a moment in `[from, to)` (a moment, when they are one).
pub fn rows_in(ds: &Dataset, field: &str, window: (f64, f64)) -> Vec<bool> {
    keep(dated(ds, field).as_deref(), ds.features.len(), window)
}

/// A window tested only on requested rows. It borrows the dataset and stores no per-row cache.
pub struct CandidateWindow<'a> {
    data: &'a Dataset,
    field_kind: Option<(usize, time::Kind)>,
    window: (f64, f64),
}

impl CandidateWindow<'_> {
    /// Whether this row is in the window. Missing fields or undetected dates keep every valid
    /// row, as [`rows_in`] does; a missing date in a detected field excludes its row.
    pub fn keeps(&self, row: usize) -> rustgis_data::Result<bool> {
        rustgis_data::progress::check()?;
        if row >= self.data.features.len() {
            return Ok(false);
        }
        let Some((field, kind)) = self.field_kind else { return Ok(true) };
        let (from, to) = self.window;
        Ok(time::instant(self.data.value(row, field), kind).is_some_and(|t| if from == to { t == from } else { t >= from && t < to }))
    }
}

/// Detects the same date kind as [`rows_in`], with Cancel checks even when empty values make
/// its 500-value sample span the whole layer. Only requested candidates are then converted to
/// dates, avoiding the full timestamp and visibility vectors of a binding's display cache.
pub fn candidate_window<'a>(data: &'a Dataset, field: &str, window: (f64, f64)) -> rustgis_data::Result<CandidateWindow<'a>> {
    rustgis_data::progress::check()?;
    let mut field_kind = None;
    if let Some(fi) = data.field_index(field) {
        let mut error = None;
        let kind = time::detect(
            (0..data.features.len()).map_while(|row| match rustgis_data::progress::check() {
                Ok(()) => Some(data.value(row, fi)),
                Err(e) => {
                    error = Some(e);
                    None
                }
            }),
            500,
        );
        if let Some(e) = error {
            return Err(e);
        }
        field_kind = kind.map(|(kind, _)| (fi, kind));
    }
    Ok(CandidateWindow { data, field_kind, window })
}

/// Each row's moment in `field`, or `None` when the field is gone (renamed, deleted) or holds no
/// dates.
fn dated(ds: &Dataset, field: &str) -> Option<Vec<Option<f64>>> {
    let n = ds.features.len();
    let fi = ds.field_index(field)?;
    let (kind, _) = time::detect((0..n).map(|r| ds.value(r, fi)), 500)?;
    Some((0..n).map(|r| time::instant(ds.value(r, fi), kind)).collect())
}

/// The rows whose moment is in `[from, to)` (the moment itself, when they are one); every row
/// when there are no moments to filter by (`None`).
fn keep(moments: Option<&[Option<f64>]>, n: usize, (from, to): (f64, f64)) -> Vec<bool> {
    match moments {
        Some(m) => m.iter().map(|t| t.is_some_and(|t| if from == to { t == from } else { t >= from && t < to })).collect(),
        None => vec![true; n],
    }
}

/// Each row's moment in `field` (`None` where it has none).
pub fn instants(ds: &Dataset, field: &str) -> Vec<Option<f64>> {
    let n = ds.features.len();
    let Some(fi) = ds.field_index(field) else { return vec![None; n] };
    let Some((kind, _)) = time::detect((0..n).map(|r| ds.value(r, fi)), 500) else { return vec![None; n] };
    (0..n).map(|r| time::instant(ds.value(r, fi), kind)).collect()
}

/// How many moments fall in each step: from a step up to the next (the last one `interval`
/// units long).
pub fn histogram(moments: impl Iterator<Item = f64>, steps: &[f64], unit: Unit, interval: u32) -> Vec<u32> {
    let mut out = vec![0u32; steps.len()];
    let Some(last) = steps.last() else { return out };
    let end = time::add(*last, unit, i64::from(interval.max(1)));
    for t in moments.filter(|t| *t < end) {
        // The step at or before it.
        let i = steps.partition_point(|s| *s <= t);
        if let Some(c) = i.checked_sub(1).and_then(|i| out.get_mut(i)) {
            *c = c.saturating_add(1);
        }
    }
    out
}

/// The span of the dates in a field: (first, last).
pub fn field_span(ds: &Dataset, field: &str) -> Option<(f64, f64)> {
    let fi = ds.field_index(field)?;
    let n = ds.features.len();
    let (kind, _) = time::detect((0..n).map(|r| ds.value(r, fi)), 500)?;
    (0..n).filter_map(|r| time::instant(ds.value(r, fi), kind)).fold(None, |acc, t| match acc {
        None => Some((t, t)),
        Some((a, b)) => Some((t.min(a), t.max(b))),
    })
}

/// Whether `ds` has a field of text dates or epoch milliseconds: one worth turning the slider on
/// for unasked. Whole numbers that read as years or epoch seconds may be heights, counts or ids,
/// and are bound only when asked.
pub fn has_dates(ds: &Dataset) -> bool {
    let n = ds.features.len();
    (0..ds.fields.len()).any(|fi| {
        time::detect((0..n).map(|r| ds.value(r, fi)), 500)
            .is_some_and(|(kind, share)| share >= 0.6 && matches!(kind, time::Kind::Text | time::Kind::EpochMs))
    })
}

/// The fields of `ds` that hold dates, best first, with the share of values that read as one
/// (at least 60%). Text dates come first, then epoch milliseconds, then whole numbers that read
/// as epoch seconds or years (which may be counts, heights or ids).
pub fn time_fields(ds: &Dataset) -> Vec<(String, f64)> {
    let n = ds.features.len();
    let mut out: Vec<(String, f64, u8, usize)> = ds
        .fields
        .iter()
        .enumerate()
        .filter_map(|(fi, f)| {
            let (kind, share) = time::detect((0..n).map(|r| ds.value(r, fi)), 500)?;
            if share < 0.6 {
                return None;
            }
            let mut distinct: Vec<String> = (0..n.min(500)).map(|r| ds.value(r, fi).to_string()).collect();
            distinct.sort();
            distinct.dedup();
            // Whole numbers that read as epoch seconds or years may be ids or counts: last.
            let rank = match kind {
                time::Kind::Text => 0,
                time::Kind::EpochMs => 1,
                time::Kind::EpochS | time::Kind::Year => 2,
            };
            Some((f.name.clone(), share, rank, distinct.len()))
        })
        .collect();
    out.sort_by(|a, b| a.2.cmp(&b.2).then(b.1.total_cmp(&a.1)).then(b.3.cmp(&a.3)));
    out.into_iter().map(|(n, s, ..)| (n, s)).collect()
}

#[cfg(test)]
mod candidate_window_tests {
    use super::*;
    use rustgis_data::{DataError, Field, FieldType, GeometryType, Value, progress};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn dates(values: Vec<Value>) -> Dataset {
        let mut ds = Dataset::new("dates", GeometryType::Point, vec![Field::new("date", FieldType::Text)]);
        for value in values {
            ds.push(None, vec![value]);
        }
        ds
    }

    #[test]
    fn candidate_windows_match_display_windows_without_full_caches() {
        let sets = [
            vec![Value::Text("2020-01-01".into()), Value::Text("2021-01-01".into()), Value::Null, Value::Text("bad".into())],
            vec![Value::Int(2020), Value::Int(2021), Value::Null, Value::Int(42)],
            vec![Value::Float(1_577_836_800_000.0), Value::Float(1_609_459_200_000.0), Value::Null],
            vec![Value::Float(1_577_836_800.0), Value::Float(1_609_459_200.0), Value::Null],
            vec![Value::Null, Value::Text("".into()), Value::Text("bad".into())],
        ];
        let start = time::parse_text("2020-01-01").unwrap();
        let end = time::parse_text("2021-01-01").unwrap();
        for values in sets {
            let ds = dates(values);
            for field in ["date", "missing"] {
                for window in [(start, end), (start, start), (f64::NEG_INFINITY, end)] {
                    let expected = rows_in(&ds, field, window);
                    let candidates = candidate_window(&ds, field, window).unwrap();
                    for row in (0..ds.features.len()).rev() {
                        assert_eq!(candidates.keeps(row).unwrap(), expected[row]);
                    }
                    assert!(!candidates.keeps(ds.features.len()).unwrap());
                }
            }
        }
        // The 500-value sample skips empty cells, so a late date still determines the kind.
        let mut sparse = dates(vec![Value::Null; 600]);
        sparse.push(None, vec![Value::Text("2020-01-01".into())]);
        let candidates = candidate_window(&sparse, "date", (start, end)).unwrap();
        assert!(!candidates.keeps(0).unwrap());
        assert!(candidates.keeps(600).unwrap());
    }

    /// Cancels after a known number of checks, including checks during an all-empty sample.
    struct CancelAfter(AtomicUsize);

    impl progress::Probe for CancelAfter {
        fn cancelled(&self) -> bool {
            self.0.fetch_sub(1, Ordering::Relaxed) <= 1
        }
        fn begin(&self, _: u64) {}
        fn advance(&self, _: u64) {}
    }

    #[test]
    fn candidate_windows_cancel_during_detection_and_matching() {
        let sparse = dates(vec![Value::Null; 600]);
        {
            let _guard = progress::install(Some(Arc::new(CancelAfter(AtomicUsize::new(300)))));
            assert!(matches!(candidate_window(&sparse, "date", (0.0, 1.0)), Err(DataError::Cancelled)));
        }
        let candidates = candidate_window(&sparse, "date", (0.0, 1.0)).unwrap();
        let _guard = progress::install(Some(Arc::new(CancelAfter(AtomicUsize::new(1)))));
        assert!(matches!(candidates.keeps(0), Err(DataError::Cancelled)));
    }
}
