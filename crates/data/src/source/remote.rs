//! Files read in place by their web address, a range of bytes at a time (FlatGeobuf and
//! GeoParquet by HTTP range requests).
//!
//! A [`Remote`] holds the parts of a file fetched so far. Readers ask it for bytes; what it doesn't
//! hold is noted as missing and the read fails. [`drive`] runs such a read, fetches everything it
//! found missing in one round of requests, and runs it again, until it needs nothing more. The
//! same loop works on the desktop and in the browser, where nothing may wait: the fetching is the
//! app's (installed with [`set_fetcher`]; this crate does no networking).
//!
//! A [`Remote`] can also hold whole answers to queries of the same service (an ArcGIS layer read a
//! view at a time, [`super::arcgis`]): a read asks for each by its address, and [`drive`] fetches
//! the missing ones in the same rounds as missing bytes.
//!
//! Readers plan ahead (a FlatGeobuf index level, the column chunks of a GeoParquet read) so a read
//! takes a few rounds, not one per missing piece. Files are hostile input: what a read may hold is
//! bounded, and a read that keeps needing more stops after [`MAX_ROUNDS`].

use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::{DataError, Result};

/// The smallest piece fetched for a read that wasn't planned (it is rounded out to it).
pub const MIN_FETCH: u64 = 64 << 10;
/// Requests closer than this are fetched as one.
const GAP: u64 = 16 << 10;
/// The largest single request.
const MAX_PIECE: u64 = 16 << 20;
/// The most bytes one read may plan to fetch.
pub const READ_BUDGET: u64 = if cfg!(target_arch = "wasm32") { 96 << 20 } else { 256 << 20 };
/// The most bytes a file keeps (least recently used parts are dropped past it).
const KEEP: usize = if cfg!(target_arch = "wasm32") { 192 << 20 } else { 640 << 20 };
/// The most rounds of fetching one read may take.
pub const MAX_ROUNDS: usize = 48;
/// The most bytes one round of a read may ask for (readers plan within [`READ_BUDGET`]).
const ROUND_BUDGET: u64 = READ_BUDGET.saturating_mul(2);
/// Files kept, with what was fetched of them.
const MAX_FILES: usize = 32;
/// The most bytes of whole answers one [`Remote`] keeps (least recently used ones are dropped past
/// it): a layer read by its query address, as [`KEEP`] bounds a file's parts, for each of up to
/// [`MAX_FILES`] addresses kept. Small in tests, so that they can fill it.
pub const KEEP_ANSWERS: usize = if cfg!(test) {
    2 << 20
} else if cfg!(target_arch = "wasm32") {
    96 << 20
} else {
    256 << 20
};
/// The most bytes one answer may have ([`Want::Upto`]).
pub const MAX_ANSWER: u64 = if cfg!(test) {
    64 << 10
} else if cfg!(target_arch = "wasm32") {
    4 << 20
} else {
    8 << 20
};
/// The most answers one round of a read asks for: the rest are asked for by the next round.
pub const ROUND_ANSWERS: usize = if cfg!(target_arch = "wasm32") { 12 } else { 16 };
// A read holds at most half of what a service keeps (`arcgis`), and a round's answers at their
// largest fit the other half: what a read needs at once is never pushed out by its own answers.
const _: () = assert!((ROUND_ANSWERS as u64) * MAX_ANSWER <= (KEEP_ANSWERS as u64) / 2);
const _: () = assert!((ROUND_ANSWERS as u64) * MAX_ANSWER <= ROUND_BUDGET);

/// The error of a read that needs bytes not fetched yet (it is run again once they are).
pub const NEED: &str = "waiting for the file's bytes";
/// The error of an answer larger than [`MAX_ANSWER`] ([`Remote::answer`]): the read asks for a
/// smaller one (see [`is_too_large`]).
pub const TOO_LARGE: &str = "the answer is larger than an answer may be";

/// Whether `e` says an answer was larger than [`MAX_ANSWER`] ([`TOO_LARGE`]).
pub fn is_too_large(e: &DataError) -> bool {
    matches!(e, DataError::Format(m) if m == TOO_LARGE)
}

/// Starts the error of a file that can't be read in place (no spatial index, no bounding-box
/// covering, a server without ranges): it is downloaded whole instead.
pub const READ_WHOLE: &str = "can't be read in place";

/// An owned, principal-free predicate for a claimed deferred request.
pub type Still = std::sync::Arc<dyn Fn() -> bool + Send + Sync>;
thread_local! { static STILL: std::cell::RefCell<Option<Still>> = const { std::cell::RefCell::new(None) }; }
/// Clones synchronous request context before a worker or browser future is scheduled.
pub fn current_still() -> Option<Still> {
    STILL.with(|s| s.borrow().clone())
}
/// The guard is synchronous. Never hold it across an await.
pub fn with_still<T>(still: Still, f: impl FnOnce() -> T) -> T {
    with_optional_still(Some(still), f)
}
/// Re-enters captured context synchronously, including around follow-on callbacks.
pub fn with_optional_still<T>(still: Option<Still>, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<Still>);
    impl Drop for Restore {
        fn drop(&mut self) {
            STILL.with(|s| *s.borrow_mut() = self.0.take());
        }
    }
    let previous = current_still();
    let combined = match (previous.clone(), still) {
        (Some(a), Some(b)) => {
            let held = is_held_still(&a) || is_held_still(&b);
            let both = std::sync::Arc::new(move || a() && b()) as Still;
            Some(if held { held_still(both) } else { both })
        }
        (Some(a), None) => Some(a),
        (None, b) => b,
    };
    STILL.with(|s| *s.borrow_mut() = combined);
    let _restore = Restore(previous);
    f()
}
/// Hands a shared I/O completion to an independently captured request, restoring the current
/// scope afterwards. Use only at that callback boundary; nested work uses [`with_optional_still`]
/// to inherit restrictions. This synchronous guard must never cross an await.
pub fn with_captured_still<T>(still: Option<Still>, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<Still>);
    impl Drop for Restore {
        fn drop(&mut self) {
            STILL.with(|s| *s.borrow_mut() = self.0.take());
        }
    }
    let _restore = Restore(STILL.with(|s| s.replace(still)));
    f()
}

/// The request contexts whose requests may reach only the internet ([`held_still`]).
static HELD: std::sync::Mutex<Vec<std::sync::Weak<dyn Fn() -> bool + Send + Sync>>> = std::sync::Mutex::new(Vec::new());

/// Marks `still` as holding its requests to the internet (a plugin's command: plugins reach only
/// the internet). The mark goes wherever the context does: a worker that re-enters it
/// ([`current_still`], [`with_optional_still`], [`with_captured_still`]) is held too, and so is a
/// context it is combined with.
pub fn held_still(still: Still) -> Still {
    let mut held = HELD.lock().unwrap_or_else(|e| e.into_inner());
    held.retain(|w| w.strong_count() > 0);
    held.push(std::sync::Arc::downgrade(&still));
    still
}

/// Whether `still` was marked by [`held_still`].
fn is_held_still(still: &Still) -> bool {
    let at = std::sync::Arc::as_ptr(still).cast::<()>();
    HELD.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|w| std::sync::Weak::as_ptr(w).cast::<()>() == at)
}

/// Whether this thread's requests may reach only the internet: its request context was marked
/// by [`held_still`].
pub fn requests_held() -> bool {
    current_still().is_some_and(|still| is_held_still(&still))
}

/// Checked immediately before a request-producing stage, without suspension before sending.
pub fn check_still(still: &Option<Still>) -> std::result::Result<(), String> {
    if still.as_ref().is_some_and(|s| !s()) { Err("request cancelled before it was sent".into()) } else { Ok(()) }
}

/// A request for part of a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Want {
    /// Bytes `start..end`.
    Range(u64, u64),
    /// The last `n` bytes (where the file's length isn't known: a GeoParquet footer).
    Tail(u64),
    /// The whole file: one that can't be read in place ([`READ_WHOLE`]), downloaded instead.
    Whole,
    /// The whole answer to a query ([`Remote::answer`]), refused past this many bytes.
    Upto(u64),
}

/// Why a fetch brought no bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failed {
    /// Dropped unfetched (the view moved on): asked for again by the next round.
    Dropped,
    /// The range starts past the end of the file (HTTP 416).
    PastEnd,
    /// The server doesn't answer byte-range requests: the file is downloaded whole instead.
    NoRanges,
    /// The download is larger than its limit (an answer past [`Want::Upto`], which a read asks
    /// for again smaller), with the fetcher's message.
    TooLarge(String),
    /// Anything else, with its message.
    Other(String),
}

/// Called once with the bytes asked for, or why they couldn't be had.
pub type Done = Box<dyn FnOnce(std::result::Result<Vec<u8>, Failed>) + Send>;
type Fetcher = Arc<dyn Fn(&str, Want, Done) + Send + Sync>;

fn fetcher() -> &'static Mutex<Option<Fetcher>> {
    static F: OnceLock<Mutex<Option<Fetcher>>> = OnceLock::new();
    F.get_or_init(Mutex::default)
}

/// Sets how parts of files are fetched (the tile cache's fetchers, which check addresses).
pub fn set_fetcher(f: impl Fn(&str, Want, Done) + Send + Sync + 'static) {
    *fetcher().lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(f));
}

/// Whether a fetcher is installed.
pub fn can_fetch() -> bool {
    fetcher().lock().unwrap_or_else(|e| e.into_inner()).is_some()
}

/// Fetches a whole file through the installed fetcher (a file that can't be read in place).
pub fn fetch_whole(url: &str, done: Done) {
    let still = current_still();
    if let Err(e) = check_still(&still) {
        done(Err(Failed::Other(e)));
        return;
    }
    let context = still.clone();
    let done: Done = Box::new(move |r| {
        with_optional_still(context.clone(), || {
            done(check_still(&context).map_err(Failed::Other).and(r));
        })
    });
    if let Err(error) = crate::login_address::check(url) {
        done(Err(Failed::Other(error.into())));
        return;
    }
    let done: Done = if crate::login_address::is_wrapper(url) {
        let address = url.to_string();
        Box::new(move |r| done(crate::login_address::check(&address).map_err(|e| Failed::Other(e.into())).and(r)))
    } else {
        done
    };
    let f = fetcher().lock().unwrap_or_else(|e| e.into_inner()).clone();
    match f {
        Some(f) => f(url, Want::Whole, done),
        None => done(Err(Failed::Other("no fetcher installed".into()))),
    }
}

struct Piece {
    bytes: Arc<Vec<u8>>,
    used: u64,
}

/// What a round fetches: part of the file, or the whole answer to a query of the same service.
#[derive(Clone)]
enum Job {
    Part(Want),
    Answer(String),
}

#[derive(Default)]
struct Store {
    /// Parts held, by where they start: never overlapping, merged when they meet.
    pieces: BTreeMap<u64, Piece>,
    held: usize,
    tick: u64,
    /// The end of the file, once a read ran past it.
    end: Option<u64>,
    /// The file's last bytes, and whether they are the whole file (it was shorter than asked).
    tail: Option<(Arc<Vec<u8>>, bool)>,
    /// Bytes reads found missing: (the read's drive, start, end, whether asked for exactly
    /// (planned ahead) rather than found by a read, which is rounded out).
    missing: Vec<(u64, u64, u64, bool)>,
    /// The last bytes reads found missing: (the read's drive, how many).
    missing_tail: Vec<(u64, u64)>,
    /// Whole answers held, by their address ([`Remote::answer`]).
    answers: std::collections::HashMap<String, Piece>,
    answers_held: usize,
    /// Answers reads found missing: (the read's drive, the address).
    missing_answers: Vec<(u64, String)>,
    /// Addresses whose answers were larger than [`MAX_ANSWER`], at most [`MAX_TOO_LARGE`].
    too_large: std::collections::HashSet<String>,
}

/// The most addresses of answers too large a [`Remote`] remembers (all are forgotten past it).
const MAX_TOO_LARGE: usize = 1024;

thread_local! {
    /// The drive whose read runs on this thread (0: none), so each drive fetches what its own
    /// read found missing, never another's on the same file.
    static DRIVE: Cell<u64> = const { Cell::new(0) };
}

/// Marks the reads on this thread as a drive's while it lives.
struct Running(u64);

impl Running {
    fn enter(drive: u64) -> Self {
        Running(DRIVE.with(|d| d.replace(drive)))
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        DRIVE.with(|d| d.set(self.0));
    }
}

fn drive_now() -> u64 {
    DRIVE.with(Cell::get)
}

/// A file by its web address: the parts fetched so far.
pub struct Remote {
    url: String,
    store: Mutex<Store>,
}

impl std::fmt::Debug for Remote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Remote").field("url", &crate::login_address::inner(&self.url)).finish_non_exhaustive()
    }
}

/// A part of a held piece, without a copy.
struct Part {
    bytes: Arc<Vec<u8>>,
    from: usize,
    to: usize,
}

impl AsRef<[u8]> for Part {
    fn as_ref(&self) -> &[u8] {
        self.bytes.get(self.from..self.to).unwrap_or_default()
    }
}

impl Remote {
    fn new(url: &str) -> Self {
        Remote { url: url.to_string(), store: Mutex::new(Store::default()) }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Store> {
        self.store.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Bytes `at..at+len` (fewer at the end of the file), or `None` when they aren't held (they
    /// are then noted as missing).
    pub fn bytes(&self, at: u64, len: u64) -> Option<bytes::Bytes> {
        self.get(at, len, false)
    }

    fn get(&self, at: u64, len: u64, exact: bool) -> Option<bytes::Bytes> {
        crate::login_address::check(&self.url).ok()?;
        let mut s = self.lock();
        let len = match s.end {
            Some(end) if at >= end => return Some(bytes::Bytes::new()),
            Some(end) => len.min(end - at),
            None => len,
        };
        if len == 0 {
            return Some(bytes::Bytes::new());
        }
        s.tick += 1;
        let tick = s.tick;
        let found = s.pieces.range_mut(..=at).next_back().and_then(|(start, p)| {
            let from = usize::try_from(at - start).ok()?;
            let to = from.checked_add(usize::try_from(len).ok()?)?;
            (to <= p.bytes.len()).then(|| {
                p.used = tick;
                Part { bytes: p.bytes.clone(), from, to }
            })
        });
        match found {
            Some(part) => Some(bytes::Bytes::from_owner(part)),
            None => {
                s.missing.push((drive_now(), at, at.saturating_add(len), exact));
                None
            }
        }
    }

    /// Bytes from `at` to the end of the held piece that holds `at` (a column chunk read in
    /// pages), or `None` (noted as missing, `want` bytes from there).
    pub fn from(&self, at: u64, want: u64) -> Option<bytes::Bytes> {
        crate::login_address::check(&self.url).ok()?;
        let mut s = self.lock();
        s.tick += 1;
        let tick = s.tick;
        let found = s.pieces.range_mut(..=at).next_back().and_then(|(start, p)| {
            let from = usize::try_from(at - start).ok()?;
            (from < p.bytes.len()).then(|| {
                p.used = tick;
                Part { bytes: p.bytes.clone(), from, to: p.bytes.len() }
            })
        });
        match found {
            Some(part) => Some(bytes::Bytes::from_owner(part)),
            None => {
                s.missing.push((drive_now(), at, at.saturating_add(want.max(1)), false));
                None
            }
        }
    }

    /// Asks for bytes a read will need, without reading them: `true` when they are held. They
    /// are fetched as asked (a read's plan), not rounded out.
    pub fn want(&self, at: u64, len: u64) -> bool {
        self.get(at, len, true).is_some()
    }

    /// The file's last `n` bytes (all of it when it's shorter), and whether that is the whole
    /// file; `None` when they aren't held (noted as missing).
    pub fn tail(&self, n: u64) -> Option<(Arc<Vec<u8>>, bool)> {
        crate::login_address::check(&self.url).ok()?;
        let mut s = self.lock();
        match &s.tail {
            Some((t, whole)) if *whole || t.len() as u64 >= n => Some((t.clone(), *whole)),
            _ => {
                s.missing_tail.push((drive_now(), n));
                None
            }
        }
    }

    /// Whether a read found something missing.
    pub fn is_missing(&self) -> bool {
        let s = self.lock();
        !s.missing.is_empty() || !s.missing_tail.is_empty() || !s.missing_answers.is_empty()
    }

    /// The whole answer to `url`, a query of the same service (the same origin, and the same login
    /// when the address is bound to one), or `None` when it isn't held (it is then noted as
    /// missing). An address of another origin is refused.
    pub fn answer(&self, url: &str) -> Result<Option<Arc<Vec<u8>>>> {
        self.same_service(url)?;
        crate::login_address::check(url).map_err(|e| DataError::Format(e.into()))?;
        let mut s = self.lock();
        s.tick += 1;
        let tick = s.tick;
        if let Some(a) = s.answers.get_mut(url) {
            a.used = tick;
            return Ok(Some(a.bytes.clone()));
        }
        if s.too_large.contains(url) {
            return Err(DataError::Format(TOO_LARGE.into()));
        }
        s.missing_answers.push((drive_now(), url.to_string()));
        Ok(None)
    }

    /// Remembers that the answer to `url` was larger than [`MAX_ANSWER`].
    fn note_too_large(&self, url: &str) {
        let mut s = self.lock();
        if s.too_large.len() >= MAX_TOO_LARGE {
            s.too_large.clear();
        }
        s.too_large.insert(url.to_string());
    }

    /// Drops a held answer (one that couldn't be read, so a later read asks for it again).
    pub fn forget_answer(&self, url: &str) {
        let mut s = self.lock();
        if let Some(a) = s.answers.remove(url) {
            s.answers_held = s.answers_held.saturating_sub(a.bytes.len());
        }
    }

    /// Refuses an address that isn't a query of this service: answers are fetched with the
    /// service's login, so they must go nowhere else.
    fn same_service(&self, url: &str) -> Result<()> {
        let mine = crate::login_address::origin(&self.url);
        if mine.is_some()
            && mine == crate::login_address::origin(url)
            && crate::login_address::is_wrapper(url) == crate::login_address::is_wrapper(&self.url)
        {
            Ok(())
        } else {
            Err(DataError::Format(crate::diagnostics::message_with_address(&self.url, "a query of another service was asked for")))
        }
    }

    /// The answers this drive's reads found missing, each once, at most [`ROUND_ANSWERS`]: the
    /// rest are found missing again by the next run. Forgets them.
    fn take_missing_answers(&self, drive: u64) -> Vec<String> {
        let mut s = self.lock();
        let (mine, others): (Vec<_>, Vec<_>) = std::mem::take(&mut s.missing_answers).into_iter().partition(|m| m.0 == drive);
        s.missing_answers = others.into_iter().filter(|m| m.0 != 0).collect();
        let mut out: Vec<String> = Vec::new();
        for (_, url) in mine {
            if out.len() == ROUND_ANSWERS {
                break;
            }
            if !out.contains(&url) {
                out.push(url);
            }
        }
        out
    }

    /// Keeps the whole answer to `url`, dropping the least recently used others past
    /// [`KEEP_ANSWERS`].
    fn insert_answer(&self, url: &str, bytes: Vec<u8>) {
        if crate::login_address::check(&self.url).is_err() || crate::login_address::check(url).is_err() {
            return;
        }
        let mut s = self.lock();
        s.tick += 1;
        let tick = s.tick;
        let len = bytes.len();
        if let Some(old) = s.answers.insert(url.to_string(), Piece { bytes: Arc::new(bytes), used: tick }) {
            s.answers_held = s.answers_held.saturating_sub(old.bytes.len());
        }
        s.answers_held = s.answers_held.saturating_add(len);
        while s.answers_held > KEEP_ANSWERS {
            let Some(old) = s.answers.iter().filter(|(u, _)| u.as_str() != url).min_by_key(|(_, a)| a.used).map(|(u, _)| u.clone()) else {
                break;
            };
            if let Some(a) = s.answers.remove(&old) {
                s.answers_held = s.answers_held.saturating_sub(a.bytes.len());
            }
        }
    }

    /// The requests that fetch what reads found missing: rounded out, merged and split into
    /// pieces of a bounded size. Forgets them.
    fn take_missing(&self, drive: u64) -> Vec<Want> {
        let mut s = self.lock();
        let (mine, others): (Vec<_>, Vec<_>) = std::mem::take(&mut s.missing).into_iter().partition(|m| m.0 == drive);
        // What reads outside any drive found missing (a check) is nobody's to fetch.
        s.missing = others.into_iter().filter(|m| m.0 != 0).collect();
        let (tails, others): (Vec<_>, Vec<_>) = std::mem::take(&mut s.missing_tail).into_iter().partition(|t| t.0 == drive);
        s.missing_tail = others.into_iter().filter(|t| t.0 != 0).collect();
        let tail = tails.into_iter().map(|t| t.1).max();
        let mut ranges: Vec<(u64, u64)> = mine
            .into_iter()
            .map(|(_, a, b, exact)| {
                let (a, b) = if exact { (a, b) } else { (a / MIN_FETCH * MIN_FETCH, b.div_ceil(MIN_FETCH).saturating_mul(MIN_FETCH)) };
                match s.end {
                    Some(end) => (a.min(end), b.min(end)),
                    None => (a, b),
                }
            })
            .filter(|(a, b)| a < b)
            .collect();
        ranges.sort_unstable();
        let mut merged: Vec<(u64, u64)> = Vec::new();
        for (a, b) in ranges {
            match merged.last_mut() {
                Some(last) if a <= last.1.saturating_add(GAP) => last.1 = last.1.max(b),
                _ => merged.push((a, b)),
            }
        }
        let mut out = Vec::new();
        for (a, b) in merged {
            let mut at = a;
            // `at < b`, so each piece ends past where it starts, even at the end of the range.
            while at < b {
                let end = at.saturating_add(MAX_PIECE).min(b);
                out.push(Want::Range(at, end));
                at = end;
            }
        }
        if let Some(n) = tail {
            out.push(Want::Tail(n));
        }
        out
    }

    /// Keeps fetched bytes `at..`: merged with the parts they meet. A piece shorter than asked
    /// for (`asked`) ends the file.
    fn insert(&self, at: u64, asked: u64, bytes: Vec<u8>) {
        if crate::login_address::check(&self.url).is_err() {
            return;
        }
        let mut s = self.lock();
        if (bytes.len() as u64) < asked {
            let end = at.saturating_add(bytes.len() as u64);
            s.end = Some(s.end.map_or(end, |e| e.min(end)));
        }
        if bytes.is_empty() {
            return;
        }
        s.tick += 1;
        let tick = s.tick;
        let mut start = at;
        let mut end = at.saturating_add(bytes.len() as u64);
        // The pieces this one meets (touching counts), merged into one.
        let meets: Vec<u64> =
            s.pieces.range(..=end).filter(|(a, p)| a.saturating_add(p.bytes.len() as u64) >= start).map(|(a, _)| *a).collect();
        let mut parts: Vec<(u64, Arc<Vec<u8>>)> = Vec::with_capacity(meets.len() + 1);
        for a in meets {
            if let Some(p) = s.pieces.remove(&a) {
                s.held = s.held.saturating_sub(p.bytes.len());
                start = start.min(a);
                end = end.max(a.saturating_add(p.bytes.len() as u64));
                parts.push((a, p.bytes));
            }
        }
        let piece = if parts.is_empty() {
            Arc::new(bytes)
        } else {
            let Ok(size) = usize::try_from(end - start) else { return };
            let mut out = vec![0u8; size];
            parts.push((at, Arc::new(bytes)));
            for (a, b) in &parts {
                let from = usize::try_from(a - start).unwrap_or(usize::MAX);
                if let Some(dst) = from.checked_add(b.len()).and_then(|to| out.get_mut(from..to)) {
                    dst.copy_from_slice(b);
                }
            }
            Arc::new(out)
        };
        s.held += piece.len();
        s.pieces.insert(start, Piece { bytes: piece, used: tick });
        // Past what a file may hold: the least recently used parts go (never the one just in).
        while s.held > KEEP {
            let Some(old) = s.pieces.iter().filter(|(a, _)| **a != start).min_by_key(|(_, p)| p.used).map(|(a, _)| *a) else { break };
            if let Some(p) = s.pieces.remove(&old) {
                s.held = s.held.saturating_sub(p.bytes.len());
            }
        }
    }

    /// Keeps a file's last `asked` bytes: fewer is the whole file.
    fn insert_tail(&self, asked: u64, bytes: Vec<u8>) {
        if crate::login_address::check(&self.url).is_err() {
            return;
        }
        let whole = (bytes.len() as u64) < asked;
        let bytes = Arc::new(bytes);
        {
            let mut s = self.lock();
            if whole {
                s.end = Some(bytes.len() as u64);
            }
            s.tail = Some((bytes.clone(), whole));
        }
        if whole {
            self.insert(0, 0, bytes.as_ref().clone());
        }
    }
}

fn files() -> &'static Mutex<Vec<Arc<Remote>>> {
    static F: OnceLock<Mutex<Vec<Arc<Remote>>>> = OnceLock::new();
    F.get_or_init(Mutex::default)
}

/// The file at `url`, with whatever was fetched of it already.
pub fn file(url: &str) -> Arc<Remote> {
    let mut f = files().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(i) = f.iter().position(|r| r.url == url) {
        let r = f.remove(i);
        f.push(r.clone());
        return r;
    }
    let r = Arc::new(Remote::new(url));
    f.push(r.clone());
    if f.len() > MAX_FILES {
        f.remove(0);
    }
    r
}

/// Forgets every file's fetched parts.
pub fn forget() {
    files().lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// Forgets the fetched parts of one file (after the address's login changed).
pub fn forget_file(url: &str) {
    files().lock().unwrap_or_else(|e| e.into_inner()).retain(|r| r.url != url);
}

/// The error a read that needs bytes gives (it runs again once they are fetched).
pub fn need() -> DataError {
    DataError::Format(NEED.into())
}

/// A refused native view needs geometry matching before sampling, but that scan cannot fit.
/// Returning a partial sample here could wrongly make a nonempty view look empty.
pub(crate) fn view_budget() -> DataError {
    DataError::Format("the requested view can't be bounded in the file's coordinate system, and matching it would exceed the remote read budget; download the file or choose a smaller view".into())
}

/// Runs `work` until it needs nothing it hasn't got: each run that finds bytes missing fetches
/// them (in one round of requests) and runs again. `done` gets the last run's result. Off the
/// caller's thread: the rounds run as the fetches land.
pub fn drive<T: Send + 'static>(
    remote: Arc<Remote>,
    work: Arc<dyn Fn() -> Result<T> + Send + Sync>,
    done: Box<dyn FnOnce(Result<T>) + Send>,
) {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    step(remote, work, done, NEXT.fetch_add(1, Ordering::Relaxed), 0, current_still());
}

/// Re-enters the owned synchronous carrier for each read round, including callbacks on another thread.
fn step<T: Send + 'static>(
    remote: Arc<Remote>,
    work: Arc<dyn Fn() -> Result<T> + Send + Sync>,
    done: Box<dyn FnOnce(Result<T>) + Send>,
    drive: u64,
    rounds: usize,
    still: Option<Still>,
) {
    with_optional_still(still.clone(), || step_in(remote, work, done, drive, rounds, still));
}

/// Checks before reading, sending each new range, accepting bytes and starting another round.
fn step_in<T: Send + 'static>(
    remote: Arc<Remote>,
    work: Arc<dyn Fn() -> Result<T> + Send + Sync>,
    done: Box<dyn FnOnce(Result<T>) + Send>,
    drive: u64,
    rounds: usize,
    still: Option<Still>,
) {
    if let Err(e) = check_still(&still) {
        done(Err(DataError::Format(e)));
        return;
    }
    if let Err(error) = crate::login_address::check(remote.url()) {
        done(Err(DataError::Format(error.into())));
        return;
    }
    let r = {
        let _running = Running::enter(drive);
        work()
    };
    if let Err(error) = crate::login_address::check(remote.url()) {
        done(Err(DataError::Format(error.into())));
        return;
    }
    let wants = remote.take_missing(drive);
    let answers = remote.take_missing_answers(drive);
    if wants.is_empty() && answers.is_empty() {
        done(check_still(&still).map_err(DataError::Format).and(r));
        return;
    }
    // Readers plan within READ_BUDGET; a round asking for far more is a hostile file's offsets.
    let asked: u64 = wants.iter().map(|w| if let Want::Range(a, b) = w { b - a } else { 0 }).fold(0, u64::saturating_add);
    if asked > ROUND_BUDGET {
        done(Err(DataError::Format(crate::diagnostics::message_with_address(
            &remote.url,
            &format!("a read asked for {} MB of the file at once", asked >> 20),
        ))));
        return;
    }
    if rounds >= MAX_ROUNDS {
        done(Err(DataError::Format(crate::diagnostics::message_with_address(
            &remote.url,
            &format!("still needed more of the file after {MAX_ROUNDS} rounds of reading"),
        ))));
        return;
    }
    let Some(fetch) = fetcher().lock().unwrap_or_else(|e| e.into_inner()).clone() else {
        done(Err(DataError::Unsupported("reading files by their web address (no fetcher)".into())));
        return;
    };
    // Every request answered (or failed), the read runs again.
    struct Round<T> {
        left: usize,
        failed: Option<String>,
        done: Option<Box<dyn FnOnce(Result<T>) + Send>>,
    }
    let jobs: Vec<Job> = wants.into_iter().map(Job::Part).chain(answers.into_iter().map(Job::Answer)).collect();
    let round = Arc::new(Mutex::new(Round { left: jobs.len(), failed: None, done: Some(done) }));
    for job in jobs {
        let (remote2, work2, round2) = (remote.clone(), work.clone(), round.clone());
        let landed_job = job.clone();
        let context = still.clone();
        let landed: Done = Box::new(move |res| {
            with_optional_still(context.clone(), || {
                let res = check_still(&context).map_err(Failed::Other).and(res);
                match (&landed_job, res) {
                    (Job::Part(Want::Range(a, b)), Ok(bytes)) => remote2.insert(*a, b - a, bytes),
                    (Job::Part(Want::Tail(n)), Ok(bytes)) => remote2.insert_tail(*n, bytes),
                    // (Reads ask for ranges and tails; a whole file is all of its bytes from the start.)
                    (Job::Part(Want::Whole | Want::Upto(_)), Ok(bytes)) => remote2.insert(0, bytes.len() as u64, bytes),
                    (Job::Answer(url), Ok(bytes)) => remote2.insert_answer(url, bytes),
                    // Said to the read by `Remote::answer`, which asks for a smaller one.
                    (Job::Answer(url), Err(Failed::TooLarge(_))) => remote2.note_too_large(url),
                    // A request dropped unfetched (the view moved on) is asked for again next round.
                    (_, Err(Failed::Dropped)) => {}
                    // A range past the end of the file.
                    (Job::Part(Want::Range(a, b)), Err(Failed::PastEnd)) => remote2.insert(*a, b - a, Vec::new()),
                    (_, Err(e)) => {
                        let e = match e {
                            Failed::NoRanges => format!("the server doesn't answer byte-range requests ({READ_WHOLE})"),
                            Failed::PastEnd => "the last bytes asked for past the end of the file".into(),
                            Failed::Dropped => "the request was dropped".into(),
                            Failed::TooLarge(e) => e,
                            Failed::Other(e) => e,
                        };
                        let mut r = round2.lock().unwrap_or_else(|e| e.into_inner());
                        r.failed.get_or_insert(e);
                    }
                }
                let next = {
                    let mut r = round2.lock().unwrap_or_else(|e| e.into_inner());
                    r.left = r.left.saturating_sub(1);
                    if r.left == 0 { Some((r.done.take(), r.failed.take())) } else { None }
                };
                if let Some((Some(done), failed)) = next {
                    match failed {
                        Some(e) => done(Err(DataError::Format(crate::diagnostics::message_with_address(remote2.url(), &e)))),
                        None => step(remote2, work2, done, drive, rounds + 1, context),
                    }
                }
            })
        });
        match (check_still(&still), &job) {
            (Err(e), _) => landed(Err(Failed::Other(e))),
            (Ok(()), Job::Part(w)) => fetch(&remote.url, *w, landed),
            // Asked for by a read through `Remote::answer`, which keeps it to this service.
            (Ok(()), Job::Answer(url)) => match remote
                .same_service(url)
                .map_err(|e| e.to_string())
                .and_then(|()| crate::login_address::check(url).map_err(String::from))
            {
                Ok(()) => fetch(url, Want::Upto(MAX_ANSWER), landed),
                Err(e) => landed(Err(Failed::Other(e))),
            },
        }
    }
}

/// [`drive`], waiting for the result (the command line and tools; never on the frame).
#[cfg(not(target_arch = "wasm32"))]
pub fn drive_blocking<T: Send + 'static>(remote: Arc<Remote>, work: Arc<dyn Fn() -> Result<T> + Send + Sync>) -> Result<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    let url = remote.url.clone();
    drive(
        remote,
        work,
        Box::new(move |r| {
            let _ = tx.send(r);
        }),
    );
    rx.recv_timeout(std::time::Duration::from_secs(120))
        .unwrap_or_else(|_| Err(DataError::Format(crate::diagnostics::message_with_address(&url, "no answer in two minutes"))))
}

/// A file by its address, read and seeked through what is held of it: a read of bytes not held
/// notes them missing and fails (`WouldBlock`). Stand-ins can be given for parts a read must pass
/// over but needn't see (a FlatGeobuf feature left out of a sample).
pub(crate) struct Reader {
    remote: Arc<Remote>,
    pos: u64,
    stand_ins: Option<Arc<StandIns>>,
    /// The stand-in being read: where it starts, and its bytes.
    active: Option<(u64, Arc<Vec<u8>>)>,
}

/// Bytes served in place of the file's, by where they start.
pub(crate) struct StandIns {
    pub at: std::collections::HashMap<u64, Arc<Vec<u8>>>,
}

impl Reader {
    pub(crate) fn new(remote: Arc<Remote>) -> Self {
        Reader { remote, pos: 0, stand_ins: None, active: None }
    }

    pub(crate) fn with_stand_ins(remote: Arc<Remote>, stand_ins: Arc<StandIns>) -> Self {
        Reader { remote, pos: 0, stand_ins: Some(stand_ins), active: None }
    }
}

impl std::io::Read for Reader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if let Some(si) = &self.stand_ins {
            if let Some(b) = si.at.get(&self.pos) {
                self.active = Some((self.pos, b.clone()));
            }
            if let Some((a, bytes)) = &self.active {
                let off = usize::try_from(self.pos.saturating_sub(*a)).unwrap_or(usize::MAX);
                if self.pos >= *a && off < bytes.len() {
                    let src = bytes.get(off..).unwrap_or_default();
                    let n = src.len().min(buf.len());
                    if let (Some(dst), Some(src)) = (buf.get_mut(..n), src.get(..n)) {
                        dst.copy_from_slice(src);
                    }
                    self.pos += n as u64;
                    return Ok(n);
                }
                self.active = None;
            }
        }
        match self.remote.bytes(self.pos, buf.len() as u64) {
            Some(b) => {
                let n = b.len().min(buf.len());
                if let (Some(dst), Some(src)) = (buf.get_mut(..n), b.get(..n)) {
                    dst.copy_from_slice(src);
                }
                self.pos += n as u64;
                Ok(n)
            }
            None => Err(std::io::Error::new(std::io::ErrorKind::WouldBlock, NEED)),
        }
    }
}

impl std::io::Seek for Reader {
    fn seek(&mut self, to: std::io::SeekFrom) -> std::io::Result<u64> {
        let bad = || std::io::Error::new(std::io::ErrorKind::InvalidInput, "a seek outside the file");
        self.pos = match to {
            std::io::SeekFrom::Start(n) => n,
            std::io::SeekFrom::Current(d) => self.pos.checked_add_signed(d).ok_or_else(bad)?,
            std::io::SeekFrom::End(_) => return Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "the file's length isn't known")),
        };
        Ok(self.pos)
    }
}

/// A fetcher for tests: files served from memory by address, counting the bytes sent.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    pub(crate) static LOGIN_ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

    pub(crate) fn validate_login(address: &str) -> std::result::Result<(), &'static str> {
        let binding = crate::login_address::parse(address)?.map(|b| b.binding);
        if binding == Some("ffffffffffffffff") && !LOGIN_ACTIVE.load(Ordering::Acquire) {
            Err("The service login is no longer active.")
        } else {
            Ok(())
        }
    }

    fn served() -> &'static Mutex<HashMap<String, (Arc<Vec<u8>>, Arc<AtomicUsize>)>> {
        static S: OnceLock<Mutex<HashMap<String, (Arc<Vec<u8>>, Arc<AtomicUsize>)>>> = OnceLock::new();
        S.get_or_init(Mutex::default)
    }

    /// Answers computed from the address, for addresses starting with a prefix (a service).
    type Handler = Arc<dyn Fn(&str) -> Option<Vec<u8>> + Send + Sync>;

    fn handlers() -> &'static Mutex<Vec<(String, Handler)>> {
        static H: OnceLock<Mutex<Vec<(String, Handler)>>> = OnceLock::new();
        H.get_or_init(Mutex::default)
    }

    /// Serves `bytes` at `url` (by range); returns the count of bytes sent.
    pub(crate) fn serve(url: &str, bytes: Vec<u8>) -> Arc<AtomicUsize> {
        let sent = Arc::new(AtomicUsize::new(0));
        served().lock().unwrap_or_else(|e| e.into_inner()).insert(url.to_string(), (Arc::new(bytes), sent.clone()));
        install();
        sent
    }

    /// Answers every address starting with `prefix` with what `answer` makes of it (`None`: HTTP
    /// 404), whole.
    pub(crate) fn handle(prefix: &str, answer: impl Fn(&str) -> Option<Vec<u8>> + Send + Sync + 'static) {
        handlers().lock().unwrap_or_else(|e| e.into_inner()).push((prefix.to_string(), Arc::new(answer)));
        install();
    }

    /// The one fetcher every test installs, so tests running at once serve each other's files.
    fn install() {
        set_fetcher(|url, w, done| {
            let handler = handlers()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .find(|(p, _)| url.starts_with(p.as_str()))
                .map(|(_, h)| h.clone());
            if let Some(h) = handler {
                let answer = h(url).ok_or_else(|| Failed::Other("HTTP 404".into()));
                done(match (w, answer) {
                    (Want::Upto(n), Ok(a)) if a.len() as u64 > n => Err(Failed::TooLarge("the answer is too large".into())),
                    (_, answer) => answer,
                });
                return;
            }
            let file = served().lock().unwrap_or_else(|e| e.into_inner()).get(url).cloned();
            let Some((f, sent)) = file else {
                done(Err(Failed::Other("HTTP 404".into())));
                return;
            };
            // Busy elsewhere: every file kept is pushed out by others before the answer lands.
            if url.ends_with("#busy") {
                for i in 0..=MAX_FILES {
                    super::file(&format!("test:busy-{i}"));
                }
            }
            let r = match w {
                Want::Range(a, b) => f.get(a as usize..(b as usize).min(f.len())).map(<[u8]>::to_vec).unwrap_or_default(),
                Want::Tail(n) => f.get(f.len().saturating_sub(n as usize)..).map(<[u8]>::to_vec).unwrap_or_default(),
                Want::Whole => f.to_vec(),
                Want::Upto(n) if f.len() as u64 > n => {
                    done(Err(Failed::TooLarge("the answer is too large".into())));
                    return;
                }
                Want::Upto(_) => f.to_vec(),
            };
            sent.fetch_add(r.len(), Ordering::Relaxed);
            done(Ok(r));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn held_ranges_and_late_publications_are_revoked() {
        crate::login_address::set_validator(testing::validate_login);
        let address = crate::login_address::bind("ffffffffffffffff", "https://revoked.test", "https://revoked.test/file.fgb").unwrap();
        let remote = Remote::new(&address);
        remote.insert(0, 4, b"body".to_vec());
        remote.insert_tail(4, b"tail".to_vec());
        assert_eq!(remote.bytes(0, 4).unwrap().as_ref(), b"body");
        assert!(remote.tail(4).is_some());
        testing::LOGIN_ACTIVE.store(false, Ordering::Release);
        assert!(remote.bytes(0, 4).is_none());
        assert!(remote.from(0, 4).is_none());
        assert!(remote.tail(4).is_none());
        remote.insert(4, 4, b"late".to_vec());
        remote.insert_tail(4, b"late".to_vec());
        assert_eq!(remote.lock().held, 4);
        assert_eq!(remote.lock().tail.as_ref().unwrap().0.as_slice(), b"tail");
        let replacement = crate::login_address::bind("eeeeeeeeeeeeeeee", "https://revoked.test", "https://revoked.test/file.fgb").unwrap();
        assert!(file(&replacement).bytes(0, 4).is_none());
        assert!(file("https://revoked.test/file.fgb").bytes(0, 4).is_none());
    }

    #[test]
    fn holds_merges_and_notes_what_is_missing() {
        let r = Remote::new("https://x.org/a.fgb");
        assert!(r.bytes(10, 5).is_none());
        assert_eq!(r.take_missing(0), vec![Want::Range(0, MIN_FETCH)]);
        r.insert(0, 8, vec![1; 8]);
        r.insert(8, 8, vec![2; 8]);
        // Touching pieces are one.
        assert_eq!(r.bytes(6, 4).map(|b| b.to_vec()), Some(vec![1, 1, 2, 2]));
        assert!(!r.is_missing());
        // A piece shorter than asked ends the file: reads past it are empty, not missing.
        r.insert(16, 100, vec![3; 4]);
        assert_eq!(r.bytes(18, 50).map(|b| b.len()), Some(2));
        assert_eq!(r.bytes(40, 4).map(|b| b.len()), Some(0));
        assert!(!r.is_missing());
        // Far apart misses are separate requests, near ones merged, large ones split.
        let r = Remote::new("u");
        r.bytes(0, 1);
        r.bytes(MIN_FETCH * 10, 1);
        r.bytes(MIN_FETCH * 10 + 5, MAX_PIECE + 5);
        let w = r.take_missing(0);
        assert_eq!(w.first(), Some(&Want::Range(0, MIN_FETCH)));
        assert!(w.len() == 3 && w.iter().all(|w| matches!(w, Want::Range(a, b) if b - a <= MAX_PIECE)));
        // The tail: shorter than asked is the whole file.
        assert!(r.tail(16).is_none());
        assert!(r.take_missing(0).contains(&Want::Tail(16)));
        r.insert_tail(16, vec![7; 4]);
        assert_eq!(r.tail(1_000).map(|(t, whole)| (t.len(), whole)), Some((4, true)));
        assert_eq!(r.bytes(0, 4).map(|b| b.to_vec()), Some(vec![7; 4]));
    }

    #[test]
    fn drives_a_read_until_it_has_what_it_needs() {
        let file: Vec<u8> = (0..=255u8).cycle().take(300_000).collect();
        testing::serve("test:drive", file.clone());
        let r = Arc::new(Remote::new("test:drive"));
        let rr = r.clone();
        // Two reads far apart, then one that depends on the first's bytes.
        let work: Arc<dyn Fn() -> Result<u64> + Send + Sync> = Arc::new(move || {
            let a = rr.bytes(5, 2).ok_or_else(need)?;
            let b = rr.bytes(200_000, 2).ok_or_else(need)?;
            let at = u64::from(*a.first().unwrap_or(&0)) * 1000;
            let c = rr.bytes(at, 1).ok_or_else(need)?;
            Ok(u64::from(*b.first().unwrap_or(&0)) + u64::from(*c.first().unwrap_or(&0)))
        });
        let (tx, rx) = std::sync::mpsc::channel();
        drive(r, work, Box::new(move |res| tx.send(res.map_err(|e| e.to_string())).unwrap_or_default()));
        let v = rx.recv().unwrap_or(Err("no answer".into()));
        assert_eq!(v, Ok(u64::from(file[200_000]) + u64::from(file[5000])));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn failed_remote_reads_preserve_http_reason_without_known_query_secrets() {
        testing::serve("test:diagnostic-fetcher", Vec::new());
        let address = "https://remote-diagnostic.test/missing.fgb?%2574oken=hidden unescaped hidden";
        let remote = Arc::new(Remote::new(address));
        let read_remote = remote.clone();
        let work: Arc<dyn Fn() -> Result<()> + Send + Sync> = Arc::new(move || {
            read_remote.bytes(0, 8).ok_or_else(need)?;
            Ok(())
        });
        let error = drive_blocking(remote, work).unwrap_err().to_string();
        assert!(error.contains("HTTP 404"), "{error}");
        assert!(!error.contains("hidden"), "{error}");
        assert_eq!(crate::diagnostics::redact(&error), error);
        let history = crate::diagnostics::History::default();
        history.push(crate::diagnostics::Severity::Error, "Data", &error, "");
        let report = history.report(crate::diagnostics::Filter::All);
        assert!(report.contains("HTTP 404") && !report.contains("hidden"), "{report}");
    }

    #[test]
    fn hostile_offsets_never_overflow_or_explode() {
        // Ranges ending at the very end of the address space (a hostile file's offsets).
        let r = Remote::new("u");
        assert!(!r.want(u64::MAX - 10, u64::MAX));
        assert!(r.bytes(u64::MAX - MAX_PIECE / 2, u64::MAX).is_none());
        let w = r.take_missing(0);
        assert!(!w.is_empty() && w.iter().all(|w| matches!(w, Want::Range(a, b) if a < b && b - a <= MAX_PIECE)), "{w:?}");
        // A read planning far more than a read may fetch fails at once, asking for nothing.
        let r = Arc::new(Remote::new("test:never-served"));
        let rr = r.clone();
        let work: Arc<dyn Fn() -> Result<()> + Send + Sync> = Arc::new(move || {
            rr.want(0, 1 << 40);
            Err(need())
        });
        let (tx, rx) = std::sync::mpsc::channel();
        drive(r, work, Box::new(move |res| tx.send(res.map_err(|e| e.to_string())).unwrap_or_default()));
        let e = rx.recv().unwrap_or(Ok(())).err().unwrap_or_default();
        assert!(e.contains("at once"), "{e}");
    }

    #[test]
    fn each_drive_fetches_what_its_own_read_missed() {
        let r = Remote::new("u");
        {
            let _a = Running::enter(1);
            r.bytes(0, 1);
            r.tail(8);
        }
        {
            let _b = Running::enter(2);
            r.bytes(10 * MIN_FETCH, 1);
        }
        // Outside any drive (a check): nobody's, dropped by the next take.
        r.bytes(20 * MIN_FETCH, 1);
        // Drive 2 takes its own miss only; drive 1's stay for it.
        assert_eq!(r.take_missing(2), vec![Want::Range(10 * MIN_FETCH, 11 * MIN_FETCH)]);
        assert_eq!(r.take_missing(1), vec![Want::Range(0, MIN_FETCH), Want::Tail(8)]);
        assert!(r.take_missing(0).is_empty() && !r.is_missing());
        // The drive marker is restored when its read ends.
        assert_eq!(drive_now(), 0);
    }

    #[test]
    fn reads_and_stands_in() {
        use std::io::{Read, Seek, SeekFrom};
        let r = Arc::new(Remote::new("u"));
        r.insert(0, 64, (0..64).collect());
        let mut rd = Reader::with_stand_ins(r.clone(), Arc::new(StandIns { at: [(32u64, Arc::new(vec![9, 9, 9]))].into_iter().collect() }));
        let mut b = [0u8; 4];
        rd.read_exact(&mut b).unwrap_or_default();
        assert_eq!(b, [0, 1, 2, 3]);
        rd.seek(SeekFrom::Start(32)).unwrap_or_default();
        let mut s = [0u8; 3];
        rd.read_exact(&mut s).unwrap_or_default();
        assert_eq!(s, [9, 9, 9]);
        // Past the stand-in, the file again.
        rd.read_exact(&mut b).unwrap_or_default();
        assert_eq!(b, [35, 36, 37, 38]);
        // Not held: missing.
        rd.seek(SeekFrom::Start(1000)).unwrap_or_default();
        assert!(rd.read_exact(&mut b).is_err() && r.is_missing());
    }
}
