//! Satellite embeddings (Home › Add › Web Data › Satellite Embeddings): the catalog of embedding
//! datasets, a search for a dataset's tiles where the map is, and a tile shown on the map —
//! AlphaEarth's as a cloud-optimised GeoTIFF of three bands, Earth Index's as points coloured by
//! their vectors' principal components, Tessera's as a raster of three bands of the view.
//! AlphaEarth's vectors compared, as rasters: each cell's similarity to a reference, and how far
//! it moved between two years (#1720).
//!
//! On the desktop and the command line these commands read what they need, waiting for it; the
//! web app reads it off the frame and hands it over (`ran`).

use std::collections::BTreeMap;

use rustgis_data::raster;
use rustgis_map::raster::RasterLayer;
use serde_json::{Value, json};

use super::{f, ps};
use crate::render::rustgis_tiles::embeddings::{self as emb, tessera};
use crate::{CmdResult, CommandSpec as C, Session};

pub fn commands() -> Vec<C> {
    vec![
        C::new("embeddings.datasets", "Satellite Embedding Datasets", "Home › Add › Web Data", |_, _| {
            Ok(json!({"datasets": emb::DATASETS.iter().map(|d| d.to_json()).collect::<Vec<_>>()}))
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .view(),
        // Not a view: it adds the footprints layer (by default).
        C::new("embeddings.search", "Search Satellite Embeddings", "Home › Add › Web Data", search).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network)).params(
            r#"{"dataset": "alphaearth" | "earth-index" | "tessera" | "olmoearth", "year"?: number (AlphaEarth, Tessera and OlmoEarth: 2017–2025, default 2024), "bbox"?: [west, south, east, north] | "view" (the map's extent, the default), "footprints"?: bool (adds the tiles' footprints as a layer, default true)}"#,
        ),
        C::new("embeddings.add", "Add Satellite Embeddings", "Home › Add › Web Data", add).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network)).params(
            r#"{"dataset": "alphaearth" | "earth-index" | "tessera" | "olmoearth", "url"?: "AlphaEarth's tile or Earth Index's file, as embeddings.search lists it (Earth Index: default the MGRS tile at the view's centre)", "year"?: number (Tessera and OlmoEarth, default 2024), "bands"?: [red, green, blue] (bands from 1; AlphaEarth default [2, 17, 10], Tessera and OlmoEarth [1, 2, 3]), "stretch"?: number (AlphaEarth: dequantised values shown over ±stretch, default 0.3), "dimension"?: number (Earth Index: a vector value kept as each point's `value`, from 1), "bbox"?: [west, south, east, north] | "view" (Earth Index, Tessera and OlmoEarth: the area read, default the map's extent), "name"?: text, "ran"?: (internal: what the app read)}"#,
        ),
        C::new("embeddings.addCube", "Add Satellite Embeddings over Time", "Home › Add › Web Data", add_cube).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network)).params(
            r#"{"dataset": "alphaearth", "bbox"?: [west, south, east, north] | "view" (the area searched, default the map's extent), "start"?: year (default 2017), "end"?: year (default 2025), "bands"?: [red, green, blue] (bands from 1, default [2, 17, 10]), "stretch"?: number (dequantised values shown over ±stretch, default 0.3), "name"?: text, "found"?: (internal: the tiles the app found, each {years: {year: address}}), "complete"?: (internal: false when the app's search read only part of the index)} → a tile layer per AlphaEarth tile, a file per year, following the time slider (turned on over the years when it is off)"#,
        ),
        // Each makes a raster; on another thread from the app (workflows::IN_BACKGROUND).
        C::new("embeddings.similarity", "Satellite Embedding Similarity", "Home › Add › Web Data", similarity).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network)).params(
            r#"{"dataset": "alphaearth", "point"?: [longitude, latitude] (the reference: the embedding of the cell there), "points"?: [[longitude, latitude], …] (the reference: the mean of their cells' embeddings, at most 1000; give point or points, inside the area), "year"?: number (2017–2025, default 2024), "bbox"?: [west, south, east, north] | "view" (the area read, default the map's extent; at most about 1000 × 1000 cells), "cellSize"?: metres (an overview with cells at most this size, for a larger area), "name"?: text, "ran"?: (internal: a raster the app has worked out)} → a raster of each cell's cosine similarity to the reference's embedding, from −1 to 1, in the tile's coordinate system (UTM)"#,
        ),
        C::new("embeddings.change", "Satellite Embedding Change", "Home › Add › Web Data", change).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network)).params(
            r#"{"dataset": "alphaearth", "from"?: year (default 2017), "to"?: year after from (default 2025), "bbox"?: [west, south, east, north] | "view" (the area read, default the map's extent; at most about 1000 × 1000 cells), "cellSize"?: metres (an overview with cells at most this size, for a larger area), "name"?: text, "ran"?: (internal: a raster the app has worked out)} → a raster of how far each cell's embedding moved between the two years, 1 − their cosine, from 0 (the same) to 2 (opposite), in the tile's coordinate system (UTM)"#,
        ),
    ]
}

/// The box a command names, the map's extent by default.
fn bbox(s: &Session, p: &Value) -> Result<[f64; 4], String> {
    match p.get("bbox") {
        Some(Value::Array(a)) => {
            let n: Option<Vec<f64>> = a.iter().map(Value::as_f64).collect();
            match n.as_deref().unwrap_or_default() {
                [w, south, e, n] => emb::check_bbox([*w, *south, *e, *n]),
                _ => Err("bbox is [west, south, east, north], or \"view\"".into()),
            }
        }
        Some(Value::String(v)) if v != "view" => Err("bbox is [west, south, east, north], or \"view\"".into()),
        _ => {
            let e = super::workspace::view_extent(s);
            if e.is_empty() {
                return Err("the map's extent isn't known; give bbox".into());
            }
            emb::check_bbox([e.min_x, e.min_y, e.max_x, e.max_y])
        }
    }
}

/// The searchable dataset a command names.
fn dataset(p: &Value) -> Result<&'static emb::EmbeddingDataset, String> {
    let id = ps(p, "dataset").map(str::trim).ok_or("dataset is required (alphaearth, earth-index, tessera or olmoearth)")?;
    let d = emb::dataset(id).ok_or_else(|| format!("unknown dataset '{id}' (embeddings.datasets lists them)"))?;
    if !d.search {
        return Err(format!("{} is listed for its links only: RustGIS doesn't read it", d.name));
    }
    Ok(d)
}

/// The year a command names, within the dataset's.
fn year(d: &emb::EmbeddingDataset, p: &Value) -> Result<i32, String> {
    let (first, last) = d.years.unwrap_or((2024, 2024));
    let y = match f(p, "year") {
        Some(y) if y.fract() == 0.0 && y >= f64::from(first) && y <= f64::from(last) => y as i32,
        Some(_) => return Err(format!("{} has the years {first}–{last}", d.name)),
        None => 2024.clamp(first, last),
    };
    Ok(y)
}

/// Three bands from 1 a command names (0-based), each below `max`.
fn bands(p: &Value, max: usize, default: [usize; 3]) -> Result<[usize; 3], String> {
    match p.get("bands") {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Array(a)) => {
            let b: Option<Vec<usize>> =
                a.iter().map(|v| v.as_u64().filter(|b| (1..=max as u64).contains(b)).map(|b| b as usize - 1)).collect();
            match b.as_deref() {
                Some([r, g, b]) => Ok([*r, *g, *b]),
                _ => Err(format!("bands is [red, green, blue], each from 1 to {max}")),
            }
        }
        Some(_) => Err(format!("bands is [red, green, blue], each from 1 to {max}")),
    }
}

/// A band list's label, from 1.
fn band_label(b: [usize; 3]) -> String {
    b.iter().map(|b| (b + 1).to_string()).collect::<Vec<_>>().join(", ")
}

fn search(s: &mut Session, p: &Value) -> CmdResult {
    let d = dataset(p)?;
    let year = year(d, p)?;
    let area = bbox(s, p)?;
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (year, area);
        Err("in the web app, search with Home › Add › Web Data › Satellite Embeddings".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let (tiles, complete) = match d.id {
            "alphaearth" => emb::search_aef_blocking(area, year)?,
            "earth-index" => emb::search_earth_index_blocking(area)?,
            _ => tessera::tiles(tessera::store(d.id).ok_or_else(|| format!("{} can't be searched", d.name))?, area, year),
        };
        let mut r = json!({"dataset": d.id, "year": year, "count": tiles.len(), "complete": complete,
            "tiles": tiles.iter().map(emb::Tile::to_json).collect::<Vec<_>>()});
        if !complete {
            s.notify(format!("{}: the area holds more tiles than one search reads; zoom in to list them all", d.name), false);
        }
        if super::b(p, "footprints") != Some(false) && !tiles.is_empty() && tessera::store(d.id).is_none() {
            let name = format!("{} tiles {year}", d.name.split(" (").next().unwrap_or(d.name));
            if let Some(id) = super::stac::add_collection(s, &emb::footprints_geojson(&tiles), &name)? {
                r["layer"] = json!(id);
            }
        }
        Ok(r)
    }
}

fn add(s: &mut Session, p: &Value) -> CmdResult {
    let d = dataset(p)?;
    let name = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).map(str::to_string);
    match d.id {
        "alphaearth" => {
            let url = ps(p, "url")
                .map(str::trim)
                .filter(|u| u.starts_with("https://"))
                .ok_or("url is required: an AlphaEarth tile (embeddings.search lists them)")?;
            let b = bands(p, 64, emb::AEF_DEFAULT_BANDS)?;
            let look = emb::aef_look(b, stretch(p)?);
            let file = crate::render::rustgis_tiles::cog::split(url).0;
            // The year from the tile's address (…/annual/2024/10N/…), when it has one.
            let year = file.split('/').find(|s| s.len() == 4 && s.parse::<i32>().is_ok());
            let name = name.unwrap_or_else(|| {
                let title = year.map(|y| format!("AlphaEarth {y}")).unwrap_or_else(|| "AlphaEarth".into());
                format!("{title} ({})", band_label(b))
            });
            // Its header read first, so its bands are known; the layer is framed only when it is the
            // map's first content (no "zoom" is passed, so add_cog defaults it to that). In the
            // browser nothing may wait, nor may a plugin's command in the app (it runs on the
            // frame): it is read as the map draws.
            let probe = cfg!(not(target_arch = "wasm32")) && !s.leave_downloads;
            #[cfg(not(target_arch = "wasm32"))]
            if probe && !crate::render::rustgis_tiles::cog::probe_blocking(file)? {
                return Err(format!("{file}: not a cloud-optimised GeoTIFF"));
            }
            let look = crate::render::rustgis_tiles::cog::with_look(file, &look);
            super::layer::add_cog(s, &json!({"name": name}), &look, probe)
        }
        "earth-index" => add_points(s, p, name),
        _ => add_zones(s, p, d, name),
    }
}

/// AlphaEarth's stretch a command names: dequantised values shown over ±stretch. Only a stretch
/// left out (or null) takes the default: one given that isn't a number is refused.
fn stretch(p: &Value) -> Result<f32, String> {
    let stretch = match p.get("stretch").filter(|v| !v.is_null()) {
        None => emb::AEF_DEFAULT_STRETCH,
        Some(_) => f(p, "stretch").map_or(f32::NAN, |v| v as f32),
    };
    if !(stretch.is_finite() && stretch > 0.0 && stretch <= 1.0) {
        return Err("stretch is a number above 0, at most 1".into());
    }
    Ok(stretch)
}

/// `embeddings.addCube` (#1720): AlphaEarth's tiles where the map is as a data cube, a year a step
/// of the time slider. Each tile is a tile layer reading its file of the year the slider is at
/// (`time_urls`, as `stac.addCube` makes them); a year a tile has no file for shows nothing, said.
/// The tiles come from one read of the index, which holds every year (the command line waits for
/// it), or from the app's search off the frame (`found`). Without `found`, natively, only the
/// first tile's latest year's header is read before the layers are added, as `embeddings.add`
/// reads one tile's; with `found` (the pane, on the frame) no header is read. The rest are read in
/// place as the map draws them.
///
/// Without `found` the command runs on its caller's thread and waits there for the index and that
/// header, as `embeddings.search` and `embeddings.add` do, so that the caller finds the layers
/// there when the command returns. The command line and Python have no window to hold. The app's
/// own routes (ribbon, command search) open the pane, which searches off the frame and hands its
/// tiles over (`found`); a plugin's command in the app (`leave_downloads`) is refused rather than
/// wait on the frame. The app's assistant runs its tools on the frame without `leave_downloads`
/// (see [`Session::run_leaving_downloads`]), so there it waits on the frame, as it does for every
/// command that waits for the network (`embeddings.search`, `layer.addUrl`); moving the
/// assistant's tools off the frame is a change for all of them, not for this one.
fn add_cube(s: &mut Session, p: &Value) -> CmdResult {
    use crate::render::rustgis_tiles::cog::{self, Look};
    let d = dataset(p)?;
    if d.id != "alphaearth" {
        return Err(format!("{}: only AlphaEarth's years make a time cube here", d.name));
    }
    let (first, last) = d.years.unwrap_or((2017, 2025));
    let (start, end) = (year_param(d, p, "start", first)?, year_param(d, p, "end", last)?);
    if start > end {
        return Err("start is a year before end, or the same one".into());
    }
    let b = bands(p, 64, emb::AEF_DEFAULT_BANDS)?;
    let look = Look { cube: true, ..emb::aef_look(b, stretch(p)?) };
    let given = p.get("found").filter(|v| !v.is_null());
    // Whether the index was read whole: a tile-year with no file is then the index's gap, not
    // the read's. The files handed over outside the years asked for, left out. And what the
    // search left out, said once the cube is added (a refused one adds nothing to speak of).
    let (series, complete, outside, notes) = match given {
        Some(v) => {
            let (series, outside) = found(v, start, end)?;
            let complete = super::b(p, "complete") != Some(false);
            // A part read: said even when every year of the tiles handed over is there.
            let notes = if complete { Vec::new() } else { vec![PART_READ.to_string()] };
            (series, complete, outside, notes)
        }
        None => {
            let (series, complete, notes) = search_series(s, p, (start, end))?;
            (series, complete, 0, notes)
        }
    };
    let listed = series.len();
    let series: Vec<BTreeMap<i32, String>> = series.into_iter().filter(|y| !y.is_empty()).collect();
    if series.is_empty() {
        return Err(format!("AlphaEarth has no tiles there for {start}–{end}"));
    }
    let room = rustgis_map::tiles::MAX_TILE_LAYERS.saturating_sub(s.map().tiles.len());
    if series.len() > room {
        return Err(format!(
            "{} AlphaEarth tiles, and the map has room for {room} more tile layers (at most {}): take a smaller area",
            series.len(),
            rustgis_map::tiles::MAX_TILE_LAYERS
        ));
    }
    let base = ps(p, "name")
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map_or_else(|| format!("AlphaEarth {start}–{end} ({})", band_label(b)), str::to_string);
    // A cube that can't be added whole takes out what it added (as stac.addCube does).
    let (tiles_before, layers_before): (Vec<rustgis_map::TileLayer>, Vec<u32>) =
        (s.map().tiles.clone(), s.map().layers.iter().map(|l| l.id).collect());
    let undo = |s: &mut Session| {
        let m = s.map_mut();
        m.tiles = tiles_before.clone();
        m.layers.retain(|l| layers_before.contains(&l.id));
    };
    // In the browser nothing may wait, nor may the app (it hands its search over) or a plugin's
    // command in it (on the frame).
    let mut probe = cfg!(not(target_arch = "wasm32")) && !s.leave_downloads && given.is_none();
    let span = usize::try_from(end - start + 1).unwrap_or(1);
    // A tile's place goes in its layer's name when the cube has several, or when a tile layer
    // already has the name (the pane adds one tile at a time).
    let tagged = series.len() > 1 || s.map().tiles.iter().any(|t| t.name == base);
    let (mut dates, mut missing) = (Vec::new(), 0usize);
    for years in &series {
        missing += span.saturating_sub(years.len());
        let urls: Vec<(f64, String)> = years.iter().map(|(y, u)| (emb::year_start_ms(*y), cog::with_look(u, &look))).collect();
        let Some((_, latest)) = urls.last() else { continue };
        let file = cog::split(latest).0.to_string();
        #[cfg(not(target_arch = "wasm32"))]
        if probe {
            match cog::probe_blocking(&file) {
                Ok(true) => {}
                Ok(false) => {
                    undo(s);
                    return Err(format!("{file}: not a cloud-optimised GeoTIFF"));
                }
                Err(e) => {
                    undo(s);
                    return Err(e);
                }
            }
        }
        let name = match (tagged, emb::aef_place(&file)) {
            (true, Some(place)) => format!("{base} · {place}"),
            _ => base.clone(),
        };
        let r = match super::layer::add_cog(s, &json!({"name": name}), latest, probe) {
            Ok(r) => r,
            Err(e) => {
                undo(s);
                return Err(e);
            }
        };
        probe = false;
        let at = r.get("index").and_then(Value::as_u64).and_then(|i| usize::try_from(i).ok());
        match at.and_then(|i| s.map_mut().tiles.get_mut(i)).filter(|t| cog::split(&t.url).0 == file) {
            Some(t) => t.time_urls = urls.clone(),
            None => {
                undo(s);
                return Err(format!("{file}: not added as a tile layer that follows the time slider"));
            }
        }
        dates.extend(urls.iter().map(|(d, _)| *d));
    }
    if let Err(e) = super::stac::cube_slider(s, &mut dates, &json!({"unit": "year", "format": "YYYY"})) {
        undo(s);
        return Err(e);
    }
    for note in notes {
        s.notify(note, false);
    }
    if outside > 0 {
        s.notify(
            if outside == 1 {
                format!("{base}: 1 file handed over is outside {start}–{end} and was left out")
            } else {
                format!("{base}: {outside} files handed over are outside {start}–{end} and were left out")
            },
            false,
        );
    }
    if listed > series.len() {
        s.notify(format!("{base}: {} of the {listed} tiles have no file in {start}–{end} and were left out", listed - series.len()), false);
    }
    if missing > 0 {
        let total = series.len() * span;
        s.notify(
            if complete {
                format!("{base}: {missing} of the {total} tile-years have no file in AlphaEarth's index; in those years the tile shows nothing")
            } else {
                format!(
                    "{base}: {missing} of the {total} tile-years have no file in the part of AlphaEarth's index that was read (zoom in to read it whole); in those years the tile shows nothing"
                )
            },
            false,
        );
    }
    Ok(json!({"tiles": series.len(), "years": dates.len(), "missing": missing}))
}

/// The tiles the app found and hands over (`found`): each a year's file by year, kept to the
/// years asked for; and how many files were outside them (said by the caller).
fn found(v: &Value, start: i32, end: i32) -> Result<(Vec<BTreeMap<i32, String>>, usize), String> {
    let bad = || "found is a list of tiles, each {\"years\": {\"2017\": an https address, …}}".to_string();
    let list = v.as_array().ok_or_else(bad)?;
    if list.len() > emb::MAX_TILES {
        return Err(format!("found holds {} tiles, more than the {} a search lists", list.len(), emb::MAX_TILES));
    }
    let mut outside = 0usize;
    let series = list
        .iter()
        .map(|t| {
            let (mut out, mut seen) = (BTreeMap::new(), std::collections::BTreeSet::new());
            for (y, u) in t.get("years").and_then(Value::as_object).ok_or_else(bad)? {
                let y: i32 = y.trim().parse().map_err(|_| bad())?;
                // Two keys of one year (" 2017" and "2017"): one file would be lost, so refused.
                if !seen.insert(y) {
                    return Err(format!("found: a tile names the year {y} twice"));
                }
                let u = u.as_str().map(str::trim).filter(|u| u.starts_with("https://") && u.len() <= 4000).ok_or_else(bad)?;
                if (start..=end).contains(&y) {
                    out.insert(y, u.to_string());
                } else {
                    outside += 1;
                }
            }
            Ok(out)
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok((series, outside))
}

/// AlphaEarth's tiles over the years where the command says, from one read of the index, waiting
/// for it (the command line); whether it read the index whole for the area; and what the read
/// left out, to be said.
fn search_series(s: &mut Session, p: &Value, years: (i32, i32)) -> Result<(Vec<BTreeMap<i32, String>>, bool, Vec<String>), String> {
    let area = bbox(s, p)?;
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (area, years);
        Err("in the web app, add AlphaEarth's years with Home › Add › Web Data › Satellite Embeddings".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        // A plugin's command in the app runs on the frame, where nothing may wait for the index.
        if s.leave_downloads {
            return Err(
                "AlphaEarth's index is read off the frame here: add its years from Home › Add › Web Data › Satellite Embeddings".into()
            );
        }
        let f = emb::search_aef_series_blocking(area, years)?;
        let notes = series_notes(&f);
        Ok((f.series.into_iter().map(|x| x.years).collect(), f.complete, notes))
    }
}

/// A search over the years that read part of the index, said.
const PART_READ: &str = "AlphaEarth: the area holds more tiles than one search reads; zoom in to take them all";

/// What a search over the years left out: rows past one read, tiles past a search's list, and a
/// tile's second file for a year.
#[cfg(not(target_arch = "wasm32"))]
fn series_notes(f: &emb::SeriesFound) -> Vec<String> {
    let mut notes = Vec::new();
    if !f.complete {
        notes.push(PART_READ.to_string());
    }
    if f.dropped > 0 {
        notes.push(format!("AlphaEarth: {} tiles past the {} a search lists were left out; zoom in for them", f.dropped, emb::MAX_TILES));
    }
    if f.duplicates > 0 {
        notes.push(format!("AlphaEarth: {} tile-years had a second file in the index; the first was taken", f.duplicates));
    }
    notes
}

/// Bytes the app read and handed over, by their id (`ran`).
fn handed(s: &mut Session, p: &Value) -> Result<Option<(u64, Vec<u8>)>, String> {
    let Some(id) = f(p, "ran").filter(|v| *v >= 0.0 && v.fract() == 0.0).map(|v| v as u64) else { return Ok(None) };
    let i = s.downloaded.iter().position(|(d, _)| *d == id).ok_or("no such read (it was dropped when another project opened)")?;
    Ok(Some((id, s.downloaded.remove(i).1)))
}

/// Earth Index's points in the view, coloured by their principal components.
fn add_points(s: &mut Session, p: &Value, name: Option<String>) -> CmdResult {
    let ds = match handed(s, p)? {
        // The app's read: the points' GeoJSON.
        Some((_, bytes)) => {
            let read = std::str::from_utf8(&bytes)
                .map_err(|_| "the points aren't UTF-8 text".to_string())
                .and_then(|text| rustgis_data::io::read_geojson(text, name.as_deref().unwrap_or("Earth Index")).map_err(|e| e.to_string()));
            // A read handed over is used once, added or not (the pane reads again), so a failed
            // one doesn't stay in the session.
            read?.0
        }
        None => {
            let area = bbox(s, p)?;
            let url = match ps(p, "url").map(str::trim).filter(|u| !u.is_empty()) {
                Some(u) if u.starts_with("https://") => u.to_string(),
                Some(_) => return Err("url is an Earth Index file's web address".into()),
                None => {
                    let tile =
                        emb::mgrs_tile((area[0] + area[2]) / 2.0, (area[1] + area[3]) / 2.0).ok_or("the map's centre has no MGRS tile")?;
                    emb::earth_index_url(&tile)
                }
            };
            #[cfg(target_arch = "wasm32")]
            {
                let _ = url;
                return Err("in the web app, add Earth Index with Home › Add › Web Data › Satellite Embeddings".into());
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                let v = emb::load_points_blocking(&url, area)?;
                if v.points.is_empty() {
                    return Err("Earth Index has no points there".into());
                }
                let dim = f(p, "dimension").filter(|d| *d >= 1.0 && d.fract() == 0.0).map(|d| d as usize - 1);
                let tile = url.rsplit('/').next().and_then(|f| f.split('_').next()).unwrap_or("");
                emb::points_dataset(name.as_deref().unwrap_or(&format!("Earth Index {tile}")), &v, dim)
            }
        }
    };
    let n = ds.features.len();
    let id = super::layer::add_dataset(s, ds);
    style_points(s, id);
    Ok(json!({"layer": id, "features": n}))
}

/// Colours a points layer by its `color` field (its principal components, 64 colours), as small
/// squares.
pub(crate) fn style_points(s: &mut Session, id: u32) {
    use rustgis_map::{Marker, Renderer, Rgba, Symbol};
    let Some(l) = s.map_mut().layer_mut(id) else { return };
    let Some(fi) = l.dataset.field_index("color") else { return };
    let mut colors: Vec<String> = l.dataset.features.iter().filter_map(|f| f.attrs.get(fi)).map(|v| v.to_string()).collect();
    colors.sort();
    colors.dedup();
    let symbol = |c: Rgba| Symbol { fill: c, stroke: c, stroke_width: 0.0, size: 5.0, marker: Marker::Square };
    let classes: Vec<(String, Symbol)> =
        colors.into_iter().take(256).filter_map(|c| Rgba::from_hex(&c).map(|rgba| (c, symbol(rgba)))).collect();
    l.renderer = Renderer::Unique {
        field: "color".into(),
        classes,
        other: symbol(Rgba::rgb(128, 128, 128)),
        ramp: Default::default(),
        class_labels: Default::default(),
        other_label: None,
        class_strokes: Default::default(),
        other_strokes: Vec::new(),
        class_fills: Default::default(),
        other_fills: Vec::new(),
        expression: None,
    };
}

/// Three bands of the view from a store of UTM zones (Tessera's, OlmoEarth's), as a raster.
///
/// Without `ran` it reads on the caller's thread. The app's routes (the pane) read off the frame
/// and hand the file over (`ran`); a plugin's command in the app (`leave_downloads`) is refused.
/// The app's assistant runs its tools on the frame without `leave_downloads`, so there it waits,
/// as `embeddings.addCube` does (see its note): moving the assistant's tools off the frame is a
/// change for all of them, not for this one.
fn add_zones(s: &mut Session, p: &Value, d: &emb::EmbeddingDataset, name: Option<String>) -> CmdResult {
    let z = tessera::store(d.id).ok_or_else(|| format!("{} can't be added", d.name))?;
    let year = year(d, p)?;
    let b = bands(p, z.bands, tessera::DEFAULT_BANDS)?;
    let none = format!("{} has no embeddings there", z.name);
    let (r, tif) = match handed(s, p)? {
        Some((_, tif)) => match raster::read_bytes(&format!("{}.tif", z.id), &tif) {
            Ok(r) if r.bands.iter().any(|band| band.stats.count > 0) => (r, tif),
            Ok(_) => return Err(none),
            Err(e) => return Err(e.to_string()),
        },
        None => {
            let area = bbox(s, p)?;
            #[cfg(target_arch = "wasm32")]
            {
                let _ = area;
                return Err(format!("in the web app, add {} with Home › Add › Web Data › Satellite Embeddings", z.name));
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                // A plugin's command in the app runs on the frame, where nothing may wait for the read.
                if s.leave_downloads {
                    return Err(format!("{}: read off the frame here: add it from Home › Add › Web Data › Satellite Embeddings", z.name));
                }
                let (r, cut) = tessera::read_blocking(z, area, year, b)?;
                if cut {
                    s.notify(format!("{}: the view is larger than one read; its centre was read (zoom in for the rest)", z.name), false);
                }
                let tif = raster::geotiff::write(&r, 0).map_err(|e| e.to_string())?;
                (r, tif)
            }
        }
    };
    if r.bands.iter().all(|band| band.stats.count == 0) {
        return Err(none);
    }
    let name = name.unwrap_or_else(|| format!("{} {year} ({})", z.name, band_label(b)));
    let layer = RasterLayer::from_bytes(r, format!("{}_{year}.tif", z.id), tif);
    let mut out = super::raster::add_raster(s, &name, layer)?;
    if !d.license.is_empty() {
        out["license"] = json!(d.license);
    }
    Ok(out)
}

// Similarity and change (#1720): what AlphaEarth's 64 values are for.

/// The most reference points `embeddings.similarity` takes.
const MAX_REFERENCE_POINTS: usize = 1000;

/// AlphaEarth's no-data value: −128 of its signed bytes.
#[cfg(not(target_arch = "wasm32"))]
const AEF_NODATA: f32 = -128.0;

/// The values a cell of an AlphaEarth file holds.
#[cfg(not(target_arch = "wasm32"))]
const AEF_BANDS: usize = 64;

/// `embeddings.similarity` (#1720), the raster [`similarity_work`] makes. Run as a command, it
/// works on its caller's thread, waiting for AlphaEarth's index and the window: the command line
/// and Python have no window to hold, and the app's workflows start it on a thread
/// ([`super::workflows::raster_start`]) and add what it made with `ran`. The app's assistant runs
/// its tools on the frame, so there the window waits, as it does for `raster.change` and the other
/// commands that read the network; a plugin's command in the app is refused ([`analysis`]).
fn similarity(s: &mut Session, p: &Value) -> CmdResult {
    super::workflows::made_or_ran(s, p, "embeddings.similarity")
}

/// `embeddings.change` (#1720), the raster [`change_work`] makes: on its caller's thread, as
/// [`similarity`] says.
fn change(s: &mut Session, p: &Value) -> CmdResult {
    super::workflows::made_or_ran(s, p, "embeddings.change")
}

/// What a similarity or a change reads of the session: AlphaEarth checked as the dataset, the
/// area (the view, read now), the cell size and the name asked for.
type Asked = (&'static emb::EmbeddingDataset, [f64; 4], Option<f64>, Option<String>);

/// [`Asked`], from `embeddings.similarity`'s or `embeddings.change`'s parameters.
fn analysis(s: &Session, p: &Value) -> Result<Asked, String> {
    let d = dataset(p)?;
    if d.id != "alphaearth" {
        return Err(format!("{}: similarity and change are worked out from AlphaEarth's tiles only", d.name));
    }
    // A plugin's command in the app runs on the frame, where nothing may wait for the files.
    if s.leave_downloads {
        return Err(
            "AlphaEarth's files are read by the command line, Python and the assistant, not by a plugin's command in the app".into()
        );
    }
    let cell = match p.get("cellSize").filter(|v| !v.is_null()) {
        None => None,
        Some(_) => Some(f(p, "cellSize").filter(|c| c.is_finite() && *c > 0.0).ok_or("cellSize is a size in metres above 0")?),
    };
    let name = super::name_given(p, "the output's name", 200)?;
    Ok((d, bbox(s, p)?, cell, name))
}

/// A year a command names (`k`), within the dataset's; `default` when left out (or null). One
/// given that isn't a year is refused.
fn year_param(d: &emb::EmbeddingDataset, p: &Value, k: &str, default: i32) -> Result<i32, String> {
    let (first, last) = d.years.unwrap_or((2017, 2025));
    match p.get(k).filter(|v| !v.is_null()) {
        None => Ok(default),
        Some(_) => match f(p, k) {
            Some(y) if y.fract() == 0.0 && y >= f64::from(first) && y <= f64::from(last) => Ok(y as i32),
            _ => Err(format!("{k} is a year from {first} to {last}")),
        },
    }
}

/// `embeddings.similarity`'s reference: `point`, or the `points` whose embeddings are averaged.
fn reference_points(p: &Value) -> Result<Vec<(f64, f64)>, String> {
    const POINTS: &str = "points is a list of [longitude, latitude] in degrees";
    let pair = |v: &Value| {
        v.as_array()
            .filter(|a| a.len() == 2)
            .and_then(|a| Some((a.first()?.as_f64()?, a.get(1)?.as_f64()?)))
            .filter(|(x, y)| (-180.0..=180.0).contains(x) && (-90.0..=90.0).contains(y))
    };
    match (p.get("point").filter(|v| !v.is_null()), p.get("points").filter(|v| !v.is_null())) {
        (Some(_), Some(_)) => Err("give point or points, not both".into()),
        (Some(v), None) => Ok(vec![pair(v).ok_or("point is [longitude, latitude] in degrees")?]),
        (None, Some(Value::Array(list))) if list.is_empty() => Err("points is empty: give at least one [longitude, latitude]".into()),
        (None, Some(Value::Array(list))) if list.len() > MAX_REFERENCE_POINTS => {
            Err(format!("{} points given: at most {MAX_REFERENCE_POINTS} make a reference", list.len()))
        }
        (None, Some(Value::Array(list))) => list.iter().map(|v| pair(v).ok_or_else(|| POINTS.to_string())).collect(),
        (None, Some(_)) => Err(POINTS.into()),
        (None, None) => Err("the reference is required: point [longitude, latitude], or points [[longitude, latitude], …]".into()),
    }
}

/// What `embeddings.similarity` reads of the session, and the work that reads the tile and
/// compares each cell's embedding with the reference.
pub(crate) fn similarity_work(s: &Session, p: &Value) -> Result<super::workflows::Work, String> {
    let (d, area, cell, name) = analysis(s, p)?;
    let year = year(d, p)?;
    let points = reference_points(p)?;
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (area, cell, name, year, points);
        Err(IN_THE_BROWSER.into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    Ok(Box::new(move || {
        rustgis_data::progress::check().map_err(|e| e.to_string())?;
        let (files, place, mut notes) = tile_files(area, &[year])?;
        let file = files.first().ok_or("AlphaEarth has no file there")?;
        let w = read_window(file, area, cell)?;
        notes.extend(w.notes());
        let r = &w.raster;
        let (reference, missed) = reference(r, &points)?;
        notes.extend(missed_points(missed, points.len()));
        let band = similarity_band(r, &reference)?;
        let name = name.unwrap_or_else(|| format!("AlphaEarth {year} similarity"));
        let raster = rustgis_analysis::rasterops::derive(r, &name, vec![band])?;
        made_saying(raster, notes, json!({"year": year, "tile": place, "points": points.len() - missed}))
    }))
}

/// What `embeddings.change` reads of the session, and the work that reads the tile's two years
/// and measures how far each cell's embedding moved.
pub(crate) fn change_work(s: &Session, p: &Value) -> Result<super::workflows::Work, String> {
    let (d, area, cell, name) = analysis(s, p)?;
    let (first, last) = d.years.unwrap_or((2017, 2025));
    let (from, to) = (year_param(d, p, "from", first)?, year_param(d, p, "to", last)?);
    if from >= to {
        return Err("from is a year before to".into());
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (area, cell, name, from, to);
        Err(IN_THE_BROWSER.into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    Ok(Box::new(move || {
        rustgis_data::progress::check().map_err(|e| e.to_string())?;
        let (files, place, mut notes) = tile_files(area, &[from, to])?;
        let (Some(a), Some(b)) = (files.first(), files.get(1)) else { return Err("AlphaEarth has no files there".into()) };
        let a = read_window(a, area, cell)?;
        let b = read_window(b, area, cell)?;
        notes.extend(two_years_notes(&a, &b, (from, to)));
        let band = change_band(&a.raster, &b.raster)?;
        let name = name.unwrap_or_else(|| format!("AlphaEarth change {from}–{to}"));
        let raster = rustgis_analysis::rasterops::derive(&a.raster, &name, vec![band])?;
        made_saying(raster, notes, json!({"from": from, "to": to, "tile": place}))
    }))
}

/// Similarity and change aren't worked out in the web app yet: nothing there may wait for files.
#[cfg(target_arch = "wasm32")]
const IN_THE_BROWSER: &str = "AlphaEarth's similarity and change are worked out on the desktop and the command line for now";

/// The made raster, kept as a GeoTIFF, with what to say of it once it is added (by its name on
/// the map) and what the command answers besides.
#[cfg(not(target_arch = "wasm32"))]
fn made_saying(r: rustgis_data::raster::Raster, notes: Vec<String>, extra: Value) -> Result<super::workflows::Made, String> {
    let bytes = raster::geotiff::write_all(&r).map_err(|e| e.to_string())?;
    let name = r.name.clone();
    let layer = RasterLayer::from_bytes(r, format!("{name}.tif"), bytes);
    let said = name.clone();
    let then: super::workflows::Then = Box::new(move |s: &mut Session, v: &mut Value| {
        let id = v.get("layer").and_then(Value::as_u64).and_then(|i| u32::try_from(i).ok());
        let added = id.and_then(|id| s.map().layer(id)).map_or(said, |l| l.name.clone());
        for note in notes {
            s.notify(format!("{added}: {note}"), false);
        }
    });
    Ok(super::workflows::made_layer(name, layer, Some(extra), Some(then)))
}

/// AlphaEarth's files of `years` (in their order) for the tile nearest the centre of `area` that
/// has them all, from one read of the index (a wait that doesn't look at Cancel); the tile's
/// place, and what to say of the search.
#[cfg(not(target_arch = "wasm32"))]
fn tile_files(area: [f64; 4], years: &[i32]) -> Result<(Vec<String>, String, Vec<String>), String> {
    let span = (years.iter().copied().min().unwrap_or(2024), years.iter().copied().max().unwrap_or(2024));
    pick_tile(&emb::search_aef_series_blocking(area, span)?, years)
}

/// [`tile_files`]'s pick from what the search `found`: the first tile (the nearest the area's
/// centre) with files for all of `years`, and what to say of the search (part of the index read,
/// tiles past a search's list left out, the others the area meets).
#[cfg(not(target_arch = "wasm32"))]
fn pick_tile(found: &emb::SeriesFound, years: &[i32]) -> Result<(Vec<String>, String, Vec<String>), String> {
    let listed = years.iter().map(i32::to_string).collect::<Vec<_>>().join(" and ");
    let (kept, dropped) = (found.series.len(), found.dropped);
    let tile = found.series.iter().find(|t| years.iter().all(|y| t.years.contains_key(y))).ok_or_else(|| {
        let none = match (kept, dropped) {
            (0, 0) => "AlphaEarth has no tiles there".to_string(),
            (_, 0) => format!("AlphaEarth has no tile there with files for {listed}"),
            _ => format!(
                "none of the {kept} AlphaEarth tiles nearest the area's centre has files for {listed}, and {dropped} more were left out: take a smaller box"
            ),
        };
        // A part of the index read: the tile may be in the rest of it.
        if found.complete { none } else { format!("{none}, in the part of the index that was read: take a smaller box to read it whole") }
    })?;
    let files: Vec<String> = years.iter().filter_map(|y| tile.years.get(y).cloned()).collect();
    let place = files.first().and_then(|u| emb::aef_place(u)).unwrap_or_else(|| tile.tile.title.clone());
    let mut notes = Vec::new();
    if !found.complete {
        notes.push(
            "the area holds more of AlphaEarth's index than one search reads, so a tile nearer its centre may have been missed: take a smaller box"
                .to_string(),
        );
    }
    if dropped > 0 {
        notes.push(format!(
            "{dropped} AlphaEarth tiles past the {} nearest the area's centre were left out, not looked at: take a smaller box",
            emb::MAX_TILES
        ));
    }
    if found.duplicates > 0 {
        notes.push(format!("{} tile-years had a second file in AlphaEarth's index; the first was taken", found.duplicates));
    }
    if kept + dropped > 1 {
        notes.push(format!(
            "the area meets {} AlphaEarth tiles: one is read ({place}), the nearest its centre with files for {listed}; a box within one tile is read whole",
            kept + dropped
        ));
    }
    Ok((files, place, notes))
}

/// What a change says of its two years' windows: what the first says of its read (the windows
/// are on one grid, so the second's says the same; anything it adds is said too, by its year),
/// and what each file's header left out, by year: once, for both, when both left it out.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn two_years_notes(a: &super::layer::WindowRaster, b: &super::layer::WindowRaster, (from, to): (i32, i32)) -> Vec<String> {
    let mut notes = a.read_notes();
    for n in b.read_notes() {
        if !notes.contains(&n) {
            notes.push(format!("the {to} file: {n}"));
        }
    }
    for l in &a.raster.lost {
        let both = b.raster.lost.contains(l);
        notes.push(if both { format!("the {from} and {to} files: {l}") } else { format!("the {from} file: {l}") });
    }
    notes.extend(b.raster.lost.iter().filter(|l| !a.raster.lost.contains(l)).map(|l| format!("the {to} file: {l}")));
    notes
}

/// What a similarity says of its reference points that have no embedding in the area read.
#[cfg(not(target_arch = "wasm32"))]
fn missed_points(missed: usize, of: usize) -> Option<String> {
    (missed > 0).then(|| {
        format!("{missed} of the {of} reference points have no embedding in the area read (outside it, or no data there) and were left out")
    })
}

/// A window of AlphaEarth's file `url` over `area`, every band, read on a thread of its own while
/// this one looks at the run's Cancel and stops the read once it is set. A thread the system
/// refuses costs only that: the read runs here, Cancel looked at once it is in.
#[cfg(not(target_arch = "wasm32"))]
fn read_window(url: &str, area: [f64; 4], cell: Option<f64>) -> Result<super::layer::WindowRaster, String> {
    use std::sync::atomic::{AtomicBool, Ordering};
    let asked = super::layer::WindowAsked::of(url, area, cell, AEF_BANDS);
    let stop = AtomicBool::new(false);
    // The reader carries the request context on (a plugin's command reaches only the internet).
    let still = crate::render::rustgis_tiles::cache::current_still();
    let w = std::thread::scope(|scope| {
        let reader = std::thread::Builder::new().name("AlphaEarth window".into()).spawn_scoped(scope, || {
            crate::render::rustgis_tiles::cache::with_optional_still(still, || super::layer::read_window_raster(&asked, &stop))
        });
        let Ok(reader) = reader else { return super::layer::read_window_raster(&asked, &stop) };
        while !reader.is_finished() {
            if rustgis_data::progress::check().is_err() {
                stop.store(true, Ordering::Relaxed);
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        reader.join().unwrap_or_else(|_| Err(format!("{url}: the read failed unexpectedly")))
    })?;
    rustgis_data::progress::check().map_err(|e| e.to_string())?;
    Ok(w)
}

#[cfg(not(target_arch = "wasm32"))]
/// The embedding of cell `i` of `r` into `out` (one value a band), dequantised
/// (`sign(v)·(v/127.5)²`, as AlphaEarth's files are stored) and scaled to unit length, which
/// takes up the quantisation's error: `false` when a band has no value there (AlphaEarth's −128,
/// or the file's no-data) or every value is 0.
fn unit_at(r: &rustgis_data::raster::Raster, i: usize, out: &mut [f32]) -> bool {
    if out.len() != r.bands.len() {
        return false;
    }
    let mut norm = 0.0f64;
    for (o, b) in out.iter_mut().zip(&r.bands) {
        let Some(v) = b.data.get(i).copied().filter(|v| r.is_valid(*v) && *v != AEF_NODATA) else { return false };
        let d = (v / 127.5) * (v / 127.5) * v.signum();
        *o = d;
        norm += f64::from(d) * f64::from(d);
    }
    if norm <= 0.0 {
        return false;
    }
    let n = norm.sqrt() as f32;
    out.iter_mut().for_each(|o| *o /= n);
    true
}

#[cfg(not(target_arch = "wasm32"))]
fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| f64::from(*x) * f64::from(*y)).sum::<f64>() as f32
}

#[cfg(not(target_arch = "wasm32"))]
/// The reference of a similarity: the mean of the unit embeddings at `points` (longitude,
/// latitude) in `r`, scaled to unit length, and how many points have none (outside `r`, or no
/// data there). An error when none has one.
fn reference(r: &rustgis_data::raster::Raster, points: &[(f64, f64)]) -> Result<(Vec<f32>, usize), String> {
    let (mut sum, mut v, mut missed) = (vec![0.0f64; r.bands.len()], vec![0.0f32; r.bands.len()], 0usize);
    for &(lon, lat) in points {
        let cell = r.pixel_at(rustgis_geom::Coord::new(lon, lat)).map(|(c, row)| row as usize * r.width as usize + c as usize);
        match cell {
            Some(i) if unit_at(r, i, &mut v) => sum.iter_mut().zip(&v).for_each(|(s, x)| *s += f64::from(*x)),
            _ => missed += 1,
        }
    }
    if missed == points.len() {
        return Err("no reference point has an embedding in the area read: give points inside the box, where the tile has data".into());
    }
    let norm = sum.iter().map(|s| s * s).sum::<f64>().sqrt();
    if norm <= 0.0 {
        return Err("the reference points' embeddings cancel out: their mean is zero".into());
    }
    Ok((sum.iter().map(|s| (s / norm) as f32).collect(), missed))
}

#[cfg(not(target_arch = "wasm32"))]
/// Each cell's cosine similarity to `reference` (a unit vector), from −1 to 1; no data where the
/// cell has no embedding.
fn similarity_band(r: &rustgis_data::raster::Raster, reference: &[f32]) -> Result<Vec<f32>, String> {
    let (n, k) = (r.width as usize * r.height as usize, reference.len());
    rustgis_analysis::rasterops::per_index(
        n,
        r.width as usize,
        k,
        || vec![0.0f32; k],
        |v, i| if unit_at(r, i, v) { dot(v, reference).clamp(-1.0, 1.0) } else { rustgis_analysis::rasterops::NODATA },
    )
}

#[cfg(not(target_arch = "wasm32"))]
/// How far each cell's embedding moved from `a` to `b` (two years of one tile, on one grid):
/// 1 − their cosine, from 0 (the same) to 2 (opposite); no data where either has none.
fn change_band(a: &rustgis_data::raster::Raster, b: &rustgis_data::raster::Raster) -> Result<Vec<f32>, String> {
    if (a.width, a.height, a.transform, a.crs, a.bands.len()) != (b.width, b.height, b.transform, b.crs, b.bands.len()) {
        return Err("the two years' files of the tile aren't on one grid, so they can't be compared cell by cell".into());
    }
    let (n, k) = (a.width as usize * a.height as usize, a.bands.len());
    rustgis_analysis::rasterops::per_index(
        n,
        a.width as usize,
        2 * k,
        || (vec![0.0f32; k], vec![0.0f32; k]),
        |(x, y), i| {
            if unit_at(a, i, x) && unit_at(b, i, y) { (1.0 - dot(x, y)).clamp(0.0, 2.0) } else { rustgis_analysis::rasterops::NODATA }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Project, Session};

    #[test]
    fn parameters() {
        let mut s = Session::new(Project::default());
        assert_eq!(s.run("embeddings.datasets", &json!({})).unwrap()["datasets"].as_array().map(Vec::len), Some(7));
        for bad in [
            json!({}),
            json!({"dataset": "clay"}),
            json!({"dataset": "nope"}),
            json!({"dataset": "alphaearth", "year": 1999, "bbox": [0, 0, 1, 1]}),
            json!({"dataset": "alphaearth", "bbox": [1, 0, 0, 1]}),
            json!({"dataset": "alphaearth", "bbox": "here"}),
        ] {
            assert!(s.run("embeddings.search", &bad).is_err(), "{bad}");
        }
        for bad in [
            json!({"dataset": "alphaearth"}),
            json!({"dataset": "alphaearth", "url": "file:///etc/passwd"}),
            json!({"dataset": "alphaearth", "url": "https://x.org/a.tiff", "bands": [0, 1, 2]}),
            json!({"dataset": "alphaearth", "url": "https://x.org/a.tiff", "bands": [1, 2]}),
            json!({"dataset": "alphaearth", "url": "https://x.org/a.tiff", "stretch": -1}),
            json!({"dataset": "earth-index", "url": "ftp://x"}),
            json!({"dataset": "tessera", "bands": [1, 2, 200]}),
            json!({"dataset": "tessera", "ran": 99}),
            json!({"dataset": "olmoearth", "bands": [1, 2, 129]}),
            json!({"dataset": "olmoearth", "year": 2016}),
            json!({"dataset": "olmoearth", "ran": 97}),
            json!({"dataset": "earth-index", "ran": 98}),
        ] {
            assert!(s.run("embeddings.add", &bad).is_err(), "{bad}");
        }
        assert_eq!(bands(&json!({"bands": [2, 17, 10]}), 64, [0, 0, 0]), Ok([1, 16, 9]));
        assert_eq!(band_label([1, 16, 9]), "2, 17, 10");
        let tile = json!([{"years": {"2017": "https://x.org/2017/23S/a-1-2.tiff"}}]);
        for bad in [
            json!({"dataset": "tessera", "found": tile}),
            json!({"dataset": "clay", "found": tile}),
            json!({"dataset": "alphaearth", "start": 2016, "found": tile}),
            json!({"dataset": "alphaearth", "end": 2024.5, "found": tile}),
            json!({"dataset": "alphaearth", "start": 2024, "end": 2020, "found": tile}),
            json!({"dataset": "alphaearth", "stretch": 2, "found": tile}),
            // A stretch that isn't a number, not taken as the default.
            json!({"dataset": "alphaearth", "stretch": "big", "found": tile}),
            json!({"dataset": "alphaearth", "stretch": true, "found": tile}),
            json!({"dataset": "alphaearth", "bands": [1, 2], "found": tile}),
            json!({"dataset": "alphaearth", "found": "tiles"}),
            json!({"dataset": "alphaearth", "found": [{"years": {"2017": "file:///etc/passwd"}}]}),
            json!({"dataset": "alphaearth", "found": [{"years": {"later": "https://x.org/a.tiff"}}]}),
            json!({"dataset": "alphaearth", "found": [{"files": []}]}),
            // A year that isn't one, not taken as the default.
            json!({"dataset": "alphaearth", "start": "2020-01-01", "found": tile}),
            json!({"dataset": "alphaearth", "end": true, "found": tile}),
            // A year named twice, one file of it lost.
            json!({"dataset": "alphaearth", "found": [{"years": {"2017": "https://x.org/a.tiff", " 2017": "https://x.org/b.tiff"}}]}),
            // Nothing in the years asked for.
            json!({"dataset": "alphaearth", "start": 2020, "found": tile}),
            json!({"dataset": "alphaearth", "found": vec![json!({"years": {}}); emb::MAX_TILES + 1]}),
        ] {
            assert!(s.run("embeddings.addCube", &bad).is_err(), "{bad}");
        }
        assert!(s.map().tiles.is_empty() && s.map().time.is_none(), "a refused cube leaves the map as it was");
    }

    /// The tiles the app found make a cube: a tile layer each, a file a year in the slider's order,
    /// the slider on over the years; the years a tile has no file for are said.
    #[test]
    fn alphaearth_years_on_the_time_slider() {
        let mut s = Session::new(Project::default());
        let file = |y: i32, place: &str| format!("https://127.0.0.1:9/annual/{y}/23S/x{y}-{place}.tiff");
        let (a, b) = ("0000008192-0000000000", "0000000000-0000000000");
        let found = json!([
            {"years": {"2025": file(2025, a), "2017": file(2017, a), "2016": file(2016, a)}},
            {"years": {"2020": file(2020, b)}},
        ]);
        let r = s.run("embeddings.addCube", &json!({"dataset": "alphaearth", "found": found, "bands": [1, 2, 3]})).unwrap();
        assert_eq!((r["tiles"].as_u64(), r["years"].as_u64(), r["missing"].as_u64()), (Some(2), Some(3), Some(15)));
        assert_eq!(s.map().tiles.len(), 2);
        let named = |s: &Session, place: &str| s.map().tiles.iter().find(|t| t.name.ends_with(place)).cloned().unwrap();
        let first = named(&s, a);
        assert_eq!(first.name, format!("AlphaEarth 2017–2025 (1, 2, 3) · 23S {a}"));
        let dates: Vec<f64> = first.time_urls.iter().map(|(d, _)| *d).collect();
        assert_eq!(dates, vec![emb::year_start_ms(2017), emb::year_start_ms(2025)], "2016 is outside the years asked for");
        let (f, look) = crate::render::rustgis_tiles::cog::split(&first.time_urls[0].1);
        assert_eq!(f, file(2017, a));
        assert!(look.aef && look.cube && look.bands == vec![0, 1, 2], "{look:?}");
        let t = s.map().time.clone().unwrap();
        assert_eq!(t.dates, vec![emb::year_start_ms(2017), emb::year_start_ms(2020), emb::year_start_ms(2025)]);
        assert_eq!(t.format, "YYYY");
        assert!(s.notices.iter().any(|n| n.text.contains("15 of the 18 tile-years have no file")));
        assert!(
            s.notices.iter().any(|n| n.text.contains("1 file handed over is outside 2017–2025 and was left out")),
            "a file outside the years asked for is said: {:?}",
            s.notices.iter().map(|n| &n.text).collect::<Vec<_>>()
        );
        // The slider shows each tile's file of its year, or nothing in a year it has none for.
        s.run("time.goTo", &json!({"step": 1})).unwrap();
        assert!(named(&s, a).time_hidden, "no file in 2020");
        let other = named(&s, b);
        assert!(!other.time_hidden && other.url.starts_with(&file(2020, b)), "{}", other.url);
        // Every year there: nothing said.
        let mut t = Session::new(Project::default());
        let one = json!([{"years": {"2024": file(2024, "1-2"), "2025": file(2025, "1-2")}}]);
        t.run("embeddings.addCube", &json!({"dataset": "alphaearth", "start": 2024, "found": one, "name": "AEF"})).unwrap();
        assert_eq!(t.map().tiles.first().map(|l| l.name.as_str()), Some("AEF"));
        assert!(!t.notices.iter().any(|n| n.text.contains("tile-years")));
        // The pane adds a tile at a time: a second cube of the same name takes its place in it.
        let mut w = Session::new(Project::default());
        for place in [a, b] {
            let one = json!([{"years": {"2024": file(2024, place)}}]);
            w.run("embeddings.addCube", &json!({"dataset": "alphaearth", "found": one})).unwrap();
        }
        let names: Vec<&str> = w.map().tiles.iter().map(|l| l.name.as_str()).collect();
        assert!(names.len() == 2 && names[0] != names[1] && names.iter().any(|n| n.ends_with(b)), "{names:?}");
        // A search that read part of the index: the missing years are the read's, said so; a tile
        // with no file in the years asked for is left out, said.
        let mut u = Session::new(Project::default());
        let part = json!([{"years": {"2025": file(2025, "3-4")}}, {"years": {"2016": file(2016, "5-6")}}]);
        let r = u.run("embeddings.addCube", &json!({"dataset": "alphaearth", "start": 2024, "found": part, "complete": false})).unwrap();
        assert_eq!((r["tiles"].as_u64(), r["missing"].as_u64()), (Some(1), Some(1)));
        assert!(
            u.notices.iter().any(|n| n.text.contains("1 of the 2 tile-years have no file in the part of AlphaEarth's index that was read"))
        );
        assert!(u.notices.iter().any(|n| n.text.contains("1 of the 2 tiles have no file in 2024–2025 and were left out")));
        // A part read with every year there: still said.
        let mut v = Session::new(Project::default());
        let whole = json!([{"years": {"2024": file(2024, "7-8"), "2025": file(2025, "7-8")}}]);
        v.run("embeddings.addCube", &json!({"dataset": "alphaearth", "start": 2024, "found": whole, "complete": false})).unwrap();
        assert!(v.notices.iter().any(|n| n.text.contains("more tiles than one search reads")), "a part read is said");
        // An hourly slider widened over the years would be more steps than it lists: refused,
        // the slider and the map as they were.
        let mut h = Session::new(Project::default());
        h.run("time.set", &json!({"start": "2024-06-01", "end": "2024-06-02", "unit": "hour"})).unwrap();
        let before = h.map().time.clone();
        let two = json!([{"years": {"2017": file(2017, a), "2025": file(2025, a)}}]);
        let e = h.run("embeddings.addCube", &json!({"dataset": "alphaearth", "found": two})).unwrap_err();
        assert!(e.contains("steps"), "{e}");
        assert!(h.map().tiles.is_empty() && h.map().time == before, "a refused cube leaves the map as it was");
        // A slider stepping two years at a time would show 2018's file in 2017's step, and never
        // 2017's: refused, saying which dates; a yearly one takes the cube.
        let nine: serde_json::Map<String, Value> = (2017..=2025).map(|y| (y.to_string(), json!(file(y, a)))).collect();
        let nine = json!([{"years": nine}]);
        let mut g = Session::new(Project::default());
        g.run("time.set", &json!({"start": "2017", "end": "2025", "unit": "year", "interval": 2})).unwrap();
        let before = g.map().time.clone();
        let e = g.run("embeddings.addCube", &json!({"dataset": "alphaearth", "found": nine})).unwrap_err();
        assert!(e.contains("2017-01-01") && e.contains("step"), "{e}");
        assert!(g.map().tiles.is_empty() && g.map().time == before);
        let mut y = Session::new(Project::default());
        y.run("time.set", &json!({"start": "2017", "end": "2025", "unit": "year"})).unwrap();
        y.run("embeddings.addCube", &json!({"dataset": "alphaearth", "found": nine})).unwrap();
        // Other bands (Apply to layers) hold at every year, not only the one shown.
        t.run("map.setTiles", &json!({"index": 0, "bands": [3, 4, 5]})).unwrap();
        t.run("time.goTo", &json!({"step": 0})).unwrap();
        let tile = t.map().tiles.first().cloned().unwrap();
        assert!(tile.url.starts_with(&file(2024, "1-2")), "{}", tile.url);
        for (_, u) in tile.time_urls.iter().chain([&(0.0, tile.url.clone())]) {
            let look = crate::render::rustgis_tiles::cog::split(u).1;
            assert!(look.bands == vec![2, 3, 4] && look.aef && look.cube, "{u}");
        }
    }

    /// What a search over the years left out is said, each with its count; a whole search says
    /// nothing.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn series_notes_say_what_a_search_left_out() {
        let whole = emb::SeriesFound { complete: true, ..Default::default() };
        assert!(series_notes(&whole).is_empty(), "a whole search says nothing");
        let gaps = emb::SeriesFound { complete: false, dropped: 61, duplicates: 2, ..Default::default() };
        let notes = series_notes(&gaps);
        assert_eq!(notes.len(), 3, "{notes:?}");
        assert!(notes.iter().any(|n| n == PART_READ), "{notes:?}");
        assert!(notes.iter().any(|n| n.contains("61 tiles past the 60 a search lists were left out")), "{notes:?}");
        assert!(notes.iter().any(|n| n.contains("2 tile-years had a second file in the index")), "{notes:?}");
    }

    /// A plugin's command in the app (`leave_downloads`) doesn't read an AlphaEarth tile's header
    /// on the frame: the layer is added and read as the map draws.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn alphaearth_added_without_waiting_when_downloads_are_left() {
        let url = "https://127.0.0.1:9/annual/2024/10N/x.tiff";
        let mut s = Session::new(Project::default());
        s.leave_downloads = true;
        let r = s.run("embeddings.add", &json!({"dataset": "alphaearth", "url": url}));
        assert!(r.is_ok(), "{r:?}");
        // The tile is added with its AlphaEarth look.
        let tile = s.map().tiles.last().map(|t| t.url.clone()).unwrap_or_default();
        let (file, look) = crate::render::rustgis_tiles::cog::split(&tile);
        assert_eq!(file, url);
        assert!(look.aef, "{tile}");
        // Elsewhere its header is read first, so an unreadable tile is refused.
        let mut t = Session::new(Project::default());
        assert!(t.run("embeddings.add", &json!({"dataset": "alphaearth", "url": url})).is_err());
    }

    #[test]
    fn points_coloured_by_their_components() {
        let mut s = Session::new(Project::default());
        let v = rustgis_data::source::parquet::Vectors {
            points: vec![(1.0, 2.0), (1.1, 2.0), (1.2, 2.1)],
            dims: 2,
            values: vec![0.0, 1.0, 5.0, 3.0, 9.0, 0.0],
        };
        let ds = emb::points_dataset("ei", &v, None);
        let text = rustgis_data::io::to_geojson(&ds).to_string();
        let id = s.download_started();
        assert!(s.download_landed(id, text.into_bytes()));
        let r = s.run("embeddings.add", &json!({"dataset": "earth-index", "ran": id, "name": "EI"})).unwrap();
        assert_eq!(r["features"], 3);
        let l = s.map().layers.iter().find(|l| l.dataset.name == "EI").unwrap();
        assert!(matches!(&l.renderer, rustgis_map::Renderer::Unique { field, classes, .. } if field == "color" && !classes.is_empty()));
        // Bytes that can't be added are dropped, not kept in the session.
        let id = s.download_started();
        assert!(s.download_landed(id, vec![0xff, 0xfe]));
        assert!(s.run("embeddings.add", &json!({"dataset": "earth-index", "ran": id})).is_err());
        assert!(!s.downloaded.iter().any(|(d, _)| *d == id));
    }

    /// A 2 × 2 raster of quantised embeddings (one value a band, as AlphaEarth stores them), in
    /// longitude/latitude over 10–12° E, 44–46° N, cells by row from the north-west.
    fn quantised(cells: [[f32; 3]; 4]) -> rustgis_data::raster::Raster {
        let bands = (0..3).map(|b| cells.iter().map(|c| c[b]).collect()).collect();
        let t = rustgis_data::raster::GeoTransform { origin_x: 10.0, origin_y: 46.0, pixel_width: 1.0, pixel_height: -1.0 };
        let (crs, gray) = (rustgis_data::raster::Crs::LonLat, rustgis_data::raster::ColorHint::Gray);
        rustgis_data::raster::Raster::new("aef", 2, 2, bands, t, crs, Some(-128.0), gray, "Int8").unwrap()
    }

    #[test]
    fn similarity_and_change_of_known_vectors() {
        use rustgis_analysis::rasterops::NODATA;
        let v = [100.0, -50.0, 20.0];
        let opposite = [-100.0, 50.0, -20.0];
        // Half of v quantised: dequantised (squared) it is a quarter of v's, the same direction.
        let half = [50.0, -25.0, 10.0];
        let no_data = [100.0, -128.0, 20.0];
        let r = quantised([v, opposite, no_data, half]);
        // The cells' centres: (10.5, 45.5), (11.5, 45.5), (10.5, 44.5), (11.5, 44.5).
        let (one, missed) = reference(&r, &[(10.5, 45.5)]).unwrap();
        assert_eq!(missed, 0);
        let close = |got: Vec<f32>, want: [f32; 4]| {
            got.iter().zip(want).all(|(g, w)| if w == NODATA { *g == NODATA } else { (g - w).abs() < 1e-6 })
        };
        let sim = similarity_band(&r, &one).unwrap();
        assert!(close(sim.clone(), [1.0, -1.0, NODATA, 1.0]), "{sim:?}");
        // Several points: their mean. One off the raster and one on no data are counted, not used.
        let (mean, missed) = reference(&r, &[(10.5, 45.5), (11.5, 44.5), (30.0, 0.0), (10.5, 44.5)]).unwrap();
        assert_eq!(missed, 2);
        assert!(close(similarity_band(&r, &mean).unwrap(), [1.0, -1.0, NODATA, 1.0]));
        assert!(reference(&r, &[(30.0, 0.0), (10.5, 44.5)]).unwrap_err().contains("no reference point"));
        // Opposite points cancel out.
        assert!(reference(&r, &[(10.5, 45.5), (11.5, 45.5)]).unwrap_err().contains("cancel out"));
        // Change: the same 0, a quarter of the length still 0, opposite 2, no data either year no data.
        let later = quantised([v, v, v, v]);
        let moved = change_band(&r, &later).unwrap();
        assert!(close(moved.clone(), [0.0, 2.0, NODATA, 0.0]), "{moved:?}");
        // A vector of zeros has no direction: no data.
        let zero = quantised([[0.0; 3], v, v, v]);
        assert_eq!(change_band(&zero, &later).unwrap()[0], NODATA);
        // Two grids that differ aren't compared.
        let mut shifted = quantised([v, v, v, v]);
        shifted.transform.origin_x += 1.0;
        assert!(change_band(&r, &shifted).unwrap_err().contains("one grid"));
    }

    #[test]
    fn similarity_and_change_parameters() {
        let mut s = Session::new(Project::default());
        let area = json!([10.0, 45.0, 10.1, 45.1]);
        for bad in [
            json!({}),
            json!({"dataset": "tessera", "point": [10.05, 45.05], "bbox": area}),
            json!({"dataset": "olmoearth", "point": [10.05, 45.05], "bbox": area}),
            json!({"dataset": "alphaearth", "bbox": area}),
            json!({"dataset": "alphaearth", "point": [10.05, 45.05], "points": [[10.05, 45.05]], "bbox": area}),
            json!({"dataset": "alphaearth", "point": [10.05], "bbox": area}),
            json!({"dataset": "alphaearth", "point": [200, 45], "bbox": area}),
            json!({"dataset": "alphaearth", "points": [], "bbox": area}),
            json!({"dataset": "alphaearth", "points": [[10.05, 45.05], "x"], "bbox": area}),
            json!({"dataset": "alphaearth", "points": vec![json!([10.05, 45.05]); 1001], "bbox": area}),
            json!({"dataset": "alphaearth", "points": "here", "bbox": area}),
            json!({"dataset": "alphaearth", "point": [10.05, 45.05], "year": 2016, "bbox": area}),
            json!({"dataset": "alphaearth", "point": [10.05, 45.05], "cellSize": 0, "bbox": area}),
            json!({"dataset": "alphaearth", "point": [10.05, 45.05], "bbox": [1, 0, 0, 1]}),
            json!({"dataset": "alphaearth", "point": [10.05, 45.05], "ran": 7, "bbox": area}),
        ] {
            assert!(s.run("embeddings.similarity", &bad).is_err(), "{bad}");
        }
        for bad in [
            json!({"dataset": "earth-index", "bbox": area}),
            json!({"dataset": "alphaearth", "from": 2020, "to": 2020, "bbox": area}),
            json!({"dataset": "alphaearth", "from": 2024, "to": 2018, "bbox": area}),
            json!({"dataset": "alphaearth", "from": 2016, "bbox": area}),
            json!({"dataset": "alphaearth", "to": 2025.5, "bbox": area}),
            json!({"dataset": "alphaearth", "cellSize": -10, "bbox": area}),
            json!({"dataset": "alphaearth", "bbox": "here"}),
        ] {
            assert!(s.run("embeddings.change", &bad).is_err(), "{bad}");
        }
        assert!(s.map().layers.is_empty());
        // A plugin's command in the app is refused before anything waits on the frame.
        s.leave_downloads = true;
        let e = s.run("embeddings.change", &json!({"dataset": "alphaearth", "bbox": area})).unwrap_err();
        assert!(e.contains("plugin"), "{e}");
        for d in ["tessera", "olmoearth"] {
            let e = s.run("embeddings.add", &json!({"dataset": d, "bbox": area})).unwrap_err();
            assert!(e.contains("off the frame"), "{e}");
        }
    }

    #[test]
    fn similarity_and_change_say_what_the_search_and_the_points_left_out() {
        let tile = |place: &str, years: &[i32]| emb::Series {
            tile: emb::Tile {
                dataset: "alphaearth",
                id: place.into(),
                title: place.into(),
                detail: String::new(),
                url: String::new(),
                year: None,
                bbox: [0.0; 4],
                footprint: Vec::new(),
            },
            years: years.iter().map(|y| (*y, format!("https://x.org/{y}/23S/h{y}-{place}.tiff"))).collect(),
        };
        // One tile, the index read whole: nothing to say.
        let one = emb::SeriesFound { series: vec![tile("0-0", &[2017, 2025])], dropped: 0, duplicates: 0, complete: true };
        let (files, place, notes) = pick_tile(&one, &[2017, 2025]).unwrap();
        assert_eq!(files, ["https://x.org/2017/23S/h2017-0-0.tiff", "https://x.org/2025/23S/h2025-0-0.tiff"]);
        assert_eq!((place.as_str(), notes.len()), ("23S 0-0", 0));
        // The nearest tile without 2017: the next one is read. Part of the index read, and three
        // tiles past a search's list: each said, the tiles counted with them.
        let many =
            emb::SeriesFound { series: vec![tile("0-0", &[2025]), tile("0-1", &[2017, 2025])], dropped: 3, duplicates: 2, complete: false };
        let (files, place, notes) = pick_tile(&many, &[2017, 2025]).unwrap();
        assert_eq!((files.len(), place.as_str()), (2, "23S 0-1"));
        assert_eq!(notes.len(), 4, "{notes:?}");
        assert!(notes[0].contains("a tile nearer its centre may have been missed"), "{notes:?}");
        assert_eq!(notes[1], "3 AlphaEarth tiles past the 60 nearest the area's centre were left out, not looked at: take a smaller box");
        assert_eq!(notes[2], "2 tile-years had a second file in AlphaEarth's index; the first was taken");
        assert!(notes[3].starts_with("the area meets 5 AlphaEarth tiles: one is read (23S 0-1)"), "{notes:?}");
        // No tile with the years: tiles left out are said, not taken for none there, and so is a
        // part of the index read.
        let e = pick_tile(&many, &[2018]).unwrap_err();
        assert!(e.contains("none of the 2 AlphaEarth tiles nearest the area's centre has files for 2018, and 3 more"), "{e}");
        assert!(
            e.ends_with(
                "and 3 more were left out: take a smaller box, in the part of the index that was read: take a smaller box to read it whole"
            ),
            "{e}"
        );
        let kept = emb::SeriesFound { dropped: 0, duplicates: 0, complete: true, ..many.clone() };
        assert_eq!(pick_tile(&kept, &[2018]).unwrap_err(), "AlphaEarth has no tile there with files for 2018");
        assert_eq!(
            pick_tile(&emb::SeriesFound { complete: true, ..emb::SeriesFound::default() }, &[2018]).unwrap_err(),
            "AlphaEarth has no tiles there"
        );
        // Reference points with no embedding: said with a count, nothing when all have one.
        assert_eq!(missed_points(0, 3), None);
        assert_eq!(
            missed_points(1, 3).as_deref(),
            Some("1 of the 3 reference points have no embedding in the area read (outside it, or no data there) and were left out")
        );
        // What the work says is said once its raster is added, under the layer's name; a raster
        // with nothing to say adds none.
        let mut s = Session::new(Project::default());
        let r = quantised([[1.0; 3]; 4]);
        let made = |name: &str, notes: Vec<String>| {
            let raster = rustgis_analysis::rasterops::derive(&r, name, vec![vec![0.5; 4]]).unwrap();
            made_saying(raster, notes, json!({"year": 2024})).unwrap()
        };
        let mark = s.notice_mark();
        let v = super::super::workflows::add_made(&mut s, made("Sim", notes.clone())).unwrap();
        assert_eq!(v["year"], 2024);
        let said: Vec<String> = s.notices_since(mark).iter().map(|n| n.text.clone()).collect();
        assert_eq!(said.iter().filter(|n| n.starts_with("Sim: ")).count(), 4, "{said:?}");
        let mark = s.notice_mark();
        super::super::workflows::add_made(&mut s, made("Clean", Vec::new())).unwrap();
        assert!(!s.notices_since(mark).iter().any(|n| n.text.starts_with("Clean: ")));
    }
}
