//! Why a fetch failed (#509), kept for the user: the status a server answered with, the few
//! headers that say who answered (a firewall in front of it, a sign-in it wants), and a short,
//! plain-text excerpt of its answer.
//!
//! Everything here came from the server or the network, so it is untrusted text: bounded,
//! stripped of markup and control characters, and only ever shown as text.

/// Headers kept from a failed answer: who answered, what it wants, and the request's id.
pub const KEPT_HEADERS: [&str; 13] = [
    "server",
    "content-type",
    "www-authenticate",
    "retry-after",
    "cf-ray",
    "cf-mitigated",
    "x-iinfo",
    "x-sucuri-id",
    "x-sucuri-block",
    "x-amzn-waf-action",
    "x-amzn-requestid",
    "x-amz-cf-id",
    "x-request-id",
];
/// The longest header value or excerpt kept, in characters.
const MAX_TEXT: usize = 200;
/// The most of a failed answer's body read for its excerpt.
pub const MAX_BODY: u64 = 16 << 10;

/// What kind of failure it was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// The server answered with this HTTP status (4xx or 5xx).
    Status(u16),
    /// The server couldn't be reached: its name, a connection or a secure connection failed.
    Unreachable,
    /// The server didn't answer in time.
    Timeout,
    /// Anything else (an address refused before fetching, a redirect refused, an answer too large).
    Other,
}

/// A failed fetch, with what is known about it.
#[derive(Clone, Debug, PartialEq)]
pub struct Failure {
    /// The address asked for (as given, so it may hold a key: redact it before showing it).
    pub url: String,
    /// The error as text, as fetches have always given it (`http status: 403`, …).
    pub message: String,
    pub reason: Reason,
    /// What the server said, when it answered and it is known (not from the browser). Boxed: most
    /// failures have none, and the failure travels in a `Result`.
    pub answer: Option<Box<Answer>>,
}

/// What a server said when it refused a request.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Answer {
    /// The kept headers, lowercase names in [`KEPT_HEADERS`] order.
    pub headers: Vec<(String, String)>,
    /// The answer's page title, or the start of its text.
    pub excerpt: Option<String>,
    /// The `User-Agent` the request went with (this app's).
    pub user_agent: Option<String>,
}

impl Failure {
    /// A failure known only by its text: its status is read back from it (`http status: 403`,
    /// the browser's `HTTP 403`).
    pub fn new(url: &str, message: impl Into<String>) -> Self {
        let message = message.into();
        let message = if message.contains(rustgis_data::login_address::PREFIX) { crate::logins::scrub(&message) } else { message };
        let reason = crate::cache::http_status(&message).filter(|s| *s >= 400).map_or(Reason::Other, Reason::Status);
        let url = if url.contains(rustgis_data::login_address::PREFIX) { crate::logins::scrub(url) } else { url.to_string() };
        Failure { url, message, reason, answer: None }
    }

    /// A failure that never reached an answer.
    pub fn without_answer(url: &str, message: impl Into<String>, reason: Reason) -> Self {
        Failure { reason, ..Failure::new(url, message) }
    }

    /// A server's answer with an error status: its kept headers and the excerpt of its `body`.
    pub fn answered(url: &str, status: u16, headers: &[(String, String)], body: &[u8]) -> Self {
        let mut kept: Vec<(String, String)> = Vec::new();
        for name in KEPT_HEADERS {
            if let Some((_, v)) = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)) {
                kept.push((name.to_string(), plain(v)));
            }
        }
        let html = kept.iter().any(|(k, v)| k == "content-type" && v.to_ascii_lowercase().contains("html"));
        Failure {
            url: crate::logins::inner(url).to_string(),
            // As ureq has always said it, so text that reads the status back still finds it.
            message: format!("http status: {status}"),
            reason: Reason::Status(status),
            answer: Some(Box::new(Answer { headers: kept, excerpt: excerpt(body, html), user_agent: None })),
        }
    }

    /// The failure, its request having gone as `user_agent`.
    pub fn sent_as(mut self, user_agent: &str) -> Self {
        if let Some(a) = self.answer.as_mut() {
            a.user_agent = Some(user_agent.to_string());
        }
        self
    }

    /// The HTTP status, if the server answered with one.
    pub fn status(&self) -> Option<u16> {
        match self.reason {
            Reason::Status(s) => Some(s),
            _ => None,
        }
    }

    /// A kept header's value (`name` in lowercase).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.answer.as_ref()?.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    /// The answer's page title, or the start of its text.
    pub fn excerpt(&self) -> Option<&str> {
        self.answer.as_ref()?.excerpt.as_deref()
    }

    /// The `User-Agent` the request went with, when it was this app's (not the browser's).
    pub fn user_agent(&self) -> Option<&str> {
        self.answer.as_ref()?.user_agent.as_deref()
    }

    /// The firewall in front of the server, when its headers say so beyond doubt. It adds them to
    /// every answer, the server's own refusals included: whether it refused the request itself is
    /// [`Failure::firewall_blocked`].
    pub fn firewall(&self) -> Option<&'static str> {
        let server = self.header("server").unwrap_or("").to_ascii_lowercase();
        if server.contains("cloudflare") || self.header("cf-ray").is_some() {
            Some("Cloudflare")
        } else if server.contains("akamaighost") {
            Some("Akamai")
        } else if self.header("x-iinfo").is_some() {
            Some("Imperva")
        } else if self.header("x-sucuri-id").is_some() || server.contains("sucuri") {
            Some("Sucuri")
        } else if self.header("x-amzn-waf-action").is_some() {
            Some("AWS WAF")
        } else {
            None
        }
    }

    /// Whether the firewall in front blocked or challenged the request itself, rather than passing
    /// on the service's own refusal (a private service wanting a token, say): told by a header it
    /// sends only then, or by its own page.
    pub fn firewall_blocked(&self) -> bool {
        let page = self.excerpt().unwrap_or("").to_ascii_lowercase();
        let says = |words: &[&str]| words.iter().any(|w| page.contains(w));
        match self.firewall() {
            Some("Cloudflare") => {
                self.header("cf-mitigated").is_some() || says(&["attention required", "just a moment", "you have been blocked"])
            }
            Some("Akamai") => says(&["access denied"]),
            Some("Imperva") => says(&["incapsula incident", "request unsuccessful"]),
            Some("Sucuri") => self.header("x-sucuri-block").is_some() || says(&["sucuri website firewall"]),
            // Sent only when one of its rules acted on the request.
            Some("AWS WAF") => true,
            _ => false,
        }
    }

    /// The id the server gave the request, to quote to its administrator.
    pub fn request_id(&self) -> Option<String> {
        ["cf-ray", "x-iinfo", "x-sucuri-id", "x-amzn-requestid", "x-amz-cf-id", "x-request-id"]
            .iter()
            .find_map(|k| self.header(k).map(|v| format!("{k} {v}")))
    }
}

/// A failure known only by its text, whose address the fetch fills in.
impl From<String> for Failure {
    fn from(message: String) -> Self {
        Failure::new("", message)
    }
}

impl From<&str> for Failure {
    fn from(message: &str) -> Self {
        Failure::new("", message)
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// A status's usual name (`403` → `Forbidden`), for the common ones.
pub fn status_name(status: u16) -> Option<&'static str> {
    Some(match status {
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        410 => "Gone",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => return None,
    })
}

/// Untrusted text as one plain line: control characters out, spaces collapsed, at most
/// [`MAX_TEXT`] characters.
fn plain(s: &str) -> String {
    let one: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let words: Vec<&str> = one.split_whitespace().collect();
    let line = words.join(" ");
    if line.chars().count() > MAX_TEXT { line.chars().take(MAX_TEXT - 1).chain(['…']).collect() } else { line }
}

/// What a failed answer says, as one plain line: a page's title, else its text without markup.
fn excerpt(body: &[u8], html: bool) -> Option<String> {
    let body = body.get(..usize::try_from(MAX_BODY).unwrap_or(usize::MAX)).unwrap_or(body);
    let text = String::from_utf8_lossy(body);
    let html = html || text.trim_start().starts_with('<');
    let line = if html {
        match between(&text, "<title", "</title>") {
            Some(title) => plain(&entities(title.split_once('>').map_or("", |(_, t)| t))),
            None => plain(&entities(&without_tags(&text))),
        }
    } else {
        plain(&text)
    };
    (!line.is_empty()).then_some(line)
}

/// The text from `open` to `close`, case aside (the first of each).
fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let lower = text.to_ascii_lowercase();
    let a = lower.find(open)?;
    let b = a + lower.get(a..)?.find(close)?;
    text.get(a + open.len()..b)
}

/// Markup removed, with the text of scripts and styles.
fn without_tags(html: &str) -> String {
    // Lowercased once: ASCII lowercasing leaves every byte where it was, so offsets match `html`.
    let lower = html.to_ascii_lowercase();
    let mut out = String::new();
    let mut at = 0;
    while let Some(i) = lower.get(at..).and_then(|r| r.find('<')).map(|i| at + i) {
        out.push_str(html.get(at..i).unwrap_or(""));
        out.push(' ');
        // A script's or style's contents aren't text to read.
        let close = if opens(&lower, i, "script") {
            "</script>"
        } else if opens(&lower, i, "style") {
            "</style>"
        } else {
            ">"
        };
        match lower.get(i..).and_then(|r| r.find(close)) {
            Some(j) => at = i + j + close.len(),
            // Never closed: the rest is the tag's.
            None => return out,
        }
    }
    out.push_str(html.get(at..).unwrap_or(""));
    out
}

/// Whether the tag at `i` of lowercased `html` is `<name`, the name ending there: `<style>` or
/// `<style media=…>`, not `<styles>`.
fn opens(html: &str, i: usize, name: &str) -> bool {
    let end = i + 1 + name.len();
    html.get(i + 1..end) == Some(name)
        && html.get(end..).and_then(|r| r.chars().next()).is_none_or(|c| c == '>' || c == '/' || c.is_whitespace())
}

/// The few character references a page title uses.
fn entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&nbsp;", " ")
        .replace("&#8212;", "—")
        .replace("&mdash;", "—")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inactive_runtime_bindings_are_removed_from_failure_fields() {
        let bound = rustgis_data::login_address::bind("0123456789abcdef", "https://failure.test", "https://failure.test/tile").unwrap();
        let failure = Failure::new(&bound, format!("failed to fetch {bound}"));
        assert!(!failure.url.contains("rustgis-login:") && !failure.url.contains("0123456789abcdef"));
        assert!(!failure.message.contains("rustgis-login:") && !failure.message.contains("0123456789abcdef"));
    }

    fn h(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn a_cloudflare_block_is_named_with_its_page_title() {
        let body = b"<!DOCTYPE html><html><head><title>Attention Required! | Cloudflare</title><style>x{}</style></head><body>Sorry, you have been blocked</body></html>";
        let f = Failure::answered(
            "https://example.org/MapServer?f=json",
            403,
            &h(&[
                ("Server", "cloudflare"),
                ("CF-RAY", "a4c929d3fa78a259-BOG"),
                ("Content-Type", "text/html; charset=UTF-8"),
                ("Set-Cookie", "x"),
            ]),
            body,
        );
        assert_eq!((f.status(), f.firewall(), f.message.as_str()), (Some(403), Some("Cloudflare"), "http status: 403"));
        assert_eq!(f.excerpt(), Some("Attention Required! | Cloudflare"));
        assert_eq!(f.request_id().as_deref(), Some("cf-ray a4c929d3fa78a259-BOG"));
        assert!(f.header("set-cookie").is_none(), "only the kept headers");
        assert_eq!(crate::cache::http_status(&f.message), Some(403), "the status reads back as before");
    }

    #[test]
    fn firewalls_are_named_only_when_sure() {
        let named = |pairs: &[(&str, &str)]| Failure::answered("u", 403, &h(pairs), b"").firewall();
        assert_eq!(named(&[("server", "AkamaiGHost")]), Some("Akamai"));
        assert_eq!(named(&[("x-iinfo", "1-2-3")]), Some("Imperva"));
        assert_eq!(named(&[("x-sucuri-id", "1")]), Some("Sucuri"));
        assert_eq!(named(&[("x-amzn-waf-action", "block")]), Some("AWS WAF"));
        assert_eq!(named(&[("server", "nginx")]), None);
        assert_eq!(named(&[]), None);
    }

    /// A firewall in front passes the service's own refusals on with its headers: only its own
    /// block or challenge says it refused the request.
    #[test]
    fn a_firewall_in_front_is_not_taken_for_one_that_blocked() {
        let blocked = |pairs: &[(&str, &str)], body: &[u8]| {
            let f = Failure::answered("u", 403, &h(pairs), body);
            (f.firewall(), f.firewall_blocked())
        };
        let cloudflare = [("server", "cloudflare"), ("cf-ray", "1-BOG")];
        // The service's own 403 (a token it wants), passed on through Cloudflare.
        assert_eq!(blocked(&cloudflare, br#"{"error":{"code":403,"message":"Token Required"}}"#), (Some("Cloudflare"), false));
        // Cloudflare's own: a challenge header, its challenge page or its block page.
        assert_eq!(blocked(&[("cf-ray", "1"), ("cf-mitigated", "challenge")], b""), (Some("Cloudflare"), true));
        assert_eq!(blocked(&cloudflare, b"<title>Just a moment...</title>"), (Some("Cloudflare"), true));
        assert_eq!(blocked(&cloudflare, b"<title>Attention Required! | Cloudflare</title>"), (Some("Cloudflare"), true));
        assert_eq!(blocked(&[("server", "AkamaiGHost")], b"<title>Access Denied</title>"), (Some("Akamai"), true));
        assert_eq!(blocked(&[("server", "AkamaiGHost")], b"Not here"), (Some("Akamai"), false));
        assert_eq!(blocked(&[("x-sucuri-id", "1"), ("x-sucuri-block", "1")], b""), (Some("Sucuri"), true));
        assert_eq!(blocked(&[("x-amzn-waf-action", "captcha")], b""), (Some("AWS WAF"), true));
        assert_eq!(blocked(&[("server", "nginx")], b"<title>Attention Required</title>"), (None, false));
    }

    #[test]
    fn answers_are_plain_bounded_text() {
        // A page without a title: its text, without scripts or tags.
        let f = Failure::answered("u", 500, &[], b"<html><script>alert(1)</script><p>Internal   error\n</p></html>");
        assert_eq!(f.excerpt(), Some("Internal error"));
        // Plain text, cut to its first 200 characters; control characters become spaces.
        let f = Failure::answered("u", 503, &h(&[("server", "a\u{7}b")]), "x".repeat(5000).as_bytes());
        assert_eq!(f.excerpt().map(|e| e.chars().count()), Some(200));
        assert_eq!(f.header("server"), Some("a b"));
        // Nothing to say: no excerpt. Not UTF-8: read as far as it is.
        assert_eq!(Failure::answered("u", 404, &[], b"  ").excerpt(), None);
        assert_eq!(Failure::answered("u", 404, &[], b"ok \xff").excerpt(), Some("ok \u{fffd}"));
        // Only `<script>` and `<style>` hide their text, whatever their case, not tags whose names
        // start the same.
        assert_eq!(Failure::answered("u", 404, &[], b"<STYLE media=x>a{}</Style><styles>Kept</styles>").excerpt(), Some("Kept"));
        assert_eq!(Failure::answered("u", 404, &[], b"<scripting>Too</scripting><script src=x></script>").excerpt(), Some("Too"));
        // A title or tag never closed doesn't hang or panic.
        assert_eq!(Failure::answered("u", 404, &[], b"<html><title>Half").excerpt(), Some("Half"));
        assert_eq!(Failure::answered("u", 404, &[], b"<html><title").excerpt(), None);
    }

    #[test]
    fn a_failure_known_by_its_text_reads_its_status() {
        assert_eq!(Failure::new("u", "http status: 401").reason, Reason::Status(401));
        assert_eq!(Failure::new("u", "HTTP 429").reason, Reason::Status(429));
        assert_eq!(Failure::new("u", "too many redirects").reason, Reason::Other);
        assert_eq!(Failure::without_answer("u", "timeout: global", Reason::Timeout).reason, Reason::Timeout);
        assert_eq!((status_name(403), status_name(299)), (Some("Forbidden"), None));
    }
}
