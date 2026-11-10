//! QGIS diagrams as RustGIS diagrams ([`crate::DiagramSettings`]): pie charts as pies, of a fixed
//! size or one interpolated between two values. Histograms and stacked bars are noted and left
//! out: QGIS draws a histogram's bars by their values (and a fixed-size one not at all), a stacked
//! bar as one column, where RustGIS's bars are each category's share side by side, which on
//! QGIS's renders differs from them more than drawing none.
//!
//! What QGIS writes, as measured on its renders: a pie's `width` is its diameter;
//! `rotationOffset` is where the first slice starts, in degrees clockwise from 3 o'clock (none:
//! 3 o'clock), unless an older file's `angleOffset` is there, which QGIS takes over it: sixteenths
//! of a degree counterclockwise from 3 o'clock; and `direction="0"` lays the slices clockwise,
//! anything else (`1`, or none) counterclockwise. RustGIS draws slices
//! clockwise, so a counterclockwise pie is drawn with its categories in reverse order from the
//! same start, which covers the same angles.

use roxmltree::Node;

use super::{Report, child, color, points, renderer::field_name};
use crate::Rgba;
use crate::diagram::{
    DiagramCategory, DiagramKind, DiagramLabel, DiagramSettings, DiagramSize, MAX_CATEGORIES, MAX_SIZE, MAX_STROKE_WIDTH,
};

/// The layer's diagram from its `<…DiagramRenderer>` (a child of a project's `<maplayer>` or a
/// style's `<qgis>`), or `None` when it has none, it is switched off or nothing of it can be
/// drawn (noted). `reference` is the layer's reference scale (RustGIS's), for sizes in map units.
pub(super) fn read(node: Node, layer: &str, reference: Option<f64>, report: &mut Report) -> Option<DiagramSettings> {
    let r = node.children().find(|c| c.is_element() && c.tag_name().name().ends_with("DiagramRenderer"))?;
    let cats: Vec<Node> = r.children().filter(|c| c.has_tag_name("DiagramCategory")).collect();
    // Switched off: QGIS draws nothing, and neither does RustGIS.
    let cat = *cats.iter().find(|c| c.attribute("enabled") != Some("0"))?;
    let renderer = r.tag_name().name();
    let scaled = match renderer {
        "SingleCategoryDiagramRenderer" => false,
        "LinearlyInterpolatedDiagramRenderer" => true,
        other => {
            report.skipped(layer, format!("its diagrams (a {other})"));
            return None;
        }
    };
    match r.attribute("diagramType").unwrap_or("Pie") {
        "Pie" => {}
        "Histogram" => {
            report.skipped(layer, "its histogram diagrams (RustGIS's bar diagrams draw each category's share, not its value)");
            return None;
        }
        "Stacked" | "StackedBar" => {
            report.skipped(layer, "its stacked bar diagrams");
            return None;
        }
        other => {
            report.skipped(layer, format!("its diagrams (of the kind {other})"));
            return None;
        }
    }
    if cats.iter().filter(|c| c.attribute("enabled") != Some("0")).count() > 1 {
        report.approximated(layer, "its stacked diagrams, of which only the first is drawn");
    }
    let num = |n: Node, k: &str| n.attribute(k).and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite());
    // The diagram's own opacity, over each colour's.
    let opacity = num(cat, "opacity").map_or(1.0, |o| o.clamp(0.0, 1.0));
    let faded = |c: Rgba, alpha: f64| c.with_alpha((f64::from(c.a()) * alpha.clamp(0.0, 1.0) * opacity).round() as u8);

    let mut categories = Vec::new();
    let mut expressions = Vec::new();
    let mut renamed = Vec::new();
    for a in cat.children().filter(|c| c.has_tag_name("attribute")) {
        let attr = a.attribute("field").unwrap_or_default();
        let Some(field) = field_name(attr) else {
            if !attr.trim().is_empty() {
                expressions.push(attr.trim().to_string());
            }
            continue;
        };
        // RustGIS's legend names a category by its field: a label QGIS gives it otherwise is noted.
        if let Some(label) = a.attribute("label").map(str::trim).filter(|l| !l.is_empty() && *l != field) {
            let more = if label.chars().count() > 40 { "…" } else { "" };
            renamed.push(format!("{field} as \"{}{more}\"", label.chars().take(40).collect::<String>()));
        }
        let colour = a.attribute("color").and_then(color).unwrap_or(Rgba::rgb(0x80, 0x80, 0x80));
        categories.push(DiagramCategory { field, colour: faded(colour, num(a, "colorOpacity").unwrap_or(1.0)) });
    }
    if let Some(first) = expressions.first() {
        let quoted: String = first.chars().take(80).collect();
        let more = if first.chars().count() > 80 { "…" } else { "" };
        report.skipped(
            layer,
            format!(
                "{} diagram categor{} by an expression (the first: `{quoted}{more}`): only fields are drawn",
                expressions.len(),
                if expressions.len() == 1 { "y" } else { "ies" }
            ),
        );
    }
    if let Some(first) = renamed.first() {
        report.approximated(
            layer,
            format!(
                "{} diagram categor{} with a label of {} own, which RustGIS's legend shows by {} (the first: {first})",
                renamed.len(),
                if renamed.len() == 1 { "y" } else { "ies" },
                if renamed.len() == 1 { "its" } else { "their" },
                if renamed.len() == 1 { "its field" } else { "their fields" },
            ),
        );
    }
    if categories.is_empty() {
        report.skipped(layer, "its diagrams, which have no category from a field");
        return None;
    }
    if categories.len() > MAX_CATEGORIES {
        report.skipped(layer, format!("{} diagram categories past the first {MAX_CATEGORIES}", categories.len() - MAX_CATEGORIES));
        categories.truncate(MAX_CATEGORIES);
    }

    let unit = cat.attribute("sizeType");
    // A pie's size is its radius; QGIS's width is its diameter.
    let size_of = |w: f64, report: &mut Report| points(w / 2.0, unit, reference, layer, report);
    let size = if scaled {
        let (min_size, max_size) =
            (size_of(num(r, "lowerWidth").unwrap_or(0.0), report), size_of(num(r, "upperWidth").unwrap_or(0.0), report));
        let (min, max) = (num(r, "lowerValue").unwrap_or(0.0), num(r, "upperValue").unwrap_or(0.0));
        let by = ["classificationAttributeExpression", "classificationField"]
            .into_iter()
            .filter_map(|k| r.attribute(k).map(str::trim))
            .find(|v| !v.is_empty())
            .unwrap_or_default();
        if !sums_categories(by, &categories) {
            let what =
                if by.is_empty() { "a field by its number".to_string() } else { format!("`{}`", by.chars().take(80).collect::<String>()) };
            report.approximated(layer, format!("its diagrams sized by the sum of their categories, not by {what}"));
        }
        // QGIS grows the pie's area from the lower value's, RustGIS from nothing: the two agree
        // when the range starts at 0.
        if min != 0.0 || num(cat, "minimumSize").is_some_and(|m| m > 0.0) {
            report.approximated(layer, "its diagrams' sizes between two values, from a lower value other than 0 or with a smallest size: grown a little differently");
        }
        if cat.attribute("scaleDependency") == Some("Diameter") {
            report.approximated(layer, "its diagrams scaled by their diameter, drawn scaled by their area");
        }
        DiagramSize::Scaled { min_size, max_size, min, max }
    } else {
        DiagramSize::Fixed(size_of(num(cat, "width").unwrap_or(12.0), report))
    };

    // Where the first slice starts: QGIS counts clockwise from 3 o'clock, RustGIS from 12.
    // An older file's angleOffset (sixteenths of a degree counterclockwise from 3 o'clock) wins.
    let start = match num(cat, "angleOffset") {
        Some(a) => 90.0 - a / 16.0,
        None => num(cat, "rotationOffset").unwrap_or(0.0) + 90.0,
    };
    let start_angle = (start.rem_euclid(360.0) as f32).rem_euclid(360.0);
    if cat.attribute("direction") != Some("0") {
        categories.reverse();
    }

    let alpha = |k: &str| num(cat, k).map_or(1.0, |a| a / 255.0);
    let background = cat.attribute("backgroundColor").and_then(color).map(|c| faded(c, alpha("backgroundAlpha"))).filter(|c| c.a() > 0);
    let stroke = cat.attribute("penColor").and_then(color).map_or(DiagramSettings::default_stroke(), |c| faded(c, alpha("penAlpha")));
    let stroke_width = num(cat, "penWidth").map_or(0.0, |w| points(w, cat.attribute("lineSizeType"), reference, layer, report));

    if cat.attribute("scaleBasedVisibility") == Some("1") {
        report.skipped(layer, "its diagrams' scale range: drawn at every scale");
    }
    if let Some(s) = child(node, "DiagramLayerSettings") {
        if num(s, "dist").is_some_and(|d| d != 0.0) {
            report.approximated(layer, "its diagrams' distance from their features: drawn on them");
        }
        if s.attribute("showAll") == Some("0") {
            report.approximated(layer, "its diagrams where they overlap, all drawn (QGIS leaves some out)");
        }
        let active = s
            .descendants()
            .any(|o| o.has_tag_name("Option") && o.attribute("name") == Some("active") && o.attribute("value") == Some("true"));
        if active {
            report.skipped(layer, "its diagrams' data-defined settings");
        }
    }

    let largest = match size {
        DiagramSize::Fixed(s) => s,
        DiagramSize::Scaled { min_size, max_size, .. } => min_size.max(max_size),
    };
    if largest > MAX_SIZE {
        report.approximated(
            layer,
            format!("its diagrams larger than RustGIS draws them, drawn {} mm across", (MAX_SIZE * 2.0 * 25.4 / 96.0).round()),
        );
    }
    if stroke_width > MAX_STROKE_WIDTH {
        report.approximated(
            layer,
            format!("its diagrams' outline, wider than RustGIS draws it, drawn {:.1} mm wide", MAX_STROKE_WIDTH * 25.4 / 96.0),
        );
    }
    let d = DiagramSettings {
        kind: DiagramKind::Pie,
        categories,
        size,
        start_angle,
        background,
        stroke,
        stroke_width,
        labels: DiagramLabel::None,
    }
    .clamped();
    (!d.categories.is_empty()).then_some(d)
}

/// Whether `by` (a field, or an expression adding fields) is the sum of exactly the categories'
/// fields, which is what a RustGIS diagram is sized by.
fn sums_categories(by: &str, categories: &[DiagramCategory]) -> bool {
    let Some(mut terms) = by.split('+').map(field_name).collect::<Option<Vec<String>>>() else { return false };
    let mut fields: Vec<String> = categories.iter().map(|c| c.field.to_lowercase()).collect();
    terms.iter_mut().for_each(|t| *t = t.to_lowercase());
    terms.sort();
    fields.sort();
    terms == fields
}
