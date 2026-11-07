//! Home › Add › Basemap: web tile layers (XYZ/TMS/WMS raster tiles, vector tiles and
//! MapLibre styles) over the basemap.

use rustgis_map::effects::Blend;
use rustgis_map::planets;
use rustgis_map::tiles::{MAX_TILE_LAYERS, TileKind, TileLayer};
use rustgis_render::rustgis_tiles::cog::{self, Look};
use serde_json::{Value, json};

use super::{b, f, ps};
use crate::{CmdResult, CommandSpec as C, Session};

/// Commands that make the map fetch from an address they're given (a plugin needs the
/// `network` permission to run them).
pub const NETWORK: &[&str] = &[
    "map.route",
    "map.addTiles",
    "map.body",
    "map.terrain",
    "layer.addUrl",
    "service.layers",
    "service.add",
    "identify.featureInfo",
    "service.import",
    "service.refresh",
    "service.groupPlan",
    "service.coverage",
    "catalog.search",
    "stac.collections",
    "stac.search",
    "stac.addAsset",
    "stac.addCube",
    // From the catalog entry's address (a Zarr entry also as `layer.addZarr`: plugins.rs).
    "cube.open",
    "map.findPlace",
    "layer.geocode",
    "osm.download",
    "boundaries.add",
    "boundaries.countries",
    "s3.list",
    "overture.add",
    "sourcecoop.products",
    "sourcecoop.list",
    "portal.search",
    "portal.add",
    "nationalMap.add",
    "fieldsOfTheWorld.add",
    "portal.files",
    "embeddings.search",
    "embeddings.add",
    "earthdata.collections",
    "earthdata.granules",
    "nldi.flowline",
    "nldi.comid",
    "nldi.basin",
    "nldi.sources",
    "nldi.navigate",
    // A sign-in reaches the portal, and a sign-out revokes its token there; a token is sent to
    // the server it is given for.
    "arcgis.signIn",
    "arcgis.signOut",
    "arcgis.token",
];

/// Registers commands for tile-layer sources, drawing properties, coverage and celestial bodies.
pub fn commands() -> Vec<C> {
    vec![
        C::new("map.addTiles", "Add Tile Layer", "Home › Add", add).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network)).params(
            r#"{"url": "https://…/{z}/{x}/{y}.png" | vector tiles ".pbf" or TileJSON | MapLibre style.json | a PMTiles archive (web address or file path) | an MBTiles file path, "kind"?: "raster"|"vector"|"style"|"pmtiles"|"mbtiles" (guessed from the url), "name"?: text, "bounds"?: [west, south, east, north] (longitude/latitude coverage, when known), "attribution"?: text, "minZoom"?: number, "maxZoom"?: number, "tileSize"?: 256|512 (1024 too for a WMS or map service asked for in Web Mercator by its tiles' box: their pictures are asked for that size), "singleImage"?: bool (a WMS or map service: one picture of the view instead of tiles, so symbols and labels aren't cut at tile edges; tiles on the globe, a tilted map or a projection the service doesn't offer; by default on for one, unless tileSize is given), "tms"?: bool, "subdomains"?: [text], "layers"?: [vector tile layer names, drawn in colours of their own], "styleLayers"?: [MapLibre style layers drawing vector tiles instead (their source is the tiles')], "opacity"?: 0–1, "blend"?: "normal"|"multiply"|"screen"|"overlay"|"darken"|"lighten"|"difference" (how it combines with what is beneath), "overlay"?: bool (on top of the map's layers, as every new layer goes; without it a basemap, under them; one with the same address and role is replaced when it has the same name or the same look), "top"?: bool (on top of the map's layers, as a layer added as data goes; by default an overlay's place), "replace"?: bool (removes the other tile basemaps, not overlays), "id"?: catalog id, "view"?: bool (a MapLibre style: go to the place, zoom, bearing and pitch its root properties give, once it loads), "start"?, "end"?: dates a url with date placeholders ({date:YYYY-MM-DD}, {YYYY}) spans, turning the time slider on (or, when it is on, setting its span), "unit"?, "interval"?, "dates"?: its steps (as in time.set), "legend"?: "the web address of its legend picture (PNG or JPEG)" | [address | {"url": address, "minScale"?: scale denominator, "maxScale"?: one (the scales it is for, from minScale up to below maxScale)}] (at most 16; the Layers pane shows the one for the map's scale under the layer; service.add gives a WMS or WMTS layer its service's), "connection"?: "the id of the saved web-service connection it comes from (as service.add): the tile layer records it, and its tiles are read with its login"}"#,
        ),
        C::new("map.removeTiles", "Remove Tile Layer", "Home › Add", remove).roles(&[]).effects(crate::policy::Effects::NONE)
            .params(r#"{"name"?: text, "index"?: number, "id"?: catalog id} (removes every tile layer when none is given)"#),
        C::new("map.setTiles", "Tile Layer Properties", "Home › Add", set).inputs(set_inputs).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r#"{"name"?: text | "index"?: number (0 = top) | "id"?: catalog id, "visible"?: bool, "opacity"?: 0–1, "blend"?: "normal"|"multiply"|"screen"|"overlay"|"darken"|"lighten"|"difference", "move"?: "up"|"down"|"top"|"bottom" (past the next layer in the drawing order of all the map's layers, a feature or raster layer too), "to"?: number (its place in that order among the other layers, 0 = top; the Layers pane lists it), "rename"?: text, "bands"?: [red, green, blue] | [band] (a cloud-optimised GeoTIFF's bands shown, from 1; [] for its own), "range"?: [low, high] (the values shown from black to full colour; null: each band's 2nd–98th percentile), "dequantize"?: "aef" (AlphaEarth's quantised embeddings) | "none", "legend"?: as map.addTiles's, or null for none, "hiddenLayers"?: [source layer name] (vector tiles' source layers not drawn or identified, as map.tiles's sourceLayers names them; replaces the list, [] or null for none), "singleImage"?: bool (as map.addTiles's), "tileSize"?: 256|512|1024 (a WMS or map service asked for in Web Mercator: its tiles' size)}"#,
        ),
        C::new("map.tiles", "Tile Layers", "Home › Add", |s, _| Ok(list(s, false))).inputs(listing_inputs).roles(&[]).effects(crate::policy::Effects::NONE).view(),
        // As `layer.expand` for a layer's classes: what the pane shows, not an edit to undo.
        C::new("map.expandTiles", "Expand Tile Legend", "Layers", |s, p| {
            let i = find(s, p)?.ok_or("name or index is required")?;
            let on = b(p, "on");
            let t = s.map_mut().tiles.get_mut(i).ok_or("no tile layer at that index")?;
            t.expanded = on.unwrap_or(!t.expanded);
            Ok(json!({"index": i, "expanded": t.expanded}))
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .params(r#"{"name"?: text | "index"?: number (0 = top) | "id"?: catalog id, "on"?: bool (toggles when omitted)} — shows or hides a tile layer's legend and source layers under it in the Layers pane"#)
        .view(),
        // As `layer.select` for a map layer: what the pane shows, not an edit to undo.
        C::new("map.selectTiles", "Select Tile Layer", "Layers", select_tiles).roles(&[]).effects(crate::policy::Effects::NONE)
            .params(r#"{"name"?: text | "index"?: number (0 = top) | "id"?: catalog id} — picks a tile layer in the Layers pane, leaving no map layer active (picking one ends it): the Selected in Layers pop-up mode identifies it"#)
            .view(),
        C::new("map.zoomToTiles", "Zoom To Tile Layer", "Home › Add", zoom_to).roles(&[]).effects(crate::policy::Effects::NONE).inputs(zoom_inputs)
            .params(r#"{"name"?: text | "index"?: number (0 = top) | "id"?: catalog id} (advertised service bounds, or the bounds in a PMTiles archive or cloud-optimised GeoTIFF header)"#)
            .view(),
        C::new("map.body", "Celestial Body", "Home › Add", body).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r#"{"name"?: "earth"|"moon"|"mars"|"mercury"|"venus"|"io"|"europa"|"ganymede"|"callisto"|"titan"|"pluto"|"charon" (map.bodies lists them; default: the basemap's body), "basemap"?: a planetary basemap's id (default: the body's first; USGS Imagery on the Earth) | false (adds none, keeping the web basemaps but other bodies')}"#,
        ),
        C::new("map.bodies", "Celestial Bodies", "Home › Add", |s, _| Ok(bodies(s))).roles(&[]).effects(crate::policy::Effects::NONE).view(),
    ]
}

/// The bodies a map can be on, with their sizes and basemaps, and the one it is on.
fn bodies(s: &Session) -> Value {
    let list: Vec<Value> = rustgis_geom::BODIES
        .iter()
        .map(|b| {
            let maps: Vec<Value> = planets::basemaps_of(b.id).map(|m| json!({"id": m.id, "name": m.name})).collect();
            json!({"id": b.id, "name": b.name, "equatorialRadius": b.a, "meanRadius": b.mean_radius_m(), "basemaps": maps})
        })
        .collect();
    json!({"body": s.map().body().id, "bodies": list})
}

/// Moves the map to a celestial body: its size sets the map's distances, areas and scale, and
/// (unless `basemap` is false) one of its basemaps replaces the web basemaps.
fn body(s: &mut Session, p: &Value) -> CmdResult {
    let keep = p.get("basemap").and_then(Value::as_bool) == Some(false);
    let basemap = match ps(p, "basemap") {
        Some(id) => Some(planets::basemap(id).ok_or_else(|| format!("no planetary basemap '{id}' (map.bodies lists them)"))?),
        None => None,
    };
    let body = match ps(p, "name") {
        Some(n) => rustgis_geom::Body::find(n).ok_or_else(|| {
            let ids: Vec<&str> = rustgis_geom::BODIES.iter().map(|b| b.id).collect();
            format!("unknown body '{n}' ({})", ids.join(", "))
        })?,
        None => rustgis_geom::Body::get(basemap.ok_or("name or basemap is required")?.body),
    };
    if let Some(m) = basemap.filter(|m| m.body != body.id) {
        return Err(format!("'{}' is a basemap of {}, not {}", m.id, rustgis_geom::Body::get(m.body).name, body.name));
    }
    let basemap = if keep {
        None
    } else {
        basemap.or_else(|| if body.is_earth() { Some(&planets::EARTH_BASEMAP) } else { planets::basemaps_of(body.id).next() })
    };
    let m = s.map_mut();
    m.body = body.id.to_string();
    // Other bodies' basemaps always go (with `keep`, this body's and the Earth's web basemaps
    // stay). The default basemap below replaces the remaining web basemaps, keeping overlays.
    m.tiles.retain(|t| t.overlay || planets::basemap(&t.id).is_none_or(|b| keep && b.body == body.id));
    if let Some(b) = basemap {
        let tile = json!({"url": b.url, "kind": "raster", "name": format!("{} · {}", body.name, b.name), "attribution": b.attribution,
            "maxZoom": b.max_zoom, "tms": b.tms, "id": b.id, "replace": true});
        add(s, &tile)?;
    }
    Ok(json!({"body": body.id, "name": body.name, "meanRadius": body.mean_radius_m(), "basemap": basemap.map(|b| b.id)}))
}

/// The map's tile layers (index 0 on top), as `map.tiles` and `info.project` list them; `redact`
/// drops the credentials from their addresses.
pub(crate) fn list(s: &Session, redact: bool) -> Value {
    let picked = s.selected_tile();
    Value::Array(
        s.map()
            .tiles
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let url = if redact { crate::sources::redact(&t.url) } else { t.url.clone() };
                let mut o = json!({"index": i, "name": t.name, "id": t.id, "kind": t.kind.code(), "url": url, "visible": t.visible, "error": t.error,
                    "opacity": t.opacity, "blend": t.blend.code(), "overlay": t.overlay, "attribution": t.attribution, "minZoom": t.min_zoom, "maxZoom": t.max_zoom});
                // Its place in the drawing order: over this map layer, or (none) under them all.
                o["over"] = json!(s.map().tile_slot(t).and_then(|id| s.map().layer(id)).map(|l| l.name.clone()));
                // A layer its service draws on request: one picture of the view, or tiles of a
                // size (#1162).
                if t.on_request() {
                    o["singleImage"] = json!(t.single_image);
                    if t.sized_tiles() {
                        o["tileSize"] = json!(t.tile_size);
                    }
                }
                // Its legends, for a layer that has them, and whether they show in the Layers pane.
                if !t.legends.is_empty() {
                    let legends: Vec<_> = t
                        .legends
                        .iter()
                        .map(|l| rustgis_data::ogc::Legend { url: if redact { crate::sources::redact(&l.url) } else { l.url.clone() }, ..l.clone() })
                        .collect();
                    o["legends"] = json!(legends);
                    o["expanded"] = json!(t.expanded);
                }
                // A vector archive's source layers (once its header is in), and those switched off.
                if let Some(names) = rustgis_render::tile_source_layers(t) {
                    o["sourceLayers"] = json!(names);
                    o["expanded"] = json!(t.expanded);
                }
                if !t.hidden_layers.is_empty() {
                    o["hiddenLayers"] = json!(t.hidden_layers);
                }
                // Picked in the Layers pane (`map.selectTiles`).
                if picked == Some(i) {
                    o["selected"] = json!(true);
                }
                o
            })
            .collect(),
    )
}

/// Refusing one picture of the view for a layer whose service doesn't draw on request.
const NOT_ON_REQUEST: &str = "singleImage is for a layer its service draws on request (a WMS, an ArcGIS map or image service)";

/// Adds a tile layer, allowing at most one second for archive metadata on the command thread.
pub(crate) fn add(s: &mut Session, p: &Value) -> CmdResult {
    add_with_metadata_wait(s, p, web_time::Duration::from_secs(1))
}

/// The executor and its source-reach selector accept the same explicit or guessed kind.
fn add_kind(p: &Value, url: &str) -> Result<TileKind, String> {
    match ps(p, "kind") {
        Some(k) => TileKind::from_code(k).ok_or_else(|| format!("unknown kind '{k}' (raster, vector, style, pmtiles or mbtiles)")),
        None => Ok(TileKind::guess(url)),
    }
}

/// Local archive additions read files even though the command also declares network reach.
pub(crate) fn add_reach(p: &Value) -> crate::policy::Effects {
    let Some(url) = ps(p, "url").map(str::trim) else { return crate::policy::Effects::NONE };
    match add_kind(p, url) {
        Ok(kind @ (TileKind::PmTiles | TileKind::MbTiles)) => tile_source_reach(&TileLayer::new("", kind, url)),
        _ => crate::policy::Effects::NONE,
    }
}

/// Adds after a caller's header check; a zero wait avoids spending that caller's budget twice.
pub(crate) fn add_with_metadata_wait(s: &mut Session, p: &Value, metadata_wait: web_time::Duration) -> CmdResult {
    let bound = crate::service_logins::request_params(s, p)?;
    let p = bound.as_ref();
    let url = ps(p, "url").map(str::trim).filter(|u| !u.is_empty()).ok_or("url is required")?;
    let kind = add_kind(p, url)?;
    // A dated address (`{date:YYYY-MM-DD}`) follows the time slider, which the layer's span
    // (`start`, `end`) turns on when it is off.
    let dated = rustgis_data::time::is_template(url);
    if dated && !matches!(kind, TileKind::Raster | TileKind::Vector | TileKind::Cog) {
        return Err(format!("{url}: dated addresses work for raster, vector and GeoTIFF tile layers, not {}", kind.code()));
    }
    let at = if dated { dated_start(s, p)? } else { 0.0 };
    let mut t = TileLayer::new(
        ps(p, "name").unwrap_or_default(),
        kind,
        if dated { rustgis_data::time::fill_template(url, at) } else { url.to_string() },
    );
    if let Some(bounds) = p.get("bounds").filter(|v| !v.is_null()) {
        let bounds = serde::Deserialize::deserialize(bounds).map_err(|_| "bounds must be [west, south, east, north]")?;
        if !rustgis_map::tiles::valid_bounds(&bounds) {
            return Err("bounds must be a finite, ordered geographic rectangle".into());
        }
        t.bounds = Some(bounds);
    }
    if dated {
        t.time_url = Some(url.to_string());
        t.time_at = at;
    }
    if let Some(a) = ps(p, "attribution") {
        t.attribution = a.to_string();
    }
    if let Some(z) = f(p, "minZoom") {
        t.min_zoom = z.clamp(0.0, 24.0) as u8;
    }
    if let Some(z) = f(p, "maxZoom") {
        t.max_zoom = z.clamp(0.0, 24.0) as u8;
    } else if kind != TileKind::Raster {
        t.max_zoom = 14;
    }
    if let Some(n) = f(p, "tileSize") {
        t.tile_size = n.clamp(0.0, 4096.0) as u32;
    }
    t.tms = b(p, "tms").unwrap_or(false);
    t.overlay = b(p, "overlay").unwrap_or(false);
    if let Some(o) = f(p, "opacity") {
        t.opacity = o as f32;
    }
    if let Some(m) = blend(p)? {
        t.blend = m;
    }
    // Vector tiles' layer names (an OGC API collection's), each drawn in a colour of its own.
    if let Some(names) = p.get("layers").and_then(Value::as_array) {
        t.layers = names.iter().filter_map(Value::as_str).map(str::to_string).collect();
    }
    // The tiles' own look: MapLibre style layers, as a JSON array or its text.
    match p.get("styleLayers") {
        Some(Value::String(text)) => t.style_layers = rustgis_map::tiles::checked_style_layers(text)?,
        Some(v @ Value::Array(_)) => t.style_layers = rustgis_map::tiles::checked_style_layers(&v.to_string())?,
        Some(Value::Null) | None => {}
        Some(_) => return Err("styleLayers is a JSON array of MapLibre style layers".into()),
    }
    if let Some(subs) = p.get("subdomains") {
        t.subdomains = match subs {
            Value::Array(a) => a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
            Value::String(s) => s.chars().map(String::from).collect(),
            _ => Vec::new(),
        };
    }
    if let Some(id) = ps(p, "id") {
        t.id = id.to_string();
    }
    if let Some(v) = p.get("legend").filter(|v| !v.is_null()) {
        t.legends = legends(s, ps(p, "connection"), v)?;
    }
    t.repair()?;
    // A layer its service draws on request: one picture of the view, or tiles of the size given
    // (#1162). Added by its address as from its service: one picture unless tiles are asked for
    // (`singleImage: false`, or a tile size).
    match b(p, "singleImage") {
        Some(true) if !t.on_request() => return Err(NOT_ON_REQUEST.into()),
        Some(single) => t.single_image = single,
        None => t.single_image = t.on_request() && p.get("tileSize").is_none_or(Value::is_null),
    }
    if f(p, "tileSize").is_some() && t.sized_tiles() {
        t.size_tiles();
    }
    let replace = b(p, "replace").unwrap_or(false);
    let view = b(p, "view").unwrap_or(false);
    if view && t.kind != TileKind::Style {
        return Err("view is read from a MapLibre style's root properties; this is no style".into());
    }
    let url = t.url.clone();
    let m = s.map_mut();
    if replace {
        m.tiles.retain(|o| o.overlay);
    }
    // The same address and role again replaces the old one; overlays survive basemap changes.
    // Two looks of one archive (Fields of the World's fields and density) are told apart by
    // their names: the same look, or the same name, replaces.
    m.tiles.retain(|o| {
        !(o.url == t.url && o.kind == t.kind && o.overlay == t.overlay && (o.style_layers == t.style_layers || o.name == t.name))
    });
    if m.tiles.len() >= MAX_TILE_LAYERS {
        return Err(format!("a map holds at most {MAX_TILE_LAYERS} tile layers"));
    }
    // Overlays and data (an archive or GeoTIFF added as data) go on top of everything, the map's
    // layers too, as every new layer does (#808); basemaps go on top of the other basemaps,
    // under the map's layers and the overlays.
    let top = b(p, "top").unwrap_or(t.overlay);
    t.over = if top { m.layers.first().map(|l| l.id) } else { None };
    let at = if top { 0 } else { m.tiles.iter().position(|o| !o.overlay && o.over.is_none()).unwrap_or(m.tiles.len()) };
    let mut out = json!({"name": t.name, "kind": t.kind.code(), "index": at});
    if t.time_url.is_some() {
        out["url"] = json!(crate::sources::redact(&t.url));
        out["dated"] = json!(true);
    }
    // A file added as data (a catalog's archive, kept where the user put it, excepted) frames the
    // map on its bounds, PMTiles and MBTiles alike, whether it draws as a basemap or as data.
    let pmtiles = t.kind == TileKind::PmTiles && t.id.is_empty();
    let archive = pmtiles || (t.kind == TileKind::MbTiles && t.id.is_empty());
    let frame_tile = (view || archive).then(|| t.clone());
    m.tiles.insert(at, t);
    // A file's header is read now (natively), so a script's next command sees the map moved; a
    // file slower than a second to answer (a network mount) is framed when it does, as a web
    // address is, and one that can't be read says so when the layer draws.
    #[cfg(not(target_arch = "wasm32"))]
    if pmtiles && rustgis_render::rustgis_tiles::pmtiles::is_local(&url) {
        // Its error, if any, is the layer's to report when it draws.
        let _ = rustgis_render::rustgis_tiles::pmtiles::info_blocking(&url, metadata_wait);
    }
    if archive && !pmtiles {
        let _ = rustgis_render::rustgis_tiles::mbtiles::info_ready(&url, metadata_wait);
    }
    // A style goes to the view it gives, an archive to its bounds, once read (see
    // follow_style_view); a catalog's archive leaves the map where it is. The latest requested
    // view supersedes an older pending one, so a slow earlier add cannot override it.
    if let Some(tile) = frame_tile {
        s.style_view = Some((tile, s.map().camera, s.map().instance_id()));
        let zoomed = follow_style_view(s, web_time::Duration::ZERO);
        if archive {
            out["zoomed"] = json!(zoomed);
        }
    }
    Ok(out)
}

/// What a tile layer covers, from service metadata or an archive/GeoTIFF header.
/// `None` while a header is still being read.
fn extent_of(t: &TileLayer) -> Result<Option<rustgis_geom::Extent>, String> {
    use rustgis_render::rustgis_tiles::{mbtiles, pmtiles};
    let bounds = match (t.bounds, t.kind) {
        (Some(bounds), _) => Some(bounds),
        (None, TileKind::PmTiles) => pmtiles::info(&t.url)?.map(|i| i.bounds),
        (None, TileKind::MbTiles) => mbtiles::info_ready(&t.url, web_time::Duration::ZERO)?.map(|i| i.bounds),
        (None, TileKind::Cog) => cog::info(&t.url)?.map(|i| i.bounds),
        _ => return Err(format!("{}: a {} doesn't say what it covers", t.name, t.kind.label().to_lowercase())),
    };
    let Some([w, so, e, n]) = bounds else { return Ok(None) };
    let ok = [w, so, e, n].iter().all(|v| v.is_finite()) && w < e && so < n;
    let e = rustgis_geom::Extent::new(w.max(-180.0), so.max(-85.06), e.min(180.0), n.min(85.06));
    if !ok || e.is_empty() {
        return Err(format!("{}: no usable geographic bounds are known", t.name));
    }
    Ok(Some(e))
}

/// Whether an extent is (nearly) the whole world: a world archive added leaves the map where it is.
fn whole_world(e: &rustgis_geom::Extent) -> bool {
    e.width() >= 350.0 && e.height() >= 160.0
}

fn zoom_to(s: &mut Session, p: &Value) -> CmdResult {
    let i = find(s, p)?.ok_or("give the tile layer's name or index")?;
    let e = zoom_to_index(s, i)?;
    Ok(json!({"bounds": [e.min_x, e.min_y, e.max_x, e.max_y]}))
}

/// Zooms to the same bounds from the Layers pane or a search hit.
pub(super) fn zoom_to_index(s: &mut Session, i: usize) -> Result<rustgis_geom::Extent, String> {
    let t = s.map().tiles.get(i).ok_or("no tile layer at that index")?;
    let e = extent_of(t)?.ok_or_else(|| format!("{}: still being read; try again in a moment", t.name))?;
    s.zoom_to(&e);
    Ok(e)
}

/// The tile layer a retained search hit names: the match nearest the stack position the hit was
/// listed at, so layers that are wholly alike (the same one added twice) still resolve. Zero
/// matches mean the layer has gone.
pub fn index_of_identity(s: &Session, identity: &super::search::TileIdentity, near: usize) -> Option<usize> {
    s.map().tiles.iter().enumerate().filter(|(_, tile)| identity.matches(tile)).map(|(i, _)| i).min_by_key(|i| i.abs_diff(near))
}

/// The date a dated tile layer starts at: the time slider's step. Its `start` (and `end`,
/// `unit`, `interval` or `dates`) set the slider up, turning it on when it is off. Should a later
/// check of the layer fail, the session restores the project, the slider with it (tested).
fn dated_start(s: &mut Session, p: &Value) -> Result<f64, String> {
    let keys = ["start", "end", "unit", "interval", "dates"];
    let mut given: serde_json::Map<String, Value> = keys.iter().filter_map(|k| p.get(*k).map(|v| (k.to_string(), v.clone()))).collect();
    if given.is_empty() {
        return s
            .map()
            .time
            .as_ref()
            .map(|t| t.current)
            .ok_or_else(|| "this address has dates: give its start (and end), or turn the time slider on first".to_string());
    }
    // Steps by what the address changes by: a day for `{date:YYYY-MM-DD}`, a year for `{YYYY}`.
    if !given.contains_key("unit")
        && let Some(u) = ps(p, "url").and_then(rustgis_data::time::template_unit)
    {
        given.insert("unit".into(), json!(u.code()));
    }
    super::time::set(s, &Value::Object(given))?;
    Ok(s.map().time.as_ref().map_or(0.0, |t| t.current))
}

/// The most passes made waiting for a style to load (each waits for the fetches under way).
const STYLE_WAITS: usize = 8;

/// Moves the map to the view in the root properties of the style added with `"view": true`
/// once it has loaded, and frames an archive on its bounds, unless the map has moved since,
/// waiting up to `wait` for it (natively; the browser loads between frames). True when the map
/// moved.
pub(crate) fn follow_style_view(s: &mut Session, wait: web_time::Duration) -> bool {
    let Some((pending, camera, map)) = s.style_view.as_ref() else { return false };
    if s.map().instance_id() != *map || s.map().camera != *camera {
        // Moved (panned, a bookmark) while the style loaded: the map stays where it was put.
        s.style_view = None;
        return false;
    }
    // Framing depends on the source and advertised bounds, not its display settings or name.
    // Same-URL layers with the same bounds have the same framing; a distinct overlay cannot
    // replace the requested bounds, and opacity/visibility/order edits do not lose the request.
    let Some(t) = s
        .map()
        .tiles
        .iter()
        .find(|t| t.kind == pending.kind && t.url == pending.url && t.bounds == pending.bounds && t.id == pending.id)
        .cloned()
    else {
        s.style_view = None;
        return false;
    };
    let url = t.url.clone();
    let wait = if cfg!(target_arch = "wasm32") { web_time::Duration::ZERO } else { wait };
    if matches!(t.kind, TileKind::PmTiles | TileKind::MbTiles) {
        return follow_archive(s, &t, wait);
    }
    let start = web_time::Instant::now();
    let mut passes = 0;
    let view = loop {
        match rustgis_render::style_view(&t) {
            Ok(Some(v)) => break v,
            Ok(None) if passes < STYLE_WAITS && start.elapsed() < wait => {
                rustgis_render::rustgis_tiles::cache::wait(wait.saturating_sub(start.elapsed()));
                passes += 1;
            }
            Ok(None) => return false,
            Err(e) => {
                s.style_view = None;
                s.notify(format!("{url}: {e}"), true);
                return false;
            }
        }
    };
    s.style_view = None;
    if view.is_empty() {
        return false;
    }
    let center = view.center.map(|[lon, lat]| rustgis_geom::Coord::new(lon, lat));
    match super::navigate::go_to_view(s, center, view.zoom, view.bearing, view.pitch) {
        Ok(()) => true,
        Err(e) => {
            s.notify(format!("{url}: {e}"), true);
            false
        }
    }
}

/// Frames the map on an archive (a PMTiles or MBTiles file) added (see [`follow_style_view`]) once
/// its header is read, unless it covers the whole world. Unreadable, it stays where it is: the
/// layer says why when it draws.
fn follow_archive(s: &mut Session, t: &TileLayer, wait: web_time::Duration) -> bool {
    let start = web_time::Instant::now();
    let mut passes = 0;
    let e = loop {
        match extent_of(t) {
            Ok(Some(e)) => break e,
            Ok(None) if passes < STYLE_WAITS && start.elapsed() < wait => {
                rustgis_render::rustgis_tiles::cache::wait(wait.saturating_sub(start.elapsed()));
                passes += 1;
            }
            Ok(None) => return false,
            Err(_) => {
                s.style_view = None;
                return false;
            }
        }
    };
    s.style_view = None;
    if whole_world(&e) {
        return false;
    }
    s.zoom_to(&e);
    true
}

/// The tile layer a command names by `index`, `name` or `id`.
pub(crate) fn find(input: &impl super::InputSource, p: &Value) -> Result<Option<usize>, String> {
    let context = input.input_context();
    let tiles = &context.map().tiles;
    if let Some(i) = f(p, "index") {
        let i = (i >= 0.0 && i.fract() == 0.0).then_some(i as usize).filter(|i| *i < tiles.len());
        return i.map(Some).ok_or_else(|| "no tile layer at that index".to_string());
    }
    if let Some(n) = ps(p, "name") {
        return tiles.iter().position(|t| t.name.eq_ignore_ascii_case(n.trim())).map(Some).ok_or_else(|| format!("no tile layer '{n}'"));
    }
    if let Some(id) = ps(p, "id") {
        return tiles.iter().position(|t| t.id == id).map(Some).ok_or_else(|| format!("no tile layer '{id}'"));
    }
    Ok(None)
}

/// Exact source and routing state, kept private and charged to the planner's existing size limit.
/// Inline style layers change drawing, not the tile source, and are deliberately not copied here.
fn tile_source_state(t: &TileLayer) -> Value {
    json!({
        "name":t.name,"id":t.id,"kind":t.kind.code(),"url":t.url,"bounds":t.bounds,
        "connection":t.connection,"anonymous":t.anonymous,"subdomains":t.subdomains,
        "minZoom":t.min_zoom,"maxZoom":t.max_zoom,"tileSize":t.tile_size,"tms":t.tms,
        "singleImage":t.single_image,"layers":t.layers,"hiddenLayers":t.hidden_layers,
        "timeUrl":t.time_url,"timeUrls":t.time_urls,"timeAt":t.time_at,"timeHidden":t.time_hidden,
        "legends":t.legends,"visible":t.visible,"opacity":t.opacity,"error":t.error.is_some(),
        "overlay":t.overlay,"over":t.over
    })
}

/// Binds the executor's chosen tile before its coverage header can be read.
fn zoom_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let i = find(input, p)?.ok_or("give the tile layer's name or index")?;
    Ok(tile_inputs(input.map(), [i]))
}

/// Freezes only the selected tile routes, with their stack positions and owning map.
pub(crate) fn tile_inputs(map: &rustgis_map::Map, indices: impl IntoIterator<Item = usize>) -> super::Inputs {
    let mut inputs = super::Inputs::layers(std::iter::empty());
    inputs.maps.push(map.instance_id());
    let tiles: Vec<_> = indices
        .into_iter()
        .filter_map(|i| {
            map.tiles.get(i).map(|tile| {
                inputs.review_places.push(crate::sources::redact(&tile.url));
                json!({"index":i,"tile":tile_source_state(tile)})
            })
        })
        .collect();
    inputs.state = json!({"map":map.instance_id(),"tiles":tiles});
    inputs
}

/// Archive names in the returned list need metadata even when their bounds are already known.
fn listing_indices(map: &rustgis_map::Map) -> impl Iterator<Item = usize> + '_ {
    map.tiles.iter().enumerate().filter(|(_, t)| matches!(t.kind, TileKind::PmTiles | TileKind::MbTiles)).map(|(i, _)| i)
}

/// Listing source-layer names asks archive metadata for every archive, even with known bounds.
pub(crate) fn listing_inputs(input: &super::InputContext<'_>, _: &Value) -> Result<super::Inputs, String> {
    Ok(tile_inputs(input.map(), listing_indices(input.map())))
}

/// Binds the property target and archive routes read for the returned listing.
fn set_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let i = find(input, p)?.ok_or("name or index is required")?;
    let mut indices: Vec<_> = listing_indices(input.map()).collect();
    if !indices.contains(&i) {
        indices.push(i);
    }
    Ok(tile_inputs(input.map(), indices))
}

/// An uploaded MBTiles archive is already in memory; only unwrapped local archives read files.
/// Anonymous S3 and saved-login wrappers retain network reach even if their payload is malformed.
fn tile_source_reach(t: &TileLayer) -> crate::policy::Effects {
    use crate::policy::{Effect, Effects};
    use rustgis_render::rustgis_tiles::{mbtiles, s3};
    if t.kind == TileKind::MbTiles && mbtiles::is_upload(&t.url) {
        Effects::NONE
    } else if matches!(t.kind, TileKind::PmTiles | TileKind::MbTiles)
        && !s3::split_anonymous(&t.url).1
        && rustgis_map::tiles::is_local_archive(&t.url)
    {
        Effects::NONE.with(Effect::Files)
    } else {
        Effects::NONE.with(Effect::Network)
    }
}

/// Mirrors `extent_of`: advertised bounds and kinds without a coverage reader do no I/O.
pub(crate) fn metadata_reach(input: &impl super::InputSource, p: &Value) -> crate::policy::Effects {
    let context = input.input_context();
    let tile = find(&context, p).ok().flatten().and_then(|i| context.map().tiles.get(i));
    tile.map_or(crate::policy::Effects::NONE, coverage_reach)
}

/// Coverage is read only when it was not supplied by the service or project.
pub(crate) fn coverage_reach(t: &TileLayer) -> crate::policy::Effects {
    if t.bounds.is_none() && matches!(t.kind, TileKind::PmTiles | TileKind::MbTiles | TileKind::Cog) {
        tile_source_reach(t)
    } else {
        crate::policy::Effects::NONE
    }
}

/// Archive source-layer names come from metadata rather than the stored geographic bounds.
pub(crate) fn listing_reach(map: &rustgis_map::Map) -> crate::policy::Effects {
    listing_indices(map)
        .filter_map(|i| map.tiles.get(i))
        .fold(crate::policy::Effects::NONE, |reach, tile| reach.union(tile_source_reach(tile)))
}

/// Setting a COG look checks its header; the command's returned list also reads archive names.
pub(crate) fn set_reach(input: &super::InputContext<'_>, p: &Value) -> crate::policy::Effects {
    let mut reach = listing_reach(input.map());
    if matches!(cog_look(p), Ok(Some(_)))
        && let Some(tile) = find(input, p).ok().flatten().and_then(|i| input.map().tiles.get(i))
        && tile.kind == TileKind::Cog
    {
        reach = reach.union(tile_source_reach(tile));
    }
    reach
}

/// Retains raw tile routes alongside the map and ordinary layers consumed by an export.
/// Only separately redacted addresses enter its public review; private state is never a label.
pub(crate) fn render_inputs(map: &rustgis_map::Map) -> super::Inputs {
    let mut inputs = super::Inputs::layers(map.layers.iter().map(|layer| (map, layer)));
    inputs.maps.push(map.instance_id());
    inputs.state = json!({"map":map.instance_id(),"tiles":map.tiles.iter().map(tile_source_state).collect::<Vec<_>>()});
    for tile in map.tiles.iter().filter(|t| t.draws() && t.opacity > 0.0) {
        inputs.review_places.push(crate::sources::redact(&tile.url));
        inputs.review_places.extend(tile.legends.iter().map(|l| crate::sources::redact(&l.url)));
    }
    inputs
}

/// Source reach of tiles a referenced map draws, including their web legend pictures.
/// The map's built-in basemap is a drawn palette and has no external source.
pub(crate) fn render_reach(map: &rustgis_map::Map) -> crate::policy::Effects {
    map.tiles.iter().fold(crate::policy::Effects::NONE, |reach, tile| reach.union(render_tile_reach(tile)))
}

/// A resolved frame's tile uses the same drawing and source rules as an ordinary map export.
pub(crate) fn render_tile_reach(tile: &TileLayer) -> crate::policy::Effects {
    if !(tile.draws() && tile.opacity > 0.0) {
        return crate::policy::Effects::NONE;
    }
    let reach = tile_source_reach(tile);
    if tile.legends.is_empty() { reach } else { reach.with(crate::policy::Effect::Network) }
}

/// Picks a tile layer in the Layers pane: the only one picked, with no map layer active.
fn select_tiles(s: &mut Session, p: &Value) -> CmdResult {
    let i = find(s, p)?.ok_or("name or index is required")?;
    s.set_layer_selection(Vec::new(), None);
    for (j, t) in s.map_mut().tiles.iter_mut().enumerate() {
        t.selected = j == i;
    }
    Ok(json!({"index": i}))
}

fn remove(s: &mut Session, p: &Value) -> CmdResult {
    match find(s, p)? {
        Some(i) => {
            let t = s.map_mut().tiles.remove(i);
            super::layer::legends_follow_layer(s, &t.name, None);
            Ok(json!({"removed": [t.name]}))
        }
        None => {
            let gone: Vec<String> = s.map_mut().tiles.drain(..).map(|t| t.name).collect();
            for n in &gone {
                super::layer::legends_follow_layer(s, n, None);
            }
            Ok(json!({"removed": gone}))
        }
    }
}

/// The `blend` mode a command gives, if any.
fn blend(p: &Value) -> Result<Option<Blend>, String> {
    ps(p, "blend").map(|m| Blend::from_code(m).ok_or_else(|| format!("unknown blend mode '{m}'"))).transpose()
}

/// Turns a map's plain COG tile layers into raster layers read in place, each where it was drawn,
/// with its name, visibility, opacity, blend, look, connection and anonymity: those of projects saved before GeoTIFFs read
/// in place were raster layers, and those `map.addTiles` (kind `cog`) still adds, as a project or
/// a map file opens, and then only: a tile layer added this session stays one until then (a
/// plain GeoTIFF is one kind of layer, however it was added; what a tile layer had that a raster
/// layer hasn't is said once, see `Session::notice_landed_rasters`). A dated series, a data cube's
/// files and AlphaEarth's embeddings stay tile layers.
pub(crate) fn cog_tiles_to_rasters(m: &mut rustgis_map::Map) {
    use rustgis_map::raster::RasterLayer;
    let mut i = 0;
    while let Some(t) = m.tiles.get(i) {
        let (file, look) = cog::split(&t.url);
        if t.kind != TileKind::Cog
            || t.time_url.is_some()
            || !t.time_urls.is_empty()
            || look.aef
            || look.cube
            || rustgis_data::time::is_template(file)
        {
            i += 1;
            continue;
        }
        let t = m.tiles.remove(i);
        let (file, look) = cog::split(&t.url);
        // What a raster layer hasn't, said once the project is open (see
        // `Session::notice_landed_rasters`).
        let plain = rustgis_map::TileLayer::new(&t.name, TileKind::Cog, file);
        let mut dropped = Vec::new();
        if !t.attribution.trim().is_empty() {
            dropped.push("attribution");
        }
        if !t.legends.is_empty() {
            dropped.push("legend");
        }
        if (t.min_zoom, t.max_zoom) != (plain.min_zoom, plain.max_zoom) {
            dropped.push("zoom range");
        }
        if t.bounds.is_some() {
            dropped.push("bounds");
        }
        if t.overlay {
            dropped.push("overlay");
        }
        // Read with the project's other rasters read in place (see `project::read_in_place`), in
        // its look once it lands.
        let rl = RasterLayer::unread_in_place(file).with_look(address_look(&look)).from_tiles_dropping(dropped);
        // A name another layer has: numbered, as a layer added is.
        let mut name = t.name.clone();
        let mut nth = 1;
        while m.layer_by_name(&name).is_some() {
            nth += 1;
            name = format!("{} ({nth})", t.name);
        }
        let id = m.add_raster(name, rl);
        if let Some(l) = m.layer_mut(id) {
            l.visible = t.visible;
            l.transparency = (1.0 - t.opacity.clamp(0.0, 1.0)) * 100.0;
            l.blend = t.blend;
            // Read again through the saved connection it came from, as the tile layer was; one an
            // anonymous plugin command added takes no saved login, as before.
            l.connection = t.connection.clone();
            l.anonymous = t.anonymous;
        }
        // Just over the layer it was drawn over, in its group; under them all without one.
        let group = t.over.and_then(|o| m.group_of(o).map(|g| g.id));
        if m.place_layer(id, group, t.over).is_err() {
            let _ = m.place_layer(id, None, None);
        }
    }
}

/// The look a COG tile layer's address keeps (see [`Look`]) as a raster layer's: its bands, one
/// grey or three as red, green and blue, and its fixed range. A COG tile layer becomes a raster
/// layer read in place with it.
pub(crate) fn address_look(look: &Look) -> rustgis_map::raster::AddressLook {
    rustgis_map::raster::AddressLook { bands: look.bands.clone(), range: look.range.map(|(lo, hi)| (f64::from(lo), f64::from(hi))) }
}

/// A change of a COG layer's look a command asks for (`bands`, `range`, `dequantize`), applied to
/// the look it has; `None` when it asks for none.
#[allow(clippy::type_complexity)]
fn cog_look(p: &Value) -> Result<Option<Box<dyn Fn(Look) -> Look>>, String> {
    if p.get("bands").is_none() && p.get("range").is_none() && p.get("dequantize").is_none() {
        return Ok(None);
    }
    let bands: Option<Vec<usize>> = match p.get("bands") {
        None => None,
        Some(Value::Array(a)) => {
            let b: Option<Vec<usize>> = a.iter().map(|v| v.as_u64().filter(|b| (1..=65_535).contains(b)).map(|b| b as usize - 1)).collect();
            match b {
                Some(b) if matches!(b.len(), 0 | 1 | 3) => Some(b),
                _ => return Err("bands is [red, green, blue] or [band], each from 1".into()),
            }
        }
        Some(_) => return Err("bands is [red, green, blue] or [band], each from 1".into()),
    };
    let range: Option<Option<(f32, f32)>> = match p.get("range") {
        None => None,
        Some(Value::Null) => Some(None),
        Some(Value::Array(a)) => match (a.first().and_then(Value::as_f64), a.get(1).and_then(Value::as_f64), a.len()) {
            // Finite as the 32-bit values kept, not only as given.
            (Some(lo), Some(hi), 2) if (lo as f32).is_finite() && (hi as f32).is_finite() && (hi as f32) > (lo as f32) => {
                Some(Some((lo as f32, hi as f32)))
            }
            _ => return Err("range is [low, high], low below high".into()),
        },
        Some(_) => return Err("range is [low, high], low below high".into()),
    };
    let aef = match ps(p, "dequantize") {
        None => None,
        Some("aef") => Some(true),
        Some("none") => Some(false),
        Some(other) => return Err(format!("unknown dequantize '{other}' (aef or none)")),
    };
    Ok(Some(Box::new(move |old: Look| Look {
        bands: bands.clone().unwrap_or(old.bands),
        range: range.unwrap_or(old.range),
        aef: aef.unwrap_or(old.aef),
        cube: old.cube,
    })))
}

/// A legend given to `map.addTiles` or `map.setTiles`: an address, or a list of addresses and
/// `{url, minScale?, maxScale?}` (the scales each is for). Each must be a web address; a scale
/// that isn't a positive number is none. A login wrapper in it must be this session's own (or is
/// bound to `connection`): another session's token never gives this one its credential.
fn legends(s: &mut Session, connection: Option<&str>, v: &Value) -> Result<Vec<rustgis_data::ogc::Legend>, String> {
    use rustgis_data::ogc::{Legend, MAX_LEGENDS};
    let bad = || "legend must be a web address, or a list of addresses and {url, minScale?, maxScale?}".to_string();
    let mut one = |v: &Value| -> Result<Legend, String> {
        let mut legend = match v {
            Value::String(url) => Legend { url: url.clone(), ..Legend::default() },
            Value::Object(o) => {
                let url = o.get("url").and_then(Value::as_str).ok_or_else(bad)?;
                let scale = |k: &str| o.get(k).and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())));
                Legend { url: url.to_string(), min_scale: scale("minScale"), max_scale: scale("maxScale") }
            }
            _ => return Err(bad()),
        };
        // A wrapper must be this session's own (checked on the trimmed text the layer keeps).
        legend.url = legend.url.trim().to_string();
        if rustgis_data::login_address::is_wrapper(&legend.url) {
            legend.url = crate::service_logins::request(s, connection.filter(|c| !c.trim().is_empty()), &legend.url)?;
        }
        // Kept as a tile layer keeps it, or refused: an address the layer wouldn't keep.
        rustgis_map::tiles::checked_legends([legend.clone()]).pop().ok_or_else(|| {
            let url: String = legend.url.chars().take(200).collect();
            format!("{url}: a legend is a web address (http or https) of at most {} bytes", rustgis_map::tiles::MAX_LEGEND_URL)
        })
    };
    let list = match v {
        Value::Array(list) if list.len() <= MAX_LEGENDS => list.iter().map(one).collect::<Result<Vec<_>, _>>()?,
        Value::Array(_) => return Err(format!("a tile layer has at most {MAX_LEGENDS} legends")),
        v => vec![one(v)?],
    };
    Ok(list)
}

/// The `hiddenLayers` a command gives, if any: a list of names (null for none).
fn hidden_layers(p: &Value) -> Result<Option<Vec<String>>, String> {
    use rustgis_map::tiles::{MAX_SOURCE_LAYERS, checked_source_layers};
    let bad = || "hiddenLayers is a list of source layer names".to_string();
    match p.get("hiddenLayers") {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(Vec::new())),
        Some(Value::Array(names)) if names.len() > MAX_SOURCE_LAYERS => Err(format!("at most {MAX_SOURCE_LAYERS} hidden source layers")),
        Some(Value::Array(names)) => {
            let names: Vec<&str> = names.iter().map(Value::as_str).collect::<Option<_>>().ok_or_else(bad)?;
            Ok(Some(checked_source_layers(names)))
        }
        Some(_) => Err(bad()),
    }
}

fn set(s: &mut Session, p: &Value) -> CmdResult {
    let i = find(s, p)?.ok_or("name or index is required")?;
    let mode = blend(p)?;
    let look = cog_look(p)?;
    let mut new_legends = match p.get("legend") {
        None => None,
        Some(Value::Null) => Some(Vec::new()),
        Some(v) => Some(legends(s, None, v)?),
    };
    // A plain legend address of a layer that recorded a logged-in connection reads through it,
    // as the layer's own address does.
    if let Some(connection) = s.map().tiles.get(i).map(|t| t.connection.clone()).filter(|c| !c.is_empty()) {
        for legend in new_legends.iter_mut().flatten() {
            legend.url = crate::service_logins::bind_logged_in(s, &connection, &legend.url)?;
        }
    }
    let hidden = hidden_layers(p)?;
    let old_name = s.map().tiles.get(i).map(|t| t.name.clone());
    let m = s.map_mut();
    let Some(t) = m.tiles.get_mut(i) else { return Err("no tile layer at that index".into()) };
    // Refused before anything changes.
    if look.is_some() && t.kind != TileKind::Cog {
        return Err("bands, range and dequantize are for a cloud-optimised GeoTIFF's layer".into());
    }
    if hidden.as_ref().is_some_and(|h| !h.is_empty()) && matches!(t.kind, TileKind::Raster | TileKind::Cog) {
        return Err("hiddenLayers is for vector tiles' source layers".into());
    }
    let single = b(p, "singleImage");
    if single == Some(true) && !t.on_request() {
        return Err(NOT_ON_REQUEST.into());
    }
    let side = match p.get("tileSize").filter(|v| !v.is_null()) {
        None => None,
        Some(v) => match v.as_f64().filter(|n| [256.0, 512.0, 1024.0].contains(n)) {
            Some(n) if t.sized_tiles() => Some(n as u32),
            Some(_) => return Err("tileSize is for a WMS or map service layer asked for in Web Mercator".into()),
            None => return Err("tileSize is 256, 512 or 1024".into()),
        },
    };
    let new_url = match look {
        Some(look) => {
            let new = look(cog::split(&t.url).1);
            // Bands the file hasn't (when its header is in): said, not shown as its own look.
            if let Ok(Some(info)) = cog::info(&t.url)
                && let Some(b) = new.bands.iter().find(|b| **b >= info.layout.bands)
            {
                return Err(format!("the GeoTIFF has {} bands, not {}", info.layout.bands, b.saturating_add(1)));
            }
            Some(cog::with_look(&t.url, &new))
        }
        None => None,
    };
    if let Some(v) = b(p, "visible") {
        t.visible = v;
    }
    if let Some(single) = single {
        t.single_image = single;
    }
    if let Some(side) = side {
        t.tile_size = side;
        t.size_tiles();
    }
    if let Some(l) = new_legends {
        t.legends = l;
    }
    if let Some(h) = hidden {
        t.hidden_layers = h;
    }
    if let Some(o) = f(p, "opacity") {
        t.opacity = if o.is_finite() { o.clamp(0.0, 1.0) as f32 } else { 1.0 };
    }
    if let Some(m) = mode {
        t.blend = m;
    }
    if let Some(r) = ps(p, "rename").map(str::trim).filter(|r| !r.is_empty()) {
        t.name = r.chars().take(120).collect();
    }
    if let Some(url) = new_url {
        // A dated layer keeps the look at every date: its template takes it too.
        if let Some(template) = t.time_url.as_mut() {
            *template = cog::with_look(template, &cog::split(&url).1);
        }
        t.url = url;
    }
    let new_name = t.name.clone();
    // In the drawing order of all the map's layers: past the next layer, a map layer too (#808).
    match (ps(p, "move"), p.get("to").filter(|v| !v.is_null())) {
        (Some(_), Some(_)) => return Err("give move or to, not both".into()),
        (Some(mv), None) => {
            m.move_tile(i, mv)?;
        }
        (None, Some(to)) => {
            let to = to.as_u64().ok_or("to is a place in the drawing order: a whole number, 0 at the top")?;
            m.place_tile(i, usize::try_from(to).unwrap_or(usize::MAX))?;
        }
        (None, None) => {}
    }
    // Legends that leave it out follow it to its new name.
    if let Some(old) = old_name.filter(|o| *o != new_name) {
        super::layer::legends_follow_layer(s, &old, Some(&new_name));
    }
    Ok(list(s, false))
}

#[cfg(test)]
mod tests {
    use crate::{Project, Session};
    use serde_json::json;

    #[test]
    fn stacks_tile_layers() {
        let mut s = Session::new(Project::default());
        let osm = json!({"url": "https://tile.openstreetmap.org/{z}/{x}/{y}.png", "name": "OSM", "attribution": "© OpenStreetMap"});
        assert!(s.run("map.addTiles", &osm).is_ok());
        let labels = json!({"url": "https://x.example.com/labels/{z}/{x}/{y}.png", "name": "Labels", "overlay": true});
        assert!(s.run("map.addTiles", &labels).is_ok());
        let style = json!({"url": "https://tiles.openfreemap.org/styles/liberty", "name": "Liberty"});
        assert!(s.run("map.addTiles", &style).is_ok());
        let names: Vec<&str> = s.map().tiles.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["Labels", "Liberty", "OSM"]);
        assert_eq!(s.map().tiles[1].kind, rustgis_map::TileKind::Style);
        // Replacing keeps the overlays.
        assert!(s.run("map.addTiles", &json!({"url": "https://y.example.com/{z}/{x}/{y}.jpg", "replace": true})).is_ok());
        let names: Vec<&str> = s.map().tiles.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["Labels", "y.example.com"]);
        assert!(s.run("map.setTiles", &json!({"name": "labels", "opacity": 0.5, "move": "bottom"})).is_ok());
        assert_eq!(s.map().tiles[1].opacity, 0.5);
        assert!(s.run("edit.undo", &json!({})).is_ok());
        assert_eq!(s.map().tiles[0].name, "Labels");
        assert!(s.run("map.addTiles", &json!({"url": "file:///etc/passwd"})).is_err());
        assert!(s.run("map.addTiles", &json!({"url": "https://a/{z}", "kind": "nope"})).is_err());
        assert!(s.run("map.removeTiles", &json!({"index": 7})).is_err());
        // A look is for a COG's layer: refused for others, which are left as they were.
        assert!(s.run("map.setTiles", &json!({"index": 0, "opacity": 0.2, "bands": [1, 2, 3]})).is_err());
        assert_ne!(s.map().tiles[0].opacity, 0.2);
        // A range past what 32 bits hold is refused.
        assert!(super::cog_look(&json!({"range": [0, 1e39]})).is_err());
        assert!(super::cog_look(&json!({"range": [-0.3, 0.3]})).is_ok_and(|l| l.is_some()));
        assert!(s.run("map.removeTiles", &json!({})).is_ok());
        assert!(s.map().tiles.is_empty());
        // Saved and read back with the project.
        assert!(s.run("map.addTiles", &osm).is_ok());
        let text = serde_json::to_string(&s.project).unwrap();
        let p = Project::from_json(&text).unwrap();
        assert_eq!(p.map().tiles.len(), 1);
        assert_eq!(p.map().tiles[0].attribution, "© OpenStreetMap");
    }

    #[test]
    fn stacks_basemaps_and_blends_them() {
        use rustgis_map::effects::Blend;
        let mut s = Session::new(Project::default());
        let relief = json!({"url": "https://relief.example.com/{z}/{x}/{y}.png", "name": "Relief", "replace": true});
        assert!(s.run("map.addTiles", &relief).is_ok());
        // Without `replace`, a second basemap goes over the first, here multiplied with it.
        let imagery = json!({"url": "https://imagery.example.com/{z}/{x}/{y}.jpg", "name": "Imagery", "blend": "multiply"});
        assert!(s.run("map.addTiles", &imagery).is_ok());
        let names: Vec<&str> = s.map().tiles.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["Imagery", "Relief"]);
        assert_eq!(s.map().tiles[0].blend, Blend::Multiply);
        assert_eq!(s.map().tiles[1].blend, Blend::Normal);
        let listed = s.run("map.tiles", &json!({})).unwrap();
        assert_eq!(listed[0]["blend"], "multiply");
        assert!(s.run("map.setTiles", &json!({"name": "Relief", "blend": "Screen"})).is_ok());
        assert_eq!(s.map().tiles[1].blend, Blend::Screen);
        assert!(s.run("edit.undo", &json!({})).is_ok());
        assert_eq!(s.map().tiles[1].blend, Blend::Normal);
        assert!(s.run("map.setTiles", &json!({"index": 0, "blend": "nope"})).is_err());
        assert!(s.run("map.addTiles", &json!({"url": "https://z.example.com/{z}/{x}/{y}.png", "blend": "nope"})).is_err());
        assert_eq!(s.map().tiles.len(), 2);
        // Saved with the project; a normal layer saves no blend.
        let text = serde_json::to_string(&s.project).unwrap();
        let p = Project::from_json(&text).unwrap();
        assert_eq!(p.map().tiles[0].blend, Blend::Multiply);
        assert_eq!(text.matches("\"blend\"").count(), 1, "{text}");
    }

    /// A login wrapper given as a legend or a download's address is that session's own, or refused.
    #[test]
    fn another_sessions_login_address_is_refused_in_legends_and_downloads() {
        use crate::{render::rustgis_tiles::logins, service_logins::NOT_THIS_SESSION};
        let origin = "https://legend-owner757.example.org";
        let definition = json!({"kind": "wms", "address": format!("{origin}/wms"), "auth": "header", "keyName": "X-Key"});
        let fresh = || {
            let mut s = Session::new(Project::default());
            s.database_logins.set_store(None);
            s
        };
        let id_of = |s: &mut Session| s.run("connection.new", &definition).unwrap()["id"].as_str().unwrap().to_string();
        let (mut a, mut b) = (fresh(), fresh());
        let (ida, idb) = (id_of(&mut a), id_of(&mut b));
        a.run("connection.login", &json!({"id": ida, "apiKey": "owner-a-757"})).unwrap();
        let theirs = crate::service_logins::request(&mut a, Some(&ida), &format!("{origin}/legend.png")).unwrap();
        let key = |address: &str| logins::apply(address).unwrap().and_then(|x| x.header).map(|(_, v)| v);
        assert_eq!(key(&theirs).as_deref(), Some("owner-a-757"));
        // The tile layer itself is on another origin, so only the legend carries a wrapper.
        let tiles =
            |legend: serde_json::Value| json!({"url": "https://basemap757.example.org/{z}/{x}/{y}.png", "name": "T", "legend": legend});
        for legend in [json!(theirs), json!(format!("  {theirs}")), json!({"url": theirs}), json!([theirs])] {
            assert_eq!(b.run("map.addTiles", &tiles(legend)).unwrap_err(), NOT_THIS_SESSION);
        }
        assert!(b.map().tiles.is_empty());
        // The owner keeps its own wrapper.
        a.run("map.addTiles", &tiles(json!(theirs))).unwrap();
        assert_eq!(a.map().tiles[0].legends[0].url, theirs);
        // Set on an existing layer: refused and left as it was.
        b.run("map.addTiles", &json!({"url": "https://basemap757.example.org/{z}/{x}/{y}.png", "name": "T"})).unwrap();
        assert_eq!(b.run("map.setTiles", &json!({"name": "T", "legend": theirs})).unwrap_err(), NOT_THIS_SESSION);
        assert!(b.map().tiles[0].legends.is_empty());
        // Given with its own connection, the wrapper's plain address is bound to that login.
        b.run("connection.login", &json!({"id": idb, "apiKey": "owner-b-757"})).unwrap();
        let mut add = tiles(json!(theirs));
        add["connection"] = json!(idb);
        add["replace"] = json!(true);
        b.run("map.addTiles", &add).unwrap();
        let mine = b.map().tiles[0].legends[0].url.clone();
        assert_ne!(mine, theirs);
        assert_eq!(key(&mine).as_deref(), Some("owner-b-757"));
        // A downloaded file's address: refused before the download is taken.
        let id = b.download_started();
        assert!(b.download_landed(id, b"{}".to_vec()));
        let err = b.run("layer.addDownload", &json!({"id": id, "url": theirs})).unwrap_err();
        assert_eq!(err, NOT_THIS_SESSION);
        assert!(b.downloaded.iter().any(|(d, _)| *d == id));
    }
}
