//! Append vector layers without changing their geometry or losing attribute types.
use crate::{Inputs, Output, ToolResult};
use rustgis_data::{Coord, Dataset, Field, Geometry, GeometryType, Value};
use serde_json::Value as Json;
use std::collections::TryReserveError;

pub(crate) fn run(inputs: &dyn Inputs, params: &Json) -> Result<ToolResult, String> {
    let names = crate::layer_list(params.get("inputs").unwrap_or(&Json::Null))?;
    let layers =
        names.iter().map(|name| inputs.dataset(name).ok_or_else(|| format!("no layer named '{name}'"))).collect::<Result<Vec<_>, _>>()?;
    let first = layers.first().ok_or("Merge needs at least one vector layer")?;
    if first.geometry_type == GeometryType::None || layers.iter().any(|ds| ds.geometry_type != first.geometry_type) {
        return Err("Merge needs vector layers of the same geometry type".into());
    }
    let attributes = params.get("attributes").and_then(Json::as_bool).ok_or("attributes must be true or false")?;
    let name = params.get("output").and_then(Json::as_str).map(str::trim).filter(|n| !n.is_empty()).unwrap_or("Merged");
    let mut out = Dataset::new(name, first.geometry_type, vec![]);
    out.crs = first.crs;
    out.crs_incomplete = layers.iter().any(|ds| ds.crs_incomplete);
    let mut original_fields: Vec<Field> = Vec::new();
    let mut mappings = Vec::new();
    for ds in &layers {
        let mut mapping = Vec::new();
        if attributes {
            for field in &ds.fields {
                let index = original_fields
                    .iter()
                    .enumerate()
                    .position(|(i, f)| f.name.eq_ignore_ascii_case(&field.name) && f.ty == field.ty && !mapping.contains(&i))
                    .unwrap_or_else(|| {
                        let mut output_field = field.clone();
                        output_field.name = crate::tools::unique_field(&out, &field.name);
                        out.fields.push(output_field);
                        original_fields.push(field.clone());
                        out.fields.len() - 1
                    });
                mapping.push(index);
            }
        }
        mappings.push(mapping);
    }
    let selected = |index: usize| {
        let name = names.get(index).copied().unwrap_or("");
        inputs.selection(name)
    };
    let n = layers.iter().enumerate().try_fold(0usize, |n, (i, ds)| {
        let selection = selected(i);
        let count = if selection.is_empty() { ds.features.len() } else { selection.iter().filter(|row| **row < ds.features.len()).count() };
        n.checked_add(count).ok_or("Merge has too many features")
    })?;
    let no_memory = |e: TryReserveError| format!("Merge: not enough memory for {n} features: {e}");
    crate::control::begin(n);
    crate::control::check()?;
    out.features.try_reserve_exact(n).map_err(no_memory)?;
    for (i, ds) in layers.iter().enumerate() {
        let mapping = mappings.get(i).ok_or("Merge field mapping is missing")?;
        let selection = selected(i);
        let all = 0..if selection.is_empty() { ds.features.len() } else { 0 };
        for row in selection.iter().copied().chain(all) {
            let Some(feature) = ds.features.get(row) else { continue };
            crate::control::check()?;
            if feature.geometry.as_ref().is_some_and(|g| g.kind() != first.geometry_type) {
                return Err(format!("{}: a feature does not match the layer geometry type", ds.name));
            }
            let mut attrs = Vec::new();
            attrs.try_reserve_exact(out.fields.len()).map_err(no_memory)?;
            attrs.resize(out.fields.len(), Value::Null);
            for (source, target) in mapping.iter().enumerate() {
                if let Some(value) = attrs.get_mut(*target) {
                    *value = crate::tools::try_clone_value(ds.value(row, source)).map_err(no_memory)?;
                }
            }
            let geometry = feature.geometry.as_ref().map(clone_geometry).transpose().map_err(no_memory)?;
            out.push_next(geometry, attrs);
            crate::control::advance(1);
        }
    }
    Ok(ToolResult {
        losses: Vec::new(),
        output: Output::Layer(out),
        messages: vec![format!("Merge: {n} features from {} layers", layers.len())],
    })
}

/// Fallible geometry copies, so a large merge can return an allocation error.
pub(crate) fn clone_geometry(g: &Geometry) -> Result<Geometry, TryReserveError> {
    fn coords(source: &[Coord]) -> Result<Vec<Coord>, TryReserveError> {
        let mut out = Vec::new();
        out.try_reserve_exact(source.len())?;
        out.extend_from_slice(source);
        Ok(out)
    }
    fn parts(source: &[Vec<Coord>]) -> Result<Vec<Vec<Coord>>, TryReserveError> {
        let mut out = Vec::new();
        out.try_reserve_exact(source.len())?;
        for part in source {
            out.push(coords(part)?);
        }
        Ok(out)
    }
    Ok(match g {
        Geometry::Point(c) => Geometry::Point(*c),
        Geometry::MultiPoint(points) => Geometry::MultiPoint(coords(points)?),
        Geometry::Line(lines) => Geometry::Line(parts(lines)?),
        Geometry::Polygon(polygons) => {
            let mut out = Vec::new();
            out.try_reserve_exact(polygons.len())?;
            for polygon in polygons {
                out.push(parts(polygon)?);
            }
            Geometry::Polygon(out)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::Control;
    use rustgis_data::FieldType;
    use std::collections::HashMap;

    #[derive(Default)]
    struct MergeInputs {
        data: HashMap<String, Dataset>,
        selected: HashMap<String, Vec<usize>>,
    }

    impl Inputs for MergeInputs {
        fn dataset(&self, layer: &str) -> Option<&Dataset> {
            self.data.get(layer)
        }

        fn selection(&self, layer: &str) -> &[usize] {
            self.selected.get(layer).map(Vec::as_slice).unwrap_or(&[])
        }
    }

    fn point_layer(name: &str, count: usize) -> Dataset {
        let mut ds = Dataset::new(name, GeometryType::Point, vec![Field::new("n", FieldType::Integer)]);
        for i in 0..count {
            ds.push_next(Some(Geometry::Point(Coord::new(i as f64, 0.0))), vec![Value::Int(i as i64)]);
        }
        ds
    }

    #[test]
    fn merge_checks_cancel_and_reports_progress() {
        let mut inputs = MergeInputs {
            data: HashMap::from([("A".to_string(), point_layer("A", 3)), ("B".to_string(), point_layer("B", 4))]),
            ..Default::default()
        };
        let params = serde_json::json!({"inputs": ["A", "B"], "attributes": true});

        let counting = Control::cancelled_at_look(u64::MAX);
        let res = crate::run_with("merge", &inputs, &params, &counting).unwrap();
        let Output::Layer(out) = res.output else { panic!("expected layer") };
        assert_eq!(out.features.len(), 7);
        assert_eq!(counting.progress(), Some((7, 7)));
        assert_eq!(counting.looked(), 8);

        let cancel_pre_reserve = Control::cancelled_at_look(1);
        let err = crate::run_with("merge", &inputs, &params, &cancel_pre_reserve).unwrap_err();
        assert!(err.contains("cancelled"), "{err}");
        assert_eq!(cancel_pre_reserve.looked(), 1);
        assert_eq!(cancel_pre_reserve.progress(), Some((0, 7)));

        let cancel_mid = Control::cancelled_at_look(5);
        let err = crate::run_with("merge", &inputs, &params, &cancel_mid).unwrap_err();
        assert!(err.contains("cancelled"), "{err}");
        assert_eq!(cancel_mid.looked(), 5);
        assert_eq!(cancel_mid.progress(), Some((3, 7)));

        // Stale selection indices in A must not inflate progress past valid count n = 1 + 4 = 5.
        inputs.selected.insert("A".to_string(), vec![0, usize::MAX]);
        let counting_sel = Control::cancelled_at_look(u64::MAX);
        let res_sel = crate::run_with("merge", &inputs, &params, &counting_sel).unwrap();
        let Output::Layer(out_sel) = res_sel.output else { panic!("expected layer") };
        assert_eq!(out_sel.features.len(), 5);
        assert_eq!(counting_sel.progress(), Some((5, 5)));
    }
}
