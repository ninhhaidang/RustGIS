//! Data › Data Design (domains, subtypes, attribute rules, contingent values), Archiving and
//! Relationships, and Edit › Manage Edits, Snapping, Features (modify tools), Elevation and
//! Corrections. Every feature edit goes through [`apply`], which enforces the layer's design.

use rustgis_data::value::PastClasses;
use rustgis_data::{Feature, FieldType, Geometry, GeometryType, Value};
use rustgis_geom::{Coord, lonlat_to_merc, merc_to_lonlat};
use rustgis_map::design::{
    AttributeRule, ContingentSet, Domain, DomainKind, Elevation, FeatureTemplate, Relationship, RuleKind, Subtype, Subtypes, TopologyRule,
};
use serde_json::{Value as Json, json};

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
                let r = s.map().layer_by_name(name)?.raster.as_ref()?.data.as_ref()?;
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
    // Calculation rules, in order.
    for rule in l.design.rules.iter().filter(|r| r.kind == RuleKind::Calculation) {
        let Some(fi) = l.dataset.field_index(&rule.field) else { continue };
        let ty = l.dataset.fields.get(fi).map(|f| f.ty).unwrap_or(FieldType::Text);
        // A rule that no longer reads (its field was removed, or a hand-edited file) is skipped
        // rather than blocking every edit.
        let Ok(calc) = super::edit::Calculation::parse(&rule.expression, &l.dataset) else { continue };
        for r in &rows {
            let v = super::edit::coerce(calc.eval(&l.dataset, *r), ty);
            if let Some(cell) = l.dataset.features.get_mut(*r).and_then(|f| f.attrs.get_mut(fi)) {
                *cell = v;
            }
        }
    }
    // Validation. Domains and contingent values are checked on the fields an edit changed (all
    // of a new feature's), so a value set before the domain doesn't block unrelated edits.
    let constraints: Vec<(String, String, rustgis_data::query::Expr)> = l
        .design
        .rules
        .iter()
        .filter(|r| r.kind == RuleKind::Constraint)
        .filter_map(|rule| {
            rustgis_data::query::parse(&rule.expression, &l.dataset).ok().map(|e| (rule.name.clone(), rule.expression.clone(), e))
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
    Ok(())
}

/// Registers data-design and editing commands, including opt-in exact-coordinate polygon moves.
/// Feature edits share `apply` for rules, archives and counts inside the session transaction.
pub fn commands() -> Vec<C> {
    vec![
        // Data design.
        C::new("data.domain", "Domains", "Data › Data Design", domain).when(need_feature_layer).params(
            r#"{"layer"?, "field": string, "coded"?: [[code, description], …], "min"?: number, "max"?: number, "clear"?: bool, "force"?: bool (set it even though existing values fall outside)}"#,
        ),
        C::new("data.subtypes", "Subtypes", "Data › Data Design", subtypes).when(need_feature_layer).params(
            r#"{"layer"?, "field": integer field, "subtypes": [{"code": n, "name": string, "defaults"?: {field: value}}], "symbolize"?: bool, "clear"?: bool}"#,
        ),
        C::new("data.attributeRule", "Attribute Rules", "Data › Data Design", attribute_rule).when(need_feature_layer).params(
            r#"{"layer"?, "name": string, "kind": "calculation"|"constraint", "field"?: field set by a calculation, "expression": calculator expression or SQL where clause, "remove"?: bool}"#,
        ),
        C::new("data.contingentValues", "Contingent Values", "Data › Data Design", contingent).when(need_feature_layer).params(
            r#"{"layer"?, "name": string, "fields": [field, …], "combos": [[value, …], …], "remove"?: bool}"#,
        ),
        C::new("data.archive", "Add Archive", "Data › Archiving", |s, p| {
            let on = b(p, "on").unwrap_or(true);
            let id = s.layer_id(p)?;
            let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
            l.design.archive = if on { Some(l.design.archive.take().unwrap_or_default()) } else { None };
            Ok(json!({"archiving": on}))
        })
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
        }).params(r#"{"layer"?: name|id (default: the active layer)}"#)
        .view()
        .when(need_feature_layer),
        C::new("data.archiveRestore", "Restore From Archive", "Data › Archiving", archive_restore)
            .when(need_editable)
            .params(r#"{"layer"?, "time": seconds since 1970 (the layer as it was then)}"#),
        C::new("data.relate", "Related Data", "Data › Relationship", relate).params(
            r#"{"name": string, "origin": layer, "originKey": field, "destination": layer, "destinationKey": field, "remove"?: bool}"#,
        ),
        C::new("data.related", "Select Related", "Data › Relationship", related)
            .params(r#"{"name"?: relationship (all when omitted), "select"?: bool (select the related rows; default true)}"#),
        // Manage edits.
        C::new("edit.enable", "Edit", "Edit › Manage Edits", |s, p| {
            let set = &mut s.map_mut().editing.settings;
            set.enabled = b(p, "on").unwrap_or(!set.enabled);
            Ok(json!({"enabled": set.enabled}))
        })
        .params(r#"{"on"?: bool}"#),
        C::new("edit.settings", "Editing Settings", "Edit › Manage Edits", |s, p| {
            let set = &mut s.map_mut().editing.settings;
            if let Some(v) = b(p, "showVertices") {
                set.show_vertices = v;
            }
            Ok(json!({"enabled": set.enabled, "showVertices": set.show_vertices}))
        })
        .params(r#"{"showVertices"?: bool}"#),
        C::new("edit.status", "Edit Status", "Edit › Manage Edits", |s, _| {
            let rows: Vec<Json> = s
                .edit_counts
                .iter()
                .filter_map(|(id, c)| s.map().layer(*id).map(|l| json!({"layer": l.name, "added": c.added, "modified": c.modified, "deleted": c.deleted})))
                .collect();
            Ok(json!({"unsaved": s.dirty(), "layers": rows}))
        })
        .view(),
        C::new("edit.template", "Manage Templates", "Edit › Manage Edits", template).when(need_feature_layer).params(
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
        })
        .params(r#"{"on"?: bool, "tolerancePx"?: number, "vertex"?: bool, "edge"?: bool, "end"?: bool}"#),
        C::new("edit.snap", "Snap Point", "Edit › Snapping", snap).view().params(r#"{"lon", "lat"}"#),
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
            modify(s, |g| Some(g.map_coords(&|c: Coord| shift(c, dx, dy))))
        })
        .when(need_editable_selection)
        .params(r#"{"dx": distance east, "dy": distance north, "units"?: "Meters" (default)|"Feet"|"Kilometers"|"Miles"}"#),
        C::new("edit.rotate", "Rotate", "Edit › Features › Modify", |s, p| {
            let deg = f(p, "angle").ok_or("angle is required")?;
            modify(s, |g| {
                // Any point, a feature's or a text graphic's, turns on itself: left out, so a
                // selection of points alone fails rather than recording an edit that changes
                // nothing (and an undo step).
                if matches!(g, Geometry::Point(_)) {
                    return None;
                }
                let c = lonlat_to_merc(g.extent().center());
                let (sin, cos) = (-deg.to_radians()).sin_cos();
                Some(g.map_coords(&|p: Coord| {
                    let m = lonlat_to_merc(p);
                    let (x, y) = (m.x - c.x, m.y - c.y);
                    merc_to_lonlat(Coord::new(c.x + x * cos - y * sin, c.y + x * sin + y * cos))
                }))
            })
        })
        .when(need_editable_selection)
        .params(r#"{"angle": degrees clockwise}"#),
        C::new("edit.scale", "Scale", "Edit › Features › Modify", |s, p| {
            let k = f(p, "factor").filter(|k| *k > 0.0 && *k < 1e6).ok_or("factor must be a positive number")?;
            modify(s, |g| {
                // Any point, a feature's or a text graphic's, scales on itself: left out, as above.
                if matches!(g, Geometry::Point(_)) {
                    return None;
                }
                let c = lonlat_to_merc(g.extent().center());
                Some(g.map_coords(&|p: Coord| {
                    let m = lonlat_to_merc(p);
                    merc_to_lonlat(Coord::new(c.x + (m.x - c.x) * k, c.y + (m.y - c.y) * k))
                }))
            })
        })
        .when(need_editable_selection)
        .params(r#"{"factor": number}"#),
        C::new("edit.flip", "Flip Line", "Edit › Features › Modify", |s, _| {
            modify(s, |g| match g {
                Geometry::Line(parts) => Some(Geometry::Line(parts.iter().rev().map(|p| p.iter().rev().copied().collect()).collect())),
                _ => None,
            })
        })
        .when(need_editable_selection),
        C::new("edit.setGeometry", "Set Geometry", "Edit › Features › Modify", set_geometry)
            .when(need_editable)
            .params(r#"{"layer"?, "row": number, "geometry": GeoJSON geometry}"#),
        C::new("edit.vertex", "Edit Vertex", "Edit › Features › Modify", edit_vertex).when(need_editable).params(
            r#"{"layer"?, "row": number, "action": "move"|"insert"|"delete", "part"?: number, "ring"?: number, "index": number (insert: the vertex the new one follows), "lon"?, "lat"?}"#,
        ),
        C::new("edit.merge", "Merge", "Edit › Features › Modify", merge).when(need_editable_selection),
        C::new("edit.moveSharedVertex", "Move Shared Vertex", "Edit › Features › Modify", shared_vertex::run).when(need_editable).params(
            r#"{"layer"?, "row": number, "part"?: number, "ring"?: number, "index": number, "lon": degrees, "lat": degrees}"#,
        ),
        C::new("edit.explode", "Explode", "Edit › Features › Modify", explode).when(need_editable_selection),
        C::new("edit.split", "Split", "Edit › Features › Modify", split).when(need_editable_selection).params(r#"{"lon", "lat"}"#),
        C::new("edit.cutPolygon", "Cut Polygon", "Edit › Features › Modify", sketch::cut_polygon)
            .when(need_editable_selection)
            .params(r#"{"line": GeoJSON LineString (lon/lat) drawn across the selected polygons}"#),
        C::new("edit.reshape", "Reshape", "Edit › Features › Modify", sketch::reshape)
            .when(need_editable_selection)
            .params(r#"{"line": GeoJSON LineString (lon/lat) drawn across the selected line or polygon's boundary}"#),
        C::new("edit.trace", "Trace Edge Path", "Edit › Features", sketch::trace).view().params(
            r#"{"from": [lon, lat], "to": [lon, lat], "layer"?, "row"?: number (else the nearest edge within the tolerance), "tolerancePx"?: number (default: the snapping tolerance)}"#,
        ),
        C::new("edit.traverse", "Traverse", "Edit › Features › Modify", traverse).when(need_editable).params(
            r#"{"layer"?, "start": [lon, lat], "units"?: "Meters" (default)|"Feet"|"Kilometers"|"Miles", "legs": [{"distance": number, "bearing": degrees from north}]} (closed in a polygon layer, open in a line layer)"#,
        ),
        C::new("edit.tools", "Editor Tools", "Edit › Tools", |s, _| {
            let tools: Vec<Json> = s.registry.commands.iter().filter(|c| c.location.starts_with("Edit › Features")).map(|c| json!({"id": c.id, "label": c.label, "params": c.params})).collect();
            Ok(Json::Array(tools))
        })
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
                    let n = ps(p, "surface").ok_or("surface (a raster layer) is required")?;
                    let l = s.map().layer_by_name(n).filter(|l| l.is_raster()).ok_or_else(|| format!("no raster layer '{n}'"))?;
                    Elevation::Surface(l.name.clone())
                }
                m => return Err(format!("unknown elevation mode '{m}'")),
            };
            s.map_mut().editing.settings.elevation = e;
            Ok(Json::Null)
        })
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
        })
        .params(r#"{"on"?: bool, "scale"?: factor, "rotation"?: degrees}"#),
        // Topology.
        C::new("topology.rules", "Topology Options", "Edit › Manage Edits", topology_rules).params(
            r#"{"add"?: {"rule": "mustNotOverlap"|"mustNotHaveGaps"|"mustNotSelfIntersect"|"mustNotHaveDangles"|"mustBeCoveredBy", "layer": layer, "by"?: polygon layer}, "remove"?: index, "clear"?: bool}"#,
        ),
        C::new("topology.validate", "Error Inspector", "Edit › Manage Edits", topology_validate).view().params(r#"{"toleranceM"?: metres (dangles)}"#),
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

/// A ground distance and direction corrected to the grid: scaled and rotated.
fn ground_to_grid(dx: f64, dy: f64, k: f64, rot_deg: f64) -> (f64, f64) {
    let (sin, cos) = (-rot_deg.to_radians()).sin_cos();
    ((dx * cos - dy * sin) * k, (dx * sin + dy * cos) * k)
}

/// Applies a geometry change to every selected feature of every layer (in the selection's layers).
fn modify(s: &mut Session, change: impl Fn(&Geometry) -> Option<Geometry>) -> CmdResult {
    let mut n = 0;
    let ids: Vec<u32> = s.map().layers.iter().filter(|l| !l.selection.is_empty() && !l.is_raster()).map(|l| l.id).collect();
    for id in ids {
        let mut changes = Vec::new();
        let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
        for r in l.selection.clone() {
            let Some(f) = l.dataset.features.get_mut(r) else { continue };
            let Some(g) = f.geometry.as_ref().and_then(&change) else { continue };
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
    let g = p.get("geometry").and_then(rustgis_data::io::geometry_from_geojson).ok_or("geometry must be a GeoJSON geometry")?;
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
    let id = selection_layer(s)?;
    let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
    let mut changes = Vec::new();
    for r in l.selection.clone() {
        let Some(f) = l.dataset.features.get(r).cloned() else { continue };
        let Some(Geometry::Line(parts)) = &f.geometry else { continue };
        // The nearest segment of the nearest part.
        let m = lonlat_to_merc(at);
        let mut best: Option<(f64, usize, usize, Coord)> = None;
        for (pi, part) in parts.iter().enumerate() {
            for (si, w) in part.windows(2).enumerate() {
                let (Some(a), Some(b2)) = (w.first(), w.get(1)) else { continue };
                let (a, b2) = (lonlat_to_merc(*a), lonlat_to_merc(*b2));
                let (dx, dy) = (b2.x - a.x, b2.y - a.y);
                let len2 = dx * dx + dy * dy;
                let t = if len2 > 0.0 { (((m.x - a.x) * dx + (m.y - a.y) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
                let q = Coord::new(a.x + t * dx, a.y + t * dy);
                let d = (q.x - m.x).powi(2) + (q.y - m.y).powi(2);
                if best.is_none_or(|b| d < b.0) {
                    best = Some((d, pi, si, merc_to_lonlat(q)));
                }
            }
        }
        let Some((_, pi, si, q)) = best else { continue };
        let Some(part) = parts.get(pi) else { continue };
        let mut first: Vec<Coord> = part.iter().take(si + 1).copied().collect();
        first.push(q);
        let mut second = vec![q];
        second.extend(part.iter().skip(si + 1).copied());
        // At (or within a centimetre of) the line's ends isn't a split.
        let near = |a: Option<&Coord>| a.is_some_and(|a| rustgis_geom::haversine_m(*a, q) < 0.01);
        if first.len() < 2 || second.len() < 2 || near(first.first()) || near(second.last()) {
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
    }
    if changes.is_empty() {
        return Err("select a line to split (away from its ends)".into());
    }
    l.touch_geometry();
    apply(s, id, changes)?;
    Ok(json!({"split": true}))
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
        let defaults = v
            .get("defaults")
            .and_then(Json::as_object)
            .map(|o| o.iter().map(|(k, v)| (k.clone(), Value::from_json(v))).collect())
            .unwrap_or_default();
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
    let (origin, origin_key) = field_of(ps(p, "origin").ok_or("origin is required")?, ps(p, "originKey").ok_or("originKey is required")?)?;
    let (destination, destination_key) =
        field_of(ps(p, "destination").ok_or("destination is required")?, ps(p, "destinationKey").ok_or("destinationKey is required")?)?;
    let rels = &mut s.map_mut().editing.relationships;
    rels.retain(|r| !r.name.eq_ignore_ascii_case(&name));
    rels.push(Relationship { name: name.clone(), origin, origin_key, destination, destination_key });
    Ok(json!({"relationship": name}))
}

/// The rows related to the selection through relationships (both ways), selecting them.
fn related(s: &mut Session, p: &Json) -> CmdResult {
    let rels: Vec<Relationship> = match ps(p, "name") {
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
    };
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
            defaults
                .push((l.dataset.fields.get(fi).map(|f| f.name.clone()).unwrap_or_default(), super::edit::coerce(Value::from_json(v), ty)));
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
        let layer_of = |k: &str| -> Result<String, String> {
            let n = a.get(k).and_then(Json::as_str).ok_or_else(|| format!("{k} is required"))?;
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

/// Checks every topology rule; the errors are kept for the Error Inspector.
fn topology_validate(s: &mut Session, p: &Json) -> CmdResult {
    use rustgis_analysis::topology as t;
    let tol = f(p, "toleranceM").unwrap_or(1.0).clamp(0.0, 10_000.0);
    let mut errors = Vec::new();
    // Rules whose check stopped early (an error or work limit): their counts are partial.
    let mut incomplete = Vec::new();
    for rule in s.map().editing.topology.clone() {
        let Some(l) = s.map().layer_by_name(rule.layer()) else { continue };
        let rows: Vec<usize> = l.visible_rows().iter().enumerate().filter(|(_, v)| **v).map(|(i, _)| i).collect();
        let found = match &rule {
            TopologyRule::MustNotOverlap { .. } => t::overlaps(&l.dataset, &rows),
            TopologyRule::MustNotHaveGaps { .. } => t::gaps(&l.dataset, &rows),
            TopologyRule::MustNotSelfIntersect { .. } => t::self_intersections(&l.dataset, &rows),
            TopologyRule::MustNotHaveDangles { .. } => t::dangles(&l.dataset, &rows, tol),
            TopologyRule::MustBeCoveredBy { by, .. } => match s.map().layer_by_name(by) {
                Some(b) => t::not_covered(&l.dataset, &rows, &b.dataset),
                None => continue,
            },
        };
        if !found.complete {
            incomplete.push(rule.label().to_string());
        }
        errors.extend(found.errors.into_iter().map(|e| crate::TopologyIssue {
            rule: rule.label().to_string(),
            layer: l.name.clone(),
            rows: e.rows,
            lon: e.at.x,
            lat: e.at.y,
        }));
    }
    let n = errors.len();
    let out: Vec<Json> =
        errors.iter().take(1000).map(|e| json!({"rule": e.rule, "layer": e.layer, "rows": e.rows, "lon": e.lon, "lat": e.lat})).collect();
    s.topology_errors = errors;
    Ok(json!({"errors": n, "first": out, "incomplete": incomplete}))
}
