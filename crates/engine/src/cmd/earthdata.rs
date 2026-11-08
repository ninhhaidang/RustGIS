//! NASA Earthaccess: Earthdata's collections found by keyword, a collection's granules where the
//! map is and between two dates (their footprints added as a layer), and the Earthdata Login
//! token their files are read with (kept for the session, or remembered on this computer; see
//! `rustgis_tiles::earthdata`). A granule's GeoTIFF is added by its address (`layer.addUrl`),
//! read in place as a COG.

use serde_json::{Value, json};

use super::{b, f, ps};
use crate::render::rustgis_tiles::earthdata::{self, Collection};
use crate::{CmdResult, CommandSpec as C, Session};

pub fn commands() -> Vec<C> {
    vec![
        C::new("earthdata.collections", "Search NASA Earthdata Collections", "Home › Add › Web Data › NASA Earthaccess", collections).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network))
            .view()
            .params(r#"{"keyword"?: text (free text: "sea surface temperature"), "shortName"?: "a collection's short name (HLSL30, ATL08, GEDI02_A…)", "cloudHosted"?: bool (only collections in NASA's cloud), "limit"?: 1–100 (20), "page"?: number (1), "answer"?: "CMR's JSON answer, when already fetched (the app's background request)"} — keyword or shortName is needed; searching needs no Earthdata Login"#),
        C::new("earthdata.granules", "Search NASA Earthdata Granules", "Home › Add › Web Data › NASA Earthaccess", granules).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network)).params(
            r#"{"collection"?: "a collection's concept id (C…-PROVIDER, as earthdata.collections lists it)", "shortName"?: "or a short name: its cloud-hosted, newest version", "bbox"?: [west, south, east, north] | "view" (the map's extent, the default), "view"?: bool (false: everywhere), "start"?: "YYYY-MM-DD", "end"?: "YYYY-MM-DD", "limit"?: 1–100 (25), "page"?: number (1), "footprints"?: bool (adds the granules' footprints as a layer, default true), "name"?: text (the footprints layer's), "answer"?: "CMR's JSON answer, when already fetched"} — each granule lists its data files; add a GeoTIFF with layer.addUrl (files need an Earthdata Login token: earthdata.login)"#,
        ),
        C::new("earthdata.login", "Earthdata Login Token", "Home › Add › Web Data › NASA Earthaccess", login).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Reserved)).view().params(
            r#"{"token": "an Earthdata Login token (Earthdata Login › Generate Token)", "remember"?: bool (false: kept for the session only; true: also remembered on this computer, in the system's credential store, and read back when next needed; without it a token remembered before is deleted)} — never in the project; sent only to NASA's Earthdata data hosts (https://*.earthdatacloud.nasa.gov, *.earthdata.nasa.gov) when their files are read; an expired token is forgotten"#,
        ),
        C::new("earthdata.logout", "Forget Earthdata Login Token", "Home › Add › Web Data › NASA Earthaccess", logout).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Reserved))
            .view()
            .params("{} — forgets the token for the session, deletes the one remembered on this computer and empties the download cache (what was read with it)"),
        C::new("earthdata.status", "Earthdata Login Status", "Home › Add › Web Data › NASA Earthaccess", |_s, _p| {
            // Asked for: the token remembered on this computer is read, if it wasn't.
            earthdata::restore();
            earthdata::forget_if_expired();
            Ok(status())
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .view()
        .params("{} — whether a token is kept, its expiry, whether it is remembered on this computer, and whether an expired one was forgotten (give a new one with earthdata.login)"),
    ]
}

/// Whether a token is kept, when it expires, whether it is remembered on this computer, and
/// whether an expired one was forgotten.
fn status() -> Value {
    json!({
        "token": earthdata::has_token(),
        "expires": earthdata::token_expiry().map(earthdata::date_of),
        "remembered": earthdata::is_remembered(),
        "expired": earthdata::expired_forgotten(),
    })
}

fn login(_s: &mut Session, p: &Value) -> CmdResult {
    // Judged before anything happens: a mistyped `remember` must not delete a remembered token.
    let remember = match p.get("remember") {
        None => false,
        Some(Value::Bool(on)) => *on,
        Some(_) => return Err("remember is true or false".into()),
    };
    let token = ps(p, "token").map(str::trim).filter(|t| !t.is_empty()).ok_or("token is required (Earthdata Login › Generate Token)")?;
    let warning = earthdata::login(token, remember)?;
    let mut out = status();
    if let Some(w) = warning {
        out["warning"] = json!(w);
    }
    Ok(out)
}

fn logout(_s: &mut Session, _p: &Value) -> CmdResult {
    let forgotten = earthdata::logout();
    // Files and documents read with the token are cached by address: they go with it, as at an
    // ArcGIS sign-out. Always, since a token forgotten on expiry may have read what is kept.
    crate::render::rustgis_tiles::cache::clear();
    let mut out = status();
    if let Err(w) = forgotten {
        out["warning"] = json!(w);
    }
    Ok(out)
}

/// A page size and number a command names.
fn paging(p: &Value, default: usize) -> (usize, usize) {
    let limit = f(p, "limit").map(|l| l.clamp(1.0, earthdata::MAX_PAGE as f64) as usize).unwrap_or(default);
    let page = f(p, "page").map(|n| n.clamp(1.0, earthdata::MAX_PAGE_NUM as f64) as usize).unwrap_or(1);
    (limit, page)
}

/// The collection search a command names.
pub fn collections_query(p: &Value) -> Result<String, String> {
    let keyword = ps(p, "keyword").map(str::trim).unwrap_or("");
    let short = ps(p, "shortName").map(str::trim).filter(|s| !s.is_empty());
    if keyword.is_empty() && short.is_none() {
        return Err("give keyword (free text) or shortName (such as HLSL30)".into());
    }
    let (limit, page) = paging(p, 20);
    Ok(earthdata::collections_url(keyword, short, b(p, "cloudHosted").unwrap_or(false), limit, page))
}

fn collections(_s: &mut Session, p: &Value) -> CmdResult {
    let url = collections_query(p)?;
    let bytes = answer_or_fetch(p, &url)?;
    let found = earthdata::parse_collections(&bytes)?;
    Ok(json!({"count": found.len(), "collections": found.iter().map(earthdata::collection_json).collect::<Vec<_>>()}))
}

/// The collection a short name means: its cloud-hosted, newest version.
fn resolve(short: &str) -> Result<Collection, String> {
    let url = earthdata::collections_url("", Some(short), false, 50, 1);
    let found = earthdata::parse_collections(&fetch(&url)?)?;
    earthdata::best(&found, short).cloned().ok_or_else(|| format!("NASA's CMR has no collection named {short}"))
}

/// The granule search a command names: its address and the collection's concept id.
pub fn granules_query(s: &Session, p: &Value) -> Result<String, String> {
    let collection = ps(p, "collection").map(str::trim).filter(|c| !c.is_empty());
    let collection = match (collection, ps(p, "shortName").map(str::trim).filter(|c| !c.is_empty())) {
        (Some(c), _) => c.to_string(),
        (None, Some(short)) => resolve(short)?.id,
        (None, None) => return Err("give collection (a concept id earthdata.collections lists) or shortName".into()),
    };
    // Where the map is, unless asked for everywhere.
    let area = if p.get("bbox").is_none() && b(p, "view") == Some(false) { None } else { Some(super::catalogs::area(s, p)?) };
    let (limit, page) = paging(p, 25);
    earthdata::granules_url(&collection, area, ps(p, "start"), ps(p, "end"), limit, page)
}

fn granules(s: &mut Session, p: &Value) -> CmdResult {
    let url = granules_query(s, p)?;
    let bytes = answer_or_fetch(p, &url)?;
    let found = earthdata::parse_granules(&bytes)?;
    let mut out = json!({"count": found.len(), "granules": found.iter().map(earthdata::granule_json).collect::<Vec<_>>()});
    if b(p, "footprints").unwrap_or(true) {
        let name = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).unwrap_or("Earthdata footprints");
        if let Some(id) = super::stac::add_collection(s, &earthdata::footprints_geojson(&found), name)? {
            out["layer"] = json!(id);
        }
    }
    if !earthdata::has_token() && found.iter().any(|g| g.data.iter().any(|u| earthdata::is_data_host(u))) {
        out["note"] = json!("reading these files needs an Earthdata Login token: earthdata.login");
    }
    Ok(out)
}

/// The answer the app fetched, or the one fetched now.
fn answer_or_fetch(p: &Value, url: &str) -> Result<Vec<u8>, String> {
    match ps(p, "answer") {
        Some(a) => Ok(a.as_bytes().to_vec()),
        None => fetch(url),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn fetch(url: &str) -> Result<Vec<u8>, String> {
    crate::render::rustgis_tiles::cache::download_blocking(url)
}

#[cfg(target_arch = "wasm32")]
fn fetch(_url: &str) -> Result<Vec<u8>, String> {
    Err("in the web app, search NASA Earthdata with Home › Add › Web Data › NASA Earthaccess".into())
}
