//! NASA Earthdata through Earthaccess's way of searching it: NASA's Common Metadata Repository
//! (CMR) for collections by keyword, then a collection's granules where the map is and between
//! two dates, their footprints and data links. Requests are built and answers read here (sans
//! I/O); the app fetches them off the frame, the command line waits for them.
//!
//! Searching needs no sign-in. Most files need an Earthdata Login token: [`set_token`] keeps one
//! in memory for the session (never in a project), and [`authorization`] gives it only to HTTPS
//! addresses on NASA's Earthdata data hosts (`*.earthdatacloud.nasa.gov`, `*.earthdata.nasa.gov`),
//! never to the login host (`urs.earthdata.nasa.gov`). The native fetcher asks for it at every
//! hop, so a redirect that leaves those hosts (to a presigned CloudFront or S3 address) goes
//! without it, and a redirect to the login host means the token was refused ([`is_login`]).
//!
//! A token may also be remembered on this computer ([`login`] with `remember`): it goes to the
//! credential store [`set_secret_store`] gives (the desktop's keychain; the command line's and
//! Python's when they opt in to saved secrets), at [`ACCOUNT`], and is read back the first time
//! it is needed ([`restore`]): by a request to a data host, or by the app asking. An expired token
//! is forgotten, there and in the store, and the user is asked for a new one ([`expired_forgotten`]).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rustgis_data::secrets::{Secret, SecretStore};

use serde_json::{Value, json};

/// NASA's Common Metadata Repository search API.
pub const CMR: &str = "https://cmr.earthdata.nasa.gov/search";
/// The login host: never sent the token; a redirect there means it was refused.
pub const LOGIN_HOST: &str = "urs.earthdata.nasa.gov";
/// The page where a token is made (Earthdata Login › Generate Token).
pub const TOKEN_PAGE: &str = "https://urs.earthdata.nasa.gov/documentation/for_users/user_token";
/// The most collections or granules a page asks for.
pub const MAX_PAGE: usize = 100;
/// The furthest page asked for (CMR pages by number up to 1 million hits).
pub const MAX_PAGE_NUM: usize = 1000;
/// The most granules read from an answer, and the most points a footprint keeps.
const MAX_ITEMS: usize = 2000;
const MAX_POINTS: usize = 10_000;
/// The longest token kept (Earthdata Login's JWTs are about 1 kB).
const MAX_TOKEN: usize = 16 << 10;

/// Popular datasets: CMR short name and title.
pub const PRESETS: &[(&str, &str)] = &[
    ("ATL03", "ICESat-2 ATL03 Geolocated Photons"),
    ("ATL06", "ICESat-2 ATL06 Land Ice Height"),
    ("ATL08", "ICESat-2 ATL08 Land and Vegetation Height"),
    ("GEDI02_A", "GEDI L2A Elevation and Height Metrics"),
    ("GEDI02_B", "GEDI L2B Canopy Cover and Vertical Profile"),
    ("GEDI_L4A_AGB_Density_V2_1_2056", "GEDI L4A Aboveground Biomass Density"),
    ("HLSL30", "HLS Landsat 30 m Surface Reflectance"),
    ("HLSS30", "HLS Sentinel-2 30 m Surface Reflectance"),
    ("NASADEM_HGT", "NASADEM Elevation"),
];

fn token_store() -> &'static Mutex<Option<String>> {
    static T: Mutex<Option<String>> = Mutex::new(None);
    &T
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// An immutable, exact active-runtime token snapshot. It never reads the credential store
/// or expires/restores a token, and has no formatting, serialization or raw-value accessors.
#[derive(Clone, PartialEq, Eq)]
pub struct AccountIdentity {
    relevant: bool,
    token: Option<String>,
}

impl AccountIdentity {
    /// Checks exact active in-memory identity, without restoration or any other I/O.
    /// An unavailable or poisoned relevant store returns false.
    pub fn is_current(&self) -> bool {
        identity_in(token_store(), self.relevant).is_ok_and(|current| *self == current)
    }
}

/// Snapshots the active token only; an unread remembered token remains unread.
/// An unavailable or poisoned store is an error, never an absent token.
pub fn account_identity() -> Result<AccountIdentity, String> {
    identity_in(token_store(), true)
}

fn identity_in(store: &Mutex<Option<String>>, relevant: bool) -> Result<AccountIdentity, String> {
    let token = if relevant { store.lock().map_err(|_| "active account store unavailable".to_string())?.clone() } else { None };
    Ok(AccountIdentity { relevant, token })
}

/// Snapshots the token for a data host, including its absence. Public/search/login and
/// unrelated addresses do not acquire a dependency on the active token.
/// An unavailable or poisoned relevant store is an error, never an absent token.
pub fn account_identity_for(url: &str) -> Result<AccountIdentity, String> {
    identity_in(token_store(), is_data_host(url))
}

/// A token as given (`None` when blank): one word of JWT characters, without `Bearer `; anything
/// else is refused, so it can't break the header it goes in.
fn checked(token: &str) -> Result<Option<String>, String> {
    let token = token.trim();
    if token.is_empty() {
        return Ok(None);
    }
    let t = token.strip_prefix("Bearer ").map(str::trim).unwrap_or(token);
    if t.is_empty() || t.len() > MAX_TOKEN || !t.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.~+/=".contains(&b)) {
        return Err("an Earthdata Login token is one word of letters, digits and - _ . ~ + / = (Generate Token on Earthdata Login)".into());
    }
    Ok(Some(t.to_string()))
}

/// Keeps an Earthdata Login token for the session (`None` forgets it), never remembering it on
/// this computer: [`login`] and [`logout`] do that.
pub fn set_token(token: Option<&str>) -> Result<(), String> {
    let token = match token {
        Some(t) => checked(t)?,
        None => None,
    };
    *lock(token_store()) = token;
    Ok(())
}

/// Whether a token is kept.
pub fn has_token() -> bool {
    lock(token_store()).is_some()
}

/// When the token kept expires (Unix seconds), as its JWT `exp` claim says, if it says.
pub fn token_expiry() -> Option<i64> {
    lock(token_store()).as_deref().and_then(jwt_expiry)
}

fn now_secs() -> i64 {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Whether a token has expired at `now`, as its JWT says (a token that doesn't say never has).
fn expired(token: &str, now: i64) -> bool {
    jwt_expiry(token).is_some_and(|e| e <= now)
}

/// The credential store's account of the token remembered on this computer.
pub const ACCOUNT: &str = "earthdata/token";
/// [`login`] with `remember` where there is no credential store (the browser; the command line
/// and Python unless they opt in to saved secrets).
pub const NO_STORE: &str = "The token is kept for this session only: there is no credential store here to remember it in.";
/// [`login`] with `remember`, when the credential store didn't keep the token.
pub const NOT_REMEMBERED: &str = "The token is kept for this session, but the system's credential store didn't remember it.";
/// The credential store didn't delete the token remembered earlier.
pub const NOT_FORGOTTEN: &str = "The token remembered on this computer couldn't be deleted from the system's credential store.";

/// The token remembered on this computer: where it is kept, and what this session knows of it.
struct Remembered {
    store: Mutex<Option<Arc<dyn SecretStore>>>,
    /// Whether the store was read, or needn't be (a token was given or forgotten this session).
    /// Held while the store is used, so one reading serves every request waiting for it.
    read: Mutex<bool>,
    /// Whether the store holds a token: the session's, unless a deletion failed.
    kept: AtomicBool,
    /// Whether an expired token was forgotten, until another is given or forgotten.
    expired: AtomicBool,
}

static REMEMBERED: Remembered =
    Remembered { store: Mutex::new(None), read: Mutex::new(false), kept: AtomicBool::new(false), expired: AtomicBool::new(false) };

fn secret_store() -> Option<Arc<dyn SecretStore>> {
    lock(&REMEMBERED.store).clone()
}

/// Gives the credential store a token is remembered in (the desktop's keychain; the command
/// line's and Python's when they opt in to saved secrets). It is read when first needed.
pub fn set_secret_store(store: Option<Arc<dyn SecretStore>>) {
    let mut read = lock(&REMEMBERED.read);
    *lock(&REMEMBERED.store) = store;
    *read = false;
    REMEMBERED.kept.store(false, Ordering::SeqCst);
}

/// What reading the remembered token found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Restored {
    /// Nothing new: no store, nothing remembered, already read, or a token given this session.
    Nothing,
    /// The remembered token is kept for the session.
    Token,
    /// The remembered token had expired, and was deleted: the user is to give a new one.
    Expired,
}

/// Reads the token remembered on this computer, once per store given. A token given or forgotten
/// this session first wins: the store then only tells whether it holds that token (another session
/// of the process may have remembered it). An expired or damaged one is deleted. Reading may ask
/// the system to unlock its store: the app calls this off the frame (requests to data hosts call
/// it themselves).
pub fn restore() -> Restored {
    let mut read = lock(&REMEMBERED.read);
    if *read {
        return Restored::Nothing;
    }
    *read = true;
    let Some(store) = secret_store() else { return Restored::Nothing };
    let Ok(Some(secret)) = store.load(ACCOUNT) else { return Restored::Nothing };
    if let Some(session) = lock(token_store()).as_deref() {
        REMEMBERED.kept.store(checked(secret.expose()).ok().flatten().as_deref() == Some(session), Ordering::SeqCst);
        return Restored::Nothing;
    }
    match checked(secret.expose()) {
        Ok(Some(token)) if !expired(&token, now_secs()) => {
            *lock(token_store()) = Some(token);
            REMEMBERED.kept.store(true, Ordering::SeqCst);
            Restored::Token
        }
        Ok(Some(_)) => {
            REMEMBERED.kept.store(store.delete(ACCOUNT).is_err(), Ordering::SeqCst);
            REMEMBERED.expired.store(true, Ordering::SeqCst);
            Restored::Expired
        }
        // Damaged: of no use to anyone.
        _ => {
            REMEMBERED.kept.store(store.delete(ACCOUNT).is_err(), Ordering::SeqCst);
            Restored::Nothing
        }
    }
}

/// Forgets the session's token when it has expired, and the remembered one with it; whether it
/// did. The user is then asked for a new one ([`expired_forgotten`]).
pub fn forget_if_expired() -> bool {
    let mut read = lock(&REMEMBERED.read);
    let gone = {
        let mut token = lock(token_store());
        let gone = token.as_deref().is_some_and(|t| expired(t, now_secs()));
        if gone {
            *token = None;
        }
        gone
    };
    if gone {
        *read = true;
        REMEMBERED.expired.store(true, Ordering::SeqCst);
        if REMEMBERED.kept.load(Ordering::SeqCst)
            && let Some(store) = secret_store()
        {
            REMEMBERED.kept.store(store.delete(ACCOUNT).is_err(), Ordering::SeqCst);
        }
    }
    gone
}

/// Keeps `token` for the session and, with `remember`, on this computer, replacing the one
/// remembered before; without it, a token remembered before is deleted. Returns a warning when
/// the token is kept for the session only against the wish to remember it, or the one remembered
/// before couldn't be deleted.
pub fn login(token: &str, remember: bool) -> Result<Option<&'static str>, String> {
    let token = checked(token)?.ok_or("token is required (Earthdata Login › Generate Token)")?;
    let mut read = lock(&REMEMBERED.read);
    *read = true;
    REMEMBERED.expired.store(false, Ordering::SeqCst);
    *lock(token_store()) = Some(token.clone());
    let Some(store) = secret_store() else { return Ok(remember.then_some(NO_STORE)) };
    let kept = REMEMBERED.kept.load(Ordering::SeqCst);
    if remember && store.save(ACCOUNT, &Secret::new(token)).is_ok() {
        REMEMBERED.kept.store(true, Ordering::SeqCst);
        return Ok(None);
    }
    // Not to be remembered, or the store didn't take it: the one remembered before goes, here or
    // in another run, so the next run doesn't bring it back.
    let deleted = store.delete(ACCOUNT);
    REMEMBERED.kept.store(kept && deleted.is_err(), Ordering::SeqCst);
    match deleted {
        _ if remember => Ok(Some(NOT_REMEMBERED)),
        Ok(()) => Ok(None),
        // A system without a credential store had nowhere to keep one.
        Err(rustgis_data::secrets::SecretError::Unavailable(_)) if !kept => Ok(None),
        Err(_) => Ok(Some(NOT_FORGOTTEN)),
    }
}

/// Forgets the token, for the session and on this computer; `Err` when the credential store
/// couldn't delete it.
pub fn logout() -> Result<(), &'static str> {
    let mut read = lock(&REMEMBERED.read);
    *read = true;
    REMEMBERED.expired.store(false, Ordering::SeqCst);
    *lock(token_store()) = None;
    // Deleted even when this session didn't read it: forgetting is asked for. A system without
    // a credential store had nowhere to keep one, unless this session saw it there.
    match secret_store().map_or(Ok(()), |store| store.delete(ACCOUNT)) {
        Ok(()) => {
            REMEMBERED.kept.store(false, Ordering::SeqCst);
            Ok(())
        }
        Err(rustgis_data::secrets::SecretError::Unavailable(_)) if !REMEMBERED.kept.load(Ordering::SeqCst) => Ok(()),
        Err(_) => Err(NOT_FORGOTTEN),
    }
}

/// Whether a token is remembered on this computer.
pub fn is_remembered() -> bool {
    REMEMBERED.kept.load(Ordering::SeqCst)
}

/// Whether an expired token was forgotten, and no other was given since.
pub fn expired_forgotten() -> bool {
    REMEMBERED.expired.load(Ordering::SeqCst) && !has_token()
}

/// A JWT's `exp` claim: its middle part, base64url-decoded, is JSON.
fn jwt_expiry(token: &str) -> Option<i64> {
    let payload = token.split('.').nth(1)?;
    let v: Value = serde_json::from_slice(&base64url(payload)?).ok()?;
    v.get("exp")?.as_i64()
}

fn base64url(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes().take(MAX_TOKEN) {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            b'=' => break,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}

/// The host of an address the token may go to, lowercase: HTTPS, no user info or port.
fn https_host(url: &str) -> Option<String> {
    let rest = url.get(..8).filter(|s| s.eq_ignore_ascii_case("https://")).and_then(|_| url.get(8..))?;
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.is_empty() || authority.contains(['@', ':', '[', '\\']) {
        return None;
    }
    Some(authority.trim_end_matches('.').to_ascii_lowercase())
}

fn within(host: &str, suffix: &str) -> bool {
    host == suffix || host.strip_suffix(suffix).is_some_and(|h| h.ends_with('.'))
}

/// Earthdata's public services, which never need the token: the searches, Earthdata Search,
/// GIBS imagery tiles and the Earthdata GIS portal.
const PUBLIC_HOSTS: &[&str] = &["cmr.earthdata.nasa.gov", "search.earthdata.nasa.gov", "gibs.earthdata.nasa.gov", "gis.earthdata.nasa.gov"];

/// Whether `url` is on an Earthdata data host, the only place the token is sent: HTTPS on
/// `*.earthdatacloud.nasa.gov` or `*.earthdata.nasa.gov`, never a login host (`*urs.…`) or a public
/// service ([`PUBLIC_HOSTS`]).
pub fn is_data_host(url: &str) -> bool {
    https_host(url).is_some_and(|h| {
        !within(&h, LOGIN_HOST)
            && !PUBLIC_HOSTS.iter().any(|p| within(&h, p))
            && (within(&h, "earthdatacloud.nasa.gov") || within(&h, "earthdata.nasa.gov"))
    })
}

/// Whether `url` is Earthdata Login (where a data host sends a request it didn't accept).
pub fn is_login(url: &str) -> bool {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h).split(':').next().unwrap_or("");
    within(&host.trim_end_matches('.').to_ascii_lowercase(), LOGIN_HOST)
}

/// The `Authorization` header a request to `url` carries: the token kept, for a data host only.
/// The first such request reads the token remembered on this computer ([`restore`]); an expired
/// token is forgotten instead of sent ([`forget_if_expired`]).
pub fn authorization(url: &str) -> Option<String> {
    if !is_data_host(url) {
        return None;
    }
    restore();
    forget_if_expired();
    lock(token_store()).as_deref().map(|t| format!("Bearer {t}"))
}

/// The message for a request Earthdata Login turned away.
pub fn refused_message(url: &str) -> String {
    let file = url.split(['?', '#']).next().unwrap_or(url).rsplit('/').next().unwrap_or("");
    if has_token() {
        format!(
            "{file}: Earthdata Login didn't accept the token (it may have expired, or the dataset needs its EULA accepted on Earthdata Login)"
        )
    } else if expired_forgotten() {
        format!(
            "{file}: the Earthdata Login token expired and was forgotten: give a new one in Web Data › NASA Earthaccess (or earthdata.login)"
        )
    } else {
        format!(
            "{file}: NASA asks for an Earthdata Login token for this file: give one in Web Data › NASA Earthaccess (or earthdata.login)"
        )
    }
}

/// A CMR collection.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Collection {
    /// Its concept id (`C…-PROVIDER`).
    pub id: String,
    pub short_name: String,
    pub version: String,
    pub title: String,
    pub data_center: String,
    pub cloud_hosted: bool,
    pub time_start: String,
    pub time_end: String,
    pub summary: String,
}

/// A granule (a file or a set of files of a collection).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Granule {
    /// Its concept id (`G…-PROVIDER`).
    pub id: String,
    pub title: String,
    pub collection: String,
    pub time_start: String,
    pub time_end: String,
    /// Its size in MB, as CMR gives it.
    pub size_mb: Option<f64>,
    pub cloud_cover: Option<f64>,
    /// Its data files' addresses.
    pub data: Vec<String>,
    /// Its preview images' addresses.
    pub browse: Vec<String>,
    /// Its footprint, a GeoJSON geometry.
    pub geometry: Option<Value>,
}

impl Granule {
    /// Its GeoTIFF data files (added to the map as COGs).
    pub fn tiffs(&self) -> impl Iterator<Item = &String> {
        self.data.iter().filter(|u| is_tiff(u))
    }

    /// Its page on Earthdata Search.
    pub fn details_url(&self) -> String {
        format!("https://search.earthdata.nasa.gov/search/granules?p={}&g={}", encode(&self.collection), encode(&self.id))
    }
}

/// Whether an address is a GeoTIFF's.
pub fn is_tiff(url: &str) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or(url).to_ascii_lowercase();
    path.ends_with(".tif") || path.ends_with(".tiff")
}

fn encode(s: &str) -> String {
    crate::s3::encode(s, true)
}

/// The text of a field, trimmed and capped.
fn text(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::String(s)) => s.trim().chars().take(4000).collect(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        _ => String::new(),
    }
}

fn number(v: &Value, k: &str) -> Option<f64> {
    match v.get(k)? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
    .filter(|x: &f64| x.is_finite())
}

/// A collection search: by keyword (free text) or short name, optionally cloud-hosted only.
pub fn collections_url(keyword: &str, short_name: Option<&str>, cloud_hosted: bool, page_size: usize, page: usize) -> String {
    let mut q = vec![("has_granules", "true".to_string())];
    let keyword = keyword.trim();
    if !keyword.is_empty() {
        q.push(("keyword", keyword.chars().take(500).collect()));
    }
    if let Some(sn) = short_name.map(str::trim).filter(|s| !s.is_empty()) {
        q.push(("short_name", sn.chars().take(200).collect()));
    }
    if cloud_hosted {
        q.push(("cloud_hosted", "true".into()));
    }
    q.push(("page_size", page_size.clamp(1, MAX_PAGE).to_string()));
    q.push(("page_num", page.clamp(1, MAX_PAGE_NUM).to_string()));
    let query: Vec<String> = q.iter().map(|(k, v)| format!("{}={}", encode(k), encode(v))).collect();
    format!("{CMR}/collections.json?{}", query.join("&"))
}

/// The entries of a CMR answer (`feed.entry`).
fn entries(bytes: &[u8]) -> Result<Vec<Value>, String> {
    let v: Value = serde_json::from_slice(bytes).map_err(|e| format!("CMR's answer isn't JSON: {e}"))?;
    if let Some(errors) = v.get("errors").and_then(Value::as_array) {
        let msg: Vec<String> = errors.iter().take(3).filter_map(Value::as_str).map(|s| s.chars().take(300).collect()).collect();
        return Err(format!("CMR: {}", msg.join("; ")));
    }
    let entry = v.get("feed").and_then(|f| f.get("entry")).ok_or("CMR's answer has no entries (feed.entry)")?;
    Ok(entry.as_array().map(|a| a.iter().take(MAX_ITEMS).cloned().collect()).unwrap_or_default())
}

/// The collections a CMR answer lists.
pub fn parse_collections(bytes: &[u8]) -> Result<Vec<Collection>, String> {
    Ok(entries(bytes)?
        .iter()
        .filter(|e| e.is_object())
        .map(|e| Collection {
            id: text(e, "id"),
            short_name: text(e, "short_name"),
            version: text(e, "version_id"),
            title: Some(text(e, "title")).filter(|t| !t.is_empty()).unwrap_or_else(|| text(e, "dataset_id")),
            data_center: text(e, "data_center"),
            cloud_hosted: e.get("cloud_hosted").and_then(Value::as_bool).unwrap_or(false),
            time_start: text(e, "time_start"),
            time_end: text(e, "time_end"),
            summary: text(e, "summary"),
        })
        .filter(|c| !c.id.is_empty())
        .collect())
}

/// The collection a short name means among several: cloud-hosted first, then the highest
/// version.
pub fn best<'a>(cols: &'a [Collection], short_name: &str) -> Option<&'a Collection> {
    let named: Vec<&Collection> = cols.iter().filter(|c| c.short_name.eq_ignore_ascii_case(short_name)).collect();
    let pool = if named.is_empty() { cols.iter().collect() } else { named };
    pool.into_iter().max_by(|a, b| {
        a.cloud_hosted
            .cmp(&b.cloud_hosted)
            .then_with(|| version_key(&a.version).partial_cmp(&version_key(&b.version)).unwrap_or(std::cmp::Ordering::Equal))
    })
}

/// A version's numbers, for ordering (`2.0` > `1.10` > `1.9`; `002` is 2).
fn version_key(v: &str) -> Vec<f64> {
    v.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty()).take(8).filter_map(|p| p.parse::<f64>().ok()).collect()
}

/// Whether a date is `YYYY-MM-DD`.
pub fn is_date(d: &str) -> bool {
    let b = d.as_bytes();
    b.len() == 10
        && b.iter().enumerate().all(|(i, c)| if i == 4 || i == 7 { *c == b'-' } else { c.is_ascii_digit() })
        && d.get(5..7).and_then(|m| m.parse::<u32>().ok()).is_some_and(|m| (1..=12).contains(&m))
        && d.get(8..10).and_then(|m| m.parse::<u32>().ok()).is_some_and(|m| (1..=31).contains(&m))
}

/// A granule search's area: within the globe, and the whole longitude range when it crosses the
/// antimeridian or isn't a box.
pub fn search_box(b: [f64; 4]) -> Result<[f64; 4], String> {
    let [w, s, e, n] = b;
    if !(w.is_finite() && s.is_finite() && e.is_finite() && n.is_finite()) {
        return Err("the area isn't four numbers".into());
    }
    let (s, n) = (s.clamp(-90.0, 90.0), n.clamp(-90.0, 90.0));
    if s >= n {
        return Err("the area's south is north of its north".into());
    }
    let (w, e) = if e - w >= 360.0 || w < -180.0 || e > 180.0 || w >= e { (-180.0, 180.0) } else { (w, e) };
    Ok([w, s, e, n])
}

/// A collection's granules: where (`area`) and when (`start`, `end`: `YYYY-MM-DD`, either open),
/// newest first.
pub fn granules_url(
    collection: &str,
    area: Option<[f64; 4]>,
    start: Option<&str>,
    end: Option<&str>,
    page_size: usize,
    page: usize,
) -> Result<String, String> {
    let collection = collection.trim();
    if collection.is_empty() || collection.len() > 100 || !collection.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return Err(format!("'{collection}' isn't a collection's concept id (C…-PROVIDER, as earthdata.collections lists them)"));
    }
    let mut q = vec![format!("collection_concept_id={}", encode(collection))];
    if let Some(a) = area {
        let [w, s, e, n] = search_box(a)?;
        q.push(format!("bounding_box={}", encode(&format!("{w},{s},{e},{n}"))));
    }
    let start = start.map(str::trim).filter(|d| !d.is_empty());
    let end = end.map(str::trim).filter(|d| !d.is_empty());
    for d in [start, end].into_iter().flatten() {
        if !is_date(d) {
            return Err(format!("'{d}' isn't a date (YYYY-MM-DD)"));
        }
    }
    if let (Some(a), Some(b)) = (start, end)
        && a > b
    {
        return Err(format!("the start ({a}) is after the end ({b})"));
    }
    if start.is_some() || end.is_some() {
        let a = start.map(|d| format!("{d}T00:00:00Z")).unwrap_or_default();
        let b = end.map(|d| format!("{d}T23:59:59Z")).unwrap_or_default();
        q.push(format!("temporal={}", encode(&format!("{a},{b}"))));
    }
    q.push(format!("page_size={}", page_size.clamp(1, MAX_PAGE)));
    q.push(format!("page_num={}", page.clamp(1, MAX_PAGE_NUM)));
    q.push(format!("sort_key={}", encode("-start_date")));
    Ok(format!("{CMR}/granules.json?{}", q.join("&")))
}

/// Whether a link is to a file worth listing: a data or browse link of the granule's own (not
/// the collection's), on the web, and not metadata.
fn kept_link(href: &str) -> bool {
    let lower = href.to_ascii_lowercase();
    let path = lower.split(['?', '#']).next().unwrap_or(&lower);
    (lower.starts_with("https://") || lower.starts_with("http://"))
        && ![".xml", ".cmr.json", ".md5", ".sha256", ".dmrpp"].iter().any(|x| path.ends_with(x))
        && !path.ends_with("/s3credentials")
}

/// The granules a CMR answer lists.
pub fn parse_granules(bytes: &[u8]) -> Result<Vec<Granule>, String> {
    Ok(entries(bytes)?
        .iter()
        .filter(|e| e.is_object())
        .map(|e| {
            let (mut data, mut browse) = (Vec::new(), Vec::new());
            for l in e.get("links").and_then(Value::as_array).into_iter().flatten().take(500) {
                if l.get("inherited").and_then(Value::as_bool) == Some(true) {
                    continue;
                }
                let (rel, href) = (text(l, "rel"), text(l, "href"));
                if !kept_link(&href) {
                    continue;
                }
                let list = if rel.ends_with("/data#") {
                    &mut data
                } else if rel.ends_with("/browse#") {
                    &mut browse
                } else {
                    continue;
                };
                if !list.contains(&href) {
                    list.push(href);
                }
            }
            Granule {
                id: text(e, "id"),
                title: Some(text(e, "title")).filter(|t| !t.is_empty()).unwrap_or_else(|| text(e, "producer_granule_id")),
                collection: text(e, "collection_concept_id"),
                time_start: text(e, "time_start"),
                time_end: text(e, "time_end"),
                size_mb: number(e, "granule_size").filter(|s| *s >= 0.0),
                cloud_cover: number(e, "cloud_cover").filter(|c| (0.0..=100.0).contains(c)),
                data,
                browse,
                geometry: geometry(e),
            }
        })
        .filter(|g| !g.id.is_empty())
        .collect())
}

/// Numbers in CMR's text (`"lat lon lat lon …"`), as `[lon, lat]` pairs within the globe.
fn latlons(s: &str) -> Option<Vec<[f64; 2]>> {
    let n: Vec<f64> = s.split_whitespace().take(MAX_POINTS * 2).map(str::parse).collect::<Result<_, _>>().ok()?;
    let pts: Vec<[f64; 2]> = n.as_chunks::<2>().0.iter().map(|[lat, lon]| [*lon, *lat]).collect();
    pts.iter().all(|[x, y]| x.is_finite() && y.is_finite() && x.abs() <= 360.0 && y.abs() <= 90.0).then_some(pts)
}

/// A granule's footprint as GeoJSON: its polygons, else its boxes (`"s w n e"`), lines or points.
fn geometry(e: &Value) -> Option<Value> {
    let strings =
        |k: &str| -> Vec<Value> { e.get(k).and_then(Value::as_array).map(|a| a.iter().take(200).cloned().collect()).unwrap_or_default() };
    // Polygons: each a list of rings (the outer first), each ring a string.
    let polys: Vec<Value> = strings("polygons")
        .iter()
        .filter_map(|p| {
            let rings: Vec<Value> = match p {
                Value::Array(r) => {
                    r.iter().take(50).filter_map(Value::as_str).filter_map(latlons).filter(|r| r.len() >= 4).map(|r| json!(r)).collect()
                }
                Value::String(s) => latlons(s).filter(|r| r.len() >= 4).map(|r| vec![json!(r)]).unwrap_or_default(),
                _ => Vec::new(),
            };
            (!rings.is_empty()).then(|| json!(rings))
        })
        .collect();
    if !polys.is_empty() {
        return Some(json!({"type": "MultiPolygon", "coordinates": polys}));
    }
    let boxes: Vec<Value> = strings("boxes")
        .iter()
        .filter_map(Value::as_str)
        .filter_map(|b| {
            let n: Vec<f64> = b.split_whitespace().map(str::parse).collect::<Result<_, _>>().ok()?;
            let [s, w, n, e] = <[f64; 4]>::try_from(n).ok()?;
            ([s, w, n, e].iter().all(|x| x.is_finite()) && s.abs() <= 90.0 && n.abs() <= 90.0 && w.abs() <= 180.0 && e.abs() <= 180.0)
                .then_some([s, w, n, e])
        })
        .flat_map(|[s, w, n, e]| {
            let ring = |w: f64, e: f64| json!([[[w, s], [e, s], [e, n], [w, n], [w, s]]]);
            // West past east crosses the antimeridian: a box either side of it.
            if w > e { vec![ring(w, 180.0), ring(-180.0, e)] } else { vec![ring(w, e)] }
        })
        .collect();
    if !boxes.is_empty() {
        return Some(json!({"type": "MultiPolygon", "coordinates": boxes}));
    }
    let lines: Vec<Value> =
        strings("lines").iter().filter_map(Value::as_str).filter_map(latlons).filter(|l| l.len() >= 2).map(|l| json!(l)).collect();
    if !lines.is_empty() {
        return Some(json!({"type": "MultiLineString", "coordinates": lines}));
    }
    let points: Vec<Value> = strings("points").iter().filter_map(Value::as_str).filter_map(latlons).flatten().map(|p| json!(p)).collect();
    (!points.is_empty()).then(|| json!({"type": "MultiPoint", "coordinates": points}))
}

/// Granules' footprints as a GeoJSON FeatureCollection, with their titles, times and sizes.
pub fn footprints_geojson(granules: &[Granule]) -> Value {
    let features: Vec<Value> = granules
        .iter()
        .filter_map(|g| {
            let geom = g.geometry.clone()?;
            Some(json!({"type": "Feature", "geometry": geom, "properties": {
                "id": g.id, "title": g.title, "start": g.time_start, "end": g.time_end,
                "size_mb": g.size_mb, "cloud_cover": g.cloud_cover,
                "files": g.data.len(), "first_file": g.data.first(),
            }}))
        })
        .collect();
    json!({"type": "FeatureCollection", "features": features})
}

/// A collection as the commands list it.
pub fn collection_json(c: &Collection) -> Value {
    json!({"id": c.id, "shortName": c.short_name, "version": c.version, "title": c.title, "dataCenter": c.data_center,
        "cloudHosted": c.cloud_hosted, "start": c.time_start, "end": c.time_end,
        "summary": c.summary.chars().take(400).collect::<String>()})
}

/// A granule as the commands list it.
pub fn granule_json(g: &Granule) -> Value {
    json!({"id": g.id, "title": g.title, "collection": g.collection, "start": g.time_start, "end": g.time_end,
        "sizeMB": g.size_mb, "cloudCover": g.cloud_cover, "data": g.data, "browse": g.browse,
        "geotiffs": g.tiffs().collect::<Vec<_>>(), "details": g.details_url()})
}

/// `YYYY-MM-DD` of a Unix time.
pub fn date_of(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Today's date and the one `days` before it (`YYYY-MM-DD`).
pub fn recent(days: i64) -> (String, String) {
    let now = now_secs();
    (date_of(now - days * 86_400), date_of(now))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_goes_to_earthdata_data_hosts_only() {
        for url in [
            "https://data.lpdaac.earthdatacloud.nasa.gov/lp-prod-protected/a.tif",
            "https://archive.podaac.earthdata.nasa.gov/x/y.tif",
            "HTTPS://Data.ORNLDAAC.Earthdata.NASA.gov/protected/a.tif",
            "https://earthdatacloud.nasa.gov/a",
        ] {
            assert!(is_data_host(url), "{url}");
        }
        for url in [
            "http://data.lpdaac.earthdatacloud.nasa.gov/a.tif",
            "https://urs.earthdata.nasa.gov/oauth/authorize",
            "https://URS.earthdata.nasa.gov./x",
            "https://uat.urs.earthdata.nasa.gov/oauth/authorize",
            "https://evilearthdata.nasa.gov/a",
            "https://earthdata.nasa.gov.evil.com/a",
            "https://user@data.lpdaac.earthdatacloud.nasa.gov/a",
            "https://data.lpdaac.earthdatacloud.nasa.gov:8443/a",
            "https://d1nklfio7vscoe.cloudfront.net/x.tif",
            "https://cmr.earthdata.nasa.gov/search/granules.json",
            "https://gibs.earthdata.nasa.gov/wmts/x.png",
            "https://search.earthdata.nasa.gov/search",
            "https://gis.earthdata.nasa.gov/portal",
            "https://lp-prod-protected.s3.us-west-2.amazonaws.com/a.tif",
            "s3://lp-prod-protected/a.tif",
            "https://",
            "",
            "https://[::1]/a",
        ] {
            assert!(!is_data_host(url), "{url}");
        }
        assert!(is_login("https://urs.earthdata.nasa.gov/oauth/authorize?client_id=x"));
        assert!(is_login("https://URS.Earthdata.nasa.gov:443/x"));
        assert!(is_login("https://uat.urs.earthdata.nasa.gov/x"));
        assert!(!is_login("https://data.lpdaac.earthdatacloud.nasa.gov/login"));
    }

    #[test]
    fn the_token_is_kept_checked_remembered_and_given_where_it_may_go() {
        // One test owns the token and its store, so tests running at once don't race on them.
        session_tokens();
        remembered_tokens();
        set_secret_store(None);
        set_token(None).unwrap();
    }

    /// A JWT whose payload is {"exp":4000000000} (2096).
    const LASTING: &str = "eyJhbGciOiJSUzI1NiJ9.eyJleHAiOjQwMDAwMDAwMDB9.c2ln";
    /// A JWT whose payload is {"exp":1000000000} (2001).
    const EXPIRED: &str = "eyJhbGciOiJSUzI1NiJ9.eyJleHAiOjEwMDAwMDAwMDB9.c2ln";
    const DATA: &str = "https://data.lpdaac.earthdatacloud.nasa.gov/a.tif";

    fn session_tokens() {
        fn send_sync<T: Send + Sync>() {}
        send_sync::<AccountIdentity>();
        let irrelevant = account_identity_for(CMR).unwrap();
        let absent = account_identity_for(DATA).unwrap();
        assert!(set_token(Some("bad token with spaces")).is_err());
        assert!(set_token(Some("a\r\nX-Evil: 1")).is_err());
        set_token(Some(&format!("Bearer {LASTING}"))).unwrap();
        assert!(!absent.is_current());
        let identity = account_identity_for(DATA).unwrap();
        assert!(identity.is_current() && identity == account_identity().unwrap());
        set_token(Some("synthetic-replacement")).unwrap();
        assert!(!identity.is_current() && irrelevant.is_current());
        assert!(account_identity_for(DATA).unwrap().is_current());
        set_token(Some(LASTING)).unwrap();
        assert!(has_token());
        assert_eq!(token_expiry(), Some(4_000_000_000));
        assert_eq!(authorization(DATA).as_deref(), Some(format!("Bearer {LASTING}").as_str()));
        assert_eq!(authorization("https://urs.earthdata.nasa.gov/a"), None);
        assert_eq!(authorization("https://d1.cloudfront.net/a.tif"), None);
        assert!(refused_message("https://x.earthdata.nasa.gov/a/b.tif?x=1").starts_with("b.tif: Earthdata Login didn't accept"));
        set_token(None).unwrap();
        assert!(!has_token());
        assert_eq!(authorization(DATA), None);
        assert!(refused_message("https://x/b.tif").contains("asks for an Earthdata Login token"));
        // Not a JWT: kept, with no expiry.
        set_token(Some("abc")).unwrap();
        assert_eq!(token_expiry(), None);
        set_token(Some("  ")).unwrap();
        assert!(!has_token());
    }

    #[test]
    fn permission_transport_earthdata_capture_fails_closed() {
        let store = Arc::new(Mutex::new(None));
        let worker = store.clone();
        assert!(
            std::thread::spawn(move || {
                let _locked = worker.lock().unwrap();
                panic!("synthetic poison");
            })
            .join()
            .is_err()
        );
        assert!(identity_in(&store, true).is_err());
        assert!(identity_in(&store, false).is_ok(), "unrelated URLs don't depend on this store");
    }

    /// A new run of the app: nothing typed yet, the store not read.
    fn relaunch(store: &Arc<rustgis_data::secrets::MemorySecrets>) {
        set_token(None).unwrap();
        set_secret_store(Some(store.clone()));
    }

    fn stored(store: &rustgis_data::secrets::MemorySecrets) -> Option<String> {
        store.load(ACCOUNT).unwrap().map(|s| s.expose().to_string())
    }

    fn remembered_tokens() {
        use rustgis_data::secrets::{MemorySecrets, SecretError};
        let store = Arc::new(MemorySecrets::new());

        // Remembered, then read back by the next run's first request to a data host only.
        relaunch(&store);
        assert_eq!(login(LASTING, true), Ok(None));
        assert_eq!(stored(&store).as_deref(), Some(LASTING));
        assert!(is_remembered());
        relaunch(&store);
        assert!(!has_token() && !is_remembered());
        let unread = account_identity_for(DATA).unwrap();
        assert!(unread.is_current() && !has_token(), "snapshotting never restores the remembered token");
        assert_eq!(authorization("https://cmr.earthdata.nasa.gov/search"), None);
        assert!(!has_token(), "a public host doesn't read the store");
        assert_eq!(authorization(DATA).as_deref(), Some(format!("Bearer {LASTING}").as_str()));
        assert!(!unread.is_current(), "restoration changes active identity just like replacement");
        assert!(is_remembered());
        assert_eq!(restore(), Restored::Nothing, "read once");

        // The store given again (another Python session opting in) still knows the token is
        // remembered; a token the store doesn't hold isn't.
        set_secret_store(Some(store.clone()));
        assert!(!is_remembered());
        assert_eq!(restore(), Restored::Nothing);
        assert!(is_remembered() && has_token());
        set_token(Some("typed")).unwrap();
        set_secret_store(Some(store.clone()));
        restore();
        assert!(!is_remembered());
        assert_eq!(stored(&store).as_deref(), Some(LASTING), "nothing is deleted");
        set_token(Some(LASTING)).unwrap();

        // A token given without Remember replaces the remembered one, which is deleted.
        assert_eq!(login("other", false), Ok(None));
        assert_eq!(stored(&store), None);
        assert!(!is_remembered());
        assert_eq!(authorization(DATA).as_deref(), Some("Bearer other"));

        // So does one given in a run that hasn't read the store yet (the command line, say).
        store.save(ACCOUNT, &Secret::new(LASTING)).unwrap();
        relaunch(&store);
        assert_eq!(login("other", false), Ok(None));
        assert_eq!(stored(&store), None);
        relaunch(&store);
        assert_eq!(restore(), Restored::Nothing);
        assert!(!has_token(), "the next run doesn't bring the old token back");

        // A token typed before the store is read wins over the remembered one.
        store.save(ACCOUNT, &Secret::new(LASTING)).unwrap();
        relaunch(&store);
        set_token(Some("typed")).unwrap();
        assert_eq!(restore(), Restored::Nothing);
        assert_eq!(authorization(DATA).as_deref(), Some("Bearer typed"));

        // Logging out deletes the remembered token, even unread.
        relaunch(&store);
        assert_eq!(logout(), Ok(()));
        assert_eq!(stored(&store), None);
        assert!(!has_token() && !is_remembered());

        // An expired remembered token is deleted at reading, and the user asked for a new one.
        store.save(ACCOUNT, &Secret::new(EXPIRED)).unwrap();
        relaunch(&store);
        assert_eq!(restore(), Restored::Expired);
        assert_eq!(stored(&store), None);
        assert!(!has_token() && expired_forgotten());
        assert!(refused_message(DATA).contains("expired and was forgotten"), "{}", refused_message(DATA));
        login(LASTING, false).unwrap();
        assert!(!expired_forgotten());

        // One that expires during the session is forgotten instead of sent.
        assert_eq!(login(EXPIRED, true), Ok(None));
        assert_eq!(authorization(DATA), None);
        assert_eq!(stored(&store), None);
        assert!(!has_token() && !is_remembered() && expired_forgotten());

        // A damaged value is deleted, never sent.
        store.save(ACCOUNT, &Secret::new("not a\r\ntoken")).unwrap();
        relaunch(&store);
        assert_eq!(restore(), Restored::Nothing);
        assert_eq!(stored(&store), None);
        assert!(!has_token());

        // A store that won't take a new token doesn't keep the old one either: the next run
        // doesn't bring back the token this one replaced.
        struct RefusesSaves(Arc<MemorySecrets>);
        impl SecretStore for RefusesSaves {
            fn load(&self, account: &str) -> Result<Option<Secret>, SecretError> {
                self.0.load(account)
            }
            fn save(&self, _: &str, _: &Secret) -> Result<(), SecretError> {
                Err(SecretError::Denied)
            }
            fn delete(&self, account: &str) -> Result<(), SecretError> {
                self.0.delete(account)
            }
        }
        relaunch(&store);
        assert_eq!(login(LASTING, true), Ok(None));
        set_secret_store(Some(Arc::new(RefusesSaves(store.clone()))));
        assert_eq!(login("other", true), Ok(Some(NOT_REMEMBERED)));
        assert_eq!(stored(&store), None);
        assert!(!is_remembered());

        // Without a store, or with one that refuses, the token is kept for the session and says so.
        set_secret_store(None);
        assert_eq!(login(LASTING, true), Ok(Some(NO_STORE)));
        assert!(has_token() && !is_remembered());
        assert_eq!(logout(), Ok(()));
        set_secret_store(Some(Arc::new(MemorySecrets::failing(SecretError::Denied))));
        assert_eq!(login(LASTING, true), Ok(Some(NOT_REMEMBERED)));
        assert!(has_token() && !is_remembered());
        assert_eq!(logout(), Err(NOT_FORGOTTEN));
        assert!(!has_token());
        assert!(login("", true).is_err() && login("two words", true).is_err());
    }

    #[test]
    fn collection_requests_and_answers() {
        let u = collections_url("sea ice & snow", None, true, 500, 0);
        assert!(u.starts_with("https://cmr.earthdata.nasa.gov/search/collections.json?"), "{u}");
        assert!(
            u.contains("keyword=sea%20ice%20%26%20snow")
                && u.contains("cloud_hosted=true")
                && u.contains("page_size=100")
                && u.contains("page_num=1"),
            "{u}"
        );
        let u = collections_url("", Some("HLSL30"), false, 20, 2);
        assert!(u.contains("short_name=HLSL30") && !u.contains("cloud_hosted"), "{u}");
        let body = br#"{"feed":{"entry":[
            {"id":"C1-LPDAAC","short_name":"HLSL30","version_id":"1.5","title":"Old","cloud_hosted":false},
            {"id":"C2-LPCLOUD","short_name":"HLSL30","version_id":"2.0","title":"New","cloud_hosted":true,"data_center":"LPCLOUD"},
            {"id":"C3-LPCLOUD","short_name":"HLSL30","version_id":"1.10","dataset_id":"Mid","cloud_hosted":true},
            {"short_name":"no id"}, 7, null]}}"#;
        let c = parse_collections(body).unwrap();
        assert_eq!(c.len(), 3);
        assert_eq!(c[2].title, "Mid");
        assert_eq!(best(&c, "hlsl30").map(|c| c.id.as_str()), Some("C2-LPCLOUD"));
        assert_eq!(best(&[], "x"), None);
        assert!(parse_collections(br#"{"errors":["bad keyword"]}"#).unwrap_err().contains("bad keyword"));
        for junk in [&b""[..], b"[]", b"{}", b"{\"feed\":7}", b"\xff\xfe", b"{\"feed\":{\"entry\":\"x\"}}"] {
            let _ = parse_collections(junk);
            let _ = parse_granules(junk);
        }
    }

    #[test]
    fn granule_requests_check_their_parts() {
        let u =
            granules_url("C2021957657-LPCLOUD", Some([-90.0, 42.0, -89.0, 43.0]), Some("2024-06-01"), Some("2024-06-30"), 25, 1).unwrap();
        assert!(u.contains("collection_concept_id=C2021957657-LPCLOUD"), "{u}");
        assert!(u.contains("bounding_box=-90%2C42%2C-89%2C43"), "{u}");
        assert!(u.contains("temporal=2024-06-01T00%3A00%3A00Z%2C2024-06-30T23%3A59%3A59Z"), "{u}");
        assert!(u.contains("sort_key=-start_date"), "{u}");
        // Open-ended dates; an area across the antimeridian becomes every longitude.
        let u = granules_url("C1-X", Some([170.0, -10.0, 190.0, 10.0]), None, Some("2024-01-01"), 1000, 99_999).unwrap();
        assert!(
            u.contains("bounding_box=-180%2C-10%2C180%2C10")
                && u.contains("temporal=%2C2024-01-01T23")
                && u.contains("page_size=100")
                && u.contains("page_num=1000"),
            "{u}"
        );
        for (c, s, e) in [
            ("", None, None),
            ("C1&x=1", None, None),
            ("C1-X", Some("2024-13-01"), None),
            ("C1-X", Some("yesterday"), None),
            ("C1-X", Some("2024-06-02"), Some("2024-06-01")),
        ] {
            assert!(granules_url(c, None, s, e, 10, 1).is_err(), "{c} {s:?} {e:?}");
        }
        assert!(granules_url("C1-X", Some([f64::NAN, 0.0, 1.0, 1.0]), None, None, 10, 1).is_err());
        assert!(granules_url("C1-X", Some([0.0, 5.0, 1.0, 1.0]), None, None, 10, 1).is_err());
    }

    #[test]
    fn granules_keep_their_data_links_and_footprints() {
        let body = br#"{"feed":{"entry":[{
            "id":"G1-LPCLOUD","title":"HLS.L30.T15TYG","collection_concept_id":"C2-LPCLOUD",
            "time_start":"2024-06-30T16:34:23Z","granule_size":"12.5","cloud_cover":"22",
            "polygons":[["41.40 -89.29 42.39 -89.23 42.39 -89.27 41.41 -89.57 41.40 -89.29"]],
            "links":[
              {"rel":"http://esipfed.org/ns/fedsearch/1.1/data#","href":"https://data.lpdaac.earthdatacloud.nasa.gov/p/a.B04.tif"},
              {"rel":"http://esipfed.org/ns/fedsearch/1.1/data#","href":"https://data.lpdaac.earthdatacloud.nasa.gov/p/a.B04.tif"},
              {"rel":"http://esipfed.org/ns/fedsearch/1.1/s3#","href":"s3://p/a.B04.tif"},
              {"rel":"http://esipfed.org/ns/fedsearch/1.1/data#","href":"s3://p/a.B05.tif"},
              {"rel":"http://esipfed.org/ns/fedsearch/1.1/data#","href":"https://x.nasa.gov/a.h5"},
              {"rel":"http://esipfed.org/ns/fedsearch/1.1/data#","href":"https://x.nasa.gov/a.cmr.xml"},
              {"rel":"http://esipfed.org/ns/fedsearch/1.1/data#","href":"https://x.nasa.gov/a.tif.sha256"},
              {"rel":"http://esipfed.org/ns/fedsearch/1.1/metadata#","href":"https://x.nasa.gov/s3credentials"},
              {"rel":"http://esipfed.org/ns/fedsearch/1.1/browse#","href":"https://x.nasa.gov/a.jpg"},
              {"rel":"http://esipfed.org/ns/fedsearch/1.1/data#","href":"https://search.earthdata.nasa.gov/x","inherited":true}
            ]},
            {"id":"G2","boxes":["-10 170 10 -170"],"granule_size":-3,"cloud_cover":"500"},
            {"id":"G3","lines":["0 0 1 1"],"points":["5 5"]},
            {"id":"G4","points":["5 6", "x y", "95 0"]},
            {"id":"G5","polygons":[["1 2 3"]],"boxes":["1 2 3"]}
        ]}}"#;
        let g = parse_granules(body).unwrap();
        assert_eq!(g.len(), 5);
        assert_eq!(g[0].data, ["https://data.lpdaac.earthdatacloud.nasa.gov/p/a.B04.tif", "https://x.nasa.gov/a.h5"]);
        assert_eq!(g[0].browse, ["https://x.nasa.gov/a.jpg"]);
        assert_eq!(g[0].tiffs().count(), 1);
        assert_eq!((g[0].size_mb, g[0].cloud_cover), (Some(12.5), Some(22.0)));
        assert_eq!(g[0].details_url(), "https://search.earthdata.nasa.gov/search/granules?p=C2-LPCLOUD&g=G1-LPCLOUD");
        let geom = g[0].geometry.as_ref().unwrap();
        assert_eq!(geom["type"], "MultiPolygon");
        assert_eq!(geom["coordinates"][0][0][0], json!([-89.29, 41.40]));
        assert_eq!((g[1].size_mb, g[1].cloud_cover), (None, None));
        // Across the antimeridian: two boxes, 170–180 and −180 to −170.
        let across = &g[1].geometry.as_ref().unwrap()["coordinates"];
        assert_eq!(across.as_array().unwrap().len(), 2);
        assert_eq!(across[0][0], json!([[170.0, -10.0], [180.0, -10.0], [180.0, 10.0], [170.0, 10.0], [170.0, -10.0]]));
        assert_eq!(across[1][0], json!([[-180.0, -10.0], [-170.0, -10.0], [-170.0, 10.0], [-180.0, 10.0], [-180.0, -10.0]]));
        assert_eq!(g[2].geometry.as_ref().unwrap()["type"], "MultiLineString");
        // Only the points that read are kept.
        assert_eq!(g[3].geometry.as_ref().unwrap()["coordinates"], json!([[6.0, 5.0]]));
        assert_eq!(g[4].geometry, None);
        let fc = footprints_geojson(&g);
        assert_eq!(fc["features"].as_array().unwrap().len(), 4);
        assert_eq!(fc["features"][0]["properties"]["title"], "HLS.L30.T15TYG");
    }

    #[test]
    fn dates() {
        assert_eq!(date_of(0), "1970-01-01");
        assert_eq!(date_of(1_718_000_000), "2024-06-10");
        assert_eq!(date_of(-86_400), "1969-12-31");
        let (a, b) = recent(30);
        assert!(is_date(&a) && is_date(&b) && a < b);
        assert!(!is_date("2024-6-01") && !is_date("2024-00-10") && !is_date("２０２４-01-01"));
    }
}
