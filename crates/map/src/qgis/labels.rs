//! QGIS labels (`<labeling type="simple|rule-based">`) as RustGIS label classes: each rule a
//! class whose SQL clause is its filter (and its parents'), an else rule the features no other
//! rule takes; the text's field, size, colour, weight, slant and letter spacing, its buffer as a
//! halo of its colour and size, its background as a rectangle behind it, its wrapping and how its
//! lines line up, its placement, offset and repeat along lines, and its scale range.

use roxmltree::Node;

use super::{QGIS_SCALE, Report, child, color, data_defined_on, expr, prop, props};
use crate::Rgba;
use crate::label_by::LabelBy;
use crate::layout::TextAlign;
use crate::symbology::{
    DEFAULT_LABEL_CLASS, HALO_WIDTH, LABEL_OFFSET, LABEL_SIZE, LabelBackground, LabelClass, LabelPlacement, Labeling, MAX_LABEL_CLASSES,
    MAX_WRAP_LENGTH,
};

/// A layer's labels from its `<labeling>`, or `None` when none can be carried over. `geometry` is
/// the layer's (`Point`, `Line`, `Polygon`), when its file says.
pub(super) fn read(l: Node, geometry: Option<&str>, layer: &str, report: &mut Report) -> Option<Labeling> {
    let mut classes: Vec<LabelClass> = Vec::new();
    match l.attribute("type") {
        Some("simple") => {
            let settings = child(l, "settings")?;
            classes.extend(class(settings, DEFAULT_LABEL_CLASS, None, (None, None), geometry, layer, report));
        }
        Some("rule-based") => {
            let mut rules = Vec::new();
            let mut left = 0;
            if let Some(root) = child(l, "rules") {
                flatten(root, Vec::new(), (None, None), Vec::new(), &mut rules, 0, &mut left);
            }
            if left > 0 {
                report.skipped(
                    layer,
                    format!("{left} of its label rules, past the {} RustGIS reads or nested more than 32 deep", MAX_LABEL_CLASSES * 4),
                );
            }
            for (i, r) in rules.iter().enumerate() {
                let Some(settings) = child(r.node, "settings") else { continue };
                // Switched off: never drawn, so nothing about it is noted.
                if r.node.attribute("active") == Some("0") {
                    continue;
                }
                let name = r
                    .node
                    .attribute("description")
                    .filter(|d| !d.trim().is_empty())
                    .map_or_else(|| format!("Rule {}", i + 1), str::to_string);
                // Its filters and its parents', and for an else rule, none of its siblings'.
                let mut parts: Vec<Option<String>> = r.filters.iter().map(|f| expr::to_sql(f)).collect();
                // (As the renderer's else rules: a sibling unknown for a feature isn't met, one
                // without a filter takes every feature, each at its scales.)
                for siblings in &r.else_of {
                    parts.push(super::none_of(siblings));
                }
                let where_ = parts.into_iter().collect::<Option<Vec<String>>>().map(|p| {
                    let p: Vec<String> = p.into_iter().filter(|c| !c.is_empty()).collect();
                    match p.as_slice() {
                        [] => None,
                        [one] => Some(one.clone()),
                        many => Some(format!("({})", many.join(") AND ("))),
                    }
                });
                // (One too long to read is left out as one unreadable.)
                let where_ = where_.filter(|w| w.as_deref().is_none_or(|w| w.chars().count() <= rustgis_data::query::MAX_LEN));
                let Some(where_) = where_ else {
                    report.skipped(
                        layer,
                        if r.else_of.is_empty() {
                            format!("the label rule \"{name}\", whose filter `{}` is empty or too long to read", r.filters.join(" AND "))
                        } else {
                            format!(
                                "the label rule \"{name}\", an else rule whose siblings' filters (or its own) are too long or can't be read"
                            )
                        },
                    );
                    continue;
                };
                classes.extend(class(settings, &name, where_, r.scales, geometry, layer, report));
            }
        }
        Some(other) => {
            report.skipped(layer, format!("its {other} labels"));
            return None;
        }
        None => return None,
    }
    if classes.is_empty() {
        return None;
    }
    if classes.len() > MAX_LABEL_CLASSES {
        report.skipped(layer, format!("label rules past the first {MAX_LABEL_CLASSES}"));
        classes.truncate(MAX_LABEL_CLASSES);
    }
    // A simple labeling is the Default class; rules are classes of their own, Default off.
    let default = if l.attribute("type") == Some("simple") {
        classes.remove(0)
    } else {
        let first = classes.first()?.clone();
        LabelClass { name: DEFAULT_LABEL_CLASS.into(), visible: false, where_: None, ..first }
    };
    let classes = if l.attribute("type") == Some("simple") { Vec::new() } else { classes };
    Some(Labeling { enabled: true, default, classes })
}

fn is_else(f: &str) -> bool {
    f.trim().eq_ignore_ascii_case("ELSE")
}

/// A rule-based labeling's rule with settings: its filter and its parents' (an else rule's own
/// left out), for an else rule and each else rule above it the siblings' filters it takes the rest
/// of (what none of them does), and the scale range along the way (RustGIS's min and max scale).
struct Rule<'a, 'i> {
    node: Node<'a, 'i>,
    filters: Vec<String>,
    else_of: Vec<std::rc::Rc<Option<Vec<super::Sibling>>>>,
    scales: (Option<f64>, Option<f64>),
}

/// The label rules under `node`, bounded in depth and number: the rules left out are counted in
/// `left`. A rule under an else rule takes only what the else rule does (`else_of`).
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
    // A label rule draws with its settings.
    let draws = |r: Node| child(r, "settings").is_some();
    if depth > 32 || out.len() > MAX_LABEL_CLASSES * 4 {
        *left += super::drawing_rules(node.children(), draws);
        return;
    }
    let rules: Vec<Node> = node.children().filter(|c| c.has_tag_name("rule")).take(MAX_LABEL_CLASSES * 4).collect();
    *left += super::drawing_rules(node.children().filter(|c| c.has_tag_name("rule")).skip(rules.len()), draws);
    // A rule is labelled only within its own scale range and its parents' (as the renderer has it).
    let range = |rule: Node| super::rule_scales(rule, scales);
    // What an else rule here is the rest of: its siblings where they label (one without a filter
    // takes every feature), each at its scales, as QGIS has it (read only when there is one).
    let siblings: std::rc::Rc<Option<Vec<super::Sibling>>> =
        std::rc::Rc::new(if rules.iter().any(|r| r.attribute("filter").is_some_and(is_else)) {
            let mut budget = MAX_LABEL_CLASSES * 4;
            super::siblings(rules.iter().copied(), scales, &draws, false, depth, &mut budget)
        } else {
            Some(Vec::new())
        });
    for (i, rule) in rules.iter().copied().enumerate() {
        if out.len() > MAX_LABEL_CLASSES * 4 {
            *left += super::drawing_rules(rules.get(i..).unwrap_or_default().iter().copied(), draws);
            break;
        }
        // Switched off: neither it nor the rules under it label.
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
        if child(rule, "settings").is_some() {
            out.push(Rule { node: rule, filters: f.clone(), else_of: else_of.clone(), scales: s });
        }
        flatten(rule, f, s, else_of, out, depth + 1, left);
    }
}

/// A label class from QGIS's label `settings`, or `None` when its text can't be carried over.
fn class(
    settings: Node,
    name: &str,
    where_: Option<String>,
    scales: (Option<f64>, Option<f64>),
    geometry: Option<&str>,
    layer: &str,
    report: &mut Report,
) -> Option<LabelClass> {
    let text = child(settings, "text-style")?;
    let field = text.attribute("fieldName").map(str::trim).unwrap_or_default();
    let mut by = LabelBy::default();
    let field = if text.attribute("isExpression") == Some("1") {
        // An expression that is only a field labels from it; any other is the text's expression
        // (one RustGIS can't read is noted when the layer's fields are known).
        let bare = field.strip_prefix('"').and_then(|f| f.strip_suffix('"')).unwrap_or(field);
        // (Empty first: a quoted space is no field name.)
        if bare.trim().is_empty() {
            report.skipped(layer, "labels whose text is an empty expression");
            return None;
        } else if bare.chars().all(|c| c.is_alphanumeric() || c == '_' || c == ' ') {
            bare.to_string()
        } else if field.chars().count() > rustgis_data::query::MAX_LEN {
            report.skipped(layer, "labels whose text is an expression too long to read");
            return None;
        } else {
            let text = super::expr::rustgis_scales(field);
            // (Its map scale read as RustGIS's lengthens it.)
            if text.chars().count() > rustgis_data::query::MAX_LEN {
                report.skipped(layer, "labels whose text is an expression too long to read");
                return None;
            }
            by.text = Some(text).filter(|t| !t.is_empty());
            String::new()
        }
    } else {
        field.to_string()
    };
    if field.is_empty() && by.text.is_none() {
        return None;
    }
    let num = |n: Node, k: &str| n.attribute(k).and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite());
    // The size in typographic points, as RustGIS's label sizes are; its letter spacing in the
    // size's unit.
    let size_unit = text.attribute("fontSizeUnit").unwrap_or("Point");
    let size = points(num(text, "fontSize").unwrap_or(10.0), size_unit, "label sizes", layer, report);
    data_defined(settings, size_unit, &mut by, layer, report);
    let opacity = num(text, "textOpacity").unwrap_or(1.0).clamp(0.0, 1.0) as f32;
    let c = text.attribute("textColor").and_then(color).unwrap_or(Rgba::rgb(0, 0, 0));
    let c = c.with_alpha((f32::from(c.a()) * opacity).round() as u8);
    if by.color.is_some() && opacity < 1.0 {
        report.approximated(layer, "see-through labels coloured by expression, drawn opaque");
    }
    let style = text.attribute("namedStyle").unwrap_or_default().to_ascii_lowercase();
    let weight = num(text, "fontWeight").unwrap_or(50.0);
    let bold = style.contains("bold")
        || style.contains("black")
        || style.contains("heavy")
        || weight >= 63.0
        || text.attribute("fontBold") == Some("1");
    let italic = style.contains("italic") || style.contains("oblique") || text.attribute("fontItalic") == Some("1");
    // QGIS's font family: drawn in it where it is installed, else in RustGIS's label font.
    let font = text.attribute("fontFamily").and_then(crate::symbology::font_name);
    let spacing =
        num(text, "fontLetterSpacing").filter(|s| *s != 0.0).map_or(0.0, |s| points(s, size_unit, "label letter spacing", layer, report));
    let (spacing_min, spacing_max) = (crate::layout::MIN_LETTER_SPACING, crate::layout::MAX_LETTER_SPACING);
    // Its buffer as the halo, in the buffer's colour (and opacity) and size.
    let buffer = child(text, "text-buffer").filter(|b| b.attribute("bufferDraw") == Some("1"));
    let halo = buffer.is_some();
    // (QGIS's buffer is white unless it says otherwise.)
    let halo_color = buffer.map(|b| b.attribute("bufferColor").and_then(color).unwrap_or(Rgba::rgb(255, 255, 255))).map(|c| {
        let opacity = buffer.and_then(|b| num(b, "bufferOpacity")).unwrap_or(1.0).clamp(0.0, 1.0) as f32;
        c.with_alpha((f32::from(c.a()) * opacity).round() as u8)
    });
    let halo_width = buffer.map(|b| {
        let w = points(num(b, "bufferSize").unwrap_or(1.0), b.attribute("bufferSizeUnits").unwrap_or("MM"), "label halos", layer, report);
        w.clamp(*HALO_WIDTH.start(), *HALO_WIDTH.end()) as f32
    });
    if let Some(b) = buffer {
        if b.attribute("bufferJoinStyle").is_some_and(|j| j != "128") {
            report.approximated(layer, "label halos with sharp or bevelled corners, drawn round");
        }
        if !matches!(b.attribute("bufferBlendMode"), None | Some("0")) {
            report.approximated(layer, "label halos blended into the map, drawn over it");
        }
    }
    let background = background(text, layer, report);
    if child(text, "shadow").is_some_and(|b| b.attribute("shadowDraw") == Some("1")) {
        report.approximated(layer, "label shadows, left out");
    }
    let placement = child(settings, "placement").map_or(LabelPlacement::Auto, |p| placement(p, layer, report));
    let offset = child(settings, "placement").map_or([0.0; 2], |p| offset(p, geometry, layer, report));
    // (Without a `<text-format>`, QGIS's lines follow the placement too.)
    let (wrap_char, wrap_length, line_align) =
        child(settings, "text-format").map_or((None, 0, follow_placement(placement)), |f| wrapping(f, placement, layer, report));
    let repeat = child(settings, "placement").and_then(|p| repeat(p, placement, geometry, layer, report));
    unread(settings, geometry, layer, report);
    // Within the settings' own scale range and the rule's (and its parents').
    let rendering = child(settings, "rendering");
    let (mut min_scale, mut max_scale) = scales;
    if let Some(r) = rendering.filter(|r| r.attribute("scaleVisibility") == Some("1")) {
        let limit = |k: &str| num(r, k).filter(|v| *v > 0.0).map(|v| v * QGIS_SCALE);
        min_scale = match (limit("scaleMin"), min_scale) {
            (Some(own), Some(rule)) => Some(own.min(rule)),
            (own, rule) => own.or(rule),
        };
        max_scale = match (limit("scaleMax"), max_scale) {
            (Some(own), Some(rule)) => Some(own.max(rule)),
            (own, rule) => own.or(rule),
        };
    }
    Some(LabelClass {
        name: name.to_string(),
        visible: true,
        where_,
        field,
        size: size.clamp(*LABEL_SIZE.start(), *LABEL_SIZE.end()) as f32,
        color: c,
        halo,
        bold,
        min_scale,
        max_scale,
        closest: None,
        farthest: None,
        placement,
        other_positions: false,
        italic,
        font,
        halo_color,
        halo_width,
        spacing: spacing.clamp(spacing_min, spacing_max) as f32,
        offset,
        background,
        wrap_char,
        wrap_length,
        line_align,
        repeat,
        by,
    })
}

/// Takes label `settings`' data-defined size (in `size_unit`, turned into points), colour and
/// rotation into `by` as expressions, and notes the others by name.
fn data_defined(settings: Node, size_unit: &str, by: &mut LabelBy, layer: &str, report: &mut Report) {
    let active = super::symbol::active_properties_in(settings, "dd_properties");
    // A layout that isn't read (QGIS 2's) is still noted.
    if active.is_empty() && data_defined_on(settings) {
        report.skipped(layer, "data-defined label settings (from fields or expressions)");
    }
    let mut left: Vec<&str> = Vec::new();
    for (name, property) in active {
        let text = super::symbol::property_text(property);
        let slot = match name {
            "Size" => &mut by.size,
            "Color" => &mut by.color,
            // (Clockwise in degrees, as RustGIS's; QGIS 2's `Rotation` turned counterclockwise.)
            "LabelRotation" | "Rotation" => &mut by.angle,
            _ => {
                left.push(name);
                continue;
            }
        };
        let Some(text) = text else {
            left.push(name);
            continue;
        };
        let text = match name {
            // QGIS's size is in the text's unit.
            "Size" => {
                let k = points(1.0, size_unit, "label sizes", layer, report);
                if (k - 1.0).abs() < 1e-9 || !k.is_finite() { text } else { format!("({text}) * {}", (k * 1e6).round() / 1e6) }
            }
            "Rotation" => format!("-({text})"),
            _ => text,
        };
        if slot.is_none() {
            *slot = Some(text);
        } else {
            // Both rotations set: the first is kept, the other noted.
            left.push(name);
        }
    }
    if !left.is_empty() {
        left.sort_unstable();
        left.dedup();
        let more = left.len().saturating_sub(12);
        left.truncate(12);
        let more = if more > 0 { format!(", and {more} more") } else { String::new() };
        report.skipped(layer, format!("data-defined label settings ({}{more})", left.join(", ")));
    }
}

/// A label's wrapping (QGIS's `<text-format>`): its wrap character, its automatic wrap length
/// (characters), and how its lines line up (QGIS's "follow placement" as the side the label
/// sits on: right-aligned left of its feature, left-aligned right of it, centred otherwise).
fn wrapping(f: Node, placement: LabelPlacement, layer: &str, report: &mut Report) -> (Option<String>, u32, TextAlign) {
    let num = |k: &str| f.attribute(k).and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite());
    let wrap_char = f.attribute("wrapChar").and_then(crate::symbology::wrap_char);
    let length = num("autoWrapLength").filter(|v| *v > 0.0).map_or(0, |v| v.min(f64::from(MAX_WRAP_LENGTH)) as u32);
    if length > 0 && f.attribute("useMaxLineLengthForAutoWrap") == Some("0") {
        report.approximated(layer, "labels wrapped at a shortest line length, wrapped at that length at most");
    }
    let align = match f.attribute("multilineAlign").unwrap_or("3") {
        "0" => TextAlign::Left,
        "1" => TextAlign::Center,
        "2" => TextAlign::Right,
        "4" => {
            report.approximated(layer, "justified label lines, centred");
            TextAlign::Center
        }
        _ => follow_placement(placement),
    };
    (wrap_char, length, align)
}

/// QGIS's "follow placement" for a label's lines: right-aligned left of its feature, left-aligned
/// right of it, centred otherwise.
fn follow_placement(placement: LabelPlacement) -> TextAlign {
    match placement {
        LabelPlacement::Left | LabelPlacement::AboveLeft | LabelPlacement::BelowLeft => TextAlign::Right,
        LabelPlacement::Right | LabelPlacement::AboveRight | LabelPlacement::BelowRight => TextAlign::Left,
        _ => TextAlign::Center,
    }
}

/// A label's repeat along its line, in points: QGIS's repeat distance in its units, for a label
/// placed along a line. Round a polygon's outline (QGIS's placement along a polygon layer's lines,
/// or round its perimeter) RustGIS labels it once, and says so.
fn repeat(p: Node, placement: LabelPlacement, geometry: Option<&str>, layer: &str, report: &mut Report) -> Option<f32> {
    let d = p.attribute("repeatDistance").and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite() && *v > 0.0)?;
    // (QGIS repeats only along lines: a polygon's outline with Line or Curved, or round its
    // perimeter, curved.)
    let placed = p.attribute("placement");
    let round_polygons = (geometry == Some("Polygon") && matches!(placed, Some("2" | "3"))) || placed == Some("7");
    if round_polygons {
        report.approximated(layer, "labels repeated round polygons, drawn once");
        return None;
    }
    if !placement.along_line() {
        return None;
    }
    // Before QGIS 3 the unit was a number (0 points, 1 millimetres, 2 map units), which QGIS
    // reads when the named one isn't there.
    let unit = p.attribute("repeatDistanceUnits").unwrap_or(match p.attribute("repeatDistanceUnit") {
        Some("0") => "Point",
        Some("2") => "MapUnit",
        _ => "MM",
    });
    crate::symbology::label_repeat(points(d, unit, "label repeat distances", layer, report))
}

/// A label's background as RustGIS's rectangle: QGIS's rectangle (its square, ellipse and circle
/// drawn as one), padded by its size, filled and outlined as its fill symbol says (else its own
/// colours), its corners rounded.
fn background(text: Node, layer: &str, report: &mut Report) -> Option<LabelBackground> {
    let b = child(text, "background").filter(|b| b.attribute("shapeDraw") == Some("1"))?;
    let num = |n: Node, k: &str| n.attribute(k).and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite());
    match b.attribute("shapeType").unwrap_or("0") {
        "0" => {}
        "1" | "2" | "3" => report.approximated(layer, "label backgrounds of squares, ellipses or circles, drawn as rectangles"),
        _ => {
            report.approximated(layer, "label backgrounds of SVG pictures or markers, left out");
            return None;
        }
    }
    let what = "label backgrounds";
    // Its size added round the text (a fixed size or a share of the text's, as padding of none).
    let padding = if b.attribute("shapeSizeType").unwrap_or("0") == "0" {
        let unit = b.attribute("shapeSizeUnit").unwrap_or("MM");
        ["shapeSizeX", "shapeSizeY"].map(|k| num(b, k).map_or(0.0, |v| points(v, unit, what, layer, report)) as f32)
    } else {
        report.approximated(layer, "label backgrounds of a size of their own, fitted to the text");
        [0.0; 2]
    };
    let radius = match b.attribute("shapeRadiiUnit").unwrap_or("MM") {
        // (QGIS writes `Percentage`.)
        "Percentage" | "Percent" => {
            report.approximated(layer, "label backgrounds' corners rounded by a share of their size, drawn square");
            0.0
        }
        unit => {
            let r = num(b, "shapeRadiiX").unwrap_or(0.0).min(num(b, "shapeRadiiY").unwrap_or(0.0));
            if r > 0.0 { points(r, unit, what, layer, report) as f32 } else { 0.0 }
        }
    };
    // QGIS 3 fills it with a fill symbol; older files, and a symbol of another kind, by its colours.
    let fill_layer = b
        .children()
        .find(|c| c.has_tag_name("symbol") && c.attribute("type") == Some("fill"))
        .and_then(|sym| sym.children().find(|c| c.has_tag_name("layer")).map(|l| (sym, l)));
    let (fill, stroke, stroke_width) = match fill_layer {
        Some((sym, l)) if l.attribute("class") == Some("SimpleFill") => {
            let pr = props(l);
            let alpha = num(sym, "alpha").unwrap_or(1.0).clamp(0.0, 1.0) as f32;
            let faded = |c: Rgba| c.with_alpha((f32::from(c.a()) * alpha).round() as u8);
            let fill = (prop(&pr, "style") != Some("no")).then(|| prop(&pr, "color").and_then(color)).flatten().map(faded);
            let stroke =
                (prop(&pr, "outline_style") != Some("no")).then(|| prop(&pr, "outline_color").and_then(color)).flatten().map(faded);
            let width = prop(&pr, "outline_width").and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite()).unwrap_or(0.26);
            let unit = prop(&pr, "outline_width_unit").unwrap_or("MM");
            (fill, stroke, points(width, unit, what, layer, report))
        }
        other => {
            if other.is_some() {
                report.approximated(layer, "label backgrounds filled with patterns or gradients, drawn in their colours");
            }
            let fill = b.attribute("shapeFillColor").and_then(color).or(Some(Rgba::rgb(255, 255, 255)));
            let stroke = b.attribute("shapeBorderColor").and_then(color);
            let width = num(b, "shapeBorderWidth").unwrap_or(0.0);
            (fill, stroke, points(width, b.attribute("shapeBorderWidthUnit").unwrap_or("MM"), what, layer, report))
        }
    };
    let opacity = num(b, "shapeOpacity").unwrap_or(1.0).clamp(0.0, 1.0) as f32;
    let faded = |c: Rgba| c.with_alpha((f32::from(c.a()) * opacity).round() as u8);
    if num(b, "shapeOffsetX").is_some_and(|v| v != 0.0) || num(b, "shapeOffsetY").is_some_and(|v| v != 0.0) {
        report.approximated(layer, "label backgrounds moved off their text, drawn behind it");
    }
    if num(b, "shapeRotation").is_some_and(|v| v != 0.0) {
        report.approximated(layer, "label backgrounds turned on their own, drawn with their text");
    }
    if !matches!(b.attribute("shapeBlendMode"), None | Some("0")) {
        report.approximated(layer, "label backgrounds blended into the map, drawn over it");
    }
    let stroke = stroke.map(faded).filter(|_| stroke_width > 0.0);
    Some(LabelBackground { fill: fill.map(faded), stroke, stroke_width: stroke_width as f32, padding, radius }.checked())
}

/// How far a label is moved from where its placement puts it (points, right and down): QGIS's
/// offset from a point (or a polygon's centroid), and on a point layer its distance around the
/// point, the label above it that far out (RustGIS's Auto puts it above).
fn offset(p: Node, geometry: Option<&str>, layer: &str, report: &mut Report) -> [f32; 2] {
    let num = |k: &str| p.attribute(k).and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite());
    let within = |v: f64| v.clamp(*LABEL_OFFSET.start(), *LABEL_OFFSET.end()) as f32;
    match p.attribute("placement").unwrap_or("0") {
        "1" => {
            let (x, y) = (num("xOffset").unwrap_or(0.0), num("yOffset").unwrap_or(0.0));
            if x == 0.0 && y == 0.0 {
                return [0.0; 2];
            }
            let unit = p.attribute("offsetUnits").unwrap_or("MM");
            [x, y].map(|v| within(points(v, unit, "label offsets", layer, report)))
        }
        "0" | "6" if geometry == Some("Point") => match num("dist").filter(|d| *d != 0.0) {
            Some(d) => {
                report.approximated(layer, "label distances round points, moved straight up");
                [0.0, -within(points(d, p.attribute("distUnits").unwrap_or("MM"), "label distances", layer, report))]
            }
            None => [0.0; 2],
        },
        _ => [0.0; 2],
    }
}

/// A QGIS length (`what`'s, for the report) in typographic points: map units taken at 1:25,000.
fn points(v: f64, unit: &str, what: &str, layer: &str, report: &mut Report) -> f64 {
    match unit {
        "Point" => v,
        "MM" => v * 72.0 / 25.4,
        "Pixel" => v * 0.75,
        "Inch" => v * 72.0,
        _ => {
            report.approximated(layer, format!("{what} in map units, taken at 1:25,000"));
            v / 25_000.0 * 72.0 / 0.0254
        }
    }
}

/// Notes the label settings RustGIS has no place for, where they are set off QGIS's defaults.
fn unread(settings: Node, geometry: Option<&str>, layer: &str, report: &mut Report) {
    let num =
        |n: Option<Node>, k: &str| n.and_then(|n| n.attribute(k)).and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite());
    let p = child(settings, "placement");
    // A distance from a line, or round a polygon (or a layer of unknown kind); offset from a
    // point, QGIS doesn't use it, and round a point it is the label's offset.
    let placed = p.and_then(|p| p.attribute("placement")).unwrap_or("0");
    let used = match placed {
        "0" | "6" => geometry != Some("Point"),
        "2" | "3" | "7" | "8" => true,
        _ => false,
    };
    if used && num(p, "dist").is_some_and(|d| d != 0.0) {
        report.approximated(layer, "label distances from their features, left out");
    }
    if num(p, "priority").is_some_and(|d| d != 5.0) {
        report.approximated(layer, "label priorities, left out");
    }
    if child(settings, "callout").is_some_and(|c| prop(&props(c), "enabled") == Some("1")) {
        report.skipped(layer, "label callouts (lines from a label to its feature)");
    }
}

/// QGIS's label placement, as RustGIS's.
fn placement(p: Node, layer: &str, report: &mut Report) -> LabelPlacement {
    let quad = |q: Option<&str>| match q {
        Some("0") => LabelPlacement::AboveLeft,
        Some("1") => LabelPlacement::Above,
        Some("2") => LabelPlacement::AboveRight,
        Some("3") => LabelPlacement::Left,
        Some("5") => LabelPlacement::Right,
        Some("6") => LabelPlacement::BelowLeft,
        Some("7") => LabelPlacement::Below,
        Some("8") => LabelPlacement::BelowRight,
        _ => LabelPlacement::Center,
    };
    match p.attribute("placement").unwrap_or("0") {
        // Around the point: QGIS picks the best spot, RustGIS's automatic is above it.
        "0" | "6" => LabelPlacement::Auto,
        "1" => quad(p.attribute("quadOffset")),
        "2" => LabelPlacement::Parallel,
        "3" => LabelPlacement::Curved,
        "4" | "5" => LabelPlacement::Center,
        other => {
            report.approximated(layer, format!("label placement {}, centred", placement_name(other)));
            LabelPlacement::Center
        }
    }
}

fn placement_name(code: &str) -> &'static str {
    match code {
        "7" => "around polygon outlines",
        "8" => "outside polygons",
        _ => "of an unknown kind",
    }
}
