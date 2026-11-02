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
    /// tile source isn't silent once the first has spoken). Bounded; cleared with the records.
    absences_said: Vec<String>,
    /// Whether servers past [`MAX_ABSENT_SERVERS`] have been said to go unlisted.
    absences_overflow_said: bool,
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
        state.absences_overflow_said = false;
        state.generation = state.generation.wrapping_add(1);
    }

    /// Changes whenever records are added or cleared, for inexpensive view snapshot caching.
    pub fn generation(&self) -> u64 {
        lock(&self.0).generation
    }

    /// Plain text suitable for a clipboard or a troubleshooting attachment. Timestamps are UTC
    /// Unix milliseconds, explicitly labelled so they cannot be mistaken for local time.
    pub fn report(&self, filter: Filter) -> String {
        let records = self.records();
        let mut report = String::from("RustGIS Diagnostics\nTimestamps: Unix milliseconds (UTC)\n");
        for record in records.iter().filter(|record| filter.matches(record)) {
            let _ =
                writeln!(report, "\n#{} {} {:?} [{}] {}", record.id, record.timestamp_ms, record.severity, record.source, record.message);
            if !record.details.is_empty() {
                let _ = writeln!(report, "{}", record.details);
            }
            if let Some(network) = &record.network {
                let _ = writeln!(
                    report,
                    "{} {}\nKind: {:?}; status: {:?}; duration_ms: {:?}",
                    network.method, network.url, network.kind, network.status, network.duration_ms
                );
            }
        }
        report
    }
}

impl Capture {
    /// Says once per server, per history, that expected absences (a tile past a map's edge, a
    /// store's absent chunk) are omitted: the pane is not silent about a misconfigured source
    /// while the history stays free of their flood. Later absences from that server add nothing.
    pub fn absence_said(&self, url: &str, status: u16) {
        let Some(state) = self.0.upgrade() else { return };
        // The server is the address's origin (scheme, host and port): two tile sources on one
        // host but different ports are two sources. An address without a web origin (`s3://`)
        // is its scheme and host, so two buckets are two sources.
        let server = url::Url::parse(url)
            .map(|u| {
                let origin = u.origin();
                if origin.is_tuple() { origin.ascii_serialization() } else { format!("{}://{}", u.scheme(), u.host_str().unwrap_or("")) }
            })
            .unwrap_or_default();
        // Checked, marked and stored under one lock: a Clear between can't leave the note behind.
        let mut guard = lock(&state);
        if guard.absences_said.contains(&server) {
            return;
        }
        if guard.absences_said.len() >= MAX_ABSENT_SERVERS {
            // Past the cap a new server isn't listed, and that is said once.
            if std::mem::replace(&mut guard.absences_overflow_said, true) {
                return;
            }
            let message =
                format!("expected absences from further servers are not listed: this history names the first {MAX_ABSENT_SERVERS} servers");
            store(
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
            return;
        }
        guard.absences_said.push(server);
        // The address without its query or fragment, or a secret in its path, as a failure's
        // record keeps it.
        let url = path_secrets_redacted(url.get(..url.find(['?', '#']).unwrap_or(url.len())).unwrap_or(""));
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
            network: Some(Network {
                url: bounded(&url, MAX_URL_BYTES),
                method: "GET".to_string(),
                status: Some(status),
                duration_ms: None,
                kind: NetworkKind::Http,
            }),
        };
        store(&mut guard, record);
    }

    /// Retains one sanitized failure only in the originating live session. Callers supply local
    /// classifications/metadata only, never bodies, arbitrary server text, or header dumps.
    pub fn network_failure(&self, source: &str, message: &str, details: &str, mut network: Network) {
        let Some(state) = self.0.upgrade() else { return };
        network.url = if network.url.len() > MAX_INPUT_BYTES {
            "[oversized address omitted]".to_string()
        } else {
            let end = network.url.find(['?', '#']).unwrap_or(network.url.len());
            bounded(&path_secrets_redacted(network.url.get(..end).unwrap_or("")), MAX_URL_BYTES)
        };
        network.method = match network.method.as_str() {
            "GET" | "HEAD" | "POST" | "PUT" | "PATCH" | "DELETE" | "OPTIONS" => network.method,
            _ => "OTHER".to_string(),
        };
        History(state).retain(Record {
            id: 0,
            timestamp_ms: timestamp_ms(),
            severity: Severity::Error,
            source: bounded(source, MAX_SOURCE_BYTES),
            message: bounded(message, MAX_MESSAGE_BYTES),
            details: bounded(details, MAX_DETAILS_BYTES),
            network: Some(network),
        });
    }
}

/// Adds a record to a locked history: numbered, the oldest let go past [`MAX_RECORDS`].
fn store(state: &mut State, mut record: Record) {
    state.next_id = state.next_id.saturating_add(1);
    state.generation = state.generation.wrapping_add(1);
    record.id = state.next_id;
    if state.records.len() >= MAX_RECORDS {
        state.records.pop_front();
    }
    state.records.push_back(record);
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

/// Redacts, then bounds: a record's copy of a message or of details.
fn bounded(text: &str, limit: usize) -> String {
    let mut clean = redact(text);
    if clean.len() > limit {
        let mut end = limit.saturating_sub(3);
        while !clean.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        clean.truncate(end);
        clean.push('…');
    }
    clean
}

fn separator(c: char) -> bool {
    c.is_whitespace()
        || c.is_control()
        || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | ':' | '=' | ';' | ',' | '?' | '&' | '#' | '@' | '"' | '\'' | '{' | '}' | '[' | ']' | '<' | '>' | '(' | ')' | '`')
}

fn secret_name(key: &str) -> bool {
    // Decode only names, never values: decoding a URL value could introduce whitespace and
    // incorrectly end its redacted query. Work and allocation are capped by MAX_INPUT_BYTES.
    let mut name = key.to_string();
    for _ in 0..4 {
        let bytes = name.as_bytes();
        let mut decoded = String::with_capacity(name.len());
        let mut i = 0;
        while let Some(&b) = bytes.get(i) {
            if b == b'%'
                && let Some(hex) = name.get(i + 1..i + 3)
                && let Ok(value) = u8::from_str_radix(hex, 16)
            {
                decoded.push(char::from(value));
                i += 3;
            } else if b == b'\\'
                && bytes.get(i + 1) == Some(&b'u')
                && let Some(hex) = name.get(i + 2..i + 6)
                && let Ok(value) = u32::from_str_radix(hex, 16)
                && let Some(c) = char::from_u32(value)
            {
                decoded.push(c);
                i += 6;
            } else if let Some(c) = name.get(i..).and_then(|s| s.chars().next()) {
                decoded.push(c);
                i += c.len_utf8();
            } else {
                break;
            }
        }
        if decoded == name {
            break;
        }
        name = decoded;
    }
    let normalized: String = name.chars().filter(|c| c.is_ascii_alphanumeric()).flat_map(char::to_lowercase).collect();
    crate::source::credentials::secret_key(&name)
        || crate::source::credentials::secret_key(&normalized)
        || normalized.contains("credential")
        || normalized.contains("connectionstring")
        || normalized.contains("privatekey")
        || normalized.contains("accountkey")
        // A prefixed name is still one: MAPBOX_API_KEY, X_API_KEY, AWS_ACCESS_KEY_ID.
        || normalized.contains("apikey")
        || normalized.contains("accesskey")
        || normalized.contains("sharedaccess")
        || normalized.contains("body")
        || matches!(
            normalized.as_str(),
            "basic" | "bearer" | "authentication" | "wwwauthenticate" | "proxyauthenticate" | "userid" | "username"
        )
}

/// Whether the name is an HTTP credential header: its value is always a scheme and a credential
/// (or, for cookies, a list of them), however many words it holds.
fn credential_header(key: &str) -> bool {
    let normalized: String = key.chars().filter(|c| c.is_ascii_alphanumeric()).flat_map(char::to_lowercase).collect();
    matches!(normalized.as_str(), "authorization" | "proxyauthorization" | "wwwauthenticate" | "proxyauthenticate" | "cookie" | "setcookie")
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
    let normalized: String = key.chars().filter(|c| c.is_ascii_alphanumeric()).flat_map(char::to_lowercase).collect();
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
/// credential, or a capital past its first letter ("dXNlcjpwYXNz"), so "Basic settings" keeps.
fn base64_shaped(word: &str) -> bool {
    credential_shaped(word) || word.chars().skip(1).any(char::is_uppercase)
}

/// Sanitizes before bounding. Oversized input is omitted *whole*, so a secret can never be
/// retained because its identifying key lay beyond a clipped prefix. URL queries/fragments
/// are all omitted, including unknown/encoded signing schemes. JSON/XML bodies are not logged.
/// A credential field discards the remaining text: quoted, multiline and malformed values need
/// not be parsed correctly to remain private. Diagnostics intentionally prefer less context.
pub fn redact(text: &str) -> String {
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
        if matches!(c, '{' | '[' | '<') && opens_structured(rest) {
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
                match url::Url::parse(address) {
                    // Nothing to take out: the address stays as written, its punctuation too
                    // ("https://example.org: timeout"), unless it holds control characters.
                    Ok(url)
                        if url.has_host()
                            && url.username().is_empty()
                            && url.password().is_none()
                            && url.query().is_none()
                            && url.fragment().is_none()
                            && !address
                                .chars()
                                .any(|c| c.is_control() || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')) =>
                    {
                        out.push_str(&path_secrets_redacted(address));
                    }
                    Ok(mut url) if url.has_host() => {
                        let _ = url.set_username("");
                        let _ = url.set_password(None);
                        url.set_query(None);
                        url.set_fragment(None);
                        out.push_str(&path_secrets_redacted(url.as_str()));
                    }
                    _ => out.push_str("[address omitted]"),
                }
                i += url_end;
                field_before = false;
                if address.contains(['?', '#']) {
                    // Its query or fragment is gone, and that is said. When a pair in it named a
                    // secret, an unescaped value may run on past whitespace: the rest of the text
                    // goes too, said. Otherwise the rest keeps going through the field rules, so a
                    // failure's reason after the address isn't lost.
                    let query = address.split_once(['?', '#']).map_or("", |(_, q)| q);
                    let secret = query.split(['&', ';', '#']).any(|pair| secret_name(pair.split('=').next().unwrap_or("")));
                    if secret {
                        out.push_str("[query and the rest omitted]");
                        break;
                    }
                    out.push_str("[query omitted]");
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
            let name: String = atom.chars().filter(|c| c.is_ascii_alphanumeric()).flat_map(char::to_lowercase).collect();
            if name == "user"
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
            if (name == "bearer" || (name == "basic" && field_before)) && after.chars().next().is_some_and(char::is_whitespace) {
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
            if c == '\n' || c == '\t' || !c.is_control() && !matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') {
                out.push(c);
            }
            i += c.len_utf8();
        }
    }
    if out.len() > MAX_DETAILS_BYTES {
        let mut end = MAX_DETAILS_BYTES.saturating_sub(3);
        while !out.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        out.truncate(end);
        out.push('…');
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
    fn prose_naming_credentials_keeps_its_words() {
        // RustGIS's own notices name credentials without holding one: redacting must not
        // swallow the rest of the message.
        for text in [
            "The saved password is missing or doesn't match this connection.",
            "4 layer address(es) were saved without their keys, tokens or passwords: after RustGIS closes, add those layers again to give them",
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
            capture.absence_said(&format!("https://tiles{i}.example.org/0/0/0.png"), 404);
        }
        let records = history.records();
        // One note for each server up to the cap, then one saying further servers aren't listed.
        assert_eq!(records.len(), MAX_ABSENT_SERVERS + 1);
        assert!(records.last().is_some_and(|r| r.message.contains("further servers are not listed")));
        assert!(records.last().is_some_and(|r| Filter::Network.matches(r)), "shown with the absences it speaks for");
        // Clear says it again.
        history.clear();
        for i in 0..MAX_ABSENT_SERVERS + 3 {
            capture.absence_said(&format!("https://tiles{i}.example.org/0/0/0.png"), 404);
        }
        assert_eq!(history.records().len(), MAX_ABSENT_SERVERS + 1);
    }

    #[test]
    fn absences_without_a_web_origin_are_said_per_bucket() {
        let history = History::default();
        let capture = history.capture();
        capture.absence_said("s3://first-bucket/tiles/0/0/0.png", 404);
        capture.absence_said("s3://first-bucket/tiles/1/1/1.png", 404);
        capture.absence_said("s3://second-bucket/tiles/0/0/0.png", 404);
        assert_eq!(history.records().len(), 2, "one note for each bucket");
    }

    #[test]
    fn secrets_in_a_url_path_are_redacted() {
        let history = History::default();
        let capture = history.capture();
        capture.absence_said("https://tiles.example.org/token=hidden-path-value/0/0/0.png", 404);
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
        // User info still goes, with the address rewritten without it.
        assert_eq!(redact("Could not reach https://alice:hidden@example.org: timeout"), "Could not reach https://example.org/ timeout");
    }
}
