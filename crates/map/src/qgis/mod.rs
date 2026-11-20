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

mod annotation;
mod diagram;
mod expr;
mod labels;
mod raster;
mod renderer;
mod source;
mod structure;
mod symbol;
#[cfg(test)]
mod tests;

use std::collections::HashMap;

use roxmltree::{Document, Node, ParsingOptions};
use serde::Serialize;

use crate::Rgba;
use crate::effects::Blend;
use crate::symbology::{Labeling, LineStroke, MarkerRotation, Renderer};
pub use annotation::{Annotation, MAX_ITEMS as MAX_ANNOTATIONS};
pub use expr::{reads_measure, to_sql};
pub use source::{MAX_MEMORY_FIELDS, Source, VirtualFrom, WebOptions, is_drive_relative, is_relative};
pub use structure::{MAX_BOOKMARKS, MAX_RELATIONS, ProjectBookmark, ProjectRelation};

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

    /// Withdraws a provisional note once checking against the destination fields disproves it.
    fn forget(&mut self, layer: &str, kind: NoteKind, text: &str) {
        let key = (layer.to_string(), kind, text.to_string());
        let Some(&at) = self.index.get(&key) else { return };
        if let Some(note) = self.notes.get_mut(at)
            && note.count > 1
        {
            note.count -= 1;
            return;
        }
        self.notes.retain(|n| n.layer != layer || n.kind != kind || n.text != text);
        self.index = self.notes.iter().enumerate().map(|(i, n)| ((n.layer.clone(), n.kind, n.text.clone()), i)).collect();
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
    /// Its diagram: a pie or bar chart on each feature.
    pub diagram: Option<crate::DiagramSettings>,
    /// The scaled diagram's original classification, for checking once the destination fields
    /// are known. Import metadata only, not a saved diagram setting.
    pub diagram_by: Option<String>,
    /// 0 opaque to 100 invisible.
    pub transparency: Option<f32>,
    /// The field whose dates the time slider reads (QGIS's time settings). Not applied by
    /// [`Style::apply`]: the importer binds the layer once its fields are checked.
    pub time: Option<String>,
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
    /// A categorized renderer classed by expression: the expression as QGIS wrote it, and what
    /// the layer is drawn with where it doesn't read on the layer's fields. Not a setting.
    pub categories_by: Option<CategoriesBy>,
    /// A categorized renderer's classes from boolean categories (`type="bool"`), by their keys:
    /// matched to the layer's field where it is added (a boolean field prints `True`, text reads
    /// `true` as it is). Not a setting.
    pub bool_classes: Vec<String>,
}

/// A categorized renderer by an expression, for where the expression doesn't read on the layer
/// (checked where the layer is added, as rules and filters are, not by [`Style::apply`]): every
/// feature is drawn with its first switched-on category's symbol, strokes and fills (`first`;
/// none: nothing drawn), noted with the expression as QGIS wrote it (`qgis`).
#[derive(Clone, Debug, PartialEq)]
pub struct CategoriesBy {
    pub qgis: String,
    pub first: Option<(crate::symbology::Symbol, Vec<LineStroke>, Vec<crate::fills::FillLayer>)>,
}

/// Renames the Unique Values class `from` of `r` to `to`, with its label, strokes and fills;
/// `false` (nothing renamed) when there is no such class or another class is already `to`.
pub fn rename_class(r: &mut Renderer, from: &str, to: &str) -> bool {
    let Renderer::Unique { classes, class_labels, class_strokes, class_fills, .. } = r else { return false };
    if from == to {
        return true;
    }
    if classes.iter().any(|c| c.0 == to) {
        return false;
    }
    let Some(c) = classes.iter_mut().find(|c| c.0 == from) else { return false };
    c.0 = to.to_string();
    if let Some(v) = class_labels.remove(from) {
        class_labels.insert(to.to_string(), v);
    }
    if let Some(v) = class_strokes.remove(from) {
        class_strokes.insert(to.to_string(), v);
    }
    if let Some(v) = class_fills.remove(from) {
        class_fills.insert(to.to_string(), v);
    }
    true
}

/// A boolean category's value as RustGIS prints a boolean (`True`, `False`) when `printed`, else
/// as QGIS writes it (`true`, `false`); `None` for any other value.
pub fn bool_key(v: &str, printed: bool) -> Option<&'static str> {
    match (v.eq_ignore_ascii_case("true"), v.eq_ignore_ascii_case("false"), printed) {
        (true, _, true) => Some("True"),
        (true, _, false) => Some("true"),
        (_, true, true) => Some("False"),
        (_, true, false) => Some("false"),
        _ => None,
    }
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
    /// Checks an imported diagram against the destination's fields. Its size classification
    /// is compared with the categories actually retained, so provisional import notes agree.
    pub fn check_diagram_fields(&self, layer: &mut crate::Layer, name: &str, report: &mut Report) {
        diagram::check_fields(layer, name, self.diagram_by.as_deref(), report);
    }

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
        // (A graphics layer takes no diagram.)
        if let Some(d) = self.diagram.as_ref().filter(|_| !layer.graphics) {
            layer.diagram = Some(d.clone());
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
    /// Its main annotation layer, drawn over its layers, when it holds any annotation.
    pub main_annotations: Option<ProjectLayer>,
    /// The groups of its layer tree (of QGIS 2's legend before it), each before the groups in it.
    pub groups: Vec<ProjectGroup>,
    /// The map's rotation when it was saved, in degrees: QGIS turns the map this far clockwise.
    pub rotation: f64,
    /// Its spatial bookmarks, in the order QGIS lists them.
    pub bookmarks: Vec<ProjectBookmark>,
    /// Its relations between layers.
    pub relations: Vec<ProjectRelation>,
}

/// A group of a QGIS project's layer tree.
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectGroup {
    pub name: String,
    /// Its own box checked in the layer tree (the groups holding it apply on top).
    pub checked: bool,
    /// Open in the layer tree.
    pub expanded: bool,
    /// The group it is in, as an index into [`Project::groups`]: `None` at the top level.
    pub parent: Option<usize>,
    /// 0 = opaque, 100 = invisible: the opacity of a QGIS group layer (a group drawn as one).
    pub transparency: Option<f32>,
    /// A group layer's visibility range (RustGIS's scales, see [`QGIS_SCALE`]): its layers draw
    /// only when zoomed in beyond 1:`min_scale` and out beyond 1:`max_scale`. QGIS 4.2 doesn't
    /// save one for a group layer (measured), but a file may have one.
    pub min_scale: Option<f64>,
    pub max_scale: Option<f64>,
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
    /// The group it is in, as an index into [`Project::groups`]: `None` at the top level.
    pub group: Option<usize>,
    /// Its own box checked, and those of any groups between it and `group` that weren't kept
    /// (nested too deep): what it takes inside its group, which applies on top.
    pub checked: bool,
    /// Its subset string, as a RustGIS definition query when it reads as one.
    pub definition_query: Option<String>,
    pub style: Style,
    /// An annotation layer's annotations, bottom first, in its coordinates (`crs`, else the
    /// project's).
    pub annotations: Vec<Annotation>,
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
        main_annotations: None,
        groups: Vec::new(),
        rotation: 0.0,
        bookmarks: Vec::new(),
        relations: Vec::new(),
    };
    // The layer tree (QGIS 2's legend before it): the order layers draw in (top first), which
    // are checked, and their groups.
    let mut tree = Tree::read(root);
    project_notes(root, report);
    // The map's rotation: one that doesn't read faces north, noted.
    if let Some(r) = canvas.and_then(|c| child(c, "rotation")).and_then(|r| r.text()).map(str::trim).filter(|r| !r.is_empty()) {
        match r.parse::<f64>().ok().filter(|r| r.is_finite()) {
            Some(r) => project.rotation = r,
            None => {
                let r: String = r.chars().take(40).collect();
                report.approximated("", format!("the map's rotation `{r}`, which doesn't read: facing north"));
            }
        }
    }
    project.bookmarks = structure::bookmarks(root, report);
    project.relations = structure::relations(root, report);
    structure::project_time(root, report);
    let maplayers: Vec<Node> =
        child(root, "projectlayers").map(|p| p.children().filter(|c| c.has_tag_name("maplayer")).collect()).unwrap_or_default();
    if maplayers.len() > MAX_LAYERS {
        report.skipped("", format!("layers past the first {MAX_LAYERS}"));
    }
    let mut layers: Vec<(Option<usize>, ProjectLayer)> = Vec::new();
    // QGIS group layers (a group drawn as one) whose blending mode RustGIS's groups don't have,
    // and those with a draw effect.
    let (mut blended, mut effects) = (Vec::new(), Vec::new());
    // Groups drawn as one past RustGIS's limits whose opacity, blending or effect is left out.
    let mut unkept = Vec::new();
    for ml in maplayers.into_iter().take(MAX_LAYERS) {
        let id = child(ml, "id").and_then(|n| n.text()).unwrap_or_default().trim().to_string();
        let name = child(ml, "layername").and_then(|n| n.text()).unwrap_or_default().trim().to_string();
        let name = if name.is_empty() { id.clone() } else { name };
        // A group layer is its group's opacity and blending, not a layer of its own.
        if ml.attribute("type") == Some("group")
            && let Some(g) = tree.groups.iter_mut().find(|g| g.layer.as_deref() == Some(id.as_str()))
        {
            // Its notes (a blending mode RustGIS hasn't, say) go under its name.
            let style = read_style_node(ml, &name, report);
            g.group.transparency = style.transparency.filter(|t| *t > 0.0);
            g.group.min_scale = style.min_scale.flatten();
            g.group.max_scale = style.max_scale.flatten();
            if style.blend.is_some_and(|b| b != Blend::Normal) {
                blended.push(g.group.name.clone());
            }
            if child(ml, "paintEffect").is_some_and(has_effect) {
                effects.push(g.group.name.clone());
            }
            continue;
        }
        // A group layer of a group not kept: its layers went in the group holding it, so what it
        // gives them is left out, when it gives them anything.
        // Each id once (removed as it's taken), so a repeated one isn't counted twice; its own
        // notes aren't given, as it draws nothing of its own.
        if ml.attribute("type") == Some("group") && tree.folded_layers.remove(&id) {
            let style = read_style_node(ml, &name, &mut Report::default());
            let blended = child(ml, "blendMode").and_then(|n| n.text()).and_then(|t| t.trim().parse::<i32>().ok()).is_some_and(|m| m != 0);
            let effect = child(ml, "paintEffect").is_some_and(has_effect);
            let ranged = style.min_scale.flatten().is_some() || style.max_scale.flatten().is_some();
            let parts: Vec<&str> = [
                (style.transparency.is_some_and(|t| t > 0.0), "opacity"),
                (blended, "blending"),
                (effect, "draw effect"),
                (ranged, "visibility range"),
            ]
            .into_iter()
            .filter(|(on, _)| *on)
            .map(|(_, part)| part)
            .collect();
            if !parts.is_empty() {
                unkept.push(format!("{name} ({})", parts.join(", ")));
            }
            continue;
        }
        let at = tree.layers.iter().position(|l| l.id == id);
        let place = at.and_then(|i| tree.layers.get(i));
        let (visible, checked, group) = place.map_or((true, true, None), |l| (l.visible, l.checked, l.group));
        let provider = child(ml, "provider").and_then(|n| n.text()).unwrap_or_default().trim().to_string();
        let datasource = child(ml, "datasource").and_then(|n| n.text()).unwrap_or_default().trim().to_string();
        let source = source::read(&provider, &datasource, dir);
        let kind = ml.attribute("type").unwrap_or_default().to_string();
        let vector = kind == "vector";
        let definition_query = definition_query(&source, &name, "subset string", report);
        let style = read_style_node(ml, &name, report);
        let annotations = if kind == "annotation" { annotation::read(ml, dir, &name, report) } else { Vec::new() };
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
                visible,
                group,
                checked,
                definition_query,
                style,
                annotations,
            },
        ));
    }
    project.main_annotations = child(root, "main-annotation-layer").and_then(|m| main_annotations(m, dir, report));
    // In the layer tree's order; layers it doesn't hold go below.
    layers.sort_by_key(|(at, _)| at.unwrap_or(usize::MAX));
    project.layers = layers.into_iter().map(|(_, l)| l).collect();
    resolve_virtual_layers(&mut project.layers, report);
    group_notes(&tree, &blended, &effects, report);
    if !unkept.is_empty() {
        let names = unkept.iter().take(8).map(String::as_str).collect::<Vec<_>>().join(", ");
        let more = if unkept.len() > 8 { ", …" } else { "" };
        report.skipped(
            "",
            format!("what groups drawn as one past RustGIS's group limits give their layers ({}: {names}{more})", unkept.len()),
        );
    }
    project.groups = tree.groups.into_iter().map(|g| g.group).collect();
    Ok(project)
}

/// A layer's filter as a RustGIS definition query, when it reads as one. `what` says where it
/// came from in the notes ("subset string", or a virtual layer's query).
fn definition_query(source: &Source, name: &str, what: &str, report: &mut Report) -> Option<String> {
    let Source::File { subset: Some(q), .. } = source else { return None };
    match expr::to_sql(q) {
        // A definition query is read with no map scale, so one reading it would take no feature.
        Some(_) if expr::reads_scale(q) => {
            report.skipped(name, format!("its filter ({what}), which reads the map's scale, which a filter has none of"));
            None
        }
        Some(sql) => Some(sql),
        None => {
            report.skipped(name, format!("its filter ({what}), which is too long to read"));
            None
        }
    }
}

/// Virtual layers that select every column of one layer, with or without a `WHERE` clause, read
/// as that layer's file with the clause as its filter (with the layer's own filter too, when it
/// is another of the project's layers). A virtual layer on another such one is read once that
/// one is, whatever their order, up to [`MAX_VIRTUAL_CHAIN`] deep. The rest are left as they are,
/// for the import to note.
fn resolve_virtual_layers(layers: &mut [ProjectLayer], report: &mut Report) {
    for _ in 0..MAX_VIRTUAL_CHAIN {
        let mut changed = false;
        for i in 0..layers.len() {
            let Some(Source::Virtual { from: Some(from), filter, .. }) = layers.get(i).map(|l| &l.source) else { continue };
            let max = rustgis_data::query::MAX_LEN;
            // The layer it selects from, as it reads (its file, with its own filter), copied
            // without a filter longer than any RustGIS reads (`true`: one was left behind).
            let base = match from {
                source::VirtualFrom::Source(s) => Some(&**s),
                source::VirtualFrom::Layer(id) => layers.iter().find(|l| &l.id == id).map(|l| &l.source),
                source::VirtualFrom::Missing(_) => None,
            };
            let Some(Source::File { path, layer, subset, ignored }) = base else { continue };
            // A project layer on a file of several named without one: as a source naming none.
            if layer.is_none() && source::holds_layers(path) {
                let why = source::no_layer_named(path);
                if let Some(Source::Virtual { from, filter, .. }) = layers.get_mut(i).map(|l| &mut l.source) {
                    *from = Some(source::VirtualFrom::Missing(why));
                    *filter = None;
                }
                changed = true;
                continue;
            }
            let (path, layer, ignored) = (path.clone(), layer.clone(), ignored.clone());
            let (subset, base_too_long) = match subset {
                Some(q) if q.len() > max => (None, true),
                q => (q.clone(), false),
            };
            let filter = filter.clone();
            let Some(l) = layers.get_mut(i) else { continue };
            if base_too_long {
                report.skipped(&l.name, "the filter of the layer its virtual layer selects from, which is too long to read");
            }
            // Its own filter likewise; the rest of the filter is kept, noted.
            let filter = match filter {
                Some(f) if f.len() > max => {
                    report.skipped(&l.name, "its filter (from its virtual layer's query), which is too long to read");
                    None
                }
                f => f,
            };
            let subset = match (subset, filter) {
                (Some(a), Some(b)) if a.len() + b.len() + 11 > max => {
                    report.skipped(&l.name, "its filter (from its virtual layer's query) with that of the layer it selects from, too long to read together: the latter alone kept");
                    Some(a)
                }
                (Some(a), Some(b)) => Some(format!("({a}) AND ({b})")),
                (a, b) => a.or(b),
            };
            let resolved = Source::File { path, layer, subset, ignored };
            l.definition_query =
                definition_query(&resolved, &l.name, "from its virtual layer's query and the layer it selects from", report);
            l.source = resolved;
            changed = true;
        }
        if !changed {
            break;
        }
    }
    // One on a layer the project doesn't have (by its id), or on one that is itself on a missing
    // layer: said as missing, not as unsupported.
    for _ in 0..MAX_VIRTUAL_CHAIN {
        let mut changed = false;
        for i in 0..layers.len() {
            let missing = match layers.get(i).map(|l| &l.source) {
                Some(Source::Virtual { from: Some(source::VirtualFrom::Layer(id)), .. }) => {
                    match layers.iter().find(|l| &l.id == id).map(|l| &l.source) {
                        None => {
                            let shown: String = id.chars().take(100).collect();
                            let more = if id.chars().count() > 100 { "…" } else { "" };
                            Some(format!("the layer with id `{shown}{more}` it selects from isn't in the project"))
                        }
                        // On one itself on a missing layer: the same reason.
                        Some(Source::Virtual { from: Some(source::VirtualFrom::Missing(why)), .. }) => Some(why.clone()),
                        Some(_) => None,
                    }
                }
                _ => None,
            };
            if let (Some(base), Some(Source::Virtual { from, filter, .. })) = (missing, layers.get_mut(i).map(|l| &mut l.source)) {
                *from = Some(source::VirtualFrom::Missing(base));
                *filter = None;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    // Still on another layer after that: the virtual layers select from each other in a ring, or
    // go deeper than RustGIS follows (the base at the end of a deep chain is a file's layer by
    // now, unresolved only because the passes ran out).
    // Followed link by link: to a file's layer (the passes ran out) or round to a layer already
    // passed (a ring); at most one link past those followed, a longer chain being too deep
    // whatever its end. One ending elsewhere (a CSV, memory or database layer, a grouping
    // query) keeps its own reason.
    let rings: Vec<bool> = {
        let by_id: std::collections::HashMap<&str, usize> = layers.iter().enumerate().map(|(i, l)| (l.id.as_str(), i)).collect();
        (0..layers.len())
            .map(|i| {
                let mut passed: Vec<&str> = Vec::new();
                let mut at = layers.get(i);
                loop {
                    match at.map(|l| (&l.id, &l.source)) {
                        Some((id, Source::Virtual { from: Some(source::VirtualFrom::Layer(next)), .. })) => {
                            if passed.contains(&id.as_str()) || passed.len() > MAX_VIRTUAL_CHAIN {
                                break true;
                            }
                            passed.push(id);
                            at = by_id.get(next.as_str()).and_then(|&j| layers.get(j));
                        }
                        // (Reached from a virtual layer: the chain's end the passes didn't reach.)
                        Some((_, Source::File { .. })) => break !passed.is_empty(),
                        _ => break false,
                    }
                }
            })
            .collect()
    };
    for (l, ring) in layers.iter_mut().zip(rings) {
        if let (true, Source::Virtual { from, filter, .. }) = (ring, &mut l.source) {
            *from = Some(source::VirtualFrom::Missing(format!(
                "the virtual layers it selects from select from each other, or go deeper than the {MAX_VIRTUAL_CHAIN} RustGIS follows"
            )));
            *filter = None;
        }
    }
}

/// How many virtual layers on virtual layers are followed.
const MAX_VIRTUAL_CHAIN: usize = 8;

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
    style.time = structure::layer_time(node, layer, report);
    if let Some(r) = child(node, "renderer-v2") {
        renderer::read(r, layer, &mut style, report);
    }
    style.diagram = diagram::read(node, layer, style.reference_scale, &mut style.diagram_by, report);
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

/// Notes the project's parts RustGIS has no place for: print layouts, annotations of QGIS's
/// older kind and map themes.
fn project_notes(root: Node, report: &mut Report) {
    let count = |list: &str, item: &str| child(root, list).map_or(0, |l| l.children().filter(|c| c.has_tag_name(item)).count());
    let layouts = count("Layouts", "Layout") + root.children().filter(|c| c.has_tag_name("Composer")).count();
    if layouts > 0 {
        report.skipped("", format!("its print layouts ({layouts})"));
    }
    // QGIS 3's older annotations, kept beside the annotation layers (form, HTML, SVG and text
    // balloons).
    let balloons = child(root, "Annotations").map_or(0, |a| a.children().filter(Node::is_element).count());
    if balloons > 0 {
        report.skipped("", format!("its annotations of QGIS's older kind ({balloons}: text, HTML, SVG and form balloons)"));
    }
    let themes = count("visibility-presets", "visibility-preset");
    if themes > 0 {
        report.skipped("", format!("its map themes ({themes})"));
    }
}

/// Notes what the layer tree's groups carry over only in part: groups past RustGIS's limits,
/// groups showing one layer at a time, group layers' opacity, blending and effects (`blended`,
/// `effects`: the groups whose blending mode, draw effect is left out), and a drawing order of
/// its own.
fn group_notes(tree: &Tree, blended: &[String], effects: &[String], report: &mut Report) {
    let listed = |names: &[&str]| {
        let mut text = names.iter().take(8).copied().collect::<Vec<_>>().join(", ");
        if names.len() > 8 {
            text.push_str(", …");
        }
        format!("{}: {text}", names.len())
    };
    if tree.folded > 0 {
        report.approximated(
            "",
            format!(
                "groups past RustGIS's {} levels or {} groups ({}): their layers put in the group holding them",
                crate::MAX_GROUP_DEPTH,
                crate::MAX_GROUPS,
                tree.folded
            ),
        );
    }
    let exclusive: Vec<&str> = tree.groups.iter().filter(|g| g.exclusive).map(|g| g.group.name.as_str()).collect();
    if !exclusive.is_empty() {
        report.approximated("", format!("groups showing one layer at a time ({}), carried over as ordinary groups", listed(&exclusive)));
    }
    let opaque: Vec<&str> = tree.groups.iter().filter(|g| g.group.transparency.is_some()).map(|g| g.group.name.as_str()).collect();
    if !opaque.is_empty() {
        report.approximated(
            "",
            format!(
                "the opacity of groups drawn as one ({}), applied to each of their layers rather than to them drawn together",
                listed(&opaque)
            ),
        );
    }
    if !blended.is_empty() {
        let names: Vec<&str> = blended.iter().map(String::as_str).collect();
        report.approximated("", format!("the blending mode of groups drawn as one ({}), drawn as Normal", listed(&names)));
    }
    if !effects.is_empty() {
        let names: Vec<&str> = effects.iter().map(String::as_str).collect();
        report.skipped("", format!("the draw effects of groups drawn as one ({})", listed(&names)));
    }
    if tree.custom_order {
        report.approximated("", "its layer order (set apart from the layer tree): drawn in the layer tree's order");
    }
    if tree.legend_order {
        report.approximated("", "its layer order (set apart from the legend), not read: drawn in the legend's order");
    }
    if tree.deep > 0 {
        report.approximated(
            "",
            format!("layers below {TREE_DEPTH} levels of groups ({}): put in the group holding them, with their groups' boxes", tree.deep),
        );
    }
    if tree.cut > 0 {
        report.approximated(
            "",
            format!("the layer tree's entries past the first {TREE_LAYERS} ({}): their layers come in at the top level, shown", tree.cut),
        );
    }
}

/// The project's main annotation layer (`<main-annotation-layer>`), drawn over its layers, as a
/// layer named "Annotations": `None` when it holds no annotation to draw.
fn main_annotations(node: Node, dir: &str, report: &mut Report) -> Option<ProjectLayer> {
    const NAME: &str = "Annotations";
    let annotations = annotation::read(node, dir, NAME, report);
    if annotations.is_empty() {
        return None;
    }
    Some(ProjectLayer {
        id: child(node, "id").and_then(|n| n.text()).unwrap_or_default().trim().to_string(),
        name: NAME.to_string(),
        provider: String::new(),
        source: Source::Other { provider: String::new() },
        crs: child(node, "srs").and_then(crs_of),
        kind: "annotation".to_string(),
        vector: false,
        visible: true,
        group: None,
        checked: true,
        definition_query: None,
        style: read_style_node(node, NAME, report),
        annotations,
    })
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

/// A layer of the layer tree, top first.
struct TreeLayer {
    id: String,
    /// Checked, and every group holding it too.
    visible: bool,
    /// Its own box, and those of the groups between it and `group` that weren't kept.
    checked: bool,
    group: Option<usize>,
}

/// A group of the layer tree.
struct TreeGroup {
    group: ProjectGroup,
    /// The id of its QGIS group layer (`groupLayer`), drawn as one, if it is one.
    layer: Option<String>,
    /// Its layers show one at a time (QGIS's mutually exclusive group).
    exclusive: bool,
}

/// Where in the layer tree a walk is: what applies to the children of a group.
#[derive(Clone, Copy)]
struct Place {
    /// Every group above checked.
    visible: bool,
    /// The boxes of the groups since `kept` that weren't kept.
    checked: bool,
    /// The group its layers go in (an index into [`Tree::groups`]).
    kept: Option<usize>,
    /// The level of a group made here (1 at the top).
    level: usize,
}

/// A project's layer tree (QGIS 2's legend before it): its layers, top first, and its groups.
/// Groups past RustGIS's limits ([`crate::MAX_GROUP_DEPTH`] levels, [`crate::MAX_GROUPS`]
/// groups) aren't kept: their layers go in the group holding them, with their boxes.
#[derive(Default)]
struct Tree {
    layers: Vec<TreeLayer>,
    groups: Vec<TreeGroup>,
    /// The groups not kept.
    folded: usize,
    /// Layers below [`TREE_DEPTH`] levels of groups, read without walking further in.
    deep: usize,
    /// The group layers (by id) of groups drawn as one that weren't kept.
    folded_layers: std::collections::HashSet<String>,
    /// Layer entries past [`TREE_LAYERS`], not read.
    cut: usize,
    /// A drawing order of its own is on (QGIS's Layer Order panel) and differs from the tree's.
    custom_order: bool,
    /// QGIS 2's legend keeps a drawing order of its own (`updateDrawingOrder="false"`).
    legend_order: bool,
}

/// The most levels of groups the layer tree is walked into; the layers further in are read
/// without walking (see [`Tree::flatten`]).
const TREE_DEPTH: usize = 64;
/// The most layer entries read from the layer tree.
const TREE_LAYERS: usize = MAX_LAYERS * 4;

/// Whether a layer tree or legend entry's box is checked.
fn checked_box(n: Node) -> bool {
    n.attribute("checked").is_none_or(|c| c == "Qt::Checked" || c == "Qt::PartiallyChecked")
}

impl Tree {
    fn read(root: Node) -> Tree {
        let mut tree = Tree::default();
        let top = Place { visible: true, checked: true, kept: None, level: 1 };
        if let Some(t) = child(root, "layer-tree-group") {
            tree.walk(t, false, top, 0);
            // The custom order, top first, of the layers in the tree.
            if let Some(c) = child(t, "custom-order").filter(|c| c.attribute("enabled") == Some("1")) {
                let known: std::collections::HashSet<&str> = tree.layers.iter().map(|l| l.id.as_str()).collect();
                let ids: Vec<&str> = c
                    .children()
                    .filter(|i| i.has_tag_name("item"))
                    .take(TREE_LAYERS)
                    .filter_map(|i| i.text())
                    .map(str::trim)
                    .filter(|id| known.contains(id))
                    .collect();
                let listed: std::collections::HashSet<&str> = ids.iter().copied().collect();
                let order: Vec<&str> = tree.layers.iter().map(|l| l.id.as_str()).filter(|id| listed.contains(id)).collect();
                tree.custom_order = ids != order;
            }
        } else if let Some(l) = child(root, "legend") {
            tree.walk(l, true, top, 0);
            // Each legend layer's place in it (`drawingOrder`, -1 when it follows the legend).
            tree.legend_order = l.attribute("updateDrawingOrder") == Some("false")
                && l.descendants()
                    .filter(|n| n.has_tag_name("legendlayer"))
                    .any(|n| n.attribute("drawingOrder").and_then(|o| o.trim().parse::<i64>().ok()).is_some_and(|o| o >= 0));
        }
        tree
    }

    /// Keeps the id of the group layer of a group not kept (`groupLayer`), if it is one.
    fn fold_group_layer(&mut self, group: Node) {
        if let Some(id) = group.attribute("groupLayer").map(str::trim).filter(|l| !l.is_empty()) {
            self.folded_layers.insert(id.to_string());
        }
    }

    /// The tags of a layer and of a group (`legend`: QGIS 2's `<legend>`).
    fn tags(legend: bool) -> (&'static str, &'static str) {
        if legend { ("legendlayer", "legendgroup") } else { ("layer-tree-layer", "layer-tree-group") }
    }

    /// Adds a layer entry's layers (a QGIS 2 legend layer may list several), up to
    /// [`TREE_LAYERS`]; past that they are counted in `cut`.
    fn add(&mut self, c: Node, legend: bool, visible: bool, checked: bool, group: Option<usize>) {
        let ids: Vec<&str> = if legend {
            c.descendants().filter(|f| f.has_tag_name("legendlayerfile")).filter_map(|f| f.attribute("layerid")).collect()
        } else {
            c.attribute("id").into_iter().collect()
        };
        for id in ids {
            if self.layers.len() >= TREE_LAYERS {
                self.cut += 1;
            } else {
                self.layers.push(TreeLayer { id: id.to_string(), visible, checked, group });
            }
        }
    }

    /// Reads a group's children (`legend`: QGIS 2's `<legend>`), walking into the groups in it
    /// [`TREE_DEPTH`] levels deep; further in, [`Tree::flatten`] reads the layers.
    fn walk(&mut self, group: Node, legend: bool, at: Place, depth: usize) {
        if depth > TREE_DEPTH {
            self.flatten(group, legend, at);
            return;
        }
        let on = checked_box;
        let (layer_tag, group_tag) = Tree::tags(legend);
        for c in group.children().filter(Node::is_element) {
            if c.has_tag_name(layer_tag) {
                self.add(c, legend, at.visible && on(c), at.checked && on(c), at.kept);
            } else if c.has_tag_name(group_tag) {
                let visible = at.visible && on(c);
                let inside = if at.level <= crate::MAX_GROUP_DEPTH && self.groups.len() < crate::MAX_GROUPS {
                    let name = c.attribute("name").map(str::trim).filter(|n| !n.is_empty()).unwrap_or("Group");
                    let expanded = if legend { c.attribute("open") != Some("false") } else { c.attribute("expanded") != Some("0") };
                    self.groups.push(TreeGroup {
                        group: ProjectGroup {
                            name: name.to_string(),
                            checked: on(c),
                            expanded,
                            parent: at.kept,
                            transparency: None,
                            min_scale: None,
                            max_scale: None,
                        },
                        layer: c.attribute("groupLayer").map(str::trim).filter(|l| !l.is_empty()).map(str::to_string),
                        exclusive: c.attribute("mutually-exclusive") == Some("1"),
                    });
                    Place { visible, checked: true, kept: Some(self.groups.len() - 1), level: at.level + 1 }
                } else {
                    self.folded += 1;
                    self.fold_group_layer(c);
                    Place { visible, checked: at.checked && on(c), kept: at.kept, level: at.level + 1 }
                };
                self.walk(c, legend, inside, depth + 1);
            }
        }
    }

    /// Reads the layers in a group too deep to walk into, in order, without recursing: each in
    /// the group holding them all (`at`), with its box and those of the groups between (counted
    /// in `deep`).
    fn flatten(&mut self, group: Node, legend: bool, at: Place) {
        let (layer_tag, group_tag) = Tree::tags(legend);
        // Entries to read, the next on top, each with the boxes of the groups above it.
        let mut stack: Vec<(Node, bool)> = group.children().filter(Node::is_element).rev().map(|c| (c, true)).collect();
        while let Some((c, boxes)) = stack.pop() {
            if c.has_tag_name(layer_tag) {
                let on = boxes && checked_box(c);
                // Only those read: the ones past the budget are counted in `cut`.
                let before = self.layers.len();
                self.add(c, legend, at.visible && on, at.checked && on, at.kept);
                self.deep += self.layers.len() - before;
            } else if c.has_tag_name(group_tag) {
                self.folded += 1;
                self.fold_group_layer(c);
                let inside = boxes && checked_box(c);
                stack.extend(c.children().filter(Node::is_element).rev().map(|n| (n, inside)));
            }
        }
    }
}

/// QGIS's blending modes RustGIS has, by the number QGIS saves a layer's (`<blendMode>`): its
/// own numbering, not Qt's composition modes (measured: QGIS 4.2 saves Multiply as 6). Older
/// QGIS does too: a QGIS 3.10 project saves 6 and a QGIS 2.2 style 10, which in Qt's numbers
/// are modes QGIS doesn't offer (DestinationIn, DestinationAtop), in QGIS's Multiply and Hard
/// Light.
fn blend_of(mode: i32) -> Option<Blend> {
    Some(match mode {
        0 => Blend::Normal,
        1 => Blend::Lighten,
        2 => Blend::Screen,
        5 => Blend::Darken,
        6 => Blend::Multiply,
        8 => Blend::Overlay,
        11 => Blend::Difference,
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

/// Parses QGIS's XML with a node limit, through [`rustgis_data::xml::parse`] (which bounds its
/// nesting and its namespaces). QGIS starts its files with `<!DOCTYPE qgis …>`, which
/// declares nothing: the document is read from past it, and one declaring entities (`[…]`) is
/// refused rather than expanded.
fn parse<'a>(text: &'a str, what: &str) -> Result<Document<'a>, String> {
    let opts = ParsingOptions { allow_dtd: false, nodes_limit: MAX_NODES, ..ParsingOptions::default() };
    let doctype = text.find("<!DOCTYPE").filter(|at| text.get(..*at).is_some_and(|before| !before.contains("<q")));
    let Some(at) = doctype else {
        return rustgis_data::xml::parse(text, opts).map_err(|e| format!("not {what}: {e}"));
    };
    let end = text.get(at..).and_then(|t| t.find('>')).map(|e| at + e + 1).ok_or_else(|| format!("not {what}: an unclosed <!DOCTYPE"))?;
    if text.get(at..end).is_some_and(|d| d.contains('[')) {
        return Err(format!("not {what}: it declares entities (a DTD), which aren't read"));
    }
    let rest = text.get(end..).unwrap_or_default();
    rustgis_data::xml::parse(rest, opts).map_err(|e| format!("not {what}: {e}"))
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
