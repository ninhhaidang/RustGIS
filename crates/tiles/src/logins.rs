//! Session-only web-service logins carried by explicit, secret-free request bindings.
//! Bare addresses never acquire a service login. Each activation creates a fresh generation;
//! the owning session revokes its token on replacement, logout and drop.

use std::{
    collections::BTreeMap,
    fmt,
    sync::{Mutex, MutexGuard, OnceLock},
};

use base64::Engine;
use rustgis_data::address::query_name_matches;
use rustgis_data::secrets::Secret;

/// Maximum header or query parameter name length, in bytes.
pub const MAX_NAME: usize = 64;
/// Maximum encoded credential header length, in bytes (including `Basic `).
pub const MAX_VALUE: usize = 4096;
/// A query login whose parameter name can't be held for redaction this run.
pub const TOO_MANY_QUERY_NAMES: &str =
    "Too many different API-key query parameter names are in use this run: reuse one, or restart RustGIS.";

pub use rustgis_data::login_address::origin;

/// An RFC 7230 token, excluding transport, browser-managed and proxy/security headers.
pub fn valid_header_name(name: &str) -> bool {
    if name.is_empty() || name.len() > MAX_NAME || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)) {
        return false;
    }
    let blocked = [
        "host",
        "connection",
        "content-length",
        "transfer-encoding",
        "te",
        "upgrade",
        "keep-alive",
        "range",
        "accept-encoding",
        "cookie",
        "origin",
        "referer",
        "user-agent",
        "content-type",
        "accept",
    ];
    !blocked.iter().any(|s| name.eq_ignore_ascii_case(s))
        && !name.get(..6).is_some_and(|s| s.eq_ignore_ascii_case("proxy-"))
        && !name.get(..4).is_some_and(|s| s.eq_ignore_ascii_case("sec-"))
}

/// A query parameter name of at most [`MAX_NAME`] unreserved ASCII characters (letters, digits
/// and `-_.~`), at least one a letter or digit, so it is the same text raw, percent-encoded and
/// in a form, and redaction can recognize it.
pub fn valid_query_name(name: &str) -> bool {
    name.len() <= MAX_NAME && name.bytes().all(unreserved) && name.bytes().any(|b| b.is_ascii_alphanumeric())
}

/// A nonempty credential of at most [`MAX_VALUE`] bytes, without control characters.
pub fn valid_value(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_VALUE && !value.chars().any(char::is_control)
}

/// An HTTP Basic authorization value, using standard padded Base64.
pub fn basic(user: &str, password: &str) -> String {
    let input = format!("{user}:{password}");
    let mut result = String::with_capacity(6 + input.len().div_ceil(3) * 4);
    result.push_str("Basic ");
    base64::engine::general_purpose::STANDARD.encode_string(input, &mut result);
    result
}

/// Whether raw Basic inputs produce an allowed encoded header, without allocating it.
pub fn valid_basic(user: &str, password: &str) -> bool {
    !user.contains(':')
        && !user.chars().chain(password.chars()).any(char::is_control)
        && user
            .len()
            .checked_add(password.len())
            .and_then(|n| n.checked_add(1))
            .is_some_and(|n| n.div_ceil(3).saturating_mul(4).saturating_add(6) <= MAX_VALUE)
}

/// How a connection credential is sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Method {
    Header(String),
    /// The credential is the value of this query parameter, added to each request to the origin.
    Query(String),
}

// Deliberately no Debug: neither credentials nor their derived redaction patterns are printable.
struct Entry {
    owner: String,
    origin: String,
    method: Method,
    value: Secret,
    redactions: Vec<Redaction>,
}

struct Redaction {
    value: Secret,
    encoded: bool,
}

fn registry() -> &'static Mutex<BTreeMap<String, Entry>> {
    static ENTRIES: OnceLock<Mutex<BTreeMap<String, Entry>>> = OnceLock::new();
    ENTRIES.get_or_init(Mutex::default)
}

fn entries() -> MutexGuard<'static, BTreeMap<String, Entry>> {
    registry().lock().unwrap_or_else(|e| e.into_inner())
}

/// An exact runtime snapshot of one session's login generations, including their absence.
/// It contains no credential and is never a display or persistence representation.
#[derive(Clone, PartialEq, Eq)]
pub struct AccountIdentity {
    session: u64,
    entries: Vec<(String, String, Method)>,
}

/// Captures only the initiating session's runtime bindings. Other sessions cannot invalidate it.
/// Approval capture must fail closed when the runtime registry cannot be read consistently.
pub fn account_identity(session: u64) -> Result<AccountIdentity, &'static str> {
    let entries = registry().lock().map_err(|_| "The service login identity is unavailable.")?;
    let entries = entries
        .iter()
        .filter(|(_, entry)| entry.owner.split_once('/').is_some_and(|(s, _)| s.parse::<u64>() == Ok(session)))
        .map(|(binding, entry)| (binding.clone(), entry.origin.clone(), entry.method.clone()))
        .collect();
    Ok(AccountIdentity { session, entries })
}

impl AccountIdentity {
    /// Poison or any change to this session's active binding generations refuses later work.
    pub fn is_current(&self) -> bool {
        account_identity(self.session).is_ok_and(|current| current == *self)
    }
}

/// Installs a fresh session/connection login generation. Only addresses bound to the returned
/// token can use it. Callers revoke their previous generation explicitly.
pub fn activate(origin: &str, owner: &str, method: Method, value: &str, insecure: bool) -> Result<String, &'static str> {
    activate_with_random(origin, owner, method, value, insecure, fill_random)
}

/// Validates the login before requesting entropy, then installs a fresh binding and its active
/// output redactions together; failed validation or entropy leaves the registry unchanged.
fn activate_with_random(
    origin: &str,
    owner: &str,
    method: Method,
    value: &str,
    insecure: bool,
    mut random: impl FnMut(&mut [u8; 16]) -> Result<(), &'static str>,
) -> Result<String, &'static str> {
    rustgis_data::login_address::set_validator(check);
    if self::origin(origin).as_deref() != Some(origin) {
        return Err("A login requires a canonical HTTP or HTTPS origin.");
    }
    if origin.starts_with("http://") && !insecure {
        return Err("An HTTP login requires explicit permission to send credentials insecurely.");
    }
    let valid_name = match &method {
        Method::Header(name) => valid_header_name(name),
        Method::Query(name) => valid_query_name(name),
    };
    if !valid_name {
        return Err("The login header or query parameter name is not allowed.");
    }
    if !valid_value(value) {
        return Err("The login credential must be nonempty, within the size limit, and contain no controls.");
    }
    let mut redactions = Vec::with_capacity(4);
    add_redactions(&mut redactions, value);
    if let Some(payload) = value.get(..6).filter(|s| s.eq_ignore_ascii_case("Basic ")).and_then(|_| value.get(6..))
        && !payload.is_empty()
    {
        add_redactions(&mut redactions, payload);
        // A server may echo the raw Basic password rather than its wire encoding. The complete
        // header was bounded above, so decoding cannot allocate beyond that input's size.
        if let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(payload)
            && let Ok(decoded) = std::str::from_utf8(&decoded)
            && let Some((_, password)) = decoded.split_once(':')
            && !password.is_empty()
        {
            add_redactions(&mut redactions, password);
        }
    }
    let mut entries = entries();
    // A token is the only thing that scopes a bound address to its login, so it is random. A
    // collision (never expected at 128 bits) is retried rather than replacing a live login.
    for _ in 0..4 {
        let binding = random_token(&mut random)?;
        if !entries.contains_key(&binding) {
            // Only a successful activation reserves a query name for address redaction.
            if let Method::Query(name) = &method
                && !rustgis_data::address::note_login_query_name(name)
            {
                return Err(TOO_MANY_QUERY_NAMES);
            }
            entries.insert(
                binding.clone(),
                Entry { owner: owner.to_string(), origin: origin.to_string(), method, value: Secret::new(value), redactions },
            );
            return Ok(binding);
        }
    }
    Err("The service login binding could not be created.")
}

/// 128 unpredictable bits as 32 lower-case hex digits.
fn random_token(random: &mut impl FnMut(&mut [u8; 16]) -> Result<(), &'static str>) -> Result<String, &'static str> {
    let mut bytes = [0u8; 16];
    random(&mut bytes)?;
    Ok(format!("{:032x}", u128::from_be_bytes(bytes)))
}

// The browser backend calls globalThis.crypto.getRandomValues (windows and workers), catching
// missing APIs and exceptions. No binding is installed if the secure source fails.
fn fill_random(bytes: &mut [u8; 16]) -> Result<(), &'static str> {
    getrandom::fill(bytes).map_err(|_| "No random numbers are available for a login binding.")
}

fn add_redactions(redactions: &mut Vec<Redaction>, value: &str) {
    redactions.push(Redaction { value: Secret::new(value), encoded: false });
    let encoded = percent_encode(value);
    if encoded != value {
        redactions.push(Redaction { value: Secret::new(encoded), encoded: true });
    }
}

/// Revokes exactly one login generation; another session's bindings are untouched.
pub fn deactivate(binding: &str) -> bool {
    let removed = entries().remove(binding).is_some();
    if removed {
        crate::cache::clear();
    }
    removed
}

pub fn owner(binding: &str) -> Option<String> {
    entries().get(binding).map(|entry| entry.owner.clone())
}

pub fn is_active(binding: &str) -> bool {
    entries().contains_key(binding)
}

pub use rustgis_data::login_address::inner;

const REVOKED: &str = "The service login is no longer active. Sign in to this connection again.";

/// Binds a same-origin address to a live login generation.
pub fn bind(binding: &str, url: &str) -> Result<String, &'static str> {
    let entries = entries();
    let entry = entries.get(binding).ok_or(REVOKED)?;
    let url = inner(url);
    if origin(url).as_deref() != Some(entry.origin.as_str()) {
        return Err("The service login does not cover this address.");
    }
    rustgis_data::login_address::bind(binding, &entry.origin, url)
}

pub use rustgis_data::login_address::inherit;

/// Checks runtime authority before any cache (including archive/style/sprite caches) is read.
pub fn check(address: &str) -> Result<(), &'static str> {
    let address =
        if rustgis_data::login_address::is_wrapper(address) { address } else { address.split_once('@').map_or(address, |(_, a)| a) };
    let Some(b) = rustgis_data::login_address::parse(address)? else { return Ok(()) };
    let entries = entries();
    let entry = entries.get(b.binding).ok_or(REVOKED)?;
    if entry.origin != b.origin || origin(b.url).as_deref() != Some(b.origin) {
        return Err("The service login does not cover this address.");
    }
    Ok(())
}

/// A request-only URL and optional header. Debug never reveals either field.
pub struct Applied {
    pub url: String,
    pub header: Option<(String, String)>,
}

impl fmt::Debug for Applied {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Applied(<redacted>)")
    }
}

/// Prepares only an explicitly bound login; bare URLs are anonymous.
pub fn apply(address: &str) -> Result<Option<Applied>, &'static str> {
    let Some(b) = rustgis_data::login_address::parse(address)? else { return Ok(None) };
    let entries = entries();
    let entry = entries.get(b.binding).ok_or(REVOKED)?;
    if entry.origin != b.origin || origin(b.url).as_deref() != Some(b.origin) {
        return Err("The service login does not cover this address.");
    }
    let applied = match &entry.method {
        Method::Header(name) => Applied { url: b.url.to_string(), header: Some((name.clone(), entry.value.expose().to_string())) },
        Method::Query(name) => Applied { url: set_query_parameter(b.url, name, Some(entry.value.expose())), header: None },
    };
    Ok(Some(applied))
}

/// `url` without the query parameter `name`, however its name is cased or escaped. A key given
/// in an address is a secret the connection never keeps.
pub fn without_query_parameter(url: &str, name: &str) -> String {
    set_query_parameter(url, name, None)
}

/// `url` with the query parameter `name` set to `value`, percent-encoded, or left out (`None`). A
/// parameter of that name already in the address (its case aside, and however its name is
/// escaped) is replaced: the login's value wins and is sent once. The fragment stays after the
/// query. Other pairs keep their text; empty ones go.
fn set_query_parameter(url: &str, name: &str, value: Option<&str>) -> String {
    let (address, fragment) = url.split_once('#').map_or((url, None), |(address, fragment)| (address, Some(fragment)));
    let (head, query) = address.split_once('?').unwrap_or((address, ""));
    let mut kept = String::with_capacity(query.len() + name.len() + value.map_or(0, str::len) + 2);
    let mut matched = false;
    for pair in query.split('&') {
        // A legacy `;` separator may hide the parameter inside an `&` pair.
        let parts: Vec<&str> = pair
            .split(';')
            .filter(|part| {
                let hit = query_name_matches(part.split('=').next().unwrap_or_default(), name);
                matched |= hit;
                !part.is_empty() && !hit
            })
            .collect();
        if !parts.is_empty() {
            if !kept.is_empty() {
                kept.push('&');
            }
            kept.push_str(&parts.join(";"));
        }
    }
    // Nothing to take out: the address is returned as it was.
    if value.is_none() && !matched {
        return url.to_string();
    }
    if let Some(value) = value {
        if !kept.is_empty() {
            kept.push('&');
        }
        kept.push_str(name);
        kept.push('=');
        append_percent_encoded(&mut kept, value);
    }
    let mut result = String::with_capacity(url.len() + kept.len());
    result.push_str(head);
    if !kept.is_empty() {
        result.push('?');
        result.push_str(&kept);
    }
    if let Some(fragment) = fragment {
        result.push('#');
        result.push_str(fragment);
    }
    result
}

fn unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~')
}

fn percent_encode(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    append_percent_encoded(&mut result, value);
    result
}

fn append_percent_encoded(result: &mut String, value: &str) {
    for b in value.bytes() {
        if unreserved(b) {
            result.push(char::from(b));
        } else {
            result.push('%');
            let digit = |n| char::from(if n < 10 { b'0' + n } else { b'A' + n - 10 });
            result.push(digit(b >> 4));
            result.push(digit(b & 15));
        }
    }
}

/// Redacts active raw credentials, percent-encoded credentials (with either hex case), and
/// the bare Base64 payload of Basic credentials. Matching the original input in one pass
/// prevents overlapping values or a credential resembling `[redacted]` from leaking secrets.
/// Matching covers every active login in this process; short passwords can replace ordinary
/// text in another session's output. Revoked credentials are not retained for redaction.
pub fn scrub(text: &str) -> String {
    let entries = entries();
    if entries.is_empty() && !text.contains(rustgis_data::login_address::PREFIX) {
        return text.to_string();
    }
    let mut patterns: Vec<_> = entries.values().flat_map(|entry| &entry.redactions).collect();
    patterns.sort_unstable_by_key(|pattern| std::cmp::Reverse(pattern.value.expose().len()));
    scrub_patterns(text, &patterns)
}

/// Cleans generated error text before active values can obscure its address syntax.
/// Runtime request bindings and successful data values must not use this output projection.
pub fn scrub_error(text: &str) -> String {
    let unbound = scrub_patterns(text, &[]);
    if let Some(strict) = rustgis_data::diagnostics::secret_continuation_cut(&unbound) {
        return scrub(&strict);
    }
    let (public, _) = rustgis_data::address::scrub_addresses(&unbound);
    scrub(&public)
}

/// Removes runtime capability headers and the supplied active patterns in one pass.
fn scrub_patterns(text: &str, patterns: &[&Redaction]) -> String {
    // Strip capabilities even after revocation, when there are no active credential patterns.
    let mut result = String::with_capacity(text.len());
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(wrapper) = rest.strip_prefix(rustgis_data::login_address::PREFIX) {
            // The hex token and length-prefixed origin precede the ordinary URL.
            let url = wrapper
                .split_once(':')
                .and_then(|(_, r)| r.split_once(':'))
                .and_then(|(len, r)| len.parse::<usize>().ok().and_then(|n| r.get(n..)))
                .and_then(|r| r.strip_prefix(':'));
            if let Some(url) = url.filter(|u| u.starts_with("http://") || u.starts_with("https://")) {
                rest = url;
                continue;
            }
            result.push_str("[redacted]");
            rest = wrapper.split_once(char::is_whitespace).map_or("", |(_, r)| r);
            continue;
        }
        if let Some(pattern) = patterns.iter().find(|pattern| {
            let value = pattern.value.expose();
            rest.get(..value.len())
                .is_some_and(|prefix| if pattern.encoded { encoded_matches(prefix.as_bytes(), value.as_bytes()) } else { prefix == value })
        }) {
            result.push_str("[redacted]");
            rest = rest.get(pattern.value.expose().len()..).unwrap_or_default();
        } else if let Some(c) = rest.chars().next() {
            result.push(c);
            rest = rest.get(c.len_utf8()..).unwrap_or_default();
        }
    }
    result
}

fn encoded_matches(actual: &[u8], expected: &[u8]) -> bool {
    let mut actual = actual.iter();
    let mut expected = expected.iter();
    while let Some(b) = expected.next() {
        if actual.next() != Some(b) {
            return false;
        }
        if *b == b'%' {
            for _ in 0..2 {
                match (actual.next(), expected.next()) {
                    (Some(a), Some(b)) if a.eq_ignore_ascii_case(b) => {}
                    _ => return false,
                }
            }
        }
    }
    actual.next().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_origins() {
        for (url, expected) in [
            ("HTTPS://Example.COM.:443/a?b=c#d", "https://example.com"),
            ("HTTP://Example.COM.:80/a", "http://example.com"),
            ("https://Example.COM:00443", "https://example.com"),
            ("https://Example.COM.:8443/", "https://example.com:8443"),
            ("https://[2001:0DB8::1]:443/", "https://[2001:db8::1]"),
            ("http://localhost:8080", "http://localhost:8080"),
        ] {
            assert_eq!(origin(url).as_deref(), Some(expected));
        }
    }

    #[test]
    fn refuses_unsafe_origins() {
        for url in [
            "https://user:password@example.com",
            "https://@example.com",
            "https://example.com/a b",
            " https://example.com",
            "https://example.com/\n",
            "https://example.com/\u{a0}",
            "ftp://example.com",
            "https://",
            "https:///path",
            "https://:443",
            "https://example.com:",
            "https://example.com:65536",
            "https://example.com:-1",
            "https://example.com:abc",
            "https://example.com\\other",
            "https://example.com/a\\b",
            "https://[bad]:443",
            "https://[::1]extra",
            "https://host:443:80",
            "https://example.com%2fother",
            "https://example..com",
        ] {
            assert_eq!(origin(url), None, "{url:?}");
        }
    }

    #[test]
    fn validates_names_and_values() {
        assert!(valid_header_name("Authorization"));
        assert!(valid_header_name("X-API_Key!"));
        assert!(valid_header_name(&"a".repeat(MAX_NAME)));
        for name in [
            "",
            "Host",
            "CONNECTION",
            "content-length",
            "Transfer-Encoding",
            "TE",
            "Upgrade",
            "Keep-Alive",
            "Range",
            "Accept-Encoding",
            "Cookie",
            "Origin",
            "Referer",
            "User-Agent",
            "Content-Type",
            "ACCEPT",
            "Proxy-Authorization",
            "SEC-Fetch-Site",
            "x y",
            "x:y",
            "ümlaut",
        ] {
            assert!(!valid_header_name(name), "{name}");
        }
        assert!(!valid_header_name(&"a".repeat(MAX_NAME + 1)));
        assert!(valid_value(" a password with spaces é "));
        assert!(valid_value(&"a".repeat(MAX_VALUE)));
        for value in ["", "bad\rvalue", "bad\nvalue", "bad\tvalue", "bad\0value", "bad\u{7f}value", "bad\u{85}value"] {
            assert!(!valid_value(value));
        }
        assert!(!valid_value(&"é".repeat(MAX_VALUE / 2 + 1)));
    }

    #[test]
    fn standard_basic_authorization() {
        assert_eq!(basic("Aladdin", "open sesame"), "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ==");
        assert_eq!(basic("", ""), "Basic Og==");
        assert_eq!(basic("user", "p:a:ss"), "Basic dXNlcjpwOmE6c3M=");
    }

    #[test]
    fn http_requires_permission_and_bindings_reject_scheme_host_and_port_changes() {
        let _turn = crate::cache::turn();
        assert!(activate("http://insecure-login.test", "http", Method::Header("X-Key".into()), "short", false).is_err());
        let binding = activate("https://scoped-login.test", "scope", Method::Header("X-Key".into()), "scope-secret", false).unwrap();
        let bound = bind(&binding, "https://scoped-login.test/tile").unwrap();
        for url in ["http://scoped-login.test/tile", "https://scoped-login.test:444/tile", "https://foreign.test/tile"] {
            assert!(bind(&binding, url).is_err());
            assert_eq!(inherit(&bound, url), url);
            assert!(apply(url).unwrap().is_none());
        }
        assert!(is_active(&binding));
        assert!(deactivate(&binding));
        assert!(!is_active(&binding));
    }

    #[test]
    fn bindings_isolate_connections_and_revoke_generations() {
        let _turn = crate::cache::turn();
        let target = "https://binding-login.example";
        let a = activate(target, "session-a/connection", Method::Header("X-Key".into()), "a", false).unwrap();
        let b = activate(target, "session-b/connection", Method::Header("X-Key".into()), "b", false).unwrap();
        let ua = bind(&a, &format!("{target}/tile")).unwrap();
        let ub = bind(&b, &format!("{target}/tile")).unwrap();
        assert_eq!(apply(&ua).unwrap().unwrap().header.unwrap().1, "a");
        assert_eq!(apply(&ub).unwrap().unwrap().header.unwrap().1, "b");
        assert!(apply(target).unwrap().is_none());
        assert!(bind(&a, "https://other.example/tile").is_err());
        assert_eq!(inherit(&ua, "https://other.example/tile"), "https://other.example/tile");
        assert!(deactivate(&a));
        assert!(check(&ua).is_err());
        assert!(apply(&ua).is_err());
        assert!(apply(&ub).unwrap().is_some());
        assert!(deactivate(&b));
    }

    #[test]
    fn failed_random_source_installs_nothing_and_preserves_a_live_login() {
        let _turn = crate::cache::turn();
        let target = "https://random-failure-login.example";
        let old = activate(target, "random-replacement", Method::Header("X-Key".into()), "old-credential", false).unwrap();
        let address = bind(&old, &format!("{target}/tile")).unwrap();
        for owner in ["random-first-login", "random-replacement"] {
            let mut attempts = 0;
            let result = activate_with_random(target, owner, Method::Header("X-Key".into()), "failed-credential", false, |bytes| {
                attempts += 1;
                // Even a source that writes before failing must never activate those bytes.
                bytes.fill(0x21);
                Err("No random numbers are available for a login binding.")
            });
            assert!(result.is_err());
            assert_eq!(attempts, 1);
            let expected = usize::from(owner == "random-replacement");
            assert_eq!(entries().values().filter(|entry| entry.owner == owner).count(), expected);
            assert_eq!(apply(&address).unwrap().unwrap().header.unwrap().1, "old-credential");
        }
        assert!(deactivate(&old));
    }

    #[test]
    fn failed_query_activation_does_not_reserve_its_parameter_name() {
        let _turn = crate::cache::turn();
        let origin = "https://failed-query-activation.example";
        let live = activate_with_random(origin, "live-query-test", Method::Header("X-Key".into()), "live-key", false, |bytes| {
            bytes.fill(0x51);
            Ok(())
        })
        .unwrap();
        for (name, fail_entropy) in [("zq_failed_entropy757", true), ("zq_failed_collision757", false)] {
            let result = activate_with_random(origin, name, Method::Query(name.into()), "failed-query-key", false, |bytes| {
                bytes.fill(0x51);
                if fail_entropy { Err("entropy unavailable") } else { Ok(()) }
            });
            assert!(result.is_err());
            let original = format!("{origin}/tiles?{name}=public");
            let mut address = original.clone();
            rustgis_data::address::redact_query(&mut address);
            assert_eq!(address, original, "failed activation registered {name}");
        }
        assert!(deactivate(&live));
        let name = "zq_successful_activation757";
        let live = activate(origin, "successful-query-test", Method::Query(name.into()), "successful-query-key", false).unwrap();
        let mut address = format!("{origin}/tiles?{name}=private");
        rustgis_data::address::redact_query(&mut address);
        assert_eq!(address, format!("{origin}/tiles"));
        assert!(deactivate(&live));
    }

    #[test]
    fn collisions_retry_without_overwriting_and_exhaust_after_four_attempts() {
        let _turn = crate::cache::turn();
        let target = "https://collision-login.example";
        let method = || Method::Header("X-Key".into());
        let old = activate_with_random(target, "collision-old", method(), "old-key", false, |bytes| {
            bytes.fill(0x3c);
            Ok(())
        })
        .unwrap();
        let old_address = bind(&old, &format!("{target}/tile")).unwrap();
        let mut attempts = 0;
        let next = activate_with_random(target, "collision-new", method(), "new-key", false, |bytes| {
            attempts += 1;
            bytes.fill(if attempts == 1 { 0x3c } else { 0x7a });
            Ok(())
        })
        .unwrap();
        assert_eq!(attempts, 2);
        let next_address = bind(&next, &format!("{target}/tile")).unwrap();
        assert_eq!(apply(&old_address).unwrap().unwrap().header.unwrap().1, "old-key");
        assert_eq!(apply(&next_address).unwrap().unwrap().header.unwrap().1, "new-key");

        attempts = 0;
        let result = activate_with_random(target, "collision-exhausted", method(), "failed-key", false, |bytes| {
            attempts += 1;
            bytes.fill(0x3c);
            Ok(())
        });
        assert_eq!(result.unwrap_err(), "The service login binding could not be created.");
        assert_eq!(attempts, 4);
        assert!(!entries().values().any(|entry| entry.owner == "collision-exhausted"));
        assert_eq!(apply(&old_address).unwrap().unwrap().header.unwrap().1, "old-key");
        assert!(deactivate(&old));
        assert!(deactivate(&next));
    }

    #[test]
    fn encoded_basic_limit_and_short_credentials() {
        assert!(valid_basic("", ""));
        assert!(valid_basic("a", "b"));
        assert!(!valid_basic("a:b", "c"));
        assert!(!valid_basic("a", "\n"));
        for n in [3064, 3065, 3066, 4096] {
            let password = "p".repeat(n);
            assert_eq!(valid_basic("", &password), valid_value(&basic("", &password)));
        }
    }

    /// Identity includes an empty session and ignores a different session with a shared prefix.
    #[test]
    fn runtime_identity_binds_session_generations_and_absence() {
        let _turn = crate::cache::turn();
        let session = 9_150_001;
        let empty = account_identity(session).unwrap();
        assert!(empty.is_current());
        let unrelated =
            activate("https://identity-login.example", "91500010/other", Method::Header("X-Key".into()), "synthetic-identity-other", false)
                .unwrap();
        assert!(empty.is_current());
        let binding = activate(
            "https://identity-login.example",
            "9150001/connection",
            Method::Header("X-Key".into()),
            "synthetic-identity-first",
            false,
        )
        .unwrap();
        assert!(!empty.is_current());
        let active = account_identity(session).unwrap();
        assert!(active.is_current() && active == account_identity(session).unwrap());
        assert!(deactivate(&unrelated));
        assert!(active.is_current());
        assert!(deactivate(&binding));
        assert!(!active.is_current());
        let replacement = activate(
            "https://identity-login.example",
            "9150001/connection",
            Method::Header("X-Key".into()),
            "synthetic-identity-first",
            false,
        )
        .unwrap();
        assert!(active != account_identity(session).unwrap(), "same credential is a new generation");
        assert!(deactivate(&replacement));
    }

    #[test]
    fn scrubs_header_and_basic_payload() {
        let _turn = crate::cache::turn();
        let password = "scrub-password: /é";
        let value = basic("scrub-user", password);
        let binding = activate("https://scrub-login.example", "scrub", Method::Header("Authorization".into()), &value, false).unwrap();
        let payload = value.strip_prefix("Basic ").unwrap();
        assert_eq!(scrub(&format!("{value} {payload} {}", percent_encode(&value))), "[redacted] [redacted] [redacted]");
        assert_eq!(scrub(&format!("server echoed {password} {}", percent_encode(password))), "server echoed [redacted] [redacted]");
        assert_eq!(scrub("scrub-user"), "scrub-user");
        assert_eq!(scrub("a clean server message"), "a clean server message");
        let url = bind(&binding, "https://scrub-login.example/tile").unwrap();
        assert!(!scrub(&format!("failed: {url}")).contains(rustgis_data::login_address::PREFIX));
        assert_eq!(format!("{:?}", apply(&url).unwrap().unwrap()), "Applied(<redacted>)");
        assert!(deactivate(&binding));
        let message = scrub(&format!("failed: {url}"));
        assert!(!message.contains(rustgis_data::login_address::PREFIX) && !message.contains(&binding));
    }

    #[test]
    fn query_names_are_plain_unreserved_text() {
        for name in ["a", "AZaz09-_.~", &"a".repeat(MAX_NAME)] {
            assert!(valid_query_name(name), "{name}");
        }
        for name in ["", "_", "-.~", "a b", "a=b", "a&b", "a;b", "a#b", "a?b", "a/b", "a%41", "a+b", "é", "a\n", &"a".repeat(MAX_NAME + 1)]
        {
            assert!(!valid_query_name(name), "{name:?}");
            assert!(activate("https://query-name.example", "name", Method::Query(name.into()), "value", false).is_err(), "{name:?}");
        }
    }

    #[test]
    fn query_key_is_encoded_replaces_its_parameter_once_and_keeps_the_fragment() {
        let _turn = crate::cache::turn();
        let target = "https://query-login.example";
        let binding = activate(target, "query", Method::Query("API_Key".into()), "query value/+?&=é", false).unwrap();
        let value = "query%20value%2F%2B%3F%26%3D%C3%A9";
        for (url, expected) in [
            ("tiles", "tiles?API_Key=VALUE"),
            ("tiles#fragment?ignored", "tiles?API_Key=VALUE#fragment?ignored"),
            ("tiles?x=1#f", "tiles?x=1&API_Key=VALUE#f"),
            ("tiles?", "tiles?API_Key=VALUE"),
            ("tiles?x=1&", "tiles?x=1&API_Key=VALUE"),
            ("tiles?api_KEY=old", "tiles?API_Key=VALUE"),
            ("tiles?x=1&%61PI%5fKey=old&y=2&API_Key", "tiles?x=1&y=2&API_Key=VALUE"),
            ("tiles?%2561PI_key=old", "tiles?API_Key=VALUE"),
            ("tiles?api_key=a&api_key=b", "tiles?API_Key=VALUE"),
            ("tiles?a=1;api_key=old;b=2&c=3", "tiles?a=1;b=2&c=3&API_Key=VALUE"),
            ("tiles?api_key_other=1&x_API_Key=2", "tiles?api_key_other=1&x_API_Key=2&API_Key=VALUE"),
        ] {
            let bound = bind(&binding, &format!("{target}/{url}")).unwrap();
            let applied = apply(&bound).unwrap().unwrap();
            assert_eq!(applied.url, format!("{target}/{}", expected.replace("VALUE", value)), "{url}");
            assert!(applied.header.is_none());
            // The bound address, the one kept and logged, still has no key.
            assert!(!bound.contains("query%20value") && !bound.contains("query value"));
        }
        // Only the origin's own addresses carry it.
        for url in ["http://query-login.example/tile", "https://query-login.example:444/tile", "https://other.example/tile"] {
            assert!(bind(&binding, url).is_err());
            assert!(apply(url).unwrap().is_none());
        }
        assert!(deactivate(&binding));
    }

    #[test]
    fn query_key_is_scrubbed_raw_and_encoded_and_never_printed() {
        let _turn = crate::cache::turn();
        let target = "https://query-scrub.example";
        let secret = "scrub query/+é key";
        let binding = activate(target, "query-scrub", Method::Query("scrub_param".into()), secret, false).unwrap();
        let encoded = percent_encode(secret);
        assert_eq!(
            scrub(&format!("echoed {secret} and {encoded} and {}", encoded.to_ascii_lowercase())),
            "echoed [redacted] and [redacted] and [redacted]"
        );
        let url = bind(&binding, &format!("{target}/tile")).unwrap();
        let applied = apply(&url).unwrap().unwrap();
        assert!(applied.url.ends_with(&format!("scrub_param={encoded}")));
        assert_eq!(format!("{applied:?}"), "Applied(<redacted>)");
        let failure = scrub_error(&format!("{}: refused ({})", applied.url, secret));
        assert!(!failure.contains(secret) && !failure.contains(&encoded), "{failure}");
        assert!(deactivate(&binding));
        assert_eq!(scrub("scrub_param"), "scrub_param");
    }

    #[test]
    fn without_query_parameter_drops_every_spelling_and_changes_nothing_else() {
        for (url, expected) in [
            ("https://h/p?api_key=1", "https://h/p"),
            ("https://h/p?a=1&API_KEY=1&b=2#f", "https://h/p?a=1&b=2#f"),
            ("https://h/p?%41pi_key=1&a=1", "https://h/p?a=1"),
            ("https://h/p?%252541pi_key=1&a=1", "https://h/p?a=1"),
            ("https://h/p?a=1;Api_Key=1&b=2", "https://h/p?a=1&b=2"),
            ("https://h/p?api_key_2=1&x_api_key", "https://h/p?api_key_2=1&x_api_key"),
            // Nothing to take out: the text is returned as it was, empty pairs and all.
            ("https://h/p?", "https://h/p?"),
            ("https://h/p?a=1&&b=2&", "https://h/p?a=1&&b=2&"),
            ("https://h/p#f", "https://h/p#f"),
        ] {
            assert_eq!(without_query_parameter(url, "api_key"), expected, "{url}");
        }
    }

    #[test]
    fn junk_addresses_and_names_never_panic_when_the_key_is_set() {
        for url in ["", "?", "#", "?#", "&&&", "?=&=", "?%", "?%4", "?%zz=1", "?é=1&%c3=2", "?;;;", "?a=%ff%fe", "https://h?a#b?c#d"] {
            let _ = set_query_parameter(url, "api_key", Some("v"));
            let _ = without_query_parameter(url, "api_key");
        }
        assert!(!query_name_matches("%", "a") && !query_name_matches("%4", "a") && !query_name_matches("%zz", "a"));
        assert!(query_name_matches("%41", "a") && query_name_matches("A", "a") && !query_name_matches("ab", "a"));
    }

    #[test]
    fn a_login_parameter_name_marks_addresses_that_carry_it_as_keys() {
        let _turn = crate::cache::turn();
        let mut plain = "https://marker.example/wmts?zqmarkerlogin9=hidden&layer=a".to_string();
        rustgis_data::address::redact_query(&mut plain);
        assert_eq!(plain, "https://marker.example/wmts?zqmarkerlogin9=hidden&layer=a");
        let binding = activate("https://marker.example", "marker", Method::Query("ZQ_Marker-Login9".into()), "marker-key", false).unwrap();
        let mut marked = "https://marker.example/wmts?zq_%254darker-login9=hidden&zqmarkerlogin9=dataset&layer=a".to_string();
        rustgis_data::address::redact_query(&mut marked);
        assert_eq!(marked, "https://marker.example/wmts?zqmarkerlogin9=dataset&layer=a");
        assert!(deactivate(&binding));
    }
}
