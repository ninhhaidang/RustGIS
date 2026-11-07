//! Geocoding: finding a place by name, and turning a table's addresses into points.

use rustgis_data::{Feature, Field, FieldType, Geometry, GeometryType, Value as Attr};
use rustgis_geom::{Coord, Extent};
use serde_json::{Value, json};

use super::{f, ps};
use crate::render::rustgis_tiles::geocode::{Geocoder, Place};
use crate::{CmdResult, CommandSpec as C, Session};

pub fn commands() -> Vec<C> {
    vec![
        C::new("map.findPlace", "By Place or Address…", "Home › Navigate › Locate", find_place).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network)).params(
            r#"{"query": "a place or an address", "provider"?: "nominatim" (OpenStreetMap, default) | "photon" | "locatieserver" (PDOK, the Netherlands' addresses and places), "url"?: "a geocoding server of your own", "limit"?: 1–20, "go"?: bool (zooms to the first, default true)}"#,
        ),
        C::new("layer.geocode", "Geocode Addresses", "Home › Add › Add Data", geocode).inputs(source_inputs).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network)).params(
            r#"{"table"?: name|id (a standalone table, such as a CSV of addresses), "layer"?: name|id (or the active layer), "address": field | [fields] (joined with commas, such as street, city, country), "provider"?: "nominatim" | "photon" | "locatieserver" (PDOK, Dutch addresses; up to 10,000 a run), "url"?: "a geocoding server of your own (more than 100 addresses with nominatim or photon)", "name"?: text, "places"?: "answers already looked up, {address: {lon, lat, name} | null} (the app's background run; nothing is fetched)"}"#,
        ),
    ]
}

/// The geocoder a command names.
pub(crate) fn geocoder(p: &Value) -> Result<Geocoder, String> {
    Geocoder::new(ps(p, "provider"), ps(p, "url"))
}

/// A place as the commands return it.
pub fn place_json(p: &Place) -> Value {
    json!({"name": p.name, "lon": p.lon, "lat": p.lat, "bbox": p.bbox})
}

/// Zooms to a place: its extent, or around its point.
pub fn go_to_place(s: &mut Session, p: &Place) {
    let e = match p.bbox {
        // A city's extent, not a country's whole territory with its islands: no wider than 40°.
        Some([w, south, e, n]) if e > w && n > south && e - w < 40.0 && n - south < 40.0 => Extent::new(w, south, e, n),
        _ => Extent::new(p.lon - 0.01, p.lat - 0.01, p.lon + 0.01, p.lat + 0.01),
    };
    s.zoom_to(&e);
}

fn find_place(s: &mut Session, p: &Value) -> CmdResult {
    let query = ps(p, "query").map(str::trim).filter(|q| !q.is_empty()).ok_or("query is required")?;
    let g = geocoder(p)?;
    let limit = f(p, "limit").map(|l| l.clamp(1.0, 20.0) as usize).unwrap_or(5);
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (s, query, g, limit);
        Err("in the web app, find places with Home › Navigate › Locate › By Place or Address…".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let found = crate::render::rustgis_tiles::geocode::search_blocking(&g, query, limit)?;
        if found.skipped > 0 {
            s.warn(skipped_notice(found.skipped));
        }
        let places = found.places;
        if super::b(p, "go").unwrap_or(true)
            && let Some(first) = places.first()
        {
            go_to_place(s, first);
        }
        Ok(json!({"count": places.len(), "places": places.iter().map(place_json).collect::<Vec<_>>()}))
    }
}

/// The table or layer whose addresses are geocoded: `table` names a standalone table; `layer` a
/// layer, or a table when no layer has its name.
fn source<'a>(input: &'a impl super::InputSource, p: &Value) -> Result<&'a rustgis_data::Dataset, String> {
    let context = input.input_context();
    let s = &context;
    let table = |n: &str| s.project.tables.iter().find(|t| t.name == n).map(|t| &t.dataset);
    if p.get("table").is_some_and(|t| !t.is_null()) {
        let i = s.table_index(p)?;
        return s.project.tables.get(i).map(|t| &t.dataset).ok_or_else(|| "no such table".to_string());
    }
    if let Some(n) = ps(p, "layer")
        && s.map().layer_by_name(n).is_none()
        && let Some(t) = table(n)
    {
        return Ok(t);
    }
    let l = s.map().layer(s.layer_id(p)?).ok_or("no such layer")?;
    if l.is_raster() {
        return Err(format!("{}: geocoding reads a table or a feature layer, not a raster", l.name));
    }
    Ok(&l.dataset)
}

/// The address fields a command names.
fn address_fields(p: &Value) -> Result<Vec<String>, String> {
    let fields: Vec<String> = match p.get("address") {
        Some(Value::String(f)) => f.split(',').map(|f| f.trim().to_string()).filter(|f| !f.is_empty()).collect(),
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(|f| f.trim().to_string()).filter(|f| !f.is_empty()).collect(),
        _ => Vec::new(),
    };
    if fields.is_empty() || fields.len() > 10 {
        return Err("address is a field, or up to 10 fields joined with commas".into());
    }
    Ok(fields)
}

/// Each row's address, from the layer's fields (empty for a row without one), and the distinct
/// addresses in their first order. The app geocodes those itself, then runs `layer.geocode` with
/// the answers in `"places"`.
pub fn addresses(s: &Session, p: &Value) -> Result<(Vec<String>, Vec<String>), String> {
    let ds = source(s, p)?;
    if ds.windowed {
        return Err(format!("{}: load all its features first (Layers › Load All Features)", ds.name));
    }
    let idx: Vec<usize> = address_fields(p)?
        .iter()
        .map(|f| ds.field_index(f).ok_or_else(|| format!("{}: no field '{f}'", ds.name)))
        .collect::<Result<_, _>>()?;
    let rows: Vec<String> = (0..ds.features.len())
        .map(|r| {
            idx.iter()
                .filter(|&&i| !ds.value(r, i).is_null())
                .map(|&i| ds.value(r, i).to_string().trim().to_string())
                .filter(|v| !v.is_empty())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .collect();
    let mut distinct: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for a in rows.iter().filter(|a| !a.is_empty()) {
        if seen.insert(a.as_str()) {
            distinct.push(a.clone());
        }
    }
    Ok((rows, distinct))
}

/// What a run says of the places the geocoder answered with but gave no usable position for
/// (they are left out, so an address may be "not found" because of them).
pub fn skipped_notice(n: usize) -> String {
    format!("{n} of the geocoder's places had no usable position (no coordinate, or an unreadable one) and were left out")
}

/// The addresses past what a run may send.
pub fn check_batch(g: &Geocoder, distinct: usize) -> Result<(), String> {
    if distinct > g.batch_limit() {
        return Err(if g.asks_light_use() {
            format!(
                "{distinct} addresses: a public geocoder takes {} per run (its usage policy); give \"url\" of a Nominatim or Photon server of your own for more",
                g.batch_limit()
            )
        } else {
            format!("{distinct} addresses: up to {} per run", g.batch_limit())
        });
    }
    Ok(())
}

/// The answers the app passes in `"places"`: an object from address to `{lon, lat, name}` (or
/// null when nothing was found).
fn given_places(v: &Value) -> Result<std::collections::HashMap<String, Option<Place>>, String> {
    let o = v.as_object().ok_or("places is an object from address to {lon, lat, name}")?;
    if o.len() > rustgis_tiles_max() {
        return Err(format!("places: {} answers, up to {} per run", o.len(), rustgis_tiles_max()));
    }
    Ok(o.iter()
        .map(|(k, v)| {
            let place = (|| {
                let (lon, lat) = (v.get("lon")?.as_f64()?, v.get("lat")?.as_f64()?);
                ((-180.0..=180.0).contains(&lon) && (-90.0..=90.0).contains(&lat)).then(|| Place {
                    name: v.get("name").and_then(Value::as_str).unwrap_or("").chars().take(500).collect(),
                    lon,
                    lat,
                    bbox: None,
                })
            })();
            (k.clone(), place)
        })
        .collect())
}

fn rustgis_tiles_max() -> usize {
    crate::render::rustgis_tiles::geocode::MAX_BATCH
}

fn geocode(s: &mut Session, p: &Value) -> CmdResult {
    let g = geocoder(p)?;
    let (rows, distinct) = addresses(s, p)?;
    if distinct.is_empty() {
        return Err("the address fields are empty in every row".into());
    }
    let found = match p.get("places") {
        Some(v) => given_places(v)?,
        None => {
            check_batch(&g, distinct.len())?;
            let l = lookup(&g, &distinct)?;
            if l.skipped > 0 {
                s.warn(skipped_notice(l.skipped));
            }
            if l.failed > 0 {
                s.notify(
                    format!(
                        "{} addresses couldn't be looked up ({}); run again later for them",
                        l.failed,
                        l.error.as_deref().unwrap_or("the server refused")
                    ),
                    true,
                );
            }
            l.found
        }
    };
    let src = source(s, p)?;
    let name =
        ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).map(str::to_string).unwrap_or_else(|| format!("{} (geocoded)", src.name));
    // The table's fields, and what the geocoder called each place.
    let mut fields = src.fields.clone();
    let mut col = "geocoded".to_string();
    while fields.iter().any(|f| f.name.eq_ignore_ascii_case(&col)) {
        col.push('_');
    }
    fields.push(Field::new(col, FieldType::Text));
    let mut ds = rustgis_data::Dataset::new(name, GeometryType::Point, fields);
    let (mut matched, mut unmatched) = (0usize, 0usize);
    for (i, (feat, address)) in src.features.iter().zip(&rows).enumerate() {
        let place = found.get(address).and_then(Option::as_ref);
        let mut attrs = feat.attrs.clone();
        attrs.resize(src.fields.len(), Attr::Null);
        attrs.push(place.map(|pl| Attr::Text(pl.name.clone())).unwrap_or(Attr::Null));
        if place.is_some() {
            matched += 1;
        } else {
            unmatched += 1;
        }
        // Rows not found stay in the table, without a point.
        ds.features.push(Feature { id: i as u64, geometry: place.map(|pl| Geometry::Point(Coord::new(pl.lon, pl.lat))), attrs });
    }
    if matched == 0 {
        return Err("none of the addresses was found".into());
    }
    let new = super::layer::add_dataset(s, ds);
    if unmatched > 0 {
        s.notify(format!("{unmatched} of {} rows weren't found; they are in the table without a point", rows.len()), false);
    }
    Ok(json!({"layer": new, "matched": matched, "unmatched": unmatched}))
}

/// What a run found: each address's place (or none), and the addresses whose requests failed.
#[derive(Default)]
struct Lookup {
    found: std::collections::HashMap<String, Option<Place>>,
    failed: usize,
    error: Option<String>,
    /// Places the answers gave without a usable position.
    skipped: usize,
}

/// Geocodes each address, waiting between requests as the server asks.
#[cfg(not(target_arch = "wasm32"))]
fn lookup(g: &Geocoder, distinct: &[String]) -> Result<Lookup, String> {
    let mut out = Lookup::default();
    let (mut succeeded, mut in_a_row) = (0usize, 0usize);
    let mut sent = false;
    for (i, a) in distinct.iter().enumerate() {
        // An address longer than the server takes isn't sent cut short: it fails on its own, and
        // isn't a refusal by the server (which would stop the run).
        if let Err(e) = g.check_query(a) {
            out.failed += 1;
            out.error = Some(e);
            continue;
        }
        if sent {
            std::thread::sleep(std::time::Duration::from_millis(g.spacing_ms()));
        }
        sent = true;
        match crate::render::rustgis_tiles::geocode::search_blocking(g, a, 1) {
            Ok(found) => {
                succeeded += 1;
                in_a_row = 0;
                out.skipped += found.skipped;
                out.found.insert(a.clone(), found.places.into_iter().next());
            }
            // A failed request isn't "not found": it is counted apart, and a server that refuses
            // every request (or starts to, as a rate limit does) stops the run.
            Err(e) => {
                in_a_row += 1;
                out.failed += 1;
                if in_a_row >= 3 && succeeded == 0 {
                    return Err(e);
                }
                if in_a_row >= 5 {
                    out.failed += distinct.len().saturating_sub(i + 1);
                    out.error = Some(e);
                    break;
                }
                out.error = Some(e);
            }
        }
    }
    Ok(out)
}

#[cfg(target_arch = "wasm32")]
fn lookup(_: &Geocoder, _: &[String]) -> Result<Lookup, String> {
    Err("in the web app, geocode addresses with Home › Add › Add Data › Geocode Addresses".into())
}

/// Reuses the executor's actual layer/table choice, including its exact-case table fallback.
fn source_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let s = input;
    let dataset = source(input, p)?;
    let mut inputs = super::Inputs::layers(s.map().layers.iter().filter(|l| std::ptr::eq(&l.dataset, dataset)).map(|l| (s.map(), l)));
    inputs.tables.extend(s.project.tables.iter().filter(|t| std::ptr::eq(&t.dataset, dataset)).map(|t| t.id));
    Ok(inputs)
}
