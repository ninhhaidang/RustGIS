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

use std::cell::{Cell, RefCell};
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

/// Bytes and the actual effective origin observed by a replacement transport.
#[derive(Debug)]
pub struct Fetched {
    pub bytes: Vec<u8>,
    pub origin: crate::address::Origin,
}
/// A policy-aware fetch completes once with bytes and their final provenance, or an error.
pub type FetchedDone = Box<dyn FnOnce(Result<Fetched, String>) + Send>;
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

type Fetcher = Box<dyn Fn(String, crate::address::Reach, FetchedDone, &'static [u16]) + Send + Sync>;
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
    Ready(Arc<Payload>, usize, Option<crate::address::Origin>),
    Pending,
    Failed(String, Instant),
}

struct Slot {
    entry: Entry,
    /// A restrictive request's refusal must not poison a later explicit local request.
    ceiling: crate::address::Reach,
    /// Completed Public and Local outcomes coexist; only one fetch per URL runs at a time.
    other: Option<(crate::address::Reach, Entry)>,
    used: u64,
    /// When the map last asked for it ([`get`]): a waiting tile not asked for lately is dropped.
    asked: Instant,
    epoch: u64,
}

impl Slot {
    /// Prefer this caller's completed outcome; otherwise a narrower ready answer can be shared.
    fn for_reach(&self, ceiling: crate::address::Reach) -> (&Entry, crate::address::Reach) {
        if let Some((reach, entry)) = &self.other
            && *reach == ceiling
        {
            return (entry, *reach);
        }
        (&self.entry, self.ceiling)
    }

    /// Both resident outcomes count towards the cache's single memory budget.
    fn bytes(&self) -> usize {
        let bytes = |entry: &Entry| match entry {
            Entry::Ready(_, n, _) => *n,
            _ => 0,
        };
        bytes(&self.entry) + self.other.as_ref().map_or(0, |(_, entry)| bytes(entry))
    }
}

#[derive(Default)]
struct Cache {
    slots: HashMap<String, Slot>,
    bytes: usize,
    tick: u64,
    /// When the map last asked for any address.
    latest: Option<Instant>,
}

impl Cache {
    fn forget_archive(&mut self, archive: &str) {
        let mut dropped = 0;
        self.slots.retain(|key, slot| {
            if resource_of(key) != archive {
                return true;
            }
            dropped += slot.bytes();
            false
        });
        self.bytes = self.bytes.saturating_sub(dropped);
    }
}

fn cache() -> &'static Mutex<Cache> {
    static C: OnceLock<Mutex<Cache>> = OnceLock::new();
    C.get_or_init(Mutex::default)
}

fn lock() -> std::sync::MutexGuard<'static, Cache> {
    cache().lock().unwrap_or_else(|e| e.into_inner())
}

static GENERATION: AtomicU64 = AtomicU64::new(0);
static CLEARS: AtomicU64 = AtomicU64::new(0);
static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

/// Source revisions are locked before any retained cache. Publication and reload share this
/// lock; callbacks and I/O must always run after releasing it.
fn archive_epochs() -> &'static Mutex<HashMap<String, u64>> {
    static E: std::sync::LazyLock<Mutex<HashMap<String, u64>>> = std::sync::LazyLock::new(Mutex::default);
    &E
}

/// The most recent reload of just these archives. Reload stamps increase globally, so any
/// selected source changing increases this maximum; unrelated reloads and tile arrivals do not.
/// The caller retains the source paths and can compare again without allocating a snapshot.
pub fn archive_revision<'a>(archives: impl IntoIterator<Item = &'a str>) -> u64 {
    let mut archives = archives.into_iter();
    let Some(first) = archives.next() else { return 0 };
    let epochs = archive_epochs().lock().unwrap_or_else(|e| e.into_inner());
    std::iter::once(first).chain(archives).filter_map(|url| epochs.get(url).copied()).max().unwrap_or(0)
}

pub(crate) fn archive_epoch(url: &str) -> u64 {
    archive_epochs().lock().unwrap_or_else(|e| e.into_inner()).get(resource_of(url)).copied().unwrap_or(0)
}

pub(crate) fn if_archive_current<T>(url: &str, epoch: u64, f: impl FnOnce() -> T) -> Option<T> {
    let epochs = archive_epochs().lock().unwrap_or_else(|e| e.into_inner());
    if epochs.get(resource_of(url)).copied().unwrap_or(0) != epoch {
        drop(epochs);
        return None;
    }
    Some(f())
}

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
/// The legacy byte-only hook is a trusted embedding override responsible for its own
/// destination policy. New transports should use set_fetcher_with_origin to receive the
/// inherited ceiling and report actual provenance; this hook cannot report redirects or peers.
pub fn set_fetcher(f: impl Fn(String, Done) + Send + Sync + 'static) {
    set_fetcher_omitting(move |url, done, _| f(url, done));
}

/// Like [`set_fetcher`], also handing the fetcher the statuses its caller expects (a tile past a
/// map's edge): a transport keeping diagnostics passes them to
/// [`RequestDiagnostics::omit_statuses`], so it records what the native transport records.
pub fn set_fetcher_omitting(f: impl Fn(String, Done, &'static [u16]) + Send + Sync + 'static) {
    *fetcher().lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(Box::new(move |url, _, done, omit| {
        let origin = crate::address::Origin::named(&crate::s3::bound_destination(&url).ok().flatten().unwrap_or_else(|| url.clone()));
        f(url, Box::new(move |r| done(r.map(|bytes| Fetched { bytes, origin }))), omit);
    })));
}

/// Installs a trusted replacement transport. The transport must check normalized URLs,
/// redirects and (when available) actual peers against the supplied ceiling before sending.
/// This wrapper validates the reported final origin before accepting bytes; it cannot undo
/// a request already sent. Completion reports provenance atomically with the bytes.
/// Browser transports use Origin::named on a normalized
/// destination; replacement native transports can use Origin::connected with their actual
/// socket peer. The built-in native connector records its peer reach separately.
/// The reported origin also owns login provenance: use Origin::retain_binding only if every
/// hop retained the selected login. Completion never restores a binding the transport dropped.
pub fn set_fetcher_with_origin(f: impl Fn(String, crate::address::Reach, FetchedDone, &'static [u16]) + Send + Sync + 'static) {
    *fetcher().lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(Box::new(move |url, ceiling, done, omit| {
        f(
            url,
            ceiling,
            Box::new(move |result| {
                done(result.and_then(|fetched| {
                    crate::address::check_reach(&fetched.origin.url, ceiling)?;
                    if fetched.origin.reach > ceiling {
                        return Err("the fetched answer exceeds its request's network reach".into());
                    }
                    Ok(fetched)
                }));
            }),
            omit,
        );
    })));
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

/// How many times [`clear`] has run. What was read through the downloads before it and is kept
/// elsewhere (the identify pop-up's pictures) goes too: it may have been read with a login that
/// has since ended.
pub fn clears() -> u64 {
    CLEARS.load(Ordering::Relaxed)
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
    request(url, want, &[], None).0
}

/// Like [`get`], for a map tile: a server's 404 (no tile there) or 410 (no longer there) for it
/// is routine (the map asks past an area's edges, and the map draws both as a gap: see
/// [`crate::warp::missing`]), so it isn't recorded as a failure; each would flood the history
/// and evict real failures. The omission is noted once per server instead. A picture that isn't
/// a tile (a single-image layer, a layout's picture, a style's sprite sheet) goes through
/// [`get`], so its 404 is recorded.
pub fn get_tile(url: &str, want: Want) -> State {
    get_tile_from(url, want, None)
}

/// Requests a tile under its supplying document's reach, with routine missing-tile statuses.
pub(crate) fn get_tile_from(url: &str, want: Want, from: Option<&crate::address::Origin>) -> State {
    request(url, want, TILE_OMIT, from).0
}

/// Requests a document-selected asset without granting it an explicit local allowance.
pub(crate) fn get_from(url: &str, want: Want, from: Option<&crate::address::Origin>) -> State {
    request(url, want, &[], from).0
}

/// Returns JSON and its effective origin from the same cache entry and lock acquisition.
pub(crate) fn document(url: &str, from: Option<&crate::address::Origin>) -> (State, Option<crate::address::Origin>) {
    let (state, origin, _) = request(url, Want::Json, &[], from);
    (state, origin)
}

/// A drawable tile and any omissions in a usable partial composite, from the same lookup.
pub(crate) fn tile_noted(url: &str, want: Want, from: Option<&crate::address::Origin>) -> (State, Option<String>) {
    let (state, _, note) = request(url, want, TILE_OMIT, from);
    (state, note)
}

/// Missing tiles and a view's unfetched dropped work are ordinary gaps, not drawing errors.
pub(crate) fn routine_tile_failure(error: &str) -> bool {
    error == DROPPED || http_status(error).is_some_and(|status| TILE_OMIT.contains(&status))
}

/// The statuses a map tile's request omits ([`get_tile`]).
const TILE_OMIT: &[u16] = &[404, 410];

/// [`get`] with the statuses that are expected answers for this fetch.
fn request(
    url: &str,
    want: Want,
    omit: &'static [u16],
    from: Option<&crate::address::Origin>,
) -> (State, Option<crate::address::Origin>, Option<String>) {
    if let Err(e) = crate::logins::check(resource_of(url)) {
        return (State::Failed(e.to_string()), None, None);
    }
    // A map tile of a layer in another system: its service's picture as it comes, or made from
    // its pictures (#769).
    if let Some(base) = crate::warp::direct(url) {
        return request(base, want, omit, from);
    }
    // This shared entry point also serves documents, sprites and archive readers without
    // drawing. Drawing checks separately so it can report refusals in unfetched margin tiles.
    if let Some(from) = from
        && let Err(e) = from.check(url)
    {
        return refused_request(url, e);
    }
    let ceiling = from.map_or(crate::address::Reach::Local, |o| o.reach);
    let epochs = archive_epochs().lock().unwrap_or_else(|e| e.into_inner());
    let epoch = epochs.get(resource_of(url)).copied().unwrap_or(0);
    let now = Instant::now();
    let mut c = lock();
    c.tick += 1;
    let tick = c.tick;
    c.latest = Some(now);
    if let Some(slot) = c.slots.get_mut(url) {
        slot.used = tick;
        slot.asked = now;
        let (entry, stored_ceiling) = slot.for_reach(ceiling);
        match entry {
            Entry::Ready(p, _, origin) => {
                if origin.as_ref().is_none_or(|o| o.reach <= ceiling) {
                    return (State::Ready(p.clone()), origin.clone(), None);
                }
                // The same name can resolve both locally and publicly. Retry through the
                // restrictive pool rather than reusing local bytes or hiding the public answer.
            }
            Entry::Pending => return (State::Pending, None, None),
            Entry::Failed(e, at) if ceiling == stored_ceiling && at.elapsed() < retry_wait(e, RETRY) => {
                // A server that said when to come back (429 or 503 with `Retry-After`) is asked
                // again then: the map draws again at that time, though nothing else draws it.
                // One that didn't is asked again by the next draw after a minute, as before.
                if let Some(wait) = retry_after(e) {
                    notify_at(*at + wait);
                }
                return (State::Failed(e.clone()), None, None);
            }
            Entry::Failed(..) => {}
        }
        // Another reach is fetching this URL. Its completion leaves this caller's retained
        // outcome intact; wait before retrying an expired or absent outcome of our own.
        if matches!(slot.entry, Entry::Pending) {
            return (State::Pending, None, None);
        }
    }
    if crate::warp::is_warp(url) {
        drop(c);
        drop(epochs);
        // Made once its pictures are here; asked again (as the map draws) until then.
        let mut note = None;
        return (
            match crate::warp::make(url, from) {
                crate::warp::Made::Waiting => State::Pending,
                crate::warp::Made::Done(payload) => {
                    let payload = Arc::new(payload);
                    let n = payload_bytes(&payload);
                    store_from(
                        url,
                        Entry::Ready(payload.clone(), n, Some(crate::address::Origin { url: url.into(), reach: ceiling })),
                        ceiling,
                    );
                    State::Ready(payload)
                }
                crate::warp::Made::ForNow(payload, omitted) => {
                    note = Some(omitted);
                    State::Ready(payload)
                }
                crate::warp::Made::Failed(e) => {
                    store_from(url, Entry::Failed(e.clone(), Instant::now()), ceiling);
                    State::Failed(e)
                }
            },
            None,
            note,
        );
    }
    let asked_at = crate::mbtiles::epoch();
    keep_for(&mut c, url, Entry::Pending, ceiling);
    if let Some(slot) = c.slots.get_mut(url) {
        slot.epoch = epoch;
    }
    drop(c);
    drop(epochs);
    start(url.to_string(), want, omit, epoch, asked_at, ceiling);
    (State::Pending, None, None)
}

/// A cache-policy refusal is a failed request too: record it like a transport refusal,
/// after releasing the cache lock, so it cannot become unexplained missing tiles.
fn refused_request(url: &str, message: String) -> (State, Option<crate::address::Origin>, Option<String>) {
    note_refusal(url, &message);
    (State::Failed(message), None, None)
}

/// A policy refusal is retried by drawing each frame; keep its network facts once per history.
pub(crate) fn note_refusal(url: &str, message: &str) {
    // Validation reports the effective web destination, without request-only account markers.
    // Resolve S3 endpoints without credentials, signing or I/O, just as destination checks do.
    let plain = crate::logins::inner(crate::s3::split_anonymous(url).0);
    let destination = match crate::s3::bound_destination(plain) {
        Ok(Some(at)) => Some(at),
        _ if crate::s3::is_s3(plain) => crate::s3::https_url(url).ok(),
        _ => None,
    };
    let effective = destination.as_deref().unwrap_or(plain);
    let mut diagnostics = RequestDiagnostics::new(effective, "GET");
    diagnostics.once = true;
    // Keep the reason independent of expanded tile paths; the history groups by server.
    let reason =
        message.strip_prefix(url).or_else(|| message.strip_prefix(effective)).and_then(|s| s.strip_prefix(": ")).unwrap_or(message);
    diagnostics.failed(&Failure::new(effective, reason));
}

/// What the cache holds for `url`, without fetching it.
pub fn peek(url: &str) -> Option<Arc<Payload>> {
    peek_from(url, None)
}

/// A cache-only lookup cannot reuse a local answer for a public document.
pub(crate) fn peek_from(url: &str, from: Option<&crate::address::Origin>) -> Option<Arc<Payload>> {
    crate::logins::check(resource_of(url)).ok()?;
    if let Some(base) = crate::warp::direct(url) {
        return peek_from(base, from);
    }
    let mut c = lock();
    c.tick += 1;
    let tick = c.tick;
    let slot = c.slots.get_mut(url)?;
    let ceiling = from.map_or(crate::address::Reach::Local, |o| o.reach);
    match slot.for_reach(ceiling).0 {
        Entry::Ready(p, _, origin) => {
            if from.is_some_and(|f| f.check(url).is_err() || origin.as_ref().is_some_and(|o| o.reach > f.reach)) {
                return None;
            }
            let payload = p.clone();
            slot.used = tick;
            Some(payload)
        }
        _ => None,
    }
}

/// Why the fetch of `url` failed, when the cache holds a failure for it, without asking for it
/// again: what a drawing asked for and couldn't have, for a caller that must say so (an export
/// of a tile layer).
pub fn peek_failure(url: &str) -> Option<String> {
    if let Err(e) = crate::logins::check(resource_of(url)) {
        return Some(e.to_string());
    }
    if let Some(base) = crate::warp::direct(url) {
        return peek_failure(base);
    }
    let c = lock();
    match &c.slots.get(url)?.entry {
        Entry::Failed(e, _) => Some(e.clone()),
        _ => None,
    }
}

/// Forgets everything (and failures, so they are tried again).
pub fn clear() {
    let mut c = lock();
    c.slots.retain(|_, s| {
        s.other = None;
        matches!(s.entry, Entry::Pending)
    });
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
    CLEARS.fetch_add(1, Ordering::Relaxed);
    // The map draws again (and the pop-up's pictures are forgotten): the waker wakes an idle app,
    // as when a fetch ends, so a clear from a worker thread isn't left until the next input.
    notify();
}

/// The resource a cache key reads: its address without a download budget, a byte range or a
/// tail, and for a tile of a PMTiles, COG or MBTiles archive, the archive's address.
fn resource_of(key: &str) -> &str {
    let key = split_download_limit(key).0;
    let key = split_tail(key).0;
    let key = split_range(key).0;
    // Archive file names are whole: only COG keys carry a look after `#`.
    if crate::mbtiles::is_tile(key) || key.starts_with(crate::pmtiles::SCHEME) {
        return key.split_once('@').map_or(key, |(_, archive)| archive);
    }
    if key.starts_with(crate::cog::SCHEME) {
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
    // PMTiles and MBTiles paths are taken whole (see `resource_of`), even with a look-like name.
    let pmtiles = url.rsplit_once('.').is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("pmtiles"));
    let url = if pmtiles || crate::mbtiles::check(url).is_ok() { url } else { crate::cog::split(url).0 };
    // First: a tile answered from here on is dropped, and one answered before goes with the rest.
    crate::mbtiles::forget_archive(url);
    let mut c = lock();
    c.slots.retain(|k, s| {
        if resource_of(k) != url {
            return true;
        }
        s.other = None;
        matches!(s.entry, Entry::Pending) && !crate::mbtiles::is_tile(k)
    });
    c.bytes = c.slots.values().map(Slot::bytes).sum();
    drop(c);
    crate::pmtiles::forget_archive(url);
    crate::cog::forget_file(url);
    // Map tiles made from a service's pictures, kept only while one is missing or on its way,
    // and their plans: all are made again (those kept in the cache stay till `clear`).
    crate::warp::forget();
    rustgis_data::source::remote::forget_file(url);
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// Reloads just this archive, including pending reads, metadata, directories, handles and
/// derived styles. Call only after uncached validation succeeds. Older answers are discarded.
pub fn reload_archive(url: &str) {
    let mut epochs = archive_epochs().lock().unwrap_or_else(|e| e.into_inner());
    static NEXT: AtomicU64 = AtomicU64::new(1);
    epochs.insert(url.to_string(), NEXT.fetch_add(1, Ordering::Relaxed));
    lock().forget_archive(url);
    let abandoned = crate::pmtiles::reload_archive(url);
    crate::mbtiles::reload_archive(url);
    crate::source::forget_archive(url);
    drop(epochs);
    // Dropping callbacks can complete tile answers and start queued reads: never under locks.
    drop(abandoned);
    notify();
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

/// A waiting fetch: decode kind, diagnostics, statuses, archive revision, context, MBTiles epoch and reach.
type Job = (Want, Capture, &'static [u16], u64, Option<Still>, u64, crate::address::Reach);

fn queue() -> &'static Mutex<Queue<Job>> {
    static Q: OnceLock<Mutex<Queue<Job>>> = OnceLock::new();
    Q.get_or_init(Mutex::default)
}

/// Asks for a tile: fetched now when its server has room, else it waits its turn.
fn start(url: String, want: Want, omit: &'static [u16], epoch: u64, asked_at: u64, ceiling: crate::address::Reach) {
    let capture = Capture::current();
    let still = current_still();
    IN_FLIGHT.fetch_add(1, Ordering::Relaxed);
    let mut swept = Vec::new();
    let now = {
        let mut q = queue().lock().unwrap_or_else(|e| e.into_inner());
        let mut c = lock();
        let latest = c.latest.unwrap_or_else(Instant::now);
        let now =
            q.push(url, (want, capture, omit, epoch, still, asked_at, ceiling), |u| c.slots.get(u).map(|s| s.asked), latest, &mut swept);
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
        if matches!(c.slots.get(&w.url), Some(Slot { entry: Entry::Pending, epoch, .. }) if *epoch == w.job.3) {
            forget_pending(c, &w.url);
        }
    }
    IN_FLIGHT.fetch_sub(dropped.len(), Ordering::Release);
}

/// Releases tiles that were selected to start but never launched (e.g. when launch is unwound
/// by a panic). Forgets their pending cache entries, frees their host reservations, safely releases
/// any queued jobs that would otherwise remain stranded when all active reservations empty, and
/// balances in_flight() accounting.
fn release_unstarted(unstarted: Vec<Waiting<Job>>) {
    if unstarted.is_empty() {
        return;
    }
    let mut stranded = Vec::new();
    {
        let mut q = queue().lock().unwrap_or_else(|e| e.into_inner());
        let mut c = lock();
        // Only this job's own pending slot: not one a newer request planted after an archive
        // reload (archive revision) or after its MBTiles file was forgotten.
        let own = |c: &Cache, w: &Waiting<Job>| {
            !crate::mbtiles::forgotten_since(&w.url, w.job.5)
                && matches!(c.slots.get(&w.url), Some(Slot { entry: Entry::Pending, epoch, .. }) if *epoch == w.job.3)
        };
        for w in &unstarted {
            if own(&c, w) {
                forget_pending(&mut c, &w.url);
            }
            stranded.extend(q.cancel(&w.host));
        }
        for s in &stranded {
            if own(&c, s) {
                forget_pending(&mut c, &s.url);
            }
        }
    }
    let total_released = unstarted.len() + stranded.len();
    IN_FLIGHT.fetch_sub(total_released, Ordering::Release);
    notify();
}

thread_local! {
    static TO_START: RefCell<Vec<Waiting<Job>>> = const { RefCell::new(Vec::new()) };
    static STARTING: Cell<bool> = const { Cell::new(false) };
}

/// Starts fetches one after another, never inside each other: a fetch can be over at once (a
/// local read, an archive that failed), and each one over starts the next waiting tile, so a
/// long queue would otherwise go as deep into the stack as it is long.
fn launch(jobs: Vec<Waiting<Job>>) {
    if !jobs.is_empty() {
        // Newest first: `pop` takes from the end.
        TO_START.with(|q| q.borrow_mut().extend(jobs.into_iter().rev()));
    }
    STARTING.with(|starting| {
        // Another call further up this thread is starting them already.
        if starting.replace(true) {
            return;
        }
        // Lowered however this ends: a fetcher that panics must not stop every later fetch,
        // and any selected but unstarted jobs must be released so pending states, host slots,
        // and in_flight() stay consistent.
        struct Lower<'a>(&'a Cell<bool>);
        impl Drop for Lower<'_> {
            fn drop(&mut self) {
                self.0.set(false);
                let unstarted = TO_START.with(|q| std::mem::take(&mut *q.borrow_mut()));
                release_unstarted(unstarted);
            }
        }
        let _lower = Lower(starting);
        while let Some(w) = TO_START.with(|q| q.borrow_mut().pop()) {
            let (want, capture, omit, epoch, still, asked_at, ceiling) = w.job;
            let _scope = capture.enter();
            with_optional_still(still, || fetch_tile(w.url, w.host, want, omit, epoch, asked_at, ceiling));
        }
    });
}

/// A tile fetch's answer: stored once it comes, and however the fetch ends (answered, dropped
/// unanswered by its fetcher, or unwound by a panic) its server's place goes to the next tile
/// and the fetch no longer counts as under way.
struct Answer {
    key: String,
    host: String,
    entry: Option<Entry>,
    epoch: u64,
    /// When the tile was asked for, for an MBTiles file forgotten since (`land`).
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
        if_archive_current(&self.key, self.epoch, || land(&self.key, self.entry.take(), self.asked_at));
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
            forget_pending(&mut c, key);
        }
    }
}

/// Retains the initiating lifetime through deferred decoding. Retired results are forgotten;
/// only live successes and resource failures from the current archive revision are shared.
fn fetch_tile(url: String, host: String, want: Want, omit: &'static [u16], epoch: u64, asked_at: u64, ceiling: crate::address::Reach) {
    let owner = current_still();
    let local = crate::pmtiles::is_tile(&url) || crate::cog::is_tile(&url) || crate::mbtiles::is_tile(&url);
    let answer = Answer { key: url.clone(), host, entry: None, epoch, asked_at };
    if archive_epoch(&url) != epoch {
        return;
    }
    let done: FetchedDone = Box::new(move |fetched| {
        let (bytes, origin) = match fetched {
            Ok(f) => (Ok(f.bytes), (!local).then_some(f.origin)),
            Err(e) => (Err(e), None),
        };
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
                    Entry::Ready(Arc::new(p), size, origin)
                }
                // The browser's decoder may answer after the initiating owner was retired.
                Err(e) if !keep_failure(&e, &still) => return,
                Err(e) => {
                    log::debug!("{}", crate::logins::scrub_error(&rustgis_data::diagnostics::message_with_address(&answer.key, &e)));
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
    if !local {
        fetch_reached(url, ceiling, done, omit);
        return;
    }
    let origin = crate::address::Origin::named(&url);
    let done: Done = Box::new(move |r| done(r.map(|bytes| Fetched { bytes, origin })));
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

/// Whether the browser gives up a fetch of `url` that stalls, as an installed fetcher is handed it
/// with `ceiling`. Tiles and documents are given up. So is a download kept to internet addresses
/// ([`download_public_limited`]): its address comes from data, like a picture for the pop-up.
/// A download the user named is not given up.
pub fn gives_up_when_stalled(url: &str, ceiling: crate::address::Reach) -> bool {
    split_download_limit(url).1.is_none() || ceiling == crate::address::Reach::Public
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
    /// Unsent policy refusals are repeated by redraw, not distinct transport attempts.
    once: bool,
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
        Self { url, method, capture, started: Instant::now(), status: None, timed_out: false, once: false, omit: &[] }
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
        let message = failure.message.contains(url).then(|| rustgis_data::diagnostics::with_address_sanitized(url, &failure.message));
        let message = clean(message.as_deref().unwrap_or(&failure.message));
        let network = Network {
            url: url.to_string(),
            method: self.method.to_string(),
            status: self.status,
            duration_ms: (!self.once).then(|| self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64),
            kind,
        };
        if self.once {
            self.capture.network_failure_once("Network", &message, &clean(&details), network);
        } else {
            self.capture.network_failure("Network", &message, &clean(&details), network);
        }
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

/// Like [`download_limited`], connecting natively only to internet addresses (as
/// [`download_public_within`], off the frame): a name, or a redirect to one, that resolves to this
/// computer or the local network is refused when it is resolved. For an address that comes from
/// data rather than the user (a picture named in a feature's values), which mustn't be able to
/// reach local services. An installed fetcher (the browser's) is handed the internet-only ceiling.
pub fn download_public_limited(url: &str, max: usize, done: Done) {
    download_reaching(url, max, DOWNLOAD_TIMEOUT, &[], Reaching::Public, Box::new(move |r| done(r.map_err(|f| f.message))));
}

/// Which addresses a native download may connect to.
#[derive(Clone, Copy)]
enum Reaching {
    /// Wherever the address's name resolves (the user named it).
    Any,
    /// Only internet addresses, judged where names resolve and redirects lead.
    Public,
}

/// Like [`download_detailed`], giving up after `timeout` natively; a status in `omit` isn't
/// recorded as a failure.
fn download_detailed_within(url: &str, max: usize, timeout: std::time::Duration, omit: &'static [u16], done: DoneDetailed) {
    download_reaching(url, max, timeout, omit, Reaching::Any, done);
}

/// [`download_detailed_within`], natively connecting only where `reaching` allows.
fn download_reaching(url: &str, max: usize, timeout: std::time::Duration, omit: &'static [u16], reaching: Reaching, done: DoneDetailed) {
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
        Some(f) => f(
            marked.clone(),
            match reaching {
                Reaching::Any => crate::address::Reach::Local,
                Reaching::Public => crate::address::Reach::Public,
            },
            Box::new(move |r| done(r.map(|fetched| fetched.bytes).map_err(|e| Failure::new(&marked, e)))),
            omit,
        ),
        None => match reaching {
            Reaching::Any => native::download_within(marked, timeout, omit, done),
            Reaching::Public => native::download_public_within(marked, timeout, done),
        },
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

/// Resolves a GET's destination and S3 headers for both native and browser transports.
/// A validated explicit service login takes precedence over an implicit endpoint signer.
/// Its credential is still applied separately at send time, so revocation cannot fall back
/// to anonymous access or ambient S3 credentials.
pub fn prepare_get(url: &str) -> Result<crate::s3::Prepared, String> {
    crate::logins::check(url).map_err(str::to_string)?;
    if rustgis_data::login_address::is_wrapper(url) {
        return Ok(crate::s3::Prepared { url: crate::logins::inner(url).to_string(), headers: Vec::new(), signed: false });
    }
    crate::s3::prepare(url)
}

/// Fetches an address's bytes, uncached (through the browser's fetcher, or the worker threads).
/// A status in `omit` is an expected answer: its failure isn't recorded, only noted once per
/// server (a tile past a map's edge).
pub(crate) fn fetch_bytes(url: String, done: Done, omit: &'static [u16]) {
    fetch_reached(url, crate::address::Reach::Local, Box::new(move |r| done(r.map(|f| f.bytes))), omit);
}

/// Carries destination policy into the transport and returns provenance with the bytes.
fn fetch_reached(url: String, ceiling: crate::address::Reach, done: FetchedDone, omit: &'static [u16]) {
    if let Err(e) = check_current() {
        done(Err(e));
        return;
    }
    let still = current_still();
    let done: FetchedDone = Box::new(move |r| with_optional_still(still, || done(r)));
    if let Err(e) = crate::logins::check(resource_of(&url)) {
        done(Err(e.to_string()));
        return;
    }
    let key = url.clone();
    let done: FetchedDone = Box::new(move |r| done(crate::logins::check(resource_of(&key)).map_err(str::to_string).and(r)));
    let f = fetcher().lock().unwrap_or_else(|e| e.into_inner()).clone();
    match f {
        Some(f) => f(url, ceiling, done, omit),
        None => native::fetch_reached(url, ceiling, done, omit),
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

/// Stores an explicitly fetched answer; composed answers use their inherited ceiling.
#[cfg(test)]
fn store(url: &str, entry: Entry) {
    store_from(url, entry, crate::address::Reach::Local);
}

/// Moves a cached failure past its original retry deadline without fetching or changing clocks.
#[cfg(test)]
pub(crate) fn expire_failure(url: &str) -> bool {
    let mut c = lock();
    let Some(slot) = c.slots.get_mut(url) else { return false };
    let Entry::Failed(error, at) = &mut slot.entry else { return false };
    *at = Instant::now() - retry_wait(error, RETRY) - Duration::from_secs(1);
    true
}

/// Composed cache entries retain the ceiling their constituent requests used.
fn store_from(url: &str, entry: Entry, ceiling: crate::address::Reach) {
    if login_gone(url) {
        return;
    }
    let mut c = lock();
    keep_for(&mut c, url, entry, ceiling);
}

/// Whether the login `url` was fetched with is gone, in which case nothing of it is kept.
fn login_gone(url: &str) -> bool {
    if crate::logins::check(resource_of(url)).is_ok() {
        return false;
    }
    let mut c = lock();
    if let Some(slot) = c.slots.remove(url) {
        c.bytes = c.bytes.saturating_sub(slot.bytes());
    }
    true
}

/// Keeps a transport completion under the ceiling of its still-pending slot.
fn keep(c: &mut Cache, url: &str, entry: Entry) {
    let ceiling = c.slots.get(url).map_or(crate::address::Reach::Local, |s| s.ceiling);
    keep_for(c, url, entry, ceiling);
}

/// Replaces one reach's outcome without evicting the other's completed answer or retry time.
fn keep_for(c: &mut Cache, url: &str, entry: Entry, ceiling: crate::address::Reach) {
    let old = c.slots.remove(url);
    let asked = old.as_ref().map_or_else(Instant::now, |s| s.asked);
    // The caller already holds the archive epoch lock when publishing a transport answer.
    // Preserve its slot's revision without recursively acquiring that lock here.
    let epoch = old.as_ref().map_or(0, |s| s.epoch);
    let other = old.and_then(|slot| {
        c.bytes = c.bytes.saturating_sub(slot.bytes());
        if slot.ceiling != ceiling && !matches!(slot.entry, Entry::Pending) { Some((slot.ceiling, slot.entry)) } else { slot.other }
    });
    let slot = Slot { entry, ceiling, other, used: c.tick, asked, epoch };
    c.bytes += slot.bytes();
    c.slots.insert(url.to_string(), slot);
    if c.bytes > BUDGET {
        evict(c);
    }
}

/// Dropping a fetch leaves any completed result for the other reach available.
fn forget_pending(c: &mut Cache, url: &str) {
    if !matches!(c.slots.get(url), Some(Slot { entry: Entry::Pending, .. })) {
        return;
    }
    if let Some(mut slot) = c.slots.remove(url)
        && let Some((reach, entry)) = slot.other.take()
    {
        slot.entry = entry;
        slot.ceiling = reach;
        c.slots.insert(url.to_string(), slot);
    }
}

/// Drops the least recently used entries until the cache is at three quarters of its budget.
fn evict(c: &mut Cache) {
    let mut ready: Vec<(u64, String, usize)> = c
        .slots
        .iter()
        .filter(|(_, s)| !matches!(s.entry, Entry::Pending) || s.other.is_some())
        .map(|(k, s)| (s.used, k.clone(), s.bytes()))
        .collect();
    ready.sort_unstable_by_key(|r| r.0);
    for (_, k, n) in ready {
        if c.bytes <= BUDGET / 4 * 3 {
            break;
        }
        if let Some(slot) = c.slots.get_mut(&k)
            && matches!(slot.entry, Entry::Pending)
        {
            slot.other = None;
        } else {
            c.slots.remove(&k);
        }
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

    use super::{Capture, DROPPED, Done, DoneDetailed, Fetched, FetchedDone, MAX_BYTES};
    use crate::address::{Origin, Reach};
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
        waiting: Vec<(String, String, FetchedDone, Capture, &'static [u16], Option<super::Still>, Reach)>,
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

    /// Queues an explicitly selected address, keeping the byte-only public callback stable.
    pub(super) fn fetch(url: String, done: Done, omit: &'static [u16]) {
        fetch_reached(url, Reach::Local, Box::new(move |r| done(r.map(|f| f.bytes))), omit);
    }

    /// Keeps the destination ceiling with queued work; the web build requires its own fetcher.
    pub(super) fn fetch_reached(url: String, ceiling: Reach, done: FetchedDone, omit: &'static [u16]) {
        let capture = Capture::current();
        let q = queue();
        let dropped = {
            let mut jobs = q.jobs.lock().unwrap_or_else(|e| e.into_inner());
            let host = host(&url);
            jobs.waiting.push((url, host, done, capture, omit, super::current_still(), ceiling));
            (jobs.waiting.len() > QUEUE).then(|| jobs.waiting.remove(0))
        };
        // Every worker may be waiting for a server at its cap, and only some can take this one.
        q.ready.notify_all();
        // Answered once the queue is unlocked: `done` takes the cache's lock and wakes the app.
        if let Some((_, _, done, _, _, _, _)) = dropped {
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
            // A proxy could resolve the target itself, past the checked resolver.
            .proxy(None)
            // Redirects are followed by `get`, which checks where each one leads.
            .max_redirects(0)
    }

    /// Workers share this agent's checked connections and bounded idle pool.
    pub(super) fn agent() -> ureq::Agent {
        let config = config_builder(std::time::Duration::from_secs(30))
            // The workers share the agent: keep a connection open for each fetch a server
            // may have under way, so the next tile doesn't connect again.
            .max_idle_connections_per_host(PER_HOST)
            .max_idle_connections(WORKERS)
            .build();
        checked_agent(config, ureq::unversioned::resolver::DefaultResolver::default(), false)
    }

    /// The agent the workers share, and with it their open connections.
    fn shared_agent() -> &'static ureq::Agent {
        static A: OnceLock<ureq::Agent> = OnceLock::new();
        A.get_or_init(agent)
    }

    /// An agent for downloads, which may take minutes.
    pub(super) fn download_agent(timeout: std::time::Duration) -> ureq::Agent {
        checked_agent(config_builder(timeout).build(), ureq::unversioned::resolver::DefaultResolver::default(), false)
    }

    /// Applies the same IP boundary to every native connection, including a new lookup after
    /// DNS changes. Document requests keep their inherited public or local ceiling,
    /// and forbidden addresses never leave the resolver. Plugin/POST agents narrow
    /// this further to internet addresses only.
    #[derive(Debug)]
    struct CheckedResolver<R> {
        inner: R,
        public_only: bool,
    }

    impl<R: ureq::unversioned::resolver::Resolver> ureq::unversioned::resolver::Resolver for CheckedResolver<R> {
        /// Filters the exact socket addresses passed to the connector, without a second lookup.
        fn resolve(
            &self,
            uri: &ureq::http::Uri,
            config: &ureq::config::Config,
            timeout: ureq::unversioned::transport::NextTimeout,
        ) -> Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
            let url = uri.to_string();
            crate::address::check(&url, None).map_err(|e| ureq::Error::Io(std::io::Error::new(std::io::ErrorKind::PermissionDenied, e)))?;
            let ceiling = if self.public_only { Reach::Public } else { Reach::Local };
            let all = self.inner.resolve(uri, config, timeout)?;
            let mut kept = self.empty();
            for a in all.iter().filter(|a| {
                let reach = crate::address::ip_reach(a.ip());
                reach != crate::address::Reach::Forbidden && ceiling != crate::address::Reach::Forbidden && reach <= ceiling
            }) {
                kept.push(*a);
            }
            if kept.is_empty() {
                let host = uri.host().unwrap_or_default();
                let msg =
                    format!("{host} resolves to this computer or the local network, or a forbidden address, which isn't fetched from here");
                return Err(ureq::Error::Io(std::io::Error::new(std::io::ErrorKind::PermissionDenied, msg)));
            }
            Ok(kept)
        }
    }

    /// Resolver filtering retains explicit local fallback while removing forbidden peers.
    /// Public-document requests never receive local candidates. This fixture opens no sockets.
    #[cfg(test)]
    #[test]
    fn mixed_dns_destinations_keep_local_only_when_allowed() {
        use ureq::unversioned::resolver::{ResolvedSocketAddrs, Resolver};
        #[derive(Debug)]
        struct Answer(bool);
        impl Resolver for Answer {
            fn resolve(
                &self,
                _: &ureq::http::Uri,
                _: &ureq::config::Config,
                _: ureq::unversioned::transport::NextTimeout,
            ) -> Result<ResolvedSocketAddrs, ureq::Error> {
                let mut all = self.empty();
                all.push("127.0.0.1:80".parse().unwrap());
                all.push("169.254.169.254:80".parse().unwrap());
                all.push("[64:ff9b::a9fe:a9fe]:80".parse().unwrap());
                if self.0 {
                    all.push("203.0.113.1:80".parse().unwrap());
                }
                Ok(all)
            }
        }
        let uri = "http://mixed-dns.example/".parse().unwrap();
        let config = config_builder(std::time::Duration::from_secs(1)).build();
        for mixed in [true, false] {
            for public_only in [true, false] {
                let result = CheckedResolver { inner: Answer(mixed), public_only }.resolve(&uri, &config, Default::default());
                if !mixed && public_only {
                    assert!(result.unwrap_err().to_string().contains("resolves to"));
                } else {
                    let kept = result.unwrap();
                    assert_eq!(kept.len(), if mixed && !public_only { 2 } else { 1 });
                    assert!(kept.iter().all(|a| crate::address::ip_reach(a.ip()) != Reach::Forbidden));
                    if public_only {
                        assert!(kept.iter().all(|a| crate::address::ip_reach(a.ip()) == Reach::Public));
                    }
                }
            }
        }
    }

    thread_local! {
        static SENT_TO: std::cell::Cell<Option<Reach>> = const { std::cell::Cell::new(None) };
    }

    /// Reads the reach of this synchronous request, including a pooled connection. This is
    /// observed provenance, never an ambient permission; each agent has a fixed IP ceiling.
    fn connected<T>(f: impl FnOnce() -> Result<T, ureq::Error>) -> Result<(T, Reach), ureq::Error> {
        struct Restore(Option<Reach>);
        impl Drop for Restore {
            fn drop(&mut self) {
                SENT_TO.with(|r| r.set(self.0));
            }
        }
        let _restore = Restore(SENT_TO.with(|r| r.replace(None)));
        f().map(|v| (v, SENT_TO.with(|r| r.get()).unwrap_or(Reach::Public)))
    }

    #[derive(Debug, Default)]
    struct ReachConnector<C = ureq::unversioned::transport::DefaultConnector>(C);

    impl<C: ureq::unversioned::transport::Connector<(), Out = Box<dyn ureq::unversioned::transport::Transport>>>
        ureq::unversioned::transport::Connector<()> for ReachConnector<C>
    {
        type Out = Box<dyn ureq::unversioned::transport::Transport>;
        fn connect(
            &self,
            details: &ureq::unversioned::transport::ConnectionDetails,
            chained: Option<()>,
        ) -> Result<Option<Self::Out>, ureq::Error> {
            use ureq::unversioned::transport::time::Duration;
            let first = details.addrs.first().map_or(Reach::Public, |a| crate::address::ip_reach(a.ip()));
            let mixed = details.addrs.iter().any(|a| crate::address::ip_reach(a.ip()) != first);
            if !mixed {
                return Ok(self.0.connect(details, chained)?.map(|inner| Box::new(ReachedTransport { inner, reach: first }) as Self::Out));
            }
            // The transport API does not expose its peer. Try at most two homogeneous
            // groups under one deadline, so success identifies the actual reach without
            // losing an explicitly selected server's fallback or changing its TLS name.
            let other = if first == Reach::Public { Reach::Local } else { Reach::Public };
            let began = web_time::Instant::now();
            let mut last = None;
            for (index, reach) in [first, other].into_iter().enumerate() {
                let mut addrs = details.resolver.empty();
                for address in details.addrs.iter().filter(|a| crate::address::ip_reach(a.ip()) == reach) {
                    addrs.push(*address);
                }
                let mut attempt = connection_at(details, addrs);
                attempt.now = (details.current_time)();
                if !details.timeout.after.is_not_happening() {
                    let left = details.timeout.after.saturating_sub(began.elapsed());
                    if left.is_zero() {
                        return Err(ureq::Error::Timeout(details.timeout.reason));
                    }
                    attempt.timeout.after = Duration::Exact(left / (2 - index) as u32);
                }
                match self.0.connect(&attempt, chained) {
                    Ok(Some(inner)) => return Ok(Some(Box::new(ReachedTransport { inner, reach }) as Self::Out)),
                    Ok(None) => {}
                    Err(e) => last = Some(e),
                }
            }
            match last {
                Some(error) => Err(error),
                None => Ok(None),
            }
        }
    }

    /// Reuses URI, TLS, cancellation and resolver settings while narrowing socket candidates.
    fn connection_at<'a>(
        d: &ureq::unversioned::transport::ConnectionDetails<'a>,
        addrs: ureq::unversioned::resolver::ResolvedSocketAddrs,
    ) -> ureq::unversioned::transport::ConnectionDetails<'a> {
        ureq::unversioned::transport::ConnectionDetails {
            uri: d.uri,
            addrs,
            config: d.config,
            request_level: d.request_level,
            resolver: d.resolver,
            now: d.now,
            timeout: d.timeout,
            current_time: d.current_time.clone(),
            run_connector: d.run_connector.clone(),
            register_shutdown: d.register_shutdown.clone(),
        }
    }

    #[derive(Debug)]
    struct ReachedTransport {
        inner: Box<dyn ureq::unversioned::transport::Transport>,
        reach: Reach,
    }

    impl ureq::unversioned::transport::Transport for ReachedTransport {
        fn buffers(&mut self) -> &mut dyn ureq::unversioned::transport::Buffers {
            self.inner.buffers()
        }
        fn transmit_output(&mut self, amount: usize, timeout: ureq::unversioned::transport::NextTimeout) -> Result<(), ureq::Error> {
            self.inner.transmit_output(amount, timeout)?;
            SENT_TO.with(|r| r.set(Some(self.reach)));
            Ok(())
        }
        fn await_input(&mut self, timeout: ureq::unversioned::transport::NextTimeout) -> Result<bool, ureq::Error> {
            self.inner.await_input(timeout)
        }
        fn is_open(&mut self) -> bool {
            self.inner.is_open()
        }
        fn is_tls(&self) -> bool {
            self.inner.is_tls()
        }
    }

    /// Public-document requests cannot reuse an explicitly local connection from another pool.
    fn shared_public_agent() -> &'static ureq::Agent {
        static A: OnceLock<ureq::Agent> = OnceLock::new();
        A.get_or_init(|| public_agent(std::time::Duration::from_secs(30)))
    }

    /// Builds the ordinary and internet-only agents through one connection boundary. The
    /// supplied resolver also lets controlled fixtures exercise DNS changes without network DNS.
    pub(super) fn checked_agent(
        config: ureq::config::Config,
        resolver: impl ureq::unversioned::resolver::Resolver,
        public_only: bool,
    ) -> ureq::Agent {
        ureq::Agent::with_parts(
            config,
            ReachConnector::<ureq::unversioned::transport::DefaultConnector>::default(),
            CheckedResolver { inner: resolver, public_only },
        )
    }

    /// Keeps the normal DNS filtering and peer recording around a controlled test transport.
    #[cfg(test)]
    pub(super) fn fixture_transport_agent(
        connector: impl ureq::unversioned::transport::Connector<(), Out = Box<dyn ureq::unversioned::transport::Transport>>,
    ) -> ureq::Agent {
        ureq::Agent::with_parts(
            config_builder(std::time::Duration::from_secs(10)).build(),
            ReachConnector(connector),
            CheckedResolver { inner: ureq::unversioned::resolver::DefaultResolver::default(), public_only: false },
        )
    }

    #[cfg(test)]
    /// Exercises classified DNS and real HTTP while routing every connection to an owned listener.
    pub(super) fn fixture_agent(answer: std::net::SocketAddr, listener: std::net::SocketAddr, public_only: bool) -> ureq::Agent {
        fixture_answers(vec![answer], listener, public_only, false)
    }

    /// Mixed-DNS fixtures fail the synthetic public connection without opening a socket;
    /// all other fixture connections go only to the listener owned by the test.
    #[cfg(test)]
    fn fixture_answers(
        answers: Vec<std::net::SocketAddr>,
        listener: std::net::SocketAddr,
        public_only: bool,
        fail_public: bool,
    ) -> ureq::Agent {
        use ureq::unversioned::{
            resolver::Resolver,
            transport::{ConnectionDetails, Connector, DefaultConnector, Transport},
        };
        #[derive(Debug)]
        struct Dns(Vec<std::net::SocketAddr>);
        impl Resolver for Dns {
            fn resolve(
                &self,
                uri: &ureq::http::Uri,
                _: &ureq::config::Config,
                _: ureq::unversioned::transport::NextTimeout,
            ) -> Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
                let mut addresses = self.empty();
                let literal = uri.host().and_then(|h| h.parse::<std::net::IpAddr>().ok());
                if let Some(ip) = literal {
                    addresses.push(std::net::SocketAddr::new(ip, uri.port_u16().unwrap_or(80)));
                } else {
                    for answer in &self.0 {
                        addresses.push(*answer);
                    }
                }
                Ok(addresses)
            }
        }
        #[derive(Debug)]
        struct Route(std::net::SocketAddr, bool);
        impl Connector<()> for Route {
            type Out = Box<dyn Transport>;
            fn connect(&self, d: &ConnectionDetails, _: Option<()>) -> Result<Option<Self::Out>, ureq::Error> {
                if self.1 && d.addrs.first().is_some_and(|a| crate::address::ip_reach(a.ip()) == Reach::Public) {
                    return Err(std::io::Error::from(std::io::ErrorKind::ConnectionRefused).into());
                }
                let mut addrs = d.addrs.clone();
                for address in addrs.iter_mut() {
                    *address = self.0;
                }
                let fixture = connection_at(d, addrs);
                DefaultConnector::default().connect(&fixture, None)
            }
        }
        ureq::Agent::with_parts(
            config_builder(std::time::Duration::from_secs(2)).build(),
            ReachConnector(Route(listener, fail_public)),
            CheckedResolver { inner: Dns(answers), public_only },
        )
    }

    /// Actual requests exercise resolver filtering, connector fallback and retained provenance.
    #[cfg(test)]
    #[test]
    fn mixed_dns_destinations_preserve_explicit_local_fallback() {
        let _turn = super::turn();
        let base = super::tests::serve(vec!["HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ntile".into()]);
        let listener = base.strip_prefix("http://").unwrap().parse().unwrap();
        let answers = vec!["203.0.113.1:80".parse().unwrap(), "127.0.0.1:80".parse().unwrap()];
        let public = fixture_answers(answers.clone(), listener, true, true);
        assert!(fetch_answer(&public, "http://mixed-fixture.example/style.json", Reach::Public, false, &[]).is_err());
        let explicit = fixture_answers(answers, listener, false, true);
        let got = fetch_answer(&explicit, "http://mixed-fixture.example/style.json", Reach::Local, false, &[]).unwrap();
        assert_eq!(got.bytes, b"tile");
        assert_eq!(got.origin.reach, Reach::Local);
    }

    /// A public hop narrows both GET and HEAD before redirected local DNS can connect.
    #[cfg(test)]
    #[test]
    fn public_redirects_filter_the_next_hosts_dns() {
        let _turn = super::turn();
        let target = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        target.set_nonblocking(true).unwrap();
        let address = target.local_addr().unwrap();
        let redirect = format!(
            "HTTP/1.1 302 Found\r\nLocation: http://redirect-local.example:{}/tile\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            address.port()
        );
        let first = super::tests::serve(vec![redirect.clone(), redirect]);
        let listener = first.strip_prefix("http://").unwrap().parse().unwrap();
        let selected = fixture_agent("203.0.113.1:80".parse().unwrap(), listener, false);
        let public = checked_agent(config_builder(std::time::Duration::from_secs(2)).build(), super::tests::LocalAnswer(address), true);
        let url = "http://first-public.example/style.json";
        assert!(
            fetch_answer_with_public(&selected, &public, url, Reach::Local, false, &[]).unwrap_err().to_string().contains("resolves to")
        );
        assert!(head_with_public(&selected, &public, url).unwrap_err().contains("resolves to"));
        assert_eq!(target.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
    }

    /// An internet-only agent with the same idle capacity as the tile workers' ordinary pool.
    pub(super) fn public_agent(timeout: std::time::Duration) -> ureq::Agent {
        let config = config_builder(timeout).max_idle_connections_per_host(PER_HOST).max_idle_connections(WORKERS).build();
        checked_agent(config, ureq::unversioned::resolver::DefaultResolver::default(), true)
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

    /// Asks for a caller-selected archive's headers, following up to 5 redirects, each checked
    /// as [`get`] checks them. Document-derived reads use the GET queue's inherited ceiling.
    pub(super) fn head_with(agent: &ureq::Agent, url: &str) -> Result<super::Head, String> {
        head_with_public(agent, shared_public_agent(), url)
    }

    /// Uses a separate internet-only pool after a public hop; fixtures supply both pools.
    fn head_with_public(agent: &ureq::Agent, public: &ureq::Agent, url: &str) -> Result<super::Head, String> {
        let mut diagnostics = super::RequestDiagnostics::new(url, "HEAD");
        let mut carried_login = false;
        let answer = (|| -> Result<super::Head, Failure> {
            crate::logins::check(url).map_err(str::to_string)?;
            let prepared = crate::s3::prepare(crate::logins::inner(url))?;
            crate::address::check(&prepared.url, None)?;
            if prepared.signed {
                return Ok(super::Head::default());
            }
            let mut at = prepared.url.clone();
            let first_origin = crate::logins::origin(&at);
            let mut same_origin = true;
            let mut ceiling = Reach::Local;
            for redirects in 0..=5 {
                // The login goes to its own origin only: once a redirect leaves it, never again.
                let bound = crate::logins::inherit(url, &at);
                let login = if same_origin { crate::logins::apply(&bound)? } else { None };
                carried_login |= login.is_some();
                let sending = if ceiling == Reach::Public { public } else { agent };
                let mut req = sending.head(login.as_ref().map_or(at.as_str(), |a| a.url.as_str()));
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
                let req = if sending.config().timeouts().global == agent.config().timeouts().global {
                    req
                } else {
                    req.config().timeout_global(agent.config().timeouts().global).build()
                };
                let (resp, reached) = connected(|| req.call()).map_err(unanswered)?;
                ceiling = ceiling.min(reached);
                diagnostics.answered(resp.status().as_u16());
                let header = |k: &str| resp.headers().get(k).and_then(|v| v.to_str().ok());
                if resp.status().is_redirection() {
                    let next = crate::template::join(&at, header("location").ok_or("a redirect with no Location")?);
                    Origin { url: at.clone(), reach: ceiling }.check(&next)?;
                    if crate::address::is_https(&at) && !crate::address::is_https(&next) {
                        return Err(rustgis_data::diagnostics::message_with_address(&next, "a redirect from https to http").into());
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

    /// Like [`download_within`], connecting only to internet addresses ([`public_agent`]).
    pub(super) fn download_public_within(url: String, timeout: std::time::Duration, done: DoneDetailed) {
        let capture = Capture::current();
        let still = super::current_still();
        std::thread::spawn(move || {
            super::with_optional_still(still, || {
                let _scope = capture.enter();
                let r =
                    super::check_current().map_err(Failure::from).and_then(|_| get_detailed_omitting(&public_agent(timeout), &url, &[]));
                done(r);
            })
        });
    }

    /// Dispatches each queued read through the pool matching its inherited IP ceiling.
    fn work() {
        let agent = shared_agent();
        let q = queue();
        loop {
            let (url, done, host, capture, omit, still, ceiling) = {
                let mut jobs = q.jobs.lock().unwrap_or_else(|e| e.into_inner());
                loop {
                    let hosts: Vec<&str> = jobs.waiting.iter().map(|(_, h, _, _, _, _, _)| h.as_str()).collect();
                    if let Some(i) = pick(&hosts, &jobs.active) {
                        let (url, host, done, capture, omit, still, ceiling) = jobs.waiting.remove(i);
                        *jobs.active.entry(host.clone()).or_insert(0) += 1;
                        break (url, done, host, capture, omit, still, ceiling);
                    }
                    jobs = q.ready.wait(jobs).unwrap_or_else(|e| e.into_inner());
                }
            };
            let _scope = capture.enter();
            let result = super::with_optional_still(still.clone(), || {
                super::check_current()?;
                fetch_answer(if ceiling == Reach::Public { shared_public_agent() } else { agent }, &url, ceiling, false, omit)
                    .map_err(|f| f.message)
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

    /// Fetches `url`, following up to 5 redirects. Checks the final network destination here,
    /// even when the address was already checked before expansion or signing; every redirect
    /// and every resolved connection is checked too.
    pub(super) fn get(agent: &ureq::Agent, url: &str, omit: &'static [u16]) -> Result<Vec<u8>, String> {
        fetch_answer(agent, url, Reach::Local, false, omit).map(|f| f.bytes).map_err(|f| f.message)
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
        fetch_answer(agent, url, Reach::Local, true, omit).map(|f| f.bytes).map_err(|f| Failure {
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
    /// ([`RequestDiagnostics::failed`]). `agent` must already enforce `initial` in its resolver;
    /// the worker selects that pool before dispatch. Redirects can only narrow it further.
    pub(super) fn fetch_answer(
        agent: &ureq::Agent,
        url: &str,
        initial: Reach,
        detail: bool,
        omit: &'static [u16],
    ) -> Result<Fetched, Failure> {
        fetch_answer_with_public(agent, shared_public_agent(), url, initial, detail, omit)
    }

    /// The request path with both pools explicit, so redirect DNS is tested without live DNS.
    fn fetch_answer_with_public(
        agent: &ureq::Agent,
        public: &ureq::Agent,
        url: &str,
        initial: Reach,
        detail: bool,
        omit: &'static [u16],
    ) -> Result<Fetched, Failure> {
        use std::io::Read;
        let mut carried_login = false;
        // Local file failures are not network traffic.
        let local = crate::mbtiles::is_tile(url) || url.starts_with(LOCAL);
        let mut diagnostics = super::RequestDiagnostics::new(url, "GET");
        diagnostics.omit_statuses(omit);
        let answer = (|| -> Result<Fetched, Failure> {
            super::check_current()?;
            if crate::mbtiles::is_tile(url) {
                return crate::mbtiles::read_local(url).map(|bytes| Fetched { bytes, origin: Origin::named(url) }).map_err(Failure::from);
            }
            if let Some(local) = url.strip_prefix(LOCAL) {
                let (path, range) = super::split_range(local);
                let (a, b) = range.ok_or("a local read without a range")?;
                return crate::pmtiles::read_file(path, a, b - a + 1)
                    .map(|bytes| Fetched { bytes, origin: Origin::named(url) })
                    .map_err(Failure::from);
            }
            let (url, limit) = super::split_download_limit(url);
            let max = limit.unwrap_or(MAX_BYTES);
            let (url, mut tail) = super::split_tail(url);
            let (url, mut range) = super::split_range(url);
            // The last bytes asked for, while the file's length is asked for instead (see below).
            let mut measuring: Option<u64> = None;
            // Explicit service logins outrank implicit S3 endpoint signing.
            let prepared = super::prepare_get(url)?;
            Origin { url: prepared.url.clone(), reach: initial }.check(&prepared.url)?;
            let mut at = prepared.url.clone();
            let mut ceiling = initial;
            let first_origin = crate::logins::origin(&at);
            let mut same_origin = true;
            let mut redirects = 0;
            let mut resp = loop {
                // The login goes to its own origin only: once a redirect leaves it, never again.
                let bound = crate::logins::inherit(url, &at);
                let login = if !prepared.signed && same_origin { crate::logins::apply(&bound)? } else { None };
                carried_login |= login.is_some();
                let sending = if ceiling == Reach::Public && initial != Reach::Public { public } else { agent };
                let mut req = sending.get(login.as_ref().map_or(at.as_str(), |a| a.url.as_str()));
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
                // Narrowing the connection pool must retain the caller's download timeout.
                let req = if sending.config().timeouts().global == agent.config().timeouts().global {
                    req
                } else {
                    req.config().timeout_global(agent.config().timeouts().global).build()
                };
                let (mut resp, reached) = connected(|| req.call()).map_err(unanswered)?;
                ceiling = ceiling.min(reached);
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
                        None => return Err(rustgis_data::diagnostics::message_with_address(url, super::NO_RANGES).into()),
                    }
                }
                if status >= 400 {
                    // A signed request or any hop carrying a login makes the refusal status-only:
                    // a server may echo credentials even after a cross-origin redirect or revocation.
                    return Err(if detail && !prepared.signed && !carried_login {
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
                // Where a redirect leads is the server's choice, not the user's. A server can echo
                // a login in its chosen destination, even after revocation.
                Origin { url: at.clone(), reach: ceiling }
                    .check(&next)
                    .map_err(|e| if carried_login { "redirect destination is not allowed".to_string() } else { e })?;
                // Never from HTTPS down to plain HTTP, however either scheme is written.
                if crate::address::is_https(&at) && !crate::address::is_https(&next) {
                    return Err(if carried_login {
                        "a redirect from https to http".into()
                    } else {
                        rustgis_data::diagnostics::message_with_address(&next, "a redirect from https to http").into()
                    });
                }
                redirects += 1;
                if redirects > 5 {
                    return Err("too many redirects".into());
                }
                same_origin &= crate::logins::origin(&next) == first_origin;
                at = next;
            };
            let retained = if same_origin || crate::s3::split_anonymous(url).1 { crate::address::inherit(url, &at)? } else { at };
            let origin = Origin { url: retained, reach: ceiling };
            if resp.status().as_u16() == 204 {
                return Ok(Fetched { bytes: Vec::new(), origin });
            }
            // A range asked for and the whole file answered (200): fine for a range from the start
            // that holds the whole file (a small archive's header read), cut to the range; otherwise
            // the server doesn't do ranges.
            let whole = range.is_some() && resp.status().as_u16() != 206;
            if let Some((a, _)) = range.filter(|_| whole)
                && a > 0
            {
                return Err(rustgis_data::diagnostics::message_with_address(url, super::NO_RANGES).into());
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
                    return Err(rustgis_data::diagnostics::message_with_address(url, super::NO_RANGES).into());
                }
                if buf.len() > max {
                    return Err(format!("larger than {max} bytes").into());
                }
                return Ok(Fetched { bytes: buf, origin });
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
                    return Err(rustgis_data::diagnostics::message_with_address(url, super::NO_RANGES).into());
                }
                buf.truncate(usize::try_from(b - a + 1).unwrap_or(usize::MAX));
            }
            Ok(Fetched { bytes: buf, origin })
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

    /// Keeps the destination ceiling with queued work; the web build requires its own fetcher.
    pub(super) fn fetch_reached(url: String, _ceiling: crate::address::Reach, done: super::FetchedDone, _omit: &'static [u16]) {
        done(Err(rustgis_data::diagnostics::message_with_address(&url, "this build has no way to fetch tiles")));
    }

    pub(super) fn fetch(url: String, done: Done, _omit: &'static [u16]) {
        done(Err(rustgis_data::diagnostics::message_with_address(&url, "this build has no way to fetch tiles")));
    }

    pub(super) fn download_within(url: String, _timeout: std::time::Duration, _omit: &'static [u16], done: DoneDetailed) {
        let message = rustgis_data::diagnostics::message_with_address(&url, "this build has no way to fetch tiles");
        done(Err(crate::failure::Failure::new(&url, message)));
    }

    pub(super) fn download_public_within(url: String, timeout: std::time::Duration, done: DoneDetailed) {
        download_within(url, timeout, &[], done);
    }

    pub(super) fn post(url: String, _body: String, done: Done) {
        done(Err(rustgis_data::diagnostics::message_with_address(&url, "this build has no way to send requests")));
    }

    pub(super) fn head(url: String, done: super::HeadDone) {
        done(Err(rustgis_data::diagnostics::message_with_address(&url, "this build has no way to send requests")));
    }

    pub(super) fn fetch_local(url: String, done: Done) {
        let (path, _) = super::split_range(&url);
        done(Err(format!("{path}: the web app reads PMTiles archives from web addresses only")));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn policy_refusals_group_paths_without_evicting_other_diagnostics() {
        let _turn = turn();
        let history = rustgis_data::diagnostics::History::default();
        let _capture = history.enter();
        for i in 0..20 {
            let url = format!("http://127.0.0.1/{i}/0/0");
            for asked in [
                url.clone(),
                crate::s3::anonymous(&url),
                rustgis_data::s3_address::bind("0123456789abcdef", "http://127.0.0.1", &url).unwrap(),
                rustgis_data::s3_address::bind("0123456789abcdef", "http://127.0.0.1", &format!("s3://fixture-bucket/{i}/0/0")).unwrap(),
            ] {
                let reason = crate::address::check_reach(&asked, crate::address::Reach::Public).unwrap_err();
                note_refusal(&asked, &reason);
            }
        }
        assert_eq!(history.records().len(), 1, "expanded paths must share a policy diagnostic");
        assert_eq!(history.records()[0].network.as_ref().unwrap().url, "http://127.0.0.1");
        assert_eq!(history.records()[0].message, "a document on the web can't point into the local network");
        note_refusal("http://127.0.0.1/other", "different refusal");
        note_refusal("http://127.0.0.2/0", "different refusal");
        assert_eq!(history.records().len(), 3);
        history.clear();
        note_refusal("http://127.0.0.1/0", "different refusal");
        assert_eq!(history.records().len(), 1);
    }
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

    #[test]
    fn archive_reload_excludes_old_decodes_and_keeps_other_sources() {
        let _turn = crate::cache::turn();
        let archive = "/cache-generation-regression.pmtiles";
        let tile = format!("pmtiles:0/0/0@{archive}");
        let other = "mbtiles:0/0/0@/cache-generation-other.mbtiles";
        let old_epoch = archive_epoch(archive);
        let mut c = Cache::default();
        keep(&mut c, &tile, Entry::Ready(Arc::new(Payload::Json(serde_json::json!("old"))), 64, None));
        keep(&mut c, other, Entry::Ready(Arc::new(Payload::Empty), 64, None));
        let other_payload = match &c.slots[other].entry {
            Entry::Ready(payload, _, _) => payload.clone(),
            _ => panic!("ready tile"),
        };
        let pending = format!("pmtiles:1/0/0@{archive}");
        c.slots.insert(
            pending.clone(),
            Slot {
                entry: Entry::Pending,
                ceiling: crate::address::Reach::Local,
                other: None,
                used: 0,
                asked: Instant::now(),
                epoch: old_epoch,
            },
        );
        reload_archive(archive);
        c.forget_archive(archive);
        assert!(!c.slots.contains_key(&tile) && !c.slots.contains_key(&pending));
        assert_eq!(c.bytes, 64);
        let fresh_epoch = archive_epoch(archive);
        c.slots.insert(
            tile.clone(),
            Slot {
                entry: Entry::Pending,
                ceiling: crate::address::Reach::Local,
                other: None,
                used: 0,
                asked: Instant::now(),
                epoch: fresh_epoch,
            },
        );
        assert!(
            if_archive_current(&tile, old_epoch, || {
                keep(&mut c, &tile, Entry::Ready(Arc::new(Payload::Json(serde_json::json!("stale"))), 64, None));
            })
            .is_none()
        );
        assert!(matches!(c.slots[&tile].entry, Entry::Pending));
        if_archive_current(&tile, fresh_epoch, || {
            keep(&mut c, &tile, Entry::Ready(Arc::new(Payload::Json(serde_json::json!("fresh"))), 64, None));
        });
        assert!(matches!(&c.slots[&tile].entry, Entry::Ready(p, _, _) if matches!(p.as_ref(), Payload::Json(v) if v == "fresh")));
        assert!(matches!(&c.slots[other].entry, Entry::Ready(p, _, _) if Arc::ptr_eq(p, &other_payload)));
        assert_eq!(resource_of(other), "/cache-generation-other.mbtiles");
    }

    #[test]
    fn delayed_archive_answers_cannot_replace_new_pixels_or_remove_new_pending_reads() {
        let _turn = crate::cache::turn();
        for scheme in ["pmtiles", "mbtiles"] {
            let archive = format!("/delayed-answer.{scheme}");
            let key = format!("{scheme}:0/0/0@{archive}");
            let old_epoch = archive_epoch(&archive);
            let mut old_picture = Pixmap::new(1, 1).unwrap();
            old_picture.fill(tiny_skia::Color::from_rgba8(255, 0, 0, 255));
            // Hold the production completion object after decoding, as an asynchronous decoder
            // does. No worker timing or process-wide fetcher replacement is involved.
            IN_FLIGHT.fetch_add(2, Ordering::Relaxed);
            let late = Answer {
                key: key.clone(),
                host: "delayed-answer.test".into(),
                epoch: old_epoch,
                asked_at: crate::mbtiles::epoch(),
                entry: Some(Entry::Ready(Arc::new(Payload::Image(old_picture)), 4, None)),
            };
            let unanswered = Answer {
                key: key.clone(),
                host: "delayed-answer.test".into(),
                epoch: old_epoch,
                asked_at: crate::mbtiles::epoch(),
                entry: None,
            };
            reload_archive(&archive);
            let current = archive_epoch(&archive);
            lock().slots.insert(
                key.clone(),
                Slot {
                    entry: Entry::Pending,
                    ceiling: crate::address::Reach::Local,
                    other: None,
                    used: 0,
                    asked: Instant::now(),
                    epoch: current,
                },
            );
            drop(unanswered);
            assert!(matches!(lock().slots.get(&key).map(|s| &s.entry), Some(Entry::Pending)));
            let mut fresh_picture = Pixmap::new(1, 1).unwrap();
            fresh_picture.fill(tiny_skia::Color::from_rgba8(0, 255, 0, 255));
            let fresh = Arc::new(Payload::Image(fresh_picture));
            store(&key, Entry::Ready(fresh.clone(), 4, None));
            drop(late);
            assert!(Arc::ptr_eq(&peek(&key).unwrap(), &fresh), "an old {scheme} decode replaced the new pixels");
            reload_archive(&archive);
        }
    }

    #[test]
    fn pmtiles_names_that_resemble_cog_looks_reload_whole() {
        let _turn = turn();
        for suffix in ["cube=1", "dequantize=1"] {
            let archive = format!("/archive-look#{suffix}.pmtiles");
            let key = format!("pmtiles:0/0/0@{archive}");
            let mut old_picture = Pixmap::new(1, 1).unwrap();
            old_picture.fill(tiny_skia::Color::from_rgba8(255, 0, 0, 255));
            let old = Arc::new(Payload::Image(old_picture));
            store(&key, Entry::Ready(old.clone(), 4, None));
            IN_FLIGHT.fetch_add(1, Ordering::Relaxed);
            let late = Answer {
                key: key.clone(),
                host: "archive-look.test".into(),
                epoch: archive_epoch(&key),
                asked_at: crate::mbtiles::epoch(),
                entry: Some(Entry::Ready(old, 4, None)),
            };
            reload_archive(&archive);
            assert!(peek(&key).is_none(), "reload retained old pixels for {archive}");
            let mut fresh_picture = Pixmap::new(1, 1).unwrap();
            fresh_picture.fill(tiny_skia::Color::from_rgba8(0, 255, 0, 255));
            let fresh = Arc::new(Payload::Image(fresh_picture));
            store(&key, Entry::Ready(fresh.clone(), 4, None));
            drop(late);
            assert!(Arc::ptr_eq(&peek(&key).unwrap(), &fresh), "old completion replaced fresh pixels for {archive}");
            forget(&archive);
            assert!(peek(&key).is_none(), "forget retained pixels for {archive}");
        }
    }

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
        let revision = archive_epoch(&key);
        lock().slots.insert(
            key.clone(),
            Slot {
                entry: Entry::Pending,
                ceiling: crate::address::Reach::Local,
                other: None,
                used: 0,
                asked: Instant::now(),
                epoch: revision,
            },
        );
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
        let revision = archive_epoch(&key);
        lock().slots.insert(
            key.clone(),
            Slot {
                entry: Entry::Failed("old file".into(), Instant::now()),
                ceiling: crate::address::Reach::Local,
                other: None,
                used: 0,
                asked: Instant::now(),
                epoch: revision,
            },
        );
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
        let revision = archive_epoch(&key);
        lock().slots.insert(
            key.clone(),
            Slot {
                entry: Entry::Pending,
                ceiling: crate::address::Reach::Local,
                other: None,
                used: 0,
                asked: Instant::now(),
                epoch: revision,
            },
        );
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
        lock().slots.insert(
            key.clone(),
            Slot {
                entry: Entry::Pending,
                ceiling: crate::address::Reach::Local,
                other: None,
                used: 0,
                asked: Instant::now(),
                epoch: revision,
            },
        );
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

    #[test]
    fn network_diagnostics_preserve_known_secret_address_failure_reasons() {
        use rustgis_data::diagnostics::{Filter, History};
        for url in [
            "https://diagnostics-secret.test/data?token=hidden",
            "https://diagnostics-secret.test/data?%2573ig=hidden unescaped hidden",
            "https://alice:hidden@diagnostics-secret.test/data#%256bey=hidden unescaped hidden",
        ] {
            let history = History::default();
            let _scope = history.enter();
            let mut diagnostics = RequestDiagnostics::new(url, "GET");
            diagnostics.answered(503);
            diagnostics.failed(&Failure::new(url, format!("{url}: HTTP 503 Service Unavailable")));
            let records = history.records();
            assert_eq!(records.len(), 1);
            assert!(records[0].message.contains("HTTP 503 Service Unavailable"), "{records:?}");
            assert_eq!(rustgis_data::diagnostics::redact(&records[0].message), records[0].message);
            let report = history.report(Filter::All);
            assert!(report.contains("HTTP 503 Service Unavailable"), "{report}");
            assert!(!report.contains("hidden") && !report.contains("alice"), "{report}");
        }
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

    /// The same off the frame ([`download_public_limited`] without a fetcher installed, called
    /// directly: other tests here install theirs): one that resolves locally is refused there.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn public_downloads_off_the_frame_refuse_names_that_resolve_locally() {
        let _turn = turn();
        let (tx, rx) = std::sync::mpsc::channel();
        let url = download_address("http://localhost:9/picture.png", 1024);
        native::download_public_within(url, std::time::Duration::from_secs(5), Box::new(move |r| drop(tx.send(r))));
        let err = rx.recv_timeout(std::time::Duration::from_secs(30)).unwrap().unwrap_err().message;
        assert!(err.contains("resolves to this computer or the local network"), "{err}");
    }

    /// A public download asked for by a request that has since ended sends nothing, as any
    /// download off the frame doesn't.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn public_downloads_off_the_frame_send_nothing_once_their_request_ended() {
        let (tx, rx) = std::sync::mpsc::channel();
        let url = download_address("https://example.org/picture.png", 1024);
        with_still(Arc::new(|| false), || {
            native::download_public_within(url, std::time::Duration::from_secs(5), Box::new(move |r| drop(tx.send(r))));
        });
        let err = rx.recv_timeout(std::time::Duration::from_secs(30)).unwrap().unwrap_err().message;
        assert!(err.contains("cancelled before it was sent"), "{err}");
    }

    /// An installed fetcher (the browser's) is handed the internet-only ceiling for a public
    /// download, and an answer that came from the local network is refused.
    #[test]
    fn public_downloads_through_a_fetcher_keep_the_public_ceiling() {
        use crate::address::{Origin, Reach};
        let _turn = turn();
        let ceilings = Arc::new(Mutex::new(Vec::new()));
        let sent = ceilings.clone();
        set_fetcher_with_origin(move |_, ceiling, done, _| {
            sent.lock().unwrap().push(ceiling);
            done(Ok(Fetched { bytes: vec![1], origin: Origin { url: "https://pictures.example/a.png".into(), reach: Reach::Local } }));
        });
        let (tx, rx) = std::sync::mpsc::channel();
        let public = tx.clone();
        download_public_limited("https://pictures.example/a.png", 1024, Box::new(move |r| drop(public.send(r))));
        let err = rx.recv_timeout(std::time::Duration::from_secs(30)).unwrap().unwrap_err();
        assert!(err.contains("exceeds its request's network reach"), "{err}");
        download_limited("https://pictures.example/a.png", 1024, Box::new(move |r| drop(tx.send(r))));
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(30)).unwrap().unwrap(), vec![1]);
        assert_eq!(*ceilings.lock().unwrap(), [Reach::Public, Reach::Local]);
    }

    /// A picture for the pop-up is given up in the browser when it stalls, as a tile is; a
    /// download the user named, with or without a smaller budget, is not.
    #[test]
    fn only_downloads_the_user_named_wait_out_a_stall() {
        let _turn = turn();
        let handed = Arc::new(Mutex::new(Vec::new()));
        let sent = handed.clone();
        set_fetcher_with_origin(move |url, ceiling, done, _| {
            sent.lock().unwrap().push(gives_up_when_stalled(&url, ceiling));
            done(Err("not fetched".into()));
        });
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let answered = || -> Done {
            let tx = tx.clone();
            Box::new(move |_| {
                let _ = tx.send(());
            })
        };
        let url = "https://pictures.example/a.png";
        download_public_limited(url, 1024, answered());
        download_limited(url, 1024, answered());
        download(url, answered());
        for _ in 0..3 {
            rx.recv_timeout(std::time::Duration::from_secs(30)).unwrap();
        }
        assert_eq!(*handed.lock().unwrap(), [true, false, false]);
        assert!(gives_up_when_stalled("https://tiles.example/1/2/3.png", crate::address::Reach::Local));
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
    pub(super) fn serve(answers: Vec<String>) -> String {
        serve_bytes(answers.into_iter().map(String::into_bytes).collect())
    }

    /// Both responses on one owned keep-alive socket retain its actual local provenance,
    /// including when the URL's hostname alone would classify it as public.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn reused_native_connection_keeps_its_origin() {
        use std::io::Write;
        for named in [false, true] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                for i in 0..2 {
                    assert!(read_request(&mut stream).starts_with("GET /"), "both requests must use the accepted socket");
                    let connection = if i == 0 { "keep-alive" } else { "close" };
                    write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: {connection}\r\n\r\n{{}}").unwrap();
                }
            });
            let agent = native::checked_agent(native::agent().config().clone(), LocalAnswer(address), false);
            let host = if named { format!("reuse.public.example:{}", address.port()) } else { address.to_string() };
            for path in ["first", "second"] {
                let url = format!("http://{host}/{path}");
                let answer = native::fetch_answer(&agent, &url, crate::address::Reach::Local, false, &[]).unwrap();
                assert_eq!(answer.origin.reach, crate::address::Reach::Local);
                assert_eq!(answer.origin.url, url);
                assert_eq!(answer.bytes, b"{}");
            }
            server.join().unwrap();
        }
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

    #[test]
    fn get_preparation_keeps_explicit_service_authority_over_ambient_s3() {
        let _turn = crate::cache::turn();
        use crate::{
            logins::{self, Method},
            s3,
        };
        let base = "https://get-preparation-login.example";
        let name = "get-preparation-ambient-s3";
        s3::connect(s3::Connection {
            name: name.into(),
            endpoint: Some(base.into()),
            region: s3::DEFAULT_REGION.into(),
            buckets: Vec::new(),
            credentials: Some(s3::Credentials {
                access_key_id: "PREPARATIONKEY".into(),
                secret_access_key: "preparation-s3-secret".into(),
                session_token: Some("preparation-s3-token".into()),
            }),
        })
        .unwrap();
        let plain = format!("{base}/service%2Fpath?query=a%20b");
        assert!(prepare_get(&plain).unwrap().signed);
        let binding =
            logins::activate(base, "get-preparation/service", Method::Header("Authorization".into()), "Bearer service-secret", false)
                .unwrap();
        let bound = logins::bind(&binding, &plain).unwrap();
        // This is the shared preparation used before browser RequestInit construction:
        // no S3 canonicalization or signing headers survive the explicit binding.
        let prepared = prepare_get(&bound).unwrap();
        assert_eq!(prepared.url, plain);
        assert!(!prepared.signed);
        assert!(prepared.headers.is_empty());
        let target = logins::inherit(&bound, &prepared.url);
        let login = logins::apply(&target).unwrap().unwrap();
        assert_eq!(login.url, plain);
        assert_eq!(login.header, Some(("Authorization".into(), "Bearer service-secret".into())));
        // Preparation itself carries no credential or authority: after revocation both
        // fresh preparation and send-time application fail instead of using ambient S3.
        assert!(logins::deactivate(&binding));
        assert!(prepare_get(&bound).unwrap_err().contains("no longer active"));
        assert!(logins::apply(&target).is_err());
        assert!(prepare_get(&plain).unwrap().signed);
        assert!(s3::disconnect(name));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn explicit_service_login_beats_ambient_s3_on_native_get_and_redirect() {
        let _turn = crate::cache::turn();
        use crate::{
            logins::{self, Method},
            s3,
        };
        let ok = b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ntile".to_vec();
        let redirect = b"HTTP/1.1 302 Found\r\nLocation: /next\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec();
        let (base, asked) = serve_recorded(vec![ok.clone(), redirect.clone(), ok.clone(), redirect, ok]);
        let name = "native-get-ambient-s3";
        s3::connect(s3::Connection {
            name: name.into(),
            endpoint: Some(base.clone()),
            region: s3::DEFAULT_REGION.into(),
            buckets: Vec::new(),
            credentials: Some(s3::Credentials {
                access_key_id: "NATIVEGETKEY".into(),
                secret_access_key: "native-get-s3-secret".into(),
                session_token: Some("native-get-s3-token".into()),
            }),
        })
        .unwrap();
        let agent = native::agent();
        let plain = format!("{base}/start");
        assert_eq!(native::get(&agent, &plain, &[]).unwrap(), b"tile");
        let received = asked.recv_timeout(Duration::from_secs(5)).unwrap().to_ascii_lowercase();
        assert!(received.contains("authorization: aws4-hmac-sha256 credential=nativegetkey/"));
        assert!(received.contains("x-amz-security-token: native-get-s3-token\r\n"));
        for (header, value) in
            [("Authorization", logins::basic("service-user", "service-password")), ("X-Service-Key", "explicit-service-key".to_string())]
        {
            let binding = logins::activate(&base, "native-get/service", Method::Header(header.into()), &value, true).unwrap();
            let bound = logins::bind(&binding, &plain).unwrap();
            assert_eq!(native::get(&agent, &bound, &[]).unwrap(), b"tile");
            for path in ["/start", "/next"] {
                let received = asked.recv_timeout(Duration::from_secs(5)).unwrap().to_ascii_lowercase();
                assert!(received.starts_with(&format!("get {path} http/1.1\r\n")), "{received}");
                assert!(received.contains(&format!("{}: {}\r\n", header.to_ascii_lowercase(), value.to_ascii_lowercase())), "{received}");
                assert!(!received.contains("aws4-hmac-sha256") && !received.contains("x-amz-"), "{received}");
                assert!(!received.contains("nativegetkey") && !received.contains("native-get-s3-token"), "{received}");
            }
            assert!(logins::deactivate(&binding));
            let failure = native::get(&agent, &bound, &[]).unwrap_err();
            assert!(failure.contains("no longer active"), "{failure}");
        }
        assert!(s3::disconnect(name));
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

    /// GET keeps refusals private after an authenticated redirect, even after login revocation.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn delayed_get_refusal_after_cross_origin_redirect_is_status_only() {
        if isolate_cache_fixture("cache::tests::delayed_get_refusal_after_cross_origin_redirect_is_status_only") {
            return;
        }
        let _turn = crate::cache::turn();
        use crate::failure::Reason;
        use crate::logins::{self, Method};
        use rustgis_data::diagnostics::{Capture, Filter, History};
        use std::io::Write;
        use std::sync::mpsc::channel;
        use std::time::Duration;

        for revoke in [false, true] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let other = format!("http://{}", listener.local_addr().unwrap());
            let secret = "redirect-get-sentinel-8430";
            let body = format!("private refusal echoed {secret}");
            let response = format!(
                "HTTP/1.1 403 Forbidden\r\nx-request-id: {secret}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let (received_tx, received_rx) = channel();
            let (release_tx, release_rx) = channel();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_request(&mut stream);
                assert!(request.starts_with("GET /refused HTTP/1.1\r\n"), "{request}");
                assert!(!request.to_ascii_lowercase().contains("x-key:") && !request.contains(secret), "{request}");
                received_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
                stream.write_all(response.as_bytes()).unwrap();
            });
            let (base, asked) = serve_recorded(vec![
                format!("HTTP/1.1 302 Found\r\nLocation: {other}/refused\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").into_bytes(),
            ]);
            let binding = logins::activate(&base, "redirect-get", Method::Header("X-Key".into()), secret, true).unwrap();
            let plain = format!("{base}/start");
            let url = logins::bind(&binding, &plain).unwrap();
            let history = History::default();
            let _scope = history.enter();
            let capture = Capture::current();
            let worker = std::thread::spawn(move || {
                let _scope = capture.enter();
                native::get_detailed(&native::agent(), &url)
            });
            let request = asked.recv_timeout(Duration::from_secs(10)).unwrap();
            assert!(request.to_ascii_lowercase().contains(&format!("x-key: {secret}\r\n")), "{request}");
            received_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            if revoke {
                assert!(logins::deactivate(&binding));
            }
            release_tx.send(()).unwrap();
            let failure = worker.join().unwrap().unwrap_err();
            server.join().unwrap();
            assert!(failure.answer.is_none(), "{failure:?}");
            if revoke {
                assert!(failure.message.contains("no longer active"), "{failure:?}");
            } else {
                assert_eq!(failure.reason, Reason::Status(403));
                assert_eq!(failure.message, "http status: 403");
                assert!(logins::deactivate(&binding));
            }
            let records = network_records(&history, &plain);
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].network.as_ref().unwrap().status, Some(403));
            assert!(records[0].details.is_empty(), "{records:?}");
            let text = format!("{failure:?}\n{records:?}\n{}", history.report(Filter::All));
            for private in [secret, "x-request-id", "private refusal echoed", binding.as_str()] {
                assert!(!text.contains(private), "{private} reached the failure or diagnostics: {text}");
            }
        }

        // A genuinely anonymous redirect still retains useful refusal headers and body.
        let body = "useful anonymous refusal";
        let (other, asked) = serve_recorded(vec![
            format!(
                "HTTP/1.1 403 Forbidden\r\nx-request-id: anonymous-trace-8430\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .into_bytes(),
        ]);
        let base =
            serve(vec![format!("HTTP/1.1 302 Found\r\nLocation: {other}/refused\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")]);
        let url = format!("{base}/anonymous");
        let history = History::default();
        let _scope = history.enter();
        let failure = native::get_detailed(&native::agent(), &url).unwrap_err();
        let request = asked.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(request.starts_with("GET /refused HTTP/1.1\r\n"), "{request}");
        assert!(!request.to_ascii_lowercase().contains("x-key:"), "{request}");
        assert_eq!(failure.reason, Reason::Status(403));
        assert_eq!(failure.excerpt(), Some(body));
        assert!(
            failure.answer.as_ref().unwrap().headers.iter().any(|(name, value)| name == "x-request-id" && value == "anonymous-trace-8430")
        );
        let records = network_records(&history, &url);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].network.as_ref().unwrap().status, Some(403));
        assert!(records[0].details.contains("x-request-id: anonymous-trace-8430"), "{records:?}");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn delayed_rejected_redirect_after_revocation_does_not_record_login() {
        if isolate_cache_fixture("cache::tests::delayed_rejected_redirect_after_revocation_does_not_record_login") {
            return;
        }
        let _turn = crate::cache::turn();
        use crate::logins::{self, Method};
        use rustgis_data::diagnostics::{Capture, Filter, History};
        use std::io::Write;
        use std::sync::mpsc::channel;
        use std::time::Duration;

        let secret = "redirect-rejection-sentinel";
        let destination = format!("ftp://example.com/{secret}");
        let response = format!("HTTP/1.1 302 Found\r\nLocation: {destination}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (received_tx, received_rx) = channel();
        let (release_tx, release_rx) = channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request(&mut stream);
            assert!(request.to_ascii_lowercase().contains(&format!("x-key: {secret}\r\n")), "{request}");
            received_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            stream.write_all(response.as_bytes()).unwrap();
        });
        let binding = logins::activate(&base, "rejected-redirect", Method::Header("X-Key".into()), secret, true).unwrap();
        let plain = format!("{base}/start");
        let url = logins::bind(&binding, &plain).unwrap();
        let history = History::default();
        let _scope = history.enter();
        let capture = Capture::current();
        let worker = std::thread::spawn(move || {
            let _scope = capture.enter();
            native::get_detailed(&native::agent(), &url)
        });
        received_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(logins::deactivate(&binding));
        release_tx.send(()).unwrap();
        let failure = worker.join().unwrap().unwrap_err();
        server.join().unwrap();
        let records = network_records(&history, &plain);
        assert_eq!(records.len(), 1);
        let text = format!("{failure:?}\n{records:?}\n{}", history.report(Filter::All));
        assert!(!text.contains(secret), "credential reached failure or diagnostics: {text}");

        let (base, _) = serve_recorded(vec![
            format!("HTTP/1.1 302 Found\r\nLocation: {destination}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").into_bytes(),
        ]);
        let failure = native::get_detailed(&native::agent(), &format!("{base}/anonymous")).unwrap_err();
        assert!(failure.message.contains(&destination), "{failure:?}");
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
        store(&ua, Entry::Ready(Arc::new(Payload::Json(serde_json::json!({"a": 1}))), 64, None));
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
        store(&url, Entry::Ready(Arc::new(Payload::Json(serde_json::json!({"old": true}))), 64, None));
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
                store(address, Entry::Ready(Arc::new(Payload::Json(json.clone())), *len, None));
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
        let f = native::get_detailed(&agent, &format!("{base}/f.parquet?token=private-range-token{TAIL}4")).unwrap_err();
        assert!(f.message.contains(super::NO_RANGES), "{}", f.message);
        assert!(!f.message.contains("private-range-token"), "{}", f.message);
        assert!(rustgis_data::diagnostics::redact(&f.message).contains(super::NO_RANGES), "{}", f.message);
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

    /// URL text inside a redirect query remains data on the same document and server.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn query_only_redirects_keep_the_document_origin() {
        let (base, asked) = serve_recorded(
            [
                "HTTP/1.1 302 Found\r\nLocation: ?next=https://other.example/tiles\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                "HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ntile",
            ]
            .into_iter()
            .map(|s| s.as_bytes().to_vec())
            .collect(),
        );
        let answer =
            native::fetch_answer(&native::agent(), &format!("{base}/style.json?old=1"), crate::address::Reach::Local, false, &[]).unwrap();
        assert_eq!(answer.bytes, b"tile");
        assert_eq!(answer.origin.url, format!("{base}/style.json?next=https://other.example/tiles"));
        assert_eq!(answer.origin.reach, crate::address::Reach::Local);
        assert!(asked.recv_timeout(std::time::Duration::from_secs(2)).unwrap().starts_with("GET /style.json?old=1 "));
        assert!(
            asked.recv_timeout(std::time::Duration::from_secs(2)).unwrap().starts_with("GET /style.json?next=https://other.example/tiles ")
        );
    }

    /// A redirected style/TileJSON must use its effective public origin, including from
    /// cache. The fixture routes synthetic public IPs only to its own loopback listener.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn redirected_documents_cannot_keep_a_local_allowance() {
        let _turn = turn();
        use crate::{
            address::Reach,
            source::{Kind, Resolve, Spec},
        };
        for kind in [Kind::Style, Kind::Vector] {
            let tiles = "http://{switch:127.0.0.1}/redirected-document/{z}/{x}/{y}";
            let doc = if kind == Kind::Style {
                serde_json::json!({"version": 8, "sources": {
                    "bad": {"type":"vector", "tiles":[tiles]},
                    "relative": {"type":"vector", "tiles":["tiles/{z}/{x}/{y}"]}
                }, "layers":[{"id":"bad", "type":"fill", "source":"bad", "source-layer":"l"}]})
            } else {
                serde_json::json!({"tiles":[tiles]})
            };
            let destination = "http://document.public.example/final/style.json";
            let base = serve(vec![
                format!("HTTP/1.1 302 Found\r\nLocation: {destination}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"),
                format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{doc}", doc.to_string().len()),
            ]);
            let listener = base.strip_prefix("http://").unwrap().parse().unwrap();
            let agent = native::fixture_agent("203.0.113.1:80".parse().unwrap(), listener, false);
            let asked = format!("{base}/original.json");
            let fetched = native::fetch_answer(&agent, &asked, Reach::Local, false, &[]).unwrap();
            assert_eq!((&*fetched.origin.url, fetched.origin.reach), (destination, Reach::Public));
            let payload = decode(&fetched.bytes, Want::Json).unwrap();
            store(&asked, Entry::Ready(Arc::new(payload), 1024, Some(fetched.origin)));
            let spec = Spec {
                kind,
                url: asked,
                subdomains: Vec::new(),
                min_zoom: 0,
                max_zoom: 14,
                tile_size: 256,
                tms: false,
                attribution: String::new(),
                layers: Vec::new(),
                style_layers: String::new(),
                hidden_layers: Vec::new(),
            };
            let Resolve::Ready(resolved) = crate::source::resolve(&spec) else { panic!("the cached document must resolve") };
            if kind == Kind::Style {
                let relative = resolved.style.sources.iter().find(|(id, _)| id == "relative").unwrap();
                assert!(
                    matches!(&relative.1, crate::style::Source::Vector { tiles, .. } if tiles == &["http://document.public.example/final/tiles/{z}/{x}/{y}"])
                );
            }
            let sent = Arc::new(AtomicUsize::new(0));
            let observed = sent.clone();
            set_fetcher(move |_, done| {
                observed.fetch_add(1, Ordering::SeqCst);
                done(Ok(Vec::new()));
            });
            let mut picture = Pixmap::new(64, 64).unwrap();
            let drawn = crate::draw::draw(&mut picture, &spec, 1.0, &crate::draw::tests::world_frame(), true);
            assert!(drawn.error.as_deref().is_some_and(|e| e.contains("local network")), "{:?}", drawn.error);
            assert_eq!(sent.load(Ordering::SeqCst), 0, "the redirected document must send no local tiles");
            clear_fetcher();
        }
        // Resolved forbidden IPs never reach even the fixture connector. A regression can
        // only contact this test's own listener, not a real metadata or local service.
        let target = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        target.set_nonblocking(true).unwrap();
        for ip in ["169.254.169.254:80", "0.0.0.0:80", "[fe80::1]:80", "[::]:80"] {
            let agent = native::fixture_agent(ip.parse().unwrap(), target.local_addr().unwrap(), false);
            assert!(native::get(&agent, "http://blocked.example/", &[]).unwrap_err().contains("resolves to"));
        }
        assert_eq!(target.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
    }

    /// Draws the actual raster selection path against seeded cache entries, without I/O.
    fn draw_cached_raster(url: &str, from: crate::address::Origin) -> (Pixmap, crate::Drawn) {
        let mut style = crate::style::Style::parse(&serde_json::json!({"version":8,
            "sources":{"s":{"type":"raster","tiles":[url],"maxzoom":0}},
            "layers":[{"id":"r","type":"raster","source":"s"}]}))
        .unwrap();
        if let Some((_, crate::style::Source::Raster { from: origin, .. })) = style.sources.first_mut() {
            *origin = Some(from.clone());
        }
        let resolved = crate::source::Resolved {
            retry: false,
            style,
            from: Some(from),
            sprites: Vec::new(),
            attribution: Vec::new(),
            left_out: Vec::new(),
            made: Mutex::new(Instant::now()),
            waiting: false,
        };
        let frame = crate::draw::tests::world_frame();
        let mut image = Pixmap::new(64, 64).unwrap();
        let mut drawn = crate::Drawn::default();
        crate::draw::draw_layers(Some(&mut image), (64, 64), &frame.extent(64, 64), &resolved, &frame, crate::draw::Pass::All, &mut drawn);
        (image, drawn)
    }

    /// Browser-normalized metadata reaches the document consumer atomically with its bytes.
    /// Descendants carry Public through the real tile queue to the replacement transport,
    /// which refuses a normalized local address before its simulated send.
    #[test]
    fn replacement_transport_destinations_keep_normalized_origins_and_ceiling() {
        let _turn = turn();
        let sent = Arc::new(Mutex::new(Vec::new()));
        let attempts = Arc::new(Mutex::new(Vec::new()));
        let attempted = attempts.clone();
        let observed = sent.clone();
        set_fetcher_with_origin(move |url, ceiling, done, _| {
            attempted.lock().unwrap().push((url.clone(), ceiling));
            let normalized = if url == "https://public.example\\@localhost/style.json" {
                "https://public.example/@localhost/style.json"
            } else if url == "http://local%68ost/tile.png" {
                "http://127.0.0.1/tile.png"
            } else {
                &url
            };
            let check = crate::address::check_reach(normalized, ceiling);
            done(check.map(|_| {
                observed.lock().unwrap().push((normalized.to_string(), ceiling));
                Fetched { bytes: b"{}".to_vec(), origin: crate::address::Origin::named(normalized) }
            }));
        });
        let root = "https://public.example\\@localhost/style.json";
        document(root, None);
        wait(std::time::Duration::from_secs(2));
        let (state, origin) = document(root, None);
        assert!(matches!(state, State::Ready(_)));
        let origin = origin.unwrap();
        assert_eq!(origin.url, "https://public.example/@localhost/style.json");
        assert_eq!(origin.reach, crate::address::Reach::Public);
        let child = "http://local%68ost/tile.png";
        get_tile_from(child, Want::Image, Some(&origin));
        wait(std::time::Duration::from_secs(2));
        assert!(matches!(get_tile_from(child, Want::Image, Some(&origin)), State::Failed(_)));
        assert_eq!(attempts.lock().unwrap().last().unwrap(), &(child.to_string(), crate::address::Reach::Public));
        assert_eq!(attempts.lock().unwrap().len(), 2, "the child reaches the normalization boundary");
        assert_eq!(sent.lock().unwrap().len(), 1, "normalized local child never reaches send");
        assert!(origin.check("http://127.0.0.1/tile.png").is_err());
    }

    /// The replacement transport, which sees every redirect, owns the retained binding.
    /// Returning anonymously to the original server must not restore a dropped login.
    #[test]
    fn replacement_destinations_do_not_restore_dropped_logins() {
        let _turn = turn();
        let plain = "https://redirect-binding.example/style.json";
        let binding = crate::logins::activate(
            "https://redirect-binding.example",
            "redirect-fixture",
            crate::logins::Method::Header("X-Key".into()),
            "fixture",
            false,
        )
        .unwrap();
        let bound = crate::logins::bind(&binding, plain).unwrap();
        for retained in [false, true] {
            let origin = crate::address::Origin::named(plain);
            let origin = if retained { origin.retain_binding(&bound).unwrap() } else { origin };
            set_fetcher_with_origin(move |_, _, done, _| {
                done(Ok(Fetched { bytes: Vec::new(), origin: origin.clone() }));
            });
            let (tx, rx) = std::sync::mpsc::channel();
            fetch_reached(bound.clone(), crate::address::Reach::Local, Box::new(move |r| tx.send(r).unwrap()), &[]);
            let fetched = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap().unwrap();
            let child = crate::template::join(&fetched.origin.url, "tile.json");
            assert_eq!(rustgis_data::login_address::parse(&child).unwrap().is_some(), retained);
        }
        crate::logins::deactivate(&binding);
    }

    /// Local bytes and local failures cannot prevent a public-ceiling fetch of the same name.
    #[test]
    fn cached_destinations_refetch_under_the_requested_reach() {
        use crate::address::{Origin, Reach};
        let _turn = turn();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let sent = calls.clone();
        set_fetcher_with_origin(move |_, ceiling, done, _| {
            sent.lock().unwrap().push(ceiling);
            done(Err("fixture destination refused".into()));
        });
        let from = Origin::named("https://public-document.example/style.json");
        for ready in [true, false] {
            let url = format!("https://split-cache.example/{ready}");
            let entry = if ready {
                Entry::Ready(Arc::new(Payload::Empty), 64, Some(Origin { url: url.clone(), reach: Reach::Local }))
            } else {
                Entry::Failed("http status: 404".into(), Instant::now())
            };
            store(&url, entry);
            assert!(matches!(get_tile_from(&url, Want::Image, Some(&from)), State::Pending), "a restricted fetch must start");
            wait(std::time::Duration::from_secs(2));
            assert!(matches!(get_tile_from(&url, Want::Image, Some(&from)), State::Failed(e) if e == "fixture destination refused"));
        }
        assert_eq!(*calls.lock().unwrap(), [Reach::Public, Reach::Public]);
        let url = "https://split-cache.example/reverse";
        store_from(url, Entry::Failed("restricted refusal".into(), Instant::now()), Reach::Public);
        assert!(matches!(get_tile(url, Want::Image), State::Pending), "explicit requests retry restrictive failures too");
        wait(std::time::Duration::from_secs(2));
        assert_eq!(calls.lock().unwrap().last(), Some(&Reach::Local));
        let url = "https://split-cache.example/reusable";
        store(url, Entry::Ready(Arc::new(Payload::Empty), 0, Some(Origin::named(url))));
        assert!(matches!(get_tile_from(url, Want::Image, Some(&from)), State::Ready(_)));
        assert_eq!(calls.lock().unwrap().len(), 3, "a permitted ready answer is reused");
    }

    /// Concurrent layers with different reach must not evict each other's ready answer or
    /// retry failure on every frame. The transport spy never sends a network request.
    #[test]
    fn shared_destination_reaches_do_not_refetch_each_other() {
        use crate::address::{Origin, Reach};
        let _turn = turn();
        let _alarm = alarm_turn();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let sent = calls.clone();
        set_fetcher_with_origin(move |url, ceiling, done, _| {
            sent.lock().unwrap().push((url.clone(), ceiling));
            done(if url.ends_with("/public") || ceiling == Reach::Local && url.ends_with("/ready") {
                let bytes: &[u8] = if ceiling == Reach::Public { br#"{"reach":"public"}"# } else { br#"{"reach":"local"}"# };
                Ok(Fetched { bytes: bytes.to_vec(), origin: Origin { url, reach: ceiling } })
            } else if url.ends_with("/retry") {
                let seconds = if ceiling == Reach::Local { "120" } else { "30" };
                Err(crate::failure::with_retry_after("http status: 429".into(), 429, Some(seconds)))
            } else {
                Err("fixture destination refused".into())
            });
        });
        let from = Origin::named("https://public-style.example/style.json");
        for case in ["ready", "failures", "retry", "public"] {
            let url = format!("https://shared-reach.example/{case}");
            let mut retry_times = None;
            get(&url, Want::Json);
            wait(std::time::Duration::from_secs(2));
            for _ in 0..3 {
                get_from(&url, Want::Json, Some(&from));
                wait(std::time::Duration::from_secs(2));
                let local = get(&url, Want::Json);
                wait(std::time::Duration::from_secs(2));
                assert_eq!(matches!(local, State::Ready(_)), matches!(case, "ready" | "public"), "the explicit layer keeps its answer");
                let (public, origin) = document(&url, Some(&from));
                if case == "public" {
                    assert_eq!(origin.unwrap().reach, Reach::Public);
                    assert!(matches!(public, State::Ready(p) if matches!(p.as_ref(), Payload::Json(v) if v["reach"] == "public")));
                    let (local, origin) = document(&url, None);
                    assert_eq!(origin.unwrap().reach, Reach::Local);
                    assert!(matches!(local, State::Ready(p) if matches!(p.as_ref(), Payload::Json(v) if v["reach"] == "local")));
                } else {
                    let State::Failed(error) = public else { panic!("the public style must retain its refusal") };
                    if case == "retry" {
                        assert_eq!(retry_after(&error), Some(Duration::from_secs(30)));
                        let State::Failed(error) = get(&url, Want::Json) else { panic!("the explicit layer keeps its cooldown") };
                        assert_eq!(retry_after(&error), Some(Duration::from_secs(120)));
                        let c = lock();
                        let slot = c.slots.get(&url).unwrap();
                        let times = [Reach::Local, Reach::Public].map(|reach| match slot.for_reach(reach) {
                            (Entry::Failed(_, at), stored) if reach == stored => *at,
                            _ => panic!("each reach keeps its own failed-at time"),
                        });
                        if let Some(previous) = retry_times {
                            assert_eq!(times, previous, "alternating reads must not postpone either retry");
                        }
                        retry_times = Some(times);
                    }
                }
            }
            let sent = calls.lock().unwrap();
            assert_eq!(sent.iter().filter(|(u, _)| u == &url).count(), 2, "one fetch under each reach, not one per frame: {sent:?}");
            drop(sent);
            if case == "retry" {
                // Move each recorded failure just past its own original wait, without sleeping
                // or changing production clocks. The other reach's cooldown remains usable.
                for (reach, count) in [(Reach::Public, 3), (Reach::Local, 4)] {
                    let from = (reach == Reach::Public).then_some(&from);
                    let State::Failed(error) = get_from(&url, Want::Json, from) else { panic!("the cooldown is still active") };
                    let expired = Instant::now() - retry_wait(&error, RETRY) - Duration::from_secs(1);
                    store_from(&url, Entry::Failed(error, expired), reach);
                    assert!(matches!(get_from(&url, Want::Json, from), State::Pending), "retry at this reach's deadline");
                    wait(std::time::Duration::from_secs(2));
                    assert_eq!(calls.lock().unwrap().iter().filter(|(u, _)| u == &url).count(), count);
                }
            }
        }
    }

    /// A pending or dropped restricted fetch cannot blank a completed explicit layer.
    /// Forgetting a resource or clearing the cache removes both retained outcomes.
    #[test]
    fn a_restricted_read_preserves_the_explicit_answer_while_pending_or_dropped() {
        use crate::address::{Origin, Reach};
        let _turn = turn();
        let (tx, rx) = std::sync::mpsc::channel();
        set_fetcher_with_origin(move |url, ceiling, done, _| {
            if ceiling == Reach::Public {
                tx.send(done).unwrap();
            } else {
                done(Ok(Fetched { bytes: b"{}".to_vec(), origin: Origin { url, reach: Reach::Local } }));
            }
        });
        let from = Origin::named("https://public-style.example/style.json");
        for clear_all in [false, true] {
            let url = format!("https://pending-reach.example/{clear_all}");
            get(&url, Want::Json);
            wait(std::time::Duration::from_secs(2));
            assert!(matches!(get_from(&url, Want::Json, Some(&from)), State::Pending));
            let pending = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
            assert!(matches!(get(&url, Want::Json), State::Ready(_)));
            assert!(peek(&url).is_some());
            assert!(peek_from(&url, Some(&from)).is_none(), "public readers cannot see retained local bytes");
            drop(pending);
            wait(std::time::Duration::from_secs(2));
            assert!(matches!(get(&url, Want::Json), State::Ready(_)), "dropping the public fetch preserves the local layer");
            assert!(matches!(get_from(&url, Want::Json, Some(&from)), State::Pending));
            rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap()(Err("fixture destination refused".into()));
            wait(std::time::Duration::from_secs(2));
            assert!(matches!(get_from(&url, Want::Json, Some(&from)), State::Failed(_)));
            if clear_all {
                clear();
            } else {
                forget(&url);
            }
            assert!(peek(&url).is_none(), "the retained local answer was forgotten");
            assert!(matches!(get_from(&url, Want::Json, Some(&from)), State::Pending), "the retained public failure was forgotten");
            rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap()(Err("fixture destination refused".into()));
            wait(std::time::Duration::from_secs(2));
        }
    }

    /// The native response's effective origin keeps the selected saved S3 account for
    /// relative documents, without transferring that binding to another server.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn native_destination_origins_keep_saved_s3_selection() {
        let _turn = turn();
        let base = serve(vec!["HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}".into()]);
        let binding = "0123456789abcdef";
        let name = "destination-origin-binding";
        crate::s3::connect_saved(
            crate::s3::Connection {
                name: name.into(),
                endpoint: Some(base.clone()),
                region: "us-east-1".into(),
                buckets: vec!["bucket".into()],
                credentials: None,
            },
            binding,
            "destination-account",
        )
        .unwrap();
        let bound = rustgis_data::s3_address::bind(binding, &base, "s3://bucket/styles/style.json").unwrap();
        let got = native::fetch_answer(&native::agent(), &bound, crate::address::Reach::Local, false, &[]).unwrap();
        let reported = crate::address::Origin::named(&crate::s3::prepare(&bound).unwrap().url).retain_binding(&bound).unwrap();
        assert_eq!(reported, got.origin, "the no-redirect replacement adapter retains the same S3 selection");
        let child = crate::template::join(&got.origin.url, "tiles.json");
        let selected = rustgis_data::s3_address::parse(&child).unwrap();
        assert!(selected.is_some(), "a relative document must retain the selected S3 binding");
        assert_eq!(selected.unwrap().binding, binding);
        assert_eq!(crate::s3::prepare(&child).unwrap().url, format!("{base}/bucket/styles/tiles.json"));
        let external = crate::template::join(&got.origin.url, "https://other-origin.example/tiles.json");
        assert!(rustgis_data::s3_address::parse(&external).unwrap().is_none());
        crate::s3::disconnect(name);
        // Joining is independent of the live login registry. A removed selection keeps the
        // child's address and fails preparation, rather than substituting the parent.
        assert_eq!(crate::template::join(&got.origin.url, "tiles.json"), child);
        assert!(crate::s3::prepare(&child).is_err());
    }

    /// A cached DNS refusal reaches the drawing, while ordinary missing tiles stay quiet.
    #[test]
    fn cached_destination_failures_reach_raster_drawing() {
        let _turn = turn();
        let from = crate::address::Origin::named("https://public-document.example/style.json");
        set_fetcher_with_origin(|_, _, done, _| done(Err("fixture resolves to the local network".into())));
        for (case, reach) in
            [("local", crate::address::Reach::Local), ("public", crate::address::Reach::Public), ("missing", crate::address::Reach::Public)]
        {
            let url = format!("http://cached-draw.example/{case}");
            let entry = if case == "missing" {
                Entry::Failed("http status: 404".into(), Instant::now())
            } else {
                let mut image = Pixmap::new(1, 1).unwrap();
                image.fill(tiny_skia::Color::from_rgba8(255, 0, 0, 255));
                Entry::Ready(Arc::new(Payload::Image(image)), 4, Some(crate::address::Origin { url: url.clone(), reach }))
            };
            let ceiling = if case == "local" { crate::address::Reach::Local } else { crate::address::Reach::Public };
            store_from(&url, entry, ceiling);
            draw_cached_raster(&url, from.clone());
            wait(std::time::Duration::from_secs(2));
            let (image, drawn) = draw_cached_raster(&url, from.clone());
            assert_eq!(drawn.error.is_some(), case == "local", "{case}: {:?}", drawn.error);
            assert_eq!(image.pixel(32, 32).unwrap().alpha() > 0, case == "public");
        }
    }

    /// Partial WMTS composites retain both their useful pixels and the refused-component
    /// warning, including the short-lived composite cache hit.
    #[test]
    fn partial_warp_destinations_report_refused_components() {
        use rustgis_data::ogc::warp::{Grid, Matrix, Warp};
        let _turn = turn();
        set_fetcher_with_origin(|_, _, done, _| done(Err("fixture resolves to the local network".into())));
        let warp = Warp {
            epsg: 4326,
            grid: Grid::Tiles(vec![Matrix {
                id: "m".into(),
                left: -180.0,
                top: 90.0,
                cell: 180.0 / 256.0,
                tile_w: 256,
                tile_h: 256,
                cols: 2,
                rows: 1,
            }]),
            area: None,
            systems: Vec::new(),
        };
        let template = warp.address("http://partial-warp.example/{nm}/{ncol}/{nrow}");
        let url = crate::template::expand(&template, crate::template::TileId { z: 0, x: 0, y: 0 }, 256, false, &[]);
        let (base, _, spec) = Warp::parse(&url).unwrap();
        let half = crate::template::HALF_WORLD;
        let plan = spec.plan(base, [-half, -half, half, half], 256, &crate::warp::transformer(4326).unwrap());
        assert_eq!(plan.parts.len(), 2);
        for (i, part) in plan.parts.iter().enumerate() {
            let mut image = Pixmap::new(1, 1).unwrap();
            image.fill(tiny_skia::Color::from_rgba8(255, 0, 0, 255));
            let reach = if i == 0 { crate::address::Reach::Public } else { crate::address::Reach::Local };
            store(
                &part.url,
                Entry::Ready(Arc::new(Payload::Image(image)), 4, Some(crate::address::Origin { url: part.url.clone(), reach })),
            );
        }
        let from = crate::address::Origin::named("https://public-document.example/style.json");
        draw_cached_raster(&url, from.clone());
        wait(std::time::Duration::from_secs(2));
        for _ in 0..2 {
            let (image, drawn) = draw_cached_raster(&url, from.clone());
            assert!(image.pixels().iter().any(|p| p.alpha() > 0), "valid component still draws");
            assert!(drawn.error.as_deref().is_some_and(|e| e.contains("local network")), "{:?}", drawn.error);
        }
    }

    /// A public style's WMS composite must not regain a local allowance for its parts or
    /// reuse a composite made explicitly from local answers. Cache seeding performs no I/O.
    #[test]
    fn warped_destinations_keep_the_document_boundary() {
        let _turn = turn();
        let url = "http://warped-local-destination.example/wms#rustgis-warp=0/0/0;crs=4326;map=east";
        let (base, (z, x, y), warp) = rustgis_data::ogc::warp::Warp::parse(url).unwrap();
        let extent = crate::template::TileId { z, x, y }.extent();
        let transformer = crate::warp::transformer(warp.epsg).unwrap();
        let plan = warp.plan(base, [extent.min_x, extent.min_y, extent.max_x, extent.max_y], 256, &transformer);
        assert!(!plan.parts.is_empty(), "the fixture must require source pictures");
        for part in plan.parts {
            let origin = crate::address::Origin { url: part.url.clone(), reach: crate::address::Reach::Local };
            store(&part.url, Entry::Ready(Arc::new(Payload::Image(Pixmap::new(1, 1).unwrap())), 4, Some(origin)));
        }
        let from = crate::address::Origin::named("https://public-document.example/style.json");
        set_fetcher_with_origin(|_, ceiling, done, _| {
            assert_eq!(ceiling, crate::address::Reach::Public);
            done(Err("fixture resolves to the local network".into()));
        });
        assert!(matches!(get_tile(url, Want::Image), State::Ready(_)), "explicit local composition still works");
        assert!(matches!(get_tile_from(url, Want::Image, Some(&from)), State::Pending), "a cached local composite needs restricted parts");
        wait(std::time::Duration::from_secs(2));
        assert!(
            matches!(get_tile_from(url, Want::Image, Some(&from)), State::Failed(e) if e.contains("local network")),
            "composition must inherit public reach and report its failed restricted fetch"
        );
    }

    /// Replaces only the fixture's public DNS names; all connections, if a check regresses,
    /// can reach only the listener this test just created.
    #[cfg(not(target_arch = "wasm32"))]
    #[derive(Debug)]
    pub(super) struct LocalAnswer(pub(super) std::net::SocketAddr);

    #[cfg(not(target_arch = "wasm32"))]
    impl ureq::unversioned::resolver::Resolver for LocalAnswer {
        fn resolve(
            &self,
            uri: &ureq::http::Uri,
            config: &ureq::config::Config,
            timeout: ureq::unversioned::transport::NextTimeout,
        ) -> Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
            if uri.host().is_some_and(|host| host.ends_with(".example")) {
                let mut answer = self.empty();
                answer.push(self.0);
                Ok(answer)
            } else {
                ureq::unversioned::resolver::DefaultResolver::default().resolve(uri, config, timeout)
            }
        }
    }

    /// GET, HEAD and downloads use the same checked resolver and recheck redirected names.
    /// A refusal must occur before connecting, not after a response or timeout.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn native_destinations_filter_dns_before_connecting() {
        let _turn = turn();
        let target = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        target.set_nonblocking(true).unwrap();
        let answer = target.local_addr().unwrap();
        let public = format!("http://destinations.example:{}/tile", answer.port());
        for config in [native::agent().config().clone(), native::download_agent(std::time::Duration::from_secs(1)).config().clone()] {
            assert!(config.proxy().is_none(), "a proxy must not bypass checked DNS");
            let agent = native::checked_agent(config, LocalAnswer(answer), true);
            assert!(native::get(&agent, &public, &[]).unwrap_err().contains("resolves to"));
            assert!(native::head_with(&agent, &public).unwrap_err().contains("resolves to"));
        }
        assert_eq!(target.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock, "no forbidden connection was attempted");

        let split = serve(vec!["HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ntile".into()]);
        let listener: std::net::SocketAddr = split.strip_prefix("http://").unwrap().parse().unwrap();
        let explicit = format!("http://self-hosted.example:{}/tiles", listener.port());
        let agent = native::checked_agent(native::agent().config().clone(), LocalAnswer(listener), false);
        let fetched = native::fetch_answer(&agent, &explicit, crate::address::Reach::Local, false, &[]).unwrap();
        assert_eq!(fetched.bytes, b"tile");
        assert_eq!(fetched.origin.url, explicit, "preserve the hostname used for TLS and relative references");
        assert_eq!(fetched.origin.reach, crate::address::Reach::Local);

        // Explicit local services still work, including a checked same-origin redirect.
        let local = serve(vec![
            "HTTP/1.1 302 Found\r\nLocation: /final\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
            "HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ntile".into(),
        ]);
        assert_eq!(download_blocking_limited(&local, 4).unwrap(), b"tile");
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
            type Out = Box<dyn Transport>;
            fn connect(&self, _: &ConnectionDetails, chained: Option<In>) -> Result<Option<Self::Out>, ureq::Error> {
                Ok(chained.map(|transport| Claimed(transport).boxed()))
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
        native::fixture_transport_agent(TcpConnector::default().chain(ClaimsTls))
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
        use std::sync::mpsc;
        let _turn = crate::cache::turn();

        fn receive(rx: &mpsc::Receiver<()>) {
            rx.recv_timeout(Duration::from_secs(10)).expect("the test fetcher answered or admitted the request");
        }

        let (completed_tx, completed_rx) = mpsc::channel();
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        set_fetcher(move |url, done| {
            CALLS.fetch_add(1, Ordering::Relaxed);
            if url.ends_with("ok.json") {
                done(Ok(b"[1, 2]".to_vec()))
            } else {
                done(Err("404".into()))
            }
            completed_tx.send(()).unwrap();
        });
        let g = generation();
        assert!(matches!(get("test://cache/ok.json", Want::Json), State::Pending));
        let t = Instant::now();
        while generation() <= g && t.elapsed() < Duration::from_secs(5) {
            wait_for_news(g, Duration::from_secs(5));
        }
        assert!(generation() > g);
        receive(&completed_rx);
        assert!(matches!(get("test://cache/ok.json", Want::Json), State::Ready(_)));
        assert!(peek("test://cache/ok.json").is_some());
        assert!(matches!(get("test://cache/missing.json", Want::Json), State::Pending));
        receive(&completed_rx);
        assert!(matches!(get("test://cache/missing.json", Want::Json), State::Failed(_)));
        assert!(peek("test://cache/never.json").is_none());
        // A request the fetcher dropped is forgotten, not failed: asked for again, it is fetched.
        let (completed_tx, completed_rx) = mpsc::channel();
        set_fetcher(move |url, done| {
            CALLS.fetch_add(1, Ordering::Relaxed);
            if url.ends_with("first.json") {
                done(Err(DROPPED.into()))
            } else {
                done(Ok(b"1".to_vec()))
            }
            completed_tx.send(()).unwrap();
        });
        assert!(matches!(get("test://cache/first.json", Want::Json), State::Pending));
        receive(&completed_rx);
        assert!(matches!(get("test://cache/first.json", Want::Json), State::Pending), "fetched again");
        receive(&completed_rx);
        assert_eq!(CALLS.load(Ordering::Relaxed), 4);

        // A server has a few fetches under way at once; the newest waiting tile goes next (#635).
        static HELD: Mutex<Vec<(String, Done)>> = Mutex::new(Vec::new());
        let (started_tx, started_rx) = mpsc::channel();
        set_fetcher(move |url, done| {
            if url.starts_with("test://held/") {
                HELD.lock().unwrap().push((url, done));
                started_tx.send(()).unwrap();
            } else {
                done(Ok(b"1".to_vec()));
            }
        });
        let n = crate::schedule::PER_HOST + 3;
        for i in 0..n {
            assert!(matches!(get(&format!("test://held/{i}.json"), Want::Json), State::Pending));
        }
        for _ in 0..crate::schedule::PER_HOST {
            receive(&started_rx);
        }
        assert_eq!(HELD.lock().unwrap().len(), crate::schedule::PER_HOST);
        let (completed_url, done) = HELD.lock().unwrap().remove(0);
        done(Ok(b"2".to_vec()));
        receive(&started_rx);
        let held: Vec<String> = HELD.lock().unwrap().iter().map(|(u, _)| u.clone()).collect();
        assert_eq!(held.len(), crate::schedule::PER_HOST);
        assert_eq!(held.last().map(String::as_str), Some(format!("test://held/{}.json", n - 1).as_str()));
        assert!(matches!(get(&completed_url, Want::Json), State::Ready(_)));
        // The rest finish, and so do the waiting ones they make room for.
        for _ in 1..n {
            let (_, done) = loop {
                if let Some(job) = HELD.lock().unwrap().pop() {
                    break job;
                }
                receive(&started_rx);
            };
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
        let (completed_tx, completed_rx) = mpsc::channel();
        set_fetcher(move |url, done| {
            if url.starts_with("test://lost/") {
                SEEN.fetch_add(1, Ordering::Relaxed);
                drop(done);
                completed_tx.send(()).unwrap();
            } else {
                done(Ok(b"1".to_vec()));
            }
        });
        for i in 0..3 * crate::schedule::PER_HOST {
            assert!(matches!(get(&format!("test://lost/{i}.json"), Want::Json), State::Pending));
        }
        for _ in 0..3 * crate::schedule::PER_HOST {
            receive(&completed_rx);
        }
        assert_eq!(SEEN.load(Ordering::Relaxed), 3 * crate::schedule::PER_HOST);
        assert!(peek("test://lost/0.json").is_none());

        // Waiting for news: a fetch never answered (another's, a slow server's) doesn't hold up
        // one waiting for what has come.
        static STUCK: Mutex<Vec<Done>> = Mutex::new(Vec::new());
        let (started_tx, started_rx) = mpsc::channel();
        set_fetcher(move |url, done| {
            if url.starts_with("test://stuck/") {
                STUCK.lock().unwrap().push(done);
                started_tx.send(()).unwrap();
            } else {
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    done(Ok(b"1".to_vec()));
                });
            }
        });
        assert!(matches!(get("test://stuck/0.json", Want::Json), State::Pending));
        receive(&started_rx);
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
        let (completed_tx, completed_rx) = mpsc::channel();
        set_fetcher_omitting(move |url, done, omit| {
            OMITS.lock().unwrap().push((url, omit));
            done(Err("http status: 404".into()));
            completed_tx.send(()).unwrap();
        });
        let _ = get_tile("test://omit/tile.png", Want::Image);
        let _ = get_tile("test://omit/tile.pbf", Want::Vector);
        let _ = get("test://omit/picture.png", Want::Image);
        let _ = get("test://omit/style.json", Want::Json);
        for _ in 0..4 {
            receive(&completed_rx);
        }
        let omits = std::mem::take(&mut *OMITS.lock().unwrap());
        let omit = |name: &str| omits.iter().find(|(u, _)| u.ends_with(name)).map(|(_, o)| *o);
        assert_eq!(omit("tile.png"), Some(&[404, 410][..]));
        assert_eq!(omit("tile.pbf"), Some(&[404, 410][..]));
        assert_eq!(omit("picture.png"), Some(&[][..]));
        assert_eq!(omit("style.json"), Some(&[][..]));
        clear_fetcher();
    }

    #[test]
    fn concurrent_fetch_starts_without_waiting_for_another_threads_launch() {
        let _turn = crate::cache::turn();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        set_fetcher(move |url, done| {
            if url.starts_with("test://slow/") {
                started_tx.send(()).unwrap();
                release_rx.lock().unwrap().recv_timeout(Duration::from_secs(5)).unwrap();
                done(Ok(b"{\"slow\": true}".to_vec()));
            } else {
                done(Ok(b"{\"fast\": true}".to_vec()));
            }
        });
        let thread_a = std::thread::spawn(|| {
            assert!(matches!(get("test://slow/1.json", Want::Json), State::Pending));
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();

        assert!(matches!(get("test://fast/1.json", Want::Json), State::Pending));
        let fast_ready_immediately = matches!(get("test://fast/1.json", Want::Json), State::Ready(_));

        release_tx.send(()).unwrap();
        thread_a.join().unwrap();

        assert!(fast_ready_immediately, "fast fetch on thread B was parked behind thread A's in-flight launch");
        assert!(matches!(get("test://fast/1.json", Want::Json), State::Ready(_)));
    }

    #[test]
    fn fetcher_panic_unwinds_launch_and_releases_queued_replacement() {
        let _turn = crate::cache::turn();
        wait(Duration::from_secs(5));
        assert_eq!(in_flight(), 0, "prior fetches should finish after wait");
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (panic_started_tx, panic_started_rx) = std::sync::mpsc::channel();
        let (panic_trigger_tx, panic_trigger_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let panic_trigger_rx = Arc::new(Mutex::new(panic_trigger_rx));

        let cap = crate::schedule::PER_HOST;
        let rx = release_rx.clone();
        let p_rx = panic_trigger_rx.clone();
        set_fetcher(move |url, done| {
            if url.contains("/slow_") {
                started_tx.send(()).unwrap();
                rx.lock().unwrap().recv_timeout(Duration::from_secs(5)).unwrap();
                done(Ok(b"{\"slow\": true}".to_vec()));
            } else if url.contains("/panic.json") {
                panic_started_tx.send(()).unwrap();
                p_rx.lock().unwrap().recv_timeout(Duration::from_secs(5)).unwrap();
                panic!("simulated fetcher panic during launch");
            } else {
                done(Ok(b"{\"replacement\": true}".to_vec()));
            }
        });

        let mut threads = Vec::new();
        for i in 0..(cap - 1) {
            threads.push(std::thread::spawn(move || {
                let _ = get(&format!("test://samehost/slow_{i}.json"), Want::Json);
            }));
            started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        }

        let panic_thread = std::thread::spawn(|| {
            let _ = get("test://samehost/panic.json", Want::Json);
        });
        panic_started_rx.recv_timeout(Duration::from_secs(5)).unwrap();

        let waiting_url = "test://samehost/waiting.json";
        assert!(matches!(get(waiting_url, Want::Json), State::Pending));

        panic_trigger_tx.send(()).unwrap();
        let _ = panic_thread.join();

        assert!(!lock().slots.contains_key(waiting_url));
        assert_eq!(in_flight(), cap - 1);

        assert!(matches!(get(waiting_url, Want::Json), State::Pending));
        assert!(matches!(get(waiting_url, Want::Json), State::Ready(_)));

        for _ in 0..(cap - 1) {
            release_tx.send(()).unwrap();
        }
        for t in threads {
            t.join().unwrap();
        }

        assert_eq!(in_flight(), 0);
    }

    #[test]
    fn release_unstarted_preserves_retained_outcome_and_bytes() {
        let _turn = crate::cache::turn();
        wait(Duration::from_secs(5));
        assert_eq!(in_flight(), 0, "prior fetches should finish after wait");
        clear();

        let url = "test://retained/tile.json";
        let payload = Arc::new(Payload::Json("{\"retained\": true}".into()));
        let initial_bytes = payload_bytes(&payload);
        {
            let mut c = lock();
            keep_for(&mut c, url, Entry::Ready(payload.clone(), initial_bytes, None), crate::address::Reach::Local);
            assert_eq!(c.bytes, initial_bytes);
        }

        {
            let mut c = lock();
            keep_for(&mut c, url, Entry::Pending, crate::address::Reach::Public);
            assert_eq!(c.bytes, initial_bytes);
            let slot = c.slots.get(url).unwrap();
            assert!(matches!(slot.entry, Entry::Pending));
            assert!(slot.other.is_some());
        }

        let starting_in_flight = in_flight();
        IN_FLIGHT.fetch_add(1, Ordering::Relaxed);
        let mut swept = Vec::new();
        let waiting = {
            let mut q = queue().lock().unwrap_or_else(|e| e.into_inner());
            let mut c = lock();
            let latest = c.latest.unwrap_or_else(Instant::now);
            let job = (Want::Json, Capture::current(), &[] as &'static [u16], 0u64, None, 0u64, crate::address::Reach::Public);
            let w = q
                .push(url.to_string(), job, |u| c.slots.get(u).map(|s| s.asked), latest, &mut swept)
                .expect("job should be scheduled immediately");
            forget_dropped(&mut c, &swept);
            w
        };
        assert_eq!(in_flight(), starting_in_flight + 1);

        release_unstarted(vec![waiting]);
        assert_eq!(in_flight(), starting_in_flight);

        {
            let c = lock();
            let slot = c.slots.get(url).expect("slot should be preserved");
            assert!(matches!(slot.entry, Entry::Ready(..)));
            assert_eq!(slot.ceiling, crate::address::Reach::Local);
            assert!(slot.other.is_none());
            assert_eq!(c.bytes, initial_bytes);
        }
    }

    #[test]
    fn release_unstarted_preserves_newer_pending_mbtiles_request_after_forget() {
        let _turn = crate::cache::turn();
        wait(Duration::from_secs(5));
        assert_eq!(in_flight(), 0, "prior fetches should finish after wait");
        clear();

        let archive = "/race-archive.mbtiles";
        let key = crate::mbtiles::template(archive).replace("{z}", "0").replace("{x}", "0").replace("{y}", "0");
        let old_epoch = crate::mbtiles::epoch();

        // An older unstarted job requested through the queue's normal path before the archive was forgotten
        let starting_in_flight = in_flight();
        IN_FLIGHT.fetch_add(1, Ordering::Relaxed);
        let mut swept = Vec::new();
        let waiting = {
            let mut q = queue().lock().unwrap_or_else(|e| e.into_inner());
            let mut c = lock();
            let latest = c.latest.unwrap_or_else(Instant::now);
            let job = (Want::Vector, Capture::current(), &[] as &'static [u16], 0u64, None, old_epoch, crate::address::Reach::Local);
            let w = q
                .push(key.clone(), job, |u| c.slots.get(u).map(|s| s.asked), latest, &mut swept)
                .expect("job should be scheduled immediately");
            forget_dropped(&mut c, &swept);
            w
        };
        assert_eq!(in_flight(), starting_in_flight + 1);

        // Forget the archive: epoch increments, and old archive tiles are recorded as forgotten
        forget(archive);

        // A newer request arrives after the archive was forgotten, planting a new Pending slot
        {
            let mut c = lock();
            c.slots.insert(
                key.clone(),
                Slot {
                    entry: Entry::Pending,
                    ceiling: crate::address::Reach::Local,
                    other: None,
                    used: 0,
                    asked: Instant::now(),
                    epoch: 0,
                },
            );
        }

        // Release the older unstarted job. It must NOT clear the newer Pending slot.
        release_unstarted(vec![waiting]);
        assert_eq!(in_flight(), starting_in_flight);

        {
            let c = lock();
            let slot = c.slots.get(&key).expect("newer pending slot must be preserved");
            assert!(matches!(slot.entry, Entry::Pending));
        }
    }

    #[test]
    fn release_unstarted_safely_releases_stranded_queued_jobs_when_active_empties() {
        let _turn = crate::cache::turn();
        wait(Duration::from_secs(5));
        assert_eq!(in_flight(), 0, "prior fetches should finish after wait");
        clear();

        let starting_in_flight = in_flight();
        let cap = crate::schedule::PER_HOST;
        let mut swept = Vec::new();
        let mut active_jobs = Vec::new();

        let host = "strandedhost.test";
        // Fill active reservations for this host
        for i in 0..cap {
            let url = format!("test://{host}/active_{i}.json");
            IN_FLIGHT.fetch_add(1, Ordering::Relaxed);
            let mut q = queue().lock().unwrap_or_else(|e| e.into_inner());
            let mut c = lock();
            let latest = c.latest.unwrap_or_else(Instant::now);
            let job = (Want::Json, Capture::current(), &[] as &'static [u16], 0u64, None, 0u64, crate::address::Reach::Public);
            c.slots.insert(
                url.clone(),
                Slot {
                    entry: Entry::Pending,
                    ceiling: crate::address::Reach::Public,
                    other: None,
                    used: 0,
                    asked: Instant::now(),
                    epoch: 0,
                },
            );
            let w = q
                .push(url, job, |u| c.slots.get(u).map(|s| s.asked), latest, &mut swept)
                .expect("active job should be scheduled immediately");
            forget_dropped(&mut c, &swept);
            active_jobs.push(w);
        }

        // Push two more jobs that exceed host capacity and become queued in waiting
        let waiting_1 = format!("test://{host}/waiting_1.json");
        let waiting_2 = format!("test://{host}/waiting_2.json");
        for url in [&waiting_1, &waiting_2] {
            IN_FLIGHT.fetch_add(1, Ordering::Relaxed);
            let mut q = queue().lock().unwrap_or_else(|e| e.into_inner());
            let mut c = lock();
            let latest = c.latest.unwrap_or_else(Instant::now);
            let job = (Want::Json, Capture::current(), &[] as &'static [u16], 0u64, None, 0u64, crate::address::Reach::Public);
            c.slots.insert(
                url.clone(),
                Slot {
                    entry: Entry::Pending,
                    ceiling: crate::address::Reach::Public,
                    other: None,
                    used: 0,
                    asked: Instant::now(),
                    epoch: 0,
                },
            );
            let w = q.push(url.clone(), job, |u| c.slots.get(u).map(|s| s.asked), latest, &mut swept);
            assert!(w.is_none(), "job should be queued in waiting because host is at capacity");
            forget_dropped(&mut c, &swept);
        }

        assert_eq!(in_flight(), starting_in_flight + cap + 2);
        assert_eq!(queue().lock().unwrap().waiting(), 2);

        // When all active unstarted jobs unwind and are released, active reservations drop to 0.
        // The remaining queued jobs must be safely drained and released, rather than stranded.
        release_unstarted(active_jobs);

        assert_eq!(in_flight(), starting_in_flight);
        assert_eq!(queue().lock().unwrap().waiting(), 0);
        assert!(!lock().slots.contains_key(&waiting_1));
        assert!(!lock().slots.contains_key(&waiting_2));
    }
}
