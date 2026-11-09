//! The attribute table's geometry calculator: stores each feature's area, perimeter, length or
//! X/Y coordinate, calculated from its WGS84 geometry, in a field.
use crate::{CmdResult, Session};
use rustgis_data::{FieldType, Geometry, GeometryType, Measure, Value, raster::Crs};
use rustgis_geom::spherical_centre;
use serde_json::{Value as Json, json};

/// The properties a layer of `kind` can calculate, as `(key, label)` pairs.
pub fn properties(kind: GeometryType) -> &'static [(&'static str, &'static str)] {
    match kind {
        GeometryType::Polygon => &[("area", "Area"), ("perimeter", "Perimeter")],
        GeometryType::Polyline => &[("length", "Length")],
        GeometryType::Point => &[("x", "X coordinate"), ("y", "Y coordinate")],
        GeometryType::None => &[],
    }
}

/// The units `property` can be calculated in, as `(key, label)` pairs.
pub fn units(property: &str) -> &'static [(&'static str, &'static str)] {
    match property {
        "area" => &[
            ("m2", "Square metres"),
            ("km2", "Square kilometres"),
            ("ha", "Hectares"),
            ("ac", "Acres"),
            ("ft2", "Square feet"),
            ("mi2", "Square miles"),
        ],
        "length" | "perimeter" => &[("m", "Metres"), ("km", "Kilometres"), ("ft", "Feet"), ("mi", "Miles")],
        "x" | "y" => &[("deg", "Degrees (WGS84)"), ("projected", "Projected CRS units")],
        _ => &[],
    }
}

/// Runs `edit.calculateGeometry`: fills `field` (new or existing numeric) of the chosen features
/// with `property` in `unit`, as one undo step; on any error nothing changes.
pub fn run(s: &mut Session, p: &Json) -> CmdResult {
    let id = s.layer_id(p)?;
    let l = s.map().layer(id).ok_or("no layer")?;
    let property = p.get("property").and_then(Json::as_str).ok_or("property is required")?;
    let unit = p.get("unit").and_then(Json::as_str).ok_or("unit is required")?;
    if !properties(l.geometry_type()).iter().any(|(k, _)| *k == property) {
        return Err("property does not match the layer geometry".into());
    }
    if !units(property).iter().any(|(k, _)| *k == unit) {
        return Err("unit does not match the property".into());
    }
    let transformer = if unit == "projected" {
        let code = p.get("epsg").and_then(Json::as_u64).and_then(|n| u32::try_from(n).ok()).ok_or("EPSG code is required")?;
        let crs = Crs::from_epsg(code).filter(|c| !c.is_geographic()).ok_or("choose a supported projected EPSG code")?;
        Some(crs.transformer()?)
    } else {
        None
    };
    let field = p.get("field").and_then(Json::as_str).map(str::trim).filter(|n| !n.is_empty()).ok_or("field is required")?;
    let create = p.get("create").and_then(Json::as_bool).unwrap_or(false);
    let existing = l.dataset.field_index(field);
    let ty = if create {
        if existing.is_some() {
            return Err("field already exists".into());
        }
        if field.chars().count() > 64 || !field.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Err("field names use letters, digits and _ (at most 64)".into());
        }
        FieldType::Double
    } else {
        let ty = existing.and_then(|i| l.dataset.fields.get(i)).map(|f| f.ty).filter(|t| t.is_numeric()).ok_or("choose a numeric field")?;
        if p.get("overwrite").and_then(Json::as_bool) != Some(true) {
            return Err("confirm overwriting the existing field".into());
        }
        ty
    };
    let mut rows: Vec<usize> = match p.get("scope").and_then(Json::as_str) {
        Some("all") => (0..l.dataset.features.len()).collect(),
        Some("selected") if !l.selection.is_empty() => l.selection.clone(),
        Some("selected") => return Err("no features selected".into()),
        _ => return Err("scope must be all or selected".into()),
    };
    rows.sort_unstable();
    rows.dedup();
    if rows.is_empty() {
        return Err("no features to calculate".into());
    }
    if property == "area"
        && let Some(message) =
            rustgis_data::ambiguous_area_message(rows.iter().filter_map(|&r| l.dataset.features.get(r)?.geometry.as_ref()))
    {
        return Err(format!("{}: {message}", l.dataset.name));
    }
    let measure = Measure::from_key(property);
    let divisor = match measure {
        Some(m) => m.unit(unit).ok_or("unit does not match the property")?,
        None => 1.0,
    };
    let mut values = Vec::with_capacity(rows.len());
    let mut nulls = 0;
    for row in &rows {
        let f = l.dataset.features.get(*row).ok_or("selection contains a missing feature")?;
        let value = if let Some(g) = &f.geometry {
            if g.kind() != l.geometry_type() {
                return Err("feature geometry does not match the layer".into());
            }
            // Lengths and areas use longitude differences, so a shape drawn or buffered across the
            // antimeridian may run past ±180°; a coordinate has to be a real longitude.
            let max_x = if matches!(property, "x" | "y") { 180.0 } else { 360.0 };
            let mut valid = true;
            g.for_each_coord(|c| {
                valid &= c.is_finite() && (-max_x..=max_x).contains(&c.x) && (-90.0..=90.0).contains(&c.y);
            });
            if !valid {
                return Err(format!("feature {} has invalid coordinates", f.id));
            }
            if g.coord_count() == 0 {
                None
            } else if matches!(property, "x" | "y") {
                let c = match g {
                    Geometry::Point(c) => Some(*c),
                    Geometry::MultiPoint(v) => spherical_centre(v),
                    _ => None,
                };
                let c = match (c, &transformer) {
                    (Some(c), Some(t)) => Some(t.from_lonlat(c).ok_or_else(|| format!("feature {} cannot be projected", f.id))?),
                    (c, _) => c,
                };
                c.map(|c| if property == "x" { c.x } else { c.y })
            } else {
                measure.and_then(|m| g.wgs84_measure(m)).map(|v| v / divisor)
            }
        } else {
            None
        };
        let value = match value {
            Some(v) if !v.is_finite() || (matches!(property, "area" | "length" | "perimeter") && v < 0.0) => {
                return Err("invalid geometry measurement".into());
            }
            Some(v) if ty == FieldType::Integer => {
                let n = v.round();
                if !(-9223372036854775808.0..9223372036854775808.0).contains(&n) {
                    return Err("result exceeds the integer field range".into());
                }
                Value::Int(n as i64)
            }
            Some(v) => Value::Float(v),
            None => {
                nulls += 1;
                Value::Null
            }
        };
        values.push((*row, value));
    }
    let l = s.map_mut().layer_mut(id).ok_or("no layer")?;
    let fi = if create { l.dataset.ensure_field(field, FieldType::Double) } else { existing.ok_or("no field")? };
    let mut changes = Vec::with_capacity(values.len());
    for (row, value) in values {
        let f = l.dataset.features.get_mut(row).ok_or("no feature")?;
        let before = f.clone();
        *f.attrs.get_mut(fi).ok_or("missing attribute cell")? = value;
        changes.push(crate::cmd::design::Change { before: Some(before), row: Some(row) });
    }
    crate::cmd::design::apply(s, id, changes)?;
    Ok(json!({"rows": rows.len(), "nulls": nulls, "field": field}))
}
