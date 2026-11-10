//! Session-local troubleshooting records. Raw credentials and network bodies never belong here.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fmt::Write as _;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::{Arc, Mutex, MutexGuard, Weak};

pub const MAX_RECORDS: usize = 500;
pub const MAX_SOURCE_BYTES: usize = 256;
pub const MAX_MESSAGE_BYTES: usize = 2048;
pub const MAX_DETAILS_BYTES: usize = 8192;
pub const MAX_URL_BYTES: usize = 2048;
/// Servers whose omitted absences are each said once; past this many, later servers add nothing,
/// so a session of many sources can't grow the list without bound.
const MAX_ABSENT_SERVERS: usize = 64;
// Reject, rather than clip, oversized input: a clipped credential key might become harmless-looking.
const MAX_INPUT_BYTES: usize = 16 * 1024;
const REDACTED: &str = "[REDACTED]";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

impl Severity {
    /// The word the copied report shows (the pane shows its own, translated).
    pub fn label(self) -> &'static str {
        match self {
            Self::Error => "Error",
            Self::Warning => "Warning",
            Self::Info => "Info",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Filter {
    #[default]
    All,
    Errors,
    Warnings,
    Info,
    Network,
}

impl Filter {
    pub fn matches(self, record: &Record) -> bool {
        match self {
            Self::All => true,
            Self::Errors => record.severity == Severity::Error,
            Self::Warnings => record.severity == Severity::Warning,
            Self::Info => record.severity == Severity::Info,
            // The note that further servers' absences go unlisted belongs with the absences.
            Self::Network => record.network.is_some() || record.source == "Network",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkKind {
    Http,
    Connection,
    Timeout,
    Other,
}

impl NetworkKind {
    /// The words the copied report shows (the pane shows its own, translated).
    pub fn label(self) -> &'static str {
        match self {
            Self::Http => "HTTP failure",
            Self::Connection => "Connection failure",
            Self::Timeout => "Timeout",
            Self::Other => "Request failure",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Network {
    pub url: String,
    pub method: String,
    pub status: Option<u16>,
    pub duration_ms: Option<u64>,
    pub kind: NetworkKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub id: u64,
    pub timestamp_ms: u64,
    pub severity: Severity,
    pub source: String,
    pub message: String,
    pub details: String,
    pub network: Option<Network>,
}

#[derive(Default)]
struct State {
    next_id: u64,
    generation: u64,
    records: VecDeque<Record>,
    /// Servers whose expected absences have been said (one note each, so a second misconfigured
    /// tile source isn't silent once the first has spoken), each with its note's record id: once
    /// the note leaves the history, the server is said again. Bounded; cleared with the records.
    absences_said: Vec<(String, u64)>,
    /// The note that servers past [`MAX_ABSENT_SERVERS`] go unlisted, while the history holds it.
    absences_overflow_said: Option<u64>,
}

static CLOCK: Mutex<Option<fn() -> u64>> = Mutex::new(None);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Installs the application's portable UTC clock (the engine uses `web_time` on every target).
pub fn set_clock(clock: fn() -> u64) {
    *lock(&CLOCK) = Some(clock);
}

fn timestamp_ms() -> u64 {
    let clock = *lock(&CLOCK);
    if let Some(clock) = clock {
        return clock();
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
    }
    // Applications install a browser-compatible clock before any requests are started.
    #[cfg(target_arch = "wasm32")]
    {
        0
    }
}

thread_local! {
    static CURRENT_CAPTURE: RefCell<Capture> = RefCell::new(Capture::default());
}

/// Request ownership, captured at dispatch without keeping a session or its records alive.
/// An empty capture deliberately omits failures rather than delivering to another session.
#[derive(Clone, Default)]
pub struct Capture(Weak<Mutex<State>>);

/// Restores the previous thread-local owner on return or unwind.
/// Keep this guard on the entering thread and never hold it across an async suspension;
/// carry a cloned [`Capture`] into async work instead.
#[must_use = "the capture scope ends when its guard is dropped"]
pub struct CaptureGuard {
    previous: Capture,
    _thread_bound: PhantomData<Rc<()>>,
}

impl Drop for CaptureGuard {
    fn drop(&mut self) {
        CURRENT_CAPTURE.with(|current| {
            *current.borrow_mut() = std::mem::take(&mut self.previous);
        });
    }
}

impl Capture {
    /// Snapshots the current synchronous scope for a request or queued task.
    pub fn current() -> Self {
        CURRENT_CAPTURE.with(|current| current.borrow().clone())
    }

    /// Enters this owner for synchronous request construction, restoring the previous owner
    /// when the returned guard is dropped. Scopes must be dropped in reverse entry order.
    pub fn enter(&self) -> CaptureGuard {
        let previous = CURRENT_CAPTURE.with(|current| current.replace(self.clone()));
        CaptureGuard { previous, _thread_bound: PhantomData }
    }
}

/// A cloneable handle, separate from project persistence and undo. Dropping its last clone
/// immediately releases the records; the network dispatcher holds only a weak reference.
#[derive(Clone, Default)]
pub struct History(Arc<Mutex<State>>);

impl History {
    pub fn capture(&self) -> Capture {
        Capture(Arc::downgrade(&self.0))
    }

    /// Enters this session's synchronous request scope. Do not hold across an async suspension.
    pub fn enter(&self) -> CaptureGuard {
        self.capture().enter()
    }

    pub fn push(&self, severity: Severity, source: &str, message: &str, details: &str) {
        self.retain(Record {
            id: 0,
            timestamp_ms: timestamp_ms(),
            severity,
            source: bounded(source, MAX_SOURCE_BYTES),
            message: bounded(message, MAX_MESSAGE_BYTES),
            details: bounded(details, MAX_DETAILS_BYTES),
            network: None,
        });
    }

    /// [`History::push`] for a message already through [`redact`] (a notice the session redacted
    /// for its status bar): it is only bounded, so a notice isn't redacted again for each copy.
    /// Only a [`Redacted`], which `redact` alone makes, gets this path.
    pub fn push_redacted(&self, severity: Severity, source: &str, message: &Redacted) {
        self.retain(Record {
            id: 0,
            timestamp_ms: timestamp_ms(),
            severity,
            source: bounded(source, MAX_SOURCE_BYTES),
            message: truncated(message.as_str(), MAX_MESSAGE_BYTES),
            details: String::new(),
            network: None,
        });
    }

    fn retain(&self, record: Record) {
        store(&mut lock(&self.0), record);
    }

    /// Oldest first; both the count and every string in this independent snapshot are bounded.
    pub fn records(&self) -> Vec<Record> {
        lock(&self.0).records.iter().cloned().collect()
    }

    pub fn clear(&self) {
        let mut state = lock(&self.0);
        state.records.clear();
        // A source said before the clear is said again: the pane must not stay silent about it.
        state.absences_said.clear();
        state.absences_overflow_said = None;
        state.generation = state.generation.wrapping_add(1);
    }

    /// Changes whenever records are added or cleared, for inexpensive view snapshot caching.
    pub fn generation(&self) -> u64 {
        lock(&self.0).generation
    }

    /// Plain text suitable for a clipboard or a troubleshooting attachment, oldest entry first.
    /// Timestamps are UTC Unix milliseconds, explicitly labelled so they cannot be mistaken for
    /// local time; a status or duration the request didn't have is left out.
    pub fn report(&self, filter: Filter) -> String {
        let records = self.records();
        let mut report = String::from("RustGIS Diagnostics\nTimestamps: Unix milliseconds (UTC)\n");
        for record in records.iter().filter(|record| filter.matches(record)) {
            let _ = writeln!(
                report,
                "\n#{} {} {} [{}] {}",
                record.id,
                record.timestamp_ms,
                record.severity.label(),
                record.source,
                record.message
            );
            if !record.details.is_empty() {
                let _ = writeln!(report, "{}", record.details);
            }
            if let Some(network) = &record.network {
                let mut facts = vec![format!("Kind: {}", network.kind.label())];
                facts.extend(network.status.map(|status| format!("status: {status}")));
                facts.extend(network.duration_ms.map(|ms| format!("duration: {ms} ms")));
                let _ = writeln!(report, "{} {}\n{}", network.method, network.url, facts.join("; "));
            }
        }
        report
    }
}

impl Capture {
    /// Says once per server, per history, that expected absences (a tile past a map's edge, a
    /// store's absent chunk) are omitted: the pane is not silent about a misconfigured source
    /// while the history stays free of their flood. Later absences from that server add nothing,
    /// until its note leaves the history.
    pub fn note_absence(&self, method: &str, url: &str, status: u16) {
        let Some(state) = self.0.upgrade() else { return };
        // The server is the address's origin (scheme, host and port): two tile sources on one
        // host but different ports are two sources. An address without a web origin (`s3://`)
        // is its scheme and host, so two buckets are two sources. They come first, so only the
        // address's first part is parsed: an oversized address keeps its own server.
        let mut head = url.len().min(MAX_URL_BYTES);
        while !url.is_char_boundary(head) {
            head -= 1;
        }
        let server = url::Url::parse(url.get(..head).unwrap_or(""))
            .map(|u| {
                let origin = u.origin();
                if origin.is_tuple() { origin.ascii_serialization() } else { format!("{}://{}", u.scheme(), u.host_str().unwrap_or("")) }
            })
            .unwrap_or_default();
        // Checked, marked and stored under one lock: a Clear between can't leave the note behind.
        let mut guard = lock(&state);
        if guard.absences_said.iter().any(|(said, _)| *said == server) {
            return;
        }
        if guard.absences_said.len() >= MAX_ABSENT_SERVERS {
            // Past the cap a new server isn't listed, and that is said once.
            if guard.absences_overflow_said.is_some() {
                return;
            }
            let message =
                format!("expected absences from further servers are not listed: this history names the first {MAX_ABSENT_SERVERS} servers");
            let id = store(
                &mut guard,
                Record {
                    id: 0,
                    timestamp_ms: timestamp_ms(),
                    severity: Severity::Warning,
                    source: "Network".to_string(),
                    message,
                    details: String::new(),
                    network: None,
                },
            );
            guard.absences_overflow_said = Some(id);
            return;
        }
        let url = clean_address(url);
        let record = Record {
            id: 0,
            timestamp_ms: timestamp_ms(),
            severity: Severity::Warning,
            source: "Network".to_string(),
            message: bounded(
                &format!("expected absences are omitted from this history: the first was HTTP {status} for {url}"),
                MAX_MESSAGE_BYTES,
            ),
            details: String::new(),
            network: Some(Network { url, method: known_method(method), status: Some(status), duration_ms: None, kind: NetworkKind::Http }),
        };
        let id = store(&mut guard, record);
        guard.absences_said.push((server, id));
    }

    /// Retains one sanitized failure only in the originating live session. Callers supply the
    /// request's own facts (address, method, status, duration) and its failure's message: the
    /// transport's, or the browser's. `details` may hold the few response headers a refusal
    /// keeps. A response body is never passed. Everything is redacted and bounded here.
    pub fn network_failure(&self, source: &str, message: &str, details: &str, network: Network) {
        self.keep_network_failure(source, message, details, network, false);
    }

    /// Keeps repeated policy refusals once while their sanitized facts remain in this bounded
    /// history. Clearing or evicting the record permits it to be reported again.
    pub fn network_failure_once(&self, source: &str, message: &str, details: &str, network: Network) {
        self.keep_network_failure(source, message, details, network, true);
    }

    /// Compares sanitized facts under the retention lock, so Clear cannot lose a refusal.
    fn keep_network_failure(&self, source: &str, message: &str, details: &str, mut network: Network, once: bool) {
        let Some(state) = self.0.upgrade() else { return };
        network.url = clean_address(&network.url);
        if once {
            // Rejected tile paths on one server are one policy refusal, not distinct attempts.
            network.url = url::Url::parse(&network.url)
                .ok()
                .filter(|u| u.origin().is_tuple())
                .map_or(network.url, |u| u.origin().ascii_serialization());
        }
        network.method = known_method(&network.method);
        let record = Record {
            id: 0,
            timestamp_ms: timestamp_ms(),
            severity: Severity::Error,
            source: bounded(source, MAX_SOURCE_BYTES),
            message: bounded(message, MAX_MESSAGE_BYTES),
            details: bounded(details, MAX_DETAILS_BYTES),
            network: Some(network),
        };
        let mut state = lock(&state);
        if once
            && state.records.iter().any(|r| {
                r.source == record.source && r.message == record.message && r.details == record.details && r.network == record.network
            })
        {
            return;
        }
        store(&mut state, record);
    }
}

/// An address as a record keeps it: without its query or fragment, a secret in its path taken
/// out, bounded; one over [`MAX_INPUT_BYTES`] is omitted whole.
fn clean_address(url: &str) -> String {
    if url.len() > MAX_INPUT_BYTES {
        return "[oversized address omitted]".to_string();
    }
    let end = url.find(['?', '#']).unwrap_or(url.len());
    bounded(&path_secrets_redacted(url.get(..end).unwrap_or("")), MAX_URL_BYTES)
}

/// The request's method when it is one of the usual ones, else `OTHER`.
fn known_method(method: &str) -> String {
    match method {
        "GET" | "HEAD" | "POST" | "PUT" | "PATCH" | "DELETE" | "OPTIONS" => method.to_string(),
        _ => "OTHER".to_string(),
    }
}

/// Adds a record to a locked history: numbered, the oldest let go past [`MAX_RECORDS`]. An
/// absence note let go frees its server to be said again. Returns the record's id.
fn store(state: &mut State, mut record: Record) -> u64 {
    state.next_id = state.next_id.saturating_add(1);
    state.generation = state.generation.wrapping_add(1);
    record.id = state.next_id;
    if state.records.len() >= MAX_RECORDS
        && let Some(gone) = state.records.pop_front()
    {
        state.absences_said.retain(|(_, id)| *id != gone.id);
        if state.absences_overflow_said == Some(gone.id) {
            state.absences_overflow_said = None;
        }
    }
    state.records.push_back(record);
    state.next_id
}

/// An address (without its query) with the value of each `name=value` in its path that names a
/// secret replaced (`/files;token=abc/x.png`, `/token=abc/x.png`): the path is kept, as context,
/// so a credential carried in it must not be.
fn path_secrets_redacted(address: &str) -> String {
    let mut out = String::with_capacity(address.len());
    for (s, segment) in address.split('/').enumerate() {
        if s > 0 {
            out.push('/');
        }
        for (p, part) in segment.split(';').enumerate() {
            if p > 0 {
                out.push(';');
            }
            // The separator may be percent-encoded (`token%3Dvalue`).
            let at = part.find('=').map(|at| (at, 1)).into_iter().chain(part.to_ascii_lowercase().find("%3d").map(|at| (at, 3))).min();
            match at.and_then(|(at, len)| Some((part.get(..at)?, part.get(..at + len)?))) {
                Some((name, kept)) if secret_name(name) => {
                    out.push_str(kept);
                    out.push_str(REDACTED);
                }
                _ => out.push_str(part),
            }
        }
    }
    out
}

/// Text that has been through [`redact`]. Only [`Redacted::new`] makes one, so a path that takes
/// a `Redacted` (the history's bound-only [`History::push_redacted`]) can't be handed raw text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Redacted(String);

impl Redacted {
    pub fn new(text: &str) -> Self {
        Self(redact(text))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

/// Redacts, then bounds: a record's copy of a message or of details.
fn bounded(text: &str, limit: usize) -> String {
    let mut clean = redact(text);
    cut_with_ellipsis(&mut clean, limit);
    clean
}

/// `text` cut to `limit` bytes at a character, an ellipsis marking the cut.
pub(crate) fn cut_with_ellipsis(text: &mut String, limit: usize) {
    if text.len() > limit {
        let mut end = limit.saturating_sub(3);
        while !text.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        text.truncate(end);
        text.push('…');
    }
}

fn truncated(text: &str, limit: usize) -> String {
    let mut text = text.to_string();
    cut_with_ellipsis(&mut text, limit);
    text
}

/// A bidirectional embedding, override or isolate control: it reorders the text around it.
fn bidi_control(c: char) -> bool {
    matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// A character kept out of retained text: a control or a bidi control.
fn hostile_control(c: char) -> bool {
    c.is_control() || bidi_control(c)
}

fn separator(c: char) -> bool {
    c.is_whitespace()
        || hostile_control(c)
        || matches!(c, ':' | '=' | ';' | ',' | '?' | '&' | '#' | '@' | '"' | '\'' | '{' | '}' | '[' | ']' | '<' | '>' | '(' | ')' | '`')
}

/// A name's letters and digits in lower case: how credential names are compared, so `Pass-Word`,
/// `pass_word` and `PASSWORD` are one name.
fn normalized_key(key: &str) -> impl Iterator<Item = char> + '_ {
    key.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_lowercase())
}

/// Whether `key` is `target` (lower case, letters and digits) however it is written.
fn key_is(key: &str, target: &str) -> bool {
    crate::address::normalized_name(key).is_some_and(|name| name == target)
}

/// Shared bounded name decoding; diagnostics additionally omit log-only fields.
fn secret_name(key: &str) -> bool {
    crate::address::log_sensitive(key)
}

/// Whether the name is an HTTP credential header: its value is always a scheme and a credential
/// (or, for cookies, a list of them), however many words it holds.
fn credential_header(key: &str) -> bool {
    ["authorization", "proxyauthorization", "wwwauthenticate", "proxyauthenticate", "cookie", "setcookie"]
        .iter()
        .any(|name| key_is(key, name))
}

/// Whether `{`, `[` or `<` opens structured content (a JSON value or a tag) rather than an
/// ordinary bracket in a path, a name or a size ("Layer {2020} failed").
fn opens_structured(rest: &str) -> bool {
    match rest.chars().next() {
        // The trim scans a bounded window: a long run of brackets is one O(window) look per
        // bracket, not a quadratic sweep of the whole text.
        Some('{') | Some('[') => {
            rest.get(..64).unwrap_or(rest).trim_start_matches(['{', '[', ' ', '\t', '\n', '\r']).starts_with(['"', '{', '['])
        }
        Some('<') => {
            // A tag: `<name>` or `</name>`, its name letters without spaces or another bracket.
            let inner = rest.get(1..).unwrap_or("");
            let name = inner.strip_prefix('/').unwrap_or(inner);
            match name.find(['>', '<']) {
                Some(at) => {
                    at > 0
                        && at <= 32
                        && name.get(at..).is_some_and(|tail| tail.starts_with('>'))
                        && name.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_')
                        && !name.get(..at).is_some_and(|tag| tag.contains(|c: char| c.is_whitespace()))
                }
                _ => false,
            }
        }
        _ => false,
    }
}

/// A bare credential line is redacted only for an unambiguous secret name: `key`, `body` and
/// their like read as ordinary words ("Key column 'id' is missing").
fn bare_name(key: &str) -> bool {
    let normalized: String = normalized_key(key).collect();
    !matches!(normalized.as_str(), "key" | "body" | "userid" | "username" | "authentication" | "wwwauthenticate" | "proxyauthenticate")
        && !normalized.contains("body")
        && secret_name(key)
}

/// Whether a word looks like a credential rather than prose: it has a digit, is long, or holds
/// punctuation, a sentence's own ending aside. "Token refreshed." and "Password is missing" are
/// prose; "token abc123" is not.
fn credential_shaped(word: &str) -> bool {
    let word = word.trim_end_matches(['.', ',', '!', '?', ';', ':', ')']);
    word.chars().any(|c| c.is_ascii_digit()) || word.chars().count() >= 16 || word.chars().any(|c| !c.is_alphanumeric())
}

/// Whether a word after a scheme word (`Basic`) looks like its base64 credential: shaped like a
/// credential, or 8 characters or more with a capital past its first letter ("dXNlcjpwYXNz"), so
/// "Basic settings" and "Basic API configuration" keep. A shorter one with neither digit nor
/// padding (`YTpi`, "a:b") reads as a word and keeps too.
fn base64_shaped(word: &str) -> bool {
    let word = word.trim_end_matches(['.', ',', '!', '?', ';', ':', ')']);
    credential_shaped(word) || (word.chars().count() >= 8 && word.chars().skip(1).any(char::is_uppercase))
}

/// A slash (or a backslash, which URL parsers read as one) before `@` can disguise userinfo as
/// an authority and path; a `?` or `#` before an `@` in the authority disguises it as a query
/// or fragment. Takes the whole address, query included.
fn ambiguous_userinfo(address: &str) -> bool {
    let Some((_, rest)) = address.split_once("://") else { return false };
    // `u:123?w@host`: the password ends at the `@`, not at the `?` a parser stops at.
    let raw_authority = rest.split(['/', '\\']).next().unwrap_or("");
    if raw_authority.split_once('@').is_some_and(|(before, _)| before.contains(['?', '#'])) {
        return true;
    }
    let rest = rest.split(['?', '#']).next().unwrap_or("");
    let Some((authority, path)) = rest.split_once(['/', '\\']) else { return false };
    let Some((before_at, _)) = path.split_once('@') else { return false };
    !authority.contains('@') && !authority.starts_with('[') && (authority.contains(':') || before_at.contains(':'))
}

/// Whether a word of text too long to scan names an address whose query or userinfo may hold
/// a credential: a linear look at each `://` word, without the full scanner.
fn may_hold_secret(text: &str) -> bool {
    text.split_whitespace().filter(|word| word.contains("://")).any(|word| {
        ambiguous_userinfo(word)
            || word
                .split_once(['?', '#'])
                .is_some_and(|(_, query)| query.split(['&', ';', '#']).any(|pair| secret_name(pair.split('=').next().unwrap_or(""))))
    })
}

/// Sanitizes a known address on its own, before composing it with diagnostic prose.
/// Unlike a raw URL with a secret query, this result can be redacted again without losing
/// the reason that follows it. This does not establish a boundary for unknown URLs in prose.
pub fn address_in_message(address: &str) -> String {
    if address.len() > MAX_INPUT_BYTES {
        return redact(address);
    }
    let address = match crate::s3_address::parse(address) {
        Ok(Some(bound)) => bound.url,
        Err(_) => return "[address omitted]".to_string(),
        Ok(None) => address,
    };
    let address = match crate::login_address::parse(address) {
        Ok(Some(bound)) => bound.url,
        Err(_) => return "[address omitted]".to_string(),
        Ok(None) => address,
    };
    if address.contains(crate::login_address::PREFIX) || address.contains(crate::s3_address::PREFIX) {
        return "[address omitted]".to_string();
    }
    // A malformed userinfo value can contain a URL-token boundary before its `@`. The generic
    // scanner cannot know those later words belong to the address; here the whole input does.
    // A web address keeps neither query nor fragment. A scheme-less one (often a file path,
    // where `#` is part of a name) is cut at its query only.
    let url = address.contains("://");
    let before_query = if url { address.split(['?', '#']).next() } else { address.split('?').next() }.unwrap_or("");
    if before_query.contains('@')
        && before_query.chars().any(|c| c.is_whitespace() || hostile_control(c) || matches!(c, '"' | '\'' | '<' | '>' | '`'))
    {
        return "[address omitted]".to_string();
    }
    // Do not guess which interpretation of a slash-containing authority held a password.
    if ambiguous_userinfo(address) {
        return "[address omitted]".to_string();
    }
    // Every remaining byte belongs to this known address, even if a value has unescaped
    // whitespace and an unfamiliar signing name. None of that suffix is diagnostic prose.
    let mut shown = redact(before_query);
    if url && shown.starts_with("[address omitted]") {
        // A malformed authority is omitted whole, including words after its first token.
        shown.truncate("[address omitted]".len());
    }
    // A redacted path secret whose value had unescaped whitespace may continue into the words
    // after it: they belong to this known address, not to diagnostic prose.
    if url
        && before_query.contains(char::is_whitespace)
        && let Some(at) = shown.find(REDACTED).map(|at| at + REDACTED.len())
    {
        shown.truncate(at);
    }
    // Keep root authorities separate from the omission marker and the message's `: `:
    // otherwise another redaction can parse the marker or separator as part of the host/port.
    if let Some((scheme, rest)) = shown.split_once("://")
        && ["http", "https", "s3"].iter().any(|s| scheme.eq_ignore_ascii_case(s))
        && !rest.is_empty()
        && !rest.chars().any(|c| c == '/' || c.is_whitespace())
    {
        shown.push('/');
    }
    if let Some(cut) = address.get(before_query.len()..).filter(|cut| !cut.is_empty()) {
        shown.push_str(if cut.starts_with('#') { "[fragment omitted]" } else { "[query omitted]" });
    }
    shown
}

/// Composes a known address and its failure reason without exposing credentials or swallowing
/// the reason after a secret query. Exact address occurrences (including unescaped whitespace)
/// are sanitized first; unrelated URLs and credential fields still use conservative [`redact`].
pub fn message_with_address(address: &str, reason: &str) -> String {
    if address.is_empty() || reason.len() > MAX_INPUT_BYTES {
        return redact(reason);
    }
    let shown = address_in_message(address);
    let reason = address_occurrences_sanitized(address, reason, &shown);
    let named = reason
        .strip_prefix(&shown)
        .is_some_and(|tail| tail.is_empty() || tail.starts_with(':') || tail.chars().next().is_some_and(char::is_whitespace));
    if named { redact(&reason) } else { redact(&format!("{shown}: {reason}")) }
}

/// [`message_with_address`] without naming the address in front: for text that already says
/// where it failed, anywhere in it (a transport's `HTTP 503 from …`).
pub fn with_address_sanitized(address: &str, text: &str) -> String {
    if address.is_empty() || text.len() > MAX_INPUT_BYTES {
        return redact(text);
    }
    redact(&address_occurrences_sanitized(address, text, &address_in_message(address)))
}

/// `reason` with each exact occurrence of `address` (or its runtime binding's inner resource)
/// replaced by `shown`, before the conservative scanner sees its unescaped secret whitespace.
fn address_occurrences_sanitized(address: &str, reason: &str, shown: &str) -> String {
    let mut safe_reason = String::with_capacity(reason.len());
    let mut copied = 0;
    // Lower-level transports may name the inner resource of a runtime binding instead.
    let inner = crate::login_address::inner(crate::s3_address::inner(address));
    let mut outer_matches = reason.match_indices(address).peekable();
    let mut inner_matches = reason.match_indices(inner).filter(|_| inner != address && !inner.is_empty()).peekable();
    loop {
        let occurrence = match (outer_matches.peek(), inner_matches.peek()) {
            (Some(outer), Some(inner)) if outer.0 <= inner.0 => outer_matches.next(),
            (Some(_), Some(_)) | (None, Some(_)) => inner_matches.next(),
            (Some(_), None) => outer_matches.next(),
            (None, None) => break,
        };
        let Some((start, matched)) = occurrence else { break };
        if start < copied {
            continue;
        }
        let end = start + matched.len();
        let before = reason.get(..start).and_then(|s| s.chars().next_back());
        let after = reason.get(end..).unwrap_or("");
        let begins = before.is_none_or(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '(' | '[' | '{' | ':' | '=' | ',' | ';'));
        let ends = after.chars().next().is_none_or(|c| {
            c.is_whitespace()
                || matches!(c, '"' | '\'' | '>' | ')' | ']' | '}')
                || (matches!(c, ':' | ',' | ';' | '.')
                    && after.get(c.len_utf8()..).is_some_and(|s| s.is_empty() || s.starts_with(char::is_whitespace)))
        });
        // A known address must not match the prefix of another address's longer secret.
        if begins && ends {
            safe_reason.push_str(reason.get(copied..start).unwrap_or(""));
            safe_reason.push_str(shown);
            copied = end;
        }
    }
    safe_reason.push_str(reason.get(copied..).unwrap_or(""));
    safe_reason
}

/// Sanitizes before bounding. Oversized input is omitted *whole*, so a secret can never be
/// retained because its identifying key lay beyond a clipped prefix. URL queries/fragments
/// are all omitted, including unknown/encoded signing schemes. JSON/XML bodies are not logged.
/// A credential field discards the remaining text: quoted, multiline and malformed values need
/// not be parsed correctly to remain private. Diagnostics intentionally prefer less context. What
/// counts as a field is narrower in the middle of a line: `name: several words` there reads as
/// prose and is kept (see `docs/privacy.md`).
pub fn redact(text: &str) -> String {
    redact_impl(text, true)
}

/// Sanitizes a replay parameter without treating innocent JSON/XML syntax or public URL
/// queries as diagnostic payloads. Credential fields and secret query continuations still
/// discard their remaining text; the same input and scanning bounds apply.
pub fn redact_parameter(text: &str) -> String {
    redact_impl(text, false)
}

/// The conservative diagnostic redaction when generated text has an unknown secret query or
/// fragment whose value may continue past whitespace. Address-only scrubbing cannot know where
/// that value ends, so callers use this result instead of retaining the later words.
pub fn secret_continuation_cut(text: &str) -> Option<String> {
    // Detected before the output is bounded: a long prefix must not cut the marker away. Text
    // too long to scan goes whole only when one of its addresses may hold a credential, and the
    // marker says how much went and why; other long text keeps its words.
    if text.len() > MAX_INPUT_BYTES {
        return may_hold_secret(text).then(|| format!("[{} bytes of text omitted: too long to check its addresses]", text.len()));
    }
    let mut strict = redact_scan(text, true);
    // Cut what comes before the marker and keep the marker (and anything after it) whole, so
    // the cut text still says what went.
    let at = ["[query and the rest omitted]", "[address and the rest omitted]"].iter().filter_map(|m| strict.rfind(m)).max()?;
    let tail = strict.split_off(at);
    cut_with_ellipsis(&mut strict, MAX_DETAILS_BYTES.saturating_sub(tail.len()));
    strict.push_str(&tail);
    Some(strict)
}

fn redact_impl(text: &str, diagnostic: bool) -> String {
    let mut out = redact_scan(text, diagnostic);
    cut_with_ellipsis(&mut out, MAX_DETAILS_BYTES);
    out
}

/// [`redact_impl`] before its output is bounded.
fn redact_scan(text: &str, diagnostic: bool) -> String {
    if text.len() > MAX_INPUT_BYTES {
        return "[oversized diagnostic omitted]".to_string();
    }
    let mut out = String::with_capacity(text.len().min(MAX_DETAILS_BYTES));
    let mut i = 0;
    let mut quote_scan_budget = MAX_INPUT_BYTES;
    // Whether the atom before this one ended in `:` or `=`: a scheme word is a credential only
    // there ("server: Basic xyz"), so "Basic plan" in prose keeps its words.
    let mut field_before = false;
    'scan: while let Some(rest) = text.get(i..) {
        let Some(c) = rest.chars().next() else { break };
        if c == '['
            && let Some(marker) = [
                REDACTED,
                "[structured content omitted]",
                "[oversized diagnostic omitted]",
                "[address and the rest omitted]",
                "[address omitted]",
                "[oversized address omitted]",
            ]
            .into_iter()
            .find(|marker| rest.starts_with(marker))
        {
            out.push_str(marker);
            i += marker.len();
            continue;
        }
        // A credential tag, attributes or not: its value can span words and lines, so omit the
        // remainder whatever else the tag looks like.
        if c == '<' {
            let inner = rest.get(1..).unwrap_or("").trim_start_matches('/');
            let tag = inner.split(|c: char| c.is_whitespace() || matches!(c, '>' | '/')).next().unwrap_or("");
            if tag.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_')
                && inner.get(..256).unwrap_or(inner).contains('>')
                && secret_name(tag.rsplit(':').next().unwrap_or(tag))
            {
                out.push_str(REDACTED);
                break;
            }
        }
        if matches!(c, '{' | '[' | '<') && opens_structured(rest) && diagnostic {
            out.push_str("[structured content omitted]");
            break;
        }
        if matches!(c, '"' | '\'') {
            let quote = c;
            let mut escaped = false;
            let mut end = None;
            for (offset, next) in rest.get(c.len_utf8()..).unwrap_or("").char_indices() {
                if next.len_utf8() > quote_scan_budget {
                    out.push_str(REDACTED);
                    break 'scan;
                }
                quote_scan_budget -= next.len_utf8();
                if next == quote && !escaped {
                    end = Some(c.len_utf8() + offset);
                    break;
                }
                escaped = next == '\\' && !escaped;
            }
            if let Some(end) = end {
                let key = rest.get(c.len_utf8()..end).unwrap_or("");
                let after = rest.get(end + c.len_utf8()..).unwrap_or("");
                // A quoted secret name before `:` or `=` is a field. Before a space, it is one
                // only when one of the next two words looks like a credential ("'password' is
                // hunter2"), so prose keeps its words ("Column 'key' is missing").
                let field = after.trim_start().starts_with([':', '='])
                    || (after.chars().next().is_some_and(char::is_whitespace) && after.split_whitespace().take(2).any(credential_shaped));
                if field && secret_name(key) {
                    out.push_str(REDACTED);
                    break;
                }
            }
            out.push(c);
            i += c.len_utf8();
            continue;
        }
        if !separator(c) {
            let end = rest.find(separator).unwrap_or(rest.len());
            let atom = rest.get(..end).unwrap_or("");
            let after = rest.get(end..).unwrap_or("");
            if after.starts_with("://") {
                let url_end = rest.find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '`')).unwrap_or(rest.len());
                let address = rest.get(..url_end).unwrap_or("");
                if crate::address::scheme_example(address.trim_end_matches([':', ',', ';', '.', ')', ']'])) {
                    out.push_str(address);
                    i += url_end;
                    field_before = false;
                    continue;
                }
                if ambiguous_userinfo(address) {
                    // Its password may run on past whitespace, as a secret query's may: the rest
                    // goes too, and the marker says so.
                    out.push_str("[address and the rest omitted]");
                    break;
                }
                let query = address.split_once(['?', '#']).map_or("", |(_, q)| q);
                let names = || query.split(['&', ';', '#']).map(|pair| pair.split('=').next().unwrap_or(""));
                let has_name = |name: &str| names().any(|key| key.eq_ignore_ascii_case(name) || key_is(key, name));
                let secret = names().any(secret_name) || (!diagnostic && has_name("code") && has_name("state"));
                match url::Url::parse(address) {
                    // Nothing to take out: the address stays as written, its punctuation too
                    // ("https://example.org: timeout"), unless it holds control characters.
                    Ok(url)
                        if url.has_host()
                            && url.username().is_empty()
                            && url.password().is_none()
                            && (url.query().is_none() || (!diagnostic && !secret))
                            && url.fragment().is_none()
                            && !address.chars().any(hostile_control) =>
                    {
                        out.push_str(&path_secrets_redacted(address));
                    }
                    Ok(mut url) if url.has_host() => {
                        // Sentence punctuation after the address (`…@example.org, retrying`)
                        // isn't the address's: parsed with it, it would join the host (or be an
                        // empty port, dropped). It is set aside and put back after; with a query,
                        // the note below puts it back instead.
                        let mut kept = None;
                        if let Some(c) = address.chars().last().filter(|c| matches!(c, ':' | ',' | ';' | ')' | '.'))
                            && !address.contains(['?', '#'])
                            && let Ok(trimmed) = url::Url::parse(address.get(..address.len() - c.len_utf8()).unwrap_or(""))
                            && trimmed.has_host()
                        {
                            (url, kept) = (trimmed, Some(c));
                        }
                        let _ = url.set_username("");
                        let _ = url.set_password(None);
                        if diagnostic || secret {
                            url.set_query(None);
                        }
                        url.set_fragment(None);
                        out.push_str(&path_secrets_redacted(url.as_str()));
                        out.extend(kept);
                    }
                    _ => out.push_str("[address omitted]"),
                }
                i += url_end;
                field_before = false;
                if address.contains(['?', '#']) && (diagnostic || secret || address.contains('#')) {
                    // Its query or fragment is gone, and that is said. When a pair in it named a
                    // secret, an unescaped value may run on past whitespace: the rest of the text
                    // goes too, said. Otherwise the rest keeps going through the field rules, so a
                    // failure's reason after the address isn't lost.
                    if secret {
                        out.push_str("[query and the rest omitted]");
                        break;
                    }
                    // Said as what went: a query (always in a diagnostic), or only a fragment.
                    let query_removed = diagnostic && address.split('#').next().unwrap_or("").contains('?');
                    out.push_str(if query_removed { "[query omitted]" } else { "[fragment omitted]" });
                    if let Some(end) = address.chars().last().filter(|c| matches!(c, ':' | ',' | ';' | ')' | '.')) {
                        out.push(end);
                    }
                }
                continue;
            }
            let after_space = after.trim_start();
            // A file path (POSIX or Windows) is context, not a credential field name.
            if !atom.contains(['/', '\\']) && secret_name(atom) {
                // At the start of a line, `name: value` is a field whatever its value's shape
                // ("Password: correct horse"). `name=value` is a field however its value is
                // shaped anywhere: an equals sign is not how prose speaks. Mid-line, `name: value`
                // needs its value to look like one (one token, quoted, or the header's own
                // credential); anything longer is prose: "its password: right-click the
                // connection".
                let line = i == 0 || text.get(..i).is_some_and(|before| before.ends_with('\n'));
                let field = if let Some(value) = after_space.strip_prefix('=').map(str::trim_start) {
                    credential_header(atom) || !value.is_empty()
                } else if let Some(value) = after_space.strip_prefix(':').map(str::trim_start) {
                    credential_header(atom)
                        || (line && !value.is_empty())
                        || value.starts_with(['"', '\''])
                        || (!value.is_empty() && !value.contains(char::is_whitespace))
                } else {
                    false
                };
                // A bare credential line is redacted only where the text or line begins, for an
                // unambiguous secret name, and when the word after it looks like a credential, so
                // ordinary notices keep their words ("Token refreshed", "Password is missing").
                // A scheme word (`Basic`) needs a word shaped like base64 after it, which may
                // look like a word but has capitals inside: "Basic settings were saved" keeps.
                let next = after.trim_start().split(char::is_whitespace).next().unwrap_or("");
                let scheme = atom.eq_ignore_ascii_case("basic");
                let shaped = if scheme { base64_shaped(next) } else { credential_shaped(next) };
                let bare = line && after.chars().next().is_some_and(char::is_whitespace) && bare_name(atom) && shaped;
                if field || bare {
                    out.push_str(REDACTED);
                    break;
                }
            }
            // A two-word key ("User ID=alice") is one field name across the space.
            if key_is(atom, "user")
                && after_space.split(separator).next().is_some_and(|next| {
                    next.eq_ignore_ascii_case("id")
                        && after_space.get(next.len()..).is_some_and(|value| value.trim_start().starts_with([':', '=']))
                })
            {
                out.push_str(REDACTED);
                break;
            }
            // A `Bearer <token>` is a credential wherever it sits ("server: Bearer xyz"); a
            // `Basic <token>` too, but only after a field name, so "Basic plan" keeps. The token
            // goes with it, and the rest of the line keeps.
            if (key_is(atom, "bearer") || (key_is(atom, "basic") && field_before)) && after.chars().next().is_some_and(char::is_whitespace)
            {
                out.push_str(REDACTED);
                field_before = false;
                let value = after.trim_start();
                // A quoted token goes whole, spaces and all; an unquoted one to its separator.
                let token = match value.chars().next() {
                    Some(q @ ('"' | '\'')) => value.get(1..).and_then(|v| v.find(q)).map_or(value.len(), |at| at + 2),
                    _ => value.find(separator).unwrap_or(value.len()),
                };
                i += end + (after.len() - value.len()) + token;
                continue;
            }
            // A non-URL user:password@host is ambiguous: omit the whole field rather than leave
            // either half of its credentials behind, its colon percent-encoded too
            // (`alice%3Ahidden@host`, to the depth `secret_name` decodes). A word before `@`
            // alone (`alice@example.org`, `a@2x.png`) is prose.
            let encoded_colon = after.starts_with('@') && atom.contains('%') && {
                let mut lower = atom.to_ascii_lowercase();
                (0..4).any(|_| {
                    let hit = lower.contains("%3a");
                    lower = lower.replace("%25", "%");
                    hit
                })
            };
            if encoded_colon || (after.starts_with(':') && after.split_whitespace().next().is_some_and(|s| s.contains('@'))) {
                out.push_str("[address omitted]");
                break;
            }
            field_before = after_space.starts_with([':', '=']);
            out.push_str(atom);
            i += end;
        } else {
            // Drop control/escape characters (including terminal escapes and bidi controls).
            if c == '\n' || c == '\t' || !hostile_control(c) {
                out.push(c);
            }
            i += c.len_utf8();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eviction_clear_and_clones_are_session_local() {
        let history = History::default();
        let other = History::default();
        for i in 0..MAX_RECORDS + 5 {
            history.push(Severity::Info, "Application", &format!("notice {i}"), "");
        }
        let records = history.records();
        assert_eq!(records.len(), MAX_RECORDS);
        assert_eq!(records.first().unwrap().message, "notice 5");
        assert_eq!(records.last().unwrap().id, 505);
        assert!(other.records().is_empty());
        history.clone().clear();
        assert!(history.records().is_empty());
        history.push(Severity::Error, "Application", "after clear", "");
        assert_eq!(history.records()[0].id, 506);
    }

    #[test]
    fn unicode_fields_and_oversized_keys_are_bounded_before_retention() {
        let history = History::default();
        let text = "界".repeat(4000);
        history.push(Severity::Warning, &text, &text, &text);
        let record = history.records().remove(0);
        assert!(record.source.len() <= MAX_SOURCE_BYTES);
        assert!(record.message.len() <= MAX_MESSAGE_BYTES);
        assert!(record.details.len() <= MAX_DETAILS_BYTES);
        let hostile = format!("{}token=hidden", "x".repeat(MAX_INPUT_BYTES));
        assert_eq!(redact(&hostile), "[oversized diagnostic omitted]");
        assert!(!history.report(Filter::All).contains('�'));
    }

    #[test]
    fn filters_and_reports_select_exact_severities() {
        let history = History::default();
        for severity in [Severity::Info, Severity::Warning, Severity::Error] {
            history.push(severity, "Application", &format!("{severity:?}"), "context");
        }
        let records = history.records();
        for (filter, expected) in [(Filter::All, 3), (Filter::Errors, 1), (Filter::Warnings, 1), (Filter::Info, 1), (Filter::Network, 0)] {
            assert_eq!(records.iter().filter(|r| filter.matches(r)).count(), expected);
        }
        let report = history.report(Filter::Errors);
        assert!(report.contains("Error") && !report.contains("Warning") && !report.contains("Info"));
        assert!(report.contains("UTC"));
    }

    #[test]
    fn adversarial_credentials_are_never_retained() {
        for input in [
            "GET https://alice:hidden@example.org/path?%74oken=hidden#hidden failed",
            "GET https://alice:hidden@example.org/path?X-Amz-Credential=hidden&signature=hidden failed",
            "https://example.org/path?sv=1&sig=hidden&se=hidden#hidden",
            "Authorization: Bearer hidden",
            "proxy-AUTHORIZATION : Basic hidden",
            "Authorization Bearer hidden",
            "token hidden42",
            "password hidden-value",
            "secret hiddenhiddenhidden1",
            "https://example.org/path?token=hidden hidden",
            "Bearer hidden",
            "Basic hidden==",
            "Cookie: session=hidden; other=hidden",
            "Set-Cookie: hidden",
            r#"failed: {"Password":"hidden"}"#,
            r#""\u0070assword": "hidden""#,
            "%2574oken=hidden",
            "api-key = hidden",
            "AccountKey=hidden;Endpoint=x",
            "Server=localhost;User ID=alice;Password='hidden;more'",
            "postgres://alice:hidden@db.example.org/db",
            "alice:hidden@db.example.org",
            "alice%3Ahidden@db.example.org",
            "alice%25253Ahidden@db.example.org",
            "connect alice%253ahidden@db.example.org failed",
            "Set MAPBOX_API_KEY=pk.hidden for the layer",
            "X_API_KEY: hidden",
            "AWS_ACCESS_KEY_ID=hidden",
            "password = 'hidden\nmore hidden'",
            "request_body: hidden",
            "responseBody=hidden",
            "error: <html>hidden</html>",
            "connection_string=hidden",
            "server: Bearer hidden",
            "server: Basic hidden",
            "Server=db;Password=very hidden",
            "password=very hidden indeed",
            "Password: correct horse hidden",
            "Bearer \"hidden more-hidden\"",
        ] {
            let clean = redact(input);
            assert!(!clean.contains("hidden"), "{input} -> {clean}");
        }
        assert_eq!(redact("Loaded 3 layers"), "Loaded 3 layers");
        assert_eq!(redact("failed https://example.org/path?unknown=opaque#fragment"), "failed https://example.org/path[query omitted]");
        // A query without a secret-named pair keeps the failure's reason after it, said.
        assert_eq!(redact("https://host/data?page=2: HTTP 503"), "https://host/data[query omitted]: HTTP 503");
        // One with a secret drops the rest too, and says so.
        assert_eq!(redact("https://host/data?sig=abc: HTTP 503"), "https://host/data[query and the rest omitted]");
        let connection = redact("Server=localhost;User ID=alice;Password='hidden;more'");
        assert!(!connection.contains("alice") && !connection.contains("hidden"), "{connection}");
    }

    #[test]
    fn replay_parameters_keep_public_structure_but_not_credentials() {
        for public in ["<div> https://example.org?format=plain </div>", r#"{"label":"public","link":"https://example.org?format=json"}"#] {
            assert_eq!(redact_parameter(public), public);
            assert_eq!(redact(public), "[structured content omitted]");
        }
        // A replay keeps a public query; only its fragment goes, and the marker says which.
        let link = "see https://example.org/a?format=json#frag then";
        let kept = redact_parameter(link);
        assert_eq!(kept, "see https://example.org/a?format=json[fragment omitted] then");
        assert_eq!(redact_parameter(&kept), kept);
        assert_eq!(redact(link), "see https://example.org/a[query omitted] then");
        assert_eq!(redact_parameter("see https://example.org/a#frag then"), "see https://example.org/a[fragment omitted] then");
        assert_eq!(redact("see https://example.org/a#frag then"), "see https://example.org/a[fragment omitted] then");
        for private in [
            "<Password>hidden hidden-tail</Password> https://example.org",
            "<x:Password>hidden hidden-tail</x:Password> https://example.org",
            r#"{"password":"hidden hidden-tail","link":"https://example.org"}"#,
            "GET https://example.org?%2574oken=hidden hidden-tail",
            "GET https://example.org?code=hidden&state=hidden hidden-tail",
            "GET https://example.org?%2563ode=hidden&%2573tate=hidden hidden-tail",
            "GET https://alice:hidden@example.org?format=json",
            "GET https://u:123/w@host/x",
            "GET https://alice/part:123@host/x",
            "GET https://u:123\\w@host/x",
            r#"<Password type="plain">hidden hidden-tail</Password> https://example.org"#,
        ] {
            let clean = redact_parameter(private);
            assert!(!clean.contains("hidden") && !clean.contains("alice") && !clean.contains("123"), "{private} -> {clean}");
        }
        for private in [r#"<Password type="plain">hidden hidden-tail</Password>"#, "GET https://u:123\\w@host/x"] {
            let clean = redact(private);
            assert!(!clean.contains("hidden") && !clean.contains("123"), "{private} -> {clean}");
        }
        // An ambiguous address in prose says the rest of the text went with it.
        let clean = redact("upload https://u:123/w@h/x failed: HTTP 500");
        assert_eq!(clean, "upload [address and the rest omitted]");
        assert_eq!(redact(&clean), clean);
        // An attribute-only tag that names no credential is still plain markup.
        let public = r#"<span class="note"> https://example.org </span>"#;
        assert_eq!(redact_parameter(public), public);
    }

    #[test]
    fn known_path_secrets_and_long_prefixes_do_not_keep_continuations() {
        let address = "https://example.org/token=hidden hidden-tail/data";
        let clean = message_with_address(address, "HTTP 404");
        assert!(clean.contains("HTTP 404") && !clean.contains("hidden"), "{clean}");
        assert_eq!(redact(&clean), clean);
        let long = format!("{} https://example.org/a?token=hidden hidden-tail", "word ".repeat(2000));
        let cut = secret_continuation_cut(&long).expect("the cut is seen past the output bound");
        assert!(!cut.contains("hidden") && cut.len() <= MAX_DETAILS_BYTES + '…'.len_utf8(), "{}", cut.len());
        // The marker sits past the 8 KiB bound in the scan; the cut keeps it whole at the end.
        assert!(cut.ends_with("…[query and the rest omitted]"), "{}", cut.get(cut.len().saturating_sub(60)..).unwrap_or(""));
        assert_eq!(redact(&cut), cut);
        // An ambiguous address cuts as a secret query does.
        let ambiguous = "run failed at https://u:123/w@h/x hidden-tail: HTTP 500";
        assert_eq!(secret_continuation_cut(ambiguous).as_deref(), Some("run failed at [address and the rest omitted]"));
        // Text too long to scan goes whole, said with its size, only when an address in it may
        // hold a credential; a long text with a public link keeps its words.
        let oversized = format!("{} https://example.org/a?token=hidden hidden-tail", "word ".repeat(4000));
        assert_eq!(
            secret_continuation_cut(&oversized),
            Some(format!("[{} bytes of text omitted: too long to check its addresses]", oversized.len()))
        );
        let ambiguous = format!("{} https://u:123?w@host/x tail", "word ".repeat(4000));
        assert!(secret_continuation_cut(&ambiguous).is_some());
        assert_eq!(secret_continuation_cut(&format!("{} see https://example.org/a?page=2 then", "word ".repeat(4000))), None);
        assert_eq!(secret_continuation_cut(&"word ".repeat(4000)), None);
        assert_eq!(secret_continuation_cut("failed at https://example.org/a: HTTP 500"), None);
    }

    #[test]
    fn known_addresses_preserve_reasons_through_repeated_redaction() {
        for address in [
            "https://example.org/data?token=hidden",
            "https://example.org/data?sig=hidden#hidden",
            "https://example.org/data?%256bey=hidden",
            "https://example.org/data#%2574oken=hidden",
            "https://alice:hidden@example.org/data?key=hidden",
            "https://example.org/data?token=hidden unescaped hidden",
            "https://example.org/data#sig=hidden unescaped hidden",
            "https://example.org/data?custom=hidden unescaped hidden",
            "https://example.org/data#hidden unescaped hidden",
            // A scheme-less address loses its whole query too, unfamiliar keys included.
            "data.tif?custom=hidden unescaped hidden",
            "data.tif?token=hidden unescaped hidden",
        ] {
            let clean = message_with_address(address, "HTTP 503 Service Unavailable");
            assert!(clean.contains("HTTP 503 Service Unavailable"), "{address} -> {clean}");
            assert!(!clean.contains("hidden") && !clean.contains("alice"), "{address} -> {clean}");
            assert_eq!(redact(&clean), clean);
            assert_eq!(redact(&redact(&clean)), clean);
            assert_eq!(message_with_address(address, &format!("{address}: HTTP 503 Service Unavailable")), clean);
            assert_eq!(message_with_address(address, &clean), clean);
        }
        // A scheme-less address says its query went; a `#` in a file name is part of the name.
        assert_eq!(address_in_message("data.tif?custom=hidden unescaped hidden"), "data.tif[query omitted]");
        assert_eq!(message_with_address("/data/plot#1.gpkg", "no such table"), "/data/plot#1.gpkg: no such table");
        // A secret after a scheme-less `#` is still a field, and goes; a `#` before `?` stays in
        // the path while the query is cut.
        let shown = address_in_message("host/data#token=hidden");
        assert!(shown.starts_with("host/data#") && !shown.contains("hidden"), "{shown}");
        assert_eq!(redact(&shown), shown);
        assert_eq!(address_in_message("/data/plot#1.gpkg?token=hidden"), "/data/plot#1.gpkg[query omitted]");
    }

    #[test]
    fn root_addresses_keep_context_through_repeated_redaction() {
        for (address, origin) in [
            ("https://models.test?token=hidden value", "https://models.test/"),
            ("https://models.test:8443", "https://models.test:8443/"),
            ("http://[::1]:8080#token=hidden value", "http://[::1]:8080/"),
            ("HTTPS://models.test?token=hidden", "HTTPS://models.test/"),
            ("s3://bucket?X-Cred=hidden", "s3://bucket/"),
            ("https://", "https://"),
        ] {
            let mut message = message_with_address(address, "HTTP 503");
            for _ in 0..3 {
                assert!(message.contains(origin) && message.contains("HTTP 503") && !message.contains("hidden"), "{address} -> {message}");
                message = redact(&message);
            }
        }
    }

    #[test]
    fn every_known_address_occurrence_is_sanitized_before_the_reason() {
        let address = "https://alice:hidden@example.org/data?%74oken=hidden unescaped hidden";
        let reason = format!("HTTP 404 fetching {address}; retry of {address} failed");
        let clean = message_with_address(address, &reason);
        assert!(clean.contains("HTTP 404 fetching") && clean.contains("; retry of") && clean.ends_with(" failed"), "{clean}");
        assert_eq!(clean.matches("https://example.org/data").count(), 3);
        assert!(!clean.contains("hidden") && !clean.contains("alice"), "{clean}");
        assert_eq!(redact(&clean), clean);
        let history = History::default();
        history.push(Severity::Error, "Network", &clean, &clean);
        assert!(history.report(Filter::All).contains("HTTP 404 fetching"));
        assert!(!history.report(Filter::All).contains("hidden"));
        // Text that already names where it failed keeps its own wording, without a label in front.
        let clean = with_address_sanitized(address, &format!("HTTP 503 from {address}; retrying"));
        assert!(clean.starts_with("HTTP 503 from https://example.org/data") && clean.ends_with("; retrying"), "{clean}");
        assert!(!clean.contains("hidden") && !clean.contains("alice"), "{clean}");
        assert_eq!(clean.matches("://").count(), 1, "{clean}");
    }

    #[test]
    fn known_address_helpers_do_not_relax_unknown_credentials() {
        let raw = "https://example.org/data?token=hidden unescaped hidden: HTTP 503";
        assert_eq!(redact(raw), "https://example.org/data[query and the rest omitted]");
        let clean = message_with_address("https://known.example.org/data?key=hidden", raw);
        assert!(!clean.contains("hidden") && !clean.contains("HTTP 503"), "{clean}");
        let clean = message_with_address("https://known.example.org/data?key=hidden", "HTTP 503; Password=hidden more hidden");
        assert!(clean.contains("HTTP 503") && !clean.contains("hidden"), "{clean}");
        assert_eq!(message_with_address("", raw), redact(raw));
        assert_eq!(message_with_address("/data/a", &"x".repeat(MAX_INPUT_BYTES + 1)), "[oversized diagnostic omitted]");
    }

    #[test]
    fn known_address_prefixes_do_not_expose_extended_credentials() {
        for address in ["https://example.org/data?token=abc", "https://example.org/data#sig=abc"] {
            for suffix in ["XYZhidden", "&other=hidden", "#hidden", "%20hidden", "/hidden", ":hidden"] {
                let reason = format!("{address}{suffix}: HTTP 503");
                let clean = message_with_address(address, &reason);
                assert!(!clean.contains("hidden") && !clean.contains("XYZ"), "{reason} -> {clean}");
                assert!(!clean.contains("HTTP 503"), "{reason} -> {clean}");
            }
        }
    }

    #[test]
    fn known_malformed_userinfo_never_leaks_its_remaining_words() {
        for address in [
            "https://alice:hidden more-hidden@example.org/data",
            "https://alice:hidden\tmore-hidden@example.org/data",
            "https://alice:hidden\"more-hidden@example.org/data",
            "https://alice:hidden<more-hidden@example.org/data",
            "alice:hidden more-hidden@example.org",
            "https://alice:hidden@/data",
            "https://alice:hidden more-hidden",
            "https://u:123/w@host/x",
            "https://u:123/w@host/x?token=hidden",
            "https://alice:hidden/path@host/x",
            "https://alice/part:123@host/x",
            "https://u:123?w@host/x",
            "https://u:123#w@host/x",
            "https://u:123\\w@host/x",
        ] {
            let shown = address_in_message(address);
            let clean = message_with_address(address, &format!("{address}: HTTP 401"));
            assert!(!shown.contains("hidden") && !shown.contains("alice"), "{address} -> {shown}");
            assert!(!clean.contains("hidden") && !clean.contains("alice"), "{address} -> {clean}");
            assert!(!shown.contains("123") && !clean.contains("123"), "{address} -> {clean}");
            assert!(clean.ends_with(": HTTP 401"), "{clean}");
            assert_eq!(redact(&clean), clean);
        }
    }

    #[test]
    fn unambiguous_ports_and_path_at_signs_keep_address_context() {
        for address in
            ["https://host:123/data", "https://host/data@edition", "https://host/data@edition:123", "https://[::1]:8080/data@edition"]
        {
            assert_eq!(address_in_message(address), address);
            assert_eq!(message_with_address(address, "HTTP 503"), format!("{address}: HTTP 503"));
        }
        // An `@` in a query after a path is no userinfo: the host and port stay readable.
        let address = "https://h:80/x?email=a@b";
        assert_eq!(message_with_address(address, "HTTP 503"), "https://h:80/x[query omitted]: HTTP 503");
        // A fragment alone is said as one, and the label survives redacting again.
        let shown = address_in_message("https://h.example/a#frag");
        assert_eq!(shown, "https://h.example/a[fragment omitted]");
        assert_eq!(redact(&shown), shown);
        let clean = message_with_address("https://alice:123%2Fw@host/x", "HTTP 503");
        assert!(clean.contains("HTTP 503") && !clean.contains("alice") && !clean.contains("123"), "{clean}");
    }

    #[test]
    fn credential_words_in_file_paths_preserve_failure_context() {
        let path = "../token-cache/secret.png";
        let clean = redact(&format!("{path}: open failed"));
        assert!(clean.contains(path) && clean.contains("open failed"));
        // Windows paths and ordinary percentages are message context, not secret field names.
        let windows = r"C:\Users\Temp\rustgis-overlay\o.kml: Far (its picture ../secret.png isn't beside it)";
        assert_eq!(redact(windows), windows);
        assert_eq!(redact(r"Loaded 50% of tiles"), "Loaded 50% of tiles");
        assert_eq!(redact(r"Progress 12% complete"), "Progress 12% complete");
    }

    #[test]
    fn known_runtime_bindings_sanitize_outer_and_inner_address_occurrences() {
        let inner = "https://example.org/data?token=hidden";
        let login = crate::login_address::bind("0123456789abcdef", "https://example.org", inner).unwrap();
        let s3 = crate::s3_address::bind("0123456789abcdef", "", inner).unwrap();
        for address in [&login, &s3] {
            let clean = message_with_address(address, &format!("HTTP 503 for {inner}; retrying {address} failed"));
            assert!(clean.contains("HTTP 503") && clean.contains("; retrying") && clean.ends_with(" failed"), "{clean}");
            for private in ["hidden", "0123456789abcdef", crate::login_address::PREFIX, crate::s3_address::PREFIX] {
                assert!(!clean.contains(private), "{private} in {clean}");
            }
            assert_eq!(redact(&clean), clean);
        }
    }

    #[test]
    fn prose_naming_credentials_keeps_its_words() {
        // RustGIS's own notices name credentials without holding one: redacting must not
        // swallow the rest of the message.
        for text in [
            "The saved password is missing or doesn't match this connection.",
            "4 layer address(es) were saved without their keys, tokens, passwords or URL fragments; after RustGIS closes, add those layers again to restore missing values",
            "3 layer(s) read the database of the connection 'db' and need its password: right-click the connection in Data Sources › Connections, or one of the layers, and choose Log In…",
            "Set Color's step keeps its key parameter. If that is an access key, delete the step (Edit Steps) before sharing the task.",
            "Exported /data/a[1].png",
            "Layer {2020} failed",
            "Key column 'id' is missing",
            "Choose the Basic plan to map with.",
            "Token refreshed",
            "Token refreshed.",
            "Exported to alice@example.org: 3 rows",
            "Loaded a@2x.png and parcels@2020",
            "Basic settings were saved",
            "Basic API configuration failed",
            "Basic Settings were saved",
            "Column 'key' is missing",
            "Layer 'Body Mass' failed",
            "The 'password' field is required.",
            "Password is missing for this connection.",
        ] {
            assert_eq!(redact(text), text, "{text}");
        }
        // A credential line is still redacted whole, wherever it begins.
        assert_eq!(redact("failed\ntoken abc123"), "failed\n[REDACTED]");
        assert_eq!(redact("Authorization: Bearer hidden"), "[REDACTED]");
        // A Basic credential is base64: capitals inside a word, or a digit or padding.
        assert_eq!(redact("Basic dXNlcjpwYXNz"), "[REDACTED]");
        assert_eq!(redact("Basic YWxhZGRpbjpvcGVuc2VzYW1l"), "[REDACTED]");
    }

    fn failed(capture: &Capture, message: &str) {
        capture.network_failure(
            "Network",
            message,
            "local classification",
            Network {
                url: "https://diagnostics-history.example.org/request?%74oken=hidden#hidden".to_string(),
                method: "GET".to_string(),
                status: Some(503),
                duration_ms: Some(7),
                kind: NetworkKind::Http,
            },
        );
    }

    #[test]
    fn network_delivery_is_scoped_redacted_and_independently_clearable() {
        let a = History::default();
        let b = History::default();
        failed(&Capture::current(), "no owner");
        {
            let _a = a.enter();
            let request_a = Capture::current();
            failed(&request_a, "a first");
            {
                let _b = b.enter();
                failed(&Capture::current(), "b only");
                // A request dispatched by A still belongs to A while B is current.
                failed(&request_a, "a dispatched");
                {
                    let _empty = Capture::default().enter();
                    failed(&Capture::current(), "explicitly unowned");
                }
                failed(&Capture::current(), "b restored");
            }
            failed(&Capture::current(), "a restored");
        }
        failed(&Capture::current(), "no owner after return");
        let records = a.records();
        assert_eq!(records.iter().map(|r| r.message.as_str()).collect::<Vec<_>>(), ["a first", "a dispatched", "a restored"]);
        assert_eq!(b.records().iter().map(|r| r.message.as_str()).collect::<Vec<_>>(), ["b only", "b restored"]);
        assert!(Filter::Network.matches(&records[0]));
        let network = records[0].network.as_ref().unwrap();
        assert_eq!(network.status, Some(503));
        assert_eq!(network.duration_ms, Some(7));
        assert_eq!(network.url, "https://diagnostics-history.example.org/request");
        assert!(!format!("{records:?}").contains("hidden"));
        a.clear();
        assert!(a.records().is_empty());
        assert_eq!(b.records().len(), 2);
        assert!(Capture::current().0.upgrade().is_none());
    }

    #[test]
    fn network_scope_restores_on_unwind_and_does_not_retain_a_dead_history() {
        let a = History::default();
        let b = History::default();
        {
            let _a = a.enter();
            let result = std::panic::catch_unwind(|| {
                let _b = b.enter();
                failed(&Capture::current(), "b before panic");
                panic!("unwind capture scope");
            });
            assert!(result.is_err());
            failed(&Capture::current(), "a after panic");
        }
        assert_eq!(a.records().len(), 1);
        assert_eq!(a.records()[0].message, "a after panic");
        assert_eq!(b.records().len(), 1);
        assert_eq!(b.records()[0].message, "b before panic");
        assert!(Capture::current().0.upgrade().is_none());

        let dropped = History::default();
        let capture = dropped.capture();
        let _scope = capture.enter();
        let weak = Arc::downgrade(&dropped.0);
        drop(dropped);
        assert!(weak.upgrade().is_none());
        failed(&capture, "dead request");
        failed(&Capture::current(), "dead current owner");
        assert_eq!(a.records().len(), 1);
        assert_eq!(b.records().len(), 1);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn worker_threads_require_explicit_request_ownership() {
        let a = History::default();
        let b = History::default();
        let _a = a.enter();
        let capture = Capture::current();
        std::thread::spawn(move || {
            failed(&Capture::current(), "unowned worker");
            let _scope = capture.enter();
            failed(&Capture::current(), "owned worker");
        })
        .join()
        .unwrap();
        assert_eq!(a.records().len(), 1);
        assert_eq!(a.records()[0].message, "owned worker");
        assert!(b.records().is_empty());
    }
    #[test]
    fn quoted_and_adversarial_credential_fields_are_omitted() {
        // A quoted secret name before a space is a field when a following word looks like a
        // credential; prose after one is covered by `prose_naming_credentials_keeps_its_words`.
        for input in [
            "\"token\" hidden-value".to_string(),
            "\"password\" is hidden42".to_string(),
            "'key': hidden".to_string(),
            format!("{} token=hidden", "\\\"".repeat(4000)),
        ] {
            assert!(!redact(&input).contains("hidden"));
        }
    }

    #[test]
    fn absences_past_the_server_cap_are_said_once() {
        let history = History::default();
        let capture = history.capture();
        for i in 0..MAX_ABSENT_SERVERS + 3 {
            capture.note_absence("GET", &format!("https://tiles{i}.example.org/0/0/0.png"), 404);
        }
        let records = history.records();
        // One note for each server up to the cap, then one saying further servers aren't listed.
        assert_eq!(records.len(), MAX_ABSENT_SERVERS + 1);
        assert!(records.last().is_some_and(|r| r.message.contains("further servers are not listed")));
        assert!(records.last().is_some_and(|r| Filter::Network.matches(r)), "shown with the absences it speaks for");
        // Clear says it again.
        history.clear();
        for i in 0..MAX_ABSENT_SERVERS + 3 {
            capture.note_absence("GET", &format!("https://tiles{i}.example.org/0/0/0.png"), 404);
        }
        assert_eq!(history.records().len(), MAX_ABSENT_SERVERS + 1);
    }

    #[test]
    fn absences_are_said_again_once_their_note_leaves_the_history() {
        let history = History::default();
        let capture = history.capture();
        let notes = |url: &str| history.records().iter().filter(|r| r.network.as_ref().is_some_and(|n| n.url == url)).count();
        let url = "https://tiles.example.org/0/0/0.png";
        capture.note_absence("GET", url, 404);
        capture.note_absence("GET", url, 404);
        assert_eq!(notes(url), 1, "said once while its note is in the history");
        for i in 0..MAX_RECORDS {
            history.push(Severity::Info, "Application", &format!("filler {i}"), "");
        }
        assert_eq!(notes(url), 0);
        capture.note_absence("GET", url, 404);
        assert_eq!(notes(url), 1, "said again once its note was let go");
        // So is the note that further servers go unlisted.
        let unlisted =
            |history: &History| history.records().iter().filter(|r| r.message.contains("further servers are not listed")).count();
        for i in 0..MAX_ABSENT_SERVERS + 1 {
            capture.note_absence("GET", &format!("https://tiles{i}.example.org/0/0/0.png"), 404);
        }
        assert_eq!(unlisted(&history), 1);
        for i in 0..MAX_RECORDS {
            history.push(Severity::Info, "Application", &format!("filler {i}"), "");
        }
        capture.note_absence("GET", "https://another.example.org/0/0/0.png", 404);
        assert_eq!(history.records().iter().filter(|r| r.network.is_some()).count(), 1, "a freed server is listed");
        for i in 0..MAX_ABSENT_SERVERS {
            capture.note_absence("GET", &format!("https://more{i}.example.org/0/0/0.png"), 404);
        }
        assert_eq!(unlisted(&history), 1, "the unlisted note is said again");
    }

    #[test]
    fn absences_without_a_web_origin_are_said_per_bucket() {
        let history = History::default();
        let capture = history.capture();
        capture.note_absence("GET", "s3://first-bucket/tiles/0/0/0.png", 404);
        capture.note_absence("GET", "s3://first-bucket/tiles/1/1/1.png", 404);
        capture.note_absence("GET", "s3://second-bucket/tiles/0/0/0.png", 404);
        assert_eq!(history.records().len(), 2, "one note for each bucket");
    }

    #[test]
    fn secrets_in_a_url_path_are_redacted() {
        let history = History::default();
        let capture = history.capture();
        capture.note_absence("GET", "https://tiles.example.org/token=hidden-path-value/0/0/0.png", 404);
        failed(&capture, "failed");
        let report = history.report(Filter::All);
        assert!(!report.contains("hidden-path-value"), "{report}");
        assert!(report.contains("token=[REDACTED]"), "{report}");
        for text in [
            "Could not reach https://tiles.example.org/files;api_key=hidden-path-value/0.png: timeout",
            "Could not reach https://alice:pw@tiles.example.org/token=hidden-path-value/0.png: timeout",
        ] {
            let clean = redact(text);
            assert!(!clean.contains("hidden-path-value") && clean.contains("[REDACTED]") && clean.ends_with("timeout"), "{clean}");
        }
        // A path without a secret stays as written.
        assert_eq!(redact("Could not reach https://example.org/a=b/c: timeout"), "Could not reach https://example.org/a=b/c: timeout");
        // A percent-encoded separator doesn't hide the value.
        let clean = redact("Could not reach https://example.org/files;token%3Dhidden-path-value/0.png: timeout");
        assert!(!clean.contains("hidden-path-value") && clean.ends_with("timeout"), "{clean}");
        let clean = redact("Could not reach https://example.org/api_key%3dhidden-path-value/0.png: timeout");
        assert!(!clean.contains("hidden-path-value"), "{clean}");
    }

    #[test]
    fn addresses_without_anything_to_take_out_stay_as_written() {
        assert_eq!(redact("Could not reach https://example.org: timeout"), "Could not reach https://example.org: timeout");
        assert_eq!(redact("failed https://example.org/a/b, retrying"), "failed https://example.org/a/b, retrying");
        for text in ["plain http://, confirm before sending", "use https:// for encryption", "an S3:// address"] {
            assert_eq!(redact(text), text);
        }
        for address in ["https://?token=RGSYNTH_example", "s3://#RGSYNTH_example", "https://u:RGSYNTH_example@"] {
            assert!(!redact(address).contains("RGSYNTH_example"), "an incomplete address with content is still withheld");
        }
    }

    /// Taking user information out rewrites the address, and the punctuation that ended it in the
    /// sentence stays with the sentence.
    #[test]
    fn removing_user_information_keeps_the_sentence_punctuation() {
        assert_eq!(redact("Could not reach https://alice:hidden@example.org: timeout"), "Could not reach https://example.org/: timeout");
        assert_eq!(redact("Could not reach https://alice:hidden@example.org, retrying"), "Could not reach https://example.org/, retrying");
        assert_eq!(redact("Could not reach https://alice:hidden@example.org. Retrying"), "Could not reach https://example.org/. Retrying");
        assert_eq!(
            redact("Could not reach https://alice:hidden@example.org/x. Retrying"),
            "Could not reach https://example.org/x. Retrying"
        );
        assert_eq!(redact("(see https://alice:hidden@example.org/x)"), "(see https://example.org/x)");
    }

    /// Redacting what is already redacted changes nothing: a notice is redacted once and bounded
    /// afterwards, and a command's failure is told apart from its notice by the same text.
    #[test]
    fn redaction_is_idempotent() {
        for text in [
            "Authorization: Bearer hidden",
            "failed\ntoken abc123",
            "Could not reach https://alice:hidden@example.org/x?token=a: timeout",
            "x=1 password=hunter2 rest",
            "B.earer hidden then more",
            "Layer {2020} failed \u{1}\u{202e}",
            "Basic dXNlcjpwYXNz",
            "{\"a\": 1}",
        ] {
            let once = redact(text);
            assert_eq!(redact(&once), once, "{text}");
        }
        // A name written around its punctuation is still the name.
        assert_eq!(redact("server: B.earer hidden"), "server: [REDACTED]");
        assert_eq!(redact("server: Ba-sic dXNlcjpwYXNz"), "server: [REDACTED]");
        assert_eq!(redact("pass%77ord=x"), "[REDACTED]");
        for key in ["%2574oken", "%2525252574oken"] {
            assert!(!redact(&format!("{key}=RGSYNTH_diagnostic_20261103")).contains("RGSYNTH_"));
        }
    }

    #[test]
    fn text_already_redacted_is_only_bounded() {
        let history = History::default();
        let long = "word ".repeat(1000);
        history.push_redacted(Severity::Info, "Application", &Redacted::new(&long));
        history.push(Severity::Info, "Application", &long, "");
        let records = history.records();
        assert_eq!(records[0].message, records[1].message);
        assert!(records[0].message.len() <= MAX_MESSAGE_BYTES && records[0].message.ends_with('…'));
        // What the type holds has been through `redact`, whatever went in.
        assert_eq!(Redacted::new("Authorization: Bearer hidden").as_str(), "[REDACTED]");
    }

    #[test]
    fn absence_notes_name_the_method_and_bound_oversized_addresses() {
        let history = History::default();
        let capture = history.capture();
        capture.note_absence("HEAD", "https://tiles.example.org/0/0/0.png", 404);
        capture.note_absence("BREW", "https://other.example.org/0/0/0.png", 404);
        let long = format!("https://big.example.org/{}", "a".repeat(MAX_INPUT_BYTES));
        let other_long = format!("https://huge.example.org/{}", "a".repeat(MAX_INPUT_BYTES));
        capture.note_absence("GET", &long, 404);
        capture.note_absence("GET", &long, 404);
        capture.note_absence("GET", &other_long, 404);
        let records = history.records();
        let method = |i: usize| records[i].network.as_ref().map(|n| n.method.as_str());
        assert_eq!((method(0), method(1)), (Some("HEAD"), Some("OTHER")));
        assert_eq!(records.len(), 4, "each oversized address's server is said once");
        assert_eq!(records[2].network.as_ref().map(|n| n.url.as_str()), Some("[oversized address omitted]"));
    }

    #[test]
    fn reports_print_plain_values_and_leave_out_what_is_absent() {
        let history = History::default();
        let capture = history.capture();
        let network = |status, duration_ms| Network {
            url: "https://example.org/x".into(),
            method: "GET".into(),
            status,
            duration_ms,
            kind: NetworkKind::Http,
        };
        capture.network_failure("Network", "http status: 503", "", network(Some(503), Some(7)));
        capture.network_failure("Network", "the browser couldn't reach it", "", network(None, None));
        let report = history.report(Filter::All);
        assert!(report.contains("Kind: HTTP failure; status: 503; duration: 7 ms"), "{report}");
        assert!(report.contains("Kind: HTTP failure\n"), "{report}");
        assert!(!report.contains("Some(") && !report.contains("None"), "{report}");
    }
}
