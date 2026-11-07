//! STAC (SpatioTemporal Asset Catalog) APIs: their collections, a search for items, and the
//! assets an item offers. Requests are built and answers read here; the fetching is the caller's
//! (blocking on the command line, off the frame in the app). Answers are hostile input: counts,
//! lengths and coordinates are bounded.

use serde_json::Value;

pub mod catalog;

/// Element 84's Earth Search (Sentinel-2, Landsat, Copernicus DEM and more, on AWS), the default.
pub const EARTH_SEARCH: &str = "https://earth-search.aws.element84.com/v1";
/// Microsoft's Planetary Computer, whose assets are signed with a token of their collection's
/// (see [`sas_token_url`]).
pub const PLANETARY_COMPUTER: &str = "https://planetarycomputer.microsoft.com/api/stac/v1";
/// The Planetary Computer's token service.
const SAS_API: &str = "https://planetarycomputer.microsoft.com/api/sas/v1/token";
/// How long a token is used: the service's last an hour; a shorter reuse leaves a COG read in
/// place time to draw.
const TOKEN_REUSE: web_time::Duration = web_time::Duration::from_secs(30 * 60);

/// The most items a search asks for.
pub const MAX_ITEMS: usize = 200;
/// The most collections read from an API.
const MAX_COLLECTIONS: usize = 2000;
/// The most assets read from an item.
const MAX_ASSETS: usize = 100;
/// The longest text kept (titles, ids, addresses).
const MAX_TEXT: usize = 2000;
/// The most coordinates a footprint keeps.
const MAX_FOOTPRINT: usize = 20_000;
/// The most properties kept with an item.
const MAX_PROPERTIES: usize = 100;

/// A collection an API offers.
#[derive(Clone, Debug, PartialEq)]
pub struct Collection {
    pub id: String,
    pub title: String,
}

/// An asset of an item: a file, by its address.
#[derive(Clone, Debug, PartialEq)]
pub struct Asset {
    pub key: String,
    pub href: String,
    /// Its media type (`image/tiff; application=geotiff; profile=cloud-optimized`…).
    pub media_type: String,
    pub roles: Vec<String>,
    pub title: String,
    /// Its first band's `raster:bands` scale and offset: a stored value `v` means
    /// `v · scale + offset`.
    pub scale: Option<f64>,
    pub offset: Option<f64>,
    /// Its first band's stored no-data value (`raster:bands` or `eo:bands` `nodata`), for files
    /// whose own header doesn't name it.
    pub nodata: Option<f64>,
}

/// An item: a scene (or tile) with its footprint, time and assets.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub id: String,
    pub collection: String,
    pub datetime: String,
    /// West, south, east, north.
    pub bbox: Option<[f64; 4]>,
    /// Its footprint, a GeoJSON geometry.
    pub geometry: Option<Value>,
    /// `eo:cloud_cover`, in percent.
    pub cloud: Option<f64>,
    pub assets: Vec<Asset>,
    /// Its own address (its `self` link), which relative asset addresses are resolved against.
    pub url: Option<String>,
    /// The address it was fetched from, which its asset addresses are judged by (never its own
    /// `self` link, which it could point anywhere); `None` for an item given whole, whose assets
    /// may then only be public web addresses.
    pub origin: Option<String>,
    /// Its properties, to show: the first hundred plain values (not objects or arrays), long
    /// text cut.
    pub properties: Option<Value>,
    /// The tile of a grid it covers, when its properties name one (`grid:code`, `s2:mgrs_tile`,
    /// the MGRS extension's zone, band and square, Landsat's WRS path and row, Brazil Data Cube's
    /// `bdc:tiles`): items of a data cube on one tile, at different dates.
    pub tile: Option<String>,
}

/// What a search asks for.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Search {
    pub collections: Vec<String>,
    /// West, south, east, north, in longitude/latitude.
    pub bbox: Option<[f64; 4]>,
    /// A date or a range: `2024-06-01`, `2024-06-01/2024-06-30`, `2024-06-01/..`, or RFC 3339.
    pub datetime: Option<String>,
    pub limit: usize,
    /// The most cloud cover, in percent.
    pub max_cloud: Option<f64>,
    /// The order of the items: a property, `-` first for descending (`eo:cloud_cover`,
    /// `-datetime`), for APIs with the sort extension.
    pub sortby: Option<String>,
}

/// An API's address from what the user gave: without a trailing `/search` or `/collections`,
/// checked like any address the user names. Empty gives Earth Search.
pub fn api(url: Option<&str>) -> Result<String, String> {
    let Some(u) = url.map(str::trim).filter(|u| !u.is_empty()) else { return Ok(EARTH_SEARCH.to_string()) };
    crate::address::check(u, None)?;
    let inner = crate::logins::inner(u);
    let inner = inner.split(['?', '#']).next().unwrap_or(inner).trim_end_matches('/');
    let inner = inner.strip_suffix("/search").or_else(|| inner.strip_suffix("/collections")).unwrap_or(inner);
    Ok(crate::logins::inherit(u, inner))
}

/// The request for an API's collections.
pub fn collections_url(api: &str) -> String {
    crate::template::join(api, &format!("{}/collections", crate::logins::inner(api)))
}

/// The request for a search (STAC API's `GET /search`).
pub fn search_url(api: &str, s: &Search) -> Result<String, String> {
    let mut q = vec![format!("limit={}", s.limit.clamp(1, MAX_ITEMS))];
    let ids: Vec<&str> = s.collections.iter().map(|c| c.trim()).filter(|c| !c.is_empty()).take(50).collect();
    if !ids.is_empty() {
        q.push(format!("collections={}", encode(&ids.join(","))));
    }
    if let Some([w, south, e, n]) = s.bbox {
        let ok = |x: f64, lim: f64| x.is_finite() && (-lim..=lim).contains(&x);
        if !(ok(w, 180.0) && ok(e, 180.0) && ok(south, 90.0) && ok(n, 90.0) && south < n) {
            return Err("bbox is [west, south, east, north] in longitude/latitude".into());
        }
        q.push(format!("bbox={w},{south},{e},{n}"));
    }
    if let Some(d) = s.datetime.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
        q.push(format!("datetime={}", encode(&datetime(d)?)));
    }
    if let Some(c) = s.max_cloud.filter(|c| c.is_finite()) {
        let c = c.clamp(0.0, 100.0);
        q.push(format!("query={}", encode(&format!(r#"{{"eo:cloud_cover":{{"lte":{c}}}}}"#))));
    }
    if let Some(by) = s.sortby.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
        let (sign, field) = by.strip_prefix('-').map(|f| ("-", f)).unwrap_or(("+", by.trim_start_matches('+')));
        if field.is_empty() || field.len() > 100 || !field.chars().all(|c| c.is_ascii_alphanumeric() || "_:.-".contains(c)) {
            return Err(format!("{by}: sortby is a property's name, - first for descending"));
        }
        let field = if field.starts_with("properties.") || matches!(field, "id" | "collection") {
            field.to_string()
        } else {
            format!("properties.{field}")
        };
        q.push(format!("sortby={}", encode(&format!("{sign}{field}"))));
    }
    Ok(crate::template::join(api, &format!("{}/search?{}", crate::logins::inner(api), q.join("&"))))
}

/// Whether an address is a web address.
fn is_web(u: &str) -> bool {
    let u = crate::logins::inner(u);
    let l = u.get(..8).unwrap_or(u).to_ascii_lowercase();
    l.starts_with("https://") || l.starts_with("http://")
}

/// Where an item given whole is taken to come from: the web, whose documents may not point into
/// the local network.
const UNTRUSTED: &str = "https://item.invalid/";

/// A date range as STAC asks for it (RFC 3339): a bare date starts or ends its day.
fn datetime(d: &str) -> Result<String, String> {
    let bad = || format!("{d}: give a date (2024-06-01) or a range (2024-06-01/2024-06-30)");
    if d.len() > 100 || !d.chars().all(|c| c.is_ascii_digit() || "-:.TZtz+/".contains(c)) {
        return Err(bad());
    }
    // Each side is open (`..` or empty), a date, or a date and time.
    let side_ok = |s: &str| {
        let s = s.trim();
        let date = |x: &str| {
            let b = x.as_bytes();
            b.len() >= 10
                && b.get(4) == Some(&b'-')
                && b.get(7) == Some(&b'-')
                && x.get(..4).is_some_and(|y| y.bytes().all(|c| c.is_ascii_digit()))
        };
        s.is_empty() || s == ".." || (date(s) && (s.len() == 10 || s.get(10..11).is_some_and(|t| t.eq_ignore_ascii_case("t"))))
    };
    let open = |s: &str| matches!(s.trim(), "" | "..");
    if !d.split('/').all(side_ok) || d.split('/').count() > 2 || d.split('/').all(open) {
        return Err(bad());
    }
    let bare = |s: &str| s.len() == 10 && s.as_bytes().get(4) == Some(&b'-') && s.as_bytes().get(7) == Some(&b'-');
    let end = |s: &str, end: bool| -> String {
        let s = s.trim();
        if s.is_empty() || s == ".." {
            "..".to_string()
        } else if bare(s) {
            format!("{s}T{}Z", if end { "23:59:59" } else { "00:00:00" })
        } else {
            s.to_string()
        }
    };
    Ok(match d.split_once('/') {
        Some((a, b)) => format!("{}/{}", end(a, false), end(b, true)),
        // One day.
        None if bare(d.trim()) => format!("{}/{}", end(d, false), end(d, true)),
        None => d.trim().to_string(),
    })
}

/// Percent-encodes a query parameter.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn text(v: Option<&Value>) -> String {
    v.and_then(Value::as_str).unwrap_or("").chars().take(MAX_TEXT).collect()
}

fn json(bytes: &[u8]) -> Result<Value, String> {
    serde_json::from_slice(bytes).map_err(|_| "the STAC API's answer isn't JSON".to_string())
}

/// The collections in an API's `/collections` answer.
pub fn parse_collections(bytes: &[u8]) -> Result<Vec<Collection>, String> {
    let v = json(bytes)?;
    let list = v.get("collections").and_then(Value::as_array).ok_or("the answer has no collections (is this a STAC API?)")?;
    Ok(list
        .iter()
        .take(MAX_COLLECTIONS)
        .filter_map(|c| {
            let id = text(c.get("id"));
            (!id.is_empty()).then(|| Collection { title: text(c.get("title")), id })
        })
        .collect())
}

/// The items of a search's answer (an ItemCollection), and the address of its next page. `from`
/// is the address the answer came from, which relative addresses are resolved against.
pub fn parse_items(bytes: &[u8], from: &str) -> Result<(Vec<Item>, Option<String>), String> {
    let v = json(bytes)?;
    let features = v.get("features").and_then(Value::as_array).ok_or("the answer isn't a list of STAC items")?;
    let items = features.iter().take(MAX_ITEMS).filter_map(|f| item(f, Some(from))).collect();
    let next = link(&v, "next", from);
    Ok((items, next))
}

/// How many items a search's answer says match in all (`numberMatched`, or the context
/// extension's `matched`).
pub fn parse_matched(bytes: &[u8]) -> Option<u64> {
    let v = json(bytes).ok()?;
    v.get("numberMatched").or_else(|| v.get("context").and_then(|c| c.get("matched"))).and_then(Value::as_u64)
}

/// The STAC API an item or collection fetched from `from` belongs to: the address before
/// `/collections/`, else its `root` link's (a static catalog's `….json` root is no API), else the
/// collection its `parent` link names. `None` when nothing says.
pub fn api_of(bytes: &[u8], from: &str) -> Option<String> {
    let before = |u: &str| crate::logins::inner(u).split_once("/collections/").map(|(a, _)| crate::logins::inherit(u, a));
    if let Some(a) = before(from) {
        return Some(a);
    }
    let v = json(bytes).ok()?;
    if let Some(root) = link(&v, "root", from) {
        let inner = crate::logins::inner(&root);
        let r = inner.split(['?', '#']).next().unwrap_or(inner).trim_end_matches('/');
        if !r.to_ascii_lowercase().ends_with(".json") && is_web(r) {
            return Some(crate::logins::inherit(&root, r));
        }
    }
    link(&v, "parent", from).and_then(|p| before(&p))
}

pub fn parse_item(bytes: &[u8], from: &str) -> Result<Item, String> {
    item(&json(bytes)?, Some(from)).ok_or_else(|| "the answer isn't a STAC item".to_string())
}

/// An item from its JSON (as STAC gives it, or as [`item_json`] wrote it).
/// `from` is the address it was fetched from, if it was.
pub fn item_from_json(v: &Value, from: Option<&str>) -> Option<Item> {
    item(v, from)
}

/// A link's address by its relation, resolved against `from`.
fn link(v: &Value, rel: &str, from: &str) -> Option<String> {
    let l = v.get("links")?.as_array()?.iter().take(100).find(|l| l.get("rel").and_then(Value::as_str) == Some(rel))?;
    // A POST link needs its body: only a plain GET is followed.
    if l.get("method").and_then(Value::as_str).is_some_and(|m| !m.eq_ignore_ascii_case("GET")) {
        return None;
    }
    resolve(from, l.get("href")?.as_str()?)
}

fn item(f: &Value, from: Option<&str>) -> Option<Item> {
    if f.get("type").and_then(Value::as_str) != Some("Feature") {
        return None;
    }
    let id = text(f.get("id"));
    if id.is_empty() {
        return None;
    }
    // Without a fetch address, only a self link that is a whole web address counts.
    let url = match from {
        Some(from) => link(f, "self", from),
        None => link(f, "self", "").filter(|u| is_web(u)),
    };
    let base = url.clone().or_else(|| from.map(str::to_string));
    let props = f.get("properties");
    let bbox = f.get("bbox").and_then(Value::as_array).and_then(|b| {
        let n: Vec<f64> = b.iter().filter_map(Value::as_f64).collect();
        // A 3D bbox gives six numbers: west, south, low, east, north, high.
        let [w, s, e, n] = match n.as_slice() {
            [w, s, e, n] => [*w, *s, *e, *n],
            [w, s, _, e, n, _] => [*w, *s, *e, *n],
            _ => return None,
        };
        let ok = |x: f64, lim: f64| x.is_finite() && (-lim..=lim).contains(&x);
        (ok(w, 180.0) && ok(e, 180.0) && ok(s, 90.0) && ok(n, 90.0) && s <= n).then_some([w, s, e, n])
    });
    let geometry = f.get("geometry").filter(|g| g.is_object() && footprint_size(g) <= MAX_FOOTPRINT).cloned();
    let cloud = props.and_then(|p| p.get("eo:cloud_cover")).and_then(Value::as_f64).filter(|c| c.is_finite());
    let datetime = props
        .and_then(|p| p.get("datetime").filter(|d| d.is_string()).or_else(|| p.get("start_datetime")))
        .and_then(Value::as_str)
        .unwrap_or("")
        .chars()
        .take(40)
        .collect();
    let mut assets: Vec<Asset> = f
        .get("assets")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .take(MAX_ASSETS)
        .filter_map(|(k, a)| {
            Some(Asset {
                key: k.chars().take(200).collect(),
                href: {
                    let h = a.get("href")?.as_str()?;
                    match &base {
                        Some(b) => resolve(b, h)?,
                        // Nothing to resolve a relative address against: whole ones only.
                        None => resolve("", h)?,
                    }
                },
                media_type: text(a.get("type")),
                roles: a
                    .get("roles")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .take(20)
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect(),
                title: text(a.get("title")),
                scale: band_number(a, "scale"),
                offset: band_number(a, "offset"),
                nodata: band_number(a, "nodata"),
            })
        })
        .collect();
    assets.sort_by(|a, b| a.key.cmp(&b.key));
    let properties = props.and_then(Value::as_object).map(|p| {
        // Shown, not used: the first hundred plain values, short text kept whole and long text cut.
        let kept: serde_json::Map<String, Value> = p
            .iter()
            .filter(|(_, v)| !v.is_object() && !v.is_array())
            .take(MAX_PROPERTIES)
            .map(|(k, v)| {
                let v = match v {
                    Value::String(t) if t.len() > 200 => Value::String(t.chars().take(200).collect()),
                    other => other.clone(),
                };
                (k.chars().take(100).collect(), v)
            })
            .collect();
        Value::Object(kept)
    });
    Some(Item {
        id,
        collection: text(f.get("collection")),
        datetime,
        bbox,
        geometry,
        cloud,
        assets,
        url,
        origin: from.map(str::to_string),
        properties,
        tile: props.and_then(tile_of),
    })
}

/// The grid tile an item's properties name (see [`Item::tile`]), as text of sane length.
fn tile_of(p: &Value) -> Option<String> {
    let plain = |v: &Value| match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Array(a) => a.first().and_then(|x| x.as_str()).map(str::to_string),
        _ => None,
    };
    let given = |k: &str| p.get(k).and_then(plain).filter(|t| !t.trim().is_empty());
    let wrs = || Some(format!("{}/{}", given("landsat:wrs_path")?, given("landsat:wrs_row")?));
    // The MGRS extension's square (`LS`) repeats in every zone and band: the three make the tile.
    let mgrs = || Some(format!("{}{}{}", given("mgrs:utm_zone")?, given("mgrs:latitude_band")?, given("mgrs:grid_square")?));
    // The first key with a value: one present but empty doesn't stop the others being tried.
    ["grid:code", "s2:mgrs_tile"]
        .iter()
        .find_map(|k| given(k))
        .or_else(mgrs)
        .or_else(|| ["bdc:tiles", "bdc:tile"].iter().find_map(|k| given(k)))
        .or_else(wrs)
        .map(|t| t.chars().take(64).collect())
}

/// An asset's first band's number `key` (`scale`, `offset`, `nodata`): from `raster:bands`, else from
/// `eo:bands`, where some catalogs (Brazil Data Cube's) put it.
fn band_number(asset: &Value, key: &str) -> Option<f64> {
    ["raster:bands", "eo:bands"].iter().find_map(|b| asset.get(*b)?.as_array()?.first()?.get(key)?.as_f64()).filter(|v| v.is_finite())
}

/// How many numbers a GeoJSON geometry holds (bounded by how deep it goes).
fn footprint_size(g: &Value) -> usize {
    fn count(v: &Value, depth: usize) -> usize {
        match v {
            // Saturating: an array nested too deep counts as too many, and many of them can't
            // wrap around to a small number.
            Value::Array(a) if depth < 6 => a.iter().fold(0usize, |n, x| n.saturating_add(count(x, depth + 1))),
            Value::Number(_) => 1,
            Value::Array(_) => usize::MAX / 4,
            _ => 0,
        }
    }
    g.get("coordinates").map(|c| count(c, 0)).unwrap_or(0)
}

/// An address relative to another, as a browser would resolve it (`s3://` and others are kept
/// as they are, so they can be refused by name).
pub fn resolve(base: &str, href: &str) -> Option<String> {
    // A login binding rides along only to an address on the base's own origin (`inherit`).
    let plain_base = rustgis_data::login_address::parse(base).ok()?.map_or(base, |b| b.url);
    rustgis_data::login_address::parse(href).ok()?;
    let resolved = resolve_plain(plain_base, crate::logins::inner(href))?;
    Some(crate::logins::inherit(base, &resolved))
}

fn resolve_plain(base: &str, href: &str) -> Option<String> {
    let href = href.trim();
    if href.is_empty() || href.len() > MAX_TEXT || href.chars().any(char::is_control) {
        return None;
    }
    if href.contains("://") {
        return Some(href.to_string());
    }
    let (scheme, rest) = base.split_once("://")?;
    let host_end = rest.find('/').unwrap_or(rest.len());
    let origin = format!("{scheme}://{}", rest.get(..host_end)?);
    if let Some(abs) = href.strip_prefix("//") {
        return Some(format!("{scheme}://{abs}"));
    }
    if href.starts_with('/') {
        return Some(format!("{origin}{href}"));
    }
    // Relative to the base's folder, with `./` and `../` worked out.
    let path = rest.get(host_end..).unwrap_or("").split(['?', '#']).next().unwrap_or("");
    let mut parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    if !path.ends_with('/') {
        parts.pop();
    }
    for seg in href.split('/') {
        match seg {
            "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    Some(format!("{origin}/{}", parts.join("/")))
}

impl Asset {
    /// Whether it is a GeoTIFF (cloud-optimised or not).
    pub fn is_geotiff(&self) -> bool {
        let t = self.media_type.to_ascii_lowercase();
        let path = crate::logins::inner(&self.href).split(['?', '#']).next().unwrap_or("").to_ascii_lowercase();
        t.contains("geotiff") || t.starts_with("image/tiff") || path.ends_with(".tif") || path.ends_with(".tiff")
    }

    /// Whether it is a LiDAR point cloud (LAS, LAZ or COPC).
    pub fn is_point_cloud(&self) -> bool {
        let t = self.media_type.to_ascii_lowercase();
        let path = crate::logins::inner(&self.href).split(['?', '#']).next().unwrap_or("").to_ascii_lowercase();
        t.contains("vnd.las") || t.contains("laszip") || path.ends_with(".laz") || path.ends_with(".las")
    }
}

impl Item {
    /// The asset `key` names, else the one a map most likely wants: `visual` (a true-colour
    /// image), else the first GeoTIFF, else the first data asset.
    pub fn pick(&self, key: Option<&str>) -> Result<&Asset, String> {
        if let Some(k) = key.map(str::trim).filter(|k| !k.is_empty()) {
            return self.assets.iter().find(|a| a.key == k).ok_or_else(|| {
                let keys: Vec<&str> = self.assets.iter().map(|a| a.key.as_str()).collect();
                format!("{}: no asset '{k}' (it has {})", self.id, keys.join(", "))
            });
        }
        // Among those on the web first (others are refused by name when nothing else is there).
        let web = |a: &&Asset| is_web(&a.href);
        let choose = |only_web: bool| {
            let ok = |a: &&Asset| !only_web || web(a);
            self.assets
                .iter()
                .filter(ok)
                .find(|a| a.key == "visual")
                .or_else(|| self.assets.iter().filter(ok).find(|a| a.is_geotiff()))
                .or_else(|| self.assets.iter().filter(ok).find(|a| a.is_point_cloud()))
                .or_else(|| self.assets.iter().filter(ok).find(|a| a.roles.iter().any(|r| r == "data")))
        };
        choose(true).or_else(|| choose(false)).ok_or_else(|| format!("{}: no asset RustGIS can add", self.id))
    }

    /// Its preview picture: the `thumbnail` asset (or one with that role, or a `preview`), a PNG
    /// or JPEG on the web.
    pub fn thumbnail(&self) -> Option<&Asset> {
        let picture = |a: &&Asset| {
            let t = a.media_type.to_ascii_lowercase();
            let path = crate::logins::inner(&a.href).split(['?', '#']).next().unwrap_or("").to_ascii_lowercase();
            is_web(&a.href)
                && (t.starts_with("image/png")
                    || t.starts_with("image/jpeg")
                    || t.starts_with("image/jpg")
                    || (t.is_empty() && [".png", ".jpg", ".jpeg"].iter().any(|e| path.ends_with(e))))
        };
        let named = |k: &str| self.assets.iter().filter(picture).find(|a| a.key == k);
        named("thumbnail")
            .or_else(|| self.assets.iter().filter(picture).find(|a| a.roles.iter().any(|r| r == "thumbnail")))
            .or_else(|| named("preview"))
            .or_else(|| self.assets.iter().filter(picture).find(|a| a.roles.iter().any(|r| r == "overview")))
    }

    /// The address of the asset to add (see [`Item::pick`]), checked: a plain web address the item
    /// may point to, not one that needs signing or a cloud store's own protocol.
    pub fn asset_href(&self, key: Option<&str>) -> Result<(String, String), String> {
        let a = self.pick(key)?;
        let lower = crate::logins::inner(&a.href).to_ascii_lowercase();
        if !(lower.starts_with("https://") || lower.starts_with("http://") || crate::s3::is_s3(&lower)) {
            let scheme = lower.split("://").next().unwrap_or("");
            return Err(format!(
                "{}: asset '{}' is at {} (a {scheme}:// address); RustGIS adds assets by their web or s3:// address",
                self.id, a.key, a.href
            ));
        }
        // Judged by where the item came from; an item given whole may only point at public
        // addresses (as a document on the web may). A Planetary Computer asset is signed by the
        // caller ([`needs_signing`]).
        crate::address::check(&a.href, Some(self.origin.as_deref().unwrap_or(UNTRUSTED)))?;
        Ok((a.key.clone(), a.href.clone()))
    }
}

/// The host of a web address, lower-cased.
fn host_of(url: &str) -> String {
    let url = crate::logins::inner(url);
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit_once('@').map(|(_, h)| h).unwrap_or(authority);
    host.split(':').next().unwrap_or(host).to_ascii_lowercase()
}

/// Whether an asset's address is in Azure Blob Storage without a signature: a Microsoft
/// Planetary Computer asset, read with a token from its token service ([`sas_token_url`]).
pub fn needs_signing(href: &str) -> bool {
    let lower = crate::logins::inner(href).to_ascii_lowercase();
    if !lower.starts_with("https://") || !host_of(&lower).ends_with(".blob.core.windows.net") {
        return false;
    }
    let query = lower.split('#').next().and_then(|u| u.split_once('?')).map(|(_, q)| q).unwrap_or("");
    !query.split('&').any(|kv| kv.split('=').next() == Some("sig"))
}

/// The request for a token that reads `href`'s storage container (the Planetary Computer's
/// `token/{account}/{container}`).
pub fn sas_token_url(href: &str) -> Result<String, String> {
    let lower = crate::logins::inner(href).to_ascii_lowercase();
    let host = host_of(&lower);
    let account = host.strip_suffix(".blob.core.windows.net").unwrap_or("");
    let path = lower.split_once("://").map(|(_, r)| r).unwrap_or("");
    let container = path.split(['?', '#']).next().unwrap_or("").split('/').nth(1).unwrap_or("");
    let ok =
        |s: &str, max: usize| (3..=max).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if !ok(account, 24) || !ok(container, 63) {
        return Err(format!("{href}: not an Azure Blob Storage address the Planetary Computer signs"));
    }
    Ok(format!("{SAS_API}/{account}/{container}"))
}

/// The token in the token service's answer (`{"token": "st=…&se=…&sig=…", "msft:expiry": …}`).
pub fn parse_sas(bytes: &[u8]) -> Result<String, String> {
    let v = serde_json::from_slice::<Value>(bytes).map_err(|_| "the Planetary Computer's token service didn't answer JSON".to_string())?;
    let token = v.get("token").and_then(Value::as_str).unwrap_or("").trim().trim_start_matches('?');
    // A query string's characters only: the token is pasted into addresses.
    let ok = !token.is_empty()
        && token.len() <= 4096
        && token.bytes().all(|b| b.is_ascii_alphanumeric() || b"=&%-_.:+/".contains(&b))
        && token.split('&').any(|kv| kv.starts_with("sig="));
    if !ok {
        return Err("the Planetary Computer's token service gave no usable token".into());
    }
    Ok(token.to_string())
}

/// An address with a token's query added.
pub fn sign(href: &str, token: &str) -> String {
    let inner = crate::logins::inner(href);
    let (base, frag) = inner.split_once('#').map(|(b, f)| (b, Some(f))).unwrap_or((inner, None));
    let sep = if base.contains('?') { "&" } else { "?" };
    let signed = match frag {
        Some(f) => format!("{base}{sep}{token}#{f}"),
        None => format!("{base}{sep}{token}"),
    };
    crate::logins::inherit(href, &signed)
}

fn tokens() -> &'static std::sync::Mutex<std::collections::HashMap<String, (String, web_time::Instant)>> {
    static T: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, (String, web_time::Instant)>>> =
        std::sync::OnceLock::new();
    T.get_or_init(Default::default)
}

/// A token fetched from `token_url` in the last half hour, if any.
pub fn cached_token(token_url: &str) -> Option<String> {
    let t = tokens().lock().unwrap_or_else(|e| e.into_inner());
    t.get(token_url).filter(|(_, at)| at.elapsed() < TOKEN_REUSE).map(|(tok, _)| tok.clone())
}

/// Keeps a token for reuse ([`cached_token`]).
pub fn remember_token(token_url: &str, token: &str) {
    let mut t = tokens().lock().unwrap_or_else(|e| e.into_inner());
    if t.len() > 64 {
        t.clear();
    }
    t.insert(token_url.to_string(), (token.to_string(), web_time::Instant::now()));
}

/// `href` signed if it needs it ([`needs_signing`]), waiting for the token service (the command
/// line; the app fetches tokens off the frame).
#[cfg(not(target_arch = "wasm32"))]
pub fn sign_blocking(href: &str) -> Result<String, String> {
    if !needs_signing(href) {
        return Ok(href.to_string());
    }
    let url = sas_token_url(href)?;
    let token = match cached_token(&url) {
        Some(t) => t,
        None => {
            let t = parse_sas(&crate::cache::download_blocking(&url)?)?;
            remember_token(&url, &t);
            t
        }
    };
    Ok(sign(href, &token))
}

/// An item as the commands return it (and read back in `stac.addAsset`), with its date and
/// cloud cover as its properties: small, for the command line and the AI Assistant.
pub fn item_json(i: &Item) -> Value {
    item_json_with(i, false)
}

/// [`item_json`], with every property kept when `all` (`stac.search`'s `"properties": true`).
pub fn item_json_with(i: &Item, all: bool) -> Value {
    // The date and cloud cover read are always there, whatever properties were kept.
    let kept = i.properties.as_ref().and_then(Value::as_object).filter(|_| all);
    let mut props = kept.cloned().unwrap_or_default();
    if props.get("datetime").is_none_or(Value::is_null) {
        props.insert("datetime".into(), Value::String(i.datetime.clone()));
    }
    if props.get("eo:cloud_cover").is_none_or(Value::is_null) {
        props.insert("eo:cloud_cover".into(), serde_json::json!(i.cloud));
    }
    serde_json::json!({
        "type": "Feature",
        "id": i.id,
        "collection": i.collection,
        "bbox": i.bbox,
        "geometry": i.geometry,
        "properties": props,
        "assets": i.assets.iter().map(|a| (a.key.clone(), serde_json::json!({"href": a.href, "type": a.media_type, "roles": a.roles, "title": a.title}))).collect::<serde_json::Map<_, _>>(),
        "links": i.url.as_ref().map(|u| vec![serde_json::json!({"rel": "self", "href": u})]).unwrap_or_default(),
    })
}

/// A GeoJSON FeatureCollection of items' footprints (their geometry, or their bbox), with the
/// id, collection, date, cloud cover and asset keys of each.
pub fn footprints_geojson(items: &[Item]) -> Value {
    let features: Vec<Value> = items
        .iter()
        .filter_map(|i| {
            let geometry = i.geometry.clone().or_else(|| {
                i.bbox.map(|[w, s, e, n]| serde_json::json!({"type": "Polygon", "coordinates": [[[w, s], [e, s], [e, n], [w, n], [w, s]]]}))
            })?;
            Some(serde_json::json!({
                "type": "Feature",
                "geometry": geometry,
                "properties": {
                    "id": i.id,
                    "collection": i.collection,
                    "datetime": i.datetime,
                    "cloud_cover": i.cloud,
                    "assets": i.assets.iter().map(|a| a.key.as_str()).collect::<Vec<_>>().join(", "),
                },
            }))
        })
        .collect();
    serde_json::json!({"type": "FeatureCollection", "features": features})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bound_stac_api_catalog_and_assets_follow_only_same_origin_links() {
        let _turn = crate::cache::turn();
        let origin = "https://stac-bound.test";
        // A binding is authority only while its runtime owns it, so install a live one.
        let token = crate::logins::activate(origin, "test-owner", crate::logins::Method::Header("X-Key".into()), "secret", false).unwrap();
        let base = rustgis_data::login_address::bind(&token, origin, "https://stac-bound.test/catalog.json").unwrap();
        let document = br#"{"type":"Catalog","links":[
            {"rel":"child","href":"collection.json"},
            {"rel":"child","href":"https://other.test/collection.json"}]}"#;
        let children = catalog::parse_children(document, &base).unwrap();
        assert!(children.iter().any(|c| rustgis_data::login_address::parse(&c.url).unwrap().is_some()));
        assert!(children.iter().any(|c| c.url == "https://other.test/collection.json"));
        let item = br#"{"type":"Feature","id":"x","properties":{},"geometry":null,
            "assets":{"data":{"href":"data.tif","type":"image/tiff"}},
            "links":[{"rel":"self","href":"items/x.json"}]}"#;
        let item = parse_item(item, &base).unwrap();
        assert_eq!(rustgis_data::login_address::parse(&item.assets[0].href).unwrap().unwrap().binding, token);
        let api = api(Some(&rustgis_data::login_address::bind(&token, origin, "https://stac-bound.test/v1/search").unwrap())).unwrap();
        for url in [collections_url(&api), search_url(&api, &Search::default()).unwrap()] {
            assert_eq!(rustgis_data::login_address::parse(&url).unwrap().unwrap().binding, token);
        }
        for foreign in ["http://stac-bound.test/tile", "https://stac-bound.test:444/tile", "https://other.test/tile"] {
            assert_eq!(resolve(&base, foreign).as_deref(), Some(foreign));
        }
        crate::logins::deactivate(&token);
    }

    #[test]
    fn api_of_a_linked_document() {
        // The address before /collections/.
        assert_eq!(api_of(b"{}", "https://x.org/v1/collections/s2/items/a").as_deref(), Some("https://x.org/v1"));
        // Else the root link (a static catalog's .json root is no API), else the parent collection's.
        let item = br#"{"links": [{"rel": "root", "href": "https://api.example.org/stac/v1/"}]}"#;
        assert_eq!(api_of(item, "https://cdn.example.org/items/a.json").as_deref(), Some("https://api.example.org/stac/v1"));
        let static_root = br#"{"links": [{"rel": "root", "href": "../catalog.json"},
            {"rel": "parent", "href": "https://api.example.org/v1/collections/s2"}]}"#;
        assert_eq!(api_of(static_root, "https://cdn.example.org/s2/a.json").as_deref(), Some("https://api.example.org/v1"));
        assert_eq!(api_of(br#"{"links": [{"rel": "root", "href": "catalog.json"}]}"#, "https://cdn.example.org/a.json"), None);
    }

    #[test]
    fn deep_footprints_saturate() {
        // Many arrays nested too deep: counted as too many, never wrapped to a small number.
        let deep = serde_json::json!([[[[[[[1]]]]]]]);
        let g = serde_json::json!({"type": "Polygon", "coordinates": vec![deep; 8]});
        assert_eq!(footprint_size(&g), usize::MAX);
    }

    #[test]
    fn requests() {
        assert_eq!(api(None).unwrap(), EARTH_SEARCH);
        assert_eq!(api(Some(" https://x.org/stac/v1/search?x=1 ")).unwrap(), "https://x.org/stac/v1");
        assert_eq!(api(Some("https://x.org/stac/collections/")).unwrap(), "https://x.org/stac");
        assert!(api(Some("file:///etc")).is_err() && api(Some("http://169.254.169.254/")).is_err());
        let s = Search {
            collections: vec!["sentinel-2-l2a".into(), " ".into()],
            bbox: Some([7.0, 46.0, 7.1, 46.1]),
            datetime: Some("2024-06-01/2024-06-30".into()),
            limit: 999,
            max_cloud: Some(20.0),
            sortby: None,
        };
        let u = search_url("https://x.org/v1", &s).unwrap();
        assert_eq!(
            u,
            "https://x.org/v1/search?limit=200&collections=sentinel-2-l2a&bbox=7,46,7.1,46.1&datetime=2024-06-01T00%3A00%3A00Z%2F2024-06-30T23%3A59%3A59Z&query=%7B%22eo%3Acloud_cover%22%3A%7B%22lte%22%3A20%7D%7D"
        );
        let sorted = |by: &str| search_url("https://x.org/v1", &Search { sortby: Some(by.into()), ..Default::default() });
        assert!(sorted("eo:cloud_cover").unwrap().ends_with("&sortby=%2Bproperties.eo%3Acloud_cover"));
        assert!(sorted("-datetime").unwrap().ends_with("&sortby=-properties.datetime"));
        assert!(sorted("a&b=c").is_err() && sorted("-").is_err());
        assert_eq!(datetime("2024-06-01").unwrap(), "2024-06-01T00:00:00Z/2024-06-01T23:59:59Z");
        assert_eq!(datetime("2024-06-01/..").unwrap(), "2024-06-01T00:00:00Z/..");
        assert!(datetime("yesterday").is_err());
        assert!(search_url("https://x.org", &Search { bbox: Some([0.0, 50.0, 1.0, 40.0]), ..Default::default() }).is_err());
    }

    #[test]
    fn answers() {
        let cols =
            br#"{"collections": [{"id": "sentinel-2-l2a", "title": "Sentinel-2 Level-2A"}, {"title": "no id"}, {"id": "cop-dem-glo-30"}]}"#;
        let c = parse_collections(cols).unwrap();
        assert_eq!(c.len(), 2);
        assert_eq!((c[0].id.as_str(), c[0].title.as_str()), ("sentinel-2-l2a", "Sentinel-2 Level-2A"));
        assert!(parse_collections(b"[]").is_err() && parse_collections(b"<html>").is_err());
        let page = br#"{"type": "FeatureCollection", "features": [
            {"type": "Feature", "id": "S2A_1", "collection": "sentinel-2-l2a", "bbox": [7, 46, 8, 47],
             "geometry": {"type": "Polygon", "coordinates": [[[7, 46], [8, 46], [8, 47], [7, 47], [7, 46]]]},
             "properties": {"datetime": "2024-06-02T10:30:00Z", "eo:cloud_cover": 3.5},
             "links": [{"rel": "self", "href": "/v1/collections/sentinel-2-l2a/items/S2A_1"}],
             "assets": {"visual": {"href": "https://data.org/S2A_1/TCI.tif", "type": "image/tiff; application=geotiff; profile=cloud-optimized", "roles": ["visual"]},
                        "thumbnail": {"href": "./thumb.jpg", "type": "image/jpeg", "roles": ["thumbnail"]},
                        "b04": {"href": "s3://bucket/B04.tif", "type": "image/tiff; application=geotiff", "roles": ["data"]}}},
            {"type": "Feature", "id": "bad", "bbox": [0, 95, 1, 96], "properties": {}},
            {"type": "Nope", "id": "x"}],
            "links": [{"rel": "next", "href": "https://x.org/v1/search?token=abc"}]}"#;
        let (items, next) = parse_items(page, "https://x.org/v1/search?limit=2").unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(next.as_deref(), Some("https://x.org/v1/search?token=abc"));
        assert_eq!(parse_matched(page), None);
        assert_eq!(parse_matched(br#"{"numberMatched": 1234}"#), Some(1234));
        assert_eq!(parse_matched(br#"{"context": {"matched": 7}}"#), Some(7));
        assert_eq!(parse_matched(br#"{"numberMatched": -1}"#), None);
        let i = &items[0];
        assert_eq!((i.datetime.as_str(), i.cloud, i.bbox), ("2024-06-02T10:30:00Z", Some(3.5), Some([7.0, 46.0, 8.0, 47.0])));
        assert_eq!(i.url.as_deref(), Some("https://x.org/v1/collections/sentinel-2-l2a/items/S2A_1"));
        assert_eq!(
            i.assets.iter().find(|a| a.key == "thumbnail").unwrap().href,
            "https://x.org/v1/collections/sentinel-2-l2a/items/thumb.jpg"
        );
        assert_eq!(items[1].bbox, None, "a bbox off the globe is dropped");
        // The asset added: the one named, else visual; an s3:// address is kept (fetched through
        // the S3 connection serving its bucket, or anonymously from AWS).
        assert_eq!(i.asset_href(None).unwrap().0, "visual");
        // The thumbnail: by key, a picture on the web.
        assert_eq!(i.thumbnail().unwrap().key, "thumbnail");
        assert!(items[1].thumbnail().is_none());
        assert_eq!(i.properties.as_ref().unwrap()["eo:cloud_cover"], 3.5);
        assert_eq!(i.asset_href(Some("b04")).unwrap().1, "s3://bucket/B04.tif");
        assert!(i.asset_href(Some("nope")).unwrap_err().contains("visual"));
        let mut pc = i.clone();
        pc.assets = vec![Asset {
            key: "B04".into(),
            href: "https://sentinel2l2a01.blob.core.windows.net/sentinel2-l2/B04.tif".into(),
            media_type: String::new(),
            roles: vec![],
            title: String::new(),
            scale: None,
            offset: None,
            nodata: None,
        }];
        // A Planetary Computer asset is added after signing it with its container's token.
        let (_, href) = pc.asset_href(None).unwrap();
        assert!(needs_signing(&href));
        assert_eq!(sas_token_url(&href).unwrap(), "https://planetarycomputer.microsoft.com/api/sas/v1/token/sentinel2l2a01/sentinel2-l2");
        let token =
            parse_sas(br#"{"msft:expiry": "2026-10-13T14:00:00Z", "token": "st=2026&se=2026&sp=rl&sv=2024&sr=c&sig=a%2Bb%3D"}"#).unwrap();
        let signed = sign(&href, &token);
        assert_eq!(
            signed,
            "https://sentinel2l2a01.blob.core.windows.net/sentinel2-l2/B04.tif?st=2026&se=2026&sp=rl&sv=2024&sr=c&sig=a%2Bb%3D"
        );
        assert!(!needs_signing(&signed) && !needs_signing("https://data.org/x.tif"));
        assert!(needs_signing("https://a.blob.core.windows.net/c/x.tif?mysig=1"));
        // Tokens that aren't a plain query, and addresses that aren't a storage container's.
        for bad in [&br#"{"token": "sig=x\"<script>"}"#[..], br#"{"token": "st=1"}"#, b"[]", b"<html>"] {
            assert!(parse_sas(bad).is_err());
        }
        assert!(sas_token_url("https://evil.org/x.blob.core.windows.net/c/x.tif").is_err());
        assert!(sas_token_url("https://a.blob.core.windows.net/").is_err());
        remember_token("k", "sig=1");
        assert_eq!(cached_token("k").as_deref(), Some("sig=1"));
        // A web item can't point into the local network.
        let mut local = i.clone();
        local.assets[2].href = "http://192.168.1.2/x.tif".into();
        assert!(local.asset_href(Some("visual")).is_err());
        // Written out and read back.
        let again = item_from_json(&item_json(i), i.origin.as_deref()).unwrap();
        assert_eq!(&again, i);
        // A date read from start_datetime is written out too.
        let ranged = serde_json::json!({"type": "Feature", "id": "r", "properties": {"start_datetime": "2024-01-01T00:00:00Z", "eo:cloud_cover": null}});
        let r = item_from_json(&ranged, None).unwrap();
        let out = item_json(&r);
        // Only the date and cloud cover, unless every property is asked for.
        assert_eq!(item_json(i)["properties"].as_object().map(|p| p.len()), Some(2));
        let mut wide = i.clone();
        wide.properties = Some(serde_json::json!({"gsd": 10, "platform": "sentinel-2a"}));
        assert_eq!(item_json_with(&wide, true)["properties"]["platform"], "sentinel-2a");
        assert!(item_json(&wide)["properties"].get("platform").is_none());
        assert_eq!(out["properties"]["datetime"], "2024-01-01T00:00:00Z");
        assert_eq!(item_from_json(&out, None).unwrap().datetime, r.datetime);
        assert_eq!(footprints_geojson(&items)["features"].as_array().unwrap().len(), 1);
        assert!(parse_items(b"{}", "https://x.org").is_err());
    }

    #[test]
    fn assets_are_judged_by_where_items_came_from() {
        let f = |self_link: &str, href: &str| {
            serde_json::json!({"type": "Feature", "id": "a", "links": [{"rel": "self", "href": self_link}],
                "assets": {"visual": {"href": href, "type": "image/tiff; application=geotiff"}}})
        };
        // A web answer's own self link can't make a local asset trusted.
        let v = f("http://192.168.1.2/item.json", "http://192.168.1.2/x.tif");
        let i = item_from_json(&v, Some("https://stac.example.org/search")).unwrap();
        assert!(i.asset_href(None).is_err());
        // Fetched from a local server, its local assets are fine.
        let i = item_from_json(&v, Some("http://192.168.1.2/search")).unwrap();
        assert!(i.asset_href(None).is_ok());
        // An item given whole: public assets only, and nothing relative to resolve against.
        let i = item_from_json(&f("item.json", "http://127.0.0.1/x.tif"), None).unwrap();
        assert!(i.url.is_none() && i.asset_href(None).is_err());
        let i = item_from_json(&f("item.json", "https://data.example.org/x.tif"), None).unwrap();
        assert!(i.asset_href(None).is_ok());
        assert!(item_from_json(&f("item.json", "x.tif"), None).unwrap().assets.is_empty());
        // Dates: each side a date, a date and time, or open.
        for good in ["2024-06-01", "2024-06-01/2024-06-30", "2024-06-01/..", "../2024-06-30", "2024-06-01T10:00:00Z/.."] {
            assert!(datetime(good).is_ok(), "{good}");
        }
        for bad in ["2024-06-01 2024-06-30", "--/--", "2024", "a/b", "2024-06-01/2024-06-30/2024-07-01", "/"] {
            assert!(datetime(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn relative_addresses() {
        assert_eq!(resolve("https://x.org/a/b/item.json", "../c/d.tif").as_deref(), Some("https://x.org/a/c/d.tif"));
        assert_eq!(resolve("https://x.org/a/b/", "./d.tif").as_deref(), Some("https://x.org/a/b/d.tif"));
        assert_eq!(resolve("https://x.org/a/b", "/d.tif").as_deref(), Some("https://x.org/d.tif"));
        assert_eq!(resolve("https://x.org/a", "//y.org/d.tif").as_deref(), Some("https://y.org/d.tif"));
        assert_eq!(resolve("https://x.org/a", "s3://b/k").as_deref(), Some("s3://b/k"));
        assert_eq!(resolve("https://x.org/a", ""), None);
    }

    /// A data cube's tile and a band's scale (#559): the grid codes catalogs use, and
    /// `raster:bands` before `eo:bands`.
    #[test]
    fn tiles_and_band_scales() {
        let p = |v: Value| tile_of(&v);
        assert_eq!(p(serde_json::json!({"grid:code": "MGRS-32TLS"})).as_deref(), Some("MGRS-32TLS"));
        assert_eq!(p(serde_json::json!({"s2:mgrs_tile": "32TLS"})).as_deref(), Some("32TLS"));
        assert_eq!(p(serde_json::json!({"bdc:tiles": ["020019"]})).as_deref(), Some("020019"));
        assert_eq!(p(serde_json::json!({"landsat:wrs_path": "220", "landsat:wrs_row": 71})).as_deref(), Some("220/71"));
        assert_eq!(p(serde_json::json!({"platform": "sentinel-2a"})), None);
        // The MGRS extension: the square alone repeats from zone to zone, so it is never the tile.
        let square = |zone: &str| serde_json::json!({"mgrs:utm_zone": zone, "mgrs:latitude_band": "T", "mgrs:grid_square": "LS"});
        assert_eq!(p(square("32")).as_deref(), Some("32TLS"));
        assert_eq!(p(square("33")).as_deref(), Some("33TLS"));
        assert_eq!(p(serde_json::json!({"mgrs:grid_square": "LS"})), None);
        // An empty code doesn't hide the next one.
        assert_eq!(p(serde_json::json!({"grid:code": "", "bdc:tiles": ["020019"]})).as_deref(), Some("020019"));
        let a = serde_json::json!({"raster:bands": [{"scale": 0.0001, "offset": -0.1}], "eo:bands": [{"scale": 2.0}]});
        assert_eq!((band_number(&a, "scale"), band_number(&a, "offset")), (Some(0.0001), Some(-0.1)));
        assert_eq!(band_number(&serde_json::json!({"eo:bands": [{"scale": 0.0001}]}), "scale"), Some(0.0001));
        assert_eq!(band_number(&serde_json::json!({"type": "image/tiff"}), "scale"), None);
        assert_eq!(band_number(&serde_json::json!({"raster:bands": [{"nodata": -3000.0}]}), "nodata"), Some(-3000.0));
    }
}
