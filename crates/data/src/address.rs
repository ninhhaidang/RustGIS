//! Credential names and public address text, shared by sources, diagnostics and writers.
//!
//! A plain name is not a promise about its value. In particular it does not authorize sending
//! that value to an address advertised by a service. These helpers never inspect dataset text.

use std::borrow::Cow;

/// Maximum decoding work for one name; anything still encoded is withheld.
pub const MAX_DECODE_PASSES: usize = 4;
pub const MAX_NAME_BYTES: usize = 16 * 1024;

/// Classification after bounded decoding, without decoding a pair's value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameClass {
    Credential,
    /// Not recognized; this alone never authorizes forwarding.
    Plain,
    /// Too long, or still decodable after the budget. Never forwarded, even under a scope.
    Unresolved,
}

/// Parses only a complete hexadecimal escape supplied by the byte scanner.
fn hex(bytes: &[u8]) -> Option<u32> {
    bytes.iter().try_fold(0, |n, b| Some(n * 16 + char::from(*b).to_digit(16)?))
}

/// One byte-preserving pass. Lossy UTF-8 is applied only after all passes, so percent-encoded
/// bytes and literal Unicode take the same route. Invalid escapes remain literal.
fn decode_pass(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        if b == b'%'
            && let Some(n) = bytes.get(i + 1..i + 3).and_then(hex)
        {
            out.push(n as u8);
            i += 3;
        } else if b == b'\\'
            && bytes.get(i + 1) == Some(&b'u')
            && let Some(c) = bytes.get(i + 2..i + 6).and_then(hex).and_then(char::from_u32)
        {
            out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
            i += 6;
        } else {
            out.push(if b == b'+' { b' ' } else { b });
            i += 1;
        }
    }
    out
}

/// Repeated decoding stops at a fixed point; a fifth changing pass is unresolved.
fn decoded_name(raw: &str) -> Option<Vec<u8>> {
    if raw.len() > MAX_NAME_BYTES {
        return None;
    }
    let mut name = raw.as_bytes().to_vec();
    for _ in 0..MAX_DECODE_PASSES {
        let next = decode_pass(&name);
        if next == name {
            return Some(name);
        }
        name = next;
    }
    (decode_pass(&name) == name).then_some(name)
}

/// Canonical comparison spelling; invalid UTF-8 contributes no ASCII name characters.
fn letters(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_lowercase()).collect()
}

/// ASCII letters and digits after bounded decoding, lower-cased; `None` means Unresolved.
pub fn normalized_name(raw: &str) -> Option<String> {
    decoded_name(raw).map(|s| letters(&s))
}

/// Context is collected from decoded query names, including legacy semicolon-separated pairs.
#[derive(Default)]
pub struct QueryNames {
    sig: bool,
    sv: bool,
    state: bool,
}

impl QueryNames {
    /// Collects context flags without decoding values or retaining query payloads.
    pub fn of(query: &str) -> Self {
        let mut names = Self::default();
        for pair in query.split(['&', ';']) {
            if let Some(decoded) = decoded_name(pair_name(pair)) {
                let k = letters(decoded.split(|b| *b == b'=').next().unwrap_or(&decoded));
                names.sig |= k == "sig";
                names.sv |= k == "sv";
                names.state |= k == "state";
            }
        }
        names
    }
}

const SAS_KEYS: &[&str] =
    &["sv", "se", "sp", "sr", "st", "spr", "sip", "si", "ss", "srt", "skoid", "sktid", "skt", "ske", "sks", "skv", "sdd"];

/// Exact and containment rules over canonical names, plus query-wide context.
fn credential(k: &str, query: &QueryNames) -> bool {
    matches!(k, "key" | "appid" | "subscriptionkey" | "clientkey" | "saskey" | "pwd" | "sig" | "signature" | "sas" | "authorization" | "proxyauthorization" | "cookie" | "setcookie" | "bearer")
        // These public refusal and tracing headers share the signing family's prefix.
        || (k.starts_with("xamz") && !matches!(k, "xamznrequestid" | "xamznwafaction" | "xamzcfid"))
        || k.starts_with("xgoog")
        || [
            "password",
            "passwd",
            "secret",
            "token",
            "credential",
            "apikey",
            "accesskey",
            "privatekey",
            "accountkey",
            "sharedaccess",
            "connectionstring",
        ]
        .iter()
        .any(|w| k.contains(w))
        || (query.sig && query.sv && SAS_KEYS.contains(&k))
        || (k == "code" && query.state)
}

/// Classifies a raw name. An encoded equals sign is checked both as a name boundary and as
/// part of the whole name; values after a literal equals sign are never decoded here.
pub fn name_class(raw: &str, query: &QueryNames) -> NameClass {
    let Some(decoded) = decoded_name(raw) else { return NameClass::Unresolved };
    let before = decoded.split(|b| *b == b'=').next().unwrap_or(&decoded);
    if credential(&letters(&decoded), query) || credential(&letters(before), query) { NameClass::Credential } else { NameClass::Plain }
}

/// True for recognized credentials and unresolved names; no query context is assumed.
pub fn secret_key(raw: &str) -> bool {
    name_class(raw, &QueryNames::default()) != NameClass::Plain
}

/// Names logs also omit, beyond credential-bearing address options. Logs are output only.
pub fn log_sensitive(raw: &str) -> bool {
    if secret_key(raw) {
        return true;
    }
    let Some(decoded) = decoded_name(raw) else { return true };
    let name = String::from_utf8_lossy(&decoded);
    let k = letters(&decoded);
    if k.contains("body")
        || matches!(k.as_str(), "basic" | "authentication" | "wwwauthenticate" | "proxyauthenticate" | "userid" | "username")
    {
        return true;
    }
    // The audit's existing word policy, including plurals and camelCase, is shared here with
    // diagnostics. It deliberately omits more than replayable state does.
    let mut words = Vec::new();
    let mut word = String::new();
    let mut prev_lower = false;
    for c in name.chars() {
        if (!c.is_ascii_alphanumeric() || (c.is_ascii_uppercase() && prev_lower)) && !word.is_empty() {
            words.push(std::mem::take(&mut word));
        }
        prev_lower = c.is_ascii_lowercase() || c.is_ascii_digit();
        if c.is_ascii_alphanumeric() {
            word.push(c.to_ascii_lowercase());
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words.iter().enumerate().any(|(i, word)| {
        let w = word.strip_suffix('s').unwrap_or(word);
        matches!(
            w,
            "passphrase"
                | "jwt"
                | "privatekey"
                | "password"
                | "passwd"
                | "pwd"
                | "secret"
                | "token"
                | "apikey"
                | "auth"
                | "authorization"
                | "bearer"
                | "credential"
                | "signature"
                | "cookie"
                | "sas"
                | "dsn"
        ) || ["token", "secret", "password", "passphrase"].iter().any(|s| w.ends_with(s))
            || (w == "key"
                && (words.len() == 1
                    || i.checked_sub(1).and_then(|j| words.get(j)).is_some_and(|p| {
                        ["api", "access", "private", "secret", "account", "sas", "subscription", "client"].contains(&p.as_str())
                    })))
    })
}

/// Forwarding options compare their complete decoded spelling, including punctuation. The
/// broader credential normalization must not make an unknown name into an allowlisted one.
fn option_name(raw: &str) -> Option<String> {
    decoded_name(raw).map(|s| String::from_utf8_lossy(&s).to_ascii_lowercase())
}

/// Only a literal equals sign separates a raw name from its value.
fn pair_name(pair: &str) -> &str {
    pair.split_once('=').map_or(pair, |(k, _)| k)
}

/// A legacy semicolon credential taints the whole ampersand pair.
fn pair_class(pair: &str, names: &QueryNames) -> NameClass {
    let mut class = NameClass::Plain;
    for sub in pair.split(';') {
        match name_class(pair_name(sub), names) {
            NameClass::Unresolved => return NameClass::Unresolved,
            NameClass::Credential => class = NameClass::Credential,
            NameClass::Plain => {}
        }
    }
    class
}

/// Query text ends at the fragment, which is never interpreted as another query.
fn query(address: &str) -> &str {
    address.split('#').next().and_then(|s| s.split_once('?')).map_or("", |(_, q)| q)
}

/// Removes credential and unresolved pairs; retained pairs keep their original bytes. The
/// caller handles user-info and fragments. A legacy `;token=…` removes its entire `&` pair.
pub fn redact_query(address: &mut String) {
    let Some(at) = address.find('?').filter(|at| address.find('#').is_none_or(|end| *at < end)) else { return };
    let q = query(address);
    let names = QueryNames::of(q);
    if q.split('&').all(|p| pair_class(p, &names) == NameClass::Plain) {
        return;
    }
    let kept = q.split('&').filter(|p| !p.is_empty() && pair_class(p, &names) == NameClass::Plain).collect::<Vec<_>>().join("&");
    let fragment = address.find('#').and_then(|i| address.get(i..)).unwrap_or("").to_string();
    address.truncate(at);
    if !kept.is_empty() {
        address.push('?');
        address.push_str(&kept);
    }
    address.push_str(&fragment);
}

/// Public form of one web or S3 address. Local paths and other unrecognized text are untouched.
#[derive(Debug, PartialEq, Eq)]
pub enum Public {
    Unchanged,
    Changed {
        address: String,
        /// Omitted user-info, query pairs and fragment components; not a credential count.
        removed: u16,
    },
    /// A recognized address whose authority cannot safely be delimited; retain only its prefix.
    Withheld,
}

/// Masks supported tile placeholders in a validation copy of the authority. Equal byte lengths
/// preserve user-info offsets; the public address always retains the original template spelling.
/// Switch choices may name host labels, but cannot introduce URL delimiters or credentials.
fn template_authority(rest: &str) -> Cow<'_, str> {
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let Some(authority) = rest.get(..end) else { return Cow::Borrowed(rest) };
    let mut masked = None;
    let mut copied = 0;
    let mut scan = 0;
    while let Some(open) = authority.get(scan..).and_then(|s| s.find('{')).map(|at| scan + at) {
        let Some(close) = authority.get(open + 1..).and_then(|s| s.find('}')).map(|at| open + 1 + at) else { break };
        let token = authority.get(open + 1..close).unwrap_or("");
        let known = matches!(
            token,
            "z" | "zoom" | "x" | "y" | "-y" | "q" | "quadkey" | "s" | "bbox-epsg-3857" | "width" | "height" | "r" | "ratio"
        ) || token.strip_prefix("switch:").is_some_and(|choices| {
            choices.split(',').all(|choice| {
                let choice = choice.trim();
                !choice.is_empty() && choice.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
            })
        });
        if known {
            let out = masked.get_or_insert_with(|| String::with_capacity(rest.len()));
            out.push_str(authority.get(copied..open).unwrap_or(""));
            out.extend(std::iter::repeat_n('x', close + 1 - open));
            copied = close + 1;
        }
        scan = close + 1;
    }
    match masked {
        Some(mut out) => {
            out.push_str(rest.get(copied..).unwrap_or(""));
            Cow::Owned(out)
        }
        None => Cow::Borrowed(rest),
    }
}

/// Removes user-info, fragments and secret query pairs, or withholds an unreadable authority.
pub fn public_web_address(address: &str) -> Public {
    let text = address.trim();
    let Some((protocol, _)) = text.split_once(':') else { return Public::Unchanged };
    if !["http", "https", "s3"].iter().any(|p| protocol.eq_ignore_ascii_case(p)) {
        return Public::Unchanged;
    }
    let Some((scheme, rest)) = text.split_once("://") else { return Public::Withheld };
    let at = crate::source::pgconn::userinfo_end(&template_authority(rest));
    let rest = at.and_then(|i| rest.get(i + 1..)).unwrap_or(rest);
    let no_fragment = rest.split('#').next().unwrap_or(rest);
    let candidate = format!("{scheme}://{no_fragment}");
    if no_fragment.split(['/', '?']).next().is_none_or(str::is_empty)
        // Backslashes can change authority/path parsing, but query backslashes are data:
        // escaped credential names still need the bounded query decoder below.
        || candidate.split('?').next().is_some_and(|head| head.contains('\\'))
        || candidate.chars().any(char::is_control)
        || url::Url::parse(&format!("{scheme}://{}", template_authority(no_fragment)))
            .ok()
            .is_none_or(|u| u.host_str().is_none() || !u.username().is_empty() || u.password().is_some())
    {
        return Public::Withheld;
    }
    let names = QueryNames::of(query(&candidate));
    let removed = query(&candidate).split('&').filter(|p| !p.is_empty() && pair_class(p, &names) != NameClass::Plain).count();
    let removed =
        u16::try_from(removed).unwrap_or(u16::MAX).saturating_add(u16::from(at.is_some())).saturating_add(u16::from(rest.contains('#')));
    let mut public = candidate;
    redact_query(&mut public);
    if public == text { Public::Unchanged } else { Public::Changed { address: public, removed } }
}

/// A bare supported scheme is a prose example with no authority or credential to remove.
pub fn scheme_example(text: &str) -> bool {
    ["http://", "https://", "s3://"].iter().any(|scheme| text.eq_ignore_ascii_case(scheme))
}

/// Cleans addresses in generated text and counts changed addresses. All spacing and all other
/// text stay byte-for-byte. Never apply this to user-authored text or dataset values.
pub fn scrub_addresses(text: &str) -> (Cow<'_, str>, u32) {
    let mut changed = 0u32;
    let mut out = String::new();
    let mut start = 0;
    for word in text.split_inclusive(char::is_whitespace) {
        let unspaced = word.trim_end_matches(char::is_whitespace);
        // Recognize supported prefixes before parsing: malformed delimiters must not turn a
        // recognized address into retained text. The preceding scheme characters establish the
        // same word boundary for a bare address, a quoted one and an address inside a wrapper.
        let mut schemes = unspaced
            .match_indices(':')
            .filter_map(|(at, _)| {
                let begin = unspaced
                    .get(..at)?
                    .char_indices()
                    .rev()
                    .find(|(_, c)| !c.is_ascii_alphanumeric() && !"+.-".contains(*c))
                    .map_or(0, |(i, c)| i + c.len_utf8());
                let scheme = unspaced.get(begin..at)?;
                ["http", "https", "s3"].iter().any(|s| scheme.eq_ignore_ascii_case(s)).then_some((begin, at))
            })
            .peekable();
        let mut copied = 0;
        while let Some((begin, scheme_at)) = schemes.next() {
            let boundary = schemes.peek().map_or(unspaced.len(), |(begin, _)| *begin);
            let end = unspaced
                .get(..boundary)
                .unwrap_or("")
                .trim_end_matches([':', ',', ';', '.', ')', ']', '>', '"', '\'', '»', '›', '”', '’'])
                .len()
                .max(scheme_at + 1);
            let address = word.get(begin..end).unwrap_or("");
            // A bare scheme in diagnostic prose has no address or credentials to remove.
            if scheme_example(address) {
                continue;
            }
            let public = public_web_address(address);
            if matches!(public, Public::Unchanged) {
                continue;
            }
            if changed == 0 {
                out.push_str(text.get(..start).unwrap_or(""));
            }
            changed = changed.saturating_add(1);
            out.push_str(word.get(copied..begin).unwrap_or(""));
            match public {
                Public::Unchanged => out.push_str(address),
                Public::Changed { address, .. } => out.push_str(&address),
                Public::Withheld => {
                    out.push_str(address.split(':').next().unwrap_or("https"));
                    out.push(':');
                }
            }
            copied = end;
        }
        if changed > 0 {
            out.push_str(word.get(copied..).unwrap_or(""));
        }
        start += word.len();
    }
    (if changed == 0 { Cow::Borrowed(text) } else { Cow::Owned(out) }, changed)
}

/// Mechanical path containment for the exact address fetched. Containment alone grants no
/// authority: advertised destinations still use F3 unless a deployment was explicitly reviewed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope {
    scheme: String,
    host: String,
    port: u16,
    segments: Vec<String>,
}

impl Scope {
    /// Reads authority with the fetch parser, and path segments from the original bytes.
    pub fn of(address: &str) -> Option<Self> {
        if address.trim() != address || address.chars().any(char::is_control) || address.contains('\\') {
            return None;
        }
        let parsed = url::Url::parse(address).ok()?;
        if !matches!(parsed.scheme(), "http" | "https") || !parsed.username().is_empty() || parsed.password().is_some() {
            return None;
        }
        let (_, rest) = address.split_once("://")?;
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        if end == 0 || rest.get(..end)?.contains('@') {
            return None;
        }
        let path = rest.get(end..)?.split(['?', '#']).next()?;
        let path = path.strip_prefix('/').unwrap_or(path);
        let path = path.strip_suffix('/').unwrap_or(path);
        let mut segments = Vec::new();
        if !path.is_empty() {
            for segment in path.split('/') {
                let lower = segment.to_ascii_lowercase();
                if segment.is_empty() || matches!(segment, "." | "..") || ["%2e", "%2f", "%5c", "%25"].iter().any(|s| lower.contains(s)) {
                    return None;
                }
                segments.push(segment.to_string());
            }
        } else if rest.get(end..)?.starts_with("//") {
            return None;
        }
        Some(Self {
            scheme: parsed.scheme().to_string(),
            host: parsed.host_str()?.trim_end_matches('.').to_ascii_lowercase(),
            port: parsed.port_or_known_default()?,
            segments,
        })
    }

    /// Whole raw segments must match; ambiguous paths and user-info are outside every scope.
    pub fn covers_below(&self, to: &str) -> bool {
        Self::of(to).is_some_and(|other| {
            self.scheme == other.scheme && self.host == other.host && self.port == other.port && other.segments.starts_with(&self.segments)
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Relation {
    Constructed,
    Advertised,
}

pub const SAFE_OPTIONS: &[&str] = &["collections", "datetime", "subset", "subset-crs", "crs", "bgcolor", "transparent"];

/// Only a reviewed deployment may authorize full-query inheritance for advertised links. Root
/// entries require separate whole-origin agreement, including every tenant on that origin.
pub struct ReviewedScope {
    pub directory: &'static str,
    pub whole_origin: bool,
}
/// No deployment has been authorized for full-query inheritance. Keep this list empty until
/// that review exists; the explicit-list tests exercise the same deny boundary.
pub const F2_SCOPES: &[ReviewedScope] = &[];

#[derive(Debug, PartialEq, Eq)]
pub struct Inherited {
    pub url: String,
    pub withheld: bool,
}

/// Adds missing pairs, preserving their bytes. Unresolved pairs never follow; F3 sends only
/// SAFE_OPTIONS to advertised addresses. Only checked constructed sub-addresses get other pairs.
pub fn inherit_query(from: &str, to: &str, relation: Relation, skip: &[&str]) -> Inherited {
    inherit_reviewed(from, to, relation, skip, F2_SCOPES)
}

/// Applies the fixed policy with an explicit reviewed list; production passes the empty list.
fn inherit_reviewed(from: &str, to: &str, relation: Relation, skip: &[&str], reviewed: &[ReviewedScope]) -> Inherited {
    let full = match relation {
        Relation::Constructed => Scope::of(from).is_some_and(|s| s.covers_below(to)),
        Relation::Advertised => reviewed.iter().any(|r| {
            Scope::of(r.directory).is_some_and(|s| (!s.segments.is_empty() || r.whole_origin) && s.covers_below(from) && s.covers_below(to))
        }),
    };
    let q = query(from);
    let names = QueryNames::of(q);
    let mut out = Inherited { url: to.to_string(), withheld: false };
    let mut present: Vec<String> = query(to).split(['&', ';']).filter_map(|p| option_name(pair_name(p))).collect();
    for pair in q.split('&').filter(|p| !p.is_empty()) {
        let class = pair_class(pair, &names);
        if class == NameClass::Unresolved {
            out.withheld = true;
            continue;
        }
        let keys: Vec<String> = pair.split(';').filter_map(|p| option_name(pair_name(p))).collect();
        let skipped = |k: &String| skip.iter().any(|s| option_name(s).as_ref() == Some(k));
        if keys.iter().any(skipped) {
            out.withheld |= keys.iter().any(|k| !skipped(k));
            continue;
        }
        if keys.iter().any(|k| present.contains(k)) {
            // Keep destination and earlier pairs unchanged. Only an identical raw pair is
            // already represented; other skipped values must be reported.
            out.withheld |= !query(&out.url).split('&').any(|kept| kept == pair);
            continue;
        }
        let safe = class == NameClass::Plain && keys.iter().all(|k| SAFE_OPTIONS.contains(&k.as_str()));
        if pair.contains(['{', '}']) || !(full || safe) {
            out.withheld = true;
            continue;
        }
        let fragment = out.url.find('#').unwrap_or(out.url.len());
        let separator = if out.url.get(..fragment).is_some_and(|u| u.contains('?')) { '&' } else { '?' };
        out.url.insert(fragment, separator);
        out.url.insert_str(fragment + 1, pair);
        present.extend(keys);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_names_remove_encoded_and_unresolved_pairs_without_touching_values() {
        let over = "a".repeat(MAX_NAME_BYTES + 1);
        let mut unredacted = Vec::new();
        for key in [
            "%74oken",
            "%2574oken",
            "to%6Ben",
            "pass%77ord",
            "+token",
            "token%3DM",
            "%FFtoken",
            "ok=1;token",
            "X-Amz-%53ignature",
            "X-Amz-%53ecurity-Token",
            "X-Amz-Credential",
            "X-Amzn-RequestId-Token",
            "X-Amz-Cf-Id-Secret",
            "sig=M&%73v",
            "\\u0074oken",
            "accessKeyId",
            "appid",
            "APP_ID",
            "%2561ppid",
            "subscription-key",
            "Subscription_Key",
            "%73ubscription%2Dkey",
            "client-key",
            "clientKey",
            "sas-key",
            "SAS_Key",
            "%2525252574oken",
            over.as_str(),
        ] {
            let address = format!("https://h.invalid/a?{key}=RGSYNTH_name_20261103&layer=a%20b&f=json&%zz=1");
            let Public::Changed { address: public, removed, .. } = public_web_address(&address) else {
                unredacted.push(key.to_string());
                continue;
            };
            assert!(!public.contains("RGSYNTH_name_20261103"), "{key}: {public}");
            assert!(public.ends_with("layer=a%20b&f=json&%zz=1"), "{public}");
            assert!(removed > 0);
        }
        assert!(unredacted.is_empty(), "credential names left unchanged: {unredacted:?}");
        for query in [
            "client_id=public&application_id=public&map=1",
            "layer=a%20b&f=json&%zz=1",
            "layer=token%3DM",
            "code=EPSG:4326",
            "sv=2026&layer=1",
            r"layer=a\b&f=json",
            "",
            "&",
            "layer=1&&f=json",
            "layer=1&",
        ] {
            let address = format!("https://h.invalid/a?{query}");
            assert_eq!(public_web_address(&address), Public::Unchanged);
        }
        for name in ["X-Amzn-RequestId", "X-Amzn-Waf-Action", "X-Amz-Cf-Id", "X-%2541mzn-RequestId"] {
            assert_eq!(name_class(name, &QueryNames::default()), NameClass::Plain, "{name}");
            assert!(!log_sensitive(name), "{name}");
        }
        for address in [r"https://h.invalid\a?token=RGSYNTH_boundary", r"https://h.invalid/a\b?token=RGSYNTH_boundary"] {
            assert_eq!(public_web_address(address), Public::Withheld);
        }
        for template in [
            "https://{switch:a,b}.host/{z}/{x}/{y}@2x.png?map=m",
            "https://{s}.host/{zoom}/{x}/{y}@2x.png",
            " \thttps://h.invalid/a?f=json \n",
        ] {
            assert_eq!(public_web_address(template), Public::Unchanged);
        }
        assert_eq!(
            public_web_address(
                "https://u:RGSYNTH_template_20261104@{switch:a,b}.host/{z}/{x}/{y}@2x.png?token=RGSYNTH_query_20261104&map=m#fragment"
            ),
            Public::Changed { address: "https://{switch:a,b}.host/{z}/{x}/{y}@2x.png?map=m".into(), removed: 3 }
        );
        assert_eq!(name_class("%2525252574oken", &QueryNames::default()), NameClass::Unresolved);
        assert_eq!(name_class(&over, &QueryNames::default()), NameClass::Unresolved);
        let mut callback = "https://h.invalid/cb?code=RGSYNTH_code_20261103&%73tate=flow".to_string();
        redact_query(&mut callback);
        assert_eq!(callback, "https://h.invalid/cb?%73tate=flow");
        let (clean, count) = scrub_addresses(
            "failed https://u:RGSYNTH_user_20261103@h.invalid/a?%74oken=RGSYNTH_query_20261103 then https://h.invalid/b?layer=a%20b\n",
        );
        assert_eq!(clean, "failed https://h.invalid/a then https://h.invalid/b?layer=a%20b\n");
        assert_eq!(count, 1);
        assert_eq!(scrub_addresses("(https://h.invalid/a?token=RGSYNTH_punctuation_20261103), done").0, "(https://h.invalid/a), done");
        for (text, public, count) in [
            ("read https:/h.invalid/x?token=RGSYNTH_0", "read https:", 1),
            ("\t(HTTP:/h.invalid/x?token=RGSYNTH_0),  done\n", "\t(HTTP:),  done\n", 1),
            ("[s3:bucket/x?token=RGSYNTH_0]", "[s3:]", 1),
            ("https:x", "https:", 1),
            ("s3:", "s3:", 1),
            ("plain http://, confirm before sending", "plain http://, confirm before sending", 0),
            ("use https:// for encryption", "use https:// for encryption", 0),
            ("an S3:// address", "an S3:// address", 0),
            ("read https:/h.invalid/x then https://h.invalid/b?layer=a%20b", "read https: then https://h.invalid/b?layer=a%20b", 1),
            (
                "http://h.invalid/a https://h.invalid/b?layer=a%20b s3://bucket/x",
                "http://h.invalid/a https://h.invalid/b?layer=a%20b s3://bucket/x",
                0,
            ),
            ("myhttps:/h.invalid/x?token=RGSYNTH_unrelated_20261104", "myhttps:/h.invalid/x?token=RGSYNTH_unrelated_20261104", 0),
            ("dataset token=RGSYNTH_data_20261104 key password", "dataset token=RGSYNTH_data_20261104 key password", 0),
        ] {
            let (scrubbed, changed) = scrub_addresses(text);
            assert_eq!((scrubbed.as_ref(), changed), (public, count), "{text}");
            if count == 0 {
                assert!(matches!(scrubbed, Cow::Borrowed(_)));
            }
        }
        assert!(matches!(scrub_addresses("dataset token key password").0, Cow::Borrowed(_)));
        assert_eq!(public_web_address("/data/token?key=password"), Public::Unchanged);
        assert_eq!(public_web_address("https:///path"), Public::Withheld);
        assert_eq!(public_web_address("https://u:RGSYNTH_malformed_20261103/path"), Public::Withheld);
        assert_eq!(
            public_web_address("https://u:RGSYNTH_malformed_20261103/part@h.invalid/a"),
            Public::Changed { address: "https://h.invalid/a".into(), removed: 1 }
        );
    }

    #[test]
    fn generated_text_scrubs_every_adjacent_address() {
        for (text, expected, count) in [
            (
                "read https://a.invalid/a?layer=roads,https://b.invalid/b?token=demo",
                "read https://a.invalid/a?layer=roads,https://b.invalid/b",
                1,
            ),
            ("([http://a.invalid/a,https://u:demo@b.invalid/b]),", "([http://a.invalid/a,https://b.invalid/b]),", 1),
            ("https://a.invalid/a?token=demo;S3://bucket/b?password=demo", "https://a.invalid/a;S3://bucket/b", 2),
            (
                "\t(https://a.invalid/a?layer=roads,https://b.invalid/b?layer=plots),\n",
                "\t(https://a.invalid/a?layer=roads,https://b.invalid/b?layer=plots),\n",
                0,
            ),
        ] {
            let (public, changed) = scrub_addresses(text);
            assert_eq!((public.as_ref(), changed), (expected, count), "{text}");
            if count == 0 {
                assert!(matches!(public, Cow::Borrowed(_)), "clean adjacent addresses need no rewrite");
            }
        }
    }

    #[test]
    fn inherited_queries_require_the_reviewed_relationship_and_unambiguous_path() {
        let source = "https://h.invalid/svc?token=RGSYNTH_inherit_20261103&k=RGSYNTH_unknown_20261103&collections=roads&f=json";
        let target = "https://h.invalid/svc/tiles/{z}/{y}/{x}";
        let kept = inherit_query(source, target, Relation::Constructed, &["f"]);
        assert!(!kept.withheld);
        assert!(kept.url.ends_with("?token=RGSYNTH_inherit_20261103&k=RGSYNTH_unknown_20261103&collections=roads"));
        for destination in [
            target,
            "https://other.invalid/svc/tiles",
            "http://h.invalid/svc/tiles",
            "https://h.invalid:444/svc/tiles",
            "https://u@h.invalid/svc/tiles",
            "https://h.invalid/svc-other/tiles",
        ] {
            let f3 = inherit_query(source, destination, Relation::Advertised, &["f"]);
            assert_eq!(f3.url, format!("{destination}?collections=roads"));
            assert!(f3.withheld);
            if destination != target {
                let constructed = inherit_query(source, destination, Relation::Constructed, &["f"]);
                assert_eq!(constructed, f3);
            }
        }
        for suffix in
            ["../tiles", "./tiles", "%2e%2e/tiles", "%2Ftiles", "%5Ctiles", "\\tiles", "%252e/tiles", "/tiles", "tiles//x", "tiles/\nnext"]
        {
            let to = format!("https://h.invalid/svc/{suffix}");
            let inherited = inherit_query(source, &to, Relation::Constructed, &["f"]);
            assert!(inherited.withheld && !inherited.url.contains("RGSYNTH_"), "{to}");
        }
        for name in ["%2525252574oken".to_string(), "a".repeat(MAX_NAME_BYTES + 1)] {
            let source = format!("https://h.invalid/svc?{name}=RGSYNTH_unresolved_20261103");
            for relation in [Relation::Constructed, Relation::Advertised] {
                assert_eq!(inherit_query(&source, target, relation, &[]), Inherited { url: target.into(), withheld: true });
            }
        }
        let root = "https://h.invalid/?token=RGSYNTH_root_20261103&collections=x";
        assert_eq!(inherit_query(root, target, Relation::Advertised, &[]).url, format!("{target}?collections=x"));
        let reviewed = [ReviewedScope { directory: "https://h.invalid/svc/", whole_origin: false }];
        assert_eq!(inherit_reviewed(source, target, Relation::Advertised, &["f"], &reviewed), kept);
        assert!(inherit_reviewed(source, "https://h.invalid/svc-other/a", Relation::Advertised, &["f"], &reviewed).withheld);
        let unreviewed_root = [ReviewedScope { directory: "https://h.invalid/", whole_origin: false }];
        assert!(inherit_reviewed(source, target, Relation::Advertised, &["f"], &unreviewed_root).withheld);
        assert!(F2_SCOPES.is_empty());
        let to = format!("{target}?%74oken=destination-owned#look");
        let own = inherit_query(source, &to, Relation::Constructed, &["f"]);
        assert!(own.url.contains("%74oken=destination-owned") && !own.url.contains("RGSYNTH_inherit"));
        assert!(own.url.ends_with("#look"));
        assert!(!inherit_query("https://h.invalid/svc?collections=x", target, Relation::Advertised, &[]).withheld);
        for key in ["c_r_s", "%FFcollections", "+collections", "f_"] {
            let source = format!("https://h.invalid/svc?{key}=RGSYNTH_alias_20261103");
            let inherited = inherit_query(&source, target, Relation::Advertised, &["f"]);
            assert_eq!(inherited, Inherited { url: target.into(), withheld: true });
        }
        assert_eq!(
            inherit_query("https://h.invalid/svc?%63ollections=x&subset-crs=y", target, Relation::Advertised, &[]).url,
            format!("{target}?%63ollections=x&subset-crs=y")
        );
        let duplicate = inherit_query(
            "https://h.invalid/svc?token=source;collections=x",
            &format!("{target}?token=destination"),
            Relation::Constructed,
            &[],
        );
        assert_eq!(duplicate.url, format!("{target}?token=destination"));
        assert!(duplicate.withheld);
        // A different value of an existing option is withheld too, including an earlier
        // source option, encoded names and indivisible legacy pairs. Exact duplicates are quiet.
        for relation in [Relation::Constructed, Relation::Advertised] {
            for (query, destination, expected, withheld) in [
                ("subset=Lat(10,20)&subset=Long(30,40)", "", "?subset=Lat(10,20)", true),
                ("%63ollections=roads", "?collections=z", "?collections=z", true),
                ("collections=roads;datetime=2020", "?collections=z&datetime=2021", "?collections=z&datetime=2021", true),
                ("collections=roads&collections=roads", "", "?collections=roads", false),
                ("collections=roads", "?collections=roads", "?collections=roads", false),
                ("collections=roads;datetime=2020", "?collections=roads;datetime=2020", "?collections=roads;datetime=2020", false),
            ] {
                let inherited = inherit_query(&format!("https://h.invalid/svc?{query}"), &format!("{target}{destination}"), relation, &[]);
                assert_eq!(inherited, Inherited { url: format!("{target}{expected}"), withheld }, "{relation:?}: {query}");
            }
        }
    }
}
