//! Secret-free runtime addresses bound to one web-service connection login.
//! Saved runtime tokens must be discarded and rebound by the owning session on project open.

use std::net::Ipv6Addr;

/// Canonical HTTP(S) origin: lowercase scheme and host, without a trailing host dot or
/// the scheme's default port. Paths, queries and fragments do not form part of the origin.
/// User information, whitespace, controls, backslashes and malformed authorities are refused.
fn raw_origin(url: &str) -> Option<String> {
    if url.chars().any(|c| c.is_whitespace() || c.is_control() || c == '\\') {
        return None;
    }
    let (scheme, rest) = url.split_once("://")?;
    let scheme = if scheme.eq_ignore_ascii_case("https") {
        "https"
    } else if scheme.eq_ignore_ascii_case("http") {
        "http"
    } else {
        return None;
    };
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let (host, port) = if let Some(ip) = authority.strip_prefix('[') {
        let (ip, tail) = ip.split_once(']')?;
        let host = format!("[{}]", ip.parse::<Ipv6Addr>().ok()?);
        let port = if tail.is_empty() { None } else { Some(tail.strip_prefix(':')?) };
        (host, port)
    } else {
        let (host, port) = authority.split_once(':').map_or((authority, None), |(h, p)| (h, Some(p)));
        let host = host.strip_suffix('.').unwrap_or(host);
        if host.is_empty() || host.contains(['[', ']', '%', '<', '>', '"', '^', '`', '{', '}', '|']) || host.split('.').any(str::is_empty) {
            return None;
        }
        (host.to_lowercase(), port)
    };
    let port = match port {
        Some(port) => {
            if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            Some(port.parse::<u16>().ok()?)
        }
        None => None,
    };
    match port.filter(|p| *p != if scheme == "https" { 443 } else { 80 }) {
        Some(port) => Some(format!("{scheme}://{host}:{port}")),
        None => Some(format!("{scheme}://{host}")),
    }
}
pub const PREFIX: &str = "rustgis-login:";
const BAD: &str = "not a bound service login address";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoundLogin<'a> {
    pub binding: &'a str,
    pub origin: &'a str,
    pub url: &'a str,
}

pub fn is_wrapper(address: &str) -> bool {
    address.starts_with(PREFIX)
}

pub fn parse(address: &str) -> Result<Option<BoundLogin<'_>>, &'static str> {
    let Some(rest) = address.strip_prefix(PREFIX) else { return Ok(None) };
    if address.len() > 16384 || address.chars().any(char::is_control) {
        return Err(BAD);
    }
    let (binding, rest) = rest.split_once(':').ok_or(BAD)?;
    if !(16..=64).contains(&binding.len()) || !binding.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(BAD);
    }
    let (len, rest) = rest.split_once(':').ok_or(BAD)?;
    if len.len() > 4 || len.is_empty() || !len.bytes().all(|b| b.is_ascii_digit()) {
        return Err(BAD);
    }
    let len: usize = len.parse().map_err(|_| BAD)?;
    let origin = rest.get(..len).ok_or(BAD)?;
    let url = rest.get(len..).and_then(|s| s.strip_prefix(':')).ok_or(BAD)?;
    if raw_origin(origin).as_deref() != Some(origin) || raw_origin(url).as_deref() != Some(origin) {
        return Err(BAD);
    }
    Ok(Some(BoundLogin { binding, origin, url }))
}

pub fn bind(binding: &str, origin: &str, url: &str) -> Result<String, &'static str> {
    let address = format!("{PREFIX}{binding}:{}:{origin}:{url}", origin.len());
    parse(&address)?;
    Ok(address)
}

pub fn inner(address: &str) -> &str {
    parse(address).ok().flatten().map_or(address, |b| b.url)
}

/// Canonical origin of a plain URL or a well-formed bound URL.
pub fn origin(address: &str) -> Option<String> {
    raw_origin(inner(address))
}

pub fn rebound(address: &str, binding: &str) -> Option<String> {
    let b = parse(address).ok().flatten()?;
    bind(binding, b.origin, b.url).ok()
}

pub fn reached(address: &str) -> &str {
    if is_wrapper(address) {
        inner(address)
    } else {
        address.split_once('@').filter(|(_, a)| is_wrapper(a)).map_or(address, |(_, a)| inner(a))
    }
}

/// Canonical origin of the scheme and authority alone (`https://host:port`), ignoring the path,
/// query and fragment. The authority itself is held to `raw_origin`'s rules, so whitespace,
/// controls, backslashes or user information before the first `/`, `?` or `#` still refuse it.
fn authority_origin(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    raw_origin(url.get(..scheme.len() + 3 + end)?)
}

/// A URL whose path, query or fragment holds whitespace, controls or backslashes, rewritten only
/// in those characters so the bound wrapper accepts it: trailing spaces and controls, tabs and
/// line breaks are dropped and a backslash in the path is a `/` (as a WHATWG URL parser such as
/// the `url` crate does on sending); other whitespace, controls and a backslash in the query or
/// fragment are percent-encoded (WHATWG sends a query backslash raw, which the wrapper refuses).
/// Other characters, template braces included, are left as they are; the HTTP client encodes
/// them as it would for any address. Only called once the authority is known to be clean,
/// which this keeps.
fn as_sent(url: &str) -> String {
    let url = url.trim_end_matches(|c: char| c <= ' ');
    let mut out = String::with_capacity(url.len());
    let mut in_path = true;
    for c in url.chars() {
        match c {
            '\t' | '\n' | '\r' => {}
            '?' | '#' => {
                in_path = false;
                out.push(c);
            }
            '\\' if in_path => out.push('/'),
            c if c == '\\' || c.is_whitespace() || c.is_control() => {
                let mut buf = [0; 4];
                for b in c.encode_utf8(&mut buf).bytes() {
                    out.push_str(&format!("%{b:02X}"));
                }
            }
            c => out.push(c),
        }
    }
    out
}

/// Retains a document's binding for same-origin derived addresses only.
pub fn inherit(base: &str, url: &str) -> String {
    let url = inner(url);
    let Ok(Some(b)) = parse(base) else { return url.to_string() };
    // Compare on the authority alone, so a same-origin link whose path or query holds a space,
    // a backslash or a control character keeps its login instead of silently going anonymous.
    // Such a link is written as it would be sent; the authority, and so the origin, is unchanged.
    if authority_origin(url).as_deref() == Some(b.origin) {
        let url = if raw_origin(url).is_some() { url.to_string() } else { as_sent(url) };
        // Keep the actual derived binding even when its size exceeds the parser's bound.
        // Request validation then refuses it instead of sending an anonymous request.
        format!("{PREFIX}{}:{}:{}:{url}", b.binding, b.origin.len(), b.origin)
    } else {
        url.to_string()
    }
}

type Validator = fn(&str) -> Result<(), &'static str>;
static VALIDATOR: std::sync::OnceLock<Validator> = std::sync::OnceLock::new();

/// Installs the runtime authority check supplied by the networking crate.
pub fn set_validator(validator: Validator) {
    let _ = VALIDATOR.set(validator);
}

/// Bound cached data is unreadable unless its owning runtime still authorizes the binding.
pub fn check(address: &str) -> Result<(), &'static str> {
    if !is_wrapper(address) {
        return Ok(());
    }
    parse(address)?;
    VALIDATOR.get().ok_or("The service login is no longer active. Sign in to this connection again.")?(address)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn binding_length_and_alphabet_are_checked() {
        let wrapped = |b: &str| format!("{PREFIX}{b}:14:https://a.test:https://a.test/x");
        assert!(parse(&wrapped(&"ab12".repeat(8))).unwrap().is_some());
        assert!(parse(&wrapped(&"a".repeat(15))).is_err());
        assert!(parse(&wrapped(&"a".repeat(65))).is_err());
        assert!(parse(&wrapped(&format!("{}g", "a".repeat(31)))).is_err());
        assert!(parse(&wrapped(&"A".repeat(32))).is_err());
    }

    #[test]
    fn oversized_same_origin_inheritance_is_refused_not_anonymous() {
        let base = bind("0123456789abcdef", "https://a.test", "https://a.test/collections").unwrap();
        let derived = format!("https://a.test/items?filter={}", "a".repeat(17000));
        let inherited = inherit(&base, &derived);
        assert!(is_wrapper(&inherited));
        assert!(inherited.ends_with(&derived), "keep the derived target, not the base document");
        assert_eq!(parse(&inherited).unwrap_err(), BAD);
        assert_eq!(check(&inherited).unwrap_err(), BAD);
        let valid = inherit(&base, "https://a.test/items");
        assert_eq!(parse(&valid).unwrap().unwrap().url, "https://a.test/items");
        let foreign = format!("https://b.test/items?filter={}", "a".repeat(17000));
        assert_eq!(inherit(&base, &foreign), foreign);
    }

    #[test]
    fn same_origin_link_with_unsafe_characters_keeps_its_login() {
        let base = bind("0123456789abcdef", "https://a.test", "https://a.test/wms?SERVICE=WMS").unwrap();
        for (derived, sent) in [
            ("https://a.test/wms?LAYERS=a b", "https://a.test/wms?LAYERS=a%20b"),
            ("https://a.test/a b/{z}/{x}/{y}.png", "https://a.test/a%20b/{z}/{x}/{y}.png"),
            ("https://a.test/dir\\file.png?q=a\\b", "https://a.test/dir/file.png?q=a%5Cb"),
            ("https://a.test/x?y=1\u{7}2", "https://a.test/x?y=1%072"),
            ("https://a.test/x\ty?z=\n1 ", "https://a.test/xy?z=1"),
            ("https://A.TEST:443/x#frag ment", "https://A.TEST:443/x#frag%20ment"),
            ("https://a.test?LAYERS=a\u{a0}b", "https://a.test?LAYERS=a%C2%A0b"),
        ] {
            let inherited = inherit(&base, derived);
            let bound = parse(&inherited).unwrap().unwrap();
            assert_eq!((bound.binding, bound.origin, bound.url), ("0123456789abcdef", "https://a.test", sent), "{derived}");
        }
        // The authority itself still has to be the same canonical origin: these never keep the login.
        for foreign in [
            "https://b.test/wms?LAYERS=a b",
            "https://a.test.evil.test/x y",
            "https://a.test\\@evil.test/x",
            "https://a.test\\.evil.test/x",
            "https://a.te st/x",
            "https://a.test\t.evil.test/x",
            "https://u@a.test/x y",
            "http://a.test/x y",
            "https://a.test:8443/x y",
            " https://a.test/x",
        ] {
            assert_eq!(inherit(&base, foreign), foreign, "{foreign}");
        }
        // Clean same-origin links are kept exactly.
        assert_eq!(parse(&inherit(&base, "https://a.test/wms?LAYERS=a+b")).unwrap().unwrap().url, "https://a.test/wms?LAYERS=a+b");
    }

    #[test]
    fn round_trip_and_fixed_errors() {
        let b = bind("0123456789abcdef", "https://a.test", "https://a.test/x?y=1").unwrap();
        let p = parse(&b).unwrap().unwrap();
        assert_eq!((p.binding, p.origin, p.url), ("0123456789abcdef", "https://a.test", "https://a.test/x?y=1"));
        assert_eq!(inner(&b), p.url);
        for bad in
            ["rustgis-login:secret", "rustgis-login:0123456789abcdef:1:é:https://a.test", "rustgis-login:0123456789abcdef:0::/etc/passwd"]
        {
            assert_eq!(parse(bad).unwrap_err(), BAD);
        }
    }
}
