//! Signing in with ChatGPT, so the user's ChatGPT plan pays for the assistant instead of an API
//! key: OpenAI's Sign in with ChatGPT for open-source apps
//! (<https://developers.openai.com/siwc/token-sharing-open-source>).
//!
//! The first sign-in registers RustGIS as the user's own agent (`client_id=dynamic_agent_client`,
//! with this computer's host id); the browser comes back with the client id OpenAI issued, which
//! later sign-ins and renewals use. It is OAuth 2.0 authorization code with PKCE in the system
//! browser, returning to a short-lived server on `127.0.0.1`. The id token is verified against
//! OpenAI's published keys, and the grant must include `chatgpt.tokens.use.direct`, the
//! permission to use the plan. The access token is a bearer token for the public Responses API
//! (`https://api.openai.com/v1`), renewed with its rotating refresh token, and revoked on sign-out.
//!
//! The pure parts (the authorization page, the token requests and replies, the credentials file's
//! shape, deciding when to renew) are plain data for the tests; the files, the system keychain,
//! the id token's signature and the callback server are desktop only. The settings folder's
//! `chatgpt-auth.json` keeps the registrations; their tokens go to the system keychain
//! ([`set_secret_store`]). Tokens are never logged, and the `Debug` form of [`Credentials`] hides
//! them.

use base64::Engine;
use serde_json::{Value, json};

use crate::provider::{HttpRequest, HttpResponse};

/// The OpenAI sign-in service (the id token's issuer).
pub const ISSUER: &str = "https://auth.openai.com";
/// Where the browser signs in.
pub const AUTHORIZE_URL: &str = "https://auth.openai.com/api/accounts/authorize";
/// Where codes and refresh tokens are exchanged.
pub const TOKEN_URL: &str = "https://auth.openai.com/api/accounts/oauth/token";
/// OpenAI's OpenID configuration: its signing keys and revocation endpoint.
pub const DISCOVERY_URL: &str = "https://auth.openai.com/.well-known/openid-configuration";
/// The API the plan's tokens are for (the `resource` of every grant).
pub const API: &str = "https://api.openai.com/v1";
/// The client id of a first sign-in, which registers RustGIS for the account.
pub const REGISTER: &str = "dynamic_agent_client";
/// The name the registration suggests (the user may edit it).
pub const AGENT_NAME: &str = "RustGIS";
/// Identity, a refresh token, and the permission to use the plan.
pub const SCOPES: &str = "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
/// The permission to use the user's ChatGPT plan.
pub const PLAN_SCOPE: &str = "chatgpt.tokens.use.direct";
/// The callback port tried first (another free one otherwise).
pub const PORT: u16 = 1455;
/// Where the user reviews and limits the plan's usage.
pub const USAGE_URL: &str = "https://chatgpt.com/settings/usage?tab=overview";
/// Access tokens are renewed this long before they expire.
pub const REFRESH_MARGIN: i64 = 5 * 60;
/// The clock skew allowed when checking an id token's expiry.
pub const CLOCK_SKEW: i64 = 5 * 60;
/// A signed-in configuration's key: a marker, not a credential (`send` authorizes each request
/// with the current access token).
pub const SIGNED_IN: &str = "chatgpt-sign-in";

/// Where a sign-in's tokens are kept.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kept {
    /// There are none.
    #[default]
    Nowhere,
    /// In the system keychain: the refresh token is there; the access and id tokens stay with
    /// the process that got them (a process without one renews first).
    Keychain,
    /// Still in the settings folder's file, from before the keychain: they move to the keychain
    /// the next time it's available.
    SettingsFile,
    /// In memory only, for this session: the keychain wasn't available to remember them.
    Session,
}

/// The registration of RustGIS for a ChatGPT account and, while signed in, its tokens.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Credentials {
    /// The client id OpenAI issued for this account (kept after signing out).
    pub client_id: String,
    /// The verified id token's subject: who signed in.
    pub subject: String,
    /// The verified email, for display.
    pub email: String,
    /// The retained identity token.
    pub id_token: String,
    /// The bearer credential.
    pub access_token: String,
    /// The rotating renewal credential.
    pub refresh_token: String,
    /// When the access token expires (Unix seconds; 0 when unknown).
    pub expires_at: i64,
    /// The scopes granted.
    pub scopes: Vec<String>,
    /// The user declined the plan's use: the next sign-in asks for it again.
    pub plan_declined: bool,
    /// When the tokens were obtained (RFC 3339, informational).
    pub saved_at: String,
    /// Changes with each sign-in and sign-out (not renewal or selection): work begun before
    /// either is refused after it.
    pub session: String,
    /// Names the refresh token in the keychain: the keychain's must carry the same, or it is out
    /// of date. Empty when the keychain holds no current one for this registration.
    pub rev: String,
    /// The keychain may hold an item for this registration (set before each keychain write,
    /// cleared once one is deleted), so removing it must delete that item first.
    pub in_keychain: bool,
    /// Where the tokens are kept (never saved).
    pub kept: Kept,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("client_id", &self.client_id)
            .field("signed_in", &self.signed_in())
            .field("scopes", &self.scopes)
            .field("saved_at", &self.saved_at)
            .field("kept", &self.kept)
            .finish_non_exhaustive()
    }
}

/// Who is signed in, for the settings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Account {
    /// The verified email, for display.
    pub email: String,
    /// The id token's subject.
    pub subject: String,
    /// Where the sign-in's tokens are kept.
    pub kept: Kept,
}

/// The registration a request is for: its issued client id, the verified subject, and the
/// sign-in session it began in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Binding {
    pub client_id: String,
    pub subject: String,
    pub session: String,
}

impl Credentials {
    /// The registration these credentials are for.
    pub fn binding(&self) -> Binding {
        Binding { client_id: self.client_id.clone(), subject: self.subject.clone(), session: self.session.clone() }
    }

    /// Whether there are tokens to use: an access token, or a refresh token here or in the
    /// keychain to get one.
    pub fn signed_in(&self) -> bool {
        !self.access_token.is_empty() || !self.refresh_token.is_empty() || self.kept == Kept::Keychain
    }

    /// Whether the grant lets RustGIS use the plan.
    pub fn plan_allowed(&self) -> bool {
        self.scopes.iter().any(|s| s == PLAN_SCOPE)
    }

    /// The registration alone: what a sign-out keeps for the next sign-in.
    pub fn registration(&self) -> Credentials {
        Credentials {
            client_id: self.client_id.clone(),
            subject: self.subject.clone(),
            email: self.email.clone(),
            plan_declined: self.plan_declined,
            session: self.session.clone(),
            in_keychain: self.in_keychain,
            ..Default::default()
        }
    }

    /// A stable label that distinguishes registrations with the same email.
    pub fn label(&self) -> String {
        let who = if self.email.is_empty() { &self.subject } else { &self.email };
        format!("{who} · {}", self.client_id)
    }

    /// The verified identity for display.
    pub fn account(&self) -> Account {
        Account { email: self.email.clone(), subject: self.subject.clone(), kept: self.kept }
    }

    /// Whether there are tokens worth keeping (an access or refresh token).
    pub fn has_tokens(&self) -> bool {
        !(self.access_token.is_empty() && self.refresh_token.is_empty())
    }

    /// The registration without its tokens.
    pub fn without_tokens(&self) -> Credentials {
        Credentials {
            id_token: String::new(),
            access_token: String::new(),
            refresh_token: String::new(),
            kept: Kept::Nowhere,
            ..self.clone()
        }
    }

    /// Reads a saved registration. Tokens are read too: older versions saved them in the file.
    pub fn from_json(v: &Value) -> Result<Credentials, String> {
        let field = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string();
        let mut c = Credentials {
            client_id: field("client_id"),
            subject: field("subject"),
            email: field("email"),
            id_token: field("id_token"),
            access_token: field("access_token"),
            refresh_token: field("refresh_token"),
            expires_at: v.get("expires_at").and_then(Value::as_i64).unwrap_or(0),
            scopes: v.get("scopes").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect(),
            plan_declined: v.get("plan_declined").and_then(Value::as_bool).unwrap_or(false),
            saved_at: field("saved_at"),
            session: field("session"),
            rev: field("rev"),
            in_keychain: v.get("keychain").and_then(Value::as_bool).unwrap_or(false),
            kept: Kept::Nowhere,
        };
        if c.client_id.is_empty() || c.client_id == REGISTER {
            return Err("the sign-in has no registration".into());
        }
        // Older versions kept the tokens in the file.
        if c.has_tokens() {
            c.kept = Kept::SettingsFile;
        }
        Ok(c)
    }

    /// The registration as saved in the settings folder: never a token.
    pub fn to_json(&self) -> Value {
        json!({
            "issuer": ISSUER,
            "client_id": self.client_id,
            "subject": self.subject,
            "email": self.email,
            "token_type": "Bearer",
            "expires_at": self.expires_at,
            "scopes": self.scopes,
            "plan_declined": self.plan_declined,
            "saved_at": self.saved_at,
            "session": self.session,
            "rev": self.rev,
            "keychain": self.in_keychain,
        })
    }

    /// Whether the access token expires within [`REFRESH_MARGIN`] of `now` (Unix seconds), or
    /// when that isn't known.
    pub fn expires_soon(&self, now: i64) -> bool {
        self.expires_at <= 0 || self.expires_at.saturating_sub(REFRESH_MARGIN) <= now
    }

    /// Puts the access token on a request (replacing any credentials there).
    pub fn authorize(&self, mut req: HttpRequest) -> HttpRequest {
        req.headers.retain(|(k, _)| !k.eq_ignore_ascii_case("authorization"));
        req.headers.push(("Authorization".to_string(), format!("Bearer {}", self.access_token)));
        req
    }
}

/// The payload of a JWT, unverified.
pub(crate) fn jwt_payload(jwt: &str) -> Option<Value> {
    let mut parts = jwt.split('.');
    let (h, p, s) = (parts.next()?, parts.next()?, parts.next()?);
    if h.is_empty() || p.is_empty() || s.is_empty() || parts.next().is_some() {
        return None;
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(p).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Checks an id token's claims (its signature is checked separately): the issuer, the audience
/// (the issued client id), the expiry and the nonce of this sign-in. Gives the subject and email.
pub fn check_id_claims(claims: &Value, client_id: &str, nonce: &str, now: i64) -> Result<Account, String> {
    let s = |k: &str| claims.get(k).and_then(Value::as_str).unwrap_or("");
    if s("iss") != ISSUER {
        return Err("the id token isn't from OpenAI".into());
    }
    let aud_ok = match claims.get("aud") {
        Some(Value::String(a)) => a == client_id,
        Some(Value::Array(a)) => a.iter().any(|x| x.as_str() == Some(client_id)),
        _ => false,
    };
    if client_id.is_empty() || !aud_ok {
        return Err("the id token is for another app".into());
    }
    if claims.get("exp").and_then(Value::as_i64).is_none_or(|exp| exp.saturating_add(CLOCK_SKEW) < now) {
        return Err("the id token has expired".into());
    }
    if nonce.is_empty() || s("nonce") != nonce {
        return Err("the id token isn't from this sign-in".into());
    }
    if s("sub").is_empty() {
        return Err("the id token names no account".into());
    }
    Ok(Account { email: s("email").to_string(), subject: s("sub").to_string(), kept: Kept::Nowhere })
}

/// What a token endpoint gave.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Grant {
    /// The bearer credential.
    pub access_token: String,
    /// The rotating renewal credential.
    pub refresh_token: String,
    /// The retained identity token.
    pub id_token: String,
    /// Seconds until expiry.
    pub expires_in: i64,
    /// The scopes granted, when the reply says.
    pub scopes: Option<Vec<String>>,
}

impl std::fmt::Debug for Grant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Grant").field("expires_in", &self.expires_in).field("scopes", &self.scopes).finish_non_exhaustive()
    }
}

/// Why a token endpoint refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GrantError {
    /// The refresh token or code can't be used again: sign in again.
    Unusable,
    /// Anything else (a message without credentials).
    Other(String),
}

impl GrantError {
    /// A credential-free diagnostic.
    pub fn message(&self, what: &str) -> String {
        match self {
            GrantError::Unusable => format!("{what}: the sign-in has expired or was revoked; sign in again"),
            GrantError::Other(why) => format!("{what}: {why}"),
        }
    }
}

/// A token endpoint's reply, or why it refused.
pub fn parse_grant(resp: &HttpResponse) -> Result<Grant, GrantError> {
    let v: Value = serde_json::from_slice(&resp.body).unwrap_or(Value::Null);
    if !(200..300).contains(&resp.status) {
        let code = v.get("error").and_then(|e| e.as_str().or_else(|| e.get("code").and_then(Value::as_str))).unwrap_or("");
        // OAuth responses are untrusted and may echo submitted credentials. Report only
        // recognized protocol codes, never raw descriptions or response bodies.
        return Err(match code {
            "invalid_grant"
            | "invalid_refresh_token"
            | "token_expired"
            | "refresh_token_expired"
            | "refresh_token_invalidated"
            | "refresh_token_reused" => GrantError::Unusable,
            "invalid_request" | "invalid_client" | "unauthorized_client" | "unsupported_grant_type" | "invalid_scope" => {
                GrantError::Other(code.to_string())
            }
            _ => GrantError::Other(format!("HTTP {}", resp.status)),
        });
    }
    let field = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string();
    let g = Grant {
        access_token: field("access_token"),
        refresh_token: field("refresh_token"),
        id_token: field("id_token"),
        expires_in: v.get("expires_in").and_then(Value::as_i64).unwrap_or(0),
        scopes: v.get("scope").and_then(Value::as_str).map(|s| s.split_whitespace().map(str::to_string).collect()),
    };
    if g.access_token.is_empty() {
        return Err(GrantError::Other("the reply has no access token".into()));
    }
    Ok(g)
}

/// When a grant's access token expires: from `expires_in`, else the token's own `exp`.
fn expiry(g: &Grant, now: i64) -> i64 {
    if g.expires_in > 0 {
        return now.saturating_add(g.expires_in);
    }
    jwt_payload(&g.access_token).and_then(|v| v.get("exp").and_then(Value::as_i64)).unwrap_or(0)
}

/// The credentials of a completed sign-in: the registration's client id, the verified account,
/// and the grant.
pub fn signed_in(client_id: &str, account: &Account, g: &Grant, now: i64) -> Credentials {
    let scopes = g.scopes.clone().unwrap_or_default();
    Credentials {
        client_id: client_id.to_string(),
        subject: account.subject.clone(),
        email: account.email.clone(),
        id_token: g.id_token.clone(),
        access_token: g.access_token.clone(),
        refresh_token: g.refresh_token.clone(),
        expires_at: expiry(g, now),
        plan_declined: !scopes.iter().any(|s| s == PLAN_SCOPE),
        scopes,
        saved_at: rfc3339(now),
        session: String::new(),
        rev: String::new(),
        in_keychain: false,
        kept: Kept::Nowhere,
    }
}

/// The credentials after a renewal: the access token, expiry, scopes and rotated refresh token
/// replaced together.
pub fn renewed(old: &Credentials, g: &Grant, now: i64) -> Credentials {
    let mut c = old.clone();
    c.access_token = g.access_token.clone();
    if !g.refresh_token.is_empty() {
        c.refresh_token = g.refresh_token.clone();
    }
    if !g.id_token.is_empty() {
        c.id_token = g.id_token.clone();
    }
    if let Some(s) = &g.scopes {
        c.scopes = s.clone();
        c.plan_declined = !c.plan_allowed();
    }
    c.expires_at = expiry(g, now);
    c.saved_at = rfc3339(now);
    c
}

/// Percent-encodes a query or form value.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn encode(params: &[(&str, &str)]) -> String {
    params.iter().map(|(k, v)| format!("{k}={}", urlencode(v))).collect::<Vec<_>>().join("&")
}

fn form_post(url: &str, params: &[(&str, &str)]) -> HttpRequest {
    HttpRequest {
        method: "POST",
        url: url.to_string(),
        headers: vec![("Content-Type".to_string(), "application/x-www-form-urlencoded".to_string())],
        body: Some(encode(params)),
    }
}

/// Decodes a query value (`+` is a space).
pub fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes.get(i) {
            Some(b'+') => out.push(b' '),
            Some(b'%') => {
                let hex = bytes.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
                match hex {
                    Some(b) => {
                        out.push(b);
                        i += 2;
                    }
                    None => out.push(b'%'),
                }
            }
            Some(b) => out.push(*b),
            None => break,
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The (decoded) parameters of a query string.
pub fn query_params(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| p.split_once('=').unwrap_or((p, "")))
        .map(|(k, v)| (urldecode(k), urldecode(v)))
        .collect()
}

/// Only the Responses API and the model list may receive the plan's access token.
pub fn api_url(url: &str) -> bool {
    let Some(rest) = url.trim().strip_prefix(API) else { return false };
    let path = rest.split('?').next().unwrap_or("");
    (path == "/responses" || path == "/models") && !url.contains(['\\', '#', '@'])
}

#[cfg(any(test, not(target_arch = "wasm32")))]
pub(crate) fn callback_state_matches(params: &[(String, String)], expected: &str) -> bool {
    let mut states = params.iter().filter(|(key, _)| key == "state");
    !expected.is_empty() && states.next().is_some_and(|(_, value)| value == expected) && states.next().is_none()
}

/// The client id a callback gives for a sign-in started with `requested`: the issued one for a
/// first sign-in (which must say it), the saved one otherwise (which it may repeat, never change).
pub fn callback_client_id(params: &[(String, String)], requested: &str) -> Result<String, String> {
    let mut ids = params.iter().filter(|(k, _)| k == "client_id").map(|(_, v)| v.trim());
    let (given, extra) = (ids.next().filter(|v| !v.is_empty()), ids.next());
    if extra.is_some() {
        return Err("the sign-in returned more than one client id".into());
    }
    match (requested == REGISTER, given) {
        (true, Some(id)) if id != REGISTER => Ok(id.to_string()),
        (true, _) => Err("the registration didn't complete; try again".into()),
        (false, Some(id)) if id != requested => Err("the sign-in returned another registration; try again".into()),
        (false, _) => Ok(requested.to_string()),
    }
}

/// The PKCE challenge (S256) of a verifier.
pub fn pkce_challenge(verifier: &str) -> String {
    use sha2::Digest;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier.as_bytes()))
}

/// A version 4 UUID from 16 random bytes, as a host id (`urn:uuid:…`).
pub fn host_id_from(b: [u8; 16]) -> String {
    let mut id = String::from("urn:uuid:");
    for (i, x) in b.iter().enumerate() {
        // The version (4) and variant (RFC 9562) bits.
        let x = match i {
            6 => (x & 0x0f) | 0x40,
            8 => (x & 0x3f) | 0x80,
            _ => *x,
        };
        if matches!(i, 4 | 6 | 8 | 10) {
            id.push('-');
        }
        id.push_str(&format!("{x:02x}"));
    }
    id
}

/// Whether a saved host id has the shape this app writes.
pub fn valid_host_id(id: &str) -> bool {
    id.strip_prefix("urn:uuid:").is_some_and(|u| {
        u.len() == 36 && u.char_indices().all(|(i, c)| if matches!(i, 8 | 13 | 18 | 23) { c == '-' } else { c.is_ascii_hexdigit() })
    })
}

/// One attempt at signing in.
pub struct Authorization<'a> {
    /// [`REGISTER`] for a first sign-in, else the account's issued client id.
    pub client_id: &'a str,
    /// The stable identity of this computer.
    pub host_id: &'a str,
    /// The bound loopback callback.
    pub redirect_uri: &'a str,
    /// The PKCE S256 challenge.
    pub challenge: &'a str,
    /// The random callback binding.
    pub state: &'a str,
    /// The random identity-token binding.
    pub nonce: &'a str,
    /// The account's email, to preselect it.
    pub login_hint: &'a str,
    /// The account's last id token, to skip choosing the account again (never after a sign-out;
    /// keep URLs carrying it out of logs, terminals and clipboards).
    pub id_token_hint: &'a str,
    /// Ask again for the permissions (after the plan's use was declined).
    pub consent: bool,
}

/// The page to open in the browser.
pub fn authorize_url(a: &Authorization) -> String {
    let mut params = vec![
        ("response_type", "code"),
        ("client_id", a.client_id),
        ("redirect_uri", a.redirect_uri),
        ("scope", SCOPES),
        ("resource", API),
        ("state", a.state),
        ("nonce", a.nonce),
        ("code_challenge", a.challenge),
        ("code_challenge_method", "S256"),
        ("ext_agent_host_id", a.host_id),
    ];
    if a.client_id == REGISTER {
        params.push(("agent_name_hint", AGENT_NAME));
    } else {
        if !a.id_token_hint.is_empty() {
            params.push(("id_token_hint", a.id_token_hint));
        }
        if !a.login_hint.is_empty() {
            params.push(("login_hint", a.login_hint));
        }
    }
    if a.consent {
        params.push(("prompt", "consent"));
    }
    format!("{AUTHORIZE_URL}?{}", encode(&params))
}

/// The request exchanging the browser's code for tokens.
pub fn exchange_request(client_id: &str, code: &str, redirect_uri: &str, verifier: &str) -> HttpRequest {
    form_post(
        TOKEN_URL,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", client_id),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", redirect_uri),
            ("resource", API),
        ],
    )
}

/// The request renewing the tokens (the scope is left out to keep the grant).
pub fn refresh_request(c: &Credentials) -> HttpRequest {
    form_post(
        TOKEN_URL,
        &[("grant_type", "refresh_token"), ("client_id", &c.client_id), ("refresh_token", &c.refresh_token), ("resource", API)],
    )
}

/// The request revoking a refresh token at `endpoint` (from the OpenID configuration).
pub fn revoke_request(endpoint: &str, client_id: &str, refresh_token: &str) -> HttpRequest {
    form_post(endpoint, &[("token", refresh_token), ("token_type_hint", "refresh_token"), ("client_id", client_id)])
}

/// An endpoint of OpenAI's OpenID configuration, if it is on the sign-in service.
pub fn discovered(config: &Value, key: &str) -> Option<String> {
    config.get(key).and_then(Value::as_str).filter(|u| u.strip_prefix(ISSUER).is_some_and(|r| r.starts_with('/'))).map(str::to_string)
}

/// `secs` since the Unix epoch as RFC 3339 (`2026-10-14T13:34:16Z`).
pub fn rfc3339(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil date from days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

// ---------------------------------------------------------------------------------------------
// The desktop and the command line: the files, the id token's signature, renewing, signing in
// and out.

#[cfg(not(target_arch = "wasm32"))]
pub use native::*;

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use base64::Engine;
    use serde_json::Value;

    use super::{
        Account, Authorization, Binding, Credentials, DISCOVERY_URL, GrantError, Kept, PORT, REGISTER, authorize_url, callback_client_id,
        check_id_claims, discovered, exchange_request, host_id_from, jwt_payload, parse_grant, query_params, refresh_request, renewed,
        revoke_request, valid_host_id,
    };
    use crate::provider::{HttpRequest, HttpResponse};
    use crate::{Error, Recovery, http};
    use rustgis_engine::data::dirs::config_dir;
    use rustgis_engine::data::secrets::{Secret, SecretStore};
    use std::collections::BTreeMap;
    use std::sync::{LazyLock, MutexGuard};

    /// The credentials' file name, in the app's settings folder.
    const FILE: &str = "chatgpt-auth.json";
    /// This computer's host id, kept apart from the credentials so signing out keeps it.
    const HOST_FILE: &str = "chatgpt-host-id";

    /// How long the browser has to come back.
    const LOGIN_TIMEOUT: Duration = Duration::from_secs(10 * 60);

    type Send<'a> = &'a mut dyn FnMut(&HttpRequest) -> Result<HttpResponse, Error>;

    /// Where the sign-in is kept: the settings folder the desktop app keeps its preferences in.
    pub fn auth_path() -> Option<PathBuf> {
        config_dir().map(|d| d.join(FILE))
    }

    fn now() -> i64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
    }

    // Store locks cover only file transactions. Renewal locks may cover network I/O.
    fn file_lock(path: &Path, extension: &str) -> Result<std::fs::File, Error> {
        let dir = path.parent().ok_or("no settings folder")?;
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path.with_extension(extension))
            .map_err(|e| e.to_string())?;
        file.lock().map_err(|e| e.to_string())?;
        Ok(file)
    }

    // The store lock covers every read, compare and write of the file and of the keychain. Only
    // a renewal, a sign-out and a removal read the keychain, under the store lock and (for a
    // renewal) the renewal lock, so two processes never both use a refresh token that rotates.
    fn store(path: &Path) -> Result<std::fs::File, Error> {
        file_lock(path, "store-lock")
    }

    // Where a registration's tokens are, from its file entry (`rev`, `keychain`) and this
    // process's `held` tokens:
    //
    // | file entry                     | refresh token          | access and id tokens  | Kept         |
    // |--------------------------------|------------------------|-----------------------|--------------|
    // | tokens in it (older version)   | the file               | the file              | SettingsFile |
    // | `rev` set                      | keychain, same `rev`   | held (same session)   | Keychain     |
    // | no `rev`, held for the session | held                   | held                  | Session      |
    // | no `rev`, nothing held         | none: signed out       | none                  | Nowhere      |
    //
    // `keychain` says an item may exist (set before the first write, cleared after a delete), so
    // removal deletes it even when `rev` no longer names it.

    /// The system keychain, and the tokens this process holds by account (see the table above).
    pub(crate) struct Vault {
        store: Mutex<Option<Arc<dyn SecretStore>>>,
        held: Mutex<BTreeMap<(String, String), Held>>,
    }

    /// Tokens this process holds for one sign-in session.
    struct Held {
        session: String,
        kept: Kept,
        id_token: String,
        access_token: String,
        /// Only for [`Kept::Session`]: the keychain keeps the others'.
        refresh_token: String,
        expires_at: i64,
    }

    impl Held {
        fn of(c: &Credentials) -> Held {
            Held {
                session: c.session.clone(),
                kept: c.kept,
                id_token: c.id_token.clone(),
                access_token: c.access_token.clone(),
                refresh_token: if c.kept == Kept::Session { c.refresh_token.clone() } else { String::new() },
                expires_at: c.expires_at,
            }
        }

        fn apply(&self, entry: &Credentials) -> Credentials {
            Credentials {
                id_token: self.id_token.clone(),
                access_token: self.access_token.clone(),
                refresh_token: self.refresh_token.clone(),
                expires_at: self.expires_at,
                kept: self.kept,
                ..entry.clone()
            }
        }
    }

    impl Vault {
        pub(crate) fn new(store: Option<Arc<dyn SecretStore>>) -> Vault {
            Vault { store: Mutex::new(store), held: Mutex::default() }
        }

        fn store(&self) -> Option<Arc<dyn SecretStore>> {
            self.store.lock().unwrap_or_else(|e| e.into_inner()).clone()
        }

        fn held(&self) -> MutexGuard<'_, BTreeMap<(String, String), Held>> {
            self.held.lock().unwrap_or_else(|e| e.into_inner())
        }
    }

    static VAULT: LazyLock<Vault> = LazyLock::new(|| Vault::new(None));

    fn vault() -> &'static Vault {
        &VAULT
    }

    /// Gives the system keychain that keeps the sign-in's refresh tokens (the desktop app's).
    /// Without one (the command line), a sign-in from before the keychain keeps working from the
    /// settings folder, and a new one lasts for the session only.
    pub fn set_secret_store(store: Option<Arc<dyn SecretStore>>) {
        *vault().store.lock().unwrap_or_else(|e| e.into_inner()) = store;
    }

    /// Why a keychain sign-in can't be used, signed out or removed here.
    const KEYCHAIN_ELSEWHERE: &str =
        "this ChatGPT sign-in is kept in the system keychain, which isn't available here; use, sign out or remove it in the RustGIS app";

    /// The keychain account of a registration's refresh token.
    fn secret_name(client_id: &str, subject: &str) -> String {
        format!("chatgpt/{client_id}/{subject}")
    }

    fn id(c: &Credentials) -> (String, String) {
        (c.client_id.clone(), c.subject.clone())
    }

    /// The keychain's value: the refresh token and the `rev` it is for, without the much longer
    /// access and id tokens, for Windows Credential Manager's 2560 bytes of UTF-16 (a real refresh
    /// token's length is not verified here).
    fn secret(c: &Credentials) -> Secret {
        Secret::new(serde_json::json!({"rev": c.rev, "refresh_token": c.refresh_token}).to_string())
    }

    fn keychain_error(what: &str, e: &rustgis_engine::data::secrets::SecretError) -> Error {
        Error::new(Recovery::Auth, format!("{what}: {e}"))
    }

    #[derive(Default)]
    struct Accounts {
        /// The registrations. Only those from before the keychain ([`Kept::SettingsFile`]) carry
        /// tokens: the file's own.
        entries: Vec<Credentials>,
        active: Option<(String, String)>,
    }

    impl Accounts {
        fn selected(&self) -> Option<&Credentials> {
            self.entries.iter().find(|c| self.active.as_ref() == Some(&id(c)))
        }

        fn put(&mut self, c: &Credentials) {
            self.entries.retain(|old| old.client_id != c.client_id || old.subject != c.subject);
            self.entries.push(c.clone());
            self.active = Some(id(c));
        }
    }

    fn read_accounts(path: &Path) -> Result<Accounts, Error> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Accounts::default()),
            Err(e) => return Err(e.to_string().into()),
        };
        let v: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|_| "the sign-in store isn't JSON")?;
        if let Some(entries) = v.get("accounts").and_then(Value::as_array) {
            let entries = entries.iter().map(Credentials::from_json).collect::<Result<Vec<_>, _>>()?;
            let active =
                v.get("active").and_then(|a| Some((a.get("client_id")?.as_str()?.to_string(), a.get("subject")?.as_str()?.to_string())));
            Ok(Accounts { entries, active })
        } else {
            let mut accounts = Accounts::default();
            accounts.put(&Credentials::from_json(&v)?);
            Ok(accounts)
        }
    }

    fn write_accounts(path: &Path, accounts: &Accounts) -> Result<(), Error> {
        let active = accounts.active.as_ref().map(|(client, subject)| serde_json::json!({"client_id": client, "subject": subject}));
        let entry = |c: &Credentials| {
            let mut v = c.to_json();
            // Tokens from before the keychain stay until it takes them.
            if c.kept == Kept::SettingsFile {
                v["id_token"] = c.id_token.clone().into();
                v["access_token"] = c.access_token.clone().into();
                v["refresh_token"] = c.refresh_token.clone().into();
            }
            v
        };
        write_private(
            path,
            &serde_json::json!({"version": 2, "active": active, "accounts": accounts.entries.iter().map(entry).collect::<Vec<_>>()})
                .to_string(),
        )
    }

    /// A file entry with the tokens this process can use (see the table above), without reading
    /// the keychain: a keychain sign-in's refresh token is read when needed ([`load_refresh`]).
    /// Tokens still in the file (from before the keychain) move to the keychain if it takes
    /// them; the file changes only then. Under the store lock.
    fn with_tokens(v: &Vault, path: &Path, accounts: &mut Accounts, entry: Credentials) -> Result<Credentials, Error> {
        if entry.kept == Kept::SettingsFile {
            // Without a refresh token there is nothing the keychain could renew it with: the
            // file's access token keeps working until it expires.
            let Some(store) = v.store().filter(|_| !entry.refresh_token.is_empty()) else { return Ok(entry) };
            let moved = Credentials { rev: random_token()?, in_keychain: true, kept: Kept::Keychain, ..entry.clone() };
            if let Err(e) = store.save(&secret_name(&entry.client_id, &entry.subject), &secret(&moved)) {
                log::warn!("the ChatGPT sign-in stays in the settings folder: {e}");
                return Ok(entry);
            }
            // Out of the file only now that the keychain has them.
            let mut after = Accounts { entries: accounts.entries.clone(), active: accounts.active.clone() };
            if let Some(e) = after.entries.iter_mut().find(|e| id(e) == id(&moved)) {
                *e = moved.without_tokens();
            }
            if let Err(e) = write_accounts(path, &after) {
                // The keychain's copy is the file's own token; the next use moves it again.
                log::warn!("the ChatGPT sign-in stays in the settings folder: {e}");
                return Ok(entry);
            }
            *accounts = after;
            v.held().insert(id(&moved), Held::of(&moved));
            return Ok(moved);
        }
        let held = v.held();
        let mine = held.get(&id(&entry)).filter(|h| h.session == entry.session);
        if entry.rev.is_empty() {
            return Ok(match mine.filter(|h| h.kept == Kept::Session) {
                Some(h) => h.apply(&entry),
                None => entry,
            });
        }
        // Another process's renewal rotates the refresh token, not this process's access token.
        Ok(match mine.filter(|h| h.kept == Kept::Keychain) {
            Some(h) => h.apply(&entry),
            None => Credentials { expires_at: 0, kept: Kept::Keychain, ..entry },
        })
    }

    /// The refresh token of `c`: the keychain's for a keychain sign-in (none when it is out of
    /// date or missing). Under the store lock.
    fn load_refresh(v: &Vault, c: &Credentials) -> Result<String, Error> {
        if c.kept != Kept::Keychain {
            return Ok(c.refresh_token.clone());
        }
        let store = v.store().ok_or_else(|| Error::new(Recovery::Auth, KEYCHAIN_ELSEWHERE))?;
        let saved =
            store.load(&secret_name(&c.client_id, &c.subject)).map_err(|e| keychain_error("the ChatGPT sign-in can't be read", &e))?;
        let saved = saved.and_then(|s| serde_json::from_str::<Value>(s.expose()).ok());
        let field = |k: &str| saved.as_ref().and_then(|s| s.get(k)).and_then(Value::as_str).unwrap_or("").to_string();
        Ok(if field("rev") == c.rev { field("refresh_token") } else { String::new() })
    }

    /// Keeps a registration and its tokens, selects it and writes the file: the refresh token
    /// goes to the keychain, and the file names it (and the item) before the keychain is written.
    /// If the keychain doesn't take it (or there is none to take), tokens renewed from the file's
    /// own go back there, others last for this session; a new sign-in (`new`) over an account the
    /// keychain still holds is refused instead. Without tokens, any in the keychain are deleted.
    /// Gives the credentials as kept. Under the store lock.
    fn keep_tokens(v: &Vault, path: &Path, accounts: &mut Accounts, c: &Credentials, new: bool) -> Result<Credentials, Error> {
        let name = secret_name(&c.client_id, &c.subject);
        let store = v.store();
        let existing = accounts.entries.iter().find(|e| id(e) == id(c)).cloned();
        let mut in_keychain = c.in_keychain || existing.as_ref().is_some_and(|e| e.in_keychain);
        if !c.has_tokens() {
            v.held().remove(&id(c));
            // The empty `rev` already makes an item left behind unusable; `keychain` remembers it.
            match store.map(|s| s.delete(&name)) {
                Some(Ok(())) => in_keychain = false,
                Some(Err(e)) if in_keychain => log::warn!("the ChatGPT sign-in couldn't be deleted from the system keychain: {e}"),
                _ => {}
            }
            let entry = Credentials { rev: String::new(), in_keychain, ..c.without_tokens() };
            accounts.put(&entry);
            write_accounts(path, accounts)?;
            return Ok(entry);
        }
        // Only tokens renewed from the file's own may go back there; a new sign-in never does.
        let from_file = c.kept == Kept::SettingsFile && existing.as_ref().is_some_and(|e| e.kept == Kept::SettingsFile);
        let before = Accounts { entries: accounts.entries.clone(), active: accounts.active.clone() };
        let mut kept = Credentials { rev: random_token()?, in_keychain: true, kept: Kept::Keychain, ..c.clone() };
        let attempted = store.is_some() && !c.refresh_token.is_empty();
        let saved = match &store {
            None => Err("no system keychain here".to_string()),
            // Nothing outlasts the access token: nothing to keep.
            Some(_) if !attempted => Err("the sign-in has no refresh token".to_string()),
            Some(store) => {
                // Written first, so a failure after the keychain's write never leaves a token the
                // file can't name. Tokens from the file stay in it until the keychain has them.
                accounts.put(&if from_file { Credentials { kept: Kept::SettingsFile, ..kept.clone() } } else { kept.without_tokens() });
                write_accounts(path, accounts)?;
                store.save(&name, &secret(&kept)).map_err(|e| e.to_string())
            }
        };
        if saved.is_ok() {
            v.held().insert(id(c), Held::of(&kept));
            if from_file {
                accounts.put(&kept.without_tokens());
                write_accounts(path, accounts)?;
            }
            return Ok(kept);
        }
        if let Err(why) = saved {
            log::warn!("the system keychain didn't keep the ChatGPT sign-in: {why}");
        }
        // Replacing it would sign the account out where the keychain holds it, unrevoked.
        if new && existing.as_ref().is_some_and(|e| !e.rev.is_empty()) {
            if attempted {
                write_accounts(path, &before)?;
            }
            *accounts = before;
            return Err(Error::new(
                Recovery::Auth,
                "the system keychain didn't keep the new sign-in, and it holds this account's earlier one; sign in again where the keychain is available",
            ));
        }
        kept.kept = if from_file { Kept::SettingsFile } else { Kept::Session };
        kept.rev = String::new();
        kept.in_keychain = in_keychain || attempted;
        if from_file {
            v.held().remove(&id(c));
            accounts.put(&kept);
        } else {
            v.held().insert(id(c), Held::of(&kept));
            accounts.put(&kept.without_tokens());
        }
        write_accounts(path, accounts)?;
        Ok(kept)
    }

    /// The selected registration with its tokens. Under the store lock.
    fn selected_at(path: &Path, v: &Vault) -> Result<Option<Credentials>, Error> {
        let mut accounts = read_accounts(path)?;
        let Some(entry) = accounts.selected().cloned() else { return Ok(None) };
        with_tokens(v, path, &mut accounts, entry).map(Some)
    }

    /// Saves `c` and selects it. Under the store lock.
    fn write_credentials(path: &Path, v: &Vault, c: &Credentials) -> Result<Credentials, Error> {
        keep_tokens(v, path, &mut read_accounts(path)?, c, false)
    }

    /// The selected registration, including signed-out accounts. Reads the keychain only to move
    /// an older version's tokens into it (once); otherwise only the file.
    pub fn load_any() -> Option<Credentials> {
        let path = auth_path()?;
        let _guard = store(&path).ok()?;
        load_any_at(&path, vault())
    }

    fn load_any_at(path: &Path, v: &Vault) -> Option<Credentials> {
        match selected_at(path, v) {
            Ok(c) => c,
            Err(e) => {
                log::warn!("{e}");
                read_accounts(path).ok()?.selected().map(Credentials::without_tokens)
            }
        }
    }

    /// Saved registrations, including those signed out (without tokens).
    pub fn accounts() -> Result<Vec<Credentials>, Error> {
        let path = auth_path().ok_or("no settings folder")?;
        let _guard = store(&path)?;
        Ok(read_accounts(&path)?.entries.iter().map(Credentials::without_tokens).collect())
    }

    /// Selects an existing registration without signing it in.
    pub fn select(client_id: &str, subject: &str) -> Result<Credentials, Error> {
        select_at(&auth_path().ok_or("no settings folder")?, vault(), client_id, subject)
    }

    fn select_at(path: &Path, v: &Vault, client_id: &str, subject: &str) -> Result<Credentials, Error> {
        let _guard = store(path)?;
        let mut accounts = read_accounts(path)?;
        accounts.active = Some((client_id.to_string(), subject.to_string()));
        let entry = accounts.selected().cloned().ok_or("the saved account wasn't found")?;
        write_accounts(path, &accounts)?;
        with_tokens(v, path, &mut accounts, entry)
    }

    /// The account signed in on this computer with the plan's use allowed, if any.
    pub fn load() -> Option<Credentials> {
        load_any().filter(|c| c.signed_in() && c.plan_allowed())
    }

    /// The account signed in, as its verified id token named it.
    pub fn account() -> Option<Account> {
        load().map(|c| c.account())
    }

    /// Writes `text` to `path` atomically, readable by this user only.
    fn write_private(path: &Path, text: &str) -> Result<(), Error> {
        let dir = path.parent().ok_or("no settings folder")?;
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        // A unique, exclusively created file avoids races and pre-existing symlinks. Tempfiles
        // have owner-only Unix permissions (Windows keeps the settings folder's own access
        // list); persist replaces atomically, including on Windows.
        let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        tmp.write_all(text.as_bytes()).and_then(|_| tmp.as_file().sync_all()).map_err(|e| format!("{}: {e}", path.display()))?;
        tmp.persist(path).map_err(|e| format!("{}: {}", path.display(), e.error))?;
        Ok(())
    }

    /// This computer's host id, created the first time.
    pub fn host_id() -> Result<String, Error> {
        let path = config_dir().ok_or("no settings folder")?.join(HOST_FILE);
        let _guard = store(&path)?;
        match std::fs::read_to_string(&path) {
            Ok(id) if valid_host_id(id.trim()) => return Ok(id.trim().to_string()),
            // Missing or malformed: a new one. Any other failure must not replace this host's id.
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", path.display()).into()),
        }
        let mut b = [0u8; 16];
        getrandom::fill(&mut b).map_err(|e| format!("no random numbers: {e}"))?;
        let id = host_id_from(b);
        write_private(&path, &id)?;
        Ok(id)
    }

    /// Verifies an RS256 JWT's signature against a JWKS (`{"keys": […]}`) and gives its claims.
    pub fn verify_jwt(jwt: &str, jwks: &Value) -> Result<Value, Error> {
        let b64 = |s: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s).ok();
        let mut parts = jwt.splitn(3, '.');
        let (h, p, s) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        let header: Value = b64(h).and_then(|b| serde_json::from_slice(&b).ok()).ok_or("the id token is malformed")?;
        if header.get("alg").and_then(Value::as_str) != Some("RS256") {
            return Err("the id token isn't signed with RS256".into());
        }
        let kid = header.get("kid").and_then(Value::as_str);
        let key = jwks
            .get("keys")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|k| k.get("kty").and_then(Value::as_str) == Some("RSA"))
            .find(|k| kid.is_none_or(|kid| k.get("kid").and_then(Value::as_str) == Some(kid)))
            .ok_or("the id token's signing key isn't published")?;
        let n = key.get("n").and_then(Value::as_str).and_then(b64).ok_or("a signing key is malformed")?;
        let e = key.get("e").and_then(Value::as_str).and_then(b64).ok_or("a signing key is malformed")?;
        let sig = b64(s).ok_or("the id token is malformed")?;
        let signed = format!("{h}.{p}");
        ring::signature::RsaPublicKeyComponents { n: &n, e: &e }
            .verify(&ring::signature::RSA_PKCS1_2048_8192_SHA256, signed.as_bytes(), &sig)
            .map_err(|_| "the id token's signature is invalid")?;
        jwt_payload(jwt).ok_or_else(|| Error::auth("the id token is malformed".into()))
    }

    fn get_json(url: &str, send: Send) -> Result<Value, Error> {
        let r = send(&HttpRequest { method: "GET", url: url.to_string(), headers: vec![], body: None })?;
        if !(200..300).contains(&r.status) {
            return Err(Error {
                status: Some(r.status),
                request_id: r.request_id().map(str::to_string),
                ..Error::auth(rustgis_engine::data::diagnostics::message_with_address(url, &format!("HTTP {}", r.status)))
            });
        }
        serde_json::from_slice(&r.body).map_err(|_| Error::auth(rustgis_engine::data::diagnostics::message_with_address(url, "not JSON")))
    }

    /// Verifies a new id token with OpenAI's published keys.
    fn verify_id_token(id_token: &str, client_id: &str, nonce: &str, send: Send) -> Result<Account, Error> {
        let config = get_json(DISCOVERY_URL, send)?;
        let jwks_uri = discovered(&config, "jwks_uri").ok_or("OpenAI's sign-in configuration has no signing keys")?;
        let claims = verify_jwt(id_token, &get_json(&jwks_uri, send)?)?;
        check_id_claims(&claims, client_id, nonce, now()).map_err(Error::auth)
    }

    /// Renews the tokens with `send` and keeps the result; the caller holds the renewal lock, so
    /// no other process uses the refresh token until the rotated one is kept. An unusable or
    /// missing refresh token signs the account out (the registration stays for the next sign-in).
    fn refresh(path: &Path, v: &Vault, c: &Credentials, send: Send) -> Result<Credentials, Error> {
        let current = {
            let _guard = store(path)?;
            let current = selected_at(path, v)?
                .filter(|now| now.binding() == c.binding())
                .ok_or_else(|| Error::new(Recovery::AccountChanged, "the ChatGPT sign-in changed; try again"))?;
            // Another request of this process renewed it meanwhile.
            if current.access_token != c.access_token && !current.access_token.is_empty() && !current.expires_soon(now()) {
                return Ok(current);
            }
            let refresh_token = load_refresh(v, &current)?;
            if refresh_token.is_empty() {
                // A keychain sign-in whose token is gone or out of date can't be used again here.
                if current.kept == Kept::Keychain {
                    write_credentials(path, v, &current.registration())?;
                }
                return Err(Error::new(Recovery::SignIn, "the sign-in can't be renewed; sign in again"));
            }
            Credentials { refresh_token, ..current }
        };
        let resp = send(&refresh_request(&current))?;
        let grant = parse_grant(&resp);
        let _guard = store(path)?;
        // A user can sign out or sign into another account while the grant is in flight.
        if selected_at(path, v).ok().flatten().map(|now| (now.binding(), now.rev)) != Some((current.binding(), current.rev.clone())) {
            return Err(Error::new(Recovery::AccountChanged, "the ChatGPT sign-in changed; try again"));
        }
        match grant {
            Ok(g) => write_credentials(path, v, &renewed(&current, &g, now())),
            Err(GrantError::Unusable) => {
                write_credentials(path, v, &current.registration())?;
                Err(Error::token(&resp, GrantError::Unusable))
            }
            Err(e) => Err(Error::token(&resp, e)),
        }
    }

    fn renewal_lock(path: &Path) -> Result<std::fs::File, Error> {
        file_lock(path, "renewal-lock")
    }

    /// The signed-in account's credentials, renewed with `send` when about to expire.
    pub fn fresh(send: Send) -> Result<Credentials, Error> {
        let path = auth_path().ok_or("no settings folder")?;
        let _guard = renewal_lock(&path)?;
        fresh_at(&path, vault(), send)
    }

    fn fresh_at(path: &Path, v: &Vault, send: Send) -> Result<Credentials, Error> {
        let c = {
            let _guard = store(path)?;
            selected_at(path, v)?.filter(Credentials::signed_in).ok_or_else(|| Error::new(Recovery::SignIn, "not signed in to ChatGPT"))?
        };
        if !c.plan_allowed() {
            return Err(Error::new(Recovery::Consent, "RustGIS isn't allowed to use your ChatGPT plan; sign in again and allow it"));
        }
        if c.expires_soon(now()) { refresh(path, v, &c, send) } else { Ok(c) }
    }

    /// Sends a request as `account` (blocking): refused if another account is now selected or it
    /// signed out; the tokens are renewed first when about to expire, and once more if the API
    /// still refuses them.
    pub fn send(req: &HttpRequest, account: &Binding) -> Result<HttpResponse, Error> {
        send_inner(req, account, None).map_err(Error::auth_context)
    }

    /// [`send`], handing the API's response body to `on_chunk` as it streams in.
    pub fn send_streaming(req: &HttpRequest, account: &Binding, on_chunk: &mut dyn FnMut(&[u8])) -> Result<HttpResponse, Error> {
        send_inner(req, account, Some(on_chunk)).map_err(Error::auth_context)
    }

    fn send_inner(req: &HttpRequest, account: &Binding, mut on_chunk: Option<&mut dyn FnMut(&[u8])>) -> Result<HttpResponse, Error> {
        // Reject an edited address before loading credentials or rotating any tokens.
        if !super::api_url(&req.url) {
            return Err(Error::new(Recovery::Configuration, "a ChatGPT sign-in can only be used with the OpenAI API"));
        }
        send_at(req, account, &auth_path().ok_or("no settings folder")?, vault(), &mut |req| {
            if req.url == super::TOKEN_URL {
                http::send_timeout(req, Duration::from_secs(30)).map_err(Error::auth_context)
            } else if let Some(f) = on_chunk.as_mut() {
                http::send_streaming(req, &mut **f).map_err(Error::auth_context)
            } else {
                http::send(req).map_err(Error::auth_context)
            }
        })
    }

    fn send_at(req: &HttpRequest, account: &Binding, path: &Path, v: &Vault, transport: Send) -> Result<HttpResponse, Error> {
        let c = {
            let _guard = renewal_lock(path)?;
            {
                let _store = store(path)?;
                // No saved sign-in at all reads as signed out.
                check_account(account, &selected_at(path, v)?.unwrap_or_default())?;
            }
            fresh_at(path, v, transport)?
        };
        check_final(path, v, account, &c)?;
        let resp = transport(&c.authorize(req.clone()))?;
        if resp.status != 401 {
            return Ok(resp);
        }
        let c = {
            let _guard = renewal_lock(path)?;
            let current = {
                let _store = store(path)?;
                selected_at(path, v)?
                    .filter(Credentials::signed_in)
                    .ok_or_else(|| Error::new(Recovery::SignIn, "not signed in to ChatGPT"))?
            };
            // Another request may already have rotated this token while ours was in flight.
            // Signing out or into another account must not be undone by a late 401 either.
            if current.binding() != c.binding() {
                return Err(Error::new(Recovery::AccountChanged, "the ChatGPT account changed; try again"));
            }
            if current.access_token != c.access_token { current } else { refresh(path, v, &current, transport)? }
        };
        check_final(path, v, account, &c)?;
        transport(&c.authorize(req.clone()))
    }

    /// Refuses a request built for another account than the selected one, or after it signed out.
    fn check_account(account: &Binding, c: &Credentials) -> Result<(), Error> {
        if !c.signed_in() {
            return Err(Error::new(Recovery::SignIn, "not signed in to ChatGPT"));
        }
        if c.binding() != *account {
            return Err(Error::new(Recovery::AccountChanged, "the ChatGPT account changed; select the account again"));
        }
        Ok(())
    }

    fn check_final(path: &Path, v: &Vault, account: &Binding, c: &Credentials) -> Result<(), Error> {
        let _guard = store(path)?;
        let saved = selected_at(path, v)?.unwrap_or_default();
        check_account(account, &saved)?;
        if !c.plan_allowed() || !saved.plan_allowed() {
            return Err(Error::new(Recovery::Consent, "RustGIS isn't allowed to use your ChatGPT plan; sign in again and allow it"));
        }
        Ok(())
    }

    // -----------------------------------------------------------------------------------------
    // Signing out

    /// What revoking a sign-out's session needs.
    pub struct Revocation {
        client_id: String,
        refresh_token: String,
    }

    impl std::fmt::Debug for Revocation {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("Revocation").field("client_id", &self.client_id).finish_non_exhaustive()
        }
    }

    /// What a sign-out leaves to do.
    #[derive(Debug, Default)]
    pub struct SignedOut {
        /// The session to end at OpenAI ([`revoke`], off the UI thread).
        pub revocation: Option<Revocation>,
        /// Why it can't be ended from here: the keychain couldn't give its refresh token.
        pub unrevoked: Option<Error>,
    }

    /// Signs out on this computer at once: the tokens are deleted (the registration and host id
    /// stay for the next sign-in). Revoke the session with what it gives.
    pub fn sign_out() -> Result<SignedOut, Error> {
        match auth_path() {
            Some(path) => sign_out_at(&path, vault()),
            None => Ok(SignedOut::default()),
        }
    }

    /// Signs out only if the displayed registration is still selected.
    pub fn sign_out_selected(account: &Binding) -> Result<SignedOut, Error> {
        let path = auth_path().ok_or("no settings folder")?;
        sign_out_checked(&path, vault(), Some(account))
    }

    fn sign_out_at(path: &Path, v: &Vault) -> Result<SignedOut, Error> {
        sign_out_checked(path, v, None)
    }

    fn sign_out_checked(path: &Path, v: &Vault, expected: Option<&Binding>) -> Result<SignedOut, Error> {
        let _guard = store(path)?;
        let mut accounts = read_accounts(path)?;
        let Some(entry) = accounts.selected().cloned() else { return Ok(SignedOut::default()) };
        if expected.is_some_and(|e| *e != entry.binding()) {
            return Err(Error::new(Recovery::AccountChanged, "the ChatGPT sign-in changed; select the account again"));
        }
        let c = with_tokens(v, path, &mut accounts, entry)?;
        let (refresh_token, unrevoked) = match load_refresh(v, &c) {
            Ok(r) => (r, None),
            // Without a keychain here (the command line), the app can still revoke it.
            Err(e) if v.store().is_none() => return Err(e),
            // A keychain that can't give it doesn't keep anyone signed in: the session can't be
            // ended at OpenAI from here, which the caller says.
            Err(e) => (String::new(), Some(e)),
        };
        let mut registration = c.registration();
        registration.session = random_token()?;
        keep_tokens(v, path, &mut accounts, &registration, false)?;
        Ok(SignedOut { revocation: revocation(&c.client_id, refresh_token), unrevoked })
    }

    fn revocation(client_id: &str, refresh_token: String) -> Option<Revocation> {
        (!refresh_token.is_empty()).then(|| Revocation { client_id: client_id.to_string(), refresh_token })
    }

    /// Removes a saved registration, with its tokens (signing it out first when signed in).
    /// Revoke the session with what it gives ([`revoke`]), off the UI thread.
    pub fn remove(client_id: &str, subject: &str) -> Result<Option<Revocation>, Error> {
        remove_at(&auth_path().ok_or("no settings folder")?, vault(), client_id, subject)
    }

    fn remove_at(path: &Path, v: &Vault, client_id: &str, subject: &str) -> Result<Option<Revocation>, Error> {
        let _guard = store(path)?;
        let mut accounts = read_accounts(path)?;
        let who = (client_id.to_string(), subject.to_string());
        let Some(entry) = accounts.entries.iter().find(|e| id(e) == who).cloned() else { return Ok(None) };
        let c = with_tokens(v, path, &mut accounts, entry)?;
        // The refresh token to revoke, and the keychain item go first: a registration whose item
        // may still exist is never forgotten, or the item would be left behind for good.
        let refresh_token = load_refresh(v, &c)?;
        let item = c.in_keychain || !c.rev.is_empty();
        match v.store() {
            Some(store) => {
                if let Err(e) = store.delete(&secret_name(client_id, subject))
                    && item
                {
                    return Err(keychain_error("the ChatGPT account can't be removed", &e));
                }
            }
            None if item => return Err(Error::new(Recovery::Auth, KEYCHAIN_ELSEWHERE)),
            None => {}
        }
        v.held().remove(&who);
        accounts.entries.retain(|e| id(e) != who);
        if accounts.active.as_ref() == Some(&who) {
            accounts.active = None;
        }
        write_accounts(path, &accounts)?;
        Ok(revocation(client_id, refresh_token))
    }

    /// Ends a signed-out session at OpenAI (blocking; retried a few times on network and server
    /// errors). An error means OpenAI didn't confirm it: the user can disconnect RustGIS in their
    /// ChatGPT settings.
    pub fn revoke(r: &Revocation) -> Result<(), Error> {
        revoke_with(r, &mut |req| http::send_timeout(req, Duration::from_secs(30)).map_err(Error::auth_context), &mut |d| {
            std::thread::sleep(d)
        })
    }

    fn revoke_with(r: &Revocation, send: Send, wait: &mut dyn FnMut(Duration)) -> Result<(), Error> {
        let mut endpoint = None;
        let mut last = Error::auth("revocation not confirmed".into());
        for attempt in 0..3u32 {
            if attempt > 0 {
                wait(Duration::from_secs(1 << attempt));
            }
            let url = match &endpoint {
                Some(url) => url,
                None => match get_json(DISCOVERY_URL, send) {
                    Ok(config) => {
                        let url = discovered(&config, "revocation_endpoint")
                            .ok_or("OpenAI's sign-in configuration has no revocation endpoint")?;
                        endpoint.insert(url)
                    }
                    Err(e) => {
                        last = e;
                        continue;
                    }
                },
            };
            match send(&revoke_request(url, &r.client_id, &r.refresh_token)) {
                Ok(resp) if (200..300).contains(&resp.status) => return Ok(()),
                Ok(resp) if resp.status < 500 => return Err(Error::auth(format!("revoking the sign-in: HTTP {}", resp.status))),
                Ok(resp) => last = Error::auth(format!("revoking the sign-in: HTTP {}", resp.status)),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    // -----------------------------------------------------------------------------------------
    // Signing in: the browser flow

    type Outcome = Arc<Mutex<Option<Result<Credentials, Error>>>>;

    /// A sign-in under way: open [`Login::url`] in the browser, then [`Login::poll`] until the
    /// credentials arrive. Dropping it stops the callback server.
    pub struct Login {
        /// The authorization page, safe to show or copy (without the id token hint).
        pub url: String,
        /// The same page with the id token hint, for opening the browser only.
        pub browser_url: String,
        /// The loopback listener port.
        pub port: u16,
        outcome: Outcome,
        cancel: Arc<Mutex<bool>>,
    }

    impl Login {
        /// The outcome, once there is one (taken: later polls say nothing).
        pub fn poll(&self) -> Option<Result<Credentials, Error>> {
            self.outcome.lock().ok().and_then(|mut o| o.take()).map(|r| r.map_err(Error::auth_context))
        }

        /// Stops the listener and discards any outstanding grant.
        pub fn cancel(&self) {
            if let Ok(mut cancel) = self.cancel.lock() {
                *cancel = true;
            }
        }
    }

    impl Drop for Login {
        fn drop(&mut self) {
            self.cancel();
        }
    }

    fn random_token() -> Result<String, Error> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|e| format!("no random numbers: {e}"))?;
        Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
    }

    /// What the callback server needs to finish a sign-in.
    struct Pending {
        redirect_uri: String,
        verifier: String,
        state: String,
        nonce: String,
        /// [`REGISTER`] or the saved client id.
        client_id: String,
        /// The account a sign-in with a saved registration must come back as.
        subject: String,
        expected: Option<Credentials>,
    }

    /// Starts a sign-in: binds the callback server and makes the page to open. The credentials
    /// are saved when they arrive. A sign-in uses the account registered on this computer, if
    /// any; `another_account` adds a separate registration.
    pub fn login(another_account: bool) -> Result<Login, Error> {
        login_inner(another_account).map_err(Error::auth_context)
    }

    fn login_inner(another_account: bool) -> Result<Login, Error> {
        let host_id = host_id()?;
        let expected = load_any();
        let saved = if another_account { None } else { expected.clone() };
        let (listener, port) = TcpListener::bind(("127.0.0.1", PORT))
            .or_else(|_| TcpListener::bind(("127.0.0.1", 0)))
            .and_then(|l| l.local_addr().map(|a| (l, a.port())))
            .map_err(|e| format!("can't start the sign-in server: {e}"))?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let p = Pending {
            redirect_uri: format!("http://127.0.0.1:{port}/auth/callback"),
            verifier: random_token()?,
            state: random_token()?,
            nonce: random_token()?,
            client_id: saved.as_ref().map_or_else(|| REGISTER.to_string(), |c| c.client_id.clone()),
            subject: saved.as_ref().map(|c| c.subject.clone()).unwrap_or_default(),
            expected,
        };
        let challenge = super::pkce_challenge(&p.verifier);
        let page = |id_token_hint: &str| {
            authorize_url(&Authorization {
                client_id: &p.client_id,
                host_id: &host_id,
                redirect_uri: &p.redirect_uri,
                challenge: &challenge,
                state: &p.state,
                nonce: &p.nonce,
                login_hint: saved.as_ref().map_or("", |c| c.email.as_str()),
                id_token_hint,
                consent: saved.as_ref().is_some_and(|c| c.plan_declined),
            })
        };
        let (url, browser_url) = (page(""), page(saved.as_ref().map_or("", |c| c.id_token.as_str())));
        let outcome: Outcome = Arc::default();
        let cancel = Arc::new(Mutex::new(false));
        let (out, stop) = (outcome.clone(), cancel.clone());
        std::thread::spawn(move || {
            let result = serve(&listener, &stop, &p);
            if let Ok(mut o) = out.lock() {
                *o = Some(result);
            }
        });
        Ok(Login { url, browser_url, port, outcome, cancel })
    }

    /// Answers requests until the callback arrives (or the sign-in is cancelled).
    fn serve(listener: &TcpListener, cancel: &Arc<Mutex<bool>>, p: &Pending) -> Result<Credentials, Error> {
        let deadline = Instant::now() + LOGIN_TIMEOUT;
        loop {
            check_attempt(cancel, deadline)?;
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
                Err(e) => return Err(Error::auth(format!("the sign-in server failed: {e}"))),
            };
            let _ = stream.set_nonblocking(false);
            let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
            let _ = stream.set_write_timeout(Some(Duration::from_millis(100)));
            let Some(target) = request_target(&mut stream, cancel, deadline) else {
                respond(&mut stream, 400, "Bad request");
                continue;
            };
            let (path, query) = target.split_once('?').unwrap_or((target.as_str(), ""));
            if path != "/auth/callback" {
                respond(&mut stream, 404, "Not found");
                continue;
            }
            let params = query_params(query);
            let get = |k: &str| params.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str()).unwrap_or("");
            if !super::callback_state_matches(&params, &p.state) {
                respond(&mut stream, 400, "State mismatch");
                continue;
            }
            if !get("error").is_empty() {
                let why = if get("error") == "access_denied" { "access denied" } else { "authorization failed" };
                respond(&mut stream, 200, &page("Sign-in failed", why));
                return Err(Error::new(if get("error") == "access_denied" { Recovery::Consent } else { Recovery::Auth }, why));
            }
            let code = get("code");
            if code.is_empty() {
                respond(&mut stream, 400, "Missing authorization code");
                continue;
            }
            // The code exchange runs to completion (its timeout is short), so that tokens issued
            // for an attempt that is then cancelled, unverified or not kept can be revoked.
            let exchanged = callback_client_id(&params, &p.client_id).map_err(Error::auth).and_then(|client_id| {
                let resp = http::send_timeout(&exchange_request(&client_id, code, &p.redirect_uri, &p.verifier), Duration::from_secs(30))
                    .map_err(Error::auth_context)?;
                let grant = parse_grant(&resp).map_err(|e| Error::token(&resp, e))?;
                Ok((client_id, grant))
            });
            let result = exchanged.and_then(|(client_id, grant)| {
                let kept = finish(p, &client_id, &grant, &mut |req| auth_send(req, cancel, deadline))
                    .and_then(|c| check_attempt(cancel, deadline).map(|()| c))
                    .and_then(|c| save_if_active(cancel, &c, &mut |c| save(c, p.expected.as_ref())));
                if kept.is_err() && !grant.refresh_token.is_empty() {
                    let _ = revoke(&Revocation { client_id, refresh_token: grant.refresh_token.clone() });
                }
                kept
            });
            return match result {
                Ok(c) if !c.plan_allowed() => {
                    let why = "RustGIS wasn't allowed to use your ChatGPT plan. Sign in again and allow it to use the assistant.";
                    respond(&mut stream, 200, &page("Sign-in incomplete", why));
                    Ok(c)
                }
                Ok(c) => {
                    respond(&mut stream, 200, &page("Signed in", "RustGIS can now use your ChatGPT plan. You can close this tab."));
                    Ok(c)
                }
                Err(e) => {
                    respond(&mut stream, 200, &page("Sign-in failed", &e.to_string()));
                    Err(e)
                }
            };
        }
    }

    fn check_attempt(cancel: &Mutex<bool>, deadline: Instant) -> Result<(), Error> {
        if *cancel.lock().map_err(|_| "the sign-in was cancelled")? {
            return Err(Error::new(Recovery::Cancelled, "the sign-in was cancelled"));
        }
        if Instant::now() >= deadline {
            return Err(Error::new(
                Recovery::Timeout,
                "the sign-in timed out; restart sign-in and allow the browser to reach this computer's loopback address",
            ));
        }
        Ok(())
    }

    fn auth_send(req: &HttpRequest, cancel: &Mutex<bool>, deadline: Instant) -> Result<HttpResponse, Error> {
        let req = req.clone();
        let timeout = deadline.saturating_duration_since(Instant::now()).min(Duration::from_secs(30));
        cancellable(cancel, deadline, move || http::send_timeout(&req, timeout).map_err(Error::auth_context))
    }

    // The listener can exit immediately; a transport already in progress has a short timeout
    // and cannot persist anything after cancellation.
    fn cancellable<T: std::marker::Send + 'static>(
        cancel: &Mutex<bool>,
        deadline: Instant,
        work: impl FnOnce() -> Result<T, Error> + std::marker::Send + 'static,
    ) -> Result<T, Error> {
        check_attempt(cancel, deadline)?;
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let _ = tx.send(work());
        });
        loop {
            check_attempt(cancel, deadline)?;
            match rx.recv_timeout(Duration::from_millis(25)) {
                Ok(result) => {
                    check_attempt(cancel, deadline)?;
                    return result;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => return Err("the sign-in request stopped".into()),
            }
        }
    }

    /// Verifies who an exchanged grant signed in, as a new session.
    fn finish(p: &Pending, client_id: &str, grant: &super::Grant, send: Send) -> Result<Credentials, Error> {
        let account = verify_id_token(&grant.id_token, client_id, &p.nonce, send)?;
        if !p.subject.is_empty() && client_id == p.client_id && account.subject != p.subject {
            return Err(Error::new(Recovery::AccountChanged, "signing in: a different ChatGPT account came back than the one registered"));
        }
        let mut c = super::signed_in(client_id, &account, grant, now());
        c.session = random_token()?;
        Ok(c)
    }

    /// Keeps a verified sign-in, with plan permission recorded separately. Gives it as kept.
    fn save(c: &Credentials, expected: Option<&Credentials>) -> Result<Credentials, Error> {
        let path = auth_path().ok_or("no settings folder")?;
        save_at(&path, vault(), c, expected)
    }

    fn save_at(path: &Path, v: &Vault, c: &Credentials, expected: Option<&Credentials>) -> Result<Credentials, Error> {
        let _guard = store(path)?;
        let mut accounts = read_accounts(path)?;
        // Who, and whether it had tokens (from the file alone: a locked keychain still compares):
        // a sign-out or another sign-in meanwhile is not undone.
        let identity = |c: &Credentials| (c.binding(), !c.rev.is_empty() || c.kept == Kept::SettingsFile);
        if accounts.selected().map(identity) != expected.map(identity) {
            return Err(Error::new(Recovery::AccountChanged, "the ChatGPT sign-in changed; select the account again"));
        }
        // Keep valid identity even when plan permission was declined.
        keep_tokens(v, path, &mut accounts, c, true)
    }

    fn save_if_active(
        cancel: &Mutex<bool>,
        c: &Credentials,
        persist: &mut dyn FnMut(&Credentials) -> Result<Credentials, Error>,
    ) -> Result<Credentials, Error> {
        let cancelled = cancel.lock().map_err(|_| "the sign-in was cancelled")?;
        if *cancelled {
            return Err(Error::new(Recovery::Cancelled, "the sign-in was cancelled"));
        }
        persist(c)
    }

    /// The request target (`/auth/callback?code=…`) of an HTTP request, read up to its headers' end.
    fn request_target(stream: &mut impl Read, cancel: &Mutex<bool>, deadline: Instant) -> Option<String> {
        let deadline = deadline.min(Instant::now() + Duration::from_secs(2));
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 16 * 1024 {
            check_attempt(cancel, deadline).ok()?;
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => buf.extend_from_slice(chunk.get(..n)?),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    continue;
                }
                Err(_) => return None,
            }
        }
        if !buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 16 * 1024 {
            return None;
        }
        let line = buf.split(|b| *b == b'\n').next()?;
        let line = std::str::from_utf8(line).ok()?.trim();
        let mut parts = line.split(' ');
        let (method, target) = (parts.next()?, parts.next()?);
        let version = parts.next()?;
        (method == "GET" && version == "HTTP/1.1" && parts.next().is_none()).then(|| target.to_string())
    }

    fn respond(stream: &mut TcpStream, status: u16, body: &str) {
        let reason = match status {
            200 => "OK",
            400 => "Bad Request",
            404 => "Not Found",
            _ => "Error",
        };
        let kind = if body.starts_with("<!DOCTYPE") { "text/html; charset=utf-8" } else { "text/plain; charset=utf-8" };
        let head = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: {kind}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(head.as_bytes()).and_then(|_| stream.write_all(body.as_bytes())).and_then(|_| stream.flush());
    }

    fn escape(s: &str) -> String {
        s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
    }

    /// The page the browser shows at the end.
    fn page(title: &str, text: &str) -> String {
        format!(
            "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>RustGIS: {t}</title></head>\
<body style=\"font-family: system-ui, sans-serif; margin: 3em auto; max-width: 32em; text-align: center\">\
<h1>{t}</h1><p>{b}</p></body></html>",
            t = escape(title),
            b = escape(text)
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use rustgis_engine::data::secrets::{MemorySecrets, SecretError};

        fn access(exp: i64) -> String {
            let payload = serde_json::json!({"client_id": "oaiapp_synthetic", "sub": "user-1", "exp": exp});
            format!("e30.{}.sig", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.to_string()))
        }

        fn synthetic(expires_at: i64) -> Credentials {
            Credentials {
                client_id: "oaiapp_synthetic".into(),
                subject: "user-1".into(),
                email: "ann@example.com".into(),
                id_token: "synthetic-id".into(),
                access_token: access(expires_at),
                refresh_token: "synthetic-refresh".into(),
                expires_at,
                scopes: super::super::SCOPES.split(' ').map(str::to_string).collect(),
                plan_declined: false,
                saved_at: String::new(),
                session: "session-1".into(),
                rev: String::new(),
                in_keychain: false,
                kept: Kept::Nowhere,
            }
        }

        /// A process's vault, on a keychain of its own.
        fn keychain() -> Vault {
            Vault::new(Some(Arc::new(MemorySecrets::new())))
        }

        /// A process's vault, on a keychain other processes share.
        fn sharing(store: &Arc<MemorySecrets>) -> Vault {
            Vault::new(Some(store.clone() as Arc<dyn SecretStore>))
        }

        fn failing() -> Vault {
            Vault::new(Some(Arc::new(MemorySecrets::failing(SecretError::Denied))))
        }

        fn read_credentials(path: &Path, v: &Vault) -> Result<Credentials, Error> {
            selected_at(path, v)?.ok_or_else(|| "nothing selected".into())
        }

        /// The credentials without where and as which revision they are kept, nor the refresh
        /// token (a keychain sign-in's is read only when needed).
        fn plain(c: Credentials) -> Credentials {
            Credentials { rev: String::new(), in_keychain: false, kept: Kept::Nowhere, refresh_token: String::new(), ..c }
        }

        /// An entry as versions before the keychain saved it, tokens and all.
        fn legacy_json(c: &Credentials) -> Value {
            let mut v = c.to_json();
            v["id_token"] = c.id_token.clone().into();
            v["access_token"] = c.access_token.clone().into();
            v["refresh_token"] = c.refresh_token.clone().into();
            v
        }

        fn file_has_tokens(path: &Path) -> bool {
            let text = std::fs::read_to_string(path).unwrap();
            ["synthetic-id", "synthetic-refresh", "rotated", "e30."].iter().any(|t| text.contains(t))
        }

        fn entry_json(path: &Path) -> Value {
            let json: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            json["accounts"][0].clone()
        }

        const NAME: &str = "chatgpt/oaiapp_synthetic/user-1";

        fn grant(expires_at: i64, refresh: &str) -> HttpResponse {
            let body = serde_json::json!({"access_token": access(expires_at), "refresh_token": refresh, "expires_in": expires_at - now()});
            HttpResponse { status: 200, body: body.to_string().into_bytes(), headers: vec![] }
        }

        fn test_request() -> HttpRequest {
            HttpRequest {
                method: "POST",
                url: format!("{}/responses", super::super::API),
                headers: vec![("Authorization".into(), format!("Bearer {}", access(0)))],
                body: None,
            }
        }

        #[test]
        fn the_refresh_token_goes_to_the_keychain_and_no_token_to_the_file() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("auth.json");
            let store = Arc::new(MemorySecrets::new());
            let v = sharing(&store);
            let c = synthetic(now() + 100_000);
            assert_eq!(write_credentials(&path, &v, &c).unwrap().kept, Kept::Keychain);
            assert!(!file_has_tokens(&path));
            let entry = entry_json(&path);
            for k in ["client_id", "subject", "email", "scopes", "expires_at"] {
                assert!(!entry[k].is_null(), "{k} stays in the file");
            }
            assert_eq!(entry["keychain"], true);
            // The keychain keeps the refresh token alone, not the access and id tokens, to stay
            // within Windows' limit (a real refresh token's length is not verified by this test).
            let saved = store.load(NAME).unwrap().unwrap();
            assert!(
                saved.expose().contains("synthetic-refresh")
                    && !saved.expose().contains("e30.")
                    && !saved.expose().contains("synthetic-id")
            );
            // This process keeps using its access token.
            assert_eq!(plain(read_credentials(&path, &v).unwrap()), plain(c.clone()));
            // Another process, on the same keychain, is signed in and renews first.
            let (other, process) = (read_credentials(&path, &sharing(&store)).unwrap(), sharing(&store));
            assert!(other.signed_in() && other.kept == Kept::Keychain && other.access_token.is_empty() && other.expires_soon(now()));
            assert_eq!(load_refresh(&process, &other).unwrap(), "synthetic-refresh");
            // Without the keychain's refresh token, renewing signs out, without any request.
            store.delete(NAME).unwrap();
            let e = send_at(&test_request(), &c.binding(), &path, &process, &mut |_| panic!("nothing may be sent")).unwrap_err();
            assert_eq!(e.recovery, Recovery::SignIn);
            assert!(!read_credentials(&path, &process).unwrap().signed_in());
        }

        #[test]
        fn tokens_from_the_file_move_to_the_keychain_once_it_takes_them() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("auth.json");
            let old = synthetic(now() + 100_000);
            let text = serde_json::json!({"version": 2, "active": {"client_id": old.client_id, "subject": old.subject},
                "accounts": [legacy_json(&old)]})
            .to_string();
            std::fs::write(&path, &text).unwrap();
            // Without a keychain, or with one that refuses, the file stays as it is and works.
            for v in [Vault::new(None), failing()] {
                let c = read_credentials(&path, &v).unwrap();
                assert_eq!((c.refresh_token.as_str(), c.kept), ("synthetic-refresh", Kept::SettingsFile));
                assert_eq!(plain(c), plain(old.clone()));
                assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
            }
            // Renewing such a sign-in without the keychain keeps the rotated token in the file:
            // the old one is used up.
            std::fs::write(&path, legacy_json(&synthetic(0)).to_string()).unwrap();
            send_at(&test_request(), &old.binding(), &path, &failing(), &mut |req| {
                Ok(if req.url == super::super::TOKEN_URL {
                    grant(now() + 100_000, "rotated")
                } else {
                    HttpResponse { status: 200, body: vec![], headers: vec![] }
                })
            })
            .unwrap();
            let renewed = read_credentials(&path, &Vault::new(None)).unwrap();
            assert_eq!((renewed.refresh_token.as_str(), renewed.kept), ("rotated", Kept::SettingsFile));
            // The keychain takes them: out of the file.
            let store = Arc::new(MemorySecrets::new());
            let moved = read_credentials(&path, &sharing(&store)).unwrap();
            assert_eq!(moved.kept, Kept::Keychain);
            assert!(!file_has_tokens(&path));
            assert!(store.load(NAME).unwrap().unwrap().expose().contains("rotated"));
            let other = sharing(&store);
            assert_eq!(load_refresh(&other, &read_credentials(&path, &other).unwrap()).unwrap(), "rotated");
        }

        #[test]
        fn a_sign_in_the_keychain_refuses_lasts_for_the_session_only() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("auth.json");
            for (v, legacy) in [(Vault::new(None), false), (failing(), false), (Vault::new(None), true), (failing(), true)] {
                let _ = std::fs::remove_file(&path);
                if legacy {
                    // Signing in again to an account whose old tokens are still in the file.
                    std::fs::write(&path, legacy_json(&synthetic(now() + 100_000)).to_string()).unwrap();
                }
                let expected = load_any_at(&path, &v);
                let mut new = synthetic(now() + 100_000);
                new.session = "session-2".into();
                new.refresh_token = "rotated".into();
                assert_eq!(save_at(&path, &v, &new, expected.as_ref()).unwrap().kept, Kept::Session);
                assert!(!file_has_tokens(&path), "never written to the file instead");
                assert_eq!(entry_json(&path)["rev"], "", "no keychain token to look for");
                let c = read_credentials(&path, &v).unwrap();
                assert!(c.signed_in() && c.kept == Kept::Session && c.refresh_token == "rotated");
                // Another process (or the next run) isn't signed in, and can sign out and remove
                // the account without a keychain: there is nothing in it.
                let next = Vault::new(None);
                assert!(!read_credentials(&path, &next).unwrap().signed_in());
                assert!(sign_out_at(&path, &next).is_ok());
                if v.store().is_none() {
                    assert!(remove_at(&path, &next, "oaiapp_synthetic", "user-1").is_ok());
                }
            }
        }

        #[test]
        fn a_sign_in_without_the_keychain_never_replaces_one_it_holds() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("auth.json");
            let store = Arc::new(MemorySecrets::new());
            write_credentials(&path, &sharing(&store), &synthetic(now() + 100_000)).unwrap();
            let before = (entry_json(&path)["rev"].clone(), store.load(NAME).unwrap());
            for v in [Vault::new(None), failing()] {
                let expected = load_any_at(&path, &v);
                let mut new = synthetic(now() + 100_000);
                new.session = "session-2".into();
                assert!(save_at(&path, &v, &new, expected.as_ref()).is_err());
                assert_eq!((entry_json(&path)["rev"].clone(), store.load(NAME).unwrap()), before);
            }
        }

        #[test]
        fn sign_out_and_removal_delete_the_stored_tokens() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("auth.json");
            let store = Arc::new(MemorySecrets::new());
            let v = sharing(&store);
            write_credentials(&path, &v, &synthetic(now() + 100_000)).unwrap();
            // Without the keychain (the command line), its token is out of reach: neither signing
            // out nor removing forgets it unrevoked.
            let none = Vault::new(None);
            assert!(sign_out_at(&path, &none).is_err());
            assert!(remove_at(&path, &none, "oaiapp_synthetic", "user-1").is_err());
            assert!(read_credentials(&path, &v).unwrap().signed_in());
            let out = sign_out_at(&path, &v).unwrap();
            assert_eq!(out.revocation.unwrap().refresh_token, "synthetic-refresh");
            assert!(out.unrevoked.is_none());
            assert_eq!(store.load(NAME).unwrap(), None);
            assert_eq!(entry_json(&path)["keychain"], false);
            // Removing: the tokens and the registration.
            let mut other = synthetic(now() + 100_000);
            other.client_id = "oaiapp_other".into();
            write_credentials(&path, &v, &synthetic(now() + 100_000)).unwrap();
            write_credentials(&path, &v, &other).unwrap();
            // A keychain that refuses keeps the account: its item would be left behind.
            assert!(remove_at(&path, &failing(), "oaiapp_synthetic", "user-1").is_err());
            assert_eq!(read_accounts(&path).unwrap().entries.len(), 2);
            let r = remove_at(&path, &v, "oaiapp_synthetic", "user-1").unwrap().unwrap();
            assert_eq!((r.client_id.as_str(), r.refresh_token.as_str()), ("oaiapp_synthetic", "synthetic-refresh"));
            assert_eq!(store.load(NAME).unwrap(), None);
            let accounts = read_accounts(&path).unwrap();
            assert_eq!(accounts.entries.iter().map(|e| e.client_id.as_str()).collect::<Vec<_>>(), ["oaiapp_other"]);
            assert_eq!(read_credentials(&path, &v).unwrap().client_id, "oaiapp_other", "the other account stays selected");
            remove_at(&path, &v, "oaiapp_other", "user-1").unwrap();
            assert!(read_accounts(&path).unwrap().selected().is_none());
            assert!(remove_at(&path, &v, "oaiapp_other", "user-1").unwrap().is_none());
        }

        #[test]
        fn an_unreadable_keychain_signs_out_here_and_keeps_its_item_noted() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("auth.json");
            let store = Arc::new(MemorySecrets::new());
            write_credentials(&path, &sharing(&store), &synthetic(now() + 100_000)).unwrap();
            let out = sign_out_at(&path, &failing()).unwrap();
            assert!(out.revocation.is_none() && out.unrevoked.is_some(), "the caller says it wasn't revoked");
            assert!(!read_credentials(&path, &sharing(&store)).unwrap().signed_in());
            // The item couldn't be deleted: it stays noted, so removal still deletes it first.
            assert_eq!(entry_json(&path)["keychain"], true);
            assert!(remove_at(&path, &failing(), "oaiapp_synthetic", "user-1").is_err());
            assert!(remove_at(&path, &Vault::new(None), "oaiapp_synthetic", "user-1").is_err());
            remove_at(&path, &sharing(&store), "oaiapp_synthetic", "user-1").unwrap();
            assert_eq!(store.load(NAME).unwrap(), None);
            assert!(read_accounts(&path).unwrap().entries.is_empty());
        }

        #[test]
        fn an_account_without_a_keychain_item_is_removed_whatever_the_keychain() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("auth.json");
            std::fs::write(&path, synthetic(0).registration().to_json().to_string()).unwrap();
            remove_at(&path, &failing(), "oaiapp_synthetic", "user-1").unwrap();
            assert!(read_accounts(&path).unwrap().entries.is_empty());
        }

        /// A keychain that checks the file already names each item, and its revision, before the
        /// item is written: a failed file write after it can't leave a token the file can't name.
        struct Noting {
            inner: MemorySecrets,
            path: std::path::PathBuf,
        }

        impl SecretStore for Noting {
            fn load(&self, account: &str) -> Result<Option<Secret>, SecretError> {
                self.inner.load(account)
            }
            fn save(&self, account: &str, value: &Secret) -> Result<(), SecretError> {
                let saved: Value = serde_json::from_str(value.expose()).unwrap();
                let entry = entry_json(&self.path);
                assert_eq!((entry["keychain"].clone(), entry["rev"].clone()), (Value::Bool(true), saved["rev"].clone()));
                self.inner.save(account, value)
            }
            fn delete(&self, account: &str) -> Result<(), SecretError> {
                self.inner.delete(account)
            }
        }

        #[test]
        fn the_file_names_a_keychain_item_before_it_is_written() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("auth.json");
            let v = Vault::new(Some(Arc::new(Noting { inner: MemorySecrets::new(), path: path.clone() })));
            // A sign-in, then a renewal of it.
            save_at(&path, &v, &synthetic(0), None).unwrap();
            send_at(&test_request(), &synthetic(0).binding(), &path, &v, &mut |req| {
                Ok(if req.url == super::super::TOKEN_URL {
                    grant(now() + 100_000, "rotated")
                } else {
                    HttpResponse { status: 200, body: vec![], headers: vec![] }
                })
            })
            .unwrap();
            let c = read_credentials(&path, &v).unwrap();
            assert_eq!((c.kept, load_refresh(&v, &c).unwrap().as_str()), (Kept::Keychain, "rotated"));
        }

        #[test]
        fn a_file_sign_in_without_a_refresh_token_stays_in_the_file() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("auth.json");
            let mut old = synthetic(now() + 100_000);
            old.refresh_token.clear();
            let text = legacy_json(&old).to_string();
            std::fs::write(&path, &text).unwrap();
            let store = Arc::new(MemorySecrets::new());
            let c = read_credentials(&path, &sharing(&store)).unwrap();
            assert!(c.signed_in() && c.kept == Kept::SettingsFile);
            assert_eq!(std::fs::read_to_string(&path).unwrap(), text, "its access token keeps working");
            assert_eq!(store.load(NAME).unwrap(), None);
        }

        #[test]
        fn keychain_tokens_of_another_revision_are_never_used() {
            // A renewal whose token the keychain took but whose file write failed: the rotated
            // refresh token is the keychain's, the file still names the previous revision.
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("auth.json");
            let store = Arc::new(MemorySecrets::new());
            write_credentials(&path, &sharing(&store), &synthetic(0)).unwrap();
            store.save(NAME, &Secret::new(serde_json::json!({"rev": "another", "refresh_token": "rotated"}).to_string())).unwrap();
            let v = sharing(&store);
            let e = send_at(&test_request(), &synthetic(0).binding(), &path, &v, &mut |_| panic!("no token may be used")).unwrap_err();
            assert_eq!(e.recovery, Recovery::SignIn);
            assert!(!read_credentials(&path, &v).unwrap().signed_in());
        }

        #[test]
        fn processes_renew_in_turn_with_the_latest_refresh_token() {
            // Each process renews once for its own access token; the renewal lock and the
            // keychain read under it mean each uses the refresh token the last one kept.
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("auth.json");
            let store = Arc::new(MemorySecrets::new());
            write_credentials(&path, &sharing(&store), &synthetic(now() + 100_000)).unwrap();
            let mut used = Vec::new();
            for (i, process) in [sharing(&store), sharing(&store)].iter().enumerate() {
                send_at(&test_request(), &synthetic(0).binding(), &path, process, &mut |req| {
                    if req.url == super::super::TOKEN_URL {
                        let body = req.body.clone().unwrap_or_default();
                        used.push(body.split('&').find_map(|p| p.strip_prefix("refresh_token=")).unwrap_or("").to_string());
                        return Ok(grant(now() + 100_000, &format!("rotated-{i}")));
                    }
                    Ok(HttpResponse { status: 200, body: vec![], headers: vec![] })
                })
                .unwrap();
            }
            assert_eq!(used, ["synthetic-refresh", "rotated-0"]);
        }

        #[test]
        fn requests_for_another_registration_or_after_sign_out_are_refused() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("auth.json");
            let v = keychain();
            for change in ["client", "subject", "signed-out"] {
                let mut c = synthetic(now() + 100_000);
                match change {
                    "client" => c.client_id = "other-registration".into(),
                    "subject" => c.subject = "other-user".into(),
                    _ => c = c.registration(),
                }
                write_credentials(&path, &v, &c).unwrap();
                let e = send_at(&test_request(), &synthetic(0).binding(), &path, &v, &mut |_| panic!("must refuse before any network I/O"))
                    .unwrap_err();
                assert!(e.to_string().contains(if change == "signed-out" { "not signed in" } else { "changed" }), "{change}: {e}");
            }
        }

        #[test]
        fn work_from_before_a_sign_out_is_refused_after_signing_in_again() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("auth.json");
            let v = keychain();
            let old = synthetic(now() + 100_000);
            write_credentials(&path, &v, &old).unwrap();
            // Another window signs out and back into the same registration.
            sign_out_at(&path, &v).unwrap();
            let mut again = old.clone();
            again.session = "session-2".into();
            write_credentials(&path, &v, &again).unwrap();
            let e = send_at(&test_request(), &old.binding(), &path, &v, &mut |_| panic!("must refuse before any network I/O")).unwrap_err();
            assert!(e.to_string().contains("changed"), "{e}");
            // A sign-in begun before another process signs out is not kept, even when the account
            // was already signed out.
            sign_out_at(&path, &v).unwrap();
            let started = read_credentials(&path, &v).unwrap();
            sign_out_at(&path, &v).unwrap();
            let mut late = old.clone();
            late.session = "session-3".into();
            assert!(save_at(&path, &v, &late, Some(&started)).is_err());
            assert!(!read_credentials(&path, &v).unwrap().signed_in());
            // Renewal and selection keep the session.
            write_credentials(&path, &v, &again).unwrap();
            assert_eq!(select_at(&path, &v, &again.client_id, &again.subject).unwrap().binding(), again.binding());
        }

        #[test]
        fn renewed_scope_is_checked_before_initial_send_and_retry() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("auth.json");
            let v = keychain();
            for retry in [false, true] {
                write_credentials(&path, &v, &synthetic(if retry { now() + 100_000 } else { 0 })).unwrap();
                let mut inference = 0;
                let e = send_at(&test_request(), &synthetic(0).binding(), &path, &v, &mut |req| {
                    if req.url == super::super::TOKEN_URL {
                        let mut r = grant(now() + 100_000, "rotated");
                        let mut body: Value = serde_json::from_slice(&r.body).unwrap();
                        body["scope"] = serde_json::json!("openid email");
                        r.body = body.to_string().into_bytes();
                        return Ok(r);
                    }
                    inference += 1;
                    Ok(HttpResponse { status: 401, body: vec![], headers: vec![] })
                })
                .unwrap_err();
                assert!(e.to_string().contains("plan"));
                assert_eq!(inference, usize::from(retry));
                assert!(!read_credentials(&path, &v).unwrap().plan_allowed());
            }
        }

        #[test]
        fn separate_store_handles_serialize_compare_and_write() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("auth.json");
            let v = keychain();
            write_credentials(&path, &v, &synthetic(0)).unwrap();
            let held = store(&path).unwrap();
            let other = std::fs::OpenOptions::new().read(true).write(true).open(path.with_extension("store-lock")).unwrap();
            assert!(other.try_lock().is_err());
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    let r = sign_out_at(&path, &v);
                    tx.send(r).unwrap();
                });
                assert!(rx.recv_timeout(Duration::from_millis(50)).is_err());
                drop(held);
                assert!(rx.recv_timeout(Duration::from_secs(2)).unwrap().is_ok());
            });
            assert!(!read_credentials(&path, &v).unwrap().signed_in());
            assert!(other.try_lock().is_ok());
        }

        #[test]
        fn independent_process_changes_survive_inflight_renewal_and_login() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("auth.json");
            let store = Arc::new(MemorySecrets::new());
            let (v, other) = (sharing(&store), sharing(&store));
            for signout in [false, true] {
                let old = synthetic(0);
                write_credentials(&path, &v, &old).unwrap();
                let result = send_at(&test_request(), &synthetic(0).binding(), &path, &v, &mut |_| {
                    std::thread::scope(|scope| {
                        scope
                            .spawn(|| {
                                if signout {
                                    sign_out_at(&path, &other).unwrap();
                                } else {
                                    let current = read_credentials(&path, &other).unwrap();
                                    let mut new = old.clone();
                                    new.subject = "new-account".into();
                                    save_at(&path, &other, &new, Some(&current)).unwrap();
                                }
                            })
                            .join()
                            .unwrap();
                    });
                    Ok(grant(now() + 100_000, "late-refresh"))
                });
                assert!(result.is_err());
                assert!(save_at(&path, &v, &old, Some(&old)).is_err(), "late login must not undo the change");
                let current = read_credentials(&path, &v).unwrap();
                assert!(!store.load(NAME).unwrap().is_some_and(|s| s.expose().contains("late-refresh")));
                assert_eq!(current.signed_in(), !signout);
            }
        }

        #[test]
        fn accounts_migrate_select_and_keep_distinct_registrations() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("auth.json");
            let v = keychain();
            let a = synthetic(now() + 100_000);
            std::fs::write(&path, legacy_json(&a).to_string()).unwrap();
            let read = read_credentials(&path, &v).unwrap();
            assert_eq!(plain(read.clone()), plain(a.clone()));
            let mut b = a.clone();
            b.client_id = "oaiapp_workspace".into();
            assert_ne!(a.label(), b.label());
            save_at(&path, &v, &b, Some(&read)).unwrap();
            let selected = select_at(&path, &v, &a.client_id, &a.subject).unwrap();
            assert_eq!(plain(selected), plain(a.clone()), "selecting changes nothing but the selection");
            sign_out_at(&path, &v).unwrap();
            let accounts = read_accounts(&path).unwrap();
            assert_eq!(accounts.entries.len(), 2);
            assert!(!read_credentials(&path, &v).unwrap().signed_in());
            let selected = select_at(&path, &v, &b.client_id, &b.subject).unwrap();
            assert!(selected.signed_in());
            assert_eq!(load_refresh(&v, &selected).unwrap(), b.refresh_token);
            let json: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(json["version"], 2);
        }

        #[test]
        fn callback_reads_and_auth_requests_obey_cancel_and_deadline() {
            let cancel = Arc::new(Mutex::new(false));
            struct Trickle;
            impl Read for Trickle {
                fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                    std::thread::sleep(Duration::from_millis(10));
                    if let Some(b) = buf.first_mut() {
                        *b = b'x';
                    }
                    Ok(1)
                }
            }
            let mut socket = Trickle;
            let start = Instant::now();
            assert!(request_target(&mut socket, &cancel, start + Duration::from_millis(60)).is_none());
            assert!(start.elapsed() < Duration::from_secs(1));
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    std::thread::sleep(Duration::from_millis(30));
                    *cancel.lock().unwrap() = true;
                });
                // The work outlasts the cancel however late the cancelling thread runs.
                let flag = cancel.clone();
                let e = cancellable(&cancel, Instant::now() + Duration::from_secs(10), move || {
                    let end = Instant::now() + Duration::from_secs(5);
                    while !*flag.lock().unwrap() && Instant::now() < end {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Ok(())
                })
                .unwrap_err();
                assert!(e.to_string().contains("cancelled"));
            });
            assert!(request_target(&mut socket, &cancel, Instant::now() + Duration::from_secs(10)).is_none());
            let e = cancellable(&Mutex::new(false), Instant::now() + Duration::from_millis(20), || {
                std::thread::sleep(Duration::from_millis(100));
                Ok(())
            })
            .unwrap_err();
            assert!(e.to_string().contains("timed out"));
        }

        #[test]
        fn simultaneous_401_responses_rotate_only_once() {
            // Two requests of one process: the second uses the first's renewal.
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("test-auth.json");
            let far = now() + 100_000;
            let v = keychain();
            write_credentials(&path, &v, &synthetic(far)).unwrap();
            let barrier = std::sync::Barrier::new(2);
            let rotations = std::sync::atomic::AtomicUsize::new(0);
            std::thread::scope(|scope| {
                let workers: Vec<_> = (0..2)
                    .map(|_| {
                        scope.spawn(|| {
                            let mut inference = 0;
                            send_at(&test_request(), &synthetic(0).binding(), &path, &v, &mut |req| {
                                if req.url == super::super::TOKEN_URL {
                                    rotations.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                                    return Ok(grant(far + 1, "synthetic-rotated"));
                                }
                                inference += 1;
                                if inference == 1 {
                                    barrier.wait();
                                    Ok(HttpResponse { status: 401, body: vec![], headers: vec![] })
                                } else {
                                    let bearer = format!("Bearer {}", access(far + 1));
                                    assert!(req.headers.iter().any(|(k, v)| k == "Authorization" && *v == bearer));
                                    Ok(HttpResponse { status: 200, body: vec![], headers: vec![] })
                                }
                            })
                            .unwrap()
                        })
                    })
                    .collect();
                for worker in workers {
                    assert_eq!(worker.join().unwrap().status, 200);
                }
            });
            assert_eq!(rotations.load(std::sync::atomic::Ordering::SeqCst), 1);
            assert_eq!(load_refresh(&v, &read_credentials(&path, &v).unwrap()).unwrap(), "synthetic-rotated");
        }

        #[test]
        fn late_401_does_not_restore_a_signed_out_or_different_account() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("test-auth.json");
            let v = keychain();
            for switch_account in [false, true] {
                write_credentials(&path, &v, &synthetic(now() + 100_000)).unwrap();
                let mut requests = 0;
                let result = send_at(&test_request(), &synthetic(0).binding(), &path, &v, &mut |_| {
                    requests += 1;
                    let _guard = store(&path).unwrap();
                    if switch_account {
                        let mut other = synthetic(now() + 100_001);
                        other.subject = "user-2".into();
                        write_credentials(&path, &v, &other).unwrap();
                    } else {
                        write_credentials(&path, &v, &synthetic(0).registration()).unwrap();
                    }
                    Ok(HttpResponse { status: 401, body: vec![], headers: vec![] })
                });
                assert!(result.is_err());
                assert_eq!(requests, 1, "a late response must not renew or resend");
                assert_eq!(read_credentials(&path, &v).unwrap().signed_in(), switch_account);
            }
        }

        #[test]
        fn sign_out_during_refresh_does_not_wait_for_network_or_restore_tokens() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("test-auth.json");
            let v = keychain();
            write_credentials(&path, &v, &synthetic(0)).unwrap();
            let result = send_at(&test_request(), &synthetic(0).binding(), &path, &v, &mut |req| {
                assert_eq!(req.url, super::super::TOKEN_URL);
                // An independent handle can sign out while renewal waits for the network.
                sign_out_at(&path, &v).unwrap();
                Ok(grant(now() + 100_000, "synthetic-rotated"))
            });
            assert!(result.is_err());
            assert!(!read_credentials(&path, &v).unwrap().signed_in());
        }

        #[test]
        fn an_unusable_refresh_token_is_cleared_and_the_registration_kept() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("test-auth.json");
            let v = keychain();
            write_credentials(&path, &v, &synthetic(0)).unwrap();
            let e = send_at(&test_request(), &synthetic(0).binding(), &path, &v, &mut |_| {
                Ok(HttpResponse { status: 400, body: br#"{"error":"refresh_token_reused"}"#.to_vec(), headers: vec![] })
            })
            .unwrap_err();
            assert!(e.to_string().contains("sign in again"), "{e}");
            let left = read_credentials(&path, &v).unwrap();
            assert!(!left.signed_in() && left.refresh_token.is_empty());
            assert_eq!(left.client_id, "oaiapp_synthetic");
            // A passing failure keeps the tokens.
            write_credentials(&path, &v, &synthetic(0)).unwrap();
            assert!(
                send_at(&test_request(), &synthetic(0).binding(), &path, &v, &mut |_| Ok(HttpResponse {
                    status: 503,
                    body: vec![],
                    headers: vec![]
                }))
                .is_err()
            );
            assert!(read_credentials(&path, &v).unwrap().signed_in());
        }

        #[test]
        fn without_the_plan_nothing_is_sent() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("test-auth.json");
            let v = keychain();
            let mut c = synthetic(now() + 100_000);
            c.scopes.retain(|s| s != super::super::PLAN_SCOPE);
            write_credentials(&path, &v, &c).unwrap();
            let e = send_at(&test_request(), &synthetic(0).binding(), &path, &v, &mut |_| panic!("nothing may be sent")).unwrap_err();
            assert!(e.to_string().contains("plan"), "{e}");
        }

        #[test]
        fn sign_out_keeps_the_registration_and_gives_the_session_to_revoke() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("test-auth.json");
            let v = keychain();
            write_credentials(&path, &v, &synthetic(now() + 100_000)).unwrap();
            let out = sign_out_at(&path, &v).unwrap();
            let r = out.revocation.unwrap();
            assert_eq!((r.client_id.as_str(), r.refresh_token.as_str()), ("oaiapp_synthetic", "synthetic-refresh"));
            assert!(!format!("{r:?}").contains("synthetic-refresh"));
            let left = read_credentials(&path, &v).unwrap();
            assert!(!left.signed_in() && left.id_token.is_empty() && left.refresh_token.is_empty());
            assert_eq!((left.client_id.as_str(), left.subject.as_str()), ("oaiapp_synthetic", "user-1"));
            // Again: nothing more to revoke. A missing file is no error.
            assert!(sign_out_at(&path, &v).unwrap().revocation.is_none());
            assert!(sign_out_at(&dir.path().join("missing.json"), &v).unwrap().revocation.is_none());
        }

        #[test]
        fn revocation_retries_server_errors_only() {
            let r = Revocation { client_id: "oaiapp_synthetic".into(), refresh_token: "synthetic-refresh".into() };
            let config = serde_json::json!({"revocation_endpoint": "https://auth.openai.com/api/accounts/oauth/revoke"}).to_string();
            for (statuses, ok, attempts) in
                [(vec![200], true, 1), (vec![503, 502, 200], true, 3), (vec![503, 503, 503], false, 3), (vec![400], false, 1)]
            {
                let mut replies = statuses.into_iter();
                let (mut sent, mut waits) = (0, 0);
                let result = revoke_with(
                    &r,
                    &mut |req| {
                        if req.url == super::super::DISCOVERY_URL {
                            return Ok(HttpResponse { status: 200, body: config.clone().into_bytes(), headers: vec![] });
                        }
                        assert_eq!(req.url, "https://auth.openai.com/api/accounts/oauth/revoke");
                        assert!(
                            req.body
                                .as_deref()
                                .is_some_and(|b| b.contains("token=synthetic-refresh") && b.contains("client_id=oaiapp_synthetic"))
                        );
                        sent += 1;
                        Ok(HttpResponse { status: replies.next().unwrap_or(500), body: vec![], headers: vec![] })
                    },
                    &mut |_| waits += 1,
                );
                assert_eq!((result.is_ok(), sent, waits), (ok, attempts, attempts - 1));
            }
            // A failed discovery is retried too.
            let mut discoveries = 0;
            let result = revoke_with(
                &r,
                &mut |req| {
                    if req.url == super::super::DISCOVERY_URL {
                        discoveries += 1;
                        return if discoveries == 1 {
                            Err("offline".into())
                        } else {
                            Ok(HttpResponse { status: 200, body: config.clone().into_bytes(), headers: vec![] })
                        };
                    }
                    Ok(HttpResponse { status: 200, body: vec![], headers: vec![] })
                },
                &mut |_| {},
            );
            assert!(result.is_ok());
            assert_eq!(discoveries, 2);
        }

        #[test]
        fn cancellation_prevents_persistence() {
            let cancelled = Mutex::new(true);
            let mut writes = 0;
            let result = save_if_active(&cancelled, &Credentials::default(), &mut |c| {
                writes += 1;
                Ok(c.clone())
            });
            assert!(result.is_err());
            assert_eq!(writes, 0);
            let result = save_if_active(&Mutex::new(false), &Credentials::default(), &mut |_| Err("disk full".into()));
            assert_eq!(result, Err("disk full".into()));
        }

        #[test]
        fn credentials_file_replaces_atomically_and_cleans_failed_writes() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("test-auth.json");
            let v = keychain();
            let first = synthetic(1);
            let next = plain(synthetic(2));
            write_credentials(&path, &v, &first).unwrap();
            write_credentials(&path, &v, &synthetic(2)).unwrap();
            assert_eq!(plain(read_credentials(&path, &v).unwrap()), next);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
            }
            let destination_dir = dir.path().join("directory");
            std::fs::create_dir(&destination_dir).unwrap();
            assert!(write_credentials(&destination_dir, &v, &next).is_err());
            assert_eq!(plain(read_credentials(&path, &v).unwrap()), next);
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2, "failed writes leave no tempfiles");
            // A byte-order mark (Windows editors) is no reason to lose the sign-in.
            std::fs::write(&path, format!("\u{feff}{}", legacy_json(&next))).unwrap();
            assert_eq!(plain(read_credentials(&path, &v).unwrap()), next);
        }
    }
}
