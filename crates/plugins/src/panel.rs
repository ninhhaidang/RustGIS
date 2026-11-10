//! A plugin's panel: a pane of simple widgets (text, checkboxes, sliders, buttons, trees,
//! dropdowns, date pickers, range sliders and picks on the map) bound to its commands, declared
//! in `plugin.json` (`panel`).
//!
//! A plugin keeps no state between calls (each gets a fresh instance), so the host keeps the
//! panel's state: the widgets' values and what the plugin last said about them. Every command
//! request of a plugin with a panel carries the values (`panel` in the request), and any command
//! may answer with the panel's new state (`panel` in its answer), which is checked like any
//! other hostile input before it is kept. Nothing here knows about the user interface: the
//! engine keeps a [`PanelState`] per plugin in its session, and the front end draws it.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// The most widgets a panel may declare.
pub const MAX_ITEMS: usize = 32;
/// The longest panel title, in characters.
pub const MAX_TITLE: usize = 48;
/// A slider's ends stay within this, either way.
pub const SLIDER_LIMIT: i64 = 1_000_000_000;
/// The most labels a slider may have (one per position), and the longest one, in characters.
pub const MAX_SLIDER_LABELS: usize = 2_000;
pub const MAX_SLIDER_LABEL: usize = 80;
/// The longest text or label a plugin may set, in characters.
pub const MAX_STATE_TEXT: usize = 4_000;
pub const MAX_STATE_LABEL: usize = 200;
/// The longest declared label, text or help, in characters.
pub const MAX_DECLARED: usize = 2_000;
/// The most nodes a tree holds, how deep they go, and the longest node id, in characters.
pub const MAX_TREE_NODES: usize = 5_000;
pub const MAX_TREE_DEPTH: usize = 8;
pub const MAX_TREE_ID: usize = 200;
/// The most options a dropdown (`choice`) holds.
pub const MAX_CHOICES: usize = 1_000;
/// The widget kinds.
pub const KINDS: &[&str] = &["text", "checkbox", "slider", "button", "tree", "choice", "date", "range", "pick"];
/// What a pick asks for on the map: a point (the default) or a box.
pub const PICK_SHAPES: &[&str] = &["point", "box"];
/// The longest coordinate system a pick's value names, in characters (a PROJ definition).
pub const MAX_PICK_CRS: usize = 2_000;

/// The panel as `plugin.json` declares it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PanelDecl {
    /// The pane's title (its tab), and the label of the ribbon button that opens it.
    pub title: String,
    /// One of RustGIS's own icons for that button (`plugin` when unset or unknown).
    #[serde(default)]
    pub icon: String,
    /// Shown on hover over that button.
    #[serde(default)]
    pub help: String,
    /// A command of the plugin's run each time the panel opens, to answer the panel's state
    /// (empty for none).
    #[serde(default)]
    pub state: String,
    pub items: Vec<PanelItemDecl>,
}

/// A widget. `kind` is `text`, `checkbox`, `slider`, `button`, `tree`, `choice` (a dropdown),
/// `date` (a date picker), `range` (a slider with two handles) or `pick` (a place on the map);
/// the other fields apply to some.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PanelItemDecl {
    pub kind: String,
    /// How the plugin names it in values and states. A button's defaults to its command.
    #[serde(default)]
    pub id: String,
    /// A caption (text, slider), the box's label (checkbox) or the button's label (a button's or a
    /// pick's; its command's label when unset).
    #[serde(default)]
    pub label: String,
    /// Shown on hover.
    #[serde(default)]
    pub help: String,
    /// A text's first contents, until the plugin sets them.
    #[serde(default)]
    pub text: String,
    /// The command it runs: a button's when clicked; a checkbox's when ticked or cleared; a
    /// slider's or a range's when let go (or stepped); a tree's or a dropdown's when a node or an
    /// option is chosen; a date's when a date is picked; a pick's when the place is picked on the
    /// map. Optional for all but buttons, whose
    /// values then just go with the plugin's next request.
    #[serde(default)]
    pub command: String,
    /// A checkbox's first value (a bool), a slider's (a whole number; its `min` if unset), a
    /// range's (`[low, high]`; its ends if unset), a dropdown's (one of its `options`' ids) or a
    /// date's (`YYYY-MM-DD`).
    #[serde(default)]
    pub default: Option<Value>,
    /// A dropdown's options, until the plugin sets them: `[{"id", "label"?}]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<Value>,
    /// A slider's or a range's ends (whole numbers), until the plugin sets them.
    #[serde(default)]
    pub min: i64,
    #[serde(default)]
    pub max: i64,
    /// A slider with ‹ and › buttons that step it by one (a range's both handles together).
    #[serde(default)]
    pub steps: bool,
    /// A tree with a search box above it, which keeps the nodes whose label holds what's typed.
    #[serde(default)]
    pub search: bool,
    /// What a pick asks for on the map: `point` (when unset) or `box`.
    #[serde(default)]
    pub shape: String,
}

impl PanelItemDecl {
    /// Its id: `id`, or a button's command.
    pub fn key(&self) -> &str {
        if self.id.is_empty() && self.kind == "button" { &self.command } else { &self.id }
    }

    /// Whether it has a value (a checkbox, a slider, a tree, a dropdown, a date, a range or a
    /// pick).
    pub fn has_value(&self) -> bool {
        matches!(self.kind.as_str(), "checkbox" | "slider" | "tree" | "choice" | "date" | "range" | "pick")
    }

    /// Whether it is a pick that asks for a box (a pick with no shape asks for a point).
    pub fn picks_box(&self) -> bool {
        self.kind == "pick" && self.shape == "box"
    }

    /// Whether it has ends (a slider or a range).
    fn ranged(&self) -> bool {
        matches!(self.kind.as_str(), "slider" | "range")
    }
}

impl PanelDecl {
    /// The widget with id `id`.
    pub fn item(&self, id: &str) -> Option<&PanelItemDecl> {
        self.items.iter().find(|i| i.key() == id)
    }

    /// Checks the panel; `command` says whether the plugin has a command by that id.
    pub fn validate(&self, command: &dyn Fn(&str) -> bool) -> Result<(), String> {
        let title = self.title.trim();
        if title.is_empty() || title.chars().count() > MAX_TITLE || title.contains('\n') {
            return Err(format!("plugin.json: the panel's title must be one line of 1 to {MAX_TITLE} characters"));
        }
        check("panel help", &self.help)?;
        check("panel icon", &self.icon)?;
        if !self.state.is_empty() && !command(&self.state) {
            return Err(format!("plugin.json: the panel's state command '{}' isn't one of the plugin's commands", clip(&self.state, 80)));
        }
        if self.items.is_empty() || self.items.len() > MAX_ITEMS {
            return Err(format!("plugin.json: the panel needs 1 to {MAX_ITEMS} items"));
        }
        let mut ids = HashSet::new();
        for item in &self.items {
            let kind = item.kind.as_str();
            if !KINDS.contains(&kind) {
                return Err(format!(
                    "plugin.json: panel item kind '{}' isn't text, checkbox, slider, button, tree, choice, date, range or pick",
                    clip(kind, 40)
                ));
            }
            let id = item.key();
            if !crate::manifest::valid_id(id, false) || !ids.insert(id) {
                return Err(format!(
                    "plugin.json: panel item id '{}' is invalid or repeated (lower-case letters, digits and hyphens; a button's is its command's unless given)",
                    clip(id, 80)
                ));
            }
            check("panel item label", &item.label)?;
            check("panel item help", &item.help)?;
            check("panel item text", &item.text)?;
            let what = |s: &str| format!("plugin.json: panel item '{id}': {s}");
            match kind {
                "text" if !item.command.is_empty() => return Err(what("a text runs no command")),
                "button" if item.command.is_empty() => return Err(what("a button needs a command")),
                "checkbox" if item.label.trim().is_empty() => return Err(what("a checkbox needs a label")),
                "pick" if item.label.trim().is_empty() && item.command.is_empty() => return Err(what("a pick needs a label or a command")),
                _ => {}
            }
            if kind == "pick" && !item.shape.is_empty() && !PICK_SHAPES.contains(&item.shape.as_str()) {
                return Err(what(&format!("a pick's shape '{}' isn't point or box", clip(&item.shape, 40))));
            }
            if kind != "pick" && !item.shape.is_empty() {
                return Err(what("shape is for picks"));
            }
            if !item.command.is_empty() && !command(&item.command) {
                return Err(what(&format!("'{}' isn't one of the plugin's commands", clip(&item.command, 80))));
            }
            if !item.ranged() && (item.min != 0 || item.max != 0 || item.steps) {
                return Err(what("min, max and steps are for sliders and ranges"));
            }
            if kind != "choice" && !item.options.is_empty() {
                return Err(what("options are for dropdowns (choice)"));
            }
            let mut option_changes = Dropped::default();
            let options = if kind == "choice" { choice_options_reported(&item.options, &mut option_changes) } else { Vec::new() };
            if let Some(notice) = option_changes.notice() {
                return Err(what(&format!("option labels or fields are invalid: {notice}")));
            }
            if options.len() != item.options.len() {
                return Err(what(&format!(
                    "a dropdown's options are at most {MAX_CHOICES} objects, each with a unique id of 1 to {MAX_TREE_ID} characters"
                )));
            }
            if kind != "text" && !item.text.is_empty() {
                return Err(what("text is for texts"));
            }
            if kind != "tree" && item.search {
                return Err(what("search is for trees"));
            }
            match (kind, &item.default) {
                (_, None) | ("checkbox", Some(Value::Bool(_))) => {}
                ("slider", Some(v)) if whole(v).is_some_and(|v| (item.min..=item.max).contains(&v)) => {}
                ("slider", Some(_)) => return Err(what("a slider's default is a whole number from min to max")),
                ("range", Some(v)) if pair(v).is_some_and(|(a, b)| item.min <= a && a <= b && b <= item.max) => {}
                ("range", Some(_)) => return Err(what("a range's default is [low, high], whole numbers from min to max, low first")),
                ("checkbox", Some(_)) => return Err(what("a checkbox's default is true or false")),
                ("choice", Some(Value::String(d))) if options.iter().any(|o| o.id == *d) => {}
                ("choice", Some(_)) => return Err(what("a dropdown's default is the id of one of its options")),
                ("date", Some(Value::String(d))) if date(d).is_some() => {}
                ("date", Some(_)) => return Err(what("a date's default is a date written YYYY-MM-DD")),
                _ => return Err(what("only checkboxes, sliders, ranges, dropdowns and dates have a default")),
            }
            if item.ranged() && (item.min > item.max || item.min < -SLIDER_LIMIT || item.max > SLIDER_LIMIT) {
                return Err(what(&format!("a slider's or a range's min must not be above its max, both within ±{SLIDER_LIMIT}")));
            }
        }
        Ok(())
    }
}

fn check(what: &str, s: &str) -> Result<(), String> {
    if s.chars().count() > MAX_DECLARED { Err(format!("plugin.json: {what} is too long")) } else { Ok(()) }
}

/// At most `n` characters of `s`.
fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// A bounded, single-line name for a diagnostic, without terminal control characters.
fn diagnostic_name(s: &str) -> String {
    s.chars()
        .take(MAX_TREE_ID)
        .map(|c| {
            if c.is_control() || matches!(c,'\u{061c}'|'\u{200e}'|'\u{200f}'|'\u{2028}'..='\u{202e}'|'\u{2066}'..='\u{2069}') {
                ' '
            } else {
                c
            }
        })
        .collect()
}

/// A whole number from an integer or a float with no fraction (JSON from other languages).
fn whole(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_f64().filter(|x| x.is_finite() && x.fract() == 0.0 && x.abs() <= SLIDER_LIMIT as f64).map(|x| x as i64))
}

/// A pick's value from `v`, as it is kept: `null` for none; for a point `{"lon", "lat"}`, for a
/// box `{"bbox": [west, south, east, north]}` (one whose west longitude is greater than its east
/// crosses the antimeridian), in degrees, with what the host works out in the map's coordinates
/// (`x` and `y`, or `extent` `[xmin, ymin, xmax, ymax]`, and `crs`) when they are right. `None`
/// for anything else. What it leaves out of an object it takes (a field it doesn't know, a wrong
/// `extent` or `crs`) is reported by [`PanelState::value_report`].
pub fn pick_value(item: &PanelItemDecl, v: &Value) -> Option<Value> {
    if v.is_null() {
        return Some(Value::Null);
    }
    let o = v.as_object()?;
    let num = |v: Option<&Value>| v.and_then(Value::as_f64).filter(|x| x.is_finite());
    let four = |k: &str| -> Option<[f64; 4]> {
        let a = o.get(k)?.as_array().filter(|a| a.len() == 4)?;
        Some([num(a.first())?, num(a.get(1))?, num(a.get(2))?, num(a.get(3))?])
    };
    let lon = |x: f64| (-180.0..=180.0).contains(&x);
    let lat = |y: f64| (-90.0..=90.0).contains(&y);
    let mut out = Map::new();
    if item.picks_box() {
        let b = four("bbox")?;
        if !(lon(b[0]) && lon(b[2]) && lat(b[1]) && lat(b[3]) && b[1] <= b[3]) {
            return None;
        }
        out.insert("bbox".into(), json!(b));
        if let Some(e) = four("extent").filter(|e| e[0] <= e[2] && e[1] <= e[3]) {
            out.insert("extent".into(), json!(e));
        }
    } else {
        let (x, y) = (num(o.get("lon")).filter(|x| lon(*x))?, num(o.get("lat")).filter(|y| lat(*y))?);
        out.insert("lon".into(), json!(x));
        out.insert("lat".into(), json!(y));
        if let (Some(x), Some(y)) = (num(o.get("x")), num(o.get("y"))) {
            out.insert("x".into(), json!(x));
            out.insert("y".into(), json!(y));
        }
    }
    if let Some(crs) = o.get("crs").and_then(Value::as_str).filter(|c| !c.trim().is_empty() && c.chars().count() <= MAX_PICK_CRS) {
        out.insert("crs".into(), json!(crs));
    }
    Some(Value::Object(out))
}

/// A nonnegative whole count, retaining exact u64 integers and accepting integral floats.
fn node_count(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| {
        v.as_f64()
            .filter(|value| value.is_finite() && value.fract() == 0.0 && *value >= 0.0 && *value < u64::MAX as f64)
            .map(|value| value as u64)
    })
}

/// `[low, high]`, whole numbers.
fn pair(v: &Value) -> Option<(i64, i64)> {
    match v.as_array().map(Vec::as_slice) {
        Some([a, b]) => Some((whole(a)?, whole(b)?)),
        _ => None,
    }
}

/// A date written `YYYY-MM-DD` (years 1 to 9999), as (year, month, day); `None` when it isn't a
/// real date.
pub fn date(s: &str) -> Option<(i32, u32, u32)> {
    if s.len() != 10 || s.get(4..5) != Some("-") || s.get(7..8) != Some("-") {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r).filter(|t| t.bytes().all(|c| c.is_ascii_digit()))?.parse::<u32>().ok();
    let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let y = i32::try_from(y).ok().filter(|y| *y >= 1)?;
    (d >= 1 && d <= days_in_month(y, m)?).then_some((y, m, d))
}

/// The days in month `m` (1 to 12) of year `y`; `None` for another month.
pub fn days_in_month(y: i32, m: u32) -> Option<u32> {
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    Some(match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    })
}

/// A dropdown's options in `v` (`[{"id", "label"?}]`, at most [`MAX_CHOICES`]): an option without
/// an id, with one taken already or too long, or that isn't an object, is left out. A label
/// defaults to the id and is kept to one line. Only the first [`MAX_CHOICES`] entries are looked
/// at, those left out among them included.
fn choice_options(v: &[Value]) -> Vec<TreeNode> {
    choice_options_reported(v, &mut Dropped::default())
}

/// Dropdown options, with the fields changed while retaining an option reported too.
fn choice_options_reported(v: &[Value], dropped: &mut Dropped) -> Vec<TreeNode> {
    let (mut left, mut seen) = (MAX_CHOICES, HashSet::new());
    let mut nodes = Vec::new();
    for o in v.iter().take(MAX_CHOICES).filter(|o| o.is_object()) {
        if left == 0 {
            break;
        }
        if let Some(fields) = o.as_object() {
            let id = o.get("id").and_then(Value::as_str).unwrap_or("option");
            for key in fields.keys().filter(|key| !matches!(key.as_str(), "id" | "label")) {
                dropped.field(format!("{id}.{key}"));
            }
        }
        // One at a time, without its children: a dropdown is flat.
        let flat = [json!({"id": o.get("id"), "label": o.get("label")})];
        nodes.extend(tree_nodes(&flat, MAX_TREE_DEPTH, &mut left, &mut seen, dropped));
    }
    nodes
}

/// The most value ids a notice names.
const MAX_NAMED: usize = 10;

/// Counts and bounded name samples for what a panel update left out or changed. `values`,
/// `fields`, `clipped` and `label_fields` retain at most ten names, each at most 200 characters.
/// The unlisted counters retain the remaining totals; `labels` is the full clipped-label count.
/// Use `notice()` for the complete counts and sampled locations.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Dropped {
    pub values: Vec<String>,
    pub nodes: usize,
    pub adjusted: Vec<String>,
    pub fields: Vec<String>,
    pub clipped: Vec<String>,
    pub labels: usize,
    pub label_fields: Vec<String>,
    pub malformed: usize,
    pub unlisted_fields: usize,
    pub unlisted_clipped: usize,
    pub unlisted_values: usize,
}

impl Dropped {
    /// Count a clipped label and retain a bounded sample of its location.
    fn clip_label(&mut self, id: &str) {
        self.labels = self.labels.saturating_add(1);
        if self.label_fields.len() < MAX_NAMED {
            self.label_fields.push(diagnostic_name(&format!("{id}.label")));
        }
    }
    /// Count every field, retaining only a bounded sample for diagnostics.
    fn field(&mut self, name: String) {
        if self.fields.len() < MAX_NAMED {
            self.fields.push(diagnostic_name(&name));
        } else {
            self.unlisted_fields = self.unlisted_fields.saturating_add(1);
        }
    }

    /// Count refused values while retaining only a bounded diagnostic sample.
    fn rejected_value(&mut self, name: &str) {
        if self.values.len() < MAX_NAMED {
            self.values.push(diagnostic_name(name));
        } else {
            self.unlisted_values = self.unlisted_values.saturating_add(1);
        }
    }

    /// Count every clipped field without retaining all names from an untrusted reply.
    fn clip_field(&mut self, name: String) {
        if self.clipped.len() < MAX_NAMED {
            self.clipped.push(diagnostic_name(&name));
        } else {
            self.unlisted_clipped = self.unlisted_clipped.saturating_add(1);
        }
    }
    /// The notice for it; `None` when nothing was left out.
    pub fn notice(&self) -> Option<String> {
        let mut parts = Vec::new();
        if self.malformed > 0 {
            parts.push(format!("{} panel object(s) not taken: expected an object", self.malformed));
        }
        if self.labels > 0 {
            let names = self.label_fields.iter().map(|name| diagnostic_name(name)).collect::<Vec<_>>().join(", ");
            let more = if self.labels > self.label_fields.len() { ", …" } else { "" };
            let location = if names.is_empty() { String::new() } else { format!(": {names}{more}") };
            parts.push(format!("{} label(s) clipped to {MAX_STATE_LABEL} characters{location}", self.labels));
        }
        for (ids, unlisted, what) in [
            (&self.fields, self.unlisted_fields, "item field(s) not taken (unknown, inapplicable, wrong type or invalid bounds)"),
            (&self.clipped, self.unlisted_clipped, "item field(s) clipped or truncated"),
            (
                &self.adjusted,
                0,
                "value(s) adjusted (clamped or reordered, choice cleared, or a place's unknown or invalid fields left out)",
            ),
        ] {
            if !ids.is_empty() {
                let names = ids.iter().take(MAX_NAMED).map(|s| diagnostic_name(s)).collect::<Vec<_>>().join(", ");
                let more = if ids.len() > MAX_NAMED || unlisted > 0 { ", …" } else { "" };
                parts.push(format!("{} {what}: {names}{more}", ids.len().saturating_add(unlisted)));
            }
        }
        if self.nodes > 0 {
            parts.push(format!(
                "{} option(s) or tree node(s) left out (no id, one repeated or too long, too deep, or past the limit)",
                self.nodes
            ));
        }
        if !self.values.is_empty() || self.unlisted_values > 0 {
            let mut named = self.values.iter().take(MAX_NAMED).map(|v| diagnostic_name(v)).collect::<Vec<_>>().join(", ");
            if self.values.len() > MAX_NAMED || self.unlisted_values > 0 {
                named.push_str(", …");
            }
            parts.push(format!(
                "{} value(s) not taken (no such widget, or not a value it holds): {named}",
                self.values.len().saturating_add(self.unlisted_values)
            ));
        }
        (!parts.is_empty()).then(|| parts.join("; "))
    }
}

/// The ids in `values` (`{"id": value}`) that [`PanelState::set_values`] didn't take (`set`).
pub fn not_taken(values: &Value, set: &[String]) -> Vec<String> {
    values.as_object().map(|o| o.keys().filter(|k| !set.contains(k)).cloned().collect()).unwrap_or_default()
}

/// How many nodes `v` holds, at every depth (bounded).
fn count_given(v: &[Value], depth: usize) -> usize {
    v.iter()
        .map(|n| {
            let below = n.get("children").and_then(Value::as_array).filter(|_| depth < 64).map_or(0, |c| count_given(c, depth + 1));
            below.saturating_add(1)
        })
        .fold(0, usize::saturating_add)
}

/// How many nodes `nodes` hold, at every depth.
fn count_kept(nodes: &[TreeNode]) -> usize {
    nodes.iter().map(|n| 1 + count_kept(&n.children)).sum()
}

/// What the plugin last said about one widget.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemState {
    /// Replaces the declared label (the plugin words it in the request's `lang`).
    pub label: Option<String>,
    /// A text's contents.
    pub text: String,
    pub enabled: bool,
    pub visible: bool,
    /// A slider's or a range's ends.
    pub min: i64,
    pub max: i64,
    /// A slider's or a range's label for each position from `min` (empty: the numbers).
    pub labels: Vec<String>,
    /// A tree's nodes, or a dropdown's options (nodes without children) (shared: the panel's
    /// front end copies the state every frame it shows).
    #[serde(skip_serializing_if = "no_nodes", serialize_with = "shared_nodes")]
    pub nodes: Arc<Vec<TreeNode>>,
}

fn no_nodes(n: &Arc<Vec<TreeNode>>) -> bool {
    n.is_empty()
}

fn shared_nodes<S: serde::Serializer>(n: &Arc<Vec<TreeNode>>, s: S) -> Result<S::Ok, S::Error> {
    n.as_slice().serialize(s)
}

/// A node of a tree, as the plugin gave it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TreeNode {
    /// How the plugin names it: the tree's value when it is chosen. Unique in the tree.
    pub id: String,
    pub label: String,
    /// A number shown beside it (how many datasets a theme holds, say).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<u64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<TreeNode>,
}

/// The nodes in `v` (`[{"id", "label"?, "count"?, "children"?}]`), at most `left` of them
/// (counted down) and [`MAX_TREE_DEPTH`] deep below `depth`. A node without an id, or with one
/// taken already or too long, is left out with its children; a label defaults to the id (when
/// missing or blank), and is kept to one line (a row of the tree), control characters as spaces.
fn tree_nodes(v: &[Value], depth: usize, left: &mut usize, seen: &mut HashSet<String>, dropped: &mut Dropped) -> Vec<TreeNode> {
    let mut nodes = Vec::new();
    for n in v {
        if *left == 0 {
            break;
        }
        let Some(id) = n.get("id").and_then(Value::as_str).filter(|i| !i.is_empty() && i.chars().count() <= MAX_TREE_ID) else {
            continue;
        };
        if !seen.insert(id.to_string()) {
            continue;
        }
        *left -= 1;
        if let Some(fields) = n.as_object() {
            for key in fields.keys().filter(|key| !matches!(key.as_str(), "id" | "label" | "count" | "children")) {
                dropped.field(format!("{id}.{key}"));
            }
        }
        if n.get("label").and_then(Value::as_str).is_some_and(|label| label.chars().any(char::is_control)) {
            dropped.clip_field(format!("{id}.label (control characters replaced)"));
        }
        match n.get("label") {
            Some(Value::String(label)) if label.chars().count() > MAX_STATE_LABEL => dropped.clip_label(id),
            Some(label) if !label.is_null() && !label.is_string() => dropped.field(format!("{id}.label")),
            _ => {}
        }
        let label: String = n
            .get("label")
            .and_then(Value::as_str)
            .filter(|l| !l.trim().is_empty())
            .map_or_else(|| id.to_string(), |l| clip(l, MAX_STATE_LABEL))
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        for key in ["count", "children"] {
            if n.get(key)
                .is_some_and(|value| !value.is_null() && if key == "count" { node_count(value).is_none() } else { !value.is_array() })
            {
                dropped.field(format!("{id}.{key}"));
            }
        }
        let children = match n.get("children").and_then(Value::as_array) {
            Some(c) if depth + 1 < MAX_TREE_DEPTH => tree_nodes(c, depth + 1, left, seen, dropped),
            _ => Vec::new(),
        };
        nodes.push(TreeNode { id: id.to_string(), label, count: n.get("count").and_then(node_count), children });
    }
    nodes
}

/// Whether `nodes` hold one with id `id`, at any depth.
fn holds(nodes: &[TreeNode], id: &str) -> bool {
    nodes.iter().any(|n| n.id == id || holds(&n.children, id))
}

/// A panel's state, kept by the host between the plugin's calls.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PanelState {
    /// Checkbox and slider values, by widget id: `true`/`false`, or a whole number.
    pub values: BTreeMap<String, Value>,
    /// Every widget's state, by id.
    pub items: BTreeMap<String, ItemState>,
    /// The declaration this state was made from, to tell when the plugin was updated (shared:
    /// the panel's front end copies the state every frame it shows).
    #[serde(skip)]
    origin: Arc<PanelDecl>,
}

impl PanelState {
    /// Whether this state was made from `decl`. A plugin updated with another panel (other
    /// widgets, a slider's range or a default) doesn't match. A plain comparison: it runs on every
    /// command of a panel plugin and every frame the panel shows.
    pub fn matches(&self, decl: &PanelDecl) -> bool {
        *self.origin == *decl
    }

    /// The state a panel starts in: what `plugin.json` declares.
    pub fn new(decl: &PanelDecl) -> PanelState {
        let mut values = BTreeMap::new();
        let mut items = BTreeMap::new();
        for d in &decl.items {
            let id = d.key().to_string();
            match d.kind.as_str() {
                "checkbox" => {
                    values.insert(id.clone(), Value::Bool(d.default.as_ref().and_then(Value::as_bool).unwrap_or(false)));
                }
                "slider" => {
                    let v = d.default.as_ref().and_then(whole).unwrap_or(d.min).clamp(d.min, d.max.max(d.min));
                    values.insert(id.clone(), json!(v));
                }
                "range" => {
                    let (lo, hi) = d.default.as_ref().and_then(pair).unwrap_or((d.min, d.max));
                    let max = d.max.max(d.min);
                    let (lo, hi) = (lo.clamp(d.min, max), hi.clamp(d.min, max));
                    values.insert(id.clone(), json!([lo.min(hi), hi.max(lo)]));
                }
                "tree" | "choice" | "date" => {
                    values.insert(id.clone(), d.default.clone().filter(Value::is_string).unwrap_or(Value::Null));
                }
                "pick" => {
                    values.insert(id.clone(), Value::Null);
                }
                _ => {}
            }
            items.insert(
                id,
                ItemState {
                    label: None,
                    text: d.text.clone(),
                    enabled: true,
                    visible: true,
                    min: d.min,
                    max: d.max.max(d.min),
                    labels: Vec::new(),
                    nodes: Arc::new(if d.kind == "choice" { choice_options(&d.options) } else { Vec::new() }),
                },
            );
        }
        PanelState { values, items, origin: Arc::new(decl.clone()) }
    }

    /// A checkbox's value.
    pub fn checked(&self, id: &str) -> bool {
        self.values.get(id).and_then(Value::as_bool).unwrap_or(false)
    }

    /// A slider's value.
    pub fn slider(&self, id: &str) -> i64 {
        self.values.get(id).and_then(Value::as_i64).unwrap_or(0)
    }

    /// A tree's chosen node, a dropdown's chosen option or a date (`YYYY-MM-DD`), if any.
    pub fn chosen(&self, id: &str) -> Option<&str> {
        self.values.get(id).and_then(Value::as_str)
    }

    /// A range's low and high values.
    pub fn range(&self, id: &str) -> (i64, i64) {
        self.values.get(id).and_then(pair).unwrap_or((0, 0))
    }

    /// A pick's place, if one was picked ([`pick_value`]).
    pub fn picked(&self, id: &str) -> Option<&Map<String, Value>> {
        self.values.get(id).and_then(Value::as_object)
    }

    /// A slider's label for value `v`: its own label there, or the number.
    pub fn slider_label(&self, id: &str, v: i64) -> String {
        let Some(st) = self.items.get(id) else { return v.to_string() };
        usize::try_from(v.saturating_sub(st.min))
            .ok()
            .and_then(|i| st.labels.get(i))
            .filter(|l| !l.is_empty())
            .cloned()
            .unwrap_or_else(|| v.to_string())
    }

    /// Sets widget values from `values` (`{"id": value}`): a checkbox takes a bool, a slider a
    /// whole number (kept within its ends), a range `[low, high]` (kept within its ends, low
    /// first), a tree one of its nodes' ids and a dropdown one of its options' ids (`null` for
    /// none), a date `YYYY-MM-DD` (`null` for none), a pick a place ([`pick_value`]) or `null`.
    /// Anything else is ignored. Returns the ids set.
    pub fn set_values(&mut self, decl: &PanelDecl, values: &Value) -> Vec<String> {
        let Some(given) = values.as_object() else { return Vec::new() };
        let mut set = Vec::new();
        for d in decl.items.iter().filter(|d| d.has_value()) {
            let id = d.key();
            let Some(v) = given.get(id) else { continue };
            let new = match d.kind.as_str() {
                "checkbox" => v.as_bool().map(Value::Bool),
                "tree" | "choice" => match v {
                    Value::Null => Some(Value::Null),
                    Value::String(n) if self.items.get(id).is_some_and(|s| holds(&s.nodes, n)) => Some(v.clone()),
                    _ => None,
                },
                "date" => match v {
                    Value::Null => Some(Value::Null),
                    Value::String(d) if date(d).is_some() => Some(v.clone()),
                    _ => None,
                },
                "pick" => pick_value(d, v),
                "range" => pair(v).map(|(a, b)| {
                    let (min, max) = self.items.get(id).map_or((d.min, d.max), |s| (s.min, s.max));
                    let (a, b) = (a.clamp(min, max.max(min)), b.clamp(min, max.max(min)));
                    json!([a.min(b), a.max(b)])
                }),
                _ => whole(v).map(|n| {
                    let (min, max) = self.items.get(id).map_or((d.min, d.max), |s| (s.min, s.max));
                    json!(n.clamp(min, max.max(min)))
                }),
            };
            if let Some(new) = new {
                self.values.insert(id.to_string(), new);
                set.push(id.to_string());
            }
        }
        set
    }

    /// Reports refused and adjusted values after [`Self::set_values`], comparing whole numbers
    /// numerically so an integral JSON float is not a changed value.
    pub fn value_report(&self, decl: &PanelDecl, values: &Value, set: &[String]) -> Dropped {
        let mut dropped = Dropped::default();
        let Some(given) = values.as_object() else {
            dropped.malformed = usize::from(!values.is_null());
            return dropped;
        };
        let accepted: HashSet<&str> = set.iter().map(String::as_str).collect();
        for id in given.keys().filter(|id| !accepted.contains(id.as_str())) {
            dropped.rejected_value(id);
        }
        for id in set {
            let (Some(item), Some(input), Some(kept)) = (decl.item(id), given.get(id), self.values.get(id)) else { continue };
            let changed = match item.kind.as_str() {
                "slider" => whole(input) != whole(kept),
                "range" => pair(input) != pair(kept),
                // A place kept without some of what was given with it (a field it doesn't know,
                // a wrong extent or coordinate system).
                "pick" => match (input.as_object(), kept.as_object()) {
                    (Some(given), Some(kept)) => given.keys().any(|k| !kept.contains_key(k)),
                    _ => false,
                },
                _ => false,
            };
            if changed {
                dropped.adjusted.push(id.clone());
            }
        }
        dropped
    }

    /// Takes in a plugin's answer about its panel: `{"values": {...}, "items": {"id": {...}}}`.
    /// An item may set `label` (`null` for the declared one), `text`, `enabled`, `visible`, for a
    /// slider or a range `min`, `max` and `labels`, for a tree `nodes`, and for a dropdown
    /// `options`. Unknown ids and fields, and values
    /// of the wrong type, are ignored; texts are clipped, sliders' values kept within their ends
    /// and a tree's chosen node among its nodes. A pick's value can only be cleared (`null`): the
    /// place is the user's to pick. Returns the values, options and nodes left out.
    pub fn apply(&mut self, decl: &PanelDecl, update: &Value) -> Dropped {
        let mut dropped = Dropped::default();
        if !update.is_object() {
            dropped.malformed = usize::from(!update.is_null());
            return dropped;
        }
        if let Some(fields) = update.as_object() {
            for key in fields.keys().filter(|key| !matches!(key.as_str(), "values" | "items" | "changed")) {
                dropped.field(format!("panel.{key}"));
            }
        }
        if update.get("items").is_some_and(|items| !items.is_null() && !items.is_object()) {
            dropped.malformed += 1;
        }
        let empty = Map::new();
        let items = update.get("items").and_then(Value::as_object).unwrap_or(&empty);
        for (id, given) in items {
            if given.is_null() {
                continue;
            }
            let Some(item) = decl.items.iter().find(|item| item.key() == id) else {
                dropped.field(id.clone());
                continue;
            };
            if let Some(fields) = given.as_object() {
                for key in fields.keys() {
                    let applies = match key.as_str() {
                        "label" | "enabled" | "visible" => true,
                        "text" => item.kind == "text",
                        "min" | "max" | "labels" => item.ranged(),
                        "nodes" => item.kind == "tree",
                        "options" => item.kind == "choice",
                        _ => false,
                    };
                    if !applies {
                        dropped.field(format!("{id}.{key}"));
                    }
                }
            }
        }
        for d in &decl.items {
            let id = d.key();
            let (Some(u), Some(st)) = (items.get(id), self.items.get_mut(id)) else { continue };
            if u.is_null() {
                continue;
            }
            if !u.is_object() {
                dropped.field(id.to_string());
                continue;
            }
            for (key, valid) in [
                ("label", u.get("label").is_none_or(|v| v.is_null() || v.is_string())),
                ("enabled", u.get("enabled").is_none_or(Value::is_boolean)),
                ("visible", u.get("visible").is_none_or(Value::is_boolean)),
                ("text", d.kind != "text" || u.get("text").is_none_or(Value::is_string)),
                ("labels", !d.ranged() || u.get("labels").is_none_or(Value::is_array)),
                ("nodes", d.kind != "tree" || u.get("nodes").is_none_or(Value::is_array)),
                ("options", d.kind != "choice" || u.get("options").is_none_or(Value::is_array)),
            ] {
                if !valid {
                    dropped.field(format!("{id}.{key}"));
                }
            }
            match u.get("label") {
                Some(Value::String(l)) => {
                    if l.chars().count() > MAX_STATE_LABEL {
                        dropped.clip_label(id);
                    }
                    st.label = Some(clip(l, MAX_STATE_LABEL));
                }
                Some(Value::Null) => st.label = None,
                _ => {}
            }
            if d.kind == "text"
                && let Some(t) = u.get("text").and_then(Value::as_str)
            {
                let count = t.chars().count();
                if count > MAX_STATE_TEXT {
                    dropped.clip_field(format!("{id}.text ({count} to {MAX_STATE_TEXT} characters)"));
                }
                st.text = clip(t, MAX_STATE_TEXT);
            }
            if let Some(b) = u.get("enabled").and_then(Value::as_bool) {
                st.enabled = b;
            }
            if let Some(b) = u.get("visible").and_then(Value::as_bool) {
                st.visible = b;
            }
            if d.ranged() {
                let bound = |value: &Value| whole(value).filter(|value| (-SLIDER_LIMIT..=SLIDER_LIMIT).contains(value));
                let min = u.get("min").and_then(bound).unwrap_or(st.min);
                let max = u.get("max").and_then(bound).unwrap_or(st.max);
                for key in ["min", "max"] {
                    if u.get(key).is_some_and(|v| bound(v).is_none()) {
                        dropped.field(format!("{id}.{key}"));
                    }
                }
                if min <= max && min >= -SLIDER_LIMIT && max <= SLIDER_LIMIT {
                    (st.min, st.max) = (min, max);
                } else {
                    for key in ["min", "max"] {
                        if u.get(key).is_some_and(|value| bound(value).is_some()) {
                            dropped.field(format!("{id}.{key}"));
                        }
                    }
                }
                if let Some(ls) = u.get("labels").and_then(Value::as_array) {
                    if ls.len() > MAX_SLIDER_LABELS {
                        dropped.clip_field(format!("{id}.labels ({} to {MAX_SLIDER_LABELS} labels)", ls.len()));
                    }
                    for (index, label) in ls.iter().take(MAX_SLIDER_LABELS).enumerate() {
                        match label.as_str() {
                            Some(label) if label.chars().count() > MAX_SLIDER_LABEL => {
                                dropped.clip_field(format!(
                                    "{id}.labels[{index}] ({} to {MAX_SLIDER_LABEL} characters)",
                                    label.chars().count()
                                ));
                            }
                            None => dropped.field(format!("{id}.labels[{index}]")),
                            _ => {}
                        }
                    }
                    st.labels = ls
                        .iter()
                        .take(MAX_SLIDER_LABELS)
                        .map(|l| l.as_str().map(|s| clip(s, MAX_SLIDER_LABEL)).unwrap_or_default())
                        .collect();
                }
            }
            if d.kind == "tree"
                && let Some(nodes) = u.get("nodes").and_then(Value::as_array)
            {
                let mut left = MAX_TREE_NODES;
                st.nodes = Arc::new(tree_nodes(nodes, 0, &mut left, &mut Default::default(), &mut dropped));
                dropped.nodes = dropped.nodes.saturating_add(count_given(nodes, 0).saturating_sub(count_kept(&st.nodes)));
            }
            if d.kind == "choice"
                && let Some(options) = u.get("options").and_then(Value::as_array)
            {
                st.nodes = Arc::new(choice_options_reported(options, &mut dropped));
                dropped.nodes = dropped.nodes.saturating_add(options.len().saturating_sub(st.nodes.len()));
            }
        }
        if let Some(values) = update.get("values") {
            // A plugin can clear a pick, not pick a place itself: the others are refused, but for
            // the place held given back (a plugin answering its whole panel), compared as it would
            // be kept, so its numbers written otherwise (`10` for `10.0`) are the same place.
            let mut values = values.clone();
            let mut placed = Vec::new();
            if let Some(o) = values.as_object_mut() {
                o.retain(|id, v| {
                    let refused = !v.is_null()
                        && decl.item(id).filter(|d| d.kind == "pick").is_some_and(|d| pick_value(d, v).as_ref() != self.values.get(id));
                    if refused {
                        placed.push(id.clone());
                    }
                    !refused
                });
            }
            let set = self.set_values(decl, &values);
            let report = self.value_report(decl, &values, &set);
            dropped.values = report.values;
            dropped.unlisted_values = report.unlisted_values;
            dropped.adjusted = report.adjusted;
            dropped.malformed += report.malformed;
            for id in &placed {
                dropped.rejected_value(id);
            }
        }
        // New ends keep the values within them, and new nodes or options the chosen one among them.
        for d in &decl.items {
            let id = d.key();
            let (Some(st), Some(v)) = (self.items.get(id), self.values.get_mut(id)) else { continue };
            let before = v.clone();
            match d.kind.as_str() {
                "slider" => *v = json!(v.as_i64().unwrap_or(st.min).clamp(st.min, st.max)),
                "range" => {
                    let (a, b) = pair(v).unwrap_or((st.min, st.max));
                    let (a, b) = (a.clamp(st.min, st.max), b.clamp(st.min, st.max));
                    *v = json!([a.min(b), a.max(b)]);
                }
                "tree" | "choice" if v.as_str().is_some_and(|n| !holds(&st.nodes, n)) => *v = Value::Null,
                _ => {}
            }
            if *v != before && !dropped.adjusted.iter().any(|given| given == id) {
                dropped.adjusted.push(id.to_string());
            }
        }
        dropped
    }

    /// What goes to the plugin in each request: `{"values": {...}, "changed": [ids]}`, `changed`
    /// naming the widgets this call set (the one the user just moved, say).
    pub fn request(&self, changed: &[String]) -> Value {
        json!({"values": self.values, "changed": changed})
    }
}
