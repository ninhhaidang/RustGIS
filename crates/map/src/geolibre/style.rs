//! GeoLibre presentation translated onto an already loaded RustGIS feature layer.
use super::{Report, array, boolean, number, resolution, string};
use crate::symbology::{ClassMethod, LabelPlacement};
use crate::{Layer, Renderer, Rgba, Symbol};
use serde_json::Value;

/// Applies bounded style settings and reports expressions and unsupported combinations.
pub fn apply(layer: &mut Layer, record: &Value, report: &mut Report) {
    let name = layer.name.clone();
    let st = &record["style"];
    layer.visible = boolean(record, "visible", true);
    layer.transparency = (100.0 * (1.0 - number(record, "opacity", 1.0).clamp(0.0, 1.0))) as f32;
    layer.min_scale = scale(st, "minZoom", 0.0);
    layer.max_scale = scale(st, "maxZoom", 24.0);
    if layer.is_raster() {
        report.approximated(&name, "raster colouring uses RustGIS defaults");
        return;
    }
    let mut symbol = Symbol::default_for(layer.geometry_type(), 0);
    symbol.fill = color(st, "fillColor", Rgba::rgb(0x3b, 0x82, 0xf6), report, &name);
    symbol.stroke = color(st, "strokeColor", Rgba::rgb(0x1e, 0x40, 0xaf), report, &name);
    let fill = symbol.fill;
    symbol.fill = symbol.fill.with_alpha((symbol.fill.a() as f64 * number(st, "fillOpacity", 0.6).clamp(0.0, 1.0)).round() as u8);
    symbol.stroke_width = number(st, "strokeWidth", 2.0).clamp(0.0, 100.0) as f32;
    symbol.size = (2.0 * number(st, "circleRadius", 6.0).clamp(0.0, 128.0)) as f32;
    let field = string(st, "vectorStyleProperty");
    let mode = string(st, "vectorStyleMode");
    layer.renderer = Renderer::Single(symbol);
    let stops = array(st, "vectorStyleStops");
    if stops.len() > 256 {
        report.approximated(&name, "style limited to 256 stops");
    }
    if matches!(mode, "categorized" | "graduated") && layer.dataset.field_index(field).is_none() {
        report.approximated(&name, format!("style field {field} is absent; using base symbol"));
    } else if mode == "categorized" {
        let classes = stops
            .iter()
            .take(256)
            .map(|stop| {
                let value = match stop.get("value") {
                    Some(Value::String(s)) => s.clone(),
                    Some(v) => v.to_string(),
                    None => String::new(),
                };
                (value, painted(symbol, color(stop, "color", fill, report, &name), layer.geometry_type(), number(st, "fillOpacity", 0.6)))
            })
            .collect();
        layer.renderer = Renderer::Unique {
            field: field.into(),
            classes,
            other: symbol,
            ramp: Default::default(),
            class_labels: stops
                .iter()
                .take(256)
                .filter_map(|stop| {
                    let label = stop.get("label")?.as_str()?;
                    let value = stop.get("value")?;
                    Some((value.as_str().map(str::to_string).unwrap_or_else(|| value.to_string()), label.to_string()))
                })
                .collect(),
            other_label: None,
            class_strokes: Default::default(),
            other_strokes: Vec::new(),
            class_fills: Default::default(),
            other_fills: Vec::new(),
            expression: None,
        };
    } else if mode == "graduated" {
        let mut pairs: Vec<_> =
            stops.iter().take(256).filter_map(|s| Some((s.get("value")?.as_f64()?, color(s, "color", fill, report, &name)))).collect();
        pairs.sort_by(|a, b| a.0.total_cmp(&b.0));
        pairs.dedup_by(|a, b| a.0 == b.0);
        if let Some(first) = pairs.first().filter(|_| pairs.len() >= 2) {
            layer.renderer = Renderer::Graduated {
                field: field.into(),
                min: first.0,
                classes: pairs.len(),
                // GeoLibre stores class lower bounds and switches at equality. RustGIS
                // stores inclusive upper bounds: the previous float preserves that boundary.
                breaks: pairs.iter().skip(1).map(|p| p.0.next_down()).chain(std::iter::once(f64::MAX)).collect(),
                symbols: pairs.iter().map(|p| painted(symbol, p.1, layer.geometry_type(), number(st, "fillOpacity", 0.6))).collect(),
                ramp: Default::default(),
                method: ClassMethod::Manual,
                class_labels: pairs
                    .iter()
                    .enumerate()
                    .map(|(i, p)| {
                        let label = if i == 0 {
                            pairs.get(1).map(|next| format!("< {}", next.0)).unwrap_or_default()
                        } else if let Some(next) = pairs.get(i + 1) {
                            format!("{} – < {}", p.0, next.0)
                        } else {
                            format!("≥ {}", p.0)
                        };
                        (i, label)
                    })
                    .collect(),
                class_strokes: Default::default(),
                class_fills: Default::default(),
            };
        } else {
            report.approximated(&name, "graduated style has no numeric stops; using base symbol");
        }
    } else if !matches!(mode, "" | "single") {
        report.approximated(&name, format!("{mode} style drawn with the base symbol"));
    }
    if boolean(st, "proportionalSizeEnabled", false) {
        let field = string(st, "proportionalSizeProperty");
        if layer.geometry_type() != rustgis_data::GeometryType::Point {
            report.approximated(&name, "proportional sizing is supported only for point markers; using a fixed width");
        } else if mode == "single" || mode.is_empty() {
            if layer.dataset.field_index(field).is_some() {
                layer.renderer = Renderer::Proportional {
                    field: field.into(),
                    base: symbol,
                    min: number(st, "proportionalSizeMinValue", 0.0).clamp(-1e100, 1e100),
                    max: number(st, "proportionalSizeMaxValue", 100.0).clamp(-1e100, 1e100),
                    min_size: (2.0 * number(st, "proportionalSizeMinRadius", 4.0).clamp(0.0, 128.0)) as f32,
                    max_size: (2.0 * number(st, "proportionalSizeMaxRadius", 24.0).clamp(0.0, 128.0)) as f32,
                };
                report.approximated(&name, "proportional sizes use area scaling instead of MapLibre linear radius scaling");
            } else {
                report.approximated(&name, format!("size field {field} is absent; using a fixed size"));
            }
        } else {
            report.approximated(&name, "combined colour classification and proportional size: colour retained, size fixed");
        }
    }
    let labels = &st["labels"];
    layer.labels.enabled = boolean(labels, "enabled", false);
    let c = &mut layer.labels.default;
    c.field = string(labels, "field").into();
    c.size = number(labels, "size", 13.0).clamp(1.0, 128.0) as f32;
    c.color = color(labels, "color", Rgba::rgb(0x11, 0x18, 0x27), report, &name);
    c.halo = number(labels, "haloWidth", 1.5) > 0.0;
    c.min_scale = scale(labels, "minZoom", 0.0);
    c.max_scale = scale(labels, "maxZoom", 24.0);
    c.placement = if string(labels, "placement") == "line" { LabelPlacement::Curved } else { LabelPlacement::Auto };
    if layer.labels.enabled && layer.dataset.field_index(&c.field).is_none() {
        layer.labels.enabled = false;
        report.skipped(&name, "labels refer to a missing field");
    }
    if layer.labels.enabled
        && c.halo
        && (number(labels, "haloWidth", 1.5) != 1.0 || !matches!(string(labels, "haloColor"), "" | "#ffffff" | "#FFFFFF"))
    {
        report.approximated(&name, "label halo uses RustGIS's default colour and width");
    }
    for (key, v) in [
        ("filterExpression", record.get("filterExpression")),
        ("popup", record.get("popup")),
        ("label expression", labels.get("expression")),
    ] {
        if v.is_some_and(|v| !v.is_null() && v != "" && v != &serde_json::json!([])) {
            report.approximated(&name, format!("{key} is not evaluated"));
        }
    }
    if string(st, "strokeWidthUnit") == "meters" {
        report.approximated(&name, "ground-unit stroke width treated as screen pixels");
    }
    if boolean(st, "extrusionEnabled", false) || boolean(st, "elevation3dEnabled", false) {
        report.approximated(&name, "3D style drawn flat");
    }
}

/// Zoom limits become scale denominators at the equator, using RustGIS's 0.28 mm pixel.
fn scale(v: &Value, key: &str, default: f64) -> Option<f64> {
    let z = number(v, key, default).clamp(0.0, 24.0);
    (z != default).then(|| resolution(z) / 0.00028)
}
/// Hex CSS colours are lossless; other CSS syntaxes use a noted default.
fn color(v: &Value, key: &str, default: Rgba, report: &mut Report, name: &str) -> Rgba {
    let text = string(v, key);
    if text.is_empty() {
        return default;
    }
    if let Some(c) = Rgba::from_hex(text) {
        return c;
    }
    report.approximated(name, format!("{key} colour {text} uses a default (only hex colours supported)"));
    default
}
/// Changes the class colour without changing the saved outline or fill opacity.
fn painted(mut s: Symbol, c: Rgba, g: rustgis_data::GeometryType, opacity: f64) -> Symbol {
    s.fill = c.with_alpha((f64::from(c.a()) * opacity.clamp(0.0, 1.0)).round() as u8);
    if g == rustgis_data::GeometryType::Polyline {
        s.stroke = c;
    }
    s
}
