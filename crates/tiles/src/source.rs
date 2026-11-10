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

use crate::cache::{self, Payload, State};
use crate::style::{LayerKind, Source, Style};
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
            "{:?}|{}:{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
            self.kind,
            self.url.len(),
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
    /// The effective style document, kept through redirects and cache hits.
    pub from: Option<address::Origin>,
    /// The style's sprites as (id, address without `.json`/`.png`), those RustGIS can
    /// fetch.
    pub sprites: Vec<(String, String)>,
    /// Credits from the layer and its sources, without duplicates.
    pub attribution: Vec<String>,
    /// What of the style was left out and why, said with every drawing of it as its error.
    pub left_out: Vec<String>,
    /// When it was resolved, used to wait before retrying a failed source.
    pub made: Mutex<Instant>,
    /// A source failed to resolve and may recover on a later attempt. Structural warnings
    /// alone never expire a style or its rendered meshes.
    pub retry: bool,
    /// An elevation source is still on its way: the rest is drawn without it meanwhile, each
    /// drawing pending, and it is resolved again on the next one.
    pub waiting: bool,
}

impl Resolved {
    /// Loads every rendering path's sprite sheets under this style's actual origin.
    pub fn load_sprites(&self, hi_dpi: bool) -> crate::sprite::Load {
        crate::sprite::load_from(&self.sprites, hi_dpi, self.from.as_ref())
    }

    /// Whether it may still be used as it is.
    fn current(&self) -> bool {
        !self.waiting && (!self.retry || self.made.lock().unwrap_or_else(|e| e.into_inner()).elapsed() < cache::RETRY)
    }
}

/// What resolving a style again gives instead of `old`: `old` itself, renewed, when it is the same
/// style leaving out the same, so what was drawn of it (pictures and meshes, kept by style) is
/// kept; else `new`.
fn kept(old: Arc<Resolved>, new: Arc<Resolved>) -> Arc<Resolved> {
    if old.left_out != new.left_out
        || old.waiting != new.waiting
        || old.retry != new.retry
        || old.style != new.style
        || old.sprites != new.sprites
        || old.attribution != new.attribution
        || old.from != new.from
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

/// Drops only styles resolved directly from this archive.
pub(crate) fn forget_archive(archive: &str) {
    let pm = format!("{:?}|{}:{archive}|", Kind::PmTiles, archive.len());
    let mb = format!("{:?}|{}:{archive}|", Kind::MbTiles, archive.len());
    resolved().lock().unwrap_or_else(|e| e.into_inner()).retain(|key, _| !key.starts_with(&pm) && !key.starts_with(&mb));
}

/// The built-in style for vector tiles without one: it knows the OpenMapTiles, Protomaps and
/// Shortbread layer names, and draws water, land use, parks, buildings, roads, boundaries and
/// place names from whichever of them the tiles have.
pub const DEFAULT_STYLE: &str = include_str!("default_style.json");

/// The style to draw `spec` with, once the documents it needs have been fetched.
pub fn resolve(spec: &Spec) -> Resolve {
    if let Err(e) = crate::logins::check(&spec.url) {
        return Resolve::Failed(e.to_string());
    }
    let key = spec.key();
    let epoch = cache::archive_epoch(&spec.url);
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
    let anew = resolve_anew(spec, key.clone(), epoch);
    cache::if_archive_current(&spec.url, epoch, || settle(anew, cached, spec, key)).unwrap_or(Resolve::Pending)
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
fn resolve_anew(spec: &Spec, key: String, epoch: u64) -> Resolve {
    let mut base = address::Origin::named(&spec.url);
    let doc = match spec.kind {
        Kind::Raster => raster_style(spec),
        Kind::Vector => match vector_style(spec) {
            Some(d) => d,
            None => return Resolve::Failed("the built-in vector style could not be read".into()),
        },
        Kind::Style => match fetch_json(&spec.url, None) {
            Ok(Some((d, reached))) => {
                base = reached;
                d
            }
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
            Err(e) => return Resolve::Failed(crate::logins::scrub(&rustgis_data::diagnostics::message_with_address(&spec.url, &e))),
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
        Err(e) => return Resolve::Failed(crate::logins::scrub(&rustgis_data::diagnostics::message_with_address(&spec.url, &e))),
    };
    if !spec.hidden_layers.is_empty() {
        style.layers.retain(|l| l.source_layer.as_ref().is_none_or(|s| !spec.hidden_layers.contains(s)));
    }
    // Sources given by a TileJSON address.
    let Sources { mut left_out, waiting, retry } = match resolve_sources(&mut style, spec, &base) {
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
    let r = Arc::new(Resolved {
        style,
        from: (spec.kind == Kind::Style).then_some(base),
        attribution,
        sprites,
        left_out,
        made: Mutex::new(Instant::now()),
        waiting,
        retry,
    });
    if let Err(e) = crate::logins::check(&spec.url) {
        return Resolve::Failed(e.to_string());
    }
    cache::if_archive_current(&spec.url, epoch, || {
        let mut all = resolved().lock().unwrap_or_else(|e| e.into_inner());
        // A few layers are in use at a time: past that many, start again rather than grow.
        if all.len() >= MAX_RESOLVED {
            all.clear();
        }
        all.insert(key, r.clone());
        Resolve::Ready(r)
    })
    .unwrap_or(Resolve::Pending)
}

/// A style's sprites with their addresses resolved against `base`, those RustGIS can fetch, and
/// what was left out of them, and why, if a layer not hidden draws images and so misses them: the
/// sprites are the style's choice, and one that can't be fetched only loses its own icons.
fn resolve_sprites(style: &Style, base: &address::Origin) -> (Vec<(String, String)>, Vec<String>) {
    let mut sprites = Vec::new();
    let mut left_out = style.sprites_left_out.clone();
    let (mut mapbox, mut refused) = (Vec::new(), Vec::new());
    for (id, s) in &style.sprites {
        let address = template::join(&base.url, s);
        if s.starts_with("mapbox://") {
            mapbox.push(id.as_str());
        } else if base.check(&address).is_err() {
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
    /// A used elevation source failed to resolve, rather than being structurally unsupported.
    retry: bool,
}

/// Resolves the sources of `style` in place: what was left out of it, or `None` while some the
/// style can't be drawn without are still on their way.
fn resolve_sources(style: &mut Style, spec: &Spec, base: &address::Origin) -> Result<Option<Sources>, String> {
    let mut pending = false;
    let mut left_out = Vec::new();
    let mut waiting = false;
    let mut retry = false;
    let skipped: Vec<&str> = style
        .layers
        .iter()
        .filter(|l| {
            let source = l.source.as_deref().and_then(|id| style.source(id));
            match l.kind {
                LayerKind::Background | LayerKind::Other => false,
                LayerKind::Raster => !matches!(source, Some(Source::Raster { .. } | Source::RasterDem { .. })),
                LayerKind::Hillshade | LayerKind::ColorRelief => !matches!(source, Some(Source::RasterDem { .. })),
                LayerKind::Fill | LayerKind::FillExtrusion | LayerKind::Line | LayerKind::Circle | LayerKind::Symbol => {
                    !matches!(source, Some(Source::Vector { .. }))
                }
            }
        })
        .map(|l| l.id.as_str())
        .collect();
    if !skipped.is_empty() {
        let names = layer_names(&skipped);
        left_out
            .push(format!("{} style layer(s) skipped because their source is missing, undefined or unsupported: {names}", skipped.len()));
    }
    let unsupported: Vec<&str> = style.layers.iter().filter(|l| l.kind == LayerKind::Other).map(|l| l.id.as_str()).collect();
    if !unsupported.is_empty() {
        left_out.push(format!(
            "{} style layer(s) skipped because their layer type is unsupported: {}",
            unsupported.len(),
            layer_names(&unsupported)
        ));
    }
    for (id, src) in &mut style.sources {
        // A missing address or a Mapbox-only source cannot recover by fetching again.
        let retryable = matches!(src, Source::RasterDem { url: Some(url), encoding, .. }
            if !url.starts_with("mapbox://") && encoding.fits());
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
                    retry |= retryable;
                }
                *src = Source::Other;
            }
            Err(e) => return Err(e),
        }
    }
    Ok((!pending).then_some(Sources { left_out, waiting, retry }))
}

/// A bounded list for diagnostics copied into every drawing, keeping the omitted count exact.
fn layer_names(ids: &[&str]) -> String {
    const MAX_NAMES: usize = 16;
    const MAX_NAME_CHARS: usize = 80;
    let mut shortened = false;
    let mut names = ids
        .iter()
        .take(MAX_NAMES)
        .map(|id| {
            let mut label: String = id.chars().take(MAX_NAME_CHARS).collect();
            if id.chars().nth(MAX_NAME_CHARS).is_some() {
                label.push('…');
                shortened = true;
            }
            label
        })
        .collect::<Vec<_>>()
        .join(", ");
    if ids.len() > MAX_NAMES {
        names.push_str(&format!(", … ({} more)", ids.len() - MAX_NAMES));
    }
    if shortened {
        names.push_str(" (names shortened)");
    }
    names
}

/// Fills in a style source's tile addresses: from its TileJSON address when it gives no tiles
/// (`Ok(true)` while that is on its way), resolved against `base`. Tile selection checks each
/// expanded address, preserving shard positions when one is refused.
fn resolve_source(src: &mut Source, spec: &Spec, base: &address::Origin) -> Result<bool, String> {
    let (tiles, url, from) = match src {
        Source::Vector { tiles, url, from, .. } | Source::Raster { tiles, url, from, .. } | Source::RasterDem { tiles, url, from, .. } => {
            (tiles, url, from)
        }
        Source::Other => return Ok(false),
    };
    // Kept only in the resolved style, never accepted from a document or saved in the project.
    *from = (spec.kind == Kind::Style).then(|| base.clone());
    if tiles.is_empty()
        && let Some(u) = url.clone()
    {
        if u.starts_with("mapbox://") {
            return Err(rustgis_data::diagnostics::message_with_address(&u, "mapbox:// addresses need Mapbox's own library"));
        }
        let u = template::join(&base.url, &u);
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
        if spec.kind == Kind::Style {
            base.check(&u)?;
        } else {
            address::check(&u, None)?;
        }
        match fetch_json(&u, (spec.kind == Kind::Style).then_some(base))? {
            Some((tj, reached)) => {
                src.apply_tilejson(&tj);
                // Tile addresses relative to the TileJSON document, which chose them.
                if let Source::Vector { tiles, from, .. } | Source::Raster { tiles, from, .. } | Source::RasterDem { tiles, from, .. } = src
                {
                    *from = Some(reached.clone());
                    for t in tiles.iter_mut() {
                        *t = template::join(&reached.url, t);
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
            *t = template::join(&base.url, t);
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
fn fetch_json(url: &str, from: Option<&address::Origin>) -> Result<Option<(Value, address::Origin)>, String> {
    match cache::document(url, from) {
        (State::Ready(p), origin) => match &*p {
            Payload::Json(v) => Ok(Some((v.clone(), origin.unwrap_or_else(|| address::Origin::named(url))))),
            _ => Err(rustgis_data::diagnostics::message_with_address(url, "not a JSON document")),
        },
        (State::Pending, _) => Ok(None),
        (State::Failed(e), _) => Err(crate::logins::scrub(&rustgis_data::diagnostics::message_with_address(url, &e))),
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
    /// An expanded selector must not let a public style choose another saved account.
    #[test]
    fn expanded_selectors_cannot_choose_a_documents_account() {
        let _turn = cache::turn();
        let endpoint = "https://expanded-account.example";
        let binding = crate::s3::new_binding().unwrap();
        let name = "expanded-account-fixture";
        crate::s3::connect_saved(
            crate::s3::Connection {
                name: name.into(),
                endpoint: Some(endpoint.into()),
                region: "us-east-1".into(),
                buckets: vec!["bucket".into()],
                credentials: None,
            },
            &binding,
            "expanded-account",
        )
        .unwrap();
        let selected = rustgis_data::s3_address::bind(&binding, endpoint, &format!("{endpoint}/bucket/tile.pbf")).unwrap();
        assert!(
            address::Origin { url: selected.clone(), reach: address::Reach::Public }.check(&selected).is_ok(),
            "the document may retain its own selected account"
        );
        let login = crate::logins::activate(
            endpoint,
            "expanded-service-fixture",
            crate::logins::Method::Header("X-Fixture".into()),
            "fixture",
            false,
        )
        .unwrap();
        let service = crate::logins::bind(&login, &format!("{endpoint}/tile.pbf")).unwrap();
        assert!(address::Origin { url: service.clone(), reach: address::Reach::Public }.check(&service).is_ok());
        let asked = Arc::new(Mutex::new(Vec::new()));
        let seen = asked.clone();
        cache::set_fetcher(move |url, done| {
            seen.lock().unwrap().push(url);
            done(Ok(crate::mvt::encode(&crate::mvt::Tile::default())));
        });
        for selected in [selected, service] {
            let mut style = Style::parse(
                &json!({"version":8,"sources":{"s":{"type":"vector","tiles":[format!("{{switch:{selected}}}")]}},"layers":[]}),
            )
            .unwrap();
            let base = address::Origin::named("https://expanded-style.example/style.json");
            let spec = spec(Kind::Style, &base.url);
            resolve_sources(&mut style, &spec, &base).unwrap();
            let src = &style.sources.first().unwrap().1;
            let frame = crate::draw::tests::world_frame();
            let mut error = None;
            crate::draw::tiles_in(src, &frame.extent(64, 64), None, &frame, 0.0, &mut false, &mut error);
            cache::wait(std::time::Duration::from_secs(2));
            assert!(error.is_some());
        }
        crate::logins::deactivate(&login);
        crate::s3::disconnect(name);
        assert!(asked.lock().unwrap().is_empty(), "the style selected an account after expansion");
    }

    #[test]
    fn forbidden_provenance_cannot_queue_sprites() {
        let _turn = cache::turn();
        let asked = Arc::new(Mutex::new(Vec::new()));
        let seen = asked.clone();
        cache::set_fetcher(move |url, done| {
            seen.lock().unwrap().push(url);
            done(Err("fixture".into()));
        });
        let from = address::Origin::named("http://metadata/style.json");
        let state = crate::sprite::load_from(&[("default".into(), "http://127.0.0.1/forbidden-sprite".into())], false, Some(&from));
        cache::wait(std::time::Duration::from_secs(2));
        assert!(asked.lock().unwrap().is_empty(), "forbidden provenance reached transport");
        assert!(matches!(state, crate::sprite::Load::Failed));
    }
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
        let _turn = crate::cache::turn();
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
        let _turn = crate::cache::turn();
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
        let _turn = crate::cache::turn();
        assert!(matches!(resolve(&spec(Kind::Raster, "http://169.254.169.254/{z}/{x}/{y}.png")), Resolve::Failed(_)));
        // A self-hosted server the user names is fine.
        assert!(matches!(resolve(&spec(Kind::Raster, "http://localhost:8080/{z}/{x}/{y}.png")), Resolve::Ready(_)));
    }

    /// Resolving then drawing exercises the actual source/template/cache chain for raster,
    /// vector and elevation tiles. The fetcher is a spy: no fixture address is contacted.
    #[test]
    fn style_tile_destinations_are_checked_after_expansion() {
        let _turn = cache::turn();
        let sent = Arc::new(Mutex::new(Vec::new()));
        let observed = sent.clone();
        cache::set_fetcher(move |url, done| {
            observed.lock().unwrap().push(url);
            done(Ok(Vec::new()));
        });
        let frame = crate::draw::tests::world_frame();
        for kind in ["raster", "vector", "raster-dem"] {
            for (case, base, host, refusal) in [
                ("forbidden", "https://destinations.example/style.json", "0.0.0.0", Some("never fetched")),
                ("local", "https://destinations.example/style.json", "127.0.0.1", Some("local network")),
                ("public", "https://destinations.example/style.json", "tiles.example", None),
                ("self-hosted", "http://localhost/style.json", "127.0.0.1", None),
            ] {
                let url = format!("http://{{switch:{host}}}/destination-regression/{kind}/{case}/{{z}}/{{x}}/{{y}}");
                let layer_kind = match kind {
                    "raster-dem" => "hillshade",
                    "vector" => "fill",
                    _ => kind,
                };
                let doc = json!({"version": 8, "sources": {
                    // A document cannot supply the runtime provenance field itself.
                    "tiles": {"type": kind, "tiles": [url], "from": "http://localhost/"}
                }, "layers": [{"id": "tiles", "type": layer_kind, "source": "tiles", "source-layer": "l"}]});
                let mut style = Style::parse(&doc).unwrap();
                let spec = spec(Kind::Style, &format!("{base}?{kind}-{case}"));
                let sources = resolve_sources(&mut style, &spec, &address::Origin::named(base)).unwrap().unwrap();
                let r = Resolved {
                    retry: false,
                    style,
                    sprites: Vec::new(),
                    from: None,
                    attribution: Vec::new(),
                    left_out: sources.left_out,
                    made: Mutex::new(Instant::now()),
                    waiting: false,
                };
                resolved().lock().unwrap().insert(spec.key(), Arc::new(r));
                let before = sent.lock().unwrap().len();
                let mut pm = tiny_skia::Pixmap::new(64, 64).unwrap();
                let drawn = crate::draw::draw(&mut pm, &spec, 1.0, &frame, true);
                cache::wait(std::time::Duration::from_secs(2));
                if let Some(why) = refusal {
                    assert_eq!(sent.lock().unwrap().len(), before, "{kind}/{case} reached the fetcher");
                    assert!(drawn.error.as_deref().is_some_and(|e| e.contains(why)), "{kind}/{case}: {:?}", drawn.error);
                } else {
                    assert!(sent.lock().unwrap().len() > before, "{kind}/{case} must still fetch");
                    assert!(drawn.error.is_none(), "{kind}/{case}: {:?}", drawn.error);
                }
                if kind == "vector" {
                    // A closer view uses prepared pictures. Its refusal must survive a cache
                    // hit, and the GPU's data-tile path must report it too.
                    let near = crate::draw::Frame { left: 0.0, top: 64000.0, m_per_px: 1000.0, ppp: 1.0 };
                    for _ in 0..2 {
                        let d = crate::draw::draw(&mut pm, &spec, 1.0, &near, false);
                        assert_eq!(d.error.is_some(), refusal.is_some(), "prepared picture: {case}");
                    }
                    let view = near.extent(64, 64);
                    let gpu = crate::shapes::vector_tiles(&spec, &near, &view, None, None).unwrap();
                    assert_eq!(gpu.error.is_some(), refusal.is_some(), "GPU data tiles: {case}");
                    let moved = crate::draw::Frame { m_per_px: 1100.0, ..near };
                    let (d, _) =
                        crate::picture::paced(crate::picture::Pace::Moving, || crate::draw::draw(&mut pm, &spec, 1.0, &moved, false));
                    assert_eq!(d.error.is_some(), refusal.is_some(), "moving stand-in: {case}");
                }
            }
        }
    }

    /// A refused shard leaves its squares empty while the same source's valid squares draw.
    /// Both a template list and a host switch are exercised without contacting a server.
    #[test]
    fn refused_tile_shards_leave_the_other_tiles_drawn() {
        let _turn = cache::turn();
        let sent = Arc::new(Mutex::new(Vec::new()));
        let observed = sent.clone();
        let mut tile = tiny_skia::Pixmap::new(1, 1).unwrap();
        tile.fill(tiny_skia::Color::from_rgba8(255, 0, 0, 255));
        let bytes = tile.encode_png().unwrap();
        cache::set_fetcher(move |url, done| {
            observed.lock().unwrap().push(url);
            done(Ok(bytes.clone()));
        });
        let public = "http://partial-tiles.example/{z}/{x}/{y}";
        let from = address::Origin::named("https://partial-style.example/style.json");
        for x in 0..2 {
            for y in 0..2 {
                cache::get_tile_from(&format!("http://partial-tiles.example/1/{x}/{y}"), cache::Want::Image, Some(&from));
            }
        }
        cache::wait(std::time::Duration::from_secs(2));
        for tiles in [
            vec!["http://127.0.0.1/{z}/{x}/{y}", public],
            vec!["pmtiles:{z}/{x}/{y}@https://archive-key.example/fixture.pmtiles", public],
            vec!["http://{switch:127.0.0.1}/{z}/{x}/{y}", public],
            vec!["http://{switch:127.0.0.1,partial-tiles.example}/{z}/{x}/{y}"],
            vec![public],
        ] {
            let clean = tiles == [public];
            let doc = json!({"version":8,"sources":{"s":{"type":"raster","tileSize":32,"maxzoom":1,"tiles":tiles}},
                "layers":[{"id":"r","type":"raster","source":"s"}]});
            let mut style = Style::parse(&doc).unwrap();
            let spec = spec(Kind::Style, &format!("{}?{}", from.url, tiles.len()));
            let sources = resolve_sources(&mut style, &spec, &from).unwrap().unwrap();
            let r = Resolved {
                retry: false,
                style,
                from: Some(from.clone()),
                sprites: Vec::new(),
                attribution: Vec::new(),
                left_out: sources.left_out,
                made: Mutex::new(Instant::now()),
                waiting: false,
            };
            let frame = crate::draw::tests::world_frame();
            let mut pm = tiny_skia::Pixmap::new(64, 64).unwrap();
            let mut drawn = crate::draw::Drawn::default();
            crate::draw::draw_layers(Some(&mut pm), (64, 64), &frame.extent(64, 64), &r, &frame, crate::draw::Pass::All, &mut drawn);
            assert_eq!(pm.pixel(48, 16).unwrap().red(), 255, "a refused shard must not hide a valid tile");
            assert_eq!(pm.pixel(16, 16).unwrap().alpha(), if clean { 255 } else { 0 });
            assert_eq!(drawn.error.is_none(), clean, "{:?}", drawn.error);
            if let Some(error) = drawn.error {
                assert!(error.contains("tiles left out"), "{error}");
                assert!(error.contains("local network") || error.contains("http://"), "{error}");
            }
            assert!(!drawn.pending);
        }
        assert!(sent.lock().unwrap().iter().all(|url| url.starts_with("http://partial-tiles.example/")));
    }

    /// Refused expanded destinations enter network history once until cleared, with clean controls.
    #[test]
    fn expanded_refusals_enter_network_history_once() {
        let _turn = cache::turn();
        cache::set_fetcher(|_, done| done(Ok(Vec::new())));
        for bad in [true, false] {
            let history = rustgis_data::diagnostics::History::default();
            let _capture = history.enter();
            let host = if bad { "{switch:127.0.0.1}" } else { "diagnostic-public.example" };
            let mut style = Style::parse(
                &json!({"version":8,"sources":{"s":{"type":"vector","tiles":[format!("http://{host}/{{z}}/{{x}}/{{y}}")]}},"layers":[]}),
            )
            .unwrap();
            let from = address::Origin::named("https://diagnostic-style.example/style.json");
            let spec = spec(Kind::Style, &from.url);
            resolve_sources(&mut style, &spec, &from).unwrap().unwrap();
            let src = &style.sources[0].1;
            let frame = crate::draw::tests::world_frame();
            let view = frame.extent(64, 64);
            for _ in 0..3 {
                let (mut pending, mut error) = (false, None);
                crate::draw::tiles_in(src, &view, None, &frame, 0.0, &mut pending, &mut error);
                assert_eq!(error.is_some(), bad);
            }
            assert_eq!(history.records().iter().filter(|r| r.network.is_some()).count(), usize::from(bad));
            history.clear();
            let (mut pending, mut error) = (false, None);
            crate::draw::tiles_in(src, &view, None, &frame, 0.0, &mut pending, &mut error);
            assert_eq!(
                history.records().iter().filter(|r| r.network.is_some()).count(),
                usize::from(bad),
                "clear allows the refusal to be reported again"
            );
        }
        cache::wait(std::time::Duration::from_secs(2));
    }

    /// Refused cache-only shards outside the view do not count as visible holes.
    #[test]
    fn cache_only_destinations_do_not_report_visible_omissions() {
        let _turn = cache::turn();
        cache::set_fetcher(|_, done| done(Ok(Vec::new())));
        let mut style = Style::parse(&json!({"version":8,"sources":{"s":{"type":"vector","tiles":[
            "http://{switch:127.0.0.1}/{z}/{x}/{y}", "https://cache-only.example/{z}/{x}/{y}"]}},"layers":[]}))
        .unwrap();
        let (_, src) = style.sources.first_mut().unwrap();
        if let Source::Vector { from, .. } = src {
            *from = Some(address::Origin::named("https://cache-only.example/style.json"));
        }
        let frame = crate::draw::tests::world_frame();
        let reach = crate::template::TileId { z: 0, x: 0, y: 0 }.extent();
        for (x, refused, padding) in [(1, false, 1.0), (1, false, 0.0), (1, true, -1.0), (0, true, 1.0)] {
            let mut view = crate::template::TileId { z: 1, x, y: 0 }.extent();
            view.min_x += padding;
            view.max_x -= padding;
            view.min_y += padding;
            view.max_y -= padding;
            let mut pending = false;
            let mut error = None;
            crate::draw::tiles_in(src, &view, Some(&reach), &frame, 1.0, &mut pending, &mut error);
            assert_eq!(error.is_some(), refused, "{error:?}");
        }
        cache::wait(std::time::Duration::from_secs(2));
    }

    /// A refused vector shard must not remove the style background over that square.
    /// Falling back to the ordinary drawing path covers both all-refused and mixed shards.
    #[test]
    fn refused_vector_destinations_keep_the_style_background() {
        let _turn = cache::turn();
        let tile = crate::mvt::encode(&crate::mvt::Tile {
            layers: vec![crate::mvt::Layer {
                name: "l".into(),
                extent: 4096,
                features: vec![crate::mvt::Feature {
                    id: None,
                    kind: crate::mvt::GeomKind::Polygon,
                    props: Vec::new(),
                    geometry: vec![vec![[0.0, 0.0], [4096.0, 0.0], [4096.0, 4096.0], [0.0, 4096.0], [0.0, 0.0]]],
                }],
            }],
        });
        cache::set_fetcher(move |url, done| {
            assert!(url.starts_with("http://background-public.example/"), "refused destinations never reach transport");
            done(Ok(tile.clone()));
        });
        let scale = crate::template::HALF_WORLD / 512.0;
        let frame = crate::draw::Frame { left: -100.0 * scale, top: 100.0 * scale, m_per_px: scale, ppp: 1.0 };
        let public = "http://background-public.example/{z}/{x}/{y}";
        let from = address::Origin::named("https://background-style.example/style.json");
        for x in 0..2 {
            for y in 0..2 {
                cache::get_tile_from(&format!("http://background-public.example/1/{x}/{y}"), cache::Want::Vector, Some(&from));
            }
        }
        cache::wait(std::time::Duration::from_secs(2));
        for (case, tiles) in [
            vec!["http://{switch:127.0.0.1}/{z}/{x}/{y}"],
            vec!["http://{switch:127.0.0.1}/{z}/{x}/{y}", public],
            vec!["http://127.0.0.1/{z}/{x}/{y}", public],
        ]
        .into_iter()
        .enumerate()
        {
            let mixed = tiles.len() > 1;
            let doc = json!({"version":8,"sources":{"s":{"type":"vector","maxzoom":1,"tiles":tiles}},"layers":[
                {"id":"bg","type":"background","paint":{"background-color":"#f00"}},
                {"id":"fill","type":"fill","source":"s","source-layer":"l","paint":{"fill-color":"#0f0"}}]});
            let mut style = Style::parse(&doc).unwrap();
            let spec = spec(Kind::Style, &format!("https://background-style.example/{case}"));
            let sources = resolve_sources(&mut style, &spec, &from).unwrap().unwrap();
            let ready = Arc::new(Resolved {
                retry: false,
                style,
                from: Some(from.clone()),
                sprites: Vec::new(),
                attribution: Vec::new(),
                left_out: sources.left_out,
                made: Mutex::new(Instant::now()),
                waiting: false,
            });
            resolved().lock().unwrap().insert(spec.key(), ready);
            let view = frame.extent(200, 200);
            assert!(crate::shapes::vector_tiles(&spec, &frame, &view, None, None).is_none(), "background needs complete fallback drawing");
            let mut image = tiny_skia::Pixmap::new(200, 200).unwrap();
            let drawn = crate::draw::draw(&mut image, &spec, 1.0, &frame, false);
            assert_eq!(image.pixel(16, 16).unwrap().red(), 255);
            if mixed {
                assert_eq!(image.pixel(150, 16).unwrap().green(), 255, "permitted vector geometry still draws");
            }
            assert!(drawn.error.is_some());
        }
    }

    /// Pending vector tiles, including a partly cached view, leave the full background
    /// visible without using a disallowed stand-in. Completed tiles can use meshes again.
    #[test]
    fn pending_vector_tiles_keep_the_style_background() {
        let _turn = cache::turn();
        let frame = crate::draw::Frame { left: -100_000.0, top: 100_000.0, m_per_px: crate::template::HALF_WORLD / 512.0, ppp: 1.0 };
        let tile = crate::mvt::encode(&crate::mvt::Tile {
            layers: vec![crate::mvt::Layer { name: "l".into(), extent: 4096, features: Vec::new() }],
        });
        for (partial, ancestor) in [(false, false), (true, false), (false, true)] {
            let host = format!("https://pending-background-{partial}-{ancestor}.example");
            let doc = json!({"version":8,"sources":{"s":{"type":"vector","tiles":[format!("{host}/{{z}}/{{x}}/{{y}}")],"maxzoom":1}},
                "layers":[{"id":"bg","type":"background","paint":{"background-color":"#f00"}},
                    {"id":"fill","type":"fill","source":"s","source-layer":"l"}]});
            let mut style = Style::parse(&doc).unwrap();
            let spec = spec(Kind::Style, &format!("{host}/style.json"));
            let from = address::Origin::named(&spec.url);
            resolve_sources(&mut style, &spec, &from).unwrap().unwrap();
            resolved().lock().unwrap().insert(
                spec.key(),
                Arc::new(Resolved {
                    retry: false,
                    style,
                    from: Some(from),
                    sprites: Vec::new(),
                    attribution: Vec::new(),
                    left_out: Vec::new(),
                    made: Mutex::new(Instant::now()),
                    waiting: false,
                }),
            );
            if partial || ancestor {
                let ready = tile.clone();
                cache::set_fetcher(move |_, done| done(Ok(ready.clone())));
                let key = if ancestor { "0/0/0" } else { "1/1/1" };
                cache::get_tile(&format!("{host}/{key}"), cache::Want::Vector);
                cache::wait(std::time::Duration::from_secs(2));
            }
            let held = Arc::new(Mutex::new(Vec::new()));
            let hold = held.clone();
            cache::set_fetcher(move |_, done| hold.lock().unwrap().push(done));
            let view = frame.extent(200, 200);
            assert_eq!(
                crate::shapes::vector_tiles(&spec, &frame, &view, None, None).is_some(),
                ancestor,
                "only a complete permitted stand-in can cover the background while pending"
            );
            let mut image = tiny_skia::Pixmap::new(200, 200).unwrap();
            let drawn = crate::draw::draw(&mut image, &spec, 1.0, &frame, false);
            assert_eq!(image.pixel(16, 16).unwrap().red(), 255);
            assert!(drawn.pending && drawn.error.is_none());
            for done in std::mem::take(&mut *held.lock().unwrap()) {
                done(Ok(tile.clone()));
            }
            cache::wait(std::time::Duration::from_secs(2));
            assert!(crate::shapes::vector_tiles(&spec, &frame, &view, None, None).is_some());
        }
    }

    /// Bounded tile selection cannot turn a large view into a partial background mesh.
    /// No pixmap or real network is needed to exercise the selection limit.
    #[test]
    fn oversized_vector_views_keep_the_style_background() {
        let _turn = cache::turn();
        let tile = crate::mvt::encode(&crate::mvt::Tile {
            layers: vec![crate::mvt::Layer { name: "l".into(), extent: 4096, features: Vec::new() }],
        });
        for (w, h) in [(20_000.0, 10_000.0), (40_000.0, 20_000.0)] {
            let host = format!("https://oversized-background-{w}.example");
            let document = json!({"version":8,"sources":{"s":{"type":"vector","tiles":[format!("{host}/{{z}}/{{x}}/{{y}}")]}},
                "layers":[{"id":"bg","type":"background"},{"id":"fill","type":"fill","source":"s","source-layer":"l"}]});
            let ready = tile.clone();
            cache::set_fetcher(move |url, done| {
                done(Ok(if url.ends_with("style.json") { document.to_string().into_bytes() } else { ready.clone() }))
            });
            let spec = spec(Kind::Style, &format!("{host}/style.json"));
            resolve(&spec);
            cache::wait(std::time::Duration::from_secs(2));
            let frame = crate::draw::Frame { left: 0.0, top: h, m_per_px: 1.0, ppp: 1.0 };
            let view = frame.extent(w as u32, h as u32);
            crate::shapes::vector_tiles(&spec, &frame, &view, None, None);
            cache::wait(std::time::Duration::from_secs(2));
            assert!(
                crate::shapes::vector_tiles(&spec, &frame, &view, None, None).is_none(),
                "bounded selection must not leave background holes in a {w} by {h} view"
            );
        }
    }

    /// A sprite notice does not remove otherwise usable vector tiles from the mesh path.
    #[test]
    fn sprite_notices_do_not_disable_vector_meshes() {
        let _turn = cache::turn();
        let document = json!({"version":8,"sprite":"unsupported://sheet",
            "sources":{"s":{"type":"vector","tiles":["https://notice-tiles.example/{z}/{x}/{y}"],"maxzoom":0}},
            "layers":[{"id":"bg","type":"background"},{"id":"fill","type":"fill","source":"s","source-layer":"l"},
                {"id":"icons","type":"symbol","source":"s","source-layer":"l","layout":{"icon-image":"pin"}}]});
        let tile = crate::mvt::encode(&crate::mvt::Tile {
            layers: vec![crate::mvt::Layer { name: "l".into(), extent: 4096, features: Vec::new() }],
        });
        cache::set_fetcher(move |url, done| {
            done(Ok(if url.ends_with("style.json") { document.to_string().into_bytes() } else { tile.clone() }))
        });
        let spec = spec(Kind::Style, "https://notice-style.example/style.json");
        resolve(&spec);
        cache::wait(std::time::Duration::from_secs(2));
        let Resolve::Ready(ready) = resolve(&spec) else { panic!("the style must be ready") };
        assert!(!ready.left_out.is_empty());
        let frame = crate::draw::tests::world_frame();
        crate::shapes::vector_tiles(&spec, &frame, &frame.extent(64, 64), None, None);
        cache::wait(std::time::Duration::from_secs(2));
        let tiles = crate::shapes::vector_tiles(&spec, &frame, &frame.extent(64, 64), None, None)
            .expect("a sprite notice must not force otherwise usable geometry through CPU drawing");
        assert!(tiles.error.is_some(), "the sprite notice is retained");
    }

    /// Resolving a child beyond the saved-address limit must report that child as unusable,
    /// rather than asking the cache for the supplying document as though it were a sprite.
    #[test]
    fn oversized_saved_s3_children_do_not_reuse_the_document() {
        let href = "x".repeat(rustgis_data::s3_address::MAX_ADDRESS);
        let style = Style::parse(&json!({"version":8,"sources":{},"sprite":href,
            "layers":[{"id":"icons","type":"symbol","layout":{"icon-image":"pin"}}]}))
        .unwrap();
        let url =
            rustgis_data::s3_address::bind("0123456789abcdef", "https://s3-join.example", "https://s3-join.example/bucket/style.json")
                .unwrap();
        let base = address::Origin { url, reach: address::Reach::Public };
        let (sprites, notes) = resolve_sprites(&style, &base);
        assert!(sprites.is_empty(), "the document is not its own sprite: {sprites:?}");
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("won't fetch"), "{notes:?}");
    }

    /// The generated raster style names the user's selected address as both base and tile;
    /// resolving it must retain one wrapper rather than nesting the same selection.
    #[test]
    fn direct_saved_s3_rasters_keep_one_binding() {
        let _turn = cache::turn();
        let selected = rustgis_data::s3_address::bind(
            "0123456789abcdef",
            "https://s3-direct.example",
            "https://s3-direct.example/bucket/{z}/{x}/{y}.png",
        )
        .unwrap();
        let spec = spec(Kind::Raster, &selected);
        let Resolve::Ready(resolved) = resolve(&spec) else { panic!("a direct raster needs no document fetch") };
        let source = &resolved.style.sources.first().unwrap().1;
        let Source::Raster { tiles, .. } = source else { panic!("the generated raster source is retained") };
        assert_eq!(tiles, &[selected]);
    }

    /// A direct TileJSON selection reaches the cache once with its chosen account, and
    /// relative tile addresses inherit that same account from the returned document.
    #[test]
    fn direct_saved_s3_tilejson_keeps_its_binding() {
        let _turn = cache::turn();
        let endpoint = "https://s3-direct-vector.example";
        let binding = crate::s3::new_binding().unwrap();
        let name = "direct TileJSON fixture";
        crate::s3::connect_saved(
            crate::s3::Connection {
                name: name.into(),
                endpoint: Some(endpoint.into()),
                region: "us-east-1".into(),
                buckets: vec!["bucket".into()],
                credentials: None,
            },
            &binding,
            "direct-vector",
        )
        .unwrap();
        let selected = rustgis_data::s3_address::bind(&binding, endpoint, &format!("{endpoint}/bucket/tiles.json")).unwrap();
        let asked = selected.clone();
        cache::set_fetcher_with_origin(move |url, _, done, _| {
            assert_eq!(url, asked, "the TileJSON request keeps exactly one selected wrapper");
            let plain = crate::s3::bound_destination(&url).unwrap().unwrap();
            let origin = address::Origin::named(&plain).retain_binding(&url).unwrap();
            done(Ok(cache::Fetched { bytes: br#"{"tiles":["{z}/{x}/{y}.pbf"]}"#.to_vec(), origin }));
        });
        let spec = spec(Kind::Vector, &selected);
        assert!(matches!(resolve(&spec), Resolve::Pending));
        cache::wait(std::time::Duration::from_secs(2));
        let Resolve::Ready(resolved) = resolve(&spec) else { panic!("the selected TileJSON must load") };
        let Source::Vector { tiles, .. } = &resolved.style.sources.first().unwrap().1 else { panic!("the vector source is retained") };
        let child = rustgis_data::s3_address::parse(&tiles[0]).unwrap().unwrap();
        assert_eq!((child.binding, child.url), (binding.as_str(), &*format!("{endpoint}/bucket/{{z}}/{{x}}/{{y}}.pbf")));
        crate::s3::disconnect(name);
    }

    /// Label-only sources are checked too, even when the GPU or prepared pictures draw
    /// a different source's shapes. Neither renderer may discard a label refusal.
    #[test]
    fn label_only_destinations_report_refusals_in_both_renderers() {
        let _turn = cache::turn();
        cache::set_fetcher(|_, done| done(Ok(Vec::new())));
        let frame = crate::draw::Frame { left: 0.0, top: 64000.0, m_per_px: 1000.0, ppp: 1.0 };
        let view = frame.extent(64, 64);
        for (host, refused) in [("0.0.0.0", true), ("labels.example", false)] {
            let base = format!("https://label-destinations.example/{host}/style.json");
            let doc = json!({"version":8,"sources":{
                "shapes":{"type":"vector","tiles":["https://shapes.example/{z}/{x}/{y}"]},
                "labels":{"type":"vector","tiles":[format!("http://{{switch:{host}}}/{{z}}/{{x}}/{{y}}")]}},
                "layers":[{"id":"shapes","type":"fill","source":"shapes","source-layer":"l"},
                    {"id":"labels","type":"symbol","source":"labels","source-layer":"l","layout":{"text-field":"name"}}]});
            let mut style = Style::parse(&doc).unwrap();
            let spec = spec(Kind::Style, &base);
            resolve_sources(&mut style, &spec, &address::Origin::named(&base)).unwrap().unwrap();
            resolved().lock().unwrap().insert(
                spec.key(),
                Arc::new(Resolved {
                    retry: false,
                    style,
                    from: None,
                    sprites: Vec::new(),
                    attribution: Vec::new(),
                    left_out: Vec::new(),
                    made: Mutex::new(Instant::now()),
                    waiting: false,
                }),
            );
            let gpu = crate::shapes::vector_tiles(&spec, &frame, &view, None, Some((64, 64))).unwrap();
            assert_eq!(gpu.error.is_some(), refused, "label-only GPU: {host}");
            let placed = crate::draw::place(&spec, 1.0, &frame, &view, &view, Some((64, 64))).unwrap();
            assert_eq!(placed.error.is_some(), refused, "label-only placement: {host}");
        }
        cache::wait(std::time::Duration::from_secs(2));
    }

    /// A local style can use a public TileJSON, but that document cannot choose local tiles.
    /// The metadata is read through the actual cache; only the resulting tile send is refused.
    #[test]
    fn tilejson_destinations_keep_the_supplying_documents_reach() {
        let _turn = cache::turn();
        let sent = Arc::new(Mutex::new(Vec::new()));
        let observed = sent.clone();
        let metadata = "https://tilejson-destinations.example/tiles.json";
        cache::set_fetcher(move |url, done| {
            observed.lock().unwrap().push(url.clone());
            done(if url == metadata {
                Ok(json!({"tiles": ["http://{switch:127.0.0.1}/tilejson-destinations/{z}/{x}/{y}"]}).to_string().into_bytes())
            } else {
                Ok(Vec::new())
            });
        });
        let base = "http://localhost/tilejson-destinations/style.json";
        let s = spec(Kind::Style, base);
        let doc = json!({"version": 8, "sources": {"tiles": {"type": "raster", "url": metadata}},
            "layers": [{"id": "tiles", "type": "raster", "source": "tiles"}]});
        let mut ready = None;
        for _ in 0..3 {
            let mut style = Style::parse(&doc).unwrap();
            if let Some(sources) = resolve_sources(&mut style, &s, &address::Origin::named(base)).unwrap() {
                ready = Some(Resolved {
                    retry: false,
                    style,
                    sprites: Vec::new(),
                    from: None,
                    attribution: Vec::new(),
                    left_out: sources.left_out,
                    made: Mutex::new(Instant::now()),
                    waiting: false,
                });
                break;
            }
            cache::wait(std::time::Duration::from_secs(2));
        }
        resolved().lock().unwrap().insert(s.key(), Arc::new(ready.expect("metadata must resolve")));
        let mut pm = tiny_skia::Pixmap::new(64, 64).unwrap();
        let drawn = crate::draw::draw(&mut pm, &s, 1.0, &crate::draw::tests::world_frame(), false);
        cache::wait(std::time::Duration::from_secs(2));
        assert!(drawn.error.as_deref().is_some_and(|e| e.contains("local network")), "{:?}", drawn.error);
        assert_eq!(*sent.lock().unwrap(), [metadata], "only the metadata, not its chosen local tile, was fetched");
    }

    #[test]
    fn layers_without_a_defined_source_are_counted_and_named() {
        let base = "https://t.example.com/style.json";
        let notes = |layers: serde_json::Value| {
            let mut style = Style::parse(&serde_json::json!({"version": 8,
                "sources": {"s": {"type": "vector", "tiles": ["https://t.example.com/{z}/{x}/{y}.pbf"]},
                    "unsupported": {"type": "geojson", "data": {"type": "FeatureCollection", "features": []}}},
                "layers": layers}))
            .unwrap();
            resolve_sources(&mut style, &spec(Kind::Style, base), &address::Origin::named(base)).unwrap().unwrap().left_out
        };
        assert!(
            notes(serde_json::json!([
                {"id": "bg", "type": "background"},
                {"id": "roads", "type": "line", "source": "s"}
            ]))
            .is_empty()
        );
        for kind in ["fill", "line", "circle", "symbol", "raster", "hillshade", "color-relief", "fill-extrusion"] {
            let said = notes(serde_json::json!([
                {"id": "absent", "type": kind},
                {"id": "unknown", "type": kind, "source": "missing"},
                {"id": "unsupported", "type": kind, "source": "unsupported"},
                {"id": "valid", "type": "line", "source": "s"},
                {"id": "bg", "type": "background"}
            ]));
            assert_eq!(
                said,
                ["3 style layer(s) skipped because their source is missing, undefined or unsupported: absent, unknown, unsupported"],
                "{kind}"
            );
        }
        let long = "名".repeat(2000);
        let layers: Vec<_> = (0..20).map(|i| json!({"id": format!("{i}-{long}"), "type": "line"})).collect();
        let said = notes(json!(layers)).join("; ");
        assert!(said.starts_with("20 style layer(s) skipped"), "{said}");
        assert!(said.contains("4 more") && said.contains("names shortened"), "{said}");
        assert!(said.len() < 6000, "diagnostic must not copy every full layer name: {} bytes", said.len());
        let said = notes(json!([
            {"id": "sky", "type": "sky"},
            {"id": "custom", "type": "custom"},
            {"id": "heat", "type": "heatmap", "source": "s"}
        ]));
        assert_eq!(said, ["3 style layer(s) skipped because their layer type is unsupported: sky, custom, heat"]);
    }

    #[test]
    fn layers_with_incompatible_source_kinds_are_counted() {
        for kind in ["fill", "line", "circle", "symbol", "fill-extrusion", "hillshade", "color-relief", "raster"] {
            for source in ["v", "r", "d"] {
                let doc = json!({"version":8,"sources":{
                    "v":{"type":"vector","tiles":["https://types.example/{z}/{x}/{y}.pbf"]},
                    "r":{"type":"raster","tiles":["https://types.example/{z}/{x}/{y}.png"]},
                    "d":{"type":"raster-dem","tiles":["https://types.example/{z}/{x}/{y}.png"]}},
                    "layers":[{"id":"test","type":kind,"source":source,
                        "paint":{"color-relief-color":["interpolate",["linear"],["elevation"],0,"#00f",10,"#f00"]}}]});
                let mut style = Style::parse(&doc).unwrap();
                let base = "https://types.example/style.json";
                let notes = resolve_sources(&mut style, &spec(Kind::Style, base), &address::Origin::named(base)).unwrap().unwrap().left_out;
                let compatible = match kind {
                    "raster" => source != "v", // Both raster sources provide drawable images.
                    "hillshade" | "color-relief" => source == "d",
                    _ => source == "v",
                };
                if compatible {
                    assert!(notes.is_empty(), "{kind}/{source}: {notes:?}");
                } else {
                    assert_eq!(notes.len(), 1, "{kind}/{source}: {notes:?}");
                    assert!(notes[0].starts_with("1 style layer(s) skipped") && notes[0].ends_with(": test"), "{notes:?}");
                }
            }
        }
    }

    #[test]
    fn an_unreadable_elevation_source_is_left_out_and_said() {
        let doc = serde_json::json!({"version": 8, "sources": {
            "dem": {"type": "raster-dem", "url": "http://169.254.169.254/dem.json"},
            "sat": {"type": "raster", "tiles": ["https://t.example.com/{z}/{x}/{y}.png"]}},
            "layers": [{"id": "shade", "type": "hillshade", "source": "dem"}, {"id": "sat", "type": "raster", "source": "sat"}]});
        let mut style = Style::parse(&doc).unwrap();
        let base = "https://t.example.com/style.json";
        let sources = resolve_sources(&mut style, &spec(Kind::Style, base), &address::Origin::named(base)).unwrap().unwrap();
        assert!(sources.retry, "a failed elevation source is retried");
        let left_out = sources.left_out;
        assert_eq!(left_out.len(), 1);
        assert!(left_out[0].starts_with("elevation source dem left out"), "{left_out:?}");
        assert!(matches!(style.sources.iter().find(|s| s.0 == "dem").map(|s| &s.1), Some(Source::Other)));
        // A source the style can't draw without still fails it.
        let doc = serde_json::json!({"version": 8, "sources": {"sat": {"type": "raster", "url": "http://169.254.169.254/sat.json"}}, "layers": []});
        let mut style = Style::parse(&doc).unwrap();
        assert!(resolve_sources(&mut style, &spec(Kind::Style, base), &address::Origin::named(base)).is_err());
        // Clean sources leave nothing out.
        let doc = serde_json::json!({"version": 8, "sources": {"sat": {"type": "raster", "tiles": ["https://t.example.com/{z}/{x}/{y}.png"]}}, "layers": []});
        let mut style = Style::parse(&doc).unwrap();
        assert_eq!(resolve_sources(&mut style, &spec(Kind::Style, base), &address::Origin::named(base)), Ok(Some(Sources::default())));
        // Only a source a layer draws from is missed; the others are dropped without a word.
        let left_out_of = |doc: serde_json::Value| {
            let mut style = Style::parse(&doc).unwrap();
            resolve_sources(&mut style, &spec(Kind::Style, base), &address::Origin::named(base)).unwrap().unwrap().left_out
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
    fn permanent_dem_failures_do_not_retry() {
        let base = "https://permanent-dem.example.com/style.json";
        for dem in [json!({"type":"raster-dem"}), json!({"type":"raster-dem","url":"mapbox://dem/a"})] {
            let doc = json!({"version":8,"sources":{"dem":dem},"layers":[{"id":"shade","type":"hillshade","source":"dem"}]});
            let mut style = Style::parse(&doc).unwrap();
            let sources = resolve_sources(&mut style, &spec(Kind::Style, base), &address::Origin::named(base)).unwrap().unwrap();
            assert_eq!(sources.left_out.len(), 1);
            assert!(!sources.retry && !sources.waiting, "{sources:?}");
        }
    }

    #[test]
    fn permanent_style_notices_do_not_expire_resolved_styles() {
        let _turn = crate::cache::turn();
        let doc = json!({"version":8, "sources":{}, "sprite":"mapbox://sprites/a", "layers":[
            {"id":"missing", "type":"symbol", "layout":{"icon-image":"x"}},
            {"id":"sky", "type":"sky"}
        ]});
        let mut style = Style::parse(&doc).unwrap();
        let s = spec(Kind::Style, "https://permanent-notices.example.com/style.json");
        let base = address::Origin::named(&s.url);
        let sources = resolve_sources(&mut style, &s, &base).unwrap().unwrap();
        assert!(!sources.retry && !sources.waiting);
        let (sprites, sprite_notes) = resolve_sprites(&style, &base);
        assert_eq!(sources.left_out.len(), 2);
        assert_eq!(sprite_notes.len(), 1);
        let Some(long_ago) = Instant::now().checked_sub(cache::RETRY * 2) else { return };
        let r = Arc::new(Resolved {
            style,
            from: Some(base),
            sprites,
            attribution: Vec::new(),
            left_out: sources.left_out.into_iter().chain(sprite_notes).collect(),
            made: Mutex::new(long_ago),
            waiting: sources.waiting,
            retry: sources.retry,
        });
        assert!(r.current(), "permanent diagnostics never schedule a refetch");
        resolved().lock().unwrap().insert(s.key(), r.clone());
        let Resolve::Ready(again) = resolve(&s) else { panic!("a cached style became unavailable") };
        assert!(Arc::ptr_eq(&r, &again));
        assert_eq!(*r.made.lock().unwrap(), long_ago, "kept without resolving or renewing");
    }

    #[test]
    fn a_readable_elevation_source_is_kept() {
        let doc = serde_json::json!({"version": 8, "sources": {"dem": {"type": "raster-dem", "tiles": ["https://t.example.com/{z}/{x}/{y}.png"]}},
            "layers": [{"id": "shade", "type": "hillshade", "source": "dem"}]});
        let mut style = Style::parse(&doc).unwrap();
        let base = "https://t.example.com/style.json";
        assert_eq!(resolve_sources(&mut style, &spec(Kind::Style, base), &address::Origin::named(base)), Ok(Some(Sources::default())));
        assert!(
            matches!(style.source("dem"), Some(Source::RasterDem { tiles, .. }) if tiles == &["https://t.example.com/{z}/{x}/{y}.png"])
        );
    }

    #[test]
    fn an_elevation_source_on_its_way_holds_back_only_its_own_layers() {
        let _turn = crate::cache::turn();
        // Its TileJSON isn't here yet (the first fetch of an address is on its way).
        let doc = serde_json::json!({"version": 8, "sources": {
            "dem": {"type": "raster-dem", "url": "https://on-its-way.example.com/dem.json"},
            "sat": {"type": "raster", "tiles": ["https://t.example.com/{z}/{x}/{y}.png"]}},
            "layers": [{"id": "shade", "type": "hillshade", "source": "dem"}, {"id": "sat", "type": "raster", "source": "sat"}]});
        let mut style = Style::parse(&doc).unwrap();
        let base = "https://t.example.com/style.json";
        let sources = resolve_sources(&mut style, &spec(Kind::Style, base), &address::Origin::named(base))
            .unwrap()
            .expect("drawn without it meanwhile");
        assert_eq!(sources, Sources { left_out: Vec::new(), waiting: true, retry: false }, "nothing lost, only waited for");
        assert!(matches!(style.source("dem"), Some(Source::Other)));
        assert!(matches!(style.source("sat"), Some(Source::Raster { .. })));
        // One no layer draws from isn't waited for.
        let unused = serde_json::json!({"version": 8, "sources": {"dem": {"type": "raster-dem", "url": "https://unused-on-its-way.example.com/dem.json"}}, "layers": []});
        let mut style = Style::parse(&unused).unwrap();
        assert_eq!(resolve_sources(&mut style, &spec(Kind::Style, base), &address::Origin::named(base)), Ok(Some(Sources::default())));
        assert!(matches!(style.source("dem"), Some(Source::Other)));
        // Drawn meanwhile, pending (so it is drawn again, and an export waits), without a notice;
        // resolved again on the next drawing.
        let doc = serde_json::json!({"version": 8, "sources": {}, "layers": [{"id": "bg", "type": "background", "paint": {"background-color": "#f00"}}]});
        let r = Resolved {
            style: Style::parse(&doc).unwrap(),
            sprites: Vec::new(),
            from: None,
            attribution: Vec::new(),
            left_out: Vec::new(),
            made: Mutex::new(Instant::now()),
            waiting: true,
            retry: false,
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
        let _turn = crate::cache::turn();
        // Lit from every side with white, a hillshade would show on any slope drawn.
        let doc = serde_json::json!({"version": 8, "sources": {"dem": {"type": "raster-dem", "url": "http://169.254.169.254/dem.json"}},
            "layers": [
                {"id": "bg", "type": "background", "paint": {"background-color": "#f00"}},
                {"id": "shade", "type": "hillshade", "source": "dem", "paint": {"hillshade-highlight-color": "#fff", "hillshade-shadow-color": "#fff"}},
                {"id": "relief", "type": "color-relief", "source": "dem",
                    "paint": {"color-relief-color": ["interpolate", ["linear"], ["elevation"], 0, "#00f", 10, "#00f"]}}]});
        let mut style = Style::parse(&doc).unwrap();
        let base = "https://t.example.com/style.json";
        let sources = resolve_sources(&mut style, &spec(Kind::Style, base), &address::Origin::named(base)).unwrap().unwrap();
        assert_eq!(sources.left_out.len(), 1);
        assert!(matches!(style.source("dem"), Some(Source::Other)));
        let r = Resolved {
            style,
            sprites: Vec::new(),
            from: None,
            attribution: Vec::new(),
            left_out: sources.left_out,
            made: Mutex::new(Instant::now()),
            waiting: false,
            retry: sources.retry,
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
    fn a_relief_layer_rustgis_cant_colour_is_said_with_each_drawing() {
        let _turn = crate::cache::turn();
        let drawn_error = |colour: serde_json::Value, host: &str| {
            let doc = serde_json::json!({"version": 8, "sources": {"dem": {"type": "raster-dem", "tiles": ["https://t.example.com/{z}/{x}/{y}.png"]}},
                "layers": [{"id": "relief", "type": "color-relief", "source": "dem", "paint": {"color-relief-color": colour}}]});
            let r = Resolved {
                style: Style::parse(&doc).unwrap(),
                from: None,
                sprites: Vec::new(),
                attribution: Vec::new(),
                left_out: Vec::new(),
                made: Mutex::new(Instant::now()),
                waiting: false,
                retry: false,
            };
            let s = spec(Kind::Style, &format!("https://{host}.example.com/style.json"));
            resolved().lock().unwrap().insert(s.key(), Arc::new(r));
            let mut pm = tiny_skia::Pixmap::new(8, 8).unwrap();
            let frame = crate::draw::Frame { left: 0.0, top: 800.0, m_per_px: 100.0, ppp: 1.0 };
            crate::draw::draw(&mut pm, &s, 1.0, &frame, false).error
        };
        let on_zoom = serde_json::json!(["interpolate", ["linear"], ["zoom"], 0, "#00f", 10, "#fff"]);
        assert!(
            drawn_error(on_zoom, "relief-on-zoom").is_some_and(|e| e.contains("color-relief layer(s) skipped") && e.ends_with(": relief"))
        );
        let ramp = serde_json::json!(["interpolate", ["linear"], ["elevation"], 0, "#00f", 1000, "#fff"]);
        assert_eq!(drawn_error(ramp, "relief-on-height"), None);
    }

    #[test]
    fn a_style_that_left_something_out_draws_the_rest_and_says_so() {
        let _turn = crate::cache::turn();
        let doc = serde_json::json!({"version": 8, "sources": {}, "layers": [{"id": "bg", "type": "background", "paint": {"background-color": "#f00"}}]});
        let left_out = vec!["elevation source dem left out, so its relief isn't drawn: gone".to_owned()];
        let r = Resolved {
            style: Style::parse(&doc).unwrap(),
            sprites: Vec::new(),
            from: None,
            attribution: Vec::new(),
            left_out,
            made: Mutex::new(Instant::now()),
            waiting: false,
            retry: true,
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
            from: None,
            attribution: Vec::new(),
            left_out: vec!["x".into()],
            made: Mutex::new(long_ago),
            waiting: false,
            retry: true,
        };
        assert!(!stale.current());
        let complete = Resolved {
            style: Style::parse(&doc).unwrap(),
            sprites: Vec::new(),
            from: None,
            attribution: Vec::new(),
            left_out: Vec::new(),
            made: Mutex::new(long_ago),
            waiting: false,
            retry: false,
        };
        assert!(complete.current());
        // Through `resolve`: an expired one is resolved again (a raster layer's at once)…
        let s = spec(Kind::Raster, "https://expired.example.com/{z}/{x}/{y}.png");
        let expired = Arc::new(Resolved {
            style: Style::parse(&doc).unwrap(),
            sprites: Vec::new(),
            from: None,
            attribution: Vec::new(),
            left_out: vec!["x".into()],
            made: Mutex::new(long_ago),
            waiting: false,
            retry: true,
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
                from: None,
                attribution: Vec::new(),
                left_out: left_out.iter().map(|s| s.to_string()).collect(),
                made: Mutex::new(at),
                waiting: false,
                retry: !left_out.is_empty(),
            })
        };
        // Left out as before: the same style, current again, so its pictures stay.
        let old = made(&["dem: gone"], long_ago);
        assert!(!old.current());
        let r = kept(old.clone(), made(&["dem: gone"], Instant::now()));
        assert!(Arc::ptr_eq(&r, &old) && r.current());
        // Equal content from another origin must not retain the old root's authority.
        let mut moved = made(&["dem: gone"], Instant::now());
        Arc::get_mut(&mut moved).unwrap().from = Some(address::Origin::named("https://refreshed-style.example/style.json"));
        assert!(Arc::ptr_eq(&kept(old.clone(), moved.clone()), &moved));
        let mut local = made(&["dem: gone"], Instant::now());
        Arc::get_mut(&mut local).unwrap().from =
            Some(address::Origin { url: "https://refreshed-style.example/style.json".into(), reach: address::Reach::Local });
        assert!(Arc::ptr_eq(&kept(local, moved.clone()), &moved), "a public refresh of the same URL drops local authority");
        // Something else (or nothing) left out, or another style: the new one.
        let new = made(&[], Instant::now());
        assert!(Arc::ptr_eq(&kept(old.clone(), new.clone()), &new));
        let other = Arc::new(Resolved {
            style: Style::parse(&serde_json::json!({"version": 8, "sources": {}, "layers": [{"id": "bg", "type": "background"}]})).unwrap(),
            sprites: Vec::new(),
            from: None,
            attribution: Vec::new(),
            left_out: vec!["dem: gone".into()],
            made: Mutex::new(Instant::now()),
            waiting: false,
            retry: true,
        });
        assert!(Arc::ptr_eq(&kept(old.clone(), other.clone()), &other));
        // Still waiting for an elevation source, or no longer: the new one.
        let arrived = Arc::new(Resolved {
            style: Style::parse(&doc).unwrap(),
            sprites: Vec::new(),
            from: None,
            attribution: Vec::new(),
            left_out: vec!["dem: gone".into()],
            made: Mutex::new(Instant::now()),
            waiting: true,
            retry: true,
        });
        assert!(Arc::ptr_eq(&kept(old.clone(), arrived.clone()), &arrived));
        // Only its credits changed: the new one too.
        let recredited = Arc::new(Resolved {
            style: Style::parse(&doc).unwrap(),
            sprites: Vec::new(),
            from: None,
            attribution: vec!["© Example".into()],
            left_out: vec!["dem: gone".into()],
            made: Mutex::new(Instant::now()),
            waiting: false,
            retry: true,
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
        let (sprites, said) = resolve_sprites(&Style::parse(&doc).unwrap(), &address::Origin::named(base));
        assert_eq!(sprites, [("good".to_owned(), "https://t.example.com/sprites/a".to_owned())]);
        assert_eq!(said.len(), 3, "{said:?}");
        assert!(said[0].starts_with("1 of 4 sprites left out"), "{said:?}");
        assert!(said[1].starts_with("1 of 4 sprites left out") && said[1].contains("mapbox://") && said[1].ends_with(": box"), "{said:?}");
        assert!(said[2].starts_with("1 of 4 sprites left out") && said[2].ends_with(": meta"), "{said:?}");
        // One line per reason, however many: with how many, and which.
        let two = |layers: &serde_json::Value| {
            let doc = serde_json::json!({"version": 8, "sources": {}, "layers": layers, "sprite": [
                {"id": "a", "url": "mapbox://sprites/a"}, {"id": "b", "url": "mapbox://sprites/b"}]});
            resolve_sprites(&Style::parse(&doc).unwrap(), &address::Origin::named(base)).1
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
        assert!(resolve_sprites(&Style::parse(&doc).unwrap(), &address::Origin::named(base)).1.is_empty());
    }

    /// Every sprite of a style fetched with a login (not only the first): one at the style's own
    /// origin inherits it, one at another origin goes without, and a revoked login reads none.
    #[test]
    fn every_sprite_at_the_styles_origin_inherits_its_login() {
        let _turn = crate::cache::turn();
        use crate::logins::{self, Method};
        let origin = "https://sprites-login.example.com";
        let binding = logins::activate(origin, "sprites", Method::Header("X-Key".into()), "sprites-secret", false).unwrap();
        let base = logins::bind(&binding, &format!("{origin}/style.json")).unwrap();
        let icons = serde_json::json!([{"id": "i", "type": "symbol", "source": "s", "layout": {"icon-image": "x"}}]);
        let doc = serde_json::json!({"version": 8, "sources": {}, "layers": icons, "sprite": [
            {"id": "near", "url": "sprites/a"},
            {"id": "also", "url": format!("{origin}/other/b")},
            {"id": "far", "url": "https://elsewhere.example.com/c"}]});
        let (sprites, said) = resolve_sprites(&Style::parse(&doc).unwrap(), &address::Origin::named(&base));
        assert_eq!(sprites.len(), 3, "{said:?}");
        let plain = [format!("{origin}/sprites/a"), format!("{origin}/other/b"), "https://elsewhere.example.com/c".to_string()];
        for ((id, address), plain) in sprites.iter().zip(&plain) {
            assert_eq!(logins::inner(address), plain);
            let applied = logins::apply(address).unwrap();
            assert_eq!(applied.is_some(), id != "far", "{id}");
            assert!(!said.iter().any(|line| line.contains("sprites-secret") || line.contains(&binding)), "{said:?}");
        }
        assert!(logins::deactivate(&binding));
        assert!(matches!(crate::sprite::load(sprites.get(..2).unwrap_or_default(), false), crate::sprite::Load::Failed));
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
