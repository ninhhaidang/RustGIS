//! A layer's statistics in a layout's text: the tokens and their figures.

use super::*;

/// A layer's figure in a layout text (#2853): `{count:<layer>}`, or `{sum:<layer>:<field>}` and
/// the same with `mean`, `min` and `max`. The layer is everything between the first colon and the
/// field's (the last), so a layer named with a colon still reads; a count's, everything after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatToken<'a> {
    /// The token as written, braces included.
    pub token: &'a str,
    pub stat: StatKind,
    pub layer: &'a str,
    /// None for a count.
    pub field: Option<&'a str>,
}

/// The [`StatToken`]s in `text`, in the order they come; other braces (`{scale}`, `{foo}`,
/// `{sum:}`) are not among them.
pub fn stat_tokens(text: &str) -> Vec<StatToken<'_>> {
    scan_stat_tokens(text, false)
}

/// [`stat_tokens`] and also the `{sum:Layer}`, `{mean:Layer}`, `{min:Layer}` and `{max:Layer}` written
/// without their field, which are no tokens to draw but a figure the writer meant: for a notice.
pub fn stat_tokens_and_gaps(text: &str) -> Vec<StatToken<'_>> {
    scan_stat_tokens(text, true)
}

pub(super) fn scan_stat_tokens(text: &str, gaps: bool) -> Vec<StatToken<'_>> {
    let mut out = Vec::new();
    for (open, _) in text.match_indices('{') {
        let Some(after) = text.get(open + 1..) else { continue };
        // The first brace after it ends the token; another opening one means this isn't one.
        let Some(len) = after.find(['{', '}']).filter(|&i| after.get(i..).is_some_and(|r| r.starts_with('}'))) else { continue };
        let (Some(inner), Some(token)) = (after.get(..len), text.get(open..open + len + 2)) else { continue };
        let Some((code, rest)) = inner.split_once(':') else { continue };
        let stat = match code {
            "count" => StatKind::Count,
            "sum" => StatKind::Sum,
            "mean" => StatKind::Mean,
            "min" => StatKind::Min,
            "max" => StatKind::Max,
            _ => continue,
        };
        let (layer, field) = match stat {
            StatKind::Count => (rest, None),
            _ => match rest.rsplit_once(':') {
                Some((layer, field)) if !field.is_empty() => (layer, Some(field)),
                // No field, or an empty one: only a notice reads it.
                Some((layer, _)) if gaps => (layer, None),
                None if gaps => (rest, None),
                _ => continue,
            },
        };
        if !layer.is_empty() {
            out.push(StatToken { token, stat, layer, field });
        }
    }
    out
}

/// `text` with the layer of each statistic token put in another name by `rename` (given the layer's
/// name, and the name to write in its place, or none to leave it): none when no token changes. The
/// layer a token names is read as [`stat_tokens`] reads it.
pub fn rename_stat_layers(text: &str, mut rename: impl FnMut(&str) -> Option<String>) -> Option<String> {
    let mut out = text.to_string();
    let mut changed = false;
    let mut done: Vec<&str> = Vec::new();
    for t in stat_tokens(text) {
        if done.contains(&t.token) {
            continue;
        }
        done.push(t.token);
        let Some(layer) = rename(t.layer) else { continue };
        let code = t.stat.code();
        let new = match t.field {
            Some(field) => format!("{{{code}:{layer}:{field}}}"),
            None => format!("{{{code}:{layer}}}"),
        };
        if new != t.token {
            out = out.replace(t.token, &new);
            changed = true;
        }
    }
    changed.then_some(out)
}

/// Why a [`StatToken`] has no figure: drawn as written instead, and said when the text is set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatMissing {
    /// The layer has no such field (or a sum, mean, least or greatest names none).
    Field,
    /// The field holds no numbers to add up.
    NotNumeric,
    /// A raster layer, which has no features to count.
    Raster,
    /// None of the features the layer's definition query keeps has a number in the field, or the
    /// statistic is past a number's range.
    NoValue,
}

/// `stat` of `layer`'s `field` (none for a count) over the features its definition query keeps
/// ([`crate::Layer::visible_rows`]), its numbers read as a chart's are (finite, nulls skipped).
pub fn layer_statistic(layer: &crate::Layer, stat: StatKind, field: Option<&str>) -> Result<f64, StatMissing> {
    layer_statistics(layer, &[(stat, field)]).into_iter().next().unwrap_or(Err(StatMissing::NoValue))
}

/// [`layer_statistic`] for each of `wanted` (a statistic and its field), in one pass over the layer's
/// rows: the rows its definition query keeps are worked out once, and every field wanted is tallied
/// in that pass, so a text with many figures of one layer costs one pass, not one for each.
pub fn layer_statistics(layer: &crate::Layer, wanted: &[(StatKind, Option<&str>)]) -> Vec<Result<f64, StatMissing>> {
    if layer.raster.is_some() {
        return wanted.iter().map(|_| Err(StatMissing::Raster)).collect();
    }
    let ds = &layer.dataset;
    // Each statistic's field as a column, or why it has none; the columns, once each.
    let mut columns: Vec<usize> = Vec::new();
    let slots: Vec<Result<Option<usize>, StatMissing>> = wanted
        .iter()
        .map(|(stat, field)| {
            let fi = match (stat, field) {
                (StatKind::Count, _) => return Ok(None),
                (_, Some(f)) => ds.field_index(f).ok_or(StatMissing::Field)?,
                (_, None) => return Err(StatMissing::Field),
            };
            if !ds.fields.get(fi).is_some_and(|f| f.ty.is_numeric()) {
                return Err(StatMissing::NotNumeric);
            }
            let slot = columns.iter().position(|c| *c == fi).unwrap_or_else(|| {
                columns.push(fi);
                columns.len() - 1
            });
            Ok(Some(slot))
        })
        .collect();
    let mut records = 0usize;
    let mut tallies = vec![Tally::default(); columns.len()];
    if slots.iter().any(Result::is_ok) {
        for (row, kept) in layer.visible_rows().into_iter().enumerate() {
            if !kept {
                continue;
            }
            records += 1;
            for (tally, fi) in tallies.iter_mut().zip(&columns) {
                if let Some(n) = ds.value(row, *fi).as_f64().filter(|v| v.is_finite()) {
                    tally.add(n, true);
                }
            }
        }
    }
    wanted
        .iter()
        .zip(slots)
        .map(|((stat, _), slot)| match slot? {
            None => Tally { records, ..Tally::default() }.value(*stat).ok_or(StatMissing::NoValue),
            Some(slot) => Tally { records, ..tallies[slot] }.value(*stat).ok_or(StatMissing::NoValue),
        })
        .collect()
}

/// A chart bar's features as its statistic reads them: how many, and their numbers' sum, mean,
/// least and greatest.
#[derive(Clone, Copy, Debug)]
pub(super) struct Tally {
    pub(super) records: usize,
    pub(super) numbers: usize,
    pub(super) sum: f64,
    /// Kept as the numbers come, each one's share of it added in turn, so it stays in a number's
    /// range where their sum doesn't (values near `f64::MAX`); read only then.
    pub(super) mean: f64,
    pub(super) min: f64,
    pub(super) max: f64,
}

impl Default for Tally {
    fn default() -> Self {
        Tally { records: 0, numbers: 0, sum: 0.0, mean: 0.0, min: f64::INFINITY, max: f64::NEG_INFINITY }
    }
}

impl Tally {
    /// Adds `n`, and to the kept mean when `mean` (only a mean reads it).
    pub(super) fn add(&mut self, n: f64, mean: bool) {
        self.numbers += 1;
        self.sum += n;
        if mean {
            // Divided before they are taken apart: `n - mean` itself may be past the range.
            let k = self.numbers as f64;
            self.mean += n / k - self.mean / k;
        }
        self.min = self.min.min(n);
        self.max = self.max.max(n);
    }

    pub(super) fn merge(&mut self, other: Option<&Tally>) {
        let Some(o) = other else { return };
        self.records += o.records;
        if o.numbers > 0 {
            // Moved towards the other's mean by the other's share of the numbers. Of the same sign,
            // their weighted difference stays in range and equal means stay as they are; of opposite
            // signs, their weighted sum does.
            let w = o.numbers as f64 / (self.numbers + o.numbers) as f64;
            if (self.mean < 0.0) == (o.mean < 0.0) {
                self.mean += o.mean * w - self.mean * w;
            } else {
                self.mean = self.mean * (1.0 - w) + o.mean * w;
            }
        }
        self.numbers += o.numbers;
        self.sum += o.sum;
        self.min = self.min.min(o.min);
        self.max = self.max.max(o.max);
    }

    /// Its statistic, read from the tally as [`StatKind::apply`] reads it from the numbers; none
    /// without a number (a count but one), or past a number's range.
    pub(super) fn value(&self, stat: StatKind) -> Option<f64> {
        let v = match stat {
            StatKind::Count => self.records as f64,
            _ if self.numbers == 0 => return None,
            StatKind::Sum => self.sum,
            // Their sum over their number while it is a number, so a mean reads as it always has;
            // past the range, the mean kept as they came.
            StatKind::Mean if self.sum.is_finite() => self.sum / self.numbers as f64,
            StatKind::Mean => self.mean,
            StatKind::Min => self.min,
            StatKind::Max => self.max,
        };
        Some(v).filter(|v| v.is_finite())
    }
}
