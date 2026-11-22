//! A QGIS project's bookmarks, relations and time settings, and a layer's time settings (QGIS
//! 3.10 on; earlier projects have no bookmarks of their own and no time settings).

use roxmltree::Node;

use super::{Report, child, crs_of};

/// The most bookmarks and relations an import reads; also the most bookmarks a map keeps.
pub const MAX_BOOKMARKS: usize = 10_000;
pub const MAX_RELATIONS: usize = 10_000;

/// One of a project's spatial bookmarks.
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectBookmark {
    pub name: String,
    /// The place it shows: [xmin, ymin, xmax, ymax] in `crs`.
    pub extent: [f64; 4],
    /// Its coordinate system (`EPSG:…`, or a PROJ string); `None`: the project's.
    pub crs: Option<String>,
    /// The map's rotation, in degrees: QGIS turns the map this far clockwise (measured: a point
    /// north of the centre draws this far clockwise from straight up).
    pub rotation: f64,
}

/// A relation between two of a project's layers: features of the referencing (child) layer
/// whose `referencing_field` holds the value of the referenced (parent) layer's
/// `referenced_field`. Layers by their QGIS ids.
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectRelation {
    pub name: String,
    pub referenced: String,
    pub referenced_field: String,
    pub referencing: String,
    pub referencing_field: String,
}

/// The project's bookmarks (`<Bookmarks>`), as QGIS 3.10 on saves them: each a polygon in its
/// own coordinate system, its group and the map's rotation. Those whose place doesn't read are
/// noted, and their groups, which RustGIS's bookmarks don't have.
pub fn bookmarks(root: Node, report: &mut Report) -> Vec<ProjectBookmark> {
    let Some(list) = child(root, "Bookmarks") else { return Vec::new() };
    let all: Vec<Node> = list.children().filter(|c| c.has_tag_name("Bookmark")).collect();
    if all.len() > MAX_BOOKMARKS {
        report.skipped("", format!("its bookmarks past the first {MAX_BOOKMARKS} ({})", all.len() - MAX_BOOKMARKS));
    }
    let mut out = Vec::new();
    let mut unread = Vec::new();
    // Those whose rotation doesn't read, facing north.
    let mut unturned = Vec::new();
    let mut groups: Vec<&str> = Vec::new();
    let mut seen_groups = std::collections::HashSet::new();
    // Those with no name, named "Bookmark".
    let mut unnamed = 0usize;
    // Those whose own coordinate system is there but doesn't read (no code, no PROJ string):
    // not placed in the project's, which may not be theirs.
    let mut unread_crs = Vec::new();
    for b in all.into_iter().take(MAX_BOOKMARKS) {
        let given = b.attribute("name").map(str::trim).filter(|n| !n.is_empty());
        let name = given.unwrap_or("Bookmark");
        let Some(extent) = b.attribute("extent").and_then(polygon_extent) else {
            unread.push(name);
            continue;
        };
        let crs = crs_of(b);
        if crs.is_none() && b.descendants().any(|d| d.has_tag_name("spatialrefsys")) {
            unread_crs.push(name);
            continue;
        }
        if let Some(g) = b.attribute("group").map(str::trim).filter(|g| !g.is_empty())
            && seen_groups.insert(g)
        {
            groups.push(g);
        }
        let rotation = match b.attribute("rotation").map(str::trim).filter(|r| !r.is_empty()) {
            None => 0.0,
            Some(r) => r.parse::<f64>().ok().filter(|r| r.is_finite()).unwrap_or_else(|| {
                unturned.push(name);
                0.0
            }),
        };
        unnamed += usize::from(given.is_none());
        out.push(ProjectBookmark { name: name.to_string(), extent, crs, rotation });
    }
    if unnamed > 0 {
        report.approximated("", format!("its bookmarks with no name, named Bookmark ({unnamed})"));
    }
    if !unread_crs.is_empty() {
        report.skipped("", format!("its bookmarks whose coordinate system doesn't read ({})", listed(&unread_crs)));
    }
    if !unread.is_empty() {
        report.skipped("", format!("its bookmarks whose place doesn't read ({})", listed(&unread)));
    }
    if !unturned.is_empty() {
        report.approximated("", format!("its bookmarks whose rotation doesn't read, facing north ({})", listed(&unturned)));
    }
    if !groups.is_empty() {
        report.skipped("", format!("the groups of its bookmarks ({}): RustGIS lists bookmarks in one list", listed(&groups)));
    }
    out
}

/// The project's relations (`<relations>`), each on one pair of fields. One on several pairs is
/// noted and related by its first; QGIS's polymorphic relations (`<polymorphicRelations>`: a
/// child layer whose features each name their parent layer) are noted, left out.
pub fn relations(root: Node, report: &mut Report) -> Vec<ProjectRelation> {
    let polymorphic = child(root, "polymorphicRelations").map_or(0, |p| p.children().filter(|c| c.has_tag_name("relation")).count());
    if polymorphic > 0 {
        report.skipped("", format!("its polymorphic relations ({polymorphic}: a layer related to several, by a field naming the layer)"));
    }
    let Some(list) = child(root, "relations") else { return Vec::new() };
    let all: Vec<Node> = list.children().filter(|c| c.has_tag_name("relation")).collect();
    if all.len() > MAX_RELATIONS {
        report.skipped("", format!("its relations past the first {MAX_RELATIONS} ({})", all.len() - MAX_RELATIONS));
    }
    let mut out = Vec::new();
    let mut unread = Vec::new();
    // Those with no name, named by their id (else "Relation").
    let mut unnamed = Vec::new();
    for r in all.into_iter().take(MAX_RELATIONS) {
        let id = r.attribute("id").map(str::trim).unwrap_or_default();
        let given = r.attribute("name").map(str::trim).filter(|n| !n.is_empty());
        let name = given.unwrap_or(id);
        let name = if name.is_empty() { "Relation" } else { name };
        let attr = |n: Node, k: &str| n.attribute(k).map(str::trim).filter(|v| !v.is_empty()).map(str::to_string);
        let refs: Vec<Node> = r.children().filter(|c| c.has_tag_name("fieldRef")).collect();
        let pairs: Vec<(String, String)> =
            refs.iter().filter_map(|c| Some((attr(*c, "referencingField")?, attr(*c, "referencedField")?))).collect();
        // (QGIS writes both fields of each pair: a pair missing one is a hand-edited project's.)
        if !pairs.is_empty() && pairs.len() < refs.len() {
            report.skipped("", format!("its relation {name}'s pairs of fields that don't name both ({})", refs.len() - pairs.len()));
        }
        let (Some(referenced), Some(referencing), Some((referencing_field, referenced_field))) =
            (attr(r, "referencedLayer"), attr(r, "referencingLayer"), pairs.first().cloned())
        else {
            unread.push(name);
            continue;
        };
        if pairs.len() > 1 {
            report.approximated(
                "",
                format!(
                    "its relation {name} on {} pairs of fields: related by the first ({referencing_field} = {referenced_field}) alone, which relates more features",
                    pairs.len()
                ),
            );
        }
        if r.attribute("strength") == Some("Composition") {
            report.skipped("", format!("its relation {name}'s composition (related features deleted and copied with theirs)"));
        }
        if given.is_none() {
            unnamed.push(name);
        }
        out.push(ProjectRelation { name: name.to_string(), referenced, referenced_field, referencing, referencing_field });
    }
    if !unread.is_empty() {
        report.skipped("", format!("its relations that name no layers or fields ({})", listed(&unread)));
    }
    if !unnamed.is_empty() {
        report.approximated("", format!("its relations with no name, named by their id, else Relation ({})", listed(&unnamed)));
    }
    out
}

/// Notes the project's time settings (`<ProjectTimeSettings>`) when they set a span: RustGIS's
/// time slider is left off, as QGIS opens a project with its time navigation off (measured, and
/// not saved with the project), so every feature draws.
pub fn project_time(root: Node, report: &mut Report) {
    let Some(t) = child(root, "ProjectTimeSettings") else { return };
    let range = child(t, "TemporalRange");
    let at = |tag: &str| range.and_then(|r| child(r, tag)).and_then(|n| n.text()).map(str::trim).filter(|v| !v.is_empty());
    let (Some(start), Some(end)) = (at("start"), at("end")) else { return };
    let step = t.attribute("timeStep").map(str::trim).unwrap_or("1");
    let (one, many) = unit_name(t.attribute("timeStepUnit").unwrap_or("h"));
    let unit = if step.parse::<f64>().ok() == Some(1.0) { one } else { many };
    let cumulative = if t.attribute("cumulativeTemporalRange") == Some("1") { ", cumulative" } else { "" };
    report.skipped(
        "",
        format!(
            "its time span and steps ({} to {}, steps of {step} {unit}{cumulative}): the time slider is left off, as QGIS opens a project; turned on, it spans the dates of the layer it is turned on for",
            day(start),
            day(end)
        ),
    );
}

/// A vector layer's time settings (`<temporal>`, on): the field whose dates the time slider
/// reads, what RustGIS's slider can't do noted. `None`: off, or no field to read.
pub fn layer_time(node: Node, layer: &str, report: &mut Report) -> Option<String> {
    let t = child(node, "temporal").filter(|t| t.attribute("enabled") == Some("1"))?;
    // A raster's, a mesh's or a service's: a time range for the whole layer. A style file
    // (`.qml`) has no type: a vector layer's time names its fields (QGIS 4.2 writes them all,
    // even empty), a raster's doesn't (`bandNumber`, `fetchMode`, `mode`; measured).
    let vector =
        node.attribute("type").map_or(t.attribute("startField").is_some() || t.attribute("startExpression").is_some(), |k| k == "vector");
    if !vector {
        report.skipped(layer, "its time settings");
        return None;
    }
    let attr = |k: &str| t.attribute(k).map(str::trim).filter(|v| !v.is_empty());
    let field = match t.attribute("mode") {
        // Each feature at the date in a field.
        Some("1") => attr("startField"),
        // From a field's date to another's, or for a duration in a field.
        Some("2") | Some("3") => {
            let (part, by) =
                if t.attribute("mode") == Some("2") { ("end dates", attr("endField")) } else { ("durations", attr("durationField")) };
            if let Some(by) = by.filter(|_| attr("startField").is_some()) {
                report.approximated(layer, format!("its features' {part} (the field {by}): each shown at its start date alone"));
            }
            attr("startField")
        }
        // From one expression's date to another's: read when the start is a field.
        Some("4") => match attr("startExpression") {
            Some(e) if bare_field(e).is_none() => {
                report.skipped(layer, format!("its time, from the expression `{e}`"));
                return None;
            }
            start => {
                if let Some(e) = attr("endExpression").filter(|_| start.is_some()) {
                    report.approximated(layer, format!("its features' end dates (`{e}`): each shown at its start date alone"));
                }
                start.and_then(bare_field)
            }
        },
        Some("0") => {
            report.skipped(layer, "its fixed time range, which RustGIS's time slider hasn't: not taken");
            return None;
        }
        Some("5") => {
            report.skipped(layer, "its redrawing as the map's time changes (expressions reading it)");
            return None;
        }
        _ => {
            report.skipped(layer, "its time settings");
            return None;
        }
    };
    let Some(field) = field else {
        report.skipped(layer, "its time settings, which name no field");
        return None;
    };
    // An event lasting a fixed time (QGIS's single date field with a duration; accumulating, it
    // has none).
    let duration = t.attribute("fixedDuration").and_then(|d| d.trim().parse::<f64>().ok()).filter(|d| d.is_finite() && *d > 0.0);
    if t.attribute("mode") == Some("1")
        && t.attribute("accumulate") != Some("1")
        && let Some(d) = duration
    {
        let unit = unit_name(t.attribute("durationUnit").unwrap_or("min"));
        let unit = if d == 1.0 { unit.0 } else { unit.1 };
        report.approximated(layer, format!("its features lasting {d} {unit} each: each shown at its date alone"));
    }
    if t.attribute("accumulate") == Some("1") {
        report.approximated(
            layer,
            "its features accumulating over time: RustGIS's time slider accumulates for every layer at once (its cumulative window)",
        );
    }
    if t.attribute("limitMode") == Some("1") {
        report.approximated(layer, "a feature at the very end of a time step shown in it: RustGIS shows it in the next step");
    }
    Some(field.to_string())
}

/// The field an expression is, when it is nothing but one: `"name"` or `name`.
fn bare_field(e: &str) -> Option<&str> {
    let e = e.trim();
    let inner = e.strip_prefix('"').and_then(|e| e.strip_suffix('"')).filter(|f| !f.contains('"'));
    let plain =
        (!e.is_empty() && e.chars().all(|c| c.is_alphanumeric() || c == '_') && !e.starts_with(|c: char| c.is_ascii_digit())).then_some(e);
    inner.or(plain).filter(|f| !f.is_empty())
}

/// A QGIS time unit's name, for one and for several (`timeStepUnit`, as QGIS 4.2 writes them,
/// measured).
fn unit_name(code: &str) -> (&'static str, &'static str) {
    match code.trim() {
        "ms" => ("millisecond", "milliseconds"),
        "s" => ("second", "seconds"),
        "min" => ("minute", "minutes"),
        "h" => ("hour", "hours"),
        "d" => ("day", "days"),
        "wk" => ("week", "weeks"),
        "mon" => ("month", "months"),
        "y" => ("year", "years"),
        "dec" => ("decade", "decades"),
        "c" => ("century", "centuries"),
        "xxx" => ("irregular step", "irregular steps"),
        _ => ("unit", "units"),
    }
}

/// A date and time as QGIS saves it (`2020-01-01T00:00:00Z`), its time left out at midnight.
fn day(t: &str) -> &str {
    match t.split_once('T') {
        Some((d, time)) if time.trim_end_matches('Z').trim_end_matches(".000") == "00:00:00" => d,
        _ => t,
    }
}

/// The most points a bookmark's ring may have.
const MAX_RING: usize = 10_000;

/// The bounds of a polygon in WKT (`Polygon ((x y, x y, …))`, as QGIS saves a bookmark's place):
/// [xmin, ymin, xmax, ymax], when its coordinates read and it has an area.
fn polygon_extent(wkt: &str) -> Option<[f64; 4]> {
    let body = wkt.trim();
    let open = body.find('(')?;
    let tag = body.get(..open)?.trim();
    if !tag.eq_ignore_ascii_case("polygon") {
        return None;
    }
    let inner = body.get(open..)?.trim_matches(|c: char| c == '(' || c == ')' || c.is_whitespace());
    // The outer ring alone.
    let ring = inner.split(')').next()?;
    // QGIS writes a bookmark's rectangle (five points); a longer ring isn't read in part.
    if ring.split(',').nth(MAX_RING).is_some() {
        return None;
    }
    let mut e = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    for pair in ring.split(',') {
        let mut v = pair.split_whitespace().map(|n| n.parse::<f64>().ok().filter(|v| v.is_finite()));
        let (Some(Some(x)), Some(Some(y))) = (v.next(), v.next()) else { return None };
        // A third value (a height) isn't a bookmark's: the place doesn't read, and is noted.
        if v.next().is_some() {
            return None;
        }
        e = [e[0].min(x), e[1].min(y), e[2].max(x), e[3].max(y)];
    }
    (e[0] < e[2] && e[1] < e[3]).then_some(e)
}

/// Up to eight names, comma-separated, after their count.
pub fn listed(names: &[&str]) -> String {
    let mut text = names.iter().take(8).copied().collect::<Vec<_>>().join(", ");
    if names.len() > 8 {
        text.push_str(", …");
    }
    format!("{}: {text}", names.len())
}
