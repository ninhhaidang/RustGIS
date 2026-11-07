//! Project sources that read an S3 bucket through a saved connection: which saved connection
//! reads each, decided before anything is fetched.
//!
//! A project keeps an S3 source's address as `rustgis-s3:<binding>:<len>:<endpoint>:<url>`
//! ([`rustgis_data::s3_address`]) when it was added through a saved connection: the endpoint and
//! bucket it names, never a key. The binding is this computer's runtime token for a login, so one
//! read from a file is cleared ([`clear_bindings`]); a source with none can't be fetched. Opening
//! a project (or importing a map) settles each bucket its sources read ([`resolve_project_s3`]):
//!
//! - exactly one saved connection explicitly lists the bucket and could read it: it logs in
//!   (a profile, the environment, anonymously, or its saved keys) and the sources are bound to that login and opened. Its
//!   keys missing or the store refusing, they stay blocked, pending that connection's log in;
//! - several could, or the only candidate has no explicit bucket scope: they stay blocked,
//!   [`PendingS3`] lists the candidates, and the user chooses (`connection.resolveS3`). Nothing is fetched, signed or not, until then;
//! - none could: an older project's bare `s3://` address is read as it always was (anonymously,
//!   or through a session-only connection); one that names a connection's endpoint stays blocked
//!   with a notice.
//!
//! An older project's bare `s3://` address names no endpoint: any saved connection whose buckets
//! include (or aren't limited to) its bucket is a candidate. Its blocked placeholder uses the
//! reserved non-login marker `s3_address::LEGACY`, distinct from explicit AWS provenance.

use rustgis_data::s3_address;

use crate::Session;
use crate::connections::Connection;
use crate::render::rustgis_tiles::{cache, s3};

/// A bucket (at an endpoint) some project sources read, and the saved connections that could read
/// it: the user chooses one (`connection.resolveS3`); until then none of those sources is fetched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingS3 {
    /// The address of one of the sources (the first of the resource), as the project keeps it.
    pub source: String,
    /// The endpoint the sources name: `None` is unknown (an older project's bare `s3://`
    /// address), `Some("")` is AWS.
    pub endpoint: Option<String>,
    pub bucket: String,
    /// The ids of the saved S3 connections that may be chosen, in list order.
    pub candidates: Vec<String>,
}

/// What is said to someone whose choice of a connection is no longer one.
pub const STALE: &str = "This S3 connection choice is no longer available.";

impl Session {
    /// The runtime token of a saved S3 connection's login in this session, once it has logged in
    /// (the interface binds the addresses it adds through that connection to it).
    pub fn s3_binding(&self, id: &str) -> Option<&str> {
        self.s3_bindings.get(id).map(String::as_str)
    }

    /// Whether this saved ID can reuse a live login. A mapped login must belong to this session;
    /// ordinary session-only connections retain the existing name-based behavior.
    pub fn has_s3_login(&self, id: &str) -> bool {
        let Some(c) = self.connections.find(id).filter(|c| c.kind == "s3" && c.id == id) else { return false };
        s3::can_reuse_login(self.s3_binding(id), id, &c.name)
    }
}

/// Whether a source's address reads from S3: an `s3://` address, or one read through a saved
/// connection (well formed or not).
pub fn is_s3_backed(address: &str) -> bool {
    s3_address::is_wrapper(address) || s3::is_s3(address)
}

/// How many bytes of a project address come before the part that reads S3: all of a tile
/// template's (`pmtiles:{z}/{x}/{y}@`), whose archive does.
fn head_len(address: &str) -> usize {
    crate::sources::template_archive(address).map_or(0, |(head, _)| head.len())
}

/// The S3-reading part of a project address: itself, or a tile template's archive.
fn part(address: &str) -> &str {
    address.get(head_len(address)..).unwrap_or(address)
}

/// Clears imported runtime authority. A legacy placeholder becomes its original bare address;
/// its reserved marker was never authority to use a login.
pub(crate) fn clear_bindings<'a>(addresses: impl IntoIterator<Item = &'a mut String>) {
    for a in addresses {
        let at = head_len(a);
        let Some(p) = a.get(at..) else { continue };
        let cleared = match s3_address::parse(p) {
            Ok(Some(b)) if b.binding == s3_address::LEGACY => Some(b.url.to_string()),
            Ok(Some(b)) if !b.binding.is_empty() => s3_address::bind("", b.endpoint, b.url).ok(),
            _ => None,
        };
        if let Some(cleared) = cleared {
            *a = format!("{}{cleared}", a.get(..at).unwrap_or(""));
        }
    }
}

/// What an address reads, when it is an S3 one that no login has been chosen for.
struct Unresolved {
    /// The canonical endpoint it names (empty: AWS), or `None` for an older project's bare
    /// `s3://` address, which names none.
    endpoint: Option<String>,
    bucket: String,
}

/// What an unresolved address names, without inferring origin from another source's bucket.
fn unresolved(address: &str) -> Option<Unresolved> {
    let p = part(address);
    match s3_address::parse(p) {
        Ok(Some(b)) => {
            let is_legacy = b.binding == s3_address::LEGACY;
            if !b.binding.is_empty() && !is_legacy {
                return None;
            }
            let endpoint = s3::canonical(b.endpoint).ok()?.unwrap_or_default();
            let bucket = s3::bucket_of(&endpoint, b.url)?;
            let endpoint = if is_legacy { None } else { Some(endpoint) };
            Some(Unresolved { endpoint, bucket })
        }
        Ok(None) if s3::is_s3(p) => Some(Unresolved { endpoint: None, bucket: s3::parse(p).ok()?.0 }),
        _ => None,
    }
}

/// `address` read through the login `binding` of a connection at `endpoint` (empty: AWS).
fn bound(address: &str, endpoint: &str, binding: &str) -> Option<String> {
    let at = head_len(address);
    let p = address.get(at..)?;
    let inner = match s3_address::parse(p).ok().flatten() {
        Some(b) => b.url,
        None => p,
    };
    let wrapped = s3_address::bind(binding, endpoint, inner).ok()?;
    Some(format!("{}{wrapped}", address.get(..at).unwrap_or("")))
}

/// A saved connection's canonical endpoint (empty: AWS), or `None` when it has none that can be
/// used.
fn endpoint_of(c: &Connection) -> Option<String> {
    s3::canonical(&c.address).ok().map(Option::unwrap_or_default)
}

/// Whether the saved connection could read `bucket` at `endpoint` (`None`: any): an S3 one at
/// that endpoint whose buckets include it, or that names none (a scope the user chooses within,
/// not a grant).
fn matches(c: &Connection, endpoint: &Option<String>, bucket: &str) -> bool {
    if c.kind != "s3" {
        return false;
    }
    let scope = c.options.get("buckets").map(String::as_str).unwrap_or("");
    let in_scope = scope.trim().is_empty() || scope.split(',').any(|b| b.trim() == bucket);
    in_scope && endpoint.as_ref().is_none_or(|e| endpoint_of(c).as_ref() == Some(e))
}

/// A resource: the bucket some sources read, at an endpoint.
struct Group {
    endpoint: Option<String>,
    bucket: String,
}

impl Group {
    fn has(&self, address: &str) -> bool {
        unresolved(address).is_some_and(|u| u.endpoint == self.endpoint && u.bucket == self.bucket)
    }
}

/// Settles the S3 sources of the project: see the module's text. Called when a project opens
/// and when maps or layers are imported, never from a frame.
pub(crate) fn resolve_project_s3(s: &mut Session) {
    let mut groups: Vec<Group> = Vec::new();
    for a in s.project.addresses_mut() {
        if let Some(u) = unresolved(a)
            && !groups.iter().any(|g| g.endpoint == u.endpoint && g.bucket == u.bucket)
        {
            groups.push(Group { endpoint: u.endpoint, bucket: u.bucket });
        }
    }
    let mut pending = Vec::new();
    for g in &groups {
        let candidates: Vec<String> =
            s.connections.connections.iter().filter(|c| matches(c, &g.endpoint, &g.bucket)).map(|c| c.id.clone()).collect();
        if let Some(existing) = s.s3_pending.iter().find(|p| p.endpoint == g.endpoint && p.bucket == g.bucket && p.candidates == candidates)
        {
            pending.push(existing.clone());
        } else if let Some(p) = settle(s, g) {
            pending.push(p);
        }
    }
    s.s3_pending = pending;
}

/// What `g`'s sources do now: bound to its one candidate, kept for a choice, or as they were.
fn settle(s: &mut Session, g: &Group) -> Option<PendingS3> {
    let candidates: Vec<Connection> = s.connections.connections.iter().filter(|c| matches(c, &g.endpoint, &g.bucket)).cloned().collect();
    let at = match g.endpoint.as_deref() {
        Some(e) if !e.is_empty() => format!(" at {e}"),
        _ => String::new(),
    };
    match candidates.as_slice() {
        [] => {
            if g.endpoint.is_none() {
                // An older project's bare address, and no saved connection to read it: as it
                // always was (anonymously, or through a connection of this session).
                clear_bindings(s.project.addresses_mut().into_iter().filter(|a| g.has(a)));
                reopen_where(s, |a| g.has(a));
            } else {
                let n = block(s, g);
                s.notify(
                    format!("{n} source(s) read the S3 bucket '{}'{at}, and no saved connection does: add one under Data Sources › Connections, then open the project again", g.bucket),
                    true,
                );
            }
            None
        }
        [c] if c.options.get("buckets").is_some_and(|scope| scope.split(',').any(|b| b.trim() == g.bucket)) => match bind_group(s, g, c) {
            Ok(_) => None,
            Err(e) => {
                // Kept blocked, naming the one connection that could read it (an older project's
                // address now names its endpoint).
                let endpoint = endpoint_of(c).unwrap_or_default();
                block(s, g);
                let source = rewrite(s, g, &endpoint, "").into_iter().next()?;
                s.notify(
                    format!("Log in to the saved S3 connection '{}' to open the sources that read the bucket '{}': {e}", c.name, g.bucket),
                    false,
                );
                Some(PendingS3 { source, endpoint: Some(endpoint), bucket: g.bucket.clone(), candidates: vec![c.id.clone()] })
            }
        },
        many => {
            let endpoint = g.endpoint.clone().unwrap_or_default();
            let binding = if g.endpoint.is_none() { s3_address::LEGACY } else { "" };
            block(s, g);
            let source = rewrite(s, g, &endpoint, binding).into_iter().next()?;
            let notice = match many {
                [only] => format!(
                    "The saved S3 connection '{}' could read the bucket '{}'{at}, but it does not list that bucket: confirm it under Data Sources › Choose S3 connection…; nothing is read until you do",
                    only.name, g.bucket
                ),
                _ => format!(
                    "{} saved S3 connections could read the bucket '{}'{at}: choose which one reads this project's sources (Data Sources › Choose S3 connection…); nothing is read until you do",
                    many.len(),
                    g.bucket
                ),
            };
            s.notify(notice, false);
            Some(PendingS3 {
                source,
                endpoint: g.endpoint.clone(),
                bucket: g.bucket.clone(),
                candidates: many.iter().map(|c| c.id.clone()).collect(),
            })
        }
    }
}

/// Rewrites the unresolved addresses of `g` to read through `binding` at `endpoint`: the new
/// addresses, in project order.
fn rewrite(s: &mut Session, g: &Group, endpoint: &str, binding: &str) -> Vec<String> {
    let mut out = Vec::new();
    for a in s.project.addresses_mut() {
        if g.has(a)
            && let Some(new) = bound(a, endpoint, binding)
        {
            *a = new.clone();
            out.push(new);
        }
    }
    out
}

/// Marks the streamed sources of `g` as blocked for want of any connection; how many addresses
/// of `g` there are.
fn block(s: &mut Session, g: &Group) -> usize {
    for src in s.project.maps.iter_mut().flat_map(|m| m.layers.iter_mut()).filter_map(|l| l.source.as_mut()) {
        if !src.is_open() && src.path.as_deref().is_some_and(|p| g.has(p)) {
            src.error = Some(s3::UNRESOLVED.into());
        }
    }
    // A raster read in place from the bucket waits for the connection too, and says so.
    for rl in s.project.maps.iter_mut().flat_map(|m| m.layers.iter_mut()).filter_map(|l| l.raster.as_mut()) {
        if rl.in_place && rl.data.is_none() && rl.path.as_deref().is_some_and(|p| g.has(p)) {
            rl.fail(s3::UNRESOLVED);
        }
    }
    s.project.addresses_mut().into_iter().filter(|a| g.has(a)).count()
}

/// What opening a bind's sources did: how many opened, how many failed, and how many rasters read
/// in place are still reading their overview. Where nothing waits for a read in place (the
/// desktop, the browser), a raster whose overview was only asked for is reading, not opened: it
/// lands as the map draws, and one that fails is said then.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Counts {
    pub(crate) opened: usize,
    pub(crate) failed: usize,
    pub(crate) reading: usize,
}

/// Opens (again) the streamed sources whose address `which` accepts: how many opened, failed, and
/// are still reading. Rasters read in place read their overviews together (see
/// [`rustgis_map::raster::read_together`]): a bucket of several waits for the slowest, not for
/// each in turn, and on the desktop for none (they land as the map draws, and count as reading
/// until then).
fn reopen_where(s: &mut Session, which: impl Fn(&str) -> bool) -> Counts {
    let mut n = Counts::default();
    let wait = s.in_place_wait;
    for src in s.project.maps.iter_mut().flat_map(|m| m.layers.iter_mut()).filter_map(|l| l.source.as_mut()) {
        let Some(path) = src.path.clone() else { continue };
        if (!src.is_open() || src.error.is_some()) && which(&path) {
            match src.reopen() {
                Ok(()) => n.opened += 1,
                Err(_) => n.failed += 1,
            }
        }
    }
    let mut in_place = Vec::new();
    for raster in s.project.maps.iter_mut().flat_map(|m| m.layers.iter_mut()).filter_map(|l| l.raster.as_mut()) {
        if raster.data.is_none() && raster.time_zarr.is_none() && raster.path.as_deref().is_some_and(&which) {
            if raster.in_place {
                in_place.push(raster);
                continue;
            }
            match raster.reload() {
                Ok(()) => n.opened += 1,
                Err(_) => n.failed += 1,
            }
        }
    }
    let unread = rustgis_map::raster::read_together(&mut in_place, wait);
    n.failed += unread.len();
    // Counted by what is in, not by what was asked for: a read nobody waited for is still reading.
    for (i, rl) in in_place.iter().enumerate() {
        if unread.iter().any(|(j, _)| *j == i) {
            continue;
        }
        if rl.data.is_some() {
            n.opened += 1;
        } else {
            n.reading += 1;
        }
    }
    n
}

/// Binds `g`'s sources to the saved connection `c`: logs it in if it isn't, lets its login read
/// the bucket (the user's choice, or its only candidate's), rewrites the addresses to name it and
/// reads them. How many sources opened, failed, and are still reading.
fn bind_group(s: &mut Session, g: &Group, c: &Connection) -> Result<Counts, String> {
    let endpoint = endpoint_of(c).ok_or_else(|| s3::MISMATCH.to_string())?;
    let binding = crate::cmd::connections::login_saved(s, c)?;
    s3::grant_saved(&binding, &g.bucket)?;
    let addresses = rewrite(s, g, &endpoint, &binding);
    // What failed before at an identical address is read again with this login.
    for a in &addresses {
        cache::forget(part(a));
    }
    let affected = |address: &str| addresses.iter().any(|a| a == address);
    let mut n = reopen_where(s, affected);
    // Tile resources fetch as they draw, by their new address.
    n.opened +=
        s.project.maps.iter().flat_map(|m| &m.tiles).filter(|t| affected(&t.url) || t.time_url.as_deref().is_some_and(affected)).count();
    Ok(n)
}

/// The user's choice of the saved connection `id` to read the sources of the pending resource
/// that `source` is the address of (`connection.resolveS3`). The candidates are judged again
/// against the connections as they are now, before anything happens; a stale choice does nothing.
pub(crate) fn choose(s: &mut Session, source: &str, id: &str) -> crate::CmdResult {
    let Some(index) = s.s3_pending.iter().position(|p| p.source == source) else { return Err(STALE.into()) };
    let pending = s.s3_pending.get(index).cloned().ok_or(STALE)?;
    let c = s.connections.find(id).filter(|c| c.id == id && matches(c, &pending.endpoint, &pending.bucket)).cloned().ok_or(STALE)?;
    let g = Group { endpoint: pending.endpoint.clone(), bucket: pending.bucket.clone() };
    if !s.project.addresses_mut().into_iter().any(|a| g.has(a)) {
        s.s3_pending.remove(index);
        return Err(STALE.into());
    }
    let n = bind_group(s, &g, &c)?;
    s.s3_pending.remove(index);
    Ok(serde_json::json!({"id": c.id, "reopened": n.opened, "failed": n.failed, "reading": n.reading}))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    /// Whether `address` reads through the login `binding`.
    fn has_binding(address: &str, binding: &str) -> bool {
        s3_address::parse(part(address)).ok().flatten().is_some_and(|b| b.binding == binding)
    }

    const B: &str = "0123456789abcdef";

    #[test]
    fn s3_saved_template_archives_and_wrappers_are_found_inside_addresses() {
        let wrapped = s3_address::bind(B, "https://s3.example.org", "https://s3.example.org/data/a.pmtiles").unwrap();
        let template = format!("pmtiles:{{z}}/{{x}}/{{y}}@{wrapped}");
        assert_eq!(part(&template), wrapped);
        assert_eq!(part("s3://data/a.fgb"), "s3://data/a.fgb");
        let mut addresses = [template.clone(), "s3://data/b.fgb".to_string(), wrapped.clone(), "https://x.org/a".to_string()];
        clear_bindings(addresses.iter_mut());
        assert_eq!(addresses[0], "pmtiles:{z}/{x}/{y}@rustgis-s3::22:https://s3.example.org:https://s3.example.org/data/a.pmtiles");
        assert_eq!(addresses[1], "s3://data/b.fgb");
        let u = unresolved(&addresses[0]).unwrap();
        assert_eq!((u.endpoint.as_deref(), u.bucket.as_str()), (Some("https://s3.example.org"), "data"));
        let u = unresolved(&addresses[1]).unwrap();
        assert_eq!((u.endpoint, u.bucket.as_str()), (None, "data"));
        assert!(unresolved(&addresses[3]).is_none());
        // A bound address is resolved; rebinding keeps its template and inner address.
        assert!(unresolved(&wrapped).is_none());
        let again = bound(&addresses[0], "https://s3.example.org", B).unwrap();
        assert_eq!(again, template);
        assert!(has_binding(&again, B) && !has_binding(&addresses[0], B));
        let bare = bound("s3://data/b.fgb", "", B).unwrap();
        assert_eq!(bare, format!("rustgis-s3:{B}:0::s3://data/b.fgb"));
        // Malformed provenance is nobody's resource and never a file.
        assert!(unresolved("rustgis-s3:junk").is_none() && is_s3_backed("rustgis-s3:junk"));
    }
}
