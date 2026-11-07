//! Cloud data catalogs: Overture Maps (a theme's data where the map is, read in place from its
//! GeoParquet files, or the whole world as map tiles) and Source Cooperative (its products and
//! their files, added by address).

use serde_json::{Value, json};

use super::{b, ps};
use crate::render::rustgis_tiles::{overture, sourcecoop};
use crate::{CmdResult, CommandSpec as C, Session};

pub fn commands() -> Vec<C> {
    vec![
        C::new("overture.add", "Add Overture Maps Data", "Home › Add › Web Data", overture_add).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network)).params(
            r#"{"theme": "buildings"|"places"|"transportation"|"divisions"|"base"|"addresses", "type"?: "the theme's type (building, building_part; place; segment, connector; division_area, division, division_boundary; land_use, land, land_cover, water, infrastructure, bathymetry; address)", "release"?: "such as 2026-09-23.1 (default: the latest)", "bbox"?: [west, south, east, north] | "view" (the map's extent, the default), "tiles"?: bool (the whole world as map tiles, from the theme's PMTiles archive, instead of the features in view), "files"?: [the files' GeoParquet addresses, when already found and opened (the app's background requests)]}"#,
        ),
        C::new("sourcecoop.products", "List Source Cooperative Products", "Home › Add › Web Data", sourcecoop_products).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network))
            .view()
            .params(r#"{"account"?: "an account's products (default: the featured ones)"}"#),
        C::new("sourcecoop.list", "List Source Cooperative Files", "Home › Add › Web Data", sourcecoop_list).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network)).view().params(
            r#"{"account": text, "product": text, "path"?: "a folder inside the product, ending in /", "token"?: "the next page's, from a listing"} (a file's "url" is added with layer.addUrl)"#,
        ),
    ]
}

/// The box a command names, the map's extent by default.
pub fn area(s: &Session, p: &Value) -> Result<[f64; 4], String> {
    match p.get("bbox") {
        Some(v @ Value::Array(_)) => {
            let n: Option<Vec<f64>> = v.as_array().into_iter().flatten().map(Value::as_f64).collect();
            let lon = |x: f64| (-180.0..=180.0).contains(&x);
            let lat = |y: f64| (-90.0..=90.0).contains(&y);
            match n.as_deref().unwrap_or_default() {
                // West past east crosses the antimeridian: the files either side of it are read.
                [w, south, e, n] if south <= n && lon(*w) && lon(*e) && lat(*south) && lat(*n) => Ok([*w, *south, *e, *n]),
                _ => Err("bbox is [west, south, east, north] in degrees (longitudes −180 to 180, south up to north; west past east crosses the antimeridian), or \"view\"".into()),
            }
        }
        Some(Value::String(v)) if v != "view" => Err("bbox is [west, south, east, north], or \"view\"".into()),
        _ => {
            let e = super::workspace::view_extent(s);
            if e.is_empty() {
                return Err("the map's extent isn't known; give bbox".into());
            }
            Ok([e.min_x.max(-180.0), e.min_y.max(-90.0), e.max_x.min(180.0), e.max_y.min(90.0)])
        }
    }
}

/// The release a command names, else the latest.
fn release(p: &Value) -> Result<String, String> {
    match ps(p, "release").map(str::trim).filter(|r| !r.is_empty() && *r != "latest") {
        Some(r) => overture::release(r),
        None => overture::parse_latest(&fetch(&overture::catalog_url())?),
    }
}

fn overture_add(s: &mut Session, p: &Value) -> CmdResult {
    let (theme, ty) = overture::theme_type(ps(p, "theme").ok_or("theme is required (such as buildings)")?, ps(p, "type"))?;
    // The whole world, as map tiles.
    if b(p, "tiles").unwrap_or(false) {
        let release = release(p)?;
        let mut r = super::tiles::add(
            s,
            &json!({"url": overture::pmtiles_url(&release, theme), "kind": "pmtiles", "name": format!("Overture {theme}"), "overlay": true, "attribution": overture::attribution(theme)}),
        )?;
        r["release"] = json!(release);
        return Ok(r);
    }
    // The files the app found and opened already: added as one (all or none).
    if let Some(files) = p.get("files") {
        let urls: Vec<String> =
            files.as_array().ok_or("files is a list of addresses")?.iter().filter_map(Value::as_str).map(str::to_string).collect();
        if urls.is_empty() || urls.len() > overture::MAX_FILES {
            return Err(format!("files is 1 to {} addresses", overture::MAX_FILES));
        }
        // Overture's own files only: the list comes back from the app, but a caller could pass
        // any address.
        if let Some(u) = urls.iter().find(|u| !overture::is_data_file(u)) {
            return Err(format!("{u}: not one of Overture's release files"));
        }
        let mut r = add_files(s, ty, &urls)?;
        s.notify(format!("Overture {ty}: {}", overture::attribution(theme)), false);
        r["theme"] = json!(theme);
        r["type"] = json!(ty);
        return Ok(r);
    }
    let area = area(s, p)?;
    let release = release(p)?;
    let files = overture::parse_collection(&fetch(&overture::collection_url(&release, theme, ty))?)?;
    let hits = overture::files_in(&files, area);
    if hits.is_empty() {
        return Err(format!("Overture has no {ty} data in the area"));
    }
    if hits.len() > overture::MAX_FILES {
        return Err(format!(
            "the area takes {} of Overture's {ty} files (up to {}): zoom in, or add the theme as map tiles (\"tiles\": true)",
            hits.len(),
            overture::MAX_FILES
        ));
    }
    // Every file's address first: a catalog that fails here leaves the project as it was.
    let urls = hits.iter().map(|f| fetch(&f.item).and_then(|b| overture::parse_item(&b))).collect::<Result<Vec<_>, _>>()?;
    let mut r = add_files(s, ty, &urls)?;
    s.notify(format!("Overture {ty} ({release}): {}", overture::attribution(theme)), false);
    r["release"] = json!(release);
    r["theme"] = json!(theme);
    r["type"] = json!(ty);
    Ok(r)
}

/// Adds a theme type's files (read in place, the map's view kept), all or none: when one can't be
/// added, the ones added before it are taken out again, so trying again doesn't add them twice.
pub(crate) fn add_files(s: &mut Session, ty: &str, urls: &[String]) -> CmdResult {
    // What a failure puts back: the map's layers in their order and its layer ids (not a copy of
    // their data), and the active layer.
    let (n, active, before) = (urls.len(), s.active_layer, s.map().layer_mark());
    let mut layers: Vec<u32> = Vec::new();
    for (i, url) in urls.iter().enumerate() {
        match super::layer::add_url(s, &json!({"url": url, "name": overture::layer_name(ty, i, n), "zoom": false})) {
            Ok(r) => layers
                .extend(r["layers"].as_array().into_iter().flatten().filter_map(Value::as_u64).filter_map(|id| u32::try_from(id).ok())),
            Err(e) => {
                s.map_mut().restore_layers(&before);
                s.active_layer = active;
                return Err(if n > 1 { format!("{e} (none of the {n} files was added)") } else { e });
            }
        }
    }
    Ok(json!({"layers": layers, "files": n}))
}

fn sourcecoop_products(_s: &mut Session, p: &Value) -> CmdResult {
    let url = match ps(p, "account").map(str::trim).filter(|a| !a.is_empty()) {
        Some(a) => sourcecoop::account_url(a)?,
        None => sourcecoop::featured_url(),
    };
    let list = sourcecoop::parse_products(&fetch(&url)?)?;
    Ok(json!({
        "count": list.len(),
        "products": list.iter().map(|p| json!({"account": p.account, "product": p.id, "title": p.title, "description": p.description, "tags": p.tags})).collect::<Vec<_>>(),
    }))
}

fn sourcecoop_list(_s: &mut Session, p: &Value) -> CmdResult {
    let account = ps(p, "account").ok_or("account is required")?;
    let product = ps(p, "product").ok_or("product is required")?;
    let url = sourcecoop::list_url(account, product, ps(p, "path").unwrap_or(""), ps(p, "token"))?;
    let listing = crate::render::rustgis_tiles::s3::parse_list(&fetch(&url)?)?;
    let account = sourcecoop::id(account)?;
    Ok(json!({
        "folders": listing.prefixes,
        "files": listing.objects.iter().map(|o| json!({"key": o.key, "size": o.size, "url": sourcecoop::file_url(&account, &o.key).ok()})).collect::<Vec<_>>(),
        "next": listing.next,
    }))
}

/// A fetch the command waits for (the command line and the assistant; the app fetches off the
/// frame).
#[cfg(not(target_arch = "wasm32"))]
fn fetch(url: &str) -> Result<Vec<u8>, String> {
    crate::render::rustgis_tiles::cache::download_blocking(url)
}

#[cfg(target_arch = "wasm32")]
fn fetch(_url: &str) -> Result<Vec<u8>, String> {
    Err("in the web app, use Home › Add › Web Data › Overture Maps… or Source Cooperative…".into())
}
