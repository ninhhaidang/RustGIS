//! The chart element's data: bars, order, look and stacks.

use super::*;

/// The bars a chart draws unless told otherwise, and the most it takes.
pub const CHART_BARS: usize = 20;
pub const MAX_CHART_BARS: usize = 100;

pub(super) fn chart_bars() -> usize {
    CHART_BARS
}

pub(super) fn is_chart_bars(n: &usize) -> bool {
    *n == CHART_BARS
}

/// How a chart orders its bars.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChartOrder {
    /// The largest value first.
    #[default]
    Descending,
    /// The smallest value first.
    Ascending,
    /// By label, A to Z (case aside).
    Category,
    /// As the features come in the layer (grouped, as each value first comes).
    Layer,
}

impl ChartOrder {
    pub const ALL: [ChartOrder; 4] = [ChartOrder::Descending, ChartOrder::Ascending, ChartOrder::Category, ChartOrder::Layer];
    pub fn code(self) -> &'static str {
        match self {
            ChartOrder::Descending => "descending",
            ChartOrder::Ascending => "ascending",
            ChartOrder::Category => "category",
            ChartOrder::Layer => "layer",
        }
    }
    pub fn from_code(s: &str) -> Option<ChartOrder> {
        ChartOrder::ALL.into_iter().find(|o| o.code().eq_ignore_ascii_case(s))
    }
    pub fn label(self) -> &'static str {
        match self {
            ChartOrder::Descending => "Largest first",
            ChartOrder::Ascending => "Smallest first",
            ChartOrder::Category => "By label",
            ChartOrder::Layer => "Layer order",
        }
    }
}

/// The most decimals a chart writes its values with.
pub const MAX_CHART_DECIMALS: u8 = 6;

/// A chart's bars' colour unless given one.
pub const CHART_BAR: Rgba = Rgba::rgb(0x3B, 0x82, 0xC4);

/// How a chart draws its bars ([`ElementKind::chart_look`]), its data aside.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChartLook {
    pub orientation: Orientation,
    pub values: bool,
    pub decimals: Option<u8>,
    pub bar_color: Rgba,
    pub label_align: TextAlign,
    /// Drawn as its mirror image ([`ElementKind::Chart`]'s `mirror`).
    pub mirror: bool,
    /// The value at the far end of its bars' room, above 0; none, its bars' own.
    pub max: Option<f64>,
    pub hide_labels: bool,
    /// The line round each bar, as it is read ([`chart_outline`]); none, no line.
    pub bar_outline: Option<ChartOutline>,
}

/// A chart's bar outline (#2487 part 4b): its colour and width in points. A key left out of a
/// hand-edited file takes the one it is first given ([`CHART_OUTLINE`]).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChartOutline {
    #[serde(default = "chart_outline_color")]
    pub color: Rgba,
    #[serde(default = "chart_outline_width")]
    pub width: f64,
}

pub(super) fn chart_outline_color() -> Rgba {
    CHART_OUTLINE.color
}

pub(super) fn chart_outline_width() -> f64 {
    CHART_OUTLINE.width
}

/// A bar outline's colour and width when first given, and the widest it is drawn.
pub const CHART_OUTLINE: ChartOutline = ChartOutline { color: Rgba::rgb(0x22, 0x22, 0x22), width: 0.5 };
pub const MAX_CHART_OUTLINE_WIDTH: f64 = 5.0;

/// A chart's bar outline as it is read: a width above 0 and finite, drawn at most
/// [`MAX_CHART_OUTLINE_WIDTH`]; else none.
pub fn chart_outline(o: Option<ChartOutline>) -> Option<ChartOutline> {
    o.filter(|o| o.width.is_finite() && o.width > 0.0).map(|o| ChartOutline { width: o.width.min(MAX_CHART_OUTLINE_WIDTH), ..o })
}

impl Default for ChartLook {
    fn default() -> Self {
        ChartLook {
            orientation: Orientation::Horizontal,
            values: false,
            decimals: None,
            bar_color: CHART_BAR,
            label_align: TextAlign::Left,
            mirror: false,
            max: None,
            hide_labels: false,
            bar_outline: None,
        }
    }
}

/// A chart's fixed maximum as it is read: above 0 and finite, else none (its bars' own).
pub fn chart_max(max: Option<f64>) -> Option<f64> {
    max.filter(|m| m.is_finite() && *m > 0.0)
}

/// A chart's value as written on its bar: its thousands apart with commas, with `decimals`
/// decimals, or none given, up to two, the zeros they end in dropped (`2,095`, `13.3`, `-0.25`).
/// The point is a decimal point; a layout with a decimal comma swaps the two when it prints it.
pub fn chart_value_text(v: f64, decimals: Option<u8>) -> String {
    let d = decimals.unwrap_or(2).min(MAX_CHART_DECIMALS) as usize;
    let mut s = format!("{:.*}", d, v.abs());
    if decimals.is_none() && s.contains('.') {
        s.truncate(s.trim_end_matches('0').trim_end_matches('.').len());
    }
    let (whole, fraction) = s.split_once('.').map_or((s.as_str(), None), |(w, f)| (w, Some(f)));
    let mut out = String::new();
    // Rounded to nothing, a negative value is written as 0, not -0.
    if v < 0.0 && s.bytes().any(|b| (b'1'..=b'9').contains(&b)) {
        out.push('-');
    }
    out.push_str(&crate::graphics::commas(whole));
    if let Some(f) = fraction {
        out.push('.');
        out.push_str(f);
    }
    out
}

/// What a bar with no value of a chart's `group` field is labelled, as a report's chart does.
pub const CHART_BLANK: &str = "(blank)";
/// The label of the bar gathering a chart's bars past its limit.
pub const CHART_OTHER: &str = "Other";

/// The bars a chart draws from its layer, and what it leaves out of them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChartData {
    /// Each bar's value and label, in the order drawn.
    pub bars: Vec<(f64, String)>,
    /// Whether its bars have labels: when grouped, or labelled by a field.
    pub labelled: bool,
    /// Bars past its limit, left out (none when gathered in an "Other" bar).
    pub left_out: usize,
    /// Bars whose statistic is past a number's range (a sum or mean overflowing), left out; an
    /// Other bar's counts the bars it would gather.
    pub out_of_range: usize,
    /// Features with no number in its field (not a number, or not finite), left out; a count
    /// counts them.
    pub no_number: usize,
    /// Its title: the field, or the statistic of the field by the group.
    pub title: String,
    /// Coloured by classes, each bar's colour as its features have it on the map, in the order of
    /// `bars`: `None` keeps the chart's bar colour (an Other bar, or a bar whose features have
    /// several or none). Empty when not coloured by classes, and when stacked: its stretches
    /// take the colours of its `key`.
    pub colors: Vec<Option<Rgba>>,
    /// Bars drawn whose features have several colours on the map, which keep the chart's.
    pub mixed: usize,
    /// Bars drawn whose features show no colour on the map (a hidden class, points drawn as an
    /// image), which keep the chart's.
    pub unshown: usize,
    /// Stacked, the key: each value of the stack field the bars drawn show, with its colour, in the
    /// order its stretch takes along every bar, a last [`CHART_OTHER`] gathering the values past
    /// [`CHART_STACKS`]. Empty when not stacked.
    pub key: Vec<(String, Rgba)>,
    /// Stacked, each bar's stretches in the order of `key` (0 where the bar has none of that
    /// value), which add up to the bar's value. Empty when not stacked.
    pub stretches: Vec<Vec<f64>>,
    /// Stacked and coloured by classes, the values of the key that keep their palette colour:
    /// their features have several colours on the map, or show none.
    pub key_kept: usize,
    /// Stacked, the values the bars drawn show past [`CHART_STACKS`], gathered in the key's Other.
    pub key_gathered: usize,
    /// Stacked, the bars drawn whole (their `stretches` empty): a stretch of theirs, or their
    /// stretches stacked, add up past a number's range, though their total doesn't.
    pub whole: usize,
    /// Its stack field isn't in the layer (deleted since, say): its bars are drawn whole.
    pub stack_gone: bool,
}

/// The most values of a stack field a chart's key lists, one per colour of
/// [`crate::diagram::CATEGORY_COLORS`]: past it, all but the last, and an Other for the rest.
pub const CHART_STACKS: usize = crate::diagram::CATEGORY_COLORS.len();

/// The colour a grouped bar's features share on the map ([`ElementKind::chart_data_of`]): none
/// read yet (or only features drawn with nothing), one, or several.
#[derive(Clone, Copy, Debug)]
pub(super) enum Shade {
    Unread,
    One(Rgba),
    Mixed,
}

impl Shade {
    /// The colour two sets of features share: the one they both show, or either's when the other
    /// shows none.
    pub(super) fn merge(&mut self, other: Shade) {
        match other {
            Shade::Unread => {}
            Shade::One(c) => self.add(Some(c)),
            Shade::Mixed => *self = Shade::Mixed,
        }
    }

    /// A feature drawn with nothing (a hidden class) shows no colour, so it leaves the bar the
    /// colour of the features that do show.
    pub(super) fn add(&mut self, c: Option<Rgba>) {
        let Some(c) = c else { return };
        *self = match *self {
            Shade::Unread => Shade::One(c),
            Shade::One(o) if o == c => Shade::One(o),
            _ => Shade::Mixed,
        };
    }
}

/// The values of a field a chart groups or stacks its bars by, each numbered as it first comes
/// ([`ElementKind::chart_data`]). Each name is held once, in `index`; a text is looked up as it
/// is, and only a new value's name is copied. A number or a yes/no is looked up by its value in
/// `by_value` first, so it is written out once per value, not per feature: a value first seen goes
/// through `index` by its name, so values written alike (3 and 3.0) stay one. No value, or only
/// spaces, is the one blank value (apart from a value that reads "(blank)").
#[derive(Default)]
pub(super) struct ValueIndex {
    pub(super) index: std::collections::HashMap<String, usize>,
    pub(super) by_value: std::collections::HashMap<GroupKey, usize>,
    pub(super) blank: Option<usize>,
    pub(super) len: usize,
}

impl ValueIndex {
    /// The number of `value`, a new one for a value not seen before.
    pub(super) fn slot(&mut self, value: &rustgis_data::Value) -> usize {
        match (value, GroupKey::of(value)) {
            (rustgis_data::Value::Null, _) => self.named(None),
            (rustgis_data::Value::Text(t), _) => self.named(Some(t)),
            (v, Some(key)) => match self.by_value.get(&key) {
                Some(at) => *at,
                None => {
                    let at = self.named(Some(&v.to_string()));
                    self.by_value.insert(key, at);
                    at
                }
            },
            (v, None) => self.named(Some(&v.to_string())),
        }
    }

    pub(super) fn named(&mut self, text: Option<&str>) -> usize {
        let at = match text.filter(|t| !t.trim().is_empty()) {
            None => match self.blank {
                Some(at) => return at,
                None => {
                    self.blank = Some(self.len);
                    self.len
                }
            },
            Some(t) => match self.index.get(t) {
                Some(at) => return *at,
                None => {
                    self.index.insert(t.to_string(), self.len);
                    self.len
                }
            },
        };
        self.len += 1;
        at
    }

    /// Each value's name by its number, none for the blank one.
    pub(super) fn names(self) -> Vec<Option<String>> {
        let mut names = vec![None; self.len];
        for (name, i) in self.index {
            if let Some(slot) = names.get_mut(i) {
                *slot = Some(name);
            }
        }
        names
    }
}

/// Why a chart grouped by `group` and taking `statistic` can't stack its bars by `stack`; none
/// when it can (#2487 part 3b). It stacks grouped by another field, summing or counting, so that
/// its stretches add up to their bar. Commands refuse a stack given otherwise and let go one a
/// change leaves unable to stack, with this reason; the Format pane offers only fields it takes.
pub fn chart_stack_refusal(group: Option<&str>, statistic: StatKind, stack: &str) -> Option<&'static str> {
    match group {
        _ if stack.trim().is_empty() => Some("it names no field"),
        None => Some("it doesn't group its bars"),
        Some(g) if g.trim().eq_ignore_ascii_case(stack.trim()) => Some("it groups by that field"),
        Some(_) if !matches!(statistic, StatKind::Sum | StatKind::Count) => {
            Some("a mean, least or greatest of its stretches wouldn't add up to its bar")
        }
        Some(_) => None,
    }
}

/// Adds a run of features' total and colour to their pair's in `cells`.
pub(super) fn add_cell(
    cells: &mut std::collections::BTreeMap<(usize, usize), (f64, Shade)>,
    pair: (usize, usize),
    (total, shade): (f64, Shade),
) {
    let (t, s) = cells.entry(pair).or_insert((0.0, Shade::Unread));
    *t += total;
    s.merge(shade);
}

/// A stacked chart's key and stretches ([`ElementKind::chart_data`]) from the total of each pair of
/// a group value and a stack value (`cells`), the bar each group is drawn in (`bar_of`, an Other
/// bar's groups all in the last of `bars`) and the stack values' names; `coloured`, each pair's
/// colour gives a value the one its features share over the bars shown. The key holds the values
/// the bars show, by their total over them, largest first; past [`CHART_STACKS`], the largest all
/// but the last and an Other for the rest; `by_label`, those kept A to Z. Returns the key, each
/// bar's stretches by the key's order, the values of the key keeping their palette colour though
/// coloured, the values gathered, and the bars drawn whole (their stretches empty) as a stretch of
/// theirs is past a number's range.
pub(super) fn stack_bars(
    cells: &std::collections::BTreeMap<(usize, usize), (f64, Shade)>,
    bar_of: &[Option<usize>],
    bars: usize,
    mut names: Vec<Option<String>>,
    coloured: bool,
    by_label: bool,
) -> (Vec<(String, Rgba)>, Vec<Vec<f64>>, usize, usize, usize) {
    // The map is read in its own order, so an Other stretch is always the same sum.
    let shown = || cells.iter().filter_map(|((g, k), (t, shade))| Some((bar_of.get(*g).copied().flatten()?, *k, *t, *shade)));
    let mut totals: Vec<Option<f64>> = vec![None; names.len()];
    let mut shades = vec![Shade::Unread; names.len()];
    for (_, k, t, shade) in shown() {
        if let (Some(s), Some(c)) = (totals.get_mut(k), shades.get_mut(k)) {
            *s = Some(s.unwrap_or(0.0) + t);
            c.merge(shade);
        }
    }
    // The key keeps the values with the largest totals, ordered by label once kept.
    // A value whose total is past a number's range, a pair's or once added up, orders as 0.
    let mut by_total: Vec<(f64, usize)> =
        totals.iter().enumerate().filter_map(|(k, t)| t.map(|t| (if t.is_finite() { t } else { 0.0 }, k))).collect();
    by_total.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let own = if by_total.len() > CHART_STACKS { CHART_STACKS - 1 } else { by_total.len() };
    if let Some(kept) = by_label.then(|| by_total.get_mut(..own)).flatten() {
        kept.sort_by_cached_key(|(_, k)| names.get(*k).and_then(|n| n.as_deref()).unwrap_or(CHART_BLANK).to_lowercase());
    }
    let mut key_of = vec![own; names.len()];
    let (mut key, mut kept) = (Vec::with_capacity(own + 1), 0);
    for (at, (_, k)) in by_total.iter().take(own).enumerate() {
        if let Some(slot) = key_of.get_mut(*k) {
            *slot = at;
        }
        let palette = crate::diagram::CATEGORY_COLORS.get(at).copied().unwrap_or(crate::symbology::OTHER_FILL);
        let colour = match (coloured, shades.get(*k)) {
            (false, _) => palette,
            (true, Some(Shade::One(c))) => *c,
            (true, _) => {
                kept += 1;
                palette
            }
        };
        let name = names.get_mut(*k).and_then(Option::take).unwrap_or_else(|| CHART_BLANK.to_string());
        key.push((name, colour));
    }
    let gathered = by_total.len() - own;
    if gathered > 0 {
        key.push((CHART_OTHER.to_string(), crate::symbology::OTHER_FILL));
    }
    let mut stretches = vec![vec![0.0; key.len()]; bars];
    for (bar, k, t, _) in shown() {
        if let Some(s) = key_of.get(k).and_then(|at| stretches.get_mut(bar)?.get_mut(*at)) {
            *s += t;
        }
    }
    // A bar a stretch of which adds up past a number's range, or whose stretches above zero or
    // below it do once stacked (features of opposite signs near the largest number, its total
    // still within it), is drawn whole, with no stretches.
    let past = |s: &[f64]| {
        let (lo, hi) = s.iter().fold((0.0_f64, 0.0_f64), |(lo, hi), v| if *v < 0.0 { (lo + v, hi) } else { (lo, hi + v) });
        !(lo.is_finite() && hi.is_finite())
    };
    let mut whole = 0;
    for s in stretches.iter_mut().filter(|s| past(s)) {
        s.clear();
        whole += 1;
    }
    (key, stretches, kept, gathered, whole)
}

/// A chart's group value looked up by itself rather than by its name ([`ElementKind::chart_data`]):
/// a whole number, a number's bits, or a yes/no. The same key is always written alike, so a bar
/// found by it is the bar of that name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum GroupKey {
    Whole(i64),
    Bits(u64),
    Bool(bool),
}

impl GroupKey {
    /// `v`'s key; none for a text, a date or no value, looked up by name.
    pub(super) fn of(v: &rustgis_data::Value) -> Option<GroupKey> {
        match v {
            rustgis_data::Value::Int(i) => Some(GroupKey::Whole(*i)),
            rustgis_data::Value::Float(f) => Some(GroupKey::Bits(f.to_bits())),
            rustgis_data::Value::Bool(b) => Some(GroupKey::Bool(*b)),
            _ => None,
        }
    }
}
