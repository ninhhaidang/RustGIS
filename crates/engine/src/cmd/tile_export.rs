//! Export Raster of a tile layer (#1561): what a WMS, WMTS or XYZ layer (or a raster tile
//! archive) shows, as a GeoTIFF placed on the ground. The layer's own tiles at one zoom are put
//! together on their Web Mercator grid, a pixel of the file for a pixel of a tile, without
//! warping, alone (no other layer, no basemap) and with their alpha.

use std::time::Duration;

use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster};
use rustgis_geom::{Coord, Extent};
use rustgis_map::tiles::{TileKind, TileLayer};
use rustgis_render::rustgis_tiles::{self, Frame, cache, source, style, template::HALF_WORLD};
use serde_json::{Value, json};

use super::exports::{Work, Wrote};
use super::ps;
use crate::Session;

/// The most tiles an export fetches. A box at a high zoom is thousands of requests, and many
/// tile services' terms forbid bulk downloads (OpenStreetMap's tile usage policy, for one).
pub(crate) const MOST_TILES: u64 = 1024;

/// The most pixels a side the file holds: four bands of 32-bit samples are held while it is
/// written, 1 GiB at this size.
const MOST_SIDE: u64 = rustgis_render::MAX_SIDE as u64;

/// The side of the blocks the picture is drawn in, each with at most a few dozen tiles of 256
/// pixels ([`block_side`] makes it smaller for smaller tiles).
const BLOCK: u32 = 2048;

/// The side of the blocks for `tile_size`-pixel tiles: a block touches at most 20 tiles a side,
/// 400 in all, the most a drawing places ([`rustgis_tiles::template::MAX_TILES`]); more would be
/// left out.
fn block_side(tile_size: u32) -> u32 {
    BLOCK.min(tile_size.max(1).saturating_mul(19))
}

/// How long a block waits for its tiles, and the layer's description (a TileJSON, an archive's
/// header) for its tile grid.
const WAIT: Duration = Duration::from_secs(60);

/// The most different drawing errors an export names in full.
const SHOWN_ERRORS: usize = 3;

/// Waits for tiles in a row that bring nothing new before a block is written with what came.
const IDLE_PASSES: u32 = 3;

/// The tile layer `share.exportRaster` writes, if it writes one: the one `tiles` names (by name,
/// or index from the top), else the one picked in the Layers pane while no map layer is
/// active and no `layer` is given.
pub(crate) fn target(input: &impl super::InputSource, p: &Value) -> Result<Option<usize>, String> {
    let context = input.input_context();
    let tiles = &context.map().tiles;
    match p.get("tiles") {
        None | Some(Value::Null) => {}
        Some(Value::String(n)) => {
            return tiles
                .iter()
                .position(|t| t.name.eq_ignore_ascii_case(n.trim()))
                .map(Some)
                .ok_or_else(|| format!("no tile layer '{n}'"));
        }
        Some(v) => {
            let i = v.as_u64().and_then(|i| usize::try_from(i).ok()).filter(|i| *i < tiles.len());
            return i.map(Some).ok_or_else(|| "tiles is a tile layer's name or its index in the Layers pane (0 = top)".to_string());
        }
    }
    let unnamed = matches!(p.get("layer"), None | Some(Value::Null));
    Ok(if unnamed && context.active_layer.is_none() { tiles.iter().position(|t| t.selected) } else { None })
}

/// Whether `p` names its tile layer (`tiles`), rather than taking the one picked.
pub(crate) fn named(p: &Value) -> bool {
    p.get("tiles").is_some_and(|t| !t.is_null())
}

/// `share.exportRaster` of tile layer `i`: reads what it needs of the session (the layer, the
/// box, the zoom the view shows) and returns the work that fetches the tiles and writes the
/// file.
pub(crate) fn export_work(s: &Session, p: &Value, i: usize) -> Result<Work, String> {
    // The browser can't wait for tiles on a thread of its own.
    if cfg!(target_arch = "wasm32") {
        return Err("a tile layer is exported as a raster in the desktop app: here, Export Map writes the view".into());
    }
    let path = ps(p, "path").ok_or("path is required")?.to_string();
    let t = s.map().tiles.get(i).ok_or("no tile layer at that index")?.clone();
    // One layer is written: a raster named beside it would be dropped.
    if !matches!(p.get("layer"), None | Some(Value::Null)) {
        return Err(format!("{}: name one layer: \"tiles\" for a tile layer, or \"layer\" for a raster", t.name));
    }
    if p.get("band").is_some_and(|b| !b.is_null()) {
        return Err(format!("{}: a tile layer is written in colour; band is for a raster layer", t.name));
    }
    let cog = match p.get("cog") {
        None | Some(Value::Null) => false,
        Some(v) => v.as_bool().ok_or("cog must be true or false")?,
    };
    if t.kind == TileKind::Cog {
        return Err(format!("{}: a cloud-optimised GeoTIFF is already a raster: add it as a layer (Add Data) and export that", t.name));
    }
    if let Some(e) = &t.error {
        return Err(format!("{}: {e}", t.name));
    }
    if t.time_hidden {
        return Err(format!("{}: none of its dates is in the time slider's span, so it shows nothing to export", t.name));
    }
    let zoom = match p.get("zoom") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_u64().filter(|z| *z <= 30).ok_or("zoom is a whole number from 0 to 30")? as u8),
    };
    let area = match p.get("bbox") {
        None | Some(Value::Null) => {
            if s.map().projection.is_globe() {
                return Err("give a bbox [west, south, east, north]: the globe shows no box to export".into());
            }
            let e = super::workspace::view_extent(s);
            if e.is_empty() {
                return Err("the view shows no part of the Earth: give a bbox [west, south, east, north]".into());
            }
            e
        }
        Some(v) => bbox(v)?,
    };
    // The resolution the view shows, as Web Mercator metres a pixel at its centre: the zoom of
    // tiles the map draws there, when none is asked.
    let shown = super::navigate::view_of(s.map()).map(|(_, z)| 2.0 * HALF_WORLD / (512.0 * 2f64.powf(z)));
    if zoom.is_none() && shown.is_none() {
        return Err("the map's centre is no place on Earth: give a zoom".into());
    }
    Ok(Box::new(move || write(&t, &path, area, zoom, shown, cog)))
}

/// A `bbox` of `[west, south, east, north]` in degrees.
fn bbox(v: &Value) -> Result<Extent, String> {
    // Every element a number: one that isn't is refused, not skipped.
    let n: Vec<f64> = v.as_array().and_then(|a| a.iter().map(Value::as_f64).collect()).unwrap_or_default();
    let [w, s, e, n] = n[..] else { return Err("bbox is [west, south, east, north] in degrees".into()) };
    if !(w < e && s < n && (-90.0..=90.0).contains(&s) && (-90.0..=90.0).contains(&n) && w >= -540.0 && e <= 540.0) {
        return Err("bbox is [west, south, east, north] in degrees, west below east and south below north".into());
    }
    // Its west side within −180..180°, the box moved by whole turns.
    let turn = ((w + 180.0) / 360.0).floor() * 360.0;
    Ok(Extent::new(w - turn, s, e - turn, n))
}

/// The tile grid a layer's tiles are on: their pixels a side and the zooms they come at, and
/// their addresses (templates the tiles' drawing fills in).
#[derive(Clone, Debug, PartialEq)]
struct Grid {
    tile_size: u32,
    min_zoom: u8,
    max_zoom: u8,
    tiles: Vec<String>,
}

/// The tile grid of a resolved layer that draws raster tiles only; why it can't be exported
/// otherwise.
fn grid_of(name: &str, r: &source::Resolved) -> Result<Grid, String> {
    let shown: Vec<&style::StyleLayer> = r.style.layers.iter().filter(|l| l.visible && l.kind != style::LayerKind::Other).collect();
    if shown.iter().any(|l| l.kind != style::LayerKind::Raster) {
        return Err(format!(
            "{name} draws vector tiles (shapes and labels in a style), not pictures: Export Map (share.exportMap) to a .tif writes what the map draws"
        ));
    }
    // One source: the tiles of several, each on its own grid, aren't one grid of pixels.
    let mut ids: Vec<&String> = shown.iter().filter_map(|l| l.source.as_ref()).collect();
    ids.sort();
    ids.dedup();
    if ids.len() > 1 {
        return Err(format!(
            "{name} draws tiles from {} sources: Export Raster writes one source's tiles; Export Map (share.exportMap) to a .tif writes what the map draws",
            ids.len()
        ));
    }
    ids.first()
        .and_then(|id| r.style.sources.iter().find(|(s, _)| s == *id))
        .and_then(|(_, s)| match s {
            style::Source::Raster { tiles, tile_size, min_zoom, max_zoom, .. } => {
                Some(Grid { tile_size: (*tile_size).max(1), min_zoom: *min_zoom, max_zoom: *max_zoom, tiles: tiles.clone() })
            }
            _ => None,
        })
        .ok_or_else(|| format!("{name} has no raster tiles to export"))
}

/// The box to write: `area` (degrees) on the pixel grid of `zoom`'s tiles, whole pixels out to
/// its edges and within the world's rows; its corner (Web Mercator) and size in pixels.
fn placed(area: &Extent, res: f64) -> (f64, f64, u64, u64) {
    let lo = rustgis_geom::lonlat_to_merc(Coord::new(area.min_x, area.min_y));
    let hi = rustgis_geom::lonlat_to_merc(Coord::new(area.max_x, area.max_y));
    let col = |x: f64, up: bool| {
        let c = (x + HALF_WORLD) / res;
        if up { c.ceil() } else { c.floor() }
    };
    let row = |y: f64, up: bool| {
        let r = ((HALF_WORLD - y) / res).clamp(0.0, 2.0 * HALF_WORLD / res);
        if up { r.ceil() } else { r.floor() }
    };
    let (c0, c1) = (col(lo.x, false), col(hi.x, true));
    // At most one world wide.
    let c1 = c1.min(c0 + (2.0 * HALF_WORLD / res).round());
    let (r0, r1) = (row(hi.y, false), row(lo.y, true));
    let left = c0 * res - HALF_WORLD;
    let top = HALF_WORLD - r0 * res;
    (left, top, (c1 - c0).max(0.0) as u64, (r1 - r0).max(0.0) as u64)
}

/// The tiles at zoom `z` that a `w × h` box at (`left`, `top`) of `res` metres a pixel touches.
fn tiles_over(left: f64, top: f64, w: u64, h: u64, res: f64, z: u8) -> u64 {
    let span = 2.0 * HALF_WORLD / 2f64.powi(i32::from(z));
    let eps = res * 1e-6;
    let cols = ((left + w as f64 * res - eps + HALF_WORLD) / span).floor() - ((left + eps + HALF_WORLD) / span).floor() + 1.0;
    let rows = ((HALF_WORLD - top + h as f64 * res - eps) / span).floor() - ((HALF_WORLD - top + eps) / span).floor() + 1.0;
    (cols.max(0.0) * rows.max(0.0)) as u64
}

/// The first zoom whose tiles the layer draws: its first, unless its tiles are smaller than the
/// 512 pixels a map's zoom counts in, as the tiles' drawing checks that zoom against the first
/// (256-pixel tiles of zoom 10 are drawn at the map's zoom 9: from zoom 11).
fn first_zoom(g: &Grid) -> u8 {
    (g.min_zoom..=g.max_zoom.max(g.min_zoom))
        .find(|z| f64::from(rustgis_tiles::template::style_zoom(res_at(*z, g.tile_size))) + 0.5 >= f64::from(g.min_zoom))
        .unwrap_or(g.min_zoom)
}

/// The tiles at zoom `z` over a `w × h` block at `frame` that failed (fetched, or read, without
/// a picture), as the cache keeps them: each address with its error. A tile the server hasn't (a 404 past its
/// coverage) is one too: on the ground its gap passes for data.
fn failed_tiles(templates: &[String], frame: &Frame, w: u32, h: u32, z: u8) -> Vec<(String, String)> {
    if templates.is_empty() {
        return Vec::new();
    }
    let (right, bottom) = (frame.left + f64::from(w) * frame.m_per_px, frame.top - f64::from(h) * frame.m_per_px);
    let view = Extent::new(frame.left, bottom, right, frame.top);
    // Each tile once: at the world's edge a tile is placed again a world over.
    let mut urls: Vec<String> = rustgis_tiles::template::covering(&view, z)
        .into_iter()
        .filter_map(|p| {
            let i = (p.id.x as usize).wrapping_add(p.id.y as usize) % templates.len();
            Some(rustgis_tiles::template::expand(templates.get(i)?, p.id, 256, false, &[]))
        })
        .collect();
    urls.sort();
    urls.dedup();
    urls.into_iter()
        // Only what the drawing asked for: a tile it didn't (a style layer out of its zooms) isn't
        // fetched now.
        .filter_map(|url| cache::peek_failure(&url).map(|e| (url, e)))
        .collect()
}

/// Metres a pixel of zoom `z`'s `tile_size`-pixel tiles.
fn res_at(z: u8, tile_size: u32) -> f64 {
    2.0 * HALF_WORLD / (f64::from(tile_size) * 2f64.powi(i32::from(z)))
}

/// The layer's description resolved: its tile grid, or why it has none. Waits while it is on
/// its way (an archive's header, a TileJSON document), as long as [`WAIT`].
fn resolved(spec: &rustgis_tiles::Spec, name: &str) -> Result<std::sync::Arc<source::Resolved>, String> {
    let start = web_time::Instant::now();
    loop {
        let since = cache::generation();
        match source::resolve(spec) {
            source::Resolve::Ready(r) => return Ok(r),
            source::Resolve::Failed(e) => return Err(format!("{name}: {e}")),
            source::Resolve::Pending => {}
        }
        rustgis_data::progress::check().map_err(|e| e.to_string())?;
        let left = WAIT.saturating_sub(start.elapsed());
        if left.is_zero() {
            return Err(format!("{name}: its description didn't come within {} s: export again later", WAIT.as_secs()));
        }
        cache::wait_for_news_unless(since, left.min(Duration::from_secs(5)), || rustgis_data::progress::check().is_err());
        if cache::generation() == since && cache::in_flight() == 0 {
            // Nothing on its way and nothing new: ask again after a moment.
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// Draws block `frame` (`w × h` pixels) of the layer, waiting for its tiles; what it drew, its
/// credits, and why it may have gaps.
fn block(spec: &rustgis_tiles::Spec, frame: &Frame, w: u32, h: u32) -> Result<(tiny_skia::Pixmap, rustgis_tiles::Drawn), String> {
    let mut pm = tiny_skia::Pixmap::new(w, h).ok_or("could not allocate the image")?;
    let start = web_time::Instant::now();
    let cancelled = || rustgis_data::progress::check().is_err();
    let mut idle = 0;
    loop {
        let since = cache::generation();
        pm.fill(tiny_skia::Color::TRANSPARENT);
        let d = rustgis_tiles::draw(&mut pm, spec, 1.0, frame, false);
        let left = WAIT.saturating_sub(start.elapsed());
        // Waits that bring nothing new end it: a tile that failed is tried again later, not now.
        if !d.pending || left.is_zero() || idle >= IDLE_PASSES || cancelled() {
            rustgis_data::progress::check().map_err(|e| e.to_string())?;
            return Ok((pm, d));
        }
        cache::wait_for_news_unless(since, left.min(Duration::from_secs(30)), cancelled);
        if cache::generation() == since {
            idle += 1;
        } else {
            idle = 0;
        }
        // Nothing on its way: news then is other layers' (a retry's alarm), not this block's.
        if cache::in_flight() == 0 {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// Fetches tile layer `t`'s tiles over `area` (degrees) at `zoom` (else the zoom of its tiles
/// the view shows, at `shown` metres a pixel), and writes them to `path` as an RGBA GeoTIFF in
/// Web Mercator (a cloud-optimised one with `cog`).
fn write(t: &TileLayer, path: &str, area: Extent, zoom: Option<u8>, shown: Option<f64>, cog: bool) -> Result<Wrote, String> {
    let name = t.name.as_str();
    let spec = rustgis_render::tile_spec(t);
    let r = resolved(&spec, name)?;
    let g = grid_of(name, &r)?;
    let z = match zoom {
        Some(z) => z,
        None => rustgis_tiles::template::zoom_for(shown.unwrap_or(f64::NAN), g.tile_size, g.max_zoom),
    };
    let first = first_zoom(&g);
    if z < first {
        return Err(format!("{name} is drawn from zoom {first}: give a zoom of at least {first}"));
    }
    let res = res_at(z, g.tile_size);
    let (left, top, w, h) = placed(&area, res);
    if w == 0 || h == 0 {
        return Err(format!("{name}: the box holds no pixel at zoom {z}"));
    }
    // A zoom past the service's takes its deepest tiles, each drawn larger.
    let fetched = z.min(g.max_zoom);
    let n = tiles_over(left, top, w, h, res, fetched);
    let fits = |z: u8| {
        let res = res_at(z, g.tile_size);
        let (l, t, w, h) = placed(&area, res);
        w <= MOST_SIDE && h <= MOST_SIDE && tiles_over(l, t, w, h, res, z.min(g.max_zoom)) <= MOST_TILES
    };
    let lower = || (first..z).rev().find(|z| fits(*z)).map_or_else(String::new, |z| format!(" (zoom {z} fits)"));
    if n > MOST_TILES {
        return Err(format!(
            "{name}: the box at zoom {z} takes {n} tiles, more than the {MOST_TILES} an export fetches, as tile services' terms forbid bulk downloads (OpenStreetMap's tile usage policy, for one): give a smaller bbox or a lower zoom{}",
            lower()
        ));
    }
    if w > MOST_SIDE || h > MOST_SIDE {
        return Err(format!(
            "{name}: the box at zoom {z} is {w} × {h} pixels, more than the {MOST_SIDE} a side an export holds: give a smaller bbox or a lower zoom{}",
            lower()
        ));
    }
    let (w, h) = (w as u32, h as u32);
    let mut bands: Vec<Vec<f32>> = (0..4)
        .map(|_| rustgis_data::raster::filled::<f32>(w as usize, h as usize, 0.0))
        .collect::<Result<_, _>>()
        .map_err(|e| format!("{name}: {e}"))?;
    let side = block_side(g.tile_size);
    let (bx, by) = (w.div_ceil(side), h.div_ceil(side));
    rustgis_data::progress::begin((bx * by) as usize);
    let (mut credits, mut errors, mut drew) = (Vec::<String>::new(), Vec::<String>::new(), false);
    // The blocks with tiles still on their way when the wait ended, and those with tiles that
    // couldn't be drawn.
    let (mut pending, mut failed, mut lost) = (0u32, 0u32, Vec::<String>::new());
    for j in 0..by {
        for i in 0..bx {
            let (x0, y0) = (i * side, j * side);
            let (bw, bh) = (side.min(w - x0), side.min(h - y0));
            let frame = Frame { left: left + f64::from(x0) * res, top: top - f64::from(y0) * res, m_per_px: res, ppp: 1.0 };
            let (pm, d) = block(&spec, &frame, bw, bh)?;
            for c in d.attribution {
                if !credits.contains(&c) {
                    credits.push(c);
                }
            }
            pending += u32::from(d.pending);
            // The drawing leaves out a tile that failed without saying so: the cache does.
            let missing = failed_tiles(&g.tiles, &frame, bw, bh, fetched);
            if d.error.is_some() || !missing.is_empty() {
                failed += 1;
            }
            // A tile on the edge of two blocks is in both: counted once.
            let mut missing_errors = Vec::new();
            for (url, e) in missing {
                if !lost.contains(&url) {
                    lost.push(url);
                }
                missing_errors.push(e);
            }
            for e in d.error.into_iter().chain(missing_errors) {
                if !errors.contains(&e) {
                    errors.push(e);
                }
            }
            for (k, px) in pm.pixels().iter().enumerate() {
                let (col, row) = (x0 as usize + k % bw as usize, y0 as usize + k / bw as usize);
                let at = row * w as usize + col;
                // tiny-skia keeps colours premultiplied; a file keeps them straight.
                let c = px.demultiply();
                drew |= c.alpha() > 0;
                for (band, v) in bands.iter_mut().zip([c.red(), c.green(), c.blue(), c.alpha()]) {
                    band[at] = f32::from(v);
                }
            }
            rustgis_data::progress::step(1).map_err(|e| e.to_string())?;
        }
    }
    let transform = GeoTransform { origin_x: left, origin_y: top, pixel_width: res, pixel_height: -res };
    let color = ColorHint::Rgb { alpha: true };
    let mut raster =
        Raster::new(name, w, h, bands, transform, Crs::WebMercator, None, color, "UInt8").map_err(|e| format!("{name}: {e}"))?;
    raster.copyright = (!credits.is_empty()).then(|| credits.join(" | "));
    let mut wrote = super::raster::write_geotiff(&raster, 0, cog, path)?;
    // What it holds, after what Export Raster says of every raster.
    let mut said = format!("{path}: {name} at zoom {z}, {w} × {h} pixels in Web Mercator (EPSG:3857), from {n} tile(s)");
    if z > g.max_zoom {
        said.push_str(&format!("; the service has tiles to zoom {} only, so they are enlarged", g.max_zoom));
    }
    wrote.notes.insert(0, said);
    if let Some(o) = wrote.answer.as_object_mut() {
        o.insert("width".into(), json!(w));
        o.insert("height".into(), json!(h));
        o.insert("zoom".into(), json!(z));
        o.insert("tiles".into(), json!(n));
        o.insert("crs".into(), json!(Crs::WebMercator.label()));
        if let Some(c) = &raster.copyright {
            o.insert("copyright".into(), json!(c));
        }
    }
    // On the ground a gap passes for data: said, as a warning.
    let mut gaps = Vec::new();
    let blocks = bx * by;
    if pending > 0 {
        gaps.push(format!(
            "{path}: in {pending} of its {blocks} block(s) of up to {side} × {side} pixels, some of {name}'s tiles hadn't come after the wait, so parts of it are empty: export again later"
        ));
    }
    if !drew && pending == 0 && errors.is_empty() {
        gaps.push(format!("{path}: {name} drew nothing in the box at zoom {z}: its service may have no tiles there"));
    }
    if !errors.is_empty() {
        // Each different error, a few in full.
        let shown: Vec<&str> = errors.iter().take(SHOWN_ERRORS).map(String::as_str).collect();
        let more = errors.len().saturating_sub(SHOWN_ERRORS);
        let more = if more > 0 { format!(" (and {more} more)") } else { String::new() };
        let tiles = if lost.is_empty() { String::new() } else { format!(" ({} tile(s) couldn't be fetched or read)", lost.len()) };
        gaps.push(format!(
            "{path}: in {failed} of its {blocks} block(s), {name} couldn't be drawn whole{tiles}, so parts of it are empty; {} different error(s): {}{more}",
            errors.len(),
            shown.join(" | ")
        ));
    }
    if !gaps.is_empty() {
        wrote.then = Some(Box::new(move |s: &mut Session, _: &mut Value| {
            for g in gaps {
                s.warn(g);
            }
            Ok(())
        }));
    }
    Ok(wrote)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A box is put on the zoom's pixel grid, whole pixels out to its edges, within the world's
    /// rows and at most one world wide.
    #[test]
    fn a_box_is_placed_on_the_tiles_pixel_grid() {
        let res = res_at(1, 256);
        // The world at zoom 1: 512 pixels a side.
        let (l, t, w, h) = placed(&Extent::new(-180.0, -85.06, 180.0, 85.06), res);
        assert_eq!((w, h), (512, 512));
        assert!((l + HALF_WORLD).abs() < 1e-6 && (t - HALF_WORLD).abs() < 1e-6);
        assert_eq!(tiles_over(l, t, w, h, res, 1), 4);
        // Wider than the world: one world.
        assert_eq!(placed(&Extent::new(-300.0, -10.0, 300.0, 10.0), res).2, 512);
        // A small box takes the pixels it touches.
        let (l, _, w, _) = placed(&Extent::new(0.1, 0.1, 0.2, 0.2), res_at(2, 256));
        assert_eq!(w, 1);
        assert!(l >= 0.0);
        assert_eq!(bbox(&json!([1, 2, 3])).unwrap_err(), "bbox is [west, south, east, north] in degrees");
        assert!(bbox(&json!([10, 0, 5, 1])).is_err());
        // A box past the antimeridian is moved into the world by whole turns.
        let e = bbox(&json!([190, 0, 200, 1])).unwrap();
        assert_eq!((e.min_x, e.max_x), (-170.0, -160.0));
    }

    /// 256-pixel tiles are drawn from the zoom after their first (the drawing counts the map's
    /// 512-pixel zoom against it); 512-pixel ones from their first.
    #[test]
    fn the_first_zoom_is_the_first_drawn() {
        let g = |tile_size, min_zoom| Grid { tile_size, min_zoom, max_zoom: 18, tiles: Vec::new() };
        assert_eq!(first_zoom(&g(256, 10)), 11);
        assert_eq!(first_zoom(&g(512, 10)), 10);
        assert_eq!(first_zoom(&g(256, 0)), 0);
        // Blocks touch at most 400 tiles, however small the tiles.
        assert_eq!((block_side(256), block_side(64), block_side(0)), (2048, 1216, 19));
    }
}
