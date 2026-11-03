//! Signing in to ArcGIS: an ArcGIS Online organization or an ArcGIS Enterprise portal, so that
//! its secured services (which otherwise answer error 499, "Token Required") can be read.
//!
//! The sign-in is OAuth 2.0 authorization code with PKCE (S256) in the system browser: the user
//! signs in on the portal's own page (its SSO and MFA included), so RustGIS never sees a password.
//! The portal sends the browser back to a short-lived server on this computer at
//! [`redirect_uri`] (`http://127.0.0.1:7290/arcgis/callback` by default). ArcGIS accepts only
//! the redirect addresses an app registration lists, exactly, so the portal's administrator
//! registers that address with the OAuth client id the user gives.
//!
//! A token kept here goes, in the `X-Esri-Authorization` header ([`with_token`]'s `token`
//! parameter in the browser, so the portal's and any proxy's request logs may keep it there, as
//! with ArcGIS's own web apps), only to the addresses its
//! session covers ([`Scope`]): the portal itself and, for ArcGIS Online, the organization's
//! hosted services (`*.arcgis.com/<org id>/…`); for ArcGIS Enterprise, the portal's host and
//! its federated servers. A token given with a service's address (`…/FeatureServer?token=…`,
//! [`adopt_url_token`]) covers that ArcGIS Server only. Tokens live in memory for the session:
//! never in a project, a connection or the settings. Access tokens are renewed with the refresh
//! token before they expire, and a sign-out revokes the refresh token.
//!
//! The requests and answers are built and read here (sans I/O); the callback server, the
//! renewal and the revocation are desktop only.

use std::sync::Mutex;

use serde_json::Value;

/// ArcGIS Online's portal.
pub const ONLINE: &str = "https://www.arcgis.com";
/// The callback server's port, unless another is given.
pub const PORT: u16 = 7290;
/// The callback's path.
pub const CALLBACK: &str = "/arcgis/callback";
/// An access token is renewed this long before it expires (seconds).
const REFRESH_MARGIN: i64 = 60;
/// After the portal couldn't be reached for a renewal, how long until it is tried again (seconds).
const RETRY_RENEWAL: i64 = 30;
/// RustGIS's own OAuth app on ArcGIS Online, used when no client id is given for an ArcGIS Online
/// portal (an Enterprise portal registers its own). Its redirect URLs are the desktop's
/// [`redirect_uri`] for [`PORT`] and the web app's callback page (`oauth-callback.html`) at
/// https://web.rustgis.app and `trunk serve`'s http://127.0.0.1:8772. A client id isn't a secret.
pub const DEFAULT_CLIENT_ID: &str = "nD2lKYqFqsaddfuD";
/// The web app's callback page, next to its index.
pub const WEB_CALLBACK: &str = "oauth-callback.html";
/// The session asked for: two weeks (the longest ArcGIS gives), in minutes.
const EXPIRATION_MINUTES: &str = "20160";
/// The longest token kept (ArcGIS's are a few hundred characters).
const MAX_TOKEN: usize = 8 << 10;
/// The most sessions kept (one per portal or server).
const MAX_SESSIONS: usize = 64;
/// The most federated servers a session covers.
const MAX_SERVERS: usize = 64;

/// The redirect address of a sign-in whose callback server listens on `port`.
pub fn redirect_uri(port: u16) -> String {
    format!("http://127.0.0.1:{port}{CALLBACK}")
}

/// An HTTPS address's origin (`https://host[:port]`, lowercase) and path (`/` when empty),
/// without its query or fragment. `None` for anything else, or an address with user info.
fn split(url: &str) -> Option<(String, String)> {
    let rest = url.get(..8).filter(|s| s.eq_ignore_ascii_case("https://")).and_then(|_| url.get(8..))?;
    let rest = rest.split(['?', '#']).next()?;
    let (authority, path) = rest.find('/').map_or((rest, "/"), |i| rest.split_at(i));
    if authority.is_empty() || authority.contains(['@', '\\', '[', ']']) || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let authority = authority.to_ascii_lowercase();
    let (host, port) = authority.split_once(':').map_or((authority.as_str(), None), |(h, p)| (h, Some(p)));
    let host = host.trim_end_matches('.');
    if host.is_empty() || port.is_some_and(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit())) {
        return None;
    }
    let origin = match port.filter(|p| *p != "443") {
        Some(p) => format!("https://{host}:{p}"),
        None => format!("https://{host}"),
    };
    Some((origin, path.to_string()))
}

fn host_of(origin: &str) -> &str {
    let h = origin.strip_prefix("https://").unwrap_or(origin);
    h.split(':').next().unwrap_or(h)
}

/// A portal's base address: `https://www.arcgis.com` for blank, an organization's
/// (`https://myorg.maps.arcgis.com`) or an Enterprise portal's (`https://gis.example.org/portal`).
/// Takes the base, its home page or any `/sharing/rest…` address. Only HTTPS: tokens go there.
pub fn portal_base(input: &str) -> Result<String, String> {
    let raw = input.trim();
    if raw.is_empty() {
        return Ok(ONLINE.to_string());
    }
    let (origin, path) = split(raw).ok_or_else(|| format!("{raw}: a portal is given by its https:// address"))?;
    if origin == "https://arcgis.com" {
        return Ok(ONLINE.to_string());
    }
    let mut kept = Vec::new();
    for part in path.split('/').filter(|p| !p.is_empty()) {
        if ["sharing", "home", "apps", "rest"].iter().any(|s| part.eq_ignore_ascii_case(s)) {
            break;
        }
        kept.push(part);
    }
    // An ArcGIS Online organization's portal is at its root.
    if is_online_host(host_of(&origin)) {
        kept.clear();
    }
    Ok(if kept.is_empty() { origin } else { format!("{origin}/{}", kept.join("/")) })
}

fn is_online_host(host: &str) -> bool {
    host == "arcgis.com" || host.ends_with(".arcgis.com")
}

/// Whether a portal base is ArcGIS Online (www.arcgis.com or an organization's address).
pub fn is_online(portal: &str) -> bool {
    split(portal).is_some_and(|(origin, _)| is_online_host(host_of(&origin)))
}

/// A portal's OAuth 2.0 endpoint (`authorize`, `token` or `revokeToken`). An ArcGIS Online
/// organization's tokens are asked of www.arcgis.com (the only one a web page may read the
/// answers of); its users sign in on its own page.
pub fn endpoint(portal: &str, name: &str) -> String {
    let base = if name != "authorize" && is_online(portal) { ONLINE } else { portal };
    format!("{base}/sharing/rest/oauth2/{name}")
}

/// Percent-encodes a form or query value.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn form(params: &[(&str, &str)]) -> String {
    params.iter().map(|(k, v)| format!("{k}={}", encode(v))).collect::<Vec<_>>().join("&")
}

/// Decodes a query value (`+` is a space).
fn decode(s: &str) -> String {
    decode_with(s, true)
}

/// Decodes `%XX` escapes, and `+` as a space when `plus` (form data, not a pasted token).
fn decode_with(s: &str, plus: bool) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        match b {
            b'+' if plus => out.push(b' '),
            b'%' => {
                let hex = bytes.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
                if let Some(v) = hex {
                    out.push(v);
                    i += 2;
                } else {
                    out.push(b'%');
                }
            }
            _ => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A query's parameters, decoded.
pub fn query_params(query: &str) -> Vec<(String, String)> {
    query.split('&').filter(|p| !p.is_empty()).map(|p| p.split_once('=').unwrap_or((p, ""))).map(|(k, v)| (decode(k), decode(v))).collect()
}

/// Whether `id` looks like an OAuth client id (ArcGIS's are 16 letters and digits).
pub fn valid_client_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

fn valid_token(t: &str) -> bool {
    !t.is_empty() && t.len() <= MAX_TOKEN && t.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.~+/=".contains(&b))
}

/// The PKCE challenge (S256) of a verifier.
pub fn pkce_challenge(verifier: &str) -> String {
    use base64::Engine;
    use sha2::Digest;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier.as_bytes()))
}

/// The portal's sign-in page.
pub fn authorize_url(portal: &str, client_id: &str, redirect_uri: &str, challenge: &str, state: &str) -> String {
    let query = form(&[
        ("client_id", client_id),
        ("response_type", "code"),
        ("redirect_uri", redirect_uri),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
        ("state", state),
        ("expiration", EXPIRATION_MINUTES),
    ]);
    format!("{}?{query}", endpoint(portal, "authorize"))
}

/// The form exchanging the browser's code for tokens.
pub fn exchange_body(client_id: &str, code: &str, redirect_uri: &str, verifier: &str) -> String {
    form(&[
        ("f", "json"),
        ("grant_type", "authorization_code"),
        ("client_id", client_id),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("code_verifier", verifier),
    ])
}

/// The form renewing an access token.
pub fn refresh_body(client_id: &str, refresh_token: &str) -> String {
    form(&[("f", "json"), ("grant_type", "refresh_token"), ("client_id", client_id), ("refresh_token", refresh_token)])
}

/// The form revoking a refresh token (or, without one, the access token).
pub fn revoke_body(client_id: &str, token: &str, refresh: bool) -> String {
    let hint = if refresh { "refresh_token" } else { "access_token" };
    form(&[("f", "json"), ("client_id", client_id), ("auth_token", token), ("token_type_hint", hint)])
}

/// The tokens a token request gave.
#[derive(Clone, PartialEq, Eq)]
pub struct Grant {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Unix seconds.
    pub expires_at: i64,
    pub username: String,
}

impl std::fmt::Debug for Grant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Grant").field("username", &self.username).field("expires_at", &self.expires_at).finish_non_exhaustive()
    }
}

/// Reads a token answer: ArcGIS reports a refusal as HTTP 200 with an `error` object.
pub fn parse_grant(body: &[u8], now: i64) -> Result<Grant, String> {
    let v: Value = serde_json::from_slice(body).map_err(|_| "the portal's token answer isn't JSON".to_string())?;
    if let Some(e) = v.get("error") {
        let why = e
            .get("error_description")
            .or_else(|| e.get("message"))
            .and_then(Value::as_str)
            .or_else(|| e.get("details").and_then(Value::as_array).and_then(|d| d.first()).and_then(Value::as_str))
            .unwrap_or("(no message)");
        return Err(format!("the portal refused the sign-in: {}", why.chars().take(300).collect::<String>()));
    }
    let token = v.get("access_token").and_then(Value::as_str).filter(|t| valid_token(t)).ok_or("the portal gave no access token")?;
    let refresh = v.get("refresh_token").and_then(Value::as_str).filter(|t| valid_token(t)).map(str::to_string);
    let lifetime = v.get("expires_in").and_then(Value::as_i64).filter(|s| *s > 0).unwrap_or(1800).min(366 * 86_400);
    let username = v.get("username").and_then(Value::as_str).unwrap_or("").chars().take(200).collect();
    Ok(Grant { access_token: token.to_string(), refresh_token: refresh, expires_at: now.saturating_add(lifetime), username })
}

/// Where a session's token may go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Addresses at this origin whose path starts with this (lowercase, ending in `/`).
    Prefix { origin: String, path: String },
    /// An ArcGIS Online organization's hosted services: an `*.arcgis.com` address with its id as
    /// its path's first or second part (`services1.arcgis.com/<id>/…`, `tiles.arcgis.com/tiles/<id>/…`).
    Org(String),
}

impl Scope {
    fn prefix(url: &str) -> Option<Scope> {
        let (origin, path) = split(url)?;
        let mut path = path.to_ascii_lowercase();
        if !path.ends_with('/') {
            path.push('/');
        }
        Some(Scope::Prefix { origin, path })
    }

    fn covers(&self, origin: &str, path: &str) -> bool {
        // A path whose dot segments a client would resolve (`/server/../other/`) is judged by
        // where it leads, so it gets no token at all.
        let lower = path.to_ascii_lowercase();
        if lower.split('/').any(|seg| {
            let seg = seg.replace("%2e", ".");
            seg == "." || seg == ".."
        }) {
            return false;
        }
        match self {
            Scope::Prefix { origin: o, path: p } => {
                if o != origin {
                    return false;
                }
                let lower = path.to_ascii_lowercase();
                lower.starts_with(p.as_str()) || format!("{lower}/") == *p
            }
            Scope::Org(id) => {
                is_online_host(host_of(origin))
                    && !origin.trim_start_matches("https://").contains(':')
                    && path.split('/').filter(|p| !p.is_empty()).take(2).any(|p| p == id)
            }
        }
    }
}

/// What a session's token covers: the portal, then its organization's hosted services (ArcGIS
/// Online, `org` is its id) or its host and federated `servers` (ArcGIS Enterprise).
pub fn scopes_for(portal: &str, org: Option<&str>, servers: &[String]) -> Vec<Scope> {
    let mut scopes = Vec::new();
    if let Some((origin, _)) = split(portal) {
        // An Enterprise portal's own host serves its federated servers too (behind one web adaptor).
        scopes.push(Scope::Prefix { origin, path: "/".into() });
    }
    if is_online(portal) {
        if let Some(s) = Scope::prefix(&format!("{ONLINE}/sharing/rest/")) {
            scopes.push(s);
        }
        if let Some(id) = org.filter(|id| !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric())) {
            scopes.push(Scope::Org(id.to_string()));
        }
    }
    for s in servers.iter().take(MAX_SERVERS).filter_map(|s| Scope::prefix(s)) {
        if !scopes.contains(&s) {
            scopes.push(s);
        }
    }
    scopes
}

/// The federated servers a portal's `portals/self/servers` answer lists.
pub fn parse_servers(body: &[u8]) -> Vec<String> {
    let v: Value = serde_json::from_slice(body).unwrap_or_default();
    let Some(servers) = v.get("servers").and_then(Value::as_array) else { return Vec::new() };
    servers
        .iter()
        .take(MAX_SERVERS)
        .filter_map(|s| s.get("url").and_then(Value::as_str))
        .filter(|u| split(u).is_some())
        .map(str::to_string)
        .collect()
}

/// The organization id a portal's `portals/self` answer gives (ArcGIS Online).
pub fn parse_org(body: &[u8]) -> Option<String> {
    let v: Value = serde_json::from_slice(body).ok()?;
    v.get("id").and_then(Value::as_str).map(str::to_string)
}

/// One sign-in, or a token given for a server.
#[derive(Clone)]
struct Session {
    /// The portal's base, or the server's root for a token given with an address.
    portal: String,
    client_id: String,
    username: String,
    access_token: String,
    /// Unix seconds; `i64::MAX` when unknown (a token given with an address).
    expires_at: i64,
    refresh_token: Option<String>,
    scopes: Vec<Scope>,
    /// Unix seconds before which a renewal isn't tried again (after the portal couldn't be reached).
    retry_at: i64,
}

fn store() -> &'static Mutex<Vec<Session>> {
    static S: std::sync::OnceLock<Mutex<Vec<Session>>> = std::sync::OnceLock::new();
    S.get_or_init(Mutex::default)
}

fn sessions() -> std::sync::MutexGuard<'static, Vec<Session>> {
    store().lock().unwrap_or_else(|e| e.into_inner())
}

fn now_secs() -> i64 {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn keep(session: Session) {
    let mut all = sessions();
    // A token given for a portal's own address doesn't replace a sign-in to it (nor the reverse).
    all.retain(|s| !(s.portal == session.portal && s.client_id.is_empty() == session.client_id.is_empty()));
    if all.len() >= MAX_SESSIONS {
        all.remove(0);
    }
    all.push(session);
}

/// Keeps a sign-in to `portal` for the session, its token covering `scopes`.
pub fn sign_in(portal: &str, client_id: &str, grant: Grant, scopes: Vec<Scope>) {
    keep(Session {
        portal: portal.to_string(),
        client_id: client_id.to_string(),
        username: grant.username,
        access_token: grant.access_token,
        expires_at: grant.expires_at,
        refresh_token: grant.refresh_token,
        scopes,
        retry_at: 0,
    });
}

/// The ArcGIS Server root of a service's address, which a token given with it covers:
/// everything before its `rest` part (`https://host/server/rest/services/…` → `https://host/server/`).
pub fn server_root(url: &str) -> Option<String> {
    let (origin, path) = split(url)?;
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    let rest = parts.iter().position(|p| p.eq_ignore_ascii_case("rest") || p.eq_ignore_ascii_case("sharing"))?;
    let root = parts.get(..rest)?.join("/");
    Some(if root.is_empty() { format!("{origin}/") } else { format!("{origin}/{root}/") })
}

/// Keeps `token` for the ArcGIS Server (or portal) that `url` is on, for the session. Returns
/// the server root it covers.
pub fn add_token(url: &str, token: &str) -> Result<String, String> {
    let token = token.trim();
    if !valid_token(token) {
        return Err("an ArcGIS token is one word of letters, digits and - _ . ~ + / =".into());
    }
    let root = server_root(url).ok_or_else(|| {
        format!("{}: a token is given with an ArcGIS REST address on https:// (…/rest/services/… or …/sharing/rest)", url.trim())
    })?;
    let scopes = Scope::prefix(&root).into_iter().collect();
    keep(Session {
        portal: root.trim_end_matches('/').to_string(),
        client_id: String::new(),
        username: String::new(),
        access_token: token.to_string(),
        expires_at: i64::MAX,
        refresh_token: None,
        scopes,
        retry_at: 0,
    });
    Ok(root)
}

/// A service's address with its `token` parameter taken out and kept ([`add_token`]): the token
/// then goes (in a header on the desktop, as the request's `token` parameter in the browser) to
/// every request to that server (the service's layers, its tiles and
/// its feature pages), and the address kept in the project has no token in it.
pub fn adopt_url_token(url: &str) -> Result<String, String> {
    let Some((base, query)) = url.split_once('?') else { return Ok(url.to_string()) };
    let (query, fragment) = query.split_once('#').map_or((query, None), |(q, f)| (q, Some(f)));
    let mut token = None;
    let kept: Vec<&str> = query
        .split('&')
        .filter(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            if k.eq_ignore_ascii_case("token") {
                // A token is pasted, not form-encoded: its `+` stays a `+`.
                token = Some(decode_with(v, false));
                false
            } else {
                true
            }
        })
        .collect();
    let Some(token) = token else { return Ok(url.to_string()) };
    if !token.trim().is_empty() {
        add_token(base, &token)?;
    }
    let mut out = base.to_string();
    if !kept.is_empty() {
        out.push('?');
        out.push_str(&kept.join("&"));
    }
    if let Some(f) = fragment {
        out.push('#');
        out.push_str(f);
    }
    Ok(out)
}

/// The session covering `url`, by index, the most specific one first: a token given for a
/// server over a portal's sign-in.
fn covering(all: &[Session], url: &str) -> Option<usize> {
    let (origin, path) = split(url)?;
    all.iter()
        .enumerate()
        .filter(|(_, s)| s.scopes.iter().any(|sc| sc.covers(&origin, &path)))
        .max_by_key(|(_, s)| (s.client_id.is_empty(), s.portal.len()))
        .map(|(i, _)| i)
}

/// The `X-Esri-Authorization` header value a request to `url` carries: the token of the session
/// covering it, while it is valid. See [`authorization_fresh`] for one renewed first.
pub fn authorization(url: &str) -> Option<String> {
    let all = sessions();
    let s = all.get(covering(&all, url)?)?;
    (s.expires_at > now_secs()).then(|| format!("Bearer {}", s.access_token))
}

/// `url` with the token of the session covering it as its `token` parameter, for the browser:
/// ArcGIS's `query` endpoints answer a CORS preflight with an error, so a request carrying the
/// header can't be sent from a web page, while a plain GET needs no preflight. Only the request
/// carries it: the address a layer or the cache keeps has no token. `None` when nothing covers it.
pub fn with_token(url: &str) -> Option<String> {
    let token = {
        let all = sessions();
        let s = all.get(covering(&all, url)?)?;
        (s.expires_at > now_secs()).then(|| s.access_token.clone())?
    };
    with_token_value(url, &token)
}

/// `url` with `token` as its `token` parameter; `None` when it carries one already (the user's).
pub fn with_token_value(url: &str, token: &str) -> Option<String> {
    let query = url.split('#').next().unwrap_or(url).split_once('?').map_or("", |(_, q)| q);
    if query.split('&').any(|kv| kv.split('=').next().is_some_and(|k| k.eq_ignore_ascii_case("token"))) {
        return None;
    }
    let (base, fragment) = url.split_once('#').map_or((url, None), |(b, f)| (b, Some(f)));
    let joint = if base.contains('?') { if base.ends_with('?') || base.ends_with('&') { "" } else { "&" } } else { "?" };
    let mut out = format!("{base}{joint}token={}", encode(token));
    if let Some(f) = fragment {
        out.push('#');
        out.push_str(f);
    }
    Some(out)
}

/// A renewal to send: the token request renewing the session covering an address.
#[derive(Clone, PartialEq, Eq)]
pub struct Renewal {
    pub portal: String,
    /// The token endpoint, and the form to POST to it.
    pub endpoint: String,
    pub body: String,
    refresh: String,
}

/// The renewal the session covering `url` needs: it has a refresh token, its access token is
/// about to expire, and its portal wasn't just found unreachable.
pub fn renewal_due(url: &str) -> Option<Renewal> {
    let all = sessions();
    let s = all.get(covering(&all, url)?)?;
    let refresh = s.refresh_token.as_ref()?;
    let now = now_secs();
    (s.expires_at - now <= REFRESH_MARGIN && now >= s.retry_at).then(|| Renewal {
        portal: s.portal.clone(),
        endpoint: endpoint(&s.portal, "token"),
        body: refresh_body(&s.client_id, refresh),
        refresh: refresh.clone(),
    })
}

/// Lands a renewal's answer: a new access token is kept; a refusal ends the session; an
/// unreachable portal (`Err`) keeps it and holds off renewing for a while. Nothing changes for a
/// session signed out or replaced since.
pub fn renewal_done(r: &Renewal, answer: Result<&[u8], String>) {
    let mut all = sessions();
    let Some(i) = all.iter().position(|s| s.portal == r.portal && s.refresh_token.as_deref() == Some(r.refresh.as_str())) else { return };
    // An answer that isn't a JSON object (a proxy's error page) says nothing of the refresh
    // token: tried again later, as an unreachable portal is.
    let answer = answer.and_then(|body| {
        if serde_json::from_slice::<Value>(body).is_ok_and(|v| v.is_object()) {
            Ok(body)
        } else {
            Err("the portal's answer isn't JSON".to_string())
        }
    });
    match answer.map(|body| parse_grant(body, now_secs())) {
        Ok(Ok(grant)) => {
            if let Some(s) = all.get_mut(i) {
                s.access_token = grant.access_token;
                s.expires_at = grant.expires_at;
                if grant.refresh_token.is_some() {
                    s.refresh_token = grant.refresh_token;
                }
            }
        }
        Ok(Err(e)) => {
            log::warn!("{}: the ArcGIS sign-in ended: {e}", r.portal);
            all.remove(i);
        }
        Err(e) => {
            log::warn!("{}: couldn't renew the ArcGIS sign-in: {e}", r.portal);
            if let Some(s) = all.get_mut(i) {
                s.retry_at = now_secs() + RETRY_RENEWAL;
            }
        }
    }
}

/// The client id a sign-in to `portal` uses: the one given, else RustGIS's own for ArcGIS Online.
pub fn client_id_for(portal: &str, given: &str) -> Option<String> {
    let given = given.trim();
    if !given.is_empty() {
        return Some(given.to_string());
    }
    is_online(portal).then(|| DEFAULT_CLIENT_ID.to_string())
}

/// The document telling what a portal's token covers: `portals/self` for ArcGIS Online (its
/// organization's id), `portals/self/servers` for ArcGIS Enterprise (its federated servers).
pub fn discovery_url(portal: &str) -> String {
    if is_online(portal) {
        format!("{portal}/sharing/rest/portals/self?f=json")
    } else {
        format!("{portal}/sharing/rest/portals/self/servers?f=json")
    }
}

/// What a portal's token covers, from its [`discovery_url`] answer (`None` when it couldn't be
/// read: the portal's own host then).
pub fn discovered_scopes(portal: &str, answer: Option<&[u8]>) -> Vec<Scope> {
    if is_online(portal) {
        scopes_for(portal, answer.and_then(parse_org).as_deref(), &[])
    } else {
        scopes_for(portal, None, &answer.map(parse_servers).unwrap_or_default())
    }
}

/// A browser sign-in's start: the page to open, the redirect its callback page has, and what
/// finishing needs. `verifier` and `state` are random (the browser's crypto).
pub struct WebSignIn {
    pub url: String,
    pub portal: String,
    pub client_id: String,
    pub redirect: String,
    verifier: String,
    state: String,
}

/// Starts a sign-in in the browser, whose app's callback page is `redirect`.
pub fn web_sign_in(portal: &str, client_id: &str, redirect: &str, verifier: &str, state: &str) -> Result<WebSignIn, String> {
    let portal = portal_base(portal)?;
    let client_id = client_id_for(&portal, client_id)
        .filter(|c| valid_client_id(c))
        .ok_or("give the OAuth client id of an app registered on the portal (Content › New item › Developer credentials)")?;
    let url = authorize_url(&portal, &client_id, redirect, &pkce_challenge(verifier), state);
    Ok(WebSignIn { url, portal, client_id, redirect: redirect.to_string(), verifier: verifier.to_string(), state: state.to_string() })
}

impl WebSignIn {
    /// The token request (endpoint and form) for the callback page's address, once it holds the
    /// portal's answer: `None` while it doesn't (still signing in), an error for a refusal or a
    /// callback that isn't this sign-in's.
    pub fn exchange(&self, callback: &str) -> Option<Result<(String, String), String>> {
        let (page, query) = callback.split_once('?')?;
        if page != self.redirect {
            return None;
        }
        let params = query_params(query.split('#').next().unwrap_or(query));
        let get = |k: &str| params.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str()).unwrap_or("");
        if params.iter().filter(|(k, _)| k == "state").count() != 1 || get("state") != self.state {
            return Some(Err("the sign-in's answer isn't this sign-in's (try again)".into()));
        }
        if !get("error").is_empty() {
            return Some(Err(if get("error") == "access_denied" {
                "the sign-in was declined".into()
            } else {
                format!("the portal said {}", get("error"))
            }));
        }
        let code = get("code");
        if code.is_empty() {
            return Some(Err("the portal gave no authorization code".into()));
        }
        Some(Ok((endpoint(&self.portal, "token"), exchange_body(&self.client_id, code, &self.redirect, &self.verifier))))
    }
}

/// What may be shown about a session: never its tokens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Info {
    pub portal: String,
    pub username: String,
    /// Unix seconds; `None` when unknown (a token given with an address).
    pub expires_at: Option<i64>,
    /// Whether it renews itself.
    pub renews: bool,
    /// A token given with an address, not a sign-in.
    pub token: bool,
}

/// The sessions kept.
pub fn list() -> Vec<Info> {
    sessions()
        .iter()
        .map(|s| Info {
            portal: s.portal.clone(),
            username: s.username.clone(),
            expires_at: (s.expires_at != i64::MAX).then_some(s.expires_at),
            renews: s.refresh_token.is_some(),
            token: s.client_id.is_empty(),
        })
        .collect()
}

/// The session signed in to `portal` (a base as [`portal_base`] gives), if any.
pub fn signed_in(portal: &str) -> Option<Info> {
    list().into_iter().find(|s| s.portal == portal && !s.token)
}

/// A revocation to send after a sign-out: the endpoint and its form.
#[derive(Clone, PartialEq, Eq)]
pub struct Revocation {
    pub endpoint: String,
    pub body: String,
}

/// Forgets the session of `portal` (a portal base or a server root), or every session for
/// `None`. Returns the revocations to send (sign-ins only) and how many were forgotten.
pub fn sign_out(portal: Option<&str>) -> (Vec<Revocation>, usize) {
    let mut all = sessions();
    let mut revocations = Vec::new();
    let before = all.len();
    all.retain(|s| {
        let gone = portal.is_none_or(|p| p.trim_end_matches('/') == s.portal);
        if gone && !s.client_id.is_empty() {
            let (token, refresh) = match &s.refresh_token {
                Some(r) => (r.as_str(), true),
                None => (s.access_token.as_str(), false),
            };
            revocations.push(Revocation { endpoint: endpoint(&s.portal, "revokeToken"), body: revoke_body(&s.client_id, token, refresh) });
        }
        !gone
    });
    let gone = before - all.len();
    (revocations, gone)
}

// ---------------------------------------------------------------------------------------------
// The desktop and the command line: the callback server, renewing and revoking.

#[cfg(not(target_arch = "wasm32"))]
pub use native::*;

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::{Duration, Instant};

    use super::{Info, Revocation};

    /// How long the browser has to come back.
    const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
    const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

    /// The sign-in under way, or how the last one ended.
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub enum Progress {
        #[default]
        Idle,
        /// Waiting for the browser to come back from `portal`.
        Pending {
            portal: String,
        },
        Done(Info),
        Failed(String),
    }

    struct Flow {
        progress: Progress,
        cancel: Option<Arc<AtomicBool>>,
    }

    fn flow() -> std::sync::MutexGuard<'static, Flow> {
        static F: Mutex<Flow> = Mutex::new(Flow { progress: Progress::Idle, cancel: None });
        F.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The sign-in under way, or how the last one ended.
    pub fn progress() -> Progress {
        flow().progress.clone()
    }

    /// Stops a sign-in under way.
    pub fn cancel_sign_in() {
        let mut f = flow();
        if let Some(c) = f.cancel.take() {
            c.store(true, Ordering::SeqCst);
        }
        if matches!(f.progress, Progress::Pending { .. }) {
            f.progress = Progress::Failed("the sign-in was cancelled".into());
        }
    }

    fn random_token() -> Result<String, String> {
        use base64::Engine;
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|e| format!("no random numbers: {e}"))?;
        Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
    }

    /// The agent for token requests (`forms`: an error status is answered, for the caller to
    /// read) or portal reads (an error status fails). Each is made once, so its TLS setup (the
    /// system's certificates, read from disk on Linux) is too; connections aren't kept, as before.
    fn agent(forms: bool) -> &'static ureq::Agent {
        static FORMS: OnceLock<ureq::Agent> = OnceLock::new();
        static READS: OnceLock<ureq::Agent> = OnceLock::new();
        (if forms { &FORMS } else { &READS }).get_or_init(|| {
            // A token request is never redirected: a redirect would replay the code or the refresh token.
            ureq::Agent::config_builder()
                .tls_config(rustgis_data::http::tls_config())
                .http_status_as_error(!forms)
                .timeout_global(Some(REQUEST_TIMEOUT))
                .user_agent(concat!("RustGIS/", env!("CARGO_PKG_VERSION"), " (+https://rustgis.app)"))
                .max_redirects(0)
                .max_idle_connections(0)
                .build()
                .into()
        })
    }

    /// POSTs a form; a refusal's answer (an OAuth `error`, often with HTTP 400) is returned for
    /// the caller to read, while a server error (5xx) counts as the portal not being reached.
    fn post_form(url: &str, body: &str) -> Result<Vec<u8>, String> {
        let resp = agent(true)
            .post(url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("Accept", "application/json")
            .send(body)
            .map_err(|e| format!("couldn't reach the portal: {e}"))?;
        let status = resp.status().as_u16();
        if status >= 500 {
            return Err(format!("the portal answered HTTP {status}"));
        }
        resp.into_body().with_config().limit(1 << 20).read_to_vec().map_err(|e| e.to_string())
    }

    fn get_json(url: &str, token: &str) -> Result<Vec<u8>, String> {
        let resp = agent(false)
            .get(url)
            .header("X-Esri-Authorization", &format!("Bearer {token}"))
            .header("Accept", "application/json")
            .call()
            .map_err(|e| e.to_string())?;
        resp.into_body().with_config().limit(4 << 20).read_to_vec().map_err(|e| e.to_string())
    }

    /// Starts signing in to `portal` (blank: ArcGIS Online) with an OAuth app's `client_id`:
    /// binds the callback server on `port` and gives the portal's sign-in page to open in the
    /// browser. The session is kept when the browser comes back ([`progress`] tells).
    pub fn start_sign_in(portal: &str, client_id: &str, port: u16) -> Result<String, String> {
        let portal = super::portal_base(portal)?;
        let client_id = super::client_id_for(&portal, client_id).unwrap_or_default();
        let client_id = client_id.as_str();
        if !super::valid_client_id(client_id) {
            return Err("give the OAuth client id of an app registered on the portal (Content › New item › Developer credentials)".into());
        }
        cancel_sign_in();
        let redirect = super::redirect_uri(port);
        let listener = bind(port)?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let verifier = random_token()?;
        let state = random_token()?;
        let url = super::authorize_url(&portal, client_id, &redirect, &super::pkce_challenge(&verifier), &state);
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut f = flow();
            f.progress = Progress::Pending { portal: portal.clone() };
            f.cancel = Some(cancel.clone());
        }
        let pending = Pending { portal, client_id: client_id.to_string(), redirect, verifier, state };
        std::thread::spawn(move || {
            let result = serve(&listener, &cancel, &pending);
            let mut f = flow();
            // A newer sign-in replaced this one: its outcome isn't this one's to tell.
            if !cancel.load(Ordering::SeqCst) {
                f.progress = match result {
                    Ok(info) => Progress::Done(info),
                    Err(e) => Progress::Failed(e),
                };
                f.cancel = None;
            }
        });
        Ok(url)
    }

    /// Binds the callback server. A sign-in just cancelled lets go of the port when its thread
    /// next looks (within a few seconds): until then, try again.
    fn bind(port: u16) -> Result<TcpListener, String> {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match TcpListener::bind(("127.0.0.1", port)) {
                Ok(l) => return Ok(l),
                Err(e) if e.kind() == std::io::ErrorKind::AddrInUse && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50))
                }
                Err(e) => {
                    return Err(format!(
                        "can't start the sign-in's callback server on 127.0.0.1:{port}: {e} (another program may be using the port)"
                    ));
                }
            }
        }
    }

    struct Pending {
        portal: String,
        client_id: String,
        redirect: String,
        verifier: String,
        state: String,
    }

    fn serve(listener: &TcpListener, cancel: &AtomicBool, p: &Pending) -> Result<Info, String> {
        let deadline = Instant::now() + SIGN_IN_TIMEOUT;
        loop {
            if cancel.load(Ordering::SeqCst) {
                return Err("the sign-in was cancelled".into());
            }
            if Instant::now() >= deadline {
                return Err("the sign-in timed out: the browser didn't come back within five minutes".into());
            }
            let mut stream = match listener.accept() {
                Ok((s, _)) => s,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted | std::io::ErrorKind::ConnectionAborted
                    ) =>
                {
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
                Err(e) => return Err(format!("the sign-in's callback server failed: {e}")),
            };
            let _ = stream.set_nonblocking(false);
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
            let Some(target) = request_target(&mut stream) else {
                respond(&mut stream, 400, "Bad request");
                continue;
            };
            let (path, query) = target.split_once('?').unwrap_or((target.as_str(), ""));
            if path != super::CALLBACK {
                respond(&mut stream, 404, "Not found");
                continue;
            }
            let params = super::query_params(query);
            let get = |k: &str| params.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str()).unwrap_or("");
            if params.iter().filter(|(k, _)| k == "state").count() != 1 || get("state") != p.state {
                respond(&mut stream, 400, "State mismatch");
                continue;
            }
            if !get("error").is_empty() {
                let why = if get("error") == "access_denied" {
                    "the sign-in was declined".to_string()
                } else {
                    format!("the portal said {}", get("error"))
                };
                respond(&mut stream, 200, &page("Sign-in failed", &why));
                return Err(why);
            }
            let code = get("code");
            if code.is_empty() {
                respond(&mut stream, 400, "Missing authorization code");
                continue;
            }
            let result = finish(p, code, cancel);
            match &result {
                Ok(info) => {
                    let who = if info.username.is_empty() { String::new() } else { format!(" as {}", info.username) };
                    respond(
                        &mut stream,
                        200,
                        &page("Signed in", &format!("RustGIS is signed in to {}{who}. You can close this tab.", info.portal)),
                    );
                }
                Err(e) => respond(&mut stream, 200, &page("Sign-in failed", e)),
            }
            return result;
        }
    }

    /// Exchanges the code, finds what the token covers and keeps the session.
    fn finish(p: &Pending, code: &str, cancel: &AtomicBool) -> Result<Info, String> {
        let body = post_form(&super::endpoint(&p.portal, "token"), &super::exchange_body(&p.client_id, code, &p.redirect, &p.verifier))?;
        let grant = super::parse_grant(&body, super::now_secs())?;
        let scopes = discover(&p.portal, &grant.access_token);
        if cancel.load(Ordering::SeqCst) {
            revoke_all(vec![Revocation {
                endpoint: super::endpoint(&p.portal, "revokeToken"),
                body: match &grant.refresh_token {
                    Some(r) => super::revoke_body(&p.client_id, r, true),
                    None => super::revoke_body(&p.client_id, &grant.access_token, false),
                },
            }]);
            return Err("the sign-in was cancelled".into());
        }
        super::sign_in(&p.portal, &p.client_id, grant, scopes);
        // Tiles and documents refused before the sign-in are asked for again.
        crate::cache::clear();
        super::signed_in(&p.portal).ok_or_else(|| "the sign-in wasn't kept".into())
    }

    /// What a portal's token covers: its organization's id (ArcGIS Online) or its federated
    /// servers (ArcGIS Enterprise). What can't be read leaves the portal's own host.
    fn discover(portal: &str, token: &str) -> Vec<super::Scope> {
        let answer = get_json(&super::discovery_url(portal), token).ok();
        super::discovered_scopes(portal, answer.as_deref())
    }

    /// Like [`authorization`](super::authorization), renewing the access token first when it is
    /// about to expire. A refused renewal ends the session; an unreachable portal keeps it.
    pub fn authorization_fresh(url: &str) -> Option<String> {
        if super::renewal_due(url).is_some() {
            // One renewal at a time: the others wait for it, then find the token fresh.
            static RENEWING: Mutex<()> = Mutex::new(());
            let _one = RENEWING.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(r) = super::renewal_due(url) {
                let answer = post_form(&r.endpoint, &r.body);
                super::renewal_done(&r, answer.as_deref().map_err(String::clone));
            }
        }
        super::authorization(url)
    }

    /// Sends revocations off the caller's thread, on a best-effort basis.
    pub fn revoke_all(revocations: Vec<Revocation>) {
        if revocations.is_empty() {
            return;
        }
        std::thread::spawn(move || {
            for r in revocations {
                let _ = post_form(&r.endpoint, &r.body);
            }
        });
    }

    /// The request line's target of an HTTP request.
    fn request_target(stream: &mut TcpStream) -> Option<String> {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 2048];
        // A client trickling bytes doesn't hold the server past a few seconds.
        let until = Instant::now() + Duration::from_secs(5);
        while !buf.windows(2).any(|w| w == b"\r\n") {
            if Instant::now() >= until {
                return None;
            }
            let n = stream.read(&mut chunk).ok()?;
            if n == 0 || buf.len() > 16 << 10 {
                return None;
            }
            buf.extend_from_slice(chunk.get(..n)?);
        }
        let line = std::str::from_utf8(&buf).ok()?.lines().next()?;
        let mut parts = line.split(' ');
        (parts.next()? == "GET").then_some(())?;
        parts.next().map(str::to_string)
    }

    fn respond(stream: &mut TcpStream, status: u16, body: &str) {
        let reason = match status {
            200 => "OK",
            400 => "Bad Request",
            _ => "Not Found",
        };
        let kind = if body.starts_with("<!doctype") { "text/html" } else { "text/plain" };
        let _ = write!(
            stream,
            "HTTP/1.1 {status} {reason}\r\nContent-Type: {kind}; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.flush();
    }

    fn escape(s: &str) -> String {
        s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
    }

    fn page(title: &str, text: &str) -> String {
        format!(
            "<!doctype html><html><head><meta charset=\"utf-8\"><title>RustGIS: {t}</title></head><body style=\"font-family:sans-serif;max-width:32em;margin:4em auto\"><h1>{t}</h1><p>{m}</p></body></html>",
            t = escape(title),
            m = escape(text)
        )
    }
}

#[cfg(test)]
mod tests;
