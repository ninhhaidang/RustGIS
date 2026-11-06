//! Which tile to fetch next: the cache's fetch queue (#635).
//!
//! A map that pans or zooms asks for far more tiles than it ends up showing. Started all at once
//! (as the browser did before), every one of them is fetched and decoded, long after the view has
//! moved on, and the tiles the user is looking at now wait behind them. So the cache starts a
//! limited number of fetches from each server at once ([`cap`]), keeps the rest waiting, and
//! starts the newest waiting one first. A waiting tile the map has stopped asking for while it
//! went on asking for others ([`STALE`]) is dropped unfetched, and asked for again if it's wanted
//! later. A map that only waits for its tiles (an export) asks for nothing else meanwhile, so
//! nothing it waits for goes stale. Only stale tiles are ever dropped: however many wait, one
//! still asked for is fetched.
//!
//! The queue only decides; [`crate::cache`] starts the fetches and tells it when one is over.

use std::collections::HashMap;

use web_time::{Duration, Instant};

/// Fetches under way at once from one server. Browsers keep six connections to a server over
/// HTTP/1.1 and multiplex HTTP/2; natively the fetch workers' own limit.
pub(crate) const PER_HOST: usize = if cfg!(target_arch = "wasm32") { 8 } else { 16 };
/// From the OpenStreetMap Foundation's tile servers, which ask for restraint.
pub(crate) const PER_HOST_OSM: usize = if cfg!(target_arch = "wasm32") { 4 } else { 8 };
/// Reads of local archives (or uploads held in memory) under way at once.
pub(crate) const LOCAL: usize = 32;
/// A waiting tile is dropped once the map has gone this long asking for others but not for it.
pub(crate) const STALE: Duration = Duration::from_millis(1500);
/// Beyond this many tiles waiting, the stale ones are swept out at once rather than when their
/// turn comes, so a long pan doesn't pile up tiles the map no longer wants (and again each time
/// the queue doubles past what a sweep left).
pub(crate) const SWEEP_AT: usize = 2048;

/// The server an address is fetched from (`host[:port]`, lowercase), or `""` for a local read,
/// which no server limits. An archive's tile (`pmtiles:z/x/y@https://…`) is its archive's server.
pub(crate) fn host(url: &str) -> String {
    if crate::mbtiles::is_tile(url) {
        return String::new();
    }
    let Some((_, rest)) = rustgis_data::login_address::reached(rustgis_data::s3_address::reached(url)).split_once("://") else {
        return String::new();
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = rest.get(..end).unwrap_or(rest);
    // Credentials in the address aren't part of the server.
    authority.rsplit_once('@').map_or(authority, |(_, h)| h).to_ascii_lowercase()
}

/// How many fetches may be under way at once from `host`.
pub(crate) fn cap(host: &str) -> usize {
    let name = host.split(':').next().unwrap_or(host);
    if host.is_empty() {
        LOCAL
    } else if name == "tile.openstreetmap.org" || name.ends_with(".tile.openstreetmap.org") {
        PER_HOST_OSM
    } else {
        PER_HOST
    }
}

/// Stale: no longer asked for while the map asked for others since.
fn is_stale(asked: &impl Fn(&str) -> Option<Instant>, url: &str, latest: Instant) -> bool {
    asked(url).is_none_or(|at| latest.saturating_duration_since(at) > STALE)
}

/// A tile waiting to be fetched.
pub(crate) struct Waiting<T> {
    pub url: String,
    pub host: String,
    pub job: T,
}

/// What [`Queue::next`] decided about the waiting tiles.
pub(crate) struct Next<T> {
    /// Fetch these now (each already counted under way at its server).
    pub start: Vec<Waiting<T>>,
    /// Forget these: the map stopped asking for them.
    pub drop: Vec<Waiting<T>>,
}

/// The waiting tiles and the fetches under way, by server.
pub(crate) struct Queue<T> {
    waiting: Vec<Waiting<T>>,
    active: HashMap<String, usize>,
    /// How many may wait before the next sweep: twice what a sweep left, so sweeping stays
    /// linear in the tiles asked for however many are still wanted.
    sweep_at: usize,
}

impl<T> Default for Queue<T> {
    fn default() -> Self {
        Self { waiting: Vec::new(), active: HashMap::new(), sweep_at: SWEEP_AT }
    }
}

impl<T> Queue<T> {
    /// Asks for `url`: `Some` to fetch it now (counted under way), or `None` when it waits.
    /// Past [`SWEEP_AT`] waiting, those stale by `asked` and `latest` (see [`Queue::done`]) are
    /// handed back in `swept`, to forget.
    pub fn push(
        &mut self,
        url: String,
        job: T,
        asked: impl Fn(&str) -> Option<Instant>,
        latest: Instant,
        swept: &mut Vec<Waiting<T>>,
    ) -> Option<Waiting<T>> {
        let host = host(&url);
        let w = Waiting { url, host, job };
        let n = self.active.entry(w.host.clone()).or_insert(0);
        if *n < cap(&w.host) {
            *n += 1;
            return Some(w);
        }
        self.waiting.push(w);
        if self.waiting.len() > self.sweep_at {
            let (stale, kept): (Vec<_>, Vec<_>) =
                std::mem::take(&mut self.waiting).into_iter().partition(|w| is_stale(&asked, &w.url, latest));
            self.waiting = kept;
            self.sweep_at = (self.waiting.len() * 2).max(SWEEP_AT);
            swept.extend(stale);
        }
        None
    }

    /// A fetch from `host` is over: what to start in its place, newest first, and what to drop.
    /// `asked` tells when the map last asked for a waiting tile, `latest` when it last asked for any.
    pub fn done(&mut self, host: &str, asked: impl Fn(&str) -> Option<Instant>, latest: Instant) -> Next<T> {
        if let Some(n) = self.active.get_mut(host) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                self.active.remove(host);
            }
        }
        self.next(host, asked, latest)
    }

    /// The newest waiting tile of `host` to take the place it freed, and the stale tiles passed
    /// on the way to it (dropped). Only `host` has a place free (the others were full when their
    /// tiles waited), so the walk stops once it is taken: a long queue of tiles still wanted
    /// (an export) costs a step a fetch, not the whole queue.
    fn next(&mut self, host: &str, asked: impl Fn(&str) -> Option<Instant>, latest: Instant) -> Next<T> {
        let mut out = Next { start: Vec::new(), drop: Vec::new() };
        let mut i = self.waiting.len();
        while i > 0 && self.active.get(host).copied().unwrap_or(0) < cap(host) {
            i -= 1;
            let Some(w) = self.waiting.get(i) else { break };
            let stale = is_stale(&asked, &w.url, latest);
            if !stale && w.host != host {
                continue;
            }
            let w = self.waiting.remove(i);
            if stale {
                out.drop.push(w);
            } else {
                *self.active.entry(w.host.clone()).or_insert(0) += 1;
                out.start.push(w);
            }
        }
        if self.waiting.is_empty() {
            self.sweep_at = SWEEP_AT;
        }
        out
    }

    /// How many tiles are waiting.
    #[cfg(test)]
    pub fn waiting(&self) -> usize {
        self.waiting.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tile(n: usize) -> String {
        format!("https://tiles.example.com/14/{n}/0.pbf")
    }

    #[test]
    fn servers_and_their_limits() {
        assert_eq!(host("https://User:pw@Tiles.Example.com:8443/a/b?x"), "tiles.example.com:8443");
        assert_eq!(host("pmtiles:3/1/2@https://data.example.org/planet.pmtiles"), "data.example.org");
        assert_eq!(host("pmtiles:3/1/2@/home/me/planet.pmtiles"), "");
        assert_eq!(host("mbtiles:3/1/2@https://example.org/x.mbtiles"), "");
        assert_eq!(cap("a.tile.openstreetmap.org"), PER_HOST_OSM);
        assert_eq!(cap("tiles.example.com"), PER_HOST);
        assert_eq!(cap(""), LOCAL);
    }

    #[test]
    fn a_server_has_a_few_fetches_under_way_and_the_newest_waiting_goes_next() {
        let mut q = Queue::default();
        let mut overflow = Vec::new();
        let started = (0..PER_HOST + 3).filter(|&n| q.push(tile(n), n, |_| None, Instant::now(), &mut overflow).is_some()).count();
        assert_eq!(started, PER_HOST);
        assert_eq!(q.waiting(), 3);
        // Another server isn't held up by this one.
        assert!(q.push("https://other.example.net/1.png".into(), 99, |_| None, Instant::now(), &mut overflow).is_some());
        let now = Instant::now();
        let next = q.done("tiles.example.com", |_| Some(now), now);
        assert_eq!(next.start.iter().map(|w| w.job).collect::<Vec<_>>(), vec![PER_HOST + 2]);
        assert!(next.drop.is_empty());
        assert_eq!(q.waiting(), 2);
    }

    #[test]
    fn a_tile_the_map_stopped_asking_for_is_dropped_unfetched() {
        let mut q = Queue::default();
        let mut overflow = Vec::new();
        for n in 0..PER_HOST + 2 {
            let _ = q.push(tile(n), n, |_| None, Instant::now(), &mut overflow);
        }
        let then = Instant::now();
        let later = then + STALE + Duration::from_millis(1);
        let waiting_old = tile(PER_HOST);
        // The newer waiting tile is still asked for, the older one was last asked for long ago.
        let asked = |u: &str| Some(if u == waiting_old { then } else { later });
        let next = q.done("tiles.example.com", asked, later);
        assert!(next.drop.is_empty());
        assert_eq!(next.start.iter().map(|w| w.job).collect::<Vec<_>>(), vec![PER_HOST + 1]);
        // Reached on the next fetch over, the older one is dropped rather than fetched.
        let next = q.done("tiles.example.com", asked, later);
        assert_eq!(next.drop.iter().map(|w| w.job).collect::<Vec<_>>(), vec![PER_HOST]);
        assert!(next.start.is_empty());
        assert_eq!(q.waiting(), 0);
    }

    #[test]
    fn a_map_that_only_waits_keeps_its_tiles() {
        // An export asks for its tiles once and waits: nothing newer was asked for, so even long
        // after, the tiles it asked for are still wanted.
        let mut q = Queue::default();
        let mut overflow = Vec::new();
        for n in 0..PER_HOST + 4 {
            let _ = q.push(tile(n), n, |_| None, Instant::now(), &mut overflow);
        }
        let asked = Instant::now();
        for _ in 0..4 {
            let next = q.done("tiles.example.com", |_| Some(asked), asked);
            assert!(next.drop.is_empty());
            assert_eq!(next.start.len(), 1);
        }
        assert_eq!(q.waiting(), 0);
    }

    #[test]
    fn many_waiting_sweep_out_only_the_stale() {
        let mut q = Queue::default();
        let mut swept = Vec::new();
        let then = Instant::now();
        let later = then + STALE + Duration::from_millis(1);
        // The first hundred waiting were asked for long ago; the rest just now.
        let asked = |u: &str| {
            let n: usize = u.split('/').nth(4).and_then(|x| x.parse().ok()).unwrap_or(0);
            Some(if n < PER_HOST + 100 { then } else { later })
        };
        for n in 0..PER_HOST + SWEEP_AT + 5 {
            let _ = q.push(tile(n), n, asked, later, &mut swept);
        }
        assert_eq!(swept.iter().map(|w| w.job).collect::<Vec<_>>(), (PER_HOST..PER_HOST + 100).collect::<Vec<_>>());
        assert_eq!(q.waiting(), SWEEP_AT + 5 - 100);
        // Every tile still asked for waits, however many: an export loses none.
        let mut kept = Vec::new();
        for n in 0..3 * SWEEP_AT {
            let _ = q.push(format!("https://export.example.com/{n}.png"), n, |_| Some(later), later, &mut kept);
        }
        assert!(kept.is_empty());
    }
}
