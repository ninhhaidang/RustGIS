//! The tile cache: every tile and document fetched, decoded, by address.
//!
//! Drawing never waits. [`get`] answers what the cache holds and, for an address it doesn't hold
//! yet, starts fetching it; when the bytes arrive they are decoded, [`generation`] goes up and
//! the waker (the app's repaint) is called, so the map draws again with the new tile. Exports
//! and the command line, which can't draw a half-loaded map, [`wait`] for the fetches first.
//!
//! Tiles are fetched a limited number at a time from each server, the newest request first (the
//! view the user is looking at now, not the one they panned away from), and a waiting tile the
//! map stopped asking for is dropped unfetched ([`crate::schedule`]). Natively they are fetched on
//! worker threads; the browser app installs its own fetcher with [`set_fetcher`].

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use rustgis_data::diagnostics::Capture;
use tiny_skia::Pixmap;
use web_time::{Duration, Instant};

use crate::failure::Failure;
use crate::mvt;
use crate::schedule::{Queue, Waiting};

/// What a fetched address is decoded as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Want {
    /// A PNG, JPEG or WebP tile.
    Image,
    /// A Mapbox Vector Tile.
    Vector,
    /// A JSON document (a style or TileJSON).
    Json,
}

/// A decoded fetch.
pub enum Payload {
    Image(Pixmap),
    Vector(mvt::Tile),
    Json(serde_json::Value),
    /// No content (a 204, or an empty body): nothing to draw.
    Empty,
}

/// What the cache knows about an address.
#[derive(Clone)]
pub enum State {
    Ready(Arc<Payload>),
    Pending,
    Failed(String),
}

/// Called once with a fetch's bytes or error.
pub type Done = Box<dyn FnOnce(Result<Vec<u8>, String>) + Send>;
/// Called once with a download's bytes or what is known of its failure (#509).
pub type DoneDetailed = Box<dyn FnOnce(Result<Vec<u8>, Failure>) + Send>;
// The carrier lives with source I/O so lower-layer workers inherit the same context.
pub use rustgis_data::source::remote::{Still, check_still, current_still, with_optional_still, with_still};
fn check_current() -> Result<(), String> {
    check_still(&current_still())
}

/// Only a live request's failure describes the shared resource. A dropped or retired read
/// must let the next caller retry immediately under its own authority, without classifying cancellation by error text.
pub(crate) fn keep_failure(message: &str, still: &Option<Still>) -> bool {
    message != DROPPED && check_still(still).is_ok()
}

/// Shared cache callbacks retain their caller across ready hits and loading waiters. A loader
/// may narrow that lifetime, but cannot replace it when the callback starts another read.
pub(crate) fn caller_callback<T: Send + 'static>(
    then: Box<dyn FnOnce(Result<T, String>) + Send>,
) -> Box<dyn FnOnce(Result<T, String>) + Send> {
    let still = current_still();
    Box::new(move |result| {
        with_optional_still(still, || {
            then(check_still(&current_still()).and(result));
        });
    })
}

type Fetcher = Box<dyn Fn(String, Done, &'static [u16]) + Send + Sync>;
/// Hands back a decoded picture: [`RAW`] pixels, or the encoded bytes it was given when it
/// couldn't decode them (they are decoded here then).
pub type Decoded = Box<dyn FnOnce(Result<Vec<u8>, Vec<u8>>) + Send>;
type ImageDecoder = Box<dyn Fn(Vec<u8>, Decoded) + Send + Sync>;

/// Bytes the cache keeps before dropping the least recently used entries.
const BUDGET: usize = if cfg!(target_arch = "wasm32") { 160 << 20 } else { 384 << 20 };
/// A failed address is tried again after this long.
pub const RETRY: Duration = Duration::from_secs(60);
/// The largest tile or document fetched.
pub const MAX_BYTES: usize = 16 << 20;
/// The error a fetcher gives a request it dropped unfetched (the view moved on): the address is
/// forgotten rather than failed, so it is fetched again when it is next wanted.
pub const DROPPED: &str = "dropped: the view moved on";
/// The end of a fetcher's error when a range was asked for and the server answered with the
/// whole file: a file read in place by range is downloaded whole instead.
pub const NO_RANGES: &str = "the server doesn't answer byte-range requests";

enum Entry {
    Ready(Arc<Payload>, usize),
    Pending,
    Failed(String, Instant),
}

struct Slot {
    entry: Entry,
    used: u64,
    /// When the map last asked for it ([`get`]): a waiting tile not asked for lately is dropped.
    asked: Instant,
}

#[derive(Default)]
struct Cache {
    slots: HashMap<String, Slot>,
    bytes: usize,
    tick: u64,
    /// When the map last asked for any address.
    latest: Option<Instant>,
}

fn cache() -> &'static Mutex<Cache> {
    static C: OnceLock<Mutex<Cache>> = OnceLock::new();
    C.get_or_init(Mutex::default)
}

fn lock() -> std::sync::MutexGuard<'static, Cache> {
    cache().lock().unwrap_or_else(|e| e.into_inner())
}

static GENERATION: AtomicU64 = AtomicU64::new(0);
static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

fn fetcher() -> &'static Mutex<Option<Arc<Fetcher>>> {
    static F: OnceLock<Mutex<Option<Arc<Fetcher>>>> = OnceLock::new();
    F.get_or_init(Mutex::default)
}

fn image_decoder() -> &'static Mutex<Option<Arc<ImageDecoder>>> {
    static D: OnceLock<Mutex<Option<Arc<ImageDecoder>>>> = OnceLock::new();
    D.get_or_init(Mutex::default)
}

fn waker() -> &'static Mutex<Option<Arc<dyn Fn() + Send + Sync>>> {
    static W: OnceLock<Mutex<Option<Arc<dyn Fn() + Send + Sync>>>> = OnceLock::new();
    W.get_or_init(Mutex::default)
}

/// Replaces how addresses are fetched (the browser's `fetch`). `done` must be called once.
/// Decode download markers with [`split_download_limit`] and enforce their budget while reading.
pub fn set_fetcher(f: impl Fn(String, Done) + Send + Sync + 'static) {
    set_fetcher_omitting(move |url, done, _| f(url, done));
}

/// Like [`set_fetcher`], also handing the fetcher the statuses its caller expects (a tile past a
/// map's edge): a transport keeping diagnostics passes them to
/// [`RequestDiagnostics::omit_statuses`], so it records what the native transport records.
pub fn set_fetcher_omitting(f: impl Fn(String, Done, &'static [u16]) + Send + Sync + 'static) {
    *fetcher().lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(Box::new(f)));
}

/// Undoes [`set_fetcher`]: addresses are fetched natively again (a test done with its own).
pub fn clear_fetcher() {
    *fetcher().lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Tests that fetch through the cache take turns. It has one fetcher for the whole process, so a
/// test that replaces it ([`set_fetcher`]) would answer another's fetches. It also starts fetches
/// on whichever thread is starting them already, so one test's fetch may run on another's thread,
/// later. Tests that end a login take turns too, as that empties the cache ([`clear`]), and so do
/// tests that wait for or plant what the cache holds (an archive read, a style resolved), which
/// that would drop. Tests that inspect clean failure text also take turns: a concurrent login
/// can redact even a single letter from that text. The turn puts the native fetcher back when it ends.
#[cfg(test)]
pub(crate) fn turn() -> Turn {
    static TURN: Mutex<()> = Mutex::new(());
    Turn { _held: TURN.lock().unwrap_or_else(|e| e.into_inner()) }
}

/// A test's turn at the cache ([`turn`]).
#[cfg(test)]
pub(crate) struct Turn {
    _held: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl Drop for Turn {
    fn drop(&mut self) {
        clear_fetcher();
    }
}

/// Replaces how tile pictures (PNG, JPEG, WebP) are decoded: the browser's own decoder works
/// off the page's thread, where decoding them here held up every frame tiles arrived in. `done`
/// must be called once, with the picture as [`RAW`] pixels, or with the bytes given back.
pub fn set_image_decoder(f: impl Fn(Vec<u8>, Decoded) + Send + Sync + 'static) {
    *image_decoder().lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(Box::new(f)));
}

/// Whether `bytes` are a PNG, JPEG or WebP picture of at most 4096 pixels a side, by its header:
/// a larger one is left to [`decode_image`], whose limits refuse it before it takes the memory.
fn encoded_picture(bytes: &[u8]) -> bool {
    let known = bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || bytes.starts_with(&[0xff, 0xd8, 0xff])
        || (bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"));
    // Reading the header can allocate (a PNG's colour profile is inflated with it): no more
    // than decoding the picture here may.
    known
        && image::ImageReader::new(std::io::Cursor::new(bytes))
            .with_guessed_format()
            .ok()
            .and_then(|mut r| {
                r.limits(picture_limits());
                r.into_dimensions().ok()
            })
            .is_some_and(|(w, h)| (1..=4096).contains(&w) && (1..=4096).contains(&h))
}

/// Sets what is called whenever a fetch finishes (the app asks for a repaint).
pub fn set_waker(f: impl Fn() + Send + Sync + 'static) {
    *waker().lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(f));
}

/// When the map is next to draw again by itself (a failed read may be tried again then): each
/// wait asked for ([`notify_at`]), in milliseconds after [`alarm_base`] plus one, the earliest
/// first. Every wait is kept (the earliest [`MAX_ALARMS`]), not the earliest alone: the draw when
/// it is over may not ask again for a later one (a style kept whole while its elevation source
/// waits).
fn alarms() -> &'static Mutex<BTreeSet<u64>> {
    static A: OnceLock<Mutex<BTreeSet<u64>>> = OnceLock::new();
    A.get_or_init(Mutex::default)
}

/// The most waits kept at once: a later one past them all is left to its read, which asks again
/// on each draw while it waits.
const MAX_ALARMS: usize = 1024;

fn alarm_base() -> Instant {
    static B: OnceLock<Instant> = OnceLock::new();
    *B.get_or_init(Instant::now)
}

/// `when` as the alarms keep it.
fn alarm_at(when: Instant) -> u64 {
    (when.saturating_duration_since(alarm_base()).as_millis() as u64).saturating_add(1)
}

fn alarm_hook() -> &'static Mutex<Option<Arc<dyn Fn(Duration) + Send + Sync>>> {
    static A: OnceLock<Mutex<Option<Arc<dyn Fn(Duration) + Send + Sync>>>> = OnceLock::new();
    A.get_or_init(Mutex::default)
}

/// Sets how the app is asked to draw again after a while (a repaint after it): a read that failed
/// is tried again once its wait is over, though nothing else draws the map. The request lasts
/// one frame (egui's `request_repaint_after`): it is made again on each draw while the wait
/// lasts (see [`generation`]).
pub fn set_alarm(f: impl Fn(Duration) + Send + Sync + 'static) {
    *alarm_hook().lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(f));
}

/// Asks the app to draw again after `after` (see [`set_alarm`]).
fn ring(after: Duration) {
    let hook = alarm_hook().lock().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(hook) = hook {
        hook(after);
    }
}

/// Draws the map again at `when`: [`generation`] goes up then, so a map waiting on a failed read
/// asks for it again. The app is asked for the earliest wait pending; each draw asks it again
/// for the earliest left (see [`generation`]).
pub fn notify_at(when: Instant) {
    let at = alarm_at(when);
    let earliest = {
        let mut a = alarms().lock().unwrap_or_else(|e| e.into_inner());
        if a.len() >= MAX_ALARMS {
            // Full: a wait past every one kept is left to its read; an earlier one takes the
            // latest's place.
            if a.last().is_some_and(|last| at >= *last) {
                return;
            }
            a.pop_last();
        }
        a.insert(at);
        a.first() == Some(&at)
    };
    if earliest {
        ring(when.saturating_duration_since(Instant::now()));
    }
}

/// Goes up each time a fetch finishes, and once a wait asked for ([`notify_at`]) is over: a map
/// drawn at an older generation may now draw more. Read as the map draws, it asks the app to draw
/// again at the earliest wait still pending: the app's request lasts one frame (egui's
/// `request_repaint_after`), and a draw before the wait was over (the pointer moved) used it up.
pub fn generation() -> u64 {
    let now = alarm_at(Instant::now());
    let next = {
        let mut a = alarms().lock().unwrap_or_else(|e| e.into_inner());
        let (over, next) = take_due(&mut a, now);
        if over {
            // Every wait over is one draw: the generation goes up once for them all.
            GENERATION.fetch_add(1, Ordering::Relaxed);
        }
        next
    };
    if let Some(at) = next {
        ring(Duration::from_millis(at.saturating_sub(now)));
    }
    GENERATION.load(Ordering::Relaxed)
}

/// Consumes every elapsed deadline together and returns the next one to rearm. Keeping the
/// clock as an input lets the ordering contract be tested without scheduler deadlines.
fn take_due(alarms: &mut std::collections::BTreeSet<u64>, now: u64) -> (bool, Option<u64>) {
    let over = alarms.first().is_some_and(|at| *at <= now);
    if over {
        *alarms = alarms.split_off(&now.saturating_add(1));
    }
    (over, alarms.first().copied())
}

/// How long until the earliest wait pending; none when there is none.
#[cfg(test)]
pub(crate) fn next_alarm() -> Option<Duration> {
    let at = alarms().lock().unwrap_or_else(|e| e.into_inner()).first().copied()?;
    Some(Duration::from_millis(at.saturating_sub(alarm_at(Instant::now()))))
}

/// Forgets every wait pending.
#[cfg(test)]
pub(crate) fn clear_alarms() {
    alarms().lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// Undoes [`set_alarm`]: a test done with its own.
#[cfg(test)]
pub(crate) fn clear_alarm() {
    *alarm_hook().lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// A test's turn at the process-wide alarms, held by the tests that arm, read or clear them, or
/// set what rings: they run in parallel threads of one process, and one's clearing would erase
/// another's.
#[cfg(test)]
pub(crate) fn alarm_turn() -> std::sync::MutexGuard<'static, ()> {
    static L: Mutex<()> = Mutex::new(());
    L.lock().unwrap_or_else(|e| e.into_inner())
}

/// How long a server that refused with 429 or 503 asked to be left before it is asked again, as
/// the error's text says it (`http status: 503 (retry after 30 s)`; see
/// [`crate::failure::with_retry_after`]).
pub fn retry_after(error: &str) -> Option<Duration> {
    let (_, rest) = error.rsplit_once("(retry after ")?;
    let secs: u64 = rest.strip_suffix(" s)")?.parse().ok()?;
    // Bounded as when written: a text from elsewhere (an address echoed) can't ask for years.
    Some(Duration::from_secs(secs.clamp(1, crate::failure::MAX_RETRY_AFTER)))
}

/// How long a failed read waits before it is tried again: what its server asked for (see
/// [`retry_after`]), else `default`.
pub fn retry_wait(error: &str, default: Duration) -> Duration {
    retry_after(error).unwrap_or(default)
}

/// How long a read that has failed `failures` times in a row waits before the next try: `wait`,
/// doubled for each failure past the first, within the hour (see [`retry_after`]'s bound), so a
/// file that never reads isn't asked for every minute all session.
pub fn backoff(wait: Duration, failures: u32) -> Duration {
    let most = Duration::from_secs(crate::failure::MAX_RETRY_AFTER);
    // Past twelve doublings the wait is over the hour whatever it was (it is a second at least).
    let doublings = failures.saturating_sub(1).min(12);
    wait.saturating_mul(1u32 << doublings).min(most)
}

/// How many fetches are under way. Once a fetch no longer counts, its [`generation`] shows.
pub fn in_flight() -> usize {
    IN_FLIGHT.load(Ordering::Acquire)
}

/// What the cache holds for `url`, fetching it when it holds nothing (or a failure old enough
/// to try again). Every failed fetch is recorded in the asking session's diagnostics: use
/// [`get_tile`] for a map tile, whose 404 is routine.
pub fn get(url: &str, want: Want) -> State {
    request(url, want, &[])
}

/// Like [`get`], for a map tile: a server's 404 (no tile there) or 410 (no longer there) for it
/// is routine (the map asks past an area's edges, and the map draws both as a gap: see
/// [`crate::warp::missing`]), so it isn't recorded as a failure; each would flood the history
/// and evict real failures. The omission is noted once per server instead. A picture that isn't
/// a tile (a single-image layer, a layout's picture, a style's sprite sheet) goes through
/// [`get`], so its 404 is recorded.
pub fn get_tile(url: &str, want: Want) -> State {
    request(url, want, TILE_OMIT)
}

/// The statuses a map tile's request omits ([`get_tile`]).
const TILE_OMIT: &[u16] = &[404, 410];

/// [`get`] with the statuses that are expected answers for this fetch.
fn request(url: &str, want: Want, omit: &'static [u16]) -> State {
    if let Err(e) = crate::logins::check(resource_of(url)) {
        return State::Failed(e.to_string());
    }
    // A map tile of a layer in another system: its service's picture as it comes, or made from
    // its pictures (#769).
    if let Some(base) = crate::warp::direct(url) {
        return request(base, want, omit);
    }
    let now = Instant::now();
    let mut c = lock();
    c.tick += 1;
    let tick = c.tick;
    c.latest = Some(now);
    if let Some(slot) = c.slots.get_mut(url) {
        slot.used = tick;
        slot.asked = now;
        match &slot.entry {
            Entry::Ready(p, _) => return State::Ready(p.clone()),
            Entry::Pending => return State::Pending,
            Entry::Failed(e, at) if at.elapsed() < retry_wait(e, RETRY) => {
                // A server that said when to come back (429 or 503 with `Retry-After`) is asked
                // again then: the map draws again at that time, though nothing else draws it.
                // One that didn't is asked again by the next draw after a minute, as before.
                if let Some(wait) = retry_after(e) {
                    notify_at(*at + wait);
                }
                return State::Failed(e.clone());
            }
            Entry::Failed(..) => {}
        }
    }
    if crate::warp::is_warp(url) {
        drop(c);
        // Made once its pictures are here; asked again (as the map draws) until then.
        return match crate::warp::make(url) {
            crate::warp::Made::Waiting => State::Pending,
            crate::warp::Made::Done(payload) => {
                let payload = Arc::new(payload);
                let n = payload_bytes(&payload);
                store(url, Entry::Ready(payload.clone(), n));
                State::Ready(payload)
            }
            crate::warp::Made::ForNow(payload) => State::Ready(payload),
            crate::warp::Made::Failed(e) => {
                store(url, Entry::Failed(e.clone(), Instant::now()));
                State::Failed(e)
            }
        };
    }
    let asked_at = crate::mbtiles::epoch();
    c.slots.insert(url.to_string(), Slot { entry: Entry::Pending, used: tick, asked: now });
    drop(c);
    start(url.to_string(), want, omit, asked_at);
    State::Pending
}

/// What the cache holds for `url`, without fetching it.
pub fn peek(url: &str) -> Option<Arc<Payload>> {
    crate::logins::check(resource_of(url)).ok()?;
    if let Some(base) = crate::warp::direct(url) {
        return peek(base);
    }
    let mut c = lock();
    c.tick += 1;
    let tick = c.tick;
    let slot = c.slots.get_mut(url)?;
    match &slot.entry {
        Entry::Ready(p, _) => {
            slot.used = tick;
            Some(p.clone())
        }
        _ => None,
    }
}

/// Forgets everything (and failures, so they are tried again).
pub fn clear() {
    let mut c = lock();
    c.slots.retain(|_, s| matches!(s.entry, Entry::Pending));
    c.bytes = 0;
    drop(c);
    // Styles, TileJSON documents and PMTiles archives are read again too.
    crate::source::forget();
    crate::picture::forget();
    crate::draw::forget_boxes();
    crate::sprite::forget();
    crate::pmtiles::forget();
    crate::mbtiles::forget();
    crate::cog::forget();
    crate::warp::forget();
    rustgis_data::source::remote::forget();
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// The resource a cache key reads: its address without a download budget, a byte range or a
/// tail, and for a tile of a PMTiles, COG or MBTiles archive, the archive's address.
fn resource_of(key: &str) -> &str {
    let key = split_download_limit(key).0;
    let key = split_tail(key).0;
    let key = split_range(key).0;
    // An MBTiles file's path whole, as its tiles and the record of forgotten files name it: a
    // `#` in a file's name isn't a COG's look.
    if crate::mbtiles::is_tile(key) {
        return key.split_once('@').map_or(key, |(_, archive)| archive);
    }
    if key.starts_with(crate::pmtiles::SCHEME) || key.starts_with(crate::cog::SCHEME) {
        return key.split_once('@').map_or(key, |(_, archive)| crate::cog::split(archive).0);
    }
    key
}

/// Forgets what was read of one address (and of the archive or file it names), so it is read
/// again, as [`clear`] does for everything: after the login that signs it changed (a source
/// whose address stays the same, read again with other keys). Reads under way are kept, except an
/// MBTiles file's tiles: it is forgotten when it may be replaced, and its old tiles mustn't stay.
pub fn forget(url: &str) {
    let url = split_download_limit(url).0;
    // An MBTiles file's path is taken whole (see [`resource_of`]).
    let url = if crate::mbtiles::check(url).is_ok() { url } else { crate::cog::split(url).0 };
    // First: a tile answered from here on is dropped, and one answered before goes with the rest.
    crate::mbtiles::forget_archive(url);
    let mut c = lock();
    let dropped: usize = c
        .slots
        .iter()
        .filter(|(k, s)| !matches!(s.entry, Entry::Pending) && resource_of(k) == url)
        .map(|(_, s)| match &s.entry {
            Entry::Ready(_, n) => *n,
            _ => 0,
        })
        .sum();
    c.slots.retain(|k, s| resource_of(k) != url || (matches!(s.entry, Entry::Pending) && !crate::mbtiles::is_tile(k)));
    c.bytes = c.bytes.saturating_sub(dropped);
    drop(c);
    crate::pmtiles::forget_archive(url);
    crate::cog::forget_file(url);
    // Map tiles made from a service's pictures, kept only while one is missing or on its way,
    // and their plans: all are made again (those kept in the cache stay till `clear`).
    crate::warp::forget();
    rustgis_data::source::remote::forget_file(url);
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// Waits (up to `timeout`) for the fetches under way to finish. Native only: in the browser
/// fetches finish between frames, so this returns at once.
pub fn wait(timeout: Duration) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let start = Instant::now();
        while in_flight() > 0 && start.elapsed() < timeout {
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
    }
    #[cfg(target_arch = "wasm32")]
    let _ = timeout;
}

/// How long nothing more has to land, after something has, for [`wait_for_news`] to return.
#[cfg(not(target_arch = "wasm32"))]
const QUIET: Duration = Duration::from_millis(100);
/// How long [`wait_for_news`] waits at most once something has landed, however much more does
/// (other maps' fetches landing one after another).
#[cfg(not(target_arch = "wasm32"))]
const SETTLE: Duration = Duration::from_secs(1);

/// Waits (up to `timeout`) for the fetches under way to finish, or until something has landed
/// since the [`generation`] `since` (read before the caller last drew) and then nothing more for
/// a moment, or for a second at most: one drawing again with what came needn't wait for every
/// fetch in the process (another map's, a slow server's, one never answered). Native only.
pub fn wait_for_news(since: u64, timeout: Duration) {
    wait_for_news_unless(since, timeout, || false);
}

/// [`wait_for_news`], given up as soon as `stop` says so (an export cancelled meanwhile).
pub fn wait_for_news_unless(since: u64, timeout: Duration, stop: impl Fn() -> bool) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let start = Instant::now();
        let (mut seen, mut changed, mut news) = (since, Instant::now(), None);
        while in_flight() > 0 && start.elapsed() < timeout && !stop() {
            let now = generation();
            if now != seen {
                (seen, changed) = (now, Instant::now());
                news.get_or_insert(changed);
            }
            if news.is_some_and(|first: Instant| changed.elapsed() >= QUIET || first.elapsed() >= SETTLE) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
    }
    #[cfg(target_arch = "wasm32")]
    let _ = (since, timeout, stop);
}

/// A waiting fetch: its decode kind, diagnostics owner, expected statuses, when it was asked for
/// ([`land`]) and initiating context.
type Job = (Want, Capture, &'static [u16], u64, Option<Still>);

fn queue() -> &'static Mutex<Queue<Job>> {
    static Q: OnceLock<Mutex<Queue<Job>>> = OnceLock::new();
    Q.get_or_init(Mutex::default)
}

/// Asks for a tile: fetched now when its server has room, else it waits its turn.
fn start(url: String, want: Want, omit: &'static [u16], asked_at: u64) {
    let capture = Capture::current();
    let still = current_still();
    IN_FLIGHT.fetch_add(1, Ordering::Relaxed);
    let mut swept = Vec::new();
    let now = {
        let mut q = queue().lock().unwrap_or_else(|e| e.into_inner());
        let mut c = lock();
        let latest = c.latest.unwrap_or_else(Instant::now);
        let now = q.push(url, (want, capture, omit, asked_at, still), |u| c.slots.get(u).map(|s| s.asked), latest, &mut swept);
        // Under the same lock as the choice: a tile asked for again meanwhile isn't forgotten.
        forget_dropped(&mut c, &swept);
        now
    };
    launch(now.into_iter().collect());
}

/// A fetch from `host` is over: starts the waiting tiles that take its place and forgets those
/// the map stopped asking for.
fn release(host: &str) {
    let next = {
        let mut q = queue().lock().unwrap_or_else(|e| e.into_inner());
        let mut c = lock();
        let latest = c.latest.unwrap_or_else(Instant::now);
        let next = q.done(host, |url| c.slots.get(url).map(|s| s.asked), latest);
        forget_dropped(&mut c, &next.drop);
        next
    };
    launch(next.start);
}

/// Forgets tiles dropped unfetched: no longer under way, and fetched again when next asked for.
/// Called holding the cache's lock taken to choose them, so a tile can't be asked for between.
fn forget_dropped(c: &mut Cache, dropped: &[Waiting<Job>]) {
    for w in dropped {
        if matches!(c.slots.get(&w.url), Some(Slot { entry: Entry::Pending, .. })) {
            c.slots.remove(&w.url);
        }
    }
    IN_FLIGHT.fetch_sub(dropped.len(), Ordering::Release);
}

/// Starts fetches one after another, never inside each other: a fetch can be over at once (a
/// local read, an archive that failed), and each one over starts the next waiting tile, so a
/// long queue would otherwise go as deep into the stack as it is long.
fn launch(jobs: Vec<Waiting<Job>>) {
    static TO_START: Mutex<Vec<Waiting<Job>>> = Mutex::new(Vec::new());
    static STARTING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let pop = || TO_START.lock().unwrap_or_else(|e| e.into_inner()).pop();
    if !jobs.is_empty() {
        // Newest first: `pop` takes from the end.
        TO_START.lock().unwrap_or_else(|e| e.into_inner()).extend(jobs.into_iter().rev());
    }
    loop {
        // Another call (this thread further up, or another thread) is starting them already.
        if STARTING.swap(true, Ordering::AcqRel) {
            return;
        }
        {
            // Lowered however this ends: a fetcher that panics must not stop every later fetch.
            struct Lower;
            impl Drop for Lower {
                fn drop(&mut self) {
                    STARTING.store(false, Ordering::Release);
                }
            }
            let _lower = Lower;
            while let Some(w) = pop() {
                let (want, capture, omit, asked_at, still) = w.job;
                let _scope = capture.enter();
                with_optional_still(still, || fetch_tile(w.url, w.host, want, omit, asked_at));
            }
        }
        // A tile queued after the last `pop` and before the flag went down is started here.
        if TO_START.lock().unwrap_or_else(|e| e.into_inner()).is_empty() {
            return;
        }
    }
}

/// A tile fetch's answer: stored once it comes, and however the fetch ends (answered, dropped
/// unanswered by its fetcher, or unwound by a panic) its server's place goes to the next tile
/// and the fetch no longer counts as under way.
struct Answer {
    key: String,
    host: String,
    entry: Option<Entry>,
    /// When the tile was asked for, for an MBTiles file forgotten since ([`land`]).
    asked_at: u64,
}

impl Answer {
    /// Stored when the answer is dropped, after the fetch.
    fn set(&mut self, entry: Entry) {
        self.entry = Some(entry);
    }
}

impl Drop for Answer {
    fn drop(&mut self) {
        land(&self.key, self.entry.take(), self.asked_at);
        release(&self.host);
        finished();
    }
}

/// Keeps what a fetch asked for at `asked_at` answered. A tile of an MBTiles file forgotten since may be
/// of the old file, so it is dropped. Its slot went with the forget: one there now is a newer read's.
fn land(key: &str, entry: Option<Entry>, asked_at: u64) {
    if entry.is_some() && login_gone(key) {
        return;
    }
    // Under the lock [`forget`] takes after recording the file: if that hasn't been recorded yet,
    // what is stored here goes with the slots it removes next.
    let mut c = lock();
    if crate::mbtiles::forgotten_since(key, asked_at) {
        return;
    }
    // Asked for before the record of forgotten files was emptied, so it can't be told: read again.
    match entry.filter(|_| !crate::mbtiles::asked_before_emptied(key, asked_at)) {
        Some(entry) => keep(&mut c, key, entry),
        // Never answered (or dropped by the fetcher): forgotten, fetched again when asked for.
        None => {
            if matches!(c.slots.get(key), Some(Slot { entry: Entry::Pending, .. })) {
                c.slots.remove(key);
            }
        }
    }
}

/// Retains the initiating lifetime through deferred decoding. Retired results are forgotten;
/// only live successes and resource failures are shared with later callers.
fn fetch_tile(url: String, host: String, want: Want, omit: &'static [u16], asked_at: u64) {
    let owner = current_still();
    let answer = Answer { key: url.clone(), host, entry: None, asked_at };
    let done: Done = Box::new(move |bytes| {
        // Shared archive reads can answer under another loader's narrower lifetime. Retain
        // that floor with this tile's owner before a browser decoder suspends completion.
        let still = with_optional_still(owner, current_still);
        if matches!(&bytes, Err(e) if e == DROPPED) {
            return;
        }
        // Stored once decoded, which the browser's decoder may finish later (the answer is held
        // till then, and dropped unanswered if the decoder never calls back).
        let settle = move |decoded: Result<Payload, String>| {
            let mut answer = answer;
            answer.set(match decoded {
                Ok(_) if check_still(&still).is_err() => return,
                Ok(p) => {
                    let size = payload_bytes(&p);
                    Entry::Ready(Arc::new(p), size)
                }
                // The browser's decoder may answer after the initiating owner was retired.
                Err(e) if !keep_failure(&e, &still) => return,
                Err(e) => {
                    log::debug!("{}", crate::logins::scrub_error(&format!("{}: {e}", answer.key)));
                    // Refused until a time its server gave: the map draws again then.
                    if let Some(wait) = retry_after(&e) {
                        notify_at(Instant::now() + wait);
                    }
                    Entry::Failed(e, Instant::now())
                }
            });
        };
        let decoder = image_decoder().lock().unwrap_or_else(|e| e.into_inner()).clone();
        decode_with(bytes, want, decoder.as_deref(), settle);
    });
    // A tile from a PMTiles archive is read from the archive, by range.
    if crate::pmtiles::is_tile(&url) {
        crate::pmtiles::fetch(&url, done);
        return;
    }
    // A tile of a cloud-optimised GeoTIFF is drawn from the file's tiles, read by range.
    if crate::cog::is_tile(&url) {
        crate::cog::fetch(&url, done);
        return;
    }
    // A tile from an MBTiles archive is read from the file, or the upload in memory.
    if crate::mbtiles::is_tile(&url) {
        crate::mbtiles::fetch(&url, done);
        return;
    }
    fetch_bytes(url, done, omit);
}

/// Decodes what a fetch gave as `want` and hands it to `settle`: a picture through `decoder`, if
/// one is set (see [`set_image_decoder`]), which may answer later.
fn decode_with(
    bytes: Result<Vec<u8>, String>,
    want: Want,
    decoder: Option<&ImageDecoder>,
    settle: impl FnOnce(Result<Payload, String>) + Send + 'static,
) {
    match (bytes, decoder) {
        (Ok(b), Some(decoder)) if want == Want::Image && b.len() <= MAX_BYTES && encoded_picture(&b) => decoder(
            b,
            Box::new(move |r| {
                settle(match r {
                    Ok(raw) => decode_image(&raw).map(Payload::Image),
                    Err(b) => decode(&b, want),
                })
            }),
        ),
        (bytes, _) => settle(bytes.and_then(|b| decode(&b, want))),
    }
}

/// Marks the part of an address that asks for a byte range, `#rustgis-range=<first>-<last>`:
/// a fragment, so it never reaches the server, which gets a `Range` header instead.
pub(crate) const RANGE: &str = "#rustgis-range=";

/// Splits an address into the address to fetch and the byte range it asks for, if any.
pub fn split_range(url: &str) -> (&str, Option<(u64, u64)>) {
    match url.rsplit_once(RANGE) {
        Some((base, r)) => match r.split_once('-').and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?))) {
            Some((a, b)) if a <= b => (base, Some((a, b))),
            _ => (url, None),
        },
        None => (url, None),
    }
}

/// Marks an address asking for the file's last bytes, `#rustgis-tail=<n>` (`Range: bytes=-n`),
/// where its length isn't known (a GeoParquet's footer). A shorter answer is the whole file.
pub const TAIL: &str = "#rustgis-tail=";

/// Splits an address into the address to fetch and how many of its last bytes it asks for, if any.
pub fn split_tail(url: &str) -> (&str, Option<u64>) {
    match url.rsplit_once(TAIL) {
        Some((base, n)) => match n.parse::<u64>() {
            Ok(n) if n > 0 => (base, Some(n)),
            _ => (url, None),
        },
        None => (url, None),
    }
}

/// Starts an address that [`download`] made: the fetchers take up to [`MAX_DOWNLOAD`] bytes for
/// it, and [`MAX_BYTES`] (a tile) for any other. No tile, style or archive address can start with
/// it: those pass `address::check`, which takes only `http(s)://` ones.
pub const DOWNLOAD: &str = "rustgis-download:";
const LIMITED_DOWNLOAD: &str = "rustgis-download-limit:";

/// The most bytes a data download may have.
pub const MAX_DOWNLOAD: usize = if cfg!(target_arch = "wasm32") { 256 << 20 } else { 512 << 20 };

/// Splits an address into the address to fetch and whether it is a download ([`DOWNLOAD`]).
pub fn split_download(url: &str) -> (&str, bool) {
    let (url, limit) = split_download_limit(url);
    (url, limit.is_some())
}

/// Splits a download marker, including a caller's smaller byte budget. Fetchers must enforce
/// this limit while reading, before allocating the whole response.
pub fn split_download_limit(url: &str) -> (&str, Option<usize>) {
    if let Some(marked) = url.strip_prefix(LIMITED_DOWNLOAD)
        && let Some((limit, url)) = marked.split_once(':')
        && let Ok(limit) = limit.parse::<usize>()
    {
        return (url, Some(limit.min(MAX_DOWNLOAD)));
    }
    match url.strip_prefix(DOWNLOAD) {
        Some(u) => (u, Some(MAX_DOWNLOAD)),
        None => (url, None),
    }
}

fn download_address(url: &str, max: usize) -> String {
    if max >= MAX_DOWNLOAD { format!("{DOWNLOAD}{url}") } else { format!("{LIMITED_DOWNLOAD}{max}:{url}") }
}

/// Failure-only diagnostics shared by the native transport and browser fetch implementation.
/// Keep the original request address (including range markers) here, never its body or secrets
/// from request headers. The diagnostics history sanitizes all retained text.
pub struct RequestDiagnostics<'a> {
    url: &'a str,
    method: &'static str,
    capture: rustgis_data::diagnostics::Capture,
    started: Instant,
    status: Option<u16>,
    timed_out: bool,
    /// Statuses the caller expects (a tile past a map's edge, a store's absent chunk): their
    /// failures aren't recorded one by one, as each would flood the history. One note per server
    /// says that they're omitted ([`rustgis_data::diagnostics::Capture::note_absence`]).
    omit: &'static [u16],
}

impl<'a> RequestDiagnostics<'a> {
    pub fn new(url: &'a str, method: &'static str) -> Self {
        Self::with_capture(url, method, rustgis_data::diagnostics::Capture::current())
    }

    /// Keeps the request's initiating owner across a queue hop or an async browser task.
    pub fn with_capture(url: &'a str, method: &'static str, capture: rustgis_data::diagnostics::Capture) -> Self {
        Self { url, method, capture, started: Instant::now(), status: None, timed_out: false, omit: &[] }
    }

    /// Statuses whose failures are expected answers, noted once per server, not recorded each
    /// (a tile past a map's edge, a store's absent chunk).
    pub fn omit_statuses(&mut self, omit: &'static [u16]) {
        self.omit = omit;
    }

    /// A new hop has no status until it actually receives headers.
    pub fn sending(&mut self) {
        self.status = None;
    }

    pub fn answered(&mut self, status: u16) {
        // Browser opaque responses expose 0, which is not an observed HTTP status.
        self.status = (100..=599).contains(&status).then_some(status);
    }

    /// Only a transport's own timer may establish a timeout, not an opaque browser error.
    pub fn timed_out(&mut self) {
        self.timed_out = true;
    }

    /// A request's result, its failure recorded ([`Self::failed`]) on the way through.
    pub fn record<T>(&self, result: Result<T, Failure>) -> Result<T, Failure> {
        result.inspect_err(|failure| self.failed(failure))
    }

    pub fn failed(&self, failure: &Failure) {
        use crate::failure::Reason;
        use rustgis_data::diagnostics::{Network, NetworkKind};
        if failure.message == DROPPED {
            return;
        }
        // The address as asked for, without the download and range markers this module adds.
        let (url, _) = split_download_limit(self.url);
        let (url, tail) = split_tail(url);
        let (url, range) = split_range(url);
        // A request carrying a login is recorded by its plain address and scrubbed text: neither
        // the binding that names the login nor a credential belongs in the history.
        let carried = rustgis_data::login_address::is_wrapper(url);
        let url = crate::logins::inner(url);
        let url = if rustgis_data::login_address::is_wrapper(url) { "[address omitted]" } else { url };
        let clean = |text: &str| {
            if carried || text.contains(rustgis_data::login_address::PREFIX) { crate::logins::scrub_error(text) } else { text.to_string() }
        };
        // A status the caller expects is an answer, not a failure for the history; a caller
        // that treats the refusal as an error says so itself. One bounded record per server says
        // the omission, so a misconfigured tile source isn't silent.
        if let Some(status) = self.status
            && self.omit.contains(&status)
        {
            self.capture.note_absence(self.method, url, status);
            return;
        }
        let mut details = match (range, tail) {
            (Some((a, b)), _) => format!("Range: bytes={a}-{b}\n"),
            (_, Some(n)) => format!("Range: bytes=-{n}\n"),
            _ => String::new(),
        };
        // Explicitly exclude Answer::excerpt: an arbitrary server response is not diagnostics.
        // Each header is redacted on its own line: a credential header (`www-authenticate`) ends
        // redaction of the text it is in, which would drop the headers after it.
        if let Some(answer) = &failure.answer {
            for (name, value) in &answer.headers {
                if crate::failure::KEPT_HEADERS.contains(&name.as_str()) {
                    details.push_str(&rustgis_data::diagnostics::redact(&clean(&format!("{name}: {value}"))));
                    details.push('\n');
                }
            }
        }
        let kind = if self.timed_out {
            NetworkKind::Timeout
        } else {
            match failure.reason {
                Reason::Timeout => NetworkKind::Timeout,
                Reason::Unreachable => NetworkKind::Connection,
                _ if self.status.is_some_and(|status| status >= 300) => NetworkKind::Http,
                _ => NetworkKind::Other,
            }
        };
        self.capture.network_failure(
            "Network",
            &clean(&failure.message),
            &clean(&details),
            Network {
                url: url.to_string(),
                method: self.method.to_string(),
                status: self.status,
                duration_ms: Some(self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64),
                kind,
            },
        );
    }
}

/// The HTTP status a fetch's error is, if it is one: the whole message, as the native fetcher
/// (`http status: 404`) and the browser's (`HTTP 404`) give it — never a number found inside
/// another message (an address can hold "404").
pub fn http_status(error: &str) -> Option<u16> {
    let e = error.trim();
    let rest = e.strip_prefix("http status: ").or_else(|| e.strip_prefix("HTTP "))?;
    // The status, and nothing after it but how long to wait (see [`retry_after`]).
    let (code, after) = rest.split_at_checked(3)?;
    let known = after.is_empty() || (after.starts_with(" (retry after ") && retry_after(after).is_some());
    (known && code.bytes().all(|b| b.is_ascii_digit())).then(|| code.parse().ok()).flatten()
}

/// Downloads a file the user named (data added by its web address): up to [`MAX_DOWNLOAD`]
/// bytes, uncached, off the frame (a thread natively, the browser's fetch on the web), counted as
/// a fetch under way.
pub fn download(url: &str, done: Done) {
    download_limited(url, MAX_DOWNLOAD, done);
}

/// Like [`download`], with a smaller budget enforced by the native and browser fetchers.
pub fn download_limited(url: &str, max: usize, done: Done) {
    download_limited_omitting(url, max, &[], done);
}

/// Like [`download_limited`], but a status in `omit` is an expected answer: its failure isn't
/// recorded, only noted once per server (a file that may not be there, which the caller handles).
pub fn download_limited_omitting(url: &str, max: usize, omit: &'static [u16], done: Done) {
    download_detailed_within(url, max, DOWNLOAD_TIMEOUT, omit, Box::new(move |r| done(r.map_err(|f| f.message))));
}

/// Like [`download_limited`], giving up after `timeout` natively (the browser's fetch keeps its
/// own): a small answer that mustn't hold a thread for long (an ArcGIS layer's description).
pub fn download_within(url: &str, max: usize, timeout: std::time::Duration, done: Done) {
    download_detailed_within(url, max, timeout, &[], Box::new(move |r| done(r.map_err(|f| f.message))));
}

/// How long a download off the frame may take natively.
const DOWNLOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(900);

/// Like [`download_limited`], telling what is known of a failure (#509): the status, the headers
/// that say who answered and an excerpt of the answer. The browser's fetch tells only the
/// status, read back from its error.
pub fn download_detailed(url: &str, max: usize, done: DoneDetailed) {
    download_detailed_within(url, max, DOWNLOAD_TIMEOUT, &[], done);
}

/// Like [`download_detailed`], giving up after `timeout` natively; a status in `omit` isn't
/// recorded as a failure.
fn download_detailed_within(url: &str, max: usize, timeout: std::time::Duration, omit: &'static [u16], done: DoneDetailed) {
    if let Err(e) = check_current() {
        done(Err(Failure::new(url, e)));
        return;
    }
    let still = current_still();
    let done: DoneDetailed = Box::new(move |r| with_optional_still(still, || done(r)));
    if let Err(e) = crate::address::check(url, None) {
        done(Err(Failure::new(url, e)));
        return;
    }
    let marked = download_address(url, max);
    IN_FLIGHT.fetch_add(1, Ordering::Relaxed);
    // The failure names the address asked for, not the marked one fetched.
    let asked = url.to_string();
    let done: DoneDetailed = Box::new(move |r| {
        let r = crate::logins::check(&asked).map_err(|e| Failure::new(&asked, e)).and(r);
        done(r.map_err(|f| Failure { url: crate::logins::inner(&asked).to_string(), ..f }));
        finished();
    });
    let f = fetcher().lock().unwrap_or_else(|e| e.into_inner()).clone();
    match f {
        Some(f) => f(marked.clone(), Box::new(move |r| done(r.map_err(|e| Failure::new(&marked, e)))), omit),
        None => native::download_within(marked, timeout, omit, done),
    }
}

/// How long a download the caller waits for may take: commands run where the app waits too (the
/// AI Assistant's), so a large file comes through Home › Add › Add Data › From URL, which downloads off the
/// frame.
#[cfg(not(target_arch = "wasm32"))]
pub const BLOCKING_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Downloads a file the user named, waiting for it (at most [`BLOCKING_TIMEOUT`]).
#[cfg(not(target_arch = "wasm32"))]
pub fn download_blocking(url: &str) -> Result<Vec<u8>, String> {
    download_blocking_limited(url, MAX_DOWNLOAD)
}

/// Like [`download_blocking`], but reads at most `max` bytes plus one overflow sentinel.
#[cfg(not(target_arch = "wasm32"))]
pub fn download_blocking_limited(url: &str, max: usize) -> Result<Vec<u8>, String> {
    download_blocking_within(url, max, BLOCKING_TIMEOUT).map_err(large_file_hint)
}

/// Like [`download_blocking`], but a status in `omit` is an expected answer: its failure isn't
/// recorded, only noted once per server (a store's absent chunk, which the caller counts itself).
#[cfg(not(target_arch = "wasm32"))]
pub fn download_blocking_omitting(url: &str, omit: &'static [u16]) -> Result<Vec<u8>, String> {
    download_blocking_omitting_within(url, MAX_DOWNLOAD, BLOCKING_TIMEOUT, omit).map_err(large_file_hint)
}

/// A waited-for download's error, with where a large file goes instead when it timed out.
#[cfg(not(target_arch = "wasm32"))]
pub fn large_file_hint(e: String) -> String {
    if e.contains("timeout") || e.contains("timed out") {
        format!("{e} (a large file: add it with Home › Add › Add Data › From URL, which downloads in the background)")
    } else {
        e
    }
}

/// Like [`download_blocking_limited`], giving up after `timeout` (with the bare error).
#[cfg(not(target_arch = "wasm32"))]
pub fn download_blocking_within(url: &str, max: usize, timeout: std::time::Duration) -> Result<Vec<u8>, String> {
    download_blocking_omitting_within(url, max, timeout, &[])
}

/// Like [`download_blocking_within`], but a status in `omit` is an expected answer: its failure
/// isn't recorded, only noted once per server (a store's absent chunk, which the caller counts
/// itself).
#[cfg(not(target_arch = "wasm32"))]
pub fn download_blocking_omitting_within(
    url: &str,
    max: usize,
    timeout: std::time::Duration,
    omit: &'static [u16],
) -> Result<Vec<u8>, String> {
    crate::address::check(url, None)?;
    native::get(&native::download_agent(timeout), &download_address(url, max), omit)
}

/// Like [`download_blocking_within`], connecting only to internet addresses: a name, or a
/// redirect to one, that resolves to this computer or the local network is refused when it is
/// resolved, so it can't be used to reach there (as a plugin's fetch mustn't).
#[cfg(not(target_arch = "wasm32"))]
pub fn download_public_within(url: &str, max: usize, timeout: std::time::Duration) -> Result<Vec<u8>, String> {
    crate::address::check(url, None)?;
    native::get(&native::public_agent(timeout), &download_address(url, max), &[])
}

/// Reads an address's first byte, connecting only to internet addresses (as
/// [`download_public_within`]), telling what is known of a failure: whether and how a server
/// serves a file, without reading it ([`crate::s3::public_region`]).
#[cfg(not(target_arch = "wasm32"))]
pub fn probe_blocking(url: &str) -> Result<(), Failure> {
    crate::address::check(url, None).map_err(|e| Failure::new(url, e))?;
    native::get_detailed(&native::public_agent(BLOCKING_TIMEOUT), &format!("{url}{RANGE}0-0")).map(|_| ())
}

/// How a JSON document is POSTed and its answer read (the browser's `fetch`): the address, the
/// body and what to call once, as [`set_poster`] installs it.
pub type Poster = Box<dyn Fn(String, String, Done) + Send + Sync>;

fn poster() -> &'static Mutex<Option<Arc<Poster>>> {
    static P: OnceLock<Mutex<Option<Arc<Poster>>>> = OnceLock::new();
    P.get_or_init(Mutex::default)
}

/// Replaces how JSON documents are POSTed (the browser's `fetch`). `done` must be called once,
/// with at most [`MAX_BYTES`] of the answer, and an error status as `HTTP <status>`.
pub fn set_poster(f: impl Fn(String, String, Done) + Send + Sync + 'static) {
    *poster().lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(Box::new(f)));
}

/// POSTs a JSON document to `url` (for a service that takes its request as one, the NLDI's
/// flowtrace) and hands over the answer, off the frame (a thread natively, the browser's fetch
/// on the web), counted as a fetch under way. Redirects aren't followed.
pub fn post_json(url: &str, body: String, done: Done) {
    if let Err(e) = crate::address::check(url, None) {
        done(Err(e));
        return;
    }
    IN_FLIGHT.fetch_add(1, Ordering::Relaxed);
    let key = url.to_string();
    let done: Done = Box::new(move |r| {
        done(crate::logins::check(&key).map_err(str::to_string).and(r));
        finished();
    });
    let p = poster().lock().unwrap_or_else(|e| e.into_inner()).clone();
    match p {
        Some(p) => p(url.to_string(), body, done),
        None => native::post(url.to_string(), body, done),
    }
}

/// Like [`post_json`], waiting for the answer (at most [`BLOCKING_TIMEOUT`]).
#[cfg(not(target_arch = "wasm32"))]
pub fn post_json_blocking(url: &str, body: &str) -> Result<Vec<u8>, String> {
    crate::address::check(url, None)?;
    native::post_with(&native::public_agent(BLOCKING_TIMEOUT), url, body)
}

/// What a `HEAD` request says of a file before any of it is read: the length of its TIFF header
/// that a server making GeoTIFFs on request announces (`X-TIFF-Header-Length`, Vantor's Transform
/// API), so a cloud-optimised GeoTIFF's header comes in one range read; its type; and whether
/// the server says it answers byte ranges (`Accept-Ranges: bytes`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Head {
    pub tiff_header_length: Option<u64>,
    pub content_type: Option<String>,
    pub accepts_ranges: bool,
}

/// Whether an `Accept-Ranges` value says byte ranges are answered.
pub fn accepts_byte_ranges(value: &str) -> bool {
    value.split(',').any(|unit| unit.trim().eq_ignore_ascii_case("bytes"))
}

/// Called once with what a `HEAD` request said.
pub type HeadDone = Box<dyn FnOnce(Result<Head, String>) + Send>;
/// How a `HEAD` request is made (the browser's `fetch`), as [`set_header`] installs it.
pub type Header = Box<dyn Fn(String, HeadDone) + Send + Sync>;

fn header() -> &'static Mutex<Option<Arc<Header>>> {
    static H: OnceLock<Mutex<Option<Arc<Header>>>> = OnceLock::new();
    H.get_or_init(Mutex::default)
}

/// Replaces how `HEAD` requests are made (the browser's `fetch`, which sees
/// `X-TIFF-Header-Length` only when the server exposes it to pages). `done` must be called once,
/// an error status as `HTTP <status>`.
pub fn set_header(f: impl Fn(String, HeadDone) + Send + Sync + 'static) {
    *header().lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(Box::new(f)));
}

/// An `X-TIFF-Header-Length` value: a length in bytes, none when it isn't one.
pub fn tiff_header_length(value: &str) -> Option<u64> {
    value.trim().parse::<u64>().ok().filter(|n| *n > 0)
}

/// Asks for `url`'s headers (`HEAD`), off the frame (a thread natively, the browser's fetch on
/// the web), counted as a fetch under way. A request signed for S3 isn't made (its signature is
/// for a GET): it answers with nothing known.
pub fn head(url: &str, done: HeadDone) {
    if let Err(e) = check_current() {
        done(Err(e));
        return;
    }
    let still = current_still();
    let done: HeadDone = Box::new(move |r| with_optional_still(still, || done(r)));
    if let Err(e) = crate::address::check(url, None) {
        done(Err(e));
        return;
    }
    IN_FLIGHT.fetch_add(1, Ordering::Relaxed);
    let key = url.to_string();
    let done: HeadDone = Box::new(move |r| {
        done(crate::logins::check(&key).map_err(str::to_string).and(r));
        finished();
    });
    let h = header().lock().unwrap_or_else(|e| e.into_inner()).clone();
    match h {
        Some(h) => h(url.to_string(), done),
        None => native::head(url.to_string(), done),
    }
}

/// Fetches an address's bytes, uncached (through the browser's fetcher, or the worker threads).
/// A status in `omit` is an expected answer: its failure isn't recorded, only noted once per
/// server (a tile past a map's edge).
pub(crate) fn fetch_bytes(url: String, done: Done, omit: &'static [u16]) {
    if let Err(e) = check_current() {
        done(Err(e));
        return;
    }
    let still = current_still();
    let done: Done = Box::new(move |r| with_optional_still(still, || done(r)));
    if let Err(e) = crate::logins::check(resource_of(&url)) {
        done(Err(e.to_string()));
        return;
    }
    let key = url.clone();
    let done: Done = Box::new(move |r| done(crate::logins::check(resource_of(&key)).map_err(str::to_string).and(r)));
    let f = fetcher().lock().unwrap_or_else(|e| e.into_inner()).clone();
    match f {
        Some(f) => f(url, done, omit),
        None => native::fetch(url, done, omit),
    }
}

/// Like [`fetch_bytes`], counted as a fetch under way (so exports [`wait`] for it) and drawing
/// again when it lands: for what the map needs before it can ask for tiles (a PMTiles header).
pub(crate) fn fetch_counted(url: String, done: Done) {
    IN_FLIGHT.fetch_add(1, Ordering::Relaxed);
    fetch_bytes(
        url,
        Box::new(move |r| {
            done(r);
            finished();
        }),
        &[],
    );
}

/// Reads a tile of a local MBTiles file on the fetch workers (counted already, as a tile).
pub(crate) fn fetch_worker(url: String, done: Done) {
    native::fetch(url, done, &[]);
}

/// Reads a range of a local file (`<path>#rustgis-range=a-b`) on the fetch workers, counted like
/// [`fetch_counted`]; never through an installed fetcher, which only knows web addresses.
pub(crate) fn fetch_local(url: String, done: Done) {
    IN_FLIGHT.fetch_add(1, Ordering::Relaxed);
    native::fetch_local(
        url,
        Box::new(move |r| {
            done(r);
            finished();
        }),
    );
}

/// Something the map draws from arrived outside the cache (a PMTiles archive's header): draw
/// again.
pub(crate) fn notify() {
    GENERATION.fetch_add(1, Ordering::Relaxed);
    let w = waker().lock().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(w) = w {
        w();
    }
}

/// A fetch is over: the map can draw again.
fn finished() {
    // The generation first: whoever sees the fetch done sees what it brought.
    GENERATION.fetch_add(1, Ordering::Relaxed);
    IN_FLIGHT.fetch_sub(1, Ordering::Release);
    let w = waker().lock().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(w) = w {
        w();
    }
}

fn store(url: &str, entry: Entry) {
    if login_gone(url) {
        return;
    }
    keep(&mut lock(), url, entry);
}

/// Whether the login `url` was fetched with is gone, in which case nothing of it is kept.
fn login_gone(url: &str) -> bool {
    if crate::logins::check(resource_of(url)).is_ok() {
        return false;
    }
    let mut c = lock();
    if let Some(Slot { entry: Entry::Ready(_, n), .. }) = c.slots.remove(url) {
        c.bytes = c.bytes.saturating_sub(n);
    }
    true
}

/// [`store`], holding the cache's lock.
fn keep(c: &mut Cache, url: &str, entry: Entry) {
    let added = if let Entry::Ready(_, n) = &entry { *n } else { 0 };
    // Dropped while it was fetched (cleared, or evicted): kept anyway, it was asked for.
    let tick = c.tick;
    let asked = c.slots.get(url).map_or_else(Instant::now, |s| s.asked);
    let old = c.slots.insert(url.to_string(), Slot { entry, used: tick, asked });
    if let Some(Slot { entry: Entry::Ready(_, n), .. }) = old {
        c.bytes = c.bytes.saturating_sub(n);
    }
    c.bytes += added;
    if c.bytes > BUDGET {
        evict(c);
    }
}

/// Drops the least recently used entries until the cache is at three quarters of its budget.
fn evict(c: &mut Cache) {
    let mut ready: Vec<(u64, String, usize)> = c
        .slots
        .iter()
        .filter_map(|(k, s)| match s.entry {
            Entry::Ready(_, n) => Some((s.used, k.clone(), n)),
            Entry::Failed(..) => Some((s.used, k.clone(), 0)),
            Entry::Pending => None,
        })
        .collect();
    ready.sort_unstable_by_key(|r| r.0);
    for (_, k, n) in ready {
        if c.bytes <= BUDGET / 4 * 3 {
            break;
        }
        c.slots.remove(&k);
        c.bytes = c.bytes.saturating_sub(n);
    }
}

fn payload_bytes(p: &Payload) -> usize {
    match p {
        Payload::Image(pm) => pm.data().len(),
        Payload::Vector(t) => t
            .layers
            .iter()
            .flat_map(|l| &l.features)
            .map(|f| 64 + f.geometry.iter().map(|g| g.len() * 8 + 24).sum::<usize>() + f.props.len() * 48)
            .sum(),
        Payload::Json(_) => 64 << 10,
        Payload::Empty => 64,
    }
}

/// Decodes fetched bytes.
pub fn decode(bytes: &[u8], want: Want) -> Result<Payload, String> {
    if bytes.is_empty() {
        return Ok(Payload::Empty);
    }
    if bytes.len() > MAX_BYTES {
        return Err(format!("{} bytes is too large for a tile", bytes.len()));
    }
    match want {
        Want::Image => decode_image(bytes).map(Payload::Image),
        Want::Vector => mvt::decode(bytes).map(Payload::Vector),
        Want::Json => serde_json::from_slice(bytes).map(Payload::Json).map_err(|e| format!("not JSON: {e}")),
    }
}

/// Starts a picture drawn by RustGIS itself (a COG's map tile) or decoded by the browser (see
/// [`set_image_decoder`]): its width and height (u32, little end first), then straight RGBA
/// pixels. No encoding, so no PNG encoder in the browser build.
pub const RAW: &[u8] = b"RUSTGIS-RGBA";

/// Decodes a PNG, JPEG or WebP picture into a premultiplied pixmap (at most 4096 pixels a side).
pub fn decode_image(bytes: &[u8]) -> Result<Pixmap, String> {
    if let Some(rest) = bytes.strip_prefix(RAW) {
        let dim = |at: usize| rest.get(at..at + 4).and_then(|b| b.try_into().ok()).map(u32::from_le_bytes).unwrap_or(0);
        let (w, h) = (dim(0), dim(4));
        let mut data = rest.get(8..).unwrap_or_default().to_vec();
        if w == 0 || h == 0 || w > 4096 || h > 4096 || data.len() != (w as usize) * (h as usize) * 4 {
            return Err("a picture of the wrong size".into());
        }
        premultiply(&mut data);
        return Pixmap::from_vec(data, tiny_skia::IntSize::from_wh(w, h).ok_or("an empty picture")?)
            .ok_or_else(|| "a picture of the wrong size".into());
    }
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().map_err(|e| e.to_string())?;
    reader.limits(picture_limits());
    let img = reader.decode().map_err(|e| e.to_string())?.into_rgba8();
    let (w, h) = img.dimensions();
    let mut data = img.into_raw();
    premultiply(&mut data);
    Pixmap::from_vec(data, tiny_skia::IntSize::from_wh(w, h).ok_or("an empty picture")?).ok_or_else(|| "a picture of the wrong size".into())
}

/// What reading a tile picture may take: at most 4096 pixels a side and 96 MiB, for its header
/// ([`encoded_picture`]) as for its pixels ([`decode_image`]).
fn picture_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(96 << 20);
    limits
}

/// Straight RGBA to premultiplied, in place.
fn premultiply(data: &mut [u8]) {
    for px in data.as_chunks_mut::<4>().0 {
        let a = u16::from(px[3]);
        if a < 255 {
            for c in &mut px[..3] {
                *c = ((u16::from(*c) * a + 127) / 255) as u8;
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    //! Worker threads fetching with `ureq`, newest request first, a few at a time per server.
    //!
    //! A tile costs one or two round trips plus the server's own time (Esri's World Imagery
    //! Wayback answers every tile with a redirect first), and the connections stay open between
    //! tiles, so fetching is bound by latency: how many requests are under way at once. Each
    //! server gets up to [`PER_HOST`] of them (OpenStreetMap's tile servers [`PER_HOST_OSM`]), and
    //! the workers share one agent, so a server never sees more open connections than that.

    use std::collections::HashMap;
    use std::sync::{Condvar, Mutex, OnceLock};

    use super::{Capture, DROPPED, Done, DoneDetailed, MAX_BYTES};
    use crate::failure::{Failure, Reason};

    /// Fetches under way at once, over all servers: two servers at their cap.
    const WORKERS: usize = 2 * PER_HOST;
    /// Fetches under way at once from one server.
    const PER_HOST: usize = 16;
    /// From the OpenStreetMap Foundation's tile servers, which ask for restraint: what every
    /// version of RustGIS before this one used in all.
    const PER_HOST_OSM: usize = 8;
    /// Requests waiting beyond this many are dropped, oldest first: the view moved on.
    const QUEUE: usize = 512;

    struct Jobs {
        /// Waiting, oldest first: the address, its server, callback, initiating owner, and the statuses
        /// whose failures the caller expects.
        waiting: Vec<(String, String, Done, Capture, &'static [u16], Option<super::Still>)>,
        /// Fetches under way, by server.
        active: HashMap<String, usize>,
    }

    struct Queue {
        jobs: Mutex<Jobs>,
        ready: Condvar,
    }

    fn queue() -> &'static Queue {
        static Q: OnceLock<Queue> = OnceLock::new();
        Q.get_or_init(|| {
            for i in 0..WORKERS {
                let spawned = std::thread::Builder::new().name(format!("rustgis-tiles-{i}")).spawn(work);
                if let Err(e) = spawned {
                    log::error!("could not start a tile fetcher: {e}");
                }
            }
            Queue { jobs: Mutex::new(Jobs { waiting: Vec::new(), active: HashMap::new() }), ready: Condvar::new() }
        })
    }

    /// The server an address is fetched from (`host[:port]`, lowercase), or `""` for a local
    /// read, which no server limits.
    pub(super) fn host(url: &str) -> String {
        if crate::mbtiles::is_tile(url) || url.starts_with(LOCAL) {
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
    pub(super) fn cap(host: &str) -> usize {
        let name = host.split(':').next().unwrap_or(host);
        if host.is_empty() {
            WORKERS
        } else if name == "tile.openstreetmap.org" || name.ends_with(".tile.openstreetmap.org") {
            PER_HOST_OSM
        } else {
            PER_HOST
        }
    }

    /// The newest waiting job whose server is below its cap.
    pub(super) fn pick(waiting: &[&str], active: &HashMap<String, usize>) -> Option<usize> {
        waiting.iter().rposition(|h| active.get(*h).copied().unwrap_or(0) < cap(h))
    }

    /// A local file's range, read on a worker.
    pub(super) fn fetch_local(url: String, done: Done) {
        fetch(format!("{LOCAL}{url}"), done, &[]);
    }

    /// Marks a queued job as a local file read.
    const LOCAL: &str = "local-file:";

    pub(super) fn fetch(url: String, done: Done, omit: &'static [u16]) {
        let capture = Capture::current();
        let q = queue();
        let dropped = {
            let mut jobs = q.jobs.lock().unwrap_or_else(|e| e.into_inner());
            let host = host(&url);
            jobs.waiting.push((url, host, done, capture, omit, super::current_still()));
            (jobs.waiting.len() > QUEUE).then(|| jobs.waiting.remove(0))
        };
        // Every worker may be waiting for a server at its cap, and only some can take this one.
        q.ready.notify_all();
        // Answered once the queue is unlocked: `done` takes the cache's lock and wakes the app.
        if let Some((_, _, done, _, _, _)) = dropped {
            done(Err(DROPPED.into()));
        }
    }

    /// The settings every agent here starts from. An error status is answered, not failed (its
    /// headers and body say who refused), set here rather than on each request: a request's own
    /// settings would make the agent set up TLS again, reading the system's certificates.
    fn config_builder(timeout: std::time::Duration) -> ureq::config::ConfigBuilder<ureq::typestate::AgentScope> {
        ureq::Agent::config_builder()
            .tls_config(rustgis_data::http::tls_config())
            .http_status_as_error(false)
            .timeout_global(Some(timeout))
            .user_agent(USER_AGENT)
            // Redirects are followed by `get`, which checks where each one leads.
            .max_redirects(0)
    }

    pub(super) fn agent() -> ureq::Agent {
        config_builder(std::time::Duration::from_secs(30))
            // The workers share the agent: keep a connection open for each fetch a server
            // may have under way, so the next tile doesn't connect again.
            .max_idle_connections_per_host(PER_HOST)
            .max_idle_connections(WORKERS)
            .build()
            .into()
    }

    /// The agent the workers share, and with it their open connections.
    fn shared_agent() -> &'static ureq::Agent {
        static A: OnceLock<ureq::Agent> = OnceLock::new();
        A.get_or_init(agent)
    }

    /// An agent for downloads, which may take minutes.
    pub(super) fn download_agent(timeout: std::time::Duration) -> ureq::Agent {
        config_builder(timeout).build().into()
    }

    /// Resolves names as ureq does, keeping only the internet addresses (checked where the
    /// connection is made, so a name that resolves elsewhere on another lookup is caught too).
    #[derive(Debug, Default)]
    struct PublicOnly(ureq::unversioned::resolver::DefaultResolver);

    impl ureq::unversioned::resolver::Resolver for PublicOnly {
        fn resolve(
            &self,
            uri: &ureq::http::Uri,
            config: &ureq::config::Config,
            timeout: ureq::unversioned::transport::NextTimeout,
        ) -> Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
            let all = self.0.resolve(uri, config, timeout)?;
            let mut kept = self.empty();
            for a in all.iter().filter(|a| crate::address::ip_reach(a.ip()) == crate::address::Reach::Public) {
                kept.push(*a);
            }
            if kept.is_empty() {
                let host = uri.host().unwrap_or_default();
                let msg = format!("{host} resolves to this computer or the local network, which isn't fetched from here");
                return Err(ureq::Error::Io(std::io::Error::new(std::io::ErrorKind::PermissionDenied, msg)));
            }
            Ok(kept)
        }
    }

    /// A download agent that connects only to internet addresses.
    pub(super) fn public_agent(timeout: std::time::Duration) -> ureq::Agent {
        let config = config_builder(timeout)
            // A proxy would resolve the address itself, past `PublicOnly`.
            .proxy(None)
            .build();
        ureq::Agent::with_parts(config, ureq::unversioned::transport::DefaultConnector::default(), PublicOnly::default())
    }

    /// Asks for an address's headers on a thread of its own.
    pub(super) fn head(url: String, done: super::HeadDone) {
        use std::sync::atomic::Ordering::Relaxed;
        // Past this many under way, a file is read without asking (the usual first read), so
        // many addresses added at once never hold up as many threads and connections.
        const MAX_HEADS: usize = 8;
        static HEADS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
        if HEADS.fetch_add(1, Relaxed) >= MAX_HEADS {
            HEADS.fetch_sub(1, Relaxed);
            done(Ok(super::Head::default()));
            return;
        }
        let capture = Capture::current();
        let still = super::current_still();
        std::thread::spawn(move || {
            super::with_optional_still(still, || {
                let _scope = capture.enter();
                let r = super::check_current().and_then(|_| head_with(AGENT.get_or_init(agent), &url));
                HEADS.fetch_sub(1, Relaxed);
                done(r);
            })
        });
    }

    /// Asks for `url`'s headers, following up to 5 redirects, each checked as [`get`] checks
    /// them.
    pub(super) fn head_with(agent: &ureq::Agent, url: &str) -> Result<super::Head, String> {
        let mut diagnostics = super::RequestDiagnostics::new(url, "HEAD");
        let mut carried_login = false;
        let answer = (|| -> Result<super::Head, Failure> {
            crate::logins::check(url).map_err(str::to_string)?;
            let prepared = crate::s3::prepare(crate::logins::inner(url))?;
            if prepared.signed {
                return Ok(super::Head::default());
            }
            let mut at = prepared.url.clone();
            let first_origin = crate::logins::origin(&at);
            let mut same_origin = true;
            for redirects in 0..=5 {
                // The login goes to its own origin only: once a redirect leaves it, never again.
                let bound = crate::logins::inherit(url, &at);
                let login = if same_origin { crate::logins::apply(&bound)? } else { None };
                carried_login |= login.is_some();
                let mut req = agent.head(login.as_ref().map_or(at.as_str(), |a| a.url.as_str()));
                if let Some((name, value)) = login.as_ref().and_then(|a| a.header.as_ref()) {
                    req = req.header(name.as_str(), value.as_str());
                }
                if redirects == 0 {
                    for (k, v) in &prepared.headers {
                        req = req.header(k.as_str(), v.as_str());
                    }
                }
                diagnostics.sending();
                super::check_current()?;
                let resp = req.call().map_err(unanswered)?;
                diagnostics.answered(resp.status().as_u16());
                let header = |k: &str| resp.headers().get(k).and_then(|v| v.to_str().ok());
                if resp.status().is_redirection() {
                    let next = crate::template::join(&at, header("location").ok_or("a redirect with no Location")?);
                    crate::address::check(&next, Some(&at))?;
                    if crate::address::is_https(&at) && !crate::address::is_https(&next) {
                        return Err(format!("{next}: a redirect from https to http").into());
                    }
                    same_origin &= crate::logins::origin(&next) == first_origin;
                    at = next;
                    continue;
                }
                if resp.status().as_u16() >= 400 {
                    // A server can echo credentials in retained headers, even after logout has
                    // removed their redaction patterns. Remember every hop, including before a
                    // cross-origin redirect: HEAD refusals keep only the status after any login hop.
                    if carried_login {
                        let status = resp.status().as_u16();
                        return Err(Failure::without_answer("", format!("http status: {status}"), Reason::Status(status)));
                    }
                    // Anonymous refusals retain the selected diagnostic headers.
                    let headers: Vec<(String, String)> = resp
                        .headers()
                        .iter()
                        .filter(|(k, _)| crate::failure::KEPT_HEADERS.contains(&k.as_str()))
                        .map(|(k, v)| (k.as_str().to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned()))
                        .collect();
                    return Err(Failure::answered("", resp.status().as_u16(), &headers, &[]));
                }
                crate::logins::check(url).map_err(str::to_string)?;
                return Ok(super::Head {
                    tiff_header_length: header("x-tiff-header-length").and_then(super::tiff_header_length),
                    content_type: header("content-type").map(str::to_string),
                    accepts_ranges: header("accept-ranges").is_some_and(super::accepts_byte_ranges),
                });
            }
            Err("too many redirects".into())
        })();
        diagnostics
            .record(answer)
            .map_err(|failure| if carried_login { crate::logins::scrub_error(&failure.message) } else { failure.message })
    }

    /// POSTs a JSON document on a thread of its own.
    pub(super) fn post(url: String, body: String, done: Done) {
        // Internet addresses only: a name that resolves into the local network gets no body.
        let capture = Capture::current();
        let still = super::current_still();
        std::thread::spawn(move || {
            super::with_optional_still(still, || {
                let _scope = capture.enter();
                done(super::check_current().and_then(|_| post_with(&public_agent(std::time::Duration::from_secs(60)), &url, &body)));
            });
        });
    }

    /// POSTs a JSON document and reads the answer (at most [`MAX_BYTES`]); an error status is
    /// `http status: <status>`, and a redirect isn't followed.
    pub(super) fn post_with(agent: &ureq::Agent, url: &str, body: &str) -> Result<Vec<u8>, String> {
        use std::io::Read;
        let mut diagnostics = super::RequestDiagnostics::new(url, "POST");
        let mut carried_login = false;
        let answer = (|| -> Result<Vec<u8>, Failure> {
            crate::logins::check(url).map_err(str::to_string)?;
            let prepared = crate::s3::prepare(crate::logins::inner(url))?;
            if prepared.signed {
                return Err("S3 signed JSON POST requests are not supported.".into());
            }
            let bound = crate::logins::inherit(url, &prepared.url);
            let login = crate::logins::apply(&bound)?;
            carried_login = login.is_some();
            let mut req = agent.post(prepared.url.as_str()).header("Content-Type", "application/json").header("Accept", "application/json");
            if let Some((name, value)) = login.as_ref().and_then(|a| a.header.as_ref()) {
                req = req.header(name.as_str(), value.as_str());
            }
            super::check_current()?;
            let mut resp = req.send(body).map_err(unanswered)?;
            let status = resp.status().as_u16();
            diagnostics.answered(status);
            if !(200..300).contains(&status) {
                return Err(Failure::without_answer(
                    "",
                    crate::failure::with_retry_after(format!("http status: {status}"), status, retry_after_header(&resp)),
                    Reason::Status(status),
                ));
            }
            let mut buf = Vec::new();
            resp.body_mut()
                .with_config()
                .limit(MAX_BYTES as u64 + 2)
                .reader()
                .take(MAX_BYTES as u64 + 1)
                .read_to_end(&mut buf)
                .map_err(|e| e.to_string())?;
            if buf.len() > MAX_BYTES {
                return Err(format!("larger than {MAX_BYTES} bytes").into());
            }
            crate::logins::check(url).map_err(str::to_string)?;
            Ok(buf)
        })();
        diagnostics
            .record(answer)
            .map_err(|failure| if carried_login { crate::logins::scrub_error(&failure.message) } else { failure.message })
    }

    /// Downloads on a thread of its own, so a large file doesn't hold up the tiles.
    #[cfg(test)]
    pub(super) fn download(url: String, done: DoneDetailed) {
        download_within(url, super::DOWNLOAD_TIMEOUT, &[], done);
    }

    pub(super) fn download_within(url: String, timeout: std::time::Duration, omit: &'static [u16], done: DoneDetailed) {
        let capture = Capture::current();
        let still = super::current_still();
        std::thread::spawn(move || {
            super::with_optional_still(still, || {
                let _scope = capture.enter();
                let r =
                    super::check_current().map_err(Failure::from).and_then(|_| get_detailed_omitting(&download_agent(timeout), &url, omit));
                done(r);
            })
        });
    }

    fn work() {
        let agent = shared_agent();
        let q = queue();
        loop {
            let (url, done, host, capture, omit, still) = {
                let mut jobs = q.jobs.lock().unwrap_or_else(|e| e.into_inner());
                loop {
                    let hosts: Vec<&str> = jobs.waiting.iter().map(|(_, h, _, _, _, _)| h.as_str()).collect();
                    if let Some(i) = pick(&hosts, &jobs.active) {
                        let (url, host, done, capture, omit, still) = jobs.waiting.remove(i);
                        *jobs.active.entry(host.clone()).or_insert(0) += 1;
                        break (url, done, host, capture, omit, still);
                    }
                    jobs = q.ready.wait(jobs).unwrap_or_else(|e| e.into_inner());
                }
            };
            let _scope = capture.enter();
            let result = super::with_optional_still(still.clone(), || {
                super::check_current()?;
                get(agent, &url, omit)
            });
            {
                let mut jobs = q.jobs.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(n) = jobs.active.get_mut(&host) {
                    *n = n.saturating_sub(1);
                    if *n == 0 {
                        jobs.active.remove(&host);
                    }
                }
            }
            // A job waiting for this server may go now.
            q.ready.notify_all();
            super::with_optional_still(still, || done(result));
        }
    }

    /// Fetches `url`, following up to 5 redirects. `url` itself passed `address::check` when
    /// it was made (the layer's address, or one its style or TileJSON chose, checked in
    /// `source::resolve`); each redirect is checked here.
    pub(super) fn get(agent: &ureq::Agent, url: &str, omit: &'static [u16]) -> Result<Vec<u8>, String> {
        fetch_answer(agent, url, false, omit).map_err(|f| f.message)
    }

    /// The User-Agent every request goes with.
    const USER_AGENT: &str = concat!("RustGIS/", env!("CARGO_PKG_VERSION"), " (+https://rustgis.app)");

    /// A request that never got an answer: what kind of failure, for the user (#509).
    fn unanswered(e: ureq::Error) -> Failure {
        let reason = match &e {
            ureq::Error::Timeout(_) => Reason::Timeout,
            // Refused here (an address on this computer or the local network), not unreachable.
            ureq::Error::Io(io) if io.kind() == std::io::ErrorKind::PermissionDenied => Reason::Other,
            ureq::Error::HostNotFound
            | ureq::Error::ConnectionFailed
            | ureq::Error::Io(_)
            | ureq::Error::Tls(_)
            | ureq::Error::Rustls(_) => Reason::Unreachable,
            _ => Reason::Other,
        };
        Failure::without_answer("", e.to_string(), reason)
    }

    /// An answer's `Retry-After` header, as sent.
    fn retry_after_header(resp: &ureq::http::Response<ureq::Body>) -> Option<&str> {
        resp.headers().get("retry-after").and_then(|v| v.to_str().ok())
    }

    /// An answer with an error status (#509): its kept headers and the start of its body.
    fn refused(resp: &mut ureq::http::Response<ureq::Body>) -> Failure {
        use std::io::Read;
        let headers: Vec<(String, String)> = resp
            .headers()
            .iter()
            .filter(|(k, _)| crate::failure::KEPT_HEADERS.contains(&k.as_str()))
            .map(|(k, v)| (k.as_str().to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned()))
            .collect();
        let mut body = Vec::new();
        let _ = resp.body_mut().as_reader().take(crate::failure::MAX_BODY).read_to_end(&mut body);
        Failure::answered("", resp.status().as_u16(), &headers, &body).sent_as(USER_AGENT)
    }

    /// [`get`], telling what is known of a failure.
    pub(super) fn get_detailed(agent: &ureq::Agent, url: &str) -> Result<Vec<u8>, Failure> {
        get_detailed_omitting(agent, url, &[])
    }

    /// [`get_detailed`], a status in `omit` not recorded as a failure.
    pub(super) fn get_detailed_omitting(agent: &ureq::Agent, url: &str, omit: &'static [u16]) -> Result<Vec<u8>, Failure> {
        fetch_answer(agent, url, true, omit).map_err(|f| Failure {
            url: if url.contains(rustgis_data::login_address::PREFIX) { crate::logins::scrub_error(url) } else { url.to_string() },
            ..f
        })
    }

    /// A file's length from a `Content-Range` header (`bytes 0-0/1234`); none when the server
    /// doesn't know it (`*`), the file is empty or the header can't be read.
    pub(super) fn content_range_total(header: &str) -> Option<u64> {
        let (_, len) = header.rsplit_once('/')?;
        len.trim().parse::<u64>().ok().filter(|len| *len > 0)
    }

    /// The answer to `url`; a refusal keeps the server's answer only when `detail` asks for it,
    /// as tiles a server doesn't have are routine and only their status is read. A status in
    /// `omit` is an expected answer: its failure isn't recorded, only noted once per server. A
    /// request carrying a login is recorded by its plain address and scrubbed text only
    /// ([`RequestDiagnostics::failed`]).
    fn fetch_answer(agent: &ureq::Agent, url: &str, detail: bool, omit: &'static [u16]) -> Result<Vec<u8>, Failure> {
        use std::io::Read;
        let mut carried_login = false;
        // Local file failures are not network traffic.
        let local = crate::mbtiles::is_tile(url) || url.starts_with(LOCAL);
        let mut diagnostics = super::RequestDiagnostics::new(url, "GET");
        diagnostics.omit_statuses(omit);
        let answer = (|| -> Result<Vec<u8>, Failure> {
            super::check_current()?;
            if crate::mbtiles::is_tile(url) {
                return crate::mbtiles::read_local(url).map_err(Failure::from);
            }
            if let Some(local) = url.strip_prefix(LOCAL) {
                let (path, range) = super::split_range(local);
                let (a, b) = range.ok_or("a local read without a range")?;
                return crate::pmtiles::read_file(path, a, b - a + 1).map_err(Failure::from);
            }
            let (url, limit) = super::split_download_limit(url);
            let max = limit.unwrap_or(MAX_BYTES);
            let (url, mut tail) = super::split_tail(url);
            let (url, mut range) = super::split_range(url);
            // The last bytes asked for, while the file's length is asked for instead (see below).
            let mut measuring: Option<u64> = None;
            // An `s3://` address becomes its web address, and a request to an S3 connection's
            // endpoint is signed.
            crate::logins::check(url)?;
            let prepared = crate::s3::prepare(crate::logins::inner(url))?;
            let mut at = prepared.url.clone();
            let first_origin = crate::logins::origin(&at);
            let mut same_origin = true;
            let mut redirects = 0;
            let mut resp = loop {
                // The login goes to its own origin only: once a redirect leaves it, never again.
                let bound = crate::logins::inherit(url, &at);
                let login = if !prepared.signed && same_origin { crate::logins::apply(&bound)? } else { None };
                carried_login |= login.is_some();
                let mut req = agent.get(login.as_ref().map_or(at.as_str(), |a| a.url.as_str()));
                if let Some((name, value)) = login.as_ref().and_then(|a| a.header.as_ref()) {
                    req = req.header(name.as_str(), value.as_str());
                }
                if let Some((a, b)) = range {
                    req = req.header("Range", &format!("bytes={a}-{b}"));
                } else if let Some(n) = tail {
                    req = req.header("Range", &format!("bytes=-{n}"));
                }
                // A range of the file's own bytes: a server compressing its answer would count the
                // range in the compressed ones.
                if range.is_some() || tail.is_some() {
                    req = req.header("Accept-Encoding", "identity");
                }
                if redirects == 0 {
                    for (k, v) in &prepared.headers {
                        req = req.header(k.as_str(), v.as_str());
                    }
                }
                // An Earthdata Login token goes to NASA's Earthdata data hosts only, asked for at
                // each hop: a redirect that leaves them (to a presigned address) goes without it.
                if !prepared.signed
                    && let Some(auth) = crate::earthdata::authorization(&at)
                {
                    req = req.header("Authorization", &auth);
                }
                // An ArcGIS sign-in's token goes to the portal and the servers it covers, asked for at
                // each hop like Earthdata's.
                if !prepared.signed
                    && let Some(auth) = crate::arcgis_auth::authorization_fresh(&at)
                {
                    req = req.header("X-Esri-Authorization", &auth);
                }
                diagnostics.sending();
                super::check_current()?;
                let mut resp = req.call().map_err(unanswered)?;
                let status = resp.status().as_u16();
                diagnostics.answered(status);
                // A data host refusing the request (or the token): say what to do.
                if status == 401 && crate::earthdata::is_data_host(&at) {
                    return Err(Failure::without_answer("", crate::earthdata::refused_message(url), Reason::Status(status)));
                }
                // A store that serves ranges but not the last bytes by their count (GitHub's release
                // files, on Azure storage, answer `bytes=-n` with 501): the file's length from a
                // one-byte range's `Content-Range`, then the last bytes by their place, at the same
                // address. A server that gives no length goes on as one without ranges; the one-byte
                // range redirected is asked for again where it leads, and refused fails as any refusal.
                if let Some(n) = tail.filter(|_| status == 501 && range.is_none()) {
                    (measuring, tail, range) = (Some(n), None, Some((0, 0)));
                    continue;
                }
                if let Some(n) = measuring.filter(|_| status < 300) {
                    measuring = None;
                    let header = resp.headers().get("content-range").and_then(|v| v.to_str().ok());
                    match header.filter(|_| status == 206).and_then(content_range_total) {
                        Some(len) => {
                            range = Some((len.saturating_sub(n), len - 1));
                            // The probe's byte read, so its connection goes back to the pool for the
                            // next request; a body that won't read is left, the request goes on.
                            let _ = resp.body_mut().with_config().limit(64).read_to_vec();
                            continue;
                        }
                        None => return Err(format!("{url}: {}", super::NO_RANGES).into()),
                    }
                }
                if status >= 400 {
                    // A signed or logged-in request's refusal is told by its status alone: a server
                    // may echo its credentials in the answer's headers or body.
                    return Err(if detail && !prepared.signed && login.is_none() {
                        refused(&mut resp)
                    } else {
                        Failure::without_answer(
                            "",
                            crate::failure::with_retry_after(format!("http status: {status}"), status, retry_after_header(&resp)),
                            Reason::Status(status),
                        )
                    });
                }
                if !resp.status().is_redirection() {
                    break resp;
                }
                // A signed request goes only where its credentials are for.
                if prepared.signed {
                    return Err(format!(
                        "{url}: the S3 endpoint answered with a redirect (HTTP {}), which a signed request doesn't follow: \
                         check the connection's region and endpoint",
                        resp.status().as_u16()
                    )
                    .into());
                }
                let location = resp.headers().get("location").and_then(|v| v.to_str().ok()).ok_or("a redirect with no Location")?;
                let next = crate::template::join(&at, location);
                // An Earthdata data host sends a request it didn't accept to Earthdata Login.
                if crate::earthdata::is_data_host(&at) && crate::earthdata::is_login(&next) {
                    return Err(Failure::without_answer("", crate::earthdata::refused_message(url), Reason::Status(401)));
                }
                // Where a redirect leads is the server's choice, not the user's.
                crate::address::check(&next, Some(&at))?;
                // Never from HTTPS down to plain HTTP, however either scheme is written.
                if crate::address::is_https(&at) && !crate::address::is_https(&next) {
                    return Err(format!("{next}: a redirect from https to http").into());
                }
                redirects += 1;
                if redirects > 5 {
                    return Err("too many redirects".into());
                }
                same_origin &= crate::logins::origin(&next) == first_origin;
                at = next;
            };
            if resp.status().as_u16() == 204 {
                return Ok(Vec::new());
            }
            // A range asked for and the whole file answered (200): fine for a range from the start
            // that holds the whole file (a small archive's header read), cut to the range; otherwise
            // the server doesn't do ranges.
            let whole = range.is_some() && resp.status().as_u16() != 206;
            if let Some((a, _)) = range.filter(|_| whole)
                && a > 0
            {
                return Err(format!("{url}: {}", super::NO_RANGES).into());
            }
            let mut buf = Vec::new();
            if !resp.status().is_success() {
                return Err(format!("HTTP {}", resp.status().as_u16()).into());
            }
            // The last bytes asked for and the whole file answered: fine when it is no longer.
            if let Some(n) = tail.filter(|_| resp.status().as_u16() != 206) {
                // Read one byte past `n` (no more, and no error at the limit): a longer file is
                // reported as a server without ranges, so the file is downloaded whole instead.
                let limit = n.min(max as u64).saturating_add(1);
                resp.body_mut()
                    .with_config()
                    .limit(limit.saturating_add(1))
                    .reader()
                    .take(limit)
                    .read_to_end(&mut buf)
                    .map_err(|e| e.to_string())?;
                if buf.len() as u64 > n {
                    return Err(format!("{url}: {}", super::NO_RANGES).into());
                }
                if buf.len() > max {
                    return Err(format!("larger than {max} bytes").into());
                }
                return Ok(buf);
            }
            // ureq limits the wire bytes; `take` also caps the decoded bytes when gzip expands them.
            resp.body_mut()
                .with_config()
                .limit(max as u64 + 2)
                .reader()
                .take(max as u64 + 1)
                .read_to_end(&mut buf)
                .map_err(|e| e.to_string())?;
            if buf.len() > max {
                return Err(format!("larger than {max} bytes").into());
            }
            if let Some((a, b)) = range.filter(|_| whole) {
                if buf.len() as u64 > b - a + 1 {
                    return Err(format!("{url}: {}", super::NO_RANGES).into());
                }
                buf.truncate(usize::try_from(b - a + 1).unwrap_or(usize::MAX));
            }
            Ok(buf)
        })();
        let result = if local { answer } else { diagnostics.record(answer) };
        let result = crate::logins::check(super::resource_of(url)).map_err(Failure::from).and(result);
        result.map_err(|mut f| {
            if carried_login {
                f.message = crate::logins::scrub_error(&f.message);
                f.url = crate::logins::scrub_error(&f.url);
            }
            f
        })
    }
}

#[cfg(target_arch = "wasm32")]
mod native {
    use super::{Done, DoneDetailed};

    pub(super) fn fetch(url: String, done: Done, _omit: &'static [u16]) {
        done(Err(format!("{url}: this build has no way to fetch tiles")));
    }

    pub(super) fn download_within(url: String, _timeout: std::time::Duration, _omit: &'static [u16], done: DoneDetailed) {
        let message = format!("{url}: this build has no way to fetch tiles");
        done(Err(crate::failure::Failure::new(&url, message)));
    }

    pub(super) fn post(url: String, _body: String, done: Done) {
        done(Err(format!("{url}: this build has no way to send requests")));
    }

    pub(super) fn head(url: String, done: super::HeadDone) {
        done(Err(format!("{url}: this build has no way to send requests")));
    }

    pub(super) fn fetch_local(url: String, done: Done) {
        let (path, _) = super::split_range(&url);
        done(Err(format!("{path}: the web app reads PMTiles archives from web addresses only")));
    }
}

#[cfg(test)]
mod tests {
    /// Runs a fixture that needs stable process-wide cache entries without unrelated login
    /// revocation clearing them. The exact named child must execute its existing assertions.
    #[cfg(not(target_arch = "wasm32"))]
    fn isolate_cache_fixture(name: &str) -> bool {
        const MARK: &str = "RUSTGIS_TEST_CACHE_FIXTURE";
        if std::env::var(MARK).as_deref() == Ok(name) {
            return false;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name, "--nocapture", "--test-threads=1"])
            .env(MARK, name)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{name}:\n{stdout}\n{}", String::from_utf8_lossy(&output.stderr));
        assert!(stdout.contains("1 passed; 0 failed"), "the child must run exactly {name}: {stdout}");
        true
    }

    use super::*;

    /// A full record of forgotten files is emptied. A tile asked for before that can't be told, so
    /// it isn't kept and is asked for again; one asked for since is, and so is what isn't MBTiles.
    #[test]
    fn a_full_record_of_forgotten_files_has_older_tiles_read_again() {
        // A turn: another test forgetting files meanwhile could empty the record of them.
        let _turn = turn();
        let archive = |i: usize| format!("/forgotten-{i}.mbtiles");
        let key = crate::mbtiles::template(&archive(0)).replace("{z}", "0").replace("{x}", "0").replace("{y}", "0");
        let failed = |why: &str| Some(Entry::Failed(why.into(), Instant::now()));
        let old = crate::mbtiles::epoch();
        lock().slots.insert(key.clone(), Slot { entry: Entry::Pending, used: 0, asked: Instant::now() });
        for i in 0..=crate::mbtiles::MAX_FORGOTTEN {
            crate::mbtiles::forget_archive(&archive(i));
        }
        assert!(crate::mbtiles::forgotten_len() <= crate::mbtiles::MAX_FORGOTTEN);
        assert!(!crate::mbtiles::forgotten_since(&key, old), "the record no longer holds it");

        land(&key, failed("old file"), old);
        assert!(!lock().slots.contains_key(&key));
        land(&key, failed("new file"), crate::mbtiles::epoch());
        assert!(matches!(lock().slots.get(&key).map(|s| &s.entry), Some(Entry::Failed(why, _)) if why == "new file"));
        land("test://not-mbtiles/forgotten.json", failed("kept"), old);
        assert!(lock().slots.remove("test://not-mbtiles/forgotten.json").is_some());
        forget(&archive(0));
    }

    /// An MBTiles file whose name reads like a COG's look (`#cube=1`) is forgotten under its whole
    /// path, the one its tiles name: its tiles go, and a read of it under way isn't kept.
    #[test]
    fn an_mbtiles_file_named_like_a_look_is_forgotten_whole() {
        let _turn = turn();
        let archive = "/maps#cube=1.mbtiles";
        let key = crate::mbtiles::template(archive).replace("{z}", "0").replace("{x}", "0").replace("{y}", "0");
        let old = crate::mbtiles::epoch();
        lock().slots.insert(key.clone(), Slot { entry: Entry::Failed("old file".into(), Instant::now()), used: 0, asked: Instant::now() });
        forget(archive);
        assert!(!lock().slots.contains_key(&key));
        assert!(crate::mbtiles::forgotten_since(&key, old), "the file is recorded under the path its tiles name");
        land(&key, Some(Entry::Failed("old file".into(), Instant::now())), old);
        assert!(!lock().slots.contains_key(&key));
    }

    /// A tile still being read when its MBTiles file is forgotten isn't kept when it lands, even
    /// after the same tile was asked for again and answered; a read begun since is.
    #[test]
    fn a_forgotten_mbtiles_file_drops_the_tile_being_read() {
        // A turn: another test forgetting files meanwhile could empty the record of them.
        let _turn = turn();
        let archive = "/forgotten-pending.mbtiles";
        let key = crate::mbtiles::template(archive).replace("{z}", "0").replace("{x}", "0").replace("{y}", "0");
        let failed = |why: &str| Some(Entry::Failed(why.into(), Instant::now()));
        let kept = || match lock().slots.get(&key).map(|s| &s.entry) {
            Some(Entry::Failed(why, _)) => Some(why.clone()),
            _ => None,
        };

        let old = crate::mbtiles::epoch();
        lock().slots.insert(key.clone(), Slot { entry: Entry::Pending, used: 0, asked: Instant::now() });
        forget(archive);
        land(&key, failed("old file"), old);
        assert_eq!(kept(), None);
        assert!(!lock().slots.contains_key(&key));

        // Asked for again before the old read is over: the new read lands first, or last.
        land(&key, failed("new file"), crate::mbtiles::epoch());
        land(&key, failed("old file"), old);
        assert_eq!(kept().as_deref(), Some("new file"));
        forget(archive);

        // Or the old one lands while the new one is pending: the new one's answer is what stays.
        let new = crate::mbtiles::epoch();
        lock().slots.insert(key.clone(), Slot { entry: Entry::Pending, used: 0, asked: Instant::now() });
        land(&key, failed("old file"), old);
        assert!(matches!(lock().slots.get(&key), Some(Slot { entry: Entry::Pending, .. })), "the new read's slot stays");
        land(&key, None, old);
        assert!(matches!(lock().slots.get(&key), Some(Slot { entry: Entry::Pending, .. })), "also if the old read went unanswered");
        land(&key, failed("new file"), new);
        assert_eq!(kept().as_deref(), Some("new file"));
        forget(archive);
    }

    /// Queued fetches and dedicated download threads recheck authority on actual start.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn revoked_workers_send_nothing_and_callbacks_inherit_context() {
        use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
        for kind in 0..2 {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let url = format!("http://{}/held", listener.local_addr().unwrap());
            let live = Arc::new(AtomicBool::new(true));
            let first = Arc::new(AtomicBool::new(true));
            let (entered, entry) = std::sync::mpsc::channel();
            let (release, released) = std::sync::mpsc::channel();
            let released = Mutex::new(released);
            let flag = live.clone();
            let still: Still = Arc::new(move || {
                if first.swap(false, SeqCst) {
                    entered.send(()).unwrap();
                    released.lock().unwrap().recv().unwrap();
                }
                flag.load(SeqCst)
            });
            let (done, result) = std::sync::mpsc::channel();
            let callback = Box::new(move |r: Result<Vec<u8>, String>| {
                assert!(check_still(&current_still()).is_err(), "a follow-on request inherits revocation");
                done.send(r).unwrap();
            });
            with_still(still, || match kind {
                0 => native::fetch(url, callback, &[]),
                _ => native::download(url, Box::new(move |r| callback(r.map_err(|e| e.message)))),
            });
            entry.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
            live.store(false, SeqCst);
            release.send(()).unwrap();
            assert!(result.recv_timeout(std::time::Duration::from_secs(10)).unwrap().unwrap_err().contains("cancelled"));
            assert_eq!(listener.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
            assert!(current_still().is_none());
        }
    }

    fn network_records(history: &rustgis_data::diagnostics::History, url: &str) -> Vec<rustgis_data::diagnostics::Record> {
        history.records().into_iter().filter(|record| record.network.as_ref().is_some_and(|network| network.url == url)).collect()
    }

    /// Exercise the worker used for tiles, styles, services and remote data ranges, without
    /// replacing the process-wide browser fetcher (other tests may be using it).
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn network_diagnostics_capture_background_failures_once() {
        let _turn = turn();
        use rustgis_data::diagnostics::{History, NetworkKind};
        let history = History::default();
        let _scope = history.enter();
        let base = serve(vec!["HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into()]);
        let expected = format!("{base}/data.parquet");
        let (tx, rx) = std::sync::mpsc::channel();
        native::fetch(format!("{expected}#rustgis-range=20-39"), Box::new(move |result| tx.send(result).unwrap()), &[]);
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap().unwrap_err(), "http status: 503");
        let records = network_records(&history, &expected);
        assert_eq!(records.len(), 1);
        let network = records[0].network.as_ref().unwrap();
        assert_eq!((&*network.method, network.status, network.kind), ("GET", Some(503), NetworkKind::Http));
        assert!(network.duration_ms.is_some());
        assert!(records[0].details.contains("bytes=20-39"));
        assert!(!network.url.contains("rustgis-range"));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn network_diagnostics_interleaved_sessions_keep_their_requests_private() {
        let _turn = turn();
        use rustgis_data::diagnostics::History;
        use std::io::{BufRead, BufReader, Write};
        let first = History::default();
        let second = History::default();
        let unrelated = History::default();
        let timeout = std::time::Duration::from_secs(10);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let first_url = format!("http://{}/first-session", listener.local_addr().unwrap());
        let (arrived_tx, arrived_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(timeout)).unwrap();
            {
                let mut request = BufReader::new(&stream);
                let mut line = String::new();
                while request.read_line(&mut line).unwrap() > 0 && line != "\r\n" {
                    line.clear();
                }
            }
            arrived_tx.send(()).unwrap();
            release_rx.recv_timeout(timeout).unwrap();
            stream.write_all(b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        });
        let (first_tx, first_rx) = std::sync::mpsc::channel();
        {
            let _scope = first.enter();
            native::fetch(first_url.clone(), Box::new(move |result| first_tx.send(result).unwrap()), &[]);
        }
        arrived_rx.recv_timeout(timeout).unwrap();

        let base = serve(vec!["HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into()]);
        let second_url = format!("{base}/second-session");
        let (second_tx, second_rx) = std::sync::mpsc::channel();
        {
            let _scope = second.enter();
            native::download(second_url.clone(), Box::new(move |result| second_tx.send(result).unwrap()));
            assert_eq!(second_rx.recv_timeout(timeout).unwrap().unwrap_err().message, "http status: 403");
            // The first request finishes while another session owns this dispatch thread.
            release_tx.send(()).unwrap();
            assert_eq!(first_rx.recv_timeout(timeout).unwrap().unwrap_err(), "http status: 503");
        }
        server.join().unwrap();

        let base = serve(vec!["HTTP/1.1 404 Missing\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into()]);
        let (unowned_tx, unowned_rx) = std::sync::mpsc::channel();
        native::fetch(format!("{base}/without-owner"), Box::new(move |result| unowned_tx.send(result).unwrap()), &[]);
        assert_eq!(unowned_rx.recv_timeout(timeout).unwrap().unwrap_err(), "http status: 404");

        let first_records = first.records();
        assert_eq!(first_records.len(), 1);
        assert_eq!(first_records[0].network.as_ref().unwrap().url, first_url);
        let second_records = second.records();
        assert_eq!(second_records.len(), 1);
        assert_eq!(second_records[0].network.as_ref().unwrap().url, second_url);
        assert!(unrelated.records().is_empty());
    }

    #[test]
    fn network_diagnostics_explicit_capture_outlives_dispatch_scope_not_owner() {
        let _turn = turn();
        use rustgis_data::diagnostics::History;
        let first = History::default();
        let second = History::default();
        let capture = {
            let _scope = first.enter();
            Capture::current()
        };
        let _scope = second.enter();
        let url = "https://diagnostics-browser.invalid/delayed-first-session";
        let diagnostics = RequestDiagnostics::with_capture(url, "GET", capture);
        diagnostics.failed(&Failure::new(url, "the browser couldn't reach it"));
        assert_eq!(network_records(&first, url).len(), 1);
        assert!(second.records().is_empty());

        let capture = {
            let closed = History::default();
            closed.capture()
        };
        RequestDiagnostics::with_capture(url, "GET", capture).failed(&Failure::new(url, "owner closed"));
        assert_eq!(network_records(&first, url).len(), 1);
        assert!(second.records().is_empty());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn network_diagnostics_redact_downloads_without_retaining_bodies() {
        let _turn = turn();
        use rustgis_data::diagnostics::History;
        let history = History::default();
        let _scope = history.enter();
        let secret_body = "private-response-body-do-not-store";
        let base = serve(vec![format!(
            "HTTP/1.1 403 Forbidden\r\nX-Request-Id: request-1155\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{secret_body}",
            secret_body.len()
        )]);
        let url = format!("{base}/download.json?token=private-query-token");
        let (tx, rx) = std::sync::mpsc::channel();
        native::download(download_address(&url, 1024), Box::new(move |result| tx.send(result).unwrap()));
        let failure = rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap().unwrap_err();
        // Preserve the existing detailed failure API; diagnostics deliberately excludes its excerpt.
        assert_eq!(failure.excerpt(), Some(secret_body));
        let records = network_records(&history, &format!("{base}/download.json"));
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert!(record.details.contains("request-1155"));
        let network = record.network.as_ref().unwrap();
        assert_eq!(network.status, Some(403));
        let retained = format!("{} {} {} {}", record.source, record.message, record.details, network.url);
        assert!(!retained.contains(secret_body));
        assert!(!retained.contains("private-query-token"));
        assert!(!retained.contains("rustgis-download"));

        let base = serve(vec!["HTTP/1.1 500 Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into()]);
        assert_eq!(native::post_with(&native::agent(), &format!("{base}/post"), "private-request-body").unwrap_err(), "http status: 500");
        let records = network_records(&history, &format!("{base}/post"));
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].network.as_ref().unwrap().method, "POST");
        assert!(!records[0].details.contains("private-request-body"));

        let base = serve(vec!["HTTP/1.1 404 Missing\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into()]);
        assert_eq!(native::head_with(&native::agent(), &format!("{base}/head")).unwrap_err(), "http status: 404");
        let records = network_records(&history, &format!("{base}/head"));
        assert_eq!(records.len(), 1);
        let network = records[0].network.as_ref().unwrap();
        assert_eq!((&*network.method, network.status), ("HEAD", Some(404)));

        let base = serve(vec!["HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}".into()]);
        let url = format!("{base}/successful.json");
        assert_eq!(native::get(&native::agent(), &url, &[]).unwrap(), b"{}");
        assert!(network_records(&history, &url).is_empty());
    }

    /// A map tile a server no longer has (410) is routine, as a missing one (404) is: noted once
    /// for the server, not recorded as a failure. A server error is still recorded.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn network_diagnostics_skip_gone_map_tiles_and_keep_server_errors() {
        let _turn = turn();
        let history = rustgis_data::diagnostics::History::default();
        let _scope = history.enter();
        let answer = |status: &str| format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        let base = serve(vec![answer("410 Gone"), answer("500 Error")]);
        let gone = format!("{base}/tiles/0/0/0.png");
        assert_eq!(native::get(&native::agent(), &gone, TILE_OMIT).unwrap_err(), "http status: 410");
        let notes = network_records(&history, &gone);
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].severity, rustgis_data::diagnostics::Severity::Warning, "a note, not a failure");
        let failed = format!("{base}/tiles/1/1/1.png");
        assert_eq!(native::get(&native::agent(), &failed, TILE_OMIT).unwrap_err(), "http status: 500");
        let records = network_records(&history, &failed);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].severity, rustgis_data::diagnostics::Severity::Error);
    }

    /// A credential header in a refusal is taken out by itself: the headers after it (which
    /// say who refused) are kept.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn network_diagnostics_keep_headers_after_a_credential_header() {
        let _turn = turn();
        let history = rustgis_data::diagnostics::History::default();
        let _scope = history.enter();
        let base = serve(vec![
            "HTTP/1.1 403 Forbidden\r\nWWW-Authenticate: Basic realm=\"private-realm\"\r\nRetry-After: 30\r\ncf-ray: 8a1-BOG\r\nX-Amzn-RequestId: amzn-1155\r\nX-Amz-Cf-Id: trace-1155\r\nX-Amzn-Waf-Action: block\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .into(),
        ]);
        let url = format!("{base}/refused");
        native::head_with(&native::agent(), &url).unwrap_err();
        let records = network_records(&history, &url);
        assert_eq!(records.len(), 1);
        let details = &records[0].details;
        for kept in
            ["retry-after: 30", "cf-ray: 8a1-BOG", "x-amzn-requestid: amzn-1155", "x-amz-cf-id: trace-1155", "x-amzn-waf-action: block"]
        {
            assert!(details.contains(kept), "{kept} in {details:?}");
        }
        assert!(!details.contains("private-realm"), "{details:?}");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn network_diagnostics_skip_routine_tile_404s_and_keep_other_statuses() {
        let _turn = turn();
        use rustgis_data::diagnostics::{History, Severity};
        let history = History::default();
        let _scope = history.enter();
        let not_found = "HTTP/1.1 404 Missing\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let missing = serve(vec![not_found.into(), not_found.into(), not_found.into(), not_found.into()]);
        // Tiles a server doesn't have are routine: not a network record each, but the omission
        // is said once, so a misconfigured source isn't silent.
        let url = format!("{missing}/tiles/0/0/0.png");
        assert_eq!(native::get(&native::agent(), &url, &[404]).unwrap_err(), "http status: 404");
        let records = network_records(&history, &url);
        assert_eq!(records.len(), 1);
        assert_eq!((records[0].severity, records[0].source.as_str()), (Severity::Warning, "Network"));
        // A style's or TileJSON's 404 is a real failure: its own record, and the omission is
        // not said twice.
        let url = format!("{missing}/style.json");
        assert_eq!(native::get(&native::agent(), &url, &[]).unwrap_err(), "http status: 404");
        let records = network_records(&history, &url);
        assert_eq!(records.len(), 1, "a JSON want's 404 is a real failure");
        assert_eq!(records[0].network.as_ref().unwrap().status, Some(404));
        // A later omitted 404 from that server adds nothing.
        let url = format!("{missing}/tiles/9/9/9.png");
        assert_eq!(native::get(&native::agent(), &url, &[404]).unwrap_err(), "http status: 404");
        assert!(network_records(&history, &url).is_empty());
        // Another server's omitted 404 is said too: a second misconfigured source isn't silent
        // because the first has spoken.
        let other = serve(vec![not_found.into(), not_found.into()]);
        let url = format!("{other}/tiles/0/0/0.png");
        assert_eq!(native::get(&native::agent(), &url, &[404]).unwrap_err(), "http status: 404");
        assert_eq!(network_records(&history, &url).len(), 1, "each server's omission is said");
        // After Clear, that server is said again.
        history.clear();
        let url = format!("{other}/tiles/1/1/1.png");
        assert_eq!(native::get(&native::agent(), &url, &[404]).unwrap_err(), "http status: 404");
        assert_eq!(network_records(&history, &url).len(), 1, "cleared history says it again");
        // A refusal that is not a missing tile is kept.
        let base = serve(vec!["HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into()]);
        let url = format!("{base}/tiles/0/0/0.png");
        assert_eq!(native::get(&native::agent(), &url, &[404]).unwrap_err(), "http status: 503");
        let records = network_records(&history, &url);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].network.as_ref().unwrap().status, Some(503));
        // One that says when to ask again: its wait goes with the status, read back as the wait.
        let wait = "HTTP/1.1 503 Unavailable\r\nRetry-After: 30\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let base = serve(vec![wait.into()]);
        let e = native::get(&native::agent(), &format!("{base}/tiles/0/0/0.png"), &[404]).unwrap_err();
        assert_eq!(e, "http status: 503 (retry after 30 s)");
        assert_eq!((http_status(&e), retry_wait(&e, RETRY)), (Some(503), Duration::from_secs(30)));
        // A download's absence names the address asked for, not the module's markers.
        let base = serve(vec![not_found.into()]);
        let url = format!("{base}/data.zarr/zarr.json");
        native::get(&native::download_agent(std::time::Duration::from_secs(10)), &download_address(&url, 64), &[404]).unwrap_err();
        let report = history.report(rustgis_data::diagnostics::Filter::All);
        assert!(!report.contains("rustgis-download"), "{report}");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn network_diagnostics_keep_observed_status_on_blocking_body_failure() {
        let _turn = turn();
        use rustgis_data::diagnostics::{History, NetworkKind};
        let history = History::default();
        let _scope = history.enter();
        let base = serve(vec!["HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ntile".into()]);
        let url = format!("{base}/large.bin?key=private-download-key");
        assert_eq!(download_blocking_limited(&url, 3).unwrap_err(), "larger than 3 bytes");
        let records = network_records(&history, &format!("{base}/large.bin"));
        assert_eq!(records.len(), 1);
        let network = records[0].network.as_ref().unwrap();
        assert_eq!((network.status, network.kind), (Some(200), NetworkKind::Other));
        assert!(network.duration_ms.is_some());
        assert!(!network.url.contains("private-download-key"));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn network_diagnostics_preserve_connection_and_timeout_causes() {
        let _turn = turn();
        use rustgis_data::diagnostics::{History, NetworkKind};
        let history = History::default();
        let _scope = history.enter();
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        let url = format!("http://{closed}/connection-refused-1155");
        native::get(&native::agent(), &url, &[]).unwrap_err();
        let records = network_records(&history, &url);
        assert_eq!(records.len(), 1);
        let network = records[0].network.as_ref().unwrap();
        assert_eq!((network.status, network.kind), (None, NetworkKind::Connection));
        assert!(network.duration_ms.is_some());

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/timeout-1155", listener.local_addr().unwrap());
        let (tx, rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let _ = rx.recv_timeout(std::time::Duration::from_secs(10));
            drop(stream);
        });
        native::get(&native::download_agent(std::time::Duration::from_millis(40)), &url, &[]).unwrap_err();
        tx.send(()).unwrap();
        server.join().unwrap();
        let records = network_records(&history, &url);
        assert_eq!(records.len(), 1);
        let network = records[0].network.as_ref().unwrap();
        assert_eq!((network.status, network.kind), (None, NetworkKind::Timeout));
        assert!(network.duration_ms.is_some_and(|duration| duration >= 1));
    }

    /// A local read (a PMTiles archive's part on disk) that fails is not network traffic: the
    /// history keeps no record of it.
    #[test]
    fn network_diagnostics_skip_failed_local_reads() {
        let _turn = turn();
        let history = rustgis_data::diagnostics::History::default();
        let _scope = history.enter();
        native::get(&native::agent(), "local-file:/nonexistent-1155.pmtiles", &[]).unwrap_err();
        assert!(history.records().is_empty(), "{:?}", history.records());
    }

    #[test]
    fn network_diagnostics_ignore_cancellation_and_keep_browser_causes_truthful() {
        let _turn = turn();
        use rustgis_data::diagnostics::{History, NetworkKind};
        let history = History::default();
        let _scope = history.enter();
        let url = "https://diagnostics-browser.invalid/opaque-1155";
        let diagnostics = RequestDiagnostics::new(url, "GET");
        diagnostics.failed(&Failure::new(url, DROPPED));
        assert!(network_records(&history, url).is_empty());
        diagnostics.failed(&Failure::new(url, "the browser couldn't reach it"));
        let records = network_records(&history, url);
        assert_eq!(records.len(), 1);
        let network = records[0].network.as_ref().unwrap();
        assert_eq!((network.status, network.kind), (None, NetworkKind::Other));
        let url = "https://diagnostics-browser.invalid/stall-1155";
        let mut diagnostics = RequestDiagnostics::new(url, "GET");
        diagnostics.answered(200);
        diagnostics.timed_out();
        diagnostics.failed(&Failure::new(url, "nothing came in 30 seconds"));
        let records = network_records(&history, url);
        assert_eq!(records.len(), 1);
        let network = records[0].network.as_ref().unwrap();
        assert_eq!((network.status, network.kind), (Some(200), NetworkKind::Timeout));
    }

    /// A name for this computer passes the check as written but is refused where it resolves.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn public_downloads_refuse_names_that_resolve_locally() {
        let _turn = turn();
        let err = download_public_within("http://localhost:9/doc.json", 1024, std::time::Duration::from_secs(5)).unwrap_err();
        assert!(err.contains("resolves to this computer or the local network"), "{err}");
    }

    #[test]
    fn announced_tiff_header_lengths() {
        assert_eq!(tiff_header_length(" 16384 "), Some(16384));
        assert!(accepts_byte_ranges("bytes") && accepts_byte_ranges("none, Bytes") && !accepts_byte_ranges("none"));
        for bad in ["", "0", "-5", "12kb", "18446744073709551616"] {
            assert_eq!(tiff_header_length(bad), None, "{bad}");
        }
    }

    #[test]
    fn download_markers_preserve_addresses_and_budgets() {
        let url = "https://example.org/items?token=a%2Fb&key=x:x";
        assert_eq!(split_download_limit(url), (url, None));
        assert_eq!(split_download_limit(&format!("{DOWNLOAD}{url}")), (url, Some(MAX_DOWNLOAD)));
        assert_eq!(split_download_limit(&format!("{LIMITED_DOWNLOAD}17:{url}")), (url, Some(17)));
        assert_eq!(split_download_limit(&format!("{LIMITED_DOWNLOAD}0:{url}")), (url, Some(0)));
        assert_eq!(split_download_limit(&format!("{LIMITED_DOWNLOAD}{}:{url}", usize::MAX)), (url, Some(MAX_DOWNLOAD)));
        assert_eq!(split_download(&format!("{LIMITED_DOWNLOAD}17:{url}")), (url, true));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn downloads_stop_at_the_callers_budget() {
        let _turn = turn();
        for framing in ["Content-Length: 4\r\n", "", "Transfer-Encoding: chunked\r\n"] {
            let body = if framing.starts_with("Transfer") { "4\r\ntile\r\n0\r\n\r\n" } else { "tile" };
            for (limit, ok) in [(0, false), (3, false), (4, true), (5, true)] {
                let base = serve(vec![format!("HTTP/1.1 200 OK\r\n{framing}Connection: close\r\n\r\n{body}")]);
                let result = download_blocking_limited(&base, limit);
                assert_eq!(result.is_ok(), ok, "{framing}, {limit}: {result:?}");
                if ok {
                    assert_eq!(result.unwrap(), b"tile");
                }
            }
        }
        // A server promising a huge body but sending only budget + 1 bytes must fail now,
        // not try to read the promised body (which would yield an unexpected EOF).
        let base = serve(vec!["HTTP/1.1 200 OK\r\nContent-Length: 999999999\r\nConnection: close\r\n\r\ntile".into()]);
        assert!(download_blocking_limited(&base, 3).unwrap_err().contains("larger than"));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn download_budget_also_bounds_decompressed_bytes() {
        let _turn = turn();
        let data = vec![b'x'; 4096];
        // The ZIP writer supplies the CRC-32 for a valid, generated gzip trailer.
        let zip = rustgis_data::zip::write(&[("body".into(), data.clone())]).unwrap();
        let mut gzip = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 255];
        gzip.extend(miniz_oxide::deflate::compress_to_vec(&data, 6));
        gzip.extend_from_slice(&zip[14..18]);
        gzip.extend_from_slice(&(data.len() as u32).to_le_bytes());
        assert!(gzip.len() < 64);
        for limit in [64, data.len()] {
            let mut response =
                format!("HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", gzip.len())
                    .into_bytes();
            response.extend_from_slice(&gzip);
            let result = download_blocking_limited(&serve_bytes(vec![response]), limit);
            if limit == data.len() {
                assert_eq!(result.unwrap(), data);
            } else {
                assert_eq!(result.unwrap_err(), "larger than 64 bytes");
            }
        }
    }

    #[test]
    fn http_statuses_from_whole_messages() {
        assert_eq!(http_status("http status: 404"), Some(404));
        assert_eq!(http_status("HTTP 403"), Some(403));
        assert_eq!(http_status("HTTP 500"), Some(500));
        // A number inside another message is not a status.
        assert_eq!(http_status("https://x.org/404/a.zarr: timed out"), None);
        assert_eq!(http_status("HTTP 4040"), None);
        assert_eq!(http_status("io: connection reset (read 404 bytes)"), None);
        // With how long the server asked to be left: the status, and the wait.
        assert_eq!(http_status("http status: 503 (retry after 30 s)"), Some(503));
        assert_eq!(http_status("HTTP 429 (retry after 2 s)"), Some(429));
        assert_eq!(http_status("HTTP 429 (something else)"), None);
        assert_eq!(retry_after("http status: 503 (retry after 30 s)"), Some(Duration::from_secs(30)));
        assert_eq!(retry_after("http status: 503"), None);
        assert_eq!(retry_after("x (retry after 18446744073709551615 s)"), Some(Duration::from_secs(3600)), "bounded, never overflowing");
        assert_eq!(retry_after("x (retry after 0 s)"), Some(Duration::from_secs(1)));
        assert_eq!(retry_wait("HTTP 429 (retry after 2 s)", RETRY), Duration::from_secs(2));
        assert_eq!(retry_wait("http status: 500", RETRY), RETRY);
    }

    /// A read that fails again waits twice as long each time, within the hour: a file that never
    /// reads isn't asked for every minute all session.
    #[test]
    fn a_read_that_fails_again_waits_longer_each_time() {
        let minute = Duration::from_secs(60);
        assert_eq!(backoff(minute, 0), minute);
        assert_eq!(backoff(minute, 1), minute);
        assert_eq!(backoff(minute, 2), Duration::from_secs(120));
        assert_eq!(backoff(minute, 3), Duration::from_secs(240));
        assert_eq!(backoff(minute, 7), Duration::from_secs(3600), "within the hour");
        assert_eq!(backoff(minute, u32::MAX), Duration::from_secs(3600), "bounded, never overflowing");
        // A server's own wait is doubled the same way, within the hour.
        assert_eq!(backoff(Duration::from_secs(1), 2), Duration::from_secs(2));
        assert_eq!(backoff(Duration::from_secs(3600), 2), Duration::from_secs(3600));
    }

    /// A tile its server refused saying when to come back (429 or 503 with `Retry-After`) has the
    /// map asked to draw again then, from the gate, on each draw while it waits (the alarm is
    /// one-shot, and another may have fired since; the failure arms it first). The alarm is the
    /// earliest pending one, read here as the map reads it. The entry is stored as a failed fetch
    /// stores it: the fetcher is one for the process, and another test's may be in.
    #[test]
    fn a_tile_refused_with_retry_after_has_the_map_drawn_again_then() {
        // Its turn at the cache (the entry is planted in it, and another test's clearing would
        // drop it) and at the alarms.
        let _turn = super::turn();
        let _alarm = super::alarm_turn();
        let within = |secs: u64| super::next_alarm().is_some_and(|d| d <= Duration::from_secs(secs));
        let url = "test://cache/busy.json";
        store(url, Entry::Failed("http status: 503 (retry after 2 s)".into(), Instant::now()));
        super::clear_alarms();
        let State::Failed(e) = get(url, Want::Json) else { panic!("not failed") };
        assert!(e.contains("retry after 2 s"), "{e}");
        assert!(within(2), "no alarm on a draw while waiting: {:?}", super::next_alarm());
        // Again on the next draw: the alarm fired (or was consumed) before the wait was over.
        super::clear_alarms();
        assert!(matches!(get(url, Want::Json), State::Failed(_)));
        assert!(within(2), "not armed again: {:?}", super::next_alarm());
    }

    /// A read that failed asks the map to draw again when it may be tried: the generation goes up
    /// once that time has come.
    #[test]
    fn the_map_draws_again_when_a_wait_is_over() {
        let _alarm = super::alarm_turn();
        let before = generation();
        notify_at(Instant::now() + Duration::from_millis(20));
        std::thread::sleep(std::time::Duration::from_millis(40));
        assert!(generation() > before);
    }

    /// Every wait asked for has the map drawn again when it is over, not the earliest alone: the
    /// draw when the earlier is over may not ask for the later again (a style kept whole while
    /// its elevation source waits), so the later is kept, and the app asked for it then.
    #[test]
    fn a_later_wait_isnt_lost_behind_an_earlier_one() {
        let mut alarms = std::collections::BTreeSet::from([20, 150]);
        assert_eq!(super::take_due(&mut alarms, 10), (false, Some(20)));
        assert_eq!(super::take_due(&mut alarms, 50), (true, Some(150)), "rearm the later wait after the first expires");
        assert_eq!(super::take_due(&mut alarms, 100), (false, Some(150)));
        assert_eq!(super::take_due(&mut alarms, 200), (true, None));
        assert_eq!(super::take_due(&mut alarms, 300), (false, None), "expired deadlines are consumed once");
        let mut delayed = std::collections::BTreeSet::from([20, 150]);
        assert_eq!(super::take_due(&mut delayed, 200), (true, None), "a delayed poll correctly consumes both waits");
    }

    /// A draw before a wait is over asks the app for it again: the app's request lasts one frame,
    /// and a draw since (the pointer moved) used it up.
    #[test]
    fn a_draw_before_a_wait_is_over_asks_for_it_again() {
        let _alarm = super::alarm_turn();
        let asked = std::sync::Arc::new(Mutex::new(Vec::new()));
        let noted = asked.clone();
        super::set_alarm(move |after| noted.lock().unwrap_or_else(|e| e.into_inner()).push(after));
        super::clear_alarms();
        notify_at(Instant::now() + Duration::from_millis(300));
        asked.lock().unwrap_or_else(|e| e.into_inner()).clear();
        let _ = generation();
        let again = asked.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|d| *d <= Duration::from_millis(300));
        assert!(again, "{:?}", asked.lock().unwrap_or_else(|e| e.into_inner()));
        super::clear_alarms();
        super::clear_alarm();
    }

    #[test]
    fn a_decoder_set_for_pictures_decodes_them() {
        let mut pm = Pixmap::new(3, 2).unwrap_or_else(|| panic!("pixmap"));
        pm.fill(tiny_skia::Color::from_rgba8(200, 100, 50, 255));
        let png = pm.encode_png().unwrap_or_else(|e| panic!("{e}"));
        let calls = Arc::new(AtomicUsize::new(0));
        // Stands in for the browser's decoder: the picture as RAW pixels, or the bytes back.
        let counted = calls.clone();
        let raw: ImageDecoder = Box::new(move |b: Vec<u8>, done: Decoded| {
            counted.fetch_add(1, Ordering::Relaxed);
            let p = decode_image(&b).unwrap_or_else(|e| panic!("{e}"));
            let mut out = RAW.to_vec();
            out.extend(p.width().to_le_bytes());
            out.extend(p.height().to_le_bytes());
            out.extend(p.data());
            done(Ok(out));
        });
        let back: ImageDecoder = Box::new(|b: Vec<u8>, done: Decoded| done(Err(b)));
        for decoder in [&raw, &back] {
            let got = Arc::new(Mutex::new(None));
            let slot = got.clone();
            decode_with(Ok(png.clone()), Want::Image, Some(decoder), move |p| *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(p));
            let got = got.lock().unwrap_or_else(|e| e.into_inner()).take();
            let Some(Ok(Payload::Image(p))) = got else { panic!("no picture") };
            assert_eq!((p.width(), p.height()), (3, 2));
            assert_eq!(p.pixel(1, 1).map(|c| (c.red(), c.green(), c.blue(), c.alpha())), Some((200, 100, 50, 255)));
        }
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        // Not a picture, or not wanted as one: decoded here.
        let json = br#"{"a": 1}"#.to_vec();
        decode_with(Ok(json), Want::Json, Some(&raw), |p| assert!(matches!(p, Ok(Payload::Json(_)))));
        decode_with(Ok(b"junk".to_vec()), Want::Image, Some(&raw), |p| assert!(p.is_err()));
        // Larger than a tile may be, by its header: refused here, never handed to the decoder.
        let big = Pixmap::new(4097, 1).unwrap_or_else(|| panic!("pixmap")).encode_png().unwrap_or_else(|e| panic!("{e}"));
        assert!(!encoded_picture(&big));
        decode_with(Ok(big), Want::Image, Some(&raw), |p| assert!(p.is_err()));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn decodes_pictures_and_documents() {
        let mut pm = Pixmap::new(2, 2).unwrap_or_else(|| panic!("pixmap"));
        pm.fill(tiny_skia::Color::from_rgba8(200, 100, 50, 128));
        let png = pm.encode_png().unwrap_or_default();
        let Payload::Image(back) = decode(&png, Want::Image).unwrap_or(Payload::Empty) else { panic!("not an image") };
        assert_eq!((back.width(), back.height()), (2, 2));
        assert!(matches!(decode(b"{\"a\": 1}", Want::Json), Ok(Payload::Json(_))));
        assert!(matches!(decode(b"", Want::Image), Ok(Payload::Empty)));
        assert!(decode(b"not a picture", Want::Image).is_err());
        assert!(decode(b"{", Want::Json).is_err());
    }

    /// A server on this computer answering each request with the next of `answers`.
    #[cfg(not(target_arch = "wasm32"))]
    fn serve(answers: Vec<String>) -> String {
        serve_bytes(answers.into_iter().map(String::into_bytes).collect())
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn serve_bytes(answers: Vec<Vec<u8>>) -> String {
        serve_recorded(answers).0
    }

    /// Reads a whole request from `stream`, body too: answering and closing while a POST's body
    /// is still arriving resets the connection, and the client sees that instead of the answer.
    #[cfg(not(target_arch = "wasm32"))]
    fn read_request(stream: &mut std::net::TcpStream) -> String {
        use std::io::Read;
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
        let mut request = Vec::new();
        let mut buf = [0u8; 2048];
        while let Ok(n) = stream.read(&mut buf) {
            if n == 0 {
                break;
            }
            request.extend_from_slice(&buf[..n]);
            let Some(head_end) = request.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
            let head = String::from_utf8_lossy(&request[..head_end]).to_ascii_lowercase();
            let length = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if request.len() >= head_end + 4 + length {
                break;
            }
        }
        String::from_utf8_lossy(&request).into_owned()
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn serve_recorded(answers: Vec<Vec<u8>>) -> (String, std::sync::mpsc::Receiver<String>) {
        use std::io::Write;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (sent, received) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for answer in answers {
                let Ok((mut stream, _)) = listener.accept() else { return };
                let _ = sent.send(read_request(&mut stream));
                let _ = stream.write_all(&answer);
            }
        });
        (format!("http://{addr}"), received)
    }

    /// Serves every connection to the returned address with what `answer` gives for its request
    /// path, sending each request to the receiver. It answers until the test binary exits, so its
    /// thread and listener live that long.
    #[cfg(not(target_arch = "wasm32"))]
    fn serve_by_path(answer: impl Fn(&str) -> Vec<u8> + Send + 'static) -> (String, std::sync::mpsc::Receiver<String>) {
        use std::io::Write;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (sent, received) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let request = read_request(&mut stream);
                let path = request.split_whitespace().nth(1).unwrap_or("").to_string();
                let _ = sent.send(request);
                let _ = stream.write_all(&answer(&path));
            }
        });
        (format!("http://{addr}"), received)
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn logins_headers_follow_same_origin_redirects() {
        let _turn = crate::cache::turn();
        use crate::logins::{self, Method};
        let (base, asked) = serve_recorded(
            [
                "HTTP/1.1 302 Found\r\nLocation: /next\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                "HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ntile",
            ]
            .into_iter()
            .map(|s| s.as_bytes().to_vec())
            .collect(),
        );
        let binding = logins::activate(&base, "same-origin", Method::Header("X-Service-Key".into()), "same-origin-secret", true).unwrap();
        let url = logins::bind(&binding, &format!("{base}/start")).unwrap();
        assert_eq!(native::get(&native::agent(), &url, &[]).unwrap(), b"tile");
        for request in asked.iter().take(2) {
            assert!(request.to_ascii_lowercase().contains("x-service-key: same-origin-secret\r\n"));
        }
        assert!(logins::deactivate(&binding));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn logins_redirects_never_pick_up_another_origins_login() {
        let _turn = crate::cache::turn();
        use crate::logins::{self, Method};
        let (other, asked) = serve_recorded(vec![b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ntile".to_vec()]);
        let base = serve(vec![format!("HTTP/1.1 302 Found\r\nLocation: {other}/tile\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")]);
        let first = logins::activate(&base, "first", Method::Header("X-Service-Key".into()), "first-secret", true).unwrap();
        let second = logins::activate(&other, "second", Method::Header("X-Service-Key".into()), "second-secret", true).unwrap();
        let url = logins::bind(&first, &format!("{base}/start")).unwrap();
        assert_eq!(native::get(&native::agent(), &url, &[]).unwrap(), b"tile");
        let request = asked.recv_timeout(std::time::Duration::from_secs(5)).unwrap().to_ascii_lowercase();
        assert!(!request.contains("x-service-key"));
        assert!(!request.contains("first-secret") && !request.contains("second-secret"));
        assert!(logins::deactivate(&first));
        assert!(logins::deactivate(&second));
    }

    /// A failed request that carried a login is recorded by its plain address and scrubbed text:
    /// neither the binding that names the login nor its secret reaches the history or its report.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn logins_diagnostics_never_record_the_binding_or_the_secret() {
        let _turn = crate::cache::turn();
        use crate::failure::Reason;
        use crate::logins::{self, Method};
        use rustgis_data::diagnostics::{Filter, History};
        let refused = b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec();
        let (base, asked) = serve_recorded(vec![refused; 5]);
        let secret = "diagnostics-login-secret";
        let binding = logins::activate(&base, "diagnostics", Method::Header("X-Service-Key".into()), secret, true).unwrap();
        let history = History::default();
        let _scope = history.enter();
        let plain = format!("{base}/data.parquet");
        let url = logins::bind(&binding, &plain).unwrap();
        let prefix = rustgis_data::login_address::PREFIX;
        assert!(url.contains(&binding) && url.contains(prefix));
        let agent = native::agent();
        // A whole read, a range read, an expected status (omitted: said once per server), a HEAD
        // and a POST: every request goes with the login, and each failure is recorded.
        native::get(&agent, &url, &[]).unwrap_err();
        native::get(&agent, &format!("{url}{RANGE}0-9"), &[]).unwrap_err();
        native::get(&agent, &url, &[503]).unwrap_err();
        native::head_with(&agent, &url).unwrap_err();
        native::post_with(&agent, &url, "{}").unwrap_err();
        for request in asked.iter().take(5) {
            assert!(request.to_ascii_lowercase().contains(&format!("x-service-key: {secret}\r\n")), "{request}");
        }
        // A server that echoes the credential, and a transport error that names the address.
        let diagnostics = RequestDiagnostics::new(&url, "GET");
        diagnostics.failed(&Failure::without_answer("", format!("the server repeated {secret}"), Reason::Other));
        let echoed = vec![("www-authenticate".to_string(), format!("Bearer realm={secret}"))];
        diagnostics.failed(&Failure::answered("", 401, &echoed, &[]));
        diagnostics.failed(&Failure::new(&url, format!("{url}: connection reset")));
        let records = history.records();
        // Five requests (the omitted status as a single absence note) and three synthetic failures.
        assert_eq!(records.len(), 8, "{records:?}");
        assert_eq!(network_records(&history, &plain).len(), 8, "every record names the plain address: {records:?}");
        let everything = format!("{records:?}\n{}", history.report(Filter::All));
        for private in [binding.as_str(), secret, prefix] {
            assert!(!everything.contains(private), "{private} reached the diagnostics: {everything}");
        }
        assert!(everything.contains(&plain));
        assert!(logins::deactivate(&binding));
    }

    /// Once any hop carried a login, a later anonymous hop must not regain response details.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn delayed_head_refusal_after_cross_origin_redirect_is_status_only() {
        let _turn = crate::cache::turn();
        use crate::logins::{self, Method};
        use rustgis_data::diagnostics::{Capture, Filter, History};
        use std::io::{Read, Write};
        use std::sync::mpsc::channel;
        use std::time::Duration;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let other = format!("http://{}", listener.local_addr().unwrap());
        let secret = "redirect-head-sentinel-757";
        let (received_tx, received_rx) = channel();
        let (release_tx, release_rx) = channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            let mut request = Vec::new();
            let mut buf = [0u8; 2048];
            while request.len() < 32768 && !request.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = stream.read(&mut buf).unwrap();
                assert_ne!(n, 0);
                request.extend_from_slice(&buf[..n]);
            }
            assert!(request.ends_with(b"\r\n\r\n"));
            let request = String::from_utf8(request).unwrap();
            assert!(request.starts_with("HEAD /refused HTTP/1.1\r\n"));
            assert!(!request.to_ascii_lowercase().contains("x-key:") && !request.contains(secret));
            received_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            write!(stream, "HTTP/1.1 403 Forbidden\r\nx-request-id: {secret}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        });
        let (base, asked) = serve_recorded(vec![
            format!("HTTP/1.1 302 Found\r\nLocation: {other}/refused\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").into_bytes(),
        ]);
        let binding = logins::activate(&base, "redirect-head", Method::Header("X-Key".into()), secret, true).unwrap();
        let foreign = logins::activate(&other, "foreign-head", Method::Header("X-Key".into()), "foreign-head-secret", true).unwrap();
        let url = logins::bind(&binding, &format!("{base}/start")).unwrap();
        let history = History::default();
        let _scope = history.enter();
        let capture = Capture::current();
        let worker = std::thread::spawn(move || {
            let _scope = capture.enter();
            native::head_with(&native::agent(), &url)
        });
        let request = asked.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(request.to_ascii_lowercase().contains(&format!("x-key: {secret}\r\n")));
        received_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(logins::deactivate(&binding));
        release_tx.send(()).unwrap();
        // Other parallel tests can register short credentials that scrub ordinary message
        // words. Check the failure status here and its structured diagnostic status below.
        assert!(worker.join().unwrap().unwrap_err().contains("403"));
        server.join().unwrap();
        let records = history.records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].network.as_ref().unwrap().status, Some(403));
        let text = format!("{records:?}\n{}", history.report(Filter::All));
        assert!(text.contains("403"));
        assert!(!text.contains(secret) && !text.contains("x-request-id") && !text.contains("foreign-head-secret"));
        assert!(logins::deactivate(&foreign));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn logins_head_and_post_carry_the_login() {
        let _turn = crate::cache::turn();
        use crate::logins::{self, Method};
        let answer = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec();
        let (base, asked) = serve_recorded(vec![answer.clone(), answer]);
        let binding = logins::activate(&base, "head-post", Method::Header("X-Service-Key".into()), "head/post secret", true).unwrap();
        let url = logins::bind(&binding, &format!("{base}/data?key=a%2Fb")).unwrap();
        let agent = native::agent();
        native::head_with(&agent, &url).unwrap();
        assert!(native::post_with(&agent, &url, "{}").unwrap().is_empty());
        for (method, request) in ["HEAD", "POST"].into_iter().zip(asked.iter().take(2)) {
            assert!(request.starts_with(&format!("{method} /data?key=a%2Fb HTTP/1.1\r\n")), "{request}");
            assert!(request.to_ascii_lowercase().contains("x-service-key: head/post secret\r\n"));
        }
        assert!(logins::deactivate(&binding));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn live_connection_bindings_separate_requests_and_cache_entries() {
        #[cfg(not(target_arch = "wasm32"))]
        if isolate_cache_fixture("cache::tests::live_connection_bindings_separate_requests_and_cache_entries") {
            return;
        }
        let _turn = crate::cache::turn();
        use crate::logins::{self, Method};
        let answer = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}".to_vec();
        let (base, asked) = serve_recorded(vec![answer.clone(), answer.clone(), answer]);
        let raw = format!("{base}/same");
        let a = logins::activate(&base, "session-a/connection-a", Method::Header("X-Key".into()), "a", true).unwrap();
        let b = logins::activate(&base, "session-b/connection-b", Method::Header("X-Key".into()), "b", true).unwrap();
        let ua = logins::bind(&a, &raw).unwrap();
        let ub = logins::bind(&b, &raw).unwrap();
        for url in [&ua, &ub, &raw] {
            native::get(&native::agent(), url, &[]).unwrap();
        }
        let requests: Vec<_> = asked.iter().take(3).map(|r| r.to_ascii_lowercase()).collect();
        assert!(requests[0].contains("x-key: a\r\n"));
        assert!(requests[1].contains("x-key: b\r\n"));
        assert!(!requests[2].contains("x-key:"));
        store(&ua, Entry::Ready(Arc::new(Payload::Json(serde_json::json!({"a": 1}))), 64));
        assert!(peek(&ua).is_some());
        assert!(peek(&ub).is_none() && peek(&raw).is_none());
        assert!(logins::deactivate(&a));
        assert!(peek(&ua).is_none());
        assert!(matches!(get(&ua, Want::Json), State::Failed(_)));
        let replacement = logins::activate(&base, "session-a/connection-a", Method::Header("X-Key".into()), "new", true).unwrap();
        assert!(logins::apply(&ua).is_err());
        assert!(logins::apply(&ub).unwrap().is_some());
        assert!(logins::deactivate(&replacement));
        assert!(logins::deactivate(&b));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn logout_during_pending_download_discards_old_authenticated_body() {
        let _turn = crate::cache::turn();
        use crate::logins::{self, Method};
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let binding =
            logins::activate(&base, "pending-session/connection", Method::Header("X-Key".into()), "pending-secret", true).unwrap();
        let url = logins::bind(&binding, &format!("{base}/pending")).unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 2048];
            let n = stream.read(&mut buf).unwrap();
            assert!(String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase().contains("x-key: pending-secret"));
            started_tx.send(()).unwrap();
            release_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nold-body").unwrap();
        });
        let worker_url = url.clone();
        let worker = std::thread::spawn(move || native::get(&native::agent(), &worker_url, &[]));
        started_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert!(logins::deactivate(&binding));
        let replacement =
            logins::activate(&base, "pending-session/connection", Method::Header("X-Key".into()), "replacement-secret", true).unwrap();
        let replacement_url = logins::bind(&replacement, &format!("{base}/pending")).unwrap();
        assert_ne!(url, replacement_url);
        release_tx.send(()).unwrap();
        assert!(worker.join().unwrap().is_err());
        store(&url, Entry::Ready(Arc::new(Payload::Json(serde_json::json!({"old": true}))), 64));
        assert!(peek(&url).is_none());
        assert!(matches!(get(&url, Want::Json), State::Failed(_)));
        assert!(peek(&replacement_url).is_none());
        assert!(logins::apply(&replacement_url).unwrap().is_some());
        assert!(logins::deactivate(&replacement));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn derived_style_tilejson_sprite_and_feature_requests_carry_the_binding() {
        #[cfg(not(target_arch = "wasm32"))]
        if isolate_cache_fixture("cache::tests::derived_style_tilejson_sprite_and_feature_requests_carry_the_binding") {
            return;
        }
        let _turn = crate::cache::turn();
        use crate::{
            logins::{self, Method},
            source,
            style::Source,
            template,
        };
        let style = serde_json::json!({
            "version": 8, "sprite": "./sprite",
            "sources": {"roads": {"type": "vector", "url": "./tiles.json"}}, "layers": []
        });
        let tilejson = serde_json::json!({"tiles": ["./tile/{z}/{x}/{y}.pbf"]});
        let answer =
            |body: String| format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).into_bytes();
        // Answered by path, for as many connections as come: a test running at once may clear the
        // shared cache, and resolving then fetches the style again in the background, so the
        // requests are neither five nor in a fixed order.
        let (base, asked) = serve_by_path(move |path| {
            if path.starts_with("/style.json") {
                answer(style.to_string())
            } else if path.starts_with("/tiles.json") {
                answer(tilejson.to_string())
            } else {
                answer("{}".into())
            }
        });
        let binding =
            logins::activate(&base, "derived-style", Method::Header("X-Service-Key".into()), "derived-style-secret", true).unwrap();
        let url = logins::bind(&binding, &format!("{base}/style.json")).unwrap();
        let tilejson_url = template::join(&url, "./tiles.json");
        // Seed asynchronous style resolution with the two documents already fetched.
        let mut documents = Vec::new();
        for address in [&url, &tilejson_url] {
            let bytes = native::get(&native::agent(), address, &[]).unwrap();
            documents.push((address.clone(), serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(), bytes.len()));
        }
        let spec = source::Spec {
            kind: source::Kind::Style,
            url: url.clone(),
            subdomains: vec![],
            min_zoom: 0,
            max_zoom: 22,
            tile_size: 256,
            tms: false,
            attribution: String::new(),
            layers: vec![],
            style_layers: String::new(),
            hidden_layers: vec![],
        };
        let mut resolved = None;
        for _ in 0..200 {
            for (address, json, len) in &documents {
                store(address, Entry::Ready(Arc::new(Payload::Json(json.clone())), *len));
            }
            if let source::Resolve::Ready(r) = source::resolve(&spec) {
                resolved = Some(r);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let resolved = resolved.expect("style must resolve from fetched documents");
        let Source::Vector { tiles, .. } = &resolved.style.sources[0].1 else { panic!("vector source") };
        let tile = template::expand(&tiles[0], template::TileId { z: 0, x: 0, y: 0 }, 256, false, &[]);
        native::get(&native::agent(), &tile, &[]).unwrap();
        assert_eq!(resolved.sprites.len(), 1);
        let sprite = &resolved.sprites[0].1;
        native::get(&native::agent(), &format!("{sprite}.json"), &[]).unwrap();
        let info = rustgis_data::ogc::FeatureInfo {
            url: template::join(&url, "./info"),
            layer: "roads".into(),
            version: "1.3.0".into(),
            formats: ["application/json".to_string()].into(),
            ..Default::default()
        };
        native::get(&native::agent(), &info.request(0.0, 0.0, 10.0).unwrap(), &[]).unwrap();
        // Every request sent so far, background fetches included. Each of the test's own requests
        // was sent on before its answer came back, so all of them are among these.
        let requests: Vec<String> = asked.try_iter().collect();
        assert!(requests.len() >= 5);
        for request in &requests {
            assert!(request.to_ascii_lowercase().contains("x-service-key: derived-style-secret\r\n"));
            assert!(!request.contains("rustgis-login:"));
        }
        assert!(logins::deactivate(&binding));
        assert!(matches!(source::resolve(&spec), source::Resolve::Failed(_)));
        assert!(matches!(crate::sprite::load(&resolved.sprites, false), crate::sprite::Load::Failed));
        assert!(crate::pmtiles::info(&url).is_err());
        assert!(crate::cog::info(&url).is_err());
    }

    /// The last bytes from a store that serves ranges but answers `bytes=-n` with 501 (GitHub's
    /// release files, on Azure storage): the file's length from a one-byte range, then the last
    /// bytes by their place.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_last_bytes_come_from_a_store_without_suffix_ranges() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let asked = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen = asked.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().take(3) {
                let Ok(mut stream) = stream else { return };
                let mut buf = [0u8; 2048];
                let n = stream.read(&mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
                let range = head.lines().find_map(|l| l.strip_prefix("range: ")).unwrap_or("").trim().to_string();
                seen.lock().unwrap().push(range.clone());
                let answer = match range.as_str() {
                    "bytes=-4" => "HTTP/1.1 501 Not Implemented\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
                    "bytes=0-0" => {
                        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-0/10\r\nContent-Length: 1\r\nConnection: close\r\n\r\n0"
                            .to_string()
                    }
                    "bytes=6-9" => {
                        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 6-9/10\r\nContent-Length: 4\r\nConnection: close\r\n\r\n6789"
                            .to_string()
                    }
                    _ => "HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
                };
                let _ = stream.write_all(answer.as_bytes());
            }
        });
        let agent = native::agent();
        let got = native::get_detailed(&agent, &format!("http://{addr}/f.parquet{TAIL}4")).unwrap();
        assert_eq!(got, b"6789");
        assert_eq!(*asked.lock().unwrap(), ["bytes=-4", "bytes=0-0", "bytes=6-9"]);
    }

    /// The one-byte range asked for a file's length: redirected, it is asked for again where it
    /// leads; refused, the refusal's status is kept; answered whole, the server has no ranges.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_length_asked_for_follows_redirects_and_keeps_refusals() {
        let _turn = turn();
        let no_suffix = "HTTP/1.1 501 Not Implemented\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string();
        let agent = native::agent();
        let base = serve(vec![
            no_suffix.clone(),
            "HTTP/1.1 302 Found\r\nLocation: /g.parquet\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-0/10\r\nContent-Length: 1\r\nConnection: close\r\n\r\n0".into(),
            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 6-9/10\r\nContent-Length: 4\r\nConnection: close\r\n\r\n6789".into(),
        ]);
        assert_eq!(native::get_detailed(&agent, &format!("{base}/f.parquet{TAIL}4")).unwrap(), b"6789");
        let base = serve(vec![no_suffix.clone(), "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into()]);
        let f = native::get_detailed(&agent, &format!("{base}/f.parquet{TAIL}4")).unwrap_err();
        assert_eq!(f.status(), Some(404), "{}", f.message);
        let base = serve(vec![no_suffix, "HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\n0123456789".into()]);
        let f = native::get_detailed(&agent, &format!("{base}/f.parquet{TAIL}4")).unwrap_err();
        assert!(f.message.contains(super::NO_RANGES), "{}", f.message);
        // More last bytes asked for than the file holds: the whole file.
        let base = serve(vec![
            "HTTP/1.1 501 Not Implemented\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-0/3\r\nContent-Length: 1\r\nConnection: close\r\n\r\na".into(),
            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-2/3\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc".into(),
        ]);
        assert_eq!(native::get_detailed(&agent, &format!("{base}/f.parquet{TAIL}4")).unwrap(), b"abc");
        // The length as a Content-Range gives it, when it does.
        assert_eq!(native::content_range_total("bytes 0-0/1234"), Some(1234));
        assert_eq!(native::content_range_total("bytes 0-0/ 10 "), Some(10));
        for unknown in ["bytes 0-0/*", "bytes 0-0/0", "bytes 0-0", "bytes 0-0/-5", ""] {
            assert_eq!(native::content_range_total(unknown), None, "{unknown}");
        }
    }

    /// A refused request keeps who refused it (#509); the plain error reads as it always has.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_refused_download_says_who_refused_it() {
        let _turn = turn();
        let page = "<html><head><title>Attention Required! | Cloudflare</title></head><body>Sorry, you have been blocked</body></html>";
        let blocked = format!(
            "HTTP/1.1 403 Forbidden\r\nServer: cloudflare\r\nCF-RAY: a4c9-BOG\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
            page.len()
        );
        let agent = native::agent();
        let base = serve(vec![blocked.clone(), blocked]);
        let f = native::get_detailed(&agent, &format!("{base}/MapServer?f=json")).unwrap_err();
        assert_eq!((f.status(), f.firewall(), f.excerpt()), (Some(403), Some("Cloudflare"), Some("Attention Required! | Cloudflare")));
        assert_eq!((f.url.as_str(), f.request_id()), (format!("{base}/MapServer?f=json").as_str(), Some("cf-ray a4c9-BOG".to_string())));
        assert!(f.user_agent().is_some_and(|u| u.starts_with("RustGIS/")));
        assert_eq!(native::get(&agent, &format!("{base}/x"), &[]).unwrap_err(), "http status: 403");
        // No server there: unreachable, not an answer.
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        let f = native::get_detailed(&agent, &format!("http://{closed}/x")).unwrap_err();
        assert_eq!(f.reason, crate::failure::Reason::Unreachable, "{}", f.message);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn follows_only_allowed_redirects() {
        let _turn = turn();
        let redirect = |to: &str| format!("HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        let ok = "HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ntile".to_string();
        let agent = native::agent();
        // Within the same server: followed.
        let base = serve(vec![redirect("/tile.png"), ok]);
        assert_eq!(native::get(&agent, &format!("{base}/old.png"), &[]).unwrap(), b"tile");
        // To a metadata address, or from HTTPS down to HTTP: refused.
        let base = serve(vec![redirect("http://169.254.169.254/latest/meta-data/")]);
        assert!(native::get(&agent, &format!("{base}/x"), &[]).unwrap_err().contains("never fetched"));
        // Too many.
        let base = serve((0..7).map(|_| redirect("/again")).collect());
        assert!(native::get(&agent, &format!("{base}/x"), &[]).unwrap_err().contains("too many"));
    }

    /// An agent that fetches `https://` addresses over plain TCP, as if TLS were there, so a test
    /// server on this computer can stand for an HTTPS server.
    #[cfg(not(target_arch = "wasm32"))]
    fn plain_tls_agent() -> ureq::Agent {
        use ureq::unversioned::transport::{Buffers, ConnectionDetails, Connector, NextTimeout, TcpConnector, Transport};
        #[derive(Debug)]
        struct ClaimsTls;
        #[derive(Debug)]
        struct Claimed<T>(T);
        impl<In: Transport> Connector<In> for ClaimsTls {
            type Out = Claimed<In>;
            fn connect(&self, _: &ConnectionDetails, chained: Option<In>) -> Result<Option<Self::Out>, ureq::Error> {
                Ok(chained.map(Claimed))
            }
        }
        impl<T: Transport> Transport for Claimed<T> {
            fn buffers(&mut self) -> &mut dyn Buffers {
                self.0.buffers()
            }
            fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
                self.0.transmit_output(amount, timeout)
            }
            fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
                self.0.await_input(timeout)
            }
            fn is_open(&mut self) -> bool {
                self.0.is_open()
            }
            fn is_tls(&self) -> bool {
                true
            }
        }
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_global(Some(std::time::Duration::from_secs(10)))
            .build();
        ureq::Agent::with_parts(config, TcpConnector::default().chain(ClaimsTls), ureq::unversioned::resolver::DefaultResolver::default())
    }

    /// Never from HTTPS down to plain HTTP, however the scheme is written: the HTTP client reads
    /// `HTTPS://` as `https://`, so the check must too, for GET and HEAD alike.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_redirect_never_goes_from_https_to_http_in_any_case() {
        let _turn = turn();
        let redirect = |to: &str| format!("HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        let ok = "HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ntile".to_string();
        let agent = plain_tls_agent();
        for (from, to) in [("HTTPS", "http"), ("https", "HTTP"), ("HttpS", "Http"), ("https", "http")] {
            // Where the redirect leads, recording whether anything was asked of it.
            let (plain, asked) = serve_recorded(vec![ok.clone().into_bytes(), ok.clone().into_bytes()]);
            let plain = plain.replacen("http", to, 1);
            for head in [false, true] {
                let secure = serve(vec![redirect(&format!("{plain}/leak"))]).replacen("http", from, 1);
                let url = format!("{secure}/x");
                let err = if head { native::head_with(&agent, &url).map(|_| ()) } else { native::get(&agent, &url, &[]).map(|_| ()) }
                    .unwrap_err();
                assert!(err.contains("a redirect from https to http"), "{from} → {to}, head {head}: {err}");
            }
            assert!(asked.recv_timeout(std::time::Duration::from_millis(200)).is_err(), "{from} → {to}: the http server was asked");
        }
        // HTTPS to HTTPS, in whatever case, is followed.
        for (from, to) in [("HTTPS", "https"), ("https", "HTTPS"), ("https", "https")] {
            let next = serve(vec![ok.clone()]).replacen("http", to, 1);
            let base = serve(vec![redirect(&format!("{next}/tile.png"))]).replacen("http", from, 1);
            assert_eq!(native::get(&agent, &format!("{base}/old.png"), &[]).unwrap(), b"tile", "{from} → {to}");
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn fetches_are_limited_per_server() {
        assert_eq!(native::host("https://Tile.Example.org/1/2/3.png"), "tile.example.org");
        assert_eq!(native::host("http://127.0.0.1:8080/a?b#c"), "127.0.0.1:8080");
        assert_eq!(native::host("https://user:pw@example.org"), "example.org");
        assert_eq!(native::host(&format!("{DOWNLOAD}https://example.org/x")), "example.org");
        assert_eq!(native::host("https://example.org/a.pmtiles#rustgis-range=0-9"), "example.org");
        assert_eq!(native::host("mbtiles:/data/a.mbtiles/1/2/3"), "");
        assert_eq!(native::host("not an address"), "");
        // OpenStreetMap's servers get fewer than other servers; local reads aren't limited.
        assert!(native::cap("tile.openstreetmap.org") < native::cap("example.org"));
        assert_eq!(native::cap("a.tile.openstreetmap.org"), native::cap("tile.openstreetmap.org"));
        assert!(native::cap("") >= native::cap("example.org"));
        // The newest job whose server has room.
        let full = native::cap("a.org");
        let active: HashMap<String, usize> = [("a.org".to_string(), full)].into();
        assert_eq!(native::pick(&["b.org", "a.org", "b.org", "a.org"], &active), Some(2));
        assert_eq!(native::pick(&["a.org", "a.org"], &active), None);
        assert_eq!(native::pick(&["a.org"], &HashMap::new()), Some(0));
    }

    /// Many tiles from one server are fetched more than 8 at a time (the old pool), and never
    /// more than the server's cap.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn fetches_from_one_server_run_side_by_side() {
        use std::io::{Read, Write};
        use std::sync::mpsc;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let open = Arc::new(AtomicUsize::new(0));
        let most = Arc::new(AtomicUsize::new(0));
        let (o, m) = (open.clone(), most.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let (o, m) = (o.clone(), m.clone());
                std::thread::spawn(move || {
                    let now = o.fetch_add(1, Ordering::SeqCst) + 1;
                    m.fetch_max(now, Ordering::SeqCst);
                    let mut buf = [0u8; 2048];
                    let _ = stream.read(&mut buf);
                    std::thread::sleep(std::time::Duration::from_millis(150));
                    o.fetch_sub(1, Ordering::SeqCst);
                    let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ntile");
                });
            }
        });
        let (tx, rx) = mpsc::channel();
        let n = 40;
        for i in 0..n {
            let tx = tx.clone();
            native::fetch(format!("http://{addr}/{i}.png"), Box::new(move |r| drop(tx.send(r))), &[]);
        }
        for _ in 0..n {
            assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(30)).unwrap().unwrap(), b"tile");
        }
        let most = most.load(Ordering::SeqCst);
        assert!(most > 8, "only {most} at a time");
        assert!(most <= native::cap(&addr.to_string()), "{most} at a time");
    }

    /// Successful bytes and browser decodes cannot populate a shared cache after their owner
    /// retires, including a decode held across reload; active owners still populate it.
    #[test]
    fn retired_successes_do_not_populate_the_tile_cache() {
        let _turn = turn();
        let held = Arc::new(Mutex::new(Vec::<Done>::new()));
        let hold = held.clone();
        set_fetcher(move |_, done| hold.lock().unwrap().push(done));
        let decoded = Arc::new(Mutex::new(Vec::<(Vec<u8>, Decoded)>::new()));
        let hold = decoded.clone();
        let old = image_decoder().lock().unwrap().clone();
        struct RestoreDecoder(Option<Arc<ImageDecoder>>);
        impl Drop for RestoreDecoder {
            fn drop(&mut self) {
                *image_decoder().lock().unwrap() = self.0.take();
            }
        }
        let _restore = RestoreDecoder(old);
        set_image_decoder(move |bytes, done| hold.lock().unwrap().push((bytes, done)));
        let png = Pixmap::new(1, 1).unwrap().encode_png().unwrap();
        let mut failures = Vec::new();
        for want in [Want::Image, Want::Json, Want::Vector] {
            for revoke in [false, true] {
                let url = format!("https://retired-success.invalid/{want:?}-{revoke}");
                let live = Arc::new(std::sync::atomic::AtomicBool::new(true));
                let flag = live.clone();
                assert!(matches!(with_still(Arc::new(move || flag.load(Ordering::SeqCst)), || get(&url, want)), State::Pending));
                let done = held.lock().unwrap().pop().unwrap();
                if want == Want::Image {
                    done(Ok(png.clone()));
                    assert_eq!(decoded.lock().unwrap().len(), 1, "real cache entry waits for browser decoding");
                    clear();
                    live.store(!revoke, Ordering::SeqCst);
                    let (bytes, done) = decoded.lock().unwrap().pop().unwrap();
                    done(Err(bytes));
                } else {
                    live.store(!revoke, Ordering::SeqCst);
                    done(Ok(if want == Want::Json { b"1".to_vec() } else { Vec::new() }));
                }
                if peek(&url).is_some() == revoke {
                    failures.push(format!("{want:?}: cached success with revoke={revoke}"));
                    if revoke {
                        forget(&url);
                    }
                }
                if revoke {
                    assert!(matches!(get(&url, want), State::Pending));
                    // A fresh live caller can finish immediately, with no inherited failure.
                    let done = held.lock().unwrap().pop().unwrap();
                    done(Ok(Vec::new()));
                    assert!(matches!(get(&url, want), State::Ready(_)));
                }
            }
        }
        assert!(failures.is_empty(), "{failures:?}");
    }

    /// Revocation belongs to the caller, while a genuine server failure belongs to the resource.
    /// Header caches must let a later caller retry immediately under its own lifetime.
    #[test]
    fn revoked_archive_headers_do_not_delay_live_callers() {
        let _turn = crate::cache::turn();
        let _alarm = crate::cache::alarm_turn();
        let held = Arc::new(Mutex::new(Vec::<Done>::new()));
        let hold = held.clone();
        set_fetcher(move |_, done| hold.lock().unwrap().push(done));
        let mut failures = Vec::new();
        for cog in [false, true] {
            let url = if cog { "https://revoked-header.invalid/retry.tif" } else { "https://revoked-header.invalid/retry.pmtiles" };
            let ask = || if cog { crate::cog::info(url).map(|i| i.is_some()) } else { crate::pmtiles::info(url).map(|i| i.is_some()) };
            let live = Arc::new(std::sync::atomic::AtomicBool::new(true));
            let flag = live.clone();
            let still: Still = Arc::new(move || flag.load(Ordering::SeqCst));
            assert_eq!(with_still(still, ask), Ok(false));
            // A distinct live tile joins the scoped header read. Its resource must not keep
            // the loader's cancellation, even though its own initiating lifetime is current.
            let tile = format!("{}:0/0/0@{url}", if cog { "cog" } else { "pmtiles" });
            assert!(matches!(get(&tile, Want::Image), State::Pending));
            live.store(false, Ordering::SeqCst);
            held.lock().unwrap().pop().unwrap()(Err("HTTP 503".into()));
            if lock().slots.contains_key(&tile) {
                failures.push(format!("{tile}: a joining live tile cached the retired loader's failure"));
            }
            let retry = ask();
            let tile_retry = get(&tile, Want::Image);
            if !matches!(tile_retry, State::Pending) {
                failures.push(format!("{tile}: the next live draw could not retry immediately"));
            }
            let next = held.lock().unwrap().pop();
            if retry != Ok(false) || next.is_none() {
                failures.push(format!("{url}: a revoked owner's error delayed a live caller: {retry:?}"));
            }
            if let Some(done) = next {
                done(Err("HTTP 503".into()));
                assert!(ask().is_err(), "a live owner's server failure keeps its retry delay");
                assert!(matches!(get(&tile, Want::Image), State::Failed(_)), "live tile failures still keep the server's retry delay");
                assert!(held.lock().unwrap().is_empty());
            }
        }
        assert!(failures.is_empty(), "{failures:?}");
    }

    /// A shared tile slot and its server queue retain their owners, release retired reads,
    /// and let live callers retry without inheriting another request's cancellation.
    #[test]
    fn fetches_through_the_fetcher_once() {
        #[cfg(not(target_arch = "wasm32"))]
        if isolate_cache_fixture("cache::tests::fetches_through_the_fetcher_once") {
            return;
        }
        let _turn = crate::cache::turn();
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        set_fetcher(|url, done| {
            CALLS.fetch_add(1, Ordering::Relaxed);
            if url.ends_with("ok.json") { done(Ok(b"[1, 2]".to_vec())) } else { done(Err("404".into())) }
        });
        let g = generation();
        assert!(matches!(get("test://cache/ok.json", Want::Json), State::Pending));
        assert!(generation() > g);
        assert!(matches!(get("test://cache/ok.json", Want::Json), State::Ready(_)));
        assert!(peek("test://cache/ok.json").is_some());
        assert!(matches!(get("test://cache/missing.json", Want::Json), State::Pending));
        assert!(matches!(get("test://cache/missing.json", Want::Json), State::Failed(_)));
        assert!(peek("test://cache/never.json").is_none());
        // A request the fetcher dropped is forgotten, not failed: asked for again, it is fetched.
        set_fetcher(|url, done| {
            CALLS.fetch_add(1, Ordering::Relaxed);
            if url.ends_with("first.json") { done(Err(DROPPED.into())) } else { done(Ok(b"1".to_vec())) }
        });
        assert!(matches!(get("test://cache/first.json", Want::Json), State::Pending));
        assert!(matches!(get("test://cache/first.json", Want::Json), State::Pending), "fetched again");
        assert_eq!(CALLS.load(Ordering::Relaxed), 4);

        // A server has a few fetches under way at once; the newest waiting tile goes next (#635).
        static HELD: Mutex<Vec<(String, Done)>> = Mutex::new(Vec::new());
        set_fetcher(|url, done| {
            if url.starts_with("test://held/") {
                HELD.lock().unwrap().push((url, done));
            } else {
                done(Ok(b"1".to_vec()));
            }
        });
        let n = crate::schedule::PER_HOST + 3;
        for i in 0..n {
            assert!(matches!(get(&format!("test://held/{i}.json"), Want::Json), State::Pending));
        }
        assert_eq!(HELD.lock().unwrap().len(), crate::schedule::PER_HOST);
        let (_, done) = HELD.lock().unwrap().remove(0);
        done(Ok(b"2".to_vec()));
        let held: Vec<String> = HELD.lock().unwrap().iter().map(|(u, _)| u.clone()).collect();
        assert_eq!(held.len(), crate::schedule::PER_HOST);
        assert_eq!(held.last().map(String::as_str), Some(format!("test://held/{}.json", n - 1).as_str()));
        assert!(matches!(get("test://held/0.json", Want::Json), State::Ready(_)));
        // The rest finish, and so do the waiting ones they make room for.
        loop {
            let Some((_, done)) = HELD.lock().unwrap().pop() else { break };
            done(Ok(b"3".to_vec()));
        }
        for i in 0..n {
            assert!(matches!(get(&format!("test://held/{i}.json"), Want::Json), State::Ready(_)), "{i}");
        }

        // Waiting at the server cap must retain the initiating request's revocation and
        // diagnostics owner, even when another owner's completion starts the fetch.
        let seen = Arc::new(Mutex::new(Vec::new()));
        let fetched = seen.clone();
        set_fetcher(move |url, done| {
            if url.starts_with("test://held/") {
                fetched.lock().unwrap().push(url.clone());
                if url == "test://held/context/diagnostics.json" {
                    let mut diagnostics = RequestDiagnostics::new(&url, "GET");
                    diagnostics.answered(503);
                    diagnostics.failed(&Failure::new(&url, "http status: 503"));
                    done(Err("http status: 503".into()));
                } else {
                    HELD.lock().unwrap().push((url, done));
                }
            } else {
                done(Ok(b"1".to_vec()));
            }
        });
        let drain_held = || loop {
            let Some((_, done)) = HELD.lock().unwrap().pop() else { break };
            done(Ok(b"3".to_vec()));
        };
        let owner = rustgis_data::diagnostics::History::default();
        let launcher = rustgis_data::diagnostics::History::default();
        let live = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let flag = live.clone();
        let still: Still = Arc::new(move || flag.load(Ordering::SeqCst));
        for i in 0..crate::schedule::PER_HOST {
            let _ = get(&format!("test://held/context/filler-{i}.json"), Want::Json);
        }
        let revoked = "test://held/context/revoked.json";
        let diagnostic = "test://held/context/diagnostics.json";
        let healthy = "test://held/context/healthy.json";
        let admitted = {
            let _scope = owner.enter();
            [with_still(still, || get(revoked, Want::Json)), get(diagnostic, Want::Json)]
        };
        let control = get(healthy, Want::Json);
        let held_before = HELD.lock().unwrap().len();
        live.store(false, Ordering::SeqCst);
        {
            let _scope = launcher.enter();
            drain_held();
        }
        // Drain before asserting: the RED result must not strand the other queued requests.
        assert_eq!(held_before, crate::schedule::PER_HOST);
        assert!(admitted.iter().all(|state| matches!(state, State::Pending)) && matches!(control, State::Pending));
        assert!(!seen.lock().unwrap().iter().any(|url| url == revoked), "a revoked waiting request reached the fetcher");
        assert!(!lock().slots.contains_key(revoked), "a revoked owner must not leave a shared tile failure");
        assert!(matches!(get(revoked, Want::Json), State::Pending), "a live caller starts its own fetch immediately");
        drain_held();
        assert!(matches!(get(revoked, Want::Json), State::Ready(_)));
        assert!(peek(healthy).is_some(), "the unscoped waiting control still completes");
        assert_eq!(network_records(&owner, diagnostic).len(), 1);
        assert!(launcher.records().is_empty(), "a queued request keeps its original diagnostics owner");
        assert!(current_still().is_none());

        // Restoring an initiator must also preserve the launcher's existing revocation floor.
        for i in 0..crate::schedule::PER_HOST {
            let _ = get(&format!("test://held/context-floor/filler-{i}.json"), Want::Json);
        }
        let floor = "test://held/context-floor/queued.json";
        let admitted = with_still(Arc::new(|| true), || get(floor, Want::Json));
        with_still(Arc::new(|| false), drain_held);
        assert!(matches!(admitted, State::Pending));
        assert!(!seen.lock().unwrap().iter().any(|url| url == floor));
        assert!(!lock().slots.contains_key(floor), "the launcher's revocation must not leave a shared tile failure");
        assert!(matches!(get(floor, Want::Json), State::Pending));
        drain_held();
        assert!(matches!(get(floor, Want::Json), State::Ready(_)));
        assert!(current_still().is_none());

        // A fetcher that drops a request unanswered (or panics) gives its server's place back:
        // more requests than the cap still all reach it, and each is forgotten, not stuck.
        static SEEN: AtomicUsize = AtomicUsize::new(0);
        set_fetcher(|url, done| {
            if url.starts_with("test://lost/") {
                SEEN.fetch_add(1, Ordering::Relaxed);
                drop(done);
            } else {
                done(Ok(b"1".to_vec()));
            }
        });
        for i in 0..3 * crate::schedule::PER_HOST {
            assert!(matches!(get(&format!("test://lost/{i}.json"), Want::Json), State::Pending));
        }
        assert_eq!(SEEN.load(Ordering::Relaxed), 3 * crate::schedule::PER_HOST);
        assert!(peek("test://lost/0.json").is_none());

        // Waiting for news: a fetch never answered (another's, a slow server's) doesn't hold up
        // one waiting for what has come.
        static STUCK: Mutex<Vec<Done>> = Mutex::new(Vec::new());
        set_fetcher(|url, done| {
            if url.starts_with("test://stuck/") {
                STUCK.lock().unwrap().push(done);
            } else {
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    done(Ok(b"1".to_vec()));
                });
            }
        });
        assert!(matches!(get("test://stuck/0.json", Want::Json), State::Pending));
        let g = generation();
        assert!(matches!(get("test://news/0.json", Want::Json), State::Pending));
        let t = Instant::now();
        // (Another test's fetch landing may end a wait first.)
        while !matches!(get("test://news/0.json", Want::Json), State::Ready(_)) && t.elapsed() < Duration::from_secs(20) {
            wait_for_news(g, Duration::from_secs(20));
        }
        assert!(t.elapsed() < Duration::from_secs(5), "waited {:?} for the stuck fetch", t.elapsed());
        for done in std::mem::take(&mut *STUCK.lock().unwrap()) {
            done(Ok(b"1".to_vec()));
        }

        // Only a map tile's 404 is an expected answer: a picture that isn't a tile (a single
        // image, a layout's picture, a sprite sheet) and a style keep theirs.
        static OMITS: Mutex<Vec<(String, &'static [u16])>> = Mutex::new(Vec::new());
        set_fetcher_omitting(|url, done, omit| {
            OMITS.lock().unwrap().push((url, omit));
            done(Err("http status: 404".into()));
        });
        let _ = get_tile("test://omit/tile.png", Want::Image);
        let _ = get_tile("test://omit/tile.pbf", Want::Vector);
        let _ = get("test://omit/picture.png", Want::Image);
        let _ = get("test://omit/style.json", Want::Json);
        let omits = std::mem::take(&mut *OMITS.lock().unwrap());
        let omit = |name: &str| omits.iter().find(|(u, _)| u.ends_with(name)).map(|(_, o)| *o);
        assert_eq!(omit("tile.png"), Some(&[404, 410][..]));
        assert_eq!(omit("tile.pbf"), Some(&[404, 410][..]));
        assert_eq!(omit("picture.png"), Some(&[][..]));
        assert_eq!(omit("style.json"), Some(&[][..]));
    }
}
