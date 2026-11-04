//! Every command, grouped by ribbon tab.

pub mod arcgis;
pub mod catalogs;
pub mod clipboard;
pub mod connections;
pub mod cube;
pub mod cube_catalog;
pub mod deeplearning;
pub mod design;
pub mod directions;
pub mod earthdata;
pub mod edit;
pub mod effects;
pub mod embeddings;
pub mod geocode;
pub mod geolibre;
pub mod gp;
pub mod gpkg;
pub mod graphics;
pub mod group;
pub mod history;
pub mod imagery;
pub mod items;
pub mod labels;
pub mod layer;
pub mod layer_copy;
pub mod layout;
pub mod navigate;
pub mod nldi;
pub mod notebook;
pub mod pg;
pub mod plugins;
pub mod portals;
pub mod presentation;
pub mod project;
pub mod qgis;
pub mod raster;
pub mod repair;
pub mod report;
pub mod s3;
pub mod search;
pub mod select;
pub mod service;
pub mod service_added;
pub mod share;
pub mod sharing;
pub mod sources;
pub mod sql;
pub mod stac;
pub mod tiles;
pub mod time;
pub mod view;
pub mod webdata;
pub mod workflows;
pub mod workspace;
pub mod zarr;

use serde_json::Value;

use crate::{CommandSpec, Session};

/// Collects the engine command registry, including project importers, for every front end.
pub fn all() -> Vec<CommandSpec> {
    let mut v = Vec::new();
    v.extend(project::commands());
    v.extend(qgis::commands());
    v.extend(geolibre::commands());
    v.extend(navigate::commands());
    v.extend(geocode::commands());
    v.extend(directions::commands());
    v.extend(search::commands());
    v.extend(tiles::commands());
    v.extend(service::commands());
    v.extend(s3::commands());
    v.extend(sources::commands());
    v.extend(connections::commands());
    v.extend(layer::commands());
    v.extend(group::commands());
    v.extend(layer_copy::commands());
    v.extend(raster::commands());
    v.extend(repair::commands());
    v.extend(zarr::commands());
    v.extend(cube::commands());
    v.extend(cube_catalog::commands());
    v.extend(select::commands());
    v.extend(edit::commands());
    v.extend(clipboard::commands());
    v.extend(labels::commands());
    v.extend(graphics::commands());
    v.extend(layout::commands());
    v.extend(report::commands());
    v.extend(presentation::commands());
    v.extend(notebook::commands());
    v.extend(items::commands());
    v.extend(design::commands());
    v.extend(workflows::commands());
    v.extend(effects::commands());
    v.extend(imagery::commands());
    v.extend(deeplearning::commands());
    v.extend(view::commands());
    v.extend(workspace::commands());
    v.extend(gp::commands());
    v.extend(history::commands());
    v.extend(share::commands());
    v.extend(sql::commands());
    v.extend(pg::commands());
    v.extend(gpkg::commands());
    v.extend(plugins::commands());
    v.extend(sharing::commands());
    v.extend(stac::commands());
    v.extend(portals::commands());
    v.extend(catalogs::commands());
    v.extend(webdata::commands());
    v.extend(embeddings::commands());
    v.extend(earthdata::commands());
    v.extend(arcgis::commands());
    v.extend(nldi::commands());
    v.extend(time::commands());
    v
}

pub(crate) fn ps<'a>(p: &'a Value, k: &str) -> Option<&'a str> {
    p.get(k).and_then(Value::as_str)
}

pub(crate) fn f(p: &Value, k: &str) -> Option<f64> {
    p.get(k).and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))).filter(|x: &f64| x.is_finite())
}

/// A whole number from an integer, a float with no fraction, or a numeric string (forms send
/// numbers as floats).
pub(crate) fn uint(p: &Value, k: &str) -> Option<u64> {
    f(p, k).filter(|x| *x >= 0.0 && x.fract() == 0.0 && *x <= u64::MAX as f64).map(|x| x as u64)
}

/// The longest name a map or a layout keeps.
pub(crate) const MAX_NAME: usize = 200;

/// `base` (cut to [`MAX_NAME`] characters), or the first of `base 2`, `base 3`… that `taken`
/// doesn't claim; the number fits in the [`MAX_NAME`] characters.
pub(crate) fn numbered(base: &str, taken: impl Fn(&str) -> bool) -> String {
    numbered_within(base, MAX_NAME, taken)
}

/// [`numbered`] for names kept to `max` characters.
pub(crate) fn numbered_within(base: &str, max: usize, taken: impl Fn(&str) -> bool) -> String {
    let base: String = base.chars().take(max).collect();
    let mut name = base.clone();
    let mut k = 1u64;
    while taken(&name) {
        k += 1;
        let suffix = format!(" {k}");
        name = base.chars().take(max.saturating_sub(suffix.len())).chain(suffix.chars()).collect();
    }
    name
}

/// A copy's name: the one `given` (trimmed), else `<original> copy` with the original cut so
/// " copy" fits; then [`numbered`] by `taken`.
pub(crate) fn copy_name(given: Option<&str>, original: &str, taken: impl Fn(&str) -> bool) -> String {
    const COPY: &str = " copy";
    let base = match given.map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => n.to_string(),
        None => original.chars().take(MAX_NAME - COPY.len()).chain(COPY.chars()).collect(),
    };
    numbered(&base, taken)
}

pub(crate) fn b(p: &Value, k: &str) -> Option<bool> {
    p.get(k).and_then(|v| v.as_bool().or_else(|| v.as_str().map(|s| matches!(s, "true" | "yes" | "1" | "on"))))
}

pub(crate) fn need_layer(s: &Session) -> Option<&'static str> {
    match s.active_layer.and_then(|id| s.map().layer(id)) {
        Some(_) => None,
        None => Some("select a layer in the Layers pane first"),
    }
}

/// A feature (vector) layer is active.
pub(crate) fn need_feature_layer(s: &Session) -> Option<&'static str> {
    match s.active_layer.and_then(|id| s.map().layer(id)) {
        Some(l) if !l.is_raster() => None,
        Some(_) => Some("this works on feature layers, not rasters"),
        None => Some("select a layer in the Layers pane first"),
    }
}

pub(crate) fn need_layers(s: &Session) -> Option<&'static str> {
    if s.map().layers.is_empty() { Some("the map has no layers") } else { None }
}

pub(crate) fn need_selection(s: &Session) -> Option<&'static str> {
    if s.map().selection_count() == 0 { Some("nothing is selected") } else { None }
}
