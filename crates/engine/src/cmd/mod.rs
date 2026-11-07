//! Every command, grouped by ribbon tab.

pub mod arcgis;
pub mod catalogs;
pub mod clipboard;
pub mod connections;
pub mod cube;
pub mod cube_catalog;
pub mod deeplearning;
pub mod design;
pub mod diagram;
pub mod directions;
pub mod earthdata;
pub mod edit;
pub mod effects;
pub mod embeddings;
pub mod exports;
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
pub mod reads;
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
    v.extend(diagram::commands());
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

/// Whether the map holds nothing but its basemap: no layer and no data tile layer (a cloud-optimised
/// GeoTIFF, a PMTiles or MBTiles archive, or an overlay). `layers` is the source of truth: a group only
/// holds ids of layers in `layers`, so a group with no live layers (an empty one, or one whose layers
/// were removed) does not count as content. Hidden content counts, a layer or a tile layer the
/// user hid alike: the map holds data and a view the user chose, so adding a layer leaves that view
/// alone. A data archive counts even when it draws as a basemap: it carries no overlay flag, so it
/// cannot be told from a basemap archive (a deliberate choice). The first content added to such a map
/// is framed; adding to a map that holds something leaves the view where the user put it.
pub(crate) fn map_is_empty(s: &Session) -> bool {
    let m = s.map();
    m.layers.is_empty()
        && !m.tiles.iter().any(|t| {
            t.overlay || matches!(t.kind, rustgis_map::TileKind::Cog | rustgis_map::TileKind::PmTiles | rustgis_map::TileKind::MbTiles)
        })
}

pub(crate) fn need_selection(s: &Session) -> Option<&'static str> {
    if s.map().selection_count() == 0 { Some("nothing is selected") } else { None }
}

/// Borrowed executor selectors for a known ordered map activation. Project data is never cloned.
#[derive(Clone, Copy)]
pub(crate) struct InputContext<'a> {
    session: &'a Session,
    pub project: &'a crate::Project,
    pub active_map: usize,
    pub active_layer: Option<u32>,
}
/// Shared by live executors and their pure input resolvers, preserving the same selector rules.
pub(crate) trait InputSource {
    fn input_context(&self) -> InputContext<'_>;
}
impl InputSource for Session {
    fn input_context(&self) -> InputContext<'_> {
        InputContext { session: self, project: &self.project, active_map: self.project.active_map, active_layer: self.active_layer }
    }
}
impl InputSource for InputContext<'_> {
    fn input_context(&self) -> InputContext<'_> {
        *self
    }
}
impl std::ops::Deref for InputContext<'_> {
    type Target = Session;
    fn deref(&self) -> &Session {
        self.session
    }
}
impl<'a> InputContext<'a> {
    /// Views live data with only the approved map/default scalar transition applied.
    pub fn at(session: &'a Session, selection: Selection) -> Self {
        Self { session, project: &session.project, active_map: selection.map, active_layer: selection.layer }
    }
    /// The real map, selected by the same valid index convention as Session::map.
    pub fn map(&self) -> &'a rustgis_map::Map {
        self.project.maps.get(self.active_map).or_else(|| self.project.maps.first()).unwrap_or_else(|| self.session.map())
    }
    /// Captures only map/default scalars; layer-selection bookkeeping is left exactly as stored.
    pub fn selection(&self) -> Selection {
        Selection { map: self.active_map, layer: self.active_layer, names_changed: false }
    }
    /// Resolves numeric, name-before-id and default layer choices for both planner and executor.
    pub fn layer_id(&self, p: &Value) -> Result<u32, String> {
        let map = self.map();
        match p.get("layer") {
            Some(Value::Number(n)) => n
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .filter(|id| map.layer(*id).is_some())
                .ok_or_else(|| format!("no layer with id {n}")),
            Some(Value::String(s)) => map
                .layer_by_name(s)
                .map(|l| l.id)
                .or_else(|| s.parse::<u32>().ok().filter(|id| map.layer(*id).is_some()))
                .ok_or_else(|| format!("no layer named '{s}'")),
            _ => {
                self.active_layer.filter(|id| map.layer(*id).is_some()).ok_or_else(|| "select a layer in the Layers pane first".to_string())
            }
        }
    }
    /// Uses the original raw selection fields, including their behavior after map activation.
    pub fn selected_layers(&self) -> Vec<u32> {
        let picked: &[u32] = if self.session.layer_selection_of == self.active_layer { &self.session.layer_selection } else { &[] };
        self.map().layers.iter().map(|l| l.id).filter(|id| picked.contains(id) || self.active_layer == Some(*id)).collect()
    }
}
/// Only scalar choices a pure owner transition can model without running a handler.
#[derive(Clone, Copy)]
pub(crate) struct Selection {
    pub map: usize,
    pub layer: Option<u32>,
    /// A prior rename preserves defaults, but subsequent named/stored map selectors need a new review.
    pub names_changed: bool,
}

/// Pure references selected by an executor, before it loads data or changes the session.
/// Stored selector state is private equality data; output roles are not consulted here.
pub(crate) struct Inputs {
    pub layers: Vec<(u64, u32)>,
    pub maps: Vec<u64>,
    pub tables: Vec<u32>,
    pub future_layers: Vec<String>,
    pub state: serde_json::Value,
    pub unresolved: bool,
    /// Public destination data, separately sanitized for review; never private selector state.
    pub review_places: Vec<String>,
    /// This owner actually selected the ordinary active/default layer.
    pub uses_default: bool,
}
impl Inputs {
    /// Captures existing layer identities, including their owning maps.
    pub fn layers<'a>(layers: impl IntoIterator<Item = (&'a rustgis_map::Map, &'a rustgis_map::Layer)>) -> Self {
        Self {
            layers: layers.into_iter().map(|(m, l)| (m.instance_id(), l.id)).collect(),
            maps: Vec::new(),
            tables: Vec::new(),
            future_layers: Vec::new(),
            state: serde_json::Value::Null,
            unresolved: false,
            review_places: Vec::new(),
            uses_default: false,
        }
    }
}

impl Inputs {
    /// Keeps scalar names and evolving defaults on the planner's existing in-run target paths.
    pub fn single_layer(s: &InputContext<'_>, p: &serde_json::Value) -> Result<Self, String> {
        match p.get("layer") {
            Some(serde_json::Value::String(name)) => Self::primary(s, p).or_else(|_| Self::named(s.map(), [name.as_str()])),
            Some(serde_json::Value::Number(_)) => Self::primary(s, p),
            _ => {
                let mut inputs = Self::layers(std::iter::empty());
                inputs.uses_default = true;
                Ok(inputs)
            }
        }
    }

    /// Literal names, exactly as map.layer_by_name resolves them (no numeric/default fallback).
    pub fn named<'a>(map: &rustgis_map::Map, names: impl IntoIterator<Item = &'a str>) -> Result<Self, String> {
        let mut inputs = Self::layers(std::iter::empty());
        for name in names {
            match map.layer_by_name(name) {
                Some(l) => inputs.layers.push((map.instance_id(), l.id)),
                None => inputs.future_layers.push(name.to_string()),
            }
        }
        Ok(inputs)
    }

    /// The ordinary primary selector uses Session's exact numeric/name/default semantics.
    pub fn primary(s: &impl InputSource, p: &serde_json::Value) -> Result<Self, String> {
        let context = s.input_context();
        let s = &context;
        let id = s.layer_id(p)?;
        Ok(Self {
            layers: vec![(s.map().instance_id(), id)],
            maps: Vec::new(),
            tables: Vec::new(),
            future_layers: Vec::new(),
            state: serde_json::Value::Null,
            unresolved: false,
            review_places: Vec::new(),
            uses_default: !matches!(p.get("layer"), Some(serde_json::Value::String(_) | serde_json::Value::Number(_))),
        })
    }
}

impl Inputs {
    /// A file-held or missing reference that an exact Ask cannot resolve without doing work.
    pub fn unresolved() -> Self {
        let mut inputs = Self::layers(std::iter::empty());
        inputs.unresolved = true;
        inputs
    }
    /// Combines operation-owned dependency selections without widening to unrelated namespaces.
    pub fn extend(&mut self, other: Self) {
        self.layers.extend(other.layers);
        self.maps.extend(other.maps);
        self.tables.extend(other.tables);
        self.future_layers.extend(other.future_layers);
        self.unresolved |= other.unresolved;
        self.review_places.extend(other.review_places);
        self.uses_default |= other.uses_default;
        if !other.state.is_null() {
            self.state = if self.state.is_null() { other.state } else { serde_json::json!([self.state, other.state]) };
        }
    }
}
