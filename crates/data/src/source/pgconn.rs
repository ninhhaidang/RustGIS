//! PostgreSQL connection strings (libpq's `key=value` form and URIs): taken apart by libpq's
//! rules, so a connection can be named without its secrets. Pure text, so the browser build has
//! it too.

/// Keys of a libpq connection string or URI query that are secrets, or point at one, and are never
/// kept in a label.
const SECRET_KEYS: &[&str] = &["password", "sslpassword", "passfile"];

/// A PostgreSQL connection, as written to be kept (a list of sources, a log): `postgres:<keyword
/// string>` or a `postgresql://` URI, without its password or any other secret, and with no side
/// effect (unlike `duckdb::Database::parse`, nothing is remembered). Takes `postgres:`, `postgresql:`,
/// GDAL's `PG:` and the URI forms; `None` for anything else.
pub fn public_label(s: &str) -> Option<String> {
    let t = s.trim();
    let lower = t.to_ascii_lowercase();
    if lower.starts_with("postgresql://") || lower.starts_with("postgres://") {
        let (_, raw) = t.split_once("://")?;
        let authority = raw.split(['/', '?', '#']).next()?;
        let path = raw.get(authority.len()..)?.split(['?', '#']).next()?;
        // A path `@` after userinfo, or several path `@`s, can be a malformed password tail.
        // Refuse ambiguous output rather than choosing another host; percent-encode a
        // database name's `@` to keep it.
        if (authority.contains('@') && path.contains('@')) || path.matches('@').nth(1).is_some() {
            return None;
        }
        let public = split_secrets(t, SECRET_KEYS).0;
        let (_, rest) = public.split_once("://")?;
        let authority = rest.split(['/', '?', '#']).next()?;
        let hosts = authority.rsplit_once('@').map_or(authority, |(_, hosts)| hosts);
        // A non-numeric port with no delimited password is an unreadable authority. Multi-host
        // URIs, IPv6 and empty hosts (the local socket) keep libpq's existing spelling.
        if hosts.split(',').any(|host| {
            let port = if host.starts_with('[') { host.split_once(']').map(|(_, rest)| rest) } else { Some(host) };
            port.and_then(|h| h.split_once(':')).is_some_and(|(_, p)| !p.bytes().all(|b| b.is_ascii_digit()))
        }) {
            return None;
        }
        return Some(public);
    }
    let rest = ["postgresql:", "postgres:", "pg:"].iter().find(|p| lower.starts_with(**p)).and_then(|p| t.get(p.len()..))?;
    let (public, _) = split_secrets(rest.trim(), SECRET_KEYS);
    // Nothing that names a database (`postgres:dbname`, `PG:`): not a connection to keep.
    if public.is_empty() {
        return None;
    }
    Some(format!("postgres:{public}"))
}

/// Where a URI's user and password end (the `@` before the host), given what follows `://`; `None`
/// when it has none. Normally the authority runs to the first `/`, `?` or `#`, and an `@` in it
/// ends the userinfo. A password holding an unencoded `/`, `?`, `#` or `@` (malformed, but it
/// happens) ends the authority early: when the authority so cut has no `@` but a `:` that isn't
/// a port's (digits only), the last `@` ends the userinfo, so the password is never kept. A real
/// port (`host:8080/@user`) is left alone.
pub fn userinfo_end(rest: &str) -> Option<usize> {
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = rest.get(..end)?;
    if let Some(i) = authority.rfind('@') {
        return Some(i);
    }
    let (_, after) = authority.split_once(':')?;
    // `host:8080` (or an IPv6 literal's colons) is a host and port, not a user and password.
    // A port: empty (`host:`), digits, or digits then another host (libpq's `h1:5432,h2:5432`).
    let digits = after.bytes().take_while(u8::is_ascii_digit).count();
    let port = after.is_empty() || digits == after.len() || (digits > 0 && after.as_bytes().get(digits) == Some(&b','));
    if authority.starts_with('[') || port {
        return None;
    }
    // A malformed authority: the password may hold any of `/?#@`, so the last `@` ends it.
    rest.rfind('@').filter(|&i| i >= end)
}

/// A connection string without the values of `secrets` (the first, `password`, returned).
pub(crate) fn split_secrets(dsn: &str, secrets: &[&str]) -> (String, Option<String>) {
    let lower = dsn.to_ascii_lowercase();
    if lower.starts_with("postgresql://") || lower.starts_with("postgres://") {
        // scheme://[user[:password]@]host…: the credentials are in the authority, before the
        // path, query or fragment (an `@` there is something else).
        let Some((scheme, rest)) = dsn.split_once("://") else { return (dsn.to_string(), None) };
        // A PostgreSQL URI's path is only the database's name, so an `@` after the authority and
        // before any query ends a password that held an unencoded `/` (even one of digits only,
        // which reads like a port: `u:123/w@db`). Found on the whole text: a password may hold
        // a `#` too.
        let at = userinfo_end(rest).or_else(|| {
            let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
            let query = rest.find('?').unwrap_or(rest.len());
            rest.get(..query)?.rfind('@').filter(|&i| i >= end)
        });
        // A connection's fragment is never kept (it isn't part of a connection).
        let no_fragment = |s: &str| s.split('#').next().unwrap_or(s).to_string();
        if let Some(at) = at
            && let (Some(userinfo), Some(after)) = (rest.get(..at), rest.get(at + 1..))
        {
            let (after, in_query) = query_secrets(&no_fragment(after), secrets);
            return match userinfo.split_once(':') {
                // A URI's password is percent-encoded (`%40` for `@`); the one returned is the
                // password itself.
                Some((user, password)) => (format!("{scheme}://{user}@{after}"), Some(percent_decode(password))),
                None => (format!("{scheme}://{userinfo}@{after}"), in_query),
            };
        }
        let (rest, in_query) = query_secrets(&no_fragment(rest), secrets);
        return (format!("{scheme}://{rest}"), in_query);
    }
    let mut pairs = Vec::new();
    let mut password = None;
    for (key, value) in keyword_pairs(dsn) {
        if key.eq_ignore_ascii_case("password") {
            password = Some(value);
        } else if !secrets.iter().any(|s| key.eq_ignore_ascii_case(s)) {
            pairs.push((key, value));
        }
    }
    let quote = |v: &str| {
        if !v.is_empty() && !v.contains([' ', '\t', '\'', '\\']) {
            v.to_string()
        } else {
            format!("'{}'", v.replace('\\', "\\\\").replace('\'', "\\'"))
        }
    };
    (pairs.iter().map(|(k, v)| format!("{k}={}", quote(v))).collect::<Vec<_>>().join(" "), password)
}

/// A URI's text after its credentials without the query's `password` and `secrets`
/// (`?password=…`), and that password, decoded.
fn query_secrets(s: &str, secrets: &[&str]) -> (String, Option<String>) {
    let Some((base, query)) = s.split_once('?') else { return (s.to_string(), None) };
    let mut password = None;
    let mut kept = Vec::new();
    for kv in query.split('&').filter(|kv| !kv.is_empty()) {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        if crate::address::normalized_name(k).as_deref() == Some("password") {
            // The URI driver decodes the name and password once; keyword strings below are
            // intentionally not percent-decoded.
            password = Some(percent_decode(v));
        } else if !secrets.iter().any(|s| crate::address::normalized_name(k).as_deref() == Some(s)) {
            kept.push(kv);
        }
    }
    let mut public = if kept.is_empty() { base.to_string() } else { format!("{base}?{}", kept.join("&")) };
    crate::address::redact_query(&mut public);
    (public, password)
}

/// A libpq keyword/value connection string's pairs (password included), in order; a value is a
/// word, or quoted with `\'` and `\\` inside. Stops at anything malformed (an unterminated quote
/// drops the rest, which could be a password).
pub(crate) fn keyword_pairs(dsn: &str) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut chars = dsn.chars().peekable();
    loop {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        if chars.peek().is_none() {
            break;
        }
        let mut key = String::new();
        while let Some(c) = chars.next_if(|c| !c.is_whitespace() && *c != '=') {
            key.push(c);
        }
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        if chars.next() != Some('=') {
            // Not a connection string RustGIS can take apart: what came before it (never the
            // rest, which could hold a password).
            break;
        }
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        let mut value = String::new();
        if chars.next_if_eq(&'\'').is_some() {
            loop {
                match chars.next() {
                    Some('\\') => value.extend(chars.next()),
                    Some('\'') => break,
                    Some(c) => value.push(c),
                    // An unterminated quote: the rest is dropped (it could be a password).
                    None => {
                        key.clear();
                        break;
                    }
                }
            }
        } else {
            while let Some(c) = chars.next_if(|c| !c.is_whitespace()) {
                if c == '\\' {
                    value.extend(chars.next());
                } else {
                    value.push(c);
                }
            }
        }
        if key.is_empty() {
            break;
        }
        pairs.push((key, value));
    }
    pairs
}

/// Text with its `%XX` escapes decoded (a malformed escape is kept as it is).
pub(crate) fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while let Some(&c) = b.get(i) {
        let hex = |j: usize| b.get(j).and_then(|h| (*h as char).to_digit(16));
        match (c, hex(i + 1), hex(i + 2)) {
            (b'%', Some(h), Some(l)) => {
                out.push((h * 16 + l) as u8);
                i += 3;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    #[test]
    fn uri_passwords_are_decoded() {
        let (public, pw) = super::split_secrets("postgresql://u:r3ad%20p%40ss@h:5432/g", &["password"]);
        assert_eq!(public, "postgresql://u@h:5432/g");
        assert_eq!(pw.as_deref(), Some("r3ad p@ss"));
        assert_eq!(super::percent_decode("a%zz%4"), "a%zz%4");
        assert_eq!(super::percent_decode("%C3%A9"), "é");
        // A password in the query goes too (never into a project), and is returned.
        let (public, pw) =
            super::split_secrets("postgresql://u@h/g?sslmode=require&password=s%40c&sslpassword=k", &["password", "sslpassword"]);
        assert_eq!(public, "postgresql://u@h/g?sslmode=require");
        assert_eq!(pw.as_deref(), Some("s@c"));
        let (public, pw) = super::split_secrets("postgresql://h/g?password=x", &["password"]);
        assert_eq!((public.as_str(), pw.as_deref()), ("postgresql://h/g", Some("x")));
        let uri = "postgresql://u@h.invalid/db?pass%77ord=RGSYNTH_pg_20261103&sslmode=require&pass%66ile=RGSYNTH_file_20261103";
        assert_eq!(super::public_label(uri).as_deref(), Some("postgresql://u@h.invalid/db?sslmode=require"));
        let (public, password) = super::query_secrets(uri, super::SECRET_KEYS);
        assert_eq!(public, "postgresql://u@h.invalid/db?sslmode=require");
        assert_eq!(password.as_deref(), Some("RGSYNTH_pg_20261103"));
        let clean = "postgresql://u@h.invalid/db?sslmode=require";
        assert_eq!(super::public_label(clean).as_deref(), Some(clean));
        // A later path `@` could be a password tail, even after an earlier split removed its
        // colon. Withhold the ambiguous label, including an intentional raw `@` database name;
        // percent-encoding the database's `@` keeps that name unambiguous.
        for ambiguous in [
            "postgresql://user:p@ss/word@host.invalid/db",
            "postgresql://user@ss/word@host.invalid/db",
            "postgresql://user@host.invalid/db@tag",
            "postgresql://user:pass/word@host.invalid/db@tag",
            "postgresql://user:pass/word@host.invalid/db@tag@tail",
        ] {
            assert_eq!(super::public_label(ambiguous), None, "{ambiguous}");
        }
        let encoded = "postgresql://user:p%40ss%2Fword@host.invalid/db%40tag";
        assert_eq!(super::public_label(encoded).as_deref(), Some("postgresql://user@host.invalid/db%40tag"));
        let query_at = "postgresql://user@host.invalid/db?application_name=x@y";
        assert_eq!(super::public_label(query_at).as_deref(), Some(query_at));
        assert_eq!(super::public_label("postgresql://u:RGSYNTH_unreadable_20261103/path"), None);
        let keyword = "host=h.invalid dbname='a%20b' user=u password='RGSYNTH_keyword_20261103'";
        let (public, password) = super::split_secrets(keyword, super::SECRET_KEYS);
        assert_eq!(public, "host=h.invalid dbname=a%20b user=u");
        assert_eq!(password.as_deref(), Some("RGSYNTH_keyword_20261103"));
    }
}
