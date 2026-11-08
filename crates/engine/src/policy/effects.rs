use crate::CommandSpec;
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    Edits,
    Files,
    Network,
    Plugin,
    Automation,
    Reserved,
    Replaces,
    History,
    Unreviewed,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Effects(u16);
impl Effects {
    pub const NONE: Self = Self(0);
    pub const UNREVIEWED: Self = Self::NONE.with(Effect::Unreviewed);
    /// Adds an effect without removing any previously declared reach.
    pub const fn with(self, effect: Effect) -> Self {
        Self(self.0 | 1 << effect as u16)
    }
    /// Tests one additive effect without treating the set as a trust level.
    pub const fn contains(self, effect: Effect) -> bool {
        self.0 & (1 << effect as u16) != 0
    }
    /// Combines reviewed effects from a command and its nested operations.
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
    /// Enumerates every declared effect so mixed Files/Network/Replaces remain additive.
    pub(super) fn iter(self) -> impl Iterator<Item = Effect> {
        [
            Effect::Edits,
            Effect::Files,
            Effect::Network,
            Effect::Plugin,
            Effect::Automation,
            Effect::Reserved,
            Effect::Replaces,
            Effect::History,
            Effect::Unreviewed,
        ]
        .into_iter()
        .filter(move |e| self.contains(*e))
    }
}
pub(crate) const PLACE_KEYS: &[&str] = &["path", "folder", "dir", "file", "url", "uri", "source", "output", "connection", "database"];
/// State-dependent additions to a reviewed declaration. Effects add; they never remove authority.
pub(super) fn declared(spec: &CommandSpec, params: &Value) -> Effects {
    let mut effects = spec.effects;
    if spec.mutates {
        effects = effects.with(Effect::Edits);
    }
    if spec.id == "map.body" && params.get("basemap").and_then(Value::as_bool) != Some(false) {
        effects = effects.with(Effect::Network);
    }
    if spec.id == "cube.open" && crate::cmd::cube_catalog::opens_zarr(params) {
        effects = effects.with(Effect::Files).with(Effect::Network);
    }
    if spec.id == "map.addTiles" {
        effects = effects.union(crate::cmd::tiles::add_reach(params));
    }
    let params = crate::cmd::items::layer_name_left_out(spec.id, params);
    // These owners read an explicit null path as no output file. Keep the existing narrow
    // in-memory raster contract, including rasterization, without ignoring any other place.
    let params =
        if matches!(spec.id, "raster.georeference" | "raster.project" | "raster.rasterize") && params.get("path") == Some(&Value::Null) {
            let mut given = params.into_owned();
            if let Some(o) = given.as_object_mut() {
                o.remove("path");
            }
            std::borrow::Cow::Owned(given)
        } else {
            params
        };
    if !effects.contains(Effect::Network) && crate::cmd::items::names_place(&params, PLACE_KEYS, 0) {
        effects = effects.with(Effect::Files);
    }
    effects
}
/// Adds only source reach of the actual current execution targets, without opening them.
pub(super) fn command(s: &crate::cmd::InputContext<'_>, spec: &CommandSpec, params: &Value, layer: Option<&rustgis_map::Layer>) -> Effects {
    let mut effects = declared(spec, params);
    effects = effects.union(match spec.id {
        "map.zoomToTiles" => crate::cmd::tiles::metadata_reach(s, params),
        "map.search" => crate::cmd::search::metadata_reach(s, params),
        "map.tiles" | "info.project" => crate::cmd::tiles::listing_reach(s.map()),
        "map.setTiles" => crate::cmd::tiles::set_reach(s, params),
        "time.export" => crate::cmd::time::export_reach(s, params),
        // A tile layer's export fetches its tiles (an archive, from its file).
        "share.exportRaster" => crate::cmd::tile_export::target(s, params)
            .ok()
            .flatten()
            .and_then(|i| s.map().tiles.get(i))
            .map_or(Effects::NONE, crate::cmd::tiles::tile_source_reach),
        _ => Effects::NONE,
    });
    if matches!(
        spec.id,
        "layout.export" | "presentation.export" | "share.exportMap" | "animation.export" | "project.thumbnail" | "map.thumbnail"
    ) && let Some(select) = spec.inputs
        && let Ok(inputs) = select(s, params)
    {
        // Reuse the export owner's actual frame/page selection, never every project map.
        for id in inputs.maps {
            if let Some(map) = s.project.maps.iter().find(|m| m.instance_id() == id) {
                effects = effects.union(crate::cmd::tiles::render_reach(map));
            }
        }
    }
    if spec.id == "layer.loadAll"
        && let Some(source) = layer.and_then(|layer| layer.source.as_ref())
    {
        effects = effects.union(source_reach(source));
    }
    if crate::cmd::time::follows_sources(spec.id) {
        for (_, _, source) in crate::cmd::time::following_sources(s) {
            effects = effects.with(Effect::Files);
            if source.starts_with("https://") || source.starts_with("http://") {
                effects = effects.with(Effect::Network);
            }
        }
    }
    effects
}
/// Pure reach of a source the executor will read, without opening it.
pub(super) fn source_reach(source: &rustgis_map::source::SourceLayer) -> Effects {
    let mut effects = Effects::NONE;
    let remote = source.path.as_deref().is_some_and(rustgis_data::source::is_remote_name);
    if (source.path.is_some() && !remote) || source.gdal.is_some() || source.duckdb.is_some() {
        effects = effects.with(Effect::Files);
    }
    if remote
        || source.postgis.is_some()
        || source.mysql.is_some()
        || source.sqlserver.is_some()
        || source.gdal.is_some()
        || source.duckdb.is_some()
    {
        effects = effects.with(Effect::Network);
    }
    effects
}
/// Legacy imported-item deny heuristic, retained in addition to declared effects.
pub(super) fn imported(command: &str, params: &Value) -> bool {
    const KEYS: &[&str] =
        &["path", "folder", "dir", "file", "url", "uri", "source", "output", "connection", "database", "data", "text", "poster", "locator"];
    let id = command.to_ascii_lowercase();
    let verb = id.split('.').nth(1).unwrap_or("");
    ["script.", "notebook.", "model.", "project.", "task.", "share.", "offline.", "cache.", "locator."].iter().any(|p| id.starts_with(p))
        || ["import", "export", "open", "save", "add", "package", "upload", "download"].iter().any(|w| verb.starts_with(w))
        || id == "raster.mosaic"
        || crate::cmd::items::names_place(&crate::cmd::items::layer_name_left_out(command, params), KEYS, 0)
}
