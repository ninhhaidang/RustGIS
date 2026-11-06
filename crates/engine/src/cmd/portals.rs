//! Open-data portals: OpenStreetMap data for an area (through Overpass), administrative boundaries
//! for a country (from geoBoundaries), and datasets found by text in CKAN catalogues (data.gov),
//! Socrata sites and ArcGIS Hub.

use serde_json::{Value, json};

use super::{b, ps};
use crate::render::rustgis_tiles::portal::{self, Portal, Use};
use crate::render::rustgis_tiles::{boundaries, overpass};
use crate::{CmdResult, CommandSpec as C, Session};

pub fn commands() -> Vec<C> {
    vec![
        C::new("osm.download", "Download OpenStreetMap Data", "Home › Add › Web Data", osm_download).params(
            r#"{"bbox"?: [west, south, east, north] | "view" (the map's extent, the default), "view"?: bool (false: a bbox must be given), "preset"?: "buildings"|"roads"|"railways"|"water"|"landuse"|"parks"|"pois"|"boundaries", "tags"?: "key=value, key" (any of them), "url"?: "an Overpass server (default https://overpass-api.de/api/interpreter)", "name"?: text, "answer"?: "the server's JSON answer, when already fetched (the app's background request)"}"#,
        ),
        C::new("boundaries.add", "Add Administrative Boundaries", "Home › Add › Web Data", boundaries_add).params(
            r#"{"country": "ISO 3166 alpha-3 code (CHE, KEN…)", "level"?: "ADM0" (the country, default) … "ADM5", "name"?: text, "text"?: "the boundaries' GeoJSON, when already fetched (the app's background request)"}"#,
        ),
        C::new("boundaries.countries", "List geoBoundaries Countries", "Home › Add › Web Data", boundaries_countries).view(),
        C::new("portal.search", "Search Open Data", "Home › Add › Web Data", portal_search).view().params(
            r#"{"query"?: text (needed by ckan, socrata, arcgis-hub and huggingface), "portal"?: "ckan" (data.gov's catalogue, the default, or any CKAN site) | "socrata" | "arcgis-hub" | "arcgis-portal" (ArcGIS Online, or the portal at url) | "earthdata-gis" (NASA's) | "openaerialmap" | "usgs-dem" (3DEP elevation models) | "ign-lidar" (IGN's LiDAR HD tiles, France) | "geolens" | "huggingface" (a dataset's files: portal.files) | "ocean-data" | "dynamical" (weather and climate Zarr stores), "url"?: "the portal: a CKAN site (https://open.canada.ca/data), a Socrata site (data.cityofnewyork.us), an ArcGIS Hub site, an ArcGIS portal, a GeoLens server", "org"?: "an ArcGIS organisation's id: its public items only", "type"?: "an ArcGIS portal's kind of item: all (the default), imagery, maps, features or webmaps", "dataset"?: "usgs-dem's elevation model: 1 meter, 1/9 arc-second (about 3 m), 1/3 arc-second (about 10 m, the default), 1 arc-second (about 30 m), Alaska 5 meter", "bbox"?: [west, south, east, north] | "view", "view"?: bool (true: where the map is; openaerialmap, usgs-dem and ign-lidar search where the map is unless given a bbox), "limit"?: 1–50 (20)}"#,
        ),
        C::new("portal.files", "List a Dataset's Files", "Home › Add › Web Data", portal_files).view().params(
            r#"{"dataset": "a Hugging Face dataset, owner/name, or an ArcGIS web map's item id (portal.search lists them)", "portal"?: "huggingface" (the default) | "arcgis-portal" | "earthdata-gis", "url"?: "the ArcGIS portal"}"#,
        ),
        C::new("portal.add", "Add Open Data", "Home › Add › Web Data", portal_add).params(
            r#"{"url": "a resource's address, as portal.search lists it", "use"?: "file" (read by address, the default) | "zarr" (a store: its variable, or the first, at "index"'s steps, as layer.addZarr) | "wms" | "wfs" | "arcgis" (a service), "variable"?: "a Zarr store's variable", "index"?: {"time": 0}, "layer"?: "a service's layer (default: its first)", "name"?: text, "portal"?: "the portal's address the resource came from (an address it gives may not reach into the local network)"}"#,
        ),
    ]
}

/// The box a command names, the map's extent by default.
fn bbox(s: &Session, p: &Value) -> Result<[f64; 4], String> {
    match p.get("bbox") {
        Some(v @ Value::Array(_)) => {
            // Every entry a number: one that isn't is an error, not dropped.
            let n: Option<Vec<f64>> = v.as_array().into_iter().flatten().map(Value::as_f64).collect();
            match n.as_deref().unwrap_or_default() {
                [w, south, e, n] => Ok([*w, *south, *e, *n]),
                _ => Err("bbox is [west, south, east, north], or \"view\"".into()),
            }
        }
        Some(Value::String(v)) if v != "view" => Err("bbox is [west, south, east, north], or \"view\"".into()),
        _ if b(p, "view") == Some(false) => Err("give bbox".into()),
        _ => {
            let e = super::workspace::view_extent(s);
            if e.is_empty() {
                return Err("the map's extent isn't known; give bbox".into());
            }
            Ok([e.min_x.max(-180.0), e.min_y.max(-90.0), e.max_x.min(180.0), e.max_y.min(90.0)])
        }
    }
}

/// The Overpass query a command describes (the app builds its request from it too).
pub fn osm_query(s: &Session, p: &Value) -> Result<overpass::Query, String> {
    overpass::Query::new(bbox(s, p)?, ps(p, "preset"), ps(p, "tags"))
}

fn osm_download(s: &mut Session, p: &Value) -> CmdResult {
    let q = osm_query(s, p)?;
    let url = q.url(ps(p, "url"))?;
    let bytes = match ps(p, "answer") {
        Some(a) => a.as_bytes().to_vec(),
        None => fetch(&url)?,
    };
    let answer = overpass::parse(&bytes)?;
    let name = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).map(str::to_string).unwrap_or_else(|| q.title());
    let mut layers = Vec::new();
    let mut counts = serde_json::Map::new();
    for (kind, fc) in answer.layers() {
        let (ds, _) = rustgis_data::io::read_geojson(&fc.to_string(), &format!("{name} ({kind})")).map_err(|e| e.to_string())?;
        counts.insert(kind.to_string(), json!(ds.features.len()));
        layers.push(super::layer::add_dataset(s, ds));
    }
    for n in answer.notes {
        s.notify(format!("{name}: {n}"), false);
    }
    if layers.is_empty() {
        return Err(format!("{name}: nothing found in the area"));
    }
    Ok(json!({"layers": layers, "features": counts}))
}

fn boundaries_add(s: &mut Session, p: &Value) -> CmdResult {
    let country = boundaries::country(ps(p, "country").ok_or("country is required (such as CHE)")?)?;
    let level = match p.get("level") {
        Some(Value::Number(n)) => boundaries::level(&n.to_string())?,
        _ => boundaries::level(ps(p, "level").unwrap_or("ADM0"))?,
    };
    let (text, title) = match ps(p, "text") {
        Some(t) => (t.to_string(), None),
        None => {
            let meta = boundaries::parse_meta(&fetch(&boundaries::meta_url(&country, &level))?)?;
            let bytes = fetch(&meta.geojson)?;
            (String::from_utf8(bytes).map_err(|_| "the boundaries aren't UTF-8 text".to_string())?, Some(meta.name))
        }
    };
    let name = ps(p, "name")
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("{} {level}", title.unwrap_or_else(|| country.clone())));
    let (ds, _) = rustgis_data::io::read_geojson(&text, &name).map_err(|e| format!("{name}: {e}"))?;
    let n = ds.features.len();
    let empty_before = super::map_is_empty(s);
    let id = super::layer::add_dataset(s, ds);
    if empty_before && let Some(e) = s.map().layer(id).map(|l| l.extent()) {
        s.zoom_to(&e);
    }
    Ok(json!({"layer": id, "features": n}))
}

fn boundaries_countries(_s: &mut Session, _p: &Value) -> CmdResult {
    let list = boundaries::parse_countries(&fetch(&boundaries::countries_url())?)?;
    Ok(json!({"count": list.len(), "countries": list.iter().map(|(iso, name)| json!({"iso": iso, "name": name})).collect::<Vec<_>>()}))
}

/// The portal and search a command names.
pub fn portal_query(s: &Session, p: &Value) -> Result<(Portal, String, Option<[f64; 4]>, usize), String> {
    let portal =
        Portal::new(ps(p, "portal"), ps(p, "url"))?.with_filter(ps(p, "org").or_else(|| ps(p, "dataset")))?.with_category(ps(p, "type"))?;
    let query = ps(p, "query").map(str::trim).unwrap_or("").to_string();
    if query.is_empty() && portal.needs_query() {
        return Err("query is required (such as parks)".into());
    }
    // Everywhere unless a place is asked for (where the map is, for a search by place).
    let place = p.get("bbox").is_some() || b(p, "view") == Some(true) || (portal.needs_place() && b(p, "view") != Some(false));
    let area = if place { Some(bbox(s, p)?) } else { None };
    portal.check_area(area)?;
    let limit = super::f(p, "limit").map(|l| l.clamp(1.0, portal::MAX_RESULTS as f64) as usize).unwrap_or(20);
    Ok((portal, query, area, limit))
}

/// A dataset as the commands and the app list it.
pub fn dataset_json(d: &portal::Dataset) -> Value {
    let resources: Vec<Value> = d
        .resources
        .iter()
        .map(|r| match &r.using {
            Use::File => json!({"format": r.format, "url": r.url, "use": "file"}),
            Use::Zarr => json!({"format": r.format, "url": r.url, "use": "zarr"}),
            Use::Service { kind, layer } => json!({"format": r.format, "url": r.url, "use": kind, "layer": layer}),
        })
        .collect();
    let mut v = json!({"id": d.id, "title": d.title, "kind": d.kind, "date": d.date, "description": d.description, "organization": d.organization, "page": d.page, "thumbnail": d.thumbnail, "bbox": d.bbox, "resources": resources});
    if d.listing.is_some() {
        v["files"] = json!("portal.files lists them (a web map's layers, a Hugging Face dataset's files)");
    }
    v
}

fn portal_search(s: &mut Session, p: &Value) -> CmdResult {
    let (portal, query, area, limit) = portal_query(s, p)?;
    let url = portal.search_url(&query, area, limit);
    let found = portal.parse(&url, &fetch(&url)?, area, &query)?;
    Ok(json!({"portal": portal.label(), "count": found.len(), "datasets": found.iter().map(dataset_json).collect::<Vec<_>>()}))
}

/// The listing address of a dataset a command names (Hugging Face's `owner/name`).
pub fn files_query(p: &Value) -> Result<(Portal, String), String> {
    let portal = Portal::new(Some(ps(p, "portal").unwrap_or("huggingface")), ps(p, "url"))?;
    let name = ps(p, "dataset").map(str::trim).filter(|d| !d.is_empty()).ok_or("dataset is required (owner/name, or a web map's id)")?;
    let url =
        portal.listing_of(name).ok_or_else(|| format!("'{name}': a Hugging Face dataset is owner/name, an ArcGIS web map its item id"))?;
    Ok((portal, url))
}

fn portal_files(_s: &mut Session, p: &Value) -> CmdResult {
    let (portal, url) = files_query(p)?;
    let files = portal.parse_listing(&url, &fetch(&url)?)?;
    let list: Vec<Value> = files
        .iter()
        .map(|r| match &r.using {
            Use::File => json!({"path": r.format, "url": r.url, "use": "file"}),
            Use::Zarr => json!({"path": r.format, "url": r.url, "use": "zarr"}),
            Use::Service { kind, layer } => json!({"title": r.format, "url": r.url, "use": kind, "layer": layer}),
        })
        .collect();
    Ok(json!({"count": list.len(), "files": list}))
}

/// Adds a portal's resource: a file by its address (read in place when it can be), a service's
/// layer (the one named, or its first).
fn portal_add(s: &mut Session, p: &Value) -> CmdResult {
    let url = ps(p, "url").map(str::trim).filter(|u| !u.is_empty()).ok_or("url is required (a resource portal.search lists)")?;
    // The portal's documents are hostile: an address they give is judged as one found there.
    let from = ps(p, "portal").map(str::trim).filter(|u| !u.is_empty());
    crate::render::rustgis_tiles::address::check(url, from)?;
    let name = ps(p, "name").map(str::trim).filter(|n| !n.is_empty());
    match ps(p, "use").unwrap_or("file") {
        "file" => {
            let mut q = json!({"url": url});
            if let Some(n) = name {
                q["name"] = json!(n);
            }
            let r = super::layer::add_url(s, &q)?;
            // A Socrata export answers as many rows as asked for and says nothing more: as many
            // as that means there were probably more.
            let socrata = url.contains("/resource/") && url.contains("limit=");
            if socrata && r["features"].as_u64() == Some(portal::SOCRATA_ROWS as u64) {
                s.notify(format!("the dataset was cut at {} rows (Socrata's export gives at most that many)", portal::SOCRATA_ROWS), false);
            }
            Ok(r)
        }
        "zarr" => {
            let mut q = json!({"url": url});
            for k in ["variable", "index", "name"] {
                if let Some(v) = p.get(k) {
                    q[k] = v.clone();
                }
            }
            super::zarr::add(s, &q)
        }
        kind @ ("wms" | "wfs" | "arcgis") => {
            let layer = match ps(p, "layer").map(str::trim).filter(|l| !l.is_empty()) {
                Some(l) => l.to_string(),
                None => first_layer(s, url, kind)?,
            };
            let mut q = json!({"url": url, "type": kind, "layer": layer});
            if let Some(n) = name {
                q["name"] = json!(n);
            }
            super::service::add(s, &q)
        }
        other => Err(format!("'{other}': a resource is used as a file, or a wms, wfs or arcgis service")),
    }
}

/// A service's first layer: an ArcGIS map or image service's whole map, else its first layer
/// with features, else its first.
fn first_layer(s: &mut Session, url: &str, kind: &str) -> Result<String, String> {
    let svc = super::service::layers(s, &json!({"url": url, "type": kind}))?;
    let layers = svc["layers"].as_array().cloned().unwrap_or_default();
    let lower = url.trim_end_matches('/').to_ascii_lowercase();
    let picture = kind == "arcgis" && (lower.ends_with("/mapserver") || lower.ends_with("/imageserver"));
    let wanted = if picture { "tiles" } else { "features" };
    layers
        .iter()
        .find(|l| l["offer"]["type"] == wanted)
        .or_else(|| layers.first())
        .and_then(|l| l["name"].as_str())
        .map(str::to_string)
        .ok_or_else(|| format!("{url}: the service lists no layer RustGIS can add"))
}

/// A fetch the command waits for (the command line and the assistant; the app fetches off the
/// frame and passes the answer).
#[cfg(not(target_arch = "wasm32"))]
fn fetch(url: &str) -> Result<Vec<u8>, String> {
    crate::render::rustgis_tiles::cache::download_blocking(url).map_err(|e| {
        if e.contains("timeout") || e.contains("timed out") {
            "the server didn't answer within a minute (it may be busy): try again later, or a smaller area".to_string()
        } else if e.contains("504") || e.contains("429") {
            format!("{e}: the server is busy, try again in a minute")
        } else {
            e
        }
    })
}

#[cfg(target_arch = "wasm32")]
fn fetch(_url: &str) -> Result<Vec<u8>, String> {
    Err("in the web app, use Home › Add › Web Data › OpenStreetMap…, Boundaries… or Search Open Data…".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_queries() {
        let (_, url) = files_query(&json!({"dataset": "giswqs/geospatial"})).unwrap();
        assert!(url.starts_with("https://huggingface.co/api/datasets/giswqs/geospatial/tree/main"));
        let (_, url) = files_query(&json!({"portal": "earthdata-gis", "dataset": "abc123"})).unwrap();
        assert_eq!(url, "https://gis.earthdata.nasa.gov/portal/sharing/rest/content/items/abc123/data?f=json");
        for bad in [
            json!({}),
            json!({"dataset": ""}),
            json!({"dataset": "../x"}),
            json!({"portal": "ckan", "dataset": "a/b"}),
            json!({"portal": "nope", "dataset": "a/b"}),
            json!({"portal": "arcgis-portal", "dataset": "a b"}),
        ] {
            assert!(files_query(&bad).is_err(), "{bad}");
        }
    }
}
