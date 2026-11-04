//! From a tile layer's address to a style ready to draw.
//!
//! Every kind of tile layer becomes a MapLibre [`Style`]: raster tiles are a style with one
//! raster source and layer, vector tiles get the built-in style ([`DEFAULT_STYLE`]), and a style
//! document is read as it is. Sources given by a TileJSON address are filled in from it. Each
//! step fetches through the cache, so resolving answers [`Resolve::Pending`] until the
//! documents have arrived, then the same resolved style every time.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::{Value, json};
use web_time::Instant;

use crate::cache::{self, Payload, State, Want};
use crate::style::{Source, Style};
use crate::{address, cog, mbtiles, pmtiles, template};

/// The kind of address.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Raster,
    Vector,
    Style,
    /// A PMTiles archive (raster or vector, as its header says).
    PmTiles,
    /// An MBTiles archive (raster or vector, as its metadata says): a file, or an upload.
    MbTiles,
    /// A cloud-optimised GeoTIFF by its web address, read in place by range.
    Cog,
}

/// A tile layer, as far as fetching and drawing it goes.
#[derive(Clone, Debug, PartialEq)]
pub struct Spec {
    pub kind: Kind,
    pub url: String,
    pub subdomains: Vec<String>,
    pub min_zoom: u8,
    pub max_zoom: u8,
    pub tile_size: u32,
    pub tms: bool,
    pub attribution: String,
    /// The vector tiles' layer names, when the service says them (an OGC API collection's): drawn
    /// in colours of their own, as an archive's layers are.
    pub layers: Vec<String>,
    /// MapLibre style layers (a JSON array) drawing vector tiles instead of their own colours,
    /// each from the tiles' source; empty for the default look.
    pub style_layers: String,
    /// Source layers left out (`roads`, `pois`): the style layers drawing them are dropped, so
    /// they neither draw nor are picked.
    pub hidden_layers: Vec<String>,
}

impl Spec {
    fn key(&self) -> String {
        format!(
            "{:?}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
            self.kind,
            self.url,
            self.subdomains.join(","),
            self.min_zoom,
            self.max_zoom,
            self.tile_size,
            self.tms,
            self.attribution,
            // Each name with its length, so ["a,b"] and ["a", "b"] differ.
            self.layers.iter().map(|l| format!("{}:{l}", l.len())).collect::<String>(),
            self.style_layers,
            self.hidden_layers.iter().map(|l| format!("{}:{l}", l.len())).collect::<String>()
        )
    }
}

/// A style whose sources all have tile addresses.
pub struct Resolved {
    pub style: Style,
    /// The style's sprites as (id, address without `.json`/`.png`), those RustGIS can
    /// fetch.
    pub sprites: Vec<(String, String)>,
    /// Credits from the layer and its sources, without duplicates.
    pub attribution: Vec<String>,
    /// What of the style was left out and why, said with every drawing of it as its error.
    pub left_out: Vec<String>,
    /// When it was resolved: one that left something out is resolved again once a failed
    /// fetch may be tried again ([`cache::RETRY`]).
    pub made: Mutex<Instant>,
    /// An elevation source is still on its way: the rest is drawn without it meanwhile, each
    /// drawing pending, and it is resolved again on the next one.
    pub waiting: bool,
}

impl Resolved {
    /// Whether it may still be used as it is.
    fn current(&self) -> bool {
        !self.waiting && (self.left_out.is_empty() || self.made.lock().unwrap_or_else(|e| e.into_inner()).elapsed() < cache::RETRY)
    }
}

/// What resolving a style again gives instead of `old`: `old` itself, renewed, when it is the same
/// style leaving out the same, so what was drawn of it (pictures and meshes, kept by style) is
/// kept; else `new`.
fn kept(old: Arc<Resolved>, new: Arc<Resolved>) -> Arc<Resolved> {
    if old.left_out != new.left_out
        || old.waiting != new.waiting
        || old.style != new.style
        || old.sprites != new.sprites
        || old.attribution != new.attribution
    {
        return new;
    }
    renewed(old)
}

/// `r`, current again until the next retry.
fn renewed(r: Arc<Resolved>) -> Arc<Resolved> {
    *r.made.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
    r
}

pub enum Resolve {
    Ready(Arc<Resolved>),
    Pending,
    Failed(String),
}

fn resolved() -> &'static Mutex<HashMap<String, Arc<Resolved>>> {
    static R: OnceLock<Mutex<HashMap<String, Arc<Resolved>>>> = OnceLock::new();
    R.get_or_init(Mutex::default)
}

/// The most resolved styles kept.
const MAX_RESOLVED: usize = 64;

/// Forgets the resolved styles, so their documents are read again (with [`crate::cache::clear`]).
pub(crate) fn forget() {
    resolved().lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// The built-in style for vector tiles without one: it knows the OpenMapTiles, Protomaps and
/// Shortbread layer names, and draws water, land use, parks, buildings, roads, boundaries and
/// place names from whichever of them the tiles have.
pub const DEFAULT_STYLE: &str = include_str!("default_style.json");

/// The style to draw `spec` with, once the documents it needs have been fetched.
pub fn resolve(spec: &Spec) -> Resolve {
    let key = spec.key();
    // A PMTiles archive on this computer is a path, not a web address.
    let local = spec.kind == Kind::PmTiles && pmtiles::is_local(&spec.url);
    let checked = if spec.kind == Kind::MbTiles {
        mbtiles::check(&spec.url)
    } else if local {
        pmtiles::check_local(&spec.url)
    } else {
        address::check(&spec.url, None)
    };
    if let Err(e) = checked {
        return Resolve::Failed(e);
    }
    let cached = resolved().lock().unwrap_or_else(|e| e.into_inner()).get(&key).cloned();
    if let Some(r) = cached.as_ref().filter(|r| r.current()) {
        return Resolve::Ready(r.clone());
    }
    settle(resolve_anew(spec, key.clone()), cached, spec, key)
}

/// What resolving `spec` again (`anew`) gives, with `cached`, the expired one under `key`.
fn settle(anew: Resolve, cached: Option<Arc<Resolved>>, spec: &Spec, key: String) -> Resolve {
    match (anew, cached) {
        // One that left something out is drawn as it was while that is fetched again, not blank.
        (Resolve::Pending, Some(old)) => Resolve::Ready(old),
        (Resolve::Ready(new), Some(old)) => {
            let r = kept(old, new);
            resolved().lock().unwrap_or_else(|e| e.into_inner()).insert(key, r.clone());
            Resolve::Ready(r)
        }
        // Or while it can't be read again: it is tried again after the next wait. Its own notice
        // still says what it leaves out; why it couldn't be read again goes to the log.
        (Resolve::Failed(e), Some(old)) => {
            log::warn!("{}: drawn as it was, as it could not be read again: {e}", spec.url);
            Resolve::Ready(renewed(old))
        }
        (other, _) => other,
    }
}

/// Reads `spec`'s style and resolves its sources, keeping the result under `key`.
fn resolve_anew(spec: &Spec, key: String) -> Resolve {
    let doc = match spec.kind {
        Kind::Raster => raster_style(spec),
        Kind::Vector => match vector_style(spec) {
            Some(d) => d,
            None => return Resolve::Failed("the built-in vector style could not be read".into()),
        },
        Kind::Style => match fetch_json(&spec.url) {
            Ok(Some(d)) => d,
            Ok(None) => return Resolve::Pending,
            Err(e) => return Resolve::Failed(e),
        },
        Kind::PmTiles => match pmtiles::info(&spec.url) {
            Ok(Some(info)) => {
                let archive = Archive {
                    tiles: pmtiles::template(&spec.url),
                    vector: info.tile_type.is_vector(),
                    min_zoom: info.min_zoom,
                    max_zoom: info.max_zoom,
                    attribution: info.attribution(),
                    layers: info.vector_layers().to_vec(),
                };
                match archive_style(spec, archive) {
                    Some(d) => d,
                    None => return Resolve::Failed("the built-in vector style could not be read".into()),
                }
            }
            Ok(None) => return Resolve::Pending,
            Err(e) => return Resolve::Failed(format!("{}: {e}", spec.url)),
        },
        Kind::Cog => match cog::info(&spec.url) {
            Ok(Some(info)) => {
                let archive = Archive {
                    tiles: cog::template(&spec.url),
                    vector: false,
                    min_zoom: info.min_zoom,
                    max_zoom: info.max_zoom,
                    attribution: String::new(),
                    layers: Vec::new(),
                };
                match archive_style(spec, archive) {
                    Some(d) => d,
                    None => return Resolve::Failed("the raster style could not be made".into()),
                }
            }
            Ok(None) => return Resolve::Pending,
            Err(e) => return Resolve::Failed(e),
        },
        Kind::MbTiles => match mbtiles::info_ready(&spec.url, web_time::Duration::ZERO) {
            Ok(Some(info)) => {
                let archive = Archive {
                    tiles: mbtiles::template(&spec.url),
                    vector: info.format.is_vector(),
                    min_zoom: info.min_zoom,
                    max_zoom: info.max_zoom,
                    attribution: info.attribution.clone(),
                    layers: info.vector_layers().to_vec(),
                };
                match archive_style(spec, archive) {
                    Some(d) => d,
                    None => return Resolve::Failed("the built-in vector style could not be read".into()),
                }
            }
            Ok(None) => return Resolve::Pending,
            Err(e) => return Resolve::Failed(e),
        },
    };
    let mut style = match Style::parse(&doc) {
        Ok(s) => s,
        Err(e) => return Resolve::Failed(format!("{}: {e}", spec.url)),
    };
    if !spec.hidden_layers.is_empty() {
        style.layers.retain(|l| l.source_layer.as_ref().is_none_or(|s| !spec.hidden_layers.contains(s)));
    }
    // Sources given by a TileJSON address.
    let base = spec.url.clone();
    let Sources { mut left_out, waiting } = match resolve_sources(&mut style, spec, &base) {
        Ok(Some(sources)) => sources,
        Ok(None) => return Resolve::Pending,
        Err(e) => return Resolve::Failed(e),
    };
    let mut attribution: Vec<String> = Vec::new();
    let mut credit = |a: &str| {
        let a = plain_text(a);
        if !a.is_empty() && !attribution.contains(&a) {
            attribution.push(a);
        }
    };
    credit(&spec.attribution);
    for (_, src) in &style.sources {
        if let Source::Vector { attribution, .. } | Source::Raster { attribution, .. } | Source::RasterDem { attribution, .. } = src {
            credit(attribution);
        }
    }
    let (sprites, sprites_left_out) = resolve_sprites(&style, &base);
    left_out.extend(sprites_left_out);
    let r = Arc::new(Resolved { style, attribution, sprites, left_out, made: Mutex::new(Instant::now()), waiting });
    let mut all = resolved().lock().unwrap_or_else(|e| e.into_inner());
    // A few layers are in use at a time: past that many, start again rather than grow.
    if all.len() >= MAX_RESOLVED {
        all.clear();
    }
    all.insert(key, r.clone());
    Resolve::Ready(r)
}

/// A style's sprites with their addresses resolved against `base`, those RustGIS can fetch, and
/// what was left out of them, and why, if a layer not hidden draws images and so misses them: the
/// sprites are the style's choice, and one that can't be fetched only loses its own icons.
fn resolve_sprites(style: &Style, base: &str) -> (Vec<(String, String)>, Vec<String>) {
    let mut sprites = Vec::new();
    let mut left_out = style.sprites_left_out.clone();
    let (mut mapbox, mut refused) = (Vec::new(), Vec::new());
    for (id, s) in &style.sprites {
        let address = template::join(base, s);
        if s.starts_with("mapbox://") {
            mapbox.push(id.as_str());
        } else if address::check(&address, Some(base)).is_err() {
            refused.push(id.as_str());
        } else {
            sprites.push((id.clone(), address));
        }
    }
    let total = style.sprites_listed;
    let mut say = |ids: &[&str], why: &str| {
        if !ids.is_empty() {
            let (n, ids) = (ids.len(), ids.join(", "));
            left_out.push(format!("{n} of {total} sprites left out, so their icons and patterns aren't drawn ({why}): {ids}"));
        }
    };
    say(&mapbox, "mapbox:// addresses need Mapbox's own library");
    say(&refused, "RustGIS won't fetch their addresses");
    if !style.layers.iter().any(|l| l.visible && l.draws_images()) {
        left_out.clear();
    }
    (sprites, left_out)
}

/// What resolving a style's sources left out of it.
#[derive(Debug, Default, PartialEq)]
struct Sources {
    /// Sources dropped, and why (see [`Resolved::left_out`]).
    left_out: Vec<String>,
    /// An elevation source still on its way, left out until it comes (see [`Resolved::waiting`]).
    waiting: bool,
}

/// Resolves the sources of `style` in place: what was left out of it, or `None` while some the
/// style can't be drawn without are still on their way.
fn resolve_sources(style: &mut Style, spec: &Spec, base: &str) -> Result<Option<Sources>, String> {
    let mut pending = false;
    let mut left_out = Vec::new();
    let mut waiting = false;
    for (id, src) in &mut style.sources {
        let resolved = resolve_source(src, spec, base).and_then(|pending| match src {
            Source::RasterDem { tiles, encoding, .. } if !pending => {
                if tiles.is_empty() {
                    Err("it gives no tile addresses".to_owned())
                } else if !encoding.fits() {
                    Err("its custom encoding gives heights too large to draw".to_owned())
                } else {
                    Ok(false)
                }
            }
            _ => Ok(pending),
        });
        // Only a source a layer draws from is waited for, or missed.
        let used = style.layers.iter().any(|l| l.source.as_deref() == Some(id.as_str()));
        match resolved {
            // Elevation on its way is drawn once it comes; the rest needn't wait for it.
            Ok(true) if matches!(src, Source::RasterDem { .. }) => {
                waiting |= used;
                *src = Source::Other;
            }
            Ok(p) => pending |= p,
            // Elevation shades what the rest of the style draws: without it, the rest still draws.
            Err(e) if matches!(src, Source::RasterDem { .. }) => {
                if used {
                    left_out.push(format!("elevation source {id} left out, so its relief isn't drawn: {e}"));
                }
                *src = Source::Other;
            }
            Err(e) => return Err(e),
        }
    }
    Ok((!pending).then_some(Sources { left_out, waiting }))
}

/// Fills in a style source's tile addresses: from its TileJSON address when it gives no tiles
/// (`Ok(true)` while that is on its way), resolved against `base` and checked.
fn resolve_source(src: &mut Source, spec: &Spec, base: &str) -> Result<bool, String> {
    let (tiles, url) = match src {
        Source::Vector { tiles, url, .. } | Source::Raster { tiles, url, .. } | Source::RasterDem { tiles, url, .. } => (tiles, url),
        Source::Other => return Ok(false),
    };
    if tiles.is_empty()
        && let Some(u) = url.clone()
    {
        if u.starts_with("mapbox://") {
            return Err(format!("{u}: mapbox:// addresses need Mapbox's own library"));
        }
        let u = template::join(base, &u);
        // An ArcGIS vector tile service answers its TileJSON-like description as JSON only
        // when asked (`?f=json`); its styles point at the service itself.
        let path = u.split(['?', '#']).next().unwrap_or(&u).trim_end_matches('/').to_string();
        // The query's other parameters (a token) stay; `f` is the one asked for.
        let others: Vec<&str> = u
            .split('#')
            .next()
            .and_then(|x| x.split_once('?'))
            .map(|(_, q)| q)
            .unwrap_or("")
            .split('&')
            .filter(|kv| !kv.is_empty() && !kv.split_once('=').map(|(k, _)| k).unwrap_or(kv).eq_ignore_ascii_case("f"))
            .collect();
        let u = if path.to_ascii_lowercase().ends_with("/vectortileserver") {
            // With the slash, so the tile paths it gives resolve inside the service.
            if others.is_empty() { format!("{path}/?f=json") } else { format!("{path}/?f=json&{}", others.join("&")) }
        } else {
            u
        };
        // A style's TileJSON is the style's choice; a vector layer's is the user's.
        address::check(&u, (spec.kind == Kind::Style).then_some(base))?;
        match fetch_json(&u)? {
            Some(tj) => {
                src.apply_tilejson(&tj);
                // Tile addresses relative to the TileJSON document, which chose them.
                if let Source::Vector { tiles, .. } | Source::Raster { tiles, .. } | Source::RasterDem { tiles, .. } = src {
                    for t in tiles.iter_mut() {
                        *t = template::join(&u, t);
                        address::check(t, Some(&u))?;
                    }
                }
            }
            None => return Ok(true),
        }
    } else if let Source::Vector { tiles, .. } | Source::Raster { tiles, .. } | Source::RasterDem { tiles, .. } = src {
        for t in tiles.iter_mut() {
            // Tiles from an archive: read from the archive the layer names.
            if (spec.kind == Kind::PmTiles && pmtiles::is_tile(t))
                || (spec.kind == Kind::MbTiles && mbtiles::is_tile(t))
                || (spec.kind == Kind::Cog && cog::is_tile(t))
            {
                continue;
            }
            *t = template::join(base, t);
            // A style's tile addresses are the style's choice; a raster or vector layer's
            // template is the user's.
            address::check(t, (spec.kind == Kind::Style).then_some(base))?;
        }
    }
    Ok(false)
}

/// A vector archive's source layers, as its metadata lists them (`vector_layers`) and
/// [`source_layers::of_metadata`](rustgis_data::source_layers::of_metadata) keeps them: the same
/// names its default look draws from. `None` for other layers, an archive that can't be read,
/// and while a PMTiles header is on its way (asking for it starts reading it in the background;
/// an MBTiles file's metadata is read here, the first time it is asked for).
pub fn source_layers(kind: Kind, url: &str) -> Option<Vec<String>> {
    match kind {
        Kind::PmTiles => pmtiles::info(url).ok().flatten().filter(|i| i.tile_type.is_vector()).map(|i| i.vector_layers().to_vec()),
        Kind::MbTiles => mbtiles::info_ready(url, web_time::Duration::ZERO)
            .ok()
            .flatten()
            .filter(|i| i.format.is_vector())
            .map(|i| i.vector_layers().to_vec()),
        _ => None,
    }
}

/// The document at `url`: `Ok(None)` while it is on its way.
fn fetch_json(url: &str) -> Result<Option<Value>, String> {
    match cache::get(url, Want::Json) {
        State::Ready(p) => match &*p {
            Payload::Json(v) => Ok(Some(v.clone())),
            _ => Err(format!("{url}: not a JSON document")),
        },
        State::Pending => Ok(None),
        State::Failed(e) => Err(format!("{url}: {e}")),
    }
}

/// The tile addresses for a template with `{s}` expanded over its subdomains, and TMS rows
/// written as `{-y}`.
fn templates(spec: &Spec) -> Vec<String> {
    // (Not in a layer's fragment naming the map tile it is for, `#rustgis-warp=`: #769.)
    let url = match spec.url.split_once(rustgis_data::ogc::warp::MARK) {
        Some((base, layout)) if spec.tms => format!("{}{}{layout}", base.replace("{y}", "{-y}"), rustgis_data::ogc::warp::MARK),
        _ if spec.tms => spec.url.replace("{y}", "{-y}"),
        _ => spec.url.clone(),
    };
    if url.contains("{s}") {
        let subs: Vec<String> = if spec.subdomains.is_empty() { vec!["a".into(), "b".into(), "c".into()] } else { spec.subdomains.clone() };
        subs.iter().map(|s| url.replace("{s}", s)).collect()
    } else {
        vec![url]
    }
}

fn raster_style(spec: &Spec) -> Value {
    json!({
        "version": 8,
        "sources": {"raster": {
            "type": "raster", "tiles": templates(spec), "tileSize": spec.tile_size,
            "minzoom": spec.min_zoom, "maxzoom": spec.max_zoom,
        }},
        "layers": [{"id": "raster", "type": "raster", "source": "raster"}],
    })
}

/// A tile archive (PMTiles or MBTiles), as far as styling it goes.
struct Archive {
    /// The template of its tile addresses.
    tiles: String,
    vector: bool,
    min_zoom: u8,
    max_zoom: u8,
    attribution: String,
    /// The vector layers its metadata lists.
    layers: Vec<String>,
}

/// The style for a tile archive: a raster layer for picture tiles; for vector tiles the
/// chosen source layers or style when given, else the built-in style for a known basemap, or a
/// layer for each of the archive's vector layers (points, lines and polygons in their own colour).
fn archive_style(spec: &Spec, info: Archive) -> Option<Value> {
    let tiles = vec![info.tiles];
    let (minzoom, maxzoom) = (info.min_zoom, info.max_zoom);
    let attribution = if spec.attribution.is_empty() { info.attribution } else { spec.attribution.clone() };
    if !info.vector {
        return Some(json!({
            "version": 8,
            "sources": {"raster": {
                "type": "raster", "tiles": tiles, "tileSize": spec.tile_size,
                "minzoom": minzoom, "maxzoom": maxzoom, "attribution": attribution,
            }},
            "layers": [{"id": "raster", "type": "raster", "source": "raster"}],
        }));
    }
    let source = json!({"type": "vector", "tiles": tiles, "minzoom": minzoom, "maxzoom": maxzoom, "attribution": attribution});
    if let Some(doc) = own_style(spec, &source) {
        return Some(doc);
    }
    if !spec.layers.is_empty() {
        return Some(named_layers_style(spec, source, &spec.layers));
    }
    let names = info.layers;
    let known = [
        "water",
        "roads",
        "transportation",
        "buildings",
        "building",
        "landuse",
        "landcover",
        "boundaries",
        "boundary",
        "places",
        "place",
        "earth",
        "streets",
    ];
    if names.iter().filter(|n| known.contains(&n.as_str())).count() >= 3 {
        let mut doc: Value = serde_json::from_str(DEFAULT_STYLE).ok()?;
        *doc.get_mut("sources")?.get_mut("vector")? = source;
        return Some(doc);
    }
    Some(named_layers_style(spec, source, &names))
}

/// Each named vector layer in a colour of its own (an archive's that isn't a basemap, or the
/// layers a service named).
fn named_layers_style(spec: &Spec, source: Value, names: &[String]) -> Value {
    const COLOURS: [&str; 8] = ["#e6550d", "#3182bd", "#31a354", "#756bb1", "#d6616b", "#8c6d31", "#17becf", "#bcbd22"];
    let mut layers = Vec::new();
    // Each source starts the palette somewhere of its own, so two of them look apart.
    let start = spec.url.bytes().fold(0usize, |h, b| h.wrapping_mul(31).wrapping_add(usize::from(b)));
    for (i, name) in names.iter().take(64).enumerate() {
        let c = COLOURS.get(start.wrapping_add(i) % COLOURS.len()).copied().unwrap_or("#3182bd");
        layers.push(json!({"id": format!("{name}-fill"), "type": "fill", "source": "vector", "source-layer": name,
            "filter": ["==", "$type", "Polygon"], "paint": {"fill-color": c, "fill-opacity": 0.35}}));
        layers.push(json!({"id": format!("{name}-line"), "type": "line", "source": "vector", "source-layer": name,
            "filter": ["in", "$type", "LineString", "Polygon"], "paint": {"line-color": c, "line-width": 1}}));
        layers.push(json!({"id": format!("{name}-point"), "type": "circle", "source": "vector", "source-layer": name,
            "filter": ["==", "$type", "Point"], "paint": {"circle-color": c, "circle-radius": 3, "circle-stroke-color": "#ffffff", "circle-stroke-width": 1}}));
    }
    json!({"version": 8, "sources": {"vector": source}, "layers": layers})
}

/// The most of a layer's own style layers drawn: `rustgis_map::tiles::MAX_STYLE_LAYER_COUNT`
/// lets a layer keep as many (the engine's tests check the two agree).
pub const MAX_OWN_STYLE_LAYERS: usize = 256;

/// The style a layer's own style layers make (`spec.style_layers`), every layer drawing from
/// `source`; `None` without them (or when they don't read).
fn own_style(spec: &Spec, source: &Value) -> Option<Value> {
    if spec.style_layers.is_empty() {
        return None;
    }
    let Ok(Value::Array(mut layers)) = serde_json::from_str::<Value>(&spec.style_layers) else { return None };
    layers.truncate(MAX_OWN_STYLE_LAYERS);
    for l in &mut layers {
        if let Some(o) = l.as_object_mut() {
            o.insert("source".into(), json!("vector"));
        }
    }
    Some(json!({"version": 8, "sources": {"vector": source}, "layers": layers}))
}

fn vector_style(spec: &Spec) -> Option<Value> {
    // Layers the service named: each in a colour of its own.
    // They keep their own colours even when their names are a basemap's (the archive guess), and
    // the template's options (`tms`, `{s}`) apply as for any template.
    if !spec.layers.is_empty() && spec.url.contains("{z}") {
        let source = json!({"type": "vector", "tiles": templates(spec), "minzoom": spec.min_zoom, "maxzoom": spec.max_zoom, "attribution": spec.attribution});
        if let Some(doc) = own_style(spec, &source) {
            return Some(doc);
        }
        let names: Vec<String> = spec.layers.iter().take(64).cloned().collect();
        return Some(named_layers_style(spec, source, &names));
    }
    let mut doc: Value = serde_json::from_str(DEFAULT_STYLE).ok()?;
    let source = if spec.url.contains("{z}") {
        json!({"type": "vector", "tiles": templates(spec), "minzoom": spec.min_zoom, "maxzoom": spec.max_zoom})
    } else {
        // A TileJSON address.
        json!({"type": "vector", "url": spec.url})
    };
    if let Some(own) = own_style(spec, &source) {
        return Some(own);
    }
    *doc.get_mut("sources")?.get_mut("vector")? = source;
    Some(doc)
}

/// Attribution HTML as plain text: tags dropped, entities decoded.
pub fn plain_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    let out = out
        .replace("&copy;", "©")
        .replace("&amp;", "&")
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"");
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(kind: Kind, url: &str) -> Spec {
        Spec {
            kind,
            url: url.into(),
            subdomains: Vec::new(),
            min_zoom: 0,
            max_zoom: 19,
            tile_size: 256,
            tms: false,
            attribution: "&copy; <a href=\"x\">OSM</a>".into(),
            layers: Vec::new(),
            style_layers: String::new(),
            hidden_layers: Vec::new(),
        }
    }

    #[test]
    fn resolves_raster_and_vector_templates_at_once() {
        let Resolve::Ready(r) = resolve(&spec(Kind::Raster, "https://{s}.tile.example.com/{z}/{x}/{y}.png")) else { panic!("not ready") };
        assert_eq!(r.attribution, vec!["© OSM".to_string()]);
        match r.style.sources.first().map(|s| &s.1) {
            Some(Source::Raster { tiles, .. }) => assert_eq!(tiles.len(), 3),
            _ => panic!("no raster source"),
        }
        let Resolve::Ready(v) = resolve(&spec(Kind::Vector, "https://tiles.example.com/{z}/{x}/{y}.pbf")) else { panic!("not ready") };
        assert!(v.style.layers.len() > 10);
        // Layers a service named (an OGC API collection's) are drawn each in a colour of its own.
        let mut named = spec(Kind::Vector, "https://tiles.example.com/vineyards/{z}/{y}/{x}?f=mvt");
        named.layers = vec!["vineyards".into()];
        let Resolve::Ready(v) = resolve(&named) else { panic!("not ready") };
        assert_eq!(v.style.layers.len(), 3, "fill, line and point of the one layer");
        // Named layers keep their own colours even when the names are a basemap's, and the
        // template's options apply (TMS rows, subdomains).
        let mut basemapish = spec(Kind::Vector, "https://{s}.tiles.example.com/osm/{z}/{y}/{x}.pbf");
        basemapish.layers = vec!["water".into(), "roads".into(), "buildings".into()];
        basemapish.tms = true;
        let Resolve::Ready(v) = resolve(&basemapish) else { panic!("not ready") };
        assert_eq!(v.style.layers.len(), 9, "three named layers, not the basemap style");
        match v.style.sources.first().map(|s| &s.1) {
            Some(Source::Vector { tiles, .. }) => {
                assert_eq!(tiles.len(), 3);
                assert!(tiles.iter().all(|t| t.contains("{-y}") && !t.contains("{s}")), "{tiles:?}");
            }
            _ => panic!("no vector source"),
        }
    }

    #[test]
    fn own_style_layers_draw_vector_tiles() {
        let mut own = spec(Kind::Vector, "https://tiles.example.com/fields/{z}/{x}/{y}.pbf");
        own.style_layers = r##"[{"id": "f", "type": "fill", "source": "elsewhere", "source-layer": "fields",
            "paint": {"fill-color": ["step", ["get", "score"], "#d73027", 50, "#1a9850"]}}]"##
            .into();
        let Resolve::Ready(v) = resolve(&own) else { panic!("not ready") };
        assert_eq!(v.style.layers.len(), 1);
        assert_eq!(v.style.layers[0].source.as_deref(), Some("vector"), "drawn from the tiles' own source");
        // Ones that don't read leave the default look.
        own.style_layers = "{not json".into();
        let Resolve::Ready(v) = resolve(&own) else { panic!("not ready") };
        assert!(v.style.layers.len() > 10);
    }

    /// Explicit archive layers take precedence over metadata and the basemap guess; a custom
    /// style still decides which source layers it draws, as for vector tile templates.
    #[test]
    fn archive_source_layers_are_drawn_instead_of_the_default_look() {
        let archive = || Archive {
            tiles: "pmtiles:{z}/{x}/{y}@atlas.pmtiles".into(),
            vector: true,
            min_zoom: 0,
            max_zoom: 14,
            attribution: String::new(),
            layers: vec!["water".into(), "roads".into(), "buildings".into(), "fields".into()],
        };
        let mut selected = spec(Kind::PmTiles, "atlas.pmtiles");
        let default = archive_style(&selected, archive()).unwrap();
        for names in [vec!["fields"], vec!["water", "roads", "buildings"]] {
            selected.layers = names.iter().map(|s| s.to_string()).collect();
            let doc = archive_style(&selected, archive()).unwrap();
            let layers = doc["layers"].as_array().unwrap();
            assert_eq!(layers.len(), names.len() * 3, "fill, line and point for each chosen layer");
            assert!(layers.iter().all(|l| names.contains(&l["source-layer"].as_str().unwrap())));
            assert_ne!(doc["layers"], default["layers"]);
        }
        selected.style_layers = r#"[{"id":"own","type":"fill","source-layer":"fields"}]"#.into();
        let doc = archive_style(&selected, archive()).unwrap();
        assert_eq!(doc["layers"].as_array().unwrap().len(), 1);
        assert_eq!(doc["layers"][0]["source-layer"], "fields");
        assert_eq!(doc["layers"][0]["source"], "vector");
    }

    #[test]
    fn layer_lists_key_apart() {
        let mut one = spec(Kind::Vector, "https://t.example.com/{z}/{x}/{y}.pbf");
        one.layers = vec!["a,b".into()];
        let mut two = one.clone();
        two.layers = vec!["a".into(), "b".into()];
        assert_ne!(one.key(), two.key());
    }

    #[test]
    fn refuses_forbidden_addresses() {
        assert!(matches!(resolve(&spec(Kind::Raster, "http://169.254.169.254/{z}/{x}/{y}.png")), Resolve::Failed(_)));
        // A self-hosted server the user names is fine.
        assert!(matches!(resolve(&spec(Kind::Raster, "http://localhost:8080/{z}/{x}/{y}.png")), Resolve::Ready(_)));
    }

    #[test]
    fn an_unreadable_elevation_source_is_left_out_and_said() {
        let doc = serde_json::json!({"version": 8, "sources": {
            "dem": {"type": "raster-dem", "url": "http://169.254.169.254/dem.json"},
            "sat": {"type": "raster", "tiles": ["https://t.example.com/{z}/{x}/{y}.png"]}},
            "layers": [{"id": "shade", "type": "hillshade", "source": "dem"}, {"id": "sat", "type": "raster", "source": "sat"}]});
        let mut style = Style::parse(&doc).unwrap();
        let base = "https://t.example.com/style.json";
        let left_out = resolve_sources(&mut style, &spec(Kind::Style, base), base).unwrap().unwrap().left_out;
        assert_eq!(left_out.len(), 1);
        assert!(left_out[0].starts_with("elevation source dem left out"), "{left_out:?}");
        assert!(matches!(style.sources.iter().find(|s| s.0 == "dem").map(|s| &s.1), Some(Source::Other)));
        // A source the style can't draw without still fails it.
        let doc = serde_json::json!({"version": 8, "sources": {"sat": {"type": "raster", "url": "http://169.254.169.254/sat.json"}}, "layers": []});
        let mut style = Style::parse(&doc).unwrap();
        assert!(resolve_sources(&mut style, &spec(Kind::Style, base), base).is_err());
        // Clean sources leave nothing out.
        let doc = serde_json::json!({"version": 8, "sources": {"sat": {"type": "raster", "tiles": ["https://t.example.com/{z}/{x}/{y}.png"]}}, "layers": []});
        let mut style = Style::parse(&doc).unwrap();
        assert_eq!(resolve_sources(&mut style, &spec(Kind::Style, base), base), Ok(Some(Sources::default())));
        // Only a source a layer draws from is missed; the others are dropped without a word.
        let left_out_of = |doc: serde_json::Value| {
            let mut style = Style::parse(&doc).unwrap();
            resolve_sources(&mut style, &spec(Kind::Style, base), base).unwrap().unwrap().left_out
        };
        let unused = serde_json::json!({"version": 8, "sources": {"dem": {"type": "raster-dem", "url": "http://169.254.169.254/dem.json"}}, "layers": []});
        assert!(left_out_of(unused).is_empty());
        // One with no tiles to read, or heights too large for its encoding, is left out too.
        for dem in [
            serde_json::json!({"type": "raster-dem"}),
            serde_json::json!({"type": "raster-dem", "tiles": ["https://t.example.com/{z}/{x}/{y}.png"], "encoding": "custom", "redFactor": 1e40}),
        ] {
            let doc = serde_json::json!({"version": 8, "sources": {"dem": dem}, "layers": [{"id": "shade", "type": "hillshade", "source": "dem"}]});
            assert_eq!(left_out_of(doc).len(), 1);
        }
    }

    #[test]
    fn a_readable_elevation_source_is_kept() {
        let doc = serde_json::json!({"version": 8, "sources": {"dem": {"type": "raster-dem", "tiles": ["https://t.example.com/{z}/{x}/{y}.png"]}},
            "layers": [{"id": "shade", "type": "hillshade", "source": "dem"}]});
        let mut style = Style::parse(&doc).unwrap();
        let base = "https://t.example.com/style.json";
        assert_eq!(resolve_sources(&mut style, &spec(Kind::Style, base), base), Ok(Some(Sources::default())));
        assert!(
            matches!(style.source("dem"), Some(Source::RasterDem { tiles, .. }) if tiles == &["https://t.example.com/{z}/{x}/{y}.png"])
        );
    }

    #[test]
    fn an_elevation_source_on_its_way_holds_back_only_its_own_layers() {
        // Its TileJSON isn't here yet (the first fetch of an address is on its way).
        let doc = serde_json::json!({"version": 8, "sources": {
            "dem": {"type": "raster-dem", "url": "https://on-its-way.example.com/dem.json"},
            "sat": {"type": "raster", "tiles": ["https://t.example.com/{z}/{x}/{y}.png"]}},
            "layers": [{"id": "shade", "type": "hillshade", "source": "dem"}, {"id": "sat", "type": "raster", "source": "sat"}]});
        let mut style = Style::parse(&doc).unwrap();
        let base = "https://t.example.com/style.json";
        let sources = resolve_sources(&mut style, &spec(Kind::Style, base), base).unwrap().expect("drawn without it meanwhile");
        assert_eq!(sources, Sources { left_out: Vec::new(), waiting: true }, "nothing lost, only waited for");
        assert!(matches!(style.source("dem"), Some(Source::Other)));
        assert!(matches!(style.source("sat"), Some(Source::Raster { .. })));
        // One no layer draws from isn't waited for.
        let unused = serde_json::json!({"version": 8, "sources": {"dem": {"type": "raster-dem", "url": "https://unused-on-its-way.example.com/dem.json"}}, "layers": []});
        let mut style = Style::parse(&unused).unwrap();
        assert_eq!(resolve_sources(&mut style, &spec(Kind::Style, base), base), Ok(Some(Sources::default())));
        assert!(matches!(style.source("dem"), Some(Source::Other)));
        // Drawn meanwhile, pending (so it is drawn again, and an export waits), without a notice;
        // resolved again on the next drawing.
        let doc = serde_json::json!({"version": 8, "sources": {}, "layers": [{"id": "bg", "type": "background", "paint": {"background-color": "#f00"}}]});
        let r = Resolved {
            style: Style::parse(&doc).unwrap(),
            sprites: Vec::new(),
            attribution: Vec::new(),
            left_out: Vec::new(),
            made: Mutex::new(Instant::now()),
            waiting: true,
        };
        assert!(!r.current());
        let s = spec(Kind::Style, "https://waiting.example.com/style.json");
        resolved().lock().unwrap().insert(s.key(), Arc::new(r));
        let mut pm = tiny_skia::Pixmap::new(4, 4).unwrap();
        let frame = crate::draw::Frame { left: 0.0, top: 400.0, m_per_px: 100.0, ppp: 1.0 };
        let drawn = crate::draw::draw(&mut pm, &s, 1.0, &frame, false);
        assert!(drawn.pending && drawn.error.is_none());
        assert_eq!(pm.pixel(1, 1).map(|p| (p.red(), p.alpha())), Some((255, 255)), "the rest is drawn");
    }

    #[test]
    fn a_relief_layer_whose_source_was_left_out_draws_nothing() {
        // Lit from every side with white, a hillshade would show on any slope drawn.
        let doc = serde_json::json!({"version": 8, "sources": {"dem": {"type": "raster-dem", "url": "http://169.254.169.254/dem.json"}},
            "layers": [
                {"id": "bg", "type": "background", "paint": {"background-color": "#f00"}},
                {"id": "shade", "type": "hillshade", "source": "dem", "paint": {"hillshade-highlight-color": "#fff", "hillshade-shadow-color": "#fff"}},
                {"id": "relief", "type": "color-relief", "source": "dem",
                    "paint": {"color-relief-color": ["interpolate", ["linear"], ["elevation"], 0, "#00f", 10, "#00f"]}}]});
        let mut style = Style::parse(&doc).unwrap();
        let base = "https://t.example.com/style.json";
        let sources = resolve_sources(&mut style, &spec(Kind::Style, base), base).unwrap().unwrap();
        assert_eq!(sources.left_out.len(), 1);
        assert!(matches!(style.source("dem"), Some(Source::Other)));
        let r = Resolved {
            style,
            sprites: Vec::new(),
            attribution: Vec::new(),
            left_out: sources.left_out,
            made: Mutex::new(Instant::now()),
            waiting: false,
        };
        let s = spec(Kind::Style, "https://relief-left-out.example.com/style.json");
        resolved().lock().unwrap().insert(s.key(), Arc::new(r));
        let mut pm = tiny_skia::Pixmap::new(8, 8).unwrap();
        let frame = crate::draw::Frame { left: 0.0, top: 800.0, m_per_px: 100.0, ppp: 1.0 };
        let drawn = crate::draw::draw(&mut pm, &s, 1.0, &frame, false);
        assert!(drawn.error.as_deref().is_some_and(|e| e.starts_with("elevation source dem left out")));
        // The background, and nothing over it.
        assert!(pm.pixels().iter().all(|p| (p.red(), p.green(), p.blue(), p.alpha()) == (255, 0, 0, 255)));
    }

    #[test]
    fn a_style_that_left_something_out_draws_the_rest_and_says_so() {
        let doc = serde_json::json!({"version": 8, "sources": {}, "layers": [{"id": "bg", "type": "background", "paint": {"background-color": "#f00"}}]});
        let left_out = vec!["elevation source dem left out, so its relief isn't drawn: gone".to_owned()];
        let r = Resolved {
            style: Style::parse(&doc).unwrap(),
            sprites: Vec::new(),
            attribution: Vec::new(),
            left_out,
            made: Mutex::new(Instant::now()),
            waiting: false,
        };
        assert!(r.current());
        let s = spec(Kind::Style, "https://left-out.example.com/style.json");
        resolved().lock().unwrap().insert(s.key(), Arc::new(r));
        let mut pm = tiny_skia::Pixmap::new(4, 4).unwrap();
        let frame = crate::draw::Frame { left: 0.0, top: 400.0, m_per_px: 100.0, ppp: 1.0 };
        let drawn = crate::draw::draw(&mut pm, &s, 1.0, &frame, false);
        assert_eq!(drawn.error.as_deref(), Some("elevation source dem left out, so its relief isn't drawn: gone"));
        assert_eq!(pm.pixel(1, 1).map(|p| (p.red(), p.alpha())), Some((255, 255)), "the rest is drawn");
        // Once a failed fetch may be tried again, it is resolved again; a complete style stays.
        let Some(long_ago) = Instant::now().checked_sub(cache::RETRY * 2) else { return };
        let stale = Resolved {
            style: Style::parse(&doc).unwrap(),
            sprites: Vec::new(),
            attribution: Vec::new(),
            left_out: vec!["x".into()],
            made: Mutex::new(long_ago),
            waiting: false,
        };
        assert!(!stale.current());
        let complete = Resolved {
            style: Style::parse(&doc).unwrap(),
            sprites: Vec::new(),
            attribution: Vec::new(),
            left_out: Vec::new(),
            made: Mutex::new(long_ago),
            waiting: false,
        };
        assert!(complete.current());
        // Through `resolve`: an expired one is resolved again (a raster layer's at once)…
        let s = spec(Kind::Raster, "https://expired.example.com/{z}/{x}/{y}.png");
        let expired = Arc::new(Resolved {
            style: Style::parse(&doc).unwrap(),
            sprites: Vec::new(),
            attribution: Vec::new(),
            left_out: vec!["x".into()],
            made: Mutex::new(long_ago),
            waiting: false,
        });
        resolved().lock().unwrap().insert(s.key(), expired.clone());
        let Resolve::Ready(r) = resolve(&s) else { panic!("not resolved again") };
        assert!(!Arc::ptr_eq(&r, &expired) && r.left_out.is_empty());
        // …and drawn as it was while what it needs is on its way.
        let s = spec(Kind::Style, "https://expired.example.com/style.json");
        let stale = Arc::new(stale);
        resolved().lock().unwrap().insert(s.key(), stale.clone());
        // Pending on the first fetch: the stale style is drawn meanwhile.
        let Resolve::Ready(r) = resolve(&s) else { panic!("blank while it is read again") };
        assert!(Arc::ptr_eq(&r, &stale), "the stale style");
        // Failing to be read again: drawn as it was, and current until it is tried again.
        assert!(!stale.current());
        let Resolve::Ready(r) = settle(Resolve::Failed("gone".into()), Some(stale.clone()), &s, s.key()) else { panic!("an error") };
        assert!(Arc::ptr_eq(&r, &stale) && stale.current());
        // With nothing drawn before, pending is pending and an error an error.
        assert!(matches!(settle(Resolve::Pending, None, &s, s.key()), Resolve::Pending));
        assert!(matches!(settle(Resolve::Failed("gone".into()), None, &s, s.key()), Resolve::Failed(_)));
    }

    #[test]
    fn resolving_again_to_the_same_omission_keeps_the_style() {
        let doc = serde_json::json!({"version": 8, "sources": {}, "layers": []});
        let Some(long_ago) = Instant::now().checked_sub(cache::RETRY * 2) else { return };
        let made = |left_out: &[&str], at: Instant| {
            Arc::new(Resolved {
                style: Style::parse(&doc).unwrap(),
                sprites: Vec::new(),
                attribution: Vec::new(),
                left_out: left_out.iter().map(|s| s.to_string()).collect(),
                made: Mutex::new(at),
                waiting: false,
            })
        };
        // Left out as before: the same style, current again, so its pictures stay.
        let old = made(&["dem: gone"], long_ago);
        assert!(!old.current());
        let r = kept(old.clone(), made(&["dem: gone"], Instant::now()));
        assert!(Arc::ptr_eq(&r, &old) && r.current());
        // Something else (or nothing) left out, or another style: the new one.
        let new = made(&[], Instant::now());
        assert!(Arc::ptr_eq(&kept(old.clone(), new.clone()), &new));
        let other = Arc::new(Resolved {
            style: Style::parse(&serde_json::json!({"version": 8, "sources": {}, "layers": [{"id": "bg", "type": "background"}]})).unwrap(),
            sprites: Vec::new(),
            attribution: Vec::new(),
            left_out: vec!["dem: gone".into()],
            made: Mutex::new(Instant::now()),
            waiting: false,
        });
        assert!(Arc::ptr_eq(&kept(old.clone(), other.clone()), &other));
        // Still waiting for an elevation source, or no longer: the new one.
        let arrived = Arc::new(Resolved {
            style: Style::parse(&doc).unwrap(),
            sprites: Vec::new(),
            attribution: Vec::new(),
            left_out: vec!["dem: gone".into()],
            made: Mutex::new(Instant::now()),
            waiting: true,
        });
        assert!(Arc::ptr_eq(&kept(old.clone(), arrived.clone()), &arrived));
        // Only its credits changed: the new one too.
        let recredited = Arc::new(Resolved {
            style: Style::parse(&doc).unwrap(),
            sprites: Vec::new(),
            attribution: vec!["© Example".into()],
            left_out: vec!["dem: gone".into()],
            made: Mutex::new(Instant::now()),
            waiting: false,
        });
        assert!(Arc::ptr_eq(&kept(old, recredited.clone()), &recredited));
    }

    #[test]
    fn sprites_that_cant_be_fetched_are_said() {
        let base = "https://t.example.com/style.json";
        let icons = serde_json::json!([{"id": "i", "type": "symbol", "source": "s", "layout": {"icon-image": "x"}}]);
        let doc = serde_json::json!({"version": 8, "sources": {}, "layers": icons, "sprite": [
            {"id": "good", "url": "sprites/a"},
            {"id": "box", "url": "mapbox://sprites/x"},
            {"id": "meta", "url": "http://169.254.169.254/s"},
            {"url": "no-id"}]});
        let (sprites, said) = resolve_sprites(&Style::parse(&doc).unwrap(), base);
        assert_eq!(sprites, [("good".to_owned(), "https://t.example.com/sprites/a".to_owned())]);
        assert_eq!(said.len(), 3, "{said:?}");
        assert!(said[0].starts_with("1 of 4 sprites left out"), "{said:?}");
        assert!(said[1].starts_with("1 of 4 sprites left out") && said[1].contains("mapbox://") && said[1].ends_with(": box"), "{said:?}");
        assert!(said[2].starts_with("1 of 4 sprites left out") && said[2].ends_with(": meta"), "{said:?}");
        // One line per reason, however many: with how many, and which.
        let two = |layers: &serde_json::Value| {
            let doc = serde_json::json!({"version": 8, "sources": {}, "layers": layers, "sprite": [
                {"id": "a", "url": "mapbox://sprites/a"}, {"id": "b", "url": "mapbox://sprites/b"}]});
            resolve_sprites(&Style::parse(&doc).unwrap(), base).1
        };
        // Said only of a style that misses them: not with no layer drawing images, or one hidden.
        assert!(two(&serde_json::json!([])).is_empty());
        let hidden = serde_json::json!([{"id": "i", "type": "symbol", "source": "s", "layout": {"icon-image": "x", "visibility": "none"}}]);
        assert!(two(&hidden).is_empty());
        let said = two(&icons);
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(said[0].starts_with("2 of 2 sprites left out") && said[0].ends_with(": a, b"), "{said:?}");
        // Clean: nothing to say.
        let doc = serde_json::json!({"version": 8, "sources": {}, "layers": [], "sprite": "https://t.example.com/s"});
        assert!(resolve_sprites(&Style::parse(&doc).unwrap(), base).1.is_empty());
    }

    #[test]
    fn only_layers_drawing_images_miss_a_sprite() {
        let layer = |j: serde_json::Value| crate::style::StyleLayer::parse(&j).unwrap();
        let of = |kind: &str, what: serde_json::Value| {
            let mut l = serde_json::json!({"id": "a", "type": kind, "source": "s"});
            l.as_object_mut().unwrap().extend(what.as_object().unwrap().clone());
            layer(l)
        };
        assert!(of("symbol", serde_json::json!({"layout": {"icon-image": "x"}})).draws_images());
        assert!(of("fill", serde_json::json!({"paint": {"fill-pattern": "x"}})).draws_images());
        assert!(!of("symbol", serde_json::json!({"layout": {"text-field": "x"}})).draws_images());
        assert!(!of("fill", serde_json::json!({"paint": {"fill-color": "#000"}})).draws_images());
        // A line painted with a pattern isn't drawn, so it misses nothing.
        assert!(!of("line", serde_json::json!({"paint": {"line-pattern": "x"}})).draws_images());
        // Nor does another kind of layer naming an icon, or a pattern, that it doesn't draw.
        assert!(!of("circle", serde_json::json!({"layout": {"icon-image": "x"}})).draws_images());
        assert!(!of("circle", serde_json::json!({"paint": {"fill-pattern": "x"}})).draws_images());
    }

    #[test]
    fn plain_attribution() {
        assert_eq!(plain_text("<a href='x'>&copy; OpenStreetMap</a>  contributors &amp; CARTO"), "© OpenStreetMap contributors & CARTO");
    }
}
