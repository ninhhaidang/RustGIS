//! QGIS renderers (`<renderer-v2>`) as RustGIS ones: single symbol, categorized (Unique Values),
//! graduated (Graduated Colors), rule-based (Unique Values where every rule picks values of one
//! field), the null renderer, and the renderers that wrap another (point cluster and
//! displacement, inverted polygons, merged features) by the one inside.

use std::collections::BTreeMap;

use roxmltree::Node;

use super::symbol::{self, Converted};
use super::{QGIS_SCALE, Report, Style, child, expr, has_effect};
use crate::color::Ramp;
use crate::symbol_by::SymbolBy;
use crate::symbology::{ClassMethod, LineStroke, Renderer, Symbol};

/// The most classes a renderer is imported with: every feature is matched against them.
const MAX_CLASSES: usize = 4096;

/// Reads the renderer `r` into `style`: its renderer, a line layer's strokes, and its reference
/// scale and symbol levels.
pub(super) fn read(r: Node, layer: &str, style: &mut Style, report: &mut Report) {
    read_depth(r, layer, style, report, 0);
}

fn read_depth(r: Node, layer: &str, style: &mut Style, report: &mut Report, depth: usize) {
    // QGIS's reference scale (its own scale, at 96 dpi): kept for sizes in map units.
    let reference = r.attribute("referencescale").and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite() && *v > 0.0);
    if let Some(s) = reference {
        style.reference_scale = Some((s * QGIS_SCALE).clamp(1.0, 1e9));
    }
    if r.attribute("symbollevels") == Some("1") {
        style.stroke_levels = Some(true);
    }
    if r.attribute("enableorderby") == Some("1") {
        report.approximated(layer, "features drawn in the order QGIS's order-by sets, drawn in the data's order");
    }
    if has_effect(r) {
        report.skipped(layer, "draw effects (shadows, glows, blurs and the like)");
    }
    // The symbols the renderer names, by name.
    let symbols: BTreeMap<String, Node> = child(r, "symbols")
        .map(|s| {
            s.children()
                .filter(|c| c.has_tag_name("symbol"))
                .filter_map(|c| Some((c.attribute("name")?.to_string(), c)))
                .take(MAX_CLASSES * 2)
                .collect()
        })
        .unwrap_or_default();
    // The first symbol turned by a field turns the layer's markers.
    let rotation = std::cell::RefCell::new(None);
    let turned = symbols.values().any(|n| symbol::turns_by_field(*n));
    // Every symbol's data-defined expressions: the first that has any are the layer's.
    let by: std::cell::RefCell<Vec<SymbolBy>> = std::cell::RefCell::new(Vec::new());
    let read = |n: Node, report: &mut Report| {
        let c = symbol::read(n, reference, turned, layer, report);
        if let (Some(r), Ok(mut had)) = (&c.rotation, rotation.try_borrow_mut())
            && *had.get_or_insert_with(|| r.clone()) != *r
        {
            report.approximated(layer, "marker layers turned by different fields, all turned by the first");
        }
        if let Ok(mut all) = by.try_borrow_mut()
            && !all.contains(&c.by)
        {
            all.push(c.by.clone());
        }
        c
    };
    let sym = |name: Option<&str>, report: &mut Report| -> Option<Converted> { symbols.get(name?).map(|n| read(*n, report)) };
    let kind = r.attribute("type").unwrap_or_default();
    match kind {
        "singleSymbol" => {
            let Some(c) = sym(Some("0"), report).or_else(|| symbols.values().next().map(|n| read(*n, report))) else {
                return;
            };
            style.line_strokes = Some(if c.stacked() { c.strokes.clone() } else { Vec::new() });
            style.fill_layers = Some(c.fills.clone());
            style.renderer = Some(Renderer::Single(c.symbol));
        }
        "nullSymbol" => {
            style.renderer = Some(Renderer::Single(symbol::hidden()));
            style.line_strokes = Some(Vec::new());
            style.fill_layers = Some(Vec::new());
        }
        "categorizedSymbol" => categorized(r, layer, style, report, &sym),
        "graduatedSymbol" => graduated(r, layer, style, report, &sym),
        "RuleRenderer" => rules(r, layer, style, report, &sym),
        "pointCluster" | "pointDisplacement" | "invertedPolygonRenderer" | "mergedFeatureRenderer" | "25dRenderer" => {
            report.approximated(layer, format!("its {} renderer, drawn by the renderer inside it", wrapper_name(kind)));
            if let Some(inner) = child(r, "renderer-v2").filter(|_| depth < 8) {
                read_depth(inner, layer, style, report, depth + 1);
            }
        }
        "heatmapRenderer" => report.skipped(layer, "its heatmap renderer"),
        other => report.skipped(layer, format!("its {other} renderer")),
    }
    // A renderer read here (not by the one inside a wrapper) says whether its markers turn.
    let wrapper =
        matches!(kind, "pointCluster" | "pointDisplacement" | "invertedPolygonRenderer" | "mergedFeatureRenderer" | "25dRenderer");
    if style.renderer.is_some() && !wrapper {
        style.marker_rotation = Some(rotation.try_borrow().ok().and_then(|r| r.clone()));
        let mut all = by.try_borrow().map(|b| b.clone()).unwrap_or_default();
        // A switched-off class draws nothing: expressions applied to every feature would paint it.
        let off =
            r.descendants().any(|n| (n.has_tag_name("category") || n.has_tag_name("range")) && n.attribute("render") == Some("false"));
        if off && !all.iter().any(SymbolBy::is_empty) {
            all.push(SymbolBy::default());
        }
        // The expressions apply to every feature: kept only when every symbol has the same.
        let one = match all.as_slice() {
            [one] => one.clone(),
            _ => {
                if all.iter().any(|b| !b.is_empty()) {
                    report.skipped(layer, "data-defined settings that differ between its classes (or a class switched off)");
                }
                SymbolBy::default()
            }
        };
        style.symbol_by = Some(one);
    }
}

/// A renderer's classifying attribute as a field: `name` or `"name"`; `None` for an expression.
fn field_of(attr: &str, layer: &str, report: &mut Report) -> Option<String> {
    let a = attr.trim();
    let unquoted = a.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(a);
    if !unquoted.is_empty() && !unquoted.contains('"') && (a.starts_with('"') || unquoted.chars().all(|c| c.is_alphanumeric() || c == '_'))
    {
        return Some(unquoted.to_string());
    }
    report.skipped(layer, format!("classes by the expression `{a}` (only by a field)"));
    None
}

/// A switched-off class (`render="false"`): it draws nothing.
fn switched_off() -> Converted {
    Converted { symbol: symbol::hidden(), strokes: Vec::new(), rotation: None, by: Default::default(), fills: Vec::new() }
}

type SymbolOf<'a> = dyn Fn(Option<&str>, &mut Report) -> Option<Converted> + 'a;

fn categorized(r: Node, layer: &str, style: &mut Style, report: &mut Report, sym: &SymbolOf) {
    let cats: Vec<Node> = child(r, "categories").map(|c| c.children().filter(|n| n.has_tag_name("category")).collect()).unwrap_or_default();
    let Some(field) = field_of(r.attribute("attr").unwrap_or_default(), layer, report) else {
        // Drawn as one: the first category's symbol.
        if let Some(c) = cats.first().and_then(|c| sym(c.attribute("symbol"), report)) {
            style.fill_layers = Some(c.fills.clone());
            style.renderer = Some(Renderer::Single(c.symbol));
        }
        return;
    };
    let mut classes: Vec<(String, Symbol)> = Vec::new();
    let (mut class_labels, mut class_strokes) = (BTreeMap::new(), BTreeMap::new());
    let mut other = symbol::hidden();
    let mut other_strokes = Vec::new();
    let (mut class_fills, mut other_fills) = (BTreeMap::new(), Vec::new());
    for cat in &cats {
        // A switched-off class draws nothing; its symbol isn't read (its field would turn the rest).
        let c = if cat.attribute("render") == Some("false") {
            switched_off()
        } else {
            let Some(c) = sym(cat.attribute("symbol"), report) else { continue };
            c
        };
        // A category of several values (QGIS's merged categories) lists them as <val>s, or as a
        // list <Option>.
        let mut values: Vec<String> = cat
            .descendants()
            .filter(|v| v.has_tag_name("val") || (v.has_tag_name("Option") && v.attribute("type") == Some("QString")))
            .filter_map(|v| v.attribute("value").map(str::to_string))
            .collect();
        if values.is_empty() {
            values.push(cat.attribute("value").unwrap_or_default().to_string());
        }
        for v in values {
            // An empty value is QGIS's "all other values".
            if v.is_empty() || v == "NULL" {
                other = c.symbol;
                other_strokes = if c.stacked() { c.strokes.clone() } else { Vec::new() };
                other_fills.clone_from(&c.fills);
                continue;
            }
            if classes.len() >= MAX_CLASSES {
                report.skipped(layer, format!("categories past the first {MAX_CLASSES}"));
                break;
            }
            if let Some(label) = cat.attribute("label").filter(|l| *l != v && !l.is_empty()) {
                class_labels.insert(v.clone(), label.to_string());
            }
            if c.stacked() {
                class_strokes.insert(v.clone(), c.strokes.clone());
            }
            if !c.fills.is_empty() {
                class_fills.insert(v.clone(), c.fills.clone());
            }
            classes.push((v, c.symbol));
        }
    }
    style.line_strokes = Some(Vec::new());
    style.fill_layers = Some(Vec::new());
    style.renderer = Some(Renderer::Unique {
        field,
        classes,
        other,
        ramp: Ramp::default(),
        class_labels,
        other_label: None,
        class_strokes,
        other_strokes,
        class_fills,
        other_fills,
        expression: None,
    });
}

fn graduated(r: Node, layer: &str, style: &mut Style, report: &mut Report, sym: &SymbolOf) {
    let mut ranges: Vec<(f64, f64, Converted, Option<String>)> = Vec::new();
    for rg in child(r, "ranges").map(|c| c.children().filter(|n| n.has_tag_name("range")).collect::<Vec<_>>()).unwrap_or_default() {
        let num = |k: &str| rg.attribute(k).and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite());
        let (Some(lower), Some(upper)) = (num("lower"), num("upper")) else { continue };
        let c = if rg.attribute("render") == Some("false") {
            switched_off()
        } else {
            let Some(c) = sym(rg.attribute("symbol"), report) else { continue };
            c
        };
        ranges.push((lower, upper, c, rg.attribute("label").map(str::to_string)));
        if ranges.len() >= MAX_CLASSES {
            report.skipped(layer, format!("ranges past the first {MAX_CLASSES}"));
            break;
        }
    }
    let Some(field) = field_of(r.attribute("attr").unwrap_or_default(), layer, report) else {
        if let Some((.., c, _)) = ranges.first() {
            style.fill_layers = Some(c.fills.clone());
            style.renderer = Some(Renderer::Single(c.symbol));
        }
        return;
    };
    if ranges.is_empty() {
        return;
    }
    ranges.sort_by(|a, b| a.0.total_cmp(&b.0));
    if ranges.windows(2).any(|w| w.first().map(|a| a.1) != w.get(1).map(|b| b.0)) {
        report.approximated(layer, "ranges with gaps or overlaps between them, drawn as if each began where the one before ends");
    }
    let min = ranges.first().map_or(0.0, |r| r.0);
    // QGIS draws nothing for a value outside every range; a RustGIS graduated renderer gives it the
    // first or last class. The import doesn't read the data, so it can't tell whether any value
    // falls outside: the note is always given, on purpose.
    report.approximated(layer, "values below the first range or above the last, drawn in the first or last class (QGIS leaves them out)");
    let mut class_labels = BTreeMap::new();
    let mut class_strokes: BTreeMap<usize, Vec<LineStroke>> = BTreeMap::new();
    let mut class_fills = BTreeMap::new();
    for (i, (.., c, label)) in ranges.iter().enumerate() {
        if let Some(l) = label.as_ref().filter(|l| !l.is_empty()) {
            class_labels.insert(i, l.clone());
        }
        if c.stacked() {
            class_strokes.insert(i, c.strokes.clone());
        }
        if !c.fills.is_empty() {
            class_fills.insert(i, c.fills.clone());
        }
    }
    style.line_strokes = Some(Vec::new());
    style.fill_layers = Some(Vec::new());
    style.renderer = Some(Renderer::Graduated {
        field,
        breaks: ranges.iter().map(|r| r.1).collect(),
        symbols: ranges.iter().map(|r| r.2.symbol).collect(),
        ramp: Ramp::default(),
        method: ClassMethod::default(),
        min,
        classes: ranges.len(),
        class_labels,
        class_strokes,
        class_fills,
    });
}

/// A rule of a rule-based renderer that draws: its filter and its parents' (an else rule's own
/// left out), for an else rule and each else rule above it the siblings it takes the rest of (what
/// none of them draws), and the scale range along the way (RustGIS's scales: drawn out to 1:`.0`,
/// in to 1:`.1`).
struct Rule<'a, 'i> {
    filters: Vec<String>,
    else_of: Vec<std::rc::Rc<Option<Vec<super::Sibling>>>>,
    node: Node<'a, 'i>,
    scales: (Option<f64>, Option<f64>),
}

impl Rule<'_, '_> {
    fn scaled(&self) -> bool {
        self.scales.0.is_some() || self.scales.1.is_some()
    }
}

fn rules(r: Node, layer: &str, style: &mut Style, report: &mut Report, sym: &SymbolOf) {
    let mut leaves = Vec::new();
    let mut left = 0;
    if let Some(root) = child(r, "rules") {
        flatten(root, Vec::new(), (None, None), Vec::new(), &mut leaves, 0, &mut left);
    }
    if left > 0 {
        report.skipped(layer, format!("{left} of its rules, past the {MAX_CLASSES} RustGIS reads or nested more than 32 deep"));
    }
    // Each symbol read once, however many ways the rules are tried (its notes said once).
    let seen: std::cell::RefCell<BTreeMap<Option<String>, Option<Converted>>> = Default::default();
    let sym = |name: Option<&str>, report: &mut Report| -> Option<Converted> {
        let key = name.map(str::to_string);
        if let Some(c) = seen.borrow().get(&key) {
            return c.clone();
        }
        let c = sym(name, report);
        seen.borrow_mut().insert(key, c.clone());
        c
    };
    // Rules that draw: those with a symbol and not switched off.
    let drawn: Vec<&Rule> =
        leaves.iter().filter(|l| l.node.attribute("symbol").is_some() && l.node.attribute("active") != Some("0")).collect();
    // Every rule picks values of one field (an else rule for the rest), at every scale: Unique
    // Values.
    let mut field: Option<String> = None;
    let mut classes: Vec<(String, Converted, Option<String>)> = Vec::new();
    let mut other: Option<Converted> = None;
    let mut other_label: Option<String> = None;
    let mut simple = !drawn.is_empty() && !drawn.iter().any(|l| l.scaled());
    for rule in drawn.iter().filter(|_| simple) {
        let label = rule.node.attribute("label").map(str::to_string);
        if !rule.else_of.is_empty() {
            // (Only the first draws the rest; a later one's symbol isn't read.)
            if rule.filters.is_empty() && rule.else_of.len() == 1 {
                if other.is_none()
                    && let Some(c) = sym(rule.node.attribute("symbol"), report)
                {
                    (other, other_label) = (Some(c), else_label(rule));
                }
                continue;
            }
            simple = false;
            break;
        }
        let parts: Option<Vec<(String, Vec<String>)>> = rule.filters.iter().map(|f| expr::equality(f)).collect();
        match parts.as_deref() {
            Some([(f, values)]) if field.as_ref().is_none_or(|g| g.eq_ignore_ascii_case(f)) => {
                field = Some(f.clone());
                let Some(c) = sym(rule.node.attribute("symbol"), report) else { continue };
                for v in values {
                    if classes.len() < MAX_CLASSES {
                        classes.push((v.clone(), c.clone(), label.clone()));
                    }
                }
            }
            _ => {
                simple = false;
                break;
            }
        }
    }
    if simple && let Some(field) = field {
        unique(style, field, None, classes, other.as_ref(), other_label);
        return;
    }
    // One rule (or none but an else, beside nothing that draws): its symbol for every feature.
    let plain = |r: &&Rule| r.filters.is_empty() && r.else_of.iter().all(|s| s.as_ref().as_ref().is_some_and(Vec::is_empty)) && !r.scaled();
    if drawn.len() == 1 && drawn.first().is_some_and(plain) || drawn.is_empty() {
        if let Some(c) = drawn.first().and_then(|r| sym(r.node.attribute("symbol"), report)) {
            single(style, &c);
        }
        return;
    }
    // Any other rules: Unique Values classed by the first rule a feature meets, each a class.
    if by_expression(&drawn, style, report, &sym) {
        return;
    }
    report.approximated(
        layer,
        format!("its {} rules (filters too long or too many to class by), drawn with the first rule's symbol", drawn.len()),
    );
    if let Some(c) = drawn.first().and_then(|r| sym(r.node.attribute("symbol"), report)) {
        single(style, &c);
    }
}

/// One symbol for every feature, with its stacked strokes and fills.
fn single(style: &mut Style, c: &Converted) {
    style.line_strokes = Some(if c.stacked() { c.strokes.clone() } else { Vec::new() });
    style.fill_layers = Some(c.fills.clone());
    style.renderer = Some(Renderer::Single(c.symbol));
}

/// A Unique Values renderer on `field`, or by `expression`, with these classes (value, symbol,
/// legend label) and the rest drawn with `other` (none: not drawn).
fn unique(
    style: &mut Style,
    field: String,
    expression: Option<String>,
    classes: Vec<(String, Converted, Option<String>)>,
    other: Option<&Converted>,
    other_label: Option<String>,
) {
    let mut class_strokes = BTreeMap::new();
    let mut class_fills = BTreeMap::new();
    let mut class_labels = BTreeMap::new();
    for (v, c, label) in &classes {
        if c.stacked() {
            class_strokes.insert(v.clone(), c.strokes.clone());
        }
        if !c.fills.is_empty() {
            class_fills.insert(v.clone(), c.fills.clone());
        }
        if let Some(l) = label.as_ref().filter(|l| !l.is_empty() && *l != v) {
            class_labels.insert(v.clone(), l.clone());
        }
    }
    let (other_symbol, other_strokes, other_fills) = match other {
        Some(c) => (c.symbol, if c.stacked() { c.strokes.clone() } else { Vec::new() }, c.fills.clone()),
        None => (symbol::hidden(), Vec::new(), Vec::new()),
    };
    style.line_strokes = Some(Vec::new());
    style.fill_layers = Some(Vec::new());
    style.renderer = Some(Renderer::Unique {
        field,
        classes: classes.into_iter().map(|(v, c, _)| (v, c.symbol)).collect(),
        other: other_symbol,
        ramp: Ramp::default(),
        class_labels,
        other_label,
        class_strokes,
        other_strokes,
        class_fills,
        other_fills,
        expression,
    });
}

/// The most rules classed by expression: one `WHEN` each.
const MAX_RULE_CLASSES: usize = crate::symbology::MAX_EXPRESSION_CLASSES;

/// The rules as Unique Values by an expression ([`super::rules_expression`]): `CASE WHEN <rule
/// 1> THEN 'Rule 1' … END`, each rule's condition its filters and its parents', an else rule's
/// that none of its siblings draws, and its scale range. A feature meets the first rule it can
/// (QGIS draws it with every rule it meets: the engine counts such features, from
/// `style.rules`). A top-level else rule draws the rest. False when the rules don't fit in one
/// expression.
fn by_expression(drawn: &[&Rule], style: &mut Style, report: &mut Report, sym: &SymbolOf) -> bool {
    let mut rules: Vec<super::RuleFilter> = Vec::new();
    let mut classes: Vec<(String, Converted, Option<String>)> = Vec::new();
    let mut other: Option<Converted> = None;
    let mut other_label: Option<String> = None;
    let mut keys = std::collections::HashSet::new();
    let quoted = |f: &String| expr::to_sql(f).map(|e| format!("({e})"));
    for (i, rule) in drawn.iter().enumerate() {
        // A top-level else rule at every scale: the rest. (A later one draws nothing more, a
        // feature being drawn with the first rule it meets: its symbol isn't read.)
        let rest = rule.else_of.len() == 1 && rule.filters.is_empty() && !rule.scaled();
        if rest && other.is_some() {
            continue;
        }
        let Some(c) = sym(rule.node.attribute("symbol"), report) else { continue };
        if rest {
            (other, other_label) = (Some(c), else_label(rule));
            continue;
        }
        let Some(mut parts) = rule.filters.iter().map(quoted).collect::<Option<Vec<String>>>() else { return false };
        for siblings in rule.else_of.iter().filter(|s| s.as_ref().as_ref().is_none_or(|s| !s.is_empty())) {
            let Some(rest) = super::none_of(siblings) else { return false };
            parts.push(rest);
        }
        // What it picks (an else rule: what none of its siblings draws), then at its scales.
        let filter = if parts.is_empty() { "TRUE".to_string() } else { parts.join(" AND ") };
        let when = super::within(filter.clone(), rule.scales);
        // Its legend label, or its number, once.
        let label = rule.node.attribute("label").map(str::trim).filter(|l| !l.is_empty()).map(str::to_string);
        let mut key = label.clone().unwrap_or_else(|| format!("Rule {}", i + 1));
        let mut n = 2;
        while !keys.insert(key.to_lowercase()) {
            key = format!("{} ({n})", label.as_deref().unwrap_or("Rule"));
            n += 1;
        }
        rules.push(super::RuleFilter { key: key.clone(), when, condition: filter, out: rule.scales.0, inside: rule.scales.1 });
        classes.push((key, c, None));
        if rules.len() > MAX_RULE_CLASSES || super::rules_expression(&rules).chars().count() > rustgis_data::query::MAX_LEN {
            return false;
        }
    }
    if rules.is_empty() {
        return false;
    }
    unique(style, String::new(), Some(super::rules_expression(&rules)), classes, other.as_ref(), other_label);
    style.rules = rules;
    true
}

/// The most rules read from one renderer's rules, and the most an else rule is the rest of.
const MAX_RULES_READ: usize = MAX_CLASSES * 2;

/// The rules under `node` that draw, each with its own filter and its parents', for an else rule
/// its siblings, and the scale range along the way. A rule with a symbol and rules under it comes
/// after them: a feature meets the first rule it can, and QGIS draws the inner rule's symbol on
/// top. A rule under an else rule takes only what the else rule does (`else_of`, the sibling
/// sets of the else rules above). Bounded in depth and number: the rules left out are counted in
/// `left`.
#[allow(clippy::too_many_arguments)]
fn flatten<'a, 'i>(
    node: Node<'a, 'i>,
    filters: Vec<String>,
    scales: (Option<f64>, Option<f64>),
    else_of: Vec<std::rc::Rc<Option<Vec<super::Sibling>>>>,
    out: &mut Vec<Rule<'a, 'i>>,
    depth: usize,
    left: &mut usize,
) {
    // A rule draws with a symbol, or with none and no rules under it (as `rules` reads them).
    let draws = |r: Node| r.attribute("symbol").is_some() || !r.children().any(|c| c.has_tag_name("rule"));
    if depth > 32 || out.len() >= MAX_CLASSES {
        *left += super::drawing_rules(node.children(), draws);
        return;
    }
    let rules: Vec<Node> = node.children().filter(|c| c.has_tag_name("rule")).take(MAX_RULES_READ).collect();
    *left += super::drawing_rules(node.children().filter(|c| c.has_tag_name("rule")).skip(rules.len()), draws);
    let range = |rule: Node| super::rule_scales(rule, scales);
    // What an else rule here is the rest of: its siblings where they draw, shared by every else
    // rule (read only when there is one).
    let siblings: std::rc::Rc<Option<Vec<super::Sibling>>> =
        std::rc::Rc::new(if rules.iter().any(|r| r.attribute("filter").is_some_and(is_else)) {
            let itself = |r: Node| r.attribute("symbol").is_some();
            let mut budget = MAX_RULES_READ;
            super::siblings(rules.iter().copied(), scales, &itself, false, depth, &mut budget)
        } else {
            Some(Vec::new())
        });
    for (i, rule) in rules.iter().copied().enumerate() {
        if out.len() >= MAX_CLASSES {
            *left += super::drawing_rules(rules.get(i..).unwrap_or_default().iter().copied(), draws);
            break;
        }
        // Switched off: neither it nor the rules under it draw.
        if rule.attribute("active") == Some("0") {
            continue;
        }
        let mut f = filters.clone();
        let own = rule.attribute("filter").map(str::trim).filter(|f| !f.is_empty());
        let mut else_of = else_of.clone();
        if own.is_some_and(is_else) {
            else_of.push(siblings.clone());
        }
        if let Some(filter) = own.filter(|o| !is_else(o)) {
            f.push(filter.to_string());
        }
        let s = range(rule);
        let children = rule.children().any(|c| c.has_tag_name("rule"));
        if children {
            flatten(rule, f.clone(), s, else_of.clone(), out, depth + 1, left);
        }
        if rule.attribute("symbol").is_some() || !children {
            if out.len() < MAX_CLASSES {
                out.push(Rule { filters: f, else_of, node: rule, scales: s });
            } else {
                // Its rules under it took the last room.
                *left += 1;
            }
        }
    }
}

/// An else rule's legend label for `<all other values>`, if it has one.
fn else_label(rule: &Rule) -> Option<String> {
    rule.node.attribute("label").map(str::trim).filter(|l| !l.is_empty()).map(str::to_string)
}

fn is_else(f: &str) -> bool {
    f.trim().eq_ignore_ascii_case("ELSE")
}

fn wrapper_name(kind: &str) -> &'static str {
    match kind {
        "pointCluster" => "point cluster",
        "pointDisplacement" => "point displacement",
        "invertedPolygonRenderer" => "inverted polygon",
        "mergedFeatureRenderer" => "merged feature",
        _ => "2.5D",
    }
}
