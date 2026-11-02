//! QGIS projects (`.qgs`) and layer styles (`.qml`) read into RustGIS: a project's layers (their
//! data sources, order, visibility, scale ranges and opacity), its coordinate system and view,
//! and each layer's style (renderer, symbols, labels, reference scale and symbol levels), as the
//! map's own types. What RustGIS can't draw as QGIS does is carried over as near as it can, and
//! the [`Report`] says so, layer by layer: nothing is left out without a note.
//!
//! The XML is hostile input: it is read with a node limit and no DTD, every list is bounded, and
//! sizes and scales are brought into range before they reach a layer. Both of QGIS's ways of
//! writing a symbol layer's settings are read: the `<Option type="Map">` of QGIS 3.26 and later,
//! and the `<prop k="…" v="…">` before it.

mod expr;
mod labels;
mod raster;
mod renderer;
mod source;
mod symbol;
#[cfg(test)]
mod tests;

use std::collections::HashMap;

use roxmltree::{Document, Node, ParsingOptions};
use serde::Serialize;

use crate::Rgba;
use crate::effects::Blend;
use crate::symbology::{Labeling, LineStroke, MarkerRotation, Renderer};
pub use expr::{reads_measure, to_sql};
pub use source::{Source, is_drive_relative, is_relative};

/// The most XML nodes a project or style is read with (a large project's file is some hundred
/// thousand).
const MAX_NODES: u32 = 5_000_000;
/// The most layers read from a project.
pub const MAX_LAYERS: usize = 1000;
/// The most notes a report keeps.
const MAX_NOTES: usize = 5000;

/// RustGIS's scale takes a screen pixel to be 0.28 mm (OGC); QGIS works out its scale at the
/// screen's 96 dots to the inch. The same view is a larger scale in QGIS by this much, so QGIS's
/// scales (reference scales, visibility ranges) are multiplied by it.
pub const QGIS_SCALE: f64 = (25.4 / 96.0) / 0.28;

/// What an import carried over only in part, or left out, layer by layer.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Report {
    /// Added to by [`Report::note`] only, which keeps `index` in step.
    pub notes: Vec<Note>,
    /// Where each note is in `notes`, by layer, kind and text: a hostile project can make
    /// thousands of different notes, and a repeat is found without going through them all.
    #[serde(skip)]
    index: HashMap<(String, NoteKind, String), usize>,
}

/// One thing an import couldn't carry over as QGIS has it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Note {
    /// The layer it is about (empty for the project).
    pub layer: String,
    pub kind: NoteKind,
    pub text: String,
    /// How many times it came up (once per symbol, class or rule it applies to).
    pub count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum NoteKind {
    /// Drawn, but not quite as QGIS draws it.
    Approximated,
    /// Not carried over at all.
    Skipped,
}

impl Report {
    /// Notes `text` about `layer`, counting a repeat rather than listing it again.
    pub fn note(&mut self, layer: &str, kind: NoteKind, text: impl Into<String>) {
        let key = (layer.to_string(), kind, text.into());
        if let Some(n) = self.index.get(&key).and_then(|&i| self.notes.get_mut(i)) {
            n.count = n.count.saturating_add(1);
        } else if self.notes.len() < MAX_NOTES {
            self.index.insert(key.clone(), self.notes.len());
            let (layer, kind, text) = key;
            self.notes.push(Note { layer, kind, text, count: 1 });
        }
    }

    pub fn approximated(&mut self, layer: &str, text: impl Into<String>) {
        self.note(layer, NoteKind::Approximated, text);
    }

    pub fn skipped(&mut self, layer: &str, text: impl Into<String>) {
        self.note(layer, NoteKind::Skipped, text);
    }
}

/// A layer's look as a QGIS style has it, in RustGIS's terms. Each part is `None` (or empty) when
/// the style doesn't set it, so applying it leaves the layer's own.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Style {
    pub renderer: Option<Renderer>,
    /// A line layer's stacked strokes (a single-symbol line with more than one line, or an
    /// offset one).
    pub line_strokes: Option<Vec<LineStroke>>,
    /// A polygon layer's stacked fills (a single-symbol fill with patterns, or several fills).
    pub fill_layers: Option<Vec<crate::fills::FillLayer>>,
    pub stroke_levels: Option<bool>,
    /// Point markers turned by a field (`Some(None)`: not turned).
    pub marker_rotation: Option<Option<MarkerRotation>>,
    /// Its data-defined size, colours, outline width and angle, as expressions (`Some` of an
    /// empty one: none).
    pub symbol_by: Option<crate::symbol_by::SymbolBy>,
    pub reference_scale: Option<f64>,
    pub labels: Option<Labeling>,
    /// 0 opaque to 100 invisible.
    pub transparency: Option<f32>,
    pub blend: Option<Blend>,
    /// A raster's colouring, stretch, resampling, brightness, contrast and mask.
    pub raster: Option<crate::raster::RasterStyle>,
    /// Drawn between these scales (RustGIS's, see [`QGIS_SCALE`]): zoomed in beyond 1:`min_scale`
    /// and out beyond 1:`max_scale`. `Some(None)` clears a limit.
    pub min_scale: Option<Option<f64>>,
    pub max_scale: Option<Option<f64>>,
    /// A rule-based renderer's rules classed by expression, in the order a feature meets them.
    /// Not a setting: what the importer checks the layer's features against (the features
    /// several rules pick).
    pub rules: Vec<RuleFilter>,
}

/// One rule of a rule-based renderer classed by expression: its class (`key`), the condition it
/// is drawn by (`when`: its filters, an else rule's siblings, its scale range), its filter alone
/// (its filters and its parents') and the scales it draws between (RustGIS's: out to 1:`out`, in
/// to 1:`inside`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RuleFilter {
    pub key: String,
    pub when: String,
    pub condition: String,
    pub out: Option<f64>,
    pub inside: Option<f64>,
}

/// A rule beside an else rule: its filter (none: every feature), the scales it draws between
/// (RustGIS's scales: drawn out to 1:`.0`, in to 1:`.1`), and for one that draws nothing itself,
/// the rules under it it draws through (`under`). The else rule takes what none of them draws.
pub(super) struct Sibling {
    pub(super) filter: Option<String>,
    pub(super) scales: (Option<f64>, Option<f64>),
    pub(super) under: Option<Vec<Sibling>>,
}

/// The siblings an else rule beside `rules` is the rest of (`None`: more than `budget` rules
/// under them, or nested more than 32 deep). As QGIS has it, a rule counts only where it draws
/// (`itself`: with a symbol or label settings) or where a rule under it does; a switched-off rule
/// never does. Else rules count among the rules under a sibling (`with_else`), each drawing where
/// it does.
pub(super) fn siblings<'a, 'i: 'a>(
    rules: impl Iterator<Item = Node<'a, 'i>>,
    parent: (Option<f64>, Option<f64>),
    itself: &impl Fn(Node) -> bool,
    with_else: bool,
    depth: usize,
    budget: &mut usize,
) -> Option<Vec<Sibling>> {
    let mut out = Vec::new();
    for r in rules.filter(|c| c.has_tag_name("rule") && c.attribute("active") != Some("0")) {
        if depth > 32 || *budget == 0 {
            return None;
        }
        *budget -= 1;
        let filter = r.attribute("filter").map(str::trim).filter(|f| !f.is_empty());
        let filter = match filter {
            Some(f) if f.eq_ignore_ascii_case("ELSE") && !with_else => continue,
            Some(f) if f.eq_ignore_ascii_case("ELSE") => None,
            f => f.map(str::to_string),
        };
        let scales = rule_scales(r, parent);
        let under = if itself(r) { None } else { Some(siblings(r.children(), scales, itself, true, depth + 1, budget)?) };
        // (One that draws nothing, nor anything under it, takes nothing from the else rule.)
        if under.as_ref().is_none_or(|u| !u.is_empty()) {
            out.push(Sibling { filter, scales, under });
        }
    }
    Some(out)
}

/// The scales a rule draws between (as [`Sibling::scales`]) inside a parent drawing between
/// `parent`: its scalemaxdenom is the out-beyond limit, its scalemindenom the in-beyond, and a
/// rule inside another is drawn only within both ranges.
pub(super) fn rule_scales(rule: Node, parent: (Option<f64>, Option<f64>)) -> (Option<f64>, Option<f64>) {
    let denom = |k: &str| {
        rule.attribute(k).and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite() && *v > 0.0).map(|v| v * QGIS_SCALE)
    };
    (
        match (denom("scalemaxdenom"), parent.0) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        },
        match (denom("scalemindenom"), parent.1) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        },
    )
}

/// `filter` with the conditions of the scale range `scales` (`@map_scale`; none, as in an export,
/// meets every range).
pub(super) fn within(filter: String, scales: (Option<f64>, Option<f64>)) -> String {
    let mut c = filter;
    if let Some(out) = scales.0 {
        c.push_str(&format!(" AND (@map_scale IS NULL OR @map_scale <= {})", out.round()));
    }
    if let Some(inside) = scales.1 {
        c.push_str(&format!(" AND (@map_scale IS NULL OR @map_scale >= {})", inside.round()));
    }
    c
}

/// What an else rule beside `siblings` takes: none of what they draw, at their scales. A filter
/// unknown (null) for a feature isn't met, as QGIS has it. `None` when a filter can't be read, or
/// the siblings weren't (see [`siblings`]).
pub(super) fn none_of(siblings: &Option<Vec<Sibling>>) -> Option<String> {
    let drawn_by = siblings.as_ref()?.iter().map(drawn_by).collect::<Option<Vec<String>>>()?;
    Some(if drawn_by.is_empty() { String::new() } else { format!("NOT coalesce({}, FALSE)", drawn_by.join(" OR ")) })
}

/// Where a sibling draws: its filter at its scales, and for one drawing through the rules under
/// it, where one of them does.
fn drawn_by(sib: &Sibling) -> Option<String> {
    let f = match &sib.filter {
        Some(f) => format!("({})", expr::to_sql(f)?),
        None => "TRUE".into(),
    };
    let mut c = within(f, sib.scales);
    if let Some(under) = &sib.under {
        let under = under.iter().map(drawn_by).collect::<Option<Vec<String>>>()?;
        c = format!("{c} AND ({})", under.join(" OR "));
    }
    Some(format!("({c})"))
}

/// Whether an expression reads the map's scale (`@map_scale`, `$scale`), outside quotes.
pub fn reads_scale(expr: &str) -> bool {
    expr::reads_scale(expr)
}

/// How many of the rules `roots` and those under them draw (`draws`): switched on, and under no
/// rule switched off. For the rules a reader leaves out.
pub(super) fn drawing_rules<'a, 'i: 'a>(roots: impl Iterator<Item = Node<'a, 'i>>, draws: impl Fn(Node) -> bool) -> usize {
    let on = |n: Node| n.attribute("active") != Some("0");
    roots
        .filter(|r| r.has_tag_name("rule"))
        .map(|r| {
            r.descendants()
                .filter(|d| d.has_tag_name("rule") && draws(*d))
                .filter(|d| d.ancestors().take_while(|a| *a != r).chain(std::iter::once(r)).all(|a| !a.has_tag_name("rule") || on(a)))
                .count()
        })
        .sum()
}

/// The expression that classes a feature by the first of `rules` it meets: `CASE WHEN <when>
/// THEN '<key>' … END`.
pub fn rules_expression(rules: &[RuleFilter]) -> String {
    let whens: Vec<String> = rules.iter().map(|r| format!("WHEN {} THEN '{}'", r.when, r.key.replace('\'', "''"))).collect();
    format!("CASE {} END", whens.join(" "))
}

impl Style {
    /// Gives `layer` this style's parts (those it sets).
    pub fn apply(&self, layer: &mut crate::Layer) {
        if let Some(t) = self.transparency {
            layer.transparency = t;
        }
        if let Some(b) = self.blend {
            layer.blend = b;
        }
        if let Some(s) = self.min_scale {
            layer.min_scale = s;
        }
        if let Some(s) = self.max_scale {
            layer.max_scale = s;
        }
        // A raster takes its raster style (when its bands are there), and none of a feature
        // layer's parts.
        if let Some(r) = layer.raster.as_mut() {
            if let Some(st) = self.raster.as_ref().filter(|_| self.raster_misfit(r).is_none()) {
                r.style = st.clone();
            }
            return;
        }
        if let Some(r) = &self.renderer {
            layer.renderer = r.clone();
        }
        if let Some(s) = &self.line_strokes {
            layer.line_strokes = s.clone();
        }
        // (Only a polygon layer's fills draw.)
        if let Some(f) = self.fill_layers.as_ref().filter(|_| layer.geometry_type() == rustgis_data::GeometryType::Polygon) {
            layer.fill_layers = f.clone();
        }
        if let Some(on) = self.stroke_levels {
            layer.stroke_levels = on;
        }
        // Only a point layer's markers turn.
        if let Some(r) = &self.marker_rotation {
            layer.marker_rotation = r.clone().filter(|_| layer.geometry_type() == rustgis_data::GeometryType::Point);
        }
        if let Some(by) = &self.symbol_by {
            layer.symbol_by = by.clone();
            // Only a point layer's markers turn.
            if layer.geometry_type() != rustgis_data::GeometryType::Point {
                layer.symbol_by.angle = None;
            }
        }
        if let Some(r) = self.reference_scale {
            layer.reference_scale = Some(r);
        }
        if let Some(l) = &self.labels {
            layer.labels = l.clone();
        }
    }

    /// Why this style's raster colouring doesn't fit the raster `r` (it reads a band the file
    /// hasn't), or `None` when it fits or there is none.
    pub fn raster_misfit(&self, r: &crate::raster::RasterLayer) -> Option<String> {
        let st = self.raster.as_ref()?;
        let count = r.data.as_ref()?.band_count();
        let band = st.renderer.bands().into_iter().find(|b| *b >= count)?;
        Some(format!("its raster colouring, which reads band {} of a raster with {count}", band + 1))
    }
}

/// A QGIS project's layers and view, read from its `.qgs` text.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Project {
    pub title: String,
    /// The project's coordinate system (`EPSG:2056`, or a PROJ string), the map's projection.
    pub crs: Option<String>,
    /// The view when it was saved: [xmin, ymin, xmax, ymax] in the project's coordinate system.
    pub extent: Option<[f64; 4]>,
    /// Its layers, top first, as its layer tree orders them.
    pub layers: Vec<ProjectLayer>,
}

/// One layer of a QGIS project.
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectLayer {
    pub id: String,
    pub name: String,
    /// QGIS's data provider (`ogr`, `gdal`, `wms`, `delimitedtext`, …).
    pub provider: String,
    pub source: Source,
    /// Its coordinate system (`EPSG:…`), when QGIS records one.
    pub crs: Option<String>,
    /// QGIS's kind of layer (`vector`, `raster`, `vector-tile`, `annotation`, `mesh`, …).
    pub kind: String,
    /// A vector layer (`false`: raster, mesh, point cloud and the like).
    pub vector: bool,
    /// Checked in the layer tree, and every group holding it too.
    pub visible: bool,
    /// Its subset string, as a RustGIS definition query when it reads as one.
    pub definition_query: Option<String>,
    pub style: Style,
}

/// Reads a QGIS project. `dir` is the folder the project is in, which its relative paths start
/// from. Notes on what can't be carried over go to `report`.
pub fn read_project(text: &str, dir: &str, report: &mut Report) -> Result<Project, String> {
    let doc = parse(text, "a QGIS project")?;
    let root = doc.root_element();
    if !root.has_tag_name("qgis") {
        return Err("not a QGIS project (.qgs): its root element isn't <qgis>".into());
    }
    // The main map canvas: its view, and (before QGIS 3) the project's coordinate system.
    let canvas = root.children().find(|c| c.has_tag_name("mapcanvas") && c.attribute("name").is_none_or(|n| n == "theMapCanvas"));
    let crs = child(root, "projectCrs")
        .and_then(crs_of)
        .or_else(|| canvas.and_then(|c| child(c, "destinationsrs")).and_then(crs_of))
        .or_else(|| {
            let p = child(root, "properties").and_then(|p| child(p, "SpatialRefSys")).and_then(|s| child(s, "ProjectCrs"))?;
            p.text().map(str::trim).filter(|t| t.contains(':')).map(str::to_string)
        });
    let mut project = Project {
        title: child(root, "title").and_then(|t| t.text()).map(str::trim).unwrap_or_default().to_string(),
        crs,
        extent: canvas.and_then(|c| child(c, "extent")).and_then(extent_of),
        layers: Vec::new(),
    };
    // The layer tree (QGIS 2's legend before it): the order layers draw in (top first) and which
    // are checked.
    let mut order: Vec<(String, bool)> = Vec::new();
    if let Some(t) = child(root, "layer-tree-group") {
        tree_layers(t, true, &mut order, 0);
    } else if let Some(l) = child(root, "legend") {
        legend_layers(l, true, &mut order, 0);
    }
    project_notes(root, report);
    let maplayers: Vec<Node> =
        child(root, "projectlayers").map(|p| p.children().filter(|c| c.has_tag_name("maplayer")).collect()).unwrap_or_default();
    if maplayers.len() > MAX_LAYERS {
        report.skipped("", format!("layers past the first {MAX_LAYERS}"));
    }
    let mut layers: Vec<(Option<usize>, ProjectLayer)> = Vec::new();
    for ml in maplayers.into_iter().take(MAX_LAYERS) {
        let id = child(ml, "id").and_then(|n| n.text()).unwrap_or_default().trim().to_string();
        let name = child(ml, "layername").and_then(|n| n.text()).unwrap_or_default().trim().to_string();
        let name = if name.is_empty() { id.clone() } else { name };
        let at = order.iter().position(|(i, _)| *i == id);
        let checked = at.and_then(|i| order.get(i)).is_none_or(|(_, c)| *c);
        let provider = child(ml, "provider").and_then(|n| n.text()).unwrap_or_default().trim().to_string();
        let datasource = child(ml, "datasource").and_then(|n| n.text()).unwrap_or_default().trim().to_string();
        let source = source::read(&provider, &datasource, dir);
        let kind = ml.attribute("type").unwrap_or_default().to_string();
        let vector = kind == "vector";
        let definition_query = match &source {
            Source::File { subset: Some(q), .. } => match expr::to_sql(q) {
                // A definition query is read with no map scale, so one reading it would take no
                // feature.
                Some(_) if expr::reads_scale(q) => {
                    report.skipped(&name, "its filter (subset string), which reads the map's scale, which a filter has none of");
                    None
                }
                Some(sql) => Some(sql),
                None => {
                    report.skipped(&name, "its filter (subset string), which is too long to read");
                    None
                }
            },
            _ => None,
        };
        let style = read_style_node(ml, &name, report);
        layers.push((
            at,
            ProjectLayer {
                id,
                name,
                provider,
                source,
                crs: child(ml, "srs").and_then(crs_of),
                kind,
                vector,
                visible: checked,
                definition_query,
                style,
            },
        ));
    }
    // In the layer tree's order; layers it doesn't hold go below.
    layers.sort_by_key(|(at, _)| at.unwrap_or(usize::MAX));
    project.layers = layers.into_iter().map(|(_, l)| l).collect();
    Ok(project)
}

/// Reads a QGIS layer style (a `.qml` file's text) for the layer named `layer` (for the notes).
pub fn read_style(text: &str, layer: &str, report: &mut Report) -> Result<Style, String> {
    let doc = parse(text, "a QGIS layer style")?;
    let root = doc.root_element();
    if !root.has_tag_name("qgis") {
        return Err("not a QGIS layer style (.qml): its root element isn't <qgis>".into());
    }
    Ok(read_style_node(root, layer, report))
}

/// A layer's style from its `<maplayer>` (a project's) or a style file's `<qgis>` root.
fn read_style_node(node: Node, layer: &str, report: &mut Report) -> Style {
    let mut style = Style::default();
    let number = |tag: &str| child(node, tag).and_then(|n| n.text()).and_then(|t| t.trim().parse::<f32>().ok()).filter(|v| v.is_finite());
    // Its opacity: 0 to 1; QGIS 2's transparency 0 to 100; QGIS 1's opacity 0 to 255.
    let opacity = number("layerOpacity")
        .or_else(|| number("layerTransparency").map(|t| 1.0 - t / 100.0))
        .or_else(|| number("transparencyLevelInt").map(|o| o / 255.0));
    if let Some(o) = opacity {
        style.transparency = Some(((1.0 - o.clamp(0.0, 1.0)) * 100.0).round());
    }
    if let Some(mode) = child(node, "blendMode").and_then(|n| n.text()).and_then(|t| t.trim().parse::<i32>().ok()) {
        style.blend = Some(blend_of(mode).unwrap_or_else(|| {
            report.approximated(layer, "its blending mode, drawn as Normal");
            Blend::Normal
        }));
    }
    if node.attribute("hasScaleBasedVisibilityFlag") == Some("1") {
        // QGIS's minScale is the out-beyond limit (the larger number), maxScale the in-beyond;
        // before QGIS 3 they were maximumScale and minimumScale.
        let scale = |k: &str| node.attribute(k).and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite() && *v > 0.0);
        let (out, inside) = if node.attribute("minScale").is_some() || node.attribute("maxScale").is_some() {
            (scale("minScale"), scale("maxScale"))
        } else {
            (scale("maximumScale"), scale("minimumScale"))
        };
        style.min_scale = Some(out.map(|s| s * QGIS_SCALE));
        style.max_scale = Some(inside.map(|s| s * QGIS_SCALE));
    }
    layer_notes(node, layer, report);
    if let Some(r) = child(node, "renderer-v2") {
        renderer::read(r, layer, &mut style, report);
    }
    // A raster's renderer: its opacity, and its colouring as RustGIS's raster style.
    if let Some(r) = child(node, "pipe").and_then(|p| child(p, "rasterrenderer")) {
        if let Some(o) = r.attribute("opacity").and_then(|o| o.trim().parse::<f32>().ok()).filter(|o| o.is_finite() && *o < 1.0) {
            style.transparency = Some(((1.0 - o.clamp(0.0, 1.0)) * 100.0).round());
        }
        style.raster = raster::read(node, layer, report);
    }
    if let Some(l) = child(node, "labeling") {
        // Set up but switched off: carried over, switched off.
        let off = node.attribute("labelsEnabled") == Some("0");
        style.labels = labels::read(l, node.attribute("geometry"), layer, report).map(|mut l| {
            l.enabled &= !off;
            l
        });
    } else if old_labels(node) {
        report.skipped(layer, "its labels, in the format of QGIS 2 and before");
    }
    style
}

/// Whether a layer has labels switched on in QGIS 2's way (its custom properties) or QGIS 1's
/// (`<label>1</label>`).
fn old_labels(node: Node) -> bool {
    let custom = child(node, "customproperties").is_some_and(|c| {
        c.children()
            .any(|p| p.has_tag_name("property") && p.attribute("key") == Some("labeling/enabled") && p.attribute("value") == Some("true"))
    });
    custom || child(node, "label").and_then(|n| n.text()).is_some_and(|t| t.trim() == "1")
}

/// Notes a layer's settings RustGIS has no place for (a project's `<maplayer>` or a style's root).
fn layer_notes(node: Node, layer: &str, report: &mut Report) {
    if child(node, "temporal").is_some_and(|t| t.attribute("enabled") == Some("1")) {
        report.skipped(layer, "its time settings");
    }
    let diagrams = node
        .children()
        .filter(|c| c.tag_name().name().ends_with("DiagramRenderer"))
        .any(|r| r.descendants().any(|d| d.has_tag_name("DiagramCategory") && d.attribute("enabled") == Some("1")));
    if diagrams {
        report.skipped(layer, "its diagrams");
    }
    if let Some(m) = child(node, "map-layer-style-manager") {
        let current = m.attribute("current").unwrap_or_default();
        let others: Vec<&str> = m
            .children()
            .filter(|s| s.has_tag_name("map-layer-style"))
            .filter_map(|s| s.attribute("name"))
            .filter(|n| *n != current)
            .take(8)
            .collect();
        if !others.is_empty() {
            report.skipped(layer, format!("its other styles ({}): only the current one is carried over", others.join(", ")));
        }
    }
    let count = |list: &str, item: &str| child(node, list).map_or(0, |l| l.children().filter(|c| c.has_tag_name(item)).count());
    if count("vectorjoins", "join") > 0 {
        report.skipped(layer, "its joins to other layers' fields");
    }
    if count("expressionfields", "field") > 0 {
        report.skipped(layer, "its virtual fields (fields computed from an expression)");
    }
    if node.attribute("type") == Some("vector-tile") && child(node, "renderer").is_some() {
        report.approximated(layer, "its vector tile style, drawn in RustGIS's own");
    }
}

/// Notes the project's parts RustGIS has no place for: print layouts, annotations, bookmarks,
/// map themes and relations.
fn project_notes(root: Node, report: &mut Report) {
    let count = |list: &str, item: &str| child(root, list).map_or(0, |l| l.children().filter(|c| c.has_tag_name(item)).count());
    let layouts = count("Layouts", "Layout") + root.children().filter(|c| c.has_tag_name("Composer")).count();
    if layouts > 0 {
        report.skipped("", format!("its print layouts ({layouts})"));
    }
    let items = child(root, "main-annotation-layer")
        .and_then(|a| child(a, "items"))
        .map_or(0, |i| i.children().filter(|c| c.has_tag_name("item")).count());
    if items > 0 {
        report.skipped("", format!("its annotations ({items}: text, shapes and pictures on the map)"));
    }
    let bookmarks = count("Bookmarks", "Bookmark");
    if bookmarks > 0 {
        report.skipped("", format!("its bookmarks ({bookmarks})"));
    }
    let themes = count("visibility-presets", "visibility-preset");
    if themes > 0 {
        report.skipped("", format!("its map themes ({themes})"));
    }
    let relations = count("relations", "relation");
    if relations > 0 {
        report.skipped("", format!("its relations between layers ({relations})"));
    }
}

/// Whether `node` (a symbol layer, a renderer) has a draw effect on: a shadow, glow, blur or the
/// like, not only the plain drawing QGIS's effect stack starts with.
pub(crate) fn has_effect(node: Node) -> bool {
    let Some(top) = node.children().find(|c| c.has_tag_name("effect")) else { return false };
    if top.attribute("enabled") != Some("1") {
        return false;
    }
    top.descendants().filter(|e| e.has_tag_name("effect")).any(|e| {
        let kind = e.attribute("type").unwrap_or_default();
        kind != "effectStack" && kind != "drawSource" && prop(&props(e), "enabled").is_none_or(|on| on == "1")
    })
}

/// Whether `node` (a symbol layer, label settings) has a data-defined setting switched on: one
/// taken from a field or an expression.
pub(crate) fn data_defined_on(node: Node) -> bool {
    node.children().filter(|c| c.has_tag_name("data_defined_properties") || c.has_tag_name("dd_properties")).any(|dd| {
        dd.descendants().any(|o| o.has_tag_name("Option") && o.attribute("name") == Some("active") && o.attribute("value") == Some("true"))
    })
}

/// The layers in a layer tree group, top first, each with whether it is checked (and every
/// group above it). Groups are walked to a bounded depth.
fn tree_layers(group: Node, checked: bool, out: &mut Vec<(String, bool)>, depth: usize) {
    if depth > 64 || out.len() > MAX_LAYERS * 4 {
        return;
    }
    let on = |n: Node| n.attribute("checked").is_none_or(|c| c == "Qt::Checked" || c == "Qt::PartiallyChecked");
    for c in group.children().filter(Node::is_element) {
        if c.has_tag_name("layer-tree-layer") {
            if let Some(id) = c.attribute("id") {
                out.push((id.to_string(), checked && on(c)));
            }
        } else if c.has_tag_name("layer-tree-group") {
            tree_layers(c, checked && on(c), out, depth + 1);
        }
    }
}

/// The layers in QGIS 2's legend (`<legend>`, before the layer tree), top first, each with
/// whether it is checked (and its group). Groups are walked to a bounded depth.
fn legend_layers(group: Node, checked: bool, out: &mut Vec<(String, bool)>, depth: usize) {
    if depth > 64 || out.len() > MAX_LAYERS * 4 {
        return;
    }
    let on = |n: Node| n.attribute("checked").is_none_or(|c| c == "Qt::Checked" || c == "Qt::PartiallyChecked");
    for c in group.children().filter(Node::is_element) {
        if c.has_tag_name("legendlayer") {
            for f in c.descendants().filter(|f| f.has_tag_name("legendlayerfile")) {
                if let Some(id) = f.attribute("layerid") {
                    out.push((id.to_string(), checked && on(c)));
                }
            }
        } else if c.has_tag_name("legendgroup") {
            legend_layers(c, checked && on(c), out, depth + 1);
        }
    }
}

/// QGIS's blending modes (`QPainter::CompositionMode`) RustGIS has.
fn blend_of(mode: i32) -> Option<Blend> {
    Some(match mode {
        0 => Blend::Normal,
        13 => Blend::Multiply,
        14 => Blend::Screen,
        15 => Blend::Overlay,
        16 => Blend::Darken,
        17 => Blend::Lighten,
        22 => Blend::Difference,
        _ => return None,
    })
}

/// A coordinate system element's (`<srs>`, `<projectCrs>`) `EPSG:…`, or its PROJ string.
fn crs_of(node: Node) -> Option<String> {
    let srs = node.descendants().find(|d| d.has_tag_name("spatialrefsys"))?;
    let text = |tag: &str| child(srs, tag).and_then(|n| n.text()).map(str::trim).filter(|t| !t.is_empty());
    text("authid").filter(|a| a.contains(':')).or_else(|| text("proj4")).map(str::to_string)
}

/// An `<extent>`'s [xmin, ymin, xmax, ymax], when all four are numbers and it isn't empty.
fn extent_of(node: Node) -> Option<[f64; 4]> {
    let v = |tag: &str| child(node, tag).and_then(|n| n.text()).and_then(|t| t.trim().parse::<f64>().ok()).filter(|v| v.is_finite());
    let e = [v("xmin")?, v("ymin")?, v("xmax")?, v("ymax")?];
    (e[0] < e[2] && e[1] < e[3]).then_some(e)
}

/// Parses QGIS's XML with a node limit. QGIS starts its files with `<!DOCTYPE qgis …>`, which
/// declares nothing: the document is read from past it, and one declaring entities (`[…]`) is
/// refused rather than expanded.
fn parse<'a>(text: &'a str, what: &str) -> Result<Document<'a>, String> {
    let opts = ParsingOptions { allow_dtd: false, nodes_limit: MAX_NODES, ..ParsingOptions::default() };
    let doctype = text.find("<!DOCTYPE").filter(|at| text.get(..*at).is_some_and(|before| !before.contains("<q")));
    let Some(at) = doctype else {
        return Document::parse_with_options(text, opts).map_err(|e| format!("not {what}: {e}"));
    };
    let end = text.get(at..).and_then(|t| t.find('>')).map(|e| at + e + 1).ok_or_else(|| format!("not {what}: an unclosed <!DOCTYPE"))?;
    if text.get(at..end).is_some_and(|d| d.contains('[')) {
        return Err(format!("not {what}: it declares entities (a DTD), which aren't read"));
    }
    let rest = text.get(end..).unwrap_or_default();
    Document::parse_with_options(rest, opts).map_err(|e| format!("not {what}: {e}"))
}

/// The first child element named `tag`.
fn child<'a, 'i>(node: Node<'a, 'i>, tag: &str) -> Option<Node<'a, 'i>> {
    node.children().find(|c| c.has_tag_name(tag))
}

/// A symbol layer's (or ramp's) settings, by name: QGIS 3.26's `<Option type="Map">` and the
/// `<prop k v>` before it.
pub(crate) fn props(node: Node) -> Vec<(String, String)> {
    let mut props = Vec::new();
    for c in node.children().filter(Node::is_element) {
        if c.has_tag_name("prop")
            && let (Some(k), Some(v)) = (c.attribute("k"), c.attribute("v"))
        {
            props.push((k.to_string(), v.to_string()));
        }
        if c.has_tag_name("Option") {
            for o in c.children().filter(|o| o.has_tag_name("Option")) {
                if let (Some(k), Some(v)) = (o.attribute("name"), o.attribute("value")) {
                    props.push((k.to_string(), v.to_string()));
                }
            }
        }
    }
    props
}

/// The setting `k` of `props`.
fn prop<'a>(props: &'a [(String, String)], k: &str) -> Option<&'a str> {
    props.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
}

/// A QGIS colour: `r,g,b[,a]` (newer files add `,rgb:…` after), or `#rrggbb[aa]`.
pub(crate) fn color(s: &str) -> Option<Rgba> {
    let s = s.trim();
    if s.starts_with('#') {
        return Rgba::from_hex(s);
    }
    let v: Vec<u8> = s.split(',').take(4).map(|p| p.trim().parse::<u8>().ok()).collect::<Option<_>>()?;
    match v.as_slice() {
        [r, g, b] => Some(Rgba::rgb(*r, *g, *b)),
        [r, g, b, a] => Some(Rgba::new(*r, *g, *b, *a)),
        _ => None,
    }
}

/// A size in QGIS's `unit` as RustGIS points (1/96 inch, a screen pixel at 1×). Map units have
/// no fixed size on screen: they are taken at the layer's reference scale when it has one
/// (`reference`, RustGIS's), else as metres at 1:25,000, and noted.
fn points(v: f64, unit: Option<&str>, reference: Option<f64>, layer: &str, report: &mut Report) -> f32 {
    let pt = in_points(v, unit, reference, layer, report);
    if pt.is_finite() { pt.clamp(0.0, 1000.0) as f32 } else { 0.0 }
}

/// [`points`] for a distance that may be negative, such as an offset to the left.
fn signed_points(v: f64, unit: Option<&str>, reference: Option<f64>, layer: &str, report: &mut Report) -> f32 {
    let pt = in_points(v, unit, reference, layer, report);
    if pt.is_finite() { pt.clamp(-1000.0, 1000.0) as f32 } else { 0.0 }
}

fn in_points(v: f64, unit: Option<&str>, reference: Option<f64>, layer: &str, report: &mut Report) -> f64 {
    match unit.unwrap_or("MM") {
        "MM" => v * 96.0 / 25.4,
        "Point" => v * 96.0 / 72.0,
        "Pixel" => v,
        "Inch" => v * 96.0,
        "MapUnit" | "RenderMetersInMapUnits" => {
            report.approximated(layer, "sizes in map units, taken at a fixed scale");
            v / reference.unwrap_or(25_000.0) * 96.0 / 0.0254
        }
        other => {
            report.approximated(layer, format!("sizes in {other}, taken as millimetres"));
            v * 96.0 / 25.4
        }
    }
}
