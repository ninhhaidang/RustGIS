//! `host.fetch`: a plugin with the `network` permission reads a small text or JSON document
//! from a web address (an API's answer, a service's index).
//!
//! The address must be `http(s)://` and on the internet: never this computer, the local network
//! or a forbidden address (see `rustgis_tiles::address`), and no redirect may lead there. It is
//! read anonymously (never signed with an S3 connection's credentials), at most
//! [`MAX_BYTES`], and must be UTF-8. Answers are kept for a while ([`TTL`]) in a small cache,
//! so a plugin walking the same chain of documents again doesn't fetch them again.
//!
//! Natively the request waits for the answer, at most [`TIMEOUT`] each and [`CALL_BUDGET`] for
//! all of one plugin call's fetches. In the browser nothing may wait: the fetch starts, the
//! request answers `{"pending": true, "url"}`, and once every pending answer has landed the
//! plugin command runs again from the start ([`Session::poll_plugin_retries`]), when the cache
//! answers at once.
//!
//! A plugin command whose `layer.addUrl` left a download to the app (the browser, and the
//! desktop app since #1170) runs again the same way once the app has added it (#1202): its
//! `layer.addUrl` then answers what adding the download answered, so the command goes on with
//! the layer (clips, renames or styles it).

use std::collections::VecDeque;
use std::sync::Mutex;

use serde_json::{Value, json};
use web_time::{Duration, Instant};

use crate::render::rustgis_tiles::{address, s3};
use crate::{CmdResult, Session};

/// The most bytes a fetched document may have.
pub const MAX_BYTES: usize = 2 << 20;
/// How long one fetch may take (natively).
pub const TIMEOUT: Duration = Duration::from_secs(20);
/// How long all the fetches of one plugin call may take together (natively): the app waits.
pub const CALL_BUDGET: Duration = Duration::from_secs(60);
/// The most fetches one plugin call may make (cached answers included).
pub const MAX_PER_CALL: usize = 64;
/// How long a fetched answer is kept.
pub const TTL: Duration = Duration::from_secs(10 * 60);
/// How long a failed fetch's error is kept: long enough for every command waiting for it to
/// see it (the browser), short enough that a later try fetches again.
pub const ERROR_TTL: Duration = Duration::from_secs(30);
/// The most answers kept, and their bytes.
const CACHE_ENTRIES: usize = 256;
const CACHE_BYTES: usize = 16 << 20;
/// The browser: the most fetches one call may leave pending, and how many times a command is
/// run again for its fetches (a chain of documents, one a run) before it is given up.
pub const MAX_PENDING: usize = 8;
pub const MAX_RETRIES: u32 = 64;
/// The browser: how long a fetch is waited for before it is taken as lost (a request that
/// never answers), so the commands waiting for it run again and it can be fetched again.
const IN_FLIGHT_FOR: Duration = Duration::from_secs(2 * 60);
/// Plugin commands waiting for their fetches.
const MAX_WAITING: usize = 8;
/// The longest address accepted.
const MAX_URL: usize = 8 * 1024;

struct Entry {
    url: String,
    at: Instant,
    size: usize,
    answer: Result<Value, String>,
}

#[derive(Default)]
struct Cache {
    entries: VecDeque<Entry>,
    /// Addresses being fetched off the frame (the browser), and since when.
    in_flight: Vec<(String, Instant)>,
}

impl Cache {
    /// The kept answer for `url`. A failed fetch's error is kept for [`ERROR_TTL`], so every
    /// command waiting for it sees it, and fetched again after that.
    fn get(&mut self, url: &str) -> Option<Result<Value, String>> {
        self.entries.retain(|e| e.at.elapsed() < if e.answer.is_ok() { TTL } else { ERROR_TTL });
        let i = self.entries.iter().position(|e| e.url == url)?;
        let e = self.entries.remove(i)?;
        let answer = e.answer.clone();
        // Kept answers move to the back: the least recently used go first.
        self.entries.push_back(e);
        Some(answer)
    }

    fn keep(&mut self, url: &str, answer: Result<Value, String>) {
        // About the bytes kept: an answer holds its text twice, as `text` and parsed as `json`.
        let size = url.len() + answer.as_ref().map_or_else(String::len, |v| v.get("text").and_then(Value::as_str).map_or(0, str::len) * 2);
        self.in_flight.retain(|(u, _)| u != url);
        self.entries.retain(|e| e.url != url);
        self.entries.push_back(Entry { url: url.to_string(), at: Instant::now(), size, answer });
        let mut bytes: usize = self.entries.iter().map(|e| e.size).sum();
        while self.entries.len() > CACHE_ENTRIES || bytes > CACHE_BYTES {
            let Some(e) = self.entries.pop_front() else { break };
            bytes = bytes.saturating_sub(e.size);
        }
    }

    /// Whether `url` is no longer being fetched, so a command waiting for it can run again. Its
    /// answer is usually kept by then; one already dropped (evicted, or an error expired while
    /// frames were throttled) is fetched again by the run, so a waiting command never sticks.
    /// Nor does one whose fetch never answers: after [`IN_FLIGHT_FOR`] it counts as landed.
    fn landed(&self, url: &str) -> bool {
        !self.in_flight.iter().any(|(u, at)| u == url && at.elapsed() < IN_FLIGHT_FOR)
    }
}

fn cache() -> std::sync::MutexGuard<'static, Cache> {
    static CACHE: Mutex<Cache> = Mutex::new(Cache { entries: VecDeque::new(), in_flight: Vec::new() });
    // A poisoned lock still holds a usable cache: at worst an entry is missing.
    CACHE.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
thread_local! {
    /// Tests: let the plugin fetch from this computer (a test server) and, when `wait` is off,
    /// fetch as the browser does (pending, then run again).
    pub(crate) static TEST: std::cell::Cell<(bool, bool)> = const { std::cell::Cell::new((false, true)) };
}

fn allow_local() -> bool {
    #[cfg(test)]
    {
        TEST.with(|t| t.get().0)
    }
    #[cfg(not(test))]
    {
        false
    }
}

/// Whether a fetch waits for its answer (natively) or answers pending (the browser).
fn waits() -> bool {
    #[cfg(test)]
    {
        TEST.with(|t| t.get().1)
    }
    #[cfg(not(test))]
    {
        cfg!(not(target_arch = "wasm32"))
    }
}

/// The address a plugin may fetch, as fetched: `http(s)://`, on the internet, no fragment.
pub fn checked(url: &str) -> Result<String, String> {
    let url = url.trim();
    if url.is_empty() {
        return Err("url is required".into());
    }
    if url.len() > MAX_URL {
        return Err(format!("the address is longer than {MAX_URL} characters"));
    }
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("https://") || lower.starts_with("http://")) {
        let shown: String = url.chars().take(80).collect();
        return Err(format!("{shown}: plugins fetch only http:// and https:// addresses"));
    }
    // A fragment never reaches the server (and RustGIS marks byte ranges with one).
    let url = url.split('#').next().unwrap_or(url);
    address::check(url, None)?;
    if address::reach(url) != address::Reach::Public && !allow_local() {
        return Err(format!("{url}: plugins fetch only from the internet, not this computer or the local network"));
    }
    Ok(url.to_string())
}

/// The answer for a fetched document: `{url, text}`, and `json` when the text is a JSON object
/// or array.
pub fn answer(url: &str, bytes: &[u8]) -> Result<Value, String> {
    if bytes.len() > MAX_BYTES {
        return Err(format!("{url}: larger than {MAX_BYTES} bytes"));
    }
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let text = std::str::from_utf8(bytes).map_err(|_| format!("{url}: the answer isn't UTF-8 text"))?;
    let trimmed = text.trim_start();
    let parsed = if trimmed.starts_with('{') || trimmed.starts_with('[') { serde_json::from_str::<Value>(text).ok() } else { None };
    Ok(match parsed {
        Some(v) => json!({"url": url, "text": text, "json": v}),
        None => json!({"url": url, "text": text}),
    })
}

/// The cache's key for `plugin`'s fetch of `url`: each plugin has answers of its own, so one
/// can't read what another fetched (an address may carry a token).
fn key(plugin: &str, url: &str) -> String {
    format!("{plugin} {url}")
}

/// Starts fetching `url` off the frame under `key`, unless it is already under way.
fn start(key: &str, url: &str) {
    {
        let mut c = cache();
        if !c.landed(key) {
            return;
        }
        // A fetch taken as lost is started again.
        c.in_flight.retain(|(u, _)| u != key);
        c.in_flight.push((key.to_string(), Instant::now()));
    }
    let (key, url) = (key.to_string(), url.to_string());
    crate::render::rustgis_tiles::cache::download_limited(
        &s3::anonymous(&url),
        MAX_BYTES,
        Box::new(move |r| {
            let answer = r.map_err(|e| format!("{url}: {e}")).and_then(|bytes| answer(&url, &bytes));
            cache().keep(&key, answer);
        }),
    );
}

/// What one plugin call has fetched.
#[derive(Default)]
pub(super) struct Calls {
    /// The plugin fetching: the cache keeps each plugin's answers apart.
    plugin: String,
    count: usize,
    started: Option<Instant>,
    /// Cache keys of the answers pending (the browser).
    pub(super) pending: Vec<String>,
}

impl Calls {
    pub(super) fn for_plugin(plugin: &str) -> Calls {
        Calls { plugin: plugin.to_string(), ..Calls::default() }
    }

    /// Runs `host.fetch {url}` for a plugin that may.
    pub(super) fn fetch(&mut self, p: &Value) -> CmdResult {
        let url = checked(p.get("url").and_then(Value::as_str).ok_or("url is required")?)?;
        if self.count >= MAX_PER_CALL {
            return Err(format!("a plugin call may fetch at most {MAX_PER_CALL} documents"));
        }
        self.count += 1;
        let key = key(&self.plugin, &url);
        let kept = cache().get(&key);
        if let Some(answer) = kept {
            return answer;
        }
        if !waits() {
            if !self.pending.contains(&key) {
                if self.pending.len() >= MAX_PENDING {
                    return Err(format!("a plugin call may wait for at most {MAX_PENDING} documents at once"));
                }
                self.pending.push(key.clone());
                start(&key, &url);
            }
            return Ok(json!({"pending": true, "url": url}));
        }
        self.blocking(&key, &url)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn blocking(&mut self, key: &str, url: &str) -> CmdResult {
        let started = *self.started.get_or_insert_with(Instant::now);
        let left = CALL_BUDGET.saturating_sub(started.elapsed());
        if left.is_zero() {
            return Err(format!("{url}: this plugin call's fetches took longer than {} s in all", CALL_BUDGET.as_secs()));
        }
        // Names are checked again where they resolve: a public-looking name for a local address
        // (or a redirect to one) is refused there. Tests fetch from a server on this computer.
        let (fetched, within) = (&s3::anonymous(url), TIMEOUT.min(left));
        let bytes = if allow_local() {
            crate::render::rustgis_tiles::cache::download_blocking_within(fetched, MAX_BYTES, within)
        } else {
            crate::render::rustgis_tiles::cache::download_public_within(fetched, MAX_BYTES, within)
        };
        // Errors are kept too (for `ERROR_TTL`), so a plugin run again and again on an address
        // that doesn't answer doesn't wait for it each time.
        let answer = bytes.map_err(|e| format!("{url}: {e}")).and_then(|bytes| answer(url, &bytes));
        cache().keep(key, answer.clone());
        answer
    }

    #[cfg(target_arch = "wasm32")]
    fn blocking(&mut self, _key: &str, url: &str) -> CmdResult {
        let _ = self.started;
        Err(format!("{url}: the web app can't wait for a fetch"))
    }
}

/// How long a plugin command waits for a download it left to the app before it runs again
/// with an error: one the app lost track of doesn't keep it waiting for ever.
pub const REQUEST_WAIT: Duration = Duration::from_secs(10 * 60);

/// A plugin command waiting for its fetches, or its downloads, before it runs again.
pub(crate) struct Retry {
    id: String,
    params: Value,
    urls: Vec<String>,
    /// The downloads it left to the app ([`Session::request_url`]), by request number.
    requests: Vec<u64>,
    /// What the downloads added for its earlier runs answered (what was asked, and the answer):
    /// a run that waits again for another download keeps them, so the next run doesn't ask for
    /// those again.
    answered: Vec<(Value, CmdResult, Added)>,
    /// The errors its earlier runs said, which stay though the runs were undone: the next run does
    /// the same again, and doesn't repeat them.
    said: Vec<String>,
    tries: u32,
}

/// The downloads plugin commands wait for (#1202): those the app is making, with what was
/// asked and since when, and those added, with what adding them answered, until the command
/// waiting for them runs again.
#[derive(Default)]
pub(crate) struct UrlWaits {
    in_flight: Vec<(u64, Value, Instant)>,
    landed: Vec<(u64, Value, CmdResult, Added)>,
    /// Requests given up (past [`REQUEST_WAIT`], or their command replaced or dropped), with their
    /// addresses (about [`MAX_EXPIRED`] at most, see `remember_given_up`): one that lands after is
    /// refused, not added with no command left to go on with it.
    expired: Vec<(u64, Value)>,
    /// Requests added, with their addresses (the latest [`MAX_EXPIRED`]): one landing again (a
    /// caller gave its number with the same address, then the app's own download came) isn't
    /// added twice.
    added: VecDeque<(u64, Value)>,
}

/// What adding a landed download added, to tell when its command runs again whether it is still
/// there: an undo may have taken it away, and a layer added since may then have its id.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Added {
    /// The layers, by id, with their instance ids (fresh for each layer added, kept by a rename
    /// and by undo): a layer added since with the same id, even of a file of the same name, isn't
    /// them.
    layers: Vec<(u32, u64)>,
    /// The tile layers (a GeoTIFF read in place), as added: told apart by [`same_tile`].
    tiles: Vec<rustgis_map::tiles::TileLayer>,
    /// The map it was added to (its instance id, which stays with it as maps are removed or
    /// moved), which may not be the one shown when the command runs again.
    pub(crate) map: u64,
}

impl Added {
    /// What `answer` (adding a download) added to `map`, just after.
    pub(crate) fn of(map: &crate::map::Map, answer: &CmdResult) -> Added {
        let at = map.instance_id();
        let Ok(v) = answer else { return Added { map: at, ..Added::default() } };
        let ids = v.get("layers").and_then(Value::as_array).into_iter().flatten().chain(v.get("layer"));
        let mut layers: Vec<(u32, u64)> = Vec::new();
        for id in ids.filter_map(Value::as_u64).filter_map(|id| u32::try_from(id).ok()) {
            if let Some(l) = map.layer(id).filter(|_| !layers.iter().any(|(m, ..)| *m == id)) {
                layers.push((id, l.instance_id()));
            }
        }
        // A tile layer's answer is its place in the list, which changes: its address doesn't.
        let tiles = match (v.get("layer"), v.get("index").and_then(Value::as_u64)) {
            (None, Some(i)) => usize::try_from(i).ok().and_then(|i| map.tiles.get(i)).cloned().into_iter().collect(),
            _ => Vec::new(),
        };
        Added { layers, tiles, map: at }
    }

    /// `answer` as it stands in `map` now: a tile layer's place in the list, which moves as others
    /// are added or taken away, is where it is now.
    pub(crate) fn answer_now(&self, map: &crate::map::Map, answer: &CmdResult) -> CmdResult {
        let mut now = answer.clone();
        if let (Ok(v), Some(added)) = (&mut now, self.tiles.first())
            && v.get("index").is_some()
            && let Some(i) = map.tiles.iter().position(|t| same_tile(t, added))
        {
            v["index"] = json!(i);
        }
        now
    }

    /// Whether it holds anything: a failed download, or one whose answer names nothing, added
    /// nothing.
    pub(crate) fn added_any(&self) -> bool {
        !(self.layers.is_empty() && self.tiles.is_empty())
    }

    /// Whether any of it is no longer in `map` (or another layer has its id).
    pub(crate) fn gone(&self, map: &crate::map::Map) -> bool {
        self.layers.iter().any(|(id, instance)| map.layer(*id).is_none_or(|l| l.instance_id() != *instance))
            || self.tiles.iter().any(|added| !map.tiles.iter().any(|t| same_tile(t, added)))
    }
}

/// Whether `answer` added a standalone table (a file with no geometry) that is gone, by its id,
/// which no table gets again.
pub(crate) fn table_gone(answer: &CmdResult, project: &crate::project::Project) -> bool {
    let table = answer.as_ref().ok().filter(|v| v.get("table").is_some()).and_then(|v| v.get("id")).and_then(Value::as_u64);
    table.is_some_and(|id| !project.tables.iter().any(|t| u64::from(t.id) == id))
}

/// Whether tile layer `t` is `added`, which has no id: the same address, kind, role and look. A
/// map can't hold two with all of these (adding one replaces the other), so another layer of the
/// same address isn't it, and a rename keeps it.
fn same_tile(t: &rustgis_map::tiles::TileLayer, added: &rustgis_map::tiles::TileLayer) -> bool {
    t.url == added.url && t.kind == added.kind && t.overlay == added.overlay && t.style_layers == added.style_layers
}

/// What a notice of a run replaced or dropped adds for the downloads only it asked for that
/// were never started (`cancelled`): nothing when there are none.
fn not_started(cancelled: &[String]) -> String {
    match cancelled {
        [] => String::new(),
        [one] => format!("; its download of {} wasn't started, and won't be", crate::sources::redact(one)),
        many => {
            let shown: Vec<_> = many.iter().map(|u| crate::sources::redact(u)).collect();
            format!("; its {} downloads not yet started won't be: {}", many.len(), shown.join(", "))
        }
    }
}

/// The most given-up requests whose downloads are still out: each is remembered until its
/// download lands (and is refused) or fails, and while this many are, plugin commands ask for no
/// more downloads.
const MAX_EXPIRED: usize = 1024;

/// Whether `params` (a front end's `layer.addUrl` or `layer.addDownload`) are what `asked`
/// asked for: the same address (spaces around it set aside), and every other key it gave that
/// the front end passes on, with the same value. What the front end leaves out or sets itself,
/// and what the app adds of its own (`id`), don't count. Another import of the same address
/// (other columns, another name) isn't it.
fn asked_by(asked: &Value, params: &Value) -> bool {
    let Some(o) = asked.as_object() else { return false };
    // What the front end passes on to a download's own `layer.addUrl` (the desktop app's
    // `ui.addUrl`): only these come back, and must come back the same. Any other key (`whole`,
    // `cog`, `dates`, a `name` that isn't text…) it leaves out. `zoom` it passes on, but may change
    // (a link's data, once zoomed to), so it isn't compared.
    let passed_on = |k: &str, v: &Value| match k {
        "name" | "layer" | "crs" | "connection" => v.as_str().is_some_and(|s| !s.trim().is_empty()),
        "x" | "y" | "wkt" | "delimiter" | "paperSpace" | "stream" | "as" => !v.is_null(),
        "service" | "coverage" | "anonymous" => v == &json!(true),
        "bounds" => v.is_array(),
        _ => false,
    };
    let given = o.iter().all(|(k, v)| match k.as_str() {
        "url" => v.as_str().map(str::trim) == params["url"].as_str().map(str::trim),
        _ if passed_on(k, v) => params.get(k) == Some(v),
        _ => true,
    });
    // ... and nothing it passes on that wasn't asked for (another import of the address).
    given && params.as_object().is_none_or(|p| p.iter().all(|(k, v)| k == "url" || !passed_on(k, v) || o.get(k) == Some(v)))
}

impl UrlWaits {
    /// The number of a download being made for `asked`, or made and added while its command
    /// waits to run again: a request made again then is that one, not another download.
    pub(crate) fn in_flight(&self, asked: &Value) -> Option<u64> {
        // One that failed added nothing: asked again, it is downloaded again.
        let made = self.landed.iter().find(|(_, q, answer, _)| answer.is_ok() && same_request(q, asked)).map(|(n, ..)| *n);
        self.in_flight.iter().find(|(_, q, _)| same_request(q, asked)).map(|(n, ..)| *n).or(made)
    }

    /// Whether what request `n` added once it landed is no longer in the project.
    pub(crate) fn taken_away(&self, n: u64, project: &crate::project::Project) -> bool {
        self.landed.iter().any(|(m, _, answer, added)| {
            *m == n
                && (table_gone(answer, project)
                    || (added.added_any()
                        && project.maps.iter().find(|map| map.instance_id() == added.map).is_none_or(|map| added.gone(map))))
        })
    }

    /// Whether a front end's `params` (a `layer.addUrl` or `layer.addDownload` carrying number
    /// `n`) are what request `n` asked for: the same address. A number given with another address
    /// matches nothing, so no caller can land another command's wait.
    pub(crate) fn holds(&self, n: u64, params: &Value) -> bool {
        self.in_flight.iter().any(|(m, q, _)| *m == n && asked_by(q, params))
    }

    /// Whether request `n` is waited for (being made, or landed).
    pub(crate) fn has(&self, n: u64) -> bool {
        self.in_flight.iter().any(|(m, ..)| *m == n) || self.landed.iter().any(|(m, ..)| *m == n)
    }

    /// Request `n` landed, as `answer`, having `added` that: kept when a plugin command waits for it.
    pub(crate) fn land(&mut self, n: u64, answer: CmdResult, added: Added) {
        if let Some(i) = self.in_flight.iter().position(|(m, ..)| *m == n) {
            let (_, asked, _) = self.in_flight.remove(i);
            if answer.is_ok() {
                if self.added.len() >= MAX_EXPIRED {
                    self.added.pop_front();
                }
                self.added.push_back((n, asked.clone()));
            }
            self.landed.push((n, asked, answer, added));
        }
    }

    /// Whether `params` land request `n` again: it was added already.
    pub(crate) fn added_already(&self, n: u64, params: &Value) -> bool {
        self.added.iter().any(|(m, q)| *m == n && asked_by(q, params))
    }

    /// Waits for request `n`, which asked for `asked`.
    fn wait(&mut self, n: u64, asked: Value) {
        if !self.has(n) {
            self.in_flight.push((n, asked, Instant::now()));
        }
    }

    /// Downloads waited for past [`REQUEST_WAIT`] land as failed, and are remembered as given up:
    /// their addresses, redacted, to be said (a notice with a key in it loses what follows it).
    fn expire(&mut self) -> Vec<String> {
        let late: Vec<(u64, Value)> =
            self.in_flight.iter().filter(|(.., at)| at.elapsed() >= REQUEST_WAIT).map(|(n, q, _)| (*n, q.clone())).collect();
        let mut addresses = Vec::new();
        for (n, asked) in late {
            self.land(n, Err(format!("no download in {} minutes", REQUEST_WAIT.as_secs() / 60)), Added::default());
            addresses.extend(asked["url"].as_str().map(crate::sources::redact));
            self.remember_given_up(n, asked);
        }
        addresses
    }

    /// Request `n`, no command waiting for it any more, is given up: forgotten, and remembered
    /// so a late landing is refused.
    /// `taken`: whether the front end took it (it may still land); one it didn't is just forgotten.
    fn give_up(&mut self, n: u64, taken: bool) {
        if let Some(i) = self.in_flight.iter().position(|(m, ..)| *m == n) {
            let (_, asked, _) = self.in_flight.remove(i);
            if taken {
                self.remember_given_up(n, asked);
            }
        }
        self.landed.retain(|(m, ..)| *m != n);
    }

    /// Kept until its download lands (and is refused) or fails: never forgotten while it can
    /// still land. [`MAX_EXPIRED`] bounds them by refusing new downloads instead, so those under
    /// way by then can still take them a little past it.
    fn remember_given_up(&mut self, n: u64, asked: Value) {
        self.expired.push((n, asked));
    }

    /// Whether as many given-up downloads are still out as are remembered ([`MAX_EXPIRED`]).
    fn given_up_full(&self) -> bool {
        self.expired.len() >= MAX_EXPIRED
    }

    /// Whether `params` land a request that was given up: its command no longer waits for it.
    pub(crate) fn given_up(&self, n: u64, params: &Value) -> bool {
        self.expired.iter().any(|(m, q)| *m == n && asked_by(q, params))
    }

    /// A given-up request's late download was refused: nothing more to remember of it.
    pub(crate) fn refused(&mut self, n: u64) {
        self.expired.retain(|(m, _)| *m != n);
    }

    /// Only what `waiting` still waits for is kept.
    fn keep_only(&mut self, waiting: &[Retry]) {
        let wanted = |n: &u64| waiting.iter().any(|r| r.requests.contains(n));
        self.in_flight.retain(|(n, ..)| wanted(n));
        self.landed.retain(|(n, ..)| wanted(n));
    }
}

/// A download request without its number: what a command asked for.
pub(crate) fn without_request(p: &Value) -> Value {
    let mut p = p.clone();
    if let Some(o) = p.as_object_mut() {
        o.remove("request");
    }
    p
}

/// Whether two download requests ask for the same thing, whatever their numbers, and whatever
/// spaces are around their addresses (the app downloads the address trimmed).
pub(crate) fn same_request(a: &Value, b: &Value) -> bool {
    let asked = |p: &Value| {
        let mut p = without_request(p);
        if let Some(u) = p.get("url").and_then(Value::as_str).map(|u| u.trim().to_string()) {
            p["url"] = json!(u);
        }
        p
    };
    asked(a) == asked(b)
}

impl Session {
    /// What a call for download `p` asks for: without its number, and read with no saved login
    /// when it comes from a plugin's command. The front end runs the queued request after that
    /// scope has ended, so the opt-out goes with it through probes, whole-file fallbacks and the
    /// eventual `layer.addDownload`; a command run again asks for the same.
    pub(crate) fn asked_for(&self, p: &Value) -> Value {
        let mut asked = without_request(p);
        if self.web_logins.is_anonymous()
            && let Some(o) = asked.as_object_mut()
        {
            o.insert("anonymous".into(), json!(true));
        }
        asked
    }

    /// Keeps plugin command `id` to run again once `urls` (cache keys) have landed, and the
    /// downloads it left to the app (`requests`: their numbers and what was asked) have been
    /// added: its answer, which says so.
    pub(super) fn retry_plugin(&mut self, id: &str, params: &Value, urls: Vec<String>, requests: Vec<(u64, Value)>) -> CmdResult {
        let tries = self.retry_tries;
        if tries >= MAX_RETRIES {
            // This call's downloads went with its undo, and aren't queued again; one another run
            // waits for still comes.
            let dropped: Vec<String> =
                requests.iter().filter(|(n, _)| !self.url_waits.has(*n)).filter_map(|(_, q)| q["url"].as_str().map(String::from)).collect();
            return Err(format!("{id}: gave up after fetching {MAX_RETRIES} times{}", not_started(&dropped)));
        }
        // Past the given-up downloads that can still land, no more are asked for: each must be
        // remembered until it lands, to be refused then. Those not yet taken by the front end
        // aren't downloaded. It is refused before any wait changes, so the command's earlier run
        // and the others still wait.
        let fresh = |s: &Self, n: &u64| !s.url_waits.has(*n) && !s.plugin_retries.iter().any(|r| r.requests.contains(n));
        if requests.iter().any(|(n, _)| fresh(self, n)) && self.url_waits.given_up_full() {
            let mut cancelled = Vec::new();
            for (n, _) in &requests {
                if fresh(self, n) {
                    cancelled.extend(
                        self.url_requests
                            .iter()
                            .filter(|q| q["request"].as_u64() == Some(*n))
                            .filter_map(|q| q["url"].as_str().map(String::from)),
                    );
                    self.url_requests.retain(|q| q["request"].as_u64() != Some(*n));
                }
            }
            return Err(format!(
                "{id}: {MAX_EXPIRED} downloads plugin commands stopped waiting for are still under way; run it again once they have come in{}",
                not_started(&cancelled)
            ));
        }
        // One waiting run per command: the latest call's, which replaces an earlier one. The
        // downloads a replaced or dropped run alone waited for are given up, so they aren't added
        // with no command to go on with them.
        let keeps: Vec<u64> = requests.iter().map(|(n, _)| *n).collect();
        if let Some(i) = self.plugin_retries.iter().position(|r| r.id == id) {
            let old = self.plugin_retries.remove(i);
            let cancelled = self.give_up_requests(&old, &keeps);
            self.notify(
                format!("{id}: an earlier run still waiting for the web was replaced by this one{}", not_started(&cancelled)),
                false,
            );
        }
        if self.plugin_retries.len() >= MAX_WAITING {
            let dropped = self.plugin_retries.remove(0);
            let cancelled = self.give_up_requests(&dropped, &keeps);
            let said = format!("{}: dropped while waiting for the web (too many plugin commands waiting)", dropped.id);
            self.notify(format!("{said}{}", not_started(&cancelled)), true);
        }
        // `urls` are cache keys (`<plugin> <url>`; plugin ids have no spaces): the answer names
        // the addresses, without their keys: the assistant and MCP get it as it is.
        let fetching: Vec<String> = urls.iter().map(|k| crate::sources::redact(k.split_once(' ').map_or(k.as_str(), |(_, u)| u))).collect();
        let mut answer = json!({"pending": true});
        if !fetching.is_empty() {
            answer["fetching"] = json!(fetching);
        }
        if !requests.is_empty() {
            answer["downloading"] = json!(requests.iter().map(|(_, q)| q["url"].as_str().map(crate::sources::redact)).collect::<Vec<_>>());
        }
        let numbers = requests.iter().map(|(n, _)| *n).collect();
        for (n, asked) in requests {
            self.url_waits.wait(n, asked);
        }
        // What this run already had answered (it is a rerun waiting again): kept for the next.
        let answered = self.rerun_landed.clone();
        let said = self.rerun_said.clone();
        self.plugin_retries.push(Retry {
            id: id.to_string(),
            params: params.clone(),
            urls,
            requests: numbers,
            answered,
            said,
            tries: tries + 1,
        });
        Ok(answer)
    }

    /// Gives up the downloads `gone` waited for that neither `keeps` (the run replacing it) nor
    /// another waiting run still waits for: the addresses of those not started, which are never
    /// downloaded now. (One under way is refused, and said, when it lands.)
    fn give_up_requests(&mut self, gone: &Retry, keeps: &[u64]) -> Vec<String> {
        let mut cancelled = Vec::new();
        for n in &gone.requests {
            if !keeps.contains(n) && !self.rerun_requests.contains(n) && !self.plugin_retries.iter().any(|r| r.requests.contains(n)) {
                // Not yet taken by the front end: not downloaded at all, so nothing will land it.
                let queued: Vec<String> = self
                    .url_requests
                    .iter()
                    .filter(|q| q["request"].as_u64() == Some(*n))
                    .filter_map(|q| q["url"].as_str().map(String::from))
                    .collect();
                self.url_waits.give_up(*n, queued.is_empty());
                self.url_requests.retain(|q| q["request"].as_u64() != Some(*n));
                cancelled.extend(queued);
            }
        }
        cancelled
    }

    /// Whether plugin commands are waiting for their fetches or downloads, so the front end
    /// keeps polling.
    pub fn plugin_retries_waiting(&self) -> bool {
        !self.plugin_retries.is_empty()
    }

    /// Runs again the plugin commands whose fetches have all landed and whose downloads the app
    /// has all added (see `host.fetch` and `layer.addUrl` in `docs/plugins.md`): each command and
    /// what it answered this time. Its `layer.addUrl` calls answer what adding each download
    /// answered. Errors are also given as notices, as for any command.
    pub fn poll_plugin_retries(&mut self) -> Vec<(String, CmdResult)> {
        if self.plugin_retries.is_empty() {
            return Vec::new();
        }
        // Said here too: the command may go on without it, and what it answers goes nowhere.
        let minutes = REQUEST_WAIT.as_secs() / 60;
        match self.url_waits.expire().as_slice() {
            [] => {}
            [url] => self.notify(format!("{url}: no download in {minutes} minutes, so its plugin command stopped waiting for it"), true),
            many => self.notify(
                format!(
                    "{} downloads gave nothing in {minutes} minutes, so their plugin commands stopped waiting for them: {}",
                    many.len(),
                    many.join(", ")
                ),
                true,
            ),
        }
        let (ready, waiting): (Vec<Retry>, Vec<Retry>) = std::mem::take(&mut self.plugin_retries).into_iter().partition(|r| {
            let c = cache();
            r.urls.iter().all(|u| c.landed(u)) && r.requests.iter().all(|n| self.url_waits.landed.iter().any(|(m, ..)| m == n))
        });
        self.plugin_retries = waiting;
        // A command run before them may drop or replace one waiting for the same download: still
        // theirs, not given up.
        self.rerun_requests = ready.iter().flat_map(|r| r.requests.iter().copied()).collect();
        let ran = ready
            .into_iter()
            .map(|r| {
                // What its downloads answered, for its `layer.addUrl` calls: those of its earlier
                // runs, and the ones it waited for. These stay in `landed` until no command waits
                // for them (`keep_only`, below), so another command waiting for the same download
                // gets the answer too.
                let mine = self
                    .url_waits
                    .landed
                    .iter()
                    .filter(|(n, ..)| r.requests.contains(n))
                    .map(|(_, asked, answer, added)| (asked.clone(), answer.clone(), added.clone()));
                self.rerun_landed = r.answered.iter().cloned().chain(mine).collect();
                self.rerun_said = r.said;
                self.retry_tries = r.tries;
                // Run as the app runs a plugin's command: what it asks for now is left to the app.
                let res = self.run_leaving_downloads(&r.id, &r.params);
                self.retry_tries = 0;
                self.rerun_landed.clear();
                self.rerun_said.clear();
                (r.id, res)
            })
            .collect();
        self.rerun_requests.clear();
        // Downloads no command waits for any more (it was run again, replaced or dropped).
        let waiting = std::mem::take(&mut self.plugin_retries);
        self.url_waits.keep_only(&waiting);
        self.plugin_retries = waiting;
        ran
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A command run again checks that what its download added is still there: a layer with its
    /// id but other data (one added after an undo), a tile layer taken away (another of its
    /// address left), or a table taken away, is said, not answered (#1202).
    #[test]
    fn a_rerun_says_when_what_its_download_added_is_gone() {
        let mut s = Session::new(crate::sample::world_project());
        s.leave_downloads = true;
        let asked = json!({"url": "https://x.org/a.geojson"});
        let id = s.map().layers[0].id;
        let answer: CmdResult = Ok(json!({"layer": id}));
        let added = Added::of(s.map(), &answer);
        assert!(added.layers.len() == 1 && !added.gone(s.map()), "{added:?}");
        s.rerun_landed = vec![(asked.clone(), answer.clone(), added)];
        assert_eq!(s.run("layer.addUrl", &asked).unwrap(), json!({"layer": id}));
        // Another map shown while the command waited: it would act on that one, and says so;
        // shown again, it is answered.
        s.run("map.new", &json!({"name": "Second"})).unwrap();
        s.run("map.activate", &json!({"name": "Second"})).unwrap();
        let e = s.run("layer.addUrl", &asked).unwrap_err();
        let first = s.project.maps[0].name.clone();
        assert!(e.contains(&format!("added to map '{first}', not the map shown now")), "{e}");
        // Its map deleted meanwhile: there is no showing it.
        let deleted = s.project.maps.remove(0);
        s.project.active_map = 0;
        let e = s.run("layer.addUrl", &asked).unwrap_err();
        assert!(e.contains("added to a map since taken away") && !e.contains("show that map"), "{e}");
        s.project.maps.insert(0, deleted);
        s.project.active_map = 1;
        // A download that failed added nothing: its own error, whichever map is shown.
        let failed: CmdResult = Err("https://x.org/a.geojson: 404 Not Found".into());
        let landed = s.rerun_landed.clone();
        s.rerun_landed = vec![(asked.clone(), failed.clone(), Added::of(&s.project.maps[0], &failed))];
        assert_eq!(s.run("layer.addUrl", &asked).unwrap_err(), "https://x.org/a.geojson: 404 Not Found");
        s.rerun_landed = landed;
        // A map put before it: still known as the map it was added to, not by its place.
        s.project.maps.insert(0, crate::map::Map::new("Before"));
        s.project.active_map = 1;
        assert_eq!(s.run("layer.addUrl", &asked).unwrap(), json!({"layer": id}));
        s.project.active_map = 0;
        let e = s.run("layer.addUrl", &asked).unwrap_err();
        assert!(e.contains(&format!("added to map '{first}', not the map shown now")), "{e}");
        s.project.maps.remove(0);
        s.run("map.activate", &json!({"name": first})).unwrap();
        assert_eq!(s.run("layer.addUrl", &asked).unwrap(), json!({"layer": id}));
        // Renamed while the command waited: still it.
        s.run("layer.rename", &json!({"layer": id, "name": "Renamed"})).unwrap();
        assert_eq!(s.run("layer.addUrl", &asked).unwrap(), json!({"layer": id}));
        // Another layer has its id now, of a file of the same name (its source is the same): not it.
        let added = s.rerun_landed[0].2.clone();
        let mut other = s.map().layer(id).unwrap().clone();
        other.renew_instance_id();
        let at = s.map().layers.iter().position(|l| l.id == id).unwrap();
        s.map_mut().layers[at] = other;
        s.rerun_landed = vec![(asked.clone(), answer, added)];
        let e = s.run("layer.addUrl", &asked).unwrap_err();
        assert!(e.contains("taken away (undone)"), "{e}");
        // A tile layer (a GeoTIFF read in place answers its place in the list), then taken away.
        let tiles = json!({"url": "https://x.org/{z}/{x}/{y}.png", "name": "T"});
        let answer = s.run("map.addTiles", &tiles);
        let added = Added::of(s.map(), &answer);
        assert!(added.tiles.len() == 1 && !added.gone(s.map()), "{added:?}");
        s.rerun_landed = vec![(asked.clone(), answer.clone(), added)];
        let at = answer.as_ref().unwrap()["index"].clone();
        assert_eq!(s.run("layer.addUrl", &asked).unwrap()["index"], at);
        // Another tile layer put above it: the answer gives where it is now.
        s.run("map.addTiles", &json!({"url": "https://y.org/{z}/{x}/{y}.png", "name": "U"})).unwrap();
        let moved = s.map().tiles.iter().position(|t| t.name == "T").unwrap();
        assert_eq!(s.run("layer.addUrl", &asked).unwrap()["index"], json!(moved));
        // The same address as an overlay too: not it, so with it taken away, the overlay doesn't
        // answer for it.
        s.run("map.addTiles", &json!({"url": "https://x.org/{z}/{x}/{y}.png", "name": "T2", "overlay": true})).unwrap();
        let moved = s.map().tiles.iter().position(|t| t.name == "T").unwrap();
        assert_eq!(s.run("layer.addUrl", &asked).unwrap()["index"], json!(moved));
        s.run("map.removeTiles", &json!({"index": moved})).unwrap();
        assert!(s.map().tiles.iter().any(|t| t.name == "T2"));
        let e = s.run("layer.addUrl", &asked).unwrap_err();
        assert!(e.contains("taken away (undone)"), "{e}");
        // A standalone table (a file with no geometry), then taken away.
        let answer = s.run("layer.addDataText", &json!({"fileName": "sites.csv", "text": "id,street\n1,Champ de Mars\n"}));
        assert!(answer.as_ref().unwrap()["table"].is_string(), "{answer:?}");
        s.rerun_landed = vec![(asked.clone(), answer.clone(), Added::of(s.map(), &answer))];
        assert_eq!(s.run("layer.addUrl", &asked), answer);
        s.run("table.remove", &json!({"table": "sites"})).unwrap();
        let e = s.run("layer.addUrl", &asked).unwrap_err();
        assert!(e.contains("the table its download added was taken away (undone)"), "{e}");
    }

    /// A number lands only the read it was given for: a plugin's has no login, and a read that
    /// may use one, or names a connection it wasn't given, is another.
    #[test]
    fn a_read_with_another_login_is_not_what_was_asked() {
        let u = "https://x.org/a.csv";
        let plugin = json!({"url": u, "anonymous": true});
        assert!(asked_by(&plugin, &json!({"url": u, "anonymous": true, "id": 1})));
        assert!(!asked_by(&plugin, &json!({"url": u, "id": 1})));
        assert!(!asked_by(&json!({"url": u}), &plugin));
        assert!(asked_by(&json!({"url": u, "connection": "c1"}), &json!({"url": u, "connection": "c1"})));
        assert!(!asked_by(&json!({"url": u, "connection": "c1"}), &json!({"url": u, "connection": "c2"})));
        assert!(!asked_by(&json!({"url": u}), &json!({"url": u, "connection": "c1"})));
    }

    /// Downloads given up after the wait are said once, in one notice with their addresses,
    /// whatever the commands do with their errors; none while they still have time.
    #[test]
    fn a_download_given_up_is_said_with_its_address() {
        // Only where the clock has run that long: a machine just started can't be that far back.
        let Some(long_ago) = Instant::now().checked_sub(REQUEST_WAIT + Duration::from_secs(1)) else { return };
        let mut s = Session::new(crate::project::Project::default());
        let url = "https://x.org/slow.csv?token=hunter2";
        s.retry_plugin("plugin:eng-slow/f", &json!({}), Vec::new(), vec![(900, json!({"url": url}))]).unwrap();
        let mark = s.notice_mark();
        s.poll_plugin_retries();
        assert!(s.notices_since(mark).is_empty(), "it still has time");
        s.url_waits.in_flight[0].2 = long_ago;
        s.poll_plugin_retries();
        s.poll_plugin_retries();
        let notices = s.notices_since(mark);
        let said: Vec<&str> = notices.iter().filter(|n| n.error && n.text.contains("stopped waiting")).map(|n| n.text.as_str()).collect();
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(said[0].starts_with("https://x.org/slow.csv") && !said[0].contains("hunter2"), "{said:?}");
        // Several at once: one notice, with how many and which.
        let others = vec![(901, json!({"url": "https://x.org/a.csv?token=hunter2"})), (902, json!({"url": "https://x.org/b.csv"}))];
        s.retry_plugin("plugin:eng-slow/g", &json!({}), Vec::new(), others).unwrap();
        s.url_waits.in_flight.iter_mut().for_each(|w| w.2 = long_ago);
        let mark = s.notice_mark();
        s.poll_plugin_retries();
        let notices = s.notices_since(mark);
        let said: Vec<&str> = notices.iter().filter(|n| n.error && n.text.contains("stopped waiting")).map(|n| n.text.as_str()).collect();
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(
            said[0].starts_with("2 downloads") && said[0].contains("https://x.org/a.csv") && said[0].contains("https://x.org/b.csv"),
            "{said:?}"
        );
        assert!(!said[0].contains("hunter2"), "{said:?}");
    }

    /// A waiting command's answer names what it waits for, without the addresses' keys.
    #[test]
    fn a_waiting_answer_hides_the_keys_of_its_addresses() {
        let mut s = Session::new(crate::project::Project::default());
        let doc = "https://x.org/doc.json?token=hunter2";
        let csv = "https://x.org/a.csv?token=hunter3";
        let r = s.retry_plugin("plugin:eng-keys/f", &json!({}), vec![format!("eng-keys {doc}")], vec![(900, json!({"url": csv}))]).unwrap();
        let (fetching, downloading) = (r["fetching"][0].as_str().unwrap(), r["downloading"][0].as_str().unwrap());
        assert!(fetching.starts_with("https://x.org/doc.json") && !fetching.contains("hunter2"), "{r}");
        assert!(downloading.starts_with("https://x.org/a.csv") && !downloading.contains("hunter3"), "{r}");
        assert!(s.url_waits.holds(900, &json!({"url": csv})), "it still waits for the address asked for");
    }

    /// A refused landing's error, which the assistant and MCP get as it is, keeps an address's
    /// keys out of it.
    #[test]
    fn a_refused_landing_hides_the_keys_of_its_address() {
        let mut s = Session::new(crate::project::Project::default());
        let url = "https://x.org/e.geojson?token=hunter2";
        s.url_waits.remember_given_up(800, json!({"url": url}));
        let e = s.run("layer.addUrl", &json!({"url": url, "request": 800})).unwrap_err();
        assert!(e.starts_with("https://x.org/e.geojson") && e.contains("stopped waiting") && !e.contains("hunter2"), "{e}");
        s.url_waits.added.push_back((801, json!({"url": url})));
        let e = s.run("layer.addUrl", &json!({"url": url, "request": 801})).unwrap_err();
        assert!(e.starts_with("https://x.org/e.geojson") && e.contains("added already") && !e.contains("hunter2"), "{e}");
    }

    /// A caller giving a waited download's number with its address lands it; the app's own
    /// download of it landing after isn't added again, and says so (#1202).
    #[test]
    fn a_download_landed_twice_is_added_once() {
        let mut s = Session::new(crate::project::Project::default());
        let url = "https://x.org/a.geojson";
        s.url_waits.wait(900, json!({"url": url}));
        let point = br#"{"type": "Point", "coordinates": [1, 2]}"#;
        let add = |s: &mut Session| {
            let id = s.download_started();
            assert!(s.download_landed(id, point.to_vec()));
            s.run("layer.addDownload", &json!({"id": id, "url": url, "request": 900}))
        };
        add(&mut s).unwrap();
        assert_eq!(s.map().layers.len(), 1);
        let mark = s.notice_mark();
        let e = add(&mut s).unwrap_err();
        assert!(e.starts_with(url) && e.contains("added already"), "{e}");
        assert_eq!(s.map().layers.len(), 1, "added once");
        // Said by the session, as any refused command is: once, for every front end.
        let said: Vec<&str> = s.notices_since(mark).iter().filter(|n| n.error).map(|n| n.text.as_str()).collect();
        assert_eq!(said, [e.as_str()]);
        assert!(s.downloaded.is_empty(), "its bytes go");
        // One that landed read already (a service's protocol buffer pages) goes the same way.
        let id = s.download_started();
        let read = rustgis_data::Dataset::new("a", rustgis_data::GeometryType::Point, Vec::new());
        assert!(s.download_landed_read(id, read, rustgis_data::ReadReport::default()));
        s.run("layer.addDownload", &json!({"id": id, "url": url, "request": 900})).unwrap_err();
        assert!(s.landed_reads.is_empty(), "its read goes");
    }

    /// A download made while a command still waits is that one when asked for again, until what it
    /// added is taken away: then a request made since is a new download, not that error (#1202).
    #[test]
    fn a_download_taken_away_is_downloaded_again() {
        let mut s = Session::new(crate::sample::world_project());
        let asked = json!({"url": "https://x.org/a.geojson"});
        let ask = |s: &mut Session| {
            s.call_requests = Some(Vec::new());
            s.request_url(&asked).unwrap();
            s.call_requests.take().unwrap()[0].0
        };
        let id = s.map().layers[0].id;
        s.url_waits.wait(900, asked.clone());
        let answer: CmdResult = Ok(json!({"layer": id}));
        let added = Added::of(s.map(), &answer);
        s.url_waits.land(900, answer, added);
        assert_eq!(ask(&mut s), 900, "still there");
        assert!(s.take_url_requests().is_empty());
        s.map_mut().layers.retain(|l| l.id != id);
        let again = ask(&mut s);
        assert_ne!(again, 900, "taken away");
        assert_eq!(s.take_url_requests(), [json!({"url": "https://x.org/a.geojson", "request": again})]);
        // A standalone table, which no map holds, the same.
        let mut s = Session::new(crate::sample::world_project());
        let table = s.project.add_table(rustgis_data::Dataset::new("t", rustgis_data::GeometryType::None, Vec::new()));
        s.url_waits.wait(901, asked.clone());
        let answer: CmdResult = Ok(json!({"table": "t", "id": table}));
        let added = Added::of(s.map(), &answer);
        s.url_waits.land(901, answer, added);
        assert_eq!(ask(&mut s), 901, "still there");
        assert!(s.take_url_requests().is_empty());
        s.project.tables.retain(|t| t.id != table);
        assert_ne!(ask(&mut s), 901, "taken away");
        assert_eq!(s.take_url_requests().len(), 1);
    }

    /// A download that failed is asked for again as a new download, not answered as the one
    /// that failed; one being made or added is the same request still for a plugin command's
    /// call, and another download for any other; a request that isn't an object is refused
    /// (#1202).
    #[test]
    fn a_failed_download_asked_again_is_downloaded_again() {
        let mut s = Session::new(crate::project::Project::default());
        let asked = json!({"url": "https://x.org/a.geojson"});
        s.url_waits.wait(900, asked.clone());
        s.url_waits.land(900, Err("https://x.org/a.geojson: 404 Not Found".into()), Added::default());
        assert_eq!(s.url_waits.in_flight(&asked), None);
        s.request_url(&asked).unwrap();
        let taken = s.take_url_requests();
        assert_eq!(taken.len(), 1, "{taken:?}");
        assert_ne!(taken[0]["request"], json!(900));
        // Added: the same request for a plugin command's call, nothing downloaded again...
        let other = json!({"url": "https://x.org/b.geojson"});
        s.url_waits.wait(901, other.clone());
        s.url_waits.land(901, Ok(json!({"layer": 1})), Added::default());
        assert_eq!(s.url_waits.in_flight(&other), Some(901));
        s.call_requests = Some(Vec::new());
        s.request_url(&other).unwrap();
        assert!(s.take_url_requests().is_empty());
        assert_eq!(s.call_requests.take().unwrap()[0].0, 901);
        // ... and downloaded again when asked from anywhere else (the Add Data dialog).
        s.request_url(&other).unwrap();
        let taken = s.take_url_requests();
        assert_eq!(taken.len(), 1, "{taken:?}");
        assert_ne!(taken[0]["request"], json!(901));
        // Being made, the same: that one for a plugin command's call, another from anywhere else.
        let third = json!({"url": "https://x.org/c.geojson"});
        s.url_waits.wait(902, third.clone());
        s.call_requests = Some(Vec::new());
        // However often one call asks for it, its command waits for it once.
        for _ in 0..3 {
            s.request_url(&third).unwrap();
        }
        assert!(s.take_url_requests().is_empty());
        let asked: Vec<u64> = s.call_requests.take().unwrap().iter().map(|(n, _)| *n).collect();
        assert_eq!(asked, [902]);
        s.request_url(&third).unwrap();
        assert_ne!(s.take_url_requests()[0]["request"], json!(902));
        assert!(s.request_url(&json!("https://x.org/d.geojson")).unwrap_err().contains("an object"));
    }

    /// A number given with another address than the download it was given for isn't taken: the
    /// request is a download of its own, with its own number; handed back with its own address,
    /// it is queued again, whole (#1202).
    #[test]
    fn a_number_given_for_another_address_is_set_aside() {
        let mut s = Session::new(crate::project::Project::default());
        s.url_waits.wait(900, json!({"url": "https://x.org/a.geojson"}));
        s.request_url(&json!({"url": "https://x.org/b.geojson", "request": 900})).unwrap();
        let taken = s.take_url_requests();
        assert_eq!(taken.len(), 1, "{taken:?}");
        assert_eq!(taken[0]["url"], "https://x.org/b.geojson");
        assert_ne!(taken[0]["request"], json!(900));
        s.request_url(&json!({"url": "https://x.org/a.geojson", "request": 900})).unwrap();
        let taken = s.take_url_requests();
        assert_eq!((taken[0]["request"].clone(), taken[0]["whole"].clone()), (json!(900), json!(true)), "{taken:?}");
        // The same address read another way (columns it wasn't asked with) isn't it either.
        let other_import = json!({"url": "https://x.org/a.geojson", "x": "lon", "y": "lat", "request": 900});
        assert!(!s.url_waits.holds(900, &other_import) && s.url_waits.holds(900, &json!({"url": "https://x.org/a.geojson"})));
        // A number nothing asked under, taken by no one: set aside, so the counter can't reach it
        // and queue two downloads under one number.
        let next = s.next_request + 1;
        s.request_url(&json!({"url": "https://x.org/c.geojson", "request": next + 1})).unwrap();
        s.request_url(&json!({"url": "https://x.org/d.geojson"})).unwrap();
        let taken = s.take_url_requests();
        let numbers: Vec<_> = taken.iter().map(|q| q["request"].as_u64()).collect();
        assert_eq!(numbers, [Some(next), Some(next + 1)], "{taken:?}");
        // Handed back with the same request queued under another number: queued as well, or nothing
        // would land the number its command waits for.
        let doubled = json!({"url": "https://x.org/f.geojson"});
        s.url_waits.wait(910, doubled.clone());
        s.request_url(&doubled).unwrap();
        s.request_url(&json!({"url": "https://x.org/f.geojson", "request": 910})).unwrap();
        let taken = s.take_url_requests();
        assert_eq!(taken.len(), 2, "{taken:?}");
        assert_eq!((taken[1]["request"].clone(), taken[1]["whole"].clone()), (json!(910), json!(true)), "{taken:?}");
        assert_ne!(taken[0]["request"], json!(910));
        // Given up, its number is set aside too: another download, and the first one's landing,
        // under that number, is still refused.
        let late = json!({"url": "https://x.org/e.geojson"});
        s.url_waits.remember_given_up(800, late.clone());
        s.request_url(&json!({"url": "https://x.org/e.geojson", "request": 800})).unwrap();
        assert_ne!(s.take_url_requests()[0]["request"], json!(800));
        assert!(s.url_waits.given_up(800, &late));
        let e = s.run("layer.addUrl", &json!({"url": "https://x.org/e.geojson", "request": 800})).unwrap_err();
        assert!(e.contains("stopped waiting"), "{e}");
    }

    /// Plugin commands waiting for the same download each get its answer, a rerun that waits
    /// again keeps the answers it had, and a download that lands after its command gave up isn't
    /// added (#1202).
    #[test]
    fn landed_downloads_answer_every_waiting_run() {
        use crate::Session;
        for id in ["eng-share-a", "eng-share-b"] {
            let pkg = rustgis_plugins::Package::from_zip(&rustgis_plugins::testing::package(id, &["edit", "network"])).unwrap();
            rustgis_plugins::load(pkg, "test").unwrap();
        }
        // Answers name layers that are on the map (the sample's), as a rerun checks.
        let mut s = Session::new(crate::sample::world_project());
        let (l7, l8) = (u64::from(s.map().layers[0].id), u64::from(s.map().layers[1].id));
        // What a command calls, and what is kept as asked: read with no saved login.
        let asked = json!({"url": "https://x.org/a.csv", "anonymous": true});
        let p = json!({"op": "layer.addUrl", "params": {"url": "https://x.org/a.csv"}});
        // Two commands wait for one download (request 900): both get what adding it answered.
        for id in ["plugin:eng-share-a/f", "plugin:eng-share-b/f"] {
            s.retry_plugin(id, &p, Vec::new(), vec![(900, asked.clone())]).unwrap();
        }
        s.url_waits.land(900, Ok(json!({"layer": l7})), Added::default());
        let ran = s.poll_plugin_retries();
        assert_eq!(ran.len(), 2, "{ran:?}");
        assert!(ran.iter().all(|(_, r)| r.as_ref().is_ok_and(|v| v["layer"] == l7)), "{ran:?}");
        assert!(s.take_url_requests().is_empty(), "nothing asked again");
        // A rerun that waits again for another download keeps what it had: a.csv isn't asked again.
        s.rerun_landed = vec![(asked.clone(), Ok(json!({"layer": l7})), Default::default())];
        s.retry_plugin("plugin:eng-share-a/f", &p, Vec::new(), vec![(901, json!({"url": "https://x.org/b.csv"}))]).unwrap();
        s.rerun_landed.clear();
        s.url_waits.land(901, Ok(json!({"layer": l8})), Added::default());
        let ran = s.poll_plugin_retries();
        assert_eq!(ran.len(), 1, "{ran:?}");
        assert_eq!(ran[0].1.as_ref().unwrap()["layer"], l7, "{ran:?}");
        assert!(s.take_url_requests().is_empty(), "nothing asked again");
        // The download it waited for answers too, for a run that asks for it.
        let b = json!({"url": "https://x.org/b.csv", "anonymous": true});
        let call = json!({"op": "layer.addUrl", "params": {"url": "https://x.org/b.csv"}});
        s.rerun_landed = vec![(asked.clone(), Ok(json!({"layer": l7})), Default::default())];
        s.retry_plugin("plugin:eng-share-a/f", &call, Vec::new(), vec![(904, b.clone())]).unwrap();
        s.rerun_landed.clear();
        s.url_waits.land(904, Ok(json!({"layer": l8})), Added::default());
        let ran = s.poll_plugin_retries();
        assert_eq!(ran[0].1.as_ref().unwrap()["layer"], l8, "{ran:?}");
        // The same address asked another way (other columns) doesn't land a wait.
        s.retry_plugin("plugin:eng-share-a/f", &p, Vec::new(), vec![(905, json!({"url": "https://x.org/f.csv", "x": "lon"}))]).unwrap();
        assert!(!s.url_waits.holds(905, &json!({"url": "https://x.org/f.csv", "x": "lng", "id": 3})));
        assert!(s.url_waits.holds(905, &json!({"url": "https://x.org/f.csv", "x": "lon", "id": 3, "cog": true})));
        // What the front end doesn't pass on (`whole`, a `name` that isn't text, `bounds` that
        // aren't a list), or may change (`zoom`), doesn't stop a landing.
        let asked =
            json!({"url": "https://x.org/g.csv", "x": "lon", "whole": true, "zoom": false, "dates": ["2020"], "name": 5, "bounds": "x"});
        s.retry_plugin("plugin:eng-share-a/f", &p, Vec::new(), vec![(906, asked)]).unwrap();
        assert!(s.url_waits.holds(906, &json!({"url": "https://x.org/g.csv", "x": "lon", "id": 4})));
        assert!(s.url_waits.holds(906, &json!({"url": "https://x.org/g.csv", "x": "lon", "zoom": true})));
        assert!(s.url_waits.holds(906, &json!({"url": "https://x.org/g.csv", "x": "lon", "zoom": false})));
        // A key the front end passes on, left out: another import, which lands nothing.
        assert!(!s.url_waits.holds(906, &json!({"url": "https://x.org/g.csv"})));
        // Given up past the wait (`expire`): each late landing is refused, once.
        // (Only where the clock has run that long: a machine just started can't be that far back.)
        if let Some(long_ago) = Instant::now().checked_sub(REQUEST_WAIT + Duration::from_secs(1)) {
            for (n, file) in [(902, "c.csv"), (903, "c2.csv")] {
                s.url_waits.in_flight.push((n, json!({"url": format!("https://x.org/{file}")}), long_ago));
            }
            s.url_waits.expire();
            for (n, file) in [(902, "c.csv"), (903, "c2.csv")] {
                let late = json!({"url": format!(" https://x.org/{file} "), "request": n});
                assert!(s.url_waits.given_up(n, &late), "{n}");
                let e = s.run("layer.addUrl", &late).unwrap_err();
                assert!(e.contains("stopped waiting"), "{e}");
                assert!(!s.url_waits.given_up(n, &late), "{n}: forgotten once refused");
            }
        }
        // One still out stays given up however many more are: past the bound, a plugin command
        // asks for no new download (and its request isn't downloaded); one that fails is
        // forgotten, which frees a place.
        let first = json!({"url": "https://x.org/out.csv"});
        s.url_waits.expired.clear();
        s.retry_plugin("plugin:eng-share-c/f", &p, Vec::new(), vec![(959, json!({"url": "https://x.org/old.csv"}))]).unwrap();
        s.url_waits.remember_given_up(800, first.clone());
        for n in 1..MAX_EXPIRED as u64 {
            s.url_waits.remember_given_up(10_000 + n, json!({"url": format!("https://x.org/{n}.csv")}));
        }
        let mark = s.notice_mark();
        s.url_requests.push(json!({"url": "https://x.org/new.csv", "request": 960}));
        let e = s.retry_plugin("plugin:eng-share-c/f", &p, Vec::new(), vec![(960, json!({"url": "https://x.org/new.csv"}))]).unwrap_err();
        assert!(e.contains("still under way") && e.contains("its download of https://x.org/new.csv wasn't started"), "{e}");
        for cancelled in [
            vec!["https://x.org/a?token=hunter2".to_string()],
            vec!["https://x.org/a?token=hunter2".to_string(), "https://x.org/b".to_string()],
        ] {
            assert!(!not_started(&cancelled).contains("hunter2"));
        }
        assert!(s.take_url_requests().is_empty(), "not downloaded");
        // Refused, it changed nothing: the command's earlier run still waits.
        assert!(s.plugin_retries.iter().any(|r| r.id == "plugin:eng-share-c/f" && r.requests == [959]));
        assert!(s.url_waits.has(959) && !s.notices_since(mark).iter().any(|n| n.text.contains("replaced by this one")));
        // One another command already waits for starts no download: it is shared, not refused.
        let shared = json!({"url": "https://x.org/shared.csv"});
        s.url_waits.wait(961, shared.clone());
        s.retry_plugin("plugin:eng-share-d/f", &p, Vec::new(), vec![(961, shared)]).unwrap();
        s.plugin_retries.clear();
        s.url_waits.keep_only(&[]);
        assert!(s.url_waits.given_up(800, &first), "still refused when it lands");
        s.request_failed(&json!({"url": "https://x.org/out.csv", "request": 800}), "offline");
        assert!(!s.url_waits.given_up(800, &first) && !s.url_waits.given_up_full());
        s.url_waits.expired.clear();
        // A run replaced by another gives up the download only it waited for.
        s.retry_plugin("plugin:eng-share-b/f", &p, Vec::new(), vec![(950, json!({"url": "https://x.org/d.csv"}))]).unwrap();
        s.retry_plugin("plugin:eng-share-b/f", &p, Vec::new(), vec![(951, json!({"url": "https://x.org/e.csv"}))]).unwrap();
        let e = s.run("layer.addUrl", &json!({"url": "https://x.org/d.csv", "request": 950})).unwrap_err();
        assert!(e.contains("stopped waiting"), "{e}");
        // One the front end hadn't taken yet is taken off the queue and forgotten: it can't land.
        let queued = json!({"url": "https://x.org/q.csv"});
        s.url_requests.push(json!({"url": "https://x.org/q.csv", "request": 970}));
        s.retry_plugin("plugin:eng-share-e/f", &p, Vec::new(), vec![(970, queued.clone())]).unwrap();
        s.retry_plugin("plugin:eng-share-e/f", &p, Vec::new(), Vec::new()).unwrap();
        assert!(!s.url_waits.given_up(970, &queued) && s.take_url_requests().is_empty());
        // A number given with another address lands nothing.
        let _ = s.run("layer.addDownload", &json!({"url": "https://x.org/other.csv", "request": 951, "id": 424242}));
        assert!(s.url_waits.holds(951, &json!({"url": "https://x.org/e.csv"})), "951 still waits for e.csv");
        // Asked again by a plugin command's call once made, while the command waits: that one,
        // not another download.
        s.url_waits.land(951, Ok(json!({"layer": l7})), Added::default());
        s.call_requests = Some(Vec::new());
        s.request_url(&json!({"url": "https://x.org/e.csv"})).unwrap();
        s.call_requests = None;
        assert!(s.take_url_requests().is_empty(), "made already");
        // Handed back by the front end that took it (the browser found it must be downloaded
        // whole): queued again, whole. Asked again by its address alone by a plugin command's
        // call, it is that one.
        let g = json!({"url": "https://x.org/g.csv"});
        s.retry_plugin("plugin:eng-share-f/f", &p, Vec::new(), vec![(952, g.clone())]).unwrap();
        s.call_requests = Some(Vec::new());
        s.request_url(&g).unwrap();
        s.call_requests = None;
        assert!(s.take_url_requests().is_empty(), "under way");
        s.request_url(&json!({"url": "https://x.org/g.csv", "request": 952})).unwrap();
        assert_eq!(s.take_url_requests(), [json!({"url": "https://x.org/g.csv", "request": 952, "whole": true})]);
        // A run replaced gives up the download only it waits for, unless a command running again
        // in the same poll (out of the list until it has run) waits for it too.
        for (n, ready) in [(953, false), (954, true)] {
            let h = json!({"url": format!("https://x.org/{n}.csv")});
            s.retry_plugin("plugin:eng-share-g/f", &p, Vec::new(), vec![(n, h.clone())]).unwrap();
            s.rerun_requests = if ready { vec![n] } else { Vec::new() };
            s.retry_plugin("plugin:eng-share-g/f", &p, Vec::new(), Vec::new()).unwrap();
            s.rerun_requests.clear();
            assert_eq!(s.url_waits.given_up(n, &h), !ready, "{n}");
        }
        for id in ["eng-share-a", "eng-share-b"] {
            rustgis_plugins::unload(id);
        }
    }

    #[test]
    fn checks_addresses() {
        assert_eq!(checked(" https://example.com/a?b=1#frag ").unwrap(), "https://example.com/a?b=1");
        for bad in [
            "",
            "file:///etc/passwd",
            "s3://bucket/key",
            "ftp://example.com/x",
            "rustgis-anonymous:https://example.com/",
            "http://localhost:8080/",
            "http://127.0.0.1/",
            "http://10.0.0.1/",
            "http://169.254.169.254/latest/meta-data/",
            "http://[::1]/",
            "http://example.com/a b",
        ] {
            assert!(checked(bad).is_err(), "{bad}");
        }
        assert!(checked(&format!("https://example.com/{}", "a".repeat(MAX_URL))).is_err());
    }

    #[test]
    fn answers_text_and_json() {
        let a = answer("https://x.org/t", br#"{"select": [49059], "valid": true}"#).unwrap();
        assert_eq!(a["json"]["select"][0], 49059);
        assert_eq!(a["url"], "https://x.org/t");
        let a = answer("https://x.org/t", b"\xEF\xBB\xBFplain").unwrap();
        assert_eq!(a["text"], "plain");
        assert!(a.get("json").is_none());
        assert!(answer("https://x.org/t", b"{broken").unwrap().get("json").is_none());
        assert!(answer("https://x.org/t", b"\xff\xfe").unwrap_err().contains("UTF-8"));
        assert!(answer("https://x.org/t", &vec![b' '; MAX_BYTES + 1]).unwrap_err().contains("larger"));
    }

    #[test]
    fn each_plugin_has_answers_of_its_own() {
        let mut c = Cache::default();
        let url = "https://cache.test/t?token=1";
        c.keep(&key("one", url), Ok(json!({"text": "secret"})));
        assert!(c.get(&key("one", url)).is_some());
        assert!(c.get(&key("two", url)).is_none(), "another plugin's answer");
    }

    #[test]
    fn the_cache_is_bounded_and_keeps_errors_briefly() {
        let mut c = Cache::default();
        for i in 0..CACHE_ENTRIES + 10 {
            c.keep(&format!("https://cache.test/{i}"), Ok(json!({"text": ""})));
        }
        assert!(c.get("https://cache.test/0").is_none(), "the oldest went first");
        assert!(c.get(&format!("https://cache.test/{}", CACHE_ENTRIES + 9)).is_some());
        assert!(c.entries.len() <= CACHE_ENTRIES);
        c.in_flight.push(("https://cache.test/err".into(), Instant::now()));
        assert!(!c.landed("https://cache.test/err"));
        // A fetch that never answers is taken as lost after a while.
        if let Some(lost) = Instant::now().checked_sub(IN_FLIGHT_FOR) {
            c.in_flight.push(("https://cache.test/lost".into(), lost));
            assert!(c.landed("https://cache.test/lost"));
        }
        c.keep("https://cache.test/err", Err("HTTP 500".into()));
        assert!(c.landed("https://cache.test/err"));
        // An address no longer fetched counts as landed even once its answer is gone: the
        // command waiting for it runs again (and fetches it again) instead of waiting forever.
        assert!(c.landed("https://cache.test/never-kept"));
        // Every command waiting for it sees the error, until it expires.
        assert_eq!(c.get("https://cache.test/err"), Some(Err("HTTP 500".into())));
        assert_eq!(c.get("https://cache.test/err"), Some(Err("HTTP 500".into())));
        if let Some(e) = c.entries.iter_mut().find(|e| e.url == "https://cache.test/err") {
            e.at = e.at.checked_sub(ERROR_TTL).unwrap_or(e.at);
        }
        assert_eq!(c.get("https://cache.test/err"), None);
        // Large answers count by their bytes.
        let big = "x".repeat(MAX_BYTES / 2);
        for i in 0..40 {
            c.keep(&format!("https://cache.test/big{i}"), Ok(json!({"text": big})));
        }
        assert!(c.entries.iter().map(|e| e.size).sum::<usize>() <= CACHE_BYTES);
        assert!(c.get("https://cache.test/big39").is_some());
    }
}
