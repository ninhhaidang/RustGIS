//! Data › Data Design (domains, subtypes, attribute rules, contingent values), Archiving and
//! Relationships, and Edit › Manage Edits, Snapping, Features (modify tools), Elevation and
//! Corrections. Every feature edit goes through [`apply`], which enforces the layer's design.

use rustgis_data::value::PastClasses;
use rustgis_data::{Feature, FieldType, Geometry, GeometryType, Value};
use rustgis_geom::{
    Aeqd, Coord, MAX_LAT, POLE_TOLERANCE, SphericalMean, haversine_m, lonlat_to_merc, lonlat_to_plane, merc_to_lonlat, plane_to_lonlat,
};
use rustgis_map::design::{
    AttributeRule, ContingentSet, Domain, DomainKind, Elevation, FeatureTemplate, Relationship, RuleKind, Subtype, Subtypes, TopologyRule,
};
use serde_json::{Value as Json, json};
use std::collections::HashMap;

use super::{b, f, need_feature_layer, ps};
use crate::{CmdResult, CommandSpec as C, Session};

mod shared_vertex;
mod sketch;
pub use sketch::{MAX_LINE, trace_between};
mod snap_vertex;

/// One edited feature: its state before (none for an insert) and its row after (none for a
/// delete).
pub(crate) struct Change {
    pub before: Option<Feature>,
    pub row: Option<usize>,
}

/// Edits since the last save, per layer: (added, modified, deleted).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EditCounts {
    pub added: usize,
    pub modified: usize,
    pub deleted: usize,
}

/// Whether a retained display expression reads area; stale field references do not count.
fn has_area_display(l: &rustgis_map::Layer) -> bool {
    let uses_area =
        |text: &str| rustgis_data::query::parse_fields(text, &l.dataset).is_ok_and(|expr| rustgis_data::query::uses_area(&expr));
    let by = &l.symbol_by;
    l.definition_query
        .as_deref()
        .into_iter()
        .chain(l.display_filters.iter().map(|filter| filter.where_.as_str()))
        .chain(l.renderer.expression())
        .chain([&by.size, &by.fill, &by.stroke, &by.stroke_width, &by.angle].into_iter().filter_map(|text| text.as_deref()))
        .chain(std::iter::once(&l.labels.default).chain(l.labels.classes.iter()).flat_map(|class| {
            [&class.where_, &class.by.text, &class.by.size, &class.by.color, &class.by.angle].into_iter().filter_map(|text| text.as_deref())
        }))
        .any(uses_area)
}

/// Explain unavailable areas in retained display expressions without discarding their text.
pub(crate) fn display_area_message(l: &rustgis_map::Layer) -> Option<String> {
    display_area_messages(std::iter::once(l))
}

/// Count unavailable values across affected layers in one notice, naming the read scope.
pub(crate) fn display_area_messages<'a>(layers: impl Iterator<Item = &'a rustgis_map::Layer>) -> Option<String> {
    let (mut rings, mut features, mut names, mut windowed) = (0usize, 0usize, Vec::new(), false);
    for layer in layers.filter(|layer| has_area_display(layer)) {
        let before = features;
        let counts = layer
            .source
            .as_ref()
            .and_then(|source| source.unavailable_areas(&layer.dataset.features))
            .unwrap_or_else(|| rustgis_map::source::AmbiguousAreas::from_features(&layer.dataset.features));
        rings = rings.saturating_add(counts.rings);
        features = features.saturating_add(counts.features);
        if features > before {
            names.push(layer.name.clone());
            windowed |= layer.source.is_some();
        }
    }
    if rings == 0 {
        return None;
    }
    let scope = if windowed { "; streamed-layer counts cover only decoded features for the current views" } else { "" };
    Some(format!(
        "{} layer(s) ({}): area is ambiguous for {rings} ring(s) in {features} feature(s): a ring through both poles winds beyond one whole surface{scope}; area values for affected features are unavailable; saved display expressions are retained",
        names.len(),
        crate::names_said(&names)
    ))
}

/// Editing is on and a feature layer is active.
pub(crate) fn need_editable(s: &Session) -> Option<&'static str> {
    if !s.map().editing.settings.enabled {
        return Some("editing is turned off (Edit › Edit)");
    }
    need_feature_layer(s)
}

/// Editing is on and features are selected.
pub(crate) fn need_editable_selection(s: &Session) -> Option<&'static str> {
    if !s.map().editing.settings.enabled {
        return Some("editing is turned off (Edit › Edit)");
    }
    super::need_selection(s)
}

/// Most edits an archive keeps.
const MAX_ARCHIVE: usize = 100_000;

fn now() -> f64 {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// Enforces a layer's design on edited rows: subtype defaults and elevation for new features,
/// calculation rules, then domains, contingent values and constraint rules (an error undoes the
/// whole command). Records the archive and the edit counts.
pub(crate) fn apply(s: &mut Session, id: u32, changes: Vec<Change>) -> Result<(), String> {
    apply_with(s, id, changes, true)
}

/// [`apply`], with `new_defaults` false for added rows that carry another feature's attributes
/// (the pieces of a cut): they keep them, with no subtype defaults or elevation filled in.
/// Calculation rules and checks still run on every row.
pub(crate) fn apply_with(s: &mut Session, id: u32, changes: Vec<Change>, new_defaults: bool) -> Result<(), String> {
    if changes.is_empty() {
        return Ok(());
    }
    if !s.map().editing.settings.enabled {
        return Err("editing is turned off (Edit › Edit)".into());
    }
    let elevation = s.map().editing.settings.elevation.clone();
    // Elevation from a surface: sampled before the layer is borrowed mutably.
    let z_of = |s: &Session, g: &Geometry| -> Option<f64> {
        match &elevation {
            Elevation::None => None,
            Elevation::Constant(z) => Some(*z),
            Elevation::Surface(name) => {
                let c = g.label_point()?;
                let r = surface_layer(s, name)?.raster.as_ref()?.data.as_ref()?;
                let (col, row) = r.pixel_at(c)?;
                r.value(0, col, row).map(f64::from)
            }
        }
    };
    let new_rows: Vec<usize> =
        if new_defaults { changes.iter().filter(|c| c.before.is_none()).filter_map(|c| c.row).collect() } else { Vec::new() };
    let zs: Vec<(usize, Option<f64>)> = new_rows
        .iter()
        .map(|r| {
            (*r, s.map().layer(id).and_then(|l| l.dataset.features.get(*r)).and_then(|f| f.geometry.as_ref()).and_then(|g| z_of(s, g)))
        })
        .collect();
    let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
    // New features: subtype defaults fill empty fields; elevation goes in `z`.
    if let Some(st) = l.design.subtypes.clone()
        && let Some(sfi) = l.dataset.field_index(&st.field)
    {
        for r in &new_rows {
            let code = l.dataset.value(*r, sfi).as_f64().map(|v| v as i64);
            let Some(sub) = code.and_then(|c| st.subtypes.iter().find(|x| x.code == c)) else { continue };
            for (field, v) in &sub.defaults {
                if let Some(fi) = l.dataset.field_index(field)
                    && let Some(ty) = l.dataset.fields.get(fi).map(|f| f.ty)
                    && let Some(cell) = l.dataset.features.get_mut(*r).and_then(|f| f.attrs.get_mut(fi))
                    && cell.is_null()
                {
                    if super::edit::non_finite_input(v, ty) {
                        return Err(format!("field '{field}' requires a finite number"));
                    }
                    *cell = super::edit::coerce(v.clone(), ty);
                }
            }
        }
    }
    if elevation != Elevation::None && !zs.is_empty() {
        l.dataset.ensure_field("z", FieldType::Double);
        if let Some(zi) = l.dataset.field_index("z") {
            for (r, z) in &zs {
                if let Some(cell) = l.dataset.features.get_mut(*r).and_then(|f| f.attrs.get_mut(zi)) {
                    *cell = z.map(Value::Float).unwrap_or(Value::Null);
                }
            }
        }
    }
    let rows: Vec<usize> = changes.iter().filter_map(|c| c.row).collect();
    // What each calculation rule's whole-number field cut: the rule and the field's name. Each
    // rule's cuts are said, even when a later rule writes the same field: it may read them.
    let mut cuts: Vec<(String, String, super::edit::Cuts)> = Vec::new();
    // Parsing against the fields separates a measurement failure from a stale rule. The latter
    // may still be skipped below, but an ambiguous area must never disable a valid rule.
    let metadata = rustgis_data::Dataset::new(l.dataset.name.clone(), l.dataset.geometry_type, l.dataset.fields.clone());
    let uses_area = |text: &str| rustgis_data::query::parse(text, &metadata).is_ok_and(|expr| rustgis_data::query::uses_area(&expr));
    let display_area = has_area_display(l);
    let rule_area = l
        .design
        .rules
        .iter()
        .any(|rule| (rule.kind == RuleKind::Constraint || l.dataset.field_index(&rule.field).is_some()) && uses_area(&rule.expression));
    if (display_area || rule_area)
        && let Some(message) = rustgis_data::ambiguous_area_message(changes.iter().filter_map(|change| {
            let feature = l.dataset.features.get(change.row?)?;
            (rule_area || change.before.as_ref().is_none_or(|before| before.geometry != feature.geometry))
                .then_some(feature.geometry.as_ref())
                .flatten()
        }))
    {
        return Err(format!("{}: {message}", l.dataset.name));
    }
    // Calculation rules, in order.
    for rule in l.design.rules.iter().filter(|r| r.kind == RuleKind::Calculation) {
        let Some(fi) = l.dataset.field_index(&rule.field) else { continue };
        let ty = l.dataset.fields.get(fi).map(|f| f.ty).unwrap_or(FieldType::Text);
        // A rule that no longer reads (its field was removed, or a hand-edited file) is skipped
        // rather than blocking every edit.
        let Ok(calc) = super::edit::Calculation::parse(&rule.expression, &metadata) else { continue };
        let mut cut = super::edit::Cuts::default();
        for r in &rows {
            let (v, c) = super::edit::coerce_cut(calc.eval(&l.dataset, *r), ty);
            cut.add(c);
            if let Some(cell) = l.dataset.features.get_mut(*r).and_then(|f| f.attrs.get_mut(fi)) {
                *cell = v;
            }
        }
        let name = l.dataset.fields.get(fi).map(|f| f.name.clone()).unwrap_or_default();
        cuts.push((rule.name.clone(), name, cut));
    }
    // Validation. Domains and contingent values are checked on the fields an edit changed (all
    // of a new feature's), so a value set before the domain doesn't block unrelated edits.
    let constraints: Vec<(String, String, rustgis_data::query::Expr)> = l
        .design
        .rules
        .iter()
        .filter(|r| r.kind == RuleKind::Constraint)
        .filter_map(|rule| {
            rustgis_data::query::parse(&rule.expression, &metadata).ok().map(|e| (rule.name.clone(), rule.expression.clone(), e))
        })
        .collect();
    let changed_fields = |r: usize| -> Option<Vec<bool>> {
        let before = changes.iter().find(|c| c.row == Some(r))?.before.as_ref()?;
        let now = l.dataset.features.get(r)?;
        Some((0..l.dataset.fields.len()).map(|i| before.attrs.get(i) != now.attrs.get(i)).collect())
    };
    for r in &rows {
        let changed = changed_fields(*r);
        let touched = |fi: usize| changed.as_ref().is_none_or(|c| c.get(fi).copied().unwrap_or(true));
        for d in &l.design.domains {
            if let Some(fi) = l.dataset.field_index(&d.field).filter(|fi| touched(*fi)) {
                let v = l.dataset.value(*r, fi);
                if !d.allows(v) {
                    return Err(format!("{} isn't allowed in {} (its domain)", v, d.field));
                }
            }
        }
        for c in &l.design.contingent {
            let vals: Vec<&Value> =
                c.fields.iter().map(|f| l.dataset.field_index(f).map(|fi| l.dataset.value(*r, fi)).unwrap_or(&Value::Null)).collect();
            let any_touched = c.fields.iter().any(|f| l.dataset.field_index(f).is_some_and(touched));
            if !any_touched || vals.iter().all(|v| v.is_null()) {
                continue;
            }
            let ok = c.combos.iter().any(|combo| {
                combo.iter().zip(&vals).all(|(want, got)| want.is_null() || got.is_null() || want.to_string() == got.to_string())
            });
            if !ok {
                let shown: Vec<String> = vals.iter().map(|v| v.to_string()).collect();
                return Err(format!("{} isn't a valid combination of {} ({})", shown.join(", "), c.fields.join(", "), c.name));
            }
        }
        for (name, expr, e) in &constraints {
            if !rustgis_data::query::eval(e, &l.dataset, *r) {
                return Err(format!("the edit breaks the rule {name} ({expr})"));
            }
        }
    }
    // Archive and counts.
    let t = now();
    let mut counts = EditCounts::default();
    let archiving = l.design.archive.is_some();
    let mut entries = Vec::new();
    for c in &changes {
        let after = c.row.and_then(|r| l.dataset.features.get(r));
        match (&c.before, after) {
            (None, Some(_)) => counts.added += 1,
            (Some(_), None) => counts.deleted += 1,
            _ => counts.modified += 1,
        }
        // Features are copied into the archive only when the layer keeps one.
        if archiving {
            let fid = c.before.as_ref().or(after).map(|f| f.id).unwrap_or(0);
            entries.push(rustgis_map::design::ArchiveEntry { time: t, id: fid, before: c.before.clone(), after: after.cloned() });
        }
    }
    if let Some(archive) = &mut l.design.archive {
        archive.extend(entries);
        // A long-running archive keeps its most recent edits.
        if archive.len() > MAX_ARCHIVE {
            let drop = archive.len() - MAX_ARCHIVE;
            archive.drain(..drop);
        }
    }
    let e = s.edit_counts.entry(id).or_default();
    e.added += counts.added;
    e.modified += counts.modified;
    e.deleted += counts.deleted;
    // Said once the edit is kept: a rejected one changed nothing.
    for (rule, field, cut) in cuts {
        for notice in cut.notices(&format!("Attribute rule {rule}"), &field) {
            s.notify(notice, false);
        }
    }
    Ok(())
}

/// Registers data-design and editing commands, including opt-in exact-coordinate polygon moves.
/// Feature edits share `apply` for rules, archives and counts inside the session transaction.
pub fn commands() -> Vec<C> {
    vec![
        // Data design.
        C::new("data.domain", "Domains", "Data › Data Design", domain).roles(&[]).effects(crate::policy::Effects::NONE).when(need_feature_layer).params(
            r#"{"layer"?, "field": string, "coded"?: [[code, description], …], "min"?: number, "max"?: number, "clear"?: bool, "force"?: bool (set it even though existing values fall outside)}"#,
        ),
        C::new("data.subtypes", "Subtypes", "Data › Data Design", subtypes).roles(&[]).effects(crate::policy::Effects::NONE).when(need_feature_layer).params(
            r#"{"layer"?, "field": integer field, "subtypes": [{"code": n, "name": string, "defaults"?: {field: value}}], "symbolize"?: bool, "clear"?: bool}"#,
        ),
        C::new("data.attributeRule", "Attribute Rules", "Data › Data Design", attribute_rule).roles(&[]).effects(crate::policy::Effects::NONE).when(need_feature_layer).params(
            r#"{"layer"?, "name": string, "kind": "calculation"|"constraint", "field"?: field set by a calculation, "expression": calculator expression or SQL where clause, "remove"?: bool}"#,
        ),
        C::new("data.contingentValues", "Contingent Values", "Data › Data Design", contingent).roles(&[]).effects(crate::policy::Effects::NONE).when(need_feature_layer).params(
            r#"{"layer"?, "name": string, "fields": [field, …], "combos": [[value, …], …], "remove"?: bool}"#,
        ),
        C::new("data.archive", "Add Archive", "Data › Archiving", |s, p| {
            let on = b(p, "on").unwrap_or(true);
            let id = s.layer_id(p)?;
            let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
            l.design.archive = if on { Some(l.design.archive.take().unwrap_or_default()) } else { None };
            Ok(json!({"archiving": on}))
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .when(need_feature_layer)
        .params(r#"{"layer"?, "on"?: bool}"#),
        C::new("data.archiveHistory", "Archive History", "Data › Archiving", |s, p| {
            let id = s.layer_id(p)?;
            let l = s.map().layer(id).ok_or("no such layer")?;
            let a = l.design.archive.as_ref().ok_or("archiving is off for this layer")?;
            let rows: Vec<Json> = a
                .iter()
                .rev()
                .take(500)
                .map(|e| json!({"time": e.time, "feature": e.id, "action": match (&e.before, &e.after) { (None, _) => "insert", (_, None) => "delete", _ => "update" }}))
                .collect();
            Ok(json!({"edits": a.len(), "recent": rows}))
        }).roles(&[]).effects(crate::policy::Effects::NONE).params(r#"{"layer"?: name|id (default: the active layer)}"#)
        .view()
        .when(need_feature_layer),
        C::new("data.archiveRestore", "Restore From Archive", "Data › Archiving", archive_restore).roles(&[]).effects(crate::policy::Effects::NONE)
            .when(need_editable)
            .params(r#"{"layer"?, "time": seconds since 1970 (the layer as it was then)}"#),
        C::new("data.relate", "Related Data", "Data › Relationship", relate).inputs(relationship_inputs).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r#"{"name": string, "origin": layer, "originKey": field, "destination": layer, "destinationKey": field, "remove"?: bool}"#,
        ),
        C::new("data.related", "Select Related", "Data › Relationship", related).inputs(related_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
            .params(r#"{"name"?: relationship (all when omitted), "select"?: bool (select the related rows; default true)}"#),
        // Manage edits.
        C::new("edit.enable", "Edit", "Edit › Manage Edits", |s, p| {
            let set = &mut s.map_mut().editing.settings;
            set.enabled = b(p, "on").unwrap_or(!set.enabled);
            Ok(json!({"enabled": set.enabled}))
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .params(r#"{"on"?: bool}"#),
        C::new("edit.settings", "Editing Settings", "Edit › Manage Edits", |s, p| {
            let set = &mut s.map_mut().editing.settings;
            if let Some(v) = b(p, "showVertices") {
                set.show_vertices = v;
            }
            Ok(json!({"enabled": set.enabled, "showVertices": set.show_vertices}))
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .params(r#"{"showVertices"?: bool}"#),
        C::new("edit.status", "Edit Status", "Edit › Manage Edits", |s, _| {
            let rows: Vec<Json> = s
                .edit_counts
                .iter()
                .filter_map(|(id, c)| s.map().layer(*id).map(|l| json!({"layer": l.name, "added": c.added, "modified": c.modified, "deleted": c.deleted})))
                .collect();
            Ok(json!({"unsaved": s.dirty(), "layers": rows}))
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .view(),
        C::new("edit.template", "Manage Templates", "Edit › Manage Edits", template).roles(&[]).effects(crate::policy::Effects::NONE).when(need_feature_layer).params(
            r#"{"layer"?, "name": string, "defaults"?: {field: value}, "description"?: string, "remove"?: bool}"#,
        ),
        // Snapping.
        C::new("edit.snapping", "Snapping", "Edit › Snapping", |s, p| {
            let sn = &mut s.map_mut().editing.snapping;
            sn.enabled = b(p, "on").unwrap_or(!sn.enabled);
            if let Some(t) = f(p, "tolerancePx") {
                sn.tolerance_px = t.clamp(1.0, 100.0);
            }
            for (k, slot) in [("vertex", &mut sn.vertex), ("edge", &mut sn.edge), ("end", &mut sn.end)] {
                if let Some(v) = b(p, k) {
                    *slot = v;
                }
            }
            Ok(json!({"enabled": sn.enabled, "tolerancePx": sn.tolerance_px, "vertex": sn.vertex, "edge": sn.edge, "end": sn.end}))
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .params(r#"{"on"?: bool, "tolerancePx"?: number, "vertex"?: bool, "edge"?: bool, "end"?: bool}"#),
        C::new("edit.snap", "Snap Point", "Edit › Snapping", snap).roles(&[]).effects(crate::policy::Effects::NONE).view().params(r#"{"lon", "lat"}"#),
        // Modify features.
        C::new("edit.move", "Move", "Edit › Features › Modify", |s, p| {
            let unit = distance_unit_metres(p)?;
            let (mut dx, mut dy) = (f(p, "dx").unwrap_or(0.0) * unit, f(p, "dy").unwrap_or(0.0) * unit);
            if let Some((k, rot)) = s.map().editing.settings.ground_to_grid {
                (dx, dy) = ground_to_grid(dx, dy, k, rot);
            }
            if !(dx.is_finite() && dy.is_finite()) || dx.abs() > 4e7 || dy.abs() > 4e7 {
                return Err("dx and dy must be finite distances in the command's units (limit: 40,000 km per axis after ground-to-grid)".into());
            }
            // In Web Mercator, as always, while a feature and its result stay within its square
            // (each moved once, and kept for the move); one that doesn't, and those sharing a
            // vertex with it, move on the ground (#1033), or for a move of no distance stay as
            // they are.
            let mut shifted: HashMap<(u32, usize), Geometry> = HashMap::new();
            for l in s.map().layers.iter().filter(|l| !l.is_raster()) {
                for r in &l.selection {
                    let Some(g) = l.dataset.features.get(*r).and_then(|f| f.geometry.as_ref()).filter(|g| within_square(g)) else { continue };
                    // A vertex missing a coordinate stays as it was, as on the ground.
                    let done = g.map_coords(&|c: Coord| if c.is_finite() { shift(c, dx, dy) } else { c });
                    if within_square(&done) {
                        shifted.insert((l.id, *r), done);
                    }
                }
            }
            let still = dx == 0.0 && dy == 0.0;
            if still && shifted.is_empty() {
                return Err("a move of no distance changes nothing".into());
            }
            let (planes, ground, coarse) = if still {
                (HashMap::new(), 0, 0)
            } else {
                move_planes(s, |key| shifted.contains_key(&key), dx.hypot(dy))?
            };
            let mut placed = HashMap::new();
            placed.try_reserve(ground).map_err(|_| "not enough memory to move the selection on the ground")?;
            // Each group's move, worked out once: its planes and its turn's sine and cosine.
            let mut moves: HashMap<(i64, i64), (Aeqd, Aeqd, f64, f64)> = HashMap::new();
            let done = move_features(s, |key, g| match planes.get(&key) {
                Some(center) => {
                    let (from, to, sin, cos) = *moves.entry(vertex_key(*center)).or_insert_with(|| {
                        let (from, to, turn) = ground_move(*center, dx, dy);
                        let (sin, cos) = turn.sin_cos();
                        (from, to, sin, cos)
                    });
                    polar_transform(g, (&from, &to), Some(&mut placed), |p| Coord::new(p.x * cos + p.y * sin, p.y * cos - p.x * sin))
                        .map(Some)
                }
                None => Ok(shifted.remove(&key)),
            })?;
            center_notice(s, coarse);
            Ok(done)
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .when(need_editable_selection)
        .params(r#"{"dx": distance east, "dy": distance north, "units"?: "Meters" (default)|"Feet"|"Kilometers"|"Miles"}"#),
        C::new("edit.rotate", "Rotate", "Edit › Features › Modify", |s, p| {
            let deg = f(p, "angle").ok_or("angle is required")?;
            let mut steps = CenterSteps::new();
            let done = move_features(s, |_, g| {
                // Any point, a feature's or a text graphic's, turns on itself: left out, so a
                // selection of points alone fails rather than recording an edit that changes
                // nothing (and an undo step).
                if matches!(g, Geometry::Point(_)) {
                    return Ok(None);
                }
                let (sin, cos) = (-deg.to_radians()).sin_cos();
                about_center(
                    g,
                    |c, m| {
                        let (x, y) = (m.x - c.x, m.y - c.y);
                        Coord::new(c.x + x * cos - y * sin, c.y + x * sin + y * cos)
                    },
                    |p| Coord::new(p.x * cos - p.y * sin, p.x * sin + p.y * cos),
                    &mut steps,
                )
            })?;
            center_notice(s, steps.coarse);
            Ok(done)
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .when(need_editable_selection)
        .params(r#"{"angle": degrees clockwise}"#),
        C::new("edit.scale", "Scale", "Edit › Features › Modify", |s, p| {
            let k = f(p, "factor").filter(|k| *k > 0.0 && *k < 1e6).ok_or("factor must be a positive number")?;
            let mut steps = CenterSteps::new();
            let done = move_features(s, |_, g| {
                // Any point, a feature's or a text graphic's, scales on itself: left out, as above.
                if matches!(g, Geometry::Point(_)) {
                    return Ok(None);
                }
                let merc = |c: Coord, m: Coord| Coord::new(c.x + (m.x - c.x) * k, c.y + (m.y - c.y) * k);
                about_center(g, merc, |p| Coord::new(p.x * k, p.y * k), &mut steps)
            })?;
            center_notice(s, steps.coarse);
            Ok(done)
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .when(need_editable_selection)
        .params(r#"{"factor": number}"#),
        C::new("edit.flip", "Flip Line", "Edit › Features › Modify", |s, _| {
            modify(s, |_, g| match g {
                Geometry::Line(parts) => Ok(Some(Geometry::Line(parts.iter().rev().map(|p| p.iter().rev().copied().collect()).collect()))),
                _ => Ok(None),
            })
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .when(need_editable_selection),
        C::new("edit.setGeometry", "Set Geometry", "Edit › Features › Modify", set_geometry).roles(&[]).effects(crate::policy::Effects::NONE)
            .when(need_editable)
            .params(r#"{"layer"?, "row": number, "geometry": GeoJSON geometry}"#),
        C::new("edit.vertex", "Edit Vertex", "Edit › Features › Modify", edit_vertex).roles(&[]).effects(crate::policy::Effects::NONE).when(need_editable).params(
            r#"{"layer"?, "row": number, "action": "move"|"insert"|"delete", "part"?: number, "ring"?: number, "index": number (insert: the vertex the new one follows), "lon"?, "lat"?}"#,
        ),
        C::new("edit.merge", "Merge", "Edit › Features › Modify", merge).roles(&[]).effects(crate::policy::Effects::NONE).when(need_editable_selection),
        C::new("edit.moveSharedVertex", "Move Shared Vertex", "Edit › Features › Modify", shared_vertex::run).roles(&[]).effects(crate::policy::Effects::NONE).when(need_editable).params(
            r#"{"layer"?, "row": number, "part"?: number, "ring"?: number, "index": number, "lon": degrees, "lat": degrees}"#,
        ),
        C::new("edit.explode", "Explode", "Edit › Features › Modify", explode).roles(&[]).effects(crate::policy::Effects::NONE).when(need_editable_selection),
        C::new("edit.split", "Split", "Edit › Features › Modify", split).roles(&[]).effects(crate::policy::Effects::NONE).when(need_editable_selection).params(r#"{"lon", "lat"}"#),
        C::new("edit.cutPolygon", "Cut Polygon", "Edit › Features › Modify", sketch::cut_polygon).roles(&[]).effects(crate::policy::Effects::NONE)
            .when(need_editable_selection)
            .params(r#"{"line": GeoJSON LineString (lon/lat) drawn across the selected polygons}"#),
        C::new("edit.reshape", "Reshape", "Edit › Features › Modify", sketch::reshape).roles(&[]).effects(crate::policy::Effects::NONE)
            .when(need_editable_selection)
            .params(r#"{"line": GeoJSON LineString (lon/lat) drawn across the selected line or polygon's boundary}"#),
        C::new("edit.trace", "Trace Edge Path", "Edit › Features", sketch::trace).roles(&[]).effects(crate::policy::Effects::NONE).view().params(
            r#"{"from": [lon, lat], "to": [lon, lat], "layer"?, "row"?: number (else the nearest edge within the tolerance), "tolerancePx"?: number (default: the snapping tolerance)}"#,
        ),
        C::new("edit.traverse", "Traverse", "Edit › Features › Modify", traverse).roles(&[]).effects(crate::policy::Effects::NONE).when(need_editable).params(
            r#"{"layer"?, "start": [lon, lat], "units"?: "Meters" (default)|"Feet"|"Kilometers"|"Miles", "legs": [{"distance": number, "bearing": degrees from north}]} (closed in a polygon layer, open in a line layer)"#,
        ),
        C::new("edit.tools", "Editor Tools", "Edit › Tools", |s, _| {
            let tools: Vec<Json> = s.registry.commands.iter().filter(|c| c.location.starts_with("Edit › Features")).map(|c| json!({"id": c.id, "label": c.label, "params": c.params})).collect();
            Ok(Json::Array(tools))
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .view(),
        // Elevation and corrections.
        C::new("edit.elevation", "Elevation", "Edit › Elevation", |s, p| {
            let mode = ps(p, "mode").ok_or("mode is required")?;
            let e = match mode {
                "none" => Elevation::None,
                "constant" => {
                    let z = f(p, "z").ok_or("z is required for a constant elevation")? * distance_unit_metres(p)?;
                    if !z.is_finite() {
                        return Err("elevation is too large".into());
                    }
                    Elevation::Constant(z.clamp(-12_000.0, 100_000.0))
                },
                "surface" => {
                    let n = surface_name(p)?;
                    let l = surface_layer(s,n).ok_or_else(|| format!("no raster layer '{n}'"))?;
                    Elevation::Surface(l.name.clone())
                }
                m => return Err(format!("unknown elevation mode '{m}'")),
            };
            s.map_mut().editing.settings.elevation = e;
            Ok(Json::Null)
        }).inputs(elevation_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
        .params(r#"{"mode": "none"|"constant"|"surface", "z"?: number, "units"?: "Meters" (default)|"Feet"|"Kilometers"|"Miles", "surface"?: raster layer}"#),
        C::new("edit.groundToGrid", "Ground To Grid", "Edit › Corrections", |s, p| {
            let on = b(p, "on").unwrap_or(true);
            let k = f(p, "scale").unwrap_or(1.0);
            let r = f(p, "rotation").unwrap_or(0.0);
            if on && !(k > 0.5 && k < 2.0) {
                return Err("the scale factor is a number near 1 (0.5 to 2)".into());
            }
            s.map_mut().editing.settings.ground_to_grid = on.then_some((k, r.clamp(-360.0, 360.0)));
            Ok(json!({"on": on}))
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .params(r#"{"on"?: bool, "scale"?: factor, "rotation"?: degrees}"#),
        // Topology.
        C::new("topology.rules", "Topology Options", "Edit › Manage Edits", topology_rules).inputs(topology_inputs).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r#"{"add"?: {"rule": "mustNotOverlap"|"mustNotHaveGaps"|"mustNotSelfIntersect"|"mustNotHaveDangles"|"mustBeCoveredBy", "layer": layer, "by"?: polygon layer}, "remove"?: index, "clear"?: bool}"#,
        ),
        C::new("topology.validate", "Error Inspector", "Edit › Manage Edits", topology_validate).inputs(validation_inputs).roles(&[]).effects(crate::policy::Effects::NONE).view().params(r#"{"toleranceM"?: metres (dangles)}"#),
    ]
}

/// Distance inputs use the Buffer tool's unit names; existing editing scripts default to metres.
fn distance_unit_metres(p: &Json) -> Result<f64, String> {
    let units = match p.get("units") {
        None | Some(Json::Null) => "Meters",
        Some(Json::String(v)) => v.trim(),
        _ => return Err("units must be Meters, Feet, Kilometers or Miles".into()),
    };
    let unit = if units.is_empty() { "Meters" } else { units };
    rustgis_geom::distance_unit_metres(&unit.to_ascii_lowercase()).ok_or_else(|| "units must be Meters, Feet, Kilometers or Miles".into())
}

/// A point moved by metres east and north (Web Mercator stretched to the point's latitude).
fn shift(c: Coord, dx: f64, dy: f64) -> Coord {
    let k = 1.0 / c.y.to_radians().cos().max(0.01);
    let m = lonlat_to_merc(c);
    merc_to_lonlat(Coord::new(m.x + dx * k, m.y + dy * k))
}

/// A move on the ground by `dx` and `dy` meters east and north of `center`, as a turn of the world
/// (#1033): the center goes along the geodesic leaving it that way, and every point keeps its
/// distance and direction from it, the direction turned as much as the geodesic's own direction
/// turns on the way. The azimuthal equidistant planes at the center and at the moved center, and
/// that turn (radians, clockwise).
fn ground_move(center: Coord, dx: f64, dy: f64) -> (Aeqd, Aeqd, f64) {
    let from = Aeqd::new(center);
    if dx == 0.0 && dy == 0.0 {
        return (from, from, 0.0);
    }
    let (to, arriving) = from.inverse_heading(Coord::new(dx, dy));
    let turn = arriving.to_radians() - dx.atan2(dy);
    // Within half a turn either way (GeographicLib may give −180° where atan2 gives 180°).
    (from, Aeqd::new(to), turn - std::f64::consts::TAU * (turn / std::f64::consts::TAU).round())
}

/// A ground distance and direction corrected to the grid: scaled and rotated.
fn ground_to_grid(dx: f64, dy: f64, k: f64, rot_deg: f64) -> (f64, f64) {
    let (sin, cos) = (-rot_deg.to_radians()).sin_cos();
    ((dx * cos - dy * sin) * k, (dx * sin + dy * cos) * k)
}

/// Whether every vertex of `g` lies within Web Mercator's square, ±85.05° (every finite one: a
/// coordinate missing stays missing either way). A feature that does, and whose Web Mercator result
/// does, is edited in Web Mercator as it always has been (#1033).
fn within_square(g: &Geometry) -> bool {
    let mut within = true;
    g.for_each_coord(|c| within &= !c.y.is_finite() || c.y.abs() <= MAX_LAT);
    within
}

/// The whole turns of 360° that bring longitude `x` nearest `was`: an exact multiple of 360, so
/// adding it once to the same longitude always gives the same result (shared vertices stay equal).
fn turns(x: f64, was: f64) -> f64 {
    360.0 * ((was - x) / 360.0).round()
}

/// A vertex as a key, in billionths of a degree (a tenth of a millimeter), the same for one place
/// however it's written: 180° or −180°, 190.1° or −169.9°, a pole at any longitude and to within
/// [`POLE_TOLERANCE`].
fn vertex_key(c: Coord) -> (i64, i64) {
    /// Billionths of a degree in a turn.
    const TURN: i64 = 360_000_000_000;
    if c.y.abs() >= 90.0 - POLE_TOLERANCE {
        return (0, (90f64.copysign(c.y) * 1e9) as i64);
    }
    // Within a turn first, so a hostile longitude can't overflow the key (exactly east of 0°; west
    // of it, adding the turn rounds, perhaps up to 360°), then rounded, and the last turn taken
    // off exactly.
    (((c.x.rem_euclid(360.0) * 1e9).round() as i64).rem_euclid(TURN), (c.y * 1e9).round() as i64)
}

/// Refuses a line or polygon that moving its vertices one by one on the ground would break
/// (#1033): one reaching a pole (as rings cut at ±180° and closed along it do), or with an edge
/// spanning 180° of longitude or more (which way around it goes is then unclear once moved).
/// Points move anywhere.
fn ground_movable(g: &Geometry) -> Result<(), String> {
    let mut beyond = false;
    g.for_each_coord(|c| beyond |= c.is_finite() && c.y.abs() > 90.0);
    if beyond {
        return Err("a latitude past ±90° is no place, so it cannot be moved, rotated or scaled".into());
    }
    if matches!(g, Geometry::Point(_) | Geometry::MultiPoint(_)) {
        return Ok(());
    }
    // A vertex missing a coordinate (or with an infinite one) is no place, and stays as it was:
    // it reaches no pole and makes no edge wide.
    let (mut pole, mut wide) = (false, false);
    g.for_each_coord(|c| pole |= c.is_finite() && c.y.abs() >= 90.0 - POLE_TOLERANCE);
    if pole {
        return Err("a line or polygon that reaches a pole can't be moved, rotated or scaled yet".into());
    }
    g.for_each_edge(|_, a, b| wide |= a.is_finite() && b.is_finite() && (b.x - a.x).abs() >= 180.0);
    if wide {
        return Err("features beyond 85.05° are moved, rotated and scaled on the ground, which can't yet take a line or polygon \
                    with an edge spanning 180° of longitude or more"
            .into());
    }
    Ok(())
}

/// The longest stretch of an edge, in degrees of longitude or latitude, that [`plane_at`] weighs
/// at once.
const CENTER_STEP_DEG: f64 = 0.5;
/// The most stretches it cuts one edge into (an edge 180° long takes 360).
const CENTER_STEPS_MAX: f64 = 1024.0;
/// The most stretches it cuts all the edges of one command's features into; past them, an edge is
/// weighed in one.
const CENTER_STEPS_TOTAL: usize = 1_000_000;

/// What a command's ground centers may still spend: the stretches left of [`CENTER_STEPS_TOTAL`],
/// and how many edges were weighed in one for want of them (said by [`center_notice`]).
struct CenterSteps {
    left: usize,
    coarse: usize,
}

impl CenterSteps {
    fn new() -> Self {
        Self { left: CENTER_STEPS_TOTAL, coarse: 0 }
    }
}

/// Says how many edges a command's ground centers weighed in one, past its limit of stretches.
fn center_notice(s: &mut Session, coarse: usize) {
    if coarse > 0 {
        s.warn(format!(
            "{coarse} edge(s) of features beyond 85.05° were weighed in one to find their center, past one command's limit of a million stretches: those centers are worked out more coarsely"
        ));
    }
}
/// The farthest (meters) a polar edit may carry a vertex from the center it's moved, turned or
/// scaled about: a quarter of the way around the world, well short of the center's antipode, where
/// the azimuthal equidistant plane tears.
const GROUND_REACH_M: f64 = 10_000_000.0;

/// The center on the sphere of `geoms`, which edits beyond Web Mercator's square move, turn and
/// scale about. It's the center of their lines and rings, each edge (as the analysis plane draws
/// it) weighed by length on the sphere in stretches of at most half a degree, so a vertex added
/// along an edge hardly moves it; with no edges of any length, of their vertices (their points).
/// The stretches come out of the command's `steps` ([`CENTER_STEPS_TOTAL`]); once they run out, an
/// edge is weighed in one, and counted in `steps.coarse`. A center at a pole takes its directions
/// from the prime meridian, however the features' longitudes are written. `None` when they balance
/// out (or have no finite coordinate).
fn plane_at<'a>(geoms: impl IntoIterator<Item = &'a Geometry>, steps: &mut CenterSteps) -> Option<Coord> {
    let (mut edges, mut vertices) = (SphericalMean::default(), SphericalMean::default());
    for g in geoms {
        g.for_each_coord(|c| vertices.add(c));
        if matches!(g, Geometry::Point(_) | Geometry::MultiPoint(_)) {
            continue;
        }
        g.for_each_edge(|_, a, b| {
            // An edge to a vertex missing a coordinate, or with an infinite one, weighs nothing
            // (an infinite latitude would otherwise reach a pole in the plane).
            if !(a.is_finite() && b.is_finite()) {
                return;
            }
            let span = (b.x - a.x).abs().max((b.y - a.y).abs()) / CENTER_STEP_DEG;
            let n = if span > 1.0 { span.ceil().min(CENTER_STEPS_MAX) } else { 1.0 };
            let n = if n as usize <= steps.left {
                n
            } else {
                steps.coarse += usize::from(n > 1.0);
                1.0
            };
            steps.left = steps.left.saturating_sub(n as usize);
            let (pa, pb) = (lonlat_to_plane(a), lonlat_to_plane(b));
            let at = |t: f64| plane_to_lonlat(Coord::new(pa.x + (pb.x - pa.x) * t, pa.y + (pb.y - pa.y) * t));
            let mut last = a;
            for k in 1..=n as usize {
                let q = if k as f64 >= n { b } else { at(k as f64 / n) };
                edges.add_weighted(at((k as f64 - 0.5) / n), haversine_m(last, q));
                last = q;
            }
        });
    }
    // A feature whose edges all have no length (one point, repeated) is centered on its vertices.
    let c = edges.center().or_else(|| vertices.center())?;
    Some(if c.y.abs() >= 90.0 - POLE_TOLERANCE { Coord::new(0.0, 90f64.copysign(c.y)) } else { c })
}

/// `g` turned or scaled about its center: in Web Mercator, as always, by `merc` (taking the
/// center of `g`'s extent and a point, both in Web Mercator) while `g` and its result stay within
/// the square; otherwise on the ground (#1033), by `ground` (taking a point's meters east and north
/// of the feature's center, [`plane_at`], found within the command's `steps`).
fn about_center(
    g: &Geometry,
    merc: impl Fn(Coord, Coord) -> Coord,
    ground: impl Fn(Coord) -> Coord,
    steps: &mut CenterSteps,
) -> Result<Option<Geometry>, String> {
    if within_square(g) {
        // About the extent of the vertices that have both coordinates; one missing a coordinate
        // stays as it was, as on the ground.
        let c = lonlat_to_merc(g.extent().center());
        let done = g.map_coords(&|p: Coord| if p.is_finite() { merc_to_lonlat(merc(c, lonlat_to_merc(p))) } else { p });
        if within_square(&done) {
            return Ok(Some(done));
        }
    }
    ground_movable(g)?;
    let plane = Aeqd::new(plane_at(std::iter::once(g), steps).ok_or("the feature has no center to turn or scale it about")?);
    polar_transform(g, (&plane, &plane), None, ground).map(Some)
}

/// For a move, the center each selected feature moves about on the ground (#1033), by its place
/// in the selection (layer, row). Selected features that share a vertex move together: a group
/// whose features all `stay` within Web Mercator's square moves there, as always, and isn't in the
/// map; any other moves rigidly about its own center ([`plane_at`]), so shared vertices stay
/// shared, and groups apart move apart (a feature far from a pole isn't pulled along by one near
/// it, and features near both poles each have a center). A move on the ground goes at most
/// [`GROUND_REACH_M`] (`reach_m` is how far this one goes), and only features it can carry
/// ([`ground_movable`]). With the map, how many vertices move on the ground.
fn move_planes(
    s: &Session,
    stays: impl Fn((u32, usize)) -> bool,
    reach_m: f64,
) -> Result<(HashMap<(u32, usize), Coord>, usize, usize), String> {
    let selected: Vec<((u32, usize), &Geometry)> = s
        .map()
        .layers
        .iter()
        .filter(|l| !l.is_raster())
        .flat_map(|l| l.selection.iter().filter_map(move |r| Some(((l.id, *r), l.dataset.features.get(*r)?.geometry.as_ref()?))))
        .collect();
    // Most moves stay within the square: then no feature needs grouping.
    let stay: Vec<bool> = selected.iter().map(|(key, _)| stays(*key)).collect();
    if stay.iter().all(|s| *s) {
        return Ok((HashMap::new(), 0, 0));
    }
    if reach_m > GROUND_REACH_M {
        return Err("a move on the ground (beyond or across 85.05°) can go at most 10,000 km at once".into());
    }
    // A feature leaving the square that can't move on the ground is refused before the selection
    // is grouped (those joined to it are checked once grouped).
    for ((_, g), stays) in selected.iter().zip(&stay) {
        if !stays {
            ground_movable(g)?;
        }
    }
    // Each feature's group: the lowest feature it's joined to through shared vertices, found by
    // following `group` up (union-find, pointing each feature on the way at the one above its own).
    let mut group: Vec<usize> = (0..selected.len()).collect();
    let find = |group: &mut [usize], mut i: usize| {
        while let Some(&up) = group.get(i) {
            if up == i {
                break;
            }
            let above = group.get(up).copied().unwrap_or(up);
            if let Some(g) = group.get_mut(i) {
                *g = above;
            }
            i = up;
        }
        i
    };
    let mut owner: HashMap<(i64, i64), usize> = HashMap::new();
    let vertices = selected.iter().map(|(_, g)| g.coord_count()).sum();
    owner.try_reserve(vertices).map_err(|_| "not enough memory to find which selected features share vertices")?;
    for (i, (_, g)) in selected.iter().enumerate() {
        let mut shared = Vec::new();
        g.for_each_coord(|c| {
            if c.is_finite() {
                match owner.entry(vertex_key(c)) {
                    std::collections::hash_map::Entry::Occupied(o) => shared.push(*o.get()),
                    std::collections::hash_map::Entry::Vacant(v) => {
                        v.insert(i);
                    }
                }
            }
        });
        for o in shared {
            let (a, b) = (find(&mut group, o), find(&mut group, i));
            if let Some(g) = group.get_mut(a.max(b)) {
                *g = a.min(b);
            }
        }
    }
    // In selection order, so the budget is spent the same way every time.
    let mut members: std::collections::BTreeMap<usize, Vec<usize>> = std::collections::BTreeMap::new();
    for i in 0..selected.len() {
        members.entry(find(&mut group, i)).or_default().push(i);
    }
    let (mut planes, mut ground, mut steps) = (HashMap::new(), 0usize, CenterSteps::new());
    for ids in members.values() {
        let geoms = || ids.iter().filter_map(|i| selected.get(*i).map(|(_, g)| *g));
        if ids.iter().all(|i| stay.get(*i).copied().unwrap_or(true)) {
            continue;
        }
        for i in ids {
            let Some((_, g)) = selected.get(*i) else { continue };
            // Those leaving the square were checked above.
            if stay.get(*i).copied().unwrap_or(true) {
                ground_movable(g)?;
            }
            ground += g.coord_count();
        }
        let plane = plane_at(geoms(), &mut steps).ok_or("the selected features have no center to move them about")?;
        planes.extend(ids.iter().filter_map(|i| selected.get(*i).map(|(key, _)| (*key, plane))));
    }
    Ok((planes, ground, steps.coarse))
}

/// How near a pole (meters) a moved line or ring may come: nearer, it's taken to reach it.
const POLE_CLEARANCE_M: f64 = 0.02;

/// What [`polar_transform`] moves at once: a feature's point, a line's part, or a polygon's ring.
#[derive(Clone, Copy, PartialEq)]
enum PathKind {
    Point,
    Line,
    Ring,
}

/// `g` moved, turned or scaled on the ground (#1033): `f` takes each vertex's meters east and north
/// of the center in `from`, an azimuthal equidistant plane, and the result is placed back from
/// `to` (the same plane, or for a move one at the moved center). Vertices are moved one by one,
/// which a line or ring can't survive where it reaches or goes around a pole: callers refuse first
/// what [`ground_movable`] refuses, and here one that would be carried onto or around a pole is
/// refused, as is any feature carried farther than [`GROUND_REACH_M`] from the center. Longitudes
/// run on across ±180° as the vertices do, each point, and each part's and shell's first vertex,
/// in the range nearest where it was (so a feature is written as before, not a turn away), and a
/// polygon's holes in its shell's; but a vertex `placed` already (shared with a feature moved
/// before) lands exactly where it did, and a line or ring follows the first it has by whole turns
/// (another it shares, written a turn apart in the two features, stays a turn apart, as written).
/// A vertex missing a coordinate can't be placed on the ground: it stays as it was. Edges stay
/// straight in longitude/latitude between the moved vertices.
fn polar_transform(
    g: &Geometry,
    (from, to): (&Aeqd, &Aeqd),
    mut placed: Option<&mut HashMap<(i64, i64), Coord>>,
    f: impl Fn(Coord) -> Coord,
) -> Result<Geometry, String> {
    use rustgis_data::geometry::{ring_contains, seg_dist2};
    let poles = [to.forward(Coord::new(0.0, 90.0)), to.forward(Coord::new(0.0, -90.0))];
    // A path moved, back in longitude/latitude, its first vertex in the range nearest the
    // longitude `anchor` (a hole's), or else where it was.
    let mut path = |pts: &[Coord], kind: PathKind, anchor: Option<f64>| -> Result<Vec<Coord>, String> {
        let mut out = Vec::with_capacity(pts.len());
        for c in pts {
            // A vertex missing a coordinate, or with an infinite one, is no place: it isn't
            // measured here, and stays as it was below.
            let p = if c.is_finite() { f(from.forward(*c)) } else { Coord::new(f64::NAN, f64::NAN) };
            if p.x.hypot(p.y) > GROUND_REACH_M {
                return Err("features beyond 85.05° are edited on the ground about their center, and that would carry one more \
                            than 10,000 km from it"
                    .into());
            }
            out.push(p);
        }
        let near = |pole: Coord| seg_dist2(&out, pole) <= POLE_CLEARANCE_M * POLE_CLEARANCE_M;
        if kind != PathKind::Point && poles.iter().any(|pole| near(*pole) || (kind == PathKind::Ring && ring_contains(&out, *pole))) {
            return Err("that would carry a line or polygon onto or around a pole, which it can't cross yet".into());
        }
        for (v, was) in out.iter_mut().zip(pts) {
            *v = if was.is_finite() { to.inverse(*v) } else { *was };
        }
        // Each vertex's whole turns: a point's nearest where it was; a line's or ring's carried on
        // from the vertex before (the last placed: one missing a coordinate is left out), not
        // wrapped at ±180°, from its first vertex's.
        let mut whole: Vec<f64> = Vec::with_capacity(out.len());
        let mut before: Option<f64> = None;
        for (c, was) in out.iter().zip(pts) {
            let near = if kind == PathKind::Point { was.x } else { before.or(anchor).unwrap_or(was.x) };
            let t = if was.is_finite() { turns(c.x, near) } else { 0.0 };
            if was.is_finite() {
                before = Some(c.x + t);
            }
            whole.push(t);
        }
        if let Some(placed) = placed.as_deref_mut() {
            // A line or ring follows its first vertex placed before by whole turns.
            let aligned = if kind == PathKind::Point {
                0.0
            } else {
                pts.iter()
                    .zip(&out)
                    .zip(&whole)
                    .find_map(|((c, p), t)| placed.get(&vertex_key(*c)).filter(|_| c.is_finite()).map(|was| turns(p.x + t, was.x)))
                    .unwrap_or(0.0)
            };
            for ((p, t), at) in out.iter_mut().zip(whole).zip(pts) {
                if !at.is_finite() {
                    continue;
                }
                p.x += t + aligned;
                // A vertex placed before lands exactly there (written perhaps as 180° there and
                // −180° here), when it's the same place to within rounding, or for a point whole
                // turns away.
                let was = *placed.entry(vertex_key(*at)).or_insert(*p);
                let off = if kind == PathKind::Point { turns(p.x, was.x) } else { 0.0 };
                if (was.x - p.x - off).abs() < 1e-9 && (was.y - p.y).abs() < 1e-9 {
                    *p = was;
                }
            }
        } else {
            out.iter_mut().zip(whole).for_each(|(c, t)| c.x += t);
        }
        // Written straight in longitude/latitude, a line or ring must still be what was moved: no
        // edge spanning 180° or more (it would run around the other side of a pole), and a closed
        // ring closed. An edge to a vertex missing a coordinate, or with an infinite one, left as
        // it was, is no edge here.
        let wide = out.windows(2).any(|w| matches!(w, [a, b] if a.is_finite() && b.is_finite() && (b.x - a.x).abs() >= 180.0));
        let opened = kind == PathKind::Ring && pts.first() == pts.last() && out.first() != out.last();
        if kind != PathKind::Point && (wide || opened) {
            return Err("that would carry a line or polygon onto or around a pole, which it can't cross yet".into());
        }
        Ok(out)
    };
    Ok(match g {
        Geometry::Point(c) => Geometry::Point(path(std::slice::from_ref(c), PathKind::Point, None)?.first().copied().unwrap_or(*c)),
        Geometry::MultiPoint(v) => Geometry::MultiPoint(path(v, PathKind::Point, None)?),
        Geometry::Line(parts) => Geometry::Line(parts.iter().map(|p| path(p, PathKind::Line, None)).collect::<Result<_, _>>()?),
        Geometry::Polygon(polys) => {
            let mut moved = Vec::with_capacity(polys.len());
            for rings in polys {
                // Its holes go in its shell's range, nearest the middle of the shell's longitudes,
                // however far the shell runs on or follows a shared vertex.
                let (mut out, mut anchor): (Vec<Vec<Coord>>, Option<f64>) = (Vec::with_capacity(rings.len()), None);
                for ring in rings {
                    let ring = path(ring, PathKind::Ring, anchor)?;
                    if out.is_empty() {
                        let (lo, hi) = ring
                            .iter()
                            .filter(|c| c.x.is_finite())
                            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), c| (lo.min(c.x), hi.max(c.x)));
                        if lo <= hi {
                            anchor = Some((lo + hi) / 2.0);
                        }
                    }
                    out.push(ring);
                }
                moved.push(out);
            }
            Geometry::Polygon(moved)
        }
    })
}

/// [`modify`] for a move, turn or scale, which leaves a vertex missing a coordinate (or with an
/// infinite one) where it was: says how many of the features it changed have one.
fn move_features(s: &mut Session, mut change: impl FnMut((u32, usize), &Geometry) -> Result<Option<Geometry>, String>) -> CmdResult {
    let mut missing = 0usize;
    let done = modify(s, |key, g| {
        let moved = change(key, g)?;
        if moved.is_some() {
            let mut has = false;
            g.for_each_coord(|c| has |= !c.is_finite());
            missing += usize::from(has);
        }
        Ok(moved)
    })?;
    if missing > 0 {
        s.warn(format!("{missing} feature(s) have a vertex missing a coordinate or with an infinite one, which was left where it was"));
    }
    Ok(done)
}

/// Applies a geometry change to every selected feature of every layer (in the selection's layers).
fn modify(s: &mut Session, mut change: impl FnMut((u32, usize), &Geometry) -> Result<Option<Geometry>, String>) -> CmdResult {
    let mut n = 0;
    let ids: Vec<u32> = s.map().layers.iter().filter(|l| !l.selection.is_empty() && !l.is_raster()).map(|l| l.id).collect();
    for id in ids {
        let mut changes = Vec::new();
        let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
        for r in l.selection.clone() {
            let Some(f) = l.dataset.features.get_mut(r) else { continue };
            let Some(g) = f.geometry.as_ref().map(|g| change((id, r), g)).transpose()?.flatten() else { continue };
            super::edit::validate(&g)?;
            let before = f.clone();
            f.geometry = Some(g);
            changes.push(Change { before: Some(before), row: Some(r) });
            n += 1;
        }
        l.touch_geometry();
        apply(s, id, changes)?;
    }
    if n == 0 {
        return Err("nothing selected can be changed this way".into());
    }
    Ok(json!({"modified": n}))
}

fn set_geometry(s: &mut Session, p: &Json) -> CmdResult {
    let id = s.layer_id(p)?;
    let row = p.get("row").and_then(Json::as_u64).ok_or("row is required")? as usize;
    let g = super::edit::geometry_param(p)?;
    replace_geometry(s, id, row, g)?;
    Ok(Json::Null)
}

/// Moves, inserts or deletes one vertex of a feature: one edit, through the same checks and undo
/// as `edit.setGeometry`.
fn edit_vertex(s: &mut Session, p: &Json) -> CmdResult {
    let id = s.layer_id(p)?;
    let row = p.get("row").and_then(Json::as_u64).ok_or("row is required")? as usize;
    // Absent is 0 (part and ring); anything else must be a whole number, never quietly 0.
    let num = |k: &str| -> Result<Option<usize>, String> {
        match p.get(k) {
            None | Some(Json::Null) => Ok(None),
            Some(v) => v.as_u64().map(|n| Some(n as usize)).ok_or_else(|| format!("{k} must be a whole number")),
        }
    };
    let v = rustgis_data::VertexId::new(num("part")?.unwrap_or(0), num("ring")?.unwrap_or(0), num("index")?.ok_or("index is required")?);
    let at = || -> Result<Coord, String> {
        let (lon, lat) = (f(p, "lon").ok_or("lon is required")?, f(p, "lat").ok_or("lat is required")?);
        if !(lon.is_finite() && lat.is_finite()) || lon.abs() > 360.0 || lat.abs() > 90.0 {
            return Err("lon and lat are degrees".into());
        }
        Ok(Coord::new(lon, lat))
    };
    let g = s
        .map()
        .layer(id)
        .and_then(|l| l.dataset.features.get(row))
        .ok_or("no such row")?
        .geometry
        .clone()
        .ok_or("the feature has no shape")?;
    let (g, v) = match ps(p, "action").ok_or("action is required")? {
        "move" => (g.move_vertex(v, at()?).ok_or("no such vertex")?, v),
        "insert" => {
            let g = g.insert_vertex(v, at()?).ok_or("vertices can only be inserted in lines and polygons")?;
            (g, rustgis_data::VertexId::new(v.part, v.ring, v.index + 1))
        }
        "delete" => (g.delete_vertex(v)?, v),
        a => return Err(format!("unknown action '{a}'")),
    };
    replace_geometry(s, id, row, g)?;
    Ok(json!({"part": v.part, "ring": v.ring, "index": v.index}))
}

/// Replaces a feature's shape with one of the layer's kind, as one edit.
fn replace_geometry(s: &mut Session, id: u32, row: usize, g: Geometry) -> Result<(), String> {
    super::edit::validate(&g)?;
    let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
    if l.geometry_type() != GeometryType::None && l.geometry_type() != g.kind() {
        return Err(format!("a {} can't replace a {}", g.kind().label(), l.geometry_type().label()));
    }
    let f = l.dataset.features.get_mut(row).ok_or("no such row")?;
    let before = f.clone();
    f.geometry = Some(g);
    l.touch_geometry();
    apply(s, id, vec![Change { before: Some(before), row: Some(row) }])
}

/// The one layer holding the selection.
fn selection_layer(s: &Session) -> Result<u32, String> {
    let ls: Vec<u32> = s.map().layers.iter().filter(|l| !l.selection.is_empty()).map(|l| l.id).collect();
    match ls.as_slice() {
        [id] => Ok(*id),
        [] => Err("nothing is selected".into()),
        _ => Err("select features in one layer only".into()),
    }
}

fn merge(s: &mut Session, _: &Json) -> CmdResult {
    let id = selection_layer(s)?;
    let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
    let mut sel = l.selection.clone();
    sel.sort_unstable();
    sel.dedup();
    if sel.len() < 2 {
        return Err("select two or more features to merge".into());
    }
    let geoms: Vec<Geometry> = sel.iter().filter_map(|r| l.dataset.features.get(*r)?.geometry.clone()).collect();
    let kind = |g: &Geometry| match g {
        Geometry::Point(_) | Geometry::MultiPoint(_) => 0,
        Geometry::Line(_) => 1,
        Geometry::Polygon(_) => 2,
    };
    if geoms.len() != sel.len() || geoms.windows(2).any(|w| w.first().map(kind) != w.get(1).map(kind)) {
        return Err("merge features of one kind (all points, all lines or all polygons), each with a shape".into());
    }
    let merged = rustgis_analysis::topology::merge(&geoms).ok_or("these features can't be merged")?;
    let keep = sel.first().copied().ok_or("nothing selected")?;
    let mut changes = Vec::new();
    for r in sel.iter().skip(1).rev() {
        if *r < l.dataset.features.len() {
            changes.push(Change { before: Some(l.dataset.features.remove(*r)), row: None });
        }
    }
    let f = l.dataset.features.get_mut(keep).ok_or("no such row")?;
    let before = f.clone();
    f.geometry = Some(merged);
    changes.push(Change { before: Some(before), row: Some(keep) });
    l.selection = vec![keep];
    l.touch_geometry();
    apply(s, id, changes)?;
    Ok(json!({"merged": sel.len()}))
}

fn explode(s: &mut Session, _: &Json) -> CmdResult {
    let id = selection_layer(s)?;
    let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
    let mut changes = Vec::new();
    let mut new_rows = Vec::new();
    let mut sel = l.selection.clone();
    sel.sort_unstable();
    sel.dedup();
    for r in sel {
        let Some(f) = l.dataset.features.get(r).cloned() else { continue };
        let parts = f.geometry.as_ref().map(rustgis_analysis::topology::explode).unwrap_or_default();
        if parts.len() < 2 {
            continue;
        }
        let mut it = parts.into_iter();
        if let (Some(first), Some(feat)) = (it.next(), l.dataset.features.get_mut(r)) {
            feat.geometry = Some(first);
            changes.push(Change { before: Some(f.clone()), row: Some(r) });
        }
        for g in it {
            l.dataset.push(Some(g), f.attrs.clone());
            let nr = l.dataset.features.len() - 1;
            new_rows.push(nr);
            changes.push(Change { before: None, row: Some(nr) });
        }
    }
    if changes.is_empty() {
        return Err("no selected feature has several parts".into());
    }
    l.selection.extend(new_rows);
    l.touch_geometry();
    let n = changes.len();
    apply(s, id, changes)?;
    Ok(json!({"features": n}))
}

/// Splits the selected lines at the point on them nearest to (lon, lat).
fn split(s: &mut Session, p: &Json) -> CmdResult {
    let at = Coord::new(f(p, "lon").ok_or("lon is required")?, f(p, "lat").ok_or("lat is required")?);
    if !(at.is_finite() && at.y.abs() <= 90.0) {
        return Err("lon and lat are degrees".into());
    }
    let id = selection_layer(s)?;
    let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
    // A vertex past ±90° is no place: the search would take it for the pole, so the lines are
    // refused up front, before any is split.
    let mut beyond_pole = false;
    for feat in l.selection.iter().filter_map(|r| l.dataset.features.get(*r)) {
        if let Some(Geometry::Line(parts)) = &feat.geometry {
            beyond_pole |= parts.iter().flatten().any(|c| c.is_finite() && c.y.abs() > 90.0);
        }
    }
    if beyond_pole {
        return Err("a selected line has a latitude past ±90°, which is no place".into());
    }
    let mut changes = Vec::new();
    let mut steps = SplitSteps { left: SPLIT_STEPS_TOTAL, floor: f64::INFINITY };
    // Lines the search, past its steps, couldn't search in full where a nearer point might lie:
    // split at the nearest point it found, or left whole when that was an end.
    let (mut rough, mut whole) = (0, 0);
    let mut unmeasured = 0usize;
    for r in l.selection.clone() {
        let Some(f) = l.dataset.features.get(r).cloned() else { continue };
        let Some(Geometry::Line(parts)) = &f.geometry else { continue };
        steps.floor = f64::INFINITY;
        // The nearest segment of the nearest part, in the analysis plane (Web Mercator within
        // ±85.05°, not clamped beyond it, #1033). Where the point asked or the line reaches beyond
        // 85.05°, where the plane stretches east–west far more than north–south, each segment's
        // nearest point is found, and compared, on the ground.
        let m = lonlat_to_plane(at);
        // A vertex missing a coordinate, or with an infinite one, is no place: it decides nothing.
        let ground = at.y.abs() > MAX_LAT || parts.iter().flatten().any(|c| c.is_finite() && c.y.abs() > MAX_LAT);
        // On the ground, each vertex's distance, measured once (part after part, in one buffer),
        // where the plane has it (a latitude past ±90° at the pole). The line's nearest point is no
        // farther than the nearest end of a segment it can measure (both ends known), so a segment
        // that can't come nearer than that (or than a point already found) isn't searched.
        let mut dists: Vec<f64> = Vec::new();
        if ground {
            dists.try_reserve(parts.iter().map(Vec::len).sum()).map_err(|_| "not enough memory to split the selected lines")?;
            dists.extend(
                parts
                    .iter()
                    .flatten()
                    .map(|c| if c.is_finite() { haversine_m(at, Coord::new(c.x, c.y.clamp(-90.0, 90.0))) } else { f64::NAN }),
            );
        }
        let (mut nearest_vertex, mut base) = (f64::INFINITY, 0);
        for p in parts {
            for ends in dists.get(base..base + p.len()).unwrap_or_default().windows(2).filter(|e| e.iter().all(|d| d.is_finite())) {
                nearest_vertex = ends.iter().fold(nearest_vertex, |n, d| n.min(*d));
            }
            base += p.len();
        }
        let (mut best, mut base): (Option<(f64, usize, usize, Coord)>, usize) = (None, 0);
        for (pi, part) in parts.iter().enumerate() {
            let part_dists = dists.get(base..base + part.len()).unwrap_or_default();
            base += part.len();
            for (si, w) in part.windows(2).enumerate() {
                let (Some(a), Some(b2)) = (w.first(), w.get(1)) else { continue };
                // A segment to a vertex that's no place (an infinite coordinate; a missing one
                // measures as NaN below) is passed over, not run to a pole.
                if !(a.is_finite() && b2.is_finite()) {
                    unmeasured += 1;
                    continue;
                }
                let (d, q) = if ground {
                    let ends = part_dists.get(si).zip(part_dists.get(si + 1)).map(|(da, db)| (*da, *db));
                    let bound = best.map_or(nearest_vertex, |b| b.0.min(nearest_vertex));
                    let Some(near) = ends.and_then(|ends| nearest_on_ground(at, (*a, *b2), ends, bound, &mut steps)) else { continue };
                    near
                } else {
                    let (a, b2) = (lonlat_to_plane(*a), lonlat_to_plane(*b2));
                    let (dx, dy) = (b2.x - a.x, b2.y - a.y);
                    let len2 = dx * dx + dy * dy;
                    let t = if len2 > 0.0 { (((m.x - a.x) * dx + (m.y - a.y) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
                    let q = Coord::new(a.x + t * dx, a.y + t * dy);
                    ((q.x - m.x).powi(2) + (q.y - m.y).powi(2), plane_to_lonlat(q))
                };
                // A segment missing a coordinate (its distance NaN) is passed over.
                if !d.is_finite() {
                    unmeasured += 1;
                } else if best.is_none_or(|b| d < b.0) {
                    best = Some((d, pi, si, q));
                }
            }
        }
        let Some((d, pi, si, q)) = best else { continue };
        let Some(part) = parts.get(pi) else { continue };
        // A part of a segment the search couldn't look at for want of steps might hold a point
        // nearer than the one found.
        let unsure = d > steps.floor;
        // At a vertex (to within a millimeter), the parts meet there, without it repeated.
        let at_end = [part.get(si), part.get(si + 1)].into_iter().flatten().find(|v| rustgis_geom::haversine_m(**v, q) < 0.001);
        let q = at_end.copied().unwrap_or(q);
        let mut first: Vec<Coord> = part.iter().take(si + 1).copied().collect();
        if first.last() != Some(&q) {
            first.push(q);
        }
        let mut second = vec![q];
        second.extend(part.iter().skip(si + 1).copied().skip_while(|c| *c == q));
        // At (or within a centimetre of) the line's ends isn't a split.
        let near = |a: Option<&Coord>| a.is_some_and(|a| rustgis_geom::haversine_m(*a, q) < 0.01);
        if first.len() < 2 || second.len() < 2 || near(first.first()) || near(second.last()) {
            whole += usize::from(unsure);
            continue;
        }
        let mut a_parts: Vec<Vec<Coord>> = parts.iter().take(pi).cloned().collect();
        a_parts.push(first);
        let mut b_parts = vec![second];
        b_parts.extend(parts.iter().skip(pi + 1).cloned());
        if let Some(feat) = l.dataset.features.get_mut(r) {
            feat.geometry = Some(Geometry::Line(a_parts));
            changes.push(Change { before: Some(f.clone()), row: Some(r) });
        }
        l.dataset.push(Some(Geometry::Line(b_parts)), f.attrs.clone());
        changes.push(Change { before: None, row: Some(l.dataset.features.len() - 1) });
        rough += usize::from(unsure);
    }
    let unsearched =
        |n: usize| format!("{n} line(s) near a pole couldn't be searched in full within one split's limit of a million distances");
    if changes.is_empty() {
        if unmeasured > 0 {
            return Err(format!(
                "no line could be split away from its ends; {unmeasured} segment(s) could not be measured because of invalid coordinates"
            ));
        }
        if whole > 0 {
            return Err(format!("{}: the nearest point found was at an end; select fewer lines, or simplify them", unsearched(whole)));
        }
        return Err("select a line to split (away from its ends)".into());
    }
    l.touch_geometry();
    apply(s, id, changes)?;
    if unmeasured > 0 {
        s.warn(format!("{unmeasured} segment(s) of the selected lines could not be measured because of invalid coordinates; the split search skipped them, so the point found may not be the nearest"));
    }
    if rough > 0 {
        s.warn(format!("{}: each was split at the nearest point found, which may not be the nearest", unsearched(rough)));
    }
    if whole > 0 {
        s.warn(format!("{} and were left whole: the nearest point found was at an end", unsearched(whole)));
    }
    Ok(json!({"split": true}))
}

/// How many ground distances one split may work out along segments (a million, as its notices
/// say); past them, a segment counts as near as the nearest point found on it so far, its nearer
/// end at first.
const SPLIT_STEPS_TOTAL: usize = 1_000_000;

/// The ground distances a split may still work out ([`SPLIT_STEPS_TOTAL`] at first), and, for the
/// line being searched, the least distance a part of a segment it couldn't search for want of them
/// might lie at.
struct SplitSteps {
    left: usize,
    floor: f64,
}

/// The ground distance (meters) from `at` to the segment a–b, straight in the analysis plane, and
/// the point on it that distance away, or None when no point on it can be nearer than `bound` or
/// an end is missing a coordinate; `da` and `db` are its ends' distances. The segment is sampled
/// evenly in the plane, once for each 22.5° of longitude or latitude it spans (eight times at
/// least), without skipping turns; each sample no farther than its neighbors marks a basin (one each time
/// the segment passes `at`, as one going around a pole does), refined by golden-section search to
/// a millimeter, nearest first, while it could hold a point nearer than the nearest found, or than
/// `bound`. Each distance takes one of the command's `steps`; once they're spent, the nearest
/// found (the nearer end, at first) stands, and `steps` notes how near the rest might be.
fn nearest_on_ground(at: Coord, (a, b): (Coord, Coord), (da, db): (f64, f64), bound: f64, steps: &mut SplitSteps) -> Option<(f64, Coord)> {
    const STEP_DEG: f64 = 22.5;
    const PIECES_MIN: usize = 8;
    const GOLDEN_STEPS: usize = 60;
    if !(da.is_finite() && db.is_finite()) {
        return None;
    }
    let (pa, pb) = (lonlat_to_plane(a), lonlat_to_plane(b));
    let point = |t: f64| plane_to_lonlat(Coord::new(pa.x + t * (pb.x - pa.x), pa.y + t * (pb.y - pa.y)));
    // A stretch of the plane covers at most cos φ as much ground, φ its latitude nearest the
    // equator, and no more than 85.05° from it (beyond, the plane is stretched north–south as much
    // as there, and east–west more), on a sphere smaller than Web Mercator's. By the triangle
    // inequality, no point of the segment is nearer than half its ends' distances less its length.
    let lat = if a.y * b.y <= 0.0 { 0.0 } else { a.y.abs().min(b.y.abs()).min(MAX_LAT) };
    let length = lat.to_radians().cos() * (pb.x - pa.x).hypot(pb.y - pa.y);
    if (da + db - length) / 2.0 > bound {
        return None;
    }
    let mut best = if db < da { (db, b) } else { (da, a) };
    let span = (b.x - a.x).abs().max((b.y - a.y).abs()) / STEP_DEG;
    let pieces = if span > PIECES_MIN as f64 { span.ceil() as usize } else { PIECES_MIN };
    if pieces > steps.left {
        steps.floor = steps.floor.min((da + db - length) / 2.0);
        return Some(best);
    }
    steps.left -= pieces;
    let samples: Vec<f64> = (0..=pieces)
        .map(|i| match i {
            0 => da,
            i if i == pieces => db,
            i => haversine_m(at, point(i as f64 / pieces as f64)),
        })
        .collect();
    // A basin's least distance is no less than its sample's less two samples' spacing of ground.
    let spacing = length / pieces as f64;
    let mut basins: Vec<(f64, usize)> = samples
        .iter()
        .enumerate()
        .filter(|&(i, d)| {
            let (before, after) = (i.checked_sub(1).and_then(|j| samples.get(j)), samples.get(i + 1));
            before.is_none_or(|n| d <= n) && after.is_none_or(|n| d <= n) && !(before == Some(d) && after == Some(d))
        })
        .map(|(i, d)| (*d, i))
        .collect();
    basins.sort_by(|x, y| x.0.total_cmp(&y.0));
    let golden = (5f64.sqrt() - 1.0) / 2.0;
    let dist = |t: f64| haversine_m(at, point(t));
    for (d, i) in basins {
        let t = i as f64 / pieces as f64;
        if d < best.0 {
            best = (d, point(t));
        }
        if d - 2.0 * spacing >= best.0.min(bound) {
            break;
        }
        if GOLDEN_STEPS + 3 > steps.left {
            // This basin and the farther ones left unsearched.
            steps.floor = steps.floor.min(d - 2.0 * spacing);
            break;
        }
        let (mut lo, mut hi) = ((t - 1.0 / pieces as f64).max(0.0), (t + 1.0 / pieces as f64).min(1.0));
        let (mut c, mut e) = (hi - golden * (hi - lo), lo + golden * (hi - lo));
        let (mut dc, mut de) = (dist(c), dist(e));
        let mut spent = 3;
        for _ in 0..GOLDEN_STEPS {
            if (hi - lo) * length < 1e-3 {
                break;
            }
            spent += 1;
            if dc < de {
                (hi, e, de) = (e, c, dc);
                c = hi - golden * (hi - lo);
                dc = dist(c);
            } else {
                (lo, c, dc) = (c, e, de);
                e = lo + golden * (hi - lo);
                de = dist(e);
            }
        }
        steps.left -= spent;
        let q = point((lo + hi) / 2.0);
        let d = haversine_m(at, q);
        if d < best.0 {
            best = (d, q);
        }
    }
    Some(best)
}

/// Draws a line (or a closed polygon) from a start point by distances and bearings, corrected by
/// ground to grid when it's on.
fn traverse(s: &mut Session, p: &Json) -> CmdResult {
    let start = p
        .get("start")
        .and_then(Json::as_array)
        .and_then(|a| Some(Coord::new(a.first()?.as_f64()?, a.get(1)?.as_f64()?)))
        .ok_or("start is [lon, lat]")?;
    if !start.is_finite() || start.y.abs() > 85.0 {
        return Err("start is [lon, lat]".into());
    }
    let legs = p.get("legs").and_then(Json::as_array).ok_or("legs is required")?;
    if legs.is_empty() || legs.len() > 10_000 {
        return Err("give 1 to 10,000 legs".into());
    }
    let unit = distance_unit_metres(p)?;
    let g2g = s.map().editing.settings.ground_to_grid;
    let mut pts = vec![start];
    for leg in legs {
        let d = leg
            .get("distance")
            .and_then(Json::as_f64)
            .map(|d| d * unit)
            .filter(|d| d.is_finite() && *d >= 0.0 && *d < 2e7)
            .ok_or("each leg has a finite, non-negative distance in the command's units (limit: less than 20,000 km)")?;
        let brg = leg.get("bearing").and_then(Json::as_f64).filter(|b| b.is_finite()).ok_or("each leg has a bearing in degrees")?;
        let (mut dx, mut dy) = (d * brg.to_radians().sin(), d * brg.to_radians().cos());
        if let Some((k, rot)) = g2g {
            (dx, dy) = ground_to_grid(dx, dy, k, rot);
        }
        let last = pts.last().copied().unwrap_or(start);
        pts.push(shift(last, dx, dy));
    }
    let id = s.layer_id(p)?;
    let gt = s.map().layer(id).map(|l| l.geometry_type()).ok_or("no such layer")?;
    // The layer decides: a polygon layer gets a closed traverse, a line layer an open one.
    let g = match gt {
        GeometryType::Polygon => {
            pts.push(start);
            json!({"type": "Polygon", "coordinates": [pts.iter().map(|c| [c.x, c.y]).collect::<Vec<_>>()]})
        }
        GeometryType::Polyline => json!({"type": "LineString", "coordinates": pts.iter().map(|c| [c.x, c.y]).collect::<Vec<_>>()}),
        _ => return Err("traverses go into a line or polygon layer".into()),
    };
    super::edit::add_feature(s, &json!({"layer": id, "geometry": g}))
}

fn domain(s: &mut Session, p: &Json) -> CmdResult {
    let id = s.layer_id(p)?;
    let field = ps(p, "field").ok_or("field is required")?;
    let l = s.map().layer(id).ok_or("no such layer")?;
    let fi = l.dataset.field_index(field).ok_or_else(|| format!("no field named '{field}'"))?;
    let (fname, ty) = l.dataset.fields.get(fi).map(|f| (f.name.clone(), f.ty)).ok_or("no field")?;
    if b(p, "clear").unwrap_or(false) {
        let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
        l.design.domains.retain(|d| !d.field.eq_ignore_ascii_case(&fname));
        return Ok(Json::Null);
    }
    let kind = if let Some(codes) = p.get("coded").and_then(Json::as_array) {
        let pairs: Vec<(Value, String)> = codes
            .iter()
            .take(10_000)
            .filter_map(|c| {
                let code = super::edit::coerce(Value::from_json(c.get(0)?), ty);
                let desc = c.get(1).and_then(Json::as_str).map(String::from).unwrap_or_else(|| code.to_string());
                (!code.is_null()).then_some((code, desc))
            })
            .collect();
        if pairs.is_empty() {
            return Err("coded is a list of [code, description] pairs".into());
        }
        DomainKind::Coded(pairs)
    } else {
        let (min, max) = (f(p, "min").ok_or("give coded values, or min and max")?, f(p, "max").ok_or("max is required")?);
        if !ty.is_numeric() {
            return Err(format!("{fname} isn't numeric, so it takes coded values"));
        }
        if min > max {
            return Err("min is above max".into());
        }
        DomainKind::Range { min, max }
    };
    let d = Domain { field: fname.clone(), kind };
    // Existing values must already fit.
    let bad = (0..l.dataset.features.len()).filter(|r| !d.allows(l.dataset.value(*r, fi))).count();
    if bad > 0 && !b(p, "force").unwrap_or(false) {
        return Err(format!("{bad} existing values don't fit the domain (pass force to set it anyway)"));
    }
    let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
    l.design.domains.retain(|x| !x.field.eq_ignore_ascii_case(&fname));
    l.design.domains.push(d);
    Ok(json!({"field": fname, "outside": bad}))
}

fn subtypes(s: &mut Session, p: &Json) -> CmdResult {
    let id = s.layer_id(p)?;
    if b(p, "clear").unwrap_or(false) {
        let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
        l.design.subtypes = None;
        return Ok(Json::Null);
    }
    let field = ps(p, "field").ok_or("field is required")?;
    let l = s.map().layer(id).ok_or("no such layer")?;
    let fi = l.dataset.field_index(field).ok_or_else(|| format!("no field named '{field}'"))?;
    let f = l.dataset.fields.get(fi).ok_or("no field")?;
    if f.ty != FieldType::Integer {
        return Err(format!("{} must be an integer field to hold subtype codes", f.name));
    }
    let fname = f.name.clone();
    let list = p.get("subtypes").and_then(Json::as_array).ok_or("subtypes is required")?;
    let mut subs = Vec::new();
    for v in list.iter().take(1000) {
        let code = v.get("code").and_then(Json::as_i64).ok_or("each subtype has an integer code")?;
        let name = v.get("name").and_then(Json::as_str).ok_or("each subtype has a name")?.chars().take(200).collect();
        let defaults: Vec<(String, Value)> = v
            .get("defaults")
            .and_then(Json::as_object)
            .map(|o| o.iter().map(|(k, v)| (k.clone(), Value::from_json(v))).collect())
            .unwrap_or_default();
        for (field, value) in &defaults {
            if let Some(ty) = l.dataset.field_index(field).and_then(|i| l.dataset.fields.get(i)).map(|f| f.ty)
                && super::edit::non_finite_input(value, ty)
            {
                return Err(format!("field '{field}' requires a finite number"));
            }
        }
        if subs.iter().any(|x: &Subtype| x.code == code) {
            return Err(format!("subtype code {code} is used twice"));
        }
        subs.push(Subtype { code, name, defaults });
    }
    if subs.is_empty() {
        return Err("give at least one subtype".into());
    }
    let symbolize = b(p, "symbolize").unwrap_or(true);
    let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
    // Unique values by subtype code.
    let mut past = PastClasses::default();
    if symbolize {
        let base = l.renderer.base_symbol();
        // The classes come from every feature, so a definition query doesn't drop any; the
        // features past them are counted among those it lets through, which are drawn.
        let (r, rest) = rustgis_map::Renderer::unique_counted(&l.dataset, &[], &fname, rustgis_map::Ramp::default(), base)?;
        let rows = l.visible_rows();
        past = PastClasses { features: drawn_past(&l.dataset, &r, &fname, &rows), ..rest };
        l.renderer = r;
    }
    let n = subs.len();
    let name = l.name.clone();
    l.design.subtypes = Some(Subtypes { field: fname.clone(), subtypes: subs });
    if past.features > 0 {
        let (max, order) = (rustgis_map::symbology::MAX_UNIQUE, if past.by_number { "by number" } else { "in text order" });
        s.notify(
            format!(
                "{name}: {} features whose value is past the first {max} values of {fname} ({order}) are drawn as <all other values>",
                past.features
            ),
            false,
        );
    }
    Ok(json!({"subtypes": n}))
}

/// How many features `rows` keeps, with a geometry, have a value of `field` that is none of the
/// Unique Values `renderer`'s classes: those drawn as `<all other values>`.
fn drawn_past(ds: &rustgis_data::Dataset, renderer: &rustgis_map::Renderer, field: &str, rows: &[bool]) -> usize {
    let (rustgis_map::Renderer::Unique { classes, .. }, Some(fi)) = (renderer, ds.field_index(field)) else { return 0 };
    let classes: std::collections::HashSet<&str> = classes.iter().map(|(k, _)| k.as_str()).collect();
    ds.features
        .iter()
        .enumerate()
        .filter(|(r, f)| rows.get(*r).copied().unwrap_or(true) && f.geometry.is_some())
        // (Each value's text as `Dataset::unique_values` writes it, a missing one empty.)
        .filter(|(_, f)| !classes.contains(f.attrs.get(fi).map(|v| v.to_string()).unwrap_or_default().as_str()))
        .count()
}

fn attribute_rule(s: &mut Session, p: &Json) -> CmdResult {
    let id = s.layer_id(p)?;
    let name = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).ok_or("name is required")?.to_string();
    if b(p, "remove").unwrap_or(false) {
        let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
        let before = l.design.rules.len();
        l.design.rules.retain(|r| !r.name.eq_ignore_ascii_case(&name));
        return if l.design.rules.len() == before { Err(format!("no rule '{name}'")) } else { Ok(Json::Null) };
    }
    let kind = match ps(p, "kind").ok_or("kind is required")? {
        "calculation" => RuleKind::Calculation,
        "constraint" => RuleKind::Constraint,
        k => return Err(format!("unknown rule kind '{k}'")),
    };
    let expression = ps(p, "expression").map(str::trim).filter(|e| !e.is_empty()).ok_or("expression is required")?.to_string();
    let l = s.map().layer(id).ok_or("no such layer")?;
    let field = match kind {
        RuleKind::Calculation => {
            let fl = ps(p, "field").ok_or("a calculation rule sets a field")?;
            super::edit::Calculation::parse(&expression, &l.dataset)?;
            l.dataset
                .field_index(fl)
                .and_then(|i| l.dataset.fields.get(i))
                .map(|f| f.name.clone())
                .ok_or_else(|| format!("no field named '{fl}'"))?
        }
        RuleKind::Constraint => {
            rustgis_data::query::parse(&expression, &l.dataset)?;
            String::new()
        }
    };
    let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
    l.design.rules.retain(|r| !r.name.eq_ignore_ascii_case(&name));
    l.design.rules.push(AttributeRule { name: name.clone(), kind, field, expression });
    Ok(json!({"rule": name}))
}

fn contingent(s: &mut Session, p: &Json) -> CmdResult {
    let id = s.layer_id(p)?;
    let name = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).ok_or("name is required")?.to_string();
    if b(p, "remove").unwrap_or(false) {
        let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
        l.design.contingent.retain(|c| !c.name.eq_ignore_ascii_case(&name));
        return Ok(Json::Null);
    }
    let l = s.map().layer(id).ok_or("no such layer")?;
    let fields: Vec<(String, FieldType)> = p
        .get("fields")
        .and_then(Json::as_array)
        .ok_or("fields is required")?
        .iter()
        .map(|v| {
            let n = v.as_str().ok_or("fields are names")?;
            l.dataset
                .field_index(n)
                .and_then(|i| l.dataset.fields.get(i))
                .map(|f| (f.name.clone(), f.ty))
                .ok_or_else(|| format!("no field named '{n}'"))
        })
        .collect::<Result<_, String>>()?;
    if fields.len() < 2 {
        return Err("contingent values relate two or more fields".into());
    }
    let combos: Vec<Vec<Value>> = p
        .get("combos")
        .and_then(Json::as_array)
        .ok_or("combos is required")?
        .iter()
        .take(100_000)
        .map(|c| {
            let vals = c.as_array().filter(|a| a.len() == fields.len()).ok_or("each combination has one value per field")?;
            Ok(vals.iter().zip(&fields).map(|(v, (_, ty))| super::edit::coerce(Value::from_json(v), *ty)).collect())
        })
        .collect::<Result<_, String>>()?;
    let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
    l.design.contingent.retain(|c| !c.name.eq_ignore_ascii_case(&name));
    l.design.contingent.push(ContingentSet { name: name.clone(), fields: fields.into_iter().map(|f| f.0).collect(), combos });
    Ok(json!({"name": name}))
}

/// Puts a layer back as it was at a time, by undoing later archived edits (newest first).
fn archive_restore(s: &mut Session, p: &Json) -> CmdResult {
    let t = f(p, "time").ok_or("time is required")?;
    let id = s.layer_id(p)?;
    let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
    // The edits after `t`, newest first (only their earlier states are copied).
    let undo: Vec<(u64, Option<Feature>)> = {
        let archive = l.design.archive.as_ref().ok_or("archiving is off for this layer")?;
        // A full archive has dropped its oldest edits: it can't go back past them.
        if archive.len() >= MAX_ARCHIVE
            && let Some(first) = archive.first()
            && t < first.time
        {
            return Err("the archive keeps the most recent edits only, and doesn't reach back that far".into());
        }
        archive.iter().rev().take_while(|e| e.time > t).map(|e| (e.id, e.before.clone())).collect()
    };
    let mut touched: Vec<(u64, Option<Feature>)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    // Rows by feature id, kept up to date as features come back or go.
    let mut index: std::collections::HashMap<u64, usize> = l.dataset.features.iter().enumerate().map(|(i, f)| (f.id, i)).collect();
    for (fid, before) in &undo {
        let pos = index.get(fid).copied();
        let now = pos.and_then(|i| l.dataset.features.get(i)).cloned();
        if seen.insert(*fid) {
            touched.push((*fid, now));
        }
        match (before, pos) {
            (Some(b), Some(i)) => {
                if let Some(slot) = l.dataset.features.get_mut(i) {
                    *slot = b.clone();
                }
            }
            // A deleted feature comes back at the end of the table (its old row isn't kept).
            (Some(b), None) => {
                index.insert(*fid, l.dataset.features.len());
                l.dataset.features.push(b.clone());
            }
            (None, Some(i)) => {
                l.dataset.features.remove(i);
                index.remove(fid);
                // Rows after it move up one.
                for v in index.values_mut() {
                    if *v > i {
                        *v -= 1;
                    }
                }
            }
            (None, None) => {}
        }
    }
    if (has_area_display(l)
        || l.design.rules.iter().any(|rule| {
            rule.kind == RuleKind::Constraint
                && rustgis_data::query::parse_fields(&rule.expression, &l.dataset).is_ok_and(|expr| rustgis_data::query::uses_area(&expr))
        }))
        && let Some(message) = rustgis_data::ambiguous_area_message(
            touched.iter().filter_map(|(fid, _)| l.dataset.features.get(*index.get(fid)?)?.geometry.as_ref()),
        )
    {
        return Err(format!("{}: {message}", l.dataset.name));
    }
    // The restore is itself archived and counted, so it can be restored past too.
    let time = now();
    let mut counts = EditCounts::default();
    let mut entries = Vec::new();
    for (fid, before) in touched {
        let after = index.get(&fid).and_then(|i| l.dataset.features.get(*i)).cloned();
        match (&before, &after) {
            (None, Some(_)) => counts.added += 1,
            (Some(_), None) => counts.deleted += 1,
            (Some(a), Some(b)) if a == b => continue,
            _ => counts.modified += 1,
        }
        entries.push(rustgis_map::design::ArchiveEntry { time, id: fid, before, after });
    }
    if let Some(archive) = &mut l.design.archive {
        archive.extend(entries);
    }
    l.selection.clear();
    l.touch_geometry();
    let e = s.edit_counts.entry(id).or_default();
    e.added += counts.added;
    e.modified += counts.modified;
    e.deleted += counts.deleted;
    s.topology_errors.clear();
    Ok(json!({"undone": undo.len()}))
}

fn relate(s: &mut Session, p: &Json) -> CmdResult {
    let name = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).ok_or("name is required")?.to_string();
    if b(p, "remove").unwrap_or(false) {
        let rels = &mut s.map_mut().editing.relationships;
        let before = rels.len();
        rels.retain(|r| !r.name.eq_ignore_ascii_case(&name));
        return if rels.len() == before { Err(format!("no relationship '{name}'")) } else { Ok(Json::Null) };
    }
    let field_of = |layer: &str, key: &str| -> Result<(String, String), String> {
        let l = s.map().layer_by_name(layer).filter(|l| !l.is_raster()).ok_or_else(|| format!("no layer '{layer}'"))?;
        let f = l.dataset.field_index(key).and_then(|i| l.dataset.fields.get(i)).ok_or_else(|| format!("{layer} has no field '{key}'"))?;
        Ok((l.name.clone(), f.name.clone()))
    };
    let [origin_name, destination_name] = relationship_names(p)?;
    let (origin, origin_key) = field_of(origin_name, ps(p, "originKey").ok_or("originKey is required")?)?;
    let (destination, destination_key) = field_of(destination_name, ps(p, "destinationKey").ok_or("destinationKey is required")?)?;
    let rels = &mut s.map_mut().editing.relationships;
    rels.retain(|r| !r.name.eq_ignore_ascii_case(&name));
    rels.push(Relationship { name: name.clone(), origin, origin_key, destination, destination_key });
    Ok(json!({"relationship": name}))
}

/// The rows related to the selection through relationships (both ways), selecting them.
fn related(s: &mut Session, p: &Json) -> CmdResult {
    let rels = related_relationships(s, p)?;
    if rels.is_empty() {
        return Err("the map has no relationships".into());
    }
    let map = s.map();
    let mut found: Vec<(u32, Vec<usize>)> = Vec::new();
    for r in &rels {
        for (from, fk, to, tk) in
            [(&r.origin, &r.origin_key, &r.destination, &r.destination_key), (&r.destination, &r.destination_key, &r.origin, &r.origin_key)]
        {
            let (Some(a), Some(bl)) = (map.layer_by_name(from), map.layer_by_name(to)) else { continue };
            let (Some(ai), Some(bi)) = (a.dataset.field_index(fk), bl.dataset.field_index(tk)) else { continue };
            let keys: std::collections::HashSet<String> =
                a.selection.iter().map(|r| a.dataset.value(*r, ai)).filter(|v| !v.is_null()).map(|v| v.to_string()).collect();
            if keys.is_empty() {
                continue;
            }
            let rows: Vec<usize> =
                (0..bl.dataset.features.len()).filter(|r| keys.contains(&bl.dataset.value(*r, bi).to_string())).collect();
            found.push((bl.id, rows));
        }
    }
    let total: usize = found.iter().map(|(_, r)| r.len()).sum();
    let names: Vec<Json> = found.iter().filter_map(|(id, rows)| map.layer(*id).map(|l| json!({"layer": l.name, "rows": rows}))).collect();
    if b(p, "select").unwrap_or(true) && !found.is_empty() {
        for (id, rows) in found {
            super::select::set(s, id, rows, super::select::Mode::Add);
        }
    }
    Ok(json!({"related": total, "layers": names}))
}

/// Validates defaults before replacing a template; a refused numeric value leaves it intact.
fn template(s: &mut Session, p: &Json) -> CmdResult {
    let id = s.layer_id(p)?;
    let name = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).ok_or("name is required")?.to_string();
    let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
    if b(p, "remove").unwrap_or(false) {
        let before = l.design.templates.len();
        l.design.templates.retain(|t| !t.name.eq_ignore_ascii_case(&name));
        return if l.design.templates.len() == before { Err(format!("no template '{name}'")) } else { Ok(Json::Null) };
    }
    let mut defaults = Vec::new();
    if let Some(o) = p.get("defaults").and_then(Json::as_object) {
        for (k, v) in o {
            let fi = l.dataset.field_index(k).ok_or_else(|| format!("no field named '{k}'"))?;
            let ty = l.dataset.fields.get(fi).map(|f| f.ty).unwrap_or(FieldType::Text);
            let value = Value::from_json(v);
            if super::edit::non_finite_input(&value, ty) {
                return Err(format!("field '{k}' requires a finite number"));
            }
            defaults.push((l.dataset.fields.get(fi).map(|f| f.name.clone()).unwrap_or_default(), super::edit::coerce(value, ty)));
        }
    }
    l.design.templates.retain(|t| !t.name.eq_ignore_ascii_case(&name));
    l.design.templates.push(FeatureTemplate {
        name: name.clone(),
        description: ps(p, "description").unwrap_or("").chars().take(500).collect(),
        defaults,
    });
    Ok(json!({"template": name, "templates": l.design.templates.len()}))
}

/// The nearest vertex, line end or edge of a visible feature layer within the snapping tolerance.
fn snap(s: &mut Session, p: &Json) -> CmdResult {
    let c = Coord::new(f(p, "lon").ok_or("lon is required")?, f(p, "lat").ok_or("lat is required")?);
    Ok(match snap_at(s, c) {
        Some((ll, kind)) => json!({"lon": ll.x, "lat": ll.y, "snapped": true, "kind": kind}),
        None => json!({"lon": c.x, "lat": c.y, "snapped": false}),
    })
}

/// Where a point at `c` (longitude/latitude) snaps to, and to what, if anywhere. Read-only, for
/// drawing the snap marker without running a command (projected shapes are cached on the way).
///
/// Point features and MultiPoint members are vertex targets only, never edges or line ends.
/// Vertices and line ends take priority over edges. Equal distances keep the original layer,
/// row and member order.
pub fn snap_at(s: &mut Session, c: Coord) -> Option<(Coord, &'static str)> {
    snap_except(s, c, None)
}

/// [`snap_at`], leaving out one feature (layer id, row): a vertex being dragged doesn't snap to
/// the shape it is changing.
/// Use [`snap_vertex_except`] when the source node is known and its other parts should remain targets.
pub fn snap_except(s: &mut Session, c: Coord, skip: Option<(u32, usize)>) -> Option<(Coord, &'static str)> {
    snap_excluding(s, c, skip, None)
}

/// Snaps while moving one source vertex, excluding that node and its incident edges instead of
/// its whole feature. Other line/polygon parts remain targets. Polygon closing copies alias the
/// first node; a closed line's two endpoints remain distinct, as in `Geometry::move_vertex`.
/// Invalid addresses are ignored. Point/multipoint targets retain the existing snapping policy.
/// Moving a point or multipoint still excludes its whole feature, as with [`snap_except`].
pub fn snap_vertex_except(s: &mut Session, c: Coord, skip: Option<(u32, usize, rustgis_data::VertexId)>) -> Option<(Coord, &'static str)> {
    let skip = skip.filter(|(id, row, v)| {
        s.map().layer(*id).and_then(|l| l.dataset.features.get(*row)).and_then(|f| f.geometry.as_ref()).and_then(|g| g.vertex(*v)).is_some()
    });
    snap_excluding(s, c, None, skip)
}

/// Shares the original layer/row ordering, scale rules and distance selection between legacy
/// feature exclusion and source-node exclusion. Only the addressed row needs source identities.
fn snap_excluding(
    s: &mut Session,
    c: Coord,
    skip: Option<(u32, usize)>,
    vertex: Option<(u32, usize, rustgis_data::VertexId)>,
) -> Option<(Coord, &'static str)> {
    let sn = s.map().editing.snapping;
    if !sn.enabled {
        return None;
    }
    let m = s.map().to_map(c)?;
    let tol = sn.tolerance_px * s.map().camera.res;
    let scale = s.scale();
    let (flat, key) = (s.map().flat(), s.map().projection.code());
    // Vertices and ends win over edges whenever one is within the tolerance.
    let (mut best_point, mut best_edge): (Option<(f64, Coord, &'static str)>, Option<(f64, Coord, &'static str)>) = (None, None);
    let mut consider = |d2: f64, at: Coord, kind: &'static str| {
        let slot = if kind == "edge" { &mut best_edge } else { &mut best_point };
        if d2 <= tol * tol && slot.is_none_or(|b| d2 < b.0) {
            *slot = Some((d2, at, kind));
        }
    };
    let held = s.map().held_by_group(scale);
    for l in s.map_mut().layers.iter_mut().filter(|l| l.visible && !l.is_raster() && !l.graphics) {
        if !l.in_scale_range(scale) || held.contains(&l.id) {
            continue;
        }
        let id = l.id;
        l.projected_in(&flat, &key);
        for (row, g) in l.projected_cache().iter().enumerate() {
            let Some(g) = g.as_ref().filter(|_| skip != Some((id, row))) else { continue };
            if !g.extent().padded(0.0, tol).contains(m) {
                continue;
            }
            if let Some((_, _, v)) = vertex.filter(|(layer, source_row, _)| *layer == id && *source_row == row) {
                if let Some(source) = l.dataset.features.get(row).and_then(|f| f.geometry.as_ref()) {
                    snap_vertex::consider(source, v, &flat, sn, m, &mut consider);
                }
                continue;
            }
            let paths: Vec<&Vec<Coord>> = match g {
                Geometry::Line(parts) => parts.iter().collect(),
                Geometry::Polygon(polys) => polys.iter().flatten().collect(),
                _ => Vec::new(),
            };
            if sn.vertex {
                let points: &[Coord] = match g {
                    Geometry::Point(pt) => std::slice::from_ref(pt),
                    Geometry::MultiPoint(points) => points,
                    _ => &[],
                };
                for pt in points {
                    consider((pt.x - m.x).powi(2) + (pt.y - m.y).powi(2), *pt, "vertex");
                }
            }
            for path in paths {
                if sn.end && matches!(g, Geometry::Line(_)) {
                    for e in [path.first(), path.last()].into_iter().flatten() {
                        consider((e.x - m.x).powi(2) + (e.y - m.y).powi(2), *e, "end");
                    }
                }
                if sn.vertex {
                    for v in path {
                        consider((v.x - m.x).powi(2) + (v.y - m.y).powi(2), *v, "vertex");
                    }
                }
                if sn.edge {
                    for w in path.windows(2) {
                        let (Some(a), Some(b2)) = (w.first(), w.get(1)) else { continue };
                        let (dx, dy) = (b2.x - a.x, b2.y - a.y);
                        let len2 = dx * dx + dy * dy;
                        let t = if len2 > 0.0 { (((m.x - a.x) * dx + (m.y - a.y) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
                        let q = Coord::new(a.x + t * dx, a.y + t * dy);
                        consider((q.x - m.x).powi(2) + (q.y - m.y).powi(2), q, "edge");
                    }
                }
            }
        }
    }
    best_point.or(best_edge).and_then(|(_, at, kind)| s.map().to_lonlat(at).map(|ll| (ll, kind)))
}

fn topology_rules(s: &mut Session, p: &Json) -> CmdResult {
    if b(p, "clear").unwrap_or(false) {
        s.map_mut().editing.topology.clear();
    }
    if let Some(i) = p.get("remove").and_then(Json::as_u64) {
        let rules = &mut s.map_mut().editing.topology;
        if (i as usize) >= rules.len() {
            return Err(format!("no rule {i}"));
        }
        rules.remove(i as usize);
    }
    if let Some(a) = p.get("add") {
        let names = topology_add_names(p)?;
        let layer_of = |k: &str| -> Result<String, String> {
            let n = names.get(if k == "by" { 1 } else { 0 }).copied().ok_or_else(|| format!("{k} is required"))?;
            s.map().layer_by_name(n).filter(|l| !l.is_raster()).map(|l| l.name.clone()).ok_or_else(|| format!("no feature layer '{n}'"))
        };
        let layer = layer_of("layer")?;
        let gt = s.map().layer_by_name(&layer).map(|l| l.geometry_type()).unwrap_or(GeometryType::None);
        let need =
            |want: GeometryType| if gt == want { Ok(()) } else { Err(format!("this rule needs a {} layer", want.label().to_lowercase())) };
        let rule = match a.get("rule").and_then(Json::as_str).ok_or("rule is required")? {
            "mustNotOverlap" => {
                need(GeometryType::Polygon)?;
                TopologyRule::MustNotOverlap { layer }
            }
            "mustNotHaveGaps" => {
                need(GeometryType::Polygon)?;
                TopologyRule::MustNotHaveGaps { layer }
            }
            "mustNotSelfIntersect" => TopologyRule::MustNotSelfIntersect { layer },
            "mustNotHaveDangles" => {
                need(GeometryType::Polyline)?;
                TopologyRule::MustNotHaveDangles { layer }
            }
            "mustBeCoveredBy" => {
                need(GeometryType::Point)?;
                let by = layer_of("by")?;
                TopologyRule::MustBeCoveredBy { layer, by }
            }
            r => return Err(format!("unknown topology rule '{r}'")),
        };
        let rules = &mut s.map_mut().editing.topology;
        if !rules.contains(&rule) {
            rules.push(rule);
        }
    }
    let rules: Vec<Json> =
        s.map().editing.topology.iter().enumerate().map(|(i, r)| json!({"index": i, "rule": r.label(), "layer": r.layer()})).collect();
    Ok(Json::Array(rules))
}

/// One topology rule as `topology.validate` checks it: the layer's rows shown (their data shared,
/// not copied) and, for a rule against another layer, that layer's.
struct TopologyInput {
    rule: TopologyRule,
    layer: String,
    data: rustgis_data::Dataset,
    rows: Vec<usize>,
    by: Option<rustgis_data::Dataset>,
}

/// The rules as the map has them now, each with what it reads; a rule whose layer is gone is left
/// out, as it always was.
fn topology_checks(s: &Session) -> Vec<TopologyInput> {
    let mut inputs = Vec::new();
    for rule in s.map().editing.topology.clone() {
        let Some(l) = s.map().layer_by_name(rule.layer()) else { continue };
        let rows: Vec<usize> = l.visible_rows().iter().enumerate().filter(|(_, v)| **v).map(|(i, _)| i).collect();
        // A covering layer that is gone leaves the rule with nothing to check (its layer's
        // latitudes are still checked).
        let by = match &rule {
            TopologyRule::MustBeCoveredBy { by, .. } => s.map().layer_by_name(by).map(|b| b.dataset.clone()),
            _ => None,
        };
        inputs.push(TopologyInput { rule, layer: l.name.clone(), data: l.dataset.clone(), rows, by });
    }
    inputs
}

/// Whether two layers' data are the same: the same rows (shared, not compared one by one), fields
/// and geometry type.
fn same_data(a: &rustgis_data::Dataset, b: &rustgis_data::Dataset) -> bool {
    a.features.shares(&b.features) && a.fields == b.fields && a.geometry_type == b.geometry_type
}

/// Whether `was` (what a check read on another thread) is what the rules read now.
fn same_topology(was: &[TopologyInput], now: &[TopologyInput]) -> bool {
    was.len() == now.len()
        && was.iter().zip(now).all(|(a, b)| {
            a.rule == b.rule
                && a.layer == b.layer
                && a.rows == b.rows
                && same_data(&a.data, &b.data)
                && match (&a.by, &b.by) {
                    (Some(x), Some(y)) => same_data(x, y),
                    (None, None) => true,
                    _ => false,
                }
        })
}

/// Checks each rule, in order, a result for each (a cancelled run is an error): a layer with a
/// latitude out of range is an error, as it always was. Each check looks at Cancel between features
/// and counts its progress, except the gaps check's union of every polygon, one step; in a run that
/// can be cancelled it takes no work limit (see `rustgis_analysis::topology`).
fn check_topology(inputs: &[TopologyInput], tol: f64) -> Result<Vec<rustgis_analysis::topology::Checked>, String> {
    use rustgis_analysis::topology as t;
    let mut found = Vec::with_capacity(inputs.len());
    for i in inputs {
        // A cancelled run stops between rules too (a gaps rule's union doesn't look), as an error,
        // so no result short of its rules is ever taken.
        rustgis_data::progress::check().map_err(|_| "the topology check was cancelled".to_string())?;
        rustgis_analysis::validate_latitudes(&i.data, i.rows.iter().copied())?;
        found.push(match (&i.rule, &i.by) {
            (TopologyRule::MustNotOverlap { .. }, _) => t::overlaps(&i.data, &i.rows),
            (TopologyRule::MustNotHaveGaps { .. }, _) => t::gaps(&i.data, &i.rows),
            (TopologyRule::MustNotSelfIntersect { .. }, _) => t::self_intersections(&i.data, &i.rows),
            (TopologyRule::MustNotHaveDangles { .. }, _) => t::dangles(&i.data, &i.rows, tol),
            (TopologyRule::MustBeCoveredBy { .. }, Some(by)) => {
                rustgis_analysis::validate_latitudes(by, 0..by.features.len())?;
                t::not_covered(&i.data, &i.rows, by)
            }
            // Its covering layer is gone: nothing checked, nothing said, as it always was.
            (TopologyRule::MustBeCoveredBy { .. }, None) => t::Checked { errors: Vec::new(), complete: true, skipped: 0 },
        });
    }
    Ok(found)
}

/// The tolerance `topology.validate` checks dangles with, in metres.
fn topology_tolerance(p: &Json) -> f64 {
    f(p, "toleranceM").unwrap_or(1.0).clamp(0.0, 10_000.0)
}

/// The key a topology check read ahead is placed under.
const TOPOLOGY_AHEAD: &str = "topology.validate";

/// A topology check made on a thread of its own ([`topology_ahead`]), with what it read.
struct TopologyAhead {
    inputs: Vec<TopologyInput>,
    tol: f64,
    found: Result<Vec<rustgis_analysis::topology::Checked>, String>,
}

/// The check `topology.validate` makes before it runs in the desktop app (#1291, #1562): every
/// rule on a thread of its own, with progress and Cancel and no work limit. The command takes it
/// as it lands when the rules and their layers are still the ones checked.
pub(crate) fn topology_ahead(s: &Session, p: &Json) -> Option<super::reads::ReadWork> {
    let (inputs, tol) = (topology_checks(s), topology_tolerance(p));
    if inputs.is_empty() {
        return None;
    }
    Some(super::reads::ahead("Error Inspector", TOPOLOGY_AHEAD.to_string(), move || {
        let found = check_topology(&inputs, tol);
        TopologyAhead { inputs, tol, found }
    }))
}

/// Checks every topology rule; the errors are kept for the Error Inspector. The work grows with
/// the layers, so the desktop app checks them on a thread of its own first ([`topology_ahead`]),
/// with the status bar's progress and **Cancel**, and the command takes that check as it lands;
/// when a rule or a layer changed meanwhile it says so and keeps the errors it had, rather than
/// check again here, which would hold the window. The assistant, tasks, scripts and the
/// command line call commands one at a time on the session and use each answer before the next,
/// so they check where they ask, with the work limit of a run that can't be cancelled.
fn topology_validate(s: &mut Session, p: &Json) -> CmdResult {
    let tol = topology_tolerance(p);
    let inputs = topology_checks(s);
    let found = match super::reads::take::<TopologyAhead>(s, TOPOLOGY_AHEAD) {
        Some(ahead) if ahead.tol == tol && same_topology(&ahead.inputs, &inputs) => ahead.found?,
        // Checking again here would hold the window: the check made on its thread is dropped.
        Some(_) => return Err("the topology rules or their layers changed while they were checked: validate again".into()),
        None => check_topology(&inputs, tol)?,
    };
    // Rules whose layer, or covering layer, isn't on the map are checked for nothing: said.
    let unchecked: Vec<String> = s
        .map()
        .editing
        .topology
        .iter()
        .filter(|r| !inputs.iter().any(|i| &i.rule == *r && i.by.is_some() == matches!(r, TopologyRule::MustBeCoveredBy { .. })))
        .map(|r| format!("{} on '{}'", r.label(), r.layer()))
        .collect();
    if !unchecked.is_empty() {
        s.warn(format!(
            "{} topology rule(s) weren't checked: their layer or covering layer isn't on the map ({})",
            unchecked.len(),
            unchecked.join(", ")
        ));
    }
    let mut errors = Vec::new();
    // Rules whose check stopped early (an error or work limit), or measured some part more coarsely
    // than it's meant to (a line winding round a pole many times): their counts may be off.
    let mut incomplete = Vec::new();
    // Polygons a check had to leave out, by rule.
    let mut skipped = Vec::new();
    for (i, found) in inputs.iter().zip(found) {
        let rule = &i.rule;
        if !found.complete || found.skipped > 0 {
            incomplete.push(rule.label().to_string());
        }
        if found.skipped > 0 {
            // The features left out are the covering layer's.
            let of = match rule {
                TopologyRule::MustBeCoveredBy { by, .. } => by.as_str(),
                _ => i.layer.as_str(),
            };
            skipped.push(format!("{} on '{}': {} feature(s) of '{}'", rule.label(), i.layer, found.skipped, of));
        }
        errors.extend(found.errors.into_iter().map(|e| crate::TopologyIssue {
            rule: rule.label().to_string(),
            layer: i.layer.clone(),
            rows: e.rows,
            lon: e.at.x,
            lat: e.at.y,
        }));
    }
    if !incomplete.is_empty() {
        s.warn(format!(
            "{} topology rule(s) weren't checked completely ({}): a check stopped early, left out a polygon, or measured a line winding round a pole coarsely, so their counts may be off",
            incomplete.len(),
            incomplete.join(", ")
        ));
    }
    if !skipped.is_empty() {
        s.warn(format!(
            "Polygons left out of the check at a pole (a coordinate missing or infinite, or a million turns around), so its result there may be wrong: {}",
            skipped.join("; ")
        ));
    }
    let n = errors.len();
    let out: Vec<Json> =
        errors.iter().take(1000).map(|e| json!({"rule": e.rule, "layer": e.layer, "rows": e.rows, "lon": e.lon, "lat": e.lat})).collect();
    s.topology_errors = errors;
    Ok(json!({"errors": n, "first": out, "incomplete": incomplete}))
}

/// Resolves the first named relationship, or all relationships, for both-direction selection.
fn related_relationships(input: &impl super::InputSource, p: &Json) -> Result<Vec<Relationship>, String> {
    let context = input.input_context();
    let s = &context;
    Ok(match ps(p, "name") {
        Some(n) => vec![
            s.map()
                .editing
                .relationships
                .iter()
                .find(|r| r.name.eq_ignore_ascii_case(n))
                .cloned()
                .ok_or_else(|| format!("no relationship '{n}'"))?,
        ],
        None => s.map().editing.relationships.clone(),
    })
}

/// Removal consumes no endpoints; creation binds both actual feature-layer names.
fn relationship_inputs(input: &super::InputContext<'_>, p: &Json) -> Result<super::Inputs, String> {
    let s = input;
    if b(p, "remove") == Some(true) {
        return Ok(super::Inputs::layers(std::iter::empty()));
    }
    let names = relationship_names(p)?;
    super::Inputs::named(s.map(), names)
}
/// Parses the required origin and destination names shared with relationship creation.
fn relationship_names(p: &Json) -> Result<[&str; 2], String> {
    Ok([ps(p, "origin").ok_or("origin is required")?, ps(p, "destination").ok_or("destination is required")?])
}
/// Binds the existing endpoints visited by the same stored relationship selection.
fn related_inputs(input: &super::InputContext<'_>, p: &Json) -> Result<super::Inputs, String> {
    let s = input;
    let relationships = related_relationships(input, p)?;
    Ok(super::Inputs::layers(
        relationships.iter().flat_map(|r| [&r.origin, &r.destination]).filter_map(|n| s.map().layer_by_name(n)).map(|l| (s.map(), l)),
    ))
}
/// Binds only the endpoints of a rule being added.
fn topology_inputs(input: &super::InputContext<'_>, p: &Json) -> Result<super::Inputs, String> {
    let s = input;
    let names = topology_add_names(p)?;
    super::Inputs::named(s.map(), names)
}
/// Only a covered-by rule consumes a second layer named by.
fn topology_add_names(p: &Json) -> Result<Vec<&str>, String> {
    let Some(add) = p.get("add") else { return Ok(Vec::new()) };
    let mut names = vec![add.get("layer").and_then(Json::as_str).ok_or("layer is required")?];
    if add.get("rule").and_then(Json::as_str) == Some("mustBeCoveredBy") {
        names.push(add.get("by").and_then(Json::as_str).ok_or("by is required")?);
    }
    Ok(names)
}
/// Binds the stored rule endpoints that validation can actually find.
fn validation_inputs(input: &super::InputContext<'_>, _: &Json) -> Result<super::Inputs, String> {
    let s = input;
    let names = s.map().editing.topology.iter().flat_map(|r| {
        let mut names = vec![r.layer()];
        if let TopologyRule::MustBeCoveredBy { by, .. } = r {
            names.push(by.as_str());
        }
        names
    });
    Ok(super::Inputs::layers(names.filter_map(|n| s.map().layer_by_name(n)).map(|l| (s.map(), l))))
}

/// Finds the literal raster surface used for elevation validation and sampling.
fn surface_layer<'a>(input: &'a impl super::InputSource, name: &str) -> Option<&'a rustgis_map::Layer> {
    let context = input.input_context();
    let s = &context;
    s.map().layer_by_name(name).filter(|l| l.is_raster())
}
/// The literal surface selector shared by the executor and dependency planning.
fn surface_name(p: &Json) -> Result<&str, String> {
    ps(p, "surface").ok_or("surface (a raster layer) is required".into())
}
/// Only surface mode consumes a layer; constant and none are parameter-only.
fn elevation_inputs(input: &super::InputContext<'_>, p: &Json) -> Result<super::Inputs, String> {
    let name = if ps(p, "mode") == Some("surface") { Some(surface_name(p)?) } else { None };
    super::Inputs::named(input.map(), name)
}
/// Only new rows sample the stored elevation surface; ordinary attribute edits do not.
pub(crate) fn new_feature_inputs(input: &super::InputContext<'_>, p: &Json) -> Result<super::Inputs, String> {
    let s = input;
    let mut inputs = super::Inputs::primary(s, p)?;
    if let Elevation::Surface(name) = &s.map().editing.settings.elevation {
        inputs.layers.extend(surface_layer(input, name).map(|l| (s.map().instance_id(), l.id)));
        inputs.state = json!({"surface":name});
    }
    Ok(inputs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_center_is_found_within_the_commands_budget() {
        // A line at 89.9°N whose ten edges each span 170° of longitude: 340 half-degree stretches
        // each, out of the budget; with none left, each edge is weighed in one, and a center is
        // still found.
        let line = Geometry::Line(vec![(0..=10).map(|i| Coord::new(if i % 2 == 0 { 0.0 } else { 170.0 }, 89.9)).collect()]);
        let mut steps = CenterSteps::new();
        assert!(plane_at([&line], &mut steps).is_some());
        assert_eq!(CENTER_STEPS_TOTAL - steps.left, 10 * 340);
        assert_eq!(steps.coarse, 0);
        let mut steps = CenterSteps { left: 0, coarse: 0 };
        assert!(plane_at([&line], &mut steps).is_some() && steps.left == 0);
        assert_eq!(steps.coarse, 10);
    }

    #[test]
    fn a_center_says_when_its_steps_run_out() {
        // A line at 89.9°N of 3,000 edges, each 340 half-degree stretches: the million run out in
        // the 2,942nd, and the 59 after it are weighed in one, which is counted; ten edges are not.
        let zigzag =
            |edges: usize| Geometry::Line(vec![(0..=edges).map(|i| Coord::new(if i % 2 == 0 { 0.0 } else { 170.0 }, 89.9)).collect()]);
        let mut steps = CenterSteps::new();
        assert!(plane_at([&zigzag(3_000)], &mut steps).is_some());
        assert_eq!(steps.coarse, 59);
        let mut steps = CenterSteps::new();
        assert!(plane_at([&zigzag(10)], &mut steps).is_some());
        assert_eq!(steps.coarse, 0);
    }

    #[test]
    fn a_tiny_move_on_the_ground_doesnt_turn() {
        for (dx, dy) in [(1e-12, 0.0), (1e-9, 1e-9), (0.0, -1e-12)] {
            let (_, _, turn) = ground_move(Coord::new(30.0, 88.0), dx, dy);
            assert!(turn.abs() < 1e-9, "{dx} {dy}: {turn}");
        }
    }

    #[test]
    fn a_vertex_is_one_key_however_its_longitude_is_written() {
        let key = |x: f64, y: f64| vertex_key(Coord::new(x, y));
        assert_eq!(key(180.0, -87.0), key(-180.0, -87.0));
        assert_eq!(key(0.0, 86.0), key(-0.0, 86.0));
        assert_eq!(key(370.0, 86.0), key(10.0, 86.0));
        assert_eq!(key(190.1, 86.0), key(-169.9, 86.0));
        assert_eq!(key(370.1, 86.0), key(10.1, 86.0));
        assert_eq!(key(36_000_000_010.0, 86.0), key(10.0, 86.0));
        assert_eq!(key(-1e-20, 86.0), key(0.0, 86.0));
        assert_eq!(key(10.0, 90.0), key(200.0, 90.0));
        assert_eq!(key(10.0, 90.0), key(200.0, 89.999_999_95));
        assert_ne!(key(10.0, 86.0), key(11.0, 86.0));
    }

    #[test]
    fn a_hole_lands_in_its_shell_however_far_the_shell_goes() {
        // A box at 86°N to 87°N with a hole, whose corner a feature moved before placed a turn
        // east: the shell follows it there, and the hole comes with the shell.
        let c = Coord::new;
        let shell = vec![c(10.0, 86.0), c(15.0, 86.0), c(15.0, 87.0), c(10.0, 87.0), c(10.0, 86.0)];
        let hole = vec![c(11.0, 86.2), c(11.0, 86.8), c(14.0, 86.8), c(14.0, 86.2), c(11.0, 86.2)];
        let plane = Aeqd::new(c(12.5, 86.5));
        let mut placed = HashMap::new();
        placed.insert(vertex_key(c(10.0, 86.0)), c(370.0, 86.0));
        let moved = polar_transform(&Geometry::Polygon(vec![vec![shell, hole]]), (&plane, &plane), Some(&mut placed), |p| p);
        let Ok(Geometry::Polygon(p)) = moved else { panic!("{moved:?}") };
        let span = |r: &[Coord]| r.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), c| (lo.min(c.x), hi.max(c.x)));
        let ((slo, shi), (hlo, hhi)) = (span(&p[0][0]), span(&p[0][1]));
        assert!(slo > 369.0 && slo < hlo && hhi < shi, "{p:?}");
    }

    #[test]
    fn a_split_searches_a_segment_within_its_budget() {
        // From 89°N on 0°, along 88°N from 30°W to 30°E: searched, its nearest point is on 0°, a
        // kilometer nearer than its ends, and the search takes a few dozen of the budget; with
        // too few left, the nearer end stands for it, and the steps note how near the segment
        // might come (no nearer than the point found, nor than half its ends' distances less its
        // length).
        let c = Coord::new;
        let (at, a, b) = (c(0.0, 89.0), c(-30.0, 88.0), c(30.0, 88.0));
        let ends = (haversine_m(at, a), haversine_m(at, b));
        let mut steps = SplitSteps { left: 1_000, floor: f64::INFINITY };
        let (d, q) = nearest_on_ground(at, (a, b), ends, f64::INFINITY, &mut steps).unwrap();
        assert!(
            q.x.abs() < 1e-6 && d < ends.0 - 1_000.0 && (900..990).contains(&steps.left) && steps.floor.is_infinite(),
            "{q:?} {d} {}",
            steps.left
        );
        let mut steps = SplitSteps { left: 5, floor: f64::INFINITY };
        assert_eq!(nearest_on_ground(at, (a, b), ends, f64::INFINITY, &mut steps), Some((ends.0, a)));
        assert!(steps.left == 5 && steps.floor <= d && steps.floor < ends.0, "{}", steps.floor);
        // Nor is a segment missing a coordinate measured.
        assert_eq!(nearest_on_ground(at, (c(f64::NAN, 88.0), b), (f64::NAN, ends.1), f64::INFINITY, &mut steps), None);
    }

    #[test]
    fn points_moved_with_lines_dont_pull_their_center() {
        // A line's center is its edges' wherever points selected with it are.
        let c = Coord::new;
        let line = Geometry::Line(vec![vec![c(0.0, 86.0), c(10.0, 86.0)]]);
        let points = Geometry::MultiPoint(vec![c(90.0, 88.0), c(100.0, 88.0)]);
        let (mut a, mut b) = (CenterSteps::new(), CenterSteps::new());
        assert_eq!(plane_at([&line, &points], &mut a), plane_at([&line], &mut b));
    }
}
