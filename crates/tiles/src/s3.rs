//! Amazon S3 and S3-compatible storage (MinIO, Cloudflare R2, Wasabi, source.coop): `s3://`
//! addresses, connections with their credentials, AWS Signature Version 4, and bucket listings.
//!
//! An `s3://bucket/key` address is fetched from the endpoint of the connection that serves its
//! bucket (AWS when none does), and requests to a connection's own endpoint are signed with its
//! credentials. Credentials stay in memory for the session: they are never in a project, a
//! command's result, a notice or the log, and they are only ever sent to the endpoint they are
//! for (a signed request that is redirected is not followed). The fetchers ([`crate::cache`] and
//! the web app's) call [`prepare`] for every web request.

use std::{borrow::Cow, sync::Mutex};

use sha2::{Digest, Sha256};

/// The region used when none is given.
pub const DEFAULT_REGION: &str = "us-east-1";
/// How many connections a session holds.
const MAX_CONNECTIONS: usize = 32;
/// The payload hash of a request whose body isn't signed (S3 GETs).
const UNSIGNED: &str = "UNSIGNED-PAYLOAD";

/// An access key and its secret (and a session token for temporary credentials).
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
}

// Never printed: a log line or an error built with `{:?}` shows no secret.
impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Credentials(…)")
    }
}

/// A named way to reach buckets: an endpoint (AWS when `None`), a region, the buckets it serves
/// (every bucket when empty) and its credentials (anonymous when `None`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Connection {
    pub name: String,
    /// `https://host[:port]` (or `http://` for a server on this computer or network), no path.
    pub endpoint: Option<String>,
    pub region: String,
    pub buckets: Vec<String>,
    pub credentials: Option<Credentials>,
}

impl Connection {
    /// What a command may show of it: never the credentials.
    pub fn public(&self) -> serde_json::Value {
        serde_json::json!({
            "name": self.name,
            "endpoint": self.endpoint,
            "region": self.region,
            "buckets": self.buckets,
            "signed": self.credentials.is_some(),
        })
    }
}

/// The live connections, and which of them a saved connection's login made (see [`connect_saved`]).
/// Under one lock so a worker never sees one without the other.
#[derive(Default)]
struct Registry {
    connections: Vec<Connection>,
    saved: Vec<Saved>,
}

/// What a saved connection's login in one session is: the session's runtime token for it, the
/// saved connection's id, the live connection's name, and the buckets the user explicitly
/// granted it by browsing or choosing it. Never a second copy of the credentials.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Saved {
    binding: String,
    id: String,
    name: String,
    grants: Vec<String>,
}

impl Registry {
    /// Whether the live connection of this name came from a saved connection's login: those
    /// sign only the addresses that name their binding, never an address found by its bucket
    /// or its host.
    fn is_saved(&self, name: &str) -> bool {
        self.saved.iter().any(|s| s.name == name)
    }

    /// The connections that sign (or serve) addresses found by their bucket or their host.
    fn implicit(&self) -> impl Iterator<Item = &Connection> {
        self.connections.iter().filter(|c| !self.is_saved(&c.name))
    }

    fn drop_name(&mut self, name: &str) {
        self.connections.retain(|c| c.name != name);
        self.saved.retain(|s| s.name != name);
    }
}

static REGISTRY: std::sync::LazyLock<Mutex<Registry>> = std::sync::LazyLock::new(Default::default);

fn lock() -> std::sync::MutexGuard<'static, Registry> {
    REGISTRY.lock().unwrap_or_else(|e| e.into_inner())
}

/// Reads selection state without recovering a poisoned store as fresh authority.
fn read_registry() -> Result<std::sync::MutexGuard<'static, Registry>, String> {
    REGISTRY.lock().map_err(|_| "active S3 connection store unavailable".to_string())
}

/// A connection as the registry keeps it: name, endpoint, region, buckets and credentials checked.
fn checked(mut c: Connection) -> Result<Connection, String> {
    c.name = c.name.trim().to_string();
    if c.name.is_empty() || c.name.chars().count() > 64 || c.name.chars().any(char::is_control) {
        return Err("a connection needs a name of up to 64 characters".into());
    }
    c.endpoint = match c.endpoint.as_deref().map(str::trim).filter(|e| !e.is_empty()) {
        Some(e) => Some(endpoint(e)?),
        None => None,
    };
    c.region = c.region.trim().to_string();
    if c.region.is_empty() {
        c.region = DEFAULT_REGION.into();
    }
    if c.region.len() > 32 || !c.region.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return Err(format!("'{}' isn't a region name", c.region));
    }
    c.buckets.retain(|b| !b.trim().is_empty());
    for b in &mut c.buckets {
        *b = b.trim().to_string();
        if !valid_bucket(b) {
            return Err(format!("'{b}' isn't a bucket name"));
        }
    }
    c.buckets.truncate(64);
    if let Some(cr) = &c.credentials {
        check_credentials(cr)?;
    }
    Ok(c)
}

/// Whether an access key, its secret and its session token (if any) are of a form S3 takes.
pub fn check_credentials(cr: &Credentials) -> Result<(), String> {
    let ok = |s: &str, max: usize| !s.is_empty() && s.len() <= max && !s.chars().any(|ch| ch.is_control() || ch.is_whitespace());
    if !ok(&cr.access_key_id, 128) || !ok(&cr.secret_access_key, 256) || cr.session_token.as_deref().is_some_and(|t| !ok(t, 4096)) {
        return Err("the access key, secret or session token isn't well formed".into());
    }
    Ok(())
}

/// Adds (or replaces, by name) a connection for the session, after checking it.
pub fn connect(c: Connection) -> Result<(), String> {
    let c = checked(c)?;
    let mut reg = lock();
    // The name stays free if it can't be filled: only a connection that fits replaces the old one.
    let replaced = reg.connections.iter().filter(|o| o.name == c.name).count();
    if reg.connections.len() - replaced >= MAX_CONNECTIONS {
        return Err(format!("up to {MAX_CONNECTIONS} connections"));
    }
    // A saved connection's login replaced by an ordinary one of its name is no longer a saved
    // login: addresses bound to it find no connection until it logs in again.
    reg.drop_name(&c.name);
    // A connection replaced by name starts afresh: the buckets granted to the old one (browsed
    // through another endpoint or with other keys) aren't the new one's.
    granted().retain(|(n, _)| n != &c.name);
    reg.connections.push(c);
    Ok(())
}

/// A runtime token for a saved connection's login in one session: 16 lower-case hexadecimal
/// digits, never zero, never given twice in this process (two sessions' saved ids are often the
/// same, `c1`; their logins mustn't be).
pub fn new_binding() -> Result<String, String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    // A compare-and-swap loop, not `fetch_update`/`try_update`: the old name is deprecated on the
    // current stable and the new one does not exist on the oldest toolchain this crate builds on.
    let mut current = NEXT.load(Ordering::SeqCst);
    loop {
        let next = current.checked_add(1).ok_or_else(|| "S3 connection tokens are used up".to_string())?;
        match NEXT.compare_exchange_weak(current, next, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return Ok(format!("{next:016x}")),
            Err(seen) => current = seen,
        }
    }
}

/// Makes the login of the saved connection `id` live for the session under `binding`: checked as
/// [`connect`] does, replacing a live connection of that name, and registered as bound in the
/// same step, so no worker sees it eligible for implicit signing in between. The buckets
/// granted to this binding are kept for a login of the same connection at the same endpoint, and
/// forgotten otherwise.
pub fn connect_saved(c: Connection, binding: &str, id: &str) -> Result<(), String> {
    if binding.is_empty() || binding == rustgis_data::s3_address::LEGACY || !rustgis_data::s3_address::valid_binding(binding) {
        return Err("not a saved S3 connection token".into());
    }
    if id.is_empty() || id.len() > 64 {
        return Err("a saved connection needs an id".into());
    }
    let c = checked(c)?;
    let mut reg = lock();
    let replaced = reg.connections.iter().filter(|o| o.name == c.name).count();
    if reg.connections.len() - replaced >= MAX_CONNECTIONS {
        return Err(format!("up to {MAX_CONNECTIONS} connections"));
    }
    // What this binding was granted, when it is the same login coming back (a login again, or a
    // new set of keys): nothing when the endpoint it was for is not this one.
    let kept = reg
        .saved
        .iter()
        .find(|s| s.binding == binding && s.id == id)
        .filter(|s| reg.connections.iter().any(|o| o.name == s.name && o.endpoint == c.endpoint))
        .map(|s| s.grants.clone())
        .unwrap_or_default();
    reg.drop_name(&c.name);
    // This binding's earlier live connection, under another name (a renamed saved connection).
    let moved: Vec<String> = reg.saved.iter().filter(|s| s.binding == binding).map(|s| s.name.clone()).collect();
    for name in moved {
        reg.drop_name(&name);
        granted().retain(|(n, _)| *n != name);
    }
    granted().retain(|(n, _)| n != &c.name);
    reg.saved.push(Saved { binding: binding.to_string(), id: id.to_string(), name: c.name.clone(), grants: kept });
    reg.connections.push(c);
    Ok(())
}

/// How many buckets may be granted to one saved connection's login.
const MAX_SAVED_GRANTS: usize = 256;

/// Lets the saved login `binding` serve a bucket for the rest of the session: the user browsed
/// or chose it through that connection. Unlike [`grant`], other bindings keep theirs (two sources
/// bound to different connections may read the same bucket).
pub fn grant_saved(binding: &str, bucket: &str) -> Result<(), String> {
    if !valid_bucket(bucket) {
        return Err(format!("'{bucket}' isn't a bucket name"));
    }
    let mut reg = lock();
    let s = reg.saved.iter_mut().find(|s| !binding.is_empty() && s.binding == binding).ok_or(UNRESOLVED)?;
    if !s.grants.iter().any(|b| b == bucket) {
        if s.grants.len() >= MAX_SAVED_GRANTS {
            return Err("too many buckets for one connection".into());
        }
        s.grants.push(bucket.to_string());
    }
    Ok(())
}

/// Forgets a connection (and its credentials, and the buckets granted to it); whether there was
/// one by that name.
pub fn disconnect(name: &str) -> bool {
    let mut reg = lock();
    let before = reg.connections.len();
    reg.drop_name(name);
    granted().retain(|(n, _)| n != name);
    reg.connections.len() != before
}

/// Shares the existing bucket grants between requests and pure identity snapshots.
fn grant_store() -> &'static Mutex<Vec<(String, String)>> {
    static G: std::sync::OnceLock<Mutex<Vec<(String, String)>>> = std::sync::OnceLock::new();
    G.get_or_init(Default::default)
}

fn granted() -> std::sync::MutexGuard<'static, Vec<(String, String)>> {
    grant_store().lock().unwrap_or_else(|e| e.into_inner())
}

/// Captures grants with the same fail-closed lock behavior as account selection.
fn read_grants() -> Result<std::sync::MutexGuard<'static, Vec<(String, String)>>, String> {
    grant_store().lock().map_err(|_| "active S3 bucket grants unavailable".to_string())
}

/// Lets a connection serve a bucket for the rest of the session: the user browsed or added it
/// through that connection.
pub fn grant(name: &str, bucket: &str) {
    if !valid_bucket(bucket) || connection(name).is_none() {
        return;
    }
    let mut g = granted();
    g.retain(|(_, b)| b != bucket);
    if g.len() < 4096 {
        g.push((name.to_string(), bucket.to_string()));
    }
}

/// Whether a connection serves a bucket: it was given the bucket, or the bucket was granted to it.
fn serves(c: &Connection, bucket: &str, grants: &[(String, String)]) -> bool {
    c.buckets.iter().any(|b| b == bucket) || grants.iter().any(|(n, b)| *n == c.name && b == bucket)
}

/// Selects the first implicit account serving this bucket under the captured grants.
fn bucket_connection(reg: &Registry, grants: &[(String, String)], bucket: &str) -> Option<Connection> {
    reg.implicit().find(|c| serves(c, bucket, grants)).cloned()
}

/// The session's connections.
pub fn connections() -> Vec<Connection> {
    lock().connections.clone()
}

/// A connection by name.
pub fn connection(name: &str) -> Option<Connection> {
    lock().connections.iter().find(|c| c.name == name).cloned()
}

/// The live connection of the saved login `binding`, and the saved id it is for.
pub fn saved_connection(binding: &str) -> Option<(Connection, String)> {
    let reg = lock();
    let s = reg.saved.iter().find(|s| !binding.is_empty() && s.binding == binding)?;
    let c = reg.connections.iter().find(|c| c.name == s.name)?;
    Some((c.clone(), s.id.clone()))
}

/// Whether a saved definition may reuse a live login. Mapped logins require the exact session
/// binding and saved ID. Only callers without a token may reuse an ordinary name-based login.
/// Checks the atomic registry without cloning credentials.
pub fn can_reuse_login(binding: Option<&str>, id: &str, name: &str) -> bool {
    let reg = lock();
    match reg.saved.iter().find(|s| s.name == name) {
        Some(saved) => binding == Some(saved.binding.as_str()) && saved.id == id,
        None => binding.is_none() && reg.connections.iter().any(|c| c.name == name),
    }
}

/// The runtime token of the saved login that is the live connection `name`, if it is one.
pub fn binding_of(name: &str) -> Option<String> {
    lock().saved.iter().find(|s| s.name == name).map(|s| s.binding.clone())
}

/// What may be said to someone whose address names no live saved connection to read it with.
pub const UNRESOLVED: &str = "Choose a saved S3 connection before opening this source.";
/// What may be said when the saved connection no longer fits the address that names it.
pub const MISMATCH: &str = "The saved S3 connection no longer matches this source.";

/// `url` read through the saved login `binding`, with that connection's own endpoint: the address
/// a fetch of it, and a project, keep. (A binding alone grants no bucket: [`grant_saved`], or the
/// connection's own buckets, do.)
pub fn bound_url(binding: &str, url: &str) -> Result<String, String> {
    let endpoint = {
        let reg = lock();
        let saved = reg.saved.iter().find(|s| !binding.is_empty() && s.binding == binding).ok_or(UNRESOLVED)?;
        reg.connections.iter().find(|c| c.name == saved.name).ok_or(UNRESOLVED)?.endpoint.clone()
    };
    rustgis_data::s3_address::bind(binding, endpoint.as_deref().unwrap_or(""), url).map_err(str::to_string)
}

/// An endpoint as connections keep it: a web address's scheme and authority, no path.
pub fn endpoint(e: &str) -> Result<String, String> {
    let e = e.trim().trim_end_matches('/');
    let lower = e.to_ascii_lowercase();
    if !(lower.starts_with("https://") || lower.starts_with("http://")) {
        return Err(format!("{e}: an endpoint is an https:// (or http://) address"));
    }
    let (scheme, rest) = e.split_once("://").unwrap_or(("https", e));
    if rest.is_empty() || rest.contains(['/', '?', '#', '@']) {
        return Err(format!("{e}: an endpoint is a scheme and a host (and port), such as https://s3.example.org"));
    }
    let e = origin(scheme, rest);
    crate::address::check(&e, None)?;
    Ok(e)
}

/// A scheme and authority as compared: lower-case, without the scheme's default port.
fn origin(scheme: &str, authority: &str) -> String {
    let (scheme, authority) = (scheme.to_ascii_lowercase(), authority.to_ascii_lowercase());
    let default = if scheme == "https" { ":443" } else { ":80" };
    let authority = authority.strip_suffix(default).unwrap_or(&authority);
    format!("{scheme}://{authority}")
}

/// Whether `b` is a bucket name S3 accepts (3–63 lower-case letters, digits, dots and hyphens).
pub fn valid_bucket(b: &str) -> bool {
    let bytes = b.as_bytes();
    (3..=63).contains(&bytes.len())
        && bytes.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'.' || *c == b'-')
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
}

/// Whether an address is an `s3://` one.
pub fn is_s3(url: &str) -> bool {
    url.get(..5).is_some_and(|s| s.eq_ignore_ascii_case("s3://"))
}

/// The bucket and key of an `s3://bucket/key` address.
pub fn parse(url: &str) -> Result<(String, String), String> {
    let rest =
        url.get(5..).filter(|_| is_s3(url)).ok_or_else(|| rustgis_data::diagnostics::message_with_address(url, "not an s3:// address"))?;
    let (bucket, key) = rest.split_once('/').unwrap_or((rest, ""));
    if !valid_bucket(bucket) {
        // The bucket text itself is not repeated: without a slash it may hold the query.
        return Err(rustgis_data::diagnostics::message_with_address(url, "the bucket name isn't one S3 takes"));
    }
    if key.len() > 1024 || key.chars().any(char::is_control) {
        return Err(rustgis_data::diagnostics::message_with_address(url, "the key isn't one S3 takes"));
    }
    Ok((bucket.to_string(), key.to_string()))
}

/// The connection serving a bucket: one given the bucket, or one it was granted to this session
/// ([`grant`]). None serves any other bucket: an address naming it (a project's, a link's) is read
/// anonymously from AWS, never with the user's keys or from the user's endpoint. A saved
/// connection's login serves no bucket this way: its addresses name it ([`connect_saved`]).
pub fn for_bucket(bucket: &str) -> Option<Connection> {
    let reg = lock();
    let grants = granted();
    bucket_connection(&reg, &grants, bucket)
}

/// Starts an address read anonymously whatever the connections: nothing signed, and an `s3://`
/// address read from AWS (the S3 dialog's "AWS, anonymous").
pub const ANONYMOUS: &str = "rustgis-anonymous:";

/// An address read anonymously ([`ANONYMOUS`]).
pub fn anonymous(url: &str) -> String {
    format!("{ANONYMOUS}{url}")
}

/// Splits off [`ANONYMOUS`]: the address, and whether it is read anonymously.
pub fn split_anonymous(url: &str) -> (&str, bool) {
    match url.strip_prefix(ANONYMOUS) {
        Some(u) => (u, true),
        None => (url, false),
    }
}

/// The web address of an object (or, with an empty key, of its bucket) through a connection:
/// path-style at a connection's own endpoint; virtual-hosted at AWS (path-style for a bucket
/// with dots, which doesn't fit a certificate). Without a connection, AWS's global endpoint,
/// which reaches a bucket in any region.
pub fn object_url(bucket: &str, key: &str, conn: Option<&Connection>) -> String {
    let key = encode(key, false);
    match (conn.and_then(|c| c.endpoint.as_deref()), conn) {
        (Some(ep), _) => format!("{ep}/{bucket}/{key}"),
        (None, Some(c)) if bucket.contains('.') => format!("https://s3.{}.amazonaws.com/{bucket}/{key}", c.region),
        (None, Some(c)) => format!("https://{bucket}.s3.{}.amazonaws.com/{key}", c.region),
        (None, None) if bucket.contains('.') => format!("https://s3.amazonaws.com/{bucket}/{key}"),
        (None, None) => format!("https://{bucket}.s3.amazonaws.com/{key}"),
    }
}

/// The web address an `s3://` address is fetched from. An address read through a saved
/// connection is the one that connection fetches it at.
pub fn https_url(url: &str) -> Result<String, String> {
    if rustgis_data::s3_address::is_wrapper(url) {
        return bound_target(url).map(|t| t.0);
    }
    let (url, anonymous) = split_anonymous(url);
    let (bucket, key) = parse(url)?;
    let conn = if anonymous { None } else { for_bucket(&bucket) };
    Ok(object_url(&bucket, &key, conn.as_ref()))
}

/// AWS's web address for an object read anonymously, as a public `s3://` address named by a
/// catalog is (a STAC item's asset): never through the user's connections, at the global endpoint,
/// or at `region`'s for a bucket in a region the global one doesn't reach.
pub fn public_url(url: &str, region: Option<&str>) -> Result<String, String> {
    let (bucket, key) = parse(url)?;
    let at = region.map(|r| Connection { name: String::new(), endpoint: None, region: r.into(), buckets: Vec::new(), credentials: None });
    Ok(object_url(&bucket, &key, at.as_ref()))
}

/// The region an S3 refusal says a bucket is in, when it was asked at the wrong endpoint: a bucket
/// in a region AWS opts in (af-south-1, Digital Earth Africa's) asked at the global endpoint
/// ("The af-south-1 location constraint is incompatible…"), or an endpoint named in a redirect
/// (`bucket.s3.eu-west-1.amazonaws.com`). Only a region's form is taken (a two-letter area, words
/// and a number: `us-east-1`, `ap-southeast-2`, `us-gov-west-1`), so a server's text steers the
/// address to no other host.
pub fn region_in(text: &str) -> Option<String> {
    let region = |r: &str| {
        let r = r.trim_matches(|c: char| !c.is_ascii_alphanumeric());
        let parts: Vec<&str> = r.split('-').collect();
        let (number, words) = parts.split_last()?;
        let word = |w: &&str| !w.is_empty() && w.bytes().all(|b| b.is_ascii_lowercase());
        (r.len() <= 30
            && words.len() >= 2
            && words.first().is_some_and(|a| a.len() == 2)
            && words.iter().all(word)
            && (1..=2).contains(&number.len())
            && number.bytes().all(|b| b.is_ascii_digit()))
        .then(|| r.to_string())
    };
    let constraint = text.split_once(" location constraint").and_then(|(before, _)| before.rsplit(' ').next()).and_then(region);
    constraint.or_else(|| text.split(".s3.").skip(1).find_map(|after| after.split_once(".amazonaws.com").and_then(|(r, _)| region(r))))
}

/// Where a public bucket's objects are read anonymously, found by reading one object's first byte
/// (`probe`; a read that got no answer, such as a timeout, is tried once more): `None` for AWS's
/// global endpoint, else the bucket's region, when AWS
/// answers that the bucket is in a region the global endpoint doesn't reach. A bucket that refuses
/// anonymous reads (a private or requester-pays one, such as Landsat's) is refused, naming it.
pub fn public_region(url: &str, probe: impl Fn(&str) -> Result<(), crate::failure::Failure>) -> Result<Option<String>, String> {
    let (bucket, _) = parse(url)?;
    let refused = |f: &crate::failure::Failure| match f.status() {
        Some(401 | 403) => format!(
            "s3://{bucket} doesn't let its files be read anonymously (a private or requester-pays bucket), so RustGIS can't read them"
        ),
        _ => format!("s3://{bucket}: {}", f.message),
    };
    // One timeout doesn't fail the cube's thousands of files.
    let probe = |u: &str| probe(u).or_else(|f| if f.status().is_none() && f.excerpt().is_none() { probe(u) } else { Err(f) });
    match probe(&public_url(url, None)?) {
        Ok(()) => Ok(None),
        Err(f) => {
            let Some(region) = f.excerpt().and_then(region_in) else { return Err(refused(&f)) };
            probe(&public_url(url, Some(&region))?).map_err(|f| refused(&f))?;
            Ok(Some(region))
        }
    }
}

/// The canonical endpoint a saved connection's address (empty: AWS) stands for.
pub fn canonical(address: &str) -> Result<Option<String>, String> {
    let a = address.trim();
    if a.is_empty() { Ok(None) } else { endpoint(a).map(Some) }
}

/// Where an address read through a saved connection is fetched, judged without any login: the
/// address's own endpoint decides how an `s3://` address maps to the web. `None` for an address
/// that isn't one read through a saved connection.
pub fn bound_destination(address: &str) -> Result<Option<String>, String> {
    let Some(b) = rustgis_data::s3_address::parse(address).map_err(str::to_string)? else { return Ok(None) };
    if !is_s3(b.url) {
        return Ok(Some(b.url.to_string()));
    }
    let (bucket, key) = parse(b.url)?;
    let at = Connection {
        name: String::new(),
        endpoint: canonical(b.endpoint)?,
        region: DEFAULT_REGION.into(),
        buckets: Vec::new(),
        credentials: None,
    };
    Ok(Some(object_url(&bucket, &key, Some(&at))))
}

/// What a saved login's address is fetched at, and by whom: the exact connection its binding
/// names (never another that merely has the same endpoint or bucket), only when the address's
/// endpoint is that connection's, and only for an address of a bucket the connection serves (its
/// own, or one explicitly granted to this binding), or of its account's bucket list.
fn bound_target(address: &str) -> Result<(String, Connection), String> {
    let b = rustgis_data::s3_address::parse(address).map_err(str::to_string)?.ok_or(UNRESOLVED)?;
    if b.binding.is_empty() {
        return Err(UNRESOLVED.into());
    }
    let wanted = canonical(b.endpoint).map_err(|_| MISMATCH.to_string())?;
    // Snapshot only the selected login and this request's explicit grant. No URL parsing,
    // origin validation or signing runs under the registry lock, and the grant list isn't copied.
    let snapshot = |bucket: Option<&str>| -> Result<(Connection, bool), String> {
        let reg = read_registry()?;
        let saved = reg.saved.iter().find(|s| s.binding == b.binding).ok_or(UNRESOLVED)?;
        let c = reg.connections.iter().find(|c| c.name == saved.name).ok_or(UNRESOLVED)?;
        let granted = bucket.is_some_and(|bucket| saved.grants.iter().any(|g| g == bucket));
        Ok((c.clone(), granted))
    };
    if is_s3(b.url) {
        let (bucket, key) = parse(b.url).map_err(|_| MISMATCH.to_string())?;
        let (c, granted) = snapshot(Some(&bucket))?;
        if c.endpoint != wanted || !(c.buckets.iter().any(|b| b == &bucket) || granted) {
            return Err(MISMATCH.into());
        }
        return Ok((object_url(&bucket, &key, Some(&c)), c));
    }
    // Only the origin is case-insensitive. Bucket paths retain their exact spelling.
    let (scheme, rest) = b.url.split_once("://").ok_or(MISMATCH)?;
    let scheme = if scheme.eq_ignore_ascii_case("https") { "https" } else { "http" };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        return Err(MISMATCH.into());
    }
    let here = origin(scheme, authority);
    let host = here.strip_prefix("https://").unwrap_or("").split(':').next().unwrap_or("");
    let path = rest.get(authority.len()..).unwrap_or("");
    let first = path.trim_start_matches('/').split(['/', '?', '#']).next().unwrap_or("");
    let bucket: Option<Cow<'_, str>> =
        if wanted.is_some() { (!first.is_empty()).then_some(Cow::Borrowed(first)) } else { aws_bucket(host, rest).map(Cow::Owned) };
    let origin_allowed = match wanted.as_deref() {
        Some(endpoint) => here == endpoint,
        None => aws_host(scheme, &here, host),
    };
    let root_path = if wanted.is_some() {
        path.split(['?', '#']).next().unwrap_or("").trim_start_matches('/').is_empty()
    } else {
        rest.split_once('/').map(|(_, p)| p).unwrap_or("").split(['?', '#']).next().unwrap_or("").is_empty()
    };
    let root_region = host.strip_prefix("s3.").and_then(|r| r.strip_suffix(".amazonaws.com"));
    let (c, granted) = snapshot(bucket.as_deref())?;
    let allowed = match bucket.as_deref() {
        Some(bucket) => c.buckets.iter().any(|b| b == bucket) || granted,
        None => root_path && (wanted.is_some() || root_region == Some(c.region.as_str())),
    };
    if c.endpoint != wanted || !origin_allowed || !allowed {
        return Err(MISMATCH.into());
    }
    Ok((b.url.to_string(), c))
}

/// A request ready to send: its web address, the headers to add, and whether it is signed (a
/// signed request is never redirected elsewhere).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prepared {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub signed: bool,
}

/// An exact in-memory S3 selection, never a display or persistence representation.
/// Known addresses retain only their selected account or its absence; unknown routes retain all state.
#[derive(Clone, PartialEq, Eq)]
pub struct AccountIdentity(Identity);

#[derive(Clone, PartialEq, Eq)]
enum Identity {
    All { connections: Vec<Connection>, saved: Vec<Saved>, grants: Vec<(String, String)> },
    Address { url: String, choice: Choice },
}

/// Captures all active selection state for a route whose eventual addresses are unknown.
/// No I/O or signing occurs; a poisoned registry or grant store is an error.
pub fn account_identity() -> Result<AccountIdentity, String> {
    let reg = read_registry()?;
    let grants = read_grants()?;
    Ok(AccountIdentity(Identity::All { connections: reg.connections.clone(), saved: reg.saved.clone(), grants: grants.clone() }))
}

/// Captures the account selected by `prepare`, including absence and explicit anonymous reads.
/// Pass the same address as `prepare`; service-login wrappers are removed by its caller.
/// No I/O or signing occurs; unavailable relevant state is an error, never an absent account.
pub fn account_identity_for(url: &str) -> Result<AccountIdentity, String> {
    Ok(AccountIdentity(Identity::Address { url: url.to_string(), choice: choice(url)? }))
}

impl AccountIdentity {
    /// Re-selects without I/O; poison, rotation, replacement or newly covering accounts refuse later work.
    pub fn is_current(&self) -> bool {
        let current = match &self.0 {
            Identity::All { .. } => account_identity(),
            Identity::Address { url, .. } => account_identity_for(url),
        };
        current.is_ok_and(|current| current == *self)
    }
}

/// The sender and identity capture share this unsigned destination/account selection.
#[derive(Clone, PartialEq, Eq)]
struct Choice {
    url: String,
    signer: Option<(Connection, String)>,
}

/// Resolves the destination and signer once, without clock-dependent signing or I/O.
fn choice(url: &str) -> Result<Choice, String> {
    if rustgis_data::s3_address::is_wrapper(url) {
        let (target, c) = bound_target(url)?;
        let region = match c.endpoint {
            Some(_) => c.region.clone(),
            None => aws_region(&target.to_ascii_lowercase(), &c.region),
        };
        return Ok(Choice { url: target, signer: Some((c, region)) });
    }
    let (url, anonymous) = split_anonymous(url);
    if anonymous && rustgis_data::s3_address::is_wrapper(url) {
        return Err("not a saved S3 address".into());
    }
    let (url, signer) = if is_s3(url) {
        let (bucket, key) = parse(url)?;
        let conn = if anonymous {
            None
        } else {
            let reg = read_registry()?;
            let grants = read_grants()?;
            bucket_connection(&reg, &grants, &bucket)
        };
        let region = conn.as_ref().map(|c| c.region.clone()).unwrap_or_default();
        (object_url(&bucket, &key, conn.as_ref()), conn.map(|c| (c, region)))
    } else {
        (url.to_string(), if anonymous { None } else { signer_for(url)? })
    };
    Ok(Choice { url, signer })
}

/// Makes a GET of `url` ready: an `s3://` address becomes its web address, and a request to a
/// connection's endpoint (or to AWS S3 for an AWS connection) is signed with its credentials.
/// An address read through a saved connection is signed by that connection alone
/// ([`bound_target`]), and never read without it. Other addresses pass unchanged.
pub fn prepare(url: &str) -> Result<Prepared, String> {
    let Choice { url, signer } = choice(url)?;
    let Some((c, region)) = signer else {
        return Ok(Prepared { url, headers: Vec::new(), signed: false });
    };
    let Some(cr) = c.credentials.as_ref() else {
        return Ok(Prepared { url, headers: Vec::new(), signed: false });
    };
    // Sent in the form it is signed in: the path's parts encoded as S3 encodes them.
    let url = canonical_url(&url)?;
    let headers = sign_get(&url, &region, cr, &amz_date(now_secs()))?;
    Ok(Prepared { url, headers, signed: true })
}

/// A web address with its path in S3's canonical form: each part decoded, then encoded.
fn canonical_url(url: &str) -> Result<String, String> {
    let (scheme, rest) = url.split_once("://").ok_or_else(|| rustgis_data::diagnostics::message_with_address(url, "not a web address"))?;
    let rest = rest.split('#').next().unwrap_or(rest);
    let end = rest.find(['/', '?']).unwrap_or(rest.len());
    let (authority, path_query) = rest.split_at(end);
    let (path, query) = match path_query.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (path_query, None),
    };
    let mut out = format!("{scheme}://{authority}{}", canonical_path(path));
    if let Some(q) = query {
        out.push('?');
        out.push_str(q);
    }
    Ok(out)
}

/// A path as S3 signs it: each part decoded (a `+` is a plus) and encoded again.
fn canonical_path(path: &str) -> String {
    if path.is_empty() {
        return "/".into();
    }
    path.split('/').map(|part| encode(&decode_path(part), true)).collect::<Vec<_>>().join("/")
}

/// Whether an address (its scheme, its origin and its host, all lower-case) is at AWS S3.
fn aws_host(scheme: &str, here: &str, host: &str) -> bool {
    scheme == "https"
        && here == format!("https://{host}")
        && host.ends_with(".amazonaws.com")
        && (host.starts_with("s3.") || host.contains(".s3.") || host.starts_with("s3-") || host.contains(".s3-"))
}

/// The region an address at AWS S3 names (`s3.<region>.amazonaws.com`), else `default`. The
/// address is lower-case.
fn aws_region(url: &str, default: &str) -> String {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.split(':').next().unwrap_or(authority);
    let after = host
        .rsplit_once(".s3.")
        .map(|(_, a)| a)
        .or_else(|| host.strip_prefix("s3."))
        .or_else(|| host.rsplit_once(".s3-").map(|(_, a)| a))
        .or_else(|| host.strip_prefix("s3-"));
    after
        .and_then(|a| a.split('.').next())
        .filter(|r| !r.is_empty() && *r != "amazonaws" && *r != "dualstack")
        .map(str::to_string)
        .unwrap_or_else(|| default.to_string())
}

/// The connection whose credentials sign a request to `url`, and the region it is signed for:
/// one whose endpoint is the address's scheme and authority (default ports aside), or (an AWS
/// connection) an address at AWS S3 of a bucket the connection serves ([`for_bucket`]): never
/// another bucket at amazonaws.com. A saved connection's login never signs by address.
fn signer_for(url: &str) -> Result<Option<(Connection, String)>, String> {
    let lower = url.to_ascii_lowercase();
    let Some((scheme, rest)) = lower.split_once("://") else { return Ok(None) };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        return Ok(None);
    }
    let here = origin(scheme, authority);
    let host = authority.split(':').next().unwrap_or(authority);
    let aws = aws_host(scheme, &here, host);
    let bucket = aws_bucket(host, rest);
    let list_region = host.strip_prefix("s3.").and_then(|r| r.strip_suffix(".amazonaws.com"));
    let path = rest.split_once('/').map(|(_, p)| p).unwrap_or("");
    let list_root = path.split(['?', '#']).next().unwrap_or("").is_empty();
    // Parse before locking; only selected connections are cloned while the registry is held.
    let c = {
        let reg = read_registry()?;
        if let Some(c) = reg.implicit().find(|c| c.credentials.is_some() && c.endpoint.as_deref() == Some(here.as_str())) {
            return Ok(Some((c.clone(), c.region.clone())));
        }
        if !aws {
            return Ok(None);
        }
        match bucket {
            Some(bucket) => {
                let grants = read_grants()?;
                reg.implicit().find(|c| c.credentials.is_some() && c.endpoint.is_none() && serves(c, &bucket, &grants)).cloned()
            }
            // The service itself names no bucket: only an AWS connection of its region signs its root.
            None => {
                let Some(region) = list_region.filter(|_| list_root) else { return Ok(None) };
                reg.implicit().find(|c| c.credentials.is_some() && c.endpoint.is_none() && c.region == region).cloned()
            }
        }
    };
    Ok(c.map(|c| {
        let region = aws_region(&lower, &c.region);
        (c, region)
    }))
}

/// The bucket an AWS S3 address names: its host's first label (virtual-hosted) or its path's
/// first part (path-style at `s3.<region>.amazonaws.com`).
fn aws_bucket(host: &str, rest: &str) -> Option<String> {
    // Virtual-hosted: bucket.s3.amazonaws.com, bucket.s3.<region>.amazonaws.com, or the legacy
    // bucket.s3-<region>.amazonaws.com.
    if let Some(b) = host
        .split_once(".s3.")
        .map(|(b, _)| b)
        .or_else(|| host.strip_suffix(".s3.amazonaws.com"))
        .or_else(|| host.split_once(".s3-").map(|(b, _)| b))
    {
        return Some(b.to_string());
    }
    let path = rest.split_once('/').map(|(_, p)| p).unwrap_or("");
    path.split(['/', '?', '#']).next().filter(|b| valid_bucket(b)).map(str::to_string)
}

/// The bucket an address read at `endpoint` (empty: AWS) names: an `s3://` address's, a web
/// address's first path part at a custom endpoint, or at AWS its host's or path's bucket. `None`
/// for an address of no bucket (the account's bucket list) or of no S3 address at all.
pub fn bucket_of(endpoint: &str, url: &str) -> Option<String> {
    if is_s3(url) {
        return parse(url).ok().map(|(bucket, _)| bucket);
    }
    let lower = url.to_ascii_lowercase();
    let (_, rest) = lower.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    if endpoint.is_empty() {
        let host = authority.split(':').next().unwrap_or(authority);
        return aws_bucket(host, rest);
    }
    let (_, original_rest) = url.split_once("://")?;
    let path = original_rest.get(authority.len()..).unwrap_or("");
    path.trim_start_matches('/').split(['/', '?', '#']).next().filter(|b| valid_bucket(b)).map(str::to_string)
}

/// The headers that sign a GET of `url` for S3 at `date` (`YYYYMMDDTHHMMSSZ`).
fn sign_get(url: &str, region: &str, cr: &Credentials, date: &str) -> Result<Vec<(String, String)>, String> {
    let (host, path, query) = split_url(url)?;
    let mut headers = vec![
        ("host".to_string(), host.clone()),
        ("x-amz-content-sha256".to_string(), UNSIGNED.to_string()),
        ("x-amz-date".to_string(), date.to_string()),
    ];
    if let Some(t) = &cr.session_token {
        headers.push(("x-amz-security-token".to_string(), t.clone()));
    }
    let auth = authorization(
        &Request { method: "GET", path: &path, query: &query, headers: &headers, payload_hash: UNSIGNED },
        region,
        "s3",
        date,
        cr,
    );
    // The browser and the HTTP client set Host themselves.
    let mut out: Vec<(String, String)> = headers.into_iter().filter(|(k, _)| k != "host").collect();
    out.push(("authorization".to_string(), auth));
    Ok(out)
}

/// A web address's host (with its port), path and query.
fn split_url(url: &str) -> Result<(String, String, String), String> {
    let rest =
        url.split_once("://").map(|(_, r)| r).ok_or_else(|| rustgis_data::diagnostics::message_with_address(url, "not a web address"))?;
    let rest = rest.split('#').next().unwrap_or(rest);
    let (authority, path_query) = match rest.find('/') {
        Some(i) => (rest.get(..i).unwrap_or(""), rest.get(i..).unwrap_or("/")),
        None => match rest.find('?') {
            Some(i) => (rest.get(..i).unwrap_or(""), rest.get(i..).unwrap_or("")),
            None => (rest, "/"),
        },
    };
    let (path, query) = path_query.split_once('?').unwrap_or((path_query, ""));
    let path = if path.is_empty() { "/" } else { path };
    // The default port isn't part of Host.
    let scheme = url.split_once("://").map(|(s, _)| s).unwrap_or("https");
    let host = origin(scheme, authority).split_once("://").map(|(_, h)| h.to_string()).unwrap_or_default();
    Ok((host, path.to_string(), query.to_string()))
}

/// A request as Signature Version 4 sees it. `headers` are lower-case names (Host included).
pub struct Request<'a> {
    pub method: &'a str,
    /// The path as sent (already encoded).
    pub path: &'a str,
    /// The query as sent, or empty.
    pub query: &'a str,
    pub headers: &'a [(String, String)],
    pub payload_hash: &'a str,
}

/// The Authorization header of a request (Signature Version 4).
pub fn authorization(r: &Request<'_>, region: &str, service: &str, date: &str, cr: &Credentials) -> String {
    let day = date.get(..8).unwrap_or(date);
    let mut headers: Vec<(String, String)> =
        r.headers.iter().map(|(k, v)| (k.to_ascii_lowercase(), v.split_whitespace().collect::<Vec<_>>().join(" "))).collect();
    headers.sort();
    let signed: Vec<&str> = headers.iter().map(|(k, _)| k.as_str()).collect();
    let signed = signed.join(";");
    let canonical_headers: String = headers.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let canonical = format!("{}\n{}\n{}\n{canonical_headers}\n{signed}\n{}", r.method, r.path, canonical_query(r.query), r.payload_hash);
    let scope = format!("{day}/{region}/{service}/aws4_request");
    let to_sign = format!("AWS4-HMAC-SHA256\n{date}\n{scope}\n{}", hex(&Sha256::digest(canonical.as_bytes())));
    let key = [day, region, service, "aws4_request"]
        .iter()
        .fold(format!("AWS4{}", cr.secret_access_key).into_bytes(), |k, part| hmac(&k, part.as_bytes()).to_vec());
    let signature = hex(&hmac(&key, to_sign.as_bytes()));
    format!("AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed}, Signature={signature}", cr.access_key_id)
}

/// A query in canonical form: each name and value encoded, sorted.
fn canonical_query(q: &str) -> String {
    let mut pairs: Vec<(String, String)> = q
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (encode(&decode(k), true), encode(&decode(v), true))
        })
        .collect();
    pairs.sort();
    pairs.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&")
}

/// Percent-encodes as S3 wants: everything but unreserved characters (and `/` unless asked).
pub fn encode(s: &str, slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b'/' if !slash => out.push('/'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Decodes a path's percent-escapes (a `+` stays a plus).
fn decode_path(s: &str) -> String {
    decode(&s.replace('+', "%2B"))
}

/// Decodes percent-escapes (and `+` as a space, as query strings do).
fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        let hexval = |c: u8| (c as char).to_digit(16);
        match (b, bytes.get(i + 1).and_then(|&c| hexval(c)), bytes.get(i + 2).and_then(|&c| hexval(c))) {
            (b'%', Some(h), Some(l)) => {
                out.push((h * 16 + l) as u8);
                i += 3;
            }
            (b'+', _, _) => {
                out.push(b' ');
                i += 1;
            }
            _ => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// HMAC-SHA256.
fn hmac(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let pad = |x: u8| k.iter().map(|b| b ^ x).collect::<Vec<u8>>();
    let inner = Sha256::new().chain_update(pad(0x36)).chain_update(msg).finalize();
    Sha256::new().chain_update(pad(0x5c)).chain_update(inner).finalize().into()
}

fn now_secs() -> i64 {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// `YYYYMMDDTHHMMSSZ` of a Unix time.
fn amz_date(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
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
    format!("{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// How many entries a listing returns at most.
pub const MAX_LIST: usize = 10_000;

/// The request listing a bucket's objects under a prefix, a level at a time (ListObjectsV2 with
/// `/` as the delimiter), from `token` when continuing.
pub fn list_url(bucket: &str, prefix: &str, token: Option<&str>, conn: Option<&Connection>) -> String {
    let base = object_url(bucket, "", conn);
    // `https://bucket.s3…/` or `endpoint/bucket/`: the bucket itself, without the slash.
    let base = if conn.and_then(|c| c.endpoint.as_deref()).is_some() || bucket.contains('.') {
        base.trim_end_matches('/').to_string()
    } else {
        base
    };
    let mut q = format!("{base}?list-type=2&delimiter=%2F&max-keys=1000&prefix={}", encode(prefix, true));
    if let Some(t) = token {
        q.push_str(&format!("&continuation-token={}", encode(t, true)));
    }
    q
}

/// The request listing a connection's buckets (ListBuckets; credentials needed).
pub fn buckets_url(conn: &Connection) -> String {
    match &conn.endpoint {
        Some(ep) => format!("{ep}/"),
        None => format!("https://s3.{}.amazonaws.com/", conn.region),
    }
}

/// An object in a listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Object {
    pub key: String,
    pub size: u64,
    pub modified: String,
}

/// A level of a bucket: its "folders" (prefixes ending in `/`), its objects, and where the next
/// page starts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Listing {
    pub prefixes: Vec<String>,
    pub objects: Vec<Object>,
    pub next: Option<String>,
}

/// An S3 error answer's code and message, if `xml` is one.
fn error_of(doc: &roxmltree::Document) -> Option<String> {
    let root = doc.root_element();
    (root.tag_name().name() == "Error").then(|| {
        let text = |n: &str| root.children().find(|c| c.tag_name().name() == n).and_then(|c| c.text()).unwrap_or("").to_string();
        format!("{}: {}", text("Code"), text("Message")).chars().take(300).collect()
    })
}

fn parse_xml(bytes: &[u8]) -> Result<roxmltree::Document<'_>, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "the answer isn't UTF-8 text".to_string())?;
    // Through the data crate's depth check: a listing is hostile input like any document.
    rustgis_data::xml::parse(text, roxmltree::ParsingOptions::default()).map_err(|_| "the answer isn't an S3 listing".to_string())
}

/// A ListObjectsV2 answer.
pub fn parse_list(bytes: &[u8]) -> Result<Listing, String> {
    let doc = parse_xml(bytes)?;
    if let Some(e) = error_of(&doc) {
        return Err(e);
    }
    let root = doc.root_element();
    if root.tag_name().name() != "ListBucketResult" {
        return Err("the answer isn't an S3 listing".into());
    }
    let child = |n: &roxmltree::Node, name: &str| {
        n.children().find(|c| c.tag_name().name() == name).and_then(|c| c.text()).unwrap_or("").to_string()
    };
    let mut out = Listing::default();
    for n in root.children().filter(|c| c.is_element()).take(MAX_LIST) {
        match n.tag_name().name() {
            "Contents" => {
                let key = child(&n, "Key");
                if !key.is_empty() && key.len() <= 1024 {
                    out.objects.push(Object { key, size: child(&n, "Size").parse().unwrap_or(0), modified: child(&n, "LastModified") });
                }
            }
            "CommonPrefixes" => {
                let p = child(&n, "Prefix");
                if !p.is_empty() && p.len() <= 1024 {
                    out.prefixes.push(p);
                }
            }
            _ => {}
        }
    }
    let truncated = child(&root, "IsTruncated") == "true";
    out.next = Some(child(&root, "NextContinuationToken")).filter(|t| truncated && !t.is_empty() && t.len() <= 4096);
    Ok(out)
}

/// A ListBuckets answer: the bucket names.
pub fn parse_buckets(bytes: &[u8]) -> Result<Vec<String>, String> {
    let doc = parse_xml(bytes)?;
    if let Some(e) = error_of(&doc) {
        return Err(e);
    }
    Ok(doc
        .descendants()
        .filter(|n| n.tag_name().name() == "Bucket")
        .filter_map(|b| b.children().find(|c| c.tag_name().name() == "Name").and_then(|c| c.text()))
        .filter(|b| valid_bucket(b))
        .take(1000)
        .map(str::to_string)
        .collect())
}

/// What an AWS profile gives, from the text of `~/.aws/credentials` and `~/.aws/config`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Profile {
    pub credentials: Option<Credentials>,
    pub region: Option<String>,
    pub endpoint: Option<String>,
}

/// Reads a profile from the shared credentials and config files' text: `[name]` in the
/// credentials file, `[profile name]` (or `[default]`) in the config file.
pub fn profile(credentials_ini: &str, config_ini: &str, name: &str) -> Profile {
    fn section<'a>(ini: &'a str, header: &str) -> Vec<(&'a str, &'a str)> {
        let mut inside = false;
        let mut out = Vec::new();
        for line in ini.lines().take(10_000) {
            let line = line.trim();
            if line.starts_with('[') {
                inside = line.trim_start_matches('[').trim_end_matches(']').trim() == header;
            } else if inside
                && !line.starts_with(['#', ';'])
                && let Some((k, v)) = line.split_once('=')
            {
                out.push((k.trim(), v.trim()));
            }
        }
        out
    }
    let creds = section(credentials_ini, name);
    let config_header = if name == "default" { "default".to_string() } else { format!("profile {name}") };
    let config = section(config_ini, &config_header);
    let get = |k: &str| {
        creds.iter().chain(config.iter()).find(|(key, _)| key.eq_ignore_ascii_case(k)).map(|(_, v)| v.to_string()).filter(|v| !v.is_empty())
    };
    let credentials = match (get("aws_access_key_id"), get("aws_secret_access_key")) {
        (Some(access_key_id), Some(secret_access_key)) => {
            Some(Credentials { access_key_id, secret_access_key, session_token: get("aws_session_token") })
        }
        _ => None,
    };
    Profile { credentials, region: get("region"), endpoint: get("endpoint_url") }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A public `s3://` address from a catalog: read anonymously at AWS's global endpoint, or at
    /// the region its refusal names (Digital Earth Africa's af-south-1); a bucket refusing anonymous
    /// reads is refused by name.
    #[test]
    fn public_buckets_are_read_at_their_region() {
        use crate::failure::Failure;
        let url = "s3://deafrica-services/wofs/x211/a b.tif";
        assert_eq!(public_url(url, None).unwrap(), "https://deafrica-services.s3.amazonaws.com/wofs/x211/a%20b.tif");
        assert_eq!(
            public_url(url, Some("af-south-1")).unwrap(),
            "https://deafrica-services.s3.af-south-1.amazonaws.com/wofs/x211/a%20b.tif"
        );
        assert!(public_url("https://x.org/a.tif", None).is_err());
        // What AWS says, as the excerpt of its refusal keeps it.
        let constraint = "IllegalLocationConstraintException The af-south-1 location constraint is incompatible for the region specific endpoint this request was sent to. XJBV";
        assert_eq!(region_in(constraint).as_deref(), Some("af-south-1"));
        let redirect = "PermanentRedirect The bucket you are attempting to access must be addressed using the specified endpoint. b.s3.eu-west-1.amazonaws.com";
        assert_eq!(region_in(redirect).as_deref(), Some("eu-west-1"));
        assert_eq!(region_in("AccessDenied Access Denied"), None);
        assert_eq!(region_in("The <script> location constraint"), None);
        for not in ["evil-host-1.attacker", "e-x-1", "us-east-x", "us-east-123", "u5-east-1", "us--1"] {
            assert_eq!(region_in(&format!("The {not} location constraint")), None, "{not}");
        }
        for r in ["us-gov-west-1", "ap-southeast-2", "il-central-1"] {
            assert_eq!(region_in(&format!("x.s3.{r}.amazonaws.com")).as_deref(), Some(r));
        }
        let refusal = |status: u16, text: &str| Failure::answered("", status, &[], text.as_bytes());
        // Read at the global endpoint: no region.
        assert_eq!(public_region(url, |_| Ok(())).unwrap(), None);
        // Refused there with the region named: read again there, once.
        let asked = std::sync::Mutex::new(Vec::new());
        let r = public_region(url, |u| {
            asked.lock().unwrap().push(u.to_string());
            if u.contains(".s3.amazonaws.com") { Err(refusal(400, &format!("<Error>{constraint}</Error>"))) } else { Ok(()) }
        });
        assert_eq!(r.unwrap().as_deref(), Some("af-south-1"));
        assert_eq!(asked.lock().unwrap().len(), 2);
        // A read that got no answer (a timeout) is tried once more.
        let tries = std::sync::atomic::AtomicUsize::new(0);
        let r = public_region(url, |u| match tries.fetch_add(1, std::sync::atomic::Ordering::Relaxed) {
            0 => Err(Failure::new(u, "timed out")),
            _ => Ok(()),
        });
        assert_eq!((r.unwrap(), tries.into_inner()), (None, 2));
        // Requester pays (Landsat's) or private: refused, naming the bucket.
        let e = public_region("s3://usgs-landsat/a.tif", |_| Err(refusal(403, "AccessDenied Access Denied"))).unwrap_err();
        assert!(e.contains("s3://usgs-landsat") && e.contains("anonymously"), "{e}");
        // The region named, but refused there too.
        let e = public_region(url, |u| Err(refusal(if u.contains("af-south-1") { 403 } else { 400 }, constraint))).unwrap_err();
        assert!(e.contains("anonymously"), "{e}");
    }

    #[test]
    fn bucket_paths_at_custom_endpoints_are_case_sensitive() {
        assert_eq!(bucket_of("https://minio.example", "https://minio.example/MyBucket/key"), None);
        assert_eq!(bucket_of("https://minio.example", "https://minio.example/mybucket/key").as_deref(), Some("mybucket"));
        assert_eq!(bucket_of("", "https://BUCKET.s3.amazonaws.com/key").as_deref(), Some("bucket"));
    }

    #[test]
    fn aws_bucket_from_every_host_form() {
        let b = |host: &str, rest: &str| aws_bucket(host, rest);
        assert_eq!(b("data.s3.amazonaws.com", "data.s3.amazonaws.com/a/b.tif").as_deref(), Some("data"));
        assert_eq!(b("data.s3.us-west-2.amazonaws.com", "x/a.tif").as_deref(), Some("data"));
        // The legacy regional form: the bucket is the host's, never a key's first segment.
        assert_eq!(b("data.s3-us-west-2.amazonaws.com", "data.s3-us-west-2.amazonaws.com/other/a.tif").as_deref(), Some("data"));
        // Path style: the first path segment.
        assert_eq!(b("s3-us-west-2.amazonaws.com", "s3-us-west-2.amazonaws.com/data/a.tif").as_deref(), Some("data"));
        assert_eq!(b("s3.amazonaws.com", "s3.amazonaws.com/data/a.tif").as_deref(), Some("data"));
    }

    fn example() -> Credentials {
        Credentials {
            access_key_id: "AKIAIOSFODNN7EXAMPLE".into(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
            session_token: None,
        }
    }

    #[test]
    fn signature_v4_test_vectors() {
        // AWS's Signature Version 4 test suite, "get-vanilla".
        let cr = Credentials {
            access_key_id: "AKIDEXAMPLE".into(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
            session_token: None,
        };
        let h = vec![("host".to_string(), "example.amazonaws.com".to_string()), ("x-amz-date".to_string(), "20150830T123600Z".to_string())];
        let empty = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let r = Request { method: "GET", path: "/", query: "", headers: &h, payload_hash: empty };
        assert_eq!(
            authorization(&r, "us-east-1", "service", "20150830T123600Z", &cr),
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, SignedHeaders=host;x-amz-date, Signature=5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
        );
        // The S3 documentation's GET Object example (a range of examplebucket/test.txt).
        let h = vec![
            ("host".to_string(), "examplebucket.s3.amazonaws.com".to_string()),
            ("range".to_string(), "bytes=0-9".to_string()),
            ("x-amz-content-sha256".to_string(), empty.to_string()),
            ("x-amz-date".to_string(), "20130524T000000Z".to_string()),
        ];
        let r = Request { method: "GET", path: "/test.txt", query: "", headers: &h, payload_hash: empty };
        assert!(
            authorization(&r, "us-east-1", "s3", "20130524T000000Z", &example())
                .ends_with("Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41")
        );
        // ... and its GET Bucket (list objects) example: the query in canonical order.
        let h = vec![
            ("host".to_string(), "examplebucket.s3.amazonaws.com".to_string()),
            ("x-amz-content-sha256".to_string(), empty.to_string()),
            ("x-amz-date".to_string(), "20130524T000000Z".to_string()),
        ];
        let r = Request { method: "GET", path: "/", query: "prefix=J&max-keys=2", headers: &h, payload_hash: empty };
        assert!(
            authorization(&r, "us-east-1", "s3", "20130524T000000Z", &example())
                .ends_with("Signature=34b48302e7b5fa45bde8084f4b7868a86f0a534bc59db6670ed5711ef69dc6f7")
        );
    }

    #[test]
    fn dates_and_encoding() {
        assert_eq!(amz_date(0), "19700101T000000Z");
        assert_eq!(amz_date(1_369_353_600), "20130524T000000Z");
        assert_eq!(amz_date(1_709_251_199), "20240229T235959Z");
        assert_eq!(encode("a b/c~d+é", false), "a%20b/c~d%2B%C3%A9");
        assert_eq!(encode("a/b", true), "a%2Fb");
        assert_eq!(decode("a%2Fb+c%zz"), "a/b c%zz");
        assert_eq!(canonical_query("b=2&a=x%20y&a=1"), "a=1&a=x%20y&b=2");
        assert_eq!(format!("{:?}", example()), "Credentials(…)");
    }

    #[test]
    fn addresses() {
        assert_eq!(parse("s3://my-bucket/a/b c.tif").unwrap(), ("my-bucket".into(), "a/b c.tif".into()));
        assert_eq!(parse("S3://my-bucket").unwrap(), ("my-bucket".into(), String::new()));
        for bad in ["s3://", "s3://A_B/x", "s3://ab/x", "https://x.org/a", "s3://-ab-/x"] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        // A query in place of a key isn't repeated in the error, and the bucket stays readable.
        let error = parse("s3://bucket?X-Custom-Cred=hidden").unwrap_err();
        assert!(!error.contains("hidden") && error.contains("s3://bucket/") && error.contains("bucket name"), "{error}");
        assert_eq!(rustgis_data::diagnostics::redact(&error), error);
        assert_eq!(object_url("my-bucket", "a/b c.tif", None), "https://my-bucket.s3.amazonaws.com/a/b%20c.tif");
        assert_eq!(object_url("my.bucket", "k", None), "https://s3.amazonaws.com/my.bucket/k");
        let aws = Connection { name: "a".into(), endpoint: None, region: "eu-west-1".into(), buckets: vec![], credentials: None };
        assert_eq!(object_url("b-1", "k", Some(&aws)), "https://b-1.s3.eu-west-1.amazonaws.com/k");
        let minio = Connection { endpoint: Some("http://127.0.0.1:9000".into()), ..aws.clone() };
        assert_eq!(object_url("b-1", "k/x", Some(&minio)), "http://127.0.0.1:9000/b-1/k/x");
        assert_eq!(
            list_url("b-1", "a/b c/", None, Some(&minio)),
            "http://127.0.0.1:9000/b-1?list-type=2&delimiter=%2F&max-keys=1000&prefix=a%2Fb%20c%2F"
        );
        assert_eq!(
            list_url("b-1", "", Some("t/1"), None),
            "https://b-1.s3.amazonaws.com/?list-type=2&delimiter=%2F&max-keys=1000&prefix=&continuation-token=t%2F1"
        );
        assert_eq!(split_url("https://h.org:443/a?b=1#x").unwrap(), ("h.org".into(), "/a".into(), "b=1".into()));
        assert_eq!(split_url("http://h:9000").unwrap(), ("h:9000".into(), "/".into(), String::new()));
    }

    #[test]
    fn connections_sign_only_their_own_endpoint() {
        let cr = Some(Credentials { access_key_id: "AKID".into(), secret_access_key: "secret".into(), session_token: Some("tok".into()) });
        connect(Connection {
            name: "test-sign".into(),
            endpoint: Some("http://127.0.0.1:9999".into()),
            region: String::new(),
            buckets: vec!["sign-test".into()],
            credentials: cr.clone(),
        })
        .unwrap();
        let p = prepare("s3://sign-test/x/y.tif").unwrap();
        assert_eq!(p.url, "http://127.0.0.1:9999/sign-test/x/y.tif");
        assert!(p.signed);
        let names: Vec<&str> = p.headers.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(names, ["x-amz-content-sha256", "x-amz-date", "x-amz-security-token", "authorization"]);
        assert!(p.headers.iter().all(|(_, v)| !v.contains("secret")));
        // The same endpoint by address is signed; another host, or the same with a user part, isn't.
        assert!(prepare("http://127.0.0.1:9999/sign-test/x").unwrap().signed);
        for other in ["http://127.0.0.1:9998/sign-test/x", "https://127.0.0.1:9999/x", "http://u@127.0.0.1:9999/x", "https://example.org/x"]
        {
            let p = prepare(other).unwrap();
            assert!(!p.signed && p.headers.is_empty() && p.url == other, "{other}");
        }
        // A bucket no connection names is anonymous at AWS's global endpoint (unless another test's
        // default connection is there).
        assert!(disconnect("test-sign") && !disconnect("test-sign"));
        assert!(
            connect(Connection { name: " ".into(), endpoint: None, region: String::new(), buckets: vec![], credentials: None }).is_err()
        );
        assert!(
            connect(Connection {
                name: "x".into(),
                endpoint: Some("ftp://h".into()),
                region: String::new(),
                buckets: vec![],
                credentials: None
            })
            .is_err()
        );
        assert!(
            connect(Connection {
                name: "x".into(),
                endpoint: Some("https://h.org/path".into()),
                region: String::new(),
                buckets: vec![],
                credentials: None
            })
            .is_err()
        );
        assert!(
            connect(Connection { name: "x".into(), endpoint: None, region: "a b".into(), buckets: vec![], credentials: None }).is_err()
        );
        assert!(
            connect(Connection { name: "x".into(), endpoint: None, region: String::new(), buckets: vec!["UP".into()], credentials: None })
                .is_err()
        );
        assert!(
            connect(Connection {
                name: "x".into(),
                endpoint: Some("http://169.254.169.254".into()),
                region: String::new(),
                buckets: vec![],
                credentials: None
            })
            .is_err()
        );
    }

    #[test]
    fn connections_sign_only_buckets_they_serve() {
        let cr = || Some(Credentials { access_key_id: "AKID2".into(), secret_access_key: "s2".into(), session_token: None });
        // A connection given no buckets claims none: another bucket is read anonymously from AWS.
        connect(Connection {
            name: "rule-endpoint".into(),
            endpoint: Some("https://minio.rule.example:443".into()),
            region: String::new(),
            buckets: vec![],
            credentials: cr(),
        })
        .unwrap();
        assert_eq!(connection("rule-endpoint").unwrap().endpoint.as_deref(), Some("https://minio.rule.example"), "default port dropped");
        let p = prepare("s3://rule-other-bucket/k").unwrap();
        assert_eq!((p.url.as_str(), p.signed), ("https://rule-other-bucket.s3.amazonaws.com/k", false));
        // Its own endpoint is signed, whatever the default port's spelling; the path as S3 encodes it.
        let p = prepare("https://minio.rule.example:443/b-1/a b(1)+c%7e.tif").unwrap();
        assert!(p.signed);
        assert_eq!(p.url, "https://minio.rule.example:443/b-1/a%20b%281%29%2Bc~.tif");
        // A bucket browsed through it is served from then on.
        grant("rule-endpoint", "rule-granted");
        let p = prepare("s3://rule-granted/k").unwrap();
        assert_eq!((p.url.as_str(), p.signed), ("https://minio.rule.example/rule-granted/k", true));
        // ... unless read anonymously on purpose.
        let p = prepare(&anonymous("s3://rule-granted/k")).unwrap();
        assert_eq!((p.url.as_str(), p.signed), ("https://rule-granted.s3.amazonaws.com/k", false));
        assert!(crate::address::check(&anonymous("s3://rule-granted/k"), None).is_ok());
        // An AWS connection signs only its buckets at amazonaws.com, for the region the address names.
        connect(Connection {
            name: "rule-aws".into(),
            endpoint: None,
            region: "eu-west-1".into(),
            buckets: vec!["rule-mine".into()],
            credentials: cr(),
        })
        .unwrap();
        let p = prepare("https://rule-mine.s3.us-west-2.amazonaws.com/k").unwrap();
        assert!(p.signed && p.headers.iter().any(|(k, v)| k == "authorization" && v.contains("/us-west-2/s3/aws4_request")));
        let p = prepare("https://rule-mine.s3.amazonaws.com/k").unwrap();
        assert!(p.headers.iter().any(|(k, v)| k == "authorization" && v.contains("/eu-west-1/s3/aws4_request")));
        for other in [
            "https://rule-theirs.s3.amazonaws.com/k",
            "https://s3.us-east-1.amazonaws.com/rule-theirs/k",
            "https://rule-mine.s3.amazonaws.com.evil.example/k",
            "http://rule-mine.s3.amazonaws.com/k",
        ] {
            assert!(!prepare(other).unwrap().signed, "{other}");
        }
        // The account's bucket list (the service root of its region) is signed; another region's
        // root, and the root with a path, aren't.
        let c = connection("rule-aws").unwrap();
        let p = prepare(&buckets_url(&c)).unwrap();
        assert!(p.signed && p.headers.iter().any(|(k, v)| k == "authorization" && v.contains("/eu-west-1/s3/aws4_request")));
        assert!(!prepare("https://s3.us-east-1.amazonaws.com/").unwrap().signed);
        // Replacing a connection by name forgets what was granted to the old one.
        connect(Connection {
            name: "rule-endpoint".into(),
            endpoint: Some("https://other.rule.example".into()),
            region: String::new(),
            buckets: vec![],
            credentials: cr(),
        })
        .unwrap();
        assert!(!prepare("s3://rule-granted/k").unwrap().signed, "grants don't pass to a replacement");
        disconnect("rule-endpoint");
        disconnect("rule-aws");
        assert!(!prepare("s3://rule-granted/k").unwrap().signed, "grants go with the connection");
    }

    fn saved_login(name: &str, key: &str, endpoint: Option<&str>, buckets: &[&str]) -> Connection {
        Connection {
            name: name.into(),
            endpoint: endpoint.map(str::to_string),
            region: String::new(),
            buckets: buckets.iter().map(|b| b.to_string()).collect(),
            credentials: Some(Credentials { access_key_id: key.into(), secret_access_key: "saved-secret-xyz".into(), session_token: None }),
        }
    }

    fn key_of(p: &Prepared) -> String {
        let auth = p.headers.iter().find(|(k, _)| k == "authorization").map(|(_, v)| v.as_str()).unwrap_or("");
        auth.split("Credential=").nth(1).and_then(|c| c.split('/').next()).unwrap_or("").to_string()
    }

    #[test]
    fn saved_logins_sign_only_through_their_own_binding() {
        let ep = "http://127.0.0.1:9701";
        let (a, b) = (new_binding().unwrap(), new_binding().unwrap());
        assert_ne!(a, b);
        assert!(a.len() == 16 && a.bytes().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        let legacy = rustgis_data::s3_address::LEGACY;
        assert!(connect_saved(saved_login("sv-pending", "NEVERINSTALLED", None, &["sv-bucket"]), legacy, "c1").is_err());
        assert!(connection("sv-pending").is_none(), "a pending marker can never install credentials");
        let pending = rustgis_data::s3_address::bind(legacy, "", "s3://sv-bucket/k.fgb").unwrap();
        assert_eq!(prepare(&pending).unwrap_err(), UNRESOLVED);
        // The second saved connection is registered first: insertion order decides nothing.
        connect_saved(saved_login("sv-b", "SAVEDKEYB", Some(ep), &["sv-bucket"]), &b, "c1").unwrap();
        connect_saved(saved_login("sv-a", "SAVEDKEYA", Some(ep), &["sv-bucket"]), &a, "c1").unwrap();
        let url = "http://127.0.0.1:9701/sv-bucket/k.fgb";
        let wa = rustgis_data::s3_address::bind(&a, ep, url).unwrap();
        let wb = rustgis_data::s3_address::bind(&b, ep, url).unwrap();
        let (pa, pb) = (prepare(&wa).unwrap(), prepare(&wb).unwrap());
        assert_eq!((key_of(&pa), key_of(&pb)), ("SAVEDKEYA".to_string(), "SAVEDKEYB".to_string()));
        assert!(pa.signed && pa.url == url);
        assert!(pa.headers.iter().all(|(_, v)| !v.contains("saved-secret")));
        // An unrelated address of the same endpoint or bucket isn't signed by either login.
        for raw in [url, "s3://sv-bucket/k.fgb", "http://127.0.0.1:9701/"] {
            let p = prepare(raw).unwrap();
            assert!(!p.signed && p.headers.is_empty(), "{raw}");
        }
        assert_eq!(for_bucket("sv-bucket"), None);
        // The account's bucket list, through the exact connection's own endpoint.
        let root = rustgis_data::s3_address::bind(&b, ep, "http://127.0.0.1:9701/").unwrap();
        assert_eq!(key_of(&prepare(&root).unwrap()), "SAVEDKEYB");
        // s3:// addresses are fetched at the connection's endpoint.
        let s3 = rustgis_data::s3_address::bind(&a, ep, "s3://sv-bucket/dir/k.fgb").unwrap();
        let p = prepare(&s3).unwrap();
        assert_eq!((p.url.as_str(), key_of(&p).as_str()), ("http://127.0.0.1:9701/sv-bucket/dir/k.fgb", "SAVEDKEYA"));
        assert_eq!(https_url(&s3).unwrap(), p.url);
        // Nothing falls back to another signer.
        let bind = rustgis_data::s3_address::bind;
        let other_endpoint = bind(&a, "http://127.0.0.1:9702", "http://127.0.0.1:9702/sv-bucket/k").unwrap();
        let mismatched = bind(&a, "http://127.0.0.1:9702", url).unwrap();
        let ungranted = bind(&a, ep, "http://127.0.0.1:9701/sv-other/k").unwrap();
        let elsewhere = bind(&a, ep, "http://127.0.0.1:9799/sv-bucket/k").unwrap();
        let userinfo = bind(&a, ep, "http://u@127.0.0.1:9701/sv-bucket/k").unwrap();
        let wrong_case = bind(&a, ep, "http://127.0.0.1:9701/SV-BUCKET/k").unwrap();
        for wrong in [other_endpoint, mismatched, ungranted, elsewhere, userinfo, wrong_case] {
            assert_eq!(prepare(&wrong).unwrap_err(), MISMATCH, "{wrong}");
        }
        for unresolved in [bind("", ep, url).unwrap(), bind("00000000deadbeef", ep, url).unwrap()] {
            assert_eq!(prepare(&unresolved).unwrap_err(), UNRESOLVED);
        }
        for malformed in [format!("rustgis-s3:{a}:99:{ep}:{url}"), format!("rustgis-s3:{a}:21:{ep}:{wa}"), anonymous(&wa)] {
            assert!(prepare(&malformed).is_err(), "{malformed}");
        }
        // An explicit grant is one binding's: the other still can't read that bucket.
        grant_saved(&a, "sv-granted").unwrap();
        let granted_url = "http://127.0.0.1:9701/sv-granted/k";
        assert_eq!(key_of(&prepare(&bind(&a, ep, granted_url).unwrap()).unwrap()), "SAVEDKEYA");
        assert_eq!(prepare(&bind(&b, ep, granted_url).unwrap()).unwrap_err(), MISMATCH);
        grant_saved(&b, "sv-granted").unwrap();
        assert_eq!(key_of(&prepare(&bind(&b, ep, granted_url).unwrap()).unwrap()), "SAVEDKEYB");
        assert!(grant_saved("", "sv-granted").is_err() && grant_saved("00000000deadbeef", "sv-granted").is_err());
        assert!(grant_saved(&a, "NOT_A_BUCKET").is_err());
        // Judged without any login, an address is kept (or refused) by where it is fetched.
        assert!(crate::address::check(&bind("", ep, url).unwrap(), None).is_ok());
        assert!(crate::address::check(&bind("", "https://s3.example.org", "s3://sv-bucket/k").unwrap(), None).is_ok());
        assert!(crate::address::check(&bind("", "http://169.254.169.254", "http://169.254.169.254/k").unwrap(), None).is_err());
        assert!(crate::address::check(&anonymous(&wa), None).is_err());
        assert!(crate::address::check("rustgis-s3:junk", None).is_err());
        // The server a wrapped address is throttled under is its own, in a tile template too.
        assert_eq!(rustgis_data::s3_address::reached(&wa), url);
        assert_eq!(rustgis_data::s3_address::reached(&format!("pmtiles:{{z}}/{{x}}/{{y}}@{wa}")), url);
        // Replaced by an ordinary connection of the same name: the binding finds nothing.
        connect(saved_login("sv-a", "PLAINKEY", Some(ep), &["sv-bucket"])).unwrap();
        assert_eq!(prepare(&wa).unwrap_err(), UNRESOLVED);
        assert_eq!(saved_connection(&a), None);
        assert_eq!(key_of(&prepare(url).unwrap()), "PLAINKEY", "ordinary connections keep their implicit behaviour");
        assert_eq!(key_of(&prepare(&wb).unwrap()), "SAVEDKEYB");
        disconnect("sv-a");
        disconnect("sv-b");
        assert_eq!(prepare(&wb).unwrap_err(), UNRESOLVED);
    }

    #[test]
    fn saved_logins_keep_grants_only_for_the_same_login_and_endpoint() {
        let ep = "http://127.0.0.1:9711";
        let a = new_binding().unwrap();
        connect_saved(saved_login("sv-grant", "GRANTKEY1", Some(ep), &[]), &a, "c7").unwrap();
        grant_saved(&a, "sv-browsed").unwrap();
        let url = rustgis_data::s3_address::bind(&a, ep, "http://127.0.0.1:9711/sv-browsed/k").unwrap();
        assert!(prepare(&url).unwrap().signed);
        assert_eq!(binding_of("sv-grant").as_deref(), Some(a.as_str()));
        assert_eq!(saved_connection(&a).map(|(c, id)| (c.name, id)), Some(("sv-grant".to_string(), "c7".to_string())));
        // Logging in again at the same endpoint (new keys): the grant stays.
        connect_saved(saved_login("sv-grant", "GRANTKEY2", Some(ep), &[]), &a, "c7").unwrap();
        assert_eq!(key_of(&prepare(&url).unwrap()), "GRANTKEY2");
        // The same binding under another name (a rename): the old name is gone.
        connect_saved(saved_login("sv-grant-2", "GRANTKEY3", Some(ep), &[]), &a, "c7").unwrap();
        assert!(connection("sv-grant").is_none() && binding_of("sv-grant").is_none());
        // At another endpoint: nothing granted before carries over.
        connect_saved(saved_login("sv-grant-2", "GRANTKEY4", Some("http://127.0.0.1:9712"), &[]), &a, "c7").unwrap();
        let moved = rustgis_data::s3_address::bind(&a, "http://127.0.0.1:9712", "http://127.0.0.1:9712/sv-browsed/k").unwrap();
        assert_eq!(prepare(&moved).unwrap_err(), MISMATCH);
        // A binding is a token, not a name: junk and the id are refused.
        assert!(connect_saved(saved_login("sv-grant-3", "K", Some(ep), &[]), "c7", "c7").is_err());
        assert!(connect_saved(saved_login("sv-grant-3", "K", Some(ep), &[]), "", "c7").is_err());
        // A refused login neither registers its name nor replaces the existing login or grants.
        grant_saved(&a, "sv-browsed").unwrap();
        let bad = Credentials { access_key_id: "has space".into(), secret_access_key: "s".into(), session_token: None };
        assert!(connect_saved(Connection { credentials: Some(bad), ..saved_login("sv-grant-4", "K", Some(ep), &[]) }, &a, "c7").is_err());
        assert!(connection("sv-grant-4").is_none());
        assert_eq!(key_of(&prepare(&moved).unwrap()), "GRANTKEY4");
        disconnect("sv-grant-2");
    }

    #[test]
    fn listings_and_profiles() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/"><Name>b</Name><Prefix>tiles/</Prefix><IsTruncated>true</IsTruncated><NextContinuationToken>abc==</NextContinuationToken>
<Contents><Key>tiles/a.tif</Key><LastModified>2024-01-02T03:04:05.000Z</LastModified><Size>1234</Size></Contents>
<CommonPrefixes><Prefix>tiles/2024/</Prefix></CommonPrefixes></ListBucketResult>"#;
        let l = parse_list(xml).unwrap();
        assert_eq!(l.objects, vec![Object { key: "tiles/a.tif".into(), size: 1234, modified: "2024-01-02T03:04:05.000Z".into() }]);
        assert_eq!((l.prefixes, l.next), (vec!["tiles/2024/".to_string()], Some("abc==".to_string())));
        let err = br#"<Error><Code>AccessDenied</Code><Message>Access Denied</Message></Error>"#;
        assert_eq!(parse_list(err).unwrap_err(), "AccessDenied: Access Denied");
        assert!(parse_list(b"<html>").is_err() && parse_list(b"<a/>").is_err() && parse_list(&[0xff]).is_err());
        let buckets = br#"<ListAllMyBucketsResult><Buckets><Bucket><Name>one</Name></Bucket><Bucket><Name>BAD_</Name></Bucket></Buckets></ListAllMyBucketsResult>"#;
        assert_eq!(parse_buckets(buckets).unwrap(), vec!["one".to_string()]);
        let creds = "[default]\naws_access_key_id = A1\naws_secret_access_key=S1\n[work]\naws_access_key_id=A2\naws_secret_access_key = S2\naws_session_token=T2\n";
        let config = "[default]\nregion = eu-central-1\n[profile work]\nregion=us-west-2\nendpoint_url = https://minio.example.org\n";
        let d = profile(creds, config, "default");
        assert_eq!((d.credentials.unwrap().access_key_id, d.region, d.endpoint), ("A1".into(), Some("eu-central-1".into()), None));
        let w = profile(creds, config, "work");
        let wc = w.credentials.unwrap();
        assert_eq!(
            (wc.secret_access_key.as_str(), wc.session_token.as_deref(), w.endpoint.as_deref()),
            ("S2", Some("T2"), Some("https://minio.example.org"))
        );
        assert_eq!(profile(creds, config, "none"), Profile::default());
    }
}
