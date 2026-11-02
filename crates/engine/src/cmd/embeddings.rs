//! Satellite embeddings (Home › Add › Web Data › Satellite Embeddings): the catalog of embedding
//! datasets, a search for a dataset's tiles where the map is, and a tile shown on the map —
//! AlphaEarth's as a cloud-optimised GeoTIFF of three bands, Earth Index's as points coloured by
//! their vectors' principal components, Tessera's as a raster of three bands of the view.
//!
//! On the desktop and the command line these commands read what they need, waiting for it; the
//! web app reads it off the frame and hands it over (`ran`).

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
        })
        .view(),
        // Not a view: it adds the footprints layer (by default).
        C::new("embeddings.search", "Search Satellite Embeddings", "Home › Add › Web Data", search).params(
            r#"{"dataset": "alphaearth" | "earth-index" | "tessera", "year"?: number (AlphaEarth and Tessera: 2017–2025, default 2024), "bbox"?: [west, south, east, north] | "view" (the map's extent, the default), "footprints"?: bool (adds the tiles' footprints as a layer, default true)}"#,
        ),
        C::new("embeddings.add", "Add Satellite Embeddings", "Home › Add › Web Data", add).params(
            r#"{"dataset": "alphaearth" | "earth-index" | "tessera", "url"?: "AlphaEarth's tile or Earth Index's file, as embeddings.search lists it (Earth Index: default the MGRS tile at the view's centre)", "year"?: number (Tessera, default 2024), "bands"?: [red, green, blue] (bands from 1; AlphaEarth default [2, 17, 10], Tessera [1, 2, 3]), "stretch"?: number (AlphaEarth: dequantised values shown over ±stretch, default 0.3), "dimension"?: number (Earth Index: a vector value kept as each point's `value`, from 1), "bbox"?: [west, south, east, north] | "view" (Earth Index and Tessera: the area read, default the map's extent), "name"?: text, "ran"?: (internal: what the app read)}"#,
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
    let id = ps(p, "dataset").map(str::trim).ok_or("dataset is required (alphaearth, earth-index or tessera)")?;
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
            _ => tessera::tiles(area, year),
        };
        let mut r = json!({"dataset": d.id, "year": year, "count": tiles.len(), "complete": complete,
            "tiles": tiles.iter().map(emb::Tile::to_json).collect::<Vec<_>>()});
        if !complete {
            s.notify(format!("{}: the area holds more tiles than one search reads; zoom in to list them all", d.name), false);
        }
        if super::b(p, "footprints") != Some(false) && !tiles.is_empty() && d.id != "tessera" {
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
            let stretch = f(p, "stretch").map(|v| v as f32).unwrap_or(emb::AEF_DEFAULT_STRETCH);
            if !(stretch.is_finite() && stretch > 0.0 && stretch <= 1.0) {
                return Err("stretch is a number above 0, at most 1".into());
            }
            let look = emb::aef_look(b, stretch);
            let file = crate::render::rustgis_tiles::cog::split(url).0;
            // The year from the tile's address (…/annual/2024/10N/…), when it has one.
            let year = file.split('/').find(|s| s.len() == 4 && s.parse::<i32>().is_ok());
            let name = name.unwrap_or_else(|| {
                let title = year.map(|y| format!("AlphaEarth {y}")).unwrap_or_else(|| "AlphaEarth".into());
                format!("{title} ({})", band_label(b))
            });
            // Its header read first (as layer.addUrl does), so the layer is framed and its bands
            // known; in the browser nothing may wait, and it is read as the map draws.
            #[cfg(not(target_arch = "wasm32"))]
            if !crate::render::rustgis_tiles::cog::probe_blocking(file)? {
                return Err(format!("{file}: not a cloud-optimised GeoTIFF"));
            }
            super::layer::add_cog(s, &json!({"name": name}), &crate::render::rustgis_tiles::cog::with_look(file, &look))
        }
        "earth-index" => add_points(s, p, name),
        _ => add_tessera(s, p, d, name),
    }
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

/// Tessera's three bands of the view, as a raster.
fn add_tessera(s: &mut Session, p: &Value, d: &emb::EmbeddingDataset, name: Option<String>) -> CmdResult {
    let year = year(d, p)?;
    let b = bands(p, tessera::BANDS, tessera::DEFAULT_BANDS)?;
    let (r, tif) = match handed(s, p)? {
        Some((_, tif)) => match raster::read_bytes("tessera.tif", &tif) {
            Ok(r) if r.bands.iter().any(|band| band.stats.count > 0) => (r, tif),
            Ok(_) => return Err("Tessera has no embeddings there".into()),
            Err(e) => return Err(e.to_string()),
        },
        None => {
            let area = bbox(s, p)?;
            #[cfg(target_arch = "wasm32")]
            {
                let _ = area;
                return Err("in the web app, add Tessera with Home › Add › Web Data › Satellite Embeddings".into());
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                let (r, cut) = tessera::read_blocking(area, year, b)?;
                if cut {
                    s.notify("Tessera: the view is larger than one read; its centre was read (zoom in for the rest)", false);
                }
                let tif = raster::geotiff::write(&r, 0).map_err(|e| e.to_string())?;
                (r, tif)
            }
        }
    };
    if r.bands.iter().all(|band| band.stats.count == 0) {
        return Err("Tessera has no embeddings there".into());
    }
    let name = name.unwrap_or_else(|| format!("Tessera {year} ({})", band_label(b)));
    let layer = RasterLayer::from_bytes(r, format!("tessera_{year}.tif"), tif);
    super::raster::add_raster(s, &name, layer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Project, Session};

    #[test]
    fn parameters() {
        let mut s = Session::new(Project::default());
        assert_eq!(s.run("embeddings.datasets", &json!({})).unwrap()["datasets"].as_array().map(Vec::len), Some(6));
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
            json!({"dataset": "earth-index", "ran": 98}),
        ] {
            assert!(s.run("embeddings.add", &bad).is_err(), "{bad}");
        }
        assert_eq!(bands(&json!({"bands": [2, 17, 10]}), 64, [0, 0, 0]), Ok([1, 16, 9]));
        assert_eq!(band_label([1, 16, 9]), "2, 17, 10");
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
}
