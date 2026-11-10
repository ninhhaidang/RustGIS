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
//!
//! A plugin command's `service.add` on the desktop waits the same way (#1319): the documents it
//! reads are fetched off the frame ([`Session::left_document`]), and its features or coverage are
//! left to the app as `layer.addUrl`'s download is.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

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
        return Err(crate::sources::shortened_message_with_address(url, 80, "plugins fetch only http:// and https:// addresses"));
    }
    // A fragment never reaches the server (and RustGIS marks byte ranges with one).
    let url = url.split('#').next().unwrap_or(url);
    address::check(url, None)?;
    if address::reach(url) != address::Reach::Public && !allow_local() {
        return Err(crate::sources::message_with_address(
            url,
            "plugins fetch only from the internet, not this computer or the local network",
        ));
    }
    Ok(url.to_string())
}

/// The answer for a fetched document: `{url, text}`, and `json` when the text is a JSON object
/// or array.
pub fn answer(url: &str, bytes: &[u8]) -> Result<Value, String> {
    if bytes.len() > MAX_BYTES {
        return Err(crate::sources::message_with_address(url, &format!("larger than {MAX_BYTES} bytes")));
    }
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let text = std::str::from_utf8(bytes).map_err(|_| crate::sources::message_with_address(url, "the answer isn't UTF-8 text"))?;
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
            let answer = r.map_err(|e| crate::sources::message_with_address(&url, &e)).and_then(|bytes| answer(&url, &bytes));
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
            return Err(crate::sources::message_with_address(
                url,
                &format!("this plugin call's fetches took longer than {} s in all", CALL_BUDGET.as_secs()),
            ));
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
        let answer = bytes.map_err(|e| crate::sources::message_with_address(url, &e)).and_then(|bytes| answer(url, &bytes));
        cache().keep(key, answer.clone());
        answer
    }

    #[cfg(target_arch = "wasm32")]
    fn blocking(&mut self, _key: &str, url: &str) -> CmdResult {
        let _ = self.started;
        Err(crate::sources::message_with_address(url, "the web app can't wait for a fetch"))
    }
}

/// How long a plugin command waits for a download it left to the app before it runs again
/// with an error: one the app lost track of doesn't keep it waiting for ever.
pub const REQUEST_WAIT: Duration = Duration::from_secs(10 * 60);

/// The most bytes a service's document read off the frame may have: it is kept until no command
/// waits for it, and read again each time the command runs.
pub const MAX_DOCUMENT: usize = 64 << 20;
/// How long a service's document read off the frame may take: what a command waiting for it had.
const DOCUMENT_TIMEOUT: Duration = Duration::from_secs(60);

/// A plugin command waiting for its fetches, or its downloads, before it runs again.
#[derive(Clone)]
pub(crate) struct Retry {
    continuation: crate::policy::Continuation,
    id: String,
    params: Value,
    /// The view it first ran with, and the size of the window showing it: the window answers while
    /// the command waits, and a view changed meanwhile must not make the next run ask for another
    /// area (#1319).
    view: (crate::map::Camera, (f64, f64)),
    /// The notices given when it began to wait: one given since is about the time it waits.
    notices: u64,
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

/// The key a window of the view is told apart by when a command run again is answered
/// ([`Session::with_view`]): matched only, never read as a parameter.
const VIEW_ASKED: &str = "viewAsked";

/// The downloads plugin commands wait for (#1202): those the app is making, with what was
/// asked and since when, and those added, with what adding them answered, until the command
/// waiting for them runs again.
#[derive(Clone, Default)]
pub(crate) struct UrlWaits {
    /// The view's boxes of the window requests asked with `"bbox": "view"`, by their numbers: a
    /// request for the view joins one being made only when it was asked in the same view. Kept
    /// here, they go aside with a project and back on a rollback as the waits do.
    pub(crate) views: std::collections::BTreeMap<u64, [f64; 4]>,
    owners: std::collections::BTreeMap<u64, crate::policy::Continuation>,
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
    /// A service's documents being read off the frame, by request number.
    reading: Vec<(u64, String, Arc<Mutex<Option<Result<Vec<u8>, String>>>>)>,
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
    /// A service's document read off the frame ([`Session::left_document`]), which adds nothing.
    pub(crate) document: Option<Arc<[u8]>>,
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
        Added { layers, tiles, map: at, document: None }
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
    pub(crate) fn owner(&self, n: u64) -> Option<&crate::policy::Continuation> {
        self.owners.get(&n)
    }

    /// The number of a download being made for `asked`, or made and added while its command
    /// waits to run again: a request made again then is that one, not another download.
    pub(crate) fn in_flight(&self, asked: &Value, owns: impl Fn(u64) -> bool) -> Option<u64> {
        // One that failed added nothing: asked again, it is downloaded again.
        let made = self.landed.iter().find(|(n, q, answer, _)| answer.is_ok() && owns(*n) && same_request(q, asked)).map(|(n, ..)| *n);
        self.in_flight.iter().find(|(n, q, _)| owns(*n) && same_request(q, asked)).map(|(n, ..)| *n).or(made)
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
            // (A document adds nothing, and no front end lands it again by its number.)
            if answer.is_ok() && added.document.is_none() {
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

    /// Downloads waited for past [`REQUEST_WAIT`] by `now` land as failed, and are remembered as
    /// given up: their addresses, redacted, to be said (a notice with a key in it loses what
    /// follows it).
    fn expire_at(&mut self, now: Instant) -> Vec<String> {
        let late: Vec<(u64, Value)> = self
            .in_flight
            .iter()
            .filter(|(.., at)| now.saturating_duration_since(*at) >= REQUEST_WAIT)
            .map(|(n, q, _)| (*n, q.clone()))
            .collect();
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
        if !taken {
            self.owners.remove(&n);
        }
    }

    /// Kept until its download lands (and is refused) or fails: never forgotten while it can
    /// still land. [`MAX_EXPIRED`] bounds them by refusing new downloads instead, so those under
    /// way by then can still take them a little past it.
    fn remember_given_up(&mut self, n: u64, asked: Value) {
        self.expired.push((n, asked));
    }

    /// Starts reading the document `asked` for off the frame, as request `n`.
    pub(crate) fn read_document(&mut self, n: u64, asked: &Value) {
        let slot: Arc<Mutex<Option<Result<Vec<u8>, String>>>> = Arc::default();
        let land = slot.clone();
        let url = asked["url"].as_str().unwrap_or("").to_string();
        crate::render::rustgis_tiles::cache::download_within(
            &url,
            MAX_DOCUMENT,
            DOCUMENT_TIMEOUT,
            Box::new(move |r| *land.lock().unwrap_or_else(|e| e.into_inner()) = Some(r)),
        );
        self.reading.push((n, url, slot));
    }

    /// Lands the documents read by now. One whose command gave up is dropped, and forgotten.
    fn documents_in(&mut self) {
        for (n, url, slot) in std::mem::take(&mut self.reading) {
            let answer = slot.lock().unwrap_or_else(|e| e.into_inner()).take();
            let Some(answer) = answer else {
                self.reading.push((n, url, slot));
                continue;
            };
            match answer {
                Ok(bytes) => self.land(n, Ok(json!({})), Added { document: Some(bytes.into()), ..Added::default() }),
                // Its command gets this as its answer: without the address's keys.
                Err(e) => self.land(n, Err(crate::sources::message_with_address(&url, &e)), Added::default()),
            }
            self.refused(n);
        }
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
        if !self.has(n) {
            self.owners.remove(&n);
        }
    }

    /// Keeps the views of the requests still being made or landed only.
    fn prune_views(&mut self) {
        let live: Vec<u64> = self.in_flight.iter().map(|(n, ..)| *n).chain(self.landed.iter().map(|(n, ..)| *n)).collect();
        self.views.retain(|n, _| live.contains(n));
    }

    /// Only what `waiting` still waits for is kept.
    fn keep_only(&mut self, waiting: &[Retry]) {
        let wanted = |n: &u64| waiting.iter().any(|r| r.requests.contains(n));
        self.in_flight.retain(|(n, ..)| wanted(n));
        self.landed.retain(|(n, ..)| wanted(n));
        self.views.retain(|n, _| wanted(n));
        let retained: Vec<u64> = self
            .in_flight
            .iter()
            .map(|(n, ..)| *n)
            .chain(self.landed.iter().map(|(n, ..)| *n))
            .chain(self.expired.iter().map(|(n, ..)| *n))
            .collect();
        self.owners.retain(|n, _| retained.contains(n));
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

/// Whether a download request asks for a service's document, which the session reads itself,
/// rather than data for the app to add.
pub(crate) fn is_document(p: &Value) -> bool {
    p.get("document").and_then(Value::as_bool) == Some(true)
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
        if let Some(o) = asked.as_object_mut() {
            // Only the session tells windows of the view apart ([`Session::with_view`]): a caller's
            // own `viewAsked` is left out.
            o.remove(VIEW_ASKED);
            if self.web_logins.is_anonymous() {
                o.insert("anonymous".into(), json!(true));
            }
        }
        asked
    }

    /// What a command run again is answered for `wanted`: what adding the download answered, and
    /// what it added. A failure answers once a run, and a call made again after it is a new try:
    /// each run gets what the run before it got, in order, so a command that tries again after a
    /// failure goes on to its next try, and doesn't find the same failure every time.
    pub(crate) fn rerun_answer(&mut self, wanted: &Value) -> Option<(CmdResult, Added)> {
        let told = &self.rerun_told;
        let at = self
            .rerun_landed
            .iter()
            .enumerate()
            .position(|(i, (asked, answer, _))| same_request(asked, wanted) && (answer.is_ok() || !told.contains(&i)))?;
        let (_, answer, added) = self.rerun_landed.get(at)?;
        let answered = (answer.clone(), added.clone());
        if answered.0.is_err() {
            self.rerun_told.push(at);
        }
        Some(answered)
    }

    /// The service document at `url`, for a plugin command's `service.add` (#1319): what reading it
    /// off the frame gave, or `None` when it is asked for now, and the command runs again once it
    /// has come. Its waiting, replacing and giving up are a download's. `Err` when it can't be asked
    /// for now, with as many being read as can be: not a read that failed.
    pub(crate) fn left_document(&mut self, url: &str) -> Result<Option<Result<Arc<[u8]>, String>>, String> {
        let asked = self.asked_for(&json!({"url": url, "document": true}));
        if let Some((answer, added)) = self.rerun_answer(&asked) {
            return Ok(Some(answer.and_then(|_| added.document.ok_or_else(|| "no document came".to_string()))));
        }
        // Asked for already in this call, or by a run before it for the same caller: that read,
        // not another (as a download is shared, `enqueue_url`).
        let caller = self.continuation().ok();
        let owns = |n: u64| {
            caller.as_ref().is_some_and(|c| {
                self.url_waits.owner(n).is_some_and(|owner| self.same_url_caller(owner, c) && self.check_continuation(owner).is_ok())
            })
        };
        let now = self.call_requests.as_deref().unwrap_or_default();
        let number =
            match now.iter().find(|(_, q)| same_request(q, &asked)).map(|(n, _)| *n).or_else(|| self.url_waits.in_flight(&asked, owns)) {
                Some(n) => n,
                None => {
                    // Each one read stays in memory, so they wait in the numbers a download does.
                    let reading = self.url_waits.reading.len() + now.iter().filter(|(_, q)| is_document(q)).count();
                    if reading >= crate::MAX_URL_REQUESTS {
                        return Err(format!("{} documents are already being read; try again when they have come", crate::MAX_URL_REQUESTS));
                    }
                    self.next_request += 1;
                    self.next_request
                }
            };
        if let Some(asked_now) = self.call_requests.as_mut()
            && !asked_now.iter().any(|(n, _)| *n == number)
        {
            asked_now.push((number, asked));
        }
        Ok(None)
    }

    /// What a window of the view was asked as, for a command run again to be answered by: `asked`
    /// with the box the view was then (`UrlWaits::views`), so two windows of the view asked in
    /// two views are told apart. Others are `asked` as it is.
    pub(crate) fn with_view(asked: &Value, view: Option<[f64; 4]>) -> Value {
        let mut asked = asked.clone();
        if let (Some(v), Some(o)) = (view, asked.as_object_mut()) {
            o.insert(VIEW_ASKED.into(), json!(v));
        }
        asked
    }

    /// Whether a command run again has an answer for `wanted`.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn rerun_has(&self, wanted: &Value) -> bool {
        let wanted = self.asked_for(wanted);
        self.rerun_landed.iter().any(|(asked, ..)| same_request(asked, &wanted))
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
        let continuation = self.continuation()?;
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
                            .filter(|q| q.params()["request"].as_u64() == Some(*n))
                            .filter_map(|q| q.params()["url"].as_str().map(String::from)),
                    );
                    self.url_requests.retain(|q| q.params()["request"].as_u64() != Some(*n));
                    self.retire_abandoned_requests();
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
        if let Some(i) = self.plugin_retries.iter().position(|r| r.id == id && self.same_url_caller(&r.continuation, &continuation)) {
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
            self.url_waits.owners.entry(n).or_insert_with(|| continuation.clone());
        }
        // What this run already had answered (it is a rerun waiting again): kept for the next.
        let answered = self.rerun_landed.clone();
        let said = self.rerun_said.clone();
        self.plugin_retries.push(Retry {
            continuation,
            id: id.to_string(),
            params: params.clone(),
            // (A run again holds the view of its first, so this keeps it.)
            view: (self.map().camera, self.view_size),
            notices: self.notice_mark(),
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
                    .filter(|q| q.params()["request"].as_u64() == Some(*n))
                    .filter_map(|q| q.params()["url"].as_str().map(String::from))
                    .collect();
                self.url_waits.give_up(*n, queued.is_empty());
                self.url_requests.retain(|q| q.params()["request"].as_u64() != Some(*n));
                self.retire_abandoned_requests();
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

    /// Whether plugin command `id` is waiting to run again with these parameters: asked for again,
    /// it would only start over.
    pub fn plugin_command_waits(&self, id: &str, params: &Value) -> bool {
        self.plugin_retries.iter().any(|r| r.id == id && r.params == *params)
    }

    /// The plugin command waiting longest for its fetches or downloads, for the front end to show:
    /// not while the latest notice is an error given since it began to wait, which goes first. A
    /// command may wait for minutes, while the window takes other work.
    pub fn plugin_command_waiting(&self) -> Option<&str> {
        let first = self.plugin_retries.first()?;
        let failed = self.notices_since(first.notices).last().is_some_and(|n| n.error);
        (!failed).then_some(first.id.as_str())
    }

    /// Runs again the plugin commands whose fetches have all landed and whose downloads the app
    /// has all added (see `host.fetch` and `layer.addUrl` in `docs/plugins.md`): each command and
    /// what it answered this time. Its `layer.addUrl` calls answer what adding each download
    /// answered. Errors are also given as notices, as for any command.
    pub fn poll_plugin_retries(&mut self) -> Vec<(String, CmdResult)> {
        self.poll_plugin_retries_at(Instant::now())
    }

    /// [`Session::poll_plugin_retries`] as at `now`: downloads waited for past [`REQUEST_WAIT`]
    /// by then are given up (a test passes a later time instead of backdating the waits).
    fn poll_plugin_retries_at(&mut self, now: Instant) -> Vec<(String, CmdResult)> {
        self.url_waits.documents_in();
        if self.plugin_retries.is_empty() {
            self.url_waits.prune_views();
            return Vec::new();
        }
        // Said here too: the command may go on without it, and what it answers goes nowhere.
        let minutes = REQUEST_WAIT.as_secs() / 60;
        match self.url_waits.expire_at(now).as_slice() {
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
                let mine = self.url_waits.landed.iter().filter(|(n, ..)| r.requests.contains(n)).map(|(n, asked, answer, added)| {
                    (Self::with_view(asked, self.url_waits.views.get(n).copied()), answer.clone(), added.clone())
                });
                self.rerun_landed = r.answered.iter().cloned().chain(mine).collect();
                self.rerun_said = r.said;
                self.retry_tries = r.tries;
                // It sees the view it first ran with, whichever map is shown now, so it asks for the
                // same area; one it doesn't move itself goes back to what the user has moved to since.
                let (held, size) = r.view;
                let map = self.map().instance_id();
                let shown = std::mem::replace(&mut self.map_mut().camera, held);
                let shown_size = std::mem::replace(&mut self.view_size, size);
                // Run as the app runs a plugin's command, as the caller it first ran for: what it
                // asks for now is left to the app.
                let res = self.with_deferred_downloads(|s| s.retry_continuation(&r.continuation, &r.params));
                self.view_size = shown_size;
                if let Some(m) = self.project.maps.iter_mut().find(|m| m.instance_id() == map && m.camera == held) {
                    m.camera = shown;
                }
                self.retry_tries = 0;
                self.rerun_landed.clear();
                self.rerun_told.clear();
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

    /// A long refused address is shortened without cutting an omission marker in half: the
    /// address before it is cut instead, and its query stays out.
    #[test]
    fn a_shortened_refused_address_keeps_whole_markers() {
        let url = format!("ftp://h.example/{}?page=1&token=hidden", "a".repeat(52));
        let error = checked(&url).unwrap_err();
        assert!(!error.contains("hidden") && !error.contains("page=1"), "{error}");
        assert!(error.ends_with("a…[query omitted]: plugins fetch only http:// and https:// addresses"), "{error}");
        assert_eq!(error.split(": plugins").next().map(|a| a.chars().count()), Some(80), "{error}");
    }

    /// A failure answers once a run, in the order it came, and a call made again after it is a new
    /// download: a command trying again gets its next try, not the same failure for ever. What was
    /// added answers as often as it is asked for (#1319).
    #[test]
    fn a_failed_answer_is_given_once_a_run() {
        let mut s = Session::new(crate::sample::world_project());
        s.leave_downloads = true;
        let id = u64::from(s.map().layers[0].id);
        let asked = json!({"url": "https://x.org/a.geojson"});
        let failed: CmdResult = Err("https://x.org/a.geojson: 502 Bad Gateway".into());
        s.rerun_landed = vec![(asked.clone(), failed.clone(), Added::default())];
        assert_eq!(s.run("layer.addUrl", &asked), failed);
        assert_eq!(s.run("layer.addUrl", &asked).unwrap()["pending"], true, "the next try");
        assert_eq!(s.take_url_requests().len(), 1);
        // The next run: that failure again, in its place, then what the try that followed gave.
        s.rerun_told.clear();
        let answer: CmdResult = Ok(json!({"layer": id}));
        s.rerun_landed.push((asked.clone(), answer.clone(), Added::of(s.map(), &answer)));
        assert_eq!(s.run("layer.addUrl", &asked), failed);
        for _ in 0..2 {
            assert_eq!(s.run("layer.addUrl", &asked), answer);
        }
        assert!(s.take_url_requests().is_empty(), "nothing asked again");
    }

    /// A document asked for twice in one call is one read, and they wait in the numbers a download
    /// does: each stays in memory until its command has run (#1319).
    #[test]
    fn documents_asked_for_in_a_call_are_one_read_each_and_limited() {
        let mut s = Session::new(crate::sample::world_project());
        s.call_requests = Some(Vec::new());
        assert!(s.left_document("https://x.org/same").unwrap().is_none());
        assert!(s.left_document("https://x.org/same").unwrap().is_none());
        assert_eq!(s.call_requests.as_ref().unwrap().len(), 1);
        for i in 1..crate::MAX_URL_REQUESTS {
            assert!(s.left_document(&format!("https://x.org/{i}")).unwrap().is_none());
        }
        let e = s.left_document("https://x.org/one-more").unwrap_err();
        assert!(e.contains("already being read"), "{e}");
        assert_eq!(s.call_requests.as_ref().unwrap().len(), crate::MAX_URL_REQUESTS);
        // One already asked for is still answered as waiting.
        assert!(s.left_document("https://x.org/same").unwrap().is_none());
    }

    /// `layer.addUrl` can't mark its download as a service's document: nothing would add it, and
    /// nothing would say so. A plugin's call and a direct one are refused, and nothing is read (#1319).
    #[test]
    fn a_download_cannot_be_marked_as_a_document() {
        let mut s = Session::new(crate::sample::world_project());
        s.leave_downloads = true;
        s.call_requests = Some(Vec::new());
        let marked = json!({"url": "https://x.org/a.csv", "document": true});
        assert!(s.run("layer.addUrl", &marked).unwrap_err().contains("internal"));
        assert!(s.call_requests.as_ref().unwrap().is_empty() && s.url_waits.reading.is_empty());
        // Without the mark, it is a download as ever.
        assert_eq!(s.run("layer.addUrl", &json!({"url": "https://x.org/a.csv"})).unwrap()["pending"], true);
    }

    /// A run refused for too many given-up downloads starts none of its documents' reads: with no
    /// command waiting, nothing would take them (#1319).
    #[test]
    fn a_refused_run_starts_no_read() {
        use rustgis_plugins as plugins;
        let pkg = plugins::Package::from_zip(&rustgis_plugins::testing::package("refused-read", &["edit", "network"])).unwrap();
        plugins::load(pkg, "test").unwrap();
        let mut s = Session::new(crate::sample::world_project());
        for n in 0..MAX_EXPIRED as u64 {
            s.url_waits.remember_given_up(n + 1000, json!({"url": format!("https://x.org/{n}")}));
        }
        let forwarded = json!({"op": "service.add", "params": {"url": "https://x.org/wfs", "type": "wfs", "layer": "a"}});
        let e = s.run_leaving_downloads("plugin:refused-read/f", &forwarded).unwrap_err();
        assert!(e.contains("still under way"), "{e}");
        assert!(s.url_waits.reading.is_empty());
        plugins::unload("refused-read");
    }

    /// A document read takes no download's place among those added: past [`MAX_EXPIRED`]
    /// documents, a download added is still refused when it lands again (#1319).
    #[test]
    fn documents_read_keep_no_download_out_of_those_added() {
        let mut waits = UrlWaits::default();
        waits.wait(1, json!({"url": "https://x.org/a.csv"}));
        waits.land(1, Ok(json!({})), Added::default());
        for n in 0..MAX_EXPIRED as u64 {
            waits.wait(n + 2, json!({"url": format!("https://x.org/doc/{n}"), "document": true}));
            waits.land(n + 2, Ok(json!({})), Added { document: Some(Arc::from(&b"caps"[..])), ..Added::default() });
        }
        assert!(waits.added_already(1, &json!({"url": "https://x.org/a.csv", "request": 1})));
    }

    /// A document read off the frame lands for the command waiting for it; one whose command gave
    /// up is dropped and forgotten. An unread one stays out, and asks nothing of the app (#1319).
    #[test]
    fn documents_read_off_the_frame_land_or_are_dropped() {
        let reading = |answer| -> Arc<Mutex<Option<Result<Vec<u8>, String>>>> { Arc::new(Mutex::new(answer)) };
        let doc = |n: u32| json!({"url": format!("https://x.org/{n}"), "document": true});
        let mut waits = UrlWaits::default();
        waits.wait(1, doc(1));
        waits.wait(2, doc(2));
        waits.wait(3, doc(3));
        waits.remember_given_up(4, doc(4));
        // Each read carries the address it fetched, which the error names.
        waits.reading = vec![
            (1, "https://x.org/1".into(), reading(Some(Ok(b"caps".to_vec())))),
            (2, "https://x.org/2?token=hunter2".into(), reading(Some(Err("https://x.org/2?token=hunter2: 404 Not Found".into())))),
            (3, "https://x.org/3".into(), reading(None)),
            (4, "https://x.org/4".into(), reading(Some(Ok(b"late".to_vec())))),
        ];
        waits.documents_in();
        let landed = |n: u64| waits.landed.iter().find(|(m, ..)| *m == n);
        assert_eq!(landed(1).unwrap().3.document.as_deref(), Some(&b"caps"[..]));
        let e = landed(2).unwrap().2.as_ref().unwrap_err();
        assert!(e.contains("404") && !e.contains("hunter2"), "{e}");
        assert!(landed(3).is_none() && waits.reading.len() == 1 && waits.reading[0].0 == 3);
        assert!(landed(4).is_none() && !waits.given_up(4, &doc(4)), "given up, it is forgotten");
        assert!(is_document(&doc(1)) && !is_document(&json!({"url": "https://x.org/1"})));
    }

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
        assert!(e.contains("removed or undone"), "{e}");
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
        assert!(e.contains("removed or undone"), "{e}");
        // A standalone table (a file with no geometry), then taken away.
        let answer = s.run("layer.addDataText", &json!({"fileName": "sites.csv", "text": "id,street\n1,Champ de Mars\n"}));
        assert!(answer.as_ref().unwrap()["table"].is_string(), "{answer:?}");
        s.rerun_landed = vec![(asked.clone(), answer.clone(), Added::of(s.map(), &answer))];
        assert_eq!(s.run("layer.addUrl", &asked), answer);
        s.run("table.remove", &json!({"table": "sites"})).unwrap();
        let e = s.run("layer.addUrl", &asked).unwrap_err();
        assert!(e.contains("the table its download added was removed or undone"), "{e}");
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

    fn load_test_plugin(id: &str) -> String {
        let package = rustgis_plugins::Package::from_zip(&rustgis_plugins::testing::package(id, &["edit", "network"])).unwrap();
        rustgis_plugins::load(package, "test").unwrap();
        format!("plugin:{id}/f")
    }

    fn ask(s: &mut Session, command: &str, params: Value) -> CmdResult {
        s.run_leaving_downloads(command, &json!({"op":"layer.addUrl", "params":params}))
    }

    /// Timeouts are one counted, redacted notice, and a real pinned command receives the failure.
    #[test]
    fn a_download_given_up_is_said_with_its_address() {
        let mut s = Session::new(crate::project::Project::default());
        let first = load_test_plugin("eng-slow-real");
        let other = load_test_plugin("eng-slow-other");
        ask(&mut s, &first, json!({"url":"https://x.org/slow.csv?token=hunter2"})).unwrap();
        let taken = s.take_url_requests();
        let mark = s.notice_mark();
        s.poll_plugin_retries();
        assert!(s.notices_since(mark).is_empty(), "it still has time");
        let ran = s.poll_plugin_retries_at(Instant::now() + REQUEST_WAIT);
        assert!(ran[0].1.is_err(), "{ran:?}");
        s.poll_plugin_retries();
        let said: Vec<_> = s.notices_since(mark).iter().filter(|n| n.error && n.text.contains("stopped waiting")).collect();
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(said[0].text.starts_with("https://x.org/slow.csv") && !said[0].text.contains("hunter2"));
        // A real late claim is refused before its bytes can add anything.
        let id = s.download_started_for(&taken[0]).unwrap();
        assert!(s.download_landed(id, b"x,y\n1,2\n".to_vec()));
        let mut p = taken[0].params().clone();
        p["id"] = json!(id);
        assert!(s.complete_download(id, &p).unwrap_err().contains("stopped waiting"));
        assert!(s.downloaded.is_empty() && s.map().layers.is_empty());
        ask(&mut s, &first, json!({"url":"https://x.org/a.csv?token=hunter2"})).unwrap();
        ask(&mut s, &other, json!({"url":"https://x.org/b.csv"})).unwrap();
        let _taken = s.take_url_requests();
        let mark = s.notice_mark();
        s.poll_plugin_retries_at(Instant::now() + REQUEST_WAIT);
        let said: Vec<_> = s.notices_since(mark).iter().filter(|n| n.error && n.text.contains("stopped waiting")).collect();
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(
            said[0].text.starts_with("2 downloads")
                && said[0].text.contains("https://x.org/a.csv")
                && said[0].text.contains("https://x.org/b.csv")
        );
        assert!(!said[0].text.contains("hunter2"));
        rustgis_plugins::unload("eng-slow-real");
        rustgis_plugins::unload("eng-slow-other");
    }

    #[test]
    fn a_waiting_answer_hides_the_keys_of_its_addresses() {
        let mut s = Session::new(crate::project::Project::default());
        let command = load_test_plugin("eng-keys-real");
        let doc = "https://x.org/doc.json?token=hunter2";
        let cache_key = key("eng-keys-real", doc);
        cache().in_flight.push((cache_key.clone(), Instant::now()));
        TEST.with(|t| t.set((false, false)));
        let r = s.run_leaving_downloads(&command, &json!({"op":"host.fetch","params":{"url":doc}})).unwrap();
        TEST.with(|t| t.set((false, true)));
        assert!(r["fetching"][0].as_str().unwrap().starts_with("https://x.org/doc.json") && !r.to_string().contains("hunter2"));
        let r = ask(&mut s, &command, json!({"url":"https://x.org/a.csv?token=hunter3"})).unwrap();
        assert!(r["downloading"][0].as_str().unwrap().starts_with("https://x.org/a.csv") && !r.to_string().contains("hunter3"));
        let taken = s.take_url_requests();
        assert!(taken[0].params()["url"].as_str().unwrap().contains("hunter3"), "the actual request remains exact");
        cache().in_flight.retain(|(k, _)| k != &cache_key);
        rustgis_plugins::unload("eng-keys-real");
    }

    /// Refusal requires a real claim and cannot consume a different caller's bytes by number.
    #[test]
    fn a_refused_landing_hides_the_keys_of_its_address() {
        let mut s = Session::new(crate::project::Project::default());
        let command = load_test_plugin("eng-refuse-real");
        let url = "https://x.org/e.geojson?token=hunter2";
        ask(&mut s, &command, json!({"url":url})).unwrap();
        let taken = s.take_url_requests();
        let id = s.download_started_for(&taken[0]).unwrap();
        let n = taken[0].params()["request"].as_u64().unwrap();
        ask(&mut s, &command, json!({"url":"https://x.org/next.csv"})).unwrap();
        assert!(s.url_waits.given_up(n, taken[0].params()));
        assert!(s.download_landed(id, br#"{"type":"Point","coordinates":[1,2]}"#.to_vec()));
        let mut p = taken[0].params().clone();
        p["id"] = json!(id);
        s.merge_undo("late download");
        s.run("map.rename", &json!({"name":"Before late completion"})).unwrap();
        s.merge_undo("late download");
        let e = s.complete_download(id, &p).unwrap_err();
        assert!(e.starts_with("https://x.org/e.geojson") && e.contains("stopped waiting") && !e.contains("hunter2"), "{e}");
        let before = s.undo_depth();
        s.run("map.rename", &json!({"name":"After late completion"})).unwrap();
        assert_eq!(s.undo_depth(), before + 1, "a pre-guard completion refusal must consume its merge key");
        s.run("edit.undo", &Value::Null).unwrap();
        assert_eq!(s.map().name, "Before late completion");
        assert!(s.map().layers.is_empty() && s.downloaded.is_empty());
        rustgis_plugins::unload("eng-refuse-real");
    }

    /// Bookkeeping matches imports exactly and keeps bounded tombstones until a late delivery.
    #[test]
    fn landed_downloads_answer_every_waiting_run() {
        let mut waits = UrlWaits::default();
        let asked = json!({"url":"https://x.org/f.csv", "x":"lon", "anonymous":true});
        waits.wait(905, asked.clone());
        assert!(!waits.holds(905, &json!({"url":"https://x.org/f.csv", "x":"lng", "anonymous":true})));
        assert!(waits.holds(905, &json!({"url":"https://x.org/f.csv", "x":"lon", "anonymous":true,"id":3,"cog":true})));
        waits.land(905, Ok(json!({"layer":7})), Added::default());
        assert_eq!(waits.in_flight(&asked, |_| true), Some(905));
        assert_eq!(waits.in_flight(&asked, |_| false), None, "another owner cannot share it");
        assert!(waits.added_already(905, &asked));
        let passed = json!({"url":"https://x.org/g.csv","x":"lon","whole":true,"zoom":false,"dates":["2020"],"name":5,"bounds":"x"});
        waits.wait(906, passed);
        for zoom in [None, Some(true), Some(false)] {
            let mut p = json!({"url":"https://x.org/g.csv","x":"lon","id":4});
            if let Some(zoom) = zoom {
                p["zoom"] = json!(zoom);
            }
            assert!(waits.holds(906, &p));
        }
        assert!(!waits.holds(906, &json!({"url":"https://x.org/g.csv"})));
        waits.give_up(906, true);
        assert!(waits.given_up(906, &json!({"url":"https://x.org/g.csv","x":"lon"})));
        waits.refused(906);
        assert!(!waits.given_up(906, &json!({"url":"https://x.org/g.csv","x":"lon"})));
        let first = json!({"url":"https://x.org/out.csv"});
        waits.remember_given_up(800, first.clone());
        for n in 1..MAX_EXPIRED as u64 {
            waits.remember_given_up(10_000 + n, json!({"url":format!("https://x.org/{n}.csv")}));
        }
        assert!(waits.given_up_full() && waits.given_up(800, &first));
        waits.refused(800);
        assert!(!waits.given_up_full());
        let mut s = Session::new(crate::project::Project::default());
        let command = load_test_plugin("eng-full-real");
        ask(&mut s, &command, json!({"url":"https://x.org/kept.csv"})).unwrap();
        for n in 0..MAX_EXPIRED as u64 {
            s.url_waits.remember_given_up(10_000 + n, json!({"url":format!("https://x.org/{n}.csv")}));
        }
        let e = ask(&mut s, &command, json!({"url":"https://x.org/refused.csv"})).unwrap_err();
        assert!(e.contains("still under way") && e.contains("wasn't started"), "{e}");
        let kept = s.take_url_requests();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].params()["url"], "https://x.org/kept.csv");
        assert!(s.plugin_retries_waiting() && s.poll_plugin_retries().is_empty());
        rustgis_plugins::unload("eng-full-real");
        for cancelled in
            [vec!["https://x.org/a?token=hunter2".into()], vec!["https://x.org/a?token=hunter2".into(), "https://x.org/b".into()]]
        {
            assert!(!not_started(&cancelled).contains("hunter2"));
        }
    }

    /// Sharing is bounded by the original plugin incarnation; duplicate clicks of that owner
    /// share one download, while another plugin gets its own real request and answer.
    #[test]
    fn a_failed_download_asked_again_is_downloaded_again() {
        let mut s = Session::new(crate::project::Project::default());
        let a = load_test_plugin("eng-share-real-a");
        let b = load_test_plugin("eng-share-real-b");
        let p = json!({"url":"https://x.org/a.csv"});
        ask(&mut s, &a, p.clone()).unwrap();
        let first = s.take_url_requests();
        ask(&mut s, &a, p.clone()).unwrap();
        assert!(s.take_url_requests().is_empty(), "same pinned owner shares its outstanding request");
        ask(&mut s, &b, p.clone()).unwrap();
        let other = s.take_url_requests();
        assert_ne!(first[0].params()["request"], other[0].params()["request"], "different plugin pins do not share authority");
        s.request_failed(first[0].params(), "404");
        let ran = s.poll_plugin_retries();
        assert_eq!(ran.len(), 1);
        assert_eq!(ran[0].0, a);
        assert!(ran[0].1.as_ref().unwrap_err().contains("404"));
        assert!(s.plugin_retries_waiting(), "the other owner's wait is intact");
        ask(&mut s, &a, p).unwrap();
        let again = s.take_url_requests();
        assert_ne!(again[0].params()["request"], first[0].params()["request"]);
        rustgis_plugins::unload("eng-share-real-a");
        rustgis_plugins::unload("eng-share-real-b");
    }

    /// Payload numbers never select an existing wait or authority. Native requests still get
    /// distinct engine receipts, and non-object requests are refused.
    #[test]
    fn a_number_given_for_another_address_is_set_aside() {
        let mut s = Session::new(crate::project::Project::default());
        s.request_url(&json!({"url":"https://x.org/a.geojson"})).unwrap();
        let first = s.take_url_requests();
        let n = first[0].params()["request"].as_u64().unwrap();
        s.request_url(&json!({"url":"https://x.org/a.geojson","request":n})).unwrap();
        s.request_url(&json!({"url":"https://x.org/b.geojson","request":n})).unwrap();
        let taken = s.take_url_requests();
        assert_eq!(taken.len(), 2);
        assert_ne!(taken[0].params()["request"], json!(n));
        assert_ne!(taken[1].params()["request"], taken[0].params()["request"]);
        assert!(s.request_url(&json!("https://x.org/d.geojson")).unwrap_err().contains("an object"));
    }

    /// In-place dispatch shares the same terminal result hook as downloaded-byte completion.
    #[test]
    fn an_owned_in_place_failure_settles_its_wait() {
        let mut s = Session::new(crate::project::Project::default());
        s.request_url(&json!({"url":""})).unwrap();
        let taken = s.take_url_requests();
        let p = taken[0].params();
        let n = p["request"].as_u64().unwrap();
        s.url_waits.wait(n, without_request(p));
        let id = s.download_started_for(&taken[0]).unwrap();
        let error = s.add_in_place_for(id, p).unwrap_err();
        assert!(error.contains("url is required"), "{error}");
        assert!(s.url_waits.landed.iter().any(|(m, _, answer, _)| *m == n && answer.is_err()));
        assert!(!s.url_waits.holds(n, p));
    }

    /// The ordinary pending-download cap can retire a plugin-owned claim below the claim cap.
    #[test]
    fn retiring_a_download_claim_settles_only_its_own_wait() {
        let mut s = Session::new(crate::project::Project::default());
        let mut requests = Vec::new();
        for url in ["https://retired.invalid/first.geojson", "https://retired.invalid/second.geojson"] {
            s.request_url(&json!({"url":url})).unwrap();
            let request = s.take_url_requests().pop().unwrap();
            let n = request.params()["request"].as_u64().unwrap();
            s.url_waits.wait(n, without_request(request.params()));
            let id = s.download_started_for(&request).unwrap();
            requests.push((id, n, request));
        }
        let (first, number, request) = &requests[0];
        let still = s.download_still(*first).unwrap();
        struct Live;
        impl crate::policy::Revocation for Live {
            fn allows(&self, _: crate::policy::Effects) -> bool {
                true
            }
        }
        let foreign = crate::policy::Scope::assistant(1521, std::sync::Arc::new(Live));
        s.with_scope(foreign, |s| {
            s.download_failed(*first);
            assert!(!s.download_done(*first));
            assert!(s.download_continues(*first).is_err());
            assert!(!s.download_landed(*first, Vec::new()));
        })
        .unwrap();
        assert!(still() && s.url_waits.holds(*number, request.params()), "foreign callbacks cannot consume the owner's claim or wait");
        for _ in 0..62 {
            s.download_started();
        }
        assert_eq!(
            requests.iter().filter(|(id, ..)| s.download_still(*id).is_some()).count(),
            2,
            "both original claims remain below the cap"
        );
        let mark = s.notice_mark();
        s.download_started();
        assert!(!still(), "retirement revokes its transport predicate");
        assert!(!s.url_waits.holds(*number, request.params()), "the retired request no longer blocks its plugin");
        let (_, other, request) = &requests[1];
        assert!(s.url_waits.holds(*other, request.params()), "the other wait is untouched");
        assert!(s.url_waits.landed.iter().any(|(n, _, answer, _)| n == number && answer.is_err()));
        assert_eq!(s.notices_since(mark).len(), 1, "retirement is reported once");
        s.download_failed(requests[1].0);
        assert!(!s.url_waits.holds(*other, request.params()), "an explicit failure also settles its exact wait");
        assert!(s.url_waits.landed.iter().any(|(n, _, answer, _)| n == other && answer.is_err()));
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
