//! Which addresses tiles and documents may be fetched from.
//!
//! A tile layer's own address is the user's choice (a project or a command names it), so it may
//! point at this computer or the local network: a self-hosted tile server. Addresses that come
//! from somewhere else (inside a style or TileJSON document, or a redirect) may not reach into
//! the local network unless the document itself came from there, so a style found on the web
//! can't make the app probe local services. Link-local addresses, where cloud metadata services
//! answer, are never fetched.
//!
//! These spelling checks also run after template expansion. Native fetchers additionally
//! filter the resolved IPs before connecting: a public document cannot select a local
//! connection, and no request can connect to a forbidden address. The browser controls its
//! own DNS and connections; its adapter checks the normalized URL and refuses redirects whose
//! destinations it cannot inspect.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// How far an address reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Reach {
    /// The internet.
    Public,
    /// This computer or the local network (loopback, private and unique-local addresses).
    Local,
    /// Never fetched: link-local and unspecified addresses, cloud metadata hosts.
    Forbidden,
}

/// Where a fetched document actually came from. Runtime only: a document cannot grant
/// itself a local-network allowance by including a field in its JSON.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Origin {
    pub(crate) url: String,
    pub(crate) reach: Reach,
}

impl Origin {
    /// Origin for a trusted transport's normalized final destination when DNS is opaque,
    /// such as the browser. This does not perform normalization or grant credentials.
    pub fn named(url: &str) -> Self {
        Self { url: url.to_owned(), reach: reach(crate::logins::inner(url)) }
    }

    /// Provenance for a trusted native transport's actual peer, never its DNS candidates.
    /// Forbidden peers and non-web destinations are rejected before their bytes are accepted.
    pub fn connected(url: &str, peer: IpAddr) -> Result<Self, String> {
        let observed = ip_reach(peer);
        check_reach(url, Reach::Local)?;
        if observed == Reach::Forbidden {
            return Err("that peer address is never fetched".into());
        }
        Ok(Self { url: url.into(), reach: observed })
    }

    /// Retains a selected binding after the transport verifies it remained authorized through
    /// every redirect. Omit this when a hop dropped the login, even on returning to its server.
    /// The normalized destination and observed network reach remain unchanged.
    pub fn retain_binding(mut self, selected: &str) -> Result<Self, String> {
        self.url = inherit(selected, &self.url)?;
        Ok(self)
    }

    /// Checks a descendant without replacing the document's observed reach with host spelling.
    pub(crate) fn check(&self, url: &str) -> Result<(), String> {
        // Expansion can introduce a selector that was hidden from document joining.
        // Only the supplying document's own selection may survive that boundary.
        let (plain, _) = crate::s3::split_anonymous(url);
        if let Some(child) = rustgis_data::login_address::parse(plain).map_err(str::to_string)? {
            let parent = rustgis_data::login_address::parse(&self.url).map_err(str::to_string)?;
            if parent.is_none_or(|p| p.binding != child.binding || p.origin != child.origin) {
                return Err("a document cannot select another service login".into());
            }
        }
        if let Some(child) = rustgis_data::s3_address::parse(plain).map_err(str::to_string)? {
            let parent = rustgis_data::s3_address::parse(&self.url).map_err(str::to_string)?;
            if parent.is_none_or(|p| p.binding != child.binding || p.endpoint != child.endpoint) {
                return Err("a document cannot select another S3 account".into());
            }
        }
        check_reach(url, self.reach)
    }
}

/// The host of an HTTP(S) address, without user info, port or IPv6 brackets.
fn host(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if let Some(v6) = authority.strip_prefix('[') { v6.split(']').next()? } else { authority.split(':').next()? };
    Some(host.trim_end_matches('.').to_ascii_lowercase())
}

/// How far an IP address reaches.
pub fn ip_reach(ip: IpAddr) -> Reach {
    match ip {
        IpAddr::V4(v4) => v4_reach(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return v4_reach(v4);
            }
            // RFC 6052's well-known /96 translates to this IPv4 destination. Its IPv6
            // spelling must not widen a local or forbidden peer's network reach.
            if let [0x64, 0xff9b, 0, 0, 0, 0, hi, lo] = v6.segments() {
                return v4_reach(Ipv4Addr::from((u32::from(hi) << 16) | u32::from(lo)));
            }
            let seg0 = v6.segments().first().copied().unwrap_or(0);
            if v6.is_unspecified() || (seg0 & 0xffc0) == 0xfe80 {
                Reach::Forbidden
            } else if v6 == Ipv6Addr::LOCALHOST || (seg0 & 0xfe00) == 0xfc00 {
                Reach::Local
            } else {
                Reach::Public
            }
        }
    }
}

fn v4_reach(v4: Ipv4Addr) -> Reach {
    if v4.is_unspecified() || v4.is_link_local() || v4.is_broadcast() {
        Reach::Forbidden
    } else if v4.is_loopback() || v4.is_private() || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 64) {
        // 100.64.0.0/10 (carrier-grade NAT) counts as local too.
        Reach::Local
    } else {
        Reach::Public
    }
}

/// How far `url` reaches, judged by its host (names are not resolved).
pub fn reach(url: &str) -> Reach {
    let Some(h) = host(url) else { return Reach::Forbidden };
    if let Ok(ip) = h.parse::<IpAddr>() {
        return ip_reach(ip);
    }
    // Resolvers read a host whose last label is a number as an IPv4 address in the old
    // `inet_aton` forms (`127.1`, `2852039166`, `0x7f.0.0.1`): judge the address it means.
    if h.rsplit('.').next().is_some_and(|l| !l.is_empty() && (l.bytes().all(|b| b.is_ascii_digit()) || l.starts_with("0x"))) {
        return inet_aton(&h).map_or(Reach::Forbidden, v4_reach);
    }
    if ["metadata.google.internal", "metadata", "instance-data"].contains(&h.as_str()) {
        Reach::Forbidden
    } else if h == "localhost" || h.ends_with(".localhost") || h.ends_with(".local") || h.ends_with(".internal") {
        Reach::Local
    } else {
        Reach::Public
    }
}

/// An IPv4 address in any form `inet_aton` reads: one to four parts, each decimal, octal
/// (leading `0`) or hexadecimal (`0x`), the last filling the remaining bytes.
fn inet_aton(h: &str) -> Option<Ipv4Addr> {
    let parts: Vec<u64> = h
        .split('.')
        .map(|p| {
            if let Some(x) = p.strip_prefix("0x").or_else(|| p.strip_prefix("0X")) {
                if x.is_empty() { Some(0) } else { u64::from_str_radix(x, 16).ok() }
            } else if p.len() > 1 && p.starts_with('0') {
                u64::from_str_radix(&p[1..], 8).ok()
            } else {
                p.parse().ok()
            }
        })
        .collect::<Option<_>>()?;
    let (last, head) = parts.split_last()?;
    if head.len() > 3 || head.iter().any(|&p| p > 255) {
        return None;
    }
    let room = 8 * (4 - head.len() as u32);
    if room < 64 && *last >= 1u64 << room {
        return None;
    }
    let value = head.iter().enumerate().fold(*last, |acc, (i, &p)| acc | (p << (24 - 8 * i as u32)));
    u32::try_from(value).ok().map(Ipv4Addr::from)
}

/// Whether `url` is an HTTPS address. Schemes are case-insensitive (RFC 3986 §3.1), and the HTTP
/// client fetches `HTTPS://…` as `https://…`, so a check comparing the scheme must ignore case.
pub fn is_https(url: &str) -> bool {
    url.get(..8).is_some_and(|s| s.eq_ignore_ascii_case("https://"))
}

/// Whether `url` may be fetched. `from` is where it was found (a document, or the address that
/// redirected to it), or `None` when the user gave it.
pub fn check(url: &str, from: Option<&str>) -> Result<(), String> {
    check_reach(url, from.map_or(Reach::Local, |u| reach(crate::logins::inner(u))))
}

/// Keeps a selected account only on its own effective server. Explicit anonymous reads stay
/// anonymous for descendants; neither wrapper can grant a wider network reach.
pub(crate) fn inherit(from: &str, to: &str) -> Result<String, String> {
    let (plain, anonymous) = crate::s3::split_anonymous(from);
    if anonymous {
        return Ok(crate::s3::anonymous(to));
    }
    if let Some(bound) = rustgis_data::s3_address::parse(plain).map_err(str::to_string)? {
        let old = crate::s3::bound_destination(plain)?.ok_or("not a saved S3 address")?;
        let candidate = rustgis_data::s3_address::bind(bound.binding, bound.endpoint, to).map_err(str::to_string)?;
        let new = crate::s3::bound_destination(&candidate)?.ok_or("not a saved S3 address")?;
        if crate::logins::origin(&old).is_some_and(|origin| Some(origin) == crate::logins::origin(&new)) {
            return Ok(candidate);
        }
        return Ok(to.to_string());
    }
    Ok(crate::logins::inherit(from, to))
}

/// Checks a transport's normalized destination under its inherited runtime ceiling, unwrapping
/// account markers without widening it. Call before sending; Forbidden is always refused.
pub fn check_reach(url: &str, ceiling: Reach) -> Result<(), String> {
    if ceiling == Reach::Forbidden {
        return Err("a forbidden document cannot supply request destinations".into());
    }
    crate::logins::check(url).map_err(str::to_string)?;
    if let Some(bound) = rustgis_data::login_address::parse(url).map_err(str::to_string)? {
        return check_reach(bound.url, ceiling);
    }
    // An address read through a saved S3 connection is judged by where it is fetched, which its
    // own endpoint decides (no login is needed to keep it).
    if let Some(destination) = crate::s3::bound_destination(url)? {
        return check_reach(&destination, ceiling);
    }
    // An `s3://` address is judged by the web address it is fetched from.
    // An address read anonymously is judged as the address it marks.
    if let (inner, true) = crate::s3::split_anonymous(url) {
        if rustgis_data::s3_address::is_wrapper(inner) || crate::s3::split_anonymous(inner).1 {
            return Err("not a saved S3 address".into());
        }
        return if crate::s3::is_s3(inner) { check_reach(&crate::s3::https_url(url)?, ceiling) } else { check_reach(inner, ceiling) };
    }
    if crate::s3::is_s3(url) {
        return check_reach(&crate::s3::https_url(url)?, ceiling);
    }
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("https://") || lower.starts_with("http://")) {
        return Err(format!("{url}: only http:// and https:// addresses are fetched"));
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!("{url}: the address has spaces in it"));
    }
    match reach(url) {
        Reach::Forbidden => Err(format!("{url}: that address is never fetched")),
        Reach::Local if ceiling == Reach::Public => Err(format!("{url}: a document on the web can't point into the local network")),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connected_destinations_separate_names_from_observed_peers() {
        let origin = Origin::connected("https://selected.internal/style.json", "203.0.113.1".parse().unwrap()).unwrap();
        assert_eq!(origin.reach, Reach::Public);
        assert!(origin.check("http://127.0.0.1/tile").is_err());
        assert!(Origin::connected("http://metadata.google.internal/", "203.0.113.1".parse().unwrap()).is_err());
        assert!(Origin::connected("https://selected.internal/", "169.254.169.254".parse().unwrap()).is_err());
    }

    /// Translation addresses retain the IPv4 destination's policy, including on native peers.
    #[test]
    fn nat64_destinations_keep_the_embedded_ipv4_reach() {
        for (v4, expected) in [
            ("169.254.169.254", Reach::Forbidden),
            ("0.0.0.0", Reach::Forbidden),
            ("127.0.0.1", Reach::Local),
            ("10.0.0.1", Reach::Local),
            ("203.0.113.1", Reach::Public),
        ] {
            let peer: IpAddr = format!("64:ff9b::{v4}").parse().unwrap();
            let url = format!("http://[{peer}]/tile");
            assert_eq!(ip_reach(peer), expected, "translated {v4}");
            assert_eq!(reach(&url), expected);
            assert_eq!(check(&url, None).is_ok(), expected != Reach::Forbidden);
            assert_eq!(check(&url, Some("https://public.example/style.json")).is_ok(), expected == Reach::Public);
            let observed = Origin::connected("https://dns.example/tile", peer);
            if expected == Reach::Forbidden {
                assert!(observed.is_err(), "DNS cannot hide a forbidden translated peer");
            } else {
                assert_eq!(observed.unwrap().reach, expected);
            }
        }
    }

    #[test]
    fn judges_addresses() {
        assert_eq!(reach("https://tile.openstreetmap.org/1/0/0.png"), Reach::Public);
        assert_eq!(reach("http://localhost:3000/tiles"), Reach::Local);
        assert_eq!(reach("http://127.0.0.1/x"), Reach::Local);
        assert_eq!(reach("http://192.168.1.20/x"), Reach::Local);
        assert_eq!(reach("http://[::1]:8080/x"), Reach::Local);
        assert_eq!(reach("http://[fd00::1]/x"), Reach::Local);
        assert_eq!(reach("http://169.254.169.254/latest/meta-data/"), Reach::Forbidden);
        assert_eq!(reach("http://[::ffff:169.254.169.254]/"), Reach::Forbidden);
        assert_eq!(reach("http://metadata.google.internal/"), Reach::Forbidden);
        assert_eq!(reach("http://0.0.0.0/"), Reach::Forbidden);
        assert_eq!(reach("http://user@localhost:80@example.com/"), Reach::Public);
        assert_eq!(reach("garbage"), Reach::Forbidden);
        // Old IPv4 forms mean the address they spell.
        assert_eq!(reach("http://127.1:8080/"), Reach::Local);
        assert_eq!(reach("http://2852039166/latest/meta-data/"), Reach::Forbidden);
        assert_eq!(reach("http://0x7f.0.0.1/"), Reach::Local);
        assert_eq!(reach("http://0177.0.0.1/"), Reach::Local);
        assert_eq!(reach("http://0/"), Reach::Forbidden);
        assert_eq!(reach("http://134744072/"), Reach::Public);
        assert_eq!(reach("http://1.2.3.4.5/"), Reach::Forbidden);
        assert_eq!(reach("http://999.1/"), Reach::Forbidden);
        assert_eq!(reach("http://tiles.example.com/"), Reach::Public);
    }

    #[test]
    fn checks_where_addresses_come_from() {
        // The user's own choice may be local; a web document's may not.
        assert!(check("http://localhost:3000/style.json", None).is_ok());
        assert!(check("http://localhost:3000/{z}/{x}/{y}.pbf", Some("http://localhost:3000/style.json")).is_ok());
        assert!(check("http://10.0.0.5/{z}/{x}/{y}.pbf", Some("https://example.com/style.json")).is_err());
        assert!(check("https://tiles.example.com/{z}", Some("https://example.com/style.json")).is_ok());
        assert!(check("http://169.254.169.254/", None).is_err());
        assert!(check("http://127.1:8080/{z}/{x}/{y}.pbf", Some("https://example.com/style.json")).is_err());
        assert!(check("file:///etc/passwd", None).is_err());
    }

    #[test]
    fn tells_https_in_any_case() {
        for yes in ["https://a.org/x", "HTTPS://a.org/x", "HttpS://a.org"] {
            assert!(is_https(yes), "{yes}");
        }
        for no in ["http://a.org/x", "HTTP://a.org/x", "https:/a.org", "https", "", "ftp://a.org", "httpsx://a.org", "é"] {
            assert!(!is_https(no), "{no}");
        }
    }
}
